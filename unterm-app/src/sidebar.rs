//! The list of tabs down the left-hand side.
//!
//! Ported from the previous front end's `left_tab_bar.rs`. A vertical strip
//! reads a tab list better than a horizontal one does: a tab's label is a
//! path and a command, which fit along a row and not across one, and there is
//! room for the state a top strip has to leave out.
//!
//! Tabs are grouped by the directory their pane is in, and the grouping is
//! derived rather than configured -- the point being that a person running
//! three projects gets three groups without setting anything up. Two projects
//! whose folders share a name are told apart by the shortest parent path that
//! distinguishes them, which is the fiddly part and the reason that algorithm
//! came across verbatim rather than being re-derived.

use std::collections::HashMap;

/// The strip's width in pixels, or nothing when it is closed.
///
/// Measured in points against the display rather than in terminal cells: the
/// strip is chrome, and a panel that changes width when somebody zooms the
/// terminal is a panel that moves for no reason.
pub fn width(
    open: bool,
    requested_points: Option<f32>,
    tab_count: usize,
    window_width: f32,
    scale: f32,
) -> f32 {
    if !open {
        return 0.0;
    }
    let pt = crate::chrome_font::point(scale);
    let window_points = window_width / pt.max(0.001);
    let widest = (window_points * crate::ui_tokens::LEFT_TAB_BAR_MAX_RATIO)
        .max(crate::ui_tokens::LEFT_TAB_BAR_MIN_WIDTH);
    let points = requested_points
        .unwrap_or_else(|| adaptive_default_width(tab_count))
        .clamp(crate::ui_tokens::LEFT_TAB_BAR_MIN_WIDTH, widest);
    (points * pt).round()
}

/// The previous front end kept a one-tab window compact and only spent the
/// full sidebar width once the list actually needed it.
pub fn adaptive_default_width(_tab_count: usize) -> f32 {
    // Two lines a row -- task, then where and which branch -- need the width
    // at any count; a strip that grows when a second tab opens moves the
    // terminal under the reader for no reason.
    crate::ui_tokens::LEFT_TAB_BAR_WIDTH
}

/// The icon a tab row leads with.
///
/// Ported unchanged from the previous front end, code points included. They
/// come from the bundled symbols face -- a robot for an AI agent's pane, the
/// platform's own terminal mark for each shell -- and they are what makes a row
/// identifiable before its text is read.
pub fn shell_icon(title: &str) -> char {
    let lower = title.to_lowercase();
    let has = |name: &str| lower.contains(name);
    if let Some(mark) = agent_icon(&lower) {
        return mark;
    }
    if has("pwsh") || has("powershell") {
        '\u{ebc7}'
    } else if has("cmd.exe") || has("command prompt") || has("cmd") {
        '\u{ebc4}'
    } else if has("bash") {
        '\u{ebca}'
    } else if has("ssh") {
        '\u{eb3a}'
    } else {
        // A generic terminal beats an empty leading slot: a row with no icon
        // reads as a row whose icon failed to load.
        '\u{ea85}'
    }
}

/// The mark for the agent running in a pane, if one is.
///
/// One per agent rather than one robot for all of them: with several
/// agents open at once, the row's own mark is what says *which* is
/// waiting, before any of its text is read. The shapes follow each
/// tool's own sign where it has one -- Claude's asterisk, Gemini's
/// four-pointed sparkle -- and are plain geometry otherwise. Every one
/// of them is guarded by `chrome_font`'s reachability tests: a mark no
/// face carries draws nothing at all, which reads as a broken row.
pub fn agent_icon(lower_title: &str) -> Option<char> {
    let has = |name: &str| lower_title.contains(name);
    Some(if has("claude") {
        '\u{273B}'
    } else if has("codex") {
        '\u{F02D8}'
    } else if has("gemini") {
        '\u{2726}'
    } else if has("aider") {
        '\u{270E}'
    } else if has("opencode") {
        '\u{25C7}'
    } else if has("kimi") {
        '\u{263E}'
    } else if has("cursor agent") {
        '\u{27A4}'
    } else if has("trae") || has("zcode") {
        // Known agents with no sign of their own: the generic robot
        // still says "an agent lives here", which is the point.
        ROBOT
    } else {
        return None;
    })
}

/// How wide one icon-only control in the strip's footer is.
///
/// Narrower than the row is tall: three controls share that row, and
/// the only one with words has to keep them. Drawing and hit-testing
/// both read it here so a press lands on what the eye sees.
pub fn footer_mark_width(row_height: f32) -> f32 {
    (row_height * 0.72).round().max(16.0)
}

/// An AI agent's pane, when the agent has no mark of its own.
pub const ROBOT: char = '\u{f06a9}';
/// The folder a project row leads with.
pub const FOLDER: char = '\u{f07b}';
/// The tab a press on the row at `at` is asking for.
///
/// A project row names a run of tabs rather than one, so going to the project
/// means going to the first tab under it. Returns `None` only when the press
/// was on the last project and it has no tabs to show -- folded ones are not
/// in `rows` at all, which is why the caller unfolds before asking.
pub fn tab_at_or_after(rows: &[Row], at: usize) -> Option<usize> {
    rows.iter().skip(at).find_map(|row| match row {
        Row::Tab { index, .. } => Some(*index),
        Row::Group { .. } => None,
    })
}

/// The disclosure arrows, closed and open.
pub const CLOSED: char = '\u{25B8}';
pub const OPEN: char = '\u{25BE}';

/// Shorten a label to `columns`, keeping its start.
///
/// A row's beginning is its name, which is what it is identified by; a project
/// called `unterm-experiments` cut from the front is unrecognisable.
pub fn fit(text: &str, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    let width: usize = text.chars().map(crate::terminal::column_width).sum();
    if width <= columns {
        return text.to_string();
    }
    let mut kept = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let wide = crate::terminal::column_width(ch);
        if used + wide > columns.saturating_sub(1) {
            break;
        }
        kept.push(ch);
        used += wide;
    }
    format!("{kept}\u{2026}")
}

