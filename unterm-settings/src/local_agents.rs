//! What the console needs to know about an agent before handing it a task,
//! beyond what the signed manifest says: agents the manifest does not list
//! yet, and whether the one it does list is signed in.
//!
//! **Agents outside the manifest.** The manifest is signed and fetched; an
//! agent whose host protocol already lives in `unterm-brain` should not wait
//! for the next signed envelope to be usable. Each one here is found the way
//! its installer leaves it, and only ever started with its own binary and the
//! host's arguments -- the caller still names an id, never a command.
//!
//! **Signed in.** Every agent CLI keeps its sign-in somewhere of its own. A
//! task handed to one that is not signed in fails at the first turn with a
//! message about a browser no one is looking at, so the console asks first.
//! The answer is `Some(true)` when a credential is there, `Some(false)` when
//! the places it would be are empty, and `None` when it cannot be told from
//! outside (a keychain, or an agent whose store is not known here) -- the
//! console then says nothing rather than guess.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// An agent the console can run that the manifest does not list.
pub struct LocalAgent {
    pub id: &'static str,
    pub name: &'static str,
    pub vendor: &'static str,
    pub homepage: &'static str,
    find: fn() -> Option<PathBuf>,
}

impl LocalAgent {
    /// Where it is installed, if it is.
    pub fn binary(&self) -> Option<PathBuf> {
        (self.find)()
    }

    /// The row `GET /api/agents/list` shows for it, in the manifest rows' shape.
    pub fn row(&self) -> Value {
        let binary = self.binary();
        json!({
            "id": self.id,
            "name": self.name,
            "vendor": self.vendor,
            "homepage": self.homepage,
            "manifest_version": Value::Null,
            "category": "cli",
            "installed": binary.is_some(),
            "detected_version": Value::Null,
            "binary_path": binary.map(|p| p.display().to_string()),
            "builtin": true,
        })
    }
}

static LOCAL_AGENTS: &[LocalAgent] = &[LocalAgent {
    id: "agy",
    name: "Antigravity CLI (agy)",
    vendor: "Google",
    homepage: "https://antigravity.google/docs/cli",
    find: find_agy,
}];

pub fn local_agents() -> &'static [LocalAgent] {
    LOCAL_AGENTS
}

pub fn local_agent(id: &str) -> Option<&'static LocalAgent> {
    LOCAL_AGENTS.iter().find(|agent| agent.id == id)
}

/// `agy` on the PATH, or where its installer puts it: `%LOCALAPPDATA%\agy\bin`
/// on Windows, `~/.local/bin` elsewhere.
fn find_agy() -> Option<PathBuf> {
    let name = if cfg!(windows) { "agy.exe" } else { "agy" };
    if let Some(found) = on_path(name) {
        return Some(found);
    }
    let mut candidates = Vec::new();
    if let Some(local) = dirs_next::data_local_dir() {
        candidates.push(local.join("agy").join("bin").join(name));
    }
    if let Some(home) = dirs_next::home_dir() {
        candidates.push(home.join(".local").join("bin").join(name));
    }
    candidates.into_iter().find(|path| path.is_file())
}

fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|candidate| candidate.is_file())
}

// ---- signed in -----------------------------------------------------------

/// Whether `id` is signed in, and how that was seen; see the module docs.
pub fn signed_in(id: &str) -> Option<(bool, &'static str)> {
    let home = dirs_next::home_dir()?;
    signed_in_with(id, &home, &|key| std::env::var(key).ok().filter(|v| !v.trim().is_empty()), cfg!(target_os = "macos"))
}

