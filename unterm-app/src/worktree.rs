//! Native Git Worktree Studio — zero-conflict parallel workspaces for AI agents.
//!
//! When multiple AI agents work in the same repository directory at once, they
//! overwrite each other's uncommitted edits and contend for `.git/index.lock`.
//! This module lets the user spin up isolated Git worktrees in one click, shows
//! live `+added -deleted` diff stats per worktree in the sidebar and palette,
//! and supports 1-click squash-merge back to the base branch or clean removal.

use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const STAT_TTL: Duration = Duration::from_secs(3);

/// One active Git worktree discovered via `git worktree list --porcelain`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub head: String,
    pub branch: Option<String>,
    pub is_main: bool,
}

/// Short diff statistics (`+added -deleted` and changed file count) for a worktree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiffStat {
    pub files: usize,
    pub added: usize,
    pub deleted: usize,
}

impl DiffStat {
    pub fn is_clean(&self) -> bool {
        self.files == 0 && self.added == 0 && self.deleted == 0
    }

    /// Format as a compact badge for the sidebar or palette (`+42 -12` or `*3`).
    pub fn badge(&self) -> Option<String> {
        if self.is_clean() {
            return None;
        }
        if self.added > 0 || self.deleted > 0 {
            Some(format!("+{} -{}", self.added, self.deleted))
        } else {
            Some(format!("*{}", self.files))
        }
    }
}

struct StatCache {
    entries: HashMap<PathBuf, (Instant, DiffStat)>,
    in_flight: HashSet<PathBuf>,
}

fn stat_cache() -> &'static Mutex<StatCache> {
    static CACHE: OnceLock<Mutex<StatCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(StatCache {
            entries: HashMap::new(),
            in_flight: HashSet::new(),
        })
    })
}

/// Non-blocking diff stat lookup for the UI thread (sidebar / topbar).
/// Schedules a background `git diff --numstat HEAD` refresh when stale.
pub fn cached_diff_stat(directory: &Path) -> Option<DiffStat> {
    let mut cache = stat_cache().lock();
    let existing = cache.entries.get(directory).copied();
    let stale = existing
        .map(|(at, _)| at.elapsed() >= STAT_TTL)
        .unwrap_or(true);
    if stale && cache.in_flight.insert(directory.to_path_buf()) {
        let dir = directory.to_path_buf();
        let spawned = std::thread::Builder::new()
            .name("wt-diff-stat".into())
            .spawn(move || {
                let stat = compute_diff_stat(&dir);
                let mut cache = stat_cache().lock();
                cache.in_flight.remove(&dir);
                cache.entries.insert(dir, (Instant::now(), stat));
                crate::mcp_host::request_repaint();
            });
        if spawned.is_err() {
            stat_cache().lock().in_flight.remove(directory);
        }
    }
    existing.map(|(_, stat)| stat).filter(|s| !s.is_clean())
}

