//! The daemon's main loop: configuration, Home Assistant connection, tray
//! menu, actions and notifications.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ksni::TrayMethods;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

use hd_core::config::Config;
use hd_core::entry::{Entry, EntryKind, action_service};
use hd_core::i18n::{tr, trf};
use hd_core::secrets::SecretStore;
use hd_ha::mobile_app::Registration;
use hd_ha::{Connector, EntityCache, HaError, Session, Subscription, format};

use crate::icons::TrayIcon;
use crate::notify::Notifier;
use crate::push::{self, Push};
use crate::tray::{MenuLine, Tray};

/// How long the tray host may be missing before we explain the problem.
const TRAY_HOST_GRACE: Duration = Duration::from_secs(20);

/// Messages to the main loop.
#[derive(Debug)]
pub enum Control {
    Reload,
    ShowSettings,
    Quit,
    /// A user entry in the tray menu was clicked: its index into
    /// `config.entries` and the entry the menu showed.
    Activate {
        index: usize,
        entry: Entry,
    },
    NotificationAction {
        id: u32,
        action: String,
    },
    NotificationClosed(u32),
    Resumed,
    WatcherOffline,
    WatcherOnline,
    ActionFailed {
        name: String,
        error: String,
    },
    RegistrationGone,
}

/// Events from the connection task.
enum SessionEvent {
    Connecting,
    Connected {
        session: Session,
        precision: HashMap<String, usize>,
    },
    Entities(Value),
    Push(Value),
    Disconnected(Option<HaError>),
    AuthFailed(HaError),
    /// Home Assistant no longer knows our webhook (device deleted).
    RegistrationGone,
}

#[derive(Debug, Clone, PartialEq)]
enum Status {
    Unconfigured,
    ConfigInvalid,
    Keyring(String),
    Connecting,
    Connected,
    Reconnecting(String),
    AuthFailed,
}

struct App {
    tx: mpsc::UnboundedSender<Control>,
    rx: mpsc::UnboundedReceiver<Control>,
    tray: ksni::Handle<Tray>,
    notifier: Option<Notifier>,
    config: Config,
    status: Status,
    connector: Option<Arc<Connector>>,
    registration: Option<Registration>,
    session: Option<Session>,
    cache: EntityCache,
    precision: HashMap<String, usize>,
    task: Option<JoinHandle<()>>,
    events: Option<mpsc::Receiver<SessionEvent>>,
    companion_note: Option<&'static str>,
    tray_offline_since: Option<Instant>,
    tray_hint_shown: bool,
    last_menu: Option<(TrayIcon, Option<String>, Vec<MenuLine>)>,
}

/// Start the hassdash4linux UI with the given arguments.
pub fn spawn_ui(args: &[&str]) {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(hd_core::UI_BIN)))
        .filter(|p| p.exists())
        .unwrap_or_else(|| hd_core::UI_BIN.into());
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.args(args);
    // The AppImage bundles its own Mesa, which cannot drive every GPU (e.g.
    // proprietary NVIDIA drivers); the cairo renderer always works and is
    // plenty for these simple windows.
    if hd_core::appimage_path().is_some() && std::env::var_os("GSK_RENDERER").is_none() {
        cmd.env("GSK_RENDERER", "cairo");
    }
    match cmd.spawn() {
        Ok(mut child) => {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
        }
        Err(e) => tracing::error!("could not start {}: {e}", exe.display()),
    }
}

/// Run the daemon until it is asked to quit.
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let (tx, rx) = mpsc::unbounded_channel();
    let conn = match crate::dbus::start(tx.clone()).await? {
        crate::dbus::Instance::Primary(c) => c,
        crate::dbus::Instance::Secondary => {
            tracing::info!("hassdash4linux is already running; asked it to show the settings");
            return Ok(());
        }
    };
    hd_core::autostart::refresh_appimage_entry();

    let tray = Tray {
        tx: tx.clone(),
        icon: TrayIcon::Disconnected,
        status: None,
        lines: Vec::new(),
    };
    // Flatpak's D-Bus proxy cannot allow owning `org.kde.StatusNotifierItem-<pid>-<n>`;
    // registering with the unique connection name is allowed by the spec.
    let tray = tray
        .assume_sni_available(true)
        .disable_dbus_name(hd_core::is_flatpak())
        .spawn()
        .await?;
    let notifier = match Notifier::new(&conn, tx.clone()).await {
        Ok(n) => Some(n),
        Err(e) => {
            tracing::warn!("notifications are not available: {e}");
            None
        }
    };
    crate::dbus::watch_sleep(tx.clone());

    let mut app = App {
        tx,
        rx,
        tray,
        notifier,
        config: Config::default(),
        status: Status::Unconfigured,
        connector: None,
        registration: None,
        session: None,
        cache: EntityCache::new(),
        precision: HashMap::new(),
        task: None,
        events: None,
        companion_note: None,
        tray_offline_since: None,
        tray_hint_shown: false,
        last_menu: None,
    };
    app.load().await;
    if matches!(app.status, Status::Unconfigured | Status::ConfigInvalid) {
        spawn_ui(&["--settings"]);
    }
    app.run().await;
    app.stop_connection();
    app.tray.shutdown().await;
    Ok(())
}

