//! `unterm-cli mcp-stdio` — a Model Context Protocol *stdio* server that
//! bridges an MCP client (Claude Code, Codex CLI, Gemini CLI, …) to the
//! running Unterm control server (Core first, then GUI/legacy discovery).
//!
//! Why this exists: Unterm's own control server speaks a custom line-
//! delimited JSON-RPC over a TCP socket with a bearer token — that's what
//! `unterm-cli` and the Web Settings UI talk to. AI agents, however, speak
//! the MCP *spec* (newline-delimited JSON-RPC over stdio, with the
//! `initialize` / `tools/list` / `tools/call` handshake). They can't dial
//! the TCP port directly. This bridge is the adapter: an agent spawns
//! `unterm-cli mcp-stdio` as a stdio MCP server, and we proxy every
//! `tools/call` to the TCP server, exposing the full Unterm surface
//! (screen.*, session.*, exec.*, capture.*, upload.*, …) as MCP tools.
//!
//! Agents are auto-wired to this command by the agent launcher (see
//! `unterm-agents`), which writes the right per-agent config
//! (`.mcp.json`, `~/.codex/config.toml`, etc.) pointing here.
//!
//! Protocol notes:
//!   * stdout carries ONLY newline-delimited JSON-RPC. All diagnostics go
//!     to stderr — a stray stdout write corrupts the MCP stream.
//!   * Requests (with `id`) get a response; notifications (no `id`) don't.
//!   * Tool inventory + param schemas come from the TCP server's
//!     `meta.surface`, so the MCP tool list never drifts from dispatch.

use super::client::McpClient;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// MCP protocol version we advertise in the initialize response. This is a
/// widely-supported revision; clients negotiate down if needed.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Friendly per-call error when the control server isn't up. Returned with
/// `isError: true` so the agent can read it and tell the user.
const GUI_NOT_RUNNING: &str = "Unterm control server is not running — open Unterm.app, run \
`unterm start`, or start `unterm-core --headless`, then retry. The tool surface \
is still listed so you know what will be available once it's up.";

