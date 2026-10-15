//! The line along the bottom: what shell, where, how big, and the window's own
//! state.
//!
//! Ported from the previous front end, segment order and thresholds included:
//!
//! ```text
//!     ▼  pwsh 7   ~/   120x31   project:unterm   capture:exclude
//!        capture:include   proxy:on   mcp:0   theme:standard
//! ```
//!
//! Three spaces between segments rather than a separator character. They are
//! already differently shaped -- a name, a path, a pair of numbers, a run of
//! `key:value` -- and a bar full of pipes reads as a table.
//!
//! Segments appear as the window widens, in the order the previous front end
//! revealed them, and the thresholds are its own. The point of porting them
//! rather than re-picking is that they were chosen against real window widths:
//! a 13-inch laptop at full screen lands around 150-180 columns, so anything
//! gated higher than that is a segment nobody on a laptop ever sees.

// 0.57.4's bar carried no menu mark: it opens with the shell's name, and the
// quick menu lives on the top bar's chevron alone. The extra `▾` this bar
// briefly grew read as a difference from every released window, so it is gone.

/// What the bar shows at a given width, in columns of chrome text.
///
/// Ported thresholds. The path shortens rather than disappearing, because where
/// you are is the one thing a status bar is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Density {
    pub cwd_columns: usize,
    pub show_project: bool,
    pub show_theme: bool,
    pub show_telemetry: bool,
    pub show_profile: bool,
    pub show_capture: bool,
}

pub fn density(columns: usize) -> Density {
    Density {
        cwd_columns: match columns {
            0..=79 => 18,
            80..=111 => 26,
            112..=143 => 34,
            _ => 48,
        },
        show_project: columns >= 72,
        show_theme: columns >= 96,
        // A laptop at full screen lands around 150-180 columns, so anything
        // higher is unreachable there.
        show_telemetry: columns >= 128,
        // The theme tier, not the telemetry tier: capture is a headline
        // feature, and at the default ~80-column window it was invisible —
        // which reads as "screenshots are not implemented", not "resize me".
        show_capture: columns >= 96,
        show_profile: columns >= 160,
    }
}

/// What the bar has to say.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// The shell, named with its version where the version matters.
    pub shell: String,
    /// Where the pane is, as a tilde path.
    pub directory: String,
    pub columns: usize,
    pub rows: usize,
    /// The project the pane is in.
    pub project: String,
    /// Whether new sessions get Unterm's proxy env vars. The chip shows the
    /// toggle, not the system proxy: 0.57.4's did, and a bar that says "on"
    /// because the OS has a proxy reads as Unterm doing something it is not.
    pub proxy: bool,
    /// A click just tried to switch the proxy on and nothing answered the
    /// probe. Shown briefly in place of on/off, in the error colour.
    pub proxy_unreachable: bool,
    /// How many times an agent has written to a pane this session, and whether
    /// one of those writes was a moment ago.
    pub agent_writes: u64,
    pub agent_wrote_recently: bool,
    /// The theme by the id the config and the CLI use.
    pub theme: String,
    /// The identity profile bound to this window, if one is.
    pub profile: Option<String>,
    /// Something that just happened, shown for a moment in place of the rest.
    ///
    /// The bar rather than a floating panel: a confirmation belongs where the
    /// eye already is, and a panel over the terminal covers the work.
    pub notice: Option<String>,
    /// This window could not reach the Core and is keeping its sessions in
    /// this process, so they die with it.
    ///
    /// Standing state, not an event: it is true for the whole life of the
    /// process, which is exactly why a notice is the wrong place for it --
    /// one scrolls past in two seconds and the window then looks normal for
    /// days while being the arrangement the user did not choose.
    pub core_local: bool,
}

/// What a segment is, so a press on it can do what 0.57.4's did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentKind {
    Shell,
    Cwd,
    Size,
    Project,
    CaptureExclude,
    CaptureInclude,
    Proxy,
    Mcp,
    Theme,
    Profile,
    Notice,
    Core,
}

