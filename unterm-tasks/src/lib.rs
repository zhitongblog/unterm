//! The durable task engine.
//!
//! Everything the Agent Cockpit knows used to live in process memory and a
//! whole-file JSON rewrite, which meant two things: it disappeared when the
//! process did, and two writers could not both be right. This is the other
//! answer — one SQLite file, one state machine, one monotonically numbered
//! event stream that every reader follows by cursor.
//!
//! Three properties the rest of the system is allowed to rely on:
//!
//! * **A step with an idempotency key runs once.** Asking again returns the
//!   step that already exists, so a retried message cannot repeat a side
//!   effect.
//! * **A step is claimed by exactly one worker.** The claim is a conditional
//!   update, so concurrency is settled by the database rather than by whoever
//!   read first.
//! * **Nothing is lost to a crash.** A worker that dies stops renewing its
//!   lease; reconciliation turns the lapsed claim into `Interrupted`, which
//!   is a verdict a reader can act on rather than a row stuck at `Running`
//!   forever.

pub mod approval;
pub mod lease;
pub mod model;
mod schema;
pub mod workspace;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub use approval::{
    risk_allows_standing_grant, risk_rank, Approval, ApprovalState, Ask, Grant, NewGrant, Scope,
};
pub use lease::{CallRecord, CallSlot, Chain, Lease, NewLease, Presented, Refusal};
pub use model::{Event, Run, RunId, State, Step, StepId, Task, TaskId};
pub use workspace::{AgentSession, Artifact, NewArtifact, Workspace};

/// The data schema this build understands.
///
/// Published in the provider manifest: an orchestrator comparing versions
/// needs the one that says whether this build can read the file it finds,
/// which is not the product version.
pub fn schema_version() -> i64 {
    schema::latest_version()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// What claiming a step gave you.
#[derive(Clone, Debug, PartialEq)]
pub enum Claim {
    /// The step is yours until the lease expires.
    Granted(Step),
    /// Somebody else holds it, or it is no longer pending.
    Denied { held_by: Option<String>, state: State },
}

/// What asking for a step with an idempotency key gave you.
#[derive(Clone, Debug, PartialEq)]
pub enum StepRequest {
    /// There was no such key; this step is new.
    Created(Step),
    /// The key was already used. This is that step, untouched — the caller's
    /// side effect has already happened (or is happening) and must not be
    /// repeated.
    AlreadyExists(Step),
}

impl StepRequest {
    pub fn step(&self) -> &Step {
        match self {
            StepRequest::Created(step) | StepRequest::AlreadyExists(step) => step,
        }
    }

    pub fn is_new(&self) -> bool {
        matches!(self, StepRequest::Created(_))
    }
}

/// What recovery found and put right.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recovery {
    pub steps_interrupted: Vec<StepId>,
    pub runs_interrupted: Vec<RunId>,
    pub tasks_interrupted: Vec<TaskId>,
}

impl Recovery {
    /// Whether the previous life left anything behind.
    pub fn is_clean(&self) -> bool {
        self.steps_interrupted.is_empty()
            && self.runs_interrupted.is_empty()
            && self.tasks_interrupted.is_empty()
    }
}

/// The task store.
///
/// One connection behind a mutex rather than a pool: the writer is the Core,
/// the volumes are human-scale, and a single connection makes "did my write
/// land before your read" a question with an answer.
pub struct TaskStore {
    connection: Mutex<Connection>,
    path: Option<PathBuf>,
}