async fn recv_opt<T>(rx: &mut Option<mpsc::Receiver<T>>) -> Option<T> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

impl App {
    async fn run(&mut self) {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        loop {
            let hint_deadline = self
                .tray_offline_since
                .filter(|_| !self.tray_hint_shown)
                .map(|t| tokio::time::Instant::from_std(t + TRAY_HOST_GRACE));
            tokio::select! {
                c = self.rx.recv() => match c {
                    None | Some(Control::Quit) => break,
                    Some(c) => self.handle_control(c).await,
                },
                ev = recv_opt(&mut self.events) => match ev {
                    Some(ev) => self.handle_session(ev).await,
                    None => self.events = None,
                },
                _ = async { tokio::time::sleep_until(hint_deadline.unwrap()).await }, if hint_deadline.is_some() => {
                    self.tray_hint_shown = true;
                    spawn_ui(&["--settings", "--no-tray-host"]);
                }
                _ = tokio::signal::ctrl_c() => break,
                _ = async { sigterm.as_mut().unwrap().recv().await }, if sigterm.is_some() => break,
            }
        }
        tracing::info!("exiting");
    }

    fn stop_connection(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
        self.events = None;
        self.session = None;
    }

    fn start_connection(&mut self) {
        self.stop_connection();
        let Some(connector) = self.connector.clone() else {
            return;
        };
        let ids: Vec<String> = self
            .config
            .entries
            .iter()
            .filter(|e| e.kind != EntryKind::Separator)
            .map(|e| e.entity_id.clone())
            .collect();
        let webhook = self
            .registration
            .as_ref()
            .filter(|_| self.config.preferences.notifications)
            .map(|r| r.webhook_id.clone());
        let (tx, rx) = mpsc::channel(256);
        self.events = Some(rx);
        self.task = Some(tokio::spawn(connection_task(connector, ids, webhook, tx)));
        self.status = Status::Connecting;
    }

