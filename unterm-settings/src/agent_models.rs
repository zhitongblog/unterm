//! The models each agent CLI can use, for choosing one (a plugin's agent
//! role, for now).
//!
//! Asked of the CLI where it can tell: Codex's app server (`model/list`,
//! with the reasoning efforts each model takes), `agy models`, `opencode
//! models`. Claude Code has no listing; its own help names the aliases, and
//! those are what it is given. Answers are kept for ten minutes.

use crate::server::Response;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const KEEP: Duration = Duration::from_secs(600);
const ASK_TIMEOUT: Duration = Duration::from_secs(40);

static CACHE: Mutex<Option<HashMap<String, (Instant, Vec<Value>)>>> = Mutex::new(None);

fn model(id: &str, label: &str) -> Value {
    json!({"id": id, "label": label, "efforts": [], "default": false})
}

/// Claude Code: the aliases its help gives, then the full names.
fn claude_models() -> Vec<Value> {
    let mut list = vec![
        model("sonnet", "Sonnet (latest)"),
        model("opus", "Opus (latest)"),
        model("fable", "Fable (latest)"),
        model("haiku", "Haiku (latest)"),
        model("claude-sonnet-5-5", "Claude Sonnet 5.5"),
        model("claude-opus-5-5", "Claude Opus 5.5"),
        model("claude-fable-5-2", "Claude Fable 5.2"),
        model("claude-haiku-5-5", "Claude Haiku 5.5"),
    ];
    list[0]["default"] = json!(true);
    list
}

/// A program on PATH, as Windows finds it (`.exe`, `.cmd`, `.bat`).
fn on_path(name: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        for ext in exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn hidden(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// Run a listing command, its output's lines, within [`ASK_TIMEOUT`].
fn lines_of(program: PathBuf, args: &[&str]) -> Result<Vec<String>, String> {
    let mut command = Command::new(&program);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    hidden(&mut command);
    let mut child = command.spawn().map_err(|e| format!("{}: {e}", program.display()))?;
    let stdout = child.stdout.take().ok_or("no output")?;
    let reader = std::thread::spawn(move || BufReader::new(stdout).lines().map_while(Result::ok).collect::<Vec<_>>());
    let deadline = Instant::now() + ASK_TIMEOUT;
    loop {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            return Err("the CLI took too long to list its models".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    reader.join().map_err(|_| "could not read the list".to_string())
}

/// `agy models`: `id<TAB>label` per line, after a "Fetching..." line.
fn agy_models() -> Result<Vec<Value>, String> {
    let program = super::local_agents::local_agent("agy").and_then(|a| a.binary()).ok_or("agy is not installed")?;
    Ok(parse_tabbed(&lines_of(program, &["models"])?))
}

fn parse_tabbed(lines: &[String]) -> Vec<Value> {
    lines
        .iter()
        .filter_map(|line| {
            let (id, label) = line.split_once('\t')?;
            let id = id.trim();
            (!id.is_empty() && !id.contains(' ')).then(|| model(id, label.trim()))
        })
        .collect()
}

/// `opencode models`: one `provider/model` per line.
fn opencode_models() -> Result<Vec<Value>, String> {
    let program = on_path("opencode").ok_or("OpenCode is not installed")?;
    Ok(parse_plain(&lines_of(program, &["models"])?))
}

fn parse_plain(lines: &[String]) -> Vec<Value> {
    lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.contains(' ') && l.contains('/'))
        .map(|id| model(id, id))
        .collect()
}

/// Codex's app server: `initialize`, then `model/list`.
fn codex_models() -> Result<Vec<Value>, String> {
    let program = on_path("codex").ok_or("Codex is not installed")?;
    let mut command = Command::new(&program);
    command.arg("app-server").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    hidden(&mut command);
    let mut child = command.spawn().map_err(|e| format!("{}: {e}", program.display()))?;
    let mut stdin = child.stdin.take().ok_or("no input")?;
    let stdout = child.stdout.take().ok_or("no output")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(message) = serde_json::from_str::<Value>(&line) {
                if message["id"] == json!(2) {
                    let _ = tx.send(message);
                    return;
                }
            }
        }
    });
    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "unzoo-one", "version": "1"}}}),
        json!({"jsonrpc": "2.0", "method": "initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "model/list", "params": {}}),
    ];
    for message in messages {
        let _ = writeln!(stdin, "{message}");
    }
    let _ = stdin.flush();
    let answer = rx.recv_timeout(ASK_TIMEOUT);
    let _ = child.kill();
    let answer = answer.map_err(|_| "Codex did not list its models in time".to_string())?;
    Ok(parse_codex(&answer))
}

