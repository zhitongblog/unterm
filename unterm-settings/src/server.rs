//! Hand-rolled synchronous HTTP/1.1 server. No async runtime, no extra deps.
//!
//! The protocol surface we care about is small: GET / POST, plain
//! Content-Length bodies, no chunked encoding, no keep-alive (we close after
//! every response). Browsers are happy talking to this just fine; the SPA
//! does no fancy upgrades.
//!
//! Routes are documented in the project spec; in summary:
//!   GET  /                              -> SPA shell
//!   GET  /static/<name>                 -> embedded asset
//!   GET  /bootstrap.json                -> auth token + ports (no auth)
//!   GET  /api/health                    -> liveness check (auth)
//!   GET  /api/state                     -> aggregate snapshot (auth)
//!   POST /api/proxy                     -> proxy_configure / proxy_disable
//!   POST /api/theme                     -> persist theme + repaint GUI
//!   POST /api/recording/start           -> recording::start_recording
//!   POST /api/recording/stop            -> recording::stop_recording
//!   GET  /api/sessions                  -> recording::list_sessions
//!   GET  /api/sessions/:id/markdown     -> recording::read_session_markdown

use crate::assets;
use crate::console;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use unterm_mcp::handler::McpHandler;
use unterm_services::server_info::{self, HTTP_PREFERRED_PORT, SERVER_BIND};

const READ_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Bind the HTTP server, register its port in `~/.unterm/server.json`, and
/// start serving on a background thread. Returns the bound port.
pub fn start_web_settings_server(auth_token: String) -> u16 {
    let (listener, port) = match server_info::bind_with_fallback(HTTP_PREFERRED_PORT) {
        Ok(pair) => pair,
        Err(e) => {
            log::error!("HTTP settings server failed to bind: {}", e);
            return 0;
        }
    };

    if let Err(e) = server_info::set_http_port(port) {
        log::warn!("Could not stamp http_port in server.json: {}", e);
    }

    let handler = Arc::new(McpHandler::new());
    thread::Builder::new()
        .name("web-settings".into())
        .spawn(move || run_server(listener, auth_token, handler))
        .expect("Failed to spawn web settings thread");

    log::info!(
        "Web settings server listening on http://{}:{}/",
        SERVER_BIND,
        port
    );
    port
}

fn run_server(listener: TcpListener, auth_token: String, handler: Arc<McpHandler>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let token = auth_token.clone();
                let handler = Arc::clone(&handler);
                thread::Builder::new()
                    .name("web-settings-conn".into())
                    .spawn(move || {
                        if let Err(e) = handle_client(stream, &token, &handler) {
                            log::debug!("HTTP client error: {}", e);
                        }
                    })
                    .ok();
            }
            Err(e) => log::warn!("HTTP accept error: {}", e),
        }
    }
}

fn handle_client(mut stream: TcpStream, auth_token: &str, handler: &McpHandler) -> Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok();
    stream.set_write_timeout(Some(WRITE_TIMEOUT)).ok();
    stream.set_nodelay(true).ok();

    let req = match parse_request(&mut stream) {
        Ok(r) => r,
        Err(e) => {
            log::debug!("malformed HTTP request: {}", e);
            let _ = write_status_response(&mut stream, 400, "Bad Request", b"bad request");
            return Ok(());
        }
    };

    let resp = route(&req, auth_token, handler);
    write_response(&mut stream, &resp)
}

#[derive(Debug)]
struct Request {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

pub(crate) struct Response {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
    extra_headers: Vec<(&'static str, String)>,
}

impl Response {
    pub(crate) fn json(status: u16, reason: &'static str, value: Value) -> Self {
        Self {
            status,
            reason,
            content_type: "application/json; charset=utf-8",
            body: serde_json::to_vec(&value).unwrap_or_default(),
            extra_headers: Vec::new(),
        }
    }

    fn text(status: u16, reason: &'static str, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            reason,
            content_type,
            body,
            extra_headers: Vec::new(),
        }
    }

    pub(crate) fn ok_json(value: Value) -> Self {
        Self::json(200, "OK", value)
    }

    pub(crate) fn err(status: u16, reason: &'static str, message: &str) -> Self {
        Self::json(status, reason, json!({"error": message}))
    }

    /// A bare redirect. Used to put the trailing slash back on `/console`
    /// so the page's relative asset URLs resolve under it.
    fn redirect(status: u16, location: &str) -> Self {
        Self {
            status,
            reason: "Moved Permanently",
            content_type: "text/plain; charset=utf-8",
            body: Vec::new(),
            extra_headers: vec![("Location", location.to_string())],
        }
    }
}

fn parse_request(stream: &mut TcpStream) -> Result<Request> {
    let mut reader = BufReader::new(stream.try_clone()?);

    // Request line
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let line = line.trim_end_matches(['\r', '\n']);
    let mut parts = line.splitn(3, ' ');
    let method = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("no method"))?
        .to_string();
    let target = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("no target"))?
        .to_string();
    let _version = parts.next().unwrap_or("HTTP/1.1");

    let (path, query) = match target.find('?') {
        Some(i) => (target[..i].to_string(), target[i + 1..].to_string()),
        None => (target, String::new()),
    };

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut total = 0usize;
    loop {
        let mut buf = String::new();
        let n = reader.read_line(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n;
        if total > MAX_HEADER_BYTES {
            anyhow::bail!("headers too large");
        }
        let trimmed = buf.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(idx) = trimmed.find(':') {
            let k = trimmed[..idx].trim().to_string();
            let v = trimmed[idx + 1..].trim().to_string();
            headers.push((k, v));
        }
    }

    let content_length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        anyhow::bail!("body too large");
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    Ok(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn write_response(stream: &mut TcpStream, resp: &Response) -> Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n",
        resp.status,
        resp.reason,
        resp.content_type,
        resp.body.len()
    );
    for (k, v) in &resp.extra_headers {
        head.push_str(k);
        head.push_str(": ");
        head.push_str(v);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&resp.body)?;
    stream.flush()?;
    Ok(())
}

fn write_status_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &'static str,
    body: &[u8],
) -> Result<()> {
    let resp = Response::text(status, reason, "text/plain; charset=utf-8", body.to_vec());
    write_response(stream, &resp)
}

