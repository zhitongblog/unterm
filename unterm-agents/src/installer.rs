//! Install + detect + uninstall + update runners.
//!
//! Every shell command we run from a manifest passes through here. We
//! deliberately do NOT use `sh -c` to interpret the steps — install
//! commands are encoded as `Vec<String>` argv arrays, so manifests can't
//! sneak in shell metacharacters or environment-variable lookups that
//! weren't intended by the author. This narrows the blast radius of a
//! compromised manifest signing key + bad envelope to exactly what the
//! argv array spells out.

use crate::errors::{AgentError, Result};
use crate::manifest::{AgentManifest, DetectSpec, InstallStep, PlatformInstall};
use sha2::{Digest, Sha256};
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct DetectOutcome {
    /// `true` if the binary was found AND (if `min_version` set) the version
    /// passes the check.
    pub ok: bool,
    pub version: Option<String>,
    pub binary_path: Option<String>,
    /// `true` if the resolved binary's directory is already on the current
    /// `$PATH` AND persisted in the user's shell RC files (when inside `$HOME`).
    pub on_path: bool,
    pub stderr_sample: Option<String>,
}

/// Run the detect command and return a structured outcome. Never errors —
/// failure to detect is just `ok: false`.
/// Build a `Command` that can actually execute `resolved` on this platform.
/// On Windows a `.cmd`/`.bat` is a batch script that `CreateProcess` (and so
/// `Command::new`) cannot launch directly — it must go through `cmd /c`; a
/// `.ps1` goes through PowerShell. Native `.exe` and Unix binaries run as-is.
/// This is what lets npm-installed CLIs (`codex.cmd`, `gemini.cmd`) and `npm`
/// itself (`npm.cmd`) be detected and run from the GUI.
pub(crate) fn spawn_command(resolved: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let lower = resolved.to_ascii_lowercase();
        let mut c = if lower.ends_with(".cmd") || lower.ends_with(".bat") {
            let mut c = Command::new("cmd");
            c.arg("/c").arg(resolved);
            c
        } else if lower.ends_with(".ps1") {
            let mut c = Command::new("powershell");
            c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                .arg(resolved);
            c
        } else {
            Command::new(resolved)
        };
        c.creation_flags(CREATE_NO_WINDOW); // no console flash
        return c;
    }
    #[cfg(not(windows))]
    Command::new(resolved)
}

pub fn detect(spec: &DetectSpec) -> DetectOutcome {
    let binary_path = which(&spec.command);
    let Some(resolved) = binary_path.clone() else {
        return DetectOutcome {
            ok: false,
            version: None,
            binary_path: None,
            on_path: false,
            stderr_sample: None,
        };
    };
    let on_path = is_bin_on_user_path(&resolved);

    // Spawn the resolved absolute path, not just `spec.command`. which()'s
    // fallback may have found the binary in ~/.local/bin (etc.) even when
    // $PATH inherited by this process doesn't list that dir; running
    // `Command::new("claude")` in that case would fail to spawn at all,
    // and we'd report `installed=false` despite knowing where the binary
    // lives. Resolve once, exec once.
    let mut cmd = spawn_command(&resolved);
    cmd.env("PATH", enriched_path());
    for a in &spec.version_args {
        cmd.arg(a);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            return DetectOutcome {
                ok: false,
                version: None,
                binary_path,
                on_path,
                stderr_sample: Some(e.to_string()),
            }
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let version = if let Some(re) = &spec.version_regex {
        regex_first_capture(re, &stdout).or_else(|| regex_first_capture(re, &stderr))
    } else {
        stdout
            .lines()
            .next()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };

    let ok = match (&version, &spec.min_version) {
        (Some(have), Some(need)) => crate::envelope::compare_versions(have, need) >= 0,
        _ => version.is_some() || output.status.success(),
    };

    DetectOutcome {
        ok,
        version,
        binary_path,
        on_path,
        stderr_sample: if stderr.is_empty() {
            None
        } else {
            Some(stderr.chars().take(500).collect())
        },
    }
}

fn regex_first_capture(pat: &str, hay: &str) -> Option<String> {
    // Tiny dependency-free regex isn't on the workspace; reuse fancy-regex
    // would pull a lot in. Manifest authors mostly need "^([0-9.]+)" style
    // patterns — handle that subset by hand: find first run of digits/dots.
    if pat.contains("([0-9.]") {
        let mut started = false;
        let mut buf = String::new();
        for c in hay.chars() {
            if c.is_ascii_digit() || c == '.' {
                buf.push(c);
                started = true;
            } else if started {
                break;
            }
        }
        if buf.is_empty() {
            return None;
        }
        return Some(buf);
    }
    // Fall back to taking the first whitespace-delimited token.
    hay.split_whitespace().next().map(|s| s.to_string())
}

/// Directories where user-installed CLIs commonly live but that macOS GUI
/// processes don't get on `$PATH`. launchd hands an app launched from Finder /
/// Launchpad / Dock a minimal `/usr/bin:/bin:/usr/sbin:/sbin`, so `npm` /
/// `pipx` / `node` (Homebrew, nvm, the user's `~/.local/bin`, …) are invisible
/// — both for *detecting* a binary and for *spawning* an install step.
/// The user's *actual* npm global prefix(es), resolved from the environment
/// and `~/.npmrc` rather than assumed. npm lets users relocate the global
/// prefix (`prefix=...` in `.npmrc`, or `npm_config_prefix`), and on Windows
/// the value is often a drive-relative POSIX path like `/home/alex/.npm-global`
/// that resolves to `D:\home\alex\.npm-global`. Global `.cmd` shims live
/// directly in this dir, so it must be on the search path for detection +
/// install. Cached — `.npmrc` is read once.
#[cfg(windows)]
fn windows_npm_prefixes() -> &'static [std::path::PathBuf] {
    use std::path::PathBuf;
    use std::sync::OnceLock;
    static PREFIXES: OnceLock<Vec<PathBuf>> = OnceLock::new();
    PREFIXES.get_or_init(|| {
        let mut out: Vec<PathBuf> = Vec::new();
        for k in ["npm_config_prefix", "NPM_CONFIG_PREFIX"] {
            if let Some(v) = std::env::var_os(k) {
                let p = PathBuf::from(v);
                if !p.as_os_str().is_empty() {
                    out.push(p);
                }
            }
        }
        if let Some(home) = dirs_next::home_dir() {
            if let Ok(rc) = std::fs::read_to_string(home.join(".npmrc")) {
                for line in rc.lines() {
                    let line = line.trim();
                    if line.starts_with('#') || line.starts_with(';') {
                        continue;
                    }
                    if let Some(rest) = line.strip_prefix("prefix") {
                        let val = rest
                            .trim_start()
                            .trim_start_matches('=')
                            .trim()
                            .trim_matches('"');
                        if val.is_empty() {
                            continue;
                        }
                        let p = PathBuf::from(val);
                        if p.is_absolute() {
                            out.push(p);
                        } else if val.starts_with('/') {
                            // Drive-relative POSIX path (e.g. /home/alex/...):
                            // try each drive and keep the one that exists.
                            for d in 'A'..='Z' {
                                let cand = PathBuf::from(format!("{d}:{val}"));
                                if cand.exists() {
                                    out.push(cand);
                                }
                            }
                        }
                    }
                }
            }
        }
        out
    })
}

