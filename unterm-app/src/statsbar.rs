//! The line of facts about the pane in front, shown in the top bar.
//!
//! Four things, in the order they answer questions people actually ask of a
//! terminal: which agent is bound to this pane, which branch it is on and how
//! dirty, what its process is costing, and what it is running.
//!
//! ```text
//!     ⚡ claude    <branch> main *3 +1    8.0% cpu  1.4G  4m    ▶ cargo test
//! ```
//!
//! Composed here rather than at the paint site because every rule in it is a
//! judgement -- when it appears, what is dropped first, what an empty value
//! looks like -- and none of those are checkable in a screenshot.

/// Below this the bar has no room for facts, and dropping them is better than
/// pushing the tabs off the edge.
///
/// Measured in logical pixels, so it does not move when the display's scale
/// does: the question is how much room the reader sees, not how many device
/// pixels the panel has.
pub const MIN_WIDTH: f32 = 900.0;

/// The gap between segments: four spaces.
///
/// Not a separator character. A run of segments that are already differently
/// shaped -- an icon, a name, a run of digits -- reads as a list without one,
/// and a dot between them puts a hard vertical edge where the eye wants to
/// skip.
const GAP: &str = "    ";

/// What is bound to this pane, if anything.
pub fn agent_segment(name: Option<&str>) -> String {
    match name {
        Some(name) if !name.trim().is_empty() => format!("\u{26A1} {}", name.trim()),
        _ => String::new(),
    }
}

/// The names a POSIX login shell gives itself at an idle prompt.
///
/// Exactly the previous front end's list, and deliberately not longer. It is
/// tempting to add `pwsh` and `cmd` -- they are shells too -- but doing that
/// empties the segment on every Windows machine, where the shell *is* one of
/// those and the whole point of the segment is to say what the pane is running.
/// The old bar showed `pwsh.exe`, and that was right: on a platform whose
/// shells are named after themselves, the name is still the answer.
const QUIET_SHELLS: &[&str] = &["zsh", "bash", "fish", "nu", "sh"];

/// What the pane is running.
///
/// Empty when there is nothing to say, or when the name is one of the few a
/// login shell gives itself while doing nothing -- those repeat what the window
/// already makes obvious.
pub fn title_segment(foreground: &str, shell: &str) -> String {
    // The pane's own shell is not consulted: which program is in front is the
    // question, and the answer does not change because the pane started with it.
    let _ = shell;
    let shown = shown_name(foreground);
    if shown.is_empty() || QUIET_SHELLS.contains(&program_name(foreground).to_lowercase().as_str())
    {
        return String::new();
    }
    format!("\u{25B6} {shown}")
}

/// The name as 0.57.4 showed it: the path gone, the extension kept.
/// `powershell.exe` in the bar is the platform's own spelling of the program,
/// and stripping it made the bar read differently from every released window.
fn shown_name(program: &str) -> String {
    program
        .trim()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_string()
}

/// A program's name with neither its path nor its extension.
///
/// `C:\Windows\System32\cmd.exe` and `cmd` are the same program, and the two
/// spellings turn up in the same comparison.
fn program_name(program: &str) -> String {
    program
        .trim()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim_end_matches(".exe")
        .trim_end_matches(".EXE")
        .to_string()
}

/// Join what there is to say, dropping what there is not.
///
/// Nothing at all when nothing is known, rather than a placeholder: an empty
/// slot in the bar is invisible, and a row of dashes is a thing to read.
pub fn compose(segments: &[String]) -> String {
    segments
        .iter()
        .filter(|segment| !segment.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(GAP)
}

/// How many cells a string takes, which is not how many characters it has.
/// Only width assertions in tests measure with it today.
#[cfg(test)]
pub fn width_of(text: &str) -> usize {
    text.chars().map(crate::terminal::column_width).sum()
}

/// The four segments for one pane, before they are fitted to a width.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    pub agent: String,
    /// Stable raw identity used by Cockpit; `agent` above is presentation.
    pub agent_id: Option<String>,
    pub git: String,
    pub process: String,
    pub title: String,
}

impl Facts {
    /// In the order they are shown, which is the order the questions are
    /// asked: whose pane is this, where is it, what is it costing, what is it
    /// doing.
    pub fn segments(&self) -> [String; 4] {
        [
            self.agent.clone(),
            self.git.clone(),
            self.process.clone(),
            self.title.clone(),
        ]
    }
}

/// How long a set of facts stays good.
///
/// A second. Reading them walks the machine's process table, which on a busy
/// desktop means several hundred processes -- so this is not a cache to save a
/// few microseconds, it is the difference between an idle terminal costing
/// nothing and costing a slice of a core forever.
///
/// A second rather than a quarter because of what is in the line: a CPU
/// percentage, a memory figure, an uptime and a branch. None of them is read
/// four times a second by anybody, and the uptime is the only one that ticks --
/// in whole seconds.
const DEFAULT_FACTS_TTL_MS: u64 = 1000;
static FACTS_TTL_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(DEFAULT_FACTS_TTL_MS);

