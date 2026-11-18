use crate::model::{DownloadRecord, ShutdownType};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
#[cfg(target_os = "macos")]
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiskSpaceStats {
    pub available_bytes: u64,
    pub total_bytes: u64,
    pub free_percent: f32,
}

/// Inspect available and total disk space for `path` (walking up to the nearest existing ancestor).
pub fn disk_space_stats(path: &Path) -> Option<DiskSpaceStats> {
    let mut probe = path.to_path_buf();
    while !probe.exists() {
        if !probe.pop() {
            #[cfg(unix)]
            {
                probe = PathBuf::from("/");
            }
            #[cfg(windows)]
            {
                probe = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("C:\\"));
            }
            #[cfg(not(any(unix, windows)))]
            {
                break;
            }
            break;
        }
    }
    if probe.is_file() {
        probe.pop();
    }
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let c_path = CString::new(probe.as_os_str().as_bytes()).ok()?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut st) };
        if rc != 0 {
            return None;
        }
        let frsize = if st.f_frsize > 0 {
            st.f_frsize as u64
        } else {
            st.f_bsize as u64
        };
        let available_bytes = (st.f_bavail as u64).saturating_mul(frsize);
        let total_bytes = (st.f_blocks as u64).saturating_mul(frsize);
        let free_percent = if total_bytes > 0 {
            (available_bytes as f64 / total_bytes as f64 * 100.0) as f32
        } else {
            100.0
        };
        Some(DiskSpaceStats {
            available_bytes,
            total_bytes,
            free_percent,
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetDiskFreeSpaceExW(
                lpDirectoryName: *const u16,
                lpFreeBytesAvailableToCaller: *mut u64,
                lpTotalNumberOfBytes: *mut u64,
                lpTotalNumberOfFreeBytes: *mut u64,
            ) -> i32;
        }

        let mut wide: Vec<u16> = probe.as_os_str().encode_wide().collect();
        wide.push(0);

        let mut available_bytes: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut total_free_bytes: u64 = 0;

        let rc = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut available_bytes,
                &mut total_bytes,
                &mut total_free_bytes,
            )
        };
        if rc == 0 {
            return None;
        }
        let free_percent = if total_bytes > 0 {
            (available_bytes as f64 / total_bytes as f64 * 100.0) as f32
        } else {
            100.0
        };
        Some(DiskSpaceStats {
            available_bytes,
            total_bytes,
            free_percent,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = probe;
        None
    }
}

/// Returns `true` when free space meets both `min_bytes` and `min_percent` thresholds.
pub fn has_sufficient_disk_space(path: &Path, min_bytes: u64, min_percent: f32) -> bool {
    if min_bytes == 0 && min_percent <= 0.0 {
        return true;
    }
    let Some(stats) = disk_space_stats(path) else {
        return true;
    };
    let bytes_ok = min_bytes == 0 || stats.available_bytes >= min_bytes;
    let pct_ok = min_percent <= 0.0 || stats.free_percent >= min_percent;
    bytes_ok && pct_ok
}

/// Build a standards-compliant browser-style User-Agent string for the given OS identifier.
pub fn format_system_user_agent(os: &str, macos_version: Option<&str>) -> String {
    match os {
        "macos" => {
            let os_ver = macos_version
                .map(|s| s.trim().replace('.', "_"))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "15_0".into());
            format!(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X {os_ver}) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15"
            )
        }
        "windows" => {
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36"
                .to_string()
        }
        _ => {
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36"
                .to_string()
        }
    }
}

/// Cached OS-native browser-style User-Agent string when `use_system_default_user_agent` is enabled.
pub fn system_default_user_agent() -> &'static str {
    static UA: OnceLock<String> = OnceLock::new();
    UA.get_or_init(|| {
        #[cfg(target_os = "macos")]
        let macos_ver = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok());
        #[cfg(not(target_os = "macos"))]
        let macos_ver: Option<String> = None;

        format_system_user_agent(std::env::consts::OS, macos_ver.as_deref())
    })
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn autorun_plist_content(exe_path: &Path, minimized: bool) -> String {
    let exe_escaped = xml_escape(&exe_path.to_string_lossy());
    let extra_arg = if minimized {
        "\n        <string>--hidden</string>"
    } else {
        ""
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>org.ferritedm.FerriteDM</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe_escaped}</string>{extra_arg}
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#
    )
}

