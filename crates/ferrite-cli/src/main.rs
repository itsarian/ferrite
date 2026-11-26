use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use ferrite_bt::BtSessionManager;
use ferrite_cli::DownloadEngine;
use ferrite_core::{
    DownloadPriority, DownloadRecord, DownloadState, ExistingFileReaction, FerriteConfig,
    FilterQuery, HashAlgorithm, KnownWebBrowser, NetworkProxyMode, NewDownloadRequest,
    ProxyEndpoint, StateFilter, Tag, TrafficUsageMode, hash::compute_file_hash,
};
use ferrite_ipc::{IpcClient, IpcRequest, IpcResponse, TorrentInspectInfo, default_socket_path};
use ferrite_media::{JsPluginInfo, convert_to_mp3, convert_to_mp4, ffprobe_inspect};
use ferrite_native_host::{default_native_manifest_dir, install_native_manifest_to_dir};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "ferrite",
    version,
    about = "Ferrite — Fast, telemetry-free native download manager"
)]
struct Cli {
    /// Start minimized / background daemon mode
    #[arg(long)]
    hidden: bool,

    /// Store database, config, and plugins next to the executable
    #[arg(long)]
    portable: bool,

    /// Override application data directory
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,

    /// Optional URL or magnet/torrent path when invoked without subcommand
    #[arg(value_name = "URL")]
    direct_url: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum CliPriority {
    Low,
    Normal,
    High,
}

impl From<CliPriority> for DownloadPriority {
    fn from(value: CliPriority) -> Self {
        match value {
            CliPriority::Low => DownloadPriority::Low,
            CliPriority::Normal => DownloadPriority::Normal,
            CliPriority::High => DownloadPriority::High,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum CliTum {
    Snail,
    Low,
    Medium,
    High,
}

impl From<CliTum> for TrafficUsageMode {
    fn from(value: CliTum) -> Self {
        match value {
            CliTum::Snail => TrafficUsageMode::Snail,
            CliTum::Low => TrafficUsageMode::Low,
            CliTum::Medium => TrafficUsageMode::Medium,
            CliTum::High => TrafficUsageMode::High,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum CliHashAlgo {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl From<CliHashAlgo> for HashAlgorithm {
    fn from(value: CliHashAlgo) -> Self {
        match value {
            CliHashAlgo::Md5 => HashAlgorithm::Md5,
            CliHashAlgo::Sha1 => HashAlgorithm::Sha1,
            CliHashAlgo::Sha256 => HashAlgorithm::Sha256,
            CliHashAlgo::Sha512 => HashAlgorithm::Sha512,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum CliBrowser {
    Chrome,
    Firefox,
    Brave,
    Edge,
    Vivaldi,
    Opera,
    All,
}

#[derive(Subcommand)]
enum Commands {
    /// Run background engine + Unix IPC socket + optional Remote Control server
    Daemon {
        /// Enable LAN HTTPS/WebSocket Remote Control server
        #[arg(long)]
        remote_control: bool,
        #[arg(long)]
        remote_host: Option<String>,
    },
    /// Add a single HTTP, FTP, HLS, DASH, Media, or Torrent/Magnet download
    Add {
        url: String,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        silent: bool,
        #[arg(long)]
        paused: bool,
        #[arg(long, value_enum, default_value_t = CliPriority::Normal)]
        priority: CliPriority,
        #[arg(long)]
        referer: Option<String>,
        #[arg(long)]
        cookies: Option<String>,
        #[arg(long)]
        user_agent: Option<String>,
        #[arg(long = "header")]
        headers: Vec<String>,
        #[arg(long = "mirror")]
        mirrors: Vec<String>,
        #[arg(long)]
        play_asap: bool,
        /// Block until download reaches a terminal state
        #[arg(long)]
        wait: bool,
    },
    /// Expand a wildcard batch pattern (e.g. `https://ex.com/[01-10].zip`) or file
    Batch {
        pattern: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        paused: bool,
        #[arg(long)]
        preview_only: bool,
    },
    /// Inspect or add a .torrent / magnet download with optional file selection
    Torrent {
        source: String,
        #[arg(long)]
        inspect: bool,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        play_asap: bool,
        /// Comma-separated 0-based file indices to download
        #[arg(long)]
        select_files: Option<String>,
    },
    /// List downloads
    List {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        search: Option<String>,
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        tag_id: Option<i64>,
    },
    Pause {
        id: i64,
    },
    Resume {
        id: i64,
    },
    Cancel {
        id: i64,
    },
    Remove {
        id: i64,
        #[arg(long)]
        delete_files: bool,
    },
    StartAll,
    PauseAll,
    /// Switch Traffic Usage Mode (`snail`, `low`, `medium`, `high`)
    Tum {
        #[arg(value_enum)]
        mode: CliTum,
    },
    /// Manage tags
    Tags {
        #[command(subcommand)]
        action: TagCommands,
    },
    /// View or modify core configuration settings
    Settings {
        #[command(subcommand)]
        action: SettingsCommands,
    },
    /// Manage JavaScript media parser plugins
    Plugins {
        #[command(subcommand)]
        action: PluginCommands,
    },
    /// Compute or verify cryptographic hash (MD5, SHA-1, SHA-256, SHA-512)
    Hash {
        path: PathBuf,
        #[arg(long, value_enum, default_value_t = CliHashAlgo::Sha256)]
        algo: CliHashAlgo,
        #[arg(long)]
        expect: Option<String>,
    },
    /// Extract / convert media to MP3 via FFmpeg
    Mp3 {
        input: PathBuf,
        output: PathBuf,
        #[arg(long, default_value_t = 192)]
        bitrate: u32,
    },
    /// Remux / transcode media to MP4 via FFmpeg
    Mp4 {
        input: PathBuf,
        output: PathBuf,
        #[arg(long)]
        transcode: bool,
    },
    /// Export downloads list to JSON
    Export {
        output: PathBuf,
    },
    /// Import downloads list (Ferrite JSON or URL list)
    Import {
        input: PathBuf,
        #[arg(long)]
        paused: bool,
    },
    /// Import settings bundle
    ImportSettings {
        input: PathBuf,
    },
    /// Export settings bundle with SHA-256 verification
    ExportSettings {
        output: PathBuf,
    },
    /// Print QR pairing code & start or display Remote Control URL
    RemoteControl {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },
    /// Install NativeMessagingHosts manifest (`org.ferrite.cnh.json`)
    NativeHostInstall {
        #[arg(long, value_enum, default_value_t = CliBrowser::Chrome)]
        browser: CliBrowser,
        #[arg(long)]
        host_exe: Option<PathBuf>,
        /// Optional override directory instead of writing browser system folder
        #[arg(long)]
        target_dir: Option<PathBuf>,
    },
    /// Run offline loopback self-verification suite in an isolated temp dir
    SelfTest,
}

#[derive(Subcommand)]
enum TagCommands {
    List,
    Create {
        name: String,
        #[arg(long, default_value = "#3B82F6")]
        color: String,
    },
    Delete {
        tag_id: i64,
    },
    Assign {
        download_id: i64,
        tag_id: i64,
    },
    Unassign {
        download_id: i64,
        tag_id: i64,
    },
}

#[derive(Subcommand)]
enum SettingsCommands {
    Show {
        /// Include sensitive credentials (remote access and proxy passwords)
        #[arg(long)]
        show_secrets: bool,
    },
    SetDownloadDir {
        path: PathBuf,
    },
    SetMaxConcurrent {
        #[arg(value_enum)]
        mode: CliTum,
        count: u32,
    },
    SetExistingReaction {
        reaction: String,
    },
    SetProxy {
        mode: String,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
    },
}

#[derive(Subcommand)]
enum PluginCommands {
    List,
    Enable { uid: String },
    Disable { uid: String },
    Uninstall { uid: String },
}

fn parse_state_filter(s: Option<&str>) -> StateFilter {
    match s.unwrap_or("all").to_ascii_lowercase().as_str() {
        "running" => StateFilter::Running,
        "downloading" => StateFilter::Downloading,
        "uploading" | "seeding" => StateFilter::Uploading,
        "finished" | "completed" => StateFilter::Finished,
        "stopped" | "paused" => StateFilter::Stopped,
        "scheduled" => StateFilter::Scheduled,
        "missing" => StateFilter::Missing,
        _ => StateFilter::All,
    }
}

async fn try_daemon_ipc(data_dir: &Path, req: IpcRequest) -> Option<IpcResponse> {
    let sock = default_socket_path(data_dir);
    let client = IpcClient::new(&sock);
    if !client.ping().await {
        return None;
    }
    client.send(&req).await.ok()
}

fn print_records_table(items: &[ferrite_core::DownloadRecord]) {
    if items.is_empty() {
        println!("No downloads.");
        return;
    }
    println!(
        "{:<4} {:<10} {:<15} {:<8} {:<11} {:<10} TITLE",
        "ID", "TYPE", "STATE", "PROG", "SIZE", "SPEED"
    );
    for d in items {
        let pct = format!("{:.0}%", d.progress_fraction() * 100.0);
        let size = d
            .total_bytes
            .map(DownloadEngine::format_human_bytes)
            .unwrap_or_else(|| DownloadEngine::format_human_bytes(d.downloaded_bytes));
        let spd = if d.download_speed_bps > 0 {
            format!(
                "{}/s",
                DownloadEngine::format_human_bytes(d.download_speed_bps)
            )
        } else {
            "-".into()
        };
        println!(
            "{:<4} {:<10} {:<15} {:<8} {:<11} {:<10} {}",
            d.id,
            d.download_type.display_name(),
            d.state.display_name(),
            pct,
            size,
            spd,
            d.title
        );
    }
}

fn print_torrent_inspect(info: &TorrentInspectInfo) {
    println!("Name:      {}", info.name);
    println!("InfoHash:  {}", info.info_hash);
    println!(
        "Size:      {}",
        DownloadEngine::format_human_bytes(info.total_bytes)
    );
    println!("Trackers:  {}", info.trackers.len());
    for f in &info.files {
        println!(
            "  [{}] {} ({})",
            f.index,
            f.relative_path,
            DownloadEngine::format_human_bytes(f.size_bytes)
        );
    }
}

fn format_settings_output(cfg: &FerriteConfig, show_secrets: bool) -> Result<String> {
    let display_cfg = if show_secrets {
        cfg.clone()
    } else {
        DownloadEngine::redact_config_secrets(cfg.clone())
    };
    Ok(serde_json::to_string_pretty(&display_cfg)?)
}

fn apply_settings_mutation(cfg: &mut FerriteConfig, action: SettingsCommands) -> String {
    match action {
        SettingsCommands::Show { .. } => String::new(),
        SettingsCommands::SetDownloadDir { path } => {
            cfg.core.default_download_path = path.clone();
            format!("Default download dir updated to {}", path.display())
        }
        SettingsCommands::SetMaxConcurrent { mode, count } => {
            let m: TrafficUsageMode = mode.into();
            if let Some(prof) = cfg.core.tum_profiles.get_mut(&m) {
                prof.max_concurrent_downloads = count.max(1);
            }
            format!(
                "Updated max concurrent for {} to {}",
                m.label(),
                count.max(1)
            )
        }
        SettingsCommands::SetExistingReaction { reaction } => {
            cfg.core.existing_file_reaction = match reaction.to_ascii_lowercase().as_str() {
                "ask" => ExistingFileReaction::Ask,
                "overwrite" => ExistingFileReaction::Overwrite,
                "skip" => ExistingFileReaction::Skip,
                _ => ExistingFileReaction::Rename,
            };
            "Updated existing file reaction.".to_string()
        }
        SettingsCommands::SetProxy { mode, host, port } => {
            cfg.app.proxy.mode = match mode.to_ascii_lowercase().as_str() {
                "none" | "no_proxy" => NetworkProxyMode::NoProxy,
                "manual" | "manual_proxy" => NetworkProxyMode::ManualProxy,
                _ => NetworkProxyMode::SystemProxy,
            };
            if let Some(h) = host {
                let ep = ProxyEndpoint {
                    host: h,
                    port: port.unwrap_or(8080),
                    ..Default::default()
                };
                cfg.app.proxy.http = ep.clone();
                cfg.app.proxy.https = ep;
            }
            "Proxy settings updated.".to_string()
        }
    }
}

fn print_tags(tags: &[Tag]) {
    for t in tags {
        println!(
            "{:>4}  {:<14}  {}  builtin={}",
            t.id, t.name, t.color_hex, t.is_builtin
        );
    }
}

fn print_plugins(plugins_dir: &Path, plugs: &[JsPluginInfo]) {
    if plugs.is_empty() {
        println!(
            "No JS parser plugins installed in {}",
            plugins_dir.display()
        );
    } else {
        for p in plugs {
            println!(
                "{}  {} v{}  enabled={}  domains={:?}",
                p.uid, p.name, p.version, p.enabled, p.domains
            );
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let data_dir = cli
        .data_dir
        .clone()
        .unwrap_or_else(|| FerriteConfig::data_dir(cli.portable));
    run_cli(cli, &data_dir).await
}

async fn run_cli(cli: Cli, data_dir: &Path) -> Result<()> {
    if let Some(url) = cli.direct_url {
        let mut req = NewDownloadRequest::from_url(url);
        req.silent = cli.hidden;
        if let Some(IpcResponse::Ok { data }) =
            try_daemon_ipc(data_dir, IpcRequest::AddDownload(Box::new(req.clone()))).await
        {
            println!(
                "Sent to running Ferrite daemon: #{} {}",
                data["id"], data["file_name"]
            );
            return Ok(());
        }
        let eng = DownloadEngine::open(data_dir, false)?;
        let _sup = eng.spawn_supervisor();
        let rec = eng.add_download(req).await?;
        println!("Added #{} {}", rec.id, rec.file_name);
        wait_until_terminal(&eng, rec.id).await;
        return Ok(());
    }

    let Some(cmd) = cli.command else {
        if cli.hidden {
            run_daemon(data_dir, false, "127.0.0.1").await?;
            return Ok(());
        }
        println!(
            "Ferrite CLI & Engine (data dir: {})\nRun `ferrite --help` for commands.",
            data_dir.display()
        );
        return Ok(());
    };

    match cmd {
        Commands::Daemon {
            remote_control,
            remote_host,
        } => {
            run_daemon(
                data_dir,
                remote_control,
                remote_host.as_deref().unwrap_or("127.0.0.1"),
            )
            .await?;
        }
        Commands::Add {
            url,
            dir,
            name,
            silent,
            paused,
            priority,
            referer,
            cookies,
            user_agent,
            headers,
            mirrors,
            play_asap,
            wait,
        } => {
            let mut req = NewDownloadRequest::from_url(&url);
            req.target_dir = dir;
            req.file_name = name.clone();
            req.title = name;
            req.silent = silent;
            req.start_paused = paused;
            req.priority = priority.into();
            req.sequential_play_asap = play_asap;
            req.http.referrer = referer;
            req.http.cookies = cookies;
            req.http.user_agent = user_agent;
            req.http.mirrors = mirrors;
            for h in headers {
                if let Some((k, v)) = h.split_once(':') {
                    req.http
                        .custom_headers
                        .insert(k.trim().to_string(), v.trim().to_string());
                }
            }

            if !wait
                && let Some(IpcResponse::Ok { data }) =
                    try_daemon_ipc(data_dir, IpcRequest::AddDownload(Box::new(req.clone()))).await
            {
                println!("Queued on daemon: #{} ({})", data["id"], data["file_name"]);
                return Ok(());
            }

            let eng = DownloadEngine::open(data_dir, false)?;
            let _sup = eng.spawn_supervisor();
            let rec = eng.add_download(req).await?;
            println!(
                "Added download #{}: {} -> {}",
                rec.id,
                rec.file_name,
                rec.target_dir.display()
            );
            if wait || !paused {
                wait_until_terminal(&eng, rec.id).await;
            }
        }
        Commands::Batch {
            pattern,
            file,
            dir,
            paused,
            preview_only,
        } => {
            let text = if let Some(p) = file {
                std::fs::read_to_string(&p)?
            } else if let Some(pat) = pattern {
                pat
            } else {
                bail!("Provide either a batch pattern or --file <PATH>");
            };
            let urls = if text.contains('\n') {
                ferrite_batch::parse_batch_text_input(&text)?
            } else {
                ferrite_batch::expand_batch_pattern(&text)?
            };
            if preview_only {
                for u in &urls {
                    println!("{u}");
                }
                println!("Total: {} URL(s)", urls.len());
                return Ok(());
            }
            if let Some(resp) = try_daemon_ipc(
                data_dir,
                IpcRequest::AddBatch {
                    input: text.clone(),
                    target_dir: dir.clone(),
                    start_paused: paused,
                },
            )
            .await
            {
                let added: Vec<DownloadRecord> = resp.into_data()?;
                println!("Queued {} batch downloads on daemon.", added.len());
                return Ok(());
            }
            let eng = DownloadEngine::open(data_dir, false)?;
            let _sup = eng.spawn_supervisor();
            let added = eng.add_batch_pattern(&text, dir, paused).await?;
            println!("Enqueued {} batch downloads.", added.len());
            if !paused {
                wait_until_all_idle(&eng).await;
            }
        }
        Commands::Torrent {
            source,
            inspect,
            dir,
            play_asap,
            select_files,
        } => {
            let resolved_source = if Path::new(&source).is_file() {
                std::fs::canonicalize(&source)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or(source)
            } else {
                source
            };
            if inspect {
                if let Some(resp) = try_daemon_ipc(
                    data_dir,
                    IpcRequest::InspectTorrent {
                        source: resolved_source.clone(),
                    },
                )
                .await
                {
                    let info: TorrentInspectInfo = resp.into_data()?;
                    print_torrent_inspect(&info);
                    return Ok(());
                }
                let bt = BtSessionManager::new_ephemeral(data_dir.join("tmp_bt")).await?;
                let info = bt.inspect_source(&resolved_source).await?;
                print_torrent_inspect(&TorrentInspectInfo {
                    info_hash: info.info_hash,
                    name: info.name,
                    total_bytes: info.total_bytes,
                    files: info.files,
                    trackers: info.trackers,
                });
                return Ok(());
            }

            let mut req = NewDownloadRequest::from_url(&resolved_source);
            req.target_dir = dir;
            req.sequential_play_asap = play_asap;
            if let Some(sel) = select_files {
                let indices: Vec<usize> = sel
                    .split(',')
                    .filter_map(|s| s.trim().parse::<usize>().ok())
                    .collect();
                req.selected_file_indices = Some(indices);
            }
            if let Some(resp) =
                try_daemon_ipc(data_dir, IpcRequest::AddDownload(Box::new(req.clone()))).await
            {
                let rec: DownloadRecord = resp.into_data()?;
                println!("Queued torrent on daemon #{}: {}", rec.id, rec.title);
                return Ok(());
            }
            let eng = DownloadEngine::open(data_dir, false)?;
            let _sup = eng.spawn_supervisor();
            let rec = eng.add_download(req).await?;
            println!("Added torrent #{}: {}", rec.id, rec.title);
            wait_until_terminal(&eng, rec.id).await;
        }
        Commands::List {
            json,
            search,
            state,
            tag_id,
        } => {
            let q = FilterQuery {
                state_filter: parse_state_filter(state.as_deref()),
                tag_id,
                search_text: search.unwrap_or_default(),
                ..Default::default()
            };
            let items: Vec<DownloadRecord> = if let Some(resp) =
                try_daemon_ipc(data_dir, IpcRequest::ListDownloads(q.clone())).await
            {
                resp.into_data()?
            } else {
                let eng = DownloadEngine::open(data_dir, true)?;
                eng.list_downloads(&q)
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else {
                print_records_table(&items);
            }
        }
        Commands::Pause { id } => {
            if let Some(resp) = try_daemon_ipc(data_dir, IpcRequest::Pause { id }).await {
                println!("{resp:?}");
            } else {
                let eng = DownloadEngine::open(data_dir, true)?;
                eng.pause_download(id).await?;
                println!("Paused #{id}");
            }
        }
        Commands::Resume { id } => {
            if let Some(resp) = try_daemon_ipc(data_dir, IpcRequest::Resume { id }).await {
                println!("{resp:?}");
            } else {
                let eng = DownloadEngine::open(data_dir, false)?;
                let _sup = eng.spawn_supervisor();
                eng.resume_download(id).await?;
                wait_until_terminal(&eng, id).await;
            }
        }
        Commands::Cancel { id } => {
            if let Some(resp) = try_daemon_ipc(data_dir, IpcRequest::Cancel { id }).await {
                println!("{resp:?}");
            } else {
                let eng = DownloadEngine::open(data_dir, true)?;
                eng.cancel_download(id).await?;
                println!("Cancelled #{id}");
            }
        }
        Commands::Remove { id, delete_files } => {
            if let Some(resp) =
                try_daemon_ipc(data_dir, IpcRequest::Remove { id, delete_files }).await
            {
                println!("{resp:?}");
            } else {
                let eng = DownloadEngine::open(data_dir, true)?;
                eng.remove_download(id, delete_files).await?;
                println!("Removed #{id}");
            }
        }
        Commands::StartAll => {
            if let Some(resp) = try_daemon_ipc(data_dir, IpcRequest::StartAll).await {
                println!("{resp:?}");
            } else {
                let eng = DownloadEngine::open(data_dir, false)?;
                let _sup = eng.spawn_supervisor();
                eng.start_all().await?;
                wait_until_all_idle(&eng).await;
            }
        }
        Commands::PauseAll => {
            if let Some(resp) = try_daemon_ipc(data_dir, IpcRequest::PauseAll).await {
                println!("{resp:?}");
            } else {
                let eng = DownloadEngine::open(data_dir, true)?;
                eng.pause_all().await?;
                println!("Paused all downloads");
            }
        }
        Commands::Tum { mode } => {
            let m: TrafficUsageMode = mode.into();
            if let Some(resp) =
                try_daemon_ipc(data_dir, IpcRequest::SetTrafficUsageMode { mode: m }).await
            {
                println!("Updated daemon TUM: {resp:?}");
            } else {
                let eng = DownloadEngine::open(data_dir, true)?;
                eng.set_traffic_usage_mode(m)?;
                println!("Traffic Usage Mode set to {}", m.label());
            }
        }
        Commands::Tags { action } => match action {
            TagCommands::List => {
                let tags: Vec<Tag> =
                    if let Some(resp) = try_daemon_ipc(data_dir, IpcRequest::ListTags).await {
                        resp.into_data()?
                    } else {
                        DownloadEngine::open(data_dir, true)?.list_tags()?
                    };
                print_tags(&tags);
            }
            TagCommands::Create { name, color } => {
                let t: Tag = if let Some(resp) = try_daemon_ipc(
                    data_dir,
                    IpcRequest::CreateTag {
                        name: name.clone(),
                        color_hex: color.clone(),
                        file_types: Vec::new(),
                    },
                )
                .await
                {
                    resp.into_data()?
                } else {
                    DownloadEngine::open(data_dir, true)?.create_tag(&name, &color)?
                };
                println!("Created tag #{}: {} ({})", t.id, t.name, t.color_hex);
            }
            TagCommands::Delete { tag_id } => {
                if let Some(resp) = try_daemon_ipc(data_dir, IpcRequest::DeleteTag { tag_id }).await
                {
                    resp.into_data::<serde_json::Value>()?;
                } else {
                    DownloadEngine::open(data_dir, true)?.delete_tag(tag_id)?;
                }
                println!("Deleted custom tag #{tag_id}");
            }
            TagCommands::Assign {
                download_id,
                tag_id,
            } => {
                if let Some(resp) = try_daemon_ipc(
                    data_dir,
                    IpcRequest::AssignTag {
                        id: download_id,
                        tag_id,
                    },
                )
                .await
                {
                    resp.into_data::<serde_json::Value>()?;
                } else {
                    DownloadEngine::open(data_dir, true)?.assign_tag(download_id, tag_id)?;
                }
                println!("Assigned tag #{tag_id} to download #{download_id}");
            }
            TagCommands::Unassign {
                download_id,
                tag_id,
            } => {
                if let Some(resp) = try_daemon_ipc(
                    data_dir,
                    IpcRequest::UnassignTag {
                        id: download_id,
                        tag_id,
                    },
                )
                .await
                {
                    resp.into_data::<serde_json::Value>()?;
                } else {
                    DownloadEngine::open(data_dir, true)?.unassign_tag(download_id, tag_id)?;
                }
                println!("Unassigned tag #{tag_id} from download #{download_id}");
            }
        },
        Commands::Settings { action } => {
            if let Some(resp) = try_daemon_ipc(data_dir, IpcRequest::GetConfig).await {
                let mut cfg: FerriteConfig = resp.into_data()?;
                if let SettingsCommands::Show { show_secrets } = action {
                    println!("{}", format_settings_output(&cfg, show_secrets)?);
                } else {
                    let msg = apply_settings_mutation(&mut cfg, action);
                    let upd = try_daemon_ipc(data_dir, IpcRequest::UpdateConfig(Box::new(cfg)))
                        .await
                        .context("Ferrite daemon disconnected before settings could be updated")?;
                    upd.into_data::<serde_json::Value>()?;
                    println!("{msg}");
                }
            } else {
                let eng = DownloadEngine::open(data_dir, true)?;
                let mut cfg = eng.get_config();
                if let SettingsCommands::Show { show_secrets } = action {
                    println!("{}", format_settings_output(&cfg, show_secrets)?);
                } else {
                    let msg = apply_settings_mutation(&mut cfg, action);
                    eng.update_config(cfg)?;
                    println!("{msg}");
                }
            }
        }
        Commands::Plugins { action } => match action {
            PluginCommands::List => {
                if let Some(resp) = try_daemon_ipc(data_dir, IpcRequest::ListPlugins).await {
                    let plugs: Vec<JsPluginInfo> = resp.into_data()?;
                    print_plugins(&data_dir.join("plugins"), &plugs);
                } else {
                    let eng = DownloadEngine::open(data_dir, true)?;
                    let plugs = eng.list_js_plugins();
                    print_plugins(eng.plugins_dir(), &plugs);
                }
            }
            PluginCommands::Enable { uid } => {
                if let Some(resp) = try_daemon_ipc(
                    data_dir,
                    IpcRequest::SetPluginEnabled {
                        uid: uid.clone(),
                        enabled: true,
                    },
                )
                .await
                {
                    resp.into_data::<serde_json::Value>()?;
                } else {
                    DownloadEngine::open(data_dir, true)?.set_js_plugin_enabled(&uid, true)?;
                }
                println!("Enabled plugin {uid}");
            }
            PluginCommands::Disable { uid } => {
                if let Some(resp) = try_daemon_ipc(
                    data_dir,
                    IpcRequest::SetPluginEnabled {
                        uid: uid.clone(),
                        enabled: false,
                    },
                )
                .await
                {
                    resp.into_data::<serde_json::Value>()?;
                } else {
                    DownloadEngine::open(data_dir, true)?.set_js_plugin_enabled(&uid, false)?;
                }
                println!("Disabled plugin {uid}");
            }
            PluginCommands::Uninstall { uid } => {
                if let Some(resp) =
                    try_daemon_ipc(data_dir, IpcRequest::UninstallPlugin { uid: uid.clone() }).await
                {
                    resp.into_data::<serde_json::Value>()?;
                } else {
                    DownloadEngine::open(data_dir, true)?.uninstall_js_plugin(&uid)?;
                }
                println!("Uninstalled plugin {uid}");
            }
        },
        Commands::Hash { path, algo, expect } => {
            let a: HashAlgorithm = algo.into();
            let actual = compute_file_hash(&path, a)?;
            println!("{} ({}): {}", a.label(), path.display(), actual);
            if let Some(exp) = expect {
                let ok = actual.eq_ignore_ascii_case(exp.trim());
                println!("Match: {}", if ok { "OK" } else { "MISMATCH" });
                if !ok {
                    std::process::exit(2);
                }
            }
        }
        Commands::Mp3 {
            input,
            output,
            bitrate,
        } => {
            let eng = DownloadEngine::open(data_dir, true)?;
            let cfg = eng.get_config();
            convert_to_mp3(&cfg, &input, &output, bitrate, |pct| {
                eprint!("\rConverting to MP3: {pct:.1}%");
            })
            .await?;
            eprintln!("\rConverted {} -> {}", input.display(), output.display());
        }
        Commands::Mp4 {
            input,
            output,
            transcode,
        } => {
            let eng = DownloadEngine::open(data_dir, true)?;
            let cfg = eng.get_config();
            let _ = ffprobe_inspect(&cfg, &input).await;
            convert_to_mp4(&cfg, &input, &output, !transcode, |pct| {
                eprint!("\rConverting to MP4: {pct:.1}%");
            })
            .await?;
            eprintln!("\rConverted {} -> {}", input.display(), output.display());
        }
        Commands::Export { output } => {
            let eng = DownloadEngine::open(data_dir, true)?;
            let n = eng.export_downloads(&output)?;
            println!("Exported {n} downloads to {}", output.display());
        }
        Commands::Import { input, paused } => {
            let eng = DownloadEngine::open(data_dir, !paused)?;
            let n = eng.import_downloads(&input, paused).await?;
            println!("Imported {n} downloads from {}", input.display());
        }
        Commands::ExportSettings { output } => {
            let eng = DownloadEngine::open(data_dir, true)?;
            eng.export_settings(&output)?;
            println!(
                "Exported settings with SHA-256 hash to {}",
                output.display()
            );
        }
        Commands::ImportSettings { input } => {
            let eng = DownloadEngine::open(data_dir, true)?;
            eng.import_settings(&input)?;
            println!("Imported settings from {}", input.display());
        }
        Commands::RemoteControl { host } => {
            let eng = DownloadEngine::open(data_dir, false)?;
            let _ipc = eng.start_ipc_listener().await?;
            let _sup = eng.spawn_supervisor();
            let pair = eng.ensure_remote_control(&host).await?;
            println!("Remote Control HTTPS Web UI: {}", pair.https_url);
            println!("Ferrite Pairing URI:        {}", pair.pairing_uri);
            println!("\n{}", pair.qr_ascii);
            println!("Serving remote control until Ctrl+C...");
            tokio::signal::ctrl_c().await?;
        }
        Commands::NativeHostInstall {
            browser,
            host_exe,
            target_dir,
        } => {
            let exe = host_exe.unwrap_or_else(|| {
                std::env::current_exe()
                    .ok()
                    .map(|p| p.with_file_name("ferrite-native-host"))
                    .unwrap_or_else(|| PathBuf::from("ferrite-native-host"))
            });
            let browsers = match browser {
                CliBrowser::Chrome => vec![KnownWebBrowser::Chrome],
                CliBrowser::Firefox => vec![KnownWebBrowser::Firefox],
                CliBrowser::Brave => vec![KnownWebBrowser::Brave],
                CliBrowser::Edge => vec![KnownWebBrowser::Edge],
                CliBrowser::Vivaldi => vec![KnownWebBrowser::Vivaldi],
                CliBrowser::Opera => vec![KnownWebBrowser::Opera],
                CliBrowser::All => vec![
                    KnownWebBrowser::Chrome,
                    KnownWebBrowser::Firefox,
                    KnownWebBrowser::Brave,
                    KnownWebBrowser::Edge,
                    KnownWebBrowser::Vivaldi,
                    KnownWebBrowser::Opera,
                ],
            };
            for b in browsers {
                let dir = target_dir
                    .clone()
                    .or_else(|| default_native_manifest_dir(b))
                    .with_context(|| format!("No NativeMessagingHosts dir known for {b:?}"))?;
                let written = install_native_manifest_to_dir(b, &exe, &dir)?;
                println!(
                    "Installed {b:?} native host manifest at {}",
                    written.display()
                );
            }
        }
        Commands::SelfTest => {
            run_self_test().await?;
        }
    }

    Ok(())
}

async fn run_daemon(data_dir: &Path, remote_control: bool, remote_host: &str) -> Result<()> {
    let eng = DownloadEngine::open(data_dir, false)?;
    let _ipc = eng.start_ipc_listener().await?;
    let _sup = eng.spawn_supervisor();
    if remote_control || eng.get_config().app.enable_remote_access {
        let pair = eng.ensure_remote_control(remote_host).await?;
        eprintln!(
            "Ferrite Remote Control listening: {} (URI: {})",
            pair.https_url, pair.pairing_uri
        );
    }
    eprintln!(
        "Ferrite daemon listening on {}",
        default_socket_path(data_dir).display()
    );
    tokio::signal::ctrl_c().await?;
    Ok(())
}

async fn wait_until_terminal(eng: &Arc<DownloadEngine>, id: i64) {
    let mut rx = eng.subscribe();
    while let Some(rec) = eng.get_download(id) {
        if matches!(
            rec.state,
            DownloadState::Completed
                | DownloadState::Error
                | DownloadState::Stopped
                | DownloadState::Paused
        ) {
            println!(
                "Download #{} finished with state: {:?} ({})",
                rec.id,
                rec.state,
                rec.output_path().display()
            );
            break;
        }
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await;
    }
}

async fn wait_until_all_idle(eng: &Arc<DownloadEngine>) {
    loop {
        let s = eng.summary();
        if s.active_count == 0 && s.queued_count == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

async fn run_self_test() -> Result<()> {
    use ferrite_core::hash::sha256_str;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let tmp = std::env::temp_dir().join(format!("ferrite_selftest_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;

    // Start local loopback range server
    let payload: Vec<u8> = (0..(128 * 1024)).map(|i| (i % 251) as u8).collect();
    let payload_arc = Arc::new(payload);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let srv = tokio::spawn({
        let payload_arc = Arc::clone(&payload_arc);
        async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let data = Arc::clone(&payload_arc);
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let total = data.len();
                    let is_head = req.starts_with("HEAD ");
                    let range_hdr = req
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                        .and_then(|l| l.split_once(':').map(|x| x.1.trim().to_string()));
                    if let Some(rh) = range_hdr {
                        let spec = rh.trim_start_matches("bytes=");
                        let (s, e) = spec.split_once('-').unwrap_or(("0", ""));
                        let start: usize = s.parse().unwrap_or(0);
                        let end: usize = e.parse().unwrap_or(total - 1).min(total - 1);
                        let slice = &data[start..=end];
                        let hdr = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{total}\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            slice.len()
                        );
                        let _ = sock.write_all(hdr.as_bytes()).await;
                        if !is_head {
                            let _ = sock.write_all(slice).await;
                        }
                    } else {
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nAccept-Ranges: bytes\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n"
                        );
                        let _ = sock.write_all(hdr.as_bytes()).await;
                        if !is_head {
                            let _ = sock.write_all(&data).await;
                        }
                    }
                });
            }
        }
    });

    let eng = DownloadEngine::open(tmp.join("data"), true)?;
    let _sup = eng.spawn_supervisor();
    let _ipc = eng.start_ipc_listener().await?;

    let mut req = NewDownloadRequest::from_url(format!("http://{addr}/self-test.bin"));
    req.target_dir = Some(tmp.join("dl"));
    let rec = eng.add_download(req).await?;
    wait_until_terminal(&eng, rec.id).await;

    let finished = eng
        .get_download(rec.id)
        .context("missing finished download")?;
    assert_eq!(finished.state, DownloadState::Completed);
    let actual_bytes = std::fs::read(finished.output_path())?;
    assert_eq!(actual_bytes.len(), payload_arc.len());

    let (hash_hex, _) = eng.verify_download_hash(rec.id, HashAlgorithm::Sha256, None)?;
    let _ = sha256_str("check");
    println!(
        "Self-test PASSED: downloaded {} bytes via engine + verified SHA-256 {}",
        actual_bytes.len(),
        hash_hex
    );

    srv.abort();
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_parses_show_secrets_and_excludes_legacy_fdm_commands() {
        let parsed_default = Cli::try_parse_from(["ferrite", "settings", "show"]).unwrap();
        assert!(matches!(
            parsed_default.command,
            Some(Commands::Settings {
                action: SettingsCommands::Show {
                    show_secrets: false
                }
            })
        ));

        let parsed_secrets =
            Cli::try_parse_from(["ferrite", "settings", "show", "--show-secrets"]).unwrap();
        assert!(matches!(
            parsed_secrets.command,
            Some(Commands::Settings {
                action: SettingsCommands::Show { show_secrets: true }
            })
        ));

        assert!(Cli::try_parse_from(["ferrite", "--service"]).is_err());
        assert!(Cli::try_parse_from(["ferrite", "import-fdm-db", "legacy.sqlite"]).is_err());

        let help = Cli::command().render_help().to_string();
        assert!(!help.contains("FDM"));
        assert!(!help.contains("--service"));
        assert!(!help.contains("import-fdm-db"));
        assert!(!help.contains("freedownloadmanager"));
    }

    #[test]
    fn settings_show_redacts_secrets_unless_opted_in() {
        let mut cfg = FerriteConfig::default();
        cfg.app.remote_access_password = "rc-super-secret-password".into();
        cfg.app.proxy.http.password = "http-proxy-secret".into();
        cfg.app.proxy.https.password = "https-proxy-secret".into();
        cfg.app.proxy.socks5.password = "socks5-proxy-secret".into();

        let redacted_json = format_settings_output(&cfg, false).unwrap();
        assert!(!redacted_json.contains("rc-super-secret-password"));
        assert!(!redacted_json.contains("http-proxy-secret"));
        assert!(!redacted_json.contains("https-proxy-secret"));
        assert!(!redacted_json.contains("socks5-proxy-secret"));

        let parsed_redacted: FerriteConfig = serde_json::from_str(&redacted_json).unwrap();
        assert_eq!(parsed_redacted.app.remote_access_password, "***REDACTED***");
        assert_eq!(parsed_redacted.app.proxy.http.password, "***REDACTED***");
        assert_eq!(parsed_redacted.app.proxy.https.password, "***REDACTED***");
        assert_eq!(parsed_redacted.app.proxy.socks5.password, "***REDACTED***");

        let revealed_json = format_settings_output(&cfg, true).unwrap();
        let parsed_revealed: FerriteConfig = serde_json::from_str(&revealed_json).unwrap();
        assert_eq!(
            parsed_revealed.app.remote_access_password,
            "rc-super-secret-password"
        );
        assert_eq!(parsed_revealed.app.proxy.http.password, "http-proxy-secret");
        assert_eq!(
            parsed_revealed.app.proxy.https.password,
            "https-proxy-secret"
        );
        assert_eq!(
            parsed_revealed.app.proxy.socks5.password,
            "socks5-proxy-secret"
        );

        let empty_cfg = FerriteConfig::default();
        let empty_redacted: FerriteConfig =
            serde_json::from_str(&format_settings_output(&empty_cfg, false).unwrap()).unwrap();
        assert_eq!(empty_redacted.app.remote_access_password, "");
        assert_eq!(empty_redacted.app.proxy.http.password, "");
    }

    #[tokio::test]
    async fn cli_routes_batch_torrent_tags_settings_and_plugins_via_daemon_ipc() {
        let dir = std::env::temp_dir().join(format!("ferrite_cli_ipc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data_dir = dir.join("data");
        let eng = DownloadEngine::open(&data_dir, true).unwrap();
        let ipc_handle = eng.start_ipc_listener().await.unwrap();

        // Install a dummy JS plugin so plugin list/enable/disable/uninstall can run via IPC
        let plug_dir = data_dir.join("plugins/sample");
        std::fs::create_dir_all(&plug_dir).unwrap();
        std::fs::write(plug_dir.join("parser.js"), "var msParser = {};").unwrap();
        std::fs::write(
            plug_dir.join("manifest.json"),
            serde_json::json!({
                "uid": "sample-plug",
                "name": "Sample",
                "version": "1.0.0",
                "entry": "parser.js",
                "domains": ["example.com"],
                "enabled": true
            })
            .to_string(),
        )
        .unwrap();

        // Create a valid minimal .torrent file for InspectTorrent IPC routing
        let torrent_path = dir.join("sample.torrent");
        std::fs::write(
            &torrent_path,
            b"d8:announce19:http://tracker/test4:infod6:lengthi123e4:name8:test.bin12:piece lengthi16384e6:pieces20:01234567890123456789ee",
        )
        .unwrap();

        // 1. Batch via daemon IPC
        let cli_batch = Cli::try_parse_from([
            "ferrite",
            "batch",
            "https://example.org/part_[1-2].bin",
            "--paused",
        ])
        .unwrap();
        run_cli(cli_batch, &data_dir).await.unwrap();
        let all_dls = eng.list_downloads(&FilterQuery::default());
        assert_eq!(all_dls.len(), 2);
        let first_dl_id = all_dls[0].id;

        // 2. Torrent --inspect via daemon IPC
        let cli_torrent = Cli::try_parse_from([
            "ferrite",
            "torrent",
            torrent_path.to_str().unwrap(),
            "--inspect",
        ])
        .unwrap();
        run_cli(cli_torrent, &data_dir).await.unwrap();

        // 3. Tags Create / Assign / Unassign / Delete via daemon IPC (mutates in-memory engine)
        let cli_tag_create =
            Cli::try_parse_from(["ferrite", "tags", "create", "IpcTag", "--color", "#112233"])
                .unwrap();
        run_cli(cli_tag_create, &data_dir).await.unwrap();
        let tag_id = eng
            .list_tags()
            .unwrap()
            .into_iter()
            .find(|t| t.name == "IpcTag")
            .expect("daemon should have IpcTag in memory/DB")
            .id;

        let cli_tag_assign = Cli::try_parse_from([
            "ferrite",
            "tags",
            "assign",
            &first_dl_id.to_string(),
            &tag_id.to_string(),
        ])
        .unwrap();
        run_cli(cli_tag_assign, &data_dir).await.unwrap();
        assert!(
            eng.get_download(first_dl_id)
                .unwrap()
                .tags
                .contains(&tag_id)
        );

        let cli_tag_unassign = Cli::try_parse_from([
            "ferrite",
            "tags",
            "unassign",
            &first_dl_id.to_string(),
            &tag_id.to_string(),
        ])
        .unwrap();
        run_cli(cli_tag_unassign, &data_dir).await.unwrap();
        assert!(
            !eng.get_download(first_dl_id)
                .unwrap()
                .tags
                .contains(&tag_id)
        );

        let cli_tag_delete =
            Cli::try_parse_from(["ferrite", "tags", "delete", &tag_id.to_string()]).unwrap();
        run_cli(cli_tag_delete, &data_dir).await.unwrap();
        assert!(eng.list_tags().unwrap().into_iter().all(|t| t.id != tag_id));

        // 4. Settings mutation via daemon IPC (mutates live engine config without restart)
        let new_dl_dir = dir.join("ipc-downloads");
        let cli_set_dir = Cli::try_parse_from([
            "ferrite",
            "settings",
            "set-download-dir",
            new_dl_dir.to_str().unwrap(),
        ])
        .unwrap();
        run_cli(cli_set_dir, &data_dir).await.unwrap();
        assert_eq!(eng.get_config().core.default_download_path, new_dl_dir);

        // 5. Plugins Disable / Enable / Uninstall via daemon IPC
        let cli_plug_dis =
            Cli::try_parse_from(["ferrite", "plugins", "disable", "sample-plug"]).unwrap();
        run_cli(cli_plug_dis, &data_dir).await.unwrap();
        assert!(
            !eng.list_js_plugins()
                .into_iter()
                .find(|p| p.uid == "sample-plug")
                .unwrap()
                .enabled
        );

        let cli_plug_en =
            Cli::try_parse_from(["ferrite", "plugins", "enable", "sample-plug"]).unwrap();
        run_cli(cli_plug_en, &data_dir).await.unwrap();
        assert!(
            eng.list_js_plugins()
                .into_iter()
                .find(|p| p.uid == "sample-plug")
                .unwrap()
                .enabled
        );

        let cli_plug_un =
            Cli::try_parse_from(["ferrite", "plugins", "uninstall", "sample-plug"]).unwrap();
        run_cli(cli_plug_un, &data_dir).await.unwrap();
        assert!(eng.list_js_plugins().is_empty());

        ipc_handle.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
