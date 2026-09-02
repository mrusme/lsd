use anyhow::Context;
use localsend::crypto::cert::fingerprint_from_cert_der;
use localsend::http::dto_v2::RegisterDtoV2;
use localsend::http::server::TlsConfig;
use localsend::http::state::ClientInfo;
use localsend::model::discovery::{DeviceType, PROTOCOL_VERSION_V2, ProtocolType};
use localsend::multicast::MulticastDevice;
use std::io::Write;
use std::path::Path;

const DEVICE_MODEL: &str = "CLI";

pub struct Identity {
    pub cert_pem: String,
    pub key_pem: String,
    pub fingerprint: String,
}

impl Identity {
    pub fn load_or_generate(dir: &Path) -> anyhow::Result<Self> {
        let path = dir.join("identity.pem");
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_pem(&text).with_context(|| {
                format!(
                    "Invalid identity file: {} (delete it to generate a new identity; \
                     other devices will then see this device as unpaired)",
                    path.display()
                )
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let identity = Self::generate()?;
                identity
                    .save(&path)
                    .with_context(|| format!("Could not save {}", path.display()))?;
                Ok(identity)
            }
            Err(err) => Err(err).context(format!("Could not read {}", path.display())),
        }
    }

    fn from_pem(text: &str) -> anyhow::Result<Self> {
        let blocks = pem::parse_many(text)?;
        let cert = blocks
            .iter()
            .find(|block| block.tag() == "CERTIFICATE")
            .context("missing CERTIFICATE block")?;
        let key = blocks
            .iter()
            .find(|block| block.tag().ends_with("PRIVATE KEY"))
            .context("missing PRIVATE KEY block")?;
        let key_pem = pem::encode(key);
        rcgen::KeyPair::from_pem(&key_pem).context("unusable private key")?;
        Ok(Self {
            fingerprint: fingerprint_from_cert_der(cert.contents()),
            cert_pem: pem::encode(cert),
            key_pem,
        })
    }

    fn generate() -> anyhow::Result<Self> {
        let cert = localsend::crypto::cert::generate_self_signed()?;
        Ok(Self {
            fingerprint: cert.fingerprint,
            cert_pem: cert.certificate_pem,
            key_pem: cert.private_key_pem,
        })
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(path)?
            .write_all(format!("{}{}", self.cert_pem, self.key_pem).as_bytes())
    }

    pub fn tls_config(&self) -> TlsConfig {
        TlsConfig {
            cert: self.cert_pem.clone(),
            private_key: self.key_pem.clone(),
        }
    }
}

pub struct Presence {
    pub alias: String,
    pub port: u16,
    pub fingerprint: String,
}

impl Presence {
    pub fn new(identity: &Identity, alias: String, port: u16) -> Self {
        Self {
            alias,
            port,
            fingerprint: identity.fingerprint.clone(),
        }
    }

    pub fn client_info(&self) -> ClientInfo {
        ClientInfo {
            alias: self.alias.clone(),
            version: PROTOCOL_VERSION_V2.to_string(),
            device_model: Some(DEVICE_MODEL.to_string()),
            device_type: Some(DeviceType::Headless),
            token: self.fingerprint.clone(),
        }
    }

    pub fn register_dto(&self) -> RegisterDtoV2 {
        RegisterDtoV2 {
            alias: self.alias.clone(),
            version: PROTOCOL_VERSION_V2.to_string(),
            device_model: Some(DEVICE_MODEL.to_string()),
            device_type: Some(DeviceType::Headless),
            fingerprint: self.fingerprint.clone(),
            port: self.port,
            protocol: ProtocolType::Https,
            download: false,
        }
    }

