//! Compatibility entry points for command names inherited from the old engine.
//!
//! These names are still advertised in `reference` because older scripts may
//! probe them. Keep the implementations small and explicit: commands with a
//! modern Unterm equivalent forward there, while engine-only mux/asciicast
//! verbs fail with a clear migration path instead of looking unknown.

use crate::output::print_json;
use crate::reference::{self, ReferenceCommand, Section};
use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use clap::Args;
use serde_json::json;
use std::io::Write as _;
use std::path::PathBuf;

#[derive(Debug, Args, Clone)]
pub struct LegacyCommand {
    /// Arguments passed to the legacy command name.
    #[arg(
        value_name = "ARGS",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    pub args: Vec<String>,
}

pub fn run_cli(_cmd: LegacyCommand, json_out: bool) -> Result<()> {
    legacy_removed(
        "cli",
        "`unterm-cli cli` belonged to the old mux CLI.",
        "Use `unterm-cli instance ...`, `unterm-cli session ...`, or `unterm-cli server ...` in this build.",
        json_out,
    )
}

pub fn run_show_keys(json_out: bool) -> Result<()> {
    reference::run(
        ReferenceCommand {
            section: Some(Section::Keys),
            filter: None,
        },
        json_out,
    )
}

pub fn run_ls_fonts(json_out: bool) -> Result<()> {
    let paths = font_search_paths();
    if json_out {
        print_json(&json!({ "font_paths": paths }));
    } else {
        println!("Font search paths");
        println!("{}", "-".repeat(50));
        for path in paths {
            println!("{}", path.display());
        }
    }
    Ok(())
}

pub fn run_imgcat(path: PathBuf) -> Result<()> {
    // iTerm2's inline-image sequence: drawn by Unterm itself (and iTerm2,
    // WezTerm, and the rest that speak it), at the cursor, its natural size
    // or the screen's width, whichever is smaller.
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let size = bytes.len();
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("image");
    let name = base64::engine::general_purpose::STANDARD.encode(name.as_bytes());
    let mut stdout = std::io::stdout().lock();
    // The cursor ends beside the picture's last row; the newline puts the
    // prompt under it, as iTerm2's own imgcat does.
    write!(
        stdout,
        "\x1b]1337;File=name={name};size={size};inline=1:{encoded}\x07\n"
    )?;
    stdout.flush()?;
    Ok(())
}

pub fn run_set_working_directory(path: Option<PathBuf>) -> Result<()> {
    let path = match path {
        Some(path) => path,
        None => std::env::current_dir().context("resolve current directory")?,
    };
    let path = path
        .canonicalize()
        .unwrap_or(path)
        .display()
        .to_string()
        .replace('\\', "/");
    let uri_path = if cfg!(windows) && !path.starts_with('/') {
        format!("/{path}")
    } else {
        path
    };
    print!("\x1b]7;file://localhost{uri_path}\x07");
    Ok(())
}

pub fn run_record(_cmd: LegacyCommand, json_out: bool) -> Result<()> {
    legacy_removed(
        "record",
        "`unterm-cli record` was an old asciicast command.",
        "Use `unterm-cli session record start|stop|status` or `unterm-cli session export` for Unterm's Markdown recording flow.",
        json_out,
    )
}

pub fn run_replay(_cmd: LegacyCommand, json_out: bool) -> Result<()> {
    legacy_removed(
        "replay",
        "`unterm-cli replay` was an old asciicast command and is not implemented in the native Unterm CLI.",
        "There is no native replay replacement yet; read completed recordings with `unterm-cli sessions read <session-id>`.",
        json_out,
    )
}

pub fn run_ssh(cmd: LegacyCommand) -> Result<()> {
    if cmd.args.is_empty() {
        return Err(anyhow!("usage: unterm-cli ssh <host> [ssh-args...]"));
    }
    let mut command = vec!["ssh".to_string()];
    command.extend(cmd.args);
    crate::run_start(None, None, command)
}

pub fn run_connect(_cmd: LegacyCommand, json_out: bool) -> Result<()> {
    legacy_removed(
        "connect",
        "`unterm-cli connect` belonged to the old mux client.",
        "Use `unterm-cli start`, `unterm-cli instance ...`, or MCP discovery through `unterm-cli server info`.",
        json_out,
    )
}

fn legacy_removed(command: &str, message: &str, replacement: &str, json_out: bool) -> Result<()> {
    if json_out {
        print_json(&json!({
            "ok": false,
            "error": {
                "code": "legacy_removed",
                "command": command,
                "message": message,
                "replacement": replacement,
            }
        }));
    }
    Err(anyhow!("{message} {replacement}"))
}

fn font_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    #[cfg(windows)]
    {
        if let Some(windir) = std::env::var_os("WINDIR") {
            paths.push(PathBuf::from(windir).join("Fonts"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            paths.push(
                PathBuf::from(local)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Fonts"),
            );
        }
    }
    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from("/System/Library/Fonts"));
        paths.push(PathBuf::from("/Library/Fonts"));
        if let Some(home) = dirs_next::home_dir() {
            paths.push(home.join("Library").join("Fonts"));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        paths.push(PathBuf::from("/usr/share/fonts"));
        paths.push(PathBuf::from("/usr/local/share/fonts"));
        if let Some(home) = dirs_next::home_dir() {
            paths.push(home.join(".local/share/fonts"));
            paths.push(home.join(".fonts"));
        }
    }
    paths
}
