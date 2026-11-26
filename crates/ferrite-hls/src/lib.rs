use aes::cipher::{BlockDecryptMut, KeyIvInit};
use anyhow::{Context, Result, bail};
use ferrite_core::{
    BandwidthLimiter, DownloadAbortReason, DownloadFileEntry, DownloadPriority, DownloadRecord,
    DownloadState, FerriteConfig, SegmentProgress, SegmentState, SslException,
    os_integration::set_mark_of_the_web,
};
use ferrite_http::{ControlCommand, build_http_client_for_url, build_request_headers};
use m3u8_rs::{MasterPlaylist, MediaPlaylist, Playlist};
use reqwest::Client;
use reqwest::header::{AUTHORIZATION, COOKIE, HeaderMap, PROXY_AUTHORIZATION, RANGE};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;
use url::Url;

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

fn lock_mutex<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

async fn write_bytes_atomic(tmp_path: &Path, final_path: &Path, data: &[u8]) -> Result<()> {
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(tmp_path)
        .await?;
    f.write_all(data).await?;
    f.flush().await?;
    f.sync_all().await?;
    drop(f);
    tokio::fs::rename(tmp_path, final_path).await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct HlsVariantInfo {
    pub url: String,
    pub bandwidth: u64,
    pub resolution: Option<String>,
    pub codecs: Option<String>,
    pub audio_group: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DashRepresentationInfo {
    pub id: String,
    pub mime_type: String,
    pub bandwidth: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub init_url: Option<String>,
    pub init_range: Option<(u64, Option<u64>)>,
    pub segment_urls: Vec<String>,
    pub segment_ranges: Vec<Option<(u64, Option<u64>)>>,
}

#[derive(Debug, Clone)]
struct HlsSegmentJob {
    seq_index: usize,
    url: String,
    byte_range: Option<(u64, u64)>, // (offset, length)
    key_uri: Option<String>,
    key_iv: Option<[u8; 16]>,
    seq_num: u64,
}

fn headers_for_url(origin_url: &str, target_url: &str, base_headers: &HeaderMap) -> HeaderMap {
    let mut headers = base_headers.clone();
    let same_origin = match (Url::parse(origin_url).ok(), Url::parse(target_url).ok()) {
        (Some(a), Some(b)) => {
            a.scheme().eq_ignore_ascii_case(b.scheme())
                && a.host_str()
                    .zip(b.host_str())
                    .is_some_and(|(ha, hb)| ha.eq_ignore_ascii_case(hb))
                && a.port_or_known_default() == b.port_or_known_default()
        }
        _ => true,
    };
    if !same_origin {
        headers.remove(COOKIE);
        headers.remove(AUTHORIZATION);
        headers.remove(PROXY_AUTHORIZATION);
    }
    headers
}

fn resolve_url(base: &Url, rel: &str) -> Result<String> {
    if rel.starts_with("http://") || rel.starts_with("https://") {
        return Ok(rel.to_string());
    }
    Ok(base.join(rel)?.to_string())
}

fn parse_iv(iv_opt: &Option<String>, seq_num: u64) -> [u8; 16] {
    if let Some(iv_str) = iv_opt {
        let stripped = iv_str
            .trim()
            .trim_start_matches("0x")
            .trim_start_matches("0X");
        let padded = format!("{:0>32}", stripped);
        if let Ok(bytes) = hex::decode(padded)
            && bytes.len() == 16
        {
            let mut out = [0u8; 16];
            out.copy_from_slice(&bytes);
            return out;
        }
    }
    let mut out = [0u8; 16];
    out[8..16].copy_from_slice(&seq_num.to_be_bytes());
    out
}

fn decrypt_aes128_cbc(ciphertext: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Result<Vec<u8>> {
    if ciphertext.is_empty() {
        return Ok(Vec::new());
    }
    let mut buf = ciphertext.to_vec();
    let pt = Aes128CbcDec::new(key.into(), iv.into())
        .decrypt_padded_mut::<aes::cipher::block_padding::Pkcs7>(&mut buf)
        .map_err(|_| anyhow::anyhow!("AES-128 PKCS7 decryption failed"))?;
    Ok(pt.to_vec())
}

pub async fn inspect_hls_variants(client: &Client, url: &str) -> Result<Vec<HlsVariantInfo>> {
    let body = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let base = Url::parse(url)?;
    match m3u8_rs::parse_playlist_res(&body) {
        Ok(Playlist::MasterPlaylist(master)) => Ok(extract_variants(&base, &master)),
        Ok(Playlist::MediaPlaylist(_)) => Ok(vec![HlsVariantInfo {
            url: url.to_string(),
            bandwidth: 0,
            resolution: None,
            codecs: None,
            audio_group: None,
        }]),
        Err(e) => bail!("Failed to parse M3U8 playlist: {e:?}"),
    }
}

pub async fn inspect_hls_variants_with_config(
    config: &FerriteConfig,
    ssl_exceptions: &[SslException],
    url: &str,
) -> Result<Vec<HlsVariantInfo>> {
    let client = build_http_client_for_url(config, ssl_exceptions, None, url)?;
    inspect_hls_variants(&client, url).await
}

fn extract_variants(base: &Url, master: &MasterPlaylist) -> Vec<HlsVariantInfo> {
    let mut list: Vec<HlsVariantInfo> = master
        .variants
        .iter()
        .filter(|v| !v.is_i_frame)
        .filter_map(|v| {
            let resolved = resolve_url(base, &v.uri).ok()?;
            Some(HlsVariantInfo {
                url: resolved,
                bandwidth: v.bandwidth,
                resolution: v.resolution.map(|r| format!("{}x{}", r.width, r.height)),
                codecs: v.codecs.clone(),
                audio_group: v.audio.clone(),
            })
        })
        .collect();
    list.sort_by_key(|a| std::cmp::Reverse(a.bandwidth));
    list
}

fn extract_audio_rendition(
    base: &Url,
    master: &MasterPlaylist,
    audio_group: &Option<String>,
) -> Option<String> {
    for alt in &master.alternatives {
        if matches!(alt.media_type, m3u8_rs::AlternativeMediaType::Audio) {
            if let Some(grp) = audio_group
                && &alt.group_id != grp
            {
                continue;
            }
            if let Some(ref uri) = alt.uri {
                return resolve_url(base, uri).ok();
            }
        }
    }
    None
}

fn resolve_ffmpeg_binary(config: &FerriteConfig) -> Option<PathBuf> {
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

pub async fn download_hls_stream<F>(
    mut record: DownloadRecord,
    config: &FerriteConfig,
    ssl_exceptions: &[SslException],
    limiter: BandwidthLimiter,
    control_rx: watch::Receiver<ControlCommand>,
    mut on_progress: F,
) -> DownloadRecord
where
    F: FnMut(&DownloadRecord) + Send,
{
    let client = match build_http_client_for_url(
        config,
        ssl_exceptions,
        record.http.user_agent.as_deref(),
        &record.url,
    ) {
        Ok(c) => c,
        Err(e) => {
            record.state = DownloadState::Error;
            record.last_error = Some(e.to_string());
            return record;
        }
    };

    record.state = DownloadState::Starting;
    on_progress(&record);

    let max_attempts = if config.core.auto_retry_failed && !record.flags.disable_auto_retry {
        config.core.max_auto_retries.max(1) + 1
    } else {
        1
    };

    for attempt in 0..max_attempts {
        let res = run_hls_inner(
            &client,
            &mut record,
            config,
            &limiter,
            control_rx.clone(),
            &mut on_progress,
        )
        .await;

        match res {
            Ok(OutcomeHls::Completed) => {
                if config.core.set_mark_of_the_web {
                    let out = record.output_path();
                    let url_c = record.url.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        set_mark_of_the_web(&out, &url_c, "Ferrite")
                    })
                    .await;
                }
                record.state = DownloadState::Completed;
                record.download_speed_bps = 0;
                record.connections_active = 0;
                record.completed_at = Some(chrono::Utc::now());
                record.files = vec![DownloadFileEntry {
                    index: 0,
                    relative_path: record.file_name.clone(),
                    size_bytes: record.downloaded_bytes,
                    downloaded_bytes: record.downloaded_bytes,
                    priority: DownloadPriority::Normal,
                    media_ready_to_play: true,
                    expected_hash: None,
                    computed_hash: None,
                }];
                on_progress(&record);
                return record;
            }
            Ok(OutcomeHls::Paused) => {
                record.state = DownloadState::Paused;
                record.download_speed_bps = 0;
                record.connections_active = 0;
                on_progress(&record);
                return record;
            }
            Ok(OutcomeHls::Cancelled) => {
                if config.core.delete_service_files_when_finished {
                    let work_dir = record.target_dir.join(format!(".{}.hls_tmp", record.uuid));
                    let _ = tokio::fs::remove_dir_all(&work_dir).await;
                }
                record.state = DownloadState::Stopped;
                record.abort_reason = DownloadAbortReason::AbortedByUser;
                record.download_speed_bps = 0;
                record.connections_active = 0;
                on_progress(&record);
                return record;
            }
            Err(e) => {
                record.retry_count += 1;
                record.last_error = Some(e.to_string());
                record.download_speed_bps = 0;
                record.connections_active = 0;
                if attempt + 1 < max_attempts {
                    tokio::time::sleep(std::time::Duration::from_secs(1 + attempt as u64)).await;
                    continue;
                }
                if config.core.delete_service_files_when_finished {
                    let work_dir = record.target_dir.join(format!(".{}.hls_tmp", record.uuid));
                    let _ = tokio::fs::remove_dir_all(&work_dir).await;
                }
                record.state = DownloadState::Error;
                on_progress(&record);
                return record;
            }
        }
    }
    record
}

enum OutcomeHls {
    Completed,
    Paused,
    Cancelled,
}

async fn run_hls_inner<F>(
    client: &Client,
    record: &mut DownloadRecord,
    config: &FerriteConfig,
    limiter: &BandwidthLimiter,
    control_rx: watch::Receiver<ControlCommand>,
    on_progress: &mut F,
) -> Result<OutcomeHls>
where
    F: FnMut(&DownloadRecord) + Send,
{
    let base_headers = build_request_headers(record, None);
    let raw = client
        .get(&record.url)
        .headers(base_headers.clone())
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let base = Url::parse(&record.url)?;
    let (media_url, audio_url) = match m3u8_rs::parse_playlist_res(&raw) {
        Ok(Playlist::MasterPlaylist(master)) => {
            let variants = extract_variants(&base, &master);
            let best = variants
                .first()
                .context("Master HLS playlist has no video variants")?;
            let aud = extract_audio_rendition(&base, &master, &best.audio_group);
            (best.url.clone(), aud)
        }
        Ok(Playlist::MediaPlaylist(_)) => (record.url.clone(), None),
        Err(e) => bail!("Invalid M3U8 playlist: {e:?}"),
    };

    tokio::fs::create_dir_all(&record.target_dir).await?;
    let work_dir = record.target_dir.join(format!(".{}.hls_tmp", record.uuid));
    tokio::fs::create_dir_all(&work_dir).await?;

    let video_raw_path = if audio_url.is_some() {
        work_dir.join("video_raw.ts")
    } else {
        record.partial_path()
    };

    let outcome = download_media_playlist_to_file(
        client,
        &media_url,
        &base_headers,
        &video_raw_path,
        &work_dir.join("v"),
        record,
        config,
        limiter,
        control_rx.clone(),
        on_progress,
    )
    .await?;

    if !matches!(outcome, OutcomeHls::Completed) {
        if matches!(outcome, OutcomeHls::Cancelled)
            && config.core.delete_service_files_when_finished
        {
            let _ = tokio::fs::remove_dir_all(&work_dir).await;
        }
        return Ok(outcome);
    }

    let output_path = record.output_path();
    if let Some(aud_url) = audio_url {
        let Some(ffmpeg) = resolve_ffmpeg_binary(config) else {
            bail!(
                "Separate HLS video and audio streams require FFmpeg for muxing, but ffmpeg was not found"
            );
        };
        let audio_raw_path = work_dir.join("audio_raw.aac");
        let a_outcome = download_media_playlist_to_file(
            client,
            &aud_url,
            &base_headers,
            &audio_raw_path,
            &work_dir.join("a"),
            record,
            config,
            limiter,
            control_rx,
            on_progress,
        )
        .await?;
        if !matches!(a_outcome, OutcomeHls::Completed) {
            if matches!(a_outcome, OutcomeHls::Cancelled)
                && config.core.delete_service_files_when_finished
            {
                let _ = tokio::fs::remove_dir_all(&work_dir).await;
            }
            return Ok(a_outcome);
        }

        record.state = DownloadState::PostProcessing;
        on_progress(record);
        let status = tokio::process::Command::new(ffmpeg)
            .args(["-y", "-i"])
            .arg(&video_raw_path)
            .args(["-i"])
            .arg(&audio_raw_path)
            .args(["-c", "copy"])
            .arg(&output_path)
            .status()
            .await?;
        if !status.success() {
            bail!("FFmpeg A/V mux exited with status {}", status);
        }
    } else if video_raw_path != output_path {
        if tokio::fs::try_exists(&output_path).await.unwrap_or(false) {
            let _ = tokio::fs::remove_file(&output_path).await;
        }
        tokio::fs::rename(&video_raw_path, &output_path).await?;
    }

    if config.core.delete_service_files_when_finished {
        let _ = tokio::fs::remove_dir_all(&work_dir).await;
    }
    Ok(OutcomeHls::Completed)
}

#[allow(clippy::too_many_arguments)]
async fn download_media_playlist_to_file<F>(
    client: &Client,
    playlist_url: &str,
    base_headers: &HeaderMap,
    dest_file: &Path,
    seg_dir: &Path,
    record: &mut DownloadRecord,
    config: &FerriteConfig,
    limiter: &BandwidthLimiter,
    mut control_rx: watch::Receiver<ControlCommand>,
    on_progress: &mut F,
) -> Result<OutcomeHls>
where
    F: FnMut(&DownloadRecord) + Send,
{
    tokio::fs::create_dir_all(seg_dir).await?;
    let mut seen_urls = HashSet::<String>::new();
    let mut seq_files: Vec<PathBuf> = Vec::new();
    let total_downloaded = Arc::new(AtomicU64::new(record.downloaded_bytes));
    let key_cache: Arc<Mutex<std::collections::HashMap<String, [u8; 16]>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    let workers = config
        .active_profile()
        .max_connections_per_server
        .clamp(1, 8) as usize;

    record.state = DownloadState::Downloading;

    // Live playlist reload loop (runs once for VOD with EXT-X-ENDLIST, polls for live streams!)
    let mut max_live_polls = 240u32;
    loop {
        let pl_bytes = client
            .get(playlist_url)
            .headers(headers_for_url(&record.url, playlist_url, base_headers))
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let base = Url::parse(playlist_url)?;
        let media: MediaPlaylist = match m3u8_rs::parse_media_playlist_res(&pl_bytes) {
            Ok(m) => m,
            Err(e) => bail!("Failed to parse media M3U8: {e:?}"),
        };

        let mut cur_key_uri: Option<String> = None;
        let mut cur_iv: Option<String> = None;
        let mut init_downloaded = false;
        let mut new_jobs: Vec<HlsSegmentJob> = Vec::new();
        let start_seq = media.media_sequence;
        let mut byte_offset = 0u64;
        let mut last_byte_range_uri: Option<String> = None;

        for (idx, seg) in media.segments.iter().enumerate() {
            if let Some(ref k) = seg.key {
                match k.method {
                    m3u8_rs::KeyMethod::None => {
                        cur_key_uri = None;
                        cur_iv = None;
                    }
                    m3u8_rs::KeyMethod::AES128 => {
                        let uri = k.uri.as_deref().context("HLS AES-128 key missing URI")?;
                        cur_key_uri = Some(resolve_url(&base, uri)?);
                        cur_iv = k.iv.clone();
                    }
                    ref other => {
                        bail!("Unsupported HLS encryption method: {other:?}");
                    }
                }
            }

            if !init_downloaded && let Some(ref map) = seg.map {
                let map_url = resolve_url(&base, &map.uri)?;
                let map_range = map
                    .byte_range
                    .as_ref()
                    .map(|r| (r.offset.unwrap_or(0), r.length));
                let map_key = match map_range {
                    Some((off, len)) => format!("MAP:{map_url}@{len}#{off}"),
                    None => format!("MAP:{map_url}"),
                };
                if seen_urls.insert(map_key) {
                    let map_part_path = seg_dir.join("000000_init.mp4.part");
                    if !tokio::fs::try_exists(&map_part_path).await.unwrap_or(false) {
                        let mut req = client.get(&map_url).headers(headers_for_url(
                            &record.url,
                            &map_url,
                            base_headers,
                        ));
                        if let Some((off, len)) = map_range
                            && len > 0
                        {
                            req = req.header(RANGE, format!("bytes={}-{}", off, off + len - 1));
                        }
                        let b = req.send().await?.error_for_status()?.bytes().await?;
                        limiter.acquire(b.len()).await;
                        let map_tmp_path = seg_dir.join("000000_init.mp4.tmp");
                        write_bytes_atomic(&map_tmp_path, &map_part_path, &b).await?;
                        total_downloaded.fetch_add(b.len() as u64, Ordering::Relaxed);
                    }
                    seq_files.push(map_part_path);
                }
                init_downloaded = true;
            }

            let seg_url = resolve_url(&base, &seg.uri)?;
            let byte_range = seg.byte_range.as_ref().map(|r| {
                if r.offset.is_none() && last_byte_range_uri.as_deref() != Some(seg_url.as_str()) {
                    byte_offset = 0;
                }
                let off = r.offset.unwrap_or(byte_offset);
                byte_offset = off.saturating_add(r.length);
                last_byte_range_uri = Some(seg_url.clone());
                (off, r.length)
            });
            let range_key = byte_range
                .map(|(off, len)| format!("@{len}#{off}"))
                .unwrap_or_default();
            let uniq = format!("{seg_url}{range_key}");
            if seen_urls.insert(uniq) {
                let seq_num = start_seq + idx as u64;
                let seq_idx = seq_files.len() + new_jobs.len();
                new_jobs.push(HlsSegmentJob {
                    seq_index: seq_idx,
                    url: seg_url,
                    byte_range,
                    key_uri: cur_key_uri.clone(),
                    key_iv: cur_key_uri.as_ref().map(|_| parse_iv(&cur_iv, seq_num)),
                    seq_num,
                });
            }
        }

        if !new_jobs.is_empty() {
            record.segments = new_jobs
                .iter()
                .map(|j| SegmentProgress {
                    id: j.seq_index as u32,
                    start: j.seq_index as u64,
                    end: j.seq_index as u64,
                    downloaded: 0,
                    state: SegmentState::Pending,
                    speed_bps: 0,
                    mirror_url: Some(j.url.clone()),
                })
                .collect();

            let jobs_q = Arc::new(Mutex::new(std::collections::VecDeque::from(
                new_jobs.clone(),
            )));
            let paths_map = Arc::new(Mutex::new(vec![None::<PathBuf>; new_jobs.len()]));
            let cancel_flag = Arc::new(AtomicBool::new(false));
            let pause_flag = Arc::new(AtomicBool::new(false));
            let err_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
            let origin_url = Arc::new(record.url.clone());

            let mut handles = Vec::new();
            for _ in 0..workers {
                let client = client.clone();
                let jobs_q = Arc::clone(&jobs_q);
                let paths_map = Arc::clone(&paths_map);
                let seg_dir = seg_dir.to_path_buf();
                let limiter = limiter.clone();
                let total_downloaded = Arc::clone(&total_downloaded);
                let key_cache = Arc::clone(&key_cache);
                let cancel_flag = Arc::clone(&cancel_flag);
                let pause_flag = Arc::clone(&pause_flag);
                let err_slot = Arc::clone(&err_slot);
                let base_headers = base_headers.clone();
                let origin_url = Arc::clone(&origin_url);
                let offset_base = seq_files.len();

                handles.push(tokio::spawn(async move {
                    loop {
                        if cancel_flag.load(Ordering::Relaxed) || pause_flag.load(Ordering::Relaxed)
                        {
                            break;
                        }
                        let next_job = lock_mutex(&jobs_q).pop_front();
                        let Some(job) = next_job else {
                            break;
                        };
                        let local_idx = job.seq_index - offset_base;
                        let part_path = seg_dir.join(format!("{:06}.ts.part", job.seq_num));
                        let tmp_path = seg_dir.join(format!("{:06}.ts.tmp", job.seq_num));

                        if !tokio::fs::try_exists(&part_path).await.unwrap_or(false) {
                            let mut req = client.get(&job.url).headers(headers_for_url(
                                &origin_url,
                                &job.url,
                                &base_headers,
                            ));
                            if let Some((off, len)) = job.byte_range
                                && len > 0
                            {
                                req = req.header(RANGE, format!("bytes={}-{}", off, off + len - 1));
                            }
                            let bytes_res = match req.send().await {
                                Ok(r) => match r.error_for_status() {
                                    Ok(ok) => ok.bytes().await,
                                    Err(e) => Err(e),
                                },
                                Err(e) => Err(e),
                            };
                            let mut raw_bytes = match bytes_res {
                                Ok(b) => b.to_vec(),
                                Err(e) => {
                                    let mut g = lock_mutex(&err_slot);
                                    if g.is_none() {
                                        *g = Some(format!("HLS segment {} error: {e}", job.url));
                                    }
                                    cancel_flag.store(true, Ordering::Relaxed);
                                    break;
                                }
                            };
                            limiter.acquire(raw_bytes.len()).await;

                            if let (Some(ref key_url), Some(ref iv)) = (job.key_uri, job.key_iv) {
                                let cached = lock_mutex(&key_cache).get(key_url).copied();
                                let key = match cached {
                                    Some(k) => k,
                                    None => match client
                                        .get(key_url)
                                        .headers(headers_for_url(
                                            &origin_url,
                                            key_url,
                                            &base_headers,
                                        ))
                                        .send()
                                        .await
                                    {
                                        Ok(r) => match r.error_for_status() {
                                            Ok(ok_r) => match ok_r.bytes().await {
                                                Ok(kb) if kb.len() == 16 => {
                                                    let mut arr = [0u8; 16];
                                                    arr.copy_from_slice(&kb);
                                                    lock_mutex(&key_cache)
                                                        .insert(key_url.clone(), arr);
                                                    arr
                                                }
                                                Ok(kb) => {
                                                    let mut g = lock_mutex(&err_slot);
                                                    if g.is_none() {
                                                        *g = Some(format!(
                                                            "Invalid AES-128 key length ({} bytes)",
                                                            kb.len()
                                                        ));
                                                    }
                                                    cancel_flag.store(true, Ordering::Relaxed);
                                                    break;
                                                }
                                                Err(e) => {
                                                    let mut g = lock_mutex(&err_slot);
                                                    if g.is_none() {
                                                        *g = Some(format!(
                                                            "Key body read error: {e}"
                                                        ));
                                                    }
                                                    cancel_flag.store(true, Ordering::Relaxed);
                                                    break;
                                                }
                                            },
                                            Err(e) => {
                                                let mut g = lock_mutex(&err_slot);
                                                if g.is_none() {
                                                    *g = Some(format!("Key fetch HTTP error: {e}"));
                                                }
                                                cancel_flag.store(true, Ordering::Relaxed);
                                                break;
                                            }
                                        },
                                        Err(e) => {
                                            let mut g = lock_mutex(&err_slot);
                                            if g.is_none() {
                                                *g = Some(format!("Key fetch error: {e}"));
                                            }
                                            cancel_flag.store(true, Ordering::Relaxed);
                                            break;
                                        }
                                    },
                                };
                                match decrypt_aes128_cbc(&raw_bytes, &key, iv) {
                                    Ok(pt) => raw_bytes = pt,
                                    Err(e) => {
                                        let mut g = lock_mutex(&err_slot);
                                        if g.is_none() {
                                            *g = Some(format!("Segment decrypt error: {e}"));
                                        }
                                        cancel_flag.store(true, Ordering::Relaxed);
                                        break;
                                    }
                                }
                            }

                            if let Err(e) =
                                write_bytes_atomic(&tmp_path, &part_path, &raw_bytes).await
                            {
                                let _ = tokio::fs::remove_file(&tmp_path).await;
                                let mut g = lock_mutex(&err_slot);
                                if g.is_none() {
                                    *g = Some(format!("Write segment failed: {e}"));
                                }
                                cancel_flag.store(true, Ordering::Relaxed);
                                break;
                            }
                            total_downloaded.fetch_add(raw_bytes.len() as u64, Ordering::Relaxed);
                        }

                        lock_mutex(&paths_map)[local_idx] = Some(part_path);
                    }
                }));
            }

            let mut last_bytes = total_downloaded.load(Ordering::Relaxed);
            let mut last_tick = tokio::time::Instant::now();
            let mut slow_accum_secs = 0.0f64;
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {}
                    changed = control_rx.changed() => {
                        if changed.is_ok() {
                            match *control_rx.borrow() {
                                ControlCommand::Pause => pause_flag.store(true, Ordering::Relaxed),
                                ControlCommand::Cancel => cancel_flag.store(true, Ordering::Relaxed),
                                ControlCommand::Run => {}
                            }
                        }
                    }
                }
                let now = tokio::time::Instant::now();
                let dt = now.duration_since(last_tick).as_secs_f64().max(0.05);
                let cur = total_downloaded.load(Ordering::Relaxed);
                let speed = (cur.saturating_sub(last_bytes) as f64 / dt) as u64;
                last_bytes = cur;
                last_tick = now;

                record.downloaded_bytes = cur;
                record.download_speed_bps = speed;
                record.connections_active = workers as u32;
                on_progress(record);

                if config.core.detect_unacceptably_slow && !pause_flag.load(Ordering::Relaxed) {
                    if speed < config.core.unacceptably_slow_speed_bps {
                        slow_accum_secs += dt;
                        if slow_accum_secs >= config.core.unacceptably_slow_interval_secs as f64 {
                            record.abort_reason = DownloadAbortReason::SlowDownload;
                            *lock_mutex(&err_slot) = Some(format!(
                                "HLS speed dropped below {} B/s for {}s",
                                config.core.unacceptably_slow_speed_bps,
                                config.core.unacceptably_slow_interval_secs
                            ));
                            cancel_flag.store(true, Ordering::Relaxed);
                            break;
                        }
                    } else {
                        slow_accum_secs = 0.0;
                    }
                }

                if handles.iter().all(|h| h.is_finished()) {
                    break;
                }
            }

            for h in handles {
                let _ = h.await;
            }
            if matches!(*control_rx.borrow(), ControlCommand::Pause) {
                return Ok(OutcomeHls::Paused);
            }
            if matches!(*control_rx.borrow(), ControlCommand::Cancel) {
                return Ok(OutcomeHls::Cancelled);
            }
            if let Some(msg) = lock_mutex(&err_slot).take() {
                bail!(msg);
            }
            let finished_paths = lock_mutex(&paths_map);
            for p in finished_paths.iter().flatten() {
                seq_files.push(p.clone());
            }
        }

        if media.end_list || max_live_polls == 0 {
            break;
        }
        max_live_polls -= 1;
        let wait_secs = (media.target_duration as u64).clamp(1, 5);
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(wait_secs)) => {}
            _ = control_rx.changed() => {
                match *control_rx.borrow() {
                    ControlCommand::Pause => return Ok(OutcomeHls::Paused),
                    ControlCommand::Cancel => return Ok(OutcomeHls::Cancelled),
                    ControlCommand::Run => {}
                }
            }
        }
    }

    // Concatenate segments cleanly into dest_file via blocking task
    let dest_file_path = dest_file.to_path_buf();
    let seq_files_buf = seq_files.clone();
    let total_written = tokio::task::spawn_blocking(move || -> Result<u64> {
        use std::io::Write;
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&dest_file_path)?;
        let mut written = 0u64;
        for sp in &seq_files_buf {
            let mut f = std::fs::File::open(sp)?;
            let n = std::io::copy(&mut f, &mut out)?;
            written += n;
        }
        out.flush()?;
        out.sync_all()?;
        Ok(written)
    })
    .await
    .map_err(|e| anyhow::anyhow!("Segment concatenation task panicked: {e}"))??;

    record.downloaded_bytes = total_written;
    record.total_bytes = Some(total_written);
    Ok(OutcomeHls::Completed)
}

