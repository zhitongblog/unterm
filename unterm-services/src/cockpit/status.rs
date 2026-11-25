//! The cockpit state registry: one `PaneAgentStatus` per pane that runs an
//! AI agent, folded together from four signal layers.
//!
//! Precedence (strongest first): official hook events (`agent.signal`) >
//! OSC parsing (title / progress / notification / bell) > foreground
//! process detection > screen-text heuristics. A weaker layer never
//! overrides state set by a stronger one within `SIGNAL_HOLD`, with one
//! deliberate exception: `WaitingForUser` is sticky — only user input in
//! that pane, a hook event, or an OSC working/idle transition clears it,
//! so an Inbox entry cannot flicker away on a stray process poll.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// A stronger layer's verdict outranks weaker layers for this long.
const SIGNAL_HOLD: Duration = Duration::from_secs(10);
/// Hook-reported presence (an agent with no visible process, e.g. inside
/// WSL) expires after this long without another hook event.
const HOOK_PRESENCE_TTL: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentState {
    /// Needs the user — the Inbox condition. Highest display priority.
    WaitingForUser,
    /// Actively running a turn.
    Working,
    /// Turn just finished (decays to Idle after DONE_HOLD).
    Done,
    /// Agent process is up, sitting at its prompt.
    Idle,
}

impl AgentState {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentState::Working => "working",
            AgentState::WaitingForUser => "waiting",
            AgentState::Idle => "idle",
            AgentState::Done => "done",
        }
    }
}

/// Signal strength; smaller wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Layer {
    Hook = 0,
    Osc = 1,
    Process = 2,
    Heuristic = 3,
}

#[derive(Debug, Clone)]
pub struct PaneAgentStatus {
    pub pane_id: u64,
    pub agent: String,
    pub state: AgentState,
    /// When the current state began.
    pub since: Instant,
    pub task_hint: Option<String>,
    /// Name of the signal that produced the current state (debugging /
    /// `agent.status` output).
    pub last_signal: &'static str,
    pub fleet_id: Option<String>,
}

struct Entry {
    status: PaneAgentStatus,
    /// Layer that set the current state.
    layer: Layer,
    /// When the current state was (re)confirmed.
    signal_at: Instant,
    /// Presence source: false = a live foreground process anchors this
    /// entry; true = only hook events do (expires via HOOK_PRESENCE_TTL).
    hook_presence: bool,
    /// Last time any hook event arrived (for HOOK_PRESENCE_TTL).
    last_hook: Option<Instant>,
}

fn registry() -> &'static Mutex<HashMap<u64, Entry>> {
    static R: OnceLock<Mutex<HashMap<u64, Entry>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

fn enabled() -> bool {
    crate::settings::current().cockpit_enabled
}

/// Core transition. Creates the entry when `agent` is provided; otherwise
/// only updates an existing entry (signals like bell/progress don't know
/// which agent they belong to).
fn apply(
    pane_id: u64,
    agent: Option<&str>,
    new_state: AgentState,
    layer: Layer,
    signal: &'static str,
    task_hint: Option<String>,
) {
    if !enabled() {
        return;
    }
    let now = Instant::now();
    let mut reg = registry().lock();
    let entry = match reg.get_mut(&pane_id) {
        Some(e) => e,
        None => {
            let Some(agent) = agent else { return };
            if new_state == AgentState::Working {
                queue_checkpoint(pane_id);
            }
            reg.insert(
                pane_id,
                Entry {
                    status: PaneAgentStatus {
                        pane_id,
                        agent: agent.to_string(),
                        state: new_state,
                        since: now,
                        task_hint,
                        last_signal: signal,
                        fleet_id: None,
                    },
                    layer,
                    signal_at: now,
                    hook_presence: layer == Layer::Hook,
                    last_hook: (layer == Layer::Hook).then_some(now),
                },
            );
            return;
        }
    };

    if layer == Layer::Hook {
        entry.last_hook = Some(now);
    }
    if let Some(agent) = agent {
        // A signal that knows the agent name refreshes it (e.g. the
        // process poll replacing a generic hook-created name).
        if entry.status.agent != agent && layer <= entry.layer {
            entry.status.agent = agent.to_string();
        }
    }
    if let Some(hint) = task_hint {
        entry.status.task_hint = Some(hint);
    }

    // A weaker layer may not override a stronger layer's recent verdict —
    // except an OSC-or-better WaitingForUser, which always penetrates:
    // losing an "agent needs you" is the worst failure mode, and a false
    // positive clears on the user's first keystroke anyway.
    let attention_override = new_state == AgentState::WaitingForUser && layer <= Layer::Osc;
    let heuristic_refines_process =
        layer == Layer::Heuristic && entry.layer == Layer::Process;
    if layer > entry.layer
        && entry.signal_at.elapsed() < SIGNAL_HOLD
        && !attention_override
        && !heuristic_refines_process
    {
        return;
    }
    // WaitingForUser is sticky against anything below OSC.
    if entry.status.state == AgentState::WaitingForUser
        && layer > Layer::Osc
        && new_state != AgentState::WaitingForUser
    {
        return;
    }

    if entry.status.state != new_state {
        if new_state == AgentState::Working && entry.status.state != AgentState::WaitingForUser {
            queue_checkpoint(pane_id);
        }
        entry.status.state = new_state;
        entry.status.since = now;
    }
    entry.status.last_signal = signal;
    entry.layer = layer;
    entry.signal_at = now;
}

