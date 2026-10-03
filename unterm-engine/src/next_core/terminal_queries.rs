use super::{MouseTrackingMode, NextCoreScreen};
use parking_lot::Mutex;
use std::io::Write;
use std::sync::Arc;

const HEADLESS_CELL_WIDTH_PX: usize = 8;
const HEADLESS_CELL_HEIGHT_PX: usize = 16;
pub(super) const MAX_PENDING_TERMINAL_QUERY_BYTES: usize = 128;

/// One cell in pixels: what the front end draws, once it has said, and a
/// nominal 8x16 before that (a headless kernel has no pixels at all).
pub(super) fn cell_pixels() -> (u32, u32) {
    unterm_images::cell_pixels()
        .unwrap_or((HEADLESS_CELL_WIDTH_PX as u32, HEADLESS_CELL_HEIGHT_PX as u32))
}

pub(super) fn answer_with_pending(
    chunk: &str,
    screen: &NextCoreScreen,
    writer: &Arc<Mutex<Box<dyn Write + Send>>>,
    pending: &mut String,
) -> usize {
    let mut response = Vec::new();
    let input = if pending.is_empty() {
        chunk.to_string()
    } else {
        let mut input = std::mem::take(pending);
        input.push_str(chunk);
        input
    };
    let bytes = input.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] != 0x1b {
            idx += 1;
            continue;
        }

        if bytes.get(idx + 1) == Some(&b']') {
            // An OSC: answered only when it is a colour question, and only
            // once its terminator has arrived -- BEL or ST, echoed back in
            // the same form, since some programs only read the one they sent.
            let body_start = idx + 2;
            let mut end = None;
            let mut cursor = body_start;
            while cursor < bytes.len() {
                match bytes[cursor] {
                    0x07 => {
                        end = Some((cursor, cursor + 1, "\x07"));
                        break;
                    }
                    0x1b if bytes.get(cursor + 1) == Some(&b'\\') => {
                        end = Some((cursor, cursor + 2, "\x1b\\"));
                        break;
                    }
                    0x1b if cursor + 1 >= bytes.len() => break,
                    _ => cursor += 1,
                }
            }
            let Some((body_end, after, terminator)) = end else {
                set_pending(pending, &input[idx..]);
                break;
            };
            if let Some(answer) = response_for_osc(&input[body_start..body_end], terminator) {
                response.extend_from_slice(answer.as_bytes());
            }
            idx = after;
            continue;
        }

        if bytes.get(idx + 1) != Some(&b'[') {
            if idx + 1 >= bytes.len() {
                set_pending(pending, &input[idx..]);
                break;
            }
            idx += 1;
            continue;
        }

        let mut final_end = None;
        for (offset, c) in input[idx + 2..].char_indices() {
            if ('@'..='~').contains(&c) {
                final_end = Some(idx + 2 + offset + c.len_utf8());
                break;
            }
        }

        let Some(end) = final_end else {
            set_pending(pending, &input[idx..]);
            break;
        };

        if let Some(answer) = response_for_csi(&input[idx + 2..end], screen) {
            response.extend_from_slice(answer.as_slice());
            idx = end;
        } else {
            idx += 1;
        }
    }
    if !response.is_empty() {
        let response_bytes = response.len();
        let mut writer = writer.lock();
        writer.write_all(&response).ok();
        writer.flush().ok();
        return response_bytes;
    }
    0
}

fn set_pending(pending: &mut String, value: &str) {
    pending.clear();
    if value.len() <= MAX_PENDING_TERMINAL_QUERY_BYTES {
        pending.push_str(value);
    }
}

