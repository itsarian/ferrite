use anyhow::Result;
use base64::Engine as _;
use ferrite_core::{
    AuthCredential, BandwidthLimiter, DownloadAbortReason, DownloadFileEntry, DownloadPriority,
    DownloadRecord, DownloadState, FdmConfig, NetworkProxyMode, ProxySettings, ResumeSupport,
    SegmentProgress, SegmentState, SslException,
    hash::sha256_str,
    os_integration::set_mark_of_the_web,
    routing::{classify_file_type, sanitize_filename, urlencoding_decode},
};
use futures::StreamExt;
use regex::Regex;
use reqwest::header::{
    ACCEPT, ACCEPT_ENCODING, ACCEPT_LANGUAGE, AUTHORIZATION, COOKIE, HeaderMap, HeaderName,
    HeaderValue, LOCATION, PROXY_AUTHORIZATION, RANGE, REFERER, SET_COOKIE, USER_AGENT,
};
use reqwest::{Client, Method, Proxy, StatusCode};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as TlsError, SignatureScheme};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use suppaftp::tokio::{AsyncRustlsConnector, AsyncRustlsFtpStream};
use tokio::io::AsyncReadExt;
use tokio::sync::watch;
use url::Url;

pub fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub final_url: String,
    pub total_bytes: Option<u64>,
    pub resume_support: ResumeSupport,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
    pub suggested_filename: Option<String>,
    pub supports_range_split: bool,
}

#[derive(Debug, Clone)]
pub enum ControlCommand {
    Run,
    Pause,
    Cancel,
}

#[derive(Debug)]
struct HostPinVerifier {
    pinned_sha256_by_host: Vec<SslException>,
    inner: Arc<dyn ServerCertVerifier>,
}

impl ServerCertVerifier for HostPinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
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
                    let host_only = exc
                        .host_port
                        .split(':')
                        .next()
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if host_only == sni && exc.sha256_fingerprint.eq_ignore_ascii_case(&fp) {
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
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

fn load_native_root_store() -> rustls::RootCertStore {
    let mut root_store = rustls::RootCertStore::empty();
    let res = rustls_native_certs::load_native_certs();
    for cert in res.certs {
        let _ = root_store.add(cert);
    }
    root_store
}

fn build_rustls_client_config(ssl_exceptions: &[SslException]) -> Option<rustls::ClientConfig> {
    ensure_crypto_provider();
    let root_store = load_native_root_store();
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
        .with_custom_certificate_verifier(Arc::new(HostPinVerifier {
            pinned_sha256_by_host: ssl_exceptions.to_vec(),
            inner: base_verifier,
        }))
        .with_no_client_auth();
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Some(cfg)
}

pub const DEFAULT_CLIENT_USER_AGENT: &str = concat!("FerriteDM/", env!("CARGO_PKG_VERSION"));

fn resolve_default_user_agent<'a>(
    config: &FdmConfig,
    user_agent_override: Option<&'a str>,
) -> &'a str {
    user_agent_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if config.core.use_system_default_user_agent {
                ferrite_core::os_integration::system_default_user_agent()
            } else {
                DEFAULT_CLIENT_USER_AGENT
            }
        })
}

fn build_http_client_inner(
    config: &FdmConfig,
    ssl_exceptions: &[SslException],
    user_agent_override: Option<&str>,
    follow_redirects: bool,
) -> Result<Client> {
    ensure_crypto_provider();
    let redirect_policy = if follow_redirects {
        reqwest::redirect::Policy::limited(10)
    } else {
        reqwest::redirect::Policy::none()
    };
    let mut default_headers = HeaderMap::new();
    default_headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
    default_headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));

    let mut builder = Client::builder()
        .redirect(redirect_policy)
        .referer(false)
        .default_headers(default_headers)
        .connect_timeout(std::time::Duration::from_secs(20));

    let ua = resolve_default_user_agent(config, user_agent_override);
    builder = builder.user_agent(ua);

    if !ssl_exceptions.is_empty()
        && let Some(tls_cfg) = build_rustls_client_config(ssl_exceptions)
    {
        builder = builder.use_preconfigured_tls(tls_cfg);
    }

    builder = apply_proxy_settings(builder, &config.app.proxy)?;
    Ok(builder.build()?)
}

pub fn build_http_client(
    config: &FdmConfig,
    ssl_exceptions: &[SslException],
    user_agent_override: Option<&str>,
) -> Result<Client> {
    build_http_client_inner(config, ssl_exceptions, user_agent_override, true)
}

fn apply_proxy_settings(
    mut builder: reqwest::ClientBuilder,
    proxy: &ProxySettings,
) -> Result<reqwest::ClientBuilder> {
    match proxy.mode {
        NetworkProxyMode::NoProxy => {
            builder = builder.no_proxy();
        }
        NetworkProxyMode::SystemProxy => {
            // reqwest enables system proxy detection by default
        }
        NetworkProxyMode::ManualProxy => {
            builder = builder.no_proxy();
            if !proxy.http.host.is_empty() && proxy.http.port > 0 {
                let mut p = Proxy::http(format!("http://{}:{}", proxy.http.host, proxy.http.port))?;
                if !proxy.http.username.is_empty() {
                    p = p.basic_auth(&proxy.http.username, &proxy.http.password);
                }
                builder = builder.proxy(p);
            }
            if !proxy.https.host.is_empty() && proxy.https.port > 0 {
                let mut p =
                    Proxy::https(format!("http://{}:{}", proxy.https.host, proxy.https.port))?;
                if !proxy.https.username.is_empty() {
                    p = p.basic_auth(&proxy.https.username, &proxy.https.password);
                }
                builder = builder.proxy(p);
            }
            if !proxy.socks5.host.is_empty() && proxy.socks5.port > 0 {
                let mut p = Proxy::all(format!(
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
    Ok(builder)
}

pub fn build_request_headers(record: &DownloadRecord, cred: Option<&AuthCredential>) -> HeaderMap {
    let mut map = HeaderMap::new();
    if let Some(ref r) = record.http.referrer
        && !r.trim().is_empty()
        && let Ok(v) = HeaderValue::from_str(r.trim())
    {
        map.insert(REFERER, v);
    }
    if let Some(ref c) = record.http.cookies
        && !c.trim().is_empty()
        && let Ok(v) = HeaderValue::from_str(c.trim())
    {
        map.insert(COOKIE, v);
    }
    if let Some(ref ua) = record.http.user_agent
        && !ua.trim().is_empty()
        && let Ok(v) = HeaderValue::from_str(ua.trim())
    {
        map.insert(USER_AGENT, v);
    }
    for (k, v) in &record.http.custom_headers {
        if let (Ok(hn), Ok(hv)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            map.insert(hn, hv);
        }
    }
    if let Some(c) = cred {
        let token = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", c.username, c.password));
        if let Ok(hv) = HeaderValue::from_str(&format!("Basic {token}")) {
            map.insert(AUTHORIZATION, hv);
        }
    }
    map
}

fn has_custom_header(headers: &std::collections::BTreeMap<String, String>, name: &str) -> bool {
    headers.keys().any(|k| k.eq_ignore_ascii_case(name))
}

fn ensure_identity_encoding(headers: &mut HeaderMap) {
    if !headers.contains_key(ACCEPT_ENCODING) {
        headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    }
}

fn origin_key(url: &Url) -> (String, Option<String>, Option<u16>) {
    (
        url.scheme().to_ascii_lowercase(),
        url.host_str().map(|h| h.to_ascii_lowercase()),
        url.port_or_known_default(),
    )
}

fn origin_referer(url_str: &str) -> Option<String> {
    let parsed = Url::parse(url_str).ok()?;
    let host = parsed.host_str()?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    match parsed.port() {
        Some(port) => Some(format!("{scheme}://{host}:{port}/")),
        None => Some(format!("{scheme}://{host}/")),
    }
}

#[derive(Debug, Clone)]
struct CookieEntry {
    domain: String,
    include_subdomains: bool,
    secure: bool,
    name: String,
    value: String,
}

#[derive(Debug, Default, Clone)]
struct RedirectCookieJar {
    entries: Arc<Mutex<Vec<CookieEntry>>>,
}

impl RedirectCookieJar {
    fn store_from_response(&self, url: &Url, headers: &HeaderMap) {
        let Some(req_host) = url.host_str().map(|h| h.to_ascii_lowercase()) else {
            return;
        };
        for val in headers.get_all(SET_COOKIE) {
            let Ok(raw) = val.to_str() else {
                continue;
            };
            let mut parts = raw.split(';');
            let Some(pair) = parts.next() else {
                continue;
            };
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            let name = name.trim();
            let value = value.trim();
            if name.is_empty() {
                continue;
            }
            let mut domain = req_host.clone();
            let mut include_subdomains = false;
            let mut secure = false;
            let mut expired = false;
            for attr in parts {
                let attr = attr.trim();
                if attr.eq_ignore_ascii_case("secure") {
                    secure = true;
                } else if let Some((k, v)) = attr.split_once('=') {
                    if k.trim().eq_ignore_ascii_case("domain") {
                        let d = v.trim().trim_start_matches('.').to_ascii_lowercase();
                        if !d.is_empty()
                            && (req_host == d
                                || (d.contains('.') && req_host.ends_with(&format!(".{d}"))))
                        {
                            domain = d;
                            include_subdomains = true;
                        }
                    } else if k.trim().eq_ignore_ascii_case("max-age")
                        && v.trim().parse::<i64>().is_ok_and(|n| n <= 0)
                    {
                        expired = true;
                    }
                }
            }
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            entries.retain(|e| !(e.domain == domain && e.name == name));
            if !expired {
                entries.push(CookieEntry {
                    domain,
                    include_subdomains,
                    secure,
                    name: name.to_string(),
                    value: value.to_string(),
                });
            }
        }
    }

    fn cookie_header_for(&self, url: &Url) -> Option<String> {
        let host = url.host_str()?.to_ascii_lowercase();
        let is_https = url.scheme().eq_ignore_ascii_case("https");
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let pairs: Vec<String> = entries
            .iter()
            .filter(|e| {
                (!e.secure || is_https)
                    && (host == e.domain
                        || (e.include_subdomains && host.ends_with(&format!(".{}", e.domain))))
            })
            .map(|e| format!("{}={}", e.name, e.value))
            .collect();
        if pairs.is_empty() {
            None
        } else {
            Some(pairs.join("; "))
        }
    }
}

async fn send_with_redirects(
    client: &Client,
    method: Method,
    url: &str,
    credential_origin_url: &str,
    base_headers: &HeaderMap,
    body: Option<String>,
    jar: Option<&RedirectCookieJar>,
) -> std::result::Result<(reqwest::Response, String), reqwest::Error> {
    let Some(jar) = jar else {
        let mut req_headers = base_headers.clone();
        if let (Ok(cred_u), Ok(cur_u)) = (Url::parse(credential_origin_url), Url::parse(url))
            && origin_key(&cred_u) != origin_key(&cur_u)
        {
            req_headers.remove(AUTHORIZATION);
            req_headers.remove(PROXY_AUTHORIZATION);
            req_headers.remove(COOKIE);
        }
        let mut req = client.request(method, url).headers(req_headers);
        if let Some(b) = body {
            req = req.body(b);
        }
        let resp = req.send().await?;
        let final_url = resp.url().to_string();
        return Ok((resp, final_url));
    };

    const MAX_REDIRECTS: usize = 10;
    let mut current_url = url.to_string();
    let mut current_method = method;
    let mut current_body = body;
    let initial_origin = Url::parse(credential_origin_url)
        .ok()
        .map(|u| origin_key(&u));

    let mut hop = 0usize;
    loop {
        let parsed_cur = Url::parse(&current_url).ok();
        let mut req_headers = base_headers.clone();
        let same_origin = match (&initial_origin, &parsed_cur) {
            (Some(init), Some(cur)) => *init == origin_key(cur),
            _ => true,
        };
        if !same_origin {
            req_headers.remove(AUTHORIZATION);
            req_headers.remove(PROXY_AUTHORIZATION);
            req_headers.remove(COOKIE);
        }
        if let Some(ref cur_u) = parsed_cur
            && let Some(jar_cookies) = jar.cookie_header_for(cur_u)
        {
            let combined = match req_headers.get(COOKIE).and_then(|v| v.to_str().ok()) {
                Some(existing) if !existing.trim().is_empty() => {
                    format!("{}; {jar_cookies}", existing.trim())
                }
                _ => jar_cookies,
            };
            if let Ok(hv) = HeaderValue::from_str(&combined) {
                req_headers.insert(COOKIE, hv);
            }
        }

        let mut req = client
            .request(current_method.clone(), &current_url)
            .headers(req_headers);
        if let Some(ref b) = current_body {
            req = req.body(b.clone());
        }
        let resp = req.send().await?;
        if let Some(ref cur_u) = parsed_cur {
            jar.store_from_response(cur_u, resp.headers());
        }

        let st = resp.status();
        if hop < MAX_REDIRECTS
            && matches!(
                st,
                StatusCode::MOVED_PERMANENTLY
                    | StatusCode::FOUND
                    | StatusCode::SEE_OTHER
                    | StatusCode::TEMPORARY_REDIRECT
                    | StatusCode::PERMANENT_REDIRECT
            )
            && let Some(loc_val) = resp.headers().get(LOCATION).and_then(|v| v.to_str().ok())
        {
            let next_url = parsed_cur
                .as_ref()
                .and_then(|base| base.join(loc_val).ok())
                .or_else(|| Url::parse(loc_val).ok());
            if let Some(next) = next_url
                && matches!(next.scheme(), "http" | "https")
            {
                if matches!(
                    st,
                    StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND | StatusCode::SEE_OTHER
                ) && current_method == Method::POST
                {
                    current_method = Method::GET;
                    current_body = None;
                }
                current_url = next.to_string();
                hop += 1;
                continue;
            }
        }

        return Ok((resp, current_url));
    }
}

static UTF8_EXT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)filename\*\s*=\s*UTF-8''([^;\s]+)"#).unwrap());
static B64_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)filename\s*=\s*"?=\?utf-8\?b\?([^;\s"?]+)\?="?"#).unwrap());
static QUOTED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)filename\s*=\s*"([^"]+)""#).unwrap());
static BARE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)filename\s*=\s*([^;\s]+)"#).unwrap());

