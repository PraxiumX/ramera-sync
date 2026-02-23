use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::stream::{self, StreamExt};
use ipnet::Ipv4Net;
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio::time::timeout;

use crate::config::{NvrConfig, ScanConfig};
use crate::error::{AppError, Result};
use crate::providers;
use crate::types::NvrDevice;

pub async fn discover_devices(scan: &ScanConfig, nvr: &NvrConfig) -> Result<Vec<NvrDevice>> {
    let net: Ipv4Net = scan
        .cidr
        .parse()
        .map_err(|_| AppError::InvalidCidr(scan.cidr.clone()))?;

    let timeout_ms = Duration::from_millis(scan.connect_timeout_ms);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(nvr.request_timeout_ms))
        .danger_accept_invalid_certs(true)
        .build()?;

    let semaphore = Arc::new(Semaphore::new(scan.concurrency.max(1)));
    let hosts: Vec<_> = net.hosts().collect();

    let results = stream::iter(hosts)
        .map(|ip| {
            let semaphore = Arc::clone(&semaphore);
            let client = client.clone();
            let ports = scan.ports.clone();
            let nvr = nvr.clone();
            async move {
                let _permit = semaphore.acquire_owned().await.ok()?;
                scan_single_host(ip.into(), &ports, timeout_ms, &client, &nvr).await
            }
        })
        .buffer_unordered(scan.concurrency.max(1))
        .collect::<Vec<_>>()
        .await;

    Ok(results.into_iter().flatten().collect())
}

async fn scan_single_host(
    ip: IpAddr,
    ports: &[u16],
    connect_timeout: Duration,
    client: &reqwest::Client,
    nvr: &NvrConfig,
) -> Option<NvrDevice> {
    let mut open_ports = Vec::new();
    for port in ports {
        if is_port_open(ip, *port, connect_timeout).await {
            open_ports.push(*port);
        }
    }

    if open_ports.is_empty() {
        return None;
    }

    let fingerprint = providers::fingerprint_device(client, ip, &open_ports, nvr).await;
    let likely_by_ports =
        open_ports.contains(&554) || open_ports.contains(&8000) || open_ports.contains(&37777);

    if !fingerprint.is_nvr && !likely_by_ports {
        return None;
    }

    Some(NvrDevice {
        ip,
        open_ports,
        provider: fingerprint.provider.to_string(),
        vendor: fingerprint.vendor,
        model: fingerprint.model,
        serial: fingerprint.serial,
        fingerprint_url: fingerprint.source_url,
        fingerprint_preview: fingerprint.preview,
        detected_at: Utc::now(),
    })
}

async fn is_port_open(ip: IpAddr, port: u16, connect_timeout: Duration) -> bool {
    let addr = SocketAddr::new(ip, port);
    matches!(
        timeout(connect_timeout, TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}
