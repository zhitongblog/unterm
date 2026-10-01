#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum OscCommand {
    Title(String),
    CurrentDir(String),
    Hyperlink(Option<String>),
    /// `OSC 52`: text a program wants on the system clipboard, decoded.
    ///
    /// How tmux copies out of a remote session, and the only way anything
    /// over ssh reaches the clipboard at all.
    Clipboard(String),
    /// `OSC 133;A`: the shell has started drawing a new prompt.
    PromptStart,
    /// `OSC 133;B`: the prompt is drawn; what follows is the user's input.
    InputStart,
    /// `OSC 133;C`: the command line was accepted and is running.
    CommandStart,
    /// `OSC 133;D[;status]`: the command finished.
    CommandEnd(Option<i32>),
    /// `OSC 9` / `OSC 777;notify`: a program asking for the user's eye.
    Notification(String),
    /// `OSC 9;4;<state>;<percent>`: progress, or `None` when it is cleared.
    Progress(Option<crate::TerminalProgress>),
}

pub(super) fn parse(sequence: &str) -> Option<OscCommand> {
    let (kind, value) = sequence.split_once(';')?;
    match kind {
        "0" | "2" if !value.is_empty() => Some(OscCommand::Title(value.to_string())),
        "7" => parse_osc7_cwd(value).map(OscCommand::CurrentDir),
        "8" => parse_osc8_hyperlink(value).map(OscCommand::Hyperlink),
        "52" => parse_osc52_clipboard(value).map(OscCommand::Clipboard),
        "133" => {
            let mut parts = value.split(';');
            match parts.next()? {
                "A" => Some(OscCommand::PromptStart),
                "B" => Some(OscCommand::InputStart),
                "C" => Some(OscCommand::CommandStart),
                "D" => Some(OscCommand::CommandEnd(
                    parts.next().and_then(|code| code.trim().parse().ok()),
                )),
                _ => None,
            }
        }
        "9" if value.starts_with("4;") || value == "4" => {
            parse_osc9_progress(value).map(OscCommand::Progress)
        }
        // `9;9;<path>` is ConEmu's working directory, and the other numbered
        // forms are its subcommands too: not text for the user's eye.
        "9" if !value.is_empty() && !is_conemu_command(value) => {
            Some(OscCommand::Notification(value.to_string()))
        }
        "777" => parse_osc777_notification(value).map(OscCommand::Notification),
        _ => None,
    }
}

/// `4;<state>;<percent>`. State 0 clears; 1 normal, 2 error, 3
/// indeterminate, 4 paused. A missing or oversized percent is read the way
/// Windows Terminal reads it: absent is 0, anything above 100 is 100.
fn parse_osc9_progress(value: &str) -> Option<Option<crate::TerminalProgress>> {
    use crate::{ProgressState, TerminalProgress};
    let mut parts = value.split(';').skip(1);
    let state = parts.next().unwrap_or("0");
    let percent = parts
        .next()
        .and_then(|text| text.trim().parse::<u32>().ok())
        .unwrap_or(0)
        .min(100) as u8;
    let state = match state.trim() {
        "" | "0" => return Some(None),
        "1" => ProgressState::Normal,
        "2" => ProgressState::Error,
        "3" => ProgressState::Indeterminate,
        "4" => ProgressState::Paused,
        _ => return None,
    };
    Some(Some(TerminalProgress { state, percent }))
}

fn is_conemu_command(value: &str) -> bool {
    let first = value.split(';').next().unwrap_or("");
    !first.is_empty() && first.bytes().all(|b| b.is_ascii_digit())
}

/// `OSC 52 ; <selection> ; <base64>`. A `?` payload -- read the clipboard --
/// is not base64 and is refused with everything else that is not.
fn parse_osc52_clipboard(value: &str) -> Option<String> {
    use base64::Engine as _;
    let (_selection, payload) = value.split_once(';')?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()?;
    String::from_utf8(decoded).ok()
}

