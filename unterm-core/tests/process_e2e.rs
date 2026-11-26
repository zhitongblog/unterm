//! End-to-end tests against the real `unterm-core` binary.
//!
//! Everything in `src/lib.rs`'s test module exercises the server
//! in-process; these spawn the actual executable, so they cover what
//! those cannot: discovery publication, the cross-process instance
//! lock, and shutdown leaving no state behind. `UNTERM_STATE_DIR`
//! points every child at a scratch directory — the user's live Core,
//! if any, must never be touched by a test run.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, serde::Deserialize)]
struct Discovery {
    endpoint: String,
    token: String,
    pid: u32,
    product_version: String,
    build_commit: String,
    protocol_version: String,
    data_schema_version: u32,
    process_role: unterm_protocol::ProcessRole,
    started_at: String,
    #[serde(default)]
    mcp_port: Option<u16>,
}

fn scratch_state_dir(label: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("unterm-core-e2e-{label}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Kills the child when dropped, so a panicking assertion cannot leave
/// an orphan core holding the executable's file lock — which would
/// break every rebuild after a failed run.
struct CoreGuard(Child);

impl Drop for CoreGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl std::ops::Deref for CoreGuard {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for CoreGuard {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

fn spawn_core(state_dir: &std::path::Path) -> CoreGuard {
    // HOME/USERPROFILE point into the scratch dir too: the Core reads
    // the user's config (~/.unterm/unterm.conf) for write policy, and
    // a test must see the config it wrote, never the real user's.
    Command::new(env!("CARGO_BIN_EXE_unterm-core"))
        .env("UNTERM_STATE_DIR", state_dir)
        .env("USERPROFILE", state_dir)
        .env("HOME", state_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(CoreGuard)
        .expect("spawn unterm-core binary")
}

fn read_discovery(state_dir: &std::path::Path) -> Option<Discovery> {
    let raw = std::fs::read(state_dir.join("core.json")).ok()?;
    serde_json::from_slice(&raw).ok()
}

fn wait_for_discovery(state_dir: &std::path::Path, timeout: Duration) -> Discovery {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(discovery) = read_discovery(state_dir) {
            return discovery;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("unterm-core never published discovery in {state_dir:?}");
}

fn request(
    stream: &mut TcpStream,
    token: &str,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let frame = serde_json::json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "method": method,
        "token": token,
        "params": params,
    });
    writeln!(stream, "{frame}").unwrap();
    stream.flush().unwrap();
    let mut line = String::new();
    BufReader::new(stream.try_clone().unwrap())
        .read_line(&mut line)
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => return false,
        }
    }
    false
}

#[test]
fn version_probe_bypasses_lock_and_state() {
    let warm_home = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_unterm-core"))
        .arg("--version")
        .env("UNTERM_STATE_DIR", warm_home.path())
        .env("USERPROFILE", warm_home.path())
        .env("HOME", warm_home.path())
        .output()
        .expect("warm up unterm-core --version");

    let state_dir = scratch_state_dir("version");
    let mut child = spawn_core(&state_dir);
    let discovery = wait_for_discovery(&state_dir, Duration::from_secs(10));

    let probe_state = scratch_state_dir("version-probe");
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_unterm-core"))
        .arg("--version")
        .env("UNTERM_STATE_DIR", &probe_state)
        .env("USERPROFILE", &probe_state)
        .env("HOME", &probe_state)
        .output()
        .expect("run unterm-core --version");

    assert!(output.status.success());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("unterm-core {}", unterm_protocol::PRODUCT_VERSION)
    );
    assert!(
        read_discovery(&probe_state).is_none(),
        "version probe must not publish discovery"
    );
    assert_eq!(
        read_discovery(&state_dir)
            .expect("running core discovery vanished")
            .pid,
        discovery.pid,
        "version probe must not disturb the running core"
    );

    let mut stream = TcpStream::connect(&discovery.endpoint).unwrap();
    let _ = request(
        &mut stream,
        &discovery.token,
        "core.shutdown",
        serde_json::Value::Null,
    );
    assert!(wait_for_exit(&mut child, Duration::from_secs(10)));
    let _ = std::fs::remove_dir_all(&state_dir);
    let _ = std::fs::remove_dir_all(&probe_state);
}

#[test]
fn real_process_serves_sessions_and_cleans_up() {
    let state_dir = scratch_state_dir("single");
    let mut child = spawn_core(&state_dir);

    let discovery = wait_for_discovery(&state_dir, Duration::from_secs(10));
    assert_eq!(discovery.pid, child.id());
    assert!(!discovery.product_version.is_empty());
    assert!(!discovery.build_commit.is_empty());
    assert_eq!(discovery.protocol_version, unterm_protocol::PROTOCOL_VERSION);
    assert_eq!(
        discovery.data_schema_version,
        unterm_protocol::DATA_SCHEMA_VERSION
    );
    assert_eq!(discovery.process_role, unterm_protocol::ProcessRole::Core);
    assert!(!discovery.started_at.is_empty());

    let mut stream = TcpStream::connect(&discovery.endpoint).unwrap();
    stream.set_nodelay(true).unwrap();

    let info = request(
        &mut stream,
        &discovery.token,
        "core.info",
        serde_json::Value::Null,
    );
    assert_eq!(info["ok"], true, "core.info failed: {info}");
    assert_eq!(info["result"]["process_role"], "core");

    let argv = if cfg!(windows) { ["cmd.exe"] } else { ["sh"] };
    let created = request(
        &mut stream,
        &discovery.token,
        "session.create",
        serde_json::json!({"cols": 80, "rows": 24, "argv": argv}),
    );
    assert_eq!(created["ok"], true, "session.create failed: {created}");
    let pane_id = created["result"]["id"].as_u64().unwrap();

    let closed = request(
        &mut stream,
        &discovery.token,
        "session.close",
        serde_json::json!({"pane_id": pane_id}),
    );
    assert_eq!(closed["ok"], true);

    let _ = request(
        &mut stream,
        &discovery.token,
        "core.shutdown",
        serde_json::Value::Null,
    );
    assert!(
        wait_for_exit(&mut child, Duration::from_secs(10)),
        "core did not exit after core.shutdown"
    );
    assert!(
        read_discovery(&state_dir).is_none(),
        "shutdown must clear the discovery record"
    );

    let _ = std::fs::remove_dir_all(&state_dir);
}

#[test]
fn health_and_readiness_split_liveness_from_accepting_work() {
    let state_dir = scratch_state_dir("readiness");
    let mut child = spawn_core(&state_dir);
    let discovery = wait_for_discovery(&state_dir, Duration::from_secs(10));

    let mut stream = TcpStream::connect(&discovery.endpoint).unwrap();
    stream.set_nodelay(true).unwrap();

    let health = request(
        &mut stream,
        &discovery.token,
        "core.health",
        serde_json::Value::Null,
    );
    assert_eq!(health["ok"], true, "core.health failed: {health}");
    assert_eq!(health["result"]["alive"], true);
    assert_eq!(health["result"]["ready"], true);
    assert_eq!(health["result"]["accepting_sessions"], true);
    assert_eq!(health["result"]["active_session_count"], 0);
    assert_eq!(health["result"]["drained"], false);

    let readiness = request(
        &mut stream,
        &discovery.token,
        "core.readiness",
        serde_json::Value::Null,
    );
    assert_eq!(
        readiness["result"]["status"], "ready",
        "core.readiness before drain: {readiness}"
    );
    assert_eq!(readiness["result"]["ready"], true);
    assert_eq!(readiness["result"]["accepting_sessions"], true);
    assert_eq!(readiness["result"]["active_session_count"], 0);
    assert_eq!(readiness["result"]["drained"], false);

    let existing = request(
        &mut stream,
        &discovery.token,
        "session.create",
        serde_json::json!({"cols": 80, "rows": 24}),
    );
    assert_eq!(existing["ok"], true, "session.create failed: {existing}");
    let pane_id = existing["result"]["id"]
        .as_u64()
        .expect("created session has id");

    let drain = request(
        &mut stream,
        &discovery.token,
        "core.drain",
        serde_json::json!({"exit_when_idle": false}),
    );
    assert_eq!(drain["ok"], true, "core.drain failed: {drain}");
    assert_eq!(drain["result"]["active_session_count"], 1);
    assert_eq!(drain["result"]["drained"], false);

    let health = request(
        &mut stream,
        &discovery.token,
        "core.health",
        serde_json::Value::Null,
    );
    assert_eq!(health["result"]["alive"], true);
    assert_eq!(health["result"]["status"], "draining");
    assert_eq!(health["result"]["ready"], false);
    assert_eq!(health["result"]["accepting_sessions"], false);
    assert_eq!(health["result"]["active_session_count"], 1);
    assert_eq!(health["result"]["drained"], false);

    let readiness = request(
        &mut stream,
        &discovery.token,
        "core.readiness",
        serde_json::Value::Null,
    );
    assert_eq!(readiness["result"]["status"], "not_ready");
    assert_eq!(readiness["result"]["ready"], false);
    assert_eq!(readiness["result"]["accepting_sessions"], false);
    assert_eq!(readiness["result"]["reason"], "draining");
    assert_eq!(readiness["result"]["active_session_count"], 1);
    assert_eq!(readiness["result"]["drained"], false);

    let closed = request(
        &mut stream,
        &discovery.token,
        "session.close",
        serde_json::json!({"pane_id": pane_id}),
    );
    assert_eq!(closed["ok"], true, "session.close failed: {closed}");

    let health = request(
        &mut stream,
        &discovery.token,
        "core.health",
        serde_json::Value::Null,
    );
    assert_eq!(health["result"]["status"], "draining");
    assert_eq!(health["result"]["active_session_count"], 0);
    assert_eq!(health["result"]["drained"], true);

    let created = request(
        &mut stream,
        &discovery.token,
        "session.create",
        serde_json::json!({"cols": 80, "rows": 24}),
    );
    assert_eq!(
        created["error"]["code"], "draining",
        "readiness false must match session.create refusal: {created}"
    );

    let _ = request(
        &mut stream,
        &discovery.token,
        "core.shutdown",
        serde_json::Value::Null,
    );
    assert!(wait_for_exit(&mut child, Duration::from_secs(10)));
    let _ = std::fs::remove_dir_all(&state_dir);
}

/// The M1 gate, as an automated test: no GUI anywhere, and the MCP
/// surface still creates a PTY, reads its screen, and refuses an
/// unauthorized write immediately instead of hanging on a banner no
/// window will ever paint.
#[test]
fn headless_mcp_serves_sessions_without_any_gui() {
    let state_dir = scratch_state_dir("mcp");
    let mut child = spawn_core(&state_dir);
    let discovery = wait_for_discovery(&state_dir, Duration::from_secs(10));
    let mcp_port = discovery.mcp_port.expect("core must publish an MCP port");

    let mut stream = TcpStream::connect(("127.0.0.1", mcp_port)).unwrap();
    stream.set_nodelay(true).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut mcp = |method: &str, params: serde_json::Value| -> serde_json::Value {
        let frame =
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        writeln!(stream, "{frame}").unwrap();
        stream.flush().unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };

    let auth = mcp("auth.login", serde_json::json!({"token": discovery.token}));
    assert_eq!(auth["result"]["status"], "ok", "auth failed: {auth}");

    let info = mcp("server.info", serde_json::json!({}));
    assert_eq!(info["result"]["build"]["process_role"], "core");
    assert_eq!(
        info["result"]["build"]["protocol_version"],
        unterm_protocol::PROTOCOL_VERSION
    );
    assert!(
        info["result"]["build"]["started_at"]
            .as_str()
            .is_some_and(|value| !value.is_empty()),
        "headless MCP server.info must include core started_at: {info}"
    );

    let created = mcp(
        "session.create",
        serde_json::json!({"cols": 80, "rows": 24}),
    );
    let pane_id = created["result"]["id"]
        .as_u64()
        .unwrap_or_else(|| panic!("session.create failed headless: {created}"));

    let listed = mcp("session.list", serde_json::json!({}));
    assert!(
        serde_json::to_string(&listed["result"])
            .unwrap()
            .contains(&pane_id.to_string()),
        "created pane missing from list: {listed}"
    );

    // Under default Balanced autonomy, a write above the autonomy ceiling
    // (such as a destructive command) must fail closed *now* — a hang here
    // would mean the gate parked on a confirmation banner that no window
    // exists to answer.
    let started = Instant::now();
    let denied = mcp(
        "exec.run",
        serde_json::json!({"id": pane_id, "command": "git push --force"}),
    );
    assert!(
        denied.get("error").is_some(),
        "headless write above autonomy ceiling must be denied: {denied}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "headless denial must be immediate, took {:?}",
        started.elapsed()
    );

    let closed = mcp("session.destroy", serde_json::json!({"id": pane_id}));
    assert!(
        closed.get("error").is_none(),
        "session.destroy failed: {closed}"
    );

    // Shut the core down over its own IPC.
    let mut core = TcpStream::connect(&discovery.endpoint).unwrap();
    let frame = serde_json::json!({"id": "x", "method": "core.shutdown", "token": discovery.token, "params": null});
    writeln!(core, "{frame}").unwrap();
    core.flush().unwrap();
    assert!(
        wait_for_exit(&mut child, Duration::from_secs(10)),
        "core did not exit after shutdown"
    );
    let _ = std::fs::remove_dir_all(&state_dir);
}

/// The authorized half of the headless story: with the user's config
/// saying `input_confirmation = "never"`, an agent's write goes
/// through with no GUI anywhere — proof the Core reads and enforces
/// the same config file the GUI does.
#[test]
fn headless_write_is_allowed_when_the_config_says_never() {
    let state_dir = scratch_state_dir("mcp-allow");
    // UNTERM_STATE_DIR replaces ~/.unterm wholesale; the config file
    // lives at its root, exactly where the Core will look.
    std::fs::write(
        state_dir.join("unterm.conf"),
        "[mcp]\ninput_confirmation = \"never\"\n",
    )
    .unwrap();

    let mut child = spawn_core(&state_dir);
    let discovery = wait_for_discovery(&state_dir, Duration::from_secs(10));
    let mcp_port = discovery.mcp_port.expect("core must publish an MCP port");

    let mut stream = TcpStream::connect(("127.0.0.1", mcp_port)).unwrap();
    stream.set_nodelay(true).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut mcp = |method: &str, params: serde_json::Value| -> serde_json::Value {
        let frame =
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        writeln!(stream, "{frame}").unwrap();
        stream.flush().unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };

    let auth = mcp("auth.login", serde_json::json!({"token": discovery.token}));
    assert_eq!(auth["result"]["status"], "ok");
    let created = mcp(
        "session.create",
        serde_json::json!({"cols": 80, "rows": 24}),
    );
    let pane_id = created["result"]["id"].as_u64().unwrap();

    let sent = mcp(
        "exec.run",
        serde_json::json!({"id": pane_id, "command": "echo headless-authorized"}),
    );
    assert_eq!(
        sent["result"]["sent"], true,
        "configured-never write must pass headless: {sent}"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = mcp("screen.text", serde_json::json!({"id": pane_id}));
        let body = text["result"]["lines"]
            .as_array()
            .map(|lines| {
                lines
                    .iter()
                    .filter_map(|line| line.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if body.contains("headless-authorized") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "echoed output never reached the screen: {text}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let _ = mcp("session.destroy", serde_json::json!({"id": pane_id}));
    let mut core = TcpStream::connect(&discovery.endpoint).unwrap();
    let frame = serde_json::json!({"id": "x", "method": "core.shutdown", "token": discovery.token, "params": null});
    writeln!(core, "{frame}").unwrap();
    core.flush().unwrap();
    assert!(wait_for_exit(&mut child, Duration::from_secs(10)));
    let _ = std::fs::remove_dir_all(&state_dir);
}

#[test]
fn concurrent_launches_yield_exactly_one_core() {
    // 20 is the M1 gate's number: "20 concurrent clients must not
    // produce a duplicate Core or fight over ports".
    let state_dir = scratch_state_dir("race");
    let mut children: Vec<CoreGuard> = (0..20).map(|_| spawn_core(&state_dir)).collect();

    let discovery = wait_for_discovery(&state_dir, Duration::from_secs(10));
    let winner_pid = discovery.pid;
    assert!(
        children.iter().any(|child| child.id() == winner_pid),
        "discovery pid {winner_pid} is not one of the spawned children"
    );

    // Losers exit on their own; give the whole field time to settle,
    // then insist the winner is the only survivor.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut alive: Vec<u32> = Vec::new();
        for child in children.iter_mut() {
            if matches!(child.try_wait(), Ok(None)) {
                alive.push(child.id());
            }
        }
        if alive == vec![winner_pid] {
            break;
        }
        if Instant::now() > deadline {
            panic!("expected only winner {winner_pid} alive, still alive: {alive:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // The record must still be the winner's after every loser exited:
    // a loser must never overwrite discovery on its way out.
    let discovery = read_discovery(&state_dir).expect("discovery vanished");
    assert_eq!(discovery.pid, winner_pid);

    let mut stream = TcpStream::connect(&discovery.endpoint).unwrap();
    let health = request(
        &mut stream,
        &discovery.token,
        "core.health",
        serde_json::Value::Null,
    );
    assert_eq!(health["result"]["status"], "ready");

    let _ = request(
        &mut stream,
        &discovery.token,
        "core.shutdown",
        serde_json::Value::Null,
    );
    for child in &mut children {
        if child.id() == winner_pid {
            assert!(
                wait_for_exit(child, Duration::from_secs(10)),
                "winner did not exit after shutdown"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&state_dir);
}

/// Restores `UNTERM_STATE_DIR` on drop.
///
/// The reconnect path goes through `ensure_running()`, which reads the
/// discovery record from wherever the *test process* says state lives --
/// so unlike every test above, this one has to set it in-process. That
/// is a process-global mutation, hence the guard.
struct StateDirEnv {
    previous: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl StateDirEnv {
    fn set(dir: &std::path::Path) -> Self {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("UNTERM_STATE_DIR");
        std::env::set_var("UNTERM_STATE_DIR", dir);
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for StateDirEnv {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(previous) => std::env::set_var("UNTERM_STATE_DIR", previous),
            None => std::env::remove_var("UNTERM_STATE_DIR"),
        }
    }
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

#[test]
fn a_killed_core_is_replaced_without_restarting_the_client() {
    // The failure this covers: before reconnection existed, a Core that
    // died left the window permanently dead -- the frame cache's worker
    // exited and the only way back was restarting Unterm. Recovery has
    // to work while the client keeps running.
    let state_dir = scratch_state_dir("reconnect");
    let _env = StateDirEnv::set(&state_dir);

    let mut first = spawn_core(&state_dir);
    let discovery = wait_for_discovery(&state_dir, Duration::from_secs(10));
    let first_pid = discovery.pid;

    let cache = unterm_core::FrameCache::start(discovery.endpoint.clone(), discovery.token.clone())
        .expect("frame cache should attach to the first core");

    // A pane on the first Core, so there is something to lose.
    let mut stream = TcpStream::connect(&discovery.endpoint).unwrap();
    let created = request(
        &mut stream,
        &discovery.token,
        "session.create",
        serde_json::json!({"cwd": null}),
    );
    let doomed_pane = created["result"]["id"]
        .as_u64()
        .expect("session.create should return a pane id") as usize;
    assert!(
        wait_until(Duration::from_secs(10), || cache
            .styled_screen(doomed_pane)
            .is_some()),
        "the cache never saw the pane it was told about"
    );
    assert!(cache.is_live());
    drop(stream);

    // Kill, do not shut down: a crash leaves the stale discovery record
    // behind, which is exactly the state reconnection has to survive.
    first.kill().unwrap();
    first.wait().unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || !cache.is_live() || cache.epoch() > 0),
        "the cache kept claiming a killed Core was live"
    );

    let _second = spawn_core(&state_dir);
    // The killed Core left its own record behind, so "a record exists"
    // proves nothing -- wait for the pid to change before reading it.
    assert!(
        wait_until(Duration::from_secs(20), || read_discovery(&state_dir)
            .map(|found| found.pid != first_pid)
            .unwrap_or(false)),
        "the replacement never published its own discovery record"
    );
    let replacement = read_discovery(&state_dir).expect("discovery vanished");

    assert!(
        wait_until(Duration::from_secs(30), || cache.is_live()),
        "the cache never found the replacement Core"
    );
    assert!(
        cache.styled_screen(doomed_pane).is_none(),
        "a pane from the dead Core survived in the cache; the window would \
         draw a terminal with nothing behind it"
    );

    // And the healed cache tracks the new Core's sessions.
    let mut stream = TcpStream::connect(&replacement.endpoint).unwrap();
    let created = request(
        &mut stream,
        &replacement.token,
        "session.create",
        serde_json::json!({"cwd": null}),
    );
    let fresh_pane = created["result"]["id"].as_u64().unwrap() as usize;
    assert!(
        wait_until(Duration::from_secs(10), || cache
            .styled_screen(fresh_pane)
            .is_some()),
        "the reconnected cache is not following the new Core's sessions"
    );

    drop(cache);
    let _ = std::fs::remove_dir_all(&state_dir);
}

/// A window that dies takes nothing with it.
///
/// The Core exists so that shells outlive the thing drawing them, and the
/// half of that nobody had a test for was the window's own death. Killing a
/// GUI closes its sockets without draining, without closing sessions, and
/// without saying goodbye — which at the Core's boundary is indistinguishable
/// from any other abrupt disconnect, and is exactly what dropping these
/// streams does. Both of the window's connections go: the request channel and
/// the `core.host` registration that makes it the front end.
///
/// What has to survive is the whole point of the architecture: the shell, its
/// scrollback, and the ability of the *next* window to pick both up and keep
/// typing.
#[test]
fn sessions_and_scrollback_outlive_a_front_end_that_dies() {
    let state_dir = scratch_state_dir("front-end-death");
    let mut core = spawn_core(&state_dir);
    let discovery = wait_for_discovery(&state_dir, Duration::from_secs(10));

    let scrollback_of = |stream: &mut TcpStream, pane: u64| -> String {
        let read = request(
            stream,
            &discovery.token,
            "session.scrollback_text",
            serde_json::json!({"pane_id": pane, "tail_lines": 500}),
        );
        read["result"]["text"].as_str().unwrap_or_default().to_string()
    };

    // The window: one request channel, one front-end registration.
    let mut window = TcpStream::connect(&discovery.endpoint).unwrap();
    window.set_nodelay(true).unwrap();
    let mut host = TcpStream::connect(&discovery.endpoint).unwrap();
    let attached = request(
        &mut host,
        &discovery.token,
        "core.host",
        serde_json::Value::Null,
    );
    assert_eq!(attached["ok"], true, "core.host refused the front end: {attached}");

    let argv = if cfg!(windows) { ["cmd.exe"] } else { ["sh"] };
    let created = request(
        &mut window,
        &discovery.token,
        "session.create",
        serde_json::json!({"cols": 80, "rows": 24, "argv": argv}),
    );
    assert_eq!(created["ok"], true, "session.create failed: {created}");
    let pane_id = created["result"]["id"].as_u64().unwrap();

    request(
        &mut window,
        &discovery.token,
        "session.write",
        serde_json::json!({"pane_id": pane_id, "data": "echo BEFORE_THE_CRASH\r"}),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if scrollback_of(&mut window, pane_id).contains("BEFORE_THE_CRASH") {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        scrollback_of(&mut window, pane_id).contains("BEFORE_THE_CRASH"),
        "the shell never echoed the marker, so there is nothing to lose yet"
    );

    // The window dies. No drain, no close, no warning.
    drop(window);
    drop(host);
    std::thread::sleep(Duration::from_millis(500));

    // The Core is still alive and still serving.
    assert!(
        core.try_wait().unwrap().is_none(),
        "the Core followed its window into the grave"
    );
    let mut reopened = TcpStream::connect(&discovery.endpoint).unwrap();
    reopened.set_nodelay(true).unwrap();
    let health = request(
        &mut reopened,
        &discovery.token,
        "core.health",
        serde_json::Value::Null,
    );
    assert_eq!(health["ok"], true, "core.health failed after the window died: {health}");

    // The session is still there, and so is everything it had printed.
    let listed = request(
        &mut reopened,
        &discovery.token,
        "session.list",
        serde_json::Value::Null,
    );
    let sessions = listed["result"].as_array().cloned().unwrap_or_default();
    assert!(
        sessions.iter().any(|s| s["id"].as_u64() == Some(pane_id)),
        "the pane died with its window: {listed}"
    );
    assert!(
        scrollback_of(&mut reopened, pane_id).contains("BEFORE_THE_CRASH"),
        "the pane came back empty; scrollback did not outlive the window"
    );

    // And the next window can register and keep typing into it.
    let mut host_again = TcpStream::connect(&discovery.endpoint).unwrap();
    let reattached = request(
        &mut host_again,
        &discovery.token,
        "core.host",
        serde_json::Value::Null,
    );
    assert_eq!(reattached["ok"], true, "the replacement front end was refused: {reattached}");
    request(
        &mut reopened,
        &discovery.token,
        "session.write",
        serde_json::json!({"pane_id": pane_id, "data": "echo AFTER_THE_CRASH\r"}),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if scrollback_of(&mut reopened, pane_id).contains("AFTER_THE_CRASH") {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let text = scrollback_of(&mut reopened, pane_id);
    assert!(
        text.contains("AFTER_THE_CRASH"),
        "the adopted pane does not take input from the new window"
    );
    assert!(
        text.contains("BEFORE_THE_CRASH"),
        "typing into the adopted pane erased what was there before it"
    );

    let _ = request(
        &mut reopened,
        &discovery.token,
        "core.shutdown",
        serde_json::Value::Null,
    );
    assert!(wait_for_exit(&mut core, Duration::from_secs(10)));
    let _ = std::fs::remove_dir_all(&state_dir);
}

/// The Core serves the same surface the GUI used to.
///
/// M1's whole promise to existing agents is that moving the MCP server from
/// the window into the Core changed nothing they can see. `legacy_contract.rs`
/// freezes the names against the in-process surface; this asserts the surface
/// an agent actually reaches — over TCP, from a Core with no window at all —
/// is that same set. A divergence here means the migration published a
/// different API than the one the code thinks it has.
#[test]
fn the_headless_core_publishes_the_same_method_surface_as_the_library() {
    let state_dir = scratch_state_dir("surface-parity");
    let mut core = spawn_core(&state_dir);
    let discovery = wait_for_discovery(&state_dir, Duration::from_secs(10));
    let mcp_port = discovery.mcp_port.expect("core must publish an MCP port");

    let mut stream = TcpStream::connect(("127.0.0.1", mcp_port)).unwrap();
    stream.set_nodelay(true).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut call = |method: &str, params: serde_json::Value| -> serde_json::Value {
        let frame =
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        writeln!(stream, "{frame}").unwrap();
        stream.flush().unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };

    let auth = call("auth.login", serde_json::json!({"token": discovery.token}));
    assert_eq!(auth["result"]["status"], "ok", "auth failed: {auth}");

    let names = |value: &serde_json::Value| -> Vec<String> {
        let mut names: Vec<String> = value["mcp_methods"]
            .as_array()
            .expect("mcp_methods is an array")
            .iter()
            .map(|method| {
                method
                    .get("name")
                    .and_then(|name| name.as_str())
                    .or_else(|| method.as_str())
                    .unwrap_or_default()
                    .to_string()
            })
            .filter(|name| !name.is_empty())
            .collect();
        names.sort();
        names
    };

    let served = call("meta.surface", serde_json::json!({}));
    let served = names(&served["result"]);
    let in_process = names(&unterm_mcp::meta::surface(&serde_json::json!({})).unwrap());

    assert!(!served.is_empty(), "the Core published an empty surface");
    let only_served: Vec<_> = served.iter().filter(|n| !in_process.contains(n)).collect();
    let only_library: Vec<_> = in_process.iter().filter(|n| !served.contains(n)).collect();
    assert!(
        only_served.is_empty() && only_library.is_empty(),
        "the Core's surface and the library's have drifted apart; \
         only over the wire: {only_served:?}; only in-process: {only_library:?}"
    );

    let _ = request(
        &mut TcpStream::connect(&discovery.endpoint).unwrap(),
        &discovery.token,
        "core.shutdown",
        serde_json::Value::Null,
    );
    assert!(wait_for_exit(&mut core, Duration::from_secs(10)));
    let _ = std::fs::remove_dir_all(&state_dir);
}
