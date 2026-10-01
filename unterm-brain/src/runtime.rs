//! Running a brain: spawn it, read it, stop it, remember what it did.
//!
//! The adapters below this are pure parsers. This is where the side effects
//! live, and the split is deliberate: everything here is about a process —
//! its group, its pipes, its death — and nothing here interprets a stream.
//!
//! Three things are harder than they look, and the shape of this module is
//! mostly the consequence of them:
//!
//! **Interrupting means the whole tree.** An agent CLI is a parent of shells,
//! which are parents of whatever the model asked for. Signalling only the
//! child leaves a build running with nobody watching it. So the child is put
//! in its own process group and the signal goes to the group.
//!
//! **Stopping politely has to have a deadline.** A CLI that ignores SIGINT
//! must still stop, or "interrupt" is a suggestion. Grace, then SIGKILL.
//!
//! **A dead brain must not take a task with it.** The runtime holds a lease
//! on the step it is working, renewed while the process lives. Kill the
//! runtime and the lease simply lapses; the next `reconcile()` returns the
//! step to the queue. Nothing has to run at the moment of death for the work
//! to survive it, which is the only way crash recovery is ever true.

use crate::{BrainAdapter, BrainEvent, StopReason, ThreadId, Usage};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How to start a brain.
///
/// The CLI-shaped knowledge — which flag resumes, which argument is the
/// prompt — lives here rather than in the adapter, because an adapter that
/// knew how to launch something would no longer be a parser and the
/// equivalence test would no longer mean anything.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// Written to the child's stdin, which is then closed. Passing a prompt
    /// this way rather than on the command line keeps it out of `ps` — and
    /// out of any shell history that recorded the launch.
    pub prompt: Option<String>,
    /// The flag that continues an existing conversation, when the CLI has one.
    pub resume_flag: Option<String>,
}

impl Spec {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            prompt: None,
            resume_flag: None,
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I: IntoIterator<Item = S>, S: Into<String>>(mut self, args: I) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    pub fn resume_flag(mut self, flag: impl Into<String>) -> Self {
        self.resume_flag = Some(flag.into());
        self
    }

    /// Continue a conversation the CLI already has.
    ///
    /// Returns `None` when there is nothing to continue with — no external id,
    /// or a CLI with no resume flag. Refusing is the point: a resume that
    /// silently starts a fresh conversation loses the context the caller asked
    /// to keep, and looks like it worked.
    pub fn resuming(mut self, external_id: Option<&str>) -> Option<Self> {
        let flag = self.resume_flag.clone()?;
        let id = external_id?;
        if id.is_empty() {
            return None;
        }
        self.args.push(flag);
        self.args.push(id.to_string());
        Some(self)
    }
}

/// What running a brain produced, and what it would take to continue it.
///
/// Written down as the stream is read, not at the end, so a process killed
/// mid-turn still leaves a usable account of itself.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    pub thread: Option<ThreadId>,
    pub adapter: String,
    /// The CLI's own session id, which is what `--resume` wants. Absent means
    /// this conversation cannot be continued.
    pub external_id: Option<String>,
    pub model: Option<String>,
    /// Summed over every turn. Cached input stays in its own column here for
    /// the same reason it does in [`Usage`]: the two are not priced alike.
    pub usage: Usage,
    /// Whether the stream reported any usage at all. Without it, `usage`
    /// reads as "cost nothing" when the truth is "nobody said".
    #[serde(default)]
    pub usage_reported: bool,
    pub turns: u64,
    pub last_stop: Option<StopReason>,
    /// The tail of stderr, kept bounded. Agent CLIs write progress there, so
    /// this is a diagnosis aid, not a log.
    pub stderr_tail: Vec<String>,
    pub exit_code: Option<i32>,
    /// Set when the stop came from us rather than from the model.
    pub interrupted: bool,
}

impl Snapshot {
    /// Whether this conversation can be picked up again.
    pub fn resumable(&self) -> bool {
        self.external_id.is_some()
            && !matches!(self.last_stop, Some(StopReason::Error))
    }
}

/// How many stderr lines to keep. Enough to see a stack trace or a rate-limit
/// message, not enough for a runaway process to fill memory.
const STDERR_TAIL: usize = 40;

