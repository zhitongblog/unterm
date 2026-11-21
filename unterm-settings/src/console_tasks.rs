//! The console's tasks, owned by the runtime.
//!
//! The Unzoo One console started out keeping its tasks in the browser and
//! driving each agent run from the page: close the tab and the run had nobody
//! following it, open another browser and the tasks were not there, and a
//! task set to run every morning only ran if a page happened to be open at
//! nine. All three are the runtime's job, so they live here:
//!
//! - **Tasks** are rows in the task engine (`kind = "console"`). The console
//!   names them (local first), and its own view of a task rides along as an
//!   opaque `console` blob the runtime never interprets. What the runtime does
//!   read -- the prompt, the schedule, whether it is paused -- is sent as its
//!   own fields, so nothing here depends on the console's wording.
//! - **Runs** are engine runs, each with one `agent_turn` step that carries
//!   the session, the agent's answer and the token count. The runtime starts
//!   the agent, follows it to the end of its turn and writes the verdict; the
//!   console only reads.
//! - **Schedules** are checked every half minute. A daily, weekday or weekly
//!   task whose slot has come since it last ran is run once -- a slot missed
//!   while the machine was off is caught up once, not once per missed day.
//!   A task can also run every N minutes, or once at a given moment.
//! - **Watches** start a run when something changes: a web page, a folder
//!   (see `console_watch`).
//! - **Standing helpers** are tasks with an agent of their own (not the run
//!   settings' one) that keep their conversation: each scheduled or watched
//!   run continues the last, so the agent remembers what it did before.
//!
//! A run is always hosted: every tool call the agent's CLI would ask about
//! comes to the console's inbox, whether or not anyone is looking at a page.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Datelike, NaiveTime, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use unterm_mcp::handler::McpHandler;
use unterm_services::agent_session;
use unterm_services::tool_risk::{self, Autonomy};
use unterm_tasks::{Run, RunId, State, Step, StepId, Task, TaskId, TaskStore};

use super::console_watch::Watch;

use super::server::Response;

const KIND: &str = "console";
const STEP_KIND: &str = "agent_turn";
/// A turn that has produced nothing -- no output, no question waiting on a
/// person -- for this long is stuck, not thinking.
const IDLE_LIMIT: Duration = Duration::from_secs(10 * 60);
const POLL: Duration = Duration::from_millis(700);
/// The most tool calls one turn keeps a line for.
const MAX_ACTIVITY: usize = 300;
/// Tools a CLI calls to manage itself rather than to do the work.
const HOUSEKEEPING_TOOLS: &[&str] = &["ToolSearch", "ListMcpResourcesTool", "ReadMcpResourceTool"];
/// The most of a reply one turn keeps (bytes).
const MAX_REPLY: usize = 64 * 1024;

// ---- run settings --------------------------------------------------------

/// How the runtime runs a task: which agent, in which directory, governed or
/// not. One set for the whole console -- the scheduler needs them with no
/// page open, so they live here and not in a browser.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunSettings {
    pub agent: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default = "yes")]
    pub governed: bool,
    /// How much the agent may do without asking (see
    /// `unterm_services::tool_risk`).
    #[serde(default)]
    pub autonomy: Autonomy,
    /// The Unzoo Browser profile the agents browse in. Empty: their own
    /// (`browser::RESERVED_PROFILE`).
    #[serde(default)]
    pub browser_profile: String,
}

fn yes() -> bool {
    true
}

impl Default for RunSettings {
    fn default() -> Self {
        Self {
            agent: "claude-code".into(),
            cwd: String::new(),
            governed: true,
            autonomy: Autonomy::default(),
            browser_profile: String::new(),
        }
    }
}

fn settings_path() -> Result<std::path::PathBuf> {
    unterm_protocol::state_path("console_run.json").context("could not resolve the state directory")
}

pub fn run_settings() -> RunSettings {
    settings_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Check and keep new settings. An agent the console cannot drive hosted,
/// or a directory that is not there, is refused here rather than at nine in
/// the morning when the schedule fires.
pub fn set_run_settings(next: RunSettings) -> Result<RunSettings> {
    let next = RunSettings { agent: next.agent.trim().into(), cwd: next.cwd.trim().into(), ..next };
    if !super::agent_run::supports_hosted(&next.agent) {
        bail!("{} cannot be run hosted, so its tool calls could not reach the inbox", next.agent);
    }
    if !next.cwd.is_empty() && !std::path::Path::new(&next.cwd).is_dir() {
        bail!("not a directory: {}", next.cwd);
    }
    let path = settings_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(&next)?)?;
    std::fs::rename(&temp, &path)?;
    Ok(next)
}

// ---- where a task works --------------------------------------------------

/// Where tasks get their own folder when the run settings name none:
/// `Documents/<product>`, a place a person finds their files in. Not the
/// directory the service happened to start in -- for an installed service
/// that is the install directory, where an agent cannot write.
pub fn work_root() -> Option<std::path::PathBuf> {
    if let Some(root) = std::env::var_os("UNTERM_WORK_ROOT").filter(|v| !v.is_empty()) {
        return Some(root.into());
    }
    let base = dirs_next::document_dir().or_else(dirs_next::home_dir)?;
    Some(base.join(unterm_protocol::product_name()))
}

/// A folder name from a task's title: what Windows, macOS and Linux all
/// accept, short, and never a device name.
pub fn folder_name(title: &str, fallback: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { ' ' } else { c })
        .collect();
    let mut name = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.chars().count() > 60 {
        name = name.chars().take(60).collect();
    }
    let name = name.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    if name.is_empty() {
        return fallback.to_string();
    }
    let stem = name.split('.').next().unwrap_or_default().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        format!("{name} task")
    } else {
        name
    }
}

/// The directory a run of `task` works in: the one the run settings name,
/// or else the task's own folder, picked on its first run and kept, so a
/// renamed task keeps its files where they were.
fn workdir_for(store: &TaskStore, task: &Task, settings: &RunSettings) -> Result<String> {
    if !settings.cwd.is_empty() {
        return Ok(settings.cwd.clone());
    }
    if let Some(kept) = task.detail.get("workdir").and_then(Value::as_str) {
        std::fs::create_dir_all(kept).with_context(|| format!("could not make {kept}"))?;
        return Ok(kept.to_string());
    }
    let root = work_root().ok_or_else(|| anyhow!("there is no documents or home folder to work in"))?;
    let taken: HashSet<String> = store
        .tasks()?
        .iter()
        .filter(|t| t.kind == KIND && t.id != task.id)
        .filter_map(|t| t.detail.get("workdir").and_then(Value::as_str).map(str::to_string))
        .collect();
    let base = folder_name(&task.title, &console_id_of(task));
    let mut dir = root.join(&base);
    let mut n = 2;
    while taken.contains(&dir.display().to_string()) {
        dir = root.join(format!("{base} ({n})"));
        n += 1;
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("could not make {}", dir.display()))?;
    let dir = dir.display().to_string();
    let mut detail = task.detail.clone();
    detail["workdir"] = json!(dir);
    store.set_task_detail(&task.id, detail)?;
    Ok(dir)
}

// ---- what a run made -----------------------------------------------------

/// Directories that hold tools' own state, never a deliverable.
const SKIPPED_DIRS: &[&str] = &[".git", "node_modules", "target", ".venv", "venv", "__pycache__", ".next", ".cache"];
const MAX_OUTPUTS: usize = 50;
const MAX_VISITED: usize = 20_000;
const MAX_DEPTH: usize = 8;

/// One file a run wrote, relative to the directory it worked in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub path: String,
    pub size: u64,
    pub modified: String,
}

