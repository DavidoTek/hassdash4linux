//! The main screen: connection status and the list of tray entries.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use hd_core::entry::{Entry, EntryKind, action_service};
use hd_core::i18n::{tr, trf};
use hd_ha::EntityState;

use crate::picker::PickerKind;
use crate::rt::on_tokio;
use crate::ui::Ui;

/// Widgets of the main page that need updating.
struct Page {
    status_row: adw::ActionRow,
    retry: gtk::Button,
    relogin: gtk::Button,
    group: adw::PreferencesGroup,
    rows: RefCell<Vec<gtk::Widget>>,
    /// Entity states (for names), loaded once per connection.
    states: RefCell<HashMap<String, EntityState>>,
}

thread_local! {
    static PAGE: RefCell<Option<Rc<Page>>> = const { RefCell::new(None) };
}

pub fn page(ui: &Rc<Ui>) -> adw::NavigationPage {
    let prefs_page = adw::PreferencesPage::new();

    let status_group = adw::PreferencesGroup::new();
    let status_row = adw::ActionRow::builder()
        .title("Home Assistant")
        .subtitle(tr("Connecting…"))
        .build();
    status_row.add_prefix(&gtk::Image::from_icon_name("network-server-symbolic"));
    let retry = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text(tr("Reconnect"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .visible(false)
        .build();
    retry.update_property(&[gtk::accessible::Property::Label(tr("Reconnect"))]);
    let relogin = gtk::Button::builder()
        .label(tr("Log in again"))
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .visible(false)
        .build();
    status_row.add_suffix(&relogin);
    status_row.add_suffix(&retry);
    status_group.add(&status_row);
    prefs_page.add(&status_group);

    let group = adw::PreferencesGroup::builder()
        .title(tr("Tray menu"))
        .description(tr("These entries appear in this order in the tray menu."))
        .build();
    prefs_page.add(&group);

    let add_group = adw::PreferencesGroup::builder()
        .title(tr("Add to the tray menu"))
        .build();
    let adders: [(&str, &str, Option<PickerKind>); 6] = [
        (
            tr("Automation"),
            "media-playback-start-symbolic",
            Some(PickerKind::Automation),
        ),
        (
            tr("Scene"),
            "applications-graphics-symbolic",
            Some(PickerKind::Scene),
        ),
        (
            tr("Script or button"),
            "system-run-symbolic",
            Some(PickerKind::Script),
        ),
        (
            tr("Light or outlet"),
            "display-brightness-symbolic",
            Some(PickerKind::Entity),
        ),
        (
            tr("Sensor value"),
            "power-profile-balanced-symbolic",
            Some(PickerKind::Sensor),
        ),
        (tr("Separator"), "view-more-horizontal-symbolic", None),
    ];
    for (title, icon, kind) in adders {
        let row = adw::ActionRow::builder()
            .title(title)
            .activatable(true)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name(icon));
        row.add_suffix(&gtk::Image::from_icon_name("list-add-symbolic"));
        row.connect_activated(move |_| {
            let ui = crate::ui::get();
            match kind {
                Some(k) => crate::picker::push(&ui, k),
                None => {
                    if let Err(e) = ui.update_config(|c| c.entries.push(Entry::separator())) {
                        crate::window::toast(&ui, &e);
                    }
                    refresh();
                }
            }
        });
        add_group.add(&row);
    }
    prefs_page.add(&add_group);

    let page = Rc::new(Page {
        status_row,
        retry: retry.clone(),
        relogin: relogin.clone(),
        group,
        rows: RefCell::default(),
        states: RefCell::default(),
    });
    PAGE.with(|p| *p.borrow_mut() = Some(page.clone()));
    retry.connect_clicked(|_| connect(&crate::ui::get()));
    // The login expired or was revoked: show the connection screen with the
    // current address; entries and the device ID are kept.
    relogin.connect_clicked(|_| {
        let ui = crate::ui::get();
        if let Some(mw) = ui.window.borrow().as_ref() {
            mw.nav.push(&crate::connect::page(&ui));
        }
    });

    let title = ui
        .config
        .borrow()
        .connection
        .as_ref()
        .and_then(|c| c.location_name.clone())
        .unwrap_or_else(|| "hassdash4linux".into());
    refresh();
    connect(ui);
    crate::window::nav_page(&title, "main", &crate::window::header(true), &prefs_page)
}

fn current() -> Option<Rc<Page>> {
    PAGE.with(|p| p.borrow().clone())
}

/// Connect (if needed) and load entity names for the list.
pub fn connect(ui: &Rc<Ui>) {
    let Some(page) = current() else { return };
    page.status_row.set_subtitle(tr("Connecting…"));
    page.retry.set_visible(false);
    page.relogin.set_visible(false);
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        let res = async {
            let session = ui.session().await?;
            let version = session.ha_version().to_owned();
            let states = on_tokio(async move { session.get_states().await }).await?;
            Ok::<_, hd_ha::HaError>((version, states))
        }
        .await;
        match res {
            Ok((version, states)) => {
                page.status_row.set_subtitle(&trf(
                    "Connected · version {version}",
                    &[("version", &version)],
                ));
                *page.states.borrow_mut() = states
                    .into_iter()
                    .map(|s| (s.entity_id.clone(), s))
                    .collect();
                refresh();
            }
            Err(e) => {
                page.status_row.set_subtitle(&e.to_string());
                page.retry.set_visible(true);
                page.relogin.set_visible(matches!(e, hd_ha::HaError::Auth(_)));
            }
        }
    });
}