/// Panes whose agent just started working — the GUI tick drains this and
/// takes a pre-work git snapshot off-thread (see cockpit::review).
fn checkpoint_queue() -> &'static Mutex<Vec<u64>> {
    static Q: OnceLock<Mutex<Vec<u64>>> = OnceLock::new();
    Q.get_or_init(|| Mutex::new(Vec::new()))
}

fn queue_checkpoint(pane_id: u64) {
    if !crate::settings::current().cockpit_auto_checkpoint {
        return;
    }
    let mut q = checkpoint_queue().lock();
    if !q.contains(&pane_id) {
        q.push(pane_id);
    }
}

pub fn take_checkpoint_requests() -> Vec<u64> {
    std::mem::take(&mut *checkpoint_queue().lock())
}

/// Shared epoch for the working-dot breathing phase, so every window
/// pulses in sync.
pub fn breath_epoch() -> &'static Instant {
    static E: OnceLock<Instant> = OnceLock::new();
    E.get_or_init(Instant::now)
}

/// Layer-4 screen-text heuristics — the fallback for agents that emit no
/// OSC signals (Aider's line-based REPL, mainly) and for process-detected
/// agents (`Layer::Process`) that do not set dynamic OSC titles (e.g. `agy` /
/// `antigravity`). Only consulted for entries whose current verdict came from
/// the process layer or below; any OSC/hook activity outranks this. `tail` is
/// the last few viewport lines, newest last.
pub fn on_screen_tail(pane_id: u64, tail: &[String]) {
    {
        let reg = registry().lock();
        match reg.get(&pane_id) {
            // A recent stronger signal (Hook or OSC) owns the state — don't
            // guess. Process-layer initial `Working` (`Layer::Process`) is a
            // launch guess awaiting screen/OSC confirmation, so screen-text
            // prompts (`>`, `❯`, `PS ...>`) may refine it immediately.
            Some(e) if e.layer < Layer::Process && e.signal_at.elapsed() < SIGNAL_HOLD => return,
            Some(_) => {}
            None => return,
        }
    }
    let joined = tail.join("\n").to_ascii_lowercase();
    let last_line = tail
        .iter()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim_end())
        .unwrap_or("");
    let state = if joined.contains("(y)es") && joined.contains("(n)o") {
        // Aider's confirmation prompt.
        Some(AgentState::WaitingForUser)
    } else if joined.contains("esc to interrupt") {
        Some(AgentState::Working)
    } else if last_line.ends_with('>')
        || last_line.ends_with('❯')
        || last_line.ends_with('›')
        || last_line.ends_with('$')
        || last_line.ends_with('%')
        || tail.iter().rev().take(6).any(|l| {
            let trimmed = l.trim();
            trimmed == ">" || trimmed == "❯" || trimmed == "›"
        })
    {
        Some(AgentState::Idle)
    } else {
        None
    };
    if let Some(state) = state {
        apply(pane_id, None, state, Layer::Heuristic, "screen-text", None);
    }
}

