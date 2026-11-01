//! The main window: a navigation view whose root is either the connection
//! screen or the entry list.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use hd_core::i18n::tr;

use crate::ui::Ui;

#[derive(Clone)]
pub struct MainWindow {
    pub window: adw::ApplicationWindow,
    pub nav: adw::NavigationView,
    pub toasts: adw::ToastOverlay,
    pub banner: adw::Banner,
}

/// Show the main window, creating it if needed.
pub fn present(ui: &Rc<Ui>) {
    let existing = ui.window.borrow().clone();
    let mw = match existing {
        Some(mw) => mw,
        None => {
            let mw = build(ui);
            *ui.window.borrow_mut() = Some(mw.clone());
            show_root(ui);
            mw
        }
    };
    mw.banner.set_revealed(ui.no_tray_host.get());
    mw.window.present();
}

fn build(ui: &Rc<Ui>) -> MainWindow {
    let window = adw::ApplicationWindow::builder()
        .application(&ui.app)
        .title("hassdash4linux")
        .default_width(520)
        .default_height(680)
        .width_request(360)
        .height_request(400)
        .build();
    let nav = adw::NavigationView::new();
    let banner = adw::Banner::builder()
        .title(tr("No system tray found. On GNOME, install and enable the “AppIndicator and KStatusNotifierItem Support” extension. The icon appears as soon as a tray is available."))
        .button_label(tr("Dismiss"))
        .build();
    banner.connect_button_clicked(|b| b.set_revealed(false));
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&banner);
    content.append(&nav);
    nav.set_vexpand(true);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&content));
    window.set_content(Some(&toasts));

    // Window actions used by the menus.
    let prefs = gio::SimpleAction::new("preferences", None);
    prefs.connect_activate(|_, _| {
        let ui = crate::ui::get();
        if ui.config.borrow().is_configured() {
            crate::prefs::push(&ui);
        }
    });
    window.add_action(&prefs);
    let disconnect = gio::SimpleAction::new("disconnect", None);
    disconnect.connect_activate(|_, _| crate::prefs::confirm_disconnect(&crate::ui::get()));
    window.add_action(&disconnect);
    let about = gio::SimpleAction::new("about", None);
    about.connect_activate(|_, _| show_about(&crate::ui::get()));
    window.add_action(&about);

    window.connect_close_request(|_| {
        let ui = crate::ui::get();
        ui.window.borrow_mut().take();
        glib::Propagation::Proceed
    });
    MainWindow {
        window,
        nav,
        toasts,
        banner,
    }
}

/// Replace the navigation stack with the right root page.
pub fn show_root(ui: &Rc<Ui>) {
    let Some(mw) = ui.window.borrow().clone() else {
        return;
    };
    let page = if let Some(err) = ui.config_error.borrow().clone() {
        invalid_config_page(&err)
    } else if ui.config.borrow().is_configured() {
        crate::entries::page(ui)
    } else {
        crate::connect::page(ui)
    };
    mw.nav.replace(&[page]);
}

/// Show a short message at the bottom of the main window.
pub fn toast(ui: &Rc<Ui>, msg: &str) {
    if let Some(mw) = ui.window.borrow().as_ref() {
        let t = adw::Toast::new(msg);
        t.set_timeout(4);
        mw.toasts.add_toast(t);
    }
}

/// The main window as dialog parent.
pub fn parent(ui: &Rc<Ui>) -> Option<gtk::Widget> {
    ui.window
        .borrow()
        .as_ref()
        .map(|w| w.window.clone().upcast())
}

/// Header bar with the primary menu.
pub fn header(with_settings: bool) -> adw::HeaderBar {
    let header = adw::HeaderBar::new();
    let menu = gio::Menu::new();
    if with_settings {
        menu.append(Some(tr("Preferences")), Some("win.preferences"));
        menu.append(
            Some(tr("Disconnect from Home Assistant…")),
            Some("win.disconnect"),
        );
    }
    menu.append(Some(tr("About hassdash4linux")), Some("win.about"));
    let button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .primary(true)
        .tooltip_text(tr("Main menu"))
        .build();
    header.pack_end(&button);
    header
}

/// A navigation page with a header bar and scrollable, width-clamped content.
pub fn nav_page(
    title: &str,
    tag: &str,
    header: &adw::HeaderBar,
    content: &impl IsA<gtk::Widget>,
) -> adw::NavigationPage {
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(header);
    toolbar.set_content(Some(content));
    adw::NavigationPage::builder()
        .title(title)
        .tag(tag)
        .child(&toolbar)
        .build()
}

fn invalid_config_page(err: &str) -> adw::NavigationPage {
    let reset = gtk::Button::builder()
        .label(tr("Start over"))
        .halign(gtk::Align::Center)
        .css_classes(["pill", "destructive-action"])
        .build();
    let status = adw::StatusPage::builder()
        .icon_name("dialog-warning-symbolic")
        .title(tr("The configuration file is invalid"))
        .description(format!(
            "{}\n\n{}",
            glib::markup_escape_text(err),
            glib::markup_escape_text(tr("“Start over” renames the file to config.toml.invalid and starts with an empty configuration."))
        ))
        .child(&reset)
        .build();
    reset.connect_clicked(|_| {
        let ui = crate::ui::get();
        let path = hd_core::paths::config_file();
        if let Err(e) = std::fs::rename(&path, path.with_extension("toml.invalid")) {
            toast(&ui, &e.to_string());
            return;
        }
        *ui.config.borrow_mut() = hd_core::config::Config::default();
        *ui.config_error.borrow_mut() = None;
        crate::rt::runtime().spawn(hd_core::ipc::notify_reload());
        show_root(&ui);
    });
    nav_page("hassdash4linux", "invalid", &header(false), &status)
}

fn show_about(ui: &Rc<Ui>) {
    let about = adw::AboutDialog::builder()
        .application_name("hassdash4linux")
        .application_icon(hd_core::APP_ID)
        .version(hd_core::VERSION)
        .comments(tr("Home Assistant in your system tray."))
        .license_type(gtk::License::Gpl30)
        .developer_name("davidotek")
        .website("https://github.com/DavidoTek/hassdash4linux")
        .issue_url("https://github.com/DavidoTek/hassdash4linux/issues")
        .build();
    about.present(parent(ui).as_ref());
}