fn resolve_node_base_url(parent_base: &Url, node: roxmltree::Node<'_, '_>) -> Url {
    if let Some(raw) = node
        .children()
        .find(|c| c.has_tag_name("BaseURL"))
        .and_then(|n| n.text())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        && let Ok(joined) = resolve_url(parent_base, raw)
        && let Ok(parsed) = Url::parse(&joined)
    {
        return parsed;
    }
    parent_base.clone()
}

fn parse_iso8601_duration(raw: &str) -> Option<f64> {
    let s = raw.trim();
    let rest = s.strip_prefix('P')?;
    let (date_part, time_part) = rest.split_once('T').unwrap_or((rest, ""));
    let mut total_secs = 0.0f64;

    if !date_part.is_empty() {
        let mut num = String::new();
        for ch in date_part.chars() {
            if ch.is_ascii_digit() || ch == '.' {
                num.push(ch);
            } else {
                let val: f64 = num.parse().ok()?;
                num.clear();
                match ch {
                    'D' => total_secs += val * 86_400.0,
                    'W' => total_secs += val * 604_800.0,
                    'M' => total_secs += val * 2_592_000.0,
                    'Y' => total_secs += val * 31_536_000.0,
                    _ => return None,
                }
            }
        }
    }

    if !time_part.is_empty() {
        let mut num = String::new();
        for ch in time_part.chars() {
            if ch.is_ascii_digit() || ch == '.' {
                num.push(ch);
            } else {
                let val: f64 = num.parse().ok()?;
                num.clear();
                match ch {
                    'H' => total_secs += val * 3600.0,
                    'M' => total_secs += val * 60.0,
                    'S' => total_secs += val,
                    _ => return None,
                }
            }
        }
    }

    if total_secs > 0.0 && total_secs.is_finite() {
        Some(total_secs)
    } else {
        None
    }
}

