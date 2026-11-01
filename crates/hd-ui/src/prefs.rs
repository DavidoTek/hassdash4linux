//! Preferences (autostart, notifications, companion device) and disconnecting.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use hd_core::autostart::{self, Method};
use hd_core::config::AuthKind;
use hd_core::i18n::{tr, trf};
use hd_core::secrets::{SecretKind, SecretStore};
use hd_ha::Credentials;
use hd_ha::client::save_registration;
use hd_ha::mobile_app::{self, DeviceInfo};

use crate::rt::on_tokio;
use crate::ui::Ui;

/// The service name Home Assistant creates for the device (`notify.mobile_app_<slug>`).
fn notify_service(device_name: &str) -> String {
    let mut slug = String::new();
    for c in device_name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    format!("notify.mobile_app_{}", slug.trim_matches('_'))
}

pub fn push(ui: &Rc<Ui>) {
    let Some(mw) = ui.window.borrow().clone() else {
        return;
    };
    let cfg = ui.config.borrow().clone();
    let page = adw::PreferencesPage::new();

    // General.
    let general = adw::PreferencesGroup::builder()
        .title(tr("General"))
        .build();
    let autostart_row = adw::SwitchRow::builder()
        .title(tr("Start on login"))
        .subtitle(tr("Show the tray icon automatically after you log in"))
        .sensitive(false)
        .build();
    general.add(&autostart_row);
    page.add(&general);

    // Notifications.
    let notifications = adw::PreferencesGroup::builder()
        .title(tr("Notifications"))
        .build();
    let show_ha = adw::SwitchRow::builder()
        .title(tr("Show notifications from Home Assistant"))
        .active(cfg.preferences.notifications)
        .build();
    show_ha.connect_active_notify(|r| {
        let ui = crate::ui::get();
        let v = r.is_active();
        if let Err(e) = ui.update_config(|c| c.preferences.notifications = v) {
            crate::window::toast(&ui, &e);
        }
    });
    let on_failure = adw::SwitchRow::builder()
        .title(tr("Notify when an action fails"))
        .active(cfg.preferences.notify_on_failure)
        .build();
    on_failure.connect_active_notify(|r| {
        let ui = crate::ui::get();
        let v = r.is_active();
        if let Err(e) = ui.update_config(|c| c.preferences.notify_on_failure = v) {
            crate::window::toast(&ui, &e);
        }
    });
    notifications.add(&show_ha);
    notifications.add(&on_failure);
    page.add(&notifications);

    // Companion device.
    let companion = adw::PreferencesGroup::builder()
        .title(tr("Companion device"))
        .description(tr(
            "Home Assistant sends notifications to this computer through the companion device.",
        ))
        .build();
    let device_name = cfg
        .companion
        .as_ref()
        .map(|c| c.device_name.clone())
        .unwrap_or_else(mobile_app::default_device_name);
    let registered = cfg.companion.as_ref().is_some_and(|c| c.registered);
    let status = adw::ActionRow::builder()
        .title(&device_name)
        .subtitle(if registered {
            trf(
                "Registered · send notifications with {service}",
                &[("service", &notify_service(&device_name))],
            )
        } else {
            tr("Not registered").to_owned()
        })
        .subtitle_selectable(true)
        .build();
    status.set_use_markup(false);
    let register = gtk::Button::builder()
        .label(if registered {
            tr("Register again")
        } else {
            tr("Register")
        })
        .valign(gtk::Align::Center)
        .build();
    status.add_suffix(&register);
    companion.add(&status);
    page.add(&companion);

    // Connection.
    let connection = adw::PreferencesGroup::builder()
        .title(tr("Connection"))
        .build();
    if let Some(c) = &cfg.connection {
        let server = adw::ActionRow::builder()
            .title(tr("Server"))
            .subtitle(&c.url)
            .subtitle_selectable(true)
            .build();
        server.set_use_markup(false);
        connection.add(&server);
        let login = adw::ActionRow::builder()
            .title(tr("Login"))
            .subtitle(match c.auth {
                AuthKind::Oauth => tr("Browser login"),
                AuthKind::LongLivedToken => tr("Long-lived access token"),
            })
            .build();
        connection.add(&login);
        if let Some(pin) = &c.tls_pin {
            let cert = adw::ActionRow::builder()
                .title(tr("Trusted certificate"))
                .subtitle(hd_ha::tls::display_fingerprint(pin))
                .subtitle_selectable(true)
                .build();
            connection.add(&cert);
        }
    }
    let disconnect = gtk::Button::builder()
        .label(tr("Disconnect from Home Assistant…"))
        .css_classes(["destructive-action", "pill"])
        .halign(gtk::Align::Center)
        .margin_top(12)
        .build();
    disconnect.connect_clicked(|_| confirm_disconnect(&crate::ui::get()));
    connection.add(&disconnect);
    page.add(&connection);

    let nav_page = crate::window::nav_page(
        tr("Preferences"),
        "preferences",
        &adw::HeaderBar::new(),
        &page,
    );
    mw.nav.push(&nav_page);

    // Autostart state is detected asynchronously.
    let flatpak_cached = cfg.preferences.flatpak_autostart;
    let row = autostart_row.clone();
    glib::spawn_future_local(async move {
        let (method, enabled) = on_tokio(async move {
            let m = autostart::method().await;
            let e = autostart::is_enabled(&m, flatpak_cached).await;
            (m, e)
        })
        .await;
        row.set_active(enabled);
        if method == Method::NixManaged {
            row.set_subtitle(tr(
                "Managed by your Nix configuration (programs.hassdash4linux.autostart)",
            ));
            return;
        }
        row.set_sensitive(true);
        let busy = Rc::new(std::cell::Cell::new(false));
        row.connect_active_notify(move |r| {
            if busy.get() {
                return;
            }
            let want = r.is_active();
            let m = method.clone();
            let r = r.clone();
            let busy = busy.clone();
            glib::spawn_future_local(async move {
                let result = on_tokio(async move { autostart::set_enabled(&m, want).await }).await;
                let ui = crate::ui::get();
                busy.set(true);
                match result {
                    Ok(state) => {
                        r.set_active(state);
                        if hd_core::is_flatpak() {
                            let _ = ui.update_config(|c| c.preferences.flatpak_autostart = state);
                        }
                    }
                    Err(e) => {
                        r.set_active(!want);
                        crate::window::toast(&ui, &e.to_string());
                    }
                }
                busy.set(false);
            });
        });
    });

    let status2 = status.clone();
    register.connect_clicked(move |b| {
        let ui = crate::ui::get();
        b.set_sensitive(false);
        let b = b.clone();
        let status = status2.clone();
        glib::spawn_future_local(async move {
            match register_device(&ui).await {
                Ok(name) => {
                    status.set_subtitle(&trf(
                        "Registered · send notifications with {service}",
                        &[("service", &notify_service(&name))],
                    ));
                    crate::window::toast(&ui, tr("Registered as a companion device."));
                }
                Err(e) => crate::window::toast(&ui, &e),
            }
            b.set_sensitive(true);
        });
    });
}

