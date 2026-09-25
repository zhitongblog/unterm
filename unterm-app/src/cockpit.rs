//! What the agents are doing, on screen.
//!
//! This is the product's reason to exist: several AI agents running in
//! several panes, and a person who needs to know which of them is waiting on
//! them without visiting each one. The data layer already tracks it --
//! `unterm_services::cockpit` watches screens, titles and hook signals -- so
//! what is here is the part a window owns: what the rows say, what order they
//! come in, and which one needs the person now.
//!
//! Sorting is the whole feature. An inbox that lists panes in pane order is a
//! list of panes; one that puts "waiting for you, for the longest" at the top
//! is an inbox.

use unterm_services::cockpit::status::{AgentState, PaneAgentStatus};

fn peer_cache() -> &'static parking_lot::Mutex<Vec<LocatedStatus>> {
    static CACHE: std::sync::OnceLock<parking_lot::Mutex<Vec<LocatedStatus>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn parse_state(state: &str) -> Option<AgentState> {
    match state {
        "waiting" => Some(AgentState::WaitingForUser),
        "working" => Some(AgentState::Working),
        "done" => Some(AgentState::Done),
        "idle" => Some(AgentState::Idle),
        _ => None,
    }
}

/// Refresh peer-window rows away from paint/input.
pub fn refresh_peer_statuses(current_instance_id: String) {
    static REFRESHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if REFRESHING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("cockpit-peer-snapshot".to_string())
        .spawn(move || {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(i64::MAX as u128) as i64;
            let peers = unterm_services::server_info::list_live_instances()
                .into_iter()
                .filter(|instance| instance.id != current_instance_id)
                .flat_map(|instance| {
                    let instance_id = instance.id.clone();
                    let window_title = instance
                        .title
                        .clone()
                        .unwrap_or_else(|| instance.id.clone());
                    instance.agents.into_iter().filter_map(move |agent| {
                        Some(LocatedStatus {
                            pane_id: agent.pane_id,
                            instance_id: instance_id.clone(),
                            window_title: window_title.clone(),
                            tab_id: agent.tab_id,
                            agent: agent.agent,
                            state: parse_state(&agent.state)?,
                            age_seconds: now.saturating_sub(agent.since_unix_ms).max(0) as u64
                                / 1000,
                            task_hint: agent.task_hint,
                        })
                    })
                })
                .collect();
            *peer_cache().lock() = peers;
            REFRESHING.store(false, std::sync::atomic::Ordering::Release);
        });
    if spawned.is_err() {
        REFRESHING.store(false, std::sync::atomic::Ordering::Release);
    }
}

pub fn peer_statuses() -> Vec<LocatedStatus> {
    peer_cache().lock().clone()
}

/// A row of the inbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub pane_id: u64,
    /// Empty for the current process's legacy/local row.
    pub instance_id: String,
    pub window_title: String,
    pub tab_id: Option<u64>,
    /// What the row says: agent, state, and how long it has been that way.
    pub label: String,
    /// The task, if the agent said what it was doing.
    pub hint: String,
    /// True when this pane wants the person.
    pub needs_you: bool,
    /// The state itself, for the mark the row carries.
    pub state: AgentState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocatedStatus {
    pub pane_id: u64,
    pub instance_id: String,
    pub window_title: String,
    pub tab_id: Option<u64>,
    pub agent: String,
    pub state: AgentState,
    pub age_seconds: u64,
    pub task_hint: Option<String>,
}

/// Whether a state is one the person has to answer.
pub fn needs_attention(state: AgentState) -> bool {
    matches!(state, AgentState::WaitingForUser | AgentState::Done)
}

/// How a duration reads in an inbox.
///
/// Coarse on purpose: "4m" is the answer to "has this been sitting a while",
/// and a ticking seconds counter redraws the row every second to say nothing.
pub fn describe_age(seconds: u64) -> String {
    match seconds {
        0..=9 => "now".to_string(),
        10..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3600),
    }
}

/// What a state is called in a row.
pub fn describe_state(state: AgentState) -> &'static str {
    match state {
        AgentState::WaitingForUser => "waiting for you",
        AgentState::Working => "working",
        AgentState::Done => "done",
        AgentState::Idle => "idle",
    }
}

/// Build the inbox's rows from the tracked statuses.
///
/// `age_of` gives each pane's seconds-in-state, which the caller measures --
/// keeping the clock out of here is what lets the ordering be tested.
/// The window itself goes through `located_rows`; only tests come in here.
#[cfg(test)]
pub fn rows(
    statuses: &[PaneAgentStatus],
    mut age_of: impl FnMut(&PaneAgentStatus) -> u64,
) -> Vec<Row> {
    let located: Vec<LocatedStatus> = statuses
        .iter()
        .map(|status| LocatedStatus {
            pane_id: status.pane_id,
            instance_id: String::new(),
            window_title: String::new(),
            tab_id: None,
            agent: status.agent.clone(),
            state: status.state,
            age_seconds: age_of(status),
            task_hint: status.task_hint.clone(),
        })
        .collect();
    located_rows(&located)
}