fn extra_path_dirs() -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let mut dirs: Vec<PathBuf> = Vec::new();
    for k in ["npm_config_prefix", "NPM_CONFIG_PREFIX"] {
        if let Some(v) = std::env::var_os(k) {
            let p = PathBuf::from(v);
            if !p.as_os_str().is_empty() {
                #[cfg(unix)]
                dirs.push(p.join("bin"));
                dirs.push(p);
            }
        }
    }
    if let Some(pnpm) = std::env::var_os("PNPM_HOME") {
        let p = PathBuf::from(pnpm);
        if !p.as_os_str().is_empty() {
            dirs.push(p);
        }
    }
    if let Some(home) = dirs_next::home_dir() {
        for sub in [
            ".local/bin",
            ".npm-global/bin",
            ".cargo/bin",
            ".kimi/bin",
            ".antigravity/bin",
            ".pyenv/shims",
            ".rye/shims",
            ".bun/bin",
            ".deno/bin",
            ".volta/bin",
            "Library/Python/3.13/bin",
            "Library/Python/3.12/bin",
            "Library/Python/3.11/bin",
            "Library/pnpm",
            ".local/share/pnpm",
        ] {
            dirs.push(home.join(sub));
        }
        #[cfg(unix)]
        if let Ok(rc) = std::fs::read_to_string(home.join(".npmrc")) {
            for line in rc.lines() {
                let line = line.trim();
                if line.starts_with('#') || line.starts_with(';') {
                    continue;
                }
                if let Some(rest) = line.strip_prefix("prefix") {
                    let val = rest
                        .trim_start()
                        .trim_start_matches('=')
                        .trim()
                        .trim_matches('"');
                    if !val.is_empty() {
                        let expanded = if let Some(stripped) = val.strip_prefix("~/") {
                            home.join(stripped)
                        } else if let Some(stripped) = val.strip_prefix("$HOME/") {
                            home.join(stripped)
                        } else {
                            PathBuf::from(val)
                        };
                        dirs.push(expanded.join("bin"));
                    }
                }
            }
        }
        // Version managers put node/npm under a per-version bin dir that isn't
        // a fixed path; glob the common ones so nvm / fnm users get the same
        // GUI-launch fallback. (nvm: <ver>/bin · fnm: <ver>/installation/bin)
        for vm in [".nvm/versions/node", ".local/share/fnm/node-versions"] {
            if let Ok(entries) = std::fs::read_dir(home.join(vm)) {
                for e in entries.flatten() {
                    dirs.push(e.path().join("bin"));
                    dirs.push(e.path().join("installation").join("bin"));
                }
            }
        }
    }
    #[cfg(unix)]
    for sys in ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"] {
        dirs.push(PathBuf::from(sys));
    }

    // Windows package managers don't follow any of the Unix conventions above.
    // The single biggest miss: npm's global prefix is %APPDATA%\npm, where
    // `npm install -g @openai/codex` drops `codex.cmd`. An Explorer-launched
    // GUI usually has a narrower PATH than the user's shell, so without these
    // a CLI the user installed from a terminal reads as "not installed" — and
    // install steps fail because even `npm` itself isn't found.
    #[cfg(windows)]
    {
        // The user's real npm prefix (from .npmrc / env) — this is where a
        // relocated global install actually lives.
        dirs.extend(windows_npm_prefixes().iter().cloned());
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(&appdata).join("npm")); // default npm -g (codex/claude/...)
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let local = PathBuf::from(&local);
            dirs.push(local.join("agy").join("bin")); // Antigravity CLI (agy.exe)
            dirs.push(local.join("kimi").join("bin")); // Kimi Code CLI
            dirs.push(local.join("pnpm")); // pnpm global
            dirs.push(local.join("Microsoft").join("WinGet").join("Links")); // winget shims
                                                                             // CPython installs (PythonXY\python.exe + PythonXY\Scripts\pip.exe);
                                                                             // glob the version dirs so pip/pip-installed CLIs are found.
            if let Ok(entries) = std::fs::read_dir(local.join("Programs").join("Python")) {
                for e in entries.flatten() {
                    dirs.push(e.path()); // python.exe
                    dirs.push(e.path().join("Scripts")); // pip / console scripts
                }
            }
        }
        // pip `--user` and pipx land in %APPDATA%\Python\PythonXY\Scripts.
        if let Some(appdata) = std::env::var_os("APPDATA") {
            if let Ok(entries) = std::fs::read_dir(PathBuf::from(&appdata).join("Python")) {
                for e in entries.flatten() {
                    dirs.push(e.path().join("Scripts"));
                }
            }
        }
        if let Some(home) = dirs_next::home_dir() {
            dirs.push(home.join(".local").join("bin")); // uv / pipx / kimi on Windows
            dirs.push(home.join("scoop").join("shims")); // scoop
            dirs.push(home.join(".bun").join("bin")); // bun
            dirs.push(home.join(".deno").join("bin")); // deno
            dirs.push(home.join(".cargo").join("bin")); // cargo (also added above, harmless dup)
        }
        // node / npm themselves, so `npm install -g ...` can even run.
        for p in [r"C:\Program Files\nodejs", r"C:\Program Files (x86)\nodejs"] {
            dirs.push(PathBuf::from(p));
        }
    }

    dirs
}