fn parse_dash_byte_range(raw: &str) -> Option<(u64, Option<u64>)> {
    let trimmed = raw.trim();
    let (start_s, end_s) = trimmed.split_once('-')?;
    let start = start_s.trim().parse::<u64>().ok()?;
    let end = if end_s.trim().is_empty() {
        None
    } else {
        Some(end_s.trim().parse::<u64>().ok()?)
    };
    Some((start, end))
}

fn format_dash_var(val: u64, spec: &str) -> String {
    if let Some(fmt) = spec.strip_prefix('%')
        && let Some(width_s) = fmt.strip_suffix('d')
        && let Ok(width) = width_s.trim_start_matches('0').parse::<usize>()
    {
        return format!("{val:0width$}");
    }
    val.to_string()
}

fn expand_dash_template(
    template: &str,
    rep_id: &str,
    bandwidth: u64,
    number: Option<u64>,
    time: Option<u64>,
) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut chars = template.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if ch != '$' {
            out.push(ch);
            continue;
        }
        if let Some((_, '$')) = chars.peek().copied() {
            chars.next();
            out.push('$');
            continue;
        }
        let rest = &template[idx + 1..];
        if let Some(end_rel) = rest.find('$') {
            let token = &rest[..end_rel];
            let advance_to = idx + 1 + end_rel + 1;
            while let Some(&(next_idx, _)) = chars.peek() {
                if next_idx < advance_to {
                    chars.next();
                } else {
                    break;
                }
            }
            if token == "RepresentationID" {
                out.push_str(rep_id);
            } else if token == "Bandwidth" {
                out.push_str(&bandwidth.to_string());
            } else if let Some(spec) = token.strip_prefix("Bandwidth") {
                if spec.starts_with('%') {
                    out.push_str(&format_dash_var(bandwidth, spec));
                } else {
                    out.push('$');
                    out.push_str(token);
                    out.push('$');
                }
            } else if token == "Number" {
                if let Some(n) = number {
                    out.push_str(&n.to_string());
                } else {
                    out.push_str("$Number$");
                }
            } else if let Some(spec) = token.strip_prefix("Number") {
                if let Some(n) = number
                    && spec.starts_with('%')
                {
                    out.push_str(&format_dash_var(n, spec));
                } else {
                    out.push('$');
                    out.push_str(token);
                    out.push('$');
                }
            } else if token == "Time" {
                if let Some(t) = time {
                    out.push_str(&t.to_string());
                } else {
                    out.push_str("$Time$");
                }
            } else if let Some(spec) = token.strip_prefix("Time") {
                if let Some(t) = time
                    && spec.starts_with('%')
                {
                    out.push_str(&format_dash_var(t, spec));
                } else {
                    out.push('$');
                    out.push_str(token);
                    out.push('$');
                }
            } else {
                out.push('$');
                out.push_str(token);
                out.push('$');
            }
        } else {
            out.push('$');
        }
    }
    out
}

