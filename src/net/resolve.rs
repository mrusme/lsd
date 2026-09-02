use super::{Event, Events, Node, SETTLE};
use anyhow::Context;
use localsend::discovery::{DiscoveredDevice, HttpChannel, StatefulDevice};
use localsend::multicast::DEFAULT_PORT;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

pub struct Target {
    pub device: DiscoveredDevice,
    pub channel: HttpChannel,
}

pub fn all(node: &Node) -> Vec<Target> {
    node.devices()
        .into_iter()
        .filter_map(|stored| {
            best_channel(&stored).map(|channel| Target {
                device: stored.device,
                channel,
            })
        })
        .collect()
}

pub async fn one(
    node: &Node,
    events: &mut Events,
    selector: &str,
    timeout: Duration,
) -> anyhow::Result<Target> {
    let stored = locate(node, events, selector, timeout).await?;
    let channel = best_channel(&stored)
        .with_context(|| format!("{}: No dialable address", stored.device.alias))?;
    Ok(Target {
        device: stored.device,
        channel,
    })
}

async fn locate(
    node: &Node,
    events: &mut Events,
    selector: &str,
    timeout: Duration,
) -> anyhow::Result<StatefulDevice> {
    if let Some((host, port)) = parse_address(selector) {
        return node
            .probe(&host, port)
            .await
            .with_context(|| format!("Could not reach {host}:{port}"))?
            .with_context(|| format!("{host}:{port} is this device"));
    }
    tokio::time::timeout(timeout, search(node, events, selector))
        .await
        .ok()
        .flatten()
        .or_else(|| find(node, selector))
        .with_context(|| format!("No device matching \"{selector}\""))
}

async fn search(node: &Node, events: &mut Events, selector: &str) -> Option<StatefulDevice> {
    let found = tokio::select! {
        _ = node.discover() => None,
        device = watch(node, events, selector) => device,
    };
    if found.is_some() {
        return found;
    }
    tokio::time::timeout(SETTLE, watch(node, events, selector))
        .await
        .ok()
        .flatten()
}

async fn watch(node: &Node, events: &mut Events, selector: &str) -> Option<StatefulDevice> {
    while let Some(event) = events.next().await {
        let Event::Discovered(device) = event else {
            continue;
        };
        if !matches(selector, &device) {
            continue;
        }
        if let Some(stored) = node.device_by_fingerprint(&device.fingerprint) {
            return Some(stored);
        }
    }
    None
}

fn find(node: &Node, selector: &str) -> Option<StatefulDevice> {
    node.devices()
        .into_iter()
        .find(|stored| matches(selector, &stored.device))
}

fn matches(selector: &str, device: &DiscoveredDevice) -> bool {
    device.alias.eq_ignore_ascii_case(selector)
        || device.fingerprint.eq_ignore_ascii_case(selector)
        || device.http().is_some_and(|http| http.host == selector)
}

fn parse_address(selector: &str) -> Option<(String, u16)> {
    if let Ok(address) = selector.parse::<SocketAddr>() {
        return Some((address.ip().to_string(), address.port()));
    }
    selector
        .parse::<IpAddr>()
        .ok()
        .map(|ip| (ip.to_string(), DEFAULT_PORT))
}

fn best_channel(stored: &StatefulDevice) -> Option<HttpChannel> {
    stored
        .get_best_channel()
        .and_then(|channel| channel.http())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use localsend::discovery::DeviceChannel;
    use localsend::model::discovery::ProtocolType;

    fn device(alias: &str, fingerprint: &str, host: &str) -> DiscoveredDevice {
        DiscoveredDevice {
            alias: alias.to_string(),
            version: "2.2".to_string(),
            device_model: None,
            device_type: None,
            fingerprint: fingerprint.to_string(),
            channel: DeviceChannel::Http(HttpChannel {
                host: host.to_string(),
                port: 53317,
                protocol: ProtocolType::Https,
            }),
            download: false,
        }
    }

    #[test]
    fn a_bare_ipv4_address_uses_the_default_port() {
        assert_eq!(
            parse_address("10.0.0.20"),
            Some(("10.0.0.20".to_string(), DEFAULT_PORT))
        );
    }

    #[test]
    fn an_ipv4_address_with_a_port_keeps_it() {
        assert_eq!(
            parse_address("10.0.0.20:9000"),
            Some(("10.0.0.20".to_string(), 9000))
        );
    }

    #[test]
    fn ipv6_addresses_are_accepted_bare_and_bracketed() {
        assert_eq!(
            parse_address("::1"),
            Some(("::1".to_string(), DEFAULT_PORT))
        );
        assert_eq!(parse_address("[::1]:9000"), Some(("::1".to_string(), 9000)));
    }

    #[test]
    fn anything_that_is_not_an_address_is_left_to_discovery() {
        assert_eq!(parse_address("Smart Raspberry"), None);
        assert_eq!(parse_address(""), None);
        assert_eq!(parse_address("10.0.0.20:"), None);
        assert_eq!(parse_address("10.0.0.20:70000"), None);
        assert_eq!(parse_address("raspberry.local"), None);
    }

    #[test]
    fn a_selector_matches_the_alias_ignoring_case() {
        let found = device("Smart Raspberry", "AABB", "10.0.0.20");
        assert!(matches("Smart Raspberry", &found));
        assert!(matches("smart raspberry", &found));
        assert!(!matches("Smart", &found));
    }

    #[test]
    fn a_selector_matches_the_fingerprint_ignoring_case() {
        let found = device("Smart Raspberry", "AABBCC", "10.0.0.20");
        assert!(matches("AABBCC", &found));
        assert!(matches("aabbcc", &found));
    }

    #[test]
    fn a_selector_matches_the_host() {
        let found = device("Smart Raspberry", "AABB", "10.0.0.20");
        assert!(matches("10.0.0.20", &found));
        assert!(!matches("10.0.0.21", &found));
    }
}