fn route(req: &Request, auth_token: &str, handler: &McpHandler) -> Response {
    let path = req.path.as_str();

    // Public (no auth) endpoints
    if req.method == "GET" && path == "/" {
        return Response::text(
            200,
            "OK",
            "text/html; charset=utf-8",
            assets::INDEX_HTML.as_bytes().to_vec(),
        );
    }
    if req.method == "GET" && path == "/favicon.ico" {
        // Browsers fetch this automatically; respond with an empty 204 so we
        // don't pollute the console with 401s.
        return Response::text(204, "No Content", "image/x-icon", Vec::new());
    }
    if req.method == "GET" && path == "/bootstrap.json" {
        // Return THIS instance's token + ports, not whichever instance
        // happens to be the registered "active" pointer. Critical for
        // multi-instance: when alpha is the active pointer and bravo's
        // Web Settings page bootstraps, it must get bravo's token —
        // otherwise the SPA tries to talk to its own HTTP server with
        // alpha's token and every subsequent /api/* call 401s.
        //
        // `read_current()` reads this process's per-instance JSON file
        // (`~/.unterm/instances/<id>.json`), so the answer is always
        // self-consistent with the server actually handling the
        // request. We deliberately do NOT fall back to the active
        // pointer when read_current fails — that would re-introduce
        // the cross-instance leak. An empty token field is the safer
        // failure mode: the SPA shows its bootstrap-failed error and
        // the user can investigate, rather than silently auth'ing
        // against the wrong instance.
        let info = server_info::read_current();
        // unterm_cli_path is the absolute path to the unterm-cli binary
        // sibling of this running GUI executable. The SPA emits this in
        // "Copy launch command" so it works even when the user's $PATH
        // doesn't include the .app bundle's MacOS dir (every fresh DMG
        // install on macOS hits this — there's no automatic symlink to
        // /usr/local/bin). Falls back to bare `unterm-cli` if we can't
        // resolve the executable path (extremely unlikely on Unix; might
        // matter for Wine / weird mount scenarios).
        let unterm_cli_path = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|d| d.join("unterm-cli")))
            .filter(|p| p.exists())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "unterm-cli".to_string());
        return Response::ok_json(json!({
            "auth_token": info.auth_token,
            "mcp_port": info.mcp_port,
            "http_port": info.http_port,
            "platform": std::env::consts::OS,
            "unterm_cli_path": unterm_cli_path,
        }));
    }
    if req.method == "GET" {
        if let Some(name) = path.strip_prefix("/static/") {
            if let Some((ct, body)) = assets::lookup_static(name) {
                return Response::text(200, "OK", ct, body.as_bytes().to_vec());
            }
            return Response::err(404, "Not Found", "no such static asset");
        }

        // The Unzoo One console, served from disk. Unauthenticated like the
        // settings SPA above: the page then reads /bootstrap.json for the
        // token and carries it on every /api/* call.
        if path == "/console" {
            // Without the trailing slash the page's relative asset URLs would
            // resolve against "/" instead of "/console/".
            return Response::redirect(301, "/console/");
        }
        if let Some(rest) = path.strip_prefix("/console/") {
            return match console::lookup(rest) {
                Some((ct, body)) => Response::text(200, "OK", ct, body),
                None if console::console_dir().is_none() => Response::err(
                    503,
                    "Service Unavailable",
                    "console assets not installed; build unzoo-one with `npm run build:static` \
                     and point UNTERM_CONSOLE_DIR at its dist/client",
                ),
                None => Response::err(404, "Not Found", "no such console asset"),
            };
        }

        // The console's HTML references its assets by absolute path
        // (/assets/…, /unzoo-favicon.png). Serving them only under /console/
        // would 401 every one of them, so fall through to the console
        // directory for anything the settings server itself does not claim.
        // The settings SPA lives at / and /static/, both handled above.
        if let Some((ct, body)) = console::lookup(path) {
            return Response::text(200, "OK", ct, body);
        }
    }

    // Everything else demands the bearer token.
    if !auth_ok(req, auth_token) {
        return Response::err(401, "Unauthorized", "missing or bad bearer token");
    }

    match (req.method.as_str(), path) {
        ("GET", "/api/health") => Response::ok_json(json!({"ok": true})),
        ("GET", "/api/state") => api_state(handler),
        ("GET", "/api/shells") => api_shells(),
        ("POST", "/api/shells/install") => api_shell_install(&req.body),
        ("POST", "/api/shells/default") => api_shell_default_set(&req.body),
        ("POST", "/api/proxy") => api_proxy(handler, &req.body),
        ("POST", "/api/proxy/rotation") => api_proxy_rotation(handler, &req.body),
        ("POST", "/api/proxy/nodes") => api_proxy_set_nodes(handler, &req.body),
        ("GET", "/api/approvals") => api_approvals(handler),
        ("POST", "/api/approvals/decide") => api_approval_decide(handler, &req.body),
        ("GET", "/api/providers") => api_providers(handler, &req.query),
        ("POST", "/api/providers/bind") => api_provider_act(handler, "provider.bind", &req.body),
        ("POST", "/api/providers/pause") => api_provider_act(handler, "provider.pause", &req.body),
        ("POST", "/api/providers/resume") => api_provider_act(handler, "provider.resume", &req.body),
        ("POST", "/api/providers/unbind") => api_provider_act(handler, "provider.unbind", &req.body),
        ("POST", "/api/providers/diagnose") => {
            api_provider_act(handler, "provider.diagnose", &req.body)
        }
        ("GET", "/api/providers/leases") => api_provider_leases(handler),
        ("POST", "/api/providers/leases/revoke") => {
            api_provider_act(handler, "provider.revoke_lease", &req.body)
        }
        ("GET", "/api/proxy/clash") => api_proxy_clash_status(handler),
        ("POST", "/api/proxy/clash/select") => api_proxy_clash_select(handler, &req.body),
        ("POST", "/api/proxy/clash/controller") => api_proxy_clash_controller(handler, &req.body),
        ("POST", "/api/theme") => api_theme(&req.body),
        ("GET", "/api/scrollback") => api_scrollback_get(),
        ("POST", "/api/scrollback") => api_scrollback_set(&req.body),
        ("GET", "/api/compat") => api_compat_get(),
        ("POST", "/api/compat") => api_compat_set(&req.body),
        ("GET", "/api/updates") => api_updates_get(),
        ("POST", "/api/updates/check") => api_updates_check(),
        ("POST", "/api/recording/start") => api_recording(handler, &req.body, true),
        ("POST", "/api/recording/stop") => api_recording(handler, &req.body, false),
        ("GET", "/api/sessions") => api_sessions(handler, &req.query),
        ("GET", p) if p.starts_with("/api/sessions/") && p.ends_with("/markdown") => {
            api_session_markdown(handler, p)
        }
        // Agent Cockpit — the Review page.
        ("GET", "/api/review/overview") => {
            Response::ok_json(unterm_services::cockpit::verification::enrich_overview(
                unterm_services::cockpit::observability::enrich_overview(
                    unterm_services::cockpit::review::overview(),
                ),
            ))
        }
        ("GET", "/api/review/inbox") => api_review_inbox(),
        ("GET", "/api/review/diff") => api_review_diff(&req.query),
        ("POST", "/api/review/rollback") => api_review_rollback(&req.body),
        ("POST", "/api/review/merge") => api_review_member_action(&req.body, "merge"),
        ("POST", "/api/review/discard") => api_review_member_action(&req.body, "discard"),
        ("POST", "/api/review/verify") => api_review_verify(&req.body),
        ("POST", "/api/review/retry") => api_review_retry(&req.body),
        ("POST", "/api/fleet/retry") => api_fleet_retry(&req.body),
        ("POST", "/api/review/clean") => api_review_clean(&req.body),
        ("GET", "/api/reference") => api_reference(),
        ("GET", "/api/i18n") => api_i18n_state(),
        ("POST", "/api/i18n") => api_i18n_set(&req.body),
        ("GET", p) if p.starts_with("/api/i18n/") => {
            let code = &p["/api/i18n/".len()..];
            api_i18n_dict(code)
        }
        // Identity profile management — backs the Settings panel and the
        // onboarding wizard. List/get are pure reads; the rest mutate
        // `~/.unterm/profiles/` and (for secrets) the OS keychain.
        // MCP trust + audit surface for the Settings panel. Read endpoints
        // are cheap (one lock acquire each); write endpoints persist to
        // ~/.unterm/trusted_agents.json synchronously.
        ("GET", "/api/mcp/trusted") => Response::ok_json(unterm_mcp::handler::trust_snapshot()),
        ("POST", "/api/mcp/trust") => api_mcp_trust(&req.body),
        ("POST", "/api/mcp/untrust") => api_mcp_untrust(&req.body),
        ("GET", "/api/mcp/audit") => api_mcp_audit(&req.query),
        ("GET", "/api/profile/list") => api_profile_list(),
        ("POST", "/api/profile/create") => api_profile_create(&req.body),
        ("GET", "/api/profile/import-scan") => api_profile_import_scan(),
        ("GET", "/api/profile/ssh-include-status") => api_profile_ssh_include_status(),
        ("POST", "/api/profile/install-ssh-include") => api_profile_install_ssh_include(),
        ("POST", "/api/profile/set-default") => api_profile_set_default(&req.body),
        ("PUT", p) if p.starts_with("/api/profile/") && !p.contains("/secret") => {
            let id = &p["/api/profile/".len()..];
            api_profile_update(id, &req.body)
        }
        ("DELETE", p) if p.starts_with("/api/profile/") && !p.contains("/secret") => {
            let id = &p["/api/profile/".len()..];
            api_profile_delete(id)
        }
        ("POST", p) if p.starts_with("/api/profile/") && p.ends_with("/secret") => {
            let id = &p["/api/profile/".len()..p.len() - "/secret".len()];
            api_profile_set_secret(id, &req.body)
        }
        ("DELETE", p) if p.starts_with("/api/profile/") && p.contains("/secret/") => {
            // /api/profile/{id}/secret/{env_name}
            let rest = &p["/api/profile/".len()..];
            if let Some((id, env_name)) = rest.split_once("/secret/") {
                api_profile_delete_secret(id, env_name)
            } else {
                Response::err(400, "Bad Request", "malformed secret path")
            }
        }

        // ---- AI agent runtime --------------------------------------------
        // Routes wrap the unterm-agents crate so the Web Settings panel can
        // list / configure / install / launch every AI CLI in the signed
        // manifest. See `agents.rs` for the actual handlers.
        // Running a brain. Takes an agent id, never an argv — see agent_run.
        ("POST", "/api/agent/start") => super::agent_run::api_start(handler, &req.body),
        ("POST", "/api/agent/events") => {
            super::agent_run::api_session_act(handler, "agent_session.events", &req.body)
        }
        ("POST", "/api/agent/status") => {
            super::agent_run::api_session_act(handler, "agent_session.status", &req.body)
        }
        ("POST", "/api/agent/input") => {
            super::agent_run::api_session_act(handler, "agent_session.submit_input", &req.body)
        }
        ("POST", "/api/agent/interrupt") => {
            super::agent_run::api_session_act(handler, "agent_session.interrupt", &req.body)
        }
        ("POST", "/api/agent/close") => {
            super::agent_run::api_session_act(handler, "agent_session.close", &req.body)
        }
        // 在真实终端里驱动一个 brain。只认 agent id，只碰自己开的 pane。
        ("POST", "/api/pty/start") => super::pty_run::api_start(handler, &req.body),
        ("POST", "/api/pty/input") => super::pty_run::api_input(handler, &req.body),
        ("POST", "/api/pty/turn") => super::pty_run::api_turn(handler, &req.body),
        ("POST", "/api/pty/stop") => super::pty_run::api_stop(handler, &req.body),
        ("GET", "/api/agents/list") => super::agents::api_list(&req.query),
        ("GET", "/api/agents/manifest") => super::agents::api_manifest_info(),
        ("POST", "/api/agents/manifest/refresh") => super::agents::api_manifest_refresh(),
        ("GET", p) if p.starts_with("/api/agents/") && p.ends_with("/settings") => {
            let id = &p["/api/agents/".len()..p.len() - "/settings".len()];
            super::agents::api_settings_get(id, &req.query)
        }
        ("PUT", p) if p.starts_with("/api/agents/") && p.ends_with("/settings") => {
            let id = &p["/api/agents/".len()..p.len() - "/settings".len()];
            super::agents::api_settings_put(id, &req.body)
        }
        ("POST", p) if p.starts_with("/api/agents/") && p.ends_with("/install") => {
            let id = &p["/api/agents/".len()..p.len() - "/install".len()];
            super::agents::api_install(id)
        }
        ("POST", p) if p.starts_with("/api/agents/") && p.ends_with("/uninstall") => {
            let id = &p["/api/agents/".len()..p.len() - "/uninstall".len()];
            super::agents::api_uninstall(id)
        }
        ("POST", p) if p.starts_with("/api/agents/") && p.ends_with("/update") => {
            let id = &p["/api/agents/".len()..p.len() - "/update".len()];
            super::agents::api_update_agent(id)
        }
        ("POST", p) if p.starts_with("/api/agents/") && p.ends_with("/auth") => {
            let id = &p["/api/agents/".len()..p.len() - "/auth".len()];
            super::agents::api_auth(id, &req.body)
        }
        ("GET", p) if p.starts_with("/api/agents/") && p.ends_with("/import") => {
            let id = &p["/api/agents/".len()..p.len() - "/import".len()];
            super::agents::api_import(id, &req.query)
        }
        ("POST", p) if p.starts_with("/api/agents/") && p.ends_with("/launch-plan") => {
            let id = &p["/api/agents/".len()..p.len() - "/launch-plan".len()];
            super::agents::api_launch_plan(id, &req.body)
        }
        ("POST", p) if p.starts_with("/api/agents/") && p.ends_with("/run-plan") => {
            let id = &p["/api/agents/".len()..p.len() - "/run-plan".len()];
            super::agents::api_run_plan(id, &req.body)
        }
        ("GET", p)
            if p.starts_with("/api/agents/") && !p[("/api/agents/".len())..].contains('/') =>
        {
            // /api/agents/<id>  — catch-all GET for one agent's full manifest.
            // Match by "no further slash after /api/agents/"; the previous
            // p.matches('/').count() == 2 check was off-by-one (the string
            // has 3 slashes once the trailing id segment is present) and
            // silently 404'd, which made the SPA's openAgent() throw on
            // manifest fetch.
            let id = &p["/api/agents/".len()..];
            super::agents::api_show(id)
        }

        _ => Response::err(404, "Not Found", "no such route"),
    }
}

// --- Profile endpoints ----------------------------------------------------
//
// Profile state lives in three places: the TOML files under
// ~/.unterm/profiles/, the OS keychain (referenced from [secrets] via
// `keychain://...` URLs), and ~/.unterm/ssh/config.unterm. Mutating
// endpoints go through ProfileRegistry which keeps all three in sync
// — write a TOML, sync_ssh_config regenerates the SSH fragment, and
// set_secret round-trips through the keychain.
//
// Secret values are accepted from the SPA over the bearer-token-
// authenticated localhost HTTP server. They are never echoed back in
// any response — list/get endpoints surface keychain reference URLs
// only, so an open browser tab can't be tricked into reading a token.

fn profile_summary_json(
    id: &str,
    profile: &unterm_profile::ProfileFile,
    default_id: Option<&str>,
) -> Value {
    let today = chrono::Local::now().date_naive();
    let warnings: Vec<Value> = profile
        .expiration
        .iter()
        .filter_map(|(env, date_str)| {
            let date = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d").ok()?;
            let days = (date - today).num_days();
            if days <= 7 {
                Some(json!({
                    "env_name": env,
                    "expires_on": date_str,
                    "days_remaining": days,
                }))
            } else {
                None
            }
        })
        .collect();
    json!({
        "id": id,
        "display_name": profile.display_name,
        "accent_color": profile.accent_color,
        "description": profile.description,
        "secret_count": profile.secrets.len(),
        "secret_keys": profile.secrets.keys().collect::<Vec<_>>(),
        "git": {
            "user_name": profile.git.user_name,
            "user_email": profile.git.user_email,
        },
        "env": profile.env,
        "ssh": profile.ssh,
        "expiration": profile.expiration,
        "warnings": warnings,
        "is_default": default_id == Some(id),
    })
}