impl TaskStore {
    /// Open (creating if needed) the store at `path`, migrating it forward.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut connection = Connection::open(&path)
            .with_context(|| format!("open task store at {}", path.display()))?;
        schema::apply_pragmas(&connection)?;
        if let Some(backup) = schema::migrate(&mut connection, Some(&path))? {
            log_line(&format!("task store backed up to {}", backup.display()));
        }
        Ok(Self {
            connection: Mutex::new(connection),
            path: Some(path),
        })
    }

    /// An in-memory store, for tests and for a Core told to keep nothing.
    pub fn in_memory() -> Result<Self> {
        let mut connection = Connection::open_in_memory()?;
        schema::apply_pragmas(&connection)?;
        schema::migrate(&mut connection, None)?;
        Ok(Self {
            connection: Mutex::new(connection),
            path: None,
        })
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn with<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut guard = self
            .connection
            .lock()
            .map_err(|_| anyhow::anyhow!("the task store lock was poisoned by an earlier panic"))?;
        f(&mut guard)
    }

    // ---- tasks -------------------------------------------------------

    pub fn create_task(&self, kind: &str, title: &str) -> Result<Task> {
        self.create_task_with_detail(kind, title, serde_json::json!({}))
    }

    /// A task carrying the caller's own data.
    pub fn create_task_with_detail(
        &self,
        kind: &str,
        title: &str,
        detail: serde_json::Value,
    ) -> Result<Task> {
        self.create_task_as(TaskId::new(), kind, title, detail)
    }

    /// A task under an id the caller already chose.
    ///
    /// For a local-first client that names the task before it reaches the
    /// store (offline, or optimistically), so the two never disagree about
    /// which task is which. Taking an id that exists is an error, not an
    /// overwrite.
    pub fn create_task_as(
        &self,
        id: TaskId,
        kind: &str,
        title: &str,
        detail: serde_json::Value,
    ) -> Result<Task> {
        let task = Task {
            id,
            kind: kind.to_string(),
            title: title.to_string(),
            state: State::Pending,
            version: 1,
            created_at: now(),
            updated_at: now(),
            detail,
        };
        self.with(|connection| {
            let transaction = connection.transaction()?;
            transaction.execute(
                "INSERT INTO tasks (id, kind, title, state, version, created_at, updated_at, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    task.id.as_str(),
                    task.kind,
                    task.title,
                    task.state.as_str(),
                    task.version,
                    task.created_at,
                    task.updated_at,
                    task.detail.to_string()
                ],
            )?;
            append_event(
                &transaction,
                Some(task.id.as_str()),
                None,
                None,
                "task.created",
                &serde_json::json!({"kind": task.kind, "title": task.title}),
            )?;
            transaction.commit()?;
            Ok(())
        })?;
        Ok(task)
    }

    pub fn task(&self, id: &TaskId) -> Result<Option<Task>> {
        self.with(|connection| {
            connection
                .query_row(
                    "SELECT id, kind, title, state, version, created_at, updated_at, detail
                     FROM tasks WHERE id = ?1",
                    params![id.as_str()],
                    row_to_task,
                )
                .optional()
                .map_err(Into::into)
        })
    }

    pub fn tasks(&self) -> Result<Vec<Task>> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, kind, title, state, version, created_at, updated_at, detail
                 FROM tasks ORDER BY created_at, id",
            )?;
            let rows = statement.query_map([], row_to_task)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// Move a task to `next`, refusing an illegal edge and refusing a write
    /// based on a version the caller no longer has.
    pub fn set_task_state(&self, id: &TaskId, next: State, expected_version: i64) -> Result<Task> {
        self.transition("tasks", id.as_str(), next, expected_version, |connection| {
            connection.query_row(
                "SELECT id, kind, title, state, version, created_at, updated_at, detail
                 FROM tasks WHERE id = ?1",
                params![id.as_str()],
                row_to_task,
            )
        })
    }

    // ---- runs --------------------------------------------------------

    /// Start another attempt at a task. Attempts number from 1 and never
    /// reuse a number, so "the third try" always means the same run.
    pub fn start_run(&self, task_id: &TaskId) -> Result<Run> {
        self.start_run_as(task_id, RunId::new())
    }

    /// [`Self::start_run`] under an id the caller already chose (see
    /// [`Self::create_task_as`]).
    pub fn start_run_as(&self, task_id: &TaskId, id: RunId) -> Result<Run> {
        self.with(|connection| {
            let transaction = connection.transaction()?;
            let attempt: i64 = transaction.query_row(
                "SELECT COALESCE(MAX(attempt), 0) + 1 FROM runs WHERE task_id = ?1",
                params![task_id.as_str()],
                |row| row.get(0),
            )?;
            let run = Run {
                id,
                task_id: task_id.clone(),
                attempt,
                state: State::Running,
                version: 1,
                started_at: now(),
                updated_at: now(),
                ended_at: None,
            };
            transaction.execute(
                "INSERT INTO runs (id, task_id, attempt, state, version, started_at, updated_at, ended_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
                params![
                    run.id.as_str(),
                    run.task_id.as_str(),
                    run.attempt,
                    run.state.as_str(),
                    run.version,
                    run.started_at,
                    run.updated_at
                ],
            )?;
            append_event(
                &transaction,
                Some(task_id.as_str()),
                Some(run.id.as_str()),
                None,
                "run.started",
                &serde_json::json!({"attempt": attempt}),
            )?;
            transaction.commit()?;
            Ok(run)
        })
    }

    pub fn runs(&self, task_id: &TaskId) -> Result<Vec<Run>> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, task_id, attempt, state, version, started_at, updated_at, ended_at
                 FROM runs WHERE task_id = ?1 ORDER BY attempt",
            )?;
            let rows = statement.query_map(params![task_id.as_str()], row_to_run)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    pub fn finish_run(&self, id: &RunId, next: State, expected_version: i64) -> Result<Run> {
        let run = self.transition("runs", id.as_str(), next, expected_version, |connection| {
            connection.query_row(
                "SELECT id, task_id, attempt, state, version, started_at, updated_at, ended_at
                 FROM runs WHERE id = ?1",
                params![id.as_str()],
                row_to_run,
            )
        })?;
        if next.is_terminal() {
            self.with(|connection| {
                connection.execute(
                    "UPDATE runs SET ended_at = ?2 WHERE id = ?1 AND ended_at IS NULL",
                    params![id.as_str(), now()],
                )?;
                Ok(())
            })?;
        }
        Ok(run)
    }

    // ---- steps -------------------------------------------------------

    /// Ask for a step. With an idempotency key, asking twice gets the same
    /// step back rather than a second one.
    pub fn request_step(
        &self,
        run_id: &RunId,
        kind: &str,
        idempotency_key: Option<&str>,
    ) -> Result<StepRequest> {
        self.request_step_with_detail(run_id, kind, idempotency_key, serde_json::json!({}))
    }

    /// A step carrying the caller's own data.
    pub fn request_step_with_detail(
        &self,
        run_id: &RunId,
        kind: &str,
        idempotency_key: Option<&str>,
        detail: serde_json::Value,
    ) -> Result<StepRequest> {
        self.with(|connection| {
            let transaction = connection.transaction()?;
            if let Some(key) = idempotency_key {
                let existing = transaction
                    .query_row(
                        "SELECT id, run_id, ordinal, kind, state, version, idempotency_key,
                                claimed_by, lease_expires_at, created_at, updated_at, detail
                         FROM steps WHERE idempotency_key = ?1",
                        params![key],
                        row_to_step,
                    )
                    .optional()?;
                if let Some(step) = existing {
                    // Deliberately not touching it: the caller is repeating
                    // itself, and the record of what actually happened
                    // belongs to the first request.
                    transaction.commit()?;
                    return Ok(StepRequest::AlreadyExists(step));
                }
            }
            let ordinal: i64 = transaction.query_row(
                "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM steps WHERE run_id = ?1",
                params![run_id.as_str()],
                |row| row.get(0),
            )?;
            let step = Step {
                id: StepId::new(),
                run_id: run_id.clone(),
                ordinal,
                kind: kind.to_string(),
                state: State::Pending,
                version: 1,
                idempotency_key: idempotency_key.map(str::to_string),
                claimed_by: None,
                lease_expires_at: None,
                created_at: now(),
                updated_at: now(),
                detail,
            };
            transaction.execute(
                "INSERT INTO steps (id, run_id, ordinal, kind, state, version, idempotency_key,
                                    claimed_by, lease_expires_at, created_at, updated_at, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, ?8, ?9, ?10)",
                params![
                    step.id.as_str(),
                    step.run_id.as_str(),
                    step.ordinal,
                    step.kind,
                    step.state.as_str(),
                    step.version,
                    step.idempotency_key,
                    step.created_at,
                    step.updated_at,
                    step.detail.to_string()
                ],
            )?;
            append_event(
                &transaction,
                None,
                Some(run_id.as_str()),
                Some(step.id.as_str()),
                "step.requested",
                &serde_json::json!({"kind": kind, "ordinal": ordinal}),
            )?;
            transaction.commit()?;
            Ok(StepRequest::Created(step))
        })
    }

    /// Take a pending step, for `lease` seconds.
    ///
    /// The whole decision is one conditional UPDATE: whoever the database
    /// applies first wins, and the loser is told who holds it. Reading then
    /// writing would let two workers both see `pending` and both proceed.
    pub fn claim_step(&self, id: &StepId, worker: &str, lease_seconds: i64) -> Result<Claim> {
        self.with(|connection| {
            let transaction = connection.transaction()?;
            let expires = (chrono::Utc::now() + chrono::Duration::seconds(lease_seconds))
                .to_rfc3339();
            let changed = transaction.execute(
                "UPDATE steps
                    SET state = 'running', claimed_by = ?2, lease_expires_at = ?3,
                        version = version + 1, updated_at = ?4
                  WHERE id = ?1 AND state = 'pending'",
                params![id.as_str(), worker, expires, now()],
            )?;
            let step = transaction.query_row(
                "SELECT id, run_id, ordinal, kind, state, version, idempotency_key,
                        claimed_by, lease_expires_at, created_at, updated_at, detail
                 FROM steps WHERE id = ?1",
                params![id.as_str()],
                row_to_step,
            )?;
            if changed == 0 {
                transaction.commit()?;
                return Ok(Claim::Denied {
                    held_by: step.claimed_by.clone(),
                    state: step.state,
                });
            }
            append_event(
                &transaction,
                None,
                Some(step.run_id.as_str()),
                Some(step.id.as_str()),
                "step.claimed",
                &serde_json::json!({"worker": worker, "lease_expires_at": expires}),
            )?;
            transaction.commit()?;
            Ok(Claim::Granted(step))
        })
    }

    /// Push the lease out. A worker that is alive says so by doing this; one
    /// that stops is what reconciliation notices.
    pub fn heartbeat_step(&self, id: &StepId, worker: &str, lease_seconds: i64) -> Result<bool> {
        self.with(|connection| {
            let expires = (chrono::Utc::now() + chrono::Duration::seconds(lease_seconds))
                .to_rfc3339();
            let changed = connection.execute(
                "UPDATE steps SET lease_expires_at = ?3, updated_at = ?4
                  WHERE id = ?1 AND claimed_by = ?2 AND state = 'running'",
                params![id.as_str(), worker, expires, now()],
            )?;
            // Deliberately not bumping `version`: a heartbeat is not a change
            // to what the step *is*, and bumping it would make every reader's
            // compare-and-swap lose a race against a liveness ping.
            Ok(changed == 1)
        })
    }

    pub fn step(&self, id: &StepId) -> Result<Option<Step>> {
        self.with(|connection| {
            connection
                .query_row(
                    "SELECT id, run_id, ordinal, kind, state, version, idempotency_key,
                            claimed_by, lease_expires_at, created_at, updated_at, detail
                     FROM steps WHERE id = ?1",
                    params![id.as_str()],
                    row_to_step,
                )
                .optional()
                .map_err(Into::into)
        })
    }

    pub fn steps(&self, run_id: &RunId) -> Result<Vec<Step>> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, run_id, ordinal, kind, state, version, idempotency_key,
                        claimed_by, lease_expires_at, created_at, updated_at, detail
                 FROM steps WHERE run_id = ?1 ORDER BY ordinal",
            )?;
            let rows = statement.query_map(params![run_id.as_str()], row_to_step)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    pub fn finish_step(&self, id: &StepId, next: State, expected_version: i64) -> Result<Step> {
        let step = self.transition("steps", id.as_str(), next, expected_version, |connection| {
            connection.query_row(
                "SELECT id, run_id, ordinal, kind, state, version, idempotency_key,
                        claimed_by, lease_expires_at, created_at, updated_at, detail
                 FROM steps WHERE id = ?1",
                params![id.as_str()],
                row_to_step,
            )
        })?;
        if next.is_terminal() {
            // The claim is over; leaving it set would make a finished step
            // look held, and reconciliation would keep considering it.
            self.with(|connection| {
                connection.execute(
                    "UPDATE steps SET claimed_by = NULL, lease_expires_at = NULL WHERE id = ?1",
                    params![id.as_str()],
                )?;
                Ok(())
            })?;
        }
        Ok(step)
    }

    // ---- recovery ----------------------------------------------------

    /// Turn claims whose lease has lapsed into `Interrupted`.
    ///
    /// This is what makes a hard kill survivable. The worker that held the
    /// step is gone, so nobody will ever report its verdict; leaving the row
    /// at `Running` means a Cockpit that shows work in progress forever.
    /// `Interrupted` says what is actually known — it started, and nobody can
    /// say how it ended — and a retry starts a new run rather than pretending
    /// this one continued.
    ///
    /// Returns the steps it reclaimed.
    pub fn reconcile(&self) -> Result<Vec<Step>> {
        self.with(|connection| {
            let cutoff = now();
            let transaction = connection.transaction()?;
            let lapsed: Vec<String> = {
                let mut statement = transaction.prepare(
                    "SELECT id FROM steps
                      WHERE state = 'running'
                        AND lease_expires_at IS NOT NULL
                        AND lease_expires_at < ?1",
                )?;
                let rows = statement.query_map(params![cutoff], |row| row.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            let mut reclaimed = Vec::new();
            for id in lapsed {
                transaction.execute(
                    "UPDATE steps
                        SET state = 'interrupted', version = version + 1, updated_at = ?2,
                            claimed_by = NULL, lease_expires_at = NULL
                      WHERE id = ?1",
                    params![id, now()],
                )?;
                let step = transaction.query_row(
                    "SELECT id, run_id, ordinal, kind, state, version, idempotency_key,
                            claimed_by, lease_expires_at, created_at, updated_at, detail
                     FROM steps WHERE id = ?1",
                    params![id],
                    row_to_step,
                )?;
                append_event(
                    &transaction,
                    None,
                    Some(step.run_id.as_str()),
                    Some(step.id.as_str()),
                    "step.interrupted",
                    &serde_json::json!({"reason": "lease expired"}),
                )?;
                reclaimed.push(step);
            }
            transaction.commit()?;
            Ok(reclaimed)
        })
    }


    /// Stop a task and everything still live underneath it.
    ///
    /// Cancelling only the task would leave its steps `Running` with workers
    /// still holding them — the row says stopped, the machine says otherwise.
    /// The whole cascade is one transaction, so a reader never sees a
    /// cancelled task with live children. Children that already reached a
    /// verdict keep it: a step that succeeded before the cancel arrived did
    /// succeed, and rewriting that would be a lie about what happened.
    pub fn cancel_task(&self, id: &TaskId, expected_version: i64) -> Result<Task> {
        self.with(|connection| {
            let transaction = connection.transaction()?;
            let (state, version): (String, i64) = transaction
                .query_row(
                    "SELECT state, version FROM tasks WHERE id = ?1",
                    params![id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or_else(|| anyhow::anyhow!("no task with id {id}"))?;
            let state = State::parse(&state)?;
            if version != expected_version {
                anyhow::bail!(
                    "task {id} is at version {version}, not {expected_version}; \
                     something changed it since you read it"
                );
            }
            if !State::Cancelled.may_follow(state) {
                anyhow::bail!("task {id} cannot go from {} to cancelled", state.as_str());
            }

            let stamp = now();
            // Steps first, then runs, then the task: a reader walking down
            // from a cancelled task must never find a live child, and the
            // order inside one transaction is what guarantees it even for a
            // reader on another connection.
            transaction.execute(
                "UPDATE steps
                    SET state = 'cancelled', version = version + 1, updated_at = ?2,
                        claimed_by = NULL, lease_expires_at = NULL
                  WHERE state IN ('pending', 'running')
                    AND run_id IN (SELECT id FROM runs WHERE task_id = ?1)",
                params![id.as_str(), stamp],
            )?;
            transaction.execute(
                "UPDATE runs
                    SET state = 'cancelled', version = version + 1, updated_at = ?2,
                        ended_at = COALESCE(ended_at, ?2)
                  WHERE task_id = ?1 AND state IN ('pending', 'running')",
                params![id.as_str(), stamp],
            )?;
            transaction.execute(
                "UPDATE tasks SET state = 'cancelled', version = version + 1, updated_at = ?2
                  WHERE id = ?1 AND version = ?3",
                params![id.as_str(), stamp, expected_version],
            )?;
            append_event(
                &transaction,
                Some(id.as_str()),
                None,
                None,
                "task.cancelled",
                &serde_json::json!({"from": state.as_str(), "cascaded": true}),
            )?;
            let task = transaction.query_row(
                "SELECT id, kind, title, state, version, created_at, updated_at, detail
                 FROM tasks WHERE id = ?1",
                params![id.as_str()],
                row_to_task,
            )?;
            transaction.commit()?;
            Ok(task)
        })
    }

    /// Put the store back into a state somebody can act on, after a crash.
    ///
    /// Called when the Core starts, where by definition no worker from the
    /// previous life survived. Three sweeps, in the order the containment
    /// runs: a claim nobody is renewing is `Interrupted`; a run whose work is
    /// all finished but which nobody closed is `Interrupted`; a task whose
    /// attempts are all over but which nobody closed is the same. A run that
    /// still has pending steps is left alone — a new worker can pick those
    /// up, and reclaiming resumable work is worse than the stall it fixes.
    pub fn recover(&self) -> Result<Recovery> {
        let steps = self.reconcile()?;
        let mut recovery = Recovery {
            steps_interrupted: steps.into_iter().map(|step| step.id).collect(),
            ..Recovery::default()
        };
        self.with(|connection| {
            let transaction = connection.transaction()?;
            let stamp = now();

            let stalled_runs: Vec<String> = {
                let mut statement = transaction.prepare(
                    "SELECT r.id FROM runs r
                      WHERE r.state = 'running'
                        AND EXISTS (SELECT 1 FROM steps s WHERE s.run_id = r.id)
                        AND NOT EXISTS (
                            SELECT 1 FROM steps s
                             WHERE s.run_id = r.id AND s.state IN ('pending', 'running'))",
                )?;
                let rows = statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                drop(statement);
                rows
            };
            for id in &stalled_runs {
                transaction.execute(
                    "UPDATE runs SET state = 'interrupted', version = version + 1,
                                     updated_at = ?2, ended_at = COALESCE(ended_at, ?2)
                      WHERE id = ?1",
                    params![id, stamp],
                )?;
                append_event(
                    &transaction,
                    None,
                    Some(id.as_str()),
                    None,
                    "run.interrupted",
                    &serde_json::json!({"reason": "no live work and nobody closed it"}),
                )?;
                recovery.runs_interrupted.push(RunId::parse(id)?);
            }

            let stalled_tasks: Vec<String> = {
                let mut statement = transaction.prepare(
                    "SELECT t.id FROM tasks t
                      WHERE t.state = 'running'
                        AND EXISTS (SELECT 1 FROM runs r WHERE r.task_id = t.id)
                        AND NOT EXISTS (
                            SELECT 1 FROM runs r
                             WHERE r.task_id = t.id AND r.state IN ('pending', 'running'))",
                )?;
                let rows = statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                drop(statement);
                rows
            };
            for id in &stalled_tasks {
                transaction.execute(
                    "UPDATE tasks SET state = 'interrupted', version = version + 1, updated_at = ?2
                      WHERE id = ?1",
                    params![id, stamp],
                )?;
                append_event(
                    &transaction,
                    Some(id.as_str()),
                    None,
                    None,
                    "task.interrupted",
                    &serde_json::json!({"reason": "every attempt is over and nobody closed it"}),
                )?;
                recovery.tasks_interrupted.push(TaskId::parse(id)?);
            }
            transaction.commit()?;
            Ok(())
        })?;
        Ok(recovery)
    }


    /// Replace a task's opaque detail, leaving its state and version alone.
    ///
    /// Detail is the caller's data, not the engine's; changing it is not a
    /// state transition and must not make every reader's compare-and-swap
    /// lose, for the same reason a heartbeat does not bump the version.
    pub fn set_task_detail(&self, id: &TaskId, detail: serde_json::Value) -> Result<()> {
        self.with(|connection| {
            connection.execute(
                "UPDATE tasks SET detail = ?2, updated_at = ?3 WHERE id = ?1",
                params![id.as_str(), detail.to_string(), now()],
            )?;
            Ok(())
        })
    }

    /// Rename a task. Like detail, not a state transition.
    pub fn set_task_title(&self, id: &TaskId, title: &str) -> Result<()> {
        self.with(|connection| {
            connection.execute(
                "UPDATE tasks SET title = ?2, updated_at = ?3 WHERE id = ?1",
                params![id.as_str(), title, now()],
            )?;
            Ok(())
        })
    }

    /// Replace a step's detail, and put it in `state`.
    ///
    /// Unlike [`Self::finish_step`] this does not walk the state machine: the
    /// caller is projecting an outside truth it already owns — which member
    /// is running, which has been reviewed — rather than driving work through
    /// its life. Transitions that a worker performs still go through
    /// `claim_step` and `finish_step`, where the edges are enforced.
    pub fn set_step_detail(
        &self,
        id: &StepId,
        detail: serde_json::Value,
        state: State,
    ) -> Result<()> {
        self.with(|connection| {
            connection.execute(
                "UPDATE steps SET detail = ?2, state = ?3, updated_at = ?4,
                                  version = version + 1
                  WHERE id = ?1",
                params![id.as_str(), detail.to_string(), state.as_str(), now()],
            )?;
            Ok(())
        })
    }

    /// Remove a task and everything under it.
    ///
    /// The only destructive operation here. Runs and steps go with it through
    /// the schema's cascade, and the events stay: what happened still
    /// happened, and an audit trail that disappears with its subject is not
    /// one.
    pub fn delete_task(&self, id: &TaskId) -> Result<()> {
        self.with(|connection| {
            let transaction = connection.transaction()?;
            transaction.execute("DELETE FROM tasks WHERE id = ?1", params![id.as_str()])?;
            append_event(
                &transaction,
                Some(id.as_str()),
                None,
                None,
                "task.deleted",
                &serde_json::json!({}),
            )?;
            transaction.commit()?;
            Ok(())
        })
    }


    // ---- grants and approvals ---------------------------------------

    /// Record what the user agreed to.
    pub fn create_grant(&self, spec: NewGrant) -> Result<Grant> {
        let scope = spec.scope_or_once.unwrap_or(Scope::Once);
        let grant = Grant {
            id: format!("grt_{}", uuid::Uuid::new_v4().simple()),
            scope,
            method: spec.method,
            actor: spec.actor,
            task_id: spec.task_id,
            resource: spec.resource,
            max_risk: spec.max_risk.unwrap_or_else(|| "local_mutation".to_string()),
            created_at: now(),
            expires_at: spec.ttl_seconds.map(|seconds| {
                (chrono::Utc::now() + chrono::Duration::seconds(seconds)).to_rfc3339()
            }),
            revoked_at: None,
            consumed_at: None,
        };
        self.with(|connection| {
            connection.execute(
                "INSERT INTO grants (id, scope, method, actor, task_id, resource, max_risk,
                                     created_at, expires_at, revoked_at, consumed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, NULL)",
                params![
                    grant.id,
                    grant.scope.as_str(),
                    grant.method,
                    grant.actor,
                    grant.task_id,
                    grant.resource,
                    grant.max_risk,
                    grant.created_at,
                    grant.expires_at
                ],
            )?;
            Ok(())
        })?;
        Ok(grant)
    }

    pub fn grants(&self) -> Result<Vec<Grant>> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, scope, method, actor, task_id, resource, max_risk,
                        created_at, expires_at, revoked_at, consumed_at
                 FROM grants ORDER BY created_at, id",
            )?;
            let rows = statement.query_map([], row_to_grant)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// The live grant that covers this action, if one does.
    pub fn covering_grant(&self, ask: &Ask) -> Result<Option<Grant>> {
        let stamp = now();
        Ok(self
            .grants()?
            .into_iter()
            .find(|grant| approval::matches(grant, ask, &stamp)))
    }

    /// Spend a one-shot grant.
    pub fn consume_grant(&self, id: &str) -> Result<()> {
        self.with(|connection| {
            connection.execute(
                "UPDATE grants SET consumed_at = ?2 WHERE id = ?1 AND consumed_at IS NULL",
                params![id, now()],
            )?;
            Ok(())
        })
    }

    /// Take a permission back.
    ///
    /// The gate this subsystem is judged by: revoking must stop the next
    /// action *and* whatever is already proceeding on this grant's authority.
    /// Stopping only the next one would leave the work the user just withdrew
    /// permission for still running, which is the opposite of what they asked.
    ///
    /// Everything happens in one transaction, so there is no window where the
    /// permission is gone but something still believes it holds it. Returns
    /// how many things were cut off.
    pub fn revoke_grant(&self, id: &str) -> Result<usize> {
        self.with(|connection| {
            let transaction = connection.transaction()?;
            let stamp = now();
            transaction.execute(
                "UPDATE grants SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
                params![id, stamp],
            )?;
            let mut cut_off = transaction.execute(
                "UPDATE approvals
                    SET state = 'revoked', decided_at = ?2, decided_by = 'revocation'
                  WHERE grant_id = ?1 AND state = 'pending'",
                params![id, stamp],
            )?;
            // Work already under way on this grant's authority. The gateway
            // stamps `authorised_by` onto a step when a grant is what let it
            // through, which is the only way revocation can find it again.
            cut_off += transaction.execute(
                "UPDATE steps
                    SET state = 'cancelled', version = version + 1, updated_at = ?2,
                        claimed_by = NULL, lease_expires_at = NULL
                  WHERE state IN ('pending', 'running')
                    AND json_extract(detail, '$.authorised_by') = ?1",
                params![id, stamp],
            )?;
            // And the capability leases issued on this grant's authority. A
            // lease outliving the permission that created it is a key still
            // turning in a lock the user changed.
            cut_off += transaction.execute(
                "UPDATE capability_leases SET revoked_at = ?2
                  WHERE grant_id = ?1 AND revoked_at IS NULL",
                params![id, stamp],
            )?;
            transaction.commit()?;
            Ok(cut_off)
        })
    }

    /// Record which grant let a step through.
    ///
    /// Without this the authority is only in the log, and revocation has no
    /// way to find the work it needs to stop.
    pub fn attribute_step_to_grant(&self, id: &StepId, grant_id: &str) -> Result<()> {
        self.with(|connection| {
            connection.execute(
                "UPDATE steps
                    SET detail = json_set(COALESCE(NULLIF(detail, ''), '{}'),
                                          '$.authorised_by', ?2),
                        updated_at = ?3
                  WHERE id = ?1",
                params![id.as_str(), grant_id, now()],
            )?;
            Ok(())
        })
    }


    // ---- capability leases -------------------------------------------

    /// Issue a lease.
    pub fn issue_lease(&self, spec: NewLease) -> Result<Lease> {
        let lease = Lease {
            id: format!("lse_{}", uuid::Uuid::new_v4().simple()),
            provider: spec.provider,
            capability: spec.capability,
            actor: spec.actor,
            task_id: spec.task_id,
            step_id: spec.step_id,
            grant_id: spec.grant_id,
            approval_id: spec.approval_id,
            issued_at: now(),
            expires_at: (chrono::Utc::now()
                + chrono::Duration::seconds(spec.ttl_seconds.max(1)))
            .to_rfc3339(),
            renewed_at: None,
            revoked_at: None,
            epoch: 1,
            last_seq: 0,
        };
        self.with(|connection| {
            connection.execute(
                "INSERT INTO capability_leases
                    (id, provider, capability, actor, task_id, step_id, grant_id, approval_id,
                     issued_at, expires_at, renewed_at, revoked_at, epoch, last_seq)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, NULL, 1, 0)",
                params![
                    lease.id,
                    lease.provider,
                    lease.capability,
                    lease.actor,
                    lease.task_id,
                    lease.step_id,
                    lease.grant_id,
                    lease.approval_id,
                    lease.issued_at,
                    lease.expires_at
                ],
            )?;
            Ok(())
        })?;
        Ok(lease)
    }

    pub fn lease(&self, id: &str) -> Result<Option<Lease>> {
        self.with(|connection| {
            connection
                .query_row(
                    "SELECT id, provider, capability, actor, task_id, step_id, grant_id,
                            approval_id, issued_at, expires_at, renewed_at, revoked_at,
                            epoch, last_seq
                     FROM capability_leases WHERE id = ?1",
                    params![id],
                    row_to_lease,
                )
                .optional()
                .map_err(Into::into)
        })
    }

    /// Every lease, newest first.
    pub fn leases(&self) -> Result<Vec<Lease>> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, provider, capability, actor, task_id, step_id, grant_id,
                        approval_id, issued_at, expires_at, renewed_at, revoked_at,
                        epoch, last_seq
                 FROM capability_leases ORDER BY issued_at DESC, id",
            )?;
            let rows = statement.query_map([], row_to_lease)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// Extend a lease and bump its epoch.
    ///
    /// The bump is what makes renewal meaningful: whoever renewed holds the
    /// current lease, and a copy of the old one — recorded by something
    /// listening, or kept by a process that was told to stop — no longer is.
    pub fn renew_lease(&self, id: &str, ttl_seconds: i64) -> Result<Option<Lease>> {
        self.with(|connection| {
            let stamp = now();
            let expires =
                (chrono::Utc::now() + chrono::Duration::seconds(ttl_seconds.max(1))).to_rfc3339();
            connection.execute(
                "UPDATE capability_leases
                    SET expires_at = ?2, renewed_at = ?3, epoch = epoch + 1
                  WHERE id = ?1 AND revoked_at IS NULL",
                params![id, expires, stamp],
            )?;
            Ok(())
        })?;
        self.lease(id)
    }

    /// Take a lease back. Returns whether there was a live one to take.
    pub fn revoke_lease(&self, id: &str) -> Result<bool> {
        self.with(|connection| {
            let changed = connection.execute(
                "UPDATE capability_leases SET revoked_at = ?2
                  WHERE id = ?1 AND revoked_at IS NULL",
                params![id, now()],
            )?;
            Ok(changed == 1)
        })
    }

    /// Revoke every live lease for a provider. Returns how many.
    ///
    /// What "unbind this provider" means: the binding is not a setting to be
    /// forgotten, it is a set of keys that have to be taken back.
    pub fn revoke_provider_leases(&self, provider: &str) -> Result<usize> {
        self.with(|connection| {
            Ok(connection.execute(
                "UPDATE capability_leases SET revoked_at = ?2
                  WHERE provider = ?1 AND revoked_at IS NULL",
                params![provider, now()],
            )?)
        })
    }

    /// Present a lease for one use.
    ///
    /// Refuses *before* anything is performed. Checking afterwards would mean
    /// the replay had already happened and all that was left was to notice.
    pub fn use_lease(&self, presented: &Presented) -> Result<std::result::Result<Lease, Refusal>> {
        let refusal = self.with(|connection| {
            let transaction = connection.transaction()?;
            let existing: Option<(String, Option<String>, i64, i64)> = transaction
                .query_row(
                    "SELECT expires_at, revoked_at, epoch, last_seq
                     FROM capability_leases WHERE id = ?1",
                    params![presented.lease_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            let Some((expires_at, revoked_at, epoch, last_seq)) = existing else {
                return Ok(Some(Refusal::Unknown));
            };
            if revoked_at.is_some() {
                return Ok(Some(Refusal::Revoked));
            }
            if expires_at <= now() {
                return Ok(Some(Refusal::Expired));
            }
            if presented.epoch != epoch {
                return Ok(Some(Refusal::StaleEpoch));
            }
            if presented.seq <= last_seq {
                return Ok(Some(Refusal::Replay));
            }
            // Recording the sequence number is part of the same transaction
            // that checked it. Two uses arriving at once must not both find
            // the old value and both be allowed.
            transaction.execute(
                "UPDATE capability_leases SET last_seq = ?2 WHERE id = ?1",
                params![presented.lease_id, presented.seq],
            )?;
            transaction.commit()?;
            Ok(None)
        })?;
        if let Some(refusal) = refusal {
            return Ok(Err(refusal));
        }
        Ok(self
            .lease(&presented.lease_id)?
            .ok_or(Refusal::Unknown)
            .map_err(|refusal| refusal))
    }

    /// Sweep leases whose time has run out. Returns the ids.
    ///
    /// Expiry is already enforced on use; this is so a reader can see that a
    /// lease is over without having to compare timestamps itself.
    pub fn expire_leases(&self) -> Result<Vec<String>> {
        self.with(|connection| {
            let stamp = now();
            let lapsed: Vec<String> = {
                let mut statement = connection.prepare(
                    "SELECT id FROM capability_leases
                      WHERE revoked_at IS NULL AND expires_at <= ?1",
                )?;
                let rows = statement.query_map(params![stamp], |row| row.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            Ok(lapsed)
        })
    }

    /// Everything that authorised one action.
    ///
    /// The gate this exists for: given a lease an action was performed under,
    /// produce the grant it rested on, the question a human answered, and the
    /// task it was done for — records, not log lines, each of which can still
    /// be revoked.
    pub fn authorisation_chain(&self, lease_id: &str) -> Result<Option<Chain>> {
        let Some(lease) = self.lease(lease_id)? else {
            return Ok(None);
        };
        let grant = match &lease.grant_id {
            Some(id) => self.grants()?.into_iter().find(|grant| &grant.id == id),
            None => None,
        };
        let approval = match &lease.approval_id {
            Some(id) => self.approval(id)?,
            None => None,
        };
        let task = match &lease.task_id {
            Some(id) => match TaskId::parse(id) {
                Ok(id) => self.task(&id)?,
                Err(_) => None,
            },
            None => None,
        };
        Ok(Some(Chain {
            lease,
            grant,
            approval,
            task,
        }))
    }

    // ---- workspaces ---------------------------------------------------

    /// Record a root that work may happen in.
    ///
    /// The root must already be canonical; resolving here would hide from the
    /// caller that what they asked for and what they got are different paths.
    pub fn create_workspace(&self, name: &str, canonical_root: &str) -> Result<Workspace> {
        let workspace = Workspace {
            id: format!("wsp_{}", uuid::Uuid::new_v4().simple()),
            name: name.to_string(),
            root: canonical_root.to_string(),
            created_at: now(),
            archived_at: None,
        };
        self.with(|connection| {
            connection.execute(
                "INSERT INTO workspaces (id, name, root, created_at, archived_at)
                 VALUES (?1, ?2, ?3, ?4, NULL)",
                params![
                    workspace.id,
                    workspace.name,
                    workspace.root,
                    workspace.created_at
                ],
            )?;
            Ok(())
        })?;
        Ok(workspace)
    }

    /// Every workspace, archived ones included.
    ///
    /// Archived roots still matter: they are still somebody's files, and a
    /// live workspace must not be able to reach into one just because nobody
    /// is working there any more.
    pub fn workspaces(&self) -> Result<Vec<Workspace>> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, name, root, created_at, archived_at
                 FROM workspaces ORDER BY created_at, id",
            )?;
            let rows = statement.query_map([], row_to_workspace)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    pub fn workspace(&self, id: &str) -> Result<Option<Workspace>> {
        self.with(|connection| {
            connection
                .query_row(
                    "SELECT id, name, root, created_at, archived_at
                     FROM workspaces WHERE id = ?1",
                    params![id],
                    row_to_workspace,
                )
                .optional()
                .map_err(Into::into)
        })
    }

    /// Stop using a workspace without forgetting where it was.
    pub fn archive_workspace(&self, id: &str) -> Result<bool> {
        self.with(|connection| {
            let changed = connection.execute(
                "UPDATE workspaces SET archived_at = ?2 WHERE id = ?1 AND archived_at IS NULL",
                params![id, now()],
            )?;
            Ok(changed == 1)
        })
    }

    // ---- artifacts ----------------------------------------------------

    /// Index one piece of content. The bytes are the caller's business.
    pub fn record_artifact(&self, spec: NewArtifact) -> Result<Artifact> {
        let artifact = Artifact {
            id: format!("art_{}", uuid::Uuid::new_v4().simple()),
            sha256: spec.sha256,
            bytes: spec.bytes,
            media_type: spec.media_type,
            task_id: spec.task_id,
            step_id: spec.step_id,
            call_id: spec.call_id,
            origin: spec.origin,
            name: spec.name,
            created_at: now(),
        };
        self.with(|connection| {
            connection.execute(
                "INSERT INTO artifacts
                    (id, sha256, bytes, media_type, task_id, step_id, call_id, origin, name,
                     created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    artifact.id,
                    artifact.sha256,
                    artifact.bytes,
                    artifact.media_type,
                    artifact.task_id,
                    artifact.step_id,
                    artifact.call_id,
                    artifact.origin,
                    artifact.name,
                    artifact.created_at
                ],
            )?;
            Ok(())
        })?;
        Ok(artifact)
    }

    pub fn artifact(&self, id: &str) -> Result<Option<Artifact>> {
        self.with(|connection| {
            connection
                .query_row(
                    "SELECT id, sha256, bytes, media_type, task_id, step_id, call_id, origin,
                            name, created_at
                     FROM artifacts WHERE id = ?1",
                    params![id],
                    row_to_artifact,
                )
                .optional()
                .map_err(Into::into)
        })
    }

    /// Everything a task produced, oldest first.
    pub fn artifacts_for_task(&self, task_id: &str) -> Result<Vec<Artifact>> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, sha256, bytes, media_type, task_id, step_id, call_id, origin,
                        name, created_at
                 FROM artifacts WHERE task_id = ?1 ORDER BY created_at, id",
            )?;
            let rows = statement.query_map(params![task_id], row_to_artifact)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    pub fn artifacts(&self) -> Result<Vec<Artifact>> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, sha256, bytes, media_type, task_id, step_id, call_id, origin,
                        name, created_at
                 FROM artifacts ORDER BY created_at DESC, id",
            )?;
            let rows = statement.query_map([], row_to_artifact)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// How many rows still point at this content.
    ///
    /// Deleting the file underneath a hash that another task also produced
    /// would silently break that task's evidence, so nothing is removed from
    /// disk while this is above zero.
    pub fn artifact_references(&self, sha256: &str) -> Result<i64> {
        self.with(|connection| {
            Ok(connection.query_row(
                "SELECT COUNT(*) FROM artifacts WHERE sha256 = ?1",
                params![sha256],
                |row| row.get(0),
            )?)
        })
    }

    /// Forget an artifact. Returns its hash, so the caller can decide whether
    /// any bytes are now unreferenced.
    pub fn forget_artifact(&self, id: &str) -> Result<Option<String>> {
        let Some(artifact) = self.artifact(id)? else {
            return Ok(None);
        };
        self.with(|connection| {
            connection.execute("DELETE FROM artifacts WHERE id = ?1", params![id])?;
            Ok(())
        })?;
        Ok(Some(artifact.sha256))
    }

    // ---- agent sessions -----------------------------------------------

    /// Write down that a session started, or how it ended.
    ///
    /// One upsert rather than insert-then-update: a session that ended before
    /// anyone recorded its start still has to be answerable.
    pub fn record_agent_session(&self, session: &AgentSession) -> Result<()> {
        self.with(|connection| {
            connection.execute(
                "INSERT INTO agent_sessions
                    (id, adapter, command, cwd, task_id, run_id, step_id, idempotency_key,
                     lease_id, state, exit_code, signal, reason, started_at, ended_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                 ON CONFLICT(id) DO UPDATE SET
                    state = excluded.state,
                    exit_code = excluded.exit_code,
                    signal = excluded.signal,
                    reason = excluded.reason,
                    ended_at = excluded.ended_at",
                params![
                    session.id,
                    session.adapter,
                    session.command,
                    session.cwd,
                    session.task_id,
                    session.run_id,
                    session.step_id,
                    session.idempotency_key,
                    session.lease_id,
                    session.state,
                    session.exit_code,
                    session.signal,
                    session.reason,
                    session.started_at,
                    session.ended_at
                ],
            )?;
            Ok(())
        })
    }

    pub fn agent_session(&self, id: &str) -> Result<Option<AgentSession>> {
        self.with(|connection| {
            connection
                .query_row(
                    "SELECT id, adapter, command, cwd, task_id, run_id, step_id,
                            idempotency_key, lease_id, state, exit_code, signal, reason,
                            started_at, ended_at
                     FROM agent_sessions WHERE id = ?1",
                    params![id],
                    row_to_agent_session,
                )
                .optional()
                .map_err(Into::into)
        })
    }

    pub fn agent_sessions(&self) -> Result<Vec<AgentSession>> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, adapter, command, cwd, task_id, run_id, step_id,
                        idempotency_key, lease_id, state, exit_code, signal, reason,
                        started_at, ended_at
                 FROM agent_sessions ORDER BY started_at DESC, id",
            )?;
            let rows = statement.query_map([], row_to_agent_session)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// Sessions this process left running when it died.
    ///
    /// Called at startup. A session recorded as `started` whose Core is gone
    /// did not succeed and did not fail — it was interrupted, and saying so
    /// is the difference between a caller that retries and one that waits
    /// forever.
    pub fn interrupt_orphan_sessions(&self, reason: &str) -> Result<Vec<String>> {
        self.with(|connection| {
            let ids: Vec<String> = {
                let mut statement =
                    connection.prepare("SELECT id FROM agent_sessions WHERE state = 'started'")?;
                let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            for id in &ids {
                connection.execute(
                    "UPDATE agent_sessions
                        SET state = 'interrupted', reason = ?2, ended_at = ?3
                      WHERE id = ?1",
                    params![id, reason, now()],
                )?;
            }
            Ok(ids)
        })
    }

    // ---- provider calls ----------------------------------------------

    /// Start a call, or discover that it has already been made.
    ///
    /// The key is what makes "once" true across a crash: an in-process memo
    /// forgets exactly when the process died mid-call, which is the moment
    /// the caller will retry.
    pub fn begin_call(
        &self,
        idempotency_key: Option<&str>,
        provider: &str,
        capability: &str,
        method: &str,
        lease_id: Option<&str>,
        request: &serde_json::Value,
    ) -> Result<CallSlot> {
        let record = CallRecord {
            id: format!("pcl_{}", uuid::Uuid::new_v4().simple()),
            idempotency_key: idempotency_key.map(str::to_string),
            provider: provider.to_string(),
            capability: capability.to_string(),
            method: method.to_string(),
            lease_id: lease_id.map(str::to_string),
            state: "pending".to_string(),
            request_sha256: lease::digest(request),
            response_sha256: None,
            response: None,
            error: None,
            created_at: now(),
            finished_at: None,
        };
        self.with(|connection| {
            let transaction = connection.transaction()?;
            if let Some(key) = idempotency_key {
                let existing = transaction
                    .query_row(
                        "SELECT id, idempotency_key, provider, capability, method, lease_id,
                                state, request_sha256, response_sha256, response, error,
                                created_at, finished_at
                         FROM provider_calls WHERE idempotency_key = ?1",
                        params![key],
                        row_to_call,
                    )
                    .optional()?;
                if let Some(found) = existing {
                    return Ok(if found.is_finished() {
                        CallSlot::Settled(found)
                    } else {
                        CallSlot::InFlight(found)
                    });
                }
            }
            transaction.execute(
                "INSERT INTO provider_calls
                    (id, idempotency_key, provider, capability, method, lease_id, state,
                     request_sha256, response_sha256, response, error, created_at, finished_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, NULL, NULL, NULL, ?8, NULL)",
                params![
                    record.id,
                    record.idempotency_key,
                    record.provider,
                    record.capability,
                    record.method,
                    record.lease_id,
                    record.request_sha256,
                    record.created_at
                ],
            )?;
            transaction.commit()?;
            Ok(CallSlot::Fresh(record.clone()))
        })
    }

    /// Record how a call turned out.
    pub fn finish_call(
        &self,
        id: &str,
        state: &str,
        response: Option<&serde_json::Value>,
        error: Option<&str>,
    ) -> Result<Option<CallRecord>> {
        let hash = response.map(lease::digest);
        // Big answers keep their hash and lose their body: the row still
        // proves what came back without storing it.
        let body = response.and_then(|value| {
            let text = value.to_string();
            (text.len() <= lease::RESPONSE_KEPT_BYTES).then_some(text)
        });
        self.with(|connection| {
            connection.execute(
                "UPDATE provider_calls
                    SET state = ?2, response_sha256 = ?3, response = ?4, error = ?5,
                        finished_at = ?6
                  WHERE id = ?1 AND state = 'pending'",
                params![id, state, hash, body, error, now()],
            )?;
            Ok(())
        })?;
        self.call_by_id(id)
    }

    pub fn call_by_id(&self, id: &str) -> Result<Option<CallRecord>> {
        self.with(|connection| {
            connection
                .query_row(
                    "SELECT id, idempotency_key, provider, capability, method, lease_id,
                            state, request_sha256, response_sha256, response, error,
                            created_at, finished_at
                     FROM provider_calls WHERE id = ?1",
                    params![id],
                    row_to_call,
                )
                .optional()
                .map_err(Into::into)
        })
    }

    /// Every call made under one lease — the other end of the authorisation
    /// chain: from a permission to what was actually done with it.
    pub fn calls_under_lease(&self, lease_id: &str) -> Result<Vec<CallRecord>> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, idempotency_key, provider, capability, method, lease_id,
                        state, request_sha256, response_sha256, response, error,
                        created_at, finished_at
                 FROM provider_calls WHERE lease_id = ?1 ORDER BY created_at, id",
            )?;
            let rows = statement.query_map(params![lease_id], row_to_call)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// Calls still in flight for a provider, so a restart can settle them.
    ///
    /// A call left `pending` by a crash is not evidence that it did not
    /// happen — only that nobody heard the answer. It is reported rather than
    /// quietly resolved either way.
    pub fn unsettled_calls(&self, provider: Option<&str>) -> Result<Vec<CallRecord>> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, idempotency_key, provider, capability, method, lease_id,
                        state, request_sha256, response_sha256, response, error,
                        created_at, finished_at
                 FROM provider_calls
                  WHERE state = 'pending' AND (?1 IS NULL OR provider = ?1)
                  ORDER BY created_at",
            )?;
            let rows = statement.query_map(params![provider], row_to_call)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// Ask the user about one action.
    pub fn request_approval(
        &self,
        method: &str,
        risk: &str,
        ask: &Ask,
        command: Option<&str>,
        ttl_seconds: Option<i64>,
    ) -> Result<Approval> {
        let approval = Approval {
            id: format!("apr_{}", uuid::Uuid::new_v4().simple()),
            method: method.to_string(),
            actor: ask.actor.clone(),
            task_id: ask.task_id.clone(),
            resource: ask.resource.clone(),
            command: command.map(str::to_string),
            risk: risk.to_string(),
            state: ApprovalState::Pending,
            created_at: now(),
            expires_at: ttl_seconds.map(|seconds| {
                (chrono::Utc::now() + chrono::Duration::seconds(seconds)).to_rfc3339()
            }),
            decided_at: None,
            decided_by: None,
            grant_id: None,
        };
        self.with(|connection| {
            connection.execute(
                "INSERT INTO approvals (id, method, actor, task_id, resource, command, risk,
                                        state, created_at, expires_at, decided_at, decided_by,
                                        grant_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, NULL, NULL)",
                params![
                    approval.id,
                    approval.method,
                    approval.actor,
                    approval.task_id,
                    approval.resource,
                    approval.command,
                    approval.risk,
                    approval.state.as_str(),
                    approval.created_at,
                    approval.expires_at
                ],
            )?;
            Ok(())
        })?;
        Ok(approval)
    }

    pub fn approval(&self, id: &str) -> Result<Option<Approval>> {
        self.with(|connection| {
            connection
                .query_row(
                    "SELECT id, method, actor, task_id, resource, command, risk, state,
                            created_at, expires_at, decided_at, decided_by, grant_id
                     FROM approvals WHERE id = ?1",
                    params![id],
                    row_to_approval,
                )
                .optional()
                .map_err(Into::into)
        })
    }

    /// Questions nobody has answered yet.
    /// The questions whose answers created this grant.
    ///
    /// The other direction from `approvals.grant_id`, so a lease resting on a
    /// grant can name the moment a human actually said yes.
    pub fn approvals_for_grant(&self, grant_id: &str) -> Result<Vec<Approval>> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, method, actor, task_id, resource, command, risk, state,
                        created_at, expires_at, decided_at, decided_by, grant_id
                 FROM approvals WHERE grant_id = ?1 ORDER BY created_at DESC",
            )?;
            let rows = statement.query_map(params![grant_id], row_to_approval)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    pub fn pending_approvals(&self) -> Result<Vec<Approval>> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, method, actor, task_id, resource, command, risk, state,
                        created_at, expires_at, decided_at, decided_by, grant_id
                 FROM approvals WHERE state = 'pending' ORDER BY created_at, id",
            )?;
            let rows = statement.query_map([], row_to_approval)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// Answer a question, optionally remembering the answer as a grant.
    pub fn decide_approval(
        &self,
        id: &str,
        allowed: bool,
        decided_by: &str,
        remember: Option<NewGrant>,
    ) -> Result<Approval> {
        // The grant is created first and outside the settle, so an approval
        // can never end up pointing at a grant that does not exist.
        let grant = match (allowed, remember) {
            (true, Some(spec)) => Some(self.create_grant(spec)?),
            _ => None,
        };
        self.with(|connection| {
            let state = if allowed {
                ApprovalState::Allowed
            } else {
                ApprovalState::Denied
            };
            let changed = connection.execute(
                "UPDATE approvals
                    SET state = ?2, decided_at = ?3, decided_by = ?4, grant_id = ?5
                  WHERE id = ?1 AND state = 'pending'",
                params![
                    id,
                    state.as_str(),
                    now(),
                    decided_by,
                    grant.as_ref().map(|grant| grant.id.clone())
                ],
            )?;
            if changed != 1 {
                anyhow::bail!("approval {id} was already settled");
            }
            Ok(())
        })?;
        self.approval(id)?
            .ok_or_else(|| anyhow::anyhow!("approval {id} vanished while being decided"))
    }

    /// Settle questions nobody answered in time.
    ///
    /// `Expired`, never `Denied`: nobody said no, they just were not there,
    /// and a caller that retries a refusal must not retry an absence.
    pub fn expire_approvals(&self) -> Result<Vec<String>> {
        self.with(|connection| {
            let stamp = now();
            let transaction = connection.transaction()?;
            let stale: Vec<String> = {
                let mut statement = transaction.prepare(
                    "SELECT id FROM approvals
                      WHERE state = 'pending' AND expires_at IS NOT NULL AND expires_at < ?1",
                )?;
                let rows = statement
                    .query_map(params![stamp], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                drop(statement);
                rows
            };
            for id in &stale {
                transaction.execute(
                    "UPDATE approvals SET state = 'expired', decided_at = ?2 WHERE id = ?1",
                    params![id, stamp],
                )?;
            }
            transaction.commit()?;
            Ok(stale)
        })
    }

    // ---- events ------------------------------------------------------

    /// Everything after `cursor`, oldest first. A subscriber remembers the
    /// `seq` of the last event it handled and passes it back.
    pub fn events_since(&self, cursor: i64, limit: i64) -> Result<Vec<Event>> {
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT seq, at, task_id, run_id, step_id, kind, payload
                 FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2",
            )?;
            let rows = statement.query_map(params![cursor, limit], row_to_event)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    /// The newest cursor, for a reader that wants only what happens next.
    pub fn latest_cursor(&self) -> Result<i64> {
        self.with(|connection| {
            Ok(connection.query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |row| {
                row.get(0)
            })?)
        })
    }

    // ---- shared transition ------------------------------------------

    /// The one place a state changes.
    ///
    /// Both guards matter and they catch different mistakes: the edge check
    /// stops a caller inventing a transition the machine does not have, and
    /// the version check stops a caller acting on a row that moved under it.
    fn transition<T>(
        &self,
        table: &'static str,
        id: &str,
        next: State,
        expected_version: i64,
        load: impl Fn(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T> {
        self.with(|connection| {
            let transaction = connection.transaction()?;
            let (current, version): (String, i64) = transaction
                .query_row(
                    &format!("SELECT state, version FROM {table} WHERE id = ?1"),
                    params![id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or_else(|| anyhow::anyhow!("no {table} row with id {id}"))?;
            let current = State::parse(&current)?;
            if version != expected_version {
                anyhow::bail!(
                    "{table} {id} is at version {version}, not {expected_version}; \
                     something changed it since you read it"
                );
            }
            if !next.may_follow(current) {
                anyhow::bail!(
                    "{table} {id} cannot go from {} to {}",
                    current.as_str(),
                    next.as_str()
                );
            }
            let changed = transaction.execute(
                &format!(
                    "UPDATE {table} SET state = ?2, version = version + 1, updated_at = ?3
                      WHERE id = ?1 AND version = ?4"
                ),
                params![id, next.as_str(), now(), expected_version],
            )?;
            if changed != 1 {
                anyhow::bail!("{table} {id} was changed by someone else mid-transition");
            }
            append_event(
                &transaction,
                (table == "tasks").then_some(id),
                (table == "runs").then_some(id),
                (table == "steps").then_some(id),
                &format!("{}.{}", table.trim_end_matches('s'), next.as_str()),
                &serde_json::json!({"from": current.as_str(), "to": next.as_str()}),
            )?;
            let loaded = load(&transaction)?;
            transaction.commit()?;
            Ok(loaded)
        })
    }
}

fn append_event(
    connection: &Connection,
    task_id: Option<&str>,
    run_id: Option<&str>,
    step_id: Option<&str>,
    kind: &str,
    payload: &serde_json::Value,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO events (at, task_id, run_id, step_id, kind, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![now(), task_id, run_id, step_id, kind, payload.to_string()],
    )?;
    Ok(())
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: TaskId::parse(&row.get::<_, String>(0)?).map_err(to_sqlite_error)?,
        kind: row.get(1)?,
        title: row.get(2)?,
        state: State::parse(&row.get::<_, String>(3)?).map_err(to_sqlite_error)?,
        version: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        detail: json_column(row, 7)?,
    })
}

fn row_to_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        id: RunId::parse(&row.get::<_, String>(0)?).map_err(to_sqlite_error)?,
        task_id: TaskId::parse(&row.get::<_, String>(1)?).map_err(to_sqlite_error)?,
        attempt: row.get(2)?,
        state: State::parse(&row.get::<_, String>(3)?).map_err(to_sqlite_error)?,
        version: row.get(4)?,
        started_at: row.get(5)?,
        updated_at: row.get(6)?,
        ended_at: row.get(7)?,
    })
}

