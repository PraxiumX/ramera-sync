use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::time;
use tracing::{error, info, warn};

use crate::b2::{B2Client, B2File};
use crate::config::AppConfig;
use crate::discovery::discover_devices;
use crate::error::{AppError, Result};
use crate::nvr::collect_records;
use crate::providers;
use crate::storage::{
    clip_dir_for_day, delete_local_day, list_clip_files_for_day, list_local_record_days,
    list_raw_files_for_day, raw_dir_for_day, snapshot_path_for_day, write_record_payloads,
    write_verified_snapshot,
};
use crate::types::{DeviceRecord, NvrDevice};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSnapshot {
    pub generated_at: DateTime<Utc>,
    pub device_count: usize,
    pub record_count: usize,
    pub devices: Vec<NvrDevice>,
    pub records: Vec<DeviceRecord>,
}

pub struct SyncOutcome {
    pub snapshot: SyncSnapshot,
    pub local_file: PathBuf,
    pub raw_records_dir: PathBuf,
    pub cloud: CloudSyncOutcome,
}

pub struct CloudSyncOutcome {
    pub uploaded_days: Vec<String>,
    pub deleted_local_days: Vec<String>,
    pub deleted_cloud_files: usize,
}

pub struct FetchOutcome {
    pub snapshot: SyncSnapshot,
    pub local_file: PathBuf,
    pub raw_records_dir: PathBuf,
}

pub struct ClipFetchOutcome {
    pub device_count: usize,
    pub saved_clips: Vec<PathBuf>,
}

pub async fn discover_only(cfg: &AppConfig) -> Result<Vec<NvrDevice>> {
    discover_devices(&cfg.scan, &cfg.nvr).await
}

pub async fn sync_once(cfg: &AppConfig) -> Result<SyncOutcome> {
    let b2 = B2Client::new(cfg.b2.clone());
    let fetch = fetch_or_restore_today_snapshot(cfg, &b2).await?;
    let day = day_for_snapshot(&fetch.snapshot);
    ensure_snapshot_uploaded_for_day(cfg, &b2, &day, &fetch.local_file).await?;

    let snapshot_clips_dir = clips_dir_for_snapshot(fetch.snapshot.generated_at);
    let _clips = fetch_video_clips_for_devices(
        cfg,
        &fetch.snapshot.devices,
        1,
        0,
        0,
        Some(&snapshot_clips_dir),
    )
    .await?;
    let cloud = sync_local_days_to_b2(cfg, &b2)
        .await
        .map_err(|err| match err {
            AppError::B2(msg) => AppError::B2(format!(
                "{msg}. Local snapshot retained at {} and raw records at {}",
                fetch.local_file.display(),
                fetch.raw_records_dir.display()
            )),
            other => other,
        })?;

    Ok(SyncOutcome {
        snapshot: fetch.snapshot,
        local_file: fetch.local_file,
        raw_records_dir: fetch.raw_records_dir,
        cloud,
    })
}

async fn fetch_or_restore_today_snapshot(cfg: &AppConfig, b2: &B2Client) -> Result<FetchOutcome> {
    let day = Utc::now().format("%Y%m%d").to_string();
    if !is_day_upload_eligible(cfg, &day) {
        return fetch_records_to_local(cfg).await;
    }

    let local_snapshot = snapshot_path_for_day(&day);
    let local_raw = raw_dir_for_day(&day);
    let local_clips = clip_dir_for_day(&day);

    // If this day is already in progress locally, keep polling NVR for fresh metadata.
    if local_snapshot.exists() || local_raw.exists() || local_clips.exists() {
        return fetch_records_to_local(cfg).await;
    }

    let prefix_root = cfg.b2.file_prefix.trim_end_matches('/');
    let snapshot_remote_name = format!("{prefix_root}/records/{day}/snapshot-{day}.json");

    match b2_file_exists_retry(b2, &snapshot_remote_name).await {
        Ok(true) => {
            info!(
                "Cloud snapshot already exists for day {}, restoring locally from {}",
                day, snapshot_remote_name
            );
            let data = b2_download_retry(b2, &snapshot_remote_name).await?;
            let snapshot: SyncSnapshot = match serde_json::from_slice(&data) {
                Ok(v) => v,
                Err(err) => {
                    warn!(
                        "failed to parse restored cloud snapshot {}: {}. Falling back to fresh NVR fetch",
                        snapshot_remote_name, err
                    );
                    return fetch_records_to_local(cfg).await;
                }
            };
            let local_file = write_verified_snapshot(&data)?;
            let raw_records_dir = raw_dir_for_day(&day);
            return Ok(FetchOutcome {
                snapshot,
                local_file,
                raw_records_dir,
            });
        }
        Ok(false) => {}
        Err(err) => {
            warn!(
                "Cloud snapshot pre-check failed for day {}: {}. Falling back to fresh NVR fetch",
                day, err
            );
        }
    }

    fetch_records_to_local(cfg).await
}

