//! The configuration file (`~/.config/hassdash4linux/config.toml`).
//!
//! Only the UI writes the file, atomically and with mode 0600. The daemon
//! reads it at startup and whenever the UI calls `ReloadConfig()`. Secrets are
//! never stored here (see [`crate::secrets`]).

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::entry::Entry;

/// Current schema version.
pub const VERSION: u32 = 1;
/// Maximum number of tray entries.
pub const MAX_ENTRIES: usize = 100;
/// Maximum length of the device name.
pub const MAX_DEVICE_NAME_CHARS: usize = 64;
/// Maximum size of the configuration file we are willing to parse.
const MAX_FILE_SIZE: u64 = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<Connection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion: Option<Companion>,
    #[serde(default)]
    pub preferences: Preferences,
    #[serde(default, rename = "entry", skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<Entry>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: VERSION,
            connection: None,
            companion: None,
            preferences: Preferences::default(),
            entries: Vec::new(),
        }
    }
}

/// How hassdash4linux authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    /// OAuth2 authorization code flow with PKCE; a refresh token is kept in the keyring.
    Oauth,
    /// A long-lived access token pasted by the user, kept in the keyring.
    LongLivedToken,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    /// Normalised base URL, e.g. `https://ha.example.org`.
    pub url: String,
    pub auth: AuthKind,
    /// OAuth only: the client ID used to obtain the refresh token. It must be
    /// sent again on every refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Optional pinned certificate (`sha256:<hex>` of the server's DER
    /// certificate) for self-signed certificates the user trusted explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_pin: Option<String>,
    /// Display name of the Home Assistant instance (from `get_config`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Companion {
    /// Random UUID identifying this installation (never the machine ID).
    pub device_id: String,
    pub device_name: String,
    /// Whether this installation registered itself as a mobile_app device.
    pub registered: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Preferences {
    /// Show notifications sent to `notify.mobile_app_<device>`.
    pub notifications: bool,
    /// Show a desktop notification when running an entry fails.
    pub notify_on_failure: bool,
    /// Flatpak only: last autostart state requested through the Background
    /// portal (the portal cannot be queried).
    pub flatpak_autostart: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            notifications: true,
            notify_on_failure: true,
            flatpak_autostart: false,
        }
    }
}

/// Errors loading or saving the configuration.
#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(String),
    Invalid(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "configuration file I/O error: {e}"),
            ConfigError::Parse(e) => write!(f, "configuration file is malformed: {e}"),
            ConfigError::Invalid(e) => write!(f, "configuration file is invalid: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<std::io::Error> for ConfigError {
    fn from(e: std::io::Error) -> Self {
        ConfigError::Io(e)
    }
}

impl Config {
    /// Load from the default location. A missing file yields the default
    /// (unconfigured) configuration.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(&crate::paths::config_file())
    }

    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        if meta.len() > MAX_FILE_SIZE {
            return Err(ConfigError::Invalid("file is too large".into()));
        }
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text)
    }

    /// Parse and validate configuration text.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let mut cfg: Config =
            toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        // Use the canonical URL everywhere, so that a hand-edited trailing slash
        // or upper-case host still finds the secrets in the keyring.
        if let Some(c) = &mut cfg.connection
            && let Ok(u) = crate::server::validate(&c.url)
        {
            c.url = crate::server::canonical(&u);
        }
        cfg.validate().map_err(ConfigError::Invalid)?;
        Ok(cfg)
    }

    /// Check all invariants. The daemon relies on this before acting on the config.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != VERSION {
            return Err(format!("unsupported version {}", self.version));
        }
        if self.entries.len() > MAX_ENTRIES {
            return Err(format!("too many entries (max {MAX_ENTRIES})"));
        }
        for e in &self.entries {
            e.validate()?;
        }
        if let Some(c) = &self.connection {
            crate::server::validate(&c.url).map_err(|e| format!("connection.url: {e}"))?;
            match (c.auth, &c.client_id) {
                (AuthKind::Oauth, Some(id)) => validate_client_id(id)?,
                (AuthKind::Oauth, None) => return Err("connection.client_id is missing".into()),
                (AuthKind::LongLivedToken, Some(_)) => {
                    return Err("connection.client_id is only valid for OAuth".into());
                }
                (AuthKind::LongLivedToken, None) => {}
            }
            if let Some(pin) = &c.tls_pin
                && !is_valid_pin(pin)
            {
                return Err("connection.tls_pin must be sha256:<64 hex digits>".into());
            }
            if let Some(n) = &c.location_name
                && n.chars().count() > 200
            {
                return Err("connection.location_name is too long".into());
            }
        }
        if let Some(c) = &self.companion {
            if !is_uuid(&c.device_id) {
                return Err("companion.device_id must be a UUID".into());
            }
            validate_device_name(&c.device_name)?;
        }
        Ok(())
    }

    /// Save atomically to the default location.
    pub fn save(&self) -> Result<(), ConfigError> {
        self.save_to(&crate::paths::config_file())
    }

    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        self.validate().map_err(ConfigError::Invalid)?;
        let text = toml::to_string_pretty(self).map_err(|e| ConfigError::Parse(e.to_string()))?;
        let dir = path
            .parent()
            .ok_or_else(|| ConfigError::Invalid("no parent directory".into()))?;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let tmp = dir.join(format!(".config.toml.{}.tmp", std::process::id()));
        let result = (|| {
            let create = || {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&tmp)
            };
            let mut f = match create() {
                // Left behind by an earlier process with the same pid that was
                // killed before the rename; no live process can own it.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    std::fs::remove_file(&tmp)?;
                    create()?
                }
                r => r?,
            };
            f.write_all(
                b"# hassdash4linux configuration. Secrets are kept in the system keyring.\n",
            )?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, path)?;
            if let Ok(d) = std::fs::File::open(dir) {
                let _ = d.sync_all();
            }
            Ok::<_, std::io::Error>(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        Ok(result?)
    }

    /// Whether a connection is configured.
    pub fn is_configured(&self) -> bool {
        self.connection.is_some()
    }

    /// The entry with the given entity ID, if configured.
    pub fn entry(&self, entity_id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.entity_id == entity_id)
    }
}

