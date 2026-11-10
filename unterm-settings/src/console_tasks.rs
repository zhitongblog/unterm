//! The console's tasks, owned by the runtime.
//!
//! The Unzoo One console started out keeping its tasks in the browser and
//! driving each agent run from the page: close the tab and the run had nobody
//! following it, open another browser and the tasks were not there, and a
//! task set to run every morning only ran if a page happened to be open at
//! nine. All three are the runtime's job, so they live here:
//!
//! - **Tasks** are rows in the task engine (`kind = "console"`). The console
//!   names them (local first), and its own view of a task rides along as an
//!   opaque `console` blob the runtime never interprets. What the runtime does
//!   read -- the prompt, the schedule, whether it is paused -- is sent as its
//!   own fields, so nothing here depends on the console's wording.
//! - **Runs** are engine runs, each with one `agent_turn` step that carries
//!   the session, the agent's answer and the token count. The runtime starts
//!   the agent, follows it to the end of its turn and writes the verdict; the
//!   console only reads.
//! - **Schedules** are checked every half minute. A daily or weekly task whose
//!   slot has come since it last ran is run once -- a slot missed while the
//!   machine was off is caught up once, not once per missed day.
//!
//! A run is always hosted: every tool call the agent's CLI would ask about
//! comes to the console's inbox, whether or not anyone is looking at a page.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Datelike, NaiveTime, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use unterm_mcp::handler::McpHandler;
use unterm_services::agent_session;
use unterm_tasks::{Run, RunId, State, Step, StepId, Task, TaskId, TaskStore};

use super::server::Response;

const KIND: &str = "console";
const STEP_KIND: &str = "agent_turn";
/// A turn that has produced nothing -- no output, no question waiting on a
/// person -- for this long is stuck, not thinking.
const IDLE_LIMIT: Duration = Duration::from_secs(10 * 60);
const POLL: Duration = Duration::from_millis(700);

// ---- run settings --------------------------------------------------------

/// How the runtime runs a task: which agent, in which directory, governed or
/// not. One set for the whole console -- the scheduler needs them with no
/// page open, so they live here and not in a browser.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunSettings {
    pub agent: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default = "yes")]
    pub governed: bool,
}

fn yes() -> bool {
    true
}

impl Default for RunSettings {
    fn default() -> Self {
        Self { agent: "claude-code".into(), cwd: String::new(), governed: true }
    }
}

fn settings_path() -> Result<std::path::PathBuf> {
    unterm_protocol::state_path("console_run.json").context("could not resolve the state directory")
}

pub fn run_settings() -> RunSettings {
    settings_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Check and keep new settings. An agent the console cannot drive hosted,
/// or a directory that is not there, is refused here rather than at nine in
/// the morning when the schedule fires.
pub fn set_run_settings(next: RunSettings) -> Result<RunSettings> {
    let next = RunSettings { agent: next.agent.trim().into(), cwd: next.cwd.trim().into(), ..next };
    if !super::agent_run::supports_hosted(&next.agent) {
        bail!("{} cannot be run hosted, so its tool calls could not reach the inbox", next.agent);
    }
    if !next.cwd.is_empty() && !std::path::Path::new(&next.cwd).is_dir() {
        bail!("not a directory: {}", next.cwd);
    }
    let path = settings_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(&next)?)?;
    std::fs::rename(&temp, &path)?;
    Ok(next)
}

// ---- schedules -----------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Schedule {
    Daily { time: String },
    /// `weekday` counts from Monday = 1 to Sunday = 7.
    Weekly { time: String, weekday: u32 },
}

/// The most recent moment at or before `now` this schedule asked for a run.
pub fn last_slot<Tz: TimeZone>(schedule: &Schedule, now: &DateTime<Tz>) -> Option<DateTime<Tz>> {
    let (time, weekday) = match schedule {
        Schedule::Daily { time } => (time, None),
        Schedule::Weekly { time, weekday } => (time, Some(*weekday)),
    };
    let time = NaiveTime::parse_from_str(time.trim(), "%H:%M").ok()?;
    let zone = now.timezone();
    let mut day = now.date_naive();
    for _ in 0..8 {
        if weekday.map_or(true, |wanted| day.weekday().number_from_monday() == wanted) {
            // A time that does not exist that day (a DST gap) is skipped; one
            // that exists twice is taken the first time.
            if let Some(at) = zone.from_local_datetime(&day.and_time(time)).earliest() {
                if at <= *now {
                    return Some(at);
                }
            }
        }
        day = day.pred_opt()?;
    }
    None
}

