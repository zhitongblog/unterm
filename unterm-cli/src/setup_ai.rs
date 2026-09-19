//! `unterm-cli setup-ai` — make every AI coding agent on this machine
//! discover Unterm without the user lifting a finger.
//!
//! Unterm's whole thesis is "the terminal AI agents can drive". That only
//! pays off if the agents *know it's here*. The agent launcher already wires
//! up agents that Unterm itself spawns, but an agent the user started on
//! their own (a bare `claude` / `codex` / `gemini` in some other terminal)
//! has no idea Unterm exists. This command closes that gap via two channels:
//!
//!   1. **MCP registration** — write the `unterm` stdio server into each
//!      detected agent's *global* MCP config, so the agent can drive the
//!      terminal the moment it starts. We merge into existing config, never
//!      clobber. The bridge (`unterm-cli mcp-stdio`) self-discovers the live
//!      Unterm instance at connect time (see `client::ServerEndpoint`), so a
//!      static registration keeps working across restarts and multi-window.
//!
//!   2. **Context-file injection** — drop a short, marker-delimited Unterm
//!      block into each agent's global instructions file (CLAUDE.md /
//!      AGENTS.md / GEMINI.md). Even an agent that never loads the MCP
//!      server then knows Unterm is here and how to reach it.
//!
//! Idempotent: re-running only writes when something actually changed.
//! Reversible: `--remove` strips both the server entry and the context block.
//! Triggered automatically on GUI first-run (see `wezterm-gui` mcp::server),
//! and runnable by hand for scripting / re-sync.

