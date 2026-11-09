//! The CLIs, read into one vocabulary.
//!
//! Codex, Claude, Gemini, OpenCode and Antigravity (`agy`) all emit JSON Lines, and they disagree about almost
//! everything above that: what a turn is called, where the text lives, how a
//! tool call is spelled, whether usage arrives per turn or per message. The
//! adapters absorb that so nothing downstream has to know which one ran.
//!
//! Both are pure. They hold parse state — a partially seen turn — and nothing
//! else; no process, no file, no clock. That is what lets the equivalence
//! test feed both a recording and compare.

use crate::{BrainAdapter, BrainEvent, StopReason, Usage};
use serde_json::Value;

/// Pull a string field, tolerating the several places each CLI puts it.
fn text_at<'a>(value: &'a Value, paths: &[&[&str]]) -> Option<&'a str> {
    for path in paths {
        let mut cursor = value;
        let mut ok = true;
        for key in *path {
            match cursor.get(key) {
                Some(next) => cursor = next,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            if let Some(text) = cursor.as_str() {
                return Some(text);
            }
        }
    }
    None
}

fn u64_at(value: &Value, keys: &[&str]) -> u64 {
    for key in keys {
        if let Some(found) = value.get(key).and_then(Value::as_u64) {
            return found;
        }
    }
    0
}

/// Reads the Codex CLI's JSONL stream.
#[derive(Default)]
pub struct CodexAdapter {
    turn_open: bool,
    session: Option<String>,
    /// A non-transient `error` was seen in the open turn. The turn is not
    /// over until `turn.failed` (or EOF) says so, but if the stream stops
    /// here it ended in error, not by interruption.
    errored: bool,
}

impl CodexAdapter {
    pub fn new() -> Self {
        Self::default()
    }
}

impl BrainAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn on_line(&mut self, line: &str) -> Vec<BrainEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            // A line that is not JSON is not nothing. Swallowing it would
            // make a stream that has started emitting diagnostics on stdout
            // look like a quiet one.
            return vec![BrainEvent::Error {
                message: format!("codex: unparsable line: {}", truncate(line)),
            }];
        };
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let Some(session) = text_at(&value, &[&["session_id"], &["session", "id"], &["thread_id"]])
        {
            // Codex names the session on its first line and repeats it; the
            // last one seen wins, which is also the one `resume` wants.
            self.session = Some(session.to_string());
        }
        let mut events = Vec::new();

        match kind.as_str() {
            "turn.started" | "task_started" => {
                self.turn_open = true;
                self.errored = false;
                events.push(BrainEvent::TurnStarted {
                    model: text_at(&value, &[&["model"], &["turn", "model"]])
                        .map(str::to_string),
                });
            }
            // `codex exec --json` (0.40 and later) reports work as items:
            // started when they begin, completed with their outcome.
            "item.started" | "item.updated" | "item.completed"
                if value.get("item").and_then(|item| item.get("type")).is_some() =>
            {
                let item = &value["item"];
                let completed = kind == "item.completed";
                let started = kind == "item.started";
                let id = text_at(item, &[&["id"]]).unwrap_or("unknown").to_string();
                match item.get("type").and_then(Value::as_str).unwrap_or_default() {
                    "agent_message" if completed => {
                        if let Some(text) = text_at(item, &[&["text"]]).filter(|t| !t.is_empty()) {
                            events.push(BrainEvent::Text { text: text.to_string() });
                        }
                    }
                    "reasoning" if completed => {
                        if let Some(text) = text_at(item, &[&["text"]]).filter(|t| !t.is_empty()) {
                            events.push(BrainEvent::Reasoning { text: text.to_string() });
                        }
                    }
                    "command_execution" => {
                        if started {
                            events.push(BrainEvent::ToolRequested {
                                call_id: id,
                                name: "shell".into(),
                                arguments: serde_json::json!({
                                    "command": item.get("command").cloned().unwrap_or(Value::Null)
                                }),
                            });
                        } else if completed {
                            let ok = item
                                .get("exit_code")
                                .and_then(Value::as_i64)
                                .map(|code| code == 0)
                                .unwrap_or_else(|| text_at(item, &[&["status"]]) != Some("failed"));
                            events.push(BrainEvent::ToolResult {
                                call_id: id,
                                ok,
                                output: text_at(item, &[&["aggregated_output"], &["output"]])
                                    .map(str::to_string),
                            });
                        }
                    }
                    "mcp_tool_call" => {
                        let name = match (text_at(item, &[&["server"]]), text_at(item, &[&["tool"]])) {
                            (Some(server), Some(tool)) => format!("{server}.{tool}"),
                            (None, Some(tool)) => tool.to_string(),
                            _ => "mcp".to_string(),
                        };
                        if started {
                            events.push(BrainEvent::ToolRequested {
                                call_id: id,
                                name,
                                arguments: item.get("arguments").cloned().unwrap_or(Value::Null),
                            });
                        } else if completed {
                            events.push(BrainEvent::ToolResult {
                                call_id: id,
                                ok: text_at(item, &[&["status"]]) != Some("failed"),
                                output: item.get("result").map(|result| result.to_string()),
                            });
                        }
                    }
                    // A patch arrives finished: one request and its result.
                    "file_change" if completed => {
                        events.push(BrainEvent::ToolRequested {
                            call_id: id.clone(),
                            name: "apply_patch".into(),
                            arguments: item.get("changes").cloned().unwrap_or(Value::Null),
                        });
                        events.push(BrainEvent::ToolResult {
                            call_id: id,
                            ok: text_at(item, &[&["status"]]) != Some("failed"),
                            output: None,
                        });
                    }
                    "error" if completed => {
                        events.push(BrainEvent::Error {
                            message: text_at(item, &[&["message"]])
                                .unwrap_or("codex reported an error")
                                .to_string(),
                        });
                    }
                    _ => {}
                }
            }
            "item.completed" | "agent_message" | "message" => {
                if let Some(text) =
                    text_at(&value, &[&["text"], &["item", "text"], &["message"]])
                {
                    if !text.is_empty() {
                        events.push(BrainEvent::Text {
                            text: text.to_string(),
                        });
                    }
                }
            }
            "reasoning" | "agent_reasoning" => {
                if let Some(text) = text_at(&value, &[&["text"], &["reasoning"]]) {
                    events.push(BrainEvent::Reasoning {
                        text: text.to_string(),
                    });
                }
            }
            "tool.requested" | "function_call" | "exec_command_begin" => {
                let call_id = text_at(&value, &[&["call_id"], &["id"]])
                    .unwrap_or("unknown")
                    .to_string();
                let name = text_at(&value, &[&["name"], &["tool"], &["command"]])
                    .unwrap_or("unknown")
                    .to_string();
                let arguments = value
                    .get("arguments")
                    .or_else(|| value.get("params"))
                    .cloned()
                    .unwrap_or(Value::Null);
                events.push(BrainEvent::ToolRequested {
                    call_id,
                    name,
                    arguments,
                });
            }
            "tool.completed" | "function_call_output" | "exec_command_end" => {
                let call_id = text_at(&value, &[&["call_id"], &["id"]])
                    .unwrap_or("unknown")
                    .to_string();
                // Codex reports success as an exit code in some shapes and a
                // boolean in others; absent either, assume it worked, because
                // reporting a phantom failure is the worse mistake here.
                let ok = value
                    .get("success")
                    .and_then(Value::as_bool)
                    .or_else(|| value.get("exit_code").and_then(Value::as_i64).map(|c| c == 0))
                    .unwrap_or(true);
                events.push(BrainEvent::ToolResult {
                    call_id,
                    ok,
                    output: text_at(&value, &[&["output"], &["stdout"], &["result"]])
                        .map(str::to_string),
                });
            }
            "usage" | "token_count" => {
                events.push(BrainEvent::Usage(usage_from(&value)));
            }
            "turn.completed" | "task_complete" => {
                self.turn_open = false;
                // The current CLI reports a turn's cost with its end.
                if let Some(usage) = value.get("usage").filter(|usage| usage.is_object()) {
                    events.push(BrainEvent::Usage(usage_from(usage)));
                }
                events.push(BrainEvent::TurnEnded {
                    reason: StopReason::Completed,
                });
            }
            "thread.started" => {}
            // `error` is not the end of the turn. Codex 0.167 prints one per
            // retry ("Reconnecting... 2/5 (…)") and goes on; treating each as
            // a failed turn ended a turn ten times over on one bad request,
            // and would end a turn that a reconnect was about to save. Only
            // `turn.failed` ends it.
            "error" => {
                let message = text_at(&value, &[&["message"], &["error", "message"]])
                    .unwrap_or("codex reported an error");
                if !message.starts_with("Reconnecting...") {
                    self.errored = true;
                    events.push(BrainEvent::Error {
                        message: message.to_string(),
                    });
                }
            }
            "turn.failed" => {
                self.turn_open = false;
                self.errored = false;
                events.push(BrainEvent::Error {
                    message: text_at(&value, &[&["message"], &["error", "message"]])
                        .unwrap_or("codex reported an error")
                        .to_string(),
                });
                events.push(BrainEvent::TurnEnded {
                    reason: StopReason::Error,
                });
            }
            _ => {}
        }
        events
    }

    fn external_id(&self) -> Option<&str> {
        self.session.as_deref()
    }

    fn on_eof(&mut self) -> Vec<BrainEvent> {
        if self.turn_open {
            // The process died mid-turn. Saying so is the difference between
            // a task that recovers and one that waits forever. After an
            // error it died *of* that error, which is not resumable.
            self.turn_open = false;
            let reason = if std::mem::take(&mut self.errored) {
                StopReason::Error
            } else {
                StopReason::Interrupted
            };
            return vec![BrainEvent::TurnEnded { reason }];
        }
        Vec::new()
    }
}

