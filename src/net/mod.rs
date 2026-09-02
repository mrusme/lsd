mod events;
pub mod resolve;

pub use events::{Event, Events, SETTLE};

use crate::settings::{Identity, Presence};
use anyhow::Context;
use localsend::discovery::{
    DEFAULT_DISCOVERY_TIMEOUT, DeviceIdentity, DiscoveryConfig, DiscoveryEvent, DiscoveryHandle,
    StatefulDevice,
};
use localsend::http::client::ClientError;
use localsend::http::server::v2::ServerEventV2;
use localsend::http::server::{ServerConfigV2, ServerHandle, start_with_port};
use localsend::model::discovery::ProtocolType;
use localsend::multicast::{DEFAULT_MULTICAST_GROUP_V6, DEFAULT_PORT};
use localsend::util::interface::{InterfaceFilter, local_interface_addresses};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

const SERVER_CHANNEL_SIZE: usize = 128;
const DISCOVERY_CHANNEL_SIZE: usize = 128;
const SCAN_GRACE: Duration = Duration::from_secs(1);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

pub struct Node {
    identity: Identity,
    presence: Presence,
    server: ServerHandle,
    discovery: Arc<DiscoveryHandle>,
    server_stop_tx: oneshot::Sender<()>,
    discovery_stop_tx: oneshot::Sender<()>,
}

impl Node {
    pub async fn start(
        identity: Identity,
        mut presence: Presence,
        multicast: Ipv4Addr,
    ) -> anyhow::Result<(Self, Events)> {
        let (server_tx, server_rx) = mpsc::channel::<ServerEventV2>(SERVER_CHANNEL_SIZE);
        let (server_stop_tx, server_stop_rx) = oneshot::channel::<()>();
        let server = start_with_port(
            presence.port,
            Some(identity.tls_config()),
            presence.client_info(),
            None,
            Some(ServerConfigV2 {
                pin: None,
                verify_checksums: true,
                event_tx: server_tx,
            }),
            None,
            server_stop_rx,
        )
        .await
        .with_context(|| {
            format!(
                "Could not start the HTTP server on port {} (use --port 0 to take a free one)",
                presence.port
            )
        })?;

        presence.port = server.port();

        let (discovery_tx, discovery_rx) = mpsc::channel::<DiscoveryEvent>(DISCOVERY_CHANNEL_SIZE);
        let (discovery_stop_tx, discovery_stop_rx) = oneshot::channel::<()>();
        let discovery = Arc::new(
            localsend::discovery::start(
                DiscoveryConfig {
                    group: multicast,
                    group_v6: Some(DEFAULT_MULTICAST_GROUP_V6),
                    port: DEFAULT_PORT,
                    interface_filter: InterfaceFilter::default(),
                    device: presence.multicast_device(),
                    identity: DeviceIdentity {
                        cert_pem: identity.cert_pem.clone(),
                        private_key_pem: identity.key_pem.clone(),
                    },
                    timeout: DEFAULT_DISCOVERY_TIMEOUT,
                    event_tx: Some(discovery_tx),
                },
                discovery_stop_rx,
            )
            .await,
        );

        if let Some(error) = discovery.multicast_error() {
            tracing::warn!(
                "Multicast is unavailable on {multicast}:{DEFAULT_PORT}, \
                 discovery falls back to scanning the local subnets: {error:#}"
            );
        }

        let node = Self {
            identity,
            presence,
            server,
            discovery,
            server_stop_tx,
            discovery_stop_tx,
        };
        let events = Events::new(
            server_rx,
            discovery_rx,
            node.discovery.clone(),
            node.identity.fingerprint.clone(),
        );
        Ok((node, events))
    }

    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    pub const fn presence(&self) -> &Presence {
        &self.presence
    }

    pub fn devices(&self) -> Vec<StatefulDevice> {
        self.discovery.devices()
    }

    fn device_by_fingerprint(&self, fingerprint: &str) -> Option<StatefulDevice> {
        self.discovery.device_by_fingerprint(fingerprint)
    }

    async fn probe(&self, host: &str, port: u16) -> Result<Option<StatefulDevice>, ClientError> {
        self.discovery
            .discover(host, port, ProtocolType::Https)
            .await
    }

    pub async fn discover(&self) -> Result<(), ClientError> {
        let interface_ips =
            local_interface_addresses(&InterfaceFilter::default()).unwrap_or_default();
        self.discovery
            .discover_staged(
                Vec::new(),
                interface_ips,
                DEFAULT_PORT,
                ProtocolType::Https,
                SCAN_GRACE,
            )
            .await
    }

    pub fn announce(&self) {
        let discovery = self.discovery.clone();
        tokio::spawn(async move {
            discovery.announce().await;
        });
    }

    pub async fn shutdown(self) {
        let _ = self.server_stop_tx.send(());
        let _ = self.discovery_stop_tx.send(());
        let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, self.server.wait_stopped()).await;
        let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, self.discovery.wait_stopped()).await;
    }
}
