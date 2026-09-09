//! Minimal JSON-RPC 2.0 client over TCP for the Unterm MCP server.
//!
//! The wire format is line-delimited JSON-RPC 2.0; the first message must be
//! `auth.login` with the token discovered from the Core record, a live GUI
//! instance record, or the legacy `server.json` / `auth_token` compatibility
//! files.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;
use unterm_protocol::{BuildHandshake, ProcessRole};

const MCP_HOST: &str = "127.0.0.1";
const LEGACY_MCP_PORT: u16 = 19876;

const NOT_RUNNING_HINT: &str =
    "Unterm control server is not running; open Unterm.app, run 'unterm start', or start \
     unterm-core --headless";
static TARGET_INSTANCE: OnceLock<String> = OnceLock::new();

pub struct McpClient {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    next_id: u64,
}

pub fn set_target_instance(id: Option<&str>) {
    if let Some(id) = id.map(str::trim).filter(|id| !id.is_empty()) {
        let _ = TARGET_INSTANCE.set(id.to_string());
    }
}

/// Windows reports a socket read timeout as `TimedOut`, but some stacks
/// surface it as `WouldBlock`; both mean the same thing here.
fn is_timeout(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

impl McpClient {
    /// Discover the auth token + MCP port, dial the MCP server, and complete
    /// the `auth.login` handshake.
    pub fn connect() -> Result<Self> {
        Self::connect_as(ProcessRole::Cli)
    }

    /// Connect while identifying the caller's process role. The MCP bridge
    /// uses this path so the server can distinguish it from an interactive
    /// CLI during drain and future supervisor accounting.
    pub fn connect_as(process_role: ProcessRole) -> Result<Self> {
        let info = ServerEndpoint::resolve()?;
        validate_peer_identity(info.identity.as_ref())?;

        let stream = TcpStream::connect_timeout(
            &format!("{}:{}", MCP_HOST, info.port)
                .parse::<std::net::SocketAddr>()
                .expect("static addr"),
            Duration::from_secs(2),
        )
        .map_err(|_| anyhow!("{}", NOT_RUNNING_HINT))?;

        // Generous read timeout for slow ops (recording stop, screenshot,
        // etc.) -- and it must outlast the server's confirmation wait, not
        // merely equal it. A PTY write parks the server thread on a
        // confirmation banner for `mcp_confirmation_timeout_ms`; when both
        // sides ran the same 30s the client gave up in the same instant the
        // server decided, so the real verdict ("the user refused", "the
        // question timed out") was never delivered and the CLI invented a
        // connection failure instead. The margin makes the server's answer
        // always win the race.
        let confirm_wait =
            Duration::from_millis(unterm_services::settings::current().mcp_confirmation_timeout_ms);
        stream
            .set_read_timeout(Some(
                confirm_wait
                    .saturating_add(Duration::from_secs(15))
                    .max(Duration::from_secs(30)),
            ))
            .ok();
        stream.set_write_timeout(Some(Duration::from_secs(10))).ok();
        stream.set_nodelay(true).ok();

        let writer = stream
            .try_clone()
            .context("cloning MCP TCP stream for writer")?;
        let reader = BufReader::new(stream);

        let mut client = McpClient {
            reader,
            writer,
            next_id: 1,
        };

        let caller = BuildHandshake::current(
            process_role,
            std::process::id(),
            chrono::Utc::now().to_rfc3339(),
        );
        let resp = client
            .call(
                "auth.login",
                json!({ "token": info.token, "client": caller }),
            )
            .context("MCP auth.login")?;
        if resp.get("status").and_then(|v| v.as_str()) != Some("ok") {
            return Err(anyhow!("MCP auth.login rejected: {}", resp));
        }
        // Validate the process actually reached, not only the registry record
        // used to discover it. A replacement can occur between file read and
        // TCP connect. Pre-M0 servers have no `build` object and remain usable
        // when their legacy registry version already matched.
        if let Ok(live) = client.call("server.info", json!({})) {
            if let Some(build) = live.get("build") {
                if let Ok(identity) = serde_json::from_value::<BuildHandshake>(build.clone()) {
                    validate_peer_identity(Some(&identity))?;
                }
            }
        }
        Ok(client)
    }

    /// Send a JSON-RPC request and return the `result` field on success.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .map_err(|e| anyhow!("MCP write failed ({}); {}", e, NOT_RUNNING_HINT))?;
        self.writer.flush().ok();

        let mut buf = String::new();
        let n = self.reader.read_line(&mut buf).map_err(|e| {
            // A read timeout means the server took the request and never
            // answered -- the opposite situation from "nothing is
            // listening", and pointing at the wrong one sends people to
            // restart an app that is running fine. The connect above
            // already proved something is there.
            if is_timeout(&e) {
                anyhow!(
                    "MCP {} timed out waiting for a reply. Unterm is running but did not answer; \
                     if this was a command for a pane, check the Unterm window for a confirmation \
                     asking you to approve it.",
                    method
                )
            } else {
                anyhow!("MCP read failed ({}); {}", e, NOT_RUNNING_HINT)
            }
        })?;
        if n == 0 {
            return Err(anyhow!(
                "MCP server closed the connection unexpectedly; {}",
                NOT_RUNNING_HINT
            ));
        }

        let resp: Value = serde_json::from_str(buf.trim())
            .with_context(|| format!("parsing MCP response for {}: {:?}", method, buf))?;

        if let Some(err) = resp.get("error") {
            let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
            let message = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return Err(anyhow!("MCP {} failed [{}]: {}", method, code, message));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }
}

/// Where to find the MCP server. Current builds prefer the Core discovery
/// record, then live GUI instance records, then the legacy `server.json` and
/// `auth_token` files older clients and agents know how to read.
pub struct ServerEndpoint {
    pub token: String,
    pub port: u16,
    pub http_port: u16,
    pub identity: Option<BuildHandshake>,
}

#[derive(Debug, Clone, Deserialize)]
struct InstanceRecord {
    pub mcp_port: u16,
    #[serde(default)]
    pub http_port: u16,
    pub auth_token: String,
    pub pid: u32,
    #[serde(default)]
    pub started_at: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub product_version: String,
    #[serde(default)]
    pub build_commit: String,
    #[serde(default)]
    pub protocol_version: String,
    #[serde(default)]
    pub data_schema_version: u32,
    #[serde(default)]
    pub process_role: ProcessRole,
}

impl InstanceRecord {
    fn build_handshake(&self) -> Option<BuildHandshake> {
        let product_version = if self.product_version.is_empty() {
            self.version.clone()
        } else {
            self.product_version.clone()
        };
        if product_version.is_empty() {
            return None;
        }
        Some(BuildHandshake {
            product_version,
            build_commit: if self.build_commit.is_empty() {
                "unknown".into()
            } else {
                self.build_commit.clone()
            },
            protocol_version: if self.protocol_version.is_empty() {
                "legacy".into()
            } else {
                self.protocol_version.clone()
            },
            data_schema_version: self.data_schema_version,
            process_role: self.process_role,
            pid: self.pid,
            started_at: self.started_at.clone(),
        })
    }
}

fn validate_peer_identity(identity: Option<&BuildHandshake>) -> Result<()> {
    let Some(identity) = identity else {
        return Ok(());
    };
    let compatibility = identity.compatibility();
    if compatibility.is_usable() {
        return Ok(());
    }
    let code = compatibility
        .error_code()
        .unwrap_or("protocol_incompatible");
    Err(anyhow!(
        "{code}: running Unterm {} (protocol {}, schema {}) is incompatible with bridge {} (protocol {}, schema {}); drain this bridge and let the MCP client restart it from the installed unterm-cli",
        identity.product_version,
        identity.protocol_version,
        identity.data_schema_version,
        unterm_protocol::PRODUCT_VERSION,
        unterm_protocol::PROTOCOL_VERSION,
        unterm_protocol::DATA_SCHEMA_VERSION,
    ))
}

impl ServerEndpoint {
    pub fn resolve() -> Result<Self> {
        let dir = unterm_dir()?;

        if let Some(instance_id) = requested_instance_id() {
            // The Core is on `instance.list` under this name, and a name
            // you can see is a name you will type. It does not register
            // in `instances/` -- it publishes its own record -- so
            // looking for it there would fail for the one instance that
            // is reachable with no window open at all.
            if instance_id == "core" {
                return resolve_core_endpoint().ok_or_else(|| {
                    anyhow!("no Unterm Core is running; start one with `unterm-core --headless`.")
                });
            }
            let path = dir.join("instances").join(format!("{instance_id}.json"));
            let Some(info) = read_live_record(&path)? else {
                return Err(anyhow!(
                    "Unterm instance '{}' was not found or is not running. Use MCP `instance.list`, or inspect {}.",
                    instance_id,
                    dir.join("instances").display()
                ));
            };
            let identity = info.build_handshake();
            return Ok(Self {
                token: info.auth_token,
                port: info.mcp_port,
                http_port: info.http_port,
                identity,
            });
        }

        // The Core first, because that is where the sessions are and
        // where the surface now runs. It outlives every window, so an
        // agent connected to it keeps working across a GUI restart --
        // which is the whole reason the surface moved.
        if let Some(endpoint) = resolve_core_endpoint() {
            return Ok(endpoint);
        }

        // A GUI's own server is the fallback for builds where the
        // surface still lives in the window.
        if let Some(info) = resolve_live_instance(&dir)? {
            let identity = info.build_handshake();
            return Ok(Self {
                token: info.auth_token,
                port: info.mcp_port,
                http_port: info.http_port,
                identity,
            });
        }

        // Prefer server.json as the legacy fallback for older builds.
        //
        // Only when its process is still alive. A window that did not get to
        // unregister -- force quit, a crash, an installer closing it -- leaves
        // this file naming a pid that is gone, and answering from it hands
        // back that build's version. Every later command then fails the
        // identity check with `product_version_mismatch`, telling the user to
        // drain a bridge that has nothing to do with it, and nothing clears
        // the record: the CLI stays broken until somebody deletes the file by
        // hand. The other two paths have checked liveness all along; this one
        // was missed.
        let server_json = dir.join("server.json");
        if let Ok(raw) = fs::read_to_string(&server_json) {
            if let Ok(info) = serde_json::from_str::<Value>(&raw) {
                let token = info
                    .get("auth_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let port = info
                    .get("mcp_port")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(LEGACY_MCP_PORT as u64) as u16;
                let http_port = info.get("http_port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                // Only a record that names a pid can be known to be stale.
                // The builds this fallback exists for did not write one, and
                // refusing those would break the compatibility it is here to
                // provide.
                let pid = info.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let known_dead = pid != 0 && !pid_alive(pid);
                if known_dead {
                    // Take it out of the way rather than skipping it. Left
                    // behind it keeps being read -- by an older CLI on the
                    // same machine, and by anything else that trusts it.
                    let _ = fs::remove_file(&server_json);
                } else if !token.is_empty() && port != 0 {
                    return Ok(Self {
                        token,
                        port,
                        http_port,
                        identity: identity_from_value(&info),
                    });
                }
            }
        }

        // Fallback to legacy auth_token
        let token_path = dir.join("auth_token");
        if !token_path.exists() {
            return Err(anyhow!("{}", NOT_RUNNING_HINT));
        }
        let token = std::fs::read_to_string(&token_path)
            .with_context(|| format!("reading {}", token_path.display()))?
            .trim()
            .to_string();
        if token.is_empty() {
            return Err(anyhow!("{}", NOT_RUNNING_HINT));
        }
        Ok(Self {
            token,
            port: LEGACY_MCP_PORT,
            http_port: 0,
            identity: None,
        })
    }

    pub fn resolve_http() -> Result<Self> {
        let dir = unterm_dir()?;

        if let Some(instance_id) = requested_instance_id() {
            let path = dir.join("instances").join(format!("{instance_id}.json"));
            let Some(info) = read_live_record(&path)? else {
                return Err(anyhow!(
                    "Unterm instance '{}' was not found or is not running. Use MCP `instance.list`, or inspect {}.",
                    instance_id,
                    dir.join("instances").display()
                ));
            };
            return Ok(Self::from_instance_record(info));
        }

        if let Some(info) = resolve_live_instance(&dir)? {
            return Ok(Self::from_instance_record(info));
        }

        if let Some(endpoint) = resolve_legacy_http_endpoint(&dir)? {
            return Ok(endpoint);
        }

        Err(anyhow!(
            "Unterm HTTP settings server is not available; open a GUI window first"
        ))
    }

    fn from_instance_record(info: InstanceRecord) -> Self {
        let identity = info.build_handshake();
        Self {
            token: info.auth_token,
            port: info.mcp_port,
            http_port: info.http_port,
            identity,
        }
    }
}

fn resolve_legacy_http_endpoint(dir: &Path) -> Result<Option<ServerEndpoint>> {
    let server_json = dir.join("server.json");
    let raw = match fs::read_to_string(&server_json) {
        Ok(raw) => raw,
        Err(_) => return Ok(None),
    };
    let info: Value = match serde_json::from_str(&raw) {
        Ok(info) => info,
        Err(_) => return Ok(None),
    };
    let token = info
        .get("auth_token")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let mcp_port = info
        .get("mcp_port")
        .and_then(|value| value.as_u64())
        .unwrap_or(LEGACY_MCP_PORT as u64) as u16;
    let http_port = info
        .get("http_port")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u16;
    if token.is_empty() || http_port == 0 {
        return Ok(None);
    }
    Ok(Some(ServerEndpoint {
        token,
        port: mcp_port,
        http_port,
        identity: identity_from_value(&info),
    }))
}

/// The Core process's discovery record: `core.json` under
/// `%LOCALAPPDATA%\Unterm` (or `UNTERM_STATE_DIR`), written by
/// `unterm-core`. Its `mcp_port` is the agent surface that keeps
/// working with no GUI alive; the token doubles for MCP auth.
fn resolve_core_endpoint() -> Option<ServerEndpoint> {
    let dir = match std::env::var_os("UNTERM_STATE_DIR") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        Some(_) => return None,
        None => dirs_next::data_local_dir()?.join("Unterm"),
    };
    let raw = fs::read_to_string(dir.join("core.json")).ok()?;
    let info: Value = serde_json::from_str(&raw).ok()?;
    let pid = info.get("pid")?.as_u64()? as u32;
    let port = info.get("mcp_port")?.as_u64()? as u16;
    let token = info.get("token")?.as_str()?.to_string();
    if pid == 0 || !pid_alive(pid) || port == 0 || token.is_empty() {
        return None;
    }
    Some(ServerEndpoint {
        token,
        port,
        http_port: 0,
        identity: identity_from_value(&info),
    })
}

fn identity_from_value(value: &Value) -> Option<BuildHandshake> {
    let version = value
        .get("product_version")
        .or_else(|| value.get("version"))?
        .as_str()?
        .to_string();
    Some(BuildHandshake {
        product_version: version,
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
            .unwrap_or(ProcessRole::Gui),
        pid: value.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32,
        started_at: value
            .get("started_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

pub fn http_post_json(path: &str, body: Value) -> Result<Value> {
    let info = ServerEndpoint::resolve_http()?;
    if info.http_port == 0 {
        return Err(anyhow!("Unterm HTTP settings server is not available"));
    }

    let addr = format!("{}:{}", MCP_HOST, info.http_port)
        .parse::<std::net::SocketAddr>()
        .expect("static addr");
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|e| {
        anyhow!(
            "HTTP settings server unavailable ({}); {}",
            e,
            NOT_RUNNING_HINT
        )
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(10))).ok();
    stream.set_nodelay(true).ok();

    let body = serde_json::to_vec(&body)?;
    let request = format!(
        "POST {} HTTP/1.1\r\nHost: {}:{}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        path,
        MCP_HOST,
        info.http_port,
        info.token,
        body.len()
    );
    stream.write_all(request.as_bytes())?;
    stream.write_all(&body)?;
    stream.flush().ok();

    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("malformed HTTP response from Unterm settings server"))?;
    let header = String::from_utf8_lossy(&response[..split]);
    let body = &response[split + 4..];
    let status = header
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| anyhow!("malformed HTTP status from Unterm settings server"))?;

    let value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(body).unwrap_or(Value::Null)
    };
    if !(200..300).contains(&status) {
        let message = value
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("HTTP settings request failed");
        return Err(anyhow!("{}: {}", status, message));
    }
    Ok(value)
}

/// The registry record for one named instance, as JSON.
///
/// `instance.info` asks the *server* who it is, and since 0.68 every window
/// of every front end answers through the Core's one MCP port -- so with two
/// front ends up, asking about `bravo` reached the same server as asking
/// about `alpha`, and it answered for whichever one it considers current.
/// The record is what tells them apart: it is per front end, it is what
/// `instance.list` reads, and the endpoint was resolved from it a moment ago.
pub fn instance_record(instance_id: &str) -> Option<Value> {
    let dir = unterm_dir().ok()?;
    let raw = fs::read_to_string(dir.join("instances").join(format!("{instance_id}.json"))).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let pid = value.get("pid").and_then(|pid| pid.as_u64())? as u32;
    // Zero is not a process to ask about: on unix `kill(0, 0)` addresses the
    // caller's own process group and answers yes, so a record left behind
    // with no pid would read as live. Guarded the same way `read_live_record`
    // guards it, and for the same reason.
    (pid != 0 && pid_alive(pid)).then_some(value)
}

/// Which instance the caller named, if it named one.
pub fn target_instance() -> Option<String> {
    requested_instance_id()
}

/// Resolve the instance id whose GUI pid matches — used by `agent
/// signal` to route hook events to the instance that owns the calling
/// pane. Hooks inherit `WEZTERM_UNIX_SOCKET=…/gui-sock-<pid>` from the
/// pane's shell, and pid is the one instance-unique key in that env.
pub fn instance_for_pid(pid: u32) -> Option<String> {
    let dir = unterm_dir().ok()?.join("instances");
    for entry in fs::read_dir(&dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e != "json").unwrap_or(true) {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        if v.get("pid").and_then(|p| p.as_u64()) == Some(pid as u64) {
            if let Some(id) = v.get("id").and_then(|i| i.as_str()) {
                return Some(id.to_string());
            }
        }
    }
    None
}

fn requested_instance_id() -> Option<String> {
    TARGET_INSTANCE
        .get()
        .cloned()
        .or_else(|| std::env::var("UNTERM_INSTANCE").ok())
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

fn resolve_live_instance(dir: &PathBuf) -> Result<Option<InstanceRecord>> {
    if let Some(info) = read_live_record(&dir.join("active.json"))? {
        return Ok(Some(info));
    }

    let instances_dir = dir.join("instances");
    let mut live = Vec::new();
    let entries = match fs::read_dir(&instances_dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(None),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|ext| ext != "json").unwrap_or(true) {
            continue;
        }
        if let Some(info) = read_live_record(&path)? {
            live.push(info);
        }
    }

    live.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    Ok(live.into_iter().next())
}

fn read_live_record(path: &PathBuf) -> Result<Option<InstanceRecord>> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(_) => return Ok(None),
    };
    let info: InstanceRecord = match serde_json::from_str(&raw) {
        Ok(info) => info,
        Err(_) => return Ok(None),
    };
    if info.pid == 0 || !pid_alive(info.pid) || info.mcp_port == 0 || info.auth_token.is_empty() {
        return Ok(None);
    }
    Ok(Some(info))
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    !matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(e) if e == libc::ESRCH
    )
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    unsafe {
        use winapi::shared::minwindef::FALSE;
        use winapi::um::handleapi::CloseHandle;
        use winapi::um::processthreadsapi::{GetExitCodeProcess, OpenProcess};
        use winapi::um::winnt::PROCESS_QUERY_LIMITED_INFORMATION;
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
        if h.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        ok && code == 259
    }
}

