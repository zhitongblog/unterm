//! Unterm describing itself, so something else can govern it.
//!
//! M5 built the *consumer* side: Unterm finding a browser, leasing it,
//! recording what it did. This is the mirror. Unzoo One — or any
//! orchestrator — has to be able to discover Unterm, read what it can do and
//! at what risk, present a lease, and be told what happened. Being managed
//! well is a feature, and it is not the same feature as managing.
//!
//! **The risk metadata is structured, and it comes from the gateway.** Not
//! from `[MUTATION]` tags in a tool description, which are prose that drifts
//! from behaviour the first time somebody edits one without the other. The
//! same table the gateway refuses by is the table the manifest publishes;
//! there is one answer to "how dangerous is this" on this machine.
//!
//! **Nothing here issues a lease.** Leases come from above and are checked
//! here — that is the whole shape of being a provider rather than a registry.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// What this provider is called wherever it is registered.
///
/// `contracts/v0` spells an id as `^provider_[a-z0-9]+(_[a-z0-9]+)*$`, so the
/// dotted name this used to carry ("unterm.terminal") did not validate. The
/// name is the provider's identity to an orchestrator, and an identity that
/// fails the schema it is being registered against is no identity at all.
pub const PROVIDER_ID: &str = "provider_unterm_terminal";

/// Which shape this manifest is in.
///
/// A constant, not a number: the contract asks for the schema's own name so a
/// reader knows which document to validate against. The number that used to
/// be here was the *task store's* schema version, which moves for reasons
/// that have nothing to do with the manifest's shape -- two different things
/// wearing one field.
pub const MANIFEST_SCHEMA_VERSION: &str = "provider_manifest.v0";

