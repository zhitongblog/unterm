//! Helpers: other agents the conversation's agent hands work to.
//!
//! The agent a person talks to is the hub. When a piece of the work suits
//! another agent CLI on this machine better -- a long coding job for Codex
//! while it works the browser -- it calls `agent_session.delegate`, and this
//! module starts that agent, waits for its turn and hands back its answer.
//!
//! A helper works under the hub's own terms, never its own: the same
//! workspace, the same autonomy level, the same task. What it must ask goes
//! to the person, in the same conversation and the inbox -- the hub cannot
//! answer for it. A helper cannot hand work on in turn, and a hub keeps a
//! few at most. When the hub's turn ends, its helpers are stopped with it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use unterm_mcp::handler::McpHandler;
use unterm_services::agent_session;

/// How long a delegate call waits for the helper's turn, unless it says.
/// `unterm-cli`'s bridge keeps the same numbers for its read timeout.
const WAIT_SECONDS: u64 = 240;
const WAIT_MAX_SECONDS: u64 = 600;
/// Helpers one hub may have at once.
const MAX_HELPERS: usize = 3;
/// The most of a helper's answer handed back (bytes).
const MAX_ANSWER: usize = 24 * 1024;
const POLL: Duration = Duration::from_millis(400);

/// Said to the helper before the work, so it knows its answer is read by
/// another agent and is all that agent gets.
const HELPER_PREAMBLE: &str = "Another agent working for the same person hands you this piece of work. \
Do it here, then end with a short report of what you did and the result: your last message is all \
it sees. If something needs the person's decision, say so in the report rather than guessing.";

#[derive(Clone, Debug)]
struct Helper {
    hub: String,
    agent: String,
    /// What it was asked, first and latest.
    prompt: String,
    order: u64,
    /// How far the delegate call has read its events.
    cursor: usize,
    /// Its words this turn.
    text: String,
    error: Option<String>,
    /// The turn's ending, once it came.
    ended: Option<String>,
    /// Stopped with its hub.
    closed: bool,
}

fn helpers() -> &'static Mutex<HashMap<String, Helper>> {
    static HELPERS: OnceLock<Mutex<HashMap<String, Helper>>> = OnceLock::new();
    HELPERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_order() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static ORDER: AtomicU64 = AtomicU64::new(0);
    ORDER.fetch_add(1, Ordering::Relaxed)
}

/// One helper of a hub, for the console to show.
#[derive(Clone, Debug, PartialEq)]
pub struct HelperRef {
    pub session_id: String,
    pub agent: String,
    pub prompt: String,
}

/// The helpers a hub started, oldest first.
pub fn of(hub: &str) -> Vec<HelperRef> {
    let helpers = helpers().lock().unwrap();
    let mut found: Vec<(&String, &Helper)> = helpers.iter().filter(|(_, h)| h.hub == hub).collect();
    found.sort_by_key(|(_, h)| h.order);
    found
        .into_iter()
        .map(|(id, h)| HelperRef { session_id: id.clone(), agent: h.agent.clone(), prompt: h.prompt.clone() })
        .collect()
}

/// Stop and forget a hub's helpers: its turn is over, and nobody is left to
/// read what they would say.
pub fn release(hub: &str) {
    let ids: Vec<String> = {
        let mut helpers = helpers().lock().unwrap();
        let ids: Vec<String> = helpers.iter().filter(|(_, h)| h.hub == hub).map(|(id, _)| id.clone()).collect();
        for id in &ids {
            helpers.remove(id);
        }
        ids
    };
    for id in ids {
        let running = agent_session::status(&id)
            .ok()
            .is_some_and(|s| s.get("state").and_then(Value::as_str) == Some("running"));
        if running {
            let _ = agent_session::interrupt(&id, 2_000);
        }
        let _ = agent_session::close(&id);
    }
}

/// The agent id a name means. The hub may say "codex" as readily as
/// "codex-cli".
fn agent_id(name: &str) -> Option<&'static str> {
    match name.trim().to_ascii_lowercase().as_str() {
        "codex" | "codex-cli" | "openai-codex" => Some("codex-cli"),
        "claude" | "claude-code" | "claude code" => Some("claude-code"),
        "opencode" => Some("opencode"),
        "agy" | "antigravity" | "google" => Some("agy"),
        _ => None,
    }
}

