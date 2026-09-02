use localsend::discovery::{DiscoveredDevice, HttpChannel};
use localsend::http::dto_v2::RegisterDtoV2;
use serde::Serialize;

#[derive(Serialize)]
pub struct Device {
    #[serde(flatten)]
    info: RegisterDtoV2,
    host: String,
}

impl Device {
    pub fn new(device: &DiscoveredDevice, channel: &HttpChannel) -> Self {
        Self {
            info: RegisterDtoV2 {
                alias: device.alias.clone(),
                version: device.version.clone(),
                device_model: device.device_model.clone(),
                device_type: device.device_type.clone(),
                fingerprint: device.fingerprint.clone(),
                port: channel.port,
                protocol: channel.protocol,
                download: device.download,
            },
            host: channel.host.clone(),
        }
    }

    pub const fn from_register(info: RegisterDtoV2, host: String) -> Self {
        Self { info, host }
    }

    pub fn alias(&self) -> &str {
        &self.info.alias
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn fingerprint(&self) -> &str {
        &self.info.fingerprint
    }
}
