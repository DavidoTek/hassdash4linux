//! "Start on login" for every install type:
//!
//! | Install type | Mechanism |
//! |---|---|
//! | Flatpak | Background portal (`RequestBackground` with `autostart: true`) |
//! | AppImage | `~/.config/autostart/<app id>.desktop` pointing to `$APPIMAGE` |
//! | Nix module | systemd user unit `hassdash4linux.service`; read-only here |
//! | anything else | `~/.config/autostart/<app id>.desktop` with `Exec=hassdash4linux` (via `PATH`) |

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use zbus::zvariant::{OwnedValue, Value};

use crate::{APP_ID, DAEMON_BIN};

/// The name of the systemd user unit installed by the Nix modules.
pub const SYSTEMD_UNIT: &str = "hassdash4linux.service";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    /// Background portal; the state cannot be queried and is cached in the preferences.
    Flatpak,
    /// Desktop file pointing to the AppImage.
    AppImage(PathBuf),
    /// Managed by a NixOS or home-manager module (systemd user unit).
    NixManaged,
    /// Desktop file that runs `hassdash4linux` from `PATH`.
    DesktopFile,
}

#[derive(Debug)]
pub struct AutostartError(pub String);

impl std::fmt::Display for AutostartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AutostartError {}

impl From<std::io::Error> for AutostartError {
    fn from(e: std::io::Error) -> Self {
        AutostartError(e.to_string())
    }
}

impl From<zbus::Error> for AutostartError {
    fn from(e: zbus::Error) -> Self {
        AutostartError(e.to_string())
    }
}

/// Detect how autostart works for this installation.
pub async fn method() -> Method {
    if crate::is_flatpak() {
        return Method::Flatpak;
    }
    if let Some(p) = crate::appimage_path() {
        return Method::AppImage(p);
    }
    if systemd_unit_exists().await {
        return Method::NixManaged;
    }
    Method::DesktopFile
}

/// Path of our XDG autostart desktop file.
pub fn desktop_file_path() -> PathBuf {
    crate::paths::autostart_dir().join(format!("{APP_ID}.desktop"))
}

/// Whether autostart is enabled. `flatpak_cached` is the value stored in the
/// preferences (used only for Flatpak).
pub async fn is_enabled(method: &Method, flatpak_cached: bool) -> bool {
    match method {
        Method::Flatpak => flatpak_cached,
        Method::NixManaged => systemd_unit_enabled().await,
        Method::AppImage(_) | Method::DesktopFile => desktop_file_path().exists(),
    }
}

/// Enable or disable autostart. Returns the new state.
pub async fn set_enabled(method: &Method, enable: bool) -> Result<bool, AutostartError> {
    match method {
        Method::Flatpak => request_background(enable).await,
        Method::NixManaged => Err(AutostartError(
            "autostart is managed by your Nix configuration".into(),
        )),
        Method::AppImage(path) => {
            if enable {
                write_desktop_file(&exec_for_appimage(path)?)?;
            } else {
                remove_desktop_file()?;
            }
            Ok(enable)
        }
        Method::DesktopFile => {
            if enable {
                write_desktop_file(DAEMON_BIN)?;
            } else {
                remove_desktop_file()?;
            }
            Ok(enable)
        }
    }
}

/// AppImage only: if an autostart entry exists but points to another file
/// (the AppImage was moved or replaced by a newer version), rewrite it.
pub fn refresh_appimage_entry() {
    let Some(appimage) = crate::appimage_path() else {
        return;
    };
    let path = desktop_file_path();
    let Ok(current) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(exec) = exec_for_appimage(&appimage) else {
        return;
    };
    let wanted = format!("Exec={exec} --autostart");
    if !current.lines().any(|l| l == wanted) {
        match write_desktop_file(&exec) {
            Ok(()) => tracing::info!("updated autostart entry to {}", appimage.display()),
            Err(e) => tracing::warn!("could not update autostart entry: {e}"),
        }
    }
}

