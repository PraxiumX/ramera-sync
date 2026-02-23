use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::SystemTime;

use chrono::Utc;
use tokio::process::Command;
use tokio::time::{Duration, Instant};
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::NvrConfig;
use crate::error::{AppError, Result};
use crate::http_auth::{get_with_auth, post_xml_with_auth};
use crate::providers::{build_base_urls, merge_paths, preview, ProviderFingerprint};
use crate::types::{DeviceRecord, NvrDevice};

const HIKVISION_FINGERPRINT_PATH: &str = "/ISAPI/System/deviceInfo";
const HIKVISION_RECORD_PATHS: [&str; 2] = [
    "/ISAPI/System/deviceInfo",
    "/ISAPI/ContentMgmt/record/tracks",
];

pub async fn fingerprint(
    client: &reqwest::Client,
    ip: std::net::IpAddr,
    open_ports: &[u16],
    cfg: &NvrConfig,
) -> Option<ProviderFingerprint> {
    let base_urls = build_base_urls(ip, open_ports, cfg.include_https);

    for base in base_urls {
        let url = format!("{base}{HIKVISION_FINGERPRINT_PATH}");
        let response = match get_with_auth(
            client,
            &url,
            cfg.username.as_deref(),
            cfg.password.as_deref(),
            cfg.request_timeout_ms,
        )
        .await
        {
            Ok(v) => v,
            Err(_) => continue,
        };

        if !response.status().is_success() {
            continue;
        }

        let body = match response.text().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        if !looks_like_hikvision(&body) {
            continue;
        }

        return Some(ProviderFingerprint {
            provider: "hikvision",
            vendor: detect_vendor(&body),
            model: extract_xml_tag(&body, "model"),
            serial: extract_xml_tag(&body, "serialNumber"),
            source_url: Some(url),
            preview: Some(preview(&body, 240)),
            is_nvr: true,
        });
    }
    None
}

pub async fn collect_records(
    client: &reqwest::Client,
    device: &NvrDevice,
    cfg: &NvrConfig,
) -> Vec<DeviceRecord> {
    let mut out = Vec::new();
    let base_urls = build_base_urls(device.ip, &device.open_ports, cfg.include_https);
    let paths = merge_paths(&HIKVISION_RECORD_PATHS, &cfg.record_paths);

    for base in base_urls {
        for path in &paths {
            let url = format!("{base}{path}");
            let response = match get_with_auth(
                client,
                &url,
                cfg.username.as_deref(),
                cfg.password.as_deref(),
                cfg.request_timeout_ms,
            )
            .await
            {
                Ok(v) => v,
                Err(_) => continue,
            };

            let status = response.status().as_u16();
            if status >= 500 {
                continue;
            }

            let body = response.text().await.unwrap_or_default();
            out.push(DeviceRecord {
                ip: device.ip.to_string(),
                provider: "hikvision".to_string(),
                path: url,
                status,
                fetched_at: Utc::now(),
                body_preview: preview(&body, 500),
                body,
            });
        }
    }

    out
}

fn looks_like_hikvision(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("hikvision")
        || lower.contains("www.hikvision.com")
        || lower.contains("<deviceinfo")
            && lower.contains("<model>")
            && lower.contains("<serialnumber>")
}

fn detect_vendor(body: &str) -> Option<String> {
    let manufacturer = extract_xml_tag(body, "manufacturer")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if manufacturer.contains("hikvision") || body.to_ascii_lowercase().contains("hikvision") {
        Some("Hikvision".to_string())
    } else {
        None
    }
}