/// DASH MPD parser supporting SegmentTemplate (timeline & duration-based VOD),
/// SegmentList, SegmentBase byte-ranges, and BaseURL inheritance.
pub fn parse_dash_mpd(mpd_url: &str, xml_text: &str) -> Result<Vec<DashRepresentationInfo>> {
    let base = Url::parse(mpd_url)?;
    let doc = roxmltree::Document::parse(xml_text)?;
    let mpd_node = doc
        .descendants()
        .find(|n| n.has_tag_name("MPD"))
        .unwrap_or_else(|| doc.root_element());
    let mpd_base = resolve_node_base_url(&base, mpd_node);
    let mpd_duration_secs = mpd_node
        .attribute("mediaPresentationDuration")
        .and_then(parse_iso8601_duration);

    let mut periods: Vec<roxmltree::Node<'_, '_>> = mpd_node
        .children()
        .filter(|n| n.has_tag_name("Period"))
        .collect();
    if periods.is_empty() {
        periods.push(mpd_node);
    }

    let mut reps = Vec::new();

    for period in periods {
        let period_base = if period == mpd_node {
            mpd_base.clone()
        } else {
            resolve_node_base_url(&mpd_base, period)
        };
        let period_duration_secs = period
            .attribute("duration")
            .and_then(parse_iso8601_duration)
            .or(mpd_duration_secs);

        for adap in period
            .children()
            .filter(|n| n.has_tag_name("AdaptationSet"))
        {
            let adap_base = resolve_node_base_url(&period_base, adap);
            let adap_mime = adap.attribute("mimeType").unwrap_or("").to_string();
            let adap_template = adap.children().find(|c| c.has_tag_name("SegmentTemplate"));
            let adap_seg_base = adap.children().find(|c| c.has_tag_name("SegmentBase"));

            for rep in adap.children().filter(|c| c.has_tag_name("Representation")) {
                let id = rep.attribute("id").unwrap_or("0").to_string();
                let mime = rep
                    .attribute("mimeType")
                    .map(str::to_string)
                    .unwrap_or_else(|| adap_mime.clone());
                let bandwidth = rep
                    .attribute("bandwidth")
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                let width = rep.attribute("width").and_then(|s| s.parse::<u32>().ok());
                let height = rep.attribute("height").and_then(|s| s.parse::<u32>().ok());

                let rep_base_url = resolve_node_base_url(&adap_base, rep);
                let rep_has_base_url = rep.children().any(|c| c.has_tag_name("BaseURL"))
                    || adap.children().any(|c| c.has_tag_name("BaseURL"));

                let rep_tmpl = rep.children().find(|c| c.has_tag_name("SegmentTemplate"));
                let tmpl_primary = rep_tmpl.or(adap_template);

                let mut init_url = None;
                let mut init_range: Option<(u64, Option<u64>)> = None;
                let mut segment_urls = Vec::new();
                let mut segment_ranges: Vec<Option<(u64, Option<u64>)>> = Vec::new();

                if let Some(t) = tmpl_primary {
                    let tmpl_attr = |name: &str| -> Option<&str> {
                        rep_tmpl
                            .and_then(|n| n.attribute(name))
                            .or_else(|| adap_template.and_then(|n| n.attribute(name)))
                    };
                    if let Some(init_attr) = tmpl_attr("initialization") {
                        let s = expand_dash_template(init_attr, &id, bandwidth, None, None);
                        init_url = resolve_url(&rep_base_url, &s).ok();
                    }
                    if let Some(media_attr) = tmpl_attr("media") {
                        let start_num = tmpl_attr("startNumber")
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(1);
                        let timeline = rep_tmpl
                            .and_then(|n| n.children().find(|c| c.has_tag_name("SegmentTimeline")))
                            .or_else(|| {
                                adap_template.and_then(|n| {
                                    n.children().find(|c| c.has_tag_name("SegmentTimeline"))
                                })
                            })
                            .or_else(|| t.children().find(|c| c.has_tag_name("SegmentTimeline")));

                        if let Some(timeline) = timeline {
                            let mut cur_t = 0u64;
                            let mut cur_num = start_num;
                            for seg in timeline.children().filter(|c| c.has_tag_name("S")) {
                                if let Some(t_val) =
                                    seg.attribute("t").and_then(|s| s.parse::<u64>().ok())
                                {
                                    cur_t = t_val;
                                }
                                let d_val = seg
                                    .attribute("d")
                                    .and_then(|s| s.parse::<u64>().ok())
                                    .unwrap_or(0);
                                let repeat = seg
                                    .attribute("r")
                                    .and_then(|s| s.parse::<u64>().ok())
                                    .unwrap_or(0);
                                for _ in 0..=repeat {
                                    let u = expand_dash_template(
                                        media_attr,
                                        &id,
                                        bandwidth,
                                        Some(cur_num),
                                        Some(cur_t),
                                    );
                                    if let Ok(full) = resolve_url(&rep_base_url, &u) {
                                        segment_urls.push(full);
                                        segment_ranges.push(None);
                                    }
                                    cur_t += d_val;
                                    cur_num += 1;
                                }
                            }
                        } else if let (Some(dur), Some(total_secs)) = (
                            tmpl_attr("duration").and_then(|s| s.parse::<u64>().ok()),
                            period_duration_secs,
                        ) && dur > 0
                        {
                            let timescale = tmpl_attr("timescale")
                                .and_then(|s| s.parse::<u64>().ok())
                                .filter(|&ts| ts > 0)
                                .unwrap_or(1);
                            let seg_dur_secs = dur as f64 / timescale as f64;
                            let count =
                                ((total_secs / seg_dur_secs).ceil() as u64).clamp(1, 100_000);
                            for i in 0..count {
                                let cur_num = start_num + i;
                                let cur_t = i.saturating_mul(dur);
                                let u = expand_dash_template(
                                    media_attr,
                                    &id,
                                    bandwidth,
                                    Some(cur_num),
                                    Some(cur_t),
                                );
                                if let Ok(full) = resolve_url(&rep_base_url, &u) {
                                    segment_urls.push(full);
                                    segment_ranges.push(None);
                                }
                            }
                        } else {
                            // Single template entry fallback if duration/timeline unknown
                            let u = expand_dash_template(
                                media_attr,
                                &id,
                                bandwidth,
                                Some(start_num),
                                Some(0),
                            );
                            if let Ok(full) = resolve_url(&rep_base_url, &u) {
                                segment_urls.push(full);
                                segment_ranges.push(None);
                            }
                        }
                    }
                } else if let Some(slist) = rep.children().find(|c| c.has_tag_name("SegmentList")) {
                    if let Some(init_node) =
                        slist.children().find(|c| c.has_tag_name("Initialization"))
                    {
                        init_range = init_node.attribute("range").and_then(parse_dash_byte_range);
                        if let Some(src) = init_node.attribute("sourceURL") {
                            init_url = resolve_url(&rep_base_url, src).ok();
                        } else if init_range.is_some() {
                            init_url = Some(rep_base_url.to_string());
                        }
                    }
                    for su in slist.children().filter(|c| c.has_tag_name("SegmentURL")) {
                        let range = su.attribute("mediaRange").and_then(parse_dash_byte_range);
                        let media_url = if let Some(m) = su.attribute("media") {
                            resolve_url(&rep_base_url, m).ok()
                        } else if range.is_some() {
                            Some(rep_base_url.to_string())
                        } else {
                            None
                        };
                        if let Some(full) = media_url {
                            segment_urls.push(full);
                            segment_ranges.push(range);
                        }
                    }
                } else if let Some(sbase) = rep
                    .children()
                    .find(|c| c.has_tag_name("SegmentBase"))
                    .or(adap_seg_base)
                {
                    let base_str = rep_base_url.to_string();
                    if let Some(init_node) =
                        sbase.children().find(|c| c.has_tag_name("Initialization"))
                    {
                        let range = init_node.attribute("range").and_then(parse_dash_byte_range);
                        let resolved_init = init_node
                            .attribute("sourceURL")
                            .and_then(|s| resolve_url(&rep_base_url, s).ok())
                            .unwrap_or_else(|| base_str.clone());
                        init_url = Some(resolved_init.clone());
                        init_range = range;
                        if resolved_init == base_str
                            && let Some((_, Some(init_end))) = range
                        {
                            segment_urls.push(base_str);
                            segment_ranges.push(Some((init_end.saturating_add(1), None)));
                        } else {
                            segment_urls.push(base_str);
                            segment_ranges.push(None);
                        }
                    } else {
                        segment_urls.push(base_str);
                        segment_ranges.push(None);
                    }
                } else if rep_has_base_url {
                    segment_urls.push(rep_base_url.to_string());
                    segment_ranges.push(None);
                }

                reps.push(DashRepresentationInfo {
                    id,
                    mime_type: mime,
                    bandwidth,
                    width,
                    height,
                    init_url,
                    init_range,
                    segment_urls,
                    segment_ranges,
                });
            }
        }
    }

    Ok(reps)
}

