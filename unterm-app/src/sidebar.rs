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

/// How many rows, counted back from the last, fit in `room` -- at least one,
/// and all of them when there is no list. Rows are as tall as what they
/// hold, so this is the count the strip shows when scrolled to its end.
pub fn rows_that_fit_at_end(extents: &[f32], room: f32) -> usize {
    let mut used = 0.0;
    let mut count = 0;
    for extent in extents.iter().rev() {
        if used + extent > room {
            break;
        }
        used += extent;
        count += 1;
    }
    count.max(1)
}

/// The first row to show so that `row` is on screen, measured in the rows'
/// own heights: unchanged when it already is, `row` itself when it is above,
/// and otherwise as far back from it as `room` allows. A count of rows that
/// fit is not enough once rows differ in height -- counted from the list's
/// short end, it let a tall row at the other end scroll out of sight.
pub fn scroll_to_show_extents(scroll_top: usize, row: usize, extents: &[f32], room: f32) -> usize {
    if row >= extents.len() {
        return scroll_top;
    }
    if row < scroll_top {
        return row;
    }
    if extents[scroll_top..=row].iter().sum::<f32>() <= room {
        return scroll_top;
    }
    let mut start = row;
    let mut used = extents[row];
    while start > 0 && used + extents[start - 1] <= room {
        start -= 1;
        used += extents[start];
    }
    start
}

/// The hues a project can wear: muted, and clear of the amber the chrome
/// keeps for "needs you".
const PROJECT_HUES: [[u8; 3]; 8] = [
    [0x6c, 0x9f, 0xd8], // blue
    [0x4d, 0xb6, 0xac], // teal
    [0x8b, 0xc3, 0x7e], // green
    [0x9f, 0x86, 0xd4], // violet
    [0xe0, 0x7a, 0xa4], // rose
    [0xe8, 0x8c, 0x6c], // coral
    [0x5f, 0xb8, 0xd9], // cyan
    [0xc9, 0xa2, 0xd8], // lilac
];

/// The hue a project prefers: hashed from its key (its directory), so it is
/// the same every run and in every window. FNV-1a, stable across runs and
/// platforms, unlike the std hasher.
fn preferred_hue(key: &str) -> usize {
    let hash = key
        .bytes()
        .fold(0x811c_9dc5_u32, |hash, byte| (hash ^ byte as u32).wrapping_mul(0x0100_0193));
    (hash % PROJECT_HUES.len() as u32) as usize
}

/// Each project's colour, remembered across frames.
///
/// The rail has no room for names, and a rule between runs of identical
/// terminal icons told nobody which run was which project. A project wears
/// the hue its key hashes to unless another project on screen already does;
/// then it takes the next free one. Eight hues hashed blindly put two of three
/// projects in the same rose, which is no way to tell them apart.
///
/// Handing the hues out afresh each frame, in row order, meant a project's
/// colour hung on its neighbours: opening or closing another project, or
/// moving a tab, could repaint one that nobody touched. So a hue, once given,
/// stays with its project for as long as that project is on screen, and a
/// project coming back asks for the hue it last had before anything else.
#[derive(Debug, Default, Clone)]
pub struct ProjectHues {
    remembered: HashMap<String, usize>,
    shown: Vec<String>,
}

