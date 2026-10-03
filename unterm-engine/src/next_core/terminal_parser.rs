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
                '\n' | '\x0b' | '\x0c' => screen.newline(),
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
            'G' => {
                let row = screen.cursor_y;
                screen.set_cursor(row, first().saturating_sub(1));
            }
            'I' => screen.cursor_forward_tab(first()),
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
                if !private && screen.left_right_margin_mode && numbers.len() >= 2 {
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
                        match *mode {
                            1049 => screen.enter_alternate_screen(1049, true),
                            1047 | 47 => screen.enter_alternate_screen(*mode, false),
                            1048 => screen.save_cursor(),
                            5 => {
                                screen.reverse_video = true;
                                screen.mark_all_dirty();
                            }
                            1 => screen.application_cursor_keys = true,
                            3 => screen.set_column_mode(true),
                            6 => screen.set_origin_mode(true),
                            7 => screen.auto_wrap = true,
                            12 => {
                                screen.cursor_blinking = true;
                                screen.mark_dirty_row(screen.cursor_y);
                            }
                            25 => {
                                screen.cursor_visible = true;
                                screen.mark_dirty_row(screen.cursor_y);
                            }
                            66 => screen.application_keypad = true,
                            69 => screen.left_right_margin_mode = true,
                            1000 => screen.mouse_tracking = MouseTrackingMode::ButtonEvent,
                            1002 => screen.mouse_tracking = MouseTrackingMode::ButtonMotion,
                            1003 => screen.mouse_tracking = MouseTrackingMode::AnyEvent,
                            1004 => screen.focus_event_reporting = true,
                            1005 => screen.utf8_mouse = true,
                            1006 => screen.sgr_mouse = true,
                            1007 => screen.alternate_scroll = true,
                            1015 => screen.urxvt_mouse = true,
                            1016 => screen.sgr_pixel_mouse = true,
                            1034 => screen.meta_sends_escape = true,
                            2004 => screen.set_bracketed_paste(true),
                            2026 => screen.set_synchronized_output(true),
                            _ => {}
                        }
                    } else if *mode == 4 {
                        screen.insert_mode = true;
                    }
                }
            }
            'l' => {
                for mode in &numbers {
                    if private {
                        match *mode {
                            1049 | 1047 | 47 => screen.leave_alternate_screen(*mode),
                            1048 => screen.restore_cursor(),
                            5 => {
                                screen.reverse_video = false;
                                screen.mark_all_dirty();
                            }
                            1 => screen.application_cursor_keys = false,
                            3 => screen.set_column_mode(false),
                            6 => screen.set_origin_mode(false),
                            7 => screen.auto_wrap = false,
                            12 => {
                                screen.cursor_blinking = false;
                                screen.mark_dirty_row(screen.cursor_y);
                            }
                            25 => {
                                screen.cursor_visible = false;
                                screen.mark_dirty_row(screen.cursor_y);
                            }
                            66 => screen.application_keypad = false,
                            69 => {
                                screen.left_right_margin_mode = false;
                                screen.set_horizontal_margins(0, screen.cols.saturating_sub(1));
                            }
                            1000 => {
                                if screen.mouse_tracking == MouseTrackingMode::ButtonEvent {
                                    screen.mouse_tracking = MouseTrackingMode::None;
                                }
                            }
                            1002 => {
                                if screen.mouse_tracking == MouseTrackingMode::ButtonMotion {
                                    screen.mouse_tracking = MouseTrackingMode::None;
                                }
                            }
                            1003 => {
                                if screen.mouse_tracking == MouseTrackingMode::AnyEvent {
                                    screen.mouse_tracking = MouseTrackingMode::None;
                                }
                            }
                            1004 => screen.focus_event_reporting = false,
                            1005 => screen.utf8_mouse = false,
                            1006 => screen.sgr_mouse = false,
                            1007 => screen.alternate_scroll = false,
                            1015 => screen.urxvt_mouse = false,
                            1016 => screen.sgr_pixel_mouse = false,
                            1034 => screen.meta_sends_escape = false,
                            2004 => screen.set_bracketed_paste(false),
                            2026 => screen.set_synchronized_output(false),
                            _ => {}
                        }
                    } else if *mode == 4 {
                        screen.insert_mode = false;
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
}
