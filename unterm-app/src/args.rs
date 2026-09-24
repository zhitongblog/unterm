//! What the terminal was asked to do on the command line.
//!
//! Deliberately small, and deliberately forgiving. The Explorer context menu,
//! desktop shortcuts and anything a user pinned to a taskbar all carry
//! arguments written against an older build; a terminal that refuses to open
//! because it did not recognise one of them is a terminal the user cannot get
//! back to in order to fix it.

use std::path::PathBuf;

/// How the terminal was started.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// A config file to read instead of the usual one.
    pub config: Option<PathBuf>,
    /// The directory the first shell should start in.
    pub cwd: Option<PathBuf>,
    /// Prefer handing `cwd` to an already-open window as a new tab, and
    /// only open a window of our own when nobody is there to take it.
    /// What the Explorer context menu's "Open in Unterm tab" passes.
    pub tab: bool,
    /// Identity profile to bind before the first pane is created.
    pub profile: Option<String>,
    /// Explicit program and arguments for the first pane.
    pub command: Vec<String>,
    /// Be the administrator window: elevated, alone, and unreachable. See
    /// `admin`.
    pub admin: bool,
    /// Arguments that meant nothing here, kept so they can be reported.
    pub unrecognised: Vec<String>,
}

/// Parse what was passed after the program name.
///
/// Accepts what the installer's shortcuts have always passed -- a leading
/// `start` verb, `--cwd <dir>` -- alongside the current spelling, because
/// those shortcuts outlive any one build and are exactly what a user has
/// pinned.
pub fn parse<I: IntoIterator<Item = String>>(arguments: I) -> Args {
    let mut args = Args::default();
    let mut rest = arguments.into_iter().peekable();

    while let Some(argument) = rest.next() {
        match argument.as_str() {
            // The verb the old binary used. There is only one thing this
            // program does, so it means "and nothing else".
            "start" => {}
            "--tab" => args.tab = true,
            "--admin" => args.admin = true,
            "--config" | "-c" => args.config = rest.next().map(PathBuf::from),
            "--cwd" => args.cwd = rest.next().map(PathBuf::from),
            "--profile" => args.profile = rest.next(),
            "--" => {
                args.command.extend(rest);
                break;
            }
            other if other.starts_with("--cwd=") => {
                args.cwd = Some(PathBuf::from(&other["--cwd=".len()..]));
            }
            other if other.starts_with("--config=") => {
                args.config = Some(PathBuf::from(&other["--config=".len()..]));
            }
            other if other.starts_with("--profile=") => {
                args.profile = Some(other["--profile=".len()..].to_string());
            }
            // A bare path: a config file if it looks like one, otherwise a
            // directory to start in. This is what a file manager hands over
            // when someone drops something on the icon.
            other if !other.starts_with('-') => {
                let path = PathBuf::from(other);
                if path.is_dir() {
                    args.cwd = Some(path);
                } else if args.config.is_none() {
                    args.config = Some(path);
                } else {
                    args.unrecognised.push(other.to_string());
                }
            }
            other => args.unrecognised.push(other.to_string()),
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(arguments: &[&str]) -> Args {
        parse(arguments.iter().map(|s| s.to_string()))
    }

    #[test]
    fn nothing_passed_means_every_default() {
        let args = parse_args(&[]);
        assert!(args.config.is_none());
        assert!(args.cwd.is_none());
        assert!(args.profile.is_none());
        assert!(args.command.is_empty());
        assert!(args.unrecognised.is_empty());
    }

    #[test]
    fn the_installers_context_menu_still_works() {
        // What the shell integration has always passed. A pinned shortcut
        // outlives any one build.
        let args = parse_args(&["start", "--cwd", "D:\\projects"]);
        assert_eq!(args.cwd, Some(PathBuf::from("D:\\projects")));
        assert!(args.unrecognised.is_empty());
    }

    #[test]
    fn both_spellings_of_an_option_are_understood() {
        assert_eq!(
            parse_args(&["--cwd=D:\\a"]).cwd,
            Some(PathBuf::from("D:\\a"))
        );
        assert_eq!(
            parse_args(&["--cwd", "D:\\a"]).cwd,
            Some(PathBuf::from("D:\\a"))
        );
        assert_eq!(
            parse_args(&["--config=x.conf"]).config,
            Some(PathBuf::from("x.conf"))
        );
    }

    #[test]
    fn an_unknown_option_is_reported_rather_than_fatal() {
        // A terminal that will not open because of an argument is one the
        // user cannot get back to in order to remove it.
        let args = parse_args(&["--enable-warp-drive", "--cwd", "D:\\a"]);
        assert_eq!(args.unrecognised, ["--enable-warp-drive"]);
        assert_eq!(args.cwd, Some(PathBuf::from("D:\\a")));
    }

    #[test]
    fn a_bare_file_is_taken_as_a_config() {
        let args = parse_args(&["my.conf"]);
        assert_eq!(args.config, Some(PathBuf::from("my.conf")));
    }

    #[test]
    fn a_bare_directory_is_taken_as_a_place_to_start() {
        // What a file manager hands over when something is dropped on the
        // icon. The current directory is one every machine has.
        let args = parse_args(&["."]);
        assert_eq!(args.cwd, Some(PathBuf::from(".")));
        assert!(args.config.is_none());
    }

    #[test]
    fn an_option_missing_its_value_does_not_swallow_the_next_one() {
        let args = parse_args(&["--cwd"]);
        assert!(args.cwd.is_none());
        assert!(args.unrecognised.is_empty());
    }

    #[test]
    fn profile_and_explicit_command_are_preserved_for_startup() {
        let args = parse_args(&[
            "start",
            "--profile",
            "work",
            "--",
            "python",
            "-c",
            "print('ready')",
        ]);
        assert_eq!(args.profile.as_deref(), Some("work"));
        assert_eq!(args.command, ["python", "-c", "print('ready')"]);
        assert!(args.unrecognised.is_empty());
    }
}
