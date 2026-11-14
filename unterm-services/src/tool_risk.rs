//! What a hosted agent's tool call is, and how much it may do unasked.
//!
//! A hosted session asks before every tool its CLI would prompt for, and a
//! person answering all of them stops reading them. [`crate::agent_policy`]
//! lets a person answer a kind of question once; this lets them choose how
//! much the agent may do *without* a rule -- its [`Autonomy`] -- and judges
//! each call on the same risk scale the gateway uses for everything else
//! ([`unterm_gateway::Risk`]).
//!
//! [`classify`] reads the call: Claude Code's, Codex's and the ACP agents'
//! own tools, Unterm's MCP methods (as the CLI names them,
//! `mcp__unterm__exec_run_wait`), and Unzoo Browser's. A command is read for
//! what it does ([`unterm_gateway::command_risk`]); a file edit is judged by
//! where the file is. A tool it does not know is `None`, which is never let
//! through unasked.
//!
//! [`Autonomy::allows`] is the ceiling. Whatever the setting, deleting,
//! secrets and money are always a person's call: those are the risks the
//! gateway refuses a standing grant for, and an autonomy level is a standing
//! grant by another name.
//!
//! [`describe`] is the other half of reading a call: one line saying what it
//! did, for the person reading the conversation afterwards.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use unterm_gateway::{command_risk, risk_of, Risk};

/// How much an agent may do without asking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Autonomy {
    /// Only reading.
    Cautious,
    /// Everything below reaching off the machine: reading, working in its
    /// folder, ordinary commands, using its own browser. The gateway's own
    /// line between asking and not.
    #[default]
    Balanced,
    /// Everything except destroying, secrets and money.
    Autonomous,
}

impl Autonomy {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "cautious" => Some(Self::Cautious),
            "balanced" => Some(Self::Balanced),
            "autonomous" => Some(Self::Autonomous),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cautious => "cautious",
            Self::Balanced => "balanced",
            Self::Autonomous => "autonomous",
        }
    }

    /// Whether a call at `risk` may run without anyone being asked.
    pub fn allows(self, risk: Risk) -> bool {
        if !risk.allows_standing_grant() {
            return false;
        }
        match self {
            Self::Cautious => risk == Risk::Read,
            Self::Balanced => risk < Risk::ExternalSideEffect,
            Self::Autonomous => risk < Risk::CredentialAccess,
        }
    }
}

/// The risk of one tool call, or `None` for a tool this does not know.
pub fn classify(tool: &str, input: &Value, cwd: Option<&str>) -> Option<Risk> {
    if let Some(rest) = tool.strip_prefix("mcp__") {
        let (server, name) = rest.split_once("__")?;
        return match server {
            "unterm" => unterm_method_risk(name, input),
            "unzoo" => unzoo_risk(name),
            _ => None,
        };
    }
    let lower = tool.to_ascii_lowercase();
    let name = lower.as_str();
    if crate::agent_policy::is_shell_tool(name) || name == "execute" {
        return Some(command_of(input).map_or(Risk::Exec, |c| command_risk(&c)));
    }
    match name {
        "read" | "read_file" | "view_file" | "view" | "glob" | "grep" | "grep_search" | "search"
        | "find_by_name" | "codebase_search" | "list" | "ls" | "list_dir" | "list_directory" | "todowrite"
        | "todoread" | "todo_write" | "update_plan" | "think" | "task" | "agent" | "skill" | "websearch"
        | "web_search" | "search_web" | "exitplanmode" | "bashoutput" | "view_image" | "notebookread" | "toolsearch"
        | "listmcpresourcestool" | "readmcpresourcetool" => {
            Some(Risk::Read)
        }
        // Fetching a page reads it, but the request leaves the machine with
        // whatever the URL carries: not a silent read.
        "webfetch" | "web_fetch" | "fetch" | "read_url_content" => Some(Risk::Exec),
        "edit" | "write" | "multiedit" | "notebookedit" | "write_file" | "write_to_file" | "replace_file_content"
        | "multi_replace_file_content" | "create_file" | "str_replace" | "str_replace_editor" | "insert" | "patch"
        | "apply_patch" | "move" => Some(edit_risk(input, cwd)),
        "delete" | "delete_file" => Some(Risk::Destructive),
        "killshell" | "killbash" => Some(Risk::LocalMutation),
        _ => None,
    }
}

