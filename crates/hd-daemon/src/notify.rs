//! Desktop notifications via `org.freedesktop.Notifications`.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::sync::mpsc;
use url::Url;
use zbus::zvariant::Value;

use crate::app::Control;
use crate::push::{Notification, escape_markup};
use hd_core::i18n::{tr, trf};

/// At most this many notifications per [`RATE_WINDOW`].
const RATE_LIMIT: usize = 10;
const RATE_WINDOW: Duration = Duration::from_secs(10);

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
    fn close_notification(&self, id: u32) -> zbus::Result<()>;
    fn get_capabilities(&self) -> zbus::Result<Vec<String>>;
    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
}

/// What a shown notification needs to remember for its buttons.
pub struct Shown {
    pub tag: Option<String>,
    pub url: Option<Url>,
    pub actions: Vec<crate::push::Action>,
    /// Home Assistant notification (actions are reported back), or a local one.
    pub from_ha: bool,
}

pub struct Notifier {
    proxy: NotificationsProxy<'static>,
    markup: bool,
    actions: bool,
    by_tag: HashMap<String, u32>,
    pub shown: HashMap<u32, Shown>,
    recent: VecDeque<Instant>,
    suppressed: u32,
    suppressed_id: u32,
    image: Option<(i32, i32, Vec<u8>)>,
}

impl Notifier {
    /// Connect to the notification server and forward its signals as [`Control`] messages.
    pub async fn new(
        conn: &zbus::Connection,
        tx: mpsc::UnboundedSender<Control>,
    ) -> zbus::Result<Self> {
        let proxy = NotificationsProxy::new(conn).await?;
        let caps = proxy.get_capabilities().await.unwrap_or_default();
        let mut invoked = proxy.receive_action_invoked().await?;
        let mut closed = proxy.receive_notification_closed().await?;
        let tx2 = tx.clone();
        tokio::spawn(async move {
            while let Some(sig) = invoked.next().await {
                if let Ok(a) = sig.args() {
                    let _ = tx2.send(Control::NotificationAction {
                        id: a.id,
                        action: a.action_key.clone(),
                    });
                }
            }
        });
        tokio::spawn(async move {
            while let Some(sig) = closed.next().await {
                if let Ok(a) = sig.args() {
                    let _ = tx.send(Control::NotificationClosed(a.id));
                }
            }
        });
        // Outside Flatpak and without an installed desktop file (AppImage),
        // the server cannot find our icon by name, so send it as image data.
        let image = hd_core::appimage_path()
            .is_some()
            .then(crate::icons::notification_image)
            .flatten();
        Ok(Self {
            proxy,
            markup: caps.iter().any(|c| c == "body-markup"),
            actions: caps.iter().any(|c| c == "actions"),
            by_tag: HashMap::new(),
            shown: HashMap::new(),
            recent: VecDeque::new(),
            suppressed: 0,
            suppressed_id: 0,
            image,
        })
    }

    fn hints(&self, urgency: u8) -> HashMap<&'static str, Value<'static>> {
        let mut h = HashMap::new();
        h.insert("desktop-entry", Value::from(hd_core::APP_ID));
        h.insert("urgency", Value::from(urgency));
        if let Some((w, hgt, data)) = &self.image {
            h.insert(
                "image-data",
                Value::from((*w, *hgt, w * 4, true, 8i32, 4i32, data.clone())),
            );
        }
        h
    }

    fn body(&self, s: &str) -> String {
        if self.markup {
            escape_markup(s)
        } else {
            s.to_owned()
        }
    }

    /// Returns false if the rate limit was hit.
    fn allow(&mut self) -> bool {
        let now = Instant::now();
        while self
            .recent
            .front()
            .is_some_and(|t| now.duration_since(*t) > RATE_WINDOW)
        {
            self.recent.pop_front();
        }
        if self.recent.len() >= RATE_LIMIT {
            return false;
        }
        self.recent.push_back(now);
        true
    }

    /// Show a notification from Home Assistant.
    pub async fn show_ha(&mut self, n: Notification) {
        if !self.allow() {
            self.suppressed += 1;
            let msg = trf(
                "{count} more notifications from Home Assistant were not shown.",
                &[("count", &self.suppressed.to_string())],
            );
            let body = self.body(&msg);
            if let Ok(id) = self
                .proxy
                .notify(
                    hd_core::APP_NAME,
                    self.suppressed_id,
                    hd_core::APP_ID,
                    "Home Assistant",
                    &body,
                    &[],
                    self.hints(1),
                    -1,
                )
                .await
            {
                self.suppressed_id = id;
            }
            return;
        }
        self.suppressed = 0;
        let replaces = n
            .tag
            .as_ref()
            .and_then(|t| self.by_tag.get(t))
            .copied()
            .unwrap_or(0);
        let mut actions: Vec<&str> = Vec::new();
        if self.actions {
            if n.url.is_some() {
                actions.extend(["default", tr("Open")]);
            }
            for (i, a) in n.actions.iter().enumerate() {
                let key = ["action0", "action1", "action2"][i.min(2)];
                actions.extend([key, a.title.as_str()]);
            }
        }
        let body = self.body(&n.message);
        match self
            .proxy
            .notify(
                hd_core::APP_NAME,
                replaces,
                hd_core::APP_ID,
                &n.title,
                &body,
                &actions,
                self.hints(1),
                -1,
            )
            .await
        {
            Ok(id) => {
                if let Some(t) = &n.tag {
                    self.by_tag.insert(t.clone(), id);
                }
                if self.shown.len() > 200 {
                    self.shown.clear();
                }
                self.shown.insert(
                    id,
                    Shown {
                        tag: n.tag,
                        url: n.url,
                        actions: n.actions,
                        from_ha: true,
                    },
                );
            }
            Err(e) => tracing::warn!("could not show a notification: {e}"),
        }
    }

    /// Close the notification with this tag.
    pub async fn clear_tag(&mut self, tag: &str) {
        if let Some(id) = self.by_tag.remove(tag) {
            let _ = self.proxy.close_notification(id).await;
            self.shown.remove(&id);
        }
    }

    /// Show a local message (e.g. a failed action).
    pub async fn show_local(&mut self, title: &str, body: &str, critical: bool) {
        if !self.allow() {
            return;
        }
        let body = self.body(body);
        if let Err(e) = self
            .proxy
            .notify(
                hd_core::APP_NAME,
                0,
                hd_core::APP_ID,
                title,
                &body,
                &[],
                self.hints(if critical { 2 } else { 1 }),
                -1,
            )
            .await
        {
            tracing::warn!("could not show a notification: {e}");
        }
    }

    pub fn forget(&mut self, id: u32) {
        if let Some(s) = self.shown.remove(&id)
            && let Some(t) = s.tag
            && self.by_tag.get(&t) == Some(&id)
        {
            self.by_tag.remove(&t);
        }
    }
}

/// Open a URL with the desktop's default handler. Only http(s) URLs are accepted.
pub fn open_url(url: &Url) {
    if !matches!(url.scheme(), "http" | "https") {
        return;
    }
    match tokio::process::Command::new("xdg-open")
        .arg(url.as_str())
        .spawn()
    {
        Ok(mut child) => {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
        }
        Err(e) => tracing::warn!("could not open {url}: {e}"),
    }
}
