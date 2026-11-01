//! Minimal gettext-compatible translations.
//!
//! The catalogues in `po/<lang>.po` are compiled into the binary by
//! `build.rs`. Use [`tr`] for plain strings and [`trf`] for strings with
//! `{name}` placeholders. Both take the English text as message ID.

use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/translations.rs"));

static LANG: OnceLock<Option<&'static str>> = OnceLock::new();

/// Pick the first supported language from the usual locale variables.
fn detect() -> Option<&'static str> {
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(l) = std::env::var("LANGUAGE") {
        candidates.extend(l.split(':').map(str::to_owned));
    }
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(var)
            && !v.is_empty()
        {
            candidates.push(v);
            break;
        }
    }
    for c in candidates {
        let code = c.split(['_', '.', '@']).next().unwrap_or("");
        if code == "C" || code == "POSIX" || code == "en" {
            return None;
        }
        if let Some(l) = LANGUAGES.iter().find(|l| **l == code) {
            return Some(l);
        }
    }
    None
}

/// The active UI language, or `None` for English.
pub fn language() -> Option<&'static str> {
    *LANG.get_or_init(detect)
}

/// Translate a message.
pub fn tr(id: &'static str) -> &'static str {
    language().and_then(|l| lookup(l, id)).unwrap_or(id)
}

/// Translate a message and substitute `{name}` placeholders.
pub fn trf(id: &'static str, args: &[(&str, &str)]) -> String {
    let mut s = tr(id).to_owned();
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn substitution() {
        assert_eq!(trf("x {a} y {b}", &[("a", "1"), ("b", "2")]), "x 1 y 2");
    }

    /// Extract the message IDs of every `tr("…")` / `trf("…"` call in the workspace.
    fn source_ids() -> Vec<String> {
        fn walk(dir: &Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs")
                    && p.file_name().is_some_and(|n| n != "i18n.rs")
                {
                    let text = std::fs::read_to_string(&p).unwrap();
                    for pat in ["tr(", "trf("] {
                        let mut rest = text.as_str();
                        while let Some(i) = rest.find(pat) {
                            let before = rest[..i].chars().last();
                            rest = &rest[i + pat.len()..];
                            if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                                continue;
                            }
                            let trimmed = rest.trim_start();
                            let Some(after_quote) = trimmed.strip_prefix('"') else {
                                continue;
                            };
                            rest = after_quote;
                            let mut id = String::new();
                            let mut chars = rest.chars();
                            while let Some(c) = chars.next() {
                                match c {
                                    '"' => break,
                                    '\\' => match chars.next() {
                                        Some('n') => id.push('\n'),
                                        Some(o) => id.push(o),
                                        None => {}
                                    },
                                    c => id.push(c),
                                }
                            }
                            out.push(id);
                        }
                    }
                }
            }
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut ids = Vec::new();
        walk(&root, &mut ids);
        ids.retain(|i| !i.contains("{a}") && !i.starts_with("x {"));
        ids.sort();
        ids.dedup();
        ids
    }

    #[test]
    fn every_message_is_translated() {
        let missing: Vec<String> = source_ids()
            .into_iter()
            .flat_map(|id| {
                LANGUAGES
                    .iter()
                    .filter(|l| lookup(l, &id).is_none())
                    .map(|l| format!("[{l}] {id:?}"))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert!(
            missing.is_empty(),
            "untranslated messages:\n{}",
            missing.join("\n")
        );
    }

    #[test]
    fn placeholders_match() {
        for id in source_ids() {
            for l in LANGUAGES {
                if let Some(t) = lookup(l, &id) {
                    let ph = |s: &str| {
                        let mut v: Vec<String> = s
                            .split('{')
                            .skip(1)
                            .filter_map(|p| p.split_once('}').map(|x| x.0.to_owned()))
                            .collect();
                        v.sort();
                        v
                    };
                    assert_eq!(ph(&id), ph(t), "placeholder mismatch in [{l}] {id:?}");
                }
            }
        }
    }
}
