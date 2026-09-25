//! `unterm-cli sessions ...` — operate on the persistent recording archive.

use super::client::McpClient;
use super::i18n;
use super::output::print_json;
use anyhow::{anyhow, Result};
use clap::Parser;
use serde_json::json;

#[derive(Debug, Parser, Clone)]
pub struct SessionsCommand {
    #[command(subcommand)]
    pub sub: SessionsSubCommand,
}

#[derive(Debug, Parser, Clone)]
pub enum SessionsSubCommand {
    /// List recorded sessions, optionally filtered by project slug.
    List {
        /// Project slug filter (matches `recording.project_slug`).
        #[arg(long)]
        project: Option<String>,
    },
    /// Print a recorded session's markdown to stdout.
    Read {
        /// Recording session id (UUID).
        session_id: String,
    },
}

pub fn run(cmd: SessionsCommand, json_out: bool) -> Result<()> {
    let mut client = McpClient::connect()?;
    match cmd.sub {
        SessionsSubCommand::List { project } => {
            let mut params = json!({});
            if let Some(p) = project {
                params["project"] = json!(p);
            }
            let result = client.call("session.recording_list", params)?;
            if json_out {
                print_json(&result);
            } else {
                let entries = result.as_array().cloned().unwrap_or_default();
                if entries.is_empty() {
                    println!("{}", i18n::t("cli.sessions.empty"));
                } else {
                    println!(
                        "{:<38} {:<6} {:<24} {}",
                        i18n::t("cli.sessions.head.session_id"),
                        i18n::t("cli.sessions.head.blocks"),
                        i18n::t("cli.sessions.head.started"),
                        i18n::t("cli.sessions.head.project")
                    );
                    for e in &entries {
                        let id = e
                            .get("unterm_session_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let blocks = e.get("block_count").and_then(|v| v.as_u64()).unwrap_or(0);
                        let started = readable_time(
                            e.get("started_at").and_then(|v| v.as_str()).unwrap_or(""),
                        );
                        let proj = e.get("project_slug").and_then(|v| v.as_str()).unwrap_or("");
                        println!("{:<38} {:<6} {:<24} {}", id, blocks, started, proj);
                    }
                }
            }
        }
        SessionsSubCommand::Read { session_id } => {
            let result = client.call(
                "session.recording_read",
                json!({ "session_id": session_id }),
            )?;
            if json_out {
                print_json(&result);
            } else {
                let md = result
                    .get("markdown")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("recording_read did not return markdown"))?;
                print!("{}", md);
                if !md.ends_with('\n') {
                    println!();
                }
            }
        }
    }
    Ok(())
}

/// A recording's start as a date. The live recorder files it as a bare count
/// of microseconds since the epoch, which read in a table as a meaningless
/// sixteen-digit number.
fn readable_time(raw: &str) -> String {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return raw.to_string();
    }
    let Ok(value) = raw.parse::<i64>() else {
        return raw.to_string();
    };
    let micros = match raw.len() {
        16.. => value,
        13..=15 => value.saturating_mul(1_000),
        _ => value.saturating_mul(1_000_000),
    };
    chrono::DateTime::from_timestamp_micros(micros)
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| raw.to_string())
}

#[cfg(test)]
mod readable_time_tests {
    #[test]
    fn a_microsecond_count_reads_as_a_date() {
        assert_eq!(super::readable_time("1790324289544601"), "2026-09-25T08:18:09Z");
        assert_eq!(super::readable_time("2026-09-25T08:18:09+00:00"), "2026-09-25T08:18:09+00:00");
    }
}