impl ProjectHues {
    pub fn colors(&mut self, rows: &[Row], is_light: bool) -> HashMap<String, [f32; 4]> {
        let mut keys: Vec<&String> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Group { key, .. } => Some(key),
                _ => None,
            })
            .collect();
        keys.sort();
        keys.dedup();
        // Projects already on screen settle first, so a newcomer never takes
        // a hue someone is wearing. Within each set, key order, so the answer
        // does not depend on where the rows happen to sit.
        let (mut staying, arriving): (Vec<&String>, Vec<&String>) =
            keys.iter().partition(|key| self.shown.iter().any(|shown| shown == **key));
        staying.extend(arriving);

        let mut taken = [false; PROJECT_HUES.len()];
        let mut hues = HashMap::new();
        for key in staying {
            let asked = self
                .remembered
                .get(key)
                .copied()
                .unwrap_or_else(|| preferred_hue(key));
            let hue = (0..PROJECT_HUES.len())
                .map(|step| (asked + step) % PROJECT_HUES.len())
                .find(|hue| !taken[*hue])
                .unwrap_or(asked);
            taken[hue] = true;
            hues.insert(key.clone(), hue);
        }
        for (key, hue) in &hues {
            self.remembered.insert(key.clone(), *hue);
        }
        self.shown = hues.keys().cloned().collect();

        hues.into_iter()
            .map(|(key, hue)| {
                let [r, g, b] = PROJECT_HUES[hue];
                let color = crate::chrome::srgb(r, g, b);
                let color = if is_light {
                    // The same hue, dark enough to carry white on a light surface.
                    crate::chrome::mix(color, [0.0, 0.0, 0.0, 1.0], 0.35)
                } else {
                    color
                };
                (key, color)
            })
            .collect()
    }
}

/// The letter a project goes by on the rail: the first letter or digit of
/// its name, capitalised.
pub fn project_initial(label: &str) -> String {
    label
        .chars()
        .find(|ch| ch.is_alphanumeric())
        .map(|ch| ch.to_uppercase().collect())
        .unwrap_or_else(|| "~".to_string())
}

/// The strip folded to its rail: one column of icons, Fluent's compact
/// navigation pane (48px at 100%).
pub fn rail_width(scale: f32) -> f32 {
    (crate::ui_tokens::LEFT_TAB_BAR_RAIL_WIDTH * crate::chrome_font::point(scale)).round()
}

/// Whether a window this wide (in logical pixels) starts with the strip on
/// its rail. Fluent's navigation pane goes compact below 1008 epx; a terminal
/// has less to spare than a settings page, so ours folds a little earlier.
pub fn rail_by_default(logical_window_width: f32) -> bool {
    logical_window_width < crate::ui_tokens::LEFT_TAB_BAR_RAIL_BELOW
}

/// Whether the grip, let go at `points` from the left edge, means "fold to
/// the rail" rather than "this wide". Past the minimum width the strip cannot
/// be narrower anyway, so well short of it is a request to get out of the way.
pub fn snaps_to_rail(points: f32) -> bool {
    points < crate::ui_tokens::LEFT_TAB_BAR_MIN_WIDTH * 0.6
}

/// What the strip remembers between runs: how wide it was dragged, and
/// whether somebody folded or opened it by hand.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Prefs {
    pub points: Option<f32>,
    /// Folded (`true`) or opened (`false`) by hand; `None` follows the
    /// window's width. A plain flag could not say "opened by hand in a narrow
    /// window", and saved a fold the window had made by itself as the user's.
    #[serde(default)]
    pub rail_choice: Option<bool>,
    /// The first release's flag, read so a fold made then is kept.
    #[serde(default, rename = "rail", skip_serializing)]
    legacy_rail: bool,
}

impl Prefs {
    pub fn new(points: Option<f32>, rail_choice: Option<bool>) -> Self {
        Self { points, rail_choice, legacy_rail: false }
    }

    fn path() -> Option<std::path::PathBuf> {
        unterm_protocol::state_path("sidebar.json")
    }

    /// The saved preference, or the defaults when there is none or it does
    /// not parse -- a strip that will not open over a bad file is worse than
    /// one that forgot its width.
    pub fn load() -> Self {
        let mut prefs: Self = Self::path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        if prefs.rail_choice.is_none() && prefs.legacy_rail {
            prefs.rail_choice = Some(true);
        }
        prefs.legacy_rail = false;
        prefs
    }

    /// Written whole or not at all: a file cut short by a crash would parse
    /// as nothing and quietly reset the strip.
    pub fn save(&self) {
        let Some(path) = Self::path() else {
            return;
        };
        let Ok(text) = serde_json::to_string(self) else {
            return;
        };
        let staged = path.with_extension("json.tmp");
        let written = std::fs::write(&staged, text).and_then(|()| std::fs::rename(&staged, &path));
        if let Err(err) = written {
            log::warn!("could not save the sidebar layout to {path:?}: {err}");
        }
    }
}