/// The files under `dir` written at or after `since`: what the run made or
/// changed. Newest first, at most fifty; hidden files and tools' own
/// directories are not deliverables and are skipped.
pub fn outputs_since(dir: &std::path::Path, since: std::time::SystemTime) -> Vec<Output> {
    let mut found: Vec<(std::time::SystemTime, Output)> = Vec::new();
    let mut visited = 0usize;
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((current, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else { continue };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_VISITED {
                break;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if depth + 1 < MAX_DEPTH && !SKIPPED_DIRS.contains(&name.as_str()) {
                    stack.push((entry.path(), depth + 1));
                }
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(modified) = meta.modified() else { continue };
            if modified < since {
                continue;
            }
            let Ok(relative) = entry.path().strip_prefix(dir).map(|p| p.to_path_buf()) else { continue };
            let path = relative.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            found.push((
                modified,
                Output {
                    path,
                    size: meta.len(),
                    modified: chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339(),
                },
            ));
        }
        if visited > MAX_VISITED {
            break;
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.path.cmp(&b.1.path)));
    found.into_iter().take(MAX_OUTPUTS).map(|(_, output)| output).collect()
}

/// Of the files a run wrote, the ones its reply names: what it made for the
/// person. The rest -- a repository it cloned, a script it wrote to scroll a
/// page, data it kept along the way -- was its working, not the result. A
/// file is named by its path or by its file name.
/// When the reply names none of them (an agent that did not say), all are
/// kept: too many is better than a result that is not there.
pub fn named_in(outputs: Vec<Output>, reply: &str) -> Vec<Output> {
    let reply = reply.replace('\\', "/");
    if !outputs.iter().any(|output| names(&reply, &output.path)) {
        return outputs;
    }
    outputs.into_iter().filter(|output| names(&reply, &output.path)).collect()
}

fn names(reply: &str, path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    reply.contains(path) || (!name.is_empty() && reply.contains(name))
}

/// A run's outputs as the console lists them. Runs recorded before
/// [`named_in`] kept everything they wrote; they are read the same way.
fn listed_outputs(detail: &Value) -> Value {
    let Some(outputs) = detail.get("outputs").and_then(Value::as_array) else { return json!([]) };
    let Some(reply) = detail.get("reply").and_then(Value::as_str) else { return json!(outputs) };
    let reply = reply.replace('\\', "/");
    let named: Vec<&Value> = outputs
        .iter()
        .filter(|o| o.get("path").and_then(Value::as_str).is_some_and(|path| names(&reply, path)))
        .collect();
    if named.is_empty() { json!(outputs) } else { json!(named) }
}

/// How much of one output the console is shown.
const MAX_READ: u64 = 1024 * 1024;

/// Read one file a run recorded as its output. Only those: the path must
/// be in the run's list and resolve inside the directory it worked in.
pub fn read_output(task_id: &str, console_run: &str, path: &str) -> Result<Value> {
    let store = store()?;
    let task = console_task(&store, task_id)?;
    let run = run_key(console_run)?;
    let step = store
        .steps(&run)?
        .into_iter()
        .find(|s| s.kind == STEP_KIND)
        .ok_or_else(|| anyhow!("no run {console_run}"))?;
    if !store.runs(&task.id)?.iter().any(|r| r.id == run) {
        bail!("{console_run} is not a run of {task_id}");
    }
    let listed = step
        .detail
        .get("outputs")
        .and_then(Value::as_array)
        .is_some_and(|outputs| outputs.iter().any(|o| o.get("path").and_then(Value::as_str) == Some(path)));
    if !listed {
        bail!("{path} is not an output of this run");
    }
    let dir = step
        .detail
        .get("cwd")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the run does not say where it worked"))?;
    let root = std::fs::canonicalize(dir).with_context(|| format!("{dir} is gone"))?;
    let file = std::fs::canonicalize(root.join(path)).with_context(|| format!("{path} is gone"))?;
    if !file.starts_with(&root) {
        bail!("{path} is outside the run's directory");
    }
    let size = std::fs::metadata(&file)?.len();
    let mut bytes = Vec::new();
    use std::io::Read as _;
    std::fs::File::open(&file)?.take(MAX_READ).read_to_end(&mut bytes)?;
    let truncated = size > MAX_READ;
    // A cut can land inside a character; drop the partial one at the end.
    // A byte-order mark is how some editors say "UTF-8", not text.
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").map(<[u8]>::to_vec).unwrap_or(bytes);
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => Some(text.to_string()),
        Err(e) if truncated && e.error_len().is_none() => Some(String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned()),
        Err(_) => None,
    };
    Ok(json!({
        "path": path,
        "absolute_path": file.display().to_string().trim_start_matches(r"\\?\"),
        "size": size,
        "truncated": truncated,
        "binary": text.is_none(),
        "text": text,
    }))
}

// ---- the agent's computer ------------------------------------------------

/// A terminal session or tab id as an argument gives it: a string or a number.
fn scalar_id(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// The value after `"key":` in a tool's output, which reaches us as text --
/// often JSON inside JSON, its quotes escaped. Letters, digits, `_` and `-`.
pub(crate) fn id_after(text: &str, key: &str) -> Option<String> {
    let needle = key;
    let mut from = 0;
    while let Some(found) = text[from..].find(&needle) {
        let after = from + found + needle.len();
        let rest = &text[after..];
        let trimmed = rest.trim_start_matches(|c: char| c == '\\' || c == '"' || c == ' ');
        if let Some(value) = trimmed.strip_prefix(':') {
            let value = value.trim_start_matches(|c: char| c == '\\' || c == '"' || c == ' ');
            let id: String = value.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
            if !id.is_empty() {
                return Some(id);
            }
        }
        from = after;
    }
    None
}

/// Whether an agent run here used this terminal session (`"terminals"`) or
/// tab (`"tabs"`). The console shows only those: its token reaches the whole
/// runtime, but this view is for watching the agents, not for reading every
/// terminal and tab on the machine.
fn used_by_an_agent(kind: &str, id: &str) -> Result<bool> {
    let store = store()?;
    let lists = |detail: &Value| detail.get(kind).and_then(Value::as_array).is_some_and(|ids| ids.iter().any(|v| v == id));
    for task in store.tasks()?.into_iter().filter(|t| t.kind == KIND) {
        for run in store.runs(&task.id)? {
            for step in store.steps(&run.id)? {
                // The conversation's agent, or a helper it handed work to.
                let helpers = step.detail.get("helpers").and_then(Value::as_array);
                if lists(&step.detail) || helpers.is_some_and(|all| all.iter().any(lists)) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// How much of a terminal's history the console shows: enough to scroll
/// back through what an agent did, little enough to send every second.
const TERMINAL_HISTORY_ROWS: i64 = 1500;

/// The most a person types in one go (a paste is more than a keystroke).
const TERMINAL_INPUT_MAX: usize = 16 * 1024;

/// What an agent's terminal printed: the output exactly as the terminal
/// received it, escapes and all, cut into lines -- replayed by a terminal
/// emulator it draws the same screen, colours and cursor. `from_line` asks
/// only for what came since (the line given included, since the last one may
/// have grown); without it, the recent history.
pub fn terminal_screen(handler: &McpHandler, session: &str, from_line: Option<i64>) -> Result<Value> {
    if !used_by_an_agent("terminals", session)? {
        bail!("{session} is not a terminal an agent used");
    }
    read_terminal(handler, session, from_line).or_else(|live| {
        // Closed when its run ended: what it showed last, kept then -- read
        // from `from_line` on like a live one, so a viewer that already has
        // the start keeps it.
        let kept = read_kept("terminal", session).ok_or(live)?;
        Ok(kept_from(kept, from_line))
    })
}

/// A kept terminal view from `from_line` on (all of it without one).
fn kept_from(mut kept: Value, from_line: Option<i64>) -> Value {
    let Some(from) = from_line else { return kept };
    let first = kept.get("first_row").and_then(Value::as_i64).unwrap_or(0);
    let text = kept.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
    let lines: Vec<&str> = if text.is_empty() { Vec::new() } else { text.split('\n').collect() };
    let skip = (from - first).clamp(0, lines.len() as i64) as usize;
    let rest = &lines[skip..];
    kept["first_row"] = json!(first + skip as i64);
    kept["row_count"] = json!(rest.len());
    kept["text"] = json!(rest.join("\n"));
    kept
}

/// Whether a tool is one of an MCP server's, however the agent names it:
/// Claude Code says `mcp__unzoo__tab_create`, Codex `unzoo.tab_create`,
/// OpenCode `unzoo_tab_create`.
fn from_server(tool: &str, server: &str) -> bool {
    tool_risk::mcp_parts(tool).is_some_and(|(of, _)| of == server)
}

/// Closed with its run (see [`tidy_up`]): the last thing it showed.
fn kept_path(kind: &str, id: &str) -> Option<std::path::PathBuf> {
    let safe: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    (!safe.is_empty()).then(|| unterm_protocol::state_path(format!("console_closed/{kind}-{safe}.json"))).flatten()
}

fn keep(kind: &str, id: &str, mut view: Value) {
    let Some(path) = kept_path(kind, id) else { return };
    view["closed"] = json!(true);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, view.to_string());
}

fn read_kept(kind: &str, id: &str) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(kept_path(kind, id)?).ok()?).ok()
}

/// Close the terminals and tabs a finished run opened (and the tabs it
/// used in an agent's own profile), keeping what each showed last. Nothing
/// of the person's is touched: a terminal or a tab in their profile that the
/// run only used was there before it and stays.
fn tidy_up(turns: &[&Turn]) {
    let handler = McpHandler::new();
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    let mut opened_tabs = Vec::new();
    let mut used_tabs = Vec::new();
    for turn in turns {
        for session in &turn.opened_terminals {
            if let Ok(view) = read_terminal(&handler, session, None) {
                keep("terminal", session, view);
            }
            let _ = handler.handle(&ctx, "session.destroy", &json!({"session_id": session}));
        }
        opened_tabs.extend(turn.opened_tabs.iter().cloned());
        used_tabs.extend(turn.tabs.iter().cloned());
    }
    if opened_tabs.is_empty() && used_tabs.is_empty() {
        return;
    }
    let _ = super::browser::close_tabs(&opened_tabs, &used_tabs, |tab| {
        if let Ok(view) = super::browser::tab_view(tab) {
            keep("tab", tab, view);
        }
    });
}

fn read_terminal(handler: &McpHandler, session: &str, from_line: Option<i64>) -> Result<Value> {
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    // The cursor first: it needs the terminal itself, where reading the
    // history of a closed one falls back to whichever is in front.
    let cursor = handler
        .handle(&ctx, "screen.cursor", &json!({"session_id": session}))
        .map_err(|e| anyhow!("{session} is closed: {e}"))?;
    let history = handler
        .handle(
            &ctx,
            "screen.scrollback_text",
            &match from_line {
                Some(from) => json!({"session_id": session, "escapes": true, "start_line": from.max(0)}),
                None => json!({"session_id": session, "escapes": true, "tail_lines": TERMINAL_HISTORY_ROWS}),
            },
        )
        .map_err(|e| anyhow!("{e}"))?;
    let field = |name: &str| history.get(name).cloned().unwrap_or(Value::Null);
    Ok(json!({
        "session_id": session,
        "text": field("text"),
        "first_row": field("first_row"),
        "row_count": field("row_count"),
        "cols": field("cols"),
        "rows": field("viewport_rows"),
        "cursor": cursor,
    }))
}

/// Keys the person typed into an agent's terminal from the console.
pub fn terminal_input(handler: &McpHandler, session: &str, input: &str) -> Result<Value> {
    if input.is_empty() || input.len() > TERMINAL_INPUT_MAX {
        bail!("input must be 1 to {TERMINAL_INPUT_MAX} bytes");
    }
    if !used_by_an_agent("terminals", session)? {
        bail!("{session} is not a terminal an agent used");
    }
    handler.person_input(session, input)?;
    Ok(json!({"session_id": session, "written": input.len()}))
}

/// Bring an agent's terminal to the front of an Unzoo One terminal window,
/// for a person to take over.
pub fn focus_terminal(handler: &McpHandler, session: &str) -> Result<Value> {
    if !used_by_an_agent("terminals", session)? {
        bail!("{session} is not a terminal an agent used");
    }
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    handler.handle(&ctx, "session.focus", &json!({"session_id": session})).map_err(|e| anyhow!("{e}"))
}

/// What an agent's tab shows now: its address, title and a picture.
pub fn tab_view(tab: &str) -> Result<Value> {
    if !used_by_an_agent("tabs", tab)? {
        bail!("{tab} is not a tab an agent used");
    }
    super::browser::tab_view(tab).or_else(|live| read_kept("tab", tab).ok_or(live))
}

/// Bring an agent's tab to the front of Unzoo Browser.
pub fn activate_tab(tab: &str) -> Result<Value> {
    if !used_by_an_agent("tabs", tab)? {
        bail!("{tab} is not a tab an agent used");
    }
    super::browser::activate_tab(tab)
}

// ---- schedules -----------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Schedule {
    Daily { time: String },
    /// Monday to Friday.
    Weekdays { time: String },
    /// `weekday` counts from Monday = 1 to Sunday = 7.
    Weekly { time: String, weekday: u32 },
    /// Every `minutes` after the last run started (or the task was made).
    Every { minutes: u32 },
    /// Once, at a local `YYYY-MM-DDTHH:MM`.
    Once { at: String },
}

pub const MIN_EVERY: u32 = 5;
pub const MAX_EVERY: u32 = 7 * 24 * 60;

impl Schedule {
    /// Refuse a schedule that would never fire, when it is set.
    pub fn check(&self) -> Result<()> {
        match self {
            Schedule::Daily { time } | Schedule::Weekdays { time } | Schedule::Weekly { time, .. } => {
                NaiveTime::parse_from_str(time.trim(), "%H:%M").map_err(|_| anyhow!("not a time of day: {time}"))?;
            }
            Schedule::Every { minutes } if !(MIN_EVERY..=MAX_EVERY).contains(minutes) => {
                bail!("a task repeats every {MIN_EVERY} to {MAX_EVERY} minutes")
            }
            Schedule::Every { .. } => {}
            Schedule::Once { at } => {
                once_moment(at).ok_or_else(|| anyhow!("not a moment: {at}"))?;
            }
        }
        if let Schedule::Weekly { weekday, .. } = self {
            if !(1..=7).contains(weekday) {
                bail!("a weekday is 1 (Monday) to 7 (Sunday)");
            }
        }
        Ok(())
    }
}

fn once_moment(at: &str) -> Option<chrono::NaiveDateTime> {
    let at = at.trim();
    chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M:%S"))
        .ok()
}

/// The most recent moment at or before `now` this schedule asked for a run.
/// `None` for one that repeats by interval, which has no fixed slots.
pub fn last_slot<Tz: TimeZone>(schedule: &Schedule, now: &DateTime<Tz>) -> Option<DateTime<Tz>> {
    let (time, days): (&String, &[u32]) = match schedule {
        Schedule::Daily { time } => (time, &[1, 2, 3, 4, 5, 6, 7]),
        Schedule::Weekdays { time } => (time, &[1, 2, 3, 4, 5]),
        Schedule::Weekly { time, weekday } => (time, std::slice::from_ref(weekday)),
        Schedule::Every { .. } => return None,
        Schedule::Once { at } => {
            let at = now.timezone().from_local_datetime(&once_moment(at)?).earliest()?;
            return (at <= *now).then_some(at);
        }
    };
    let time = NaiveTime::parse_from_str(time.trim(), "%H:%M").ok()?;
    let zone = now.timezone();
    let mut day = now.date_naive();
    for _ in 0..8 {
        if days.contains(&day.weekday().number_from_monday()) {
            // A time that does not exist that day (a DST gap) is skipped; one
            // that exists twice is taken the first time.
            if let Some(at) = zone.from_local_datetime(&day.and_time(time)).earliest() {
                if at <= *now {
                    return Some(at);
                }
            }
        }
        day = day.pred_opt()?;
    }
    None
}

/// Whether a scheduled task is due: its last slot came after it was made
/// and after its last run started.
pub fn is_due<Tz: TimeZone>(
    schedule: &Schedule,
    now: &DateTime<Tz>,
    created_at: &str,
    last_started_at: Option<&str>,
) -> bool {
    let parse = |text: &str| DateTime::parse_from_rfc3339(text).ok();
    let Some(created) = parse(created_at) else { return false };
    if let Schedule::Every { minutes } = schedule {
        if !(MIN_EVERY..=MAX_EVERY).contains(minutes) {
            return false;
        }
        let since = last_started_at.and_then(parse).map_or(created, |started| started.max(created));
        return now.fixed_offset() >= since + chrono::Duration::minutes(i64::from(*minutes));
    }
    let Some(slot) = last_slot(schedule, now) else { return false };
    if slot <= created {
        return false;
    }
    match last_started_at.and_then(parse) {
        Some(started) => started < slot,
        None => true,
    }
}

// ---- ids -----------------------------------------------------------------

/// The console names tasks and runs itself (`task-…`, `run-…`); the engine's
/// ids carry a kind prefix. The console's id is kept as is, behind it.
fn task_key(console_id: &str) -> Result<TaskId> {
    if console_id.is_empty() || console_id.len() > 200 {
        bail!("a task id is 1 to 200 characters");
    }
    TaskId::parse(&format!("tsk_{}", console_id.trim_start_matches("tsk_")))
}

fn run_key(console_id: &str) -> Result<RunId> {
    if console_id.is_empty() || console_id.len() > 200 {
        bail!("a run id is 1 to 200 characters");
    }
    RunId::parse(&format!("run_{}", console_id.trim_start_matches("run_")))
}

fn console_id_of(task: &Task) -> String {
    task.detail
        .get("console_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| task.id.as_str().trim_start_matches("tsk_").to_string())
}

fn store() -> Result<Arc<TaskStore>> {
    unterm_services::cockpit::fleet_store::tasks().ok_or_else(|| anyhow!("there is no task store"))
}

fn console_task(store: &TaskStore, console_id: &str) -> Result<Task> {
    let task = store
        .task(&task_key(console_id)?)?
        .ok_or_else(|| anyhow!("no task {console_id}"))?;
    if task.kind != KIND {
        bail!("{console_id} is not a console task");
    }
    Ok(task)
}

/// Runs the runtime is following in this process. A run the engine says is
/// running and that is not in here has nobody following it -- its process
/// died with the last Unterm -- and is closed as interrupted.
fn following() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Runs a person asked to stop. However the agent's CLI then reports its
/// end -- an interrupted turn, or a process that "exited in the middle of a
/// turn" -- the run was cancelled, not failed.
fn stopping() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

// ---- the views the console reads -----------------------------------------

fn run_view(run: &Run, step: Option<&Step>) -> Value {
    let detail = step.map(|s| s.detail.clone()).unwrap_or_else(|| json!({}));
    let field = |key: &str| detail.get(key).cloned().unwrap_or(Value::Null);
    json!({
        "id": detail.get("console_run_id").and_then(Value::as_str)
            .unwrap_or_else(|| run.id.as_str().trim_start_matches("run_")),
        "attempt": run.attempt,
        "state": run.state.as_str(),
        "started_at": run.started_at,
        "ended_at": run.ended_at,
        "trigger": field("trigger"),
        "agent": field("agent"),
        "adapter": field("adapter"),
        "session_id": field("session_id"),
        "summary": field("summary"),
        "error": field("error"),
        "error_code": field("error_code"),
        "waiting": detail.get("waiting").and_then(Value::as_bool).unwrap_or(false),
        "usage": field("usage"),
        "workdir": field("cwd"),
        "outputs": listed_outputs(&detail),
    })
}

/// A run as one turn of the conversation: [`run_view`] and what was said
/// both ways, and what the agent did in between.
fn turn_view(run: &Run, step: Option<&Step>) -> Value {
    let mut view = run_view(run, step);
    let detail = step.map(|s| &s.detail);
    for key in ["prompt", "reply", "fold", "activity", "continued", "terminals", "tabs", "purpose", "saved_skills", "skill_error", "helpers", "handovers"] {
        view[key] = detail.and_then(|d| d.get(key)).cloned().unwrap_or(Value::Null);
    }
    for key in ["activity", "terminals", "tabs", "helpers"] {
        if view[key].is_null() {
            view[key] = json!([]);
        }
    }
    view
}

/// The most turns a conversation is read back with.
const MAX_THREAD: usize = 500;

/// One task as a conversation: the task, and every run oldest first, each
/// with its prompt, the agent's whole reply and what it did.
pub fn thread(console_id: &str) -> Result<Value> {
    let store = store()?;
    let task = console_task(&store, console_id)?;
    let mut runs = store.runs(&task.id)?;
    if runs.len() > MAX_THREAD {
        runs.drain(..runs.len() - MAX_THREAD);
    }
    let mut turns = Vec::with_capacity(runs.len());
    for run in &runs {
        let steps = store.steps(&run.id)?;
        turns.push(turn_view(run, steps.iter().find(|s| s.kind == STEP_KIND)));
    }
    let mut view = task_view(&store, &task)?;
    if let Some(map) = view.as_object_mut() {
        map.remove("runs");
    }
    view["turns"] = json!(turns);
    Ok(view)
}

fn task_view(store: &TaskStore, task: &Task) -> Result<Value> {
    let mut runs = store.runs(&task.id)?;
    runs.reverse();
    runs.truncate(20);
    let mut views = Vec::with_capacity(runs.len());
    for run in &runs {
        let steps = store.steps(&run.id)?;
        views.push(run_view(run, steps.iter().find(|s| s.kind == STEP_KIND)));
    }
    let detail = &task.detail;
    Ok(json!({
        "id": console_id_of(task),
        "title": task.title,
        "created_at": task.created_at,
        "updated_at": task.updated_at,
        "prompt": detail.get("prompt").cloned().unwrap_or(Value::Null),
        "schedule": detail.get("schedule").cloned().unwrap_or(Value::Null),
        "paused": detail.get("paused").and_then(Value::as_bool).unwrap_or(false),
        "console": detail.get("console").cloned().unwrap_or(Value::Null),
        "workdir": detail.get("workdir").cloned().unwrap_or(Value::Null),
        "watch": detail.get("watch").cloned().unwrap_or(Value::Null),
        "watch_status": watch_of(task).map(|watch| {
            let id = console_id_of(task);
            let mut status = super::console_watch::status(&id, &watch);
            if watch.needs_secret() {
                status["has_secret"] = json!(super::console_watch::has_secret(&id));
            }
            status
        }),
        "agent": detail.get("agent").cloned().unwrap_or(Value::Null),
        "keep_conversation": keeps_conversation(task),
        "runs": views,
    }))
}

/// What agents are waiting on a person to do by hand -- sign in, pass a
/// check -- across every conversation that is still running, newest last.
/// The phone lists these with the questions.
pub fn waiting_handovers() -> Result<Vec<Value>> {
    let store = store()?;
    let mut out = Vec::new();
    for task in store.tasks()?.into_iter().filter(|t| t.kind == KIND) {
        for run in store.runs(&task.id)?.iter().filter(|r| r.state == State::Running) {
            let steps = store.steps(&run.id)?;
            let Some(step) = steps.iter().find(|s| s.kind == STEP_KIND) else {
                continue;
            };
            let Some(handovers) = step.detail.get("handovers").and_then(Value::as_array) else {
                continue;
            };
            for handover in handovers.iter().filter(|h| h["state"] == "waiting") {
                out.push(json!({
                    "task_id": console_id_of(&task),
                    "title": task.title,
                    "session_id": step.detail.get("session_id").cloned().unwrap_or(Value::Null),
                    "id": handover["id"],
                    "what": handover["what"],
                    "tab_id": handover["tab_id"],
                    "at": handover["at"],
                }));
            }
        }
    }
    Ok(out)
}

pub fn list() -> Result<Vec<Value>> {
    let store = store()?;
    let mut tasks: Vec<Task> = store.tasks()?.into_iter().filter(|t| t.kind == KIND).collect();
    tasks.reverse();
    tasks.iter().map(|task| task_view(&store, task)).collect()
}

// ---- writes --------------------------------------------------------------

/// What the console sends for one task. `console` is its own view of the
/// task, kept and handed back untouched.
#[derive(Debug, Default, Deserialize)]
pub struct Put {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub schedule: Option<Schedule>,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub console: Value,
    /// Run when this changes, besides (or instead of) a schedule.
    #[serde(default)]
    pub watch: Option<Watch>,
    /// This task's own agent; the run settings' when absent.
    #[serde(default)]
    pub agent: Option<String>,
    /// Scheduled and watched runs continue the task's conversation.
    #[serde(default)]
    pub keep_conversation: bool,
    /// The watch's secret (a mail password, a calendar address), kept in
    /// the system's credential store, never in the task. Absent: keep the
    /// one already there.
    #[serde(default)]
    pub watch_secret: Option<String>,
}

pub fn put(put: Put) -> Result<Value> {
    let title = put.title.trim();
    if title.is_empty() {
        bail!("a task needs a title");
    }
    if let Some(schedule) = &put.schedule {
        schedule.check()?;
    }
    if let Some(watch) = &put.watch {
        watch.check()?;
        let given = put.watch_secret.as_deref().map(str::trim).filter(|s| !s.is_empty());
        if watch.needs_secret() {
            match given {
                Some(secret) => keep_watch_secret(&put.id, Some(watch), secret)?,
                None if !super::console_watch::has_secret(&put.id) => {
                    bail!("this watch needs its password or address")
                }
                None => {}
            }
        }
    }
    let agent = put.agent.as_deref().map(str::trim).filter(|a| !a.is_empty()).map(str::to_string);
    if let Some(agent) = &agent {
        if !super::agent_run::supports_hosted(agent) {
            bail!("{agent} cannot be run hosted, so its tool calls could not reach the inbox");
        }
    }
    let key = task_key(&put.id)?;
    let mut detail = json!({
        "console_id": put.id,
        "prompt": put.prompt.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()),
        "schedule": put.schedule,
        "paused": put.paused,
        "console": put.console,
        "watch": put.watch,
        "agent": agent,
        "keep_conversation": put.keep_conversation,
    });
    let store = store()?;
    match store.task(&key)? {
        Some(existing) if existing.kind != KIND => bail!("{} is not a console task", put.id),
        Some(existing) => {
            // The runtime's own: where the task works stays where it was.
            if let Some(workdir) = existing.detail.get("workdir") {
                detail["workdir"] = workdir.clone();
            }
            store.set_task_title(&key, title)?;
            store.set_task_detail(&key, detail)?;
        }
        None => {
            store.create_task_as(key.clone(), KIND, title, detail)?;
        }
    }
    let task = store.task(&key)?.ok_or_else(|| anyhow!("the task vanished as it was written"))?;
    task_view(&store, &task)
}

pub fn delete(console_id: &str) -> Result<()> {
    let store = store()?;
    let task = console_task(&store, console_id)?;
    // A task going away takes its agent with it.
    let _ = stop(console_id);
    super::console_watch::forget(console_id);
    store.delete_task(&task.id)
}

// ---- runs ----------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct RunRequest {
    pub task_id: String,
    /// The console's id for the run, when it already shows one.
    #[serde(default)]
    pub run_id: Option<String>,
    /// What to say this time; the task's prompt when absent.
    #[serde(default)]
    pub prompt: Option<String>,
    /// A session whose conversation this run continues.
    #[serde(default)]
    pub continue_from: Option<String>,
    /// Continue the task's own conversation: the latest run's session, found
    /// here so the caller need not know it. A new conversation when there is
    /// none to continue.
    #[serde(default)]
    pub follow_up: bool,
    /// Ask the agent to write what this conversation did as a skill, and take
    /// it in when the turn ends. Implies `follow_up`.
    #[serde(default)]
    pub save_skill: bool,
    /// Never more than this, whatever the run settings say: work handed over
    /// by another agent runs under that agent's terms at most.
    #[serde(skip)]
    pub autonomy_cap: Option<Autonomy>,
}

fn autonomy_rank(level: Autonomy) -> u8 {
    match level {
        Autonomy::Cautious => 0,
        Autonomy::Balanced => 1,
        Autonomy::Autonomous => 2,
    }
}

/// A standing helper, as the hub is told about it.
#[derive(Clone, Debug, PartialEq)]
pub struct StandingHelper {
    pub id: String,
    pub title: String,
    pub agent: String,
    pub busy: bool,
}

/// Tasks that keep their conversation: helpers that remember their work,
/// which a hub may hand more of it to. Paused ones are not.
pub fn standing_helpers() -> Result<Vec<StandingHelper>> {
    let store = store()?;
    let default_agent = run_settings().agent;
    let mut found = Vec::new();
    for task in store.tasks()?.into_iter().filter(|t| t.kind == KIND) {
        if !keeps_conversation(&task) || task.detail.get("paused").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let busy = store.runs(&task.id)?.iter().any(|r| r.state == State::Running);
        found.push(StandingHelper {
            id: console_id_of(&task),
            title: task.title.clone(),
            agent: task.detail.get("agent").and_then(Value::as_str).filter(|a| !a.is_empty()).unwrap_or(&default_agent).to_string(),
            busy,
        });
    }
    Ok(found)
}

/// The standing helper a hub names: its id, or its title (exactly, or the
/// one title that contains it).
fn find_helper(name: &str) -> Result<StandingHelper> {
    let helpers = standing_helpers()?;
    let wanted = name.trim().to_lowercase();
    let listed = || helpers.iter().map(|h| format!("{:?} ({})", h.title, h.id)).collect::<Vec<_>>().join(", ");
    if let Some(found) = helpers.iter().find(|h| h.id.to_lowercase() == wanted || h.title.to_lowercase() == wanted) {
        return Ok(found.clone());
    }
    let partial: Vec<&StandingHelper> = helpers.iter().filter(|h| h.title.to_lowercase().contains(&wanted)).collect();
    match partial.as_slice() {
        [one] => Ok((*one).clone()),
        [] if helpers.is_empty() => bail!("there are no standing helpers; hand the work to an agent instead"),
        [] => bail!("no standing helper {name:?}; there are: {}", listed()),
        _ => bail!("{name:?} could be more than one standing helper: {}", listed()),
    }
}

/// Hand a piece of work to a standing helper: a run of its task that
/// continues its conversation, under `cap` at most. Its questions go to its
/// own conversation and the inbox. The session it runs in, and how far its
/// events had come before (a continued session keeps its earlier ones).
pub fn hand_to_helper(handler: &McpHandler, name: &str, prompt: &str, cap: Autonomy) -> Result<(String, String, usize)> {
    let helper = find_helper(name)?;
    if helper.busy {
        bail!("standing helper {:?} is busy with its own work; try again later", helper.title);
    }
    let store = store()?;
    let task = console_task(&store, &helper.id)?;
    let earlier = latest_conversation(&store, &task, &helper.agent)?;
    let before = earlier
        .as_deref()
        .and_then(|session| agent_session::events(session, usize::MAX).ok())
        .map_or(0, |(_, count)| count);
    let view = run(
        handler,
        RunRequest {
            task_id: helper.id.clone(),
            prompt: Some(prompt.to_string()),
            follow_up: true,
            autonomy_cap: Some(cap),
            ..RunRequest::default()
        },
        "delegate",
    )?;
    if view["state"] == "failed" {
        bail!("standing helper {:?} did not start: {}", helper.title, view["error"].as_str().unwrap_or("unknown error"));
    }
    let session = view["session_id"]
        .as_str()
        .ok_or_else(|| anyhow!("standing helper {:?} started without a session", helper.title))?
        .to_string();
    let cursor = if earlier.as_deref() == Some(session.as_str()) { before } else { 0 };
    Ok((session, helper.agent, cursor))
}

/// The session of the task's latest run that can be continued: its agent
/// named a conversation (it has a resume record).
/// Only a conversation with the agent the task has now: one handed to
/// another agent since starts afresh with it.
fn latest_conversation(store: &TaskStore, task: &Task, agent: &str) -> Result<Option<String>> {
    for run in store.runs(&task.id)?.into_iter().rev() {
        let Some(step) = store.steps(&run.id)?.into_iter().find(|s| s.kind == STEP_KIND) else { continue };
        let Some(session) = step.detail.get("session_id").and_then(Value::as_str) else { continue };
        if let Ok(Some(record)) = agent_session::resume_record(session) {
            if super::agent_run::agent_of(&record.adapter) != Some(agent) {
                return Ok(None);
            }
            return Ok(Some(session.to_string()));
        }
    }
    Ok(None)
}

/// What the task last told the person on its own, if its latest run was
/// one that started on its own and said something.
fn told_on_its_own(store: &TaskStore, task: &Task) -> Option<String> {
    let run = store.runs(&task.id).ok()?.into_iter().last()?;
    let step = store.steps(&run.id).ok()?.into_iter().find(|s| s.kind == STEP_KIND)?;
    if !matches!(step.detail.get("trigger").and_then(Value::as_str), Some("schedule" | "watch")) {
        return None;
    }
    step.detail.get("reply").and_then(Value::as_str).map(str::trim).filter(|r| !r.is_empty()).map(str::to_string)
}

fn watch_of(task: &Task) -> Option<Watch> {
    task.detail.get("watch").cloned().and_then(|w| serde_json::from_value(w).ok())
}

fn keeps_conversation(task: &Task) -> bool {
    task.detail.get("keep_conversation").and_then(Value::as_bool).unwrap_or(false)
}

/// What a task asks its agent: its prompt, else its title.
fn task_prompt(task: &Task) -> String {
    task.detail
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| task.title.clone())
}

