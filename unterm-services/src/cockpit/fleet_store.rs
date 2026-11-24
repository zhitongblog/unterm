//! Where fleets actually live now.
//!
//! They used to live in `~/.unterm/fleets.json`, rewritten whole on every
//! change. That file was the Cockpit's second source of truth: a crash
//! between the write and the rename lost the lot, two writers could not both
//! be right, and nothing about a half-finished member survived a restart in a
//! form anybody could act on.
//!
//! This projects the same `Fleet` and `FleetMember` the rest of the code
//! already speaks onto the durable task engine — a fleet is a task, a member
//! is a step — so the shapes callers see are unchanged while the storage
//! underneath gains transactions, a state machine and crash recovery.
//!
//! The member's *execution* state is the step's state; its *review* state
//! (pending / merged / discarded) is disposition, not execution, and stays in
//! the step's opaque detail. Collapsing the two would lose the difference
//! between "the agent is still working" and "I have not looked at it yet".

use super::fleet::{Fleet, FleetMember, ReviewState};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use unterm_tasks::{State, Step, TaskStore};

/// The kind marking a task as a fleet, and a step as one of its members.
const FLEET_KIND: &str = "fleet";
const MEMBER_KIND: &str = "fleet.member";

fn database_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("UNTERM_TASKS_DB") {
        return Some(PathBuf::from(path));
    }
    // A test binary must not be able to reach the real database, whether or
    // not the test remembered to say so. Relying on every test to call the
    // reset helper first is a rule that holds until one test does not, and
    // the cost of that miss is writing into the developer's live `~/.unterm`
    // — which is exactly what happened the first time this ran.
    #[cfg(any(test, feature = "test-support"))]
    {
        return Some(std::env::temp_dir().join(format!(
            "unterm-tasks-test-{}.db",
            std::process::id()
        )));
    }
    #[cfg(not(any(test, feature = "test-support")))]
    unterm_protocol::state_path("tasks.db")
}

fn slot() -> &'static Mutex<Option<Option<Arc<TaskStore>>>> {
    static STORE: OnceLock<Mutex<Option<Option<Arc<TaskStore>>>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(None))
}

/// Drop the open store so the next call reopens from the current path.
///
/// Tests only. Without it the first test to touch a fleet would pin the
/// database for the whole process — and, worse, pin it at whatever path was
/// in effect then, which is how a test run ends up writing into the user's
/// real `~/.unterm`.
#[cfg(any(test, feature = "test-support"))]
pub fn reset_for_tests() {
    if let Ok(mut slot) = slot().lock() {
        *slot = None;
    }
}

/// The process-wide store. One writer — since M1 every fleet operation runs
/// in the Core — so a single connection is the whole concurrency story.
pub fn tasks() -> Option<Arc<TaskStore>> {
    let mut slot = slot().lock().ok()?;
    slot.get_or_insert_with(|| {
            let store = match database_path() {
                Some(path) => TaskStore::open(path),
                None => TaskStore::in_memory(),
            };
            match store {
                Ok(store) => {
                    // Whatever the last life left mid-flight becomes a verdict
                    // before anybody reads it.
                    match store.recover() {
                        Ok(recovery) if !recovery.is_clean() => {
                            eprintln!(
                                "unterm: recovered {} step(s), {} run(s), {} task(s) left running by a previous session",
                                recovery.steps_interrupted.len(),
                                recovery.runs_interrupted.len(),
                                recovery.tasks_interrupted.len()
                            );
                        }
                        Ok(_) => {}
                        Err(error) => eprintln!("unterm: task recovery failed: {error:#}"),
                    }
                    Some(Arc::new(store))
                }
                Err(error) => {
                    eprintln!("unterm: could not open the task store: {error:#}");
                    None
                }
            }
        })
        .clone()
}

fn member_to_detail(member: &FleetMember) -> serde_json::Value {
    serde_json::json!({
        "agent": member.agent,
        "agent_cmd": member.agent_cmd,
        "worktree": member.worktree,
        "branch": member.branch,
        "pane_id": member.pane_id,
        "checkpoint": member.checkpoint,
        "review": member.review,
        "attempt": member.attempt,
        "last_started_at": member.last_started_at,
        "last_launch_error": member.last_launch_error,
    })
}