/// The command a shell tool was asked to run: a string, or Codex's argv.
pub fn command_of(input: &Value) -> Option<String> {
    for key in ["command", "CommandLine", "cmd", "commandLine"] {
        match input.get(key) {
            Some(Value::String(text)) => return Some(text.clone()),
            Some(Value::Array(parts)) => {
                let words: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
                if !words.is_empty() {
                    return Some(words.join(" "));
                }
            }
            _ => {}
        }
    }
    None
}

/// Every file an edit names: Claude's `file_path`/`notebook_path`, the
/// ACP agents' `path`/`TargetFile`, Codex's patch entries.
fn edited_paths(input: &Value) -> Vec<String> {
    fn take(value: &Value, out: &mut Vec<String>) {
        for key in ["file_path", "notebook_path", "path", "TargetFile", "target_file", "destination"] {
            if let Some(path) = value.get(key).and_then(Value::as_str) {
                out.push(path.to_string());
            }
        }
    }
    let mut out = Vec::new();
    match input {
        Value::Array(entries) => entries.iter().for_each(|entry| take(entry, &mut out)),
        Value::Object(map) => {
            take(input, &mut out);
            match map.get("changes") {
                Some(Value::Object(changes)) => out.extend(changes.keys().cloned()),
                Some(Value::Array(entries)) => entries.iter().for_each(|entry| take(entry, &mut out)),
                _ => {}
            }
        }
        _ => {}
    }
    out
}

fn deletes(input: &Value) -> bool {
    let kind_is_delete = |value: &Value| {
        value.get("kind").and_then(|k| k.as_str().or_else(|| k.get("type").and_then(Value::as_str))) == Some("delete")
    };
    match input {
        Value::Array(entries) => entries.iter().any(kind_is_delete),
        Value::Object(map) => {
            kind_is_delete(input)
                || map.get("changes").is_some_and(|changes| match changes {
                    Value::Array(entries) => entries.iter().any(kind_is_delete),
                    Value::Object(by_path) => by_path.values().any(|v| v.get("delete").is_some() || kind_is_delete(v)),
                    _ => false,
                })
        }
        _ => false,
    }
}

/// An edit inside the folder the agent works in is the work; one outside it
/// reaches into the rest of the person's machine, which is asked about
/// unless they chose to let the agent roam.
fn edit_risk(input: &Value, cwd: Option<&str>) -> Risk {
    if deletes(input) {
        return Risk::Destructive;
    }
    let paths = edited_paths(input);
    let Some(cwd) = cwd.filter(|c| !c.is_empty()) else { return Risk::ExternalSideEffect };
    if paths.is_empty() {
        return Risk::ExternalSideEffect;
    }
    let root = lexical(Path::new(cwd));
    let inside = paths.iter().all(|path| {
        let path = Path::new(path);
        let full = if path.is_absolute() || path.has_root() { lexical(path) } else { lexical(&root.join(path)) };
        starts_with(&full, &root)
    });
    if inside {
        Risk::LocalMutation
    } else {
        Risk::ExternalSideEffect
    }
}

/// `a/./b/../c` as `a/c`, without touching the disk (the file may not exist
/// yet).
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn starts_with(path: &Path, root: &Path) -> bool {
    let norm = |p: &Path| {
        let text = p.display().to_string().replace('\\', "/");
        let text = text.trim_start_matches("//?/").trim_end_matches('/').to_string();
        if cfg!(windows) {
            text.to_ascii_lowercase()
        } else {
            text
        }
    };
    let (path, root) = (norm(path), norm(root));
    path == root || path.starts_with(&format!("{root}/"))
}