pub fn run() -> Result<()> {
    let bridge_started_at = chrono::Utc::now().to_rfc3339();
    let bridge_build = unterm_protocol::BuildHandshake::current(
        unterm_protocol::ProcessRole::McpBridge,
        std::process::id(),
        bridge_started_at.clone(),
    );
    let bridge_registration = unterm_services::bridge_registry::register(bridge_build.clone())
        .map_err(|error| anyhow!("registering MCP bridge lifecycle: {error}"))?;
    // Connect to the running Unterm control server up front — but if neither
    // Core nor GUI is running, come up anyway and keep speaking MCP: serve `initialize` and
    // `tools/list` from the static surface tables baked into this binary,
    // return a clean per-call error on `tools/call`, and lazily reconnect
    // when the GUI appears. Exiting here used to break (a) registry health
    // checks that introspect the server headlessly and (b) agents that start
    // before the terminal does.
    // Who is on the other end, from the MCP handshake (`clientInfo`): told to
    // the server once per connection, so its confirmations and audit say
    // "Claude Desktop" or "Cursor" rather than "anonymous".
    let mut client_info: Option<Value> = None;
    let mut identified = false;
    let (mut client, drain_reason): (Option<McpClient>, Option<String>) =
        match McpClient::connect_as(unterm_protocol::ProcessRole::McpBridge) {
            Ok(c) => (Some(c), None),
            Err(e) if requires_bridge_replacement(&e.to_string()) => {
                let reason = e.to_string();
                eprintln!("unterm mcp-stdio: entering drain: {reason}");
                (None, Some(reason))
            }
            Err(e) => {
                eprintln!(
                    "unterm mcp-stdio: Unterm control server not reachable ({e}); \
                 serving static tool list, will reconnect on demand"
                );
                (None, None)
            }
        };

    // Tool inventory: prefer the live server's meta.surface (never drifts
    // from dispatch); fall back to the compiled-in tables.
    let surface = client
        .as_mut()
        .and_then(|c| c.call("meta.surface", json!({})).ok())
        .unwrap_or_else(|| {
            json!({
                "mcp_methods": serde_json::to_value(unterm_agents::mcp_meta::MCP_METHODS)
                    .unwrap_or_else(|_| json!([]))
            })
        });
    let tools = build_tool_list(&surface);

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    let mut line = String::new();
    loop {
        line.clear();
        let n = stdin.lock().read_line(&mut line)?;
        if n == 0 {
            break; // EOF — client closed stdin
        }
        let trimmed = normalize_input_line(&line);
        if trimmed.is_empty() {
            continue;
        }

        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                // Parse error — reply only if we can't even find an id.
                write_msg(
                    &mut stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": { "code": -32700, "message": format!("parse error: {e}") },
                    }),
                )?;
                continue;
            }
        };

        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let id = req.get("id").cloned();
        let is_notification = id.is_none();

        if let Some(reason) = bridge_registration.drain_reason() {
            if !is_notification {
                respond_err(&mut stdout, id, -32010, &reason)?;
            }
            return Ok(());
        }

        // A bridge compiled from a different product/protocol must not keep
        // serving a stale tool surface. Reject one request with a stable code,
        // flush it, and exit cleanly; the owning MCP client then respawns the
        // command path from its config, loading the installed replacement.
        if let Some(reason) = drain_reason.as_deref() {
            if !is_notification {
                respond_err(&mut stdout, id, -32010, reason)?;
            }
            return Ok(());
        }

        match method {
            "initialize" => {
                client_info = req
                    .pointer("/params/clientInfo")
                    .and_then(|info| {
                        let name = info.get("name")?.as_str()?.trim();
                        (!name.is_empty()).then(|| {
                            json!({
                                "name": name.chars().take(64).collect::<String>(),
                                "version": info.get("version").and_then(Value::as_str),
                            })
                        })
                    });
                identified = false;
                respond(
                    &mut stdout,
                    id,
                    json!({
                        "protocolVersion": PROTOCOL_VERSION,
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": {
                            "name": "unterm",
                            "version": unterm_protocol::PRODUCT_VERSION,
                            "build": bridge_build,
                        },
                        // Injected into the connecting agent's context by most
                        // MCP clients — this is where we teach it what Unterm
                        // is and which tools drive it. Shared with the text
                        // `setup-ai` writes into context files, so the two
                        // discovery channels stay in sync.
                        "instructions": super::setup_ai::MCP_INSTRUCTIONS,
                    }),
                )?;
            }
            "notifications/initialized" | "initialized" => {
                // Notification — no response.
            }
            "ping" => {
                respond(&mut stdout, id, json!({}))?;
            }
            "tools/list" => {
                respond(&mut stdout, id, json!({ "tools": tools }))?;
            }
            "tools/call" => {
                let params = req.get("params").cloned().unwrap_or(Value::Null);
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                // Lazy reconnect: the GUI may have started after us.
                if client.is_none() {
                    match McpClient::connect_as(unterm_protocol::ProcessRole::McpBridge) {
                        Ok(connected) => client = Some(connected),
                        Err(error) if requires_bridge_replacement(&error.to_string()) => {
                            respond(
                                &mut stdout,
                                id,
                                json!({
                                    "content": [ { "type": "text", "text": error.to_string() } ],
                                    "isError": true,
                                }),
                            )?;
                            return Ok(());
                        }
                        Err(_) => {}
                    }
                }
                let Some(live) = client.as_mut() else {
                    respond(
                        &mut stdout,
                        id,
                        json!({
                            "content": [ { "type": "text", "text": GUI_NOT_RUNNING } ],
                            "isError": true,
                        }),
                    )?;
                    continue;
                };
                if !identified {
                    if let Some(info) = &client_info {
                        // Best effort: an older server without the method
                        // still serves the call, under "anonymous".
                        let _ = live.call("agent.identify", info.clone());
                    }
                    identified = true;
                }
                match live.call(&name, arguments) {
                    Ok(result) => {
                        let text = serde_json::to_string_pretty(&result)
                            .unwrap_or_else(|_| result.to_string());
                        respond(
                            &mut stdout,
                            id,
                            json!({
                                "content": [ { "type": "text", "text": text } ],
                                "isError": false,
                            }),
                        )?;
                    }
                    Err(e) => {
                        // MCP convention: tool execution failures are returned
                        // as a successful response with isError=true, so the
                        // model can read + react to the error text.
                        respond(
                            &mut stdout,
                            id,
                            json!({
                                "content": [ { "type": "text", "text": format!("{e}") } ],
                                "isError": true,
                            }),
                        )?;
                        // A server can be replaced while this bridge is alive.
                        // Drop the dead connection so the next request performs
                        // discovery and either reconnects or enters drain.
                        client = None;
                        identified = false;
                    }
                }
            }
            _ => {
                if !is_notification {
                    respond_err(
                        &mut stdout,
                        id,
                        -32601,
                        &format!("method not found: {method}"),
                    )?;
                }
            }
        }
    }

    Ok(())
}

fn requires_bridge_replacement(message: &str) -> bool {
    [
        "product_version_mismatch:",
        "protocol_incompatible:",
        "data_schema_incompatible:",
    ]
    .iter()
    .any(|prefix| message.starts_with(prefix))
}

/// Convert `meta.surface`'s mcp_methods array into MCP tool descriptors with
/// a JSON-Schema inputSchema derived from each method's param list.
fn build_tool_list(surface: &Value) -> Vec<Value> {
    let methods = surface
        .get("mcp_methods")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    methods
        .iter()
        .filter_map(|m| {
            let name = m.get("name").and_then(|v| v.as_str())?;
            // meta.surface is itself proxyable but pointless to expose as a
            // tool to the agent — it'd just be describing the toolset the
            // agent already received. Skip it.
            if name == "meta.surface" {
                return None;
            }
            let summary = m.get("summary").and_then(|v| v.as_str()).unwrap_or("");
            let mut properties = serde_json::Map::new();
            let mut required: Vec<Value> = Vec::new();
            if let Some(params) = m.get("params").and_then(|v| v.as_array()) {
                for p in params {
                    let pname = match p.get("name").and_then(|v| v.as_str()) {
                        Some(n) => n,
                        None => continue,
                    };
                    let kind = p.get("kind").and_then(|v| v.as_str()).unwrap_or("string");
                    let psummary = p.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                    properties.insert(
                        pname.to_string(),
                        json!({ "type": json_schema_type(kind), "description": psummary }),
                    );
                    if p.get("required").and_then(|v| v.as_bool()).unwrap_or(false) {
                        required.push(Value::String(pname.to_string()));
                    }
                }
            }
            Some(json!({
                "name": name,
                "description": summary,
                "inputSchema": {
                    "type": "object",
                    "properties": Value::Object(properties),
                    "required": required,
                },
            }))
        })
        .collect()
}

