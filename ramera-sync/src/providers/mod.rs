pub mod generic;
pub mod hikvision;

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use crate::config::NvrConfig;
use crate::error::Result;
use crate::types::{DeviceRecord, NvrDevice};

#[derive(Debug, Clone)]
pub struct ProviderFingerprint {
    pub provider: &'static str,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub source_url: Option<String>,
    pub preview: Option<String>,
    pub is_nvr: bool,
}

pub async fn fingerprint_device(
    client: &reqwest::Client,
    ip: IpAddr,
    open_ports: &[u16],
    cfg: &NvrConfig,
) -> ProviderFingerprint {
    if let Some(fp) = hikvision::fingerprint(client, ip, open_ports, cfg).await {
        return fp;
    }

    if let Some(fp) = generic::fingerprint(client, ip, open_ports, cfg).await {
        return fp;
    }

    ProviderFingerprint {
        provider: "generic",
        vendor: None,
        model: None,
        serial: None,
        source_url: None,
        preview: None,
        is_nvr: false,
    }
}

pub async fn collect_records_for_device(
    client: &reqwest::Client,
    device: &NvrDevice,
    cfg: &NvrConfig,
) -> Vec<DeviceRecord> {
    match device.provider.as_str() {
        "hikvision" => hikvision::collect_records(client, device, cfg).await,
        _ => generic::collect_records(client, device, cfg).await,
    }
}

pub async fn download_video_clips_for_device(
    client: &reqwest::Client,
    device: &NvrDevice,
    cfg: &NvrConfig,
    days: u32,
    max_clips: usize,
    clip_seconds: u32,
    output_dir: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    match device.provider.as_str() {
        "hikvision" => {
            hikvision::download_video_clips(
                client,
                device,
                cfg,
                days,
                max_clips,
                clip_seconds,
                output_dir,
            )
            .await
        }
        _ => Ok(Vec::new()),
    }
}

pub fn build_base_urls(ip: IpAddr, open_ports: &[u16], include_https: bool) -> Vec<String> {
    let mut out = Vec::new();
    for port in open_ports {
        if *port == 80 {
            out.push(format!("http://{ip}:{port}"));
        } else if *port == 443 {
            if include_https {
                out.push(format!("https://{ip}:{port}"));
            } else {
                out.push(format!("http://{ip}:{port}"));
            }
        } else {
            out.push(format!("http://{ip}:{port}"));
            if include_https {
                out.push(format!("https://{ip}:{port}"));
            }
        }
    }
    out
}

pub fn normalize_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

pub fn preview(input: &str, max: usize) -> String {
    let mut s = input.replace('\n', " ");
    if s.len() > max {
        s.truncate(max);
    }
    s
}

pub fn merge_paths(required_paths: &[&str], config_paths: &[String]) -> Vec<String> {
    let mut merged: BTreeSet<String> = required_paths.iter().map(|v| normalize_path(v)).collect();
    for path in config_paths {
        merged.insert(normalize_path(path));
    }
    merged.into_iter().collect()
}
