//! Selecting text without a mouse, and grabbing what is already on screen.
//!
//! Two features that answer the same complaint. Copy mode moves a cursor with
//! the keyboard and extends a selection from it, which is the only way to copy
//! from a terminal over ssh on a laptop with no mouse. Quick select labels
//! every URL, path and hash on screen with a letter, so the common case --
//! "copy that one thing" -- takes two keystrokes instead of a careful drag.
//!
//! Both are pure decisions over a screen: where the cursor goes, what a key
//! means, which spans get labelled. The window draws the result.

use unterm_engine::StyledScreenLine;

/// How a selection grows from its anchor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectKind {
    /// Character by character, vim's `v`.
    #[default]
    Cell,
    /// Whole rows, vim's `V`.
    Line,
    /// A rectangle, vim's `Ctrl+v`.
    Block,
}

/// Where copy mode's cursor is and what it has selected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CopyMode {
    pub row: usize,
    pub column: usize,
    /// Where the selection started, if one is being made.
    pub anchor: Option<(usize, usize)>,
    /// The shape the selection takes.
    pub kind: SelectKind,
    /// An `f`/`t` waiting for its target: the next key is a character to jump
    /// to, not a motion. (forward, till).
    pub pending_find: Option<(bool, bool)>,
    /// What `;` and `,` repeat: the last find's (target, forward, till).
    pub last_find: Option<(char, bool, bool)>,
}

/// What a key does in copy mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
    Top,
    Bottom,
    WordLeft,
    WordRight,
    /// Start or stop extending a selection.
    ToggleSelection,
    /// As ToggleSelection, but whole rows: vim's `V`.
    ToggleLineSelection,
    /// As ToggleSelection, but a rectangle: vim's `Ctrl+v`.
    ToggleBlockSelection,
    /// vim's `f`/`F`/`t`/`T`: arm a jump to the next typed character. `till`
    /// stops one short of it, the way `t` does.
    Find {
        forward: bool,
        till: bool,
    },
    /// vim's `;` and `,`: the last find again, in the same or the opposite
    /// direction.
    RepeatFind(bool),
    /// Copy what is selected and leave.
    Yank,
    Leave,
}

/// Vim's keys, plus the arrows for people who do not use them.
pub fn motion_for(named: Option<&str>, character: Option<&str>, ctrl: bool) -> Option<Motion> {
    if ctrl {
        return match character {
            Some(text) if text.eq_ignore_ascii_case("v") => Some(Motion::ToggleBlockSelection),
            _ => None,
        };
    }
    match named {
        Some("Escape") => return Some(Motion::Leave),
        Some("ArrowLeft") => return Some(Motion::Left),
        Some("ArrowRight") => return Some(Motion::Right),
        Some("ArrowUp") => return Some(Motion::Up),
        Some("ArrowDown") => return Some(Motion::Down),
        Some("Home") => return Some(Motion::LineStart),
        Some("End") => return Some(Motion::LineEnd),
        Some("Enter") => return Some(Motion::Yank),
        _ => {}
    }
    match character? {
        "h" => Some(Motion::Left),
        "j" => Some(Motion::Down),
        "k" => Some(Motion::Up),
        "l" => Some(Motion::Right),
        "0" => Some(Motion::LineStart),
        "$" => Some(Motion::LineEnd),
        "g" => Some(Motion::Top),
        "G" => Some(Motion::Bottom),
        "b" => Some(Motion::WordLeft),
        "w" => Some(Motion::WordRight),
        "f" => Some(Motion::Find {
            forward: true,
            till: false,
        }),
        "F" => Some(Motion::Find {
            forward: false,
            till: false,
        }),
        "t" => Some(Motion::Find {
            forward: true,
            till: true,
        }),
        "T" => Some(Motion::Find {
            forward: false,
            till: true,
        }),
        ";" => Some(Motion::RepeatFind(true)),
        "," => Some(Motion::RepeatFind(false)),
        "v" | " " => Some(Motion::ToggleSelection),
        "V" => Some(Motion::ToggleLineSelection),
        "y" => Some(Motion::Yank),
        "q" => Some(Motion::Leave),
        _ => None,
    }
}