fn extract_xml_tag(body: &str, key: &str) -> Option<String> {
    let open = format!("<{key}>");
    let close = format!("</{key}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    let value = body[start..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

pub async fn download_video_clips(
    client: &reqwest::Client,
    device: &NvrDevice,
    cfg: &NvrConfig,
    days: u32,
    max_clips: usize,
    clip_seconds: u32,
    output_dir: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    info!(
        "Hikvision {}: starting download (days={}, max_clips={}, clip_seconds={})",
        device.ip,
        days,
        if max_clips == 0 {
            "unlimited".to_string()
        } else {
            max_clips.to_string()
        },
        if clip_seconds == 0 {
            "full".to_string()
        } else {
            clip_seconds.to_string()
        }
    );
    let base_urls = build_base_urls(device.ip, &device.open_ports, cfg.include_https);
    let username = cfg.username.as_deref().ok_or_else(|| {
        AppError::Command("nvr.username is required for Hikvision clip download".to_string())
    })?;
    let password = cfg.password.as_deref().ok_or_else(|| {
        AppError::Command("nvr.password is required for Hikvision clip download".to_string())
    })?;

    let tracks = find_tracks(client, &base_urls, cfg).await?;
    if tracks.is_empty() {
        info!("Hikvision {}: no tracks found", device.ip);
        return Ok(Vec::new());
    }
    info!(
        "Hikvision {}: discovered {} track(s): {:?}",
        device.ip,
        tracks.len(),
        tracks
    );

    let day_start = chrono::Utc::now() - chrono::Duration::days(i64::from(days.max(1) - 1));
    let search_start = day_start
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap_or_else(|| chrono::Utc::now().naive_utc());
    // Some NVRs report clip timestamps ahead of host UTC; add a forward buffer.
    let search_end = (chrono::Utc::now() + chrono::Duration::hours(24)).naive_utc();

    let clips_root = output_dir.map(Path::to_path_buf).unwrap_or_else(|| {
        PathBuf::from("records")
            .join("clips")
            .join(chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string())
    });
    std::fs::create_dir_all(&clips_root)?;

    let mut saved = Vec::new();
    for track_id in tracks {
        if max_clips > 0 && saved.len() >= max_clips {
            break;
        }

        info!("Hikvision {}: searching track {}", device.ip, track_id);
        let entries =
            search_track_entries(client, &base_urls, cfg, track_id, search_start, search_end)
                .await?;
        info!(
            "Hikvision {}: track {} returned {} record item(s)",
            device.ip,
            track_id,
            entries.len()
        );
        for entry in entries {
            if max_clips > 0 && saved.len() >= max_clips {
                break;
            }

            let Some(rtsp_uri) = inject_rtsp_auth(&entry.playback_uri, username, password) else {
                continue;
            };
            let out = clips_root.join(format!(
                "{}_track{}_{}_{}.mkv",
                sanitize_filename(&device.ip.to_string()),
                track_id,
                sanitize_filename(&entry.start_time),
                sanitize_filename(&entry.end_time)
            ));
            info!(
                "Hikvision {}: downloading track {} start={} end={} -> {}",
                device.ip,
                track_id,
                entry.start_time,
                entry.end_time,
                out.display()
            );

            if run_ffmpeg_clip(&rtsp_uri, &out, clip_seconds).await? {
                info!("Hikvision {}: saved {}", device.ip, out.display());
                saved.push(out);
            } else {
                warn!(
                    "Hikvision {}: ffmpeg did not produce output for track {} start={}",
                    device.ip, track_id, entry.start_time
                );
            }
        }
    }

    info!(
        "Hikvision {}: download finished with {} file(s)",
        device.ip,
        saved.len()
    );
    Ok(saved)
}

#[derive(Debug, Clone)]
struct PlaybackEntry {
    start_time: String,
    end_time: String,
    playback_uri: String,
}

async fn find_tracks(
    client: &reqwest::Client,
    base_urls: &[String],
    cfg: &NvrConfig,
) -> Result<Vec<u32>> {
    for base in base_urls {
        let url = format!("{base}/ISAPI/ContentMgmt/record/tracks");
        let response = match get_with_auth(
            client,
            &url,
            cfg.username.as_deref(),
            cfg.password.as_deref(),
            cfg.request_timeout_ms,
        )
        .await
        {
            Ok(v) => v,
            Err(_) => continue,
        };

        if !response.status().is_success() {
            continue;
        }
        let body = response.text().await.unwrap_or_default();
        let ids = extract_track_ids(&body);
        if !ids.is_empty() {
            return Ok(ids);
        }
    }
    Ok(Vec::new())
}

async fn search_track_entries(
    client: &reqwest::Client,
    base_urls: &[String],
    cfg: &NvrConfig,
    track_id: u32,
    start: chrono::NaiveDateTime,
    end: chrono::NaiveDateTime,
) -> Result<Vec<PlaybackEntry>> {
    let start = start.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let end = end.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let search_id = format!("{{{}}}", Uuid::new_v4());

    let xml = format!(
        "<?xml version='1.0' encoding='utf-8'?>\
<CMSearchDescription><searchID>{search_id}</searchID>\
<trackList><trackID>{track_id}</trackID></trackList>\
<timeSpanList><timeSpan><startTime>{start}</startTime><endTime>{end}</endTime></timeSpan></timeSpanList>\
<maxResults>100</maxResults><searchResultPostion>0</searchResultPostion>\
<metadataList><metadataDescriptor>//recordType.meta.std-cgi.com</metadataDescriptor></metadataList>\
</CMSearchDescription>"
    );

    for base in base_urls {
        let url = format!("{base}/ISAPI/ContentMgmt/search");
        let response = match post_xml_with_auth(
            client,
            &url,
            &xml,
            cfg.username.as_deref(),
            cfg.password.as_deref(),
            cfg.request_timeout_ms,
        )
        .await
        {
            Ok(v) => v,
            Err(_) => continue,
        };
        if !response.status().is_success() {
            continue;
        }
        let body = response.text().await.unwrap_or_default();
        return Ok(extract_playback_entries(&body));
    }

    Ok(Vec::new())
}

async fn run_ffmpeg_clip(rtsp_uri: &str, output_path: &PathBuf, clip_seconds: u32) -> Result<bool> {
    recover_pending_resume_segment(output_path);
    let complete_marker = clip_complete_marker_path(output_path);

    let full_length = clip_seconds == 0;
    let existing_bytes = std::fs::metadata(output_path).map(|m| m.len()).unwrap_or(0);
    let existing_secs = if existing_bytes > 0 {
        probe_duration_seconds(output_path).unwrap_or(0.0)
    } else {
        0.0
    };

    if !full_length && existing_secs >= (f64::from(clip_seconds) - 0.5).max(0.0) {
        write_clip_complete_marker(output_path, existing_bytes)?;
        eprintln!(
            "[progress] {}: already complete {}, skipping",
            output_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("clip"),
            format_size(existing_bytes)
        );
        return Ok(existing_bytes > 0);
    }

    let mut resume_from_secs = 0.0;
    let mut base_bytes = 0_u64;
    let mut segment_path = output_path.clone();
    let mut needs_merge = false;
    if existing_secs >= 1.0 && existing_bytes > 0 {
        resume_from_secs = existing_secs;
        base_bytes = existing_bytes;
        segment_path = hidden_sidecar_path(output_path, "resume.part");
        let _ = std::fs::remove_file(&segment_path);
        needs_merge = true;
        eprintln!(
            "[progress] {}: resuming from {:.1}s ({} already saved)",
            output_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("clip"),
            resume_from_secs,
            format_size(base_bytes)
        );
    }

    let mut cmd = Command::new(resolve_ffmpeg_bin());
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-rtsp_transport")
        .arg("tcp");

    if resume_from_secs > 0.0 {
        cmd.arg("-ss").arg(format!("{resume_from_secs:.3}"));
    }

    cmd.arg("-i")
        .arg(rtsp_uri)
        .arg("-map")
        .arg("0:v:0")
        .arg("-an");

    if !full_length {
        let remaining = (f64::from(clip_seconds) - resume_from_secs).max(0.0);
        if remaining <= 0.25 {
            write_clip_complete_marker(output_path, existing_bytes)?;
            eprintln!(
                "[progress] {}: already complete {}, skipping",
                output_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("clip"),
                format_size(existing_bytes)
            );
            return Ok(existing_bytes > 0);
        }
        cmd.arg("-t").arg(format!("{remaining:.3}"));
    }

    cmd.arg("-c:v")
        .arg("copy")
        .arg("-f")
        .arg("matroska")
        .arg(&segment_path);

    let _ = std::fs::remove_file(&complete_marker);

    let timeout_secs = if full_length {
        3 * 60 * 60
    } else {
        u64::from(clip_seconds + 25)
    };
    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::Command(format!("ffmpeg failed to start: {e}")))?;
    let mut ticker = tokio::time::interval(Duration::from_secs(2));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let started_at = Instant::now();
    let mut last_reported_bytes: Option<u64> = None;

    let status = loop {
        tokio::select! {
            status = child.wait() => {
                break status.map_err(|e| AppError::Command(format!("ffmpeg wait failed: {e}")))?;
            }
            _ = ticker.tick() => {
                let elapsed = started_at.elapsed();
                if elapsed >= Duration::from_secs(timeout_secs) {
                    let _ = child.kill().await;
                    return Err(AppError::Command("ffmpeg timed out while downloading clip".to_string()));
                }

                let segment_bytes = std::fs::metadata(&segment_path).map(|m| m.len()).unwrap_or(0);
                let downloaded_bytes = base_bytes.saturating_add(segment_bytes);
                if last_reported_bytes == Some(downloaded_bytes) {
                    continue;
                }
                last_reported_bytes = Some(downloaded_bytes);
                let target_bytes = estimate_target_bytes(downloaded_bytes, elapsed, clip_seconds);
                print_clip_progress(output_path, downloaded_bytes, target_bytes, elapsed);
            }
        }
    };

    if !status.success() {
        let _ = std::fs::remove_file(&complete_marker);
        return Ok(false);
    }

    if needs_merge {
        merge_segments(output_path, &segment_path)?;
    }

    let size = std::fs::metadata(output_path).map(|m| m.len()).unwrap_or(0);
    let elapsed = started_at.elapsed();
    if full_length {
        if last_reported_bytes != Some(size) {
            print_clip_progress(output_path, size, None, elapsed);
        }
    } else {
        if last_reported_bytes != Some(size) {
            print_clip_progress(output_path, size, Some(size), elapsed);
        }
    }
    print_clip_done(output_path, size, elapsed);
    if size > 0 {
        write_clip_complete_marker(output_path, size)?;
    } else {
        let _ = std::fs::remove_file(&complete_marker);
    }
    Ok(size > 0)
}

fn estimate_target_bytes(
    downloaded_bytes: u64,
    elapsed: Duration,
    clip_seconds: u32,
) -> Option<u64> {
    if clip_seconds == 0 || downloaded_bytes == 0 {
        return None;
    }

    let elapsed_secs = elapsed.as_secs_f64();
    if elapsed_secs < 1.0 {
        return None;
    }

    let bytes_per_sec = downloaded_bytes as f64 / elapsed_secs;
    let estimated = (bytes_per_sec * f64::from(clip_seconds)).round();
    if estimated <= 0.0 {
        None
    } else {
        Some(estimated as u64)
    }
}

fn print_clip_progress(
    output_path: &Path,
    downloaded_bytes: u64,
    target_bytes: Option<u64>,
    elapsed: Duration,
) {
    let downloaded = format_size(downloaded_bytes);
    let mbps = format_mbps(downloaded_bytes, elapsed);
    let name = output_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("clip");

    match target_bytes {
        Some(target) if target > 0 => {
            let percent = (downloaded_bytes as f64 / target as f64) * 100.0;
            eprintln!(
                "[progress] {}: {} / {} ({:.0}%) at {}",
                name,
                downloaded,
                format_size(target),
                percent.clamp(0.0, 999.0),
                mbps
            );
        }
        _ => {
            eprintln!("[progress] {}: {} downloaded at {}", name, downloaded, mbps);
        }
    }
}

fn print_clip_done(output_path: &Path, downloaded_bytes: u64, elapsed: Duration) {
    let name = output_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("clip");
    eprintln!(
        "[progress] {}: done {}, avg {}",
        name,
        format_size(downloaded_bytes),
        format_mbps(downloaded_bytes, elapsed)
    );
}

fn format_mbps(downloaded_bytes: u64, elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 {
        return "0.00 Mbps".to_string();
    }
    let mbps = (downloaded_bytes as f64 * 8.0) / secs / 1_000_000.0;
    format!("{mbps:.2} Mbps")
}

fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;

    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.2} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn merge_segments(final_path: &Path, segment_path: &Path) -> Result<()> {
    if !segment_path.exists() {
        return Err(AppError::Command(format!(
            "resume segment not found: {}",
            segment_path.display()
        )));
    }
    if !final_path.exists() {
        std::fs::rename(segment_path, final_path)?;
        return Ok(());
    }

    let list_path = hidden_sidecar_path(final_path, "concat.list");
    let merged_tmp = hidden_sidecar_path(final_path, "merge.tmp");

    let base_abs = absolute_path(final_path)?;
    let segment_abs = absolute_path(segment_path)?;
    let list_content = format!(
        "file '{}'\nfile '{}'\n",
        escape_concat_path(&base_abs),
        escape_concat_path(&segment_abs)
    );
    std::fs::write(&list_path, list_content)?;

    let status = StdCommand::new(resolve_ffmpeg_bin())
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-f")
        .arg("concat")
        .arg("-safe")
        .arg("0")
        .arg("-i")
        .arg(&list_path)
        .arg("-c")
        .arg("copy")
        .arg("-f")
        .arg("matroska")
        .arg(&merged_tmp)
        .status()
        .map_err(|e| AppError::Command(format!("ffmpeg concat failed to start: {e}")))?;

    let _ = std::fs::remove_file(&list_path);
    if !status.success() {
        let _ = std::fs::remove_file(&merged_tmp);
        return Err(AppError::Command(format!(
            "ffmpeg concat failed with status {status}"
        )));
    }

    if final_path.exists() {
        std::fs::remove_file(final_path)?;
    }
    std::fs::rename(&merged_tmp, final_path)?;
    let _ = std::fs::remove_file(segment_path);
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

fn escape_concat_path(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "'\\''")
}

