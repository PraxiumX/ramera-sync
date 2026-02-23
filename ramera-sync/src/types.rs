use std::net::IpAddr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NvrDevice {
    pub ip: IpAddr,
    pub open_ports: Vec<u16>,
    pub provider: String,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub fingerprint_url: Option<String>,
    pub fingerprint_preview: Option<String>,
    pub detected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub ip: String,
    pub provider: String,
    pub path: String,
    pub status: u16,
    pub fetched_at: DateTime<Utc>,
    pub body_preview: String,
    pub body: String,
}
