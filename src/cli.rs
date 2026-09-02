use crate::util;
use clap::{ArgGroup, Parser};
use std::fmt;
use std::io::IsTerminal;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::Duration;

const HELP_SECTIONS: &str = "Examples:\n  \
                             lsd --discover\n  \
                             lsd --send \"Smart Raspberry\" report.pdf notes.txt\n  \
                             lsd --json --send 10.0.0.20 report.pdf\n  \
                             lsd --json --receive\n\
                             \nEnvironment Variables:\n  \
                             XDG_CONFIG_HOME, LOCALSEND_ALIAS, LOCALSEND_PORT, LOCALSEND_DESTINATION,\n  LOCALSEND_MULTICAST";

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_RECEIVE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Parser)]
#[command(name = "lsd", version, about = "LocalSend Dispatcher", after_help = HELP_SECTIONS)]
#[command(group(ArgGroup::new("mode").required(true).args(["discover", "send", "receive"])))]
pub struct Args {
    #[arg(
        long,
        help = "Print the LocalSend devices found on the network, then exit"
    )]
    pub discover: bool,

    #[arg(
        long,
        value_name = "DEVICE",
        requires = "files",
        help = "Send files to a device, given as an alias, a fingerprint, an IP address or IP:PORT"
    )]
    pub send: Option<String>,

    #[arg(long, help = "Accept a single incoming transfer, then exit")]
    pub receive: bool,

    #[arg(
        value_name = "FILE",
        conflicts_with_all = ["discover", "receive"],
        help = "The files to send, or - to send a text message read from stdin"
    )]
    pub files: Vec<PathBuf>,

    #[arg(long, help = "Print the result as a JSON document instead of text")]
    pub json: bool,

    #[arg(
        long,
        env = "LOCALSEND_ALIAS",
        help = "Device name shown to other devices [default: config.toml, else the hostname]"
    )]
    pub alias: Option<String>,

    #[arg(
        long,
        env = "LOCALSEND_PORT",
        help = "Port of the HTTP server, 0 takes a free one [default: config.toml, else 53317]"
    )]
    pub port: Option<u16>,

    #[arg(
        long,
        env = "LOCALSEND_DESTINATION",
        help = "Directory where received files are saved [default: config.toml, else the Downloads folder]"
    )]
    pub destination: Option<PathBuf>,

    #[arg(
        long,
        value_name = "ADDRESS",
        env = "LOCALSEND_MULTICAST",
        help = "Multicast group used for discovery [default: config.toml, else 224.0.0.167]"
    )]
    pub multicast: Option<Ipv4Addr>,

    #[arg(
        long,
        value_name = "SECONDS",
        help = "Seconds to wait for discovery, or for a transfer to arrive or advance [default: 10, with --receive 60]"
    )]
    pub timeout: Option<u64>,
}

pub struct Overrides {
    pub alias: Option<String>,
    pub port: Option<u16>,
    pub destination: Option<PathBuf>,
    pub multicast: Option<Ipv4Addr>,
}

impl From<&Args> for Overrides {
    fn from(args: &Args) -> Self {
        Self {
            alias: args.alias.clone(),
            port: args.port,
            destination: args.destination.clone(),
            multicast: args.multicast,
        }
    }
}

#[derive(Debug)]
pub struct UsageError(String);

impl fmt::Display for UsageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

pub enum Command {
    Discover,
    Send { device: String, files: Vec<PathBuf> },
    Receive,
}

impl Command {
    pub fn from_args(args: &Args) -> Result<Self, UsageError> {
        if let Some(device) = &args.send {
            let from_stdin = args
                .files
                .iter()
                .filter(|path| util::is_stdin(path))
                .count();
            if from_stdin > 1 {
                return Err(UsageError(format!(
                    "Stdin can only be read once, but {from_stdin} arguments are \"{}\"",
                    util::STDIN
                )));
            }
            if from_stdin == 1 && std::io::stdin().is_terminal() {
                return Err(UsageError(
                    "No text on stdin (pipe it, as in: echo \"Hello World\" | lsd --send DEVICE -)"
                        .to_string(),
                ));
            }
            for path in &args.files {
                if !util::is_stdin(path) {
                    util::regular_file(path).map_err(UsageError)?;
                }
            }
            return Ok(Self::Send {
                device: device.clone(),
                files: args.files.clone(),
            });
        }
        if args.receive {
            Ok(Self::Receive)
        } else {
            Ok(Self::Discover)
        }
    }

    pub fn timeout(&self, seconds: Option<u64>) -> Duration {
        let default = match self {
            Self::Receive => DEFAULT_RECEIVE_TIMEOUT,
            _ => DEFAULT_TIMEOUT,
        };
        seconds.map_or(default, Duration::from_secs)
    }
}
