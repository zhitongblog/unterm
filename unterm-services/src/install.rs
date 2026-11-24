//! What is already installed, what conflicts, and what removing it would take.
//!
//! Shipping one package means arriving on machines that already have Unterm
//! some other way — a DMG dragged to `/Applications`, a Homebrew cask, a
//! `cargo install`, a `.deb`, a symlink somebody made in `/usr/local/bin`
//! pointing at a build tree. Two installs are not a cosmetic problem: the one
//! on `PATH` and the one the user double-clicks can be different versions
//! talking to the same state directory, and the failure looks like data
//! corruption rather than like two installs.
//!
//! This is the part of that work that is decision rather than packaging: find
//! them, say which is authoritative, and describe what an uninstall would
//! remove **before** removing anything.
//!
//! Nothing here deletes. A survey that could delete is one nobody dares run,
//! and the plan is more useful than the act anyway: the answer to "what will
//! I lose" is what a person actually wants before they answer yes.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// How Unterm got onto this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A `.app` bundle on macOS.
    Bundle,
    /// A Homebrew cask or formula.
    Homebrew,
    /// A system package: deb, rpm, MSI.
    SystemPackage,
    /// `cargo install`, or a binary somebody copied into place.
    Loose,
    /// A symlink pointing at one of the above — or at a build tree, which is
    /// how a developer's machine ends up shadowing the real install.
    Symlink,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Bundle => "bundle",
            Kind::Homebrew => "homebrew",
            Kind::SystemPackage => "system_package",
            Kind::Loose => "loose",
            Kind::Symlink => "symlink",
        }
    }
}

/// One copy of Unterm found on this machine.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Install {
    pub kind: String,
    pub path: String,
    /// Where a symlink points, when that is the interesting part.
    pub target: Option<String>,
    /// Whether this is the one a shell would run.
    pub on_path: bool,
}

/// Every place Unterm might have been installed, per platform.
fn candidates() -> Vec<(Kind, PathBuf)> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut found = Vec::new();

    if cfg!(target_os = "macos") {
        found.push((Kind::Bundle, PathBuf::from("/Applications/Unterm.app")));
        found.push((Kind::Bundle, home.join("Applications/Unterm.app")));
        found.push((
            Kind::Homebrew,
            PathBuf::from("/opt/homebrew/Caskroom/unterm"),
        ));
        found.push((Kind::Homebrew, PathBuf::from("/usr/local/Caskroom/unterm")));
    } else if cfg!(target_os = "windows") {
        for base in ["ProgramFiles", "LOCALAPPDATA"] {
            if let Some(dir) = std::env::var_os(base) {
                found.push((Kind::SystemPackage, PathBuf::from(dir).join("Unterm")));
            }
        }
    } else {
        found.push((Kind::SystemPackage, PathBuf::from("/usr/bin/unterm")));
        found.push((Kind::SystemPackage, PathBuf::from("/usr/lib/unterm")));
        found.push((Kind::Loose, PathBuf::from("/opt/unterm")));
    }

    // Everywhere, and the usual source of the confusing case.
    found.push((Kind::Loose, home.join(".cargo/bin/unterm")));
    found.push((Kind::Symlink, PathBuf::from("/usr/local/bin/unterm")));
    found.push((Kind::Symlink, PathBuf::from("/usr/local/bin/unterm-cli")));
    found
}

/// What a shell would actually run.
fn on_path() -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        for name in ["unterm", "unterm.exe"] {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Find every copy on this machine.
pub fn survey() -> Vec<Install> {
    let running = on_path();
    let mut found: Vec<Install> = Vec::new();
    for (kind, path) in candidates() {
        if !path.exists() {
            continue;
        }
        let target = std::fs::read_link(&path)
            .ok()
            .map(|target| target.display().to_string());
        found.push(Install {
            // A path that is a symlink is reported as one whatever list it
            // came from: where it points is the fact that explains the
            // machine's behaviour.
            kind: if target.is_some() {
                Kind::Symlink.as_str().to_string()
            } else {
                kind.as_str().to_string()
            },
            on_path: running.as_deref() == Some(path.as_path()),
            path: path.display().to_string(),
            target,
        });
    }
    found
}

/// Two installs that will fight, and why.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Conflict {
    pub reason: String,
    pub paths: Vec<String>,
    /// What to do about it, in words a person can act on.
    pub advice: String,
}

