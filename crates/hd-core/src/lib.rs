//! Shared building blocks of hassdash4linux: configuration, entry model,
//! secrets, D-Bus interface names, autostart and translations.

#![forbid(unsafe_code)]

pub mod autostart;
pub mod config;
pub mod entry;
pub mod i18n;
pub mod ipc;
pub mod paths;
pub mod rand;
pub mod secrets;
pub mod server;

/// The reverse-DNS application ID. It is used for the GtkApplication ID, the
/// desktop file, Flatpak and the Home Assistant `mobile_app` `app_id`.
pub const APP_ID: &str = "net.davidotek.hassdash4linux";
/// The human-readable application name.
pub const APP_NAME: &str = "hassdash4linux";
/// The version from Cargo.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// File name of the daemon binary.
pub const DAEMON_BIN: &str = "hassdash4linux";
/// File name of the UI binary.
pub const UI_BIN: &str = "hassdash4linux-ui";

/// Whether the process runs inside a Flatpak sandbox.
pub fn is_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

/// The path of the AppImage file this process was started from, if any.
pub fn appimage_path() -> Option<std::path::PathBuf> {
    let p = std::env::var_os("APPIMAGE")?;
    let p = std::path::PathBuf::from(p);
    p.is_absolute().then_some(p)
}

/// Shorten a string to at most `max` characters (not bytes), appending an
/// ellipsis when something was cut off.
pub fn truncate_chars(s: &str, max: usize) -> String {
    let mut it = s.char_indices();
    match it.nth(max) {
        None => s.to_owned(),
        Some((idx, _)) => {
            let mut out = s[..idx].to_owned();
            out.pop();
            out.push('…');
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate() {
        assert_eq!(truncate_chars("hello", 10), "hello");
        assert_eq!(truncate_chars("hello", 5), "hello");
        assert_eq!(truncate_chars("hello world", 5), "hell…");
        assert_eq!(truncate_chars("äöüäöü", 3), "äö…");
    }
}
