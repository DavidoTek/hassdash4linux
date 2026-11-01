//! Shared UI state: configuration, the Home Assistant link and open windows.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use hd_core::config::Config;
use hd_core::entry::EntryKind;
use hd_core::secrets::SecretStore;
use hd_ha::{Connector, HaError, Session};

use crate::rt::on_tokio;
use crate::window::MainWindow;

/// Connection to Home Assistant for the lifetime of the UI process.
pub struct HaLink {
    pub connector: Arc<Connector>,
    session: tokio::sync::Mutex<Option<Session>>,
}

impl HaLink {
    pub fn new(connector: Arc<Connector>, session: Option<Session>) -> Arc<Self> {
        Arc::new(Self {
            connector,
            session: tokio::sync::Mutex::new(session),
        })
    }

    /// The current session, reconnecting if needed. Must run on tokio.
    pub async fn session(&self) -> Result<Session, HaError> {
        let mut guard = self.session.lock().await;
        if let Some(s) = guard.as_ref().filter(|s| !s.is_closed()) {
            return Ok(s.clone());
        }
        let s = self.connector.connect().await?;
        *guard = Some(s.clone());
        Ok(s)
    }
}

pub struct Ui {
    pub app: adw::Application,
    pub config: RefCell<Config>,
    /// Set when the configuration file could not be loaded.
    pub config_error: RefCell<Option<String>>,
    link: RefCell<Option<Arc<HaLink>>>,
    pub window: RefCell<Option<MainWindow>>,
    pub entity_windows: RefCell<HashMap<String, adw::ApplicationWindow>>,
    pub no_tray_host: Cell<bool>,
}

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

pub fn init(app: &adw::Application) {
    let (config, error) = match Config::load() {
        Ok(c) => (c, None),
        Err(e) => {
            tracing::error!("{e}");
            (Config::default(), Some(e.to_string()))
        }
    };
    let ui = Rc::new(Ui {
        app: app.clone(),
        config: RefCell::new(config),
        config_error: RefCell::new(error),
        link: RefCell::new(None),
        window: RefCell::new(None),
        entity_windows: RefCell::new(HashMap::new()),
        no_tray_host: Cell::new(false),
    });
    UI.with(|u| *u.borrow_mut() = Some(ui));
}

/// The UI state (only valid after `startup`).
pub fn get() -> Rc<Ui> {
    UI.with(|u| u.borrow().clone()).expect("UI not initialised")
}

/// Handle command-line arguments (also those forwarded from a second instance).
pub fn handle_args(args: &[String]) {
    let ui = get();
    let mut entity: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--settings" => {}
            "--no-tray-host" => ui.no_tray_host.set(true),
            "--entity" => entity = it.next().cloned(),
            other => tracing::warn!("ignoring unknown argument {other:?}"),
        }
    }
    match entity {
        Some(id) => {
            // Re-read the configuration: entries may have changed since startup.
            ui.reload_config();
            let allowed = ui
                .config
                .borrow()
                .entries
                .iter()
                .any(|e| e.kind == EntryKind::Entity && e.entity_id == id);
            if allowed {
                crate::entity::open(&ui, &id);
            } else {
                tracing::warn!("refusing to open {id:?}: not a configured light or outlet entry");
                crate::window::present(&ui);
            }
        }
        None => crate::window::present(&ui),
    }
}

impl Ui {
    /// Re-read the configuration from disk (keeps the old one on error).
    pub fn reload_config(&self) {
        match Config::load() {
            Ok(c) => {
                *self.config.borrow_mut() = c;
                *self.config_error.borrow_mut() = None;
            }
            Err(e) => tracing::warn!("{e}"),
        }
    }

    /// Modify, validate and save the configuration, then tell the daemon.
    pub fn update_config(&self, f: impl FnOnce(&mut Config)) -> Result<(), String> {
        let mut c = self.config.borrow().clone();
        f(&mut c);
        c.save().map_err(|e| e.to_string())?;
        *self.config.borrow_mut() = c;
        crate::rt::runtime().spawn(hd_core::ipc::notify_reload());
        Ok(())
    }

    pub fn set_link(&self, link: Option<Arc<HaLink>>) {
        *self.link.borrow_mut() = link;
    }

    /// The Home Assistant link, created from the configuration and keyring on first use.
    pub async fn link(self: &Rc<Self>) -> Result<Arc<HaLink>, HaError> {
        if let Some(l) = self.link.borrow().clone() {
            return Ok(l);
        }
        let cfg = self.config.borrow().clone();
        let connector = on_tokio(async move {
            let store = SecretStore::open().await?;
            Connector::from_config(&cfg, &store).await
        })
        .await?;
        let link = HaLink::new(Arc::new(connector), None);
        *self.link.borrow_mut() = Some(link.clone());
        Ok(link)
    }

    /// The current session (connecting if needed).
    pub async fn session(self: &Rc<Self>) -> Result<Session, HaError> {
        let link = self.link().await?;
        on_tokio(async move { link.session().await }).await
    }
}