pub async fn download_dash_stream<F>(
    mut record: DownloadRecord,
    config: &FerriteConfig,
    ssl_exceptions: &[SslException],
    limiter: BandwidthLimiter,
    control_rx: watch::Receiver<ControlCommand>,
    mut on_progress: F,
) -> DownloadRecord
where
    F: FnMut(&DownloadRecord) + Send,
{
    let client = match build_http_client_for_url(
        config,
        ssl_exceptions,
        record.http.user_agent.as_deref(),
        &record.url,
    ) {
        Ok(c) => c,
        Err(e) => {
            record.state = DownloadState::Error;
            record.last_error = Some(e.to_string());
            return record;
        }
    };

    record.state = DownloadState::Starting;
    on_progress(&record);

    let base_headers = build_request_headers(&record, None);
    let xml = match client
        .get(&record.url)
        .headers(base_headers.clone())
        .send()
        .await
    {
        Ok(r) => match r.error_for_status() {
            Ok(ok_r) => match ok_r.text().await {
                Ok(t) => t,
                Err(e) => {
                    record.state = DownloadState::Error;
                    record.last_error = Some(e.to_string());
                    return record;
                }
            },
            Err(e) => {
                record.state = DownloadState::Error;
                record.last_error = Some(e.to_string());
                return record;
            }
        },
        Err(e) => {
            record.state = DownloadState::Error;
            record.last_error = Some(e.to_string());
            return record;
        }
    };

    let reps = match parse_dash_mpd(&record.url, &xml) {
        Ok(r) if !r.is_empty() => r,
        Ok(_) => {
            record.state = DownloadState::Error;
            record.last_error = Some("No DASH representations found in MPD".into());
            return record;
        }
        Err(e) => {
            record.state = DownloadState::Error;
            record.last_error = Some(e.to_string());
            return record;
        }
    };

    let best_video = reps
        .iter()
        .filter(|r| r.mime_type.starts_with("video/") || r.width.is_some())
        .max_by_key(|r| r.bandwidth)
        .or_else(|| {
            reps.iter()
                .filter(|r| !r.mime_type.starts_with("audio/"))
                .max_by_key(|r| r.bandwidth)
        })
        .cloned();
    let best_audio = reps
        .iter()
        .filter(|r| r.mime_type.starts_with("audio/"))
        .max_by_key(|r| r.bandwidth)
        .cloned();

    if let Err(e) = tokio::fs::create_dir_all(&record.target_dir).await {
        record.state = DownloadState::Error;
        record.last_error = Some(format!("Create dir failed: {e}"));
        return record;
    }
    let work_dir = record.target_dir.join(format!(".{}.dash_tmp", record.uuid));
    if let Err(e) = tokio::fs::create_dir_all(&work_dir).await {
        record.state = DownloadState::Error;
        record.last_error = Some(format!("Create work dir failed: {e}"));
        return record;
    }

    let mut total_done = 0u64;
    let video_path = work_dir.join("video_dash.mp4");
    let audio_path = work_dir.join("audio_dash.m4a");
    let separate_audio = best_audio
        .as_ref()
        .is_some_and(|ar| best_video.as_ref().is_some_and(|v| v.id != ar.id));

    if separate_audio && resolve_ffmpeg_binary(config).is_none() {
        if config.core.delete_service_files_when_finished {
            let _ = tokio::fs::remove_dir_all(&work_dir).await;
        }
        record.state = DownloadState::Error;
        record.last_error = Some(
            "Separate DASH video and audio streams require FFmpeg for muxing, but ffmpeg was not found"
                .into(),
        );
        on_progress(&record);
        return record;
    }

    record.state = DownloadState::Downloading;
    let mut ctrl_rx = control_rx;
    if let Some(ref vr) = best_video {
        match fetch_dash_rep(
            &client,
            &mut record,
            config,
            &base_headers,
            vr,
            &video_path,
            &limiter,
            &mut ctrl_rx,
            &mut total_done,
            &mut on_progress,
        )
        .await
        {
            Ok(OutcomeHls::Completed) => {}
            Ok(OutcomeHls::Paused) => {
                record.state = DownloadState::Paused;
                record.download_speed_bps = 0;
                record.connections_active = 0;
                on_progress(&record);
                return record;
            }
            Ok(OutcomeHls::Cancelled) => {
                if config.core.delete_service_files_when_finished {
                    let _ = tokio::fs::remove_dir_all(&work_dir).await;
                }
                record.state = DownloadState::Stopped;
                record.abort_reason = DownloadAbortReason::AbortedByUser;
                record.download_speed_bps = 0;
                record.connections_active = 0;
                on_progress(&record);
                return record;
            }
            Err(e) => {
                if config.core.delete_service_files_when_finished {
                    let _ = tokio::fs::remove_dir_all(&work_dir).await;
                }
                record.state = DownloadState::Error;
                record.download_speed_bps = 0;
                record.connections_active = 0;
                record.last_error = Some(e.to_string());
                on_progress(&record);
                return record;
            }
        }
    }

    let output_path = record.output_path();
    if let Some(ref ar) = best_audio {
        if best_video.as_ref().map(|v| v.id != ar.id).unwrap_or(true) {
            let aud_dest = &audio_path;
            match fetch_dash_rep(
                &client,
                &mut record,
                config,
                &base_headers,
                ar,
                aud_dest,
                &limiter,
                &mut ctrl_rx,
                &mut total_done,
                &mut on_progress,
            )
            .await
            {
                Ok(OutcomeHls::Completed) => {}
                Ok(OutcomeHls::Paused) => {
                    record.state = DownloadState::Paused;
                    record.download_speed_bps = 0;
                    record.connections_active = 0;
                    on_progress(&record);
                    return record;
                }
                Ok(OutcomeHls::Cancelled) => {
                    if config.core.delete_service_files_when_finished {
                        let _ = tokio::fs::remove_dir_all(&work_dir).await;
                    }
                    record.state = DownloadState::Stopped;
                    record.abort_reason = DownloadAbortReason::AbortedByUser;
                    record.download_speed_bps = 0;
                    record.connections_active = 0;
                    on_progress(&record);
                    return record;
                }
                Err(e) => {
                    if config.core.delete_service_files_when_finished {
                        let _ = tokio::fs::remove_dir_all(&work_dir).await;
                    }
                    record.state = DownloadState::Error;
                    record.download_speed_bps = 0;
                    record.connections_active = 0;
                    record.last_error = Some(e.to_string());
                    on_progress(&record);
                    return record;
                }
            }

            if best_video.is_some() {
                let Some(ffmpeg) = resolve_ffmpeg_binary(config) else {
                    if config.core.delete_service_files_when_finished {
                        let _ = tokio::fs::remove_dir_all(&work_dir).await;
                    }
                    record.state = DownloadState::Error;
                    record.last_error = Some(
                        "Separate DASH video and audio streams require FFmpeg for muxing, but ffmpeg was not found"
                            .into(),
                    );
                    on_progress(&record);
                    return record;
                };
                record.state = DownloadState::PostProcessing;
                record.download_speed_bps = 0;
                record.connections_active = 0;
                on_progress(&record);
                match tokio::process::Command::new(ffmpeg)
                    .args(["-y", "-i"])
                    .arg(&video_path)
                    .args(["-i"])
                    .arg(&audio_path)
                    .args(["-c", "copy"])
                    .arg(&output_path)
                    .status()
                    .await
                {
                    Ok(st) if st.success() => {}
                    Ok(st) => {
                        if config.core.delete_service_files_when_finished {
                            let _ = tokio::fs::remove_dir_all(&work_dir).await;
                        }
                        record.state = DownloadState::Error;
                        record.last_error =
                            Some(format!("FFmpeg DASH A/V mux exited with status {st}"));
                        on_progress(&record);
                        return record;
                    }
                    Err(e) => {
                        if config.core.delete_service_files_when_finished {
                            let _ = tokio::fs::remove_dir_all(&work_dir).await;
                        }
                        record.state = DownloadState::Error;
                        record.last_error =
                            Some(format!("Failed to launch FFmpeg for DASH mux: {e}"));
                        on_progress(&record);
                        return record;
                    }
                }
            } else {
                if tokio::fs::try_exists(&output_path).await.unwrap_or(false) {
                    let _ = tokio::fs::remove_file(&output_path).await;
                }
                if let Err(e) = tokio::fs::rename(&audio_path, &output_path).await {
                    record.state = DownloadState::Error;
                    record.last_error = Some(format!("Finalize rename failed: {e}"));
                    on_progress(&record);
                    return record;
                }
            }
        } else {
            if tokio::fs::try_exists(&output_path).await.unwrap_or(false) {
                let _ = tokio::fs::remove_file(&output_path).await;
            }
            if let Err(e) = tokio::fs::rename(&video_path, &output_path).await {
                record.state = DownloadState::Error;
                record.last_error = Some(format!("Finalize rename failed: {e}"));
                on_progress(&record);
                return record;
            }
        }
    } else if best_video.is_some() {
        if tokio::fs::try_exists(&output_path).await.unwrap_or(false) {
            let _ = tokio::fs::remove_file(&output_path).await;
        }
        if let Err(e) = tokio::fs::rename(&video_path, &output_path).await {
            record.state = DownloadState::Error;
            record.last_error = Some(format!("Finalize rename failed: {e}"));
            on_progress(&record);
            return record;
        }
    }

    if config.core.delete_service_files_when_finished {
        let _ = tokio::fs::remove_dir_all(&work_dir).await;
    }
    if config.core.set_mark_of_the_web {
        let out_c = output_path.clone();
        let url_c = record.url.clone();
        let _ = tokio::task::spawn_blocking(move || set_mark_of_the_web(&out_c, &url_c, "Ferrite"))
            .await;
    }
    record.downloaded_bytes = tokio::fs::metadata(&output_path)
        .await
        .map(|m| m.len())
        .unwrap_or(total_done);
    record.total_bytes = Some(record.downloaded_bytes);
    record.download_speed_bps = 0;
    record.connections_active = 0;
    record.state = DownloadState::Completed;
    record.completed_at = Some(chrono::Utc::now());
    on_progress(&record);
    record
}

