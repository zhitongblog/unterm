//! Driving a brain in a real terminal pane.
//!
//! The headless route in `agent_run` is the cleaner one — structured events,
//! token counts, a real interrupt. But on a machine where `claude -p` cannot
//! authenticate, the interactive CLI still can, so this route runs the agent
//! the way a person would: in a pane, typing at it, reading the screen.
//!
//! Two things keep that from being a hole:
//!
//! 1. **Which command** — same rule as `agent_run`: the caller names an agent,
//!    the command comes from that agent's manifest. Never an argv.
//! 2. **Which pane** — this module only ever writes to panes it opened itself.
//!    A caller cannot name one of the user's own terminals and type into it.
//!    That is what `OWNED_PANES` is for.
//!
//! Reading the screen back is done *here*, not by the caller. A TUI agent
//! redraws one screen rather than appending to scrollback, so "read what is
//! new since line N" does not work on it; what does work is knowing the
//! agent's own line markers. That knowledge belongs next to the terminal,
//! not spread across every client that wants to run an agent.

use super::server::Response;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Mutex;
use unterm_agents::{fetch_manifests, installer};
use unterm_mcp::handler::McpHandler;

/// Panes this module opened. Only these can be typed into or read.
static OWNED_PANES: Mutex<Option<HashSet<u64>>> = Mutex::new(None);

fn owned() -> std::sync::MutexGuard<'static, Option<HashSet<u64>>> {
    let mut guard = OWNED_PANES.lock().unwrap_or_else(|p| p.into_inner());
    if guard.is_none() {
        *guard = Some(HashSet::new());
    }
    guard
}

fn remember(pane: u64) {
    if let Some(set) = owned().as_mut() {
        set.insert(pane);
    }
}

fn is_ours(pane: u64) -> bool {
    owned().as_ref().is_some_and(|set| set.contains(&pane))
}

fn forget(pane: u64) {
    if let Some(set) = owned().as_mut() {
        set.remove(&pane);
    }
}

fn parse_body(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

fn string_field<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn pane_field(body: &Value) -> Option<u64> {
    body.get("pane_id").and_then(Value::as_u64)
}

fn ctx() -> unterm_mcp::handler::ConnectionContext {
    unterm_mcp::handler::ConnectionContext::internal("web_settings")
}

/// Where a turn stands, read off the agent's own screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    /// The agent is mid-turn.
    Working,
    /// The turn finished; `answer` is complete.
    Done,
    /// Nothing has been asked yet, or the agent is sitting at its prompt.
    Idle,
}

impl TurnState {
    fn as_str(self) -> &'static str {
        match self {
            TurnState::Working => "working",
            TurnState::Done => "done",
            TurnState::Idle => "idle",
        }
    }
}

/// One turn as read off the screen.
#[derive(Debug, Clone, Default)]
pub struct Turn {
    pub prompt: Option<String>,
    pub answer: Option<String>,
}

/// Read a turn out of Claude Code's TUI.
///
/// Its transcript has three stable line markers, and this reads only those —
/// not colour, not the box drawing, not the banner:
///
/// ```text
/// ❯ what the user asked
/// ● what it answered
///   continued answer lines are indented
/// ✻ Crunched for 2s · done 22:58
/// ```
///
/// Everything else on screen is chrome. The trailing `❯` with nothing after
/// it is the empty input box, not a prompt.
fn parse_claude_turn(lines: &[String]) -> (TurnState, Turn) {
    let mut turn = Turn::default();
    let mut answer: Vec<String> = Vec::new();
    let mut collecting = false;
    let mut saw_done = false;

    for line in lines {
        let trimmed = line.trim_end();
        if let Some(rest) = trimmed.strip_prefix("❯ ") {
            let asked = rest.trim();
            if !asked.is_empty() {
                // A new prompt starts a new turn: drop whatever we had.
                turn.prompt = Some(asked.to_string());
                answer.clear();
                collecting = false;
                saw_done = false;
            }
        } else if let Some(rest) = trimmed.strip_prefix("● ") {
            answer.clear();
            answer.push(rest.trim().to_string());
            collecting = true;
        } else if trimmed.starts_with('✻') || trimmed.starts_with('✳') {
            // The status line closes the answer. `· done` means finished;
            // without it the agent is still working on this turn.
            collecting = false;
            if trimmed.contains("· done") || trimmed.contains(" done ") {
                saw_done = true;
            }
        } else if collecting {
            let continued = trimmed.trim();
            if continued.is_empty() || continued.starts_with('─') {
                collecting = false;
            } else {
                answer.push(continued.to_string());
            }
        }
    }

    if !answer.is_empty() {
        turn.answer = Some(answer.join(" "));
    }
    let state = if turn.prompt.is_none() {
        TurnState::Idle
    } else if saw_done {
        TurnState::Done
    } else {
        TurnState::Working
    };
    (state, turn)
}

