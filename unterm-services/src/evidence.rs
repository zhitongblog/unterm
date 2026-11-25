//! Everything that happened for one task, in a directory somebody else can check.
//!
//! The question this answers is not "what does Unterm think happened" — the
//! Cockpit answers that, from a database only Unterm can open. It is "can I
//! show somebody what happened, and can they tell whether it has been
//! edited". So a bundle is plain files with a manifest of hashes, and
//! [`verify`] recomputes rather than trusts.
//!
//! What goes in: the task, its runs and steps, the leases used and the
//! authorisation behind each, every provider call with its request and
//! response hashes, the artifacts produced (by content, deduplicated), and
//! the slice of the audit trail that names this task.
//!
//! What does not: anything belonging to another task. A bundle exported by
//! one person and read by another is exactly the situation where a stray row
//! from a different task is a leak rather than a curiosity.

use anyhow::{anyhow, Context, Result};
use serde_json::json;
use std::path::Path;
use unterm_tasks::TaskId;

/// The version of the bundle format written into every manifest.
///
/// A reader that finds a version it does not know must say so rather than
/// interpret the fields it recognises — a partially-understood evidence
/// bundle is worse than an unreadable one.
pub const FORMAT: &str = "unterm-evidence/1";

fn store() -> Result<std::sync::Arc<unterm_tasks::TaskStore>> {
    crate::cockpit::fleet_store::tasks().ok_or_else(|| anyhow!("there is no task store"))
}

fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Write a bundle for one task into `destination`.
///
/// Returns the manifest. The directory is created; an existing one is added
/// to rather than emptied, because deleting a directory somebody named is not
/// an export's business.
pub fn export(task_id: &str, destination: impl AsRef<Path>) -> Result<serde_json::Value> {
    let destination = destination.as_ref();
    let store = store()?;
    let id = TaskId::parse(task_id)?;
    let task = store
        .task(&id)?
        .ok_or_else(|| anyhow!("no such task: {task_id}"))?;

    std::fs::create_dir_all(destination)
        .with_context(|| format!("create {}", destination.display()))?;

    let runs = store.runs(&id)?;
    let mut steps = Vec::new();
    for run in &runs {
        steps.extend(store.steps(&run.id)?);
    }

    // Only this task's leases, and only the calls made under them. A lease
    // belonging to another task is not evidence about this one.
    let leases: Vec<unterm_tasks::Lease> = store
        .leases()?
        .into_iter()
        .filter(|lease| lease.task_id.as_deref() == Some(task_id))
        .collect();
    let mut chains = Vec::new();
    let mut calls = Vec::new();
    for lease in &leases {
        if let Some(chain) = store.authorisation_chain(&lease.id)? {
            chains.push(json!({
                "lease": chain.lease,
                "grant": chain.grant,
                "approval": chain.approval,
            }));
        }
        calls.extend(store.calls_under_lease(&lease.id)?);
    }

    let artifacts = store.artifacts_for_task(task_id)?;
    let blobs = destination.join("artifacts");
    if !artifacts.is_empty() {
        std::fs::create_dir_all(&blobs)?;
    }
    let mut artifact_entries = Vec::new();
    for artifact in &artifacts {
        // Named by hash inside the bundle, so two artifacts with the same
        // content are one file here as well, and a name a model chose cannot
        // collide with another or escape the directory.
        let file = blobs.join(&artifact.sha256);
        let mut present = file.exists();
        if !present {
            match crate::artifacts::read(artifact) {
                Ok(bytes) => {
                    std::fs::write(&file, bytes)?;
                    present = true;
                }
                // Recorded as missing rather than failing the export: an
                // artifact whose bytes were swept by retention is a fact
                // about the task, and a bundle that refuses to exist is not
                // more honest than one that says so.
                Err(_) => present = false,
            }
        }
        artifact_entries.push(json!({
            "id": artifact.id,
            "sha256": artifact.sha256,
            "bytes": artifact.bytes,
            "name": artifact.name,
            "media_type": artifact.media_type,
            "origin": artifact.origin,
            "created_at": artifact.created_at,
            "present": present,
        }));
    }

    let audit: Vec<serde_json::Value> = crate::audit_store::recent(usize::MAX)
        .into_iter()
        .filter(|entry| entry.get("task_id").and_then(|v| v.as_str()) == Some(task_id))
        .collect();

    let record = json!({
        "task": task,
        "runs": runs,
        "steps": steps,
        "leases": leases,
        "authorisation": chains,
        "calls": calls,
        "artifacts": artifact_entries,
        "audit": audit,
    });
    let record_bytes = serde_json::to_vec_pretty(&record)?;
    std::fs::write(destination.join("task.json"), &record_bytes)?;

    let manifest = json!({
        "format": FORMAT,
        "task_id": task_id,
        // Deliberately not a wall-clock "exported_at" inside the hashed
        // record: the manifest is what changes between two exports of the
        // same task, and the record is what stays identical.
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "unterm_version": unterm_protocol::PRODUCT_VERSION,
        "record_sha256": digest(&record_bytes),
        "counts": {
            "runs": runs.len(),
            "steps": steps.len(),
            "leases": leases.len(),
            "calls": calls.len(),
            "artifacts": artifacts.len(),
            "audit": audit.len(),
        },
        "artifacts": artifact_entries,
    });
    std::fs::write(
        destination.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(manifest)
}

/// What checking a bundle found.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct Verdict {
    pub intact: bool,
    pub format: Option<String>,
    pub task_id: Option<String>,
    /// Everything wrong, not just the first thing: somebody handed a broken
    /// bundle wants the list, not a game of twenty questions.
    pub problems: Vec<String>,
    pub artifacts_checked: usize,
    pub artifacts_missing: usize,
}

