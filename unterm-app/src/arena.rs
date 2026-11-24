//! Native Fleet Arena — launch 2-3 AI coding agents in parallel isolated Git
//! worktrees on the same task, compare live diff stats side-by-side, and
//! 1-click squash-merge the winner while cleaning up all losing worktrees.

use crate::worktree::{compute_diff_stat, create_worktree, remove_worktree, squash_merge_worktree, DiffStat};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArenaMember {
    pub agent: String,
    pub worktree_path: PathBuf,
    pub branch: String,
    pub pane_id: Option<usize>,
    #[serde(default)]
    pub files_changed: usize,
    #[serde(default)]
    pub added: usize,
    #[serde(default)]
    pub deleted: usize,
    pub exit_code: Option<i32>,
}

impl ArenaMember {
    pub fn diff_stat(&self) -> DiffStat {
        DiffStat {
            files: self.files_changed,
            added: self.added,
            deleted: self.deleted,
        }
    }

    pub fn refresh_diff(&mut self) {
        if self.worktree_path.exists() {
            let stat = compute_diff_stat(&self.worktree_path);
            self.files_changed = stat.files;
            self.added = stat.added;
            self.deleted = stat.deleted;
        }
    }

    pub fn status_badge(&self) -> String {
        let diff_str = self
            .diff_stat()
            .badge()
            .unwrap_or_else(|| "clean".to_string());
        match self.exit_code {
            Some(0) => format!("✓ exit 0 · {diff_str}"),
            Some(code) => format!("✗ exit {code} · {diff_str}"),
            None => format!("● {diff_str}"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArenaRun {
    pub id: String,
    pub repo_root: PathBuf,
    pub task: String,
    pub created_at: u64,
    pub members: Vec<ArenaMember>,
}

impl ArenaRun {
    pub fn refresh_all_diffs(&mut self) {
        for member in &mut self.members {
            member.refresh_diff();
        }
    }
}

fn arena_store_path() -> PathBuf {
    unterm_protocol::state_dir()
        .unwrap_or_else(|| PathBuf::from(".").join(".unterm"))
        .join("arena-runs.json")
}

pub fn load_runs() -> Vec<ArenaRun> {
    let path = arena_store_path();
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub fn save_runs(runs: &[ArenaRun]) {
    let path = arena_store_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(runs) {
        let _ = std::fs::write(path, json);
    }
}

/// Create isolated worktrees for each agent in `agents` under `repo_dir`.
pub fn create_arena_run(
    repo_dir: &Path,
    task: &str,
    agents: &[String],
) -> Result<ArenaRun, String> {
    if agents.is_empty() {
        return Err("No agents specified for Fleet Arena".to_string());
    }
    let root_out = crate::git::hidden_command("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| format!("git failed: {e}"))?;
    if !root_out.status.success() {
        return Err("Current directory is not inside a Git repository".to_string());
    }
    let repo_root = PathBuf::from(String::from_utf8_lossy(&root_out.stdout).trim());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let id = format!("arena-{}", now % 100_000);

    let mut members = Vec::with_capacity(agents.len());
    for agent in agents {
        let hint = format!("{id}-{agent}");
        let (worktree_path, branch) = create_worktree(&repo_root, &hint)?;
        members.push(ArenaMember {
            agent: agent.clone(),
            worktree_path,
            branch,
            pane_id: None,
            files_changed: 0,
            added: 0,
            deleted: 0,
            exit_code: None,
        });
    }

    let run = ArenaRun {
        id,
        repo_root,
        task: task.to_string(),
        created_at: now,
        members,
    };
    let mut runs = load_runs();
    runs.insert(0, run.clone());
    runs.truncate(12);
    save_runs(&runs);
    Ok(run)
}

/// Squash-merge the winning branch into the main repo and remove all other worktrees in the run.
pub fn pick_winner(run_id: &str, winning_branch: &str) -> Result<String, String> {
    let mut runs = load_runs();
    let Some(pos) = runs.iter().position(|r| r.id == run_id) else {
        return Err(format!("Arena run '{run_id}' not found"));
    };
    let run = runs.remove(pos);
    let Some(winner) = run.members.iter().find(|m| m.branch == winning_branch) else {
        runs.insert(pos, run);
        save_runs(&runs);
        return Err(format!("Branch '{winning_branch}' not found in arena '{run_id}'"));
    };

    let msg = squash_merge_worktree(&run.repo_root, &winner.worktree_path, &winner.branch)?;

    // Clean up all losing worktrees and delete their temporary branches.
    for member in &run.members {
        if member.branch != winning_branch {
            let _ = remove_worktree(&run.repo_root, &member.worktree_path);
        }
        let _ = crate::git::hidden_command("git")
            .args(["branch", "-D", &member.branch])
            .current_dir(&run.repo_root)
            .output();
    }
    save_runs(&runs);
    Ok(msg)
}

/// Discard an arena run and remove all of its temporary worktrees and branches.
pub fn cleanup_run(run_id: &str) -> Result<usize, String> {
    let mut runs = load_runs();
    let Some(pos) = runs.iter().position(|r| r.id == run_id) else {
        return Err(format!("Arena run '{run_id}' not found"));
    };
    let run = runs.remove(pos);
    let count = run.members.len();
    for member in &run.members {
        let _ = remove_worktree(&run.repo_root, &member.worktree_path);
        let _ = crate::git::hidden_command("git")
            .args(["branch", "-D", &member.branch])
            .current_dir(&run.repo_root)
            .output();
    }
    save_runs(&runs);
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_member_status_badges_with_diff_and_exit_code() {
        let m1 = ArenaMember {
            agent: "claude".into(),
            worktree_path: PathBuf::from("/tmp/wt-claude"),
            branch: "wt/arena-1-claude".into(),
            pane_id: Some(2),
            files_changed: 3,
            added: 42,
            deleted: 7,
            exit_code: Some(0),
        };
        assert_eq!(m1.status_badge(), "✓ exit 0 · +42 -7");

        let m2 = ArenaMember {
            agent: "codex".into(),
            worktree_path: PathBuf::from("/tmp/wt-codex"),
            branch: "wt/arena-1-codex".into(),
            pane_id: Some(3),
            files_changed: 0,
            added: 0,
            deleted: 0,
            exit_code: None,
        };
        assert_eq!(m2.status_badge(), "● clean");
    }

    #[test]
    fn arena_run_round_trips_through_json() {
        let run = ArenaRun {
            id: "arena-42".into(),
            repo_root: PathBuf::from("/repo"),
            task: "Fix auth".into(),
            created_at: 1700000000,
            members: vec![ArenaMember {
                agent: "agy".into(),
                worktree_path: PathBuf::from("/repo.worktrees/arena-42-agy"),
                branch: "wt/arena-42-agy".into(),
                pane_id: Some(5),
                files_changed: 2,
                added: 15,
                deleted: 4,
                exit_code: Some(0),
            }],
        };
        let encoded = serde_json::to_string(&run).unwrap();
        let decoded: ArenaRun = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, run);
    }
}
