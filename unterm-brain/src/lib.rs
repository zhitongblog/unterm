//! One shape for what a model did, whatever CLI printed it.
//!
//! Unterm already watches agents from the outside — screen tails, window
//! titles, terminal bells, progress escapes. That works for any program and
//! knows almost nothing: it can tell that something is happening, not what
//! was asked, what tool was called, or how many tokens it cost. Codex and
//! Claude both emit structured streams saying exactly that, in two different
//! vocabularies.
//!
//! This is the third vocabulary, the one everything downstream reads. The
//! milestone's gate is that the two adapters produce *isomorphic* events, so
//! the Cockpit, the audit trail and the task store never learn which CLI was
//! behind a turn.
//!
//! **Adapters are pure.** A line of bytes goes in, events come out; nothing
//! here spawns a process, opens a socket or performs a tool call. That is
//! deliberate and it is what makes the gate testable: two adapters can be fed
//! recordings and compared, with no processes involved. The runtime above
//! does the side effects — in particular, a [`BrainEvent::ToolRequested`] is
//! a *request*, and the only thing allowed to act on it is the action
//! gateway.

pub mod acp;
pub mod adapters;
pub mod health;
pub mod host;
pub mod sdk;
pub mod runtime;
pub mod supervisor;

use serde::{Deserialize, Serialize};
use unterm_tasks::{RunId, TaskId};

/// Identifies one conversation with a model.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ThreadId(String);

impl ThreadId {
    pub fn new() -> Self {
        // Threads are named by the caller far more often than they are
        // generated — a CLI session id, a pane, a fleet member — so this is
        // the fallback rather than the norm.
        Self(format!("thr_{:016x}", fastrand_seed()))
    }

    pub fn from_external(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ThreadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Default for ThreadId {
    fn default() -> Self {
        Self::new()
    }
}

/// A counter, because this crate has no business depending on a clock or an
/// RNG: both would make adapter output unreproducible, and reproducibility is
/// what the equivalence test rests on.
fn fastrand_seed() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// One conversation, tied to the work it is being done for.
///
/// The association is the point: a turn that spends tokens and calls tools is
/// part of a run of a task, and without that link the Cockpit can show that
/// an agent is busy but never what it is busy *with*.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Thread {
    pub id: ThreadId,
    /// Which adapter produced it, for diagnosis. Downstream logic must not
    /// branch on this — that would be the isomorphism quietly breaking.
    pub adapter: String,
    pub model: Option<String>,
    pub task_id: Option<TaskId>,
    pub run_id: Option<RunId>,
}

impl Thread {
    pub fn new(adapter: impl Into<String>) -> Self {
        Self {
            id: ThreadId::new(),
            adapter: adapter.into(),
            model: None,
            task_id: None,
            run_id: None,
        }
    }

    pub fn with_id(mut self, id: ThreadId) -> Self {
        self.id = id;
        self
    }

    pub fn for_run(mut self, task_id: TaskId, run_id: RunId) -> Self {
        self.task_id = Some(task_id);
        self.run_id = Some(run_id);
        self
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
}

/// Why a turn stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished what it was saying.
    Completed,
    /// It stopped to have a tool run.
    ToolRequest,
    /// Somebody interrupted it.
    Interrupted,
    /// It hit a limit — context, output cap, budget.
    Limit,
    /// It failed.
    Error,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::Completed => "completed",
            StopReason::ToolRequest => "tool_request",
            StopReason::Interrupted => "interrupted",
            StopReason::Limit => "limit",
            StopReason::Error => "error",
        }
    }
}

/// What a turn cost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Reported separately when the provider does; folded into neither of the
    /// above, because a cached read is not priced like a fresh one and a
    /// column that mixes them cannot be un-mixed later.
    pub cached_input_tokens: u64,
}

