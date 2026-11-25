//! Upgrading in a way that can be taken back.
//!
//! Two things go wrong when a terminal updates itself, and they fail
//! differently. The **binary** can be replaced with one that does not start —
//! recoverable, if the old one is still on disk. The **data** can be migrated
//! by a build that then turns out to be wrong — recoverable only if the old
//! bytes still exist, because a migration that succeeded and was wrong rolls
//! nothing back.
//!
//! So an upgrade here is: snapshot the data, stage the new version beside the
//! old one, swap, confirm it is healthy, and — if it is not — put both back.
//! The order is the whole design. Confirming before swapping would be
//! confirming something else; snapshotting after migrating would snapshot the
//! damage.
//!
//! What this module does not do is download anything or decide when to
//! upgrade. It is the part that must be right when somebody else has decided.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Where snapshots live.
pub fn snapshot_dir() -> Option<PathBuf> {
    unterm_protocol::state_path("snapshots")
}

/// A copy of the data as it was before an upgrade touched it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub path: String,
    pub taken_at: String,
    /// The version that produced this data. What a rollback restores *to*.
    pub version: String,
    pub files: Vec<String>,
}

/// The state that is worth putting back.
///
/// The task store and the identity pins, not the caches: a snapshot that
/// copies everything is one nobody takes because it is slow, and a snapshot
/// nobody takes protects nothing.
const PRECIOUS: &[&str] = &["tasks.db", "settings.json", "instances", "providers"];

/// Copy the data aside, and say what was copied.
pub fn snapshot(version: &str) -> Result<Snapshot> {
    let state = unterm_protocol::state_dir()
        .ok_or_else(|| anyhow!("there is no state directory to snapshot"))?;
    let root = snapshot_dir().ok_or_else(|| anyhow!("there is nowhere to put a snapshot"))?;
    let id = format!(
        "snap_{}_{}",
        version.replace(['.', ' '], "-"),
        chrono::Utc::now().format("%Y%m%d%H%M%S")
    );
    let destination = root.join(&id);
    std::fs::create_dir_all(&destination)
        .with_context(|| format!("create {}", destination.display()))?;

    let mut files = Vec::new();
    for name in PRECIOUS {
        let source = state.join(name);
        if !source.exists() {
            continue;
        }
        let target = destination.join(name);
        if source.is_dir() {
            copy_tree(&source, &target)?;
        } else {
            std::fs::copy(&source, &target)
                .with_context(|| format!("copy {}", source.display()))?;
        }
        files.push((*name).to_string());
    }

    let snapshot = Snapshot {
        id,
        path: destination.display().to_string(),
        taken_at: chrono::Utc::now().to_rfc3339(),
        version: version.to_string(),
        files,
    };
    // The manifest goes in last: a snapshot directory without one is one
    // whose copy was interrupted, and restoring from it would be restoring
    // half the data.
    std::fs::write(
        destination.join("snapshot.json"),
        serde_json::to_vec_pretty(&snapshot)?,
    )?;
    Ok(snapshot)
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Snapshots on disk, newest first.
pub fn snapshots() -> Vec<Snapshot> {
    let Some(root) = snapshot_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut found: Vec<Snapshot> = entries
        .flatten()
        .filter_map(|entry| {
            let manifest = entry.path().join("snapshot.json");
            let text = std::fs::read_to_string(manifest).ok()?;
            serde_json::from_str(&text).ok()
        })
        .collect();
    found.sort_by(|a, b| b.taken_at.cmp(&a.taken_at));
    found
}

/// Put the data back as a snapshot has it.
///
/// The current data is snapshotted first. Restoring is itself a change, and
/// somebody who rolls back by mistake must be able to roll forward again.
pub fn restore(id: &str, current_version: &str) -> Result<Snapshot> {
    let snapshot = snapshots()
        .into_iter()
        .find(|snapshot| snapshot.id == id)
        .ok_or_else(|| anyhow!("no such snapshot: {id}"))?;
    let state = unterm_protocol::state_dir()
        .ok_or_else(|| anyhow!("there is no state directory to restore into"))?;

    let _ = self::snapshot(current_version);

    for name in &snapshot.files {
        let source = PathBuf::from(&snapshot.path).join(name);
        let target = state.join(name);
        if !source.exists() {
            continue;
        }
        if source.is_dir() {
            let _ = std::fs::remove_dir_all(&target);
            copy_tree(&source, &target)?;
        } else {
            std::fs::copy(&source, &target)
                .with_context(|| format!("restore {}", target.display()))?;
        }
    }
    Ok(snapshot)
}

/// Keep the last `keep` snapshots and delete the rest.
pub fn prune_snapshots(keep: usize) -> Vec<String> {
    let mut dropped = Vec::new();
    for snapshot in snapshots().into_iter().skip(keep) {
        if std::fs::remove_dir_all(&snapshot.path).is_ok() {
            dropped.push(snapshot.id);
        }
    }
    dropped
}

/// How an upgrade went.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// The new version is in place and answered.
    Upgraded {
        from: String,
        to: String,
        snapshot: String,
    },
    /// It did not, and both the binary and the data are back as they were.
    RolledBack {
        from: String,
        attempted: String,
        snapshot: String,
        reason: String,
    },
}

