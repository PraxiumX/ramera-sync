use futures::stream::{self, StreamExt};

use crate::config::NvrConfig;
use crate::providers;
use crate::types::{DeviceRecord, NvrDevice};

pub async fn collect_records(
    client: &reqwest::Client,
    devices: &[NvrDevice],
    cfg: &NvrConfig,
) -> Vec<DeviceRecord> {
    stream::iter(devices)
        .map(|device| async move { providers::collect_records_for_device(client, device, cfg).await })
        .buffer_unordered(32)
        .collect::<Vec<Vec<DeviceRecord>>>()
        .await
        .into_iter()
        .flatten()
        .collect()
}