/// Whether a scheduled task is due: its last slot came after it was made
/// and after its last run started.
pub fn is_due<Tz: TimeZone>(
    schedule: &Schedule,
    now: &DateTime<Tz>,
    created_at: &str,
    last_started_at: Option<&str>,
) -> bool {
    let Some(slot) = last_slot(schedule, now) else { return false };
    let parse = |text: &str| DateTime::parse_from_rfc3339(text).ok();
    let Some(created) = parse(created_at) else { return false };
    if slot <= created {
        return false;
    }
    match last_started_at.and_then(parse) {
        Some(started) => started < slot,
        None => true,
    }
}

// ---- ids -----------------------------------------------------------------

/// The console names tasks and runs itself (`task-…`, `run-…`); the engine's
/// ids carry a kind prefix. The console's id is kept as is, behind it.
fn task_key(console_id: &str) -> Result<TaskId> {
    if console_id.is_empty() || console_id.len() > 200 {
        bail!("a task id is 1 to 200 characters");
    }
    TaskId::parse(&format!("tsk_{}", console_id.trim_start_matches("tsk_")))
}

fn run_key(console_id: &str) -> Result<RunId> {
    if console_id.is_empty() || console_id.len() > 200 {
        bail!("a run id is 1 to 200 characters");
    }
    RunId::parse(&format!("run_{}", console_id.trim_start_matches("run_")))
}

fn console_id_of(task: &Task) -> String {
    task.detail
        .get("console_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| task.id.as_str().trim_start_matches("tsk_").to_string())
}

fn store() -> Result<Arc<TaskStore>> {
    unterm_services::cockpit::fleet_store::tasks().ok_or_else(|| anyhow!("there is no task store"))
}

fn console_task(store: &TaskStore, console_id: &str) -> Result<Task> {
    let task = store
        .task(&task_key(console_id)?)?
        .ok_or_else(|| anyhow!("no task {console_id}"))?;
    if task.kind != KIND {
        bail!("{console_id} is not a console task");
    }
    Ok(task)
}

/// Runs the runtime is following in this process. A run the engine says is
/// running and that is not in here has nobody following it -- its process
/// died with the last Unterm -- and is closed as interrupted.
fn following() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Runs a person asked to stop. However the agent's CLI then reports its
/// end -- an interrupted turn, or a process that "exited in the middle of a
/// turn" -- the run was cancelled, not failed.
fn stopping() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

// ---- the views the console reads -----------------------------------------

fn run_view(run: &Run, step: Option<&Step>) -> Value {
    let detail = step.map(|s| s.detail.clone()).unwrap_or_else(|| json!({}));
    let field = |key: &str| detail.get(key).cloned().unwrap_or(Value::Null);
    json!({
        "id": detail.get("console_run_id").and_then(Value::as_str)
            .unwrap_or_else(|| run.id.as_str().trim_start_matches("run_")),
        "attempt": run.attempt,
        "state": run.state.as_str(),
        "started_at": run.started_at,
        "ended_at": run.ended_at,
        "trigger": field("trigger"),
        "agent": field("agent"),
        "adapter": field("adapter"),
        "session_id": field("session_id"),
        "summary": field("summary"),
        "error": field("error"),
        "error_code": field("error_code"),
        "waiting": detail.get("waiting").and_then(Value::as_bool).unwrap_or(false),
        "usage": field("usage"),
    })
}

fn task_view(store: &TaskStore, task: &Task) -> Result<Value> {
    let mut runs = store.runs(&task.id)?;
    runs.reverse();
    runs.truncate(20);
    let mut views = Vec::with_capacity(runs.len());
    for run in &runs {
        let steps = store.steps(&run.id)?;
        views.push(run_view(run, steps.iter().find(|s| s.kind == STEP_KIND)));
    }
    let detail = &task.detail;
    Ok(json!({
        "id": console_id_of(task),
        "title": task.title,
        "created_at": task.created_at,
        "updated_at": task.updated_at,
        "prompt": detail.get("prompt").cloned().unwrap_or(Value::Null),
        "schedule": detail.get("schedule").cloned().unwrap_or(Value::Null),
        "paused": detail.get("paused").and_then(Value::as_bool).unwrap_or(false),
        "console": detail.get("console").cloned().unwrap_or(Value::Null),
        "runs": views,
    }))
}

pub fn list() -> Result<Vec<Value>> {
    let store = store()?;
    let mut tasks: Vec<Task> = store.tasks()?.into_iter().filter(|t| t.kind == KIND).collect();
    tasks.reverse();
    tasks.iter().map(|task| task_view(&store, task)).collect()
}

// ---- writes --------------------------------------------------------------

/// What the console sends for one task. `console` is its own view of the
/// task, kept and handed back untouched.
#[derive(Debug, Deserialize)]
pub struct Put {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub schedule: Option<Schedule>,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub console: Value,
}

pub fn put(put: Put) -> Result<Value> {
    let title = put.title.trim();
    if title.is_empty() {
        bail!("a task needs a title");
    }
    let key = task_key(&put.id)?;
    let detail = json!({
        "console_id": put.id,
        "prompt": put.prompt.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()),
        "schedule": put.schedule,
        "paused": put.paused,
        "console": put.console,
    });
    let store = store()?;
    match store.task(&key)? {
        Some(existing) if existing.kind != KIND => bail!("{} is not a console task", put.id),
        Some(_) => {
            store.set_task_title(&key, title)?;
            store.set_task_detail(&key, detail)?;
        }
        None => {
            store.create_task_as(key.clone(), KIND, title, detail)?;
        }
    }
    let task = store.task(&key)?.ok_or_else(|| anyhow!("the task vanished as it was written"))?;
    task_view(&store, &task)
}