fn icon_for(entry: &Entry) -> &'static str {
    match (entry.kind, entry.domain()) {
        (EntryKind::Separator, _) => "view-more-horizontal-symbolic",
        (_, "automation") => "media-playback-start-symbolic",
        (_, "scene") => "applications-graphics-symbolic",
        (_, "script") => "system-run-symbolic",
        (_, "button" | "input_button") => "input-mouse-symbolic",
        (_, "light") => "display-brightness-symbolic",
        (_, "switch") => "system-shutdown-symbolic",
        (_, "binary_sensor") => "checkbox-checked-symbolic",
        _ => "power-profile-balanced-symbolic",
    }
}

fn icon_button(icon: &str, label: &str) -> gtk::Button {
    let b = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(label)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    b.update_property(&[gtk::accessible::Property::Label(label)]);
    b
}

/// Rebuild the entry rows from the configuration.
pub fn refresh() {
    let Some(page) = current() else { return };
    let ui = crate::ui::get();
    for r in page.rows.borrow_mut().drain(..) {
        page.group.remove(&r);
    }
    let entries = ui.config.borrow().entries.clone();
    let states = page.states.borrow();
    let mut rows = Vec::new();
    if entries.is_empty() {
        let row = adw::ActionRow::builder()
            .title(tr("No entries yet"))
            .subtitle(tr("Add automations, scenes, lights or sensors below."))
            .build();
        page.group.add(&row);
        rows.push(row.upcast());
    }
    let n = entries.len();
    for (i, e) in entries.iter().enumerate() {
        let name = e
            .label
            .clone()
            .or_else(|| states.get(&e.entity_id).map(EntityState::friendly_name));
        let row = adw::ActionRow::new();
        row.set_use_markup(false);
        match e.kind {
            EntryKind::Separator => row.set_title(tr("Separator")),
            _ => {
                row.set_title(name.as_deref().unwrap_or(&e.entity_id));
                row.set_subtitle(&e.entity_id);
                row.set_activatable(true);
                row.connect_activated(move |_| edit(i));
            }
        }
        row.add_prefix(&gtk::Image::from_icon_name(icon_for(e)));

        match e.kind {
            EntryKind::Action => {
                let b = icon_button("media-playback-start-symbolic", tr("Run now"));
                b.connect_clicked(move |_| execute(i));
                row.add_suffix(&b);
            }
            EntryKind::Entity => {
                let b = icon_button("window-new-symbolic", tr("Open controls"));
                b.connect_clicked(move |_| execute(i));
                row.add_suffix(&b);
            }
            EntryKind::Sensor | EntryKind::Separator => {}
        }
        let up = icon_button("go-up-symbolic", tr("Move up"));
        up.set_sensitive(i > 0);
        up.connect_clicked(move |_| move_entry(i, -1));
        let down = icon_button("go-down-symbolic", tr("Move down"));
        down.set_sensitive(i + 1 < n);
        down.connect_clicked(move |_| move_entry(i, 1));
        let remove = icon_button("user-trash-symbolic", tr("Remove"));
        remove.connect_clicked(move |_| remove_entry(i));
        row.add_suffix(&up);
        row.add_suffix(&down);
        row.add_suffix(&remove);
        page.group.add(&row);
        rows.push(row.upcast());
    }
    *page.rows.borrow_mut() = rows;
}

