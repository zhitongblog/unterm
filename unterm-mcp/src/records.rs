//! `scope.*`, `artifact.*`, `audit.verify`, `task.*_evidence` — what happened
//! and where it was allowed to happen.
//!
//! Four namespaces in one module because they are one story told at four
//! points: a workspace bounds the work, artifacts are what it produced, the
//! audit chain is the record of it, and an evidence bundle is all of that
//! handed to somebody who was not there.
//!
//! On the name `scope.*`: this surface has meant saved pane layouts by
//! `workspace.*` since long before M6, and renaming that would break every
//! agent in the field for the sake of a word. So the filesystem scopes get
//! their own namespace and the old one keeps its meaning.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use unterm_services::path_scope::PathAccess;
use unterm_services::{artifacts, audit_store, evidence, workspace_scope};

pub const METHODS: &[&str] = &[
    "scope.list",
    "scope.create",
    "scope.check",
    "scope.archive",
    "artifact.list",
    "artifact.usage",
    "artifact.verify",
    "artifact.forget",
    "audit.verify",
    "task.export_evidence",
    "task.verify_evidence",
    "supervisor.status",
    "supervisor.reconcile",
    "system.diagnostics",
    "system.snapshots",
    "system.snapshot",
    "system.restore_snapshot",
    "system.installs",
    "system.uninstall_plan",
    "system.uninstall",
    "system.upgrade",
    "agent_session.start",
    "agent_session.events",
    "agent_session.submit_input",
    "agent_session.interrupt",
    "agent_session.status",
    "agent_session.close",
    "terminal.manifest",
    "terminal.health",
    "terminal.capabilities",
    "terminal.accept_lease",
    "terminal.invoke",
    "terminal.cancel",
];

pub fn handles(method: &str) -> bool {
    METHODS.contains(&method)
}

