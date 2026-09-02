use crate::net::{Events, Node, SETTLE, resolve};
use crate::output::{Device, DiscoverReport};
use anyhow::Context;
use localsend::http::client::ClientError;
use std::time::Duration;

pub async fn run(
    node: &Node,
    events: &mut Events,
    timeout: Duration,
) -> anyhow::Result<DiscoverReport> {
    if let Ok(result) = tokio::time::timeout(timeout, collect(node, events)).await {
        result.context("Discovery failed")?;
    }
    Ok(DiscoverReport {
        ok: true,
        devices: devices(node),
    })
}

async fn collect(node: &Node, events: &mut Events) -> Result<(), ClientError> {
    tokio::select! {
        result = node.discover() => result?,
        () = events.drain() => {}
    }
    let _ = tokio::time::timeout(SETTLE, events.drain()).await;
    Ok(())
}

fn devices(node: &Node) -> Vec<Device> {
    resolve::all(node)
        .iter()
        .map(|target| Device::new(&target.device, &target.channel))
        .collect()
}