fn day_for_snapshot(snapshot: &SyncSnapshot) -> String {
    snapshot.generated_at.format("%Y%m%d").to_string()
}

async fn ensure_snapshot_uploaded_for_day(
    cfg: &AppConfig,
    b2: &B2Client,
    day: &str,
    local_snapshot_path: &Path,
) -> Result<()> {
    if !is_day_upload_eligible(cfg, day) {
        eprintln!(
            "[progress] cloud snapshot deferred by lag policy (day={}, lag_days={})",
            day, cfg.b2.upload_lag_days
        );
        return Ok(());
    }

    let prefix_root = cfg.b2.file_prefix.trim_end_matches('/');
    let snapshot_remote_name = format!("{prefix_root}/records/{day}/snapshot-{day}.json");
    if b2_file_exists_retry(b2, &snapshot_remote_name).await? {
        eprintln!(
            "[progress] cloud snapshot already present: {}",
            snapshot_remote_name
        );
        return Ok(());
    }
    if !local_snapshot_path.exists() {
        return Err(AppError::Storage(format!(
            "local snapshot missing for upload: {}",
            local_snapshot_path.display()
        )));
    }

    let data = std::fs::read(local_snapshot_path)?;
    eprintln!(
        "[progress] cloud upload snapshot: {} ({})",
        snapshot_remote_name,
        format_size(data.len() as u64)
    );
    b2_upload_retry(b2, &snapshot_remote_name, "application/json", &data).await?;
    eprintln!(
        "[progress] cloud uploaded snapshot: {}",
        snapshot_remote_name
    );
    info!(
        "Uploaded snapshot early for day {} to {}",
        day, snapshot_remote_name
    );
    Ok(())
}

pub async fn fetch_records_to_local(cfg: &AppConfig) -> Result<FetchOutcome> {
    let devices = discover_devices(&cfg.scan, &cfg.nvr).await?;
    let client = reqwest::Client::new();
    let records = collect_records(&client, &devices, &cfg.nvr).await;

    let snapshot = SyncSnapshot {
        generated_at: Utc::now(),
        device_count: devices.len(),
        record_count: records.len(),
        devices,
        records,
    };

    let payload = serde_json::to_vec_pretty(&snapshot)?;
    let local_file = write_verified_snapshot(&payload)?;
    let raw_records_dir = write_record_payloads(&snapshot.records)?;

    Ok(FetchOutcome {
        snapshot,
        local_file,
        raw_records_dir,
    })
}

