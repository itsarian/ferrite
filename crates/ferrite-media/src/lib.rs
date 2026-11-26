use anyhow::{Context as _, Result, bail};
use boa_engine::context::ContextBuilder;
use boa_engine::job::{Job, JobExecutor, SimpleJobExecutor, TimeoutJob};
use boa_engine::vm::RuntimeLimits;
use boa_engine::{
    Context, JsArgs, JsNativeError, JsObject, JsResult, JsValue, NativeFunction, Source, js_string,
};
use ferrite_core::{FerriteConfig, ProxySettings, SslException};
use ferrite_http::{build_http_client, build_http_client_for_url};
use reqwest::Client;
use reqwest::header::{
    ACCEPT, ACCEPT_LANGUAGE, COOKIE, HeaderMap, HeaderName, HeaderValue, LOCATION,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as TlsError, SignatureScheme};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsPluginInfo {
    pub uid: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub enabled: bool,
    pub auto_update: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_url: Option<String>,
    pub domains: Vec<String>,
    pub entry_script: PathBuf,
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginUpdateOutcome {
    pub uid: String,
    pub name: String,
    pub old_version: String,
    pub new_version: String,
}

pub fn compare_semver(a: &str, b: &str) -> std::cmp::Ordering {
    fn parse_parts(v: &str) -> (Vec<u64>, Option<String>) {
        let trimmed = v.trim().trim_start_matches(['v', 'V']);
        let (core, pre) = match trimmed.split_once('-') {
            Some((c, p)) => (c, Some(p.to_ascii_lowercase())),
            None => (trimmed, None),
        };
        let nums = core
            .split('.')
            .map(|seg| seg.trim().parse::<u64>().unwrap_or(0))
            .collect();
        (nums, pre)
    }
    let (pa, pre_a) = parse_parts(a);
    let (pb, pre_b) = parse_parts(b);
    let len = pa.len().max(pb.len());
    for i in 0..len {
        let va = pa.get(i).copied().unwrap_or(0);
        let vb = pb.get(i).copied().unwrap_or(0);
        match va.cmp(&vb) {
            std::cmp::Ordering::Equal => continue,
            non_eq => return non_eq,
        }
    }
    match (pre_a, pre_b) {
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(x), Some(y)) => x.cmp(&y),
        (None, None) => std::cmp::Ordering::Equal,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaVariantOption {
    pub format_id: String,
    pub title: String,
    pub ext: String,
    pub url: String,
    pub audio_url: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bitrate_kbps: Option<u64>,
    pub codecs: Option<String>,
    pub is_audio_only: bool,
    pub filesize: Option<u64>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub http_headers: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleOption {
    pub lang: String,
    pub url: String,
    pub ext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistEntry {
    pub url: String,
    pub title: String,
    pub duration_secs: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaParseResult {
    pub source_url: String,
    pub title: String,
    pub thumbnail_url: Option<String>,
    pub duration_secs: Option<f64>,
    pub uploader: Option<String>,
    pub is_playlist: bool,
    pub variants: Vec<MediaVariantOption>,
    pub subtitles: Vec<SubtitleOption>,
    pub playlist_entries: Vec<PlaylistEntry>,
    pub extractor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaProbeInfo {
    pub duration_secs: Option<f64>,
    pub format_name: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub bit_rate: Option<u64>,
}

const MAX_HTTP_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PLUGIN_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_PLUGIN_ARCHIVE_BYTES: usize = 32 * 1024 * 1024;
const MAX_HTTP_REDIRECTS: usize = 10;
const HTTP_FETCH_TIMEOUT: Duration = Duration::from_secs(20);
const JS_EXECUTION_TIMEOUT: Duration = Duration::from_secs(30);
const JS_LOOP_ITERATION_LIMIT: u64 = 1_000_000;
const JS_RECURSION_LIMIT: usize = 128;
const JS_STACK_SIZE_LIMIT: usize = 4096;
const MAX_JS_TIMEOUT_MS: u64 = 10_000;
const MAX_ACTIVE_JS_TIMERS: usize = 1024;

#[derive(Debug, Clone)]
struct PluginHttpSettings {
    user_agent: String,
    proxy: ProxySettings,
    ssl_exceptions: Vec<SslException>,
    allow_loopback: bool,
}

thread_local! {
    static TL_HTTP_CLIENT: RefCell<Option<Client>> = const { RefCell::new(None) };
    static TL_HTTP_SETTINGS: RefCell<Option<PluginHttpSettings>> = const { RefCell::new(None) };
    static TL_TIMERS: RefCell<(u32, std::collections::HashSet<u32>, usize)> =
        RefCell::new((1, std::collections::HashSet::new(), 0));
    static TL_ALLOW_PYTHON: RefCell<bool> = const { RefCell::new(false) };
    static TL_ALLOW_COOKIES: RefCell<bool> = const { RefCell::new(false) };
    static TL_LOGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    #[cfg(test)]
    static TL_ALLOW_LOOPBACK_TEST: RefCell<bool> = const { RefCell::new(false) };
}

fn current_allow_loopback_for_test() -> bool {
    #[cfg(test)]
    {
        TL_ALLOW_LOOPBACK_TEST.with(|b| *b.borrow())
    }
    #[cfg(not(test))]
    {
        false
    }
}

struct JsRuntimeThreadGuard;

impl Drop for JsRuntimeThreadGuard {
    fn drop(&mut self) {
        TL_HTTP_CLIENT.with(|c| *c.borrow_mut() = None);
        TL_HTTP_SETTINGS.with(|s| *s.borrow_mut() = None);
        TL_ALLOW_PYTHON.with(|b| *b.borrow_mut() = false);
        TL_ALLOW_COOKIES.with(|b| *b.borrow_mut() = false);
        TL_LOGS.with(|l| l.borrow_mut().clear());
        TL_TIMERS.with(|t| *t.borrow_mut() = (1, std::collections::HashSet::new(), 0));
    }
}

fn ssl_exception_matches(exc: &SslException, sni: &str, target_port: u16) -> bool {
    let raw = exc.host_port.trim();
    let (exc_host, exc_port) = if let Some(rest) = raw.strip_prefix('[') {
        if let Some((h, p)) = rest.split_once("]:") {
            (h, p.parse::<u16>().ok())
        } else {
            (rest.trim_end_matches(']'), None)
        }
    } else if let Some((h, p)) = raw.rsplit_once(':') {
        if !h.contains(':')
            && let Ok(port_num) = p.parse::<u16>()
        {
            (h, Some(port_num))
        } else {
            (raw, None)
        }
    } else {
        (raw, None)
    };
    exc_port.unwrap_or(443) == target_port && exc_host.eq_ignore_ascii_case(sni)
}

#[derive(Debug)]
struct PluginHostPinVerifier {
    pinned_sha256_by_host: Vec<SslException>,
    target_port: u16,
    inner: Arc<dyn ServerCertVerifier>,
}

impl ServerCertVerifier for PluginHostPinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, TlsError> {
        match self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Ok(v) => Ok(v),
            Err(orig_err) => {
                let mut h = Sha256::new();
                h.update(end_entity.as_ref());
                let fp = hex::encode(h.finalize());
                let sni = match server_name {
                    ServerName::DnsName(d) => d.as_ref().to_ascii_lowercase(),
                    ServerName::IpAddress(ip) => std::net::IpAddr::from(*ip).to_string(),
                    _ => String::new(),
                };
                for exc in &self.pinned_sha256_by_host {
                    if ssl_exception_matches(exc, &sni, self.target_port)
                        && exc.sha256_fingerprint.eq_ignore_ascii_case(&fp)
                    {
                        return Ok(ServerCertVerified::assertion());
                    }
                }
                Err(orig_err)
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

fn build_plugin_rustls_config(
    ssl_exceptions: &[SslException],
    target_port: u16,
) -> Option<rustls::ClientConfig> {
    ferrite_http::ensure_crypto_provider();
    let mut root_store = rustls::RootCertStore::empty();
    let res = rustls_native_certs::load_native_certs();
    for cert in res.certs {
        let _ = root_store.add(cert);
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let base_verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
        Arc::new(root_store),
        provider.clone(),
    )
    .build()
    .ok()?;

    let mut cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .ok()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PluginHostPinVerifier {
            pinned_sha256_by_host: ssl_exceptions.to_vec(),
            target_port,
            inner: base_verifier,
        }))
        .with_no_client_auth();
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Some(cfg)
}

/// Returns `true` when `ip` belongs to a loopback, private, link-local,
/// unspecified, multicast, CGNAT, or reserved address block that untrusted
/// plugin scripts must not reach.
fn is_disallowed_ip(ip: IpAddr, allow_loopback: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() {
                return !allow_loopback;
            }
            if v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
            {
                return true;
            }
            let oct = v4.octets();
            // 0.0.0.0/8 ("this network"), 100.64.0.0/10 (CGNAT), 240.0.0.0/4 (reserved)
            oct[0] == 0 || (oct[0] == 100 && (oct[1] & 0b1100_0000) == 64) || oct[0] >= 240
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                return !allow_loopback;
            }
            if v6.is_unspecified() || v6.is_multicast() {
                return true;
            }
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_disallowed_ip(IpAddr::V4(mapped), allow_loopback);
            }
            if let Some(compat) = v6.to_ipv4() {
                return is_disallowed_ip(IpAddr::V4(compat), allow_loopback);
            }
            let seg0 = v6.segments()[0];
            // fc00::/7 (unique local) and fe80::/10 (link-local)
            (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80
        }
    }
}

struct SsrfSafeResolver {
    allow_loopback: bool,
}

impl reqwest::dns::Resolve for SsrfSafeResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let allow_loopback = self.allow_loopback;
        let host = name.as_str().to_string();
        Box::pin(async move {
            let lower = host.trim_end_matches('.').to_ascii_lowercase();
            if !allow_loopback && (lower == "localhost" || lower.ends_with(".localhost")) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("Blocked SSRF host: {host}"),
                )
                .into());
            }
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            if addrs.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("No DNS records for {host}"),
                )
                .into());
            }
            for addr in &addrs {
                if is_disallowed_ip(addr.ip(), allow_loopback) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("Blocked SSRF IP {} for {host}", addr.ip()),
                    )
                    .into());
                }
            }
            let iter: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

async fn validate_fetch_url(parsed: &url::Url, allow_loopback: bool) -> Result<()> {
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("Blocked unsupported URL scheme '{}'", parsed.scheme());
    }
    let port = parsed.port_or_known_default().unwrap_or(80);
    match parsed.host() {
        Some(url::Host::Ipv4(v4)) => {
            if is_disallowed_ip(IpAddr::V4(v4), allow_loopback) {
                bail!("Blocked SSRF target IP {v4}");
            }
        }
        Some(url::Host::Ipv6(v6)) => {
            if is_disallowed_ip(IpAddr::V6(v6), allow_loopback) {
                bail!("Blocked SSRF target IP {v6}");
            }
        }
        Some(url::Host::Domain(domain)) => {
            let lower = domain.trim_end_matches('.').to_ascii_lowercase();
            if lower.is_empty() {
                bail!("URL missing host");
            }
            if !allow_loopback && (lower == "localhost" || lower.ends_with(".localhost")) {
                bail!("Blocked SSRF target host {domain}");
            }
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((domain, port))
                .await
                .with_context(|| format!("Failed DNS resolution for {domain}"))?
                .collect();
            if addrs.is_empty() {
                bail!("DNS resolution returned no addresses for {domain}");
            }
            for addr in addrs {
                if is_disallowed_ip(addr.ip(), allow_loopback) {
                    bail!("Blocked SSRF target IP {} for {domain}", addr.ip());
                }
            }
        }
        None => bail!("URL missing host"),
    }
    Ok(())
}