fn response_for_csi(csi: &str, screen: &NextCoreScreen) -> Option<Vec<u8>> {
    if let Some(mode) = csi
        .strip_prefix('?')
        .and_then(|params| params.strip_suffix("$p"))
        .and_then(|params| params.parse::<usize>().ok())
    {
        let enabled = match mode {
            1 => screen.application_cursor_keys,
            3 => screen.column_132_mode,
            5 => screen.reverse_video,
            6 => screen.origin_mode,
            7 => screen.auto_wrap,
            12 => screen.cursor_blinking,
            25 => screen.cursor_visible,
            47 => screen.alternate_screen_modes.contains(&47),
            66 => screen.application_keypad,
            69 => screen.left_right_margin_mode,
            1000 => screen.mouse_tracking == MouseTrackingMode::ButtonEvent,
            1002 => screen.mouse_tracking == MouseTrackingMode::ButtonMotion,
            1003 => screen.mouse_tracking == MouseTrackingMode::AnyEvent,
            1004 => screen.focus_event_reporting,
            1005 => screen.utf8_mouse,
            1006 => screen.sgr_mouse,
            1007 => screen.alternate_scroll,
            1015 => screen.urxvt_mouse,
            1016 => screen.sgr_pixel_mouse,
            1034 => screen.meta_sends_escape,
            1047 => screen.alternate_screen_modes.contains(&1047),
            1049 => screen.alternate_screen_modes.contains(&1049),
            2004 => screen.bracketed_paste,
            2026 => screen.synchronized_output,
            _ => return None,
        };
        return Some(format!("\x1b[?{mode};{}$y", mode_report_state(enabled)).into_bytes());
    }

    if let Some(mode) = csi
        .strip_suffix("$p")
        .and_then(|params| params.parse::<usize>().ok())
    {
        if mode == 4 {
            return Some(
                format!("\x1b[4;{}$y", mode_report_state(screen.insert_mode)).into_bytes(),
            );
        }
    }

    match csi {
        "?6n" => {
            Some(format!("\x1b[?{};{}R", screen.cursor_y + 1, screen.cursor_x + 1).into_bytes())
        }
        // Image tools size their pictures from these two: the text area,
        // and one cell, in pixels.
        "14t" => {
            let (width, height) = cell_pixels();
            Some(
                format!(
                    "\x1b[4;{};{}t",
                    screen.rows as u64 * u64::from(height),
                    screen.cols as u64 * u64::from(width)
                )
                .into_bytes(),
            )
        }
        "16t" => {
            let (width, height) = cell_pixels();
            Some(format!("\x1b[6;{height};{width}t").into_bytes())
        }
        "18t" => Some(format!("\x1b[8;{};{}t", screen.rows, screen.cols).into_bytes()),
        "5n" => Some(b"\x1b[0n".to_vec()),
        // The kitty keyboard query: the flags in force, of those supported.
        "?u" => Some(format!("\x1b[?{}u", screen.kitty_keyboard_flags()).into_bytes()),
        "6n" => Some(format!("\x1b[{};{}R", screen.cursor_y + 1, screen.cursor_x + 1).into_bytes()),
        ">c" | ">0c" => Some(b"\x1b[>0;0;0c".to_vec()),
        // XTVERSION. What a program checks before relying on anything newer
        // than the DA answers can say.
        ">q" | ">0q" => Some(
            format!("\x1bP>|Unterm {}\x1b\\", unterm_protocol::PRODUCT_VERSION).into_bytes(),
        ),
        // 4: sixel graphics.
        "c" | "0c" => Some(b"\x1b[?64;1;2;4;6;9;15;18;21;22c".to_vec()),
        _ => None,
    }
}

/// Answer `OSC 10/11/12 ; ?` (foreground, background, cursor) and
/// `OSC 4 ; <index> ; ?` (a palette entry) from the colours the front end
/// last reported. Anything else is not a question and gets no answer.
///
/// `OSC 10 ; ? ; ?` asks for 10 and then 11: each further `?` moves to the
/// next of the dynamic colours, which is how xterm reads the list.
fn response_for_osc(body: &str, terminator: &str) -> Option<String> {
    let colors = super::color::reported_colors();
    let (kind, rest) = body.split_once(';')?;
    let mut answer = String::new();
    match kind {
        "10" | "11" | "12" => {
            let first: u8 = kind.parse().ok()?;
            for (offset, spec) in rest.split(';').enumerate() {
                if spec != "?" {
                    continue;
                }
                let which = first + offset as u8;
                let color = match which {
                    10 => colors.foreground,
                    11 => colors.background,
                    12 => colors.cursor,
                    _ => continue,
                };
                answer.push_str(&format!("\x1b]{which};{}{terminator}", color.to_xparse()));
            }
        }
        "4" => {
            let mut parts = rest.split(';');
            while let (Some(index), Some(spec)) = (parts.next(), parts.next()) {
                if spec != "?" {
                    continue;
                }
                let Ok(index) = index.parse::<u8>() else {
                    continue;
                };
                let color = match colors.ansi.get(index as usize) {
                    Some(color) => *color,
                    None => super::color::palette_rgb(index),
                };
                answer.push_str(&format!("\x1b]4;{index};{}{terminator}", color.to_xparse()));
            }
        }
        _ => return None,
    }
    (!answer.is_empty()).then_some(answer)
}

