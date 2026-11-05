//! Resize, reflow and viewport regressions: height shrinks that keep the
//! cursor's line, re-wrapping on width changes, and dirty rows that land on
//! the viewport rows they describe.

use super::cell::ScreenCell;
use super::render_frame::{self, FrameSelection};
use super::NextCoreScreen;

fn row_text(row: &[ScreenCell]) -> String {
    NextCoreScreen::line_text(&row.to_vec())
}

fn live(screen: &NextCoreScreen, row: usize) -> String {
    screen.lines.get(row).map(|r| row_text(r)).unwrap_or_default()
}

/// Every row of history, scrollback then live lines.
fn all_rows(screen: &NextCoreScreen) -> Vec<String> {
    screen.history_text_range(0, screen.history_len())
}

/// The logical lines: rows joined across soft wraps.
fn logical_lines(screen: &NextCoreScreen) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for idx in 0..screen.history_len() {
        let row = screen.history_line(idx).expect("row in range");
        let wrapped = row.last().is_some_and(|cell| cell.wrapped);
        let mut text = String::new();
        for cell in row.iter().filter(|cell| cell.width > 0) {
            text.push(cell.ch);
            text.push_str(cell.combining());
        }
        if wrapped {
            current.push_str(&text);
        } else {
            current.push_str(text.trim_end());
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    // Rows below the last line of text are the screen's blank tail, not
    // lines anybody printed.
    while out.last().is_some_and(|line| line.is_empty()) {
        out.pop();
    }
    out
}

/// No wide character is split across rows or left without its tail.
fn assert_wide_chars_whole(screen: &NextCoreScreen) {
    for idx in 0..screen.history_len() {
        let row = screen.history_line(idx).expect("row in range");
        assert!(row.len() <= screen.cols, "row {} is wider than the screen", idx);
        for (col, cell) in row.iter().enumerate() {
            if cell.width == 2 {
                assert!(
                    row.get(col + 1).is_some_and(|tail| tail.width == 0),
                    "row {idx}: wide char {:?} at col {col} has no tail",
                    cell.ch
                );
            }
            if cell.width == 0 {
                assert!(
                    col > 0 && row[col - 1].width == 2,
                    "row {}: orphaned continuation at col {}", idx, col
                );
            }
        }
    }
}

fn char_at_cursor(screen: &NextCoreScreen) -> char {
    screen.lines[screen.cursor_y]
        .get(screen.cursor_x)
        .map(|cell| cell.ch)
        .unwrap_or(' ')
}

// ---------------------------------------------------------------- A5: height

#[test]
fn shrinking_keeps_a_prompt_drawn_at_the_top_after_a_clear() {
    let mut screen = NextCoreScreen::new(80, 24);
    for n in 1..=50 {
        screen.feed(&format!("{n}\r\n"));
    }
    screen.feed("\x1b[H\x1b[2Jprompt> ");
    assert_eq!((screen.cursor_x, screen.cursor_y), (8, 0));

    screen.resize(80, 10);
    assert_eq!(live(&screen, 0), "prompt>");
    assert_eq!((screen.cursor_x, screen.cursor_y), (8, 0));
    let cursor = screen.cursor_snapshot();
    assert_eq!((cursor.x, cursor.y), (8, 0));
    assert_eq!(screen.snapshot_viewport_lines()[0], "prompt>");
    assert_eq!(screen.lines.len(), 10);

    screen.feed("ls");
    assert_eq!(live(&screen, 0), "prompt> ls", "typing lands on the prompt line");
}

#[test]
fn shrinking_with_the_cursor_on_the_last_row_banks_the_top() {
    let mut screen = NextCoreScreen::new(80, 24);
    for n in 1..=50 {
        screen.feed(&format!("{n}\r\n"));
    }
    screen.feed("prompt> ");
    let banked = screen.history.scrollback_rows();
    screen.resize(80, 10);
    assert_eq!(screen.cursor_y, 9);
    assert_eq!(live(&screen, 9), "prompt>");
    assert_eq!(live(&screen, 8), "50");
    assert_eq!(screen.history.scrollback_rows(), banked + 14);
}

#[test]
fn shrinking_moves_only_as_many_rows_as_the_cursor_needs() {
    let mut screen = NextCoreScreen::new(40, 10);
    for n in 0..10 {
        screen.feed(&format!("r{n}"));
        if n < 9 {
            screen.feed("\r\n");
        }
    }
    // Cursor on row 5, rows 6..9 have text.
    screen.feed("\x1b[6;3H");
    let banked = screen.history.scrollback_rows();
    screen.resize(40, 4);
    assert_eq!(screen.history.scrollback_rows(), banked + 2);
    assert_eq!(screen.cursor_y, 3);
    assert_eq!(live(&screen, 3), "r5");
    assert_eq!(screen.lines.len(), 4);
}

#[test]
fn leaving_the_alternate_screen_after_a_shrink_keeps_the_prompt_on_screen() {
    let mut screen = NextCoreScreen::new(80, 24);
    for n in 1..=50 {
        screen.feed(&format!("{n}\r\n"));
    }
    screen.feed("\x1b[H\x1b[2Jprompt> ");
    screen.feed("\x1b[?1049h");
    screen.resize(80, 10);
    screen.feed("\x1b[?1049l");
    assert_eq!(live(&screen, 0), "prompt>");
    assert_eq!((screen.cursor_x, screen.cursor_y), (8, 0));
    assert_eq!(screen.lines.len(), 10);
}

#[test]
fn shrinking_adjusts_the_saved_cursor_with_the_rows_banked() {
    let mut screen = NextCoreScreen::new(20, 10);
    for n in 0..10 {
        screen.feed(&format!("r{n}"));
        if n < 9 {
            screen.feed("\r\n");
        }
    }
    // Save on r7, cursor stays on r9.
    screen.feed("\x1b[8;2H\x1b7\x1b[10;3H");
    screen.resize(20, 5);
    screen.feed("\x1b8");
    assert_eq!(live(&screen, screen.cursor_y), "r7");
    assert_eq!(screen.cursor_x, 1);
}

// ----------------------------------------------------------------- A6: width

#[test]
fn narrowing_moves_a_wide_char_whole_to_the_next_row() {
    let mut screen = NextCoreScreen::new(10, 5);
    screen.feed("abcd中");
    assert_eq!(screen.cursor_x, 6);

    screen.resize(5, 5);
    assert_wide_chars_whole(&screen);
    assert_eq!(live(&screen, 0), "abcd");
    assert_eq!(screen.lines[0].len(), 5, "the row ends on a spacer");
    assert!(screen.lines[0][4].wrapped, "and is still marked wrapped");
    assert_eq!(screen.lines[1][0].ch, '中');
    assert_eq!(screen.lines[1][0].width, 2);
    assert_eq!(screen.lines[1][1].width, 0);
    assert_eq!((screen.cursor_x, screen.cursor_y), (2, 1));

    screen.resize(10, 5);
    assert_wide_chars_whole(&screen);
    assert_eq!(live(&screen, 0), "abcd中", "the spacer goes away again");
    assert_eq!((screen.cursor_x, screen.cursor_y), (6, 0));
}

#[test]
fn a_long_line_round_trips_through_a_narrow_width() {
    let mut screen = NextCoreScreen::new(80, 24);
    let long: String = (0..200).map(|n| char::from(b'a' + (n % 26) as u8)).collect();
    screen.feed("$ echo\r\n");
    screen.feed(&long);
    screen.feed("\r\n$ ");
    let rows_before = screen.history_len();
    let text_before = all_rows(&screen);
    let lines_before = logical_lines(&screen);
    let cursor_before = (screen.cursor_x, screen.cursor_y);
    assert_eq!(lines_before[1], long);

    screen.resize(40, 24);
    assert_eq!(logical_lines(&screen), lines_before, "nothing lost at 40");
    assert_eq!(live(&screen, 1), &long[..40]);
    assert_eq!(live(&screen, 5), &long[160..]);
    assert_eq!(live(&screen, 6), "$");

    screen.resize(80, 24);
    assert_eq!(all_rows(&screen), text_before);
    assert_eq!(screen.history_len(), rows_before, "no extra rows");
    assert_eq!((screen.cursor_x, screen.cursor_y), cursor_before);
}

#[test]
fn widening_puts_a_wrapped_line_back_on_one_row() {
    let mut screen = NextCoreScreen::new(10, 5);
    screen.feed("0123456789abcdefghij");
    screen.feed("\r\nok");
    assert_eq!(live(&screen, 0), "0123456789");
    assert_eq!(live(&screen, 1), "abcdefghij");
    screen.resize(30, 5);
    assert_eq!(live(&screen, 0), "0123456789abcdefghij");
    assert_eq!(live(&screen, 1), "ok");
    assert_eq!((screen.cursor_x, screen.cursor_y), (2, 1));
}

#[test]
fn scrollback_lines_are_rewrapped_too() {
    let mut screen = NextCoreScreen::new(80, 5);
    let lines: Vec<String> = (0..20)
        .map(|n| format!("L{n:02}{}", "x".repeat(57)))
        .collect();
    for line in &lines {
        screen.feed(line);
        screen.feed("\r\n");
    }
    screen.feed("$ ");
    assert!(screen.history.scrollback_rows() > 10);
    let before = logical_lines(&screen);

    screen.resize(30, 5);
    assert_eq!(logical_lines(&screen), before);
    let first = row_text(&screen.history.scrollback()[0]);
    assert_eq!(first, format!("L00{}", "x".repeat(27)));
    assert!(screen.history.scrollback()[0].last().unwrap().wrapped);

    screen.resize(80, 5);
    assert_eq!(logical_lines(&screen), before);
    assert_eq!(row_text(&screen.history.scrollback()[0]), lines[0]);
}

#[test]
fn the_cursor_stays_on_the_same_character() {
    let mut screen = NextCoreScreen::new(80, 10);
    screen.feed("$ the quick brown fox jumps over the lazy dog");
    screen.feed("\x1b[12D");
    let target = char_at_cursor(&screen);
    assert_eq!(target, 't');
    for cols in [7, 13, 25, 80, 40, 3, 40] {
        screen.resize(cols, 10);
        assert_eq!(char_at_cursor(&screen), target, "at {cols} columns");
    }
}

#[test]
fn trailing_blanks_of_a_line_are_not_rewrapped_into_rows() {
    let mut screen = NextCoreScreen::new(40, 10);
    screen.feed(&format!("ab{}", " ".repeat(30)));
    screen.feed("\r\n$ ");
    screen.resize(10, 10);
    assert_eq!(live(&screen, 0), "ab");
    assert_eq!(live(&screen, 1), "$");
    assert_eq!(screen.cursor_y, 1);
}

#[test]
fn a_pending_wrap_survives_a_reflow() {
    let mut screen = NextCoreScreen::new(10, 5);
    screen.feed("0123456789");
    assert_eq!(screen.cursor_x, 10, "pending wrap");
    screen.resize(5, 5);
    assert_eq!((screen.cursor_x, screen.cursor_y), (5, 1));
    assert_eq!(live(&screen, 1), "56789");
    screen.feed("A");
    assert_eq!(live(&screen, 2), "A", "the next character wraps, not overwrites");
    screen.resize(20, 5);
    assert_eq!(live(&screen, 0), "0123456789A");
    assert_eq!((screen.cursor_x, screen.cursor_y), (11, 0));
}

#[test]
fn a_cursor_past_the_text_gets_a_row_of_its_own_line() {
    let mut screen = NextCoreScreen::new(20, 5);
    screen.feed("abcdefgh\x1b[13G");
    assert_eq!(screen.cursor_x, 12);
    screen.resize(5, 5);
    // 12 = row 2, col 2 of the same logical line.
    assert_eq!((screen.cursor_x, screen.cursor_y), (2, 2));
    screen.feed("Z");
    screen.resize(20, 5);
    assert_eq!(live(&screen, 0), "abcdefgh    Z");
}

#[test]
fn the_saved_cursor_follows_its_character() {
    let mut screen = NextCoreScreen::new(20, 5);
    screen.feed("abcdefghij\x1b[8G\x1b7\x1b[15G");
    screen.resize(5, 5);
    screen.feed("\x1b8");
    assert_eq!(char_at_cursor(&screen), 'h');
}

#[test]
fn skipping_evicted_scrollback_gives_the_same_rows_as_rewrapping_it() {
    let feed = |screen: &mut NextCoreScreen| {
        for n in 0..60 {
            screen.feed(&format!("{n:02} {}\r\n", "k".repeat(30 + n % 7)));
        }
        screen.feed("\x1b]133;A\x07$ ");
    };
    let mut limited = NextCoreScreen::new_with_scrollback_limit(40, 5, 20);
    let mut unlimited = NextCoreScreen::new_with_scrollback_limit(40, 5, 10_000);
    feed(&mut limited);
    feed(&mut unlimited);
    limited.resize(9, 5);
    unlimited.resize(9, 5);
    assert_eq!(limited.history.scrollback_rows(), 20);
    let kept = all_rows(&limited);
    let full = all_rows(&unlimited);
    assert_eq!(kept[..], full[full.len() - kept.len()..]);
    assert_eq!(
        (limited.cursor_x, limited.cursor_y),
        (unlimited.cursor_x, unlimited.cursor_y)
    );
    let prompt = *limited.prompt_rows.last().unwrap();
    assert!(row_text(limited.history_line(prompt).unwrap()).starts_with('$'));
}

#[test]
fn the_scrollback_limit_holds_after_narrowing() {
    let mut screen = NextCoreScreen::new_with_scrollback_limit(40, 5, 20);
    for n in 0..40 {
        screen.feed(&format!("{n:02}{}\r\n", "y".repeat(38)));
    }
    screen.resize(10, 5);
    assert!(screen.history.scrollback_rows() <= 20);
    assert_wide_chars_whole(&screen);
}

#[test]
fn prompt_marks_follow_their_rows() {
    let mut screen = NextCoreScreen::new(20, 5);
    for n in 0..6 {
        screen.feed(&format!("\x1b]133;A\x07$ cmd{n} {}\r\nout{n}\r\n", "y".repeat(30)));
    }
    screen.feed("\x1b]133;A\x07$ ");
    assert!(screen.prompt_rows.len() >= 6);
    for cols in [10, 50, 7] {
        screen.resize(cols, 5);
        for row in &screen.prompt_rows {
            let text = row_text(screen.history_line(*row).expect("prompt row exists"));
            assert!(text.starts_with('$'), "prompt row {} at {} cols reads {:?}", row, cols, text);
        }
    }
}

#[test]
fn a_scrolled_back_viewport_keeps_its_text_in_view() {
    let mut screen = NextCoreScreen::new(20, 5);
    for n in 0..30 {
        screen.feed(&format!("line {n:02} {}\r\n", "z".repeat(15)));
    }
    screen.feed("$ ");
    // Each line is two rows at 20 columns; scroll so a line starts on top.
    let top = screen.viewport_start();
    let target = (0..top)
        .rev()
        .find(|row| row_text(screen.history_line(*row).unwrap()).starts_with("line 10"))
        .unwrap();
    screen.scroll_viewport_by(target as isize - top as isize);
    assert!(screen.snapshot_viewport_lines()[0].starts_with("line 10"));

    screen.resize(40, 5);
    assert!(screen.history.viewport_is_pinned());
    assert!(
        screen.snapshot_viewport_lines()[0].starts_with("line 10"),
        "got {:?}",
        screen.snapshot_viewport_lines()
    );
    screen.resize(12, 5);
    assert!(screen.snapshot_viewport_lines()[0].starts_with("line 10"));
}

#[test]
fn the_alternate_screen_is_cut_not_rewrapped_but_wide_safe() {
    let mut screen = NextCoreScreen::new(10, 5);
    screen.feed("0123456789");
    screen.feed("\r\n$ ");
    screen.feed("\x1b[?1049h");
    screen.feed("abcd中xx");
    screen.resize(5, 5);
    assert_eq!(screen.lines.len(), 1, "no new rows on the alternate screen");
    assert_eq!(live(&screen, 0), "abcd");
    assert!(screen.lines[0].iter().all(|cell| cell.width == 1));
    assert_wide_chars_whole(&screen);

    screen.feed("\x1b[?1049l");
    assert_eq!(live(&screen, 0), "01234");
    assert_eq!(live(&screen, 1), "56789");
    assert_eq!(live(&screen, 2), "$");
    assert_eq!((screen.cursor_x, screen.cursor_y), (2, 2));
}

#[test]
fn hyperlinks_travel_with_their_cells() {
    let mut screen = NextCoreScreen::new(20, 5);
    screen.feed("see \x1b]8;;https://example.com\x07example-link-text\x1b]8;;\x07!");
    screen.resize(8, 5);
    let linked: String = (0..screen.history_len())
        .flat_map(|row| screen.history_line(row).unwrap().clone())
        .filter(|cell| cell.attr.hyperlink.is_some())
        .map(|cell| cell.ch)
        .collect();
    assert_eq!(linked, "example-link-text");
}

#[test]
fn reflowing_moves_pictures_with_their_rows() {
    // A 1x1 sixel at the start of a row, after some wrapped text.
    let mut screen = NextCoreScreen::new(20, 10);
    screen.feed(&"w".repeat(50));
    screen.feed("\r\nimg:");
    let before_row = screen.history.scrollback_rows() + screen.cursor_y;
    screen.feed("\x1bPq#0;2;100;0;0#0~\x1b\\");
    let placed = screen.all_images();
    if placed.is_empty() {
        // Sixel needs cell metrics in some builds; nothing to check then.
        return;
    }
    assert_eq!(placed[0].row, before_row as i64);
    screen.resize(50, 10);
    let row = screen.all_images()[0].row as usize;
    let text = row_text(screen.history_line(row).expect("picture row exists"));
    assert!(text.starts_with("img:"), "picture now on {}: {:?}", row, text);
}

// ---------------------------------------------------------- C: dirty rows

/// What a `read_render_frame` reader does, against one screen.
struct FrameReader {
    picture: Vec<String>,
    revision: Option<u64>,
}

impl FrameReader {
    fn new() -> Self {
        Self {
            picture: Vec::new(),
            revision: None,
        }
    }

    /// Mirrors `screen_dispatch::read_render_frame` for an unheld screen.
    fn read(&mut self, screen: &mut NextCoreScreen) {
        let first_row = screen.viewport_first_row();
        let revision = screen.revision();
        let selection = render_frame::select_frame(
            screen.rows,
            revision,
            screen.dirty_rows(),
            screen.history.viewport_is_pinned(),
            self.revision,
            |since| screen.can_render_delta_since(since),
        );
        if let FrameSelection::Changed {
            dirty_rows,
            full,
            clear_dirty,
        } = selection
        {
            let lines = match dirty_rows {
                Some(_) if full => screen.shown_viewport_lines(first_row).0,
                Some(rows) => screen.styled_viewport_dirty_lines(rows, first_row),
                None => Vec::new(),
            };
            if full {
                self.picture = vec![String::new(); screen.rows];
            }
            self.picture.resize(screen.rows, String::new());
            for line in lines {
                let idx = (line.row - first_row) as usize;
                let text: String = line
                    .cells
                    .iter()
                    .filter(|cell| cell.width > 0)
                    .map(|cell| cell.ch)
                    .collect();
                self.picture[idx] = text.trim_end().to_string();
            }
            if clear_dirty {
                screen.clear_dirty_rows();
            }
        }
        self.revision = Some(revision);
    }
}

#[test]
fn delta_frames_rebuild_the_screen_after_the_pane_grew() {
    let mut screen = NextCoreScreen::new(20, 6);
    for n in 0..12 {
        screen.feed(&format!("line {n}\r\n"));
    }
    screen.feed("$ ");
    let mut reader = FrameReader::new();
    reader.read(&mut screen);
    assert_eq!(reader.picture, screen.snapshot_viewport_lines());

    screen.resize(20, 10);
    reader.read(&mut screen);
    assert_eq!(reader.picture, screen.snapshot_viewport_lines());

    // Output after the grow: typing on the prompt row (live row 5, drawn on
    // viewport row 9), then new lines that grow the live area and shift
    // every row of the viewport.
    for chunk in ["ls", "\r\nout a", "\r\nout b\r\n$ ", "x"] {
        screen.feed(chunk);
        reader.read(&mut screen);
        assert_eq!(
            reader.picture,
            screen.snapshot_viewport_lines(),
            "after {chunk:?}"
        );
    }
}

#[test]
fn delta_rows_are_viewport_rows() {
    let mut screen = NextCoreScreen::new(20, 6);
    for n in 0..12 {
        screen.feed(&format!("line {n}\r\n"));
    }
    screen.feed("$ ");
    screen.resize(20, 10);
    screen.clear_dirty_rows();
    screen.bump_revision();
    screen.feed("x");
    // The prompt is live row 5 but is drawn on viewport row 9.
    assert_eq!(screen.cursor_y, 5);
    assert_eq!(screen.cursor_snapshot().y, 9);
    let dirty = screen.dirty_rows().expect("typing marks a row");
    assert_eq!((dirty.start, dirty.end), (9, 9));
}

#[test]
fn delta_frames_rebuild_the_screen_across_a_reflow() {
    let mut screen = NextCoreScreen::new(30, 6);
    for n in 0..12 {
        screen.feed(&format!("line {n} {}\r\n", "q".repeat(n * 3)));
    }
    screen.feed("$ ");
    let mut reader = FrameReader::new();
    reader.read(&mut screen);
    screen.resize(60, 6);
    reader.read(&mut screen);
    assert_eq!(reader.picture, screen.snapshot_viewport_lines());
    for chunk in ["a", "\r\nb", "\r\n"] {
        screen.feed(chunk);
        reader.read(&mut screen);
        assert_eq!(reader.picture, screen.snapshot_viewport_lines(), "after {chunk:?}");
    }
}

// ------------------------------------------- grow, then an absolute repaint

/// What ConPTY does after a resize: it has no scrollback of its own, so a
/// grow leaves its text at the top of a taller buffer, and it repaints that
/// buffer with absolute cursor moves.
fn conpty_repaint(screen: &mut NextCoreScreen, rows: &[&str], total_rows: usize, cursor: (usize, usize)) {
    let mut out = String::from("\x1b[?25l");
    for row in 0..total_rows {
        out.push_str(&format!("\x1b[{};1H", row + 1));
        out.push_str(rows.get(row).copied().unwrap_or(""));
        out.push_str("\x1b[K");
    }
    out.push_str(&format!("\x1b[{};{}H\x1b[?25h", cursor.1 + 1, cursor.0 + 1));
    screen.feed(&out);
}

#[test]
fn growing_then_an_absolute_repaint_does_not_duplicate_lines() {
    let mut screen = NextCoreScreen::new(20, 6);
    for n in 0..10 {
        screen.feed(&format!("line {n}\r\n"));
    }
    screen.feed("$ ");
    // Live: line 5..line 9, "$ ".
    screen.resize(20, 9);
    let pulled = screen.snapshot_viewport_lines();
    assert_eq!(pulled[0], "line 2", "growing shows scrollback above");
    assert_eq!(pulled[8], "$");

    let conpty = ["line 5", "line 6", "line 7", "line 8", "line 9", "$"];
    conpty_repaint(&mut screen, &conpty, 9, (2, 5));
    let shown = screen.snapshot_viewport_lines();
    let mut seen = std::collections::HashSet::new();
    for line in shown.iter().filter(|line| !line.is_empty()) {
        assert!(seen.insert(line.clone()), "{:?} is on screen twice: {:?}", line, shown);
    }
    assert_eq!(
        &shown[..6],
        &conpty[..],
        "the repaint's rows are where it put them"
    );
    assert_eq!(screen.cursor_snapshot().y, 5);
    // And scrollback still holds every line once.
    let history = all_rows(&screen);
    for n in 0..10 {
        let needle = format!("line {n}");
        assert_eq!(
            history.iter().filter(|row| **row == needle).count(),
            1,
            "{needle} in {history:?}"
        );
    }
}

#[test]
fn growing_then_a_partial_repaint_keeps_history_above_without_duplicates() {
    let mut screen = NextCoreScreen::new(20, 6);
    for n in 0..10 {
        screen.feed(&format!("line {n}\r\n"));
    }
    screen.feed("$ ");
    screen.resize(20, 9);
    // Only the rows the program knows about, then erase-below.
    let conpty = ["line 5", "line 6", "line 7", "line 8", "line 9", "$"];
    let mut out = String::from("\x1b[H");
    for (row, text) in conpty.iter().enumerate() {
        out.push_str(&format!("\x1b[{};1H{text}\x1b[K", row + 1));
    }
    out.push_str("\x1b[6;3H\x1b[J");
    screen.feed(&out);
    let shown = screen.snapshot_viewport_lines();
    let mut seen = std::collections::HashSet::new();
    for line in shown.iter().filter(|line| !line.is_empty()) {
        assert!(seen.insert(line.clone()), "{:?} is on screen twice: {:?}", line, shown);
    }
    let cursor = screen.cursor_snapshot();
    assert_eq!(shown[cursor.y as usize], "$");
}

// --------------------------------------------------------------- performance

/// A reflow of a full scrollback has to be quick: it runs under the screen
/// lock on every width change. Release only; run with
/// `cargo test --release -p unterm-engine reflow_of_a_full_scrollback -- --ignored --nocapture`.
#[test]
#[ignore]
fn reflow_of_a_full_scrollback_is_fast() {
    let mut screen = NextCoreScreen::new_with_scrollback_limit(200, 50, 10_000);
    let row: String = (0..200).map(|n| char::from(b'a' + (n % 26) as u8)).collect();
    let mut chunk = String::new();
    for _ in 0..10_050 {
        chunk.push_str(&row);
        chunk.push_str("\r\n");
    }
    screen.feed(&chunk);
    assert_eq!(screen.history.scrollback_rows(), 10_000);
    let mut worst = std::time::Duration::ZERO;
    for cols in [120, 200, 80, 200] {
        let started = std::time::Instant::now();
        screen.resize(cols, 50);
        let took = started.elapsed();
        eprintln!("reflow to {cols} cols: {took:?}");
        worst = worst.max(took);
    }
    if !cfg!(debug_assertions) {
        assert!(worst < std::time::Duration::from_millis(50), "worst {:?}", worst);
    }
}