/// What one line of the strip is.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// The project a run of tabs belongs to. Only when there is more than one.
    Group {
        key: String,
        label: String,
        /// The parent path that tells this project from another of the same
        /// name, when one is needed.
        hint: Option<String>,
        count: usize,
        /// Whether its tabs are folded away. Window state, not disk state: it
        /// survives repaints and resizes without touching the filesystem.
        collapsed: bool,
        /// Whether the tab in front belongs to it.
        active: bool,
    },
    Tab {
        index: usize,
        label: String,
        /// The second line: whose agent it is when the label is its task,
        /// where the tab is when no project header says so, and its branch.
        subtitle: Option<String>,
        /// What is running in it, if anything is.
        detail: Option<String>,
        active: bool,
        /// The icon it leads with.
        icon: char,
        /// Whether it sits under a project header, and is therefore indented.
        grouped: bool,
        /// What the agent in this pane wants, if anything. Right-aligned, and
        /// the reason it is here rather than on a top-bar tab: an agent waiting
        /// for an answer has to be visible from whichever tab you are in.
        badge: Option<crate::cockpit::Badge>,
        indicators: Indicators,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Indicators {
    pub unread: bool,
    pub running: bool,
    pub error: bool,
}

/// One tab, as the strip needs to know it.
#[derive(Clone, Debug, PartialEq)]
pub struct TabInfo {
    pub index: usize,
    pub title: String,
    /// What the agent in this pane wants, if anything.
    pub badge: Option<crate::cockpit::Badge>,
    /// The AI agent bound to the pane, if one is. The agent's name *is* the
    /// row's title when there is one -- the pane title just repeats it.
    pub agent: Option<String>,
    /// The full working directory, which is the project's identity: two
    /// folders called `app` in different places are two projects.
    pub cwd: Option<String>,
    /// The command running in front of the shell, if one is.
    pub foreground: Option<String>,
    /// What the agent in this pane says it is doing -- the title Claude Code
    /// and its peers set on the terminal -- when it is more than its name.
    pub task: Option<String>,
    /// The git branch the pane is on, when it is known yet.
    pub branch: Option<String>,
    pub active: bool,
    pub indicators: Indicators,
}

/// Build the strip's lines.
pub fn rows(tabs: &[TabInfo], collapsed: &std::collections::HashSet<String>) -> Vec<Row> {
    let mut rows = Vec::new();

    // Rows hold their positions no matter what the agents inside them do.
    // A pinned "waiting for you" section used to float such tabs to the
    // top — and every agent state flip teleported rows under the pointer,
    // which read as "the tabs run around and I cannot click them". The
    // badge alone carries the state now; the strip stays still.
    let rest: Vec<&TabInfo> = tabs.iter().collect();

    let projects: Vec<(String, String)> = {
        let mut seen = Vec::new();
        for tab in &rest {
            let Some(cwd) = tab.cwd.as_deref() else {
                continue;
            };
            let key = project_key(cwd);
            if !seen.iter().any(|(existing, _)| *existing == key) {
                seen.push((key, cwd.to_string()));
            }
        }
        seen
    };
    let hints = shortest_unique_parent_hints(&projects);

    // One project needs no headers: a header above every tab is a header that
    // says nothing.
    let grouped = projects.len() > 1;

    if !grouped {
        rows.extend(rest.iter().map(|tab| row_for(tab, grouped)));
        return rows;
    }

    // Tabs are opened over time, so a project's are seldom a run: alpha, then
    // beta, then alpha again. Emitting each header where its project first
    // appeared and nowhere else left the later runs under whichever header
    // came before them -- eleven tabs named by a header with six of them
    // beneath it, and the five elsewhere reading as another project's. The
    // buckets keep the order the projects first appeared in, and the tabs
    // inside one keep theirs, so nothing moves except the tabs that were
    // under the wrong name.
    let mut buckets: Vec<(Option<String>, Vec<&TabInfo>)> = Vec::new();
    for tab in &rest {
        let key = tab.cwd.as_deref().map(project_key);
        match buckets.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, members)) => members.push(tab),
            None => buckets.push((key, vec![tab])),
        }
    }

    for (key, members) in &buckets {
        // Tabs with no directory have no project to be filed under, and get
        // no header of their own -- a header reading "no project" names
        // nothing.
        // A header over a single tab says the tab's own name twice. Only a
        // project with several tabs gets one; a lone tab carries its folder
        // and branch itself.
        if members.len() == 1 && !key.as_ref().is_some_and(|key| collapsed.contains(key)) {
            rows.push(row_for(members[0], false));
            continue;
        }
        if let Some(key) = key {
            rows.push(Row::Group {
                label: leaf(key),
                hint: hints.get(key).cloned(),
                count: members.len(),
                collapsed: collapsed.contains(key),
                active: members.iter().any(|tab| tab.active),
                key: key.clone(),
            });
        }
        // A collapsed project shows its header and nothing under it -- except
        // the tab in front, which must never disappear: a window with no
        // visible selection reads as a window that lost track of itself.
        let folded = key
            .as_ref()
            .map(|key| collapsed.contains(key))
            .unwrap_or(false);
        for tab in members {
            if folded && !tab.active {
                continue;
            }
            rows.push(row_for(tab, grouped));
        }
    }
    rows
}

/// One tab's line.
fn row_for(tab: &TabInfo, grouped: bool) -> Row {
    Row::Tab {
        index: tab.index,
        label: label_for(tab),
        subtitle: subtitle_for(tab, grouped),
        detail: tab.foreground.clone(),
        active: tab.active,
        icon: shell_icon(tab.agent.as_deref().unwrap_or(&tab.title)),
        grouped,
        badge: tab.badge,
        indicators: tab.indicators,
    }
}

