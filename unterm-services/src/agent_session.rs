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
use unterm_brain::{BrainAdapter, BrainEvent, StopReason};
use unterm_tasks::AgentSession;

/// The shape every event on this stream claims to be.
///
/// `contracts/v0` wants a discriminant on the object itself, not only on the
/// endpoint that served it: an event handed on by itself has to say what it
/// is, because the consumer that receives it second-hand never saw the call
/// it came from.
///
/// A number rather than a string, matching the one the terminal manifest
/// already publishes, so a reader has one convention to learn rather than
/// two. Worth confirming against the schema before the contract freezes.
const EVENT_SCHEMA_VERSION: u32 = 1;

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
            } else {
                "generic".into()
            }
        }
    };
    Ok(match chosen.as_str() {
        "claude" => ("claude", Box::new(unterm_brain::adapters::ClaudeAdapter::new())),
        "codex" => ("codex", Box::new(unterm_brain::adapters::CodexAdapter::new())),
        "generic" => ("generic", Box::new(unterm_brain::adapters::GenericAdapter::new())),
        other => anyhow::bail!("unknown adapter {other:?}: use claude, codex or generic"),
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
    let program = command
        .first()
        .ok_or_else(|| anyhow!("Missing 'command'"))?
        .clone();
    let (adapter_id, adapter) = adapter_for(command, adapter)?;

    let mut spec = Spec::new(&program).args(command[1..].to_vec());
    if let Some(cwd) = cwd {
        spec = spec.cwd(cwd);
    }
    for (key, value) in env {
        spec = spec.env(key, value);
    }
    if let Some(prompt) = prompt {
        spec = spec.prompt(prompt);
    }

    let id = format!("ags_{}", uuid::Uuid::new_v4().simple());
    let mut running = runtime::spawn(&spec, adapter)?;
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

    push(&events, &context, &id, json!({"event": "session.started", "adapter": adapter_id}));

    let reader = std::thread::Builder::new()
        .name("agent-session-events".into())
        .spawn({
            let events = Arc::clone(&events);
            let context = context.clone();
            let id = id.clone();
            move || {
                for event in stream {
                    push(&events, &context, &id, describe(&event));
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
/// `output.delta` keeps its `stream` discriminator rather than splitting
/// reasoning into an event of its own. That is a deliberate divergence and
/// the one place this file knowingly disagrees with `contracts/v0`: a
/// consumer that wants to render a transcript in order needs the two
/// interleaved, and two event names make that the consumer's problem. Raised
/// for review on the contract side rather than settled here.
fn describe(event: &BrainEvent) -> Value {
    match event {
        BrainEvent::TurnStarted { model } => json!({"event": "turn.started", "model": model}),
        BrainEvent::Text { text } => {
            json!({"event": "output.delta", "stream": "assistant", "text": text})
        }
        BrainEvent::Reasoning { text } => {
            json!({"event": "output.delta", "stream": "reasoning", "text": text})
        }
        BrainEvent::ToolRequested {
            call_id,
            name,
            arguments,
        } => json!({
            "event": "tool_call.requested",
            "tool_call_id": call_id,
            "name": name,
            "arguments": arguments,
        }),
        BrainEvent::ToolResult {
            call_id,
            ok,
            output,
        } => json!({
            "event": "tool_call.result",
            "tool_call_id": call_id,
            "ok": ok,
            "output": output,
        }),
        BrainEvent::Usage(usage) => json!({"event": "usage.updated", "usage": usage}),
        BrainEvent::TurnEnded { reason } => {
            json!({"event": "turn.completed", "reason": reason.as_str()})
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
                "code": body.code,
                "message": body.message,
                "retryable": body.retryable,
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

/// Put something on the agent's stdin.
pub fn submit_input(id: &str, _text: &str) -> Result<()> {
    let sessions = sessions().lock().unwrap();
    sessions
        .get(id)
        .ok_or_else(|| anyhow!("no such session: {id}"))?;
    // Said plainly rather than silently accepted: a one-shot CLI's stdin is
    // closed after the prompt, and pretending the text went somewhere would
    // leave a caller waiting for a reply to something nobody read.
    Err(anyhow!(
        "this session's stdin was closed after its prompt; start a session without a prompt to keep it open"
    ))
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
    } = live;

    let snapshot = running.wait()?;
    if let Some(reader) = reader {
        let _ = reader.join();
    }

    // An ending that says which of the three it was. A subprocess that
    // vanished must not read as success.
    let (state, reason) = match snapshot.last_stop {
        _ if snapshot.interrupted => ("interrupted", Some("interrupted".to_string())),
        Some(StopReason::Error) => ("failed", runtime::Running::failure_reason(&snapshot)),
        _ if snapshot.exit_code.unwrap_or(0) != 0 => {
            ("failed", runtime::Running::failure_reason(&snapshot))
        }
        _ => ("exited", None),
    };

    let ending = json!({
        "event": "session.closed",
        "state": state,
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
        assert_eq!(ending["state"], "exited");

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