/// Reads the Claude CLI's `--output-format stream-json` stream.
#[derive(Default)]
pub struct ClaudeAdapter {
    turn_open: bool,
    session: Option<String>,
}

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self::default()
    }
}

impl BrainAdapter for ClaudeAdapter {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn on_line(&mut self, line: &str) -> Vec<BrainEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return vec![BrainEvent::Error {
                message: format!("claude: unparsable line: {}", truncate(line)),
            }];
        };
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut events = Vec::new();

        match kind.as_str() {
            "system" => {
                if let Some(session) = text_at(&value, &[&["session_id"]]) {
                    self.session = Some(session.to_string());
                }
                // Claude opens with a system line carrying the session and
                // the model; that is the closest thing it has to "a turn
                // started".
                if value.get("subtype").and_then(Value::as_str) == Some("init") {
                    self.turn_open = true;
                    events.push(BrainEvent::TurnStarted {
                        model: text_at(&value, &[&["model"]]).map(str::to_string),
                    });
                }
            }
            "assistant" => {
                // The content is a list of blocks, and one message can carry
                // prose, thinking and tool calls at once — which is why
                // `on_line` returns a vector rather than one event.
                let blocks = value
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for block in blocks {
                    match block.get("type").and_then(Value::as_str).unwrap_or_default() {
                        "text" => {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                if !text.is_empty() {
                                    events.push(BrainEvent::Text {
                                        text: text.to_string(),
                                    });
                                }
                            }
                        }
                        "thinking" => {
                            if let Some(text) = text_at(&block, &[&["thinking"], &["text"]]) {
                                events.push(BrainEvent::Reasoning {
                                    text: text.to_string(),
                                });
                            }
                        }
                        "tool_use" => {
                            events.push(BrainEvent::ToolRequested {
                                call_id: block
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("unknown")
                                    .to_string(),
                                name: block
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("unknown")
                                    .to_string(),
                                arguments: block.get("input").cloned().unwrap_or(Value::Null),
                            });
                        }
                        _ => {}
                    }
                }
                // No usage here. Claude repeats a message's usage on every
                // line that carries one of its blocks, and reports the
                // session's total again with the result: counting these as
                // well added the same tokens several times over.
            }
            "user" => {
                // A tool result comes back as a user message holding
                // `tool_result` blocks.
                let blocks = value
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for block in blocks {
                    if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                        events.push(BrainEvent::ToolResult {
                            call_id: block
                                .get("tool_use_id")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown")
                                .to_string(),
                            ok: !block
                                .get("is_error")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            output: block
                                .get("content")
                                .map(|content| match content.as_str() {
                                    Some(text) => text.to_string(),
                                    None => content.to_string(),
                                }),
                        });
                    }
                }
            }
            "result" => {
                self.turn_open = false;
                if let Some(usage) = value.get("usage") {
                    events.push(BrainEvent::Usage(usage_from(usage)));
                }
                let errored = value
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let subtype = value.get("subtype").and_then(Value::as_str).unwrap_or("");
                if errored {
                    events.push(BrainEvent::Error {
                        message: text_at(&value, &[&["result"], &["error"]])
                            .unwrap_or("claude reported an error")
                            .to_string(),
                    });
                }
                events.push(BrainEvent::TurnEnded {
                    reason: if errored {
                        StopReason::Error
                    } else if subtype.contains("max_turns") || subtype.contains("limit") {
                        StopReason::Limit
                    } else {
                        StopReason::Completed
                    },
                });
            }
            _ => {}
        }
        events
    }

    fn external_id(&self) -> Option<&str> {
        self.session.as_deref()
    }

    fn on_eof(&mut self) -> Vec<BrainEvent> {
        if self.turn_open {
            self.turn_open = false;
            return vec![BrainEvent::TurnEnded {
                reason: StopReason::Interrupted,
            }];
        }
        Vec::new()
    }
}

/// Reads the Gemini CLI's `-p … --output-format stream-json` stream.
///
/// The wire format is `packages/core/src/output/types.ts` in gemini-cli
/// (checked against v0.67.0): `init`, `message`, `tool_use`, `tool_result`,
/// `error`, `result`, each one JSON object per line. One invocation is one
/// turn, however many tool round-trips it makes, and the `result` line closes
/// it -- the same shape as Claude's, which is why this reads much like that
/// adapter.
///
/// Three things the format does differently and this adapter has to absorb:
///
/// - **Assistant text arrives as deltas** (`delta: true`), not as finished
///   blocks. Each one becomes a `Text`; downstream already concatenates
///   `output.delta`, which is what the event is named for.
/// - **Errors come in two severities.** `warning` (a detected loop, say) is
///   not a failure and is not reported as one; an `Error` event would read as
///   "the turn failed" to everything downstream. `error` is.
/// - **Hitting the session-turn cap is an error line plus an error result**
///   (`MaxSessionTurnsError`). It is a limit, not a failure -- resumable,
///   exactly like Claude's `error_max_turns` -- so neither line produces an
///   `Error` and the turn ends as `Limit`.
#[derive(Default)]
pub struct GeminiAdapter {
    turn_open: bool,
    session: Option<String>,
    hit_turn_limit: bool,
}

impl GeminiAdapter {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Gemini's token stats, into the shared shape.
///
/// `input_tokens` there is the whole prompt, of which `cached` was a cache
/// read and `input` was fresh. `Usage` keeps the two apart, so the fresh
/// count goes in `input_tokens` -- falling back to subtracting the cache when
/// an older CLI leaves `input` out.
fn gemini_usage(stats: &Value) -> Usage {
    let prompt = u64_at(stats, &["input_tokens"]);
    let cached = u64_at(stats, &["cached"]);
    let fresh = match stats.get("input").and_then(Value::as_u64) {
        Some(fresh) => fresh,
        None => prompt.saturating_sub(cached),
    };
    Usage {
        input_tokens: fresh,
        output_tokens: u64_at(stats, &["output_tokens"]),
        cached_input_tokens: cached,
    }
}

impl BrainAdapter for GeminiAdapter {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn on_line(&mut self, line: &str) -> Vec<BrainEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return vec![BrainEvent::Error {
                message: format!("gemini: unparsable line: {}", truncate(line)),
            }];
        };
        let mut events = Vec::new();