/// The four-phase spinner a working agent's row turns. Quantised like
/// the breath before it: paint records the phase it drew, the idle
/// tick asks for a frame only when the phase moved on.
pub const SPIN_GLYPHS: [&str; 4] = ["\u{25D0}", "\u{25D3}", "\u{25D1}", "\u{25D2}"];
pub const SPIN_STEP_MS: u64 = 280;

pub fn spin_step(elapsed_ms: u64) -> u8 {
    ((elapsed_ms / SPIN_STEP_MS) % SPIN_GLYPHS.len() as u64) as u8
}

/// Cheap tail classifier for a background tab's sticky error indicator.
pub fn output_looks_like_error(lines: &[String]) -> bool {
    lines.iter().any(|line| {
        let line = line.trim().to_ascii_lowercase();
        if line.contains("0 failed") || line.contains("failed: 0") {
            return false;
        }
        [
            "error:",
            "error[",
            "fatal:",
            "panic",
            "traceback",
            "exception:",
            "test result: failed",
            "tests failed",
            "build failed",
        ]
        .iter()
        .any(|marker| line.contains(marker))
    })
}

/// What a tab's line says.
///
/// The command in front of the shell first: three shells in one project are
/// told apart by what they are doing, not by all being called the same thing.
fn label_for(tab: &TabInfo) -> String {
    use unterm_engine::next_core::tab_title::{resolve_name, TabContext, TabTitleRules};

    // What an agent is doing is what its row is about; its name goes to the
    // second line. Without a task, the name is the title.
    if let Some(task) = tab.task.as_deref().filter(|task| !task.trim().is_empty()) {
        return task.trim().to_string();
    }
    if let Some(agent) = tab.agent.as_deref().filter(|name| !name.trim().is_empty()) {
        return agent.to_string();
    }
    // A shell sitting at its prompt is known by where it is. Three rows all
    // reading "zsh" said nothing at all.
    if is_a_shell_name(&tab.title) {
        if let Some(cwd) = tab.cwd.as_deref() {
            let name = if is_home(cwd) { "~".to_string() } else { leaf(cwd) };
            if !name.is_empty() {
                return name;
            }
        }
    }

    let rules = TabTitleRules {
        capitalize: false,
        ..TabTitleRules::default()
    };
    let resolved = resolve_name(
        &rules,
        TabContext {
            pane_title: &tab.title,
            process_path: "",
            index: tab.index,
        },
    );
    if resolved.trim().is_empty() {
        format!("{}", tab.index + 1)
    } else {
        resolved
    }
}

/// A tab's second line, if it has anything to say.
fn subtitle_for(tab: &TabInfo, grouped: bool) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    // The label is the agent's task: name the agent here.
    if tab.task.as_deref().is_some_and(|task| !task.trim().is_empty()) {
        if let Some(agent) = tab.agent.as_deref().filter(|name| !name.trim().is_empty()) {
            parts.push(agent.trim().to_string());
        }
    }
    // Where it is -- unless a project header above already says, or the
    // label already is the folder.
    if !grouped {
        if let Some(cwd) = tab.cwd.as_deref() {
            let name = if is_home(cwd) { "~".to_string() } else { leaf(cwd) };
            if !name.is_empty() && name != label_for(tab) {
                parts.push(name);
            }
        }
    }
    if let Some(branch) = tab.branch.as_deref().filter(|branch| !branch.trim().is_empty()) {
        parts.push(format!("\u{e0a0} {}", branch.trim()));
    }
    (!parts.is_empty()).then(|| parts.join("  \u{00b7}  "))
}

/// Whether a tab title is only a shell's name.
fn is_a_shell_name(title: &str) -> bool {
    let stem = title
        .trim()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    matches!(
        stem.as_str(),
        "zsh" | "bash" | "sh" | "fish" | "nu" | "pwsh" | "powershell" | "cmd" | "tcsh" | "dash" | "elvish" | "xonsh" | "shell"
    )
}