fn running_run(store: &TaskStore, task: &Task) -> Result<Option<(Run, Option<Step>)>> {
    for run in store.runs(&task.id)? {
        if run.state == State::Running {
            let step = store.steps(&run.id)?.into_iter().find(|s| s.kind == STEP_KIND);
            return Ok(Some((run, step)));
        }
    }
    Ok(None)
}

/// Close a run with a verdict: the step's detail and state, then the run.
fn finish(store: &TaskStore, task: &TaskId, run: &RunId, step: &StepId, detail: Value, state: State) {
    if let Err(error) = store.set_step_detail(step, detail, state) {
        log::warn!("console run {run}: could not write the step: {error:#}");
    }
    let current = store.runs(task).ok().and_then(|runs| runs.into_iter().find(|r| r.id == *run));
    if let Some(current) = current {
        if current.state == State::Running {
            if let Err(error) = store.finish_run(run, state, current.version) {
                log::warn!("console run {run}: could not close it: {error:#}");
            }
        }
    }
    following().lock().unwrap().remove(run.as_str());
}

/// Start a run of a task: the agent in the run settings, hosted, followed by
/// a thread of the runtime's own until its turn ends.
pub fn run(handler: &McpHandler, request: RunRequest, trigger: &str) -> Result<Value> {
    let store = store()?;
    let task = console_task(&store, &request.task_id)?;
    if running_run(&store, &task)?.is_some() {
        bail!("{} is already running", request.task_id);
    }
    let mut request = request;
    if request.save_skill {
        request.prompt = Some(super::skills::save_prompt());
        request.follow_up = true;
    }
    let prompt = request
        .prompt
        .clone()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| task_prompt(&task));
    // What the person said, for the reviewer of a sending step: the message
    // they typed, or the task they saved when it runs on its own. Work
    // another agent handed over, and the app's own instructions, are not.
    let person_words: Value = match trigger {
        "delegate" => Value::Null,
        _ if request.save_skill => Value::Null,
        "schedule" | "watch" => json!(task_prompt(&task)),
        _ => json!(prompt),
    };
    // A run that starts on its own decides whether it should interrupt the
    // person, by the rules they taught it and the hour.
    let agent_prompt = if matches!(trigger, "schedule" | "watch") {
        format!("{prompt}\n\n{}", super::tell_rules::run_note(&super::tell_rules::load(), chrono::Local::now()))
    } else {
        prompt.clone()
    };
    let mut settings = run_settings();
    if let Some(cap) = request.autonomy_cap {
        if autonomy_rank(cap) < autonomy_rank(settings.autonomy) {
            settings.autonomy = cap;
        }
    }
    // A standing helper's own agent.
    if let Some(agent) = task.detail.get("agent").and_then(Value::as_str).filter(|a| !a.is_empty()) {
        settings.agent = agent.to_string();
    }
    let workdir = workdir_for(&store, &task, &settings)?;
    // The person answering something a task told them on its own: read the
    // answer for how they want to be told, in the background.
    if matches!(trigger, "manual" | "continue") && !request.save_skill {
        if let Some(reply) = request.prompt.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            if let Some(told) = told_on_its_own(&store, &task) {
                if let Ok(argv) = super::agent_run::session_command(&settings.agent, &json!({"hosted": false})) {
                    let argv: Vec<String> = argv.iter().filter_map(|a| a.as_str().map(str::to_string)).collect();
                    let adapter = super::agent_run::adapter_name(&settings.agent).to_string();
                    super::tell_rules::learn_in_background(argv, adapter, told, reply.to_string());
                }
            }
        }
    }
    let continue_from = match &request.continue_from {
        Some(session) => Some(session.clone()),
        None if request.follow_up => latest_conversation(&store, &task, &settings.agent)?,
        None => None,
    };
    let console_run_id = request
        .run_id
        .clone()
        .unwrap_or_else(|| format!("run-{}-{}", trigger, uuid::Uuid::new_v4().simple()));
    let run_id = run_key(&console_run_id)?;
    let step_id = format!("{console_run_id}-step-1");
    let console_task_id = console_id_of(&task);

    following().lock().unwrap().insert(run_id.as_str().to_string());
    let run = match store.start_run_as(&task.id, run_id.clone()) {
        Ok(run) => run,
        Err(error) => {
            following().lock().unwrap().remove(run_id.as_str());
            return Err(error);
        }
    };
    let mut detail = json!({
        "console_run_id": console_run_id,
        "trigger": trigger,
        "agent": settings.agent,
        "prompt": prompt,
        "cwd": workdir,
    });
    if request.follow_up || request.continue_from.is_some() {
        detail["continued"] = json!(continue_from.is_some());
    }
    if request.save_skill {
        detail["purpose"] = json!("save_skill");
    }
    let step = store.request_step_with_detail(&run.id, STEP_KIND, None, detail.clone())?.step().clone();
    store.set_step_detail(&step.id, detail.clone(), State::Running)?;

    let resumed = match &continue_from {
        Some(session) => {
            let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
            // The environment is not kept from the earlier session; the
            // skills it was handed are handed again.
            let record = agent_session::resume_record(session).ok().flatten();
            let skills = record.as_ref().and_then(|record| {
                super::agent_run::session_env(&record.adapter, record.governed, Some(settings.browser_profile.as_str()).filter(|p| !p.is_empty()), false)
            });
            // Today's command line for the same agent: the conversation is
            // continued, the instructions (its own browser profile, handing
            // over, what is asked) are the ones a new conversation gets.
            let command = record.as_ref().and_then(|record| {
                let agent = super::agent_run::agent_of(&record.adapter)?;
                super::agent_run::session_command(
                    agent,
                    &json!({
                        "governed": record.governed,
                        "hosted": true,
                        "browser_profile": settings.browser_profile,
                    }),
                )
                .map_err(|(_, _, why)| log::warn!("continued on the earlier command line: {why}"))
                .ok()
            });
            handler
                .handle(
                    &ctx,
                    "agent_session.resume",
                    &json!({
                        "env": skills,
                        "command": command,
                        "session_id": session,
                        "prompt": if record.as_ref().is_some_and(|r| r.governed) {
                            agent_prompt.clone()
                        } else {
                            super::agent_run::with_deliverables_note(&agent_prompt)
                        },
                        // Today's level, not the one the conversation began under.
                        "autonomy": settings.autonomy.as_str(),
                        "person_words": person_words,
                        "task_id": console_task_id,
                        "run_id": console_run_id,
                        "step_id": step_id,
                    }),
                )
                .map(Some)
                .map_err(|e| e.to_string())
        }
        None => Ok(None),
    };
    // A conversation that cannot be picked up again (its agent's own record
    // of it is gone) is started afresh rather than failing the message.
    let resumed = match resumed {
        Err(message) if request.follow_up && request.continue_from.is_none() => {
            log::warn!("console run {console_run_id}: could not continue, starting afresh: {message}");
            detail["continued"] = json!(false);
            Ok(None)
        }
        other => other,
    };
    let started = match resumed {
        Ok(Some(value)) => Ok(value),
        Err(message) => Err(message),
        Ok(None) => {
            let governed = settings.governed && super::agent_run::supports_governed(&settings.agent);
            let mut body = json!({
                "agent": settings.agent,
                "prompt": if governed { agent_prompt.clone() } else { super::agent_run::with_deliverables_note(&agent_prompt) },
                "hosted": true,
                "governed": governed,
                "autonomy": settings.autonomy.as_str(),
                "browser_profile": settings.browser_profile,
                "person_words": person_words,
                "task_id": console_task_id,
                "run_id": console_run_id,
                "step_id": step_id,
            });
            body["cwd"] = json!(workdir);
            super::agent_run::start_session(handler, &body).map_err(|(_, _, message)| message)
        }
    };
    let started = match started {
        Ok(value) => value,
        Err(message) => {
            detail["error"] = json!(message);
            detail["error_code"] = json!("start_failed");
            finish(&store, &task.id, &run.id, &step.id, detail, State::Failed);
            let run = store.runs(&task.id)?.into_iter().find(|r| r.id == run.id).unwrap_or(run);
            return Ok(run_view(&run, store.step(&step.id)?.as_ref()));
        }
    };
    let session_id = started
        .pointer("/session/session_id")
        .or_else(|| started.get("session_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the agent started but no session id came back"))?
        .to_string();
    detail["session_id"] = json!(session_id);
    if let Some(adapter) = started.get("adapter").and_then(Value::as_str) {
        detail["adapter"] = json!(adapter);
    }
    store.set_step_detail(&step.id, detail.clone(), State::Running)?;

    let (follow_task, follow_run, follow_step) = (task.id.clone(), run.id.clone(), step.id.clone());
    std::thread::Builder::new()
        .name("console-run".into())
        .spawn(move || follow(follow_task, follow_run, follow_step, session_id, detail))
        .context("could not start following the run")?;

    let run = store.runs(&task.id)?.into_iter().find(|r| r.id == run.id).unwrap_or(run);
    Ok(run_view(&run, store.step(&step.id)?.as_ref()))
}

