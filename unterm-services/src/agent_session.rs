//! Hosting a CLI agent and turning it into events somebody else can read.
//!
//! `unterm agent run` used to hand a prompt to `codex exec` and wait for an
//! exit code. A caller who wanted to know what the agent was *doing* had to
//! parse terminal output — which is to say, screen-scrape a program that was
//! already printing structured JSON.
//!
//! This hosts one instead: start it, stream what it did, put input in,
//! interrupt it, ask how it ended. The parsing is [`unterm_brain`]'s; the
//! process handling is its runtime's; what is here is the session — the
//! thing that has an id, an owner and an ending.
//!
//! **The caller's identifiers are carried, never invented.** `task_id`,
//! `run_id`, `step_id`, `idempotency_key` and `lease_id` come from whoever
//! asked for the session and appear on every event and every log line
//! untouched. An id this process made up would correlate with nothing
//! upstream, which is worse than no id: it looks like correlation.
//!
//! **Events live in memory; the ending does not.** A caller streams events
//! while it is watching. "What happened to the agent I started before the
//! Core restarted" is a different question, and it has an answer because the
//! final state is a row.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use unterm_brain::runtime::{self, Running, Spec};
use unterm_brain::host::{self, Decision, HostProtocol};
use unterm_brain::{BrainAdapter, BrainEvent, StopReason};
use unterm_tasks::AgentSession;

/// The shape every event on this stream claims to be.
///
/// `contracts/v0` wants a discriminant on the object itself, not only on the
/// endpoint that served it: an event handed on by itself has to say what it
/// is, because the consumer that receives it second-hand never saw the call
/// it came from.
///
/// The constant `contracts/v0/brain/events.schema.json` pins it to. A string
/// naming the object and its major version, the same convention as
/// `provider_manifest.v0` on the terminal manifest.
const EVENT_SCHEMA_VERSION: &str = "brain_event.v0";

/// A `turn_…` id in the contract's shape: 26 Crockford base32 characters,
/// a millisecond timestamp in the high 48 bits and randomness below it, so
/// ids sort by when the turn began.
///
/// Made here rather than in the adapter on purpose: adapters are pure and
/// reproducible, and an id needs a clock and an RNG. This is the boundary
/// that publishes the contract, so it is the one that mints what the
/// contract requires and the CLI never supplied.
fn new_turn_id() -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let millis = chrono::Utc::now().timestamp_millis().max(0) as u128 & ((1u128 << 48) - 1);
    let random = uuid::Uuid::new_v4().as_u128() & ((1u128 << 80) - 1);
    let mut value = (millis << 80) | random;
    let mut out = [0u8; 26];
    for slot in out.iter_mut().rev() {
        *slot = ALPHABET[(value & 31) as usize];
        value >>= 5;
    }
    format!("turn_{}", std::str::from_utf8(&out).expect("ascii"))
}

/// Which turn the events on this session belong to.
///
/// The CLIs have no turn ids of their own, so one is minted when a turn
/// starts and carried on every event until it ends -- a consumer that is
/// handed one event by itself can still say which turn it was part of.
///
/// Shared, because an approval is answered from outside the reader thread
/// and its `approval.resolved` belongs to the same turn as the question.
#[derive(Clone, Default)]
struct TurnTracker {
    current: Arc<Mutex<Option<String>>>,
}

impl TurnTracker {
    fn stamp(&self, event: &BrainEvent, described: &mut Value) {
        let mut current = self.current.lock().unwrap();
        if matches!(event, BrainEvent::TurnStarted { .. }) {
            *current = Some(new_turn_id());
        }
        Self::put(&current, described);
        if matches!(event, BrainEvent::TurnEnded { .. }) {
            *current = None;
        }
    }

    /// Stamp an event made outside the stream with the turn now open.
    fn stamp_now(&self, described: &mut Value) {
        Self::put(&self.current.lock().unwrap(), described);
    }

    fn put(current: &Option<String>, described: &mut Value) {
        if let (Some(turn), Some(map)) = (current, described.as_object_mut()) {
            map.insert("turn_id".into(), Value::String(turn.clone()));
        }
    }
}

/// How a turn ended, in the contract's terms.
///
/// `tool_request` is a pause -- the turn goes on after the tool runs -- so it
/// is still `running`. A limit is resumable, which is what `interrupted`
/// promises a consumer and `failed` would deny.
fn turn_state(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Completed => "finished",
        StopReason::ToolRequest => "running",
        StopReason::Interrupted | StopReason::Limit => "interrupted",
        StopReason::Error => "failed",
    }
}

/// The contract's `tool_call.arguments` is an object. A CLI that hands over
/// a JSON-encoded string gets it decoded; anything else is wrapped rather
/// than dropped, so nothing the model asked for disappears on the way.
fn tool_arguments(arguments: &Value) -> Value {
    match arguments {
        Value::Object(_) => arguments.clone(),
        Value::String(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(parsed @ Value::Object(_)) => parsed,
            _ => json!({ "input": raw }),
        },
        Value::Null => json!({}),
        other => json!({ "input": other }),
    }
}

/// Where the caller's identifiers come from.
///
/// Every field optional, none of them generated here. What is supplied is
/// carried; what is not is absent rather than filled in.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct TaskContext {
    pub task_id: Option<String>,
    pub run_id: Option<String>,
    pub step_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub lease_id: Option<String>,
}

impl TaskContext {
    fn stamp(&self, event: &mut Value) {
        let Some(map) = event.as_object_mut() else {
            return;
        };
        for (key, value) in [
            ("task_id", &self.task_id),
            ("run_id", &self.run_id),
            ("step_id", &self.step_id),
            ("idempotency_key", &self.idempotency_key),
            ("lease_id", &self.lease_id),
        ] {
            if let Some(value) = value {
                map.insert(key.to_string(), Value::String(value.clone()));
            }
        }
    }

    fn correlation(&self) -> crate::audit_store::Correlation {
        crate::audit_store::Correlation {
            task_id: self.task_id.clone(),
            run_id: self.run_id.clone(),
            step_id: self.step_id.clone(),
            lease_id: self.lease_id.clone(),
            ..Default::default()
        }
    }
}

struct Live {
    running: Running,
    context: TaskContext,
    adapter: String,
    /// Everything the session has said, in order. A cursor into this is how a
    /// request/response protocol streams: the caller asks for what happened
    /// after event N.
    events: Arc<Mutex<Vec<Value>>>,
    reader: Option<std::thread::JoinHandle<()>>,
    /// Set for a hosted session: how to talk back to the CLI.
    host: Option<Arc<dyn HostProtocol>>,
    /// Permission questions the CLI is waiting on, by its request id, with
    /// the input that was asked about (allowing echoes it back unchanged).
    pending: Arc<Mutex<HashMap<String, Value>>>,
    turn: TurnTracker,
}

fn sessions() -> &'static Mutex<HashMap<String, Live>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, Live>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn store() -> Result<Arc<unterm_tasks::TaskStore>> {
    crate::cockpit::fleet_store::tasks().ok_or_else(|| anyhow!("there is no task store"))
}

/// The program a command line actually runs, seen through the launchers in
/// front of it.
///
/// On Windows an npm-installed CLI is a `.cmd` shim and has to be started as
/// `cmd /c claude …`; elsewhere the same agent turns up behind `env`, `npx`,
/// `node …/cli.js` or `sh -c "…"`. Reading only the first word named every
/// one of those `cmd` -- and every session `codex`.
pub fn effective_program(command: &[String]) -> Option<String> {
    fn name_of(word: &str) -> String {
        let last = word.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(word);
        let lower = last.to_ascii_lowercase();
        for suffix in [".exe", ".cmd", ".bat", ".ps1", ".js", ".mjs", ".cjs"] {
            if let Some(stem) = lower.strip_suffix(suffix) {
                return stem.to_string();
            }
        }
        lower
    }
    let mut words: Vec<String> = command.to_vec();
    for _ in 0..8 {
        let first = words.first()?.clone();
        let name = name_of(&first);
        let rest = &words[1..];
        let next: Option<Vec<String>> = match name.as_str() {
            "cmd" => rest
                .iter()
                .position(|arg| matches!(arg.to_ascii_lowercase().as_str(), "/c" | "/k"))
                .map(|at| rest[at + 1..].to_vec()),
            "sh" | "bash" | "zsh" | "dash" => rest
                .iter()
                .position(|arg| arg == "-c" || arg == "-lc")
                .and_then(|at| rest.get(at + 1))
                .map(|line| vec![line.clone()]),
            "pwsh" | "powershell" => rest
                .iter()
                .position(|arg| {
                    let arg = arg.to_ascii_lowercase();
                    arg == "-command" || arg == "-c"
                })
                .map(|at| rest[at + 1..].to_vec()),
            "env" => Some(
                rest.iter()
                    .skip_while(|arg| arg.contains('=') || arg.starts_with('-'))
                    .cloned()
                    .collect(),
            ),
            "npx" | "bunx" | "pnpx" | "node" | "bun" | "deno" => Some(
                rest.iter()
                    .skip_while(|arg| arg.starts_with('-'))
                    .cloned()
                    .collect(),
            ),
            _ => None,
        };
        match next {
            // A single argument holding a whole command line (`cmd /c "claude
            // -p"`, `sh -c '…'`) is split for its first word.
            Some(mut words_after) if !words_after.is_empty() => {
                if words_after.len() == 1 && words_after[0].contains(' ') {
                    words_after = words_after[0]
                        .split_whitespace()
                        .map(|word| word.trim_matches(|c| c == '"' || c == '\'').to_string())
                        .collect();
                }
                words = words_after;
            }
            _ => return Some(name),
        }
    }
    words.first().map(|word| name_of(word))
}