pub fn delete(console_id: &str) -> Result<()> {
    let store = store()?;
    let task = console_task(&store, console_id)?;
    // A task going away takes its agent with it.
    let _ = stop(console_id);
    store.delete_task(&task.id)
}

// ---- runs ----------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct RunRequest {
    pub task_id: String,
    /// The console's id for the run, when it already shows one.
    #[serde(default)]
    pub run_id: Option<String>,
    /// What to say this time; the task's prompt when absent.
    #[serde(default)]
    pub prompt: Option<String>,
    /// A session whose conversation this run continues.
    #[serde(default)]
    pub continue_from: Option<String>,
}

fn running_run(store: &TaskStore, task: &Task) -> Result<Option<(Run, Option<Step>)>> {
    for run in store.runs(&task.id)? {
        if run.state == State::Running {
            let step = store.steps(&run.id)?.into_iter().find(|s| s.kind == STEP_KIND);
            return Ok(Some((run, step)));
        }
    }
    Ok(None)
}

/// Close a run with a verdict: the step's detail and state, then the run.
fn finish(store: &TaskStore, task: &TaskId, run: &RunId, step: &StepId, detail: Value, state: State) {
    if let Err(error) = store.set_step_detail(step, detail, state) {
        log::warn!("console run {run}: could not write the step: {error:#}");
    }
    let current = store.runs(task).ok().and_then(|runs| runs.into_iter().find(|r| r.id == *run));
    if let Some(current) = current {
        if current.state == State::Running {
            if let Err(error) = store.finish_run(run, state, current.version) {
                log::warn!("console run {run}: could not close it: {error:#}");
            }
        }
    }
    following().lock().unwrap().remove(run.as_str());
}