/// `agent_session.delegate`, for the hosted session `hub`.
pub fn delegate(handler: &McpHandler, hub: &str, params: &Value) -> Result<Value> {
    if helpers().lock().unwrap().contains_key(hub) {
        return Err(anyhow!("a helper cannot hand its work on; do it yourself or report back"));
    }
    let wait = Duration::from_secs(
        params.get("wait_seconds").and_then(Value::as_u64).unwrap_or(WAIT_SECONDS).clamp(1, WAIT_MAX_SECONDS),
    );
    let prompt = params.get("prompt").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty());
    let session = match params.get("session_id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
        Some(earlier) => {
            let helper = helpers()
                .lock()
                .unwrap()
                .get(earlier)
                .filter(|h| h.hub == hub)
                .cloned()
                .ok_or_else(|| anyhow!("{earlier} is not a helper you started in this turn"))?;
            if let Some(prompt) = prompt {
                if helper.ended.is_none() {
                    return Err(anyhow!("helper {earlier} is still on its last piece of work; wait for it first"));
                }
                if helper.closed {
                    return Err(anyhow!("helper {earlier} has stopped; start a new one"));
                }
                agent_session::submit_input(earlier, prompt)?;
                if let Some(h) = helpers().lock().unwrap().get_mut(earlier) {
                    h.text.clear();
                    h.error = None;
                    h.ended = None;
                    h.prompt = prompt.to_string();
                }
            }
            earlier.to_string()
        }
        None => {
            let name = params.get("agent").and_then(Value::as_str).unwrap_or_default();
            let agent = agent_id(name).ok_or_else(|| {
                anyhow!("which agent? one of codex-cli, claude-code, opencode or agy (not {name:?})")
            })?;
            let prompt = prompt.ok_or_else(|| anyhow!("say what the helper should do (prompt)"))?;
            start(handler, hub, agent, prompt)?
        }
    };
    wait_for(&session, wait)
}

fn start(handler: &McpHandler, hub: &str, agent: &str, prompt: &str) -> Result<String> {
    let live = helpers().lock().unwrap().values().filter(|h| h.hub == hub && !h.closed).count();
    if live >= MAX_HELPERS {
        return Err(anyhow!("you already have {MAX_HELPERS} helpers; wait for one and continue it instead"));
    }
    let standing = agent_session::standing(hub).ok_or_else(|| anyhow!("your session is not one Unterm is running"))?;
    let settings = super::console_tasks::run_settings();
    let mut body = json!({
        "agent": agent,
        "prompt": format!("{HELPER_PREAMBLE}\n\n{prompt}"),
        "hosted": true,
        "governed": super::agent_run::supports_governed(agent),
        "browser_profile": settings.browser_profile,
        // Told nothing about helpers of its own: it may not have any.
        "helper": true,
    });
    // Never wider than the hub: an agent that runs nothing unasked cannot
    // get round that by asking a helper.
    body["autonomy"] = json!(standing.autonomy.unwrap_or_default().as_str());
    if let Some(cwd) = &standing.cwd {
        body["cwd"] = json!(cwd);
    }
    let context = &standing.context;
    for (key, value) in [("task_id", &context.task_id), ("run_id", &context.run_id), ("step_id", &context.step_id)] {
        if let Some(value) = value {
            body[key] = json!(value);
        }
    }
    let started = super::agent_run::start_session(handler, &body).map_err(|(_, _, message)| anyhow!(message))?;
    let session = started
        .pointer("/session/session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the helper started but no session id came back"))?
        .to_string();
    helpers().lock().unwrap().insert(
        session.clone(),
        Helper {
            hub: hub.to_string(),
            agent: agent.to_string(),
            prompt: prompt.to_string(),
            order: next_order(),
            cursor: 0,
            text: String::new(),
            error: None,
            ended: None,
            closed: false,
        },
    );
    Ok(session)
}

/// Read the helper's new events into its record. False once it is gone
/// from this process.
fn catch_up(session: &str) -> bool {
    let cursor = match helpers().lock().unwrap().get(session) {
        Some(h) => h.cursor,
        None => return false,
    };
    let read = agent_session::events(session, cursor);
    let mut helpers = helpers().lock().unwrap();
    let Some(helper) = helpers.get_mut(session) else {
        return false;
    };
    match read {
        Ok((events, next)) => {
            helper.cursor = next;
            for event in &events {
                note(helper, event);
            }
            true
        }
        Err(_) => {
            helper.closed = true;
            if helper.ended.is_none() {
                let state = agent_session::status(session)
                    .ok()
                    .and_then(|s| s.get("state").and_then(Value::as_str).map(str::to_string));
                helper.ended = Some(match state.as_deref() {
                    Some("running") | None => "failed".into(),
                    Some(state) => state.to_string(),
                });
            }
            false
        }
    }
}

fn note(helper: &mut Helper, event: &Value) {
    match event.get("event").and_then(Value::as_str).unwrap_or_default() {
        "output.delta" if event.get("stream").and_then(Value::as_str) != Some("reasoning") => {
            if let Some(text) = event.get("text").and_then(Value::as_str) {
                helper.text.push_str(text);
            }
        }
        "error" => {
            if let Some(message) = event.pointer("/error/message").and_then(Value::as_str) {
                helper.error = Some(message.to_string());
            }
        }
        "turn.completed" | "session.closed" => {
            helper.ended = Some(event.get("state").and_then(Value::as_str).unwrap_or("finished").to_string());
            if event["event"] == "session.closed" {
                helper.closed = true;
            }
        }
        _ => {}
    }
}