// --- MCP trust + audit endpoints ------------------------------------------

fn api_mcp_trust(body: &[u8]) -> Response {
    let v = parse_json_body(body);
    let name = match v.get("name").and_then(|x| x.as_str()) {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => return Response::err(400, "Bad Request", "name required"),
    };
    let added = unterm_mcp::handler::grant_trust(&name);
    Response::ok_json(json!({ "ok": true, "name": name, "added": added }))
}

fn api_mcp_untrust(body: &[u8]) -> Response {
    let v = parse_json_body(body);
    let name = match v.get("name").and_then(|x| x.as_str()) {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => return Response::err(400, "Bad Request", "name required"),
    };
    let removed = unterm_mcp::handler::revoke_trust(&name);
    Response::ok_json(json!({ "ok": true, "name": name, "removed": removed }))
}

fn api_mcp_audit(query: &str) -> Response {
    // ?limit=N caps the response. Default 100, max 1000 so the SPA
    // can't accidentally request the whole 1000-entry log on every
    // refresh. The audit log itself is already capped at the
    // mcp_audit_log_capacity config knob (default 1000).
    let limit = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(k, v)| (k == "limit").then(|| v.parse::<usize>().ok()).flatten())
        .unwrap_or(100)
        .min(1000);
    let payload_str = unterm_mcp::handler::audit_log_snapshot_json(limit);
    // audit_log_snapshot_json already returns a complete JSON value; we
    // serve it as-is rather than re-parsing + re-serializing.
    Response::text(
        200,
        "OK",
        "application/json; charset=utf-8",
        payload_str.into_bytes(),
    )
}

fn api_profile_list() -> Response {
    let registry = match unterm_profile::ProfileRegistry::load() {
        Ok(r) => r,
        Err(e) => return Response::err(500, "Internal Error", &e.to_string()),
    };
    let default = registry.default_id().map(str::to_string);
    let profiles: Vec<Value> = registry
        .iter_ordered()
        .into_iter()
        .map(|(id, p)| profile_summary_json(id, p, default.as_deref()))
        .collect();
    Response::ok_json(json!({
        "profiles": profiles,
        "default": default,
    }))
}

fn api_profile_create(body: &[u8]) -> Response {
    let v = parse_json_body(body);
    let display_name = match v.get("display_name").and_then(|x| x.as_str()) {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => return Response::err(400, "Bad Request", "display_name required"),
    };
    let accent = v.get("accent_color").and_then(|x| x.as_str());
    let mut registry = match unterm_profile::ProfileRegistry::load() {
        Ok(r) => r,
        Err(e) => return Response::err(500, "Internal Error", &e.to_string()),
    };
    match registry.create(&display_name, accent) {
        Ok(id) => Response::ok_json(json!({"id": id, "display_name": display_name})),
        Err(e) => Response::err(500, "Internal Error", &e.to_string()),
    }
}

fn api_profile_delete(id: &str) -> Response {
    use unterm_profile::SecretKey;
    let registry = match unterm_profile::ProfileRegistry::load() {
        Ok(r) => r,
        Err(e) => return Response::err(500, "Internal Error", &e.to_string()),
    };
    let Some(profile) = registry.get(id).cloned() else {
        return Response::err(404, "Not Found", "profile not found");
    };
    // Best-effort keychain cleanup before removing the TOML.
    if let Ok(store) = unterm_profile::default_store() {
        for env in profile.secrets.keys() {
            let _ = store.delete(&SecretKey::new(id, env));
        }
    }
    if let Err(e) = std::fs::remove_file(unterm_profile::profile_path(id)) {
        return Response::err(500, "Internal Error", &format!("remove profile: {e}"));
    }
    // Reload registry so sync_ssh_config regenerates from current state.
    let reloaded = unterm_profile::ProfileRegistry::load()
        .unwrap_or_else(|_| unterm_profile::ProfileRegistry::empty());
    let _ = reloaded.sync_ssh_config();
    Response::ok_json(json!({"ok": true}))
}

fn api_profile_update(id: &str, body: &[u8]) -> Response {
    let v = parse_json_body(body);
    let mut registry = match unterm_profile::ProfileRegistry::load() {
        Ok(r) => r,
        Err(e) => return Response::err(500, "Internal Error", &e.to_string()),
    };
    let Some(mut profile) = registry.get(id).cloned() else {
        return Response::err(404, "Not Found", "profile not found");
    };
    // Patch only the fields present in the body — partial update.
    if let Some(s) = v.get("display_name").and_then(|x| x.as_str()) {
        profile.display_name = s.to_string();
    }
    if let Some(s) = v.get("accent_color").and_then(|x| x.as_str()) {
        profile.accent_color = s.to_string();
    }
    if let Some(s) = v.get("description").and_then(|x| x.as_str()) {
        profile.description = s.to_string();
    }
    if let Some(g) = v.get("git").and_then(|x| x.as_object()) {
        if let Some(s) = g.get("user_name").and_then(|x| x.as_str()) {
            profile.git.user_name = s.to_string();
        }
        if let Some(s) = g.get("user_email").and_then(|x| x.as_str()) {
            profile.git.user_email = s.to_string();
        }
    }
    if let Some(map) = v.get("ssh").and_then(|x| x.as_object()) {
        profile.ssh.clear();
        for (k, val) in map {
            if let Some(s) = val.as_str() {
                profile.ssh.insert(k.clone(), s.to_string());
            }
        }
    }
    if let Some(map) = v.get("env").and_then(|x| x.as_object()) {
        profile.env.clear();
        for (k, val) in map {
            if let Some(s) = val.as_str() {
                profile.env.insert(k.clone(), s.to_string());
            }
        }
    }
    if let Err(e) = registry.save_profile(id, profile) {
        return Response::err(500, "Internal Error", &e.to_string());
    }
    Response::ok_json(json!({"ok": true}))
}

fn api_profile_set_secret(id: &str, body: &[u8]) -> Response {
    let v = parse_json_body(body);
    let env_name = match v.get("env_name").and_then(|x| x.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Response::err(400, "Bad Request", "env_name required"),
    };
    let value = match v.get("value").and_then(|x| x.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Response::err(400, "Bad Request", "non-empty value required"),
    };
    let mut registry = match unterm_profile::ProfileRegistry::load() {
        Ok(r) => r,
        Err(e) => return Response::err(500, "Internal Error", &e.to_string()),
    };
    let store = match unterm_profile::default_store() {
        Ok(s) => s,
        Err(e) => return Response::err(500, "Internal Error", &format!("keychain: {e}")),
    };
    if let Err(e) = registry.set_secret(store.as_ref(), id, &env_name, &value) {
        return Response::err(500, "Internal Error", &e.to_string());
    }
    Response::ok_json(json!({"ok": true, "env_name": env_name}))
}

fn api_profile_delete_secret(id: &str, env_name: &str) -> Response {
    use unterm_profile::SecretKey;
    let mut registry = match unterm_profile::ProfileRegistry::load() {
        Ok(r) => r,
        Err(e) => return Response::err(500, "Internal Error", &e.to_string()),
    };
    let Some(mut profile) = registry.get(id).cloned() else {
        return Response::err(404, "Not Found", "profile not found");
    };
    if profile.secrets.remove(env_name).is_none() {
        return Response::err(404, "Not Found", "secret not on this profile");
    }
    // Drop the keychain entry too. Idempotent — if it was already
    // removed out-of-band, the delete returns Ok.
    if let Ok(store) = unterm_profile::default_store() {
        let _ = store.delete(&SecretKey::new(id, env_name));
    }
    profile.expiration.remove(env_name);
    if let Err(e) = registry.save_profile(id, profile) {
        return Response::err(500, "Internal Error", &e.to_string());
    }
    Response::ok_json(json!({"ok": true}))
}

fn api_profile_set_default(body: &[u8]) -> Response {
    let v = parse_json_body(body);
    let id = v
        .get("id")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let mut registry = match unterm_profile::ProfileRegistry::load() {
        Ok(r) => r,
        Err(e) => return Response::err(500, "Internal Error", &e.to_string()),
    };
    if let Err(e) = registry.set_default(id.clone()) {
        return Response::err(500, "Internal Error", &e.to_string());
    }
    Response::ok_json(json!({"ok": true, "default": id}))
}

fn api_profile_import_scan() -> Response {
    let candidates = unterm_profile::sniff::scan_all();
    let json_list: Vec<Value> = candidates
        .into_iter()
        .map(|c| {
            json!({
                "source": c.source.as_str(),
                "label": c.label,
                "suggested_env_name": c.suggested_env_name,
                "has_value": c.suggested_value.is_some(),
                // We deliberately DON'T return `suggested_value` here even
                // though the sniffer has it — the SPA shouldn't display
                // raw tokens, and shipping them over HTTP (even on
                // localhost) is an unnecessary exposure. The wizard
                // passes the candidate identity (source/label/env_name)
                // back to the server, which re-runs the sniffer just for
                // that source to get a fresh value. See
                // api_profile_import_scan_value.
                "host": c.host,
                "user": c.user,
            })
        })
        .collect();
    Response::ok_json(json!({"candidates": json_list}))
}

fn api_profile_ssh_include_status() -> Response {
    Response::ok_json(json!({
        "installed": unterm_profile::ssh::user_config_has_include(),
        "user_config_path": unterm_profile::ssh::user_ssh_config_path()
            .map(|p| p.display().to_string()),
        "include_path": unterm_profile::ssh::ssh_config_unterm_path()
            .display()
            .to_string(),
    }))
}

fn api_profile_install_ssh_include() -> Response {
    let Some(path) = unterm_profile::ssh::user_ssh_config_path() else {
        return Response::err(500, "Internal Error", "no home dir");
    };
    // Read existing content (if any) and check for the Include line.
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing
        .lines()
        .any(|l| l.trim().starts_with("Include") && l.contains("config.unterm"))
    {
        return Response::ok_json(json!({"ok": true, "already_present": true}));
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Response::err(500, "Internal Error", &format!("create dir: {e}"));
        }
    }
    // Append (or create) — leading newline so we don't fuse with the
    // previous line if the file didn't end with one. The header
    // comment is preserved so the user knows where the line came from.
    let mut appended = existing;
    if !appended.is_empty() && !appended.ends_with('\n') {
        appended.push('\n');
    }
    appended.push_str(
        "\n# Added by Unterm — exposes per-profile SSH key routing.\n\
         Include ~/.unterm/ssh/config.unterm\n",
    );
    if let Err(e) = std::fs::write(&path, appended) {
        return Response::err(500, "Internal Error", &format!("write ssh config: {e}"));
    }
    Response::ok_json(json!({"ok": true, "already_present": false}))
}

