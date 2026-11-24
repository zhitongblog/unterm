//! A bundle somebody can send us without sending us their life.
//!
//! When a terminal misbehaves the useful information is versions, process
//! health, what the providers look like and roughly what happened. The same
//! places hold tokens, paths with the user's name in them, prompts, and
//! whatever an agent pasted into a command — and a diagnostics bundle that
//! carries those is one nobody should send and, once sent, cannot be
//! un-sent.
//!
//! So redaction here is a **whitelist**: fields are copied in by name, and
//! anything not named is absent. A blocklist would be the natural way to
//! write it and would leak the first time somebody added a field, which is
//! exactly the moment nobody is looking.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::Path;

/// Field names whose values never leave this machine, wherever they appear.
///
/// Belt and braces: the bundle is built by naming what goes in, and this is
/// checked over the finished product so a future contributor who assembles a
/// section by copying a whole struct cannot quietly widen it.
const NEVER: &[&str] = &[
    "token",
    "auth_token",
    "password",
    "secret",
    "api_key",
    "authorization",
    "cookie",
    "prompt",
    "command",
    "response",
];

/// Redact a value: keep the shape, drop the sensitive leaves.
pub fn redact(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, inner)| {
                    let lowered = key.to_lowercase();
                    if NEVER.iter().any(|name| lowered.contains(name)) {
                        // Named rather than removed: a reader has to be able
                        // to see that something was here, or they will ask
                        // for the field again.
                        (key.clone(), Value::String("[redacted]".into()))
                    } else {
                        (key.clone(), redact(inner))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        other => other.clone(),
    }
}

/// Paths are replaced by their shape, not their content.
///
/// A home directory carries the user's name, and a project path can carry a
/// client's. Which directory it was matters far less than that there was one.
fn shape_of_path(path: &str) -> String {
    let separator = if path.contains('\\') { '\\' } else { '/' };
    let depth = path.split(separator).filter(|part| !part.is_empty()).count();
    format!("<path with {depth} components>")
}

/// Build the bundle.
pub fn collect() -> Value {
    let processes: Vec<Value> = crate::supervisor::survey()
        .into_iter()
        .map(|process| {
            json!({
                "role": process.role.as_str(),
                "state": process.health.as_str(),
                // The pid is kept: it is the one number that makes a log line
                // and a crash report line up, and it names nothing about the
                // person.
                "pid": process.health.pid(),
                "version": process.version,
            })
        })
        .collect();

    let providers: Vec<Value> = crate::providers::statuses()
        .into_iter()
        .map(|status| {
            json!({
                "id": status.id,
                "state": status.state,
                // Not the endpoint: a port is harmless, but the same field
                // carries a path for stdio providers.
                "capabilities": status.capabilities,
                "source": status.source,
                "live_leases": status.live_leases,
            })
        })
        .collect();

    let store = crate::cockpit::fleet_store::tasks();
    let tasks = store
        .as_ref()
        .and_then(|store| store.tasks().ok())
        .map(|tasks| {
            let mut by_state: std::collections::BTreeMap<&str, usize> =
                std::collections::BTreeMap::new();
            for task in &tasks {
                *by_state.entry(task.state.as_str()).or_default() += 1;
            }
            // Counts, not titles. A task is called "fix the Henderson
            // invoice" and that is the customer's name, not ours.
            json!({"total": tasks.len(), "by_state": by_state})
        })
        .unwrap_or(Value::Null);

    let artifacts = crate::artifacts::usage()
        .ok()
        .map(|usage| json!(usage))
        .unwrap_or(Value::Null);

    let audit = crate::audit_store::verify_chain();

    json!({
        "format": "unterm-diagnostics/1",
        "collected_at": chrono::Utc::now().to_rfc3339(),
        "unterm_version": unterm_protocol::PRODUCT_VERSION,
        "platform": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        },
        "state_dir": unterm_protocol::state_dir()
            .map(|dir| shape_of_path(&dir.display().to_string())),
        "processes": processes,
        "providers": providers,
        "tasks": tasks,
        "artifacts": artifacts,
        "audit": {
            "entries": audit.entries,
            "chain_intact": audit.intact,
            "first_break_at": audit.broken_at,
        },
        "snapshots": crate::upgrade::snapshots()
            .into_iter()
            .map(|snapshot| json!({"version": snapshot.version, "taken_at": snapshot.taken_at}))
            .collect::<Vec<_>>(),
    })
}