impl CopyMode {
    /// Apply a motion against a screen of `rows` lines.
    ///
    /// `line_width` gives the length of a row, so `$` lands on the last
    /// character rather than in the blank space a terminal pads rows with.
    pub fn apply(&mut self, motion: Motion, rows: usize, line_width: impl Fn(usize) -> usize) {
        let last_row = rows.saturating_sub(1);
        let width = |row: usize| line_width(row).saturating_sub(1);

        match motion {
            Motion::Left => self.column = self.column.saturating_sub(1),
            Motion::Right => self.column = (self.column + 1).min(width(self.row)),
            Motion::Up => {
                self.row = self.row.saturating_sub(1);
                self.column = self.column.min(width(self.row));
            }
            Motion::Down => {
                self.row = (self.row + 1).min(last_row);
                self.column = self.column.min(width(self.row));
            }
            Motion::LineStart => self.column = 0,
            Motion::LineEnd => self.column = width(self.row),
            Motion::Top => {
                self.row = 0;
                self.column = self.column.min(width(0));
            }
            Motion::Bottom => {
                self.row = last_row;
                self.column = self.column.min(width(last_row));
            }
            // Handled by the window, which has the screen's text; the plain
            // width this function is given is not enough to find a character.
            Motion::WordLeft | Motion::WordRight | Motion::Find { .. } | Motion::RepeatFind(_) => {}
            Motion::ToggleSelection => self.toggle(SelectKind::Cell),
            Motion::ToggleLineSelection => self.toggle(SelectKind::Line),
            Motion::ToggleBlockSelection => self.toggle(SelectKind::Block),
            Motion::Yank | Motion::Leave => {}
        }
    }

    /// `w` and `b`: to the start of the next word, or back to the start of
    /// the previous one, wrapping across lines the way vim does.
    pub fn apply_word(&mut self, motion: Motion, rows: usize, line_text: impl Fn(usize) -> String) {
        let last_row = rows.saturating_sub(1);
        let chars = |row: usize| line_text(row).chars().collect::<Vec<char>>();
        match motion {
            Motion::WordRight => {
                let line = chars(self.row);
                let mut at = self.column.min(line.len());
                while at < line.len() && !line[at].is_whitespace() {
                    at += 1;
                }
                while at < line.len() && line[at].is_whitespace() {
                    at += 1;
                }
                if at < line.len() {
                    self.column = at;
                } else if self.row < last_row {
                    self.row += 1;
                    let next = chars(self.row);
                    self.column = next.iter().position(|ch| !ch.is_whitespace()).unwrap_or(0);
                } else {
                    self.column = line.len().saturating_sub(1);
                }
            }
            Motion::WordLeft => {
                let mut row = self.row;
                let mut line = chars(row);
                let mut at = self.column.min(line.len());
                loop {
                    while at > 0 && line[at - 1].is_whitespace() {
                        at -= 1;
                    }
                    // Nothing but whitespace behind the cursor on this line:
                    // the previous word lives on an earlier one.
                    if at == 0 {
                        if row == 0 {
                            break;
                        }
                        row -= 1;
                        line = chars(row);
                        at = line.len();
                        continue;
                    }
                    while at > 0 && !line[at - 1].is_whitespace() {
                        at -= 1;
                    }
                    break;
                }
                self.row = row;
                self.column = at;
            }
            _ => {}
        }
    }

    /// `f`/`F`/`t`/`T`, once the target is known: jump within the current
    /// line, or stay put when the line has no such character ahead.
    ///
    /// Remembered for `;` and `,` even on a miss, the way vim does: the
    /// character you asked for may be on the line the cursor moves to next.
    pub fn apply_find(&mut self, target: char, forward: bool, till: bool, line: &str) {
        self.last_find = Some((target, forward, till));
        let chars: Vec<char> = line.chars().collect();
        let found = if forward {
            (self.column + 1..chars.len()).find(|&at| chars[at] == target)
        } else {
            (0..self.column.min(chars.len()))
                .rev()
                .find(|&at| chars[at] == target)
        };
        let Some(at) = found else {
            return;
        };
        self.column = if till {
            // One short of the target, from whichever side the cursor came.
            if forward {
                at.saturating_sub(1)
            } else {
                at + 1
            }
        } else {
            at
        };
    }