    /// (Re)load configuration and secrets, then reconnect.
    async fn load(&mut self) {
        self.stop_connection();
        self.cache.clear();
        self.precision.clear();
        self.connector = None;
        self.registration = None;
        self.companion_note = None;
        match Config::load() {
            Ok(c) => self.config = c,
            Err(e) => {
                tracing::error!("{e}");
                self.config = Config::default();
                self.status = Status::ConfigInvalid;
                self.refresh_tray().await;
                return;
            }
        }
        if !self.config.is_configured() {
            self.status = Status::Unconfigured;
            self.refresh_tray().await;
            return;
        }
        let store = match SecretStore::open().await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("{e}");
                self.status = Status::Keyring(e.to_string());
                self.refresh_tray().await;
                return;
            }
        };
        match Connector::from_config(&self.config, &store).await {
            Ok(c) => self.connector = Some(Arc::new(c)),
            Err(HaError::Auth(m)) => {
                tracing::error!("{m}");
                self.status = Status::AuthFailed;
                self.refresh_tray().await;
                return;
            }
            Err(e) => {
                tracing::error!("{e}");
                self.status = Status::Keyring(e.to_string());
                self.refresh_tray().await;
                return;
            }
        }
        match hd_ha::client::load_registration(&self.config, &store).await {
            Ok(r) => self.registration = r,
            Err(e) => tracing::warn!("companion registration not available: {e}"),
        }
        self.refresh_registration();
        self.start_connection();
        self.refresh_tray().await;
    }

    /// Send the current app version and OS to Home Assistant. A 410 answer
    /// means the device was deleted there.
    fn refresh_registration(&self) {
        let (Some(reg), Some(connector), Some(comp)) = (
            self.registration.clone(),
            self.connector.clone(),
            self.config.companion.clone(),
        ) else {
            return;
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let info = hd_ha::mobile_app::DeviceInfo::collect(&comp.device_id, &comp.device_name);
            match hd_ha::mobile_app::update_registration(
                &connector.http,
                &connector.base,
                &reg,
                &info,
            )
            .await
            {
                Ok(()) => tracing::debug!("companion registration updated"),
                Err(HaError::Gone) => {
                    let _ = tx.send(Control::RegistrationGone);
                }
                Err(e) => tracing::info!("could not update the companion registration: {e}"),
            }
        });
    }

    async fn handle_control(&mut self, c: Control) {
        match c {
            Control::Reload => {
                tracing::info!("reloading configuration");
                self.load().await;
            }
            Control::ShowSettings => spawn_ui(&["--settings"]),
            Control::Quit => {}
            Control::Activate { index, entry } => self.activate(index, &entry),
            Control::ActionFailed { name, error } => {
                if self.config.preferences.notify_on_failure
                    && let Some(n) = &mut self.notifier
                {
                    n.show_local(
                        &trf("Could not run {name}", &[("name", &name)]),
                        &error,
                        false,
                    )
                    .await;
                }
            }
            Control::NotificationAction { id, action } => self.notification_action(id, &action),
            Control::NotificationClosed(id) => {
                if let Some(n) = &mut self.notifier {
                    n.forget(id);
                }
            }
            Control::Resumed => {
                if self.connector.is_some() && self.status != Status::AuthFailed {
                    tracing::info!("resumed from suspend; reconnecting");
                    self.start_connection();
                    self.refresh_tray().await;
                }
            }
            Control::WatcherOffline => {
                self.tray_offline_since.get_or_insert_with(Instant::now);
            }
            Control::WatcherOnline => self.tray_offline_since = None,
            Control::RegistrationGone => {
                if self.registration.take().is_some() {
                    tracing::warn!("the companion registration was deleted in Home Assistant");
                    self.companion_note = Some(tr(
                        "This device was removed from Home Assistant. Open Settings to register it again.",
                    ));
                    self.start_connection();
                    self.refresh_tray().await;
                }
            }
        }
    }

    fn display_name(&self, index: usize) -> String {
        let e = &self.config.entries[index];
        e.label
            .clone()
            .or_else(|| self.cache.get(&e.entity_id).map(|s| s.friendly_name()))
            .unwrap_or_else(|| e.entity_id.clone())
    }

    fn activate(&mut self, index: usize, shown: &Entry) {
        // The menu may be older than the config (after `ReloadConfig`); never
        // run an entry other than the one the user clicked.
        let Some(entry) = self.config.entries.get(index).cloned() else {
            return;
        };
        if entry != *shown {
            tracing::info!("ignoring a click on an outdated tray menu");
            return;
        }
        match entry.kind {
            EntryKind::Action => {
                let Some(call) = action_service(&entry) else {
                    return;
                };
                let name = self.display_name(index);
                let tx = self.tx.clone();
                let Some(session) = self.session.clone() else {
                    let _ = tx.send(Control::ActionFailed {
                        name,
                        error: tr("Not connected to Home Assistant.").into(),
                    });
                    return;
                };
                tracing::info!(
                    "running {}.{} on {}",
                    call.domain,
                    call.service,
                    call.entity_id
                );
                tokio::spawn(async move {
                    if let Err(e) = session
                        .call_service(call.domain, call.service, &call.entity_id, call.data)
                        .await
                    {
                        tracing::warn!("{} failed: {e}", call.entity_id);
                        let _ = tx.send(Control::ActionFailed {
                            name,
                            error: e.to_string(),
                        });
                    }
                });
            }
            EntryKind::Entity => spawn_ui(&["--entity", &entry.entity_id]),
            EntryKind::Sensor | EntryKind::Separator => {}
        }
    }

    fn notification_action(&mut self, id: u32, action: &str) {
        let Some(notifier) = &mut self.notifier else {
            return;
        };
        let Some(shown) = notifier.shown.get(&id) else {
            return;
        };
        if !shown.from_ha {
            return;
        }
        if action == "default" {
            if let Some(u) = &shown.url {
                crate::notify::open_url(u);
            }
            return;
        }
        let Some(idx) = action
            .strip_prefix("action")
            .and_then(|n| n.parse::<usize>().ok())
        else {
            return;
        };
        let Some(a) = shown.actions.get(idx) else {
            return;
        };
        if let Some(u) = &a.uri {
            crate::notify::open_url(u);
            return;
        }
        let (Some(connector), Some(reg)) = (self.connector.clone(), self.registration.clone())
        else {
            return;
        };
        let mut data = json!({"action": a.key});
        if let Some(t) = &shown.tag {
            data["tag"] = json!(t);
        }
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match hd_ha::mobile_app::fire_event(
                &connector.http,
                &connector.base,
                &reg,
                "mobile_app_notification_action",
                data,
            )
            .await
            {
                Ok(()) => {}
                Err(HaError::Gone) => {
                    let _ = tx.send(Control::RegistrationGone);
                }
                Err(e) => tracing::warn!("could not send the notification action: {e}"),
            }
        });
    }

    async fn handle_session(&mut self, ev: SessionEvent) {
        match ev {
            SessionEvent::Connecting => {
                if self.status != Status::Connected
                    && !matches!(self.status, Status::Reconnecting(_))
                {
                    self.status = Status::Connecting;
                }
            }
            SessionEvent::Connected { session, precision } => {
                tracing::info!("connected to Home Assistant {}", session.ha_version());
                self.session = Some(session);
                self.precision = precision;
                self.status = Status::Connected;
            }
            SessionEvent::Entities(v) => {
                self.cache.apply(&v);
            }
            SessionEvent::Push(v) => {
                let Some(base) = self.connector.as_ref().map(|c| c.base.clone()) else {
                    return;
                };
                match push::parse(&v, &base) {
                    Push::Show(n) => {
                        if self.config.preferences.notifications
                            && let Some(notifier) = &mut self.notifier
                        {
                            notifier.show_ha(n).await;
                        }
                    }
                    Push::Clear { tag } => {
                        if let Some(notifier) = &mut self.notifier {
                            notifier.clear_tag(&tag).await;
                        }
                    }
                    Push::Ignored(why) => tracing::info!("ignored a notification: {why}"),
                }
            }
            SessionEvent::Disconnected(err) => {
                self.session = None;
                let msg = err.map(|e| e.to_string()).unwrap_or_default();
                if !msg.is_empty() {
                    tracing::warn!("connection failed: {msg}");
                }
                self.status = Status::Reconnecting(msg);
            }
            SessionEvent::RegistrationGone => {
                let _ = self.tx.send(Control::RegistrationGone);
            }
            SessionEvent::AuthFailed(e) => {
                tracing::error!("authentication failed: {e}");
                self.session = None;
                self.status = Status::AuthFailed;
            }
        }
        self.refresh_tray().await;
    }

    fn menu_lines(&self) -> Vec<MenuLine> {
        let connected = self.status == Status::Connected;
        let mut lines = Vec::new();
        if !self.config.is_configured() {
            return lines;
        }
        for (i, e) in self.config.entries.iter().enumerate() {
            let st = self.cache.get(&e.entity_id);
            match e.kind {
                EntryKind::Separator => lines.push(MenuLine::Separator),
                EntryKind::Action => {
                    let disabled_automation =
                        e.domain() == "automation" && st.is_some_and(|s| s.state == "off");
                    let unavailable = st.is_some_and(|s| s.state == "unavailable");
                    lines.push(MenuLine::Item {
                        index: i,
                        entry: e.clone(),
                        label: self.display_name(i),
                        enabled: connected && !disabled_automation && !unavailable,
                    });
                }
                EntryKind::Entity => {
                    let name = self.display_name(i);
                    let label = match st {
                        Some(s) if connected => format!("{name} — {}", format::entity_state(s)),
                        _ => name,
                    };
                    lines.push(MenuLine::Item {
                        index: i,
                        entry: e.clone(),
                        label,
                        enabled: connected,
                    });
                }
                EntryKind::Sensor => {
                    let name = self.display_name(i);
                    let value = match st {
                        Some(s) if connected => {
                            format::sensor_value(s, self.precision.get(&e.entity_id).copied())
                        }
                        _ => "…".into(),
                    };
                    lines.push(MenuLine::Text(format!("{name}: {value}")));
                }
            }
        }
        if self.config.is_configured() && self.config.entries.is_empty() {
            lines.push(MenuLine::Text(
                tr("No entries yet. Add some in Settings.").into(),
            ));
        }
        lines
    }

    async fn refresh_tray(&mut self) {
        let (icon, status) = match &self.status {
            Status::Unconfigured => (
                TrayIcon::Disconnected,
                Some(tr(
                    "Not connected to Home Assistant. Open Settings to connect.",
                )),
            ),
            Status::ConfigInvalid => (
                TrayIcon::Error,
                Some(tr(
                    "The configuration file is invalid. Open Settings to fix it.",
                )),
            ),
            Status::Keyring(_) => (
                TrayIcon::Error,
                Some(tr("Cannot read the login data from the keyring.")),
            ),
            Status::Connecting => (
                TrayIcon::Disconnected,
                Some(tr("Connecting to Home Assistant…")),
            ),
            Status::Reconnecting(_) => (
                TrayIcon::Disconnected,
                Some(tr("Connection lost. Reconnecting…")),
            ),
            Status::AuthFailed => (
                TrayIcon::Error,
                Some(tr(
                    "The login is no longer valid. Open Settings to log in again.",
                )),
            ),
            Status::Connected => (TrayIcon::Connected, self.companion_note),
        };
        let snapshot = (icon, status.map(str::to_owned), self.menu_lines());
        if self.last_menu.as_ref() == Some(&snapshot) {
            return;
        }
        let (icon, status, lines) = snapshot.clone();
        self.tray
            .update(move |t| {
                t.icon = icon;
                t.status = status;
                t.lines = lines;
            })
            .await;
        self.last_menu = Some(snapshot);
    }
}

