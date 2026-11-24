//! Finding something in what has already scrolled past.
//!
//! The engine does the matching -- it owns the scrollback and knows where the
//! rows are. What lives here is the part a front end owns: what the bar says,
//! which match is current, and how moving between them behaves at the ends.

use unterm_engine::ScreenSearchMatch;

/// How the pattern reads. The bar's three modes; the kernel only speaks the
/// two literal ones, and the regex mode is matched by the front end over the
/// same lines -- the kernel's dependency budget has no room for a regex
/// engine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    CaseSensitive,
    #[default]
    CaseInsensitive,
    Regex,
}

/// An open search.
#[derive(Clone, Debug, Default)]
pub struct Search {
    pub pattern: String,
    pub matches: Vec<ScreenSearchMatch>,
    /// Which match is current, if there are any.
    pub selected: usize,
    /// How the pattern reads: ignore case to start, Ctrl+R to cycle.
    pub mode: Mode,
}

impl Search {
    /// The match the viewport should be showing.
    pub fn current(&self) -> Option<&ScreenSearchMatch> {
        self.matches.get(self.selected)
    }

    /// Move to the next match, wrapping.
    ///
    /// Wrapping because a search that stops at the last match leaves the user
    /// pressing a key that does nothing, with no way to tell that from a
    /// search that has broken.
    pub fn step(&mut self, delta: isize) {
        if self.matches.is_empty() {
            self.selected = 0;
            return;
        }
        let len = self.matches.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
    }

    /// Keep the current match pointing at the same place after a re-search.
    ///
    /// Typing another character narrows the results; jumping back to the
    /// first one every keystroke drags the viewport away from what the user
    /// was already looking at.
    pub fn adopt(&mut self, matches: Vec<ScreenSearchMatch>) {
        let previous = self.current().map(|found| (found.row, found.col));
        self.selected = previous
            .and_then(|(row, col)| {
                matches
                    .iter()
                    .position(|found| found.row == row && found.col == col)
            })
            .unwrap_or(0);
        self.matches = matches;
        if self.selected >= self.matches.len() {
            self.selected = 0;
        }
    }

    /// The next way of reading the pattern, in the order 0.57.4 cycled:
    /// exact, then ignore case, then regex, then around again.
    pub fn cycle_mode(&mut self) {
        self.mode = match self.mode {
            Mode::CaseSensitive => Mode::CaseInsensitive,
            Mode::CaseInsensitive => Mode::Regex,
            Mode::Regex => Mode::CaseSensitive,
        };
    }

    /// What the bar says.
    ///
    /// The mode is always named, the way 0.57.4's bar named it: cycling with
    /// Ctrl+R has to show something even when the matches do not change.
    pub fn label(&self) -> String {
        let mode = match self.mode {
            Mode::CaseSensitive => "case-sensitive",
            Mode::CaseInsensitive => "ignore case",
            Mode::Regex => "regex",
        };
        if self.pattern.is_empty() {
            return format!("Search:  ({mode})");
        }
        if self.matches.is_empty() {
            return format!("Search: {}  (no matches · {mode})", self.pattern);
        }
        format!(
            "Search: {}  ({}/{} · {mode})",
            self.pattern,
            self.selected + 1,
            self.matches.len()
        )
    }

    /// Build a prompt containing the currently selected search match plus
    /// surrounding context lines to send directly to an active AI Agent pane.
    pub fn agent_context_prompt(
        &self,
        lines: &[String],
        first_row: i64,
        is_zh: bool,
    ) -> Option<String> {
        let current = self.current()?;
        if lines.is_empty() {
            return None;
        }
        let rel = (current.row - first_row).clamp(0, lines.len().saturating_sub(1) as i64) as usize;
        let start = rel.saturating_sub(4);
        let end = (rel + 5).min(lines.len());
        let excerpt = lines[start..end]
            .iter()
            .map(|l| l.trim_end())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ⏎ ");
        if excerpt.is_empty() {
            return None;
        }
        let pos = format!("{}/{}", self.selected + 1, self.matches.len());
        if is_zh {
            Some(format!(
                "请分析终端搜索命中 `{}` ({pos}) 的上下文：{excerpt}\r",
                self.pattern
            ))
        } else {
            Some(format!(
                "Please analyze this terminal search match `{}` ({pos}) with context: {excerpt}\r",
                self.pattern
            ))
        }
    }
}