fn unterm_dir() -> Result<PathBuf> {
    unterm_protocol::state_dir().ok_or_else(|| anyhow!("could not resolve home directory"))
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap()
    }

    struct StateDirGuard {
        previous: Option<std::ffi::OsString>,
    }

    impl StateDirGuard {
        fn set(path: &std::path::Path) -> Self {
            let previous = std::env::var_os("UNTERM_STATE_DIR");
            std::env::set_var("UNTERM_STATE_DIR", path);
            Self { previous }
        }
    }

    impl Drop for StateDirGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("UNTERM_STATE_DIR", value),
                None => std::env::remove_var("UNTERM_STATE_DIR"),
            }
        }
    }

    #[test]
    fn same_version_legacy_instance_is_allowed() {
        let record: InstanceRecord = serde_json::from_value(json!({
            "mcp_port": 19876,
            "http_port": 19877,
            "auth_token": "secret",
            "pid": 42,
            "started_at": "now",
            "version": unterm_protocol::PRODUCT_VERSION,
        }))
        .unwrap();
        let identity = record.build_handshake().unwrap();
        assert_eq!(
            identity.compatibility(),
            unterm_protocol::Compatibility::Legacy
        );
        validate_peer_identity(Some(&identity)).unwrap();
    }

    #[test]
    fn http_endpoint_keeps_the_live_gui_port() {
        let record: InstanceRecord = serde_json::from_value(json!({
            "mcp_port": 19876,
            "http_port": 19877,
            "auth_token": "secret",
            "pid": 42,
            "started_at": "now",
            "product_version": unterm_protocol::PRODUCT_VERSION,
            "build_commit": "abc123",
            "protocol_version": unterm_protocol::PROTOCOL_VERSION,
            "data_schema_version": unterm_protocol::DATA_SCHEMA_VERSION,
        }))
        .unwrap();
        let endpoint = ServerEndpoint::from_instance_record(record);

        assert_eq!(endpoint.port, 19876);
        assert_eq!(endpoint.http_port, 19877);
        assert_eq!(endpoint.token, "secret");
        assert_eq!(
            endpoint
                .identity
                .as_ref()
                .map(|identity| identity.process_role),
            Some(ProcessRole::Gui)
        );
    }

    #[test]
    fn endpoint_prefers_core_discovery_over_live_gui_registry() {
        let _lock = env_lock();
        let root = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::set(root.path());
        fs::create_dir_all(root.path().join("instances")).unwrap();
        fs::write(
            root.path().join("core.json"),
            serde_json::to_string(&json!({
                "mcp_port": 25001,
                "token": "core-token",
                "pid": std::process::id(),
                "product_version": unterm_protocol::PRODUCT_VERSION,
                "build_commit": "core-build",
                "protocol_version": unterm_protocol::PROTOCOL_VERSION,
                "data_schema_version": unterm_protocol::DATA_SCHEMA_VERSION,
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            root.path().join("active.json"),
            serde_json::to_string(&json!({
                "id": "alpha",
                "mcp_port": 26001,
                "http_port": 26002,
                "auth_token": "gui-token",
                "pid": std::process::id(),
                "started_at": "2026-08-12T00:00:00+08:00",
            }))
            .unwrap(),
        )
        .unwrap();

        let endpoint = ServerEndpoint::resolve().unwrap();

        assert_eq!(endpoint.port, 25001);
        assert_eq!(endpoint.token, "core-token");
    }

    /// `instance.list` names the Core `core`, so `--instance core` has to
    /// reach it. It publishes its own record instead of registering in
    /// `instances/`, so the by-name path would otherwise fail for the one
    /// instance that is reachable with no window open.

    /// Two front ends, one MCP port, and a question about one of them.
    ///
    /// Since 0.68 every window serves through the Core's port, so two front
    /// ends register the same `mcp_port` and differ only by pid and their own
    /// HTTP port. Asking the server which instance it is therefore answers
    /// for whichever it considers current, whoever was asked about -- which
    /// is why the record, not the server, is what tells them apart.
    #[test]
    fn a_named_instance_is_read_from_its_own_record() {
        let _lock = env_lock();
        let root = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::set(root.path());
        let instances = root.path().join("instances");
        fs::create_dir_all(&instances).unwrap();
        for (id, http) in [("alpha", 19877u16), ("bravo", 19878)] {
            fs::write(
                instances.join(format!("{id}.json")),
                serde_json::to_string(&json!({
                    "id": id,
                    "pid": std::process::id(),
                    // The same port on purpose: that is the shape that made
                    // the server unable to tell the two apart.
                    "mcp_port": 62144,
                    "http_port": http,
                    "auth_token": format!("{id}-token"),
                    "product_version": unterm_protocol::PRODUCT_VERSION,
                    "protocol_version": unterm_protocol::PROTOCOL_VERSION,
                    "data_schema_version": unterm_protocol::DATA_SCHEMA_VERSION,
                    "process_role": "gui",
                }))
                .unwrap(),
            )
            .unwrap();
        }

        for (id, http) in [("alpha", 19877u64), ("bravo", 19878)] {
            std::env::set_var("UNTERM_INSTANCE", id);
            let record = instance_record(id).expect("the record is there");
            std::env::remove_var("UNTERM_INSTANCE");
            assert_eq!(record.get("id").and_then(|v| v.as_str()), Some(id));
            assert_eq!(record.get("http_port").and_then(|v| v.as_u64()), Some(http));
        }

        // A name nothing answers to is nothing, not the nearest match.
        assert!(instance_record("charlie").is_none());
    }

    /// A record whose process has gone is not an answer.
    #[test]
    fn a_dead_instance_has_no_record_to_report() {
        let _lock = env_lock();
        let root = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::set(root.path());
        let instances = root.path().join("instances");
        fs::create_dir_all(&instances).unwrap();
        fs::write(
            instances.join("ghost.json"),
            serde_json::to_string(&json!({
                "id": "ghost",
                // No process has pid 0, which is how every other reader here
                // spells "this record outlived its front end".
                "pid": 0,
                "mcp_port": 62144,
                "http_port": 19999,
                "auth_token": "ghost-token",
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(instance_record("ghost").is_none());
    }

    #[test]
    fn the_instance_named_core_resolves_to_the_core() {
        let _lock = env_lock();
        let root = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::set(root.path());
        fs::create_dir_all(root.path().join("instances")).unwrap();
        fs::write(
            root.path().join("core.json"),
            serde_json::to_string(&json!({
                "mcp_port": 25007,
                "token": "core-token",
                "pid": std::process::id(),
                "product_version": unterm_protocol::PRODUCT_VERSION,
                "build_commit": "core-build",
                "protocol_version": unterm_protocol::PROTOCOL_VERSION,
                "data_schema_version": unterm_protocol::DATA_SCHEMA_VERSION,
                "process_role": "core",
            }))
            .unwrap(),
        )
        .unwrap();

        std::env::set_var("UNTERM_INSTANCE", "core");
        let resolved = ServerEndpoint::resolve();
        std::env::remove_var("UNTERM_INSTANCE");

        let endpoint = resolved.expect("--instance core must resolve");
        assert_eq!(endpoint.port, 25007);
        assert_eq!(endpoint.token, "core-token");
    }

    /// And says so plainly when there is none, rather than reporting a
    /// missing instance file for something that never had one.
    #[test]
    fn the_instance_named_core_says_when_no_core_is_running() {
        let _lock = env_lock();
        let root = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::set(root.path());
        fs::create_dir_all(root.path().join("instances")).unwrap();

        std::env::set_var("UNTERM_INSTANCE", "core");
        let resolved = ServerEndpoint::resolve();
        std::env::remove_var("UNTERM_INSTANCE");

        let error = match resolved {
            Ok(_) => panic!("no core is running"),
            Err(err) => err.to_string(),
        };
        assert!(error.contains("Core"), "{error}");
    }

    #[test]
    fn http_endpoint_does_not_use_core_discovery_without_a_gui() {
        let _lock = env_lock();
        let root = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::set(root.path());
        fs::write(
            root.path().join("core.json"),
            serde_json::to_string(&json!({
                "mcp_port": 25001,
                "token": "core-token",
                "pid": std::process::id(),
                "product_version": unterm_protocol::PRODUCT_VERSION,
                "build_commit": "core-build",
                "protocol_version": unterm_protocol::PROTOCOL_VERSION,
                "data_schema_version": unterm_protocol::DATA_SCHEMA_VERSION,
                "process_role": "core",
            }))
            .unwrap(),
        )
        .unwrap();

        let error = match ServerEndpoint::resolve_http() {
            Ok(_) => panic!("core-only discovery must not resolve an HTTP endpoint"),
            Err(err) => err.to_string(),
        };

        assert!(
            error.contains("HTTP settings server is not available"),
            "{error}"
        );
    }

    #[test]
    fn endpoint_uses_live_active_pointer_when_core_is_absent() {
        let _lock = env_lock();
        let root = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::set(root.path());
        fs::write(
            root.path().join("active.json"),
            serde_json::to_string(&json!({
                "id": "alpha",
                "mcp_port": 26001,
                "http_port": 26002,
                "auth_token": "gui-token",
                "pid": std::process::id(),
                "started_at": "2026-08-12T00:00:00+08:00",
                "product_version": unterm_protocol::PRODUCT_VERSION,
            }))
            .unwrap(),
        )
        .unwrap();

        let endpoint = ServerEndpoint::resolve().unwrap();

        assert_eq!(endpoint.port, 26001);
        assert_eq!(endpoint.http_port, 26002);
        assert_eq!(endpoint.token, "gui-token");
    }

    #[test]
    fn endpoint_uses_legacy_server_json_before_auth_token() {
        let _lock = env_lock();
        let root = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::set(root.path());
        fs::write(root.path().join("auth_token"), "old-token").unwrap();
        fs::write(
            root.path().join("server.json"),
            serde_json::to_string(&json!({
                "mcp_port": 27001,
                "http_port": 27002,
                "auth_token": "server-token",
                "version": unterm_protocol::PRODUCT_VERSION,
            }))
            .unwrap(),
        )
        .unwrap();

        let endpoint = ServerEndpoint::resolve().unwrap();

        assert_eq!(endpoint.port, 27001);
        assert_eq!(endpoint.http_port, 27002);
        assert_eq!(endpoint.token, "server-token");
        assert_eq!(
            endpoint
                .identity
                .as_ref()
                .map(|identity| identity.protocol_version.as_str()),
            Some("legacy")
        );
    }

    #[test]
    fn http_endpoint_uses_legacy_server_json_without_core_or_instances() {
        let _lock = env_lock();
        let root = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::set(root.path());
        fs::write(
            root.path().join("server.json"),
            serde_json::to_string(&json!({
                "mcp_port": 27001,
                "http_port": 27002,
                "auth_token": "server-token",
                "version": unterm_protocol::PRODUCT_VERSION,
            }))
            .unwrap(),
        )
        .unwrap();

        let endpoint = ServerEndpoint::resolve_http().unwrap();

        assert_eq!(endpoint.port, 27001);
        assert_eq!(endpoint.http_port, 27002);
        assert_eq!(endpoint.token, "server-token");
    }

    #[test]
    fn endpoint_uses_legacy_auth_token_last() {
        let _lock = env_lock();
        let root = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::set(root.path());
        fs::write(root.path().join("auth_token"), "old-token\n").unwrap();

        let endpoint = ServerEndpoint::resolve().unwrap();

        assert_eq!(endpoint.port, LEGACY_MCP_PORT);
        assert_eq!(endpoint.http_port, 0);
        assert_eq!(endpoint.token, "old-token");
        assert!(endpoint.identity.is_none());
    }

    /// A window that never got to unregister must not lock the CLI out.
    ///
    /// `server.json` naming a dead pid used to be answered from anyway. Its
    /// version then failed the identity check, so every command reported
    /// `product_version_mismatch` and told the user to drain a bridge that
    /// was not involved — with no way back except deleting the file by hand.
    #[test]
    fn a_server_record_whose_process_is_gone_is_not_believed() {
        let _lock = env_lock();
        let root = tempfile::tempdir().unwrap();
        let _guard = StateDirGuard::set(root.path());
        // A pid that certainly no longer exists: run something trivial and
        // wait for it to finish. Inventing a number could collide with a
        // live process and make this test pass for the wrong reason.
        let mut spawned = std::process::Command::new(if cfg!(windows) { "cmd" } else { "sh" })
            .args(if cfg!(windows) { ["/C", "exit"] } else { ["-c", "exit"] })
            .spawn()
            .expect("a process to spawn");
        let dead_pid = spawned.id();
        spawned.wait().expect("it to exit");

        let server_json = root.path().join("server.json");
        fs::write(
            &server_json,
            serde_json::to_string(&serde_json::json!({
                "auth_token": "dead-token",
                "mcp_port": 27001,
                "http_port": 27002,
                "pid": dead_pid,
                "product_version": "0.57.4",
                "protocol_version": "1.0.0",
                "data_schema_version": 1,
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(root.path().join("auth_token"), "old-token
").unwrap();

        let endpoint = ServerEndpoint::resolve().unwrap();

        assert_eq!(
            endpoint.token, "old-token",
            "a dead record must not be answered from"
        );
        assert!(
            !server_json.exists(),
            "a dead record must not be left for the next reader"
        );
    }

    #[test]
    fn stale_product_version_has_a_machine_readable_error() {
        let mut identity = BuildHandshake::current(ProcessRole::Gui, 42, "now");
        identity.product_version = "0.57.4".into();
        let error = validate_peer_identity(Some(&identity))
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("product_version_mismatch:"), "{error}");
        assert!(error.contains("drain this bridge"), "{error}");
    }

    #[test]
    fn newer_schema_is_rejected_before_connecting() {
        let mut identity = BuildHandshake::current(ProcessRole::Gui, 42, "now");
        identity.data_schema_version = unterm_protocol::DATA_SCHEMA_VERSION + 1;
        let error = validate_peer_identity(Some(&identity))
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("data_schema_incompatible:"), "{error}");
    }
}
