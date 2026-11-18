//! Content-addressed storage for what tasks produce.
//!
//! A task's output can be a page of JSON or a screen recording. The index
//! belongs in the database; the bytes do not — a SQLite file with a video in
//! it is one nobody can copy, back up or open, and the failure shows up as a
//! slow terminal months later.
//!
//! So: bytes go to `<state>/artifacts/sha256/ab/abcdef…`, addressed by their
//! own hash. Three properties follow, and each is the reason for a decision
//! below.
//!
//! * **Identical content is stored once.** Two tasks that download the same
//!   file share a blob and keep separate rows, because provenance is not
//!   content.
//! * **A file cannot be quietly altered.** Its name is its hash, so
//!   [`verify`] is a comparison rather than a matter of trust.
//! * **Deleting is reference-counted.** Removing the last row that mentions a
//!   hash is what makes its bytes collectable; removing any earlier one must
//!   not, or one task's cleanup silently guts another task's evidence.

use anyhow::{anyhow, Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};
use unterm_tasks::{Artifact, NewArtifact};

/// Where the blobs live.
pub fn store_dir() -> PathBuf {
    let base = unterm_protocol::state_dir().unwrap_or_else(|| PathBuf::from(".unterm"));
    base.join("artifacts").join("sha256")
}

/// The path a hash's bytes live at.
///
/// Two levels, split on the first two characters: a hundred thousand
/// artifacts in one directory makes every listing slow on every filesystem
/// that has ever shipped.
pub fn blob_path(sha256: &str) -> PathBuf {
    let prefix = &sha256[..2.min(sha256.len())];
    store_dir().join(prefix).join(sha256)
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Hash a file without reading it into memory.
pub fn hash_file(path: impl AsRef<Path>) -> Result<(String, u64)> {
    use sha2::{Digest, Sha256};
    let path = path.as_ref();
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("open {} to hash it", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok((format!("{:x}", hasher.finalize()), total))
}

fn store() -> Result<std::sync::Arc<unterm_tasks::TaskStore>> {
    crate::cockpit::fleet_store::tasks().ok_or_else(|| anyhow!("there is no task store"))
}

/// Write bytes into the store and index them.
pub fn put_bytes(bytes: &[u8], mut spec: NewArtifact) -> Result<Artifact> {
    let sha256 = hash_bytes(bytes);
    let path = blob_path(&sha256);
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        // Written beside and renamed: a reader that finds a file under a hash
        // must be able to trust that the whole content is there. A partial
        // write left by a crash would be a blob whose name lies about it.
        let temporary = path.with_extension("partial");
        std::fs::write(&temporary, bytes)?;
        std::fs::rename(&temporary, &path)?;
    }
    spec.sha256 = sha256;
    spec.bytes = bytes.len() as i64;
    store()?.record_artifact(spec)
}

/// Take a file that already exists into the store.
///
/// Copied rather than moved: the file belongs to whoever made it, and a
/// screen recording that vanishes from where the user put it because Unterm
/// filed it away is a surprise nobody asked for.
pub fn put_file(path: impl AsRef<Path>, mut spec: NewArtifact) -> Result<Artifact> {
    let path = path.as_ref();
    let (sha256, bytes) = hash_file(path)?;
    let destination = blob_path(&sha256);
    if !destination.exists() {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = destination.with_extension("partial");
        std::fs::copy(path, &temporary)?;
        std::fs::rename(&temporary, &destination)?;
    }
    spec.sha256 = sha256;
    spec.bytes = bytes as i64;
    if spec.name.is_none() {
        spec.name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string());
    }
    store()?.record_artifact(spec)
}

/// Read an artifact's bytes back.
pub fn read(artifact: &Artifact) -> Result<Vec<u8>> {
    let path = blob_path(&artifact.sha256);
    std::fs::read(&path).with_context(|| format!("read artifact {} at {}", artifact.id, path.display()))
}

/// Whether the bytes on disk still hash to the name they are filed under.
///
/// A comparison, not a matter of trust: this is what makes an evidence bundle
/// checkable by somebody who was not there.
pub fn verify(artifact: &Artifact) -> Result<bool> {
    let path = blob_path(&artifact.sha256);
    if !path.exists() {
        return Ok(false);
    }
    let (actual, bytes) = hash_file(&path)?;
    Ok(actual == artifact.sha256 && bytes as i64 == artifact.bytes)
}

/// Forget an artifact, and delete its bytes if nothing else refers to them.
pub fn forget(id: &str) -> Result<bool> {
    let store = store()?;
    let Some(sha256) = store.forget_artifact(id)? else {
        return Ok(false);
    };
    if store.artifact_references(&sha256)? == 0 {
        // The last mention is gone, so the bytes are nobody's now. While any
        // row remains they belong to that row's task, whatever this caller
        // thought they were cleaning up.
        let _ = std::fs::remove_file(blob_path(&sha256));
    }
    Ok(true)
}