/// `$PATH` as inherited, enriched with [`extra_path_dirs`] and deduped. A child
/// we spawn (npm, pipx, or the resolved agent binary) gets this so it can find
/// its own sub-tools (e.g. `npm` finding `node`) even when this process was
/// started by launchd with the bare GUI PATH.
pub(crate) fn enriched_path() -> std::ffi::OsString {
    use std::collections::HashSet;
    let mut seen: HashSet<std::path::PathBuf> = HashSet::new();
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path) {
            if seen.insert(d.clone()) {
                dirs.push(d);
            }
        }
    }
    for d in extra_path_dirs() {
        if seen.insert(d.clone()) {
            dirs.push(d);
        }
    }
    std::env::join_paths(dirs).unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
}

pub(crate) fn which(bin: &str) -> Option<String> {
    if bin.contains('/') {
        return std::path::Path::new(bin).exists().then(|| bin.to_string());
    }
    // Primary lookup: $PATH as inherited from the parent process.
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if let Some(found) = check_dir(&dir, bin) {
                return Some(found);
            }
        }
    }
    // Fallback lookup: the GUI-unreachable dirs (see extra_path_dirs()).
    // Without this, an agent like Claude Code installed via `npm install -g`
    // shows as "not installed" in the AI Agents panel even though `which
    // claude` works from a shell.
    for dir in extra_path_dirs() {
        if let Some(found) = check_dir(&dir, bin) {
            return Some(found);
        }
    }
    None
}

fn check_dir(dir: &std::path::Path, bin: &str) -> Option<String> {
    // On Windows, PREFER the executable wrapper over the bare file. `npm i -g`
    // drops both `codex` (a POSIX shebang script that CreateProcess — and thus
    // `Command::new` — cannot execute) and `codex.cmd`/`codex.exe`/`codex.ps1`.
    // Returning the bare script made detection and install spawns fail; the
    // `.cmd`/`.exe` actually runs.
    #[cfg(windows)]
    for ext in ["cmd", "exe", "bat", "ps1"] {
        let p = dir.join(format!("{bin}.{ext}"));
        if p.is_file() {
            return Some(p.to_string_lossy().to_string());
        }
    }
    let p = dir.join(bin);
    if p.is_file() {
        return Some(p.to_string_lossy().to_string());
    }
    None
}

#[derive(Debug, Clone)]
pub struct StepReport {
    pub label: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout_tail: String,
    pub stderr_tail: String,
}

