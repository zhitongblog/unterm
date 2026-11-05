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

// --- A4: private-marked finals without a private meaning do nothing -----

fn three_rows() -> NextCoreScreen {
    let mut screen = screen(10, 4);
    screen.feed("A\r\nB\r\nC");
    screen
}

#[test]
fn xtsmgraphics_queries_do_not_scroll() {
    let mut screen = three_rows();
    screen.feed("\x1b[?2;1;0S\x1b[?1;1;0S\x1b[?1;4S");
    assert_eq!(screen.snapshot_viewport_lines()[..3], ["A", "B", "C"]);
    assert_eq!(screen.scrollback_rows(), 0);
    assert_eq!((screen.cursor_x, screen.cursor_y), (1, 2));
}

#[test]
fn title_mode_reset_does_not_scroll_down() {
    let mut screen = three_rows();
    screen.feed("\x1b[>0T\x1b[>2T\x1b[>1t");
    assert_eq!(screen.snapshot_viewport_lines()[..3], ["A", "B", "C"]);
}

#[test]
fn other_marked_finals_run_no_ansi_action() {
    let mut screen = three_rows();
    // `?`/`>`/`=` forms of IL, DL, ICH, DCH, CUP, ED-less finals and REP.
    screen.feed("\x1b[?2L\x1b[>2M\x1b[=3@\x1b[?3P\x1b[>1;1H\x1b[?5b\x1b[=2X");
    assert_eq!(screen.snapshot_viewport_lines()[..3], ["A", "B", "C"]);
    assert_eq!((screen.cursor_x, screen.cursor_y), (1, 2));
    // The unmarked form still works.
    screen.feed("\x1b[S");
    assert_eq!(screen.snapshot_viewport_lines()[..2], ["B", "C"]);
}

#[test]
fn xtsmgraphics_is_answered_consistently_with_da1() {
    let screen = screen(10, 4);
    let ask = |csi: &str| {
        String::from_utf8(terminal_queries::response_for_csi_for_tests(csi, &screen).unwrap())
            .unwrap()
    };
    assert_eq!(ask("?1;1;0S"), "\x1b[?1;0;1024S");
    assert_eq!(ask("?1;4;0S"), "\x1b[?1;0;1024S");
    let (w, h) = terminal_queries::cell_pixels();
    assert_eq!(ask("?2;1;0S"), format!("\x1b[?2;0;{};{}S", 10 * w, 4 * h));
    assert_eq!(ask("?2;4;0S"), "\x1b[?2;0;4096;4096S");
    // ReGIS is not offered; a bad action is an error in Pa.
    assert_eq!(ask("?3;1;0S"), "\x1b[?3;1;0S");
    assert_eq!(ask("?1;9;0S"), "\x1b[?1;2;0S");
}

// --- A7: XTSAVE / XTRESTORE keep mode values, nothing else --------------

#[test]
fn xtrestore_does_not_home_the_cursor_or_reset_the_region() {
    let mut screen = screen(20, 10);
    screen.feed("abc\x1b[3;8r\x1b[5;5H\x1b[?1r X");
    assert_eq!((screen.scroll_top, screen.scroll_bottom), (2, 7));
    assert_eq!(row(&screen, 4), "     X");
    screen.feed("\x1b[?1049r");
    assert_eq!((screen.cursor_x, screen.cursor_y), (6, 4));
}

#[test]
fn xtsave_does_not_overwrite_the_decsc_slot() {
    let mut screen = screen(20, 10);
    screen.feed("\x1b[2;3H\x1b7\x1b[6;6H\x1b[?25s\x1b8");
    assert_eq!((screen.cursor_x, screen.cursor_y), (2, 1));
    screen.feed("\x1b[6;6H\x1b[?7s\x1b[u");
    assert_eq!((screen.cursor_x, screen.cursor_y), (2, 1));
}

#[test]
fn xtsave_and_xtrestore_round_trip_mode_values() {
    let mut screen = screen(20, 10);
    screen.feed("\x1b[?1h\x1b[?25l\x1b[?2004h\x1b[?1;25;2004;7s");
    screen.feed("\x1b[?1l\x1b[?25h\x1b[?2004l\x1b[?7l");
    assert!(!screen.application_cursor_keys && screen.cursor_visible && !screen.auto_wrap);
    screen.feed("\x1b[4;4H\x1b[?1;25;2004;7r");
    assert!(screen.application_cursor_keys);
    assert!(!screen.cursor_visible);
    assert!(screen.bracketed_paste);
    assert!(screen.auto_wrap);
    assert_eq!((screen.cursor_x, screen.cursor_y), (3, 3));
    // A mode never saved is left as it is.
    screen.feed("\x1b[?1004h\x1b[?1004r");
    assert!(screen.focus_event_reporting);
}

