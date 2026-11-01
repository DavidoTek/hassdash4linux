//! The daemon's D-Bus service (single instance + `ReloadConfig`/`ShowSettings`/`Quit`)
//! and the logind sleep signal.

use futures_util::StreamExt;
use tokio::sync::mpsc;
use zbus::fdo::{RequestNameFlags, RequestNameReply};

use crate::app::Control;
use hd_core::ipc::{DAEMON_BUS_NAME, DAEMON_OBJECT_PATH, DaemonProxy};

struct DaemonIface {
    tx: mpsc::UnboundedSender<Control>,
}

#[zbus::interface(name = "net.davidotek.hassdash4linux.Daemon")]
impl DaemonIface {
    async fn reload_config(&self) {
        let _ = self.tx.send(Control::Reload);
    }

    async fn show_settings(&self) {
        let _ = self.tx.send(Control::ShowSettings);
    }

    async fn quit(&self) {
        let _ = self.tx.send(Control::Quit);
    }
}

pub enum Instance {
    /// We own the bus name.
    Primary(zbus::Connection),
    /// Another daemon runs; it was asked to show the settings.
    Secondary,
}

/// Serve the interface and claim the well-known name. If another daemon
/// already owns it, ask that one to show its settings.
pub async fn start(tx: mpsc::UnboundedSender<Control>) -> zbus::Result<Instance> {
    let conn = zbus::connection::Builder::session()?
        .serve_at(DAEMON_OBJECT_PATH, DaemonIface { tx })?
        .build()
        .await?;
    match conn
        .request_name_with_flags(DAEMON_BUS_NAME, RequestNameFlags::DoNotQueue.into())
        .await
    {
        Ok(RequestNameReply::PrimaryOwner) | Ok(RequestNameReply::AlreadyOwner) => {
            Ok(Instance::Primary(conn))
        }
        Ok(_) | Err(zbus::Error::NameTaken) => {
            DaemonProxy::new(&conn).await?.show_settings().await?;
            Ok(Instance::Secondary)
        }
        Err(e) => Err(e),
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1Manager {
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

/// Send [`Control::Resumed`] after the system wakes up. Failures are ignored
/// (e.g. no system bus access in a sandbox).
pub fn watch_sleep(tx: mpsc::UnboundedSender<Control>) {
    tokio::spawn(async move {
        let res: zbus::Result<()> = async {
            let conn = zbus::Connection::system().await?;
            let proxy = Login1ManagerProxy::new(&conn).await?;
            let mut stream = proxy.receive_prepare_for_sleep().await?;
            while let Some(sig) = stream.next().await {
                if let Ok(args) = sig.args()
                    && !args.start
                {
                    let _ = tx.send(Control::Resumed);
                }
            }
            Ok(())
        }
        .await;
        if let Err(e) = res {
            tracing::info!("not watching for suspend/resume: {e}");
        }
    });
}