/// Unterm's MCP method, from the name a CLI gives it. Claude Code turns
/// `exec.run_wait` into `exec_run_wait`, which could be read back more than
/// one way; every reading the gateway knows is tried and the riskiest wins.
fn unterm_method_risk(name: &str, input: &Value) -> Option<Risk> {
    let mut candidates = vec![name.to_string()];
    for (at, c) in name.char_indices() {
        if c == '_' {
            candidates.push(format!("{}.{}", &name[..at], &name[at + 1..]));
        }
    }
    let method_risk = candidates.iter().filter_map(|method| risk_of(method).map(|r| (method, r))).max_by_key(|(_, r)| *r)?;
    let (method, mut risk) = (method_risk.0.clone(), method_risk.1);
    // Starting an agent of its own choosing, on its own terms, or answering
    // for the person stands in for the person's authority: never unasked.
    // Handing work on is `agent_session.delegate`, which keeps the asking
    // agent's terms and is judged as running a program.
    const ACTS_FOR_THE_PERSON: &[&str] = &[
        "agent_session.start",
        "agent_session.resume",
        "agent_session.submit_input",
        "agent_session.decide",
        "approval.decide",
        "agent.trust",
        "agent.untrust",
    ];
    if ACTS_FOR_THE_PERSON.contains(&method.as_str()) {
        risk = risk.max(Risk::CredentialAccess);
    }
    // A command run through the terminal is judged on what it runs, as one
    // run through the agent's own shell would be.
    if method.starts_with("exec.") || method == "session.input" || method == "session.paste" {
        let typed = command_of(input).or_else(|| {
            ["text", "input", "data"].iter().find_map(|k| input.get(*k).and_then(Value::as_str).map(str::to_string))
        });
        if let Some(command) = typed {
            risk = risk.max(command_risk(&command));
        }
    }
    Some(risk)
}

/// Unzoo Browser's tools. The agent browses in its own isolated profile:
/// looking at a page is a read, going somewhere and working a page are what
/// a browser is for, and handing over files, cookies or identities is not.
fn unzoo_risk(name: &str) -> Option<Risk> {
    const READ: &[&str] = &[
        "browser_a11y_snapshot", "browser_snapshot", "browser_screenshot", "browser_get_text", "browser_get_html",
        "browser_get_title", "browser_get_url", "browser_get_attribute", "browser_get_bounding_box",
        "browser_get_console_messages", "browser_get_network_requests", "browser_query_selector_all",
        "browser_find", "browser_element_exists", "browser_wait_for", "browser_wait_for_selector",
        "browser_wait_for_navigation", "browser_wait_for_network_idle", "browser_network_wait_for",
        "browser_delay", "browser_scroll", "extract_form_fields", "extract_images", "extract_links",
        "extract_structured", "extract_structured_data", "extract_table", "page_analyze", "tab_list",
        "tab_get_info", "profile_list", "profile_get_current", "session_info", "service_status",
        "service_version", "skill_list", "skill_get", "media_capture_list", "media_capture_get",
        "media_deps_status", "human_status", "human_profile_get", "lease_list", "file_list", "file_read",
        "youtube_transcript",
    ];
    const GOES_OUT: &[&str] = &[
        "browser_navigate", "browser_navigate_back", "browser_navigate_forward", "browser_reload", "tab_create",
        "tab_activate", "download_url", "media_download_smart", "ytdlp_download", "lease_request",
    ];
    const WORKS_THE_PAGE: &[&str] = &[
        "browser_click", "browser_double_click", "browser_type", "browser_input_text", "browser_fill_form",
        "browser_select_option", "browser_check", "browser_press_key", "browser_hover", "browser_focus",
        "browser_drag_and_drop", "browser_dismiss_blocking_dialog", "browser_print_pdf", "page_click",
        "page_type", "page_select", "page_batch_actions", "page_cleanup", "human_click", "human_type",
        "human_select", "human_task", "human_wait", "human_profile_set", "tab_close", "file_write",
        "media_capture_start", "media_capture_stop", "media_capture_remove", "workspace_set", "tool_profile_set",
    ];
    const SENDS_FILES: &[&str] = &["browser_upload_trusted", "browser_set_input_files", "human_upload", "media_deps_ensure"];
    const SECRETS: &[&str] = &[
        "cookie", "cookie_get_all", "cookie_set", "cookie_import", "cookie_delete", "download_with_cookies",
    ];
    if READ.contains(&name) {
        Some(Risk::Read)
    } else if GOES_OUT.contains(&name) {
        Some(Risk::Exec)
    } else if WORKS_THE_PAGE.contains(&name) {
        Some(Risk::LocalMutation)
    } else if SENDS_FILES.contains(&name) {
        Some(Risk::ExternalSideEffect)
    } else if SECRETS.contains(&name) {
        Some(Risk::CredentialAccess)
    } else if name == "session_clear" {
        Some(Risk::Destructive)
    } else if name == "lease_grant" {
        // Granting is a person's act; an agent granting itself is not.
        Some(Risk::Destructive)
    } else {
        None
    }
}

