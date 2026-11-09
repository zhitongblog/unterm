//! `approval.*` — the questions waiting for a human, and answering them.
//!
//! The gateway has been able to ask since M3, and until now nothing could
//! answer: a destructive action from an identified agent created a question
//! that sat there until it expired. That is a refusal with a five-minute
//! delay, dressed as a prompt.
//!
//! **Who may answer.** Not the agent. `approval.decide` refuses any caller
//! that reached the surface over the network, and is reachable only from
//! in-process callers — the settings page, and the application itself. The
//! boundary is not a security perimeter and should not be described as one:
//! an agent with a shell can read the instance token, run the CLI, and drive
//! this machine as thoroughly as the person sitting at it. What it is, is a
//! *deliberation* boundary. An agent asking for permission must stop and a
//! human must notice, and an agent that could answer its own question would
//! never stop.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use unterm_tasks::Scope;

pub const METHODS: &[&str] = &["approval.list", "approval.decide"];

pub fn handles(method: &str) -> bool {
    METHODS.contains(&method)
}

/// Whether this caller is inside the application rather than out on the wire.
///
/// `ConnectionContext::internal` is what the settings page and other
/// in-process dispatchers use; the TCP server allocates connection ids from
/// one upwards and always sets a real peer address.
pub fn is_internal(context: &crate::handler::ConnectionContext) -> bool {
    context.conn_id == 0 && context.peer_addr.starts_with("internal:")
}

pub fn dispatch(
    context: &crate::handler::ConnectionContext,
    method: &str,
    params: &Value,
) -> Result<Value> {
    match method {
        "approval.list" => {
            // Readable by anyone: an agent that asked for something is
            // entitled to know its question is still pending, and being able
            // to see a queue is not being able to empty it.
            let pending = unterm_services::gateway::pending();
            let pending_input = crate::handler::pending_confirmations_list();
            let recent_input = crate::handler::recent_confirmations_list();
            Ok(json!({
                "approvals": pending,
                "pending_input_confirmations": pending_input,
                "recent_input_confirmations": recent_input,
            }))
        }

        "approval.decide" => {
            if !is_internal(context) {
                return Err(anyhow!(
                    "approvals are answered by the person, in Unterm's settings — not over MCP. \
                     An agent that could answer its own request would never have to stop."
                ));
            }
            let id = params
                .get("approval")
                .or_else(|| params.get("id"))
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Missing 'approval'"))?;
            let allowed = params
                .get("allowed")
                .and_then(Value::as_bool)
                .ok_or_else(|| anyhow!("Missing 'allowed'"))?;
            if let Some(num) = id
                .strip_prefix("cfm_")
                .or(Some(id))
                .and_then(|raw| raw.parse::<u64>().ok())
            {
                let decision = if allowed {
                    if params.get("remember").and_then(Value::as_str).is_some() {
                        crate::handler::ConfirmationDecision::AlwaysAllow
                    } else {
                        crate::handler::ConfirmationDecision::Allow
                    }
                } else {
                    crate::handler::ConfirmationDecision::Block
                };
                if crate::handler::resolve_confirmation(num, decision) {
                    return Ok(json!({
                        "approval": {
                            "id": format!("cfm_{num}"),
                            "allowed": allowed,
                            "resolved": true,
                        }
                    }));
                }
            }
            // "Remember this" is opt-in and narrow. The default is that an
            // answer covers the question it was asked about and nothing else.
            let remember = params
                .get("remember")
                .and_then(Value::as_str)
                .map(Scope::parse)
                .transpose()?;
            let decided_by = params
                .get("decided_by")
                .and_then(Value::as_str)
                .unwrap_or("the user");
            let approval =
                unterm_services::gateway::answer_by_id(id, allowed, decided_by, remember)?;
            Ok(json!({"approval": approval}))
        }

        other => Err(anyhow!("approval dispatch reached {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::ConnectionContext;

    fn from_the_wire() -> ConnectionContext {
        ConnectionContext {
            conn_id: 7,
            peer_addr: "127.0.0.1:51234".into(),
        }
    }

    #[test]
    fn every_listed_method_is_dispatched() {
        // The handler's drift check reads this table instead of scanning for
        // literal match arms, so the table has to be the truth about what is
        // dispatched — otherwise a name here that no arm handles would look
        // published and answer "unknown method".
        for method in METHODS {
            assert!(handles(method));
            if let Err(error) = dispatch(
                &ConnectionContext::internal("test"),
                method,
                &json!({}),
            ) {
                assert!(
                    !error.to_string().contains("approval dispatch reached"),
                    "{method} has no arm"
                );
            }
        }
    }

    #[test]
    fn an_agent_cannot_answer_its_own_question() {
        let error = dispatch(
            &from_the_wire(),
            "approval.decide",
            &json!({"approval": "apr_1", "allowed": true}),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("settings"), "{error}");
        // And the refusal happens before the id is even looked at, so a
        // caller cannot learn which approvals exist by guessing.
        assert!(!error.contains("apr_1"), "{error}");
    }

    #[test]
    fn an_agent_may_still_see_that_its_question_is_pending() {
        // Being able to see a queue is not being able to empty it, and an
        // agent that cannot tell whether it is waiting will simply retry.
        assert!(dispatch(&from_the_wire(), "approval.list", &json!({})).is_ok());
    }

    #[test]
    fn the_settings_page_is_inside() {
        assert!(is_internal(&ConnectionContext::internal("web_settings")));
        assert!(!is_internal(&from_the_wire()));
        // A peer that names itself "internal:" over the wire is still on the
        // wire: the connection id is what the server allocates, not the
        // caller.
        assert!(!is_internal(&ConnectionContext {
            conn_id: 3,
            peer_addr: "internal:web_settings".into(),
        }));
    }

    #[test]
    fn answering_needs_an_answer() {
        let error = dispatch(
            &ConnectionContext::internal("web_settings"),
            "approval.decide",
            &json!({"approval": "apr_1"}),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("allowed"), "{error}");
    }

    #[test]
    fn a_scope_nobody_defined_is_refused_by_name() {
        let error = dispatch(
            &ConnectionContext::internal("web_settings"),
            "approval.decide",
            &json!({"approval": "apr_1", "allowed": true, "remember": "forever"}),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("forever"), "{error}");
    }
}