/// Which adapter reads a given command's output.
///
/// `requested` -- the caller naming one -- wins; otherwise the program decides,
/// and a program no adapter knows gets the generic one, which reports its
/// output as output and claims nothing else.
fn adapter_for(command: &[String], requested: Option<&str>) -> Result<(&'static str, Box<dyn BrainAdapter>)> {
    let chosen = match requested {
        Some(name) => name.to_ascii_lowercase(),
        None => {
            let program = effective_program(command).unwrap_or_default();
            if program.contains("claude") {
                "claude".into()
            } else if program.contains("codex") {
                "codex".into()
            } else if program.contains("gemini") {
                "gemini".into()
            } else if program.contains("opencode") {
                "opencode".into()
            } else if program == "agy" || program.contains("antigravity") {
                // Google's agent CLI; checked before "gemini" would matter
                // only if a name held both, but `agy` is exact on purpose:
                // three letters inside another program's name is no signal.
                "agy".into()
            } else {
                "generic".into()
            }
        }
    };
    Ok(match chosen.as_str() {
        "claude" => ("claude", Box::new(unterm_brain::adapters::ClaudeAdapter::new())),
        "codex" => ("codex", Box::new(unterm_brain::adapters::CodexAdapter::new())),
        "gemini" => ("gemini", Box::new(unterm_brain::adapters::GeminiAdapter::new())),
        "opencode" => ("opencode", Box::new(unterm_brain::adapters::OpenCodeAdapter::new())),
        "agy" | "antigravity" => ("agy", Box::new(unterm_brain::adapters::AgyAdapter::new())),
        "generic" => ("generic", Box::new(unterm_brain::adapters::GenericAdapter::new())),
        other => anyhow::bail!("unknown adapter {other:?}: use claude, codex, gemini, opencode, agy or generic"),
    })
}

/// Start hosting an agent.
pub fn start(
    command: &[String],
    cwd: Option<&str>,
    env: &[(String, String)],
    prompt: Option<&str>,
    adapter: Option<&str>,
    context: TaskContext,
) -> Result<String> {
    start_with(command, cwd, env, prompt, adapter, context, false)
}

/// Start an agent that is *driven*, not just watched.
///
/// The CLI is put into its host protocol (for Claude: `--input-format
/// stream-json --permission-prompt-tool stdio`, added only where the command
/// does not already say so) and its stdin stays open. Two things follow that
/// a plain session cannot do:
///
/// - **Permission questions come to the caller.** A tool the CLI would
///   prompt for arrives as `approval.requested` and waits for [`decide`].
///   Nothing is pre-approved and nothing is skipped: the CLI's own rules
///   still auto-allow what they allow, and everything else is asked.
/// - **Follow-up turns.** [`submit_input`] sends the next user message to
///   the same process.
///
/// Only CLIs with a host protocol can be driven; for the others this is an
/// error rather than a session that silently cannot be answered.
pub fn start_hosted(
    command: &[String],
    cwd: Option<&str>,
    env: &[(String, String)],
    prompt: Option<&str>,
    adapter: Option<&str>,
    context: TaskContext,
) -> Result<String> {
    start_with(command, cwd, env, prompt, adapter, context, true)
}

fn start_with(
    command: &[String],
    cwd: Option<&str>,
    env: &[(String, String)],
    prompt: Option<&str>,
    adapter: Option<&str>,
    context: TaskContext,
    hosted: bool,
) -> Result<String> {
    let (adapter_id, mut adapter) = adapter_for(command, adapter)?;
    let host: Option<Arc<dyn HostProtocol>> = if hosted {
        Some(Arc::from(host::host_for(adapter_id).ok_or_else(|| {
            anyhow!(
                "{adapter_id} cannot be driven: no host protocol (hosted sessions support claude, codex, opencode and agy)"
            )
        })?))
    } else {
        None
    };
    let command: Vec<String> = match &host {
        Some(host) => {
            // The mode can read differently from the plain output: `opencode
            // acp` speaks ACP, not `opencode run --format json`.
            if let Some(hosted_adapter) = host.adapter() {
                adapter = hosted_adapter;
            }
            host.hosted_command(command)?
        }
        None => command.to_vec(),
    };
    let program = command
        .first()
        .ok_or_else(|| anyhow!("Missing 'command'"))?
        .clone();

    let mut spec = Spec::new(&program).args(command[1..].to_vec());
    if let Some(cwd) = cwd {
        spec = spec.cwd(cwd);
    }
    if let Some(host) = &host {
        // The mode's own environment first, so the caller's wins: a caller
        // that set OPENCODE_PERMISSION itself meant it.
        for (key, value) in host.hosted_env() {
            if !env.iter().any(|(k, _)| *k == key) {
                spec = spec.env(key, value);
            }
        }
    }
    for (key, value) in env {
        spec = spec.env(key, value);
    }
    match (&host, prompt) {
        (Some(host), prompt) => {
            spec = spec.keep_stdin_open();
            let mut first = host.opening(cwd);
            if let Some(prompt) = prompt {
                first.extend(host.user_message(prompt));
            }
            if !first.is_empty() {
                spec = spec.prompt(first.join("\n"));
            }
        }
        (None, Some(prompt)) => spec = spec.prompt(prompt),
        (None, None) => {}
    }

    let id = format!("ags_{}", uuid::Uuid::new_v4().simple());
    let pending: Arc<Mutex<HashMap<String, Value>>> = Arc::new(Mutex::new(HashMap::new()));
    let turn = TurnTracker::default();
    let tap: Option<runtime::Tap> = host.as_ref().map(|host| {
        let host = Arc::clone(host);
        Box::new(move |line: &str| host.observe(line)) as runtime::Tap
    });
    let mut running = runtime::spawn_tapped(&spec, adapter, tap)?;
    let stream = running.events();
    let events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));

    let session = AgentSession {
        id: id.clone(),
        adapter: adapter_id.to_string(),
        command: command.join(" "),
        cwd: cwd.map(str::to_string),
        task_id: context.task_id.clone(),
        run_id: context.run_id.clone(),
        step_id: context.step_id.clone(),
        idempotency_key: context.idempotency_key.clone(),
        lease_id: context.lease_id.clone(),
        state: "started".into(),
        started_at: chrono::Utc::now().to_rfc3339(),
        ..AgentSession::default()
    };
    store()?.record_agent_session(&session)?;

    push(
        &events,
        &context,
        &id,
        json!({"event": "session.started", "adapter": adapter_id, "hosted": host.is_some()}),
    );

    let reader = std::thread::Builder::new()
        .name("agent-session-events".into())
        .spawn({
            let events = Arc::clone(&events);
            let context = context.clone();
            let id = id.clone();
            let pending = Arc::clone(&pending);
            let turn = turn.clone();
            move || {
                for event in stream {
                    if let BrainEvent::PermissionRequested { request_id, input, .. } = &event {
                        // Recorded before the event is published, so a caller
                        // that answers the moment it sees the question finds
                        // it pending.
                        pending.lock().unwrap().insert(request_id.clone(), input.clone());
                    }
                    let mut described = describe(&event);
                    turn.stamp(&event, &mut described);
                    push(&events, &context, &id, described);
                }
            }
        })
        .ok();

    sessions().lock().unwrap().insert(
        id.clone(),
        Live {
            running,
            context,
            adapter: adapter_id.to_string(),
            events,
            reader,
            host,
            pending,
            turn,
        },
    );
    Ok(id)
}