/// Start a run of a task: the agent in the run settings, hosted, followed by
/// a thread of the runtime's own until its turn ends.
pub fn run(handler: &McpHandler, request: RunRequest, trigger: &str) -> Result<Value> {
    let store = store()?;
    let task = console_task(&store, &request.task_id)?;
    if running_run(&store, &task)?.is_some() {
        bail!("{} is already running", request.task_id);
    }
    let prompt = request
        .prompt
        .clone()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .or_else(|| task.detail.get("prompt").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| task.title.clone());
    let settings = run_settings();
    let console_run_id = request
        .run_id
        .clone()
        .unwrap_or_else(|| format!("run-{}-{}", trigger, uuid::Uuid::new_v4().simple()));
    let run_id = run_key(&console_run_id)?;
    let step_id = format!("{console_run_id}-step-1");
    let console_task_id = console_id_of(&task);

    following().lock().unwrap().insert(run_id.as_str().to_string());
    let run = match store.start_run_as(&task.id, run_id.clone()) {
        Ok(run) => run,
        Err(error) => {
            following().lock().unwrap().remove(run_id.as_str());
            return Err(error);
        }
    };
    let mut detail = json!({
        "console_run_id": console_run_id,
        "trigger": trigger,
        "agent": settings.agent,
        "prompt": prompt,
    });
    let step = store.request_step_with_detail(&run.id, STEP_KIND, None, detail.clone())?.step().clone();
    store.set_step_detail(&step.id, detail.clone(), State::Running)?;

    let started = match &request.continue_from {
        Some(session) => {
            let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
            handler
                .handle(
                    &ctx,
                    "agent_session.resume",
                    &json!({
                        "session_id": session,
                        "prompt": prompt,
                        "task_id": console_task_id,
                        "run_id": console_run_id,
                        "step_id": step_id,
                    }),
                )
                .map_err(|e| e.to_string())
        }
        None => {
            let governed = settings.governed && super::agent_run::supports_governed(&settings.agent);
            let mut body = json!({
                "agent": settings.agent,
                "prompt": prompt,
                "hosted": true,
                "governed": governed,
                "task_id": console_task_id,
                "run_id": console_run_id,
                "step_id": step_id,
            });
            if !settings.cwd.is_empty() {
                body["cwd"] = json!(settings.cwd);
            }
            super::agent_run::start_session(handler, &body).map_err(|(_, _, message)| message)
        }
    };
    let started = match started {
        Ok(value) => value,
        Err(message) => {
            detail["error"] = json!(message);
            detail["error_code"] = json!("start_failed");
            finish(&store, &task.id, &run.id, &step.id, detail, State::Failed);
            let run = store.runs(&task.id)?.into_iter().find(|r| r.id == run.id).unwrap_or(run);
            return Ok(run_view(&run, store.step(&step.id)?.as_ref()));
        }
    };
    let session_id = started
        .pointer("/session/session_id")
        .or_else(|| started.get("session_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the agent started but no session id came back"))?
        .to_string();
    detail["session_id"] = json!(session_id);
    if let Some(adapter) = started.get("adapter").and_then(Value::as_str) {
        detail["adapter"] = json!(adapter);
    }
    store.set_step_detail(&step.id, detail.clone(), State::Running)?;

    let (follow_task, follow_run, follow_step) = (task.id.clone(), run.id.clone(), step.id.clone());
    std::thread::Builder::new()
        .name("console-run".into())
        .spawn(move || follow(follow_task, follow_run, follow_step, session_id, detail))
        .context("could not start following the run")?;

    let run = store.runs(&task.id)?.into_iter().find(|r| r.id == run.id).unwrap_or(run);
    Ok(run_view(&run, store.step(&step.id)?.as_ref()))
}

/// What one pass over a session's new events learned.
#[derive(Default)]
struct Turn {
    text: String,
    error: Option<String>,
    /// Why the runtime itself ended the turn, for the console to say in the
    /// reader's language; `None` when the agent reported its own error.
    error_code: Option<&'static str>,
    waiting: bool,
    ended: Option<String>,
}

impl Turn {
    fn read(&mut self, event: &Value) {
        match event.get("event").and_then(Value::as_str).unwrap_or_default() {
            "output.delta" if event.get("stream").and_then(Value::as_str) != Some("reasoning") => {
                if let Some(text) = event.get("text").and_then(Value::as_str) {
                    self.text.push_str(text);
                }
            }
            "error" => {
                self.error = event
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or(self.error.take());
            }
            "approval.requested" => self.waiting = true,
            "approval.resolved" => self.waiting = false,
            "turn.completed" | "session.closed" => {
                self.ended = Some(event.get("state").and_then(Value::as_str).unwrap_or("finished").to_string());
            }
            _ => {}
        }
    }

    /// The agent's last words, as the run's summary.
    fn summary(&self) -> String {
        let text = self.text.trim();
        let chars: Vec<char> = text.chars().collect();
        if chars.len() <= 600 {
            text.to_string()
        } else {
            format!("…{}", chars[chars.len() - 600..].iter().collect::<String>())
        }
    }
}

