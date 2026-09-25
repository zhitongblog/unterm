//! Review — checkpoints, diffs, rollback, and merge for agent-produced
//! changes. Serves both the `review.*` MCP methods and the Web Review
//! page's HTTP API.
//!
//! Checkpoints are non-invasive: a temporary index captures the whole
//! worktree (tracked + untracked) as a dangling commit — HEAD, the real
//! index, and the files are never touched. `~/.unterm/checkpoints.json`
//! remembers the last N per repo so the Review page can diff/roll back
//! "loose" agent runs (fleet members use their worktree's start commit
//! instead and don't need entries here).

use anyhow::{anyhow, bail, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    OnceLock,
};

const MAX_CHECKPOINTS_PER_REPO: usize = 20;
/// Don't re-snapshot the same repo more often than this.
const DEBOUNCE_SECS: i64 = 60;
static NEXT_TMP_INDEX: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub sha: String,
    pub at: String, // RFC3339
    pub agent: String,
    pub pane_id: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CheckpointFile {
    /// repo root path → newest-first checkpoints.
    repos: HashMap<String, Vec<Checkpoint>>,
}

fn path() -> Option<PathBuf> {
    unterm_protocol::state_path("checkpoints.json")
}

fn store() -> &'static Mutex<CheckpointFile> {
    static S: OnceLock<Mutex<CheckpointFile>> = OnceLock::new();
    S.get_or_init(|| {
        let file = path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Mutex::new(file)
    })
}

fn save_locked(file: &CheckpointFile) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(body) = serde_json::to_string_pretty(file) {
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    git_env(repo, args, &[])
}

fn git_env(repo: &Path, args: &[&str], envs: &[(&str, &str)]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().with_context(|| format!("run git {args:?}"))?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn repo_root(cwd: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(git(cwd, &["rev-parse", "--show-toplevel"])?))
}

/// Snapshot the entire worktree (tracked + untracked) as a dangling
/// commit without touching HEAD / index / files. Returns the sha.
pub fn snapshot_worktree(repo: &Path) -> Result<String> {
    let tmp = std::env::temp_dir().join(format!(
        "unterm-ckpt-index-{}-{}-{}",
        std::process::id(),
        NEXT_TMP_INDEX.fetch_add(1, Ordering::Relaxed),
        chrono::Utc::now().timestamp_millis()
    ));
    let tmp_str = tmp.to_string_lossy().to_string();
    let envs: &[(&str, &str)] = &[("GIT_INDEX_FILE", &tmp_str)];
    let result = (|| {
        // Seed the temp index from HEAD, then absorb every change
        // including untracked files.
        git_env(repo, &["read-tree", "HEAD"], envs)?;
        git_env(repo, &["add", "-A"], envs)?;
        let tree = git_env(repo, &["write-tree"], envs)?;
        let head = git(repo, &["rev-parse", "HEAD"])?;
        git_env(
            repo,
            &[
                "commit-tree",
                &tree,
                "-p",
                &head,
                "-m",
                "unterm cockpit checkpoint",
            ],
            envs,
        )
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

/// Record an automatic checkpoint for a repo (debounced).
pub fn record_auto_checkpoint(repo: &Path, agent: &str, pane_id: u64) -> Result<Option<String>> {
    let root = repo_root(repo)?;
    let key = root.to_string_lossy().to_string();
    {
        let s = store().lock();
        if let Some(list) = s.repos.get(&key) {
            if let Some(latest) = list.first() {
                if let Ok(at) = chrono::DateTime::parse_from_rfc3339(&latest.at) {
                    if chrono::Utc::now().timestamp() - at.timestamp() < DEBOUNCE_SECS {
                        return Ok(None);
                    }
                }
            }
        }
    }
    let sha = snapshot_worktree(&root)?;
    let mut s = store().lock();
    let list = s.repos.entry(key).or_default();
    // Skip when nothing changed since the previous checkpoint.
    if list.first().map(|c| c.sha == sha).unwrap_or(false) {
        return Ok(None);
    }
    list.insert(
        0,
        Checkpoint {
            sha: sha.clone(),
            at: chrono::Utc::now().to_rfc3339(),
            agent: agent.to_string(),
            pane_id: Some(pane_id),
        },
    );
    list.truncate(MAX_CHECKPOINTS_PER_REPO);
    save_locked(&s);
    Ok(Some(sha))
}

pub fn checkpoints() -> HashMap<String, Vec<Checkpoint>> {
    store().lock().repos.clone()
}

/// File-level + line-level diff of the worktree against `from` (a sha).
/// Untracked files are synthesized as additions so agent-created files
/// show up (plain `git diff <sha>` misses them).
pub fn diff(repo: &Path, from: &str) -> Result<Value> {
    let root = repo_root(repo)?;
    let numstat = git(&root, &["diff", "--numstat", from])?;
    let mut files: Vec<Value> = numstat
        .lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            let add = it.next()?.trim().to_string();
            let del = it.next()?.trim().to_string();
            let path = it.next()?.trim().to_string();
            Some(json!({ "path": path, "added": add, "deleted": del, "untracked": false }))
        })
        .collect();
    let patch = git(&root, &["diff", from])?;
    let mut patches = vec![patch];

    // Untracked (not yet in the object DB) — synthesize add-diffs.
    let untracked = git(&root, &["ls-files", "--others", "--exclude-standard"])?;
    for f in untracked.lines().filter(|l| !l.is_empty()) {
        files.push(json!({ "path": f, "added": "?", "deleted": "0", "untracked": true }));
        // git diff --no-index exits 1 when files differ; ignore status.
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&root).args(["diff", "--no-index", "--"]);
        #[cfg(not(windows))]
        cmd.args(["/dev/null", f]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.args(["NUL", f]);
            cmd.creation_flags(0x0800_0000);
        }
        if let Ok(out) = cmd.output() {
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            if !text.is_empty() {
                patches.push(text);
            }
        }
    }

    Ok(json!({
        "repo": root.to_string_lossy(),
        "from": from,
        "files": files,
        "patch": patches.join("\n"),
    }))
}