fn row_to_step(row: &rusqlite::Row<'_>) -> rusqlite::Result<Step> {
    Ok(Step {
        id: StepId::parse(&row.get::<_, String>(0)?).map_err(to_sqlite_error)?,
        run_id: RunId::parse(&row.get::<_, String>(1)?).map_err(to_sqlite_error)?,
        ordinal: row.get(2)?,
        kind: row.get(3)?,
        state: State::parse(&row.get::<_, String>(4)?).map_err(to_sqlite_error)?,
        version: row.get(5)?,
        idempotency_key: row.get(6)?,
        claimed_by: row.get(7)?,
        lease_expires_at: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        detail: json_column(row, 11)?,
    })
}

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let payload: String = row.get(6)?;
    Ok(Event {
        seq: row.get(0)?,
        at: row.get(1)?,
        task_id: row
            .get::<_, Option<String>>(2)?
            .and_then(|raw| TaskId::parse(&raw).ok()),
        run_id: row
            .get::<_, Option<String>>(3)?
            .and_then(|raw| RunId::parse(&raw).ok()),
        step_id: row
            .get::<_, Option<String>>(4)?
            .and_then(|raw| StepId::parse(&raw).ok()),
        kind: row.get(5)?,
        payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
    })
}

/// A JSON column, tolerating a row written before the column existed.
fn json_column(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<serde_json::Value> {
    let raw: Option<String> = row.get(index)?;
    Ok(raw
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({})))
}

