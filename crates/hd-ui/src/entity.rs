//! Control windows for lights and switches/outlets, with live state updates.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;
use serde_json::{Map, Value, json};

use hd_core::entry::{domain_of, entity_service_allowed};
use hd_core::i18n::tr;
use hd_ha::{EntityCache, EntityState, format};

use crate::rt::{on_tokio, runtime};
use crate::ui::Ui;

/// Ignore state updates for a control this long after the user moved it.
const USER_GRACE: Duration = Duration::from_millis(1500);
/// Minimum interval between service calls while a slider moves.
const THROTTLE: Duration = Duration::from_millis(150);

const PRESETS: [(&str, [u8; 3]); 8] = [
    ("red", [237, 51, 59]),
    ("orange", [255, 120, 0]),
    ("yellow", [246, 211, 45]),
    ("green", [51, 209, 122]),
    ("cyan", [0, 200, 220]),
    ("blue", [53, 132, 228]),
    ("purple", [145, 65, 172]),
    ("pink", [255, 105, 180]),
];

fn preset_label(name: &str) -> &'static str {
    match name {
        "red" => tr("Red"),
        "orange" => tr("Orange"),
        "yellow" => tr("Yellow"),
        "green" => tr("Green"),
        "cyan" => tr("Cyan"),
        "blue" => tr("Blue"),
        "purple" => tr("Purple"),
        _ => tr("Pink"),
    }
}

struct Controls {
    entity_id: String,
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    banner: adw::Banner,
    spinner_page: adw::StatusPage,
    stack: gtk::Stack,
    power: adw::SwitchRow,
    brightness_row: adw::ActionRow,
    brightness: gtk::Scale,
    temp_row: adw::ActionRow,
    temp: gtk::Scale,
    color_row: adw::ActionRow,
    color: gtk::ColorDialogButton,
    changed_row: adw::ActionRow,
    /// Set while widgets are updated from Home Assistant (suppresses handlers).
    updating: Cell<bool>,
    last_user: RefCell<[Option<Instant>; 4]>,
    pending: RefCell<[Option<Map<String, Value>>; 4]>,
    scheduled: Cell<[bool; 4]>,
    cache: RefCell<EntityCache>,
}

#[derive(Clone, Copy)]
enum Ctl {
    Power = 0,
    Brightness = 1,
    Temp = 2,
    Color = 3,
}