/// `text` cut in the middle to fit `room`, with one ellipsis.
///
/// For a branch name, whose two ends both say which branch it is. The strip
/// used to receive a name already cut to 28 characters and then cut the end
/// off that as well: `chore/installe…ampe…`, two ellipses and neither end.
pub fn fit_middle(text: &str, room: f32, measure: &mut dyn FnMut(&str) -> f32) -> String {
    if room <= 0.0 {
        return String::new();
    }
    if measure(text) <= room {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let ellipsis = measure("\u{2026}");
    // The longest head+tail that fits, head taking the odd character: the
    // prefix is where the convention lives (`feat/`, `fix/`).
    let mut best = String::from("\u{2026}");
    for keep in (1..chars.len()).rev() {
        let tail = keep / 2;
        let head = keep - tail;
        let candidate: String = chars[..head]
            .iter()
            .chain(std::iter::once(&'\u{2026}'))
            .chain(chars[chars.len() - tail..].iter())
            .collect();
        if measure(&candidate) <= room {
            best = candidate;
            break;
        }
    }
    if measure(&best) > room && ellipsis > room {
        return String::new();
    }
    best
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
    if is_a_shell_name(&tab.title) || is_prompt_title(&tab.title) {
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
/// `user@host: ~/code/api` -- the title bash and zsh set at their prompt on
/// most Linux systems. It says where the shell is, not what it is doing.
pub fn is_prompt_title(title: &str) -> bool {
    let Some((who, _)) = title.trim().split_once(':') else {
        return false;
    };
    let Some((user, host)) = who.split_once('@') else {
        return false;
    };
    let word = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    word(user) && word(host)
}

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
        || is_prompt_title(title)
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
    fn a_tall_row_is_followed_by_its_own_height() {
        // Twelve tall rows above eight short ones, ten lines of room: from
        // the short end nine fit, but only six tall ones do.
        let mut extents = vec![1.55; 12];
        extents.extend(vec![0.95; 8]);
        let start = scroll_to_show_extents(0, 9, &extents, 10.0);
        let shown: f32 = extents[start..=9].iter().sum();
        assert!(shown <= 10.0 && start <= 9, "row 9 from {start} needs {shown}");
        assert_eq!(scroll_to_show_extents(4, 6, &extents, 10.0), 4);
        assert_eq!(scroll_to_show_extents(5, 2, &extents, 10.0), 2);
    }

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
        assert_eq!(task_from_title("runner@ci-box: ~/code/api", None, "bash"), None);
        assert_eq!(task_from_title("me@laptop:~", None, "zsh"), None);
        assert!(!is_prompt_title("Fix login: handle expired tokens"));
        assert!(!is_prompt_title("email a@b.c: done"));
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

    /// A branch too long for its row is cut once, in the middle, keeping
    /// both ends -- never the double cut of `chore/installe…ampe…`.
    #[test]
    fn a_long_branch_is_cut_once_in_the_middle() {
        let mut measure = |text: &str| text.chars().count() as f32;
        let shown = fit_middle("chore/installer-timestamped-backups", 20.0, &mut measure);
        assert_eq!(shown.chars().count(), 20, "{shown}");
        assert_eq!(shown.matches('\u{2026}').count(), 1, "{shown}");
        assert!(shown.starts_with("chore/"), "{shown}");
        assert!(shown.ends_with("backups"), "{shown}");
        assert_eq!(fit_middle("main", 20.0, &mut measure), "main");
    }

    /// Rows are as tall as what they hold, so what fits is counted from the
    /// rows themselves: a half-empty strip does not scroll.
    #[test]
    fn a_strip_with_room_left_does_not_scroll() {
        let extents = [40.0, 60.0, 60.0, 40.0, 60.0];
        assert_eq!(rows_that_fit_at_end(&extents, 1000.0), extents.len());
        assert_eq!(rows_that_fit_at_end(&extents, 125.0), 2);
        assert_eq!(rows_that_fit_at_end(&extents, 10.0), 1);
    }

    /// On the rail a project is a lettered square in its own colour, and two
    /// projects in one window never share one.
    #[test]
    fn projects_on_the_rail_are_told_apart() {
        let group = |key: &str| Row::Group {
            key: key.into(),
            label: key.into(),
            hint: None,
            count: 1,
            collapsed: false,
            active: false,
        };
        let rows: Vec<Row> = (0..8).map(|n| group(&format!("D:/code/p{n}"))).collect();
        let mut hues = ProjectHues::default();
        let colors = hues.colors(&rows, false);
        let mut seen: Vec<[u32; 3]> = colors
            .values()
            .map(|c| [(c[0] * 255.0) as u32, (c[1] * 255.0) as u32, (c[2] * 255.0) as u32])
            .collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 8, "eight projects, eight colours");
        // The same project keeps its colour from one frame to the next.
        assert_eq!(hues.colors(&rows, false), colors);
        assert_eq!(project_initial("unterm"), "U");
        assert_eq!(project_initial(".claude"), "C");
        assert_eq!(project_initial("项目"), "项");
    }

    /// A project keeps its colour while others open and close around it, and
    /// wherever its rows happen to sit.
    #[test]
    fn a_project_keeps_its_colour_as_others_come_and_go() {
        let group = |key: &str| Row::Group {
            key: key.into(),
            label: key.into(),
            hint: None,
            count: 1,
            collapsed: false,
            active: false,
        };
        // Find two keys that ask for the same hue, so one of them must move.
        let keys: Vec<String> = (0..64).map(|n| format!("/code/p{n}")).collect();
        let (first, clash) = keys
            .iter()
            .enumerate()
            .find_map(|(i, a)| {
                keys[i + 1..]
                    .iter()
                    .find(|b| preferred_hue(b) == preferred_hue(a))
                    .map(|b| (a.clone(), b.clone()))
            })
            .expect("64 keys over 8 hues share one");
        let (early, late) = if first < clash { (first, clash) } else { (clash, first) };
        let others: Vec<String> = keys
            .iter()
            .filter(|k| **k != early && **k != late)
            .take(3)
            .cloned()
            .collect();

        let mut hues = ProjectHues::default();
        // The later key is open first and gets the hue both of them want.
        let alone = hues.colors(&[group(&late)], false)[&late];
        // The earlier key sorts ahead of it, but arrives later: it moves over.
        let mut rows = vec![group(&early), group(&late)];
        let both = hues.colors(&rows, false);
        assert_eq!(both[&late], alone, "an arrival does not repaint who was there");
        assert_ne!(both[&early], both[&late]);
        // More projects open, the rows reorder, and nobody changes.
        rows.extend(others.iter().map(|k| group(k)));
        rows.reverse();
        let crowd = hues.colors(&rows, false);
        assert_eq!(crowd[&late], alone);
        assert_eq!(crowd[&early], both[&early]);
        // A neighbour closes; the rest stay as they were.
        rows.retain(|row| !matches!(row, Row::Group { key, .. } if *key == others[0]));
        let fewer = hues.colors(&rows, false);
        for key in [&early, &late, &others[1], &others[2]] {
            assert_eq!(fewer[key], crowd[key], "{key} was repainted");
        }
        // A project that closes and comes back gets its old colour.
        hues.colors(&[group(&late)], false);
        let back = hues.colors(&[group(&late), group(&early)], false);
        assert_eq!(back[&early], both[&early]);
    }

    /// Letting the grip go well short of the minimum folds the strip to its
    /// rail; a narrow window starts there.
    #[test]
    fn the_strip_folds_to_a_rail() {
        assert!(snaps_to_rail(crate::ui_tokens::LEFT_TAB_BAR_RAIL_WIDTH));
        assert!(!snaps_to_rail(crate::ui_tokens::LEFT_TAB_BAR_MIN_WIDTH));
        assert!(rail_by_default(800.0));
        assert!(!rail_by_default(1280.0));
        assert!(rail_width(1.0) < width(true, None, 1, 1600.0, 1.0));
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