/// What one pass over a session's new events learned.
#[derive(Default)]
struct Turn {
    text: String,
    error: Option<String>,
    /// Why the runtime itself ended the turn, for the console to say in the
    /// reader's language; `None` when the agent reported its own error.
    error_code: Option<&'static str>,
    waiting: bool,
    ended: Option<String>,
    /// Something other than the answer came since its last words -- a tool
    /// call, a question -- so the next words are a new paragraph.
    paragraph: bool,
    /// What the agent did, one entry per tool call, in order.
    activity: Vec<Value>,
    /// Where it works, for judging its edits.
    cwd: Option<String>,
    /// Terminal sessions it used (Unterm's ids), in the order first seen.
    terminals: Vec<String>,
    /// Unzoo Browser tabs it used.
    tabs: Vec<String>,
    /// The terminals and tabs it opened itself: closed when the run ends.
    opened_terminals: Vec<String>,
    opened_tabs: Vec<String>,
    /// What it asked the person to do themselves (sign in, solve a
    /// captcha), and whether they did.
    handovers: Vec<Value>,
}

impl Turn {
    fn read(&mut self, event: &Value) {
        match event.get("event").and_then(Value::as_str).unwrap_or_default() {
            "output.delta" if event.get("stream").and_then(Value::as_str) != Some("reasoning") => {
                if let Some(text) = event.get("text").and_then(Value::as_str) {
                    if self.paragraph && !self.text.is_empty() && !text.trim().is_empty() {
                        self.text.truncate(self.text.trim_end().len());
                        self.text.push_str("\n\n");
                    }
                    if !text.trim().is_empty() {
                        self.paragraph = false;
                    }
                    self.text.push_str(text);
                }
                return;
            }
            "error" => {
                self.error = event
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or(self.error.take());
            }
            "tool_call.requested" => self.tool_requested(event),
            "tool_call.result" => {
                let id = event.get("external_call_id").and_then(Value::as_str);
                let mut tool = None;
                if let Some(entry) =
                    self.activity.iter_mut().rev().find(|a| id.is_some() && a["call_id"].as_str() == id)
                {
                    entry["ok"] = event.get("ok").cloned().unwrap_or(Value::Null);
                    tool = entry["tool"].as_str().map(str::to_string);
                }
                // What a terminal or a tab was opened as is only in the
                // answer: `session.create` and `tab_create` name it there.
                if let (Some(tool), Some(output)) = (tool, event.get("output").and_then(Value::as_str)) {
                    let ok = event.get("ok").and_then(Value::as_bool) != Some(false);
                    if from_server(&tool, "unterm") {
                        if let Some(session) = id_after(output, "session_id") {
                            self.note_terminal(&session);
                            if ok && tool.replace('.', "_").ends_with("session_create") && !self.opened_terminals.contains(&session) {
                                self.opened_terminals.push(session);
                            }
                        }
                    } else if from_server(&tool, "unzoo") {
                        if let Some(tab) = id_after(output, "tab_id") {
                            self.note_tab(&tab);
                            if ok && tool.replace('.', "_").ends_with("tab_create") && !self.opened_tabs.contains(&tab) {
                                self.opened_tabs.push(tab);
                            }
                        }
                    }
                }
            }
            "handover.requested" => {
                let handover = &event["handover"];
                if let Some(tab) = scalar_id(&handover["tab_id"]) {
                    self.note_tab(&tab);
                }
                if self.handovers.len() < 32 {
                    self.handovers.push(json!({
                        "id": handover["id"],
                        "what": handover["what"],
                        "tab_id": scalar_id(&handover["tab_id"]),
                        "state": "waiting",
                        "at": event.get("at").cloned().unwrap_or(Value::Null),
                    }));
                }
            }
            "handover.resolved" => {
                let handover = &event["handover"];
                if let Some(entry) = self.handovers.iter_mut().find(|h| h["id"] == handover["id"]) {
                    entry["state"] = handover["state"].clone();
                    entry["note"] = handover["note"].clone();
                }
            }
            "approval.requested" => {
                self.waiting = true;
                self.approval_requested(event);
            }
            "approval.resolved" => {
                self.waiting = false;
                self.approval_resolved(event);
            }
            "approval.reviewed" => {
                let approval = &event["approval"];
                let request = &approval["request_id"];
                if let Some(entry) = self.activity.iter_mut().rev().find(|a| a.get("request_id") == Some(request)) {
                    entry["review"] = approval["review"].clone();
                }
            }
            "turn.completed" | "session.closed" => {
                self.ended = Some(event.get("state").and_then(Value::as_str).unwrap_or("finished").to_string());
            }
            _ => {}
        }
        self.paragraph = true;
    }