        match value.get("type").and_then(Value::as_str).unwrap_or_default() {
            "init" => {
                if let Some(session) = text_at(&value, &[&["session_id"]]) {
                    self.session = Some(session.to_string());
                }
                self.turn_open = true;
                self.hit_turn_limit = false;
                events.push(BrainEvent::TurnStarted {
                    model: text_at(&value, &[&["model"]]).map(str::to_string),
                });
            }
            "message" => {
                // The prompt is echoed back as a user message; it is not
                // something the model said.
                if value.get("role").and_then(Value::as_str) == Some("assistant") {
                    if let Some(text) = text_at(&value, &[&["content"]]) {
                        if !text.is_empty() {
                            events.push(BrainEvent::Text {
                                text: text.to_string(),
                            });
                        }
                    }
                }
            }
            "tool_use" => {
                events.push(BrainEvent::ToolRequested {
                    call_id: text_at(&value, &[&["tool_id"]])
                        .unwrap_or("unknown")
                        .to_string(),
                    name: text_at(&value, &[&["tool_name"]])
                        .unwrap_or("unknown")
                        .to_string(),
                    arguments: value.get("parameters").cloned().unwrap_or(Value::Null),
                });
            }
            "tool_result" => {
                let ok = value.get("status").and_then(Value::as_str) == Some("success");
                events.push(BrainEvent::ToolResult {
                    call_id: text_at(&value, &[&["tool_id"]])
                        .unwrap_or("unknown")
                        .to_string(),
                    ok,
                    // A failed tool carries its reason in `error`, not
                    // `output`; dropping it would leave "failed" with no why.
                    output: text_at(&value, &[&["output"], &["error", "message"]])
                        .map(str::to_string),
                });
            }
            "error" => {
                let message = text_at(&value, &[&["message"]]).unwrap_or("gemini reported an error");
                if message.contains("Maximum session turns") {
                    self.hit_turn_limit = true;
                } else if value.get("severity").and_then(Value::as_str) != Some("warning") {
                    events.push(BrainEvent::Error {
                        message: message.to_string(),
                    });
                }
            }
            "result" => {
                self.turn_open = false;
                if let Some(stats) = value.get("stats") {
                    events.push(BrainEvent::Usage(gemini_usage(stats)));
                }
                let errored = value.get("status").and_then(Value::as_str) == Some("error");
                let limited = self.hit_turn_limit
                    || text_at(&value, &[&["error", "type"]])
                        .is_some_and(|kind| kind.contains("MaxSessionTurns"));
                self.hit_turn_limit = false;
                if errored && !limited {
                    events.push(BrainEvent::Error {
                        message: text_at(&value, &[&["error", "message"]])
                            .unwrap_or("gemini reported an error")
                            .to_string(),
                    });
                }
                events.push(BrainEvent::TurnEnded {
                    reason: if limited {
                        StopReason::Limit
                    } else if errored {
                        StopReason::Error
                    } else {
                        StopReason::Completed
                    },
                });
            }
            _ => {}
        }
        events
    }

    fn external_id(&self) -> Option<&str> {
        self.session.as_deref()
    }

    fn on_eof(&mut self) -> Vec<BrainEvent> {
        if self.turn_open {
            self.turn_open = false;
            return vec![BrainEvent::TurnEnded {
                reason: StopReason::Interrupted,
            }];
        }
        Vec::new()
    }
}

/// Reads `opencode run --format json`.
///
/// The wire format is `packages/opencode/src/cli/cmd/run.ts` plus the part
/// types in the SDK (checked against v1.19.5): every line is
/// `{type, timestamp, sessionID, part}` with `type` one of `step_start`,
/// `step_finish`, `text`, `reasoning`, `tool_use`, or `{type:"error", error}`.
///
/// It differs from the other three in ways that decide how this is written:
///
/// - **There is no end-of-turn line.** `run` stops when the session goes idle
///   and the process exits. The turn therefore ends at EOF, and *how* it
///   ended is read from the last `step_finish.reason`: `stop` finished,
///   `length` hit a limit, anything else (or nothing) means the stream was
///   cut off mid-turn.
/// - **A turn is several steps**, one per model call, each with its own
///   `step_finish` and tokens. They are added up and reported once at the
///   end, so a turn costs one `Usage` here as it does for every other CLI.
/// - **A tool is reported once, after it ran** (`state.status` completed or
///   error), carrying both its input and its outcome. One line becomes the
///   request and the result, in that order -- the same pair the others emit
///   from two lines.
/// - **No model name** appears anywhere in the stream; `TurnStarted` says so
///   by leaving it out rather than guessing.
/// - Reasoning only appears when `run` is given `--thinking`.
#[derive(Default)]
pub struct OpenCodeAdapter {
    turn_open: bool,
    session: Option<String>,
    usage: Usage,
    usage_seen: bool,
    last_finish: Option<String>,
    errored: bool,
}

impl OpenCodeAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    fn open_turn(&mut self, events: &mut Vec<BrainEvent>) {
        if !self.turn_open {
            self.turn_open = true;
            self.usage = Usage::default();
            self.usage_seen = false;
            self.last_finish = None;
            self.errored = false;
            events.push(BrainEvent::TurnStarted { model: None });
        }
    }
}

impl BrainAdapter for OpenCodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn on_line(&mut self, line: &str) -> Vec<BrainEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return vec![BrainEvent::Error {
                message: format!("opencode: unparsable line: {}", truncate(line)),
            }];
        };
        if let Some(session) = text_at(&value, &[&["sessionID"]]) {
            self.session = Some(session.to_string());
        }
        let part = value.get("part").cloned().unwrap_or(Value::Null);
        let mut events = Vec::new();

        match value.get("type").and_then(Value::as_str).unwrap_or_default() {
            "step_start" => self.open_turn(&mut events),
            "step_finish" => {
                self.open_turn(&mut events);
                if let Some(tokens) = part.get("tokens") {
                    self.usage_seen = true;
                    // Cache writes are fresh input, as for Claude.
                    self.usage.input_tokens += u64_at(tokens, &["input"])
                        + tokens
                            .get("cache")
                            .map(|cache| u64_at(cache, &["write"]))
                            .unwrap_or(0);
                    self.usage.output_tokens += u64_at(tokens, &["output"]);
                    self.usage.cached_input_tokens += tokens
                        .get("cache")
                        .map(|cache| u64_at(cache, &["read"]))
                        .unwrap_or(0);
                }
                self.last_finish = text_at(&part, &[&["reason"]]).map(str::to_string);
            }
            "text" => {
                self.open_turn(&mut events);
                if let Some(text) = text_at(&part, &[&["text"]]) {
                    if !text.is_empty() {
                        events.push(BrainEvent::Text {
                            text: text.to_string(),
                        });
                    }
                }
            }
            "reasoning" => {
                self.open_turn(&mut events);
                if let Some(text) = text_at(&part, &[&["text"]]) {
                    if !text.is_empty() {
                        events.push(BrainEvent::Reasoning {
                            text: text.to_string(),
                        });
                    }
                }
            }
            "tool_use" => {
                self.open_turn(&mut events);
                let call_id = text_at(&part, &[&["callID"], &["id"]])
                    .unwrap_or("unknown")
                    .to_string();
                let state = part.get("state").cloned().unwrap_or(Value::Null);
                events.push(BrainEvent::ToolRequested {
                    call_id: call_id.clone(),
                    name: text_at(&part, &[&["tool"]]).unwrap_or("unknown").to_string(),
                    arguments: state.get("input").cloned().unwrap_or(Value::Null),
                });
                let ok = state.get("status").and_then(Value::as_str) == Some("completed");
                events.push(BrainEvent::ToolResult {
                    call_id,
                    ok,
                    output: text_at(&state, &[&["output"], &["error"]]).map(str::to_string),
                });
            }
            "error" => {
                self.errored = true;
                let error = value.get("error").cloned().unwrap_or(Value::Null);
                events.push(BrainEvent::Error {
                    message: text_at(&error, &[&["data", "message"], &["message"], &["name"]])
                        .unwrap_or("opencode reported an error")
                        .to_string(),
                });
            }
            _ => {}
        }
        events
    }

    fn external_id(&self) -> Option<&str> {
        self.session.as_deref()
    }

    fn on_eof(&mut self) -> Vec<BrainEvent> {
        if !self.turn_open {
            return Vec::new();
        }
        self.turn_open = false;
        let mut events = Vec::new();
        if self.usage_seen {
            events.push(BrainEvent::Usage(self.usage));
        }
        let reason = if self.errored {
            StopReason::Error
        } else {
            match self.last_finish.as_deref() {
                Some("stop") | Some("end_turn") => StopReason::Completed,
                Some("length") | Some("max_tokens") => StopReason::Limit,
                // `tool-calls` with nothing after it, or no step ever
                // finishing: the process went away in the middle.
                _ => StopReason::Interrupted,
            }
        };
        events.push(BrainEvent::TurnEnded { reason });
        events
    }
}

/// Reads the Antigravity CLI: `agy -p … --output-format stream-json`.
///
/// Google's agent CLI, and a different wire format from `gemini` entirely
/// (checked against agy 1.5.3 and https://antigravity.google/docs/cli/headless/).
/// Three events: `init`, `step_update`, `result`, each with its payload under
/// the key of the same name.
///
/// - **Everything happens in `step_update`**, told apart by `step_type`:
///   `agent_response` carries `text_delta` (partial while `ACTIVE`, the rest
///   on `DONE`), `tool` carries `tool_info{name, parameters, output, error}`,
///   and `user_input` / `checkpoint` say nothing about the model.
/// - **A tool is reported once, on `DONE`**, input and outcome together, and
///   it has no call id: the step index is the only stable handle, so it
///   becomes one (`step_<index>`).
/// - **No reasoning text**, only a `thinking_tokens` count.
/// - **Usage is taken from `result`**, which is cumulative for the turn;
///   per-step usage would be the same tokens counted again. Its
///   `input_tokens` includes `cache_read_tokens`, so the fresh part is the
///   difference.
/// - **Failure is a `result`**, `status: ERROR` with an `error` string, and
///   can arrive with no `init` before it (an unauthenticated run prints only
///   that). There is no separate error event.
#[derive(Default)]
pub struct AgyAdapter {
    turn_open: bool,
    conversation: Option<String>,
    model: Option<String>,
}

