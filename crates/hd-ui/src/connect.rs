//! The connection screen: server address, browser login (OAuth + PKCE) or a
//! long-lived access token, optional companion registration.

use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use serde_json::Value;
use zeroize::Zeroizing;

use hd_core::config::{AuthKind, Companion, Connection};
use hd_core::i18n::{tr, trf};
use hd_core::secrets::{SecretKind, SecretStore};
use hd_ha::client::save_registration;
use hd_ha::mobile_app::{self, DeviceInfo};
use hd_ha::{Connector, Credentials, HaError, Session};

use crate::rt::runtime;
use crate::ui::{HaLink, Ui};

#[derive(Clone)]
enum Method {
    Browser,
    Token(Zeroizing<String>),
}

struct Outcome {
    connection: Connection,
    companion: Companion,
    connector: Arc<Connector>,
    session: Session,
    companion_error: Option<String>,
}

#[derive(Clone)]
struct Widgets {
    url: adw::EntryRow,
    device: adw::EntryRow,
    companion: adw::SwitchRow,
    token: adw::PasswordEntryRow,
    http_warning: gtk::Label,
    login: gtk::Button,
    token_button: gtk::Button,
    cancel: gtk::Button,
    spinner: gtk::Spinner,
    status: gtk::Label,
    status_box: gtk::Box,
    task: Rc<std::cell::RefCell<Option<tokio::task::AbortHandle>>>,
}