/// One brain event as this surface's vocabulary.
///
/// The names are the contract's, not the brain crate's and not the CLI's:
/// `tool_call.requested`, `tool_call.result`, `usage.updated`,
/// `turn.completed`, `session.closed`. A caller reading these should not have
/// to know which CLI ran or which crate parsed it -- which is the whole point
/// of not simply forwarding whatever Claude Code or Codex happened to print.
///
/// The shapes are `contracts/v0/brain/events.schema.json`'s. `output.delta`
/// keeps its `stream` discriminator rather than splitting reasoning into an
/// event of its own -- the contract adopted that from here (`unzoo-one#15`).
/// `turn_id` is added by [`TurnTracker`], not here.
fn describe(event: &BrainEvent) -> Value {
    match event {
        // Model is left out rather than null when the stream has not said
        // yet: the contract types it as a string, and "unknown" is not one.
        BrainEvent::TurnStarted { model } => match model {
            Some(model) => json!({"event": "turn.started", "model": model}),
            None => json!({"event": "turn.started"}),
        },
        // `message` duplicates `text` for readers of the older contract
        // wording (unterm#30). The contract's field is `text`; the alias is
        // kept until those readers have moved, and must always equal it.
        BrainEvent::Text { text } => {
            json!({"event": "output.delta", "stream": "assistant", "text": text, "message": text})
        }
        BrainEvent::Reasoning { text } => {
            json!({"event": "output.delta", "stream": "reasoning", "text": text, "message": text})
        }
        BrainEvent::ToolRequested {
            call_id,
            name,
            arguments,
        } => json!({
            "event": "tool_call.requested",
            "tool_call": {
                "tool": name,
                "arguments": tool_arguments(arguments),
                "external_call_id": call_id,
            },
        }),
        BrainEvent::ToolResult {
            call_id,
            ok,
            output,
        } => json!({
            "event": "tool_call.result",
            // Same name as on the request, so the pair correlates by one key.
            "external_call_id": call_id,
            "ok": ok,
            "output": output,
        }),
        BrainEvent::Usage(usage) => json!({"event": "usage.updated", "usage": usage}),
        // `state` is the contract's; `reason` keeps the finer distinction
        // (a limit and an interruption are both `interrupted`).
        BrainEvent::TurnEnded { reason } => json!({
            "event": "turn.completed",
            "state": turn_state(*reason),
            "reason": reason.as_str(),
        }),
        BrainEvent::PermissionRequested {
            request_id,
            tool,
            input,
            description,
        } => {
            let mut approval = json!({
                "request_id": request_id,
                "tool_call": {"tool": tool, "arguments": tool_arguments(input)},
            });
            if let Some(description) = description {
                approval["description"] = Value::String(description.clone());
            }
            json!({"event": "approval.requested", "approval": approval})
        }
        // The CLI upstream has no stable code of its own -- it printed a
        // sentence -- so the code is assigned here, at the boundary that
        // owns the contract. That is the point of the field: a consumer
        // branches on what *this* surface promises, never on prose that
        // belongs to whichever agent happened to be running.
        BrainEvent::Error { message } => {
            let body = unterm_protocol::ErrorBody::new(
                unterm_protocol::ErrorCode::BrainAdapterFailed,
                message,
            );
            json!({
                "event": "error",
                "error": {
                    "code": body.code,
                    "message": body.message,
                    "retryable": body.retryable,
                },
            })
        }
    }
}

fn push(events: &Arc<Mutex<Vec<Value>>>, context: &TaskContext, id: &str, mut event: Value) {
    // Taken before the stamp so the number written into the event is the
    // index the event actually lands at, whatever else happens below.
    let mut sink = events.lock().unwrap();
    let cursor = sink.len();
    if let Some(map) = event.as_object_mut() {
        map.insert("schema_version".into(), json!(EVENT_SCHEMA_VERSION));
        map.insert("session_id".into(), Value::String(id.to_string()));
        // Every event carries the position it was read from, not just the
        // page around it. `events(id, cursor)` hands back a batch and a new
        // cursor, which is enough to poll and no help at all once one event
        // has been passed on by itself -- and being passed on by itself is
        // what events are for.
        map.insert("cursor".into(), json!(cursor));
        map.insert("at".into(), Value::String(chrono::Utc::now().to_rfc3339()));
    }
    context.stamp(&mut event);
    // The same ids reach the trail, so "what did this run do" is answerable
    // from either side.
    crate::audit_store::append_correlated(&event, &context.correlation());
    sink.push(event);
}

/// Everything the session has said after `cursor`.
///
/// A cursor rather than a stream because this is a request/response protocol:
/// a caller polls, and a caller that reconnects asks from where it left off
/// rather than from the beginning.
pub fn events(id: &str, cursor: usize) -> Result<(Vec<Value>, usize)> {
    let sessions = sessions().lock().unwrap();
    let live = sessions
        .get(id)
        .ok_or_else(|| anyhow!("no such session: {id}"))?;
    let events = live.events.lock().unwrap();
    let from = cursor.min(events.len());
    Ok((events[from..].to_vec(), events.len()))
}

fn write_lines(running: &runtime::Running, lines: Vec<String>) -> Result<()> {
    for line in lines {
        running.write_line(&line)?;
    }
    Ok(())
}

/// Put something on the agent's stdin.
pub fn submit_input(id: &str, text: &str) -> Result<()> {
    let sessions = sessions().lock().unwrap();
    let live = sessions
        .get(id)
        .ok_or_else(|| anyhow!("no such session: {id}"))?;
    if let Some(host) = &live.host {
        // A hosted session reads stdin as a stream of user turns.
        return write_lines(&live.running, host.user_message(text));
    }
    // Said plainly rather than silently accepted: a one-shot CLI's stdin is
    // closed after the prompt, and pretending the text went somewhere would
    // leave a caller waiting for a reply to something nobody read.
    Err(anyhow!(
        "this session's stdin was closed after its prompt; start a session without a prompt to keep it open"
    ))
}

/// Answer a permission question the agent is waiting on.
///
/// `allow` runs the tool with exactly the input that was asked about;
/// otherwise `message` reaches the model as the tool's error. Answering a
/// question twice, or one that was never asked, is an error: the CLI is no
/// longer waiting, and pretending the answer went somewhere would hide that.
///
/// `interrupt` (deny only) also ends the turn, so the agent does not go on
/// to ask for the same thing another way.
pub fn decide(
    id: &str,
    request_id: &str,
    allow: bool,
    message: Option<&str>,
    interrupt: bool,
) -> Result<Value> {
    let sessions = sessions().lock().unwrap();
    let live = sessions
        .get(id)
        .ok_or_else(|| anyhow!("no such session: {id}"))?;
    let host = live
        .host
        .as_ref()
        .ok_or_else(|| anyhow!("session {id} is not hosted; nothing in it can be answered"))?;
    let input = live
        .pending
        .lock()
        .unwrap()
        .remove(request_id)
        .ok_or_else(|| anyhow!("no pending approval {request_id} in session {id}: already answered, or never asked"))?;
    let decision = if allow {
        Decision::Allow
    } else {
        Decision::Deny {
            message: message.unwrap_or("denied by the user").to_string(),
            interrupt,
        }
    };
    write_lines(&live.running, host.permission_reply(request_id, &decision, &input))?;
    let mut resolved = resolution(request_id, &decision, "caller");
    live.turn.stamp_now(&mut resolved);
    push(&live.events, &live.context, id, resolved.clone());
    Ok(resolved)
}

/// The `approval.resolved` event for a decision.
///
/// `allow_once` and `deny` are the contract's approval words: this answers
/// one tool call, it grants nothing beyond it.
fn resolution(request_id: &str, decision: &Decision, decided_by: &str) -> Value {
    let mut approval = json!({"request_id": request_id, "decided_by": decided_by});
    match decision {
        Decision::Allow => approval["decision"] = json!("allow_once"),
        Decision::Deny { message, interrupt } => {
            approval["decision"] = json!("deny");
            approval["message"] = json!(message);
            if *interrupt {
                approval["interrupt"] = json!(true);
            }
        }
    }
    json!({"event": "approval.resolved", "approval": approval})
}

/// Approvals still waiting, for a caller that reconnected and lost track.
pub fn pending_approvals(id: &str) -> Result<Vec<String>> {
    let sessions = sessions().lock().unwrap();
    let live = sessions
        .get(id)
        .ok_or_else(|| anyhow!("no such session: {id}"))?;
    let mut ids: Vec<String> = live.pending.lock().unwrap().keys().cloned().collect();
    ids.sort();
    Ok(ids)
}

/// Stop the agent, and everything it started.
pub fn interrupt(id: &str, grace_ms: u64) -> Result<()> {
    let sessions = sessions().lock().unwrap();
    let live = sessions
        .get(id)
        .ok_or_else(|| anyhow!("no such session: {id}"))?;
    live.running
        .interrupt(std::time::Duration::from_millis(grace_ms))?;
    Ok(())
}