fn build_plugin_http_client(settings: &PluginHttpSettings, target_port: u16) -> Result<Client> {
    ferrite_http::ensure_crypto_provider();
    let mut default_headers = HeaderMap::new();
    default_headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
    default_headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));

    let mut builder = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .referer(false)
        .default_headers(default_headers)
        .connect_timeout(HTTP_FETCH_TIMEOUT)
        .dns_resolver(Arc::new(SsrfSafeResolver {
            allow_loopback: settings.allow_loopback,
        }))
        .user_agent(&settings.user_agent);

    if !settings.ssl_exceptions.is_empty()
        && let Some(tls_cfg) = build_plugin_rustls_config(&settings.ssl_exceptions, target_port)
    {
        builder = builder.use_preconfigured_tls(tls_cfg);
    }

    let proxy = &settings.proxy;
    match proxy.mode {
        ferrite_core::NetworkProxyMode::NoProxy => {
            builder = builder.no_proxy();
        }
        ferrite_core::NetworkProxyMode::SystemProxy => {}
        ferrite_core::NetworkProxyMode::ManualProxy => {
            builder = builder.no_proxy();
            if !proxy.http.host.is_empty() && proxy.http.port > 0 {
                let mut p = reqwest::Proxy::http(format!(
                    "http://{}:{}",
                    proxy.http.host, proxy.http.port
                ))?;
                if !proxy.http.username.is_empty() {
                    p = p.basic_auth(&proxy.http.username, &proxy.http.password);
                }
                builder = builder.proxy(p);
            }
            if !proxy.https.host.is_empty() && proxy.https.port > 0 {
                let mut p = reqwest::Proxy::https(format!(
                    "http://{}:{}",
                    proxy.https.host, proxy.https.port
                ))?;
                if !proxy.https.username.is_empty() {
                    p = p.basic_auth(&proxy.https.username, &proxy.https.password);
                }
                builder = builder.proxy(p);
            }
            if !proxy.socks5.host.is_empty() && proxy.socks5.port > 0 {
                let mut p = reqwest::Proxy::all(format!(
                    "socks5h://{}:{}",
                    proxy.socks5.host, proxy.socks5.port
                ))?;
                if !proxy.socks5.username.is_empty() {
                    p = p.basic_auth(&proxy.socks5.username, &proxy.socks5.password);
                }
                builder = builder.proxy(p);
            }
        }
    }

    Ok(builder.build()?)
}

fn urls_share_origin(a: &url::Url, b: &url::Url) -> bool {
    a.scheme().eq_ignore_ascii_case(b.scheme())
        && a.host_str()
            .zip(b.host_str())
            .is_some_and(|(ha, hb)| ha.eq_ignore_ascii_case(hb))
        && a.port_or_known_default() == b.port_or_known_default()
}

