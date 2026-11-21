//! CLI-side i18n. Mirrors `wezterm-gui/src/i18n` (same JSON files, embedded
//! via include_str!) so the CLI's table headers and status output match the
//! locale of the running Unterm GUI.
//!
//! Lookup priority:
//!   1. Process-local override set via the `--lang <code>` flag.
//!   2. `~/.unterm/lang.json`.
//!   3. OS locale (macOS / Windows / $LANG / $LC_ALL).
//!   4. `en-US`.

use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

const LOCALE_BUNDLES: &[(&str, &str, &str)] = &[
    (
        "en-US",
        "English",
        include_str!("../../../unterm-services/src/i18n/locales/en.json"),
    ),
    (
        "zh-CN",
        "简体中文",
        include_str!("../../../unterm-services/src/i18n/locales/zh-CN.json"),
    ),
    (
        "zh-TW",
        "繁體中文",
        include_str!("../../../unterm-services/src/i18n/locales/zh-TW.json"),
    ),
    (
        "ja-JP",
        "日本語",
        include_str!("../../../unterm-services/src/i18n/locales/ja.json"),
    ),
    (
        "ko-KR",
        "한국어",
        include_str!("../../../unterm-services/src/i18n/locales/ko.json"),
    ),
    (
        "de-DE",
        "Deutsch",
        include_str!("../../../unterm-services/src/i18n/locales/de.json"),
    ),
    (
        "fr-FR",
        "Français",
        include_str!("../../../unterm-services/src/i18n/locales/fr.json"),
    ),
    (
        "it-IT",
        "Italiano",
        include_str!("../../../unterm-services/src/i18n/locales/it.json"),
    ),
    (
        "hi-IN",
        "हिन्दी",
        include_str!("../../../unterm-services/src/i18n/locales/hi.json"),
    ),
];

const LOCALE_CODES: &[&str] = &[
    "en-US", "zh-CN", "zh-TW", "ja-JP", "ko-KR", "de-DE", "fr-FR", "it-IT", "hi-IN",
];

type Dict = HashMap<String, String>;

fn bundles() -> &'static HashMap<&'static str, Dict> {
    static B: OnceLock<HashMap<&'static str, Dict>> = OnceLock::new();
    B.get_or_init(|| {
        let mut out = HashMap::new();
        for (code, _name, raw) in LOCALE_BUNDLES {
            let mut dict: Dict = HashMap::new();
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(raw) {
                for (k, v) in map {
                    if let Value::String(s) = v {
                        dict.insert(k, s);
                    }
                }
            }
            out.insert(*code, dict);
        }
        out
    })
}

pub fn available_locales() -> &'static [(&'static str, &'static str)] {
    static OUT: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
    OUT.get_or_init(|| {
        LOCALE_BUNDLES
            .iter()
            .map(|(code, name, _)| (*code, *name))
            .collect()
    })
    .as_slice()
}

pub fn canonicalize(code: &str) -> Option<&'static str> {
    for c in LOCALE_CODES {
        if c.eq_ignore_ascii_case(code) {
            return Some(*c);
        }
    }
    map_to_canonical(code)
}

fn map_to_canonical(raw: &str) -> Option<&'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let main = trimmed
        .split(|c: char| c == '.' || c == '@')
        .next()
        .unwrap_or("")
        .replace('_', "-");
    let parts: Vec<&str> = main.split('-').collect();
    let lang = parts
        .first()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let script_or_region = parts.get(1).map(|s| s.to_string()).unwrap_or_default();
    let region = parts.get(2).map(|s| s.to_string()).unwrap_or_default();

    match lang.as_str() {
        "zh" => {
            let combined = format!("{}-{}", script_or_region, region).to_ascii_lowercase();
            if combined.contains("hant")
                || script_or_region.eq_ignore_ascii_case("TW")
                || script_or_region.eq_ignore_ascii_case("HK")
                || script_or_region.eq_ignore_ascii_case("MO")
            {
                Some("zh-TW")
            } else {
                Some("zh-CN")
            }
        }
        "ja" => Some("ja-JP"),
        "ko" => Some("ko-KR"),
        "de" => Some("de-DE"),
        "fr" => Some("fr-FR"),
        "it" => Some("it-IT"),
        "hi" => Some("hi-IN"),
        "en" => Some("en-US"),
        _ => None,
    }
}