/// The command that starts an agent's interactive session.
///
/// `detect` resolves the binary; on Windows that is a `.cmd` shim, which a
/// PTY spawns fine as a shell command but not as a bare argv0.
fn launch_command(agent_id: &str) -> Result<String, Response> {
    // Same allowlist as the headless route. Only Claude Code's transcript is
    // understood by `parse_claude_turn`; codex's TUI has a different shape and
    // is deliberately not claimed here until it is read as carefully.
    if agent_id != "claude-code" {
        return Err(Response::err(
            400,
            "Bad Request",
            &format!("{agent_id} is not an agent the console can drive in a pane"),
        ));
    }
    let set = fetch_manifests()
        .map_err(|e| Response::err(503, "Service Unavailable", &format!("manifest fetch: {e}")))?;
    let manifest = set
        .for_current_platform()
        .into_iter()
        .find(|m| m.id == agent_id)
        .ok_or_else(|| Response::err(404, "Not Found", &format!("no agent named {agent_id}")))?;
    let detected = installer::detect(&manifest.detect);
    if !detected.ok {
        return Err(Response::err(
            503,
            "Service Unavailable",
            &format!("{} is not installed on this machine", manifest.name),
        ));
    }
    detected
        .binary_path
        
        .ok_or_else(|| Response::err(503, "Service Unavailable", "the agent has no resolved path"))
}

fn screen_lines(handler: &McpHandler, pane: u64) -> Result<Vec<String>, Response> {
    let params = json!({ "pane_id": pane });
    let value = handler
        .handle(&ctx(), "screen.text", &params)
        .map_err(|e| Response::err(400, "Bad Request", &e.to_string()))?;
    Ok(value
        .get("lines")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| row.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default())
}