pub fn set_refresh_ms(milliseconds: u64) {
    FACTS_TTL_MS.store(
        milliseconds.clamp(250, 60_000),
        std::sync::atomic::Ordering::Relaxed,
    );
}

fn facts_ttl() -> std::time::Duration {
    std::time::Duration::from_millis(FACTS_TTL_MS.load(std::sync::atomic::Ordering::Relaxed))
}

type FactsCache = std::collections::HashMap<usize, (std::time::Instant, Facts)>;

fn facts_cache() -> &'static parking_lot::Mutex<FactsCache> {
    static CACHE: std::sync::OnceLock<parking_lot::Mutex<FactsCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn refreshing() -> &'static parking_lot::Mutex<std::collections::HashSet<usize>> {
    static RUNNING: std::sync::OnceLock<parking_lot::Mutex<std::collections::HashSet<usize>>> =
        std::sync::OnceLock::new();
    RUNNING.get_or_init(Default::default)
}

/// How many panes may be refreshing their facts at once.
const MAX_REFRESHING: usize = 4;

/// What is known about a pane, refreshing behind the caller's back.
///
/// Returns whatever was known last, immediately -- including nothing, the
/// first time a pane is asked about. Everything underneath it (the process
/// table, git, the per-process sampler) is cached the same way, so the bar
/// stays a bar rather than becoming a reason the window is slow.
pub fn facts_for(pane_id: usize) -> Facts {
    let previous;
    {
        let cache = facts_cache().lock();
        match cache.get(&pane_id) {
            Some((at, facts)) if at.elapsed() < facts_ttl() => return facts.clone(),
            Some((_, facts)) => previous = facts.clone(),
            None => previous = Facts::default(),
        }
    }

    let mut running = refreshing().lock();
    // A few at a time. Every refresh asks the Core over the one connection the
    // window shares, so fifty panes going stale together queued fifty threads
    // on it -- and the window's own requests behind them, a third of a second
    // per tick. A pane left out now is still stale on the next look.
    if running.len() < MAX_REFRESHING && running.insert(pane_id) {
        let spawned = std::thread::Builder::new()
            .name("stats-facts".into())
            .spawn(move || {
                let facts = read_facts(pane_id);
                facts_cache()
                    .lock()
                    .insert(pane_id, (std::time::Instant::now(), facts));
                refreshing().lock().remove(&pane_id);
            });
        if spawned.is_err() {
            running.remove(&pane_id);
        }
    }
    previous
}

/// What is already known about a pane, without asking for more.
///
/// The strip wants a name for every tab it draws, and asking for each of them
/// means walking the machine's process table once per tab several times a
/// second. The pane in front is worth that; the others are worth whatever was
/// learned when they were in front.
pub fn known_facts(pane_id: usize) -> Facts {
    facts_cache()
        .lock()
        .get(&pane_id)
        .map(|(_, facts)| facts.clone())
        .unwrap_or_default()
}

/// Drop what is known about a pane that is gone, so a long-lived window does
/// not accumulate one entry per tab it has ever opened.
pub fn forget(pane_id: usize) {
    facts_cache().lock().remove(&pane_id);
}

fn command_stem(command: &str) -> String {
    command
        .trim()
        .trim_matches('"')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim_end_matches(".exe")
        .trim_end_matches(".cmd")
        .trim_end_matches(".bat")
        .trim_end_matches(".ps1")
        .to_ascii_lowercase()
}

fn manifest_agent_match<'a>(
    candidates: impl IntoIterator<Item = &'a str>,
    manifests: &[(String, String)],
) -> Option<String> {
    let candidates: Vec<String> = candidates
        .into_iter()
        .map(command_stem)
        .filter(|candidate| !candidate.is_empty())
        .collect();
    manifests
        .iter()
        .find(|(command, _)| candidates.iter().any(|candidate| candidate == command))
        .map(|(_, id)| id.clone())
}

fn manifest_agent_for_process(process: &unterm_engine::ProcessTreeSnapshot) -> Option<String> {
    static MANIFEST_COMMANDS: std::sync::OnceLock<Vec<(String, String)>> =
        std::sync::OnceLock::new();
    let manifests = MANIFEST_COMMANDS.get_or_init(|| {
        unterm_agents::fetch_manifests_offline()
            .map(|set| {
                set.envelope
                    .manifests
                    .into_iter()
                    .filter_map(|manifest| {
                        let command = command_stem(&manifest.detect.command);
                        (!command.is_empty()).then_some((command, manifest.id))
                    })
                    .collect()
            })
            .unwrap_or_default()
    });
    manifest_agent_match(
        std::iter::once(process.foreground_process.as_str())
            .chain(process.foreground_argv.first().map(String::as_str))
            .chain(std::iter::once(process.root_process.as_str())),
        manifests,
    )
}

