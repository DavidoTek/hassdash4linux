//! Picker screens for adding entries: searchable lists of the instance's
//! automations, scenes, scripts/buttons, lights/outlets and sensors.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::Value;

use hd_core::entry::{Entry, EntryKind};
use hd_core::i18n::{tr, trf};
use hd_ha::EntityState;

use crate::rt::on_tokio;
use crate::ui::Ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Automation,
    Scene,
    Script,
    Entity,
    Sensor,
}

impl PickerKind {
    fn title(self) -> &'static str {
        match self {
            PickerKind::Automation => tr("Add automation"),
            PickerKind::Scene => tr("Add scene"),
            PickerKind::Script => tr("Add script or button"),
            PickerKind::Entity => tr("Add light or outlet"),
            PickerKind::Sensor => tr("Add sensor value"),
        }
    }

    fn domains(self) -> &'static [&'static str] {
        match self {
            PickerKind::Automation => &["automation"],
            PickerKind::Scene => &["scene"],
            PickerKind::Script => &["script", "button", "input_button"],
            PickerKind::Entity => &["light", "switch"],
            PickerKind::Sensor => &["sensor", "binary_sensor"],
        }
    }

    fn entry_kind(self) -> EntryKind {
        match self {
            PickerKind::Automation | PickerKind::Scene | PickerKind::Script => EntryKind::Action,
            PickerKind::Entity => EntryKind::Entity,
            PickerKind::Sensor => EntryKind::Sensor,
        }
    }
}

/// One selectable entity.
#[derive(Debug, Clone)]
struct Item {
    entity_id: String,
    name: String,
    area: String,
    detail: String,
    added: bool,
    /// Lower-case text searched by the filter.
    haystack: String,
}

pub fn push(ui: &Rc<Ui>, kind: PickerKind) {
    let Some(mw) = ui.window.borrow().clone() else {
        return;
    };

    let search = gtk::SearchEntry::builder()
        .placeholder_text(tr("Search by name, area or entity ID"))
        .hexpand(true)
        .build();
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let query: Rc<RefCell<Vec<String>>> = Rc::default();
    let q = query.clone();
    let filter = gtk::CustomFilter::new(move |obj| {
        let Some(b) = obj.downcast_ref::<glib::BoxedAnyObject>() else {
            return false;
        };
        let item = b.borrow::<Item>();
        q.borrow()
            .iter()
            .all(|t| item.haystack.contains(t.as_str()))
    });
    let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
    let selection = gtk::NoSelection::new(Some(filtered.clone()));

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, li| {
        let li = li.downcast_ref::<gtk::ListItem>().expect("ListItem");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_margin_top(8);
        row.set_margin_bottom(8);
        row.set_margin_start(12);
        row.set_margin_end(12);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let subtitle = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label", "caption"])
            .build();
        text.append(&title);
        text.append(&subtitle);
        let added = gtk::Image::from_icon_name("object-select-symbolic");
        added.set_tooltip_text(Some(tr("Already in the tray menu")));
        row.append(&text);
        row.append(&added);
        li.set_child(Some(&row));
    });
    factory.connect_bind(|_, li| {
        let li = li.downcast_ref::<gtk::ListItem>().expect("ListItem");
        let Some(obj) = li.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let item = obj.borrow::<Item>();
        let Some(row) = li.child().and_downcast::<gtk::Box>() else {
            return;
        };
        let text = row
            .first_child()
            .and_downcast::<gtk::Box>()
            .expect("text box");
        let title = text
            .first_child()
            .and_downcast::<gtk::Label>()
            .expect("title");
        let subtitle = title
            .next_sibling()
            .and_downcast::<gtk::Label>()
            .expect("subtitle");
        title.set_text(&item.name);
        let mut sub = vec![];
        if !item.area.is_empty() {
            sub.push(item.area.clone());
        }
        if !item.detail.is_empty() {
            sub.push(item.detail.clone());
        }
        sub.push(item.entity_id.clone());
        subtitle.set_text(&sub.join(" · "));
        if let Some(added) = text.next_sibling() {
            added.set_visible(item.added);
        }
        li.set_activatable(!item.added);
    });

    let list = gtk::ListView::builder()
        .model(&selection)
        .factory(&factory)
        .single_click_activate(true)
        .css_classes(["boxed-list"])
        .build();
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();

    let loading = adw::StatusPage::builder().title(tr("Loading…")).build();
    let spinner = gtk::Spinner::builder()
        .spinning(true)
        .width_request(32)
        .height_request(32)
        .halign(gtk::Align::Center)
        .build();
    loading.set_child(Some(&spinner));
    let empty = adw::StatusPage::builder()
        .icon_name("edit-find-symbolic")
        .title(tr("Nothing found"))
        .build();
    let error = adw::StatusPage::builder()
        .icon_name("network-error-symbolic")
        .title(tr("Could not load the list"))
        .build();

    let stack = gtk::Stack::new();
    stack.add_named(&loading, Some("loading"));
    stack.add_named(&scrolled, Some("list"));
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&error, Some("error"));
    stack.set_visible_child_name("loading");

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&search);
    content.append(&stack);
    let clamp = adw::Clamp::builder()
        .maximum_size(640)
        .child(&content)
        .build();

    let header = adw::HeaderBar::new();
    let page = crate::window::nav_page(kind.title(), "picker", &header, &clamp);

    // Filtering.
    let update_empty = {
        let stack = stack.clone();
        let filtered = filtered.clone();
        move || {
            if stack.visible_child_name().as_deref() == Some("loading")
                || stack.visible_child_name().as_deref() == Some("error")
            {
                return;
            }
            stack.set_visible_child_name(if filtered.n_items() == 0 {
                "empty"
            } else {
                "list"
            });
        }
    };
    let ue = update_empty.clone();
    search.connect_search_changed(move |s| {
        *query.borrow_mut() = s
            .text()
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        filter.changed(gtk::FilterChange::Different);
        ue();
    });

    // Activation adds the entry.
    let nav = mw.nav.clone();
    list.connect_activate(move |lv, pos| {
        let Some(obj) = lv
            .model()
            .and_then(|m| m.item(pos))
            .and_downcast::<glib::BoxedAnyObject>()
        else {
            return;
        };
        let item = obj.borrow::<Item>().clone();
        if item.added {
            return;
        }
        let ui = crate::ui::get();
        let entry = Entry::new(kind.entry_kind(), item.entity_id.clone());
        match ui.update_config(|c| c.entries.push(entry)) {
            Ok(()) => {
                crate::entries::refresh();
                nav.pop();
                crate::window::toast(
                    &ui,
                    &trf("Added “{name}” to the tray menu.", &[("name", &item.name)]),
                );
            }
            Err(e) => crate::window::toast(&ui, &e),
        }
    });

    mw.nav.push(&page);
    search.grab_focus();

    // Load the data.
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        let res = async {
            let session = ui.session().await?;
            on_tokio(async move {
                let states = session.get_states().await?;
                // Registry data is optional (areas); ignore failures.
                let entities = session
                    .entity_registry_display()
                    .await
                    .unwrap_or(Value::Null);
                let areas = session.area_registry().await.unwrap_or(Value::Null);
                let devices = session.device_registry().await.unwrap_or(Value::Null);
                Ok::<_, hd_ha::HaError>((states, entities, areas, devices))
            })
            .await
        }
        .await;
        match res {
            Ok((states, entities, areas, devices)) => {
                let existing: HashSet<String> = ui
                    .config
                    .borrow()
                    .entries
                    .iter()
                    .map(|e| e.entity_id.clone())
                    .collect();
                let items = build_items(kind, &states, &entities, &areas, &devices, &existing);
                for it in items {
                    store.append(&glib::BoxedAnyObject::new(it));
                }
                stack.set_visible_child_name("list");
                update_empty();
            }
            Err(e) => {
                error.set_description(Some(&glib::markup_escape_text(&e.to_string())));
                stack.set_visible_child_name("error");
            }
        }
    });
}