// --- reference endpoint ----------------------------------------------------

/// `GET /api/reference` — proxies the `meta.surface` MCP method so the
/// Reference tab in Web Settings renders the same inventory an agent
/// would receive. No params, no auth state — pure read.
fn api_reference() -> Response {
    match unterm_mcp::meta::surface(&serde_json::Value::Null) {
        Ok(v) => Response::ok_json(v),
        Err(e) => Response::err(500, "Internal Server Error", &e.to_string()),
    }
}

// --- i18n endpoints --------------------------------------------------------

fn api_i18n_state() -> Response {
    let current = unterm_services::i18n::current_locale();
    let available: Vec<Value> = unterm_services::i18n::available_locales()
        .iter()
        .map(|(code, name)| json!({"code": code, "name": name}))
        .collect();
    let dict = unterm_services::i18n::dictionary(current)
        .map(|d| {
            let map: serde_json::Map<String, Value> = d
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect();
            Value::Object(map)
        })
        .unwrap_or_else(|| json!({}));
    Response::ok_json(json!({
        "current": current,
        "available": available,
        "dict": dict,
    }))
}

fn api_i18n_dict(code: &str) -> Response {
    match unterm_services::i18n::dictionary(code) {
        Some(d) => {
            let map: serde_json::Map<String, Value> = d
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect();
            Response::ok_json(Value::Object(map))
        }
        None => Response::err(
            404,
            "Not Found",
            &unterm_services::i18n::t("web.api.unknown_locale"),
        ),
    }
}

fn api_i18n_set(body: &[u8]) -> Response {
    let body = parse_json_body(body);
    let lang = match body.get("lang").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            return Response::err(
                400,
                "Bad Request",
                &unterm_services::i18n::t("web.api.invalid_lang"),
            )
        }
    };
    if !unterm_services::i18n::set_locale(&lang) {
        return Response::err(
            400,
            "Bad Request",
            &unterm_services::i18n::t("web.api.unknown_locale"),
        );
    }
    let current = unterm_services::i18n::current_locale();
    let available: Vec<Value> = unterm_services::i18n::available_locales()
        .iter()
        .map(|(code, name)| json!({"code": code, "name": name}))
        .collect();
    Response::ok_json(json!({
        "current": current,
        "available": available,
    }))
}

fn auth_ok(req: &Request, auth_token: &str) -> bool {
    if auth_token.is_empty() {
        return false;
    }
    match req.header("Authorization") {
        Some(v) => v
            .strip_prefix("Bearer ")
            .map(|t| t.trim() == auth_token)
            .unwrap_or(false),
        None => false,
    }
}

fn parse_json_body(body: &[u8]) -> Value {
    if body.is_empty() {
        return json!({});
    }
    serde_json::from_slice(body).unwrap_or(json!({}))
}

// --- API implementations --------------------------------------------------

fn api_state(handler: &McpHandler) -> Response {
    // Same instance-locality requirement as /bootstrap.json: each Web
    // Settings server must surface ITS OWN pid / started_at / ports,
    // not whichever instance happens to own active.json. The "Status"
    // line in the SPA header otherwise reads e.g. `127.0.0.1:19877`
    // (alpha) while the page is actually being served by bravo on
    // 19879, which is both confusing and breaks the "click to copy
    // a working /api/health curl command" affordance.
    let info = server_info::read_current();
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    let proxy = handler
        .handle(&ctx, "proxy.status", &json!({}))
        .unwrap_or_else(|e| json!({"error": e.to_string()}));

    let theme = current_theme_id();
    let project = current_project_info();
    let recording = current_recording_info();
    let sessions_path = sessions_path_string();
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_default();

    Response::ok_json(json!({
        // The release users know ("v0.55.0") first; the raw build stamp
        // (date-time-sha) stays in parentheses for bug reports. Showing
        // only the stamp made the Settings page read as "not updated"
        // right after installing a new release.
        "version": format!("v{} ({})", env!("CARGO_PKG_VERSION"), unterm_services::VERSION),
        "hostname": hostname,
        "pid": info.pid,
        "started_at": info.started_at,
        "ports": {
            "mcp": info.mcp_port,
            "http": info.http_port,
        },
        "theme": theme,
        "proxy": proxy,
        "project": project,
        "recording": recording,
        "sessions_path": sessions_path,
        "scrollback": {
            "lines": current_scrollback_lines(),
            "default": 10_000,
            "max": 999_999_999u64,
        },
    }))
}

fn api_shells() -> Response {
    Response::ok_json(json!({
        "platform": std::env::consts::OS,
        "shells": shell_inventory(),
        "default_shell": current_default_shell(),
    }))
}

fn current_default_shell() -> Value {
    let (config, _) = unterm_services::settings::load(None);
    let explicit = unterm_services::settings::Settings::from_config(&config).shell;
    let (source, argv) = if let Some(argv) = explicit {
        ("config", argv)
    } else {
        (
            "platform",
            unterm_services::settings::preferred_platform_shell().unwrap_or_default(),
        )
    };
    json!({
        "source": source,
        "argv": argv,
        "label": argv.first().map(|s| short_program_name(s)).unwrap_or_default(),
    })
}

fn api_shell_default_set(body: &[u8]) -> Response {
    let body = parse_json_body(body);
    let id = match body.get("id").and_then(|v| v.as_str()) {
        Some(id) if !id.trim().is_empty() => id.trim(),
        _ => return Response::err(400, "Bad Request", "missing shell id"),
    };
    let Some(argv) = shell_default_argv(id) else {
        return Response::err(400, "Bad Request", "shell cannot be used as a default");
    };
    if argv.is_empty() {
        return Response::err(400, "Bad Request", "shell command is empty");
    }
    if let Err(err) = write_default_shell(&argv) {
        return Response::err(500, "Internal Error", &err.to_string());
    }
    Response::ok_json(json!({
        "applied": true,
        "requires_new_shell": true,
        "default_shell": current_default_shell(),
    }))
}

fn api_shell_install(body: &[u8]) -> Response {
    let body = parse_json_body(body);
    let id = match body.get("id").and_then(|v| v.as_str()) {
        Some(id) if !id.trim().is_empty() => id.trim(),
        _ => return Response::err(400, "Bad Request", "missing shell id"),
    };
    let Some(plan) = shell_install_plan(id) else {
        return Response::err(400, "Bad Request", "shell has no supported installer");
    };
    if body
        .get("dry_run")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        return Response::ok_json(json!({
            "ok": true,
            "dry_run": true,
            "id": id,
            "command": plan.command_line(),
            "installer_available": program_exists(&plan.program),
            "requires_new_shell": true,
        }));
    }
    if !program_exists(&plan.program) {
        return Response::err(
            409,
            "Conflict",
            &format!("installer '{}' was not found", plan.program),
        );
    }
    match std::process::Command::new(&plan.program)
        .args(&plan.args)
        .spawn()
    {
        Ok(_) => Response::ok_json(json!({
            "ok": true,
            "dry_run": false,
            "id": id,
            "command": plan.command_line(),
            "requires_new_shell": true,
        })),
        Err(e) => Response::err(500, "Internal Error", &format!("launch installer: {e}")),
    }
}

struct ShellInstallPlan {
    program: String,
    args: Vec<String>,
}

