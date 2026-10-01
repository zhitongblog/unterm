//! "Report on GitHub" for a crash.
//!
//! A crash used to end in a dialog and a line in `panic.log` that nobody but
//! the person who crashed would ever see. Unterm has no server to send
//! anything to, and should not quietly send anything anywhere; so the dialog
//! offers to open a new GitHub issue with the facts filled in -- version,
//! platform, the message and the last lines of `panic.log` -- for the person
//! to read, edit and submit themselves. Nothing leaves the machine unless they
//! press Submit on that page.

const ISSUES: &str = "https://github.com/zhitongblog/unterm/issues/new";
/// Browsers and GitHub both cope with long URLs, but not unboundedly.
const MAX_BODY: usize = 5_000;

fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The last `lines` lines of `panic.log`, if there is one.
fn recent_log(lines: usize) -> String {
    let Some(path) = unterm_protocol::state_path("panic.log") else {
        return String::new();
    };
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// A new-issue URL with the crash written into it.
pub fn issue_url(message: &str) -> String {
    let first_line = message.lines().next().unwrap_or("crash");
    let mut title: String = first_line.chars().take(90).collect();
    if title.len() < first_line.len() {
        title.push('…');
    }
    let mut body = format!(
        "Unterm {} on {} {}\n\n**What happened**\n\n```\n{}\n```\n\n**Recent panic.log**\n\n```\n{}\n```\n\n**What I was doing**\n\n",
        unterm_protocol::PRODUCT_VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH,
        message.trim(),
        recent_log(20),
    );
    if body.len() > MAX_BODY {
        let mut cut = MAX_BODY;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
        body.push_str("\n…(trimmed)\n```\n");
    }
    format!(
        "{ISSUES}?labels=bug&title={}&body={}",
        encode(&format!("[Crash] {title}")),
        encode(&body)
    )
}

fn open(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).status();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).status();
    #[cfg(windows)]
    unsafe {
        use std::os::windows::ffi::OsStrExt;
        let wide = |s: &str| -> Vec<u16> { std::ffi::OsStr::new(s).encode_wide().chain([0]).collect() };
        winapi::um::shellapi::ShellExecuteW(
            std::ptr::null_mut(),
            wide("open").as_ptr(),
            wide(url).as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            winapi::um::winuser::SW_SHOWNORMAL,
        );
    }
}

/// Tell the person what happened, and offer to report it.
pub fn offer(message: &str) {
    let url = issue_url(message);
    // Nobody to answer a dialog on a CI runner or a display-less session,
    // and a crash must not become a hang: say where to report, and go.
    let unattended = std::env::var_os("CI").is_some()
        || std::env::var_os("UNTERM_NO_CRASH_DIALOG").is_some()
        || (cfg!(all(unix, not(target_os = "macos")))
            && std::env::var_os("DISPLAY").is_none()
            && std::env::var_os("WAYLAND_DISPLAY").is_none());
    if unattended {
        eprintln!("{message}\nReport it at: {url}");
        return;
    }
    let question = format!(
        "{message}\n\nReport this on GitHub? A new issue opens in your browser with these details filled in; nothing is sent until you submit it."
    );
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
        }
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        const MB_ICONERROR: u32 = 0x10;
        const MB_YESNO: u32 = 0x04;
        const IDYES: i32 = 6;
        let answer = unsafe {
            MessageBoxW(0, wide(&question).as_ptr(), wide("Unterm").as_ptr(), MB_ICONERROR | MB_YESNO)
        };
        if answer == IDYES {
            open(&url);
        }
    }
    #[cfg(target_os = "macos")]
    {
        let quoted = question.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "display dialog \"{quoted}\" with title \"Unterm\" with icon stop buttons {{\"Close\", \"Report on GitHub\"}} default button \"Report on GitHub\" giving up after 120"
        );
        let answered = std::process::Command::new("osascript")
            .arg("-e")
            .arg(script)
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).contains("Report on GitHub"))
            .unwrap_or(false);
        if answered {
            open(&url);
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let asked = std::process::Command::new("zenity")
            .args(["--question", "--title=Unterm", "--ok-label=Report on GitHub", "--cancel-label=Close", "--timeout=120"])
            .arg(format!("--text={question}"))
            .status();
        match asked {
            Ok(status) if status.success() => open(&url),
            Ok(_) => {}
            Err(_) => eprintln!("Unterm crashed. Report it at: {url}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_carries_the_facts_and_stays_a_valid_url() {
        let url = issue_url("unterm panicked: index out of bounds\nat window.rs:42");
        assert!(url.starts_with(ISSUES));
        assert!(url.contains("labels=bug"));
        assert!(url.contains(&encode(unterm_protocol::PRODUCT_VERSION)));
        assert!(url.contains("index%20out%20of%20bounds"));
        assert!(!url.contains(' ') && !url.contains('\n'));
        let long = issue_url(&"x".repeat(50_000));
        assert!(long.len() < MAX_BODY * 3 + 1_000);
    }
}
