//! Plugins: programs that run beside Unzoo One, with their own control page,
//! state and tools.
//!
//! A skill is know-how an agent reads; a plugin is code that runs. Anything
//! that needs a page of its own, state that persists (accounts, queues,
//! drafts) or code of its own is a plugin. A plugin is a folder with an
//! `unzoo-plugin.json`:
//!
//! ```json
//! {
//!   "id": "xtools", "name": "Xtools", "version": "1.0.0", "description": "...",
//!   "run":  { "command": ["python", "{dir}/server.py", "--port", "{port}"], "health": "/api/ping" },
//!   "page": "/",
//!   "mcp":  { "command": ["python", "{dir}/mcp_server.py"], "tools": { "drafts": "read", "publish": "send" } },
//!   "roles": {
//!     "account": { "kind": "browser", "label": "X account" },
//!     "writer":  { "kind": "agent", "label": "Writing", "default": { "agent": "claude-code", "model": "sonnet" } }
//!   },
//!   "permissions": { "domains": ["x.com"] }
//! }
//! ```
//!
//! - `run`: the Core starts it when the plugin is enabled, stops it when it is
//!   disabled or the Core quits, restarts it (a few times) when it crashes.
//!   Its output goes to a log the page can show.
//! - `page`: shown inside Unzoo One, from the port the Core gave the program.
//! - `mcp`: mounted for governed agents beside the terminal and the browser.
//!   Each tool says what it does (`read`, `change`, `run`, `send`, `pay`,
//!   `delete`); a tool that does not say counts as `send`, so at the usual
//!   autonomy level it is asked.
//! - `roles`: what the person assigns: a browser identity for each `browser`
//!   role, an agent and model for each `agent` role. The program reads them
//!   from `UNZOO_PLUGIN_ROLES`.
//!
//! Placeholders in commands: `{dir}` (the plugin's folder), `{port}`,
//! `{data}` (a folder the plugin may keep its state in).

use crate::server::Response;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const MANIFEST: &str = "unzoo-plugin.json";

/// How many times a crashed program is started again before it is left
/// stopped, within [`RESTART_WINDOW`].
const MAX_RESTARTS: usize = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(600);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(30);

