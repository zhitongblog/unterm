//! Key encoding for next-core input.
//!
//! GUI key events arrive as a termwiz `KeyCode` plus `Modifiers`; a PTY wants
//! bytes. This turns one into the other.
//!
//! Everything here emits **normal-mode** sequences. Application cursor mode is
//! not applied at this layer — `input_dispatch::write` runs the result through
//! `input_pipeline::application_cursor_input` using the session's live mode
//! state, so encoding twice would double-translate. Keep that split: this
//! function is pure and mode-free, the session owns the mode.

use termwiz::input::{KeyCode, Modifiers};

/// xterm modifier parameter: 1 + a bitmask of the active modifiers.
///
/// Returns `None` when no modifier that xterm encodes is held, which is the
/// signal to emit the short (unparameterized) form of a sequence.
fn modifier_param(mods: Modifiers) -> Option<u8> {
    let mut bits = 0;
    if mods.contains(Modifiers::SHIFT) {
        bits |= 1;
    }
    if mods.contains(Modifiers::ALT) {
        bits |= 2;
    }
    if mods.contains(Modifiers::CTRL) {
        bits |= 4;
    }
    if mods.contains(Modifiers::SUPER) {
        bits |= 8;
    }
    (bits != 0).then_some(bits + 1)
}

/// `CSI <final>` / `CSI 1 ; <mods> <final>` — the arrow/Home/End family.
fn csi_letter(final_byte: char, mods: Modifiers) -> String {
    match modifier_param(mods) {
        Some(param) => format!("\x1b[1;{param}{final_byte}"),
        None => format!("\x1b[{final_byte}"),
    }
}

/// `CSI <n> ~` / `CSI <n> ; <mods> ~` — the Insert/Delete/PageUp family.
fn csi_tilde(number: u8, mods: Modifiers) -> String {
    match modifier_param(mods) {
        Some(param) => format!("\x1b[{number};{param}~"),
        None => format!("\x1b[{number}~"),
    }
}

/// Map a character to its control byte, following the classic ASCII rules.
///
/// Returns `None` for characters that have no control form, so the caller can
/// fall back to sending the character itself.
fn control_byte(c: char) -> Option<char> {
    let byte = match c {
        ' ' | '@' => 0x00,
        '[' => 0x1b,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' => 0x1f,
        '?' => 0x7f,
        'a'..='z' => c as u8 - b'a' + 1,
        'A'..='Z' => c as u8 - b'A' + 1,
        _ => return None,
    };
    Some(byte as char)
}

fn encode_char(c: char, mods: Modifiers) -> Option<String> {
    let base = if mods.contains(Modifiers::CTRL) {
        // An unmapped Ctrl+<char> (Ctrl+1, Ctrl+;) has no control byte;
        // sending the bare character matches what xterm does.
        control_byte(c).unwrap_or(c).to_string()
    } else {
        c.to_string()
    };

    // ALT is the ESC prefix. SUPER has no PTY encoding — it is a
    // window-manager modifier, so a SUPER chord must not reach the shell as
    // if it were an unmodified keystroke.
    if mods.contains(Modifiers::SUPER) {
        return None;
    }
    if mods.contains(Modifiers::ALT) {
        return Some(format!("\x1b{base}"));
    }
    Some(base)
}

/// The keyboard protocols a program asked for, as far as encoding goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyboardModes {
    /// kitty keyboard flags in force (only `1`, disambiguate, is acted on).
    pub kitty: u8,
    /// xterm `modifyOtherKeys` level, 0-2.
    pub modify_other_keys: u8,
}

/// Whether `key` with `mods` encodes differently under some keyboard mode,
/// so a front end knows when the pane's modes are worth asking for.
///
/// Plain typing never is: asking the Core about every letter would put a
/// round trip in front of each keystroke for nothing.
pub fn depends_on_keyboard_modes(key: KeyCode, mods: Modifiers) -> bool {
    let modified = mods.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER);
    match key {
        KeyCode::Escape => true,
        KeyCode::Enter | KeyCode::Tab | KeyCode::Backspace => !mods.is_empty(),
        KeyCode::Char(_) => modified,
        _ => false,
    }
}

