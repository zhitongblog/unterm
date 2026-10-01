//! The product's settings, read from next-core's config.
//!
//! next-core parses a config; it does not decide where one lives or which
//! settings a product has. That is this crate's job, and it is why this is
//! here rather than in the kernel.
//!
//! One loaded config per process, because several things read it -- the
//! window, the MCP surface, the recorder -- and a setting that means one thing
//! in one of them and something else in another is worse than no setting.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use unterm_engine::next_core::config::{Config, ConfigError};

pub const MAX_SCROLLBACK_LINES: usize = 999_999_999;

/// The alphabet quick select draws its labels from when the config does not
/// name one. 0.57.4's `quick_select_alphabet` default, kept letter for letter
/// so the labels land where muscle memory already expects them.
pub const DEFAULT_QUICK_SELECT_ALPHABET: &str = "asdfqwerzxcvjklmiuopghtybn";

/// When an agent's write to a pty needs the user to say yes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpInputConfirmation {
    /// Never ask.
    Never,
    /// Ask every time.
    Always,
    /// Ask the first time each agent writes, then remember the answer.
    FirstTimePerAgent,
}

impl McpInputConfirmation {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "never" => Some(Self::Never),
            "always" => Some(Self::Always),
            "first_time_per_agent" => Some(Self::FirstTimePerAgent),
            _ => None,
        }
    }
}

/// Everything the product reads out of a config.
///
/// Resolved once rather than looked up per use: a default that is written out
/// here is a default a reader cannot get wrong, and a setting whose name is
/// spelled in one place cannot be spelled two ways.
#[derive(Clone, Debug)]
pub struct Settings {
    pub mcp_input_confirmation: McpInputConfirmation,
    pub mcp_trusted_agents: Vec<String>,
    pub mcp_confirmation_timeout_ms: u64,
    pub mcp_suggest_default_ttl_ms: u64,
    pub mcp_suggest_queue_capacity: usize,
    pub mcp_audit_log_capacity: usize,
    pub cockpit_enabled: bool,
    pub cockpit_auto_checkpoint: bool,
    pub cockpit_done_hold_secs: u64,
    /// The shell to start, as a program and its arguments.
    pub shell: Option<Vec<String>>,
    /// The letters quick select labels spans with.
    pub quick_select_alphabet: String,
    pub initial_cols: usize,
    pub initial_rows: usize,
    pub color_scheme: Option<String>,
    /// What `$TERM` says a pane is.
    pub term: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // Ask once per agent: asking every time is ignored after the
            // third banner, and never asking makes a pty write invisible.
            mcp_input_confirmation: McpInputConfirmation::FirstTimePerAgent,
            mcp_trusted_agents: Vec::new(),
            mcp_confirmation_timeout_ms: 30_000,
            mcp_suggest_default_ttl_ms: 300_000,
            mcp_suggest_queue_capacity: 32,
            mcp_audit_log_capacity: 1_000,
            cockpit_enabled: true,
            cockpit_auto_checkpoint: true,
            cockpit_done_hold_secs: 20,
            shell: None,
            quick_select_alphabet: DEFAULT_QUICK_SELECT_ALPHABET.to_string(),
            initial_cols: 80,
            initial_rows: 24,
            color_scheme: None,
            term: "xterm-256color".to_string(),
        }
    }
}