    /// One toggle per shape: the same shape again ends the selection, a
    /// different one re-anchors it in the new shape from the same spot.
    fn toggle(&mut self, kind: SelectKind) {
        match self.anchor {
            Some(_) if self.kind == kind => self.anchor = None,
            Some(_) => self.kind = kind,
            None => {
                self.anchor = Some((self.row, self.column));
                self.kind = kind;
            }
        }
    }

    /// The selected range, as (start, end) in (row, column), start first.
    pub fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let anchor = self.anchor?;
        let cursor = (self.row, self.column);
        Some(if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        })
    }
}

#[cfg(test)]
mod word_motion_tests {
    use super::*;

    fn text(row: usize) -> String {
        ["cargo build --release", "  done ok", ""][row.min(2)].to_string()
    }

    #[test]
    fn w_hops_word_starts_and_wraps_to_the_next_line() {
        let mut mode = CopyMode::default();
        mode.apply_word(Motion::WordRight, 3, text);
        assert_eq!((mode.row, mode.column), (0, 6));
        mode.apply_word(Motion::WordRight, 3, text);
        assert_eq!((mode.row, mode.column), (0, 12));
        mode.apply_word(Motion::WordRight, 3, text);
        assert_eq!((mode.row, mode.column), (1, 2), "wraps to the next word");
    }

    #[test]
    fn f_jumps_forward_to_the_character_and_t_stops_short() {
        let line = "cargo build --release";
        let mut mode = CopyMode::default();
        mode.apply_find('l', true, false, line);
        assert_eq!(mode.column, 9, "the first `l` ahead of the cursor");
        assert_eq!(mode.last_find, Some(('l', true, false)));

        let mut till = CopyMode::default();
        till.apply_find('l', true, true, line);
        assert_eq!(till.column, 8, "`t` stops one short of the target");
    }

    #[test]
    fn a_character_the_line_does_not_have_moves_nothing() {
        let mut mode = CopyMode {
            column: 3,
            ..Default::default()
        };
        mode.apply_find('z', true, false, "cargo build");
        assert_eq!(mode.column, 3);
        assert_eq!(
            mode.last_find,
            Some(('z', true, false)),
            "remembered anyway, so `;` can try again on another line"
        );
    }

    #[test]
    fn semicolon_repeats_the_find_from_where_it_landed() {
        let line = "cargo build --release";
        let mut mode = CopyMode::default();
        mode.apply_find('l', true, false, line);
        assert_eq!(mode.column, 9);
        // `;` is the same find again: the window replays last_find.
        let (target, forward, till) = mode.last_find.expect("a find to repeat");
        mode.apply_find(target, forward, till, line);
        assert_eq!(mode.column, 16, "the next `l` along");
        // `,` is the same find, mirrored.
        mode.apply_find(target, !forward, till, line);
        assert_eq!(mode.column, 9, "back to the previous `l`");
    }

    #[test]
    fn b_walks_back_to_word_starts_across_lines() {
        let mut mode = CopyMode {
            row: 1,
            column: 2,
            ..Default::default()
        };
        mode.apply_word(Motion::WordLeft, 3, text);
        assert_eq!((mode.row, mode.column), (0, 12));
        mode.apply_word(Motion::WordLeft, 3, text);
        assert_eq!((mode.row, mode.column), (0, 6));
        mode.apply_word(Motion::WordLeft, 3, text);
        assert_eq!((mode.row, mode.column), (0, 0));
    }
}