/// One piece of the bar.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub text: String,
    /// Chips are drawn dimmer than the shell and the path, which is what makes
    /// where-you-are readable at a glance rather than one of nine things.
    pub dim: bool,
    /// Where the accent-coloured value starts, when the segment has one.
    /// 0.57.4 drew every actionable value — the path, and everything after a
    /// chip's colon — in the theme's teal, which is what made the bar read as
    /// labels-and-values rather than one grey line.
    pub teal_from: Option<usize>,
    /// The value is bad news for a moment — a proxy probe just failed — and
    /// reads in the error colour rather than the accent.
    pub error: bool,
    pub kind: SegmentKind,
}

/// The shell's name, with the version where two versions behave differently.
///
/// Ported: `pwsh 7` and `pwsh 5.1` are different shells in practice, and a bar
/// that calls both "powershell" answers the wrong question. A WSL shell says so,
/// because a `bash` on Windows is either Git Bash or WSL and they are not the
/// same machine.
pub fn shell_label(process: &str) -> String {
    let lower = process.to_lowercase();
    let name = lower
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&lower)
        .to_string();
    let from_wsl = cfg!(windows) && lower.starts_with('/');
    let with_wsl = |base: &str| {
        if from_wsl {
            format!("{base} (wsl)")
        } else {
            base.to_string()
        }
    };

    if name.contains("pwsh") {
        "pwsh 7".to_string()
    } else if name.contains("powershell") {
        "pwsh 5.1".to_string()
    } else if name.contains("cmd") {
        "cmd".to_string()
    } else if name.contains("wsl") {
        "wsl".to_string()
    } else if name.contains("bash") {
        with_wsl("bash")
    } else if name.contains("zsh") {
        with_wsl("zsh")
    } else if name.contains("fish") {
        with_wsl("fish")
    } else if name.starts_with("nu") {
        "nu".to_string()
    } else {
        "shell".to_string()
    }
}

/// A path as a tilde path, shortened from the front.
///
/// From the front because the end of a path is where you are: `…/unterm/ci` says
/// more than `D:/code/unt…`.
pub fn short_path(path: &str, columns: usize) -> String {
    let path = tilde(path);
    if columns == 0 {
        return String::new();
    }
    let width: usize = path.chars().map(crate::terminal::column_width).sum();
    if width <= columns {
        return path;
    }
    let mut kept: Vec<char> = Vec::new();
    let mut used = 1usize;
    for ch in path.chars().rev() {
        let wide = crate::terminal::column_width(ch);
        if used + wide > columns {
            break;
        }
        kept.push(ch);
        used += wide;
    }
    kept.reverse();
    format!("\u{2026}{}", kept.into_iter().collect::<String>())
}

/// Home as `~`, and forward slashes: a status bar is not a shell prompt, and
/// one separator reads better than two.
fn tilde(path: &str) -> String {
    let path = path.replace('\\', "/");
    let trimmed = path.trim_end_matches('/');
    let shown = if trimmed.is_empty() { &path } else { trimmed };
    if let Some(home) = dirs_next::home_dir() {
        let home = home.display().to_string().replace('\\', "/");
        let home = home.trim_end_matches('/');
        let comparable = |text: &str| {
            if cfg!(windows) {
                text.to_lowercase()
            } else {
                text.to_string()
            }
        };
        if comparable(shown) == comparable(home) {
            return "~/".to_string();
        }
        if let Some(rest) = comparable(shown).strip_prefix(&format!("{}/", comparable(home))) {
            let at = shown.len() - rest.len();
            return format!("~/{}", &shown[at..]);
        }
    }
    format!("{shown}/")
}