impl ShellInstallPlan {
    fn command_line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .map(shell_arg)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn shell_inventory() -> Vec<Value> {
    let mut shells = Vec::new();
    push_shell(
        &mut shells,
        "pwsh",
        "PowerShell 7",
        "Modern PowerShell recommended for Windows users",
        &["pwsh.exe", "pwsh"],
        &[
            "-NoLogo",
            "-NoProfile",
            "-Command",
            "$PSVersionTable.PSVersion.ToString()",
        ],
        shell_install_plan("pwsh").map(|p| p.command_line()),
        true,
    );
    push_shell(
        &mut shells,
        "windows-powershell",
        "Windows PowerShell",
        "Built-in Windows shell; supported, but older than PowerShell 7",
        &["powershell.exe"],
        &[
            "-NoLogo",
            "-NoProfile",
            "-Command",
            "$PSVersionTable.PSVersion.ToString()",
        ],
        None,
        false,
    );
    push_shell(
        &mut shells,
        "cmd",
        "Command Prompt",
        "Built-in Windows command shell",
        &["cmd.exe"],
        &[],
        None,
        false,
    );
    push_shell(
        &mut shells,
        "git-bash",
        "Git Bash",
        "Bash from Git for Windows",
        &[
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
            "bash.exe",
            "bash",
        ],
        &["--version"],
        shell_install_plan("git-bash").map(|p| p.command_line()),
        false,
    );
    push_shell(
        &mut shells,
        "msys2",
        "MSYS2 Bash",
        "Pacman-based Unix shell environment for Windows",
        &[
            r"C:\msys64\usr\bin\bash.exe",
            r"C:\tools\msys64\usr\bin\bash.exe",
            "msys2.exe",
        ],
        &["--version"],
        shell_install_plan("msys2").map(|p| p.command_line()),
        false,
    );
    push_shell(
        &mut shells,
        "cygwin",
        "Cygwin Bash",
        "POSIX-compatible shell environment for Windows",
        &[r"C:\cygwin64\bin\bash.exe", r"C:\cygwin\bin\bash.exe"],
        &["--version"],
        shell_install_plan("cygwin").map(|p| p.command_line()),
        false,
    );
    push_shell(
        &mut shells,
        "clink",
        "Clink",
        "Enhanced cmd.exe editing and completion",
        &["clink.exe"],
        &["--version"],
        shell_install_plan("clink").map(|p| p.command_line()),
        false,
    );
    push_shell(
        &mut shells,
        "cmder",
        "Cmder",
        "Portable console environment built around cmd and Clink",
        &[
            r"C:\tools\cmder\Cmder.exe",
            r"C:\cmder\Cmder.exe",
            "Cmder.exe",
        ],
        &[],
        None,
        false,
    );
    push_shell(
        &mut shells,
        "wsl",
        "WSL",
        "Linux shell through Windows Subsystem for Linux",
        &["wsl.exe", "wsl"],
        &["--version"],
        shell_install_plan("wsl").map(|p| p.command_line()),
        false,
    );
    push_shell(
        &mut shells,
        "nushell",
        "Nushell",
        "Structured data shell",
        &["nu.exe", "nu"],
        &["--version"],
        shell_install_plan("nushell").map(|p| p.command_line()),
        false,
    );
    if std::env::consts::OS != "windows" {
        push_shell(
            &mut shells,
            "bash",
            "Bash",
            "Bourne Again Shell",
            &["bash"],
            &["--version"],
            shell_install_plan("bash").map(|p| p.command_line()),
            false,
        );
        push_shell(
            &mut shells,
            "zsh",
            "Zsh",
            "Z shell",
            &["zsh"],
            &["--version"],
            shell_install_plan("zsh").map(|p| p.command_line()),
            false,
        );
        push_shell(
            &mut shells,
            "fish",
            "fish",
            "Friendly interactive shell",
            &["fish"],
            &["--version"],
            shell_install_plan("fish").map(|p| p.command_line()),
            false,
        );
        push_shell(
            &mut shells,
            "elvish",
            "Elvish",
            "Expressive interactive shell",
            &["elvish"],
            &["--version"],
            shell_install_plan("elvish").map(|p| p.command_line()),
            false,
        );
        push_shell(
            &mut shells,
            "xonsh",
            "Xonsh",
            "Python-powered shell",
            &["xonsh"],
            &["--version"],
            shell_install_plan("xonsh").map(|p| p.command_line()),
            false,
        );
        push_shell(
            &mut shells,
            "oil",
            "Oil Shell",
            "Shell language compatible with bash scripts",
            &["osh", "oil"],
            &["--version"],
            shell_install_plan("oil").map(|p| p.command_line()),
            false,
        );
    }
    shells
}

fn push_shell(
    shells: &mut Vec<Value>,
    id: &str,
    name: &str,
    description: &str,
    candidates: &[&str],
    version_args: &[&str],
    install_command: Option<String>,
    recommended: bool,
) {
    let path = candidates
        .iter()
        .find_map(|candidate| resolve_program(candidate));
    let version = (!version_args.is_empty()).then_some(()).and(
        path.as_ref()
            .and_then(|path| command_output(path, version_args))
            .and_then(|output| {
                output
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .map(str::trim)
                    .map(str::to_string)
            }),
    );
    let installed = path.is_some() && (version_args.is_empty() || version.is_some());
    shells.push(json!({
        "id": id,
        "name": name,
        "description": description,
        "installed": installed,
        "path": path.unwrap_or_default(),
        "version": version,
        "install_command": install_command,
        "recommended": recommended,
    }));
}

fn shell_install_plan(id: &str) -> Option<ShellInstallPlan> {
    match (std::env::consts::OS, id) {
        ("windows", "pwsh") => Some(ShellInstallPlan {
            program: "winget.exe".to_string(),
            args: vec![
                "install".to_string(),
                "--id".to_string(),
                "Microsoft.PowerShell".to_string(),
                "--source".to_string(),
                "winget".to_string(),
            ],
        }),
        ("windows", "git-bash") => Some(ShellInstallPlan {
            program: "winget.exe".to_string(),
            args: vec![
                "install".to_string(),
                "--id".to_string(),
                "Git.Git".to_string(),
                "--source".to_string(),
                "winget".to_string(),
            ],
        }),
        ("windows", "msys2") => Some(ShellInstallPlan {
            program: "winget.exe".to_string(),
            args: vec![
                "install".to_string(),
                "--id".to_string(),
                "MSYS2.MSYS2".to_string(),
                "--source".to_string(),
                "winget".to_string(),
            ],
        }),
        ("windows", "cygwin") => Some(ShellInstallPlan {
            program: "winget.exe".to_string(),
            args: vec![
                "install".to_string(),
                "--id".to_string(),
                "Cygwin.Cygwin".to_string(),
                "--source".to_string(),
                "winget".to_string(),
            ],
        }),
        ("windows", "clink") => Some(ShellInstallPlan {
            program: "winget.exe".to_string(),
            args: vec![
                "install".to_string(),
                "--id".to_string(),
                "chrisant996.Clink".to_string(),
                "--source".to_string(),
                "winget".to_string(),
            ],
        }),
        ("windows", "wsl") => Some(ShellInstallPlan {
            program: "wsl.exe".to_string(),
            args: vec!["--install".to_string()],
        }),
        ("windows", "nushell") => Some(ShellInstallPlan {
            program: "winget.exe".to_string(),
            args: vec![
                "install".to_string(),
                "--id".to_string(),
                "Nushell.Nushell".to_string(),
                "--source".to_string(),
                "winget".to_string(),
            ],
        }),
        ("macos", "pwsh") => Some(ShellInstallPlan {
            program: "brew".to_string(),
            args: vec!["install".to_string(), "powershell".to_string()],
        }),
        ("macos", "nushell") => Some(ShellInstallPlan {
            program: "brew".to_string(),
            args: vec!["install".to_string(), "nushell".to_string()],
        }),
        ("macos", "bash") => Some(ShellInstallPlan {
            program: "brew".to_string(),
            args: vec!["install".to_string(), "bash".to_string()],
        }),
        ("macos", "zsh") => Some(ShellInstallPlan {
            program: "brew".to_string(),
            args: vec!["install".to_string(), "zsh".to_string()],
        }),
        ("macos", "fish") => Some(ShellInstallPlan {
            program: "brew".to_string(),
            args: vec!["install".to_string(), "fish".to_string()],
        }),
        ("macos", "elvish") => Some(ShellInstallPlan {
            program: "brew".to_string(),
            args: vec!["install".to_string(), "elvish".to_string()],
        }),
        ("macos", "xonsh") => Some(ShellInstallPlan {
            program: "brew".to_string(),
            args: vec!["install".to_string(), "xonsh".to_string()],
        }),
        ("linux", "fish") => Some(ShellInstallPlan {
            program: "sh".to_string(),
            args: vec!["-lc".to_string(), "command -v apt >/dev/null && sudo apt install -y fish || command -v dnf >/dev/null && sudo dnf install -y fish || command -v pacman >/dev/null && sudo pacman -S --noconfirm fish".to_string()],
        }),
        _ => None,
    }
}

fn shell_default_argv(id: &str) -> Option<Vec<String>> {
    let (candidates, args): (&[&str], &[&str]) = match (std::env::consts::OS, id) {
        ("windows", "pwsh") => (
            &[
                r"C:\Program Files\PowerShell\7\pwsh.exe",
                "pwsh.exe",
                "pwsh",
            ],
            &["-NoLogo", "-NoProfile"],
        ),
        ("windows", "windows-powershell") => (&["powershell.exe"], &["-NoLogo", "-NoProfile"]),
        ("windows", "cmd") => (&["cmd.exe"], &[]),
        ("windows", "git-bash") => (
            &[
                r"C:\Program Files\Git\bin\bash.exe",
                r"C:\Program Files (x86)\Git\bin\bash.exe",
                "bash.exe",
                "bash",
            ],
            &[],
        ),
        ("windows", "msys2") => (
            &[
                r"C:\msys64\usr\bin\bash.exe",
                r"C:\tools\msys64\usr\bin\bash.exe",
                "msys2.exe",
            ],
            &[],
        ),
        ("windows", "cygwin") => (
            &[r"C:\cygwin64\bin\bash.exe", r"C:\cygwin\bin\bash.exe"],
            &[],
        ),
        ("windows", "wsl") => (&["wsl.exe", "wsl"], &[]),
        ("windows", "nushell") => (&["nu.exe", "nu"], &[]),
        (_, "bash") => (&["bash"], &[]),
        (_, "zsh") => (&["zsh"], &[]),
        (_, "fish") => (&["fish"], &[]),
        (_, "elvish") => (&["elvish"], &[]),
        (_, "xonsh") => (&["xonsh"], &[]),
        (_, "oil") => (&["osh", "oil"], &[]),
        (_, "pwsh") => (&["pwsh"], &["-NoLogo", "-NoProfile"]),
        (_, "nushell") => (&["nu"], &[]),
        _ => return None,
    };
    let program = candidates
        .iter()
        .find_map(|candidate| resolve_program(candidate))?;
    let mut argv = vec![program];
    argv.extend(args.iter().map(|arg| arg.to_string()));
    Some(argv)
}

fn write_default_shell(argv: &[String]) -> std::io::Result<()> {
    let path = unterm_services::settings::default_path()
        .unwrap_or_else(|| std::path::PathBuf::from(".unterm").join("unterm.conf"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let source = std::fs::read_to_string(&path).unwrap_or_default();
    let updated = set_top_level_shell(&source, argv);
    std::fs::write(path, updated)
}

fn set_top_level_shell(source: &str, argv: &[String]) -> String {
    let shell_line = format!("shell = {}\n", config_string_list(argv));
    let mut output = String::new();
    let mut replaced = false;
    let mut inserted = false;
    for line in source.lines() {
        let trimmed = line.trim_start();
        let starts_section = trimmed.starts_with('[');
        if starts_section && !inserted && !replaced {
            output.push_str(&shell_line);
            if !output.ends_with("\n\n") {
                output.push('\n');
            }
            inserted = true;
        }
        if !starts_section && !replaced && top_level_shell_line(line) {
            output.push_str(&shell_line);
            replaced = true;
            inserted = true;
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    if !inserted && !replaced {
        output.push_str(&shell_line);
    }
    output
}

fn top_level_shell_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') || trimmed.starts_with('[') {
        return false;
    }
    trimmed
        .strip_prefix("shell")
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

fn config_string_list(values: &[String]) -> String {
    let quoted = values
        .iter()
        .map(|value| format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{quoted}]")
}

fn short_program_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string()
}

fn resolve_program(program: &str) -> Option<String> {
    let path = std::path::Path::new(program);
    if path.is_absolute() && path.is_file() {
        return Some(program.to_string());
    }
    if program.contains(std::path::MAIN_SEPARATOR) {
        return None;
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate.display().to_string());
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat"] {
                let candidate = dir.join(format!("{program}.{ext}"));
                if candidate.is_file() {
                    return Some(candidate.display().to_string());
                }
            }
        }
    }
    None
}

fn program_exists(program: &str) -> bool {
    resolve_program(program).is_some()
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut text = decode_command_output(&output.stdout);
    if text.trim().is_empty() {
        text = decode_command_output(&output.stderr);
    }
    Some(text)
}

fn decode_command_output(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[1] == 0 {
        let words: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16_lossy(&words);
    }
    String::from_utf8_lossy(bytes).replace('\0', "")
}

fn shell_arg(arg: &str) -> String {
    if arg
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '/' | '\\' | ':'))
    {
        arg.to_string()
    } else {
        format!("\"{}\"", arg.replace('"', "\\\""))
    }
}

// --- Scrollback override ---------------------------------------------------
//
// Stored at `~/.unterm/scrollback.json` so config::default_scrollback_lines()
// can read it without going through the lua config layer. Changes only
// affect newly-created panes (the existing VecDeque<Line> per pane has its
// capacity locked at construction), so the UI surfaces a "restart to apply"
// hint after Save.

fn scrollback_path() -> std::path::PathBuf {
    unterm_protocol::state_path("scrollback.json")
        .unwrap_or_else(|| std::path::PathBuf::from(".unterm").join("scrollback.json"))
}

fn current_scrollback_lines() -> u64 {
    if let Ok(content) = std::fs::read_to_string(scrollback_path()) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(n) = value.get("lines").and_then(|v| v.as_u64()) {
                if n > 0 {
                    return n;
                }
            }
        }
    }
    10_000
}

fn api_scrollback_get() -> Response {
    Response::ok_json(json!({
        "lines": current_scrollback_lines(),
        "default": 10_000,
        "max": 999_999_999u64,
        "min": 100u64,
    }))
}