/// Restore the worktree to a checkpoint: files AND untracked state become
/// exactly the snapshot's content. Destructive for anything newer — the
/// callers gate this behind explicit confirmation.
pub fn rollback(repo: &Path, sha: &str) -> Result<()> {
    let root = repo_root(repo)?;
    // Validate the sha exists first — a typo must not half-run.
    git(&root, &["cat-file", "-e", &format!("{sha}^{{commit}}")])?;
    let expected_tree = git(&root, &["rev-parse", &format!("{sha}^{{tree}}")])?;
    // Reset the temporary index and worktree together.  `checkout-index`
    // only writes entries that exist in the target tree; it does not remove
    // a path that is tracked by HEAD but was deleted in the checkpoint.  In
    // that case the old file survived as an apparent untracked file on some
    // Git/platform combinations.  `read-tree --reset -u` applies both sides
    // of the tree transition, including deletions, without moving HEAD.
    git(&root, &["read-tree", "--reset", "-u", sha])?;
    // Remove files that exist now but not in the snapshot.
    git(&root, &["clean", "-fd"])?;
    // Leave the index back at HEAD so `git status` reads naturally.
    git(&root, &["read-tree", "HEAD"])?;
    // Re-snapshot through Git's normal filters and verify that every
    // repository-visible file now matches the requested checkpoint. This
    // permits platform checkout rules such as core.autocrlf while catching
    // partial restores.
    let restored = snapshot_worktree(&root)?;
    let restored_tree = git(&root, &["rev-parse", &format!("{restored}^{{tree}}")])?;
    if restored_tree != expected_tree {
        bail!(
            "rollback verification failed: restored tree {restored_tree} does not match checkpoint tree {expected_tree}"
        );
    }
    Ok(())
}

/// Squash-merge a fleet member's branch into the base repo, leaving the
/// result staged (no commit — the user owns the commit).
pub fn merge_member(fleet_id: &str, member: &str) -> Result<Value> {
    merge_member_with_policy(fleet_id, member, false)
}