    fn tool_requested(&mut self, event: &Value) {
        if self.activity.len() >= MAX_ACTIVITY {
            return;
        }
        let call = &event["tool_call"];
        let tool = call["tool"].as_str().unwrap_or("tool");
        // The CLI looking up its own tools is not something it did.
        if HOUSEKEEPING_TOOLS.contains(&tool) {
            return;
        }
        let arguments = &call["arguments"];
        if from_server(tool, "unterm") {
            for key in ["session_id", "pane_id", "id"] {
                if let Some(session) = scalar_id(&arguments[key]) {
                    self.note_terminal(&session);
                    break;
                }
            }
        } else if from_server(tool, "unzoo") {
            if let Some(tab) = scalar_id(&arguments["tab_id"]) {
                self.note_tab(&tab);
            }
        }
        let (verb, target) = tool_risk::describe_in(tool, arguments, self.cwd.as_deref());
        let risk = tool_risk::classify(tool, arguments, self.cwd.as_deref());
        self.activity.push(json!({
            "call_id": call["external_call_id"],
            "tool": tool,
            "verb": verb,
            "target": target,
            "risk": risk.map(|r| r.as_str()),
            "at": event.get("at").cloned().unwrap_or(Value::Null),
        }));
    }

    /// A question about a call: matched to the call it is about (the latest
    /// one of that tool not yet asked about), or recorded by itself when the
    /// CLI asks before it says it is calling.
    fn approval_requested(&mut self, event: &Value) {
        let approval = &event["approval"];
        let tool = approval["tool_call"]["tool"].as_str().unwrap_or("tool").to_string();
        let request = approval["request_id"].clone();
        let room = self.activity.len() < MAX_ACTIVITY;
        let entry = self
            .activity
            .iter_mut()
            .rev()
            .find(|a| a["tool"].as_str() == Some(tool.as_str()) && a.get("request_id").is_none());
        match entry {
            Some(entry) => {
                entry["request_id"] = request;
                if let Some(risk) = approval.get("risk") {
                    entry["risk"] = risk.clone();
                }
                for key in ["consequence", "commit", "repeat", "review"] {
                    if let Some(value) = approval.get(key) {
                        entry[key] = value.clone();
                    }
                }
            }
            None if room => {
                let arguments = &approval["tool_call"]["arguments"];
                let (verb, target) = tool_risk::describe_in(&tool, arguments, self.cwd.as_deref());
                let risk = approval.get("risk").cloned().unwrap_or_else(|| {
                    json!(tool_risk::classify(&tool, arguments, self.cwd.as_deref()).map(|r| r.as_str()))
                });
                let mut entry = json!({
                    "tool": tool,
                    "verb": verb,
                    "target": target,
                    "risk": risk,
                    "request_id": request,
                    "at": event.get("at").cloned().unwrap_or(Value::Null),
                });
                for key in ["consequence", "commit", "repeat", "review"] {
                    if let Some(value) = approval.get(key) {
                        entry[key] = value.clone();
                    }
                }
                self.activity.push(entry);
            }
            None => {}
        }
    }

    fn approval_resolved(&mut self, event: &Value) {
        let approval = &event["approval"];
        let request = &approval["request_id"];
        if let Some(entry) = self.activity.iter_mut().rev().find(|a| a.get("request_id") == Some(request)) {
            // `caller` is whoever answered through the API: a person, at the
            // console or in the inbox.
            let by = match approval["decided_by"].as_str() {
                Some("caller") | None => "person",
                Some(other) => other,
            };
            entry["decided_by"] = json!(by);
            entry["decision"] = json!(if approval["decision"] == "deny" { "deny" } else { "allow" });
            if let Some(reason) = approval.get("reason") {
                entry["review"] = json!({"state": "allow", "reason": reason});
            }
        }
    }

    fn note_terminal(&mut self, session: &str) {
        if !self.terminals.iter().any(|known| known == session) && self.terminals.len() < 32 {
            self.terminals.push(session.to_string());
        }
    }

    fn note_tab(&mut self, tab: &str) {
        if !self.tabs.iter().any(|known| known == tab) && self.tabs.len() < 32 {
            self.tabs.push(tab.to_string());
        }
    }

    /// Everything the agent said this turn, as its reply. A very long reply
    /// keeps its beginning and its end.
    fn reply(&self) -> String {
        let text = self.text.trim();
        if text.len() <= MAX_REPLY {
            return text.to_string();
        }
        let mut head = MAX_REPLY / 2;
        while !text.is_char_boundary(head) {
            head -= 1;
        }
        let mut tail = text.len() - MAX_REPLY / 2;
        while !text.is_char_boundary(tail) {
            tail += 1;
        }
        format!("{}\n\n…\n\n{}", &text[..head], &text[tail..])
    }

    /// The agent's last words, as the run's summary.
    fn summary(&self) -> String {
        let text = self.text.trim();
        let chars: Vec<char> = text.chars().collect();
        if chars.len() <= 600 {
            text.to_string()
        } else {
            format!("…{}", chars[chars.len() - 600..].iter().collect::<String>())
        }
    }
}

/// A helper the conversation's agent handed work to, as the console follows
/// it: what it did and said, read from its own events.
struct Followed {
    session: String,
    agent: String,
    prompt: String,
    cursor: usize,
    turn: Turn,
    waiting: bool,
    /// Something to write down since the step was last saved.
    changed: bool,
}

/// Read every helper's new events. True when any of them got on, or waits
/// for a person -- the conversation's agent is waiting on them, not stuck.
fn follow_helpers(hub: &str, followed: &mut Vec<Followed>, cwd: Option<&str>) -> bool {
    for helper in super::helpers::of(hub) {
        match followed.iter_mut().find(|f| f.session == helper.session_id) {
            Some(known) => {
                if known.prompt != helper.prompt {
                    // Continued with more work: a new turn of the same helper.
                    known.prompt = helper.prompt;
                    known.turn.ended = None;
                    known.turn.text.clear();
                    known.changed = true;
                }
            }
            None => followed.push(Followed {
                session: helper.session_id,
                agent: helper.agent,
                prompt: helper.prompt,
                cursor: 0,
                turn: Turn { cwd: cwd.map(str::to_string), ..Turn::default() },
                waiting: false,
                changed: true,
            }),
        }
    }
    let mut progressed = false;
    for helper in followed.iter_mut() {
        if let Ok((events, next)) = agent_session::events(&helper.session, helper.cursor) {
            if !events.is_empty() {
                progressed = true;
                helper.changed = true;
            }
            helper.cursor = next;
            for event in &events {
                helper.turn.read(event);
            }
            let pending = agent_session::pending_approvals(&helper.session).is_ok_and(|p| !p.is_empty());
            let waiting = pending || helper.turn.waiting;
            if waiting != helper.waiting {
                helper.waiting = waiting;
                helper.changed = true;
            }
            progressed |= waiting;
        }
    }
    progressed
}

fn helpers_view(followed: &mut [Followed]) -> Value {
    Value::Array(
        followed
            .iter_mut()
            .map(|helper| {
                helper.changed = false;
                let state = match helper.turn.ended.as_deref() {
                    None if helper.waiting => "waiting",
                    None => "working",
                    Some("finished" | "exited" | "succeeded") => "done",
                    Some("interrupted" | "cancelled") => "stopped",
                    Some(_) => "failed",
                };
                json!({
                    "session_id": helper.session,
                    "agent": helper.agent,
                    "prompt": helper.prompt,
                    "state": state,
                    "waiting": helper.waiting,
                    "reply": helper.turn.reply(),
                    "error": helper.turn.error,
                    "activity": helper.turn.activity,
                    "terminals": helper.turn.terminals,
                    "tabs": helper.turn.tabs,
                })
            })
            .collect(),
    )
}

/// How a session's ending reads as a run's verdict.
fn verdict(ended: &str, stopped_for_idling: bool) -> State {
    match ended {
        _ if stopped_for_idling => State::Failed,
        "finished" | "exited" | "succeeded" => State::Succeeded,
        "interrupted" | "cancelled" => State::Cancelled,
        _ => State::Failed,
    }
}

fn follow(task: TaskId, run: RunId, step: StepId, session: String, mut detail: Value) {
    let started = Instant::now();
    // A file written in the second the run started still counts; clocks and
    // file systems round.
    let started_at = std::time::SystemTime::now() - Duration::from_secs(2);
    let mut last_progress = Instant::now();
    let mut cursor = 0usize;
    let mut turn = Turn { cwd: detail.get("cwd").and_then(Value::as_str).map(str::to_string), ..Turn::default() };
    let mut shown_waiting = false;
    let mut shown_len = 0usize;
    let mut shown_activity = 0usize;
    let mut idled: Option<Instant> = None;
    let mut helpers: Vec<Followed> = Vec::new();
    let mut shown_handovers = json!([]);

    loop {
        std::thread::sleep(POLL);
        if follow_helpers(&session, &mut helpers, turn.cwd.as_deref()) {
            last_progress = Instant::now();
        }
        match agent_session::events(&session, cursor) {
            Ok((events, next)) => {
                if !events.is_empty() {
                    last_progress = Instant::now();
                }
                cursor = next;
                for event in &events {
                    turn.read(event);
                }
            }
            Err(_) => {
                // Gone from memory: closed elsewhere, or never live. Its
                // record still says how it ended.
                let state = agent_session::status(&session)
                    .ok()
                    .and_then(|v| v.get("state").and_then(Value::as_str).map(str::to_string));
                turn.ended = Some(match state.as_deref() {
                    Some("running") | None => {
                        turn.error.get_or_insert_with(|| "the agent session disappeared".into());
                        turn.error_code.get_or_insert("session_lost");
                        "failed".into()
                    }
                    Some(state) => state.to_string(),
                });
            }
        }
        if turn.ended.is_some() {
            break;
        }

        let status = agent_session::status(&session).ok();
        let pending = status
            .as_ref()
            .and_then(|s| s.get("pending_approvals"))
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty());
        let handing_over = agent_session::has_open_handover(&session);
        if pending || handing_over {
            // Waiting for a person is not being stuck.
            last_progress = Instant::now();
        }
        let waiting = pending || handing_over || turn.waiting || helpers.iter().any(|h| h.waiting);
        let text_len = turn.text.len();
        let activity_len = turn.activity.len();
        let helpers_changed = helpers.iter().any(|h| h.changed);
        let handovers_seen = json!(turn.handovers);
        let handovers_changed = handovers_seen != shown_handovers;
        if waiting != shown_waiting
            || text_len != shown_len
            || activity_len != shown_activity
            || helpers_changed
            || handovers_changed
        {
            shown_handovers = handovers_seen;
            shown_waiting = waiting;
            shown_len = text_len;
            shown_activity = activity_len;
            detail["waiting"] = json!(waiting);
            detail["summary"] = json!(turn.summary());
            detail["reply"] = json!(turn.reply());
            detail["activity"] = json!(turn.activity);
            detail["terminals"] = json!(turn.terminals);
            detail["tabs"] = json!(turn.tabs);
            // Kept as it goes, so a run cut off by a restart can still have
            // the tabs it opened closed (see [`sweep_orphans`]).
            detail["opened_tabs"] = json!(turn.opened_tabs);
            if !turn.handovers.is_empty() {
                detail["handovers"] = json!(turn.handovers);
            }
            if !helpers.is_empty() {
                detail["helpers"] = helpers_view(&mut helpers);
            }
            if let Ok(store) = store() {
                let _ = store.set_step_detail(&step, detail.clone(), State::Running);
            }
        }

        match idled {
            None if last_progress.elapsed() > IDLE_LIMIT => {
                idled = Some(Instant::now());
                turn.error = Some(format!("no progress for {} minutes; stopped", IDLE_LIMIT.as_secs() / 60));
                turn.error_code = Some("idle_timeout");
                let _ = agent_session::interrupt(&session, 2_000);
            }
            Some(at) if at.elapsed() > Duration::from_secs(30) => {
                turn.ended = Some("failed".into());
                break;
            }
            _ => {}
        }
    }

    let usage = agent_session::status(&session).ok().and_then(|s| s.get("usage").cloned());
    // Its helpers' last words, then they stop with it: nobody is left to
    // read what they would say next.
    follow_helpers(&session, &mut helpers, turn.cwd.as_deref());
    super::helpers::release(&session);
    for helper in helpers.iter_mut().filter(|h| h.turn.ended.is_none()) {
        helper.turn.ended = Some("interrupted".into());
        helper.waiting = false;
    }
    // A hosted agent outlives its turn, waiting for the next message. This
    // run is over; the conversation is kept by the CLI and continued, when
    // asked, in a new session.
    let _ = agent_session::close(&session);
    // What it opened goes with it, so terminals and tabs do not pile up run
    // after run. What they showed last is kept for the console first.
    let mut turns: Vec<&Turn> = vec![&turn];
    turns.extend(helpers.iter().map(|h| &h.turn));
    tidy_up(&turns);

    let ended = turn.ended.clone().unwrap_or_else(|| "failed".into());
    let asked_to_stop = stopping().lock().unwrap().remove(run.as_str());
    let state = if asked_to_stop && idled.is_none() {
        State::Cancelled
    } else {
        verdict(&ended, idled.is_some())
    };
    detail["waiting"] = json!(false);
    detail["summary"] = json!(turn.summary());
    detail["reply"] = json!(turn.reply());
    if matches!(detail.get("trigger").and_then(Value::as_str), Some("schedule" | "watch")) {
        if let Some((fold, reply)) = super::tell_rules::fold(&turn.reply()) {
            detail["fold"] = json!(fold);
            detail["reply"] = json!(reply);
        }
    }
    detail["activity"] = json!(turn.activity);
    detail["terminals"] = json!(turn.terminals);
    detail["tabs"] = json!(turn.tabs);
    if !helpers.is_empty() {
        detail["helpers"] = helpers_view(&mut helpers);
    }
    detail["ended"] = json!(ended);
    detail["duration_seconds"] = json!(started.elapsed().as_secs());
    if let Some(usage) = usage.filter(|u| !u.is_null()) {
        detail["usage"] = usage;
    }
    if let Some(dir) = detail.get("cwd").and_then(Value::as_str).map(std::path::PathBuf::from) {
        // Asked to save a skill: take in what it wrote before the folder is
        // scanned, so the skill's files are not listed as deliverables.
        if detail.get("purpose").and_then(Value::as_str) == Some("save_skill") && state == State::Succeeded {
            match super::skills::import_saved(&dir) {
                Ok(names) => detail["saved_skills"] = json!(names),
                Err(error) => detail["skill_error"] = json!(format!("{error:#}")),
            }
        }
        detail["outputs"] = json!(named_in(outputs_since(&dir, started_at), &turn.text));
    }
    if state == State::Failed {
        if let Some(error) = &turn.error {
            detail["error"] = json!(error);
            if let Some(code) = turn.error_code {
                detail["error_code"] = json!(code);
            }
        }
    }
    match store() {
        Ok(store) => finish(&store, &task, &run, &step, detail, state),
        Err(error) => log::warn!("console run {run}: no store to write the verdict to: {error:#}"),
    }
}

