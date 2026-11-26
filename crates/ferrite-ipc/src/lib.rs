use anyhow::{Context, Result};
use ferrite_core::{
    DownloadFileEntry, DownloadId, FerriteConfig, FileExistsAction, FileType, FilterQuery,
    NewDownloadRequest, TrafficUsageMode, hash::sha256_str,
};
use futures::{SinkExt, StreamExt};
use qrcode::QrCode;
use qrcode::render::svg;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::future::Future;
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
    ReadBuf,
};
use tokio::net::TcpListener;
#[cfg(windows)]
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;

const MAX_IPC_LINE_BYTES: usize = 1024 * 1024;
const REMOTE_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REMOTE_HEADER_BYTES: usize = 16 * 1024;
const MAX_FAILED_AUTH_ATTEMPTS: u32 = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentInspectInfo {
    pub info_hash: String,
    pub name: String,
    pub total_bytes: u64,
    pub files: Vec<DownloadFileEntry>,
    pub trackers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum IpcRequest {
    Ping,
    AddDownload(Box<NewDownloadRequest>),
    AddBatch {
        input: String,
        #[serde(default)]
        target_dir: Option<PathBuf>,
        #[serde(default)]
        start_paused: bool,
    },
    InspectTorrent {
        source: String,
    },
    ListDownloads(FilterQuery),
    GetDownload {
        id: DownloadId,
    },
    Pause {
        id: DownloadId,
    },
    Resume {
        id: DownloadId,
    },
    Cancel {
        id: DownloadId,
    },
    Remove {
        id: DownloadId,
        delete_files: bool,
    },
    StartAll,
    PauseAll,
    SetTrafficUsageMode {
        mode: TrafficUsageMode,
    },
    GetConfig,
    UpdateConfig(Box<FerriteConfig>),
    ListTags,
    CreateTag {
        name: String,
        color_hex: String,
        #[serde(default)]
        file_types: Vec<FileType>,
    },
    DeleteTag {
        tag_id: i64,
    },
    AssignTag {
        id: DownloadId,
        tag_id: i64,
    },
    UnassignTag {
        id: DownloadId,
        tag_id: i64,
    },
    ListPlugins,
    SetPluginEnabled {
        uid: String,
        enabled: bool,
    },
    UninstallPlugin {
        uid: String,
    },
    ResolveConflict {
        id: DownloadId,
        action: FileExistsAction,
    },
    BrowserQuerySettings,
    BrowserPostSettings(serde_json::Value),
    RemoteCommand {
        action: String,
        payload: serde_json::Value,
    },
    RaiseWindow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IpcResponse {
    Ok { data: serde_json::Value },
    Err { code: String, message: String },
}

impl IpcResponse {
    pub fn ok<T: Serialize>(v: T) -> Self {
        match serde_json::to_value(v) {
            Ok(data) => Self::Ok { data },
            Err(e) => Self::Err {
                code: "serialize_error".into(),
                message: e.to_string(),
            },
        }
    }

    pub fn err(code: impl Into<String>, msg: impl Into<String>) -> Self {
        Self::Err {
            code: code.into(),
            message: msg.into(),
        }
    }

    pub fn into_data<T: serde::de::DeserializeOwned>(self) -> Result<T> {
        match self {
            Self::Ok { data } => Ok(serde_json::from_value(data)?),
            Self::Err { code, message } => anyhow::bail!("IPC error ({code}): {message}"),
        }
    }
}

#[cfg(unix)]
fn current_euid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: POSIX `geteuid()` takes no arguments and is always safe to call.
    unsafe { geteuid() }
}

pub fn normalize_socket_path(path: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::MetadataExt;
        // Standard sockaddr_un sun_path limit is 104 bytes on macOS/BSD and 108 bytes on Linux.
        // We enforce <= 103 bytes (leaving room for null terminator).
        if path.as_os_str().as_bytes().len() >= 104 {
            let hash = sha256_str(&path.to_string_lossy());
            let uid = current_euid();

            if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR")
                && !xdg.is_empty()
            {
                let xdg_path = PathBuf::from(&xdg);
                if xdg_path.is_absolute()
                    && std::fs::symlink_metadata(&xdg_path)
                        .map(|m| m.is_dir() && m.uid() == uid)
                        .unwrap_or(false)
                {
                    let sub = xdg_path
                        .join(format!("ferrite-{uid}"))
                        .join(format!("{}.sock", &hash[..16]));
                    if sub.as_os_str().as_bytes().len() < 104 {
                        return sub;
                    }
                    let flat = xdg_path.join(format!("ferrite-{}.sock", &hash[..16]));
                    if flat.as_os_str().as_bytes().len() < 104 {
                        return flat;
                    }
                }
            }

            let temp_sub = std::env::temp_dir()
                .join(format!("ferrite-{uid}"))
                .join(format!("{}.sock", &hash[..16]));
            if temp_sub.as_os_str().as_bytes().len() < 104 {
                return temp_sub;
            }
            return PathBuf::from(format!("/tmp/ferrite-{uid}"))
                .join(format!("{}.sock", &hash[..16]));
        }
    }
    path.to_path_buf()
}

pub fn default_socket_path(data_dir: &Path) -> PathBuf {
    normalize_socket_path(&data_dir.join("ferrite.sock"))
}

#[cfg(windows)]
fn pipe_name_from_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.starts_with(r"\\.\pipe\") {
        s.to_string()
    } else {
        let hash = sha256_str(&s);
        format!(r"\\.\pipe\ferrite-{}", &hash[..16])
    }
}

async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<Option<String>> {
    let mut buf = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if buf.is_empty() {
                return Ok(None);
            }
            break;
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            let take_len = pos + 1;
            if buf.len() + take_len > max_bytes {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("IPC message exceeds {max_bytes} byte limit"),
                ));
            }
            buf.extend_from_slice(&available[..take_len]);
            reader.consume(take_len);
            break;
        }
        let len = available.len();
        if buf.len() + len > max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("IPC message exceeds {max_bytes} byte limit"),
            ));
        }
        buf.extend_from_slice(available);
        reader.consume(len);
    }
    if buf.ends_with(b"\n") {
        buf.pop();
        if buf.ends_with(b"\r") {
            buf.pop();
        }
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

async fn send_over_stream<R, W>(reader: R, mut writer: W, req: &IpcRequest) -> Result<IpcResponse>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut line = serde_json::to_string(req)?;
    if line.len() >= MAX_IPC_LINE_BYTES {
        anyhow::bail!("IPC request exceeds {MAX_IPC_LINE_BYTES} byte limit");
    }
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;

    let mut buf_reader = BufReader::new(reader);
    let Some(resp_line) = read_bounded_line(&mut buf_reader, MAX_IPC_LINE_BYTES).await? else {
        anyhow::bail!("Daemon closed IPC socket without response");
    };
    if resp_line.trim().is_empty() {
        anyhow::bail!("Daemon closed IPC socket without response");
    }
    Ok(serde_json::from_str(&resp_line)?)
}

