//! A paired phone's view of the product's conversations: the ones running,
//! the questions agents are waiting on, the handovers, and a way to start or
//! continue a conversation. Served by the remote gateway (`remote.rs`) under
//! `/api/one/`, behind the same LAN gate and device token as everything else
//! there; the console's own routes stay on the loopback port.
//!
//! Every answer goes through the same functions the console's routes use, so
//! a phone can do nothing the console cannot, and nothing the console would
//! refuse.

use serde_json::{json, Value};
use unterm_mcp::handler::{ConnectionContext, McpHandler};

use crate::server::Response;

/// The most conversations the phone lists.
const MAX_CONVERSATIONS: usize = 40;
/// The longest thing a phone may ask for.
const MAX_TEXT: usize = 20_000;

/// Whether a device paired with `scope` may change anything (start, answer,
/// stop). A read-only device only watches.
fn may_act(scope: &str) -> bool {
    crate::harness::normalize_scope(Some(scope)) != "readonly"
}

fn body_json(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

fn text_field(body: &Value, key: &str) -> Option<String> {
    body.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn read_only() -> Response {
    Response::err(
        403,
        "Forbidden",
        "this phone was paired to watch only; pair it again with full access to answer or send",
    )
}

/// A route under `/api/one/`, or `None` when there is no such route.
pub(crate) fn route(
    handler: &McpHandler,
    scope: &str,
    method: &str,
    path: &str,
    body: &[u8],
) -> Option<Response> {
    let acts = matches!(
        path,
        "/api/one/send" | "/api/one/decide" | "/api/one/handover" | "/api/one/stop"
    );
    if acts && !may_act(scope) {
        return Some(read_only());
    }
    Some(match (method, path) {
        ("GET", "/api/one/overview") => Response::ok_json(overview(scope)),
        ("POST", "/api/one/thread") => crate::console_tasks::api_thread(body),
        ("POST", "/api/one/send") => send(handler, &body_json(body)),
        ("POST", "/api/one/decide") => decide(handler, &body_json(body)),
        ("POST", "/api/one/handover") => crate::console_tasks::api_handover(body),
        ("POST", "/api/one/stop") => crate::console_tasks::api_stop(body),
        _ => return None,
    })
}

/// Everything the phone's front page shows, in one read. Reading it counts
/// as someone watching: a question from an outside client waits for this
/// phone instead of being refused for want of a window.
fn overview(scope: &str) -> Value {
    unterm_mcp::handler::note_console_watching();
    let tasks = crate::console_tasks::list().unwrap_or_default();
    let title_of = |task_id: &str| -> Value {
        let id = task_id.trim_start_matches("tsk_");
        tasks
            .iter()
            .find(|t| t["id"] == id)
            .map(|t| t["title"].clone())
            .unwrap_or(Value::Null)
    };
    let conversations: Vec<Value> = tasks
        .iter()
        .take(MAX_CONVERSATIONS)
        .map(|task| {
            let latest = &task["runs"][0];
            let at = [&latest["ended_at"], &latest["started_at"], &task["updated_at"]]
                .into_iter()
                .find(|v| !v.is_null())
                .cloned()
                .unwrap_or(Value::Null);
            json!({
                "id": task["id"],
                "title": task["title"],
                "state": latest.get("state").cloned().unwrap_or(Value::Null),
                "waiting": latest.get("waiting").cloned().unwrap_or(json!(false)),
                "at": at,
                "summary": latest.get("summary").cloned().unwrap_or(Value::Null),
                "scheduled": !task["schedule"].is_null(),
            })
        })
        .collect();
    let approvals: Vec<Value> = unterm_services::agent_session::pending_requests()
        .into_iter()
        .map(|entry| {
            let approval = &entry["approval"];
            let task_id = entry["task_id"].as_str().unwrap_or("").to_string();
            let task_ref = if task_id.is_empty() {
                Value::Null
            } else {
                json!(task_id.trim_start_matches("tsk_"))
            };
            json!({
                "kind": "agent",
                "session_id": entry["session_id"],
                "request_id": approval["request_id"],
                "agent": entry["adapter"],
                "tool": approval["tool_call"]["tool"],
                "description": approval["description"],
                "detail": argument_preview(&approval["tool_call"]["arguments"]),
                "task_id": task_ref,
                "title": title_of(&task_id),
                "at": entry["at"],
            })
        })
        .collect();
    let confirmations: Vec<Value> = unterm_mcp::handler::pending_confirmations_list()
        .into_iter()
        .filter_map(|record| serde_json::to_value(record).ok())
        .map(|record| {
            json!({
                "kind": "input",
                "id": record["id"],
                "agent": record["agent"],
                "method": record["method"],
                "detail": record["input_preview"],
                "risk": record["risk"],
                "at": record["requested_at"],
            })
        })
        .collect();
    let handovers = crate::console_tasks::waiting_handovers().unwrap_or_default();
    json!({
        "product": unterm_protocol::product_name(),
        "can_act": may_act(scope),
        "conversations": conversations,
        "approvals": approvals,
        "confirmations": confirmations,
        "handovers": handovers,
    })
}

/// The part of a tool call a person decides on: the command, the file or the
/// address, short enough for a phone.
fn argument_preview(arguments: &Value) -> Value {
    let pick = ["command", "cmd", "file_path", "path", "url", "query", "prompt", "input"]
        .iter()
        .find_map(|key| arguments.get(*key).and_then(Value::as_str).map(str::to_string));
    let text = match pick {
        Some(text) => text,
        None if arguments.is_null() => return Value::Null,
        None => arguments.to_string(),
    };
    let mut short: String = text.chars().take(400).collect();
    if short.len() < text.len() {
        short.push('…');
    }
    json!(short)
}

/// The first line of a request, short: what the console would call the
/// conversation too.
fn title_from(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(text);
    let mut title: String = line.chars().take(48).collect();
    if title.chars().count() < line.chars().count() {
        title.push('…');
    }
    title
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// `{text, task_id?}`: start a conversation, or say more in one. The new
/// conversation is kept the way the console keeps its own, so it shows up
/// there as if it had been typed there.
fn send(handler: &McpHandler, body: &Value) -> Response {
    let Some(text) = text_field(body, "text") else {
        return Response::err(400, "Bad Request", "text is required");
    };
    if text.chars().count() > MAX_TEXT {
        return Response::err(400, "Bad Request", "that is too long to send from a phone");
    }
    let now = now_ms();
    let run_id = format!("run-{now}");
    if let Some(task_id) = text_field(body, "task_id") {
        let run = json!({
            "task_id": task_id,
            "run_id": run_id,
            "prompt": text,
            "follow_up": true,
        });
        return crate::console_tasks::api_run(handler, run.to_string().as_bytes());
    }
    let id = format!("task-{now}");
    let title = title_from(&text);
    let put = json!({
        "id": id,
        "title": title,
        "prompt": text,
        "console": {
            "id": id,
            "title": title,
            "description": text,
            "target": "",
            "status": "运行中",
            "progress": 3,
            "updated": "",
            "schedule": "仅运行一次",
            "requiresApproval": true,
            "retryCount": 0,
        },
    });
    let stored = crate::console_tasks::api_put(put.to_string().as_bytes());
    if stored.status != 200 {
        return stored;
    }
    let run = json!({"task_id": id, "run_id": run_id});
    let started = crate::console_tasks::api_run(handler, run.to_string().as_bytes());
    if started.status != 200 {
        return started;
    }
    let mut reply: Value = serde_json::from_slice(&started.body).unwrap_or_else(|_| json!({}));
    reply["task_id"] = json!(id);
    Response::ok_json(reply)
}

/// `{kind: "agent", session_id, request_id, allow}` for a question from an
/// agent the product runs, `{kind: "input", id, allow}` for one from an
/// outside client. A refusal from the phone also stops the turn, as it does
/// in the console: otherwise the agent tends to ask again another way.
fn decide(handler: &McpHandler, body: &Value) -> Response {
    let Some(allow) = body.get("allow").and_then(Value::as_bool) else {
        return Response::err(400, "Bad Request", "allow is required: true or false, never assumed");
    };
    match body.get("kind").and_then(Value::as_str) {
        Some("agent") => {
            let mut answer = json!({
                "session_id": body.get("session_id").cloned().unwrap_or(Value::Null),
                "request_id": body.get("request_id").cloned().unwrap_or(Value::Null),
                "allow": allow,
            });
            if !allow {
                answer["interrupt"] = json!(true);
                answer["message"] = json!("Refused from the phone.");
            }
            crate::agent_run::api_decide(handler, answer.to_string().as_bytes())
        }
        Some("input") => {
            let Some(id) = body.get("id").and_then(Value::as_u64) else {
                return Response::err(400, "Bad Request", "id is required");
            };
            let ctx = ConnectionContext::internal("mobile");
            let params = json!({"approval": format!("cfm_{id}"), "allowed": allow});
            match handler.handle(&ctx, "approval.decide", &params) {
                Ok(value) => Response::ok_json(value),
                Err(e) => Response::err(409, "Conflict", &e.to_string()),
            }
        }
        _ => Response::err(400, "Bad Request", "kind is agent or input"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_watching_phone_changes_nothing() {
        let handler = McpHandler::new();
        for path in ["/api/one/send", "/api/one/decide", "/api/one/handover", "/api/one/stop"] {
            let reply = route(&handler, "readonly", "POST", path, b"{}").expect("a route");
            assert_eq!(reply.status, 403, "{path}");
        }
        assert!(route(&handler, "full", "GET", "/api/one/nothing", b"").is_none());
    }

    #[test]
    fn nothing_is_decided_by_default() {
        let handler = McpHandler::new();
        let reply = decide(&handler, &json!({"kind": "input", "id": 1}));
        assert_eq!(reply.status, 400);
        let reply = decide(&handler, &json!({"kind": "other", "allow": true}));
        assert_eq!(reply.status, 400);
    }

    #[test]
    fn a_phone_sees_what_is_being_decided() {
        assert_eq!(argument_preview(&json!({"command": "git push"})), json!("git push"));
        assert_eq!(argument_preview(&Value::Null), Value::Null);
        let long = "x".repeat(500);
        let short = argument_preview(&json!({"path": long}));
        assert_eq!(short.as_str().unwrap().chars().count(), 401);
        assert_eq!(title_from("\n  fix the build\nthen test"), "fix the build");
    }
}