/// Recompute a bundle's hashes and report whether it still holds together.
pub fn verify(bundle: impl AsRef<Path>) -> Result<Verdict> {
    let bundle = bundle.as_ref();
    let manifest_bytes = std::fs::read(bundle.join("manifest.json"))
        .with_context(|| format!("read the manifest in {}", bundle.display()))?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)?;
    let mut verdict = Verdict {
        format: manifest
            .get("format")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        task_id: manifest
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        ..Verdict::default()
    };

    if verdict.format.as_deref() != Some(FORMAT) {
        verdict.problems.push(format!(
            "this build reads {FORMAT}; the bundle says {}",
            verdict.format.clone().unwrap_or_else(|| "nothing".into())
        ));
        // Nothing below can be trusted to mean what it appears to.
        return Ok(verdict);
    }

    match std::fs::read(bundle.join("task.json")) {
        Ok(record) => {
            let expected = manifest
                .get("record_sha256")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if digest(&record) != expected {
                verdict
                    .problems
                    .push("task.json does not match the hash in the manifest".into());
            }
        }
        Err(error) => verdict.problems.push(format!("task.json is unreadable: {error}")),
    }

    for artifact in manifest
        .get("artifacts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
    {
        let Some(sha256) = artifact.get("sha256").and_then(|v| v.as_str()) else {
            verdict.problems.push("an artifact entry has no hash".into());
            continue;
        };
        let path = bundle.join("artifacts").join(sha256);
        if !path.exists() {
            if artifact.get("present").and_then(|v| v.as_bool()) == Some(false) {
                // The export already said these bytes were gone. Honest at
                // export time and honest here.
                verdict.artifacts_missing += 1;
            } else {
                verdict
                    .problems
                    .push(format!("the artifact {sha256} is missing from the bundle"));
            }
            continue;
        }
        verdict.artifacts_checked += 1;
        let bytes = std::fs::read(&path)?;
        if digest(&bytes) != sha256 {
            verdict
                .problems
                .push(format!("the artifact {sha256} does not match its own hash"));
        }
    }

    verdict.intact = verdict.problems.is_empty();
    Ok(verdict)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unterm_tasks::{NewArtifact, NewLease};

    fn isolate() -> crate::test_env::IsolatedEnv {
        crate::test_env::IsolatedEnv::new()
    }

    /// A task with a run, a step, a lease, a call and an artifact.
    fn a_task_that_did_something() -> (String, std::path::PathBuf) {
        let store = crate::cockpit::fleet_store::tasks().unwrap();
        let task = store.create_task("browse", "check the dashboard").unwrap();
        let run = store.start_run(&task.id).unwrap();
        store.request_step(&run.id, "turn", None).unwrap();
        let lease = store
            .issue_lease(NewLease {
                provider: "unzoo".into(),
                capability: "browser".into(),
                task_id: Some(task.id.to_string()),
                ttl_seconds: 300,
                ..NewLease::default()
            })
            .unwrap();
        let slot = store
            .begin_call(
                None,
                "unzoo",
                "browser",
                "tab_list",
                Some(&lease.id),
                &json!({}),
            )
            .unwrap();
        store
            .finish_call(&slot.record().id, "succeeded", Some(&json!({"tabs": []})), None)
            .unwrap();
        crate::artifacts::put_bytes(
            b"a screenshot, allegedly",
            NewArtifact {
                task_id: Some(task.id.to_string()),
                origin: "provider.call".into(),
                name: Some("shot.png".into()),
                ..NewArtifact::default()
            },
        )
        .unwrap();
        (task.id.to_string(), std::path::PathBuf::new())
    }

    #[test]
    fn a_bundle_holds_the_whole_story_and_verifies() {
        let dir = isolate();
        let (task_id, _) = a_task_that_did_something();
        let out = dir.path().join("bundle");

        let manifest = export(&task_id, &out).unwrap();
        assert_eq!(manifest["format"], FORMAT);
        assert_eq!(manifest["counts"]["runs"], 1);
        assert_eq!(manifest["counts"]["steps"], 1);
        assert_eq!(manifest["counts"]["leases"], 1);
        assert_eq!(manifest["counts"]["calls"], 1);
        assert_eq!(manifest["counts"]["artifacts"], 1);

        let verdict = verify(&out).unwrap();
        assert!(verdict.intact, "{verdict:?}");
        assert_eq!(verdict.artifacts_checked, 1);
        assert_eq!(verdict.task_id.as_deref(), Some(task_id.as_str()));
    }

    #[test]
    fn editing_the_record_breaks_the_bundle() {
        let dir = isolate();
        let (task_id, _) = a_task_that_did_something();
        let out = dir.path().join("bundle");
        export(&task_id, &out).unwrap();

        let path = out.join("task.json");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("check the dashboard", "something else")).unwrap();

        let verdict = verify(&out).unwrap();
        assert!(!verdict.intact);
        assert!(verdict.problems[0].contains("task.json"), "{verdict:?}");
    }

    #[test]
    fn replacing_an_artifact_breaks_the_bundle() {
        // The reason artifacts are stored by hash inside the bundle: swapping
        // one has to be visible without anybody remembering what it was.
        let dir = isolate();
        let (task_id, _) = a_task_that_did_something();
        let out = dir.path().join("bundle");
        let manifest = export(&task_id, &out).unwrap();

        let sha = manifest["artifacts"][0]["sha256"].as_str().unwrap();
        std::fs::write(out.join("artifacts").join(sha), b"a different screenshot").unwrap();

        let verdict = verify(&out).unwrap();
        assert!(!verdict.intact);
        assert!(
            verdict.problems.iter().any(|p| p.contains("its own hash")),
            "{verdict:?}"
        );
    }

    #[test]
    fn a_bundle_from_a_format_we_do_not_know_is_refused_rather_than_half_read() {
        let dir = isolate();
        let out = dir.path().join("bundle");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(
            out.join("manifest.json"),
            serde_json::to_vec(&json!({"format": "unterm-evidence/99", "task_id": "tsk_x"}))
                .unwrap(),
        )
        .unwrap();

        let verdict = verify(&out).unwrap();
        assert!(!verdict.intact);
        assert_eq!(verdict.problems.len(), 1, "{verdict:?}");
        assert!(verdict.problems[0].contains("unterm-evidence/99"));
    }

    #[test]
    fn another_tasks_work_stays_out() {
        // A bundle is handed to somebody else; a stray row from a different
        // task is a leak, not a curiosity.
        let dir = isolate();
        let (mine, _) = a_task_that_did_something();
        let (theirs, _) = a_task_that_did_something();
        let out = dir.path().join("bundle");
        export(&mine, &out).unwrap();

        let record = std::fs::read_to_string(out.join("task.json")).unwrap();
        assert!(record.contains(&mine));
        assert!(
            !record.contains(&theirs),
            "another task's records were exported"
        );
    }

    #[test]
    fn a_swept_artifact_is_reported_missing_rather_than_failing_the_export() {
        let dir = isolate();
        let (task_id, _) = a_task_that_did_something();
        let store = crate::cockpit::fleet_store::tasks().unwrap();
        let artifact = store.artifacts_for_task(&task_id).unwrap().remove(0);
        std::fs::remove_file(crate::artifacts::blob_path(&artifact.sha256)).unwrap();

        let out = dir.path().join("bundle");
        let manifest = export(&task_id, &out).unwrap();
        assert_eq!(manifest["artifacts"][0]["present"], false);

        let verdict = verify(&out).unwrap();
        assert!(verdict.intact, "a known-missing artifact broke the bundle: {verdict:?}");
        assert_eq!(verdict.artifacts_missing, 1);
    }

    #[test]
    fn two_exports_of_an_unchanged_task_carry_the_same_record_hash() {
        // The manifest changes (it has a timestamp); the record does not. A
        // record that differed every time would make "has this been edited"
        // unanswerable.
        let dir = isolate();
        let (task_id, _) = a_task_that_did_something();
        let first = export(&task_id, dir.path().join("one")).unwrap();
        let second = export(&task_id, dir.path().join("two")).unwrap();
        assert_eq!(first["record_sha256"], second["record_sha256"]);
    }

    #[test]
    fn a_task_nobody_created_cannot_be_exported() {
        let dir = isolate();
        let error = export("tsk_invented", dir.path().join("bundle"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("no such task"), "{error}");
    }
}
