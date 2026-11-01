//! The StatusNotifierItem (tray icon) and its menu.

use tokio::sync::mpsc;

use crate::app::Control;
use crate::icons::{self, TrayIcon};
use hd_core::entry::Entry;
use hd_core::i18n::tr;

/// One line of the user part of the menu.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuLine {
    /// Clickable entry; the index refers to `config.entries`, and `entry` is
    /// the entry shown, so a click on an outdated menu can be detected.
    Item {
        index: usize,
        entry: Entry,
        label: String,
        enabled: bool,
    },
    /// Read-only text (sensors, hints).
    Text(String),
    Separator,
}

pub struct Tray {
    pub tx: mpsc::UnboundedSender<Control>,
    pub icon: TrayIcon,
    /// Optional status line shown at the top (e.g. "Reconnecting…").
    pub status: Option<String>,
    pub lines: Vec<MenuLine>,
}

/// DBusMenu treats `_` as a mnemonic marker; double it to show it literally.
fn escape(s: &str) -> String {
    s.replace('_', "__")
}

impl ksni::Tray for Tray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        hd_core::APP_NAME.into()
    }

    fn title(&self) -> String {
        hd_core::APP_NAME.into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        icons::tray(self.icon)
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: hd_core::APP_NAME.into(),
            // The description may be interpreted as markup by some hosts.
            description: crate::push::escape_markup(
                self.status
                    .as_deref()
                    .unwrap_or(tr("Connected to Home Assistant")),
            ),
            icon_name: String::new(),
            icon_pixmap: Vec::new(),
        }
    }

    fn watcher_online(&self) {
        let _ = self.tx.send(Control::WatcherOnline);
    }

    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        tracing::warn!("StatusNotifierWatcher offline: {reason:?}");
        let _ = self.tx.send(Control::WatcherOffline);
        true // keep running; the watcher may come back
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem};
        let mut items: Vec<MenuItem<Self>> = Vec::new();
        if let Some(status) = &self.status {
            items.push(
                StandardItem {
                    label: escape(status),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
            items.push(MenuItem::Separator);
        }
        for line in &self.lines {
            match line {
                MenuLine::Item {
                    index,
                    entry,
                    label,
                    enabled,
                } => {
                    let index = *index;
                    let entry = entry.clone();
                    items.push(
                        StandardItem {
                            label: escape(label),
                            enabled: *enabled,
                            activate: Box::new(move |t: &mut Self| {
                                let _ = t.tx.send(Control::Activate {
                                    index,
                                    entry: entry.clone(),
                                });
                            }),
                            ..Default::default()
                        }
                        .into(),
                    );
                }
                MenuLine::Text(text) => {
                    items.push(
                        StandardItem {
                            label: escape(text),
                            enabled: false,
                            ..Default::default()
                        }
                        .into(),
                    );
                }
                MenuLine::Separator => items.push(MenuItem::Separator),
            }
        }
        if !self.lines.is_empty() {
            items.push(MenuItem::Separator);
        }
        items.push(
            StandardItem {
                label: tr("Settings").into(),
                icon_name: "emblem-system-symbolic".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Control::ShowSettings);
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: tr("Close hassdash4linux").into(),
                icon_name: "application-exit-symbolic".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Control::Quit);
                }),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn escapes_underscores() {
        assert_eq!(super::escape("a_b__c"), "a__b____c");
    }
}
