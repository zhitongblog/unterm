//! Codex driven over `codex app-server` (JSON-RPC over stdio).
//!
//! `codex exec --json` cannot be asked anything: its approval policy is
//! `never`, so a command that needs approval is rejected -- live, on a
//! Windows machine without Codex's sandbox set up, that was every command --
//! and a rejected command does not even appear in its output. The app server
//! is Codex's own host protocol (the one its IDE extensions use):
//!
//! - `initialize`, then the `initialized` notification, then `thread/start`
//!   (here with approval policy `untrusted`: only commands Codex knows to be
//!   safe run unasked, which is what Claude does by default);
//! - each user turn is `turn/start` on that thread;
//! - progress arrives as notifications (`turn/started`, `item/started`,
//!   `item/completed`, `thread/tokenUsage/updated`, `turn/completed`);
//! - before a command or a patch Codex sends a *request*
//!   (`item/commandExecution/requestApproval`,
//!   `item/fileChange/requestApproval`), answered with a decision:
//!   `accept`, `decline` (the turn goes on) or `cancel` (it stops).
//!
//! Shapes verified against Codex 0.167 (`codex app-server`), except
//! `item/permissions/requestApproval`, taken from its generated schema.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::host::{Decision, HostProtocol};
use crate::{BrainAdapter, BrainEvent, StopReason, Usage};

const COMMAND_APPROVAL: &str = "item/commandExecution/requestApproval";
const PATCH_APPROVAL: &str = "item/fileChange/requestApproval";
const PERMISSIONS_APPROVAL: &str = "item/permissions/requestApproval";

