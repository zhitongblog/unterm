//! Shell integration, injected when a shell is launched.
//!
//! Prompt and command marks (`OSC 133`) are what let a terminal tell a
//! prompt from output, a finished command from a running one, and a failed
//! command from a successful one -- the thing an agent driving a shell most
//! needs to know. Shells only send them when a script tells them to, and a
//! script the user has to install by hand is a script almost nobody has.
//! So the shells Unterm starts get one, the way kitty and Ghostty do it:
//! without touching the user's own files, and with their startup files read
//! exactly as before.
//!
//! The scripts live in this crate and are written to the state directory on
//! first use, so every package -- installed, portable, a development build --
//! carries the same ones.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const ZSHENV: &str = include_str!("../../assets/shell-integration/zshenv");
const ZSH: &str = include_str!("../../assets/shell-integration/unterm.zsh");
const BASH: &str = include_str!("../../assets/shell-integration/unterm.bash");
const FISH: &str = include_str!("../../assets/shell-integration/unterm.fish");
const POWERSHELL: &str = include_str!("../../assets/shell-integration/unterm.ps1");

static ENABLED: AtomicBool = AtomicBool::new(true);

/// `shell_integration = false` in the config turns injection off.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Injection off until the guard is dropped, for tests about something else.
#[cfg(test)]
pub(crate) fn disabled_for_test() -> impl Drop {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            set_enabled(self.0);
        }
    }
    let was = enabled();
    set_enabled(false);
    Restore(was)
}

/// Which shell a program name is, for the ones that get a script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
    PowerShell,
}

pub fn shell_of(program: &str) -> Option<Shell> {
    // Split on both separators by hand: a Windows path read on any platform,
    // or a Unix one on Windows, must still name its program.
    let name = program
        .trim_start_matches('-')
        .rsplit(|c| c == '/' || c == '\\')
        .next()?
        .to_ascii_lowercase();
    let stem = name.strip_suffix(".exe").unwrap_or(&name);
    match stem {
        "zsh" => Some(Shell::Zsh),
        "bash" => Some(Shell::Bash),
        "fish" => Some(Shell::Fish),
        "pwsh" | "powershell" => Some(Shell::PowerShell),
        _ => None,
    }
}

/// Write the scripts where shells can read them, once per process, and say
/// where that is. Rewritten only when the content differs, so a newer build
/// replaces an older one's scripts and an unchanged one costs a read.
fn installed_dir() -> Option<PathBuf> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let root = unterm_protocol::state_path("shell-integration")?;
        let files: [(&str, &str); 5] = [
            ("zsh/.zshenv", ZSHENV),
            ("unterm.zsh", ZSH),
            ("unterm.bash", BASH),
            ("fish/fish/vendor_conf.d/unterm.fish", FISH),
            ("unterm.ps1", POWERSHELL),
        ];
        for (relative, content) in files {
            let path = root.join(relative);
            if std::fs::read_to_string(&path).ok().as_deref() == Some(content) {
                continue;
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok()?;
            }
            std::fs::write(&path, content).ok()?;
        }
        Some(root)
    })
    .clone()
}

fn slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Rewrite a launch so the shell loads Unterm's integration.
///
/// Anything unusual is left as it was: a shell with arguments of its own
/// (`bash -c …`, `pwsh -File …`) is running something specific, and a
/// program that is not one of the four gets nothing.
pub(super) fn apply(command: &mut portable_pty::CommandBuilder) {
    if !enabled() {
        return;
    }
    let default = command.is_default_prog();
    let program = if default {
        if cfg!(windows) {
            return;
        }
        command.get_shell()
    } else {
        match command.get_argv().first() {
            Some(program) => program.to_string_lossy().into_owned(),
            None => return,
        }
    };
    let Some(shell) = shell_of(&program) else {
        return;
    };
    let args: Vec<String> = command
        .get_argv()
        .iter()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let Some(dir) = installed_dir() else {
        return;
    };
    match shell {
        Shell::Zsh => {
            let only_login_flags = args.iter().all(|arg| arg == "-l" || arg == "--login" || arg == "-i");
            if !only_login_flags {
                return;
            }
            if let Some(existing) = env_value(command, "ZDOTDIR") {
                command.env("UNTERM_ORIG_ZDOTDIR", existing);
            }
            command.env("ZDOTDIR", dir.join("zsh"));
            command.env("UNTERM_SHELL_INTEGRATION_DIR", &dir);
        }
        Shell::Bash => {
            let login = default || args.iter().any(|arg| arg == "-l" || arg == "--login");
            let only_flags = args.iter().all(|arg| arg == "-l" || arg == "--login" || arg == "-i");
            if !only_flags {
                return;
            }
            let mut rebuilt = portable_pty::CommandBuilder::new(&program);
            rebuilt.arg("--rcfile");
            rebuilt.arg(slashes(&dir.join("unterm.bash")));
            rebuilt.arg("-i");
            carry_over(command, &mut rebuilt);
            if login {
                rebuilt.env("UNTERM_BASH_LOGIN", "1");
            }
            *command = rebuilt;
        }
        Shell::Fish => {
            let data = dir.join("fish");
            let existing = env_value(command, "XDG_DATA_DIRS")
                .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
            command.env("XDG_DATA_DIRS", format!("{}:{existing}", data.display()));
        }
        Shell::PowerShell => {
            let runs_something = args.iter().any(|arg| {
                let arg = arg.to_ascii_lowercase();
                matches!(
                    arg.as_str(),
                    "-command" | "-c" | "-file" | "-f" | "-encodedcommand" | "-e" | "-ec"
                ) || arg.starts_with("-command") || arg.starts_with("-file")
            });
            if runs_something {
                return;
            }
            let script = dir.join("unterm.ps1");
            if !args.iter().any(|arg| arg.eq_ignore_ascii_case("-noexit")) {
                command.arg("-NoExit");
            }
            // Read as text and run as a script block, not dot-sourced as a
            // file: Windows' default execution policy (Restricted) refuses
            // to run a .ps1, which printed a red UnauthorizedAccess error at
            // the top of every new tab and left the tab unmarked. A policy
            // is about script files; this leaves the user's policy as it is.
            command.arg("-Command");
            command.arg(format!(
                "try {{ . ([ScriptBlock]::Create([IO.File]::ReadAllText('{}'))) }} catch {{ }}",
                script.display().to_string().replace('\'', "''")
            ));
        }
    }
}