/// Merge with an explicit verification override. `force` is intentionally
/// surfaced only by audited API/CLI paths; the normal Review UI stays gated.
pub fn merge_member_with_policy(fleet_id: &str, member: &str, force: bool) -> Result<Value> {
    let verification = if force {
        super::verification::latest_for_member(fleet_id, member)
    } else {
        Some(super::verification::ensure_passed(fleet_id, member)?)
    };
    let fleet = super::fleet::get(fleet_id).ok_or_else(|| anyhow!("no fleet {fleet_id:?}"))?;
    let m = super::fleet::resolve_member(&fleet, member)?;
    let base_head = git(&fleet.base_repo, &["rev-parse", "HEAD"])?;
    let dirty = git(&fleet.base_repo, &["status", "--porcelain"])?;
    if !dirty.is_empty() {
        bail!("base repo not clean — commit or stash before merging a fleet member");
    }
    // What an agent leaves is usually uncommitted: it edits, it tests, it
    // stops. The diff and the verification both saw that work, so the merge
    // takes it too -- committed on the member's own branch, in the worktree
    // the fleet made for it, rather than refused as "no changes".
    commit_leftover_work(&m.worktree, &fleet.task, &m.branch)?;
    let member_head = git(
        &fleet.base_repo,
        &["rev-parse", "--verify", &format!("{}^{{commit}}", m.branch)],
    )?;
    git(&fleet.base_repo, &["merge", "--squash", &m.branch])?;
    let staged_files: Vec<String> = git(&fleet.base_repo, &["diff", "--cached", "--name-only"])?
        .lines()
        .map(str::to_owned)
        .collect();
    if staged_files.is_empty() {
        bail!("fleet member produced no staged changes to review");
    }
    super::fleet::set_review_state(fleet_id, member, super::fleet::ReviewState::Merged)?;
    Ok(json!({
        "ok": true,
        "fleet": fleet_id,
        "branch": m.branch,
        "base_head": base_head,
        "member_head": member_head,
        "merged_at": chrono::Utc::now().to_rfc3339(),
        "staged_files": staged_files,
        "staged_in": fleet.base_repo.to_string_lossy(),
        "verification": verification,
        "verification_forced": force,
    }))
}

/// Commit whatever a fleet member left uncommitted in its worktree.
fn commit_leftover_work(worktree: &Path, task: &str, branch: &str) -> Result<()> {
    if git(worktree, &["status", "--porcelain"])?.is_empty() {
        return Ok(());
    }
    git(worktree, &["add", "-A"])?;
    let message = format!("{branch}: {task}");
    // An agent's worktree may have no identity configured; the squash that
    // follows is committed by the person anyway.
    let known = git(worktree, &["config", "user.email"]).unwrap_or_default();
    let mut args: Vec<&str> = Vec::new();
    if known.trim().is_empty() {
        args.extend(["-c", "user.name=Unterm fleet", "-c", "user.email=fleet@unterm.invalid"]);
    }
    args.extend(["commit", "-q", "--no-verify", "-m", &message]);
    git(worktree, &args)?;
    Ok(())
}

pub fn discard_member(fleet_id: &str, member: &str) -> Result<Value> {
    super::fleet::set_review_state(fleet_id, member, super::fleet::ReviewState::Discarded)?;
    Ok(json!({ "ok": true, "fleet": fleet_id, "member": member }))
}

/// Everything the Review page needs in one call: fleets with member
/// summaries + loose checkpoints per repo.
pub fn overview() -> Value {
    let fleets: Vec<Value> = super::fleet::list()
        .iter()
        .map(|f| {
            let members: Vec<Value> = f
                .members
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let state = m
                        .pane_id
                        .and_then(super::status::status_for_pane)
                        .map(|s| s.state.as_str().to_string());
                    json!({
                        "n": i + 1,
                        "agent": m.agent,
                        "branch": m.branch,
                        "worktree": m.worktree.to_string_lossy(),
                        "checkpoint": m.checkpoint,
                        "review": m.review,
                        "pane_id": m.pane_id,
                        "agent_state": state,
                        "attempt": m.attempt,
                        "last_started_at": m.last_started_at,
                        "last_launch_error": m.last_launch_error,
                    })
                })
                .collect();
            json!({
                "id": f.id,
                "task": f.task,
                "base_repo": f.base_repo.to_string_lossy(),
                "base_branch": f.base_branch,
                "created_at": f.created_at,
                "members": members,
            })
        })
        .collect();
    let checkpoints: Vec<Value> = checkpoints()
        .into_iter()
        .map(|(repo, list)| json!({ "repo": repo, "checkpoints": list }))
        .collect();
    json!({ "fleets": fleets, "checkpoints": checkpoints })
}

