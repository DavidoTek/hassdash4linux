//! hassdash4linux tray daemon.
//!
//! Runs permanently with a small footprint: no GTK, one Home Assistant
//! WebSocket connection, the tray icon (StatusNotifierItem over D-Bus) and
//! desktop notifications. Windows are opened by starting `hassdash4linux-ui`.

#![forbid(unsafe_code)]

mod app;
mod dbus;
mod icons;
mod notify;
mod push;
mod tray;

use std::io::IsTerminal;
use std::process::ExitCode;

const HELP: &str = "\
hassdash4linux — Home Assistant in your system tray

Usage: hassdash4linux [OPTIONS]

Options:
  --autostart   Started automatically at login (no window unless setup is needed)
  --version     Print the version
  --help        Print this help

If hassdash4linux is already running, starting it again opens the settings.
Set RUST_LOG=debug for verbose logs.";

fn main() -> ExitCode {
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--autostart" => {}
            "--version" | "-V" => {
                println!("hassdash4linux {}", hd_core::VERSION);
                return ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                println!("{HELP}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument {other:?}\n\n{HELP}");
                return ExitCode::from(2);
            }
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,zbus=warn,ksni=warn".into()),
        )
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .init();

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(app::run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::FAILURE
        }
    }
}