/// If an install plan invokes `pipx` but the machine doesn't have it, install
/// pipx for the user via `python -m pip install --user pipx`. The pipx shim
/// then lands in `%APPDATA%\Python\PythonXY\Scripts` (covered by
/// [`extra_path_dirs`]), so the subsequent `pipx install <app>` step resolves.
fn ensure_pipx_if_needed(platform: &PlatformInstall) {
    let needs_pipx = platform.steps.iter().any(|s| {
        matches!(s, InstallStep::Shell { cmd } if cmd.first().map(|c| c == "pipx").unwrap_or(false))
    });
    if !needs_pipx || which("pipx").is_some() {
        return;
    }
    let Some(py) = which("python")
        .or_else(|| which("python3"))
        .or_else(|| which("py"))
    else {
        return; // no Python either — let the pipx step report it
    };
    let mut c = spawn_command(&py);
    c.env("PATH", enriched_path())
        .args(["-m", "pip", "install", "--user", "pipx"]);
    let _ = c.output();
}

/// If an install plan invokes `uv` (e.g. `trae-agent`) and `uv` is not yet
/// installed, bootstrap it via `python -m pip install --user uv` or the
/// official Astral installer (`~/.local/bin/uv`).
fn ensure_uv_if_needed(platform: &PlatformInstall) {
    let needs_uv = platform.steps.iter().any(|s| {
        matches!(s, InstallStep::Shell { cmd } if cmd.first().map(|c| c == "uv").unwrap_or(false))
    });
    if !needs_uv || which("uv").is_some() {
        return;
    }
    if let Some(py) = which("python")
        .or_else(|| which("python3"))
        .or_else(|| which("py"))
    {
        let mut c = spawn_command(&py);
        c.env("PATH", enriched_path())
            .args(["-m", "pip", "install", "--user", "uv"]);
        let _ = c.output();
        if which("uv").is_some() {
            return;
        }
    }
    #[cfg(unix)]
    if let Some(sh) = which("sh").or_else(|| which("bash")) {
        let mut c = spawn_command(&sh);
        c.env("PATH", enriched_path())
            .args(["-c", "curl -LsSf https://astral.sh/uv/install.sh | sh"]);
        let _ = c.output();
    }
    #[cfg(windows)]
    {
        let mut c = spawn_command("powershell");
        c.env("PATH", enriched_path()).args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "irm https://astral.sh/uv/install.ps1 | iex",
        ]);
        let _ = c.output();
    }
}

/// Ensure `bin_dir` is on the current process's `$PATH` AND persisted in the
/// user's shell startup files (`.zshrc` / `.bashrc` / `.bash_profile` /
/// `config.fish` on Unix, or `HKCU\Environment\Path` on Windows).
///
/// Returns the list of shell config files (or registry entries) that were
/// modified or verified.
pub fn ensure_dir_on_user_path(bin_dir: &std::path::Path) -> Vec<String> {
    // 1. Prepend to current process PATH if not already present so subsequent
    // spawns in this Unterm session find the newly-installed binary immediately.
    let current_path = std::env::var_os("PATH").unwrap_or_default();
    let already_in_env = std::env::split_paths(&current_path).any(|p| p == bin_dir);
    if !already_in_env {
        let mut dirs = vec![bin_dir.to_path_buf()];
        dirs.extend(std::env::split_paths(&current_path));
        if let Ok(joined) = std::env::join_paths(dirs) {
            std::env::set_var("PATH", joined);
        }
    }

    #[cfg(unix)]
    {
        let Some(home) = dirs_next::home_dir() else {
            return Vec::new();
        };
        let shell = std::env::var("SHELL").ok();
        return ensure_dir_on_user_path_in_home(bin_dir, &home, shell.as_deref());
    }

    #[cfg(windows)]
    {
        return ensure_dir_on_windows_user_path(bin_dir);
    }
}

#[cfg(windows)]
fn ensure_dir_on_windows_user_path(bin_dir: &std::path::Path) -> Vec<String> {
    let dir_str = bin_dir.to_string_lossy().to_string();
    let script = format!(
        "$dir = '{}'; \
         $u = [Environment]::GetEnvironmentVariable('Path', 'User'); \
         $m = [Environment]::GetEnvironmentVariable('Path', 'Machine'); \
         $all = (($u + ';' + $m) -split ';') | ForEach-Object {{ $_.Trim().TrimEnd('\\') }}; \
         $norm = $dir.Trim().TrimEnd('\\'); \
         if ($all -notcontains $norm) {{ \
             $newU = if ([string]::IsNullOrWhiteSpace($u)) {{ $dir }} else {{ $u.TrimEnd(';') + ';' + $dir }}; \
             [Environment]::SetEnvironmentVariable('Path', $newU, 'User'); \
             Write-Output 'updated:HKCU\\Environment\\Path' \
         }}",
        dir_str.replace('\'', "''")
    );
    let mut cmd = spawn_command("powershell");
    cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &script]);
    if let Ok(out) = cmd.output() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.contains("updated:HKCU\\Environment\\Path") {
            return vec!["HKCU\\Environment\\Path".to_string()];
        }
    }
    Vec::new()
}

