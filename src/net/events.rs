use localsend::discovery::{
    DeviceChannel, DiscoveredDevice, DiscoveryEvent, DiscoveryHandle, HttpChannel,
};
use localsend::http::dto_v2::RegisterDtoV2;
use localsend::http::server::common::save::FileUploadTarget;
use localsend::http::server::v2::{PrepareUploadDecisionV2, ServerEventV2, SessionEndReasonV2};
use localsend::model::transfer::FileDto;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

pub const SETTLE: Duration = Duration::from_millis(500);

pub enum Event {
    PrepareUpload {
        session_id: String,
        host: String,
        info: RegisterDtoV2,
        files: HashMap<String, FileDto>,
        decision_tx: oneshot::Sender<PrepareUploadDecisionV2>,
    },
    FileUpload {
        session_id: String,
        file_id: String,
        file: FileDto,
        target_tx: oneshot::Sender<FileUploadTarget>,
    },
    SessionEnd {
        session_id: String,
        reason: SessionEndReasonV2,
    },
    CancelReceived {
        session_id: String,
        host: String,
    },
    Discovered(DiscoveredDevice),
    ListenerFailed(String),
}

enum Incoming {
    Server(Option<ServerEventV2>),
    Discovery(Option<DiscoveryEvent>),
}

pub struct Events {
    server_rx: mpsc::Receiver<ServerEventV2>,
    discovery_rx: mpsc::Receiver<DiscoveryEvent>,
    discovery: Arc<DiscoveryHandle>,
    fingerprint: String,
    server_closed: bool,
    discovery_closed: bool,
}

impl Events {
    pub const fn new(
        server_rx: mpsc::Receiver<ServerEventV2>,
        discovery_rx: mpsc::Receiver<DiscoveryEvent>,
        discovery: Arc<DiscoveryHandle>,
        fingerprint: String,
    ) -> Self {
        Self {
            server_rx,
            discovery_rx,
            discovery,
            fingerprint,
            server_closed: false,
            discovery_closed: false,
        }
    }

    pub async fn next(&mut self) -> Option<Event> {
        loop {
            if self.server_closed && self.discovery_closed {
                return None;
            }
            let incoming = tokio::select! {
                event = self.server_rx.recv(), if !self.server_closed => Incoming::Server(event),
                event = self.discovery_rx.recv(), if !self.discovery_closed => {
                    Incoming::Discovery(event)
                }
            };
            let event = match incoming {
                Incoming::Server(None) => {
                    self.server_closed = true;
                    continue;
                }
                Incoming::Discovery(None) => {
                    self.discovery_closed = true;
                    continue;
                }
                Incoming::Server(Some(event)) => self.server_event(event).await,
                Incoming::Discovery(Some(event)) => Self::discovery_event(event),
            };
            if event.is_some() {
                return event;
            }
        }
    }

    pub async fn drain(&mut self) {
        while self.next().await.is_some() {}
    }

    async fn server_event(&self, event: ServerEventV2) -> Option<Event> {
        match event {
            ServerEventV2::Register { ip, info } => {
                self.confirm(ip.to_string(), info).await;
                None
            }
            ServerEventV2::PrepareUpload {
                session_id,
                ip,
                mut info,
                cert_fingerprint,
                files,
                decision_tx,
            } => {
                if let Some(verified) = &cert_fingerprint {
                    info.fingerprint.clone_from(verified);
                }
                let host = ip.to_string();
                self.confirm(host.clone(), info.clone()).await;
                Some(Event::PrepareUpload {
                    session_id,
                    host,
                    info,
                    files,
                    decision_tx,
                })
            }
            ServerEventV2::FileUpload {
                session_id,
                file_id,
                file,
                target_tx,
            } => Some(Event::FileUpload {
                session_id,
                file_id,
                file,
                target_tx,
            }),
            ServerEventV2::SessionEnd { session_id, reason } => {
                Some(Event::SessionEnd { session_id, reason })
            }
            ServerEventV2::CancelReceived { ip, session_id } => Some(Event::CancelReceived {
                session_id,
                host: ip.to_string(),
            }),
            ServerEventV2::ListenerFailed { error } => Some(Event::ListenerFailed(error)),
            ServerEventV2::PrepareUploadAborted { .. } => None,
        }
    }

    fn discovery_event(event: DiscoveryEvent) -> Option<Event> {
        match event {
            DiscoveryEvent::Discovered { device } | DiscoveryEvent::Updated { device } => {
                Some(Event::Discovered(device))
            }
            DiscoveryEvent::MulticastFailed => {
                tracing::warn!("Multicast stopped working, discovery now depends on scanning");
                None
            }
        }
    }

