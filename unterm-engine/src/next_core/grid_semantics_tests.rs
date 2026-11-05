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