/// What the store is holding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Usage {
    pub artifacts: usize,
    /// Distinct blobs. Lower than `artifacts` when tasks produced identical
    /// content — the difference is what deduplication saved.
    pub blobs: usize,
    pub bytes: i64,
    pub unique_bytes: i64,
}

pub fn usage() -> Result<Usage> {
    let artifacts = store()?.artifacts()?;
    let mut seen: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    let mut total = 0;
    for artifact in &artifacts {
        total += artifact.bytes;
        seen.insert(artifact.sha256.clone(), artifact.bytes);
    }
    Ok(Usage {
        artifacts: artifacts.len(),
        blobs: seen.len(),
        bytes: total,
        unique_bytes: seen.values().sum(),
    })
}

/// How much the store may hold before old artifacts are dropped.
///
/// A quota that is never enforced is a number in a settings page. This one is
/// applied by [`enforce_quota`], which callers run after producing something.
pub const DEFAULT_QUOTA_BYTES: i64 = 2 * 1024 * 1024 * 1024;

/// What a sweep did, and whether it was enough.
///
/// `still_over` exists because the answer can honestly be "no": a running
/// task's evidence is never dropped, so a store full of live work stays over
/// quota. Reporting that is the difference between a limit a user can act on
/// and a number that is quietly wrong.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Sweep {
    pub dropped: Vec<String>,
    pub bytes_after: i64,
    pub still_over: bool,
    /// Bytes held by tasks that are still running, and therefore untouchable.
    pub held_by_live_work: i64,
}