pub fn sync_autorun_plist_in_dir(
    launch_agents_dir: &Path,
    exe_path: &Path,
    enabled: bool,
    minimized: bool,
) -> Result<()> {
    let plist_path = launch_agents_dir.join("org.ferritedm.FerriteDM.plist");
    if enabled {
        std::fs::create_dir_all(launch_agents_dir)?;
        let content = autorun_plist_content(exe_path, minimized);
        std::fs::write(plist_path, content)?;
    } else if plist_path.exists() {
        std::fs::remove_file(plist_path)?;
    }
    Ok(())
}

pub fn linux_autostart_desktop_content(exe_path: &Path, minimized: bool) -> String {
    let exec_cmd = if minimized {
        format!("{} --hidden", exe_path.display())
    } else {
        format!("{}", exe_path.display())
    };
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Ferrite DM\n\
         Comment=Ferrite Download Manager\n\
         Exec={exec_cmd}\n\
         Icon=ferrite-dm\n\
         Terminal=false\n\
         Categories=Network;FileTransfer;\n\
         StartupNotify=false\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// Sync login autorun item (macOS LaunchAgent plist; Linux ~/.config/autostart desktop entry).
pub fn sync_autorun_on_login(enabled: bool, minimized: bool) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) else {
            return Ok(());
        };
        let launch_agents = home.join("Library").join("LaunchAgents");
        let exe = std::env::current_exe()?;
        sync_autorun_plist_in_dir(&launch_agents, &exe, enabled, minimized)?;
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) {
            let autostart_dir = home.join(".config").join("autostart");
            let desktop_file = autostart_dir.join("ferrite-dm.desktop");
            if enabled {
                std::fs::create_dir_all(&autostart_dir)?;
                let exe = std::env::current_exe()?;
                let content = linux_autostart_desktop_content(&exe, minimized);
                std::fs::write(desktop_file, content)?;
            } else if desktop_file.exists() {
                std::fs::remove_file(desktop_file)?;
            }
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (enabled, minimized);
    }
    Ok(())
}