/// A span quick select has labelled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Labelled {
    pub label: String,
    pub row: usize,
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// The alphabet labels are drawn from: `quick_select_alphabet` in the config,
/// or 0.57.4's default when the config is silent. Resolved in the settings
/// layer so an unusable alphabet has already been rejected by the time labels
/// are computed.
fn label_alphabet() -> String {
    unterm_services::settings::current()
        .quick_select_alphabet
        .clone()
}

/// Everything on screen worth grabbing, labelled.
///
/// Nearest to the bottom first, because the thing you want is almost always
/// the thing that just scrolled past.
pub fn labelled(lines: &[StyledScreenLine]) -> Vec<Labelled> {
    let mut found: Vec<(usize, usize, usize, String)> = Vec::new();

    for (row, line) in lines.iter().enumerate() {
        let mut text = String::new();
        let mut columns: Vec<usize> = Vec::new();
        let mut column = 0usize;
        for cell in &line.cells {
            // Spacers after wide characters hold no columns of their own.
            if cell.width == 0 {
                continue;
            }
            text.push(cell.ch);
            columns.push(column);
            column += cell.width;
        }
        columns.push(column);

        for (start, end) in interesting_spans(&text) {
            found.push((
                row,
                columns[start],
                columns[end],
                text.chars().skip(start).take(end - start).collect(),
            ));
        }
    }

    // Bottom-up: the thing you want is usually the thing that just appeared.
    found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let labels = labels_for(found.len());
    found
        .into_iter()
        .zip(labels)
        .map(|((row, start, end, text), label)| Labelled {
            label,
            row,
            start,
            end,
            text,
        })
        .collect()
}

/// Exactly `count` labels, none of them a prefix of another.
///
/// That last part is the whole algorithm, and getting it wrong is not subtle.
/// Single letters first and pairs afterwards looks right and is broken: `a`
/// and `aa` both exist, `a` matches the moment it is typed, and every label
/// starting with `a` becomes unreachable. Which is what happened here -- with
/// more than twenty things on screen, most of the hints could not be typed.
///
/// Instead letters are *taken* from the end of the alphabet to become the
/// first character of pairs, so no letter is ever both a label and a prefix.
/// One match more than the alphabet has letters means the last letter stops
/// being a label of its own and becomes the prefix for as many pairs as are
/// still needed.
///
/// From tmux-thumbs by way of the previous front end, which is where the
/// property came from in the first place.
pub fn labels_for(count: usize) -> Vec<String> {
    labels_from(&label_alphabet(), count)
}

/// Exactly `count` labels drawn from `alphabet`. Split from `labels_for` so
/// the algorithm can be tested without touching the process-wide settings.
fn labels_from(alphabet: &str, count: usize) -> Vec<String> {
    let alphabet: Vec<String> = alphabet.chars().map(|ch| ch.to_string()).collect();
    let mut single = alphabet.clone();
    let mut paired: Vec<String> = Vec::new();

    while single.len() + paired.len() < count {
        let Some(prefix) = single.pop() else {
            break;
        };
        // Only as many pairs as are still short, and taken from the front of
        // the alphabet -- the prefix came off the back, so the two cannot
        // collide.
        let wanted = count.saturating_sub(single.len() + paired.len());
        let prefixed: Vec<String> = alphabet
            .iter()
            .take(wanted)
            .map(|second| format!("{prefix}{second}"))
            .collect();
        paired.splice(0..0, prefixed);
    }

    let pairs = paired.len();
    single
        .into_iter()
        .take(count.saturating_sub(pairs))
        .chain(paired)
        .collect()
}

/// Character ranges worth offering: URLs, paths, hashes, quoted strings.
fn interesting_spans(text: &str) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut index = 0usize;

    while index < chars.len() {
        if chars[index].is_whitespace() {
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len() && !chars[index].is_whitespace() {
            index += 1;
        }
        let word: String = chars[start..index].iter().collect();
        let trimmed = word.trim_end_matches(['.', ',', ';', ':', ')', ']', '"', '\'']);
        let end = start + trimmed.chars().count();
        if end > start && is_interesting(trimmed) {
            spans.push((start, end));
        }
    }
    spans
}

