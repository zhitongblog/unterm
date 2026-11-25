pub(super) fn decode_pty_chunk(pending: &mut Vec<u8>, bytes: &[u8]) -> Option<String> {
    pending.extend_from_slice(bytes);
    match std::str::from_utf8(pending.as_slice()) {
        Ok(text) => {
            let text = text.to_string();
            pending.clear();
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }
        Err(err) if err.error_len().is_none() => {
            let valid_up_to = err.valid_up_to();
            if valid_up_to == 0 {
                return None;
            }
            let text = String::from_utf8(pending[..valid_up_to].to_vec()).ok()?;
            pending.drain(..valid_up_to);
            Some(text)
        }
        Err(_) => {
            let text = String::from_utf8_lossy(pending.as_slice()).to_string();
            pending.clear();
            Some(text)
        }
    }
}

#[derive(Default)]
pub(super) struct StartupOutputFilter {
    buffer: String,
    done: bool,
    suppressing_powershell_warning: bool,
}

impl StartupOutputFilter {
    pub(super) fn filter(&mut self, chunk: String) -> Option<String> {
        if self.done {
            return non_empty(chunk);
        }

        self.buffer.push_str(&chunk);
        if self.suppressing_powershell_warning {
            if let Some(prompt) = first_powershell_prompt(&self.buffer) {
                self.buffer.clear();
                self.done = true;
                return Some(format!("\x1b[H{prompt}"));
            }
            if self.buffer.len() <= 8192 {
                return None;
            }
            self.done = true;
            return non_empty(
                strip_powershell_startup_noise(&std::mem::take(&mut self.buffer)).into_owned(),
            );
        }
        if startup_buffer_is_blank(&self.buffer) {
            return None;
        }
        if startup_chunk_is_control_only(&self.buffer) {
            return non_empty(std::mem::take(&mut self.buffer));
        }
        if let Some(filtered) = remove_powershell_startup_warning(&self.buffer) {
            self.suppressing_powershell_warning = true;
            if let Some(prompt) = first_powershell_prompt(&filtered) {
                self.buffer.clear();
                self.done = true;
                return Some(format!("\x1b[H{prompt}"));
            }
            self.buffer.clear();
            return None;
        }

        if may_be_split_powershell_warning(&self.buffer) {
            return None;
        }

        self.done = true;
        non_empty(trim_blank_prefix_before_prompt(std::mem::take(&mut self.buffer)))
    }
}

