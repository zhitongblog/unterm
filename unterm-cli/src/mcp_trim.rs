//! `unterm-cli mcp-trim --keep a,b -- <command...>` — run another MCP stdio
//! server and pass every message through unchanged, except that the replies
//! to `tools/list` name only the tools kept.
//!
//! Why: an agent's client sends every listed tool's description with every
//! request. Unzoo Browser lists about a hundred (some 35,000 tokens); the
//! agents Unzoo One runs need a third of them. Unzoo's own profiles are per
//! client and its smallest leaves out the tab tools those agents are told to
//! use, so the list is cut here instead, without changing Unzoo or what it
//! does with a call. Calls are not filtered: what a tool may do is still
//! judged by the agent's approvals and by Unzoo itself.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Debug, clap::Args, Clone)]
pub struct McpTrimCommand {
    /// The tools to list (comma-separated names as the server spells them).
    #[arg(long, value_delimiter = ',', required = true)]
    pub keep: Vec<String>,

    /// The MCP stdio server to run, after `--`.
    #[arg(last = true, required = true)]
    pub command: Vec<String>,
}

pub fn run(cmd: McpTrimCommand) -> Result<()> {
    let (program, args) = cmd.command.split_first().ok_or_else(|| anyhow!("no command after --"))?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("starting {program}"))?;
    let mut to_child = child.stdin.take().ok_or_else(|| anyhow!("no stdin for {program}"))?;
    let from_child = child.stdout.take().ok_or_else(|| anyhow!("no stdout for {program}"))?;
    let keep: HashSet<String> = cmd.keep.into_iter().collect();
    let asked: Arc<Mutex<HashSet<String>>> = Arc::default();

    let replies = {
        let asked = asked.clone();
        std::thread::spawn(move || {
            let mut stdout = std::io::stdout();
            for line in BufReader::new(from_child).lines() {
                let Ok(line) = line else { break };
                let line = trim_reply(&line, &asked, &keep);
                if stdout.write_all(line.as_bytes()).and_then(|_| stdout.write_all(b"\n")).and_then(|_| stdout.flush()).is_err() {
                    break;
                }
            }
            // The server is gone: so is this bridge, whatever the client
            // still has to say.
            std::process::exit(0);
        })
    };

    for line in std::io::stdin().lock().lines() {
        let line = line?;
        note_request(&line, &asked);
        if writeln!(to_child, "{line}").and_then(|_| to_child.flush()).is_err() {
            break;
        }
    }
    drop(to_child);
    let _ = child.wait();
    let _ = replies.join();
    Ok(())
}

/// Remember the id of a `tools/list` request, so its reply can be found.
/// Noted before the request is passed on, so the reply never comes first.
fn note_request(line: &str, asked: &Mutex<HashSet<String>>) {
    let Ok(message) = serde_json::from_str::<Value>(line.trim_start_matches('\u{feff}')) else { return };
    if message.get("method").and_then(Value::as_str) == Some("tools/list") {
        if let Some(id) = message.get("id") {
            asked.lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_string());
        }
    }
}

/// A line from the server, with the tools it lists cut to `keep` when it
/// answers a `tools/list`; any other line as it came.
fn trim_reply(line: &str, asked: &Mutex<HashSet<String>>, keep: &HashSet<String>) -> String {
    let Ok(mut message) = serde_json::from_str::<Value>(line) else { return line.to_string() };
    let Some(id) = message.get("id").map(Value::to_string) else { return line.to_string() };
    // A request from the server to the client shares the id space with ours.
    if message.get("method").is_some() || !asked.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) {
        return line.to_string();
    }
    if let Some(tools) = message.pointer_mut("/result/tools").and_then(Value::as_array_mut) {
        tools.retain(|tool| tool.get("name").and_then(Value::as_str).is_some_and(|name| keep.contains(name)));
    }
    message.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_the_answer_to_tools_list_is_cut() {
        let asked = Mutex::new(HashSet::new());
        let keep: HashSet<String> = ["tab_create".to_string()].into();
        let listing = json!({"jsonrpc": "2.0", "id": 2, "result": {"tools": [{"name": "tab_create"}, {"name": "human_click"}], "nextCursor": "x"}}).to_string();

        // Not asked for: passed on as it came.
        assert_eq!(trim_reply(&listing, &asked, &keep), listing);

        note_request(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}).to_string(), &asked);
        let cut: Value = serde_json::from_str(&trim_reply(&listing, &asked, &keep)).unwrap();
        assert_eq!(cut["result"]["tools"], json!([{"name": "tab_create"}]));
        assert_eq!(cut["result"]["nextCursor"], "x", "the rest of the answer stays");
        assert!(asked.lock().unwrap().is_empty(), "each answer is cut once");

        // A call's answer, and a request from the server, go through.
        note_request(&json!({"jsonrpc": "2.0", "id": "a", "method": "tools/call", "params": {"name": "human_click"}}).to_string(), &asked);
        assert!(asked.lock().unwrap().is_empty());
        note_request(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}).to_string(), &asked);
        let from_server = json!({"jsonrpc": "2.0", "id": 3, "method": "elicitation/create", "params": {}}).to_string();
        assert_eq!(trim_reply(&from_server, &asked, &keep), from_server);
        assert_eq!(trim_reply("not json", &asked, &keep), "not json");
    }

    #[test]
    fn the_command_comes_after_two_dashes() {
        let opt = <crate::Opt as clap::Parser>::try_parse_from([
            "unterm-cli", "mcp-trim", "--keep", "tab_create,tab_close", "--", "unzoo-service.exe", "--mcp", "stdio",
        ])
        .unwrap();
        let crate::SubCommand::McpTrim(cmd) = opt.cmd else { panic!("not mcp-trim") };
        assert_eq!(cmd.keep, ["tab_create", "tab_close"]);
        assert_eq!(cmd.command, ["unzoo-service.exe", "--mcp", "stdio"]);
    }
}