    pub fn multicast_device(&self) -> MulticastDevice {
        let dto = self.register_dto();
        MulticastDevice {
            alias: dto.alias,
            version: dto.version,
            device_model: dto.device_model,
            device_type: dto.device_type,
            fingerprint: dto.fingerprint,
            port: dto.port,
            protocol: dto.protocol,
            download: dto.download,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static GENERATED: std::sync::LazyLock<(String, String)> = std::sync::LazyLock::new(|| {
        let identity = Identity::generate().unwrap();
        (identity.cert_pem, identity.key_pem)
    });

    fn failure(result: anyhow::Result<Identity>) -> String {
        format!("{:#}", result.err().expect("expected an error"))
    }

    fn pem(cert: bool, key: bool) -> String {
        let (cert_pem, key_pem) = GENERATED.clone();
        let mut text = String::new();
        if cert {
            text.push_str(&cert_pem);
        }
        if key {
            text.push_str(&key_pem);
        }
        text
    }

    #[test]
    fn a_generated_identity_is_saved_privately_and_reloads_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let first = Identity::load_or_generate(dir.path()).unwrap();
        assert!(!first.fingerprint.is_empty());

        let path = dir.path().join("identity.pem");
        assert!(path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let second = Identity::load_or_generate(dir.path()).unwrap();
        let third = Identity::load_or_generate(dir.path()).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(second.fingerprint, third.fingerprint);
        assert_eq!(second.cert_pem, third.cert_pem);
        assert_eq!(second.key_pem, third.key_pem);
    }

    #[test]
    fn a_pem_without_a_certificate_is_rejected() {
        let error = failure(Identity::from_pem(&pem(false, true)));
        assert!(error.contains("missing CERTIFICATE block"));
    }

    #[test]
    fn a_pem_without_a_private_key_is_rejected() {
        let error = failure(Identity::from_pem(&pem(true, false)));
        assert!(error.contains("missing PRIVATE KEY block"));
    }

    #[test]
    fn a_pem_with_an_unusable_key_is_rejected() {
        let text = format!(
            "{}-----BEGIN PRIVATE KEY-----\nQUJD\n-----END PRIVATE KEY-----\n",
            pem(true, false)
        );
        let error = failure(Identity::from_pem(&text));
        assert!(error.contains("unusable private key"));
    }

    #[test]
    fn a_corrupt_identity_file_points_at_the_file_and_the_way_out() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("identity.pem"), pem(false, true)).unwrap();
        let error = failure(Identity::load_or_generate(dir.path()));
        assert!(error.contains("Invalid identity file"));
        assert!(error.contains("delete it to generate a new identity"));
        assert!(error.contains("missing CERTIFICATE block"));
    }

    #[test]
    fn presence_announces_the_same_device_over_every_channel() {
        let (cert_pem, key_pem) = GENERATED.clone();
        let identity = Identity {
            cert_pem,
            key_pem,
            fingerprint: "FINGERPRINT".to_string(),
        };
        let presence = Presence::new(&identity, "Smart Raspberry".to_string(), 1234);

        let register = presence.register_dto();
        let multicast = presence.multicast_device();
        let client = presence.client_info();

        assert_eq!(register.alias, "Smart Raspberry");
        assert_eq!(register.port, 1234);
        assert_eq!(register.fingerprint, "FINGERPRINT");
        assert_eq!(register.device_model.as_deref(), Some(DEVICE_MODEL));
        assert_eq!(register.protocol, ProtocolType::Https);
        assert!(!register.download);

        assert_eq!(multicast.alias, register.alias);
        assert_eq!(multicast.port, register.port);
        assert_eq!(multicast.fingerprint, register.fingerprint);
        assert_eq!(multicast.version, register.version);
        assert_eq!(multicast.device_model, register.device_model);
        assert_eq!(multicast.download, register.download);

        assert_eq!(client.alias, register.alias);
        assert_eq!(client.version, register.version);
        assert_eq!(client.token, register.fingerprint);
        assert_eq!(client.device_model, register.device_model);
    }

    #[test]
    fn the_bound_port_is_updated_without_touching_the_identity() {
        let (cert_pem, key_pem) = GENERATED.clone();
        let identity = Identity {
            cert_pem,
            key_pem,
            fingerprint: "FINGERPRINT".to_string(),
        };
        let mut presence = Presence::new(&identity, "RX".to_string(), 0);
        presence.port = 53400;
        assert_eq!(presence.register_dto().port, 53400);
        assert_eq!(presence.fingerprint, identity.fingerprint);
    }
}
