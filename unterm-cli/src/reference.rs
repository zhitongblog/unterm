//! `unterm-cli reference` — print the full surface (MCP methods + CLI
//! subcommands + live keybindings) by calling the `meta.surface` MCP
//! method on the connected Unterm instance.
//!
//! Pretty-prints by default. Use `--json` for machine-readable output.
//! Use `--section mcp|cli|keys` to limit the output.

use super::client::McpClient;
use super::output::print_json;
use anyhow::{anyhow, Result};
use clap::{Args, ValueEnum};
use serde_json::json;
use serde_json::Value;

#[derive(Debug, Args, Clone)]
pub struct ReferenceCommand {
    /// Restrict the output to one section. Default: all three.
    #[arg(long = "section", value_enum)]
    pub section: Option<Section>,

    /// Filter by substring match (case-insensitive). Applies to method/
    /// command names, summaries, and keybinding actions.
    #[arg(short = 'f', long = "filter")]
    pub filter: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Section {
    Mcp,
    Cli,
    Keys,
}

pub fn run(cmd: ReferenceCommand, json_out: bool) -> Result<()> {
    let (result, fallback_reason) = reference_payload();

    if json_out {
        let scoped = scope_payload(&result, cmd.section, cmd.filter.as_deref());
        print_json(&scoped);
        return Ok(());
    }

    if let Some(reason) = fallback_reason {
        eprintln!("unterm-cli reference: GUI not reachable ({reason}); showing static MCP/CLI reference without live keybindings");
    }

    let filter = cmd.filter.as_deref().map(|s| s.to_ascii_lowercase());
    let filter = filter.as_deref();

    let want_mcp = matches!(cmd.section, None | Some(Section::Mcp));
    let want_cli = matches!(cmd.section, None | Some(Section::Cli));
    let want_keys = matches!(cmd.section, None | Some(Section::Keys));

    if want_mcp {
        print_mcp(&result, filter)?;
    }
    if want_cli {
        if want_mcp {
            println!();
        }
        print_cli(&result, filter)?;
    }
    if want_keys {
        if want_mcp || want_cli {
            println!();
        }
        print_keys(&result, filter)?;
    }
    Ok(())
}

fn reference_payload() -> (Value, Option<String>) {
    match McpClient::connect().and_then(|mut client| client.call("meta.surface", json!({}))) {
        Ok(value) => (with_local_cli_commands(value), None),
        Err(err) => (static_reference_payload(), Some(err.to_string())),
    }
}

fn with_local_cli_commands(mut value: Value) -> Value {
    value = with_mcp_tool_counts(value);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "cli_commands".to_string(),
            serde_json::to_value(unterm_agents::mcp_meta::CLI_COMMANDS)
                .unwrap_or_else(|_| json!([])),
        );
        object.insert("cli_source".to_string(), json!("local_binary"));
    }
    value
}

fn static_reference_payload() -> Value {
    with_mcp_tool_counts(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "source": "static_fallback",
        "mcp_methods": serde_json::to_value(unterm_agents::mcp_meta::MCP_METHODS)
            .unwrap_or_else(|_| json!([])),
        "cli_commands": serde_json::to_value(unterm_agents::mcp_meta::CLI_COMMANDS)
            .unwrap_or_else(|_| json!([])),
        "keybindings": [],
    }))
}

fn with_mcp_tool_counts(mut value: Value) -> Value {
    const TOOL_EXCLUSIONS: &[&str] = &["meta.surface"];
    let method_count = value
        .get("mcp_methods")
        .and_then(|methods| methods.as_array())
        .map(Vec::len)
        .unwrap_or(0);
    let tool_count = value
        .get("mcp_methods")
        .and_then(|methods| methods.as_array())
        .map(|methods| {
            methods
                .iter()
                .filter_map(|method| method.get("name").and_then(|name| name.as_str()))
                .filter(|name| !TOOL_EXCLUSIONS.contains(name))
                .count()
        })
        .unwrap_or(0);

    if let Some(object) = value.as_object_mut() {
        object.insert("mcp_method_count".to_string(), json!(method_count));
        object.insert("mcp_tool_count".to_string(), json!(tool_count));
        object.insert("mcp_tool_exclusions".to_string(), json!(TOOL_EXCLUSIONS));
    }
    value
}

fn scope_payload(result: &Value, section: Option<Section>, filter: Option<&str>) -> Value {
    let mut out = serde_json::Map::new();
    out.insert(
        "version".to_string(),
        result.get("version").cloned().unwrap_or(Value::Null),
    );
    if let Some(source) = result.get("source") {
        out.insert("source".to_string(), source.clone());
    }
    if let Some(cli_source) = result.get("cli_source") {
        out.insert("cli_source".to_string(), cli_source.clone());
    }
    for key in ["mcp_method_count", "mcp_tool_count", "mcp_tool_exclusions"] {
        if let Some(value) = result.get(key) {
            out.insert(key.to_string(), value.clone());
        }
    }
    let fl = filter.map(|s| s.to_ascii_lowercase());
    let fl = fl.as_deref();
    if matches!(section, None | Some(Section::Mcp)) {
        out.insert(
            "mcp_methods".to_string(),
            filter_array(result.get("mcp_methods"), fl, |v| {
                let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
                let summary = v.get("summary").and_then(|x| x.as_str()).unwrap_or("");
                format!("{} {}", name, summary)
            }),
        );
    }
    if matches!(section, None | Some(Section::Cli)) {
        out.insert(
            "cli_commands".to_string(),
            filter_array(result.get("cli_commands"), fl, |v| {
                let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
                let summary = v.get("summary").and_then(|x| x.as_str()).unwrap_or("");
                format!("{} {}", name, summary)
            }),
        );
    }
    if matches!(section, None | Some(Section::Keys)) {
        out.insert(
            "keybindings".to_string(),
            filter_array(result.get("keybindings"), fl, |v| {
                let key = v.get("key").and_then(|x| x.as_str()).unwrap_or("");
                let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
                format!("{} {}", key, action)
            }),
        );
    }
    Value::Object(out)
}

