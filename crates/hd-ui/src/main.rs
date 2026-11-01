//! hassdash4linux settings and entity windows (GTK4 + libadwaita).
//!
//! Started on demand by the tray daemon:
//! * `hassdash4linux-ui --settings [--no-tray-host]` opens the main window,
//! * `hassdash4linux-ui --entity <entity_id>` opens a light/outlet window.
//!
//! GtkApplication uniqueness forwards repeated invocations to the running
//! instance. The process exits when its last window closes.

#![forbid(unsafe_code)]

mod connect;
mod entity;
mod entries;
mod icons;
mod picker;
mod prefs;
mod rt;
mod ui;
mod window;

use std::io::IsTerminal;

use adw::prelude::*;
use gtk::{gio, glib};

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,zbus=warn".into()),
        )
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .init();

    let app = adw::Application::builder()
        .application_id(hd_core::APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.connect_startup(|app| {
        icons::install();
        ui::init(app);
    });
    app.connect_command_line(|_, cmd| {
        let args: Vec<String> = cmd
            .arguments()
            .iter()
            .skip(1)
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        ui::handle_args(&args);
        glib::ExitCode::SUCCESS
    });
    app.connect_activate(|_| ui::handle_args(&[]));
    app.run()
}