fn detail_to_member(step: &Step) -> Option<FleetMember> {
    let detail = &step.detail;
    Some(FleetMember {
        agent: detail.get("agent")?.as_str()?.to_string(),
        agent_cmd: detail
            .get("agent_cmd")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        worktree: PathBuf::from(detail.get("worktree")?.as_str()?),
        branch: detail.get("branch")?.as_str()?.to_string(),
        pane_id: detail.get("pane_id").and_then(|v| v.as_u64()),
        checkpoint: detail
            .get("checkpoint")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        review: detail
            .get("review")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(ReviewState::Pending),
        attempt: detail
            .get("attempt")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as u32,
        last_started_at: detail
            .get("last_started_at")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        last_launch_error: detail
            .get("last_launch_error")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

/// A member's execution state, as the engine understands it.
///
/// Review disposition is deliberately not folded in here: a merged member and
/// a still-unreviewed one both *ran*, and the Cockpit needs to say which of
/// those two things it is talking about.
fn member_state(member: &FleetMember) -> State {
    match member.review {
        // Reviewed either way means its run is over and accounted for.
        ReviewState::Merged | ReviewState::Discarded => State::Succeeded,
        ReviewState::Pending if member.last_launch_error.is_some() => State::Failed,
        ReviewState::Pending if member.pane_id.is_some() => State::Running,
        ReviewState::Pending => State::Pending,
    }
}

/// Every fleet, oldest first, exactly as the old JSON file ordered them.
pub fn load_all() -> Vec<Fleet> {
    let Some(store) = tasks() else {
        return Vec::new();
    };
    let tasks_list = match store.tasks() {
        Ok(tasks) => tasks,
        Err(error) => {
            eprintln!("unterm: could not read fleets: {error:#}");
            return Vec::new();
        }
    };
    let mut fleets = Vec::new();
    for task in tasks_list.into_iter().filter(|t| t.kind == FLEET_KIND) {
        let detail = &task.detail;
        let Some(id) = detail.get("fleet_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let mut members = Vec::new();
        if let Ok(runs) = store.runs(&task.id) {
            for run in &runs {
                if let Ok(steps) = store.steps(&run.id) {
                    members.extend(
                        steps
                            .iter()
                            .filter(|step| step.kind == MEMBER_KIND)
                            .filter_map(detail_to_member),
                    );
                }
            }
        }
        fleets.push(Fleet {
            id: id.to_string(),
            task: task.title.clone(),
            base_repo: detail
                .get("base_repo")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_default(),
            base_branch: detail
                .get("base_branch")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            members,
            created_at: detail
                .get("created_at")
                .and_then(|v| v.as_str())
                .unwrap_or(&task.created_at)
                .to_string(),
        });
    }
    fleets
}

/// Make the store hold exactly these fleets.
///
/// Mirrors what the whole-file rewrite used to mean, so every existing caller
/// keeps its semantics — but as a set of transactions rather than one
/// truncate-and-write that a crash could catch halfway.
pub fn save_all(fleets: &[Fleet]) {
    let Some(store) = tasks() else { return };
    if let Err(error) = write_all(&store, fleets) {
        eprintln!("unterm: could not persist fleets: {error:#}");
    }
}

fn write_all(store: &TaskStore, fleets: &[Fleet]) -> anyhow::Result<()> {
    let existing = store.tasks()?;
    let wanted: std::collections::HashSet<&str> = fleets.iter().map(|f| f.id.as_str()).collect();

    // Fleets the caller dropped — `fleet.clean` is the usual reason.
    for task in existing.iter().filter(|t| t.kind == FLEET_KIND) {
        let id = task.detail.get("fleet_id").and_then(|v| v.as_str());
        if id.is_some_and(|id| !wanted.contains(id)) {
            store.delete_task(&task.id)?;
        }
    }

    for fleet in fleets {
        let detail = serde_json::json!({
            "fleet_id": fleet.id,
            "base_repo": fleet.base_repo,
            "base_branch": fleet.base_branch,
            "created_at": fleet.created_at,
        });
        let existing_task = existing.iter().find(|task| {
            task.kind == FLEET_KIND
                && task.detail.get("fleet_id").and_then(|v| v.as_str()) == Some(fleet.id.as_str())
        });
        let task = match existing_task {
            Some(task) => {
                store.set_task_detail(&task.id, detail)?;
                task.clone()
            }
            None => store.create_task_with_detail(FLEET_KIND, &fleet.task, detail)?,
        };

        // One run holds the members. Retries live in the member's own attempt
        // counter, which is what `fleet retry` has always meant, so a second
        // run here would invent a second meaning for the same word.
        let run = match store.runs(&task.id)?.into_iter().next() {
            Some(run) => run,
            None => store.start_run(&task.id)?,
        };
        let known = store.steps(&run.id)?;
        for member in &fleet.members {
            let key = format!("{}:{}", fleet.id, member.branch);
            let detail = member_to_detail(member);
            match known
                .iter()
                .find(|step| step.idempotency_key.as_deref() == Some(key.as_str()))
            {
                Some(step) => store.set_step_detail(&step.id, detail, member_state(member))?,
                None => {
                    let created = store.request_step_with_detail(
                        &run.id,
                        MEMBER_KIND,
                        Some(&key),
                        detail,
                    )?;
                    store.set_step_detail(
                        &created.step().id,
                        member_to_detail(member),
                        member_state(member),
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// Bring `fleets.json` across, once, and take it out of service.
///
/// Returns how many fleets were imported. The file is renamed rather than
/// deleted: it is the only copy of that history, and a migration that eats
/// its input is one nobody can check afterwards.
pub fn migrate_legacy_json() -> usize {
    let Some(path) = legacy_path() else { return 0 };
    if !path.exists() {
        return 0;
    }
    let Some(store) = tasks() else { return 0 };
    let already = store
        .tasks()
        .map(|tasks| tasks.iter().any(|t| t.kind == FLEET_KIND))
        .unwrap_or(false);

    let imported = if already {
        // Somebody already has fleets in the database. Importing on top would
        // resurrect fleets they cleaned; leave the file alone and say nothing.
        0
    } else {
        let legacy: Vec<Fleet> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|value| {
                serde_json::from_value::<Vec<Fleet>>(value.get("fleets")?.clone()).ok()
            })
            .unwrap_or_default();
        if !legacy.is_empty() {
            save_all(&legacy);
        }
        legacy.len()
    };

    let retired = path.with_extension("json.migrated");
    if std::fs::rename(&path, &retired).is_ok() && imported > 0 {
        eprintln!(
            "unterm: imported {imported} fleet(s) from {} (kept as {})",
            path.display(),
            retired.display()
        );
    }
    imported
}

fn legacy_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("UNTERM_FLEETS_PATH") {
        return Some(PathBuf::from(path));
    }
    unterm_protocol::state_path("fleets.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test gets its own database, or they inherit each other's fleets.
    fn isolate() -> crate::test_env::IsolatedEnv {
        crate::test_env::IsolatedEnv::new()
    }

    fn member(branch: &str, review: ReviewState) -> FleetMember {
        FleetMember {
            agent: "claude".to_string(),
            agent_cmd: "claude 'do it'".to_string(),
            worktree: PathBuf::from(format!("/tmp/wt/{branch}")),
            branch: branch.to_string(),
            pane_id: Some(7),
            checkpoint: "abc123".to_string(),
            review,
            attempt: 2,
            last_started_at: Some("2026-08-15T00:00:00Z".to_string()),
            last_launch_error: None,
        }
    }

    fn fleet(id: &str) -> Fleet {
        Fleet {
            id: id.to_string(),
            task: "ship the thing".to_string(),
            base_repo: PathBuf::from("/tmp/repo"),
            base_branch: "master".to_string(),
            members: vec![
                member("fleet/a-1", ReviewState::Pending),
                member("fleet/a-2", ReviewState::Merged),
            ],
            created_at: "2026-08-15T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn a_fleet_survives_the_round_trip_field_for_field() {
        let _dir = isolate();
        let original = fleet("f1");
        save_all(&[original.clone()]);

        let loaded = load_all();
        assert_eq!(loaded.len(), 1);
        let back = &loaded[0];
        assert_eq!(back.id, original.id);
        assert_eq!(back.task, original.task);
        assert_eq!(back.base_repo, original.base_repo);
        assert_eq!(back.base_branch, original.base_branch);
        assert_eq!(back.created_at, original.created_at);
        assert_eq!(back.members.len(), 2);
        for (got, want) in back.members.iter().zip(original.members.iter()) {
            assert_eq!(got.agent, want.agent);
            assert_eq!(got.agent_cmd, want.agent_cmd);
            assert_eq!(got.worktree, want.worktree);
            assert_eq!(got.branch, want.branch);
            assert_eq!(got.pane_id, want.pane_id);
            assert_eq!(got.checkpoint, want.checkpoint);
            assert_eq!(got.review, want.review);
            assert_eq!(got.attempt, want.attempt, "the retry count must survive");
            assert_eq!(got.last_started_at, want.last_started_at);
        }
    }

    #[test]
    fn saving_again_updates_in_place_instead_of_duplicating() {
        let _dir = isolate();
        let mut f = fleet("f1");
        save_all(&[f.clone()]);
        f.members[0].review = ReviewState::Discarded;
        f.members[0].attempt = 3;
        save_all(&[f.clone()]);

        let loaded = load_all();
        assert_eq!(loaded.len(), 1, "a second save created a second fleet");
        assert_eq!(loaded[0].members.len(), 2, "a second save duplicated members");
        assert_eq!(loaded[0].members[0].review, ReviewState::Discarded);
        assert_eq!(loaded[0].members[0].attempt, 3);
    }

    #[test]
    fn a_fleet_left_out_of_the_save_is_removed() {
        let _dir = isolate();
        save_all(&[fleet("f1"), fleet("f2")]);
        assert_eq!(load_all().len(), 2);
        // What `fleet.clean` does: hand back the set without it.
        save_all(&[fleet("f2")]);
        let left = load_all();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, "f2");
    }

    #[test]
    fn execution_state_and_review_state_stay_separate() {
        let _dir = isolate();
        let mut f = fleet("f1");
        f.members[0].pane_id = None;
        f.members[0].review = ReviewState::Pending;
        f.members[1].pane_id = Some(9);
        f.members[1].review = ReviewState::Merged;
        save_all(&[f]);

        let store = tasks().unwrap();
        let task = store
            .tasks()
            .unwrap()
            .into_iter()
            .find(|t| t.kind == FLEET_KIND)
            .unwrap();
        let run = store.runs(&task.id).unwrap().remove(0);
        let steps = store.steps(&run.id).unwrap();
        let states: Vec<State> = steps.iter().map(|s| s.state).collect();
        // Not launched and unreviewed is pending work; merged is over. The
        // point is that the engine has an opinion about execution while the
        // review verdict rides along in detail, unread by it.
        assert_eq!(states, vec![State::Pending, State::Succeeded]);
        assert_eq!(
            steps[1].detail.get("review").unwrap().as_str(),
            Some("merged"),
            "the review verdict must survive in detail"
        );
    }

    #[test]
    fn the_legacy_json_is_imported_once_and_then_retired() {
        let dir = isolate();
        let json = dir.path().join("fleets.json");
        std::env::set_var("UNTERM_FLEETS_PATH", &json);
        let legacy = serde_json::json!({ "fleets": [{
            "id": "old-1",
            "task": "from the json era",
            "base_repo": "/tmp/repo",
            "base_branch": "main",
            "created_at": "2026-01-01T00:00:00Z",
            "members": [{
                "agent": "codex",
                "agent_cmd": "codex go",
                "worktree": "/tmp/wt/old",
                "branch": "fleet/old-1",
                "pane_id": 3,
                "checkpoint": "deadbeef",
                "review": "pending"
            }]
        }]});
        std::fs::write(&json, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();

        assert_eq!(migrate_legacy_json(), 1);
        let loaded = load_all();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "old-1");
        assert_eq!(loaded[0].task, "from the json era");
        assert_eq!(loaded[0].members[0].branch, "fleet/old-1");
        // A record written before `attempt` existed reads as its first try,
        // exactly as the old serde default did.
        assert_eq!(loaded[0].members[0].attempt, 1);

        // The file is kept, not eaten: it is the only copy of that history.
        assert!(!json.exists(), "the retired file must not still be live");
        assert!(
            json.with_extension("json.migrated").exists(),
            "the migration destroyed its own input"
        );
        // And a second run does nothing, because there is no file any more.
        assert_eq!(migrate_legacy_json(), 0);
        assert_eq!(load_all().len(), 1);
        std::env::remove_var("UNTERM_FLEETS_PATH");
    }

    #[test]
    fn an_import_never_resurrects_fleets_that_were_cleaned() {
        let dir = isolate();
        // Someone already has fleets in the database.
        save_all(&[fleet("live")]);
        let json = dir.path().join("fleets.json");
        std::env::set_var("UNTERM_FLEETS_PATH", &json);
        std::fs::write(
            &json,
            serde_json::to_string(&serde_json::json!({"fleets": [{
                "id": "long-gone", "task": "t", "base_repo": "/tmp", "base_branch": "main",
                "created_at": "2026-01-01T00:00:00Z", "members": []
            }]}))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(migrate_legacy_json(), 0, "a stale file must not be imported");
        let ids: Vec<String> = load_all().into_iter().map(|f| f.id).collect();
        assert_eq!(ids, vec!["live".to_string()]);
        std::env::remove_var("UNTERM_FLEETS_PATH");
    }
}