/// Encode a key press under the keyboard protocols the program asked for.
///
/// The kitty protocol's "disambiguate" level (flag 1) is what agents and
/// modern TUIs ask for: it is how Shift+Enter becomes a newline in Claude
/// Code's prompt instead of a send, and how Esc stops being confused with
/// the start of an Alt chord. Keys whose legacy encoding is unambiguous --
/// plain text, Enter without modifiers, the arrows -- keep it, exactly as the
/// protocol specifies.
pub fn encode_key_with(key: KeyCode, mods: Modifiers, modes: KeyboardModes) -> Option<String> {
    let param = modifier_param(mods);
    if modes.kitty & 1 != 0 {
        let codepoint = |code: u32| match param {
            Some(param) => format!("\x1b[{code};{param}u"),
            None => format!("\x1b[{code}u"),
        };
        match key {
            KeyCode::Escape => return Some(codepoint(27)),
            KeyCode::Enter if param.is_some() => return Some(codepoint(13)),
            KeyCode::Tab if param.is_some() => return Some(codepoint(9)),
            KeyCode::Backspace if param.is_some() => return Some(codepoint(127)),
            KeyCode::Char(c)
                if mods.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER) =>
            {
                // The key, not the shifted symbol: the protocol names keys
                // by their unshifted codepoint and carries Shift in the
                // modifiers.
                let base = c.to_lowercase().next().unwrap_or(c);
                return Some(codepoint(base as u32));
            }
            _ => {}
        }
    } else if modes.modify_other_keys > 0 {
        let other = |code: u32| param.map(|param| format!("\x1b[27;{param};{code}~"));
        match key {
            KeyCode::Enter if param.is_some() => return other(13),
            KeyCode::Tab if param.is_some() && mods != Modifiers::SHIFT => return other(9),
            KeyCode::Backspace if param.is_some() => return other(127),
            // Level 2 reports every Ctrl/Alt chord; level 1 only the ones
            // the legacy encoding cannot tell apart (Ctrl+digit and the
            // like, which have no control byte).
            KeyCode::Char(c) if mods.intersects(Modifiers::CTRL | Modifiers::ALT) => {
                let ambiguous = mods.contains(Modifiers::CTRL) && control_byte(c).is_none();
                if modes.modify_other_keys >= 2 || ambiguous {
                    return other(c as u32);
                }
            }
            _ => {}
        }
    }
    encode_key(key, mods)
}

/// Encode a key press for a next-core PTY.
///
/// Returns `None` when the key produces no input: modifier keys themselves,
/// SUPER chords (window-manager territory), and keys with no terminal meaning.
/// A `None` result means "this keystroke is not for the shell", not "encoding
/// failed" — the caller should simply send nothing.
pub fn encode_key(key: KeyCode, mods: Modifiers) -> Option<String> {
    Some(match key {
        KeyCode::Char(c) => return encode_char(c, mods),

        KeyCode::Enter => "\r".to_string(),
        KeyCode::Escape => "\x1b".to_string(),
        // DEL, not BS: this is what every modern terminal sends, and what
        // readline expects for the Backspace key.
        KeyCode::Backspace => "\x7f".to_string(),
        KeyCode::Tab => {
            if mods.contains(Modifiers::SHIFT) {
                "\x1b[Z".to_string()
            } else {
                "\t".to_string()
            }
        }

        KeyCode::UpArrow => csi_letter('A', mods),
        KeyCode::DownArrow => csi_letter('B', mods),
        KeyCode::RightArrow => csi_letter('C', mods),
        KeyCode::LeftArrow => csi_letter('D', mods),
        KeyCode::Home => csi_letter('H', mods),
        KeyCode::End => csi_letter('F', mods),

        KeyCode::Insert => csi_tilde(2, mods),
        KeyCode::Delete => csi_tilde(3, mods),
        KeyCode::PageUp => csi_tilde(5, mods),
        KeyCode::PageDown => csi_tilde(6, mods),

        KeyCode::Function(n) => return encode_function_key(n, mods),

        // Modifier keys and everything without a terminal encoding.
        _ => return None,
    })
}

