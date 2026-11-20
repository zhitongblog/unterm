//! `unterm-cli worktree`, `unterm-cli telemetry`, and `unterm-cli unzoo` —
//! CLI entrypoints for the parallel worktree studio, live AI agent context
//! telemetry HUD, and Unzoo Browser synergy hub.

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use super::client::McpClient;
use super::output::print_json;

#[derive(Debug, Parser, Clone)]
pub struct WorktreeCommand {
    #[command(subcommand)]
    pub sub: WorktreeSubCommand,
}

#[derive(Debug, Subcommand, Clone)]
pub enum WorktreeSubCommand {
    /// List active Git worktrees for a repository with live diff stats.
    List {
        /// Repository path; defaults to the active pane's cwd.
        #[arg(long)]
        cwd: Option<String>,
    },
    /// Create an isolated Git worktree (<repo>.worktrees/<slug> on branch wt/<slug>).
    Create {
        /// Slug hint for the new branch and worktree folder (e.g. fix-auth).
        #[arg(long)]
        name: Option<String>,
        /// Repository path; defaults to the active pane's cwd.
        #[arg(long)]
        cwd: Option<String>,
        /// Open a live Unterm terminal session inside the newly created worktree.
        #[arg(long)]
        open_session: bool,
    },
    /// Auto-commit uncommitted work, squash-merge a worktree branch into main_repo, and clean up.
    Merge {
        #[arg(long = "main-repo")]
        main_repo: String,
        #[arg(long = "worktree-path")]
        worktree_path: String,
        #[arg(long)]
        branch: String,
    },
    /// Remove an isolated Git worktree and prune stale worktree metadata.
    Remove {
        #[arg(long = "main-repo")]
        main_repo: String,
        #[arg(long = "worktree-path")]
        worktree_path: String,
    },
}

pub fn run_worktree(cmd: WorktreeCommand, json_out: bool) -> Result<()> {
    let mut client = McpClient::connect()?;
    match cmd.sub {
        WorktreeSubCommand::List { cwd } => {
            let mut params = json!({});
            if let Some(c) = cwd {
                params["cwd"] = Value::String(c);
            }
            let result = client.call("worktree.list", params)?;
            if json_out {
                print_json(&result);
            } else {
                for wt in result
                    .get("worktrees")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                {
                    let role = if wt.get("is_main").and_then(Value::as_bool).unwrap_or(false) {
                        "main"
                    } else {
                        "worktree"
                    };
                    println!(
                        "[{role:<8}] {:<22} (+{} -{}, {} files)  {}",
                        wt.get("branch").and_then(Value::as_str).unwrap_or("(detached)"),
                        wt.get("added").and_then(Value::as_u64).unwrap_or(0),
                        wt.get("deleted").and_then(Value::as_u64).unwrap_or(0),
                        wt.get("files_changed").and_then(Value::as_u64).unwrap_or(0),
                        wt.get("path").and_then(Value::as_str).unwrap_or(""),
                    );
                }
            }
        }
        WorktreeSubCommand::Create {
            name,
            cwd,
            open_session,
        } => {
            let mut params = json!({ "open_session": open_session });
            if let Some(n) = name {
                params["name"] = Value::String(n);
            }
            if let Some(c) = cwd {
                params["cwd"] = Value::String(c);
            }
            let result = client.call("worktree.create", params)?;
            if json_out {
                print_json(&result);
            } else {
                println!(
                    "created worktree {} on branch {}",
                    result.get("path").and_then(Value::as_str).unwrap_or(""),
                    result.get("branch").and_then(Value::as_str).unwrap_or(""),
                );
            }
        }
        WorktreeSubCommand::Merge {
            main_repo,
            worktree_path,
            branch,
        } => {
            let result = client.call(
                "worktree.merge",
                json!({
                    "main_repo": main_repo,
                    "worktree_path": worktree_path,
                    "branch": branch,
                }),
            )?;
            if json_out {
                print_json(&result);
            } else {
                println!("squash-merged {branch} into {main_repo}");
            }
        }
        WorktreeSubCommand::Remove {
            main_repo,
            worktree_path,
        } => {
            let result = client.call(
                "worktree.remove",
                json!({
                    "main_repo": main_repo,
                    "worktree_path": worktree_path,
                }),
            )?;
            if json_out {
                print_json(&result);
            } else {
                println!("removed worktree {worktree_path}");
            }
        }
    }
    Ok(())
}

#[derive(Debug, Parser, Clone)]
pub struct TelemetryCommand {
    #[command(subcommand)]
    pub sub: TelemetrySubCommand,
}

#[derive(Debug, Subcommand, Clone)]
pub enum TelemetrySubCommand {
    /// Inspect live AI agent context window percentage, token usage, and cost across panes.
    Status {
        /// Optional pane id to filter to.
        #[arg(long = "pane-id", aliases = ["id", "pane"])]
        pane_id: Option<String>,
    },
}

