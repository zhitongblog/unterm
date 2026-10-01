//! A searchable list of things you can do.
//!
//! One overlay serves two jobs, because they are the same job with different
//! rows in it: the command palette lists what the terminal can do, and the
//! launcher lists what it can start. Both are "type a few letters, pick the
//! one you meant".
//!
//! The matching is subsequence-based rather than substring, because that is
//! what people expect from a palette: `nt` should find "New Tab". The scoring
//! then has to earn its keep -- a subsequence match will find almost
//! everything, so the order it puts them in *is* the feature.

/// What picking a row does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// One of the front end's key actions, by the same name an agent sees.
    Action(crate::keys::Action),
    /// Start a new tab running a named program.
    Launch { program: String, args: Vec<String> },
    /// Take the focused pane to a directory.
    ChangeDirectory { path: String },
    /// Start a new tab already in a directory.
    NewTabIn { path: String },
    /// Reopen a saved workspace, a tab per saved directory.
    RestoreWorkspace { name: String },
    /// Open the operating system's directory chooser.
    OpenDirectoryPicker { then: BrowseThen },
    /// Ask for a name to save the open tabs under.
    OpenWorkspaceSave,
    /// Save the open tabs as a workspace named by the typed line.
    SaveWorkspace,
    /// Start or stop recording the focused pane.
    ToggleRecording,
    /// Write the focused pane's session out as markdown.
    ExportSession,
    /// Open the web settings page in a browser.
    OpenSettings,
    /// Download the newer release, verify it, quit and restart into it.
    InstallUpdate,
    /// Open the Unzoo One console in a browser. Only offered when the
    /// console assets are actually installed.
    OpenConsole,
    /// Open the shell picker: a new tab running the shell you choose.
    OpenShellSelector,
    /// Show the directories under a path, so one can be picked.
    Browse { path: String, then: BrowseThen },
    /// Switch to a named theme.
    ApplyTheme { id: String },
    /// Type a character at the prompt, and remember that it was picked.
    TypeCharacter { glyph: String, name: String },
    /// Send a crew of agents at the task that has been typed.
    LaunchFleet { agents: Vec<String> },
    /// Open the fuzzy project/tab navigator.
    OpenTabNavigator,
    /// Focus one stable tab selected by that navigator.
    ActivateTab { tab_id: usize },
    /// Give one tab the name that has been typed. An empty line hands the
    /// tab back to automatic titling.
    RenameTab { tab_id: usize },
    /// Render the focused pane's entire scrollback to one tall PNG.
    CaptureScrollback,
    /// Capture the current desktop while keeping the Unterm window out.
    CaptureDesktop,
    /// Open a page in the system browser.
    OpenUrl { url: String },
    /// The confirmation the close request asked for.
    ConfirmCloseWindow,
    /// Close the window and leave the Core running: the shells and the
    /// agents in them carry on, and reopening finds them where they
    /// were. Only offered when a Core actually holds them.
    KeepRunningInBackground,
    /// Refuse new sessions, let the running ones finish, then stop.
    DrainThenExit,
    /// End every session now and exit. The one irreversible answer.
    CancelAndExit,
    /// Open the rename line for one tab, by its visible position.
    OpenTabRename { index: usize },
    /// Let the user drag a rectangle to capture from the desktop.
    SelectCaptureRegion,
    /// A row that is information rather than a verb. The Insights card is a
    /// list to read, not a list to run, and pressing a line of it should do
    /// nothing rather than whatever a guess would be.
    Nothing,
}

/// What picking a folder is for.
///
/// Carried through the browsing rather than asked at the end: the two quick
/// actions that open a picker differ only in what happens once a folder is
/// chosen, and a picker that forgets which one opened it can only do one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowseThen {
    ChangeDirectory,
    NewTab,
}

/// A row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub label: String,
    /// Shown dimmed after the label: a key chord, a path, a description.
    pub hint: String,
    pub command: Command,
}

