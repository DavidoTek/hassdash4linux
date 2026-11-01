//! Parsing of notifications received over the mobile_app push channel.
//!
//! All content is untrusted: lengths are capped, URLs are restricted to
//! http(s) (or paths relative to the Home Assistant URL) and every `command_*`
//! message is ignored. hassdash4linux never executes anything on behalf of a
//! notification.

use serde_json::Value;
use url::Url;

pub const MAX_TITLE: usize = 256;
pub const MAX_BODY: usize = 4096;
pub const MAX_ACTIONS: usize = 3;

/// A notification button.
#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    /// Key sent back to Home Assistant (`mobile_app_notification_action`).
    pub key: String,
    pub title: String,
    /// For `URI` actions: what to open.
    pub uri: Option<Url>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Push {
    /// Show a notification.
    Show(Notification),
    /// Close the notification with this tag.
    Clear { tag: String },
    /// Ignored (commands and unsupported special messages).
    Ignored(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub title: String,
    pub message: String,
    pub tag: Option<String>,
    pub actions: Vec<Action>,
    /// Opened when the notification itself is clicked.
    pub url: Option<Url>,
}

fn text(v: Option<&Value>, max: usize) -> String {
    let s = match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    };
    // Remove control characters except newlines and tabs.
    let s: String = s
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    hd_core::truncate_chars(s.trim(), max)
}

/// Resolve a URL from a notification: absolute http(s) URLs, or paths
/// (starting with a single `/`) relative to the Home Assistant base URL.
pub fn resolve_url(raw: &str, base: &Url) -> Option<Url> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 2048 || raw == "noAction" {
        return None;
    }
    if raw.starts_with('/') && !raw.starts_with("//") {
        let mut u = base.clone();
        let (path, query) = raw.split_once('?').unwrap_or((raw, ""));
        u.set_path(&format!("{}{}", base.path().trim_end_matches('/'), path));
        u.set_query((!query.is_empty()).then_some(query));
        return Some(u);
    }
    let u = Url::parse(raw).ok()?;
    matches!(u.scheme(), "http" | "https").then_some(u)
}

/// Parse a push channel event.
pub fn parse(ev: &Value, base: &Url) -> Push {
    let data = ev.get("data").and_then(Value::as_object);
    let get = |k: &str| ev.get(k).or_else(|| data.and_then(|d| d.get(k)));
    let message = text(ev.get("message"), MAX_BODY);
    let tag = text(get("tag"), 128);
    let tag = (!tag.is_empty()).then_some(tag);

    if message == "clear_notification" {
        return match tag {
            Some(tag) => Push::Clear { tag },
            None => Push::Ignored("clear_notification without tag".into()),
        };
    }
    const SPECIAL: &[&str] = &[
        "request_location_update",
        "remove_channel",
        "clear_badge",
        "update_complications",
        "update_widgets",
        "TTS",
    ];
    if message.starts_with("command_") || SPECIAL.contains(&message.as_str()) {
        return Push::Ignored(format!("unsupported special message {message:?}"));
    }

    let mut actions = Vec::new();
    if let Some(list) = get("actions").and_then(Value::as_array) {
        for a in list.iter().take(10) {
            if actions.len() == MAX_ACTIONS {
                break;
            }
            let key = text(a.get("action"), 64);
            let title = text(a.get("title"), 40);
            if key.is_empty() || title.is_empty() {
                continue;
            }
            let uri = if key == "URI" {
                a.get("uri")
                    .and_then(Value::as_str)
                    .and_then(|u| resolve_url(u, base))
            } else {
                None
            };
            if key == "URI" && uri.is_none() {
                continue;
            }
            actions.push(Action { key, title, uri });
        }
    }
    let url = get("url")
        .or_else(|| get("clickAction"))
        .and_then(Value::as_str)
        .and_then(|u| resolve_url(u, base));

    let mut title = text(ev.get("title"), MAX_TITLE);
    if title.is_empty() {
        title = "Home Assistant".into();
    }
    Push::Show(Notification {
        title,
        message,
        tag,
        actions,
        url,
    })
}

/// Escape text for notification servers that interpret body markup.
pub fn escape_markup(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> Url {
        Url::parse("https://ha.example.org").unwrap()
    }

    #[test]
    fn simple() {
        let p = parse(
            &json!({"message": "Door open", "title": "Alarm", "hass_confirm_id": "x"}),
            &base(),
        );
        let Push::Show(n) = p else { panic!() };
        assert_eq!(
            (n.title.as_str(), n.message.as_str()),
            ("Alarm", "Door open")
        );
        assert!(n.actions.is_empty() && n.url.is_none() && n.tag.is_none());
    }

    #[test]
    fn data_fields() {
        let ev = json!({"message": "m", "data": {
            "tag": "door",
            "url": "/lovelace/0",
            "actions": [
                {"action": "OPEN", "title": "Open"},
                {"action": "URI", "title": "Cam", "uri": "javascript:alert(1)"},
                {"action": "URI", "title": "Web", "uri": "https://example.org/x"},
                {"action": "", "title": "bad"},
                {"action": "B", "title": "b"},
                {"action": "C", "title": "c"}
            ]
        }});
        let Push::Show(n) = parse(&ev, &base()) else {
            panic!()
        };
        assert_eq!(n.tag.as_deref(), Some("door"));
        assert_eq!(n.url.unwrap().as_str(), "https://ha.example.org/lovelace/0");
        assert_eq!(n.actions.len(), 3);
        assert_eq!(n.actions[0].key, "OPEN");
        assert_eq!(
            n.actions[1].uri.as_ref().unwrap().as_str(),
            "https://example.org/x"
        );
    }

    #[test]
    fn commands_and_clear() {
        assert!(matches!(
            parse(&json!({"message": "command_launch_app"}), &base()),
            Push::Ignored(_)
        ));
        assert!(matches!(
            parse(&json!({"message": "TTS"}), &base()),
            Push::Ignored(_)
        ));
        assert_eq!(
            parse(
                &json!({"message": "clear_notification", "data": {"tag": "t"}}),
                &base()
            ),
            Push::Clear { tag: "t".into() }
        );
    }

    #[test]
    fn urls() {
        assert!(resolve_url("file:///etc/passwd", &base()).is_none());
        assert!(resolve_url("//evil.example/x", &base()).is_none());
        assert!(resolve_url("noAction", &base()).is_none());
        assert_eq!(
            resolve_url("/a?b=c", &base()).unwrap().as_str(),
            "https://ha.example.org/a?b=c"
        );
    }

    #[test]
    fn sanitises_text() {
        let long = "x".repeat(10_000);
        let Push::Show(n) = parse(&json!({"message": long, "title": "a\u{0007}b"}), &base()) else {
            panic!()
        };
        assert!(n.message.chars().count() <= MAX_BODY);
        assert_eq!(n.title, "ab");
        assert_eq!(escape_markup("<b>&"), "&lt;b&gt;&amp;");
    }
}