pub fn run_telemetry(cmd: TelemetryCommand, json_out: bool) -> Result<()> {
    let mut client = McpClient::connect()?;
    match cmd.sub {
        TelemetrySubCommand::Status { pane_id } => {
            let mut params = json!({});
            if let Some(id) = pane_id {
                params["pane_id"] = Value::String(id);
            }
            let result = client.call("telemetry.status", params)?;
            if json_out {
                print_json(&result);
            } else {
                for p in result
                    .get("panes")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                {
                    println!(
                        "pane {:<4} {:<12} pressure={:<8} ctx={:<4} tokens={:<8} cost={}",
                        p.get("pane_id").and_then(Value::as_u64).unwrap_or(0),
                        p.get("agent")
                            .and_then(Value::as_str)
                            .or_else(|| p.get("process_name").and_then(Value::as_str))
                            .unwrap_or("-"),
                        p.get("pressure").and_then(Value::as_str).unwrap_or("normal"),
                        p.get("context_pct")
                            .and_then(Value::as_u64)
                            .map(|v| format!("{v}%"))
                            .unwrap_or_else(|| "-".to_string()),
                        p.get("tokens").and_then(Value::as_str).unwrap_or("-"),
                        p.get("cost_usd").and_then(Value::as_str).unwrap_or("-"),
                    );
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Parser, Clone)]
pub struct UnzooCommand {
    #[command(subcommand)]
    pub sub: UnzooSubCommand,
}

#[derive(Debug, Subcommand, Clone)]
pub enum UnzooSubCommand {
    /// Inspect Unzoo Browser installation, MCP binding state, and live localhost dev-server URLs.
    Status,
    /// Open a localhost dev-server URL (or auto-sniffed pane URL) inside Unzoo Browser.
    Preview {
        /// Target URL to preview; omit to auto-sniff from active terminal panes.
        #[arg(long)]
        url: Option<String>,
        /// Optional pane id to sniff the localhost URL from.
        #[arg(long = "pane-id", aliases = ["id", "pane"])]
        pane_id: Option<String>,
    },
    /// Bridge frontend console/network errors from Unzoo Browser into a terminal AI agent pane.
    #[command(name = "bridge-errors")]
    BridgeErrors {
        /// Localhost dev-server URL being debugged.
        #[arg(long)]
        url: String,
        /// Optional frontend console/network error text captured by Unzoo Browser.
        #[arg(long)]
        error: Option<String>,
        /// Target terminal AI agent pane id.
        #[arg(long = "pane-id", aliases = ["id", "pane"])]
        pane_id: Option<String>,
    },
}

pub fn run_unzoo(cmd: UnzooCommand, json_out: bool) -> Result<()> {
    let mut client = McpClient::connect()?;
    match cmd.sub {
        UnzooSubCommand::Status => {
            let result = client.call("unzoo.status", json!({}))?;
            if json_out {
                print_json(&result);
            } else {
                println!(
                    "Unzoo Browser: state={} installed={} leases={} download={}",
                    result.get("state").and_then(Value::as_str).unwrap_or("unknown"),
                    result.get("installed").and_then(Value::as_bool).unwrap_or(false),
                    result.get("active_leases").and_then(Value::as_u64).unwrap_or(0),
                    result.get("download_url").and_then(Value::as_str).unwrap_or("https://unzoo.com"),
                );
            }
        }
        UnzooSubCommand::Preview { url, pane_id } => {
            let mut params = json!({});
            if let Some(u) = url {
                params["url"] = Value::String(u);
            }
            if let Some(id) = pane_id {
                params["pane_id"] = Value::String(id);
            }
            let result = client.call("unzoo.preview", params)?;
            if json_out {
                print_json(&result);
            } else {
                println!(
                    "preview {} (opened_in_unzoo={})",
                    result.get("url").and_then(Value::as_str).unwrap_or(""),
                    result.get("opened_in_unzoo").and_then(Value::as_bool).unwrap_or(false),
                );
            }
        }
        UnzooSubCommand::BridgeErrors {
            url,
            error,
            pane_id,
        } => {
            let mut params = json!({ "url": url });
            if let Some(e) = error {
                params["error"] = Value::String(e);
            }
            if let Some(id) = pane_id {
                params["pane_id"] = Value::String(id);
            }
            let result = client.call("unzoo.bridge_errors", params)?;
            if json_out {
                print_json(&result);
            } else {
                println!(
                    "bridged frontend error for {} to pane #{}",
                    result.get("url").and_then(Value::as_str).unwrap_or(""),
                    result.get("pane_id").and_then(Value::as_u64).unwrap_or(0),
                );
            }
        }
    }
    Ok(())
}