/// Shorten a label to `columns`, cutting the middle.
pub(crate) fn middle(text: &str, columns: usize) -> String {
    let width: usize = text.chars().map(crate::terminal::column_width).sum();
    if columns == 0 || width <= columns {
        return text.to_string();
    }
    if columns == 1 {
        return "\u{2026}".to_string();
    }
    let available = columns - 1;
    let head_budget = available / 2;
    let characters: Vec<char> = text.chars().collect();
    let mut head = String::new();
    let mut used = 0;
    let mut taken = 0;
    for ch in &characters {
        let wide = crate::terminal::column_width(*ch);
        if used + wide > head_budget {
            break;
        }
        head.push(*ch);
        used += wide;
        taken += 1;
    }
    let mut tail: Vec<char> = Vec::new();
    let mut used = 0;
    for ch in characters[taken..].iter().rev() {
        let wide = crate::terminal::column_width(*ch);
        if used + wide > available - head_budget {
            break;
        }
        tail.push(*ch);
        used += wide;
    }
    tail.reverse();
    format!("{head}\u{2026}{}", tail.into_iter().collect::<String>())
}

/// The bar's segments, in order, for a window this many columns wide.
pub fn segments(status: &Status, columns: usize) -> Vec<Segment> {
    let mut segments = Vec::new();
    if columns < 12 {
        return segments;
    }

    // A notice takes the whole line: it is transient, and reading it matters
    // more for the moment it is up than any of the standing state does.
    if let Some(notice) = status.notice.as_deref().map(str::trim) {
        if !notice.is_empty() {
            segments.push(Segment {
                text: format!("\u{2713} {}", middle(notice, columns.saturating_sub(4))),
                dim: false,
                teal_from: None,
                error: false,
                kind: SegmentKind::Notice,
            });
            return segments;
        }
    }

    // Leads the bar, and survives every density: a window whose sessions will
    // not outlive it is not a detail to fit in after the theme name. Drawn in
    // the error colour for the same reason the proxy chip is -- this is the
    // bar reporting something wrong, not labelling a setting.
    if status.core_local {
        segments.push(Segment {
            text: "core:local".to_string(),
            dim: false,
            teal_from: None,
            error: true,
            kind: SegmentKind::Core,
        });
    }

    let density = density(columns);
    let push = |text: String,
                dim: bool,
                teal_from: Option<usize>,
                kind: SegmentKind,
                into: &mut Vec<Segment>| {
        if !text.is_empty() {
            into.push(Segment {
                text,
                dim,
                teal_from,
                error: false,
                kind,
            });
        }
    };

    // The shell, where, and how big: the three that are always there.
    push(
        status.shell.clone(),
        false,
        None,
        SegmentKind::Shell,
        &mut segments,
    );
    push(
        short_path(&status.directory, density.cwd_columns),
        false,
        Some(0),
        SegmentKind::Cwd,
        &mut segments,
    );
    push(
        format!("{}x{}", status.columns, status.rows),
        true,
        None,
        SegmentKind::Size,
        &mut segments,
    );

    if density.show_project && !status.project.is_empty() {
        push(
            format!("project:{}", middle(&status.project, 18)),
            true,
            Some("project:".len()),
            SegmentKind::Project,
            &mut segments,
        );
    }
    // Both capture chips, always as a pair: they are two halves of one
    // decision, and one of them alone reads as a state rather than a choice.
    // Literal label:value chips as 0.57.4 drew them, teal after the colon --
    // prose labels lost the value colouring and stopped reading as chips.
    if density.show_capture {
        let value = "capture:".len();
        push(
            "capture:exclude".to_string(),
            true,
            Some(value),
            SegmentKind::CaptureExclude,
            &mut segments,
        );
        push(
            "capture:include".to_string(),
            true,
            Some(value),
            SegmentKind::CaptureInclude,
            &mut segments,
        );
    }
    if density.show_telemetry {
        // The chip is the injection toggle, and its click is the switch. The
        // unreachable state is transient: a press tried to switch it on, the
        // probe found nothing listening, and the chip says so for a moment
        // rather than silently ignoring the press.
        let (proxy_text, proxy_error) = if status.proxy_unreachable {
            ("proxy:unreachable", true)
        } else if status.proxy {
            ("proxy:on", false)
        } else {
            ("proxy:off", false)
        };
        segments.push(Segment {
            text: proxy_text.to_string(),
            dim: true,
            teal_from: Some("proxy:".len()),
            error: proxy_error,
            kind: SegmentKind::Proxy,
        });
        // Always drawn, even at zero, so the position is stable: a chip that
        // appears and disappears moves every neighbour and every hit test with
        // it. The bolt says a write landed a moment ago, so a flash is
        // noticeable without comparing counts.
        push(
            format!(
                "mcp:{}{}",
                status.agent_writes,
                if status.agent_wrote_recently {
                    "\u{26A1}"
                } else {
                    ""
                }
            ),
            true,
            Some("mcp:".len()),
            SegmentKind::Mcp,
            &mut segments,
        );
    }
    if density.show_theme && !status.theme.is_empty() {
        push(
            format!("theme:{}", status.theme),
            true,
            Some("theme:".len()),
            SegmentKind::Theme,
            &mut segments,
        );
    }
    if density.show_profile {
        // A dash rather than nothing when no profile is bound: the chip is how
        // somebody finds out profiles exist, and one that is missing until it
        // is configured cannot be.
        let profile = status.profile.as_deref().unwrap_or("\u{2014}");
        push(
            format!("profile:{}", middle(profile, 18)),
            true,
            Some("profile:".len()),
            SegmentKind::Profile,
            &mut segments,
        );
    }
    segments
}