fn non_empty(text: String) -> Option<String> {
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

const POWERSHELL_SCREEN_READER_WARNINGS: &[&str] = &[
    "WARNING: PowerShell detected that you might be using a screen reader and has disabled PSReadLine for compatibility purposes. If you want to re-enable it, run 'Import-Module PSReadLine'.",
];

fn remove_powershell_startup_warning(text: &str) -> Option<String> {
    for warning in POWERSHELL_SCREEN_READER_WARNINGS {
        if let Some(start) = text.find(warning) {
            let mut end = start + warning.len();
            if text[end..].starts_with("\r\n") {
                end += 2;
            } else if text[end..].starts_with('\n') {
                end += 1;
            }
            let mut filtered = text.to_string();
            filtered.replace_range(start..end, "");
            return Some(trim_startup_blank_prefix(filtered));
        }
    }
    for marker in ["Import-Module PSReadLine", "ort-Module PSReadLine"] {
        if let Some(marker_start) = text.find(marker) {
            let line_start = text[..marker_start]
                .rfind('\n')
                .map(|idx| idx + 1)
                .unwrap_or(0);
            let line_end = text[marker_start..]
                .find('\n')
                .map(|idx| marker_start + idx + 1)
                .unwrap_or(text.len());
            let mut filtered = text.to_string();
            filtered.replace_range(line_start..line_end, "");
            return Some(trim_startup_blank_prefix(filtered));
        }
    }
    None
}

fn trim_startup_blank_prefix(mut text: String) -> String {
    while text.starts_with("\r\n") {
        text.drain(..2);
    }
    while text.starts_with('\n') || text.starts_with('\r') {
        text.drain(..1);
    }
    text
}

fn startup_buffer_is_blank(text: &str) -> bool {
    text.len() <= 64 && text.chars().all(|ch| ch == '\r' || ch == '\n')
}

fn startup_chunk_is_control_only(text: &str) -> bool {
    text.len() <= 4096
        && text.starts_with('\x1b')
        && !text.contains("PS ")
        && !text.contains("PSReadLine")
}

fn trim_blank_prefix_before_prompt(text: String) -> String {
    let trimmed = trim_startup_blank_prefix(text.clone());
    if trimmed.starts_with("PS ") {
        trimmed
    } else {
        text
    }
}

fn first_powershell_prompt(text: &str) -> Option<String> {
    let start = text.find("PS ")?;
    let rest = &text[start..];
    let mut end = rest.find('>')? + 1;
    if rest[end..].starts_with(' ') {
        end += 1;
    }
    Some(rest[..end].to_string())
}

fn may_be_split_powershell_warning(text: &str) -> bool {
    const MAX_STARTUP_WARNING_BYTES: usize = 512;
    if text.len() > MAX_STARTUP_WARNING_BYTES {
        return false;
    }
    POWERSHELL_SCREEN_READER_WARNINGS
        .iter()
        .any(|warning| warning_prefix_is_at_end(text, warning))
}

fn warning_prefix_is_at_end(text: &str, warning: &str) -> bool {
    warning
        .char_indices()
        .skip(1)
        .any(|(idx, _)| text.ends_with(&warning[..idx]))
}

/// Append to a buffer that keeps roughly the last `max_bytes` of output.
///
/// "Roughly" is the point. Trimming a `String` from the front memmoves
/// everything that stays, so trimming on every chunk made this 1 MiB buffer
/// the most expensive thing in the entire output path -- on a 400k-line
/// flood, 71% of the PTY reader thread's samples were that one memmove.
/// Letting the buffer overshoot by a quarter and then cutting back to
/// `max_bytes` turns one memmove per chunk into one per quarter-buffer, for a
/// bounded amount of extra memory per session.
///
/// Returns how many bytes were cut from the front (0 almost always).
pub(super) fn append_bounded_output(output: &mut String, chunk: &str, max_bytes: usize) -> usize {
    output.push_str(chunk);
    if output.len() > trim_threshold(max_bytes) {
        return trim_to_utf8_boundary(output, max_bytes);
    }
    0
}

/// What a session printed, as it arrived, with the count of bytes already
/// cut from the front: `dropped + text.len()` is everything it ever printed,
/// so a byte offset into that whole stream stays valid across trims. A viewer
/// that replays the output in an emulator asks "from byte N" and gets exactly
/// the bytes it has not seen -- carriage returns, trailing spaces and all.
#[derive(Debug, Default)]
pub(super) struct OutputLog {
    pub(super) text: String,
    pub(super) dropped: u64,
}

impl OutputLog {
    pub(super) fn append(&mut self, chunk: &str, max_bytes: usize) {
        self.dropped += append_bounded_output(&mut self.text, chunk, max_bytes) as u64;
    }

    /// Offset (in the whole stream) just past the last byte printed.
    pub(super) fn end(&self) -> u64 {
        self.dropped + self.text.len() as u64
    }

    /// The bytes from `from` on, at most `max_bytes` of the newest, starting
    /// on a char boundary; with the offset the returned text starts at. Older
    /// than what is kept starts at the oldest kept byte instead.
    pub(super) fn since(&self, from: Option<u64>, max_bytes: usize) -> (u64, &str) {
        let end = self.end();
        let wanted = from.unwrap_or(0).clamp(self.dropped, end);
        let mut start = (wanted - self.dropped) as usize;
        if self.text.len() - start > max_bytes {
            start = self.text.len() - max_bytes;
        }
        while !self.text.is_char_boundary(start) {
            start += 1;
        }
        (self.dropped + start as u64, &self.text[start..])
    }
}

impl std::ops::Deref for OutputLog {
    type Target = String;
    fn deref(&self) -> &String {
        &self.text
    }
}

/// The size the buffer has to reach before a trim is worth its memmove.
fn trim_threshold(max_bytes: usize) -> usize {
    max_bytes.saturating_add((max_bytes / 4).max(1))
}

/// Strip PowerShell's screen-reader warning if this chunk carries it.
///
/// Every output chunk of every session, on every platform, passes through
/// here for the whole life of the session -- so the common answer, "there is
/// nothing to strip", has to be cheap. Borrowing instead of copying saves an
/// allocation and a copy of the chunk, and the `PSReadLine` guard rules the
/// chunk out in one scan instead of three: every pattern below contains it.
pub(super) fn strip_powershell_startup_noise(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains("PSReadLine") {
        return std::borrow::Cow::Borrowed(text);
    }
    match remove_powershell_startup_warning(text) {
        Some(filtered) => std::borrow::Cow::Owned(filtered),
        None => std::borrow::Cow::Borrowed(text),
    }
}

pub(super) fn trim_to_utf8_boundary(text: &mut String, max_bytes: usize) -> usize {
    if text.len() <= max_bytes {
        return 0;
    }
    // Walk forward from the cut to the next char boundary -- three bytes at
    // most. The scan this replaces started at the front and decoded every
    // char up to the cut, which on a megabyte buffer means reading the whole
    // buffer to find something that is never more than three bytes away.
    let mut keep_from = text.len() - max_bytes;
    while !text.is_char_boundary(keep_from) {
        keep_from += 1;
    }
    text.drain(..keep_from);
    keep_from
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_log_offsets_survive_trims() {
        let mut log = OutputLog::default();
        log.append("abcd", 4);
        assert_eq!(log.since(None, 100), (0, "abcd"));
        // Past four plus slack: trims to the newest four, two bytes dropped.
        log.append("\r\nx", 4);
        assert_eq!(log.text, "d\r\nx");
        assert_eq!(log.dropped, 3);
        assert_eq!(log.end(), 7);
        // Exactly what came after byte 5, carriage return and all.
        assert_eq!(log.since(Some(5), 100), (5, "\nx"));
        // Older than what is kept: from the oldest kept byte.
        assert_eq!(log.since(Some(1), 100), (3, "d\r\nx"));
        // Caught up: nothing.
        assert_eq!(log.since(Some(7), 100), (7, ""));
        // A cap keeps the newest bytes.
        assert_eq!(log.since(Some(3), 2), (5, "\nx"));
    }

    #[test]
    fn output_log_starts_on_a_char_boundary() {
        let mut log = OutputLog::default();
        log.append("a\u{4f60}b", 100);
        // Byte 2 is inside the three-byte character: start after it.
        assert_eq!(log.since(Some(2), 100), (4, "b"));
    }

    #[test]
    fn waits_for_incomplete_utf8_sequence() {
        let mut pending = Vec::new();
        let bytes = "\u{4f60}".as_bytes();

        assert_eq!(decode_pty_chunk(&mut pending, &bytes[..1]), None);
        assert_eq!(decode_pty_chunk(&mut pending, &bytes[1..2]), None);
        assert_eq!(
            decode_pty_chunk(&mut pending, &bytes[2..]),
            Some("\u{4f60}".to_string())
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn emits_valid_prefix_before_incomplete_suffix() {
        let mut pending = Vec::new();
        let mut bytes = b"ok ".to_vec();
        bytes.extend_from_slice(&"\u{4f60}".as_bytes()[..1]);

        assert_eq!(
            decode_pty_chunk(&mut pending, &bytes),
            Some("ok ".to_string())
        );
        assert_eq!(pending, vec!["\u{4f60}".as_bytes()[0]]);
    }

    #[test]
    fn replaces_invalid_utf8_and_clears_pending() {
        let mut pending = Vec::new();

        assert_eq!(
            decode_pty_chunk(&mut pending, &[0xff, b'a']),
            Some("\u{fffd}a".to_string())
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn bounded_output_keeps_the_newest_text_once_it_trims() {
        let mut output = String::from("abcdefg");

        // Seven bytes is past four plus its quarter of slack, so this append
        // is the one that trims.
        append_bounded_output(&mut output, "h", 4);

        assert_eq!(output, "efgh");
    }

    #[test]
    fn bounded_output_tolerates_slack_before_trimming() {
        let mut output = String::from("abcd");

        append_bounded_output(&mut output, "e", 4);

        assert_eq!(output, "abcde");
    }

    #[test]
    fn bounded_output_stays_bounded_across_many_appends() {
        let mut output = String::new();
        for _ in 0..1000 {
            append_bounded_output(&mut output, "0123456789", 64);
        }

        assert!(
            output.len() <= 64 + 64 / 4 + "0123456789".len(),
            "buffer grew to {} bytes",
            output.len()
        );
        assert!(output.ends_with("0123456789"));
    }

    #[test]
    fn bounded_output_does_not_split_utf8() {
        let mut output = String::from("ab\u{4f60}\u{597d}");

        append_bounded_output(&mut output, "cd", 6);

        assert_eq!(output, "\u{597d}cd");
        assert!(std::str::from_utf8(output.as_bytes()).is_ok());
    }

    #[test]
    fn startup_filter_removes_split_powershell_warning() {
        let mut filter = StartupOutputFilter::default();

        assert_eq!(
            filter.filter("WARNING: PowerShell detected that you might be using ".to_string()),
            None
        );
        let output = filter
            .filter("a screen reader and has disabled PSReadLine for compatibility purposes. If you want to re-enable it, run 'Import-Module PSReadLine'.\r\nPS C:\\Users\\Alex> ".to_string())
            .unwrap();

        assert_eq!(output, "\u{1b}[HPS C:\\Users\\Alex> ");
    }

    #[test]
    fn startup_filter_removes_split_warning_tail() {
        let mut filter = StartupOutputFilter::default();

        let output = filter
            .filter("ort-Module PSReadLine\"銆俓r\nPS C:\\Users\\Alex> ".to_string())
            .unwrap();

        assert_eq!(output, "\u{1b}[HPS C:\\Users\\Alex> ");
    }

    #[test]
    fn strips_powershell_warning_tail_from_any_chunk() {
        assert_eq!(
            strip_powershell_startup_noise("ort-Module PSReadLine\"銆俓r\nPS C:\\Users\\Alex> "),
            "PS C:\\Users\\Alex> "
        );
    }

    #[test]
    fn startup_filter_drops_blank_lines_left_by_warning_removal() {
        assert_eq!(
            strip_powershell_startup_noise(
                "\r\n\r\n\r\nort-Module PSReadLine\"\r\nPS C:\\Users\\Alex> "
            ),
            "PS C:\\Users\\Alex> "
        );
    }

    #[test]
    fn startup_filter_waits_on_initial_blank_lines_before_prompt() {
        let mut filter = StartupOutputFilter::default();

        assert_eq!(filter.filter("\r\n\r\n\r\n".to_string()), None);
        assert_eq!(
            filter.filter("PS C:\\Users\\Alex> ".to_string()).unwrap(),
            "PS C:\\Users\\Alex> "
        );
    }

    #[test]
    fn startup_filter_passes_normal_output() {
        let mut filter = StartupOutputFilter::default();

        assert_eq!(filter.filter("PS C:\\Users\\Alex> ".to_string()).unwrap(), "PS C:\\Users\\Alex> ");
        assert_eq!(filter.filter("echo ok\r\n".to_string()).unwrap(), "echo ok\r\n");
    }
}
