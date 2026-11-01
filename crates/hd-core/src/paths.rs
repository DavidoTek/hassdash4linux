//! XDG paths used by hassdash4linux.

use std::path::PathBuf;

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `$XDG_CONFIG_HOME`, falling back to `~/.config`.
pub fn xdg_config_home() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        Some(p) if p.is_absolute() => p,
        _ => home().join(".config"),
    }
}

/// Directory holding `config.toml`.
pub fn config_dir() -> PathBuf {
    xdg_config_home().join("hassdash4linux")
}

/// Full path of the configuration file.
pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

/// Directory for XDG autostart entries.
pub fn autostart_dir() -> PathBuf {
    xdg_config_home().join("autostart")
}