/// What a key press means to an open search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// Extend the pattern, and search again.
    Type(String),
    /// Shorten the pattern, and search again.
    Backspace,
    /// Drop the whole pattern, and search again from nothing.
    Clear,
    /// Read the pattern the next way, and search again.
    CycleMode,
    /// Send the current match and surrounding context to the active AI Agent pane.
    SendToAgent,
    /// Move through the results.
    Step(isize),
    /// Close the search.
    Close,
    /// Not the search's: the shell should have it.
    NotOurs,
}

/// Decide what an open search does with a key.
///
/// Separated from the window so the rules can be checked without a keyboard:
/// which keys the search takes decides which keys the shell never sees, and
/// taking one too many is how a search bar breaks a program underneath it.
pub fn key_for(named: Option<&str>, character: Option<&str>, ctrl: bool, shift: bool) -> Key {
    match named {
        Some("Escape") => return Key::Close,
        Some("Enter") => return Key::Step(if shift { -1 } else { 1 }),
        // The arrows walk the matches too, as they did before: down is
        // onward through the list, up is back.
        Some("ArrowDown") => return Key::Step(1),
        Some("ArrowUp") => return Key::Step(-1),
        Some("Backspace") => return Key::Backspace,
        Some("Space") if !ctrl => return Key::Type(" ".to_string()),
        _ => {}
    }
    match character {
        // Ctrl+U clears the line wholesale, the way readline does.
        Some(text) if ctrl && text.eq_ignore_ascii_case("u") => Key::Clear,
        // Ctrl+R cycles how the pattern reads, as it always has.
        Some(text) if ctrl && text.eq_ignore_ascii_case("r") => Key::CycleMode,
        // Ctrl+S feeds the current match + surrounding context to the active AI agent pane.
        Some(text) if ctrl && text.eq_ignore_ascii_case("s") => Key::SendToAgent,
        // Ctrl+something else is a command, not text to search for.
        Some(text) if !ctrl => Key::Type(text.to_string()),
        _ => Key::NotOurs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(row: i64, col: usize) -> ScreenSearchMatch {
        ScreenSearchMatch {
            row,
            col,
            text: "x".to_string(),
        }
    }

    fn search(rows: &[i64]) -> Search {
        Search {
            pattern: "x".to_string(),
            matches: rows.iter().map(|row| found(*row, 0)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn stepping_past_the_last_match_wraps_to_the_first() {
        let mut s = search(&[1, 2, 3]);
        s.step(1);
        s.step(1);
        assert_eq!(s.selected, 2);
        s.step(1);
        assert_eq!(s.selected, 0, "a key that does nothing reads as broken");
    }

    #[test]
    fn stepping_back_from_the_first_wraps_to_the_last() {
        let mut s = search(&[1, 2, 3]);
        s.step(-1);
        assert_eq!(s.selected, 2);
    }

    #[test]
    fn stepping_with_no_matches_answers_rather_than_dividing_by_zero() {
        let mut s = Search::default();
        s.step(1);
        assert_eq!(s.selected, 0);
        assert!(s.current().is_none());
    }

    #[test]
    fn narrowing_the_pattern_keeps_the_match_the_user_was_looking_at() {
        let mut s = search(&[10, 20, 30]);
        s.step(1);
        assert_eq!(s.current().map(|f| f.row), Some(20));

        // The second match survives the narrower search; the others do not.
        s.adopt(vec![found(20, 0), found(40, 0)]);
        assert_eq!(
            s.current().map(|f| f.row),
            Some(20),
            "the viewport should not jump away from what was on screen"
        );
    }

    #[test]
    fn a_match_that_no_longer_exists_falls_back_to_the_first() {
        let mut s = search(&[10, 20, 30]);
        s.step(1);
        s.adopt(vec![found(50, 0), found(60, 0)]);
        assert_eq!(s.current().map(|f| f.row), Some(50));
    }

    #[test]
    fn searching_for_nothing_leaves_the_selection_somewhere_defined() {
        let mut s = search(&[10, 20]);
        s.step(1);
        s.adopt(Vec::new());
        assert_eq!(s.selected, 0);
        assert!(s.current().is_none());
    }

    #[test]
    fn the_bar_says_where_in_the_results_you_are() {
        let mut s = search(&[1, 2, 3]);
        assert_eq!(s.label(), "Search: x  (1/3 · ignore case)");
        s.step(1);
        assert_eq!(s.label(), "Search: x  (2/3 · ignore case)");
    }

    #[test]
    fn a_search_with_no_results_says_so_rather_than_showing_zero_of_zero() {
        let s = Search {
            pattern: "nope".to_string(),
            ..Default::default()
        };
        assert_eq!(s.label(), "Search: nope  (no matches · ignore case)");
    }

    #[test]
    fn the_modes_cycle_in_the_order_they_always_have() {
        // Exact, ignore case, regex, around again — 0.57.4's order, and a
        // fresh search starts on ignore case as it did there.
        let mut s = Search::default();
        assert_eq!(s.mode, Mode::CaseInsensitive);
        s.cycle_mode();
        assert_eq!(s.mode, Mode::Regex);
        s.cycle_mode();
        assert_eq!(s.mode, Mode::CaseSensitive);
        s.cycle_mode();
        assert_eq!(s.mode, Mode::CaseInsensitive);
    }

    #[test]
    fn the_bar_names_the_mode_even_before_anything_is_typed() {
        let mut s = Search::default();
        assert_eq!(s.label(), "Search:  (ignore case)");
        s.cycle_mode();
        assert_eq!(s.label(), "Search:  (regex)");
        s.cycle_mode();
        assert_eq!(s.label(), "Search:  (case-sensitive)");
    }

    #[test]
    fn typing_extends_the_pattern() {
        assert_eq!(
            key_for(None, Some("a"), false, false),
            Key::Type("a".to_string())
        );
        assert_eq!(
            key_for(None, Some("中"), false, false),
            Key::Type("中".to_string())
        );
    }

    #[test]
    fn enter_steps_and_shift_enter_steps_back() {
        assert_eq!(key_for(Some("Enter"), None, false, false), Key::Step(1));
        assert_eq!(key_for(Some("Enter"), None, false, true), Key::Step(-1));
    }

    #[test]
    fn escape_closes_and_backspace_shortens() {
        assert_eq!(key_for(Some("Escape"), None, false, false), Key::Close);
        assert_eq!(
            key_for(Some("Backspace"), None, false, false),
            Key::Backspace
        );
    }

    #[test]
    fn a_control_chord_is_not_text_to_search_for() {
        // Ctrl+C while searching has to reach the shell, or a program that
        // is running under the search can never be stopped.
        assert_eq!(key_for(None, Some("c"), true, false), Key::NotOurs);
    }

    #[test]
    fn a_key_the_search_has_no_use_for_reaches_the_shell() {
        assert_eq!(key_for(Some("F5"), None, false, false), Key::NotOurs);
    }

    #[test]
    fn arrows_walk_the_matches_and_ctrl_u_clears_the_line() {
        assert_eq!(key_for(Some("ArrowDown"), None, false, false), Key::Step(1));
        assert_eq!(key_for(Some("ArrowUp"), None, false, false), Key::Step(-1));
        assert_eq!(key_for(None, Some("u"), true, false), Key::Clear);
    }

    #[test]
    fn ctrl_r_cycles_the_mode_and_a_plain_r_is_still_text() {
        assert_eq!(key_for(None, Some("r"), true, false), Key::CycleMode);
        assert_eq!(key_for(None, Some("R"), true, false), Key::CycleMode);
        assert_eq!(
            key_for(None, Some("r"), false, false),
            Key::Type("r".to_string())
        );
    }

    #[test]
    fn ctrl_s_sends_match_context_to_agent() {
        assert_eq!(key_for(None, Some("s"), true, false), Key::SendToAgent);
        assert_eq!(key_for(None, Some("S"), true, false), Key::SendToAgent);
        let s = Search {
            pattern: "E0308".to_string(),
            matches: vec![found(12, 0)],
            selected: 0,
            mode: Mode::CaseSensitive,
        };
        let lines = vec![
            "Compiling unterm-app".to_string(),
            "error[E0308]: mismatched types".to_string(),
            "  --> src/window.rs:42:5".to_string(),
        ];
        let prompt = s.agent_context_prompt(&lines, 11, false).unwrap();
        assert!(prompt.contains("E0308"));
        assert!(prompt.contains("1/1"));
        assert!(prompt.contains("mismatched types"));
    }
}