impl Settings {
    /// Read what a config says, keeping the default for anything it does not.
    pub fn from_config(config: &Config) -> Self {
        let mut settings = Settings::default();

        if let Some(text) = config.get("mcp.input_confirmation").and_then(as_str) {
            if let Some(parsed) = McpInputConfirmation::parse(text) {
                settings.mcp_input_confirmation = parsed;
            }
        }
        if let Some(values) = config.get("mcp.trusted_agents") {
            settings.mcp_trusted_agents = strings(values);
        }
        if let Some(value) = number(config, "mcp.confirmation_timeout_ms") {
            settings.mcp_confirmation_timeout_ms = value.max(0) as u64;
        }
        if let Some(value) = number(config, "mcp.suggest_default_ttl_ms") {
            settings.mcp_suggest_default_ttl_ms = value.max(0) as u64;
        }
        if let Some(value) = number(config, "mcp.suggest_queue_capacity") {
            settings.mcp_suggest_queue_capacity = value.max(0) as usize;
        }
        if let Some(value) = number(config, "mcp.audit_log_capacity") {
            settings.mcp_audit_log_capacity = value.max(0) as usize;
        }
        if let Ok(Some(value)) = config.bool_of("cockpit.enabled") {
            settings.cockpit_enabled = value;
        }
        if let Ok(Some(value)) = config.bool_of("cockpit.auto_checkpoint") {
            settings.cockpit_auto_checkpoint = value;
        }
        if let Some(value) = number(config, "cockpit.done_hold_secs") {
            settings.cockpit_done_hold_secs = value.max(0) as u64;
        }
        if let Some(value) = number(config, "window.initial_cols") {
            settings.initial_cols = value.max(1) as usize;
        }
        if let Some(value) = number(config, "window.initial_rows") {
            settings.initial_rows = value.max(1) as usize;
        }
        settings.color_scheme = config
            .get("color_scheme")
            .and_then(as_str)
            .map(String::from);
        if let Some(shell) = config.get("shell") {
            let words = strings(shell);
            if !words.is_empty() {
                settings.shell = Some(words);
            }
        }
        if let Some(text) = config.get("quick_select_alphabet").and_then(as_str) {
            match usable_alphabet(text) {
                Ok(alphabet) => settings.quick_select_alphabet = alphabet,
                // The default still works; an alphabet that cannot label a
                // screen would break the overlay in ways a user cannot see.
                Err(problem) => {
                    log::warn!("quick_select_alphabet {problem}; keeping the default")
                }
            }
        }
        settings
    }
}

/// A label alphabet the quick select overlay can actually use, or what is
/// wrong with the one the config offers.
///
/// Labels are matched lowercase -- typing one shifted means "and paste it" --
/// so the alphabet is folded before checking. A repeated letter would hand two
/// spans the same label, and fewer than eight letters cannot label a busy
/// screen even after pairing them up.
fn usable_alphabet(text: &str) -> Result<String, String> {
    let folded = text.to_lowercase();
    if folded.chars().count() < 8 {
        return Err(format!("`{text}` has fewer than 8 characters"));
    }
    let mut seen = Vec::new();
    for ch in folded.chars() {
        if ch.is_whitespace() {
            return Err(format!("`{text}` contains whitespace"));
        }
        if seen.contains(&ch) {
            return Err(format!("`{ch}` appears in `{text}` more than once"));
        }
        seen.push(ch);
    }
    Ok(folded)
}

fn as_str(value: &unterm_engine::next_core::config::Value) -> Option<&str> {
    match value {
        unterm_engine::next_core::config::Value::Str(text) => Some(text.as_str()),
        _ => None,
    }
}

/// A setting that may be written as one word or as a list of them.
fn strings(value: &unterm_engine::next_core::config::Value) -> Vec<String> {
    use unterm_engine::next_core::config::Value;
    match value {
        Value::Str(text) if text.is_empty() => Vec::new(),
        Value::Str(text) => vec![text.clone()],
        Value::List(items) => items.iter().filter_map(as_str).map(String::from).collect(),
        _ => Vec::new(),
    }
}

/// A whole-number setting, written either way round.
fn number(config: &Config, key: &str) -> Option<i64> {
    use unterm_engine::next_core::config::Value;
    match config.get(key) {
        Some(Value::Int(value)) => Some(*value),
        // A timeout written `5000.0` means 5000, not an error.
        Some(Value::Float(value)) => Some(*value as i64),
        _ => None,
    }
}

static CURRENT: RwLock<Option<Arc<Settings>>> = RwLock::new(None);

