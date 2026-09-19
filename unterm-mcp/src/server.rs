//! TCP server for the Unterm MCP JSON-RPC protocol.
//!
//! Binds 127.0.0.1 with a preferred-port-then-fallback strategy (see
//! `unterm_services::server_info`), authenticates clients with the UUID token written
//! to `~/.unterm/server.json`, and dispatches each request to the handler
//! module.

use super::handler::{ConnectionContext, McpHandler};
use anyhow::Result;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use unterm_services::server_info::{self, MCP_PREFERRED_PORT, SERVER_BIND};

/// Monotonically-increasing connection ID assigned to each accepted
/// client. Used by the handler to bind `agent.identify` claims to a
/// specific TCP connection so two concurrent agents claiming the same
/// name don't merge their state.
static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

/// Bind the MCP server, write the initial `server.json`, and start
/// accepting clients on a background thread. Returns the bound port and the
/// generated auth token.
pub fn start_mcp_server() -> (u16, String) {
    start_mcp_server_with_version(unterm_protocol::PRODUCT_VERSION)
}

/// Start the MCP server and publish the owning product binary's version in the
/// instance registry.  The MCP crate's version is an internal implementation
/// detail and can legitimately differ from the GUI product version.
pub fn start_mcp_server_with_version(product_version: &str) -> (u16, String) {
    // Trust the user gave by clicking "always allow" moves into the grant
    // store, once, before anything can read it from the old file. Done here
    // rather than lazily so the import happens while nothing is racing to
    // create the same grants.
    crate::handler::migrate_trusted_agents_to_grants();

    let (listener, port) = match server_info::bind_with_fallback(MCP_PREFERRED_PORT) {
        Ok(pair) => pair,
        Err(e) => {
            log::error!("MCP server failed to bind any port: {}", e);
            return (0, String::new());
        }
    };

    let info = match server_info::write_initial_with_version(port, product_version) {
        Ok(info) => info,
        Err(e) => {
            log::error!("Could not write ~/.unterm/server.json: {}", e);
            return (port, String::new());
        }
    };

    // If we were launched with `--profile X` (which sets
    // UNTERM_STARTUP_PROFILE in main.rs), resolve it against the
    // profile registry and stamp the resulting ID into this
    // instance's JSON now — before any pane gets spawned, so the
    // very first shell already inherits the profile's env. Failures
    // log a warning and the window just starts un-bound, which is
    // strictly better than blocking startup.
    apply_startup_profile_binding();

    // First-run discovery: make every AI agent on this machine aware of
    // Unterm (register the `unterm` MCP server + drop a context-file note)
    // without the user having to do anything. Best-effort, detached, and
    // gated by a per-version stamp so it does real work only on first launch
    // of each new version. The spawned `unterm-cli setup-ai` is purely local
    // file I/O — it doesn't need this server to be up.
    maybe_register_ai_agents();

    let token = info.auth_token.clone();
    let token_for_thread = token.clone();
    thread::Builder::new()
        .name("mcp-server".into())
        .spawn(move || {
            if let Err(e) = run_server(listener, &token_for_thread) {
                log::error!("MCP server error: {}", e);
            }
        })
        .expect("Failed to spawn MCP server thread");

    log::info!("MCP server listening on {}:{}", SERVER_BIND, port);
    (port, token)
}