async fn read_response_bytes_limited(
    mut resp: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    if let Some(len) = resp.content_length()
        && len > max_bytes as u64
    {
        bail!("Response size ({len} bytes) exceeds limit ({max_bytes} bytes)");
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if out.len().saturating_add(chunk.len()) > max_bytes {
            bail!("Response body exceeds limit ({max_bytes} bytes)");
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

const QT_JS_MS_PARSER_LIB_SHIM: &str = r#"
var console = {
  log: function() { __ferrite_log(Array.prototype.slice.call(arguments).map(String).join(' ')); },
  warn: function() { __ferrite_log('[warn] ' + Array.prototype.slice.call(arguments).map(String).join(' ')); },
  error: function() { __ferrite_log('[error] ' + Array.prototype.slice.call(arguments).map(String).join(' ')); }
};

function setTimeout(cb, ms) {
  return __ferrite_set_timeout(cb, Number(ms) || 0);
}
function clearTimeout(id) {
  __ferrite_clear_timeout(Number(id) || 0);
}

var qtJsDownloadsFactory = {
  create: function() {
    var req = {
      url: '',
      cookies: '',
      postData: null,
      postDataAsUtf8String: null,
      _headers: [],
      _callbacks: [],
      _body: '',
      error: null,
      serializedError: null,
      isNetworkError: false,
      isInternalError: false,
      setCustomHeader: function(k, v) { this._headers.push({ key: String(k), value: String(v) }); },
      dataAsUtf8Text: function() { return this._body; },
      finished: {
        connect: function(cb) { req._callbacks.push(cb); }
      },
      start: function() {
        var self = this;
        var post = self.postDataAsUtf8String !== null ? String(self.postDataAsUtf8String) : '';
        __ferrite_http_fetch(
          String(self.url),
          String(self.cookies || ''),
          JSON.stringify(self._headers),
          post
        ).then(function(res) {
          if (res.ok) {
            self._body = res.body;
            self.cookies = res.cookies || self.cookies;
          } else {
            self.error = res.error || 'HTTP error';
            self.serializedError = self.error;
            self.isNetworkError = !!res.isNetworkError;
          }
          for (var i = 0; i < self._callbacks.length; i++) {
            self._callbacks[i]();
          }
        }, function(err) {
          self.error = String(err);
          self.serializedError = self.error;
          self.isNetworkError = true;
          for (var i = 0; i < self._callbacks.length; i++) {
            self._callbacks[i]();
          }
        });
      }
    };
    return req;
  }
};

function downloadUrlAsUtf8Text(url, cookies, headers, postData) {
  cookies = cookies || '';
  headers = headers || [];
  return new Promise(function(resolve, reject) {
    var download = qtJsDownloadsFactory.create();
    download.url = url;
    download.cookies = cookies;
    if (typeof postData === 'string') {
      download.postDataAsUtf8String = postData;
    }
    for (var i = 0; i < headers.length; i++) {
      download.setCustomHeader(headers[i].key, headers[i].value);
    }
    download.finished.connect(function() {
      var err = download.serializedError || download.error;
      if (err) {
        reject({ error: err, isParseError: !download.isNetworkError && !download.isInternalError });
      } else {
        resolve({ url: url, body: download.dataAsUtf8Text(), cookies: download.cookies });
      }
    });
    download.start();
  });
}

function launchPythonScript(requestId, interactive, script, args) {
  return __ferrite_python_exec(String(script || ''), JSON.stringify(args || [])).then(function(res) {
    if (res.error) {
      return Promise.reject(res);
    }
    return res;
  });
}
"#;

async fn execute_ssrf_safe_http_fetch(
    url_str: &str,
    cookies: &str,
    headers_json: &str,
    post_data: &str,
    allow_cookies: bool,
) -> (bool, String, String, String, bool) {
    let settings = TL_HTTP_SETTINGS.with(|s| s.borrow().clone());
    let allow_loopback = settings
        .as_ref()
        .map(|s| s.allow_loopback)
        .unwrap_or_else(current_allow_loopback_for_test);
    let default_client = TL_HTTP_CLIENT.with(|cell| cell.borrow().clone());

    let mut current_url = match url::Url::parse(url_str) {
        Ok(u) => u,
        Err(e) => {
            return (
                false,
                String::new(),
                String::new(),
                format!("Invalid URL: {e}"),
                true,
            );
        }
    };

    let custom_headers: Vec<(String, String)> =
        serde_json::from_str::<Vec<serde_json::Value>>(headers_json)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|item| {
                let k = item.get("key")?.as_str()?.trim().to_string();
                let v = item.get("value")?.as_str()?.to_string();
                if k.is_empty()
                    || k.eq_ignore_ascii_case("host")
                    || k.eq_ignore_ascii_case("content-length")
                    || k.eq_ignore_ascii_case("transfer-encoding")
                {
                    return None;
                }
                Some((k, v))
            })
            .collect();

    let mut active_cookies = if allow_cookies {
        cookies.to_string()
    } else {
        String::new()
    };
    let mut use_post = !post_data.is_empty();
    let mut body_payload = if use_post {
        Some(post_data.to_string())
    } else {
        None
    };
    let mut cross_origin = false;

    for hop in 0..=MAX_HTTP_REDIRECTS {
        if let Err(e) = validate_fetch_url(&current_url, allow_loopback).await {
            return (false, String::new(), active_cookies, e.to_string(), true);
        }

        let target_port = current_url.port_or_known_default().unwrap_or(443);
        let client = if let Some(ref st) = settings
            && current_url.scheme() == "https"
            && !st.ssl_exceptions.is_empty()
            && target_port != 443
        {
            match build_plugin_http_client(st, target_port) {
                Ok(c) => c,
                Err(e) => {
                    return (false, String::new(), active_cookies, e.to_string(), true);
                }
            }
        } else if let Some(ref c) = default_client {
            c.clone()
        } else {
            let fallback_settings = PluginHttpSettings {
                user_agent: concat!("Ferrite/", env!("CARGO_PKG_VERSION")).into(),
                proxy: ProxySettings::default(),
                ssl_exceptions: Vec::new(),
                allow_loopback,
            };
            match build_plugin_http_client(&fallback_settings, target_port) {
                Ok(c) => c,
                Err(e) => {
                    return (false, String::new(), active_cookies, e.to_string(), true);
                }
            }
        };

        let mut req = if use_post {
            let mut b = client.post(current_url.as_str());
            if let Some(ref data) = body_payload {
                b = b.body(data.clone());
            }
            b
        } else {
            client.get(current_url.as_str())
        };
        req = req.timeout(HTTP_FETCH_TIMEOUT);

        if allow_cookies && !cross_origin && !active_cookies.is_empty() {
            req = req.header(COOKIE, active_cookies.clone());
        }
        for (k, v) in &custom_headers {
            if (!allow_cookies || cross_origin) && k.eq_ignore_ascii_case("cookie") {
                continue;
            }
            if cross_origin
                && (k.eq_ignore_ascii_case("authorization")
                    || k.eq_ignore_ascii_case("proxy-authorization"))
            {
                continue;
            }
            if let (Ok(name), Ok(val)) = (
                HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(v),
            ) {
                req = req.header(name, val);
            }
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => return (false, String::new(), active_cookies, e.to_string(), true),
        };

        let status = resp.status();
        if status.is_redirection() {
            if hop == MAX_HTTP_REDIRECTS {
                return (
                    false,
                    String::new(),
                    active_cookies,
                    "Too many HTTP redirects".into(),
                    true,
                );
            }
            let Some(loc) = resp.headers().get(LOCATION).and_then(|v| v.to_str().ok()) else {
                return (
                    false,
                    String::new(),
                    active_cookies,
                    format!("HTTP redirect {} missing Location header", status),
                    false,
                );
            };
            let next_url = match current_url.join(loc.trim()) {
                Ok(u) => u,
                Err(e) => {
                    return (
                        false,
                        String::new(),
                        active_cookies,
                        format!("Invalid redirect Location: {e}"),
                        true,
                    );
                }
            };
            if !urls_share_origin(&current_url, &next_url) {
                cross_origin = true;
                active_cookies.clear();
            }
            if matches!(status.as_u16(), 301..=303) {
                use_post = false;
                body_payload = None;
            }
            current_url = next_url;
            continue;
        }

        let final_cookies = if allow_cookies {
            let set_cookie = resp
                .headers()
                .get_all(reqwest::header::SET_COOKIE)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .map(|s| s.split(';').next().unwrap_or(s))
                .collect::<Vec<_>>()
                .join("; ");
            if set_cookie.is_empty() {
                active_cookies
            } else {
                set_cookie
            }
        } else {
            String::new()
        };

        return match read_response_bytes_limited(resp, MAX_HTTP_RESPONSE_BYTES).await {
            Ok(bytes) => {
                let txt = String::from_utf8_lossy(&bytes).into_owned();
                if status.is_success() {
                    (true, txt, final_cookies, String::new(), false)
                } else {
                    (
                        false,
                        txt,
                        final_cookies,
                        format!("HTTP status {}", status),
                        false,
                    )
                }
            }
            Err(e) => (false, String::new(), final_cookies, e.to_string(), true),
        };
    }

    (
        false,
        String::new(),
        active_cookies,
        "Too many HTTP redirects".into(),
        true,
    )
}

async fn host_http_fetch(
    _this: &JsValue,
    args: &[JsValue],
    ctx: &RefCell<&mut Context>,
) -> JsResult<JsValue> {
    let (url, cookies, headers_json, post_data) = {
        let mut c = ctx.borrow_mut();
        let url = args
            .get_or_undefined(0)
            .to_string(&mut c)?
            .to_std_string_escaped();
        let cookies = args
            .get_or_undefined(1)
            .to_string(&mut c)?
            .to_std_string_escaped();
        let headers_json = args
            .get_or_undefined(2)
            .to_string(&mut c)?
            .to_std_string_escaped();
        let post_data = args
            .get_or_undefined(3)
            .to_string(&mut c)?
            .to_std_string_escaped();
        (url, cookies, headers_json, post_data)
    };

    let allow_cookies = TL_ALLOW_COOKIES.with(|b| *b.borrow());
    let (ok, body, out_cookies, error, is_net) =
        execute_ssrf_safe_http_fetch(&url, &cookies, &headers_json, &post_data, allow_cookies)
            .await;

    let mut c = ctx.borrow_mut();
    let obj = JsObject::with_object_proto(c.intrinsics());
    obj.set(js_string!("ok"), ok, false, &mut c)?;
    obj.set(js_string!("body"), js_string!(body), false, &mut c)?;
    obj.set(
        js_string!("cookies"),
        js_string!(out_cookies),
        false,
        &mut c,
    )?;
    obj.set(js_string!("error"), js_string!(error), false, &mut c)?;
    obj.set(js_string!("isNetworkError"), is_net, false, &mut c)?;
    Ok(obj.into())
}

async fn host_python_exec(
    _this: &JsValue,
    args: &[JsValue],
    ctx: &RefCell<&mut Context>,
) -> JsResult<JsValue> {
    let (script, args_json) = {
        let mut c = ctx.borrow_mut();
        let script = args
            .get_or_undefined(0)
            .to_string(&mut c)?
            .to_std_string_escaped();
        let args_json = args
            .get_or_undefined(1)
            .to_string(&mut c)?
            .to_std_string_escaped();
        (script, args_json)
    };

    let allowed = TL_ALLOW_PYTHON.with(|b| *b.borrow());
    let (exit_code, error, output, error_output) = if !allowed {
        (
            -1,
            "Python execution disabled".to_string(),
            String::new(),
            String::new(),
        )
    } else {
        let cli_args: Vec<String> = serde_json::from_str(&args_json).unwrap_or_default();
        let mut cmd = Command::new("python3");
        cmd.kill_on_drop(true);
        cmd.arg("-c").arg(&script);
        for a in cli_args {
            cmd.arg(a);
        }
        match tokio::time::timeout(Duration::from_secs(30), cmd.output()).await {
            Ok(Ok(out)) => {
                let code = out.status.code().unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
                let err = if out.status.success() {
                    String::new()
                } else {
                    format!("Python exited with {}", code)
                };
                (code, err, stdout, stderr)
            }
            Ok(Err(e)) => (
                -1,
                format!("Failed to spawn python3: {e}"),
                String::new(),
                String::new(),
            ),
            Err(_) => (
                -1,
                "Python script timed out".into(),
                String::new(),
                String::new(),
            ),
        }
    };

    let mut c = ctx.borrow_mut();
    let obj = JsObject::with_object_proto(c.intrinsics());
    obj.set(js_string!("exitCode"), exit_code, false, &mut c)?;
    obj.set(js_string!("error"), js_string!(error), false, &mut c)?;
    obj.set(js_string!("output"), js_string!(output), false, &mut c)?;
    obj.set(
        js_string!("errorOutput"),
        js_string!(error_output),
        false,
        &mut c,
    )?;
    Ok(obj.into())
}

fn host_log(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let line = args
        .get_or_undefined(0)
        .to_string(ctx)?
        .to_std_string_escaped();
    TL_LOGS.with(|l| l.borrow_mut().push(line));
    Ok(JsValue::undefined())
}

fn clamp_js_timeout_ms(raw_ms: f64) -> u64 {
    if raw_ms.is_nan() || raw_ms <= 0.0 {
        0
    } else if !raw_ms.is_finite() {
        MAX_JS_TIMEOUT_MS
    } else {
        raw_ms.min(MAX_JS_TIMEOUT_MS as f64) as u64
    }
}

fn host_set_timeout(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let cb = args
        .get_or_undefined(0)
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("setTimeout expects callback function"))?;
    let raw_ms = args.get_or_undefined(1).to_number(ctx).unwrap_or(0.0);
    let ms = clamp_js_timeout_ms(raw_ms);

    let timer_id = TL_TIMERS.with(|cell| -> JsResult<u32> {
        let mut st = cell.borrow_mut();
        if st.2 >= MAX_ACTIVE_JS_TIMERS {
            return Err(JsNativeError::range()
                .with_message("Too many active setTimeout timers")
                .into());
        }
        let id = st.0;
        st.0 = id.wrapping_add(1).max(1);
        st.2 += 1;
        Ok(id)
    })?;
    let job = TimeoutJob::from_duration(
        move |c| {
            let cancelled = TL_TIMERS.with(|cell| {
                let mut st = cell.borrow_mut();
                let was_cancelled = st.1.remove(&timer_id);
                st.2 = st.2.saturating_sub(1);
                was_cancelled
            });
            if !cancelled {
                cb.call(&JsValue::undefined(), &[], c)?;
            }
            Ok(JsValue::undefined())
        },
        Duration::from_millis(ms),
    );
    ctx.enqueue_job(Job::TimeoutJob(job));
    Ok(JsValue::from(timer_id as i32))
}

fn host_clear_timeout(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = args.get_or_undefined(0).to_i32(ctx).unwrap_or(0) as u32;
    if id > 0 {
        TL_TIMERS.with(|cell| {
            cell.borrow_mut().1.insert(id);
        });
    }
    Ok(JsValue::undefined())
}

/// Evaluates a JavaScript parser script inside a bounded `boa_engine` sandbox.
///
/// `allow_python` explicitly controls `launchPythonScript` (`__ferrite_python_exec`)
/// and should be wired from `config.app.plugins_allow_python`.
pub async fn run_js_parser_script(
    script_source: &str,
    eval_expr: &str,
    config: &FerriteConfig,
    ssl_exceptions: &[SslException],
    allow_python: bool,
) -> Result<(serde_json::Value, Vec<String>)> {
    run_js_parser_script_with_timeout(
        script_source,
        eval_expr,
        config,
        ssl_exceptions,
        allow_python,
        JS_EXECUTION_TIMEOUT,
    )
    .await
}

async fn run_js_parser_script_with_timeout(
    script_source: &str,
    eval_expr: &str,
    config: &FerriteConfig,
    ssl_exceptions: &[SslException],
    allow_python: bool,
    execution_timeout: Duration,
) -> Result<(serde_json::Value, Vec<String>)> {
    let user_agent = if config.core.use_system_default_user_agent {
        ferrite_core::os_integration::system_default_user_agent().to_string()
    } else {
        concat!("Ferrite/", env!("CARGO_PKG_VERSION")).to_string()
    };
    let settings = PluginHttpSettings {
        user_agent,
        proxy: config.app.proxy.clone(),
        ssl_exceptions: ssl_exceptions.to_vec(),
        allow_loopback: current_allow_loopback_for_test(),
    };
    let client = build_plugin_http_client(&settings, 443)?;
    let allow_cookies = config.app.plugins_allow_wb_cookies;
    let script_owned = script_source.to_string();
    let expr_owned = eval_expr.to_string();

    tokio::task::spawn_blocking(move || -> Result<(serde_json::Value, Vec<String>)> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let local = tokio::task::LocalSet::new();

        local.block_on(&rt, async move {
            let _guard = JsRuntimeThreadGuard;
            TL_HTTP_CLIENT.with(|c| *c.borrow_mut() = Some(client));
            TL_HTTP_SETTINGS.with(|s| *s.borrow_mut() = Some(settings));
            TL_ALLOW_COOKIES.with(|b| *b.borrow_mut() = allow_cookies);
            TL_ALLOW_PYTHON.with(|b| *b.borrow_mut() = allow_python);
            TL_LOGS.with(|l| l.borrow_mut().clear());
            TL_TIMERS.with(|t| *t.borrow_mut() = (1, std::collections::HashSet::new(), 0));

            let executor = Rc::new(SimpleJobExecutor::new());
            let cancel_token = executor.get_cancellation_token();
            let mut ctx = ContextBuilder::new()
                .job_executor(executor.clone())
                .build()
                .map_err(|e| anyhow::anyhow!("Boa context init failed: {e}"))?;

            let mut limits = RuntimeLimits::default();
            limits.set_loop_iteration_limit(JS_LOOP_ITERATION_LIMIT);
            limits.set_recursion_limit(JS_RECURSION_LIMIT);
            limits.set_stack_size_limit(JS_STACK_SIZE_LIMIT);
            ctx.set_runtime_limits(limits);

            ctx.register_global_callable(
                js_string!("__ferrite_log"),
                1,
                NativeFunction::from_copy_closure(host_log),
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            ctx.register_global_callable(
                js_string!("__ferrite_set_timeout"),
                2,
                NativeFunction::from_copy_closure(host_set_timeout),
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            ctx.register_global_callable(
                js_string!("__ferrite_clear_timeout"),
                1,
                NativeFunction::from_copy_closure(host_clear_timeout),
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            ctx.register_global_callable(
                js_string!("__ferrite_http_fetch"),
                4,
                NativeFunction::from_async_fn(host_http_fetch),
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            ctx.register_global_callable(
                js_string!("__ferrite_python_exec"),
                2,
                NativeFunction::from_async_fn(host_python_exec),
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;

            ctx.eval(Source::from_bytes(QT_JS_MS_PARSER_LIB_SHIM))
                .map_err(|e| anyhow::anyhow!("Shim eval failed: {e}"))?;
            ctx.eval(Source::from_bytes(&script_owned))
                .map_err(|e| anyhow::anyhow!("Plugin script eval failed: {e}"))?;

            let wrapped = format!(
                "globalThis.__ferrite_result = null; globalThis.__ferrite_error = null;\
                 Promise.resolve().then(function() {{ return ({expr_owned}); }})\
                 .then(function(r) {{ globalThis.__ferrite_result = JSON.stringify(r === undefined ? null : r); }},\
                       function(e) {{ globalThis.__ferrite_error = JSON.stringify(e && e.stack ? String(e) : e); }});"
            );
            ctx.eval(Source::from_bytes(&wrapped))
                .map_err(|e| anyhow::anyhow!("Eval expression failed: {e}"))?;

            match tokio::time::timeout(
                execution_timeout,
                executor.run_jobs_async(&RefCell::new(&mut ctx)),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(e)) => bail!("Async JS job error: {e}"),
                Err(_) => {
                    cancel_token.store(true, Ordering::Relaxed);
                    bail!("JS parser execution timed out");
                }
            }

            let err_val = ctx
                .global_object()
                .get(js_string!("__ferrite_error"), &mut ctx)
                .ok();
            if let Some(v) = err_val
                && !v.is_null_or_undefined()
            {
                let s = v
                    .to_string(&mut ctx)
                    .map(|j| j.to_std_string_escaped())
                    .unwrap_or_else(|_| "JS promise rejected".into());
                bail!("JS parser rejected: {s}");
            }

            let res_val = ctx
                .global_object()
                .get(js_string!("__ferrite_result"), &mut ctx)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let parsed = if res_val.is_null_or_undefined() {
                serde_json::Value::Null
            } else {
                let s = res_val
                    .to_string(&mut ctx)
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .to_std_string_escaped();
                serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s))
            };
            let logs = TL_LOGS.with(|l| l.borrow().clone());
            Ok((parsed, logs))
        })
    })
    .await?
}

#[derive(Debug, Serialize, Deserialize)]
struct PluginManifestFile {
    uid: String,
    name: String,
    version: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    auto_update: bool,
    #[serde(default)]
    domains: Vec<String>,
    #[serde(default = "default_entry")]
    entry: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    update_url: Option<String>,
}

fn default_true() -> bool {
    true
}
fn default_entry() -> String {
    "parser.js".into()
}

fn validate_relative_plugin_entry_syntax(entry: &str) -> Result<&str> {
    let trimmed = entry.trim();
    if trimmed.is_empty()
        || trimmed.contains('\0')
        || trimmed.contains('\\')
        || trimmed.starts_with('/')
    {
        bail!("Invalid plugin entry path '{}'", entry);
    }
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        bail!("Plugin entry path '{}' must be relative", entry);
    }
    let rel = Path::new(trimmed);
    if rel.is_absolute() {
        bail!("Plugin entry path '{}' must be relative", entry);
    }
    let mut has_normal = false;
    for comp in rel.components() {
        match comp {
            Component::Normal(_) => has_normal = true,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!(
                    "Plugin entry path '{}' contains disallowed component",
                    entry
                );
            }
        }
    }
    if !has_normal {
        bail!("Plugin entry path '{}' must name a file", entry);
    }
    Ok(trimmed)
}

fn resolve_safe_plugin_entry(plugin_dir: &Path, entry: &str) -> Result<PathBuf> {
    let rel = validate_relative_plugin_entry_syntax(entry)?;
    let canon_dir = std::fs::canonicalize(plugin_dir)
        .with_context(|| format!("Failed to canonicalize plugin dir {}", plugin_dir.display()))?;
    let candidate = plugin_dir.join(rel);
    let canon_entry = std::fs::canonicalize(&candidate)
        .with_context(|| format!("Plugin entry '{}' not found", entry))?;
    if !canon_entry.starts_with(&canon_dir) {
        bail!("Plugin entry path '{}' escapes plugin directory", entry);
    }
    if !canon_entry.is_file() {
        bail!("Plugin entry '{}' is not a regular file", entry);
    }
    Ok(canon_entry)
}

