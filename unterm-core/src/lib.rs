//! Independent per-user Core process for the Unterm control plane.
//!
//! Hosts next-core terminal sessions behind an authenticated local IPC
//! boundary so they can outlive any GUI process. Identity and version
//! compatibility come from `unterm-protocol`; the terminal engine is
//! `unterm-engine`'s next-core. This is the M1 service entry point that
//! issue #12 requires: no GUI needed to create a PTY or query a screen.

use anyhow::{Context, Result};
use portable_pty::CommandBuilder;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use unterm_engine::next_core::mouse_encoding::{
    MouseButton, MouseEvent, MouseEventKind, MouseModifiers,
};
use unterm_engine::{
    CreateSessionRequest, CursorSnapshot, EngineHealthSnapshot, HealthEngine, InputEngine,
    LaunchPolicySnapshot, PaneModesSnapshot, RecordingEngine, RecordingExportResult,
    RecordingStartResult, RecordingStatusSnapshot, RecordingStopResult, RenderFrameSnapshot,
    ScreenEngine, ScreenLine, ScreenSearchMatch, ScreenSnapshot, ScrollbackTextRequest,
    ScrollbackTextSnapshot, SearchMode, SessionActivitySnapshot, SessionEngine,
    SessionSnapshot, ShellSnapshot, SplitDirection, SplitSessionRequest,
    StyledScreenSnapshot, StyledScrollbackSnapshot,
};
use unterm_protocol::{BuildHandshake, ProcessRole};

/// Discovery record other processes read to find and authenticate to
/// the running Core.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryInfo {
    pub endpoint: String,
    pub token: String,
    pub pid: u32,
    pub product_version: String,
    #[serde(default)]
    pub build_commit: String,
    #[serde(default)]
    pub protocol_version: String,
    #[serde(default)]
    pub data_schema_version: u32,
    #[serde(default)]
    pub process_role: ProcessRole,
    #[serde(default)]
    pub started_at: String,
    /// The Core-hosted MCP surface, when serving. Same token as the
    /// engine IPC. This is how agents reach sessions with no GUI
    /// alive; a GUI's own MCP server keeps `server.json` untouched.
    #[serde(default)]
    pub mcp_port: Option<u16>,
}

/// Where this Core keeps its discovery record and instance lock.
///
/// Delegates to `unterm-protocol` so that everything reading this record —
/// the supervisor, diagnostics, the provider manifest — resolves it the same
/// way. It used to be computed here as well as there, and the two drifted:
/// on Linux the Core wrote `~/.local/share/Unterm/core.json` while the
/// supervisor read `~/.unterm/core.json` and reported a running Core as
/// absent.
fn state_dir() -> Option<std::path::PathBuf> {
    unterm_protocol::core_state_dir()
}

pub fn discovery_path() -> Option<std::path::PathBuf> {
    state_dir().map(|dir| dir.join("core.json"))
}

pub fn read_discovery() -> Result<Option<DiscoveryInfo>> {
    let Some(path) = discovery_path() else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(
        &std::fs::read(path).context("read core discovery")?,
    )?))
}

pub fn write_discovery(
    endpoint: &str,
    token: &str,
    mcp_port: Option<u16>,
    started_at: &str,
) -> Result<()> {
    let Some(path) = discovery_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let info = DiscoveryInfo {
        endpoint: endpoint.into(),
        token: token.into(),
        pid: std::process::id(),
        product_version: unterm_protocol::PRODUCT_VERSION.into(),
        build_commit: unterm_protocol::BUILD_COMMIT.into(),
        protocol_version: unterm_protocol::PROTOCOL_VERSION.into(),
        data_schema_version: unterm_protocol::DATA_SCHEMA_VERSION,
        process_role: ProcessRole::Core,
        started_at: started_at.into(),
        mcp_port,
    };
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&info)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

