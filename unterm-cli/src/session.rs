//! `unterm-cli session ...` — operate on a single live pane (record / export).

use super::client::McpClient;
use super::i18n;
use super::output::{print_json, print_kv};
use anyhow::{anyhow, Result};
use clap::Parser;
use serde_json::{json, Value};
use std::io::Read;
use std::path::PathBuf;

#[derive(Debug, Parser, Clone)]
pub struct SessionCommand {
    #[command(subcommand)]
    pub sub: SessionSubCommand,
}

#[derive(Debug, Parser, Clone)]
pub enum SessionSubCommand {
    /// List live panes (sessions).
    List,
    /// Spawn a new tab.
    Create {
        /// Working directory for the new tab.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Identity profile to apply to this tab's environment.
        #[arg(long)]
        profile: Option<String>,
        /// Shell command to run. Use `--` before commands with flags.
        // No allow_hyphen_values — see exec.rs: mistyped flags must
        // error out, not get swallowed into the command text.
        #[arg(value_name = "COMMAND", trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Split an existing pane and spawn a shell in the new split.
    Split {
        /// Source pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
        /// Split direction: right, left, down, or up.
        #[arg(long, default_value = "right")]
        direction: String,
        /// Size of the new pane as a percentage.
        #[arg(long, default_value_t = 50)]
        size_percent: u8,
        /// Working directory for the new split.
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    /// Focus a pane and its containing tab.
    Focus {
        /// Target pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
    },
    /// Resize a pane's PTY.
    Resize {
        /// Target pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
        #[arg(long)]
        cols: u64,
        #[arg(long)]
        rows: u64,
    },
    /// Close a pane. Requires an explicit pane id.
    Destroy {
        /// Target pane id.
        #[arg(long = "pane-id", alias = "id")]
        id: u64,
    },
    /// Manage block recording for a pane.
    Record(RecordCommand),
    /// Export a pane's block log as Markdown.
    Export {
        /// Target pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
        /// Optional output file. If omitted, the Unterm-side path is printed.
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
    },
    /// Write text into a pane via MCP `session.input`.
    Input {
        /// Target pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
        /// Read additional input from stdin.
        #[arg(long)]
        stdin: bool,
        /// Append carriage return, matching a real Enter keypress.
        #[arg(long)]
        enter: bool,
        /// Text to write. Use `--` before text that starts with a dash.
        // No allow_hyphen_values — see exec.rs: mistyped flags must
        // error out, not get swallowed into the text payload.
        #[arg(value_name = "TEXT", trailing_var_arg = true)]
        text: Vec<String>,
    },
    /// Read the visible pane viewport as plain text.
    Text {
        /// Target pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
    },
    /// Print the pane's current working directory.
    Cwd {
        /// Target pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
    },
    /// Print whether the pane appears idle or running.
    Status {
        /// Target pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
    },
    /// Scan the visible viewport for common error patterns.
    Errors {
        /// Target pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
    },
    /// Print recent non-empty scrollback lines.
    History {
        /// Target pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
        /// Number of trailing rows to inspect.
        #[arg(long, default_value_t = 100)]
        limit: u64,
    },
    /// Print recent audited MCP/CLI write actions.
    AuditLog {
        /// Filter to one pane id.
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
        /// Maximum number of entries to print.
        #[arg(long, default_value_t = 50)]
        limit: u64,
    },
    /// Search pane scrollback for a substring.
    Search {
        /// Target pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
        /// Maximum number of matches to return.
        #[arg(long, default_value_t = 50)]
        max_results: u64,
        /// Scroll the GUI viewport to the first match.
        #[arg(long)]
        goto: bool,
        /// Scroll the GUI viewport to the Nth match, 0-based.
        #[arg(long)]
        goto_match: Option<u64>,
        /// Substring to search for. Use `--` before patterns that start with a dash.
        // No allow_hyphen_values — see exec.rs.
        #[arg(value_name = "PATTERN", trailing_var_arg = true)]
        pattern: Vec<String>,
    },
    /// Queue, inspect, or cancel user-accepted suggestions.
    Suggest(SuggestCommand),
}

#[derive(Debug, Parser, Clone)]
pub struct RecordCommand {
    #[command(subcommand)]
    pub sub: RecordSubCommand,
}

#[derive(Debug, Parser, Clone)]
pub enum RecordSubCommand {
    /// Start recording on the target pane.
    Start {
        /// Target pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
    },
    /// Stop recording on the target pane.
    Stop {
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
    },
    /// Show recording status for the target pane.
    Status {
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
    },
}

#[derive(Debug, Parser, Clone)]
pub struct SuggestCommand {
    #[command(subcommand)]
    pub sub: SuggestSubCommand,
}

#[derive(Debug, Parser, Clone)]
pub enum SuggestSubCommand {
    /// Queue a suggestion for the user to accept or dismiss.
    Post {
        /// Target pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
        /// Optional reason shown to consumers of the suggestion payload.
        #[arg(long)]
        rationale: Option<String>,
        /// Time to keep the suggestion alive.
        #[arg(long)]
        ttl_ms: Option<u64>,
        /// Suggested text. Use `--` before text that starts with a dash.
        // No allow_hyphen_values — see exec.rs: mistyped flags must
        // error out, not get swallowed into the text payload.
        #[arg(value_name = "TEXT", trailing_var_arg = true)]
        text: Vec<String>,
    },
    /// Print one suggestion by id.
    Status {
        /// Suggestion id returned by `suggest post`.
        suggestion_id: String,
    },
    /// Cancel a pending suggestion by id.
    Cancel {
        /// Suggestion id returned by `suggest post`.
        suggestion_id: String,
    },
    /// List pending suggestions.
    List {
        /// Filter to one pane id.
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
    },
}

pub fn run(cmd: SessionCommand, json_out: bool) -> Result<()> {
    let mut client = McpClient::connect()?;
    match cmd.sub {
        SessionSubCommand::List => {
            let result = client.call("session.list", json!({}))?;
            if json_out {
                print_json(&result);
            } else {
                let sessions = result
                    .get("sessions")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                if sessions.is_empty() {
                    println!("{}", i18n::t("cli.session.empty"));
                } else {
                    println!(
                        "{:<5} {:<6} {:<6} {:<10} {}",
                        i18n::t("cli.session.head.id"),
                        i18n::t("cli.session.head.cols"),
                        i18n::t("cli.session.head.rows"),
                        i18n::t("cli.session.head.shell"),
                        i18n::t("cli.session.head.title")
                    );
                    for s in &sessions {
                        let id = s.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                        let cols = s.get("cols").and_then(|v| v.as_u64()).unwrap_or(0);
                        let rows = s.get("rows").and_then(|v| v.as_u64()).unwrap_or(0);
                        let shell = s
                            .get("shell")
                            .and_then(|v| v.get("shell_type"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let title = s.get("title").and_then(|v| v.as_str()).unwrap_or("");
                        println!("{:<5} {:<6} {:<6} {:<10} {}", id, cols, rows, shell, title);
                    }
                }
            }
        }
        SessionSubCommand::Create {
            cwd,
            profile,
            command,
        } => {
            let mut params = json!({});
            if let Some(cwd) = cwd.as_ref() {
                params["cwd"] = json!(cwd.display().to_string());
            }
            if let Some(profile) = profile.as_ref() {
                params["profile"] = json!(profile);
            }
            if let Some((key, value)) = session_create_command_param(&command) {
                params[key] = value;
            }
            let result = client.call("session.create", params)?;
            if json_out {
                print_json(&result);
            } else {
                if let Some(id) = result.get("id").and_then(|v| v.as_u64()) {
                    print_kv("Pane", &id.to_string());
                }
                if let Some(title) = result.get("title").and_then(|v| v.as_str()) {
                    print_kv("Title", title);
                }
                if let Some(profile) = result.get("profile").and_then(|v| v.as_str()) {
                    print_kv("Profile", profile);
                }
            }
        }
        SessionSubCommand::Split {
            id,
            direction,
            size_percent,
            cwd,
        } => {
            let id = resolve_pane_id(&mut client, id)?;
            let mut params = json!({
                "id": id,
                "direction": direction,
                "size_percent": size_percent,
            });
            if let Some(cwd) = cwd.as_ref() {
                params["cwd"] = json!(cwd.display().to_string());
            }
            let result = client.call("session.split", params)?;
            if json_out {
                print_json(&result);
            } else {
                if let Some(id) = result.get("id").and_then(Value::as_u64) {
                    print_kv("Pane", &id.to_string());
                }
                if let Some(title) = result.get("title").and_then(Value::as_str) {
                    print_kv("Title", title);
                }
                if let Some(direction) = result.get("direction").and_then(Value::as_str) {
                    print_kv("Direction", direction);
                }
            }
        }
        SessionSubCommand::Focus { id } => {
            let id = resolve_pane_id(&mut client, id)?;
            let result = client.call("session.focus", json!({ "id": id }))?;
            if json_out {
                print_json(&result);
            } else {
                println!(
                    "{}",
                    result.get("ok").and_then(Value::as_bool).unwrap_or(false)
                );
            }
        }
        SessionSubCommand::Resize { id, cols, rows } => {
            let id = resolve_pane_id(&mut client, id)?;
            let result = client.call(
                "session.resize",
                json!({ "id": id, "cols": cols, "rows": rows }),
            )?;
            if json_out {
                print_json(&result);
            } else {
                println!(
                    "{}",
                    result.get("status").and_then(Value::as_str).unwrap_or("ok")
                );
            }
        }
        SessionSubCommand::Destroy { id } => {
            let result = client.call("session.destroy", json!({ "id": id }))?;
            if json_out {
                print_json(&result);
            } else {
                println!(
                    "{}",
                    result
                        .get("destroyed")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                );
            }
        }
        SessionSubCommand::Record(rec) => match rec.sub {
            RecordSubCommand::Start { id } => {
                let id = resolve_pane_id(&mut client, id)?;
                let result = client.call("session.recording_start", json!({ "id": id }))?;
                if json_out {
                    print_json(&result);
                } else {
                    if let Some(sid) = result.get("session_id").and_then(|v| v.as_str()) {
                        print_kv(&i18n::t("cli.session.label.session_id"), sid);
                    }
                    if let Some(p) = result.get("log_path").and_then(|v| v.as_str()) {
                        print_kv(&i18n::t("cli.session.label.log_path"), p);
                    }
                    if let Some(p) = result.get("md_path_when_done").and_then(|v| v.as_str()) {
                        print_kv(&i18n::t("cli.session.label.md_when_done"), p);
                    }
                }
            }
            RecordSubCommand::Stop { id } => {
                let id = resolve_pane_id(&mut client, id)?;
                let result = client.call("session.recording_stop", json!({ "id": id }))?;
                if json_out {
                    print_json(&result);
                } else {
                    if let Some(sid) = result.get("session_id").and_then(|v| v.as_str()) {
                        print_kv(&i18n::t("cli.session.label.session_id"), sid);
                    }
                    if let Some(c) = result.get("block_count").and_then(|v| v.as_u64()) {
                        print_kv(&i18n::t("cli.session.label.block_count"), &c.to_string());
                    }
                    if let Some(p) = result.get("md_path").and_then(|v| v.as_str()) {
                        print_kv(&i18n::t("cli.session.label.markdown"), p);
                    }
                    if let Some(reason) = result.get("exit_reason").and_then(|v| v.as_str()) {
                        print_kv(&i18n::t("cli.session.label.exit_reason"), reason);
                    }
                }
            }
            RecordSubCommand::Status { id } => {
                let id = resolve_pane_id(&mut client, id)?;
                let result = client.call("session.recording_status", json!({ "id": id }))?;
                if json_out {
                    print_json(&result);
                } else {
                    let active = result
                        .get("enabled")
                        .or_else(|| result.get("active"))
                        .or_else(|| result.get("recording"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let yes_no = if active {
                        i18n::t("cli.session.recording.yes")
                    } else {
                        i18n::t("cli.session.recording.no")
                    };
                    print_kv(&i18n::t("cli.session.label.recording"), &yes_no);
                    if let Some(sid) = result.get("session_id").and_then(|v| v.as_str()) {
                        print_kv(&i18n::t("cli.session.label.session_id"), sid);
                    }
                    if let Some(c) = result.get("block_count").and_then(|v| v.as_u64()) {
                        print_kv(&i18n::t("cli.session.label.block_count"), &c.to_string());
                    }
                }
            }
        },
        SessionSubCommand::Export { id, output } => {
            let id = resolve_pane_id(&mut client, id)?;
            let mut params = json!({ "id": id });
            if let Some(out) = output.as_ref() {
                // If the caller supplied a path, ask MCP to write directly there.
                params["path"] = json!(out.display().to_string());
            }
            let result = client.call("session.export_markdown", params)?;
            let mcp_path = result
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("session.export_markdown did not return a path"))?;

            // If caller asked for an explicit destination and MCP wrote elsewhere,
            // copy the file across so `-o FILE` always lands at FILE.
            if let Some(dest) = output.as_ref() {
                let dest_path = dest.canonicalize().unwrap_or_else(|_| dest.clone());
                let src_path = std::path::Path::new(mcp_path);
                let src_canon = src_path
                    .canonicalize()
                    .unwrap_or_else(|_| src_path.to_path_buf());
                if src_canon != dest_path {
                    if let Some(parent) = dest.parent() {
                        if !parent.as_os_str().is_empty() {
                            std::fs::create_dir_all(parent).ok();
                        }
                    }
                    std::fs::copy(src_path, dest)?;
                }
            }

            if json_out {
                print_json(&result);
            } else if output.is_some() {
                println!("{}", output.unwrap().display());
            } else {
                println!("{}", mcp_path);
            }
        }
        SessionSubCommand::Input {
            id,
            stdin,
            enter,
            text,
        } => {
            let id = resolve_pane_id(&mut client, id)?;
            let mut input = text.join(" ");
            if stdin {
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                if !input.is_empty() && !buf.is_empty() {
                    input.push('\n');
                }
                input.push_str(&buf);
            }
            if enter {
                input.push('\r');
            }
            if input.is_empty() {
                return Err(anyhow!("session input needs TEXT or --stdin"));
            }
            let result = client.call("session.input", json!({ "id": id, "input": input }))?;
            if json_out {
                print_json(&result);
            } else {
                println!(
                    "{}",
                    result.get("status").and_then(Value::as_str).unwrap_or("ok")
                );
            }
        }
        SessionSubCommand::Text { id } => {
            let id = resolve_pane_id(&mut client, id)?;
            let result = client.call("screen.text", json!({ "id": id }))?;
            if json_out {
                print_json(&result);
            } else {
                let lines = result
                    .get("lines")
                    .and_then(Value::as_array)
                    .ok_or_else(|| anyhow!("screen.text did not return `lines`: {}", result))?;
                for line in lines {
                    if let Some(text) = line.as_str() {
                        println!("{text}");
                    }
                }
            }
        }
        SessionSubCommand::Cwd { id } => {
            let id = resolve_pane_id(&mut client, id)?;
            let result = client.call("session.cwd", json!({ "id": id }))?;
            if json_out {
                print_json(&result);
            } else {
                println!(
                    "{}",
                    result.get("cwd").and_then(Value::as_str).unwrap_or("")
                );
            }
        }
        SessionSubCommand::Status { id } => {
            let id = resolve_pane_id(&mut client, id)?;
            let result = client.call("exec.status", json!({ "id": id }))?;
            if json_out {
                print_json(&result);
            } else {
                print_kv(
                    "Status",
                    result.get("status").and_then(Value::as_str).unwrap_or("?"),
                );
                if let Some(fg) = result.get("foreground_process").and_then(Value::as_str) {
                    print_kv("Foreground", fg);
                }
            }
        }
        SessionSubCommand::Errors { id } => {
            let id = resolve_pane_id(&mut client, id)?;
            let result = client.call("screen.detect_errors", json!({ "id": id }))?;
            if json_out {
                print_json(&result);
            } else {
                let errors = result
                    .get("errors")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if errors.is_empty() {
                    println!("No visible errors.");
                } else {
                    println!("{:<8} {:<18} TEXT", "ROW", "PATTERN");
                    for err in errors {
                        let row = err.get("row").and_then(Value::as_i64).unwrap_or(0);
                        let pattern = err.get("pattern").and_then(Value::as_str).unwrap_or("");
                        let text = err.get("text").and_then(Value::as_str).unwrap_or("");
                        println!("{:<8} {:<18} {}", row, pattern, text);
                    }
                }
            }
        }
        SessionSubCommand::History { id, limit } => {
            let id = resolve_pane_id(&mut client, id)?;
            let result = client.call("session.history", json!({ "id": id, "limit": limit }))?;
            if json_out {
                print_json(&result);
            } else {
                let entries = result
                    .get("entries")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        anyhow!("session.history did not return `entries`: {}", result)
                    })?;
                for entry in entries {
                    if let Some(text) = entry.get("text").and_then(Value::as_str) {
                        println!("{text}");
                    }
                }
            }
        }
        SessionSubCommand::AuditLog { id, limit } => {
            let mut params = json!({ "limit": limit });
            if let Some(id) = id {
                params["session_id"] = json!(id.to_string());
            }
            let result = client.call("session.audit_log", params)?;
            if json_out {
                print_json(&result);
            } else {
                let entries = result.as_array().ok_or_else(|| {
                    anyhow!("session.audit_log did not return an array: {}", result)
                })?;
                if entries.is_empty() {
                    println!("No audited write actions.");
                } else {
                    println!(
                        "{:<25} {:<22} {:<8} {:<10} DETAIL",
                        "TIME", "METHOD", "PANE", "AGENT"
                    );
                    for entry in entries {
                        let timestamp =
                            entry.get("timestamp").and_then(Value::as_str).unwrap_or("");
                        let method = entry.get("method").and_then(Value::as_str).unwrap_or("");
                        let pane = entry
                            .get("session_id")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let agent = entry.get("agent").and_then(Value::as_str).unwrap_or("");
                        let detail = entry.get("detail").and_then(Value::as_str).unwrap_or("");
                        println!(
                            "{:<25} {:<22} {:<8} {:<10} {}",
                            timestamp, method, pane, agent, detail
                        );
                    }
                }
            }
        }
        SessionSubCommand::Search {
            id,
            max_results,
            goto,
            goto_match,
            pattern,
        } => {
            let id = resolve_pane_id(&mut client, id)?;
            let pattern = pattern.join(" ");
            if pattern.trim().is_empty() {
                return Err(anyhow!("session search needs PATTERN"));
            }
            let mut params = json!({
                "id": id,
                "pattern": pattern,
                "max_results": max_results,
            });
            if goto {
                params["goto"] = json!(true);
            }
            if let Some(index) = goto_match {
                params["goto_match"] = json!(index);
            }
            let result = client.call("screen.search", params)?;
            if json_out {
                print_json(&result);
            } else {
                let matches = result
                    .get("matches")
                    .and_then(Value::as_array)
                    .ok_or_else(|| anyhow!("screen.search did not return `matches`: {}", result))?;
                if matches.is_empty() {
                    println!("No matches.");
                } else {
                    println!("{:<8} {:<6} TEXT", "ROW", "COL");
                    for item in matches {
                        let row = item.get("row").and_then(Value::as_i64).unwrap_or(0);
                        let col = item.get("col").and_then(Value::as_u64).unwrap_or(0);
                        let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                        println!("{:<8} {:<6} {}", row, col, text);
                    }
                }
                if let Some(scrolled_to) = result.get("scrolled_to") {
                    if !scrolled_to.is_null() {
                        print_kv("Scrolled", &scrolled_to.to_string());
                    }
                }
            }
        }
        SessionSubCommand::Suggest(suggest) => match suggest.sub {
            SuggestSubCommand::Post {
                id,
                rationale,
                ttl_ms,
                text,
            } => {
                let id = resolve_pane_id(&mut client, id)?;
                let text = text.join(" ");
                if text.trim().is_empty() {
                    return Err(anyhow!("session suggest post needs TEXT"));
                }
                let mut params = json!({ "id": id, "text": text });
                if let Some(rationale) = rationale {
                    params["rationale"] = json!(rationale);
                }
                if let Some(ttl_ms) = ttl_ms {
                    params["ttl_ms"] = json!(ttl_ms);
                }
                let result = client.call("session.suggest", params)?;
                if json_out {
                    print_json(&result);
                } else {
                    if let Some(suggestion_id) = result.get("suggestion_id").and_then(Value::as_str)
                    {
                        print_kv("Suggestion", suggestion_id);
                    }
                    if let Some(status) = result.get("status").and_then(Value::as_str) {
                        print_kv("Status", status);
                    }
                }
            }
            SuggestSubCommand::Status { suggestion_id } => {
                let result = client.call(
                    "session.suggest_status",
                    json!({ "suggestion_id": suggestion_id }),
                )?;
                if json_out {
                    print_json(&result);
                } else {
                    print_suggestion(&result);
                }
            }
            SuggestSubCommand::Cancel { suggestion_id } => {
                let result = client.call(
                    "session.suggest_cancel",
                    json!({ "suggestion_id": suggestion_id }),
                )?;
                if json_out {
                    print_json(&result);
                } else {
                    println!(
                        "{}",
                        result.get("status").and_then(Value::as_str).unwrap_or("ok")
                    );
                }
            }
            SuggestSubCommand::List { id } => {
                let mut params = json!({});
                if let Some(id) = id {
                    params["pane_id"] = json!(id);
                }
                let result = client.call("session.suggest_list", params)?;
                if json_out {
                    print_json(&result);
                } else {
                    let suggestions = result.as_array().ok_or_else(|| {
                        anyhow!("session.suggest_list did not return an array: {}", result)
                    })?;
                    if suggestions.is_empty() {
                        println!("No pending suggestions.");
                    } else {
                        println!("{:<24} {:<8} {:<10} TEXT", "SUGGESTION", "PANE", "AGENT");
                        for suggestion in suggestions {
                            let id = suggestion.get("id").and_then(Value::as_str).unwrap_or("");
                            let pane = suggestion
                                .get("pane_id")
                                .and_then(Value::as_u64)
                                .map(|id| id.to_string())
                                .unwrap_or_default();
                            let agent = suggestion
                                .get("posted_by_agent")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            let text = suggestion.get("text").and_then(Value::as_str).unwrap_or("");
                            println!("{:<24} {:<8} {:<10} {}", id, pane, agent, text);
                        }
                    }
                }
            }
        },
    }
    Ok(())
}

fn session_create_command_param(parts: &[String]) -> Option<(&'static str, Value)> {
    match parts {
        [] => None,
        [command] if !command.trim().is_empty() => Some(("command", json!(command))),
        [command] => Some(("argv", json!([command]))),
        argv => Some(("argv", json!(argv))),
    }
}

fn print_suggestion(value: &Value) {
    if let Some(id) = value.get("id").and_then(Value::as_str) {
        print_kv("Suggestion", id);
    }
    if let Some(pane_id) = value.get("pane_id").and_then(Value::as_u64) {
        print_kv("Pane", &pane_id.to_string());
    }
    if let Some(agent) = value.get("posted_by_agent").and_then(Value::as_str) {
        print_kv("Agent", agent);
    }
    if let Some(created_at) = value.get("created_at").and_then(Value::as_str) {
        print_kv("Created", created_at);
    }
    if let Some(state) = value.get("state") {
        print_kv("State", &state.to_string());
    }
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        print_kv("Text", text);
    }
    if let Some(rationale) = value.get("rationale").and_then(Value::as_str) {
        print_kv("Rationale", rationale);
    }
}

/// Pick the user-supplied id, or fall back to the active pane
/// (lowest-id pane if the server didn't flag one active).
fn resolve_pane_id(client: &mut McpClient, id: Option<u64>) -> Result<u64> {
    if let Some(id) = id {
        return Ok(id);
    }
    let result = client.call("session.list", json!({}))?;
    let sessions = result
        .get("sessions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let target = sessions
        .iter()
        .find(|s| s.get("is_active").and_then(Value::as_bool).unwrap_or(false))
        .or_else(|| sessions.first())
        .ok_or_else(|| anyhow!("{}", i18n::t("cli.session.no_panes")))?;
    target
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("target pane is missing an integer id"))
}

#[cfg(test)]
mod tests {
    use super::session_create_command_param;
    use serde_json::json;

    #[test]
    fn session_create_command_param_preserves_single_shell_string() {
        let parts = vec!["printf \"hello from unterm\\n\"; exec zsh".to_string()];
        let (key, value) = session_create_command_param(&parts).expect("command param");

        assert_eq!(key, "command");
        assert_eq!(value, json!("printf \"hello from unterm\\n\"; exec zsh"));
    }

    #[test]
    fn session_create_command_param_preserves_multi_arg_argv() {
        let parts = vec![
            "powershell.exe".to_string(),
            "-NoLogo".to_string(),
            "-NoProfile".to_string(),
            "-Command".to_string(),
            "Write-Output UNTERM_MARK".to_string(),
        ];
        let (key, value) = session_create_command_param(&parts).expect("argv param");

        assert_eq!(key, "argv");
        assert_eq!(value, json!(parts));
    }
}