/// Once per version, spawn `unterm-cli setup-ai` to auto-register Unterm with
/// the AI agents installed on this machine. Runs on a detached thread so the
/// stamp read + process spawn never touch the window-display startup path.
/// Gated by `~/.unterm/setup-ai.stamp` (written by setup-ai on a clean run) so
/// a steady-state launch does nothing. Entirely best-effort: any failure is
/// logged and ignored — discovery is a convenience, never a reason to block.
fn maybe_register_ai_agents() {
    let Some(stamp) = unterm_protocol::state_path("setup-ai.stamp") else {
        return;
    };
    thread::Builder::new()
        .name("setup-ai-register".into())
        .spawn(move || {
            let current = unterm_protocol::PRODUCT_VERSION;
            if std::fs::read_to_string(&stamp)
                .map(|s| s.trim() == current)
                .unwrap_or(false)
            {
                return; // already registered for this version
            }

            // The CLI bridge ships as a sibling of this GUI binary (same
            // pattern as the Web Settings "copy launch command" path).
            // EXE_SUFFIX makes this find `unterm-cli.exe` on Windows, not just
            // bare `unterm-cli`. Fall back to a PATH lookup otherwise.
            let cli_name = format!("unterm-cli{}", std::env::consts::EXE_SUFFIX);
            let cli = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|d| d.join(&cli_name)))
                .filter(|p| p.exists())
                .map(|p| p.into_os_string())
                .unwrap_or_else(|| std::ffi::OsString::from(cli_name));

            let mut cmd = std::process::Command::new(&cli);
            cmd.arg("setup-ai")
                // Stamp the value WE gate on, so the first-run check stays
                // consistent even if the GUI and CLI crate versions diverge.
                .env("UNTERM_SETUP_AI_STAMP", current)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            #[cfg(windows)]
            {
                // CREATE_NO_WINDOW: unterm-cli is a console binary; without
                // this it flashes a console window on first-run registration.
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000);
            }
            match cmd.spawn() {
                // Wait so the thread (and any test harness) can observe the
                // child finished; on the GUI it's a detached worker thread so
                // this blocks nothing the user sees.
                Ok(mut child) => {
                    log::info!(
                        "setup-ai: registering Unterm with AI agents (first run for v{current})"
                    );
                    let _ = child.wait();
                }
                Err(e) => log::warn!("setup-ai: could not spawn {:?}: {}", cli, e),
            }
        })
        .ok();
}

/// Honor the `UNTERM_STARTUP_PROFILE` env var (set by `unterm --profile X`)
/// by resolving the name through the profile registry and persisting
/// the matched ID into this instance's JSON. Failures degrade
/// gracefully: a missing registry, a typo in the name, or a keychain
/// hiccup all just log a warning. We *always* clear the env var
/// afterward so child processes spawned inside Unterm don't inherit
/// the marker and re-attempt to claim the profile.
fn apply_startup_profile_binding() {
    let explicit = std::env::var("UNTERM_STARTUP_PROFILE").unwrap_or_default();
    std::env::remove_var("UNTERM_STARTUP_PROFILE");

    let registry = match unterm_profile::ProfileRegistry::load() {
        Ok(r) => r,
        Err(e) => {
            if !explicit.is_empty() {
                log::warn!("startup profile {explicit:?}: registry load failed: {e:#}");
            }
            return;
        }
    };

    // Three-tier resolution:
    //   1. `--profile X` passed → resolve `X` (display name OR ID OR
    //      unique prefix). Missing match logs a warning and the
    //      window starts un-bound.
    //   2. No flag, but `index.toml` has a `default` set → use it
    //      silently. Common path for users who pick one profile in
    //      Settings and want every new window to inherit it.
    //   3. No flag and no default → window starts un-bound, panes
    //      spawn with the user's normal env. This is the v0.12
    //      behavior so existing workflows aren't disturbed.
    let resolved: Option<String> = if !explicit.is_empty() {
        match registry.resolve(&explicit) {
            Some((id, _)) => Some(id.to_string()),
            None => {
                log::warn!(
                    "startup profile {explicit:?}: no match (try `unterm-cli profile list`)"
                );
                return;
            }
        }
    } else if let Some(id) = registry.default_id() {
        Some(id.to_string())
    } else {
        None
    };

    let Some(id) = resolved else {
        // No explicit, no default. Still re-sync SSH config so any
        // Edits made since last launch should propagate, but SSH config
        // regeneration does not need to hold the window's first paint.
        sync_ssh_config_after_startup(registry);
        return;
    };

    if let Err(e) = server_info::set_profile(Some(id.clone())) {
        log::warn!("startup profile set_profile({id:?}) failed: {e:#}");
        return;
    }
    // Regenerate the SSH config fragment at startup so users who edit
    // profiles between Unterm sessions don't end up with a stale
    // config.unterm referencing deleted entries. Do it off the display
    // startup path; the profile binding above is the only synchronous part
    // required before the first pane.
    sync_ssh_config_after_startup(registry);
    log::info!("Instance bound to profile {id}");
}

fn sync_ssh_config_after_startup(registry: unterm_profile::ProfileRegistry) {
    thread::Builder::new()
        .name("startup-ssh-config-sync".into())
        .spawn(move || {
            if let Err(e) = registry.sync_ssh_config() {
                log::warn!("startup SSH config sync failed: {e:#}");
            }
        })
        .ok();
}