/// Testable Unix helper that writes the idempotent PATH export snippet to the
/// user's shell RC files under `home` when `bin_dir` is not already present.
pub fn ensure_dir_on_user_path_in_home(
    bin_dir: &std::path::Path,
    home: &std::path::Path,
    shell: Option<&str>,
) -> Vec<String> {
    let dir_raw = bin_dir.to_string_lossy().to_string();
    // Standard system directories are already on every login shell's default PATH.
    if matches!(
        dir_raw.as_str(),
        "/usr/bin" | "/bin" | "/usr/sbin" | "/sbin"
    ) {
        return Vec::new();
    }

    // Use `$HOME/...` when `bin_dir` lives inside `home` so the shell snippet
    // remains portable across home directory moves.
    let (posix_expr, rel_sub) = match bin_dir.strip_prefix(home) {
        Ok(rel) if !rel.as_os_str().is_empty() => {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            (format!("$HOME/{rel_str}"), Some(rel_str))
        }
        _ => (dir_raw.clone(), None),
    };

    let shell_name = shell.unwrap_or("");
    let mut posix_targets = Vec::new();
    let zshrc = home.join(".zshrc");
    if shell_name.contains("zsh") || zshrc.exists() || cfg!(target_os = "macos") {
        posix_targets.push(zshrc);
    }
    let bashrc = home.join(".bashrc");
    if shell_name.contains("bash") || bashrc.exists() {
        posix_targets.push(bashrc);
    }
    let bash_profile = home.join(".bash_profile");
    if bash_profile.exists() {
        posix_targets.push(bash_profile);
    }
    if posix_targets.is_empty() {
        posix_targets.push(home.join(".profile"));
    }

    let mut updated = Vec::new();
    let already_mentions = |content: &str| -> bool {
        if content.contains(&dir_raw) || content.contains(&posix_expr) {
            return true;
        }
        if let Some(rel) = &rel_sub {
            if content.contains(&format!("~/{rel}")) || content.contains(&format!("${{HOME}}/{rel}")) {
                return true;
            }
        }
        false
    };

    let posix_snippet = format!(
        "\n# Added by Unterm (AI agent PATH)\ncase \":$PATH:\" in\n  *\":{posix_expr}:\"*) ;;\n  *) export PATH=\"{posix_expr}:$PATH\" ;;\nesac\n"
    );

    for rc in posix_targets {
        let existing = std::fs::read_to_string(&rc).unwrap_or_default();
        if already_mentions(&existing) {
            continue;
        }
        let mut next = existing;
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str(&posix_snippet);
        if std::fs::write(&rc, next).is_ok() {
            updated.push(rc.to_string_lossy().to_string());
        }
    }

    let fish_cfg = home.join(".config").join("fish").join("config.fish");
    if shell_name.contains("fish") || fish_cfg.exists() {
        let existing = std::fs::read_to_string(&fish_cfg).unwrap_or_default();
        if !already_mentions(&existing) {
            if let Some(parent) = fish_cfg.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let mut next = existing;
            if !next.is_empty() && !next.ends_with('\n') {
                next.push('\n');
            }
            next.push_str(&format!(
                "\n# Added by Unterm (AI agent PATH)\nfish_add_path -g \"{posix_expr}\"\n"
            ));
            if std::fs::write(&fish_cfg, next).is_ok() {
                updated.push(fish_cfg.to_string_lossy().to_string());
            }
        }
    }

    updated
}