/// Decay an unconfirmed process-layer `Working` guess to `Idle` when a pane
/// has produced no screen changes for at least `SIGNAL_HOLD`.
pub fn on_pane_unchanged(pane_id: u64) {
    let mut reg = registry().lock();
    if let Some(entry) = reg.get_mut(&pane_id) {
        if entry.layer >= Layer::Process
            && entry.status.state == AgentState::Working
            && entry.signal_at.elapsed() >= SIGNAL_HOLD
        {
            let now = Instant::now();
            entry.status.state = AgentState::Idle;
            entry.status.since = now;
            entry.status.last_signal = "idle-decay";
            entry.layer = Layer::Heuristic;
            entry.signal_at = now;
        }
    }
}

// ─── Signal entry points ────────────────────────────────────────────────

/// OSC 0/2 title changed for a pane.
pub fn on_title_change(pane_id: u64, title: &str) {
    let Some(sig) = super::title::parse_title(title) else {
        return;
    };
    apply(
        pane_id,
        sig.agent,
        sig.state,
        Layer::Osc,
        "osc-title",
        sig.task_hint,
    );
}

/// OSC 9;4 progress state changed. `active` covers Percentage and
/// Indeterminate; `cleared` is `Progress::None`.
pub fn on_progress(pane_id: u64, active: bool) {
    if active {
        apply(
            pane_id,
            None,
            AgentState::Working,
            Layer::Osc,
            "osc-progress",
            None,
        );
    } else {
        // Progress cleared: the turn ended. Done (not Idle) so the user
        // notices completion; decays via poll().
        let has_entry = registry().lock().contains_key(&pane_id);
        if has_entry {
            apply(
                pane_id,
                None,
                AgentState::Done,
                Layer::Osc,
                "osc-progress-clear",
                None,
            );
        }
    }
}

/// OSC 9 / OSC 777 toast notification from the pane.
pub fn on_notification(pane_id: u64, title: Option<&str>, body: &str) {
    let has_entry = registry().lock().contains_key(&pane_id);
    if !has_entry {
        return;
    }
    let state = super::title::classify_notification(title, body);
    apply(pane_id, None, state, Layer::Osc, "osc-notify", None);
}

/// BEL in a pane. Part of the OSC signal family, but semantically weak:
/// only meaningful for a pane that already hosts an agent, and it never
/// interrupts a turn that a stronger signal says is still running.
pub fn on_bell(pane_id: u64) {
    let mut reg = registry().lock();
    let Some(entry) = reg.get_mut(&pane_id) else {
        return;
    };
    if entry.status.state == AgentState::Working
        && entry.layer <= Layer::Osc
        && entry.signal_at.elapsed() < SIGNAL_HOLD
    {
        return;
    }
    drop(reg);
    apply(
        pane_id,
        None,
        AgentState::WaitingForUser,
        Layer::Osc,
        "bell",
        None,
    );
}

/// Official hook event (`agent.signal` over MCP / CLI). Strongest layer.
/// `event` ∈ working|waiting|done|idle.
pub fn on_hook_signal(pane_id: u64, agent: &str, event: &str) -> bool {
    let state = match event {
        "working" => AgentState::Working,
        "waiting" => AgentState::WaitingForUser,
        "done" => AgentState::Done,
        "idle" => AgentState::Idle,
        _ => return false,
    };
    apply(pane_id, Some(agent), state, Layer::Hook, "hook", None);
    true
}

/// The user typed into this pane — clears WaitingForUser (they answered).
pub fn on_user_input(pane_id: u64) {
    let mut reg = registry().lock();
    if let Some(entry) = reg.get_mut(&pane_id) {
        if entry.status.state == AgentState::WaitingForUser {
            entry.status.state = AgentState::Working;
            entry.status.since = Instant::now();
            entry.status.last_signal = "user-input";
            // User input is authoritative about "no longer waiting", but
            // weak about what comes next — let any layer re-classify.
            entry.layer = Layer::Heuristic;
            entry.signal_at = Instant::now();
        }
    }
}

/// Set / clear fleet membership for a pane (called by the fleet engine).
pub fn set_fleet(pane_id: u64, fleet_id: Option<String>) {
    if let Some(entry) = registry().lock().get_mut(&pane_id) {
        entry.status.fleet_id = fleet_id;
    }
}