/// Serve MCP without a front end and without registry side effects:
/// bind an ephemeral port, authenticate with the caller's token, and
/// dispatch on a background thread. The Core process hosts the agent
/// surface this way — discovery goes through the Core's own record,
/// not `server.json`, so a GUI's instance registration is never
/// contested and the two servers can coexist during the migration.
pub fn start_headless_mcp_server(auth_token: &str) -> Result<u16> {
    let listener = TcpListener::bind((SERVER_BIND, 0))?;
    let port = listener.local_addr()?.port();
    let token = auth_token.to_string();
    thread::Builder::new()
        .name("mcp-server".into())
        .spawn(move || {
            if let Err(e) = run_server(listener, &token) {
                log::error!("headless MCP server error: {}", e);
            }
        })?;
    log::info!("headless MCP server listening on {}:{}", SERVER_BIND, port);
    Ok(port)
}

fn run_server(listener: TcpListener, auth_token: &str) -> Result<()> {
    let handler = Arc::new(McpHandler::new());

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let handler = Arc::clone(&handler);
                let token = auth_token.to_string();
                thread::Builder::new()
                    .name("mcp-client".into())
                    .spawn(move || {
                        if let Err(e) = handle_client(stream, &token, &handler) {
                            log::debug!("MCP client disconnected: {}", e);
                        }
                    })
                    .ok();
            }
            Err(e) => {
                log::warn!("MCP accept error: {}", e);
            }
        }
    }
    Ok(())
}

fn handle_client(stream: TcpStream, auth_token: &str, handler: &McpHandler) -> Result<()> {
    stream.set_nodelay(true)?;
    let peer = stream.peer_addr()?;
    log::info!("MCP client connected: {}", peer);

    let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
    let ctx = ConnectionContext {
        conn_id,
        peer_addr: peer.to_string(),
    };

    let reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let mut authenticated = false;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let request: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let error_resp = make_error_response(
                    serde_json::Value::Null,
                    -32700,
                    &format!("Parse error: {}", e),
                );
                write_response(&mut writer, &error_resp)?;
                continue;
            }
        };

        let id = request
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = request
            .get("params")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        // Auth check
        if !authenticated {
            if method == "auth.login" {
                let client_token = params.get("token").and_then(|t| t.as_str()).unwrap_or("");
                if client_token == auth_token {
                    authenticated = true;
                    if let Some(client) = params
                        .get("client")
                        .cloned()
                        .and_then(|value| serde_json::from_value(value).ok())
                    {
                        handler.register_client(conn_id, client);
                    }
                    // Where this connection is speaking from, when it says
                    // so. A call that names no pane then means this one
                    // rather than whichever the user is looking at.
                    if let Some(pane) = params.get("pane_id").and_then(|v| v.as_u64()) {
                        handler.register_caller_pane(conn_id, pane as usize);
                    }
                    let resp = make_success_response(id, serde_json::json!({"status": "ok"}));
                    write_response(&mut writer, &resp)?;
                } else {
                    let resp = make_error_response(id, -32001, "Invalid auth token");
                    write_response(&mut writer, &resp)?;
                }
                continue;
            } else {
                let resp =
                    make_error_response(id, -32002, "Not authenticated. Call auth.login first");
                write_response(&mut writer, &resp)?;
                continue;
            }
        }

        // Dispatch to handler
        let result = handler.handle(&ctx, method, &params);
        let resp = match result {
            Ok(value) => make_success_response(id, value),
            Err(e) => make_error_response(id, -32603, &e.to_string()),
        };
        write_response(&mut writer, &resp)?;
    }

    // Free any per-connection state (notably the agent.identify claim)
    // so we don't leak entries for short-lived clients.
    handler.drop_connection(conn_id);
    log::info!("MCP client disconnected: {}", peer);
    Ok(())
}

fn write_response(writer: &mut impl Write, resp: &serde_json::Value) -> Result<()> {
    let mut data = serde_json::to_string(resp)?;
    data.push('\n');
    writer.write_all(data.as_bytes())?;
    writer.flush()?;
    Ok(())
}

fn make_success_response(id: serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn make_error_response(id: serde_json::Value, code: i32, message: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    })
}