/// How the session is doing, or how it ended.
///
/// Answers from the durable record when the session is not in this process —
/// which is what makes the question survivable across a restart.
pub fn status(id: &str) -> Result<Value> {
    if let Some(live) = sessions().lock().unwrap().get(id) {
        let snapshot = live.running.snapshot();
        let mut pending: Vec<String> = live.pending.lock().unwrap().keys().cloned().collect();
        pending.sort();
        return Ok(json!({
            "session_id": id,
            "state": if live.running.is_running() { "running" } else { "finished" },
            "adapter": live.adapter,
            "turns": snapshot.turns,
            // Null when the agent never said, rather than zeros that read as
            // "this cost nothing".
            "usage": if snapshot.usage_reported {
                serde_json::to_value(&snapshot.usage)?
            } else {
                Value::Null
            },
            "hosted": live.host.is_some(),
            // A hosted agent blocked on these is waiting for a person, not
            // working; a status that said only "running" would hide that.
            "pending_approvals": pending,
            "task_id": live.context.task_id,
            "run_id": live.context.run_id,
            "step_id": live.context.step_id,
        }));
    }
    let session = store()?
        .agent_session(id)?
        .ok_or_else(|| anyhow!("no such session: {id}"))?;
    Ok(serde_json::to_value(session)?)
}