/// A variable as the shell will see it: set on the launch, or inherited.
fn env_value(command: &portable_pty::CommandBuilder, key: &str) -> Option<String> {
    command
        .get_env(key)
        .map(|value| value.to_string_lossy().into_owned())
        .or_else(|| std::env::var(key).ok())
        .filter(|value| !value.is_empty())
}

/// Everything a launch carried besides its argv: directory and environment.
fn carry_over(from: &portable_pty::CommandBuilder, to: &mut portable_pty::CommandBuilder) {
    if let Some(cwd) = from.get_cwd() {
        to.cwd(cwd);
    }
    for (key, value) in from.iter_extra_env_as_str() {
        to.env(key, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shells_are_known_by_name_on_every_platform() {
        assert_eq!(shell_of("/bin/zsh"), Some(Shell::Zsh));
        assert_eq!(shell_of("-zsh"), Some(Shell::Zsh));
        assert_eq!(shell_of("C:\\Program Files\\Git\\bin\\bash.exe"), Some(Shell::Bash));
        assert_eq!(shell_of("/opt/homebrew/bin/fish"), Some(Shell::Fish));
        assert_eq!(shell_of("pwsh.exe"), Some(Shell::PowerShell));
        assert_eq!(shell_of("powershell"), Some(Shell::PowerShell));
        assert_eq!(shell_of("cmd.exe"), None);
        assert_eq!(shell_of("/usr/bin/python3"), None);
    }

    #[test]
    fn a_shell_running_something_specific_is_left_alone() {
        let mut command = portable_pty::CommandBuilder::new("bash");
        command.arg("-c");
        command.arg("make test");
        apply(&mut command);
        let argv: Vec<String> = command
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv, vec!["bash", "-c", "make test"]);

        let mut command = portable_pty::CommandBuilder::new("pwsh");
        command.arg("-File");
        command.arg("build.ps1");
        apply(&mut command);
        assert_eq!(command.get_argv().len(), 3);
    }

    /// A default login bash becomes `bash --rcfile … -i` that still reads
    /// the login files: the rcfile is told so through the environment.
    #[cfg(unix)]
    #[test]
    fn a_default_login_bash_keeps_its_login_files() {
        let mut command = portable_pty::CommandBuilder::new_default_prog();
        command.env("SHELL", "/bin/bash");
        command.env("KEEP_ME", "1");
        apply(&mut command);
        let argv: Vec<String> = command
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv.first().map(String::as_str), Some("/bin/bash"), "{:?}", argv);
        assert!(argv.iter().any(|arg| arg == "--rcfile"), "{:?}", argv);
        assert_eq!(command.get_env("UNTERM_BASH_LOGIN").and_then(|v| v.to_str()), Some("1"));
        assert_eq!(command.get_env("KEEP_ME").and_then(|v| v.to_str()), Some("1"));
    }

    /// PowerShell gets the script as text, never `. 'file.ps1'`: the default
    /// execution policy on Windows refuses script files.
    #[test]
    fn powershell_loads_the_script_whatever_the_execution_policy() {
        let mut command = portable_pty::CommandBuilder::new("powershell.exe");
        command.arg("-NoLogo");
        apply(&mut command);
        let argv: Vec<String> = command
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&argv[..3], ["powershell.exe", "-NoLogo", "-NoExit"], "{:?}", argv);
        assert_eq!(argv[3], "-Command");
        let load = &argv[4];
        assert!(load.starts_with("try { . ([ScriptBlock]::Create([IO.File]::ReadAllText('"), "{}", load);
        assert!(load.contains("unterm.ps1"), "{}", load);
        assert!(!load.starts_with(". '"), "{}", load);
    }

    #[test]
    fn the_scripts_say_what_they_mark() {
        for script in [ZSH, BASH, FISH, POWERSHELL] {
            assert!(script.contains("133;A"), "prompt mark missing");
            assert!(script.contains("133;D"), "command-end mark missing");
        }
        assert!(ZSHENV.contains("UNTERM_ORIG_ZDOTDIR"));
    }
}