/// An OAuth client ID must be our loopback URL (`http://127.0.0.1:<port>/`).
fn validate_client_id(id: &str) -> Result<(), String> {
    let u = url::Url::parse(id).map_err(|_| "connection.client_id is not a URL".to_string())?;
    let ok = u.scheme() == "http"
        && u.host_str() == Some("127.0.0.1")
        && u.port().is_some()
        && u.path() == "/"
        && u.query().is_none()
        && u.fragment().is_none();
    if ok {
        Ok(())
    } else {
        Err("connection.client_id must be http://127.0.0.1:<port>/".into())
    }
}

pub fn validate_device_name(name: &str) -> Result<(), String> {
    let n = name.chars().count();
    if n == 0 || n > MAX_DEVICE_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(format!(
            "the device name must be 1 to {MAX_DEVICE_NAME_CHARS} characters"
        ));
    }
    Ok(())
}

pub fn is_valid_pin(pin: &str) -> bool {
    pin.strip_prefix("sha256:").is_some_and(|h| {
        h.len() == 64
            && h.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Entry, EntryKind};

    #[allow(clippy::field_reassign_with_default)]
    fn sample() -> Config {
        let mut c = Config::default();
        c.connection = Some(Connection {
            url: "https://ha.example.org".into(),
            auth: AuthKind::Oauth,
            client_id: Some("http://127.0.0.1:43123/".into()),
            tls_pin: None,
            location_name: Some("Home".into()),
        });
        c.companion = Some(Companion {
            device_id: crate::rand::uuid_v4(),
            device_name: "laptop".into(),
            registered: true,
        });
        c.entries = vec![
            Entry::new(EntryKind::Action, "scene.movie"),
            Entry::separator(),
            Entry::new(EntryKind::Sensor, "sensor.temp"),
        ];
        c
    }

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join(format!("hd-core-test-{}", crate::rand::token(6)));
        let path = dir.join("config.toml");
        let c = sample();
        c.save_to(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(mode.mode() & 0o777, 0o600);
        assert_eq!(Config::load_from(&path).unwrap(), c);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stale_tmp_file_does_not_block_saving() {
        let dir = std::env::temp_dir().join(format!("hd-core-test-{}", crate::rand::token(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join(format!(".config.toml.{}.tmp", std::process::id()));
        std::fs::write(&tmp, "stale").unwrap();
        let path = dir.join("config.toml");
        let c = sample();
        c.save_to(&path).unwrap();
        assert!(!tmp.exists());
        assert_eq!(Config::load_from(&path).unwrap(), c);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn url_is_canonicalised_on_load() {
        let text = "version = 1\n[connection]\nurl = \"https://HA.example.org/\"\nauth = \"long_lived_token\"\n";
        let c = Config::parse(text).unwrap();
        assert_eq!(c.connection.unwrap().url, "https://ha.example.org");
    }

    #[test]
    fn missing_file_is_default() {
        let c = Config::load_from(Path::new("/nonexistent/hassdash4linux.toml")).unwrap();
        assert!(!c.is_configured());
    }

    #[test]
    fn rejects_bad_input() {
        assert!(Config::parse("version = 1\nfoo = 1").is_err());
        assert!(Config::parse("version = 2").is_err());
        let bad_service =
            "version = 1\n[[entry]]\nkind = \"action\"\nentity_id = \"hassio.host_shutdown\"\n";
        assert!(Config::parse(bad_service).is_err());
        let bad_field = "version = 1\n[[entry]]\nkind = \"action\"\nentity_id = \"scene.x\"\nservice = \"hassio.host_shutdown\"\n";
        assert!(Config::parse(bad_field).is_err());
        let bad_url =
            "version = 1\n[connection]\nurl = \"http://8.8.8.8\"\nauth = \"long_lived_token\"\n";
        assert!(Config::parse(bad_url).is_err());
        let bad_client = "version = 1\n[connection]\nurl = \"https://a.example\"\nauth = \"oauth\"\nclient_id = \"https://evil.example/\"\n";
        assert!(Config::parse(bad_client).is_err());
    }

    #[test]
    fn pins() {
        assert!(is_valid_pin(&format!("sha256:{}", "a".repeat(64))));
        assert!(!is_valid_pin(&format!("sha256:{}", "A".repeat(64))));
        assert!(!is_valid_pin("md5:abc"));
    }
}
