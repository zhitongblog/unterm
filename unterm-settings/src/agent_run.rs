//! Running a brain from the console.
//!
//! `agent_session.*` over MCP takes an argv, which is the right shape for a
//! trusted caller and the wrong one to put behind HTTP: a page that can pass
//! argv can run anything. So these routes take an **agent id** instead. The
//! command comes from that agent's manifest entry and the resolved binary
//! path that `installer::detect` hands back; the caller's own input only ever
//! reaches the prompt, which is written to the agent's stdin.
//!
//! What the caller can decide: which agent, what prompt, which workspace
//! directory it works in, whether it is hosted (asks before tools), and which
//! task/run/step the events belong to. What it cannot decide: what runs.

use super::server::Response;
use serde_json::{json, Value};
use unterm_agents::{fetch_manifests, installer};
use unterm_mcp::handler::McpHandler;

/// Flags that put an agent into headless JSON mode.
///
/// A manifest's `launch.args` is written for a human at a terminal. The
/// adapters in `unterm-brain` read a JSON event stream instead, so the flags
/// that produce one live here, next to the code that depends on them.
fn headless_args(agent_id: &str) -> Option<&'static [&'static str]> {
    match agent_id {
        // ClaudeAdapter and AgyAdapter read the `type: system|assistant|result`
        // lines that `--output-format stream-json` emits. `--verbose` is what
        // makes it stream rather than buffer to a single final message.
        "claude-code" | "agy" | "antigravity" => {
            Some(&["-p", "--output-format", "stream-json", "--verbose"])
        }
        // CodexAdapter's JSONL is the reference format.
        "codex-cli" => Some(&["exec", "--json"]),
        _ => None,
    }
}

/// Arguments for a *hosted* run: the agent is driven over its host protocol
/// (unterm-brain's `host` module adds the rest -- Claude's stream-json and
/// stdio permission prompts, `codex app-server`, `opencode acp`, `agy acp`),
/// stdin stays open for follow-up turns, and every tool that needs permission
/// comes to the console's inbox instead of being pre-approved or skipped.
fn hosted_args(agent_id: &str) -> Option<&'static [&'static str]> {
    match agent_id {
        "claude-code" => Some(&["-p"]),
        "codex-cli" => Some(&[]),
        "opencode" => Some(&[]),
        "agy" | "antigravity" => Some(&[]),
        _ => None,
    }
}

/// Whether the console can run this agent hosted (every tool call a
/// question in its inbox). Listed with the agents so the console offers
/// only the ones it can drive.
pub(crate) fn supports_hosted(agent_id: &str) -> bool {
    hosted_args(agent_id).is_some()
}

/// Whether this agent has a governed preset: its own shell off, Unterm's in
/// its place. Only Claude Code, for now -- see [`governed_args`].
pub(crate) fn supports_governed(agent_id: &str) -> bool {
    agent_id == "claude-code"
}

/// Where `unterm-cli` is: next to this binary (an installed Unterm), one
/// level up (a cargo `examples/` build), or `UNTERM_CLI`.
fn unterm_cli_path() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("UNTERM_CLI").map(std::path::PathBuf::from) {
        return path.is_file().then_some(path);
    }
    let exe = std::env::current_exe().ok()?;
    let name = if cfg!(windows) { "unterm-cli.exe" } else { "unterm-cli" };
    exe.ancestors()
        .skip(1)
        .take(2)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Unzoo Browser's MCP service, when it is installed (or `UNZOO_SERVICE`).
fn unzoo_service_path() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("UNZOO_SERVICE").map(std::path::PathBuf::from) {
        return path.is_file().then_some(path);
    }
    let candidates: &[&str] = if cfg!(windows) {
        &[r"C:\Program Files\Unzoo Browser\services\unzoo-service.exe"]
    } else {
        &[]
    };
    candidates.iter().map(std::path::PathBuf::from).find(|p| p.is_file())
}

/// GET /api/console/capabilities — what an agent can use on this machine:
/// Unterm's MCP for governed runs (needs `unterm-cli` beside this binary),
/// and Unzoo Browser's, when it is installed.
pub(crate) fn api_capabilities() -> Response {
    Response::ok_json(capabilities())
}