/// The caller's identifiers, taken as given.
///
/// Never generated here: an id this process invented would correlate with
/// nothing upstream, which is worse than no id because it looks like
/// correlation.
fn task_context(params: &Value) -> unterm_services::agent_session::TaskContext {
    let field = |name: &str| {
        params
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    unterm_services::agent_session::TaskContext {
        task_id: field("task_id"),
        run_id: field("run_id"),
        step_id: field("step_id"),
        idempotency_key: field("idempotency_key"),
        lease_id: field("lease_id"),
    }
}

fn text(params: &Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Missing '{key}'"))
}

/// Dispatch, with the handler for the one method that re-enters it.
///
/// `terminal.invoke` is a governed envelope around an ordinary method: it
/// checks the lease and the context, records the call, and then dispatches
/// through the *same* handler every other door uses — so the gateway judges
/// it exactly as it would judge that method called directly. Anything less
/// would be a second execution path, which is the thing M3 exists to prevent.
pub fn dispatch_with(
    handler: &crate::handler::McpHandler,
    connection: &crate::handler::ConnectionContext,
    method: &str,
    params: &Value,
) -> Result<Value> {
    if method == "terminal.invoke" {
        return invoke(handler, connection, params);
    }
    dispatch(method, params)
}

fn invoke(
    handler: &crate::handler::McpHandler,
    connection: &crate::handler::ConnectionContext,
    params: &Value,
) -> Result<Value> {
    use unterm_services::terminal_provider::{self as provider, Admission};
    let capability = text(params, "capability")?;
    let method = text(params, "method")?;
    let context = task_context(params);
    let inner = params.get("params").cloned().unwrap_or(json!({}));

    // Required before anything happens, not reported afterwards: a record
    // that cannot be tied to the work it was done for is one nobody can join.
    provider::check_context(
        &capability,
        context.task_id.as_deref(),
        context.idempotency_key.as_deref(),
    )?;
    if let Some(lease) = context.lease_id.as_deref() {
        provider::accept_lease(lease, &capability)?;
    }

    let call = match provider::begin(
        &capability,
        &method,
        context.lease_id.as_deref(),
        context.idempotency_key.as_deref(),
        &inner,
    )? {
        Admission::Settled(answer) => return Ok(answer),
        Admission::InFlight(id) => {
            return Err(anyhow!(
                "an identical call ({id}) is already running; wait for it rather than repeating it"
            ))
        }
        Admission::Fresh(id) => id,
    };

    match handler.handle(connection, &method, &inner) {
        Ok(value) => {
            provider::finish(&call, true, Some(&value), None)?;
            Ok(json!({"outcome": "succeeded", "call_id": call, "value": value}))
        }
        Err(error) => {
            let message = error.to_string();
            provider::finish(&call, false, None, Some(&message))?;
            Err(anyhow!(message))
        }
    }
}

pub fn dispatch(method: &str, params: &Value) -> Result<Value> {
    match method {
        "scope.list" => Ok(json!({"workspaces": workspace_scope::list()?})),

        "scope.create" => {
            let workspace = workspace_scope::create(&text(params, "name")?, text(params, "path")?)?;
            Ok(json!({"workspace": workspace}))
        }

        "scope.check" => {
            let access = match params.get("access").and_then(Value::as_str).unwrap_or("read") {
                "write" => PathAccess::Write,
                "read" => PathAccess::Read,
                other => {
                    return Err(anyhow!(
                        "{other:?} is not an access; expected read or write"
                    ))
                }
            };
            let decision = workspace_scope::check(
                &text(params, "workspace")?,
                access,
                text(params, "path")?,
            )?;
            Ok(serde_json::to_value(decision)?)
        }

        "scope.archive" => {
            let id = text(params, "workspace")?;
            Ok(json!({"workspace": id, "archived": workspace_scope::archive(&id)?}))
        }

        "artifact.list" => {
            let store = unterm_services::cockpit::fleet_store::tasks()
                .ok_or_else(|| anyhow!("there is no task store"))?;
            let list = match params.get("task_id").and_then(Value::as_str) {
                Some(task) => store.artifacts_for_task(task)?,
                None => store.artifacts()?,
            };
            Ok(json!({"artifacts": list}))
        }

        "artifact.usage" => Ok(json!({"usage": artifacts::usage()?})),

        "artifact.verify" => {
            let store = unterm_services::cockpit::fleet_store::tasks()
                .ok_or_else(|| anyhow!("there is no task store"))?;
            let id = text(params, "artifact")?;
            let artifact = store
                .artifact(&id)?
                .ok_or_else(|| anyhow!("no such artifact: {id}"))?;
            Ok(json!({
                "artifact": id,
                "sha256": artifact.sha256,
                "intact": artifacts::verify(&artifact)?,
            }))
        }

        "artifact.forget" => {
            let id = text(params, "artifact")?;
            Ok(json!({"artifact": id, "forgotten": artifacts::forget(&id)?}))
        }

        "audit.verify" => Ok(serde_json::to_value(audit_store::verify_chain())?),

        "task.export_evidence" => {
            let manifest = evidence::export(&text(params, "task_id")?, text(params, "path")?)?;
            Ok(json!({"manifest": manifest}))
        }

        "task.verify_evidence" => Ok(serde_json::to_value(evidence::verify(text(
            params, "path",
        )?)?)?),

        "supervisor.status" => {
            // Flattened on the way out. `Health` is an internally-tagged enum
            // whose payload differs per variant, which is right for Rust and
            // awkward on a wire: every client would have to know that `pid`
            // lives under `health` and only for some states.
            let processes: Vec<Value> = unterm_services::supervisor::survey()
                .into_iter()
                .map(|process| {
                    json!({
                        "role": process.role.as_str(),
                        "state": process.health.as_str(),
                        "pid": process.health.pid(),
                        "usable": process.health.is_usable(),
                        "detail": match &process.health {
                            unterm_services::supervisor::Health::Ready { endpoint, .. } => {
                                endpoint.clone()
                            }
                            unterm_services::supervisor::Health::Stale { since, .. } => {
                                since.clone().map(|since| format!("since {since}"))
                            }
                            _ => None,
                        },
                        "version": process.version,
                        "source": process.source,
                    })
                })
                .collect();
            Ok(json!({
                "processes": processes,
                // The answer to "can this machine do work right now", which
                // is not the same as "is every process up".
                "can_work_without_ui": unterm_services::supervisor::can_work_without_ui(),
            }))
        }

        "supervisor.reconcile" => unterm_services::supervisor::reconcile_after_crash(),

        "system.diagnostics" => {
            let bundle = match params.get("path").and_then(Value::as_str) {
                Some(path) => unterm_services::diagnostics::write(path)?,
                // Without a path the bundle is returned rather than written:
                // a caller reading it over MCP should not have to find a
                // directory first.
                None => unterm_services::diagnostics::redact(
                    &unterm_services::diagnostics::collect(),
                ),
            };
            Ok(bundle)
        }

        "system.snapshots" => Ok(json!({"snapshots": unterm_services::upgrade::snapshots()})),

        "system.snapshot" => {
            let version = params
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or(unterm_protocol::PRODUCT_VERSION);
            Ok(json!({"snapshot": unterm_services::upgrade::snapshot(version)?}))
        }

        "system.restore_snapshot" => {
            let id = text(params, "snapshot")?;
            let snapshot = unterm_services::upgrade::restore(&id, unterm_protocol::PRODUCT_VERSION)?;
            Ok(json!({"restored": snapshot}))
        }

        "system.installs" => {
            let installs = unterm_services::install::survey();
            let conflicts = unterm_services::install::conflicts(&installs);
            Ok(json!({"installs": installs, "conflicts": conflicts}))
        }

        // A plan, never an act. What somebody wants before they answer "yes,
        // delete it" is what they would be losing.
        "system.uninstall_plan" => {
            let keep = params
                .get("keep_data")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            Ok(serde_json::to_value(unterm_services::install::uninstall_plan(keep))?)
        }

        // The act, kept apart from the plan and made awkward on purpose: it
        // takes the plan's own confirmation string, so a caller cannot reach
        // it by guessing an argument.
        "system.uninstall" => {
            let keep = params
                .get("keep_data")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let confirm = text(params, "confirm")?;
            if confirm != "remove unterm" {
                return Err(anyhow!(
                    "this removes Unterm from this machine. Pass confirm: \"remove unterm\" if that is what you mean."
                ));
            }
            let plan = unterm_services::install::uninstall_plan(keep);
            let removed = unterm_services::install::uninstall(&plan);
            Ok(json!({"planned": plan, "removed": removed}))
        }

        // The real thing, with real binaries: swap, run the new one, and put
        // both the program and the data back if it does not answer.
        "system.upgrade" => {
            let live = std::path::PathBuf::from(text(params, "live")?);
            let staged = std::path::PathBuf::from(text(params, "staged")?);
            let to = text(params, "to_version")?;
            let from = params
                .get("from_version")
                .and_then(Value::as_str)
                // The *product* version, not this crate's. `CARGO_PKG_VERSION`
                // here is unterm-mcp's own 0.1.0, and an upgrade report that
                // names the wrong version is a report nobody can act on —
                // found by running a real rollback and reading what it said.
                .unwrap_or(unterm_protocol::PRODUCT_VERSION)
                .to_string();
            let outcome = unterm_services::upgrade::swap_with_rollback(
                &live,
                &staged,
                &from,
                &to,
                |path| {
                    // The confirmation is running it. Not "does the file
                    // exist" — a corrupt binary exists — and not a checksum,
                    // which says the bytes arrived and nothing about whether
                    // they run on this machine.
                    let output = std::process::Command::new(path)
                        .arg("--version")
                        .output()
                        .map_err(|error| anyhow!("{} did not start: {error}", path.display()))?;
                    if !output.status.success() {
                        return Err(anyhow!(
                            "{} exited {}",
                            path.display(),
                            output.status
                        ));
                    }
                    let said = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if said.is_empty() {
                        return Err(anyhow!("{} answered nothing", path.display()));
                    }
                    Ok(())
                },
            )?;
            Ok(serde_json::to_value(outcome)?)
        }

        // ---- hosted agent sessions ------------------------------------
        "agent_session.start" => {
            let command: Vec<String> = match params.get("command") {
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|part| part.as_str().map(str::to_string))
                    .collect(),
                // A string is split the way a shell would not: naively, on
                // spaces. Accepted because callers send it, and documented as
                // the lesser form — an array says exactly what runs.
                Some(Value::String(line)) => {
                    line.split_whitespace().map(str::to_string).collect()
                }
                _ => return Err(anyhow!("Missing 'command'")),
            };
            let env: Vec<(String, String)> = params
                .get("env")
                .and_then(Value::as_object)
                .map(|map| {
                    map.iter()
                        .filter_map(|(key, value)| {
                            value.as_str().map(|value| (key.clone(), value.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let context = task_context(params);
            let id = unterm_services::agent_session::start(
                &command,
                params.get("cwd").and_then(Value::as_str),
                &env,
                params.get("prompt").and_then(Value::as_str),
                params.get("adapter").and_then(Value::as_str),
                context,
            )?;
            Ok(json!({"session_id": id}))
        }

        "agent_session.events" => {
            let id = text(params, "session_id")?;
            let cursor = params
                .get("cursor")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let (events, next) = unterm_services::agent_session::events(&id, cursor)?;
            // The cursor comes back so a caller that reconnects asks from
            // where it left off rather than from the beginning.
            Ok(json!({"events": events, "cursor": next}))
        }

        "agent_session.submit_input" => {
            let id = text(params, "session_id")?;
            unterm_services::agent_session::submit_input(&id, &text(params, "text")?)?;
            Ok(json!({"session_id": id, "submitted": true}))
        }

        "agent_session.interrupt" => {
            let id = text(params, "session_id")?;
            let grace = params
                .get("grace_ms")
                .and_then(Value::as_u64)
                .unwrap_or(2_000);
            unterm_services::agent_session::interrupt(&id, grace)?;
            Ok(json!({"session_id": id, "interrupted": true}))
        }

        "agent_session.status" => {
            unterm_services::agent_session::status(&text(params, "session_id")?)
        }

        "agent_session.close" => {
            unterm_services::agent_session::close(&text(params, "session_id")?)
        }

        // ---- Unterm as a governable provider ---------------------------
        // The mirror of `provider.*`: that surface is Unterm managing a
        // browser, this one is something else managing Unterm.
        "terminal.manifest" => Ok(unterm_services::terminal_provider::manifest()),

        "terminal.health" => Ok(unterm_services::terminal_provider::health()),

        "terminal.capabilities" => Ok(json!({
            "capabilities": unterm_services::terminal_provider::manifest()["capabilities"].clone()
        })),

        "terminal.accept_lease" => unterm_services::terminal_provider::accept_lease(
            &text(params, "lease")?,
            &text(params, "capability")?,
        ),

        // Handled by `dispatch_with`, which has the handler to re-enter.
        "terminal.invoke" => Err(anyhow!(
            "terminal.invoke must be dispatched with the handler"
        )),

        "terminal.cancel" => unterm_services::terminal_provider::cancel(&text(params, "call_id")?),

        other => Err(anyhow!("records dispatch reached {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_method_is_dispatched() {
        // The handler's drift check reads this table rather than scanning for
        // literal match arms, so a name here with no arm would look published
        // and answer "unknown method".
        for method in METHODS {
            assert!(handles(method));
            if let Err(error) = dispatch(method, &json!({})) {
                assert!(
                    !error.to_string().contains("records dispatch reached"),
                    "{method} has no arm"
                );
            }
        }
    }

    #[test]
    fn an_access_nobody_defined_is_refused_by_name() {
        let error = dispatch(
            "scope.check",
            &json!({"workspace": "wsp_1", "path": "/tmp", "access": "execute"}),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("execute"), "{error}");
        assert!(error.contains("read or write"), "{error}");
    }

    #[test]
    fn exporting_needs_somewhere_to_put_it() {
        let error = dispatch("task.export_evidence", &json!({"task_id": "tsk_1"}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("path"), "{error}");
    }
}