/// Where a palette's rows come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Source {
    /// A list settled when the palette opened. Typing narrows it.
    #[default]
    Fixed,
    /// A fixed list, and a line of text that is not a filter.
    ///
    /// The fleet card: what is typed is the task to send the crew at, and the
    /// rows are crews. Narrowing them by what has been typed would empty the
    /// list on the first word of the task.
    Text,
    /// Characters, matched again on every keystroke.
    ///
    /// Asked again rather than narrowed for the same reason as directories,
    /// but a different one underneath: there are thousands of them, and only
    /// the top of the list is ever drawn. Handing the palette all of them and
    /// letting it score them on every keystroke is thousands of comparisons
    /// per letter for rows nobody sees.
    Characters,
    /// Directories, asked for again on every keystroke.
    ///
    /// It has to be asked again, because typing a path names a place nothing
    /// has scanned: the scan is bounded and the disk is not, so `D:/somewhere`
    /// can only be answered by going and looking. Narrowing a settled list
    /// cannot do it -- and that is exactly what the first version did, which
    /// is why typing a path found nothing.
    Directories,
}

/// How a palette presents its first and last lines.
///
/// A shell chooser is not a command search box. Giving both the same blank
/// `>` prompt made the launcher look like an unfinished command palette and
/// hid the fact that the numbered rows could be selected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Search,
    ShellSelector,
    /// A question with a short list of answers, over a dimmed screen.
    ///
    /// No query line: a decision is not something you search for, and
    /// a prompt that looks like the command palette is one people
    /// dismiss out of habit. The one irreversible answer is drawn in
    /// the danger colour so it cannot be picked for the safe one.
    Confirm,
}

/// An open palette.
#[derive(Clone, Debug, Default)]
pub struct Palette {
    /// A short heading that says what this particular palette is for.
    pub title: Option<String>,
    pub query: String,
    pub entries: Vec<Entry>,
    /// Indices into `entries`, best first.
    pub matches: Vec<usize>,
    pub selected: usize,
    /// First match currently visible in the bounded result window.
    ///
    /// This is deliberately separate from `selected`.  The old command
    /// palette kept both values: keyboard navigation keeps the selection in
    /// view, while a wheel or the on-card arrows can move the visible window
    /// itself.  Deriving this value only from `selected` makes a palette look
    /// as though it cannot scroll.
    pub top_row: usize,
    pub source: Source,
    pub view: View,
    /// Something that went wrong, shown under the line rather than instead of
    /// the palette: the answer to "this repository has uncommitted changes" is
    /// to go and commit them and press Enter again, which needs the task still
    /// to be there.
    pub error: Option<String>,
}

impl Palette {
    pub fn new(entries: Vec<Entry>) -> Self {
        let mut palette = Self {
            title: None,
            query: String::new(),
            entries,
            matches: Vec::new(),
            selected: 0,
            top_row: 0,
            source: Source::Fixed,
            view: View::Search,
            error: None,
        };
        palette.refilter();
        palette
    }

    /// Give a search palette a visible purpose instead of an anonymous `>`.
    pub fn titled(mut self, title: String) -> Self {
        self.title = Some(title);
        self
    }

    /// A palette whose line is text rather than a filter.
    pub fn writing(entries: Vec<Entry>) -> Self {
        let mut palette = Self::new(entries);
        palette.source = Source::Text;
        palette
    }

    /// A palette whose rows are asked for again as the query changes.
    pub fn browsing(entries: Vec<Entry>) -> Self {
        let mut palette = Self::new(entries);
        palette.source = Source::Directories;
        palette
    }

    /// A palette of characters, matched again as the query changes.
    pub fn characters(entries: Vec<Entry>) -> Self {
        let mut palette = Self::new(entries);
        palette.source = Source::Characters;
        palette
    }

    /// A question over a dimmed screen, answered from a short list.
    ///
    /// The line is text rather than a filter, so a stray keystroke
    /// cannot narrow the answers away and leave a prompt with nothing
    /// to press.
    pub fn confirm(entries: Vec<Entry>, title: String) -> Self {
        let mut palette = Self::new(entries);
        palette.view = View::Confirm;
        palette.source = Source::Text;
        palette.title = Some(title);
        palette
    }

    /// A fixed, explicitly-labelled list of shells.
    pub fn shells(entries: Vec<Entry>) -> Self {
        let mut palette = Self::new(entries);
        palette.view = View::ShellSelector;
        palette
    }