/// A brain that is running.
pub struct Running {
    child: Arc<Mutex<Child>>,
    pid: u32,
    events: Option<Receiver<BrainEvent>>,
    snapshot: Arc<Mutex<Snapshot>>,
    interrupted: Arc<AtomicBool>,
    readers: Vec<std::thread::JoinHandle<()>>,
}

/// Start a brain and begin reading it.
pub fn spawn(spec: &Spec, mut adapter: Box<dyn BrainAdapter>) -> anyhow::Result<Running> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    // Its own process group, so interrupting reaches the shells and builds the
    // agent started rather than only the agent itself.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command.spawn()?;
    let pid = child.id();

    if let Some(prompt) = &spec.prompt {
        // Closing stdin afterwards is what tells a one-shot CLI the prompt is
        // complete; leaving it open makes the process wait forever.
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(prompt.as_bytes());
            let _ = stdin.write_all(b"\n");
        }
    } else {
        drop(child.stdin.take());
    }

    let snapshot = Arc::new(Mutex::new(Snapshot {
        adapter: adapter.id().to_string(),
        ..Snapshot::default()
    }));
    let (sender, receiver) = mpsc::channel();
    let mut readers = Vec::new();

    if let Some(stderr) = child.stderr.take() {
        let snapshot = Arc::clone(&snapshot);
        readers.push(std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut snapshot = snapshot.lock().unwrap();
                if snapshot.stderr_tail.len() == STDERR_TAIL {
                    snapshot.stderr_tail.remove(0);
                }
                snapshot.stderr_tail.push(line);
            }
        }));
    }

    if let Some(stdout) = child.stdout.take() {
        let snapshot = Arc::clone(&snapshot);
        readers.push(std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                for event in adapter.on_line(&line) {
                    record(&snapshot, &event, adapter.external_id());
                    if sender.send(event).is_err() {
                        // Nobody is listening any more. Keep draining rather
                        // than returning: the snapshot is still being written,
                        // and a reader that stops reading a pipe eventually
                        // wedges the process it is reading.
                        continue;
                    }
                }
            }
            for event in adapter.on_eof() {
                record(&snapshot, &event, adapter.external_id());
                let _ = sender.send(event);
            }
            if let Some(id) = adapter.external_id() {
                snapshot.lock().unwrap().external_id = Some(id.to_string());
            }
        }));
    }

    Ok(Running {
        child: Arc::new(Mutex::new(child)),
        pid,
        events: Some(receiver),
        snapshot,
        interrupted: Arc::new(AtomicBool::new(false)),
        readers,
    })
}

fn record(snapshot: &Arc<Mutex<Snapshot>>, event: &BrainEvent, external_id: Option<&str>) {
    let mut snapshot = snapshot.lock().unwrap();
    if let Some(id) = external_id {
        snapshot.external_id = Some(id.to_string());
    }
    match event {
        BrainEvent::TurnStarted { model } => {
            snapshot.turns += 1;
            if model.is_some() {
                snapshot.model = model.clone();
            }
        }
        BrainEvent::Usage(usage) => {
            snapshot.usage_reported = true;
            snapshot.usage.input_tokens += usage.input_tokens;
            snapshot.usage.output_tokens += usage.output_tokens;
            snapshot.usage.cached_input_tokens += usage.cached_input_tokens;
        }
        BrainEvent::TurnEnded { reason } => snapshot.last_stop = Some(*reason),
        _ => {}
    }
}

impl Running {
    /// The stream of events. Taken once — there is one reader.
    pub fn events(&mut self) -> Receiver<BrainEvent> {
        self.events
            .take()
            .expect("a brain's events can only be taken once")
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// What has been seen so far. Safe to call while the brain is running.
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().unwrap().clone()
    }

    /// Whether the process is still alive.
    pub fn is_running(&self) -> bool {
        matches!(self.child.lock().unwrap().try_wait(), Ok(None))
    }