/// Map our param `kind` strings to JSON-Schema `type` values. Keep unions as
/// real JSON Schema type arrays so older clients that pass numeric pane ids do
/// not get rejected before the bridge can proxy the call.
fn json_schema_type(kind: &str) -> Value {
    let parts: Vec<Value> = kind
        .split('|')
        .map(|part| match part.trim() {
            "int" => "integer",
            "bool" => "boolean",
            "number" | "float" => "number",
            "array" => "array",
            "object" => "object",
            "null" => "null",
            _ => "string",
        })
        .map(|kind| Value::String(kind.to_string()))
        .collect();
    match parts.as_slice() {
        [single] => single.clone(),
        _ => Value::Array(parts),
    }
}

fn respond(out: &mut impl Write, id: Option<Value>, result: Value) -> Result<()> {
    write_msg(
        out,
        &json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": result }),
    )
}

fn respond_err(out: &mut impl Write, id: Option<Value>, code: i64, message: &str) -> Result<()> {
    write_msg(
        out,
        &json!({
            "jsonrpc": "2.0",
            "id": id.unwrap_or(Value::Null),
            "error": { "code": code, "message": message },
        }),
    )
}

fn write_msg(out: &mut impl Write, msg: &Value) -> Result<()> {
    let mut s = serde_json::to_string(msg).map_err(|e| anyhow!("serialize MCP message: {e}"))?;
    s.push('\n');
    out.write_all(s.as_bytes())?;
    out.flush()?;
    Ok(())
}

fn normalize_input_line(line: &str) -> &str {
    line.trim().trim_start_matches('\u{feff}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_compatibility_failures_trigger_bridge_replacement() {
        for message in [
            "product_version_mismatch: old bridge",
            "protocol_incompatible: major changed",
            "data_schema_incompatible: newer store",
        ] {
            assert!(requires_bridge_replacement(message), "{message}");
        }
        assert!(!requires_bridge_replacement(GUI_NOT_RUNNING));
        assert!(!requires_bridge_replacement("MCP read failed"));
    }

    #[test]
    fn drain_response_uses_a_stable_json_rpc_code() {
        let mut output = Vec::new();
        respond_err(
            &mut output,
            Some(json!(7)),
            -32010,
            "protocol_incompatible: restart",
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["id"], 7);
        assert_eq!(value["error"]["code"], -32010);
        assert_eq!(value["error"]["message"], "protocol_incompatible: restart");
    }

    #[test]
    fn input_lines_tolerate_utf8_bom() {
        assert_eq!(
            normalize_input_line("\u{feff}{\"jsonrpc\":\"2.0\"}\r\n"),
            "{\"jsonrpc\":\"2.0\"}"
        );
    }

    #[test]
    fn tool_schema_maps_array_params_to_array_type() {
        assert_eq!(json_schema_type("array"), "array");

        let tools = build_tool_list(&json!({
            "mcp_methods": [{
                "name": "session.create",
                "summary": "Spawn a new tab.",
                "params": [
                    { "name": "argv", "kind": "array", "required": false, "summary": "Program argv array." }
                ]
            }]
        }));

        assert_eq!(
            tools[0]["inputSchema"]["properties"]["argv"]["type"],
            "array"
        );
    }

    #[test]
    fn tool_schema_preserves_union_param_types() {
        assert_eq!(json_schema_type("string|int"), json!(["string", "integer"]));

        let tools = build_tool_list(&json!({
            "mcp_methods": [{
                "name": "session.status",
                "summary": "Get pane state.",
                "params": [
                    { "name": "pane_id", "kind": "string|int", "required": false, "summary": "Target pane." }
                ]
            }]
        }));

        assert_eq!(
            tools[0]["inputSchema"]["properties"]["pane_id"]["type"],
            json!(["string", "integer"])
        );
    }

    #[test]
    fn tool_list_matches_public_mcp_metadata_except_meta_surface() {
        let surface = json!({
            "mcp_methods": unterm_agents::mcp_meta::MCP_METHODS,
        });
        let tools = build_tool_list(&surface);
        let tool_names: std::collections::BTreeSet<_> = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(|value| value.as_str()))
            .collect();
        let expected: std::collections::BTreeSet<_> = unterm_agents::mcp_meta::MCP_METHODS
            .iter()
            .map(|method| method.name)
            .filter(|name| *name != "meta.surface")
            .collect();

        assert_eq!(tool_names, expected);
    }
}