// ---- the manifest ----------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub run: Option<RunSpec>,
    pub page: Option<String>,
    pub mcp: Option<McpSpec>,
    pub roles: Vec<Role>,
    pub domains: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunSpec {
    pub command: Vec<String>,
    pub health: Option<String>,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpSpec {
    pub command: Vec<String>,
    /// Tool name -> what it does, as one of [`RISK_WORDS`].
    pub tools: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Role {
    pub name: String,
    pub kind: RoleKind,
    pub label: String,
    pub default: Value,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RoleKind {
    Browser,
    Agent,
}

/// What a plugin's tool may say it does, and the risk each word means.
pub const RISK_WORDS: &[&str] = &["read", "change", "run", "send", "pay", "delete"];

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
}

fn words(value: Option<&Value>, what: &str) -> Result<Vec<String>, String> {
    let list = value.and_then(Value::as_array).ok_or(format!("{what} must be a list of words"))?;
    let words: Vec<String> = list.iter().filter_map(Value::as_str).map(str::to_string).collect();
    if words.is_empty() || words.len() != list.len() {
        return Err(format!("{what} must be a non-empty list of words"));
    }
    Ok(words)
}

pub fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let raw: Value = serde_json::from_str(text).map_err(|e| format!("{MANIFEST} is not valid JSON: {e}"))?;
    let text_of = |key: &str| raw.get(key).and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
    let id = text_of("id");
    if !valid_id(&id) {
        return Err("id must be 1-40 lowercase letters, digits or dashes".into());
    }
    let name = Some(text_of("name")).filter(|n| !n.is_empty()).unwrap_or_else(|| id.clone());
    let run = match raw.get("run") {
        None | Some(Value::Null) => None,
        Some(run) => Some(RunSpec {
            command: words(run.get("command"), "run.command")?,
            health: run.get("health").and_then(Value::as_str).map(str::to_string),
            env: run
                .get("env")
                .and_then(Value::as_object)
                .map(|env| env.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect())
                .unwrap_or_default(),
        }),
    };
    let page = raw.get("page").and_then(Value::as_str).map(str::to_string);
    if page.is_some() && run.is_none() {
        return Err("a page needs run: the page is served by the plugin's program".into());
    }
    let mcp = match raw.get("mcp") {
        None | Some(Value::Null) => None,
        Some(mcp) => {
            let mut tools = Vec::new();
            if let Some(map) = mcp.get("tools").and_then(Value::as_object) {
                for (tool, word) in map {
                    let word = word.as_str().unwrap_or("");
                    if !RISK_WORDS.contains(&word) {
                        return Err(format!("mcp.tools.{tool} must be one of {}", RISK_WORDS.join(", ")));
                    }
                    tools.push((tool.clone(), word.to_string()));
                }
            }
            Some(McpSpec { command: words(mcp.get("command"), "mcp.command")?, tools })
        }
    };
    let mut roles = Vec::new();
    if let Some(map) = raw.get("roles").and_then(Value::as_object) {
        for (role, spec) in map {
            let kind = match spec.get("kind").and_then(Value::as_str) {
                Some("browser") => RoleKind::Browser,
                Some("agent") => RoleKind::Agent,
                _ => return Err(format!("roles.{role}.kind must be browser or agent")),
            };
            roles.push(Role {
                name: role.clone(),
                kind,
                label: spec.get("label").and_then(Value::as_str).unwrap_or(role).to_string(),
                default: spec.get("default").cloned().unwrap_or(Value::Null),
            });
        }
    }
    let domains = raw
        .pointer("/permissions/domains")
        .and_then(Value::as_array)
        .map(|d| d.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    Ok(Manifest {
        id,
        name,
        version: text_of("version"),
        description: text_of("description"),
        run,
        page,
        mcp,
        roles,
        domains,
    })
}

pub fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let file = dir.join(MANIFEST);
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    parse_manifest(&text)
}

/// `{dir}`, `{port}` and `{data}` filled in.
pub fn fill(word: &str, dir: &Path, port: Option<u16>, data: &Path) -> String {
    let mut out = word.replace("{dir}", &dir.display().to_string()).replace("{data}", &data.display().to_string());
    if let Some(port) = port {
        out = out.replace("{port}", &port.to_string());
    }
    out
}

/// The MCP server name a plugin's tools appear under: `plugin_<id>`, with
/// dashes as underscores (CLIs build tool names as `mcp__<server>__<tool>`).
pub fn server_name(id: &str) -> String {
    format!("plugin_{}", id.replace('-', "_"))
}

// ---- the registry ----------------------------------------------------------

/// What the person set up: which folders are plugins, which are on, and what
/// each role is bound to. `plugins/registry.json` in the state folder.
#[derive(Debug, Clone, Default)]
struct Entry {
    id: String,
    dir: PathBuf,
    enabled: bool,
    bindings: Map<String, Value>,
}

fn registry_path() -> Option<PathBuf> {
    unterm_protocol::state_path("plugins/registry.json")
}

fn data_dir(id: &str) -> Option<PathBuf> {
    unterm_protocol::state_path(format!("plugins/{id}/data"))
}

fn log_path(id: &str) -> Option<PathBuf> {
    unterm_protocol::state_path(format!("plugins/{id}/log.txt"))
}

fn load() -> Vec<Entry> {
    let Some(text) = registry_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Vec::new();
    };
    let raw: Value = serde_json::from_str(&text).unwrap_or_default();
    raw.get("plugins")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|e| {
                    Some(Entry {
                        id: e.get("id")?.as_str()?.to_string(),
                        dir: PathBuf::from(e.get("dir")?.as_str()?),
                        enabled: e.get("enabled").and_then(Value::as_bool).unwrap_or(false),
                        bindings: e.get("bindings").and_then(Value::as_object).cloned().unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn save(entries: &[Entry]) -> Result<(), String> {
    let path = registry_path().ok_or("there is no state folder")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let list: Vec<Value> = entries
        .iter()
        .map(|e| json!({"id": e.id, "dir": e.dir.display().to_string(), "enabled": e.enabled, "bindings": e.bindings}))
        .collect();
    std::fs::write(&path, serde_json::to_string_pretty(&json!({"plugins": list})).unwrap()).map_err(|e| e.to_string())?;
    write_tool_risks(entries);
    Ok(())
}

/// The risk of each enabled plugin's tools, for [`unterm_services::tool_risk`]
/// to read when an agent calls one (it lives below this crate).
fn write_tool_risks(entries: &[Entry]) {
    let mut servers = Map::new();
    for entry in entries.iter().filter(|e| e.enabled) {
        if let Ok(manifest) = read_manifest(&entry.dir) {
            if let Some(mcp) = manifest.mcp {
                let tools: Map<String, Value> = mcp.tools.into_iter().map(|(t, w)| (t, json!(w))).collect();
                servers.insert(server_name(&manifest.id), json!({"tools": tools, "default": "send"}));
            }
        }
    }
    if let Some(path) = unterm_protocol::state_path(unterm_services::tool_risk::PLUGIN_RISKS_FILE) {
        let _ = std::fs::write(path, serde_json::to_string_pretty(&json!({"servers": servers})).unwrap());
    }
}

// ---- running programs ------------------------------------------------------

struct Running {
    child: Child,
    port: Option<u16>,
    started: Instant,
    healthy: bool,
}

#[derive(Default)]
struct State {
    running: HashMap<String, Running>,
    /// Start times of recent restarts after a crash, per plugin.
    restarts: HashMap<String, Vec<Instant>>,
    /// Why a plugin is not running, when it should be.
    errors: HashMap<String, String>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> T {
    let mut guard = STATE.lock().unwrap_or_else(|p| p.into_inner());
    f(guard.get_or_insert_with(State::default))
}

fn free_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok().map(|a| a.port())
}

/// The roles as the program reads them: each role's binding, or its default.
fn roles_env(manifest: &Manifest, bindings: &Map<String, Value>) -> Value {
    let mut out = Map::new();
    for role in &manifest.roles {
        let bound = bindings.get(&role.name).cloned().filter(|v| !v.is_null()).unwrap_or_else(|| role.default.clone());
        out.insert(
            role.name.clone(),
            json!({"kind": if role.kind == RoleKind::Browser { "browser" } else { "agent" }, "value": bound}),
        );
    }
    Value::Object(out)
}

fn start(entry: &Entry) -> Result<(), String> {
    let manifest = read_manifest(&entry.dir)?;
    let Some(run) = manifest.run.clone() else {
        return Ok(()); // tools only: nothing to keep running
    };
    let data = data_dir(&manifest.id).ok_or("there is no state folder")?;
    std::fs::create_dir_all(&data).map_err(|e| e.to_string())?;
    let port = free_port().ok_or("no free port")?;
    let words: Vec<String> = run.command.iter().map(|w| fill(w, &entry.dir, Some(port), &data)).collect();
    let log = log_path(&manifest.id).ok_or("there is no state folder")?;
    let out = std::fs::File::create(&log).map_err(|e| format!("{}: {e}", log.display()))?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    let mut command = Command::new(&words[0]);
    command
        .args(&words[1..])
        .current_dir(&entry.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .env("UNZOO_PLUGIN_ID", &manifest.id)
        .env("UNZOO_PLUGIN_DIR", &entry.dir)
        .env("UNZOO_PLUGIN_DATA", &data)
        .env("UNZOO_PLUGIN_PORT", port.to_string())
        .env("PORT", port.to_string())
        .env("UNZOO_PLUGIN_ROLES", roles_env(&manifest, &entry.bindings).to_string())
        // Python programs print UTF-8 to the log whatever the console's code page.
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8");
    for (key, value) in &run.env {
        command.env(key, fill(value, &entry.dir, Some(port), &data));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Its own hidden console, so the console programs it starts share it
        // instead of each opening a window. It inherits the Core's
        // kill-on-close job: a Core that dies takes its plugins with it.
        command.creation_flags(0x0800_0000);
    }
    let child = command.spawn().map_err(|e| format!("could not start {}: {e}", words[0]))?;
    with_state(|s| {
        s.errors.remove(&manifest.id);
        s.running.insert(manifest.id.clone(), Running { child, port: Some(port), started: Instant::now(), healthy: run.health.is_none() });
    });
    if let Some(health) = run.health {
        let id = manifest.id.clone();
        std::thread::spawn(move || wait_healthy(&id, port, &health));
    }
    Ok(())
}

fn answers(port: u16, path: &str) -> bool {
    use std::io::{Read, Write};
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(2)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut head = [0u8; 12];
    stream.read_exact(&mut head).is_ok() && head.starts_with(b"HTTP/1.") && head[9] == b'2'
}

fn wait_healthy(id: &str, port: u16, path: &str) {
    let deadline = Instant::now() + HEALTH_TIMEOUT;
    while Instant::now() < deadline {
        let still_ours = with_state(|s| s.running.get(id).map(|r| r.port == Some(port)).unwrap_or(false));
        if !still_ours {
            return;
        }
        if answers(port, path) {
            with_state(|s| {
                if let Some(r) = s.running.get_mut(id) {
                    r.healthy = true;
                }
            });
            return;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    with_state(|s| {
        s.errors.insert(id.to_string(), format!("did not answer {path} within {}s; see its log", HEALTH_TIMEOUT.as_secs()));
    });
}

fn stop(id: &str) {
    let running = with_state(|s| s.running.remove(id));
    if let Some(mut running) = running {
        // The whole tree: a Python program's own children, a browser driver.
        let _ = unterm_services::process_lifetime::terminate_tree(running.child.id());
        let _ = running.child.kill();
        let _ = running.child.wait();
    }
}

/// Every running plugin program, stopped: the Core is quitting.
pub fn stop_all() {
    let ids: Vec<String> = with_state(|s| s.running.keys().cloned().collect());
    for id in ids {
        stop(&id);
    }
}

/// Start the enabled plugins and keep them running: called once when the
/// Core's console server starts.
pub fn start_supervisor() {
    let _ = std::thread::Builder::new().name("plugins".into()).spawn(|| {
        let entries = load();
        write_tool_risks(&entries);
        for entry in entries.iter().filter(|e| e.enabled) {
            if let Err(error) = start(entry) {
                with_state(|s| s.errors.insert(entry.id.clone(), error));
            }
        }
        loop {
            std::thread::sleep(Duration::from_secs(2));
            watch_once();
        }
    });
}

/// A program that exited on its own is started again, a few times.
fn watch_once() {
    let exited: Vec<(String, String)> = with_state(|s| {
        let mut out = Vec::new();
        for (id, running) in s.running.iter_mut() {
            if let Ok(Some(status)) = running.child.try_wait() {
                out.push((id.clone(), format!("exited ({status})")));
            }
        }
        for (id, _) in &out {
            s.running.remove(id);
        }
        out
    });
    for (id, why) in exited {
        let Some(entry) = load().into_iter().find(|e| e.id == id && e.enabled) else {
            continue;
        };
        let allowed = with_state(|s| {
            let recent = s.restarts.entry(id.clone()).or_default();
            recent.retain(|t| t.elapsed() < RESTART_WINDOW);
            if recent.len() >= MAX_RESTARTS {
                s.errors.insert(id.clone(), format!("{why}; stopped after {MAX_RESTARTS} restarts, see its log"));
                false
            } else {
                recent.push(Instant::now());
                true
            }
        });
        if allowed {
            log::warn!("plugin {id} {why}; starting it again");
            if let Err(error) = start(&entry) {
                with_state(|s| s.errors.insert(id.clone(), error));
            }
        }
    }
}

// ---- what agents see -------------------------------------------------------

/// One plugin MCP server as the agent's CLI starts it.
pub struct McpMount {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// What the plugin's program is given too: the CLI starts this server
    /// itself, and without them the tools would keep their own data apart
    /// from the page's and know nothing of the roles.
    pub env: Vec<(String, String)>,
}

/// The enabled plugins' MCP servers, mounted for governed agents beside the
/// terminal and the browser.
pub fn mcp_servers() -> Vec<McpMount> {
    let mut out = Vec::new();
    for entry in load().into_iter().filter(|e| e.enabled) {
        let Ok(manifest) = read_manifest(&entry.dir) else { continue };
        let Some(mcp) = &manifest.mcp else { continue };
        let Some(data) = data_dir(&manifest.id) else { continue };
        let port = with_state(|s| s.running.get(&manifest.id).and_then(|r| r.port));
        let words: Vec<String> = mcp.command.iter().map(|w| fill(w, &entry.dir, port, &data)).collect();
        let mut env = vec![
            ("UNZOO_PLUGIN_ID".to_string(), manifest.id.clone()),
            ("UNZOO_PLUGIN_DIR".to_string(), entry.dir.display().to_string()),
            ("UNZOO_PLUGIN_DATA".to_string(), data.display().to_string()),
            ("UNZOO_PLUGIN_ROLES".to_string(), roles_env(&manifest, &entry.bindings).to_string()),
            ("PYTHONUTF8".to_string(), "1".to_string()),
            ("PYTHONIOENCODING".to_string(), "utf-8".to_string()),
        ];
        if let Some(port) = port {
            env.push(("UNZOO_PLUGIN_PORT".to_string(), port.to_string()));
        }
        out.push(McpMount { name: server_name(&manifest.id), command: words[0].clone(), args: words[1..].to_vec(), env });
    }
    out
}

/// One sentence per enabled plugin with tools, for the governed prompt.
pub fn prompt() -> String {
    let named: Vec<String> = load()
        .into_iter()
        .filter(|e| e.enabled)
        .filter_map(|e| read_manifest(&e.dir).ok())
        .filter(|m| m.mcp.is_some())
        .map(|m| {
            let what = if m.description.is_empty() { String::new() } else { format!(": {}", m.description) };
            format!("{} (tools {}__*){what}", m.name, server_name(&m.id))
        })
        .collect();
    if named.is_empty() {
        return String::new();
    }
    format!(
        "Plugins the person installed are available as tools: {}. Use a plugin's tools for its job instead of redoing it in the browser.",
        named.join("; ")
    )
}

// ---- the console's API -----------------------------------------------------

fn view(entry: &Entry) -> Value {
    let manifest = read_manifest(&entry.dir);
    let (status, port, error) = with_state(|s| {
        let error = s.errors.get(&entry.id).cloned();
        match s.running.get(&entry.id) {
            Some(r) if r.healthy => ("running", r.port, error),
            Some(r) if r.started.elapsed() < HEALTH_TIMEOUT => ("starting", r.port, error),
            Some(r) => ("not_answering", r.port, error),
            None if error.is_some() => ("failed", None, error),
            None => ("stopped", None, None),
        }
    });
    match manifest {
        Err(problem) => json!({"id": entry.id, "dir": entry.dir, "enabled": entry.enabled, "status": "broken", "error": problem}),
        Ok(m) => {
            let tools: Vec<Value> = m
                .mcp
                .as_ref()
                .map(|mcp| mcp.tools.iter().map(|(t, w)| json!({"name": t, "does": w})).collect())
                .unwrap_or_default();
            let roles: Vec<Value> = m
                .roles
                .iter()
                .map(|r| {
                    json!({
                        "name": r.name,
                        "kind": if r.kind == RoleKind::Browser { "browser" } else { "agent" },
                        "label": r.label,
                        "default": r.default,
                        "binding": entry.bindings.get(&r.name).cloned().unwrap_or(Value::Null),
                    })
                })
                .collect();
            let runs = m.run.is_some();
            json!({
                "id": m.id,
                "name": m.name,
                "version": m.version,
                "description": m.description,
                "dir": entry.dir,
                "enabled": entry.enabled,
                "status": if !runs && entry.enabled { "tools_only" } else { status },
                "error": error,
                "port": port,
                "page_url": match (&m.page, port, status) {
                    (Some(page), Some(port), "running") => json!(format!("http://127.0.0.1:{port}{page}")),
                    _ => Value::Null,
                },
                "has_page": m.page.is_some(),
                "mcp_server": m.mcp.as_ref().map(|_| server_name(&m.id)),
                "tools": tools,
                "roles": roles,
                "domains": m.domains,
            })
        }
    }
}

pub fn api_list() -> Response {
    let plugins: Vec<Value> = load().iter().map(view).collect();
    Response::ok_json(json!({"plugins": plugins, "browser_profiles": super::browser::profile_list()}))
}

fn body_str(body: &Value, key: &str) -> Option<String> {
    body.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

pub fn api_add(body: &Value) -> Response {
    let Some(dir) = body_str(body, "dir") else {
        return Response::err(400, "Bad Request", "dir is required: the plugin's folder");
    };
    let dir = PathBuf::from(dir);
    let dir = std::fs::canonicalize(&dir).map(strip_verbatim).unwrap_or(dir);
    let manifest = match read_manifest(&dir) {
        Ok(m) => m,
        Err(problem) => return Response::err(400, "Bad Request", &format!("not a plugin: {problem}")),
    };
    let mut entries = load();
    if let Some(existing) = entries.iter().find(|e| e.id == manifest.id) {
        if existing.dir != dir {
            return Response::err(
                409,
                "Conflict",
                &format!("a plugin named {} is already added from {}", manifest.id, existing.dir.display()),
            );
        }
    } else {
        // Added stopped: the person looks at what it asks for, then turns it on.
        entries.push(Entry { id: manifest.id.clone(), dir: dir.clone(), enabled: false, bindings: Map::new() });
    }
    if let Err(error) = save(&entries) {
        return Response::err(500, "Internal Error", &error);
    }
    Response::ok_json(json!({"ok": true, "id": manifest.id}))
}

/// `\\?\C:\...` from canonicalize, as the path people know.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.display().to_string();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => path,
    }
}

fn with_entry(body: &Value, change: impl FnOnce(&mut Entry) -> Result<(), String>) -> Result<Entry, Response> {
    let Some(id) = body_str(body, "id") else {
        return Err(Response::err(400, "Bad Request", "id is required"));
    };
    let mut entries = load();
    let Some(entry) = entries.iter_mut().find(|e| e.id == id) else {
        return Err(Response::err(404, "Not Found", "no such plugin"));
    };
    change(entry).map_err(|e| Response::err(400, "Bad Request", &e))?;
    let changed = entry.clone();
    save(&entries).map_err(|e| Response::err(500, "Internal Error", &e))?;
    Ok(changed)
}

pub fn api_enable(body: &Value) -> Response {
    let on = body.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    match with_entry(body, |e| {
        e.enabled = on;
        Ok(())
    }) {
        Err(response) => response,
        Ok(entry) => {
            stop(&entry.id);
            with_state(|s| {
                s.errors.remove(&entry.id);
                s.restarts.remove(&entry.id);
            });
            if on {
                if let Err(error) = start(&entry) {
                    with_state(|s| s.errors.insert(entry.id.clone(), error.clone()));
                    return Response::err(500, "Internal Error", &error);
                }
            }
            Response::ok_json(view(&entry))
        }
    }
}

pub fn api_restart(body: &Value) -> Response {
    match with_entry(body, |_| Ok(())) {
        Err(response) => response,
        Ok(entry) if !entry.enabled => Response::err(409, "Conflict", "turn the plugin on first"),
        Ok(entry) => {
            stop(&entry.id);
            with_state(|s| {
                s.errors.remove(&entry.id);
                s.restarts.remove(&entry.id);
            });
            match start(&entry) {
                Ok(()) => Response::ok_json(view(&entry)),
                Err(error) => {
                    with_state(|s| s.errors.insert(entry.id.clone(), error.clone()));
                    Response::err(500, "Internal Error", &error)
                }
            }
        }
    }
}

pub fn api_remove(body: &Value) -> Response {
    let Some(id) = body_str(body, "id") else {
        return Response::err(400, "Bad Request", "id is required");
    };
    stop(&id);
    let mut entries = load();
    let before = entries.len();
    entries.retain(|e| e.id != id);
    if entries.len() == before {
        return Response::err(404, "Not Found", "no such plugin");
    }
    // Its folder and its data stay: removing it from Unzoo One is not
    // deleting someone's files.
    match save(&entries) {
        Ok(()) => Response::ok_json(json!({"ok": true})),
        Err(error) => Response::err(500, "Internal Error", &error),
    }
}

/// Bind a role: `{"id", "role", "value"}`, where value is `{"profile": id}`
/// for a browser role and `{"agent", "model"?, "effort"?}` for an agent role;
/// null clears it. A running program is restarted to read the new roles.
pub fn api_bind(body: &Value) -> Response {
    let Some(role) = body_str(body, "role") else {
        return Response::err(400, "Bad Request", "role is required");
    };
    let value = body.get("value").cloned().unwrap_or(Value::Null);
    let result = with_entry(body, |entry| {
        let manifest = read_manifest(&entry.dir)?;
        let spec = manifest.roles.iter().find(|r| r.name == role).ok_or(format!("{} has no role {role}", manifest.name))?;
        let ok = match (spec.kind, &value) {
            (_, Value::Null) => true,
            (RoleKind::Browser, v) => v.get("profile").and_then(Value::as_str).is_some_and(|p| !p.trim().is_empty()),
            (RoleKind::Agent, v) => v.get("agent").and_then(Value::as_str).is_some_and(|a| !a.trim().is_empty()),
        };
        if !ok {
            return Err(match spec.kind {
                RoleKind::Browser => "a browser role takes {\"profile\": \"<profile id>\"}".into(),
                RoleKind::Agent => "an agent role takes {\"agent\": \"<agent>\", \"model\": \"...\", \"effort\": \"...\"}".into(),
            });
        }
        if value.is_null() {
            entry.bindings.remove(&role);
        } else {
            entry.bindings.insert(role.clone(), value.clone());
        }
        Ok(())
    });
    match result {
        Err(response) => response,
        Ok(entry) => {
            let running = with_state(|s| s.running.contains_key(&entry.id));
            if running {
                stop(&entry.id);
                if let Err(error) = start(&entry) {
                    with_state(|s| s.errors.insert(entry.id.clone(), error));
                }
            }
            Response::ok_json(view(&entry))
        }
    }
}

/// The end of a plugin's log: `?id=...`.
pub fn api_log(query: &str) -> Response {
    let id = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == "id")
        .map(|(_, v)| v.to_string())
        .unwrap_or_default();
    if !valid_id(&id) {
        return Response::err(400, "Bad Request", "id is required");
    }
    let text = log_path(&id).and_then(|p| std::fs::read(p).ok()).unwrap_or_default();
    let text = String::from_utf8_lossy(&text);
    let lines: Vec<&str> = text.lines().collect();
    let tail = lines[lines.len().saturating_sub(300)..].join("\n");
    Response::ok_json(json!({"id": id, "log": tail}))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"{
        "id": "xtools", "name": "Xtools", "version": "1.0.0", "description": "X operations",
        "run": {"command": ["python", "{dir}/server.py", "--port", "{port}"], "health": "/api/ping", "env": {"X_DATA": "{data}"}},
        "page": "/",
        "mcp": {"command": ["python", "{dir}/mcp_server.py"], "tools": {"drafts": "read", "publish": "send"}},
        "roles": {
            "account": {"kind": "browser", "label": "X account"},
            "writer": {"kind": "agent", "label": "Writing", "default": {"agent": "claude-code", "model": "sonnet"}}
        },
        "permissions": {"domains": ["x.com"]}
    }"#;

    #[test]
    fn a_full_manifest_is_read() {
        let m = parse_manifest(FULL).unwrap();
        assert_eq!(m.id, "xtools");
        assert_eq!(m.run.as_ref().unwrap().command[1], "{dir}/server.py");
        assert_eq!(m.run.as_ref().unwrap().env, vec![("X_DATA".to_string(), "{data}".to_string())]);
        assert_eq!(m.page.as_deref(), Some("/"));
        assert_eq!(m.mcp.as_ref().unwrap().tools.len(), 2);
        assert_eq!(m.roles.len(), 2);
        let writer = m.roles.iter().find(|r| r.name == "writer").unwrap();
        assert_eq!(writer.kind, RoleKind::Agent);
        assert_eq!(writer.default["model"], "sonnet");
        assert_eq!(m.domains, vec!["x.com"]);
    }

    #[test]
    fn bad_manifests_say_what_is_wrong() {
        assert!(parse_manifest(r#"{"id": "Has Space"}"#).unwrap_err().contains("id"));
        assert!(parse_manifest(r#"{"id": "a", "page": "/"}"#).unwrap_err().contains("needs run"));
        assert!(parse_manifest(r#"{"id": "a", "run": {"command": []}}"#).unwrap_err().contains("run.command"));
        assert!(parse_manifest(r#"{"id": "a", "mcp": {"command": ["x"], "tools": {"t": "launch"}}}"#)
            .unwrap_err()
            .contains("mcp.tools.t"));
        assert!(parse_manifest(r#"{"id": "a", "roles": {"r": {"kind": "robot"}}}"#).unwrap_err().contains("roles.r.kind"));
        // The smallest plugin: tools only.
        assert!(parse_manifest(r#"{"id": "a", "mcp": {"command": ["node", "x.js"]}}"#).is_ok());
    }

    #[test]
    fn placeholders_are_filled() {
        let dir = Path::new("C:/plugins/x");
        let data = Path::new("C:/state/x");
        assert_eq!(fill("{dir}/server.py", dir, Some(4100), data), "C:/plugins/x/server.py");
        assert_eq!(fill("--port={port}", dir, Some(4100), data), "--port=4100");
        assert_eq!(fill("{data}", dir, None, data), "C:/state/x");
        assert_eq!(fill("--port={port}", dir, None, data), "--port={port}");
    }

    #[test]
    fn server_names_suit_tool_names() {
        assert_eq!(server_name("x-tools"), "plugin_x_tools");
    }

    #[test]
    fn roles_fall_back_to_their_defaults() {
        let m = parse_manifest(FULL).unwrap();
        let mut bound = Map::new();
        bound.insert("account".into(), json!({"profile": "work"}));
        let env = roles_env(&m, &bound);
        assert_eq!(env["account"]["value"]["profile"], "work");
        assert_eq!(env["writer"]["kind"], "agent");
        assert_eq!(env["writer"]["value"]["model"], "sonnet");
    }
}