/// Wait for the agent to finish, write down how it ended, and forget it.
pub fn close(id: &str) -> Result<Value> {
    let live = sessions()
        .lock()
        .unwrap()
        .remove(id)
        .ok_or_else(|| anyhow!("no such session: {id}"))?;
    let Live {
        running,
        context,
        adapter,
        events,
        reader,
        host,
        pending,
        turn,
    } = live;

    // A question nobody answered is a refusal, said out loud: the CLI would
    // otherwise wait on it forever, and `wait` below with it.
    let mut auto_denied = 0usize;
    let mark = events.lock().unwrap().len();
    if let Some(host) = &host {
        let waiting: Vec<(String, Value)> = pending.lock().unwrap().drain().collect();
        for (request_id, input) in waiting {
            auto_denied += 1;
            let decision = Decision::Deny {
                message: "the session was closed before this was answered".into(),
                interrupt: true,
            };
            let _ = write_lines(&running, host.permission_reply(&request_id, &decision, &input));
            let mut resolved = resolution(&request_id, &decision, "unterm");
            turn.stamp_now(&mut resolved);
            push(&events, &context, id, resolved);
        }
    }
    if auto_denied > 0 {
        // Let the CLI finish the turn it was just told to stop before its
        // input ends. Closing stdin straight after the refusal raced it:
        // live, Claude saw EOF mid-interrupt and exited 1, and a session
        // that was closed on purpose read as one that failed.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let turn_over = events.lock().unwrap()[mark..]
                .iter()
                .any(|event| event["event"] == "turn.completed");
            if turn_over || !running.is_running() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    let snapshot = running.wait()?;
    if let Some(reader) = reader {
        let _ = reader.join();
    }

    // An ending that says which of the three it was. A subprocess that
    // vanished must not read as success.
    let (state, reason) = match snapshot.last_stop {
        _ if snapshot.interrupted => ("interrupted", Some("interrupted".to_string())),
        // Closed with questions open: unterm refused them and stopped the
        // turn. That is an interruption by the host, whatever the exit code
        // of a CLI that was cut off says -- and it says why.
        _ if auto_denied > 0 => (
            "interrupted",
            Some(format!(
                "closed with {auto_denied} unanswered approval request(s); denied and interrupted"
            )),
        ),
        Some(StopReason::Error) => ("failed", runtime::Running::failure_reason(&snapshot)),
        _ if snapshot.exit_code.unwrap_or(0) != 0 => (
            "failed",
            runtime::Running::failure_reason(&snapshot).or_else(|| {
                Some(format!(
                    "the agent exited with {}",
                    snapshot.exit_code.map(|c| c.to_string()).unwrap_or_default()
                ))
            }),
        ),
        _ => ("exited", None),
    };

    // The stored record keeps unterm's own word (`exited`), which status
    // queries and older rows already use; the event speaks the contract's,
    // where a clean ending is `finished`.
    let event_state = match state {
        "exited" => "finished",
        other => other,
    };
    let ending = json!({
        "event": "session.closed",
        "state": event_state,
        "exit_code": snapshot.exit_code,
        "reason": reason,
        "adapter": adapter,
    });
    push(&events, &context, id, ending.clone());

    if let Ok(store) = store() {
        if let Ok(Some(mut session)) = store.agent_session(id) {
            session.state = state.to_string();
            session.exit_code = snapshot.exit_code.map(|code| code as i64);
            session.reason = reason.clone();
            session.ended_at = Some(chrono::Utc::now().to_rfc3339());
            let _ = store.record_agent_session(&session);
        }
    }
    Ok(ending)
}

/// Sessions this process is hosting.
pub fn live_ids() -> Vec<String> {
    let mut ids: Vec<String> = sessions().lock().unwrap().keys().cloned().collect();
    ids.sort();
    ids
}

/// What a previous life left running.
///
/// Called at startup: a session recorded as started whose Core is gone did
/// not succeed and did not fail, and silence reads exactly like success.
pub fn interrupt_orphans() -> Result<Vec<String>> {
    store()?.interrupt_orphan_sessions("the Core that was hosting this session stopped")
}

#[cfg(test)]
mod tests {
    use super::effective_program;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    /// Issue #35: the program behind the launcher picks the adapter, and an
    /// unknown one is generic rather than codex.
    #[test]
    fn the_adapter_follows_the_program_behind_the_launcher() {
        let cases: &[(&[&str], &str)] = &[
            (&["cmd", "/c", "claude", "-p", "--output-format", "stream-json", "--verbose"], "claude"),
            (&["cmd.exe", "/C", "codex", "exec", "--json", "-"], "codex"),
            (&["cmd", "/c", "gemini", "--yolo", "-p", "x"], "gemini"),
            (&["cmd", "/c", "echo", "hello"], "echo"),
            (&["cmd", "/c", "claude -p --verbose"], "claude"),
            (&["env", "FOO=1", "claude", "-p"], "claude"),
            (&["npx", "-y", "@anthropic-ai/claude-code", "-p"], "claude-code"),
            (&["node", "/usr/lib/node_modules/@openai/codex/bin/codex.js", "exec"], "codex"),
            (&["sh", "-c", "codex exec --json -"], "codex"),
            (&["C:\\Users\\me\\AppData\\Roaming\\npm\\claude.cmd", "-p"], "claude"),
            (&["/opt/homebrew/bin/claude", "-p"], "claude"),
        ];
        for (command, expected) in cases {
            assert_eq!(
                effective_program(&argv(command)).as_deref(),
                Some(*expected),
                "{command:?}"
            );
        }
        let pick = |command: &[&str]| super::adapter_for(&argv(command), None).unwrap().0;
        assert_eq!(pick(&["cmd", "/c", "claude", "-p"]), "claude");
        assert_eq!(pick(&["cmd", "/c", "qwen", "--yolo"]), "generic");
        assert_eq!(pick(&["cmd", "/c", "echo", "hello"]), "generic");
        assert_eq!(pick(&["codex", "exec", "--json"]), "codex");
        assert_eq!(pick(&["agy", "-p", "hi", "--output-format", "stream-json"]), "agy");
        assert_eq!(pick(&[r"C:\Users\u\AppData\Local\agy\bin\agy.exe", "-p"]), "agy");
        assert_eq!(pick(&["gemini", "-p", "hi"]), "gemini");
        assert_eq!(pick(&["cmd", "/c", "opencode", "run"]), "opencode");
        // `agy` only as the whole name, never as a fragment of another.
        assert_eq!(pick(&["stagyard"]), "generic");
        assert_eq!(super::adapter_for(&argv(&["anything"]), Some("claude")).unwrap().0, "claude");
        assert!(super::adapter_for(&argv(&["anything"]), Some("bogus")).is_err());
    }

    use super::*;

    fn isolate() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("UNTERM_STATE_DIR", dir.path());
        std::env::set_var("UNTERM_TASKS_DB", dir.path().join("tasks.db"));
        crate::cockpit::fleet_store::reset_for_tests();
        dir
    }

    fn a_codex_like_agent(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    const A_TURN: &str = r#"
printf '%s\n' '{"type":"turn.started","model":"gpt-5"}'
printf '%s\n' '{"type":"agent_message","text":"hello"}'
printf '%s\n' '{"type":"function_call","call_id":"c1","name":"shell","arguments":{"command":"ls"}}'
printf '%s\n' '{"type":"turn.completed"}'
"#;

    #[test]
    fn a_session_reports_what_the_agent_did_without_anyone_parsing_a_terminal() {
        let _dir = isolate();
        let id = start(
            &a_codex_like_agent(A_TURN),
            None,
            &[],
            None,
            Some("codex"),
            TaskContext::default(),
        )
        .unwrap();
        let ending = close(&id).unwrap();
        // The contract's word for a clean ending; the stored row keeps
        // `exited` (see `the_ending_outlives_the_process_that_hosted_it`).
        assert_eq!(ending["state"], "finished");

        // `close` drains the reader, so everything is in by now.
        let (events, _) = replay_of(&id);
        let names: Vec<&str> = events
            .iter()
            .filter_map(|event| event["event"].as_str())
            .collect();
        assert!(names.contains(&"session.started"), "{names:?}");
        assert!(names.contains(&"output.delta"), "{names:?}");
        assert!(names.contains(&"tool_call.requested"), "{names:?}");
        assert!(names.contains(&"session.closed"), "{names:?}");
        // The names the contract does not use must not survive anywhere on
        // this stream: a consumer switching to the new ones would otherwise
        // see both and have to guess which is authoritative.
        for retired in ["tool.requested", "tool.completed", "usage", "turn.ended", "session.exited"] {
            assert!(!names.contains(&retired), "{retired} still emitted: {names:?}");
        }

        // Every event says what it is and where it sat, so one passed on by
        // itself is still locatable.
        for event in &events {
            assert_eq!(
                event["schema_version"], EVENT_SCHEMA_VERSION,
                "{event}"
            );
            assert!(event["cursor"].is_u64(), "{event}");
        }
        let cursors: Vec<u64> = events
            .iter()
            .filter_map(|event| event["cursor"].as_u64())
            .collect();
        let ordered: Vec<u64> = (0..cursors.len() as u64).collect();
        assert_eq!(cursors, ordered, "cursors must be the events' own indices");
    }

    /// The per-event rules of `contracts/v0/brain/events.schema.json`, held
    /// here because this crate has no JSON Schema validator. Set
    /// `UNTERM_CONTRACT_DUMP=<file>` to write the events out and check them
    /// against the schema itself (`contracts/v0` has ajv).
    #[test]
    fn every_event_has_the_contracts_shape() {
        let _dir = isolate();
        let script = r#"
printf '%s\n' '{"type":"turn.started","model":"gpt-5"}'
printf '%s\n' '{"type":"agent_reasoning","text":"look first"}'
printf '%s\n' '{"type":"function_call","call_id":"c1","name":"shell","arguments":"{\"command\":\"ls\"}"}'
printf '%s\n' '{"type":"function_call_output","call_id":"c1","exit_code":0,"output":"a"}'
printf '%s\n' '{"type":"agent_message","text":"hello"}'
printf '%s\n' '{"type":"token_count","input_tokens":3,"output_tokens":2,"cached_tokens":1}'
printf '%s\n' '{"type":"turn.completed"}'
printf '%s\n' 'not json at all'
"#;
        let id = start(
            &a_codex_like_agent(script),
            None,
            &[],
            None,
            Some("codex"),
            TaskContext::default(),
        )
        .unwrap();
        close(&id).unwrap();
        let (events, _) = replay_of(&id);

        if let Ok(path) = std::env::var("UNTERM_CONTRACT_DUMP") {
            std::fs::write(&path, serde_json::to_string_pretty(&events).unwrap()).unwrap();
        }

        let turn_id = |event: &Value| {
            let id = event["turn_id"].as_str().unwrap_or_default().to_string();
            assert!(id.starts_with("turn_") && id.len() == 31, "{event}");
            assert!(
                id[5..].chars().all(|c| "0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(c)),
                "{event}"
            );
            id
        };
        let mut turns = Vec::new();
        for event in &events {
            assert_eq!(event["schema_version"], "brain_event.v0", "{event}");
            let at = event["at"].as_str().unwrap();
            assert!(at.ends_with('Z') || at.ends_with("+00:00"), "{event}");
            match event["event"].as_str().unwrap() {
                "turn.started" => {
                    assert!(event["model"].is_string(), "{event}");
                    turns.push(turn_id(event));
                }
                "output.delta" => {
                    assert!(matches!(event["stream"].as_str(), Some("assistant" | "reasoning")), "{event}");
                    assert!(event["text"].is_string(), "{event}");
                    // The compatibility alias, if present, says the same thing.
                    if let Some(alias) = event.get("message") {
                        assert_eq!(alias, &event["text"], "{event}");
                    }
                    turns.push(turn_id(event));
                }
                "tool_call.requested" => {
                    assert_eq!(event["tool_call"]["tool"], "shell", "{event}");
                    // A JSON-encoded string is decoded, not passed through.
                    assert_eq!(event["tool_call"]["arguments"]["command"], "ls", "{event}");
                    assert_eq!(event["tool_call"]["external_call_id"], "c1", "{event}");
                }
                "tool_call.result" => assert_eq!(event["external_call_id"], "c1", "{event}"),
                "usage.updated" => assert!(event["usage"]["input_tokens"].is_u64(), "{event}"),
                "turn.completed" => {
                    assert_eq!(event["state"], "finished", "{event}");
                    turns.push(turn_id(event));
                }
                "error" => {
                    assert_eq!(event["error"]["code"], "brain.adapter_failed", "{event}");
                    assert!(event["error"]["retryable"].is_boolean(), "{event}");
                    assert!(event.get("turn_id").is_none(), "after the turn ended: {event}");
                }
                "session.closed" => {
                    assert!(matches!(event["state"].as_str(), Some("finished" | "interrupted" | "failed")), "{event}")
                }
                "session.started" => {}
                other => panic!("event the contract does not define: {other}"),
            }
        }
        turns.dedup();
        assert_eq!(turns.len(), 1, "one turn, one id carried throughout: {turns:?}");
        assert!(events.iter().any(|e| e["event"] == "error"), "the junk line was reported");
    }

    /// The real Claude CLI, end to end through this session layer: a tool
    /// call, its result, the answer, the cost. Skipped unless
    /// `UNTERM_LIVE_CLAUDE=1`, because it spends tokens and needs a logged-in
    /// `claude` on PATH. `UNTERM_CONTRACT_DUMP` works here too.
    #[test]
    fn live_claude_session_speaks_the_contract() {
        if std::env::var("UNTERM_LIVE_CLAUDE").as_deref() != Ok("1") {
            eprintln!("skipped: set UNTERM_LIVE_CLAUDE=1 to run against the real claude CLI");
            return;
        }
        let dir = isolate();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("alpha.txt"), "a").unwrap();
        std::fs::write(work.join("beta.txt"), "b").unwrap();

        let command: Vec<String> = [
            "claude", "-p", "--output-format", "stream-json", "--verbose",
            "--model", "haiku", "--allowedTools", "Bash(ls:*)",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let id = start(
            &command,
            Some(work.to_str().unwrap()),
            &[],
            Some("Use the Bash tool to run `ls` in the current directory, then reply with only the number of files."),
            None,
            TaskContext::default(),
        )
        .unwrap();
        let ending = close(&id).unwrap();
        let (events, _) = replay_of(&id);
        if let Ok(path) = std::env::var("UNTERM_CONTRACT_DUMP") {
            std::fs::write(&path, serde_json::to_string_pretty(&events).unwrap()).unwrap();
        }

        let names: Vec<&str> = events.iter().filter_map(|e| e["event"].as_str()).collect();
        eprintln!("events: {names:?}");
        assert_eq!(ending["state"], "finished", "{ending}");
        assert_eq!(ending["adapter"], "claude", "picked from the program name");
        for expected in [
            "session.started", "turn.started", "tool_call.requested", "tool_call.result",
            "output.delta", "usage.updated", "turn.completed", "session.closed",
        ] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
        let request = events.iter().find(|e| e["event"] == "tool_call.requested").unwrap();
        assert_eq!(request["tool_call"]["tool"], "Bash", "{request}");
        let result = events.iter().find(|e| e["event"] == "tool_call.result").unwrap();
        assert_eq!(result["external_call_id"], request["tool_call"]["external_call_id"]);
        let answer: String = events
            .iter()
            .filter(|e| e["event"] == "output.delta" && e["stream"] == "assistant")
            .filter_map(|e| e["text"].as_str())
            .collect();
        // Exactly 2: a looser check let "21" through when the agent ran in
        // the wrong directory.
        assert_eq!(answer.trim(), "2", "two files, the model said: {answer:?}");
    }

    /// The real Claude CLI, *driven*: nothing pre-approved. Writing a file
    /// must come back as a question; allowing it writes the file, a follow-up
    /// turn runs in the same process, and a second session that denies it
    /// leaves no file. Skipped unless `UNTERM_LIVE_CLAUDE=1`.
    #[test]
    fn live_claude_hosted_session_asks_before_writing() {
        if std::env::var("UNTERM_LIVE_CLAUDE").as_deref() != Ok("1") {
            eprintln!("skipped: set UNTERM_LIVE_CLAUDE=1 to run against the real claude CLI");
            return;
        }
        let dir = isolate();
        let run = |name: &str, allow: bool| {
            let work = dir.path().join(name);
            std::fs::create_dir_all(&work).unwrap();
            let command: Vec<String> = ["claude", "-p", "--model", "haiku"].iter().map(|s| s.to_string()).collect();
            let id = start_hosted(
                &command,
                Some(work.to_str().unwrap()),
                &[],
                Some("Use the Bash tool to run exactly this command: echo hi > out.txt . Then reply with only: written"),
                None,
                TaskContext::default(),
            )
            .unwrap();
            let events = wait_for_long(&id, |e| e.iter().any(|e| e["event"] == "approval.requested"));
            let asked = events
                .iter()
                .find(|e| e["event"] == "approval.requested")
                .unwrap_or_else(|| panic!("never asked: {events:#?}"))
                .clone();
            eprintln!("asked: {asked}");
            assert_eq!(asked["approval"]["tool_call"]["tool"], "Bash", "{asked}");
            let request_id = asked["approval"]["request_id"].as_str().unwrap().to_string();
            decide(&id, &request_id, allow, (!allow).then_some("not on this machine"), !allow).unwrap();
            let started = std::time::Instant::now();
            let events = wait_for_long(&id, |e| e.iter().filter(|e| e["event"] == "turn.completed").count() >= 1);
            let names: Vec<&str> = events.iter().filter_map(|e| e["event"].as_str()).collect();
            eprintln!("{name}: answered turn ended after {:?}: {names:?}", started.elapsed());
            (id, work, events)
        };

        // Allowed: the file is written, then a second turn in the same process.
        let (id, work, _) = run("allowed", true);
        assert!(work.join("out.txt").exists(), "allowed write did not happen");
        submit_input(&id, "Now reply with only the word: done").unwrap();
        let events = wait_for_long(&id, |e| e.iter().filter(|e| e["event"] == "turn.completed").count() >= 2);
        let second: String = events
            .iter()
            .rev()
            .take_while(|e| e["event"] != "turn.started")
            .filter(|e| e["event"] == "output.delta" && e["stream"] == "assistant")
            .filter_map(|e| e["text"].as_str())
            .collect();
        assert_eq!(second.trim(), "done", "{events:#?}");
        let ending = close(&id).unwrap();
        assert_eq!(ending["state"], "finished", "{ending}");
        if let Ok(path) = std::env::var("UNTERM_CONTRACT_DUMP") {
            let (all, _) = replay_of(&id);
            std::fs::write(&path, serde_json::to_string_pretty(&all).unwrap()).unwrap();
        }

        // Denied, stopping the turn: no file, the turn ends interrupted (not
        // failed), and the agent does not come back asking another way.
        let started = std::time::Instant::now();
        let (id, work, events) = run("denied", false);
        assert!(!work.join("out.txt").exists(), "denied write happened anyway");
        assert!(started.elapsed() < std::time::Duration::from_secs(60), "denial did not end the turn");
        let completed = events.iter().find(|e| e["event"] == "turn.completed").expect("turn ended");
        assert_eq!(completed["state"], "interrupted", "{completed}");
        assert_eq!(events.iter().filter(|e| e["event"] == "approval.requested").count(), 1, "asked again");
        let resolved = events.iter().find(|e| e["event"] == "approval.resolved").unwrap();
        assert_eq!(resolved["approval"]["interrupt"], true);
        assert!(resolved["turn_id"].is_string(), "{resolved}");
        close(&id).unwrap();
        if let Ok(path) = std::env::var("UNTERM_CONTRACT_DUMP") {
            let (all, _) = replay_of(&id);
            std::fs::write(format!("{path}.denied.json"), serde_json::to_string_pretty(&all).unwrap()).unwrap();
        }
    }

    fn wait_for_long(id: &str, predicate: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            let (events, _) = events(id, 0).unwrap();
            if predicate(&events) || std::time::Instant::now() > deadline {
                return events;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    /// The real Antigravity CLI (`agy`) through this session layer. Skipped
    /// unless `UNTERM_LIVE_AGY=1`; needs one interactive `agy` sign-in first
    /// (headless runs reuse the cached credentials).
    #[test]
    fn live_agy_session_speaks_the_contract() {
        if std::env::var("UNTERM_LIVE_AGY").as_deref() != Ok("1") {
            eprintln!("skipped: set UNTERM_LIVE_AGY=1 to run against the real agy CLI");
            return;
        }
        let dir = isolate();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("alpha.txt"), "a").unwrap();
        std::fs::write(work.join("beta.txt"), "b").unwrap();
        // The prompt goes on the command line: plain-text stdin with `-p` is
        // not documented for agy.
        let command: Vec<String> = [
            "agy", "-p",
            "Run a shell command that lists the files in the current directory, then reply with only the number of files.",
            "--output-format", "stream-json", "--dangerously-skip-permissions",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let id = start(&command, Some(work.to_str().unwrap()), &[], None, None, TaskContext::default()).unwrap();
        let ending = close(&id).unwrap();
        let (events, _) = replay_of(&id);
        if let Ok(path) = std::env::var("UNTERM_CONTRACT_DUMP") {
            std::fs::write(&path, serde_json::to_string_pretty(&events).unwrap()).unwrap();
        }
        let names: Vec<&str> = events.iter().filter_map(|e| e["event"].as_str()).collect();
        eprintln!("events: {names:?}");
        assert_eq!(ending["adapter"], "agy", "{ending}");
        assert_eq!(ending["state"], "finished", "{ending}");
        for expected in ["turn.started", "tool_call.requested", "tool_call.result", "output.delta", "usage.updated", "turn.completed"] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
        let answer: String = events
            .iter()
            .filter(|e| e["event"] == "output.delta" && e["stream"] == "assistant")
            .filter_map(|e| e["text"].as_str())
            .collect();
        assert_eq!(answer.trim(), "2", "two files, the model said: {answer:?}");
    }

    /// The real OpenCode CLI through this session layer, on one of its free
    /// models so no login is needed. Skipped unless `UNTERM_LIVE_OPENCODE=1`;
    /// `UNTERM_LIVE_OPENCODE_MODEL` overrides the model.
    #[test]
    fn live_opencode_session_speaks_the_contract() {
        if std::env::var("UNTERM_LIVE_OPENCODE").as_deref() != Ok("1") {
            eprintln!("skipped: set UNTERM_LIVE_OPENCODE=1 to run against the real opencode CLI");
            return;
        }
        let dir = isolate();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("alpha.txt"), "a").unwrap();
        std::fs::write(work.join("beta.txt"), "b").unwrap();
        let model = std::env::var("UNTERM_LIVE_OPENCODE_MODEL")
            .unwrap_or_else(|_| "opencode/deepseek-v4.1-flash-free".into());

        // npm installs opencode as a .cmd shim on Windows; the session layer
        // sees through `cmd /c` to pick the adapter.
        let mut command: Vec<String> = Vec::new();
        if cfg!(windows) {
            command.extend(["cmd".to_string(), "/c".to_string()]);
        }
        command.extend(
            [
                "opencode", "run", "--format", "json", "--auto", "-m", &model,
                "Use the bash tool to run ls in the current directory, then reply with only the number of files.",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        let id = start(&command, Some(work.to_str().unwrap()), &[], None, None, TaskContext::default()).unwrap();
        let ending = close(&id).unwrap();
        let (events, _) = replay_of(&id);
        if let Ok(path) = std::env::var("UNTERM_CONTRACT_DUMP") {
            std::fs::write(&path, serde_json::to_string_pretty(&events).unwrap()).unwrap();
        }

        let names: Vec<&str> = events.iter().filter_map(|e| e["event"].as_str()).collect();
        eprintln!("events: {names:?}");
        assert_eq!(ending["adapter"], "opencode", "picked through cmd /c: {ending}");
        assert_eq!(ending["state"], "finished", "{ending}");
        for expected in [
            "turn.started", "tool_call.requested", "tool_call.result",
            "output.delta", "usage.updated", "turn.completed",
        ] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
        assert_eq!(names.iter().filter(|n| **n == "turn.completed").count(), 1, "{names:?}");
        let completed = events.iter().find(|e| e["event"] == "turn.completed").unwrap();
        assert_eq!(completed["state"], "finished", "{completed}");
        let answer: String = events
            .iter()
            .filter(|e| e["event"] == "output.delta" && e["stream"] == "assistant")
            .filter_map(|e| e["text"].as_str())
            .collect();
        // Exactly 2: a looser check let "21" through when the agent ran in
        // the wrong directory.
        assert_eq!(answer.trim(), "2", "two files, the model said: {answer:?}");
    }

    /// The real `codex exec --json` (0.167): one turn that runs a command,
    /// read through the session layer. Skipped unless `UNTERM_LIVE_CODEX=1`;
    /// needs a signed-in codex (`codex login`) and spends a few tokens.
    #[test]
    fn live_codex_session_speaks_the_contract() {
        if std::env::var("UNTERM_LIVE_CODEX").as_deref() != Ok("1") {
            eprintln!("skipped: set UNTERM_LIVE_CODEX=1 to run against the real codex CLI");
            return;
        }
        let dir = isolate();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("alpha.txt"), "a").unwrap();
        std::fs::write(work.join("beta.txt"), "b").unwrap();

        // npm installs codex as a .cmd shim on Windows.
        let mut command: Vec<String> = Vec::new();
        if cfg!(windows) {
            command.extend(["cmd".to_string(), "/c".to_string()]);
        }
        // `exec` runs with approval policy `never`. Where Codex's own
        // sandbox is not set up (live, Windows 11, 0.167) that rejects every
        // command -- "blocked by policy" -- and a rejected command never
        // appears in `--json` at all. This test is about reading a turn,
        // not about Codex's sandbox: it runs one `ls` in a throwaway
        // directory, as the OpenCode test does with `--auto`. Asking first
        // is what the hosted session is for.
        command.extend(
            ["codex", "exec", "--json", "--skip-git-repo-check", "--dangerously-bypass-approvals-and-sandbox", "-"]
                .iter()
                .map(|s| s.to_string()),
        );
        let id = start(
            &command,
            Some(work.to_str().unwrap()),
            &[],
            Some("Run a shell command to list the files in the current directory, then reply with only the number of files."),
            None,
            TaskContext::default(),
        )
        .unwrap();
        let ending = close(&id).unwrap();
        let (events, _) = replay_of(&id);
        if let Ok(path) = std::env::var("UNTERM_CONTRACT_DUMP") {
            std::fs::write(&path, serde_json::to_string_pretty(&events).unwrap()).unwrap();
        }

        let names: Vec<&str> = events.iter().filter_map(|e| e["event"].as_str()).collect();
        eprintln!("events: {names:?}");
        assert_eq!(ending["adapter"], "codex", "picked through cmd /c: {ending}");
        assert_eq!(ending["state"], "finished", "{ending}");
        for expected in [
            "turn.started", "tool_call.requested", "tool_call.result",
            "output.delta", "usage.updated", "turn.completed",
        ] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
        let usage = events.iter().find(|e| e["event"] == "usage.updated").unwrap();
        eprintln!("usage: {usage}");
        // Codex says what it is about to do before it does it ("I'll count
        // the files."); the answer is its last message.
        let answer = events
            .iter()
            .filter(|e| e["event"] == "output.delta" && e["stream"] == "assistant")
            .filter_map(|e| e["text"].as_str())
            .last()
            .unwrap_or_default();
        assert_eq!(answer.trim(), "2", "two files, the model said: {answer:?}");
    }

    // The events vector is moved into `close`; for assertions the audit trail
    // is the same sequence, correlated the same way, and it is what a caller
    // would read afterwards anyway.
    fn replay_of(id: &str) -> (Vec<Value>, usize) {
        let events: Vec<Value> = crate::audit_store::recent(usize::MAX)
            .into_iter()
            .filter(|event| event["session_id"] == id)
            .collect();
        let len = events.len();
        (events, len)
    }

    /// A stand-in for `claude` driven over its host protocol: it asks one
    /// permission question, says which answer it got, and takes one
    /// follow-up turn. Lines are the shapes real Claude Code 2.1.319 uses.
    const HOSTED_CLAUDE: &str = r#"
printf '%s\n' '{"type":"system","subtype":"init","model":"m","session_id":"s1"}'
read -r prompt
printf '%s\n' '{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"echo hi > out.txt"},"description":"Write hi"}}'
read -r reply
case "$reply" in
  *'"allow"'*) printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"allowed"}]}}' ;;
  *) printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"denied"}]}}' ;;