// ─── Periodic poll (the 2s status tick) ────────────────────────────────

/// Feed the process-detection layer and run decay for a set of panes.
/// `probe(pane_id)` returns the detected agent name for the pane (from
/// the non-blocking mcp handler cache) — kept as a closure so this module
/// stays testable without a Mux.
pub fn poll<F>(pane_ids: &[u64], mut probe: F)
where
    F: FnMut(u64) -> Option<String>,
{
    if !enabled() {
        return;
    }
    let now = Instant::now();
    for &pane_id in pane_ids {
        let detected = probe(pane_id);
        match detected {
            Some(agent) => {
                let mut reg = registry().lock();
                match reg.get_mut(&pane_id) {
                    Some(entry) => {
                        entry.hook_presence = false;
                        if entry.status.agent != agent && entry.layer >= Layer::Process {
                            entry.status.agent = agent;
                        }
                    }
                    None => {
                        drop(reg);
                        // Fresh agent appearance: assume Working — an
                        // agent that was just launched with a task is
                        // running it; the first OSC signal corrects us
                        // within a second if not.
                        apply(
                            pane_id,
                            Some(&agent),
                            AgentState::Working,
                            Layer::Process,
                            "process",
                            None,
                        );
                        // Restore fleet membership (e.g. the agent was
                        // relaunched in a fleet member's pane).
                        if let Some(fid) = super::fleet::fleet_for_pane(pane_id) {
                            set_fleet(pane_id, Some(fid));
                        }
                    }
                }
            }
            None => {
                let mut reg = registry().lock();
                let remove = match reg.get(&pane_id) {
                    Some(entry) => {
                        if entry.hook_presence {
                            entry
                                .last_hook
                                .map(|t| t.elapsed() > HOOK_PRESENCE_TTL)
                                .unwrap_or(true)
                        } else {
                            // The process cache refreshes every ~2s; two
                            // consecutive misses (handled naturally by
                            // the tick cadence) mean the agent exited.
                            entry.signal_at.elapsed() > Duration::from_secs(5)
                        }
                    }
                    None => false,
                };
                if remove {
                    reg.remove(&pane_id);
                }
            }
        }
    }
    // Decay Done → Idle.
    let mut reg = registry().lock();
    for entry in reg.values_mut() {
        if entry.status.state == AgentState::Done && entry.status.since.elapsed() > done_hold() {
            entry.status.state = AgentState::Idle;
            entry.status.since = now;
            entry.status.last_signal = "done-decay";
        }
    }
}

fn done_hold() -> Duration {
    Duration::from_secs(crate::settings::current().cockpit_done_hold_secs)
}

/// Drop panes that no longer exist (called with the live pane-id set).
pub fn retain_panes(live: &std::collections::HashSet<u64>) {
    registry().lock().retain(|id, _| live.contains(id));
}

// ─── Read side ──────────────────────────────────────────────────────────

pub fn status_for_pane(pane_id: u64) -> Option<PaneAgentStatus> {
    registry().lock().get(&pane_id).map(|e| e.status.clone())
}

pub fn snapshot() -> Vec<PaneAgentStatus> {
    let mut v: Vec<_> = registry()
        .lock()
        .values()
        .map(|e| e.status.clone())
        .collect();
    // Inbox order: waiting (longest first) → working → done → idle.
    v.sort_by(|a, b| {
        a.state
            .cmp(&b.state)
            .then_with(|| b.since.elapsed().cmp(&a.since.elapsed()))
    });
    v
}

/// Highest-priority state among a set of panes (the tab badge). The enum
/// is ordered so that `min` = display priority: waiting > working > done
/// > idle.
pub fn tab_state(pane_ids: &[u64]) -> Option<AgentState> {
    let reg = registry().lock();
    pane_ids
        .iter()
        .filter_map(|id| reg.get(id).map(|e| e.status.state))
        .min()
}

/// (working, waiting) counts across all panes — the top-bar chip.
pub fn summary() -> (usize, usize) {
    let reg = registry().lock();
    let mut working = 0;
    let mut waiting = 0;
    for e in reg.values() {
        match e.status.state {
            AgentState::Working => working += 1,
            AgentState::WaitingForUser => waiting += 1,
            _ => {}
        }
    }
    (working, waiting)
}