/// Stop the task's running run, if it has one: its agent is interrupted and
/// the run ends as cancelled when the turn does.
pub fn stop(console_id: &str) -> Result<bool> {
    let store = store()?;
    let task = console_task(&store, console_id)?;
    let Some((run, step)) = running_run(&store, &task)? else { return Ok(false) };
    let session = step
        .as_ref()
        .and_then(|s| s.detail.get("session_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the run has not started its agent yet"))?;
    stopping().lock().unwrap().insert(run.id.as_str().to_string());
    agent_session::interrupt(session, 2_000)?;
    Ok(true)
}

/// Close every console run the engine thinks is running that nobody here is
/// following: its agent died with an earlier Unterm.
pub fn sweep_orphans() -> Result<usize> {
    let store = store()?;
    let followed = following().lock().unwrap().clone();
    let mut closed = 0;
    let mut left_open = Turn::default();
    for task in store.tasks()?.into_iter().filter(|t| t.kind == KIND) {
        for run in store.runs(&task.id)? {
            if run.state != State::Running || followed.contains(run.id.as_str()) {
                continue;
            }
            let step = store.steps(&run.id)?.into_iter().find(|s| s.kind == STEP_KIND);
            if let Some(step) = step {
                let ids = |key: &str| -> Vec<String> {
                    step.detail
                        .get(key)
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                        .unwrap_or_default()
                };
                // Its terminals died with the Core that held them (and their
                // ids may be some other terminal's now); its tabs did not.
                left_open.tabs.extend(ids("tabs"));
                left_open.opened_tabs.extend(ids("opened_tabs"));
                let mut detail = step.detail.clone();
                detail["waiting"] = json!(false);
                detail["error"] = json!("the background service stopped before this run finished");
                detail["error_code"] = json!("service_restarted");
                let _ = store.set_step_detail(&step.id, detail, State::Interrupted);
            }
            store.finish_run(&run.id, State::Interrupted, run.version)?;
            closed += 1;
        }
    }
    if !left_open.tabs.is_empty() || !left_open.opened_tabs.is_empty() {
        tidy_up(&[&left_open]);
    }
    Ok(closed)
}

/// A run in progress, or a schedule that is not paused: something that needs
/// the runtime to be there with no window open.
pub fn has_work() -> Result<bool> {
    let store = store()?;
    for task in store.tasks()?.into_iter().filter(|t| t.kind == KIND) {
        let scheduled = (task.detail.get("schedule").is_some_and(|s| !s.is_null()) || watch_of(&task).is_some())
            && !task.detail.get("paused").and_then(Value::as_bool).unwrap_or(false);
        if scheduled || store.runs(&task.id)?.iter().any(|r| r.state == State::Running) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// One pass of the scheduler: close orphans, then run what is due.
pub fn tick(handler: &McpHandler) -> Result<Vec<String>> {
    sweep_orphans()?;
    let store = store()?;
    let now = chrono::Local::now();
    let profile = run_settings().browser_profile;
    let mut started = Vec::new();
    for task in store.tasks()?.into_iter().filter(|t| t.kind == KIND) {
        if task.detail.get("paused").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let schedule = task
            .detail
            .get("schedule")
            .cloned()
            .and_then(|s| serde_json::from_value::<Schedule>(s).ok());
        let watch = watch_of(&task);
        if schedule.is_none() && watch.is_none() {
            continue;
        }
        let runs = store.runs(&task.id)?;
        if runs.iter().any(|r| r.state == State::Running) {
            continue;
        }
        let id = console_id_of(&task);
        let last = runs.iter().map(|r| r.started_at.as_str()).max();
        let (trigger, prompt) = if schedule.as_ref().is_some_and(|s| is_due(s, &now, &task.created_at, last)) {
            ("schedule", None)
        } else if let Some(watch) = &watch {
            let ended = runs
                .iter()
                .filter_map(|r| r.ended_at.as_deref())
                .filter_map(|at| DateTime::parse_from_rfc3339(at).ok())
                .map(|at| at.with_timezone(&chrono::Utc))
                .max();
            match super::console_watch::changed(&id, watch, &profile, ended, now.with_timezone(&chrono::Utc)) {
                Some(what) => ("watch", Some(format!("{}\n\n{what}", task_prompt(&task)))),
                None => continue,
            }
        } else {
            continue;
        };
        let request = RunRequest { task_id: id.clone(), prompt, follow_up: keeps_conversation(&task), ..RunRequest::default() };
        match run(handler, request, trigger) {
            Ok(_) => started.push(id),
            Err(error) => log::warn!("{trigger} run of {id} did not start: {error:#}"),
        }
    }
    Ok(started)
}

/// Start the scheduler. Once per process.
pub fn start_scheduler(handler: Arc<McpHandler>) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new().name("console-scheduler".into()).spawn(move || loop {
        if let Err(error) = tick(&handler) {
            log::warn!("console scheduler: {error:#}");
        }
        std::thread::sleep(Duration::from_secs(30));
    });
    if let Err(error) = spawned {
        log::error!("console scheduler did not start: {error}");
    }
}

// ---- REST ----------------------------------------------------------------

fn body_json(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

fn id_field(body: &Value, key: &str) -> Option<String> {
    body.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

/// GET /api/console/tasks
pub fn api_list() -> Response {
    match list() {
        Ok(tasks) => Response::ok_json(json!({"tasks": tasks})),
        Err(e) => Response::err(500, "Internal Server Error", &format!("{e:#}")),
    }
}

/// POST /api/console/tasks/put — `{id, title, prompt?, schedule?, paused?, console?}`
pub fn api_put(body: &[u8]) -> Response {
    let request: Put = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(e) => return Response::err(400, "Bad Request", &format!("a task needs id and title: {e}")),
    };
    match put(request) {
        Ok(task) => Response::ok_json(json!({"task": task})),
        Err(e) => Response::err(400, "Bad Request", &format!("{e:#}")),
    }
}

/// Keep a watch's secret for a task, checked for the watch it is for.
fn keep_watch_secret(task: &str, watch: Option<&Watch>, secret: &str) -> Result<()> {
    let secret = secret.trim();
    if secret.is_empty() || secret.len() > 4096 {
        bail!("the password or address is empty or too long");
    }
    let lower = secret.to_ascii_lowercase();
    let looks_like_address = lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("webcal://");
    if matches!(watch, Some(Watch::Calendar { .. })) && !looks_like_address {
        bail!("a calendar is watched through its iCal address (https:// or webcal://)");
    }
    task_key(task)?;
    super::console_watch::set_secret(task, secret).context("could not keep the secret in the system's credential store")
}

/// POST /api/console/tasks/watch-secret — `{id, secret, kind?}`: the mail
/// password or calendar address of a task's watch, straight into the
/// system's credential store. Sent on its own, before the task, so it is
/// never part of what a page keeps.
pub fn api_watch_secret(body: &[u8]) -> Response {
    let body = body_json(body);
    let (Some(id), Some(secret)) = (id_field(&body, "id"), id_field(&body, "secret")) else {
        return Response::err(400, "Bad Request", "id and secret are required");
    };
    let watch = match body.get("kind").and_then(Value::as_str) {
        Some("calendar") => Some(Watch::Calendar { minutes_before: 15 }),
        _ => None,
    };
    match keep_watch_secret(&id, watch.as_ref(), &secret) {
        Ok(()) => Response::ok_json(json!({"kept": true})),
        Err(e) => Response::err(400, "Bad Request", &format!("{e:#}")),
    }
}

/// POST /api/console/tasks/delete — `{id}`
pub fn api_delete(body: &[u8]) -> Response {
    let Some(id) = id_field(&body_json(body), "id") else {
        return Response::err(400, "Bad Request", "id is required");
    };
    match delete(&id) {
        Ok(()) => Response::ok_json(json!({"deleted": id})),
        Err(e) => Response::err(404, "Not Found", &format!("{e:#}")),
    }
}

/// POST /api/console/tasks/run — `{task_id, run_id?, prompt?, continue_from?}`
pub fn api_run(handler: &McpHandler, body: &[u8]) -> Response {
    let request: RunRequest = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(e) => return Response::err(400, "Bad Request", &format!("task_id is required: {e}")),
    };
    let trigger = if request.continue_from.is_some() { "continue" } else { "manual" };
    match run(handler, request, trigger) {
        Ok(run) => Response::ok_json(json!({"run": run})),
        Err(e) => Response::err(409, "Conflict", &format!("{e:#}")),
    }
}

/// POST /api/console/tasks/stop — `{task_id}`
pub fn api_stop(body: &[u8]) -> Response {
    let Some(id) = id_field(&body_json(body), "task_id") else {
        return Response::err(400, "Bad Request", "task_id is required");
    };
    match stop(&id) {
        Ok(stopped) => Response::ok_json(json!({"stopped": stopped})),
        Err(e) => Response::err(409, "Conflict", &format!("{e:#}")),
    }
}

/// POST /api/console/tasks/output — `{task_id, run_id, path}`: one file a
/// run made, as text when it is text.
pub fn api_output(body: &[u8]) -> Response {
    let body = body_json(body);
    let (Some(task), Some(run), Some(path)) = (id_field(&body, "task_id"), id_field(&body, "run_id"), id_field(&body, "path"))
    else {
        return Response::err(400, "Bad Request", "task_id, run_id and path are required");
    };
    match read_output(&task, &run, &path) {
        Ok(output) => Response::ok_json(json!({"output": output})),
        Err(e) => Response::err(404, "Not Found", &format!("{e:#}")),
    }
}

/// POST /api/console/tasks/thread — `{task_id}`: the task as a conversation.
pub fn api_thread(body: &[u8]) -> Response {
    let body = body_json(body);
    let Some(task) = id_field(&body, "task_id") else {
        return Response::err(400, "Bad Request", "task_id is required");
    };
    match thread(&task) {
        Ok(thread) => Response::ok_json(json!({"thread": thread})),
        Err(e) => Response::err(404, "Not Found", &format!("{e:#}")),
    }
}

fn computer_reply(result: Result<Value>) -> Response {
    match result {
        Ok(value) => Response::ok_json(value),
        Err(e) => Response::err(404, "Not Found", &format!("{e:#}")),
    }
}

/// POST /api/console/computer/terminal — `{session_id, from_line?}`: its output.
pub fn api_terminal(handler: &McpHandler, body: &[u8]) -> Response {
    let body = body_json(body);
    match id_field(&body, "session_id") {
        Some(session) => {
            let from = body.get("from_line").and_then(Value::as_i64);
            computer_reply(terminal_screen(handler, &session, from))
        }
        None => Response::err(400, "Bad Request", "session_id is required"),
    }
}

/// POST /api/console/computer/terminal/input — `{session_id, input}`: keys
/// the person typed, as the bytes a terminal sends.
pub fn api_terminal_input(handler: &McpHandler, body: &[u8]) -> Response {
    let body = body_json(body);
    let input = body.get("input").and_then(Value::as_str);
    match (id_field(&body, "session_id"), input) {
        (Some(session), Some(input)) => match terminal_input(handler, &session, input) {
            Ok(value) => Response::ok_json(value),
            Err(e) => Response::err(409, "Conflict", &format!("{e:#}")),
        },
        _ => Response::err(400, "Bad Request", "session_id and input are required"),
    }
}

/// POST /api/console/handover — `{session_id, handover_id, done, note?}`:
/// the person did what the agent asked them to (or cannot).
pub fn api_handover(body: &[u8]) -> Response {
    let body = body_json(body);
    let done = body.get("done").and_then(Value::as_bool);
    match (id_field(&body, "session_id"), id_field(&body, "handover_id"), done) {
        (Some(session), Some(handover), Some(done)) => {
            let note = body.get("note").and_then(Value::as_str);
            match agent_session::answer_handover(&session, &handover, done, note) {
                Ok(()) => Response::ok_json(json!({"answered": handover, "done": done})),
                Err(e) => Response::err(409, "Conflict", &format!("{e:#}")),
            }
        }
        _ => Response::err(400, "Bad Request", "session_id, handover_id and done are required"),
    }
}

/// POST /api/console/computer/terminal/focus — `{session_id}`.
pub fn api_terminal_focus(handler: &McpHandler, body: &[u8]) -> Response {
    match id_field(&body_json(body), "session_id") {
        Some(session) => computer_reply(focus_terminal(handler, &session)),
        None => Response::err(400, "Bad Request", "session_id is required"),
    }
}

/// POST /api/console/computer/tab — `{tab_id}`: address, title, screenshot.
pub fn api_tab(body: &[u8]) -> Response {
    match id_field(&body_json(body), "tab_id") {
        Some(tab) => computer_reply(tab_view(&tab)),
        None => Response::err(400, "Bad Request", "tab_id is required"),
    }
}

/// GET /api/console/skills
pub fn api_skills() -> Response {
    Response::ok_json(json!({"skills": super::skills::list()}))
}

/// POST /api/console/skills/toggle — `{name, enabled}`.
pub fn api_skill_toggle(body: &[u8]) -> Response {
    let body = body_json(body);
    let (Some(name), Some(enabled)) = (id_field(&body, "name"), body.get("enabled").and_then(Value::as_bool)) else {
        return Response::err(400, "Bad Request", "name and enabled are required");
    };
    match super::skills::set_enabled(&name, enabled) {
        Ok(()) => api_skills(),
        Err(e) => Response::err(400, "Bad Request", &format!("{e:#}")),
    }
}

/// POST /api/console/skills/delete — `{name}`.
pub fn api_skill_delete(body: &[u8]) -> Response {
    let Some(name) = id_field(&body_json(body), "name") else {
        return Response::err(400, "Bad Request", "name is required");
    };
    match super::skills::delete(&name) {
        Ok(()) => api_skills(),
        Err(e) => Response::err(400, "Bad Request", &format!("{e:#}")),
    }
}

/// POST /api/console/skills/import — `{path, replace?}`: a folder holding a SKILL.md.
pub fn api_skill_import(body: &[u8]) -> Response {
    let body = body_json(body);
    let Some(path) = body.get("path").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty()) else {
        return Response::err(400, "Bad Request", "path is required");
    };
    let replace = body.get("replace").and_then(Value::as_bool).unwrap_or(false);
    match super::skills::import(std::path::Path::new(path), replace) {
        Ok(name) => Response::ok_json(json!({"imported": name, "skills": super::skills::list()})),
        Err(e) => Response::err(400, "Bad Request", &format!("{e:#}")),
    }
}