fn wait_for(session: &str, wait: Duration) -> Result<Value> {
    let deadline = Instant::now() + wait;
    loop {
        catch_up(session);
        let helper = helpers()
            .lock()
            .unwrap()
            .get(session)
            .cloned()
            .ok_or_else(|| anyhow!("helper {session} has stopped"))?;
        if helper.ended.is_some() || Instant::now() >= deadline {
            return Ok(answer(session, &helper));
        }
        std::thread::sleep(POLL);
    }
}

fn answer(session: &str, helper: &Helper) -> Value {
    let waiting_for_person = agent_session::pending_approvals(session).is_ok_and(|p| !p.is_empty());
    let state = match helper.ended.as_deref() {
        None if waiting_for_person => "waiting_for_person",
        None => "working",
        Some("finished" | "exited" | "succeeded") => "done",
        Some("interrupted" | "cancelled") => "stopped",
        Some(_) => "failed",
    };
    let mut reply = helper.text.trim().to_string();
    if reply.len() > MAX_ANSWER {
        let mut cut = reply.len() - MAX_ANSWER;
        while !reply.is_char_boundary(cut) {
            cut += 1;
        }
        reply = format!("…{}", &reply[cut..]);
    }
    let mut out = json!({
        "session_id": session,
        "agent": helper.agent,
        "state": state,
    });
    match state {
        "done" | "failed" | "stopped" => out["reply"] = json!(reply),
        "waiting_for_person" => {
            out["note"] = json!("it is waiting for the person to approve something; call again with this session_id to keep waiting")
        }
        _ => out["note"] = json!("still working; call again with this session_id to keep waiting"),
    }
    if let Some(error) = &helper.error {
        if state != "done" {
            out["error"] = json!(error);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn helper() -> Helper {
        Helper {
            hub: "hub".into(),
            agent: "codex-cli".into(),
            prompt: "fix the tests".into(),
            order: 0,
            cursor: 0,
            text: String::new(),
            error: None,
            ended: None,
            closed: false,
        }
    }

    #[test]
    fn the_hub_may_name_an_agent_the_way_people_do() {
        assert_eq!(agent_id("Codex"), Some("codex-cli"));
        assert_eq!(agent_id("claude"), Some("claude-code"));
        assert_eq!(agent_id("antigravity"), Some("agy"));
        assert_eq!(agent_id("cursor"), None, "no host protocol to drive it with");
    }

    #[test]
    fn a_helpers_words_are_its_answer_and_its_ending_says_how_it_went() {
        let mut h = helper();
        note(&mut h, &json!({"event": "output.delta", "stream": "reasoning", "text": "hmm"}));
        note(&mut h, &json!({"event": "output.delta", "text": "Fixed 3 tests."}));
        assert!(h.ended.is_none());
        note(&mut h, &json!({"event": "turn.completed", "state": "finished"}));
        let out = answer("s1", &h);
        assert_eq!(out["state"], "done");
        assert_eq!(out["reply"], "Fixed 3 tests.");
        assert!(!h.closed, "a finished turn leaves it ready for the next piece of work");

        let mut failed = helper();
        note(&mut failed, &json!({"event": "error", "error": {"message": "not signed in"}}));
        note(&mut failed, &json!({"event": "session.closed", "state": "failed"}));
        let out = answer("s2", &failed);
        assert_eq!(out["state"], "failed");
        assert_eq!(out["error"], "not signed in");
        assert!(failed.closed);
    }

    #[test]
    fn a_helper_still_at_work_says_so_and_how_to_wait() {
        let out = answer("s3", &helper());
        assert_eq!(out["state"], "working");
        assert!(out.get("reply").is_none());
        assert!(out["note"].as_str().unwrap().contains("session_id"));
    }

    #[test]
    fn a_long_answer_keeps_its_end() {
        let mut h = helper();
        h.text = format!("{}THE END", "é".repeat(MAX_ANSWER));
        h.ended = Some("finished".into());
        let reply = answer("s4", &h)["reply"].as_str().unwrap().to_string();
        assert!(reply.ends_with("THE END"));
        assert!(reply.len() <= MAX_ANSWER + 4);
    }

    #[test]
    fn helpers_are_listed_for_their_hub_only_and_released_with_it() {
        {
            let mut all = helpers().lock().unwrap();
            all.insert("t_a".into(), Helper { hub: "t_hub".into(), order: next_order(), ..helper() });
            all.insert("t_b".into(), Helper { hub: "t_hub".into(), order: next_order(), ..helper() });
            all.insert("t_c".into(), Helper { hub: "t_other".into(), order: next_order(), ..helper() });
        }
        let ids: Vec<String> = of("t_hub").into_iter().map(|h| h.session_id).collect();
        assert_eq!(ids, ["t_a", "t_b"]);
        release("t_hub");
        assert!(of("t_hub").is_empty());
        assert_eq!(of("t_other").len(), 1);
        release("t_other");
    }
}