    async fn confirm(&self, host: String, info: RegisterDtoV2) {
        if info.fingerprint == self.fingerprint {
            return;
        }
        self.discovery
            .add_device(DiscoveredDevice {
                alias: info.alias,
                version: info.version,
                device_model: info.device_model,
                device_type: info.device_type,
                fingerprint: info.fingerprint,
                channel: DeviceChannel::Http(HttpChannel {
                    host,
                    port: info.port,
                    protocol: info.protocol,
                }),
                download: info.download,
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use localsend::discovery::{DeviceIdentity, DiscoveryConfig};
    use localsend::http::server::PeerIp;
    use localsend::model::discovery::{DeviceType, PROTOCOL_VERSION_V2, ProtocolType};
    use localsend::multicast::{DEFAULT_MULTICAST_GROUP, MulticastDevice};
    use std::net::{IpAddr, Ipv4Addr};

    const OWN: &str = "OWN-FINGERPRINT";

    static CERT: std::sync::LazyLock<(String, String)> = std::sync::LazyLock::new(|| {
        let cert = localsend::crypto::cert::generate_self_signed().unwrap();
        (cert.certificate_pem, cert.private_key_pem)
    });

    async fn discovery() -> (Arc<DiscoveryHandle>, oneshot::Sender<()>) {
        let (cert_pem, private_key_pem) = CERT.clone();
        let (stop_tx, stop_rx) = oneshot::channel();
        let handle = localsend::discovery::start(
            DiscoveryConfig {
                group: DEFAULT_MULTICAST_GROUP,
                group_v6: None,
                port: 0,
                interface_filter: localsend::util::interface::InterfaceFilter::default(),
                device: MulticastDevice {
                    alias: "TEST".to_string(),
                    version: PROTOCOL_VERSION_V2.to_string(),
                    device_model: None,
                    device_type: Some(DeviceType::Headless),
                    fingerprint: OWN.to_string(),
                    port: 1,
                    protocol: ProtocolType::Https,
                    download: false,
                },
                identity: DeviceIdentity {
                    cert_pem,
                    private_key_pem,
                },
                timeout: Duration::from_millis(50),
                event_tx: None,
            },
            stop_rx,
        )
        .await;
        (Arc::new(handle), stop_tx)
    }

    fn register(fingerprint: &str) -> ServerEventV2 {
        ServerEventV2::Register {
            ip: PeerIp {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)),
                scope_id: None,
            },
            info: RegisterDtoV2 {
                alias: "PEER".to_string(),
                version: PROTOCOL_VERSION_V2.to_string(),
                device_model: None,
                device_type: Some(DeviceType::Headless),
                fingerprint: fingerprint.to_string(),
                port: 53317,
                protocol: ProtocolType::Https,
                download: false,
            },
        }
    }

    #[tokio::test]
    async fn closed_channels_end_the_stream_instead_of_panicking() {
        let (discovery, _stop) = discovery().await;
        let (server_tx, server_rx) = mpsc::channel(4);
        let (discovery_tx, discovery_rx) = mpsc::channel(4);
        let mut events = Events::new(server_rx, discovery_rx, discovery, OWN.to_string());

        drop(server_tx);
        drop(discovery_tx);

        assert!(events.next().await.is_none());
        assert!(events.next().await.is_none());
    }

    #[tokio::test]
    async fn one_closed_channel_does_not_end_the_stream() {
        let (discovery, _stop) = discovery().await;
        let (server_tx, server_rx) = mpsc::channel(4);
        let (discovery_tx, discovery_rx) = mpsc::channel(4);
        let mut events = Events::new(server_rx, discovery_rx, discovery, OWN.to_string());

        drop(discovery_tx);
        server_tx
            .send(ServerEventV2::ListenerFailed {
                error: "boom".to_string(),
            })
            .await
            .unwrap();
        drop(server_tx);

        assert!(matches!(
            events.next().await,
            Some(Event::ListenerFailed(error)) if error == "boom"
        ));
        assert!(events.next().await.is_none());
    }

    #[tokio::test]
    async fn a_registration_is_in_the_store_before_next_returns() {
        let (discovery, _stop) = discovery().await;
        let (server_tx, server_rx) = mpsc::channel(4);
        let (discovery_tx, discovery_rx) = mpsc::channel(4);
        let mut events = Events::new(server_rx, discovery_rx, discovery.clone(), OWN.to_string());

        server_tx.send(register("PEER-FINGERPRINT")).await.unwrap();
        drop(server_tx);
        drop(discovery_tx);

        assert!(discovery.devices().is_empty());
        assert!(events.next().await.is_none());
        assert_eq!(discovery.devices().len(), 1);
        assert_eq!(discovery.devices()[0].device.alias, "PEER");
    }

    #[tokio::test]
    async fn our_own_registration_is_ignored() {
        let (discovery, _stop) = discovery().await;
        let (server_tx, server_rx) = mpsc::channel(4);
        let (discovery_tx, discovery_rx) = mpsc::channel(4);
        let mut events = Events::new(server_rx, discovery_rx, discovery.clone(), OWN.to_string());

        server_tx.send(register(OWN)).await.unwrap();
        drop(server_tx);
        drop(discovery_tx);

        assert!(events.next().await.is_none());
        assert!(discovery.devices().is_empty());
    }
}