pub fn open(ui: &Rc<Ui>, entity_id: &str) {
    if let Some(w) = ui.entity_windows.borrow().get(entity_id) {
        w.present();
        return;
    }
    let domain = domain_of(entity_id).to_owned();
    let window = adw::ApplicationWindow::builder()
        .application(&ui.app)
        .title(entity_id)
        .default_width(440)
        .default_height(470)
        .width_request(340)
        .resizable(true)
        .build();

    let header = adw::HeaderBar::new();
    let banner = adw::Banner::new(tr("This device is unavailable."));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.add_top_bar(&banner);

    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    let power = adw::SwitchRow::builder().title(tr("Power")).build();
    group.add(&power);

    let brightness = gtk::Scale::with_range(gtk::Orientation::Horizontal, 1.0, 100.0, 1.0);
    brightness.set_hexpand(true);
    brightness.set_width_request(180);
    brightness.set_draw_value(true);
    brightness.set_value_pos(gtk::PositionType::Left);
    brightness.set_format_value_func(|_, v| format!("{v:.0} %"));
    brightness.update_property(&[gtk::accessible::Property::Label(tr("Brightness"))]);
    let brightness_row = adw::ActionRow::builder()
        .title(tr("Brightness"))
        .visible(false)
        .build();
    brightness_row.add_suffix(&brightness);
    group.add(&brightness_row);

    let temp = gtk::Scale::with_range(gtk::Orientation::Horizontal, 2000.0, 6500.0, 50.0);
    temp.set_hexpand(true);
    temp.set_width_request(180);
    temp.set_draw_value(true);
    temp.set_value_pos(gtk::PositionType::Left);
    temp.set_format_value_func(|_, v| format!("{v:.0} K"));
    temp.update_property(&[gtk::accessible::Property::Label(tr("Color temperature"))]);
    temp.add_css_class("hd-temp-scale");
    let temp_row = adw::ActionRow::builder()
        .title(tr("Color temperature"))
        .visible(false)
        .build();
    temp_row.add_suffix(&temp);
    group.add(&temp_row);

    let color =
        gtk::ColorDialogButton::new(Some(gtk::ColorDialog::builder().with_alpha(false).build()));
    color.set_valign(gtk::Align::Center);
    color.update_property(&[gtk::accessible::Property::Label(tr("Color"))]);
    let color_row = adw::ActionRow::builder()
        .title(tr("Color"))
        .visible(false)
        .build();
    let presets = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    presets.set_valign(gtk::Align::Center);
    color_row.add_suffix(&presets);
    color_row.add_suffix(&color);
    group.add(&color_row);

    let changed_row = adw::ActionRow::builder().title(tr("Last changed")).build();
    group.add(&changed_row);
    page.add(&group);

    let spinner = gtk::Spinner::builder()
        .spinning(true)
        .width_request(32)
        .height_request(32)
        .halign(gtk::Align::Center)
        .build();
    let spinner_page = adw::StatusPage::builder()
        .title(tr("Connecting…"))
        .child(&spinner)
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(&spinner_page, Some("loading"));
    stack.add_named(&page, Some("controls"));
    toolbar.set_content(Some(&stack));
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&toolbar));
    window.set_content(Some(&toasts));
    install_css();

    let c = Rc::new(Controls {
        entity_id: entity_id.to_owned(),
        window: window.clone(),
        toasts,
        banner,
        spinner_page,
        stack,
        power,
        brightness_row,
        brightness,
        temp_row,
        temp,
        color_row,
        color,
        changed_row,
        updating: Cell::new(false),
        last_user: RefCell::default(),
        pending: RefCell::default(),
        scheduled: Cell::new([false; 4]),
        cache: RefCell::new(EntityCache::new()),
    });

    // Preset colour swatches (lights only).
    if domain == "light" {
        for (name, rgb) in PRESETS {
            let b = gtk::Button::builder()
                .css_classes(["circular", "hd-swatch", &format!("hd-swatch-{name}")])
                .tooltip_text(preset_label(name))
                .width_request(22)
                .height_request(22)
                .build();
            b.update_property(&[gtk::accessible::Property::Label(preset_label(name))]);
            let c2 = c.clone();
            b.connect_clicked(move |_| {
                user_change(&c2, Ctl::Color, data(&[("rgb_color", json!(rgb))]))
            });
            presets.append(&b);
        }
    }

    let c2 = c.clone();
    c.power.connect_active_notify(move |r| {
        if c2.updating.get() {
            return;
        }
        let service = if r.is_active() { "turn_on" } else { "turn_off" };
        mark_user(&c2, Ctl::Power);
        call(&c2, service, Map::new());
    });
    let c2 = c.clone();
    c.brightness.connect_value_changed(move |s| {
        if !c2.updating.get() {
            user_change(
                &c2,
                Ctl::Brightness,
                data(&[("brightness_pct", json!(s.value().round() as u8))]),
            );
        }
    });
    let c2 = c.clone();
    c.temp.connect_value_changed(move |s| {
        if !c2.updating.get() {
            user_change(
                &c2,
                Ctl::Temp,
                data(&[("color_temp_kelvin", json!(s.value().round() as u32))]),
            );
        }
    });
    let c2 = c.clone();
    c.color.connect_rgba_notify(move |b| {
        if c2.updating.get() {
            return;
        }
        let rgba = b.rgba();
        let rgb = [rgba.red(), rgba.green(), rgba.blue()]
            .map(|x| (x * 255.0).round().clamp(0.0, 255.0) as u8);
        user_change(&c2, Ctl::Color, data(&[("rgb_color", json!(rgb))]));
    });

    // Closing the channel stops the subscription task and the update loop.
    let (tx, rx) = async_channel::bounded::<Value>(64);
    let id = entity_id.to_owned();
    let rx_close = rx.clone();
    window.connect_close_request(move |_| {
        rx_close.close();
        crate::ui::get().entity_windows.borrow_mut().remove(&id);
        glib::Propagation::Proceed
    });
    ui.entity_windows
        .borrow_mut()
        .insert(entity_id.to_owned(), window.clone());
    window.present();

    subscribe(ui, c, tx, rx);
}

