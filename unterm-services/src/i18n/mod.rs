//! Unterm i18n — flat-key string lookup with locale autodetection.
//!
//! Strings live in `src/i18n/locales/*.json`, one canonical English file plus
//! eight translations. All non-English files must share the same key set as
//! `en.json` (the canonical fallback).
//!
//! Public API surface:
//!   * `t(key)`                 — translated string for the current locale,
//!                                falling back to en, then to the key itself.
//!   * `t_args(key, &[...])`    — same but substitutes `{name}` placeholders.
//!   * `current_locale()`       — active code, e.g. "zh-CN".
//!   * `set_locale(code)`       — explicit override; persists to
//!                                `~/.unterm/lang.json`.
//!   * `available_locales()`    — list of `(code, native_name)` pairs.
//!   * `dictionary(code)`       — raw map for the given locale (used by the
//!                                Web Settings server to ship dictionaries to
//!                                the JS frontend).

pub mod detect;

use parking_lot::RwLock;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

/// One canonical language code per supported translation. The first entry is
/// the fallback (English).
pub const LOCALE_CODES: &[&str] = &[
    "en-US", "zh-CN", "zh-TW", "ja-JP", "ko-KR", "es-ES", "pt-BR", "de-DE", "fr-FR", "it-IT",
    "ru-RU", "ar-SA", "hi-IN", "vi-VN", "id-ID", "tr-TR",
];

/// Embedded JSON dictionaries. The mapping below is the single source of
/// truth — anything else (CLI, web JS) reads from this same data so
/// translations stay in lockstep.
const LOCALE_BUNDLES: &[(&str, &str, &str)] = &[
    ("en-US", "English", include_str!("locales/en.json")),
    ("zh-CN", "简体中文", include_str!("locales/zh-CN.json")),
    ("zh-TW", "繁體中文", include_str!("locales/zh-TW.json")),
    ("ja-JP", "日本語", include_str!("locales/ja.json")),
    ("ko-KR", "한국어", include_str!("locales/ko.json")),
    ("es-ES", "Español", include_str!("locales/es.json")),
    ("pt-BR", "Português (Brasil)", include_str!("locales/pt-BR.json")),
    ("de-DE", "Deutsch", include_str!("locales/de.json")),
    ("fr-FR", "Français", include_str!("locales/fr.json")),
    ("it-IT", "Italiano", include_str!("locales/it.json")),
    ("ru-RU", "Русский", include_str!("locales/ru.json")),
    ("ar-SA", "العربية", include_str!("locales/ar.json")),
    ("hi-IN", "हिन्दी", include_str!("locales/hi.json")),
    ("vi-VN", "Tiếng Việt", include_str!("locales/vi.json")),
    ("id-ID", "Bahasa Indonesia", include_str!("locales/id.json")),
    ("tr-TR", "Türkçe", include_str!("locales/tr.json")),
];

type Dict = HashMap<String, String>;

struct Bundles {
    by_code: HashMap<&'static str, Dict>,
}

fn bundles() -> &'static Bundles {
    static BUNDLES: OnceLock<Bundles> = OnceLock::new();
    BUNDLES.get_or_init(|| {
        let mut by_code = HashMap::new();
        for (code, _name, raw) in LOCALE_BUNDLES {
            let parsed: Value = serde_json::from_str(raw).unwrap_or_else(|err| {
                panic!("i18n: failed to parse embedded {} JSON: {err}", code)
            });
            let mut dict: Dict = HashMap::new();
            if let Value::Object(map) = parsed {
                for (k, v) in map {
                    if let Value::String(s) = v {
                        dict.insert(k, s);
                    }
                }
            }
            by_code.insert(*code, dict);
        }
        Bundles { by_code }
    })
}

/// Returns the list of supported `(code, native_name)` pairs in the order
/// they should appear in pickers.
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

/// Returns the raw dictionary for `code`, or `None` if unknown.
pub fn dictionary(code: &str) -> Option<&'static Dict> {
    let canon = canonicalize(code)?;
    bundles().by_code.get(canon)
}

fn canonicalize(code: &str) -> Option<&'static str> {
    for c in LOCALE_CODES {
        if c.eq_ignore_ascii_case(code) {
            return Some(*c);
        }
    }
    detect::map_to_canonical(code)
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

