//! What git says about the directory a pane is in.
//!
//! Read-only, deliberately. A terminal that stages and commits for you is a
//! git client with a shell attached, and the shell is right there -- what is
//! missing when you are looking at a terminal is not the ability to run `git
//! add`, it is knowing whether you are on the branch you think you are and
//! what is dirty before you run something.
//!
//! Parsing is separated from running so it can be checked without a
//! repository. Porcelain v1 is the format to parse: it is the one git
//! promises not to change, and every line is fixed-width where it matters.

/// What a repository looks like right now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: String,
    /// Commits this branch is ahead of its upstream, and behind it.
    pub ahead: usize,
    pub behind: usize,
    pub entries: Vec<Entry>,
}

/// One changed path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The two-letter code git prints: index state, then worktree state.
    pub code: String,
    pub path: String,
}

impl Status {
    /// A one-line summary, for a heading.
    pub fn summary(&self) -> String {
        let mut parts = vec![if self.branch.is_empty() {
            "(detached)".to_string()
        } else {
            self.branch.clone()
        }];
        if self.ahead > 0 {
            parts.push(format!("↑{}", self.ahead));
        }
        if self.behind > 0 {
            parts.push(format!("↓{}", self.behind));
        }
        parts.push(match self.entries.len() {
            0 => "clean".to_string(),
            1 => "1 change".to_string(),
            count => format!("{count} changes"),
        });
        parts.join("  ")
    }
}

/// Parse `git status --porcelain=v1 --branch`.
///
/// Unrecognised lines are skipped rather than guessed at: a status panel that
/// invents an entry is worse than one that shows fewer.
pub fn parse(output: &str) -> Status {
    let mut status = Status::default();
    for line in output.lines() {
        if let Some(header) = line.strip_prefix("## ") {
            parse_branch(header, &mut status);
        } else if line.len() > 3 {
            // Two code characters, a space, then the path. Renames read
            // `R  old -> new`; the new name is the one that exists now.
            let (code, rest) = line.split_at(2);
            let path = rest.trim_start();
            let path = path.rsplit(" -> ").next().unwrap_or(path);
            status.entries.push(Entry {
                code: code.trim().to_string(),
                path: path.trim_matches('"').to_string(),
            });
        }
    }
    status
}

/// The `## branch...upstream [ahead 1, behind 2]` header.
fn parse_branch(header: &str, status: &mut Status) {
    let (names, tracking) = match header.split_once(" [") {
        Some((names, tracking)) => (names, tracking.trim_end_matches(']')),
        None => (header, ""),
    };
    // `main...origin/main` -- the local name is the part before the dots.
    status.branch = names
        .split("...")
        .next()
        .unwrap_or(names)
        .trim()
        .to_string();
    if status.branch == "HEAD (no branch)" {
        status.branch.clear();
    }

    for part in tracking.split(", ") {
        if let Some(count) = part.strip_prefix("ahead ") {
            status.ahead = count.trim().parse().unwrap_or(0);
        } else if let Some(count) = part.strip_prefix("behind ") {
            status.behind = count.trim().parse().unwrap_or(0);
        }
    }
}

/// A command that does not flash a console window.
///
/// This is a GUI binary with no console of its own, so starting a console
/// program makes Windows pop one for it and take it away again. Opening the
/// git panel did exactly that, and it is very visible. No-op elsewhere.
pub fn hidden_command(program: &str) -> std::process::Command {
    #[allow(unused_mut)]
    let mut command = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// What the panel has to show.
///
/// Three answers, not two. "No git installed" and "not a repository" look the
/// same from a failed command and mean completely different things: one is
/// "this folder is not tracked", the other is "this terminal cannot tell you".
/// Reporting the first when the second is true sends someone looking for a
/// `.git` directory that is right there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Panel {
    Status(Status),
    NotARepository,
    NoGit,
}

impl Panel {
    /// The heading this answer deserves.
    pub fn heading(&self) -> String {
        let title = unterm_services::i18n::t("menu.git_panel");
        match self {
            Panel::Status(status) => format!("{title}  {}", status.summary()),
            Panel::NotARepository => {
                format!(
                    "{title}  ({})",
                    unterm_services::i18n::t("git.not_a_repository")
                )
            }
            Panel::NoGit => format!("{title}  ({})", unterm_services::i18n::t("git.no_git")),
        }
    }

