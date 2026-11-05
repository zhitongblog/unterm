use super::{
    cell::ScreenCell, csi_params, parser_state::ParserState, MouseTrackingMode, NextCoreScreen,
};

/// The longest DCS or APC kept: a large sixel picture, with room to spare.
/// Anything longer is dropped, as every DCS and APC used to be.
const MAX_CONTROL_STRING_CHARS: usize = 64 * 1024 * 1024;

#[derive(Default)]
pub(super) struct TerminalParser {
    state: ParserState,
}

impl TerminalParser {
    pub(super) fn feed(&mut self, screen: &mut NextCoreScreen, chunk: &str) {
        for c in chunk.chars() {
            self.feed_char(screen, c);
        }
    }

    fn feed_char(&mut self, screen: &mut NextCoreScreen, c: char) {
        match self.state {
            ParserState::Ground => match c {
                '\x1b' => self.state = ParserState::Escape,
                '\u{0084}' => screen.index(),
                '\u{0085}' => screen.next_line(),
                '\u{008d}' => screen.reverse_index(),
                '\u{0090}' => self.state = ParserState::Dcs(String::new()),
                '\u{009f}' => self.state = ParserState::Apc(String::new()),
                '\u{0098}' | '\u{009e}' => {
                    self.state = ParserState::IgnoredString;
                }
                '\u{009b}' => self.state = ParserState::Csi(String::new()),
                '\u{009d}' => self.state = ParserState::Osc(String::new()),
                '\r' => screen.carriage_return(),
                '\n' | '\x0b' | '\x0c' => screen.line_feed(),
                '\x07' => screen.ring_bell(),
                '\x08' => screen.backspace(),
                '\t' => screen.horizontal_tab(),
                c if !c.is_control() => screen.put_char(c),
                _ => {}
            },
            ParserState::Escape => match c {
                '[' => self.state = ParserState::Csi(String::new()),
                ']' => self.state = ParserState::Osc(String::new()),
                '=' => {
                    screen.application_keypad = true;
                    self.state = ParserState::Ground;
                }
                '>' => {
                    screen.application_keypad = false;
                    self.state = ParserState::Ground;
                }
                '(' | ')' | '*' | '+' | '-' | '.' | '/' | '%' => {
                    self.state = ParserState::EscapeIgnoreOne;
                }
                '#' => {
                    self.state = ParserState::EscapeHash;
                }
                'P' => self.state = ParserState::Dcs(String::new()),
                '_' => self.state = ParserState::Apc(String::new()),
                'X' | '^' => {
                    self.state = ParserState::IgnoredString;
                }
                '7' => {
                    screen.save_cursor();
                    self.state = ParserState::Ground;
                }
                '8' => {
                    screen.restore_cursor();
                    self.state = ParserState::Ground;
                }
                'D' => {
                    screen.index();
                    self.state = ParserState::Ground;
                }
                'E' => {
                    screen.next_line();
                    self.state = ParserState::Ground;
                }
                'H' => {
                    screen.set_tab_stop();
                    self.state = ParserState::Ground;
                }
                'M' => {
                    screen.reverse_index();
                    self.state = ParserState::Ground;
                }
                'c' => {
                    screen.reset_terminal();
                    self.state = ParserState::Ground;
                }
                _ => self.state = ParserState::Ground,
            },
            ParserState::EscapeIgnoreOne => {
                self.state = ParserState::Ground;
            }
            ParserState::EscapeHash => {
                if c == '8' {
                    screen.fill_alignment_test();
                }
                self.state = ParserState::Ground;
            }
            ParserState::IgnoredString => match c {
                '\x07' | '\u{009c}' => self.state = ParserState::Ground,
                '\x1b' => self.state = ParserState::IgnoredStringEscape,
                _ => {}
            },
            ParserState::IgnoredStringEscape => {
                if c == '\\' {
                    self.state = ParserState::Ground;
                } else {
                    self.state = ParserState::IgnoredString;
                }
            }
            ParserState::Dcs(ref mut body) | ParserState::Apc(ref mut body) => match c {
                '\x07' | '\u{009c}' => {
                    let body = std::mem::take(body);
                    let apc = matches!(self.state, ParserState::Apc(_));
                    self.state = ParserState::Ground;
                    Self::finish_control_string(screen, apc, &body);
                }
                '\x1b' => {
                    let body = std::mem::take(body);
                    self.state = if matches!(self.state, ParserState::Apc(_)) {
                        ParserState::ApcEscape(body)
                    } else {
                        ParserState::DcsEscape(body)
                    };
                }
                _ => {
                    if body.len() >= MAX_CONTROL_STRING_CHARS {
                        self.state = ParserState::IgnoredString;
                    } else {
                        body.push(c);
                    }
                }
            },
            ParserState::DcsEscape(ref mut body) | ParserState::ApcEscape(ref mut body) => {
                let body = std::mem::take(body);
                let apc = matches!(self.state, ParserState::ApcEscape(_));
                self.state = ParserState::Ground;
                if c == '\\' {
                    Self::finish_control_string(screen, apc, &body);
                } else {
                    // An ESC that does not end the string cancels it and
                    // starts a sequence of its own.
                    self.state = ParserState::Escape;
                    self.feed_char(screen, c);
                }
            }
            ParserState::Csi(ref mut sequence) => {
                if ('@'..='~').contains(&c) {
                    sequence.push(c);
                    let sequence = std::mem::take(sequence);
                    Self::handle_csi(screen, &sequence);
                    self.state = ParserState::Ground;
                } else {
                    sequence.push(c);
                }
            }
            ParserState::Osc(ref mut sequence) => match c {
                '\x07' | '\u{009c}' => {
                    let sequence = std::mem::take(sequence);
                    screen.apply_osc(&sequence);
                    self.state = ParserState::Ground;
                }
                '\x1b' => {
                    let sequence = std::mem::take(sequence);
                    self.state = ParserState::OscEscape(sequence);
                }
                _ => sequence.push(c),
            },
            ParserState::OscEscape(ref mut sequence) => {
                if c == '\\' {
                    let sequence = std::mem::take(sequence);
                    screen.apply_osc(&sequence);
                }
                self.state = ParserState::Ground;
            }
        }
    }