pub fn parse_content_disposition_filename(header_val: &str) -> Option<String> {
    // Matches FDM's filename*=UTF-8'', RFC 2047 =?utf-8?b?...?=, quoted filename="..." and bare filename=...
    if let Some(caps) = UTF8_EXT_RE.captures(header_val) {
        let decoded = urlencoding_decode(&caps[1]);
        if !decoded.is_empty() {
            return Some(sanitize_filename(&decoded));
        }
    }
    if let Some(caps) = B64_RE.captures(header_val)
        && let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(&caps[1])
    {
        let s = String::from_utf8_lossy(&raw).into_owned();
        if !s.is_empty() {
            return Some(sanitize_filename(&s));
        }
    }
    if let Some(caps) = QUOTED_RE.captures(header_val) {
        return Some(sanitize_filename(&caps[1]));
    }
    if let Some(caps) = BARE_RE.captures(header_val) {
        return Some(sanitize_filename(&caps[1]));
    }
    None
}

fn parse_content_range_total(val: &str) -> Option<u64> {
    // bytes 0-0/12345
    val.rsplit('/').next().and_then(|s| {
        let t = s.trim();
        if t == "*" {
            None
        } else {
            t.parse::<u64>().ok()
        }
    })
}

async fn probe_http_resource_inner(
    client: &Client,
    record: &DownloadRecord,
    ssl_exceptions: &[SslException],
    cred: Option<&AuthCredential>,
    jar: Option<&RedirectCookieJar>,
) -> Result<(ProbeResult, Option<&'static str>)> {
    if record.url.starts_with("ftp://") || record.url.starts_with("ftps://") {
        let p = probe_ftp_resource(&record.url, ssl_exceptions, cred).await?;
        return Ok((p, None));
    }

    let mut headers = build_request_headers(record, cred);
    ensure_identity_encoding(&mut headers);

    let head_resp = send_with_redirects(
        client,
        Method::HEAD,
        &record.url,
        &record.url,
        &headers,
        None,
        jar,
    )
    .await;

    let mut final_url = record.url.clone();
    let mut total_bytes = None;
    let mut accept_ranges = false;
    let mut etag = None;
    let mut last_modified = None;
    let mut content_type = None;
    let mut suggested_filename = None;
    let mut head_status = None;
    let mut winning_ua: Option<&'static str> = None;

    if let Ok((resp, resolved_url)) = head_resp {
        head_status = Some(resp.status());
        if resp.status().is_success() {
            final_url = resolved_url;
            let h = resp.headers();
            total_bytes = h
                .get(reqwest::header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            accept_ranges = h
                .get(reqwest::header::ACCEPT_RANGES)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_ascii_lowercase().contains("bytes"))
                .unwrap_or(false);
            etag = h
                .get(reqwest::header::ETAG)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            last_modified = h
                .get(reqwest::header::LAST_MODIFIED)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            content_type = h
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            suggested_filename = h
                .get(reqwest::header::CONTENT_DISPOSITION)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_disposition_filename);
        }
    }

    let mut range_probe_status = None;
    // Fallback or verify byte-range capability via GET Range: bytes=0-0
    if !accept_ranges || total_bytes.is_none() {
        let mut h2 = headers.clone();
        h2.insert(RANGE, HeaderValue::from_static("bytes=0-0"));
        if let Ok((resp, resolved_url)) = send_with_redirects(
            client,
            Method::GET,
            &record.url,
            &record.url,
            &h2,
            None,
            jar,
        )
        .await
        {
            let st = resp.status();
            range_probe_status = Some(st);
            if st == StatusCode::PARTIAL_CONTENT || st.is_success() {
                final_url = resolved_url;
                let h = resp.headers();
                if etag.is_none() {
                    etag = h
                        .get(reqwest::header::ETAG)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                }
                if last_modified.is_none() {
                    last_modified = h
                        .get(reqwest::header::LAST_MODIFIED)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                }
                if content_type.is_none() {
                    content_type = h
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                }
                if suggested_filename.is_none() {
                    suggested_filename = h
                        .get(reqwest::header::CONTENT_DISPOSITION)
                        .and_then(|v| v.to_str().ok())
                        .and_then(parse_content_disposition_filename);
                }
                if st == StatusCode::PARTIAL_CONTENT {
                    accept_ranges = true;
                    if let Some(cr) = h
                        .get(reqwest::header::CONTENT_RANGE)
                        .and_then(|v| v.to_str().ok())
                        && let Some(tot) = parse_content_range_total(cr)
                    {
                        total_bytes = Some(tot);
                    }
                } else {
                    accept_ranges = false;
                    if total_bytes.is_none() {
                        total_bytes = h
                            .get(reqwest::header::CONTENT_LENGTH)
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok());
                    }
                }
            }
        }
    }

    // If both HEAD and ranged GET received 403 and no explicit User-Agent was pinned,
    // probe once with alternate User-Agent profiles (browser UA and non-browser client UA).
    if head_status == Some(StatusCode::FORBIDDEN)
        && range_probe_status == Some(StatusCode::FORBIDDEN)
        && record.http.user_agent.is_none()
        && !has_custom_header(&record.http.custom_headers, "user-agent")
    {
        for fallback_ua in [
            ferrite_core::os_integration::system_default_user_agent(),
            DEFAULT_CLIENT_USER_AGENT,
        ] {
            let mut h_alt = headers.clone();
            if let Ok(hv) = HeaderValue::from_str(fallback_ua) {
                h_alt.insert(USER_AGENT, hv);
            }
            h_alt.insert(RANGE, HeaderValue::from_static("bytes=0-0"));
            if let Ok((resp, resolved_url)) = send_with_redirects(
                client,
                Method::GET,
                &record.url,
                &record.url,
                &h_alt,
                None,
                jar,
            )
            .await
            {
                let st = resp.status();
                if st == StatusCode::PARTIAL_CONTENT || st.is_success() {
                    winning_ua = Some(fallback_ua);
                    final_url = resolved_url;
                    let h = resp.headers();
                    etag = h
                        .get(reqwest::header::ETAG)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    last_modified = h
                        .get(reqwest::header::LAST_MODIFIED)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    content_type = h
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    suggested_filename = h
                        .get(reqwest::header::CONTENT_DISPOSITION)
                        .and_then(|v| v.to_str().ok())
                        .and_then(parse_content_disposition_filename);
                    if st == StatusCode::PARTIAL_CONTENT {
                        accept_ranges = true;
                        total_bytes = h
                            .get(reqwest::header::CONTENT_RANGE)
                            .and_then(|v| v.to_str().ok())
                            .and_then(parse_content_range_total);
                    } else {
                        accept_ranges = false;
                        total_bytes = h
                            .get(reqwest::header::CONTENT_LENGTH)
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok());
                    }
                    break;
                }
            }
        }
    }

    Ok((
        ProbeResult {
            final_url,
            total_bytes,
            resume_support: if accept_ranges {
                ResumeSupport::Present
            } else {
                ResumeSupport::Absent
            },
            etag,
            last_modified,
            content_type,
            suggested_filename,
            supports_range_split: accept_ranges && total_bytes.unwrap_or(0) >= 256 * 1024,
        },
        winning_ua,
    ))
}

pub async fn probe_http_resource(
    client: &Client,
    record: &DownloadRecord,
    ssl_exceptions: &[SslException],
    cred: Option<&AuthCredential>,
) -> Result<ProbeResult> {
    probe_http_resource_inner(client, record, ssl_exceptions, cred, None)
        .await
        .map(|(p, _)| p)
}

pub fn build_initial_segments(
    total_bytes: u64,
    sections: u32,
    mirrors: &[String],
) -> Vec<SegmentProgress> {
    let n = sections.clamp(1, 32);
    let chunk = total_bytes / n as u64;
    let mut out = Vec::with_capacity(n as usize);
    let mut offset = 0u64;
    for i in 0..n {
        let end = if i == n - 1 {
            total_bytes.saturating_sub(1)
        } else {
            (offset + chunk).saturating_sub(1)
        };
        let mirror_url = if mirrors.is_empty() {
            None
        } else {
            Some(mirrors[i as usize % mirrors.len()].clone())
        };
        out.push(SegmentProgress {
            id: i,
            start: offset,
            end,
            downloaded: 0,
            state: SegmentState::Pending,
            speed_bps: 0,
            mirror_url,
        });
        offset = end + 1;
    }
    out
}