fn encode_function_key(n: u8, mods: Modifiers) -> Option<String> {
    // F1-F4 are SS3 sequences when unmodified and CSI when modified;
    // F5 and up are always tilde sequences with a per-key number.
    let ss3_final = match n {
        1 => Some('P'),
        2 => Some('Q'),
        3 => Some('R'),
        4 => Some('S'),
        _ => None,
    };
    if let Some(final_byte) = ss3_final {
        return Some(match modifier_param(mods) {
            Some(param) => format!("\x1b[1;{param}{final_byte}"),
            None => format!("\x1bO{final_byte}"),
        });
    }

    let number = match n {
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        13 => 25,
        14 => 26,
        15 => 28,
        16 => 29,
        17 => 31,
        18 => 32,
        19 => 33,
        20 => 34,
        _ => return None,
    };
    Some(csi_tilde(number, mods))
}

#[cfg(test)]
mod tests {
    use super::{encode_key_with, KeyboardModes};

    fn kitty() -> KeyboardModes {
        KeyboardModes { kitty: 1, modify_other_keys: 0 }
    }

    /// What an agent's prompt needs from the kitty protocol: Shift+Enter is
    /// not Enter, Esc is not the start of an Alt chord, Ctrl chords carry
    /// their key -- and plain typing is untouched.
    #[test]
    fn kitty_disambiguate_encodes_only_what_was_ambiguous() {
        use termwiz::input::{KeyCode, Modifiers};
        let k = |key, mods| encode_key_with(key, mods, kitty());
        assert_eq!(k(KeyCode::Enter, Modifiers::SHIFT).as_deref(), Some("\x1b[13;2u"));
        assert_eq!(k(KeyCode::Enter, Modifiers::NONE).as_deref(), Some("\r"));
        assert_eq!(k(KeyCode::Escape, Modifiers::NONE).as_deref(), Some("\x1b[27u"));
        assert_eq!(k(KeyCode::Char('c'), Modifiers::CTRL).as_deref(), Some("\x1b[99;5u"));
        assert_eq!(
            k(KeyCode::Char('A'), Modifiers::CTRL | Modifiers::SHIFT).as_deref(),
            Some("\x1b[97;6u")
        );
        assert_eq!(k(KeyCode::Char('x'), Modifiers::ALT).as_deref(), Some("\x1b[120;3u"));
        assert_eq!(k(KeyCode::Tab, Modifiers::SHIFT).as_deref(), Some("\x1b[9;2u"));
        assert_eq!(k(KeyCode::Backspace, Modifiers::CTRL).as_deref(), Some("\x1b[127;5u"));
        assert_eq!(k(KeyCode::Char('a'), Modifiers::NONE).as_deref(), Some("a"));
        assert_eq!(k(KeyCode::Char('A'), Modifiers::SHIFT).as_deref(), Some("A"));
        assert_eq!(k(KeyCode::UpArrow, Modifiers::CTRL).as_deref(), Some("\x1b[1;5A"));
        // Without the protocol, the legacy bytes, unchanged.
        assert_eq!(
            encode_key_with(KeyCode::Enter, Modifiers::SHIFT, KeyboardModes::default()).as_deref(),
            Some("\r")
        );
    }