fn api_scrollback_set(body: &[u8]) -> Response {
    let body = parse_json_body(body);
    let lines = match body.get("lines").and_then(|v| v.as_u64()) {
        Some(n) if n >= 100 && n <= 999_999_999 => n,
        Some(_) => return Response::err(400, "Bad Request", "lines must be in [100, 999999999]"),
        None => return Response::err(400, "Bad Request", "missing lines (u64)"),
    };
    let path = scrollback_path();
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Response::err(500, "Internal Error", &e.to_string());
        }
    }
    let payload = serde_json::to_string_pretty(&json!({"lines": lines})).unwrap();
    if let Err(e) = std::fs::write(&path, payload) {
        return Response::err(500, "Internal Error", &e.to_string());
    }
    Response::ok_json(json!({
        "applied": true,
        "lines": lines,
        // Existing panes keep their old buffer; new panes pick up the new
        // value. Tell the client so it can prompt the user.
        "requires_restart_for_existing_panes": true,
    }))
}

// --- Compatibility (TERM_PROGRAM masquerade) -------------------------------
//
// Some third-party tools — Gemini CLI, certain IDE terminal detectors —
// whitelist a fixed set of TERM_PROGRAM values and reject anything else.
// We default to "Unterm" because that's our product identity, but expose
// a per-user override at ~/.unterm/compat.json. config::read_term_program_override
// reads it when spawning each new shell. Existing shells keep their old
// env until the user opens a new tab.

fn compat_path() -> std::path::PathBuf {
    unterm_protocol::state_path("compat.json")
        .unwrap_or_else(|| std::path::PathBuf::from(".unterm").join("compat.json"))
}

fn current_term_program() -> String {
    if let Ok(content) = std::fs::read_to_string(compat_path()) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(s) = value.get("term_program").and_then(|v| v.as_str()) {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    "Unterm".to_string()
}

fn api_compat_get() -> Response {
    Response::ok_json(json!({
        "term_program": current_term_program(),
        "default": "Unterm",
        // Common values the UI offers as one-click presets. Custom is
        // implicitly available — any non-listed string is accepted.
        "presets": ["Unterm", "WezTerm", "Apple_Terminal", "iTerm.app", "xterm"],
    }))
}

// --- Update check ---------------------------------------------------------
//
// The actual GitHub poll lives in `crate::update_check` and runs in a
// background thread that writes ~/.unterm/update_check.json every 6 h.
// These endpoints are thin: GET reads that file, POST kicks the
// poller's check_once() so the user can hit "Check now" in the UI
// without waiting on the next 6h tick.

fn api_updates_get() -> Response {
    Response::ok_json(crate::update_check::read_state())
}

fn api_updates_check() -> Response {
    // Run synchronously — the request takes one HTTP round-trip to
    // GitHub plus a small file write, well under a second on any
    // network where we can reach the user at all.
    if let Err(e) = crate::update_check::check_once() {
        return Response::err(502, "Bad Gateway", &format!("github check failed: {e:#}"));
    }
    Response::ok_json(crate::update_check::read_state())
}

fn api_compat_set(body: &[u8]) -> Response {
    let body = parse_json_body(body);
    let term_program = match body.get("term_program").and_then(|v| v.as_str()) {
        Some(s) => {
            let s = s.trim();
            if s.is_empty() {
                return Response::err(
                    400,
                    "Bad Request",
                    "term_program may not be empty — pass \"Unterm\" to reset to default",
                );
            }
            if s.len() > 64 {
                return Response::err(400, "Bad Request", "term_program too long (max 64 chars)");
            }
            // Reject anything that contains nul / newline / control chars,
            // since this string ends up in an env var passed to child
            // processes — corrupt env breaks downstream shells.
            if s.chars().any(|c| c.is_control()) {
                return Response::err(
                    400,
                    "Bad Request",
                    "term_program contains control characters",
                );
            }
            s.to_string()
        }
        None => return Response::err(400, "Bad Request", "missing term_program (string)"),
    };
    let path = compat_path();
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Response::err(500, "Internal Error", &e.to_string());
        }
    }
    let payload = serde_json::to_string_pretty(&json!({"term_program": term_program})).unwrap();
    if let Err(e) = std::fs::write(&path, payload) {
        return Response::err(500, "Internal Error", &e.to_string());
    }
    Response::ok_json(json!({
        "applied": true,
        "term_program": term_program,
        // Existing shells keep their old TERM_PROGRAM; only newly spawned
        // shells pick up the change. Tell the client to prompt for new tab.
        "requires_new_shell": true,
    }))
}