// ---- saying what a call did ----------------------------------------------

/// What kind of thing a call did, for a person reading the conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verb {
    /// Ran a command.
    Run,
    Read,
    Search,
    Edit,
    Delete,
    /// Opened a terminal.
    Terminal,
    /// Read the terminal's screen or output.
    Watch,
    /// Opened a web page.
    Open,
    /// Looked at the page that is open.
    Look,
    Click,
    Type,
    Download,
    Upload,
    Plan,
    /// Loaded a skill.
    Skill,
    /// Handed work to another agent.
    Delegate,
    /// Asked the person to take over (sign in, solve a captcha).
    Handover,
    /// Anything else, named by its tool.
    Use,
}

/// One line about a call: what it did, and to what (a command, a path, a
/// URL), cut to fit a line.
pub fn describe(tool: &str, input: &Value) -> (Verb, String) {
    describe_in(tool, input, None)
}

/// [`describe`], with paths inside `cwd` written relative to it: the folder
/// the agent works in is where everything happens, and repeating it on every
/// line hides the part that differs.
pub fn describe_in(tool: &str, input: &Value, cwd: Option<&str>) -> (Verb, String) {
    let (verb, target) = describe_full(tool, input);
    let target = match cwd.filter(|c| !c.trim().is_empty()) {
        Some(cwd) => relative_to(&target, cwd),
        None => target,
    };
    (verb, clip(target.trim(), 160))
}

/// `text` with every occurrence of `cwd` and a separator after it taken out,
/// whichever way its slashes lean and, on Windows, whatever its case; the
/// folder itself becomes `.`.
fn relative_to(text: &str, cwd: &str) -> String {
    let root = cwd.trim_end_matches(['/', '\\']);
    if root.is_empty() {
        return text.to_string();
    }
    let fold = |s: &str| {
        let s = s.replace('\\', "/");
        if cfg!(windows) {
            s.to_ascii_lowercase()
        } else {
            s
        }
    };
    let needle = fold(root);
    // Folding keeps byte offsets (ASCII-only changes), so a match in the
    // folded text is the same span in the original.
    let haystack = fold(text);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(found) = haystack[at..].find(&needle) {
        let start = at + found;
        let end = start + needle.len();
        out.push_str(&text[at..start]);
        match text[end..].chars().next() {
            Some('/' | '\\') => at = end + 1,
            Some(c) if !c.is_whitespace() && c != '"' && c != '\'' => {
                // A longer name that merely starts like the folder.
                out.push_str(&text[start..end]);
                at = end;
            }
            _ => {
                out.push('.');
                at = end;
            }
        }
    }
    out.push_str(&text[at..]);
    out
}