/// Check whether Ferrite DM is currently registered as the default handler for `magnet:` links or `.torrent` files.
pub async fn is_default_torrent_client() -> bool {
    #[cfg(target_os = "macos")]
    {
        let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) else {
            return false;
        };
        let plist = home
            .join("Library")
            .join("Preferences")
            .join("com.apple.LaunchServices")
            .join("com.apple.launchservices.secure.plist");
        if !plist.exists() {
            return false;
        }
        if let Ok(out) = Command::new("plutil")
            .args(["-convert", "json", "-o", "-"])
            .arg(&plist)
            .output()
            .await
            && let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
            && let Some(handlers) = val.get("LSHandlers").and_then(|v| v.as_array())
        {
            for entry in handlers {
                let scheme = entry
                    .get("LSHandlerURLScheme")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let tag = entry
                    .get("LSHandlerContentTag")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let role = entry
                    .get("LSHandlerRoleAll")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if (scheme.eq_ignore_ascii_case("magnet") || tag.eq_ignore_ascii_case("torrent"))
                    && role.to_ascii_lowercase().contains("ferritedm")
                {
                    return true;
                }
            }
        }
        false
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Returns `(is_metered_or_cellular, is_roaming)` cached for 5 seconds to avoid spawning processes on every scheduler tick.
pub async fn detect_network_restrictions() -> (bool, bool) {
    static CACHE: OnceLock<Mutex<Option<(Instant, bool, bool)>>> = OnceLock::new();
    let mutex = CACHE.get_or_init(|| Mutex::new(None));
    {
        let g = mutex.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, cellular, roaming)) = *g
            && at.elapsed() < Duration::from_secs(5)
        {
            return (cellular, roaming);
        }
    }

    #[allow(unused_mut)]
    let mut cellular = false;
    #[allow(unused_mut)]
    let mut roaming = false;
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = Command::new("route")
            .args(["-n", "get", "default"])
            .output()
            .await
        {
            let txt = String::from_utf8_lossy(&out.stdout);
            for line in txt.lines() {
                if let Some(iface) = line.trim().strip_prefix("interface:") {
                    let iface = iface.trim();
                    if iface.starts_with("pdp_ip") {
                        cellular = true;
                        break;
                    }
                    if let Ok(services) = Command::new("networksetup")
                        .arg("-listnetworkserviceorder")
                        .output()
                        .await
                    {
                        let svc_txt =
                            String::from_utf8_lossy(&services.stdout).to_ascii_lowercase();
                        if svc_txt.contains(&format!("device: {iface})"))
                            && (svc_txt.contains("cellular")
                                || svc_txt.contains("iphone")
                                || svc_txt.contains("hotspot")
                                || svc_txt.contains("modem"))
                        {
                            cellular = true;
                        }
                    }
                }
            }
        }
        if cellular && let Ok(out) = Command::new("scutil").arg("--nwi").output().await {
            let txt = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
            if txt.contains("roaming") {
                roaming = true;
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        let mut default_iface = None;
        if let Ok(route_content) = std::fs::read_to_string("/proc/net/route") {
            for line in route_content.lines().skip(1) {
                let mut fields = line.split_whitespace();
                let iface = fields.next();
                let dest = fields.next();
                if dest == Some("00000000") {
                    default_iface = iface.map(|s| s.to_string());
                    break;
                }
            }
        }

        if let Some(iface) = default_iface {
            let is_wwan = iface.starts_with("ww")
                || iface.starts_with("cdc")
                || iface.starts_with("ppp")
                || iface.starts_with("lte");
            if is_wwan {
                cellular = true;
            }
        } else if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if (name.starts_with("ww") || name.starts_with("cdc") || name.starts_with("ppp"))
                    && let Ok(operstate) = std::fs::read_to_string(entry.path().join("operstate"))
                    && operstate.trim() == "up"
                {
                    cellular = true;
                    break;
                }
            }
        }

        if !cellular
            && let Ok(out) = Command::new("nmcli")
                .args(["-t", "-f", "GENERAL.METERED", "dev", "show"])
                .output()
                .await
        {
            let txt = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
            if txt.contains("yes") {
                cellular = true;
            }
        }
    }

    {
        let mut g = mutex.lock().unwrap_or_else(|e| e.into_inner());
        *g = Some((Instant::now(), cellular, roaming));
    }
    (cellular, roaming)
}

#[cfg(target_os = "macos")]
fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\n', '\r'], " ")
}

/// Trigger a non-blocking OS desktop notification.
pub fn send_os_notification(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{}\" with title \"Ferrite DM\" subtitle \"{}\"",
            applescript_escape(body),
            applescript_escape(title)
        );
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .args(["--app-name=Ferrite DM", "--icon=ferrite-dm", title, body])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (title, body);
    }
}