/// Everything a brain can say, in the one vocabulary.
///
/// Freeze point: two adapters must be able to produce these from different
/// wire formats, and everything downstream reads only this. Adding a variant
/// means every reader gets a new case; changing one means every recording
/// ever captured is reinterpreted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrainEvent {
    /// A turn began. `model` is reported when the stream says so, which is
    /// not always at the start.
    TurnStarted { model: Option<String> },
    /// Prose meant for the user.
    Text { text: String },
    /// The model's private thinking, when the provider exposes it. Kept
    /// separate from `Text` because it is shown differently, redacted
    /// differently, and must never be mistaken for an answer.
    Reasoning { text: String },
    /// The model wants a tool run. This is a *request*: nothing in this crate
    /// performs it, and the only thing that may is the action gateway.
    ToolRequested {
        /// Correlates the request with its result.
        call_id: String,
        name: String,
        arguments: serde_json::Value,
    },
    /// How a tool call turned out, as reported back into the stream.
    ToolResult {
        call_id: String,
        ok: bool,
        output: Option<String>,
    },
    /// What the turn cost.
    Usage(Usage),
    /// The turn is over.
    TurnEnded { reason: StopReason },
    /// The adapter could not make sense of something, or the model reported a
    /// failure. Never silently dropped: a stream that stops making sense is
    /// something an operator has to be able to see.
    Error { message: String },
    /// The CLI is asking its host whether a tool may run, and is waiting for
    /// the answer. Only a CLI driven over a host protocol asks (Claude with
    /// `--permission-prompt-tool stdio`); the answer goes back through
    /// [`host::HostProtocol::permission_reply`]. Like `ToolRequested`, this
    /// is a question, never a grant.
    PermissionRequested {
        /// The CLI's id for this question; the reply must carry it back.
        request_id: String,
        tool: String,
        input: serde_json::Value,
        description: Option<String>,
    },
}

impl BrainEvent {
    /// A short name for logs and metrics.
    pub fn kind(&self) -> &'static str {
        match self {
            BrainEvent::TurnStarted { .. } => "turn_started",
            BrainEvent::Text { .. } => "text",
            BrainEvent::Reasoning { .. } => "reasoning",
            BrainEvent::ToolRequested { .. } => "tool_requested",
            BrainEvent::ToolResult { .. } => "tool_result",
            BrainEvent::Usage(_) => "usage",
            BrainEvent::TurnEnded { .. } => "turn_ended",
            BrainEvent::Error { .. } => "error",
            BrainEvent::PermissionRequested { .. } => "permission_requested",
        }
    }

    /// Whether this is something only the gateway may act on.
    pub fn is_tool_request(&self) -> bool {
        matches!(self, BrainEvent::ToolRequested { .. })
    }
}

/// Turns one CLI's stream into the shared vocabulary.
///
/// Pure by contract. An implementation that spawns anything, reads a file or
/// performs a tool call has broken the property the equivalence test depends
/// on, and there is no way for the test to notice.
pub trait BrainAdapter: Send {
    /// Which CLI this reads. For diagnosis only.
    fn id(&self) -> &'static str;

    /// Interpret one line of the stream.
    ///
    /// Returns every event that line implies, in order — one line can carry
    /// several, and many lines carry none. A line that cannot be understood
    /// yields [`BrainEvent::Error`] rather than nothing, because a stream
    /// that has stopped making sense must not look like a quiet one.
    fn on_line(&mut self, line: &str) -> Vec<BrainEvent>;

    /// The CLI's own id for this conversation, once the stream has named it.
    ///
    /// Parsed state, not I/O — the adapter stays pure. It is separate from
    /// the event vocabulary on purpose: `--resume` is a fact about a CLI, and
    /// putting it in [`BrainEvent`] would make every reader learn a concept
    /// that only the launcher needs.
    fn external_id(&self) -> Option<&str> {
        None
    }