    /// Replace the rows without disturbing what has been typed.
    ///
    /// The rows are shown in the order they arrive: they were already ordered
    /// by whoever went and looked, and scoring them again against the query
    /// undoes that -- a typed path is not a fuzzy match, and rescoring it puts
    /// whichever child happens to contain those letters at the top.
    pub fn replace_entries(&mut self, entries: Vec<Entry>) {
        self.matches = (0..entries.len()).collect();
        self.entries = entries;
        self.selected = 0;
        self.top_row = 0;
    }

    /// The rows to show, in order.
    pub fn visible(&self) -> Vec<&Entry> {
        self.matches
            .iter()
            .filter_map(|i| self.entries.get(*i))
            .collect()
    }

    pub fn current(&self) -> Option<&Entry> {
        self.matches
            .get(self.selected)
            .and_then(|i| self.entries.get(*i))
    }

    /// First result shown in a bounded-height palette.
    ///
    /// The selection is an index in the complete match list.  Keeping the
    /// drawing fixed on rows 0..12 while that index moves past 12 makes the
    /// rest of the command palette exist only in memory: arrows select an
    /// invisible command and neither mouse nor Enter agrees with the screen.
    pub fn window_start(&self, rows: usize) -> usize {
        self.top_row
            .min(self.matches.len().saturating_sub(rows.max(1)))
    }

    /// Keep the selected row inside the visible result window.
    fn reveal_selection(&mut self, rows: usize) {
        if self.matches.is_empty() || rows == 0 {
            self.top_row = 0;
            return;
        }
        if self.selected < self.top_row {
            self.top_row = self.selected;
        } else if self.selected >= self.top_row.saturating_add(rows) {
            self.top_row = self.selected + 1 - rows;
        }
        self.top_row = self.top_row.min(self.matches.len().saturating_sub(rows));
    }

    /// Move the selection, clamping at both ends as the 0.57.4 palette did.
    pub fn step(&mut self, delta: isize) {
        if self.matches.is_empty() {
            self.selected = 0;
            self.top_row = 0;
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.matches.len() - 1);
        self.reveal_selection(MAX_ROWS);
    }

    /// Move the result window with a wheel or one of its visible arrows.
    pub fn scroll(&mut self, delta: isize, rows: usize) {
        if self.matches.is_empty() || rows == 0 {
            self.selected = 0;
            self.top_row = 0;
            return;
        }
        let max_top = self.matches.len().saturating_sub(rows);
        self.top_row = self.top_row.saturating_add_signed(delta).min(max_top);
        self.selected = self
            .selected
            .max(self.top_row)
            .min((self.top_row + rows - 1).min(self.matches.len() - 1));
    }

    pub fn can_scroll_up(&self) -> bool {
        self.top_row > 0
    }

    pub fn can_scroll_down(&self, rows: usize) -> bool {
        self.top_row.saturating_add(rows) < self.matches.len()
    }

    /// Rebuild the match list for the current query.
    pub fn refilter(&mut self) {
        let query = self.query.to_lowercase();
        let mut scored: Vec<(i32, usize)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| score(&entry.label, &query).map(|s| (s, index)))
            .collect();
        // Best score first; ties keep the order the entries were given in, so
        // an empty query shows the list as its author arranged it.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.matches = scored.into_iter().map(|(_, index)| index).collect();
        self.selected = 0;
        self.top_row = 0;
    }
}