fn describe_full(tool: &str, input: &Value) -> (Verb, String) {
    let field = |keys: &[&str]| keys.iter().find_map(|k| input.get(*k).and_then(Value::as_str)).unwrap_or_default().to_string();
    if let Some(rest) = tool.strip_prefix("mcp__") {
        let (server, name) = rest.split_once("__").unwrap_or(("", rest));
        let pretty = name.replace('_', " ");
        if server == "unterm" {
            if name.replace('.', "_") == "agent_session_delegate" {
                let task = field(&["prompt"]);
                let agent = field(&["agent", "session_id"]);
                return (Verb::Delegate, if task.is_empty() { agent } else { format!("{agent}: {task}") });
            }
            if name.replace('.', "_") == "agent_session_handover" {
                return (Verb::Handover, field(&["what", "handover_id"]));
            }
            if name.starts_with("exec_") || name == "session_input" || name == "session_paste" {
                let command = command_of(input).unwrap_or_else(|| field(&["text", "input", "data"]));
                return (Verb::Run, command);
            }
            if name == "session_create" {
                return (Verb::Terminal, field(&["cwd", "name", "title"]));
            }
            if name.starts_with("screen_") || name.starts_with("session_") {
                return (Verb::Watch, field(&["session_id", "id"]));
            }
            return (Verb::Use, pretty);
        }
        if server == "unzoo" {
            let url = field(&["url", "href"]);
            return match name {
                "browser_navigate" | "tab_create" => (Verb::Open, url),
                "download_url" | "media_download_smart" | "ytdlp_download" | "download_with_cookies" => {
                    (Verb::Download, url)
                }
                "browser_upload_trusted" | "browser_set_input_files" | "human_upload" => {
                    (Verb::Upload, field(&["path", "file", "files"]))
                }
                n if n.contains("click") || n == "browser_check" || n == "browser_select_option" || n == "page_select" => {
                    (Verb::Click, field(&["ref", "selector", "text", "element", "label"]))
                }
                n if n.contains("type") || n == "browser_input_text" || n == "browser_fill_form" || n == "browser_press_key" => {
                    (Verb::Type, field(&["ref", "selector", "key", "element"]))
                }
                n if n.starts_with("browser_") || n.starts_with("extract_") || n == "page_analyze" => (Verb::Look, url),
                _ => (Verb::Use, pretty),
            };
        }
        return (Verb::Use, format!("{server} {pretty}").trim().to_string());
    }
    let lower = tool.to_ascii_lowercase();
    if crate::agent_policy::is_shell_tool(&lower) || lower == "execute" {
        return (Verb::Run, command_of(input).unwrap_or_default());
    }
    match lower.as_str() {
        "read" | "read_file" | "view_file" | "view" | "notebookread" | "view_image" => {
            (Verb::Read, field(&["file_path", "path", "notebook_path", "AbsolutePath", "target_file"]))
        }
        "glob" | "grep" | "grep_search" | "search" | "find_by_name" | "codebase_search" | "websearch"
        | "web_search" | "search_web" => (Verb::Search, field(&["pattern", "query", "Query", "Pattern", "SearchPath"])),
        "list" | "ls" | "list_dir" | "list_directory" => (Verb::Read, field(&["path", "DirectoryPath", "directory"])),
        "webfetch" | "web_fetch" | "fetch" | "read_url_content" => (Verb::Open, field(&["url", "Url"])),
        "edit" | "write" | "multiedit" | "notebookedit" | "write_file" | "write_to_file" | "replace_file_content"
        | "multi_replace_file_content" | "create_file" | "str_replace" | "str_replace_editor" | "insert" | "patch"
        | "apply_patch" | "move" => {
            let verb = if deletes(input) { Verb::Delete } else { Verb::Edit };
            (verb, edited_paths(input).join(", "))
        }
        "delete" | "delete_file" => (Verb::Delete, field(&["path", "file_path"])),
        "todowrite" | "todo_write" | "update_plan" | "todoread" | "exitplanmode" => (Verb::Plan, String::new()),
        "skill" => (Verb::Skill, field(&["skill", "command", "name"])),
        _ => (Verb::Use, tool.to_string()),
    }
}

