//! Translations. Each language is a flat JSON object of `key -> template`;
//! templates are Telegram HTML with `{placeholder}` arguments.
//!
//! Persian and English are compiled in. More languages (or corrections) can
//! be dropped into `LOCALES_DIR` as `<code>.json` without rebuilding. Groups
//! can override any key for their language (see `custom_texts`).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};

/// The language every other language falls back to for missing keys.
pub const FALLBACK: &str = "en";

const BUILT_IN: &[(&str, &str)] = &[("fa", include_str!("../locales/fa.json")), ("en", include_str!("../locales/en.json"))];

type Catalog = BTreeMap<String, HashMap<String, String>>;

static CATALOG: OnceLock<Catalog> = OnceLock::new();

fn parse(code: &str, json: &str) -> Result<HashMap<String, String>> {
    serde_json::from_str(json).with_context(|| format!("locale `{code}` is not a JSON object of strings"))
}

/// Loads the built-in languages and any `*.json` in `extra_dir`.
pub fn init(extra_dir: Option<&str>) -> Result<()> {
    let mut catalog = Catalog::new();
    for (code, json) in BUILT_IN {
        catalog.insert((*code).to_owned(), parse(code, json)?);
    }
    if let Some(dir) = extra_dir {
        for entry in std::fs::read_dir(dir).with_context(|| format!("cannot read LOCALES_DIR {dir}"))? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(code) = path.file_stem().and_then(|s| s.to_str()).map(str::to_lowercase) else { continue };
            let json = std::fs::read_to_string(&path)?;
            let texts = parse(&code, &json)?;
            catalog.entry(code).or_default().extend(texts);
            log::info!("loaded locale {}", path.display());
        }
    }
    let _ = CATALOG.set(catalog);
    Ok(())
}

fn catalog() -> &'static Catalog {
    CATALOG.get_or_init(|| {
        BUILT_IN.iter().map(|(code, json)| ((*code).to_owned(), parse(code, json).expect("built-in locale is valid"))).collect()
    })
}

/// Available languages: (code, native name).
pub fn languages() -> Vec<(String, String)> {
    catalog().iter().map(|(code, texts)| (code.clone(), texts.get("_name").cloned().unwrap_or_else(|| code.clone()))).collect()
}

pub fn is_language(code: &str) -> bool {
    catalog().contains_key(code)
}

/// Every key of the fallback language, for listing overridable texts.
pub fn keys() -> Vec<String> {
    let mut keys: Vec<String> = catalog().get(FALLBACK).map(|t| t.keys().cloned().collect()).unwrap_or_default();
    keys.retain(|k| !k.starts_with('_'));
    keys.sort();
    keys
}

pub fn has_key(key: &str) -> bool {
    catalog().get(FALLBACK).is_some_and(|t| t.contains_key(key))
}

/// The texts of one group: its language plus its overrides.
#[derive(Clone)]
pub struct Texts {
    pub lang: String,
    overrides: Arc<HashMap<String, String>>,
}

impl Texts {
    pub fn new(lang: &str, overrides: Arc<HashMap<String, String>>) -> Texts {
        let lang = if is_language(lang) { lang.to_owned() } else { FALLBACK.to_owned() };
        Texts { lang, overrides }
    }

    pub fn plain(lang: &str) -> Texts {
        Texts::new(lang, Arc::default())
    }

    /// The raw template for a key.
    pub fn template(&self, key: &str) -> String {
        if let Some(text) = self.overrides.get(key) {
            return text.clone();
        }
        let catalog = catalog();
        catalog
            .get(&self.lang)
            .and_then(|t| t.get(key))
            .or_else(|| catalog.get(FALLBACK).and_then(|t| t.get(key)))
            .cloned()
            .unwrap_or_else(|| {
                log::warn!("missing translation `{key}`");
                key.to_owned()
            })
    }