/// How a session's ending reads as a run's verdict.
fn verdict(ended: &str, stopped_for_idling: bool) -> State {
    match ended {
        _ if stopped_for_idling => State::Failed,
        "finished" | "exited" | "succeeded" => State::Succeeded,
        "interrupted" | "cancelled" => State::Cancelled,
        _ => State::Failed,
    }
}

fn follow(task: TaskId, run: RunId, step: StepId, session: String, mut detail: Value) {
    let started = Instant::now();
    let mut last_progress = Instant::now();
    let mut cursor = 0usize;
    let mut turn = Turn::default();
    let mut shown_waiting = false;
    let mut shown_len = 0usize;
    let mut idled: Option<Instant> = None;

    loop {
        std::thread::sleep(POLL);
        match agent_session::events(&session, cursor) {
            Ok((events, next)) => {
                if !events.is_empty() {
                    last_progress = Instant::now();
                }
                cursor = next;
                for event in &events {
                    turn.read(event);
                }
            }
            Err(_) => {
                // Gone from memory: closed elsewhere, or never live. Its
                // record still says how it ended.
                let state = agent_session::status(&session)
                    .ok()
                    .and_then(|v| v.get("state").and_then(Value::as_str).map(str::to_string));
                turn.ended = Some(match state.as_deref() {
                    Some("running") | None => {
                        turn.error.get_or_insert_with(|| "the agent session disappeared".into());
                        turn.error_code.get_or_insert("session_lost");
                        "failed".into()
                    }
                    Some(state) => state.to_string(),
                });
            }
        }
        if turn.ended.is_some() {
            break;
        }

        let status = agent_session::status(&session).ok();
        let pending = status
            .as_ref()
            .and_then(|s| s.get("pending_approvals"))
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty());
        if pending {
            // Waiting for a person is not being stuck.
            last_progress = Instant::now();
        }
        let waiting = pending || turn.waiting;
        let text_len = turn.text.len();
        if waiting != shown_waiting || text_len >= shown_len + 200 {
            shown_waiting = waiting;
            shown_len = text_len;
            detail["waiting"] = json!(waiting);
            detail["summary"] = json!(turn.summary());
            if let Ok(store) = store() {
                let _ = store.set_step_detail(&step, detail.clone(), State::Running);
            }
        }

        match idled {
            None if last_progress.elapsed() > IDLE_LIMIT => {
                idled = Some(Instant::now());
                turn.error = Some(format!("no progress for {} minutes; stopped", IDLE_LIMIT.as_secs() / 60));
                turn.error_code = Some("idle_timeout");
                let _ = agent_session::interrupt(&session, 2_000);
            }
            Some(at) if at.elapsed() > Duration::from_secs(30) => {
                turn.ended = Some("failed".into());
                break;
            }
            _ => {}
        }
    }

    let usage = agent_session::status(&session).ok().and_then(|s| s.get("usage").cloned());
    // A hosted agent outlives its turn, waiting for the next message. This
    // run is over; the conversation is kept by the CLI and continued, when
    // asked, in a new session.
    let _ = agent_session::close(&session);

    let ended = turn.ended.clone().unwrap_or_else(|| "failed".into());
    let asked_to_stop = stopping().lock().unwrap().remove(run.as_str());
    let state = if asked_to_stop && idled.is_none() {
        State::Cancelled
    } else {
        verdict(&ended, idled.is_some())
    };
    detail["waiting"] = json!(false);
    detail["summary"] = json!(turn.summary());
    detail["ended"] = json!(ended);
    detail["duration_seconds"] = json!(started.elapsed().as_secs());
    if let Some(usage) = usage.filter(|u| !u.is_null()) {
        detail["usage"] = usage;
    }
    if state == State::Failed {
        if let Some(error) = &turn.error {
            detail["error"] = json!(error);
            if let Some(code) = turn.error_code {
                detail["error_code"] = json!(code);
            }
        }
    }
    match store() {
        Ok(store) => finish(&store, &task, &run, &step, detail, state),
        Err(error) => log::warn!("console run {run}: no store to write the verdict to: {error:#}"),
    }
}

