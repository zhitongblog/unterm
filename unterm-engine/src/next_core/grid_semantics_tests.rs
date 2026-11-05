//! Grid semantics checked against xterm: what moves where, and what a
//! sequence must leave alone.

use super::*;

fn screen(cols: usize, rows: usize) -> NextCoreScreen {
    NextCoreScreen::new(cols, rows)
}

fn row(screen: &NextCoreScreen, index: usize) -> String {
    screen.snapshot_viewport_lines()[index].clone()
}

// --- A1: LF/VT/FF keep the column; LNM adds the carriage return ---------

#[test]
fn bare_line_feed_keeps_the_cursor_column() {
    let mut screen = screen(20, 4);
    screen.feed("abc\ndef");
    assert_eq!(row(&screen, 0), "abc");
    assert_eq!(row(&screen, 1), "   def");
    assert_eq!((screen.cursor_x, screen.cursor_y), (6, 1));
}

#[test]
fn vertical_tab_and_form_feed_keep_the_cursor_column() {
    let mut screen = screen(20, 4);
    screen.feed("ab\x0bcd\x0cef");
    assert_eq!(row(&screen, 1), "  cd");
    assert_eq!(row(&screen, 2), "    ef");
}

#[test]
fn line_feed_keeps_the_column_without_synchronized_output() {
    let mut screen = screen(20, 5);
    screen.feed("\x1b[3;1H* \n\n\x1b[6C");
    assert_eq!(screen.cursor_x, 8);
    assert_eq!(screen.cursor_y, 4);
}

#[test]
fn newline_mode_makes_line_feed_return_the_carriage() {
    let mut screen = screen(20, 4);
    screen.feed("\x1b[20habc\ndef");
    assert_eq!(row(&screen, 1), "def");
    assert_eq!(
        terminal_queries::response_for_csi_for_tests("20$p", &screen).unwrap(),
        b"\x1b[20;1$y"
    );
    screen.feed("\x1b[20lgh\nij");
    assert_eq!(row(&screen, 2), "     ij");
    assert_eq!(
        terminal_queries::response_for_csi_for_tests("20$p", &screen).unwrap(),
        b"\x1b[20;2$y"
    );
}

#[test]
fn line_feed_from_pending_wrap_stays_in_the_last_column() {
    let mut screen = screen(5, 3);
    screen.feed("ABCDE\nX");
    assert_eq!(row(&screen, 0), "ABCDE");
    assert_eq!(row(&screen, 1), "    X");
}

// --- A2: grapheme joining only looks at the cell before the cursor ------

fn width_of(c: char) -> usize {
    usize::from(cell::ScreenCell::char_width(c))
}

fn cell_at(screen: &NextCoreScreen, row: usize, col: usize) -> &cell::ScreenCell {
    &screen.lines[row][col]
}

#[test]
fn a_regional_indicator_does_not_pair_across_other_cells() {
    let mut screen = screen(20, 2);
    screen.feed("🇺 A 🇸");
    let ri = width_of('🇺');
    let s_col = ri + 3;
    assert_eq!(cell_at(&screen, 0, 0).ch, '🇺');
    assert_eq!(cell_at(&screen, 0, 0).combining(), "");
    assert_eq!(cell_at(&screen, 0, s_col).ch, '🇸');
    assert_eq!(screen.cursor_x, s_col + ri);
}

#[test]
fn an_emoji_modifier_does_not_reach_back_past_other_cells() {
    let mut screen = screen(20, 2);
    screen.feed("👍 x🏽");
    assert_eq!(cell_at(&screen, 0, 0).combining(), "");
    assert_eq!(cell_at(&screen, 0, 4).ch, '🏽');
    assert_eq!(screen.cursor_x, 4 + width_of('🏽'));
}

#[test]
fn an_emoji_modifier_joins_the_emoji_right_before_it() {
    let mut screen = screen(20, 2);
    screen.feed("👍🏽!");
    assert_eq!(cell_at(&screen, 0, 0).combining(), "🏽");
    assert_eq!(cell_at(&screen, 0, 2).ch, '!');
    assert_eq!(screen.cursor_x, 3);
}

#[test]
fn a_joiner_only_joins_a_following_pictograph() {
    let mut screen = screen(20, 2);
    screen.feed("a\u{200d}b");
    assert_eq!(cell_at(&screen, 0, 0).combining(), "\u{200d}");
    assert_eq!(cell_at(&screen, 0, 1).ch, 'b');
    assert_eq!(screen.cursor_x, 2);
}

#[test]
fn a_zwj_family_stays_one_wide_cell() {
    let mut screen = screen(20, 2);
    screen.feed("👨\u{200d}👩x");
    assert_eq!(cell_at(&screen, 0, 0).ch, '👨');
    assert_eq!(cell_at(&screen, 0, 0).combining(), "\u{200d}👩");
    assert_eq!(cell_at(&screen, 0, 0).width, 2);
    assert_eq!(cell_at(&screen, 0, 2).ch, 'x');
    assert_eq!(screen.cursor_x, 3);
}

#[test]
fn two_flags_in_a_row_are_two_cells() {
    let mut screen = screen(20, 2);
    screen.feed("🇺🇸🇩🇪");
    let ri = width_of('🇺');
    assert_eq!(cell_at(&screen, 0, 0).ch, '🇺');
    assert_eq!(cell_at(&screen, 0, 0).combining(), "🇸");
    assert_eq!(cell_at(&screen, 0, ri).ch, '🇩');
    assert_eq!(cell_at(&screen, 0, ri).combining(), "🇪");
    assert_eq!(screen.cursor_x, 2 * ri);
}

#[test]
fn a_combining_mark_stays_on_its_base() {
    let mut screen = screen(20, 2);
    screen.feed("e\u{301}x");
    assert_eq!(cell_at(&screen, 0, 0).combining(), "\u{301}");
    assert_eq!(cell_at(&screen, 0, 1).ch, 'x');
    assert_eq!(screen.cursor_x, 2);
}

#[test]
fn a_combining_mark_attaches_to_the_cell_before_the_cursor_only() {
    let mut screen = screen(20, 2);
    // After CUF past the text there is no cell before the cursor; the mark
    // must not travel left onto `a`.
    screen.feed("a\x1b[3C\u{301}");
    assert_eq!(cell_at(&screen, 0, 0).combining(), "");
    // In pending wrap the last column is the one before the cursor.
    let mut screen = screen_with_pending_wrap();
    screen.feed("\u{301}");
    assert_eq!(cell_at(&screen, 0, 4).combining(), "\u{301}");
}

fn screen_with_pending_wrap() -> NextCoreScreen {
    let mut screen = screen(5, 2);
    screen.feed("ABCDE");
    screen
}