impl AgyAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    fn open_turn(&mut self, events: &mut Vec<BrainEvent>) {
        if !self.turn_open {
            self.turn_open = true;
            events.push(BrainEvent::TurnStarted {
                model: self.model.clone(),
            });
        }
    }
}

impl BrainAdapter for AgyAdapter {
    fn id(&self) -> &'static str {
        "agy"
    }

    fn on_line(&mut self, line: &str) -> Vec<BrainEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return vec![BrainEvent::Error {
                message: format!("agy: unparsable line: {}", truncate(line)),
            }];
        };
        let kind = value.get("event").and_then(Value::as_str).unwrap_or_default();
        let payload = value.get(kind).cloned().unwrap_or(Value::Null);
        if let Some(id) = text_at(&value, &[&["conversation_id"]])
            .or_else(|| text_at(&payload, &[&["conversation_id"]]))
            .filter(|id| !id.is_empty())
        {
            self.conversation = Some(id.to_string());
        }
        let mut events = Vec::new();

        match kind {
            "init" => {
                self.model = text_at(&payload, &[&["model"]]).map(str::to_string);
                self.turn_open = false;
                self.open_turn(&mut events);
            }
            "step_update" => {
                let done = payload.get("state").and_then(Value::as_str) == Some("DONE");
                match payload.get("step_type").and_then(Value::as_str).unwrap_or_default() {
                    "agent_response" => {
                        if let Some(text) = text_at(&payload, &[&["text_delta"]]) {
                            if !text.is_empty() {
                                self.open_turn(&mut events);
                                events.push(BrainEvent::Text {
                                    text: text.to_string(),
                                });
                            }
                        }
                    }
                    "tool" if done => {
                        self.open_turn(&mut events);
                        let info = payload.get("tool_info").cloned().unwrap_or(Value::Null);
                        let call_id = format!(
                            "step_{}",
                            payload.get("step_index").and_then(Value::as_u64).unwrap_or(0)
                        );
                        events.push(BrainEvent::ToolRequested {
                            call_id: call_id.clone(),
                            name: text_at(&info, &[&["name"]])
                                .or_else(|| text_at(&payload, &[&["tool_name"]]))
                                .unwrap_or("unknown")
                                .to_string(),
                            arguments: info.get("parameters").cloned().unwrap_or(Value::Null),
                        });
                        let failed = info.get("error").is_some_and(|error| !error.is_null());
                        events.push(BrainEvent::ToolResult {
                            call_id,
                            ok: !failed,
                            output: text_at(&info, &[&["output"], &["error", "message"]])
                                .map(str::to_string),
                        });
                    }
                    _ => {}
                }
            }
            "result" => {
                self.turn_open = false;
                if let Some(usage) = payload.get("usage") {
                    let prompt = u64_at(usage, &["input_tokens"]);
                    let cached = u64_at(usage, &["cache_read_tokens"]);
                    events.push(BrainEvent::Usage(Usage {
                        input_tokens: prompt.saturating_sub(cached),
                        output_tokens: u64_at(usage, &["output_tokens"]),
                        cached_input_tokens: cached,
                    }));
                }
                let status = payload.get("status").and_then(Value::as_str).unwrap_or("SUCCESS");
                let finished = payload.get("finished_reason").and_then(Value::as_str);
                let reason = match (status, finished) {
                    (_, Some("print_timeout")) => StopReason::Limit,
                    ("SUCCESS", _) => StopReason::Completed,
                    ("CANCELED" | "INTERRUPTED", _) | (_, Some("interrupted")) => {
                        StopReason::Interrupted
                    }
                    _ => StopReason::Error,
                };
                if reason == StopReason::Error {
                    events.push(BrainEvent::Error {
                        message: text_at(&payload, &[&["error"], &["error", "message"]])
                            .filter(|message| !message.is_empty())
                            .unwrap_or("agy reported an error")
                            .to_string(),
                    });
                }
                events.push(BrainEvent::TurnEnded { reason });
            }
            _ => {}
        }
        events
    }

    fn external_id(&self) -> Option<&str> {
        self.conversation.as_deref()
    }

    fn on_eof(&mut self) -> Vec<BrainEvent> {
        if self.turn_open {
            self.turn_open = false;
            return vec![BrainEvent::TurnEnded {
                reason: StopReason::Interrupted,
            }];
        }
        Vec::new()
    }
}

/// Reads a CLI no adapter knows: each line of output is output, nothing
/// more is claimed about it.
///
/// What an unrecognised program used to get was the Codex adapter, which
/// read Claude's, Gemini's and Qwen's streams as malformed Codex -- every
/// event lost, every session reported as `codex`.
#[derive(Default)]
pub struct GenericAdapter;

impl GenericAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl BrainAdapter for GenericAdapter {
    fn id(&self) -> &'static str {
        "generic"
    }

    fn on_line(&mut self, line: &str) -> Vec<BrainEvent> {
        let line = line.trim_end();
        if line.trim().is_empty() {
            return Vec::new();
        }
        vec![BrainEvent::Text {
            text: line.to_string(),
        }]
    }
}

fn usage_from(value: &Value) -> Usage {
    Usage {
        // Claude reports tokens written to the prompt cache apart from
        // `input_tokens`. They are fresh input -- billed as such, at a
        // premium -- not cache reads, so they belong here. Left out, a live
        // `claude -p` turn reported 4 input tokens for a ~17k-token prompt.
        input_tokens: u64_at(value, &["input_tokens", "prompt_tokens", "input"])
            + u64_at(value, &["cache_creation_input_tokens"]),
        output_tokens: u64_at(value, &["output_tokens", "completion_tokens", "output"]),
        cached_input_tokens: u64_at(
            value,
            &[
                "cache_read_input_tokens",
                "cached_input_tokens",
                "cached_tokens",
            ],
        ),
    }
}