fn row_to_agent_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentSession> {
    Ok(AgentSession {
        id: row.get(0)?,
        adapter: row.get(1)?,
        command: row.get(2)?,
        cwd: row.get(3)?,
        task_id: row.get(4)?,
        run_id: row.get(5)?,
        step_id: row.get(6)?,
        idempotency_key: row.get(7)?,
        lease_id: row.get(8)?,
        state: row.get(9)?,
        exit_code: row.get(10)?,
        signal: row.get(11)?,
        reason: row.get(12)?,
        started_at: row.get(13)?,
        ended_at: row.get(14)?,
    })
}

fn row_to_workspace(row: &rusqlite::Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: row.get(0)?,
        name: row.get(1)?,
        root: row.get(2)?,
        created_at: row.get(3)?,
        archived_at: row.get(4)?,
    })
}

fn row_to_artifact(row: &rusqlite::Row<'_>) -> rusqlite::Result<Artifact> {
    Ok(Artifact {
        id: row.get(0)?,
        sha256: row.get(1)?,
        bytes: row.get(2)?,
        media_type: row.get(3)?,
        task_id: row.get(4)?,
        step_id: row.get(5)?,
        call_id: row.get(6)?,
        origin: row.get(7)?,
        name: row.get(8)?,
        created_at: row.get(9)?,
    })
}