    #[test]
    fn modify_other_keys_reports_modified_enter_and_chords() {
        use termwiz::input::{KeyCode, Modifiers};
        let level = |n| KeyboardModes { kitty: 0, modify_other_keys: n };
        assert_eq!(
            encode_key_with(KeyCode::Enter, Modifiers::SHIFT, level(2)).as_deref(),
            Some("\x1b[27;2;13~")
        );
        assert_eq!(
            encode_key_with(KeyCode::Char('c'), Modifiers::CTRL, level(2)).as_deref(),
            Some("\x1b[27;5;99~")
        );
        // Level 1 leaves the chords that already have a byte alone.
        assert_eq!(
            encode_key_with(KeyCode::Char('c'), Modifiers::CTRL, level(1)).as_deref(),
            Some("\u{3}")
        );
        assert_eq!(
            encode_key_with(KeyCode::Char('1'), Modifiers::CTRL, level(1)).as_deref(),
            Some("\x1b[27;5;49~")
        );
    }

    use super::encode_key;
    use termwiz::input::{KeyCode, Modifiers};

    #[test]
    fn plain_characters_encode_as_themselves() {
        assert_eq!(
            encode_key(KeyCode::Char('a'), Modifiers::NONE).as_deref(),
            Some("a")
        );
        assert_eq!(
            encode_key(KeyCode::Char('你'), Modifiers::NONE).as_deref(),
            Some("你")
        );
        assert_eq!(
            encode_key(KeyCode::Char('A'), Modifiers::SHIFT).as_deref(),
            Some("A")
        );
    }

    #[test]
    fn ctrl_characters_encode_as_control_bytes() {
        assert_eq!(
            encode_key(KeyCode::Char('c'), Modifiers::CTRL).as_deref(),
            Some("\u{3}")
        );
        assert_eq!(
            encode_key(KeyCode::Char('C'), Modifiers::CTRL).as_deref(),
            Some("\u{3}")
        );
        assert_eq!(
            encode_key(KeyCode::Char('d'), Modifiers::CTRL).as_deref(),
            Some("\u{4}")
        );
        assert_eq!(
            encode_key(KeyCode::Char(' '), Modifiers::CTRL).as_deref(),
            Some("\u{0}")
        );
        assert_eq!(
            encode_key(KeyCode::Char('['), Modifiers::CTRL).as_deref(),
            Some("\u{1b}")
        );
        assert_eq!(
            encode_key(KeyCode::Char('?'), Modifiers::CTRL).as_deref(),
            Some("\u{7f}")
        );
    }

    #[test]
    fn ctrl_without_a_control_byte_sends_the_bare_character() {
        assert_eq!(
            encode_key(KeyCode::Char('1'), Modifiers::CTRL).as_deref(),
            Some("1")
        );
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(
            encode_key(KeyCode::Char('b'), Modifiers::ALT).as_deref(),
            Some("\x1bb")
        );
        // Alt+Ctrl+C is ESC followed by the control byte.
        assert_eq!(
            encode_key(KeyCode::Char('c'), Modifiers::ALT | Modifiers::CTRL).as_deref(),
            Some("\x1b\u{3}")
        );
    }

    #[test]
    fn super_chords_produce_no_pty_input() {
        // Cmd/Win+C is a window-manager binding. It must not reach the shell
        // as a bare "c" — that would type a character on every hotkey.
        assert_eq!(encode_key(KeyCode::Char('c'), Modifiers::SUPER), None);
    }

    #[test]
    fn control_keys_use_canonical_bytes() {
        assert_eq!(
            encode_key(KeyCode::Enter, Modifiers::NONE).as_deref(),
            Some("\r")
        );
        assert_eq!(
            encode_key(KeyCode::Tab, Modifiers::NONE).as_deref(),
            Some("\t")
        );
        assert_eq!(
            encode_key(KeyCode::Tab, Modifiers::SHIFT).as_deref(),
            Some("\x1b[Z")
        );
        assert_eq!(
            encode_key(KeyCode::Backspace, Modifiers::NONE).as_deref(),
            Some("\x7f")
        );
        assert_eq!(
            encode_key(KeyCode::Escape, Modifiers::NONE).as_deref(),
            Some("\x1b")
        );
    }