esac
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":1,"output_tokens":1}}'
read -r followup || exit 0
printf '%s\n' '{"type":"system","subtype":"init","model":"m","session_id":"s1"}'
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"second turn"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":1,"output_tokens":1}}'
"#;

    fn hosted_fake() -> String {
        let id = start_hosted(
            &a_codex_like_agent(HOSTED_CLAUDE),
            None,
            &[],
            Some("write the file"),
            Some("claude"),
            TaskContext::default(),
        )
        .unwrap();
        id
    }

    /// Poll the live stream until `predicate` holds, or give up.
    fn wait_for(id: &str, predicate: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let (events, _) = events(id, 0).unwrap();
            if predicate(&events) || std::time::Instant::now() > deadline {
                return events;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    fn said(events: &[Value], text: &str) -> bool {
        events.iter().any(|e| e["event"] == "output.delta" && e["text"] == text)
    }

    #[test]
    fn a_hosted_session_asks_and_runs_only_what_was_allowed() {
        let _dir = isolate();
        let id = hosted_fake();
        let events = wait_for(&id, |e| e.iter().any(|e| e["event"] == "approval.requested"));
        let asked = events.iter().find(|e| e["event"] == "approval.requested").expect("asked");
        assert_eq!(asked["approval"]["request_id"], "r1", "{asked}");
        assert_eq!(asked["approval"]["tool_call"]["tool"], "Bash", "{asked}");
        assert_eq!(asked["approval"]["tool_call"]["arguments"]["command"], "echo hi > out.txt");
        assert!(asked["turn_id"].is_string(), "inside the turn: {asked}");
        assert_eq!(pending_approvals(&id).unwrap(), ["r1"]);
        assert_eq!(status(&id).unwrap()["pending_approvals"], json!(["r1"]));

        let resolved = decide(&id, "r1", true, None, false).unwrap();
        assert_eq!(resolved["approval"]["decision"], "allow_once");
        let events = wait_for(&id, |e| said(e, "allowed"));
        assert!(said(&events, "allowed"), "{events:#?}");
        // Answering twice is an error, not a second reply nobody reads.
        assert!(decide(&id, "r1", true, None, false).is_err());

        // The same process takes a follow-up turn.
        submit_input(&id, "and again").unwrap();
        let events = wait_for(&id, |e| said(e, "second turn"));
        assert!(said(&events, "second turn"), "{events:#?}");
        let turns: Vec<&Value> = events.iter().filter(|e| e["event"] == "turn.started").collect();
        assert_eq!(turns.len(), 2);
        assert_ne!(turns[0]["turn_id"], turns[1]["turn_id"], "each turn its own id");

        let ending = close(&id).unwrap();
        assert_eq!(ending["state"], "finished", "{ending}");
    }

    #[test]
    fn a_denied_tool_is_not_run_and_the_model_is_told() {
        let _dir = isolate();
        let id = hosted_fake();
        wait_for(&id, |e| e.iter().any(|e| e["event"] == "approval.requested"));
        let resolved = decide(&id, "r1", false, Some("not on this machine"), false).unwrap();
        assert_eq!(resolved["approval"]["decision"], "deny");
        assert_eq!(resolved["approval"]["message"], "not on this machine");
        let events = wait_for(&id, |e| said(e, "denied"));
        assert!(said(&events, "denied"), "{events:#?}");
        close(&id).unwrap();
    }

    #[test]
    fn closing_with_a_question_open_denies_it_instead_of_hanging() {
        let _dir = isolate();
        let id = hosted_fake();
        wait_for(&id, |e| e.iter().any(|e| e["event"] == "approval.requested"));
        // Nobody answers. Close must not wait forever on a CLI that is
        // waiting on us.
        let ending = close(&id).unwrap();
        assert_eq!(ending["state"], "interrupted", "{ending}");
        assert!(ending["reason"].as_str().unwrap().contains("1 unanswered"), "{ending}");
        let (events, _) = replay_of(&id);
        let resolved = events.iter().find(|e| e["event"] == "approval.resolved").expect("resolved");
        assert_eq!(resolved["approval"]["decision"], "deny");
        assert_eq!(resolved["approval"]["decided_by"], "unterm");
        assert!(said(&events, "denied"), "the model heard the refusal: {events:#?}");
    }

    /// An ACP agent in the shape OpenCode 1.19.5 speaks over `opencode acp`.
    /// It checks what the host sends: the handshake, then a prompt carrying
    /// the session id it was given -- a prompt sent before the session
    /// existed would have no id to carry.
    const HOSTED_ACP: &str = r#"
id_of() { printf '%s' "$1" | sed 's/.*"id":\([0-9]*\).*/\1/'; }
read -r init
case "$init" in *'"initialize"'*) ;; *) exit 3 ;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'
read -r new
case "$new" in *'"session/new"'*) ;; *) exit 4 ;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"s1","configOptions":[{"category":"model","currentValue":"m"}]}}'
read -r prompt
case "$prompt" in *'"sessionId":"s1"'*) ;; *) exit 5 ;; esac
turn=$(id_of "$prompt")
case "$OPENCODE_PERMISSION" in *'"bash":"ask"'*) perm=asks ;; *) perm=allows ;; esac
printf '%s\n' "{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"update\":{\"sessionUpdate\":\"agent_thought_chunk\",\"content\":{\"type\":\"text\",\"text\":\"bash $perm\"}}}}"
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"c1","title":"bash","kind":"execute","status":"pending","rawInput":{}}}}'
printf '%s\n' '{"jsonrpc":"2.0","id":0,"method":"session/request_permission","params":{"sessionId":"s1","toolCall":{"toolCallId":"c1","title":"echo hi > out.txt","kind":"execute","rawInput":{"command":"echo hi > out.txt"}},"options":[{"optionId":"once","kind":"allow_once"},{"optionId":"always","kind":"allow_always"},{"optionId":"reject","kind":"reject_once"}]}}'
read -r reply
case "$reply" in
  *'"optionId":"once"'*)
    printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"completed","content":[{"type":"content","content":{"type":"text","text":"(no output)"}}]}}}'
    printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"allowed"}}}}'
    printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":$turn,\"result\":{\"stopReason\":\"end_turn\",\"usage\":{\"inputTokens\":3,\"outputTokens\":1,\"cachedReadTokens\":10}}}" ;;
  *'"optionId":"reject"'*)
    read -r cancel
    case "$cancel" in *'"session/cancel"'*) ;; *) exit 6 ;; esac
    printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"failed","content":[{"type":"content","content":{"type":"text","text":"Tool execution aborted"}}]}}}'
    printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":$turn,\"result\":{\"stopReason\":\"cancelled\"}}" ;;
  *) exit 7 ;;