pub async fn download_http_or_ftp<F>(
    mut record: DownloadRecord,
    config: &FdmConfig,
    ssl_exceptions: &[SslException],
    cred: Option<AuthCredential>,
    limiter: BandwidthLimiter,
    mut control_rx: watch::Receiver<ControlCommand>,
    mut on_progress: F,
) -> DownloadRecord
where
    F: FnMut(&DownloadRecord) + Send,
{
    if record.url.starts_with("ftp://") || record.url.starts_with("ftps://") {
        return download_ftp(
            record,
            config,
            ssl_exceptions,
            cred,
            limiter,
            control_rx,
            on_progress,
        )
        .await;
    }

    let mut active_ua =
        resolve_default_user_agent(config, record.http.user_agent.as_deref()).to_string();
    let mut client = match build_http_client_inner(config, ssl_exceptions, Some(&active_ua), false)
    {
        Ok(c) => c,
        Err(e) => {
            record.state = DownloadState::Error;
            record.last_error = Some(e.to_string());
            record.abort_reason = DownloadAbortReason::NetworkError;
            return record;
        }
    };
    let cookie_jar = RedirectCookieJar::default();

    record.state = DownloadState::Starting;
    on_progress(&record);

    // Retry wrapper honoring auto_retry_failed + disable_auto_retry + max_auto_retries
    let max_attempts = if config.core.auto_retry_failed && !record.flags.disable_auto_retry {
        config.core.max_auto_retries.max(1) + 1
    } else {
        1
    };

    for attempt in 0..max_attempts {
        if matches!(*control_rx.borrow(), ControlCommand::Pause) {
            record.state = DownloadState::Paused;
            return record;
        }
        if matches!(*control_rx.borrow(), ControlCommand::Cancel) {
            record.state = DownloadState::Stopped;
            record.abort_reason = DownloadAbortReason::AbortedByUser;
            return record;
        }

        let probe = probe_http_resource_inner(
            &client,
            &record,
            ssl_exceptions,
            cred.as_ref(),
            Some(&cookie_jar),
        )
        .await;
        let (probe, winning_ua) = match probe {
            Ok(p) => p,
            Err(e) => {
                record.last_error = Some(e.to_string());
                if attempt + 1 < max_attempts {
                    tokio::time::sleep(std::time::Duration::from_secs(1 + attempt as u64)).await;
                    continue;
                }
                record.state = DownloadState::Error;
                record.abort_reason = DownloadAbortReason::NetworkError;
                return record;
            }
        };
        if let Some(ua) = winning_ua
            && ua != active_ua
            && let Ok(switched) = build_http_client_inner(config, ssl_exceptions, Some(ua), false)
        {
            client = switched;
            active_ua = ua.to_string();
        }

        // Detect remote resource change if previously probed
        let remote_changed = record
            .etag
            .as_deref()
            .zip(probe.etag.as_deref())
            .map(|(old, new)| old != new)
            .unwrap_or(false)
            || record
                .total_bytes
                .zip(probe.total_bytes)
                .map(|(old, new)| old != new)
                .unwrap_or(false);
        if remote_changed && (!record.segments.is_empty() || record.downloaded_bytes > 0) {
            if config.core.auto_restart_if_remote_changed {
                record.segments.clear();
                record.downloaded_bytes = 0;
                let _ = std::fs::remove_file(record.partial_path());
            } else {
                record.state = DownloadState::Error;
                record.abort_reason = DownloadAbortReason::RemoteResourceChanged;
                record.last_error = Some("Remote resource changed on server".into());
                record.connections_active = 0;
                record.download_speed_bps = 0;
                on_progress(&record);
                return record;
            }
        }

        if let Some(ref name) = probe.suggested_filename
            && (record.file_name.is_empty() || record.file_name == "download.bin")
        {
            record.file_name = name.clone();
            record.title = name.clone();
        }
        record.file_type = classify_file_type(&record.file_name, probe.content_type.as_deref());
        record.total_bytes = probe.total_bytes;
        record.resume_support = probe.resume_support;
        record.etag = probe.etag.clone();
        record.last_modified = probe.last_modified.clone();

        if let Err(e) = std::fs::create_dir_all(&record.target_dir) {
            record.state = DownloadState::Error;
            record.last_error = Some(format!("Create dir failed: {e}"));
            return record;
        }

        let partial_path = record.partial_path();
        let profile = config.active_profile();

        let mut used_range_in_single = false;
        let mut result = if probe.supports_range_split
            && record.http.post_data.is_none()
            && profile.max_connections_per_server > 1
        {
            let multi_res = download_multi_section(
                &client,
                &mut record,
                &partial_path,
                &probe,
                profile.max_connections_per_server,
                cred.as_ref(),
                config,
                &limiter,
                &cookie_jar,
                &mut control_rx,
                &mut on_progress,
            )
            .await;
            match multi_res {
                Err(ref err) if err.status == Some(StatusCode::FORBIDDEN) || err.range_rejected => {
                    // Multi-section range burst was forbidden or range ignored; fall back to plain single-stream GET.
                    record.segments.clear();
                    record.downloaded_bytes = 0;
                    let _ = std::fs::remove_file(&partial_path);
                    download_single_stream(
                        &client,
                        &mut record,
                        &partial_path,
                        &probe,
                        cred.as_ref(),
                        config,
                        &limiter,
                        &cookie_jar,
                        false,
                        None,
                        &mut control_rx,
                        &mut on_progress,
                    )
                    .await
                }
                other => other,
            }
        } else {
            used_range_in_single =
                probe.resume_support == ResumeSupport::Present && partial_path.exists();
            download_single_stream(
                &client,
                &mut record,
                &partial_path,
                &probe,
                cred.as_ref(),
                config,
                &limiter,
                &cookie_jar,
                true,
                None,
                &mut control_rx,
                &mut on_progress,
            )
            .await
        };

        if let Err(err_403) = result {
            if err_403.status == Some(StatusCode::FORBIDDEN) {
                result = try_download_403_fallbacks(
                    &client,
                    &active_ua,
                    used_range_in_single,
                    &mut record,
                    &partial_path,
                    &probe,
                    cred.as_ref(),
                    config,
                    ssl_exceptions,
                    &limiter,
                    &cookie_jar,
                    &mut control_rx,
                    &mut on_progress,
                    err_403,
                )
                .await;
            } else {
                result = Err(err_403);
            }
        }

        match result {
            Ok(Outcome::Completed) => {
                let output_path = record.output_path();
                if partial_path != output_path {
                    if output_path.exists() {
                        let _ = std::fs::remove_file(&output_path);
                    }
                    if let Err(e) = std::fs::rename(&partial_path, &output_path) {
                        record.state = DownloadState::Error;
                        record.last_error = Some(format!("Finalize rename failed: {e}"));
                        return record;
                    }
                }
                if config.core.take_last_modified_from_server
                    && let Some(ref lm) = record.last_modified
                    && let Ok(st) = httpdate::parse_http_date(lm)
                {
                    let ft = filetime::FileTime::from_system_time(st);
                    let _ = filetime::set_file_mtime(&output_path, ft);
                }
                if config.core.set_mark_of_the_web {
                    let _ = set_mark_of_the_web(&output_path, &record.url, "FerriteDM");
                }
                record.state = DownloadState::Completed;
                record.download_speed_bps = 0;
                record.connections_active = 0;
                record.eta_secs = Some(0);
                record.completed_at = Some(chrono::Utc::now());
                record.updated_at = chrono::Utc::now();
                let algo_val = sha256_str(&record.url);
                record.files = vec![DownloadFileEntry {
                    index: 0,
                    relative_path: record.file_name.clone(),
                    size_bytes: record.total_bytes.unwrap_or(record.downloaded_bytes),
                    downloaded_bytes: record.downloaded_bytes,
                    priority: DownloadPriority::Normal,
                    media_ready_to_play: true,
                    expected_hash: None,
                    computed_hash: None,
                }];
                let _ = algo_val;
                on_progress(&record);
                return record;
            }
            Ok(Outcome::Paused) => {
                record.state = DownloadState::Paused;
                record.download_speed_bps = 0;
                record.connections_active = 0;
                on_progress(&record);
                return record;
            }
            Ok(Outcome::Cancelled) => {
                record.state = DownloadState::Stopped;
                record.download_speed_bps = 0;
                record.connections_active = 0;
                record.abort_reason = DownloadAbortReason::AbortedByUser;
                on_progress(&record);
                return record;
            }
            Err(err_info) => {
                record.retry_count += 1;
                record.last_error = Some(err_info.message.clone());
                record.abort_reason = err_info.reason;
                record.connections_active = 0;
                record.download_speed_bps = 0;
                if err_info.unwanted || attempt + 1 >= max_attempts {
                    record.state = DownloadState::Error;
                    on_progress(&record);
                    return record;
                }
                tokio::time::sleep(std::time::Duration::from_secs(1 + attempt as u64)).await;
            }
        }
    }

    record
}

enum Outcome {
    Completed,
    Paused,
    Cancelled,
}

struct DownloadFailure {
    message: String,
    reason: DownloadAbortReason,
    unwanted: bool,
    status: Option<StatusCode>,
    range_rejected: bool,
}