/// The capability families a caller can lease from a terminal.
///
/// Coarse, like the ones Unterm itself asks for from a browser: a person
/// deciding whether to let something run commands here is answering a
/// question about a family, not about a hundred and forty methods.
fn families() -> Vec<(&'static str, &'static [&'static str])> {
    vec![
        ("terminal.exec", &["exec.run", "exec.run_wait", "exec.send", "signal.send"] as &[&str]),
        ("terminal.session", &["session.create", "session.split", "session.destroy", "session.input", "session.paste", "session.resize"]),
        ("terminal.read", &["screen.read", "screen.text", "screen.scrollback_text", "session.list", "session.history"]),
        ("terminal.agent", &["agent_session.start", "agent_session.interrupt", "agent_session.close"]),
        ("terminal.workspace", &["scope.create", "scope.check", "workspace.save", "workspace.restore"]),
    ]
}

/// The manifest: who this is, what it speaks, what it can do.
pub fn manifest() -> Value {
    let capabilities: Vec<Value> = families()
        .into_iter()
        .map(|(name, methods)| {
            // The most dangerous method in the family decides the family's
            // risk. Publishing the average would let a caller lease "reading"
            // and get a `session.destroy`.
            let risk = methods
                .iter()
                .filter_map(|method| unterm_gateway::risk_of(method))
                .max()
                .unwrap_or(unterm_gateway::Risk::Destructive);
            json!({
                "name": name,
                "risk": risk.as_str(),
                "methods": methods,
                "scopes": ["path"],
                // Whether the layer above can undo this by itself. It cannot
                // undo a killed process or a sent keystroke, and saying
                // otherwise is how an orchestrator plans a rollback that
                // silently does nothing.
                "reversible": risk == unterm_gateway::Risk::Read,
                "evidence": ["command", "exit_code", "output_ref", "resolved_path"],
                // The remaining five of the contract's seven. Each is derived
                // from something already known here rather than asserted
                // separately: a declaration that can drift from the gateway
                // it describes is worse than none, because it is believed.
                //
                // Does anything outside this machine change? Reading a screen
                // does not. Running a command can do anything the shell can,
                // which includes reaching the network -- so the honest answer
                // for the higher tiers is "yes, possibly", not "no".
                "side_effect": if risk == unterm_gateway::Risk::Read {
                    "none"
                } else if risk >= unterm_gateway::Risk::ExternalSideEffect {
                    "external"
                } else {
                    "local"
                },
                // What a grant has to name. Every family here is bounded by a
                // path; nothing in this provider is scoped by anything else,
                // and saying so is what lets an orchestrator refuse a grant
                // that names the wrong kind of thing.
                "resource_scopes": ["path"],
                // Whether an argument may carry a secret. A command line can:
                // people type tokens into them, and the audit trail has to
                // know to redact before it stores. A screen read cannot.
                "sensitive_inputs": risk != unterm_gateway::Risk::Read,
                // Who says yes. The gateway already decides this per method,
                // and the top three tiers there refuse a standing grant --
                // they want the user, once, for this exact call.
                "approval_mode": if risk == unterm_gateway::Risk::Read {
                    "none"
                } else if risk >= unterm_gateway::Risk::CredentialAccess {
                    "per_call"
                } else {
                    "grant"
                },
                // What must survive the call. A read leaves the evidence it
                // returned; anything that changes the machine has to leave a
                // record whether or not anybody asks for it.
                "evidence_policy": if risk == unterm_gateway::Risk::Read {
                    "on_request"
                } else {
                    "always"
                },
            })
        })
        .collect();

    let health = health();
    json!({
        "provider_id": PROVIDER_ID,
        "product_version": unterm_protocol::PRODUCT_VERSION,
        "protocol_version": unterm_protocol::PROTOCOL_VERSION,
        "schema_version": MANIFEST_SCHEMA_VERSION,
        // The task store's own version, kept because a caller that plans
        // around durable task rows still needs it -- just not under the name
        // that means "this manifest's shape".
        "task_schema_version": unterm_tasks::schema_version(),
        "endpoint": health["endpoint"].clone(),
        "health": health["state"].clone(),
        "capabilities": capabilities,
    })
}

/// Whether this terminal can take work right now.
pub fn health() -> Value {
    let processes = crate::supervisor::survey();
    let mcp = processes
        .iter()
        .find(|process| process.role == crate::supervisor::Role::Mcp);
    let ready = crate::supervisor::can_work_without_ui();
    json!({
        // "ready" is about taking work, not about being alive — the
        // distinction the whole supervisor exists for.
        "state": if ready { "ready" } else { "unavailable" },
        "endpoint": mcp.and_then(|process| match &process.health {
            crate::supervisor::Health::Ready { endpoint, .. } => endpoint.clone(),
            _ => None,
        }),
        // The reason, whatever kind of not-ready it is: an orchestrator
        // deciding whether to wait or to give up needs the difference between
        // "still starting" and "the process is gone".
        "detail": mcp.map(|process| process.health.as_str()),
    })
}

/// Check a lease the layer above issued.
///
/// Accepting is checking. This never creates one: a provider that could issue
/// its own leases could authorise itself, and then the lease means nothing.
pub fn accept_lease(lease_id: &str, capability: &str) -> Result<Value> {
    let store = crate::cockpit::fleet_store::tasks()
        .ok_or_else(|| anyhow!("there is no store to check a lease against"))?;
    let lease = store
        .lease(lease_id)?
        .ok_or_else(|| anyhow!("no such lease: {lease_id}"))?;
    let now = chrono::Utc::now().to_rfc3339();
    if !lease.is_live(&now) {
        return Err(anyhow!(
            "that lease is {}",
            if lease.revoked_at.is_some() {
                "revoked"
            } else {
                "expired"
            }
        ));
    }
    if lease.capability != capability {
        return Err(anyhow!(
            "that lease is for {}, not {capability}",
            lease.capability
        ));
    }
    Ok(json!({"lease": lease_id, "capability": capability, "accepted": true, "expires_at": lease.expires_at}))
}

/// Whether a capability changes anything.
fn is_side_effecting(capability: &str) -> bool {
    families()
        .into_iter()
        .find(|(name, _)| *name == capability)
        .map(|(_, methods)| {
            methods
                .iter()
                .filter_map(|method| unterm_gateway::risk_of(method))
                .any(|risk| risk != unterm_gateway::Risk::Read)
        })
        // A capability nobody declared is treated as changing something.
        .unwrap_or(true)
}

/// What an invocation needs to carry.
///
/// A side-effecting call without a task context is refused: the record it
/// would leave could not be tied to the work it was done for, and an audit
/// trail nobody can join is one nobody reads.
pub fn check_context(
    capability: &str,
    task_id: Option<&str>,
    idempotency_key: Option<&str>,
) -> Result<()> {
    if !is_side_effecting(capability) {
        return Ok(());
    }
    if task_id.is_none() {
        return Err(anyhow!(
            "{capability} changes something; pass the task_id it is being done for"
        ));
    }
    if idempotency_key.is_none() {
        return Err(anyhow!(
            "{capability} changes something; pass an idempotency_key so a retry cannot repeat it"
        ));
    }
    Ok(())
}

/// Record that an invocation is starting, or hand back the answer it already
/// has.
///
/// Reuses the same table provider calls use, so "did this already happen" has
/// one implementation whichever direction the call was going.
pub enum Admission {
    /// Nobody has asked this before.
    Fresh(String),
    /// The same key already produced this.
    Settled(Value),
    /// The same key is in flight. Neither repeating nor pretending it is done
    /// would be true.
    InFlight(String),
}

pub fn begin(
    capability: &str,
    method: &str,
    lease_id: Option<&str>,
    idempotency_key: Option<&str>,
    params: &Value,
) -> Result<Admission> {
    let store = crate::cockpit::fleet_store::tasks()
        .ok_or_else(|| anyhow!("there is no store to record a call in"))?;
    let slot = store.begin_call(
        idempotency_key,
        PROVIDER_ID,
        capability,
        method,
        lease_id,
        params,
    )?;
    Ok(match slot {
        unterm_tasks::CallSlot::Fresh(record) => Admission::Fresh(record.id),
        unterm_tasks::CallSlot::Settled(record) => Admission::Settled(json!({
            "outcome": record.state,
            "value": record
                .response
                .as_deref()
                .and_then(|text| serde_json::from_str::<Value>(text).ok()),
            "response_sha256": record.response_sha256,
            "replayed_from_record": true,
        })),
        unterm_tasks::CallSlot::InFlight(record) => Admission::InFlight(record.id),
    })
}

/// Record how an invocation turned out.
pub fn finish(call_id: &str, ok: bool, value: Option<&Value>, error: Option<&str>) -> Result<Value> {
    let store = crate::cockpit::fleet_store::tasks()
        .ok_or_else(|| anyhow!("there is no store to record a call in"))?;
    let state = if ok { "succeeded" } else { "failed" };
    let record = store.finish_call(call_id, state, value, error)?;
    Ok(json!({
        "call_id": call_id,
        "outcome": state,
        "response_sha256": record.and_then(|record| record.response_sha256),
    }))
}

/// Stop an invocation that is still running.
pub fn cancel(call_id: &str) -> Result<Value> {
    let store = crate::cockpit::fleet_store::tasks()
        .ok_or_else(|| anyhow!("there is no store"))?;
    // Cancelling a terminal call means cancelling what it started. The
    // session-level machinery already knows how; this closes the record so a
    // caller is not left reading `pending` forever.
    let record = store.finish_call(call_id, "cancelled", None, Some("cancelled by the caller"))?;
    Ok(json!({"call_id": call_id, "cancelled": record.is_some()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolate() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("UNTERM_STATE_DIR", dir.path());
        std::env::set_var("UNTERM_TASKS_DB", dir.path().join("tasks.db"));
        crate::cockpit::fleet_store::reset_for_tests();
        dir
    }

    /// All seven of the contract's risk fields, and each one derived rather
    /// than written down.
    ///
    /// The contract asks for seven; the manifest published two. The other five
    /// are not new facts -- they follow from the gateway's tier for the family
    /// -- so this checks the derivation rather than a table of expected
    /// strings: a table would have to be edited whenever a method moves tier,
    /// and an orchestrator reading a stale table plans around a risk the
    /// gateway no longer agrees with.
    #[test]
    fn every_capability_declares_all_seven_risk_fields() {
        let _dir = isolate();
        let manifest = manifest();
        let capabilities = manifest["capabilities"]
            .as_array()
            .expect("capabilities")
            .clone();
        assert!(!capabilities.is_empty(), "no capabilities were published");

        for capability in capabilities {
            let name = capability["name"].as_str().unwrap_or("<unnamed>");
            for field in [
                "risk",
                "reversible",
                "side_effect",
                "resource_scopes",
                "sensitive_inputs",
                "approval_mode",
                "evidence_policy",
            ] {
                assert!(
                    !capability[field].is_null(),
                    "{name} does not declare {field}"
                );
            }

            // Read is the one tier that promises nothing changes; every other
            // answer has to follow from that, in the same direction.
            let reads_only = capability["risk"] == "read";
            assert_eq!(
                capability["side_effect"] == "none",
                reads_only,
                "{name}: side_effect and risk disagree about whether anything changes"
            );
            assert_eq!(
                capability["sensitive_inputs"].as_bool(),
                Some(!reads_only),
                "{name}: a call that can change the machine can carry a secret"
            );
            assert_eq!(
                capability["approval_mode"] == "none",
                reads_only,
                "{name}: approval_mode and risk disagree about needing a yes"
            );
            assert_eq!(
                capability["evidence_policy"] == "always",
                !reads_only,
                "{name}: anything that changes the machine must leave a record"
            );
            assert_eq!(
                capability["reversible"].as_bool(),
                Some(reads_only),
                "{name}: only a read is undoable by the layer above"
            );
        }
    }

    #[test]
    fn the_manifest_publishes_risk_from_the_gateway_not_from_prose() {
        let _dir = isolate();
        let manifest = manifest();
        assert_eq!(manifest["provider_id"], PROVIDER_ID);
        assert!(manifest["product_version"].is_string());
        // Pinned to the contract's own spellings rather than to "is a string"
        // / "is a number": the shapes are what an orchestrator validates
        // against, and a type check would have kept passing through the very
        // change that broke them.
        assert_eq!(manifest["schema_version"], "provider_manifest.v0");
        assert!(
            manifest["task_schema_version"].is_number(),
            "the task store's version moved out from under `schema_version`, \
             not away"
        );
        let id = manifest["provider_id"].as_str().expect("provider_id");
        assert!(
            id.starts_with("provider_")
                && id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                && !id.contains("__")
                && !id.ends_with('_'),
            "provider_id {id:?} does not match the contract's \
             ^provider_[a-z0-9]+(_[a-z0-9]+)*$"
        );

        let capabilities = manifest["capabilities"].as_array().unwrap();
        let by_name = |name: &str| {
            capabilities
                .iter()
                .find(|capability| capability["name"] == name)
                .cloned()
                .unwrap()
        };
        // Reading is reading; running commands is its own band, which is the
        // whole reason there are seven of them and not three; killing a pane
        // is the one nobody can undo.
        assert_eq!(by_name("terminal.read")["risk"], "read");
        assert_eq!(by_name("terminal.exec")["risk"], "exec");
        assert_eq!(by_name("terminal.session")["risk"], "destructive");
        assert_eq!(by_name("terminal.read")["reversible"], true);
        assert_eq!(by_name("terminal.exec")["reversible"], false);
    }

    #[test]
    fn a_family_is_as_dangerous_as_its_worst_method() {
        // Publishing the average would let a caller lease "sessions" for
        // resizing and get `session.destroy`.
        let _dir = isolate();
        let capabilities = manifest();
        let session = capabilities["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|capability| capability["name"] == "terminal.session")
            .cloned()
            .unwrap();
        assert!(session["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|method| method == "session.resize"));
        assert_eq!(session["risk"], "destructive");
    }

    #[test]
    fn a_lease_is_checked_never_issued() {
        let _dir = isolate();
        let store = crate::cockpit::fleet_store::tasks().unwrap();
        let lease = store
            .issue_lease(unterm_tasks::NewLease {
                provider: PROVIDER_ID.into(),
                capability: "terminal.exec".into(),
                ttl_seconds: 300,
                ..Default::default()
            })
            .unwrap();

        assert!(accept_lease(&lease.id, "terminal.exec").is_ok());
        // For the wrong capability, and for one nobody issued.
        assert!(accept_lease(&lease.id, "terminal.session").is_err());
        assert!(accept_lease("lse_invented", "terminal.exec").is_err());

        // Revoked from above, refused here immediately.
        store.revoke_lease(&lease.id).unwrap();
        let error = accept_lease(&lease.id, "terminal.exec").unwrap_err().to_string();
        assert!(error.contains("revoked"), "{error}");
    }

    #[test]
    fn a_side_effecting_call_without_a_task_context_is_refused() {
        // The record it would leave could not be tied to the work it was done
        // for, and a trail nobody can join is one nobody reads.
        let _dir = isolate();
        let error = check_context("terminal.exec", None, Some("k"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("task_id"), "{error}");

        let error = check_context("terminal.exec", Some("tsk_1"), None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("idempotency_key"), "{error}");

        assert!(check_context("terminal.exec", Some("tsk_1"), Some("k")).is_ok());
        // Reading needs neither: there is nothing to retry wrongly.
        assert!(check_context("terminal.read", None, None).is_ok());
        // And something nobody declared is treated as changing things.
        assert!(check_context("terminal.whatever", None, None).is_err());
    }

    #[test]
    fn the_same_key_gets_the_first_answer_rather_than_a_second_run() {
        let _dir = isolate();
        let params = json!({"command": "echo hi"});
        let Admission::Fresh(call) = begin(
            "terminal.exec",
            "exec.run",
            None,
            Some("idem-1"),
            &params,
        )
        .unwrap() else {
            panic!("the first call was not fresh");
        };
        finish(&call, true, Some(&json!({"exit_code": 0})), None).unwrap();

        match begin("terminal.exec", "exec.run", None, Some("idem-1"), &params).unwrap() {
            Admission::Settled(answer) => {
                assert_eq!(answer["replayed_from_record"], true);
                assert_eq!(answer["value"]["exit_code"], 0);
            }
            _ => panic!("the same key ran twice"),
        }
    }

    #[test]
    fn a_call_in_flight_is_neither_repeated_nor_reported_done() {
        let _dir = isolate();
        let params = json!({});
        begin("terminal.exec", "exec.run", None, Some("idem-2"), &params).unwrap();
        assert!(matches!(
            begin("terminal.exec", "exec.run", None, Some("idem-2"), &params).unwrap(),
            Admission::InFlight(_)
        ));
    }

    #[test]
    fn cancelling_closes_the_record_so_nobody_reads_pending_forever() {
        let _dir = isolate();
        let Admission::Fresh(call) =
            begin("terminal.exec", "exec.run", None, Some("idem-3"), &json!({})).unwrap()
        else {
            panic!("not fresh");
        };
        let cancelled = cancel(&call).unwrap();
        assert_eq!(cancelled["cancelled"], true);
        let record = crate::cockpit::fleet_store::tasks()
            .unwrap()
            .call_by_id(&call)
            .unwrap()
            .unwrap();
        assert_eq!(record.state, "cancelled");
    }

    #[test]
    fn health_says_ready_only_when_work_can_actually_happen() {
        let _dir = isolate();
        // Nothing is running in a test process, so this is the honest answer.
        assert_eq!(health()["state"], "unavailable");
    }
}