use super::output::print_json;
use anyhow::Result;
use clap::Args;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Taught to agents that have already loaded the `unterm` MCP server (returned
/// in the MCP `initialize` response's `instructions` field). Imperative and
/// tool-centric: they have the tools, they need to know what they're for.
pub const MCP_INSTRUCTIONS: &str = "\
Unterm is the terminal you are running inside — and one you can drive directly.
Use these tools to operate it:
  • session.*  — list / create / focus / split / resize / destroy panes
  • session.suggest/search/history/audit_log — propose commands safely, search scrollback, inspect history/audit
  • exec.*     — run shells and one-off commands in a pane; signal.send sends terminal control signals
  • screen.*   — read the visible buffer and scrollback as text; screen.search can jump the GUI viewport
  • capture.*  — screenshot a region of the screen
  • workspace.*— read and write files in the working directory
  • instance.* — enumerate Unterm windows (named alpha, bravo, charlie, …)
  • server.*   — health/capability probes; selftest.run runs built-in checks
  • policy.check / agent.list_trusted — inspect write policy and trusted AI agents
Call meta.surface for the full method catalog, or run `unterm-cli reference`
in any shell. Multiple Unterm windows may be open at once; tools act on the
active window unless you target one — use instance.list to see them all.

You are running inside one of these panes. `agent.whoami` reports which one
as `pane_id`, and $UNTERM_PANE holds the same number in your shell. Knowing
it matters mostly so you can stay out of it: that pane is where you are
being read, and typing into it means typing into your own session.

Anything that writes to a pane — exec.*, session.input — asks which pane,
and asks on purpose. Name one you were given or one you made with
session.create. Do not fall back on whichever pane is active: that is the
one the user is looking at, it is almost never the one you mean, and its
screen belongs to work you cannot see.";

/// Injected into agents' global context files for agents that may NOT have the
/// MCP server loaded. Markdown, self-contained, points at how to connect.
const CONTEXT_BRIEF: &str = "\
## Unterm — the terminal you can drive

This machine runs **Unterm**, a terminal built for AI agents to operate
directly. An MCP server named `unterm` should be registered with you: use it to
list/create/focus panes, run commands, read the screen, take screenshots, and
read/write files in the real terminal session the user is looking at.
It also exposes safe suggestion, scrollback search, audit-log, server-health,
and trusted-agent inspection surfaces for AI workflows.

- If the `unterm` tools aren't loaded, ask the user to open Unterm.app or start
  `unterm-core --headless`, then reconnect.
- Full tool + CLI surface from any shell: `unterm-cli reference`.
- Multiple Unterm windows may be open (named alpha, bravo, …); tools target the
  active one unless you pass an instance. List them with the `instance.list` tool.
- You are running inside one of its panes. `agent.whoami` reports which one as
  `pane_id`, and `$UNTERM_PANE` holds the same number in your shell. That pane
  is where you are being read, so it is the one to stay out of — typing into it
  means typing into your own session.
- Anything that writes to a pane asks which pane, and asks on purpose. Name one
  you were given or one you made. The active pane is the one the user is looking
  at; its screen belongs to work you cannot see.";

const BLOCK_BEGIN: &str = "<!-- BEGIN UNTERM (managed by `unterm-cli setup-ai`) -->";
const BLOCK_END: &str = "<!-- END UNTERM -->";

#[derive(Debug, Args, Clone)]
pub struct SetupAiCommand {
    /// Remove Unterm's MCP registration and context blocks instead of adding
    /// them. Also clears the first-run stamp so the GUI re-registers on next
    /// launch.
    #[arg(long)]
    pub remove: bool,

    /// Only register the MCP server; skip injecting the Unterm block into
    /// agents' global context files (CLAUDE.md / AGENTS.md / GEMINI.md).
    #[arg(long = "no-context")]
    pub no_context: bool,

    /// Show what would change without writing anything.
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Skip wiring cockpit lifecycle hooks (Claude Code hooks, Codex
    /// notify, Aider notifications-command) that report agent state
    /// back to Unterm's Agent Cockpit.
    #[arg(long = "no-hooks")]
    pub no_hooks: bool,

    /// Limit to specific client ids (repeatable). Default: every client
    /// detected on this machine. Ids: claude-code, codex, gemini, cursor,
    /// windsurf, opencode.
    #[arg(long = "client")]
    pub clients: Vec<String>,
}

/// On-disk MCP config shape an agent expects. Mirrors the launcher's
/// `format` strings but specialised for the *global* config path.
#[derive(Clone, Copy, PartialEq)]
enum McpFormat {
    /// Top-level `mcpServers` map, each entry `{type:"stdio", command, args}`.
    /// Claude Code (`~/.claude.json`), Gemini, Cursor, Windsurf.
    JsonServers,
    /// Top-level `mcp` map, each entry `{type:"local", enabled, command:[…]}`.
    /// OpenCode.
    OpenCode,
    /// `[mcp_servers.<name>]` TOML table with `command` + `args`. Codex.
    Toml,
}

struct AiClient {
    id: &'static str,
    label: &'static str,
    /// Home-relative paths; the client is "detected" if any exists.
    detect: &'static [&'static str],
    /// Home-relative global MCP config path.
    mcp_path: &'static str,
    mcp_format: McpFormat,
    /// Home-relative global context file, or None to skip context injection.
    context_path: Option<&'static str>,
}

fn clients() -> Vec<AiClient> {
    vec![
        AiClient {
            id: "claude-code",
            label: "Claude Code",
            detect: &[".claude.json", ".claude"],
            mcp_path: ".claude.json",
            mcp_format: McpFormat::JsonServers,
            context_path: Some(".claude/CLAUDE.md"),
        },
        AiClient {
            id: "codex",
            label: "Codex CLI",
            detect: &[".codex"],
            mcp_path: ".codex/config.toml",
            mcp_format: McpFormat::Toml,
            context_path: Some(".codex/AGENTS.md"),
        },
        AiClient {
            id: "gemini",
            label: "Gemini CLI",
            detect: &[".gemini"],
            mcp_path: ".gemini/settings.json",
            mcp_format: McpFormat::JsonServers,
            context_path: Some(".gemini/GEMINI.md"),
        },
        AiClient {
            id: "cursor",
            label: "Cursor",
            detect: &[".cursor"],
            mcp_path: ".cursor/mcp.json",
            mcp_format: McpFormat::JsonServers,
            // Cursor's global rules live in app settings, not a file we can
            // safely edit — register the MCP server only.
            context_path: None,
        },
        AiClient {
            id: "windsurf",
            label: "Windsurf",
            detect: &[".codeium/windsurf"],
            mcp_path: ".codeium/windsurf/mcp_config.json",
            mcp_format: McpFormat::JsonServers,
            context_path: None,
        },
        AiClient {
            id: "opencode",
            label: "OpenCode",
            detect: &[".config/opencode"],
            mcp_path: ".config/opencode/opencode.json",
            mcp_format: McpFormat::OpenCode,
            context_path: Some(".config/opencode/AGENTS.md"),
        },
    ]
}

/// Outcome of one file operation, for reporting.
#[derive(Clone)]
enum Action {
    Wrote,
    Unchanged,
    Removed,
    NotPresent,
    Skipped(&'static str),
    Failed(String),
}

impl Action {
    fn label(&self) -> String {
        match self {
            Action::Wrote => "wrote".into(),
            Action::Unchanged => "unchanged".into(),
            Action::Removed => "removed".into(),
            Action::NotPresent => "not present".into(),
            Action::Skipped(why) => format!("skipped ({why})"),
            Action::Failed(e) => format!("FAILED: {e}"),
        }
    }
}

struct ClientReport {
    id: &'static str,
    label: &'static str,
    detected: bool,
    mcp: Option<Action>,
    context: Option<Action>,
}

pub fn run(cmd: SetupAiCommand, json_out: bool) -> Result<()> {
    let home =
        dirs_next::home_dir().ok_or_else(|| anyhow::anyhow!("cannot resolve home directory"))?;
    let cli = unterm_cli_path();

    let filter: Option<Vec<String>> = if cmd.clients.is_empty() {
        None
    } else {
        Some(cmd.clients.iter().map(|s| s.to_lowercase()).collect())
    };

    let mut reports = Vec::new();
    for c in clients() {
        if let Some(f) = &filter {
            if !f.iter().any(|x| x == c.id) {
                continue;
            }
        }
        let detected = c.detect.iter().any(|p| home.join(p).exists());
        if !detected {
            reports.push(ClientReport {
                id: c.id,
                label: c.label,
                detected: false,
                mcp: None,
                context: None,
            });
            continue;
        }

        let mcp = apply_mcp(
            &home.join(c.mcp_path),
            c.mcp_format,
            &cli,
            cmd.remove,
            cmd.dry_run,
        );

        let context = match (c.context_path, cmd.no_context) {
            (_, true) => Some(Action::Skipped("--no-context")),
            (None, _) => None,
            (Some(p), false) => Some(apply_context(&home.join(p), cmd.remove, cmd.dry_run)),
        };

        reports.push(ClientReport {
            id: c.id,
            label: c.label,
            detected: true,
            mcp: Some(mcp),
            context,
        });
    }

    // Maintain the first-run stamp the GUI uses to decide whether to re-run us.
    let any_detected = reports.iter().any(|r| r.detected);
    let any_failed = reports.iter().any(|r| {
        matches!(r.mcp, Some(Action::Failed(_))) || matches!(r.context, Some(Action::Failed(_)))
    });
    if !cmd.dry_run && filter.is_none() {
        if cmd.remove {
            if let Some(stamp) = stamp_path() {
                let _ = std::fs::remove_file(stamp);
            }
        } else if any_detected && !any_failed {
            // Only stamp a clean, complete run. If no agent was detected (the
            // user hasn't installed one yet) or any write failed, leave the
            // stamp absent so the GUI re-runs us next launch — an agent
            // installed later in this same version still gets picked up.
            write_stamp();
        }
    }

    // Third channel: cockpit lifecycle hooks (merge-only; .unterm-bak
    // backups; see cockpit_hooks.rs). Filtered runs skip it because hooks are
    // machine-global, not per-client.
    let hook_reports = if !cmd.no_hooks && filter.is_none() {
        super::cockpit_hooks::apply_all(&cli, cmd.remove, cmd.dry_run)
    } else {
        Vec::new()
    };

    render(&reports, &cmd, &cli, &hook_reports, json_out);

    if !json_out && !hook_reports.is_empty() {
        println!();
        println!("Cockpit hooks:");
        for r in &hook_reports {
            println!("  {:<12} {:<10} {}", r.client, r.action, r.path);
        }
    }
    Ok(())
}
/// Absolute path to this `unterm-cli` binary, for embedding in agent configs.
/// `current_exe()` IS the unterm-cli path (we're running it). Falls back to a
/// bare `unterm-cli` (resolved via PATH) only if the exe can't be resolved.
fn unterm_cli_path() -> String {
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "unterm-cli".to_string())
}

/// The stamp is Unterm's own state, not part of any agent's config tree, so
/// it moves with the state directory even though the agent paths above are
/// anchored to the real home directory.
fn stamp_path() -> Option<PathBuf> {
    unterm_protocol::state_path("setup-ai.stamp")
}

fn write_stamp() {
    let Some(path) = stamp_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // The GUI gates first-run on its OWN crate version and tells us which value
    // to stamp via UNTERM_SETUP_AI_STAMP, so the gate stays self-consistent
    // even if the GUI (`unterm`) and CLI (`unterm-cli`) crate versions ever
    // diverge. A manual `unterm-cli setup-ai` falls back to this binary's
    // version.
    let version = std::env::var("UNTERM_SETUP_AI_STAMP")
        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    let _ = std::fs::write(path, version);
}

// ---------------------------------------------------------------------------
// MCP config registration
// ---------------------------------------------------------------------------

fn apply_mcp(path: &Path, format: McpFormat, cli: &str, remove: bool, dry_run: bool) -> Action {
    match format {
        McpFormat::JsonServers => apply_mcp_json(path, cli, remove, dry_run, "mcpServers", false),
        McpFormat::OpenCode => apply_mcp_json(path, cli, remove, dry_run, "mcp", true),
        McpFormat::Toml => apply_mcp_toml(path, cli, remove, dry_run),
    }
}

/// JSON formats: `JsonServers` ({type:stdio,command,args} under `mcpServers`)
/// and `OpenCode` ({type:local,enabled,command:[…]} under `mcp`). We compare
/// the parsed Value before/after and only write on a semantic change, so an
/// already-correct config (or an unrelated one we're not touching) is left
/// byte-for-byte intact.
fn apply_mcp_json(
    path: &Path,
    cli: &str,
    remove: bool,
    dry_run: bool,
    top_key: &str,
    opencode: bool,
) -> Action {
    let before = match read_json(path) {
        Ok(v) => v,
        Err(e) => return Action::Failed(e),
    };
    let mut after = before.clone();

    let Some(obj) = after.as_object_mut() else {
        return Action::Failed(format!("{} is not a JSON object", path.display()));
    };
    let map = match obj
        .entry(top_key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
    {
        Some(m) => m,
        None => return Action::Failed(format!("`{top_key}` is not an object")),
    };

    if remove {
        if map.remove("unterm").is_none() {
            return Action::NotPresent;
        }
        // Prune the container if our removal emptied it, so `--remove` leaves
        // the file as close as possible to its pre-setup-ai state.
        if map.is_empty() {
            obj.remove(top_key);
        }
    } else {
        let entry = if opencode {
            json!({ "type": "local", "enabled": true, "command": [cli, "mcp-stdio"] })
        } else {
            json!({ "type": "stdio", "command": cli, "args": ["mcp-stdio"] })
        };
        map.insert("unterm".to_string(), entry);
    }

    commit_json(path, &before, &after, remove, dry_run)
}

fn apply_mcp_toml(path: &Path, cli: &str, remove: bool, dry_run: bool) -> Action {
    let before = match read_toml(path) {
        Ok(v) => v,
        Err(e) => return Action::Failed(e),
    };
    let mut after = before.clone();

    let Some(tbl) = after.as_table_mut() else {
        return Action::Failed(format!("{} is not a TOML table", path.display()));
    };
    let servers = match tbl
        .entry("mcp_servers")
        .or_insert_with(|| toml::Value::Table(Default::default()))
        .as_table_mut()
    {
        Some(s) => s,
        None => return Action::Failed("`mcp_servers` is not a table".into()),
    };

    if remove {
        if servers.remove("unterm").is_none() {
            return Action::NotPresent;
        }
        // Prune the now-empty table so `--remove` doesn't leave a bare
        // `[mcp_servers]` header the user never wrote.
        if servers.is_empty() {
            tbl.remove("mcp_servers");
        }
    } else {
        let mut entry = toml::value::Table::new();
        entry.insert("command".into(), toml::Value::String(cli.to_string()));
        entry.insert(
            "args".into(),
            toml::Value::Array(vec![toml::Value::String("mcp-stdio".into())]),
        );
        servers.insert("unterm".into(), toml::Value::Table(entry));
    }

    if before == after {
        return if remove {
            Action::Removed
        } else {
            Action::Unchanged
        };
    }
    if dry_run {
        return if remove {
            Action::Removed
        } else {
            Action::Wrote
        };
    }
    let text = match toml::to_string_pretty(&after) {
        Ok(t) => t,
        Err(e) => return Action::Failed(format!("serialize toml: {e}")),
    };
    match write_create(path, &text) {
        Ok(()) => {
            if remove {
                Action::Removed
            } else {
                Action::Wrote
            }
        }
        Err(e) => Action::Failed(e),
    }
}

fn commit_json(path: &Path, before: &Value, after: &Value, remove: bool, dry_run: bool) -> Action {
    if before == after {
        return if remove {
            Action::Removed
        } else {
            Action::Unchanged
        };
    }
    if dry_run {
        return if remove {
            Action::Removed
        } else {
            Action::Wrote
        };
    }
    let text = match serde_json::to_string_pretty(after) {
        Ok(t) => t,
        Err(e) => return Action::Failed(format!("serialize json: {e}")),
    };
    match write_create(path, &text) {
        Ok(()) => {
            if remove {
                Action::Removed
            } else {
                Action::Wrote
            }
        }
        Err(e) => Action::Failed(e),
    }
}

// ---------------------------------------------------------------------------
// Context-file injection
// ---------------------------------------------------------------------------

/// Insert (or replace, or remove) the marker-delimited Unterm block in a
/// global context file. We only ever touch the region between our markers —
/// the rest of the user's CLAUDE.md / AGENTS.md is untouched.
fn apply_context(path: &Path, remove: bool, dry_run: bool) -> Action {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let new = if remove {
        strip_block(&existing)
    } else {
        upsert_block(&existing)
    };

    if new == existing {
        return if remove {
            Action::NotPresent
        } else {
            Action::Unchanged
        };
    }
    if dry_run {
        return if remove {
            Action::Removed
        } else {
            Action::Wrote
        };
    }
    match write_create(path, &new) {
        Ok(()) => {
            if remove {
                Action::Removed
            } else {
                Action::Wrote
            }
        }
        Err(e) => Action::Failed(e),
    }
}

fn managed_block() -> String {
    format!("{BLOCK_BEGIN}\n{CONTEXT_BRIEF}\n{BLOCK_END}")
}

/// Strip every managed region, then append exactly one fresh block. Building
/// on strip_all (which also clears duplicates and truncated half-blocks)
/// guarantees the file ends with precisely one well-formed Unterm block.
fn upsert_block(existing: &str) -> String {
    let base = strip_all_blocks(existing);
    let block = managed_block();
    if base.trim().is_empty() {
        format!("{block}\n")
    } else {
        let sep = if base.ends_with('\n') { "\n" } else { "\n\n" };
        format!("{base}{sep}{block}\n")
    }
}

/// Remove all managed regions (and tidy trailing whitespace).
fn strip_block(existing: &str) -> String {
    strip_all_blocks(existing)
}

/// Remove every `BEGIN..END` region. Also removes a dangling `BEGIN` with no
/// matching `END` (a block truncated by a crash mid-write) by treating it as
/// extending to end-of-file — our blocks always close with END, so an unclosed
/// BEGIN is ours and orphaned. Idempotent and handles duplicates: a file the
/// tool never touched (no markers) comes back unchanged.
fn strip_all_blocks(existing: &str) -> String {
    let mut s = existing.to_string();
    while let Some(start) = s.find(BLOCK_BEGIN) {
        let end = match s[start..].find(BLOCK_END) {
            Some(rel) => start + rel + BLOCK_END.len(),
            None => s.len(), // dangling BEGIN — drop through EOF
        };
        let mut out = String::with_capacity(s.len());
        out.push_str(s[..start].trim_end());
        let tail = s[end..].trim_start();
        if !out.is_empty() && !tail.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(tail);
        s = out;
    }
    // Preserve a single trailing newline if any content remains.
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

// ---------------------------------------------------------------------------
// IO helpers
// ---------------------------------------------------------------------------

/// Read a config file for merging. CRITICAL distinction: a *missing* file is
/// fine (start from empty and create it), but a file that EXISTS yet fails to
/// read or parse must NOT be treated as empty — doing so would make us
/// overwrite the user's real config (their entire `~/.claude.json`, etc.) with
/// just the unterm entry. So missing → Ok(empty); present-but-unreadable or
/// present-but-unparseable → Err, and the caller bails without writing.
fn read_json(path: &Path) -> std::result::Result<Value, String> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("read {}: {e}", path.display())),
        Ok(s) if s.trim().is_empty() => Ok(json!({})),
        Ok(s) => serde_json::from_str(&s).map_err(|e| {
            format!(
                "{} exists but is not valid JSON ({e}); refusing to overwrite",
                path.display()
            )
        }),
    }
}