impl DownloadFailure {
    fn network(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            reason: DownloadAbortReason::NetworkError,
            unwanted: false,
            status: None,
            range_rejected: false,
        }
    }

    fn slow(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            reason: DownloadAbortReason::SlowDownload,
            unwanted: false,
            status: None,
            range_rejected: false,
        }
    }

    fn http_status(st: StatusCode, message: impl Into<String>, detect_unwanted: bool) -> Self {
        let unwanted = detect_unwanted
            && matches!(
                st,
                StatusCode::UNAUTHORIZED
                    | StatusCode::FORBIDDEN
                    | StatusCode::NOT_FOUND
                    | StatusCode::GONE
                    | StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS
            );
        Self {
            message: message.into(),
            reason: if unwanted {
                DownloadAbortReason::UnwantedError
            } else {
                DownloadAbortReason::NetworkError
            },
            unwanted,
            status: Some(st),
            range_rejected: false,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn try_download_403_fallbacks<F>(
    initial_client: &Client,
    initial_ua: &str,
    used_range_in_single: bool,
    record: &mut DownloadRecord,
    partial_path: &Path,
    probe: &ProbeResult,
    cred: Option<&AuthCredential>,
    config: &FdmConfig,
    ssl_exceptions: &[SslException],
    limiter: &BandwidthLimiter,
    cookie_jar: &RedirectCookieJar,
    control_rx: &mut watch::Receiver<ControlCommand>,
    on_progress: &mut F,
    mut last_err: DownloadFailure,
) -> std::result::Result<Outcome, DownloadFailure>
where
    F: FnMut(&DownloadRecord) + Send,
{
    // 1. If the failed single-stream attempt sent a resume Range header, retry plain GET without Range.
    if used_range_in_single {
        if matches!(
            *control_rx.borrow(),
            ControlCommand::Pause | ControlCommand::Cancel
        ) {
            return Err(last_err);
        }
        match download_single_stream(
            initial_client,
            record,
            partial_path,
            probe,
            cred,
            config,
            limiter,
            cookie_jar,
            false,
            None,
            control_rx,
            on_progress,
        )
        .await
        {
            Ok(o) => return Ok(o),
            Err(e) if e.status != Some(StatusCode::FORBIDDEN) => return Err(e),
            Err(e) => last_err = e,
        }
    }

    let user_pinned_ua = record
        .http
        .user_agent
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
        || has_custom_header(&record.http.custom_headers, "user-agent");

    // 2. If User-Agent was not explicitly pinned, try the complementary User-Agent profile
    //    (browser UA <-> non-browser client UA).
    if !user_pinned_ua {
        if matches!(
            *control_rx.borrow(),
            ControlCommand::Pause | ControlCommand::Cancel
        ) {
            return Err(last_err);
        }
        let sys_ua = ferrite_core::os_integration::system_default_user_agent();
        let alt_ua = if initial_ua == sys_ua {
            DEFAULT_CLIENT_USER_AGENT
        } else {
            sys_ua
        };
        if let Ok(alt_client) = build_http_client_inner(config, ssl_exceptions, Some(alt_ua), false)
        {
            match download_single_stream(
                &alt_client,
                record,
                partial_path,
                probe,
                cred,
                config,
                limiter,
                cookie_jar,
                false,
                None,
                control_rx,
                on_progress,
            )
            .await
            {
                Ok(o) => return Ok(o),
                Err(e) if e.status != Some(StatusCode::FORBIDDEN) => return Err(e),
                Err(e) => last_err = e,
            }
        }
    }

    // 3. Hotlink-protected servers often require a same-origin Referer header.
    let user_pinned_referer = record
        .http
        .referrer
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
        || has_custom_header(&record.http.custom_headers, "referer");
    if !user_pinned_referer && let Some(ref_origin) = origin_referer(&record.url) {
        if matches!(
            *control_rx.borrow(),
            ControlCommand::Pause | ControlCommand::Cancel
        ) {
            return Err(last_err);
        }
        let ref_client = if user_pinned_ua {
            initial_client.clone()
        } else {
            build_http_client_inner(
                config,
                ssl_exceptions,
                Some(ferrite_core::os_integration::system_default_user_agent()),
                false,
            )
            .unwrap_or_else(|_| initial_client.clone())
        };
        match download_single_stream(
            &ref_client,
            record,
            partial_path,
            probe,
            cred,
            config,
            limiter,
            cookie_jar,
            false,
            Some(&ref_origin),
            control_rx,
            on_progress,
        )
        .await
        {
            Ok(o) => return Ok(o),
            Err(e) => return Err(e),
        }
    }

    Err(last_err)
}

#[allow(clippy::too_many_arguments)]
async fn download_multi_section<F>(
    client: &Client,
    record: &mut DownloadRecord,
    partial_path: &Path,
    probe: &ProbeResult,
    sections_limit: u32,
    cred: Option<&AuthCredential>,
    config: &FdmConfig,
    limiter: &BandwidthLimiter,
    cookie_jar: &RedirectCookieJar,
    control_rx: &mut watch::Receiver<ControlCommand>,
    on_progress: &mut F,
) -> std::result::Result<Outcome, DownloadFailure>
where
    F: FnMut(&DownloadRecord) + Send,
{
    let total = record.total_bytes.unwrap_or(0);
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .truncate(false)
        .open(partial_path)
        .map_err(|e| DownloadFailure::network(format!("Open partial file failed: {e}")))?;
    file.set_len(total).map_err(|e| {
        DownloadFailure::network(format!("Preallocate {} bytes failed: {e}", total))
    })?;
    let file = Arc::new(file);

    let primary_url = if !probe.final_url.is_empty() {
        probe.final_url.clone()
    } else {
        record.url.clone()
    };
    let original_url = record.url.clone();
    let mut urls_pool = vec![primary_url];
    urls_pool.extend(record.http.mirrors.clone());

    if record.segments.is_empty()
        || record
            .segments
            .last()
            .map(|s| s.end + 1 != total)
            .unwrap_or(true)
    {
        record.segments = build_initial_segments(total, sections_limit, &urls_pool);
    }
    let already_done: u64 = record.segments.iter().map(|s| s.downloaded).sum();
    record.downloaded_bytes = already_done;
    record.state = DownloadState::Downloading;

    let shared_segments = Arc::new(Mutex::new(record.segments.clone()));
    let total_downloaded = Arc::new(AtomicU64::new(already_done));
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let pause_flag = Arc::new(AtomicBool::new(false));
    let failed_err: Arc<Mutex<Option<DownloadFailure>>> = Arc::new(Mutex::new(None));

    let mut base_headers = build_request_headers(record, cred);
    ensure_identity_encoding(&mut base_headers);
    let urls_pool = Arc::new(urls_pool);
    let original_url = Arc::new(original_url);

    let mut handles = Vec::new();
    let num_workers = record.segments.len();

    for idx in 0..num_workers {
        let client = client.clone();
        let file = Arc::clone(&file);
        let shared_segments = Arc::clone(&shared_segments);
        let total_downloaded = Arc::clone(&total_downloaded);
        let cancel_flag = Arc::clone(&cancel_flag);
        let pause_flag = Arc::clone(&pause_flag);
        let failed_err = Arc::clone(&failed_err);
        let headers = base_headers.clone();
        let limiter = limiter.clone();
        let cookie_jar = cookie_jar.clone();
        let urls_pool = Arc::clone(&urls_pool);
        let original_url = Arc::clone(&original_url);
        let detect_unwanted = config.core.detect_unwanted_errors;

        handles.push(tokio::spawn(async move {
            loop {
                if cancel_flag.load(Ordering::Relaxed) || pause_flag.load(Ordering::Relaxed) {
                    break;
                }
                // Pick this worker's segment first, else steal half of largest pending/slow remainder!
                let job = {
                    let mut segs = shared_segments.lock().unwrap();
                    let mine = &mut segs[idx];
                    if mine.start + mine.downloaded <= mine.end {
                        mine.state = SegmentState::Active;
                        let start_at = mine.start + mine.downloaded;
                        let end_at = mine.end;
                        let u = mine
                            .mirror_url
                            .clone()
                            .unwrap_or_else(|| urls_pool[0].clone());
                        Some((idx, start_at, end_at, u))
                    } else {
                        // Work-stealing: find segment with > 512 KiB left and split its tail!
                        let mut best: Option<(usize, u64)> = None;
                        for (j, s) in segs.iter().enumerate() {
                            let cur = s.start + s.downloaded;
                            if cur <= s.end {
                                let rem = s.end - cur + 1;
                                if rem >= 512 * 1024 && best.map(|b| rem > b.1).unwrap_or(true) {
                                    best = Some((j, rem));
                                }
                            }
                        }
                        if let Some((victim_idx, rem)) = best {
                            let victim_end = segs[victim_idx].end;
                            let cur = segs[victim_idx].start + segs[victim_idx].downloaded;
                            let split_point = cur + (rem / 2);
                            segs[victim_idx].end = split_point - 1;
                            let stolen_id = segs.len() as u32;
                            let u = urls_pool[stolen_id as usize % urls_pool.len()].clone();
                            segs.push(SegmentProgress {
                                id: stolen_id,
                                start: split_point,
                                end: victim_end,
                                downloaded: 0,
                                state: SegmentState::Active,
                                speed_bps: 0,
                                mirror_url: Some(u.clone()),
                            });
                            Some((segs.len() - 1, split_point, victim_end, u))
                        } else {
                            None
                        }
                    }
                };

                let Some((seg_idx, range_start, _orig_end, target_url)) = job else {
                    break;
                };

                // Execute HTTP Range GET (fallback to primary URL and original URL if mirror/redirect fails)
                let mut try_urls = vec![target_url];
                if try_urls[0] != urls_pool[0] {
                    try_urls.push(urls_pool[0].clone());
                }
                if !try_urls.iter().any(|u| u == original_url.as_str()) {
                    try_urls.push((*original_url).clone());
                }

                let mut seg_ok = false;
                let last_idx = try_urls.len().saturating_sub(1);
                for (attempt_i, try_url) in try_urls.into_iter().enumerate() {
                    let is_last_url = attempt_i == last_idx;
                    let current_end = {
                        let segs = shared_segments.lock().unwrap();
                        segs[seg_idx].end
                    };
                    if range_start > current_end {
                        seg_ok = true;
                        break;
                    }
                    let mut h = headers.clone();
                    if let Ok(hv) =
                        HeaderValue::from_str(&format!("bytes={range_start}-{current_end}"))
                    {
                        h.insert(RANGE, hv);
                    }
                    let resp = match send_with_redirects(
                        &client,
                        Method::GET,
                        &try_url,
                        &original_url,
                        &h,
                        None,
                        Some(&cookie_jar),
                    )
                    .await
                    {
                        Ok((r, _)) => r,
                        Err(e) => {
                            if is_last_url {
                                let mut g = failed_err.lock().unwrap();
                                if g.is_none() {
                                    *g = Some(DownloadFailure::network(format!(
                                        "Section {seg_idx} request error: {e}"
                                    )));
                                }
                            }
                            continue;
                        }
                    };

                    let st = resp.status();
                    if range_start > 0 && st == StatusCode::OK {
                        let mut g = failed_err.lock().unwrap();
                        if g.is_none() {
                            *g = Some(DownloadFailure {
                                message: format!(
                                    "Server ignored Range header on section {seg_idx} (HTTP 200)"
                                ),
                                reason: DownloadAbortReason::NetworkError,
                                unwanted: false,
                                status: Some(st),
                                range_rejected: true,
                            });
                        }
                        cancel_flag.store(true, Ordering::Relaxed);
                        break;
                    }

                    if !(st == StatusCode::PARTIAL_CONTENT || st == StatusCode::OK) {
                        let fail = DownloadFailure::http_status(
                            st,
                            format!("HTTP status {} on section {}", st, seg_idx),
                            detect_unwanted,
                        );
                        let is_unwanted = fail.unwanted;
                        if is_last_url {
                            let mut g = failed_err.lock().unwrap();
                            if g.is_none() {
                                *g = Some(fail);
                            }
                            if is_unwanted {
                                cancel_flag.store(true, Ordering::Relaxed);
                                break;
                            }
                        }
                        continue;
                    }

                    let mut stream = resp.bytes_stream();
                    let mut offset = range_start;
                    let mut chunk_err = false;
                    while let Some(item) = stream.next().await {
                        if cancel_flag.load(Ordering::Relaxed) || pause_flag.load(Ordering::Relaxed)
                        {
                            break;
                        }
                        let bytes = match item {
                            Ok(b) => b,
                            Err(e) => {
                                let mut g = failed_err.lock().unwrap();
                                if g.is_none() {
                                    *g = Some(DownloadFailure::network(format!(
                                        "Stream read error: {e}"
                                    )));
                                }
                                chunk_err = true;
                                break;
                            }
                        };
                        let cur_end = {
                            let segs = shared_segments.lock().unwrap();
                            segs[seg_idx].end
                        };
                        if offset > cur_end {
                            break;
                        }
                        let allowed_len = ((cur_end - offset + 1) as usize).min(bytes.len());
                        if allowed_len == 0 {
                            break;
                        }
                        limiter.acquire(allowed_len).await;
                        let slice = &bytes[..allowed_len];
                        if let Err(e) = write_at_exact(&file, slice, offset) {
                            let mut g = failed_err.lock().unwrap();
                            if g.is_none() {
                                *g = Some(DownloadFailure::network(format!(
                                    "Disk write failed: {e}"
                                )));
                            }
                            cancel_flag.store(true, Ordering::Relaxed);
                            chunk_err = true;
                            break;
                        }
                        offset += allowed_len as u64;
                        total_downloaded.fetch_add(allowed_len as u64, Ordering::Relaxed);
                        {
                            let mut segs = shared_segments.lock().unwrap();
                            let s = &mut segs[seg_idx];
                            s.downloaded = offset.saturating_sub(s.start);
                            if offset > s.end {
                                s.state = SegmentState::Completed;
                            }
                        }
                        if offset > cur_end {
                            break;
                        }
                    }

                    if !chunk_err {
                        seg_ok = true;
                        break;
                    }
                }

                if !seg_ok {
                    let mut segs = shared_segments.lock().unwrap();
                    if segs[seg_idx].start + segs[seg_idx].downloaded <= segs[seg_idx].end {
                        segs[seg_idx].state = SegmentState::Failed;
                    }
                    break;
                }
            }
        }));
    }

    // Monitor loop: compute speeds, enforce slow-download watchdog, propagate pause/cancel
    let mut last_bytes = already_done;
    let mut last_tick = tokio::time::Instant::now();
    let mut slow_accum_secs = 0f64;

    loop {
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
            changed = control_rx.changed() => {
                if changed.is_ok() {
                    match *control_rx.borrow() {
                        ControlCommand::Pause => {
                            pause_flag.store(true, Ordering::Relaxed);
                        }
                        ControlCommand::Cancel => {
                            cancel_flag.store(true, Ordering::Relaxed);
                        }
                        ControlCommand::Run => {}
                    }
                }
            }
        }

        let now = tokio::time::Instant::now();
        let dt = now.duration_since(last_tick).as_secs_f64().max(0.05);
        let cur = total_downloaded.load(Ordering::Relaxed);
        let delta = cur.saturating_sub(last_bytes);
        let speed = (delta as f64 / dt) as u64;
        last_bytes = cur;
        last_tick = now;

        {
            let segs = shared_segments.lock().unwrap();
            record.segments = segs.clone();
        }
        record.downloaded_bytes = cur.min(total);
        record.download_speed_bps = speed;
        record.connections_active = record
            .segments
            .iter()
            .filter(|s| s.state == SegmentState::Active)
            .count() as u32;
        record.eta_secs = total.saturating_sub(cur).checked_div(speed);
        record.updated_at = chrono::Utc::now();
        on_progress(record);

        if config.core.detect_unacceptably_slow && !pause_flag.load(Ordering::Relaxed) {
            if speed < config.core.unacceptably_slow_speed_bps && cur < total {
                slow_accum_secs += dt;
                if slow_accum_secs >= config.core.unacceptably_slow_interval_secs as f64 {
                    cancel_flag.store(true, Ordering::Relaxed);
                    for h in handles {
                        let _ = h.await;
                    }
                    return Err(DownloadFailure::slow(format!(
                        "Speed dropped below {} B/s for {}s",
                        config.core.unacceptably_slow_speed_bps,
                        config.core.unacceptably_slow_interval_secs
                    )));
                }
            } else {
                slow_accum_secs = 0.0;
            }
        }

        if handles.iter().all(|h| h.is_finished()) {
            break;
        }
        if pause_flag.load(Ordering::Relaxed) || cancel_flag.load(Ordering::Relaxed) {
            for h in handles {
                let _ = h.await;
            }
            break;
        }
    }

    {
        let segs = shared_segments.lock().unwrap();
        record.segments = segs.clone();
    }
    let _ = file.sync_all();

    if matches!(*control_rx.borrow(), ControlCommand::Cancel) {
        return Ok(Outcome::Cancelled);
    }
    if matches!(*control_rx.borrow(), ControlCommand::Pause) {
        return Ok(Outcome::Paused);
    }
    if let Some(err) = failed_err.lock().unwrap().take() {
        return Err(err);
    }
    if total_downloaded.load(Ordering::Relaxed) >= total {
        record.downloaded_bytes = total;
        Ok(Outcome::Completed)
    } else {
        Err(DownloadFailure::network("Download ended incomplete"))
    }
}

#[allow(clippy::too_many_arguments)]
async fn download_single_stream<F>(
    client: &Client,
    record: &mut DownloadRecord,
    partial_path: &Path,
    probe: &ProbeResult,
    cred: Option<&AuthCredential>,
    config: &FdmConfig,
    limiter: &BandwidthLimiter,
    cookie_jar: &RedirectCookieJar,
    allow_range: bool,
    extra_referer: Option<&str>,
    control_rx: &mut watch::Receiver<ControlCommand>,
    on_progress: &mut F,
) -> std::result::Result<Outcome, DownloadFailure>
where
    F: FnMut(&DownloadRecord) + Send,
{
    let existing_len =
        if allow_range && probe.resume_support == ResumeSupport::Present && partial_path.exists() {
            std::fs::metadata(partial_path)
                .map(|m| m.len())
                .unwrap_or(0)
        } else {
            0
        };

    let mut headers = build_request_headers(record, cred);
    ensure_identity_encoding(&mut headers);
    if let Some(referer_val) = extra_referer
        && !headers.contains_key(REFERER)
        && let Ok(hv) = HeaderValue::from_str(referer_val)
    {
        headers.insert(REFERER, hv);
    }
    if existing_len > 0
        && let Ok(hv) = HeaderValue::from_str(&format!("bytes={existing_len}-"))
    {
        headers.insert(RANGE, hv);
    }

    let (method, body) = if let Some(ref b) = record.http.post_data {
        (Method::POST, Some(b.clone()))
    } else {
        (Method::GET, None)
    };

    let (resp, _) = send_with_redirects(
        client,
        method,
        &record.url,
        &record.url,
        &headers,
        body,
        Some(cookie_jar),
    )
    .await
    .map_err(|e| DownloadFailure::network(format!("HTTP request failed: {e}")))?;

    let st = resp.status();
    if !st.is_success() && st != StatusCode::PARTIAL_CONTENT {
        return Err(DownloadFailure::http_status(
            st,
            format!("HTTP error status {}", st),
            config.core.detect_unwanted_errors,
        ));
    }

    // Populate metadata from GET response if probe was blocked (e.g. HEAD/Range returned 403).
    {
        let h = resp.headers();
        if record.total_bytes.is_none() {
            if st == StatusCode::PARTIAL_CONTENT {
                record.total_bytes = h
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_content_range_total);
            } else {
                record.total_bytes = h
                    .get(reqwest::header::CONTENT_LENGTH)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok());
            }
        }
        if record.etag.is_none() {
            record.etag = h
                .get(reqwest::header::ETAG)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
        }
        if record.last_modified.is_none() {
            record.last_modified = h
                .get(reqwest::header::LAST_MODIFIED)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
        }
        if (record.file_name.is_empty() || record.file_name == "download.bin")
            && let Some(name) = h
                .get(reqwest::header::CONTENT_DISPOSITION)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_disposition_filename)
        {
            record.file_name = name.clone();
            record.title = name;
        }
    }

    let append_offset = if st == StatusCode::PARTIAL_CONTENT {
        existing_len
    } else {
        0
    };
    let file = if append_offset > 0 {
        OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(false)
            .open(partial_path)
    } else {
        File::create(partial_path)
    }
    .map_err(|e| DownloadFailure::network(format!("Open partial file failed: {e}")))?;

    let mut written = append_offset;
    record.state = DownloadState::Downloading;
    record.connections_active = 1;
    record.downloaded_bytes = written;
    record.segments = vec![SegmentProgress {
        id: 0,
        start: 0,
        end: record.total_bytes.unwrap_or(0).saturating_sub(1),
        downloaded: written,
        state: SegmentState::Active,
        speed_bps: 0,
        mirror_url: Some(record.url.clone()),
    }];
    on_progress(record);

    let mut stream = resp.bytes_stream();
    let mut last_emit = tokio::time::Instant::now();
    let mut last_speed_check = last_emit;
    let mut bytes_in_window = 0u64;
    let mut slow_accum_secs = 0.0f64;
    let mut watchdog = tokio::time::interval_at(
        last_emit + std::time::Duration::from_millis(50),
        std::time::Duration::from_millis(50),
    );
    watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        if matches!(*control_rx.borrow(), ControlCommand::Pause) {
            record.downloaded_bytes = written;
            if let Some(s0) = record.segments.get_mut(0) {
                s0.downloaded = written;
            }
            let _ = file.sync_all();
            return Ok(Outcome::Paused);
        }
        if matches!(*control_rx.borrow(), ControlCommand::Cancel) {
            record.downloaded_bytes = written;
            if let Some(s0) = record.segments.get_mut(0) {
                s0.downloaded = written;
            }
            return Ok(Outcome::Cancelled);
        }

        tokio::select! {
            _ = watchdog.tick() => {
                let now = tokio::time::Instant::now();
                let speed_dt = now.duration_since(last_speed_check).as_secs_f64();
                let mut should_emit = now.duration_since(last_emit) >= std::time::Duration::from_millis(50);
                if speed_dt >= 0.25 {
                    let speed = (bytes_in_window as f64 / speed_dt) as u64;
                    bytes_in_window = 0;
                    last_speed_check = now;
                    record.download_speed_bps = speed;
                    if let Some(s0) = record.segments.get_mut(0) {
                        s0.speed_bps = speed;
                    }
                    record.eta_secs = record
                        .total_bytes
                        .and_then(|tot| tot.saturating_sub(written).checked_div(speed));
                    if config.core.detect_unacceptably_slow {
                        if speed < config.core.unacceptably_slow_speed_bps {
                            slow_accum_secs += speed_dt;
                            if slow_accum_secs >= config.core.unacceptably_slow_interval_secs as f64 {
                                return Err(DownloadFailure::slow(format!(
                                    "Speed dropped below {} B/s for {}s",
                                    config.core.unacceptably_slow_speed_bps,
                                    config.core.unacceptably_slow_interval_secs
                                )));
                            }
                        } else {
                            slow_accum_secs = 0.0;
                        }
                    }
                    should_emit = true;
                }
                if should_emit {
                    last_emit = now;
                    on_progress(record);
                }
            }
            _ = control_rx.changed() => {
                match *control_rx.borrow() {
                    ControlCommand::Pause => {
                        record.downloaded_bytes = written;
                        if let Some(s0) = record.segments.get_mut(0) {
                            s0.downloaded = written;
                        }
                        let _ = file.sync_all();
                        return Ok(Outcome::Paused);
                    }
                    ControlCommand::Cancel => {
                        record.downloaded_bytes = written;
                        if let Some(s0) = record.segments.get_mut(0) {
                            s0.downloaded = written;
                        }
                        return Ok(Outcome::Cancelled);
                    }
                    ControlCommand::Run => {}
                }
            }
            next_chunk = stream.next() => {
                match next_chunk {
                    Some(Ok(chunk)) => {
                        limiter.acquire(chunk.len()).await;
                        write_at_exact(&file, &chunk, written)
                            .map_err(|e| DownloadFailure::network(format!("Disk write error: {e}")))?;
                        written += chunk.len() as u64;
                        bytes_in_window += chunk.len() as u64;
                        record.downloaded_bytes = written;
                        if let Some(s0) = record.segments.get_mut(0) {
                            s0.downloaded = written;
                        }
                        let now = tokio::time::Instant::now();
                        let speed_dt = now.duration_since(last_speed_check).as_secs_f64();
                        let mut should_emit = now.duration_since(last_emit) >= std::time::Duration::from_millis(50);
                        if speed_dt >= 0.25 {
                            let speed = (bytes_in_window as f64 / speed_dt) as u64;
                            bytes_in_window = 0;
                            last_speed_check = now;
                            if config.core.detect_unacceptably_slow {
                                if speed < config.core.unacceptably_slow_speed_bps {
                                    slow_accum_secs += speed_dt;
                                    if slow_accum_secs >= config.core.unacceptably_slow_interval_secs as f64 {
                                        return Err(DownloadFailure::slow(format!(
                                            "Speed dropped below {} B/s for {}s",
                                            config.core.unacceptably_slow_speed_bps,
                                            config.core.unacceptably_slow_interval_secs
                                        )));
                                    }
                                } else {
                                    slow_accum_secs = 0.0;
                                }
                            }
                            record.download_speed_bps = speed;
                            if let Some(s0) = record.segments.get_mut(0) {
                                s0.speed_bps = speed;
                            }
                            record.eta_secs = record
                                .total_bytes
                                .and_then(|tot| tot.saturating_sub(written).checked_div(speed));
                            should_emit = true;
                        }
                        if should_emit {
                            last_emit = now;
                            on_progress(record);
                        }
                    }
                    Some(Err(e)) => {
                        return Err(DownloadFailure::network(format!("Stream read error: {e}")));
                    }
                    None => break,
                }
            }
        }
    }

    let _ = file.sync_all();
    record.downloaded_bytes = written;
    if record.total_bytes.is_none() {
        record.total_bytes = Some(written);
    }
    if let Some(s0) = record.segments.get_mut(0) {
        s0.downloaded = written;
        s0.state = SegmentState::Completed;
    }
    Ok(Outcome::Completed)
}