fn move_entry(i: usize, delta: isize) {
    let ui = crate::ui::get();
    let res = ui.update_config(|c| {
        let j = i as isize + delta;
        if j >= 0 && (j as usize) < c.entries.len() && i < c.entries.len() {
            c.entries.swap(i, j as usize);
        }
    });
    if let Err(e) = res {
        crate::window::toast(&ui, &e);
    }
    refresh();
}

fn remove_entry(i: usize) {
    let ui = crate::ui::get();
    let Some(removed) = ui.config.borrow().entries.get(i).cloned() else {
        return;
    };
    if let Err(e) = ui.update_config(|c| {
        c.entries.remove(i);
    }) {
        crate::window::toast(&ui, &e);
        return;
    }
    refresh();
    // Offer to undo.
    if let Some(mw) = ui.window.borrow().as_ref() {
        let toast = adw::Toast::builder()
            .title(tr("Entry removed"))
            .button_label(tr("Undo"))
            .timeout(5)
            .build();
        toast.connect_button_clicked(move |_| {
            let ui = crate::ui::get();
            let entry = removed.clone();
            let _ = ui.update_config(|c| {
                let at = i.min(c.entries.len());
                c.entries.insert(at, entry);
            });
            refresh();
        });
        mw.toasts.add_toast(toast);
    }
}

/// Run an action entry or open an entity window.
fn execute(i: usize) {
    let ui = crate::ui::get();
    let Some(entry) = ui.config.borrow().entries.get(i).cloned() else {
        return;
    };
    match entry.kind {
        EntryKind::Entity => crate::entity::open(&ui, &entry.entity_id),
        EntryKind::Action => {
            let Some(call) = action_service(&entry) else {
                return;
            };
            glib::spawn_future_local(async move {
                let res = async {
                    let session = ui.session().await?;
                    on_tokio(async move {
                        session
                            .call_service(call.domain, call.service, &call.entity_id, call.data)
                            .await
                    })
                    .await
                }
                .await;
                match res {
                    Ok(_) => crate::window::toast(&ui, tr("Done.")),
                    Err(e) => crate::window::toast(&ui, &e.to_string()),
                }
            });
        }
        _ => {}
    }
}

/// Dialog to change an entry's label (and, for automations, whether conditions are skipped).
fn edit(i: usize) {
    let ui = crate::ui::get();
    let Some(entry) = ui.config.borrow().entries.get(i).cloned() else {
        return;
    };
    let page = current();
    let default_name = page
        .as_ref()
        .and_then(|p| {
            p.states
                .borrow()
                .get(&entry.entity_id)
                .map(EntityState::friendly_name)
        })
        .unwrap_or_else(|| entry.entity_id.clone());

    let dialog = adw::AlertDialog::new(Some(tr("Edit entry")), Some(&entry.entity_id));
    let list = gtk::ListBox::builder()
        .css_classes(["boxed-list"])
        .selection_mode(gtk::SelectionMode::None)
        .build();
    let label = adw::EntryRow::builder()
        .title(tr("Label (empty: use the name from Home Assistant)"))
        .build();
    label.set_text(entry.label.as_deref().unwrap_or(""));
    if entry.label.is_none() {
        label.set_tooltip_text(Some(&default_name));
    }
    list.append(&label);
    let skip = adw::SwitchRow::builder()
        .title(tr("Skip conditions"))
        .subtitle(tr(
            "Run the actions even if the automation's conditions are not met",
        ))
        .active(entry.skip_condition.unwrap_or(true))
        .build();
    if entry.domain() == "automation" {
        list.append(&skip);
    }
    dialog.set_extra_child(Some(&list));
    dialog.add_responses(&[("cancel", tr("Cancel")), ("save", tr("Save"))]);
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("save"));
    dialog.set_close_response("cancel");
    let is_automation = entry.domain() == "automation";
    dialog.connect_response(None, move |_, resp| {
        if resp != "save" {
            return;
        }
        let ui = crate::ui::get();
        let text = label.text().trim().to_owned();
        let res = ui.update_config(|c| {
            if let Some(e) = c.entries.get_mut(i) {
                e.label = (!text.is_empty())
                    .then(|| hd_core::truncate_chars(&text, hd_core::entry::MAX_LABEL_CHARS));
                if is_automation {
                    e.skip_condition = (!skip.is_active()).then_some(false);
                }
            }
        });
        if let Err(e) = res {
            crate::window::toast(&ui, &e);
        }
        refresh();
    });
    dialog.present(crate::window::parent(&ui).as_ref());
}
