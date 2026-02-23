use std::path::Path;

use ipnet::IpNet;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub scan: ScanConfig,
    pub nvr: NvrConfig,
    pub scheduler: SchedulerConfig,
    pub b2: B2Config,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanConfig {
    pub cidr: String,
    pub ports: Vec<u16>,
    pub connect_timeout_ms: u64,
    pub concurrency: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvrConfig {
    pub username: Option<String>,
    pub password: Option<String>,
    pub record_paths: Vec<String>,
    pub request_timeout_ms: u64,
    pub include_https: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerConfig {
    pub interval_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct B2Config {
    pub key_id: String,
    pub application_key: String,
    pub bucket_id: String,
    pub file_prefix: String,
    pub api_base: Option<String>,
    pub max_upload_days: u32,
    pub upload_lag_days: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            scan: ScanConfig {
                cidr: "192.168.1.0/24".to_string(),
                ports: vec![80, 443, 554, 8000],
                connect_timeout_ms: 600,
                concurrency: 128,
            },
            nvr: NvrConfig {
                username: None,
                password: None,
                record_paths: vec![
                    "/ISAPI/System/deviceInfo".to_string(),
                    "/ISAPI/ContentMgmt/record/tracks".to_string(),
                    "/cgi-bin/magicBox.cgi?action=getSystemInfo".to_string(),
                    "/api/records".to_string(),
                ],
                request_timeout_ms: 1_500,
                include_https: true,
            },
            scheduler: SchedulerConfig {
                interval_seconds: 300,
            },
            b2: B2Config {
                key_id: "${B2_KEY_ID}".to_string(),
                application_key: "${B2_APPLICATION_KEY}".to_string(),
                bucket_id: "${B2_BUCKET_ID}".to_string(),
                file_prefix: "ramera/nvr-snapshots".to_string(),
                api_base: None,
                max_upload_days: 7,
                upload_lag_days: 1,
            },
        }
    }
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        let mut cfg = AppConfig::parse_from_conf(&raw)?;
        cfg.resolve_env();
        Ok(cfg)
    }

    pub fn write_default(path: &Path) -> Result<()> {
        std::fs::write(path, AppConfig::default_conf_text())?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        self.scan.cidr.parse::<IpNet>().map_err(|e| {
            AppError::ConfigParse(format!("invalid scan.cidr `{}`: {e}", self.scan.cidr))
        })?;

        if self.scan.ports.is_empty() {
            return Err(AppError::ConfigParse(
                "scan.ports must have at least one port".to_string(),
            ));
        }
        if self.scan.concurrency == 0 {
            return Err(AppError::ConfigParse(
                "scan.concurrency must be > 0".to_string(),
            ));
        }
        if self.scan.connect_timeout_ms < 100 {
            return Err(AppError::ConfigParse(
                "scan.connect_timeout_ms should be >= 100".to_string(),
            ));
        }
        if self.nvr.request_timeout_ms < 100 {
            return Err(AppError::ConfigParse(
                "nvr.request_timeout_ms should be >= 100".to_string(),
            ));
        }
        if self.scheduler.interval_seconds < 5 {
            return Err(AppError::ConfigParse(
                "scheduler.interval_seconds must be >= 5".to_string(),
            ));
        }
        if self.b2.max_upload_days == 0 {
            return Err(AppError::ConfigParse(
                "b2.max_retentation_days must be >= 1".to_string(),
            ));
        }
        if self.b2.upload_lag_days >= self.b2.max_upload_days {
            return Err(AppError::ConfigParse(format!(
                "b2.upload_lag_days ({}) must be less than b2.max_retentation_days ({})",
                self.b2.upload_lag_days, self.b2.max_upload_days
            )));
        }

        let b2_fields = [
            self.b2.key_id.trim(),
            self.b2.application_key.trim(),
            self.b2.bucket_id.trim(),
        ];
        let b2_set = b2_fields.iter().filter(|v| !v.is_empty()).count();
        if b2_set != 0 && b2_set != b2_fields.len() {
            return Err(AppError::ConfigParse(
                "b2.key_id, b2.application_key, and b2.bucket_id must be set together".to_string(),
            ));
        }
        if b2_set == b2_fields.len() {
            let prefix = self.b2.file_prefix.trim().trim_matches('/');
            if prefix.is_empty() {
                return Err(AppError::ConfigParse(
                    "b2.file_prefix must be non-empty when B2 upload is enabled".to_string(),
                ));
            }
        }

        if self.nvr.username.is_some() ^ self.nvr.password.is_some() {
            return Err(AppError::ConfigParse(
                "nvr.username and nvr.password must be set together".to_string(),
            ));
        }

        Ok(())
    }

    fn resolve_env(&mut self) {
        self.b2.key_id = resolve_env_token(&self.b2.key_id);
        self.b2.application_key = resolve_env_token(&self.b2.application_key);
        self.b2.bucket_id = resolve_env_token(&self.b2.bucket_id);
        self.b2.file_prefix = resolve_env_token(&self.b2.file_prefix);
    }

    fn parse_from_conf(raw: &str) -> Result<Self> {
        let mut cfg = AppConfig::default();

        for (idx, line) in raw.lines().enumerate() {
            let line_num = idx + 1;
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
                continue;
            }

            let (raw_key, raw_value) = trimmed.split_once('=').ok_or_else(|| {
                AppError::ConfigParse(format!("line {line_num}: expected key=value"))
            })?;
            let key = raw_key.trim();
            let value = raw_value.trim();

            match key {
                "scan.cidr" => cfg.scan.cidr = parse_string(value),
                "scan.ports" => cfg.scan.ports = parse_list_u16(value, line_num)?,
                "scan.connect_timeout_ms" => {
                    cfg.scan.connect_timeout_ms = parse_u64(value, line_num, key)?
                }
                "scan.concurrency" => cfg.scan.concurrency = parse_usize(value, line_num, key)?,
                "nvr.username" => cfg.nvr.username = parse_optional_string(value),
                "nvr.password" => cfg.nvr.password = parse_optional_string(value),
                "nvr.record_paths" => cfg.nvr.record_paths = parse_list_string(value),
                "nvr.request_timeout_ms" => {
                    cfg.nvr.request_timeout_ms = parse_u64(value, line_num, key)?
                }
                "nvr.include_https" => cfg.nvr.include_https = parse_bool(value, line_num, key)?,
                "scheduler.interval_seconds" => {
                    cfg.scheduler.interval_seconds = parse_u64(value, line_num, key)?
                }
                "b2.key_id" => cfg.b2.key_id = parse_string(value),
                "b2.application_key" => cfg.b2.application_key = parse_string(value),
                "b2.bucket_id" => cfg.b2.bucket_id = parse_string(value),
                "b2.file_prefix" => cfg.b2.file_prefix = parse_string(value),
                "b2.api_base" => cfg.b2.api_base = parse_optional_string(value),
                "b2.max_retentation_days" | "b2.max_upload_days" => {
                    cfg.b2.max_upload_days = parse_u32(value, line_num, key)?;
                }
                "b2.upload_lag_days" => {
                    cfg.b2.upload_lag_days = parse_u32(value, line_num, key)?;
                }
                _ => {
                    return Err(AppError::ConfigParse(format!(
                        "line {line_num}: unknown key `{key}`"
                    )));
                }
            }
        }

        if cfg.nvr.record_paths.is_empty() {
            return Err(AppError::ConfigParse(
                "nvr.record_paths must have at least one path".to_string(),
            ));
        }
        if cfg.scan.ports.is_empty() {
            return Err(AppError::ConfigParse(
                "scan.ports must have at least one port".to_string(),
            ));
        }

        Ok(cfg)
    }

    fn default_conf_text() -> String {
        let cfg = AppConfig::default();
        format!(
            "\
# ramera-sync settings.conf
# Format: key=value
# Lists are comma-separated values.
# Empty value means None for optional fields.

scan.cidr={}
scan.ports={}
scan.connect_timeout_ms={}
scan.concurrency={}

nvr.username=
nvr.password=
nvr.record_paths={}
nvr.request_timeout_ms={}
nvr.include_https={}

scheduler.interval_seconds={}

b2.key_id={}
b2.application_key={}
b2.bucket_id={}
b2.file_prefix={}
b2.api_base=https://api.backblazeb2.com
b2.max_retentation_days={}
b2.upload_lag_days={}
",
            cfg.scan.cidr,
            join_u16(&cfg.scan.ports),
            cfg.scan.connect_timeout_ms,
            cfg.scan.concurrency,
            cfg.nvr.record_paths.join(","),
            cfg.nvr.request_timeout_ms,
            cfg.nvr.include_https,
            cfg.scheduler.interval_seconds,
            cfg.b2.key_id,
            cfg.b2.application_key,
            cfg.b2.bucket_id,
            cfg.b2.file_prefix,
            cfg.b2.max_upload_days,
            cfg.b2.upload_lag_days
        )
    }
}