fn data(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn install_css() {
    thread_local!(static DONE: Cell<bool> = const { Cell::new(false) });
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    let mut css = String::from(
        ".hd-swatch { min-width: 22px; min-height: 22px; padding: 0; border: 1px solid alpha(currentColor, 0.25); }\n\
         .hd-temp-scale trough { background-image: linear-gradient(to right, #ff9f43, #fff4e0, #cfe3ff); }\n",
    );
    for (name, [r, g, b]) in PRESETS {
        css.push_str(&format!(
            ".hd-swatch-{name} {{ background: rgb({r},{g},{b}); }}\n"
        ));
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn mark_user(c: &Controls, ctl: Ctl) {
    c.last_user.borrow_mut()[ctl as usize] = Some(Instant::now());
}

fn recently_changed(c: &Controls, ctl: Ctl) -> bool {
    c.last_user.borrow()[ctl as usize].is_some_and(|t| t.elapsed() < USER_GRACE)
}

/// A slider or colour change: throttle and send `turn_on` with the data.
fn user_change(c: &Rc<Controls>, ctl: Ctl, d: Map<String, Value>) {
    mark_user(c, ctl);
    c.pending.borrow_mut()[ctl as usize] = Some(d);
    let mut sched = c.scheduled.get();
    if sched[ctl as usize] {
        return;
    }
    sched[ctl as usize] = true;
    c.scheduled.set(sched);
    // Send immediately, then at most every THROTTLE while values keep coming.
    flush(c, ctl);
    let c = c.clone();
    glib::timeout_add_local(THROTTLE, move || {
        if c.pending.borrow()[ctl as usize].is_some() {
            flush(&c, ctl);
            glib::ControlFlow::Continue
        } else {
            let mut s = c.scheduled.get();
            s[ctl as usize] = false;
            c.scheduled.set(s);
            glib::ControlFlow::Break
        }
    });
}

fn flush(c: &Rc<Controls>, ctl: Ctl) {
    if let Some(d) = c.pending.borrow_mut()[ctl as usize].take() {
        call(c, "turn_on", d);
    }
}

fn call(c: &Rc<Controls>, service: &'static str, d: Map<String, Value>) {
    let domain = domain_of(&c.entity_id).to_owned();
    if !entity_service_allowed(&domain, service) {
        return;
    }
    let ui = crate::ui::get();
    let c = c.clone();
    glib::spawn_future_local(async move {
        let id = c.entity_id.clone();
        let res = async {
            let session = ui.session().await?;
            on_tokio(async move { session.call_service(&domain, service, &id, d).await }).await
        }
        .await;
        if let Err(e) = res {
            c.toasts.add_toast(adw::Toast::new(&e.to_string()));
        }
    });
}

/// Subscribe to the entity and keep the widgets up to date while the window lives.
fn subscribe(
    ui: &Rc<Ui>,
    c: Rc<Controls>,
    tx: async_channel::Sender<Value>,
    rx: async_channel::Receiver<Value>,
) {
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        let session = match ui.session().await {
            Ok(s) => s,
            Err(e) => {
                c.spinner_page.set_title(tr("Could not connect"));
                c.spinner_page
                    .set_description(Some(&glib::markup_escape_text(&e.to_string())));
                c.spinner_page.set_child(None::<&gtk::Widget>);
                return;
            }
        };
        let id = c.entity_id.clone();
        runtime().spawn(async move {
            let mut sub = match session.subscribe_entities(&[id]).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("subscribe failed: {e}");
                    return;
                }
            };
            // Ends when the window closes (receiver dropped) or the session ends;
            // dropping the subscription unsubscribes.
            while let Some(ev) = sub.next().await {
                if tx.send(ev).await.is_err() {
                    break;
                }
            }
        });
        drop(ui);
        while let Ok(ev) = rx.recv().await {
            c.cache.borrow_mut().apply(&ev);
            let st = c.cache.borrow().get(&c.entity_id).cloned();
            if let Some(st) = st {
                update(&c, &st);
            }
        }
        if c.window.is_visible() {
            c.banner
                .set_title(tr("The connection to Home Assistant was closed."));
            c.banner.set_revealed(true);
        }
    });
}

fn update(c: &Controls, st: &EntityState) {
    c.updating.set(true);
    c.window.set_title(Some(&st.friendly_name()));
    c.stack.set_visible_child_name("controls");
    let unavailable = st.is_unavailable();
    c.banner.set_title(tr("This device is unavailable."));
    c.banner.set_revealed(unavailable);
    c.power.set_sensitive(!unavailable);
    if !recently_changed(c, Ctl::Power) {
        c.power.set_active(st.is_on());
    }
    c.power.set_subtitle(&format::entity_state(st));

    if st.domain() == "light" {
        let modes: Vec<&str> = st
            .attributes
            .get("supported_color_modes")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let dimmable = modes.iter().any(|m| *m != "onoff");
        let has_temp = modes.contains(&"color_temp");
        let has_color = modes
            .iter()
            .any(|m| matches!(*m, "hs" | "xy" | "rgb" | "rgbw" | "rgbww"));
        let on = st.is_on() && !unavailable;

        c.brightness_row.set_visible(dimmable);
        c.brightness.set_sensitive(on);
        if let Some(p) = format::brightness_percent(st)
            && !recently_changed(c, Ctl::Brightness)
        {
            c.brightness.set_value(f64::from(p.max(1)));
        }

        c.temp_row.set_visible(has_temp);
        c.temp.set_sensitive(on);
        if has_temp {
            let min = st.attr_f64("min_color_temp_kelvin").unwrap_or(2000.0);
            let max = st.attr_f64("max_color_temp_kelvin").unwrap_or(6500.0);
            if min < max {
                c.temp.set_range(min, max);
            }
            if let Some(k) = st.attr_f64("color_temp_kelvin")
                && !recently_changed(c, Ctl::Temp)
            {
                c.temp.set_value(k);
            }
        }

        c.color_row.set_visible(has_color);
        c.color_row.set_sensitive(on);
        if let Some(rgb) = st.attributes.get("rgb_color").and_then(Value::as_array) {
            let v: Vec<f32> = rgb
                .iter()
                .filter_map(Value::as_f64)
                .map(|x| (x / 255.0) as f32)
                .collect();
            if v.len() == 3 && !recently_changed(c, Ctl::Color) {
                c.color
                    .set_rgba(&gtk::gdk::RGBA::new(v[0], v[1], v[2], 1.0));
            }
        }
    }

    match st
        .last_changed
        .and_then(|t| glib::DateTime::from_unix_local(t as i64).ok())
    {
        Some(dt) => c.changed_row.set_subtitle(
            &dt.format("%x %X")
                .map(|s| s.to_string())
                .unwrap_or_default(),
        ),
        None => c.changed_row.set_subtitle("—"),
    }
    c.updating.set(false);
}