pub(crate) fn truncate(line: &str) -> String {
    // Enough to recognise the line, not enough to put a whole prompt — or a
    // secret pasted into one — in a log.
    let limit = 120;
    if line.chars().count() <= limit {
        return line.to_string();
    }
    let clipped: String = line.chars().take(limit).collect();
    format!("{clipped}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay;

    /// The same turn, as each CLI actually prints it: a greeting, a thought,
    /// one shell call and its result, the cost, and a clean finish.
    const CODEX_STREAM: &str = r#"
{"type":"turn.started","model":"gpt-5"}
{"type":"agent_reasoning","text":"I should list the directory"}
{"type":"function_call","call_id":"call_1","name":"shell","arguments":{"command":"ls"}}
{"type":"function_call_output","call_id":"call_1","exit_code":0,"output":"a\nb"}
{"type":"agent_message","text":"There are two files."}
{"type":"token_count","input_tokens":120,"output_tokens":30,"cached_tokens":100}
{"type":"turn.completed"}
"#;

    const CLAUDE_STREAM: &str = r#"
{"type":"system","subtype":"init","model":"gpt-5"}
{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"I should list the directory"},{"type":"tool_use","id":"call_1","name":"shell","input":{"command":"ls"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"call_1","is_error":false,"content":"a\nb"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"There are two files."}]}}
{"type":"result","subtype":"success","usage":{"input_tokens":120,"output_tokens":30,"cache_read_input_tokens":100}}
"#;

    /// The same turn as Gemini prints it. No thought line: Gemini's
    /// stream-json does not carry reasoning. Its `input_tokens` is the whole
    /// prompt (fresh 120 + cached 100); `input` is the fresh part.
    const GEMINI_STREAM: &str = r#"
{"type":"init","timestamp":"2026-11-09T02:10:01.000Z","session_id":"g1","model":"gpt-5"}
{"type":"message","timestamp":"2026-11-09T02:10:01.000Z","role":"user","content":"what is here?"}
{"type":"tool_use","timestamp":"2026-11-09T02:10:02.000Z","tool_name":"shell","tool_id":"call_1","parameters":{"command":"ls"}}
{"type":"tool_result","timestamp":"2026-11-09T02:10:02.000Z","tool_id":"call_1","status":"success","output":"a\nb"}
{"type":"message","timestamp":"2026-11-09T02:10:03.000Z","role":"assistant","content":"There are two files.","delta":true}
{"type":"result","timestamp":"2026-11-09T02:10:03.000Z","status":"success","stats":{"total_tokens":250,"input_tokens":220,"output_tokens":30,"cached":100,"input":120,"duration_ms":2000,"tool_calls":1,"models":{}}}
"#;

    fn gemini() -> Vec<BrainEvent> {
        replay(&mut GeminiAdapter::new(), GEMINI_STREAM)
    }

    /// The same turn as `opencode run --format json --thinking` prints it:
    /// two steps (the tool call, then the answer), the tool reported once
    /// after it ran, tokens split across the steps, and no closing line.
    const OPENCODE_STREAM: &str = r#"
{"type":"step_start","timestamp":1,"sessionID":"ses_1","part":{"id":"p1","sessionID":"ses_1","messageID":"m1","type":"step-start"}}
{"type":"reasoning","timestamp":2,"sessionID":"ses_1","part":{"id":"p2","type":"reasoning","text":"I should list the directory","time":{"start":1,"end":2}}}
{"type":"tool_use","timestamp":3,"sessionID":"ses_1","part":{"id":"p3","type":"tool","callID":"call_1","tool":"shell","state":{"status":"completed","input":{"command":"ls"},"output":"a\nb","title":"ls","metadata":{},"time":{"start":2,"end":3}}}}
{"type":"step_finish","timestamp":4,"sessionID":"ses_1","part":{"id":"p4","type":"step-finish","reason":"tool-calls","cost":0,"tokens":{"input":70,"output":10,"reasoning":0,"cache":{"read":60,"write":0}}}}
{"type":"step_start","timestamp":5,"sessionID":"ses_1","part":{"id":"p5","type":"step-start"}}
{"type":"text","timestamp":6,"sessionID":"ses_1","part":{"id":"p6","type":"text","text":"There are two files.","time":{"start":5,"end":6}}}
{"type":"step_finish","timestamp":7,"sessionID":"ses_1","part":{"id":"p7","type":"step-finish","reason":"stop","cost":0,"tokens":{"input":50,"output":20,"reasoning":0,"cache":{"read":40,"write":0}}}}
"#;

    fn opencode() -> Vec<BrainEvent> {
        replay(&mut OpenCodeAdapter::new(), OPENCODE_STREAM)
    }

    #[test]
    fn opencode_describes_the_same_turn_the_same_way() {
        // OpenCode never names the model, so that one field is set aside;
        // everything else -- including the tokens it splits across two
        // steps -- must come out as the other CLIs say it.
        let normalized = |events: Vec<BrainEvent>| {
            let mut described: Vec<String> = events
                .into_iter()
                .map(|event| match event {
                    BrainEvent::TurnStarted { .. } => BrainEvent::TurnStarted { model: None },
                    other => other,
                })
                .map(|event| serde_json::to_string(&event).unwrap())
                .collect();
            described.sort();
            described
        };
        assert_eq!(
            normalized(opencode()),
            normalized(replay(&mut ClaudeAdapter::new(), CLAUDE_STREAM))
        );
    }

    #[test]
    fn opencode_reports_a_tool_as_request_then_result() {
        let kinds: Vec<&str> = opencode().iter().map(BrainEvent::kind).collect();
        let request = kinds.iter().position(|k| *k == "tool_requested").unwrap();
        assert_eq!(kinds[request + 1], "tool_result", "{kinds:?}");
    }

    #[test]
    fn opencode_ends_the_turn_once_at_eof_with_one_usage() {
        let events = opencode();
        let kinds: Vec<&str> = events.iter().map(BrainEvent::kind).collect();
        assert_eq!(kinds.iter().filter(|k| **k == "turn_started").count(), 1, "{kinds:?}");
        assert_eq!(kinds.iter().filter(|k| **k == "usage").count(), 1, "{kinds:?}");
        assert_eq!(&kinds[kinds.len() - 2..], ["usage", "turn_ended"]);
    }

    #[test]
    fn opencode_learns_the_session_it_would_resume_by() {
        let mut adapter = OpenCodeAdapter::new();
        replay(&mut adapter, OPENCODE_STREAM);
        assert_eq!(adapter.external_id(), Some("ses_1"));
    }

    #[test]
    fn opencode_cut_off_after_a_tool_call_is_an_interruption() {
        // The last step asked for a tool and nothing followed: the process
        // went away mid-turn, which must not read as a clean finish.
        let events = replay(
            &mut OpenCodeAdapter::new(),
            r#"{"type":"step_start","sessionID":"s","part":{"type":"step-start"}}
{"type":"step_finish","sessionID":"s","part":{"type":"step-finish","reason":"tool-calls","tokens":{"input":1,"output":1,"reasoning":0,"cache":{"read":0,"write":0}}}}"#,
        );
        assert_eq!(
            events.last(),
            Some(&BrainEvent::TurnEnded { reason: StopReason::Interrupted })
        );
    }

    #[test]
    fn opencode_running_out_of_length_is_a_limit() {
        let events = replay(
            &mut OpenCodeAdapter::new(),
            r#"{"type":"step_finish","sessionID":"s","part":{"type":"step-finish","reason":"length","tokens":{"input":1,"output":1,"reasoning":0,"cache":{"read":0,"write":0}}}}"#,
        );
        assert_eq!(events.last(), Some(&BrainEvent::TurnEnded { reason: StopReason::Limit }));
    }

    #[test]
    fn opencode_session_error_ends_the_turn_as_an_error() {
        let events = replay(
            &mut OpenCodeAdapter::new(),
            r#"{"type":"step_start","sessionID":"s","part":{"type":"step-start"}}
{"type":"error","sessionID":"s","error":{"name":"ProviderAuthError","data":{"message":"no api key"}}}"#,
        );
        assert!(events.contains(&BrainEvent::Error { message: "no api key".into() }), "{events:?}");
        assert_eq!(events.last(), Some(&BrainEvent::TurnEnded { reason: StopReason::Error }));
    }

    #[test]
    fn opencode_failed_tool_keeps_its_reason() {
        let events = replay(
            &mut OpenCodeAdapter::new(),
            r#"{"type":"tool_use","sessionID":"s","part":{"type":"tool","callID":"c1","tool":"read","state":{"status":"error","input":{"path":"x"},"error":"nope","time":{"start":1,"end":2}}}}"#,
        );
        assert!(events.contains(&BrainEvent::ToolResult {
            call_id: "c1".into(),
            ok: false,
            output: Some("nope".into()),
        }), "{events:?}");
    }

    #[test]
    fn claude_cache_writes_count_as_input_not_as_nothing() {
        // Shape taken from a live `claude -p --output-format stream-json` run.
        let events = replay(
            &mut ClaudeAdapter::new(),
            r#"{"type":"system","subtype":"init","model":"m"}
{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":2,"cache_creation_input_tokens":17360,"cache_read_input_tokens":14517,"output_tokens":4}}"#,
        );
        assert!(events.contains(&BrainEvent::Usage(Usage {
            input_tokens: 17362,
            output_tokens: 4,
            cached_input_tokens: 14517,
        })), "{events:?}");
    }

    /// Shape recorded from a real `codex exec --json` 0.167 run without
    /// credentials (URL and config path trimmed): retries, a fallback
    /// notice, then one `turn.failed`.
    const CODEX_UNAUTHORIZED: &str = r#"
{"type":"thread.started","thread_id":"01a1c04c-b5a9-7750-b3ee-acc8ea5f0dda"}
{"type":"turn.started"}
{"type":"error","message":"Reconnecting... 2/5 (unexpected status 401 Unauthorized: Missing bearer or basic authentication in header)"}
{"type":"error","message":"Reconnecting... 5/5 (unexpected status 401 Unauthorized: Missing bearer or basic authentication in header)"}
{"type":"item.completed","item":{"id":"item_2","type":"error","message":"Falling back from WebSockets to HTTPS transport. unexpected status 401 Unauthorized"}}
{"type":"error","message":"Reconnecting... 1/5 (unexpected status 401 Unauthorized: Missing bearer or basic authentication in header)"}
{"type":"error","message":"unexpected status 401 Unauthorized: Missing bearer or basic authentication in header"}
{"type":"turn.failed","error":{"message":"unexpected status 401 Unauthorized: Missing bearer or basic authentication in header"}}
"#;

    #[test]
    fn codex_retries_do_not_end_the_turn_turn_failed_does() {
        let events = replay(&mut CodexAdapter::new(), CODEX_UNAUTHORIZED);
        let ended: Vec<&BrainEvent> = events
            .iter()
            .filter(|e| matches!(e, BrainEvent::TurnEnded { .. }))
            .collect();
        assert_eq!(ended, [&BrainEvent::TurnEnded { reason: StopReason::Error }], "{events:#?}");
        assert_eq!(events.last(), Some(&BrainEvent::TurnEnded { reason: StopReason::Error }));
        // Retry notices are not errors; the real failure is reported.
        let errors: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                BrainEvent::Error { message } => Some(message.as_str()),
                _ => None,
            })
            .collect();
        assert!(errors.iter().all(|m| !m.starts_with("Reconnecting")), "{errors:?}");
        assert!(errors.iter().any(|m| m.contains("401")), "{errors:?}");
    }

    #[test]
    fn codex_turn_that_recovers_after_a_retry_is_not_failed() {
        let events = replay(
            &mut CodexAdapter::new(),
            r#"{"type":"turn.started"}
{"type":"error","message":"Reconnecting... 1/5 (stream disconnected)"}
{"type":"item.completed","item":{"id":"i","type":"agent_message","text":"done"}}
{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}"#,
        );
        assert!(!events.iter().any(|e| matches!(e, BrainEvent::Error { .. })), "{events:?}");
        assert_eq!(events.last(), Some(&BrainEvent::TurnEnded { reason: StopReason::Completed }));
    }

    #[test]
    fn codex_dying_after_an_error_ended_in_error() {
        let events = replay(
            &mut CodexAdapter::new(),
            r#"{"type":"turn.started"}
{"type":"error","message":"quota exceeded"}"#,
        );
        assert_eq!(events.last(), Some(&BrainEvent::TurnEnded { reason: StopReason::Error }));
    }

    #[test]
    fn opencode_cache_writes_count_as_input() {
        let events = replay(
            &mut OpenCodeAdapter::new(),
            r#"{"type":"step_finish","sessionID":"s","part":{"type":"step-finish","reason":"stop","tokens":{"input":10,"output":2,"reasoning":0,"cache":{"read":5,"write":7}}}}"#,
        );
        assert!(events.contains(&BrainEvent::Usage(Usage { input_tokens: 17, output_tokens: 2, cached_input_tokens: 5 })), "{events:?}");
    }

    /// The same turn as `agy -p … --output-format stream-json` prints it.
    /// No reasoning text (agy only counts thinking tokens) and no tool call
    /// id; `input_tokens` includes the 100 cached.
    const AGY_STREAM: &str = r#"
{"event":"init","conversation_id":"c3b6","init":{"cwd":"/w","tools":["run_command"],"permission_mode":"always-proceed","model":"gpt-5"}}
{"event":"step_update","step_update":{"conversation_id":"c3b6","step_index":0,"state":"DONE","step_type":"user_input"}}
{"event":"step_update","step_update":{"conversation_id":"c3b6","step_index":1,"state":"ACTIVE","step_type":"tool","tool_name":"shell"}}
{"event":"step_update","step_update":{"conversation_id":"c3b6","step_index":1,"state":"DONE","step_type":"tool","tool_name":"shell","duration_seconds":0.07,"tool_info":{"name":"shell","parameters":{"command":"ls"},"output":"a\nb"}}}
{"event":"step_update","step_update":{"conversation_id":"c3b6","step_index":2,"state":"ACTIVE","step_type":"agent_response","text_delta":"There are "}}
{"event":"step_update","step_update":{"conversation_id":"c3b6","step_index":2,"state":"DONE","step_type":"agent_response","text_delta":"two files.","usage":{"input_tokens":220,"output_tokens":30,"thinking_tokens":5,"cache_read_tokens":100,"total_tokens":250}}}
{"event":"step_update","step_update":{"conversation_id":"c3b6","step_index":3,"state":"DONE","step_type":"checkpoint","usage":{"input_tokens":9,"output_tokens":1,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":10}}}
{"event":"result","result":{"conversation_id":"c3b6","status":"SUCCESS","response":"There are two files.","duration_seconds":2.1,"num_turns":1,"usage":{"input_tokens":220,"output_tokens":30,"thinking_tokens":5,"cache_read_tokens":100,"total_tokens":250}}}
"#;

    fn agy() -> Vec<BrainEvent> {
        replay(&mut AgyAdapter::new(), AGY_STREAM)
    }

    #[test]
    fn agy_describes_the_same_turn_as_the_others() {
        // agy has no reasoning text and no tool call ids; set those aside
        // and the turn must read as Claude's does. Its text arrives in two
        // deltas, which concatenate to Claude's one block.
        let normalize = |events: Vec<BrainEvent>| {
            let mut text = String::new();
            let mut rest: Vec<String> = Vec::new();
            for event in events {
                match event {
                    BrainEvent::Reasoning { .. } => {}
                    BrainEvent::Text { text: t } => text.push_str(&t),
                    BrainEvent::ToolRequested { name, arguments, .. } => rest.push(
                        serde_json::to_string(&BrainEvent::ToolRequested { call_id: String::new(), name, arguments }).unwrap(),
                    ),
                    BrainEvent::ToolResult { ok, output, .. } => rest.push(
                        serde_json::to_string(&BrainEvent::ToolResult { call_id: String::new(), ok, output }).unwrap(),
                    ),
                    other => rest.push(serde_json::to_string(&other).unwrap()),
                }
            }
            rest.sort();
            (text, rest)
        };
        assert_eq!(normalize(agy()), normalize(replay(&mut ClaudeAdapter::new(), CLAUDE_STREAM)));
    }

    #[test]
    fn agy_tool_request_and_result_share_the_step_handle() {
        let events = agy();
        let ids: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                BrainEvent::ToolRequested { call_id, .. } | BrainEvent::ToolResult { call_id, .. } => Some(call_id.as_str()),
                _ => None,
            })
            .collect();
        // One pair, from the DONE line only -- the ACTIVE line is not a
        // second request.
        assert_eq!(ids, ["step_1", "step_1"]);
    }

    #[test]
    fn agy_counts_the_turn_once_from_the_result() {
        let usage: Vec<Usage> = agy()
            .into_iter()
            .filter_map(|e| match e {
                BrainEvent::Usage(u) => Some(u),
                _ => None,
            })
            .collect();
        assert_eq!(usage, [Usage { input_tokens: 120, output_tokens: 30, cached_input_tokens: 100 }]);
    }

    #[test]
    fn agy_learns_the_conversation_it_would_resume_by() {
        let mut adapter = AgyAdapter::new();
        replay(&mut adapter, AGY_STREAM);
        assert_eq!(adapter.external_id(), Some("c3b6"));
    }

    #[test]
    fn agy_unauthenticated_run_is_an_error_not_silence() {
        // Verbatim from agy 1.5.3 with no cached login: one result line, no
        // init, an empty conversation id.
        let mut adapter = AgyAdapter::new();
        let events = replay(
            &mut adapter,
            r#"{"event":"result","result":{"conversation_id":"","status":"ERROR","response":"","error":"authentication failed or timed out","duration_seconds":0,"num_turns":0,"usage":{"input_tokens":0,"output_tokens":0,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":0},"finished_reason":"error"}}"#,
        );
        assert!(events.contains(&BrainEvent::Error { message: "authentication failed or timed out".into() }), "{events:?}");
        assert_eq!(events.last(), Some(&BrainEvent::TurnEnded { reason: StopReason::Error }));
        assert_eq!(adapter.external_id(), None, "an empty id is no id");
    }

    #[test]
    fn agy_print_timeout_is_a_limit_and_cancel_an_interruption() {
        let end = |line: &str| replay(&mut AgyAdapter::new(), line).pop();
        assert_eq!(
            end(r#"{"event":"result","result":{"status":"ERROR","error":"timed out","finished_reason":"print_timeout"}}"#),
            Some(BrainEvent::TurnEnded { reason: StopReason::Limit })
        );
        assert_eq!(
            end(r#"{"event":"result","result":{"status":"CANCELED"}}"#),
            Some(BrainEvent::TurnEnded { reason: StopReason::Interrupted })
        );
    }

    #[test]
    fn agy_failed_tool_keeps_its_reason() {
        let events = replay(
            &mut AgyAdapter::new(),
            r#"{"event":"step_update","step_update":{"step_index":4,"state":"DONE","step_type":"tool","tool_name":"view_file","tool_info":{"name":"view_file","parameters":{"path":"x"},"error":{"type":"NOT_FOUND","message":"nope"}}}}"#,
        );
        assert!(events.contains(&BrainEvent::ToolResult { call_id: "step_4".into(), ok: false, output: Some("nope".into()) }), "{events:?}");
    }

    #[test]
    fn agy_cut_off_mid_turn_is_an_interruption() {
        let events = replay(&mut AgyAdapter::new(), r#"{"event":"init","conversation_id":"c","init":{}}"#);
        assert_eq!(events.last(), Some(&BrainEvent::TurnEnded { reason: StopReason::Interrupted }));
    }

    #[test]
    fn opencode_silence_is_not_a_turn() {
        assert!(replay(&mut OpenCodeAdapter::new(), "").is_empty());
    }

    #[test]
    fn the_two_adapters_describe_the_same_turn_the_same_way() {
        // M4's gate. Two wire formats that agree about almost nothing above
        // "it is JSON" must come out identical, or everything downstream ends
        // up branching on which CLI ran.
        let codex = replay(&mut CodexAdapter::new(), CODEX_STREAM);
        let claude = replay(&mut ClaudeAdapter::new(), CLAUDE_STREAM);

        // Ordering differs between the CLIs — Claude reports usage with the
        // result, Codex just before it — so compare as multisets of what
        // happened, which is the isomorphism that actually matters.
        let sorted = |events: &[BrainEvent]| {
            let mut described: Vec<String> = events
                .iter()
                .map(|event| serde_json::to_string(event).unwrap())
                .collect();
            described.sort();
            described
        };
        assert_eq!(
            sorted(&codex),
            sorted(&claude),
            "\ncodex:  {codex:#?}\nclaude: {claude:#?}"
        );
    }

    #[test]
    fn gemini_describes_the_same_turn_except_the_reasoning_it_never_prints() {
        // The third wire format, held to the same gate. Gemini's stream has
        // no thinking in it, so the comparison drops reasoning from Claude's
        // side rather than pretending Gemini said something it did not.
        let without_reasoning = |events: Vec<BrainEvent>| {
            let mut described: Vec<String> = events
                .into_iter()
                .filter(|event| !matches!(event, BrainEvent::Reasoning { .. }))
                .map(|event| serde_json::to_string(&event).unwrap())
                .collect();
            described.sort();
            described
        };
        let claude = without_reasoning(replay(&mut ClaudeAdapter::new(), CLAUDE_STREAM));
        let gemini = without_reasoning(gemini());
        assert_eq!(gemini, claude);
    }

    #[test]
    fn gemini_echoed_prompt_is_not_model_output() {
        let texts: Vec<String> = gemini()
            .into_iter()
            .filter_map(|event| match event {
                BrainEvent::Text { text } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["There are two files."]);
    }

    #[test]
    fn gemini_learns_the_session_it_would_resume_by() {
        let mut adapter = GeminiAdapter::new();
        replay(&mut adapter, GEMINI_STREAM);
        assert_eq!(adapter.external_id(), Some("g1"));
    }

    #[test]
    fn gemini_warning_is_not_a_failure() {
        // A detected loop is reported as a warning and the run goes on; an
        // Error here would make downstream mark a working turn as failed.
        let events = replay(
            &mut GeminiAdapter::new(),
            r#"{"type":"init","session_id":"g","model":"m"}
{"type":"error","severity":"warning","message":"Loop detected, stopping execution"}
{"type":"result","status":"success"}"#,
        );
        assert!(!events.iter().any(|e| matches!(e, BrainEvent::Error { .. })), "{events:?}");
        assert_eq!(
            events.last(),
            Some(&BrainEvent::TurnEnded { reason: StopReason::Completed })
        );
    }

    #[test]
    fn gemini_error_result_ends_the_turn_as_an_error() {
        let events = replay(
            &mut GeminiAdapter::new(),
            r#"{"type":"init","session_id":"g","model":"m"}
{"type":"error","severity":"error","message":"quota exhausted"}
{"type":"result","status":"error","error":{"type":"FatalAuthenticationError","message":"quota exhausted"}}"#,
        );
        assert!(events.iter().any(|e| matches!(e, BrainEvent::Error { .. })));
        assert_eq!(
            events.last(),
            Some(&BrainEvent::TurnEnded { reason: StopReason::Error })
        );
    }

    #[test]
    fn gemini_hitting_the_turn_cap_is_a_limit_not_a_failure() {
        let events = replay(
            &mut GeminiAdapter::new(),
            r#"{"type":"init","session_id":"g","model":"m"}
{"type":"error","severity":"error","message":"Maximum session turns exceeded"}
{"type":"result","status":"error","error":{"type":"MaxSessionTurnsError","message":"Maximum session turns exceeded"}}"#,
        );
        assert!(!events.iter().any(|e| matches!(e, BrainEvent::Error { .. })), "{events:?}");
        assert_eq!(
            events.last(),
            Some(&BrainEvent::TurnEnded { reason: StopReason::Limit })
        );
    }

    #[test]
    fn gemini_failed_tool_keeps_its_reason() {
        let events = replay(
            &mut GeminiAdapter::new(),
            r#"{"type":"tool_result","tool_id":"c1","status":"error","error":{"type":"FILE_NOT_FOUND","message":"nope"}}"#,
        );
        assert_eq!(
            events,
            vec![BrainEvent::ToolResult {
                call_id: "c1".into(),
                ok: false,
                output: Some("nope".into()),
            }]
        );
    }

    #[test]
    fn gemini_usage_falls_back_when_the_fresh_count_is_missing() {
        let usage = gemini_usage(&serde_json::json!({"input_tokens": 220, "cached": 100, "output_tokens": 30}));
        assert_eq!((usage.input_tokens, usage.cached_input_tokens, usage.output_tokens), (120, 100, 30));
    }

    #[test]
    fn both_report_the_pieces_that_matter() {
        for (name, events) in [
            ("codex", replay(&mut CodexAdapter::new(), CODEX_STREAM)),
            ("claude", replay(&mut ClaudeAdapter::new(), CLAUDE_STREAM)),
        ] {
            let kinds: Vec<&str> = events.iter().map(BrainEvent::kind).collect();
            for expected in [
                "turn_started",
                "reasoning",
                "tool_requested",
                "tool_result",
                "text",
                "usage",
                "turn_ended",
            ] {
                assert!(
                    kinds.contains(&expected),
                    "{name} never reported {expected}: {kinds:?}"
                );
            }
            let usage = events
                .iter()
                .find_map(|event| match event {
                    BrainEvent::Usage(usage) => Some(*usage),
                    _ => None,
                })
                .expect("usage");
            assert_eq!(usage.input_tokens, 120);
            assert_eq!(usage.output_tokens, 30);
            assert_eq!(
                usage.cached_input_tokens, 100,
                "{name} folded cached reads into fresh input"
            );
        }
    }

    /// Issue #36: what `claude -p --output-format stream-json --verbose`
    /// prints today -- usage repeated on every assistant line and totalled
    /// again in the result -- is read as events, and the tokens are counted
    /// once.
    #[test]
    fn claude_stream_json_is_read_and_counted_once() {
        let stream = r#"
{"type":"system","subtype":"init","session_id":"s1","model":"claude-sonnet-5","tools":["Bash"]}
{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Writing rmb.py"}],"usage":{"input_tokens":10,"cache_read_input_tokens":500,"output_tokens":3}}}
{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"rmb.py"}}],"usage":{"input_tokens":10,"cache_read_input_tokens":500,"output_tokens":40}}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"File created"}]}}
{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":25,"cache_read_input_tokens":1000,"output_tokens":90},"total_cost_usd":0.01}
"#;
        let events = replay(&mut ClaudeAdapter::new(), stream);
        let kinds: Vec<&str> = events.iter().map(|event| event.kind()).collect();
        assert_eq!(
            kinds,
            ["turn_started", "text", "tool_requested", "tool_result", "usage", "turn_ended"]
        );
        let usage: Vec<&Usage> = events
            .iter()
            .filter_map(|event| match event {
                BrainEvent::Usage(usage) => Some(usage),
                _ => None,
            })
            .collect();
        assert_eq!(usage.len(), 1);
        assert_eq!(
            (usage[0].input_tokens, usage[0].cached_input_tokens, usage[0].output_tokens),
            (25, 1000, 90)
        );
    }

    /// Issue #36, Codex half: `codex exec --json` reports work as items and
    /// the turn's tokens with `turn.completed`.
    #[test]
    fn codex_exec_json_items_and_usage_are_read() {
        let stream = r#"
{"type":"thread.started","thread_id":"th_1"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"reasoning","text":"list files"}}
{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","aggregated_output":"a\nb","exit_code":0,"status":"completed"}}
{"type":"item.completed","item":{"id":"item_2","type":"file_change","changes":[{"path":"rmb.py","kind":"add"}],"status":"completed"}}
{"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":"Done."}}
{"type":"turn.completed","usage":{"input_tokens":24763,"cached_input_tokens":24448,"output_tokens":122}}
"#;
        let mut adapter = CodexAdapter::new();
        let events = replay(&mut adapter, stream);
        let kinds: Vec<&str> = events.iter().map(|event| event.kind()).collect();
        assert_eq!(
            kinds,
            [
                "turn_started",
                "reasoning",
                "tool_requested",
                "tool_result",
                "tool_requested",
                "tool_result",
                "text",
                "usage",
                "turn_ended"
            ]
        );
        assert!(events.iter().any(|event| matches!(
            event,
            BrainEvent::Usage(Usage { input_tokens: 24763, cached_input_tokens: 24448, output_tokens: 122 })
        )));
        assert_eq!(adapter.external_id(), Some("th_1"));
    }

    #[test]
    fn an_unknown_cli_still_reports_its_output() {
        let events = replay(&mut GenericAdapter::new(), "hello\n\n{\"x\":1}\n");
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| event.kind() == "text"));
    }

    #[test]
    fn a_tool_call_keeps_its_correlation_id() {
        // Without this the runtime cannot tell which result belongs to which
        // request, and a second call would look like a repeat of the first.
        for (name, events) in [
            ("codex", replay(&mut CodexAdapter::new(), CODEX_STREAM)),
            ("claude", replay(&mut ClaudeAdapter::new(), CLAUDE_STREAM)),
            ("gemini", gemini()),
            ("opencode", opencode()),
        ] {
            let requested = events.iter().find_map(|event| match event {
                BrainEvent::ToolRequested { call_id, name, .. } => {
                    Some((call_id.clone(), name.clone()))
                }
                _ => None,
            });
            let result = events.iter().find_map(|event| match event {
                BrainEvent::ToolResult { call_id, ok, .. } => Some((call_id.clone(), *ok)),
                _ => None,
            });
            let (request_id, tool) = requested.unwrap_or_else(|| panic!("{name}: no request"));
            let (result_id, ok) = result.unwrap_or_else(|| panic!("{name}: no result"));
            assert_eq!(tool, "shell");
            assert_eq!(request_id, result_id, "{name} lost the correlation");
            assert!(ok);
        }
    }

    #[test]
    fn a_stream_that_dies_mid_turn_says_the_turn_was_interrupted() {
        // A task waiting on a turn that will never end is the failure this
        // prevents; the runtime needs to be told, not left guessing.
        for (name, adapter, stream) in [
            (
                "codex",
                Box::new(CodexAdapter::new()) as Box<dyn BrainAdapter>,
                "{\"type\":\"turn.started\",\"model\":\"m\"}",
            ),
            (
                "claude",
                Box::new(ClaudeAdapter::new()),
                "{\"type\":\"system\",\"subtype\":\"init\",\"model\":\"m\"}",
            ),
            (
                "gemini",
                Box::new(GeminiAdapter::new()),
                "{\"type\":\"init\",\"session_id\":\"g\",\"model\":\"m\"}",
            ),
        ] {
            let mut adapter = adapter;
            let events = replay(adapter.as_mut(), stream);
            assert_eq!(
                events.last(),
                Some(&BrainEvent::TurnEnded {
                    reason: StopReason::Interrupted
                }),
                "{name} left the turn open"
            );
        }
    }

    #[test]
    fn a_clean_stream_does_not_invent_an_interruption() {
        for (name, events) in [
            ("codex", replay(&mut CodexAdapter::new(), CODEX_STREAM)),
            ("claude", replay(&mut ClaudeAdapter::new(), CLAUDE_STREAM)),
            ("gemini", gemini()),
            ("opencode", opencode()),
        ] {
            let interruptions = events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        BrainEvent::TurnEnded {
                            reason: StopReason::Interrupted
                        }
                    )
                })
                .count();
            assert_eq!(interruptions, 0, "{name} reported a phantom interruption");
        }
    }

    #[test]
    fn a_line_that_is_not_json_is_reported_rather_than_swallowed() {
        // A CLI that starts printing warnings to the same stream must not
        // look like a quiet one.
        for (name, events) in [
            ("codex", replay(&mut CodexAdapter::new(), "warning: rate limited")),
            (
                "claude",
                replay(&mut ClaudeAdapter::new(), "warning: rate limited"),
            ),
            (
                "gemini",
                replay(&mut GeminiAdapter::new(), "warning: rate limited"),
            ),
            (
                "opencode",
                replay(&mut OpenCodeAdapter::new(), "warning: rate limited"),
            ),
            ("agy", replay(&mut AgyAdapter::new(), "warning: rate limited")),
        ] {
            assert!(
                matches!(events.first(), Some(BrainEvent::Error { .. })),
                "{name} swallowed a line it could not parse: {events:?}"
            );
        }
    }

    #[test]
    fn an_error_result_ends_the_turn_as_an_error() {
        let events = replay(
            &mut ClaudeAdapter::new(),
            r#"{"type":"system","subtype":"init","model":"m"}
{"type":"result","subtype":"error","is_error":true,"result":"it broke"}"#,
        );
        assert!(events.iter().any(|e| matches!(e, BrainEvent::Error { .. })));
        assert_eq!(
            events.last(),
            Some(&BrainEvent::TurnEnded {
                reason: StopReason::Error
            })
        );
    }

    #[test]
    fn hitting_a_limit_is_not_the_same_as_failing() {
        // A turn stopped by a cap can be resumed; one that errored usually
        // cannot, and the runtime decides differently on each.
        let events = replay(
            &mut ClaudeAdapter::new(),
            r#"{"type":"system","subtype":"init","model":"m"}
{"type":"result","subtype":"error_max_turns"}"#,
        );
        assert_eq!(
            events.last(),
            Some(&BrainEvent::TurnEnded {
                reason: StopReason::Limit
            })
        );
    }

    #[test]
    fn a_failed_tool_is_reported_as_failed() {
        let codex = replay(
            &mut CodexAdapter::new(),
            r#"{"type":"function_call_output","call_id":"c1","exit_code":1,"output":"nope"}"#,
        );
        assert_eq!(
            codex,
            vec![BrainEvent::ToolResult {
                call_id: "c1".into(),
                ok: false,
                output: Some("nope".into()),
            }]
        );
        let claude = replay(
            &mut ClaudeAdapter::new(),
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"c1","is_error":true,"content":"nope"}]}}"#,
        );
        assert_eq!(
            claude,
            vec![BrainEvent::ToolResult {
                call_id: "c1".into(),
                ok: false,
                output: Some("nope".into()),
            }]
        );
    }

    #[test]
    fn one_message_carrying_several_things_yields_several_events() {
        // Claude packs prose, thinking and a tool call into one line; an
        // adapter returning a single event would drop two of them.
        let events = replay(
            &mut ClaudeAdapter::new(),
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"ok"},{"type":"tool_use","id":"c","name":"shell","input":{}}]}}"#,
        );
        let kinds: Vec<&str> = events.iter().map(BrainEvent::kind).collect();
        assert_eq!(kinds, ["reasoning", "text", "tool_requested"]);
    }

    #[test]
    fn both_learn_the_session_id_the_cli_would_resume_by() {
        let mut codex = CodexAdapter::new();
        replay(&mut codex, r#"{"type":"turn.started","model":"m","session_id":"sess_9"}"#);
        assert_eq!(codex.external_id(), Some("sess_9"));

        let mut claude = ClaudeAdapter::new();
        replay(
            &mut claude,
            r#"{"type":"system","subtype":"init","model":"m","session_id":"sess_9"}"#,
        );
        assert_eq!(claude.external_id(), Some("sess_9"));

        // And an adapter that never saw one says so, rather than inventing an
        // id that would resume the wrong conversation.
        assert_eq!(CodexAdapter::new().external_id(), None);
    }

    #[test]
    fn adapters_perform_nothing() {
        // The contract the equivalence test rests on: feeding a tool request
        // through an adapter must produce a *request*, never a call. There is
        // no I/O in this crate to make one with, and this test is here so that
        // stays true by intent rather than by accident.
        let events = replay(
            &mut CodexAdapter::new(),
            r#"{"type":"function_call","call_id":"c","name":"shell","arguments":{"command":"rm -rf /"}}"#,
        );
        assert_eq!(events.len(), 1);
        assert!(events[0].is_tool_request());
    }
}
