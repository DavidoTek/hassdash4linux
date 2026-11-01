//! The daemon's D-Bus interface.
//!
//! It is deliberately tiny and takes no arguments: nothing reachable over
//! D-Bus can call Home Assistant services.

/// Well-known bus name owned by the daemon.
pub const DAEMON_BUS_NAME: &str = "net.davidotek.hassdash4linux.Daemon";
/// Object path of the daemon interface.
pub const DAEMON_OBJECT_PATH: &str = "/net/davidotek/hassdash4linux/Daemon";

#[zbus::proxy(
    interface = "net.davidotek.hassdash4linux.Daemon",
    default_service = "net.davidotek.hassdash4linux.Daemon",
    default_path = "/net/davidotek/hassdash4linux/Daemon"
)]
pub trait Daemon {
    /// Re-read the configuration file and secrets.
    fn reload_config(&self) -> zbus::Result<()>;
    /// Open the settings window.
    fn show_settings(&self) -> zbus::Result<()>;
    /// Exit the daemon.
    fn quit(&self) -> zbus::Result<()>;
}

/// Ask a running daemon to reload its configuration. Errors (for example,
/// when no daemon runs) are logged and otherwise ignored.
pub async fn notify_reload() {
    let res = async {
        let conn = zbus::Connection::session().await?;
        DaemonProxy::new(&conn).await?.reload_config().await
    }
    .await;
    if let Err(e) = res {
        tracing::info!("could not notify the daemon: {e}");
    }
}