fn row_to_call(row: &rusqlite::Row<'_>) -> rusqlite::Result<CallRecord> {
    Ok(CallRecord {
        id: row.get(0)?,
        idempotency_key: row.get(1)?,
        provider: row.get(2)?,
        capability: row.get(3)?,
        method: row.get(4)?,
        lease_id: row.get(5)?,
        state: row.get(6)?,
        request_sha256: row.get(7)?,
        response_sha256: row.get(8)?,
        response: row.get(9)?,
        error: row.get(10)?,
        created_at: row.get(11)?,
        finished_at: row.get(12)?,
    })
}

fn row_to_lease(row: &rusqlite::Row<'_>) -> rusqlite::Result<Lease> {
    Ok(Lease {
        id: row.get(0)?,
        provider: row.get(1)?,
        capability: row.get(2)?,
        actor: row.get(3)?,
        task_id: row.get(4)?,
        step_id: row.get(5)?,
        grant_id: row.get(6)?,
        approval_id: row.get(7)?,
        issued_at: row.get(8)?,
        expires_at: row.get(9)?,
        renewed_at: row.get(10)?,
        revoked_at: row.get(11)?,
        epoch: row.get(12)?,
        last_seq: row.get(13)?,
    })
}

fn row_to_grant(row: &rusqlite::Row<'_>) -> rusqlite::Result<Grant> {
    Ok(Grant {
        id: row.get(0)?,
        scope: Scope::parse(&row.get::<_, String>(1)?).map_err(to_sqlite_error)?,
        method: row.get(2)?,
        actor: row.get(3)?,
        task_id: row.get(4)?,
        resource: row.get(5)?,
        max_risk: row.get(6)?,
        created_at: row.get(7)?,
        expires_at: row.get(8)?,
        revoked_at: row.get(9)?,
        consumed_at: row.get(10)?,
    })
}