fn clip(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or_default();
    let more = line.len() < text.len();
    let chars: Vec<char> = line.chars().collect();
    if chars.len() > max {
        format!("{}…", chars[..max].iter().collect::<String>())
    } else if more {
        format!("{line} …")
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CWD: &str = if cfg!(windows) { r"C:\Users\me\Documents\Unzoo One\report" } else { "/home/me/Documents/Unzoo One/report" };

    fn risk(tool: &str, input: Value) -> Option<Risk> {
        classify(tool, &input, Some(CWD))
    }

    #[test]
    fn autonomy_levels_draw_their_lines_where_the_table_says() {
        use Risk::*;
        let all = [Read, LocalMutation, Exec, ExternalSideEffect, CredentialAccess, Financial, Destructive];
        let allowed = |a: Autonomy| all.iter().copied().filter(|r| a.allows(*r)).collect::<Vec<_>>();
        assert_eq!(allowed(Autonomy::Cautious), vec![Read]);
        assert_eq!(allowed(Autonomy::Balanced), vec![Read, LocalMutation, Exec]);
        assert_eq!(allowed(Autonomy::Autonomous), vec![Read, LocalMutation, Exec, ExternalSideEffect]);
        assert_eq!(Autonomy::default(), Autonomy::Balanced);
        assert_eq!(Autonomy::parse("Autonomous"), Some(Autonomy::Autonomous));
        assert_eq!(Autonomy::parse("yolo"), None);
    }

    #[test]
    fn claude_codes_own_tools() {
        assert_eq!(risk("Read", json!({"file_path": "/etc/hosts"})), Some(Risk::Read));
        assert_eq!(risk("Grep", json!({"pattern": "x"})), Some(Risk::Read));
        assert_eq!(risk("Bash", json!({"command": "npm test"})), Some(Risk::Exec));
        assert_eq!(risk("Bash", json!({"command": "rm -rf node_modules"})), Some(Risk::Destructive));
        assert_eq!(risk("PowerShell", json!({"command": "git push"})), Some(Risk::ExternalSideEffect));
        assert_eq!(risk("WebFetch", json!({"url": "https://example.com"})), Some(Risk::Exec));
        assert_eq!(risk("SomethingNew", json!({})), None);
    }

    #[test]
    fn an_edit_is_judged_by_where_the_file_is() {
        let inside = Path::new(CWD).join("out").join("summary.md").display().to_string();
        assert_eq!(risk("Write", json!({"file_path": inside})), Some(Risk::LocalMutation));
        assert_eq!(risk("Edit", json!({"file_path": "notes/a.md"})), Some(Risk::LocalMutation));
        assert_eq!(risk("Edit", json!({"file_path": "../other/a.md"})), Some(Risk::ExternalSideEffect));
        let outside = if cfg!(windows) { r"C:\Windows\System32\drivers\etc\hosts" } else { "/etc/hosts" };
        assert_eq!(risk("Write", json!({"file_path": outside})), Some(Risk::ExternalSideEffect));
        // A sibling whose name starts like the folder is not inside it.
        let sibling = format!("{CWD}-old{}a.md", std::path::MAIN_SEPARATOR);
        assert_eq!(risk("Write", json!({"file_path": sibling})), Some(Risk::ExternalSideEffect));
        // No folder to judge by: asked.
        assert_eq!(classify("Write", &json!({"file_path": "a.md"}), None), Some(Risk::ExternalSideEffect));
    }

    #[test]
    fn asking_the_person_to_take_over_runs_nothing() {
        let input = json!({"what": "Sign in to GitHub", "tab_id": "42"});
        assert_eq!(risk("mcp__unterm__agent_session_handover", input.clone()), Some(Risk::Read));
        assert_eq!(
            describe("mcp__unterm__agent_session_handover", &input),
            (Verb::Handover, "Sign in to GitHub".to_string())
        );
    }

    #[test]
    fn handing_work_on_is_running_a_program_but_acting_for_the_person_is_always_asked() {
        let delegate = risk("mcp__unterm__agent_session_delegate", json!({"agent": "codex", "prompt": "x"}));
        assert_eq!(delegate, Some(Risk::Exec));
        assert!(Autonomy::Balanced.allows(Risk::Exec));
        for tool in [
            "mcp__unterm__agent_session_start",
            "mcp__unterm__agent_session_decide",
            "mcp__unterm__approval_decide",
            "mcp__unterm__agent_trust",
        ] {
            let level = risk(tool, json!({"command": ["claude", "--dangerously-skip-permissions"]})).unwrap();
            assert!(!Autonomy::Autonomous.allows(level), "{tool} ran unasked at {level:?}");
        }
    }

    #[test]
    fn codex_and_acp_tools() {
        assert_eq!(risk("shell", json!({"command": ["bash", "-lc", "cargo test"]})), Some(Risk::Exec));
        assert_eq!(risk("shell", json!({"command": ["bash", "-lc", "git reset --hard"]})), Some(Risk::Destructive));
        assert_eq!(risk("apply_patch", json!([{"path": "a.txt", "kind": "add"}])), Some(Risk::LocalMutation));
        assert_eq!(risk("apply_patch", json!([{"path": "a.txt", "kind": "delete"}])), Some(Risk::Destructive));
        assert_eq!(risk("run_command", json!({"CommandLine": "dir"})), Some(Risk::Exec));
        assert_eq!(risk("execute", json!({"command": "curl -X POST https://x -d 1"})), Some(Risk::ExternalSideEffect));
        assert_eq!(risk("view_file", json!({"AbsolutePath": "/x"})), Some(Risk::Read));
    }

    #[test]
    fn unterm_methods_are_read_back_from_the_cli_names() {
        assert_eq!(risk("mcp__unterm__session_create", json!({})), Some(Risk::Exec));
        assert_eq!(risk("mcp__unterm__screen_read", json!({})), Some(Risk::Read));
        assert_eq!(risk("mcp__unterm__exec_run_wait", json!({"command": "npm test"})), Some(Risk::Exec));
        assert_eq!(risk("mcp__unterm__exec_run_wait", json!({"command": "rm -rf /"})), Some(Risk::Destructive));
        assert_eq!(risk("mcp__unterm__session_input", json!({"text": "git push\r"})), Some(Risk::ExternalSideEffect));
        assert_eq!(risk("mcp__unterm__session_destroy", json!({})), Some(Risk::Destructive));
        assert_eq!(risk("mcp__unterm__no_such_thing", json!({})), None);
        assert_eq!(risk("mcp__elsewhere__anything", json!({})), None);
    }

    #[test]
    fn unzoo_browser_tools() {
        assert_eq!(risk("mcp__unzoo__browser_a11y_snapshot", json!({})), Some(Risk::Read));
        assert_eq!(risk("mcp__unzoo__browser_navigate", json!({"url": "https://x"})), Some(Risk::Exec));
        assert_eq!(risk("mcp__unzoo__browser_click", json!({"ref": "ref=3"})), Some(Risk::LocalMutation));
        assert_eq!(risk("mcp__unzoo__browser_upload_trusted", json!({})), Some(Risk::ExternalSideEffect));
        assert_eq!(risk("mcp__unzoo__cookie_get_all", json!({})), Some(Risk::CredentialAccess));
        assert_eq!(risk("mcp__unzoo__lease_grant", json!({})), Some(Risk::Destructive));
        assert_eq!(risk("mcp__unzoo__brand_new_tool", json!({})), None);
    }

    #[test]
    fn calls_are_described_in_one_line() {
        assert_eq!(describe("Bash", &json!({"command": "npm test\nnpm run lint"})), (Verb::Run, "npm test …".into()));
        assert_eq!(describe("Write", &json!({"file_path": "out/report.md"})), (Verb::Edit, "out/report.md".into()));
        assert_eq!(
            describe("mcp__unzoo__browser_navigate", &json!({"url": "https://example.com"})),
            (Verb::Open, "https://example.com".into())
        );
        assert_eq!(
            describe("mcp__unterm__exec_run_wait", &json!({"command": "cargo build"})),
            (Verb::Run, "cargo build".into())
        );
        assert_eq!(describe("TodoWrite", &json!({"todos": []})).0, Verb::Plan);
        assert_eq!(describe("Skill", &json!({"skill": "unzoo-one:unzoo-browser"})), (Verb::Skill, "unzoo-one:unzoo-browser".into()));
        assert_eq!(
            describe("mcp__unterm__agent_session_delegate", &json!({"agent": "codex", "prompt": "fix the tests"})),
            (Verb::Delegate, "codex: fix the tests".into())
        );
        assert_eq!(describe("mcp__unterm__agent_session_delegate", &json!({"session_id": "as_1"})).1, "as_1");
        // Inside the folder it works in, paths are written relative to it.
        let file = Path::new(CWD).join("out").join("a.md").display().to_string();
        let (_, short) = describe_in("Write", &json!({"file_path": file}), Some(CWD));
        assert_eq!(short.replace('\\', "/"), "out/a.md");
        let (_, command) = describe_in("Bash", &json!({"command": format!("cd \"{CWD}\" && ls")}), Some(CWD));
        assert_eq!(command, "cd \".\" && ls");
        let sibling = format!("{CWD}-old");
        assert_eq!(describe_in("Read", &json!({"file_path": sibling.clone()}), Some(CWD)).1, sibling);
        let (_, long) = describe("Bash", &json!({"command": "x".repeat(400)}));
        assert_eq!(long.chars().count(), 161);
    }
}