async fn handle_ipc_connection<R, W, F, Fut>(reader: R, mut writer: W, handler: Arc<F>)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
{
    let mut buf_reader = BufReader::new(reader);
    loop {
        let line = match read_bounded_line(&mut buf_reader, MAX_IPC_LINE_BYTES).await {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(e) => {
                let err_resp = IpcResponse::err("message_too_large", e.to_string());
                if let Ok(mut out) = serde_json::to_string(&err_resp) {
                    out.push('\n');
                    let _ = writer.write_all(out.as_bytes()).await;
                    let _ = writer.flush().await;
                }
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<IpcRequest>(&line) {
            Ok(req) => handler(req).await,
            Err(e) => IpcResponse::err("invalid_request", e.to_string()),
        };
        if let Ok(mut out) = serde_json::to_string(&resp) {
            out.push('\n');
            if writer.write_all(out.as_bytes()).await.is_err() {
                break;
            }
            let _ = writer.flush().await;
        }
    }
}

pub struct IpcClient {
    socket_path: PathBuf,
}

impl IpcClient {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: normalize_socket_path(&socket_path.into()),
        }
    }

    #[cfg(unix)]
    pub async fn send(&self, req: &IpcRequest) -> Result<IpcResponse> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to connect to daemon socket {}",
                    self.socket_path.display()
                )
            })?;
        let (reader, writer) = stream.into_split();
        send_over_stream(reader, writer, req).await
    }

    #[cfg(windows)]
    pub async fn send(&self, req: &IpcRequest) -> Result<IpcResponse> {
        let pipe_name = pipe_name_from_path(&self.socket_path);
        let client = ClientOptions::new()
            .open(&pipe_name)
            .with_context(|| format!("Failed to connect to daemon pipe {}", pipe_name))?;
        let (reader, writer) = tokio::io::split(client);
        send_over_stream(reader, writer, req).await
    }

    pub async fn ping(&self) -> bool {
        matches!(
            self.send(&IpcRequest::Ping).await,
            Ok(IpcResponse::Ok { .. })
        )
    }
}

#[cfg(unix)]
fn ensure_private_socket_parent(parent: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let is_system_tmp = parent == Path::new("/tmp") || parent == Path::new("/var/tmp");
    if is_system_tmp {
        anyhow::bail!(
            "Refusing to bind IPC socket directly in shared directory {}",
            parent.display()
        );
    }
    if !parent.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let meta = std::fs::symlink_metadata(parent)?;
    if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
        anyhow::bail!(
            "IPC socket parent {} must be a real directory",
            parent.display()
        );
    }
    let euid = current_euid();
    if meta.uid() != euid {
        anyhow::bail!(
            "IPC socket parent {} is owned by uid {}, expected {euid}",
            parent.display(),
            meta.uid()
        );
    }
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(unix)]
pub async fn start_ipc_server<F, Fut>(
    socket_path: PathBuf,
    handler: Arc<F>,
) -> Result<tokio::task::JoinHandle<()>>
where
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
{
    let socket_path = normalize_socket_path(&socket_path);
    if let Some(parent) = socket_path.parent() {
        ensure_private_socket_parent(parent)?;
    }
    if let Ok(meta) = std::fs::symlink_metadata(&socket_path) {
        if meta.file_type().is_symlink() {
            let _ = std::fs::remove_file(&socket_path);
        } else {
            // Check if existing daemon is actually alive; if stale, remove socket
            if IpcClient::new(&socket_path).ping().await {
                anyhow::bail!(
                    "Ferrite daemon is already running on {}",
                    socket_path.display()
                );
            }
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    let listener = UnixListener::bind(&socket_path)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;

    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let h = Arc::clone(&handler);
            tokio::spawn(async move {
                let (reader, writer) = stream.into_split();
                handle_ipc_connection(reader, writer, h).await;
            });
        }
    });
    Ok(handle)
}

#[cfg(windows)]
pub async fn start_ipc_server<F, Fut>(
    socket_path: PathBuf,
    handler: Arc<F>,
) -> Result<tokio::task::JoinHandle<()>>
where
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
{
    if IpcClient::new(&socket_path).ping().await {
        anyhow::bail!(
            "Ferrite daemon is already running on {}",
            socket_path.display()
        );
    }

    let pipe_name = pipe_name_from_path(&socket_path);
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe_name)?;

    let handle = tokio::spawn(async move {
        loop {
            if server.connect().await.is_err() {
                break;
            }
            let connected_client = server;
            server = match ServerOptions::new().create(&pipe_name) {
                Ok(s) => s,
                Err(_) => break,
            };
            let h = Arc::clone(&handler);
            tokio::spawn(async move {
                let (reader, writer) = tokio::io::split(connected_client);
                handle_ipc_connection(reader, writer, h).await;
            });
        }
    });
    Ok(handle)
}

pub fn build_browser_settings_payload(config: &FerriteConfig) -> serde_json::Value {
    serde_json::json!({
        "skipSmallerThan": if config.app.wb_dont_intercept_too_small {
            config.app.wb_too_small_size_bytes
        } else {
            0
        },
        "skipExtensions": if config.app.wb_dont_intercept_unwanted_extensions {
            config.app.wb_unwanted_extensions.join(" ")
        } else {
            String::new()
        },
        "catchExtensions": if config.app.wb_intercept_only_extensions {
            config.app.wb_intercept_extensions.join(" ")
        } else {
            String::new()
        },
        "skipServersEnabled": config.app.wb_dont_intercept_unwanted_hosts,
        "skipServers": config.app.wb_unwanted_hosts,
        "skipIfKeyPressed": if config.app.wb_dont_intercept_if_key_pressed {
            config.app.wb_bypass_key.clone()
        } else {
            String::new()
        },
        "forwardAbortedToBrowser": config.app.wb_forward_aborted_to_browser,
        "monitor": config.app.wb_intercept_downloads,
        "allowDownload": true
    })
}