fn write_at_exact(file: &File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    while !buf.is_empty() {
        #[cfg(unix)]
        let n = file.write_at(buf, offset)?;
        #[cfg(windows)]
        let n = file.seek_write(buf, offset)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "write_at returned 0",
            ));
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

async fn connect_ftp_stream(
    parsed: &Url,
    ssl_exceptions: &[SslException],
    cred: Option<&AuthCredential>,
) -> Result<AsyncRustlsFtpStream> {
    let host = parsed.host_str().unwrap_or("localhost");
    let is_ftps = parsed.scheme().eq_ignore_ascii_case("ftps");
    let port = parsed.port().unwrap_or(21);
    let user = cred
        .map(|c| c.username.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if parsed.username().is_empty() {
                "anonymous"
            } else {
                parsed.username()
            }
        });
    let pass = cred
        .map(|c| c.password.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| parsed.password().unwrap_or("ferrite@example.com"));

    let mut ftp = AsyncRustlsFtpStream::connect(format!("{host}:{port}")).await?;
    if is_ftps {
        let mut tls_cfg = build_rustls_client_config(ssl_exceptions).unwrap_or_else(|| {
            ensure_crypto_provider();
            rustls::ClientConfig::builder()
                .with_root_certificates(load_native_root_store())
                .with_no_client_auth()
        });
        tls_cfg.alpn_protocols.clear();
        let connector = AsyncRustlsConnector::from(suppaftp::tokio_rustls::TlsConnector::from(
            Arc::new(tls_cfg),
        ));
        ftp = ftp.into_secure(connector, host).await?;
    }
    ftp.login(user, pass).await?;
    Ok(ftp)
}

async fn probe_ftp_resource(
    url_str: &str,
    ssl_exceptions: &[SslException],
    cred: Option<&AuthCredential>,
) -> Result<ProbeResult> {
    let parsed = Url::parse(url_str)?;
    let path = parsed.path().to_string();

    let mut ftp = connect_ftp_stream(&parsed, ssl_exceptions, cred).await?;
    let size = ftp.size(&path).await.ok().map(|s| s as u64);
    let lm = ftp.mdtm(&path).await.ok().map(|dt| dt.to_string());
    let _ = ftp.quit().await;

    Ok(ProbeResult {
        final_url: url_str.to_string(),
        total_bytes: size,
        resume_support: ResumeSupport::Present,
        etag: None,
        last_modified: lm,
        content_type: None,
        suggested_filename: Path::new(&path)
            .file_name()
            .and_then(|s| s.to_str())
            .map(sanitize_filename),
        supports_range_split: false,
    })
}