/// Whether a word is the kind of thing someone copies.
///
/// Deliberately narrow. Labelling every word on screen turns the display into
/// a wall of letters and makes the useful ones harder to find than the text
/// they cover.
fn is_interesting(word: &str) -> bool {
    if word.len() < 4 {
        return false;
    }
    let looks_like_url = ["http://", "https://", "ftp://", "file://", "git@"]
        .iter()
        .any(|scheme| word.starts_with(scheme));
    let looks_like_path = word.contains('/') || (word.contains('\\') && word.contains(':'));
    // A hash: long, and hexadecimal all the way through.
    let looks_like_hash = word.len() >= 7 && word.chars().all(|ch| ch.is_ascii_hexdigit());
    let looks_like_ip = word.split('.').count() == 4
        && word
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()));
    // The rest of 0.57.4's catalogue, word-shaped: colours, UUIDs, hex
    // addresses, container digests, IPFS hashes, IPv6, and long numbers.
    let hex_run = |text: &str| !text.is_empty() && text.chars().all(|ch| ch.is_ascii_hexdigit());
    let looks_like_color = word.len() == 7 && word.starts_with('#') && hex_run(&word[1..]);
    let looks_like_uuid = {
        let parts: Vec<&str> = word.split('-').collect();
        parts.len() == 5
            && [8, 4, 4, 4, 12]
                .iter()
                .zip(&parts)
                .all(|(len, part)| part.len() == *len && hex_run(part))
    };
    let looks_like_address = word.len() >= 6
        && (word.starts_with("0x") || word.starts_with("0X"))
        && hex_run(&word[2..]);
    let looks_like_digest = word
        .strip_prefix("sha256:")
        .map(|rest| rest.len() == 64 && hex_run(rest))
        .unwrap_or(false);
    let looks_like_ipfs = word.len() == 46 && word.starts_with("Qm");
    let looks_like_ipv6 = word.matches(':').count() >= 2
        && word.contains(':')
        && word
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() || ch == ':' || ch == '%');
    let looks_like_number = word.chars().all(|ch| ch.is_ascii_digit());

    looks_like_url
        || looks_like_path
        || looks_like_hash
        || looks_like_ip
        || looks_like_color
        || looks_like_uuid
        || looks_like_address
        || looks_like_digest
        || looks_like_ipfs
        || looks_like_ipv6
        || looks_like_number
}

#[cfg(test)]
mod tests {
    use super::*;
    use unterm_engine::{CellStyle, StyledCell};

    fn line(text: &str) -> StyledScreenLine {
        StyledScreenLine {
            row: 0,
            wrapped: false,
            cells: text
                .chars()
                .map(|ch| StyledCell {
                    ch,
                    style: CellStyle::default(),
                    width: 1,
                    combining: None,
                })
                .collect(),
        }
    }

    fn width_of(_: usize) -> usize {
        20
    }

    #[test]
    fn vim_keys_and_arrows_both_move() {
        assert_eq!(motion_for(None, Some("h"), false), Some(Motion::Left));
        assert_eq!(
            motion_for(Some("ArrowLeft"), None, false),
            Some(Motion::Left)
        );
        assert_eq!(motion_for(None, Some("j"), false), Some(Motion::Down));
        assert_eq!(
            motion_for(Some("ArrowDown"), None, false),
            Some(Motion::Down)
        );
    }

    #[test]
    fn a_key_copy_mode_has_no_use_for_is_ignored_rather_than_guessed() {
        assert_eq!(motion_for(None, Some("z"), false), None);
        assert_eq!(motion_for(Some("F5"), None, false), None);
    }

    #[test]
    fn the_cursor_stops_at_the_edges() {
        let mut mode = CopyMode::default();
        mode.apply(Motion::Left, 5, width_of);
        assert_eq!((mode.row, mode.column), (0, 0), "no column -1");
        mode.apply(Motion::Up, 5, width_of);
        assert_eq!(mode.row, 0, "no row -1");

        mode.apply(Motion::Bottom, 5, width_of);
        assert_eq!(mode.row, 4);
        mode.apply(Motion::Down, 5, width_of);
        assert_eq!(mode.row, 4, "no row past the last");
    }

