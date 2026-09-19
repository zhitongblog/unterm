//! The environment variables the terminal puts into every shell it starts.
//!
//! These carried the WezTerm name into the environment of every command a user
//! ever ran. They are `UNTERM_*` now.
//!
//! The old names are still written alongside the new ones, and still read as a
//! fallback. That is not attachment to them: a user's shell prompt or script
//! may read `$WEZTERM_PANE`, and quietly breaking those would be the same
//! silent failure this codebase has been removing everywhere else. Nothing in
//! the program reads them by preference, and they can be dropped once the
//! transition has had time to happen.

/// Read a terminal-provided variable, preferring the current name.
pub fn var(name: &str) -> Option<String> {
    std::env::var(format!("UNTERM_{name}"))
        .ok()
        .or_else(|| std::env::var(format!("WEZTERM_{name}")).ok())
}

/// Same, without requiring the value to be UTF-8 -- paths on Windows are not
/// guaranteed to be.
pub fn var_os(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(format!("UNTERM_{name}"))
        .or_else(|| std::env::var_os(format!("WEZTERM_{name}")))
}

/// Set a terminal-provided variable under both names.
pub fn set_var<V: AsRef<std::ffi::OsStr>>(name: &str, value: V) {
    std::env::set_var(format!("UNTERM_{name}"), value.as_ref());
    std::env::set_var(format!("WEZTERM_{name}"), value.as_ref());
}

/// Remove a terminal-provided variable under both names.
pub fn remove_var(name: &str) {
    std::env::remove_var(format!("UNTERM_{name}"));
    std::env::remove_var(format!("WEZTERM_{name}"));
}

/// Both spellings of a name, for callers that hand a list to a child process.
pub fn both(name: &str) -> [String; 2] {
    [format!("UNTERM_{name}"), format!("WEZTERM_{name}")]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_writes_both_spellings() {
        set_var("TEST_BOTH", "value");

        // A shell prompt reading the old name must keep working.
        assert_eq!(std::env::var("UNTERM_TEST_BOTH").as_deref(), Ok("value"));
        assert_eq!(std::env::var("WEZTERM_TEST_BOTH").as_deref(), Ok("value"));
        remove_var("TEST_BOTH");
    }

    #[test]
    fn reading_prefers_the_current_name() {
        std::env::set_var("UNTERM_TEST_PREFER", "new");
        std::env::set_var("WEZTERM_TEST_PREFER", "old");

        assert_eq!(var("TEST_PREFER").as_deref(), Some("new"));

        remove_var("TEST_PREFER");
    }

    #[test]
    fn the_old_name_still_answers_on_its_own() {
        remove_var("TEST_FALLBACK");
        std::env::set_var("WEZTERM_TEST_FALLBACK", "old");

        // Someone's existing environment may only have the old one.
        assert_eq!(var("TEST_FALLBACK").as_deref(), Some("old"));

        remove_var("TEST_FALLBACK");
    }

    /// The engine spells these two names itself, and must keep spelling the
    /// same two.
    ///
    /// It sits below this crate, so it cannot call `both`. That is fine right
    /// up until one side is renamed: the shell would carry a name nothing
    /// reads, the MCP bridge would find no pane, and every agent would go
    /// back to driving whichever pane the user was looking at -- silently,
    /// because a missing variable looks exactly like a pane that did not
    /// want to say.
    #[test]
    fn the_engine_spells_the_pane_variable_the_same_way() {
        let source = include_str!("../../unterm-engine/src/next_core/session_runtime.rs");
        for name in both("PANE") {
            assert!(
                source.contains(&format!("command.env(\"{name}\"")),
                "the engine no longer sets {name}; \
                 `agent signal` and the MCP bridge both read it"
            );
        }
    }

    #[test]
    fn a_spawned_command_carries_both_names() {
        // What a shell started by the terminal actually receives. The old name
        // is what a user's existing prompt reads; the new one is what the
        // program uses.
        let mut cmd = portable_pty::CommandBuilder::new("pwsh");
        for name in both("PANE") {
            cmd.env(name, "7");
        }

        assert_eq!(
            cmd.get_env("UNTERM_PANE").and_then(|v| v.to_str()),
            Some("7")
        );
        assert_eq!(
            cmd.get_env("WEZTERM_PANE").and_then(|v| v.to_str()),
            Some("7")
        );
    }

    #[test]
    fn removing_clears_both() {
        set_var("TEST_REMOVE", "value");
        remove_var("TEST_REMOVE");

        assert!(std::env::var("UNTERM_TEST_REMOVE").is_err());
        assert!(std::env::var("WEZTERM_TEST_REMOVE").is_err());
    }
}