/// The settings this process is running with.
///
/// Defaults until something loads a config, so a surface that starts before
/// the window -- or a test with no config at all -- still has answers.
pub fn current() -> Arc<Settings> {
    if let Some(settings) = CURRENT.read().ok().and_then(|held| held.clone()) {
        return settings;
    }
    let settings = Arc::new(Settings::default());
    if let Ok(mut held) = CURRENT.write() {
        held.get_or_insert_with(|| settings.clone()).clone()
    } else {
        settings
    }
}

/// Install the settings a config asks for.
pub fn set_current(config: &Config) {
    let settings = Arc::new(Settings::from_config(config));
    if let Ok(mut held) = CURRENT.write() {
        *held = Some(settings);
    }
}

/// The platform shell used by the previous Windows front end when no shell
/// was named explicitly. Keep this decision shared by the window and MCP
/// surfaces so a keyboard split and `unterm-cli session split` cannot open
/// different shells.
pub fn preferred_platform_shell() -> Option<Vec<String>> {
    #[cfg(windows)]
    {
        for pwsh in windows_program_candidates(&[
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            "pwsh.exe",
            "pwsh",
        ]) {
            if windows_shell_candidate_exists(&pwsh) {
                return Some(vec![
                    pwsh,
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                ]);
            }
        }
        Some(vec![
            "powershell.exe".to_string(),
            "-NoLogo".to_string(),
            "-NoProfile".to_string(),
        ])
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
fn windows_shell_candidate_exists(program: &str) -> bool {
    let path = PathBuf::from(program);
    if path.is_absolute() {
        return path.is_file();
    }
    resolve_windows_program(program).is_some()
}

#[cfg(all(windows, test))]
fn windows_shell_version_probe(program: &str) -> bool {
    let path = PathBuf::from(program);
    if path.is_absolute() && !path.is_file() {
        return false;
    }
    std::process::Command::new(program)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-Command",
            "$PSVersionTable.PSVersion.ToString()",
        ])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn windows_program_candidates(programs: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    for program in programs {
        let path = PathBuf::from(program);
        if path.is_absolute() {
            found.push(program.to_string());
            continue;
        }
        if let Some(path) = resolve_windows_program(program) {
            found.push(path);
        }
        found.push(program.to_string());
    }
    found
}

#[cfg(windows)]
fn resolve_windows_program(program: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate.display().to_string());
        }
        if std::path::Path::new(program).extension().is_none() {
            for ext in ["exe", "cmd", "bat"] {
                let candidate = dir.join(format!("{program}.{ext}"));
                if candidate.is_file() {
                    return Some(candidate.display().to_string());
                }
            }
        }
    }
    None
}

/// Scrollback capacity for panes created after startup.
///
/// An explicit declarative setting wins. Otherwise preserve the previous
/// product's Web Settings contract by reading `~/.unterm/scrollback.json`.
/// Whether shells Unterm starts get its integration script (`OSC 133`
/// prompt and command marks). On unless `shell_integration = false`.
pub fn shell_integration(config: &Config) -> bool {
    !matches!(config.bool_of("shell_integration"), Ok(Some(false)))
}

pub fn scrollback_lines(config: &Config) -> usize {
    if let Ok(Some(value)) = config.int_of("scrollback_lines") {
        if let Ok(value) = usize::try_from(value) {
            if value <= MAX_SCROLLBACK_LINES {
                return value;
            }
        }
    }
    scrollback_override(default_scrollback_path().as_deref())
        .unwrap_or(unterm_engine::next_core::DEFAULT_SCROLLBACK_LINES)
}

fn default_scrollback_path() -> Option<PathBuf> {
    unterm_protocol::state_path("scrollback.json")
}

fn scrollback_override(path: Option<&std::path::Path>) -> Option<usize> {
    let content = std::fs::read_to_string(path?).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&content).ok()?;
    let lines = usize::try_from(value.get("lines")?.as_u64()?).ok()?;
    (lines > 0 && lines <= MAX_SCROLLBACK_LINES).then_some(lines)
}

/// Where the config file lives when the user does not say.
pub fn default_path() -> Option<PathBuf> {
    // The config file is state like everything else — a test or headless
    // Core must read the config it was given, never the real user's.
    unterm_protocol::state_path("unterm.conf")
}

