//! The two CLIs, read into one vocabulary.
//!
//! Both Codex and Claude emit JSON Lines, and they disagree about almost
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
            "turn.failed" | "error" => {
                self.turn_open = false;
                events.push(BrainEvent::Error {
                    message: text_at(&value, &[&["message"], &["error"], &["error", "message"]])
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
            // a task that recovers and one that waits forever.
            self.turn_open = false;
            return vec![BrainEvent::TurnEnded {
                reason: StopReason::Interrupted,
            }];
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
        input_tokens: u64_at(value, &["input_tokens", "prompt_tokens", "input"]),
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
