use crate::hash::sha256_str;
use crate::model::{
    DownloadFlags, DownloadPriority, DownloadRecord, DownloadState, DownloadType,
    HttpRequestOptions, ResumeSupport,
};
use crate::routing::{
    classify_file_type, infer_filename_from_url, sanitize_filename, sanitize_partial_suffix,
};
use crate::settings::FerriteConfig;
use anyhow::{Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedDownloadsBundle {
    #[serde(rename = "FormatName")]
    pub format_name: String,
    #[serde(rename = "FormatVersion")]
    pub format_version: u32,
    #[serde(rename = "FileContents")]
    pub file_contents: ExportedDownloadsContents,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedDownloadsContents {
    pub downloads: Vec<DownloadRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedSettingsBundle {
    #[serde(rename = "AppName")]
    pub app_name: String,
    #[serde(rename = "SettingsFile")]
    pub settings_file: String,
    #[serde(rename = "SettingsHash")]
    pub settings_hash: String,
}

pub fn export_downloads_json(downloads: &[DownloadRecord], path: &Path) -> Result<()> {
    let bundle = ExportedDownloadsBundle {
        format_name: "Ferrite-Downloads".into(),
        format_version: 1,
        file_contents: ExportedDownloadsContents {
            downloads: downloads.to_vec(),
        },
    };
    let text = serde_json::to_string_pretty(&bundle)?;
    std::fs::write(path, text)?;
    Ok(())
}

pub fn export_downloads_urls_txt(downloads: &[DownloadRecord], path: &Path) -> Result<()> {
    let lines: Vec<String> = downloads
        .iter()
        .filter(|d| !d.url.trim().is_empty())
        .map(|d| d.url.clone())
        .collect();
    std::fs::write(path, lines.join("\n"))?;
    Ok(())
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub fn export_downloads_csv(downloads: &[DownloadRecord], path: &Path) -> Result<()> {
    let mut out =
        String::from("id,title,url,state,downloaded_bytes,total_bytes,output_path,created_at\n");
    for d in downloads {
        let row = [
            d.id.to_string(),
            csv_escape(&d.title),
            csv_escape(&d.url),
            csv_escape(d.state.display_name()),
            d.downloaded_bytes.to_string(),
            d.total_bytes.map(|v| v.to_string()).unwrap_or_default(),
            csv_escape(&d.output_path().to_string_lossy()),
            csv_escape(&d.created_at.to_rfc3339()),
        ];
        out.push_str(&row.join(","));
        out.push('\n');
    }
    std::fs::write(path, out)?;
    Ok(())
}

fn sanitize_imported_record(mut rec: DownloadRecord) -> DownloadRecord {
    rec.file_name = sanitize_filename(&rec.file_name);
    rec.partial_suffix = sanitize_partial_suffix(&rec.partial_suffix);
    rec
}

pub fn import_downloads_file(path: &Path, config: &FerriteConfig) -> Result<Vec<DownloadRecord>> {
    let raw = std::fs::read_to_string(path)?;
    let trimmed = raw.trim();
    if trimmed.starts_with('{') {
        if let Ok(bundle) = serde_json::from_str::<ExportedDownloadsBundle>(trimmed) {
            if matches!(
                bundle.format_name.as_str(),
                "Ferrite-Downloads" | "FerriteDM-Downloads"
            ) {
                return Ok(bundle
                    .file_contents
                    .downloads
                    .into_iter()
                    .map(sanitize_imported_record)
                    .collect());
            }
            bail!(
                "Unsupported downloads bundle format: {}",
                bundle.format_name
            );
        }
        // Fallback: parse generic JSON export shapes containing `FileContents.downloads` or `downloads`.
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(fmt) = val.get("FormatName").and_then(|v| v.as_str())
                && !matches!(fmt, "Ferrite-Downloads" | "FerriteDM-Downloads")
            {
                bail!("Unsupported downloads bundle format: {fmt}");
            }
            let arr = val
                .get("FileContents")
                .and_then(|fc| fc.get("downloads"))
                .and_then(|d| d.as_array())
                .or_else(|| val.get("downloads").and_then(|d| d.as_array()));
            if let Some(items) = arr {
                let mut out = Vec::new();
                for item in items {
                    if let Ok(rec) = serde_json::from_value::<DownloadRecord>(item.clone()) {
                        out.push(sanitize_imported_record(rec));
                    } else if let Some(url) = item
                        .get("Url")
                        .or_else(|| item.get("url"))
                        .and_then(|u| u.as_str())
                    {
                        out.push(record_from_imported_url(url, config));
                    }
                }
                if !out.is_empty() {
                    return Ok(out);
                }
            }
        }
    }

    // Plain URL list (one URL per line, ignoring empty lines & # comments)
    let mut out = Vec::new();
    for line in raw.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if l.contains("://") || l.starts_with("magnet:") {
            out.push(record_from_imported_url(l, config));
        }
    }
    Ok(out)
}

pub fn export_settings_json(config: &FerriteConfig, path: &Path) -> Result<()> {
    let mut redacted = config.clone();
    redacted.app.remote_access_password.clear();
    redacted.app.proxy.http.password.clear();
    redacted.app.proxy.https.password.clear();
    redacted.app.proxy.socks5.password.clear();

    let inner = serde_json::to_string_pretty(&redacted)?;
    let hash = sha256_str(&inner);
    let bundle = ExportedSettingsBundle {
        app_name: "Ferrite".into(),
        settings_file: inner,
        settings_hash: hash,
    };
    std::fs::write(path, serde_json::to_string_pretty(&bundle)?)?;
    Ok(())
}

pub fn import_settings_json(path: &Path) -> Result<FerriteConfig> {
    let raw = std::fs::read_to_string(path)?;
    if let Ok(bundle) = serde_json::from_str::<ExportedSettingsBundle>(&raw) {
        let actual = sha256_str(&bundle.settings_file);
        if !actual.eq_ignore_ascii_case(&bundle.settings_hash) {
            bail!("SettingsHash verification failed (corrupted or tampered settings file)");
        }
        let cfg: FerriteConfig = serde_json::from_str(&bundle.settings_file)?;
        return Ok(cfg);
    }
    Ok(serde_json::from_str::<FerriteConfig>(&raw)?)
}

fn record_from_imported_url(url: &str, config: &FerriteConfig) -> DownloadRecord {
    let file_name = infer_filename_from_url(url);
    let dl_type = if url.starts_with("magnet:") || url.ends_with(".torrent") {
        DownloadType::BitTorrent
    } else if url.contains(".m3u8") {
        DownloadType::HlsM3u8
    } else {
        DownloadType::Www
    };
    DownloadRecord {
        id: 0,
        uuid: uuid::Uuid::new_v4().to_string(),
        parent_id: None,
        children_ids: vec![],
        order_index: 0,
        download_type: dl_type,
        state: DownloadState::Stopped,
        priority: DownloadPriority::Normal,
        resume_support: ResumeSupport::Unknown,
        flags: DownloadFlags::default(),
        title: file_name.clone(),
        url: url.to_string(),
        target_dir: config.core.default_download_path.clone(),
        file_name: file_name.clone(),
        partial_suffix: config.core.partial_download_suffix.clone(),
        total_bytes: None,
        downloaded_bytes: 0,
        uploaded_bytes: 0,
        download_speed_bps: 0,
        upload_speed_bps: 0,
        eta_secs: None,
        connections_active: 0,
        etag: None,
        last_modified: None,
        file_type: classify_file_type(&file_name, None),
        http: HttpRequestOptions {
            url: url.to_string(),
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
        abort_reason: Default::default(),
        preview_path: None,
        module_data: serde_json::Value::Null,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        completed_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_export_import_verifies_hash_and_redacts_secrets() {
        let dir = std::env::temp_dir().join(format!("ferrite_exp_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("settings.json");
        let mut cfg = FerriteConfig::default();
        cfg.app.remote_access_password = "super-secret-rc-pass".into();
        cfg.app.proxy.http.password = "http-secret-pass".into();
        cfg.app.proxy.https.password = "https-secret-pass".into();
        cfg.app.proxy.socks5.password = "socks-secret-pass".into();

        export_settings_json(&cfg, &p).unwrap();
        let exported_raw = std::fs::read_to_string(&p).unwrap();
        assert!(exported_raw.contains("\"AppName\": \"Ferrite\""));
        assert!(!exported_raw.contains("super-secret-rc-pass"));
        assert!(!exported_raw.contains("http-secret-pass"));
        assert!(!exported_raw.contains("https-secret-pass"));
        assert!(!exported_raw.contains("socks-secret-pass"));

        let loaded = import_settings_json(&p).unwrap();
        assert_eq!(
            loaded.core.partial_download_suffix,
            cfg.core.partial_download_suffix
        );
        assert!(loaded.app.remote_access_password.is_empty());
        assert!(loaded.app.proxy.http.password.is_empty());
        assert!(loaded.app.proxy.https.password.is_empty());
        assert!(loaded.app.proxy.socks5.password.is_empty());

        let rec = record_from_imported_url("https://example.org/test.iso", &cfg);
        let txt_p = dir.join("urls.txt");
        let csv_p = dir.join("downloads.csv");
        export_downloads_urls_txt(std::slice::from_ref(&rec), &txt_p).unwrap();
        export_downloads_csv(&[rec], &csv_p).unwrap();
        assert!(
            std::fs::read_to_string(&txt_p)
                .unwrap()
                .contains("test.iso")
        );
        assert!(
            std::fs::read_to_string(&csv_p)
                .unwrap()
                .contains("id,title,url")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn downloads_export_import_accepts_current_and_legacy_format_and_sanitizes() {
        let dir = std::env::temp_dir().join(format!("ferrite_dl_exp_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&dir);
        let cfg = FerriteConfig::default();

        let mut rec = record_from_imported_url("https://example.org/ubuntu.iso", &cfg);
        rec.target_dir = dir.clone();
        rec.file_name = "../../etc/passwd".into();
        rec.partial_suffix = "/../../evil".into();

        let current_path = dir.join("current.json");
        export_downloads_json(std::slice::from_ref(&rec), &current_path).unwrap();
        let raw_current = std::fs::read_to_string(&current_path).unwrap();
        assert!(raw_current.contains("\"FormatName\": \"Ferrite-Downloads\""));

        let imported_current = import_downloads_file(&current_path, &cfg).unwrap();
        assert_eq!(imported_current.len(), 1);
        assert_eq!(imported_current[0].file_name, "passwd");
        assert_eq!(imported_current[0].partial_suffix, "evil");
        assert_eq!(imported_current[0].output_path(), dir.join("passwd"));
        assert_eq!(imported_current[0].partial_path(), dir.join("passwdevil"));

        let legacy_path = dir.join("legacy.json");
        let legacy_raw = raw_current.replace("Ferrite-Downloads", "FerriteDM-Downloads");
        std::fs::write(&legacy_path, legacy_raw).unwrap();
        let imported_legacy = import_downloads_file(&legacy_path, &cfg).unwrap();
        assert_eq!(imported_legacy.len(), 1);
        assert_eq!(imported_legacy[0].file_name, "passwd");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