/// The task an agent announced in its pane's title, if the title is one.
///
/// Claude Code and its peers set the terminal title to what they are working
/// on, led by a status glyph that changes as they work; and to their own name
/// when idle. Only the former is a task.
pub fn task_from_title(title: &str, agent: Option<&str>, program: &str) -> Option<String> {
    let trimmed = title
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_lowercase();
    let is_name = |name: &str| {
        let name = name.trim().to_lowercase();
        !name.is_empty() && (lower == name || lower == format!("{name} code"))
    };
    if agent.is_some_and(is_name)
        || is_name(program)
        || lower == "claude code"
        || is_a_shell_name(trimmed)
        || looks_like_a_path(title.trim())
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// A title that is a place rather than a piece of work: `/Users/me/work`,
/// `~/code`, `C:\\Windows\\system32\\cmd.exe`. A task may mention a path --
/// "Add rate limiting to /v1/charge" -- and is still a task.
fn looks_like_a_path(title: &str) -> bool {
    let bytes = title.as_bytes();
    title.starts_with('/')
        || title.starts_with('~')
        || (bytes.len() > 2 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/'))
        || (!title.contains(' ') && (title.contains('/') || title.contains('\\')))
}

/// A project's identity: its path, compared the way the platform compares
/// paths.
fn project_key(cwd: &str) -> String {
    let normalised = cwd.replace('\\', "/");
    let trimmed = normalised.trim_end_matches('/');
    if cfg!(windows) {
        trimmed.to_lowercase()
    } else {
        trimmed.to_string()
    }
}

/// What a project is called: the last part of the path it is in.
///
/// The same naming the strip uses, so the status bar and the strip never
/// disagree about which project a pane is in.
pub fn project_name(cwd: &str) -> String {
    if cwd.trim().is_empty() {
        return String::new();
    }
    leaf(&project_key(cwd))
}

/// The last component of a path -- what a project is called.
fn leaf(path: &str) -> String {
    // Home is "Home", not whatever the account is called. It is the one project
    // everybody has and the one whose folder name says nothing about it: a
    // header reading `lixd2` names the machine's owner rather than the project.
    if is_home(path) {
        return "Home".to_string();
    }
    path.replace('\\', "/")
        .trim_end_matches('/')
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// Whether a path *is* the home directory, rather than something inside it.
fn is_home(path: &str) -> bool {
    let Some(home) = dirs_next::home_dir() else {
        return false;
    };
    let tidy = |text: &str| text.replace('\\', "/").trim_end_matches('/').to_lowercase();
    tidy(path) == tidy(&home.display().to_string())
}

/// The shortest parent path that tells same-named projects apart.
///
/// Ported whole. Two folders called `app` need something in front of them or
/// the list has two identical headers; the shortest suffix that is unique is
/// the least noise that does the job. When no suffix is unique -- `/acme/app`
/// against `/work/acme/app`, where one path's components are a suffix of the
/// other's -- the immediate parent is used, so the pair still reads
/// differently even though neither is strictly unique.
fn shortest_unique_parent_hints(projects: &[(String, String)]) -> HashMap<String, String> {
    let components = |path: &str| {
        path.replace('\\', "/")
            .trim_end_matches('/')
            .split('/')
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let comparable = |value: &str| {
        if cfg!(windows) {
            value.to_lowercase()
        } else {
            value.to_string()
        }
    };
    let mut result = HashMap::new();

    for (key, path) in projects {
        let parts = components(path);
        let Some(leaf) = parts.last() else { continue };
        let peers: Vec<Vec<String>> = projects
            .iter()
            .filter_map(|(_, peer_path)| {
                let peer = components(peer_path);
                peer.last()
                    .is_some_and(|name| comparable(name) == comparable(leaf))
                    .then_some(peer)
            })
            .collect();
        if peers.len() < 2 || parts.len() < 2 {
            continue;
        }

        let mut disambiguated = false;
        for suffix_len in 2..=parts.len() {
            let suffix = parts[parts.len() - suffix_len..].join("/");
            let matches = peers
                .iter()
                .filter(|peer| {
                    peer.len() >= suffix_len
                        && comparable(&peer[peer.len() - suffix_len..].join("/"))
                            == comparable(&suffix)
                })
                .count();
            if matches == 1 {
                result.insert(
                    key.clone(),
                    parts[parts.len() - suffix_len..parts.len() - 1].join("/"),
                );
                disambiguated = true;
                break;
            }
        }
        if !disambiguated {
            result.insert(key.clone(), parts[parts.len() - 2].clone());
        }
    }
    result
}

/// How many of the strip's lines fit, and which one is at the top.
///
/// Clamped so a list that shrinks -- a tab closed, a group collapsed -- cannot
/// leave the strip scrolled past its own end showing nothing.
/// Clicks on one strip row, counted so only a true same-row double-click
/// renames. Terminal-pane clicks keep their own counter: a click in the pane
/// and a click on the row are different gestures even when they come fast.
#[derive(Clone, Debug)]
pub struct RowClick {
    row: usize,
    x: f32,
    y: f32,
    at: std::time::Instant,
    streak: usize,
}

impl RowClick {
    const DOUBLE_CLICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
    const DOUBLE_CLICK_SLOP_PT: f32 = 8.0;

    pub fn first(row: usize, x: f32, y: f32) -> Self {
        Self {
            row,
            x,
            y,
            at: std::time::Instant::now(),
            streak: 1,
        }
    }

    /// The next press: a continuation of the streak when it lands on the same
    /// row, nearby, and soon enough — a fresh first click otherwise.
    pub fn again(&self, row: usize, x: f32, y: f32) -> Self {
        let now = std::time::Instant::now();
        let same_target = row == self.row
            && (x - self.x).abs() <= Self::DOUBLE_CLICK_SLOP_PT
            && (y - self.y).abs() <= Self::DOUBLE_CLICK_SLOP_PT
            && now.duration_since(self.at) <= Self::DOUBLE_CLICK_INTERVAL;
        Self {
            row,
            x,
            y,
            at: now,
            streak: if same_target { self.streak + 1 } else { 1 },
        }
    }

    pub fn streak(&self) -> usize {
        self.streak
    }
}

pub fn clamp_scroll(scroll_top: usize, rows: usize, visible: usize) -> usize {
    scroll_top.min(rows.saturating_sub(visible))
}

/// Where the strip sits after a wheel of `delta` rows.
///
/// Bounded here rather than at the painter. The painter has always clamped
/// what it draws, so a strip scrolled past its end looked right -- but the
/// number itself kept climbing, and every notch spent past the end had to be
/// spent again before the strip would move back. A dozen notches at the
/// bottom of a short list is a strip that ignores the wheel a dozen times.
/// The file tree beside it bounds its own scroll on the way in; this is the
/// same rule, in the one place that lacked it.
pub fn scroll_by(scroll_top: usize, delta: isize, rows: usize, visible: usize) -> usize {
    let last = rows.saturating_sub(visible.max(1)) as isize;
    (scroll_top as isize + delta).clamp(0, last.max(0)) as usize
}

/// Scroll far enough to bring `row` into view, moving as little as possible.
pub fn scroll_to_show(scroll_top: usize, row: usize, visible: usize) -> usize {
    if visible == 0 {
        return scroll_top;
    }
    if row < scroll_top {
        row
    } else if row >= scroll_top + visible {
        row + 1 - visible
    } else {
        scroll_top
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows with nothing folded away, which is what every test here wants.
    fn rows_of(tabs: &[TabInfo]) -> Vec<Row> {
        rows(tabs, &std::collections::HashSet::new())
    }

    /// What a row says, for the tests that assert on its text. The window
    /// draws the pieces separately -- the count has to float right -- so this
    /// is the tests' own joining rather than something the painter uses.
    fn text_of(row: &Row) -> String {
        match row {
            Row::Group {
                label, hint, count, ..
            } => match hint {
                Some(hint) => format!("{hint}/{label}  {count}"),
                None => format!("{label}  {count}"),
            },
            Row::Tab {
                index,
                label,
                detail,
                ..
            } => match detail {
                Some(detail) => format!(" {}  {label}  {detail}", index + 1),
                None => format!(" {}  {label}", index + 1),
            },
        }
    }

    fn tab(index: usize, title: &str, cwd: Option<&str>) -> TabInfo {
        TabInfo {
            index,
            title: title.to_string(),
            badge: None,
            agent: None,
            cwd: cwd.map(str::to_string),
            foreground: None,
            task: None,
            branch: None,
            active: index == 0,
            indicators: Indicators::default(),
        }
    }


    /// A project's tabs are all of them, wherever they were opened.
    ///
    /// Tabs are opened over time, so a project's are seldom a run in the
    /// strip: alpha, then beta, then alpha again. The header was emitted at
    /// the project's first tab and never again, so the second run of alpha
    /// tabs came out under beta's header -- a header naming two tabs with
    /// four rows beneath it, none of the last three its own.
    #[test]
    fn a_project_gathers_every_one_of_its_tabs() {
        let tabs = vec![
            tab(0, "one", Some("/work/alpha")),
            tab(1, "two", Some("/work/beta")),
            tab(2, "three", Some("/work/alpha")),
            tab(3, "four", Some("/work/beta")),
        ];
        let rows = rows_of(&tabs);
        // Every header owns the rows between it and the next header, and
        // says how many there are.
        let mut header: Option<(String, usize)> = None;
        let mut under = 0usize;
        let mut seen = Vec::new();
        for row in &rows {
            match row {
                Row::Group { key, count, .. } => {
                    if let Some((key, count)) = header.take() {
                        assert_eq!(count, under, "project {key} counts rows it has not got");
                    }
                    header = Some((key.clone(), *count));
                    under = 0;
                }
                Row::Tab { index, .. } => {
                    let owner = &header.as_ref().expect("a tab with no project above it").0;
                    assert_eq!(
                        project_key(tabs[*index].cwd.as_deref().unwrap()),
                        *owner,
                        "tab {index} sits under {owner}",
                    );
                    seen.push(*index);
                    under += 1;
                }
            }
        }
        if let Some((key, count)) = header {
            assert_eq!(count, under, "project {key} counts rows it has not got");
        }
        seen.sort();
        assert_eq!(seen, vec![0, 1, 2, 3], "every tab is somewhere in the strip");
    }

    /// The ninth row is the ninth tab, and so is the twentieth.
    ///
    /// This is the shape the strip is actually used in -- twenty-two tabs
    /// over seven projects -- and what went wrong in it: a row's position was
    /// handed to `select_tab`, which reads a number *key*, and nine and above
    /// mean "the last one" there. Every press past the eighth landed on the
    /// last tab, so a dozen rows all showed the one pane.
    #[test]
    fn a_row_past_the_ninth_leads_to_its_own_tab() {
        let projects = [
            "/work/unflick",
            "/work/story",
            "/work/xianxia",
            "/work/bypass",
            "/work/xmarket",
            "/work/song",
            "/work/liqingzhao",
        ];
        let tabs: Vec<TabInfo> = (0..22)
            .map(|index| tab(index, "powershell", Some(projects[index % projects.len()])))
            .collect();
        let rows = rows_of(&tabs);
        for (at, row) in rows.iter().enumerate() {
            if let Row::Tab { index, .. } = row {
                assert_eq!(
                    tab_at_or_after(&rows, at),
                    Some(*index),
                    "row {at} of the strip",
                );
            }
        }
        // No two rows lead to the same tab: that is the symptom itself.
        let mut led_to: Vec<usize> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Tab { index, .. } => Some(*index),
                Row::Group { .. } => None,
            })
            .collect();
        let count = led_to.len();
        led_to.sort();
        led_to.dedup();
        assert_eq!(led_to.len(), count, "two rows of the strip lead to one tab");
        assert_eq!(count, 22, "every tab has a row");
    }

    /// A press on a project row is asking for the project, and a project is
    /// reached through the tab under it.
    ///
    /// This is what a press on that row could not do before: it folded, and
    /// with one tab to a project -- the common shape -- folding hid the very
    /// tab being aimed at. Two presses put it back, so the strip read as a
    /// tab that would not switch.
    #[test]
    fn a_press_on_a_project_row_finds_the_tab_under_it() {
        let tabs = vec![
            tab(0, "one", Some("/work/alpha")),
            tab(1, "two", Some("/work/beta")),
            tab(2, "three", Some("/work/beta")),
        ];
        let rows = rows_of(&tabs);
        // Every project row leads to the first tab beneath it, and every tab
        // row leads to itself.
        for (at, row) in rows.iter().enumerate() {
            let found = tab_at_or_after(&rows, at);
            match row {
                Row::Tab { index, .. } => assert_eq!(found, Some(*index), "row {at}"),
                Row::Group { .. } => {
                    let next_tab = rows[at + 1..].iter().find_map(|row| match row {
                        Row::Tab { index, .. } => Some(*index),
                        Row::Group { .. } => None,
                    });
                    assert_eq!(found, next_tab, "row {at} is a project");
                    assert!(found.is_some(), "a project row led nowhere at {at}");
                }
            }
        }
    }

    /// Past the end there is nothing to go to, and saying so is better than
    /// picking the nearest thing.
    #[test]
    fn a_press_past_every_tab_asks_for_none() {
        let tabs = vec![tab(0, "one", Some("/work/alpha"))];
        let rows = rows_of(&tabs);
        assert_eq!(tab_at_or_after(&rows, rows.len()), None);
        assert_eq!(tab_at_or_after(&[], 0), None);
    }

    fn labels(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Group { label, hint, .. } => match hint {
                    Some(hint) => format!("[{hint}/{label}]"),
                    None => format!("[{label}]"),
                },
                Row::Tab { label, .. } => label.clone(),
            })
            .collect()
    }

    /// Every row fits the strip: one that runs past its width is drawn over
    /// the terminal beside it.
    #[test]
    fn no_row_is_wider_than_the_strip() {
        let rows = rows_of(&[
            TabInfo {
                index: 0,
                title: "a very long shell name that goes on".to_string(),
                badge: None,
                agent: None,
                cwd: Some("/work/some/deeply/nested/project".to_string()),
                foreground: Some("npm run dev --workspace=everything".to_string()),
                task: None,
                branch: None,
                active: true,
                indicators: Indicators::default(),
            },
            tab(1, "pwsh", Some("/elsewhere/project")),
        ]);
        // Twenty-two columns is about what the default strip holds.
        const ROOM: usize = 22;
        for row in &rows {
            let fitted = fit(&text_of(row), ROOM);
            let wide: usize = fitted.chars().map(crate::terminal::column_width).sum();
            assert!(wide <= ROOM, "{fitted:?} is {wide} wide");
        }
    }

    /// A tab's line leads with its number, so it can be found by the key that
    /// selects it.
    #[test]
    fn a_tabs_line_starts_with_its_number() {
        let rows = rows_of(&[tab(0, "pwsh", Some("/work/app"))]);
        let text = text_of(&rows[0]);
        assert!(text.trim_start().starts_with('1'), "{text:?}");
    }

    /// What is running comes after the name: three shells in one project are
    /// told apart by what they are doing.
    #[test]
    fn a_running_command_is_shown_beside_the_name() {
        let rows = rows_of(&[TabInfo {
            index: 0,
            title: "pwsh".to_string(),
            badge: None,
            agent: None,
            cwd: Some("/work/app".to_string()),
            foreground: Some("cargo test".to_string()),
            task: None,
            branch: None,
            active: true,
            indicators: Indicators::default(),
        }]);
        assert!(text_of(&rows[0]).contains("cargo test"));
    }

    #[test]
    fn shell_command_project_and_known_agent_are_visually_distinct() {
        // A shell at its prompt is known by where it is: three rows all
        // reading "pwsh" told nobody anything.
        let shell = rows_of(&[tab(0, "pwsh", Some("/work/app"))]);
        assert!(text_of(&shell[0]).contains("app"));
        assert!(!text_of(&shell[0]).contains("pwsh"));

        let command = rows_of(&[TabInfo {
            foreground: Some("cargo test".into()),
            task: None,
            branch: None,
            ..tab(0, "pwsh", Some("/work/app"))
        }]);
        assert!(text_of(&command[0]).contains("cargo test"));

        let projects = rows_of(&[
            tab(0, "pwsh", Some("/work/alpha")),
            tab(1, "pwsh", Some("/work/alpha")),
            tab(2, "pwsh", Some("/work/beta")),
        ]);
        assert!(labels(&projects).contains(&"[alpha]".to_string()));

        let agent = rows_of(&[TabInfo {
            agent: Some("codex".into()),
            ..tab(0, "pwsh", Some("/work/app"))
        }]);
        assert!(text_of(&agent[0]).contains("codex"));
        assert!(!text_of(&agent[0]).contains("pwsh"));
    }

    /// An agent flipping between working and waiting must not move its
    /// row: a strip that reshuffles under the pointer is a strip nobody
    /// can click. The badge changes, the geometry does not.
    #[test]
    fn agent_state_never_moves_a_row() {
        let calm = rows_of(&[
            tab(0, "pwsh", Some("/work/alpha")),
            tab(1, "claude", Some("/work/alpha")),
            tab(2, "codex", Some("/work/beta")),
            tab(3, "pwsh", Some("/work/beta")),
        ]);
        let waiting = rows_of(&[
            tab(0, "pwsh", Some("/work/alpha")),
            TabInfo {
                badge: Some(crate::cockpit::Badge::NeedsYou),
                ..tab(1, "claude", Some("/work/alpha"))
            },
            TabInfo {
                badge: Some(crate::cockpit::Badge::NeedsYou),
                ..tab(2, "codex", Some("/work/beta"))
            },
            tab(3, "pwsh", Some("/work/beta")),
        ]);
        let order = |rows: &[Row]| -> Vec<Option<usize>> {
            rows.iter()
                .map(|row| match row {
                    Row::Tab { index, .. } => Some(*index),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(order(&calm), order(&waiting), "{waiting:?}");
    }

    #[test]
    fn error_tail_detection_ignores_successful_zero_failure_summaries() {
        assert!(output_looks_like_error(&[
            "error: could not compile".to_string()
        ]));
        assert!(output_looks_like_error(&[
            "Traceback (most recent call last)".to_string()
        ]));
        assert!(!output_looks_like_error(&[
            "test result: ok. 42 passed; 0 failed".to_string()
        ]));
    }

    #[test]
    fn tab_indicators_survive_grouping() {
        let indicators = Indicators {
            unread: true,
            running: true,
            error: true,
        };
        let rows = rows_of(&[TabInfo {
            indicators,
            ..tab(0, "pwsh", Some("/work/app"))
        }]);
        assert!(matches!(
            rows.as_slice(),
            [Row::Tab {
                indicators: actual,
                ..
            }] if *actual == indicators
        ));
    }

    /// A closed strip takes nothing, and an open one takes the default width
    /// -- in points against the display, not in terminal cells: the strip is
    /// chrome, and it must not move when the terminal is zoomed.
    #[test]
    fn a_closed_strip_takes_no_width() {
        assert_eq!(width(false, None, 1, 1600.0, 1.0), 0.0);
        let open = width(true, None, 1, 1600.0, 1.0);
        assert!(open > 0.0);
        // The same window at a bigger scale gives a proportionally wider strip.
        let scaled = width(true, None, 1, 2400.0, 1.5);
        assert!((scaled / open - 1.5).abs() < 0.05, "{open} then {scaled}");
    }

    /// Two-line rows need the full width whatever the count, and a strip
    /// that widened when a second tab opened moved the terminal under the
    /// reader.
    #[test]
    fn the_default_width_does_not_move_with_the_number_of_tabs() {
        for count in [0, 1, 3, 8, 40] {
            assert_eq!(adaptive_default_width(count), crate::ui_tokens::LEFT_TAB_BAR_WIDTH);
        }
    }

    /// An agent's task is the row's title; its name moves to the second line.
    #[test]
    fn an_agents_task_leads_its_row() {
        let rows = rows_of(&[TabInfo {
            agent: Some("claude".into()),
            task: Some("Rotate buttons for PDFs".into()),
            branch: Some("main".into()),
            ..tab(0, "claude", Some("/work/pdf"))
        }]);
        let crate::sidebar::Row::Tab { label, subtitle, .. } = &rows[0] else {
            panic!("a tab row");
        };
        assert_eq!(label, "Rotate buttons for PDFs");
        let subtitle = subtitle.as_deref().unwrap_or_default();
        assert!(subtitle.contains("claude") && subtitle.contains("main"), "{subtitle:?}");
    }

    /// Only a title that is a task counts as one: the agent's own name, a
    /// shell, or a path is not.
    #[test]
    fn a_task_is_told_apart_from_a_name() {
        assert_eq!(
            task_from_title("\u{2733} 文档阅读旋转功能", Some("claude"), "claude"),
            Some("文档阅读旋转功能".to_string())
        );
        assert_eq!(task_from_title("\u{2733} Claude Code", Some("claude"), "claude"), None);
        assert_eq!(task_from_title("claude", Some("claude"), "claude"), None);
        assert_eq!(task_from_title("pwsh", None, "pwsh"), None);
        assert_eq!(task_from_title("/Users/me/work", None, "zsh"), None);
        assert_eq!(task_from_title("~/code/api", None, "zsh"), None);
        assert_eq!(task_from_title("C:\\Program Files\\Git\\bin\\bash.exe", None, "bash"), None);
        assert_eq!(task_from_title("src/app.rs", None, "vim"), None);
        assert_eq!(
            task_from_title("\u{2834} Add rate limiting to /v1/charge", Some("claude"), "claude"),
            Some("Add rate limiting to /v1/charge".to_string())
        );
    }

    /// It never takes more of the window than the budget allows, however wide
    /// somebody drags it: a strip that is most of the window is not a strip.
    #[test]
    fn the_strip_never_takes_more_than_its_share() {
        for window in [400.0, 800.0, 1600.0, 3840.0] {
            let widest = width(true, Some(10_000.0), 1, window, 1.0);
            assert!(
                widest <= window * crate::ui_tokens::LEFT_TAB_BAR_MAX_RATIO + 1.0
                    || widest
                        <= crate::ui_tokens::LEFT_TAB_BAR_MIN_WIDTH
                            * crate::chrome_font::point(1.0)
                            + 1.0,
                "a {window}px window gave a {widest}px strip"
            );
        }
    }

    /// And never so narrow that a project name cannot be read.
    #[test]
    fn the_strip_never_collapses_to_nothing_while_open() {
        for window in [200.0, 600.0, 1600.0] {
            assert!(width(true, Some(0.0), 1, window, 1.0) > 0.0);
        }
    }

    /// One project needs no headers: a header above every tab says nothing.
    #[test]
    fn a_single_project_gets_no_group_headers() {
        let rows = rows_of(&[
            tab(0, "pwsh", Some("/home/me/app")),
            tab(1, "pwsh", Some("/home/me/app")),
        ]);
        assert!(
            !rows.iter().any(|row| matches!(row, Row::Group { .. })),
            "{:?}",
            labels(&rows)
        );
        assert_eq!(rows.len(), 2);
    }

    /// Two projects get one header each, above their own tabs.
    #[test]
    fn two_projects_are_grouped() {
        let rows = rows_of(&[
            tab(0, "pwsh", Some("/home/me/alpha")),
            tab(1, "pwsh", Some("/home/me/alpha")),
            tab(2, "pwsh", Some("/home/me/beta")),
            tab(3, "pwsh", Some("/home/me/beta")),
        ]);
        let labels = labels(&rows);
        assert_eq!(labels.len(), 6, "{labels:?}");
        assert!(labels[0].starts_with('['), "{labels:?}");
        assert!(labels[3].starts_with('['), "{labels:?}");
    }

    /// Two folders with the same name are told apart by the shortest parent
    /// that distinguishes them -- not by the whole path, which is noise.
    #[test]
    fn same_named_projects_are_told_apart_by_the_shortest_parent() {
        let rows = rows_of(&[
            tab(0, "pwsh", Some("/work/acme/app")),
            tab(1, "pwsh", Some("/work/acme/app")),
            tab(2, "pwsh", Some("/work/globex/app")),
            tab(3, "pwsh", Some("/work/globex/app")),
        ]);
        let labels = labels(&rows);
        assert!(labels.contains(&"[acme/app]".to_string()), "{labels:?}");
        assert!(labels.contains(&"[globex/app]".to_string()), "{labels:?}");
    }

    /// And a project whose path is a suffix of another's still reads
    /// differently, even though no suffix of it is unique.
    #[test]
    fn a_path_that_is_a_suffix_of_another_still_gets_a_hint() {
        let rows = rows_of(&[
            tab(0, "pwsh", Some("/acme/app")),
            tab(1, "pwsh", Some("/acme/app")),
            tab(2, "pwsh", Some("/work/acme/app")),
            tab(3, "pwsh", Some("/work/acme/app")),
        ]);
        let hints: Vec<String> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Group { hint, .. } => hint.clone(),
                _ => None,
            })
            .collect();
        assert_eq!(hints.len(), 2, "both need one: {hints:?}");
        assert_ne!(hints[0], hints[1], "and they have to differ: {hints:?}");
    }

    /// Differently-named projects need no hint at all.
    #[test]
    fn distinct_names_are_left_alone() {
        let rows = rows_of(&[
            tab(0, "pwsh", Some("/work/alpha")),
            tab(1, "pwsh", Some("/work/beta")),
        ]);
        for row in &rows {
            if let Row::Group { hint, label, .. } = row {
                assert!(hint.is_none(), "{label} did not need a hint");
            }
        }
    }

    /// A tab with no directory still gets a line: a pane whose shell has not
    /// reported one yet is not a pane to hide.
    #[test]
    fn a_tab_with_no_directory_is_still_listed() {
        let rows = rows_of(&[tab(0, "pwsh", None), tab(1, "pwsh", Some("/work/app"))]);
        let tabs = rows
            .iter()
            .filter(|row| matches!(row, Row::Tab { .. }))
            .count();
        assert_eq!(tabs, 2, "{:?}", labels(&rows));
    }

    #[test]
    fn a_group_counts_its_own_tabs() {
        let rows = rows_of(&[
            tab(0, "pwsh", Some("/work/alpha")),
            tab(1, "pwsh", Some("/work/alpha")),
            tab(2, "pwsh", Some("/work/beta")),
        ]);
        let counts: Vec<usize> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Group { count, .. } => Some(*count),
                _ => None,
            })
            .collect();
        // beta's lone tab stands on its own: a header over one tab repeats it.
        assert_eq!(counts, vec![2]);
    }

    /// The wheel cannot run the strip's position past its end.
    ///
    /// The painter clamps what it draws, so this stayed invisible until the
    /// wheel turned back: the stored position had climbed past the end, and
    /// every notch spent up there had to be spent again before the first row
    /// moved. What that looks like is a strip that ignores the wheel.
    #[test]
    fn a_wheel_past_the_end_leaves_nothing_to_spend_coming_back() {
        // 20 rows in a strip showing 10: the last page starts at row 10.
        assert_eq!(scroll_by(0, 5, 20, 10), 5);
        assert_eq!(scroll_by(5, 50, 20, 10), 10, "stops at the last page");
        // One notch back from there moves, rather than paying off a debt.
        assert_eq!(scroll_by(scroll_by(5, 50, 20, 10), -1, 20, 10), 9);
    }

    #[test]
    fn a_wheel_cannot_scroll_above_the_first_row() {
        assert_eq!(scroll_by(3, -10, 20, 10), 0);
        // A strip with room for every row has nowhere to go.
        assert_eq!(scroll_by(0, 5, 4, 10), 0);
    }

    /// A list that shrinks must not leave the strip scrolled past its end
    /// showing nothing.
    #[test]
    fn scrolling_cannot_run_off_the_end() {
        assert_eq!(clamp_scroll(20, 5, 10), 0);
        assert_eq!(clamp_scroll(3, 20, 10), 3);
        assert_eq!(clamp_scroll(15, 20, 10), 10);
    }

    /// Bringing a row into view moves as little as it can.
    #[test]
    fn showing_a_row_moves_the_least_it_can() {
        assert_eq!(scroll_to_show(5, 7, 10), 5, "already visible");
        assert_eq!(scroll_to_show(5, 2, 10), 2, "above: scroll up to it");
        assert_eq!(scroll_to_show(0, 12, 10), 3, "below: just far enough");
    }

    #[test]
    fn a_strip_with_no_room_does_not_scroll() {
        assert_eq!(scroll_to_show(4, 99, 0), 4);
    }
}