/// Stop the task's running run, if it has one: its agent is interrupted and
/// the run ends as cancelled when the turn does.
pub fn stop(console_id: &str) -> Result<bool> {
    let store = store()?;
    let task = console_task(&store, console_id)?;
    let Some((run, step)) = running_run(&store, &task)? else { return Ok(false) };
    let session = step
        .as_ref()
        .and_then(|s| s.detail.get("session_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the run has not started its agent yet"))?;
    stopping().lock().unwrap().insert(run.id.as_str().to_string());
    agent_session::interrupt(session, 2_000)?;
    Ok(true)
}

/// Close every console run the engine thinks is running that nobody here is
/// following: its agent died with an earlier Unterm.
pub fn sweep_orphans() -> Result<usize> {
    let store = store()?;
    let followed = following().lock().unwrap().clone();
    let mut closed = 0;
    for task in store.tasks()?.into_iter().filter(|t| t.kind == KIND) {
        for run in store.runs(&task.id)? {
            if run.state != State::Running || followed.contains(run.id.as_str()) {
                continue;
            }
            let step = store.steps(&run.id)?.into_iter().find(|s| s.kind == STEP_KIND);
            if let Some(step) = step {
                let mut detail = step.detail.clone();
                detail["waiting"] = json!(false);
                detail["error"] = json!("the background service stopped before this run finished");
                detail["error_code"] = json!("service_restarted");
                let _ = store.set_step_detail(&step.id, detail, State::Interrupted);
            }
            store.finish_run(&run.id, State::Interrupted, run.version)?;
            closed += 1;
        }
    }
    Ok(closed)
}

/// One pass of the scheduler: close orphans, then run what is due.
pub fn tick(handler: &McpHandler) -> Result<Vec<String>> {
    sweep_orphans()?;
    let store = store()?;
    let now = chrono::Local::now();
    let mut started = Vec::new();
    for task in store.tasks()?.into_iter().filter(|t| t.kind == KIND) {
        if task.detail.get("paused").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let Some(schedule) = task
            .detail
            .get("schedule")
            .cloned()
            .and_then(|s| serde_json::from_value::<Schedule>(s).ok())
        else {
            continue;
        };
        let runs = store.runs(&task.id)?;
        if runs.iter().any(|r| r.state == State::Running) {
            continue;
        }
        let last = runs.iter().map(|r| r.started_at.as_str()).max();
        if !is_due(&schedule, &now, &task.created_at, last) {
            continue;
        }
        let id = console_id_of(&task);
        match run(handler, RunRequest { task_id: id.clone(), ..RunRequest::default() }, "schedule") {
            Ok(_) => started.push(id),
            Err(error) => log::warn!("scheduled run of {id} did not start: {error:#}"),
        }
    }
    Ok(started)
}

/// Start the scheduler. Once per process.
pub fn start_scheduler(handler: Arc<McpHandler>) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new().name("console-scheduler".into()).spawn(move || loop {
        if let Err(error) = tick(&handler) {
            log::warn!("console scheduler: {error:#}");
        }
        std::thread::sleep(Duration::from_secs(30));
    });
    if let Err(error) = spawned {
        log::error!("console scheduler did not start: {error}");
    }
}

// ---- REST ----------------------------------------------------------------

fn body_json(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

fn id_field(body: &Value, key: &str) -> Option<String> {
    body.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

/// GET /api/console/tasks
pub fn api_list() -> Response {
    match list() {
        Ok(tasks) => Response::ok_json(json!({"tasks": tasks})),
        Err(e) => Response::err(500, "Internal Server Error", &format!("{e:#}")),
    }
}

/// POST /api/console/tasks/put — `{id, title, prompt?, schedule?, paused?, console?}`
pub fn api_put(body: &[u8]) -> Response {
    let request: Put = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(e) => return Response::err(400, "Bad Request", &format!("a task needs id and title: {e}")),
    };
    match put(request) {
        Ok(task) => Response::ok_json(json!({"task": task})),
        Err(e) => Response::err(400, "Bad Request", &format!("{e:#}")),
    }
}

/// POST /api/console/tasks/delete — `{id}`
pub fn api_delete(body: &[u8]) -> Response {
    let Some(id) = id_field(&body_json(body), "id") else {
        return Response::err(400, "Bad Request", "id is required");
    };
    match delete(&id) {
        Ok(()) => Response::ok_json(json!({"deleted": id})),
        Err(e) => Response::err(404, "Not Found", &format!("{e:#}")),
    }
}

/// POST /api/console/tasks/run — `{task_id, run_id?, prompt?, continue_from?}`
pub fn api_run(handler: &McpHandler, body: &[u8]) -> Response {
    let request: RunRequest = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(e) => return Response::err(400, "Bad Request", &format!("task_id is required: {e}")),
    };
    let trigger = if request.continue_from.is_some() { "continue" } else { "manual" };
    match run(handler, request, trigger) {
        Ok(run) => Response::ok_json(json!({"run": run})),
        Err(e) => Response::err(409, "Conflict", &format!("{e:#}")),
    }
}