    pub fn entries(&self) -> &[Entry] {
        match self {
            Panel::Status(status) => &status.entries,
            _ => &[],
        }
    }
}

/// Ask git about `directory`.
pub fn read(directory: &std::path::Path) -> Panel {
    let output = hidden_command("git")
        .args(["status", "--porcelain=v1", "--branch"])
        .current_dir(directory)
        .output();
    match output {
        // Git ran and said no: this is not a repository.
        Ok(output) if !output.status.success() => Panel::NotARepository,
        Ok(output) => Panel::Status(parse(&String::from_utf8_lossy(&output.stdout))),
        // Git did not run at all.
        Err(_) => Panel::NoGit,
    }
}

/// How long a reading stays good. Two seconds: long enough that a bar
/// refreshing four times a second does not run git four times a second, short
/// enough that a commit shows up while you are still looking at the window.
const CACHE_TTL: std::time::Duration = std::time::Duration::from_millis(2000);

type Cache = std::collections::HashMap<std::path::PathBuf, (std::time::Instant, Panel)>;

fn cache() -> &'static parking_lot::Mutex<Cache> {
    static CACHE: std::sync::OnceLock<parking_lot::Mutex<Cache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// What git says about `directory`, reading again only when the last answer
/// has gone stale.
///
/// **Blocks.** Running git costs tens of milliseconds and a frame is sixteen,
/// so this belongs on the thread that refreshes the bar, never on the one that
/// draws it.
pub fn read_cached(directory: &std::path::Path) -> Panel {
    if let Some((at, panel)) = cache().lock().get(directory) {
        if at.elapsed() < CACHE_TTL {
            return panel.clone();
        }
    }
    let panel = read(directory);
    cache().lock().insert(
        directory.to_path_buf(),
        (std::time::Instant::now(), panel.clone()),
    );
    panel
}

/// The branch `directory` is on, from whatever is already known -- never
/// waiting for git.
///
/// For the sidebar, which draws every tab's branch on every frame and cannot
/// afford a git run per row. An unknown or stale answer schedules one refresh
/// in the background (one at a time per directory), so a row fills in a
/// moment after its tab first appears and follows a checkout within seconds.
pub fn branch_hint(directory: &std::path::Path) -> Option<String> {
    let known = cache().lock().get(directory).map(|(at, panel)| (at.elapsed(), panel.clone()));
    let stale = known.as_ref().map_or(true, |(age, _)| *age >= CACHE_TTL * 5);
    if stale {
        static REFRESHING: std::sync::OnceLock<parking_lot::Mutex<std::collections::HashSet<std::path::PathBuf>>> =
            std::sync::OnceLock::new();
        let refreshing = REFRESHING.get_or_init(Default::default);
        if refreshing.lock().insert(directory.to_path_buf()) {
            let directory = directory.to_path_buf();
            let spawned = std::thread::Builder::new()
                .name("git-branch".into())
                .spawn({
                    let directory = directory.clone();
                    move || {
                        let _ = read_cached(&directory);
                        REFRESHING.get().map(|set| set.lock().remove(&directory));
                        // The row that asked is drawn already; draw it again
                        // now that there is a branch to show.
                        crate::mcp_host::request_repaint();
                    }
                });
            if spawned.is_err() {
                refreshing.lock().remove(&directory);
            }
        }
    }
    match known?.1 {
        Panel::Status(status) if !status.branch.is_empty() => Some(shorten_branch(&status.branch)),
        _ => None,
    }
}

/// The longest branch name the bar will print in full.
///
/// Long enough for the ones people type and the ones tools generate up to a
/// point; past it, a name is a sentence and it pushes everything else off the
/// bar.
const BRANCH_LIMIT: usize = 28;