/// GET /api/console/browser — Unzoo Browser for the agents: the plan, the
/// profiles, and which one they browse in (see `browser`). Looking makes
/// their own profile while the plan has room for it.
pub(crate) fn api_browser() -> Response {
    let chosen = super::console_tasks::run_settings().browser_profile;
    Response::ok_json(super::browser::status(unzoo_service_path().is_some(), &chosen))
}

/// GET /api/console/clients — the AI clients on this machine, and whether
/// each can use this terminal and Unzoo Browser.
pub(crate) fn api_clients() -> Response {
    let cli = unterm_cli_path().map(|p| p.display().to_string());
    Response::ok_json(json!({
        "server": super::clients::server_name(),
        "terminal_ready": cli.is_some(),
        "browser_installed": unzoo_service_path().is_some(),
        "clients": super::clients::status(cli.as_deref()),
    }))
}

/// POST /api/console/clients/connect — `{id, connect}`: put this terminal in
/// that client's MCP servers, or take it out.
pub(crate) fn api_client_connect(body: &[u8]) -> Response {
    let body = parse_body(body);
    let (Some(id), Some(connect)) = (string_field(&body, "id"), body.get("connect").and_then(Value::as_bool)) else {
        return Response::err(400, "Bad Request", "id and connect are required");
    };
    let Some(cli) = unterm_cli_path() else {
        return Response::err(503, "Service Unavailable", "unterm-cli is not next to this service");
    };
    match super::clients::set(id, &cli.display().to_string(), connect) {
        Ok(value) => Response::ok_json(value),
        Err(e) => Response::err(400, "Bad Request", &format!("{e:#}")),
    }
}

/// POST /api/console/clients/browser — run Unzoo Browser's own setup, which
/// adds its server to every AI client it knows. Its server is its to manage;
/// this only starts the program that does.
pub(crate) fn api_client_browser() -> Response {
    let script = unzoo_service_path()
        .and_then(|service| service.parent().and_then(|dir| dir.parent()).map(|root| root.join("setup-mcp.ps1")))
        .filter(|script| script.is_file());
    let Some(script) = script else {
        return Response::err(404, "Not Found", "Unzoo Browser's setup (setup-mcp.ps1) was not found");
    };
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg("-Silent")
        .output();
    match output {
        Ok(out) if out.status.success() => Response::ok_json(json!({"ok": true})),
        Ok(out) => Response::err(
            500,
            "Internal Server Error",
            &format!("Unzoo Browser's setup failed: {}", String::from_utf8_lossy(&out.stderr).trim()),
        ),
        Err(e) => Response::err(500, "Internal Server Error", &format!("could not run Unzoo Browser's setup: {e}")),
    }
}

fn capabilities() -> Value {
    let browser = unzoo_service_path();
    json!({
        "terminal": {"governed_ready": unterm_cli_path().is_some()},
        "browser": {
            "installed": browser.is_some(),
            "path": browser.map(|p| p.display().to_string()),
        },
    })
}

const GOVERNED_PROMPT: &str = "This session runs inside Unzoo One. Your own shell tools \
(Bash, PowerShell) and WebFetch are switched off. Run every command through the unterm MCP \
server: open a terminal with its session.create tool when you need one, run commands with \
exec.run_wait and read their output from the result. For anything in a web browser use the \
unzoo MCP tools. Calls that could do harm are shown to the user for approval first; when one is \
refused, say what you needed it for instead of trying another way.";