fn build_items(
    kind: PickerKind,
    states: &[EntityState],
    entities: &Value,
    areas: &Value,
    devices: &Value,
    existing: &HashSet<String>,
) -> Vec<Item> {
    let area_names: HashMap<&str, &str> = areas
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| Some((a.get("area_id")?.as_str()?, a.get("name")?.as_str()?)))
        .collect();
    let device_area: HashMap<&str, &str> = devices
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| Some((d.get("id")?.as_str()?, d.get("area_id")?.as_str()?)))
        .collect();
    let mut entity_area: HashMap<&str, &str> = HashMap::new();
    for e in entities
        .get("entities")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = e.get("ei").and_then(Value::as_str) else {
            continue;
        };
        let area = e.get("ai").and_then(Value::as_str).or_else(|| {
            e.get("di")
                .and_then(Value::as_str)
                .and_then(|d| device_area.get(d).copied())
        });
        if let Some(a) = area.and_then(|a| area_names.get(a)) {
            entity_area.insert(id, a);
        }
    }

    let mut items: Vec<Item> = states
        .iter()
        .filter(|s| kind.domains().contains(&s.domain()))
        .map(|s| {
            let name = s.friendly_name();
            let area = entity_area
                .get(s.entity_id.as_str())
                .map(|a| hd_core::truncate_chars(a, 60))
                .unwrap_or_default();
            let detail = match kind {
                PickerKind::Sensor => hd_ha::format::sensor_value(s, None),
                PickerKind::Entity => {
                    if s.domain() == "light" {
                        tr("Light").to_owned()
                    } else {
                        tr("Switch").to_owned()
                    }
                }
                PickerKind::Automation if s.state == "off" => tr("disabled").to_owned(),
                _ => String::new(),
            };
            let haystack = format!("{name} {area} {}", s.entity_id).to_lowercase();
            Item {
                added: existing.contains(&s.entity_id),
                entity_id: s.entity_id.clone(),
                name,
                area,
                detail,
                haystack,
            }
        })
        .collect();
    items.sort_by(|a, b| {
        (
            a.area.is_empty(),
            a.area.to_lowercase(),
            a.name.to_lowercase(),
        )
            .cmp(&(
                b.area.is_empty(),
                b.area.to_lowercase(),
                b.name.to_lowercase(),
            ))
    });
    items
}