/// How well `label` matches `query`, or None if it does not.
///
/// Higher is better. The rules, in the order they matter:
///
/// - A match at the start of a word beats one in the middle, so `nt` finds
///   "New Tab" before "Print Nothing".
/// - Consecutive characters beat scattered ones, so `tab` finds "New Tab"
///   before "Toggle Alt Buffer".
/// - A shorter label beats a longer one, so an exact command beats one that
///   merely contains it.
fn score(label: &str, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let haystack: Vec<char> = label.to_lowercase().chars().collect();
    let needle: Vec<char> = query.chars().collect();

    let mut total = 0i32;
    let mut at = 0usize;
    let mut previous: Option<usize> = None;

    for wanted in needle {
        let found = haystack[at..].iter().position(|ch| *ch == wanted)? + at;

        let starts_word = found == 0
            || haystack
                .get(found.wrapping_sub(1))
                .map(|ch| !ch.is_alphanumeric())
                .unwrap_or(false);
        if starts_word {
            total += 8;
        }
        // Weighted above a word start: a run of letters the user typed
        // together is a stronger signal than the same letters scattered
        // across word boundaries. Without this, "tab" finds "Toggle Alt
        // Buffer" before "New Tab", because three word-starts outscore one
        // word-start and two neighbours.
        if previous == Some(found.wrapping_sub(1)) {
            total += 10;
        }
        previous = Some(found);
        at = found + 1;
    }

    // Shorter labels win ties: an exact command beats one that contains it.
    total -= haystack.len() as i32 / 8;
    Some(total)
}

/// What a key press means to an open palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Type(String),
    Backspace,
    Step(isize),
    Complete,
    Accept,
    Close,
    NotOurs,
}

