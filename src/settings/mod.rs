mod config;
mod identity;

pub use identity::{Identity, Presence};

use crate::cli::Overrides;
use anyhow::Context;
use localsend::multicast::{DEFAULT_MULTICAST_GROUP, DEFAULT_PORT};
use std::io::ErrorKind;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

pub struct Settings {
    pub destination: PathBuf,
    pub multicast: Ipv4Addr,
    pub identity: Identity,
    pub presence: Presence,
}

impl Settings {
    pub fn load(overrides: &Overrides) -> anyhow::Result<Self> {
        let dir = dir()?;
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Could not create {}", dir.display()))?;
        Self::load_from(&dir, overrides)
    }

    fn load_from(dir: &Path, overrides: &Overrides) -> anyhow::Result<Self> {
        let config = config::load(dir)?;
        let alias = overrides
            .alias
            .clone()
            .or(config.alias)
            .unwrap_or_else(config::default_alias);
        let port = overrides.port.or(config.port).unwrap_or(DEFAULT_PORT);
        let destination = overrides
            .destination
            .clone()
            .or(config.destination)
            .map_or_else(config::default_destination, config::expand_tilde);

        let multicast = overrides
            .multicast
            .or(config.multicast)
            .unwrap_or(DEFAULT_MULTICAST_GROUP);

        let identity = Identity::load_or_generate(dir)?;
        let presence = Presence::new(&identity, alias, port);
        Ok(Self {
            destination,
            multicast,
            identity,
            presence,
        })
    }

    pub fn prepare_destination(&self) -> anyhow::Result<()> {
        let path = &self.destination;
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => Ok(()),
            Ok(_) => anyhow::bail!("Not a directory: {}", path.display()),
            Err(err) if err.kind() == ErrorKind::NotFound => std::fs::create_dir_all(path)
                .with_context(|| format!("Could not create {}", path.display())),
            Err(err) => Err(err).context(format!("Could not use the directory {}", path.display())),
        }
    }
}

fn dir() -> anyhow::Result<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .context("Could not locate a configuration directory (set XDG_CONFIG_HOME)")?;
    Ok(base.join("localsend-cli"))
}

#[cfg(test)]
mod tests {
    use super::*;

    static IDENTITY: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let dir = tempfile::tempdir().unwrap();
        Identity::load_or_generate(dir.path()).unwrap();
        std::fs::read_to_string(dir.path().join("identity.pem")).unwrap()
    });

    fn seeded() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("identity.pem"), &*IDENTITY).unwrap();
        dir
    }

    fn nothing() -> Overrides {
        Overrides {
            alias: None,
            port: None,
            destination: None,
            multicast: None,
        }
    }

    fn config(dir: &Path, text: &str) {
        std::fs::write(dir.join("config.toml"), text).unwrap();
    }

    #[test]
    fn defaults_apply_when_nothing_is_configured() {
        let dir = seeded();
        let settings = Settings::load_from(dir.path(), &nothing()).unwrap();
        assert_eq!(settings.presence.port, DEFAULT_PORT);
        assert_eq!(settings.presence.alias, config::default_alias());
        assert_eq!(settings.destination, config::default_destination());
        assert_eq!(settings.multicast, DEFAULT_MULTICAST_GROUP);
    }

    #[test]
    fn the_multicast_group_follows_the_same_precedence() {
        let dir = seeded();
        config(dir.path(), "multicast = \"239.1.2.3\"\n");
        let settings = Settings::load_from(dir.path(), &nothing()).unwrap();
        assert_eq!(settings.multicast, Ipv4Addr::new(239, 1, 2, 3));

        let overrides = Overrides {
            multicast: Some(Ipv4Addr::new(224, 0, 0, 200)),
            ..nothing()
        };
        let settings = Settings::load_from(dir.path(), &overrides).unwrap();
        assert_eq!(settings.multicast, Ipv4Addr::new(224, 0, 0, 200));
    }

    #[test]
    fn the_config_file_is_used_when_there_are_no_overrides() {
        let dir = seeded();
        config(
            dir.path(),
            "alias = \"FromConfig\"\nport = 1111\ndestination = \"/tmp/from-config\"\n",
        );
        let settings = Settings::load_from(dir.path(), &nothing()).unwrap();
        assert_eq!(settings.presence.alias, "FromConfig");
        assert_eq!(settings.presence.port, 1111);
        assert_eq!(settings.destination, PathBuf::from("/tmp/from-config"));
    }

    #[test]
    fn an_override_beats_the_config_file() {
        let dir = seeded();
        config(
            dir.path(),
            "alias = \"FromConfig\"\nport = 1111\ndestination = \"/tmp/from-config\"\n",
        );
        let overrides = Overrides {
            alias: Some("FromFlag".to_string()),
            port: Some(2222),
            destination: Some(PathBuf::from("/tmp/from-flag")),
            ..nothing()
        };
        let settings = Settings::load_from(dir.path(), &overrides).unwrap();
        assert_eq!(settings.presence.alias, "FromFlag");
        assert_eq!(settings.presence.port, 2222);
        assert_eq!(settings.destination, PathBuf::from("/tmp/from-flag"));
    }

    #[test]
    fn overrides_are_applied_field_by_field() {
        let dir = seeded();
        config(dir.path(), "alias = \"FromConfig\"\nport = 1111\n");
        let overrides = Overrides {
            port: Some(2222),
            ..nothing()
        };
        let settings = Settings::load_from(dir.path(), &overrides).unwrap();
        assert_eq!(settings.presence.alias, "FromConfig");
        assert_eq!(settings.presence.port, 2222);
    }

    #[test]
    fn a_destination_tilde_is_expanded() {
        let dir = seeded();
        let overrides = Overrides {
            destination: Some(PathBuf::from("~/inbox")),
            ..nothing()
        };
        let settings = Settings::load_from(dir.path(), &overrides).unwrap();
        assert_eq!(
            settings.destination,
            dirs::home_dir().unwrap().join("inbox")
        );
    }

    #[test]
    fn the_presence_carries_the_identity_fingerprint() {
        let dir = seeded();
        let settings = Settings::load_from(dir.path(), &nothing()).unwrap();
        assert_eq!(settings.presence.fingerprint, settings.identity.fingerprint);
    }

    #[test]
    fn a_missing_destination_is_created_and_a_file_in_the_way_is_reported() {
        let dir = seeded();
        let nested = dir.path().join("deep/nested");
        let overrides = Overrides {
            destination: Some(nested.clone()),
            ..nothing()
        };
        let settings = Settings::load_from(dir.path(), &overrides).unwrap();
        settings.prepare_destination().unwrap();
        assert!(nested.is_dir());
        settings.prepare_destination().unwrap();

        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"x").unwrap();
        let overrides = Overrides {
            destination: Some(blocked.clone()),
            ..nothing()
        };
        let settings = Settings::load_from(dir.path(), &overrides).unwrap();
        let error = format!("{:#}", settings.prepare_destination().unwrap_err());
        assert_eq!(error, format!("Not a directory: {}", blocked.display()));
    }
}