/// Drop the oldest artifacts until the store fits, and report what went.
///
/// Oldest-first, and never anything belonging to a task that is still
/// running: retention is about old evidence, and deleting what a live task is
/// still producing would be a bug reported as data loss.
pub fn enforce_quota(limit_bytes: i64) -> Result<Sweep> {
    let store = store()?;
    let mut artifacts = store.artifacts()?;
    artifacts.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    let live: std::collections::BTreeSet<String> = store
        .tasks()?
        .into_iter()
        .filter(|task| !task.state.is_terminal())
        .map(|task| task.id.to_string())
        .collect();

    let mut total: i64 = artifacts.iter().map(|artifact| artifact.bytes).sum();
    let mut held = 0;
    let mut dropped = Vec::new();
    for artifact in artifacts {
        let is_live = artifact
            .task_id
            .as_deref()
            .is_some_and(|task| live.contains(task));
        if is_live {
            held += artifact.bytes;
            continue;
        }
        if total <= limit_bytes {
            continue;
        }
        total -= artifact.bytes;
        forget(&artifact.id)?;
        dropped.push(artifact.id);
    }
    Ok(Sweep {
        dropped,
        bytes_after: total,
        still_over: total > limit_bytes,
        held_by_live_work: held,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Isolated {
        _dir: tempfile::TempDir,
    }

    fn isolate() -> Isolated {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("UNTERM_STATE_DIR", dir.path());
        std::env::set_var("UNTERM_TASKS_DB", dir.path().join("tasks.db"));
        crate::cockpit::fleet_store::reset_for_tests();
        Isolated { _dir: dir }
    }

    fn spec(origin: &str) -> NewArtifact {
        NewArtifact {
            origin: origin.to_string(),
            ..NewArtifact::default()
        }
    }

    #[test]
    fn bytes_go_in_and_come_back() {
        let _isolated = isolate();
        let artifact = put_bytes(b"hello evidence", spec("test")).unwrap();
        assert_eq!(artifact.bytes, 14);
        assert_eq!(read(&artifact).unwrap(), b"hello evidence");
        assert!(verify(&artifact).unwrap());
    }

    #[test]
    fn the_same_content_is_stored_once_and_recorded_twice() {
        // Provenance is not content: two tasks that produced the same bytes
        // each keep their own row.
        let _isolated = isolate();
        let first = put_bytes(
            b"same",
            NewArtifact {
                task_id: Some("tsk_1".into()),
                ..spec("test")
            },
        )
        .unwrap();
        let second = put_bytes(
            b"same",
            NewArtifact {
                task_id: Some("tsk_2".into()),
                ..spec("test")
            },
        )
        .unwrap();

        assert_ne!(first.id, second.id);
        assert_eq!(first.sha256, second.sha256);
        let usage = usage().unwrap();
        assert_eq!(usage.artifacts, 2);
        assert_eq!(usage.blobs, 1, "the same bytes were stored twice");
    }

    #[test]
    fn forgetting_one_of_two_rows_does_not_delete_the_bytes() {
        // The failure this prevents: one task's cleanup silently gutting
        // another task's evidence.
        let _isolated = isolate();
        let first = put_bytes(b"shared", spec("test")).unwrap();
        let second = put_bytes(b"shared", spec("test")).unwrap();

        assert!(forget(&first.id).unwrap());
        assert!(
            blob_path(&second.sha256).exists(),
            "the surviving artifact lost its bytes"
        );
        assert_eq!(read(&second).unwrap(), b"shared");

        assert!(forget(&second.id).unwrap());
        assert!(
            !blob_path(&second.sha256).exists(),
            "the last reference went and the bytes stayed"
        );
    }

    #[test]
    fn a_tampered_blob_fails_verification() {
        let _isolated = isolate();
        let artifact = put_bytes(b"original", spec("test")).unwrap();
        std::fs::write(blob_path(&artifact.sha256), b"tampered").unwrap();
        assert!(
            !verify(&artifact).unwrap(),
            "content that no longer matches its own hash verified"
        );
    }

    #[test]
    fn a_missing_blob_fails_verification_rather_than_erroring() {
        // A bundle checker has to be able to report "the file is gone" as a
        // finding, not as a crash.
        let _isolated = isolate();
        let artifact = put_bytes(b"gone", spec("test")).unwrap();
        std::fs::remove_file(blob_path(&artifact.sha256)).unwrap();
        assert!(!verify(&artifact).unwrap());
    }

    #[test]
    fn a_file_is_copied_in_and_left_where_it_was() {
        let _isolated = isolate();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recording.cast");
        std::fs::write(&path, b"asciicast").unwrap();

        let artifact = put_file(&path, spec("recording")).unwrap();
        assert_eq!(artifact.name.as_deref(), Some("recording.cast"));
        assert!(path.exists(), "the user's file was moved out from under them");
        assert_eq!(read(&artifact).unwrap(), b"asciicast");
    }

    #[test]
    fn nothing_is_left_half_written_under_a_hash() {
        // The `.partial` name exists so a crash cannot leave a blob whose
        // name lies about its contents.
        let _isolated = isolate();
        let artifact = put_bytes(b"complete", spec("test")).unwrap();
        let leftovers: Vec<PathBuf> = walk(&store_dir())
            .into_iter()
            .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("partial"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        assert!(verify(&artifact).unwrap());
    }

    #[test]
    fn the_quota_drops_the_oldest_and_spares_live_work() {
        let _isolated = isolate();
        let store = crate::cockpit::fleet_store::tasks().unwrap();
        let running = store.create_task("brain", "still going").unwrap();

        let old = put_bytes(b"0123456789", spec("test")).unwrap();
        let mine = put_bytes(
            b"0123456789",
            NewArtifact {
                task_id: Some(running.id.to_string()),
                name: Some("live".into()),
                ..spec("test")
            },
        )
        .unwrap();
        // Distinct content, or the two rows would share a blob and the sizes
        // would not tell us anything.
        let newer = put_bytes(b"abcdefghij", spec("test")).unwrap();

        let sweep = enforce_quota(15).unwrap();
        assert!(sweep.dropped.contains(&old.id), "the oldest was not dropped");
        assert!(
            !sweep.dropped.contains(&mine.id),
            "a running task's evidence was deleted"
        );
        // What is left is exactly the live task's ten bytes, and the sweep
        // reports how much of the store it was not allowed to touch — the
        // number a user needs to understand why a limit is not being met.
        assert_eq!(sweep.held_by_live_work, 10);
        assert!(sweep.dropped.contains(&newer.id));
        assert_eq!(sweep.bytes_after, 10);
        assert!(!sweep.still_over);
    }

    #[test]
    fn a_store_full_of_live_work_says_it_is_still_over() {
        // The honest answer when retention cannot reach the limit, instead of
        // reporting a sweep that "succeeded" and a store that is still full.
        let _isolated = isolate();
        let store = crate::cockpit::fleet_store::tasks().unwrap();
        let running = store.create_task("brain", "still going").unwrap();
        put_bytes(
            b"0123456789",
            NewArtifact {
                task_id: Some(running.id.to_string()),
                ..spec("test")
            },
        )
        .unwrap();

        let sweep = enforce_quota(4).unwrap();
        assert!(sweep.dropped.is_empty());
        assert!(sweep.still_over, "a store over its limit reported success");
        assert_eq!(sweep.held_by_live_work, 10);
    }

    #[test]
    fn a_quota_that_is_already_met_drops_nothing() {
        let _isolated = isolate();
        put_bytes(b"small", spec("test")).unwrap();
        assert!(enforce_quota(DEFAULT_QUOTA_BYTES).unwrap().dropped.is_empty());
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return found;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(walk(&path));
            } else {
                found.push(path);
            }
        }
        found
    }
}
