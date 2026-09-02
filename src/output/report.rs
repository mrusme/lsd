use super::device::Device;
use super::{Category, block};
use crate::util;
use serde::{Serialize, Serializer};
use std::fmt::Write;
use std::path::PathBuf;
use std::time::Duration;

fn as_seconds<S: Serializer>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_f64(util::seconds(*duration))
}

#[derive(Serialize)]
pub struct DiscoverReport {
    pub ok: bool,
    pub devices: Vec<Device>,
}

impl DiscoverReport {
    fn text(&self) -> String {
        if self.devices.is_empty() {
            return block(Category::Discovery, "No devices found");
        }
        self.devices
            .iter()
            .enumerate()
            .map(|(index, device)| {
                block(
                    Category::Discovery,
                    &format!("[{}] {} ({})", index + 1, device.alias(), device.host()),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Serialize)]
pub struct SentFile {
    pub name: String,
    pub size: u64,
}

#[derive(Serialize)]
pub struct FailedFile {
    pub name: String,
    pub error: String,
}

#[derive(Serialize)]
pub struct ReceivedFile {
    pub name: String,
    pub size: u64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendReport {
    pub ok: bool,
    pub device: Device,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,

    pub files: Vec<SentFile>,
    pub declined: Vec<String>,
    pub bytes: u64,

    #[serde(serialize_with = "as_seconds")]
    pub seconds: Duration,
}

impl SendReport {
    fn text(&self) -> String {
        if self.text.is_some() {
            return block(
                Category::Send,
                &format!(
                    "{}: Message sent (took {})",
                    self.device.alias(),
                    util::format_duration(self.seconds),
                ),
            );
        }
        let mut line = format!(
            "{}: Sent {} ({}, took {})",
            self.device.alias(),
            util::count_files(self.files.len()),
            util::format_bytes(self.bytes),
            util::format_duration(self.seconds),
        );
        if !self.declined.is_empty() {
            let _ = write!(line, ", {} declined", self.declined.len());
        }
        block(Category::Send, &line)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiveReport {
    pub ok: bool,
    pub device: Device,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,

    pub files: Vec<ReceivedFile>,
    pub failed: Vec<FailedFile>,
    pub bytes: u64,

    #[serde(serialize_with = "as_seconds")]
    pub seconds: Duration,
}

impl ReceiveReport {
    fn text(&self) -> String {
        if let Some(text) = &self.text {
            return block(
                Category::Receive,
                &format!("{}: Message received\n{text}", self.device.alias()),
            );
        }
        let mut line = format!(
            "{}: Received {} ({}, took {})",
            self.device.alias(),
            util::count_files(self.files.len()),
            util::format_bytes(self.bytes),
            util::format_duration(self.seconds),
        );
        if !self.failed.is_empty() {
            let _ = write!(line, ", {} failed", self.failed.len());
        }
        block(Category::Receive, &line)
    }
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum Report {
    Discover(DiscoverReport),
    Send(SendReport),
    Receive(ReceiveReport),
}

impl Report {
    pub const fn ok(&self) -> bool {
        match self {
            Self::Discover(report) => report.ok,
            Self::Send(report) => report.ok,
            Self::Receive(report) => report.ok,
        }
    }

    pub fn text(&self) -> String {
        match self {
            Self::Discover(report) => report.text(),
            Self::Send(report) => report.text(),
            Self::Receive(report) => report.text(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use localsend::http::dto_v2::RegisterDtoV2;
    use localsend::model::discovery::{DeviceType, ProtocolType};

    fn device(alias: &str, port: u16) -> Device {
        Device::from_register(
            RegisterDtoV2 {
                alias: alias.to_string(),
                version: "2.2".to_string(),
                device_model: Some("CLI".to_string()),
                device_type: Some(DeviceType::Headless),
                fingerprint: "FINGERPRINT".to_string(),
                port,
                protocol: ProtocolType::Https,
                download: false,
            },
            "127.0.0.1".to_string(),
        )
    }

    fn json(report: &Report) -> String {
        serde_json::to_string_pretty(report).unwrap()
    }

    #[test]
    fn discover_without_devices() {
        let report = Report::Discover(DiscoverReport {
            ok: true,
            devices: Vec::new(),
        });
        assert_eq!(json(&report), "{\n  \"ok\": true,\n  \"devices\": []\n}");
        assert_eq!(report.text(), "D No devices found");
        assert!(report.ok());
    }

    #[test]
    fn discover_numbers_devices_from_one() {
        let report = Report::Discover(DiscoverReport {
            ok: true,
            devices: vec![device("RXBOX", 53400), device("Juicy Lemon", 53317)],
        });
        assert_eq!(
            report.text(),
            "D [1] RXBOX (127.0.0.1)\nD [2] Juicy Lemon (127.0.0.1)"
        );
    }

    #[test]
    fn send_report_shape() {
        let report = Report::Send(SendReport {
            ok: true,
            device: device("RXBOX", 53410),
            session_id: Some("SESSION".to_string()),
            text: None,
            files: vec![
                SentFile {
                    name: "a.txt".to_string(),
                    size: 12,
                },
                SentFile {
                    name: "b.bin".to_string(),
                    size: 2_000_000,
                },
            ],
            declined: Vec::new(),
            bytes: 2_000_012,
            seconds: Duration::ZERO,
        });
        assert_eq!(
            json(&report),
            r#"{
  "ok": true,
  "device": {
    "alias": "RXBOX",
    "version": "2.2",
    "deviceModel": "CLI",
    "deviceType": "headless",
    "fingerprint": "FINGERPRINT",
    "port": 53410,
    "protocol": "https",
    "download": false,
    "host": "127.0.0.1"
  },
  "sessionId": "SESSION",
  "files": [
    {
      "name": "a.txt",
      "size": 12
    },
    {
      "name": "b.bin",
      "size": 2000000
    }
  ],
  "declined": [],
  "bytes": 2000012,
  "seconds": 0.0
}"#
        );
        assert_eq!(report.text(), "S RXBOX: Sent 2 files (2.0 MB, took 0s)");
    }

    #[test]
    fn receive_report_shape() {
        let report = Report::Receive(ReceiveReport {
            ok: true,
            device: device("TXBOX", 53411),
            text: None,
            files: vec![ReceivedFile {
                name: "a.txt".to_string(),
                size: 12,
                path: Some(PathBuf::from("/tmp/rxc/a.txt")),
            }],
            failed: Vec::new(),
            bytes: 12,
            seconds: Duration::ZERO,
        });
        assert_eq!(
            json(&report),
            r#"{
  "ok": true,
  "device": {
    "alias": "TXBOX",
    "version": "2.2",
    "deviceModel": "CLI",
    "deviceType": "headless",
    "fingerprint": "FINGERPRINT",
    "port": 53411,
    "protocol": "https",
    "download": false,
    "host": "127.0.0.1"
  },
  "files": [
    {
      "name": "a.txt",
      "size": 12,
      "path": "/tmp/rxc/a.txt"
    }
  ],
  "failed": [],
  "bytes": 12,
  "seconds": 0.0
}"#
        );
        assert_eq!(report.text(), "R TXBOX: Received 1 file (12 B, took 0s)");
    }

    #[test]
    fn receive_report_counts_failures() {
        let report = Report::Receive(ReceiveReport {
            ok: false,
            device: device("TXBOX", 53413),
            text: None,
            files: Vec::new(),
            failed: vec![FailedFile {
                name: "b.bin".to_string(),
                error: "Permission denied".to_string(),
            }],
            bytes: 0,
            seconds: Duration::ZERO,
        });
        assert!(!report.ok());
        assert_eq!(
            report.text(),
            "R TXBOX: Received 0 files (0 B, took 0s), 1 failed"
        );
    }

    #[test]
    fn a_sent_message_reports_itself_as_a_message() {
        let report = Report::Send(SendReport {
            ok: true,
            device: device("RXBOX", 53410),
            session_id: None,
            text: Some("Hello World".to_string()),
            files: Vec::new(),
            declined: Vec::new(),
            bytes: 0,
            seconds: Duration::ZERO,
        });
        assert!(report.ok());
        assert_eq!(report.text(), "S RXBOX: Message sent (took 0s)");
        let document = json(&report);
        assert!(document.contains("\"text\": \"Hello World\""));
        assert!(!document.contains("sessionId"));
    }

    #[test]
    fn a_partial_acceptance_is_not_ok_and_says_so() {
        let report = Report::Send(SendReport {
            ok: false,
            device: device("RXBOX", 53410),
            session_id: Some("SESSION".to_string()),
            text: None,
            files: vec![SentFile {
                name: "a.txt".to_string(),
                size: 12,
            }],
            declined: vec!["b.bin".to_string()],
            bytes: 12,
            seconds: Duration::ZERO,
        });
        assert!(!report.ok());
        assert_eq!(
            report.text(),
            "S RXBOX: Sent 1 file (12 B, took 0s), 1 declined"
        );
        assert!(json(&report).contains("\"declined\": [\n    \"b.bin\"\n  ]"));
    }

    #[test]
    fn receive_failures_carry_their_reason_into_json() {
        let report = Report::Receive(ReceiveReport {
            ok: false,
            device: device("TXBOX", 53413),
            text: None,
            files: Vec::new(),
            failed: vec![FailedFile {
                name: "a.txt".to_string(),
                error: "Permission denied".to_string(),
            }],
            bytes: 0,
            seconds: Duration::ZERO,
        });
        let document = json(&report);
        assert!(document.contains("\"name\": \"a.txt\""));
        assert!(document.contains("\"error\": \"Permission denied\""));
    }

    #[test]
    fn receive_report_renders_a_message() {
        let report = Report::Receive(ReceiveReport {
            ok: true,
            device: device("TXBOX", 53411),
            text: Some("hello\nworld".to_string()),
            files: Vec::new(),
            failed: Vec::new(),
            bytes: 0,
            seconds: Duration::ZERO,
        });
        assert_eq!(
            json(&report),
            r#"{
  "ok": true,
  "device": {
    "alias": "TXBOX",
    "version": "2.2",
    "deviceModel": "CLI",
    "deviceType": "headless",
    "fingerprint": "FINGERPRINT",
    "port": 53411,
    "protocol": "https",
    "download": false,
    "host": "127.0.0.1"
  },
  "text": "hello\nworld",
  "files": [],
  "failed": [],
  "bytes": 0,
  "seconds": 0.0
}"#
        );
        assert_eq!(report.text(), "R TXBOX: Message received\n  hello\n  world");
    }

    #[test]
    fn duration_rendering_matches_the_rounded_seconds() {
        let render = |duration| {
            Report::Send(SendReport {
                ok: true,
                device: device("RXBOX", 53410),
                session_id: Some("SESSION".to_string()),
                text: None,
                files: Vec::new(),
                declined: Vec::new(),
                bytes: 0,
                seconds: duration,
            })
            .text()
        };
        assert!(render(Duration::from_millis(994)).ends_with("took 0s)"));
        assert!(render(Duration::from_millis(995)).ends_with("took 1s)"));
        assert!(render(Duration::from_millis(2996)).ends_with("took 3s)"));
        assert!(render(Duration::from_millis(59_997)).ends_with("took 1m 0s)"));
        assert!(render(Duration::from_millis(2500)).ends_with("took 2s)"));
    }

    #[test]
    fn seconds_are_rounded_to_two_decimals() {
        let report = Report::Send(SendReport {
            ok: true,
            device: device("RXBOX", 53410),
            session_id: Some("SESSION".to_string()),
            text: None,
            files: Vec::new(),
            declined: Vec::new(),
            bytes: 0,
            seconds: Duration::from_micros(3_456_789),
        });
        assert!(json(&report).contains("\"seconds\": 3.46"));
    }
}