fn resolve_env_token(value: &str) -> String {
    if value.starts_with("${") && value.ends_with('}') {
        let key = &value[2..value.len() - 1];
        return std::env::var(key).unwrap_or_default();
    }
    value.to_string()
}

fn parse_string(value: &str) -> String {
    let v = value.trim();
    if v.len() >= 2
        && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')))
    {
        v[1..v.len() - 1].to_string()
    } else {
        v.to_string()
    }
}

fn parse_optional_string(value: &str) -> Option<String> {
    let v = parse_string(value);
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

fn parse_list_string(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(parse_string)
        .filter(|v| !v.is_empty())
        .collect()
}

fn parse_list_u16(value: &str, line_num: usize) -> Result<Vec<u16>> {
    let mut out = Vec::new();
    for item in parse_list_string(value) {
        let port = item.parse::<u16>().map_err(|e| {
            AppError::ConfigParse(format!("line {line_num}: invalid port `{item}`: {e}"))
        })?;
        out.push(port);
    }
    Ok(out)
}

fn parse_u64(value: &str, line_num: usize, key: &str) -> Result<u64> {
    parse_string(value)
        .parse::<u64>()
        .map_err(|e| AppError::ConfigParse(format!("line {line_num}: invalid {key}: {e}")))
}

fn parse_u32(value: &str, line_num: usize, key: &str) -> Result<u32> {
    parse_string(value)
        .parse::<u32>()
        .map_err(|e| AppError::ConfigParse(format!("line {line_num}: invalid {key}: {e}")))
}

fn parse_usize(value: &str, line_num: usize, key: &str) -> Result<usize> {
    parse_string(value)
        .parse::<usize>()
        .map_err(|e| AppError::ConfigParse(format!("line {line_num}: invalid {key}: {e}")))
}

fn parse_bool(value: &str, line_num: usize, key: &str) -> Result<bool> {
    match parse_string(value).to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => Err(AppError::ConfigParse(format!(
            "line {line_num}: invalid {key} boolean `{other}`"
        ))),
    }
}

fn join_u16(items: &[u16]) -> String {
    items
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",")
}