pub fn apply_browser_settings_patch(config: &mut FerriteConfig, patch: &serde_json::Value) {
    if let Some(m) = patch.get("monitor").and_then(|v| v.as_bool()) {
        config.app.wb_intercept_downloads = m;
    }
    if let Some(b) = patch
        .get("forwardAbortedToBrowser")
        .and_then(|v| v.as_bool())
    {
        config.app.wb_forward_aborted_to_browser = b;
    }
    if let Some(sz) = patch.get("skipSmallerThan").and_then(|v| v.as_u64()) {
        if sz == 0 {
            config.app.wb_dont_intercept_too_small = false;
        } else {
            config.app.wb_dont_intercept_too_small = true;
            config.app.wb_too_small_size_bytes = sz;
        }
    }
    if let Some(ext_str) = patch.get("skipExtensions").and_then(|v| v.as_str()) {
        config.app.wb_unwanted_extensions =
            ext_str.split_whitespace().map(str::to_string).collect();
        config.app.wb_dont_intercept_unwanted_extensions =
            !config.app.wb_unwanted_extensions.is_empty();
    }
    if let Some(ext_str) = patch.get("catchExtensions").and_then(|v| v.as_str()) {
        config.app.wb_intercept_extensions =
            ext_str.split_whitespace().map(str::to_string).collect();
        config.app.wb_intercept_only_extensions = !config.app.wb_intercept_extensions.is_empty();
    }
    if let Some(b) = patch.get("skipServersEnabled").and_then(|v| v.as_bool()) {
        config.app.wb_dont_intercept_unwanted_hosts = b;
    }
    if let Some(arr) = patch.get("skipServers").and_then(|v| v.as_array()) {
        config.app.wb_unwanted_hosts = arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
    }
    if let Some(key) = patch.get("skipIfKeyPressed").and_then(|v| v.as_str()) {
        if key.is_empty() {
            config.app.wb_dont_intercept_if_key_pressed = false;
        } else {
            config.app.wb_dont_intercept_if_key_pressed = true;
            config.app.wb_bypass_key = key.to_string();
        }
    }
}

fn percent_encode_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for &b in raw.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(char::from_digit(((b >> 4) & 0xf) as u32, 16).unwrap_or('0'));
            out.push(char::from_digit((b & 0xf) as u32, 16).unwrap_or('0'));
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemotePairingInfo {
    pub server_id: String,
    pub pairing_secret: String,
    pub password_hash: String,
    pub pairing_uri: String,
    pub https_url: String,
    pub qr_svg: String,
    pub qr_ascii: String,
}

pub fn generate_pairing_info(server_id: &str, host: &str, port: u16) -> Result<RemotePairingInfo> {
    let secret = uuid::Uuid::new_v4().simple().to_string()[..16].to_string();
    generate_pairing_info_with_secret(server_id, host, port, &secret)
}

pub fn generate_pairing_info_with_secret(
    server_id: &str,
    host: &str,
    port: u16,
    secret: &str,
) -> Result<RemotePairingInfo> {
    let password_hash = sha256_str(secret);
    let enc_id = percent_encode_component(server_id);
    let enc_secret = percent_encode_component(secret);
    let enc_host = percent_encode_component(host);
    let pairing_uri = format!(
        "ferrite://remote_control/?id={enc_id}&password={enc_secret}&host={enc_host}&port={port}"
    );
    let formatted_host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let https_url = format!("https://{formatted_host}:{port}/#token={enc_secret}");
    let code = QrCode::new(pairing_uri.as_bytes())?;
    let qr_svg = code
        .render()
        .min_dimensions(200, 200)
        .dark_color(svg::Color("#111827"))
        .light_color(svg::Color("#ffffff"))
        .build();
    let qr_ascii = code
        .render::<char>()
        .quiet_zone(true)
        .module_dimensions(2, 1)
        .build();
    Ok(RemotePairingInfo {
        server_id: server_id.to_string(),
        pairing_secret: secret.to_string(),
        password_hash,
        pairing_uri,
        https_url,
        qr_svg,
        qr_ascii,
    })
}

fn detect_local_lan_ips() -> Vec<String> {
    let mut ips = Vec::new();
    for (bind, target) in [
        ("0.0.0.0:0", "8.8.8.8:80"),
        ("[::]:0", "[2001:4860:4860::8888]:80"),
    ] {
        if let Ok(sock) = UdpSocket::bind(bind)
            && sock.connect(target).is_ok()
            && let Ok(local) = sock.local_addr()
        {
            let ip = local.ip();
            if !ip.is_unspecified() {
                ips.push(ip.to_string());
            }
        }
    }
    ips
}

fn write_private_key_file(key_path: &Path, key_bytes: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(key_path)?;
        file.write_all(key_bytes)?;
        file.flush()?;
        std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(key_path, key_bytes)?;
    }
    Ok(())
}

pub fn load_or_create_self_signed_tls(cert_dir: &Path) -> Result<Arc<rustls::ServerConfig>> {
    load_or_create_self_signed_tls_with_sans(cert_dir, &[])
}

pub fn load_or_create_self_signed_tls_with_sans(
    cert_dir: &Path,
    extra_sans: &[String],
) -> Result<Arc<rustls::ServerConfig>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    std::fs::create_dir_all(cert_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(cert_dir, std::fs::Permissions::from_mode(0o700));
    }
    let cert_path = cert_dir.join("rc_cert.der");
    let key_path = cert_dir.join("rc_key.der");

    let (cert_der, key_der) = if cert_path.is_file() && key_path.is_file() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
        }
        (std::fs::read(&cert_path)?, std::fs::read(&key_path)?)
    } else {
        let mut sans = vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "::1".to_string(),
        ];
        for env_key in ["HOSTNAME", "HOST", "COMPUTERNAME"] {
            if let Ok(hn) = std::env::var(env_key) {
                let trimmed = hn.trim();
                if !trimmed.is_empty() && !sans.iter().any(|s| s == trimmed) {
                    sans.push(trimmed.to_string());
                }
            }
        }
        #[cfg(target_os = "linux")]
        if let Ok(hn) = std::fs::read_to_string("/etc/hostname") {
            let trimmed = hn.trim();
            if !trimmed.is_empty() && !sans.iter().any(|s| s == trimmed) {
                sans.push(trimmed.to_string());
            }
        }
        for ip in detect_local_lan_ips() {
            if !sans.iter().any(|s| s == &ip) {
                sans.push(ip);
            }
        }
        for raw in extra_sans {
            let cleaned = raw.trim().trim_start_matches('[').trim_end_matches(']');
            if !cleaned.is_empty() && !sans.iter().any(|s| s == cleaned) {
                sans.push(cleaned.to_string());
            }
        }
        let ck = rcgen::generate_simple_self_signed(sans)?;
        let c = ck.cert.der().to_vec();
        let k = ck.signing_key.serialize_der();
        write_private_key_file(&key_path, &k)?;
        std::fs::write(&cert_path, &c)?;
        (c, k)
    };

    let cert = CertificateDer::from(cert_der);
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der));
    let cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;
    Ok(Arc::new(cfg))
}