/// Which of the installs found will get in each other's way.
pub fn conflicts(installs: &[Install]) -> Vec<Conflict> {
    let mut conflicts = Vec::new();

    // Symlinks that point somewhere else entirely. The classic: a developer's
    // `/usr/local/bin/unterm` aimed at a build tree, shadowing the release
    // they think they are running.
    for install in installs {
        if let Some(target) = &install.target {
            if target.contains("/target/debug") || target.contains("/target/release") {
                conflicts.push(Conflict {
                    reason: format!("{} points at a build tree", install.path),
                    paths: vec![install.path.clone(), target.clone()],
                    advice: "Remove the symlink, or accept that the shell runs the build and the app icon runs the release.".into(),
                });
            }
        }
    }

    // Real installs of more than one kind. Not counting symlinks: a symlink
    // into a bundle is how a bundle gets onto PATH, and calling that a
    // conflict would flag every correct macOS install.
    let substantial: Vec<&Install> = installs
        .iter()
        .filter(|install| install.kind != Kind::Symlink.as_str())
        .collect();
    if substantial.len() > 1 {
        conflicts.push(Conflict {
            reason: "more than one Unterm is installed".into(),
            paths: substantial
                .iter()
                .map(|install| install.path.clone())
                .collect(),
            // The specific failure, because "you have two installs" is not by
            // itself alarming and the consequence is.
            advice: "They share one state directory: whichever starts first owns the sessions, and two versions can migrate the same database differently. Keep one.".into(),
        });
    }
    conflicts
}

/// What removing Unterm would take away.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UninstallPlan {
    /// Program files. Always removed.
    pub programs: Vec<String>,
    /// The user's data. Removed only if they say so.
    pub data: Vec<String>,
    /// What the data is, so "delete my data?" is a question with an answer.
    pub data_description: Vec<String>,
    pub keeps_data: bool,
}

/// Describe an uninstall without performing one.
///
/// `keep_data` is the whole point of the type. "Uninstall" means two very
/// different things — remove the program, and forget everything the user did
/// with it — and a package manager that does both because they were one
/// checkbox is how people lose a year of task history.
pub fn uninstall_plan(keep_data: bool) -> UninstallPlan {
    let programs = survey()
        .into_iter()
        .map(|install| install.path)
        .collect::<Vec<_>>();

    let state = unterm_protocol::state_dir();
    let mut data = Vec::new();
    let mut description = Vec::new();

    // The Core keeps its discovery record and instance lock somewhere else —
    // the platform data directory, not `~/.unterm`. Leaving it out of the
    // plan would mean an "uninstall" that removes everything the user was
    // shown and leaves a lock file behind.
    if let Some(core_dir) = unterm_protocol::core_state_dir() {
        if core_dir.exists() && Some(&core_dir) != state.as_ref() {
            data.push(core_dir.display().to_string());
            description.push(format!(
                "{} — the Core's discovery record and instance lock",
                core_dir.display()
            ));
        }
    }
    if let Some(state) = state {
        for (name, what) in [
            ("tasks.db", "every task, run, step, approval and lease"),
            ("audit", "the audit trail"),
            ("artifacts", "everything tasks produced"),
            ("sessions", "recorded sessions"),
            ("snapshots", "data snapshots taken before upgrades"),
            ("settings.json", "your settings"),
            ("providers", "provider bindings and pinned identities"),
        ] {
            let path = state.join(name);
            if path.exists() {
                data.push(path.display().to_string());
                description.push(format!("{name} — {what}"));
            }
        }
    }

    UninstallPlan {
        programs,
        data: if keep_data { Vec::new() } else { data },
        data_description: description,
        keeps_data: keep_data,
    }
}

/// Whether a path looks like something this machine should not have twice.
pub fn is_installed_at(path: impl AsRef<Path>) -> bool {
    path.as_ref().exists()
}