esac
read -r followup || exit 0
turn=$(id_of "$followup")
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"second turn"}}}}'
printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":$turn,\"result\":{\"stopReason\":\"end_turn\"}}"
read -r rest || exit 0
"#;

    fn hosted_acp_fake() -> String {
        start_hosted(
            &a_codex_like_agent(HOSTED_ACP),
            None,
            &[],
            Some("write the file"),
            Some("opencode"),
            TaskContext::default(),
        )
        .unwrap()
    }

    #[test]
    fn an_acp_session_asks_runs_what_was_allowed_and_takes_a_second_turn() {
        let _dir = isolate();
        let id = hosted_acp_fake();
        let events = wait_for(&id, |e| e.iter().any(|e| e["event"] == "approval.requested"));
        assert!(said(&events, "bash asks"), "OpenCode set to ask: {events:#?}");
        let asked = events.iter().find(|e| e["event"] == "approval.requested").expect("asked");
        assert_eq!(asked["approval"]["request_id"], "0", "{asked}");
        assert_eq!(asked["approval"]["tool_call"]["tool"], "bash", "{asked}");
        assert_eq!(asked["approval"]["tool_call"]["arguments"]["command"], "echo hi > out.txt");
        let requested = events.iter().find(|e| e["event"] == "tool_call.requested").expect("requested");
        assert_eq!(requested["tool_call"]["arguments"]["command"], "echo hi > out.txt", "{requested}");
        assert_eq!(pending_approvals(&id).unwrap(), ["0"]);

        decide(&id, "0", true, None, false).unwrap();
        let events = wait_for(&id, |e| e.iter().any(|e| e["event"] == "turn.completed"));
        assert!(said(&events, "allowed"), "{events:#?}");
        let done = events.iter().find(|e| e["event"] == "turn.completed").unwrap();
        assert_eq!(done["state"], "finished", "{done}");
        assert!(events.iter().any(|e| e["event"] == "usage.updated"), "{events:#?}");

        submit_input(&id, "and again").unwrap();
        let events = wait_for(&id, |e| said(e, "second turn") && e.iter().filter(|e| e["event"] == "turn.completed").count() == 2);
        let turns: Vec<&Value> = events.iter().filter(|e| e["event"] == "turn.started").collect();
        assert_eq!(turns.len(), 2, "{events:#?}");
        assert_ne!(turns[0]["turn_id"], turns[1]["turn_id"]);

        let ending = close(&id).unwrap();
        assert_eq!(ending["state"], "finished", "{ending}");
    }

    #[test]
    fn an_acp_refusal_with_interrupt_cancels_the_turn() {
        let _dir = isolate();
        let id = hosted_acp_fake();
        wait_for(&id, |e| e.iter().any(|e| e["event"] == "approval.requested"));
        decide(&id, "0", false, Some("not here"), true).unwrap();
        let events = wait_for(&id, |e| e.iter().any(|e| e["event"] == "turn.completed"));
        let done = events.iter().find(|e| e["event"] == "turn.completed").expect("ended");
        assert_eq!(done["state"], "interrupted", "{events:#?}");
        assert!(!said(&events, "allowed"));
        close(&id).unwrap();
    }

    #[test]
    fn closing_an_acp_session_with_a_question_open_cancels_it() {
        let _dir = isolate();
        let id = hosted_acp_fake();
        wait_for(&id, |e| e.iter().any(|e| e["event"] == "approval.requested"));
        let ending = close(&id).unwrap();
        assert_eq!(ending["state"], "interrupted", "{ending}");
        assert!(ending["reason"].as_str().unwrap().contains("1 unanswered"), "{ending}");
    }

    #[test]
    fn only_a_cli_with_a_host_protocol_can_be_driven() {
        let _dir = isolate();
        let err = start_hosted(&a_codex_like_agent("true"), None, &[], None, Some("generic"), TaskContext::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot be driven"), "{err}");
        // And a plain session still refuses input it cannot deliver.
        let id = start(&a_codex_like_agent(A_TURN), None, &[], None, Some("codex"), TaskContext::default()).unwrap();
        assert!(submit_input(&id, "x").is_err());
        assert!(decide(&id, "r1", true, None, false).is_err());
        close(&id).unwrap();
    }

    #[test]
    fn the_callers_identifiers_appear_on_every_event() {
        // Not invented here: an id this process made up correlates with
        // nothing upstream, which is worse than no id because it looks like
        // correlation.
        let _dir = isolate();
        let context = TaskContext {
            task_id: Some("tsk_upstream".into()),
            run_id: Some("run_upstream".into()),
            step_id: Some("stp_upstream".into()),
            idempotency_key: Some("idem-1".into()),
            lease_id: Some("lse_1".into()),
        };
        let id = start(&a_codex_like_agent(A_TURN), None, &[], None, Some("codex"), context).unwrap();
        close(&id).unwrap();

        let (events, count) = replay_of(&id);
        assert!(count >= 3, "{events:#?}");
        for event in &events {
            assert_eq!(event["task_id"], "tsk_upstream", "{event}");
            assert_eq!(event["run_id"], "run_upstream", "{event}");
            assert_eq!(event["step_id"], "stp_upstream", "{event}");
        }
    }

    #[test]
    fn an_agent_that_dies_is_reported_as_failed_with_a_reason() {
        // Not silently gone: a subprocess that vanished must not read as
        // success.
        let _dir = isolate();
        let id = start(
            &a_codex_like_agent("echo 'no credentials' >&2; exit 3"),
            None,
            &[],
            None,
            Some("codex"),
            TaskContext::default(),
        )
        .unwrap();
        let ending = close(&id).unwrap();
        assert_eq!(ending["state"], "failed");
        assert_eq!(ending["exit_code"], 3);
        assert!(ending["reason"].as_str().unwrap().contains("no credentials"));
    }

    #[test]
    #[cfg(unix)]
    fn interrupt_reaches_the_process_and_the_session_says_so() {
        let _dir = isolate();
        let id = start(
            &a_codex_like_agent(
                "printf '%s\\n' '{\"type\":\"turn.started\",\"model\":\"m\"}'; sleep 30",
            ),
            None,
            &[],
            None,
            Some("codex"),
            TaskContext::default(),
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(150));
        interrupt(&id, 200).unwrap();
        let ending = close(&id).unwrap();
        assert_eq!(ending["state"], "interrupted", "{ending}");
    }

    #[test]
    fn the_ending_outlives_the_process_that_hosted_it() {
        // "What happened to the agent I started before the Core restarted" is
        // a question with an answer.
        let _dir = isolate();
        let id = start(
            &a_codex_like_agent(A_TURN),
            None,
            &[],
            None,
            Some("codex"),
            TaskContext {
                task_id: Some("tsk_1".into()),
                ..TaskContext::default()
            },
        )
        .unwrap();
        close(&id).unwrap();

        // Nothing live any more — exactly the situation after a restart.
        assert!(!live_ids().contains(&id));
        let status = status(&id).unwrap();
        assert_eq!(status["state"], "exited");
        assert_eq!(status["task_id"], "tsk_1");
    }

    #[test]
    fn a_session_a_dead_core_left_behind_is_interrupted_not_forgotten() {
        let _dir = isolate();
        let store = crate::cockpit::fleet_store::tasks().unwrap();
        store
            .record_agent_session(&unterm_tasks::AgentSession {
                id: "ags_orphan".into(),
                adapter: "codex".into(),
                command: "codex exec".into(),
                state: "started".into(),
                started_at: chrono::Utc::now().to_rfc3339(),
                ..Default::default()
            })
            .unwrap();

        let interrupted = interrupt_orphans().unwrap();
        assert_eq!(interrupted, vec!["ags_orphan".to_string()]);
        let status = status("ags_orphan").unwrap();
        assert_eq!(status["state"], "interrupted");
        assert!(status["reason"].as_str().unwrap().contains("stopped"));
    }

    #[test]
    fn a_session_nobody_started_is_an_error_not_an_empty_answer() {
        let _dir = isolate();
        for result in [
            status("ags_invented").err(),
            events("ags_invented", 0).err(),
            interrupt("ags_invented", 10).err(),
        ] {
            assert!(result.is_some());
        }
    }
}