fn api_proxy(handler: &McpHandler, body: &[u8]) -> Response {
    let body = parse_json_body(body);
    let enabled = body
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    let result = if !enabled {
        handler.handle(&ctx, "proxy.disable", &json!({}))
    } else {
        let mut params = serde_json::Map::new();
        params.insert("enabled".into(), Value::Bool(true));
        let mode = if body.get("http_proxy").is_some() || body.get("socks_proxy").is_some() {
            "manual"
        } else {
            "auto"
        };
        params.insert("mode".into(), Value::String(mode.into()));
        for k in ["http_proxy", "socks_proxy", "no_proxy"] {
            if let Some(v) = body.get(k) {
                params.insert(k.into(), v.clone());
            }
        }
        handler.handle(&ctx, "proxy.configure", &Value::Object(params))
    };
    match result {
        Ok(v) => Response::ok_json(v),
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

/// POST /api/proxy/rotation — get/set endpoint auto-rotation. Body forwards
/// `enabled` / `pool` / `interval_secs` straight to the `proxy.rotation` MCP
/// method (all optional; an empty body is a read).
fn api_proxy_rotation(handler: &McpHandler, body: &[u8]) -> Response {
    let body = parse_json_body(body);
    let mut params = serde_json::Map::new();
    for k in ["enabled", "pool", "interval_secs"] {
        if let Some(v) = body.get(k) {
            params.insert(k.into(), v.clone());
        }
    }
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    match handler.handle(&ctx, "proxy.rotation", &Value::Object(params)) {
        Ok(v) => Response::ok_json(v),
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

/// POST /api/proxy/clash/controller — set/clear the manual Clash controller
/// (host:port + secret) for when auto-discovery can't find it.
fn api_proxy_clash_controller(handler: &McpHandler, body: &[u8]) -> Response {
    let body = parse_json_body(body);
    let mut params = serde_json::Map::new();
    for k in ["controller", "secret"] {
        if let Some(v) = body.get(k) {
            params.insert(k.into(), v.clone());
        }
    }
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    match handler.handle(&ctx, "proxy.clash_set_controller", &Value::Object(params)) {
        Ok(v) => Response::ok_json(v),
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

/// POST /api/proxy/nodes — replace the rotation node list. Body forwards
/// `{nodes: [{name, url}, …]}` to the `proxy.set_nodes` MCP method so the pool
/// can be built from the GUI instead of hand-editing proxy.json.
fn api_proxy_set_nodes(handler: &McpHandler, body: &[u8]) -> Response {
    let body = parse_json_body(body);
    let mut params = serde_json::Map::new();
    if let Some(nodes) = body.get("nodes") {
        params.insert("nodes".into(), nodes.clone());
    }
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    match handler.handle(&ctx, "proxy.set_nodes", &Value::Object(params)) {
        Ok(v) => Response::ok_json(v),
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

/// GET /api/proxy/clash — controller status + switchable groups and their
/// nodes (alive + delay), read from the Clash/mihomo controller.
fn api_proxy_clash_status(handler: &McpHandler) -> Response {
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    match handler.handle(&ctx, "proxy.clash_status", &json!({})) {
        Ok(v) => Response::ok_json(v),
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

/// GET /api/approvals — what the gateway is waiting on a person to answer.
fn api_approvals(handler: &McpHandler) -> Response {
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    match handler.handle(&ctx, "approval.list", &json!({})) {
        Ok(v) => Response::ok_json(v),
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

/// POST /api/approvals/decide — the answer.
///
/// This route is the reason `approval.decide` exists at all: the settings
/// page is in-process, so it clears the check that keeps agents from
/// answering their own questions.
fn api_approval_decide(handler: &McpHandler, body: &[u8]) -> Response {
    let body = parse_json_body(body);
    let mut params = serde_json::Map::new();
    for key in ["approval", "allowed", "remember"] {
        if let Some(value) = body.get(key) {
            params.insert(key.to_string(), value.clone());
        }
    }
    params.insert("decided_by".into(), json!("the user, in settings"));
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    match handler.handle(&ctx, "approval.decide", &Value::Object(params)) {
        Ok(v) => Response::ok_json(v),
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

/// GET /api/providers — what can be reached, and how each one stands.
fn api_providers(handler: &McpHandler, query: &str) -> Response {
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    // Rediscovery is explicit: opening the page must not go looking for the
    // user's browser every time it refreshes.
    let rediscover = query.contains("rediscover=1");
    match handler.handle(&ctx, "provider.list", &json!({"rediscover": rediscover})) {
        Ok(v) => Response::ok_json(v),
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

/// GET /api/providers/leases — the keys currently handed out.
fn api_provider_leases(handler: &McpHandler) -> Response {
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    match handler.handle(&ctx, "provider.leases", &json!({"live_only": true})) {
        Ok(v) => Response::ok_json(v),
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

/// The provider actions, which differ only in the method they call.
///
/// One function rather than six: they take the same arguments, and six
/// copies is six chances for one of them to forget to report an error.
fn api_provider_act(handler: &McpHandler, method: &str, body: &[u8]) -> Response {
    let body = parse_json_body(body);
    let mut params = serde_json::Map::new();
    for key in ["provider", "lease", "method"] {
        if let Some(value) = body.get(key) {
            params.insert(key.to_string(), value.clone());
        }
    }
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    match handler.handle(&ctx, method, &Value::Object(params)) {
        Ok(v) => Response::ok_json(v),
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

/// POST /api/proxy/clash/select — switch a Selector group to a node.
fn api_proxy_clash_select(handler: &McpHandler, body: &[u8]) -> Response {
    let body = parse_json_body(body);
    let mut params = serde_json::Map::new();
    for k in ["group", "name"] {
        if let Some(v) = body.get(k) {
            params.insert(k.into(), v.clone());
        }
    }
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    match handler.handle(&ctx, "proxy.clash_select", &Value::Object(params)) {
        Ok(v) => Response::ok_json(v),
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

fn api_theme(body: &[u8]) -> Response {
    let body = parse_json_body(body);
    let name = match body.get("name").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return Response::err(400, "Bad Request", "missing name"),
    };
    let preset = match find_theme(&name) {
        Some(p) => p,
        None => return Response::err(400, "Bad Request", "unknown theme name"),
    };
    if let Err(e) = save_theme_to_disk(&preset) {
        return Response::err(500, "Internal Error", &e.to_string());
    }
    // The settings server and native windows share this process. Publishing a
    // generation-stamped request lets every open window observe and apply the
    // change on its event-loop tick without moving window/GPU state across
    // threads.
    let generation = unterm_services::theme_state::request(preset.id);
    Response::ok_json(json!({
        "saved": true,
        "applied_to_open_windows": true,
        "generation": generation,
        "theme": preset.id,
        "color_scheme": preset.scheme,
    }))
}

fn api_recording(handler: &McpHandler, body: &[u8], start: bool) -> Response {
    let body = parse_json_body(body);
    let pane_id = match body.get("pane_id").and_then(|v| v.as_u64()) {
        Some(p) => p,
        None => return Response::err(400, "Bad Request", "missing pane_id"),
    };
    let method = if start {
        "session.recording_start"
    } else {
        "session.recording_stop"
    };
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    match handler.handle(&ctx, method, &json!({"id": pane_id})) {
        Ok(v) => Response::ok_json(v),
        Err(e) => Response::err(500, "Internal Error", &e.to_string()),
    }
}

fn api_sessions(handler: &McpHandler, query: &str) -> Response {
    let mut params = serde_json::Map::new();
    if let Some(p) = parse_query(query, "project") {
        params.insert("project".into(), Value::String(p));
    }
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    match handler.handle(&ctx, "session.recording_list", &Value::Object(params)) {
        // The MCP method returns an array directly; wrap it for the SPA so
        // it can grow `total`/etc. fields later without breaking clients.
        Ok(v) => Response::ok_json(json!({"sessions": v})),
        Err(e) => Response::err(500, "Internal Error", &e.to_string()),
    }
}

fn api_session_markdown(handler: &McpHandler, path: &str) -> Response {
    // /api/sessions/<id>/markdown
    let stripped = match path.strip_prefix("/api/sessions/") {
        Some(s) => s,
        None => return Response::err(404, "Not Found", "no such session"),
    };
    let id = match stripped.strip_suffix("/markdown") {
        Some(s) => s,
        None => return Response::err(404, "Not Found", "no such session"),
    };
    let id = match urldecode(id) {
        Some(s) => s,
        None => return Response::err(400, "Bad Request", "invalid session id encoding"),
    };
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    match handler.handle(&ctx, "session.recording_read", &json!({"session_id": id})) {
        Ok(v) => {
            let md = v
                .get("markdown")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            Response::text(200, "OK", "text/plain; charset=utf-8", md.into_bytes())
        }
        Err(e) => Response::err(404, "Not Found", &e.to_string()),
    }
}

// --- Helpers ---------------------------------------------------------------

// --- Agent Cockpit: Review API ---

fn api_review_inbox() -> Response {
    let items: Vec<serde_json::Value> = unterm_services::cockpit::snapshot()
        .iter()
        .map(|s| {
            json!({
                "pane_id": s.pane_id,
                "agent": s.agent,
                "state": s.state.as_str(),
                "for_secs": s.since.elapsed().as_secs(),
                "task_hint": s.task_hint,
                "fleet_id": s.fleet_id,
            })
        })
        .collect();
    Response::ok_json(json!({ "items": items }))
}

fn api_review_diff(query: &str) -> Response {
    let result = if let (Some(fleet_id), Some(member)) =
        (parse_query(query, "fleet"), parse_query(query, "member"))
    {
        unterm_services::cockpit::fleet::get(&fleet_id)
            .ok_or_else(|| anyhow::anyhow!("no fleet {fleet_id:?}"))
            .and_then(|f| unterm_services::cockpit::fleet::resolve_member(&f, &member))
            .and_then(|m| unterm_services::cockpit::review::diff(&m.worktree, &m.checkpoint))
    } else if let (Some(repo), Some(from)) =
        (parse_query(query, "repo"), parse_query(query, "from"))
    {
        unterm_services::cockpit::review::diff(std::path::Path::new(&repo), &from)
    } else {
        Err(anyhow::anyhow!("need fleet+member or repo+from"))
    };
    match result {
        Ok(v) => Response::ok_json(v),
        Err(e) => Response::err(400, "Bad Request", &format!("{e:#}")),
    }
}

fn api_review_rollback(body: &[u8]) -> Response {
    let v: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return Response::err(400, "Bad Request", &format!("bad json: {e}")),
    };
    let (Some(repo), Some(sha)) = (
        v.get("repo").and_then(|x| x.as_str()),
        v.get("sha").and_then(|x| x.as_str()),
    ) else {
        return Response::err(400, "Bad Request", "need repo + sha");
    };
    match unterm_services::cockpit::review::rollback(std::path::Path::new(repo), sha) {
        Ok(()) => Response::ok_json(json!({ "ok": true })),
        Err(e) => Response::err(500, "Internal Server Error", &format!("{e:#}")),
    }
}

fn api_review_member_action(body: &[u8], action: &str) -> Response {
    let v: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return Response::err(400, "Bad Request", &format!("bad json: {e}")),
    };
    let (Some(fleet), Some(member)) = (
        v.get("fleet_id").and_then(|x| x.as_str()),
        v.get("member").and_then(|x| x.as_str()),
    ) else {
        return Response::err(400, "Bad Request", "need fleet_id + member");
    };
    let force = v.get("force").and_then(|x| x.as_bool()).unwrap_or(false);
    let result = match action {
        "merge" => unterm_services::cockpit::review::merge_member_with_policy(fleet, member, force),
        _ => unterm_services::cockpit::review::discard_member(fleet, member),
    };
    match result {
        Ok(v) => Response::ok_json(v),
        Err(e) => Response::err(500, "Internal Server Error", &format!("{e:#}")),
    }
}

fn api_review_verify(body: &[u8]) -> Response {
    let v: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return Response::err(400, "Bad Request", &format!("bad json: {e}")),
    };
    let (Some(fleet), Some(member)) = (
        v.get("fleet_id").and_then(|x| x.as_str()),
        v.get("member").and_then(|x| x.as_str()),
    ) else {
        return Response::err(400, "Bad Request", "need fleet_id + member");
    };
    let command = v.get("command").and_then(|x| x.as_str());
    let timeout = v.get("timeout_secs").and_then(|x| x.as_u64());
    match unterm_services::cockpit::verification::verify_member(fleet, member, command, timeout) {
        Ok(record) => Response::ok_json(serde_json::to_value(record).unwrap_or_default()),
        Err(e) => Response::err(400, "Bad Request", &format!("{e:#}")),
    }
}

fn api_review_retry(body: &[u8]) -> Response {
    // A verification retry is a fresh immutable record; the previous failure
    // remains available in verifications.json for audit/history.
    api_review_verify(body)
}

fn api_fleet_retry(body: &[u8]) -> Response {
    let v: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return Response::err(400, "Bad Request", &format!("bad json: {e}")),
    };
    let (Some(fleet), Some(member)) = (
        v.get("fleet_id").and_then(|x| x.as_str()),
        v.get("member").and_then(|x| x.as_str()),
    ) else {
        return Response::err(400, "Bad Request", "need fleet_id + member");
    };
    match unterm_services::cockpit::fleet::retry_member(fleet, member) {
        Ok(member) => Response::ok_json(serde_json::to_value(member).unwrap_or_default()),
        Err(e) => Response::err(400, "Bad Request", &format!("{e:#}")),
    }
}

fn api_review_clean(body: &[u8]) -> Response {
    let v: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return Response::err(400, "Bad Request", &format!("bad json: {e}")),
    };
    let Some(id) = v.get("id").and_then(|x| x.as_str()) else {
        return Response::err(400, "Bad Request", "need id");
    };
    let force = v.get("force").and_then(|x| x.as_bool()).unwrap_or(false);
    match unterm_services::cockpit::fleet::clean(id, force) {
        Ok(()) => Response::ok_json(json!({ "ok": true })),
        Err(e) => Response::err(500, "Internal Server Error", &format!("{e:#}")),
    }
}

fn parse_query(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut it = pair.splitn(2, '=');
        let k = it.next().unwrap_or("");
        let v = it.next().unwrap_or("");
        if k == key {
            return urldecode(v);
        }
    }
    None
}

fn urldecode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16)?;
            let lo = (bytes[i + 2] as char).to_digit(16)?;
            out.push(((hi << 4) | lo) as u8);
            i += 3;
        } else if b == b'+' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

struct ThemePreset {
    id: &'static str,
    name: &'static str,
    scheme: &'static str,
}

fn theme_presets() -> &'static [ThemePreset] {
    // Must stay in sync with the CLI's PRESETS (wezterm/src/unterm_cli/theme.rs)
    // and the SPA's themes array (assets/settings/app.js). All three list the
    // same presets; this is the one the /api/theme write path resolves
    // against, so a missing entry here makes "apply" silently 400 even though
    // the SPA shows the swatch.
    &[
        ThemePreset {
            id: "standard",
            name: "Standard",
            scheme: "Unterm Dark",
        },
        ThemePreset {
            id: "midnight",
            name: "Midnight",
            scheme: "Unterm Midnight",
        },
        ThemePreset {
            id: "daylight",
            name: "Daylight",
            scheme: "Unterm Daylight",
        },
        ThemePreset {
            id: "agent-inbox",
            name: "Agent Inbox",
            scheme: "Agent Inbox",
        },
        ThemePreset {
            id: "classic",
            name: "Classic",
            scheme: "Classic Dark",
        },
        ThemePreset {
            id: "notion-dark",
            name: "Notion Dark",
            scheme: "Notion Dark",
        },
        ThemePreset {
            id: "notion-light",
            name: "Notion Light",
            scheme: "Notion Light",
        },
    ]
}

fn find_theme(id: &str) -> Option<&'static ThemePreset> {
    theme_presets().iter().find(|p| p.id == id)
}

fn theme_config_path() -> std::path::PathBuf {
    unterm_protocol::state_path("theme.json")
        .unwrap_or_else(|| std::path::PathBuf::from(".unterm").join("theme.json"))
}

fn save_theme_to_disk(preset: &ThemePreset) -> Result<()> {
    let path = theme_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let value = json!({
        "theme": preset.id,
        "name": preset.name,
        "color_scheme": preset.scheme,
    });
    std::fs::write(path, serde_json::to_string_pretty(&value)?)?;
    Ok(())
}

fn current_theme_id() -> String {
    let path = theme_config_path();
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str::<Value>(&content).ok())
        .and_then(|v| v.get("theme").and_then(|t| t.as_str()).map(str::to_string))
        .unwrap_or_else(|| "standard".to_string())
}

/// The project the user is most likely looking at.
///
/// The lowest-numbered live pane's working directory: a best effort, and the
/// same one the tab bar makes. Asks the engine rather than a mux, so it works
/// in a front end that has no mux and in a process that has no window.
fn current_project_info() -> Value {
    let engine = unterm_engine::next_core();
    let Ok(sessions) = unterm_engine::SessionEngine::list_sessions(&engine) else {
        return json!({"path": null, "slug": null});
    };
    let Some(first) = sessions.into_iter().min_by_key(|session| session.id) else {
        return json!({"path": null, "slug": null});
    };
    let Some(cwd) = unterm_engine::SessionEngine::shell(&engine, first.id)
        .ok()
        .and_then(|shell| shell.cwd)
    else {
        return json!({"path": null, "slug": null});
    };
    let path = std::path::PathBuf::from(&cwd);
    json!({
        "path": cwd,
        "slug": path.file_name().and_then(|name| name.to_str()),
    })
}

/// Which pane is being recorded, if any.
fn current_recording_info() -> Value {
    let engine = unterm_engine::next_core();
    let Ok(sessions) = unterm_engine::SessionEngine::list_sessions(&engine) else {
        return json!({"active": false});
    };
    for session in sessions {
        match unterm_engine::RecordingEngine::recording_status(&engine, session.id) {
            Ok(status) if status.enabled => {
                return json!({
                    "active": true,
                    "pane_id": session.id,
                    "status": status,
                });
            }
            _ => {}
        }
    }
    json!({"active": false})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, path: &str, token: Option<&str>, body: Vec<u8>) -> Request {
        let headers = token
            .map(|token| vec![("Authorization".into(), format!("Bearer {token}"))])
            .unwrap_or_default();
        Request {
            method: method.into(),
            path: path.into(),
            query: String::new(),
            headers,
            body,
        }
    }

    fn response_json(response: Response) -> Value {
        serde_json::from_slice(&response.body).expect("response body should be json")
    }

    #[test]
    fn api_routes_require_the_instance_bearer_token() {
        let handler = McpHandler::new();
        let response = route(
            &request("GET", "/api/health", None, Vec::new()),
            "secret",
            &handler,
        );
        assert_eq!(response.status, 401);

        let response = route(
            &request("GET", "/api/health", Some("wrong"), Vec::new()),
            "secret",
            &handler,
        );
        assert_eq!(response.status, 401);

        let response = route(
            &request("GET", "/api/health", Some("secret"), Vec::new()),
            "secret",
            &handler,
        );
        assert_eq!(response.status, 200);
        assert_eq!(response_json(response)["ok"], true);
    }

    #[test]
    fn bootstrap_remains_public_but_api_reference_is_token_gated() {
        let handler = McpHandler::new();
        let bootstrap = route(
            &request("GET", "/bootstrap.json", None, Vec::new()),
            "secret",
            &handler,
        );
        assert_eq!(bootstrap.status, 200);
        assert!(response_json(bootstrap).get("auth_token").is_some());

        let reference = route(
            &request("GET", "/api/reference", None, Vec::new()),
            "secret",
            &handler,
        );
        assert_eq!(reference.status, 401);
    }

    #[test]
    fn api_reference_uses_the_same_meta_surface_as_mcp() {
        let handler = McpHandler::new();
        let response = route(
            &request("GET", "/api/reference", Some("secret"), Vec::new()),
            "secret",
            &handler,
        );
        assert_eq!(response.status, 200);
        let from_settings = response_json(response);
        let from_mcp = unterm_mcp::meta::surface(&Value::Null).expect("meta.surface");

        assert_eq!(from_settings["mcp_methods"], from_mcp["mcp_methods"]);
        assert_eq!(from_settings["cli_commands"], from_mcp["cli_commands"]);
    }

    #[test]
    fn shell_inventory_keeps_recommended_powershell_and_install_plan() {
        let response = api_shells();
        assert_eq!(response.status, 200);
        let value = response_json(response);
        let shells = value["shells"].as_array().expect("shells array");
        let pwsh = shells
            .iter()
            .find(|shell| shell["id"] == "pwsh")
            .expect("PowerShell 7 entry");

        assert_eq!(pwsh["recommended"], true);
        assert_eq!(pwsh["name"], "PowerShell 7");
        // Case-insensitive: winget says Microsoft.PowerShell, brew says
        // powershell. Linux has no one-line official install, so the plan
        // is legitimately absent there rather than invented.
        if cfg!(any(windows, target_os = "macos")) {
            assert!(
                pwsh["install_command"]
                    .as_str()
                    .is_some_and(|command| command.to_ascii_lowercase().contains("powershell")),
                "pwsh entry should expose an install command: {pwsh:?}"
            );
        } else {
            assert!(
                pwsh["install_command"].is_null(),
                "no invented install plan on platforms without one: {pwsh:?}"
            );
        }
    }

    #[test]
    fn web_settings_theme_picker_lists_every_backend_preset() {
        let app_js = crate::assets::APP_JS;
        for preset in theme_presets() {
            let needle = format!("id: '{}'", preset.id);
            assert!(
                app_js.contains(&needle),
                "settings app.js should list theme preset {}",
                preset.id
            );
        }
    }

    #[test]
    fn shell_install_dry_run_returns_a_plan_without_launching() {
        #[cfg(any(windows, target_os = "macos"))]
        let id = "pwsh";
        #[cfg(all(unix, not(target_os = "macos")))]
        let id = "fish";

        let body = json!({ "id": id, "dry_run": true }).to_string();
        let response = api_shell_install(body.as_bytes());
        assert_eq!(response.status, 200);

        let value = response_json(response);
        assert_eq!(value["ok"], true);
        assert_eq!(value["dry_run"], true);
        assert_eq!(value["id"], id);
        assert_eq!(value["requires_new_shell"], true);
        assert!(!value["command"].as_str().unwrap_or_default().is_empty());
        assert!(value["installer_available"].is_boolean());
    }

    #[test]
    fn shell_install_rejects_unknown_shells() {
        let body = br#"{"id":"not-a-shell","dry_run":true}"#;
        let response = api_shell_install(body);
        assert_eq!(response.status, 400);
    }

    #[test]
    fn default_shell_config_is_written_before_sections() {
        let updated = set_top_level_shell(
            "font_size = 12\n\n[window]\ninitial_cols = 120\n",
            &[
                "powershell.exe".to_string(),
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
            ],
        );

        assert!(updated.starts_with("font_size = 12\n\nshell = ["));
        assert!(updated.contains("\"powershell.exe\""));
        assert!(updated.contains("\n[window]\n"));
        let parsed = unterm_engine::next_core::config::parse(&updated).expect("valid config");
        let settings = unterm_services::settings::Settings::from_config(&parsed);
        assert_eq!(
            settings.shell,
            Some(vec![
                "powershell.exe".to_string(),
                "-NoLogo".to_string(),
                "-NoProfile".to_string()
            ])
        );
    }

    #[test]
    fn default_shell_api_round_trips_through_state_config() {
        let _guard = StateDirGuard::new("shell-default");
        #[cfg(windows)]
        let id = "cmd";
        #[cfg(not(windows))]
        let id = "bash";

        let body = json!({ "id": id }).to_string();
        let response = api_shell_default_set(body.as_bytes());
        assert_eq!(response.status, 200);
        let value = response_json(response);
        assert_eq!(value["applied"], true);
        assert_eq!(value["requires_new_shell"], true);
        assert_eq!(value["default_shell"]["source"], "config");

        let path = unterm_services::settings::default_path().expect("state config path");
        let source = std::fs::read_to_string(path).expect("default shell config");
        assert!(source.contains("shell = ["));
        let (config, errors) = unterm_services::settings::load(None);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(unterm_services::settings::Settings::from_config(&config)
            .shell
            .is_some());
    }

    #[test]
    fn compat_api_round_trips_term_program_in_state_dir() {
        let _guard = StateDirGuard::new("compat-roundtrip");

        let response = api_compat_get();
        assert_eq!(response.status, 200);
        assert_eq!(response_json(response)["term_program"], "Unterm");

        let body = json!({ "term_program": "WezTerm" }).to_string();
        let response = api_compat_set(body.as_bytes());
        assert_eq!(response.status, 200);
        let value = response_json(response);
        assert_eq!(value["term_program"], "WezTerm");
        assert_eq!(value["requires_new_shell"], true);

        let response = api_compat_get();
        assert_eq!(response.status, 200);
        assert_eq!(response_json(response)["term_program"], "WezTerm");
    }

    #[test]
    fn compat_api_rejects_empty_and_control_values() {
        let _guard = StateDirGuard::new("compat-reject");

        let response = api_compat_set(br#"{"term_program":"   "}"#);
        assert_eq!(response.status, 400);

        let response = api_compat_set(b"{\"term_program\":\"bad\\nvalue\"}");
        assert_eq!(response.status, 400);

        let response = api_compat_get();
        assert_eq!(response.status, 200);
        assert_eq!(response_json(response)["term_program"], "Unterm");
    }

    struct StateDirGuard {
        original: Option<std::ffi::OsString>,
        path: std::path::PathBuf,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl StateDirGuard {
        fn new(name: &str) -> Self {
            static ENV_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
            let lock = ENV_LOCK
                .get_or_init(|| std::sync::Mutex::new(()))
                .lock()
                .expect("state dir env lock");
            let original = std::env::var_os("UNTERM_STATE_DIR");
            let path =
                std::env::temp_dir().join(format!("unterm-settings-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create isolated state dir");
            std::env::set_var("UNTERM_STATE_DIR", &path);
            Self {
                original,
                path,
                _lock: lock,
            }
        }
    }

    impl Drop for StateDirGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => std::env::set_var("UNTERM_STATE_DIR", value),
                None => std::env::remove_var("UNTERM_STATE_DIR"),
            }
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn the_providers_routes_are_reachable_and_authenticated() {
        // Every provider route must sit behind the same token as the rest;
        // one that did not would be a way to unbind somebody's browser from
        // any page they happened to visit.
        let handler = McpHandler::new();
        for (method, path) in [
            ("GET", "/api/providers"),
            ("GET", "/api/providers/leases"),
            ("POST", "/api/providers/bind"),
            ("POST", "/api/providers/pause"),
            ("POST", "/api/providers/resume"),
            ("POST", "/api/providers/unbind"),
            ("POST", "/api/providers/diagnose"),
            ("POST", "/api/providers/leases/revoke"),
        ] {
            let unauthenticated = route(
                &request(method, path, None, b"{}".to_vec()),
                "secret",
                &handler,
            );
            assert_eq!(unauthenticated.status, 401, "{method} {path} let a stranger in");

            let authenticated = route(
                &request(method, path, Some("secret"), b"{}".to_vec()),
                "secret",
                &handler,
            );
            // The action may well fail — there is no provider in a test
            // process — but it must be *routed*, not 404.
            assert_ne!(
                authenticated.status, 404,
                "{method} {path} is not routed"
            );
        }
    }

    #[test]
    fn listing_providers_does_not_go_looking_unless_asked() {
        // Opening the page must not wake the user's browser on every
        // refresh; rediscovery is a button.
        let handler = McpHandler::new();
        let response = route(
            &request("GET", "/api/providers", Some("secret"), Vec::new()),
            "secret",
            &handler,
        );
        assert_eq!(response.status, 200);
        let value = response_json(response);
        assert!(value.get("providers").is_some(), "{value}");
    }

    #[test]
    fn the_settings_page_ships_the_providers_panel() {
        // The assets are embedded at build time, so a panel that failed to
        // make it into the binary is invisible until somebody opens the tab.
        let html = crate::assets::INDEX_HTML;
        assert!(html.contains("active==='providers'"), "no providers panel");
        assert!(html.contains("Unbind"), "no way to take a binding back");

        let js = crate::assets::APP_JS;
        assert!(js.contains("loadProviders"), "the panel has nothing to load it");
        assert!(js.contains("/api/providers/leases/revoke"), "leases cannot be revoked");
    }

}

fn sessions_path_string() -> String {
    unterm_protocol::state_dir()
        .unwrap_or_else(|| std::path::PathBuf::from(".unterm"))
        .join("sessions")
        .display()
        .to_string()

}
