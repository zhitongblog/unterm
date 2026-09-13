//! `unterm-cli exec ...` — thin wrappers around MCP exec.* methods.

use super::client::McpClient;
use super::i18n;
use super::output::{print_json, print_kv};
use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};

#[derive(Debug, Parser, Clone)]
pub struct ExecCommand {
    #[command(subcommand)]
    pub sub: ExecSubCommand,
}

#[derive(Debug, Subcommand, Clone)]
pub enum ExecSubCommand {
    /// Send a command and return immediately.
    Run {
        /// Target pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
        /// Shell command to run. Use `--` before commands with flags.
        // No allow_hyphen_values: a mistyped flag (`--sesion 0 ...`)
        // must fail parsing instead of being swallowed into the command
        // text and typed into a live pane. Commands whose FIRST token
        // starts with a dash still work via the documented `--` escape;
        // flags after the first token (`echo --version`) parse fine
        // because trailing_var_arg collects them verbatim.
        #[arg(value_name = "COMMAND", trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Send a command and wait for Unterm's sentinel to appear.
    Wait {
        /// Target pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
        /// Timeout in milliseconds.
        #[arg(long, default_value_t = 30000)]
        timeout_ms: u64,
        /// Shell command to run. Use `--` before commands with flags.
        // No allow_hyphen_values: a mistyped flag (`--sesion 0 ...`)
        // must fail parsing instead of being swallowed into the command
        // text and typed into a live pane. Commands whose FIRST token
        // starts with a dash still work via the documented `--` escape;
        // flags after the first token (`echo --version`) parse fine
        // because trailing_var_arg collects them verbatim.
        #[arg(value_name = "COMMAND", trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Print whether the pane appears idle or running.
    Status {
        /// Target pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
    },
    /// Send Ctrl+C to the pane.
    Cancel {
        /// Target pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
    },
    /// Send a terminal control signal to the pane.
    Signal {
        /// Target pane id (defaults to the active pane).
        #[arg(long = "pane-id", alias = "id")]
        id: Option<u64>,
        /// Signal/control code: SIGINT, INT, SIGTSTP, TSTP, SIGQUIT, QUIT, or EOF.
        signal: String,
    },
}

pub fn run(cmd: ExecCommand, json_out: bool) -> Result<()> {
    let mut client = McpClient::connect()?;
    match cmd.sub {
        ExecSubCommand::Run { id, command } => {
            let id = resolve_pane_id(&mut client, id)?;
            let command = command_string(command)?;
            let result = client.call("exec.run", json!({ "id": id, "command": command }))?;
            if json_out {
                print_json(&result);
            } else {
                println!(
                    "{}",
                    result.get("sent").and_then(Value::as_bool).unwrap_or(false)
                );
            }
        }
        ExecSubCommand::Wait {
            id,
            timeout_ms,
            command,
        } => {
            let id = resolve_pane_id(&mut client, id)?;
            let command = command_string(command)?;
            let result = client.call(
                "exec.run_wait",
                json!({ "id": id, "command": command, "timeout_ms": timeout_ms }),
            )?;
            if json_out {
                print_json(&result);
            } else {
                if let Some(output) = result.get("output").and_then(Value::as_str) {
                    print!("{output}");
                    if !output.ends_with('\n') {
                        println!();
                    }
                }
                if result
                    .get("timed_out")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    print_kv("Timed out", "true");
                }
            }
        }
        ExecSubCommand::Status { id } => {
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
        ExecSubCommand::Cancel { id } => {
            let id = resolve_pane_id(&mut client, id)?;
            let result = client.call("exec.cancel", json!({ "id": id }))?;
            if json_out {
                print_json(&result);
            } else {
                println!(
                    "{}",
                    result
                        .get("cancelled")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                );
            }
        }
        ExecSubCommand::Signal { id, signal } => {
            let id = resolve_pane_id(&mut client, id)?;
            let result = client.call("signal.send", json!({ "id": id, "signal": signal }))?;
            if json_out {
                print_json(&result);
            } else {
                println!(
                    "{}",
                    result.get("sent").and_then(Value::as_bool).unwrap_or(false)
                );
            }
        }
    }
    Ok(())
}

fn command_string(parts: Vec<String>) -> Result<String> {
    let command = parts.join(" ");
    if command.trim().is_empty() {
        return Err(anyhow!("exec command requires COMMAND"));
    }
    Ok(command)
}

fn resolve_pane_id(client: &mut McpClient, id: Option<u64>) -> Result<u64> {
    if let Some(id) = id {
        return Ok(id);
    }
    let result = client.call("session.list", json!({}))?;
    let sessions = result
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // Prefer the pane the user is looking at; fall back to the
    // lowest-id pane (the list is sorted by id server-side).
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