    #[test]
    fn arrows_encode_in_normal_mode_so_the_session_can_translate() {
        // Normal mode (CSI), never application mode (SS3): the session applies
        // application-cursor translation after this.
        assert_eq!(
            encode_key(KeyCode::UpArrow, Modifiers::NONE).as_deref(),
            Some("\x1b[A")
        );
        assert_eq!(
            encode_key(KeyCode::DownArrow, Modifiers::NONE).as_deref(),
            Some("\x1b[B")
        );
        assert_eq!(
            encode_key(KeyCode::RightArrow, Modifiers::NONE).as_deref(),
            Some("\x1b[C")
        );
        assert_eq!(
            encode_key(KeyCode::LeftArrow, Modifiers::NONE).as_deref(),
            Some("\x1b[D")
        );
        assert_eq!(
            encode_key(KeyCode::Home, Modifiers::NONE).as_deref(),
            Some("\x1b[H")
        );
        assert_eq!(
            encode_key(KeyCode::End, Modifiers::NONE).as_deref(),
            Some("\x1b[F")
        );
    }

    #[test]
    fn modified_arrows_use_xterm_modifier_parameters() {
        assert_eq!(
            encode_key(KeyCode::RightArrow, Modifiers::CTRL).as_deref(),
            Some("\x1b[1;5C")
        );
        assert_eq!(
            encode_key(KeyCode::LeftArrow, Modifiers::SHIFT).as_deref(),
            Some("\x1b[1;2D")
        );
        assert_eq!(
            encode_key(KeyCode::UpArrow, Modifiers::ALT).as_deref(),
            Some("\x1b[1;3A")
        );
        assert_eq!(
            encode_key(KeyCode::DownArrow, Modifiers::CTRL | Modifiers::SHIFT).as_deref(),
            Some("\x1b[1;6B")
        );
    }

    #[test]
    fn tilde_keys_encode_with_and_without_modifiers() {
        assert_eq!(
            encode_key(KeyCode::Insert, Modifiers::NONE).as_deref(),
            Some("\x1b[2~")
        );
        assert_eq!(
            encode_key(KeyCode::Delete, Modifiers::NONE).as_deref(),
            Some("\x1b[3~")
        );
        assert_eq!(
            encode_key(KeyCode::PageUp, Modifiers::NONE).as_deref(),
            Some("\x1b[5~")
        );
        assert_eq!(
            encode_key(KeyCode::PageDown, Modifiers::CTRL).as_deref(),
            Some("\x1b[6;5~")
        );
    }

    #[test]
    fn function_keys_split_between_ss3_and_tilde_forms() {
        assert_eq!(
            encode_key(KeyCode::Function(1), Modifiers::NONE).as_deref(),
            Some("\x1bOP")
        );
        assert_eq!(
            encode_key(KeyCode::Function(4), Modifiers::NONE).as_deref(),
            Some("\x1bOS")
        );
        assert_eq!(
            encode_key(KeyCode::Function(1), Modifiers::CTRL).as_deref(),
            Some("\x1b[1;5P")
        );
        assert_eq!(
            encode_key(KeyCode::Function(5), Modifiers::NONE).as_deref(),
            Some("\x1b[15~")
        );
        assert_eq!(
            encode_key(KeyCode::Function(12), Modifiers::NONE).as_deref(),
            Some("\x1b[24~")
        );
        assert_eq!(encode_key(KeyCode::Function(21), Modifiers::NONE), None);
    }

    #[test]
    fn modifier_keys_alone_produce_no_input() {
        for key in [
            KeyCode::Control,
            KeyCode::LeftControl,
            KeyCode::Shift,
            KeyCode::LeftShift,
            KeyCode::Alt,
            KeyCode::LeftAlt,
            KeyCode::Super,
            KeyCode::Meta,
            KeyCode::CapsLock,
        ] {
            assert_eq!(encode_key(key, Modifiers::NONE), None, "{key:?}");
        }
    }
}