/// Swap in a staged version, confirm it, and undo everything if it fails.
///
/// `confirm` is the caller's health check — starting the new binary and
/// asking it something. It is a parameter because "healthy" means different
/// things to an installer and to a test, and because a confirmation this
/// module wrote itself would be one that always passes.
pub fn swap_with_rollback(
    live: &Path,
    staged: &Path,
    from_version: &str,
    to_version: &str,
    confirm: impl FnOnce(&Path) -> Result<()>,
) -> Result<Outcome> {
    if !staged.exists() {
        return Err(anyhow!("{} was never staged", staged.display()));
    }
    // Data first. A migration that succeeds and is wrong rolls nothing back,
    // so the copy has to exist before the new version has ever opened it.
    let snapshot = snapshot(from_version)?;

    let previous = live.with_extension("previous");
    let _ = std::fs::remove_file(&previous);
    if live.exists() {
        std::fs::rename(live, &previous)
            .with_context(|| format!("move {} aside", live.display()))?;
    }
    if let Err(error) = std::fs::rename(staged, live) {
        // Nothing was swapped; put the old one straight back rather than
        // leaving a machine with no binary at all.
        if previous.exists() {
            let _ = std::fs::rename(&previous, live);
        }
        return Err(anyhow!("could not put the new version in place: {error}"));
    }

    match confirm(live) {
        Ok(()) => {
            let _ = std::fs::remove_file(&previous);
            Ok(Outcome::Upgraded {
                from: from_version.to_string(),
                to: to_version.to_string(),
                snapshot: snapshot.id,
            })
        }
        Err(reason) => {
            // Both halves go back, in the order that leaves the machine
            // usable at every point: binary first, because a running Unterm
            // with old data beats a machine with no Unterm at all.
            let _ = std::fs::remove_file(live);
            if previous.exists() {
                let _ = std::fs::rename(&previous, live);
            }
            let restored = restore(&snapshot.id, to_version)
                .map(|snapshot| snapshot.id)
                .unwrap_or_else(|_| snapshot.id.clone());
            Ok(Outcome::RolledBack {
                from: from_version.to_string(),
                attempted: to_version.to_string(),
                snapshot: restored,
                reason: reason.to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolate() -> crate::test_env::IsolatedEnv {
        crate::test_env::IsolatedEnv::new()
    }

    fn write_state(dir: &tempfile::TempDir, name: &str, contents: &str) {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn a_snapshot_copies_the_data_that_matters() {
        let dir = isolate();
        write_state(&dir, "tasks.db", "the tasks");
        write_state(&dir, "providers/unzoo.identity.json", "{}");
        write_state(&dir, "cache/enormous.bin", "not worth copying");

        let snapshot = snapshot("0.67.0").unwrap();
        assert!(snapshot.files.contains(&"tasks.db".to_string()));
        assert!(snapshot.files.contains(&"providers".to_string()));
        assert!(
            !snapshot.files.iter().any(|name| name == "cache"),
            "a snapshot that copies everything is one nobody takes"
        );
        let taken = PathBuf::from(&snapshot.path);
        assert_eq!(std::fs::read_to_string(taken.join("tasks.db")).unwrap(), "the tasks");
        assert!(taken.join("snapshot.json").exists());
    }

    #[test]
    fn restoring_puts_the_old_data_back() {
        let dir = isolate();
        write_state(&dir, "tasks.db", "before");
        let snapshot = snapshot("0.67.0").unwrap();
        write_state(&dir, "tasks.db", "after");

        restore(&snapshot.id, "0.68.0").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("tasks.db")).unwrap(),
            "before"
        );
    }

    #[test]
    fn rolling_back_is_itself_snapshotted() {
        // Somebody who rolls back by mistake has to be able to roll forward.
        let dir = isolate();
        write_state(&dir, "tasks.db", "before");
        let first = snapshot("0.67.0").unwrap();
        write_state(&dir, "tasks.db", "after");

        restore(&first.id, "0.68.0").unwrap();
        let all = snapshots();
        assert_eq!(all.len(), 2, "the state being replaced was not kept");
        let newest = PathBuf::from(&all[0].path).join("tasks.db");
        assert_eq!(std::fs::read_to_string(newest).unwrap(), "after");
    }

    #[test]
    fn a_healthy_upgrade_keeps_the_new_binary() {
        let dir = isolate();
        write_state(&dir, "tasks.db", "data");
        let live = dir.path().join("unterm");
        let staged = dir.path().join("unterm.staged");
        std::fs::write(&live, b"old").unwrap();
        std::fs::write(&staged, b"new").unwrap();

        let outcome =
            swap_with_rollback(&live, &staged, "0.67.0", "0.68.0", |_| Ok(())).unwrap();
        assert!(matches!(outcome, Outcome::Upgraded { .. }), "{outcome:?}");
        assert_eq!(std::fs::read(&live).unwrap(), b"new");
        assert!(
            !live.with_extension("previous").exists(),
            "the old binary was left behind after a successful upgrade"
        );
    }

    #[test]
    fn an_upgrade_that_does_not_come_up_restores_the_binary_and_the_data() {
        // M7's third gate.
        let dir = isolate();
        write_state(&dir, "tasks.db", "the data as it was");
        let live = dir.path().join("unterm");
        let staged = dir.path().join("unterm.staged");
        std::fs::write(&live, b"old").unwrap();
        std::fs::write(&staged, b"new and broken").unwrap();

        let outcome = swap_with_rollback(&live, &staged, "0.67.0", "0.68.0", |path| {
            // Stand in for the new version migrating the data and then
            // failing to answer.
            std::fs::write(dir.path().join("tasks.db"), "migrated by the bad build").unwrap();
            Err(anyhow!("{} never answered", path.display()))
        })
        .unwrap();

        match outcome {
            Outcome::RolledBack { reason, .. } => assert!(reason.contains("never answered")),
            other => panic!("a failed upgrade was not rolled back: {other:?}"),
        }
        assert_eq!(
            std::fs::read(&live).unwrap(),
            b"old",
            "the machine was left with the broken binary"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("tasks.db")).unwrap(),
            "the data as it was",
            "the data migrated by the bad build was left in place"
        );
    }

    #[test]
    fn a_version_that_was_never_staged_is_not_swapped_in() {
        let dir = isolate();
        let live = dir.path().join("unterm");
        std::fs::write(&live, b"old").unwrap();
        let error = swap_with_rollback(
            &live,
            &dir.path().join("nothing-here"),
            "0.67.0",
            "0.68.0",
            |_| Ok(()),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("never staged"), "{error}");
        assert_eq!(std::fs::read(&live).unwrap(), b"old");
    }

    #[test]
    fn pruning_keeps_the_newest() {
        let dir = isolate();
        write_state(&dir, "tasks.db", "data");
        for version in ["0.65.0", "0.66.0", "0.67.0"] {
            snapshot(version).unwrap();
            // The id carries a whole-second timestamp; without this the three
            // sort arbitrarily and the test asserts nothing.
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
        let dropped = prune_snapshots(2);
        assert_eq!(dropped.len(), 1);
        let left = snapshots();
        assert_eq!(left.len(), 2);
        assert_eq!(left[0].version, "0.67.0");
    }
}