fn hidden_sidecar_path(base: &Path, suffix: &str) -> PathBuf {
    let parent = base.parent().unwrap_or_else(|| Path::new("."));
    let name = base.file_name().and_then(|s| s.to_str()).unwrap_or("clip");
    parent.join(format!(".{name}.{suffix}"))
}

fn clip_complete_marker_path(base: &Path) -> PathBuf {
    hidden_sidecar_path(base, "complete")
}

fn write_clip_complete_marker(base: &Path, size: u64) -> Result<()> {
    let marker_path = clip_complete_marker_path(base);
    let content = format!("size={size}\nfinished_at={}\n", Utc::now().to_rfc3339());
    std::fs::write(marker_path, content)?;
    Ok(())
}

fn recover_pending_resume_segment(output_path: &Path) {
    let segment_path = hidden_sidecar_path(output_path, "resume.part");
    if !segment_path.exists() {
        return;
    }

    if output_path.exists() {
        let final_mtime = file_mtime(output_path);
        let segment_mtime = file_mtime(&segment_path);
        if let (Some(final_mtime), Some(segment_mtime)) = (final_mtime, segment_mtime) {
            if final_mtime >= segment_mtime {
                let _ = std::fs::remove_file(&segment_path);
                eprintln!(
                    "[progress] {}: removed stale resume segment",
                    output_path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("clip")
                );
                return;
            }
        }
    }

    match merge_segments(output_path, &segment_path) {
        Ok(()) => {
            eprintln!(
                "[progress] {}: recovered pending resume segment",
                output_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("clip")
            );
        }
        Err(err) => {
            warn!(
                "Failed to recover pending resume segment for {}: {}",
                output_path.display(),
                err
            );
        }
    }
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn probe_duration_seconds(path: &Path) -> Option<f64> {
    let output = StdCommand::new(resolve_ffprobe_bin())
        .arg("-v")
        .arg("error")
        .arg("-show_entries")
        .arg("format=duration")
        .arg("-of")
        .arg("default=noprint_wrappers=1:nokey=1")
        .arg(path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    raw.trim().parse::<f64>().ok().filter(|v| *v > 0.0)
}

fn resolve_ffmpeg_bin() -> String {
    if let Ok(path) = std::env::var("FFMPEG_BIN") {
        if !path.trim().is_empty() {
            return path;
        }
    }

    let local = PathBuf::from("ffmpeg").join("ffmpeg");
    if local.exists() {
        return local.display().to_string();
    }

    "ffmpeg".to_string()
}

fn resolve_ffprobe_bin() -> String {
    if let Ok(path) = std::env::var("FFPROBE_BIN") {
        if !path.trim().is_empty() {
            return path;
        }
    }

    let local = PathBuf::from("ffmpeg").join("ffprobe");
    if local.exists() {
        return local.display().to_string();
    }

    "ffprobe".to_string()
}

fn extract_track_ids(xml: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(track_start) = rest.find("<Track") {
        let after_track = &rest[track_start..];
        let Some(track_end) = after_track.find("</Track>") else {
            break;
        };
        let block = &after_track[..track_end];
        if let Some(id) = extract_xml_tag(block, "id").and_then(|v| v.parse::<u32>().ok()) {
            out.push(id);
        }
        rest = &after_track[track_end + "</Track>".len()..];
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn extract_playback_entries(xml: &str) -> Vec<PlaybackEntry> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(match_start) = rest.find("<searchMatchItem>") {
        let after = &rest[match_start..];
        let Some(match_end) = after.find("</searchMatchItem>") else {
            break;
        };
        let block = &after[..match_end];
        let start_time = extract_xml_tag(block, "startTime").unwrap_or_default();
        let end_time = extract_xml_tag(block, "endTime").unwrap_or_default();
        let playback_uri = extract_xml_tag(block, "playbackURI")
            .unwrap_or_default()
            .replace("&amp;", "&");
        if !start_time.is_empty() && playback_uri.starts_with("rtsp://") {
            out.push(PlaybackEntry {
                start_time,
                end_time: if end_time.is_empty() {
                    "unknown-end".to_string()
                } else {
                    end_time
                },
                playback_uri,
            });
        }
        rest = &after[match_end + "</searchMatchItem>".len()..];
    }
    out
}

fn inject_rtsp_auth(uri: &str, username: &str, password: &str) -> Option<String> {
    let mut with_auth = uri.replacen("rtsp://", &format!("rtsp://{username}:{password}@"), 1);
    if with_auth == uri {
        return None;
    }
    if let Some(at) = with_auth.find('@') {
        let host_start = at + 1;
        let path_start = with_auth[host_start..]
            .find('/')
            .map(|v| host_start + v)
            .unwrap_or(with_auth.len());
        let host_port = &with_auth[host_start..path_start];
        if !host_port.contains(':') {
            with_auth.insert_str(path_start, ":554");
        }
    }
    Some(with_auth)
}

fn sanitize_filename(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.len() > 100 {
        out.truncate(100);
    }
    out
}
