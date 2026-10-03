//! The environment every new pane inherits, from the config.
//!
//! Applied to the process itself so the pty spawn path, shell discovery and
//! the agents the terminal launches all see the same values. Both the GUI and
//! the Core call it: shells are spawned by the Core, and a Core started on
//! its own (by the CLI, or headless) inherits nothing from any window.

use unterm_engine::next_core::config::{Config, Value};

/// Apply `path_append` and the `[env]` section to this process.
pub fn apply(config: &Config) {
    apply_path_append(config);
    apply_session_env(config);
}

/// Extend the environment inherited by every new pane.
///
/// The 0.57 Windows config did this in Lua. Keeping it at process startup
/// makes shell discovery, agent discovery and the shells themselves see one
/// identical PATH.
fn apply_path_append(config: &Config) {
    // Only the Windows block below appends to it; other platforms read as-is.
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut additions: Vec<std::path::PathBuf> = config
        .list_of("path_append")
        .ok()
        .flatten()
        .into_iter()
        .flatten()
        .filter_map(|value| match value {
            Value::Str(path) => {
                Some(std::path::PathBuf::from(path))
            }
            _ => None,
        })
        .collect();

    #[cfg(windows)]
    {
        let mut standard = vec![
            std::path::PathBuf::from(r"C:\Program Files\nodejs"),
            std::path::PathBuf::from(r"C:\Strawberry\perl\bin"),
        ];
        if let Some(appdata) = std::env::var_os("APPDATA") {
            standard.push(std::path::PathBuf::from(appdata).join("npm"));
        }
        if let Some(home) = std::env::var_os("USERPROFILE") {
            standard.push(std::path::PathBuf::from(home).join(".bun").join("bin"));
        }
        additions.extend(standard.into_iter().filter(|path| path.is_dir()));
    }

    let current = std::env::var_os("PATH").unwrap_or_default();
    let paths = merged_path(std::env::split_paths(&current), additions);
    match std::env::join_paths(paths) {
        Ok(path) => std::env::set_var("PATH", path),
        Err(error) => log::warn!("could not extend PATH: {error}"),
    }
}

fn merged_path(
    current: impl IntoIterator<Item = std::path::PathBuf>,
    additions: impl IntoIterator<Item = std::path::PathBuf>,
) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<std::path::PathBuf> = current.into_iter().collect();
    for addition in additions {
        let duplicate = paths.iter().any(|known| {
            if cfg!(windows) {
                known
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&addition.to_string_lossy())
            } else {
                known == &addition
            }
        });
        if !duplicate {
            paths.push(addition);
        }
    }
    paths
}

/// Put the `[env]` section into the environment every new pane inherits.
///
/// The Lua config called this `set_environment_variables`. Setting it on the
/// process, like `path_append` above, means the pty spawn path, shell
/// discovery and the agents this terminal launches all see the same values.
fn apply_session_env(config: &Config) {
    let names: Vec<String> = config
        .keys()
        .filter_map(|key| key.strip_prefix("env."))
        .map(String::from)
        .collect();
    for name in names {
        match config.str_of(&format!("env.{name}")) {
            Ok(Some(value)) => std::env::set_var(&name, value),
            Ok(None) => {}
            // A non-string value: report it with its line rather than
            // exporting something the user did not write.
            Err(error) => log::warn!("config line {}: {}", error.line, error.message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_extensions_keep_order_and_do_not_duplicate_entries() {
        let current = [
            std::path::PathBuf::from("first"),
            std::path::PathBuf::from("second"),
        ];
        let merged = merged_path(
            current,
            [
                std::path::PathBuf::from("second"),
                std::path::PathBuf::from("third"),
            ],
        );

        assert_eq!(
            merged,
            [
                std::path::PathBuf::from("first"),
                std::path::PathBuf::from("second"),
                std::path::PathBuf::from("third"),
            ]
        );
    }

    #[test]
    fn the_env_section_reaches_the_process_environment() {
        let config = unterm_engine::next_core::config::parse(
            "[env]\nUNTERM_TEST_ENV_SECTION = \"on\"\nUNTERM_TEST_ENV_BAD = 3",
        )
        .unwrap();

        apply_session_env(&config);

        // The string is exported; the non-string is reported, not invented.
        assert_eq!(
            std::env::var("UNTERM_TEST_ENV_SECTION").as_deref(),
            Ok("on")
        );
        assert!(std::env::var("UNTERM_TEST_ENV_BAD").is_err());
    }

    #[test]
    #[cfg(windows)]
    fn windows_path_deduplication_ignores_case() {
        let merged = merged_path(
            [std::path::PathBuf::from(r"C:\Tools")],
            [std::path::PathBuf::from(r"c:\tools")],
        );
        assert_eq!(merged.len(), 1);
    }
}