/// (Re-)register the companion device. Re-registering with the same device ID
/// replaces the old registration in Home Assistant.
async fn register_device(ui: &Rc<Ui>) -> Result<String, String> {
    let cfg = ui.config.borrow().clone();
    let conn = cfg.connection.clone().ok_or("not connected")?;
    let companion = cfg
        .companion
        .clone()
        .unwrap_or_else(|| hd_core::config::Companion {
            device_id: hd_core::rand::uuid_v4(),
            device_name: mobile_app::default_device_name(),
            registered: false,
        });
    let link = ui.link().await.map_err(|e| e.to_string())?;
    let c2 = companion.clone();
    on_tokio(async move {
        let token = link.connector.access_token().await?;
        let info = DeviceInfo::collect(&c2.device_id, &c2.device_name);
        let reg =
            mobile_app::register(&link.connector.http, &link.connector.base, &token, &info).await?;
        let store = SecretStore::open().await?;
        save_registration(&conn.url, &store, &reg).await?;
        Ok::<_, hd_ha::HaError>(())
    })
    .await
    .map_err(|e| e.to_string())?;
    let name = companion.device_name.clone();
    ui.update_config(|c| {
        c.companion = Some(hd_core::config::Companion {
            registered: true,
            ..companion
        });
    })?;
    Ok(name)
}