/// A branch name short enough for a bar, cut in the middle.
///
/// The middle, because both ends carry meaning and neither one alone
/// identifies the branch: `agent/fix-windows-input-instance-stability` cut
/// from the end is `agent/fix-windows-input-inst`, which is every branch that
/// session opened. Keeping both ends says which one this is.
///
/// This is the one place a value here is abbreviated rather than dropped. A
/// branch always has a name, so there is no "nothing to show" to fall back to,
/// and a bar that goes blank because the branch name got long is worse than
/// one that shortens it.
fn shorten_branch(branch: &str) -> String {
    let characters: Vec<char> = branch.chars().collect();
    if characters.len() <= BRANCH_LIMIT {
        return branch.to_string();
    }
    // One for the ellipsis, and the head gets the odd character: the prefix is
    // where the convention lives (`agent/`, `feature/`, an issue number).
    let room = BRANCH_LIMIT - 1;
    let tail = room / 2;
    let head = room - tail;
    format!(
        "{}\u{2026}{}",
        characters[..head].iter().collect::<String>(),
        characters[characters.len() - tail..]
            .iter()
            .collect::<String>()
    )
}

/// `<branch> *3 +1 -2` -- what the top bar shows.
///
/// Only for a repository: a directory that is not one has nothing to say here,
/// and "not a repository" belongs in the panel that was opened to ask, not in
/// a bar nobody was reading.
pub fn render_segment(panel: &Panel) -> String {
    let Panel::Status(status) = panel else {
        return String::new();
    };
    if status.branch.is_empty() && status.entries.is_empty() {
        return String::new();
    }
    let branch = if status.branch.is_empty() {
        "(detached)".to_string()
    } else {
        shorten_branch(&status.branch)
    };
    let mut out = format!("{} {branch}", unterm_render::box_glyphs::BRANCH);
    if !status.entries.is_empty() {
        out.push_str(&format!(" *{}", status.entries.len()));
    }
    if status.ahead > 0 {
        out.push_str(&format!(" +{}", status.ahead));
    }
    if status.behind > 0 {
        out.push_str(&format!(" -{}", status.behind));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "No git" and "not a repository" are different answers. Showing the
    /// second when the first is true sends someone looking for a `.git`
    /// directory that is sitting right there.
    ///
    /// Checked against the catalogue rather than against English words: the
    /// front end is translated, and a test that expects English passes or
    /// fails depending on the machine's language.
    #[test]
    fn a_missing_git_does_not_claim_the_folder_is_untracked() {
        let no_git = Panel::NoGit.heading();
        let untracked = Panel::NotARepository.heading();
        assert_ne!(no_git, untracked);
        assert!(
            no_git.contains(&unterm_services::i18n::t("git.no_git")),
            "{no_git}"
        );
        assert!(
            untracked.contains(&unterm_services::i18n::t("git.not_a_repository")),
            "{untracked}"
        );
    }

    /// And every language actually has both, so neither falls back to the
    /// key itself showing through the interface.
    #[test]
    fn both_answers_are_translated_everywhere() {
        for key in ["git.no_git", "git.not_a_repository", "menu.git_panel"] {
            assert_ne!(
                unterm_services::i18n::t(key),
                key,
                "{key} is missing from the catalogue"
            );
        }
    }

    #[test]
    fn a_status_heading_names_the_branch() {
        let panel = Panel::Status(parse(
            "## main...origin/main [ahead 1]
 M a.rs
",
        ));
        let heading = panel.heading();
        assert!(heading.contains("main"), "{heading}");
        assert!(heading.contains("1 change"), "{heading}");
        assert_eq!(panel.entries().len(), 1);
    }

    #[test]
    fn the_answers_that_are_not_a_status_have_no_entries() {
        assert!(Panel::NoGit.entries().is_empty());
        assert!(Panel::NotARepository.entries().is_empty());
    }

    #[test]
    fn a_clean_repository_reads_as_clean() {
        let status = parse("## main...origin/main\n");
        assert_eq!(status.branch, "main");
        assert_eq!(status.entries.len(), 0);
        assert_eq!(status.summary(), "main  clean");
    }

    #[test]
    fn ahead_and_behind_are_both_read() {
        let status = parse("## main...origin/main [ahead 2, behind 3]\n");
        assert_eq!((status.ahead, status.behind), (2, 3));
        assert!(status.summary().contains("↑2"), "{}", status.summary());
        assert!(status.summary().contains("↓3"), "{}", status.summary());
    }

    #[test]
    fn ahead_alone_does_not_invent_a_behind() {
        let status = parse("## main...origin/main [ahead 1]\n");
        assert_eq!((status.ahead, status.behind), (1, 0));
        assert!(!status.summary().contains('↓'), "{}", status.summary());
    }

    /// A branch with no upstream has no counts, and must not be mistaken for
    /// one that is level with its upstream.
    #[test]
    fn a_branch_with_no_upstream_still_names_itself() {
        let status = parse("## experiment\n");
        assert_eq!(status.branch, "experiment");
        assert_eq!((status.ahead, status.behind), (0, 0));
    }

    /// Detached HEAD is a state people get into by accident, and the panel
    /// exists to tell them.
    #[test]
    fn a_detached_head_says_so_rather_than_naming_a_branch() {
        let status = parse("## HEAD (no branch)\n");
        assert!(status.branch.is_empty());
        assert!(
            status.summary().starts_with("(detached)"),
            "{}",
            status.summary()
        );
    }

    #[test]
    fn changed_paths_keep_their_codes() {
        let status = parse("## main\n M src/lib.rs\n?? notes.txt\nA  added.rs\n");
        assert_eq!(status.entries.len(), 3);
        assert_eq!(status.entries[0].code, "M");
        assert_eq!(status.entries[0].path, "src/lib.rs");
        assert_eq!(status.entries[1].code, "??");
        assert_eq!(status.entries[2].code, "A");
    }

    /// A rename prints both names. The one that exists now is the one to
    /// show: pointing at a path that is gone is a panel telling a lie.
    #[test]
    fn a_rename_shows_where_the_file_is_now() {
        let status = parse("## main\nR  old/name.rs -> new/name.rs\n");
        assert_eq!(status.entries[0].path, "new/name.rs");
    }

    /// Paths with spaces come back quoted; the quotes are git's, not the
    /// file's.
    #[test]
    fn a_quoted_path_loses_its_quotes() {
        let status = parse("## main\n M \"a file.txt\"\n");
        assert_eq!(status.entries[0].path, "a file.txt");
    }

    #[test]
    fn one_change_is_singular_and_two_are_not() {
        let one = parse("## main\n M a.rs\n");
        assert!(one.summary().ends_with("1 change"), "{}", one.summary());
        let two = parse("## main\n M a.rs\n M b.rs\n");
        assert!(two.summary().ends_with("2 changes"), "{}", two.summary());
    }

    /// Nothing at all is a repository with no commits yet, not a crash.
    #[test]
    fn empty_output_is_a_status_rather_than_a_panic() {
        let status = parse("");
        assert_eq!(status, Status::default());
        assert_eq!(status.summary(), "(detached)  clean");
    }

    /// Junk in the middle is skipped, not guessed at: a panel that invents an
    /// entry is worse than one that shows fewer.
    #[test]
    fn a_line_too_short_to_be_an_entry_is_ignored() {
        let status = parse("## main\nx\n M real.rs\n");
        assert_eq!(status.entries.len(), 1);
        assert_eq!(status.entries[0].path, "real.rs");
    }
}

#[cfg(test)]
mod segment_tests {
    use super::*;

    #[test]
    fn a_clean_branch_is_just_its_name() {
        let panel = Panel::Status(Status {
            branch: "master".into(),
            ..Default::default()
        });
        assert_eq!(
            render_segment(&panel),
            format!("{} master", unterm_render::box_glyphs::BRANCH)
        );
    }

    /// The three counts, in the order they matter: what is uncommitted, what
    /// is unpushed, what is unpulled.
    #[test]
    fn changes_and_both_directions_are_all_counted() {
        let panel = Panel::Status(Status {
            branch: "main".into(),
            ahead: 1,
            behind: 2,
            entries: vec![
                Entry {
                    code: "M".into(),
                    path: "a".into(),
                },
                Entry {
                    code: "M".into(),
                    path: "b".into(),
                },
                Entry {
                    code: "??".into(),
                    path: "c".into(),
                },
            ],
        });
        assert_eq!(
            render_segment(&panel),
            format!("{} main *3 +1 -2", unterm_render::box_glyphs::BRANCH)
        );
    }

    #[test]
    fn a_detached_head_says_so_rather_than_showing_a_blank() {
        let panel = Panel::Status(Status {
            branch: String::new(),
            entries: vec![Entry {
                code: "M".into(),
                path: "a".into(),
            }],
            ..Default::default()
        });
        assert!(render_segment(&panel).contains("(detached)"));
    }

    /// A folder that is not a repository, and a machine with no git, both say
    /// nothing here. The bar is for what is true, not for what is missing --
    /// the panel is where that question gets an answer.
    #[test]
    fn nothing_to_say_says_nothing() {
        assert_eq!(render_segment(&Panel::NotARepository), "");
        assert_eq!(render_segment(&Panel::NoGit), "");
        assert_eq!(render_segment(&Panel::Status(Status::default())), "");
    }

    /// A second reading inside the window does not run git again: the bar
    /// refreshes four times a second, and a process each time is a process
    /// each time.
    #[test]
    fn a_second_reading_inside_the_window_is_free() {
        let here = std::env::current_dir().expect("a working directory");
        let first = read_cached(&here);
        let start = std::time::Instant::now();
        let second = read_cached(&here);
        assert_eq!(first, second);
        assert!(
            start.elapsed() < std::time::Duration::from_millis(5),
            "the second reading took {:?}",
            start.elapsed()
        );
    }

    /// And this crate is inside a repository, so what comes back is a status
    /// rather than a refusal.
    #[test]
    fn a_real_repository_reads_as_one() {
        let here = std::env::current_dir().expect("a working directory");
        match read_cached(&here) {
            // Whether it names a branch is not what this asks. A pull
            // request is checked out as a detached merge commit, and
            // `parse_branch` empties the name there on purpose -- so
            // requiring one made every pull-request build red on all three
            // platforms whatever the change was, which is how PR #31 came
            // back red for a defect it did not have. A repository still
            // describes itself when its head is detached; that is the thing
            // being tested, and `summary` is where it says so.
            Panel::Status(status) => assert!(
                !status.summary().is_empty(),
                "a repository describes itself, detached or not"
            ),
            // A machine with no git on PATH cannot be asked to have one.
            Panel::NoGit => {}
            Panel::NotARepository => panic!("this crate is inside a repository"),
        }
    }
}

#[cfg(test)]
mod branch_name_tests {
    use super::*;

    #[test]
    fn a_branch_name_that_fits_is_left_alone() {
        assert_eq!(shorten_branch("master"), "master");
        assert_eq!(shorten_branch("feature/tab-bar"), "feature/tab-bar");
        let exactly = "a".repeat(BRANCH_LIMIT);
        assert_eq!(shorten_branch(&exactly), exactly);
    }

    /// Cut in the middle, because both ends say which branch this is. Cut from
    /// the end, every branch a session opened reads the same.
    #[test]
    fn a_long_branch_name_keeps_both_of_its_ends() {
        let long = "agent/fix-windows-input-instance-stability";
        let short = shorten_branch(long);
        assert_eq!(short.chars().count(), BRANCH_LIMIT);
        assert!(short.starts_with("agent/fix"), "{short}");
        assert!(short.ends_with("stability"), "{short}");
        assert!(short.contains('\u{2026}'), "{short}");
    }

    /// Two branches that share a long prefix still read as two branches.
    #[test]
    fn two_long_branches_do_not_shorten_to_the_same_thing() {
        let first = shorten_branch("agent/fix-windows-input-instance-stability");
        let second = shorten_branch("agent/fix-windows-input-instance-performance");
        assert_ne!(first, second);
    }

    /// And the whole segment stays inside the room a bar can give it, which is
    /// what the branch limit exists for.
    #[test]
    fn the_segment_fits_the_room_a_bar_has() {
        let panel = Panel::Status(Status {
            branch: "agent/fix-windows-input-instance-stability".into(),
            ahead: 12,
            behind: 34,
            entries: (0..99)
                .map(|index| Entry {
                    code: "M".into(),
                    path: format!("file{index}"),
                })
                .collect(),
        });
        let segment = render_segment(&panel);
        assert!(
            crate::statsbar::width_of(&segment) <= 45,
            "the git segment is {} columns: {segment}",
            crate::statsbar::width_of(&segment)
        );
    }
}
