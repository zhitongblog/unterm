//! OS-level locale detection with mapping to Unterm's canonical 9 locales.

/// Map an arbitrary BCP-47 / POSIX-style locale tag to one of Unterm's
/// canonical codes. Returns `None` if the tag doesn't match any supported
/// language family — callers should fall back to `en-US`.
pub fn map_to_canonical(raw: &str) -> Option<&'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Normalise: replace `_` with `-`, keep just the `lang[-script][-region]`
    // portion (drop `.UTF-8`, `@modifier` etc.).
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
            // Could be: zh, zh-CN, zh-TW, zh-Hans, zh-Hant, zh-Hans-CN, zh-Hant-TW
            // Hant family → Traditional, anything else → Simplified.
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

/// Probe the OS for the user's preferred locale. Returns a canonical code or
/// `None` if no usable signal was found.
pub fn detect_os_locale() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        if let Some(code) = detect_macos() {
            return Some(code);
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(code) = detect_windows() {
            return Some(code);
        }
    }
    if let Some(code) = detect_env() {
        return Some(code);
    }
    None
}

fn detect_env() -> Option<&'static str> {
    for var in &["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"] {
        if let Ok(v) = std::env::var(var) {
            if let Some(code) = map_to_canonical(&v) {
                return Some(code);
            }
            // LANGUAGE may be colon-delimited list of preferences.
            for part in v.split(':') {
                if let Some(code) = map_to_canonical(part) {
                    return Some(code);
                }
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn detect_macos() -> Option<&'static str> {
    // `defaults read NSGlobalDomain AppleLanguages` returns something like:
    // (
    //     "en-US",
    //     "zh-Hans-CN",
    //     ...
    // )
    let output = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "NSGlobalDomain", "AppleLanguages"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for raw in text.split(|c: char| c == ',' || c == '(' || c == ')' || c == '\n') {
        let cleaned = raw.trim().trim_matches('"').trim();
        if cleaned.is_empty() {
            continue;
        }
        if let Some(code) = map_to_canonical(cleaned) {
            return Some(code);
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn detect_windows() -> Option<&'static str> {
    map_to_canonical(&windows_locale_name()?)
}

/// The user's display locale (`HKCU\Control Panel\International`,
/// `LocaleName`), read from the registry directly. It used to be `reg query`
/// in a hidden console at every start -- a program quietly running `reg.exe`
/// is one of the things antivirus engines score as suspicious.
#[cfg(target_os = "windows")]
pub fn windows_locale_name() -> Option<String> {
    use winreg::enums::HKEY_CURRENT_USER;
    winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Control Panel\International")
        .ok()?
        .get_value::<String, _>("LocaleName")
        .ok()
}