pub fn located_rows(statuses: &[LocatedStatus]) -> Vec<Row> {
    let mut rows: Vec<(u8, u64, Row)> = statuses
        .iter()
        .map(|status| {
            let row = Row {
                pane_id: status.pane_id,
                instance_id: status.instance_id.clone(),
                window_title: status.window_title.clone(),
                tab_id: status.tab_id,
                label: format!(
                    "{} \u{00B7} {} \u{00B7} {}",
                    status.agent,
                    describe_state(status.state),
                    describe_age(status.age_seconds)
                ),
                hint: status.task_hint.clone().unwrap_or_default(),
                needs_you: needs_attention(status.state),
                state: status.state,
            };
            (rank(status.state), status.age_seconds, row)
        })
        .collect();

    // Waiting first, longest-waiting at the top of that: the person should be
    // able to answer the oldest question without reading the list.
    rows.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    rows.into_iter().map(|(_, _, row)| row).collect()
}

fn rank(state: AgentState) -> u8 {
    match state {
        AgentState::WaitingForUser => 0,
        AgentState::Done => 1,
        AgentState::Working => 2,
        AgentState::Idle => 3,
    }
}

/// How many panes want the person.
///
/// For the tab bar, where a number is all there is room for.
pub fn attention_count(statuses: &[PaneAgentStatus]) -> usize {
    statuses
        .iter()
        .filter(|status| needs_attention(status.state))
        .count()
}

/// Move the inbox selection one row, wrapping at both ends.
pub fn step_selection(selected: usize, count: usize, forward: bool) -> usize {
    if count == 0 {
        return 0;
    }
    let selected = selected.min(count - 1);
    if forward {
        (selected + 1) % count
    } else if selected == 0 {
        count - 1
    } else {
        selected - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn status(pane_id: u64, agent: &str, state: AgentState) -> PaneAgentStatus {
        PaneAgentStatus {
            pane_id,
            agent: agent.to_string(),
            state,
            since: Instant::now(),
            task_hint: None,
            last_signal: "test",
            fleet_id: None,
        }
    }

    #[test]
    fn waiting_panes_come_first() {
        let statuses = [
            status(1, "claude", AgentState::Working),
            status(2, "codex", AgentState::WaitingForUser),
            status(3, "gemini", AgentState::Idle),
        ];
        let rows = rows(&statuses, |_| 0);
        assert_eq!(rows[0].pane_id, 2, "the one wanting an answer goes first");
        assert!(rows[0].needs_you);
    }

    #[test]
    fn the_longest_wait_is_at_the_top() {
        // The person should be able to answer the oldest question without
        // reading the list.
        let statuses = [
            status(1, "claude", AgentState::WaitingForUser),
            status(2, "codex", AgentState::WaitingForUser),
        ];
        let rows = rows(
            &statuses,
            |status| if status.pane_id == 2 { 300 } else { 5 },
        );
        assert_eq!(rows[0].pane_id, 2);
    }

    #[test]
    fn done_outranks_working_but_not_waiting() {
        let statuses = [
            status(1, "a", AgentState::Working),
            status(2, "b", AgentState::Done),
            status(3, "c", AgentState::WaitingForUser),
        ];
        let order: Vec<u64> = rows(&statuses, |_| 0).iter().map(|r| r.pane_id).collect();
        assert_eq!(order, [3, 2, 1]);
    }

    #[test]
    fn a_row_says_which_agent_what_it_is_doing_and_for_how_long() {
        let statuses = [status(7, "claude", AgentState::WaitingForUser)];
        let rows = rows(&statuses, |_| 125);
        assert!(rows[0].label.contains("claude"));
        assert!(rows[0].label.contains("waiting for you"));
        assert!(rows[0].label.contains("2m"));
    }

    #[test]
    fn ages_are_coarse_rather_than_ticking() {
        // A seconds counter redraws the row every second to say nothing.
        assert_eq!(describe_age(3), "now");
        assert_eq!(describe_age(45), "45s");
        assert_eq!(describe_age(90), "1m");
        assert_eq!(describe_age(7200), "2h");
    }

    #[test]
    fn only_waiting_and_done_want_the_person() {
        assert!(needs_attention(AgentState::WaitingForUser));
        assert!(needs_attention(AgentState::Done));
        assert!(!needs_attention(AgentState::Working));
        assert!(!needs_attention(AgentState::Idle));
    }

    #[test]
    fn the_count_is_what_the_tab_bar_has_room_for() {
        let statuses = [
            status(1, "a", AgentState::WaitingForUser),
            status(2, "b", AgentState::Working),
            status(3, "c", AgentState::Done),
        ];
        assert_eq!(attention_count(&statuses), 2);
    }

    #[test]
    fn an_empty_cockpit_has_no_rows_and_wants_nothing() {
        assert!(rows(&[], |_| 0).is_empty());
        assert_eq!(attention_count(&[]), 0);
    }

    #[test]
    fn a_task_hint_is_carried_through_when_the_agent_gave_one() {
        let mut status = status(1, "claude", AgentState::Working);
        status.task_hint = Some("refactoring the parser".to_string());
        let rows = rows(&[status], |_| 0);
        assert_eq!(rows[0].hint, "refactoring the parser");
    }

    #[test]
    fn inbox_selection_wraps_and_survives_a_shorter_list() {
        assert_eq!(step_selection(0, 3, false), 2);
        assert_eq!(step_selection(2, 3, true), 0);
        assert_eq!(step_selection(99, 2, false), 0);
        assert_eq!(step_selection(4, 0, true), 0);
    }

    #[test]
    fn cross_instance_rows_keep_window_and_tab_location() {
        let rows = located_rows(&[LocatedStatus {
            pane_id: 7,
            instance_id: "bravo".to_string(),
            window_title: "payments".to_string(),
            tab_id: Some(3),
            agent: "codex".to_string(),
            state: AgentState::WaitingForUser,
            age_seconds: 90,
            task_hint: Some("fix checkout".to_string()),
        }]);

        assert_eq!(rows[0].instance_id, "bravo");
        assert_eq!(rows[0].window_title, "payments");
        assert_eq!(rows[0].tab_id, Some(3));
        assert_eq!(rows[0].hint, "fix checkout");
        assert!(rows[0].needs_you);
    }
}

/// How a pane's agent shows on its tab.
///
/// A dot, because a tab is three characters wide and a word does not fit. The
/// colours are the ones the product has always used and people read without
/// being told: amber means it wants you, blue means it is working, green means
/// it finished. Idle is nothing at all -- a marker on every tab is a marker
/// that says nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    NeedsYou,
    Working,
    Done,
}