    fn finish_control_string(screen: &mut NextCoreScreen, apc: bool, body: &str) {
        if apc {
            screen.apply_apc(body);
        } else {
            screen.apply_dcs(body);
        }
    }

    fn handle_csi(screen: &mut NextCoreScreen, sequence: &str) {
        let Some(final_byte) = sequence.chars().last() else {
            return;
        };
        let raw_params = &sequence[..sequence.len().saturating_sub(final_byte.len_utf8())];
        let private = raw_params.starts_with('?');
        let numeric_params = raw_params.trim_start_matches('?');
        let numbers = numeric_params
            .split(';')
            .map(|part| part.trim().parse::<usize>().unwrap_or(0))
            .collect::<Vec<_>>();
        let first = || numbers.first().copied().filter(|n| *n > 0).unwrap_or(1);

        // A private marker (`?`, `>`, `<`, `=`) makes a different sequence
        // out of the same final byte. Only the finals below give a marked
        // form a meaning on the screen; every other marked sequence is a
        // query answered elsewhere (`CSI ? Pi;Pa;Pv S` is XTSMGRAPHICS,
        // answered in terminal_queries) or a setting this terminal does not
        // keep (`CSI > Ps T` resets title modes). Run as the unmarked ANSI
        // action they scrolled the screen: chafa, yazi and notcurses probe
        // sixel geometry with `CSI ? 2;1;0 S`, which read as `SU 2`.
        if let Some(marker) = raw_params
            .chars()
            .next()
            .filter(|c| matches!(c, '?' | '>' | '<' | '='))
        {
            let meaningful = matches!(
                (marker, final_byte),
                ('?', 'h' | 'l' | 'J' | 'K' | 'W' | 'r' | 's' | 'm') | ('>', 'm') | (_, 'u')
            );
            if !meaningful {
                return;
            }
        }

        match final_byte {
            '@' => {
                if raw_params.ends_with(' ') {
                    screen.scroll_left(first());
                } else {
                    screen.insert_chars(first());
                }
            }
            'A' => {
                if raw_params.ends_with(' ') {
                    screen.scroll_right(first());
                } else {
                    screen.move_cursor_up(first());
                }
            }
            'B' => screen.move_cursor_down(first()),
            'C' => screen.move_cursor_right(first()),
            'D' => screen.move_cursor_left(first()),
            'E' => screen.cursor_next_line(first()),
            'F' => screen.cursor_previous_line(first()),
            'X' => screen.erase_chars(first()),
            'L' => screen.insert_lines(first()),
            'M' => screen.delete_lines(first()),
            'P' => screen.delete_chars(first()),
            'S' => screen.scroll_up(first()),
            'T' => screen.scroll_down(first()),
            'Z' => screen.reverse_horizontal_tab(first()),
            '`' => screen.set_horizontal_position(first().saturating_sub(1)),
            'a' => screen.move_cursor_right(first()),
            'b' => screen.repeat_previous_char(first()),
            'd' => screen.set_vertical_position(first().saturating_sub(1)),
            'e' => screen.move_cursor_down(first()),
            // CHA is origin-relative, exactly like HPA.
            'G' => screen.set_horizontal_position(first().saturating_sub(1)),
            'I' => screen.cursor_forward_tab(first()),
            'W' => {
                if private && numbers.as_slice() == [5] {
                    screen.reset_tab_stops();
                }
            }
            'g' => screen.clear_tab_stop(numbers.first().copied().unwrap_or(0)),
            'H' | 'f' => {
                let row = numbers.first().copied().filter(|n| *n > 0).unwrap_or(1);
                let col = numbers.get(1).copied().filter(|n| *n > 0).unwrap_or(1);
                screen.set_cursor_position(row.saturating_sub(1), col.saturating_sub(1));
            }
            'J' => {
                let mode = numbers.first().copied().unwrap_or(0);
                if private {
                    screen.selective_erase_in_display(mode);
                } else {
                    screen.erase_in_display(mode);
                }
            }
            'K' => {
                let mode = numbers.first().copied().unwrap_or(0);
                if private {
                    screen.selective_erase_in_line(mode);
                } else {
                    screen.erase_in_line(mode);
                }
            }
            // `CSI > … m` is xterm's resource modifier, `CSI ? … m` a query:
            // neither is a style. Read as SGR, `CSI > 4 ; 2 m` turned on
            // underline and faint for everything printed after it.
            'm' if raw_params.starts_with('>') => {
                screen.apply_modify_keys(&csi_params::parse_numbers(&raw_params[1..]))
            }
            'm' if raw_params.starts_with('?') => {}
            'm' => screen.apply_sgr(&csi_params::parse_sgr(raw_params)),
            'p' => {
                if raw_params == "!" {
                    screen.soft_reset_terminal();
                }
            }
            'q' => {
                if raw_params.ends_with(' ') {
                    screen.set_cursor_shape(numbers.first().copied().unwrap_or(0));
                } else if raw_params.ends_with('"') {
                    let numbers = csi_params::parse_numbers(raw_params);
                    screen.set_character_protection(numbers.first().copied().unwrap_or(0));
                }
            }
            's' => {
                if private {
                    // XTSAVE: DEC private mode values, not the cursor.
                    screen.save_private_modes(&numbers);
                } else if screen.left_right_margin_mode && numbers.len() >= 2 {
                    let left = numbers.first().copied().filter(|n| *n > 0).unwrap_or(1);
                    let right = numbers
                        .get(1)
                        .copied()
                        .filter(|n| *n > 0)
                        .unwrap_or(screen.cols);
                    screen.set_horizontal_margins(left.saturating_sub(1), right.saturating_sub(1));
                } else {
                    screen.save_cursor();
                }
            }
            'u' => match raw_params.chars().next() {
                Some(marker @ ('>' | '<' | '=')) => screen
                    .apply_kitty_keyboard(marker, &csi_params::parse_numbers(&raw_params[1..])),
                // `CSI ? u` is the flags query, answered with the others.
                Some('?') => {}
                _ => screen.restore_cursor(),
            },
            't' => {
                if raw_params.ends_with('$') {
                    let numbers = csi_params::parse_numbers(raw_params);
                    let (top, left, bottom, right) =
                        csi_params::rect_from_numbers(&numbers, screen.rows, screen.cols);
                    let params = numbers.get(4..).unwrap_or(&[]);
                    screen.reverse_rect_attributes(top, left, bottom, right, params);
                } else {
                    Self::handle_window_operation(screen, &numbers);
                }
            }
            'x' => {
                if raw_params.ends_with('$') {
                    let numbers = csi_params::parse_numbers(raw_params);
                    let ch = numbers
                        .first()
                        .copied()
                        .and_then(|code| char::from_u32(code as u32))
                        .filter(|ch| ScreenCell::char_width(*ch) == 1)
                        .unwrap_or(' ');
                    let (top, left, bottom, right) =
                        csi_params::rect_from_numbers(&numbers[1..], screen.rows, screen.cols);
                    screen.fill_rect(ch, top, left, bottom, right);
                }
            }
            'z' => {
                if raw_params.ends_with('$') {
                    let numbers = csi_params::parse_numbers(raw_params);
                    let (top, left, bottom, right) =
                        csi_params::rect_from_numbers(&numbers, screen.rows, screen.cols);
                    screen.erase_rect(top, left, bottom, right);
                }
            }
            '{' => {
                if raw_params.ends_with('$') {
                    let numbers = csi_params::parse_numbers(raw_params);
                    let (top, left, bottom, right) =
                        csi_params::rect_from_numbers(&numbers, screen.rows, screen.cols);
                    screen.selective_erase_rect(top, left, bottom, right);
                }
            }
            'r' => {
                if raw_params.ends_with('$') {
                    let numbers = csi_params::parse_numbers(raw_params);
                    let (top, left, bottom, right) =
                        csi_params::rect_from_numbers(&numbers, screen.rows, screen.cols);
                    let params = numbers.get(4..).unwrap_or(&[]);
                    screen.change_rect_attributes(top, left, bottom, right, params);
                    return;
                }
                if private {
                    // XTRESTORE: mode values only. Read as DECSTBM it reset
                    // the scroll region and homed the cursor.
                    screen.restore_private_modes(&numbers);
                    return;
                }
                let top = numbers.first().copied().filter(|n| *n > 0).unwrap_or(1);
                let bottom = numbers
                    .get(1)
                    .copied()
                    .filter(|n| *n > 0)
                    .unwrap_or(screen.rows);
                screen.set_scroll_region(top.saturating_sub(1), bottom.saturating_sub(1));
            }
            'h' => {
                for mode in &numbers {
                    if private {
                        screen.set_dec_private_mode(*mode, true);
                    } else if *mode == 4 {
                        screen.insert_mode = true;
                    } else if *mode == 20 {
                        screen.newline_mode = true;
                    }
                }
            }
            'l' => {
                for mode in &numbers {
                    if private {
                        screen.set_dec_private_mode(*mode, false);
                    } else if *mode == 4 {
                        screen.insert_mode = false;
                    } else if *mode == 20 {
                        screen.newline_mode = false;
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_window_operation(screen: &mut NextCoreScreen, numbers: &[usize]) {
        let op = numbers.first().copied().unwrap_or(0);
        let target = numbers.get(1).copied().unwrap_or(0);
        match (op, target) {
            (22, 0 | 2) => screen.push_title(),
            (23, 0 | 2) => screen.pop_title(),
            _ => {}
        }
    }
}

impl NextCoreScreen {
    /// `CSI ? Pm h` / `CSI ? Pm l`, one mode at a time.
    pub(super) fn set_dec_private_mode(&mut self, mode: usize, enabled: bool) {
        match (mode, enabled) {
            (1049, true) => self.enter_alternate_screen(1049, true),
            (1047 | 47, true) => self.enter_alternate_screen(mode, false),
            (1049 | 1047 | 47, false) => self.leave_alternate_screen(mode),
            (1048, true) => self.save_cursor(),
            (1048, false) => self.restore_cursor(),
            (5, _) => {
                self.reverse_video = enabled;
                self.mark_all_dirty();
            }
            (1, _) => self.application_cursor_keys = enabled,
            // DECCOLM. xterm ignores it unless allowColumnMode is on, and
            // that is off by default: the window and the PTY keep their
            // size, so switching the grid to 80/132 columns would wrap at a
            // width nobody else agrees on -- and `reset` / `tput init` send
            // `CSI ? 3 l` every time. Nothing changes and nothing is cleared.
            (3, _) => {}
            (6, _) => self.set_origin_mode(enabled),
            (7, _) => self.auto_wrap = enabled,
            (12, _) => {
                self.cursor_blinking = enabled;
                self.mark_dirty_row(self.cursor_y);
            }
            (25, _) => {
                self.cursor_visible = enabled;
                self.mark_dirty_row(self.cursor_y);
            }
            (66, _) => self.application_keypad = enabled,
            (69, true) => self.left_right_margin_mode = true,
            (69, false) => {
                self.left_right_margin_mode = false;
                self.set_horizontal_margins(0, self.cols.saturating_sub(1));
            }
            (1000 | 1002 | 1003, _) => {
                let tracking = match mode {
                    1000 => MouseTrackingMode::ButtonEvent,
                    1002 => MouseTrackingMode::ButtonMotion,
                    _ => MouseTrackingMode::AnyEvent,
                };
                if enabled {
                    self.mouse_tracking = tracking;
                } else if self.mouse_tracking == tracking {
                    self.mouse_tracking = MouseTrackingMode::None;
                }
            }
            (1004, _) => self.focus_event_reporting = enabled,
            (1005, _) => self.utf8_mouse = enabled,
            (1006, _) => self.sgr_mouse = enabled,
            (1007, _) => self.alternate_scroll = enabled,
            (1015, _) => self.urxvt_mouse = enabled,
            (1016, _) => self.sgr_pixel_mouse = enabled,
            (1034, _) => self.meta_sends_escape = enabled,
            (2004, _) => self.set_bracketed_paste(enabled),
            (2026, _) => self.set_synchronized_output(enabled),
            _ => {}
        }
    }

    /// The value of a DEC private mode XTSAVE can keep, or `None` for a
    /// mode it leaves alone. The alternate screen (47/1047/1049), 1048
    /// (a cursor save, not a value), DECCOLM (ignored) and synchronized
    /// output (a restore must not resurrect a hold) are not saved.
    fn savable_private_mode(&self, mode: usize) -> Option<bool> {
        Some(match mode {
            1 => self.application_cursor_keys,
            5 => self.reverse_video,
            6 => self.origin_mode,
            7 => self.auto_wrap,
            12 => self.cursor_blinking,
            25 => self.cursor_visible,
            66 => self.application_keypad,
            69 => self.left_right_margin_mode,
            1000 => self.mouse_tracking == MouseTrackingMode::ButtonEvent,
            1002 => self.mouse_tracking == MouseTrackingMode::ButtonMotion,
            1003 => self.mouse_tracking == MouseTrackingMode::AnyEvent,
            1004 => self.focus_event_reporting,
            1005 => self.utf8_mouse,
            1006 => self.sgr_mouse,
            1007 => self.alternate_scroll,
            1015 => self.urxvt_mouse,
            1016 => self.sgr_pixel_mouse,
            1034 => self.meta_sends_escape,
            2004 => self.bracketed_paste,
            _ => return None,
        })
    }

    /// XTSAVE, `CSI ? Pm s`.
    pub(super) fn save_private_modes(&mut self, modes: &[usize]) {
        for &mode in modes {
            if let Some(value) = self.savable_private_mode(mode) {
                self.saved_private_modes.insert(mode, value);
            }
        }
    }

    /// XTRESTORE, `CSI ? Pm r`: put back what XTSAVE kept. Leaves the
    /// scroll region, the cursor and the DECSC slot alone -- DECOM included,
    /// which is restored as a value without the homing `CSI ? 6 h` does.
    pub(super) fn restore_private_modes(&mut self, modes: &[usize]) {
        for &mode in modes {
            let Some(&value) = self.saved_private_modes.get(&mode) else {
                continue;
            };
            match mode {
                6 => self.origin_mode = value,
                69 if !value => self.left_right_margin_mode = false,
                _ => self.set_dec_private_mode(mode, value),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_csi_state_across_chunks() {
        let mut screen = NextCoreScreen::new(10, 2);
        let mut parser = TerminalParser::default();

        parser.feed(&mut screen, "abcd\x1b[2");
        parser.feed(&mut screen, "DZ");

        assert_eq!(&screen.snapshot_viewport_lines()[0][..4], "abZd");
    }

    #[test]
    fn applies_osc_st_across_chunks() {
        let mut screen = NextCoreScreen::new(10, 2);
        let mut parser = TerminalParser::default();

        parser.feed(&mut screen, "\x1b]0;hello");
        parser.feed(&mut screen, "\x1b\\");

        assert_eq!(screen.title.as_deref(), Some("hello"));
    }

    #[test]
    fn ignores_split_control_strings_until_st() {
        let mut screen = NextCoreScreen::new(10, 2);
        let mut parser = TerminalParser::default();

        parser.feed(&mut screen, "a\x1bPignored");
        parser.feed(&mut screen, "\x1b\\b");

        assert_eq!(&screen.snapshot_viewport_lines()[0][..2], "ab");
    }

    #[test]
    fn decst8c_resets_tab_stops_to_every_eight_columns() {
        let mut screen = NextCoreScreen::new(40, 4);
        screen.feed("\x1b[3g");
        assert!(screen.tab_stops.is_empty());

        screen.feed("\x1b[?5W\ta");
        assert_eq!(screen.cursor_x, 9);
        assert_eq!(&screen.snapshot_viewport_lines()[0][..9], "        a");
    }

    #[test]
    fn resizing_narrow_and_wide_again_restores_eight_column_tab_stops() {
        let mut screen = NextCoreScreen::new(80, 24);
        screen.resize(4, 24);
        assert!(screen.tab_stops.is_empty());

        screen.resize(80, 24);
        screen.feed("\ta");
        assert_eq!(screen.cursor_x, 9);
        assert_eq!(&screen.snapshot_viewport_lines()[0][..9], "        a");
    }

    #[test]
    fn synchronized_hold_survives_mid_frame_scroll_on_unpinned_viewport() {
        let mut screen = NextCoreScreen::new(20, 3);
        screen.feed("row0\r\nrow1\r\nrow2");
        assert_eq!(
            screen.snapshot_viewport_lines(),
            vec!["row0", "row1", "row2"]
        );

        // Start a synchronized update that scrolls a line into history before
        // `?2026l` arrives. An unpinned reader must still see the pre-update
        // frame rather than a half-drawn mid-scroll screen.
        screen.feed("\x1b[?2026h\r\nhalf-written");
        let (held_lines, held_cursor) = screen.shown_viewport_lines(screen.viewport_first_row());
        let held_text: Vec<String> = held_lines
            .iter()
            .map(|l| l.cells.iter().map(|c| c.ch).collect::<String>().trim_end().to_string())
            .collect();
        assert_eq!(held_text, vec!["row0", "row1", "row2"]);
        assert!(held_cursor.is_some());

        let rev_during_hold = screen.revision();
        screen.feed("\x1b[?2026l");
        assert!(screen.revision() > rev_during_hold);
        let (live_lines, live_cursor) = screen.shown_viewport_lines(screen.viewport_first_row());
        let live_text: Vec<String> = live_lines
            .iter()
            .map(|l| l.cells.iter().map(|c| c.ch).collect::<String>().trim_end().to_string())
            .collect();
        assert_eq!(live_text, vec!["row1", "row2", "half-written"]);
        assert!(live_cursor.is_none());
    }
}