pub fn clear_discovery() -> Result<()> {
    if let Some(path) = discovery_path() {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

/// Held for the lifetime of the owning Core process. The OS releases
/// the underlying lock when the process exits (cleanly or not), so a
/// crashed Core never leaves a stale lock behind.
pub struct InstanceLock {
    _file: std::fs::File,
}

pub fn instance_lock_path() -> Option<std::path::PathBuf> {
    discovery_path().map(|path| path.with_file_name("core.lock"))
}

/// Try to become the single Core instance for this user.
/// Returns `Ok(None)` when another live Core already holds the lock.
pub fn try_acquire_instance_lock() -> Result<Option<InstanceLock>> {
    let path = instance_lock_path()
        .ok_or_else(|| anyhow::anyhow!("no local data directory for core lock"))?;
    try_acquire_instance_lock_at(&path)
}

pub fn try_acquire_instance_lock_at(path: &std::path::Path) -> Result<Option<InstanceLock>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(open_lock_file(path)?.map(|file| InstanceLock { _file: file }))
}

#[cfg(windows)]
fn open_lock_file(path: &std::path::Path) -> Result<Option<std::fs::File>> {
    use std::os::windows::fs::OpenOptionsExt;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .share_mode(0)
        .open(path)
    {
        Ok(file) => Ok(Some(file)),
        Err(err) if err.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => Ok(None),
        Err(err) => Err(err).context("open core instance lock"),
    }
}

#[cfg(unix)]
fn open_lock_file(path: &std::path::Path) -> Result<Option<std::fs::File>> {
    use std::os::unix::io::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(path)
        .context("open core instance lock")?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(Some(file))
    } else {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(err).context("flock core instance lock")
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Request {
    pub id: Option<String>,
    pub method: String,
    pub token: Option<String>,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response<T> {
    pub id: Option<String>,
    pub ok: bool,
    pub result: Option<T>,
    pub error: Option<CoreError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CoreError {
    pub code: String,
    pub message: String,
}

pub fn response_ok<T: Serialize>(id: Option<String>, result: T) -> Response<T> {
    Response {
        id,
        ok: true,
        result: Some(result),
        error: None,
    }
}

pub fn response_error<T: Serialize>(
    id: Option<String>,
    code: &'static str,
    message: impl Into<String>,
) -> Response<T> {
    Response {
        id,
        ok: false,
        result: None,
        error: Some(CoreError {
            code: code.into(),
            message: message.into(),
        }),
    }
}

/// One session-affecting change, pushed to `core.events` subscribers.
///
/// Edge notifications, not a replayable log: a subscriber that connects
/// late bootstraps from `session.list` and only hears what happens next.
/// The durable, cursor-addressable event store is M2's work, not this.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CoreEvent {
    SessionCreated { pane_id: usize },
    SessionClosed { pane_id: usize },
    SessionDead { pane_id: usize, reason: Option<String> },
    ScreenUpdated { pane_id: usize, revision: u64 },
    Draining,
}

/// Fan-out point between the engine watcher and `core.events`
/// connections. Dead subscribers are dropped on the next publish.
struct EventHub {
    subscribers: std::sync::Mutex<Vec<std::sync::mpsc::Sender<String>>>,
}

impl EventHub {
    fn new() -> Self {
        Self {
            subscribers: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn subscribe(&self) -> std::sync::mpsc::Receiver<String> {
        let (sender, receiver) = std::sync::mpsc::channel();
        self.subscribers
            .lock()
            .expect("event hub lock poisoned")
            .push(sender);
        receiver
    }

    fn subscriber_count(&self) -> usize {
        self.subscribers
            .lock()
            .expect("event hub lock poisoned")
            .len()
    }

    fn publish(&self, event: &CoreEvent) {
        let Ok(line) = serde_json::to_string(event) else {
            return;
        };
        self.subscribers
            .lock()
            .expect("event hub lock poisoned")
            .retain(|sender| sender.send(line.clone()).is_ok());
    }
}

/// A call the Core makes *into* a front end.
///
/// Everything else on this wire runs client-to-Core. This one runs the
/// other way, because a few things only a window can do -- render text
/// with the window's font stack, raise the window, ask the person in
/// front of it whether an agent may type into their shell -- have to be
/// reachable from a Core that owns the sessions but owns no screen.
#[derive(Debug, Serialize, Deserialize)]
struct HostCall {
    host_call: HostCallBody,
}

#[derive(Debug, Serialize, Deserialize)]
struct HostCallBody {
    id: String,
    method: String,
    params: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct HostReplyFrame {
    host_reply: HostReplyBody,
}

#[derive(Debug, Deserialize)]
struct HostReplyBody {
    id: String,
    ok: bool,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<String>,
}

/// The Core's end of the reverse channel to a front end.
///
/// At most one front end is attached at a time -- a second registration
/// replaces the first, which is what happens when a window is closed and
/// reopened. When none is attached, `call` fails immediately rather than
/// blocking: a Core with no window must decline the things only a window
/// can do, not park its worker threads until they time out.
#[derive(Default)]
pub struct HostChannel {
    attachments: std::sync::Mutex<Vec<HostAttachment>>,
    pending: std::sync::Mutex<std::collections::HashMap<String, std::sync::mpsc::Sender<HostReplyBody>>>,
    next_id: std::sync::atomic::AtomicU64,
}

struct HostAttachment {
    id: u64,
    sender: std::sync::mpsc::Sender<String>,
}

pub fn host_channel() -> &'static Arc<HostChannel> {
    static CHANNEL: OnceLock<Arc<HostChannel>> = OnceLock::new();
    CHANNEL.get_or_init(|| Arc::new(HostChannel::default()))
}

impl HostChannel {
    /// Whether a front end is attached right now.
    ///
    /// The MCP surface asks this before offering capabilities that need
    /// one, and before parking a write on a confirmation nobody could
    /// answer.
    pub fn is_attached(&self) -> bool {
        self.attachments
            .lock()
            .map(|attachments| !attachments.is_empty())
            .unwrap_or(false)
    }

    /// Attach a front end, making it the current window.
    ///
    /// Older windows stay in the stack. If the current window closes,
    /// Core can fall back to the previous live one instead of becoming
    /// headless while another Unterm window is still open.
    fn attach(&self, sender: std::sync::mpsc::Sender<String>) -> u64 {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.attachments
            .lock()
            .expect("host channel lock poisoned")
            .push(HostAttachment { id, sender });
        self.fail_pending();
        id
    }

    /// Forget every front end and every waiter.
    ///
    /// Tests only. The channel is a process-global singleton while the
    /// front ends in it belong to individual tests, and a test that
    /// inherits the previous one's window is measuring the wrong window.
    #[cfg(test)]
    fn clear_for_tests(&self) {
        if let Ok(mut attachments) = self.attachments.lock() {
            attachments.clear();
        }
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
    }

    fn detach(&self, id: u64) {
        let mut attachments = self.attachments.lock().expect("host channel lock poisoned");
        let was_current = attachments
            .last()
            .map(|attachment| attachment.id == id)
            .unwrap_or(false);
        attachments.retain(|attachment| attachment.id != id);
        drop(attachments);
        if was_current {
            self.fail_pending();
        }
    }

    fn current_sender(&self) -> Option<(u64, std::sync::mpsc::Sender<String>)> {
        self.attachments.lock().ok().and_then(|attachments| {
            attachments
                .last()
                .map(|attachment| (attachment.id, attachment.sender.clone()))
        })
    }

    /// Drop every waiter. Their `recv` fails, which each caller turns
    /// into its own "no front end" answer.
    fn fail_pending(&self) {
        self.pending
            .lock()
            .expect("host channel lock poisoned")
            .clear();
    }

    fn resolve(&self, reply: HostReplyBody) {
        let waiter = self
            .pending
            .lock()
            .expect("host channel lock poisoned")
            .remove(&reply.id);
        if let Some(waiter) = waiter {
            let _ = waiter.send(reply);
        }
    }

    /// Tell the attached front end something, without waiting.
    ///
    /// For the calls that have no answer worth having -- "paint a frame"
    /// is the whole set so far. Blocking an MCP worker on a repaint
    /// would make the cheapest possible request the slowest one.
    pub fn notify(&self, method: &str, params: serde_json::Value) {
        let id = format!(
            "{}",
            self.next_id
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
        );
        let Ok(line) = serde_json::to_string(&HostCall {
            host_call: HostCallBody {
                id,
                method: method.to_string(),
                params,
            },
        }) else {
            return;
        };
        // No waiter registered, so the reply is dropped on arrival --
        // `resolve` simply finds nothing to hand it to.
        if let Some((id, sender)) = self.current_sender() {
            if sender.send(line).is_err() {
                self.detach(id);
            }
        }
    }

    /// Ask the attached front end to do something, and wait for its answer.
    ///
    /// `timeout` is a hard ceiling because the caller is usually an MCP
    /// worker thread: a front end that stops answering must not be able
    /// to hold that thread forever. The one long timeout is the
    /// confirmation prompt, which is waiting for a person.
    pub fn call(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value> {
        let id = format!(
            "{}",
            self.next_id
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let line = serde_json::to_string(&HostCall {
            host_call: HostCallBody {
                id: id.clone(),
                method: method.to_string(),
                params,
            },
        })?;

        let Some((attachment_id, sender)) = self.current_sender() else {
            anyhow::bail!("no Unterm window is attached to answer {method}");
        };
        // Registered before the send, so a reply that arrives
        // between them still finds its waiter.
        self.pending
            .lock()
            .expect("host channel lock poisoned")
            .insert(id.clone(), tx);
        if sender.send(line).is_err() {
            self.pending
                .lock()
                .expect("host channel lock poisoned")
                .remove(&id);
            self.detach(attachment_id);
            anyhow::bail!("the attached Unterm window stopped listening");
        }

        match rx.recv_timeout(timeout) {
            Ok(reply) if reply.ok => Ok(reply.result.unwrap_or(serde_json::Value::Null)),
            Ok(reply) => Err(anyhow::anyhow!(
                "{}",
                reply.error.unwrap_or_else(|| format!("{method} refused"))
            )),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.pending
                    .lock()
                    .expect("host channel lock poisoned")
                    .remove(&id);
                anyhow::bail!("the Unterm window did not answer {method} in time")
            }
            // The waiter was dropped, which is how detach reports that
            // the window went away mid-call.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("the Unterm window went away before answering {method}")
            }
        }
    }
}

/// Run one connection as the reverse channel until the front end hangs
/// up or the Core stops.
///
/// Two directions on one socket, so it is split: a writer thread drains
/// queued calls, and this thread reads replies. Sharing one thread would
/// mean a call could not be sent while a reply was being awaited, which
/// is exactly the situation the channel exists to serve.
fn serve_host_channel(stream: TcpStream, running: &AtomicBool) -> Result<()> {
    let channel = host_channel().clone();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let mut writer = stream.try_clone().context("clone host channel stream")?;
    // No stop flag of its own: the writer ends when the channel drops
    // the sender, which is exactly what detaching does.
    std::thread::Builder::new()
        .name("core-host-writer".into())
        .spawn(move || {
            while let Ok(line) = rx.recv() {
                if writeln!(writer, "{line}").is_err() || writer.flush().is_err() {
                    return;
                }
            }
        })
        .context("spawn host channel writer")?;

    let attachment_id = channel.attach(tx);
    // Ask the new window who it is -- on another thread, because the
    // answer comes back through the read loop below. Asking from here
    // would block the only thread that could deliver the reply.
    std::thread::Builder::new()
        .name("core-host-identity".into())
        .spawn(RemoteMcpHost::learn_identity)
        .ok();
    // A read timeout rather than a blocking read, so a stopping Core
    // does not have to wait for the window to hang up first.
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if !line.trim().is_empty() {
                    match serde_json::from_str::<HostReplyFrame>(line.trim()) {
                        Ok(frame) => channel.resolve(frame.host_reply),
                        // A frame this end cannot read is not worth
                        // killing the channel over; the call it belonged
                        // to times out on its own.
                        Err(err) => log::warn!("unreadable host reply: {err}"),
                    }
                }
            }
            // The timeout is this loop's heartbeat, not a failure.
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
        if !running.load(Ordering::Acquire) {
            break;
        }
    }
    channel.detach(attachment_id);
    Ok(())
}

/// Poll the engine for changes and publish them as events.
///
/// The engine has no wakeup hook yet, so this is where the polling
/// lives -- once, in the Core, instead of in every client's frame
/// loop. Adding a real hook later changes this function, not the wire
/// protocol. Idle cost stays low: with no subscribers the watcher only
/// keeps its baseline fresh, at a slower cadence.
fn watch_engine_events(
    hub: Arc<EventHub>,
    running: Arc<AtomicBool>,
    draining: Arc<AtomicBool>,
    exit_when_idle: Arc<AtomicBool>,
) {
    let engine = unterm_engine::next_core();
    let mut known: std::collections::HashMap<usize, (bool, u64)> = std::collections::HashMap::new();
    let mut announced_draining = false;
    while running.load(Ordering::Acquire) {
        let has_subscribers = hub.subscriber_count() > 0;

        if draining.load(Ordering::Acquire) && !announced_draining {
            announced_draining = true;
            hub.publish(&CoreEvent::Draining);
        }

        // A drain that was asked to finish the job: once the sessions
        // it was told to let run have ended, there is nothing left to
        // stay alive for.
        if exit_when_idle.load(Ordering::Acquire)
            && engine
                .list_sessions()
                .map(|sessions| sessions.is_empty())
                .unwrap_or(false)
        {
            running.store(false, Ordering::Release);
            break;
        }

        // Publishing to an empty hub is a no-op, so events are never
        // gated on subscriber presence: gating would race a subscriber
        // arriving between the check and a session appearing, and lose
        // that session's created event for good.
        if let Ok(sessions) = engine.list_sessions() {
            let mut seen = std::collections::HashSet::new();
            for session in &sessions {
                seen.insert(session.id);
                let revision = engine.screen_revision(session.id).unwrap_or(0);
                match known.get(&session.id) {
                    None => {
                        known.insert(session.id, (session.is_dead, revision));
                        hub.publish(&CoreEvent::SessionCreated {
                            pane_id: session.id,
                        });
                    }
                    Some(&(was_dead, last_revision)) => {
                        if session.is_dead && !was_dead {
                            hub.publish(&CoreEvent::SessionDead {
                                pane_id: session.id,
                                reason: session.dead_reason.clone(),
                            });
                        }
                        if revision != last_revision {
                            hub.publish(&CoreEvent::ScreenUpdated {
                                pane_id: session.id,
                                revision,
                            });
                        }
                        known.insert(session.id, (session.is_dead, revision));
                    }
                }
            }
            let closed: Vec<usize> = known
                .keys()
                .copied()
                .filter(|id| !seen.contains(id))
                .collect();
            for pane_id in closed {
                known.remove(&pane_id);
                hub.publish(&CoreEvent::SessionClosed { pane_id });
            }
        }

        let interval = if has_subscribers { 25 } else { 250 };
        std::thread::sleep(Duration::from_millis(interval));
    }
}

/// Authenticated local server shared by GUI, CLI and automation clients.
pub struct CoreServer {
    listener: TcpListener,
    token: String,
    started_at: String,
    running: Arc<AtomicBool>,
    draining: Arc<AtomicBool>,
    /// Set by `core.drain {exit_when_idle: true}`: stop for good once
    /// the sessions still running have ended.
    exit_when_idle: Arc<AtomicBool>,
    events: Arc<EventHub>,
}

impl CoreServer {
    pub fn bind<A: ToSocketAddrs>(address: A, token: impl Into<String>) -> Result<Self> {
        let listener = TcpListener::bind(address).context("bind unterm-core listener")?;
        listener
            .set_nonblocking(true)
            .context("set core listener nonblocking")?;
        Ok(Self {
            listener,
            token: token.into(),
            started_at: chrono::Utc::now().to_rfc3339(),
            running: Arc::new(AtomicBool::new(true)),
            draining: Arc::new(AtomicBool::new(false)),
            exit_when_idle: Arc::new(AtomicBool::new(false)),
            events: Arc::new(EventHub::new()),
        })
    }

    pub fn endpoint(&self) -> Result<std::net::SocketAddr> {
        Ok(self.listener.local_addr()?)
    }

    pub fn started_at(&self) -> &str {
        &self.started_at
    }

    pub fn run(&self) -> Result<()> {
        let watcher = {
            let hub = self.events.clone();
            let running = self.running.clone();
            let draining = self.draining.clone();
            let exit_when_idle = self.exit_when_idle.clone();
            std::thread::Builder::new()
                .name("core-event-watcher".into())
                .spawn(move || watch_engine_events(hub, running, draining, exit_when_idle))
                .context("spawn core event watcher")?
        };
        while self.running.load(Ordering::Acquire) {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .context("set core client blocking")?;
                    // Request/response frames are latency-bound, not
                    // throughput-bound; Nagle only adds stalls here.
                    stream
                        .set_nodelay(true)
                        .context("set core client nodelay")?;
                    let token = self.token.clone();
                    let started_at = self.started_at.clone();
                    let running = self.running.clone();
                    let draining = self.draining.clone();
                    let exit_when_idle = self.exit_when_idle.clone();
                    let events = self.events.clone();
                    std::thread::spawn(move || {
                        if let Err(err) =
                            handle_stream(stream, &token, &started_at, &running, &draining, &exit_when_idle, &events)
                        {
                            eprintln!("unterm-core client error: {err:#}");
                        }
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(err) => return Err(err).context("accept unterm-core client"),
            }
        }
        let _ = watcher.join();
        Ok(())
    }
}

fn handle_stream(
    mut stream: TcpStream,
    token: &str,
    started_at: &str,
    running: &AtomicBool,
    draining: &AtomicBool,
    exit_when_idle: &AtomicBool,
    events: &Arc<EventHub>,
) -> Result<()> {
    let reader = BufReader::new(stream.try_clone()?);
    for line in reader.lines() {
        let line = line?;
        let request: Request = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(err) => {
                writeln!(
                    stream,
                    "{}",
                    serde_json::to_string(&response_error::<()>(
                        None,
                        "invalid_request",
                        err.to_string()
                    ))?
                )?;
                stream.flush()?;
                continue;
            }
        };
        if request.token.as_deref() != Some(token) {
            writeln!(
                stream,
                "{}",
                serde_json::to_string(&response_error::<()>(
                    request.id,
                    "unauthenticated",
                    "invalid core token"
                ))?
            )?;
            stream.flush()?;
            continue;
        }
        if request.method == "core.events" {
            // This connection becomes a one-way event feed: acknowledge,
            // then push until the subscriber hangs up or the core stops.
            let receiver = events.subscribe();
            writeln!(
                stream,
                "{}",
                serde_json::to_string(&response_ok(
                    request.id,
                    serde_json::json!({"subscribed": true}),
                ))?
            )?;
            stream.flush()?;
            loop {
                match receiver.recv_timeout(Duration::from_millis(500)) {
                    Ok(line) => {
                        if writeln!(stream, "{line}").is_err() || stream.flush().is_err() {
                            return Ok(());
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if !running.load(Ordering::Acquire) {
                            return Ok(());
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                }
            }
        }
        if request.method == "core.host" {
            // This connection becomes the reverse channel: from here on
            // the Core writes calls down it and reads their replies. It
            // is a separate connection from the event feed on purpose --
            // the feed is owned by the frame cache and must stay a
            // simple one-way loop.
            writeln!(
                stream,
                "{}",
                serde_json::to_string(&response_ok(
                    request.id,
                    serde_json::json!({"attached": true}),
                ))?
            )?;
            stream.flush()?;
            return serve_host_channel(stream, running);
        }
        let should_stop = request.method == "core.shutdown";
        let creates_session =
            matches!(request.method.as_str(), "session.create" | "session.split");
        if creates_session && draining.load(Ordering::Acquire) {
            writeln!(
                stream,
                "{}",
                serde_json::to_string(&response_error::<()>(
                    request.id,
                    "draining",
                    "core is draining; new sessions are rejected"
                ))?
            )?;
            stream.flush()?;
            continue;
        }
        let response = dispatch(request, started_at, draining, exit_when_idle);
        writeln!(stream, "{response}")?;
        stream.flush()?;
        if should_stop {
            running.store(false, Ordering::Release);
            break;
        }
    }
    Ok(())
}

fn dispatch(
    request: Request,
    started_at: &str,
    draining: &AtomicBool,
    exit_when_idle: &AtomicBool,
) -> String {
    match dispatch_inner(&request, started_at, draining, exit_when_idle) {
        Ok(response) => response,
        Err(err) => serde_json::to_string(&response_error::<()>(
            request.id,
            "internal_error",
            format!("{err:#}"),
        ))
        .unwrap_or_else(|_| r#"{"ok":false}"#.into()),
    }
}

fn dispatch_inner(
    request: &Request,
    started_at: &str,
    draining: &AtomicBool,
    exit_when_idle: &AtomicBool,
) -> Result<String> {
    let engine = unterm_engine::next_core();
    let id = request.id.clone();
    let response = match request.method.as_str() {
        "core.info" => serde_json::to_string(&response_ok(
            id,
            BuildHandshake::current(ProcessRole::Core, std::process::id(), started_at),
        ))?,
        "core.health" => {
            let is_draining = draining.load(Ordering::Acquire);
            let status = if is_draining { "draining" } else { "ready" };
            let active_session_count = engine.list_sessions()?.len();
            serde_json::to_string(&response_ok(
                id,
                serde_json::json!({
                    "status": status,
                    "alive": true,
                    "ready": !is_draining,
                    "accepting_sessions": !is_draining,
                    "draining": is_draining,
                    "active_session_count": active_session_count,
                    "drained": is_draining && active_session_count == 0,
                }),
            ))?
        }
        "core.readiness" => {
            let is_draining = draining.load(Ordering::Acquire);
            let status = if is_draining { "not_ready" } else { "ready" };
            let active_session_count = engine.list_sessions()?.len();
            serde_json::to_string(&response_ok(
                id,
                serde_json::json!({
                    "status": status,
                    "ready": !is_draining,
                    "accepting_sessions": !is_draining,
                    "reason": if is_draining { "draining" } else { "ready" },
                    "active_session_count": active_session_count,
                    "drained": is_draining && active_session_count == 0,
                }),
            ))?
        }
        "core.drain" => {
            draining.store(true, Ordering::Release);
            // `exit_when_idle` is what "drain, then exit" actually
            // means: refuse new sessions now, and stop for good once
            // the ones still running have ended. Without it a client
            // asking to drain has no way to say it also wanted to go.
            let then_exit = request
                .params
                .get("exit_when_idle")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if then_exit {
                exit_when_idle.store(true, Ordering::Release);
            }
            let active_session_count = engine.list_sessions()?.len();
            serde_json::to_string(&response_ok(
                id,
                serde_json::json!({
                    "status":"draining",
                    "exit_when_idle": then_exit,
                    "active_session_count": active_session_count,
                    "drained": active_session_count == 0,
                }),
            ))?
        }
        "core.shutdown" => serde_json::to_string(&response_ok(
            id,
            serde_json::json!({"status":"stopping"}),
        ))?,
        "session.create" => {
            let (cols, rows) = parse_dimensions(&request.params);
            let session = engine.create_session(CreateSessionRequest {
                cols,
                rows,
                command_dir: parse_cwd(&request.params),
                command: parse_argv(&request.params),
                env: parse_env(&request.params),
                launch_policy: parse_launch_policy(&request.params),
            })?;
            serde_json::to_string(&response_ok(id, session))?
        }
        "session.split" => {
            let source_pane_id = required_pane_id(request)?;
            let direction = match request
                .params
                .get("direction")
                .and_then(|v| v.as_str())
                .unwrap_or("right")
            {
                "left" => SplitDirection::Left,
                "down" => SplitDirection::Down,
                "up" => SplitDirection::Up,
                _ => SplitDirection::Right,
            };
            let size_percent = request
                .params
                .get("size_percent")
                .and_then(|v| v.as_u64())
                .unwrap_or(50) as u8;
            let session = engine.split_session(SplitSessionRequest {
                source_pane_id,
                direction,
                size_percent,
                command_dir: parse_cwd(&request.params),
                command: parse_argv(&request.params),
                env: parse_env(&request.params),
                launch_policy: parse_launch_policy(&request.params),
            })?;
            serde_json::to_string(&response_ok(id, session))?
        }
        "session.get" => {
            let pane_id = required_pane_id(request)?;
            serde_json::to_string(&response_ok(id, engine.get_session(pane_id)?))?
        }
        "session.focus" => {
            engine.focus_session(required_pane_id(request)?)?;
            serde_json::to_string(&response_ok(id, serde_json::json!({"focused": true})))?
        }
        "session.shell" => {
            let pane_id = required_pane_id(request)?;
            serde_json::to_string(&response_ok(id, engine.shell(pane_id)?))?
        }
        "session.activity" => {
            let pane_id = required_pane_id(request)?;
            serde_json::to_string(&response_ok(id, engine.activity(pane_id)?))?
        }
        "session.modes" => {
            let pane_id = required_pane_id(request)?;
            serde_json::to_string(&response_ok(id, engine.pane_modes(pane_id)?))?
        }
        "session.styled_screen" => {
            let pane_id = required_pane_id(request)?;
            serde_json::to_string(&response_ok(id, engine.read_styled_screen(pane_id)?))?
        }
        "session.styled_frame" => {
            let pane_id = required_pane_id(request)?;
            let since_revision = request
                .params
                .get("since_revision")
                .and_then(|v| v.as_u64());
            // The engine read costs microseconds; what a stale caller
            // must not pay is serializing 4800 styled cells for a frame
            // it already has. Compare revisions server-side and send a
            // small envelope instead.
            let screen = engine.read_styled_screen(pane_id)?;
            let body = if since_revision == Some(screen.revision) {
                serde_json::json!({"unchanged": true, "revision": screen.revision, "screen": null})
            } else {
                serde_json::json!({"unchanged": false, "revision": screen.revision, "screen": screen})
            };
            serde_json::to_string(&response_ok(id, body))?
        }
        "session.pane_pulse" => {
            let pane_id = required_pane_id(request)?;
            let since_revision = request
                .params
                .get("since_revision")
                .and_then(|v| v.as_u64());
            let tail_rows = request
                .params
                .get("tail_rows")
                .and_then(|v| v.as_u64())
                .unwrap_or(8) as usize;
            // Built here rather than at the caller: the point is that the
            // cells never cross the socket. A pane whose revision has not
            // moved sends an empty envelope.
            let screen = engine.read_styled_screen(pane_id)?;
            let pulse = unterm_engine::PanePulse::from_snapshot(&screen, since_revision, tail_rows);
            serde_json::to_string(&response_ok(id, pulse))?
        }
        "session.visible_text" => {
            let pane_id = required_pane_id(request)?;
            serde_json::to_string(&response_ok(id, engine.read_visible_text(pane_id)?))?
        }
        "session.lines" => {
            let pane_id = required_pane_id(request)?;
            let start = request
                .params
                .get("start")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let count = request
                .params
                .get("count")
                .and_then(|v| v.as_u64())
                .unwrap_or(100) as usize;
            serde_json::to_string(&response_ok(id, engine.read_lines(pane_id, start, count)?))?
        }
        "session.scrollback" => {
            let pane_id = required_pane_id(request)?;
            let limit = request
                .params
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(1000) as usize;
            serde_json::to_string(&response_ok(id, engine.read_scrollback(pane_id, limit)?))?
        }
        "session.scrollback_text" => {
            let pane_id = required_pane_id(request)?;
            serde_json::to_string(&response_ok(
                id,
                engine.read_scrollback_text(pane_id, parse_scrollback_request(&request.params))?,
            ))?
        }
        "session.styled_scrollback" => {
            let pane_id = required_pane_id(request)?;
            serde_json::to_string(&response_ok(
                id,
                engine.read_styled_scrollback(pane_id, parse_scrollback_request(&request.params))?,
            ))?
        }
        "session.search" => {
            let pane_id = required_pane_id(request)?;
            let pattern = request
                .params
                .get("pattern")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("pattern is required"))?;
            let mode = match request.params.get("mode").and_then(|v| v.as_str()) {
                Some("case_sensitive") => SearchMode::CaseSensitive,
                _ => SearchMode::CaseInsensitive,
            };
            let max_results = request
                .params
                .get("max_results")
                .and_then(|v| v.as_u64())
                .unwrap_or(100) as usize;
            serde_json::to_string(&response_ok(
                id,
                engine.search(pane_id, pattern, mode, max_results)?,
            ))?
        }
        "session.cursor" => {
            let pane_id = required_pane_id(request)?;
            serde_json::to_string(&response_ok(id, engine.cursor(pane_id)?))?
        }
        "session.erase_scrollback" => {
            let pane_id = required_pane_id(request)?;
            let include_viewport = request
                .params
                .get("include_viewport")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            engine.erase_scrollback(pane_id, include_viewport)?;
            serde_json::to_string(&response_ok(id, serde_json::json!({"erased": true})))?
        }
        "session.paste" => {
            let pane_id = required_pane_id(request)?;
            let data = request
                .params
                .get("data")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            engine.paste_input(pane_id, data)?;
            serde_json::to_string(&response_ok(id, serde_json::json!({"pasted": true})))?
        }
        "session.revision" => {
            let pane_id = required_pane_id(request)?;
            serde_json::to_string(&response_ok(id, engine.screen_revision(pane_id)?))?
        }
        "session.scroll_to" => {
            let pane_id = required_pane_id(request)?;
            let target = required_i64(request, "target")? as isize;
            engine.scroll_viewport_to(pane_id, target)?;
            serde_json::to_string(&response_ok(id, serde_json::json!({"scrolled": true})))?
        }
        "session.scroll_by" => {
            let pane_id = required_pane_id(request)?;
            let delta = required_i64(request, "delta")? as isize;
            engine.scroll_viewport_by(pane_id, delta)?;
            serde_json::to_string(&response_ok(id, serde_json::json!({"scrolled": true})))?
        }
        "session.scroll_to_prompt" => {
            let pane_id = required_pane_id(request)?;
            let amount = required_i64(request, "amount")? as isize;
            engine.scroll_viewport_to_prompt(pane_id, amount)?;
            serde_json::to_string(&response_ok(id, serde_json::json!({"scrolled": true})))?
        }
        "session.report_mouse" => {
            let pane_id = required_pane_id(request)?;
            engine.report_mouse(pane_id, parse_mouse_event(&request.params)?)?;
            serde_json::to_string(&response_ok(id, serde_json::json!({"reported": true})))?
        }
        "session.recording_start" => {
            let pane_id = required_pane_id(request)?;
            serde_json::to_string(&response_ok(id, engine.start_recording(pane_id)?))?
        }
        "session.recording_stop" => {
            let pane_id = required_pane_id(request)?;
            serde_json::to_string(&response_ok(id, engine.stop_recording(pane_id)?))?
        }
        "session.recording_status" => {
            let pane_id = required_pane_id(request)?;
            serde_json::to_string(&response_ok(id, engine.recording_status(pane_id)?))?
        }
        "session.recording_attach_trace" => {
            let pane_id = required_pane_id(request)?;
            let trace_id = request
                .params
                .get("trace_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("trace_id is required"))?
                .to_owned();
            serde_json::to_string(&response_ok(
                id,
                engine.attach_recording_trace(pane_id, trace_id)?,
            ))?
        }
        "session.recording_export" => {
            let pane_id = required_pane_id(request)?;
            let target_path = request
                .params
                .get("target_path")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            serde_json::to_string(&response_ok(
                id,
                engine.export_markdown(pane_id, target_path)?,
            ))?
        }
        "core.engine_health" => {
            serde_json::to_string(&response_ok(id, engine.health()?))?
        }
        // The agent-facing state a window draws but does not own.
        // Pulled rather than pushed: none of it is latency-critical, and
        // a pull keeps the reverse channel free for the things that
        // genuinely have to interrupt.
        "mcp.suggestions" => {
            let pane_id = request
                .params
                .get("pane_id")
                .and_then(|value| value.as_u64())
                .unwrap_or_default();
            serde_json::to_string(&response_ok(
                id,
                unterm_mcp::handler::pending_suggestions_for_pane(pane_id),
            ))?
        }
        "mcp.accept_suggestion" => {
            let suggestion_id = request
                .params
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let run = request
                .params
                .get("run")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            match unterm_mcp::handler::accept_suggestion(&suggestion_id, run) {
                Ok(text) => serde_json::to_string(&response_ok(id, text))?,
                Err(message) => serde_json::to_string(&response_error::<serde_json::Value>(
                    id,
                    "suggestion_rejected",
                    &message,
                ))?,
            }
        }
        "mcp.dismiss_suggestion" => {
            let suggestion_id = request
                .params
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            match unterm_mcp::handler::dismiss_suggestion(&suggestion_id) {
                Ok(()) => serde_json::to_string(&response_ok(id, serde_json::Value::Null))?,
                Err(message) => serde_json::to_string(&response_error::<serde_json::Value>(
                    id,
                    "suggestion_rejected",
                    &message,
                ))?,
            }
        }
        "mcp.insights" => {
            let limit = request
                .params
                .get("recent_audit_limit")
                .and_then(|value| value.as_u64())
                .unwrap_or_default() as usize;
            serde_json::to_string(&response_ok(
                id,
                unterm_mcp::handler::insights_mcp_snapshot(limit),
            ))?
        }
        "mcp.audit_gui_write" => {
            let text = |key: &str| {
                request
                    .params
                    .get(key)
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            unterm_mcp::handler::audit_gui_write(
                &text("method"),
                request
                    .params
                    .get("pane_id")
                    .and_then(|value| value.as_u64())
                    .unwrap_or_default(),
                &text("detail"),
            );
            serde_json::to_string(&response_ok(id, serde_json::Value::Null))?
        }
        "core.set_scrollback_lines" => {
            let lines = request
                .params
                .get("lines")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow::anyhow!("lines is required"))?
                as usize;
            // Applies to sessions created from here on; existing panes
            // keep their capacity, matching the settings page's
            // "new pane to apply" contract. Last client to set wins.
            unterm_engine::next_core::NextCoreEngine::set_new_session_scrollback_lines(lines);
            serde_json::to_string(&response_ok(id, serde_json::json!({"applied": lines})))?
        }
        "session.list" => {
            serde_json::to_string(&response_ok(id, engine.list_sessions()?))?
        }
        "session.write" => {
            let pane_id = required_pane_id(request)?;
            let data = request
                .params
                .get("data")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            engine.write_input(pane_id, data)?;
            serde_json::to_string(&response_ok(
                id,
                serde_json::json!({"written": true}),
            ))?
        }
        "session.screen" => {
            let pane_id = required_pane_id(request)?;
            serde_json::to_string(&response_ok(id, engine.read_screen(pane_id)?))?
        }
        "session.frame" => {
            let pane_id = required_pane_id(request)?;
            let since_revision = request
                .params
                .get("since_revision")
                .and_then(|v| v.as_u64());
            serde_json::to_string(&response_ok(
                id,
                engine.read_render_frame(pane_id, since_revision)?,
            ))?
        }
        "session.resize" => {
            let pane_id = required_pane_id(request)?;
            let cols = request
                .params
                .get("cols")
                .and_then(|v| v.as_u64())
                .unwrap_or(120) as usize;
            let rows = request
                .params
                .get("rows")
                .and_then(|v| v.as_u64())
                .unwrap_or(30) as usize;
            // A window laying itself out marks its resizes: those pass through
            // provisional sizes on the way to the real one, and a shrink that
            // is taken back must never reach the pane. Anyone else asked for
            // exactly this size and gets it now.
            let settle = request
                .params
                .get("settle")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if settle {
                engine.resize_session_settled(pane_id, cols, rows)?;
            } else {
                engine.resize_session(pane_id, cols, rows)?;
            }
            serde_json::to_string(&response_ok(
                id,
                serde_json::json!({"resized": true}),
            ))?
        }
        // A divider the user dragged in a window, told to the process that
        // will still be here when that window is gone.
        "session.set_split_ratio" => {
            let pane_id = required_pane_id(request)?;
            let first_ratio = request
                .params
                .get("first_ratio")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| anyhow::anyhow!("session.set_split_ratio needs first_ratio"))?;
            engine.set_split_ratio(pane_id, first_ratio)?;
            serde_json::to_string(&response_ok(
                id,
                serde_json::json!({"first_ratio": first_ratio}),
            ))?
        }
        "session.close" => {
            let pane_id = required_pane_id(request)?;
            engine.destroy_session(pane_id)?;
            serde_json::to_string(&response_ok(
                id,
                serde_json::json!({"status":"closed"}),
            ))?
        }
        method => serde_json::to_string(&response_error::<()>(
            id,
            "method_not_found",
            method,
        ))?,
    };
    Ok(response)
}

fn required_pane_id(request: &Request) -> Result<usize> {
    request
        .params
        .get("pane_id")
        .and_then(|v| v.as_u64())
        .map(|pane_id| pane_id as usize)
        .ok_or_else(|| anyhow::anyhow!("pane_id is required"))
}

fn parse_dimensions(params: &serde_json::Value) -> (usize, usize) {
    let cols = params.get("cols").and_then(|v| v.as_u64()).unwrap_or(120) as usize;
    let rows = params.get("rows").and_then(|v| v.as_u64()).unwrap_or(30) as usize;
    (cols, rows)
}

fn parse_cwd(params: &serde_json::Value) -> Option<String> {
    params
        .get("cwd")
        .and_then(|v| v.as_str())
        .filter(|dir| !dir.is_empty())
        .map(str::to_owned)
}

fn parse_argv(params: &serde_json::Value) -> Option<CommandBuilder> {
    params
        .get("argv")
        .and_then(|v| v.as_array())
        .map(|argv| {
            argv.iter()
                .filter_map(|item| item.as_str().map(std::ffi::OsString::from))
                .collect::<Vec<_>>()
        })
        .filter(|argv| !argv.is_empty())
        .map(CommandBuilder::from_argv)
}

fn parse_env(params: &serde_json::Value) -> Vec<(String, String)> {
    params
        .get("env")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

fn parse_launch_policy(params: &serde_json::Value) -> LaunchPolicySnapshot {
    params
        .get("launch_policy")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

fn required_i64(request: &Request, key: &str) -> Result<i64> {
    request
        .params
        .get(key)
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow::anyhow!("{key} is required"))
}

/// The wire form of a mouse event. Explicit fields instead of serde on
/// the engine type, so the protocol does not inherit termwiz's layout.
fn parse_mouse_event(params: &serde_json::Value) -> Result<MouseEvent> {
    let kind = match params.get("kind").and_then(|v| v.as_str()) {
        Some("press") => MouseEventKind::Press,
        Some("release") => MouseEventKind::Release,
        Some("motion") => MouseEventKind::Motion,
        other => anyhow::bail!("unknown mouse event kind: {other:?}"),
    };
    let button = match params.get("button").and_then(|v| v.as_str()) {
        None => None,
        Some("left") => Some(MouseButton::Left),
        Some("middle") => Some(MouseButton::Middle),
        Some("right") => Some(MouseButton::Right),
        Some("wheel_up") => Some(MouseButton::WheelUp),
        Some("wheel_down") => Some(MouseButton::WheelDown),
        Some("wheel_left") => Some(MouseButton::WheelLeft),
        Some("wheel_right") => Some(MouseButton::WheelRight),
        Some(other) => anyhow::bail!("unknown mouse button: {other}"),
    };
    let mut modifiers = MouseModifiers::NONE;
    if params.get("shift").and_then(|v| v.as_bool()).unwrap_or(false) {
        modifiers |= MouseModifiers::SHIFT;
    }
    if params.get("alt").and_then(|v| v.as_bool()).unwrap_or(false) {
        modifiers |= MouseModifiers::ALT;
    }
    if params.get("ctrl").and_then(|v| v.as_bool()).unwrap_or(false) {
        modifiers |= MouseModifiers::CTRL;
    }
    Ok(MouseEvent {
        kind,
        button,
        column: params.get("column").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        row: params.get("row").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        modifiers,
    })
}

fn mouse_event_params(pane_id: usize, event: MouseEvent) -> serde_json::Value {
    let kind = match event.kind {
        MouseEventKind::Press => "press",
        MouseEventKind::Release => "release",
        MouseEventKind::Motion => "motion",
    };
    let button = event.button.map(|button| match button {
        MouseButton::Left => "left",
        MouseButton::Middle => "middle",
        MouseButton::Right => "right",
        MouseButton::WheelUp => "wheel_up",
        MouseButton::WheelDown => "wheel_down",
        MouseButton::WheelLeft => "wheel_left",
        MouseButton::WheelRight => "wheel_right",
    });
    serde_json::json!({
        "pane_id": pane_id,
        "kind": kind,
        "button": button,
        "column": event.column,
        "row": event.row,
        "shift": event.modifiers.contains(MouseModifiers::SHIFT),
        "alt": event.modifiers.contains(MouseModifiers::ALT),
        "ctrl": event.modifiers.contains(MouseModifiers::CTRL),
    })
}

fn parse_scrollback_request(params: &serde_json::Value) -> ScrollbackTextRequest {
    ScrollbackTextRequest {
        start_line: params.get("start_line").and_then(|v| v.as_i64()),
        end_line: params.get("end_line").and_then(|v| v.as_i64()),
        tail_lines: params.get("tail_lines").and_then(|v| v.as_i64()),
        escapes: params
            .get("escapes")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    }
}

pub struct CoreClient {
    stream: TcpStream,
    token: String,
}

impl CoreClient {
    /// Give up request/response framing and take the raw socket.
    ///
    /// For connections that change protocol after a handshake -- the
    /// reverse channel does, once `core.host` is acknowledged.
    pub fn into_stream(self) -> TcpStream {
        self.stream
    }

    pub fn connect<A: ToSocketAddrs>(address: A, token: impl Into<String>) -> Result<Self> {
        let mut last_error = None;
        let mut stream = None;
        for addr in address
            .to_socket_addrs()
            .context("resolve unterm-core endpoint")?
        {
            match TcpStream::connect_timeout(&addr, Duration::from_millis(250)) {
                Ok(connected) => {
                    stream = Some(connected);
                    break;
                }
                Err(err) => last_error = Some(err),
            }
        }
        let stream = stream.ok_or_else(|| {
            anyhow::anyhow!(
                "connect unterm-core: {}",
                last_error
                    .map(|err| err.to_string())
                    .unwrap_or_else(|| "endpoint resolved to no addresses".to_string())
            )
        })?;
        stream.set_nodelay(true).context("set core nodelay")?;
        Ok(Self {
            stream,
            token: token.into(),
        })
    }

    pub fn request<T: for<'de> Deserialize<'de>>(&mut self, method: &str) -> Result<Response<T>> {
        self.request_with_params(method, serde_json::Value::Null)
    }

    pub fn request_with_params<T: for<'de> Deserialize<'de>>(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<Response<T>> {
        let request = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "method": method,
            "token": self.token,
            "params": params,
        });
        writeln!(self.stream, "{request}")?;
        self.stream.flush()?;
        let mut line = String::new();
        BufReader::new(self.stream.try_clone()?).read_line(&mut line)?;
        Ok(serde_json::from_str(&line).context("decode unterm-core response")?)
    }

    /// Query the Core's identity and fail loudly on version skew.
    /// Version-mismatched client/core pairs must not talk past the
    /// handshake: silently degraded sessions are worse than a clear
    /// startup error.
    pub fn handshake(&mut self) -> Result<BuildHandshake> {
        let info: Response<BuildHandshake> = self.request("core.info")?;
        let info = info
            .result
            .ok_or_else(|| anyhow::anyhow!("unterm-core returned no identity"))?;
        let compatibility = info.compatibility();
        if !compatibility.is_usable() {
            anyhow::bail!(
                "unterm-core is not compatible ({}): core pid {} runs {} ({}), this client is {} ({})",
                compatibility.error_code().unwrap_or("incompatible"),
                info.pid,
                info.product_version,
                info.protocol_version,
                unterm_protocol::PRODUCT_VERSION,
                unterm_protocol::PROTOCOL_VERSION,
            );
        }
        Ok(info)
    }
}

/// Blocking subscription to the Core's event feed.
///
/// Owns its own connection: events push on it continuously, so it
/// cannot share the request/response connection a `CoreEngineClient`
/// multiplexes. Framing is buffered by hand instead of `BufReader`:
/// with a read timeout set, a timed-out read must leave any half-
/// received line in the buffer instead of losing it — that is what
/// lets a consumer poll a stop flag without corrupting the stream.
/// (`shutdown()` is no escape hatch here: on Windows it does not
/// unblock an already-blocked `recv`.)
pub struct CoreEventStream {
    stream: TcpStream,
    buffer: Vec<u8>,
}

impl CoreEventStream {
    pub fn connect<A: ToSocketAddrs>(address: A, token: impl Into<String>) -> Result<Self> {
        let stream = TcpStream::connect(address).context("connect unterm-core events")?;
        stream
            .set_nodelay(true)
            .context("set core events nodelay")?;
        let request = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "method": "core.events",
            "token": token.into(),
            "params": serde_json::Value::Null,
        });
        let mut this = Self {
            stream,
            buffer: Vec::new(),
        };
        writeln!(this.stream, "{request}")?;
        this.stream.flush()?;
        let line = this
            .next_line()?
            .ok_or_else(|| anyhow::anyhow!("core.events feed closed before the ack"))?;
        let ack: Response<serde_json::Value> =
            serde_json::from_str(&line).context("decode core.events ack")?;
        if !ack.ok {
            let code = ack.error.map(|error| error.code).unwrap_or_default();
            anyhow::bail!("core.events subscription refused ({code})");
        }
        Ok(this)
    }

    /// Block until the next event arrives. `Ok(None)` means the feed
    /// closed: the core shut down or the connection dropped. With a
    /// read timeout set, the timeout surfaces as `Err` and the stream
    /// stays consistent — retrying is safe.
    pub fn next_event(&mut self) -> Result<Option<CoreEvent>> {
        match self.next_line()? {
            None => Ok(None),
            Some(line) => Ok(Some(
                serde_json::from_str(line.trim()).context("decode core event")?,
            )),
        }
    }

    fn next_line(&mut self) -> Result<Option<String>> {
        use std::io::Read;
        loop {
            if let Some(pos) = self.buffer.iter().position(|&byte| byte == b'\n') {
                let line: Vec<u8> = self.buffer.drain(..=pos).collect();
                return Ok(Some(String::from_utf8(line).context("core event not utf-8")?));
            }
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Ok(None),
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                Err(err) => return Err(err).context("read core event feed"),
            }
        }
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> Result<()> {
        self.stream
            .set_read_timeout(timeout)
            .context("set core event read timeout")
    }
}

/// True when the error is only a read timeout: the feed is intact and
/// the caller may retry after checking whatever it paused to check.
pub fn is_timeout_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<std::io::Error>().is_some_and(|io| {
        matches!(
            io.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        )
    })
}

/// How long the Core waits on a window for each kind of question.
///
/// Split by what is actually happening on the other end. A repaint or a
/// title change is a few microseconds of work; rendering a whole
/// scrollback to a PNG is real work on a real font stack. Using one
/// timeout for both would either abandon the slow one or let the fast
/// one hang an MCP worker for a minute.
const HOST_QUICK: Duration = Duration::from_secs(5);
const HOST_SLOW: Duration = Duration::from_secs(60);

/// The `McpHost` a Core presents when a window is attached: every call
/// is forwarded down the reverse channel and answered by that window.
///
/// With no window attached each method degrades exactly the way the
/// no-front-end defaults in `unterm-engine` already do -- bail, or
/// report nothing -- so the MCP surface needs no special case for
/// "headless" beyond what it already has.
pub struct RemoteMcpHost;

/// The window's identity, learned once when a front end first attaches.
///
/// `WindowIdentity` is made of `&'static str`, and this one arrives over
/// a wire as owned strings. Storing it once rather than per call is what
/// keeps that from being a leak: a GUI's identity is a property of the
/// product, so a second window reports what the first did.
static REMOTE_IDENTITY: OnceLock<unterm_engine::WindowIdentity> = OnceLock::new();

fn remember_remote_identity(value: &serde_json::Value) {
    if REMOTE_IDENTITY.get().is_some() {
        return;
    }
    let text = |key: &str| -> Option<&'static str> {
        value
            .get(key)
            .and_then(|found| found.as_str())
            .map(|found| &*Box::leak(found.to_string().into_boxed_str()))
    };
    let (Some(engine), Some(window_owner), Some(native_window_lifecycle)) = (
        text("engine"),
        text("window_owner"),
        text("native_window_lifecycle"),
    ) else {
        return;
    };
    let _ = REMOTE_IDENTITY.set(unterm_engine::WindowIdentity {
        engine,
        window_owner,
        native_window_lifecycle,
        uses_host_window: value
            .get("uses_host_window")
            .and_then(|found| found.as_bool())
            .unwrap_or(false),
    });
}

impl RemoteMcpHost {
    /// Learn who the attached window is, so `window_identity` can be
    /// answered without a round trip. Called once per attach; every
    /// reader of the identity is on a hot path and none of them can
    /// afford IPC.
    pub fn learn_identity() {
        if let Ok(value) = host_channel().call(
            "window_identity",
            serde_json::Value::Null,
            HOST_QUICK,
        ) {
            remember_remote_identity(&value);
        }
    }
}

impl unterm_engine::McpHost for RemoteMcpHost {
    fn window_identity(&self) -> unterm_engine::WindowIdentity {
        // Only claim a window when one is actually attached: an agent
        // reading this decides whether there is anyone to ask.
        if !host_channel().is_attached() {
            return unterm_engine::WindowIdentity::HEADLESS;
        }
        REMOTE_IDENTITY
            .get()
            .copied()
            .unwrap_or(unterm_engine::WindowIdentity::HEADLESS)
    }

    /// True only while a window is attached. Between windows this host
    /// exists but can reach nobody, and the confirmation gate has to
    /// know the difference.
    fn can_prompt(&self) -> bool {
        host_channel().is_attached()
    }

    fn request_repaint(&self) {
        host_channel().notify("request_repaint", serde_json::Value::Null);
    }

    /// Forward to the attached front end, which is where windows live.
    ///
    /// A call rather than a notify, because the answer is the new window's
    /// id and only the front end may invent one. A Core allocating its own
    /// would collide the first time a person opened a window while an agent
    /// opened another. The front end reserves the number and returns it at
    /// once -- it does not wait for the window, which can only be built on
    /// the event loop.
    fn open_window_on(
        &self,
        cwd: Option<std::path::PathBuf>,
        profile: Option<String>,
        command: Vec<String>,
    ) -> Option<u64> {
        if !self.can_prompt() {
            return None;
        }
        // The ask travels with the call: a Core-mediated `unterm start --cwd`
        // has to reach the front end with its directory intact, or it opens a
        // window on the wrong folder.
        let mut params = serde_json::Map::new();
        if let Some(cwd) = cwd {
            params.insert("cwd".into(), cwd.display().to_string().into());
        }
        if let Some(profile) = profile {
            params.insert("profile".into(), profile.into());
        }
        if !command.is_empty() {
            params.insert("command".into(), command.into());
        }
        host_channel()
            .call("open_window", serde_json::Value::Object(params), HOST_QUICK)
            .ok()
            .and_then(|value| value.get("window_id").and_then(|id| id.as_u64()))
    }

    fn list_windows(&self) -> Vec<unterm_engine::WindowSummary> {
        if !self.can_prompt() {
            return Vec::new();
        }
        host_channel()
            .call("list_windows", serde_json::Value::Null, HOST_QUICK)
            .ok()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default()
    }

    fn ask_confirmation(
        &self,
        request: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        // Waits longer than the question itself does, so the window's
        // own timeout is what decides -- one clock, at the end that can
        // see the banner. A margin shorter than that would report "the
        // window is not answering" while it was still on screen.
        let window_timeout = request
            .get("timeout_ms")
            .and_then(|value| value.as_u64())
            .unwrap_or(30_000);
        host_channel().call(
            "ask_confirmation",
            request.clone(),
            Duration::from_millis(window_timeout) + Duration::from_secs(10),
        )
    }

    fn set_window_title(&self, title: Option<&str>) -> bool {
        host_channel()
            .call(
                "set_window_title",
                serde_json::json!({ "title": title }),
                HOST_QUICK,
            )
            .ok()
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    }

    fn focus_window(&self, window_id: Option<u64>) -> Result<u64> {
        let raised = host_channel().call(
            "focus_window",
            serde_json::json!({ "window_id": window_id }),
            HOST_QUICK,
        )?;
        raised
            .get("window_id")
            .and_then(|id| id.as_u64())
            .context("the front end raised a window but did not say which")
    }

    fn key_assignments(&self) -> Vec<serde_json::Value> {
        host_channel()
            .call("key_assignments", serde_json::Value::Null, HOST_QUICK)
            .ok()
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default()
    }

    fn capture_region(
        &self,
        left: i32,
        top: i32,
        width: usize,
        height: usize,
        include_base64: bool,
    ) -> Result<serde_json::Value> {
        host_channel().call(
            "capture_region",
            serde_json::json!({
                "left": left, "top": top, "width": width,
                "height": height, "include_base64": include_base64,
            }),
            HOST_SLOW,
        )
    }

    fn capture_own_window(
        &self,
        title: Option<&str>,
        pid: Option<u32>,
        include_base64: bool,
    ) -> Result<serde_json::Value> {
        host_channel().call(
            "capture_own_window",
            serde_json::json!({
                "title": title, "pid": pid, "include_base64": include_base64,
            }),
            HOST_SLOW,
        )
    }

    fn capture_external_window(
        &self,
        request: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        host_channel().call("capture_external_window", request.clone(), HOST_SLOW)
    }

    fn render_scrollback_png(
        &self,
        pane_id: Option<usize>,
        path: &std::path::Path,
        max_rows: usize,
        dpi: usize,
    ) -> Result<serde_json::Value> {
        // The path crosses the process boundary rather than the pixels:
        // Core and window are the same user on the same machine, and a
        // tall scrollback render is megabytes that would otherwise be
        // base64'd through a JSON line for no reason.
        host_channel().call(
            "render_scrollback_png",
            serde_json::json!({
                "pane_id": pane_id,
                "path": path.display().to_string(),
                "max_rows": max_rows,
                "dpi": dpi,
            }),
            HOST_SLOW,
        )
    }
}

/// What a front end must be able to do when the Core asks.
///
/// Deliberately expressed as raw JSON rather than the `McpHost` trait:
/// this crate is below the MCP surface, and the set of things worth
/// asking a window will keep growing. An unknown method is answered with
/// an error, never a panic -- a Core newer than its window must degrade,
/// not crash it.
pub trait HostResponder: Send + Sync {
    fn respond(&self, method: &str, params: &serde_json::Value) -> Result<serde_json::Value>;
}

/// The front end's end of the reverse channel.
///
/// Owns its own connection and a thread that serves calls from the Core
/// until either side goes away. Dropping it detaches.
pub struct HostChannelClient {
    stopping: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl HostChannelClient {
    /// Register this process as the Core's front end and start serving.
    pub fn attach<A: ToSocketAddrs>(
        address: A,
        token: impl Into<String>,
        responder: Arc<dyn HostResponder>,
    ) -> Result<Self> {
        let token = token.into();
        let mut client = CoreClient::connect(address, token)?;
        let _: Response<serde_json::Value> = client.request("core.host")?;
        let stream = client.into_stream();
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .ok();
        let mut writer = stream.try_clone().context("clone host channel stream")?;
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = stopping.clone();
        let worker = std::thread::Builder::new()
            .name("core-host-client".into())
            .spawn(move || {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                while !worker_stopping.load(Ordering::Acquire) {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(_) => {
                            let Ok(frame) = serde_json::from_str::<HostCall>(line.trim()) else {
                                continue;
                            };
                            let call = frame.host_call;
                            // Every call is answered, including the ones
                            // that fail: a Core left waiting on a silent
                            // window is the failure mode this whole
                            // channel exists to avoid.
                            let reply = match responder.respond(&call.method, &call.params) {
                                Ok(result) => serde_json::json!({"host_reply": {
                                    "id": call.id, "ok": true, "result": result,
                                }}),
                                Err(err) => serde_json::json!({"host_reply": {
                                    "id": call.id, "ok": false, "error": format!("{err:#}"),
                                }}),
                            };
                            if writeln!(writer, "{reply}").is_err() || writer.flush().is_err() {
                                break;
                            }
                        }
                        Err(err)
                            if matches!(
                                err.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(_) => break,
                    }
                }
            })
            .context("spawn host channel client")?;
        Ok(Self {
            stopping,
            worker: Some(worker),
        })
    }
}

impl Drop for HostChannelClient {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The Core-backed engine a GUI or CLI holds in place of a local
/// `NextCoreEngine`. Implements the same engine traits, but every call
/// crosses the authenticated IPC boundary, so sessions live in the Core
/// process and survive this process exiting.
///
/// One TCP connection serves all callers; the mutex makes each
/// request/response pair atomic, so concurrent threads can never
/// interleave one request's frame with another's reply.
pub struct CoreEngineClient {
    inner: std::sync::Mutex<CoreConnection>,
}

/// The live connection plus the identity of the Core on the other end.
///
/// The pid is what makes reconnection safe to build on: it is how this
/// client tells "the Core died and a new one took its place" from "the
/// socket hiccupped but the same Core is still there".
struct CoreConnection {
    client: CoreClient,
    core_pid: u32,
}

impl CoreEngineClient {
    /// Connect and complete the version handshake. Version skew is a
    /// hard error here for the same reason it is in `ensure_running`.
    pub fn connect<A: ToSocketAddrs>(address: A, token: impl Into<String>) -> Result<Self> {
        let mut client = CoreClient::connect(address, token)?;
        let identity = client.handshake()?;
        Ok(Self {
            inner: std::sync::Mutex::new(CoreConnection {
                client,
                core_pid: identity.pid,
            }),
        })
    }

    fn call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T> {
        let mut connection = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("unterm-core client lock poisoned"))?;
        // This connection has no read timeout, so an `Err` here is a
        // genuine broken connection rather than a slow reply.
        let response: Response<T> = match connection
            .client
            .request_with_params(method, params.clone())
        {
            Ok(response) => response,
            Err(lost) => Self::recover(&mut connection, method, params, lost)?,
        };
        if let Some(error) = response.error {
            anyhow::bail!("unterm-core {method} failed ({}): {}", error.code, error.message);
        }
        response
            .result
            .ok_or_else(|| anyhow::anyhow!("unterm-core {method} returned no result"))
    }

    /// Re-establish the connection after it broke, and replay the lost
    /// request only when replaying it is provably safe.
    ///
    /// A Core that crashed and came back has a **new port and a new
    /// token**, so redialling the address this client was built with is
    /// useless -- rediscovery is the only way home, which is what
    /// `ensure_running` does (starting a Core if none is left).
    ///
    /// Whether to replay turns on which Core answered. A different pid
    /// cannot have applied the request that was lost, so replaying it
    /// cannot apply it twice. The same live Core might have applied it
    /// already and merely failed to say so -- replaying a `session.write`
    /// there would type the user's command into the shell a second time,
    /// so that case surfaces the original error and lets the caller
    /// decide. Either way the connection is healed for the next call.
    fn recover<T: for<'de> Deserialize<'de>>(
        connection: &mut CoreConnection,
        method: &str,
        params: serde_json::Value,
        lost: anyhow::Error,
    ) -> Result<Response<T>> {
        if is_shutting_down() {
            return Err(lost.context(format!(
                "unterm-core {method} was interrupted while this process was \
                 quitting; the connection was not re-established, because \
                 reconnecting here would start the Core we just stopped"
            )));
        }
        let info = ensure_running().context("reconnect to unterm-core")?;
        let mut fresh = CoreClient::connect(&info.endpoint, &info.token)?;
        let identity = fresh.handshake()?;
        let replaced_a_dead_core = identity.pid != connection.core_pid;
        connection.client = fresh;
        connection.core_pid = identity.pid;

        if !replaced_a_dead_core {
            return Err(lost.context(format!(
                "unterm-core {method} was interrupted; the connection has been \
                 re-established but the request was not replayed, because the \
                 same Core may already have applied it"
            )));
        }
        log::warn!(
            "unterm-core replaced (now pid {}); replaying {method} against the new process",
            identity.pid
        );
        connection.client.request_with_params(method, params)
    }

    /// The agent-facing state the window draws: suggestions posted over
    /// MCP, the Insights numbers, and the audit trail. All of it lives
    /// wherever the MCP surface does, which after the move is here.
    pub fn pending_suggestions(
        &self,
        pane_id: u64,
    ) -> Result<Vec<unterm_mcp::handler::Suggestion>> {
        self.call("mcp.suggestions", serde_json::json!({"pane_id": pane_id}))
    }

    pub fn accept_suggestion(&self, id: &str, run: bool) -> Result<String> {
        self.call(
            "mcp.accept_suggestion",
            serde_json::json!({"id": id, "run": run}),
        )
    }

    pub fn dismiss_suggestion(&self, id: &str) -> Result<()> {
        self.call_unit("mcp.dismiss_suggestion", serde_json::json!({"id": id}))
    }

    pub fn insights(
        &self,
        recent_audit_limit: usize,
    ) -> Result<unterm_mcp::handler::InsightsMcpSnapshot> {
        self.call(
            "mcp.insights",
            serde_json::json!({"recent_audit_limit": recent_audit_limit}),
        )
    }

    pub fn audit_gui_write(&self, method: &str, pane_id: u64, detail: &str) -> Result<()> {
        self.call_unit(
            "mcp.audit_gui_write",
            serde_json::json!({"method": method, "pane_id": pane_id, "detail": detail}),
        )
    }

    /// The pid of the Core currently on the other end. Changes when a
    /// dead Core is replaced, which is how a caller notices its sessions
    /// are gone even though the client still works.
    pub fn core_pid(&self) -> Option<u32> {
        self.inner.lock().ok().map(|connection| connection.core_pid)
    }

    fn call_unit(&self, method: &str, params: serde_json::Value) -> Result<()> {
        let _: serde_json::Value = self.call(method, params)?;
        Ok(())
    }

    /// Mirror of `NextCoreEngine::resize_session_settled`: the resize a
    /// window sends while laying itself out, whose shrinks wait to be sure.
    ///
    /// A Core too old to know the flag ignores it and resizes at once, which
    /// is what it always did.
    pub fn resize_session_settled(&self, pane_id: usize, cols: usize, rows: usize) -> Result<()> {
        self.call_unit(
            "session.resize",
            serde_json::json!({"pane_id": pane_id, "cols": cols, "rows": rows, "settle": true}),
        )
    }

    /// Mirror of `NextCoreEngine::pane_modes`, which the GUI needs per
    /// frame to decide who owns a mouse click.
    pub fn pane_modes(&self, pane_id: usize) -> Result<PaneModesSnapshot> {
        self.call("session.modes", serde_json::json!({"pane_id": pane_id}))
    }

    /// Mirror of `NextCoreEngine::screen_revision`: the cheap "anything
    /// new?" probe a render loop asks between frames.
    pub fn screen_revision(&self, pane_id: usize) -> Result<u64> {
        self.call("session.revision", serde_json::json!({"pane_id": pane_id}))
    }

    pub fn scroll_viewport_to(&self, pane_id: usize, target: isize) -> Result<()> {
        self.call_unit(
            "session.scroll_to",
            serde_json::json!({"pane_id": pane_id, "target": target as i64}),
        )
    }

    pub fn scroll_viewport_by(&self, pane_id: usize, delta: isize) -> Result<()> {
        self.call_unit(
            "session.scroll_by",
            serde_json::json!({"pane_id": pane_id, "delta": delta as i64}),
        )
    }

    pub fn scroll_viewport_to_prompt(&self, pane_id: usize, amount: isize) -> Result<()> {
        self.call_unit(
            "session.scroll_to_prompt",
            serde_json::json!({"pane_id": pane_id, "amount": amount as i64}),
        )
    }

    pub fn report_mouse(&self, pane_id: usize, event: MouseEvent) -> Result<()> {
        self.call_unit("session.report_mouse", mouse_event_params(pane_id, event))
    }

    /// Refuse new sessions; the ones running carry on. With
    /// `exit_when_idle` the Core also stops once they have ended --
    /// which is what a person means by "drain, then exit".
    pub fn drain(&self, exit_when_idle: bool) -> Result<()> {
        self.call_unit(
            "core.drain",
            serde_json::json!({ "exit_when_idle": exit_when_idle }),
        )
    }

    /// Stop the Core now, ending every session it holds.
    pub fn shutdown(&self) -> Result<()> {
        self.call_unit("core.shutdown", serde_json::Value::Null)
    }

    /// Set the scrollback capacity for sessions the Core creates from
    /// now on. A client passes its own configured value along right
    /// after connecting; the config file lives client-side.
    pub fn set_new_session_scrollback_lines(&self, lines: usize) -> Result<()> {
        self.call_unit(
            "core.set_scrollback_lines",
            serde_json::json!({"lines": lines}),
        )
    }

    /// Styled screen, but only when it moved past `since_revision`.
    /// `Ok(None)` means the caller's copy is already current — the
    /// server skips serializing the cell grid entirely for that case.
    pub fn styled_frame(
        &self,
        pane_id: usize,
        since_revision: Option<u64>,
    ) -> Result<Option<StyledScreenSnapshot>> {
        let envelope: StyledFrameEnvelope = self.call(
            "session.styled_frame",
            serde_json::json!({"pane_id": pane_id, "since_revision": since_revision}),
        )?;
        Ok(envelope.screen)
    }
}

#[derive(Debug, Deserialize)]
struct StyledFrameEnvelope {
    #[allow(dead_code)]
    unchanged: bool,
    #[allow(dead_code)]
    revision: u64,
    screen: Option<StyledScreenSnapshot>,
}

impl RecordingEngine for CoreEngineClient {
    fn start_recording(&self, pane_id: usize) -> Result<RecordingStartResult> {
        self.call(
            "session.recording_start",
            serde_json::json!({"pane_id": pane_id}),
        )
    }

    fn stop_recording(&self, pane_id: usize) -> Result<RecordingStopResult> {
        self.call(
            "session.recording_stop",
            serde_json::json!({"pane_id": pane_id}),
        )
    }

    fn recording_status(&self, pane_id: usize) -> Result<RecordingStatusSnapshot> {
        self.call(
            "session.recording_status",
            serde_json::json!({"pane_id": pane_id}),
        )
    }

    fn attach_recording_trace(&self, pane_id: usize, trace_id: String) -> Result<Vec<String>> {
        self.call(
            "session.recording_attach_trace",
            serde_json::json!({"pane_id": pane_id, "trace_id": trace_id}),
        )
    }

    fn export_markdown(
        &self,
        pane_id: usize,
        target_path: Option<String>,
    ) -> Result<RecordingExportResult> {
        self.call(
            "session.recording_export",
            serde_json::json!({"pane_id": pane_id, "target_path": target_path}),
        )
    }
}

impl HealthEngine for CoreEngineClient {
    fn health(&self) -> Result<EngineHealthSnapshot> {
        self.call("core.engine_health", serde_json::Value::Null)
    }
}

fn command_argv(command: &Option<CommandBuilder>) -> Option<Vec<String>> {
    command.as_ref().map(|command| {
        command
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    })
}

impl SessionEngine for CoreEngineClient {
    fn list_sessions(&self) -> Result<Vec<SessionSnapshot>> {
        self.call("session.list", serde_json::Value::Null)
    }

    fn get_session(&self, pane_id: usize) -> Result<SessionSnapshot> {
        self.call("session.get", serde_json::json!({"pane_id": pane_id}))
    }

    fn create_session(&self, request: CreateSessionRequest) -> Result<SessionSnapshot> {
        let cwd = request.command_dir.clone().or_else(|| {
            request
                .command
                .as_ref()
                .and_then(|command| command.get_cwd())
                .map(|dir| dir.to_string_lossy().into_owned())
        });
        self.call(
            "session.create",
            serde_json::json!({
                "cols": request.cols,
                "rows": request.rows,
                "cwd": cwd,
                "argv": command_argv(&request.command),
                "env": request.env,
                "launch_policy": request.launch_policy,
            }),
        )
    }

    fn split_session(&self, request: SplitSessionRequest) -> Result<SessionSnapshot> {
        let direction = match request.direction {
            SplitDirection::Right => "right",
            SplitDirection::Left => "left",
            SplitDirection::Down => "down",
            SplitDirection::Up => "up",
        };
        self.call(
            "session.split",
            serde_json::json!({
                "pane_id": request.source_pane_id,
                "direction": direction,
                "size_percent": request.size_percent,
                "cwd": request.command_dir,
                "argv": command_argv(&request.command),
                "env": request.env,
                "launch_policy": request.launch_policy,
            }),
        )
    }

    fn focus_session(&self, pane_id: usize) -> Result<()> {
        self.call_unit("session.focus", serde_json::json!({"pane_id": pane_id}))
    }

    fn shell(&self, pane_id: usize) -> Result<ShellSnapshot> {
        self.call("session.shell", serde_json::json!({"pane_id": pane_id}))
    }

    fn activity(&self, pane_id: usize) -> Result<SessionActivitySnapshot> {
        self.call("session.activity", serde_json::json!({"pane_id": pane_id}))
    }

    fn resize_session(&self, pane_id: usize, cols: usize, rows: usize) -> Result<()> {
        self.call_unit(
            "session.resize",
            serde_json::json!({"pane_id": pane_id, "cols": cols, "rows": rows}),
        )
    }

    fn destroy_session(&self, pane_id: usize) -> Result<()> {
        self.call_unit("session.close", serde_json::json!({"pane_id": pane_id}))
    }

    fn set_split_ratio(&self, pane_id: usize, first_ratio: f64) -> Result<()> {
        self.call_unit(
            "session.set_split_ratio",
            serde_json::json!({"pane_id": pane_id, "first_ratio": first_ratio}),
        )
    }
}

impl ScreenEngine for CoreEngineClient {
    fn read_screen(&self, pane_id: usize) -> Result<ScreenSnapshot> {
        self.call("session.screen", serde_json::json!({"pane_id": pane_id}))
    }

    fn erase_scrollback(&self, pane_id: usize, include_viewport: bool) -> Result<()> {
        self.call_unit(
            "session.erase_scrollback",
            serde_json::json!({"pane_id": pane_id, "include_viewport": include_viewport}),
        )
    }

    fn read_styled_screen(&self, pane_id: usize) -> Result<StyledScreenSnapshot> {
        self.call("session.styled_screen", serde_json::json!({"pane_id": pane_id}))
    }

    fn read_pane_pulse(
        &self,
        pane_id: usize,
        since_revision: Option<u64>,
        tail_rows: usize,
    ) -> Result<unterm_engine::PanePulse> {
        self.call(
            "session.pane_pulse",
            serde_json::json!({
                "pane_id": pane_id,
                "since_revision": since_revision,
                "tail_rows": tail_rows,
            }),
        )
    }

    fn read_render_frame(
        &self,
        pane_id: usize,
        since_revision: Option<u64>,
    ) -> Result<RenderFrameSnapshot> {
        self.call(
            "session.frame",
            serde_json::json!({"pane_id": pane_id, "since_revision": since_revision}),
        )
    }

    fn read_visible_text(&self, pane_id: usize) -> Result<String> {
        self.call("session.visible_text", serde_json::json!({"pane_id": pane_id}))
    }

    fn read_lines(&self, pane_id: usize, start: i64, count: usize) -> Result<Vec<ScreenLine>> {
        self.call(
            "session.lines",
            serde_json::json!({"pane_id": pane_id, "start": start, "count": count}),
        )
    }

    fn read_scrollback(&self, pane_id: usize, limit: usize) -> Result<Vec<String>> {
        self.call(
            "session.scrollback",
            serde_json::json!({"pane_id": pane_id, "limit": limit}),
        )
    }

    fn read_scrollback_text(
        &self,
        pane_id: usize,
        request: ScrollbackTextRequest,
    ) -> Result<ScrollbackTextSnapshot> {
        self.call(
            "session.scrollback_text",
            scrollback_request_params(pane_id, &request),
        )
    }

    fn read_styled_scrollback(
        &self,
        pane_id: usize,
        request: ScrollbackTextRequest,
    ) -> Result<StyledScrollbackSnapshot> {
        self.call(
            "session.styled_scrollback",
            scrollback_request_params(pane_id, &request),
        )
    }

    fn search(
        &self,
        pane_id: usize,
        pattern: &str,
        mode: SearchMode,
        max_results: usize,
    ) -> Result<Vec<ScreenSearchMatch>> {
        let mode = match mode {
            SearchMode::CaseSensitive => "case_sensitive",
            SearchMode::CaseInsensitive => "case_insensitive",
        };
        self.call(
            "session.search",
            serde_json::json!({
                "pane_id": pane_id,
                "pattern": pattern,
                "mode": mode,
                "max_results": max_results,
            }),
        )
    }

    fn cursor(&self, pane_id: usize) -> Result<CursorSnapshot> {
        self.call("session.cursor", serde_json::json!({"pane_id": pane_id}))
    }
}

impl InputEngine for CoreEngineClient {
    fn write_input(&self, pane_id: usize, input: &str) -> Result<()> {
        self.call_unit(
            "session.write",
            serde_json::json!({"pane_id": pane_id, "data": input}),
        )
    }

    fn paste_input(&self, pane_id: usize, text: &str) -> Result<()> {
        self.call_unit(
            "session.paste",
            serde_json::json!({"pane_id": pane_id, "data": text}),
        )
    }
}

/// Event-driven client-side cache of styled screens.
///
/// The benchmark that shaped this: a full styled screen over IPC costs
/// ~5ms; the GUI reads it 20+ times per frame today. A renderer
/// therefore must never fetch per read. This cache subscribes to
/// `core.events`, refetches a pane only when the Core says its screen
/// moved, and serves every read from local memory at clone cost.
pub struct FrameCache {
    inner: Arc<FrameCacheInner>,
    worker: Option<std::thread::JoinHandle<()>>,
}

struct FrameCacheInner {
    frames: std::sync::RwLock<std::collections::HashMap<usize, StyledScreenSnapshot>>,
    generation: std::sync::atomic::AtomicU64,
    stopping: AtomicBool,
    /// False once the event feed ends for a reason other than being
    /// asked to stop -- which is how a client learns the Core died.
    /// Without it the cached frames simply stop changing, and a
    /// frozen terminal with no explanation is the worst way to
    /// report a crash.
    live: AtomicBool,
    /// Counts how many times this cache has attached to a *replacement*
    /// Core. A holder of pane ids compares it against what it last saw:
    /// a change means every id it remembers belonged to a process that
    /// no longer exists, and it must resync rather than keep drawing
    /// tabs with nothing behind them.
    epoch: std::sync::atomic::AtomicU64,
    client: CoreEngineClient,
    /// Called after every cache change, off the caller's thread. A GUI
    /// hangs its wake-the-event-loop hook here so a screen update
    /// becomes a redraw now, not at the next timer tick.
    notify: Option<Box<dyn Fn() + Send + Sync>>,
}

impl FrameCacheInner {
    fn refresh(&self, pane_id: usize) {
        let since = self
            .frames
            .read()
            .expect("frame cache lock poisoned")
            .get(&pane_id)
            .map(|screen| screen.revision);
        // A pane can die between the event and this fetch; that is the
        // close event's job to clean up, not an error worth surfacing.
        if let Ok(Some(screen)) = self.client.styled_frame(pane_id, since) {
            self.frames
                .write()
                .expect("frame cache lock poisoned")
                .insert(pane_id, screen);
            self.bump();
        }
    }

    fn evict(&self, pane_id: usize) {
        if self
            .frames
            .write()
            .expect("frame cache lock poisoned")
            .remove(&pane_id)
            .is_some()
        {
            self.bump();
        }
    }

    fn bump(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if let Some(notify) = &self.notify {
            notify();
        }
    }

    /// Replace everything cached with what the Core has now.
    ///
    /// Used after a reconnect. The panes held before belonged to a
    /// process that no longer exists -- their pids, their scrollback and
    /// their shells all went with it -- so they are dropped rather than
    /// merged. Keeping them would leave the window drawing terminals
    /// that nothing is behind.
    fn adopt_current_sessions(&self) {
        let sessions = self.client.list_sessions().unwrap_or_default();
        self.frames
            .write()
            .expect("frame cache lock poisoned")
            .clear();
        for session in sessions {
            self.refresh(session.id);
        }
        self.bump();
    }
}

/// Keep looking for a Core to subscribe to, until one answers or this
/// cache is dropped.
///
/// Backs off to a second between tries: a Core that is coming back needs
/// a moment to bind and publish its discovery record, and a client that
/// spins on it during that window costs more than it gains. Returns
/// `None` only when asked to stop, so the caller can end the worker.
fn reconnect_events(inner: &FrameCacheInner) -> Option<CoreEventStream> {
    let mut backoff = Duration::from_millis(100);
    while !inner.stopping.load(Ordering::Acquire) && !is_shutting_down() {
        if let Ok(info) = ensure_running() {
            if let Ok(feed) = CoreEventStream::connect(&info.endpoint, &info.token) {
                if feed
                    .set_read_timeout(Some(Duration::from_millis(200)))
                    .is_ok()
                {
                    log::warn!("unterm-core event feed re-established (pid {})", info.pid);
                    return Some(feed);
                }
            }
        }
        // Slept in slices so a drop during the backoff is noticed
        // promptly rather than after the whole interval.
        let deadline = std::time::Instant::now() + backoff;
        while std::time::Instant::now() < deadline {
            if inner.stopping.load(Ordering::Acquire) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        backoff = (backoff * 2).min(Duration::from_secs(1));
    }
    None
}

impl FrameCache {
    /// Connect, seed from `session.list`, and start the update worker.
    /// Subscription begins before seeding, so a screen that changes
    /// mid-seed is refetched rather than missed.
    pub fn start<A: ToSocketAddrs + Clone>(address: A, token: impl Into<String>) -> Result<Self> {
        Self::start_inner(address, token.into(), None)
    }

    /// Like `start`, plus a hook called after every cache change (from
    /// the worker thread). Keep it cheap and non-blocking: it runs on
    /// the same thread that applies updates.
    pub fn start_with_notify<A: ToSocketAddrs + Clone>(
        address: A,
        token: impl Into<String>,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self> {
        Self::start_inner(address, token.into(), Some(Box::new(notify)))
    }

    fn start_inner<A: ToSocketAddrs + Clone>(
        address: A,
        token: String,
        notify: Option<Box<dyn Fn() + Send + Sync>>,
    ) -> Result<Self> {
        let client = CoreEngineClient::connect(address.clone(), token.clone())?;
        let mut events = CoreEventStream::connect(address, token)?;
        // The worker must notice `stopping` even when the feed is
        // quiet; the buffered framing keeps timed-out reads lossless.
        events.set_read_timeout(Some(Duration::from_millis(200)))?;
        let inner = Arc::new(FrameCacheInner {
            frames: std::sync::RwLock::new(std::collections::HashMap::new()),
            generation: std::sync::atomic::AtomicU64::new(0),
            stopping: AtomicBool::new(false),
            live: AtomicBool::new(true),
            epoch: std::sync::atomic::AtomicU64::new(0),
            client,
            notify,
        });
        if let Ok(sessions) = inner.client.list_sessions() {
            for session in sessions {
                inner.refresh(session.id);
            }
        }
        let worker_inner = inner.clone();
        let worker = std::thread::Builder::new()
            .name("frame-cache".into())
            .spawn(move || loop {
                if worker_inner.stopping.load(Ordering::Acquire) {
                    break;
                }
                match events.next_event() {
                    Ok(Some(CoreEvent::SessionCreated { pane_id }))
                    | Ok(Some(CoreEvent::ScreenUpdated { pane_id, .. })) => {
                        worker_inner.refresh(pane_id)
                    }
                    Ok(Some(CoreEvent::SessionClosed { pane_id })) => {
                        worker_inner.evict(pane_id)
                    }
                    Ok(Some(CoreEvent::SessionDead { .. }))
                    | Ok(Some(CoreEvent::Draining)) => {}
                    // A read timeout is the loop's own heartbeat, not
                    // a failure: it is how this thread gets to check
                    // whether it has been asked to stop. It has to be
                    // matched before the catch-all below, or an idle
                    // feed reads as a dead Core within 200ms.
                    Err(err) if is_timeout_error(&err) => continue,
                    // The feed ended without being asked to. The Core
                    // is gone -- say so at once, rather than letting the
                    // last frames sit there looking like a live
                    // terminal, and then go looking for its replacement.
                    Ok(None) | Err(_) => {
                        if worker_inner.stopping.load(Ordering::Acquire) {
                            break;
                        }
                        worker_inner.live.store(false, Ordering::Release);
                        worker_inner.bump();
                        match reconnect_events(&worker_inner) {
                            Some(feed) => {
                                events = feed;
                                // The old panes died with the old Core.
                                // Adopt whatever the new one has instead
                                // of leaving corpses in the cache.
                                worker_inner.adopt_current_sessions();
                                worker_inner.live.store(true, Ordering::Release);
                                // Announced after the cache is already
                                // consistent, so a reader woken by this
                                // never sees the half-adopted state.
                                worker_inner.epoch.fetch_add(1, Ordering::AcqRel);
                                worker_inner.bump();
                            }
                            None => break,
                        }
                    }
                }
            })
            .expect("spawn frame cache worker");
        Ok(Self {
            inner,
            worker: Some(worker),
        })
    }

    /// Serve a styled screen from local memory. `None` is a genuine
    /// miss (unknown pane) — the caller decides whether to fall back
    /// to a direct fetch.
    pub fn styled_screen(&self, pane_id: usize) -> Option<StyledScreenSnapshot> {
        self.inner
            .frames
            .read()
            .expect("frame cache lock poisoned")
            .get(&pane_id)
            .cloned()
    }

    /// Refresh one pane immediately, bypassing the event-feed cadence.
    ///
    /// Startup is the only caller today: the Core event watcher can be in its
    /// no-subscriber sleep when the first window appears, and waiting for that
    /// sleep makes the first prompt look slower than the shell actually is.
    pub fn refresh_pane(&self, pane_id: usize) {
        self.inner.refresh(pane_id);
    }

    pub fn panes(&self) -> Vec<usize> {
        self.inner
            .frames
            .read()
            .expect("frame cache lock poisoned")
            .keys()
            .copied()
            .collect()
    }

    /// Whether the Core is still there.
    ///
    /// False once its event feed ends unbidden: the sessions this
    /// cache holds frames for no longer exist, and whoever is drawing
    /// them has to stop claiming they do.
    pub fn is_live(&self) -> bool {
        self.inner.live.load(Ordering::Acquire)
    }

    /// How many times this cache has attached to a replacement Core.
    ///
    /// Zero for the Core it started with. Every increment means the
    /// previous process died and its panes went with it, so anything
    /// holding pane ids has to throw them away and ask again.
    pub fn epoch(&self) -> u64 {
        self.inner.epoch.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Monotonic change counter: a renderer that saw the same value
    /// twice knows nothing on screen moved and can skip the frame.
    pub fn generation(&self) -> u64 {
        self.inner
            .generation
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

impl Drop for FrameCache {
    fn drop(&mut self) {
        self.inner.stopping.store(true, Ordering::Release);
        // The worker wakes within its read timeout, sees the flag and
        // exits. No socket shutdown games: Windows would not unblock
        // an in-flight recv anyway.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn scrollback_request_params(
    pane_id: usize,
    request: &ScrollbackTextRequest,
) -> serde_json::Value {
    serde_json::json!({
        "pane_id": pane_id,
        "start_line": request.start_line,
        "end_line": request.end_line,
        "tail_lines": request.tail_lines,
        "escapes": request.escapes,
    })
}

/// Whether the Core was already up, or this process had to start it.
///
/// A front end that started the Core is the only one that may stop it. The
/// Core is spawned detached on purpose -- closing a window must not end the
/// shells behind it -- so nothing else ever takes it down, and one that was
/// already running belongs to whoever started it, including a `--headless`
/// one the user launched deliberately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreArrival {
    /// Found one already running and attached to it.
    Attached,
    /// None was running, so this process started it.
    Started,
}

/// Set once this process has decided to quit.
///
/// From that moment a broken connection means the Core *this process just
/// stopped*, not a Core that crashed -- and the difference matters, because
/// both recovery paths heal a crash by calling `ensure_running`, which
/// starts a Core when none is left. During teardown that turns into a race
/// the front end loses: it stops its Core, its own poll worker sees the
/// connection drop a moment later and starts a fresh one, and that new Core
/// outlives the process that was trying to take it down. On Windows the
/// orphan then holds `unterm-core.exe` open and the next install cannot
/// replace it.
static SHUTTING_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Close the gate: from here on, a lost connection is expected and must not
/// be answered by starting another Core. One-way on purpose -- a process
/// that has begun quitting does not come back.
pub fn begin_shutdown() {
    SHUTTING_DOWN.store(true, std::sync::atomic::Ordering::Release);
}

/// Whether `begin_shutdown` has been called in this process.
pub fn is_shutting_down() -> bool {
    SHUTTING_DOWN.load(std::sync::atomic::Ordering::Acquire)
}

/// Ensure the per-user Core process is available and return its
/// discovery record. GUI, CLI and MCP entry points share this path.
pub fn ensure_running() -> Result<DiscoveryInfo> {
    ensure_running_reporting_arrival().map(|(info, _)| info)
}

/// `ensure_running`, plus whether this call is the one that started it.
///
/// Split out rather than changing `ensure_running`'s signature: three of its
/// four callers have no use for the answer, and a returned tuple they all
/// have to destructure is worse than one extra entry point for the caller
/// that does.
pub fn ensure_running_reporting_arrival() -> Result<(DiscoveryInfo, CoreArrival)> {
    if let Some(info) = read_discovery()? {
        if unterm_services::server_info::pid_alive(info.pid) {
            if let Ok(mut client) = CoreClient::connect(&info.endpoint, &info.token) {
                // Version skew is a hard error: do not fall through and
                // spawn a second core alongside a live-but-incompatible one.
                client.handshake()?;
                let health: Response<serde_json::Value> = client.request("core.health")?;
                if health.ok {
                    return Ok((info, CoreArrival::Attached));
                }
            }
        } else {
            let _ = clear_discovery();
        }
    }
    let current = std::env::current_exe().context("resolve unterm executable path")?;
    let core_name = if cfg!(windows) {
        "unterm-core.exe"
    } else {
        "unterm-core"
    };
    let core_path = current.with_file_name(core_name);
    let mut command = std::process::Command::new(&core_path);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
        .spawn()
        .with_context(|| format!("start unterm-core at {}", core_path.display()))?;
    for _ in 0..100 {
        if let Some(info) = read_discovery()? {
            if !unterm_services::server_info::pid_alive(info.pid) {
                let _ = clear_discovery();
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            if let Ok(mut client) = CoreClient::connect(&info.endpoint, &info.token) {
                client.handshake()?;
                let health: Response<serde_json::Value> = client.request("core.health")?;
                if health.ok {
                    return Ok((info, CoreArrival::Started));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    anyhow::bail!("unterm-core did not become ready")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start_server(token: &str) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        let server = CoreServer::bind(("127.0.0.1", 0), token).unwrap();
        let endpoint = server.endpoint().unwrap();
        let worker = std::thread::spawn(move || server.run().unwrap());
        std::thread::sleep(Duration::from_millis(20));
        (endpoint, worker)
    }

    /// Answers a fixed set of methods, and records what it was asked.
    struct ProbeResponder {
        seen: std::sync::Mutex<Vec<String>>,
    }

    impl HostResponder for ProbeResponder {
        fn respond(
            &self,
            method: &str,
            params: &serde_json::Value,
        ) -> Result<serde_json::Value> {
            self.seen
                .lock()
                .expect("probe lock poisoned")
                .push(method.to_string());
            match method {
                "window_identity" => Ok(serde_json::json!({
                    "engine": "probe",
                    "window_owner": "probe",
                    "native_window_lifecycle": "probe",
                    "uses_host_window": false,
                })),
                "echo" => Ok(params.clone()),
                "refuse" => anyhow::bail!("the window said no"),
                "silent" => {
                    // Long enough to outlast the caller's timeout.
                    std::thread::sleep(Duration::from_millis(600));
                    Ok(serde_json::Value::Null)
                }
                other => anyhow::bail!("unknown host method {other}"),
            }
        }
    }

    #[test]
    fn a_core_with_no_window_attached_fails_the_call_instead_of_waiting() {
        // The property that keeps a headless Core usable: things only a
        // window can do must be declined at once, not parked until they
        // time out, or every MCP worker thread ends up blocked.
        let channel = HostChannel::default();
        assert!(!channel.is_attached());
        let started = std::time::Instant::now();
        let refused = channel.call("echo", serde_json::json!({}), Duration::from_secs(30));
        assert!(refused.is_err());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a call with no window attached waited {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn the_core_can_call_into_an_attached_front_end() {
        let _guard = host_channel_test_guard();
        let (endpoint, worker) = start_server("host-token");
        let probe = Arc::new(ProbeResponder {
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let attached =
            HostChannelClient::attach(endpoint, "host-token", probe.clone()).unwrap();
        assert!(
            wait_for(Duration::from_secs(5), || host_channel().is_attached()),
            "the core never saw the front end attach"
        );

        let answered = host_channel()
            .call(
                "echo",
                serde_json::json!({"hello": "world"}),
                Duration::from_secs(5),
            )
            .expect("the front end should have answered");
        assert_eq!(answered["hello"], "world");

        // A refusal reaches the caller as a refusal, not as a timeout:
        // "the window said no" and "the window is not answering" are
        // different facts and callers act on them differently.
        let refused = host_channel()
            .call("refuse", serde_json::json!({}), Duration::from_secs(5))
            .expect_err("a refusal should surface as an error");
        assert!(
            refused.to_string().contains("the window said no"),
            "the refusal reason was lost: {refused}"
        );

        // And a method the window does not know is an error, not a hang.
        assert!(host_channel()
            .call("invented", serde_json::json!({}), Duration::from_secs(5))
            .is_err());

        let seen = probe.seen.lock().unwrap().clone();
        // The Core asks who the window is as soon as it attaches, on its
        // own thread -- so it lands somewhere in here, but not at a
        // position worth pinning down.
        assert!(
            seen.contains(&"window_identity".to_string()),
            "the core never asked the new window who it was: {seen:?}"
        );
        let requested: Vec<_> = seen
            .into_iter()
            .filter(|method| method != "window_identity")
            .collect();
        assert_eq!(
            requested,
            ["echo".to_string(), "refuse".to_string(), "invented".to_string()]
        );

        drop(attached);
        assert!(
            wait_for(Duration::from_secs(5), || !host_channel().is_attached()),
            "the core kept believing a detached window was there"
        );
        let _ = worker;
    }

    #[test]
    fn closing_the_current_front_end_restores_the_previous_one() {
        let _guard = host_channel_test_guard();
        let (endpoint, worker) = start_server("host-stack-token");
        let first = Arc::new(ProbeResponder {
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let second = Arc::new(ProbeResponder {
            seen: std::sync::Mutex::new(Vec::new()),
        });

        let first_attached =
            HostChannelClient::attach(endpoint, "host-stack-token", first.clone()).unwrap();
        assert!(
            wait_for(Duration::from_secs(5), || host_channel().is_attached()),
            "the first front end never attached"
        );
        let answered = host_channel()
            .call(
                "echo",
                serde_json::json!({"window": "first"}),
                Duration::from_secs(5),
            )
            .expect("first front end should answer");
        assert_eq!(answered["window"], "first");

        let second_attached =
            HostChannelClient::attach(endpoint, "host-stack-token", second.clone()).unwrap();
        assert!(
            wait_for(Duration::from_secs(5), || {
                second
                    .seen
                    .lock()
                    .unwrap()
                    .contains(&"window_identity".to_string())
            }),
            "the second front end never became current"
        );
        let answered = host_channel()
            .call(
                "echo",
                serde_json::json!({"window": "second"}),
                Duration::from_secs(5),
            )
            .expect("second front end should answer");
        assert_eq!(answered["window"], "second");

        drop(second_attached);
        assert!(
            wait_for(Duration::from_secs(5), || {
                host_channel()
                    .call(
                        "echo",
                        serde_json::json!({"window": "first-again"}),
                        Duration::from_millis(200),
                    )
                    .ok()
                    .and_then(|value| value["window"].as_str().map(str::to_string))
                    == Some("first-again".to_string())
            }),
            "closing the current front end did not restore the previous one"
        );

        drop(first_attached);
        assert!(
            wait_for(Duration::from_secs(5), || !host_channel().is_attached()),
            "the core kept a detached front end"
        );
        let _ = worker;
    }

    #[test]
    fn a_window_that_stops_answering_cannot_hold_a_core_thread() {
        let _guard = host_channel_test_guard();
        let (endpoint, worker) = start_server("slow-token");
        let probe = Arc::new(ProbeResponder {
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let attached = HostChannelClient::attach(endpoint, "slow-token", probe).unwrap();
        assert!(wait_for(Duration::from_secs(5), || host_channel()
            .is_attached()));

        let started = std::time::Instant::now();
        let timed_out = host_channel().call(
            "silent",
            serde_json::json!({}),
            Duration::from_millis(150),
        );
        assert!(timed_out.is_err(), "a silent window should not succeed");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the timeout was not honoured; waited {:?}",
            started.elapsed()
        );
        drop(attached);
        let _ = worker;
    }

    fn host_channel_test_guard() -> std::sync::MutexGuard<'static, ()> {
        static GUARD: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
        let guard = GUARD
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            // A test that fails while holding this poisons it, and every
            // later test then failed on the lock instead of on its own
            // subject -- one real failure reported as two, the second one
            // naming a defect that was not there.
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Serialising these tests does not make the process-global channel
        // empty for them. A server thread detaches when its client's socket
        // closes, which it notices on its own schedule and on the far side
        // of a drop the test cannot observe -- and one test leaves a front
        // end that stops answering on purpose. Either way the leftover
        // attachment makes `is_attached` answer for a window this test
        // never opened, which is how "the core kept a detached front end"
        // failed roughly one run in four.
        wait_for(Duration::from_secs(2), || !host_channel().is_attached());
        host_channel().clear_for_tests();
        guard
    }

    fn wait_for(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn handshake_reports_core_role_and_compatibility() {
        let (endpoint, worker) = start_server("handshake-token");
        let mut client = CoreClient::connect(endpoint, "handshake-token").unwrap();
        let info = client.handshake().unwrap();
        assert_eq!(info.process_role, ProcessRole::Core);
        assert_eq!(info.product_version, unterm_protocol::PRODUCT_VERSION);
        assert!(!info.build_commit.is_empty());
        assert!(!info.started_at.is_empty());
        let _: Response<serde_json::Value> = client.request("core.shutdown").unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn rejects_wrong_token() {
        let (endpoint, worker) = start_server("right-token");
        let mut client = CoreClient::connect(endpoint, "wrong-token").unwrap();
        let info: Response<BuildHandshake> = client.request("core.info").unwrap();
        assert!(!info.ok);
        assert_eq!(info.error.unwrap().code, "unauthenticated");
        let mut owner = CoreClient::connect(endpoint, "right-token").unwrap();
        let _: Response<serde_json::Value> = owner.request("core.shutdown").unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn instance_lock_is_exclusive_and_released_on_drop() {
        let path = std::env::temp_dir().join(format!(
            "unterm-core-lock-test-{}.lock",
            uuid::Uuid::new_v4()
        ));
        let first = try_acquire_instance_lock_at(&path).unwrap();
        assert!(first.is_some(), "first acquire should win the lock");
        let second = try_acquire_instance_lock_at(&path).unwrap();
        assert!(second.is_none(), "second acquire must observe the held lock");
        drop(first);
        let third = try_acquire_instance_lock_at(&path).unwrap();
        assert!(third.is_some(), "lock must be reacquirable after release");
        drop(third);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn session_round_trip_through_core_ipc() {
        let (endpoint, worker) = start_server("session-token");
        let mut client = CoreClient::connect(endpoint, "session-token").unwrap();
        let argv: Vec<&str> = if cfg!(windows) {
            vec!["cmd.exe"]
        } else {
            vec!["sh"]
        };
        let created: Response<serde_json::Value> = client
            .request_with_params(
                "session.create",
                serde_json::json!({"cols": 80, "rows": 24, "argv": argv}),
            )
            .unwrap();
        assert!(created.ok, "session.create failed: {:?}", created.error);
        let pane_id = created.result.unwrap()["id"].as_u64().unwrap();

        let written: Response<serde_json::Value> = client
            .request_with_params(
                "session.write",
                serde_json::json!({"pane_id": pane_id, "data": "echo core-ready\r\n"}),
            )
            .unwrap();
        assert!(written.ok);

        // The shell needs a moment to echo. Poll rather than sleep once.
        let mut saw_output = false;
        for _ in 0..50 {
            let screen: Response<serde_json::Value> = client
                .request_with_params(
                    "session.screen",
                    serde_json::json!({"pane_id": pane_id}),
                )
                .unwrap();
            let result = screen.result.unwrap_or_default();
            let text = result["lines"]
                .as_array()
                .map(|lines| {
                    lines
                        .iter()
                        .filter_map(|line| line.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if text.contains("core-ready") {
                saw_output = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(saw_output, "shell output never reached the core screen");

        let frame: Response<serde_json::Value> = client
            .request_with_params(
                "session.frame",
                serde_json::json!({"pane_id": pane_id}),
            )
            .unwrap();
        let revision = frame.result.unwrap()["revision"].as_u64().unwrap();
        assert!(revision > 0);

        let listed: Response<serde_json::Value> = client.request("session.list").unwrap();
        assert!(listed.ok);

        let closed: Response<serde_json::Value> = client
            .request_with_params(
                "session.close",
                serde_json::json!({"pane_id": pane_id}),
            )
            .unwrap();
        assert!(closed.ok);

        let _: Response<serde_json::Value> = client.request("core.shutdown").unwrap();
        worker.join().unwrap();
    }

    /// The window holds the split tree; the Core outlives the window. A
    /// divider dragged in one has to be readable from the other, or the
    /// arrangement a restart rebuilds is the one the split was created
    /// with rather than the one the user settled on.
    #[test]
    fn a_dragged_divider_reaches_the_core_and_stays_in_the_snapshot() {
        let (endpoint, worker) = start_server("ratio-token");
        let mut client = CoreClient::connect(endpoint, "ratio-token").unwrap();
        let argv: Vec<&str> = if cfg!(windows) {
            vec!["cmd.exe"]
        } else {
            vec!["sh"]
        };
        let created: Response<serde_json::Value> = client
            .request_with_params(
                "session.create",
                serde_json::json!({"cols": 80, "rows": 24, "argv": argv}),
            )
            .unwrap();
        let source = created.result.unwrap()["id"].as_u64().unwrap();

        let split: Response<serde_json::Value> = client
            .request_with_params(
                "session.split",
                serde_json::json!({"pane_id": source, "direction": "right", "size_percent": 50, "argv": argv}),
            )
            .unwrap();
        assert!(split.ok, "session.split failed: {:?}", split.error);
        let split = split.result.unwrap();
        let pane_id = split["id"].as_u64().unwrap();
        assert_eq!(split["split_ratio"].as_f64(), Some(0.5));

        let moved: Response<serde_json::Value> = client
            .request_with_params(
                "session.set_split_ratio",
                serde_json::json!({"pane_id": pane_id, "first_ratio": 0.72}),
            )
            .unwrap();
        assert!(moved.ok, "set_split_ratio failed: {:?}", moved.error);

        // Read it back the way a reopened window would: from the list, not
        // from the reply to our own write.
        let listed: Response<serde_json::Value> = client.request("session.list").unwrap();
        let sessions = listed.result.unwrap();
        let rebuilt = sessions
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["id"].as_u64() == Some(pane_id))
            .expect("the split pane should still be listed");
        assert_eq!(rebuilt["split_ratio"].as_f64(), Some(0.72));
        assert_eq!(rebuilt["split_from"].as_u64(), Some(source));

        let refused: Response<serde_json::Value> = client
            .request_with_params(
                "session.set_split_ratio",
                serde_json::json!({"pane_id": pane_id}),
            )
            .unwrap();
        assert!(!refused.ok, "a ratio-less request must not be accepted");

        // Sessions outlive their client by design, so a test that leaves
        // them behind hands the next one a Core that is not idle.
        for pane in [pane_id, source] {
            let closed: Response<serde_json::Value> = client
                .request_with_params("session.close", serde_json::json!({"pane_id": pane}))
                .unwrap();
            assert!(closed.ok, "session.close failed: {:?}", closed.error);
        }

        let _: Response<serde_json::Value> = client.request("core.shutdown").unwrap();
        worker.join().unwrap();
    }

    fn shell_argv() -> Vec<std::ffi::OsString> {
        if cfg!(windows) {
            vec!["cmd.exe".into()]
        } else {
            vec!["sh".into()]
        }
    }

    fn styled_screen_text(snapshot: &StyledScreenSnapshot) -> String {
        snapshot
            .lines
            .iter()
            .map(|line| line.cells.iter().map(|cell| cell.ch).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn facade_drives_sessions_over_ipc() {
        let (endpoint, worker) = start_server("facade-token");
        let facade = CoreEngineClient::connect(endpoint, "facade-token").unwrap();

        let session = facade
            .create_session(CreateSessionRequest {
                cols: 80,
                rows: 24,
                command_dir: None,
                command: Some(CommandBuilder::from_argv(shell_argv())),
                env: Vec::new(),
                launch_policy: Default::default(),
            })
            .unwrap();
        let pane_id = session.id;

        facade
            .write_input(pane_id, "echo facade-ready\r\n")
            .unwrap();

        let mut styled = None;
        for _ in 0..50 {
            let snapshot = facade.read_styled_screen(pane_id).unwrap();
            if styled_screen_text(&snapshot).contains("facade-ready") {
                styled = Some(snapshot);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut styled = styled.expect("styled screen never showed the echoed text");
        assert!(styled.revision > 0);

        // The echoed text arriving does not mean the shell is done drawing:
        // the prompt repaint can land a beat later, and asserting "unchanged"
        // against a frame still in motion is a coin toss on a slow runner.
        // Wait for two consecutive reads to agree before holding it still.
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(50));
            let again = facade.read_styled_screen(pane_id).unwrap();
            let settled = again.revision == styled.revision;
            styled = again;
            if settled {
                break;
            }
        }

        // The incremental path a renderer relies on: an up-to-date
        // revision must come back empty instead of resending the frame.
        let unchanged = facade
            .read_render_frame(pane_id, Some(styled.revision))
            .unwrap();
        assert!(!unchanged.full);
        assert!(unchanged.lines.is_empty());

        assert!(facade
            .read_visible_text(pane_id)
            .unwrap()
            .contains("facade-ready"));
        let matches = facade
            .search(pane_id, "facade-ready", SearchMode::CaseInsensitive, 10)
            .unwrap();
        assert!(!matches.is_empty());

        let cursor = facade.cursor(pane_id).unwrap();
        assert!(!cursor.shape.is_empty());
        let modes = facade.pane_modes(pane_id).unwrap();
        assert!(!modes.alt_screen_active);
        let shell = facade.shell(pane_id).unwrap();
        assert!(!shell.process_name.is_empty());
        facade.activity(pane_id).unwrap();

        let listed = facade.list_sessions().unwrap();
        assert!(listed.iter().any(|s| s.id == pane_id));
        assert_eq!(facade.get_session(pane_id).unwrap().id, pane_id);

        facade.resize_session(pane_id, 100, 30).unwrap();
        assert_eq!(facade.get_session(pane_id).unwrap().cols, 100);

        facade.destroy_session(pane_id).unwrap();

        let mut owner = CoreClient::connect(endpoint, "facade-token").unwrap();
        let _: Response<serde_json::Value> = owner.request("core.shutdown").unwrap();
        worker.join().unwrap();
    }

    /// A window's shrink waits and can be taken back; anyone else's applies.
    ///
    /// The window marks its resizes with `settle`, because it passes through
    /// sizes it does not mean -- and a shrink it takes back a moment later
    /// had already cut the pane's screen to pieces the program in it never
    /// knew about. An agent asking for a size means it, and gets it at once.
    #[test]
    fn a_window_shrink_settles_but_an_explicit_resize_does_not() {
        let (endpoint, worker) = start_server("settle-token");
        let facade = CoreEngineClient::connect(endpoint, "settle-token").unwrap();
        let pane_id = facade
            .create_session(CreateSessionRequest {
                cols: 120,
                rows: 40,
                command_dir: None,
                command: Some(CommandBuilder::from_argv(shell_argv())),
                env: Vec::new(),
                launch_policy: Default::default(),
            })
            .unwrap()
            .id;

        facade.resize_session_settled(pane_id, 60, 20).unwrap();
        let waiting = facade.get_session(pane_id).unwrap();
        assert_eq!((waiting.cols, waiting.rows), (120, 40), "the shrink waits");
        facade.resize_session_settled(pane_id, 120, 40).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let kept = facade.get_session(pane_id).unwrap();
        assert_eq!((kept.cols, kept.rows), (120, 40), "taken back, it never happened");

        facade.resize_session(pane_id, 60, 20).unwrap();
        let explicit = facade.get_session(pane_id).unwrap();
        assert_eq!((explicit.cols, explicit.rows), (60, 20), "an explicit size applies at once");

        facade.destroy_session(pane_id).unwrap();
        let mut owner = CoreClient::connect(endpoint, "settle-token").unwrap();
        let _: Response<serde_json::Value> = owner.request("core.shutdown").unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn facade_split_creates_adjacent_session_and_drain_blocks_it() {
        let (endpoint, worker) = start_server("split-token");
        let facade = CoreEngineClient::connect(endpoint, "split-token").unwrap();

        let source = facade
            .create_session(CreateSessionRequest {
                cols: 120,
                rows: 40,
                command_dir: None,
                command: Some(CommandBuilder::from_argv(shell_argv())),
                env: Vec::new(),
                launch_policy: Default::default(),
            })
            .unwrap();

        let split = facade
            .split_session(SplitSessionRequest {
                source_pane_id: source.id,
                direction: SplitDirection::Right,
                size_percent: 50,
                command_dir: None,
                command: Some(CommandBuilder::from_argv(shell_argv())),
                env: Vec::new(),
                launch_policy: Default::default(),
            })
            .unwrap();
        assert_ne!(split.id, source.id);
        assert_eq!(split.split_from, Some(source.id));

        let mut owner = CoreClient::connect(endpoint, "split-token").unwrap();
        let _: Response<serde_json::Value> = owner.request("core.drain").unwrap();

        let blocked = facade.split_session(SplitSessionRequest {
            source_pane_id: source.id,
            direction: SplitDirection::Down,
            size_percent: 50,
            command_dir: None,
            command: Some(CommandBuilder::from_argv(shell_argv())),
            env: Vec::new(),
            launch_policy: Default::default(),
        });
        let err = blocked.expect_err("split must be refused while draining");
        assert!(err.to_string().contains("draining"), "got: {err:#}");

        facade.destroy_session(split.id).unwrap();
        facade.destroy_session(source.id).unwrap();
        let _: Response<serde_json::Value> = owner.request("core.shutdown").unwrap();
        worker.join().unwrap();
    }

    /// Read events until one satisfies the predicate. Other tests share
    /// the process-global engine, so unrelated sessions' events are
    /// expected here and must be skipped, not treated as failures.
    fn wait_for_event(
        events: &mut CoreEventStream,
        what: &str,
        mut predicate: impl FnMut(&CoreEvent) -> bool,
    ) -> CoreEvent {
        for _ in 0..500 {
            match events.next_event() {
                Ok(Some(event)) if predicate(&event) => return event,
                Ok(Some(_)) => continue,
                Ok(None) => panic!("event feed closed while waiting for {what}"),
                Err(err) => panic!("event feed failed while waiting for {what}: {err:#}"),
            }
        }
        panic!("gave up waiting for {what}");
    }

    #[test]
    fn events_stream_reports_session_lifecycle() {
        let (endpoint, worker) = start_server("events-token");
        let mut events = CoreEventStream::connect(endpoint, "events-token").unwrap();
        events
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();

        let facade = CoreEngineClient::connect(endpoint, "events-token").unwrap();
        let session = facade
            .create_session(CreateSessionRequest {
                cols: 80,
                rows: 24,
                command_dir: None,
                command: Some(CommandBuilder::from_argv(shell_argv())),
                env: Vec::new(),
                launch_policy: Default::default(),
            })
            .unwrap();
        let pane_id = session.id;

        wait_for_event(&mut events, "session_created", |event| {
            matches!(event, CoreEvent::SessionCreated { pane_id: id } if *id == pane_id)
        });

        facade
            .write_input(pane_id, "echo events-ready\r\n")
            .unwrap();
        let updated = wait_for_event(&mut events, "screen_updated", |event| {
            matches!(event, CoreEvent::ScreenUpdated { pane_id: id, .. } if *id == pane_id)
        });
        match updated {
            CoreEvent::ScreenUpdated { revision, .. } => assert!(revision > 0),
            _ => unreachable!(),
        }

        facade.destroy_session(pane_id).unwrap();
        wait_for_event(&mut events, "session_closed", |event| {
            matches!(event, CoreEvent::SessionClosed { pane_id: id } if *id == pane_id)
        });

        let mut owner = CoreClient::connect(endpoint, "events-token").unwrap();
        let _: Response<serde_json::Value> = owner.request("core.drain").unwrap();
        wait_for_event(&mut events, "draining", |event| {
            matches!(event, CoreEvent::Draining)
        });

        let _: Response<serde_json::Value> = owner.request("core.shutdown").unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn scrollback_lines_setting_crosses_the_ipc_boundary() {
        let (endpoint, worker) = start_server("scrollback-token");
        let facade = CoreEngineClient::connect(endpoint, "scrollback-token").unwrap();
        let default = unterm_engine::next_core::NextCoreEngine::new_session_scrollback_lines();

        facade.set_new_session_scrollback_lines(4321).unwrap();
        // The test server shares this process, so the global it set is
        // directly observable here.
        assert_eq!(
            unterm_engine::next_core::NextCoreEngine::new_session_scrollback_lines(),
            4321
        );

        // Other tests create sessions in this same process; leave the
        // default as we found it.
        facade.set_new_session_scrollback_lines(default).unwrap();
        let mut owner = CoreClient::connect(endpoint, "scrollback-token").unwrap();
        let _: Response<serde_json::Value> = owner.request("core.shutdown").unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn facade_covers_interaction_and_lifecycle_surface() {
        let (endpoint, worker) = start_server("interact-token");
        let facade = CoreEngineClient::connect(endpoint, "interact-token").unwrap();

        // The compile-time claim M1-04 rests on: the facade satisfies
        // the full TerminalEngine bound, not just the three base traits.
        fn assert_terminal_engine<T: unterm_engine::TerminalEngine>(_: &T) {}
        assert_terminal_engine(&facade);

        let session = facade
            .create_session(CreateSessionRequest {
                cols: 80,
                rows: 24,
                command_dir: None,
                command: Some(CommandBuilder::from_argv(shell_argv())),
                env: Vec::new(),
                launch_policy: Default::default(),
            })
            .unwrap();
        let pane_id = session.id;

        facade
            .write_input(pane_id, "echo interact-ready\r\n")
            .unwrap();
        let mut revision = 0;
        for _ in 0..50 {
            revision = facade.screen_revision(pane_id).unwrap();
            if revision > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(revision > 0, "screen revision never advanced");

        facade.scroll_viewport_by(pane_id, -3).unwrap();
        facade.scroll_viewport_to(pane_id, 0).unwrap();

        facade
            .report_mouse(
                pane_id,
                MouseEvent {
                    kind: MouseEventKind::Press,
                    button: Some(MouseButton::Left),
                    column: 0,
                    row: 0,
                    modifiers: MouseModifiers::NONE,
                },
            )
            .unwrap();
        facade
            .report_mouse(
                pane_id,
                MouseEvent {
                    kind: MouseEventKind::Release,
                    button: Some(MouseButton::Left),
                    column: 0,
                    row: 0,
                    modifiers: MouseModifiers::SHIFT,
                },
            )
            .unwrap();

        let health = facade.health().unwrap();
        assert!(health.ready, "engine health not ready: {health:?}");

        let status = facade.recording_status(pane_id).unwrap();
        assert!(!status.enabled);
        let started = facade.start_recording(pane_id).unwrap();
        assert!(!started.session_id.is_empty());
        let status = facade.recording_status(pane_id).unwrap();
        assert!(status.enabled);
        let stopped = facade.stop_recording(pane_id).unwrap();
        assert_eq!(stopped.session_id, started.session_id);

        facade.destroy_session(pane_id).unwrap();
        let mut owner = CoreClient::connect(endpoint, "interact-token").unwrap();
        let _: Response<serde_json::Value> = owner.request("core.shutdown").unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn frame_cache_converges_via_events_and_evicts_on_close() {
        let (endpoint, worker) = start_server("cache-token");
        let facade = CoreEngineClient::connect(endpoint, "cache-token").unwrap();
        let notified = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cache = {
            let notified = notified.clone();
            FrameCache::start_with_notify(endpoint, "cache-token", move || {
                notified.fetch_add(1, Ordering::AcqRel);
            })
            .unwrap()
        };

        let session = facade
            .create_session(CreateSessionRequest {
                cols: 80,
                rows: 24,
                command_dir: None,
                command: Some(CommandBuilder::from_argv(shell_argv())),
                env: Vec::new(),
                launch_policy: Default::default(),
            })
            .unwrap();
        let pane_id = session.id;
        facade
            .write_input(pane_id, "echo cache-ready\r\n")
            .unwrap();

        // The test thread never fetches: content may only arrive via
        // the event -> refetch pipeline the GUI will depend on.
        let mut converged = false;
        for _ in 0..100 {
            if let Some(screen) = cache.styled_screen(pane_id) {
                if styled_screen_text(&screen).contains("cache-ready") {
                    converged = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(converged, "cache never converged to the echoed text");
        assert!(cache.panes().contains(&pane_id));
        let generation = cache.generation();
        assert!(generation > 0);

        facade.destroy_session(pane_id).unwrap();
        let mut evicted = false;
        for _ in 0..100 {
            if cache.styled_screen(pane_id).is_none() {
                evicted = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(evicted, "closed pane was never evicted from the cache");
        assert!(cache.generation() > generation);
        // Every generation bump must have fired the wake hook: that is
        // what turns a Core-side screen change into a GUI redraw.
        assert_eq!(notified.load(Ordering::Acquire), cache.generation());

        drop(cache);
        let mut owner = CoreClient::connect(endpoint, "cache-token").unwrap();
        let _: Response<serde_json::Value> = owner.request("core.shutdown").unwrap();
        worker.join().unwrap();
    }

    /// A Core that goes away has to be reported, not merely stop
    /// sending. The frames already drawn still look like a live
    /// terminal; only this flag can tell a window otherwise.
    #[test]
    fn the_cache_reports_a_core_that_went_away() {
        let (endpoint, worker) = start_server("lost-token");
        let cache = FrameCache::start(endpoint, "lost-token").unwrap();
        assert!(cache.is_live(), "a fresh cache must start live");
        let initial_epoch = cache.epoch();

        // Idle long enough to cross several read timeouts: the
        // worker's own heartbeat must never read as a dead Core.
        std::thread::sleep(Duration::from_millis(700));
        assert!(cache.is_live(), "an idle feed is not a dead Core");

        let mut owner = CoreClient::connect(endpoint, "lost-token").unwrap();
        let _: Response<serde_json::Value> = owner.request("core.shutdown").unwrap();
        worker.join().unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while cache.is_live() && cache.epoch() == initial_epoch {
            assert!(
                std::time::Instant::now() < deadline,
                "the cache never noticed the core had gone"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Not a correctness test: measures what a GUI frame would pay to
    /// read a styled screen across the IPC boundary instead of from
    /// process-local memory. Run explicitly with
    /// `cargo test -p unterm-core --release -- --ignored --nocapture bench_styled`.
    #[test]
    #[ignore]
    fn bench_styled_screen_ipc_round_trip() {
        let (endpoint, worker) = start_server("bench-token");
        let facade = CoreEngineClient::connect(endpoint, "bench-token").unwrap();
        let session = facade
            .create_session(CreateSessionRequest {
                cols: 120,
                rows: 40,
                command_dir: None,
                command: Some(CommandBuilder::from_argv(shell_argv())),
                env: Vec::new(),
                launch_policy: Default::default(),
            })
            .unwrap();
        let pane_id = session.id;

        // Put real content on the screen so the payload is honest.
        for chunk in 0..5 {
            facade
                .write_input(pane_id, &format!("echo bench-fill-{chunk} {}\r\n", "x".repeat(80)))
                .unwrap();
        }
        std::thread::sleep(Duration::from_millis(500));

        let percentiles = |label: &str, mut samples: Vec<Duration>| {
            samples.sort();
            let p50 = samples[samples.len() / 2];
            let p95 = samples[samples.len() * 95 / 100];
            let max = samples[samples.len() - 1];
            println!("{label}: p50 {p50:?}, p95 {p95:?}, max {max:?}");
        };

        const ROUNDS: usize = 500;

        let mut styled = Vec::with_capacity(ROUNDS);
        for _ in 0..ROUNDS {
            let start = std::time::Instant::now();
            let snapshot = facade.read_styled_screen(pane_id).unwrap();
            styled.push(start.elapsed());
            assert_eq!(snapshot.cols, 120);
        }
        percentiles("read_styled_screen (full, 120x40)", styled);

        let revision = facade.read_styled_screen(pane_id).unwrap().revision;
        let mut unchanged = Vec::with_capacity(ROUNDS);
        for _ in 0..ROUNDS {
            let start = std::time::Instant::now();
            let frame = facade.read_render_frame(pane_id, Some(revision)).unwrap();
            unchanged.push(start.elapsed());
            assert!(!frame.full);
        }
        percentiles("read_render_frame (unchanged)", unchanged);

        // The in-process baseline the IPC numbers are judged against.
        let local = unterm_engine::next_core();
        let mut baseline = Vec::with_capacity(ROUNDS);
        for _ in 0..ROUNDS {
            let start = std::time::Instant::now();
            let snapshot = local.read_styled_screen(pane_id).unwrap();
            baseline.push(start.elapsed());
            assert_eq!(snapshot.cols, 120);
        }
        percentiles("read_styled_screen (in-process baseline)", baseline);

        facade.destroy_session(pane_id).unwrap();
        let mut owner = CoreClient::connect(endpoint, "bench-token").unwrap();
        let _: Response<serde_json::Value> = owner.request("core.shutdown").unwrap();
        worker.join().unwrap();
    }

    /// "Drain, then exit": the Core refuses new sessions, lets the
    /// running one finish, and stops itself the moment nothing is
    /// left -- without anyone sending a shutdown.
    #[test]
    fn draining_with_exit_when_idle_stops_once_the_last_session_ends() {
        let (endpoint, worker) = start_server("drain-exit-token");
        let facade = CoreEngineClient::connect(endpoint, "drain-exit-token").unwrap();
        let session = facade
            .create_session(CreateSessionRequest {
                cols: 80,
                rows: 24,
                command_dir: None,
                command: Some(CommandBuilder::from_argv(shell_argv())),
                env: Vec::new(),
                launch_policy: Default::default(),
            })
            .unwrap();

        facade.drain(true).unwrap();
        // Still serving while that session lives: draining is not
        // dying, and a Core that went away here would take a running
        // shell with it.
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            facade.list_sessions().is_ok(),
            "the core stopped while a session was still running"
        );

        facade.destroy_session(session.id).unwrap();
        // The watcher notices an empty engine and ends the server; the
        // run loop returning is the observable proof.
        let stopped = std::thread::spawn(move || worker.join());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !stopped.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "the core did not exit after its last session ended"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        stopped.join().unwrap().unwrap();
    }

    #[test]
    fn drain_rejects_new_sessions_but_keeps_existing() {
        let (endpoint, worker) = start_server("drain-token");
        let mut client = CoreClient::connect(endpoint, "drain-token").unwrap();
        let argv: Vec<&str> = if cfg!(windows) {
            vec!["cmd.exe"]
        } else {
            vec!["sh"]
        };
        let created: Response<serde_json::Value> = client
            .request_with_params(
                "session.create",
                serde_json::json!({"cols": 80, "rows": 24, "argv": argv}),
            )
            .unwrap();
        assert!(created.ok);
        let pane_id = created.result.unwrap()["id"].as_u64().unwrap();

        let drained: Response<serde_json::Value> = client.request("core.drain").unwrap();
        assert_eq!(
            drained.result.unwrap()["status"].as_str(),
            Some("draining")
        );
        let health: Response<serde_json::Value> = client.request("core.health").unwrap();
        assert_eq!(health.result.unwrap()["status"].as_str(), Some("draining"));

        let rejected: Response<serde_json::Value> = client
            .request_with_params(
                "session.create",
                serde_json::json!({"cols": 80, "rows": 24, "argv": argv}),
            )
            .unwrap();
        assert!(!rejected.ok);
        assert_eq!(rejected.error.unwrap().code, "draining");

        // The pre-drain session keeps working.
        let screen: Response<serde_json::Value> = client
            .request_with_params(
                "session.screen",
                serde_json::json!({"pane_id": pane_id}),
            )
            .unwrap();
        assert!(screen.ok);

        let closed: Response<serde_json::Value> = client
            .request_with_params(
                "session.close",
                serde_json::json!({"pane_id": pane_id}),
            )
            .unwrap();
        assert!(closed.ok);
        let _: Response<serde_json::Value> = client.request("core.shutdown").unwrap();
        worker.join().unwrap();
    }
}