#[allow(clippy::too_many_arguments)]
async fn fetch_dash_rep<F>(
    client: &Client,
    record: &mut DownloadRecord,
    config: &FerriteConfig,
    base_headers: &HeaderMap,
    rep: &DashRepresentationInfo,
    out_path: &Path,
    limiter: &BandwidthLimiter,
    control_rx: &mut watch::Receiver<ControlCommand>,
    total_done: &mut u64,
    on_progress: &mut F,
) -> Result<OutcomeHls>
where
    F: FnMut(&DownloadRecord) + Send,
{
    type DashFetchItem<'a> = (&'a str, Option<(u64, Option<u64>)>);
    let mut items: Vec<DashFetchItem<'_>> = Vec::new();
    if let Some(ref init_u) = rep.init_url {
        items.push((init_u.as_str(), rep.init_range));
    }
    for (idx, u) in rep.segment_urls.iter().enumerate() {
        let r = rep.segment_ranges.get(idx).copied().flatten();
        items.push((u.as_str(), r));
    }

    record.segments = items
        .iter()
        .enumerate()
        .map(|(i, (u, _))| SegmentProgress {
            id: i as u32,
            start: i as u64,
            end: i as u64,
            downloaded: 0,
            state: SegmentState::Pending,
            speed_bps: 0,
            mirror_url: Some((*u).to_string()),
        })
        .collect();
    record.connections_active = 1;
    on_progress(record);

    let tmp_path = out_path.with_extension("tmp");
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp_path)
        .await?;

    let mut last_tick = tokio::time::Instant::now();
    let mut bytes_since_tick = 0u64;
    let mut slow_accum_secs = 0.0f64;

    for (idx, (u, range_opt)) in items.into_iter().enumerate() {
        let cmd = control_rx.borrow().clone();
        match cmd {
            ControlCommand::Pause => {
                let _ = f.flush().await;
                drop(f);
                let _ = tokio::fs::remove_file(&tmp_path).await;
                return Ok(OutcomeHls::Paused);
            }
            ControlCommand::Cancel => {
                drop(f);
                let _ = tokio::fs::remove_file(&tmp_path).await;
                return Ok(OutcomeHls::Cancelled);
            }
            ControlCommand::Run => {}
        }

        if let Some(seg) = record.segments.get_mut(idx) {
            seg.state = SegmentState::Active;
        }
        on_progress(record);

        let mut req = client
            .get(u)
            .headers(headers_for_url(&record.url, u, base_headers));
        if let Some((start, end_opt)) = range_opt {
            let hdr_val = match end_opt {
                Some(end) => format!("bytes={start}-{end}"),
                None => format!("bytes={start}-"),
            };
            req = req.header(RANGE, hdr_val);
        }

        let resp = tokio::select! {
            res = req.send() => res?.error_for_status()?,
            changed = control_rx.changed() => {
                if changed.is_ok() {
                    let cmd = control_rx.borrow().clone();
                    match cmd {
                        ControlCommand::Pause => {
                            let _ = f.flush().await;
                            drop(f);
                            let _ = tokio::fs::remove_file(&tmp_path).await;
                            return Ok(OutcomeHls::Paused);
                        }
                        ControlCommand::Cancel => {
                            drop(f);
                            let _ = tokio::fs::remove_file(&tmp_path).await;
                            return Ok(OutcomeHls::Cancelled);
                        }
                        ControlCommand::Run => {}
                    }
                }
                bail!("DASH control channel interrupted");
            }
        };

        let mut resp = resp;
        let mut seg_bytes = 0u64;
        loop {
            tokio::select! {
                changed = control_rx.changed() => {
                    if changed.is_ok() {
                        let cmd = control_rx.borrow().clone();
                        match cmd {
                            ControlCommand::Pause => {
                                let _ = f.flush().await;
                                drop(f);
                                let _ = tokio::fs::remove_file(&tmp_path).await;
                                return Ok(OutcomeHls::Paused);
                            }
                            ControlCommand::Cancel => {
                                drop(f);
                                let _ = tokio::fs::remove_file(&tmp_path).await;
                                return Ok(OutcomeHls::Cancelled);
                            }
                            ControlCommand::Run => {}
                        }
                    }
                }
                chunk_res = resp.chunk() => {
                    match chunk_res? {
                        Some(chunk) => {
                            limiter.acquire(chunk.len()).await;
                            f.write_all(&chunk).await?;
                            let n = chunk.len() as u64;
                            *total_done += n;
                            seg_bytes += n;
                            bytes_since_tick += n;
                            record.downloaded_bytes = *total_done;
                            let now = tokio::time::Instant::now();
                            let dt = now.duration_since(last_tick).as_secs_f64();
                            if dt >= 0.25 {
                                let speed = (bytes_since_tick as f64 / dt) as u64;
                                bytes_since_tick = 0;
                                last_tick = now;
                                record.download_speed_bps = speed;
                                if let Some(seg) = record.segments.get_mut(idx) {
                                    seg.downloaded = seg_bytes;
                                    seg.speed_bps = speed;
                                }
                                record.updated_at = chrono::Utc::now();
                                on_progress(record);
                                if config.core.detect_unacceptably_slow {
                                    if speed < config.core.unacceptably_slow_speed_bps {
                                        slow_accum_secs += dt;
                                        if slow_accum_secs >= config.core.unacceptably_slow_interval_secs as f64 {
                                            record.abort_reason = DownloadAbortReason::SlowDownload;
                                            bail!(
                                                "DASH speed dropped below {} B/s for {}s",
                                                config.core.unacceptably_slow_speed_bps,
                                                config.core.unacceptably_slow_interval_secs
                                            );
                                        }
                                    } else {
                                        slow_accum_secs = 0.0;
                                    }
                                }
                            } else if let Some(seg) = record.segments.get_mut(idx) {
                                seg.downloaded = seg_bytes;
                            }
                        }
                        None => break,
                    }
                }
            }
        }

        if let Some(seg) = record.segments.get_mut(idx) {
            seg.downloaded = seg_bytes;
            seg.state = SegmentState::Completed;
            seg.speed_bps = record.download_speed_bps;
        }
        record.downloaded_bytes = *total_done;
        record.updated_at = chrono::Utc::now();
        on_progress(record);
    }

    f.flush().await?;
    f.sync_all().await?;
    drop(f);
    if tokio::fs::try_exists(out_path).await.unwrap_or(false) {
        let _ = tokio::fs::remove_file(out_path).await;
    }
    tokio::fs::rename(&tmp_path, out_path).await?;
    Ok(OutcomeHls::Completed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncryptMut;
    use ferrite_core::{DownloadFlags, DownloadType, HttpRequestOptions, ResumeSupport};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;

    #[tokio::test]
    async fn downloads_and_decrypts_aes128_hls_stream() {
        let key = [0x42u8; 16];
        let seg1_plain = b"HELLO_SEGMENT_ONE_PAYLOAD_123456".to_vec();
        let seg2_plain = b"WORLD_SEGMENT_TWO_PAYLOAD_7890!".to_vec();

        let encrypt = |pt: &[u8], seq: u64| -> Vec<u8> {
            let iv = parse_iv(&None, seq);
            let mut buf = pt.to_vec();
            let pos = buf.len();
            buf.resize(pos + 16, 0);
            Aes128CbcEnc::new(&key.into(), &iv.into())
                .encrypt_padded_mut::<aes::cipher::block_padding::Pkcs7>(&mut buf, pos)
                .unwrap()
                .to_vec()
        };
        let seg1_enc = Arc::new(encrypt(&seg1_plain, 0));
        let seg2_enc = Arc::new(encrypt(&seg2_plain, 1));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let s1 = Arc::clone(&seg1_enc);
        let s2 = Arc::clone(&seg2_enc);

        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let s1 = Arc::clone(&s1);
                let s2 = Arc::clone(&s2);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");
                    let (ctype, body): (&str, Vec<u8>) = match path {
                        "/master.m3u8" => (
                            "application/vnd.apple.mpegurl",
                            b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=1280x720\n/media.m3u8\n"
                                .to_vec(),
                        ),
                        "/media.m3u8" => (
                            "application/vnd.apple.mpegurl",
                            b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-KEY:METHOD=AES-128,URI=\"/key.bin\"\n#EXTINF:4.0,\n/seg0.ts\n#EXTINF:4.0,\n/seg1.ts\n#EXT-X-ENDLIST\n".to_vec(),
                        ),
                        "/key.bin" => ("application/octet-stream", key.to_vec()),
                        "/seg0.ts" => ("video/mp2t", s1.to_vec()),
                        "/seg1.ts" => ("video/mp2t", s2.to_vec()),
                        _ => ("text/plain", b"not found".to_vec()),
                    };
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = sock.write_all(hdr.as_bytes()).await;
                    let _ = sock.write_all(&body).await;
                });
            }
        });

        let dir = std::env::temp_dir().join(format!("ferrite_hls_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut cfg = FerriteConfig::default();
        cfg.core.set_mark_of_the_web = false;
        cfg.core.delete_service_files_when_finished = true;

        let url = format!("http://{addr}/master.m3u8");
        let rec = DownloadRecord {
            id: 2,
            uuid: uuid::Uuid::new_v4().to_string(),
            parent_id: None,
            children_ids: vec![],
            order_index: 2,
            download_type: DownloadType::HlsM3u8,
            state: DownloadState::Queued,
            priority: DownloadPriority::Normal,
            resume_support: ResumeSupport::Present,
            flags: DownloadFlags::default(),
            title: "stream.ts".into(),
            url: url.clone(),
            target_dir: dir.clone(),
            file_name: "stream.ts".into(),
            partial_suffix: ".part".into(),
            total_bytes: None,
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            download_speed_bps: 0,
            upload_speed_bps: 0,
            eta_secs: None,
            connections_active: 0,
            etag: None,
            last_modified: None,
            file_type: ferrite_core::FileType::Video,
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
        let done = download_hls_stream(rec, &cfg, &[], BandwidthLimiter::new(0), rx, |_| {}).await;
        assert_eq!(
            done.state,
            DownloadState::Completed,
            "err={:?}",
            done.last_error
        );

        let final_bytes = std::fs::read(dir.join("stream.ts")).unwrap();
        let mut expected = seg1_plain;
        expected.extend_from_slice(&seg2_plain);
        assert_eq!(final_bytes, expected);

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_dash_segment_timeline() {
        let xml = r#"<?xml version="1.0"?>
        <MPD>
          <Period>
            <AdaptationSet mimeType="video/mp4">
              <Representation id="v720" bandwidth="1500000" width="1280" height="720">
                <SegmentTemplate initialization="init-$RepresentationID$.mp4" media="chunk-$RepresentationID$-$Number$.m4s" startNumber="1">
                  <SegmentTimeline>
                    <S t="0" d="1000" r="1"/>
                  </SegmentTimeline>
                </SegmentTemplate>
              </Representation>
            </AdaptationSet>
          </Period>
        </MPD>"#;
        let reps = parse_dash_mpd("https://cdn.example.com/live/manifest.mpd", xml).unwrap();
        assert_eq!(reps.len(), 1);
        assert_eq!(
            reps[0].init_url.as_deref(),
            Some("https://cdn.example.com/live/init-v720.mp4")
        );
        assert_eq!(
            reps[0].segment_urls,
            vec![
                "https://cdn.example.com/live/chunk-v720-1.m4s",
                "https://cdn.example.com/live/chunk-v720-2.m4s",
            ]
        );
    }

    #[tokio::test]
    async fn hls_propagates_referer_and_cookies_avoiding_403() {
        let seg_payload = b"HLS-PROTECTED-SEGMENT-BYTES".to_vec();
        let seg_srv = Arc::new(seg_payload.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let seg_data = Arc::clone(&seg_srv);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let has_ref = req.lines().any(|l| {
                        l.to_ascii_lowercase().starts_with("referer:")
                            && l.contains("https://player.example.com/")
                    });
                    let has_cookie = req.lines().any(|l| {
                        l.to_ascii_lowercase().starts_with("cookie:")
                            && l.contains("hls_auth=secret")
                    });
                    if !has_ref || !has_cookie {
                        let resp = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        let _ = sock.write_all(resp.as_bytes()).await;
                        return;
                    }
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");
                    let (ctype, body): (&str, Vec<u8>) = match path {
                        "/media.m3u8" => (
                            "application/vnd.apple.mpegurl",
                            b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:4.0,\n/seg0.ts\n#EXT-X-ENDLIST\n".to_vec(),
                        ),
                        "/seg0.ts" => ("video/mp2t", (*seg_data).clone()),
                        _ => ("text/plain", b"missing".to_vec()),
                    };
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = sock.write_all(hdr.as_bytes()).await;
                    let _ = sock.write_all(&body).await;
                });
            }
        });

        let dir = std::env::temp_dir().join(format!("ferrite_hls_403_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = FerriteConfig::default();
        cfg.core.set_mark_of_the_web = false;
        cfg.core.delete_service_files_when_finished = true;

        let url = format!("http://{addr}/media.m3u8");
        let rec = DownloadRecord {
            id: 3,
            uuid: uuid::Uuid::new_v4().to_string(),
            parent_id: None,
            children_ids: vec![],
            order_index: 3,
            download_type: DownloadType::HlsM3u8,
            state: DownloadState::Queued,
            priority: DownloadPriority::Normal,
            resume_support: ResumeSupport::Present,
            flags: DownloadFlags::default(),
            title: "protected.ts".into(),
            url: url.clone(),
            target_dir: dir.clone(),
            file_name: "protected.ts".into(),
            partial_suffix: ".part".into(),
            total_bytes: None,
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            download_speed_bps: 0,
            upload_speed_bps: 0,
            eta_secs: None,
            connections_active: 0,
            etag: None,
            last_modified: None,
            file_type: ferrite_core::FileType::Video,
            http: HttpRequestOptions {
                url,
                referrer: Some("https://player.example.com/".into()),
                cookies: Some("hls_auth=secret".into()),
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
        let done = download_hls_stream(rec, &cfg, &[], BandwidthLimiter::new(0), rx, |_| {}).await;
        assert_eq!(
            done.state,
            DownloadState::Completed,
            "err={:?}",
            done.last_error
        );
        assert_eq!(
            std::fs::read(dir.join("protected.ts")).unwrap(),
            seg_payload
        );

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn make_hls_or_dash_record(
        url: String,
        dir: &Path,
        file_name: &str,
        dtype: DownloadType,
    ) -> DownloadRecord {
        DownloadRecord {
            id: 10,
            uuid: uuid::Uuid::new_v4().to_string(),
            parent_id: None,
            children_ids: vec![],
            order_index: 10,
            download_type: dtype,
            state: DownloadState::Queued,
            priority: DownloadPriority::Normal,
            resume_support: ResumeSupport::Present,
            flags: DownloadFlags::default(),
            title: file_name.into(),
            url: url.clone(),
            target_dir: dir.to_path_buf(),
            file_name: file_name.into(),
            partial_suffix: ".part".into(),
            total_bytes: None,
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            download_speed_bps: 0,
            upload_speed_bps: 0,
            eta_secs: None,
            connections_active: 0,
            etag: None,
            last_modified: None,
            file_type: ferrite_core::FileType::Video,
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
    async fn hls_byterange_tracks_cumulative_offset_across_segments() {
        let full_blob = Arc::new(b"AAAAAA_BBBB_CCCCC".to_vec());
        let blob_srv = Arc::clone(&full_blob);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let blob = Arc::clone(&blob_srv);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");
                    if path == "/range.m3u8" {
                        let m3u8 = b"#EXTM3U\n#EXT-X-VERSION:4\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:4.0,\n#EXT-X-BYTERANGE:6@0\n/stream.mp4\n#EXTINF:4.0,\n#EXT-X-BYTERANGE:5\n/stream.mp4\n#EXTINF:4.0,\n#EXT-X-BYTERANGE:6\n/stream.mp4\n#EXT-X-ENDLIST\n";
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.apple.mpegurl\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            m3u8.len()
                        );
                        let _ = sock.write_all(hdr.as_bytes()).await;
                        let _ = sock.write_all(m3u8).await;
                        return;
                    }
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
                            .unwrap_or(blob.len() - 1)
                            .min(blob.len() - 1);
                        let slice = &blob[start..=end];
                        let hdr = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nConnection: close\r\n\r\n",
                            slice.len(),
                            blob.len()
                        );
                        let _ = sock.write_all(hdr.as_bytes()).await;
                        let _ = sock.write_all(slice).await;
                    }
                });
            }
        });

        let dir = std::env::temp_dir().join(format!("ferrite_hls_br_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = FerriteConfig::default();
        cfg.core.set_mark_of_the_web = false;
        cfg.core.auto_retry_failed = false;

        let rec = make_hls_or_dash_record(
            format!("http://{addr}/range.m3u8"),
            &dir,
            "range.mp4",
            DownloadType::HlsM3u8,
        );
        let (_tx, rx) = watch::channel(ControlCommand::Run);
        let done = download_hls_stream(rec, &cfg, &[], BandwidthLimiter::new(0), rx, |_| {}).await;
        assert_eq!(
            done.state,
            DownloadState::Completed,
            "err={:?}",
            done.last_error
        );
        assert_eq!(std::fs::read(dir.join("range.mp4")).unwrap(), *full_blob);

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn hls_rejects_unsupported_encryption_method() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 2048];
                    let _ = sock.read(&mut buf).await;
                    let body = b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"/key.bin\"\n#EXTINF:4.0,\n/seg0.ts\n#EXT-X-ENDLIST\n";
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.apple.mpegurl\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = sock.write_all(hdr.as_bytes()).await;
                    let _ = sock.write_all(body).await;
                });
            }
        });

        let dir = std::env::temp_dir().join(format!("ferrite_hls_enc_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = FerriteConfig::default();
        cfg.core.auto_retry_failed = false;

        let rec = make_hls_or_dash_record(
            format!("http://{addr}/sample.m3u8"),
            &dir,
            "sample.ts",
            DownloadType::HlsM3u8,
        );
        let (_tx, rx) = watch::channel(ControlCommand::Run);
        let done = download_hls_stream(rec, &cfg, &[], BandwidthLimiter::new(0), rx, |_| {}).await;
        assert_eq!(done.state, DownloadState::Error);
        assert!(
            done.last_error
                .as_deref()
                .is_some_and(|m| m.contains("Unsupported HLS encryption method")),
            "unexpected error: {:?}",
            done.last_error
        );
        assert!(!dir.join("sample.ts").exists());

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn hls_fails_when_aes_key_endpoint_returns_http_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");
                    match path {
                        "/media.m3u8" => {
                            let body = b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-KEY:METHOD=AES-128,URI=\"/key.bin\"\n#EXTINF:4.0,\n/seg0.ts\n#EXT-X-ENDLIST\n";
                            let hdr = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = sock.write_all(hdr.as_bytes()).await;
                            let _ = sock.write_all(body).await;
                        }
                        "/key.bin" => {
                            let err_body = b"0123456789abcdef"; // 16-byte 403 body!
                            let hdr = format!(
                                "HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                err_body.len()
                            );
                            let _ = sock.write_all(hdr.as_bytes()).await;
                            let _ = sock.write_all(err_body).await;
                        }
                        _ => {
                            let body = [0xAAu8; 16];
                            let hdr = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = sock.write_all(hdr.as_bytes()).await;
                            let _ = sock.write_all(&body).await;
                        }
                    }
                });
            }
        });

        let dir = std::env::temp_dir().join(format!("ferrite_hls_key403_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = FerriteConfig::default();
        cfg.core.auto_retry_failed = false;

        let rec = make_hls_or_dash_record(
            format!("http://{addr}/media.m3u8"),
            &dir,
            "k403.ts",
            DownloadType::HlsM3u8,
        );
        let (_tx, rx) = watch::channel(ControlCommand::Run);
        let done = download_hls_stream(rec, &cfg, &[], BandwidthLimiter::new(0), rx, |_| {}).await;
        assert_eq!(done.state, DownloadState::Error);
        assert!(
            done.last_error
                .as_deref()
                .is_some_and(|m| m.contains("Key fetch HTTP error")),
            "expected Key fetch HTTP error, got: {:?}",
            done.last_error
        );

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_dash_duration_template_formatting_and_baseurl_inheritance() {
        let xml = r#"<?xml version="1.0"?>
        <MPD type="static" mediaPresentationDuration="PT10S">
          <BaseURL>https://cdn.example.com/root/</BaseURL>
          <Period>
            <BaseURL>v1/</BaseURL>
            <AdaptationSet mimeType="video/mp4">
              <BaseURL>media/</BaseURL>
              <Representation id="hd" bandwidth="2500000" width="1280" height="720">
                <SegmentTemplate
                  initialization="init-$RepresentationID$-$Bandwidth$.mp4"
                  media="chunk-$RepresentationID$-$Number%04d$-$Bandwidth$.m4s"
                  startNumber="1"
                  duration="5000"
                  timescale="1000"/>
              </Representation>
            </AdaptationSet>
          </Period>
        </MPD>"#;
        let reps = parse_dash_mpd("https://origin.example.com/manifest.mpd", xml).unwrap();
        assert_eq!(reps.len(), 1);
        assert_eq!(
            reps[0].init_url.as_deref(),
            Some("https://cdn.example.com/root/v1/media/init-hd-2500000.mp4")
        );
        assert_eq!(
            reps[0].segment_urls,
            vec![
                "https://cdn.example.com/root/v1/media/chunk-hd-0001-2500000.m4s",
                "https://cdn.example.com/root/v1/media/chunk-hd-0002-2500000.m4s",
            ]
        );
    }

    #[test]
    fn parses_dash_segment_base_byte_ranges() {
        let xml = r#"<?xml version="1.0"?>
        <MPD type="static" mediaPresentationDuration="PT30S">
          <Period>
            <AdaptationSet mimeType="video/mp4">
              <Representation id="v1" bandwidth="1000000" width="640" height="360">
                <BaseURL>single.mp4</BaseURL>
                <SegmentBase indexRange="500-699">
                  <Initialization range="0-499"/>
                </SegmentBase>
              </Representation>
            </AdaptationSet>
          </Period>
        </MPD>"#;
        let reps = parse_dash_mpd("https://cdn.example.com/dash/manifest.mpd", xml).unwrap();
        assert_eq!(reps.len(), 1);
        assert_eq!(
            reps[0].init_url.as_deref(),
            Some("https://cdn.example.com/dash/single.mp4")
        );
        assert_eq!(reps[0].init_range, Some((0, Some(499))));
        assert_eq!(
            reps[0].segment_urls,
            vec!["https://cdn.example.com/dash/single.mp4"]
        );
        assert_eq!(reps[0].segment_ranges, vec![Some((500, None))]);
    }

    #[tokio::test]
    async fn dash_pause_returns_paused_and_emits_segment_progress() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");
                    if path == "/manifest.mpd" {
                        let xml = br#"<?xml version="1.0"?>
                        <MPD type="static" mediaPresentationDuration="PT6S">
                          <Period>
                            <AdaptationSet mimeType="video/mp4">
                              <Representation id="v1" bandwidth="500000" width="640" height="360">
                                <SegmentTemplate initialization="init.mp4" media="seg-$Number%02d$.m4s" startNumber="1" duration="2000" timescale="1000"/>
                              </Representation>
                            </AdaptationSet>
                          </Period>
                        </MPD>"#;
                        let hdr = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/dash+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            xml.len()
                        );
                        let _ = sock.write_all(hdr.as_bytes()).await;
                        let _ = sock.write_all(xml).await;
                        return;
                    }
                    let chunk = vec![0x5Au8; 16 * 1024];
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        chunk.len()
                    );
                    let _ = sock.write_all(hdr.as_bytes()).await;
                    let _ = sock.write_all(&chunk).await;
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                });
            }
        });

        let dir = std::env::temp_dir().join(format!("ferrite_dash_pause_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = FerriteConfig::default();
        cfg.core.set_mark_of_the_web = false;
        cfg.core.delete_service_files_when_finished = true;

        // Part 1: Verify full completion emits SegmentProgress entries.
        let rec_ok = make_hls_or_dash_record(
            format!("http://{addr}/manifest.mpd"),
            &dir,
            "dash_full.mp4",
            DownloadType::Dash,
        );
        let (_tx1, rx1) = watch::channel(ControlCommand::Run);
        let mut saw_segments = false;
        let done = download_dash_stream(rec_ok, &cfg, &[], BandwidthLimiter::new(0), rx1, |snap| {
            if !snap.segments.is_empty() {
                saw_segments = true;
            }
        })
        .await;
        assert_eq!(
            done.state,
            DownloadState::Completed,
            "err={:?}",
            done.last_error
        );
        assert!(saw_segments, "expected DASH progress updates with segments");
        assert_eq!(done.downloaded_bytes, 4 * 16 * 1024);

        // Part 2: Verify ControlCommand::Pause results in DownloadState::Paused (not Error).
        let rec_pause = make_hls_or_dash_record(
            format!("http://{addr}/manifest.mpd"),
            &dir,
            "dash_paused.mp4",
            DownloadType::Dash,
        );
        let (tx2, rx2) = watch::channel(ControlCommand::Run);
        let sent_pause = Arc::new(AtomicBool::new(false));
        let paused = download_dash_stream(
            rec_pause,
            &cfg,
            &[],
            BandwidthLimiter::new(0),
            rx2,
            |snap| {
                if snap.downloaded_bytes >= 16 * 1024 && !sent_pause.swap(true, Ordering::SeqCst) {
                    let _ = tx2.send(ControlCommand::Pause);
                }
            },
        )
        .await;
        assert_eq!(
            paused.state,
            DownloadState::Paused,
            "expected Paused state, got {:?} (err={:?})",
            paused.state,
            paused.last_error
        );

        srv.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