/// The governed preset for Claude Code: hosted, its own shell and fetch
/// switched off, Unterm's MCP (and Unzoo Browser's, when installed) mounted
/// in their place, and a note saying so. Every command it runs then goes
/// through Unterm -- `AGENTS.md`'s first rule -- and every one is a question
/// in the inbox first.
fn governed_args(agent_id: &str, browser_profile: Option<&str>) -> Result<Vec<String>, String> {
    if !supports_governed(agent_id) {
        return Err(format!(
            "{agent_id} has no governed mode yet: only Claude Code can have its own shell switched off and Unterm mounted in its place"
        ));
    }
    let cli = unterm_cli_path()
        .ok_or("governed mode needs unterm-cli next to this Unterm (or UNTERM_CLI) to mount Unterm's MCP")?;
    let mut servers = serde_json::Map::new();
    servers.insert(
        "unterm".into(),
        json!({"type": "stdio", "command": cli.display().to_string(), "args": ["mcp-stdio"]}),
    );
    if let Some(unzoo) = unzoo_service_path() {
        // Unzoo gives each client its own isolated browser profile and tabs.
        // Not `claude-code`: that is the person's own Claude Code, whose tabs
        // an agent running here must not navigate away (it did, live).
        servers.insert(
            "unzoo".into(),
            json!({"type": "stdio", "command": unzoo.display().to_string(),
                   "args": ["--mcp", "stdio", "--client", super::browser::CLIENT]}),
        );
    }
    let mut prompt = GOVERNED_PROMPT.to_string();
    if let Some(profile) = browser_profile.map(str::trim).filter(|p| !p.is_empty()) {
        if unzoo_service_path().is_some() && profile != super::browser::RESERVED_PROFILE {
            prompt.push(' ');
            prompt.push_str(&super::browser::profile_instructions(profile));
        }
    }
    Ok(vec![
        "-p".into(),
        "--disallowedTools".into(),
        "Bash,PowerShell,WebFetch".into(),
        "--mcp-config".into(),
        json!({"mcpServers": servers}).to_string(),
        "--append-system-prompt".into(),
        prompt,
    ])
}

/// The adapter a run of this agent is read with. Said back to the console
/// so it knows which vocabulary the events came from.
fn adapter_of(agent_id: &str) -> &'static str {
    match agent_id {
        "claude-code" => "claude",
        "codex-cli" => "codex",
        "opencode" => "opencode",
        "agy" | "antigravity" => "agy",
        _ => "generic",
    }
}