fn id_key(id: &Value) -> String {
    match id {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// What a tool item is, as a request: its name and arguments.
fn tool_of(item: &Value) -> Option<(String, Value)> {
    match str_at(item, "type")? {
        "commandExecution" => Some((
            "shell".into(),
            json!({"command": item.get("command").cloned().unwrap_or(Value::Null),
                   "cwd": item.get("cwd").cloned().unwrap_or(Value::Null)}),
        )),
        "fileChange" => Some((
            "apply_patch".into(),
            item.get("changes").cloned().unwrap_or(Value::Null),
        )),
        "mcpToolCall" => {
            let name = match (str_at(item, "server"), str_at(item, "tool")) {
                (Some(server), Some(tool)) => format!("{server}.{tool}"),
                (None, Some(tool)) => tool.to_string(),
                _ => "mcp".into(),
            };
            Some((name, item.get("arguments").cloned().unwrap_or(Value::Null)))
        }
        "dynamicToolCall" => Some((
            str_at(item, "tool").unwrap_or("tool").to_string(),
            item.get("arguments").cloned().unwrap_or(Value::Null),
        )),
        "webSearch" => Some((
            "web_search".into(),
            json!({"query": item.get("query").cloned().unwrap_or(Value::Null)}),
        )),
        _ => None,
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
struct Totals {
    input: u64,
    cached: u64,
    cache_write: u64,
    output: u64,
}

impl Totals {
    fn of(total: &Value) -> Self {
        let count = |key: &str| total.get(key).and_then(Value::as_u64).unwrap_or(0);
        Self {
            input: count("inputTokens"),
            cached: count("cachedInputTokens"),
            cache_write: count("cacheWriteInputTokens"),
            output: count("outputTokens"),
        }
    }
}

/// Reads `codex app-server`'s stdout.
#[derive(Default)]
pub struct CodexAppAdapter {
    model: Option<String>,
    thread: Option<String>,
    in_turn: bool,
    /// Tool items by id, so a patch approval (which names only the item)
    /// can say what the patch is.
    items: HashMap<String, (String, Value)>,
    reported: HashSet<String>,
    /// Running totals for the thread, and where they stood when the last
    /// turn ended: a turn's cost is the difference.
    total: Totals,
    at_turn_start: Totals,
}

impl CodexAppAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    fn start_turn(&mut self, out: &mut Vec<BrainEvent>) {
        if !self.in_turn {
            self.in_turn = true;
            self.at_turn_start = self.total;
            out.push(BrainEvent::TurnStarted {
                model: self.model.clone(),
            });
        }
    }

    fn request(&mut self, item: &Value, out: &mut Vec<BrainEvent>) {
        let Some(id) = str_at(item, "id").map(str::to_string) else {
            return;
        };
        let Some((name, arguments)) = tool_of(item) else {
            return;
        };
        self.items
            .insert(id.clone(), (name.clone(), arguments.clone()));
        if self.reported.insert(id.clone()) {
            out.push(BrainEvent::ToolRequested {
                call_id: id,
                name,
                arguments,
            });
        }
    }

    fn completed(&mut self, item: &Value, out: &mut Vec<BrainEvent>) {
        match str_at(item, "type").unwrap_or_default() {
            "agentMessage" => {
                if let Some(text) = str_at(item, "text").filter(|text| !text.is_empty()) {
                    out.push(BrainEvent::Text {
                        text: text.to_string(),
                    });
                }
            }
            "reasoning" => {
                let text: Vec<&str> = ["summary", "content"]
                    .iter()
                    .filter_map(|key| item.get(*key).and_then(Value::as_array))
                    .find(|parts| !parts.is_empty())
                    .map(|parts| parts.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                let text = text.join("\n");
                if !text.is_empty() {
                    out.push(BrainEvent::Reasoning { text });
                }
            }
            kind => {
                if tool_of(item).is_none() {
                    return;
                }
                // Reported as a request even when Codex only says it ended.
                self.request(item, out);
                let id = str_at(item, "id").unwrap_or_default().to_string();
                let status = str_at(item, "status");
                let ok = match kind {
                    "commandExecution" => {
                        status == Some("completed")
                            && item.get("exitCode").and_then(Value::as_i64).unwrap_or(0) == 0
                    }
                    "dynamicToolCall" => item
                        .get("success")
                        .and_then(Value::as_bool)
                        .unwrap_or(status == Some("completed")),
                    "webSearch" => true,
                    _ => status == Some("completed"),
                };
                let output = match kind {
                    "commandExecution" => str_at(item, "aggregatedOutput").map(str::to_string),
                    "mcpToolCall" => item
                        .get("error")
                        .filter(|error| !error.is_null())
                        .or_else(|| item.get("result").filter(|result| !result.is_null()))
                        .map(|value| {
                            value
                                .as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| value.to_string())
                        }),
                    _ => None,
                }
                .or_else(|| (status == Some("declined")).then(|| "declined".to_string()));
                out.push(BrainEvent::ToolResult {
                    call_id: id.clone(),
                    ok,
                    output,
                });
                self.items.remove(&id);
                self.reported.remove(&id);
            }
        }
    }

    fn turn_completed(&mut self, turn: &Value, out: &mut Vec<BrainEvent>) {
        self.start_turn(out);
        if self.total != self.at_turn_start {
            let (now, before) = (self.total, self.at_turn_start);
            let input = now.input.saturating_sub(before.input);
            let cached = now.cached.saturating_sub(before.cached);
            out.push(BrainEvent::Usage(Usage {
                // `inputTokens` counts the whole prompt, cache hits included
                // (live: 15099 in, 12672 of them cached).
                input_tokens: input.saturating_sub(cached),
                output_tokens: now.output.saturating_sub(before.output),
                cached_input_tokens: cached,
            }));
            let _ = now.cache_write;
        }
        let reason = match str_at(turn, "status") {
            Some("completed") => StopReason::Completed,
            Some("interrupted") => StopReason::Interrupted,
            _ => StopReason::Error,
        };
        if reason == StopReason::Error {
            out.push(BrainEvent::Error {
                message: turn["error"]["message"]
                    .as_str()
                    .unwrap_or("the turn failed")
                    .to_string(),
            });
        }
        out.push(BrainEvent::TurnEnded { reason });
        self.in_turn = false;
    }
}

impl BrainAdapter for CodexAppAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn on_line(&mut self, line: &str) -> Vec<BrainEvent> {
        let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let params = &message["params"];
        match (
            message.get("method").and_then(Value::as_str),
            message.get("id"),
        ) {
            (Some("turn/started"), None) => self.start_turn(&mut out),
            (Some("item/started"), None) => {
                self.start_turn(&mut out);
                self.request(&params["item"], &mut out);
            }
            (Some("item/completed"), None) => {
                self.start_turn(&mut out);
                self.completed(&params["item"], &mut out);
            }
            (Some("thread/tokenUsage/updated"), None) => {
                self.total = Totals::of(&params["tokenUsage"]["total"]);
            }
            (Some("turn/completed"), None) => self.turn_completed(&params["turn"], &mut out),
            // A retry is not a failure; one that will not be retried is
            // reported, and `turn/completed` (failed) ends the turn.
            (Some("error"), None) if params["willRetry"] != json!(true) => {
                out.push(BrainEvent::Error {
                    message: params["error"]["message"]
                        .as_str()
                        .unwrap_or("codex reported an error")
                        .to_string(),
                });
            }
            (
                Some(method @ (COMMAND_APPROVAL | PATCH_APPROVAL | PERMISSIONS_APPROVAL)),
                Some(id),
            ) => {
                self.start_turn(&mut out);
                let item_id = str_at(params, "itemId").unwrap_or_default().to_string();
                let (tool, input) = match method {
                    COMMAND_APPROVAL => (
                        "shell".to_string(),
                        json!({"command": params.get("command").cloned().unwrap_or(Value::Null),
                               "cwd": params.get("cwd").cloned().unwrap_or(Value::Null)}),
                    ),
                    PATCH_APPROVAL => (
                        "apply_patch".to_string(),
                        self.items
                            .get(&item_id)
                            .map(|(_, args)| args.clone())
                            .unwrap_or(Value::Null),
                    ),
                    _ => (
                        "permissions".to_string(),
                        params.get("permissions").cloned().unwrap_or(Value::Null),
                    ),
                };
                out.push(BrainEvent::PermissionRequested {
                    request_id: id_key(id),
                    tool,
                    input,
                    description: str_at(params, "reason").map(str::to_string),
                });
            }
            (Some(_), _) => {}
            (None, Some(_)) => {
                if let Some(result) = message.get("result") {
                    // `thread/start`'s answer names the thread and the model.
                    if let Some(thread) = result["thread"]["id"].as_str() {
                        self.thread = Some(thread.to_string());
                        if let Some(model) = str_at(result, "model") {
                            self.model = Some(model.to_string());
                        }
                    }
                } else if let Some(error) = message.get("error") {
                    // Setup failed (not signed in, bad cwd) or a turn could
                    // not start: nothing more is coming for it.
                    self.start_turn(&mut out);
                    out.push(BrainEvent::Error {
                        message: error["message"]
                            .as_str()
                            .unwrap_or("codex answered with an error")
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

    fn external_id(&self) -> Option<&str> {
        self.thread.as_deref()
    }

    fn on_eof(&mut self) -> Vec<BrainEvent> {
        if self.in_turn {
            self.in_turn = false;
            vec![
                BrainEvent::Error {
                    message: "codex exited in the middle of a turn".into(),
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
    cwd: String,
    initialize_id: Option<u64>,
    /// A thread to continue rather than start.
    resume: Option<String>,
    thread: Option<String>,
    queued: Vec<String>,
    /// Open questions: the id as Codex wrote it, the method, and the params.
    asked: HashMap<String, (Value, String, Value)>,
    /// Skill folders for this app server only (`skills/extraRoots/set`),
    /// so nothing is written into the person's own `~/.codex`.
    skill_roots: Vec<String>,
}

/// Drives `codex app-server`.
#[derive(Default)]
pub struct CodexAppHost {
    state: Mutex<HostState>,
}

impl CodexAppHost {
    pub fn new() -> Self {
        Self::default()
    }

    fn request(state: &mut HostState, method: &str, params: Value) -> (u64, String) {
        state.next_id += 1;
        let id = state.next_id;
        (
            id,
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
        )
    }

    fn turn(state: &mut HostState, thread: &str, text: &str) -> String {
        Self::request(
            state,
            "turn/start",
            json!({"threadId": thread, "input": [{"type": "text", "text": text, "text_elements": []}]}),
        )
        .1
    }
}

impl HostProtocol for CodexAppHost {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn hosted_args(&self) -> &'static [&'static str] {
        &[]
    }

    fn adapter(&self) -> Option<Box<dyn BrainAdapter>> {
        Some(Box::new(CodexAppAdapter::new()))
    }

    fn hosted_command(&self, command: &[String]) -> anyhow::Result<Vec<String>> {
        crate::host::with_subcommand(command, "codex", "app-server")
    }

    /// The same command; the thread is continued with `thread/resume`
    /// instead of `thread/start` once the server is initialized.
    fn resume(&self, command: Vec<String>, external_id: &str) -> anyhow::Result<Vec<String>> {
        if external_id.trim().is_empty() {
            anyhow::bail!("no thread id to resume");
        }
        self.state.lock().unwrap().resume = Some(external_id.to_string());
        Ok(command)
    }

    fn use_skill_roots(&self, roots: &[String]) {
        self.state.lock().unwrap().skill_roots = roots.to_vec();
    }

    /// Only `initialize`: the thread is started once it has been answered.
    fn opening(&self, cwd: Option<&str>) -> Vec<String> {
        let mut state = self.state.lock().unwrap();
        state.cwd = cwd
            .map(str::to_string)
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|dir| dir.display().to_string())
            })
            .unwrap_or_else(|| ".".into());
        let (id, line) = Self::request(
            &mut state,
            "initialize",
            json!({"clientInfo": {"name": "unterm", "version": env!("CARGO_PKG_VERSION")}}),
        );
        state.initialize_id = Some(id);
        vec![line]
    }

    fn observe(&self, line: &str) -> Vec<String> {
        let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
            return Vec::new();
        };
        let mut state = self.state.lock().unwrap();
        match (message.get("method").and_then(Value::as_str), message.get("id")) {
            (Some(method @ (COMMAND_APPROVAL | PATCH_APPROVAL | PERMISSIONS_APPROVAL)), Some(id)) => {
                state.asked.insert(id_key(id), (id.clone(), method.to_string(), message["params"].clone()));
                Vec::new()
            }
            // Anything else Codex asks of its host -- user input, MCP
            // elicitation, a dynamic tool, a token refresh -- this host does
            // not offer. Answered, so the turn does not hang on it.
            (Some(method), Some(id)) => vec![json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("{method} is not offered by this host")},
            })
            .to_string()],
            (None, Some(id)) if message.get("result").is_some() => {
                if id.as_u64().is_some() && id.as_u64() == state.initialize_id {
                    state.initialize_id = None;
                    let cwd = state.cwd.clone();
                    let start = match state.resume.clone() {
                        // The history stays where it is: the caller has it
                        // already, and replaying it would read as new events.
                        Some(thread) => Self::request(
                            &mut state,
                            "thread/resume",
                            json!({"threadId": thread, "cwd": cwd, "approvalPolicy": "untrusted",
                                   "sandbox": "workspace-write", "excludeTurns": true}),
                        ),
                        None => Self::request(
                            &mut state,
                            "thread/start",
                            json!({"cwd": cwd, "approvalPolicy": "untrusted", "sandbox": "workspace-write"}),
                        ),
                    }
                    .1;
                    let mut lines = vec![json!({"jsonrpc": "2.0", "method": "initialized"}).to_string()];
                    if !state.skill_roots.is_empty() {
                        let roots = state.skill_roots.clone();
                        lines.push(Self::request(&mut state, "skills/extraRoots/set", json!({"extraRoots": roots})).1);
                    }
                    lines.push(start);
                    return lines;
                }
                let Some(thread) = message["result"]["thread"]["id"].as_str().map(str::to_string) else {
                    return Vec::new();
                };
                if state.thread.is_some() {
                    return Vec::new();
                }
                state.thread = Some(thread.clone());
                let queued = std::mem::take(&mut state.queued);
                queued.iter().map(|text| Self::turn(&mut state, &thread, text)).collect()
            }
            _ => Vec::new(),
        }
    }

    fn user_message(&self, text: &str) -> Vec<String> {
        let mut state = self.state.lock().unwrap();
        match state.thread.clone() {
            Some(thread) => vec![Self::turn(&mut state, &thread, text)],
            None => {
                state.queued.push(text.to_string());
                Vec::new()
            }
        }
    }

    /// `accept`, `decline` (the turn goes on) or `cancel` (it stops): the
    /// decision covers this one call, never the session. Codex's answer has
    /// no message, so a refusal's reason stays on the record.
    ///
    /// A permissions request is answered with what it asked for, for this
    /// turn only, or with nothing; stopping the turn then takes
    /// `turn/interrupt`, since that answer has no `cancel`.
    fn permission_reply(
        &self,
        request_id: &str,
        decision: &Decision,
        _input: &Value,
    ) -> Vec<String> {
        let mut state = self.state.lock().unwrap();
        let (id, method, params) = state.asked.remove(request_id).unwrap_or_else(|| {
            (
                Value::String(request_id.to_string()),
                COMMAND_APPROVAL.to_string(),
                Value::Null,
            )
        });
        if method == PERMISSIONS_APPROVAL {
            let (result, interrupt) = match decision {
                Decision::Allow => (
                    json!({"permissions": params["permissions"], "scope": "turn"}),
                    false,
                ),
                Decision::Deny { interrupt, .. } => (json!({"permissions": {}}), *interrupt),
            };
            let mut lines = vec![json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()];
            if interrupt {
                let turn = json!({"threadId": params["threadId"], "turnId": params["turnId"]});
                lines.push(Self::request(&mut state, "turn/interrupt", turn).1);
            }
            return lines;
        }
        let answer = match decision {
            Decision::Allow => "accept",
            Decision::Deny {
                interrupt: false, ..
            } => "decline",
            Decision::Deny {
                interrupt: true, ..
            } => "cancel",
        };
        vec![json!({"jsonrpc": "2.0", "id": id, "result": {"decision": answer}}).to_string()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines Codex 0.167 printed over `codex app-server` (trimmed).
    const LIVE: &[&str] = &[
        r#"{"id": 1, "result": {"userAgent": "probe/0.167.0", "platformOs": "windows"}}"#,
        r#"{"method": "configWarning", "params": {"summary": "Codex is ignoring 1 unrecognized configuration setting."}}"#,
        r#"{"id": 2, "result": {"thread": {"id": "th_1"}, "model": "gpt-6.1-sol", "approvalPolicy": "untrusted"}}"#,
        r#"{"id": 3, "result": {"turn": {"id": "tu_1", "status": "inProgress"}}}"#,
        r#"{"method": "turn/started", "params": {"threadId": "th_1", "turn": {"id": "tu_1", "status": "inProgress"}}}"#,
        r#"{"method": "item/started", "params": {"item": {"type": "userMessage", "id": "u1", "content": []}}}"#,
        r#"{"method": "item/completed", "params": {"item": {"type": "agentMessage", "id": "m1", "text": "I'll run the command exactly as provided.", "phase": "commentary"}}}"#,
        r#"{"method": "item/started", "params": {"item": {"type": "commandExecution", "id": "exec-1", "command": "powershell.exe -Command 'echo hi > out.txt'", "cwd": "C:\\w", "status": "inProgress", "exitCode": null}}}"#,
        r#"{"method": "item/commandExecution/requestApproval", "id": 0, "params": {"kind": "command", "threadId": "th_1", "turnId": "tu_1", "itemId": "exec-1", "command": "powershell.exe -Command 'echo hi > out.txt'", "cwd": "C:\\w"}}"#,
        r#"{"method": "serverRequest/resolved", "params": {"threadId": "th_1", "requestId": 0}}"#,
        r#"{"method": "item/completed", "params": {"item": {"type": "commandExecution", "id": "exec-1", "command": "powershell.exe -Command 'echo hi > out.txt'", "cwd": "C:\\w", "status": "completed", "exitCode": 0, "aggregatedOutput": ""}}}"#,
        r#"{"method": "thread/tokenUsage/updated", "params": {"threadId": "th_1", "turnId": "tu_1", "tokenUsage": {"total": {"totalTokens": 15146, "inputTokens": 15099, "cachedInputTokens": 12672, "outputTokens": 47}}}}"#,
        r#"{"method": "item/completed", "params": {"item": {"type": "agentMessage", "id": "m2", "text": "written", "phase": "final_answer"}}}"#,
        r#"{"method": "thread/tokenUsage/updated", "params": {"threadId": "th_1", "turnId": "tu_1", "tokenUsage": {"total": {"totalTokens": 30355, "inputTokens": 30303, "cachedInputTokens": 27648, "outputTokens": 52}}}}"#,
        r#"{"method": "turn/completed", "params": {"threadId": "th_1", "turn": {"id": "tu_1", "status": "completed", "error": null}}}"#,
    ];

    fn run(adapter: &mut CodexAppAdapter, lines: &[&str]) -> Vec<BrainEvent> {
        lines
            .iter()
            .flat_map(|line| adapter.on_line(line))
            .collect()
    }

    #[test]
    fn a_live_app_server_turn_reads_as_brain_events() {
        let mut adapter = CodexAppAdapter::new();
        let command =
            json!({"command": "powershell.exe -Command 'echo hi > out.txt'", "cwd": "C:\\w"});
        assert_eq!(
            run(&mut adapter, LIVE),
            vec![
                BrainEvent::TurnStarted {
                    model: Some("gpt-6.1-sol".into())
                },
                BrainEvent::Text {
                    text: "I'll run the command exactly as provided.".into()
                },
                BrainEvent::ToolRequested {
                    call_id: "exec-1".into(),
                    name: "shell".into(),
                    arguments: command.clone()
                },
                BrainEvent::PermissionRequested {
                    request_id: "0".into(),
                    tool: "shell".into(),
                    input: command,
                    description: None,
                },
                BrainEvent::ToolResult {
                    call_id: "exec-1".into(),
                    ok: true,
                    output: Some(String::new())
                },
                BrainEvent::Text {
                    text: "written".into()
                },
                BrainEvent::Usage(Usage {
                    input_tokens: 2655,
                    output_tokens: 52,
                    cached_input_tokens: 27648
                }),
                BrainEvent::TurnEnded {
                    reason: StopReason::Completed
                },
            ]
        );
        assert_eq!(adapter.external_id(), Some("th_1"));

        // The second turn costs only what it added to the thread's totals.
        let second = run(
            &mut adapter,
            &[
                r#"{"method": "turn/started", "params": {"turn": {"id": "tu_2"}}}"#,
                r#"{"method": "item/started", "params": {"item": {"type": "commandExecution", "id": "exec-2", "command": "echo no > deny.txt", "status": "inProgress"}}}"#,
                r#"{"method": "item/completed", "params": {"item": {"type": "commandExecution", "id": "exec-2", "command": "echo no > deny.txt", "status": "declined", "exitCode": null}}}"#,
                r#"{"method": "thread/tokenUsage/updated", "params": {"tokenUsage": {"total": {"inputTokens": 46542, "cachedInputTokens": 42624, "outputTokens": 99}}}}"#,
                r#"{"method": "turn/completed", "params": {"turn": {"id": "tu_2", "status": "interrupted", "error": null}}}"#,
            ],
        );
        assert!(second.contains(&BrainEvent::ToolResult {
            call_id: "exec-2".into(),
            ok: false,
            output: Some("declined".into())
        }));
        assert!(second.contains(&BrainEvent::Usage(Usage {
            input_tokens: (46542 - 30303) - (42624 - 27648),
            output_tokens: 47,
            cached_input_tokens: 42624 - 27648,
        })));
        assert_eq!(
            second.last(),
            Some(&BrainEvent::TurnEnded {
                reason: StopReason::Interrupted
            })
        );
    }

    #[test]
    fn retries_are_not_failures_and_a_failed_turn_says_why() {
        let mut adapter = CodexAppAdapter::new();
        let events = run(
            &mut adapter,
            &[
                r#"{"method": "turn/started", "params": {"turn": {"id": "t"}}}"#,
                r#"{"method": "error", "params": {"error": {"message": "Reconnecting... 1/5"}, "willRetry": true}}"#,
                r#"{"method": "turn/completed", "params": {"turn": {"id": "t", "status": "failed", "error": {"message": "usage limit reached"}}}}"#,
            ],
        );
        assert_eq!(
            events,
            vec![
                BrainEvent::TurnStarted { model: None },
                BrainEvent::Error {
                    message: "usage limit reached".into()
                },
                BrainEvent::TurnEnded {
                    reason: StopReason::Error
                },
            ]
        );
        let failed = run(
            &mut CodexAppAdapter::new(),
            &[r#"{"id": 2, "error": {"code": -32600, "message": "not logged in"}}"#],
        );
        assert_eq!(
            failed.last(),
            Some(&BrainEvent::TurnEnded {
                reason: StopReason::Error
            })
        );
        let cut = CodexAppAdapter::new().on_eof();
        assert!(cut.is_empty(), "no turn, nothing to end");
    }

    #[test]
    fn a_patch_approval_says_what_the_patch_is() {
        let mut adapter = CodexAppAdapter::new();
        let events = run(
            &mut adapter,
            &[
                r#"{"method": "item/started", "params": {"item": {"type": "fileChange", "id": "p1", "changes": [{"path": "a.txt", "kind": "add"}], "status": "inProgress"}}}"#,
                r#"{"method": "item/fileChange/requestApproval", "id": 5, "params": {"itemId": "p1", "threadId": "th", "turnId": "tu", "reason": "write a.txt"}}"#,
            ],
        );
        assert!(events.contains(&BrainEvent::PermissionRequested {
            request_id: "5".into(),
            tool: "apply_patch".into(),
            input: json!([{"path": "a.txt", "kind": "add"}]),
            description: Some("write a.txt".into()),
        }));
    }

    fn parsed(lines: Vec<String>) -> Vec<Value> {
        lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn skill_roots_are_set_for_this_server_before_the_thread_starts() {
        let host = CodexAppHost::new();
        host.use_skill_roots(&["C:/state/skills".to_string()]);
        let opening = parsed(host.opening(Some("C:/w")));
        let after_init = parsed(host.observe(&json!({"id": opening[0]["id"], "result": {}}).to_string()));
        assert_eq!(after_init[0]["method"], "initialized");
        assert_eq!(after_init[1]["method"], "skills/extraRoots/set");
        assert_eq!(after_init[1]["params"], json!({"extraRoots": ["C:/state/skills"]}));
        assert_eq!(after_init[2]["method"], "thread/start");
        // Its answer is not a thread: nothing is sent for it.
        assert!(host.observe(&json!({"id": after_init[1]["id"], "result": {}}).to_string()).is_empty());
    }

    #[test]
    fn the_host_starts_the_thread_after_initialize_and_answers_with_decisions() {
        let host = CodexAppHost::new();
        let opening = parsed(host.opening(Some("C:\\w")));
        assert_eq!(opening.len(), 1);
        assert_eq!(opening[0]["method"], "initialize");
        assert!(
            host.user_message("hello").is_empty(),
            "held until the thread exists"
        );

        let after_init =
            parsed(host.observe(&json!({"id": opening[0]["id"], "result": {}}).to_string()));
        assert_eq!(
            after_init[0],
            json!({"jsonrpc": "2.0", "method": "initialized"})
        );
        assert_eq!(after_init[1]["method"], "thread/start");
        assert_eq!(
            after_init[1]["params"],
            json!({"cwd": "C:\\w", "approvalPolicy": "untrusted", "sandbox": "workspace-write"})
        );

        let turn = parsed(host.observe(LIVE[2]));
        assert_eq!(turn.len(), 1);
        assert_eq!(turn[0]["method"], "turn/start");
        assert_eq!(turn[0]["params"]["threadId"], "th_1");
        assert_eq!(turn[0]["params"]["input"][0]["text"], "hello");
        assert!(host.observe(LIVE[2]).is_empty(), "one thread, started once");

        host.observe(LIVE[8]);
        assert_eq!(
            parsed(host.permission_reply("0", &Decision::Allow, &Value::Null)),
            vec![json!({"jsonrpc": "2.0", "id": 0, "result": {"decision": "accept"}})]
        );
        host.observe(LIVE[8]);
        let decline = parsed(host.permission_reply(
            "0",
            &Decision::Deny {
                message: "no".into(),
                interrupt: false,
            },
            &Value::Null,
        ));
        assert_eq!(decline[0]["result"]["decision"], "decline");
        host.observe(LIVE[8]);
        let cancel = parsed(host.permission_reply(
            "0",
            &Decision::Deny {
                message: "no".into(),
                interrupt: true,
            },
            &Value::Null,
        ));
        assert_eq!(cancel[0]["result"]["decision"], "cancel");

        host.observe(r#"{"method": "item/permissions/requestApproval", "id": 9, "params": {"threadId": "th_1", "turnId": "tu_1", "itemId": "x", "permissions": {"network": {"enabled": true}}}}"#);
        let refused = parsed(host.permission_reply(
            "9",
            &Decision::Deny {
                message: "no".into(),
                interrupt: true,
            },
            &Value::Null,
        ));
        assert_eq!(refused[0]["result"], json!({"permissions": {}}));
        assert_eq!(refused[1]["method"], "turn/interrupt");
        assert_eq!(
            refused[1]["params"],
            json!({"threadId": "th_1", "turnId": "tu_1"})
        );

        let unknown = parsed(
            host.observe(r#"{"method": "item/tool/requestUserInput", "id": 4, "params": {}}"#),
        );
        assert_eq!(unknown[0]["id"], 4);
        assert_eq!(unknown[0]["error"]["code"], -32601);
    }

    #[test]
    fn a_resumed_codex_session_continues_its_thread() {
        let host = CodexAppHost::new();
        let words = vec!["codex".to_string(), "app-server".to_string()];
        assert_eq!(host.resume(words.clone(), "th_9").unwrap(), words);
        let opening = parsed(host.opening(Some("C:\\w")));
        let after_init = parsed(host.observe(&json!({"id": opening[0]["id"], "result": {}}).to_string()));
        assert_eq!(after_init[1]["method"], "thread/resume");
        assert_eq!(after_init[1]["params"]["threadId"], "th_9");
        assert_eq!(after_init[1]["params"]["approvalPolicy"], "untrusted");
        assert_eq!(after_init[1]["params"]["excludeTurns"], true);
        assert!(host.resume(words, " ").is_err());
    }

    #[test]
    fn codex_is_put_into_app_server_mode_without_guessing() {
        let host = CodexAppHost::new();
        let words = |list: &[&str]| list.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert_eq!(
            host.hosted_command(&words(&["codex"])).unwrap(),
            words(&["codex", "app-server"])
        );
        assert_eq!(
            host.hosted_command(&words(&["cmd", "/c", "codex", "-c", "model=\"o3\""]))
                .unwrap(),
            words(&["cmd", "/c", "codex", "app-server", "-c", "model=\"o3\""])
        );
        let err = host
            .hosted_command(&words(&["codex", "exec", "--json"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("codex app-server"), "{err}");
    }
}