    #[test]
    fn line_end_lands_on_the_last_character_not_the_padding() {
        let mut mode = CopyMode::default();
        mode.apply(Motion::LineEnd, 5, |_| 7);
        assert_eq!(mode.column, 6, "a 7-column line ends at column 6");
    }

    #[test]
    fn moving_to_a_shorter_line_pulls_the_column_in() {
        let mut mode = CopyMode {
            row: 0,
            column: 15,
            ..Default::default()
        };
        mode.apply(Motion::Down, 3, |row| if row == 1 { 4 } else { 20 });
        assert_eq!(mode.column, 3, "the shorter line has no column 15");
    }

    #[test]
    fn v_selects_lines_ctrl_v_selects_a_block_and_repeats_end_it() {
        let mut mode = CopyMode::default();
        mode.apply(Motion::ToggleLineSelection, 10, width_of);
        assert_eq!(mode.kind, SelectKind::Line);
        assert!(mode.anchor.is_some());
        // A different shape re-anchors in place rather than ending.
        mode.apply(Motion::ToggleBlockSelection, 10, width_of);
        assert_eq!(mode.kind, SelectKind::Block);
        assert!(mode.anchor.is_some());
        // The same shape again ends the selection.
        mode.apply(Motion::ToggleBlockSelection, 10, width_of);
        assert!(mode.anchor.is_none());
    }

    #[test]
    fn a_selection_is_ordered_however_it_was_made() {
        let mut mode = CopyMode {
            row: 3,
            column: 5,
            ..Default::default()
        };
        mode.apply(Motion::ToggleSelection, 10, width_of);
        mode.apply(Motion::Up, 10, width_of);
        let (start, end) = mode.selection().expect("a selection");
        assert!(start <= end, "dragging backwards still selects forwards");
        assert_eq!(end, (3, 5));
    }

    #[test]
    fn toggling_twice_clears_the_selection() {
        let mut mode = CopyMode::default();
        mode.apply(Motion::ToggleSelection, 10, width_of);
        assert!(mode.selection().is_some());
        mode.apply(Motion::ToggleSelection, 10, width_of);
        assert!(mode.selection().is_none());
    }

    #[test]
    fn quick_select_finds_urls_paths_and_hashes() {
        let found = labelled(&[line(
            "see https://example.com/x and /usr/local/bin and 8f3a2b1c9d",
        )]);
        let texts: Vec<&str> = found.iter().map(|item| item.text.as_str()).collect();
        assert!(texts.contains(&"https://example.com/x"));
        assert!(texts.contains(&"/usr/local/bin"));
        assert!(texts.contains(&"8f3a2b1c9d"));
    }

    #[test]
    fn ordinary_words_are_not_labelled() {
        // Labelling every word turns the screen into a wall of letters.
        let found = labelled(&[line("the quick brown fox jumped over")]);
        assert!(found.is_empty(), "got {found:?}");
    }

    #[test]
    fn trailing_punctuation_is_left_out_of_the_span() {
        let found = labelled(&[line("go to https://example.com/page.")]);
        assert_eq!(found[0].text, "https://example.com/page");
    }

    #[test]
    fn the_nearest_match_gets_the_first_label() {
        // The thing you want is usually the thing that just scrolled past.
        let found = labelled(&[line("/first/path"), line("/second/path")]);
        assert_eq!(found[0].text, "/second/path");
        assert_eq!(found[0].label, "a");
    }

    #[test]
    fn labels_never_run_out() {
        let lines: Vec<_> = (0..40)
            .map(|i| line(&format!("/path/number/{i}")))
            .collect();
        let found = labelled(&lines);
        assert_eq!(found.len(), 40);
        let mut seen = std::collections::HashSet::new();
        for item in &found {
            assert!(
                seen.insert(item.label.clone()),
                "duplicate label {}",
                item.label
            );
        }
    }