fn locale_path() -> PathBuf {
    unterm_protocol::state_path("lang.json")
        .unwrap_or_else(|| PathBuf::from(".unterm").join("lang.json"))
}

fn read_persisted_locale() -> Option<&'static str> {
    let raw = std::fs::read_to_string(locale_path()).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let code = v.get("lang").and_then(Value::as_str)?;
    canonicalize(code)
}

fn detect_os_locale() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("/usr/bin/defaults")
            .args(["read", "NSGlobalDomain", "AppleLanguages"])
            .output()
        {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                for raw in text.split(|c: char| c == ',' || c == '(' || c == ')' || c == '\n') {
                    let cleaned = raw.trim().trim_matches('"').trim();
                    if !cleaned.is_empty() {
                        if let Some(code) = map_to_canonical(cleaned) {
                            return Some(code);
                        }
                    }
                }
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(code) =
            unterm_services::i18n::detect::windows_locale_name().and_then(|name| map_to_canonical(&name))
        {
            return Some(code);
        }
    }
    for var in &["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"] {
        if let Ok(v) = std::env::var(var) {
            if let Some(code) = map_to_canonical(&v) {
                return Some(code);
            }
            for part in v.split(':') {
                if let Some(code) = map_to_canonical(part) {
                    return Some(code);
                }
            }
        }
    }
    None
}

fn current_lock() -> &'static RwLock<&'static str> {
    static CUR: OnceLock<RwLock<&'static str>> = OnceLock::new();
    CUR.get_or_init(|| {
        let initial = read_persisted_locale()
            .or_else(detect_os_locale)
            .unwrap_or("en-US");
        RwLock::new(initial)
    })
}

pub fn current_locale() -> &'static str {
    *current_lock().read().unwrap()
}

/// Persist the locale to `~/.unterm/lang.json` for use by future runs.
pub fn set_locale_persistent(code: &str) -> bool {
    let Some(canon) = canonicalize(code) else {
        return false;
    };
    *current_lock().write().unwrap() = canon;
    let path = locale_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let value = serde_json::json!({"lang": canon});
    if let Ok(text) = serde_json::to_string_pretty(&value) {
        let _ = std::fs::write(path, text);
    }
    true
}

/// Override the locale for this process only — does not write to disk.
pub fn set_locale_transient(code: &str) -> bool {
    let Some(canon) = canonicalize(code) else {
        return false;
    };
    *current_lock().write().unwrap() = canon;
    true
}

pub fn locale_name(code: &str) -> Option<&'static str> {
    LOCALE_BUNDLES
        .iter()
        .find(|(c, _, _)| c.eq_ignore_ascii_case(code))
        .map(|(_, n, _)| *n)
}

pub fn t(key: &str) -> String {
    lookup(current_locale(), key)
}

pub fn t_args(key: &str, args: &[(&str, &str)]) -> String {
    let mut out = lookup(current_locale(), key);
    for (k, v) in args {
        let pat = format!("{{{}}}", k);
        if out.contains(&pat) {
            out = out.replace(&pat, v);
        }
    }
    out
}

fn lookup(locale: &str, key: &str) -> String {
    let bundles = bundles();
    if let Some(dict) = bundles.get(locale) {
        if let Some(v) = dict.get(key) {
            return v.clone();
        }
    }
    if locale != "en-US" {
        if let Some(en) = bundles.get("en-US") {
            if let Some(v) = en.get(key) {
                return v.clone();
            }
        }
    }
    key.to_string()
}
