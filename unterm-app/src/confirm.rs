//! The banner that asks before an agent writes to a shell.
//!
//! Bytes written to a pty are indistinguishable from bytes the user typed, so
//! the MCP surface parks the writing thread until someone says yes. Someone
//! has to be asked, and only a front end can ask: a surface with no window
//! would leave that thread parked until it timed out, and the agent would see
//! a refusal it could not explain.
//!
//! Laid out here rather than in the window so the wording can be tested
//! without a GPU.

use termwiz::cell::unicode_column_width;

/// Grid columns the text occupies. Not `chars().count()`: once the labels are
/// translated, "允许" is two characters and four columns, and measuring in
/// characters lets a Chinese banner overrun the row it was fitted to -- which
/// it did, cutting `[Esc] 拒绝` in half.
fn columns_of(text: &str) -> usize {
    unicode_column_width(text, None)
}

/// Cut `text` to at most `max` columns, appending an ellipsis if anything was
/// dropped. Wide characters are never split down the middle.
fn cut_to_columns(text: &str, max: usize) -> String {
    if columns_of(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let budget = max.saturating_sub(1);
    let mut used = 0usize;
    let mut out = String::new();
    for ch in text.chars() {
        let mut buf = [0u8; 4];
        let width = unicode_column_width(ch.encode_utf8(&mut buf), None);
        if used + width > budget {
            break;
        }
        used += width;
        out.push(ch);
    }
    out.push('…');
    out
}

/// What the banner says, as the one status-bar row it takes over --
/// 0.57.4 painted this question in the status row, and so does this.
///
/// The preview is truncated rather than wrapped: a banner that grows with the
/// input can cover the screen it is asking about. The key hints give way last,
/// because they are the part that answers the question.
pub fn status_line(agent: &str, method: &str, preview: &str, cols: usize) -> String {
    use unterm_services::i18n::t;

    let width = cols.clamp(24, 240);
    let command = preview.trim();

    // The keys outrank everything: a question nobody can answer parks the
    // agent until it times out, which reads as a hang. They shrink to an
    // abbreviated form before they are allowed to crowd out the command.
    let keys_full = format!(
        "[Enter] {}   [A] {}   [Esc] {}",
        t("confirm.allow"),
        t("confirm.always_allow"),
        t("confirm.refuse")
    );
    let keys_tight = "[Enter]/[A]/[Esc]".to_string();
    let keys = if width >= columns_of(&keys_full) + 32 {
        keys_full
    } else {
        keys_tight
    };
    let room = width.saturating_sub(columns_of(&keys) + 3);

    // How much of the row the command is owed before an opener may use the
    // rest. Fixed at 28 this went off a cliff: a row with 29 columns to spare
    // handed all of them to the command and dropped *who was asking*, which
    // is the one fact that makes an unknown agent alarming. Scaling with the
    // room degrades instead -- full sentence, then "agent ▸ method", then
    // method alone, then the bare command.
    let wanted = columns_of(command).min((room * 3 / 5).max(12)).max(1);

    // Openers from most to least explanatory. Boilerplate is what gives way
    // when the row is tight; who is asking and what they want to run are the
    // last words to go.
    let openers = [
        format!(
            "{agent} {} {method}:  ",
            t("confirm.wants_to_run")
        ),
        format!("{agent} \u{25b8} {method}  "),
        format!("{method}  "),
        String::new(),
    ];
    let opener = openers
        .iter()
        .find(|opener| columns_of(opener) + wanted <= room)
        .cloned()
        .unwrap_or_default();

    let command = cut_to_columns(command, room.saturating_sub(columns_of(&opener)));

    let ask = format!("{opener}{command}");
    let line = if ask.trim().is_empty() {
        keys
    } else {
        format!("{ask}   {keys}")
    };
    cut_to_columns(&line, width)
}

/// Which decision a typed character means, if any.
///
/// Enter and Escape arrive as named keys and are handled beside this; the
/// letters exist so the 0.57.4 muscle memory and the labelled keys both work.
/// While the banner is up no key reaches the shell, so none of these can leak.
pub fn decision_for(text: &str) -> Option<unterm_mcp::handler::ConfirmationDecision> {
    use unterm_mcp::handler::ConfirmationDecision;
    match text {
        t if t.eq_ignore_ascii_case("y") => Some(ConfirmationDecision::Allow),
        t if t.eq_ignore_ascii_case("a") => Some(ConfirmationDecision::AlwaysAllow),
        _ => None,
    }
}

/// Which decision a mouse click at `column` (0-based monospace column from the
/// start of `line`) hits on the confirmation banner, if any.
pub fn decision_at_column(
    line: &str,
    column: usize,
) -> Option<unterm_mcp::handler::ConfirmationDecision> {
    use unterm_mcp::handler::ConfirmationDecision;
    let enter_byte = line.rfind("[Enter]")?;
    let a_byte = line[enter_byte..].find("[A]").map(|off| enter_byte + off)?;
    let esc_byte = line[a_byte..].find("[Esc]").map(|off| a_byte + off)?;

    let enter_col = columns_of(&line[..enter_byte]);
    let a_col = columns_of(&line[..a_byte]);
    let esc_col = columns_of(&line[..esc_byte]);
    let end_col = columns_of(line);

    if column >= esc_col && column <= end_col {
        Some(ConfirmationDecision::Block)
    } else if column >= a_col && column < esc_col {
        Some(ConfirmationDecision::AlwaysAllow)
    } else if column >= enter_col && column < a_col {
        Some(ConfirmationDecision::Allow)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the preview the way the MCP surface does, so these tests see the
    /// string the banner is actually handed. Passing a bare command here once
    /// hid a defect for a whole release cycle.
    fn preview(command: &str) -> String {
        unterm_mcp::handler::input_preview(command)
    }

    #[test]
    fn the_banner_says_who_is_asking_and_for_what() {
        let line = status_line("claude", "session.input", &preview("rm -rf /"), 200);
        assert!(line.contains("claude"));
        assert!(line.contains("session.input"));
        assert!(line.contains("rm -rf /"));
    }

    #[test]
    fn the_command_survives_a_normal_window() {
        // The whole point of the question. At the width a real window gives
        // this row, a short command must be readable in full -- approving
        // `exec.run: len=22…` is approving nothing.
        for cols in [80, 100, 120, 160] {
            let line = status_line("claude", "exec.run", &preview("echo hello"), cols);
            assert!(
                line.contains("echo hello"),
                "the command was cut at {cols} cols: {line}"
            );
        }
    }

    #[test]
    fn the_byte_count_gives_way_before_the_command_does() {
        let long = format!("git push --force origin {}", "b".repeat(60));
        let line = status_line("claude", "exec.run", &preview(&long), 100);
        assert!(
            line.contains("git push --force"),
            "the dangerous part was cut: {line}"
        );
        assert!(
            !line.contains("bytes)"),
            "the byte count outlived the command it describes: {line}"
        );
    }

    #[test]
    fn hidden_control_bytes_are_announced_even_when_the_row_is_tight() {
        // Text that looks harmless but carries an embedded CR can run a
        // second command. The warning has to outlive truncation.
        let line = status_line("claude", "session.input", &preview("ls\rrm -rf /"), 60);
        assert!(line.contains("[ctrl]"), "the control-byte warning was cut: {line}");
    }

    #[test]
    fn every_option_is_offered() {
        let line = status_line("claude", "session.input", "ls", 200);
        assert!(line.contains("[Enter]"));
        assert!(line.contains("[A]"));
        assert!(line.contains("[Esc]"));
    }

    #[test]
    fn clicking_banner_buttons_resolves_to_matching_decisions() {
        use unterm_mcp::handler::ConfirmationDecision;
        let line = status_line("claude", "exec.run_wait", "rg --files", 120);
        let enter_col = columns_of(&line[..line.rfind("[Enter]").unwrap()]);
        let a_col = columns_of(&line[..line.rfind("[A]").unwrap()]);
        let esc_col = columns_of(&line[..line.rfind("[Esc]").unwrap()]);
        assert!(matches!(
            decision_at_column(&line, enter_col + 1),
            Some(ConfirmationDecision::Allow)
        ));
        assert!(matches!(
            decision_at_column(&line, a_col + 1),
            Some(ConfirmationDecision::AlwaysAllow)
        ));
        assert!(matches!(
            decision_at_column(&line, esc_col + 1),
            Some(ConfirmationDecision::Block)
        ));
        assert!(decision_at_column(&line, 0).is_none());
    }

    #[test]
    fn a_long_command_is_cut_rather_than_allowed_to_cover_the_screen() {
        let long = "echo ".to_string() + &"x".repeat(500);
        let line = status_line("claude", "session.input", &long, 100);
        assert!(
            line.chars().count() <= 100,
            "banner line ran past the window: {line}"
        );
        assert!(line.contains("[Esc]"), "the keys gave way: {line}");
    }

    #[test]
    fn a_narrow_window_still_gets_a_readable_banner() {
        // Below the floor the text would be cut to nothing, which is worse
        // than a banner that runs a little wide.
        let line = status_line("claude", "session.input", "ls", 4);
        assert!(line.chars().count() <= 24);
        assert!(!line.is_empty());
    }

    #[test]
    fn who_is_asking_survives_a_normal_window() {
        // An unknown agent asking is the case the banner exists for. Losing
        // the name to make room for ten more characters of command trades
        // away the alarming half of the question.
        let line = status_line("suspicious-agent", "exec.run", &preview("ls"), 140);
        assert!(line.contains("suspicious-agent"), "who was asking got cut: {line}");
        assert!(line.contains("ls"), "the command got cut: {line}");
    }

    #[test]
    fn a_tight_row_keeps_the_method_rather_than_nothing() {
        let long = format!("git push --force origin {}", "b".repeat(40));
        let line = status_line("claude", "exec.run", &preview(&long), 90);
        assert!(line.contains("exec.run"), "degraded straight past the method: {line}");
        assert!(line.contains("git push --force"), "the command got cut: {line}");
    }

    #[test]
    fn a_translated_banner_still_fits_the_row() {
        // Measured in characters, "[Enter] 允许   [A] 始终允许   [Esc] 拒绝"
        // looks shorter than its English original and is in fact wider. The
        // row it was fitted to then overflowed, and the last thing to fall
        // off the edge was the key that refuses.
        // Transient, never `set_locale`: that one writes the choice to disk
        // and a test has no business changing what the user sees next launch.
        let restore = unterm_services::i18n::current_locale();
        for locale in ["zh-CN", "ja-JP", "ko-KR", "en-US"] {
            unterm_services::i18n::set_locale_transient(locale);
            for cols in [80, 100, 120, 160] {
                let line = status_line("claude", "exec.run", &preview("echo hello"), cols);
                assert!(
                    columns_of(&line) <= cols,
                    "{locale} at {cols} cols overran to {}: {line}",
                    columns_of(&line)
                );
            }
        }
        unterm_services::i18n::set_locale_transient(&restore);
    }

    #[test]
    fn only_the_offered_keys_decide() {
        use unterm_mcp::handler::ConfirmationDecision;
        assert!(matches!(
            decision_for("y"),
            Some(ConfirmationDecision::Allow)
        ));
        assert!(matches!(
            decision_for("Y"),
            Some(ConfirmationDecision::Allow)
        ));
        assert!(matches!(
            decision_for("a"),
            Some(ConfirmationDecision::AlwaysAllow)
        ));
        assert!(decision_for("n").is_none(), "refusing is Esc, not a letter");
        assert!(decision_for("").is_none());
    }
}