pub fn page(ui: &Rc<Ui>) -> adw::NavigationPage {
    let cfg = ui.config.borrow().clone();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(16);
    content.set_margin_end(16);

    let icon = gtk::Image::builder()
        .icon_name(hd_core::APP_ID)
        .pixel_size(96)
        .build();
    let title = gtk::Label::builder()
        .label(tr("Connect to Home Assistant"))
        .css_classes(["title-1"])
        .wrap(true)
        .build();
    let intro = gtk::Label::builder()
        .label(tr(
            "Enter the address of your Home Assistant server, then log in with your browser.",
        ))
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["dim-label"])
        .build();
    content.append(&icon);
    content.append(&title);
    content.append(&intro);

    let group = adw::PreferencesGroup::new();
    let url = adw::EntryRow::builder()
        .title(tr("Server address, e.g. homeassistant.local:8123"))
        .build();
    url.set_input_purpose(gtk::InputPurpose::Url);
    if let Some(c) = &cfg.connection {
        url.set_text(&c.url);
    }
    let device_name = cfg
        .companion
        .as_ref()
        .map(|c| c.device_name.clone())
        .unwrap_or_else(mobile_app::default_device_name);
    let device = adw::EntryRow::builder()
        .title(tr("Device name in Home Assistant"))
        .text(device_name)
        .build();
    let companion = adw::SwitchRow::builder()
        .title(tr("Receive notifications"))
        .subtitle(tr("Registers this computer as a companion device"))
        .active(true)
        .build();
    group.add(&url);
    group.add(&device);
    group.add(&companion);
    content.append(&group);

    let http_warning = gtk::Label::builder()
        .label(tr("This connection is not encrypted. Anyone on your network could read your login data. Use https:// if your server supports it."))
        .wrap(true)
        .css_classes(["warning", "caption"])
        .visible(false)
        .xalign(0.0)
        .build();
    content.append(&http_warning);

    let login = gtk::Button::builder()
        .label(tr("Log in with browser"))
        .css_classes(["suggested-action", "pill"])
        .halign(gtk::Align::Center)
        .build();
    content.append(&login);

    let spinner = gtk::Spinner::builder().visible(false).build();
    let status = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .hexpand(true)
        .build();
    let cancel = gtk::Button::builder()
        .label(tr("Cancel"))
        .visible(false)
        .build();
    let status_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    status_box.set_visible(false);
    status_box.append(&spinner);
    status_box.append(&status);
    status_box.append(&cancel);
    content.append(&status_box);

    let token_group = adw::PreferencesGroup::builder()
        .title(tr("Alternative: access token"))
        .description(tr(
            "Create a long-lived access token in Home Assistant under Profile → Security.",
        ))
        .build();
    let token = adw::PasswordEntryRow::builder()
        .title(tr("Long-lived access token"))
        .build();
    let token_button = gtk::Button::builder()
        .label(tr("Connect"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    token.add_suffix(&token_button);
    token_group.add(&token);
    content.append(&token_group);

    let hint = gtk::Label::builder()
        .label(tr("Tip: for the best security, log in with a dedicated Home Assistant user without administrator rights. hassdash4linux does not need them."))
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .build();
    content.append(&hint);

    let w = Widgets {
        url: url.clone(),
        device,
        companion,
        token,
        http_warning,
        login: login.clone(),
        token_button: token_button.clone(),
        cancel: cancel.clone(),
        spinner,
        status,
        status_box: status_box.clone(),
        task: Rc::default(),
    };

    let w2 = w.clone();
    url.connect_changed(move |e| {
        let insecure = hd_core::server::normalize(&e.text()).is_ok_and(|u| u.scheme() == "http");
        w2.http_warning.set_visible(insecure);
    });
    w.http_warning.set_visible(
        hd_core::server::normalize(&url.text()).is_ok_and(|u| u.scheme() == "http"),
    );
    let w2 = w.clone();
    login.connect_clicked(move |_| start(&w2, Method::Browser, None));
    let w2 = w.clone();
    token_button.connect_clicked(move |_| {
        let t = w2.token.text().trim().to_owned();
        if t.is_empty() {
            set_status(&w2, false, tr("Paste a long-lived access token first."));
            return;
        }
        start(&w2, Method::Token(Zeroizing::new(t)), None);
    });
    let w2 = w.clone();
    w.token
        .connect_entry_activated(move |_| w2.token_button.emit_clicked());
    let w2 = w.clone();
    cancel.connect_clicked(move |_| {
        if let Some(t) = w2.task.borrow_mut().take() {
            t.abort();
        }
        busy(&w2, false);
        set_status(&w2, false, tr("Cancelled."));
    });

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    scrolled.set_child(Some(
        &adw::Clamp::builder()
            .maximum_size(520)
            .child(&content)
            .build(),
    ));
    let _ = ui;
    crate::window::nav_page(
        "hassdash4linux",
        "connect",
        &crate::window::header(false),
        &scrolled,
    )
}

fn busy(w: &Widgets, on: bool) {
    w.status_box.set_visible(true);
    w.spinner.set_spinning(on);
    w.spinner.set_visible(on);
    w.login.set_sensitive(!on);
    w.token_button.set_sensitive(!on);
    w.url.set_sensitive(!on);
    w.device.set_sensitive(!on);
    w.companion.set_sensitive(!on);
    w.cancel.set_visible(on);
}

fn set_status(w: &Widgets, ok: bool, msg: &str) {
    w.status_box.set_visible(true);
    w.status.set_text(msg);
    if ok {
        w.status.remove_css_class("error");
    } else {
        w.status.add_css_class("error");
    }
}

/// Start (or retry, with a pinned certificate) the connection flow.
fn start(w: &Widgets, method: Method, pin: Option<String>) {
    let ui = crate::ui::get();
    let base = match hd_core::server::normalize(&w.url.text()) {
        Ok(u) => u,
        Err(e) => {
            set_status(w, false, &e.to_string());
            return;
        }
    };
    let device_name = w.device.text().trim().to_owned();
    if let Err(e) = hd_core::config::validate_device_name(&device_name) {
        set_status(w, false, &e);
        return;
    }
    let register = w.companion.is_active();
    let device_id = ui
        .config
        .borrow()
        .companion
        .as_ref()
        .map(|c| c.device_id.clone())
        .unwrap_or_else(hd_core::rand::uuid_v4);

    busy(w, true);
    set_status(
        w,
        true,
        match method {
            Method::Browser => tr("Waiting for the login in your browser…"),
            Method::Token(_) => tr("Connecting…"),
        },
    );

    let (tx, rx) = async_channel::bounded::<Result<Outcome, HaError>>(1);
    let m = method.clone();
    let pin2 = pin.clone();
    let handle = runtime().spawn(async move {
        let r = flow(base, m, pin2, device_id, device_name, register).await;
        let _ = tx.send(r).await;
    });
    *w.task.borrow_mut() = Some(handle.abort_handle());

    let w = w.clone();
    glib::spawn_future_local(async move {
        let Ok(result) = rx.recv().await else { return }; // aborted
        w.task.borrow_mut().take();
        busy(&w, false);
        match result {
            Ok(out) => finish(&ui, &w, out),
            Err(HaError::Tls {
                fingerprint: Some(fp),
                ..
            }) if pin.is_none() => ask_trust(&ui, &w, method, fp),
            Err(e) => set_status(&w, false, &e.to_string()),
        }
    });
}

/// Open a URL in the browser from any thread.
fn open_in_browser(url: url::Url) {
    glib::MainContext::default().invoke(move || {
        gtk::UriLauncher::new(url.as_str()).launch(
            None::<&gtk::Window>,
            None::<&gtk::gio::Cancellable>,
            |r| {
                if let Err(e) = r {
                    tracing::warn!("could not open the browser: {e}");
                }
            },
        );
    });
}

/// The whole connection flow; runs on tokio.
async fn flow(
    base: url::Url,
    method: Method,
    pin: Option<String>,
    device_id: String,
    device_name: String,
    register: bool,
) -> Result<Outcome, HaError> {
    let tls = hd_ha::tls::build(pin.as_deref())?;
    let http = hd_ha::http::Http::new(&tls)?;
    hd_ha::auth::check_server(&http, &base).await?;

    let (creds, auth, client_id) = match method {
        Method::Browser => {
            let login = hd_ha::auth::oauth_login(&http, &base, open_in_browser).await?;
            let refresh = login.tokens.refresh_token.clone().unwrap_or_default();
            (
                Credentials::OAuth {
                    client_id: login.client_id.clone(),
                    refresh_token: refresh,
                },
                AuthKind::Oauth,
                Some(login.client_id),
            )
        }
        Method::Token(t) => (Credentials::LongLived(t), AuthKind::LongLivedToken, None),
    };
    let connector = Arc::new(Connector::new(base.clone(), pin.as_deref(), creds)?);
    let session = connector.connect().await?;
    let ha_config = session.get_config().await?;
    let location_name = ha_config
        .get("location_name")
        .and_then(Value::as_str)
        .map(|s| hd_core::truncate_chars(s, 100))
        .filter(|s| !s.is_empty());

    let url = base.as_str().trim_end_matches('/').to_owned();
    let store = SecretStore::open().await?;
    store.delete_all(&url).await?;
    match connector.credentials() {
        Credentials::OAuth { refresh_token, .. } => {
            store
                .set(&url, SecretKind::RefreshToken, refresh_token)
                .await?
        }
        Credentials::LongLived(t) => store.set(&url, SecretKind::LongLivedToken, t).await?,
    }

    let mut companion = Companion {
        device_id,
        device_name,
        registered: false,
    };
    let mut companion_error = None;
    if register {
        let info = DeviceInfo::collect(&companion.device_id, &companion.device_name);
        let token = connector.access_token().await?;
        match mobile_app::register(&connector.http, &base, &token, &info).await {
            Ok(reg) => {
                save_registration(&url, &store, &reg).await?;
                companion.registered = true;
            }
            Err(e) => companion_error = Some(e.to_string()),
        }
    }

    Ok(Outcome {
        connection: Connection {
            url,
            auth,
            client_id,
            tls_pin: pin,
            location_name,
        },
        companion,
        connector,
        session,
        companion_error,
    })
}

fn finish(ui: &Rc<Ui>, w: &Widgets, out: Outcome) {
    let res = ui.update_config(|c| {
        c.connection = Some(out.connection);
        c.companion = Some(out.companion);
    });
    if let Err(e) = res {
        set_status(w, false, &e);
        return;
    }
    ui.set_link(Some(HaLink::new(out.connector, Some(out.session))));
    crate::window::show_root(ui);
    match out.companion_error {
        Some(e) => crate::window::toast(
            ui,
            &trf(
                "Connected, but the device could not be registered for notifications: {error}",
                &[("error", &e)],
            ),
        ),
        None => crate::window::toast(ui, tr("Connected to Home Assistant.")),
    }
}

fn ask_trust(ui: &Rc<Ui>, w: &Widgets, method: Method, fingerprint: String) {
    let body = trf(
        "The server's certificate is not signed by a trusted authority. This is normal for self-signed certificates, but it can also mean that someone is intercepting the connection.\n\nOnly continue if this fingerprint matches the certificate of your server:\n\n{fingerprint}",
        &[(
            "fingerprint",
            &hd_ha::tls::display_fingerprint(&fingerprint),
        )],
    );
    let dialog = adw::AlertDialog::new(Some(tr("Untrusted certificate")), Some(&body));
    dialog.add_responses(&[
        ("cancel", tr("Cancel")),
        ("trust", tr("Trust this certificate")),
    ]);
    dialog.set_response_appearance("trust", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let w = w.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "trust" {
            start(&w, method.clone(), Some(fingerprint.clone()));
        } else {
            set_status(&w, false, tr("The certificate was not trusted."));
        }
    });
    dialog.present(crate::window::parent(ui).as_ref());
}