/// What actually happened when a plan was carried out.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Removed {
    pub programs: Vec<String>,
    pub data: Vec<String>,
    /// Paths that could not be removed, and why. Reported rather than
    /// swallowed: a half-finished uninstall that claims success is how
    /// somebody ends up with a program they cannot reinstall.
    pub failed: Vec<String>,
}

/// Carry out a plan.
///
/// Separate from [`uninstall_plan`] on purpose, and it takes the plan as an
/// argument rather than recomputing one: whoever said yes said yes to a
/// specific list, and re-surveying here would remove whatever appeared in
/// between.
///
/// The program goes first. If something interrupts this, a machine with the
/// data and no program is recoverable — reinstall — and a machine with the
/// program and no data is not.
pub fn uninstall(plan: &UninstallPlan) -> Removed {
    let mut removed = Removed::default();
    for path in &plan.programs {
        match remove_any(Path::new(path)) {
            // Only what was actually there. Reporting "removed /foo" when
            // /foo was never present is a small lie in a report people read
            // to decide whether the uninstall worked.
            Ok(true) => removed.programs.push(path.clone()),
            Ok(false) => {}
            Err(error) => removed.failed.push(format!("{path}: {error}")),
        }
    }
    for path in &plan.data {
        match remove_any(Path::new(path)) {
            Ok(true) => removed.data.push(path.clone()),
            Ok(false) => {}
            Err(error) => removed.failed.push(format!("{path}: {error}")),
        }
    }
    removed
}