/// POST /api/pty/start — open a pane running an agent interactively.
pub fn api_start(handler: &McpHandler, body: &[u8]) -> Response {
    let body = parse_body(body);
    let Some(agent_id) = string_field(&body, "agent") else {
        return Response::err(400, "Bad Request", "agent is required");
    };
    let command = match launch_command(agent_id) {
        Ok(command) => command,
        Err(response) => return response,
    };

    let mut params = serde_json::Map::new();
    params.insert("command".into(), json!(command));
    if let Some(cwd) = string_field(&body, "cwd") {
        params.insert("cwd".into(), json!(cwd));
    }

    match handler.handle(&ctx(), "session.create", &Value::Object(params)) {
        Ok(value) => {
            let Some(pane) = value.get("id").and_then(Value::as_u64) else {
                return Response::err(502, "Bad Gateway", "the pane came back without an id");
            };
            remember(pane);
            Response::ok_json(json!({ "pane_id": pane, "agent": agent_id }))
        }
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

/// POST /api/pty/input — type into one of our panes.
///
/// `text` goes in as-is; `enter` appends a carriage return. Splitting the two
/// lets a caller fill the prompt and submit separately, and lets it send a
/// bare Escape or Ctrl-C without a stray newline.
pub fn api_input(handler: &McpHandler, body: &[u8]) -> Response {
    let body = parse_body(body);
    let Some(pane) = pane_field(&body) else {
        return Response::err(400, "Bad Request", "pane_id is required");
    };
    if !is_ours(pane) {
        return Response::err(
            403,
            "Forbidden",
            "that pane was not opened by the console; it will not be typed into",
        );
    }

    let mut input = string_field(&body, "text").unwrap_or_default().to_string();
    match string_field(&body, "key") {
        Some("escape") => input.push('\u{1b}'),
        Some("interrupt") => input.push('\u{3}'),
        Some(other) => return Response::err(400, "Bad Request", &format!("unknown key: {other}")),
        None => {}
    }
    if body.get("enter").and_then(Value::as_bool).unwrap_or(false) {
        input.push('\r');
    }
    if input.is_empty() {
        return Response::err(400, "Bad Request", "nothing to send");
    }

    let params = json!({ "pane_id": pane, "input": input });
    match handler.handle(&ctx(), "session.input", &params) {
        Ok(value) => Response::ok_json(value),
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

/// POST /api/pty/turn — where the current turn stands, and what it said.
///
/// Returns the parsed turn *and* the raw screen. The parse is the useful
/// part; the screen is there so a caller can show what the terminal actually
/// looks like rather than trusting the parse blindly.
pub fn api_turn(handler: &McpHandler, body: &[u8]) -> Response {
    let body = parse_body(body);
    let Some(pane) = pane_field(&body) else {
        return Response::err(400, "Bad Request", "pane_id is required");
    };
    if !is_ours(pane) {
        return Response::err(
            403,
            "Forbidden",
            "that pane was not opened by the console; it will not be read",
        );
    }
    let lines = match screen_lines(handler, pane) {
        Ok(lines) => lines,
        Err(response) => return response,
    };
    let (state, turn) = parse_claude_turn(&lines);
    Response::ok_json(json!({
        "pane_id": pane,
        "state": state.as_str(),
        "prompt": turn.prompt,
        "answer": turn.answer,
        "screen": lines,
    }))
}

/// POST /api/pty/stop — close one of our panes.
pub fn api_stop(handler: &McpHandler, body: &[u8]) -> Response {
    let body = parse_body(body);
    let Some(pane) = pane_field(&body) else {
        return Response::err(400, "Bad Request", "pane_id is required");
    };
    if !is_ours(pane) {
        return Response::err(403, "Forbidden", "that pane was not opened by the console");
    }
    let params = json!({ "pane_id": pane });
    let result = handler.handle(&ctx(), "session.destroy", &params);
    // Forget it either way: if the destroy failed because the pane is already
    // gone, holding the id only lets a later request address a pane we no
    // longer know anything about.
    forget(pane);
    match result {
        Ok(value) => Response::ok_json(value),
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owned-pane set is process-wide, so these take turns.
    static PANE_SET: Mutex<()> = Mutex::new(());

    fn clear() {
        if let Some(set) = owned().as_mut() {
            set.clear();
        }
    }

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    #[test]
    fn a_pane_we_did_not_open_is_not_ours() {
        let _lock = PANE_SET.lock().unwrap_or_else(|p| p.into_inner());
        clear();
        remember(42);
        assert!(is_ours(42));
        // The user's own terminals are not addressable through this module.
        assert!(!is_ours(1));
        clear();
    }

    #[test]
    fn stopping_a_pane_forgets_it() {
        let _lock = PANE_SET.lock().unwrap_or_else(|p| p.into_inner());
        clear();
        remember(7);
        forget(7);
        assert!(!is_ours(7), "a stopped pane stops being addressable");
        clear();
    }

    #[test]
    fn only_claude_can_be_driven_in_a_pane() {
        // Refused before any manifest lookup or spawn. codex is on the
        // headless route's allowlist but not this one: its TUI has not been
        // read as carefully as Claude Code's.
        for id in ["codex-cli", "gemini-cli", "", "bash", "../../evil"] {
            assert!(launch_command(id).is_err(), "{id} should be refused");
        }
    }

    #[test]
    fn a_finished_turn_reads_back_prompt_and_answer() {
        // Taken off a real session, banner and box drawing included.
        let screen = lines(
            " ▐▛███▛█   Claude Code v2.1.251\n\
             ▝▜██████▀  Opus 5 (1M context) · Claude Max\n\
             \n\
             ❯ 用一句话说明什么是 PTY\n\
             \n\
             ● PTY（伪终端）是一对内核提供的虚拟设备，一端让程序以为自己连着真实终端、\n\
             \x20 另一端交给终端模拟器收发数据。\n\
             \n\
             ✻ Sautéed for 4s · done 23:08\n\
             ────────────────────────\n\
             ❯\n",
        );
        let (state, turn) = parse_claude_turn(&screen);
        assert_eq!(state, TurnState::Done);
        assert_eq!(turn.prompt.as_deref(), Some("用一句话说明什么是 PTY"));
        let answer = turn.answer.expect("answer");
        assert!(answer.starts_with("PTY（伪终端）"), "{answer}");
        assert!(answer.contains("另一端交给终端模拟器"), "续行也要收进来: {answer}");
        assert!(!answer.contains("─"), "边框不算答案: {answer}");
    }

    #[test]
    fn a_turn_still_running_is_not_done() {
        let screen = lines("❯ 帮我查一下\n\n● 正在读取页面\n\n✻ Crunched for 2s\n");
        let (state, turn) = parse_claude_turn(&screen);
        assert_eq!(state, TurnState::Working, "没有 done 标记就还没结束");
        assert_eq!(turn.answer.as_deref(), Some("正在读取页面"));
    }

    #[test]
    fn a_fresh_pane_is_idle() {
        let screen = lines(" ▐▛███▛█   Claude Code v2.1.251\n\n❯\n────────\n");
        let (state, turn) = parse_claude_turn(&screen);
        assert_eq!(state, TurnState::Idle, "空输入框不算一次提问");
        assert!(turn.prompt.is_none());
        assert!(turn.answer.is_none());
    }

    #[test]
    fn a_second_prompt_starts_a_new_turn() {
        let screen = lines(
            "❯ 第一个问题\n● 第一个回答\n✻ done 22:58\n❯ 第二个问题\n● 第二个回答\n✻ 思考中\n",
        );
        let (state, turn) = parse_claude_turn(&screen);
        assert_eq!(turn.prompt.as_deref(), Some("第二个问题"));
        assert_eq!(turn.answer.as_deref(), Some("第二个回答"), "上一轮的回答不能串进来");
        assert_eq!(state, TurnState::Working);
    }

    #[test]
    fn a_malformed_body_does_not_panic() {
        assert_eq!(parse_body(b"}{"), Value::Null);
        assert!(pane_field(&parse_body(b"}{")).is_none());
    }
}