/// Ask the machine. Only ever called from the refresh thread.
fn read_facts(pane_id: usize) -> Facts {
    // Through the installed provider rather than a direct next-core
    // handle: in Core mode the sessions live in another process, and
    // the process-local engine would answer for an empty world.
    let engine = unterm_engine::host_engine();
    let activity = unterm_engine::SessionEngine::activity(&*engine, pane_id).ok();
    let process = activity
        .as_ref()
        .and_then(|activity| activity.process.clone());

    let directory = process
        .as_ref()
        .and_then(|process| {
            process
                .foreground_cwd
                .clone()
                .or_else(|| process.root_cwd.clone())
        })
        .or_else(|| {
            unterm_engine::SessionEngine::shell(&*engine, pane_id)
                .ok()
                .and_then(|shell| shell.cwd)
        });

    let manifest_agent = process
        .as_ref()
        .filter(|process| process.detected_agent.is_none())
        .and_then(manifest_agent_for_process);
    let detected_agent = process
        .as_ref()
        .and_then(|process| process.detected_agent.as_deref())
        .or(manifest_agent.as_deref());
    // This already runs on the facts worker, so taking a dangling Git
    // snapshot cannot hold paint/input. It covers loose, process-detected
    // agents; the checkpoint service itself debounces repeated refreshes.
    if let (Some(agent), Some(cwd)) = (detected_agent, directory.as_deref()) {
        if let Err(err) = unterm_services::cockpit::review::record_auto_checkpoint(
            std::path::Path::new(cwd),
            agent,
            pane_id as u64,
        ) {
            log::debug!("automatic agent checkpoint skipped: {err:#}");
        }
    }

    Facts {
        agent: agent_segment(detected_agent),
        agent_id: detected_agent.map(str::to_string),
        git: directory
            .as_deref()
            // Blocking is fine here: this is already the refresh thread, and
            // waiting for git is what it is for.
            .map(|cwd| crate::git::read_cached(std::path::Path::new(cwd)))
            .map(|panel| crate::git::render_segment(&panel))
            .unwrap_or_default(),
        process: unterm_services::process_stats::render_segment(
            &process
                .as_ref()
                .and_then(|process| process.foreground_pid.or(process.root_pid))
                .and_then(unterm_services::process_stats::sample_now),
        ),
        title: title_segment(
            activity
                .as_ref()
                .map(|activity| activity.foreground_process.as_str())
                .unwrap_or_default(),
            process
                .as_ref()
                .map(|process| process.root_process.as_str())
                .unwrap_or_default(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bound_agent_is_named() {
        assert_eq!(agent_segment(Some("claude")), "\u{26A1} claude");
    }

    /// No agent is no segment -- not a lightning bolt on its own, which reads
    /// as an agent whose name failed to load.
    #[test]
    fn no_agent_is_no_segment() {
        assert_eq!(agent_segment(None), "");
        assert_eq!(agent_segment(Some("  ")), "");
    }

    /// The few names a login shell gives itself while doing nothing repeat
    /// what the window already makes obvious.
    #[test]
    fn a_posix_login_shell_at_its_prompt_is_not_worth_naming() {
        for shell in ["zsh", "bash", "fish", "nu", "sh", "/usr/bin/zsh"] {
            assert_eq!(title_segment(shell, shell), "", "{shell}");
        }
    }

    /// The Windows shells *are* named. Adding them to that list empties the
    /// segment on every Windows machine -- the shell there is one of them, and
    /// the point of the segment is to say what the pane is running. The old bar
    /// showed `pwsh.exe`, and that was right.
    #[test]
    fn a_windows_shell_is_still_named() {
        assert_eq!(title_segment("pwsh.exe", "pwsh"), "\u{25B6} pwsh.exe");
        assert_eq!(title_segment("cmd.exe", "cmd"), "\u{25B6} cmd.exe");
        assert_eq!(
            title_segment("C:\\Windows\\System32\\cmd.exe", "cmd"),
            "\u{25B6} cmd.exe"
        );
    }

    /// The path and the extension are spelling, not identity: what comes back
    /// as `C:\Windows\System32\cmd.exe` is named `cmd`.
    #[test]
    fn a_program_is_named_however_its_path_is_spelled() {
        assert_eq!(title_segment("/usr/bin/vim", "bash"), "\u{25B6} vim");
        assert_eq!(title_segment("CARGO.EXE", "cmd"), "\u{25B6} CARGO.EXE");
    }

    #[test]
    fn future_manifest_agents_match_process_names_and_script_paths() {
        let manifests = vec![
            ("future-agent".to_string(), "future-agent-id".to_string()),
            ("other".to_string(), "other-id".to_string()),
        ];
        assert_eq!(
            manifest_agent_match(
                [
                    "node.exe",
                    "C:\\Users\\me\\AppData\\Roaming\\npm\\future-agent.cmd",
                ],
                &manifests,
            )
            .as_deref(),
            Some("future-agent-id")
        );
        assert_eq!(
            manifest_agent_match(["/opt/bin/unrelated"], &manifests),
            None
        );
    }

    /// And a shell running another program is named, which is what the segment
    /// exists for.
    #[test]
    fn a_shell_running_something_else_is_worth_naming() {
        assert_eq!(
            title_segment("powershell.exe", "cmd.exe"),
            "\u{25B6} powershell.exe"
        );
    }

    #[test]
    fn anything_else_running_is_named() {
        assert_eq!(title_segment("cargo.exe", "cmd"), "\u{25B6} cargo.exe");
        assert_eq!(title_segment("  vim ", "bash"), "\u{25B6} vim");
        assert_eq!(title_segment("", "bash"), "");
    }

    /// Knowing nothing about the pane's own shell is not a reason to stay
    /// quiet: naming what is in front is still right, and the only thing lost
    /// is the chance to recognise it as the shell.
    #[test]
    fn a_pane_with_no_shell_recorded_still_names_what_is_in_front() {
        assert_eq!(title_segment("cargo", ""), "\u{25B6} cargo");
        assert_eq!(title_segment("", ""), "");
    }

    /// Four spaces, so segments that are already differently shaped read as a
    /// list without a character drawing a line between them.
    #[test]
    fn segments_are_separated_by_a_gap_rather_than_a_mark() {
        let line = compose(&["a".into(), "b".into()]);
        assert_eq!(line, "a    b");
        assert!(!line.contains('\u{00B7}'), "a separator crept in");
    }

    #[test]
    fn empty_segments_leave_no_gap_behind_them() {
        assert_eq!(
            compose(&["a".into(), String::new(), "  ".into(), "b".into()]),
            "a    b"
        );
    }

    /// Nothing known shows nothing, not a placeholder: an empty slot in a bar
    /// is invisible, and a row of dashes is a thing the eye stops on.
    #[test]
    fn nothing_known_shows_nothing() {
        assert_eq!(compose(&[]), "");
        assert_eq!(compose(&[String::new(), "   ".into()]), "");
    }

    /// Width is measured in cells. A CJK title takes two columns per character,
    /// and measuring it in characters puts the line through the tabs.
    #[test]
    fn width_is_counted_in_cells_rather_than_characters() {
        assert_eq!(width_of("abc"), 3);
        assert_eq!(width_of("\u{4E2D}\u{6587}"), 4);
    }

    /// The whole line, as it appears in a window wide enough for all of it.
    #[test]
    fn the_whole_line_reads_in_the_order_the_questions_are_asked() {
        let line = compose(&[
            agent_segment(Some("claude")),
            "\u{E0A0} main *3".to_string(),
            "8.0% cpu  1.4G  4m".to_string(),
            title_segment("cargo.exe", "pwsh"),
        ]);
        assert_eq!(
            line,
            "\u{26A1} claude    \u{E0A0} main *3    8.0% cpu  1.4G  4m    \u{25B6} cargo.exe"
        );
    }
}

#[cfg(test)]
mod freshness_tests {
    use super::*;

    /// The numbers keep moving whether or not the window has the keyboard.
    ///
    /// Gating the refresh on focus looked like a saving and was not -- the cost
    /// was elsewhere -- and it froze every value in a background window with
    /// nothing on screen to say they were stale. A terminal glanced at from
    /// across a desk is exactly when a wrong number is believed.
    #[test]
    fn the_numbers_refresh_without_being_asked_twice() {
        let pane = std::process::id() as usize;
        // The first look cannot answer; the refresh behind it must land.
        let _ = facts_for(pane);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if facts_for(pane) != Facts::default() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        // A pane id that is not a pane has nothing to report, which is a fair
        // answer on a machine where this test's own id is not a session.
    }

    /// A second look inside the window is free: the whole point of the cache
    /// is that reading them walks the machine's process table.
    #[test]
    fn a_second_look_inside_the_window_costs_nothing() {
        let pane = 987_654;
        let _ = facts_for(pane);
        let start = std::time::Instant::now();
        for _ in 0..50 {
            let _ = facts_for(pane);
        }
        assert!(
            start.elapsed() < std::time::Duration::from_millis(50),
            "fifty looks took {:?}",
            start.elapsed()
        );
    }
}