fn filter_array<F: Fn(&Value) -> String>(
    arr: Option<&Value>,
    filter: Option<&str>,
    haystack_of: F,
) -> Value {
    let Some(Value::Array(items)) = arr else {
        return Value::Array(vec![]);
    };
    let Some(f) = filter else {
        return Value::Array(items.clone());
    };
    let f = f.to_ascii_lowercase();
    let filtered: Vec<Value> = items
        .iter()
        .filter(|v| haystack_of(v).to_ascii_lowercase().contains(&f))
        .cloned()
        .collect();
    Value::Array(filtered)
}

fn print_mcp(result: &Value, filter: Option<&str>) -> Result<()> {
    let methods = result
        .get("mcp_methods")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("meta.surface returned no mcp_methods array"))?;
    println!("MCP methods ({})", methods.len());
    println!("{}", "-".repeat(50));
    let mut shown = 0;
    for m in methods {
        let name = m.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let summary = m.get("summary").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(f) = filter {
            if !format!("{} {}", name, summary)
                .to_ascii_lowercase()
                .contains(f)
            {
                continue;
            }
        }
        println!("  {:<32} {}", name, summary);
        shown += 1;
    }
    if filter.is_some() {
        println!("({} matched)", shown);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{scope_payload, static_reference_payload, Section};
    use serde_json::json;

    #[test]
    fn static_reference_payload_has_mcp_and_cli_without_live_keys() {
        let payload = static_reference_payload();
        assert_eq!(
            payload.get("source").and_then(|v| v.as_str()),
            Some("static_fallback")
        );
        assert!(payload
            .get("mcp_methods")
            .and_then(|v| v.as_array())
            .is_some_and(|items| items
                .iter()
                .any(|m| { m.get("name").and_then(|v| v.as_str()) == Some("meta.surface") })));
        assert!(payload
            .get("cli_commands")
            .and_then(|v| v.as_array())
            .is_some_and(|items| items
                .iter()
                .any(|m| { m.get("name").and_then(|v| v.as_str()) == Some("reference") })));
        assert_eq!(
            payload
                .get("keybindings")
                .and_then(|v| v.as_array())
                .map(Vec::len),
            Some(0)
        );
        // Kept in step with mcp_meta's count contract on purpose: the
        // offline reference an agent reads must describe the same surface
        // the server publishes, or `unterm-cli reference` becomes a document
        // about a previous version.
        assert_eq!(payload["mcp_method_count"], 152);
        assert_eq!(payload["mcp_tool_count"], 151);
        assert_eq!(payload["mcp_tool_exclusions"], json!(["meta.surface"]));
    }

    #[test]
    fn scoped_static_reference_keeps_source_marker() {
        let payload = static_reference_payload();
        let scoped = scope_payload(&payload, Some(Section::Mcp), Some("capture.scrollback"));
        assert_eq!(
            scoped.get("source").and_then(|v| v.as_str()),
            Some("static_fallback")
        );
        assert!(scoped
            .get("mcp_methods")
            .and_then(|v| v.as_array())
            .is_some_and(|items| items.len() == 1));
        assert_eq!(scoped["mcp_method_count"], 152);
        assert_eq!(scoped["mcp_tool_count"], 151);
        assert_eq!(scoped["mcp_tool_exclusions"], json!(["meta.surface"]));
        assert!(scoped.get("cli_commands").is_none());
        assert!(scoped.get("keybindings").is_none());
    }
}

fn print_cli(result: &Value, filter: Option<&str>) -> Result<()> {
    let cmds = result
        .get("cli_commands")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("meta.surface returned no cli_commands array"))?;
    println!("CLI subcommands ({})", cmds.len());
    println!("{}", "-".repeat(50));
    let mut shown = 0;
    for c in cmds {
        let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let summary = c.get("summary").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(f) = filter {
            if !format!("{} {}", name, summary)
                .to_ascii_lowercase()
                .contains(f)
            {
                continue;
            }
        }
        let subs = c
            .get("subcommands")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        println!("  unterm-cli {:<18} {}", name, summary);
        if !subs.is_empty() {
            println!("    └── {}", subs);
        }
        shown += 1;
    }
    if filter.is_some() {
        println!("({} matched)", shown);
    }
    Ok(())
}

fn print_keys(result: &Value, filter: Option<&str>) -> Result<()> {
    let keys = result
        .get("keybindings")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("meta.surface returned no keybindings array"))?;
    println!("Keybindings ({})", keys.len());
    println!("{}", "-".repeat(50));
    let mut shown = 0;
    for k in keys {
        let table = k.get("table").and_then(|v| v.as_str()).unwrap_or("");
        let key = k.get("key").and_then(|v| v.as_str()).unwrap_or("");
        let mods = k.get("mods").and_then(|v| v.as_str()).unwrap_or("");
        let action = k.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let pretty_keys = if mods.is_empty() || mods == "(empty)" {
            key.to_string()
        } else {
            format!("{} {}", mods, key)
        };
        if let Some(f) = filter {
            if !format!("{} {}", pretty_keys, action)
                .to_ascii_lowercase()
                .contains(f)
            {
                continue;
            }
        }
        let table_tag = if table == "default" {
            String::new()
        } else {
            format!("[{}] ", table)
        };
        println!("  {}{:<32} → {}", table_tag, pretty_keys, action);
        shown += 1;
    }
    if filter.is_some() {
        println!("({} matched)", shown);
    }
    Ok(())
}