#[test]
fn xtrestore_of_origin_mode_does_not_home_the_cursor() {
    let mut screen = screen(20, 10);
    screen.feed("\x1b[?6s\x1b[?6h\x1b[5;5H\x1b[?6r");
    assert!(!screen.origin_mode);
    assert_eq!((screen.cursor_x, screen.cursor_y), (4, 4));
}

// --- A8: DECCOLM is ignored ---------------------------------------------

#[test]
fn deccolm_changes_neither_the_width_nor_the_history() {
    let mut screen = screen(120, 3);
    screen.feed("one\r\ntwo\r\nthree\r\nfour");
    assert_eq!(screen.scrollback_rows(), 1);
    screen.feed("\x1b[?3l\x1b[?3h");
    assert_eq!(screen.cols, 120);
    assert_eq!(screen.scrollback_rows(), 1);
    assert_eq!(screen.snapshot_viewport_lines(), vec!["two", "three", "four"]);
    assert_eq!((screen.cursor_x, screen.cursor_y), (4, 2));
}

// --- B5: rectangle ops split wide characters at their edges -------------

fn widths(screen: &NextCoreScreen, row: usize) -> Vec<u8> {
    screen.lines[row].iter().map(|cell| cell.width).collect()
}

#[test]
fn decfra_splits_wide_characters_at_both_edges() {
    let mut screen = screen(10, 2);
    screen.feed("中文\x1b[88;1;2;1;3$x");
    assert_eq!(row(&screen, 0), " XX");
    assert_eq!(widths(&screen, 0), vec![1, 1, 1, 1]);
}

#[test]
fn decera_splits_wide_characters_at_both_edges() {
    let mut screen = screen(10, 2);
    screen.feed("中文ab\x1b[1;2;1;3$z");
    assert_eq!(row(&screen, 0), "    ab");
    assert!(!widths(&screen, 0).contains(&0));
    assert!(!widths(&screen, 0).contains(&2));
}

#[test]
fn decsera_treats_a_wide_pair_as_protected_by_its_head() {
    let mut screen = screen(10, 2);
    screen.feed("\x1b[1\"q中\x1b[0\"q文b\x1b[1;2;1;3${");
    assert_eq!(row(&screen, 0), "中  b");
    assert_eq!(widths(&screen, 0), vec![2, 0, 1, 1, 1]);
}

// --- B6: synchronized output is terminal-wide ---------------------------

fn shown_text(screen: &NextCoreScreen) -> Vec<String> {
    let (lines, _) = screen.shown_viewport_lines(screen.viewport_first_row());
    lines
        .iter()
        .map(|l| l.cells.iter().map(|c| c.ch).collect::<String>().trim_end().to_string())
        .collect()
}

#[test]
fn entering_the_alternate_screen_keeps_the_synchronized_hold() {
    let mut screen = screen(10, 2);
    screen.feed("shell");
    screen.feed("\x1b[?2026h\x1b[?1049h\x1b[Hhalf");
    assert!(screen.synchronized_output);
    assert!(screen.held().is_some());
    assert_eq!(shown_text(&screen), vec!["shell", ""]);
    screen.feed("\x1b[?2026l");
    assert_eq!(shown_text(&screen), vec!["half", ""]);
}

#[test]
fn leaving_the_alternate_screen_does_not_resurrect_a_finished_update() {
    let mut screen = screen(10, 2);
    screen.feed("\x1b[?2026h\x1b[?1049h\x1b[?2026l\x1b[?1049l");
    assert!(!screen.synchronized_output);
    assert!(screen.synchronized_since.is_none());
    // A later update on the main screen takes a hold again.
    screen.feed("\x1b[?2026h");
    assert!(screen.held().is_some());
    screen.feed("\x1b[?2026l");
}

#[test]
fn soft_reset_ends_the_hold_with_the_flag() {
    let mut screen = screen(10, 2);
    screen.feed("\x1b[?2026h\x1b[!p");
    assert!(!screen.synchronized_output);
    assert!(screen.synchronized_since.is_none());
}