/// Trigger a non-blocking OS notification sound using the custom sound file or macOS default alert sound.
pub fn play_notification_sound(custom_path: Option<&Path>) {
    #[cfg(target_os = "macos")]
    {
        let chosen = custom_path
            .filter(|p| p.exists())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/System/Library/Sounds/Glass.aiff"));
        if chosen.exists() {
            let _ = std::process::Command::new("afplay")
                .arg(&chosen)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(path) = custom_path.filter(|p| p.exists()) {
            let _ = std::process::Command::new("paplay")
                .arg(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .or_else(|_| {
                    std::process::Command::new("aplay")
                        .arg(path)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                });
        } else {
            let _ = std::process::Command::new("canberra-gtk-play")
                .args(["-i", "complete"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = custom_path;
    }
}

/// Apply macOS Mark-of-the-Web (`com.apple.quarantine`) extended attribute on completed files.
pub fn set_mark_of_the_web(path: &Path, source_url: &str, app_name: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let guid = uuid::Uuid::new_v4().to_string().to_uppercase();
        let val = format!("0083;{:08x};{};{}", ts, app_name, guid);
        xattr::set(path, "com.apple.quarantine", val.as_bytes())?;
        let _ = xattr::set(
            path,
            "com.apple.metadata:kMDItemWhereFroms",
            source_url.as_bytes(),
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, source_url, app_name);
    }
    Ok(())
}

/// Check battery and power state: returns `(on_battery, battery_percentage)`.
pub async fn check_power_status() -> (bool, u8) {
    #[cfg(target_os = "macos")]
    if let Ok(out) = Command::new("pmset").args(["-g", "batt"]).output().await {
        let txt = String::from_utf8_lossy(&out.stdout);
        let on_battery = txt.contains("Battery Power");
        let mut pct = 100u8;
        for tok in txt.split_whitespace() {
            if let Some(num) = tok.strip_suffix("%;") {
                if let Ok(v) = num.parse::<u8>() {
                    pct = v;
                    break;
                }
            } else if let Some(num) = tok.strip_suffix('%')
                && let Ok(v) = num.parse::<u8>()
            {
                pct = v;
                break;
            }
        }
        return (on_battery, pct);
    }
    #[cfg(target_os = "linux")]
    {
        let mut on_battery = false;
        let mut battery_capacity: Option<u8> = None;
        let mut has_battery = false;
        let mut ac_online: Option<bool> = None;

        if let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") {
            for entry in entries.flatten() {
                let path = entry.path();
                let type_file = path.join("type");
                let supply_type = std::fs::read_to_string(&type_file)
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase();

                if supply_type == "mains" || supply_type.contains("ac") {
                    if let Ok(online_str) = std::fs::read_to_string(path.join("online")) {
                        if online_str.trim() == "1" {
                            ac_online = Some(true);
                        } else if online_str.trim() == "0" {
                            ac_online = Some(false);
                        }
                    }
                } else if supply_type == "battery" {
                    has_battery = true;
                    if let Ok(cap_str) = std::fs::read_to_string(path.join("capacity"))
                        && let Ok(v) = cap_str.trim().parse::<u8>()
                    {
                        battery_capacity = Some(battery_capacity.map_or(v, |old| old.min(v)));
                    }
                    if let Ok(status_str) = std::fs::read_to_string(path.join("status"))
                        && status_str.trim().eq_ignore_ascii_case("discharging")
                    {
                        on_battery = true;
                    }
                }
            }
        }

        if let Some(ac) = ac_online
            && !ac
            && has_battery
        {
            on_battery = true;
        }

        let pct = battery_capacity.unwrap_or(100);
        return (on_battery, pct);
    }
    #[allow(unreachable_code)]
    (false, 100)
}

/// Holds a child process (macOS `caffeinate` or Linux `systemd-inhibit`) while active downloads prevent auto-sleep.
#[derive(Debug, Default)]
pub struct SleepPreventer {
    child: Option<std::process::Child>,
}

impl SleepPreventer {
    pub fn update(&mut self, should_prevent: bool) {
        if should_prevent {
            #[cfg(target_os = "macos")]
            {
                if let Some(ref mut c) = self.child
                    && let Ok(None) = c.try_wait()
                {
                    return;
                }
                let pid = std::process::id().to_string();
                if let Ok(child) = std::process::Command::new("caffeinate")
                    .args(["-i", "-d", "-w", &pid])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    self.child = Some(child);
                }
            }
            #[cfg(target_os = "linux")]
            {
                if let Some(ref mut c) = self.child
                    && let Ok(None) = c.try_wait()
                {
                    return;
                }
                if let Ok(child) = std::process::Command::new("systemd-inhibit")
                    .args([
                        "--what=sleep:idle",
                        "--who=FerriteDM",
                        "--why=Active downloads",
                        "--mode=block",
                        "sleep",
                        "infinity",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    self.child = Some(child);
                }
            }
        } else if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for SleepPreventer {
    fn drop(&mut self) {
        self.update(false);
    }
}

fn split_template_args(template: &str, record: &DownloadRecord) -> Vec<String> {
    let file_str = record.output_path().to_string_lossy().to_string();
    let dir_str = record.target_dir.to_string_lossy().to_string();
    let replaced = template
        .replace("{file}", &file_str)
        .replace("%path%", &file_str)
        .replace("{path}", &file_str)
        .replace("%1", &file_str)
        .replace("{dir}", &dir_str)
        .replace("{url}", &record.url)
        .replace("{title}", &record.title);

    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut quote_char = '"';
    for ch in replaced.chars() {
        if in_quotes {
            if ch == quote_char {
                in_quotes = false;
            } else {
                cur.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            in_quotes = true;
            quote_char = ch;
        } else if ch.is_whitespace() {
            if !cur.is_empty() {
                args.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        args.push(cur);
    }
    args
}

pub async fn run_external_completion_app(
    app_path: &Path,
    args_template: &str,
    record: &DownloadRecord,
) -> Result<()> {
    let args = split_template_args(args_template, record);
    let mut cmd = Command::new(app_path);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = cmd.output().await?;
    if !output.status.success() {
        anyhow::bail!(
            "External command exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

pub async fn scan_file_with_antivirus(
    av_path: &Path,
    args_template: &str,
    record: &DownloadRecord,
) -> Result<()> {
    run_external_completion_app(av_path, args_template, record).await
}

pub async fn perform_shutdown_action(action: ShutdownType) -> Result<()> {
    match action {
        ShutdownType::None | ShutdownType::QuitApp => Ok(()),
        #[cfg(target_os = "macos")]
        ShutdownType::Sleep => {
            Command::new("pmset").arg("sleepnow").status().await?;
            Ok(())
        }
        #[cfg(target_os = "macos")]
        ShutdownType::Hibernate => {
            Command::new("pmset").arg("sleepnow").status().await?;
            Ok(())
        }
        #[cfg(target_os = "macos")]
        ShutdownType::Shutdown => {
            Command::new("osascript")
                .args(["-e", "tell application \"System Events\" to shut down"])
                .status()
                .await?;
            Ok(())
        }
        #[cfg(target_os = "linux")]
        ShutdownType::Sleep => {
            Command::new("systemctl").arg("suspend").status().await?;
            Ok(())
        }
        #[cfg(target_os = "linux")]
        ShutdownType::Hibernate => {
            Command::new("systemctl").arg("hibernate").status().await?;
            Ok(())
        }
        #[cfg(target_os = "linux")]
        ShutdownType::Shutdown => {
            Command::new("systemctl").arg("poweroff").status().await?;
            Ok(())
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_space_stats_checks_thresholds() {
        let cwd = std::env::current_dir().unwrap();
        let stats = disk_space_stats(&cwd).expect("statvfs should succeed on workspace dir");
        assert!(stats.total_bytes > 0);
        assert!(has_sufficient_disk_space(&cwd, 1, 0.01));
        assert!(!has_sufficient_disk_space(&cwd, u64::MAX, 0.0));
        assert!(!has_sufficient_disk_space(&cwd, 0, 100.1));
    }

    #[test]
    fn autorun_plist_sync_writes_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("ferrite-autorun-{}", uuid::Uuid::new_v4()));
        let exe = PathBuf::from("/Applications/Ferrite DM.app/Contents/MacOS/fdm-gui");
        sync_autorun_plist_in_dir(&dir, &exe, true, true).unwrap();
        let plist = dir.join("org.ferritedm.FerriteDM.plist");
        let content = std::fs::read_to_string(&plist).unwrap();
        assert!(content.contains("org.ferritedm.FerriteDM"));
        assert!(content.contains("--hidden"));
        assert!(content.contains("<true/>"));

        sync_autorun_plist_in_dir(&dir, &exe, false, true).unwrap();
        assert!(!plist.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn system_default_user_agent_is_non_empty() {
        let ua = system_default_user_agent();
        assert!(ua.contains("Mozilla/5.0"));
        assert!(
            !ua.contains("linux;"),
            "Linux UA must not be malformed: {ua}"
        );
    }

    #[test]
    fn format_system_user_agent_uses_standard_os_tokens() {
        let linux_ua = format_system_user_agent("linux", None);
        assert!(linux_ua.contains("(X11; Linux x86_64)"));
        assert!(!linux_ua.contains("linux;"));

        let win_ua = format_system_user_agent("windows", None);
        assert!(win_ua.contains("(Windows NT 10.0; Win64; x64)"));

        let mac_ua = format_system_user_agent("macos", Some("15.2"));
        assert!(mac_ua.contains("(Macintosh; Intel Mac OS X 15_2)"));
    }
}