/// Decide what an open palette does with a key.
///
/// It takes the whole keyboard while it is open, apart from the chords that
/// are not text -- a palette that let a keystroke through to the shell would
/// be running commands in the pane behind it.
pub fn key_for(named: Option<&str>, character: Option<&str>, ctrl: bool) -> Key {
    match named {
        Some("Escape") => return Key::Close,
        Some("Enter") => return Key::Accept,
        Some("Backspace") => return Key::Backspace,
        Some("ArrowDown") => return Key::Step(1),
        Some("ArrowUp") => return Key::Step(-1),
        Some("PageDown") => return Key::Step(MAX_ROWS as isize),
        Some("PageUp") => return Key::Step(-(MAX_ROWS as isize)),
        Some("Tab") => return Key::Complete,
        // Space arrives as a named key rather than as a character, and falling
        // through to the character arm loses it. Which meant no query could
        // contain one: "new tab" typed as "newtab" found nothing, and a task
        // written on the fleet card arrived as one word.
        Some("Space") if !ctrl => return Key::Type(" ".to_string()),
        _ => {}
    }
    match character {
        Some(text) if !ctrl => Key::Type(text.to_string()),
        _ => Key::NotOurs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Action;

    fn entry(label: &str) -> Entry {
        Entry {
            label: label.to_string(),
            hint: String::new(),
            command: Command::Action(Action::Copy),
        }
    }

    fn palette(labels: &[&str]) -> Palette {
        Palette::new(labels.iter().map(|label| entry(label)).collect())
    }

    #[test]
    fn an_empty_query_shows_everything_in_order() {
        let palette = palette(&["New Tab", "Close Tab", "Copy"]);
        assert_eq!(
            palette
                .visible()
                .iter()
                .map(|e| e.label.as_str())
                .collect::<Vec<_>>(),
            ["New Tab", "Close Tab", "Copy"]
        );
    }

    #[test]
    fn initials_find_the_command_they_stand_for() {
        let mut palette = palette(&["Print Nothing", "New Tab", "Close Tab"]);
        palette.query = "nt".to_string();
        palette.refilter();
        assert_eq!(
            palette.current().map(|e| e.label.as_str()),
            Some("New Tab"),
            "a word-start match should beat one in the middle"
        );
    }

    #[test]
    fn consecutive_letters_beat_scattered_ones() {
        let mut palette = palette(&["Toggle Alt Buffer", "New Tab"]);
        palette.query = "tab".to_string();
        palette.refilter();
        assert_eq!(palette.current().map(|e| e.label.as_str()), Some("New Tab"));
    }

    #[test]
    fn an_exact_command_beats_one_that_merely_contains_it() {
        let mut palette = palette(&["Copy Selection And Close", "Copy"]);
        palette.query = "copy".to_string();
        palette.refilter();
        assert_eq!(palette.current().map(|e| e.label.as_str()), Some("Copy"));
    }

    #[test]
    fn a_query_nothing_matches_leaves_nothing_selected() {
        let mut palette = palette(&["New Tab", "Copy"]);
        palette.query = "zzz".to_string();
        palette.refilter();
        assert!(palette.visible().is_empty());
        assert!(palette.current().is_none());
    }

    #[test]
    fn matching_ignores_case() {
        let mut palette = palette(&["New Tab"]);
        palette.query = "NEW".to_string();
        palette.refilter();
        assert_eq!(palette.visible().len(), 1);
    }

    #[test]
    fn stepping_stops_at_both_ends_like_the_old_palette() {
        let mut palette = palette(&["a", "b", "c"]);
        palette.step(-1);
        assert_eq!(palette.selected, 0);
        palette.step(99);
        assert_eq!(palette.selected, 2);
    }

    #[test]
    fn the_visible_window_follows_a_selection_below_the_first_page() {
        let labels: Vec<String> = (0..20).map(|index| format!("command {index}")).collect();
        let mut palette = Palette::new(labels.iter().map(|label| entry(label)).collect());
        for _ in 0..15 {
            palette.step(1);
        }

        assert_eq!(palette.selected, 15);
        assert_eq!(palette.window_start(12), 4);
        assert!(palette.selected < palette.window_start(12) + 12);
    }

    #[test]
    fn wheel_scrolling_moves_the_window_and_keeps_selection_visible() {
        let labels: Vec<String> = (0..20).map(|index| format!("command {index}")).collect();
        let mut palette = Palette::new(labels.iter().map(|label| entry(label)).collect());

        palette.scroll(5, 12);
        assert_eq!(palette.top_row, 5);
        assert_eq!(palette.selected, 5);
        assert!(palette.can_scroll_up());
        assert!(palette.can_scroll_down(12));

        palette.scroll(99, 12);
        assert_eq!(palette.top_row, 8);
        assert_eq!(palette.selected, 8);
        assert!(!palette.can_scroll_down(12));
    }

    #[test]
    fn the_visible_window_stays_at_the_top_on_the_first_page() {
        let mut palette = palette(&["a", "b", "c"]);
        palette.step(2);

        assert_eq!(palette.window_start(12), 0);
    }

    #[test]
    fn stepping_with_nothing_matched_answers_rather_than_dividing_by_zero() {
        let mut palette = palette(&[]);
        palette.step(1);
        assert_eq!(palette.selected, 0);
    }

    #[test]
    fn narrowing_the_query_puts_the_selection_back_at_the_top() {
        // The best match for the new query, not whatever index the old
        // selection happened to be -- which would land on an unrelated row.
        let mut palette = palette(&["New Tab", "Close Tab", "Copy"]);
        palette.step(2);
        palette.query = "tab".to_string();
        palette.refilter();
        assert_eq!(palette.selected, 0);
        assert_eq!(palette.top_row, 0);
    }

    #[test]
    fn the_palette_takes_the_keyboard_while_it_is_open() {
        assert_eq!(key_for(None, Some("n"), false), Key::Type("n".to_string()));
        assert_eq!(key_for(Some("Enter"), None, false), Key::Accept);
        assert_eq!(key_for(Some("Escape"), None, false), Key::Close);
        assert_eq!(key_for(Some("ArrowDown"), None, false), Key::Step(1));
        assert_eq!(key_for(Some("ArrowUp"), None, false), Key::Step(-1));
    }

    #[test]
    fn a_control_chord_is_not_text_to_search_for() {
        assert_eq!(key_for(None, Some("c"), true), Key::NotOurs);
    }

    #[test]
    fn pages_and_tab_are_navigation_not_shell_input() {
        assert_eq!(
            key_for(Some("PageUp"), None, false),
            Key::Step(-(MAX_ROWS as isize))
        );
        assert_eq!(
            key_for(Some("PageDown"), None, false),
            Key::Step(MAX_ROWS as isize)
        );
        assert_eq!(key_for(Some("Tab"), None, false), Key::Complete);
    }

    #[test]
    fn a_launcher_row_carries_the_program_it_starts() {
        let entry = Entry {
            label: "pwsh".to_string(),
            hint: "PowerShell".to_string(),
            command: Command::Launch {
                program: "pwsh.exe".to_string(),
                args: vec!["-NoLogo".to_string()],
            },
        };
        let palette = Palette::new(vec![entry]);
        assert_eq!(
            palette.current().map(|e| &e.command),
            Some(&Command::Launch {
                program: "pwsh.exe".to_string(),
                args: vec!["-NoLogo".to_string()],
            })
        );
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;

    fn row(label: &str) -> Entry {
        Entry {
            label: label.to_string(),
            hint: String::new(),
            command: Command::ChangeDirectory {
                path: label.to_string(),
            },
        }
    }

    /// An ordinary palette settles its list when it opens and only narrows it.
    #[test]
    fn a_fixed_palette_narrows_what_it_was_given() {
        let mut palette = Palette::new(vec![row("alpha"), row("beta")]);
        assert_eq!(palette.source, Source::Fixed);
        palette.query = "al".into();
        palette.refilter();
        assert_eq!(
            palette
                .visible()
                .iter()
                .map(|e| e.label.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha"]
        );
    }

    /// A browsing palette says so, so the window knows to go and look again.
    /// Without this the directory jump can only ever narrow what was scanned
    /// when it opened, and a typed path finds nothing.
    #[test]
    fn a_browsing_palette_says_it_wants_asking_again() {
        let palette = Palette::browsing(vec![row("alpha")]);
        assert_eq!(palette.source, Source::Directories);
    }

    /// New rows arrive in the order they were found. They were ordered by
    /// whoever went and looked -- scoring them again against a typed path puts
    /// whichever child happens to contain those letters at the top.
    #[test]
    fn replaced_rows_keep_the_order_they_arrived_in() {
        let mut palette = Palette::browsing(vec![row("old")]);
        palette.query = "d:/code/".into();
        palette.replace_entries(vec![row("zebra"), row("apple"), row("mango")]);
        assert_eq!(
            palette
                .visible()
                .iter()
                .map(|e| e.label.as_str())
                .collect::<Vec<_>>(),
            vec!["zebra", "apple", "mango"]
        );
    }

    /// And what has been typed survives the replacement: the query is what
    /// asked for these rows in the first place.
    #[test]
    fn replacing_rows_leaves_the_query_alone() {
        let mut palette = Palette::browsing(vec![row("old")]);
        palette.query = "d:/code/un".into();
        palette.replace_entries(vec![row("unterm")]);
        assert_eq!(palette.query, "d:/code/un");
        assert_eq!(palette.current().map(|e| e.label.as_str()), Some("unterm"));
    }

    /// The selection starts at the top of the new list rather than staying on
    /// a row number that now means something else.
    #[test]
    fn replacing_rows_puts_the_selection_back_at_the_top() {
        let mut palette = Palette::browsing(vec![row("a"), row("b"), row("c")]);
        palette.step(2);
        assert_eq!(palette.selected, 2);
        palette.replace_entries(vec![row("x"), row("y")]);
        assert_eq!(palette.selected, 0);
        assert_eq!(palette.current().map(|e| e.label.as_str()), Some("x"));
    }

    /// Nothing found is an empty list rather than the previous one: a picker
    /// that keeps showing the last directory's children while you type a path
    /// that names nothing is a picker showing the wrong place.
    #[test]
    fn nothing_found_shows_nothing() {
        let mut palette = Palette::browsing(vec![row("a"), row("b")]);
        palette.replace_entries(Vec::new());
        assert!(palette.visible().is_empty());
        assert_eq!(palette.current(), None);
    }
}

#[cfg(test)]
mod text_source_tests {
    use super::*;

    fn row(label: &str) -> Entry {
        Entry {
            label: label.to_string(),
            hint: String::new(),
            command: Command::LaunchFleet {
                agents: vec![label.to_string()],
            },
        }
    }

    /// The line is what will be sent, not a filter over the rows. Narrowing
    /// the crews by the task empties the list on its first word, and there is
    /// then nothing to press Enter on.
    #[test]
    fn typing_a_task_does_not_narrow_the_crews() {
        let mut palette = Palette::writing(vec![row("claude x2"), row("codex x1")]);
        assert_eq!(palette.source, Source::Text);
        palette.query = "fix the flaky test".into();
        assert_eq!(palette.visible().len(), 2);
        assert!(palette.current().is_some());
    }

    /// Moving between crews leaves the task alone.
    #[test]
    fn choosing_a_crew_leaves_the_task_alone() {
        let mut palette = Palette::writing(vec![row("a"), row("b"), row("c")]);
        palette.query = "rewrite the parser".into();
        palette.step(2);
        assert_eq!(palette.query, "rewrite the parser");
        assert_eq!(palette.current().map(|e| e.label.as_str()), Some("c"));
    }

    /// A complaint starts absent, and it is not one of the rows: it is about
    /// the attempt, and the rows are still what can be pressed.
    #[test]
    fn a_new_palette_has_nothing_to_complain_about() {
        let palette = Palette::writing(vec![row("a")]);
        assert_eq!(palette.error, None);
        assert_eq!(palette.visible().len(), 1);
    }
}

/// How many rows a palette shows at once.
///
/// Enough that the answer is usually visible without typing, few enough that
/// the card does not become the window. Here rather than at the paint site
/// because the hit-testing has to agree with it.
pub const MAX_ROWS: usize = 12;

/// Where an open palette is on screen.
///
/// One place, so a row that is drawn somewhere is a row that is clicked there.
/// The previous front end learned this the hard way with its menu: laid out in
/// one function and hit-tested in another, and a row could be pressed where it
/// was not drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    /// How tall one row is, which is also the height of the line above them.
    pub row_height: f32,
    pub rows: usize,
    pub leading_rows: usize,
    pub trailing_rows: usize,
}

impl Geometry {
    /// Lay a palette out in a window `window_width` wide.
    pub fn new(window_width: f32, cell: (f32, f32), rows: usize, has_error: bool) -> Self {
        Self::framed(window_width, cell, rows, has_error, 1, 0)
    }

    /// A search palette with a heading above its query line.
    pub fn titled(window_width: f32, cell: (f32, f32), rows: usize, has_error: bool) -> Self {
        Self::framed(window_width, cell, rows, has_error, 2, 0)
    }

    /// A confirmation: a heading, the answers, and a line of keyboard
    /// help under them. No query line -- there is nothing to search.
    pub fn confirming(window_width: f32, cell: (f32, f32), rows: usize, has_error: bool) -> Self {
        Self::framed(window_width, cell, rows, has_error, 2, 2)
    }

    /// Lay out a card with a known number of non-selectable lines around its
    /// selectable rows. Shell selection uses a title and subtitle above the
    /// rows and a keyboard hint below them.
    pub fn framed(
        window_width: f32,
        cell: (f32, f32),
        rows: usize,
        has_error: bool,
        leading_rows: usize,
        trailing_rows: usize,
    ) -> Self {
        let (cell_width, cell_height) = cell;
        let width = (window_width * 0.6)
            .max(cell_width * 24.0)
            .min(cell_width * 76.0)
            .min((window_width - cell_width * 4.0).max(cell_width * 24.0))
            .min(window_width);
        let lines = rows + leading_rows + trailing_rows + usize::from(has_error);
        Self {
            left: ((window_width - width) / 2.0).max(0.0),
            top: cell_height * 2.0,
            width,
            height: cell_height * lines as f32,
            row_height: cell_height,
            rows,
            leading_rows,
            trailing_rows,
        }
    }

    /// Which row a point is over, if any.
    ///
    /// The line being typed into is not a row: pressing it should not run
    /// whatever happens to be selected.
    pub fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        if !self.contains(x, y) {
            return None;
        }
        let below_query = y - (self.top + self.row_height * self.leading_rows as f32);
        if below_query < 0.0 {
            return None;
        }
        let row = (below_query / self.row_height.max(1.0)) as usize;
        (row < self.rows).then_some(row)
    }

    /// Whether a point is anywhere on the card.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.left && x < self.left + self.width && y >= self.top && y < self.top + self.height
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::*;

    const CELL: (f32, f32) = (10.0, 20.0);

    fn card(rows: usize) -> Geometry {
        Geometry::new(1000.0, CELL, rows, false)
    }

    /// A row is clicked where it is drawn. The rows start one line down,
    /// because the first line is what is typed into.
    #[test]
    fn a_point_lands_on_the_row_that_is_drawn_there() {
        let card = card(4);
        assert_eq!(card.row_at(card.left + 5.0, card.top + 25.0), Some(0));
        assert_eq!(card.row_at(card.left + 5.0, card.top + 45.0), Some(1));
        assert_eq!(card.row_at(card.left + 5.0, card.top + 85.0), Some(3));
    }

    /// The line being typed into is not a row: pressing it should not run
    /// whichever row happens to be selected.
    #[test]
    fn the_line_that_is_typed_into_is_not_a_row() {
        let card = card(4);
        assert_eq!(card.row_at(card.left + 5.0, card.top + 1.0), None);
        assert_eq!(card.row_at(card.left + 5.0, card.top + 19.0), None);
    }

    /// And nothing outside the card is a row, or a click meant for the
    /// terminal runs a command instead.
    #[test]
    fn nothing_outside_the_card_is_a_row() {
        let card = card(4);
        assert_eq!(card.row_at(card.left - 1.0, card.top + 25.0), None);
        assert_eq!(card.row_at(card.left + card.width, card.top + 25.0), None);
        assert_eq!(card.row_at(card.left + 5.0, card.top - 1.0), None);
        assert_eq!(card.row_at(card.left + 5.0, card.top + card.height), None);
    }

    /// Past the last row is not the last row. The card is a line taller when
    /// it is complaining about something, and that line is not pressable.
    #[test]
    fn the_space_past_the_last_row_is_not_a_row() {
        let card = Geometry::new(1000.0, CELL, 3, true);
        assert_eq!(card.row_at(card.left + 5.0, card.top + 65.0), Some(2));
        assert_eq!(card.row_at(card.left + 5.0, card.top + 85.0), None);
        assert!(card.contains(card.left + 5.0, card.top + 85.0));
    }

    /// An empty palette has no rows to press, and is still a card.
    #[test]
    fn an_empty_palette_has_no_rows() {
        let card = card(0);
        assert_eq!(card.row_at(card.left + 5.0, card.top + 25.0), None);
        assert!(card.contains(card.left + 5.0, card.top + 5.0));
    }

    /// The card stays on screen in a window narrower than its own minimum.
    #[test]
    fn a_narrow_window_does_not_push_the_card_off_the_edge() {
        let card = Geometry::new(100.0, CELL, 3, false);
        assert!(card.left >= 0.0);
        assert!(card.left + card.width <= 100.0 + 0.001);
    }
}

#[cfg(test)]
mod typing_tests {
    use super::*;

    /// Space arrives as a named key, not as a character. Losing it meant no
    /// query could contain one: `new tab` typed as `newtab` matched nothing,
    /// and a task written on the fleet card arrived as a single word.
    #[test]
    fn a_space_is_typed_rather_than_swallowed() {
        assert_eq!(
            key_for(Some("Space"), None, false),
            Key::Type(" ".to_string())
        );
    }

    /// Ctrl+Space is not a space: it is a chord, and passing it through would
    /// put a space in the query every time somebody reached for one.
    #[test]
    fn control_space_is_not_a_space() {
        assert_eq!(key_for(Some("Space"), None, true), Key::NotOurs);
    }

    /// Ordinary letters still arrive as characters.
    #[test]
    fn letters_are_typed() {
        assert_eq!(key_for(None, Some("n"), false), Key::Type("n".to_string()));
        assert_eq!(key_for(None, Some("n"), true), Key::NotOurs);
    }

    /// And a query with a space in it finds the row that has one.
    #[test]
    fn a_query_with_a_space_matches_a_label_with_one() {
        let mut palette = Palette::new(vec![
            Entry {
                label: "New Tab".into(),
                hint: String::new(),
                command: Command::Action(crate::keys::Action::NewTab),
            },
            Entry {
                label: "Notebook".into(),
                hint: String::new(),
                command: Command::Action(crate::keys::Action::Copy),
            },
        ]);
        palette.query = "new t".into();
        palette.refilter();
        assert_eq!(
            palette.visible().first().map(|entry| entry.label.as_str()),
            Some("New Tab")
        );
    }
}