    /// A text with its `{name}` placeholders filled in. Values must already be HTML-safe.
    pub fn t(&self, key: &str, args: &[(&str, &str)]) -> String {
        self.localize(fill(&self.template(key), args))
    }

    /// A text without arguments.
    pub fn s(&self, key: &str) -> String {
        self.localize(self.template(key))
    }

    /// Numbers are formatted with Persian digits; other languages get ASCII digits.
    fn localize(&self, text: String) -> String {
        if self.lang == "fa" {
            return text;
        }
        text.chars()
            .map(|c| match c {
                '\u{06F0}'..='\u{06F9}' => char::from(b'0' + (c as u32 - 0x06F0) as u8),
                c => c,
            })
            .collect()
    }
}

pub fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = template.to_owned();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_locales_have_the_same_keys() {
        let catalog = catalog();
        let en: std::collections::BTreeSet<_> = catalog["en"].keys().collect();
        let fa: std::collections::BTreeSet<_> = catalog["fa"].keys().collect();
        let only_en: Vec<_> = en.difference(&fa).collect();
        let only_fa: Vec<_> = fa.difference(&en).collect();
        assert!(only_en.is_empty() && only_fa.is_empty(), "missing in fa: {only_en:?}; missing in en: {only_fa:?}");
    }

    #[test]
    fn placeholders_match_between_languages() {
        let catalog = catalog();
        let placeholders = |text: &str| {
            let mut found: Vec<String> =
                text.split('{').skip(1).filter_map(|part| part.split_once('}').map(|(name, _)| name.to_owned())).collect();
            found.sort();
            found
        };
        for (key, en) in &catalog["en"] {
            let fa = &catalog["fa"][key];
            assert_eq!(placeholders(en), placeholders(fa), "placeholders differ for `{key}`");
        }
    }

    #[test]
    fn every_key_used_in_the_code_exists() {
        fn visit(dir: &std::path::Path, missing: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(&path, missing);
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                for key in used_keys(&source) {
                    if !has_key(&key) {
                        missing.push(format!("{key} ({})", path.display()));
                    }
                }
            }
        }
        let mut missing = Vec::new();
        visit(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")), &mut missing);
        assert!(missing.is_empty(), "keys missing from locales: {missing:#?}");
    }

    #[test]
    fn dynamic_keys_exist() {
        let mut keys: Vec<String> = crate::locks::LOCKS.iter().map(|l| format!("lockname.{}", l.code)).collect();
        keys.extend(["all", "ranked", "owners"].map(|a| format!("answer.audience_{a}")));
        for page in 0..crate::locks::PANEL_PAGES.len() {
            keys.push(format!("ui.locks_title_{page}"));
            keys.push(format!("ui.locks_hint_{page}"));
        }
        // Screens named in settings-change log entries.
        keys.extend(
            [
                "glock", "flood", "limit", "chars", "strict", "captcha", "tabchi", "dup", "betray", "reason", "members", "welcome",
                "fadd", "fjoin", "words", "goodbye",
            ]
            .map(|s| format!("ui.b_{s}")),
        );
        keys.extend(
            ["owners", "mods", "vips", "exempt", "admins", "muted", "banned", "warned", "filters", "answers"]
                .map(|l| format!("panel.list_{l}")),
        );
        keys.extend(["bots", "deleted", "fake", "blocked", "restricted"].map(|c| format!("ui.cl_{c}")));
        for section in ["locks", "protect", "ranks", "punish", "words", "join", "stats", "clean", "tools", "texts"] {
            keys.push(format!("ui.h_{section}"));
            keys.push(format!("help.{section}"));
        }
        keys.extend(
            ["fc", "lc", "ls", "mx", "mn", "cm", "dh", "dl", "bl", "wm", "ac", "fa"].map(|f| format!("ui.v_{f}")),
        );
        // The operator panel (src/admin/).
        for flag in crate::admin::global::FLAGS {
            keys.push(format!("adm.flag_{}", flag.code()));
            keys.push(format!("adm.flagd_{}", flag.code()));
        }
        keys.extend(["broadcast", "leave", "gban", "ungban", "backup", "prune", "sync"].map(|k| format!("adm.job_{k}")));
        keys.extend(["pending", "running", "done", "failed", "cancelled"].map(|s| format!("adm.status_{s}")));
        keys.extend(["link", "sync", "send", "reset", "leave", "block", "unblock"].map(|a| format!("adm.ga_{a}")));
        keys.extend(["recent", "top", "gban"].map(|k| format!("adm.ul_{k}")));
        keys.extend(["a", "i", "b"].map(|f| format!("adm.gf_{f}")));
        keys.extend(["groups", "users", "all", "chat"].map(|t| format!("adm.target_{t}")));
        keys.extend(["e", "w", "i", "d"].map(|l| format!("adm.lv_{l}")));
        keys.extend(["1h", "24h", "7d"].map(|r| format!("adm.range_{r}")));
        keys.extend(["upd", "msg", "cmd", "cb", "del", "pun", "sf", "warn", "err"].map(|m| format!("adm.m_{m}")));
        keys.extend(["group_blocked", "new_groups_closed", "group_too_small"].map(|k| format!("adm.{k}_short")));
        for lang in ["en", "fa"] {
            let missing: Vec<_> = keys.iter().filter(|k| !catalog()[lang].contains_key(*k)).collect();
            assert!(missing.is_empty(), "{lang} lacks {missing:?}");
        }
    }

    #[test]
    fn templates_are_valid_telegram_html() {
        // Every opening tag must be closed, so FormattedText::html never rejects a built-in text.
        for (lang, texts) in catalog() {
            for (key, text) in texts {
                let mut stack = Vec::new();
                let mut rest = text.as_str();
                while let Some(start) = rest.find('<') {
                    let end = rest[start..].find('>').map(|e| start + e).unwrap_or_else(|| panic!("{lang}.{key}: unclosed <"));
                    let tag = &rest[start + 1..end];
                    if let Some(name) = tag.strip_prefix('/') {
                        assert_eq!(stack.pop(), Some(name.to_owned()), "{lang}.{key}: mismatched </{name}>");
                    } else {
                        stack.push(tag.split_whitespace().next().unwrap_or_default().to_owned());
                    }
                    rest = &rest[end + 1..];
                }
                assert!(stack.is_empty(), "{lang}.{key}: unclosed {stack:?}");
            }
        }
    }

    /// String literals that look like translation keys: `"section.name"`.
    fn used_keys(source: &str) -> Vec<String> {
        let bytes = source.as_bytes();
        let mut keys = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != b'"' {
                i += 1;
                continue;
            }
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_lowercase() || bytes[end].is_ascii_digit() || b"_.".contains(&bytes[end])) {
                end += 1;
            }
            if end < bytes.len() && bytes[end] == b'"' && end > start {
                let candidate = &source[start..end];
                let valid = candidate.contains('.')
                    && !candidate.starts_with('.')
                    && !candidate.ends_with('.')
                    && candidate.split('.').next().is_some_and(|section| KEY_SECTIONS.contains(&section));
                if valid {
                    keys.push(candidate.to_owned());
                }
                i = end + 1;
            } else {
                i = start;
            }
        }
        keys
    }

    /// Top-level sections of translation keys.
    const KEY_SECTIONS: &[&str] = &[
        "common", "rank", "punish", "lock", "panel", "user", "warn", "filter", "clean", "fadd", "fjoin", "welcome", "answer",
        "stats", "sec", "fun", "help", "lang", "text", "err", "tabchi", "flood", "limit", "strict", "chars", "glock", "media",
        "report", "captcha", "pv", "list", "log", "guard", "photo", "link", "rules", "pin", "ui", "adm",
    ];
}