/// Whether two names are the same program spelled differently.
///
/// `cmd` and `cmd.exe`, `pwsh` and `pwsh.exe`, `bash` and `/usr/bin/bash`. A
/// row that shows both says one thing twice, on a row with no room for it.
pub fn same_program(a: &str, b: &str) -> bool {
    let bare = |name: &str| {
        name.trim()
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .trim_end_matches(".exe")
            .trim_end_matches(".EXE")
            .to_lowercase()
    };
    let (a, b) = (bare(a), bare(b));
    !a.is_empty() && a == b
}

#[cfg(test)]
mod naming_tests {
    use super::*;

    /// `cmd` and `cmd.exe` are one program, and a row that shows both says one
    /// thing twice.
    #[test]
    fn a_program_is_recognised_however_it_is_spelled() {
        assert!(same_program("cmd.exe", "cmd"));
        assert!(same_program("PWSH.EXE", "pwsh"));
        assert!(same_program("/usr/bin/bash", "bash"));
        assert!(same_program("C:\\Windows\\System32\\cmd.exe", "cmd"));
    }

    /// And two different programs are two different programs, which is the
    /// case the second half of the row exists for.
    #[test]
    fn two_programs_stay_two() {
        assert!(!same_program("cargo", "pwsh"));
        assert!(!same_program("npm run dev", "pwsh"));
        assert!(!same_program("", "pwsh"));
    }

    /// A double-click is two presses on the same row, near each other; a
    /// press on a different row, or too far away, starts a new streak.
    #[test]
    fn row_double_click_requires_the_same_row_and_nearby_coordinates() {
        let first = RowClick::first(2, 40.0, 80.0);
        assert_eq!(first.again(2, 44.0, 84.0).streak(), 2);
        assert_eq!(first.again(3, 44.0, 84.0).streak(), 1);
        assert_eq!(first.again(2, 60.0, 80.0).streak(), 1);
    }

    /// The home directory is "Home". A header reading the account name names
    /// the machine's owner rather than the project.
    #[test]
    fn the_home_directory_is_called_home() {
        let Some(home) = dirs_next::home_dir() else {
            return;
        };
        assert_eq!(leaf(&home.display().to_string()), "Home");
        // And something *inside* home keeps its own name.
        let inside = home.join("projects").join("unterm");
        assert_eq!(leaf(&inside.display().to_string()), "unterm");
    }
}
