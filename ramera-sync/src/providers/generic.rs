use chrono::Utc;

use crate::config::NvrConfig;
use crate::http_auth::get_with_auth;
use crate::providers::{
    build_base_urls, merge_paths, normalize_path, preview, ProviderFingerprint,
};
use crate::types::{DeviceRecord, NvrDevice};

pub async fn fingerprint(
    client: &reqwest::Client,
    ip: std::net::IpAddr,
    open_ports: &[u16],
    cfg: &NvrConfig,
) -> Option<ProviderFingerprint> {
    let endpoints = build_base_urls(ip, open_ports, cfg.include_https);
    for base in endpoints {
        for path in &cfg.record_paths {
            let path = normalize_path(path);
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

            if !response.status().is_success() {
                continue;
            }

            let body = match response.text().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            let analysis = analyze_fingerprint(&body);
            if analysis.is_nvr {
                return Some(ProviderFingerprint {
                    provider: "generic",
                    vendor: analysis.vendor,
                    model: analysis.model,
                    serial: analysis.serial,
                    source_url: Some(url),
                    preview: Some(preview(&body, 240)),
                    is_nvr: true,
                });
            }
        }
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
    let paths = merge_paths(&[], &cfg.record_paths);

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
                provider: "generic".to_string(),
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

struct GenericFingerprint {
    vendor: Option<String>,
    model: Option<String>,
    serial: Option<String>,
    is_nvr: bool,
}

fn analyze_fingerprint(body: &str) -> GenericFingerprint {
    let lower = body.to_ascii_lowercase();

    let vendor = detect_vendor(&lower);
    let model = extract_value(body, &["model", "devicetype", "deviceType", "deviceModel"]);
    let serial = extract_value(
        body,
        &["serialNumber", "serialnumber", "serialno", "sn", "deviceID"],
    );

    let nvr_keywords = [
        "nvr",
        "dvr",
        "network video recorder",
        "hikvision",
        "dahua",
        "uniview",
        "recording",
        "isapi/system/deviceinfo",
        "onvif",
    ];

    let is_nvr = nvr_keywords.iter().any(|k| lower.contains(k)) || vendor.is_some();

    GenericFingerprint {
        vendor,
        model,
        serial,
        is_nvr,
    }
}

fn detect_vendor(lower: &str) -> Option<String> {
    if lower.contains("hikvision") {
        Some("Hikvision".to_string())
    } else if lower.contains("dahua") {
        Some("Dahua".to_string())
    } else if lower.contains("uniview") || lower.contains("unv") {
        Some("Uniview".to_string())
    } else if lower.contains("axis") {
        Some("Axis".to_string())
    } else {
        None
    }
}

fn extract_value(body: &str, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(v) = extract_xml_tag(body, key) {
            return Some(v);
        }
        if let Some(v) = extract_key_value(body, key) {
            return Some(v);
        }
    }
    None
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

fn extract_key_value(body: &str, key: &str) -> Option<String> {
    let mut candidates = Vec::with_capacity(4);
    candidates.push(format!("{key}="));
    candidates.push(format!("{key}:"));
    candidates.push(format!("\"{key}\":"));
    candidates.push(format!("'{key}':"));

    let lower = body.to_ascii_lowercase();
    for candidate in candidates {
        let lower_candidate = candidate.to_ascii_lowercase();
        let start = lower.find(&lower_candidate)?;
        let value_start = start + candidate.len();
        let tail = body.get(value_start..)?.trim_start();
        let value = tail
            .trim_matches('"')
            .trim_matches('\'')
            .split(['\n', '\r', ',', ';', '"', '\''])
            .next()
            .unwrap_or("")
            .trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}