/// Ask for confirmation, then log out and forget the connection.
pub fn confirm_disconnect(ui: &Rc<Ui>) {
    let cfg = ui.config.borrow().clone();
    if cfg.connection.is_none() {
        return;
    }
    let device = cfg
        .companion
        .as_ref()
        .filter(|c| c.registered)
        .map(|c| c.device_name.clone());
    let mut body =
        tr("hassdash4linux logs out and deletes the login data from the keyring.").to_owned();
    if let Some(d) = &device {
        body.push_str("\n\n");
        body.push_str(&trf(
            "Remove the device “{device}” in Home Assistant under Settings → Devices & services → Mobile App. hassdash4linux cannot do this without administrator rights.",
            &[("device", d)],
        ));
    }
    let dialog = adw::AlertDialog::new(Some(tr("Disconnect from Home Assistant?")), Some(&body));
    let wipe = gtk::CheckButton::with_label(tr("Also remove the tray menu entries"));
    dialog.set_extra_child(Some(&wipe));
    dialog.add_responses(&[("cancel", tr("Cancel")), ("disconnect", tr("Disconnect"))]);
    dialog.set_response_appearance("disconnect", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, resp| {
        if resp != "disconnect" {
            return;
        }
        let wipe_entries = wipe.is_active();
        let ui = crate::ui::get();
        glib::spawn_future_local(async move { disconnect(&ui, wipe_entries).await });
    });
    dialog.present(crate::window::parent(ui).as_ref());
}

async fn disconnect(ui: &Rc<Ui>, wipe_entries: bool) {
    let cfg = ui.config.borrow().clone();
    let Some(conn) = cfg.connection.clone() else {
        return;
    };
    // Revoke the refresh token (best effort) and delete all secrets.
    let link = ui.link().await.ok();
    let url = conn.url.clone();
    let res = on_tokio(async move {
        if let Some(link) = link
            && let Credentials::OAuth { refresh_token, .. } = link.connector.credentials()
            && let Err(e) =
                hd_ha::auth::revoke(&link.connector.http, &link.connector.base, refresh_token).await
        {
            tracing::warn!("could not revoke the login: {e}");
        }
        let store = SecretStore::open().await?;
        for kind in SecretKind::ALL {
            store.delete(&url, kind).await?;
        }
        Ok::<_, hd_core::secrets::SecretError>(())
    })
    .await;
    if let Err(e) = res {
        crate::window::toast(ui, &e.to_string());
    }
    let res = ui.update_config(|c| {
        c.connection = None;
        // Keep the device ID: registering again replaces the old device in
        // Home Assistant instead of creating a duplicate.
        if let Some(comp) = &mut c.companion {
            comp.registered = false;
        }
        if wipe_entries {
            c.entries.clear();
        }
    });
    if let Err(e) = res {
        crate::window::toast(ui, &e);
    }
    ui.set_link(None);
    crate::window::show_root(ui);
    crate::window::toast(ui, tr("Disconnected from Home Assistant."));
}

#[cfg(test)]
mod tests {
    #[test]
    fn service_names() {
        assert_eq!(
            super::notify_service("David's Laptop"),
            "notify.mobile_app_david_s_laptop"
        );
        assert_eq!(super::notify_service("fedora"), "notify.mobile_app_fedora");
    }
}