    /// Ask the brain to stop, and make sure it does.
    ///
    /// Polite first — SIGINT to the whole process group, which is what a
    /// human pressing Ctrl-C would send — then SIGKILL once `grace` has
    /// passed. Without the deadline a CLI that traps SIGINT and keeps working
    /// would turn "interrupt" into a suggestion.
    ///
    /// The deadline is measured against the *group*, not against the agent.
    /// A shell dies on SIGINT while the background job it started ignores it
    /// — POSIX requires exactly that — so waiting only for the direct child
    /// declares success at the moment the survivors are orphaned. This waits
    /// until nothing in the group is left, and kills whatever still is.
    ///
    /// Windows has no polite stage: there is no signal a detached child
    /// reliably honours, so the tree is terminated outright. Callers that need
    /// a clean shutdown there must ask the CLI to stop through its own
    /// protocol first.
    pub fn interrupt(&self, grace: Duration) -> anyhow::Result<()> {
        self.interrupted.store(true, Ordering::SeqCst);
        self.snapshot.lock().unwrap().interrupted = true;

        #[cfg(unix)]
        {
            signal_group(self.pid, libc::SIGINT);
            let deadline = Instant::now() + grace;
            loop {
                // Calling this reaps the child once it exits, so a zombie
                // leader does not keep the group looking occupied.
                let child_alive = self.is_running();
                if !child_alive && !group_is_alive(self.pid) {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            signal_group(self.pid, libc::SIGKILL);
        }
        #[cfg(windows)]
        {
            let _ = grace;
            let _ = Command::new("taskkill")
                .args(["/PID", &self.pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        // Reap it, so the interrupted brain does not linger as a zombie.
        let _ = self.child.lock().unwrap().wait();
        Ok(())
    }

    /// Wait for the brain to finish and collect what it did.
    ///
    /// Joins the readers first: the process can exit while a last line is
    /// still in the pipe, and returning a snapshot that is missing the final
    /// turn would make a completed run look interrupted.
    pub fn wait(mut self) -> anyhow::Result<Snapshot> {
        let status = self.child.lock().unwrap().wait()?;
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        let mut snapshot = self.snapshot.lock().unwrap().clone();
        snapshot.exit_code = status.code();
        if self.interrupted.load(Ordering::SeqCst) {
            snapshot.interrupted = true;
            snapshot.last_stop = Some(StopReason::Interrupted);
        } else if snapshot.last_stop.is_none() && status.code() != Some(0) {
            // The process died without the stream saying why. The stderr tail
            // is usually the only account of what went wrong, so it becomes
            // the reason rather than being left in a field nobody reads.
            snapshot.last_stop = Some(StopReason::Error);
        }
        *self.snapshot.lock().unwrap() = snapshot.clone();
        Ok(snapshot)
    }

    /// Why the brain stopped, for a caller that already has the snapshot.
    pub fn failure_reason(snapshot: &Snapshot) -> Option<String> {
        match snapshot.last_stop {
            Some(StopReason::Error) => Some(if snapshot.stderr_tail.is_empty() {
                format!(
                    "the brain exited with {}",
                    snapshot
                        .exit_code
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "no status".into())
                )
            } else {
                snapshot.stderr_tail.join("\n")
            }),
            _ => None,
        }
    }
}

/// Whether anything is still in the group.
///
/// Signal 0 performs the permission and existence checks without delivering
/// anything, which is the only cheap way to ask "is any of that tree left".
#[cfg(unix)]
fn group_is_alive(pid: u32) -> bool {
    unsafe { libc::kill(-(pid as i32), 0) == 0 }
}

#[cfg(unix)]
fn signal_group(pid: u32, signal: i32) {
    // Negative pid means the group. The child was put in its own group at
    // spawn, so this cannot reach back into Unterm's own processes.
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::CodexAdapter;

    /// A brain that prints a turn and exits — enough to exercise the reading
    /// and accounting without needing an agent CLI installed.
    fn fake_brain(script: &str) -> Spec {
        Spec::new("sh").arg("-c").arg(script)
    }

    #[test]
    fn a_turn_is_read_and_accounted_for() {
        let script = r#"
printf '%s\n' '{"type":"turn.started","model":"gpt-5","session_id":"sess_9"}'
printf '%s\n' '{"type":"agent_message","text":"hello"}'
printf '%s\n' '{"type":"token_count","input_tokens":10,"output_tokens":4,"cached_tokens":6}'
printf '%s\n' '{"type":"turn.completed"}'
"#;
        let mut running = spawn(&fake_brain(script), Box::new(CodexAdapter::new())).unwrap();
        let events: Vec<BrainEvent> = running.events().into_iter().collect();
        let snapshot = running.wait().unwrap();

        assert!(events.iter().any(|e| matches!(e, BrainEvent::Text { .. })));
        assert_eq!(snapshot.turns, 1);
        assert_eq!(snapshot.model.as_deref(), Some("gpt-5"));
        assert_eq!(snapshot.usage.input_tokens, 10);
        assert_eq!(snapshot.usage.cached_input_tokens, 6);
        assert_eq!(snapshot.last_stop, Some(StopReason::Completed));
        assert_eq!(snapshot.exit_code, Some(0));
        assert_eq!(
            snapshot.external_id.as_deref(),
            Some("sess_9"),
            "without the CLI's own session id the conversation cannot be resumed"
        );
    }

    #[test]
    fn usage_accumulates_across_turns() {
        let script = r#"
printf '%s\n' '{"type":"turn.started","model":"m"}'
printf '%s\n' '{"type":"token_count","input_tokens":10,"output_tokens":1,"cached_tokens":2}'
printf '%s\n' '{"type":"turn.completed"}'
printf '%s\n' '{"type":"turn.started","model":"m"}'
printf '%s\n' '{"type":"token_count","input_tokens":20,"output_tokens":3,"cached_tokens":4}'
printf '%s\n' '{"type":"turn.completed"}'
"#;
        let mut running = spawn(&fake_brain(script), Box::new(CodexAdapter::new())).unwrap();
        running.events().into_iter().for_each(drop);
        let snapshot = running.wait().unwrap();

        assert_eq!(snapshot.turns, 2);
        assert_eq!(snapshot.usage.input_tokens, 30);
        assert_eq!(snapshot.usage.output_tokens, 4);
        // Still counted apart after summing — this is the column a bill is
        // reconciled against.
        assert_eq!(snapshot.usage.cached_input_tokens, 6);
    }

    #[test]
    fn the_prompt_goes_in_over_stdin() {
        // Not on the command line: a prompt in argv is visible to every `ps`
        // on the machine, and lands in whatever recorded the launch.
        let spec = fake_brain(
            r#"read -r line; printf '{"type":"agent_message","text":"%s"}\n' "$line""#,
        )
        .prompt("secret question");
        let mut running = spawn(&spec, Box::new(CodexAdapter::new())).unwrap();
        let events: Vec<BrainEvent> = running.events().into_iter().collect();
        running.wait().unwrap();
        assert_eq!(
            events,
            vec![BrainEvent::Text {
                text: "secret question".into()
            }]
        );
    }

    #[test]
    #[cfg(unix)]
    fn interrupting_reaches_the_children_the_agent_started() {
        // The failure this prevents: signalling the CLI and leaving the build
        // it launched running with nobody watching it.
        let marker = tempfile::tempdir().unwrap();
        let alive = marker.path().join("alive");
        let script = format!(
            r#"sh -c 'while true; do touch {alive:?}; sleep 0.05; done' &
printf '%s\n' '{{"type":"turn.started","model":"m"}}'
sleep 30"#,
            alive = alive.display().to_string()
        );
        let mut running = spawn(&fake_brain(&script), Box::new(CodexAdapter::new())).unwrap();
        let events = running.events();
        // Wait until the grandchild is definitely up.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !alive.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(alive.exists(), "the grandchild never started");

        running.interrupt(Duration::from_millis(200)).unwrap();
        let seen: Vec<BrainEvent> = events.into_iter().collect();
        let snapshot = running.wait().unwrap();

        // The grandchild stopped touching the marker. Before the group-aware
        // escalation this failed: `sh` died on SIGINT, the runtime saw its
        // child gone and returned, and the background job — which POSIX makes
        // ignore SIGINT — was left running with nobody watching it.
        std::fs::remove_file(&alive).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !alive.exists(),
            "the process the agent started outlived the interrupt"
        );

        assert!(snapshot.interrupted);
        assert_eq!(snapshot.last_stop, Some(StopReason::Interrupted));
        assert!(
            seen.contains(&BrainEvent::TurnEnded {
                reason: StopReason::Interrupted
            }),
            "the reader never reported the turn as interrupted: {seen:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_brain_that_ignores_the_polite_signal_still_stops() {
        // Otherwise "interrupt" is a request the CLI may decline.
        let script = r#"trap '' INT
printf '%s\n' '{"type":"turn.started","model":"m"}'
sleep 30"#;
        let mut running = spawn(&fake_brain(script), Box::new(CodexAdapter::new())).unwrap();
        let _events = running.events();
        // Give the trap time to be installed.
        std::thread::sleep(Duration::from_millis(150));
        let started = Instant::now();
        running.interrupt(Duration::from_millis(200)).unwrap();
        assert!(!running.is_running(), "the brain survived its interrupt");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the escalation never happened"
        );
    }

    #[test]
    fn a_brain_that_dies_leaves_its_last_words() {
        let script = r#"echo 'connection refused' >&2; exit 7"#;
        let mut running = spawn(&fake_brain(script), Box::new(CodexAdapter::new())).unwrap();
        running.events().into_iter().for_each(drop);
        let snapshot = running.wait().unwrap();

        assert_eq!(snapshot.exit_code, Some(7));
        assert_eq!(snapshot.last_stop, Some(StopReason::Error));
        assert_eq!(
            Running::failure_reason(&snapshot).as_deref(),
            Some("connection refused"),
            "a brain that failed without saying why must still be diagnosable"
        );
    }

    #[test]
    fn a_noisy_brain_does_not_keep_every_line_it_ever_wrote() {
        let script = r#"i=0; while [ $i -lt 200 ]; do echo "line $i" >&2; i=$((i+1)); done"#;
        let mut running = spawn(&fake_brain(script), Box::new(CodexAdapter::new())).unwrap();
        running.events().into_iter().for_each(drop);
        let snapshot = running.wait().unwrap();
        assert_eq!(snapshot.stderr_tail.len(), STDERR_TAIL);
        assert_eq!(snapshot.stderr_tail.last().unwrap(), "line 199");
    }

    #[test]
    fn resuming_needs_something_to_resume() {
        let base = Spec::new("codex").resume_flag("resume");
        assert!(
            base.clone().resuming(None).is_none(),
            "resuming without a session id would quietly start a new conversation"
        );
        assert!(base.clone().resuming(Some("")).is_none());
        let resumed = base.resuming(Some("sess_9")).unwrap();
        assert_eq!(resumed.args, ["resume", "sess_9"]);

        // A CLI with no resume flag cannot be continued at all, and says so.
        assert!(Spec::new("something").resuming(Some("sess_9")).is_none());
    }

    #[test]
    fn a_failed_conversation_is_not_offered_for_resumption() {
        let snapshot = Snapshot {
            external_id: Some("sess_9".into()),
            last_stop: Some(StopReason::Error),
            ..Snapshot::default()
        };
        assert!(!snapshot.resumable());
        // A turn that was interrupted or hit a cap can be picked back up.
        assert!(Snapshot {
            last_stop: Some(StopReason::Interrupted),
            ..snapshot.clone()
        }
        .resumable());
        assert!(Snapshot {
            last_stop: Some(StopReason::Limit),
            ..snapshot
        }
        .resumable());
    }

    #[test]
    fn a_snapshot_survives_being_written_down() {
        // It is stored on the step so a restarted Unterm can say what a dead
        // brain had done and cost.
        let snapshot = Snapshot {
            thread: Some(ThreadId::from_external("t1")),
            adapter: "codex".into(),
            external_id: Some("sess_9".into()),
            model: Some("gpt-5".into()),
            usage: Usage {
                input_tokens: 1,
                output_tokens: 2,
                cached_input_tokens: 3,
            },
            usage_reported: true,
            turns: 2,
            last_stop: Some(StopReason::Completed),
            stderr_tail: vec!["warn".into()],
            exit_code: Some(0),
            interrupted: false,
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        assert_eq!(
            serde_json::from_str::<Snapshot>(&json).unwrap(),
            snapshot,
            "{json}"
        );
    }
}
