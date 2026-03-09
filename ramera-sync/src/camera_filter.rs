use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::types::NvrDevice;

#[derive(Debug, Clone, Default)]
pub struct CameraFilter {
    pub enabled_cameras: HashMap<String, bool>,
    pub camera_names: HashMap<String, String>,
    pub track_enabled: HashMap<String, HashMap<u32, bool>>,
    pub track_names: HashMap<String, HashMap<u32, String>>,
    pub track_status: HashMap<String, HashMap<u32, String>>,
}

impl CameraFilter {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(CameraFilter {
                enabled_cameras: HashMap::new(),
                camera_names: HashMap::new(),
                track_enabled: HashMap::new(),
                track_names: HashMap::new(),
                track_status: HashMap::new(),
            });
        }

        let raw = std::fs::read_to_string(path)?;
        let mut enabled_cameras = HashMap::new();
        let mut camera_names = HashMap::new();
        let mut track_enabled: HashMap<String, HashMap<u32, bool>> = HashMap::new();
        let mut track_names: HashMap<String, HashMap<u32, String>> = HashMap::new();
        let mut track_status: HashMap<String, HashMap<u32, String>> = HashMap::new();

        for (idx, line) in raw.lines().enumerate() {
            let line_num = idx + 1;
            let trimmed = line.trim();

            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }

            let parts: Vec<&str> = trimmed.split('|').map(|s| s.trim()).collect();
            if parts.len() < 2 {
                crate::progress!("Warning: line {line_num}: skipping malformed entry");
                continue;
            }

            let ip = parts[0].to_string();
            if let Some(track_id) = parse_track_token(parts[1]) {
                if parts.len() < 3 {
                    crate::progress!("Warning: line {line_num}: track entry missing enabled value");
                    continue;
                }
                let enabled = parse_enabled(parts[2]);
                let name = if parts.len() > 3 {
                    parts[3].to_string()
                } else {
                    format!("Track {track_id}")
                };
                let status = if parts.len() > 4 {
                    parts[4].to_string()
                } else {
                    "unknown".to_string()
                };
                track_enabled
                    .entry(ip.clone())
                    .or_default()
                    .insert(track_id, enabled);
                track_names
                    .entry(ip.clone())
                    .or_default()
                    .insert(track_id, name);
                track_status.entry(ip).or_default().insert(track_id, status);
                continue;
            }

            let enabled = parse_enabled(parts[1]);
            let name = if parts.len() > 2 {
                parts[2].to_string()
            } else {
                ip.clone()
            };

            enabled_cameras.insert(ip.clone(), enabled);
            camera_names.insert(ip, name);
        }

        Ok(CameraFilter {
            enabled_cameras,
            camera_names,
            track_enabled,
            track_names,
            track_status,
        })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut lines = vec![
            "# camera-filter.conf".to_string(),
            "# Device format: ip | enabled(true/false) | friendly_name".to_string(),
            "# Track format:  ip | track101 | enabled(true/false) | friendly_name | status"
                .to_string(),
            "# status is auto-filled by discover: active / no_records / unknown".to_string(),
            "# Set enabled=true to process, false to skip".to_string(),
            "#".to_string(),
        ];

        for ip in self.sorted_ips() {
            let enabled = self.enabled_cameras.get(&ip).copied().unwrap_or(true);
            let name = self
                .camera_names
                .get(&ip)
                .map(|s| s.as_str())
                .unwrap_or(ip.as_str());
            let status = if enabled { "true" } else { "false" };
            lines.push(format!("{} | {} | {}", ip, status, name));

            if let Some(track_map) = self.track_enabled.get(&ip) {
                let mut track_ids: Vec<u32> = track_map.keys().copied().collect();
                track_ids.sort_unstable();
                for track_id in track_ids {
                    let enabled = track_map.get(&track_id).copied().unwrap_or(true);
                    let status = if enabled { "true" } else { "false" };
                    let track_name = self
                        .track_names
                        .get(&ip)
                        .and_then(|m| m.get(&track_id))
                        .cloned()
                        .unwrap_or_else(|| format!("Track {track_id}"));
                    let track_status = self
                        .track_status
                        .get(&ip)
                        .and_then(|m| m.get(&track_id))
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string());
                    lines.push(format!(
                        "{} | track{} | {} | {} | {}",
                        ip, track_id, status, track_name, track_status
                    ));
                }
            }
        }

        let content = lines.join("\n");
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn update_from_devices(&mut self, devices: &[NvrDevice]) {
        for device in devices {
            let ip = device.ip.to_string();

            if !self.enabled_cameras.contains_key(&ip) {
                self.enabled_cameras.insert(ip.clone(), true);
            }

            if !self.camera_names.contains_key(&ip) {
                let name = device
                    .model
                    .as_deref()
                    .or_else(|| device.vendor.as_deref())
                    .unwrap_or("Unknown");
                self.camera_names.insert(ip, name.to_string());
            }
        }
    }

    pub fn update_tracks_for_device(&mut self, device: &NvrDevice, tracks: &[u32]) {
        let ip = device.ip.to_string();
        for track_id in tracks {
            self.track_enabled
                .entry(ip.clone())
                .or_default()
                .entry(*track_id)
                .or_insert(true);
            self.track_names
                .entry(ip.clone())
                .or_default()
                .entry(*track_id)
                .or_insert_with(|| format!("Track {track_id}"));
            self.track_status
                .entry(ip.clone())
                .or_default()
                .entry(*track_id)
                .or_insert_with(|| "unknown".to_string());
        }
    }

    pub fn update_track_status_for_device(
        &mut self,
        device: &NvrDevice,
        statuses: &HashMap<u32, String>,
    ) {
        let ip = device.ip.to_string();
        let entry = self.track_status.entry(ip).or_default();
        for (track_id, status) in statuses {
            entry.insert(*track_id, status.clone());
        }
    }

    pub fn is_enabled(&self, device: &NvrDevice) -> bool {
        let ip = device.ip.to_string();
        self.enabled_cameras.get(&ip).copied().unwrap_or(true)
    }

    pub fn track_rules_for_device(&self, device: &NvrDevice) -> Option<HashMap<u32, bool>> {
        self.track_enabled.get(&device.ip.to_string()).cloned()
    }

    pub fn filter_devices(&self, devices: Vec<NvrDevice>) -> Vec<NvrDevice> {
        devices.into_iter().filter(|d| self.is_enabled(d)).collect()
    }

    fn sorted_ips(&self) -> Vec<String> {
        let mut ips: BTreeSet<String> = BTreeSet::new();
        ips.extend(self.enabled_cameras.keys().cloned());
        ips.extend(self.track_enabled.keys().cloned());
        ips.into_iter().collect()
    }
}

fn parse_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

fn parse_track_token(token: &str) -> Option<u32> {
    let raw = token.trim().to_ascii_lowercase();
    let value = if let Some(v) = raw.strip_prefix("track:") {
        v
    } else if let Some(v) = raw.strip_prefix("track") {
        v
    } else {
        return None;
    };

    value.parse::<u32>().ok().filter(|v| *v > 0)
}

pub fn camera_filter_path_for_config(config_path: &Path) -> PathBuf {
    let config_file = if config_path.is_absolute() {
        config_path.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(config_path)
    } else {
        config_path.to_path_buf()
    };

    config_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("camera-filter.conf")
}