async fn next_opt(sub: &mut Option<Subscription>) -> Option<Value> {
    match sub {
        Some(s) => s.next().await,
        None => std::future::pending().await,
    }
}

/// Read `display_precision` for the given entities from the entity registry.
async fn load_precision(session: &Session, ids: &[String]) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    let Ok(v) = session.entity_registry_display().await else {
        return out;
    };
    for e in v
        .get("entities")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(id), Some(dp)) = (
            e.get("ei").and_then(Value::as_str),
            e.get("dp").and_then(Value::as_u64),
        ) else {
            continue;
        };
        if ids.iter().any(|i| i == id) {
            out.insert(id.to_owned(), dp.min(6) as usize);
        }
    }
    out
}

fn jitter(max: Duration) -> Duration {
    let mut b = [0u8; 2];
    hd_core::rand::fill(&mut b);
    max.mul_f64(f64::from(u16::from_le_bytes(b)) / f64::from(u16::MAX))
}

/// Keeps a session open: connect, subscribe, forward events, reconnect with
/// exponential backoff (1 s … 60 s, with jitter).
async fn connection_task(
    connector: Arc<Connector>,
    ids: Vec<String>,
    webhook: Option<Zeroizing<String>>,
    tx: mpsc::Sender<SessionEvent>,
) {
    let mut backoff = Duration::from_secs(1);
    loop {
        if tx.send(SessionEvent::Connecting).await.is_err() {
            return;
        }
        match connector.connect().await {
            Ok(session) => {
                let started = Instant::now();
                let mut entities = if ids.is_empty() {
                    None
                } else {
                    match session.subscribe_entities(&ids).await {
                        Ok(s) => Some(s),
                        Err(e) => {
                            tracing::warn!("subscribe_entities failed: {e}");
                            None
                        }
                    }
                };
                let precision = load_precision(&session, &ids).await;
                let mut push = match &webhook {
                    None => None,
                    Some(w) => match session
                        .subscribe(json!({"type": "mobile_app/push_notification_channel", "webhook_id": w.as_str(), "support_confirm": true}))
                        .await
                    {
                        Ok(s) => Some(s),
                        Err(HaError::Ha { code, .. }) if code == "not_found" => {
                            let _ = tx.send(SessionEvent::RegistrationGone).await;
                            None
                        }
                        Err(e) => {
                            tracing::warn!("could not open the notification channel: {e}");
                            None
                        }
                    },
                };
                if tx
                    .send(SessionEvent::Connected {
                        session: session.clone(),
                        precision,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                loop {
                    tokio::select! {
                        ev = next_opt(&mut entities) => match ev {
                            Some(v) => if tx.send(SessionEvent::Entities(v)).await.is_err() { return },
                            None => break,
                        },
                        ev = next_opt(&mut push) => match ev {
                            Some(v) => {
                                // Confirm within 10 s, or Home Assistant closes the channel.
                                if let (Some(id), Some(w)) = (v.get("hass_confirm_id").and_then(Value::as_str), &webhook) {
                                    session.send(json!({"type": "mobile_app/push_notification_confirm", "webhook_id": w.as_str(), "confirm_id": id}));
                                }
                                if tx.send(SessionEvent::Push(v)).await.is_err() { return }
                            }
                            None => break,
                        },
                        _ = session.closed() => break,
                    }
                }
                if tx.send(SessionEvent::Disconnected(None)).await.is_err() {
                    return;
                }
                if started.elapsed() > Duration::from_secs(60) {
                    backoff = Duration::from_secs(1);
                }
            }
            Err(e @ HaError::Auth(_)) => {
                let _ = tx.send(SessionEvent::AuthFailed(e)).await;
                return;
            }
            Err(e) => {
                if tx.send(SessionEvent::Disconnected(Some(e))).await.is_err() {
                    return;
                }
            }
        }
        tokio::time::sleep(backoff + jitter(backoff / 4)).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}