pub async fn run_loop(cfg: &AppConfig) -> Result<()> {
    let mut ticker = time::interval(Duration::from_secs(cfg.scheduler.interval_seconds.max(5)));
    ticker.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

    info!(
        "Starting periodic sync: every {} seconds",
        cfg.scheduler.interval_seconds
    );

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let today = Utc::now().format("%Y%m%d").to_string();
                let b2 = B2Client::new(cfg.b2.clone());
                let prefix = cfg.b2.file_prefix.trim_end_matches('/');
                let marker_name = format!("{prefix}/records/{today}/_complete.json");
                match b2_file_exists_retry(&b2, &marker_name).await {
                    Ok(true) => {
                        match sync_local_days_to_b2(cfg, &b2).await {
                            Ok(cloud) => {
                                info!(
                                    "Day {today} already uploaded. Cleanup complete: local day(s) deleted={}, cloud file(s) deleted={}",
                                    cloud.deleted_local_days.len(),
                                    cloud.deleted_cloud_files
                                );
                            }
                            Err(err) => {
                                error!("Cleanup cycle failed: {err}");
                            }
                        }
                    }
                    Ok(false) => {
                        info!("Starting sync cycle for day {today}");
                        match sync_once(cfg).await {
                            Ok(outcome) => {
                                info!(
                                    "Sync cycle complete: {} devices, {} records, saved snapshot {}, raw {}, uploaded day(s)={}, local day(s) deleted={}, cloud file(s) deleted={}",
                                    outcome.snapshot.device_count,
                                    outcome.snapshot.record_count,
                                    outcome.local_file.display(),
                                    outcome.raw_records_dir.display(),
                                    outcome.cloud.uploaded_days.len(),
                                    outcome.cloud.deleted_local_days.len(),
                                    outcome.cloud.deleted_cloud_files
                                );
                            }
                            Err(err) => {
                                error!("Sync cycle failed: {err}");
                            }
                        }
                    }
                    Err(err) => {
                        error!("Pre-check cycle failed: {err}");
                    }
                }
            }
            signal = tokio::signal::ctrl_c() => {
                if signal.is_ok() {
                    warn!("Received Ctrl+C, shutting down sync loop");
                }
                break;
            }
        }
    }

    Ok(())
}

pub async fn run_local_loop(cfg: &AppConfig) -> Result<()> {
    let mut ticker = time::interval(Duration::from_secs(cfg.scheduler.interval_seconds.max(5)));
    ticker.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

    info!(
        "Starting local snapshot+clip pull: every {} seconds",
        cfg.scheduler.interval_seconds
    );

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                info!("Starting local snapshot+clip cycle");
                match fetch_records_to_local(cfg).await {
                    Ok(snapshot_outcome) => {
                        info!(
                            "Local snapshot complete: {} devices, {} records, saved snapshot {}, raw {}",
                            snapshot_outcome.snapshot.device_count,
                            snapshot_outcome.snapshot.record_count,
                            snapshot_outcome.local_file.display(),
                            snapshot_outcome.raw_records_dir.display()
                        );
                        let snapshot_clips_dir =
                            clips_dir_for_snapshot(snapshot_outcome.snapshot.generated_at);
                        match fetch_video_clips_for_devices(
                            cfg,
                            &snapshot_outcome.snapshot.devices,
                            1,
                            0,
                            0,
                            Some(&snapshot_clips_dir),
                        )
                        .await
                        {
                            Ok(clips_outcome) => {
                                info!(
                                    "Local clip download complete: {} device(s), {} file(s) downloaded to {}",
                                    clips_outcome.device_count,
                                    clips_outcome.saved_clips.len(),
                                    snapshot_clips_dir.display()
                                );
                            }
                            Err(err) => {
                                error!("Local clip download failed: {err}");
                            }
                        }
                    }
                    Err(err) => {
                        error!("Local snapshot cycle failed: {err}");
                    }
                }
            }
            signal = tokio::signal::ctrl_c() => {
                if signal.is_ok() {
                    warn!("Received Ctrl+C, shutting down local snapshot+clip loop");
                }
                break;
            }
        }
    }

    Ok(())
}

