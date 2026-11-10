//! Agent Client Protocol (ACP): JSON-RPC 2.0 over stdio.
//!
//! One protocol, several CLIs (`opencode acp`, `gemini --acp`, ...). Unlike
//! Claude's stream-json it is request/response: the host sends `initialize`
//! and `session/new`, and only once the agent has answered with a
//! `sessionId` can a `session/prompt` be sent. The agent pushes progress as
//! `session/update` notifications, asks before a tool with a
//! `session/request_permission` *request* (answered with one of the
//! `optionId`s it offered), and ends a turn by answering the prompt with a
//! `stopReason`.
//!
//! Two halves, kept apart like the rest of the crate: [`AcpAdapter`] reads
//! (pure, a parser like any other adapter) and [`AcpHost`] writes. The host
//! has to remember what the agent said -- the session id, the options each
//! question offered, the JSON type of each request id -- so it watches the
//! raw stream through [`HostProtocol::observe`].
//!
//! Shapes verified against OpenCode 1.19.5 (`opencode acp`).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::host::{Decision, HostProtocol};
use crate::{BrainAdapter, BrainEvent, StopReason, Usage};

/// The request id the agent used, as the string [`BrainEvent::PermissionRequested`]
/// carries. The original JSON value is kept by the host for the reply.
fn id_key(id: &Value) -> String {
    match id {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// A tool call's input: `rawInput` (the ACP field), else agy's `args`.
fn input_of(call: &Value) -> Value {
    call.get("rawInput")
        .or_else(|| call.get("args"))
        .filter(|input| !input.is_null())
        .cloned()
        .unwrap_or(Value::Null)
}

/// Reads an ACP agent's stdout.
pub struct AcpAdapter {
    id: &'static str,
    model: Option<String>,
    in_turn: bool,
    /// Tool calls seen but not yet reported: name and the latest arguments.
    /// `tool_call` arrives before the arguments are complete (live: only
    /// `cwd`), so the request is reported when the call starts running, is
    /// asked about, or finishes -- whichever comes first.
    calls: HashMap<String, (String, Value)>,
    reported: HashSet<String>,
}

impl AcpAdapter {
    pub fn new(id: &'static str) -> Self {
        Self {
            id,
            model: None,
            in_turn: false,
            calls: HashMap::new(),
            reported: HashSet::new(),
        }
    }

    fn start_turn(&mut self, out: &mut Vec<BrainEvent>) {
        if !self.in_turn {
            self.in_turn = true;
            out.push(BrainEvent::TurnStarted {
                model: self.model.clone(),
            });
        }
    }

    fn report_call(&mut self, call_id: &str, out: &mut Vec<BrainEvent>) {
        if self.reported.contains(call_id) {
            return;
        }
        if let Some((name, arguments)) = self.calls.get(call_id) {
            out.push(BrainEvent::ToolRequested {
                call_id: call_id.to_string(),
                name: name.clone(),
                arguments: arguments.clone(),
            });
            self.reported.insert(call_id.to_string());
        }
    }

    fn update(&mut self, update: &Value, out: &mut Vec<BrainEvent>) {
        let text = || {
            update["content"]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        match update["sessionUpdate"].as_str().unwrap_or_default() {
            "agent_message_chunk" => {
                self.start_turn(out);
                let text = text();
                if !text.is_empty() {
                    out.push(BrainEvent::Text { text });
                }
            }
            "agent_thought_chunk" => {
                self.start_turn(out);
                let text = text();
                if !text.is_empty() {
                    out.push(BrainEvent::Reasoning { text });
                }
            }
            "tool_call" => {
                self.start_turn(out);
                let call_id = update["toolCallId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                // agy names the tool (`run_command`) and makes `title` a
                // description; OpenCode puts the name in `title` (`bash`).
                let name = update["name"]
                    .as_str()
                    .or(update["title"].as_str())
                    .or(update["kind"].as_str())
                    .unwrap_or("tool");
                let arguments = input_of(update);
                self.calls
                    .insert(call_id.clone(), (name.to_string(), arguments));
                if matches!(
                    update["status"].as_str(),
                    Some("in_progress" | "completed" | "failed")
                ) {
                    self.report_call(&call_id, out);
                }
                self.finish_call(&call_id, update, out);
            }
            "tool_call_update" => {
                self.start_turn(out);
                let call_id = update["toolCallId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let entry = self.calls.entry(call_id.clone()).or_insert_with(|| {
                    (
                        update["name"].as_str().or(update["kind"].as_str()).unwrap_or("tool").to_string(),
                        Value::Null,
                    )
                });
                // The name stays what `tool_call` called it (live: `bash`);
                // later titles are descriptions (`echo hi > out.txt`).
                let input = input_of(update);
                if !input.is_null() {
                    entry.1 = input;
                }
                if matches!(
                    update["status"].as_str(),
                    Some("in_progress" | "completed" | "failed")
                ) {
                    self.report_call(&call_id, out);
                }
                self.finish_call(&call_id, update, out);
            }
            // Plans, available commands, running context size: nothing a
            // turn's events describe.
            _ => {}
        }
    }

    fn finish_call(&mut self, call_id: &str, update: &Value, out: &mut Vec<BrainEvent>) {
        let ok = match update["status"].as_str() {
            Some("completed") => true,
            Some("failed") => false,
            _ => return,
        };
        let output = update["content"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| part["content"]["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|text| !text.is_empty())
            .or_else(|| update["rawOutput"]["output"].as_str().map(str::to_string))
            .or_else(|| update["rawOutput"]["error"].as_str().map(str::to_string));
        out.push(BrainEvent::ToolResult {
            call_id: call_id.to_string(),
            ok,
            output,
        });
        self.calls.remove(call_id);
        self.reported.remove(call_id);
    }

    fn ended(&mut self, result: &Value, out: &mut Vec<BrainEvent>) {
        self.start_turn(out);
        let usage = &result["usage"];
        if usage.is_object() {
            let count = |key: &str| usage[key].as_u64().unwrap_or(0);
            out.push(BrainEvent::Usage(Usage {
                // Fresh input: cache reads are reported apart (live:
                // 139 in + 12032 cached + 2 out = 12173 total), and a cache
                // write is input that was paid for.
                input_tokens: count("inputTokens") + count("cachedWriteTokens"),
                output_tokens: count("outputTokens"),
                cached_input_tokens: count("cachedReadTokens"),
            }));
        }
        let reason = match result["stopReason"].as_str() {
            Some("end_turn") => StopReason::Completed,
            Some("cancelled") => StopReason::Interrupted,
            Some("max_tokens" | "max_turn_requests") => StopReason::Limit,
            _ => StopReason::Error,
        };
        if reason == StopReason::Error {
            out.push(BrainEvent::Error {
                message: format!(
                    "the turn stopped: {}",
                    result["stopReason"].as_str().unwrap_or("no stopReason")
                ),
            });
        }
        out.push(BrainEvent::TurnEnded { reason });
        self.in_turn = false;
    }
}

impl BrainAdapter for AcpAdapter {
    fn id(&self) -> &'static str {
        self.id
    }

    fn on_line(&mut self, line: &str) -> Vec<BrainEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            // ACP owns stdout; anything else there is a log line the agent
            // should have sent to stderr. Not an error in the conversation.
            return Vec::new();
        };
        let mut out = Vec::new();
        match (
            message.get("method").and_then(Value::as_str),
            message.get("id"),
        ) {
            (Some("session/update"), None) => self.update(&message["params"]["update"], &mut out),
            (Some("session/request_permission"), Some(id)) => {
                self.start_turn(&mut out);
                let call = &message["params"]["toolCall"];
                let call_id = call["toolCallId"].as_str().unwrap_or_default().to_string();
                let input = input_of(call);
                let entry = self.calls.entry(call_id.clone()).or_insert_with(|| {
                    (
                        call["name"].as_str().or(call["kind"].as_str()).unwrap_or("tool").to_string(),
                        Value::Null,
                    )
                });
                if !input.is_null() {
                    entry.1 = input.clone();
                }
                let tool = entry.0.clone();
                self.report_call(&call_id, &mut out);
                out.push(BrainEvent::PermissionRequested {
                    request_id: id_key(id),
                    tool,
                    input,
                    description: call["title"].as_str().map(str::to_string),
                });
            }
            (Some(_), _) => {}
            (None, Some(_)) => {
                if let Some(result) = message.get("result") {
                    if let Some(model) = result["configOptions"]
                        .as_array()
                        .and_then(|options| options.iter().find(|o| o["category"] == "model"))
                        .and_then(|option| option["currentValue"].as_str())
                        // agy: `models.currentModelId`.
                        .or(result["models"]["currentModelId"].as_str())
                    {
                        self.model = Some(model.to_string());
                    }
                    if result.get("stopReason").is_some() {
                        self.ended(result, &mut out);
                    }
                } else if let Some(error) = message.get("error") {
                    // An answer that is an error: setup failed (not logged
                    // in, bad cwd) or the prompt did. Either way nothing more
                    // is coming for this turn, and a caller waiting on it
                    // must hear so.
                    self.start_turn(&mut out);
                    out.push(BrainEvent::Error {
                        message: error["message"]
                            .as_str()
                            .unwrap_or("the agent answered with an error")
                            .to_string(),
                    });
                    out.push(BrainEvent::TurnEnded {
                        reason: StopReason::Error,
                    });
                    self.in_turn = false;
                }
            }
            (None, None) => {}
        }
        out
    }

    fn on_eof(&mut self) -> Vec<BrainEvent> {
        if self.in_turn {
            self.in_turn = false;
            vec![
                BrainEvent::Error {
                    message: "the agent exited in the middle of a turn".into(),
                },
                BrainEvent::TurnEnded {
                    reason: StopReason::Error,
                },
            ]
        } else {
            Vec::new()
        }
    }
}

#[derive(Default)]
struct HostState {
    next_id: u64,
    session: Option<String>,
    /// Prompts asked for before the session existed.
    queued: Vec<String>,
    /// For each open question: the id as the agent wrote it, and its options.
    asked: HashMap<String, (Value, Vec<Value>)>,
}

/// Drives an ACP agent.
pub struct AcpHost {
    id: &'static str,
    /// Turns `opencode` into `opencode acp`; see [`AcpHost::hosted_command`].
    subcommand: Option<&'static str>,
    flag: Option<&'static str>,
    env: Vec<(&'static str, &'static str)>,
    state: Mutex<HostState>,
}

impl AcpHost {
    /// `opencode acp`, with every tool that can change something set to
    /// ask. OpenCode's defaults allow everything, so a hosted session left
    /// at the defaults would never ask a single question -- the opposite of
    /// what hosting it is for. A caller that sets `OPENCODE_PERMISSION`
    /// itself keeps its own.
    pub fn opencode() -> Self {
        Self {
            id: "opencode",
            subcommand: Some("acp"),
            flag: None,
            env: vec![(
                "OPENCODE_PERMISSION",
                r#"{"bash":"ask","edit":"ask","webfetch":"ask","external_directory":"ask"}"#,
            )],
            state: Mutex::new(HostState::default()),
        }
    }

    /// `agy acp`: Google's agent CLI (Antigravity). It asks before running
    /// a command by default, so nothing needs setting. Verified against
    /// agy 1.5.3.
    pub fn agy() -> Self {
        Self {
            id: "agy",
            subcommand: Some("acp"),
            flag: None,
            env: Vec::new(),
            state: Mutex::new(HostState::default()),
        }
    }

    fn request(state: &mut HostState, method: &str, params: Value) -> String {
        state.next_id += 1;
        json!({"jsonrpc": "2.0", "id": state.next_id, "method": method, "params": params})
            .to_string()
    }

    fn prompt(state: &mut HostState, session: &str, text: &str) -> String {
        Self::request(
            state,
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]}),
        )
    }
}

