use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::error::{AppError, Result};
use crate::types::DeviceRecord;

pub fn write_verified_snapshot(payload: &[u8]) -> Result<PathBuf> {
    let records_dir = Path::new("records");
    std::fs::create_dir_all(records_dir)?;

    let day = Utc::now().format("%Y%m%d").to_string();
    let final_name = format!("snapshot-{day}.json");
    let final_path = records_dir.join(final_name);

    let ts = Utc::now().format("%Y%m%dT%H%M%S%.fZ");
    let tmp_name = format!(".tmp-{ts}-{}.json", std::process::id());
    let tmp_path = records_dir.join(tmp_name);

    std::fs::write(&tmp_path, payload)?;
    if final_path.exists() {
        std::fs::remove_file(&final_path)?;
    }
    std::fs::rename(&tmp_path, &final_path)?;

    verify_moved_file(&final_path, payload)?;
    Ok(final_path)
}

pub fn write_record_payloads(records: &[DeviceRecord]) -> Result<PathBuf> {
    let day = Utc::now().format("%Y%m%d").to_string();
    let raw_dir = Path::new("records").join("raw").join(day);
    if raw_dir.exists() {
        std::fs::remove_dir_all(&raw_dir)?;
    }
    std::fs::create_dir_all(&raw_dir)?;

    for (idx, record) in records.iter().enumerate() {
        let ext = if record.body.trim_start().starts_with('<') {
            "xml"
        } else if record.body.trim_start().starts_with('{')
            || record.body.trim_start().starts_with('[')
        {
            "json"
        } else {
            "txt"
        };
        let file_name = format!(
            "{:04}_{}_{}_{}_{}.{}",
            idx + 1,
            sanitize_filename(&record.ip),
            sanitize_filename(&record.provider),
            record.status,
            sanitize_filename(&record.path),
            ext
        );
        let path = raw_dir.join(file_name);
        std::fs::write(path, &record.body)?;
    }

    Ok(raw_dir)
}

pub fn list_local_record_days() -> Result<Vec<String>> {
    let mut days = std::collections::BTreeSet::new();

    let records_dir = Path::new("records");
    if records_dir.exists() {
        for entry in std::fs::read_dir(records_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(day) = parse_day_from_snapshot_name(&name) {
                days.insert(day);
            }
        }
    }

    let raw_root = Path::new("records").join("raw");
    if raw_root.exists() {
        for entry in std::fs::read_dir(raw_root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy().to_string();
            if is_day(&name) {
                days.insert(name);
            }
        }
    }

    let clips_root = Path::new("records").join("clips");
    if clips_root.exists() {
        for entry in std::fs::read_dir(clips_root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(day) = parse_day_from_clip_dir_name(&name) {
                days.insert(day);
            }
        }
    }

    Ok(days.into_iter().collect())
}

pub fn snapshot_path_for_day(day: &str) -> PathBuf {
    Path::new("records").join(format!("snapshot-{day}.json"))
}

pub fn raw_dir_for_day(day: &str) -> PathBuf {
    Path::new("records").join("raw").join(day)
}

pub fn clip_dir_for_day(day: &str) -> PathBuf {
    Path::new("records")
        .join("clips")
        .join(format!("snapshot-{day}"))
}

pub fn list_raw_files_for_day(day: &str) -> Result<Vec<PathBuf>> {
    let raw_dir = raw_dir_for_day(day);
    if !raw_dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    for entry in std::fs::read_dir(&raw_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

pub fn list_clip_files_for_day(day: &str) -> Result<Vec<PathBuf>> {
    let clip_dir = clip_dir_for_day(day);
    if !clip_dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    for entry in std::fs::read_dir(&clip_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        files.push(path);
    }
    files.sort();
    Ok(files)
}

pub fn delete_local_day(day: &str) -> Result<()> {
    let snapshot = snapshot_path_for_day(day);
    if snapshot.exists() {
        std::fs::remove_file(snapshot)?;
    }

    let raw_dir = raw_dir_for_day(day);
    if raw_dir.exists() {
        std::fs::remove_dir_all(raw_dir)?;
    }

    let clip_dir = clip_dir_for_day(day);
    if clip_dir.exists() {
        std::fs::remove_dir_all(clip_dir)?;
    }
    Ok(())
}

pub fn parse_day_from_snapshot_name(name: &str) -> Option<String> {
    let day = name.strip_prefix("snapshot-")?.strip_suffix(".json")?;
    if is_day(day) {
        Some(day.to_string())
    } else {
        None
    }
}

pub fn parse_day_from_clip_dir_name(name: &str) -> Option<String> {
    let day = name.strip_prefix("snapshot-")?;
    if is_day(day) {
        Some(day.to_string())
    } else {
        None
    }
}

fn verify_moved_file(path: &Path, expected: &[u8]) -> Result<()> {
    if !path.exists() {
        return Err(AppError::Storage(format!(
            "snapshot file not found after move: {}",
            path.display()
        )));
    }

    let metadata = std::fs::metadata(path)?;
    if metadata.len() != expected.len() as u64 {
        return Err(AppError::Storage(format!(
            "snapshot size mismatch: expected {}, got {}",
            expected.len(),
            metadata.len()
        )));
    }

    let actual = std::fs::read(path)?;
    if actual != expected {
        return Err(AppError::Storage(format!(
            "snapshot content verification failed: {}",
            path.display()
        )));
    }

    Ok(())
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
    if out.len() > 140 {
        out.truncate(140);
    }
    if out.is_empty() {
        "record".to_string()
    } else {
        out
    }
}

fn is_day(day: &str) -> bool {
    day.len() == 8 && day.chars().all(|c| c.is_ascii_digit())
}