#[cfg(test)]
pub(super) fn response_for_csi_for_tests(csi: &str, screen: &NextCoreScreen) -> Option<Vec<u8>> {
    response_for_csi(csi, screen)
}

fn mode_report_state(enabled: bool) -> usize {
    if enabled {
        1
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responds_to_basic_status_and_device_queries() {
        let mut screen = NextCoreScreen::new(80, 10);
        screen.set_cursor(2, 4);

        assert_eq!(response_for_csi("6n", &screen).unwrap(), b"\x1b[3;5R");
        assert_eq!(response_for_csi("?6n", &screen).unwrap(), b"\x1b[?3;5R");
        assert_eq!(response_for_csi("5n", &screen).unwrap(), b"\x1b[0n");
        assert_eq!(
            response_for_csi("c", &screen).unwrap(),
            b"\x1b[?64;1;2;4;6;9;15;18;21;22c"
        );
        assert_eq!(response_for_csi(">0c", &screen).unwrap(), b"\x1b[>0;0;0c");
    }

    #[test]
    fn responds_to_window_size_queries() {
        let screen = NextCoreScreen::new(132, 43);

        assert_eq!(
            response_for_csi("14t", &screen).unwrap(),
            b"\x1b[4;688;1056t"
        );
        assert_eq!(response_for_csi("18t", &screen).unwrap(), b"\x1b[8;43;132t");
    }

    /// The questions TUIs ask to choose their palette, answered from the
    /// front end's colours in the form and with the terminator they used.
    #[test]
    fn colour_queries_are_answered_from_the_reported_theme() {
        use super::super::color::{set_reported_colors, Rgb, TerminalColors};
        let mut colors = TerminalColors::default();
        colors.background = Rgb::new(0xff, 0xff, 0xff);
        colors.foreground = Rgb::new(0x10, 0x20, 0x30);
        colors.ansi[1] = Rgb::new(0xab, 0x00, 0x01);
        set_reported_colors(colors);

        assert_eq!(
            response_for_osc("11;?", "\x07").unwrap(),
            "\x1b]11;rgb:ffff/ffff/ffff\x07"
        );
        assert_eq!(
            response_for_osc("10;?;?", "\x1b\\").unwrap(),
            "\x1b]10;rgb:1010/2020/3030\x1b\\\x1b]11;rgb:ffff/ffff/ffff\x1b\\"
        );
        assert_eq!(
            response_for_osc("4;1;?", "\x07").unwrap(),
            "\x1b]4;1;rgb:abab/0000/0101\x07"
        );
        // Setting a colour is not a question.
        assert!(response_for_osc("11;#000000", "\x07").is_none());
        assert!(response_for_osc("2;title", "\x07").is_none());
        set_reported_colors(TerminalColors::default());
    }

    #[test]
    fn xtversion_names_the_product() {
        let screen = NextCoreScreen::new(80, 10);
        let answer = String::from_utf8(response_for_csi(">q", &screen).unwrap()).unwrap();
        assert!(answer.starts_with("\x1bP>|Unterm "), "{:?}", answer);
        assert!(answer.ends_with("\x1b\\"));
    }

    #[test]
    fn responds_to_mode_reports() {
        let mut screen = NextCoreScreen::new(80, 10);
        screen.application_cursor_keys = true;
        screen.insert_mode = true;

        assert_eq!(response_for_csi("?1$p", &screen).unwrap(), b"\x1b[?1;1$y");
        assert_eq!(response_for_csi("4$p", &screen).unwrap(), b"\x1b[4;1$y");
        assert!(response_for_csi("?9999$p", &screen).is_none());
    }
}