impl HostProtocol for AcpHost {
    fn id(&self) -> &'static str {
        self.id
    }

    fn hosted_args(&self) -> &'static [&'static str] {
        &[]
    }

    fn adapter(&self) -> Option<Box<dyn BrainAdapter>> {
        Some(Box::new(AcpAdapter::new(self.id)))
    }

    /// The command in ACP mode. For a subcommand CLI (`opencode`) the
    /// subcommand goes right after the program; a command that already names
    /// it is left alone. `opencode run ...` is refused rather than rewritten:
    /// its flags belong to `run`, and guessing which of them `acp` accepts
    /// would launch something other than what was asked for.
    fn hosted_command(&self, command: &[String]) -> anyhow::Result<Vec<String>> {
        let mut out = match self.subcommand {
            Some(sub) => crate::host::with_subcommand(command, self.id, sub)?,
            None => command.to_vec(),
        };
        if let Some(flag) = self.flag {
            if !out.iter().any(|word| word == flag) {
                out.push(flag.to_string());
            }
        }
        Ok(out)
    }

    fn hosted_env(&self) -> Vec<(String, String)> {
        self.env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn opening(&self, cwd: Option<&str>) -> Vec<String> {
        let mut state = self.state.lock().unwrap();
        let cwd = cwd
            .map(str::to_string)
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|dir| dir.display().to_string())
            })
            .unwrap_or_else(|| ".".into());
        vec![
            Self::request(
                &mut state,
                "initialize",
                json!({
                    "protocolVersion": 1,
                    // No file system or terminal offered: the agent uses its
                    // own tools, and asks before the ones that need asking.
                    "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
                }),
            ),
            Self::request(
                &mut state,
                "session/new",
                json!({"cwd": cwd, "mcpServers": []}),
            ),
        ]
    }

    fn user_message(&self, text: &str) -> Vec<String> {
        let mut state = self.state.lock().unwrap();
        match state.session.clone() {
            Some(session) => vec![Self::prompt(&mut state, &session, text)],
            None => {
                state.queued.push(text.to_string());
                Vec::new()
            }
        }
    }

    fn observe(&self, line: &str) -> Vec<String> {
        let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
            return Vec::new();
        };
        let mut state = self.state.lock().unwrap();
        match (message.get("method").and_then(Value::as_str), message.get("id")) {
            (Some("session/request_permission"), Some(id)) => {
                let options = message["params"]["options"].as_array().cloned().unwrap_or_default();
                state.asked.insert(id_key(id), (id.clone(), options));
                Vec::new()
            }
            // Any other request: we offered no capability that it could be
            // for. Answer it anyway -- an agent waiting on an answer that
            // never comes hangs the turn.
            (Some(method), Some(id)) => vec![json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("{method} is not offered by this host")},
            })
            .to_string()],
            (None, Some(_)) => {
                let Some(session) = message["result"]["sessionId"].as_str().map(str::to_string) else {
                    return Vec::new();
                };
                if state.session.is_some() {
                    return Vec::new();
                }
                state.session = Some(session.clone());
                let queued = std::mem::take(&mut state.queued);
                queued.iter().map(|text| Self::prompt(&mut state, &session, text)).collect()
            }
            _ => Vec::new(),
        }
    }

    /// `allow_once` or `reject_once` from the options the agent offered.
    /// Never an `always` option: one answer covers one call.
    ///
    /// ACP's answer is a choice, not a message, so a refusal's `message`
    /// cannot reach the model here; it stays on the record. `interrupt`
    /// adds `session/cancel`, which ends the turn as `cancelled`.
    fn permission_reply(
        &self,
        request_id: &str,
        decision: &Decision,
        _input: &Value,
    ) -> Vec<String> {
        let mut state = self.state.lock().unwrap();
        let (id, options) = state
            .asked
            .remove(request_id)
            .unwrap_or_else(|| (Value::String(request_id.to_string()), Vec::new()));
        let wanted = match decision {
            Decision::Allow => "allow_once",
            Decision::Deny { .. } => "reject_once",
        };
        let option = options
            .iter()
            .find(|option| option["kind"] == wanted)
            .and_then(|option| option["optionId"].as_str())
            .map(str::to_string);
        let outcome = match option {
            Some(option) => json!({"outcome": "selected", "optionId": option}),
            // Nothing to choose that means what was decided: cancelling the
            // question is the one answer that never runs the tool.
            None => json!({"outcome": "cancelled"}),
        };
        let mut lines =
            vec![json!({"jsonrpc": "2.0", "id": id, "result": {"outcome": outcome}}).to_string()];
        if let (
            Decision::Deny {
                interrupt: true, ..
            },
            Some(session),
        ) = (decision, &state.session)
        {
            lines.push(
                json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session}}).to_string(),
            );
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines OpenCode 1.19.5 printed over `opencode acp` (trimmed).
    const LIVE: &[&str] = &[
        r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"name":"OpenCode","version":"1.19.5"}}}"#,
        r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"ses_e3ce","configOptions":[{"id":"model","category":"model","type":"select","currentValue":"opencode/deepseek-v4.1-flash-free"}]}}"#,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"ses_e3ce","update":{"sessionUpdate":"available_commands_update","availableCommands":[]}}}"#,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"ses_e3ce","update":{"sessionUpdate":"tool_call","toolCallId":"call_1","title":"bash","kind":"execute","status":"pending","rawInput":{"cwd":"/w"}}}}"#,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"ses_e3ce","update":{"sessionUpdate":"tool_call_update","toolCallId":"call_1","status":"in_progress","kind":"execute","title":"echo hi > out.txt","rawInput":{"command":"echo hi > out.txt","cwd":"/w"}}}}"#,
        r#"{"jsonrpc":"2.0","id":0,"method":"session/request_permission","params":{"sessionId":"ses_e3ce","toolCall":{"toolCallId":"call_1","title":"echo hi > out.txt","kind":"execute","status":"pending","rawInput":{"command":"echo hi > out.txt"}},"options":[{"optionId":"once","kind":"allow_once","name":"Allow once"},{"optionId":"always","kind":"allow_always","name":"Always allow"},{"optionId":"reject","kind":"reject_once","name":"Reject"}]}}"#,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"ses_e3ce","update":{"sessionUpdate":"tool_call_update","toolCallId":"call_1","status":"completed","title":"echo hi > out.txt","content":[{"type":"content","content":{"type":"text","text":"(no output)"}}],"rawOutput":{"output":"(no output)"}}}}"#,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"ses_e3ce","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"done"}}}}"#,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"ses_e3ce","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"written"}}}}"#,
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"ses_e3ce","update":{"sessionUpdate":"usage_update","used":12171,"size":200000}}}"#,
        r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn","usage":{"inputTokens":139,"outputTokens":2,"totalTokens":12173,"cachedReadTokens":12032},"_meta":{}}}"#,
    ];

    fn run(lines: &[&str]) -> Vec<BrainEvent> {
        let mut adapter = AcpAdapter::new("opencode");
        let mut out: Vec<BrainEvent> = lines
            .iter()
            .flat_map(|line| adapter.on_line(line))
            .collect();
        out.extend(adapter.on_eof());
        out
    }

    #[test]
    fn a_live_opencode_turn_reads_as_brain_events() {
        assert_eq!(
            run(LIVE),
            vec![
                BrainEvent::TurnStarted {
                    model: Some("opencode/deepseek-v4.1-flash-free".into())
                },
                BrainEvent::ToolRequested {
                    call_id: "call_1".into(),
                    name: "bash".into(),
                    arguments: json!({"command": "echo hi > out.txt", "cwd": "/w"}),
                },
                BrainEvent::PermissionRequested {
                    request_id: "0".into(),
                    tool: "bash".into(),
                    input: json!({"command": "echo hi > out.txt"}),
                    description: Some("echo hi > out.txt".into()),
                },
                BrainEvent::ToolResult {
                    call_id: "call_1".into(),
                    ok: true,
                    output: Some("(no output)".into())
                },
                BrainEvent::Reasoning {
                    text: "done".into()
                },
                BrainEvent::Text {
                    text: "written".into()
                },
                BrainEvent::Usage(Usage {
                    input_tokens: 139,
                    output_tokens: 2,
                    cached_input_tokens: 12032
                }),
                BrainEvent::TurnEnded {
                    reason: StopReason::Completed
                },
            ]
        );
    }

    #[test]
    fn a_cancelled_turn_is_interrupted_and_an_error_answer_ends_the_turn() {
        let cancelled = run(&[
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"c","status":"failed","kind":"execute","rawInput":{"command":"x"},"content":[{"type":"content","content":{"type":"text","text":"Tool execution aborted"}}]}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"result":{"stopReason":"cancelled","usage":{"inputTokens":0,"outputTokens":0}}}"#,
        ]);
        assert!(cancelled.contains(&BrainEvent::ToolResult {
            call_id: "c".into(),
            ok: false,
            output: Some("Tool execution aborted".into())
        }));
        assert_eq!(
            cancelled.last(),
            Some(&BrainEvent::TurnEnded {
                reason: StopReason::Interrupted
            })
        );

        let failed = run(&[
            r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"Authentication required"}}"#,
        ]);
        assert_eq!(
            failed,
            vec![
                BrainEvent::TurnStarted { model: None },
                BrainEvent::Error {
                    message: "Authentication required".into()
                },
                BrainEvent::TurnEnded {
                    reason: StopReason::Error
                },
            ]
        );

        let cut = run(&[
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hal"}}}}"#,
        ]);
        assert_eq!(
            cut.last(),
            Some(&BrainEvent::TurnEnded {
                reason: StopReason::Error
            })
        );
        assert!(run(&["INFO some log line", ""]).is_empty());
    }

    #[test]
    fn a_live_agy_turn_reads_with_its_names_and_model() {
        // agy 1.5.3 over `agy acp` (trimmed): the tool is `name`, the
        // request id a string, the model under `models`.
        let events = run(&[
            r#"{"jsonrpc":"2.0","result":{"sessionId":"c2aa","models":{"currentModelId":"gemini-4-argon-medium"}},"id":2}"#,
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"run it"}}}}"#,
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"call_17","name":"run_command","args":{"CommandLine":"echo hi > out.txt"},"status":"in_progress","kind":"execute","title":"Write hi to out.txt"}}}"#,
            r#"{"jsonrpc":"2.0","method":"session/request_permission","params":{"toolCall":{"toolCallId":"call_17","name":"run_command","args":{"CommandLine":"echo hi > out.txt"},"status":"pending","kind":"execute","title":"Write hi to out.txt"},"options":[{"optionId":"allow","kind":"allow_once"},{"optionId":"deny","kind":"reject_once"}]},"id":"1"}"#,
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"call_17","name":"run_command","status":"completed","kind":"execute"}}}"#,
            r#"{"jsonrpc":"2.0","result":{"stopReason":"end_turn","usage":{"inputTokens":9696,"outputTokens":55,"cachedReadTokens":5447,"cachedWriteTokens":0,"totalTokens":15198}},"id":3}"#,
        ]);
        let args = json!({"CommandLine": "echo hi > out.txt"});
        assert_eq!(
            events,
            vec![
                BrainEvent::TurnStarted { model: Some("gemini-4-argon-medium".into()) },
                BrainEvent::ToolRequested { call_id: "call_17".into(), name: "run_command".into(), arguments: args.clone() },
                BrainEvent::PermissionRequested {
                    request_id: "1".into(),
                    tool: "run_command".into(),
                    input: args,
                    description: Some("Write hi to out.txt".into()),
                },
                BrainEvent::ToolResult { call_id: "call_17".into(), ok: true, output: None },
                BrainEvent::Usage(Usage { input_tokens: 9696, output_tokens: 55, cached_input_tokens: 5447 }),
                BrainEvent::TurnEnded { reason: StopReason::Completed },
            ]
        );
        let host = AcpHost::agy();
        host.observe(r#"{"jsonrpc":"2.0","method":"session/request_permission","params":{"options":[{"optionId":"allow","kind":"allow_once"},{"optionId":"deny","kind":"reject_once"}]},"id":"1"}"#);
        let reply: Value = serde_json::from_str(&host.permission_reply("1", &Decision::Allow, &Value::Null)[0]).unwrap();
        assert_eq!(reply, json!({"jsonrpc": "2.0", "id": "1", "result": {"outcome": {"outcome": "selected", "optionId": "allow"}}}));
        let words = |list: &[&str]| list.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert_eq!(host.hosted_command(&words(&["agy"])).unwrap(), words(&["agy", "acp"]));
        assert!(host.hosted_env().is_empty(), "agy asks by default");
    }

    fn parsed(lines: Vec<String>) -> Vec<Value> {
        lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn the_prompt_waits_for_the_session_and_answers_use_the_offered_options() {
        let host = AcpHost::opencode();
        let opening = parsed(host.opening(Some("/w")));
        assert_eq!(opening[0]["method"], "initialize");
        assert_eq!(opening[1]["method"], "session/new");
        assert_eq!(opening[1]["params"]["cwd"], "/w");

        // Asked before the session exists: held, then sent once it does.
        assert!(host.user_message("hello").is_empty());
        let sent = parsed(host.observe(LIVE[1]));
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["method"], "session/prompt");
        assert_eq!(sent[0]["params"]["sessionId"], "ses_e3ce");
        assert_eq!(sent[0]["params"]["prompt"][0]["text"], "hello");
        assert_ne!(sent[0]["id"], opening[1]["id"]);
        // A second session/new answer does not resend anything.
        assert!(host.observe(LIVE[1]).is_empty());
        let next = parsed(host.user_message("again"));
        assert_eq!(next[0]["params"]["prompt"][0]["text"], "again");

        // Allow picks allow_once (never "always"), with the id's own type.
        assert!(host.observe(LIVE[5]).is_empty());
        let allow = parsed(host.permission_reply("0", &Decision::Allow, &Value::Null));
        assert_eq!(
            allow,
            vec![
                json!({"jsonrpc": "2.0", "id": 0, "result": {"outcome": {"outcome": "selected", "optionId": "once"}}})
            ]
        );

        // Deny + interrupt: reject_once, then cancel the turn.
        host.observe(LIVE[5]);
        let deny = parsed(host.permission_reply(
            "0",
            &Decision::Deny {
                message: "no".into(),
                interrupt: true,
            },
            &Value::Null,
        ));
        assert_eq!(deny[0]["result"]["outcome"]["optionId"], "reject");
        assert_eq!(
            deny[1],
            json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "ses_e3ce"}})
        );

        // No matching option: cancel the question rather than guess.
        host.observe(r#"{"jsonrpc":"2.0","id":"q","method":"session/request_permission","params":{"options":[{"optionId":"always","kind":"allow_always"}]}}"#);
        let only_always = parsed(host.permission_reply("q", &Decision::Allow, &Value::Null));
        assert_eq!(only_always[0]["id"], "q");
        assert_eq!(
            only_always[0]["result"]["outcome"],
            json!({"outcome": "cancelled"})
        );

        // A request for something we never offered is answered, not ignored.
        let refused = parsed(
            host.observe(r#"{"jsonrpc":"2.0","id":7,"method":"fs/read_text_file","params":{}}"#),
        );
        assert_eq!(refused[0]["id"], 7);
        assert_eq!(refused[0]["error"]["code"], -32601);
    }

    #[test]
    fn opencode_is_put_into_acp_mode_without_guessing() {
        let host = AcpHost::opencode();
        let words = |list: &[&str]| list.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert_eq!(
            host.hosted_command(&words(&["opencode"])).unwrap(),
            words(&["opencode", "acp"])
        );
        assert_eq!(
            host.hosted_command(&words(&["cmd", "/c", "opencode", "--pure"]))
                .unwrap(),
            words(&["cmd", "/c", "opencode", "acp", "--pure"])
        );
        assert_eq!(
            host.hosted_command(&words(&["opencode", "acp"])).unwrap(),
            words(&["opencode", "acp"])
        );
        assert_eq!(
            host.hosted_command(&words(&["sh", "acp-wrapper.sh"]))
                .unwrap(),
            words(&["sh", "acp-wrapper.sh"])
        );
        let err = host
            .hosted_command(&words(&["opencode", "run", "hi"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("opencode acp"), "{err}");
        assert!(host
            .hosted_env()
            .iter()
            .any(|(k, v)| k == "OPENCODE_PERMISSION" && v.contains("\"bash\":\"ask\"")));
    }
}