fn read_toml(path: &Path) -> std::result::Result<toml::Value, String> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(toml::Value::Table(Default::default()))
        }
        Err(e) => Err(format!("read {}: {e}", path.display())),
        Ok(s) if s.trim().is_empty() => Ok(toml::Value::Table(Default::default())),
        Ok(s) => s.parse::<toml::Value>().map_err(|e| {
            format!(
                "{} exists but is not valid TOML ({e}); refusing to overwrite",
                path.display()
            )
        }),
    }
}

/// Write atomically: stream to a sibling temp file, then rename(2) over the
/// target. A crash / full disk / concurrent reader can never observe a
/// truncated config (rename is atomic on the same filesystem). Mirrors
/// `server_info::write_atomic`. The temp lives in the same dir so the rename
/// stays on one filesystem.
fn write_create(path: &Path, text: &str) -> std::result::Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let tmp = path.with_file_name(format!(
        "{}.unterm-tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config")
    ));
    std::fs::write(&tmp, text).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename {} -> {}: {e}", tmp.display(), path.display())
    })
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

fn render(
    reports: &[ClientReport],
    cmd: &SetupAiCommand,
    cli: &str,
    hook_reports: &[super::cockpit_hooks::HookReport],
    json_out: bool,
) {
    if json_out {
        print_json(&render_json_value(reports, cmd, cli, hook_reports));
        return;
    }

    let verb = if cmd.remove {
        "Removing Unterm from"
    } else {
        "Registering Unterm with"
    };
    let dry = if cmd.dry_run {
        "  (dry run — nothing written)"
    } else {
        ""
    };
    println!("{verb} AI agents on this machine{dry}");
    println!("  unterm-cli: {cli}\n");

    let mut any = false;
    for r in reports {
        if !r.detected {
            println!("  ◦ {:<13} not detected — skipped", r.label);
            continue;
        }
        any = true;
        let mcp = r.mcp.as_ref().map(|a| a.label()).unwrap_or_default();
        match &r.context {
            Some(c) => println!(
                "  ✓ {:<13} mcp: {:<16} context: {}",
                r.label,
                mcp,
                c.label()
            ),
            None => println!("  ✓ {:<13} mcp: {}", r.label, mcp),
        }
    }

    if !any {
        println!("\nNo supported AI agents detected. Install one (Claude Code, Codex,");
        println!("Gemini CLI, Cursor, Windsurf, OpenCode) then re-run `unterm-cli setup-ai`.");
    } else if !cmd.remove {
        println!("\nDone. Agents will reach Unterm via the `unterm` MCP server");
        println!(
            "(needs Unterm.app or unterm-core running). Re-run any time to re-sync; \
             `--remove` to undo."
        );
    }
}

