//! Compiles the gettext catalogues in `po/*.po` into a lookup function, so
//! translations work without installed `.mo` files (AppImage, Nix, Flatpak
//! and `cargo run` alike).

use std::fmt::Write as _;
use std::path::PathBuf;

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(o) => {
                    out.push('\\');
                    out.push(o);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn quoted(line: &str) -> Option<String> {
    let l = line.trim();
    let inner = l.strip_prefix('"')?.strip_suffix('"')?;
    Some(unescape(inner))
}

/// Returns (msgid, msgstr) pairs, skipping the header, fuzzy, plural and
/// untranslated entries.
fn parse_po(text: &str) -> Vec<(String, String)> {
    #[derive(PartialEq)]
    enum Field {
        None,
        Id,
        Str,
        Other,
    }
    let mut out = Vec::new();
    let (mut id, mut st, mut fuzzy, mut skip) = (String::new(), String::new(), false, false);
    let mut field = Field::None;
    let mut flush = |id: &mut String, st: &mut String, fuzzy: &mut bool, skip: &mut bool| {
        if !id.is_empty() && !st.is_empty() && !*fuzzy && !*skip {
            out.push((std::mem::take(id), std::mem::take(st)));
        }
        id.clear();
        st.clear();
        *fuzzy = false;
        *skip = false;
    };
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            flush(&mut id, &mut st, &mut fuzzy, &mut skip);
            field = Field::None;
        } else if t.starts_with("#,") {
            if field != Field::None {
                flush(&mut id, &mut st, &mut fuzzy, &mut skip);
                field = Field::None;
            }
            fuzzy |= t.contains("fuzzy");
        } else if t.starts_with('#') {
        } else if let Some(rest) = t.strip_prefix("msgid ") {
            if field != Field::None {
                flush(&mut id, &mut st, &mut fuzzy, &mut skip);
            }
            id = quoted(rest).unwrap_or_default();
            field = Field::Id;
        } else if let Some(rest) = t.strip_prefix("msgstr ") {
            st = quoted(rest).unwrap_or_default();
            field = Field::Str;
        } else if t.starts_with("msgid_plural")
            || t.starts_with("msgstr[")
            || t.starts_with("msgctxt")
        {
            skip = true;
            field = Field::Other;
        } else if t.starts_with('"') {
            let q = quoted(t).unwrap_or_default();
            match field {
                Field::Id => id.push_str(&q),
                Field::Str => st.push_str(&q),
                _ => {}
            }
        }
    }
    flush(&mut id, &mut st, &mut fuzzy, &mut skip);
    out
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let po_dir = manifest.join("../../po");
    println!("cargo:rerun-if-changed={}", po_dir.display());

    let mut langs: Vec<(String, Vec<(String, String)>)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&po_dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "po") {
                println!("cargo:rerun-if-changed={}", p.display());
                let lang = p.file_stem().unwrap().to_string_lossy().into_owned();
                let text = std::fs::read_to_string(&p).unwrap();
                langs.push((lang, parse_po(&text)));
            }
        }
    }
    langs.sort();

    let mut code = String::new();
    let names: Vec<String> = langs.iter().map(|(l, _)| format!("{l:?}")).collect();
    writeln!(code, "/// Languages with a translation catalogue.").unwrap();
    writeln!(
        code,
        "pub const LANGUAGES: &[&str] = &[{}];",
        names.join(", ")
    )
    .unwrap();
    writeln!(
        code,
        "#[allow(clippy::match_single_binding, clippy::match_like_matches_macro)]"
    )
    .unwrap();
    writeln!(
        code,
        "fn lookup(lang: &str, id: &str) -> Option<&'static str> {{"
    )
    .unwrap();
    writeln!(code, "    match lang {{").unwrap();
    for (lang, entries) in &langs {
        writeln!(code, "        {lang:?} => match id {{").unwrap();
        let mut seen = std::collections::HashSet::new();
        for (id, st) in entries {
            if seen.insert(id.clone()) {
                writeln!(code, "            {id:?} => Some({st:?}),").unwrap();
            }
        }
        writeln!(code, "            _ => None,\n        }},").unwrap();
    }
    writeln!(code, "        _ => None,\n    }}\n}}").unwrap();
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("translations.rs");
    std::fs::write(out, code).unwrap();
}