async fn download_ftp<F>(
    mut record: DownloadRecord,
    config: &FdmConfig,
    ssl_exceptions: &[SslException],
    cred: Option<AuthCredential>,
    limiter: BandwidthLimiter,
    mut control_rx: watch::Receiver<ControlCommand>,
    mut on_progress: F,
) -> DownloadRecord
where
    F: FnMut(&DownloadRecord) + Send,
{
    let Ok(parsed) = Url::parse(&record.url) else {
        record.state = DownloadState::Error;
        record.abort_reason = DownloadAbortReason::InvalidUrl;
        record.last_error = Some("Invalid FTP URL".into());
        return record;
    };
    let remote_path = parsed.path().to_string();

    record.state = DownloadState::Starting;
    on_progress(&record);

    let max_attempts = if config.core.auto_retry_failed && !record.flags.disable_auto_retry {
        config.core.max_auto_retries.max(1) + 1
    } else {
        1
    };

    for attempt in 0..max_attempts {
        let mut ftp = match connect_ftp_stream(&parsed, ssl_exceptions, cred.as_ref()).await {
            Ok(f) => f,
            Err(e) => {
                record.last_error = Some(format!("FTP connect/login error: {e}"));
                record.abort_reason = DownloadAbortReason::NetworkError;
                if attempt + 1 < max_attempts {
                    tokio::time::sleep(std::time::Duration::from_secs(1 + attempt as u64)).await;
                    continue;
                }
                record.state = DownloadState::Error;
                return record;
            }
        };
        let _ = ftp.transfer_type(suppaftp::types::FileType::Binary).await;
        if let Ok(sz) = ftp.size(&remote_path).await {
            record.total_bytes = Some(sz as u64);
        }

        let _ = std::fs::create_dir_all(&record.target_dir);
        let partial_path = record.partial_path();
        let offset = std::fs::metadata(&partial_path)
            .map(|m| m.len())
            .unwrap_or(0);
        if offset > 0 {
            let _ = ftp.resume_transfer(offset as usize).await;
        }
        let mut reader = match ftp.retr_as_stream(&remote_path).await {
            Ok(r) => r,
            Err(e) => {
                record.last_error = Some(format!("FTP RETR error: {e}"));
                record.abort_reason = DownloadAbortReason::NetworkError;
                if attempt + 1 < max_attempts {
                    tokio::time::sleep(std::time::Duration::from_secs(1 + attempt as u64)).await;
                    continue;
                }
                record.state = DownloadState::Error;
                return record;
            }
        };

        let file = if offset > 0 {
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&partial_path)
        } else {
            File::create(&partial_path)
        };
        let file = match file {
            Ok(f) => f,
            Err(e) => {
                record.state = DownloadState::Error;
                record.last_error = Some(format!("Open file failed: {e}"));
                return record;
            }
        };

        let mut written = offset;
        record.state = DownloadState::Downloading;
        record.connections_active = 1;
        let mut buf = vec![0u8; 64 * 1024];
        let mut last_tick = tokio::time::Instant::now();
        let mut bytes_since_tick = 0u64;
        let mut slow_accum_secs = 0.0f64;
        let mut watchdog = tokio::time::interval(std::time::Duration::from_millis(250));
        watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let mut failed_retryable = false;
        let mut eof = false;
        while !eof {
            if matches!(*control_rx.borrow(), ControlCommand::Pause) {
                record.state = DownloadState::Paused;
                record.connections_active = 0;
                record.download_speed_bps = 0;
                on_progress(&record);
                return record;
            }
            if matches!(*control_rx.borrow(), ControlCommand::Cancel) {
                record.state = DownloadState::Stopped;
                record.connections_active = 0;
                record.download_speed_bps = 0;
                record.abort_reason = DownloadAbortReason::AbortedByUser;
                on_progress(&record);
                return record;
            }
            tokio::select! {
                _ = watchdog.tick() => {
                    let now = tokio::time::Instant::now();
                    let dt = now.duration_since(last_tick).as_secs_f64();
                    if dt >= 0.25 {
                        let speed = (bytes_since_tick as f64 / dt) as u64;
                        bytes_since_tick = 0;
                        last_tick = now;
                        record.download_speed_bps = speed;
                        record.eta_secs = record
                            .total_bytes
                            .and_then(|tot| tot.saturating_sub(written).checked_div(speed));
                        on_progress(&record);
                        if config.core.detect_unacceptably_slow {
                            if speed < config.core.unacceptably_slow_speed_bps {
                                slow_accum_secs += dt;
                                if slow_accum_secs >= config.core.unacceptably_slow_interval_secs as f64 {
                                    record.abort_reason = DownloadAbortReason::SlowDownload;
                                    record.last_error = Some(format!(
                                        "FTP speed dropped below {} B/s for {}s",
                                        config.core.unacceptably_slow_speed_bps,
                                        config.core.unacceptably_slow_interval_secs
                                    ));
                                    failed_retryable = true;
                                    break;
                                }
                            } else {
                                slow_accum_secs = 0.0;
                            }
                        }
                    }
                }
                _ = control_rx.changed() => {}
                res = reader.read(&mut buf) => {
                    match res {
                        Ok(0) => {
                            eof = true;
                        }
                        Ok(n) => {
                            limiter.acquire(n).await;
                            if let Err(e) = write_at_exact(&file, &buf[..n], written) {
                                record.state = DownloadState::Error;
                                record.connections_active = 0;
                                record.download_speed_bps = 0;
                                record.last_error = Some(format!("Disk write error: {e}"));
                                return record;
                            }
                            written += n as u64;
                            bytes_since_tick += n as u64;
                            record.downloaded_bytes = written;
                        }
                        Err(e) => {
                            record.abort_reason = DownloadAbortReason::NetworkError;
                            record.last_error = Some(format!("FTP stream error: {e}"));
                            failed_retryable = true;
                            break;
                        }
                    }
                }
            }
        }

        if failed_retryable {
            record.retry_count += 1;
            record.connections_active = 0;
            record.download_speed_bps = 0;
            if attempt + 1 < max_attempts {
                tokio::time::sleep(std::time::Duration::from_secs(1 + attempt as u64)).await;
                continue;
            }
            record.state = DownloadState::Error;
            on_progress(&record);
            return record;
        }

        let _ = reader.finish().await;
        let _ = ftp.quit().await;

        let output_path = record.output_path();
        if partial_path != output_path {
            let _ = std::fs::rename(&partial_path, &output_path);
        }
        if config.core.set_mark_of_the_web {
            let _ = set_mark_of_the_web(&output_path, &record.url, "FerriteDM");
        }
        record.state = DownloadState::Completed;
        record.connections_active = 0;
        record.download_speed_bps = 0;
        record.completed_at = Some(chrono::Utc::now());
        on_progress(&record);
        return record;
    }

    record
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrite_core::{DownloadFlags, HttpRequestOptions, TrafficUsageMode};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn parses_content_disposition_variants() {
        assert_eq!(
            parse_content_disposition_filename("attachment; filename*=UTF-8''my%20file.zip")
                .as_deref(),
            Some("my file.zip")
        );
        assert_eq!(
            parse_content_disposition_filename("inline; filename=\"report-2026.pdf\"").as_deref(),
            Some("report-2026.pdf")
        );
    }

    #[tokio::test]
    async fn multi_section_range_download_and_sha256_verify() {
        // Generate 600 KiB deterministic payload (> 256 KiB triggers 4-section split!)
        let payload: Arc<Vec<u8>> = Arc::new((0..(600 * 1024)).map(|i| (i % 251) as u8).collect());
        let expected_hash = {
            let mut h = Sha256::new();
            h.update(&*payload);
            hex::encode(h.finalize())
        };

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload_srv = Arc::clone(&payload);

        let srv_handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let data = Arc::clone(&payload_srv);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let is_head = req.starts_with("HEAD ");
                    let range_hdr = req
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                        .and_then(|l| l.split_once(':'))
                        .map(|(_, v)| v.trim().to_string());

                    if let Some(rh) = range_hdr
                        && let Some(spec) = rh.strip_prefix("bytes=")
                        && let Some((a, b)) = spec.split_once('-')
                    {
                        let start: usize = a.parse().unwrap_or(0);
                        let end: usize = b
                            .parse::<usize>()
                            .unwrap_or(data.len() - 1)
                            .min(data.len() - 1);
                        let slice = &data[start..=end];
                        let hdr = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nAccept-Ranges: bytes\r\nContent-Disposition: attachment; filename=\"bundle.bin\"\r\nConnection: close\r\n\r\n",
                            slice.len(),
                            data.len()
                        );
                        let _ = sock.write_all(hdr.as_bytes()).await;
                        let _ = sock.write_all(slice).await;
                        return;
                    }

                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nContent-Disposition: attachment; filename=\"bundle.bin\"\r\nConnection: close\r\n\r\n",
                        data.len()
                    );
                    let _ = sock.write_all(hdr.as_bytes()).await;
                    if !is_head {
                        let _ = sock.write_all(&data).await;
                    }
                });
            }
        });

        let dir = std::env::temp_dir().join(format!("ferrite_http_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut cfg = FdmConfig::default();
        cfg.core.default_download_path = dir.clone();
        cfg.core.current_tum = TrafficUsageMode::Medium; // 8 connections max
        cfg.core.set_mark_of_the_web = false;

        let url = format!("http://{}/bundle.bin", addr);
        let rec = DownloadRecord {
            id: 1,
            uuid: uuid::Uuid::new_v4().to_string(),
            parent_id: None,
            children_ids: vec![],
            order_index: 1,
            download_type: ferrite_core::DownloadType::Www,
            state: DownloadState::Queued,
            priority: DownloadPriority::Normal,
            resume_support: ResumeSupport::Unknown,
            flags: DownloadFlags::default(),
            title: "bundle.bin".into(),
            url: url.clone(),
            target_dir: dir.clone(),
            file_name: "bundle.bin".into(),
            partial_suffix: ".fdmdownload".into(),
            total_bytes: None,
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            download_speed_bps: 0,
            upload_speed_bps: 0,
            eta_secs: None,
            connections_active: 0,
            etag: None,
            last_modified: None,
            file_type: ferrite_core::FileType::Other,
            http: HttpRequestOptions {
                url,
                ..Default::default()
            },
            torrent_info_hash: None,
            torrent_source: None,
            tags: vec![],
            files: vec![],
            segments: vec![],
            peers: vec![],
            trackers: vec![],
            schedule: None,
            retry_count: 0,
            forbid_auto_retry_count: 0,
            last_error: None,
            abort_reason: DownloadAbortReason::None,
            preview_path: None,
            module_data: serde_json::Value::Null,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            completed_at: None,
        };

        let (_tx, rx) = watch::channel(ControlCommand::Run);
        let limiter = BandwidthLimiter::new(0);
        let done = download_http_or_ftp(rec, &cfg, &[], None, limiter, rx, |_| {}).await;
        assert_eq!(
            done.state,
            DownloadState::Completed,
            "err={:?}",
            done.last_error
        );
        assert!(done.segments.len() >= 4, "expected >=4 sections");
        assert_eq!(done.downloaded_bytes, 600 * 1024);

        let actual_hash = ferrite_core::hash::compute_file_hash(
            &dir.join("bundle.bin"),
            ferrite_core::HashAlgorithm::Sha256,
        )
        .unwrap();
        assert_eq!(actual_hash, expected_hash);

        srv_handle.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn make_test_record(url: String, dir: &Path, file_name: &str) -> DownloadRecord {
        DownloadRecord {
            id: 1,
            uuid: uuid::Uuid::new_v4().to_string(),
            parent_id: None,
            children_ids: vec![],
            order_index: 1,
            download_type: ferrite_core::DownloadType::Www,
            state: DownloadState::Queued,
            priority: DownloadPriority::Normal,
            resume_support: ResumeSupport::Unknown,
            flags: DownloadFlags::default(),
            title: file_name.into(),
            url: url.clone(),
            target_dir: dir.to_path_buf(),
            file_name: file_name.into(),
            partial_suffix: ".fdmdownload".into(),
            total_bytes: None,
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            download_speed_bps: 0,
            upload_speed_bps: 0,
            eta_secs: None,
            connections_active: 0,
            etag: None,
            last_modified: None,
            file_type: ferrite_core::FileType::Other,
            http: HttpRequestOptions {
                url,
                ..Default::default()
            },
            torrent_info_hash: None,
            torrent_source: None,
            tags: vec![],
            files: vec![],
            segments: vec![],
            peers: vec![],
            trackers: vec![],
            schedule: None,
            retry_count: 0,
            forbid_auto_retry_count: 0,
            last_error: None,
            abort_reason: DownloadAbortReason::None,
            preview_path: None,
            module_data: serde_json::Value::Null,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            completed_at: None,
        }
    }

    #[tokio::test]
    async fn pause_and_resume_preserves_partial_and_completes() {
        let payload: Arc<Vec<u8>> = Arc::new((0..(128 * 1024)).map(|i| (i % 199) as u8).collect());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload_srv = Arc::clone(&payload);
        let saw_resume_range = Arc::new(AtomicBool::new(false));
        let saw_resume_srv = Arc::clone(&saw_resume_range);

        let srv_handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let data = Arc::clone(&payload_srv);
                let saw_resume = Arc::clone(&saw_resume_srv);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let is_head = req.starts_with("HEAD ");
                    let range_hdr = req
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                        .and_then(|l| l.split_once(':'))
                        .map(|(_, v)| v.trim().to_string());

                    if let Some(rh) = range_hdr
                        && let Some(spec) = rh.strip_prefix("bytes=")
                        && let Some((a, b)) = spec.split_once('-')
                    {
                        let start: usize = a.parse().unwrap_or(0);
                        let end: usize = b
                            .parse::<usize>()
                            .unwrap_or(data.len() - 1)
                            .min(data.len() - 1);
                        if start > 0 {
                            saw_resume.store(true, Ordering::SeqCst);
                        }
                        let slice = &data[start..=end];
                        let hdr = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                            slice.len(),
                            data.len()
                        );
                        let _ = sock.write_all(hdr.as_bytes()).await;
                        for chunk in slice.chunks(8192) {
                            if sock.write_all(chunk).await.is_err() {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(12)).await;
                        }
                        return;
                    }

                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                        data.len()
                    );
                    let _ = sock.write_all(hdr.as_bytes()).await;
                    if !is_head {
                        for chunk in data.chunks(8192) {
                            if sock.write_all(chunk).await.is_err() {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(12)).await;
                        }
                    }
                });
            }
        });

        let dir = std::env::temp_dir().join(format!("ferrite_http_pause_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut cfg = FdmConfig::default();
        cfg.core.default_download_path = dir.clone();
        cfg.core.set_mark_of_the_web = false;

        let rec = make_test_record(format!("http://{addr}/paused.bin"), &dir, "paused.bin");
        let (tx, rx) = watch::channel(ControlCommand::Run);
        let paused_sent = Arc::new(AtomicBool::new(false));

        let paused =
            download_http_or_ftp(rec, &cfg, &[], None, BandwidthLimiter::new(0), rx, |p| {
                if p.downloaded_bytes >= 16384 && !paused_sent.swap(true, Ordering::SeqCst) {
                    let _ = tx.send(ControlCommand::Pause);
                }
            })
            .await;
        assert_eq!(paused.state, DownloadState::Paused);
        assert!(dir.join("paused.bin.fdmdownload").exists());
        assert!(paused.downloaded_bytes > 0);
        assert!(paused.downloaded_bytes < payload.len() as u64);

        let (_tx2, rx2) = watch::channel(ControlCommand::Run);
        let completed = download_http_or_ftp(
            paused,
            &cfg,
            &[],
            None,
            BandwidthLimiter::new(0),
            rx2,
            |_| {},
        )
        .await;
        assert_eq!(completed.state, DownloadState::Completed);
        assert!(saw_resume_range.load(Ordering::SeqCst));
        assert_eq!(std::fs::read(dir.join("paused.bin")).unwrap(), *payload);

        srv_handle.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unwanted_404_fails_fast_without_retrying() {
        let hits = Arc::new(AtomicU64::new(0));
        let hits_srv = Arc::clone(&hits);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let hits = Arc::clone(&hits_srv);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 1024];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    if req.starts_with("GET ") {
                        let range_hdr = req
                            .lines()
                            .any(|l| l.to_ascii_lowercase().starts_with("range: bytes=0-0"));
                        if !range_hdr {
                            hits.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                    let resp =
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });

        let dir = std::env::temp_dir().join(format!("ferrite_http_404_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut cfg = FdmConfig::default();
        cfg.core.auto_retry_failed = true;
        cfg.core.max_auto_retries = 5;
        cfg.core.detect_unwanted_errors = true;

        let rec = make_test_record(format!("http://{addr}/missing.zip"), &dir, "missing.zip");
        let (_tx, rx) = watch::channel(ControlCommand::Run);
        let done =
            download_http_or_ftp(rec, &cfg, &[], None, BandwidthLimiter::new(0), rx, |_| {}).await;

        assert_eq!(done.state, DownloadState::Error);
        assert_eq!(done.abort_reason, DownloadAbortReason::UnwantedError);
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn system_default_user_agent_header_is_sent_when_enabled() {
        let seen_ua = Arc::new(Mutex::new(String::new()));
        let seen_ua_srv = Arc::clone(&seen_ua);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let seen_ua = Arc::clone(&seen_ua_srv);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    if let Some(ua_line) = req
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("user-agent:"))
                        && let Some((_, val)) = ua_line.split_once(':')
                    {
                        *seen_ua.lock().unwrap() = val.trim().to_string();
                    }
                    let resp =
                        "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc";
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });

        let mut cfg = FdmConfig::default();
        cfg.core.use_system_default_user_agent = true;
        let client = build_http_client(&cfg, &[], None).unwrap();
        let _ = client
            .get(format!("http://{addr}/ua"))
            .send()
            .await
            .unwrap();
        let captured = seen_ua.lock().unwrap().clone();
        assert_eq!(
            captured,
            ferrite_core::os_integration::system_default_user_agent()
        );

        srv.abort();
    }

    #[tokio::test]
    async fn disable_auto_retry_and_slow_download_watchdog_work() {
        let hits = Arc::new(AtomicU64::new(0));
        let hits_srv = Arc::clone(&hits);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let hits = Arc::clone(&hits_srv);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    if req.starts_with("GET ") && req.contains("/err500") {
                        let range_hdr = req
                            .lines()
                            .any(|l| l.to_ascii_lowercase().starts_with("range: bytes=0-0"));
                        if !range_hdr {
                            hits.fetch_add(1, Ordering::SeqCst);
                        }
                        let resp = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        let _ = sock.write_all(resp.as_bytes()).await;
                    } else if req.contains("/stall") {
                        let is_head = req.starts_with("HEAD ");
                        let hdr =
                            "HTTP/1.1 200 OK\r\nContent-Length: 10240\r\nConnection: close\r\n\r\n";
                        let _ = sock.write_all(hdr.as_bytes()).await;
                        if !is_head {
                            let _ = sock.write_all(&[1u8; 8]).await;
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    }
                });
            }
        });

        let dir = std::env::temp_dir().join(format!("ferrite_http_flags_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut cfg = FdmConfig::default();
        cfg.core.auto_retry_failed = true;
        cfg.core.max_auto_retries = 3;
        cfg.core.set_mark_of_the_web = false;

        let mut rec = make_test_record(format!("http://{addr}/err500"), &dir, "err.bin");
        rec.flags.disable_auto_retry = true;
        let (_tx, rx) = watch::channel(ControlCommand::Run);
        let done =
            download_http_or_ftp(rec, &cfg, &[], None, BandwidthLimiter::new(0), rx, |_| {}).await;
        assert_eq!(done.state, DownloadState::Error);
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // Now verify single-stream slow download detection aborts stalled transfers.
        cfg.core.auto_retry_failed = false;
        cfg.core.detect_unacceptably_slow = true;
        cfg.core.unacceptably_slow_speed_bps = 10_000;
        cfg.core.unacceptably_slow_interval_secs = 1;
        let slow_rec = make_test_record(format!("http://{addr}/stall"), &dir, "stall.bin");
        let (_tx2, rx2) = watch::channel(ControlCommand::Run);
        let slow_done = download_http_or_ftp(
            slow_rec,
            &cfg,
            &[],
            None,
            BandwidthLimiter::new(0),
            rx2,
            |_| {},
        )
        .await;
        assert_eq!(slow_done.state, DownloadState::Error);
        assert_eq!(slow_done.abort_reason, DownloadAbortReason::SlowDownload);

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn head_and_range_403_falls_back_to_single_stream_get() {
        let payload = b"ACTUAL-BINARY-PAYLOAD-AFTER-HEAD-AND-RANGE-403".to_vec();
        let payload_srv = Arc::new(payload.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let data = Arc::clone(&payload_srv);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let is_head = req.starts_with("HEAD ");
                    let has_range = req
                        .lines()
                        .any(|l| l.to_ascii_lowercase().starts_with("range:"));
                    if is_head || has_range {
                        let body = "<html>403 Forbidden</html>";
                        let resp = format!(
                            "HTTP/1.1 403 Forbidden\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            if is_head { "" } else { body }
                        );
                        let _ = sock.write_all(resp.as_bytes()).await;
                        return;
                    }
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"actual.bin\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        data.len()
                    );
                    let _ = sock.write_all(hdr.as_bytes()).await;
                    let _ = sock.write_all(&data).await;
                });
            }
        });

        let dir = std::env::temp_dir().join(format!(
            "ferrite_http_403_head_range_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = FdmConfig::default();
        cfg.core.set_mark_of_the_web = false;

        let url = format!("http://{addr}/signed-get-only");
        let rec = make_test_record(url, &dir, "download.bin");
        let client = build_http_client(&cfg, &[], None).unwrap();
        let probe = probe_http_resource(&client, &rec, &[], None).await.unwrap();
        // Probe must NOT pollute metadata with the 403 error HTML response.
        assert!(probe.total_bytes.is_none());
        assert!(probe.content_type.is_none());
        assert_eq!(probe.resume_support, ResumeSupport::Absent);

        let (_tx, rx) = watch::channel(ControlCommand::Run);
        let done =
            download_http_or_ftp(rec, &cfg, &[], None, BandwidthLimiter::new(0), rx, |_| {}).await;
        assert_eq!(
            done.state,
            DownloadState::Completed,
            "err={:?}",
            done.last_error
        );
        assert_eq!(done.file_name, "actual.bin");
        assert_eq!(done.total_bytes, Some(payload.len() as u64));
        assert_eq!(std::fs::read(done.output_path()).unwrap(), payload);

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn multi_section_403_falls_back_to_single_stream_and_completes() {
        let payload: Arc<Vec<u8>> = Arc::new((0..(512 * 1024)).map(|i| (i % 239) as u8).collect());
        let payload_srv = Arc::clone(&payload);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let data = Arc::clone(&payload_srv);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    if req.starts_with("HEAD ") {
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            data.len()
                        );
                        let _ = sock.write_all(hdr.as_bytes()).await;
                        return;
                    }
                    let has_range = req
                        .lines()
                        .any(|l| l.to_ascii_lowercase().starts_with("range:"));
                    if has_range {
                        let resp = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        let _ = sock.write_all(resp.as_bytes()).await;
                        return;
                    }
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        data.len()
                    );
                    let _ = sock.write_all(hdr.as_bytes()).await;
                    let _ = sock.write_all(&data).await;
                });
            }
        });

        let dir =
            std::env::temp_dir().join(format!("ferrite_http_403_multi_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = FdmConfig::default();
        cfg.core.set_mark_of_the_web = false;
        cfg.core.current_tum = TrafficUsageMode::Medium;

        let rec = make_test_record(format!("http://{addr}/big.bin"), &dir, "big.bin");
        let (_tx, rx) = watch::channel(ControlCommand::Run);
        let done =
            download_http_or_ftp(rec, &cfg, &[], None, BandwidthLimiter::new(0), rx, |_| {}).await;
        assert_eq!(
            done.state,
            DownloadState::Completed,
            "err={:?}",
            done.last_error
        );
        assert_eq!(std::fs::read(dir.join("big.bin")).unwrap(), *payload);

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn user_agent_403_fallback_works_in_both_directions() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");
                    let ua = req
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("user-agent:"))
                        .and_then(|l| l.split_once(':'))
                        .map(|(_, v)| v.trim().to_string())
                        .unwrap_or_default();
                    let has_accept_lang = req
                        .lines()
                        .any(|l| l.to_ascii_lowercase().starts_with("accept-language:"));

                    let allowed = match path {
                        "/browser-only" => ua.starts_with("Mozilla/5.0") && has_accept_lang,
                        "/client-only" => {
                            !ua.starts_with("Mozilla/5.0") && ua.contains("FerriteDM")
                        }
                        _ => false,
                    };
                    if !allowed {
                        let resp = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        let _ = sock.write_all(resp.as_bytes()).await;
                        return;
                    }
                    let body = b"OK-UA-FALLBACK";
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = sock.write_all(hdr.as_bytes()).await;
                    if !req.starts_with("HEAD ") {
                        let _ = sock.write_all(body).await;
                    }
                });
            }
        });

        let dir =
            std::env::temp_dir().join(format!("ferrite_http_403_ua_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // Case A: Config disables browser UA, server requires browser UA -> falls back to browser UA.
        let mut cfg_a = FdmConfig::default();
        cfg_a.core.use_system_default_user_agent = false;
        cfg_a.core.set_mark_of_the_web = false;
        let rec_a = make_test_record(format!("http://{addr}/browser-only"), &dir, "a.bin");
        let (_tx1, rx1) = watch::channel(ControlCommand::Run);
        let done_a = download_http_or_ftp(
            rec_a,
            &cfg_a,
            &[],
            None,
            BandwidthLimiter::new(0),
            rx1,
            |_| {},
        )
        .await;
        assert_eq!(
            done_a.state,
            DownloadState::Completed,
            "browser-only failed: {:?}",
            done_a.last_error
        );
        assert_eq!(std::fs::read(dir.join("a.bin")).unwrap(), b"OK-UA-FALLBACK");

        // Case B: Config enables browser UA (default), server blocks Mozilla/5.0 (like SourceForge) -> falls back to FerriteDM UA.
        let mut cfg_b = FdmConfig::default();
        cfg_b.core.use_system_default_user_agent = true;
        cfg_b.core.set_mark_of_the_web = false;
        let rec_b = make_test_record(format!("http://{addr}/client-only"), &dir, "b.bin");
        let (_tx2, rx2) = watch::channel(ControlCommand::Run);
        let done_b = download_http_or_ftp(
            rec_b,
            &cfg_b,
            &[],
            None,
            BandwidthLimiter::new(0),
            rx2,
            |_| {},
        )
        .await;
        assert_eq!(
            done_b.state,
            DownloadState::Completed,
            "client-only failed: {:?}",
            done_b.last_error
        );
        assert_eq!(std::fs::read(dir.join("b.bin")).unwrap(), b"OK-UA-FALLBACK");

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn hotlink_referer_403_falls_back_to_same_origin_referer() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let expected_referer = format!("http://{addr}/");
        let expected_ref_srv = expected_referer.clone();

        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let expected = expected_ref_srv.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let ref_hdr = req
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("referer:"))
                        .and_then(|l| l.split_once(':'))
                        .map(|(_, v)| v.trim().to_string());
                    if ref_hdr.as_deref() != Some(expected.as_str()) {
                        let resp = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        let _ = sock.write_all(resp.as_bytes()).await;
                        return;
                    }
                    let body = b"HOTLINK-OK";
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = sock.write_all(hdr.as_bytes()).await;
                    if !req.starts_with("HEAD ") {
                        let _ = sock.write_all(body).await;
                    }
                });
            }
        });

        let dir =
            std::env::temp_dir().join(format!("ferrite_http_403_ref_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = FdmConfig::default();
        cfg.core.set_mark_of_the_web = false;

        let rec = make_test_record(format!("http://{addr}/protected/img.png"), &dir, "img.png");
        let (_tx, rx) = watch::channel(ControlCommand::Run);
        let done =
            download_http_or_ftp(rec, &cfg, &[], None, BandwidthLimiter::new(0), rx, |_| {}).await;
        assert_eq!(
            done.state,
            DownloadState::Completed,
            "err={:?}",
            done.last_error
        );
        assert_eq!(std::fs::read(dir.join("img.png")).unwrap(), b"HOTLINK-OK");

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn redirect_set_cookie_is_preserved_and_range_uses_identity_encoding() {
        let seen_encodings = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen_enc_srv = Arc::clone(&seen_encodings);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let seen_enc = Arc::clone(&seen_enc_srv);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");
                    if let Some(ae) = req
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("accept-encoding:"))
                        .and_then(|l| l.split_once(':'))
                        .map(|(_, v)| v.trim().to_string())
                    {
                        seen_enc.lock().unwrap().push(ae);
                    }
                    if path == "/gate" {
                        let resp = "HTTP/1.1 302 Found\r\nSet-Cookie: dl_gate=allowed42; Path=/\r\nLocation: /payload.bin\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        let _ = sock.write_all(resp.as_bytes()).await;
                        return;
                    }
                    let has_cookie = req.lines().any(|l| {
                        l.to_ascii_lowercase().starts_with("cookie:")
                            && l.contains("dl_gate=allowed42")
                    });
                    if !has_cookie {
                        let resp = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        let _ = sock.write_all(resp.as_bytes()).await;
                        return;
                    }
                    let body = b"COOKIE-GATED-CONTENT";
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = sock.write_all(hdr.as_bytes()).await;
                    if !req.starts_with("HEAD ") {
                        let _ = sock.write_all(body).await;
                    }
                });
            }
        });

        let dir =
            std::env::temp_dir().join(format!("ferrite_http_cookie_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = FdmConfig::default();
        cfg.core.set_mark_of_the_web = false;

        let rec = make_test_record(format!("http://{addr}/gate"), &dir, "payload.bin");
        let (_tx, rx) = watch::channel(ControlCommand::Run);
        let done =
            download_http_or_ftp(rec, &cfg, &[], None, BandwidthLimiter::new(0), rx, |_| {}).await;
        assert_eq!(
            done.state,
            DownloadState::Completed,
            "err={:?}",
            done.last_error
        );
        assert_eq!(
            std::fs::read(dir.join("payload.bin")).unwrap(),
            b"COOKIE-GATED-CONTENT"
        );
        let encodings = seen_encodings.lock().unwrap();
        assert!(!encodings.is_empty());
        assert!(
            encodings.iter().all(|e| e == "identity"),
            "expected identity encoding for binary/range download, got: {encodings:?}"
        );

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn permanent_403_fails_fast_as_unwanted_error_without_retry_loop() {
        let hits = Arc::new(AtomicU64::new(0));
        let hits_srv = Arc::clone(&hits);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let hits = Arc::clone(&hits_srv);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 1024];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    if req.starts_with("GET ")
                        && !req
                            .lines()
                            .any(|l| l.to_ascii_lowercase().starts_with("range: bytes=0-0"))
                    {
                        hits.fetch_add(1, Ordering::SeqCst);
                    }
                    let resp =
                        "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });

        let dir =
            std::env::temp_dir().join(format!("ferrite_http_perm403_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = FdmConfig::default();
        cfg.core.auto_retry_failed = true;
        cfg.core.max_auto_retries = 5;
        cfg.core.detect_unwanted_errors = true;

        let rec = make_test_record(
            format!("http://{addr}/forbidden.zip"),
            &dir,
            "forbidden.zip",
        );
        let (_tx, rx) = watch::channel(ControlCommand::Run);
        let done =
            download_http_or_ftp(rec, &cfg, &[], None, BandwidthLimiter::new(0), rx, |_| {}).await;
        assert_eq!(done.state, DownloadState::Error);
        assert_eq!(done.abort_reason, DownloadAbortReason::UnwantedError);
        assert_eq!(done.retry_count, 1);
        // Initial GET + at most 2 fallbacks (alt UA + same-origin referer) = <= 3 GETs, not 6 retry loops!
        assert!(hits.load(Ordering::SeqCst) <= 3);

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cross_origin_redirect_strips_credentials_in_multi_section_download() {
        let payload: Arc<Vec<u8>> = Arc::new((0..(512 * 1024)).map(|i| (i % 241) as u8).collect());
        let payload_cdn = Arc::clone(&payload);

        // Origin B (CDN): rejects ANY Authorization or origin-A Cookie with 403!
        let cdn_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cdn_addr = cdn_listener.local_addr().unwrap();
        let leaked_creds = Arc::new(AtomicBool::new(false));
        let leaked_srv = Arc::clone(&leaked_creds);

        let cdn_srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = cdn_listener.accept().await else {
                    break;
                };
                let data = Arc::clone(&payload_cdn);
                let leaked = Arc::clone(&leaked_srv);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let has_auth = req
                        .lines()
                        .any(|l| l.to_ascii_lowercase().starts_with("authorization:"));
                    let has_origin_cookie = req.lines().any(|l| {
                        l.to_ascii_lowercase().starts_with("cookie:")
                            && l.contains("origin_a=secret")
                    });
                    if has_auth || has_origin_cookie {
                        leaked.store(true, Ordering::SeqCst);
                        let resp = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        let _ = sock.write_all(resp.as_bytes()).await;
                        return;
                    }
                    let is_head = req.starts_with("HEAD ");
                    let range_hdr = req
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                        .and_then(|l| l.split_once(':'))
                        .map(|(_, v)| v.trim().to_string());
                    if let Some(rh) = range_hdr
                        && let Some(spec) = rh.strip_prefix("bytes=")
                        && let Some((a, b)) = spec.split_once('-')
                    {
                        let start: usize = a.parse().unwrap_or(0);
                        let end: usize = b
                            .parse::<usize>()
                            .unwrap_or(data.len() - 1)
                            .min(data.len() - 1);
                        let slice = &data[start..=end];
                        let hdr = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                            slice.len(),
                            data.len()
                        );
                        let _ = sock.write_all(hdr.as_bytes()).await;
                        let _ = sock.write_all(slice).await;
                        return;
                    }
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                        data.len()
                    );
                    let _ = sock.write_all(hdr.as_bytes()).await;
                    if !is_head {
                        let _ = sock.write_all(&data).await;
                    }
                });
            }
        });

        // Origin A (redirector on a distinct port = distinct origin)
        let origin_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin_addr = origin_listener.local_addr().unwrap();
        let origin_srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = origin_listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    let resp = format!(
                        "HTTP/1.1 302 Found\r\nLocation: http://{cdn_addr}/signed.bin\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });

        let dir =
            std::env::temp_dir().join(format!("ferrite_http_xorigin_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = FdmConfig::default();
        cfg.core.set_mark_of_the_web = false;
        cfg.core.current_tum = TrafficUsageMode::Medium;

        let mut rec = make_test_record(format!("http://{origin_addr}/dl"), &dir, "signed.bin");
        rec.http.cookies = Some("origin_a=secret".into());
        let cred = Some(AuthCredential {
            scope: origin_addr.to_string(),
            username: "alice".into(),
            password: "s3cr3t".into(),
        });

        let (_tx, rx) = watch::channel(ControlCommand::Run);
        let done =
            download_http_or_ftp(rec, &cfg, &[], cred, BandwidthLimiter::new(0), rx, |_| {}).await;
        assert_eq!(
            done.state,
            DownloadState::Completed,
            "err={:?}",
            done.last_error
        );
        assert!(
            !leaked_creds.load(Ordering::SeqCst),
            "origin-A Authorization or Cookie leaked to cross-origin CDN"
        );
        assert!(done.segments.len() >= 4, "expected multi-section download");
        assert_eq!(std::fs::read(dir.join("signed.bin")).unwrap(), *payload);

        origin_srv.abort();
        cdn_srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cookie_jar_enforces_secure_flag_and_rejects_single_label_domain() {
        let jar = RedirectCookieJar::default();
        let mut headers = HeaderMap::new();
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("sec=1; Secure; Domain=example.com"),
        );
        headers.append(SET_COOKIE, HeaderValue::from_static("broad=2; Domain=com"));
        let https_url = Url::parse("https://dl.example.com/file").unwrap();
        jar.store_from_response(&https_url, &headers);

        // Secure cookie sent on https subdomain, NOT on http downgrade.
        let https_sub = Url::parse("https://cdn.example.com/file").unwrap();
        let http_sub = Url::parse("http://cdn.example.com/file").unwrap();
        let https_cookies = jar.cookie_header_for(&https_sub).unwrap_or_default();
        assert!(https_cookies.contains("sec=1"));
        assert!(jar.cookie_header_for(&http_sub).is_none());

        // Single-label `Domain=com` must be scoped only to exact `dl.example.com`, never `other.com`.
        let other_com = Url::parse("https://other.com/file").unwrap();
        assert!(jar.cookie_header_for(&other_com).is_none());
        let same_host = jar.cookie_header_for(&https_url).unwrap_or_default();
        assert!(same_host.contains("broad=2"));
    }

    #[tokio::test]
    async fn probe_winning_ua_is_reused_for_multi_section_without_403_burst() {
        let payload: Arc<Vec<u8>> = Arc::new((0..(512 * 1024)).map(|i| (i % 227) as u8).collect());
        let payload_srv = Arc::clone(&payload);
        let section_403s = Arc::new(AtomicU64::new(0));
        let section_403s_srv = Arc::clone(&section_403s);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let data = Arc::clone(&payload_srv);
                let s403 = Arc::clone(&section_403s_srv);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let ua = req
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("user-agent:"))
                        .and_then(|l| l.split_once(':'))
                        .map(|(_, v)| v.trim().to_string())
                        .unwrap_or_default();
                    let range_hdr = req
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                        .and_then(|l| l.split_once(':'))
                        .map(|(_, v)| v.trim().to_string());

                    if ua.starts_with("Mozilla/5.0") {
                        if let Some(ref rh) = range_hdr
                            && rh != "bytes=0-0"
                        {
                            s403.fetch_add(1, Ordering::SeqCst);
                        }
                        let resp = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        let _ = sock.write_all(resp.as_bytes()).await;
                        return;
                    }

                    if let Some(rh) = range_hdr
                        && let Some(spec) = rh.strip_prefix("bytes=")
                        && let Some((a, b)) = spec.split_once('-')
                    {
                        let start: usize = a.parse().unwrap_or(0);
                        let end: usize = b
                            .parse::<usize>()
                            .unwrap_or(data.len() - 1)
                            .min(data.len() - 1);
                        let slice = &data[start..=end];
                        let hdr = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                            slice.len(),
                            data.len()
                        );
                        let _ = sock.write_all(hdr.as_bytes()).await;
                        let _ = sock.write_all(slice).await;
                        return;
                    }

                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                        data.len()
                    );
                    let _ = sock.write_all(hdr.as_bytes()).await;
                    if !req.starts_with("HEAD ") {
                        let _ = sock.write_all(&data).await;
                    }
                });
            }
        });

        let dir =
            std::env::temp_dir().join(format!("ferrite_http_probe_ua_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = FdmConfig::default();
        cfg.core.use_system_default_user_agent = true;
        cfg.core.set_mark_of_the_web = false;
        cfg.core.current_tum = TrafficUsageMode::Medium;

        let rec = make_test_record(format!("http://{addr}/sf.tar.xz"), &dir, "sf.tar.xz");
        let (_tx, rx) = watch::channel(ControlCommand::Run);
        let done =
            download_http_or_ftp(rec, &cfg, &[], None, BandwidthLimiter::new(0), rx, |_| {}).await;
        assert_eq!(
            done.state,
            DownloadState::Completed,
            "err={:?}",
            done.last_error
        );
        assert_eq!(
            section_403s.load(Ordering::SeqCst),
            0,
            "winning probe UA should prevent any 403 section burst"
        );
        assert!(done.segments.len() >= 4, "expected multi-section download");
        assert_eq!(std::fs::read(dir.join("sf.tar.xz")).unwrap(), *payload);

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