fn render_json_value(
    reports: &[ClientReport],
    cmd: &SetupAiCommand,
    cli: &str,
    hook_reports: &[super::cockpit_hooks::HookReport],
) -> Value {
    let clients: Vec<Value> = reports
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "label": r.label,
                "detected": r.detected,
                "mcp": r.mcp.as_ref().map(|a| a.label()),
                "context": r.context.as_ref().map(|a| a.label()),
            })
        })
        .collect();
    let hooks: Vec<Value> = hook_reports
        .iter()
        .map(|r| json!({ "client": r.client, "path": r.path, "action": r.action }))
        .collect();
    json!({
        "action": if cmd.remove { "remove" } else { "register" },
        "dry_run": cmd.dry_run,
        "unterm_cli": cli,
        "clients": clients,
        "cockpit_hooks": hooks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("unterm-setupai-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn rj(p: &Path) -> Value {
        read_json(p).expect("valid json")
    }

    #[test]
    fn json_servers_register_merge_idempotent_remove() {
        let dir = tmp("json");
        let path = dir.join(".claude.json");
        // Pre-existing unrelated config must survive.
        std::fs::write(
            &path,
            r#"{"mcpServers":{"other":{"command":"x","args":[]}},"numHistory":7}"#,
        )
        .unwrap();

        let a = apply_mcp_json(&path, "/opt/unterm-cli", false, false, "mcpServers", false);
        assert!(matches!(a, Action::Wrote));
        let v = rj(&path);
        assert_eq!(
            v["mcpServers"]["other"]["command"], "x",
            "clobbered sibling"
        );
        assert_eq!(v["mcpServers"]["unterm"]["type"], "stdio");
        assert_eq!(v["mcpServers"]["unterm"]["command"], "/opt/unterm-cli");
        assert_eq!(v["mcpServers"]["unterm"]["args"][0], "mcp-stdio");
        assert_eq!(v["numHistory"], 7, "unrelated key dropped");

        // Second run = no change.
        let b = apply_mcp_json(&path, "/opt/unterm-cli", false, false, "mcpServers", false);
        assert!(
            matches!(b, Action::Unchanged),
            "second run should be a no-op"
        );

        // Remove pulls our entry, leaves the sibling.
        let c = apply_mcp_json(&path, "/opt/unterm-cli", true, false, "mcpServers", false);
        assert!(matches!(c, Action::Removed));
        let v = rj(&path);
        assert!(v["mcpServers"].get("unterm").is_none());
        assert_eq!(v["mcpServers"]["other"]["command"], "x");

        // Remove again = nothing to do.
        let d = apply_mcp_json(&path, "/opt/unterm-cli", true, false, "mcpServers", false);
        assert!(matches!(d, Action::NotPresent));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn opencode_shape() {
        let dir = tmp("oc");
        let path = dir.join("opencode.json");
        apply_mcp_json(&path, "/opt/unterm-cli", false, false, "mcp", true);
        let v = rj(&path);
        assert!(
            v.get("mcpServers").is_none(),
            "opencode must not use mcpServers"
        );
        assert_eq!(v["mcp"]["unterm"]["type"], "local");
        assert_eq!(v["mcp"]["unterm"]["enabled"], true);
        assert_eq!(v["mcp"]["unterm"]["command"][0], "/opt/unterm-cli");
        assert_eq!(v["mcp"]["unterm"]["command"][1], "mcp-stdio");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn codex_toml_shape_and_remove() {
        let dir = tmp("codex");
        let path = dir.join("config.toml");
        std::fs::write(&path, "model = \"gpt-5\"\n").unwrap();
        apply_mcp_toml(&path, "/opt/unterm-cli", false, false);
        let doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(doc["model"].as_str(), Some("gpt-5"), "preserved user key");
        assert_eq!(
            doc["mcp_servers"]["unterm"]["command"].as_str(),
            Some("/opt/unterm-cli")
        );
        assert_eq!(
            doc["mcp_servers"]["unterm"]["args"][0].as_str(),
            Some("mcp-stdio")
        );

        assert!(matches!(
            apply_mcp_toml(&path, "/opt/unterm-cli", false, false),
            Action::Unchanged
        ));
        assert!(matches!(
            apply_mcp_toml(&path, "/opt/unterm-cli", true, false),
            Action::Removed
        ));
        let doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert!(doc
            .get("mcp_servers")
            .and_then(|s| s.get("unterm"))
            .is_none());
        assert_eq!(doc["model"].as_str(), Some("gpt-5"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn context_inject_idempotent_and_preserves_user_text() {
        let dir = tmp("ctx");
        let path = dir.join("CLAUDE.md");
        std::fs::write(&path, "# My rules\n\nUse tabs.\n").unwrap();

        assert!(matches!(apply_context(&path, false, false), Action::Wrote));
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.starts_with("# My rules"),
            "user content preserved at top"
        );
        assert!(body.contains("## Unterm — the terminal you can drive"));
        assert!(body.contains(BLOCK_BEGIN) && body.contains(BLOCK_END));

        // Idempotent.
        assert!(matches!(
            apply_context(&path, false, false),
            Action::Unchanged
        ));

        // Editing the brief re-syncs in place (simulate drift by mangling block).
        let mangled = body.replace("the terminal you can drive", "OUTDATED");
        std::fs::write(&path, &mangled).unwrap();
        assert!(matches!(apply_context(&path, false, false), Action::Wrote));
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("OUTDATED"), "stale block not re-synced");
        // exactly one block
        assert_eq!(body.matches(BLOCK_BEGIN).count(), 1);

        // Remove restores user content cleanly.
        assert!(matches!(apply_context(&path, true, false), Action::Removed));
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains(BLOCK_BEGIN));
        assert!(body.contains("# My rules") && body.contains("Use tabs."));
        assert!(matches!(
            apply_context(&path, true, false),
            Action::NotPresent
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refuses_to_clobber_unparseable_config() {
        // A real config that exists but doesn't parse must NEVER be overwritten
        // — that would destroy the user's whole agent config.
        let dir = tmp("corrupt");
        let json = dir.join(".claude.json");
        std::fs::write(&json, "{ this is not json, trailing comma, }").unwrap();
        let a = apply_mcp_json(&json, "/opt/unterm-cli", false, false, "mcpServers", false);
        assert!(matches!(a, Action::Failed(_)), "must refuse, not clobber");
        assert_eq!(
            std::fs::read_to_string(&json).unwrap(),
            "{ this is not json, trailing comma, }",
            "file left byte-for-byte intact"
        );

        let toml_path = dir.join("config.toml");
        std::fs::write(&toml_path, "model = \"x\"\n[unterminated").unwrap();
        let b = apply_mcp_toml(&toml_path, "/opt/unterm-cli", false, false);
        assert!(matches!(b, Action::Failed(_)));
        assert_eq!(
            std::fs::read_to_string(&toml_path).unwrap(),
            "model = \"x\"\n[unterminated",
            "toml left intact"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn context_block_repairs_truncated_and_duplicate() {
        let dir = tmp("blockfix");
        let path = dir.join("AGENTS.md");

        // Truncated: a BEGIN marker with no END (crash mid-write).
        std::fs::write(
            &path,
            format!("# rules\n\n{BLOCK_BEGIN}\n## Unterm\nhalf written"),
        )
        .unwrap();
        assert!(matches!(apply_context(&path, false, false), Action::Wrote));
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            body.matches(BLOCK_BEGIN).count(),
            1,
            "exactly one block after repair"
        );
        assert_eq!(
            body.matches(BLOCK_END).count(),
            1,
            "dangling block got an END"
        );
        assert!(body.starts_with("# rules"), "user text preserved");
        assert!(!body.contains("half written"), "truncated remnant cleared");

        // Duplicate: two full blocks → collapse to one, no orphan markers left.
        let two = format!("{}\n\n{}", managed_block(), managed_block());
        std::fs::write(&path, format!("# rules\n\n{two}\n")).unwrap();
        assert!(matches!(apply_context(&path, true, false), Action::Removed));
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.matches(BLOCK_BEGIN).count(), 0, "all blocks removed");
        assert!(body.contains("# rules"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_prunes_empty_containers() {
        let dir = tmp("prune");
        // JSON: unterm is the only server → mcpServers pruned entirely on remove.
        let json = dir.join(".cursor-mcp.json");
        apply_mcp_json(&json, "/opt/unterm-cli", false, false, "mcpServers", false);
        apply_mcp_json(&json, "/opt/unterm-cli", true, false, "mcpServers", false);
        assert!(
            rj(&json).get("mcpServers").is_none(),
            "empty mcpServers pruned"
        );
        // TOML: same for [mcp_servers].
        let toml_path = dir.join("config.toml");
        apply_mcp_toml(&toml_path, "/opt/unterm-cli", false, false);
        apply_mcp_toml(&toml_path, "/opt/unterm-cli", true, false);
        let doc: toml::Value = std::fs::read_to_string(&toml_path)
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            doc.get("mcp_servers").is_none(),
            "empty [mcp_servers] pruned"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = tmp("dry");
        let path = dir.join(".claude.json");
        let a = apply_mcp_json(&path, "/opt/unterm-cli", false, true, "mcpServers", false);
        assert!(matches!(a, Action::Wrote));
        assert!(!path.exists(), "dry-run must not create the file");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn json_report_contains_clients_and_hooks_in_one_document() {
        let cmd = SetupAiCommand {
            remove: false,
            no_context: false,
            dry_run: true,
            no_hooks: false,
            clients: Vec::new(),
        };
        let reports = vec![ClientReport {
            id: "claude-code",
            label: "Claude Code",
            detected: true,
            mcp: Some(Action::Wrote),
            context: Some(Action::Unchanged),
        }];
        let hooks = vec![super::super::cockpit_hooks::HookReport {
            client: "claude-code",
            path: ".claude/settings.json".to_string(),
            action: "written".to_string(),
        }];

        let value = render_json_value(&reports, &cmd, "/opt/unterm-cli", &hooks);

        assert_eq!(value["action"], "register");
        assert_eq!(value["dry_run"], true);
        assert_eq!(value["clients"].as_array().unwrap().len(), 1);
        assert_eq!(value["clients"][0]["mcp"], "wrote");
        assert_eq!(value["clients"][0]["context"], "unchanged");
        assert_eq!(value["cockpit_hooks"].as_array().unwrap().len(), 1);
        assert_eq!(value["cockpit_hooks"][0]["client"], "claude-code");
    }
}
