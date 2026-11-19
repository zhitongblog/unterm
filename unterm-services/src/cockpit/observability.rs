//! Low-overhead Fleet observability for the Review overview response.
//!
//! Git can take a noticeable amount of time in a large worktree, so overview
//! requests never run it inline.  They return the last cached sample and kick
//! a stale/missing sample to a background thread.  The Review page already
//! polls the overview endpoint and will naturally pick up the fresh values.

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const SAMPLE_TTL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
struct WorktreeSample {
    changed_files: usize,
    additions: u64,
    deletions: u64,
    untracked_files: usize,
    ahead_commits: u64,
    worktree_health: WorktreeHealth,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum WorktreeHealth {
    Healthy,
    Missing,
    Invalid,
    Error,
}

struct CachedSample {
    sampled_at: Instant,
    sample: WorktreeSample,
}

#[derive(Default)]
struct SampleStore {
    samples: HashMap<PathBuf, CachedSample>,
    in_flight: HashSet<PathBuf>,
}

fn store() -> &'static Mutex<SampleStore> {
    static STORE: OnceLock<Mutex<SampleStore>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(SampleStore::default()))
}

/// Add live, cached metrics to the regular Review overview response.
///
/// The stable member fields are:
/// - `elapsed_seconds`: wall time since the fleet was launched
/// - `metrics_pending`: whether a first/refresh sample is being collected
/// - `changed_files`, `additions`, `deletions`, `untracked_files`
/// - `ahead_commits`, `worktree_health`
pub fn enrich_overview(mut overview: Value) -> Value {
    let Some(fleets) = overview.get_mut("fleets").and_then(Value::as_array_mut) else {
        return overview;
    };

    for fleet in fleets {
        let elapsed = fleet
            .get("created_at")
            .and_then(Value::as_str)
            .and_then(elapsed_seconds);
        let Some(members) = fleet.get_mut("members").and_then(Value::as_array_mut) else {
            continue;
        };

        for member in members {
            if let (Some(object), Some(elapsed)) = (member.as_object_mut(), elapsed) {
                object.insert("elapsed_seconds".into(), elapsed.into());
            }

            let Some(worktree) = member
                .get("worktree")
                .and_then(Value::as_str)
                .map(PathBuf::from)
            else {
                continue;
            };
            let checkpoint = member
                .get("checkpoint")
                .and_then(Value::as_str)
                .unwrap_or("HEAD")
                .to_owned();

            let (sample, pending) = cached_or_schedule(worktree, checkpoint);
            if let Some(object) = member.as_object_mut() {
                object.insert("metrics_pending".into(), pending.into());
                if let Some(sample) = sample {
                    if let Ok(Value::Object(metrics)) = serde_json::to_value(sample) {
                        object.extend(metrics);
                    }
                }
            }
        }
    }
    overview
}

fn elapsed_seconds(created_at: &str) -> Option<u64> {
    let created = chrono::DateTime::parse_from_rfc3339(created_at).ok()?;
    let seconds = chrono::Utc::now()
        .signed_duration_since(created.with_timezone(&chrono::Utc))
        .num_seconds();
    Some(seconds.max(0) as u64)
}

fn cached_or_schedule(worktree: PathBuf, checkpoint: String) -> (Option<WorktreeSample>, bool) {
    let mut samples = store().lock();
    let stale = samples
        .samples
        .get(&worktree)
        .map(|cached| cached.sampled_at.elapsed() >= SAMPLE_TTL)
        .unwrap_or(true);
    let sample = samples
        .samples
        .get(&worktree)
        .map(|cached| cached.sample.clone());
    let should_spawn = stale && samples.in_flight.insert(worktree.clone());
    drop(samples);

    if should_spawn {
        let worker_worktree = worktree.clone();
        let spawn_result = std::thread::Builder::new()
            .name("fleet-observability".into())
            .spawn(move || {
                let sample = sample_worktree(&worker_worktree, &checkpoint);
                let mut samples = store().lock();
                samples.in_flight.remove(&worker_worktree);
                samples.samples.insert(
                    worker_worktree,
                    CachedSample {
                        sampled_at: Instant::now(),
                        sample,
                    },
                );
            });
        if spawn_result.is_err() {
            store().lock().in_flight.remove(&worktree);
        }
    }

    (sample, should_spawn)
}

fn sample_worktree(worktree: &Path, checkpoint: &str) -> WorktreeSample {
    if !worktree.exists() {
        return empty_sample(WorktreeHealth::Missing);
    }
    match git(worktree, &["rev-parse", "--is-inside-work-tree"]) {
        Ok(value) if value.trim() == "true" => {}
        Ok(_) => return empty_sample(WorktreeHealth::Invalid),
        Err(_) => return empty_sample(WorktreeHealth::Error),
    }

    let mut sample = empty_sample(WorktreeHealth::Healthy);
    if let Ok(numstat) = git(worktree, &["diff", "--numstat", checkpoint, "--"]) {
        for line in numstat.lines() {
            let mut columns = line.splitn(3, '\t');
            let additions = columns.next().and_then(|v| v.parse::<u64>().ok());
            let deletions = columns.next().and_then(|v| v.parse::<u64>().ok());
            if columns.next().is_some() {
                sample.changed_files += 1;
                sample.additions += additions.unwrap_or(0);
                sample.deletions += deletions.unwrap_or(0);
            }
        }
    }
    if let Ok(status) = git(
        worktree,
        &["status", "--porcelain", "--untracked-files=normal"],
    ) {
        sample.untracked_files = status.lines().filter(|line| line.starts_with("??")).count();
        sample.changed_files += sample.untracked_files;
    }
    if let Ok(ahead) = git(
        worktree,
        &["rev-list", "--count", &format!("{checkpoint}..HEAD")],
    ) {
        sample.ahead_commits = ahead.trim().parse().unwrap_or(0);
    }
    sample
}

fn empty_sample(worktree_health: WorktreeHealth) -> WorktreeSample {
    WorktreeSample {
        changed_files: 0,
        additions: 0,
        deletions: 0,
        untracked_files: 0,
        ahead_commits: 0,
        worktree_health,
    }
}

fn git(repo: &Path, args: &[&str]) -> std::io::Result<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(std::process::Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_is_non_negative() {
        assert_eq!(elapsed_seconds("2999-01-01T00:00:00Z"), Some(0));
        assert!(elapsed_seconds("2020-01-01T00:00:00Z").unwrap() > 0);
        assert_eq!(elapsed_seconds("invalid"), None);
    }

    #[test]
    fn missing_worktree_is_reported() {
        let missing = std::env::temp_dir().join(format!(
            "unterm-missing-observability-{}",
            std::process::id()
        ));
        let value = serde_json::to_value(sample_worktree(&missing, "HEAD")).unwrap();
        assert_eq!(value["worktree_health"], "missing");
    }
}
