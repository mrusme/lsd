use anyhow::Context;
use serde::Deserialize;
use std::io::ErrorKind;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub alias: Option<String>,
    pub port: Option<u16>,
    pub destination: Option<PathBuf>,
    pub multicast: Option<Ipv4Addr>,
}

pub fn load(dir: &Path) -> anyhow::Result<Config> {
    let path = dir.join("config.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text)
            .with_context(|| format!("Invalid config file: {}", path.display())),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(Config::default()),
        Err(err) => Err(err).context(format!("Could not read {}", path.display())),
    }
}

pub fn default_alias() -> String {
    trim_host(&gethostname::gethostname().to_string_lossy())
}

fn trim_host(host: &str) -> String {
    host.trim_end_matches(".local").to_string()
}

pub fn default_destination() -> PathBuf {
    dirs::download_dir().unwrap_or_else(|| PathBuf::from("."))
}

pub fn expand_tilde(path: PathBuf) -> PathBuf {
    match (path.strip_prefix("~"), dirs::home_dir()) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, text: &str) {
        std::fs::write(dir.join("config.toml"), text).unwrap();
    }

    #[test]
    fn a_missing_config_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = load(dir.path()).unwrap();
        assert!(config.alias.is_none());
        assert!(config.port.is_none());
        assert!(config.destination.is_none());
    }

    #[test]
    fn every_field_is_optional() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "port = 1234\n");
        let config = load(dir.path()).unwrap();
        assert_eq!(config.port, Some(1234));
        assert!(config.alias.is_none());
    }

    #[test]
    fn all_three_fields_are_read() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "alias = \"Smart Raspberry\"\nport = 1234\ndestination = \"/tmp/in\"\n",
        );
        let config = load(dir.path()).unwrap();
        assert_eq!(config.alias.as_deref(), Some("Smart Raspberry"));
        assert_eq!(config.port, Some(1234));
        assert_eq!(config.destination, Some(PathBuf::from("/tmp/in")));
    }

    #[test]
    fn an_unknown_key_is_rejected_rather_than_ignored() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "prot = 1234\n");
        let error = format!("{:#}", load(dir.path()).unwrap_err());
        assert!(error.contains("Invalid config file"));
        assert!(error.contains("unknown field `prot`"));
    }

    #[test]
    fn malformed_toml_is_reported_with_the_path() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "port = \n");
        let error = format!("{:#}", load(dir.path()).unwrap_err());
        assert!(error.contains("Invalid config file"));
        assert!(error.contains("config.toml"));
    }

    #[test]
    fn a_tilde_is_expanded_against_the_home_directory() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_tilde(PathBuf::from("~/in")), home.join("in"));
        assert_eq!(expand_tilde(PathBuf::from("~")), home);
        assert_eq!(
            expand_tilde(PathBuf::from("/tmp/in")),
            PathBuf::from("/tmp/in")
        );
        assert_eq!(
            expand_tilde(PathBuf::from("~user/in")),
            PathBuf::from("~user/in")
        );
    }

    #[test]
    fn the_default_alias_drops_a_local_suffix() {
        assert_eq!(trim_host("raspberry.local"), "raspberry");
        assert_eq!(trim_host("raspberry"), "raspberry");
        assert_eq!(trim_host("my.local.box"), "my.local.box");
        assert!(!default_alias().is_empty());
    }
}