/// Write the bundle where the user asked, and hand back what was written.
pub fn write(destination: impl AsRef<Path>) -> Result<Value> {
    let destination = destination.as_ref();
    let bundle = redact(&collect());
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(destination, serde_json::to_vec_pretty(&bundle)?)
        .with_context(|| format!("write {}", destination.display()))?;
    Ok(bundle)
}

/// Whether a finished bundle carries anything it should not.
///
/// Used by the test below and available to a caller who wants to check
/// before sending. An assertion somebody can run beats a promise in a doc
/// comment.
pub fn leaks_in(value: &Value) -> Vec<String> {
    let mut found = Vec::new();
    walk(value, "", &mut found);
    found
}

fn walk(value: &Value, path: &str, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                let lowered = key.to_lowercase();
                let here = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                if NEVER.iter().any(|name| lowered.contains(name))
                    && inner.as_str() != Some("[redacted]")
                {
                    found.push(here.clone());
                }
                walk(inner, &here, found);
            }
        }
        Value::Array(items) => {
            for (index, inner) in items.iter().enumerate() {
                walk(inner, &format!("{path}[{index}]"), found);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolate() -> crate::test_env::IsolatedEnv {
        crate::test_env::IsolatedEnv::new()
    }

    #[test]
    fn the_bundle_says_what_is_running_and_what_version() {
        let _dir = isolate();
        let bundle = collect();
        assert_eq!(bundle["format"], "unterm-diagnostics/1");
        assert_eq!(bundle["unterm_version"], unterm_protocol::PRODUCT_VERSION);
        assert_eq!(bundle["processes"].as_array().unwrap().len(), 3);
        assert!(bundle["platform"]["os"].is_string());
    }

    #[test]
    fn secrets_are_replaced_rather_than_removed() {
        // Removed, a reader assumes the field does not exist and asks for it
        // again; replaced, they can see there was something and stop asking.
        let value = json!({
            "auth_token": "ynwa-secret",
            "nested": {"api_key": "sk-123", "harmless": 7},
            "list": [{"password": "hunter2"}],
        });
        let clean = redact(&value);
        assert_eq!(clean["auth_token"], "[redacted]");
        assert_eq!(clean["nested"]["api_key"], "[redacted]");
        assert_eq!(clean["nested"]["harmless"], 7);
        assert_eq!(clean["list"][0]["password"], "[redacted]");
        assert!(leaks_in(&clean).is_empty());
    }

    #[test]
    fn a_field_nobody_thought_about_is_caught_by_the_check() {
        // The blocklist above is not the mechanism — the bundle is built by
        // naming what goes in. This is the second line, and it has to be able
        // to fail.
        let leaky = json!({"providers": [{"id": "unzoo", "auth_token": "left in"}]});
        let found = leaks_in(&leaky);
        assert_eq!(found, vec!["providers[0].auth_token".to_string()]);
    }

    #[test]
    fn the_collected_bundle_carries_no_secrets() {
        let _dir = isolate();
        let bundle = redact(&collect());
        assert!(leaks_in(&bundle).is_empty(), "{bundle:#}");
    }

    #[test]
    fn paths_become_shapes() {
        // Which directory it was matters far less than that there was one,
        // and a home directory carries the user's name.
        let _dir = isolate();
        let bundle = collect();
        let shape = bundle["state_dir"].as_str().unwrap_or_default();
        assert!(shape.starts_with("<path with"), "{shape}");
        assert!(!shape.contains('/'), "{shape}");
    }

    #[test]
    fn task_titles_do_not_travel() {
        // A task is called "fix the Henderson invoice", and that is the
        // customer's name, not ours.
        let _dir = isolate();
        let store = crate::cockpit::fleet_store::tasks().unwrap();
        store
            .create_task("brain", "fix the Henderson invoice")
            .unwrap();

        let bundle = serde_json::to_string(&redact(&collect())).unwrap();
        assert!(!bundle.contains("Henderson"), "{bundle}");
        // But the shape of the work is there, which is the part that helps.
        let parsed: Value = serde_json::from_str(&bundle).unwrap();
        assert_eq!(parsed["tasks"]["total"], 1);
    }

    #[test]
    fn the_bundle_is_written_where_it_was_asked_for() {
        let dir = isolate();
        let path = dir.path().join("reports/diagnostics.json");
        write(&path).unwrap();
        assert!(path.exists());
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(leaks_in(&written).is_empty());
    }
}