    #[test]
    fn a_wide_character_shifts_the_columns_a_label_covers() {
        let mut line = line("中 /usr/bin");
        line.cells[0].width = 2;
        let found = labelled(&[line]);
        assert_eq!(
            found[0].start, 3,
            "two columns for the character, one space"
        );
    }
}

#[cfg(test)]
mod label_tests {
    use super::*;

    #[test]
    fn a_short_list_gets_single_letters() {
        assert_eq!(labels_for(3), vec!["a", "s", "d"]);
        assert_eq!(labels_for(0), Vec::<String>::new());
    }

    #[test]
    fn exactly_the_number_asked_for() {
        for count in [0, 1, 5, 20, 21, 40, 100, 399] {
            assert_eq!(labels_for(count).len(), count, "for {count}");
        }
    }

    /// The property everything else rests on. Without it a label that is also
    /// the start of another label fires the moment it is typed, and every
    /// longer label beginning with it can never be reached -- which is what
    /// singles-then-pairs did, and why most hints on a busy screen could not
    /// be typed at all.
    #[test]
    fn no_label_is_the_beginning_of_another() {
        for count in [1, 19, 20, 21, 25, 40, 60, 200, 399] {
            let labels = labels_for(count);
            for (index, label) in labels.iter().enumerate() {
                for other in labels.iter().skip(index + 1) {
                    assert!(
                        !other.starts_with(label.as_str()),
                        "with {count} labels, {label:?} is the start of {other:?}"
                    );
                    assert!(
                        !label.starts_with(other.as_str()),
                        "with {count} labels, {other:?} is the start of {label:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_label_is_different() {
        for count in [20, 21, 40, 200] {
            let labels = labels_for(count);
            let unique: std::collections::HashSet<&String> = labels.iter().collect();
            assert_eq!(unique.len(), labels.len(), "for {count}");
        }
    }

    /// Twenty-seven is the interesting number: one more than the alphabet, so
    /// exactly one letter has to stop being a label and become a prefix.
    #[test]
    fn one_letter_past_the_alphabet_gives_up_exactly_one_letter() {
        let labels = labels_for(27);
        let singles = labels.iter().filter(|label| label.len() == 1).count();
        assert_eq!(singles, 25, "{labels:?}");
        assert_eq!(labels.iter().filter(|label| label.len() == 2).count(), 2);
    }

    /// Labels stay short: a screen full of matches should not need three
    /// keystrokes each while two-letter labels are still available.
    #[test]
    fn labels_stay_as_short_as_they_can() {
        for (count, longest) in [(26, 1), (27, 2), (100, 2), (399, 2)] {
            let widest = labels_for(count)
                .iter()
                .map(|label| label.chars().count())
                .max()
                .unwrap_or(0);
            assert_eq!(widest, longest, "for {count}");
        }
    }

    /// And they only use the alphabet that was chosen for being unambiguous in
    /// a terminal font.
    #[test]
    fn labels_use_only_the_chosen_alphabet() {
        let allowed: std::collections::HashSet<char> =
            unterm_services::settings::DEFAULT_QUICK_SELECT_ALPHABET
                .chars()
                .collect();
        for label in labels_for(200) {
            for ch in label.chars() {
                assert!(allowed.contains(&ch), "{label:?} uses {ch:?}");
            }
        }
    }

    /// The alphabet is the user's to choose, and the labels follow it.
    #[test]
    fn a_configured_alphabet_is_the_one_the_labels_come_from() {
        assert_eq!(labels_from("jkl", 3), vec!["j", "k", "l"]);
        // Past the alphabet, the same no-prefix rule holds for any choice.
        let labels = labels_from("jkl", 5);
        assert_eq!(labels.len(), 5);
        for (index, label) in labels.iter().enumerate() {
            for other in labels.iter().skip(index + 1) {
                assert!(!other.starts_with(label.as_str()), "{labels:?}");
            }
        }
    }
}