fn parse_body(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

fn string_field<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// POST /api/agent/start — run one agent against one prompt.
pub fn api_start(handler: &McpHandler, body: &[u8]) -> Response {
    match start_session(handler, &parse_body(body)) {
        Ok(value) => Response::ok_json(value),
        Err((code, reason, message)) => Response::err(code, reason, &message),
    }
}

/// Why a start was refused: status code, reason phrase, message.
pub(crate) type StartError = (u16, &'static str, String);

/// Start a session from a start body (`agent`, `prompt`, `hosted`,
/// `governed`, `cwd`, ids). The one path both `/api/agent/start` and the
/// console's task runner take, so a task the runtime starts on its own is
/// started exactly as one the console asked for.
pub(crate) fn start_session(handler: &McpHandler, body: &Value) -> Result<Value, StartError> {
    let Some(agent_id) = string_field(body, "agent") else {
        return Err((400, "Bad Request", "agent is required".to_string()));
    };
    let Some(prompt) = string_field(body, "prompt") else {
        return Err((400, "Bad Request", "prompt is required".to_string()));
    };
    let governed = body.get("governed").and_then(Value::as_bool).unwrap_or(false);
    let hosted = governed || body.get("hosted").and_then(Value::as_bool).unwrap_or(false);
    let governed_args = if governed {
        match governed_args(agent_id, string_field(body, "browser_profile")) {
            Ok(args) => Some(args),
            Err(why) => return Err((400, "Bad Request", why.to_string())),
        }
    } else {
        None
    };
    let args = if hosted { hosted_args(agent_id) } else { headless_args(agent_id) };
    let Some(args) = args else {
        return Err((
            400,
            "Bad Request",
            format!(
                "{agent_id} has no {} mode the console knows how to drive",
                if hosted { "hosted" } else { "headless" }
            ),
        ));
    };

    let binary = match super::local_agents::local_agent(agent_id) {
        // Built in: found where its installer puts it.
        Some(local) => match local.binary() {
            Some(path) => path.display().to_string(),
            None => {
                return Err((503, "Service Unavailable", format!("{} is not installed on this machine", local.name)))
            }
        },
        None => {
            let set = match fetch_manifests() {
                Ok(set) => set,
                Err(e) => return Err((503, "Service Unavailable", format!("manifest fetch: {e}"))),
            };
            let Some(manifest) = set
                .for_current_platform()
                .into_iter()
                .find(|m| m.id == agent_id)
            else {
                return Err((404, "Not Found", format!("no agent named {agent_id}")));
            };

            // Resolve once, here: on Windows the binary is a .cmd shim that a
            // bare name does not spawn, and the detected path is already
            // absolute.
            let detected = installer::detect(&manifest.detect);
            if !detected.ok {
                return Err((
                    503,
                    "Service Unavailable",
                    format!("{} is not installed on this machine", manifest.name),
                ));
            }
            let Some(binary) = detected.binary_path else {
                return Err((503, "Service Unavailable", "the agent has no resolved path".to_string()));
            };
            binary
        }
    };

    let mut command = vec![json!(binary)];
    match &governed_args {
        Some(governed) => command.extend(governed.iter().map(|arg| json!(arg))),
        None => command.extend(args.iter().map(|arg| json!(arg))),
    }
    // The enabled skills, as a plugin for this session only. Claude Code is
    // the CLI that loads one from a folder; a skill that cannot be laid out
    // is not a reason to refuse the run.
    if agent_id == "claude-code" {
        match super::skills::prepare_plugin() {
            Ok(Some(dir)) => {
                command.push(json!("--plugin-dir"));
                command.push(json!(dir.display().to_string()));
            }
            Ok(None) => {}
            Err(error) => log::warn!("skills were not loaded for this run: {error:#}"),
        }
    }

    let mut params = serde_json::Map::new();
    params.insert("command".into(), Value::Array(command));
    params.insert("prompt".into(), json!(prompt));
    if hosted {
        params.insert("hosted".into(), json!(true));
    }
    if governed {
        params.insert("governed".into(), json!(true));
    }
    // How much it may do unasked: only a hosted agent asks anything.
    if let Some(level) = string_field(body, "autonomy") {
        if !hosted {
            return Err((400, "Bad Request", "autonomy needs a hosted session".to_string()));
        }
        params.insert("autonomy".into(), json!(level));
    }
    // Where the agent works: the workspace the console has open. Only an
    // existing directory -- it decides where the agent reads and writes, not
    // what runs, but a typo should fail here rather than at spawn.
    if let Some(cwd) = string_field(body, "cwd") {
        if !std::path::Path::new(cwd).is_dir() {
            return Err((400, "Bad Request", format!("cwd is not a directory: {cwd}")));
        }
        params.insert("cwd".into(), json!(cwd));
    }
    // Carried onto every event and log line, so the console can thread them
    // back onto the task they belong to.
    for key in ["task_id", "run_id", "step_id", "idempotency_key"] {
        if let Some(value) = string_field(body, key) {
            params.insert(key.into(), json!(value));
        }
    }

    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    match handler.handle(&ctx, "agent_session.start", &Value::Object(params)) {
        Ok(value) => Ok(json!({
            "agent": agent_id,
            "adapter": adapter_of(agent_id),
            "hosted": hosted,
            "governed": governed,
            "session": value,
        })),
        Err(e) => Err((400, "Bad Request", e.to_string())),
    }
}

/// The rest of a session's life: events, status, input, interrupt, close.
///
/// These only ever address an existing session by id, so they pass through
/// with the fields each method takes and nothing else.
pub fn api_session_act(handler: &McpHandler, method: &str, body: &[u8]) -> Response {
    let body = parse_body(body);
    let Some(session_id) = string_field(&body, "session_id") else {
        return Response::err(400, "Bad Request", "session_id is required");
    };

    let mut params = serde_json::Map::new();
    params.insert("session_id".into(), json!(session_id));
    if let Some(cursor) = body.get("cursor").and_then(Value::as_u64) {
        params.insert("cursor".into(), json!(cursor));
    }
    if let Some(text) = string_field(&body, "text") {
        params.insert("text".into(), json!(text));
    }
    if let Some(grace) = body.get("grace_ms").and_then(Value::as_u64) {
        params.insert("grace_ms".into(), json!(grace));
    }

    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    match handler.handle(&ctx, method, &Value::Object(params)) {
        Ok(value) => Response::ok_json(value),
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

/// GET /api/agent/approvals — the inbox: every question a hosted session is
/// waiting on, oldest first, each the `approval.requested` event as
/// published (tool, arguments, description, turn, task/run/step ids) plus
/// `session_id` and `adapter`.
///
/// A read, so it goes straight to the session layer; answering goes through
/// the handler below, which audits it.
pub fn api_approvals() -> Response {
    Response::ok_json(json!({
        "approvals": unterm_services::agent_session::pending_requests(),
    }))
}

/// The decision fields of a POST /api/agent/decide body, checked. Kept apart
/// from the handler call so the checks can be tested without a session.
fn decide_params(body: &Value) -> Result<serde_json::Map<String, Value>, &'static str> {
    let session_id = string_field(body, "session_id").ok_or("session_id is required")?;
    let request_id = string_field(body, "request_id").ok_or("request_id is required")?;
    let allow = body
        .get("allow")
        .and_then(Value::as_bool)
        .ok_or("allow is required: true or false, never assumed")?;
    let mut params = serde_json::Map::new();
    params.insert("session_id".into(), json!(session_id));
    params.insert("request_id".into(), json!(request_id));
    params.insert("allow".into(), json!(allow));
    if allow && body.get("remember").and_then(Value::as_bool) == Some(true) {
        params.insert("remember".into(), json!(true));
    }
    if !allow {
        if let Some(message) = string_field(body, "message") {
            params.insert("message".into(), json!(message));
        }
        if let Some(interrupt) = body.get("interrupt").and_then(Value::as_bool) {
            params.insert("interrupt".into(), json!(interrupt));
        }
    }
    Ok(params)
}

/// POST /api/agent/decide — answer one question: `allow`, or deny with an
/// optional `message` and `interrupt` (also stop the turn).
pub fn api_decide(handler: &McpHandler, body: &[u8]) -> Response {
    let params = match decide_params(&parse_body(body)) {
        Ok(params) => params,
        Err(problem) => return Response::err(400, "Bad Request", problem),
    };
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    match handler.handle(&ctx, "agent_session.decide", &Value::Object(params)) {
        Ok(value) => Response::ok_json(value),
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

/// POST /api/agent/resume — `{session_id, prompt?}`: continue a hosted
/// session's conversation in a new session (see `agent_session.resume`).
/// Only a session the console could have started can be resumed this way:
/// the command comes from the earlier session's record, never the request.
pub fn api_resume(handler: &McpHandler, body: &[u8]) -> Response {
    let body = parse_body(body);
    let Some(session_id) = string_field(&body, "session_id") else {
        return Response::err(400, "Bad Request", "session_id is required");
    };
    let mut params = serde_json::Map::new();
    params.insert("session_id".into(), json!(session_id));
    if let Some(prompt) = string_field(&body, "prompt") {
        params.insert("prompt".into(), json!(prompt));
    }
    // A continued conversation is a new run of the same task: the console
    // names the run and step, and the task id defaults to the earlier one's.
    for key in ["task_id", "run_id", "step_id"] {
        if let Some(value) = string_field(&body, key) {
            params.insert(key.into(), json!(value));
        }
    }
    let ctx = unterm_mcp::handler::ConnectionContext::internal("web_settings");
    match handler.handle(&ctx, "agent_session.resume", &Value::Object(params)) {
        Ok(value) => Response::ok_json(value),
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

/// GET /api/agent/policy — the standing rules, and where they are kept.
pub fn api_policy_list() -> Response {
    match unterm_services::agent_policy::list() {
        Ok(rules) => Response::ok_json(json!({
            "rules": rules,
            "store": unterm_services::agent_policy::store_file().map(|p| p.display().to_string()),
        })),
        Err(e) => Response::err(500, "Internal Server Error", &e.to_string()),
    }
}

/// POST /api/agent/policy — add a rule: `{tool, command_prefix?, workspace?,
/// decision: "allow"|"deny", note?}`.
///
/// Only here, behind the console's token: there is no MCP method for it,
/// because an agent that could add allow rules would approve itself.
pub fn api_policy_add(body: &[u8]) -> Response {
    let draft: unterm_services::agent_policy::Draft = match serde_json::from_slice(body) {
        Ok(draft) => draft,
        Err(e) => return Response::err(400, "Bad Request", &format!("a rule needs tool and decision: {e}")),
    };
    match unterm_services::agent_policy::add(draft, "console") {
        Ok(rule) => Response::ok_json(json!({"rule": rule})),
        Err(e) => Response::err(400, "Bad Request", &e.to_string()),
    }
}

/// POST /api/agent/policy/remove — `{id}`.
pub fn api_policy_remove(body: &[u8]) -> Response {
    let body = parse_body(body);
    let Some(id) = string_field(&body, "id") else {
        return Response::err(400, "Bad Request", "id is required");
    };
    match unterm_services::agent_policy::remove(id) {
        Ok(true) => Response::ok_json(json!({"removed": id})),
        Ok(false) => Response::err(404, "Not Found", &format!("no rule {id}")),
        Err(e) => Response::err(500, "Internal Server Error", &e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_agents_with_a_known_headless_mode_are_runnable() {
        assert!(headless_args("claude-code").is_some());
        assert!(headless_args("codex-cli").is_some());
        // An agent we have not taught the console to drive is refused rather
        // than launched with whatever its interactive flags happen to be.
        assert!(headless_args("gemini-cli").is_none());
        assert!(headless_args("").is_none());
        assert!(headless_args("../../evil").is_none());
    }

    #[test]
    fn capabilities_say_what_governed_runs_can_mount() {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join(if cfg!(windows) { "unterm-cli.exe" } else { "unterm-cli" });
        let browser = dir.path().join("unzoo-service.exe");
        std::fs::write(&cli, b"").unwrap();
        std::env::set_var("UNTERM_CLI", &cli);
        std::env::set_var("UNZOO_SERVICE", dir.path().join("missing.exe"));
        let found = capabilities();
        assert_eq!(found["terminal"]["governed_ready"], true);
        assert_eq!(found["browser"]["installed"], false);
        std::fs::write(&browser, b"").unwrap();
        std::env::set_var("UNZOO_SERVICE", &browser);
        assert_eq!(capabilities()["browser"]["installed"], true);
        std::env::set_var("UNTERM_CLI", dir.path().join("gone.exe"));
        assert_eq!(capabilities()["terminal"]["governed_ready"], false);
        std::env::remove_var("UNTERM_CLI");
        std::env::remove_var("UNZOO_SERVICE");
    }

    #[test]
    fn claude_gets_the_flags_its_adapter_parses() {
        let args = headless_args("claude-code").expect("claude");
        assert!(args.contains(&"-p"), "headless, not interactive");
        assert!(args.contains(&"stream-json"), "the adapter reads JSON lines");
        assert!(args.contains(&"--verbose"), "streamed, not buffered to one message");
    }

    #[test]
    fn a_start_without_an_agent_or_prompt_is_refused() {
        // No handler needed: both checks run before anything is dispatched.
        let missing_agent = parse_body(br#"{"prompt":"hi"}"#);
        assert!(string_field(&missing_agent, "agent").is_none());
        let missing_prompt = parse_body(br#"{"agent":"claude-code"}"#);
        assert!(string_field(&missing_prompt, "prompt").is_none());
        // An empty string is not a value.
        let empty = parse_body(br#"{"agent":"","prompt":""}"#);
        assert!(string_field(&empty, "agent").is_none());
        assert!(string_field(&empty, "prompt").is_none());
    }

    #[test]
    fn hosted_runs_are_offered_only_for_agents_with_a_host_protocol() {
        for agent in ["claude-code", "codex-cli", "opencode", "agy"] {
            assert!(hosted_args(agent).is_some(), "{agent}");
            let adapter = adapter_of(agent);
            assert!(
                unterm_brain::host::host_for(adapter).is_some(),
                "{agent} is offered hosted, so {adapter} must have a host protocol"
            );
        }
        assert!(hosted_args("gemini-cli").is_none());
        assert!(hosted_args("aider").is_none());
        // What the agent list tells the console agrees with what start does.
        assert!(supports_hosted("codex-cli") && !supports_hosted("aider"));
        assert!(supports_governed("claude-code"));
        assert!(!supports_governed("codex-cli"));
        assert!(governed_args("codex-cli", None).is_err());
    }

    #[test]
    fn the_governed_preset_switches_the_shell_off_and_mounts_unterm() {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join(if cfg!(windows) { "unterm-cli.exe" } else { "unterm-cli" });
        std::fs::write(&cli, b"").unwrap();
        std::env::set_var("UNTERM_CLI", &cli);
        let args = governed_args("claude-code", None).unwrap();
        let at = |flag: &str| args.iter().position(|a| a == flag).map(|i| args[i + 1].clone());
        let off = at("--disallowedTools").unwrap();
        for tool in ["Bash", "PowerShell", "WebFetch"] {
            assert!(off.split(',').any(|t| t == tool), "{tool} must be off: {off}");
        }
        let config: Value = serde_json::from_str(&at("--mcp-config").unwrap()).unwrap();
        assert_eq!(config["mcpServers"]["unterm"]["command"], cli.display().to_string());
        assert_eq!(config["mcpServers"]["unterm"]["args"], json!(["mcp-stdio"]));
        assert!(at("--append-system-prompt").unwrap().contains("exec.run_wait"));
        assert!(args.contains(&"-p".to_string()));
        // Only Claude Code can have its shell switched off today.
        assert!(governed_args("codex-cli", None).is_err());
        std::env::set_var("UNTERM_CLI", dir.path().join("missing"));
        assert!(governed_args("claude-code", None).is_err(), "no unterm-cli, no governed mode");
        std::env::remove_var("UNTERM_CLI");
    }

    #[test]
    fn unzoo_is_mounted_as_the_agents_own_client_and_a_chosen_profile_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join(if cfg!(windows) { "unterm-cli.exe" } else { "unterm-cli" });
        let unzoo = dir.path().join("unzoo-service.exe");
        std::fs::write(&cli, b"").unwrap();
        std::fs::write(&unzoo, b"").unwrap();
        std::env::set_var("UNTERM_CLI", &cli);
        std::env::set_var("UNZOO_SERVICE", &unzoo);
        let prompt_of = |args: &[String]| {
            let at = args.iter().position(|a| a == "--append-system-prompt").unwrap();
            args[at + 1].clone()
        };
        let mounted = |args: &[String]| {
            let at = args.iter().position(|a| a == "--mcp-config").unwrap();
            serde_json::from_str::<Value>(&args[at + 1]).unwrap()["mcpServers"]["unzoo"]["args"].clone()
        };

        let own = governed_args("claude-code", None).unwrap();
        // Not the person's own Claude Code's profile.
        assert_eq!(mounted(&own), json!(["--mcp", "stdio", "--client", "unzoo-one-agent"]));
        assert!(!prompt_of(&own).contains("profile_id"));
        // Its own reserved profile needs no instructions either.
        let reserved = governed_args("claude-code", Some(super::super::browser::RESERVED_PROFILE)).unwrap();
        assert!(!prompt_of(&reserved).contains("profile_id"));
        // A chosen one is named on every call, in tabs the agent opened.
        let chosen = governed_args("claude-code", Some("default")).unwrap();
        assert!(prompt_of(&chosen).contains("profile_id=\"default\""), "{}", prompt_of(&chosen));

        std::env::remove_var("UNTERM_CLI");
        std::env::remove_var("UNZOO_SERVICE");
    }

    #[test]
    fn a_decision_must_say_allow_or_deny_and_whom_it_answers() {
        let ok = decide_params(&json!({"session_id": "ags_1", "request_id": "0", "allow": false,
                                        "message": "not here", "interrupt": true}))
        .unwrap();
        assert_eq!(Value::Object(ok), json!({"session_id": "ags_1", "request_id": "0", "allow": false,
                                              "message": "not here", "interrupt": true}));
        // Allowing carries nothing else: no message, no interrupt.
        let allow = decide_params(&json!({"session_id": "a", "request_id": "r", "allow": true, "interrupt": true})).unwrap();
        assert!(allow.get("interrupt").is_none());
        // remember belongs to an allow; a refusal is never kept as a yes.
        let kept = decide_params(&json!({"session_id": "a", "request_id": "r", "allow": true, "remember": true})).unwrap();
        assert_eq!(kept.get("remember"), Some(&json!(true)));
        let refused = decide_params(&json!({"session_id": "a", "request_id": "r", "allow": false, "remember": true})).unwrap();
        assert!(refused.get("remember").is_none());
        // A missing allow is refused, never read as yes.
        assert!(decide_params(&json!({"session_id": "a", "request_id": "r"})).is_err());
        assert!(decide_params(&json!({"session_id": "a", "request_id": "r", "allow": "yes"})).is_err());
        assert!(decide_params(&json!({"request_id": "r", "allow": true})).is_err());
        assert!(decide_params(&json!({"session_id": "a", "allow": true})).is_err());
    }

    #[test]
    fn a_malformed_body_does_not_panic() {
        assert_eq!(parse_body(b"not json"), Value::Null);
        assert!(string_field(&parse_body(b"not json"), "agent").is_none());
    }
}