/// The gap between segments: three spaces.
pub const GAP: &str = "   ";

/// A program's bare name, for the places that want one.
pub fn short_name(program: &str) -> String {
    program
        .trim()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim_end_matches(".exe")
        .trim_end_matches(".EXE")
        .to_string()
}

/// A proxy URL as something short enough for a bar.
pub fn short_proxy(url: &str) -> String {
    url.trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> Status {
        Status {
            shell: "pwsh 7".into(),
            directory: "/home/me/work/unterm".into(),
            columns: 120,
            rows: 31,
            project: "unterm".into(),
            proxy: true,
            proxy_unreachable: false,
            agent_writes: 0,
            agent_wrote_recently: false,
            theme: "standard".into(),
            profile: None,
            notice: None,
            core_local: false,
        }
    }

    fn texts(segments: &[Segment]) -> Vec<String> {
        segments.iter().map(|s| s.text.clone()).collect()
    }

    /// The order is the previous front end's, and it is the order the questions
    /// are asked in: what shell, where, how big, then the window's own state.
    #[test]
    fn the_segments_come_in_the_order_they_did_before() {
        let shown = texts(&segments(&status(), 200));
        let wanted = vec![
            "pwsh 7".to_string(),
            "project:unterm".to_string(),
            "capture:exclude".to_string(),
            "capture:include".to_string(),
            "proxy:on".to_string(),
            "mcp:0".to_string(),
            "theme:standard".to_string(),
        ];
        let mut at = 0;
        for want in wanted {
            let found = shown[at..]
                .iter()
                .position(|text| text == &want)
                .unwrap_or_else(|| panic!("{want:?} is missing or out of order in {shown:?}"));
            at += found + 1;
        }
        // The size sits between the path and the project.
        assert!(shown.iter().any(|text| text == "120x31"), "{shown:?}");
    }

    /// Segments appear as the window widens, and the ones that go first are the
    /// ones that matter least. Ported thresholds.
    #[test]
    fn segments_appear_as_the_window_widens() {
        assert!(!density(60).show_project);
        assert!(density(72).show_project);
        assert!(!density(90).show_theme);
        assert!(density(96).show_theme);
        assert!(!density(120).show_telemetry);
        assert!(density(128).show_telemetry);
        assert!(!density(150).show_profile);
        assert!(density(160).show_profile);
    }

    /// A narrow window still says which shell and where: those are what a
    /// status bar is for, and everything else is context.
    #[test]
    fn a_narrow_window_keeps_the_shell_and_the_path() {
        let shown = texts(&segments(&status(), 40));
        assert!(shown.iter().any(|text| text == "pwsh 7"), "{shown:?}");
        assert!(
            shown.iter().any(|text| text.contains("unterm")),
            "{shown:?}"
        );
        assert!(
            !shown.iter().any(|text| text.starts_with("proxy:")),
            "a narrow bar showed telemetry: {shown:?}"
        );
    }

    /// The proxy chip is the injection toggle, so it reads off when the toggle
    /// is off — whatever the OS's own proxy settings say.
    #[test]
    fn the_proxy_chip_shows_the_toggle() {
        let mut status = status();
        status.proxy = false;
        let shown = texts(&segments(&status, 200));
        assert!(shown.iter().any(|text| text == "proxy:off"), "{shown:?}");
    }

    /// A failed probe takes the chip over for a moment, in the error colour:
    /// the press did something, and what it did was find nothing listening.
    #[test]
    fn a_failed_probe_shows_on_the_chip() {
        let mut status = status();
        status.proxy_unreachable = true;
        let shown = segments(&status, 200);
        let chip = shown
            .iter()
            .find(|segment| segment.kind == SegmentKind::Proxy)
            .expect("the proxy chip");
        assert_eq!(chip.text, "proxy:unreachable");
        assert!(chip.error);
        // And only the proxy chip carries the error colour.
        assert!(shown
            .iter()
            .filter(|segment| segment.kind != SegmentKind::Proxy)
            .all(|segment| !segment.error));
    }

    /// The MCP chip is always drawn once telemetry is on, even at zero: a chip
    /// that appears and disappears moves every neighbour with it.
    #[test]
    fn the_agent_chip_holds_its_place_at_zero() {
        let shown = texts(&segments(&status(), 200));
        assert!(shown.iter().any(|text| text == "mcp:0"), "{shown:?}");
    }

    /// And says when something just happened, so a flash is noticeable without
    /// comparing counts.
    #[test]
    fn a_recent_agent_write_is_marked() {
        let mut status = status();
        status.agent_writes = 3;
        status.agent_wrote_recently = true;
        let shown = texts(&segments(&status, 200));
        assert!(
            shown.iter().any(|text| text == "mcp:3\u{26A1}"),
            "{shown:?}"
        );
    }

    /// Two versions of PowerShell behave differently enough that a bar calling
    /// both "powershell" answers the wrong question.
    #[test]
    fn the_shell_is_named_with_its_version() {
        assert_eq!(shell_label("pwsh.exe"), "pwsh 7");
        assert_eq!(shell_label("powershell.exe"), "pwsh 5.1");
        assert_eq!(shell_label("cmd.exe"), "cmd");
        assert_eq!(shell_label("fish"), "fish");
        assert_eq!(shell_label("nu"), "nu");
        assert_eq!(shell_label("something-else"), "shell");
    }

    /// On Windows a shell whose path starts at the root came through WSL, and
    /// that is worth saying: a `bash` there is either Git Bash or another
    /// machine entirely.
    #[test]
    #[cfg(windows)]
    fn a_shell_reached_through_wsl_says_so() {
        assert_eq!(shell_label("/usr/bin/bash"), "bash (wsl)");
        assert_eq!(shell_label("/bin/zsh"), "zsh (wsl)");
        assert_eq!(shell_label("bash"), "bash");
    }

    /// Home is `~`. A bar that spells out the home directory spends a third of
    /// itself on something every path starts with.
    #[test]
    fn home_is_a_tilde() {
        let Some(home) = dirs_next::home_dir() else {
            return;
        };
        assert_eq!(short_path(&home.display().to_string(), 40), "~/");
        let inside = home.join("work");
        assert_eq!(short_path(&inside.display().to_string(), 40), "~/work");
    }

    /// A path too long is cut from the front: the end of a path is where you
    /// are, and `D:/code/unt…` says nothing.
    #[test]
    fn a_long_path_keeps_its_end() {
        let shown = short_path("/a/very/deeply/nested/place/indeed/here", 18);
        let width: usize = shown.chars().map(crate::terminal::column_width).sum();
        assert!(width <= 18, "{shown:?} is {width} wide");
        // The trailing slash is the previous front end's, and it is what makes
        // the path read as a place rather than as a file.
        assert!(shown.ends_with("here/"), "{shown:?}");
        assert!(shown.starts_with('\u{2026}'), "{shown:?}");
    }

    /// A notice takes the line while it is up: for that moment it matters more
    /// than any of the standing state.
    #[test]
    fn a_notice_takes_the_whole_line() {
        let mut status = status();
        status.notice = Some("copied".into());
        let shown = texts(&segments(&status, 200));
        assert_eq!(shown.len(), 1, "{shown:?}");
        assert!(shown[0].contains("copied"), "{shown:?}");
    }

    /// The profile chip shows a dash rather than vanishing: it is how somebody
    /// finds out profiles exist, and one that is missing until it is configured
    /// cannot be.
    #[test]
    fn the_profile_chip_is_visible_before_it_is_set() {
        let shown = texts(&segments(&status(), 200));
        assert!(
            shown.iter().any(|text| text == "profile:\u{2014}"),
            "{shown:?}"
        );
    }

    /// Every segment fits its own budget, so nothing runs into its neighbour.
    #[test]
    fn no_segment_runs_away_with_the_bar() {
        let mut status = status();
        status.project = "a-project-with-a-very-long-name-indeed".into();
        status.profile = Some("a-profile-with-a-very-long-name".into());
        for piece in segments(&status, 200) {
            let width: usize = piece.text.chars().map(crate::terminal::column_width).sum();
            assert!(width <= 27, "{:?} is {width} wide", piece.text);
        }
    }

    /// The chips carry their value offsets, so the painter can colour the
    /// answer without re-parsing the text.
    #[test]
    fn chips_know_where_their_values_start() {
        let shown = segments(&status(), 200);
        let of = |prefix: &str| {
            shown
                .iter()
                .find(|s| s.text.starts_with(prefix))
                .unwrap_or_else(|| panic!("{prefix:?} missing"))
                .teal_from
        };
        assert_eq!(of("theme:"), Some("theme:".len()));
        let capture = shown
            .iter()
            .find(|segment| segment.kind == SegmentKind::CaptureExclude)
            .expect("desktop capture chip");
        assert_eq!(capture.teal_from, Some("capture:".len()));
        // The path is the second segment, and reads teal from its first
        // character.
        assert_eq!(shown[1].teal_from, Some(0));
        assert_eq!(of("pwsh"), None);
    }

    /// The bar's job here is to be noticed. A window that could not reach the
    /// Core keeps its sessions in this process, so closing it ends them --
    /// the opposite of what the Core is for, and not something the user can
    /// see by looking at a terminal that otherwise behaves normally.
    #[test]
    fn a_window_that_lost_the_core_leads_the_bar_with_it() {
        let mut status = status();
        status.core_local = true;
        let segments = segments(&status, 200);
        let first = segments.first().expect("a bar with segments");
        assert_eq!(first.text, "core:local");
        assert_eq!(first.kind, SegmentKind::Core);
        assert!(first.error, "it reports something wrong, not a setting");
    }

    /// Every other chip has a width it drops out at. This one must not: the
    /// narrow window is exactly where a user would otherwise never learn that
    /// their sessions are not the Core's.
    #[test]
    fn the_core_chip_outlives_every_other_chip_as_the_bar_narrows() {
        let mut status = status();
        status.core_local = true;
        for columns in [12, 40, 71, 95, 127, 159] {
            let shown = texts(&segments(&status, columns));
            assert!(
                shown.contains(&"core:local".to_string()),
                "core:local went missing at {columns} columns: {shown:?}"
            );
        }
    }

    /// And it is absent when there is nothing wrong -- a chip that is always
    /// there is furniture, and stops being read.
    #[test]
    fn an_attached_core_puts_no_chip_on_the_bar() {
        let shown = texts(&segments(&status(), 200));
        assert!(!shown.contains(&"core:local".to_string()), "{shown:?}");
    }

}