/// `OSC 777 ; notify ; <title> ; <body>` — the urxvt form tmux and
/// systemd-run emit. Anything other than `notify` is some other extension.
fn parse_osc777_notification(value: &str) -> Option<String> {
    let mut parts = value.splitn(3, ';');
    if parts.next() != Some("notify") {
        return None;
    }
    let title = parts.next().unwrap_or("").trim();
    let body = parts.next().unwrap_or("").trim();
    match (title.is_empty(), body.is_empty()) {
        (true, true) => None,
        (false, true) => Some(title.to_string()),
        (true, false) => Some(body.to_string()),
        (false, false) => Some(format!("{title}: {body}")),
    }
}

fn parse_osc8_hyperlink(value: &str) -> Option<Option<String>> {
    let (_params, uri) = value.split_once(';')?;
    if uri.is_empty() {
        Some(None)
    } else {
        Some(Some(uri.to_string()))
    }
}

fn parse_osc7_cwd(value: &str) -> Option<String> {
    let uri = value.strip_prefix("file://")?;
    let path = if uri.starts_with('/') {
        uri
    } else {
        let slash = uri.find('/')?;
        &uri[slash..]
    };
    let decoded = percent_decode(path)?;
    if decoded.is_empty() {
        return None;
    }
    #[cfg(windows)]
    {
        let mut path = decoded;
        let bytes = path.as_bytes();
        if path.starts_with('/')
            && bytes.len() >= 4
            && bytes[2] == b':'
            && bytes[1].is_ascii_alphabetic()
        {
            path.remove(0);
        }
        Some(path.replace('/', "\\"))
    }
    #[cfg(not(windows))]
    {
        Some(decoded)
    }
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'%' {
            let hi = *bytes.get(idx + 1)?;
            let lo = *bytes.get(idx + 2)?;
            out.push(hex_value(hi)? << 4 | hex_value(lo)?);
            idx += 3;
        } else {
            out.push(bytes[idx]);
            idx += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc133_marks_every_stage_of_a_command() {
        assert_eq!(parse("133;A"), Some(OscCommand::PromptStart));
        assert_eq!(parse("133;A;cl=m"), Some(OscCommand::PromptStart));
        assert_eq!(parse("133;B"), Some(OscCommand::InputStart));
        assert_eq!(parse("133;C"), Some(OscCommand::CommandStart));
        assert_eq!(parse("133;D;2"), Some(OscCommand::CommandEnd(Some(2))));
        assert_eq!(parse("133;D"), Some(OscCommand::CommandEnd(None)));
        assert_eq!(parse("133;Z"), None);
    }

    /// `OSC 9;4` is progress, not a notification: Windows Terminal's
    /// reading of each state, and `0` clears.
    #[test]
    fn osc9_4_is_progress() {
        use crate::{ProgressState, TerminalProgress};
        assert_eq!(
            parse("9;4;1;60"),
            Some(OscCommand::Progress(Some(TerminalProgress {
                state: ProgressState::Normal,
                percent: 60
            })))
        );
        assert_eq!(
            parse("9;4;2;250"),
            Some(OscCommand::Progress(Some(TerminalProgress {
                state: ProgressState::Error,
                percent: 100
            })))
        );
        assert_eq!(
            parse("9;4;3"),
            Some(OscCommand::Progress(Some(TerminalProgress {
                state: ProgressState::Indeterminate,
                percent: 0
            })))
        );
        assert_eq!(parse("9;4;0;0"), Some(OscCommand::Progress(None)));
        assert_eq!(parse("9;4"), Some(OscCommand::Progress(None)));
        assert_eq!(parse("9;4;9;10"), None);
        // Plain text is still a notification.
        assert_eq!(
            parse("9;build finished"),
            Some(OscCommand::Notification("build finished".into()))
        );
    }


    #[test]
    fn parses_title_updates() {
        assert_eq!(
            parse("0;Codex Pane"),
            Some(OscCommand::Title("Codex Pane".to_string()))
        );
        assert_eq!(
            parse("2;Tab Title"),
            Some(OscCommand::Title("Tab Title".to_string()))
        );
        assert_eq!(parse("2;"), None);
    }

    #[test]
    fn parses_osc8_hyperlink_start_and_end() {
        assert_eq!(
            parse("8;id=1;https://example.com"),
            Some(OscCommand::Hyperlink(Some(
                "https://example.com".to_string()
            )))
        );
        assert_eq!(parse("8;;"), Some(OscCommand::Hyperlink(None)));
    }

    #[test]
    fn parses_shell_prompt_markers_and_ignores_unknown_zones() {
        assert_eq!(parse("133;A"), Some(OscCommand::PromptStart));
        assert_eq!(parse("133;A;extra"), Some(OscCommand::PromptStart));
        // B, C and D are read now that Unterm's own integration sends them;
        // a zone nobody defined is still not guessed at.
        assert_eq!(parse("133;B"), Some(OscCommand::InputStart));
        assert_eq!(parse("133;P;k=i"), None);
    }

    #[test]
    fn rejects_invalid_percent_encoded_cwd() {
        assert_eq!(parse("7;file://localhost/tmp/%zz"), None);
    }

    #[test]
    fn parses_osc7_cwd_with_host() {
        let parsed = parse("7;file://localhost/tmp/my%20project");
        #[cfg(windows)]
        assert_eq!(
            parsed,
            Some(OscCommand::CurrentDir("\\tmp\\my project".to_string()))
        );
        #[cfg(not(windows))]
        assert_eq!(
            parsed,
            Some(OscCommand::CurrentDir("/tmp/my project".to_string()))
        );
    }
}

#[cfg(test)]
mod clipboard_tests {
    use super::*;

    #[test]
    fn a_program_can_put_text_on_the_clipboard() {
        // What tmux sends to copy a selection out of a remote session.
        assert_eq!(
            parse("52;c;aGVsbG8="),
            Some(OscCommand::Clipboard("hello".to_string()))
        );
    }

    #[test]
    fn every_selection_name_is_accepted() {
        // c, p, s, and the combinations programs actually send.
        for selection in ["c", "p", "s", "cp", ""] {
            assert!(
                parse(&format!("52;{selection};aGk=")).is_some(),
                "selection {:?} should still be a clipboard write",
                selection
            );
        }
    }

    #[test]
    fn reading_the_clipboard_is_refused() {
        // `?` asks the terminal to report what the user has copied. Handing
        // that to any program that can print is how a terminal leaks
        // passwords, and every terminal that has thought about it refuses.
        assert_eq!(parse("52;c;?"), None);
    }

    #[test]
    fn a_payload_that_is_not_base64_is_ignored_rather_than_guessed() {
        assert_eq!(parse("52;c;not base64!!"), None);
    }

    #[test]
    fn text_that_is_not_utf8_is_ignored() {
        // 0xff is not a valid UTF-8 start byte.
        assert_eq!(parse("52;c;/w=="), None);
    }

    #[test]
    fn padding_is_handled_whichever_length_the_text_is() {
        assert_eq!(
            parse("52;c;YQ=="),
            Some(OscCommand::Clipboard("a".to_string()))
        );
        assert_eq!(
            parse("52;c;YWI="),
            Some(OscCommand::Clipboard("ab".to_string()))
        );
        assert_eq!(
            parse("52;c;YWJj"),
            Some(OscCommand::Clipboard("abc".to_string()))
        );
    }

    #[test]
    fn a_multi_line_payload_survives_the_newlines_in_it() {
        assert_eq!(
            parse("52;c;bGluZSAxCmxpbmUgMg=="),
            Some(OscCommand::Clipboard("line 1\nline 2".to_string()))
        );
    }
}

#[cfg(test)]
mod conemu_tests {
    use super::{parse, OscCommand};

    #[test]
    fn a_progress_report_is_not_a_notification() {
        // Progress is read as progress now, and still never as text for the
        // user's eye.
        assert!(matches!(parse("9;4;1;60"), Some(OscCommand::Progress(Some(_)))));
        assert_eq!(parse("9;4;0"), Some(OscCommand::Progress(None)));
        assert_eq!(parse("9;9;C:\\work"), None);
        assert_eq!(
            parse("9;Build finished"),
            Some(OscCommand::Notification("Build finished".into()))
        );
    }
}