fn current_lock() -> &'static RwLock<&'static str> {
    static CUR: OnceLock<RwLock<&'static str>> = OnceLock::new();
    CUR.get_or_init(|| {
        let initial = read_persisted_locale()
            .or_else(|| detect::detect_os_locale())
            .unwrap_or("en-US");
        RwLock::new(initial)
    })
}

/// Currently active locale code (one of `LOCALE_CODES`).
pub fn current_locale() -> &'static str {
    *current_lock().read()
}

/// Override the active locale. If `code` is unknown the call is a no-op and
/// returns `false`. Persists the choice to `~/.unterm/lang.json` so future
/// invocations / restarts honour it.
pub fn set_locale(code: &str) -> bool {
    let Some(canon) = canonicalize(code) else {
        return false;
    };
    *current_lock().write() = canon;
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

/// Override the locale for the *current process only* — does not write to
/// disk. Used by the CLI's `--lang` flag.
pub fn set_locale_transient(code: &str) -> bool {
    let Some(canon) = canonicalize(code) else {
        return false;
    };
    *current_lock().write() = canon;
    true
}

/// Translated string for `key`. Falls back to English, then to `key` itself.
pub fn t(key: &str) -> String {
    lookup(current_locale(), key)
}

/// Translated, with `{name}` placeholders substituted from `args`.
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
    if let Some(dict) = bundles.by_code.get(locale) {
        if let Some(v) = dict.get(key) {
            return v.clone();
        }
    }
    if locale != "en-US" {
        if let Some(en) = bundles.by_code.get("en-US") {
            if let Some(v) = en.get(key) {
                return v.clone();
            }
        }
    }
    key.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_is_canonical() {
        assert!(bundles().by_code.contains_key("en-US"));
    }

    #[test]
    fn substitution_works() {
        let result = t_args("recording.started", &[("path", "/tmp/foo")]);
        assert!(result.contains("/tmp/foo"));
    }

    #[test]
    fn fallback_to_key() {
        let result = t("nonexistent.key.value");
        assert_eq!(result, "nonexistent.key.value");
    }

    #[test]
    fn map_zh_variants() {
        assert_eq!(detect::map_to_canonical("zh-Hans-CN"), Some("zh-CN"));
        assert_eq!(detect::map_to_canonical("zh-Hant-TW"), Some("zh-TW"));
        assert_eq!(detect::map_to_canonical("zh_CN.UTF-8"), Some("zh-CN"));
    }
}

#[cfg(test)]
mod catalogue_tests {
    use super::*;

    /// Every language the product claims has every string the interface asks
    /// for. A missing one falls back to English, and one missing from English
    /// falls back to the key -- `composer.title` showing through the window
    /// where a heading should be.
    #[test]
    fn the_interfaces_own_strings_exist_in_every_language() {
        const NEEDED: &[&str] = &[
            "composer.title",
            "composer.hint",
            "composer.waiting",
            "git.no_git",
            "git.not_a_repository",
            "menu.git_panel",
            "cockpit.inbox_title",
            "settings.menu.change_cwd",
            "settings.menu.open_folder",
            "settings.menu.split_right",
            "settings.menu.recording_on",
            "settings.menu.recording_off",
            "settings.menu.console",
            "settings.menu.console.hint",
            "settings.menu.export_session",
            "settings.menu.web_settings",
        ];

        let bundles = bundles();
        for (code, dictionary) in &bundles.by_code {
            for key in NEEDED {
                assert!(dictionary.contains_key(*key), "{code} is missing {key}");
                assert!(
                    !dictionary[*key].trim().is_empty(),
                    "{code}'s {key} is empty"
                );
            }
        }
    }

    /// A placeholder that is not carried through the translation leaves the
    /// number out of the sentence entirely.
    #[test]
    fn a_counted_string_keeps_its_placeholder_in_every_language() {
        let bundles = bundles();
        for (code, dictionary) in &bundles.by_code {
            let waiting = &dictionary["composer.waiting"];
            assert!(waiting.contains("{n}"), "{code}: {waiting}");
        }
    }
}