/// POST /api/console/tasks/stop — `{task_id}`
pub fn api_stop(body: &[u8]) -> Response {
    let Some(id) = id_field(&body_json(body), "task_id") else {
        return Response::err(400, "Bad Request", "task_id is required");
    };
    match stop(&id) {
        Ok(stopped) => Response::ok_json(json!({"stopped": stopped})),
        Err(e) => Response::err(409, "Conflict", &format!("{e:#}")),
    }
}

/// GET /api/console/run-settings
pub fn api_settings_get() -> Response {
    Response::ok_json(json!({"settings": run_settings()}))
}

/// POST /api/console/run-settings — `{agent, cwd, governed}`
pub fn api_settings_set(body: &[u8]) -> Response {
    let next: RunSettings = match serde_json::from_slice(body) {
        Ok(next) => next,
        Err(e) => return Response::err(400, "Bad Request", &format!("settings need an agent: {e}")),
    };
    match set_run_settings(next) {
        Ok(settings) => Response::ok_json(json!({"settings": settings})),
        Err(e) => Response::err(400, "Bad Request", &format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn at(text: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(text).unwrap()
    }

    #[test]
    fn a_daily_slot_is_today_once_its_time_has_come_and_yesterday_before() {
        let daily = Schedule::Daily { time: "09:00".into() };
        assert_eq!(last_slot(&daily, &at("2026-11-10T10:00:00+08:00")), Some(at("2026-11-10T09:00:00+08:00")));
        assert_eq!(last_slot(&daily, &at("2026-11-10T08:59:00+08:00")), Some(at("2026-11-09T09:00:00+08:00")));
        assert_eq!(last_slot(&Schedule::Daily { time: "9am".into() }, &at("2026-11-10T10:00:00+08:00")), None);
    }

    #[test]
    fn a_weekly_slot_is_the_last_such_weekday() {
        // 2026-11-10 is a Tuesday.
        let monday = Schedule::Weekly { time: "10:00".into(), weekday: 1 };
        assert_eq!(last_slot(&monday, &at("2026-11-10T12:00:00+08:00")), Some(at("2026-11-09T10:00:00+08:00")));
        let tuesday = Schedule::Weekly { time: "10:00".into(), weekday: 2 };
        assert_eq!(last_slot(&tuesday, &at("2026-11-10T09:00:00+08:00")), Some(at("2026-11-03T10:00:00+08:00")));
        assert_eq!(last_slot(&tuesday, &at("2026-11-10T10:00:00+08:00")), Some(at("2026-11-10T10:00:00+08:00")));
    }

    #[test]
    fn a_task_is_due_once_per_slot_and_never_for_a_slot_before_it_existed() {
        let daily = Schedule::Daily { time: "09:00".into() };
        let now = at("2026-11-10T09:30:00+08:00");
        // Made after this morning's slot: wait for tomorrow.
        assert!(!is_due(&daily, &now, "2026-11-10T01:15:00Z", None));
        // Made yesterday, never ran: due.
        assert!(is_due(&daily, &now, "2026-11-09T03:00:00Z", None));
        // Ran at the slot (01:00Z is 09:00 +08:00): not again.
        assert!(!is_due(&daily, &now, "2026-11-09T03:00:00Z", Some("2026-11-10T01:00:05Z")));
        // Last ran yesterday: due -- and only once, however many days were missed.
        assert!(is_due(&daily, &now, "2026-11-01T03:00:00Z", Some("2026-11-03T01:00:05Z")));
    }

    #[test]
    fn the_console_names_and_the_engine_keys_round_trip() {
        assert_eq!(task_key("task-171").unwrap().as_str(), "tsk_task-171");
        assert_eq!(task_key("tsk_abc").unwrap().as_str(), "tsk_abc", "an engine id is not prefixed twice");
        assert_eq!(run_key("run-task-171-1").unwrap().as_str(), "run_run-task-171-1");
        assert!(task_key("").is_err());
    }

    #[test]
    fn a_turn_reads_its_answer_and_ending_from_the_contracts_events() {
        let mut turn = Turn::default();
        for event in [
            json!({"event": "output.delta", "stream": "reasoning", "text": "thinking"}),
            json!({"event": "output.delta", "stream": "assistant", "text": "Done: "}),
            json!({"event": "approval.requested"}),
            json!({"event": "approval.resolved"}),
            json!({"event": "output.delta", "stream": "assistant", "text": "notes.txt written."}),
            json!({"event": "turn.completed", "state": "finished"}),
        ] {
            turn.read(&event);
        }
        assert_eq!(turn.summary(), "Done: notes.txt written.");
        assert!(!turn.waiting);
        assert_eq!(turn.ended.as_deref(), Some("finished"));
        assert_eq!(verdict("finished", false), State::Succeeded);
        assert_eq!(verdict("exited", false), State::Succeeded);
        assert_eq!(verdict("interrupted", false), State::Cancelled);
        assert_eq!(verdict("interrupted", true), State::Failed, "stopped for idling is a failure");
        assert_eq!(verdict("failed", false), State::Failed);
    }

    #[test]
    fn tasks_are_kept_and_an_orphaned_run_is_closed_as_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("UNTERM_STATE_DIR", dir.path());
        std::env::set_var("UNTERM_TASKS_DB", dir.path().join("tasks.db"));
        unterm_services::cockpit::fleet_store::reset_for_tests();

        let made = put(Put {
            id: "task-1".into(),
            title: "weekly report".into(),
            prompt: Some("write it".into()),
            schedule: Some(Schedule::Weekly { time: "09:00".into(), weekday: 5 }),
            paused: false,
            console: json!({"status": "待确认", "target": "repo"}),
        })
        .unwrap();
        assert_eq!(made["id"], "task-1");
        assert_eq!(made["console"]["target"], "repo", "the console's view comes back untouched");

        // Editing keeps the id; the engine has one task, not two.
        put(Put { id: "task-1".into(), title: "monthly report".into(), prompt: None, schedule: None, paused: true, console: json!({}) })
            .unwrap();
        let tasks = list().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["title"], "monthly report");
        assert_eq!(tasks[0]["paused"], true);
        assert!(tasks[0]["schedule"].is_null());

        // A run left running by a process that is gone.
        let store = store().unwrap();
        let task = console_task(&store, "task-1").unwrap();
        let run = store.start_run_as(&task.id, run_key("run-old").unwrap()).unwrap();
        let step = store.request_step_with_detail(&run.id, STEP_KIND, None, json!({"console_run_id": "run-old"})).unwrap().step().clone();
        store.set_step_detail(&step.id, json!({"console_run_id": "run-old", "session_id": "ags_gone"}), State::Running).unwrap();
        assert_eq!(sweep_orphans().unwrap(), 1);
        let runs = list().unwrap()[0]["runs"].clone();
        assert_eq!(runs[0]["id"], "run-old");
        assert_eq!(runs[0]["state"], "interrupted");
        assert!(runs[0]["error"].as_str().unwrap().contains("stopped"));
        assert_eq!(runs[0]["error_code"], "service_restarted");
        assert_eq!(sweep_orphans().unwrap(), 0);

        delete("task-1").unwrap();
        assert!(list().unwrap().is_empty());
        assert!(delete("task-1").is_err());

        // Settings refuse what a scheduled run could not use.
        assert!(set_run_settings(RunSettings { agent: "aider".into(), ..RunSettings::default() }).is_err());
        assert!(set_run_settings(RunSettings { cwd: "Z:\\no\\such\\dir".into(), ..RunSettings::default() }).is_err());
        let kept = set_run_settings(RunSettings {
            agent: "codex-cli".into(),
            cwd: dir.path().display().to_string(),
            governed: false,
        })
        .unwrap();
        assert_eq!(run_settings(), kept);
    }
}