/// [`signed_in`] with the home folder, environment and platform given, so it
/// can be checked without touching the real ones.
pub fn signed_in_with(
    id: &str,
    home: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    macos: bool,
) -> Option<(bool, &'static str)> {
    let any_env = |keys: &[&str]| keys.iter().any(|key| env(key).is_some());
    let file = |path: PathBuf| std::fs::metadata(path).map(|m| m.is_file() && m.len() > 0).unwrap_or(false);
    match id {
        "claude-code" => {
            if any_env(&["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"]) {
                return Some((true, "api_key"));
            }
            let dir = env("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".claude"));
            if file(dir.join(".credentials.json")) {
                return Some((true, "credentials_file"));
            }
            // On macOS Claude Code keeps its sign-in in the keychain.
            if macos {
                None
            } else {
                Some((false, "credentials_file"))
            }
        }
        "codex-cli" => {
            if any_env(&["OPENAI_API_KEY"]) {
                return Some((true, "api_key"));
            }
            let dir = env("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".codex"));
            Some((file(dir.join("auth.json")), "credentials_file"))
        }
        "gemini-cli" => {
            if any_env(&["GEMINI_API_KEY", "GOOGLE_API_KEY", "GOOGLE_GENAI_USE_VERTEXAI"]) {
                return Some((true, "api_key"));
            }
            Some((file(home.join(".gemini").join("oauth_creds.json")), "credentials_file"))
        }
        "opencode" => {
            let data = env("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local").join("share"));
            if file(data.join("opencode").join("auth.json")) {
                return Some((true, "credentials_file"));
            }
            if any_env(&["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "GEMINI_API_KEY", "OPENROUTER_API_KEY"]) {
                return Some((true, "api_key"));
            }
            // OpenCode has free models that need no sign-in, so no key is
            // not the same as not able to run.
            None
        }
        // agy signs in with a Google account and keeps it in the OS
        // credential store; nothing outside it can tell.
        _ => None,
    }
}

/// What to type in a terminal to sign in.
pub fn sign_in_command(id: &str) -> Option<&'static str> {
    match id {
        "claude-code" => Some("claude"),
        "codex-cli" => Some("codex login"),
        "gemini-cli" => Some("gemini"),
        "opencode" => Some("opencode auth login"),
        "agy" => Some("agy"),
        _ => None,
    }
}

/// The sign-in fields an agent row carries.
pub fn sign_in_fields(id: &str) -> Value {
    let state = signed_in(id);
    json!({
        "signed_in": state.map(|(yes, _)| yes),
        "signed_in_via": state.map(|(_, via)| via),
        "sign_in_command": sign_in_command(id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn signed_in_reads_each_agents_own_store() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        // Nothing yet: each says no, except where it cannot tell.
        assert_eq!(signed_in_with("codex-cli", h, &none, false), Some((false, "credentials_file")));
        assert_eq!(signed_in_with("claude-code", h, &none, false), Some((false, "credentials_file")));
        assert_eq!(signed_in_with("claude-code", h, &none, true), None, "macOS keeps it in the keychain");
        assert_eq!(signed_in_with("agy", h, &none, false), None);
        assert_eq!(signed_in_with("opencode", h, &none, false), None, "its free models need no sign-in");

        std::fs::create_dir_all(h.join(".codex")).unwrap();
        std::fs::write(h.join(".codex/auth.json"), "").unwrap();
        assert_eq!(signed_in_with("codex-cli", h, &none, false).map(|s| s.0), Some(false), "an empty file is not a sign-in");
        std::fs::write(h.join(".codex/auth.json"), "{}").unwrap();
        assert_eq!(signed_in_with("codex-cli", h, &none, false), Some((true, "credentials_file")));

        std::fs::create_dir_all(h.join(".claude")).unwrap();
        std::fs::write(h.join(".claude/.credentials.json"), "{}").unwrap();
        assert_eq!(signed_in_with("claude-code", h, &none, true), Some((true, "credentials_file")));

        let key = |name: &str| (name == "GEMINI_API_KEY").then(|| "k".to_string());
        assert_eq!(signed_in_with("gemini-cli", h, &key, false), Some((true, "api_key")));
        assert_eq!(signed_in_with("opencode", h, &key, false), Some((true, "api_key")));

        let elsewhere = tempfile::tempdir().unwrap();
        let moved = elsewhere.path().to_path_buf();
        let codex_home = move |name: &str| (name == "CODEX_HOME").then(|| moved.display().to_string());
        assert_eq!(signed_in_with("codex-cli", h, &codex_home, false).map(|s| s.0), Some(false), "CODEX_HOME moves it");
    }

    #[test]
    fn agy_is_offered_and_found_where_its_installer_puts_it() {
        let agy = local_agent("agy").expect("agy is built in");
        let row = agy.row();
        assert_eq!(row["id"], "agy");
        assert_eq!(row["installed"], json!(agy.binary().is_some()));
        assert!(local_agent("claude-code").is_none(), "manifest agents are not duplicated");
        assert_eq!(sign_in_command("agy"), Some("agy"));
    }
}