/// Check whether `resolved_bin` is already on the current process's `$PATH`
/// AND (for user-local directories under `$HOME` on Unix) mentioned in at
/// least one of the user's shell RC files.
pub fn is_bin_on_user_path(resolved_bin: &str) -> bool {
    let Some(bin_dir) = std::path::Path::new(resolved_bin).parent() else {
        return false;
    };
    let dir_raw = bin_dir.to_string_lossy();
    if matches!(
        dir_raw.as_ref(),
        "/usr/bin" | "/bin" | "/usr/sbin" | "/sbin"
    ) {
        return true;
    }

    let current_path = std::env::var_os("PATH").unwrap_or_default();
    let in_env = std::env::split_paths(&current_path).any(|p| p == bin_dir);
    if !in_env {
        return false;
    }

    #[cfg(unix)]
    if let Some(home) = dirs_next::home_dir() {
        if let Ok(rel) = bin_dir.strip_prefix(&home) {
            if !rel.as_os_str().is_empty() {
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                let posix_expr = format!("$HOME/{rel_str}");
                let candidates = [
                    home.join(".zshrc"),
                    home.join(".bashrc"),
                    home.join(".bash_profile"),
                    home.join(".profile"),
                    home.join(".config").join("fish").join("config.fish"),
                ];
                let any_rc_exists = candidates.iter().any(|p| p.exists());
                if any_rc_exists {
                    let mentioned = candidates.iter().any(|rc| {
                        let Ok(text) = std::fs::read_to_string(rc) else {
                            return false;
                        };
                        text.contains(dir_raw.as_ref())
                            || text.contains(&posix_expr)
                            || text.contains(&format!("~/{rel_str}"))
                            || text.contains(&format!("${{HOME}}/{rel_str}"))
                    });
                    if !mentioned {
                        return false;
                    }
                }
            }
        }
    }

    true
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PathRepairReport {
    pub id: String,
    pub installed: bool,
    pub binary_path: Option<String>,
    pub bin_dir: Option<String>,
    pub on_path_before: bool,
    pub on_path_after: bool,
    pub updated_files: Vec<String>,
}

/// Detect `manifest`'s binary and repair the user's `$PATH` (both in the
/// current process environment and in `.zshrc` / `.bashrc` / `.bash_profile` /
/// `config.fish` / Windows `HKCU\Environment\Path`).
pub fn repair_agent_path(manifest: &AgentManifest) -> PathRepairReport {
    let before = detect(&manifest.detect);
    let Some(bin_path) = before.binary_path.clone() else {
        return PathRepairReport {
            id: manifest.id.clone(),
            installed: false,
            binary_path: None,
            bin_dir: None,
            on_path_before: false,
            on_path_after: false,
            updated_files: Vec::new(),
        };
    };

    let Some(bin_dir) = std::path::Path::new(&bin_path).parent() else {
        return PathRepairReport {
            id: manifest.id.clone(),
            installed: before.ok,
            binary_path: Some(bin_path),
            bin_dir: None,
            on_path_before: before.on_path,
            on_path_after: before.on_path,
            updated_files: Vec::new(),
        };
    };

    let updated_files = ensure_dir_on_user_path(bin_dir);
    let on_path_after = is_bin_on_user_path(&bin_path);
    let bin_dir_str = bin_dir.to_string_lossy().to_string();
    PathRepairReport {
        id: manifest.id.clone(),
        installed: before.ok,
        binary_path: Some(bin_path),
        bin_dir: Some(bin_dir_str),
        on_path_before: before.on_path,
        on_path_after,
        updated_files,
    }
}

/// Repair `$PATH` for all installed agents in `manifests`.
pub fn repair_all_agents_path(manifests: &[AgentManifest]) -> Vec<PathRepairReport> {
    manifests.iter().map(repair_agent_path).collect()
}

pub fn run_install(manifest: &AgentManifest) -> Result<Vec<StepReport>> {
    let platform: &PlatformInstall =
        manifest
            .platform_install()
            .ok_or_else(|| AgentError::InstallFailed {
                exit: None,
                detail: format!(
                    "agent {:?} has no install steps for this platform ({})",
                    manifest.id,
                    std::env::consts::OS
                ),
            })?;

    // Non-technical-user nicety: if a step needs `pipx` or `uv` and it isn't
    // present, bootstrap it automatically so the user doesn't have to install
    // package managers by hand first.
    ensure_pipx_if_needed(platform);
    ensure_uv_if_needed(platform);

    let mut reports = Vec::new();
    for (i, step) in platform.steps.iter().enumerate() {
        let report = run_step(step, &format!("step {}/{}", i + 1, platform.steps.len()))?;
        let ok = report.success;
        reports.push(report);
        if !ok {
            return Err(AgentError::InstallFailed {
                exit: reports.last().and_then(|r| r.exit_code),
                detail: format!(
                    "{}: stderr: {}",
                    manifest.id,
                    reports.last().map(|r| r.stderr_tail.as_str()).unwrap_or("")
                ),
            });
        }
    }

    // After install, run detect again to confirm.
    let after = detect(&manifest.detect);
    if !after.ok {
        return Err(AgentError::InstallFailed {
            exit: None,
            detail: format!(
                "install steps completed but `{}` still not detected on PATH",
                manifest.detect.command
            ),
        });
    }

    // Automatically ensure the installed binary's directory is on the current
    // process PATH and persisted in the user's shell configuration / OS PATH.
    if let Some(bin_path) = &after.binary_path {
        if let Some(bin_dir) = std::path::Path::new(bin_path).parent() {
            let modified = ensure_dir_on_user_path(bin_dir);
            let msg = if modified.is_empty() {
                format!("PATH verified ({})", bin_dir.display())
            } else {
                format!(
                    "added {} to PATH ({})",
                    bin_dir.display(),
                    modified.join(", ")
                )
            };
            reports.push(StepReport {
                label: format!("configure PATH ({})", bin_dir.display()),
                success: true,
                exit_code: Some(0),
                stdout_tail: msg,
                stderr_tail: String::new(),
            });
        }
    }

    Ok(reports)
}

pub fn run_uninstall(manifest: &AgentManifest) -> Result<StepReport> {
    let cmd = manifest
        .install
        .uninstall
        .as_ref()
        .ok_or_else(|| AgentError::InstallFailed {
            exit: None,
            detail: format!("agent {:?} has no uninstall command", manifest.id),
        })?;
    run_shell(&cmd.cmd, "uninstall")
}

pub fn run_update(manifest: &AgentManifest) -> Result<StepReport> {
    let cmd = manifest
        .install
        .update
        .as_ref()
        .ok_or_else(|| AgentError::InstallFailed {
            exit: None,
            detail: format!("agent {:?} has no update command", manifest.id),
        })?;
    run_shell(&cmd.cmd, "update")
}

fn run_step(step: &InstallStep, label: &str) -> Result<StepReport> {
    match step {
        InstallStep::Shell { cmd } => run_shell(cmd, label),
        InstallStep::Download {
            url,
            sha256,
            dest,
            chmod,
        } => download_to(url, sha256, dest, chmod.as_deref(), label),
        InstallStep::ScriptText {
            interpreter,
            text,
            sha256,
        } => run_script_text(interpreter, text, sha256, label),
    }
}

fn run_shell(cmd: &[String], label: &str) -> Result<StepReport> {
    let mut iter = cmd.iter();
    let bin = iter.next().ok_or_else(|| AgentError::InstallFailed {
        exit: None,
        detail: format!("{label}: empty command"),
    })?;
    // Resolve to an absolute path with the same GUI-aware lookup detect()
    // uses, and run with an enriched PATH. A Dock-launched Unterm otherwise
    // spawns `npm`/`pipx` against launchd's minimal PATH and the install step
    // dies with "spawn failed: No such file or directory" even though the
    // tool is on the user's shell PATH.
    let resolved = which(bin).unwrap_or_else(|| bin.to_string());
    let mut child = spawn_command(&resolved);
    child.env("PATH", enriched_path());
    for a in iter {
        child.arg(a);
    }
    let output = child.output().map_err(|e| AgentError::InstallFailed {
        exit: None,
        detail: format!("{label}: spawn failed ({resolved}): {e}"),
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // On Unix, a system-wide Node installation (`/usr/local/lib/node_modules`)
    // often rejects `npm install -g <pkg>` with EACCES when run without sudo.
    // Automatically retry with `NPM_CONFIG_PREFIX=$HOME/.local` so the binary
    // installs into `~/.local/bin` and `ensure_dir_on_user_path` wires it up.
    #[cfg(unix)]
    if !output.status.success()
        && bin == "npm"
        && cmd.iter().any(|a| a == "-g" || a == "--global")
        && (stderr.contains("EACCES") || stderr.to_ascii_lowercase().contains("permission denied"))
    {
        if let Some(home) = dirs_next::home_dir() {
            let user_prefix = home.join(".local");
            let _ = std::fs::create_dir_all(user_prefix.join("bin"));
            let mut retry = spawn_command(&resolved);
            retry.env("PATH", enriched_path());
            retry.env("NPM_CONFIG_PREFIX", &user_prefix);
            for a in &cmd[1..] {
                retry.arg(a);
            }
            if let Ok(retry_out) = retry.output() {
                let r_stdout = String::from_utf8_lossy(&retry_out.stdout).to_string();
                let r_stderr = String::from_utf8_lossy(&retry_out.stderr).to_string();
                return Ok(StepReport {
                    label: format!("{label} (user prefix ~/.local)"),
                    success: retry_out.status.success(),
                    exit_code: retry_out.status.code(),
                    stdout_tail: tail(&r_stdout, 2000),
                    stderr_tail: tail(&r_stderr, 2000),
                });
            }
        }
    }

    Ok(StepReport {
        label: label.to_string(),
        success: output.status.success(),
        exit_code: output.status.code(),
        stdout_tail: tail(&stdout, 2000),
        stderr_tail: tail(&stderr, 2000),
    })
}

fn run_script_text(
    interpreter: &str,
    text: &str,
    expected_sha256: &str,
    label: &str,
) -> Result<StepReport> {
    let actual = {
        let mut h = Sha256::new();
        h.update(text.as_bytes());
        hex_lower(&h.finalize())
    };
    if actual != expected_sha256.to_ascii_lowercase() {
        return Err(AgentError::InstallFailed {
            exit: None,
            detail: format!(
                "{label}: script_text sha256 mismatch (manifest tampered? expected {expected_sha256}, got {actual})"
            ),
        });
    }

    let resolved = which(interpreter).unwrap_or_else(|| interpreter.to_string());
    // spawn_command applies CREATE_NO_WINDOW on Windows so the interpreter
    // (python/sh -c …) doesn't flash a console window.
    let mut child = spawn_command(&resolved);
    child.env("PATH", enriched_path());
    child.arg("-c").arg(text);
    let output = child.output().map_err(|e| AgentError::InstallFailed {
        exit: None,
        detail: format!("{label}: spawn failed ({resolved}): {e}"),
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok(StepReport {
        label: label.to_string(),
        success: output.status.success(),
        exit_code: output.status.code(),
        stdout_tail: tail(&stdout, 2000),
        stderr_tail: tail(&stderr, 2000),
    })
}

fn download_to(
    url: &str,
    sha256: &str,
    dest: &str,
    chmod: Option<&str>,
    label: &str,
) -> Result<StepReport> {
    let resp = reqwest::blocking::get(url).map_err(|e| AgentError::InstallFailed {
        exit: None,
        detail: format!("{label}: download {url}: {e}"),
    })?;
    if !resp.status().is_success() {
        return Err(AgentError::InstallFailed {
            exit: Some(resp.status().as_u16() as i32),
            detail: format!("{label}: download {url}: HTTP {}", resp.status()),
        });
    }
    let bytes = resp.bytes().map_err(|e| AgentError::InstallFailed {
        exit: None,
        detail: format!("{label}: read body: {e}"),
    })?;
    let mut h = Sha256::new();
    h.update(&bytes);
    let actual = hex_lower(&h.finalize());
    if actual != sha256.to_ascii_lowercase() {
        return Err(AgentError::InstallFailed {
            exit: None,
            detail: format!(
                "{label}: sha256 mismatch (expected {sha256}, got {actual}) — refusing to write"
            ),
        });
    }
    let path = std::path::Path::new(dest);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AgentError::InstallFailed {
            exit: None,
            detail: format!("{label}: mkdir {parent:?}: {e}"),
        })?;
    }
    std::fs::write(path, &bytes).map_err(|e| AgentError::InstallFailed {
        exit: None,
        detail: format!("{label}: write {dest}: {e}"),
    })?;
    #[cfg(unix)]
    if let Some(mode_str) = chmod {
        use std::os::unix::fs::PermissionsExt;
        let mode = u32::from_str_radix(mode_str.trim_start_matches('0'), 8).unwrap_or(0o755);
        let perm = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(path, perm).map_err(|e| AgentError::InstallFailed {
            exit: None,
            detail: format!("{label}: chmod {dest}: {e}"),
        })?;
    }
    #[cfg(not(unix))]
    let _ = chmod;
    Ok(StepReport {
        label: label.to_string(),
        success: true,
        exit_code: Some(0),
        stdout_tail: format!("downloaded {} bytes to {}", bytes.len(), dest),
        stderr_tail: String::new(),
    })
}

fn tail(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    s[s.len() - n..].to_string()
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{:02x}", b);
    }
    s
}

#[cfg(test)]
mod path_fallback_tests {
    use super::*;

    #[test]
    fn enriched_path_includes_gui_unreachable_dirs() {
        // A GUI launched from the Dock or from Explorer inherits a narrower
        // PATH than the user's shell, so enriched_path must always re-add the
        // dirs where node/npm/pipx actually live. Which dirs those are is
        // entirely platform-specific, so each platform checks its own.
        let path = enriched_path();
        let dirs: Vec<_> = std::env::split_paths(&path).collect();

        #[cfg(unix)]
        assert!(
            dirs.iter().any(|dir| dir.ends_with("homebrew/bin"))
                || dirs
                    .iter()
                    .any(|dir| dir == std::path::Path::new("/usr/local/bin")),
            "enriched_path missing brew dirs: {dirs:?}"
        );

        #[cfg(windows)]
        assert!(
            dirs.iter().any(|dir| dir.ends_with("npm"))
                || dirs.iter().any(|dir| dir.ends_with(r"WinGet\Links")),
            "enriched_path missing the npm and winget dirs: {dirs:?}"
        );
    }

    #[test]
    fn fallback_dirs_resolve_npm_when_present() {
        // If npm is installed under one of the GUI-unreachable dirs, the
        // fallback lookup must find it there — i.e. a restricted-PATH install
        // would still resolve the binary. Skips on hosts without it (CI).
        if let Some(p) = extra_path_dirs().iter().find_map(|d| check_dir(d, "npm")) {
            assert!(std::path::Path::new(&p).exists(), "resolved npm gone: {p}");
        }
    }

    #[test]
    fn ensure_dir_on_user_path_updates_rc_files_idempotently() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let local_bin = home.join(".local").join("bin");
        std::fs::create_dir_all(&local_bin).unwrap();
        std::fs::write(home.join(".bashrc"), "# existing bashrc\n").unwrap();

        let modified = ensure_dir_on_user_path_in_home(&local_bin, home, Some("/bin/zsh"));
        assert!(!modified.is_empty(), "expected zshrc/bashrc to be updated");

        let zshrc_text = std::fs::read_to_string(home.join(".zshrc")).unwrap();
        assert!(
            zshrc_text.contains("$HOME/.local/bin"),
            "zshrc should export $HOME/.local/bin: {zshrc_text}"
        );
        let bashrc_text = std::fs::read_to_string(home.join(".bashrc")).unwrap();
        assert!(
            bashrc_text.contains("# existing bashrc") && bashrc_text.contains("$HOME/.local/bin"),
            "bashrc should preserve user content and add PATH: {bashrc_text}"
        );

        // Second call must be a no-op (idempotent).
        let second = ensure_dir_on_user_path_in_home(&local_bin, home, Some("/bin/zsh"));
        assert!(second.is_empty(), "second call should not duplicate PATH entry: {second:?}");
    }
}