/// Compute working-tree `+added -deleted` and untracked file count synchronously.
pub fn compute_diff_stat(directory: &Path) -> DiffStat {
    let mut stat = DiffStat::default();
    if let Ok(out) = crate::git::hidden_command("git")
        .args(["diff", "--numstat", "HEAD", "--"])
        .current_dir(directory)
        .output()
    {
        if out.status.success() {
            stat = parse_numstat(&String::from_utf8_lossy(&out.stdout));
        }
    }
    if let Ok(out) = crate::git::hidden_command("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(directory)
        .output()
    {
        if out.status.success() {
            let untracked = String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count();
            stat.files += untracked;
        }
    }
    stat
}

/// Parse `git diff --numstat` output into total files, additions, and deletions.
pub fn parse_numstat(output: &str) -> DiffStat {
    let mut stat = DiffStat::default();
    for line in output.lines() {
        let mut parts = line.split('\t');
        let Some(add_str) = parts.next() else {
            continue;
        };
        let Some(del_str) = parts.next() else {
            continue;
        };
        let Some(path) = parts.next() else {
            continue;
        };
        if path.trim().is_empty() {
            continue;
        }
        stat.files += 1;
        stat.added += add_str.trim().parse::<usize>().unwrap_or(0);
        stat.deleted += del_str.trim().parse::<usize>().unwrap_or(0);
    }
    stat
}

/// Parse `git worktree list --porcelain` output.
pub fn parse_worktree_list(output: &str) -> Vec<WorktreeInfo> {
    let mut list = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_head = String::new();
    let mut current_branch: Option<String> = None;

    let flush = |list: &mut Vec<WorktreeInfo>,
                 path: &mut Option<PathBuf>,
                 head: &mut String,
                 branch: &mut Option<String>| {
        if let Some(p) = path.take() {
            let is_main = list.is_empty();
            list.push(WorktreeInfo {
                path: p,
                head: std::mem::take(head),
                branch: branch.take(),
                is_main,
            });
        }
    };

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            flush(
                &mut list,
                &mut current_path,
                &mut current_head,
                &mut current_branch,
            );
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("worktree ") {
            flush(
                &mut list,
                &mut current_path,
                &mut current_head,
                &mut current_branch,
            );
            current_path = Some(PathBuf::from(rest.trim()));
        } else if let Some(rest) = trimmed.strip_prefix("HEAD ") {
            current_head = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("branch ") {
            let b = rest
                .trim()
                .strip_prefix("refs/heads/")
                .unwrap_or(rest.trim())
                .to_string();
            current_branch = Some(b);
        }
    }
    flush(
        &mut list,
        &mut current_path,
        &mut current_head,
        &mut current_branch,
    );
    list
}

/// List all worktrees belonging to the repository at `directory`.
pub fn list_worktrees(directory: &Path) -> Vec<WorktreeInfo> {
    let Ok(out) = crate::git::hidden_command("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(directory)
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_worktree_list(&String::from_utf8_lossy(&out.stdout))
}

/// Sanitize a user task or label into a clean git branch / folder slug.
pub fn slugify_name(input: &str) -> String {
    let slug: String = input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() % 10000)
            .unwrap_or(1);
        format!("agent-{secs}")
    } else {
        slug.chars().take(28).collect()
    }
}