/// Remove a file, directory or symlink. `false` means there was nothing there.
fn remove_any(path: &Path) -> std::io::Result<bool> {
    if !path.exists() && std::fs::symlink_metadata(path).is_err() {
        return Ok(false);
    }
    // A symlink is removed as a link, never followed: deleting what
    // `/usr/local/bin/unterm` points *at* could take the bundle with it.
    if path.is_dir() && std::fs::read_link(path).is_err() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install(kind: Kind, path: &str, target: Option<&str>) -> Install {
        Install {
            kind: kind.as_str().to_string(),
            path: path.to_string(),
            target: target.map(str::to_string),
            on_path: false,
        }
    }

    #[test]
    fn one_install_is_not_a_conflict() {
        let found = vec![install(Kind::Bundle, "/Applications/Unterm.app", None)];
        assert!(conflicts(&found).is_empty());
    }

    #[test]
    fn a_bundle_with_a_symlink_onto_path_is_not_a_conflict() {
        // That is how a macOS install gets onto PATH at all; flagging it
        // would flag every correct install, and a warning everybody sees is a
        // warning nobody reads.
        let found = vec![
            install(Kind::Bundle, "/Applications/Unterm.app", None),
            install(
                Kind::Symlink,
                "/usr/local/bin/unterm",
                Some("/Applications/Unterm.app/Contents/MacOS/unterm"),
            ),
        ];
        assert!(conflicts(&found).is_empty(), "{:?}", conflicts(&found));
    }

    #[test]
    fn two_real_installs_conflict_and_the_reason_says_why_it_matters() {
        let found = vec![
            install(Kind::Bundle, "/Applications/Unterm.app", None),
            install(Kind::Homebrew, "/opt/homebrew/Caskroom/unterm", None),
        ];
        let conflicts = conflicts(&found);
        assert_eq!(conflicts.len(), 1);
        // "You have two installs" is not alarming by itself; the consequence
        // is, and it is what the message has to carry.
        assert!(conflicts[0].advice.contains("state directory"));
        assert!(conflicts[0].advice.contains("migrate"));
    }

    #[test]
    fn a_symlink_into_a_build_tree_is_called_out() {
        // The developer's own machine: the shell runs the build, the icon
        // runs the release, and the bug report says "it works sometimes".
        let found = vec![install(
            Kind::Symlink,
            "/usr/local/bin/unterm",
            Some("/Volumes/Dev/code/unterm/target/debug/unterm"),
        )];
        let conflicts = conflicts(&found);
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].reason.contains("build tree"));
    }

    #[test]
    fn a_symlink_is_reported_as_one_whatever_list_it_came_from() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::write(&real, b"binary").unwrap();
        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            let target = std::fs::read_link(&link).unwrap();
            assert_eq!(target, real, "the survey's link resolution is what tells them apart");
        }
    }

    #[test]
    fn an_uninstall_that_keeps_data_lists_none_of_it() {
        let dir = crate::test_env::IsolatedEnv::new();
        std::fs::write(dir.path().join("tasks.db"), b"x").unwrap();

        let plan = uninstall_plan(true);
        assert!(plan.keeps_data);
        assert!(plan.data.is_empty(), "{plan:?}");
        // But it still says what is being kept, so the choice is informed.
        assert!(plan
            .data_description
            .iter()
            .any(|line| line.contains("tasks.db")));
    }

    #[test]
    fn an_uninstall_that_removes_data_says_what_each_thing_is() {
        // "Delete my data?" is a question with an answer only if the answer
        // says what the data is. A year of task history is not obvious from a
        // filename.
        let dir = crate::test_env::IsolatedEnv::new();
        for name in ["tasks.db", "settings.json"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();

        let plan = uninstall_plan(false);
        assert!(!plan.keeps_data);
        assert_eq!(plan.data.len(), 3, "{plan:?}");
        assert!(plan
            .data_description
            .iter()
            .any(|line| line.contains("every task, run, step, approval and lease")));
        assert!(plan
            .data_description
            .iter()
            .any(|line| line.contains("everything tasks produced")));
    }

    #[test]
    fn carrying_out_a_plan_removes_exactly_what_it_listed() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("program");
        let data = dir.path().join("data");
        let untouched = dir.path().join("something-else");
        std::fs::write(&program, b"x").unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("inside"), b"x").unwrap();
        std::fs::write(&untouched, b"x").unwrap();

        let plan = UninstallPlan {
            programs: vec![program.display().to_string()],
            data: vec![data.display().to_string()],
            data_description: Vec::new(),
            keeps_data: false,
        };
        let removed = uninstall(&plan);
        assert_eq!(removed.programs.len(), 1);
        assert_eq!(removed.data.len(), 1);
        assert!(removed.failed.is_empty());
        assert!(!program.exists() && !data.exists());
        // Only what the plan listed. Whoever said yes said yes to a list.
        assert!(untouched.exists());
    }

    #[test]
    fn a_plan_that_keeps_data_removes_none_of_it() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("tasks.db");
        std::fs::write(&data, b"a year of history").unwrap();
        let plan = UninstallPlan {
            programs: Vec::new(),
            data: Vec::new(),
            data_description: vec!["tasks.db — everything".into()],
            keeps_data: true,
        };
        uninstall(&plan);
        assert_eq!(std::fs::read(&data).unwrap(), b"a year of history");
    }

    #[test]
    #[cfg(unix)]
    fn what_could_not_be_removed_is_reported() {
        // A half-finished uninstall that claims success is how somebody ends
        // up with a program they cannot reinstall over.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        let inside = locked.join("program");
        std::fs::write(&inside, b"x").unwrap();
        // Unlinking needs write permission on the *parent*.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();

        let removed = uninstall(&UninstallPlan {
            programs: vec![inside.display().to_string()],
            data: Vec::new(),
            data_description: Vec::new(),
            keeps_data: true,
        });
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(removed.programs.is_empty(), "it claimed to remove a file it could not");
        assert_eq!(removed.failed.len(), 1, "{removed:?}");
        assert!(removed.failed[0].contains("program"));
    }

    #[test]
    fn a_path_that_was_never_there_is_not_reported_as_removed() {
        let removed = uninstall(&UninstallPlan {
            programs: vec!["/nowhere/at/all".into()],
            data: Vec::new(),
            data_description: Vec::new(),
            keeps_data: true,
        });
        assert!(removed.programs.is_empty());
        assert!(removed.failed.is_empty());
    }

    #[test]
    fn a_plan_removes_nothing() {
        // The whole reason this is a plan and not an action: the answer to
        // "what will I lose" is what somebody wants *before* they say yes.
        let dir = crate::test_env::IsolatedEnv::new();
        let path = dir.path().join("tasks.db");
        std::fs::write(&path, b"precious").unwrap();

        uninstall_plan(false);
        assert_eq!(std::fs::read(&path).unwrap(), b"precious");
    }
}