/// POST /api/console/computer/tab/activate — `{tab_id}`.
pub fn api_tab_activate(body: &[u8]) -> Response {
    match id_field(&body_json(body), "tab_id") {
        Some(tab) => computer_reply(activate_tab(&tab)),
        None => Response::err(400, "Bad Request", "tab_id is required"),
    }
}

/// GET /api/console/run-settings
pub fn api_settings_get() -> Response {
    Response::ok_json(json!({"settings": run_settings()}))
}

/// POST /api/console/run-settings — `{agent, cwd, governed, autonomy}`.
/// A field left out keeps its current value: an older console that does
/// not know `autonomy` must not reset it by saving the agent.
pub fn api_settings_set(body: &[u8]) -> Response {
    let mut merged = serde_json::to_value(run_settings()).unwrap_or_else(|_| json!({}));
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(given)) => {
            for (key, value) in given {
                merged[key] = value;
            }
        }
        _ => return Response::err(400, "Bad Request", "settings are a JSON object"),
    }
    let next: RunSettings = match serde_json::from_value(merged) {
        Ok(next) => next,
        Err(e) => return Response::err(400, "Bad Request", &format!("settings: {e}")),
    };
    match set_run_settings(next) {
        Ok(settings) => Response::ok_json(json!({"settings": settings})),
        Err(e) => Response::err(400, "Bad Request", &format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn at(text: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(text).unwrap()
    }

    #[test]
    fn a_daily_slot_is_today_once_its_time_has_come_and_yesterday_before() {
        let daily = Schedule::Daily { time: "09:00".into() };
        assert_eq!(last_slot(&daily, &at("2026-11-10T10:00:00+08:00")), Some(at("2026-11-10T09:00:00+08:00")));
        assert_eq!(last_slot(&daily, &at("2026-11-10T08:59:00+08:00")), Some(at("2026-11-09T09:00:00+08:00")));
        assert_eq!(last_slot(&Schedule::Daily { time: "9am".into() }, &at("2026-11-10T10:00:00+08:00")), None);
    }

    #[test]
    fn a_weekly_slot_is_the_last_such_weekday() {
        // 2026-11-10 is a Tuesday.
        let monday = Schedule::Weekly { time: "10:00".into(), weekday: 1 };
        assert_eq!(last_slot(&monday, &at("2026-11-10T12:00:00+08:00")), Some(at("2026-11-09T10:00:00+08:00")));
        let tuesday = Schedule::Weekly { time: "10:00".into(), weekday: 2 };
        assert_eq!(last_slot(&tuesday, &at("2026-11-10T09:00:00+08:00")), Some(at("2026-11-03T10:00:00+08:00")));
        assert_eq!(last_slot(&tuesday, &at("2026-11-10T10:00:00+08:00")), Some(at("2026-11-10T10:00:00+08:00")));
    }

    #[test]
    fn a_task_is_due_once_per_slot_and_never_for_a_slot_before_it_existed() {
        let daily = Schedule::Daily { time: "09:00".into() };
        let now = at("2026-11-10T09:30:00+08:00");
        // Made after this morning's slot: wait for tomorrow.
        assert!(!is_due(&daily, &now, "2026-11-10T01:15:00Z", None));
        // Made yesterday, never ran: due.
        assert!(is_due(&daily, &now, "2026-11-09T03:00:00Z", None));
        // Ran at the slot (01:00Z is 09:00 +08:00): not again.
        assert!(!is_due(&daily, &now, "2026-11-09T03:00:00Z", Some("2026-11-10T01:00:05Z")));
        // Last ran yesterday: due -- and only once, however many days were missed.
        assert!(is_due(&daily, &now, "2026-11-01T03:00:00Z", Some("2026-11-03T01:00:05Z")));
    }

    #[test]
    fn weekdays_skip_the_weekend_and_once_fires_once() {
        // 2026-11-16 is a Monday; 2026-11-15 a Sunday.
        let weekdays = Schedule::Weekdays { time: "09:00".into() };
        assert_eq!(last_slot(&weekdays, &at("2026-11-16T08:00:00+08:00")), Some(at("2026-11-13T09:00:00+08:00")));
        assert_eq!(last_slot(&weekdays, &at("2026-11-16T09:30:00+08:00")), Some(at("2026-11-16T09:00:00+08:00")));
        let once = Schedule::Once { at: "2026-11-20T14:30".into() };
        assert_eq!(last_slot(&once, &at("2026-11-20T14:00:00+08:00")), None);
        let now = at("2026-11-20T15:00:00+08:00");
        assert!(is_due(&once, &now, "2026-11-18T01:00:00Z", None));
        assert!(!is_due(&once, &now, "2026-11-18T01:00:00Z", Some("2026-11-20T06:30:02Z")));
        assert!(Schedule::Once { at: "tomorrow".into() }.check().is_err());
        assert!(Schedule::Daily { time: "25:00".into() }.check().is_err());
    }

    #[test]
    fn an_interval_counts_from_the_last_run() {
        let every = Schedule::Every { minutes: 60 };
        let now = at("2026-11-18T10:00:00+00:00");
        assert!(!is_due(&every, &now, "2026-11-18T09:30:00Z", None), "made half an hour ago");
        assert!(is_due(&every, &now, "2026-11-18T08:00:00Z", None));
        assert!(!is_due(&every, &now, "2026-11-18T08:00:00Z", Some("2026-11-18T09:10:00Z")));
        assert!(is_due(&every, &now, "2026-11-18T08:00:00Z", Some("2026-11-18T09:00:00Z")));
        assert!(Schedule::Every { minutes: 1 }.check().is_err());
        assert!(!is_due(&Schedule::Every { minutes: 1 }, &now, "2026-11-01T00:00:00Z", None));
    }

    #[test]
    fn the_console_names_and_the_engine_keys_round_trip() {
        assert_eq!(task_key("task-171").unwrap().as_str(), "tsk_task-171");
        assert_eq!(task_key("tsk_abc").unwrap().as_str(), "tsk_abc", "an engine id is not prefixed twice");
        assert_eq!(run_key("run-task-171-1").unwrap().as_str(), "run_run-task-171-1");
        assert!(task_key("").is_err());
    }

    #[test]
    fn a_turn_reads_its_answer_and_ending_from_the_contracts_events() {
        let mut turn = Turn::default();
        for event in [
            json!({"event": "output.delta", "stream": "reasoning", "text": "thinking"}),
            json!({"event": "output.delta", "stream": "assistant", "text": "Done: "}),
            json!({"event": "approval.requested"}),
            json!({"event": "approval.resolved"}),
            json!({"event": "output.delta", "stream": "assistant", "text": "notes.txt written."}),
            json!({"event": "turn.completed", "state": "finished"}),
        ] {
            turn.read(&event);
        }
        assert_eq!(turn.summary(), "Done:\n\nnotes.txt written.", "a tool call between is a paragraph break");
        assert!(!turn.waiting);
        assert_eq!(turn.ended.as_deref(), Some("finished"));
        assert_eq!(verdict("finished", false), State::Succeeded);
        assert_eq!(verdict("exited", false), State::Succeeded);
        assert_eq!(verdict("interrupted", false), State::Cancelled);
        assert_eq!(verdict("interrupted", true), State::Failed, "stopped for idling is a failure");
        assert_eq!(verdict("failed", false), State::Failed);
    }

    #[test]
    fn a_task_folder_is_named_after_it_and_safe_everywhere() {
        assert_eq!(folder_name("Weekly report", "task-1"), "Weekly report");
        assert_eq!(folder_name("  a/b:c?  ", "task-1"), "a b c");
        assert_eq!(folder_name("...", "task-1"), "task-1");
        assert_eq!(folder_name("con", "task-1"), "con task");
        assert_eq!(folder_name("COM1.txt", "task-1"), "COM1.txt task");
        assert_eq!(folder_name("Compare prices", "task-1"), "Compare prices");
        assert_eq!(folder_name(&"x".repeat(100), "task-1").len(), 60);
    }

    #[test]
    fn a_runs_outputs_are_the_files_it_wrote_not_the_ones_already_there() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "before").unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        let since = std::time::SystemTime::now();
        std::thread::sleep(Duration::from_millis(50));
        std::fs::create_dir_all(dir.path().join("out")).unwrap();
        std::fs::write(dir.path().join("out/report.md"), "# Report").unwrap();
        std::fs::write(dir.path().join(".git/HEAD"), "ref").unwrap();
        std::fs::write(dir.path().join("node_modules/pkg/index.js"), "x").unwrap();
        std::fs::write(dir.path().join(".hidden"), "x").unwrap();
        let old = std::fs::File::options().write(true).open(dir.path().join("old.txt")).unwrap();
        old.set_modified(since - Duration::from_secs(60)).unwrap();
        let outputs = outputs_since(dir.path(), since);
        assert_eq!(outputs.iter().map(|o| o.path.as_str()).collect::<Vec<_>>(), vec!["out/report.md"]);
        assert_eq!(outputs[0].size, 8);
    }

    #[test]
    fn the_result_is_the_files_the_reply_names_not_everything_written() {
        let output = |path: &str| Output { path: path.into(), size: 1, modified: String::new() };
        let written = vec![
            output("HN-Top10.md"),
            output("scroll.js"),
            output("repo/README.md"),
            output("repo/Cargo.lock"),
            output("out/posts.csv"),
        ];
        let reply = "Saved the list as HN-Top10.md and the raw data as `out\\posts.csv`.";
        let named = named_in(written, reply);
        assert_eq!(named.iter().map(|o| o.path.as_str()).collect::<Vec<_>>(), vec!["HN-Top10.md", "out/posts.csv"]);
        // A reply that names none of them: all are kept rather than none.
        assert_eq!(named_in(vec![output("notes.md"), output("tmp.txt")], "Done.").len(), 2);
    }

    #[test]
    fn standing_helpers_are_tasks_that_keep_their_conversation() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("UNTERM_STATE_DIR", dir.path());
        std::env::set_var("UNTERM_TASKS_DB", dir.path().join("tasks.db"));
        unterm_services::cockpit::fleet_store::reset_for_tests();

        put(Put { id: "inbox".into(), title: "Invoice inbox".into(), keep_conversation: true, agent: Some("codex-cli".into()), ..Put::default() }).unwrap();
        put(Put { id: "shop".into(), title: "Shop support".into(), keep_conversation: true, ..Put::default() }).unwrap();
        put(Put { id: "once".into(), title: "One-off report".into(), ..Put::default() }).unwrap();
        put(Put { id: "rest".into(), title: "Resting helper".into(), keep_conversation: true, paused: true, ..Put::default() }).unwrap();
        assert!(put(Put { id: "x".into(), title: "x".into(), agent: Some("notepad".into()), ..Put::default() }).is_err());

        let mut helpers = standing_helpers().unwrap();
        helpers.sort_by(|a, b| a.id.cmp(&b.id));
        let named: Vec<(&str, &str)> = helpers.iter().map(|h| (h.id.as_str(), h.agent.as_str())).collect();
        assert_eq!(named, vec![("inbox", "codex-cli"), ("shop", run_settings().agent.as_str())]);
        assert_eq!(find_helper("invoice").unwrap().id, "inbox", "a word of its title is enough");
        assert_eq!(find_helper("SHOP SUPPORT").unwrap().id, "shop");
        let unknown = find_helper("payroll").unwrap_err().to_string();
        assert!(unknown.contains("Invoice inbox") && unknown.contains("Shop support"), "{unknown}");
        assert!(find_helper("one-off").is_err(), "a task that does not keep its conversation is no standing helper");
        assert!(autonomy_rank(Autonomy::Cautious) < autonomy_rank(Autonomy::Balanced));
        assert!(autonomy_rank(Autonomy::Balanced) < autonomy_rank(Autonomy::Autonomous));
    }

    #[test]
    fn tasks_are_kept_and_an_orphaned_run_is_closed_as_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("UNTERM_STATE_DIR", dir.path());
        std::env::set_var("UNTERM_TASKS_DB", dir.path().join("tasks.db"));
        unterm_services::cockpit::fleet_store::reset_for_tests();

        let made = put(Put {
            id: "task-1".into(),
            title: "weekly report".into(),
            prompt: Some("write it".into()),
            schedule: Some(Schedule::Weekly { time: "09:00".into(), weekday: 5 }),
            paused: false,
            console: json!({"status": "待确认", "target": "repo"}),
            ..Put::default()
        })
        .unwrap();
        assert_eq!(made["id"], "task-1");
        assert_eq!(made["console"]["target"], "repo", "the console's view comes back untouched");

        // Editing keeps the id; the engine has one task, not two.
        put(Put { id: "task-1".into(), title: "monthly report".into(), prompt: None, schedule: None, paused: true, console: json!({}), ..Put::default() })
            .unwrap();
        let tasks = list().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["title"], "monthly report");
        assert_eq!(tasks[0]["paused"], true);
        assert!(tasks[0]["schedule"].is_null());

        // A run left running by a process that is gone.
        let store = store().unwrap();
        let task = console_task(&store, "task-1").unwrap();
        let run = store.start_run_as(&task.id, run_key("run-old").unwrap()).unwrap();
        let step = store.request_step_with_detail(&run.id, STEP_KIND, None, json!({"console_run_id": "run-old"})).unwrap().step().clone();
        store.set_step_detail(&step.id, json!({"console_run_id": "run-old", "session_id": "ags_gone"}), State::Running).unwrap();
        assert_eq!(sweep_orphans().unwrap(), 1);
        let runs = list().unwrap()[0]["runs"].clone();
        assert_eq!(runs[0]["id"], "run-old");
        assert_eq!(runs[0]["state"], "interrupted");
        assert!(runs[0]["error"].as_str().unwrap().contains("stopped"));
        assert_eq!(runs[0]["error_code"], "service_restarted");
        assert_eq!(sweep_orphans().unwrap(), 0);

        // With no directory in the settings, a task works in its own folder,
        // kept across edits and renames.
        let root = dir.path().join("Documents").join("Unzoo One");
        std::env::set_var("UNTERM_WORK_ROOT", &root);
        let task = console_task(&store, "task-1").unwrap();
        let workdir = workdir_for(&store, &task, &RunSettings::default()).unwrap();
        assert_eq!(std::path::PathBuf::from(&workdir), root.join("monthly report"));
        assert!(std::path::Path::new(&workdir).is_dir());
        put(Put { id: "task-1".into(), title: "renamed".into(), prompt: None, schedule: None, paused: true, console: json!({}), ..Put::default() })
            .unwrap();
        assert_eq!(list().unwrap()[0]["workdir"], json!(workdir));
        let task = console_task(&store, "task-1").unwrap();
        assert_eq!(workdir_for(&store, &task, &RunSettings::default()).unwrap(), workdir);
        // A second task of the same name gets a folder of its own.
        put(Put { id: "task-2".into(), title: "monthly report".into(), prompt: None, schedule: None, paused: true, console: json!({}), ..Put::default() })
            .unwrap();
        let other = console_task(&store, "task-2").unwrap();
        assert_eq!(
            std::path::PathBuf::from(workdir_for(&store, &other, &RunSettings::default()).unwrap()),
            root.join("monthly report (2)")
        );
        delete("task-2").unwrap();

        // What a run wrote can be read back, and nothing else.
        std::fs::write(std::path::Path::new(&workdir).join("report.md"), "\u{feff}# Done\n").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "no").unwrap();
        let mut detail = store.step(&step.id).unwrap().unwrap().detail;
        detail["cwd"] = json!(workdir);
        detail["outputs"] = json!([{"path": "report.md", "size": 7, "modified": "2026-11-11T00:00:00Z"},
                                   {"path": "../../../secret.txt", "size": 2, "modified": "2026-11-11T00:00:00Z"}]);
        store.set_step_detail(&step.id, detail, State::Interrupted).unwrap();
        let read = read_output("task-1", "run-old", "report.md").unwrap();
        assert_eq!(read["text"], "# Done\n");
        assert_eq!(read["binary"], false);
        assert_eq!(list().unwrap()[0]["runs"][0]["outputs"][0]["path"], "report.md");
        assert!(read_output("task-1", "run-old", "other.md").is_err(), "not listed");
        assert!(read_output("task-1", "run-old", "../../../secret.txt").is_err(), "outside the folder");
        std::env::remove_var("UNTERM_WORK_ROOT");

        // A paused task with no schedule is no reason to stay; a schedule is.
        assert!(!has_work().unwrap());
        put(Put {
            id: "task-1".into(),
            title: "renamed".into(),
            prompt: None,
            schedule: Some(Schedule::Daily { time: "09:00".into() }),
            paused: false,
            console: json!({}),
            ..Put::default()
        })
        .unwrap();
        assert!(has_work().unwrap());

        delete("task-1").unwrap();
        assert!(list().unwrap().is_empty());
        assert!(!has_work().unwrap());
        assert!(delete("task-1").is_err());

        // Settings refuse what a scheduled run could not use.
        assert!(set_run_settings(RunSettings { agent: "aider".into(), ..RunSettings::default() }).is_err());
        assert!(set_run_settings(RunSettings { cwd: "Z:\\no\\such\\dir".into(), ..RunSettings::default() }).is_err());
        let kept = set_run_settings(RunSettings {
            agent: "codex-cli".into(),
            cwd: dir.path().display().to_string(),
            governed: false,
            autonomy: Autonomy::Cautious,
            browser_profile: String::new(),
        })
        .unwrap();
        assert_eq!(run_settings(), kept);
        // Saving without `autonomy` (an older console) keeps it.
        let response = api_settings_set(br#"{"agent": "claude-code", "cwd": "", "governed": true}"#);
        assert_eq!(response.status, 200);
        assert_eq!(run_settings().autonomy, Autonomy::Cautious);
        assert_eq!(run_settings().agent, "claude-code");
        api_settings_set(br#"{"autonomy": "autonomous"}"#);
        assert_eq!(run_settings().autonomy, Autonomy::Autonomous);
        assert_eq!(api_settings_set(br#"{"autonomy": "yolo"}"#).status, 400);
        assert_eq!(run_settings().autonomy, Autonomy::Autonomous);
    }

    fn event(value: Value) -> Value {
        value
    }

    #[test]
    fn a_turn_records_what_the_agent_did_and_who_let_it() {
        let mut turn = Turn { cwd: Some(if cfg!(windows) { r"C:\work".into() } else { "/work".into() }), ..Turn::default() };
        for e in [
            event(json!({"event": "turn.started"})),
            event(json!({"event": "output.delta", "stream": "assistant", "text": "Looking."})),
            event(json!({"event": "tool_call.requested", "tool_call": {"tool": "Read", "arguments": {"file_path": "a.md"}, "external_call_id": "c1"}})),
            event(json!({"event": "tool_call.result", "external_call_id": "c1", "ok": true})),
            event(json!({"event": "tool_call.requested", "tool_call": {"tool": "Bash", "arguments": {"command": "npm test"}, "external_call_id": "c2"}})),
            event(json!({"event": "approval.requested", "approval": {"request_id": "r1", "risk": "exec", "tool_call": {"tool": "Bash", "arguments": {"command": "npm test"}}}})),
            event(json!({"event": "approval.resolved", "approval": {"request_id": "r1", "decided_by": "autonomy", "decision": "allow_once"}})),
            event(json!({"event": "tool_call.requested", "tool_call": {"tool": "Bash", "arguments": {"command": "git push"}, "external_call_id": "c3"}})),
            event(json!({"event": "approval.requested", "approval": {"request_id": "r2", "risk": "external_side_effect", "tool_call": {"tool": "Bash", "arguments": {"command": "git push"}}}})),
            event(json!({"event": "approval.resolved", "approval": {"request_id": "r2", "decided_by": "caller", "decision": "deny"}})),
            // Asked about before the CLI said it was calling: a line of its own.
            event(json!({"event": "approval.requested", "approval": {"request_id": "r3", "tool_call": {"tool": "mcp__unzoo__browser_navigate", "arguments": {"url": "https://example.com"}}}})),
            event(json!({"event": "output.delta", "stream": "assistant", "text": "Done."})),
        ] {
            turn.read(&e);
        }
        let a = &turn.activity;
        assert_eq!(a.len(), 4, "{a:#?}");
        assert_eq!((a[0]["verb"].as_str(), a[0]["target"].as_str(), a[0]["risk"].as_str()), (Some("read"), Some("a.md"), Some("read")));
        assert_eq!(a[0]["ok"], true);
        assert_eq!((a[1]["decided_by"].as_str(), a[1]["decision"].as_str()), (Some("autonomy"), Some("allow")));
        assert_eq!((a[2]["decided_by"].as_str(), a[2]["decision"].as_str()), (Some("person"), Some("deny")));
        assert_eq!(a[2]["risk"], "external_side_effect");
        assert_eq!((a[3]["verb"].as_str(), a[3]["target"].as_str()), (Some("open"), Some("https://example.com")));
        assert_eq!(a[3]["risk"], "exec");
        assert!(turn.waiting, "r3 is still open");
        assert_eq!(turn.reply(), "Looking.\n\nDone.");
    }

    #[test]
    fn ids_are_found_in_tool_output_even_inside_escaped_json() {
        assert_eq!(id_after(r#"{"session_id":"17","cols":120}"#, "session_id").as_deref(), Some("17"));
        assert_eq!(
            id_after(r#"[{"type":"text","text":"{\"tab_id\": 1030143360, \"url\": \"x\"}"}]"#, "tab_id").as_deref(),
            Some("1030143360")
        );
        assert_eq!(id_after("no ids here", "tab_id"), None);
        assert_eq!(id_after(r#"{"tab_id": null}"#, "tab_id").as_deref(), Some("null"));
    }

    #[test]
    fn a_turn_remembers_the_terminals_and_tabs_its_agent_used() {
        let mut turn = Turn::default();
        for e in [
            json!({"event": "tool_call.requested", "tool_call": {"tool": "mcp__unterm__session_create", "arguments": {}, "external_call_id": "c1"}}),
            json!({"event": "tool_call.result", "external_call_id": "c1", "ok": true,
                   "output": r#"[{"type":"text","text":"{\"session_id\":\"5\"}"}]"#}),
            json!({"event": "tool_call.requested", "tool_call": {"tool": "mcp__unterm__exec_run_wait", "arguments": {"session_id": "5", "command": "dir"}, "external_call_id": "c2"}}),
            json!({"event": "tool_call.requested", "tool_call": {"tool": "mcp__unterm__screen_text", "arguments": {"session_id": 9}, "external_call_id": "c3"}}),
            json!({"event": "tool_call.requested", "tool_call": {"tool": "mcp__unzoo__tab_create", "arguments": {"url": "https://x"}, "external_call_id": "c4"}}),
            json!({"event": "tool_call.result", "external_call_id": "c4", "ok": true, "output": r#"{"tab_id": 77}"#}),
            json!({"event": "tool_call.requested", "tool_call": {"tool": "mcp__unzoo__browser_get_title", "arguments": {"tab_id": 77}, "external_call_id": "c5"}}),
            // Another program's output naming a session is not a terminal.
            json!({"event": "tool_call.requested", "tool_call": {"tool": "Bash", "arguments": {"command": "echo"}, "external_call_id": "c6"}}),
            json!({"event": "tool_call.result", "external_call_id": "c6", "ok": true, "output": r#"{"session_id":"99"}"#}),
        ] {
            turn.read(&e);
        }
        assert_eq!(turn.terminals, vec!["5".to_string(), "9".to_string()]);
        assert_eq!(turn.tabs, vec!["77".to_string()]);
        // Only what it opened itself is closed when the run ends: terminal 9
        // was there before (the person's, perhaps).
        assert_eq!(turn.opened_terminals, vec!["5".to_string()]);
        assert_eq!(turn.opened_tabs, vec!["77".to_string()]);
    }

    #[test]
    fn tools_are_known_by_their_server_whichever_agent_names_them() {
        assert!(from_server("mcp__unzoo__tab_create", "unzoo"));
        assert!(from_server("unzoo.tab_create", "unzoo"));
        assert!(!from_server("mcp__unzoo_x__tab_create", "unzoo"));
        assert!(from_server("unterm_exec_run_wait", "unterm"));
        assert!(!from_server("unzooish.tab_create", "unzoo"));
        assert!(!from_server("shell", "unterm"));
    }

    #[test]
    fn a_closed_terminal_is_read_from_where_the_viewer_is() {
        let kept = json!({"session_id": "5", "text": "a\nb\nc", "first_row": 10, "row_count": 3, "closed": true});
        assert_eq!(kept_from(kept.clone(), None), kept);
        let rest = kept_from(kept.clone(), Some(11));
        assert_eq!((rest["first_row"].as_i64(), rest["text"].as_str(), rest["row_count"].as_u64()), (Some(11), Some("b\nc"), Some(2)));
        // Before the start: all of it; past the end: nothing more.
        assert_eq!(kept_from(kept.clone(), Some(0))["text"], "a\nb\nc");
        assert_eq!(kept_from(kept, Some(40))["row_count"], 0);
    }

    #[test]
    fn a_very_long_reply_keeps_its_beginning_and_end() {
        let turn = Turn { text: format!("start{}end", "é".repeat(MAX_REPLY)), ..Turn::default() };
        let reply = turn.reply();
        assert!(reply.starts_with("start") && reply.ends_with("end"));
        assert!(reply.contains("\n\n…\n\n"));
        assert!(reply.len() <= MAX_REPLY + 16);
    }
}