    /// Anything held back once the stream ends.
    ///
    /// A turn cut off mid-flight has no closing line, and the runtime still
    /// needs to be told the turn is over.
    fn on_eof(&mut self) -> Vec<BrainEvent> {
        Vec::new()
    }
}

/// Feed a whole stream through an adapter. Test and replay helper.
pub fn replay(adapter: &mut dyn BrainAdapter, stream: &str) -> Vec<BrainEvent> {
    let mut events = Vec::new();
    for line in stream.lines() {
        events.extend(adapter.on_line(line));
    }
    events.extend(adapter.on_eof());
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_carries_the_work_it_belongs_to() {
        let task = TaskId::new();
        let run = RunId::new();
        let thread = Thread::new("codex")
            .for_run(task.clone(), run.clone())
            .model("gpt-5");
        assert_eq!(thread.task_id, Some(task));
        assert_eq!(thread.run_id, Some(run));
        assert_eq!(thread.model.as_deref(), Some("gpt-5"));
        // Without this link the Cockpit can say an agent is busy but never
        // what it is busy with.
    }

    #[test]
    fn an_external_id_is_kept_as_given() {
        // A CLI's own session id is more useful than one we invent, because
        // it is what appears in that CLI's logs when someone goes looking.
        let thread = Thread::new("claude").with_id(ThreadId::from_external("abc-123"));
        assert_eq!(thread.id.as_str(), "abc-123");
    }

    #[test]
    fn events_round_trip_through_their_wire_form() {
        // Recordings are replayed by the equivalence test and stored in the
        // task log; a variant that cannot survive serialisation would break
        // both, silently.
        let events = vec![
            BrainEvent::TurnStarted {
                model: Some("gpt-5".into()),
            },
            BrainEvent::Text { text: "hi".into() },
            BrainEvent::Reasoning {
                text: "thinking".into(),
            },
            BrainEvent::ToolRequested {
                call_id: "c1".into(),
                name: "shell".into(),
                arguments: serde_json::json!({"cmd": "ls"}),
            },
            BrainEvent::ToolResult {
                call_id: "c1".into(),
                ok: true,
                output: Some("a\nb".into()),
            },
            BrainEvent::Usage(Usage {
                input_tokens: 10,
                output_tokens: 20,
                cached_input_tokens: 5,
            }),
            BrainEvent::TurnEnded {
                reason: StopReason::Completed,
            },
            BrainEvent::Error {
                message: "boom".into(),
            },
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let back: BrainEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(back, event, "{json}");
            // The tag is what a reader matches on.
            assert!(json.contains(event.kind()), "{json} should name {}", event.kind());
        }
    }

    #[test]
    fn reasoning_is_not_text() {
        // They are shown differently, redacted differently, and a reader that
        // conflates them will eventually present private thinking as an
        // answer.
        let text = BrainEvent::Text { text: "x".into() };
        let reasoning = BrainEvent::Reasoning { text: "x".into() };
        assert_ne!(text, reasoning);
        assert_ne!(text.kind(), reasoning.kind());
    }

    #[test]
    fn only_a_tool_request_is_a_tool_request() {
        assert!(BrainEvent::ToolRequested {
            call_id: "c".into(),
            name: "shell".into(),
            arguments: serde_json::Value::Null,
        }
        .is_tool_request());
        // A result is a report of something that already happened; treating
        // it as a request would run the tool twice.
        assert!(!BrainEvent::ToolResult {
            call_id: "c".into(),
            ok: true,
            output: None,
        }
        .is_tool_request());
        assert!(!BrainEvent::Text { text: "x".into() }.is_tool_request());
    }

    #[test]
    fn cached_input_is_counted_apart_from_fresh_input() {
        let usage = Usage {
            input_tokens: 100,
            cached_input_tokens: 90,
            output_tokens: 10,
        };
        // Folding cached reads into `input_tokens` cannot be undone later,
        // and the two are not priced the same.
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.cached_input_tokens, 90);
    }
}