/// Quote a path for the `Exec` key (Desktop Entry spec). Paths containing
/// characters that need nested escaping are rejected instead.
fn exec_for_appimage(path: &Path) -> Result<String, AutostartError> {
    let s = path
        .to_str()
        .ok_or_else(|| AutostartError("the AppImage path is not valid UTF-8".into()))?;
    if s.chars()
        .any(|c| matches!(c, '"' | '`' | '$' | '\\') || c.is_control())
    {
        return Err(AutostartError(
            "the AppImage path contains characters that cannot be used for autostart; move it to a simpler path".into(),
        ));
    }
    Ok(format!("\"{}\"", s.replace('%', "%%")))
}

fn desktop_file_content(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=hassdash4linux\n\
         Comment=Home Assistant in your system tray\n\
         Exec={exec} --autostart\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n\
         X-KDE-autostart-after=panel\n"
    )
}

fn write_desktop_file(exec: &str) -> Result<(), AutostartError> {
    let path = desktop_file_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("desktop.tmp");
    std::fs::write(&tmp, desktop_file_content(exec))?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn remove_desktop_file() -> Result<(), AutostartError> {
    match std::fs::remove_file(desktop_file_path()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait SystemdManager {
    fn get_unit_file_state(&self, name: &str) -> zbus::Result<String>;
}

async fn unit_file_state() -> Option<String> {
    let conn = zbus::Connection::session().await.ok()?;
    let proxy = SystemdManagerProxy::new(&conn).await.ok()?;
    proxy.get_unit_file_state(SYSTEMD_UNIT).await.ok()
}

async fn systemd_unit_exists() -> bool {
    unit_file_state().await.is_some()
}

async fn systemd_unit_enabled() -> bool {
    matches!(
        unit_file_state().await.as_deref(),
        Some("enabled" | "enabled-runtime" | "linked" | "linked-runtime" | "static" | "alias")
    )
}

#[zbus::proxy(
    interface = "org.freedesktop.portal.Background",
    default_service = "org.freedesktop.portal.Desktop",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait Background {
    fn request_background(
        &self,
        parent_window: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
}

/// Ask the Background portal to enable or disable autostart.
async fn request_background(enable: bool) -> Result<bool, AutostartError> {
    let conn = zbus::Connection::session().await?;
    let token = format!("hd{}", crate::rand::token(9).replace(['-', '_'], "x"));
    let sender = conn
        .unique_name()
        .ok_or_else(|| AutostartError("no unique bus name".into()))?
        .trim_start_matches(':')
        .replace('.', "_");
    let request_path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");

    // Subscribe to the response before sending the request to avoid a race.
    let request = zbus::Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        request_path.as_str(),
        "org.freedesktop.portal.Request",
    )
    .await?;
    let mut responses = request.receive_signal("Response").await?;

    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert(
        "reason",
        Value::from(crate::i18n::tr(
            "Show the Home Assistant tray icon after login",
        )),
    );
    options.insert("autostart", Value::from(enable));
    options.insert("commandline", Value::from(vec![DAEMON_BIN, "--autostart"]));
    options.insert("dbus-activatable", Value::from(false));
    BackgroundProxy::new(&conn)
        .await?
        .request_background("", options)
        .await?;

    use futures_util::StreamExt;
    let msg = tokio::time::timeout(std::time::Duration::from_secs(300), responses.next())
        .await
        .map_err(|_| AutostartError("the background portal did not answer".into()))?
        .ok_or_else(|| AutostartError("the background portal closed the request".into()))?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize()?;
    if code != 0 {
        return Err(AutostartError(
            crate::i18n::tr("The request to run in the background was denied.").into(),
        ));
    }
    let granted = results
        .get("autostart")
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(enable);
    Ok(granted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_quoting() {
        assert_eq!(
            exec_for_appimage(Path::new("/home/a/Apps/hd.AppImage")).unwrap(),
            "\"/home/a/Apps/hd.AppImage\""
        );
        assert_eq!(
            exec_for_appimage(Path::new("/home/a/My Apps/100%.AppImage")).unwrap(),
            "\"/home/a/My Apps/100%%.AppImage\""
        );
        assert!(exec_for_appimage(Path::new("/home/a/$x.AppImage")).is_err());
    }

    #[test]
    fn desktop_content() {
        let c = desktop_file_content("hassdash4linux");
        assert!(c.contains("Exec=hassdash4linux --autostart\n"));
        assert!(c.starts_with("[Desktop Entry]\n"));
    }
}