pub async fn fetch_video_clips_local(
    cfg: &AppConfig,
    days: u32,
    max_clips: usize,
    clip_seconds: u32,
) -> Result<ClipFetchOutcome> {
    eprintln!(
        "[progress] starting video download: days={}, max_clips={}, clip_seconds={}",
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
    info!(
        "Starting video download: days={}, max_clips={}, clip_seconds={}",
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
    eprintln!("[progress] discovering devices");
    info!("Discovering devices for video download");
    let devices = discover_devices(&cfg.scan, &cfg.nvr).await?;
    eprintln!("[progress] discovered {} device(s)", devices.len());
    info!("Discovered {} device(s) for video download", devices.len());
    fetch_video_clips_for_devices(cfg, &devices, days, max_clips, clip_seconds, None).await
}

async fn fetch_video_clips_for_devices(
    cfg: &AppConfig,
    devices: &[NvrDevice],
    days: u32,
    max_clips: usize,
    clip_seconds: u32,
    output_dir: Option<&Path>,
) -> Result<ClipFetchOutcome> {
    let client = reqwest::Client::new();
    let mut clips = Vec::new();

    if max_clips == 0 && output_dir.is_none() {
        let parallelism = devices.len().clamp(1, 4);
        let output_dir = output_dir.map(Path::to_path_buf);
        eprintln!(
            "[progress] starting parallel download across {} device(s), concurrency={}",
            devices.len(),
            parallelism
        );
        info!(
            "Starting parallel download across {} device(s), concurrency={}",
            devices.len(),
            parallelism
        );

        let mut jobs = stream::iter(devices.iter().cloned().map(|device| {
            let client = client.clone();
            let nvr = cfg.nvr.clone();
            let output_dir = output_dir.clone();
            async move {
                eprintln!(
                    "[progress] processing device {} provider={}",
                    device.ip, device.provider
                );
                info!(
                    "Processing device {} provider={} ports={:?}",
                    device.ip, device.provider, device.open_ports
                );
                let result = providers::download_video_clips_for_device(
                    &client,
                    &device,
                    &nvr,
                    days,
                    0,
                    clip_seconds,
                    output_dir.as_deref(),
                )
                .await;
                (device, result)
            }
        }))
        .buffer_unordered(parallelism);

        while let Some((device, result)) = jobs.next().await {
            let downloaded = result?;
            eprintln!(
                "[progress] device {} done, downloaded {} file(s)",
                device.ip,
                downloaded.len()
            );
            info!(
                "Device {} finished: downloaded {} file(s)",
                device.ip,
                downloaded.len()
            );
            clips.extend(downloaded);
        }

        eprintln!(
            "[progress] completed: {} device(s), {} saved file(s)",
            devices.len(),
            clips.len()
        );
        info!(
            "Video download completed: {} device(s), {} saved file(s)",
            devices.len(),
            clips.len()
        );
        return Ok(ClipFetchOutcome {
            device_count: devices.len(),
            saved_clips: clips,
        });
    }

    for device in devices {
        eprintln!(
            "[progress] processing device {} provider={}",
            device.ip, device.provider
        );
        info!(
            "Processing device {} provider={} ports={:?}",
            device.ip, device.provider, device.open_ports
        );
        let before = clips.len();
        let per_device_limit = if max_clips == 0 {
            0
        } else {
            max_clips.saturating_sub(clips.len())
        };
        let mut downloaded = providers::download_video_clips_for_device(
            &client,
            device,
            &cfg.nvr,
            days,
            per_device_limit,
            clip_seconds,
            output_dir,
        )
        .await?;
        clips.append(&mut downloaded);
        let added = clips.len().saturating_sub(before);
        eprintln!(
            "[progress] device {} done, downloaded {} file(s)",
            device.ip, added
        );
        info!(
            "Device {} finished: downloaded {} file(s)",
            device.ip, added
        );
        if max_clips > 0 && clips.len() >= max_clips {
            eprintln!("[progress] reached max_clips limit ({max_clips})");
            info!("Reached global max_clips limit ({max_clips})");
            break;
        }
    }

    eprintln!(
        "[progress] completed: {} device(s), {} saved file(s)",
        devices.len(),
        clips.len()
    );
    info!(
        "Video download completed: {} device(s), {} saved file(s)",
        devices.len(),
        clips.len()
    );
    Ok(ClipFetchOutcome {
        device_count: devices.len(),
        saved_clips: clips,
    })
}

fn clips_dir_for_snapshot(generated_at: DateTime<Utc>) -> PathBuf {
    let day = generated_at.format("%Y%m%d").to_string();
    clip_dir_for_day(&day)
}

async fn sync_local_days_to_b2(cfg: &AppConfig, b2: &B2Client) -> Result<CloudSyncOutcome> {
    let keep_days = cfg.b2.max_upload_days.max(1);
    let min_day = (Utc::now() - ChronoDuration::days(i64::from(keep_days - 1)))
        .format("%Y%m%d")
        .to_string();
    let cutoff_day = upload_cutoff_day(cfg);
    let mut local_days = list_local_record_days()?;
    local_days.sort();

    let mut uploaded_days = Vec::new();
    let mut deleted_local_days = Vec::new();

    for day in &local_days {
        if day < &min_day {
            delete_local_day(day)?;
            deleted_local_days.push(day.clone());
        }
    }

    let prefix_root = cfg.b2.file_prefix.trim_end_matches('/');
    for day in local_days.into_iter().filter(|d| d >= &min_day) {
        if day.as_str() > cutoff_day.as_str() {
            let hot_day_marker = format!("{prefix_root}/records/{day}/_complete.json");
            if b2_file_exists_retry(b2, &hot_day_marker).await? {
                warn!(
                    "Deferred day {} has completion marker; deleting marker to keep day mutable",
                    day
                );
                let marker_files = b2_list_files_retry(b2, &hot_day_marker).await?;
                for marker in marker_files
                    .into_iter()
                    .filter(|f| f.file_name == hot_day_marker)
                {
                    b2_delete_retry(b2, &marker.file_id, &marker.file_name).await?;
                }
            }
            continue;
        }

        let marker_name = format!("{prefix_root}/records/{day}/_complete.json");
        if b2_file_exists_retry(b2, &marker_name).await? {
            let snapshot_remote_name = format!("{prefix_root}/records/{day}/snapshot-{day}.json");
            if !b2_file_exists_retry(b2, &snapshot_remote_name).await? {
                warn!(
                    "Cloud day {} has completion marker but missing snapshot; attempting repair",
                    day
                );
                let snapshot = snapshot_path_for_day(&day);
                let raw_dir = raw_dir_for_day(&day);
                let clip_dir = clip_dir_for_day(&day);
                if snapshot.exists() || raw_dir.exists() || clip_dir.exists() {
                    upload_local_day(cfg, b2, &day, true, false, true).await?;
                    delete_local_day(&day)?;
                    deleted_local_days.push(day.clone());
                    uploaded_days.push(day.clone());
                    continue;
                }

                warn!(
                    "Cloud day {} missing snapshot and local data unavailable; deleting stale marker to force re-capture",
                    day
                );
                let marker_files = b2_list_files_retry(b2, &marker_name).await?;
                for marker in marker_files
                    .into_iter()
                    .filter(|f| f.file_name == marker_name)
                {
                    b2_delete_retry(b2, &marker.file_id, &marker.file_name).await?;
                }
                continue;
            }

            let snapshot = snapshot_path_for_day(&day);
            let raw_dir = raw_dir_for_day(&day);
            let clip_dir = clip_dir_for_day(&day);
            if snapshot.exists() || raw_dir.exists() || clip_dir.exists() {
                delete_local_day(&day)?;
                deleted_local_days.push(day.clone());
            }
            continue;
        }

        upload_local_day(cfg, b2, &day, true, false, true).await?;
        delete_local_day(&day)?;
        deleted_local_days.push(day.clone());
        uploaded_days.push(day);
    }

    let deleted_cloud_files = delete_old_cloud_days(cfg, b2, &min_day).await?;
    Ok(CloudSyncOutcome {
        uploaded_days,
        deleted_local_days,
        deleted_cloud_files,
    })
}

async fn upload_local_day(
    cfg: &AppConfig,
    b2: &B2Client,
    day: &str,
    finalize_day: bool,
    overwrite_snapshot: bool,
    remove_local_after_upload: bool,
) -> Result<()> {
    let prefix_root = cfg.b2.file_prefix.trim_end_matches('/');
    let day_prefix = format!("{prefix_root}/records/{day}/");
    let snapshot_remote_name = format!("{prefix_root}/records/{day}/snapshot-{day}.json");
    let snapshot_path = snapshot_path_for_day(day);
    let raw_files = list_raw_files_for_day(day)?;
    let clip_files = list_clip_files_for_day(day)?;
    let snapshot_present = snapshot_path.exists();
    let raw_file_count = raw_files.len();
    let clip_file_count = clip_files.len();
    let mut uploaded_files = 0usize;
    let mut uploaded_bytes = 0u64;
    let mut pending_incomplete_clips = 0usize;

    eprintln!(
        "[progress] cloud sync day {}: mode={}, snapshot={}, raw={}, clips={}",
        day,
        if finalize_day { "finalize" } else { "open" },
        if snapshot_present { "yes" } else { "no" },
        raw_file_count,
        clip_file_count
    );

    let existing_remote = b2_list_files_retry(b2, &day_prefix).await?;
    let mut remote_names: HashSet<String> =
        existing_remote.into_iter().map(|f| f.file_name).collect();
    let remote_payload_exists = remote_names
        .iter()
        .any(|name| !name.ends_with("/_complete.json"));

    if !snapshot_present && raw_files.is_empty() && clip_files.is_empty() && !remote_payload_exists
    {
        return Ok(());
    }

    if snapshot_present {
        if overwrite_snapshot || !remote_names.contains(&snapshot_remote_name) {
            let data = std::fs::read(&snapshot_path)?;
            eprintln!(
                "[progress] cloud upload snapshot: {} ({})",
                snapshot_remote_name,
                format_size(data.len() as u64)
            );
            b2_upload_retry(b2, &snapshot_remote_name, "application/json", &data).await?;
            remote_names.insert(snapshot_remote_name.clone());
            uploaded_files += 1;
            uploaded_bytes += data.len() as u64;
            eprintln!(
                "[progress] cloud uploaded snapshot: {}",
                snapshot_remote_name
            );
        }
        if remove_local_after_upload && snapshot_path.exists() {
            std::fs::remove_file(&snapshot_path)?;
        }
    }

    for raw_path in raw_files {
        let Some(name) = raw_path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let data = std::fs::read(&raw_path)?;
        let content_type = if name.ends_with(".xml") {
            "application/xml"
        } else if name.ends_with(".json") {
            "application/json"
        } else {
            "text/plain"
        };
        let file_name = format!("{prefix_root}/records/{day}/raw/{name}");
        if !remote_names.contains(&file_name) {
            eprintln!(
                "[progress] cloud upload raw: {} ({})",
                file_name,
                format_size(data.len() as u64)
            );
            b2_upload_retry(b2, &file_name, content_type, &data).await?;
            remote_names.insert(file_name);
            uploaded_files += 1;
            uploaded_bytes += data.len() as u64;
            eprintln!("[progress] cloud uploaded raw: {}", name);
        }
        if remove_local_after_upload && raw_path.exists() {
            std::fs::remove_file(&raw_path)?;
        }
    }

    for clip_path in clip_files {
        let Some(name) = clip_path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let complete_marker = clip_complete_marker_path(&clip_path);
        let resume_sidecar = clip_resume_sidecar_path(&clip_path);
        if resume_sidecar.exists() || !complete_marker.exists() {
            pending_incomplete_clips += 1;
            eprintln!(
                "[progress] cloud skip clip (incomplete): {} (resume_part={}, complete_marker={})",
                name,
                if resume_sidecar.exists() { "yes" } else { "no" },
                if complete_marker.exists() {
                    "yes"
                } else {
                    "no"
                }
            );
            continue;
        }
        let data = std::fs::read(&clip_path)?;
        let content_type = if name.ends_with(".mkv") {
            "video/x-matroska"
        } else {
            "application/octet-stream"
        };
        let file_name = format!("{prefix_root}/records/{day}/clips/{name}");
        if !remote_names.contains(&file_name) {
            eprintln!(
                "[progress] cloud upload clip: {} ({})",
                file_name,
                format_size(data.len() as u64)
            );
            b2_upload_retry(b2, &file_name, content_type, &data).await?;
            remote_names.insert(file_name);
            uploaded_files += 1;
            uploaded_bytes += data.len() as u64;
            eprintln!("[progress] cloud uploaded clip: {}", name);
        }
        if remove_local_after_upload && clip_path.exists() {
            std::fs::remove_file(&clip_path)?;
            let _ = std::fs::remove_file(&complete_marker);
            let _ = std::fs::remove_file(&resume_sidecar);
        }
    }

    if !remote_names.contains(&snapshot_remote_name) {
        return Err(AppError::B2(format!(
            "Refusing day {day} sync: missing remote snapshot {}",
            snapshot_remote_name
        )));
    }

    if !finalize_day {
        if pending_incomplete_clips > 0 {
            eprintln!(
                "[progress] cloud day {} has {} incomplete clip(s); will retry next cycle",
                day, pending_incomplete_clips
            );
        }
        eprintln!(
            "[progress] cloud day {} synced: {} file(s), {} uploaded",
            day,
            uploaded_files,
            format_size(uploaded_bytes)
        );
        return Ok(());
    }

    if pending_incomplete_clips > 0 {
        return Err(AppError::Storage(format!(
            "refusing to finalize day {}: {} incomplete clip(s) pending",
            day, pending_incomplete_clips
        )));
    }

    let marker_name = format!("{prefix_root}/records/{day}/_complete.json");
    let marker = serde_json::to_vec_pretty(&serde_json::json!({
        "day": day,
        "uploaded_at": Utc::now(),
        "snapshot_present": snapshot_present,
        "raw_file_count": raw_file_count,
        "clip_file_count": clip_file_count
    }))?;
    if !remote_names.contains(&marker_name) {
        eprintln!(
            "[progress] cloud upload marker: {} ({})",
            marker_name,
            format_size(marker.len() as u64)
        );
        b2_upload_retry(b2, &marker_name, "application/json", &marker).await?;
        uploaded_files += 1;
        uploaded_bytes += marker.len() as u64;
        eprintln!("[progress] cloud uploaded marker: {}", marker_name);
    }
    eprintln!(
        "[progress] cloud day {} finalized: {} file(s), {} uploaded",
        day,
        uploaded_files,
        format_size(uploaded_bytes)
    );
    Ok(())
}

async fn delete_old_cloud_days(cfg: &AppConfig, b2: &B2Client, min_day: &str) -> Result<usize> {
    let prefix_root = cfg.b2.file_prefix.trim_end_matches('/');
    let mut deleted = 0usize;

    let records_prefix = format!("{prefix_root}/records/");
    let records_files = b2_list_files_retry(b2, &records_prefix).await?;
    for file in records_files {
        if let Some(day) = extract_day_from_remote_name(&prefix_root, &file.file_name) {
            if day.as_str() < min_day {
                b2_delete_retry(b2, &file.file_id, &file.file_name).await?;
                deleted += 1;
            }
        }
    }

    // Cleanup legacy snapshot uploads from older versions:
    // <file_prefix>/snapshot-YYYYMMDDTHHMMSSZ.json
    let legacy_prefix = format!("{prefix_root}/snapshot-");
    let legacy_files = b2_list_files_retry(b2, &legacy_prefix).await?;
    for file in legacy_files {
        if let Some(day) = extract_day_from_legacy_snapshot_name(&prefix_root, &file.file_name) {
            if day.as_str() < min_day {
                b2_delete_retry(b2, &file.file_id, &file.file_name).await?;
                deleted += 1;
            }
        }
    }

    Ok(deleted)
}

fn extract_day_from_remote_name(prefix_root: &str, file_name: &str) -> Option<String> {
    let records_prefix = format!("{}/records/", prefix_root.trim_end_matches('/'));
    let tail = file_name.strip_prefix(&records_prefix)?;
    let day = tail.split('/').next()?;
    if day.len() == 8 && day.chars().all(|c| c.is_ascii_digit()) {
        Some(day.to_string())
    } else {
        None
    }
}

fn extract_day_from_legacy_snapshot_name(prefix_root: &str, file_name: &str) -> Option<String> {
    let legacy_prefix = format!("{}/snapshot-", prefix_root.trim_end_matches('/'));
    let tail = file_name.strip_prefix(&legacy_prefix)?;
    let day = tail.get(0..8)?;
    if day.len() == 8 && day.chars().all(|c| c.is_ascii_digit()) {
        Some(day.to_string())
    } else {
        None
    }
}

async fn b2_file_exists_retry(b2: &B2Client, file_name: &str) -> Result<bool> {
    retry_b2("b2.file_exists", || async {
        b2.file_exists(file_name).await
    })
    .await
}

async fn b2_list_files_retry(b2: &B2Client, prefix: &str) -> Result<Vec<B2File>> {
    retry_b2("b2.list_files", || async { b2.list_files(prefix).await }).await
}

async fn b2_download_retry(b2: &B2Client, file_name: &str) -> Result<Vec<u8>> {
    retry_b2("b2.download_named_bytes", || async {
        b2.download_named_bytes(file_name).await
    })
    .await
}

async fn b2_upload_retry(
    b2: &B2Client,
    file_name: &str,
    content_type: &str,
    data: &[u8],
) -> Result<()> {
    retry_b2("b2.upload_named_bytes", || async {
        b2.upload_named_bytes(file_name, content_type, data).await
    })
    .await?;
    Ok(())
}

async fn b2_delete_retry(b2: &B2Client, file_id: &str, file_name: &str) -> Result<()> {
    retry_b2("b2.delete_file_version", || async {
        b2.delete_file_version(file_id, file_name).await
    })
    .await?;
    Ok(())
}

async fn retry_b2<T, F, Fut>(label: &str, mut op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    const MAX_ATTEMPTS: usize = 5;
    let mut delay_secs = 1u64;

    for attempt in 1..=MAX_ATTEMPTS {
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                let transient = matches!(err, AppError::B2(_) | AppError::Http(_));
                if transient && attempt < MAX_ATTEMPTS {
                    warn!(
                        "{} failed (attempt {}/{}): {}. retrying in {}s",
                        label, attempt, MAX_ATTEMPTS, err, delay_secs
                    );
                    time::sleep(Duration::from_secs(delay_secs)).await;
                    delay_secs = (delay_secs * 2).min(16);
                    continue;
                }
                return Err(err);
            }
        }
    }

    Err(AppError::B2(format!(
        "{label} failed after {MAX_ATTEMPTS} attempts"
    )))
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

fn upload_cutoff_day(cfg: &AppConfig) -> String {
    (Utc::now() - ChronoDuration::days(i64::from(cfg.b2.upload_lag_days)))
        .format("%Y%m%d")
        .to_string()
}

fn is_day_upload_eligible(cfg: &AppConfig, day: &str) -> bool {
    let cutoff = upload_cutoff_day(cfg);
    day <= cutoff.as_str()
}

fn clip_complete_marker_path(clip_path: &Path) -> PathBuf {
    hidden_clip_sidecar_path(clip_path, "complete")
}

fn clip_resume_sidecar_path(clip_path: &Path) -> PathBuf {
    hidden_clip_sidecar_path(clip_path, "resume.part")
}

fn hidden_clip_sidecar_path(base: &Path, suffix: &str) -> PathBuf {
    let parent = base.parent().unwrap_or_else(|| Path::new("."));
    let name = base.file_name().and_then(|s| s.to_str()).unwrap_or("clip");
    parent.join(format!(".{name}.{suffix}"))
}