/// Load the config, and say what was wrong with it.
///
/// A config that will not parse is reported, not fatal: a terminal you cannot
/// open is no way to fix your config.
pub fn load(path: Option<PathBuf>) -> (Config, Vec<ConfigError>) {
    let Some(path) = path.or_else(default_path) else {
        return (Config::default(), Vec::new());
    };
    let Ok(source) = std::fs::read_to_string(&path) else {
        // No config is not a problem; it means the defaults.
        return (Config::default(), Vec::new());
    };
    // Notepad, PowerShell's `Set-Content -Encoding utf8`, and several Windows
    // editors put a BOM at the front of a UTF-8 file. Left in place it glues
    // itself to the first key, and the whole config is quietly ignored -- on
    // a Windows-first product, by the exact route a user is most likely to
    // take to edit it.
    let source = source.strip_prefix('\u{feff}').unwrap_or(&source);
    match unterm_engine::next_core::config::parse(source) {
        // A key the schema does not know is a setting doing nothing — the
        // exact silent failure the schema was written to catch, and until
        // now nothing ever asked it.
        Ok(config) => {
            let warnings = unterm_engine::next_core::config_schema::check(&config);
            (config, warnings)
        }
        Err(errors) => (Config::default(), errors),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unterm_engine::next_core::config::parse;

    fn settings(source: &str) -> Settings {
        Settings::from_config(&parse(source).expect("config should parse"))
    }

    #[test]
    fn a_config_saved_by_a_windows_editor_is_still_read() {
        // Notepad and `Set-Content -Encoding utf8` both write this BOM. The
        // config it prefixes used to be discarded in full, with no error --
        // the setting simply had no effect and nothing said why.
        let dir = std::env::temp_dir().join(format!(
            "unterm-bom-test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("unterm.conf");
        std::fs::write(
            &path,
            "\u{feff}[mcp]\ninput_confirmation = \"never\"\n".as_bytes(),
        )
        .expect("write config");

        let (config, errors) = load(Some(path));
        let settings = Settings::from_config(&config);
        std::fs::remove_dir_all(&dir).ok();

        assert!(
            errors.is_empty(),
            "a BOM was reported as a config error: {errors:?}"
        );
        assert_eq!(
            settings.mcp_input_confirmation,
            McpInputConfirmation::Never,
            "the BOM swallowed the whole config"
        );
    }

    #[test]
    fn an_empty_config_is_all_defaults() {
        let settings = settings("");
        assert_eq!(
            settings.mcp_input_confirmation,
            McpInputConfirmation::FirstTimePerAgent
        );
        assert!(settings.cockpit_enabled);
        assert!(settings.shell.is_none());
    }

    #[test]
    fn the_confirmation_policy_is_read_by_name() {
        assert_eq!(
            settings("[mcp]\ninput_confirmation = \"never\"").mcp_input_confirmation,
            McpInputConfirmation::Never
        );
        assert_eq!(
            settings("[mcp]\ninput_confirmation = \"always\"").mcp_input_confirmation,
            McpInputConfirmation::Always
        );
    }

    #[test]
    fn a_policy_nobody_recognises_keeps_the_safe_default() {
        // Silently never asking would be the dangerous way to read a typo.
        assert_eq!(
            settings("[mcp]\ninput_confirmation = \"whenever\"").mcp_input_confirmation,
            McpInputConfirmation::FirstTimePerAgent
        );
    }

    #[test]
    fn trusted_agents_can_be_one_name_or_a_list() {
        assert_eq!(
            settings("[mcp]\ntrusted_agents = \"claude\"").mcp_trusted_agents,
            vec!["claude".to_string()]
        );
        assert_eq!(
            settings("[mcp]\ntrusted_agents = [\"claude\", \"codex\"]").mcp_trusted_agents,
            vec!["claude".to_string(), "codex".to_string()]
        );
    }

    #[test]
    fn numbers_are_read_and_anything_else_keeps_the_default() {
        assert_eq!(
            settings("[mcp]\nconfirmation_timeout_ms = 5000").mcp_confirmation_timeout_ms,
            5000
        );
        assert_eq!(
            settings("[mcp]\nconfirmation_timeout_ms = \"soon\"").mcp_confirmation_timeout_ms,
            Settings::default().mcp_confirmation_timeout_ms
        );
    }

    #[test]
    fn the_cockpit_can_be_switched_off() {
        assert!(!settings("[cockpit]\nenabled = false").cockpit_enabled);
    }

    #[test]
    fn the_shell_can_carry_its_arguments() {
        assert_eq!(
            settings(r#"shell = ["pwsh.exe", "-NoLogo"]"#).shell,
            Some(vec!["pwsh.exe".to_string(), "-NoLogo".to_string()])
        );
        assert_eq!(
            settings("shell = \"cmd.exe\"").shell,
            Some(vec!["cmd.exe".to_string()])
        );
    }

    #[test]
    fn the_quick_select_alphabet_can_be_replaced() {
        assert_eq!(
            settings("quick_select_alphabet = \"qwertyuiop\"").quick_select_alphabet,
            "qwertyuiop"
        );
        // Labels are matched lowercase, so the alphabet is folded on the way in.
        assert_eq!(
            settings("quick_select_alphabet = \"QWERTYUIOP\"").quick_select_alphabet,
            "qwertyuiop"
        );
    }

    #[test]
    fn an_unusable_alphabet_keeps_the_default() {
        // A repeated letter would hand two spans the same label.
        assert_eq!(
            settings("quick_select_alphabet = \"aabbccddee\"").quick_select_alphabet,
            DEFAULT_QUICK_SELECT_ALPHABET
        );
        // Too few letters cannot label a busy screen, even with pairs.
        assert_eq!(
            settings("quick_select_alphabet = \"asdf\"").quick_select_alphabet,
            DEFAULT_QUICK_SELECT_ALPHABET
        );
        // Case-folding can introduce the duplicate: `a` and `A` are one label.
        assert_eq!(
            settings("quick_select_alphabet = \"aAsdfqwer\"").quick_select_alphabet,
            DEFAULT_QUICK_SELECT_ALPHABET
        );
    }

    #[test]
    fn the_default_alphabet_is_the_one_0_57_4_shipped() {
        assert_eq!(DEFAULT_QUICK_SELECT_ALPHABET, "asdfqwerzxcvjklmiuopghtybn");
        assert!(usable_alphabet(DEFAULT_QUICK_SELECT_ALPHABET).is_ok());
    }

    #[test]
    fn settings_are_available_before_anything_loads_a_config() {
        // The MCP surface starts before the window does.
        assert!(current().cockpit_enabled);
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_default_prefers_usable_pwsh_then_powershell_not_cmd() {
        let shell = preferred_platform_shell().expect("Windows has a preferred shell");
        let name = shell[0].to_ascii_lowercase();
        if windows_shell_version_probe("pwsh.exe") || windows_shell_version_probe("pwsh") {
            assert!(name.ends_with("pwsh.exe") || name == "pwsh", "{shell:?}");
        } else {
            assert!(name.ends_with("powershell.exe"), "{shell:?}");
        }
        assert_eq!(shell.get(1).map(String::as_str), Some("-NoLogo"));
        assert_eq!(shell.get(2).map(String::as_str), Some("-NoProfile"));
    }

    #[test]
    fn declarative_scrollback_capacity_is_used_for_new_panes() {
        let config = parse("scrollback_lines = 123456").unwrap();
        assert_eq!(scrollback_lines(&config), 123_456);
    }

    #[test]
    fn the_legacy_web_override_is_still_understood() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scrollback.json");
        std::fs::write(&path, r#"{"lines":4321}"#).unwrap();
        assert_eq!(scrollback_override(Some(&path)), Some(4_321));

        std::fs::write(&path, r#"{"lines":1000000000}"#).unwrap();
        assert_eq!(scrollback_override(Some(&path)), None);
    }
}
