use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub type DownloadId = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadType {
    Www,
    BitTorrent,
    HlsM3u8,
    Dash,
    MediaParser,
    Batch,
    YoutubeChannel,
    Unknown,
}

impl DownloadType {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Www => "HTTP/FTP",
            Self::BitTorrent => "BitTorrent",
            Self::HlsM3u8 => "HLS Stream",
            Self::Dash => "DASH Stream",
            Self::MediaParser => "Media",
            Self::Batch => "Batch",
            Self::YoutubeChannel => "Channel",
            Self::Unknown => "Other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Stopped,
    Queued,
    Starting,
    Downloading,
    Checking,
    Paused,
    PostProcessing,
    Seeding,
    Completed,
    Error,
    WaitingForNetwork,
    Scheduled,
    Moving,
}

impl DownloadState {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Starting
                | Self::Downloading
                | Self::Checking
                | Self::PostProcessing
                | Self::Moving
        )
    }

    pub fn is_running_or_seeding(&self) -> bool {
        self.is_active() || matches!(self, Self::Seeding)
    }

    pub fn is_stopped_or_paused(&self) -> bool {
        matches!(
            self,
            Self::Stopped | Self::Paused | Self::Error | Self::Scheduled
        )
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Stopped => "Stopped",
            Self::Queued => "Queued",
            Self::Starting => "Starting",
            Self::Downloading => "Downloading",
            Self::Checking => "Checking",
            Self::Paused => "Paused",
            Self::PostProcessing => "Post-processing",
            Self::Seeding => "Seeding",
            Self::Completed => "Completed",
            Self::Error => "Error",
            Self::WaitingForNetwork => "Waiting for network",
            Self::Scheduled => "Scheduled",
            Self::Moving => "Moving files",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum DownloadPriority {
    DontDownload = 0,
    Low = 1,
    #[default]
    Normal = 2,
    High = 3,
    Unknown = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ResumeSupport {
    #[default]
    Unknown,
    Present,
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum FileType {
    Audio,
    Video,
    Archive,
    Document,
    Program,
    Image,
    Torrent,
    Other,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HashAlgorithm {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl HashAlgorithm {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Md5 => "MD5",
            Self::Sha1 => "SHA-1",
            Self::Sha256 => "SHA-256",
            Self::Sha512 => "SHA-512",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ExistingFileReaction {
    Ask,
    Overwrite,
    #[default]
    Rename,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileExistsAction {
    Rename,
    Overwrite,
    Abort,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum DownloadAbortReason {
    #[default]
    None,
    AbortedByUser,
    AlreadyExists,
    InvalidUrl,
    NetworkError,
    UnwantedError,
    SlowDownload,
    MissingFiles,
    RemoteResourceChanged,
    LowBattery,
    NoAllowedNetwork,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum AutoStartPreventer {
    #[default]
    None,
    NotOnline,
    LowBattery,
    NetworkRoaming,
    NoAllowedNetworkInterface,
    LowDiskSpace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum MissingFilesFilter {
    #[default]
    Off,
    AcceptNormal,
    AcceptMissingFiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum SortField {
    #[default]
    CreationTime,
    Order,
    Size,
    Status,
    Title,
    Progress,
    Priority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TrafficUsageMode {
    Snail,
    Low,
    Medium,
    #[default]
    High,
}

impl TrafficUsageMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Snail => "Snail",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum NetworkProxyMode {
    NoProxy,
    #[default]
    SystemProxy,
    ManualProxy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnownWebBrowser {
    Chrome,
    Firefox,
    Edge,
    Vivaldi,
    Opera,
    Brave,
    Safari,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ShutdownType {
    #[default]
    None,
    Sleep,
    Hibernate,
    Shutdown,
    QuitApp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationEvent {
    DownloadAdded,
    DownloadComplete,
    DownloadsComplete,
    DownloadFailed,
    DownloadsFailed,
    NoActiveDownloads,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum StateFilter {
    #[default]
    All,
    Running,
    Downloading,
    Uploading,
    Finished,
    Stopped,
    Scheduled,
    Missing,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadFlags {
    pub sequential_play_asap: bool,
    pub allow_change_source_url: bool,
    pub supports_mirrors: bool,
    pub supports_restart: bool,
    pub can_detect_remote_changes: bool,
    pub endless_stream: bool,
    pub delete_service_files_when_done: bool,
    pub silent_mode: bool,
    pub is_missing_files: bool,
    pub created_by_batch: bool,
    pub bt_disable_seeding: bool,
    pub bt_ignore_upload_ratio: bool,
    pub disable_auto_retry: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadFileEntry {
    pub index: usize,
    pub relative_path: String,
    pub size_bytes: u64,
    pub downloaded_bytes: u64,
    pub priority: DownloadPriority,
    pub media_ready_to_play: bool,
    pub expected_hash: Option<(HashAlgorithm, String)>,
    pub computed_hash: Option<(HashAlgorithm, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentState {
    Pending,
    Active,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentProgress {
    pub id: u32,
    pub start: u64,
    pub end: u64,
    pub downloaded: u64,
    pub state: SegmentState,
    pub speed_bps: u64,
    pub mirror_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub address: String,
    pub client: String,
    pub download_bps: u64,
    pub upload_bps: u64,
    pub progress: f32,
    pub flags: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackerInfo {
    pub url: String,
    pub status: String,
    pub peers: u32,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HttpRequestOptions {
    pub url: String,
    pub original_url: Option<String>,
    pub referrer: Option<String>,
    pub document_url: Option<String>,
    pub user_agent: Option<String>,
    pub cookies: Option<String>,
    pub post_data: Option<String>,
    pub custom_headers: BTreeMap<String, String>,
    pub mirrors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRule {
    pub enabled: bool,
    /// Bitmask where bit 0 = Monday .. bit 6 = Sunday (127 = every day)
    pub days_mask: u8,
    /// Minutes since midnight (0..=1439)
    pub start_minute: u16,
    /// Minutes since midnight (0..=1440), may wrap past midnight if end < start
    pub end_minute: u16,
}

impl Default for ScheduleRule {
    fn default() -> Self {
        Self {
            enabled: false,
            days_mask: 0b0111_1111,
            start_minute: 0,
            end_minute: 360, // 00:00 -> 06:00 default window
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadRecord {
    pub id: DownloadId,
    pub uuid: String,
    pub parent_id: Option<DownloadId>,
    pub children_ids: Vec<DownloadId>,
    pub order_index: i64,
    pub download_type: DownloadType,
    pub state: DownloadState,
    pub priority: DownloadPriority,
    pub resume_support: ResumeSupport,
    pub flags: DownloadFlags,
    pub title: String,
    pub url: String,
    pub target_dir: PathBuf,
    pub file_name: String,
    pub partial_suffix: String,
    pub total_bytes: Option<u64>,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub download_speed_bps: u64,
    pub upload_speed_bps: u64,
    pub eta_secs: Option<u64>,
    pub connections_active: u32,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub file_type: FileType,
    pub http: HttpRequestOptions,
    pub torrent_info_hash: Option<String>,
    pub torrent_source: Option<String>,
    pub tags: Vec<i64>,
    pub files: Vec<DownloadFileEntry>,
    pub segments: Vec<SegmentProgress>,
    pub peers: Vec<PeerInfo>,
    pub trackers: Vec<TrackerInfo>,
    pub schedule: Option<ScheduleRule>,
    pub retry_count: u32,
    pub forbid_auto_retry_count: u32,
    pub last_error: Option<String>,
    pub abort_reason: DownloadAbortReason,
    pub preview_path: Option<PathBuf>,
    pub module_data: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl DownloadRecord {
    pub fn output_path(&self) -> PathBuf {
        self.target_dir
            .join(crate::routing::sanitize_filename(&self.file_name))
    }

    pub fn partial_path(&self) -> PathBuf {
        let safe_name = crate::routing::sanitize_filename(&self.file_name);
        let safe_suffix = crate::routing::sanitize_partial_suffix(&self.partial_suffix);
        if safe_suffix.is_empty() {
            self.target_dir.join(safe_name)
        } else {
            self.target_dir.join(format!("{safe_name}{safe_suffix}"))
        }
    }

    pub fn progress_fraction(&self) -> f64 {
        match self.total_bytes {
            Some(total) if total > 0 => {
                (self.downloaded_bytes as f64 / total as f64).clamp(0.0, 1.0)
            }
            _ => {
                if self.state == DownloadState::Completed {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Tag {
    pub id: i64,
    pub name: String,
    pub color_hex: String,
    pub is_builtin: bool,
    pub file_types: Vec<FileType>,
    pub default_extensions: Vec<String>,
    pub default_download_folder: Option<PathBuf>,
}

impl Default for Tag {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            color_hex: "#2983FE".into(),
            is_builtin: false,
            file_types: Vec::new(),
            default_extensions: Vec::new(),
            default_download_folder: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyEndpoint {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxySettings {
    pub mode: NetworkProxyMode,
    pub http: ProxyEndpoint,
    pub https: ProxyEndpoint,
    pub socks5: ProxyEndpoint,
}

impl Default for ProxySettings {
    fn default() -> Self {
        Self {
            mode: NetworkProxyMode::SystemProxy,
            http: ProxyEndpoint::default(),
            https: ProxyEndpoint::default(),
            socks5: ProxyEndpoint::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SslException {
    pub host_port: String,
    pub sha256_fingerprint: String,
    pub added_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthCredential {
    pub scope: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FilterQuery {
    pub state_filter: StateFilter,
    pub tag_id: Option<i64>,
    pub missing_filter: MissingFilesFilter,
    pub search_text: String,
    pub sort_by: SortField,
    pub sort_ascending: bool,
    pub only_top_level: bool,
    pub parent_id_filter: Option<DownloadId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewDownloadRequest {
    pub url: String,
    pub title: Option<String>,
    pub target_dir: Option<PathBuf>,
    pub file_name: Option<String>,
    pub download_type: Option<DownloadType>,
    pub priority: DownloadPriority,
    pub existing_file_reaction: Option<ExistingFileReaction>,
    pub http: HttpRequestOptions,
    pub tags: Vec<i64>,
    pub schedule: Option<ScheduleRule>,
    pub silent: bool,
    pub start_paused: bool,
    pub sequential_play_asap: bool,
    pub selected_file_indices: Option<Vec<usize>>,
    pub parent_id: Option<DownloadId>,
    pub torrent_bytes_b64: Option<String>,
}

impl NewDownloadRequest {
    pub fn from_url(url: impl Into<String>) -> Self {
        let u = url.into();
        Self {
            http: HttpRequestOptions {
                url: u.clone(),
                ..Default::default()
            },
            url: u,
            title: None,
            target_dir: None,
            file_name: None,
            download_type: None,
            priority: DownloadPriority::Normal,
            existing_file_reaction: None,
            tags: Vec::new(),
            schedule: None,
            silent: false,
            start_paused: false,
            sequential_play_asap: false,
            selected_file_indices: None,
            parent_id: None,
            torrent_bytes_b64: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EngineEvent {
    Added {
        id: DownloadId,
    },
    Updated {
        id: DownloadId,
    },
    Removed {
        id: DownloadId,
    },
    Completed {
        id: DownloadId,
    },
    Failed {
        id: DownloadId,
        error: String,
    },
    FileConflict {
        id: DownloadId,
        path: PathBuf,
    },
    TrafficModeChanged {
        mode: TrafficUsageMode,
    },
    SettingsUpdated,
    QuitRequested,
    QueueStats {
        total_download_bps: u64,
        total_upload_bps: u64,
        active_count: usize,
        preventer: AutoStartPreventer,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(target_dir: PathBuf, file_name: &str, partial_suffix: &str) -> DownloadRecord {
        DownloadRecord {
            id: 1,
            uuid: "u1".into(),
            parent_id: None,
            children_ids: vec![],
            order_index: 1,
            download_type: DownloadType::Www,
            state: DownloadState::Stopped,
            priority: DownloadPriority::Normal,
            resume_support: ResumeSupport::Unknown,
            flags: DownloadFlags::default(),
            title: "sample".into(),
            url: "https://example.org/file.bin".into(),
            target_dir,
            file_name: file_name.into(),
            partial_suffix: partial_suffix.into(),
            total_bytes: None,
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            download_speed_bps: 0,
            upload_speed_bps: 0,
            eta_secs: None,
            connections_active: 0,
            etag: None,
            last_modified: None,
            file_type: FileType::Unknown,
            http: HttpRequestOptions::default(),
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
            created_at: Utc::now(),
            updated_at: Utc::now(),
            completed_at: None,
        }
    }

    #[test]
    fn output_and_partial_paths_prevent_path_traversal() {
        let target = PathBuf::from("/tmp/ferrite-downloads");

        let normal = sample_record(target.clone(), "ubuntu.iso", ".part");
        assert_eq!(normal.output_path(), target.join("ubuntu.iso"));
        assert_eq!(normal.partial_path(), target.join("ubuntu.iso.part"));

        let no_suffix = sample_record(target.clone(), "ubuntu.iso", "");
        assert_eq!(no_suffix.partial_path(), no_suffix.output_path());

        for (mal_name, mal_suffix) in [
            ("../../etc/passwd", "/../../evil"),
            ("/etc/shadow", "../.part"),
            ("..\\..\\Windows\\cmd.exe", "..\\..\\bad"),
            ("subdir/../secret.txt", ".."),
            ("..", "."),
            (".", "/etc/passwd"),
            ("../../", "/"),
        ] {
            let rec = sample_record(target.clone(), mal_name, mal_suffix);
            let out = rec.output_path();
            let part = rec.partial_path();
            assert_eq!(
                out.parent(),
                Some(target.as_path()),
                "output_path escaped for ({mal_name:?}, {mal_suffix:?}): {out:?}"
            );
            assert_eq!(
                part.parent(),
                Some(target.as_path()),
                "partial_path escaped for ({mal_name:?}, {mal_suffix:?}): {part:?}"
            );
            assert!(
                !out.file_name().unwrap().to_string_lossy().contains(".."),
                "unexpected '..' in output filename: {out:?}"
            );
            assert!(
                !part.file_name().unwrap().to_string_lossy().contains(".."),
                "unexpected '..' in partial filename: {part:?}"
            );
        }
    }

    #[test]
    fn proxy_settings_deserializes_with_removed_legacy_fields() {
        let json = r#"{
            "mode": "manual_proxy",
            "http": { "host": "127.0.0.1", "port": 8080 },
            "ftp": { "host": "127.0.0.1", "port": 2121 },
            "dont_use_for_remote_access": false,
            "dont_use_for_remote_access_listener": false
        }"#;
        let proxy: ProxySettings = serde_json::from_str(json).unwrap();
        assert_eq!(proxy.mode, NetworkProxyMode::ManualProxy);
        assert_eq!(proxy.http.host, "127.0.0.1");
        assert_eq!(proxy.http.port, 8080);
        assert_eq!(proxy.socks5.port, 0);
    }
}