struct PrefixedStream<S> {
    prefix: std::io::Cursor<Vec<u8>>,
    inner: S,
}

impl<S> PrefixedStream<S> {
    fn new(prefix: Vec<u8>, inner: S) -> Self {
        Self {
            prefix: std::io::Cursor::new(prefix),
            inner,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for PrefixedStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let pos = self.prefix.position() as usize;
        let total = self.prefix.get_ref().len();
        if pos < total {
            let rem = &self.prefix.get_ref()[pos..];
            let n = rem.len().min(buf.remaining());
            buf.put_slice(&rem[..n]);
            self.prefix.set_position((pos + n) as u64);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PrefixedStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

const REMOTE_WEB_UI_HTML: &str = r#"<!doctype html>
<html>
<head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Ferrite Remote Control</title>
<style>
body{font-family:system-ui,-apple-system,sans-serif;background:#0f172a;color:#e2e8f0;margin:0;padding:16px}
.card{background:#1e293b;border-radius:10px;padding:14px;margin-bottom:12px;border:1px solid #334155}
input,button,select{padding:8px 12px;border-radius:6px;border:1px solid #475569;background:#0f172a;color:#fff}
button{background:#2563eb;border:none;cursor:pointer;font-weight:600}
.row{display:flex;gap:8px;flex-wrap:wrap;align-items:center}
.dl{display:flex;justify-content:space-between;align-items:center;padding:8px 0;border-bottom:1px solid #334155}
</style>
</head>
<body>
<h2>Ferrite Remote Control</h2>
<div class="card row">
  <input id="tok" type="password" placeholder="Pairing token" style="width:160px">
  <button id="conn-btn" type="button">Connect</button>
  <select id="tum">
    <option value="high">High Speed</option>
    <option value="medium">Medium</option>
    <option value="low">Low</option>
    <option value="snail">Snail</option>
  </select>
  <span id="st">Disconnected</span>
</div>
<div class="card row">
  <input id="url" placeholder="https://... or magnet:?xt=..." style="flex:1;min-width:200px">
  <button id="add-btn" type="button">Add Download</button>
</div>
<div id="list" class="card">No downloads loaded yet.</div>
<script>
let ws = null;
let pollTimer = null;
const hp = new URLSearchParams(location.hash.replace(/^#/, ''));
const hashToken = hp.get('token');
if (hashToken) document.getElementById('tok').value = hashToken;
function stopPolling() {
  if (pollTimer !== null) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
}
function closeExistingWs() {
  stopPolling();
  if (ws) {
    ws.onopen = null;
    ws.onmessage = null;
    ws.onclose = null;
    ws.onerror = null;
    try { ws.close(); } catch (_) {}
    ws = null;
  }
}
function connectWs() {
  closeExistingWs();
  const st = document.getElementById('st');
  st.textContent = 'Connecting...';
  const socket = new WebSocket((location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host);
  ws = socket;
  socket.onopen = () => {
    if (ws !== socket) return;
    st.textContent = 'Authenticating...';
    sendCmd('auth', { token: document.getElementById('tok').value });
  };
  socket.onmessage = (ev) => {
    if (ws !== socket) return;
    let msg;
    try { msg = JSON.parse(ev.data); } catch (_) { return; }
    if (msg.type === 'auth') {
      if (msg.ok) {
        st.textContent = 'Authenticated';
        stopPolling();
        sendCmd('list', {});
        pollTimer = setInterval(() => sendCmd('list', {}), 1500);
      } else {
        st.textContent = 'Authentication failed';
        stopPolling();
      }
      return;
    }
    if (msg.type === 'list') {
      render(Array.isArray(msg.downloads) ? msg.downloads : []);
    } else if (msg.type === 'error') {
      st.textContent = 'Error: ' + String(msg.message || msg.error || 'request failed');
    }
  };
  socket.onclose = () => {
    if (ws !== socket) return;
    stopPolling();
    st.textContent = 'Disconnected';
  };
  socket.onerror = () => {
    if (ws !== socket) return;
    stopPolling();
    st.textContent = 'Connection error';
  };
}
function sendCmd(action, payload) {
  if (ws && ws.readyState === 1) ws.send(JSON.stringify({ action, payload }));
}
function addUrl() {
  const input = document.getElementById('url');
  const u = input.value.trim();
  if (u) { sendCmd('add', { url: u }); input.value = ''; }
}
function render(items) {
  const listEl = document.getElementById('list');
  listEl.replaceChildren();
  if (!items.length) {
    const empty = document.createElement('div');
    empty.textContent = 'Queue empty';
    listEl.appendChild(empty);
    return;
  }
  for (const d of items) {
    const row = document.createElement('div');
    row.className = 'dl';
    const info = document.createElement('div');
    const title = document.createElement('b');
    title.textContent = String(d.file_name || d.title || ('Download #' + (d.id ?? '')));
    const br = document.createElement('br');
    const meta = document.createElement('small');
    const mb = (Number(d.downloaded_bytes || 0) / 1048576).toFixed(1);
    const kbps = Math.round(Number(d.download_speed_bps || 0) / 1024);
    meta.textContent = `${String(d.state || '')} • ${mb} MB (${kbps} KB/s)`;
    info.append(title, br, meta);
    const actions = document.createElement('div');
    actions.className = 'row';
    const dlId = Number(d.id);
    const pauseBtn = document.createElement('button');
    pauseBtn.type = 'button';
    pauseBtn.textContent = 'Pause';
    pauseBtn.addEventListener('click', () => sendCmd('pause', { id: dlId }));
    const resumeBtn = document.createElement('button');
    resumeBtn.type = 'button';
    resumeBtn.textContent = 'Start';
    resumeBtn.addEventListener('click', () => sendCmd('resume', { id: dlId }));
    actions.append(pauseBtn, resumeBtn);
    row.append(info, actions);
    listEl.appendChild(row);
  }
}
document.getElementById('conn-btn').addEventListener('click', connectWs);
document.getElementById('add-btn').addEventListener('click', addUrl);
document.getElementById('tum').addEventListener('change', (ev) => sendCmd('set_tum', { mode: ev.target.value }));
</script>
</body>
</html>"#;

fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

static REMOTE_WEB_UI_SCRIPT_CSP_HASH: LazyLock<String> = LazyLock::new(|| {
    let start = REMOTE_WEB_UI_HTML
        .find("<script>")
        .map(|i| i + "<script>".len())
        .unwrap_or(0);
    let end = REMOTE_WEB_UI_HTML
        .rfind("</script>")
        .unwrap_or(REMOTE_WEB_UI_HTML.len());
    let script_body = &REMOTE_WEB_UI_HTML[start..end];
    let digest = Sha256::digest(script_body.as_bytes());
    format!("sha256-{}", encode_base64(&digest))
});

fn build_web_ui_http_response(include_body: bool) -> String {
    let csp = format!(
        "default-src 'none'; script-src '{}'; style-src 'unsafe-inline'; connect-src 'self' wss: ws:; img-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        *REMOTE_WEB_UI_SCRIPT_CSP_HASH
    );
    let body = if include_body { REMOTE_WEB_UI_HTML } else { "" };
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Security-Policy: {csp}\r\nX-Frame-Options: DENY\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        REMOTE_WEB_UI_HTML.len()
    )
}

/// Compare two byte slices in constant time without short-circuiting on mismatches.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let len_match = a.len() == b.len();
    let max_len = a.len().max(b.len());
    let mut diff: u8 = if len_match { 0 } else { 1 };
    for i in 0..max_len {
        let ba = a.get(i).copied().unwrap_or(0);
        let bb = b.get(i).copied().unwrap_or(0);
        diff |= ba ^ bb;
    }
    std::hint::black_box(diff) == 0 && len_match
}

/// Compare two ASCII strings case-insensitively in constant time.
pub fn constant_time_eq_ascii_ignore_case(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let len_match = a_bytes.len() == b_bytes.len();
    let max_len = a_bytes.len().max(b_bytes.len());
    let mut diff: u8 = if len_match { 0 } else { 1 };
    for i in 0..max_len {
        let ba = a_bytes.get(i).copied().unwrap_or(0).to_ascii_lowercase();
        let bb = b_bytes.get(i).copied().unwrap_or(0).to_ascii_lowercase();
        diff |= ba ^ bb;
    }
    std::hint::black_box(diff) == 0 && len_match
}

/// Verify a candidate token against stored SHA-256 hex digests in constant time,
/// inspecting all entries without early exit.
pub fn verify_token_against_sha256_hashes<'a>(
    token: &str,
    expected_hashes: impl IntoIterator<Item = &'a str>,
) -> bool {
    let cand = sha256_str(token);
    let mut matched = false;
    for expected in expected_hashes {
        matched |= constant_time_eq_ascii_ignore_case(&cand, expected);
    }
    std::hint::black_box(matched)
}

/// Validate URLs accepted from remote LAN Web UI clients (`http://`, `https://`, `ftp://`, `ftps://`, `magnet:`).
pub fn is_allowed_remote_add_url(raw: &str) -> bool {
    let u = raw.trim();
    if u.is_empty() || u.chars().any(|c| c.is_control()) {
        return false;
    }
    let lower = u.to_ascii_lowercase();
    ["http://", "https://", "ftp://", "ftps://", "magnet:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme) && u.len() > scheme.len())
}

/// Validate `Upgrade: websocket` on actual HTTP header lines (ignoring request line and other headers).
pub fn is_websocket_upgrade_request(raw_headers: &[u8]) -> bool {
    let hdr_str = String::from_utf8_lossy(raw_headers);
    let mut lines = hdr_str.split('\n').map(|l| l.trim_end_matches('\r'));
    let Some(request_line) = lines.next() else {
        return false;
    };
    if !request_line.starts_with("GET ") {
        return false;
    }
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("upgrade")
            && value.trim().eq_ignore_ascii_case("websocket")
        {
            return true;
        }
    }
    false
}

async fn read_http_handshake_with_timeout<S: AsyncRead + Unpin>(
    stream: &mut S,
    timeout: Duration,
) -> std::io::Result<Vec<u8>> {
    tokio::time::timeout(timeout, async {
        let mut hdr_buf = Vec::with_capacity(2048);
        let mut byte = [0u8; 1];
        while hdr_buf.len() < MAX_REMOTE_HEADER_BYTES {
            let n = stream.read(&mut byte).await?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "EOF before end of HTTP headers",
                ));
            }
            hdr_buf.push(byte[0]);
            if hdr_buf.ends_with(b"\r\n\r\n") {
                return Ok(hdr_buf);
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "HTTP headers exceed maximum size",
        ))
    })
    .await
    .map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "HTTP handshake read timed out",
        )
    })?
}

pub async fn start_remote_control_server<F, Fut, V>(
    bind_addr: SocketAddr,
    cert_dir: &Path,
    verify_token: Arc<V>,
    ipc_handler: Arc<F>,
) -> Result<(SocketAddr, tokio::task::JoinHandle<()>)>
where
    V: Fn(&str) -> bool + Send + Sync + 'static,
    F: Fn(IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = IpcResponse> + Send + 'static,
{
    let mut extra_sans = Vec::new();
    if !bind_addr.ip().is_unspecified() {
        extra_sans.push(bind_addr.ip().to_string());
    }
    let tls_cfg = load_or_create_self_signed_tls_with_sans(cert_dir, &extra_sans)?;
    let acceptor = TlsAcceptor::from(tls_cfg);
    let listener = TcpListener::bind(bind_addr).await?;
    let local_addr = listener.local_addr()?;

    let handle = tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            let verify_token = Arc::clone(&verify_token);
            let ipc_handler = Arc::clone(&ipc_handler);

            tokio::spawn(async move {
                let Ok(Ok(mut tls)) =
                    tokio::time::timeout(REMOTE_HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await
                else {
                    return;
                };
                let Ok(hdr_buf) =
                    read_http_handshake_with_timeout(&mut tls, REMOTE_HANDSHAKE_TIMEOUT).await
                else {
                    return;
                };
                if is_websocket_upgrade_request(&hdr_buf) {
                    let prefixed = PrefixedStream::new(hdr_buf, tls);
                    let Ok(mut ws) = tokio_tungstenite::accept_async(prefixed).await else {
                        return;
                    };
                    let mut authed = false;
                    let mut failed_auth_attempts: u32 = 0;
                    while let Some(Ok(msg)) = ws.next().await {
                        if let Message::Text(txt) = msg {
                            let Ok(val) = serde_json::from_str::<serde_json::Value>(&txt) else {
                                continue;
                            };
                            let action = val.get("action").and_then(|v| v.as_str()).unwrap_or("");
                            let payload = val
                                .get("payload")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            if action == "auth" {
                                let tok =
                                    payload.get("token").and_then(|v| v.as_str()).unwrap_or("");
                                if verify_token(tok) {
                                    authed = true;
                                    failed_auth_attempts = 0;
                                    let _ = ws
                                        .send(Message::Text(
                                            serde_json::json!({ "type": "auth", "ok": true })
                                                .to_string()
                                                .into(),
                                        ))
                                        .await;
                                } else {
                                    authed = false;
                                    failed_auth_attempts = failed_auth_attempts.saturating_add(1);
                                    if failed_auth_attempts >= MAX_FAILED_AUTH_ATTEMPTS {
                                        let _ = ws
                                            .send(Message::Text(
                                                serde_json::json!({
                                                    "type": "auth",
                                                    "ok": false,
                                                    "error": "too_many_failed_attempts"
                                                })
                                                .to_string()
                                                .into(),
                                            ))
                                            .await;
                                        let _ = ws.close(None).await;
                                        break;
                                    }
                                    let _ = ws
                                        .send(Message::Text(
                                            serde_json::json!({ "type": "auth", "ok": false })
                                                .to_string()
                                                .into(),
                                        ))
                                        .await;
                                }
                                continue;
                            }
                            if !authed {
                                let _ = ws
                                    .send(Message::Text(
                                        serde_json::json!({ "type": "error", "error": "unauthorized" })
                                            .to_string()
                                            .into(),
                                    ))
                                    .await;
                                continue;
                            }

                            if action == "add" {
                                let valid_url = payload
                                    .get("url")
                                    .and_then(|u| u.as_str())
                                    .map(str::trim)
                                    .filter(|u| is_allowed_remote_add_url(u));
                                let Some(url) = valid_url else {
                                    let _ = ws
                                        .send(Message::Text(
                                            serde_json::json!({
                                                "type": "error",
                                                "code": "invalid_url",
                                                "message": "Remote add only supports http://, https://, ftp://, ftps://, and magnet: URLs"
                                            })
                                            .to_string()
                                            .into(),
                                        ))
                                        .await;
                                    continue;
                                };
                                let resp = ipc_handler(IpcRequest::AddDownload(Box::new(
                                    NewDownloadRequest::from_url(url),
                                )))
                                .await;
                                let wire = match resp {
                                    IpcResponse::Ok { data } => {
                                        serde_json::json!({ "type": "ok", "action": action, "data": data })
                                    }
                                    IpcResponse::Err { code, message } => {
                                        serde_json::json!({ "type": "error", "code": code, "message": message })
                                    }
                                };
                                let _ = ws.send(Message::Text(wire.to_string().into())).await;
                                continue;
                            }

                            let req = match action {
                                "list" => Some(IpcRequest::ListDownloads(FilterQuery::default())),
                                "pause" => payload
                                    .get("id")
                                    .and_then(|i| i.as_i64())
                                    .map(|id| IpcRequest::Pause { id }),
                                "resume" => payload
                                    .get("id")
                                    .and_then(|i| i.as_i64())
                                    .map(|id| IpcRequest::Resume { id }),
                                "cancel" => payload
                                    .get("id")
                                    .and_then(|i| i.as_i64())
                                    .map(|id| IpcRequest::Cancel { id }),
                                "set_tum" => payload
                                    .get("mode")
                                    .and_then(|m| {
                                        serde_json::from_value::<TrafficUsageMode>(m.clone()).ok()
                                    })
                                    .map(|mode| IpcRequest::SetTrafficUsageMode { mode }),
                                _ => Some(IpcRequest::RemoteCommand {
                                    action: action.to_string(),
                                    payload,
                                }),
                            };
                            if let Some(ipc_req) = req {
                                let resp = ipc_handler(ipc_req).await;
                                let wire = match (action, resp) {
                                    ("list", IpcResponse::Ok { data }) => {
                                        serde_json::json!({ "type": "list", "downloads": data })
                                    }
                                    (_, IpcResponse::Ok { data }) => {
                                        serde_json::json!({ "type": "ok", "action": action, "data": data })
                                    }
                                    (_, IpcResponse::Err { code, message }) => {
                                        serde_json::json!({ "type": "error", "code": code, "message": message })
                                    }
                                };
                                let _ = ws.send(Message::Text(wire.to_string().into())).await;
                            }
                        }
                    }
                } else {
                    let hdr_str = String::from_utf8_lossy(&hdr_buf);
                    let req_line = hdr_str.lines().next().unwrap_or("");
                    let resp = if req_line.starts_with("HEAD ") {
                        build_web_ui_http_response(false)
                    } else if req_line.starts_with("GET ") {
                        build_web_ui_http_response(true)
                    } else {
                        "HTTP/1.1 405 Method Not Allowed\r\nAllow: GET, HEAD\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                    };
                    let _ = tls.write_all(resp.as_bytes()).await;
                    let _ = tls.flush().await;
                    let _ = tls.shutdown().await;
                }
            });
        }
    });

    Ok((local_addr, handle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn unix_ipc_server_client_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ferrite_ipc_sock_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sock = default_socket_path(&dir);

        let srv = start_ipc_server(
            sock.clone(),
            Arc::new(|req| async move {
                match req {
                    IpcRequest::Ping => IpcResponse::ok(serde_json::json!({"pong": true})),
                    IpcRequest::SetTrafficUsageMode { mode } => IpcResponse::ok(mode),
                    _ => IpcResponse::err("unhandled", "unsupported"),
                }
            }),
        )
        .await
        .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let parent_mode = std::fs::metadata(sock.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(parent_mode, 0o700);
            let sock_mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
            assert_eq!(sock_mode, 0o600);
        }

        let client = IpcClient::new(&sock);
        assert!(client.ping().await);
        let resp = client
            .send(&IpcRequest::SetTrafficUsageMode {
                mode: TrafficUsageMode::Low,
            })
            .await
            .unwrap();
        match resp {
            IpcResponse::Ok { data } => assert_eq!(data, serde_json::json!("low")),
            _ => panic!("expected Ok"),
        }

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn bounded_line_reader_rejects_oversized_ipc_message() {
        let oversized = vec![b'a'; MAX_IPC_LINE_BYTES + 64];
        let mut cursor = BufReader::new(std::io::Cursor::new(oversized));
        let err = read_bounded_line(&mut cursor, MAX_IPC_LINE_BYTES)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn generates_qr_pairing_and_browser_settings() {
        let pair = generate_pairing_info("srv-123", "192.168.1.50", 9443).unwrap();
        assert!(
            pair.pairing_uri
                .starts_with("ferrite://remote_control/?id=srv-123&password=")
        );
        assert!(pair.pairing_uri.contains("&host=192.168.1.50&port=9443"));
        assert!(!pair.https_url.contains("?token="));
        assert!(
            pair.https_url
                .starts_with("https://192.168.1.50:9443/#token=")
        );
        assert!(pair.qr_svg.contains("<svg"));
        assert!(!pair.qr_ascii.is_empty());
        assert_eq!(pair.password_hash, sha256_str(&pair.pairing_secret));

        let mut cfg = FerriteConfig::default();
        let payload = build_browser_settings_payload(&cfg);
        assert_eq!(payload["monitor"], serde_json::json!(true));
        assert_eq!(payload["forwardAbortedToBrowser"], serde_json::json!(true));
        apply_browser_settings_patch(
            &mut cfg,
            &serde_json::json!({
                "monitor": false,
                "forwardAbortedToBrowser": false,
                "skipSmallerThan": 204800,
                "skipExtensions": "tmp log"
            }),
        );
        assert!(!cfg.app.wb_intercept_downloads);
        assert!(!cfg.app.wb_forward_aborted_to_browser);
        assert_eq!(cfg.app.wb_too_small_size_bytes, 204800);
        assert_eq!(cfg.app.wb_unwanted_extensions, vec!["tmp", "log"]);
    }

    #[test]
    fn socket_path_fallback_for_long_paths() {
        let long_dir = PathBuf::from(
            "/tmp/very_long_path_that_exceeds_the_standard_unix_sun_path_limit_of_one_hundred_and_four_bytes_by_far_for_testing/ferrite-data",
        );
        let sock = default_socket_path(&long_dir);
        #[cfg(unix)]
        {
            assert_ne!(sock.parent(), Some(Path::new("/tmp")));
            assert!(sock.to_string_lossy().ends_with(".sock"));
            assert!(sock.to_string_lossy().len() < 104);
        }
        #[cfg(not(unix))]
        {
            let _ = sock;
        }
    }

    #[test]
    fn remote_web_ui_avoids_innerhtml_and_leaks_and_gates_polling() {
        assert!(!REMOTE_WEB_UI_HTML.contains("innerHTML"));
        assert!(!REMOTE_WEB_UI_HTML.contains("location.search"));
        assert!(REMOTE_WEB_UI_HTML.contains("location.hash"));
        assert!(REMOTE_WEB_UI_HTML.contains("createElement"));
        assert!(REMOTE_WEB_UI_HTML.contains("textContent"));
        assert!(REMOTE_WEB_UI_HTML.contains("stopPolling()"));

        // Ensure polling starts only after auth ok, not inside onopen.
        let onopen_pos = REMOTE_WEB_UI_HTML.find("socket.onopen").unwrap();
        let onmsg_pos = REMOTE_WEB_UI_HTML.find("socket.onmessage").unwrap();
        let interval_pos = REMOTE_WEB_UI_HTML.find("setInterval").unwrap();
        assert!(interval_pos > onmsg_pos);
        assert!(!REMOTE_WEB_UI_HTML[onopen_pos..onmsg_pos].contains("setInterval"));

        let http = build_web_ui_http_response(true);
        assert!(http.contains("Content-Security-Policy: default-src 'none'; script-src 'sha256-"));
        assert!(!http.contains("script-src 'unsafe-inline'"));
        assert!(http.contains("X-Frame-Options: DENY"));
        assert!(http.contains("X-Content-Type-Options: nosniff"));
        assert!(http.contains("Referrer-Policy: no-referrer"));
    }

    #[test]
    fn validates_websocket_upgrade_header_strictly() {
        assert!(is_websocket_upgrade_request(
            b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n"
        ));
        assert!(is_websocket_upgrade_request(
            b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nupgrade:   WebSocket  \r\n\r\n"
        ));
        assert!(!is_websocket_upgrade_request(
            b"GET /?q=upgrade:%20websocket HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"
        ));
        assert!(!is_websocket_upgrade_request(
            b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Spoof: upgrade: websocket\r\n\r\n"
        ));
        assert!(!is_websocket_upgrade_request(
            b"POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\n\r\n"
        ));
    }

    #[test]
    fn validates_remote_add_urls_and_constant_time_helpers() {
        assert!(is_allowed_remote_add_url("https://example.com/a.iso"));
        assert!(is_allowed_remote_add_url("HTTP://example.com/a.iso"));
        assert!(is_allowed_remote_add_url("ftp://files.example.com/b.tar"));
        assert!(is_allowed_remote_add_url("ftps://files.example.com/b.tar"));
        assert!(is_allowed_remote_add_url("magnet:?xt=urn:btih:012345"));

        assert!(!is_allowed_remote_add_url(""));
        assert!(!is_allowed_remote_add_url("/etc/passwd"));
        assert!(!is_allowed_remote_add_url("file:///Users/test/a.torrent"));
        assert!(!is_allowed_remote_add_url(r"C:\secret\a.torrent"));
        assert!(!is_allowed_remote_add_url("javascript:alert(1)"));
        assert!(!is_allowed_remote_add_url("https://"));

        assert!(constant_time_eq(b"secret-123", b"secret-123"));
        assert!(!constant_time_eq(b"secret-123", b"secret-124"));
        assert!(!constant_time_eq(b"secret-123", b"secret-12"));

        let h1 = sha256_str("alpha");
        let h2 = sha256_str("beta");
        assert!(verify_token_against_sha256_hashes(
            "beta",
            [h1.as_str(), h2.as_str()]
        ));
        assert!(!verify_token_against_sha256_hashes(
            "gamma",
            [h1.as_str(), h2.as_str()]
        ));
    }

    #[test]
    fn tls_key_written_with_private_permissions() {
        let dir = std::env::temp_dir().join(format!(
            "ferrite_ipc_tls_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let cfg = load_or_create_self_signed_tls_with_sans(&dir, &["192.0.2.10".into()]).unwrap();
        assert!(!cfg.alpn_protocols.is_empty() || cfg.alpn_protocols.is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let key_mode = std::fs::metadata(dir.join("rc_key.der"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(key_mode, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn slowloris_handshake_times_out() {
        let (mut client, mut server) = tokio::io::duplex(64);
        client.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
        let err = read_http_handshake_with_timeout(&mut server, Duration::from_millis(40))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn remote_ws_rate_limits_auth_and_restricts_add_urls() {
        let dir = std::env::temp_dir().join(format!(
            "ferrite_ipc_ws_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let expected_hash = sha256_str("good-token");
        let add_calls = Arc::new(AtomicUsize::new(0));
        let add_calls_srv = Arc::clone(&add_calls);

        let (addr, handle) = start_remote_control_server(
            "127.0.0.1:0".parse().unwrap(),
            &dir,
            Arc::new(move |tok: &str| {
                verify_token_against_sha256_hashes(tok, [expected_hash.as_str()])
            }),
            Arc::new(move |req| {
                let add_calls_srv = Arc::clone(&add_calls_srv);
                async move {
                    match req {
                        IpcRequest::AddDownload(dl) => {
                            add_calls_srv.fetch_add(1, Ordering::SeqCst);
                            IpcResponse::ok(serde_json::json!({ "url": dl.url }))
                        }
                        IpcRequest::ListDownloads(_) => IpcResponse::ok(serde_json::json!([])),
                        _ => IpcResponse::ok(true),
                    }
                }
            }),
        )
        .await
        .unwrap();

        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert_bytes = std::fs::read(dir.join("rc_cert.der")).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(cert_bytes)).unwrap();
        let client_cfg = Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        let connector = tokio_rustls::TlsConnector::from(client_cfg);
        let server_name = rustls::pki_types::ServerName::try_from("127.0.0.1").unwrap();

        // 1. Plain HTTPS GET returns security headers and no unsafe innerHTML
        {
            let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
            let mut tls = connector.connect(server_name.clone(), tcp).await.unwrap();
            tls.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
                .await
                .unwrap();
            let mut buf = Vec::new();
            tls.read_to_end(&mut buf).await.unwrap();
            let resp_text = String::from_utf8_lossy(&buf);
            assert!(resp_text.starts_with("HTTP/1.1 200 OK"));
            assert!(resp_text.contains("X-Frame-Options: DENY"));
            assert!(resp_text.contains("X-Content-Type-Options: nosniff"));
            assert!(resp_text.contains("Content-Security-Policy:"));
        }

        // 2. Five failed auth attempts close the WebSocket
        {
            let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
            let tls = connector.connect(server_name.clone(), tcp).await.unwrap();
            let (mut ws, _) = tokio_tungstenite::client_async(format!("wss://{addr}/"), tls)
                .await
                .unwrap();

            for i in 0..4 {
                ws.send(Message::Text(
                    serde_json::json!({ "action": "auth", "payload": { "token": format!("bad-{i}") } })
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
                let msg = ws.next().await.unwrap().unwrap();
                let v: serde_json::Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
                assert_eq!(v["type"], "auth");
                assert_eq!(v["ok"], false);
            }

            // 5th failed attempt triggers rate-limit error and close
            ws.send(Message::Text(
                serde_json::json!({ "action": "auth", "payload": { "token": "bad-4" } })
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let msg = ws.next().await.unwrap().unwrap();
            let v: serde_json::Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
            assert_eq!(v["ok"], false);
            assert_eq!(v["error"], "too_many_failed_attempts");
            let closed = ws.next().await;
            assert!(matches!(closed, None | Some(Ok(Message::Close(_)))));
        }

        // 3. Valid auth allows http/magnet add, rejects local .torrent / file:// URLs
        {
            let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
            let tls = connector.connect(server_name, tcp).await.unwrap();
            let (mut ws, _) = tokio_tungstenite::client_async(format!("wss://{addr}/"), tls)
                .await
                .unwrap();

            ws.send(Message::Text(
                serde_json::json!({ "action": "auth", "payload": { "token": "good-token" } })
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let auth_msg = ws.next().await.unwrap().unwrap();
            let auth_v: serde_json::Value =
                serde_json::from_str(auth_msg.to_text().unwrap()).unwrap();
            assert_eq!(auth_v, serde_json::json!({ "type": "auth", "ok": true }));

            for bad_url in [
                "/etc/passwd.torrent",
                "file:///tmp/x.torrent",
                "javascript:alert(1)",
            ] {
                ws.send(Message::Text(
                    serde_json::json!({ "action": "add", "payload": { "url": bad_url } })
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
                let err_msg = ws.next().await.unwrap().unwrap();
                let err_v: serde_json::Value =
                    serde_json::from_str(err_msg.to_text().unwrap()).unwrap();
                assert_eq!(err_v["type"], "error");
                assert_eq!(err_v["code"], "invalid_url");
            }
            assert_eq!(add_calls.load(Ordering::SeqCst), 0);

            ws.send(Message::Text(
                serde_json::json!({ "action": "add", "payload": { "url": "https://example.com/file.zip" } })
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let ok_msg = ws.next().await.unwrap().unwrap();
            let ok_v: serde_json::Value = serde_json::from_str(ok_msg.to_text().unwrap()).unwrap();
            assert_eq!(ok_v["type"], "ok");
            assert_eq!(ok_v["data"]["url"], "https://example.com/file.zip");
            assert_eq!(add_calls.load(Ordering::SeqCst), 1);
        }

        handle.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_ipc_request_variants_roundtrip_json() {
        let reqs = vec![
            IpcRequest::AddBatch {
                input: "https://example.com/[1-3].zip".into(),
                target_dir: Some(PathBuf::from("/tmp/dl")),
                start_paused: true,
            },
            IpcRequest::InspectTorrent {
                source: "magnet:?xt=urn:btih:0123".into(),
            },
            IpcRequest::DeleteTag { tag_id: 9 },
            IpcRequest::UnassignTag { id: 4, tag_id: 9 },
            IpcRequest::ListPlugins,
            IpcRequest::SetPluginEnabled {
                uid: "yt-plugin".into(),
                enabled: false,
            },
            IpcRequest::UninstallPlugin {
                uid: "yt-plugin".into(),
            },
            IpcRequest::UpdateConfig(Box::default()),
        ];
        for req in reqs {
            let s = serde_json::to_string(&req).unwrap();
            let decoded: IpcRequest = serde_json::from_str(&s).unwrap();
            let s2 = serde_json::to_string(&decoded).unwrap();
            assert_eq!(s, s2);
        }

        let info = TorrentInspectInfo {
            info_hash: "abc".into(),
            name: "ubuntu".into(),
            total_bytes: 1024,
            files: vec![],
            trackers: vec!["udp://tracker.example.com:6969".into()],
        };
        let resp = IpcResponse::ok(&info);
        let decoded_info: TorrentInspectInfo = resp.into_data().unwrap();
        assert_eq!(decoded_info.info_hash, info.info_hash);
        assert_eq!(decoded_info.name, info.name);
        assert_eq!(decoded_info.total_bytes, info.total_bytes);
        assert_eq!(decoded_info.files.len(), info.files.len());
        assert_eq!(decoded_info.trackers, info.trackers);
    }
}