fn parse_codex(answer: &Value) -> Vec<Value> {
    answer
        .pointer("/result/data")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|m| !m["hidden"].as_bool().unwrap_or(false))
                .filter_map(|m| {
                    let id = m["model"].as_str().or(m["id"].as_str())?;
                    let efforts: Vec<Value> = m["supportedReasoningEfforts"]
                        .as_array()
                        .map(|e| e.iter().filter_map(|x| x["reasoningEffort"].as_str()).map(|s| json!(s)).collect())
                        .unwrap_or_default();
                    Some(json!({
                        "id": id,
                        "label": m["displayName"].as_str().unwrap_or(id),
                        "description": m["description"].as_str().unwrap_or(""),
                        "efforts": efforts,
                        "default_effort": m["defaultReasoningEffort"],
                        "default": m["isDefault"].as_bool().unwrap_or(false),
                    }))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn models(agent: &str, refresh: bool) -> Result<Vec<Value>, String> {
    if !refresh {
        let kept = CACHE
            .lock()
            .ok()
            .and_then(|c| c.as_ref().and_then(|m| m.get(agent).cloned()))
            .filter(|(at, _)| at.elapsed() < KEEP);
        if let Some((_, list)) = kept {
            return Ok(list);
        }
    }
    let list = match agent {
        "claude-code" => claude_models(),
        "codex-cli" => codex_models()?,
        "agy" => agy_models()?,
        "opencode" => opencode_models()?,
        other => return Err(format!("no model list for {other}")),
    };
    if let Ok(mut cache) = CACHE.lock() {
        cache.get_or_insert_with(HashMap::new).insert(agent.to_string(), (Instant::now(), list.clone()));
    }
    Ok(list)
}

/// `GET /api/agents/models?agent=codex-cli[&refresh=1]`.
pub fn api_models(query: &str) -> Response {
    let param = |key: &str| {
        query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == key).map(|(_, v)| v.to_string())
    };
    let agent = param("agent").unwrap_or_default();
    match models(&agent, param("refresh").is_some_and(|v| v == "1")) {
        Ok(list) => Response::ok_json(json!({"agent": agent, "models": list})),
        Err(error) => Response::ok_json(json!({"agent": agent, "models": [], "error": error})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_cli_listing_is_read() {
        let agy = parse_tabbed(&[
            "Fetching available models...".into(),
            "gemini-4-neon-high\tGemini 4 Neon (High)".into(),
            "gemini-3.8-flash-low\tGemini 3.8 Flash (Low)".into(),
        ]);
        assert_eq!(agy.len(), 2);
        assert_eq!(agy[0]["id"], "gemini-4-neon-high");
        assert_eq!(agy[1]["label"], "Gemini 3.8 Flash (Low)");

        let opencode = parse_plain(&["opencode/big-pickle".into(), "".into(), "Some notice line".into()]);
        assert_eq!(opencode.len(), 1);

        let codex = parse_codex(&json!({"id": 2, "result": {"data": [
            {"id": "gpt-6.2-sol", "model": "gpt-6.2-sol", "displayName": "GPT-6.2-Sol", "hidden": false, "isDefault": true,
             "supportedReasoningEfforts": [{"reasoningEffort": "low"}, {"reasoningEffort": "high"}], "defaultReasoningEffort": "low"},
            {"id": "internal", "model": "internal", "hidden": true}
        ]}}));
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0]["label"], "GPT-6.2-Sol");
        assert_eq!(codex[0]["efforts"], json!(["low", "high"]));
        assert_eq!(codex[0]["default"], true);

        assert!(claude_models().iter().any(|m| m["id"] == "sonnet" && m["default"] == true));
    }
}