#[cfg(test)]
mod tests {
    use super::*;

    static NEXT_TMP_REPO: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmp_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "unterm-review-test-{}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_millis(),
            NEXT_TMP_REPO.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]).unwrap();
        git(&dir, &["config", "user.email", "t@t"]).unwrap();
        git(&dir, &["config", "user.name", "t"]).unwrap();
        // Keep fixture bytes deterministic on Windows; production rollback
        // intentionally honors each repository's checkout filters.
        git(&dir, &["config", "core.autocrlf", "false"]).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(&dir, &["add", "-A"]).unwrap();
        git(&dir, &["commit", "-q", "-m", "init"]).unwrap();
        dir
    }

    #[test]
    fn snapshot_diff_rollback_roundtrip() {
        let repo = tmp_repo();
        let base = snapshot_worktree(&repo).unwrap();

        // Mutate: edit a tracked file + add an untracked one.
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(repo.join("new.txt"), "fresh\n").unwrap();

        let d = diff(&repo, &base).unwrap();
        let files = d["files"].as_array().unwrap();
        assert!(files.iter().any(|f| f["path"] == "a.txt"));
        assert!(files
            .iter()
            .any(|f| f["path"] == "new.txt" && f["untracked"] == true));
        assert!(d["patch"].as_str().unwrap().contains("+two"));
        assert!(d["patch"].as_str().unwrap().contains("+fresh"));

        // Snapshot never touched the worktree.
        assert_eq!(
            std::fs::read_to_string(repo.join("a.txt")).unwrap(),
            "one\ntwo\n"
        );

        rollback(&repo, &base).unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.join("a.txt")).unwrap(),
            "one\n"
        );
        assert!(!repo.join("new.txt").exists());

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn rollback_restores_a_checkpoint_with_a_deleted_tracked_file() {
        let repo = tmp_repo();
        std::fs::remove_file(repo.join("a.txt")).unwrap();
        let without_a = snapshot_worktree(&repo).unwrap();

        std::fs::write(repo.join("a.txt"), "newer\n").unwrap();
        std::fs::write(repo.join("extra.txt"), "extra\n").unwrap();
        rollback(&repo, &without_a).unwrap();

        assert!(!repo.join("a.txt").exists());
        assert!(!repo.join("extra.txt").exists());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn invalid_rollback_target_does_not_touch_the_worktree() {
        let repo = tmp_repo();
        std::fs::write(repo.join("a.txt"), "keep me\n").unwrap();

        assert!(rollback(&repo, "definitely-not-a-commit").is_err());
        assert_eq!(
            std::fs::read_to_string(repo.join("a.txt")).unwrap(),
            "keep me\n"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }
}

#[cfg(test)]
mod leftover_work_tests {
    use super::{commit_leftover_work, git};

    #[test]
    fn an_agents_uncommitted_edits_are_committed_on_its_branch() {
        let repo = std::env::temp_dir().join(format!("unterm-leftover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]).unwrap();
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        git(&repo, &["add", "-A"]).unwrap();
        git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "base"]).unwrap();
        std::fs::write(repo.join("b.txt"), "b").unwrap();
        commit_leftover_work(&repo, "add b", "fleet/add-b-1").unwrap();
        assert!(git(&repo, &["status", "--porcelain"]).unwrap().is_empty());
        assert_eq!(git(&repo, &["log", "-1", "--format=%s"]).unwrap(), "fleet/add-b-1: add b");
        // Nothing left over is nothing to do.
        commit_leftover_work(&repo, "add b", "fleet/add-b-1").unwrap();
        assert_eq!(git(&repo, &["rev-list", "--count", "HEAD"]).unwrap(), "2");
        let _ = std::fs::remove_dir_all(&repo);
    }
}