impl Badge {
    /// The colour, as the renderer wants it.
    pub fn color(self) -> [f32; 4] {
        match self {
            // Amber: the one colour on the bar that should pull an eye.
            Badge::NeedsYou => [0.98, 0.70, 0.20, 1.0],
            // Muted blue-grey: working is the normal state, and a
            // normal state must not compete with the amber that asks.
            Badge::Working => [0.49, 0.56, 0.63, 1.0],
            Badge::Done => [0.35, 0.80, 0.45, 1.0],
        }
    }

    /// The one mark a sidebar row shows for this state. A raised hand
    /// asks, a spinner works, a tick finished — the row's whole story
    /// in a single glyph, which is what lets twenty rows be read as a
    /// glance instead of a paragraph. Working animates, so it takes
    /// the current spinner phase.
    pub fn glyph(self, spin: u8) -> &'static str {
        match self {
            Badge::NeedsYou => "\u{270B}",
            Badge::Working => {
                crate::sidebar::SPIN_GLYPHS[(spin as usize) % crate::sidebar::SPIN_GLYPHS.len()]
            }
            Badge::Done => "\u{2713}",
        }
    }
}

/// The badge for a state, if it has one.
pub fn badge(state: AgentState) -> Option<Badge> {
    match state {
        AgentState::WaitingForUser => Some(Badge::NeedsYou),
        AgentState::Working => Some(Badge::Working),
        AgentState::Done => Some(Badge::Done),
        AgentState::Idle => None,
    }
}

/// The badge for a pane, given every pane's status.
pub fn badge_for_pane(statuses: &[PaneAgentStatus], pane_id: u64) -> Option<Badge> {
    statuses
        .iter()
        .find(|status| status.pane_id == pane_id)
        .and_then(|status| badge(status.state))
}

#[cfg(test)]
mod badge_tests {
    use super::*;

    #[test]
    fn every_state_that_means_something_has_a_badge() {
        assert_eq!(badge(AgentState::WaitingForUser), Some(Badge::NeedsYou));
        assert_eq!(badge(AgentState::Working), Some(Badge::Working));
        assert_eq!(badge(AgentState::Done), Some(Badge::Done));
    }

    /// A marker on every tab is a marker that says nothing.
    #[test]
    fn an_idle_pane_is_not_marked() {
        assert_eq!(badge(AgentState::Idle), None);
    }

    /// Three states, three colours. Two that matched would be a badge that
    /// cannot be read without clicking the tab, which is the trip it exists
    /// to save.
    #[test]
    fn the_three_badges_are_three_different_colours() {
        let colours = [
            Badge::NeedsYou.color(),
            Badge::Working.color(),
            Badge::Done.color(),
        ];
        for (index, colour) in colours.iter().enumerate() {
            for other in &colours[index + 1..] {
                assert_ne!(colour, other, "two badges share a colour");
            }
        }
    }

    /// Amber is the one that has to pull an eye across the window: more red
    /// and green than blue, and brighter than the others.
    #[test]
    fn the_one_that_wants_you_is_the_warm_one() {
        let needs_you = Badge::NeedsYou.color();
        assert!(needs_you[0] > needs_you[2], "amber is warm: {needs_you:?}");
        assert!(needs_you[1] > needs_you[2], "{needs_you:?}");
    }

    #[test]
    fn a_pane_nobody_is_tracking_has_no_badge() {
        assert_eq!(badge_for_pane(&[], 7), None);
    }
}