/// Clear the status table; see the note on `fleet::reset_store_for_tests`.
#[cfg(any(test, feature = "test-support"))]
pub fn reset_for_tests() {
    registry().lock().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry is process-global; serialize tests so parallel test
    /// threads don't see each other's panes (summary() counts them all).
    fn test_guard() -> parking_lot::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let guard = LOCK.get_or_init(|| Mutex::new(())).lock();
        reset_for_tests();
        guard
    }

    fn state_of(pane: u64) -> Option<AgentState> {
        status_for_pane(pane).map(|s| s.state)
    }

    #[test]
    fn process_then_title_lifecycle() {
        let _g = test_guard();
        // Process poll discovers claude → Working (fresh launch).
        poll(&[1], |_| Some("claude".to_string()));
        assert_eq!(state_of(1), Some(AgentState::Working));

        // Title says idle (✳) → OSC beats process.
        on_title_change(1, "✳ Fix the bug");
        assert_eq!(state_of(1), Some(AgentState::Idle));
        assert_eq!(
            status_for_pane(1).unwrap().task_hint.as_deref(),
            Some("Fix the bug")
        );

        // Braille spinner → Working.
        on_title_change(1, "⠐ Fix the bug");
        assert_eq!(state_of(1), Some(AgentState::Working));

        // Agent exits: two poll misses spaced past the grace window are
        // simulated by direct removal check (elapsed-based; not
        // time-travel-testable without injecting a clock — covered by
        // the integration test instead).
    }

    #[test]
    fn waiting_is_sticky_against_weak_layers() {
        let _g = test_guard();
        poll(&[2], |_| Some("codex".to_string()));
        on_notification(2, Some("Codex"), "approval-requested");
        assert_eq!(state_of(2), Some(AgentState::WaitingForUser));

        // A process poll (weaker) must not clear waiting.
        poll(&[2], |_| Some("codex".to_string()));
        assert_eq!(state_of(2), Some(AgentState::WaitingForUser));

        // User input clears it.
        on_user_input(2);
        assert_eq!(state_of(2), Some(AgentState::Working));
    }

    #[test]
    fn hook_beats_osc() {
        let _g = test_guard();
        assert!(on_hook_signal(3, "claude", "waiting"));
        assert_eq!(state_of(3), Some(AgentState::WaitingForUser));
        // OSC progress-working may clear waiting (same-or-stronger rule:
        // Osc > Hook is false, but waiting-stickiness allows Osc).
        on_progress(3, true);
        assert_eq!(state_of(3), Some(AgentState::WaitingForUser)); // Hook holds within SIGNAL_HOLD
    }

    #[test]
    fn bell_only_matters_for_agent_panes() {
        let _g = test_guard();
        on_bell(9); // no agent → ignored
        assert!(status_for_pane(9).is_none());

        poll(&[4], |_| Some("aider".to_string()));
        on_title_change(4, "✳ t"); // put it at Idle via OSC… (claude syntax, fine for the test)
        on_bell(4);
        assert_eq!(state_of(4), Some(AgentState::WaitingForUser));
    }

    #[test]
    fn summary_counts() {
        let _g = test_guard();
        poll(&[5, 6], |id| {
            Some(if id == 5 { "claude" } else { "codex" }.to_string())
        });
        on_notification(6, None, "approval-requested");
        // pane 5 fresh-launch Working, pane 6 waiting.
        assert_eq!(summary(), (1, 1));
    }

    #[test]
    fn screen_tail_prompt_refines_process_layer_working_to_idle() {
        let _g = test_guard();
        // Process poll discovers antigravity → starts at Working (Layer::Process).
        poll(&[7], |_| Some("antigravity".to_string()));
        assert_eq!(state_of(7), Some(AgentState::Working));

        // Screen tail shows an idle prompt with a status footer line below it
        // (like Antigravity TUI: `───`, `>`, `───`, `? for shortcuts ...`).
        on_screen_tail(
            7,
            &[
                "───────────────".to_string(),
                ">".to_string(),
                "───────────────".to_string(),
                "? for shortcuts   auto · Gemini 4 Argon · medium".to_string(),
            ],
        );
        assert_eq!(state_of(7), Some(AgentState::Idle));
    }
}