// --- B8: repeat counts are clamped --------------------------------------

fn assert_fast(started: std::time::Instant) {
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn huge_insert_and_delete_line_counts_return_at_once() {
    let mut screen = screen(10, 5);
    screen.feed("a\r\nb\r\nc\x1b[2;1H");
    let started = std::time::Instant::now();
    screen.feed("\x1b[999999999L");
    assert_eq!(screen.snapshot_viewport_lines(), vec!["a", "", "", "", ""]);
    screen.feed("\x1b[1;1H\x1b[999999999M");
    assert_fast(started);
    assert_eq!(screen.snapshot_viewport_lines(), vec!["", "", "", "", ""]);
}

#[test]
fn huge_repeat_count_returns_at_once() {
    let mut screen = screen(10, 3);
    let started = std::time::Instant::now();
    screen.feed("x\x1b[999999999b");
    assert_fast(started);
    assert!(screen.scrollback_rows() <= 3);
}

#[test]
fn huge_scroll_up_puts_at_most_one_screenful_into_history() {
    let mut screen = screen(10, 4);
    screen.feed("one\r\ntwo");
    let started = std::time::Instant::now();
    screen.feed("\x1b[99999999S");
    assert_fast(started);
    assert_eq!(screen.scrollback_rows(), 4);
    assert_eq!(screen.history_text_range(0, 2), vec!["one", "two"]);
    screen.feed("\x1b[99999999T\x1b[99999999I\x1b[99999999Z\x1b[99999999C\x1b[99999999B");
    assert_fast(started);
    assert_eq!((screen.cursor_x, screen.cursor_y), (9, 3));
}

// --- B9: IL/DL outside the margins do nothing ---------------------------

#[test]
fn insert_and_delete_lines_outside_the_scroll_region_do_nothing() {
    let mut screen = screen(10, 6);
    screen.feed("r0\r\nr1\r\nr2\r\nr3\r\nr4\r\nr5");
    screen.feed("\x1b[3;5r\x1b[2;1H\x1b[3L");
    assert_eq!(screen.snapshot_viewport_lines(), vec!["r0", "r1", "r2", "r3", "r4", "r5"]);
    screen.feed("\x1b[6;1H\x1b[2M");
    assert_eq!(screen.snapshot_viewport_lines(), vec!["r0", "r1", "r2", "r3", "r4", "r5"]);
    // Inside the region they still work, and stay inside it.
    screen.feed("\x1b[3;1H\x1b[L");
    assert_eq!(screen.snapshot_viewport_lines(), vec!["r0", "r1", "", "r2", "r3", "r5"]);
}

// --- C-BS: BS and CUB 1 agree in pending-wrap ---------------------------

#[test]
fn backspace_and_cub_from_pending_wrap_both_land_one_left_of_the_last_column() {
    let mut bs = screen_with_pending_wrap();
    bs.feed("\x08X");
    let mut cub = screen_with_pending_wrap();
    cub.feed("\x1b[DX");
    assert_eq!(row(&bs, 0), "ABCXE");
    assert_eq!(row(&cub, 0), "ABCXE");
    assert_eq!(bs.cursor_x, cub.cursor_x);
}

// --- C-DECLRMM: left/right margins --------------------------------------

/// 10x6 with DECLRMM on and margins at columns 3..=6 (1-based 4;7).
fn margined() -> NextCoreScreen {
    let mut screen = screen(10, 6);
    screen.feed("\x1b[?69h\x1b[4;7s");
    screen
}

/// `rows` rows of distinct letters, then the margins of `margined`.
fn margined_filled(rows: usize) -> NextCoreScreen {
    let mut screen = screen(10, 6);
    for r in 0..rows {
        let text: String = (0..10).map(|c| char::from(b'a' + ((r + c) % 26) as u8)).collect();
        screen.feed(&format!("\x1b[{};1H{}", r + 1, text));
    }
    screen.feed("\x1b[?69h\x1b[4;7s");
    screen
}

#[test]
fn cub_left_of_the_left_margin_goes_on_to_column_zero() {
    let mut screen = margined();
    screen.feed("\x1b[1;3H\x1b[9D");
    assert_eq!(screen.cursor_x, 0);
    screen.feed("\x1b[1;7H\x1b[9D");
    assert_eq!(screen.cursor_x, 3);
}

#[test]
fn carriage_return_goes_to_the_left_margin_or_column_zero() {
    let mut screen = margined();
    screen.feed("\x1b[1;6H\r");
    assert_eq!(screen.cursor_x, 3);
    screen.feed("\x1b[1;2H\r");
    assert_eq!(screen.cursor_x, 0);
    screen.feed("\x1b[2;9H\x1bE");
    assert_eq!((screen.cursor_x, screen.cursor_y), (3, 2));
}

#[test]
fn cup_reaches_columns_outside_the_margins_without_origin_mode() {
    let mut screen = screen(40, 3);
    screen.feed("\x1b[?69h\x1b[10;20s\x1b[1;30HX");
    assert_eq!(screen.lines[0][29].ch, 'X');
    screen.feed("\x1b[1;2HY");
    assert_eq!(screen.lines[0][1].ch, 'Y');
}

#[test]
fn origin_mode_keeps_cup_inside_the_margins() {
    let mut screen = margined();
    screen.feed("\x1b[?6h\x1b[1;9H");
    assert_eq!(screen.cursor_x, 6);
    screen.feed("\x1b[1;2H");
    assert_eq!(screen.cursor_x, 4);
}

#[test]
fn vpa_does_not_add_the_left_margin_twice() {
    let mut screen = margined();
    screen.feed("\x1b[?6h\x1b[1;2H\x1b[3d");
    assert_eq!((screen.cursor_x, screen.cursor_y), (4, 2));
}

#[test]
fn cha_is_origin_relative_like_hpa() {
    let mut screen = margined();
    screen.feed("\x1b[?6h\x1b[2G");
    assert_eq!(screen.cursor_x, 4);
    screen.feed("\x1b[1`");
    assert_eq!(screen.cursor_x, 3);
}

#[test]
fn insert_and_delete_lines_move_only_the_columns_between_the_margins() {
    let mut screen = margined_filled(3);
    screen.feed("\x1b[1;4H\x1b[L");
    assert_eq!(row(&screen, 0), "abc    hij");
    assert_eq!(row(&screen, 1), "bcddefgijk");
    assert_eq!(row(&screen, 2), "cdeefghjkl");
    screen.feed("\x1b[M");
    assert_eq!(row(&screen, 0), "abcdefghij");
    assert_eq!(row(&screen, 1), "bcdefghijk");
    // Outside the left/right margins IL does nothing.
    screen.feed("\x1b[1;2H\x1b[L");
    assert_eq!(row(&screen, 0), "abcdefghij");
}

#[test]
fn su_sd_and_index_scroll_only_between_the_margins() {
    let mut screen = margined_filled(6);
    let before = screen.snapshot_viewport_lines();
    screen.feed("\x1b[S");
    assert_eq!(row(&screen, 0), "abcefghhij");
    assert_eq!(row(&screen, 5), "fgh    mno");
    assert_eq!(screen.scrollback_rows(), 0);
    screen.feed("\x1b[T");
    assert_eq!(row(&screen, 0), "abc    hij");
    assert_eq!(row(&screen, 1), before[1]);
    // LF on the bottom row, inside the margins, scrolls the same way.
    screen.feed("\x1b[6;5H\n");
    assert_eq!(row(&screen, 4), "efgijkllmn");
    assert_eq!(screen.scrollback_rows(), 0);
}

#[test]
fn ich_dch_and_insert_mode_stop_at_the_right_margin() {
    let mut screen = margined_filled(1);
    screen.feed("\x1b[1;4H\x1b[2@");
    assert_eq!(row(&screen, 0), "abc  dehij");
    screen.feed("\x1b[2P");
    assert_eq!(row(&screen, 0), "abcde  hij");
    screen.feed("\x1b[4h\x1b[1;4HZ\x1b[4l");
    assert_eq!(row(&screen, 0), "abcZde hij");
    // Outside the margins ICH does nothing.
    screen.feed("\x1b[1;9H\x1b[@");
    assert_eq!(row(&screen, 0), "abcZde hij");
}

#[test]
fn tabs_stop_at_the_margins() {
    let mut screen = screen(30, 2);
    screen.feed("\x1b[?69h\x1b[3;12s\x1b[1;5H\t\t");
    assert_eq!(screen.cursor_x, 11);
    screen.feed("\x1b[Z\x1b[Z");
    assert_eq!(screen.cursor_x, 2);
    // From beyond the right margin a tab runs on to the screen edge.
    screen.feed("\x1b[1;14H\t\t\t");
    assert_eq!(screen.cursor_x, 29);
}

// --- C-vertical: CUU/CUD clamp to the region; CPR is origin-relative -----

#[test]
fn cuu_and_cud_stop_at_the_scroll_margins_without_origin_mode() {
    let mut screen = screen(10, 10);
    screen.feed("\x1b[3;7r\x1b[5;1H\x1b[9A");
    assert_eq!(screen.cursor_y, 2);
    screen.feed("\x1b[9B");
    assert_eq!(screen.cursor_y, 6);
    // Starting outside the region they run to the screen edge.
    screen.feed("\x1b[2;1H\x1b[9A");
    assert_eq!(screen.cursor_y, 0);
    screen.feed("\x1b[9;1H\x1b[9B");
    assert_eq!(screen.cursor_y, 9);
}

#[test]
fn cpr_counts_from_the_margins_under_origin_mode() {
    let mut screen = margined();
    screen.feed("\x1b[2;5r\x1b[?6h\x1b[2;3H");
    assert_eq!(
        terminal_queries::response_for_csi_for_tests("6n", &screen).unwrap(),
        b"\x1b[2;3R"
    );
    assert_eq!((screen.cursor_x, screen.cursor_y), (5, 2));
}

// --- C-wrapped: the soft-wrap flag belongs to the row ---------------------

/// "abcde" soft-wrapped into "fg" on a 5x3 screen.
fn wrapped_row() -> NextCoreScreen {
    let mut screen = screen(5, 3);
    screen.feed("abcdefg");
    assert_eq!(wrapped_cells(&screen, 0), vec![4]);
    screen
}

/// Columns of `row` carrying the soft-wrap flag.
fn wrapped_cells(screen: &NextCoreScreen, row: usize) -> Vec<usize> {
    screen.lines[row]
        .iter()
        .enumerate()
        .filter(|(_, cell)| cell.wrapped)
        .map(|(col, _)| col)
        .collect()
}

#[test]
fn overwriting_the_last_column_keeps_the_soft_wrap() {
    let mut screen = wrapped_row();
    screen.feed("\x1b[1;5HZ");
    assert_eq!(row(&screen, 0), "abcdZ");
    assert_eq!(wrapped_cells(&screen, 0), vec![4]);
    // With autowrap off too.
    screen.feed("\x1b[?7l\x1b[1;5HY");
    assert_eq!(wrapped_cells(&screen, 0), vec![4]);
}

#[test]
fn dch_and_ich_keep_the_flag_on_the_last_cell() {
    let mut screen = wrapped_row();
    screen.feed("\x1b[1;2H\x1b[2P");
    assert_eq!(row(&screen, 0).trim_end(), "ade");
    assert_eq!(wrapped_cells(&screen, 0), vec![4]);

    let mut screen = wrapped_row();
    screen.feed("\x1b[1;2H\x1b[2@");
    assert_eq!(row(&screen, 0), "a  bc");
    assert_eq!(wrapped_cells(&screen, 0), vec![4]);
}

#[test]
fn ech_el1_and_rectangle_edits_keep_the_soft_wrap() {
    for edit in [
        "\x1b[1;5H\x1b[X",
        "\x1b[1;5H\x1b[1K",
        "\x1b[1;1;1;5$z",
        "\x1b[88;1;1;1;5$x",
        "\x1b[1;1;1;5${",
        "\x1b[1;1H\x1b[2 @",
        "\x1b[1;1H\x1b[2 A",
    ] {
        let mut screen = wrapped_row();
        screen.feed(edit);
        assert_eq!(wrapped_cells(&screen, 0), vec![4], "after {edit:?}");
        // SL/SR slide every row of the region; the rest touch row 0 only.
        if !edit.contains(' ') {
            assert_eq!(row(&screen, 1).trim_end(), "fg", "after {edit:?}");
        }
        assert!(wrapped_cells(&screen, 1).is_empty(), "after {:?}", edit);
    }
}

#[test]
fn erasing_the_end_of_the_row_ends_the_soft_wrap() {
    for edit in ["\x1b[1;3H\x1b[K", "\x1b[1;3H\x1b[2K", "\x1b[2J", "\x1b[1;3H\x1b[J"] {
        let mut screen = wrapped_row();
        screen.feed(edit);
        assert!(wrapped_cells(&screen, 0).is_empty(), "after {:?}", edit);
    }
}