fn row_to_approval(row: &rusqlite::Row<'_>) -> rusqlite::Result<Approval> {
    Ok(Approval {
        id: row.get(0)?,
        method: row.get(1)?,
        actor: row.get(2)?,
        task_id: row.get(3)?,
        resource: row.get(4)?,
        command: row.get(5)?,
        risk: row.get(6)?,
        state: ApprovalState::parse(&row.get::<_, String>(7)?).map_err(to_sqlite_error)?,
        created_at: row.get(8)?,
        expires_at: row.get(9)?,
        decided_at: row.get(10)?,
        decided_by: row.get(11)?,
        grant_id: row.get(12)?,
    })
}

fn to_sqlite_error(error: anyhow::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error.into())
}

/// The store has no logger of its own; the Core prints what it says.
fn log_line(message: &str) {
    eprintln!("unterm-tasks: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> TaskStore {
        TaskStore::in_memory().expect("in-memory store")
    }

    /// A task, one attempt at it, one step inside that — the shape everything
    /// else is built from.
    fn scaffold(store: &TaskStore) -> (Task, Run) {
        let task = store.create_task("agent", "write the tests").unwrap();
        let run = store.start_run(&task.id).unwrap();
        (task, run)
    }

    #[test]
    fn a_client_can_name_its_task_and_runs_before_the_store_sees_them() {
        let store = store();
        let id = TaskId::parse("tsk_named_by_the_console").unwrap();
        let task = store
            .create_task_as(id.clone(), "console", "weekly report", serde_json::json!({"a": 1}))
            .unwrap();
        assert_eq!(task.id, id);
        assert!(
            store.create_task_as(id.clone(), "console", "again", serde_json::json!({})).is_err(),
            "an existing id is refused, not overwritten"
        );
        store.set_task_title(&id, "monthly report").unwrap();
        assert_eq!(store.task(&id).unwrap().unwrap().title, "monthly report");

        let run_id = RunId::parse("run_named_too").unwrap();
        let run = store.start_run_as(&id, run_id.clone()).unwrap();
        assert_eq!((run.id, run.attempt), (run_id.clone(), 1));
        assert!(store.start_run_as(&id, run_id).is_err());
        assert_eq!(store.start_run(&id).unwrap().attempt, 2, "named and unnamed runs share one count");
    }

    #[test]
    fn a_task_runs_and_the_event_stream_says_so_in_order() {
        let store = store();
        let (task, run) = scaffold(&store);
        let step = store
            .request_step(&run.id, "tool.call", None)
            .unwrap()
            .step()
            .clone();

        assert_eq!(task.state, State::Pending);
        assert_eq!(run.attempt, 1);
        assert_eq!(step.ordinal, 1);

        let events = store.events_since(0, 100).unwrap();
        let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, ["task.created", "run.started", "step.requested"]);
        // The cursor is what every reader follows; it must only go up.
        let seqs: Vec<i64> = events.iter().map(|e| e.seq).collect();
        let mut sorted = seqs.clone();
        sorted.sort_unstable();
        assert_eq!(seqs, sorted, "the event cursor went backwards");
        assert_eq!(store.latest_cursor().unwrap(), *seqs.last().unwrap());

        // Reading from a cursor returns only what came after it.
        let tail = store.events_since(seqs[0], 100).unwrap();
        assert_eq!(tail.len(), 2);
        assert!(tail.iter().all(|e| e.seq > seqs[0]));
    }

    #[test]
    fn the_same_idempotency_key_never_makes_a_second_step() {
        // The gate: "相同幂等键不重复副作用". A retried message must not run
        // the tool twice, and the store — not the caller — is what enforces
        // it, because the caller is the thing that just retried.
        let store = store();
        let (_task, run) = scaffold(&store);

        let first = store
            .request_step(&run.id, "shell.write", Some("call-42"))
            .unwrap();
        assert!(first.is_new(), "the first request creates the step");

        let second = store
            .request_step(&run.id, "shell.write", Some("call-42"))
            .unwrap();
        assert!(
            !second.is_new(),
            "the second request must not create a second step"
        );
        assert_eq!(
            first.step().id,
            second.step().id,
            "the repeat must name the step that already exists"
        );
        assert_eq!(store.steps(&run.id).unwrap().len(), 1);

        // A different key is a different request, and no key at all opts out
        // of the whole mechanism rather than colliding with the others.
        assert!(store
            .request_step(&run.id, "shell.write", Some("call-43"))
            .unwrap()
            .is_new());
        assert!(store.request_step(&run.id, "shell.write", None).unwrap().is_new());
        assert!(store.request_step(&run.id, "shell.write", None).unwrap().is_new());
        assert_eq!(store.steps(&run.id).unwrap().len(), 4);
    }

    #[test]
    fn a_repeated_request_does_not_disturb_the_step_already_running() {
        let store = store();
        let (_task, run) = scaffold(&store);
        let step = store
            .request_step(&run.id, "tool", Some("once"))
            .unwrap()
            .step()
            .clone();
        assert!(matches!(
            store.claim_step(&step.id, "worker-a", 60).unwrap(),
            Claim::Granted(_)
        ));

        let again = store.request_step(&run.id, "tool", Some("once")).unwrap();
        let seen = again.step();
        assert!(!again.is_new());
        // The retry must not reset it to pending, or a second worker could
        // claim work that is already in flight.
        assert_eq!(seen.state, State::Running);
        assert_eq!(seen.claimed_by.as_deref(), Some("worker-a"));
    }

    #[test]
    fn only_one_worker_can_claim_a_step() {
        let store = store();
        let (_task, run) = scaffold(&store);
        let step = store.request_step(&run.id, "tool", None).unwrap().step().clone();

        let first = store.claim_step(&step.id, "worker-a", 60).unwrap();
        let second = store.claim_step(&step.id, "worker-b", 60).unwrap();

        assert!(matches!(first, Claim::Granted(_)));
        match second {
            Claim::Denied { held_by, state } => {
                assert_eq!(held_by.as_deref(), Some("worker-a"));
                assert_eq!(state, State::Running);
            }
            Claim::Granted(_) => panic!("two workers were both told they own the step"),
        }
    }

    #[test]
    fn concurrent_workers_across_connections_still_produce_one_winner() {
        // The gate: "并发 Worker 不重复执行 Step". The single-connection test
        // above cannot see the real race — two processes, two connections,
        // one row. This one opens the same file twice, which is how the Core
        // and a worker actually meet.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.db");
        let store = TaskStore::open(&path).unwrap();
        let (_task, run) = scaffold(&store);
        let step = store.request_step(&run.id, "tool", None).unwrap().step().clone();

        let mut granted = 0;
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|worker| {
                    let path = path.clone();
                    let step = step.id.clone();
                    scope.spawn(move || {
                        let store = TaskStore::open(&path).expect("second connection");
                        matches!(
                            store.claim_step(&step, &format!("worker-{worker}"), 60),
                            Ok(Claim::Granted(_))
                        )
                    })
                })
                .collect();
            for handle in handles {
                if handle.join().unwrap() {
                    granted += 1;
                }
            }
        });

        assert_eq!(granted, 1, "{granted} workers were each told the step was theirs");
        let step = store.step(&step.id).unwrap().unwrap();
        assert_eq!(step.state, State::Running);
        assert!(step.claimed_by.is_some());
    }

    #[test]
    fn a_stale_version_loses_instead_of_overwriting() {
        let store = store();
        let task = store.create_task("agent", "t").unwrap();
        let running = store
            .set_task_state(&task.id, State::Running, task.version)
            .unwrap();
        assert_eq!(running.version, task.version + 1);

        // Someone still holding the old version tries to act on it. Letting
        // this through is how a cancel silently undoes a completion.
        let error = store
            .set_task_state(&task.id, State::Succeeded, task.version)
            .unwrap_err()
            .to_string();
        assert!(error.contains("version"), "got: {error}");
        assert_eq!(store.task(&task.id).unwrap().unwrap().state, State::Running);
    }

    #[test]
    fn the_store_refuses_a_transition_the_machine_does_not_have() {
        let store = store();
        let task = store.create_task("agent", "t").unwrap();
        // Pending -> Succeeded skips ever having run.
        let error = store
            .set_task_state(&task.id, State::Succeeded, task.version)
            .unwrap_err()
            .to_string();
        assert!(error.contains("cannot go from"), "got: {error}");

        let running = store
            .set_task_state(&task.id, State::Running, task.version)
            .unwrap();
        let done = store
            .set_task_state(&task.id, State::Succeeded, running.version)
            .unwrap();
        // And terminal is terminal.
        let error = store
            .set_task_state(&task.id, State::Failed, done.version)
            .unwrap_err()
            .to_string();
        assert!(error.contains("cannot go from"), "got: {error}");
    }

    #[test]
    fn a_lapsed_lease_becomes_interrupted_rather_than_running_forever() {
        let store = store();
        let (_task, run) = scaffold(&store);
        let alive = store.request_step(&run.id, "tool", None).unwrap().step().clone();
        let doomed = store.request_step(&run.id, "tool", None).unwrap().step().clone();

        store.claim_step(&alive.id, "worker-a", 600).unwrap();
        // A lease that expired a second ago: the worker holding it is gone.
        store.claim_step(&doomed.id, "worker-b", -1).unwrap();

        let reclaimed = store.reconcile().unwrap();
        assert_eq!(reclaimed.len(), 1, "reconcile took the wrong number of steps");
        assert_eq!(reclaimed[0].id, doomed.id);
        assert_eq!(reclaimed[0].state, State::Interrupted);
        assert!(
            reclaimed[0].claimed_by.is_none(),
            "an interrupted step must not still look held"
        );
        // The live worker is untouched: reconciliation that stops healthy work
        // is worse than the stall it is fixing.
        assert_eq!(store.step(&alive.id).unwrap().unwrap().state, State::Running);

        // And it is said out loud, so a Cockpit following the cursor learns.
        let kinds: Vec<String> = store
            .events_since(0, 100)
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert!(kinds.iter().any(|k| k == "step.interrupted"), "{kinds:?}");
    }

    #[test]
    fn a_heartbeat_keeps_a_claim_alive_without_disturbing_its_version() {
        let store = store();
        let (_task, run) = scaffold(&store);
        let step = store.request_step(&run.id, "tool", None).unwrap().step().clone();
        // Claim with an already-expired lease, then prove a heartbeat rescues
        // it before reconciliation can take it.
        store.claim_step(&step.id, "worker-a", -1).unwrap();
        let claimed = store.step(&step.id).unwrap().unwrap();

        assert!(store.heartbeat_step(&step.id, "worker-a", 600).unwrap());
        assert!(store.reconcile().unwrap().is_empty(), "a beating worker was reaped");

        let after = store.step(&step.id).unwrap().unwrap();
        assert_eq!(
            after.version, claimed.version,
            "a liveness ping is not a change to what the step is, and bumping \
             the version would make every reader's compare-and-swap lose to it"
        );
        // Somebody else's heartbeat is not accepted.
        assert!(!store.heartbeat_step(&step.id, "worker-b", 600).unwrap());
    }

    #[test]
    fn every_state_survives_the_process_dying_underneath_it() {
        // The gate: "逐状态强杀恢复". Dropping the store is what a hard kill
        // leaves behind — no flush, no cleanup, no chance to write a verdict.
        // Reopening must find exactly what was committed, and reconciliation
        // must turn the one ambiguous case into a verdict.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.db");

        let mut expected = Vec::new();
        {
            let store = TaskStore::open(&path).unwrap();
            for state in [
                State::Pending,
                State::Running,
                State::Succeeded,
                State::Failed,
                State::Cancelled,
            ] {
                let task = store.create_task("agent", "survivor").unwrap();
                let mut current = task.clone();
                if state != State::Pending {
                    current = store
                        .set_task_state(&task.id, State::Running, current.version)
                        .unwrap();
                }
                if state != State::Pending && state != State::Running {
                    current = store
                        .set_task_state(&task.id, state, current.version)
                        .unwrap();
                }
                expected.push((current.id.clone(), state));
            }
            // Plus a step left mid-flight by a worker that never came back.
            let (_t, run) = scaffold(&store);
            let step = store.request_step(&run.id, "tool", None).unwrap().step().clone();
            store.claim_step(&step.id, "doomed-worker", -1).unwrap();
            // No drop ceremony: this is the kill.
        }

        let store = TaskStore::open(&path).unwrap();
        for (id, state) in expected {
            let task = store.task(&id).unwrap().expect("task survived the kill");
            assert_eq!(task.state, state, "task {id} came back in the wrong state");
        }
        let reclaimed = store.reconcile().unwrap();
        assert_eq!(
            reclaimed.len(),
            1,
            "the step whose worker died must come back as a verdict, not a stall"
        );
        assert_eq!(reclaimed[0].state, State::Interrupted);
    }

    #[test]
    fn retrying_adds_an_attempt_instead_of_rewriting_the_failed_one() {
        let store = store();
        let task = store.create_task("agent", "flaky").unwrap();
        let first = store.start_run(&task.id).unwrap();
        let finished = store
            .finish_run(&first.id, State::Failed, first.version)
            .unwrap();
        assert_eq!(finished.state, State::Failed);

        let second = store.start_run(&task.id).unwrap();
        assert_eq!(second.attempt, 2);

        let runs = store.runs(&task.id).unwrap();
        assert_eq!(runs.len(), 2);
        // The record of the failure is exactly as it was left: a retry that
        // edits the previous attempt destroys the evidence of what went wrong.
        assert_eq!(runs[0].state, State::Failed);
        assert!(runs[0].ended_at.is_some(), "a finished run records when");
        assert_eq!(runs[1].state, State::Running);
        assert!(runs[1].ended_at.is_none());
    }

    #[test]
    fn a_finished_step_stops_looking_claimed() {
        let store = store();
        let (_task, run) = scaffold(&store);
        let step = store.request_step(&run.id, "tool", None).unwrap().step().clone();
        let Claim::Granted(claimed) = store.claim_step(&step.id, "worker-a", 600).unwrap() else {
            panic!("the claim was refused");
        };
        let done = store
            .finish_step(&claimed.id, State::Succeeded, claimed.version)
            .unwrap();
        assert_eq!(done.state, State::Succeeded);

        let after = store.step(&step.id).unwrap().unwrap();
        assert!(after.claimed_by.is_none(), "a finished step still looks held");
        assert!(after.lease_expires_at.is_none());
        // And reconciliation has no interest in it any more.
        assert!(store.reconcile().unwrap().is_empty());
    }

    #[test]
    fn cancelling_a_task_stops_everything_still_live_under_it() {
        let store = store();
        let task = store.create_task("agent", "big job").unwrap();
        let running = store
            .set_task_state(&task.id, State::Running, task.version)
            .unwrap();
        let run = store.start_run(&task.id).unwrap();
        let pending = store.request_step(&run.id, "a", None).unwrap().step().clone();
        let claimed = store.request_step(&run.id, "b", None).unwrap().step().clone();
        let finished = store.request_step(&run.id, "c", None).unwrap().step().clone();
        store.claim_step(&claimed.id, "worker-a", 600).unwrap();
        let claimed_now = store.step(&claimed.id).unwrap().unwrap();
        let Claim::Granted(done) = store.claim_step(&finished.id, "worker-b", 600).unwrap() else {
            panic!("claim refused");
        };
        store
            .finish_step(&done.id, State::Succeeded, done.version)
            .unwrap();

        let cancelled = store.cancel_task(&task.id, running.version).unwrap();
        assert_eq!(cancelled.state, State::Cancelled);

        // Everything that was still live is stopped...
        assert_eq!(store.step(&pending.id).unwrap().unwrap().state, State::Cancelled);
        let was_running = store.step(&claimed.id).unwrap().unwrap();
        assert_eq!(was_running.state, State::Cancelled);
        assert!(
            was_running.claimed_by.is_none(),
            "a cancelled step must not still look held by a worker"
        );
        assert_eq!(store.runs(&task.id).unwrap()[0].state, State::Cancelled);
        // ...and what already reached a verdict keeps it. Rewriting that
        // would be a lie about what happened.
        assert_eq!(store.step(&finished.id).unwrap().unwrap().state, State::Succeeded);
        let _ = claimed_now;
    }

    #[test]
    fn cancelling_refuses_a_stale_version_and_a_finished_task() {
        let store = store();
        let task = store.create_task("agent", "t").unwrap();
        let error = store.cancel_task(&task.id, task.version + 9).unwrap_err().to_string();
        assert!(error.contains("version"), "got: {error}");

        let running = store
            .set_task_state(&task.id, State::Running, task.version)
            .unwrap();
        let done = store
            .set_task_state(&task.id, State::Succeeded, running.version)
            .unwrap();
        let error = store.cancel_task(&task.id, done.version).unwrap_err().to_string();
        assert!(error.contains("cannot go from"), "got: {error}");
        assert_eq!(store.task(&task.id).unwrap().unwrap().state, State::Succeeded);
    }

    #[test]
    fn recovery_rolls_a_dead_worker_all_the_way_up() {
        let store = store();
        let task = store.create_task("agent", "t").unwrap();
        store.set_task_state(&task.id, State::Running, task.version).unwrap();
        let run = store.start_run(&task.id).unwrap();
        let step = store.request_step(&run.id, "tool", None).unwrap().step().clone();
        // A worker took it and died: the lease is already in the past.
        store.claim_step(&step.id, "ghost", -1).unwrap();

        let recovery = store.recover().unwrap();
        assert!(!recovery.is_clean());
        assert_eq!(recovery.steps_interrupted, vec![step.id.clone()]);
        assert_eq!(recovery.runs_interrupted, vec![run.id.clone()]);
        assert_eq!(recovery.tasks_interrupted, vec![task.id.clone()]);

        assert_eq!(store.step(&step.id).unwrap().unwrap().state, State::Interrupted);
        assert_eq!(store.runs(&task.id).unwrap()[0].state, State::Interrupted);
        assert_eq!(store.task(&task.id).unwrap().unwrap().state, State::Interrupted);

        // And it is idempotent: a second start must not keep rewriting rows.
        assert!(store.recover().unwrap().is_clean());
    }

    #[test]
    fn recovery_leaves_work_a_new_worker_could_still_pick_up() {
        let store = store();
        let task = store.create_task("agent", "t").unwrap();
        store.set_task_state(&task.id, State::Running, task.version).unwrap();
        let run = store.start_run(&task.id).unwrap();
        let dead = store.request_step(&run.id, "dead", None).unwrap().step().clone();
        let waiting = store.request_step(&run.id, "waiting", None).unwrap().step().clone();
        store.claim_step(&dead.id, "ghost", -1).unwrap();

        let recovery = store.recover().unwrap();
        assert_eq!(recovery.steps_interrupted, vec![dead.id.clone()]);
        assert!(
            recovery.runs_interrupted.is_empty(),
            "a run with pending work is resumable; reclaiming it is worse than the stall"
        );
        assert!(recovery.tasks_interrupted.is_empty());
        assert_eq!(store.step(&waiting.id).unwrap().unwrap().state, State::Pending);
        assert_eq!(store.runs(&task.id).unwrap()[0].state, State::Running);
    }

    #[test]
    fn recovery_does_not_touch_a_run_that_has_not_started_work_yet() {
        let store = store();
        let task = store.create_task("agent", "t").unwrap();
        store.set_task_state(&task.id, State::Running, task.version).unwrap();
        store.start_run(&task.id).unwrap();
        // No steps at all: nothing says this is dead, only that it is young.
        assert!(store.recover().unwrap().is_clean());
    }

    #[test]
    fn the_detail_column_carries_a_callers_own_data_untouched() {
        let store = store();
        let detail = serde_json::json!({"repo": "/tmp/x", "branch": "fleet/a-1"});
        let task = store
            .create_task_with_detail("fleet", "ship it", detail.clone())
            .unwrap();
        assert_eq!(task.detail, detail);
        assert_eq!(store.task(&task.id).unwrap().unwrap().detail, detail);

        let run = store.start_run(&task.id).unwrap();
        let member = serde_json::json!({"agent": "claude", "review": "pending"});
        let step = store
            .request_step_with_detail(&run.id, "fleet.member", Some("k"), member.clone())
            .unwrap();
        assert_eq!(step.step().detail, member);
        // A state change must not disturb it: the engine carries this, it
        // does not own it.
        let claimed = step.step().clone();
        store.claim_step(&claimed.id, "w", 60).unwrap();
        assert_eq!(store.step(&claimed.id).unwrap().unwrap().detail, member);
    }

    #[test]
    fn an_approval_outlives_the_process_that_asked() {
        // The gate: "审批可跨重启". A question that evaporates when the Core
        // restarts turns an agent's request into a silent refusal, and the
        // user never learns they were asked.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.db");
        let id = {
            let store = TaskStore::open(&path).unwrap();
            let ask = Ask {
                method: "session.destroy".to_string(),
                actor: Some("claude".to_string()),
                ..Ask::default()
            };
            store
                .request_approval("session.destroy", "destructive", &ask, None, None)
                .unwrap()
                .id
        };

        let store = TaskStore::open(&path).unwrap();
        let pending = store.pending_approvals().unwrap();
        assert_eq!(pending.len(), 1, "the question did not survive the restart");
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].actor.as_deref(), Some("claude"));

        // And it can still be answered afterwards.
        let settled = store.decide_approval(&id, true, "the user", None).unwrap();
        assert_eq!(settled.state, ApprovalState::Allowed);
        assert!(store.pending_approvals().unwrap().is_empty());
    }

    #[test]
    fn revoking_a_grant_stops_the_next_action_and_anything_already_waiting() {
        // The gate: "Grant 撤销立即阻断等待及后续动作".
        let store = store();
        let ask = Ask {
            method: "exec.run".to_string(),
            risk_rank: unterm_tasks_risk("local_mutation"),
            ..Ask::default()
        };
        let grant = store
            .create_grant(NewGrant {
                scope_or_once: Some(Scope::Always),
                max_risk: Some("destructive".to_string()),
                ..NewGrant::default()
            })
            .unwrap();
        assert!(store.covering_grant(&ask).unwrap().is_some());

        // A question that this grant is on the hook for.
        let waiting = store
            .request_approval("exec.run", "local_mutation", &ask, None, None)
            .unwrap();
        store
            .with(|connection| {
                connection.execute(
                    "UPDATE approvals SET grant_id = ?2 WHERE id = ?1",
                    rusqlite::params![waiting.id, grant.id],
                )?;
                Ok(())
            })
            .unwrap();

        let cut_off = store.revoke_grant(&grant.id).unwrap();

        assert_eq!(cut_off, 1, "the waiting question was left hanging");
        assert_eq!(
            store.approval(&waiting.id).unwrap().unwrap().state,
            ApprovalState::Revoked,
            "a waiter must not still believe permission is coming"
        );
        assert!(
            store.covering_grant(&ask).unwrap().is_none(),
            "the next action must not still be covered"
        );
    }

    #[test]
    fn revoking_a_grant_stops_work_already_running_on_its_authority() {
        // The half of the gate that a pending-approval cascade cannot reach:
        // by the time work is under way there is no question outstanding, and
        // stopping only future actions would leave the very thing the user
        // withdrew permission for still running.
        let store = store();
        let grant = store
            .create_grant(NewGrant {
                scope_or_once: Some(Scope::Always),
                max_risk: Some("destructive".to_string()),
                ..NewGrant::default()
            })
            .unwrap();
        let (_task, run) = scaffold(&store);
        let authorised = store.request_step(&run.id, "tool", None).unwrap().step().clone();
        let unrelated = store.request_step(&run.id, "tool", None).unwrap().step().clone();
        store.claim_step(&authorised.id, "worker", 600).unwrap();
        store.claim_step(&unrelated.id, "worker", 600).unwrap();
        store
            .attribute_step_to_grant(&authorised.id, &grant.id)
            .unwrap();

        let cut_off = store.revoke_grant(&grant.id).unwrap();
        assert_eq!(cut_off, 1);
        assert_eq!(
            store.step(&authorised.id).unwrap().unwrap().state,
            State::Cancelled,
            "work running on a revoked grant kept going"
        );
        assert_eq!(
            store.step(&unrelated.id).unwrap().unwrap().state,
            State::Running,
            "revocation reached work it had no business touching"
        );
    }

    #[test]
    fn a_once_grant_is_spent_after_it_is_used() {
        let store = store();
        let ask = Ask {
            method: "session.destroy".to_string(),
            risk_rank: unterm_tasks_risk("destructive"),
            ..Ask::default()
        };
        // Named, because a single-use grant is bound to the call it was
        // given for -- which is what every real one is: `answer` writes the
        // method of the question being answered into it. An unnamed one is
        // "one free pass, for anything", and that is the shape the contract
        // refuses for a destructive action.
        let grant = store
            .create_grant(NewGrant {
                scope_or_once: Some(Scope::Once),
                method: Some("session.destroy".to_string()),
                max_risk: Some("destructive".to_string()),
                ..NewGrant::default()
            })
            .unwrap();
        assert_eq!(
            store.covering_grant(&ask).unwrap().map(|g| g.id),
            Some(grant.id.clone())
        );

        store.consume_grant(&grant.id).unwrap();
        assert!(
            store.covering_grant(&ask).unwrap().is_none(),
            "\"just this once\" covered a second action"
        );

        // And "once, for anything" is not a thing a deletion may be answered
        // by. Checked here, with the named grant already spent, so nothing
        // else can be the reason the answer comes back empty.
        store
            .create_grant(NewGrant {
                scope_or_once: Some(Scope::Once),
                max_risk: Some("destructive".to_string()),
                ..NewGrant::default()
            })
            .unwrap();
        assert!(
            store.covering_grant(&ask).unwrap().is_none(),
            "a once-grant that names no method answered for a deletion"
        );
    }

    #[test]
    fn saying_yes_and_remembering_it_creates_the_grant_the_answer_described() {
        let store = store();
        let ask = Ask {
            method: "exec.run".to_string(),
            task_id: Some("tsk_1".to_string()),
            risk_rank: unterm_tasks_risk("local_mutation"),
            ..Ask::default()
        };
        let question = store
            .request_approval("exec.run", "local_mutation", &ask, Some("cargo test"), None)
            .unwrap();
        let settled = store
            .decide_approval(
                &question.id,
                true,
                "the user",
                Some(NewGrant {
                    scope_or_once: Some(Scope::Task),
                    task_id: Some("tsk_1".to_string()),
                    max_risk: Some("local_mutation".to_string()),
                    ..NewGrant::default()
                }),
            )
            .unwrap();

        assert_eq!(settled.state, ApprovalState::Allowed);
        let grant_id = settled.grant_id.expect("remembering must create a grant");
        // The grant covers this task and only this task.
        assert!(store.covering_grant(&ask).unwrap().is_some());
        let elsewhere = Ask {
            task_id: Some("tsk_2".to_string()),
            ..ask.clone()
        };
        assert!(store.covering_grant(&elsewhere).unwrap().is_none());
        // And revoking it reaches the action that would next have used it.
        store.revoke_grant(&grant_id).unwrap();
        assert!(store.covering_grant(&ask).unwrap().is_none());
    }

    #[test]
    fn an_unanswered_question_expires_rather_than_being_refused() {
        let store = store();
        let ask = Ask {
            method: "session.destroy".to_string(),
            ..Ask::default()
        };
        let question = store
            .request_approval("session.destroy", "destructive", &ask, None, Some(-1))
            .unwrap();
        let expired = store.expire_approvals().unwrap();
        assert_eq!(expired, vec![question.id.clone()]);
        assert_eq!(
            store.approval(&question.id).unwrap().unwrap().state,
            ApprovalState::Expired,
            "nobody said no; they were not there, and the two must stay tellable apart"
        );
        // Settling it twice must not resurrect it.
        assert!(store.decide_approval(&question.id, true, "late", None).is_err());
    }

    /// Local mirror of the gateway's ranking, so this crate's tests do not
    /// depend on the crate that sits above it.
    fn unterm_tasks_risk(name: &str) -> u8 {
        crate::approval::risk_rank(name)
    }

    #[test]
    fn the_file_keeps_everything_across_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.db");
        let (task_id, cursor) = {
            let store = TaskStore::open(&path).unwrap();
            let (task, run) = scaffold(&store);
            store.request_step(&run.id, "tool", Some("k")).unwrap();
            (task.id, store.latest_cursor().unwrap())
        };
        let store = TaskStore::open(&path).unwrap();
        assert!(store.task(&task_id).unwrap().is_some());
        assert_eq!(store.latest_cursor().unwrap(), cursor, "the cursor restarted");
        // The idempotency key outlives the process too, which is the only way
        // it can protect against a retry that follows a crash.
        let run = store.runs(&task_id).unwrap().remove(0);
        assert!(!store.request_step(&run.id, "tool", Some("k")).unwrap().is_new());
    }

    // ---- capability leases -------------------------------------------

    fn a_lease(store: &TaskStore) -> Lease {
        store
            .issue_lease(NewLease {
                provider: "unzoo".into(),
                capability: "browser".into(),
                ttl_seconds: 300,
                ..NewLease::default()
            })
            .unwrap()
    }

    #[test]
    fn a_fresh_lease_can_be_used_once_per_sequence_number() {
        let store = TaskStore::in_memory().unwrap();
        let lease = a_lease(&store);
        let present = |seq| Presented {
            lease_id: lease.id.clone(),
            epoch: lease.epoch,
            seq,
        };
        assert!(store.use_lease(&present(1)).unwrap().is_ok());
        assert!(store.use_lease(&present(2)).unwrap().is_ok());
    }

    #[test]
    fn a_recorded_exchange_cannot_be_replayed() {
        // The whole reason leases carry a sequence number: something that
        // captured a legitimate use must not be able to repeat it.
        let store = TaskStore::in_memory().unwrap();
        let lease = a_lease(&store);
        let use_once = Presented {
            lease_id: lease.id.clone(),
            epoch: lease.epoch,
            seq: 7,
        };
        assert!(store.use_lease(&use_once).unwrap().is_ok());
        assert_eq!(
            store.use_lease(&use_once).unwrap().unwrap_err(),
            Refusal::Replay
        );
        // And an older number, which is what a recording of an earlier
        // exchange would carry.
        assert_eq!(
            store
                .use_lease(&Presented { seq: 3, ..use_once })
                .unwrap()
                .unwrap_err(),
            Refusal::Replay
        );
    }

    #[test]
    fn renewing_invalidates_the_lease_somebody_else_is_holding() {
        let store = TaskStore::in_memory().unwrap();
        let lease = a_lease(&store);
        let old_epoch = lease.epoch;
        let renewed = store.renew_lease(&lease.id, 600).unwrap().unwrap();
        assert_eq!(renewed.epoch, old_epoch + 1);
        assert!(renewed.expires_at > lease.expires_at);

        assert_eq!(
            store
                .use_lease(&Presented {
                    lease_id: lease.id.clone(),
                    epoch: old_epoch,
                    seq: 1,
                })
                .unwrap()
                .unwrap_err(),
            Refusal::StaleEpoch
        );
        assert!(store
            .use_lease(&Presented {
                lease_id: lease.id,
                epoch: renewed.epoch,
                seq: 1,
            })
            .unwrap()
            .is_ok());
    }

    #[test]
    fn a_revoked_lease_stops_working_immediately() {
        let store = TaskStore::in_memory().unwrap();
        let lease = a_lease(&store);
        assert!(store.revoke_lease(&lease.id).unwrap());
        assert_eq!(
            store
                .use_lease(&Presented {
                    lease_id: lease.id.clone(),
                    epoch: lease.epoch,
                    seq: 1,
                })
                .unwrap()
                .unwrap_err(),
            Refusal::Revoked
        );
        // Revoking twice is not an error, but it does report that there was
        // nothing left to take.
        assert!(!store.revoke_lease(&lease.id).unwrap());
        // And a revoked lease cannot be renewed back to life.
        let renewed = store.renew_lease(&lease.id, 600).unwrap().unwrap();
        assert!(renewed.revoked_at.is_some());
        assert_eq!(renewed.epoch, lease.epoch, "a revoked lease was renewed");
    }

    #[test]
    fn a_lease_whose_time_ran_out_is_refused_and_reported() {
        let store = TaskStore::in_memory().unwrap();
        let lease = store
            .issue_lease(NewLease {
                provider: "unzoo".into(),
                capability: "browser".into(),
                // The floor is one second, so this is the shortest lease the
                // store will issue; the sweep sees it after it lapses.
                ttl_seconds: -5,
                ..NewLease::default()
            })
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert_eq!(
            store
                .use_lease(&Presented {
                    lease_id: lease.id.clone(),
                    epoch: lease.epoch,
                    seq: 1,
                })
                .unwrap()
                .unwrap_err(),
            Refusal::Expired
        );
        assert_eq!(store.expire_leases().unwrap(), vec![lease.id]);
    }

    #[test]
    fn unbinding_a_provider_takes_back_every_key_it_handed_out() {
        let store = TaskStore::in_memory().unwrap();
        let browser = a_lease(&store);
        let profile = store
            .issue_lease(NewLease {
                provider: "unzoo".into(),
                capability: "profile".into(),
                ttl_seconds: 300,
                ..NewLease::default()
            })
            .unwrap();
        let elsewhere = store
            .issue_lease(NewLease {
                provider: "other".into(),
                capability: "browser".into(),
                ttl_seconds: 300,
                ..NewLease::default()
            })
            .unwrap();

        assert_eq!(store.revoke_provider_leases("unzoo").unwrap(), 2);
        for id in [&browser.id, &profile.id] {
            assert!(store.lease(id).unwrap().unwrap().revoked_at.is_some());
        }
        // Another provider's leases are none of this one's business.
        assert!(store.lease(&elsewhere.id).unwrap().unwrap().revoked_at.is_none());
    }

    #[test]
    fn revoking_the_permission_revokes_the_lease_it_paid_for() {
        // A lease outliving the grant that created it is a key still turning
        // in a lock the user changed.
        let store = TaskStore::in_memory().unwrap();
        let grant = store
            .create_grant(NewGrant {
                scope_or_once: Some(Scope::Always),
                method: Some("browser.navigate".into()),
                ..NewGrant::default()
            })
            .unwrap();
        let lease = store
            .issue_lease(NewLease {
                provider: "unzoo".into(),
                capability: "browser".into(),
                grant_id: Some(grant.id.clone()),
                ttl_seconds: 3600,
                ..NewLease::default()
            })
            .unwrap();

        store.revoke_grant(&grant.id).unwrap();
        assert!(store.lease(&lease.id).unwrap().unwrap().revoked_at.is_some());
        assert_eq!(
            store
                .use_lease(&Presented {
                    lease_id: lease.id,
                    epoch: 1,
                    seq: 1,
                })
                .unwrap()
                .unwrap_err(),
            Refusal::Revoked
        );
    }

    #[test]
    fn an_action_can_be_traced_back_to_the_human_who_allowed_it() {
        // M5's gate: not a log line saying it was allowed, but the records —
        // each of which can still be revoked.
        let store = TaskStore::in_memory().unwrap();
        let task = store.create_task("browse", "check the dashboard").unwrap();
        let approval = store
            .request_approval(
                "browser.navigate",
                "local_mutation",
                &Ask {
                    method: "browser.navigate".into(),
                    actor: Some("claude".into()),
                    task_id: Some(task.id.to_string()),
                    resource: None,
                    risk_rank: 1,
                },
                None,
                Some(300),
            )
            .unwrap();
        store
            .decide_approval(
                &approval.id,
                true,
                "the user",
                Some(NewGrant {
                    scope_or_once: Some(Scope::Task),
                    method: Some("browser.navigate".into()),
                    task_id: Some(task.id.to_string()),
                    ..NewGrant::default()
                }),
            )
            .unwrap();
        let grant = store.grants().unwrap().remove(0);
        let lease = store
            .issue_lease(NewLease {
                provider: "unzoo".into(),
                capability: "browser".into(),
                actor: Some("claude".into()),
                task_id: Some(task.id.to_string()),
                grant_id: Some(grant.id.clone()),
                approval_id: Some(approval.id.clone()),
                ttl_seconds: 300,
                ..NewLease::default()
            })
            .unwrap();

        let chain = store.authorisation_chain(&lease.id).unwrap().unwrap();
        assert_eq!(chain.lease.id, lease.id);
        assert_eq!(chain.grant.unwrap().id, grant.id);
        assert_eq!(chain.approval.unwrap().id, approval.id);
        assert_eq!(chain.task.unwrap().id, task.id);
    }

    #[test]
    fn a_lease_nobody_issued_cannot_be_used() {
        let store = TaskStore::in_memory().unwrap();
        assert_eq!(
            store
                .use_lease(&Presented {
                    lease_id: "lse_invented".into(),
                    epoch: 1,
                    seq: 1,
                })
                .unwrap()
                .unwrap_err(),
            Refusal::Unknown
        );
        assert!(store.authorisation_chain("lse_invented").unwrap().is_none());
    }

    #[test]
    fn leases_survive_a_restart_with_their_sequence_intact() {
        // Otherwise a restart would reset the replay window, and the oldest
        // recorded exchange would work again.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.db");
        let id = {
            let store = TaskStore::open(&path).unwrap();
            let lease = a_lease(&store);
            store
                .use_lease(&Presented {
                    lease_id: lease.id.clone(),
                    epoch: 1,
                    seq: 42,
                })
                .unwrap()
                .unwrap();
            lease.id
        };
        let store = TaskStore::open(&path).unwrap();
        assert_eq!(store.lease(&id).unwrap().unwrap().last_seq, 42);
        assert_eq!(
            store
                .use_lease(&Presented {
                    lease_id: id,
                    epoch: 1,
                    seq: 42,
                })
                .unwrap()
                .unwrap_err(),
            Refusal::Replay
        );
    }
}