/// Create a new isolated Git worktree for `repo_dir` under `<parent>/<repo>.worktrees/<slug>`
/// on branch `wt/<slug>`. Returns `(worktree_path, branch_name)`.
pub fn create_worktree(repo_dir: &Path, name_hint: &str) -> Result<(PathBuf, String), String> {
    let root_out = crate::git::hidden_command("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(repo_dir)
        .output()
        .map_err(|e| format!("git failed: {e}"))?;
    if !root_out.status.success() {
        return Err("Not inside a Git repository".to_string());
    }
    let root = PathBuf::from(String::from_utf8_lossy(&root_out.stdout).trim());
    let repo_name = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo");
    let parent = root.parent().unwrap_or(&root);
    let slug = slugify_name(name_hint);
    let container = parent.join(format!("{repo_name}.worktrees"));
    let _ = std::fs::create_dir_all(&container);

    // Pick a non-colliding folder and branch name.
    let mut candidate_slug = slug.clone();
    let mut suffix = 2usize;
    while container.join(&candidate_slug).exists() {
        candidate_slug = format!("{slug}-{suffix}");
        suffix += 1;
    }
    let wt_path = container.join(&candidate_slug);
    let branch = format!("wt/{candidate_slug}");

    let out = crate::git::hidden_command("git")
        .args([
            "worktree",
            "add",
            "-b",
            &branch,
            &wt_path.to_string_lossy(),
            "HEAD",
        ])
        .current_dir(&root)
        .output()
        .map_err(|e| format!("git worktree add failed: {e}"))?;

    if !out.status.success() {
        // Branch might already exist; retry without `-b` or with timestamped branch.
        let stamp_branch = format!(
            "wt/{candidate_slug}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() % 1000)
                .unwrap_or(1)
        );
        let retry = crate::git::hidden_command("git")
            .args([
                "worktree",
                "add",
                "-b",
                &stamp_branch,
                &wt_path.to_string_lossy(),
                "HEAD",
            ])
            .current_dir(&root)
            .output()
            .map_err(|e| format!("git worktree add failed: {e}"))?;
        if !retry.status.success() {
            return Err(String::from_utf8_lossy(&retry.stderr).trim().to_string());
        }
        return Ok((wt_path, stamp_branch));
    }

    Ok((wt_path, branch))
}

/// Auto-commit any uncommitted work in `worktree_path`, squash-merge `branch`
/// into `main_repo_path` (leaving changes staged for inspection), and remove
/// the secondary worktree.
pub fn squash_merge_worktree(
    main_repo_path: &Path,
    worktree_path: &Path,
    branch: &str,
) -> Result<String, String> {
    // 1. Check if the worktree has uncommitted changes; if so, commit them onto its branch.
    let status_out = crate::git::hidden_command("git")
        .args(["status", "--porcelain"])
        .current_dir(worktree_path)
        .output()
        .map_err(|e| e.to_string())?;
    if !String::from_utf8_lossy(&status_out.stdout).trim().is_empty() {
        let _ = crate::git::hidden_command("git")
            .args(["add", "-A"])
            .current_dir(worktree_path)
            .output();
        let _ = crate::git::hidden_command("git")
            .args(["commit", "-m", &format!("unterm worktree snapshot ({branch})")])
            .current_dir(worktree_path)
            .output();
    }

    // 2. Squash-merge `branch` into `main_repo_path`.
    let merge_out = crate::git::hidden_command("git")
        .args(["merge", "--squash", branch])
        .current_dir(main_repo_path)
        .output()
        .map_err(|e| e.to_string())?;
    if !merge_out.status.success() {
        return Err(String::from_utf8_lossy(&merge_out.stderr).trim().to_string());
    }

    // 3. Remove the worktree now that its changes are staged in the main repo.
    let _ = remove_worktree(main_repo_path, worktree_path);
    Ok(format!("Squash-merged {branch} into main worktree"))
}

/// Remove a secondary Git worktree and prune stale metadata.
pub fn remove_worktree(main_repo_path: &Path, worktree_path: &Path) -> Result<(), String> {
    let out = crate::git::hidden_command("git")
        .args([
            "worktree",
            "remove",
            "--force",
            &worktree_path.to_string_lossy(),
        ])
        .current_dir(main_repo_path)
        .output()
        .map_err(|e| e.to_string())?;
    let _ = crate::git::hidden_command("git")
        .args(["worktree", "prune"])
        .current_dir(main_repo_path)
        .output();
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_worktree_list() {
        let raw = "\
worktree /Volumes/Dev/code/unterm
HEAD 519ae4e500000000000000000000000000000000
branch refs/heads/master

worktree /Volumes/Dev/code/unterm.worktrees/feat-ui
HEAD 3aa535de00000000000000000000000000000000
branch refs/heads/wt/feat-ui
";
        let list = parse_worktree_list(raw);
        assert_eq!(list.len(), 2);
        assert!(list[0].is_main);
        assert_eq!(list[0].branch.as_deref(), Some("master"));
        assert!(!list[1].is_main);
        assert_eq!(list[1].branch.as_deref(), Some("wt/feat-ui"));
    }

    #[test]
    fn parses_numstat_and_formats_badge() {
        let raw = "42\t12\tsrc/window.rs\n8\t0\tsrc/worktree.rs\n";
        let stat = parse_numstat(raw);
        assert_eq!(
            stat,
            DiffStat {
                files: 2,
                added: 50,
                deleted: 12
            }
        );
        assert_eq!(stat.badge().as_deref(), Some("+50 -12"));
        assert_eq!(DiffStat::default().badge(), None);
    }

    #[test]
    fn slugifies_names_cleanly() {
        assert_eq!(slugify_name("Fix Login & Auth Bug!"), "fix-login-auth-bug");
        assert!(slugify_name("   ").starts_with("agent-"));
    }
}