async fn resolve_safe_plugin_entry_async(plugin_dir: &Path, entry: &str) -> Result<PathBuf> {
    let rel = validate_relative_plugin_entry_syntax(entry)?.to_string();
    let canon_dir = tokio::fs::canonicalize(plugin_dir)
        .await
        .with_context(|| format!("Failed to canonicalize plugin dir {}", plugin_dir.display()))?;
    let candidate = plugin_dir.join(&rel);
    let canon_entry = tokio::fs::canonicalize(&candidate)
        .await
        .with_context(|| format!("Plugin entry '{}' not found", entry))?;
    if !canon_entry.starts_with(&canon_dir) {
        bail!("Plugin entry path '{}' escapes plugin directory", entry);
    }
    let meta = tokio::fs::metadata(&canon_entry)
        .await
        .with_context(|| format!("Plugin entry '{}' metadata failed", entry))?;
    if !meta.is_file() {
        bail!("Plugin entry '{}' is not a regular file", entry);
    }
    Ok(canon_entry)
}

fn sanitize_plugin_folder_name(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        "plugin".into()
    } else {
        sanitized
    }
}

pub fn list_plugins(plugins_dir: &Path) -> Vec<JsPluginInfo> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(plugins_dir) else {
        return out;
    };
    for ent in rd.flatten() {
        let dir = ent.path();
        if !dir.is_dir() {
            continue;
        }
        let json_path = dir.join("manifest.json");
        if json_path.is_file()
            && let Ok(raw) = std::fs::read_to_string(&json_path)
            && let Ok(m) = serde_json::from_str::<PluginManifestFile>(&raw)
            && let Ok(entry_script) = resolve_safe_plugin_entry(&dir, &m.entry)
        {
            out.push(JsPluginInfo {
                uid: m.uid,
                name: m.name,
                version: m.version,
                author: m.author,
                description: m.description,
                enabled: m.enabled,
                auto_update: m.auto_update,
                update_url: m.update_url,
                domains: m.domains,
                entry_script,
                dir,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn set_plugin_enabled(plugins_dir: &Path, uid: &str, enabled: bool) -> Result<()> {
    for p in list_plugins(plugins_dir) {
        if p.uid == uid {
            let jp = p.dir.join("manifest.json");
            let raw = std::fs::read_to_string(&jp)?;
            let mut mf: PluginManifestFile = serde_json::from_str(&raw)?;
            mf.enabled = enabled;
            std::fs::write(&jp, serde_json::to_string_pretty(&mf)?)?;
            return Ok(());
        }
    }
    bail!("Plugin '{uid}' not found")
}

pub fn uninstall_plugin(plugins_dir: &Path, uid: &str) -> Result<()> {
    for p in list_plugins(plugins_dir) {
        if p.uid == uid {
            std::fs::remove_dir_all(&p.dir)?;
            return Ok(());
        }
    }
    bail!("Plugin '{uid}' not found")
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &to)?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

pub fn install_plugin_from_path(plugins_dir: &Path, source: &Path) -> Result<JsPluginInfo> {
    install_plugin_from_path_with_options(plugins_dir, source, false)
}

fn verify_plugin_manifest_checksum(
    pkg_dir: &Path,
    mf: &PluginManifestFile,
    ignore_sign: bool,
) -> Result<()> {
    let entry = resolve_safe_plugin_entry(pkg_dir, &mf.entry)?;
    if ignore_sign {
        return Ok(());
    }
    if let Some(ref expected) = mf.sha256 {
        let expected = expected.trim();
        if !expected.is_empty() {
            let actual =
                ferrite_core::hash::compute_file_hash(&entry, ferrite_core::HashAlgorithm::Sha256)?;
            if !actual.eq_ignore_ascii_case(expected) {
                bail!(
                    "Plugin '{}' SHA-256 signature mismatch (expected {}, got {})",
                    mf.name,
                    expected,
                    actual
                );
            }
        }
    }
    Ok(())
}

struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn install_plugin_from_path_with_options(
    plugins_dir: &Path,
    source: &Path,
    ignore_signature: bool,
) -> Result<JsPluginInfo> {
    std::fs::create_dir_all(plugins_dir)?;
    if source.is_dir() {
        let manifest_path = source.join("manifest.json");
        if !manifest_path.is_file() {
            bail!("Directory does not contain manifest.json");
        }
        let raw = std::fs::read_to_string(&manifest_path)?;
        let mf: PluginManifestFile = serde_json::from_str(&raw)?;
        verify_plugin_manifest_checksum(source, &mf, ignore_signature)?;
        let folder_name = sanitize_plugin_folder_name(&mf.uid);
        let dest = plugins_dir.join(folder_name);
        if dest.exists() {
            std::fs::remove_dir_all(&dest)?;
        }
        copy_dir_recursive(source, &dest)?;
        return list_plugins(plugins_dir)
            .into_iter()
            .find(|p| p.uid == mf.uid)
            .context("Installed plugin manifest invalid");
    }

    if !source.is_file() {
        bail!("Plugin source does not exist: {}", source.display());
    }

    let ext = source
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    if ext == "js" {
        let stem = source
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("custom-plugin")
            .to_string();
        let uid = sanitize_plugin_folder_name(&stem.to_ascii_lowercase());
        let dest = plugins_dir.join(&uid);
        std::fs::create_dir_all(&dest)?;
        std::fs::copy(source, dest.join("parser.js"))?;
        let mf = PluginManifestFile {
            uid: uid.clone(),
            name: stem,
            version: "1.0.0".into(),
            author: "User".into(),
            description: "Imported JS parser plugin".into(),
            enabled: true,
            auto_update: false,
            domains: vec![],
            entry: "parser.js".into(),
            sha256: None,
            update_url: None,
        };
        std::fs::write(
            dest.join("manifest.json"),
            serde_json::to_string_pretty(&mf)?,
        )?;
        return list_plugins(plugins_dir)
            .into_iter()
            .find(|p| p.uid == uid)
            .context("Failed to load installed JS plugin");
    }

    // Treat .fda / .zip archives via system unzip into staging dir
    let canon_source = std::fs::canonicalize(source)
        .with_context(|| format!("Failed to resolve plugin archive {}", source.display()))?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let stage = plugins_dir.join(format!(".stage-{}-{}", std::process::id(), ts));
    std::fs::create_dir_all(&stage)?;
    let _stage_guard = TempDirGuard(stage.clone());

    let out = std::process::Command::new("unzip")
        .args(["-q", "-o"])
        .arg(&canon_source)
        .arg("-d")
        .arg(&stage)
        .output();
    let ok = matches!(out, Ok(ref o) if o.status.success());
    if !ok {
        bail!("Failed to extract plugin archive {}", source.display());
    }

    // Locate manifest.json either at root or one subdirectory deep
    let root_manifest = stage.join("manifest.json");
    let pkg_root = if root_manifest.is_file() {
        stage.clone()
    } else {
        std::fs::read_dir(&stage)?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.is_dir() && p.join("manifest.json").is_file())
            .context("Archive does not contain manifest.json")?
    };

    let raw = std::fs::read_to_string(pkg_root.join("manifest.json"))?;
    let mf: PluginManifestFile = serde_json::from_str(&raw)?;
    verify_plugin_manifest_checksum(&pkg_root, &mf, ignore_signature)?;
    let folder_name = sanitize_plugin_folder_name(&mf.uid);
    let dest = plugins_dir.join(folder_name);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    copy_dir_recursive(&pkg_root, &dest)?;
    drop(_stage_guard);
    list_plugins(plugins_dir)
        .into_iter()
        .find(|p| p.uid == mf.uid)
        .context("Installed archive plugin invalid")
}

#[derive(Debug, Deserialize)]
struct PluginRemoteUpdateManifest {
    version: String,
    #[serde(default)]
    download_url: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    script_url: Option<String>,
    #[serde(default)]
    sha256: Option<String>,
}

fn require_https_url(raw: &str, context_label: &str) -> Result<url::Url> {
    let parsed = url::Url::parse(raw.trim())
        .with_context(|| format!("{context_label} is not a valid URL"))?;
    if !parsed.scheme().eq_ignore_ascii_case("https") {
        bail!("{context_label} must use https://");
    }
    Ok(parsed)
}

/// Checks plugins that declare an `update_url` and installs newer versions if available.
/// Only contacts HTTPS URLs explicitly declared in user-installed plugin manifests (zero telemetry)
/// and requires SHA-256 verification unless `plugins_dev_ignore_sign` is enabled.
pub async fn check_and_update_plugins(
    plugins_dir: &Path,
    config: &FerriteConfig,
    ssl_exceptions: &[SslException],
) -> Result<Vec<PluginUpdateOutcome>> {
    let plugins_dir_owned = plugins_dir.to_path_buf();
    let installed = tokio::task::spawn_blocking(move || list_plugins(&plugins_dir_owned)).await?;
    let updatable: Vec<_> = installed
        .into_iter()
        .filter(|p| {
            p.update_url
                .as_deref()
                .is_some_and(|u| !u.trim().is_empty())
        })
        .filter(|p| config.app.update_plugins_automatically || p.auto_update)
        .collect();
    if updatable.is_empty() {
        return Ok(Vec::new());
    }

    let shared_client = if ssl_exceptions.is_empty() {
        Some(build_http_client(config, ssl_exceptions, None)?)
    } else {
        None
    };
    let mut updated = Vec::new();
    for (idx, pl) in updatable.into_iter().enumerate() {
        let Some(ref upd_url) = pl.update_url else {
            continue;
        };
        let parsed_upd = require_https_url(upd_url, &format!("Plugin '{}' update_url", pl.name))?;
        let upd_client = match shared_client {
            Some(ref c) => c.clone(),
            None => build_http_client_for_url(config, ssl_exceptions, None, parsed_upd.as_str())?,
        };

        let resp = match upd_client
            .get(parsed_upd.as_str())
            .timeout(Duration::from_secs(12))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r,
            _ => continue,
        };
        if !resp.url().scheme().eq_ignore_ascii_case("https") {
            bail!(
                "Plugin '{}' update_url redirected to non-HTTPS URL",
                pl.name
            );
        }
        let manifest_bytes = read_response_bytes_limited(resp, MAX_PLUGIN_MANIFEST_BYTES).await?;
        let remote: PluginRemoteUpdateManifest = match serde_json::from_slice(&manifest_bytes) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if compare_semver(&remote.version, &pl.version) != std::cmp::Ordering::Greater {
            continue;
        }
        let Some(pkg_url) = remote.download_url.or(remote.url).or(remote.script_url) else {
            continue;
        };
        let parsed_pkg = require_https_url(&pkg_url, &format!("Plugin '{}' package URL", pl.name))?;
        let pkg_client = match shared_client {
            Some(ref c) => c.clone(),
            None => build_http_client_for_url(config, ssl_exceptions, None, parsed_pkg.as_str())?,
        };

        let pkg_resp = match pkg_client
            .get(parsed_pkg.as_str())
            .timeout(Duration::from_secs(20))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r,
            _ => continue,
        };
        if !pkg_resp.url().scheme().eq_ignore_ascii_case("https") {
            bail!(
                "Plugin '{}' package URL redirected to non-HTTPS URL",
                pl.name
            );
        }
        let bytes = read_response_bytes_limited(pkg_resp, MAX_PLUGIN_ARCHIVE_BYTES).await?;

        if !config.app.plugins_dev_ignore_sign {
            let expected = remote
                .sha256
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .with_context(|| {
                    format!(
                        "Plugin '{}' update manifest missing required sha256 checksum",
                        pl.name
                    )
                })?;
            let actual = hex::encode(Sha256::digest(&bytes));
            if !actual.eq_ignore_ascii_case(expected) {
                bail!(
                    "Plugin update '{}' SHA-256 mismatch (expected {}, got {})",
                    pl.name,
                    expected,
                    actual
                );
            }
        }

        let ext = parsed_pkg
            .path_segments()
            .and_then(|mut s| s.next_back().map(str::to_string))
            .and_then(|f| {
                Path::new(&f)
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
            })
            .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or_else(|| "js".into());

        let stage_dir = plugins_dir.join(format!(".upd-stage-{}-{}", std::process::id(), idx));
        let _ = tokio::fs::remove_dir_all(&stage_dir).await;
        tokio::fs::create_dir_all(&stage_dir).await?;
        let stage_file = stage_dir.join(format!("payload.{ext}"));
        if let Err(e) = tokio::fs::write(&stage_file, &bytes).await {
            let _ = tokio::fs::remove_dir_all(&stage_dir).await;
            return Err(e.into());
        }

        let install_res: Result<()> = async {
            if ext == "js" {
                let manifest_path = pl.dir.join("manifest.json");
                let raw = tokio::fs::read_to_string(&manifest_path).await?;
                let mut mf: PluginManifestFile = serde_json::from_str(&raw)?;
                let safe_entry = resolve_safe_plugin_entry_async(&pl.dir, &mf.entry).await?;
                tokio::fs::copy(&stage_file, &safe_entry).await?;
                mf.version = remote.version.clone();
                if remote.sha256.is_some() {
                    mf.sha256 = remote.sha256.clone();
                }
                tokio::fs::write(&manifest_path, serde_json::to_string_pretty(&mf)?).await?;
            } else {
                let plugins_dir_clone = plugins_dir.to_path_buf();
                let stage_file_clone = stage_file.clone();
                let ignore_sign = config.app.plugins_dev_ignore_sign;
                tokio::task::spawn_blocking(move || {
                    install_plugin_from_path_with_options(
                        &plugins_dir_clone,
                        &stage_file_clone,
                        ignore_sign,
                    )
                })
                .await??;
            }
            Ok(())
        }
        .await;

        let _ = tokio::fs::remove_dir_all(&stage_dir).await;
        install_res?;

        updated.push(PluginUpdateOutcome {
            uid: pl.uid,
            name: pl.name,
            old_version: pl.version,
            new_version: remote.version,
        });
    }
    Ok(updated)
}

pub fn resolve_ffmpeg_path(config: &FerriteConfig) -> Option<PathBuf> {
    if let Some(ref p) = config.core.ffmpeg_path
        && p.exists()
    {
        return Some(p.clone());
    }
    for cand in [
        "/opt/homebrew/bin/ffmpeg",
        "/usr/local/bin/ffmpeg",
        "/usr/bin/ffmpeg",
    ] {
        let p = PathBuf::from(cand);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

async fn resolve_ffmpeg_path_async(config: &FerriteConfig) -> Option<PathBuf> {
    if let Some(ref p) = config.core.ffmpeg_path
        && tokio::fs::try_exists(p).await.unwrap_or(false)
    {
        return Some(p.clone());
    }
    for cand in [
        "/opt/homebrew/bin/ffmpeg",
        "/usr/local/bin/ffmpeg",
        "/usr/bin/ffmpeg",
    ] {
        let p = PathBuf::from(cand);
        if tokio::fs::try_exists(&p).await.unwrap_or(false) {
            return Some(p);
        }
    }
    None
}

pub fn resolve_ffprobe_path(config: &FerriteConfig) -> Option<PathBuf> {
    if let Some(ffmpeg) = resolve_ffmpeg_path(config)
        && let Some(parent) = ffmpeg.parent()
    {
        let probe = parent.join("ffprobe");
        if probe.exists() {
            return Some(probe);
        }
    }
    for cand in [
        "/opt/homebrew/bin/ffprobe",
        "/usr/local/bin/ffprobe",
        "/usr/bin/ffprobe",
    ] {
        let p = PathBuf::from(cand);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

async fn resolve_ffprobe_path_async(config: &FerriteConfig) -> Option<PathBuf> {
    if let Some(ffmpeg) = resolve_ffmpeg_path_async(config).await
        && let Some(parent) = ffmpeg.parent()
    {
        let probe = parent.join("ffprobe");
        if tokio::fs::try_exists(&probe).await.unwrap_or(false) {
            return Some(probe);
        }
    }
    for cand in [
        "/opt/homebrew/bin/ffprobe",
        "/usr/local/bin/ffprobe",
        "/usr/bin/ffprobe",
    ] {
        let p = PathBuf::from(cand);
        if tokio::fs::try_exists(&p).await.unwrap_or(false) {
            return Some(p);
        }
    }
    None
}

pub fn resolve_yt_dlp_path(config: &FerriteConfig) -> Option<PathBuf> {
    if let Some(ref p) = config.core.yt_dlp_path
        && p.exists()
    {
        return Some(p.clone());
    }
    for cand in [
        "/opt/homebrew/bin/yt-dlp",
        "/usr/local/bin/yt-dlp",
        "/usr/bin/yt-dlp",
    ] {
        let p = PathBuf::from(cand);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

async fn resolve_yt_dlp_path_async(config: &FerriteConfig) -> Option<PathBuf> {
    if let Some(ref p) = config.core.yt_dlp_path
        && tokio::fs::try_exists(p).await.unwrap_or(false)
    {
        return Some(p.clone());
    }
    for cand in [
        "/opt/homebrew/bin/yt-dlp",
        "/usr/local/bin/yt-dlp",
        "/usr/bin/yt-dlp",
    ] {
        let p = PathBuf::from(cand);
        if tokio::fs::try_exists(&p).await.unwrap_or(false) {
            return Some(p);
        }
    }
    None
}

pub async fn ffprobe_inspect(config: &FerriteConfig, input: &Path) -> Result<MediaProbeInfo> {
    let ffprobe = resolve_ffprobe_path_async(config)
        .await
        .context("ffprobe not found")?;
    let mut cmd = Command::new(ffprobe);
    cmd.kill_on_drop(true);
    let out = cmd
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            "--",
        ])
        .arg(input)
        .output()
        .await?;
    if !out.status.success() {
        bail!("ffprobe failed");
    }
    let val: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let fmt = &val["format"];
    let duration_secs = fmt["duration"].as_str().and_then(|s| s.parse::<f64>().ok());
    let bit_rate = fmt["bit_rate"].as_str().and_then(|s| s.parse::<u64>().ok());
    let format_name = fmt["format_name"].as_str().unwrap_or("").to_string();
    let mut width = None;
    let mut height = None;
    let mut video_codec = None;
    let mut audio_codec = None;

    if let Some(streams) = val["streams"].as_array() {
        for st in streams {
            match st["codec_type"].as_str() {
                Some("video") if video_codec.is_none() => {
                    video_codec = st["codec_name"].as_str().map(str::to_string);
                    width = st["width"].as_u64().map(|n| n as u32);
                    height = st["height"].as_u64().map(|n| n as u32);
                }
                Some("audio") if audio_codec.is_none() => {
                    audio_codec = st["codec_name"].as_str().map(str::to_string);
                }
                _ => {}
            }
        }
    }

    Ok(MediaProbeInfo {
        duration_secs,
        format_name,
        width,
        height,
        video_codec,
        audio_codec,
        bit_rate,
    })
}

async fn run_ffmpeg_with_progress<F>(
    ffmpeg: &Path,
    args: &[&str],
    output: &Path,
    total_duration_secs: Option<f64>,
    mut on_progress: F,
) -> Result<()>
where
    F: FnMut(f32) + Send,
{
    let mut cmd = Command::new(ffmpeg);
    cmd.kill_on_drop(true);
    let mut child = cmd
        .args(["-progress", "pipe:1", "-nostats"])
        .args(args)
        .arg("--")
        .arg(output)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_fut = async {
        if let Some(out_pipe) = stdout {
            let mut lines = BufReader::new(out_pipe).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(val) = line.strip_prefix("out_time_ms=") {
                    if let (Ok(us), Some(tot)) = (val.trim().parse::<f64>(), total_duration_secs)
                        && tot > 0.0
                    {
                        let pct = ((us / 1_000_000.0) / tot * 100.0).clamp(0.0, 99.9) as f32;
                        on_progress(pct);
                    }
                } else if line.trim() == "progress=end" {
                    on_progress(100.0);
                }
            }
        }
    };

    let stderr_fut = async {
        const MAX_STDERR_TAIL: usize = 16 * 1024;
        let mut tail = Vec::new();
        if let Some(mut err_pipe) = stderr {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err_pipe.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let take = n.min(MAX_STDERR_TAIL);
                let slice = &buf[n - take..n];
                let total = tail.len() + slice.len();
                if total > MAX_STDERR_TAIL {
                    let drop_n = total - MAX_STDERR_TAIL;
                    let drain_n = drop_n.min(tail.len());
                    tail.drain(..drain_n);
                }
                tail.extend_from_slice(slice);
            }
        }
        String::from_utf8_lossy(&tail).into_owned()
    };

    let ((), stderr_tail) = tokio::join!(stdout_fut, stderr_fut);

    let status = child.wait().await?;
    if !status.success() {
        let msg = stderr_tail.trim();
        if msg.is_empty() {
            bail!("ffmpeg exited with {}", status);
        }
        bail!("ffmpeg exited with {}: {}", status, msg);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp3QualityOption {
    Cbr(u32),
    Vbr(u8),
}

pub async fn convert_to_mp3<F>(
    config: &FerriteConfig,
    input: &Path,
    output: &Path,
    bitrate_kbps: u32,
    on_progress: F,
) -> Result<()>
where
    F: FnMut(f32) + Send,
{
    convert_to_mp3_with_options(
        config,
        input,
        output,
        Mp3QualityOption::Cbr(bitrate_kbps),
        on_progress,
    )
    .await
}

pub async fn convert_to_mp3_with_options<F>(
    config: &FerriteConfig,
    input: &Path,
    output: &Path,
    quality: Mp3QualityOption,
    on_progress: F,
) -> Result<()>
where
    F: FnMut(f32) + Send,
{
    let ffmpeg = resolve_ffmpeg_path_async(config)
        .await
        .context("ffmpeg not found")?;
    let probe = ffprobe_inspect(config, input).await.ok();
    let total_dur = probe.and_then(|p| p.duration_secs);
    let in_s = input.to_string_lossy().into_owned();
    match quality {
        Mp3QualityOption::Cbr(kbps) => {
            let br = format!("{}k", kbps.clamp(64, 320));
            run_ffmpeg_with_progress(
                &ffmpeg,
                &[
                    "-y",
                    "-i",
                    &in_s,
                    "-vn",
                    "-acodec",
                    "libmp3lame",
                    "-b:a",
                    &br,
                ],
                output,
                total_dur,
                on_progress,
            )
            .await
        }
        Mp3QualityOption::Vbr(q) => {
            let qv = q.min(9).to_string();
            run_ffmpeg_with_progress(
                &ffmpeg,
                &[
                    "-y",
                    "-i",
                    &in_s,
                    "-vn",
                    "-acodec",
                    "libmp3lame",
                    "-q:a",
                    &qv,
                ],
                output,
                total_dur,
                on_progress,
            )
            .await
        }
    }
}

pub async fn convert_to_mp4<F>(
    config: &FerriteConfig,
    input: &Path,
    output: &Path,
    stream_copy: bool,
    on_progress: F,
) -> Result<()>
where
    F: FnMut(f32) + Send,
{
    let ffmpeg = resolve_ffmpeg_path_async(config)
        .await
        .context("ffmpeg not found")?;
    let probe = ffprobe_inspect(config, input).await.ok();
    let total_dur = probe.and_then(|p| p.duration_secs);
    let in_s = input.to_string_lossy().into_owned();
    if stream_copy {
        run_ffmpeg_with_progress(
            &ffmpeg,
            &["-y", "-i", &in_s, "-c", "copy", "-movflags", "+faststart"],
            output,
            total_dur,
            on_progress,
        )
        .await
    } else {
        run_ffmpeg_with_progress(
            &ffmpeg,
            &[
                "-y",
                "-i",
                &in_s,
                "-c:v",
                "libx264",
                "-c:a",
                "aac",
                "-movflags",
                "+faststart",
            ],
            output,
            total_dur,
            on_progress,
        )
        .await
    }
}

pub async fn merge_av_files(
    config: &FerriteConfig,
    video_path: &Path,
    audio_path: &Path,
    output: &Path,
) -> Result<()> {
    let ffmpeg = resolve_ffmpeg_path_async(config)
        .await
        .context("ffmpeg not found")?;
    let mut cmd = Command::new(ffmpeg);
    cmd.kill_on_drop(true);
    let status = cmd
        .args(["-y", "-i"])
        .arg(video_path)
        .args(["-i"])
        .arg(audio_path)
        .args(["-c", "copy", "-movflags", "+faststart", "--"])
        .arg(output)
        .output()
        .await?;
    if !status.status.success() {
        bail!(
            "ffmpeg merge failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }
    Ok(())
}

pub async fn extract_video_thumbnail(
    config: &FerriteConfig,
    input: &Path,
    output_jpg: &Path,
    timestamp_secs: f64,
) -> Result<()> {
    let ffmpeg = resolve_ffmpeg_path_async(config)
        .await
        .context("ffmpeg not found")?;
    let ts = format!("{:.2}", timestamp_secs.max(0.0));
    let mut cmd = Command::new(ffmpeg);
    cmd.kill_on_drop(true);
    let out = cmd
        .args(["-y", "-ss", &ts, "-i"])
        .arg(input)
        .args(["-vframes", "1", "-q:v", "3", "--"])
        .arg(output_jpg)
        .output()
        .await?;
    if !out.status.success() {
        bail!(
            "Thumbnail extraction failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

fn extract_json_headers(val: &serde_json::Value) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    if let Some(obj) = val.as_object() {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                let kt = k.trim();
                let vt = s.trim();
                if !kt.is_empty() && !vt.is_empty() {
                    out.insert(kt.to_string(), vt.to_string());
                }
            }
        }
    }
    out
}

/// Probes `url` using enabled JS media parser plugins, direct HLS/DASH manifests, or `yt-dlp`.
pub async fn probe_media_url(
    url: &str,
    plugins_dir: &Path,
    config: &FerriteConfig,
    ssl_exceptions: &[SslException],
) -> Result<MediaParseResult> {
    let parsed_url = url::Url::parse(url)?;
    if !matches!(parsed_url.scheme(), "http" | "https") {
        bail!("Unsupported media URL scheme '{}'", parsed_url.scheme());
    }
    let host = parsed_url.host_str().unwrap_or("").to_ascii_lowercase();

    // 1. Check matching enabled JS plugins first
    let plugins_dir_owned = plugins_dir.to_path_buf();
    let plugins = tokio::task::spawn_blocking(move || list_plugins(&plugins_dir_owned)).await?;
    for plugin in plugins {
        if !plugin.enabled {
            continue;
        }
        let domain_match = plugin.domains.is_empty()
            || plugin
                .domains
                .iter()
                .any(|d| host == *d || host.ends_with(&format!(".{d}")));
        if !domain_match {
            continue;
        }
        if let Ok(script) = tokio::fs::read_to_string(&plugin.entry_script).await {
            let escaped = serde_json::to_string(url)?;
            let expr = format!(
                "(typeof msParser !== 'undefined' && (!msParser.isSupportedSource || msParser.isSupportedSource({escaped}))) ? msParser.parse({escaped}) : null"
            );
            if let Ok((val, _logs)) = run_js_parser_script(
                &script,
                &expr,
                config,
                ssl_exceptions,
                config.app.plugins_allow_python,
            )
            .await
                && !val.is_null()
            {
                if let Ok(mut parsed) = serde_json::from_value::<MediaParseResult>(val.clone()) {
                    if parsed.extractor.is_empty() {
                        parsed.extractor = format!("plugin:{}", plugin.name);
                    }
                    return Ok(parsed);
                }
                // Map looser JS plugin objects cleanly
                let title = val["title"].as_str().unwrap_or("Media Stream").to_string();
                let mut variants = Vec::new();
                if let Some(arr) = val["formats"]
                    .as_array()
                    .or_else(|| val["variants"].as_array())
                {
                    for (idx, item) in arr.iter().enumerate() {
                        if let Some(u) = item["url"].as_str() {
                            variants.push(MediaVariantOption {
                                format_id: item["format_id"]
                                    .as_str()
                                    .map(str::to_string)
                                    .unwrap_or_else(|| format!("v{idx}")),
                                title: item["label"]
                                    .as_str()
                                    .or_else(|| item["title"].as_str())
                                    .unwrap_or("Stream")
                                    .to_string(),
                                ext: item["ext"].as_str().unwrap_or("mp4").to_string(),
                                url: u.to_string(),
                                audio_url: item["audio_url"].as_str().map(str::to_string),
                                width: item["width"].as_u64().map(|n| n as u32),
                                height: item["height"].as_u64().map(|n| n as u32),
                                bitrate_kbps: item["bitrate_kbps"].as_u64(),
                                codecs: item["codecs"].as_str().map(str::to_string),
                                is_audio_only: item["is_audio_only"].as_bool().unwrap_or(false),
                                filesize: item["filesize"].as_u64(),
                                http_headers: {
                                    let h = extract_json_headers(&item["http_headers"]);
                                    if h.is_empty() {
                                        extract_json_headers(&item["headers"])
                                    } else {
                                        h
                                    }
                                },
                            });
                        }
                    }
                }
                if !variants.is_empty() {
                    return Ok(MediaParseResult {
                        source_url: url.to_string(),
                        title,
                        thumbnail_url: val["thumbnail"].as_str().map(str::to_string),
                        duration_secs: val["duration"].as_f64(),
                        uploader: val["uploader"].as_str().map(str::to_string),
                        is_playlist: false,
                        variants,
                        subtitles: vec![],
                        playlist_entries: vec![],
                        extractor: format!("plugin:{}", plugin.name),
                    });
                }
            }
        }
    }

    // 2. Direct HLS (.m3u8) / DASH (.mpd) manifests
    let lower = url.to_ascii_lowercase();
    if lower.contains(".m3u8") {
        let variants =
            ferrite_hls::inspect_hls_variants_with_config(config, ssl_exceptions, url).await?;
        return Ok(MediaParseResult {
            source_url: url.to_string(),
            title: parsed_url
                .path_segments()
                .and_then(|mut s| s.next_back())
                .filter(|s| !s.is_empty())
                .unwrap_or("HLS Stream")
                .to_string(),
            thumbnail_url: None,
            duration_secs: None,
            uploader: None,
            is_playlist: false,
            variants: variants
                .into_iter()
                .enumerate()
                .map(|(i, v)| MediaVariantOption {
                    format_id: format!("hls-{i}"),
                    title: v
                        .resolution
                        .clone()
                        .unwrap_or_else(|| format!("{} kbps", v.bandwidth / 1000)),
                    ext: "mp4".into(),
                    url: v.url,
                    audio_url: None,
                    width: v
                        .resolution
                        .as_deref()
                        .and_then(|r| r.split_once('x')?.0.parse().ok()),
                    height: v
                        .resolution
                        .as_deref()
                        .and_then(|r| r.split_once('x')?.1.parse().ok()),
                    bitrate_kbps: Some(v.bandwidth / 1000),
                    codecs: v.codecs,
                    is_audio_only: false,
                    filesize: None,
                    http_headers: std::collections::BTreeMap::new(),
                })
                .collect(),
            subtitles: vec![],
            playlist_entries: vec![],
            extractor: "hls".into(),
        });
    }

    if lower.contains(".mpd") {
        let client = build_http_client_for_url(config, ssl_exceptions, None, url)?;
        let resp = client.get(url).send().await?.error_for_status()?;
        let bytes = read_response_bytes_limited(resp, MAX_HTTP_RESPONSE_BYTES).await?;
        let xml = String::from_utf8_lossy(&bytes).into_owned();
        let reps = ferrite_hls::parse_dash_mpd(url, &xml)?;
        return Ok(MediaParseResult {
            source_url: url.to_string(),
            title: "DASH Stream".into(),
            thumbnail_url: None,
            duration_secs: None,
            uploader: None,
            is_playlist: false,
            variants: reps
                .into_iter()
                .map(|r| MediaVariantOption {
                    format_id: r.id,
                    title: format!("{}p ({})", r.height.unwrap_or(0), r.mime_type),
                    ext: "mp4".into(),
                    url: url.to_string(),
                    audio_url: None,
                    width: r.width,
                    height: r.height,
                    bitrate_kbps: Some(r.bandwidth / 1000),
                    codecs: None,
                    is_audio_only: r.mime_type.starts_with("audio/"),
                    filesize: None,
                    http_headers: std::collections::BTreeMap::new(),
                })
                .collect(),
            subtitles: vec![],
            playlist_entries: vec![],
            extractor: "dash".into(),
        });
    }

    // 3. Optional yt-dlp backend (if user installed yt-dlp)
    if let Some(yt_dlp) = resolve_yt_dlp_path_async(config).await {
        let mut cmd = Command::new(yt_dlp);
        cmd.kill_on_drop(true);
        let out = cmd
            .args(["--dump-single-json", "--no-warnings", "--"])
            .arg(url)
            .output()
            .await?;
        if out.status.success() {
            let val: serde_json::Value = serde_json::from_slice(&out.stdout)?;
            let title = val["title"].as_str().unwrap_or("Media").to_string();
            let mut entries = Vec::new();
            let mut is_playlist = false;
            if let Some(arr) = val["entries"].as_array() {
                is_playlist = true;
                for e in arr {
                    if let Some(u) = e["webpage_url"].as_str().or_else(|| e["url"].as_str()) {
                        entries.push(PlaylistEntry {
                            url: u.to_string(),
                            title: e["title"].as_str().unwrap_or("Item").to_string(),
                            duration_secs: e["duration"].as_f64(),
                        });
                    }
                }
            }
            let top_headers = extract_json_headers(&val["http_headers"]);
            let mut variants = Vec::new();
            if let Some(fmts) = val["formats"].as_array() {
                for f in fmts {
                    if let Some(u) = f["url"].as_str() {
                        let mut fmt_headers = top_headers.clone();
                        for (k, v) in extract_json_headers(&f["http_headers"]) {
                            fmt_headers.insert(k, v);
                        }
                        variants.push(MediaVariantOption {
                            format_id: f["format_id"].as_str().unwrap_or("").to_string(),
                            title: f["format_note"]
                                .as_str()
                                .or_else(|| f["format"].as_str())
                                .unwrap_or("format")
                                .to_string(),
                            ext: f["ext"].as_str().unwrap_or("mp4").to_string(),
                            url: u.to_string(),
                            audio_url: None,
                            width: f["width"].as_u64().map(|n| n as u32),
                            height: f["height"].as_u64().map(|n| n as u32),
                            bitrate_kbps: f["tbr"].as_f64().map(|n| n as u64),
                            codecs: f["vcodec"].as_str().map(str::to_string),
                            is_audio_only: f["vcodec"].as_str() == Some("none"),
                            filesize: f["filesize"].as_u64(),
                            http_headers: fmt_headers,
                        });
                    }
                }
            }
            return Ok(MediaParseResult {
                source_url: url.to_string(),
                title,
                thumbnail_url: val["thumbnail"].as_str().map(str::to_string),
                duration_secs: val["duration"].as_f64(),
                uploader: val["uploader"].as_str().map(str::to_string),
                is_playlist,
                variants,
                subtitles: vec![],
                playlist_entries: entries,
                extractor: "yt-dlp".into(),
            });
        }
    }

    bail!("No installed media parser or yt-dlp handled {}", url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    struct LoopbackTestGuard(bool);

    impl LoopbackTestGuard {
        fn enable() -> Self {
            let prev = TL_ALLOW_LOOPBACK_TEST.with(|b| {
                let p = *b.borrow();
                *b.borrow_mut() = true;
                p
            });
            Self(prev)
        }
    }

    impl Drop for LoopbackTestGuard {
        fn drop(&mut self) {
            TL_ALLOW_LOOPBACK_TEST.with(|b| *b.borrow_mut() = self.0);
        }
    }

    fn build_test_https_acceptor(sans: Vec<String>) -> (TlsAcceptor, String) {
        ferrite_http::ensure_crypto_provider();
        let ck = rcgen::generate_simple_self_signed(sans).unwrap();
        let cert_der = ck.cert.der().to_vec();
        let key_der = ck.signing_key.serialize_der();
        let fp = hex::encode(Sha256::digest(&cert_der));
        let cert = CertificateDer::from(cert_der);
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der));
        let server_cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .unwrap();
        (TlsAcceptor::from(Arc::new(server_cfg)), fp)
    }

    #[tokio::test]
    async fn js_plugin_runtime_supports_async_fetch_and_timers() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = sock.read(&mut buf).await;
                    let json = r#"{"title":"Demo Clip","media":"https://cdn.example/video.mp4"}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        json.len(),
                        json
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });

        let dir = std::env::temp_dir().join(format!("ferrite_media_js_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plug_dir = dir.join("example_parser");
        std::fs::create_dir_all(&plug_dir).unwrap();

        std::fs::write(
            plug_dir.join("manifest.json"),
            r#"{"uid":"demo-parser","name":"DemoParser","version":"1.0","author":"Test","domains":["127.0.0.1"],"enabled":true}"#,
        )
        .unwrap();
        std::fs::write(
            plug_dir.join("parser.js"),
            r#"
            var msParser = {
              isSupportedSource: function(url) { return url.indexOf('127.0.0.1') >= 0; },
              parse: function(url) {
                console.log("parsing", url);
                return downloadUrlAsUtf8Text(url).then(function(res) {
                  var data = JSON.parse(res.body);
                  return new Promise(function(resolve) {
                    setTimeout(function() {
                      resolve({
                        title: data.title,
                        formats: [{ format_id: "hd", label: "720p", ext: "mp4", url: data.media, height: 720 }]
                      });
                    }, 15);
                  });
                });
              }
            };
            "#,
        )
        .unwrap();

        let cfg = FerriteConfig::default();
        let parsed = {
            let _guard = LoopbackTestGuard::enable();
            probe_media_url(&format!("http://{addr}/watch?v=1"), &dir, &cfg, &[])
                .await
                .unwrap()
        };
        assert_eq!(parsed.title, "Demo Clip");
        assert_eq!(parsed.variants.len(), 1);
        assert_eq!(parsed.variants[0].height, Some(720));
        assert_eq!(parsed.variants[0].url, "https://cdn.example/video.mp4");

        // Disable + verify toggle
        set_plugin_enabled(&dir, "demo-parser", false).unwrap();
        let listed = list_plugins(&dir);
        assert!(!listed[0].enabled);

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ffmpeg_probe_mp3_mp4_and_thumbnail_work_if_installed() {
        let cfg = FerriteConfig::default();
        let Some(ffmpeg) = resolve_ffmpeg_path(&cfg) else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("ferrite_media_ff_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let sample = dir.join("sample.mp4");
        let st = Command::new(&ffmpeg)
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1:size=320x240:rate=24",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-c:v",
                "mpeg4",
                "-c:a",
                "aac",
            ])
            .arg(&sample)
            .output()
            .await
            .unwrap();
        assert!(st.status.success(), "ffmpeg sample gen failed");

        let info = ffprobe_inspect(&cfg, &sample).await.unwrap();
        assert_eq!(info.width, Some(320));
        assert_eq!(info.height, Some(240));

        let mp3_out = dir.join("audio.mp3");
        convert_to_mp3(&cfg, &sample, &mp3_out, 192, |_| {})
            .await
            .unwrap();
        assert!(std::fs::metadata(&mp3_out).unwrap().len() > 100);

        let mp4_out = dir.join("remux.mp4");
        convert_to_mp4(&cfg, &sample, &mp4_out, true, |_| {})
            .await
            .unwrap();
        assert!(std::fs::metadata(&mp4_out).unwrap().len() > 100);

        let jpg_out = dir.join("thumb.jpg");
        extract_video_thumbnail(&cfg, &sample, &jpg_out, 0.2)
            .await
            .unwrap();
        assert!(std::fs::metadata(&jpg_out).unwrap().len() > 100);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_ffmpeg_with_progress_drains_verbose_stderr_without_deadlock() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("ferrite_media_stderr_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let fake_ffmpeg = dir.join("fake-ffmpeg.sh");
        std::fs::write(
            &fake_ffmpeg,
            r#"#!/bin/sh
# Emit >200 KiB on stderr to overflow OS pipe buffer if stderr is not drained concurrently.
i=0
while [ "$i" -lt 2500 ]; do
  printf 'verbose ffmpeg diagnostic line %04d 0123456789abcdef0123456789abcdef0123456789abcdef\n' "$i" >&2
  i=$((i + 1))
done
printf 'out_time_ms=500000\n'
printf 'progress=end\n'
exit 0
"#,
        )
        .unwrap();
        let mut perms = std::fs::metadata(&fake_ffmpeg).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake_ffmpeg, perms).unwrap();

        let mut progress_updates = Vec::new();
        let out_path = dir.join("-dash-prefixed-out.mp4");
        tokio::time::timeout(
            Duration::from_secs(10),
            run_ffmpeg_with_progress(&fake_ffmpeg, &["-y"], &out_path, Some(1.0), |pct| {
                progress_updates.push(pct)
            }),
        )
        .await
        .expect("run_ffmpeg_with_progress deadlocked on verbose stderr")
        .unwrap();

        assert!(progress_updates.iter().any(|&p| (p - 50.0).abs() < 1.0));
        assert_eq!(progress_updates.last().copied(), Some(100.0));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn js_plugin_respects_allow_wb_cookies_and_signature_bypass() {
        let seen_cookie = std::sync::Arc::new(std::sync::Mutex::new(Option::<String>::None));
        let seen_srv = std::sync::Arc::clone(&seen_cookie);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let seen_srv = std::sync::Arc::clone(&seen_srv);
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let cookie = req
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("cookie:"))
                        .map(|l| l.to_string());
                    *seen_srv.lock().unwrap() = cookie;
                    let body = "{}";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{body}"
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });

        let mut cfg = FerriteConfig::default();
        cfg.app.plugins_allow_wb_cookies = false;
        let url = format!("http://{addr}/test");
        {
            let _guard = LoopbackTestGuard::enable();
            let _ = run_js_parser_script(
                "",
                &format!("downloadUrlAsUtf8Text({url:?}, 'session=secret123')"),
                &cfg,
                &[],
                false,
            )
            .await
            .unwrap();
        }
        assert!(
            seen_cookie.lock().unwrap().is_none(),
            "cookie must be stripped when plugins_allow_wb_cookies is false"
        );

        let dir = std::env::temp_dir().join(format!("ferrite_media_sig_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let src_dir = dir.join("src");
        let dst_dir = dir.join("dst");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("parser.js"), "var msParser = {};").unwrap();
        std::fs::write(
            src_dir.join("manifest.json"),
            r#"{"uid":"sig-test","name":"SigTest","version":"1.0","sha256":"deadbeef"}"#,
        )
        .unwrap();
        assert!(install_plugin_from_path_with_options(&dst_dir, &src_dir, false).is_err());
        assert!(install_plugin_from_path_with_options(&dst_dir, &src_dir, true).is_ok());

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn python_exec_is_gated_by_allow_python_parameter_and_config() {
        TL_ALLOW_PYTHON.with(|b| assert!(!*b.borrow(), "TL_ALLOW_PYTHON must default to false"));
        let mut cfg = FerriteConfig::default();
        assert!(!cfg.app.plugins_allow_python);

        let err = run_js_parser_script(
            "",
            "launchPythonScript('req1', false, 'print(42)', [])",
            &cfg,
            &[],
            false,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("Python execution disabled"),
            "unexpected error: {err}"
        );

        let dir = std::env::temp_dir().join(format!("ferrite_media_py_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plug_dir = dir.join("py_plug");
        std::fs::create_dir_all(&plug_dir).unwrap();
        std::fs::write(
            plug_dir.join("manifest.json"),
            r#"{"uid":"py-plug","name":"PyPlug","version":"1.0","domains":["example.com"],"enabled":true}"#,
        )
        .unwrap();
        std::fs::write(
            plug_dir.join("parser.js"),
            r#"
            var msParser = {
              isSupportedSource: function() { return true; },
              parse: function(url) {
                return launchPythonScript('1', false, 'import json; print(json.dumps({"title":"PyTitle","formats":[{"url":"https://cdn.example/v.mp4"}]}))', []).then(function(res) {
                  return JSON.parse(res.output);
                });
              }
            };
            "#,
        )
        .unwrap();

        let probe_blocked = probe_media_url("https://example.com/watch", &dir, &cfg, &[]).await;
        assert!(
            probe_blocked.is_err(),
            "probe_media_url must block python when plugins_allow_python is false"
        );

        // When allow_python is true, host_python_exec attempts execution rather than gating it.
        cfg.app.plugins_allow_python = true;
        let res = run_js_parser_script(
            "",
            "launchPythonScript('req2', false, 'import sys; print(sys.argv[1])', ['hello'])",
            &cfg,
            &[],
            true,
        )
        .await;
        match res {
            Ok((val, _)) => {
                assert_eq!(val["exitCode"], serde_json::json!(0));
                assert_eq!(val["output"].as_str().unwrap_or("").trim(), "hello");
            }
            Err(e) => {
                let s = e.to_string();
                assert!(
                    !s.contains("Python execution disabled"),
                    "allow_python=true must not report disabled: {s}"
                );
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn host_http_fetch_blocks_ssrf_schemes_ips_redirects_and_oversized_bodies() {
        for bad in [
            "127.0.0.1",
            "127.255.0.1",
            "::1",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.1.1",
            "169.254.169.254",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "0.0.0.0",
            "::",
            "224.0.0.1",
            "ff02::1",
            "255.255.255.255",
            "100.64.0.1",
            "240.0.0.1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:10.0.0.1",
        ] {
            let ip: IpAddr = bad.parse().unwrap();
            assert!(is_disallowed_ip(ip, false), "expected {bad} to be blocked");
        }
        assert!(!is_disallowed_ip("93.184.216.34".parse().unwrap(), false));
        assert!(!is_disallowed_ip(
            "2606:2800:220:1:248:1893:25c8:1946".parse().unwrap(),
            false
        ));

        let cfg = FerriteConfig::default();
        for blocked_url in [
            "file:///etc/passwd",
            "ftp://example.com/resource",
            "http://127.0.0.1:80/admin",
            "http://[::1]:80/admin",
            "http://localhost:80/admin",
            "http://10.0.0.1/internal",
            "http://172.16.0.1/internal",
            "http://192.168.1.1/router",
            "http://169.254.169.254/latest/meta-data/",
            "http://[fe80::1]/",
            "http://[fc00::1]/",
            "http://0.0.0.0/",
            "http://224.0.0.1/",
            "http://[::ffff:127.0.0.1]/",
        ] {
            let err = run_js_parser_script(
                "",
                &format!("downloadUrlAsUtf8Text({blocked_url:?})"),
                &cfg,
                &[],
                false,
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(
                err.contains("Blocked"),
                "expected SSRF block for {blocked_url}, got: {err}"
            );
        }

        // Test redirect SSRF and >16 MiB body cap against a mock local server.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let first = req.lines().next().unwrap_or("");
                    if first.contains("/redir-meta") {
                        let resp = "HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest/meta-data/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        let _ = sock.write_all(resp.as_bytes()).await;
                    } else if first.contains("/redir-file") {
                        let resp = "HTTP/1.1 302 Found\r\nLocation: file:///etc/passwd\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        let _ = sock.write_all(resp.as_bytes()).await;
                    } else if first.contains("/too-big-header") {
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            MAX_HTTP_RESPONSE_BYTES + 1024
                        );
                        let _ = sock.write_all(resp.as_bytes()).await;
                    } else if first.contains("/too-big-stream") {
                        let head = "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n";
                        let _ = sock.write_all(head.as_bytes()).await;
                        let chunk = vec![b'A'; 1024 * 1024];
                        for _ in 0..17 {
                            if sock.write_all(&chunk).await.is_err() {
                                break;
                            }
                        }
                    }
                });
            }
        });

        {
            let _guard = LoopbackTestGuard::enable();
            let redir_meta = format!("http://{addr}/redir-meta");
            let err1 = run_js_parser_script(
                "",
                &format!("downloadUrlAsUtf8Text({redir_meta:?})"),
                &cfg,
                &[],
                false,
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(
                err1.contains("Blocked SSRF"),
                "redirect to metadata IP must be blocked: {err1}"
            );

            let redir_file = format!("http://{addr}/redir-file");
            let err2 = run_js_parser_script(
                "",
                &format!("downloadUrlAsUtf8Text({redir_file:?})"),
                &cfg,
                &[],
                false,
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(
                err2.contains("Blocked unsupported URL scheme"),
                "redirect to file:// must be blocked: {err2}"
            );

            let big_hdr = format!("http://{addr}/too-big-header");
            let err3 = run_js_parser_script(
                "",
                &format!("downloadUrlAsUtf8Text({big_hdr:?})"),
                &cfg,
                &[],
                false,
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(
                err3.contains("exceeds limit"),
                "oversized Content-Length must be rejected: {err3}"
            );

            let big_stream = format!("http://{addr}/too-big-stream");
            let err4 = run_js_parser_script(
                "",
                &format!("downloadUrlAsUtf8Text({big_stream:?})"),
                &cfg,
                &[],
                false,
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(
                err4.contains("exceeds limit"),
                "oversized streamed response must be rejected: {err4}"
            );
        }

        srv.abort();
    }

    #[tokio::test]
    async fn boa_js_runtime_aborts_infinite_loops_recursion_clamps_timers_and_times_out() {
        assert_eq!(clamp_js_timeout_ms(f64::NAN), 0);
        assert_eq!(clamp_js_timeout_ms(-50.0), 0);
        assert_eq!(clamp_js_timeout_ms(f64::INFINITY), MAX_JS_TIMEOUT_MS);
        assert_eq!(clamp_js_timeout_ms(1e12), MAX_JS_TIMEOUT_MS);
        assert_eq!(clamp_js_timeout_ms(25.0), 25);

        let cfg = FerriteConfig::default();

        // Top-level infinite loop aborts cleanly via RuntimeLimits
        let res_sync = tokio::time::timeout(
            Duration::from_secs(5),
            run_js_parser_script("while (true) {}", "1", &cfg, &[], false),
        )
        .await
        .expect("infinite loop in script_source must not hang worker");
        assert!(res_sync.is_err());

        // Infinite loop inside async promise / expression aborts cleanly
        let res_async = tokio::time::timeout(
            Duration::from_secs(5),
            run_js_parser_script(
                "function spin() { while (true) {} }",
                "spin()",
                &cfg,
                &[],
                false,
            ),
        )
        .await
        .expect("infinite loop in eval_expr must not hang worker");
        assert!(res_async.is_err());

        // Infinite recursion aborts cleanly via recursion limit
        let res_rec = tokio::time::timeout(
            Duration::from_secs(5),
            run_js_parser_script(
                "function rec() { return rec(); }",
                "rec()",
                &cfg,
                &[],
                false,
            ),
        )
        .await
        .expect("infinite recursion must not hang worker");
        assert!(res_rec.is_err());

        // Infinite Promise microtask loop hits execution timeout cleanly
        let res_timeout = run_js_parser_script_with_timeout(
            "function tick() { return Promise.resolve().then(tick); }",
            "tick()",
            &cfg,
            &[],
            false,
            Duration::from_millis(60),
        )
        .await;
        let msg = res_timeout.unwrap_err().to_string();
        assert!(
            msg.contains("timed out"),
            "expected execution timeout, got: {msg}"
        );
    }

    #[test]
    fn plugin_manifest_entry_rejects_traversal_and_symlink_escapes() {
        let dir = std::env::temp_dir().join(format!("ferrite_media_trav_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plugins_dir = dir.join("plugins");
        let evil_src = dir.join("evil_src");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        std::fs::create_dir_all(&evil_src).unwrap();

        let outside_file = dir.join("secret.js");
        std::fs::write(&outside_file, "var leaked = true;").unwrap();
        std::fs::write(evil_src.join("parser.js"), "var msParser = {};").unwrap();
        std::fs::create_dir_all(evil_src.join("lib")).unwrap();
        std::fs::write(evil_src.join("lib/parser.js"), "var msParser = {};").unwrap();

        for bad_entry in [
            "",
            "   ",
            ".",
            "./",
            "../secret.js",
            "lib/../../secret.js",
            "/etc/passwd",
            "C:\\Windows\\system32\\evil.js",
            "C:evil.js",
            "..\\secret.js",
            "parser\0.js",
        ] {
            assert!(
                resolve_safe_plugin_entry(&evil_src, bad_entry).is_err(),
                "resolve_safe_plugin_entry should reject {bad_entry:?}"
            );

            let mf = PluginManifestFile {
                uid: "bad-plug".into(),
                name: "BadPlug".into(),
                version: "1.0".into(),
                author: String::new(),
                description: String::new(),
                enabled: true,
                auto_update: false,
                domains: vec![],
                entry: bad_entry.into(),
                sha256: Some("0".repeat(64)),
                update_url: None,
            };
            assert!(
                verify_plugin_manifest_checksum(&evil_src, &mf, false).is_err(),
                "verify_plugin_manifest_checksum (signed) should reject {bad_entry:?}"
            );
            assert!(
                verify_plugin_manifest_checksum(&evil_src, &mf, true).is_err(),
                "verify_plugin_manifest_checksum (unsigned) should reject {bad_entry:?}"
            );

            std::fs::write(
                evil_src.join("manifest.json"),
                serde_json::to_string(&mf).unwrap(),
            )
            .unwrap();
            assert!(
                install_plugin_from_path_with_options(&plugins_dir, &evil_src, true).is_err(),
                "install_plugin_from_path_with_options should reject {bad_entry:?}"
            );
        }

        // list_plugins must also ignore a plugin whose manifest.json on disk has traversal entry
        let on_disk_bad = plugins_dir.join("tampered");
        std::fs::create_dir_all(&on_disk_bad).unwrap();
        std::fs::write(
            on_disk_bad.join("manifest.json"),
            r#"{"uid":"tampered","name":"Tampered","version":"1.0","entry":"../../secret.js","enabled":true}"#,
        )
        .unwrap();
        assert!(
            list_plugins(&plugins_dir)
                .iter()
                .all(|p| p.uid != "tampered")
        );

        // Symlink pointing outside the plugin dir must be rejected
        #[cfg(unix)]
        {
            let link_path = evil_src.join("link.js");
            let _ = std::fs::remove_file(&link_path);
            std::os::unix::fs::symlink(&outside_file, &link_path).unwrap();
            assert!(resolve_safe_plugin_entry(&evil_src, "link.js").is_err());
        }

        // Safe nested relative entry is accepted
        assert!(resolve_safe_plugin_entry(&evil_src, "lib/parser.js").is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ssl_exception_matching_respects_host_and_port() {
        let exc_443 = SslException {
            host_port: "example.com".into(),
            sha256_fingerprint: "aa".into(),
            added_at: chrono::Utc::now(),
        };
        assert!(ssl_exception_matches(&exc_443, "example.com", 443));
        assert!(!ssl_exception_matches(&exc_443, "example.com", 8443));

        let exc_8443 = SslException {
            host_port: "example.com:8443".into(),
            sha256_fingerprint: "aa".into(),
            added_at: chrono::Utc::now(),
        };
        assert!(ssl_exception_matches(&exc_8443, "example.com", 8443));
        assert!(!ssl_exception_matches(&exc_8443, "example.com", 443));

        let exc_v6 = SslException {
            host_port: "[::1]:9443".into(),
            sha256_fingerprint: "aa".into(),
            added_at: chrono::Utc::now(),
        };
        assert!(ssl_exception_matches(&exc_v6, "::1", 9443));
        assert!(!ssl_exception_matches(&exc_v6, "::1", 443));
    }

    #[tokio::test]
    async fn semver_and_plugin_auto_update_work_end_to_end() {
        assert_eq!(
            compare_semver("1.2.0", "1.1.9"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(compare_semver("v2.0", "2.0.0"), std::cmp::Ordering::Equal);
        assert_eq!(
            compare_semver("1.0.0-alpha", "1.0.0"),
            std::cmp::Ordering::Less
        );

        let dir = std::env::temp_dir().join(format!("ferrite_pl_upd_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plugins_dir = dir.join("plugins");
        let pl_dir = plugins_dir.join("demo");
        std::fs::create_dir_all(&pl_dir).unwrap();
        std::fs::write(pl_dir.join("parser.js"), "var oldv = 1;").unwrap();

        let new_script = b"var oldv = 2;";
        let new_hash = hex::encode(Sha256::digest(new_script));

        let (acceptor, cert_fp) =
            build_test_https_acceptor(vec!["127.0.0.1".into(), "localhost".into()]);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_https_url = format!("https://{addr}");

        let manifest_mode = std::sync::Arc::new(std::sync::Mutex::new(String::from("valid")));
        let mode_srv = std::sync::Arc::clone(&manifest_mode);
        let hash_srv = new_hash.clone();
        let base_srv = base_https_url.clone();

        let srv = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                let acceptor = acceptor.clone();
                let mode = std::sync::Arc::clone(&mode_srv);
                let hash = hash_srv.clone();
                let base = base_srv.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(sock).await else {
                        return;
                    };
                    let mut buf = [0u8; 2048];
                    let n = tls.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let first = req.lines().next().unwrap_or("");
                    let cur_mode = mode.lock().unwrap().clone();
                    let (ctype, body): (&str, Vec<u8>) = if first.contains("/manifest.json") {
                        let mj = match cur_mode.as_str() {
                            "http_pkg" => format!(
                                r#"{{"version":"1.2.0","script_url":"http://127.0.0.1:{}/parser.js","sha256":"{hash}"}}"#,
                                addr.port()
                            ),
                            "missing_sha" => {
                                format!(r#"{{"version":"1.2.0","script_url":"{base}/parser.js"}}"#)
                            }
                            "bad_sha" => format!(
                                r#"{{"version":"1.2.0","script_url":"{base}/parser.js","sha256":"{}"}}"#,
                                "0".repeat(64)
                            ),
                            _ => format!(
                                r#"{{"version":"1.2.0","script_url":"{base}/parser.js","sha256":"{hash}"}}"#
                            ),
                        };
                        ("application/json", mj.into_bytes())
                    } else {
                        ("application/javascript", new_script.to_vec())
                    };
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = tls.write_all(head.as_bytes()).await;
                    let _ = tls.write_all(&body).await;
                    let _ = tls.shutdown().await;
                });
            }
        });

        let ssl = vec![SslException {
            host_port: format!("127.0.0.1:{}", addr.port()),
            sha256_fingerprint: cert_fp,
            added_at: chrono::Utc::now(),
        }];

        let mut cfg = FerriteConfig::default();
        cfg.app.update_plugins_automatically = true;

        // 1. Plain http:// update_url must be rejected.
        std::fs::write(
            pl_dir.join("manifest.json"),
            format!(
                r#"{{"uid":"demo","name":"Demo","version":"1.0.0","enabled":true,"auto_update":true,"update_url":"http://{addr}/manifest.json"}}"#
            ),
        )
        .unwrap();
        let err_http_upd = check_and_update_plugins(&plugins_dir, &cfg, &ssl)
            .await
            .unwrap_err()
            .to_string();
        assert!(err_http_upd.contains("https://"), "got: {err_http_upd}");

        // Switch plugin manifest to https:// update_url
        std::fs::write(
            pl_dir.join("manifest.json"),
            format!(
                r#"{{"uid":"demo","name":"Demo","version":"1.0.0","enabled":true,"auto_update":true,"update_url":"{base_https_url}/manifest.json"}}"#
            ),
        )
        .unwrap();

        // 2. Remote manifest pointing to http:// script_url must be rejected.
        *manifest_mode.lock().unwrap() = "http_pkg".into();
        let err_http_pkg = check_and_update_plugins(&plugins_dir, &cfg, &ssl)
            .await
            .unwrap_err()
            .to_string();
        assert!(err_http_pkg.contains("https://"), "got: {err_http_pkg}");

        // 3. Missing sha256 must be rejected when plugins_dev_ignore_sign is false.
        *manifest_mode.lock().unwrap() = "missing_sha".into();
        let err_missing_sha = check_and_update_plugins(&plugins_dir, &cfg, &ssl)
            .await
            .unwrap_err()
            .to_string();
        assert!(err_missing_sha.contains("sha256"), "got: {err_missing_sha}");

        // 4. Mismatched sha256 must be rejected when plugins_dev_ignore_sign is false.
        *manifest_mode.lock().unwrap() = "bad_sha".into();
        let err_bad_sha = check_and_update_plugins(&plugins_dir, &cfg, &ssl)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err_bad_sha.contains("SHA-256 mismatch"),
            "got: {err_bad_sha}"
        );

        // 5. Valid https:// + valid sha256 succeeds end-to-end.
        *manifest_mode.lock().unwrap() = "valid".into();
        let updated = check_and_update_plugins(&plugins_dir, &cfg, &ssl)
            .await
            .unwrap();
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].new_version, "1.2.0");
        assert_eq!(
            std::fs::read_to_string(pl_dir.join("parser.js")).unwrap(),
            "var oldv = 2;"
        );

        // 6. When plugins_dev_ignore_sign is true, missing sha256 over https:// succeeds.
        std::fs::write(
            pl_dir.join("manifest.json"),
            format!(
                r#"{{"uid":"demo","name":"Demo","version":"1.0.0","enabled":true,"auto_update":true,"update_url":"{base_https_url}/manifest.json"}}"#
            ),
        )
        .unwrap();
        std::fs::write(pl_dir.join("parser.js"), "var oldv = 1;").unwrap();
        *manifest_mode.lock().unwrap() = "missing_sha".into();
        cfg.app.plugins_dev_ignore_sign = true;
        let updated_dev = check_and_update_plugins(&plugins_dir, &cfg, &ssl)
            .await
            .unwrap();
        assert_eq!(updated_dev.len(), 1);

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
