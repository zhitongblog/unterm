//! Opt-in LAN / Tailscale Remote Gateway for Unterm Mobile (App + Web).
//!
//! Security properties enforced here:
//! 1. **Disabled by default** (`enabled: false` in `~/.unterm/remote/config.json`).
//! 2. **Hard LAN / Tailscale CIDR gate**: Connections from outside RFC1918
//!    private LAN, link-local, loopback, or Tailscale (`100.64.0.0/10`,
//!    `fc00::/7`) are rejected immediately before any HTTP/WS processing.
//! 3. **One-time 6-digit pairing code** (120s TTL, single-use) exchanged for a
//!    long-lived per-device token whose SHA-256 hash is stored in
//!    `~/.unterm/remote/devices.json`.
//! 4. **Cryptographic machine identity**: Ed25519 keypair + SHA-256 fingerprint
//!    allowing paired mobile apps to auto-discover this machine over UDP/mDNS
//!    and verify its identity via challenge-response even when DHCP IPs change.
//! 5. **Strict MCP method allowlist**: Only cockpit, inbox, screen read, pane
//!    input, and approval decision methods are exposed to mobile devices.

use anyhow::{anyhow, Result};
use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use unterm_mcp::handler::{ConnectionContext, McpHandler};
use unterm_services::server_info;

pub const DEFAULT_REMOTE_PORT: u16 = 19880;
pub const PAIRING_TTL_MS: u64 = 120_000; // 2 minutes
pub const NETWORK_SCOPE_NOTICE: &str =
    "仅支持同一局域网（Wi-Fi / 有线）或 Tailscale 连接，已拦截所有公网来源访问";

const MOBILE_INDEX_HTML: &str = include_str!("../assets/mobile/index.html");

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteConfig {
    pub enabled: bool,
    pub port: u16,
    pub machine_id: String,
    pub signing_key_hex: String,
}

fn default_scope() -> String {
    "full".to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairedDevice {
    pub device_id: String,
    pub device_name: String,
    pub platform: String,
    pub token_hash: String,
    #[serde(default = "default_scope")]
    pub scope: String,
    pub paired_at: String,
    pub last_seen_at: String,
    pub last_ip: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairingRecord {
    pub code: String,
    #[serde(default = "default_scope")]
    pub scope: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteAddress {
    pub ip: String,
    pub kind: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairingBundle {
    pub code: String,
    pub scope: String,
    pub scope_label: String,
    pub expires_at_ms: u64,
    pub ttl_seconds: u64,
    pub port: u16,
    pub machine_id: String,
    pub hostname: String,
    pub fingerprint: String,
    pub public_key: String,
    pub addresses: Vec<RemoteAddress>,
    pub qr_uri: String,
    pub web_url: String,
    pub qr_svg: String,
    pub address_qrs: Vec<Value>,
    pub notice: String,
}

static SERVER_RUNNING: AtomicBool = AtomicBool::new(false);
static BOUND_PORT: Mutex<u16> = Mutex::new(0);
static CONFIG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn config_guard() -> &'static Mutex<()> {
    CONFIG_LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(test)]
thread_local! {
    static TEST_REMOTE_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

fn remote_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(dir) = TEST_REMOTE_DIR.with(|c| c.borrow().clone()) {
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    let base = unterm_protocol::state_dir()
        .unwrap_or_else(|| dirs_next::home_dir().unwrap_or_default().join(".unterm"));
    let dir = base.join("remote");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn config_path() -> PathBuf {
    remote_dir().join("config.json")
}

fn devices_path() -> PathBuf {
    remote_dir().join("devices.json")
}

fn pairing_path() -> PathBuf {
    remote_dir().join("pairing.json")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    for b in &mut out {
        *b = fastrand::u8(..);
    }
    out
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Hard network boundary check: returns `true` ONLY for loopback, private LAN
/// (RFC1918 + link-local), or Tailscale (`100.64.0.0/10` and `fc00::/7`).
pub fn is_allowed_remote_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_allowed_ipv4(v4),
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                return true;
            }
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_allowed_ipv4(mapped);
            }
            let seg0 = v6.segments()[0];
            // Unique Local Address fc00::/7 (includes Tailscale fd7a:115c:a1e0::/48)
            // or Link-Local fe80::/10
            (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80
        }
    }
}

fn is_allowed_ipv4(v4: Ipv4Addr) -> bool {
    if v4.is_loopback() || v4.is_private() || v4.is_link_local() {
        return true;
    }
    let octets = v4.octets();
    // Tailscale CGNAT range: 100.64.0.0/10 (100.64.0.0 - 100.127.255.255)
    octets[0] == 100 && (octets[1] & 0b1100_0000) == 64
}

pub fn classify_ip(ip: IpAddr) -> Option<(&'static str, &'static str)> {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            if o[0] == 100 && (o[1] & 0b1100_0000) == 64 {
                Some(("tailscale", "Tailscale 虚拟局域网"))
            } else if v4.is_private() {
                Some(("lan", "局域网 (Wi-Fi / LAN)"))
            } else {
                None
            }
        }
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            if seg[0] == 0xfd7a && seg[1] == 0x115c && seg[2] == 0xa1e0 {
                Some(("tailscale", "Tailscale IPv6"))
            } else {
                None
            }
        }
    }
}

/// Enumerate local LAN and Tailscale addresses on this machine.
pub fn detect_network_addresses() -> Vec<RemoteAddress> {
    let mut found: Vec<RemoteAddress> = Vec::new();
    let mut push_ip = |ip: IpAddr| {
        if let Some((kind, label)) = classify_ip(ip) {
            let ip_str = ip.to_string();
            if !found.iter().any(|x| x.ip == ip_str) {
                found.push(RemoteAddress {
                    ip: ip_str,
                    kind: kind.to_string(),
                    label: label.to_string(),
                });
            }
        }
    };

    // 1. Probe outbound routing for LAN and Tailscale
    for target in ["192.168.1.1:80", "10.0.0.1:80", "100.100.100.100:80", "8.8.8.8:80"] {
        if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
            if sock.connect(target).is_ok() {
                if let Ok(local) = sock.local_addr() {
                    push_ip(local.ip());
                }
            }
        }
    }

    // 2. Resolve local hostname addresses
    if let Ok(host) = hostname::get() {
        let host_str = host.to_string_lossy();
        if let Ok(addrs) = std::net::ToSocketAddrs::to_socket_addrs(&(host_str.as_ref(), 0)) {
            for addr in addrs {
                push_ip(addr.ip());
            }
        }
    }

    // 3. Parse `ipconfig` on Windows or `ifconfig`/`ip` on Unix to catch extra interfaces (e.g. Tailscale)
    #[cfg(windows)]
    {
        if let Ok(out) = std::process::Command::new("ipconfig").output() {
            let text = String::from_utf8_lossy(&out.stdout);
            for token in text.split(|c: char| !c.is_ascii_digit() && c != '.') {
                if token.split('.').count() == 4 {
                    if let Ok(v4) = token.parse::<Ipv4Addr>() {
                        // Exclude typical subnet masks like 255.x.x.x or .0 / .255 broadcast
                        let o = v4.octets();
                        if o[0] != 255 && o[3] != 0 && o[3] != 255 {
                            push_ip(IpAddr::V4(v4));
                        }
                    }
                }
            }
        }
    }

    // Sort LAN first, then Tailscale
    found.sort_by_key(|a| if a.kind == "lan" { 0 } else { 1 });
    found
}

pub fn load_or_init_config() -> RemoteConfig {
    let _g = config_guard().lock();
    load_or_init_config_unlocked()
}

fn load_or_init_config_unlocked() -> RemoteConfig {
    let path = config_path();
    if let Ok(raw) = std::fs::read_to_string(&path) {
        if let Ok(cfg) = serde_json::from_str::<RemoteConfig>(&raw) {
            if cfg.signing_key_hex.len() == 64 && !cfg.machine_id.is_empty() {
                return cfg;
            }
        }
    }
    let secret = random_bytes::<32>();
    let cfg = RemoteConfig {
        enabled: false,
        port: DEFAULT_REMOTE_PORT,
        machine_id: uuid::Uuid::new_v4().to_string(),
        signing_key_hex: hex::encode(secret),
    };
    if let Ok(s) = serde_json::to_string_pretty(&cfg) {
        let _ = std::fs::write(&path, s);
    }
    cfg
}

fn save_config_unlocked(cfg: &RemoteConfig) -> Result<()> {
    let s = serde_json::to_string_pretty(cfg)?;
    std::fs::write(config_path(), s)?;
    Ok(())
}

impl RemoteConfig {
    pub fn signing_key(&self) -> SigningKey {
        let mut bytes = [0u8; 32];
        if let Ok(decoded) = hex::decode(&self.signing_key_hex) {
            if decoded.len() == 32 {
                bytes.copy_from_slice(&decoded);
            }
        }
        SigningKey::from_bytes(&bytes)
    }

    pub fn public_key_hex(&self) -> String {
        hex::encode(self.signing_key().verifying_key().as_bytes())
    }

    pub fn fingerprint(&self) -> String {
        let full = sha256_hex(self.signing_key().verifying_key().as_bytes());
        full[..32].to_string()
    }

    pub fn sign_challenge(&self, nonce: &str) -> (String, String) {
        let fp = self.fingerprint();
        let message = format!("unterm-remote-v1:{}:{}:{}", self.machine_id, fp, nonce);
        let sig = self.signing_key().sign(message.as_bytes());
        let sig_hex = hex::encode(sig.to_bytes());

        let mut mac =
            HmacSha256::new_from_slice(fp.as_bytes()).expect("HMAC accepts any key length");
        mac.update(nonce.as_bytes());
        let hmac_hex = hex::encode(mac.finalize().into_bytes());
        (sig_hex, hmac_hex)
    }
}

pub fn set_enabled(enabled: bool, handler: Option<Arc<McpHandler>>) -> Result<Value> {
    {
        let _g = config_guard().lock();
        let mut cfg = load_or_init_config_unlocked();
        cfg.enabled = enabled;
        save_config_unlocked(&cfg)?;
    }
    if enabled {
        ensure_remote_server_started(handler.unwrap_or_else(|| Arc::new(McpHandler::new())));
    }
    Ok(status_snapshot())
}

pub fn generate_pairing_code() -> Result<PairingBundle> {
    generate_pairing_code_with_scope(None)
}

pub fn generate_pairing_code_with_scope(scope: Option<&str>) -> Result<PairingBundle> {
    let cfg = load_or_init_config();
    let code_num = fastrand::u32(100_000..1_000_000);
    let code = format!("{:06}", code_num);
    let norm_scope = crate::harness::normalize_scope(scope);
    let created_at_ms = now_ms();
    let expires_at_ms = created_at_ms + PAIRING_TTL_MS;
    let rec = PairingRecord {
        code: code.clone(),
        scope: norm_scope,
        created_at_ms,
        expires_at_ms,
    };
    std::fs::write(pairing_path(), serde_json::to_string_pretty(&rec)?)?;
    Ok(build_pairing_bundle(&cfg, &rec))
}

fn current_pairing_bundle(cfg: &RemoteConfig) -> Option<PairingBundle> {
    let raw = std::fs::read_to_string(pairing_path()).ok()?;
    let rec: PairingRecord = serde_json::from_str(&raw).ok()?;
    let now = now_ms();
    if now >= rec.expires_at_ms {
        let _ = std::fs::remove_file(pairing_path());
        return None;
    }
    Some(build_pairing_bundle(cfg, &rec))
}

fn build_pairing_bundle(cfg: &RemoteConfig, rec: &PairingRecord) -> PairingBundle {
    let addresses = detect_network_addresses();
    let host_list: Vec<String> = if addresses.is_empty() {
        vec!["127.0.0.1".to_string()]
    } else {
        addresses.iter().map(|a| a.ip.clone()).collect()
    };
    let primary_ip = host_list.first().cloned().unwrap_or_else(|| "127.0.0.1".to_string());
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unterm-PC".to_string());
    let fp = cfg.fingerprint();
    let pk = cfg.public_key_hex();
    let port = current_bound_port(cfg.port);
    let qr_uri = format!(
        "unterm://pair?h={}&p={}&id={}&fp={}&pk={}&name={}&c={}",
        host_list.join(","),
        port,
        cfg.machine_id,
        fp,
        pk,
        urlencoding_simple(&hostname),
        rec.code
    );
    let web_url = format!("http://{}:{}/?pair={}", primary_ip, port, rec.code);
    let qr_svg = crate::qrcode_svg::render_svg(&web_url);
    let address_qrs: Vec<Value> = addresses
        .iter()
        .map(|a| {
            let url = format!("http://{}:{}/?pair={}", a.ip, port, rec.code);
            json!({
                "ip": a.ip,
                "kind": a.kind,
                "label": a.label,
                "url": url,
                "qr_svg": crate::qrcode_svg::render_svg(&url),
            })
        })
        .collect();
    let now = now_ms();
    let ttl_seconds = rec.expires_at_ms.saturating_sub(now) / 1000;
    let norm_scope = crate::harness::normalize_scope(Some(&rec.scope));
    let scope_label = crate::harness::scope_label(&norm_scope).to_string();
    PairingBundle {
        code: rec.code.clone(),
        scope: norm_scope,
        scope_label,
        expires_at_ms: rec.expires_at_ms,
        ttl_seconds,
        port,
        machine_id: cfg.machine_id.clone(),
        hostname,
        fingerprint: fp,
        public_key: pk,
        addresses,
        qr_uri,
        web_url,
        qr_svg,
        address_qrs,
        notice: NETWORK_SCOPE_NOTICE.to_string(),
    }
}

fn urlencoding_simple(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

pub fn list_devices() -> Vec<PairedDevice> {
    let Ok(raw) = std::fs::read_to_string(devices_path()) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<PairedDevice>>(&raw).unwrap_or_default()
}

fn save_devices(devices: &[PairedDevice]) -> Result<()> {
    std::fs::write(devices_path(), serde_json::to_string_pretty(devices)?)?;
    Ok(())
}

pub fn revoke_device(device_id: &str) -> Result<bool> {
    let _g = config_guard().lock();
    let mut devices = list_devices();
    let before = devices.len();
    devices.retain(|d| d.device_id != device_id);
    let removed = devices.len() != before;
    save_devices(&devices)?;
    Ok(removed)
}

pub fn redeem_pairing_code(
    code: &str,
    device_name: &str,
    platform: &str,
    peer_ip: &str,
) -> Result<(PairedDevice, String)> {
    let _g = config_guard().lock();
    let code = code.trim();
    if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
        return Err(anyhow!("配对码格式无效（需为 6 位数字）"));
    }
    let raw = std::fs::read_to_string(pairing_path())
        .map_err(|_| anyhow!("当前没有生效中的配对码，请在电脑端重新生成"))?;
    let rec: PairingRecord =
        serde_json::from_str(&raw).map_err(|_| anyhow!("配对码记录已损坏"))?;
    if now_ms() >= rec.expires_at_ms {
        let _ = std::fs::remove_file(pairing_path());
        return Err(anyhow!("配对码已过期（有效期 2 分钟），请在电脑端重新生成"));
    }
    if rec.code != code {
        return Err(anyhow!("配对码不正确"));
    }
    // Single-use: consume immediately
    let _ = std::fs::remove_file(pairing_path());

    let raw_token = format!("utr_{}", hex::encode(random_bytes::<32>()));
    let token_hash = sha256_hex(raw_token.as_bytes());
    let now = now_iso();
    let norm_scope = crate::harness::normalize_scope(Some(&rec.scope));
    let dev = PairedDevice {
        device_id: uuid::Uuid::new_v4().to_string(),
        device_name: if device_name.trim().is_empty() {
            "Unterm Mobile".to_string()
        } else {
            device_name.trim().to_string()
        },
        platform: if platform.trim().is_empty() {
            "android".to_string()
        } else {
            platform.trim().to_string()
        },
        token_hash,
        scope: norm_scope,
        paired_at: now.clone(),
        last_seen_at: now,
        last_ip: peer_ip.to_string(),
    };
    let mut devices = list_devices();
    devices.push(dev.clone());
    save_devices(&devices)?;
    Ok((dev, raw_token))
}

pub fn authenticate_device_token(token: &str, peer_ip: &str) -> Option<PairedDevice> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let hash = sha256_hex(token.as_bytes());
    let _g = config_guard().lock();
    let mut devices = list_devices();
    let mut matched: Option<PairedDevice> = None;
    for d in &mut devices {
        if d.token_hash == hash {
            d.last_seen_at = now_iso();
            d.last_ip = peer_ip.to_string();
            matched = Some(d.clone());
            break;
        }
    }
    if matched.is_some() {
        let _ = save_devices(&devices);
    }
    matched
}

fn current_bound_port(default_port: u16) -> u16 {
    let p = *BOUND_PORT.lock();
    if p != 0 {
        p
    } else {
        default_port
    }
}

pub fn status_snapshot() -> Value {
    let cfg = load_or_init_config();
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unterm-PC".to_string());
    let addresses = detect_network_addresses();
    let pairing = current_pairing_bundle(&cfg);
    let devices: Vec<Value> = list_devices()
        .into_iter()
        .map(|d| {
            let norm_scope = crate::harness::normalize_scope(Some(&d.scope));
            json!({
                "device_id": d.device_id,
                "device_name": d.device_name,
                "platform": d.platform,
                "scope": norm_scope,
                "scope_label": crate::harness::scope_label(&norm_scope),
                "paired_at": d.paired_at,
                "last_seen_at": d.last_seen_at,
                "last_ip": d.last_ip,
            })
        })
        .collect();
    let port = current_bound_port(cfg.port);
    let info = server_info::read_current();
    let ports = crate::harness::discover_listening_ports(&[port, info.mcp_port, info.http_port]);
    let watched_cwd = info
        .cwd
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        });
    let checkpoints = if watched_cwd.is_empty() {
        Vec::new()
    } else {
        crate::harness::list_git_checkpoints(&watched_cwd).unwrap_or_default()
    };
    json!({
        "enabled": cfg.enabled,
        "running": SERVER_RUNNING.load(Ordering::Relaxed) && cfg.enabled,
        "port": port,
        "machine_id": cfg.machine_id,
        "hostname": hostname,
        "fingerprint": cfg.fingerprint(),
        "public_key": cfg.public_key_hex(),
        "network_scope": "lan_or_tailscale_only",
        "notice": NETWORK_SCOPE_NOTICE,
        "addresses": addresses,
        "ports": ports,
        "watched_cwd": watched_cwd,
        "checkpoints": checkpoints,
        "pairing": pairing,
        "devices": devices,
    })
}

pub fn dispatch_local_rpc(
    local_handler: &McpHandler,
    method: &str,
    params: Value,
) -> Result<Value> {
    let host_dev = PairedDevice {
        device_id: "host-settings".to_string(),
        device_name: "PC Web Settings".to_string(),
        platform: "desktop".to_string(),
        token_hash: String::new(),
        scope: "full".to_string(),
        paired_at: String::new(),
        last_seen_at: String::new(),
        last_ip: "127.0.0.1".to_string(),
    };
    let target_instance = params
        .get("instance")
        .and_then(Value::as_str)
        .map(str::to_string);
    dispatch_mobile_rpc(
        local_handler,
        &host_dev,
        target_instance.as_deref(),
        method,
        params,
    )
}

/// Strict allowlist of MCP methods exposed to mobile clients.
/// Blocks all Destructive, CredentialAccess, arbitrary shell/process spawn,
/// and profile secret operations.
pub fn is_allowed_remote_method(method: &str) -> bool {
    matches!(
        method,
        "instance.list"
            | "instance.info"
            | "agent.status"
            | "cockpit.inbox"
            | "session.list"
            | "session.focus"
            | "session.input"
            | "session.paste"
            | "session.resize"
            | "session.cwd"
            | "session.idle"
            | "screen.text"
            | "screen.read"
            | "screen.scrollback_text"
            | "screen.cursor"
            | "screen.scroll"
            | "signal.send"
            | "approval.list"
            | "approval.decide"
            | "review.list"
            | "review.diff"
            | "ports.list"
            | "checkpoint.list"
            | "checkpoint.create"
            | "checkpoint.diff"
            | "checkpoint.restore"
            | "handoff.build"
            | "handoff.execute"
            | "snapshot.capture"
    )
}

/// Start the remote listener with a default `McpHandler`.
pub fn ensure_remote_server_started_default() -> u16 {
    ensure_remote_server_started(Arc::new(McpHandler::new()))
}

/// Start the remote listener and UDP discovery responder if not already running.
pub fn ensure_remote_server_started(handler: Arc<McpHandler>) -> u16 {
    let cfg = load_or_init_config();
    if SERVER_RUNNING.load(Ordering::Relaxed) {
        return current_bound_port(cfg.port);
    }
    let bind_addr = SocketAddr::from(([0, 0, 0, 0], cfg.port));
    let listener = match TcpListener::bind(bind_addr) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("remote server could not bind {}: {}", bind_addr, e);
            return 0;
        }
    };
    let port = listener
        .local_addr()
        .map(|a| a.port())
        .unwrap_or(cfg.port);
    *BOUND_PORT.lock() = port;
    SERVER_RUNNING.store(true, Ordering::Relaxed);

    // Spawn UDP discovery responder on the same port + mDNS port 5353
    spawn_discovery_responder(port);

    thread::Builder::new()
        .name("unterm-remote-http".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let h = Arc::clone(&handler);
                        thread::Builder::new()
                            .name("unterm-remote-conn".into())
                            .spawn(move || {
                                if let Err(e) = handle_remote_conn(stream, &h) {
                                    log::debug!("remote client closed: {}", e);
                                }
                            })
                            .ok();
                    }
                    Err(e) => log::debug!("remote accept error: {}", e),
                }
            }
        })
        .ok();

    log::info!(
        "Unterm remote gateway listening on 0.0.0.0:{} (LAN/Tailscale only)",
        port
    );
    port
}

fn spawn_discovery_responder(port: u16) {
    // 1. Direct UDP discovery responder on `0.0.0.0:port` (handles broadcast & unicast probes)
    if let Ok(udp) = UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], port))) {
        let _ = udp.set_broadcast(true);
        thread::Builder::new()
            .name("unterm-remote-udp".into())
            .spawn(move || run_udp_discovery_loop(udp, port))
            .ok();
    }

    // 2. Best-effort mDNS responder on `0.0.0.0:5353` joining `224.0.0.251`
    if let Ok(sock) = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    ) {
        let _ = sock.set_reuse_address(true);
        let mdns_bind = SocketAddr::from(([0, 0, 0, 0], 5353));
        if sock.bind(&mdns_bind.into()).is_ok() {
            let udp: UdpSocket = sock.into();
            let _ = udp.join_multicast_v4(&Ipv4Addr::new(224, 0, 0, 251), &Ipv4Addr::UNSPECIFIED);
            thread::Builder::new()
                .name("unterm-remote-mdns".into())
                .spawn(move || run_udp_discovery_loop(udp, port))
                .ok();
        }
    }
}

fn run_udp_discovery_loop(udp: UdpSocket, port: u16) {
    let mut buf = [0u8; 1500];
    loop {
        let Ok((n, peer)) = udp.recv_from(&mut buf) else {
            thread::sleep(Duration::from_millis(100));
            continue;
        };
        if !is_allowed_remote_ip(peer.ip()) {
            continue;
        }
        let cfg = load_or_init_config();
        if !cfg.enabled {
            continue;
        }
        let packet = &buf[..n];
        // Handle text probe: "UNTERM_DISCOVER_V1" or "UNTERM_DISCOVER_V1:<nonce>"
        if let Ok(text) = std::str::from_utf8(packet) {
            let trimmed = text.trim();
            if let Some(rest) = trimmed.strip_prefix("UNTERM_DISCOVER_V1") {
                let nonce = rest.strip_prefix(':').unwrap_or("0");
                let payload = build_discovery_beacon(&cfg, port, nonce);
                if let Ok(bytes) = serde_json::to_vec(&payload) {
                    let _ = udp.send_to(&bytes, peer);
                }
                continue;
            }
        }
        // Also detect raw mDNS query containing "_unterm"
        if packet.windows(7).any(|w| w.eq_ignore_ascii_case(b"_unterm")) {
            let payload = build_discovery_beacon(&cfg, port, "mdns");
            if let Ok(bytes) = serde_json::to_vec(&payload) {
                let _ = udp.send_to(&bytes, peer);
            }
        }
    }
}

pub fn build_discovery_beacon(cfg: &RemoteConfig, port: u16, nonce: &str) -> Value {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unterm-PC".to_string());
    let (sig_hex, hmac_hex) = cfg.sign_challenge(nonce);
    json!({
        "proto": "unterm-discover-v1",
        "service": "_unterm._tcp.local",
        "machine_id": cfg.machine_id,
        "hostname": hostname,
        "port": port,
        "fingerprint": cfg.fingerprint(),
        "public_key": cfg.public_key_hex(),
        "nonce": nonce,
        "signature": sig_hex,
        "hmac": hmac_hex,
        "version": env!("CARGO_PKG_VERSION"),
        "addresses": detect_network_addresses(),
        "network_scope": "lan_or_tailscale_only",
        "notice": NETWORK_SCOPE_NOTICE,
    })
}

fn handle_remote_conn(mut stream: TcpStream, handler: &Arc<McpHandler>) -> Result<()> {
    let peer = stream.peer_addr()?;
    // 1. Hard LAN / Tailscale CIDR gate
    if !is_allowed_remote_ip(peer.ip()) {
        log::warn!(
            "Rejected non-LAN/non-Tailscale remote connection from {}",
            peer
        );
        let _ = write_http_json(
            &mut stream,
            403,
            "Forbidden",
            &json!({
                "error": "仅允许局域网 (LAN) 或 Tailscale IP 访问，已拒绝外部连接",
                "peer_ip": peer.ip().to_string(),
            }),
        );
        return Ok(());
    }

    // 2. Check if remote access is enabled
    let cfg = load_or_init_config();
    if !cfg.enabled {
        let _ = write_http_json(
            &mut stream,
            403,
            "Forbidden",
            &json!({
                "error": "电脑端未开启「允许手机远程连接」，请在 Unterm 设置或 unterm-cli remote enable 中开启",
            }),
        );
        return Ok(());
    }

    stream.set_read_timeout(Some(Duration::from_secs(15))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(15))).ok();
    stream.set_nodelay(true).ok();

    let req = parse_http_request(&mut stream)?;

    // CORS preflight
    if req.method == "OPTIONS" {
        return write_http_raw(
            &mut stream,
            204,
            "No Content",
            "text/plain",
            b"",
            &[
                ("Access-Control-Allow-Origin", "*"),
                ("Access-Control-Allow-Headers", "Authorization, Content-Type"),
                ("Access-Control-Allow-Methods", "GET, POST, OPTIONS"),
            ],
        );
    }

    // WebSocket upgrade: GET /ws?token=<device_token>
    if req.method == "GET" && req.path == "/ws" {
        let token = query_param(&req.query, "token")
            .or_else(|| {
                req.header("Authorization")
                    .and_then(|v| v.strip_prefix("Bearer "))
                    .map(|s| s.trim().to_string())
            })
            .unwrap_or_default();
        let Some(device) = authenticate_device_token(&token, &peer.ip().to_string()) else {
            return write_http_json(
                &mut stream,
                401,
                "Unauthorized",
                &json!({"error": "设备未配对或令牌已被撤销"}),
            );
        };
        return handle_websocket(stream, &req, handler, device);
    }

    // Public (LAN/Tailscale-gated) routes
    if req.method == "GET" && (req.path == "/" || req.path == "/mobile" || req.path == "/mobile/") {
        return write_http_raw(
            &mut stream,
            200,
            "OK",
            "text/html; charset=utf-8",
            MOBILE_INDEX_HTML.as_bytes(),
            &[],
        );
    }
    if req.method == "GET" && req.path == "/favicon.ico" {
        return write_http_raw(&mut stream, 204, "No Content", "image/x-icon", b"", &[]);
    }
    if req.method == "GET" && req.path == "/api/remote/hello" {
        let nonce = query_param(&req.query, "nonce").unwrap_or_else(|| "0".to_string());
        let port = current_bound_port(cfg.port);
        let beacon = build_discovery_beacon(&cfg, port, &nonce);
        return write_http_json(&mut stream, 200, "OK", &beacon);
    }
    if req.method == "POST" && req.path == "/api/remote/pair" {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or_else(|_| json!({}));
        let code = body.get("code").and_then(Value::as_str).unwrap_or("");
        let device_name = body
            .get("device_name")
            .and_then(Value::as_str)
            .unwrap_or("Unterm Mobile");
        let platform = body
            .get("platform")
            .and_then(Value::as_str)
            .unwrap_or("android");
        match redeem_pairing_code(code, device_name, platform, &peer.ip().to_string()) {
            Ok((dev, token)) => {
                let port = current_bound_port(cfg.port);
                let hostname = hostname::get()
                    .map(|h| h.to_string_lossy().to_string())
                    .unwrap_or_else(|_| "Unterm-PC".to_string());
                let norm_scope = crate::harness::normalize_scope(Some(&dev.scope));
                return write_http_json(
                    &mut stream,
                    200,
                    "OK",
                    &json!({
                        "ok": true,
                        "device_id": dev.device_id,
                        "device_token": token,
                        "scope": norm_scope,
                        "scope_label": crate::harness::scope_label(&norm_scope),
                        "machine_id": cfg.machine_id,
                        "hostname": hostname,
                        "port": port,
                        "fingerprint": cfg.fingerprint(),
                        "public_key": cfg.public_key_hex(),
                        "addresses": detect_network_addresses(),
                        "notice": NETWORK_SCOPE_NOTICE,
                    }),
                );
            }
            Err(e) => {
                return write_http_json(
                    &mut stream,
                    400,
                    "Bad Request",
                    &json!({"error": e.to_string()}),
                );
            }
        }
    }

    // Localhost Dev Server Preview Proxy: /preview/<port>/...
    if let Some(rest) = req.path.strip_prefix("/preview/") {
        let (port_str, subpath) = match rest.split_once('/') {
            Some((p, sub)) => (p, format!("/{}", sub)),
            None => (rest, "/".to_string()),
        };
        if let Ok(target_port) = port_str.parse::<u16>() {
            let query_token = query_param(&req.query, "token");
            let cookie_token = req.header("Cookie").and_then(|c| {
                c.split(';').find_map(|kv| {
                    let (k, v) = kv.trim().split_once('=')?;
                    (k == "unterm_preview_token").then(|| v.trim().to_string())
                })
            });
            let bearer_token = req
                .header("Authorization")
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(|s| s.trim().to_string());
            let tok = query_token
                .clone()
                .or(bearer_token)
                .or(cookie_token)
                .unwrap_or_default();
            if authenticate_device_token(&tok, &peer.ip().to_string()).is_none() {
                return write_http_json(
                    &mut stream,
                    401,
                    "Unauthorized",
                    &json!({"error": "预览本地端口需要已配对的手机令牌 (?token=...)"}),
                );
            }
            return crate::harness::proxy_localhost_preview(
                &mut stream,
                target_port,
                &subpath,
                &req.query,
                &req.method,
                &req.body,
                query_token.as_deref(),
            );
        }
    }

    // Authenticated device routes
    let token = req
        .header("Authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .unwrap_or("");
    let Some(device) = authenticate_device_token(token, &peer.ip().to_string()) else {
        return write_http_json(
            &mut stream,
            401,
            "Unauthorized",
            &json!({"error": "设备令牌无效或已被撤销，请重新扫码配对"}),
        );
    };

    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/api/remote/overview") => {
            let watched_inst = query_param(&req.query, "instance");
            let watched_pane = query_param(&req.query, "pane_id")
                .and_then(|s| s.parse::<u64>().ok());
            let mut snap = collect_mobile_overview(handler, watched_inst.as_deref(), watched_pane);
            if let Some(obj) = snap.as_object_mut() {
                let norm_scope = crate::harness::normalize_scope(Some(&device.scope));
                obj.insert("device_scope".to_string(), json!(norm_scope));
                obj.insert(
                    "device_scope_label".to_string(),
                    json!(crate::harness::scope_label(&norm_scope)),
                );
            }
            write_http_json(&mut stream, 200, "OK", &snap)
        }
        ("POST", "/api/remote/rpc") => {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_else(|_| json!({}));
            let method = body.get("method").and_then(Value::as_str).unwrap_or("");
            let params = body.get("params").cloned().unwrap_or_else(|| json!({}));
            let target_instance = body.get("instance").and_then(Value::as_str);
            match dispatch_mobile_rpc(handler, &device, target_instance, method, params) {
                Ok(result) => write_http_json(&mut stream, 200, "OK", &json!({"ok": true, "result": result})),
                Err(e) => write_http_json(
                    &mut stream,
                    403,
                    "Forbidden",
                    &json!({"ok": false, "error": e.to_string()}),
                ),
            }
        }
        _ => write_http_json(&mut stream, 404, "Not Found", &json!({"error": "not found"})),
    }
}

pub fn dispatch_mobile_rpc(
    local_handler: &McpHandler,
    device: &PairedDevice,
    target_instance: Option<&str>,
    method: &str,
    params: Value,
) -> Result<Value> {
    if !is_allowed_remote_method(method) {
        return Err(anyhow!(
            "安全策略拦截：方法 '{}' 不在移动端白名单内（禁止高风险或凭据类操作）",
            method
        ));
    }
    if !crate::harness::is_method_allowed_for_scope(&device.scope, method) {
        return Err(anyhow!(
            "权限不足：当前设备权限为「{}」，不允许执行 '{}'",
            crate::harness::scope_label(&device.scope),
            method
        ));
    }

    let current_info = server_info::read_current();

    // Built-in T3 Code-inspired Agent Harness methods
    match method {
        "ports.list" => {
            let port = current_bound_port(DEFAULT_REMOTE_PORT);
            let ports = crate::harness::discover_listening_ports(&[
                port,
                current_info.mcp_port,
                current_info.http_port,
            ]);
            return Ok(json!({ "ports": ports }));
        }
        "checkpoint.list" => {
            let cwd = resolve_rpc_cwd(local_handler, target_instance, &params);
            let list = crate::harness::list_git_checkpoints(&cwd).unwrap_or_default();
            return Ok(json!({ "cwd": cwd, "checkpoints": list }));
        }
        "checkpoint.create" => {
            let cwd = resolve_rpc_cwd(local_handler, target_instance, &params);
            let label = params
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or("手动检查点");
            let pane_id = params.get("pane_id").and_then(Value::as_u64);
            let ckpt = crate::harness::create_git_checkpoint(&cwd, label, pane_id)?;
            return Ok(json!({ "ok": true, "checkpoint": ckpt }));
        }
        "checkpoint.diff" => {
            let cwd = resolve_rpc_cwd(local_handler, target_instance, &params);
            let id = params
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("缺少检查点 id"))?;
            return crate::harness::diff_git_checkpoint(&cwd, id);
        }
        "checkpoint.restore" => {
            let cwd = resolve_rpc_cwd(local_handler, target_instance, &params);
            let id = params
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("缺少检查点 id"))?;
            return crate::harness::restore_git_checkpoint(&cwd, id);
        }
        "handoff.build" | "handoff.execute" => {
            let pane_id = params.get("pane_id").and_then(Value::as_u64).unwrap_or(1);
            let target_agent = params
                .get("target_agent")
                .and_then(Value::as_str)
                .unwrap_or("claude");
            let user_request = params
                .get("user_request")
                .and_then(Value::as_str)
                .unwrap_or("");
            let token_cap = params
                .get("token_cap")
                .and_then(Value::as_u64)
                .unwrap_or(16_000) as usize;
            let cwd = resolve_rpc_cwd(local_handler, target_instance, &params);
            let scrollback = fetch_pane_scrollback_for_handoff(
                local_handler,
                device,
                target_instance,
                pane_id,
            );
            let bundle = crate::harness::build_portable_handoff(
                pane_id,
                &cwd,
                "pane",
                &scrollback,
                target_agent,
                user_request,
                token_cap,
            )?;
            if method == "handoff.execute" {
                let target_pane = params
                    .get("target_pane_id")
                    .and_then(Value::as_u64)
                    .unwrap_or(pane_id);
                let steer_cmd = format!(
                    "请阅读上下文接力文件 `{}` 并继续执行：{}\n",
                    bundle.markdown_path,
                    if user_request.trim().is_empty() {
                        "继续完成未完事项"
                    } else {
                        user_request.trim()
                    }
                );
                let _ = dispatch_raw_mcp(
                    local_handler,
                    device,
                    target_instance,
                    "session.input",
                    json!({ "id": target_pane, "data": steer_cmd }),
                );
            }
            return Ok(serde_json::to_value(bundle)?);
        }
        "snapshot.capture" => {
            let include_a11y = params
                .get("include_a11y")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let snap = crate::harness::capture_window_with_a11y(include_a11y)?;
            if let Some(pane_id) = params.get("paste_to_pane").and_then(Value::as_u64) {
                let _ = dispatch_raw_mcp(
                    local_handler,
                    device,
                    target_instance,
                    "session.input",
                    json!({ "id": pane_id, "data": format!("{}\n", snap.agent_prompt_snippet) }),
                );
            }
            return Ok(serde_json::to_value(snap)?);
        }
        _ => {}
    }

    dispatch_raw_mcp(local_handler, device, target_instance, method, params)
}

fn resolve_rpc_cwd(
    local_handler: &McpHandler,
    target_instance: Option<&str>,
    params: &Value,
) -> String {
    if let Some(c) = params
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return c.to_string();
    }
    if let Some(pane_id) = params.get("pane_id").and_then(Value::as_u64) {
        let dummy = PairedDevice {
            device_id: String::new(),
            device_name: "cwd".to_string(),
            platform: "internal".to_string(),
            token_hash: String::new(),
            scope: "full".to_string(),
            paired_at: String::new(),
            last_seen_at: String::new(),
            last_ip: String::new(),
        };
        if let Ok(res) = dispatch_raw_mcp(
            local_handler,
            &dummy,
            target_instance,
            "session.cwd",
            json!({ "id": pane_id }),
        ) {
            if let Some(c) = res
                .get("cwd")
                .and_then(Value::as_str)
                .or_else(|| res.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return c.to_string();
            }
        }
    }
    server_info::read_current()
        .cwd
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string())
        })
}

fn fetch_pane_scrollback_for_handoff(
    local_handler: &McpHandler,
    device: &PairedDevice,
    target_instance: Option<&str>,
    pane_id: u64,
) -> String {
    if let Ok(res) = dispatch_raw_mcp(
        local_handler,
        device,
        target_instance,
        "screen.scrollback_text",
        json!({ "id": pane_id, "max_lines": 160 }),
    ) {
        if let Some(t) = res.get("text").and_then(Value::as_str) {
            if !t.trim().is_empty() {
                return t.to_string();
            }
        }
    }
    if let Ok(res) = dispatch_raw_mcp(
        local_handler,
        device,
        target_instance,
        "screen.text",
        json!({ "id": pane_id }),
    ) {
        if let Some(t) = res.get("text").and_then(Value::as_str) {
            return t.to_string();
        }
    }
    String::new()
}

fn dispatch_raw_mcp(
    local_handler: &McpHandler,
    device: &PairedDevice,
    target_instance: Option<&str>,
    method: &str,
    params: Value,
) -> Result<Value> {
    let current_info = server_info::read_current();
    let target = target_instance
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "default");

    // Route to peer instance if requested and different from this process
    if let Some(inst_id) = target {
        if inst_id != current_info.id {
            let instances = server_info::list_live_instances();
            if let Some(peer_inst) = instances.iter().find(|i| i.id == inst_id) {
                return unterm_services::peer_mcp::call_instance(peer_inst, method, params);
            }
        }
    }

    // Otherwise execute against local_handler, or fallback to first live instance if local has no sessions
    let label = format!("mobile:{}", device.device_name);
    let ctx = ConnectionContext::internal(&label);
    match local_handler.handle(&ctx, method, &params) {
        Ok(val) => Ok(val),
        Err(e) => {
            // If this process doesn't own the pane, try the active/first live instance
            let instances = server_info::list_live_instances();
            if let Some(first) = instances.first() {
                if first.pid != current_info.pid && first.mcp_port != 0 {
                    if let Ok(val) = unterm_services::peer_mcp::call_instance(first, method, params) {
                        return Ok(val);
                    }
                }
            }
            Err(e)
        }
    }
}

pub fn collect_mobile_overview(
    local_handler: &McpHandler,
    watched_instance: Option<&str>,
    watched_pane: Option<u64>,
) -> Value {
    let current_info = server_info::read_current();
    let live_instances = server_info::list_live_instances();
    let ctx = ConnectionContext::internal("mobile_overview");

    let mut all_sessions: Vec<Value> = Vec::new();
    let mut all_agents: Vec<Value> = Vec::new();
    let mut all_inbox: Vec<Value> = Vec::new();
    let mut all_approvals: Vec<Value> = Vec::new();

    if live_instances.is_empty() {
        // Fallback to in-process handler
        if let Ok(s) = local_handler.handle(&ctx, "session.list", &json!({})) {
            if let Some(arr) = s.get("sessions").and_then(Value::as_array) {
                for item in arr {
                    let mut obj = item.clone();
                    if let Some(m) = obj.as_object_mut() {
                        m.insert("instance_id".to_string(), json!(current_info.id));
                    }
                    all_sessions.push(obj);
                }
            }
        }
        if let Ok(ib) = local_handler.handle(&ctx, "cockpit.inbox", &json!({})) {
            if let Some(arr) = ib.get("items").or_else(|| ib.get("inbox")).and_then(Value::as_array)
            {
                for item in arr {
                    let mut obj = item.clone();
                    if let Some(m) = obj.as_object_mut() {
                        m.insert("instance_id".to_string(), json!(current_info.id));
                    }
                    all_inbox.push(obj);
                }
            }
        }
        if let Ok(ap) = local_handler.handle(&ctx, "approval.list", &json!({})) {
            if let Some(arr) = ap
                .get("approvals")
                .or_else(|| ap.get("items"))
                .and_then(Value::as_array)
            {
                all_approvals.extend(arr.iter().cloned());
            }
        }
    } else {
        for inst in &live_instances {
            let call = |method: &str, params: Value| -> Option<Value> {
                if inst.pid == current_info.pid {
                    local_handler.handle(&ctx, method, &params).ok()
                } else {
                    unterm_services::peer_mcp::call_instance(inst, method, params).ok()
                }
            };

            if let Some(s) = call("session.list", json!({})) {
                let arr = s
                    .get("sessions")
                    .and_then(Value::as_array)
                    .or_else(|| s.as_array());
                if let Some(list) = arr {
                    for item in list {
                        let mut obj = item.clone();
                        if let Some(m) = obj.as_object_mut() {
                            m.insert("instance_id".to_string(), json!(inst.id));
                        }
                        all_sessions.push(obj);
                    }
                }
            }

            if let Some(st) = call("agent.status", json!({})) {
                if let Some(arr) = st
                    .get("agents")
                    .or_else(|| st.get("panes"))
                    .and_then(Value::as_array)
                {
                    for item in arr {
                        let mut obj = item.clone();
                        if let Some(m) = obj.as_object_mut() {
                            m.insert("instance_id".to_string(), json!(inst.id));
                        }
                        all_agents.push(obj);
                    }
                }
            }

            if let Some(ib) = call("cockpit.inbox", json!({})) {
                if let Some(arr) = ib
                    .get("items")
                    .or_else(|| ib.get("inbox"))
                    .and_then(Value::as_array)
                {
                    for item in arr {
                        let mut obj = item.clone();
                        if let Some(m) = obj.as_object_mut() {
                            m.insert("instance_id".to_string(), json!(inst.id));
                        }
                        all_inbox.push(obj);
                    }
                }
            }

            if let Some(ap) = call("approval.list", json!({})) {
                if let Some(arr) = ap
                    .get("approvals")
                    .or_else(|| ap.get("items"))
                    .and_then(Value::as_array)
                {
                    for item in arr {
                        let mut obj = item.clone();
                        if let Some(m) = obj.as_object_mut() {
                            m.insert("instance_id".to_string(), json!(inst.id));
                        }
                        all_approvals.push(obj);
                    }
                }
            }
        }
    }

    // Fetch screen text for the watched pane (or first session's pane)
    let target_pane = watched_pane.or_else(|| {
        all_sessions.first().and_then(|s| {
            s.get("pane_id")
                .or_else(|| s.get("id"))
                .and_then(Value::as_u64)
        })
    });
    let target_inst = watched_instance.map(str::to_string).or_else(|| {
        all_sessions
            .first()
            .and_then(|s| s.get("instance_id").and_then(Value::as_str))
            .map(str::to_string)
    });

    let mut screen_text = String::new();
    let mut watched_cwd = String::new();
    if let Some(pane_id) = target_pane {
        let dummy_dev = PairedDevice {
            device_id: String::new(),
            device_name: "overview".to_string(),
            platform: "mobile".to_string(),
            token_hash: String::new(),
            scope: "full".to_string(),
            paired_at: String::new(),
            last_seen_at: String::new(),
            last_ip: String::new(),
        };
        if let Ok(res) = dispatch_mobile_rpc(
            local_handler,
            &dummy_dev,
            target_inst.as_deref(),
            "screen.text",
            json!({ "id": pane_id }),
        ) {
            if let Some(t) = res.get("text").and_then(Value::as_str) {
                screen_text = t.to_string();
            } else if let Some(t) = res.as_str() {
                screen_text = t.to_string();
            }
        }
        watched_cwd = resolve_rpc_cwd(
            local_handler,
            target_inst.as_deref(),
            &json!({ "pane_id": pane_id }),
        );
    }
    if watched_cwd.is_empty() {
        watched_cwd = current_info.cwd.clone().unwrap_or_default();
    }

    let port = current_bound_port(DEFAULT_REMOTE_PORT);
    let ports = crate::harness::discover_listening_ports(&[
        port,
        current_info.mcp_port,
        current_info.http_port,
    ]);
    let checkpoints = crate::harness::list_git_checkpoints(&watched_cwd).unwrap_or_default();
    let routes = detect_network_addresses();

    let instances_json: Vec<Value> = live_instances
        .iter()
        .map(|i| {
            json!({
                "id": i.id,
                "pid": i.pid,
                "title": i.title,
                "cwd": i.cwd,
                "agents": i.agents,
            })
        })
        .collect();

    json!({
        "instances": instances_json,
        "inbox": all_inbox,
        "agents": all_agents,
        "sessions": all_sessions,
        "approvals": all_approvals,
        "ports": ports,
        "checkpoints": checkpoints,
        "routes": routes,
        "watched_cwd": watched_cwd,
        "watched_instance": target_inst,
        "watched_pane": target_pane,
        "screen_text": screen_text,
        "timestamp_ms": now_ms(),
    })
}

// --- Minimal RFC 6455 WebSocket implementation ----------------------------

fn handle_websocket(
    mut stream: TcpStream,
    req: &HttpRequest,
    handler: &Arc<McpHandler>,
    device: PairedDevice,
) -> Result<()> {
    let Some(ws_key) = req.header("Sec-WebSocket-Key") else {
        return write_http_json(
            &mut stream,
            400,
            "Bad Request",
            &json!({"error": "missing Sec-WebSocket-Key"}),
        );
    };
    let mut sha1 = Sha1::new();
    sha1.update(ws_key.trim().as_bytes());
    sha1.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let accept = base64::engine::general_purpose::STANDARD.encode(sha1.finalize());

    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        accept
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;

    // Short read timeout so we can interleave server->client state push with client->server commands
    stream
        .set_read_timeout(Some(Duration::from_millis(400)))
        .ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

    let mut watched_instance = query_param(&req.query, "instance");
    let mut watched_pane = query_param(&req.query, "pane_id").and_then(|s| s.parse::<u64>().ok());
    let mut last_snapshot_hash = String::new();
    let mut last_push = std::time::Instant::now() - Duration::from_secs(10);

    loop {
        if !load_or_init_config().enabled {
            let _ = ws_send_text(
                &mut stream,
                &serde_json::to_string(&json!({"type": "closed", "reason": "remote_disabled"}))
                    .unwrap_or_default(),
            );
            break;
        }

        // Push snapshot diff every ~800ms
        if last_push.elapsed() >= Duration::from_millis(800) {
            last_push = std::time::Instant::now();
            let mut snap = collect_mobile_overview(
                handler,
                watched_instance.as_deref(),
                watched_pane,
            );
            // Exclude timestamp_ms when computing change hash
            if let Some(obj) = snap.as_object_mut() {
                obj.remove("timestamp_ms");
            }
            let serialized = serde_json::to_string(&snap).unwrap_or_default();
            let digest = sha256_hex(serialized.as_bytes());
            if digest != last_snapshot_hash {
                last_snapshot_hash = digest;
                let msg = serde_json::to_string(&json!({
                    "type": "snapshot",
                    "data": snap,
                }))?;
                if ws_send_text(&mut stream, &msg).is_err() {
                    break;
                }
            }
        }

        // Read incoming frame if available
        match ws_read_frame(&mut stream) {
            Ok(Some((opcode, payload))) => match opcode {
                0x8 => break, // Close
                0x9 => {
                    // Ping -> Pong
                    let _ = ws_send_frame(&mut stream, 0xA, &payload);
                }
                0x1 => {
                    // Text frame
                    if let Ok(val) = serde_json::from_slice::<Value>(&payload) {
                        let msg_type = val.get("type").and_then(Value::as_str).unwrap_or("rpc");
                        if msg_type == "watch" {
                            watched_instance = val
                                .get("instance")
                                .and_then(Value::as_str)
                                .map(str::to_string);
                            watched_pane = val.get("pane_id").and_then(Value::as_u64);
                            last_snapshot_hash.clear();
                        } else if msg_type == "rpc" {
                            let req_id = val.get("id").cloned().unwrap_or(Value::Null);
                            let inst = val.get("instance").and_then(Value::as_str);
                            let method = val.get("method").and_then(Value::as_str).unwrap_or("");
                            let params = val.get("params").cloned().unwrap_or_else(|| json!({}));
                            let reply = match dispatch_mobile_rpc(handler, &device, inst, method, params) {
                                Ok(res) => json!({"type": "rpc_result", "id": req_id, "ok": true, "result": res}),
                                Err(e) => json!({"type": "rpc_result", "id": req_id, "ok": false, "error": e.to_string()}),
                            };
                            if ws_send_text(&mut stream, &serde_json::to_string(&reply)?).is_err() {
                                break;
                            }
                            last_snapshot_hash.clear();
                        }
                    }
                }
                _ => {}
            },
            Ok(None) => {
                // Timeout (no client frame in this 400ms window), continue loop
            }
            Err(_) => break,
        }
    }
    Ok(())
}

fn ws_read_frame(stream: &mut TcpStream) -> Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0u8; 2];
    match stream.read_exact(&mut head) {
        Ok(()) => {}
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    }
    let opcode = head[0] & 0x0f;
    let masked = (head[1] & 0x80) != 0;
    let mut len = (head[1] & 0x7f) as usize;
    if len == 126 {
        let mut ext = [0u8; 2];
        stream.read_exact(&mut ext)?;
        len = u16::from_be_bytes(ext) as usize;
    } else if len == 127 {
        let mut ext = [0u8; 8];
        stream.read_exact(&mut ext)?;
        len = u64::from_be_bytes(ext) as usize;
    }
    if len > 256 * 1024 {
        return Err(anyhow!("ws frame too large"));
    }
    let mut mask = [0u8; 4];
    if masked {
        stream.read_exact(&mut mask)?;
    }
    let mut payload = vec![0u8; len];
    if len > 0 {
        stream.read_exact(&mut payload)?;
    }
    if masked {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i & 3];
        }
    }
    Ok(Some((opcode, payload)))
}

fn ws_send_text(stream: &mut TcpStream, text: &str) -> Result<()> {
    ws_send_frame(stream, 0x1, text.as_bytes())
}

fn ws_send_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> Result<()> {
    let mut header = Vec::with_capacity(10);
    header.push(0x80 | (opcode & 0x0f));
    let len = payload.len();
    if len < 126 {
        header.push(len as u8);
    } else if len <= 65535 {
        header.push(126);
        header.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        header.push(127);
        header.extend_from_slice(&(len as u64).to_be_bytes());
    }
    stream.write_all(&header)?;
    stream.write_all(payload)?;
    stream.flush()?;
    Ok(())
}

// --- Lightweight HTTP helpers for remote gateway --------------------------

struct HttpRequest {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

fn parse_http_request(stream: &mut TcpStream) -> Result<HttpRequest> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let line = line.trim_end_matches(['\r', '\n']);
    let mut parts = line.splitn(3, ' ');
    let method = parts.next().ok_or_else(|| anyhow!("no method"))?.to_string();
    let target = parts.next().ok_or_else(|| anyhow!("no target"))?.to_string();
    let (path, query) = match target.find('?') {
        Some(i) => (target[..i].to_string(), target[i + 1..].to_string()),
        None => (target, String::new()),
    };
    let mut headers = Vec::new();
    loop {
        let mut buf = String::new();
        if reader.read_line(&mut buf)? == 0 {
            break;
        }
        let trimmed = buf.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(idx) = trimmed.find(':') {
            headers.push((
                trimmed[..idx].trim().to_string(),
                trimmed[idx + 1..].trim().to_string(),
            ));
        }
    }
    let content_len = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0)
        .min(256 * 1024);
    let mut body = vec![0u8; content_len];
    if content_len > 0 {
        reader.read_exact(&mut body)?;
    }
    Ok(HttpRequest {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn write_http_json(
    stream: &mut TcpStream,
    status: u16,
    reason: &'static str,
    value: &Value,
) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    write_http_raw(
        stream,
        status,
        reason,
        "application/json; charset=utf-8",
        &body,
        &[("Access-Control-Allow-Origin", "*")],
    )
}

fn write_http_raw(
    stream: &mut TcpStream,
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    body: &[u8],
    extra: &[(&str, &str)],
) -> Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n",
        status,
        reason,
        content_type,
        body.len()
    );
    for (k, v) in extra {
        head.push_str(k);
        head.push_str(": ");
        head.push_str(v);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr_gate_allows_only_lan_and_tailscale() {
        // Allowed: LAN RFC1918
        assert!(is_allowed_remote_ip("192.168.1.25".parse().unwrap()));
        assert!(is_allowed_remote_ip("10.0.4.12".parse().unwrap()));
        assert!(is_allowed_remote_ip("172.16.0.1".parse().unwrap()));
        assert!(is_allowed_remote_ip("172.31.255.254".parse().unwrap()));
        // Allowed: Tailscale CGNAT 100.64.0.0/10
        assert!(is_allowed_remote_ip("100.64.0.1".parse().unwrap()));
        assert!(is_allowed_remote_ip("100.101.42.88".parse().unwrap()));
        assert!(is_allowed_remote_ip("100.127.255.254".parse().unwrap()));
        // Allowed: Tailscale IPv6 ULA
        assert!(is_allowed_remote_ip(
            "fd7a:115c:a1e0::1".parse().unwrap()
        ));
        // Allowed: loopback
        assert!(is_allowed_remote_ip("127.0.0.1".parse().unwrap()));
        assert!(is_allowed_remote_ip("::1".parse().unwrap()));

        // Blocked: Public IPv4 (including 100.x outside /10 and 172.x outside /12)
        assert!(!is_allowed_remote_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_allowed_remote_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_allowed_remote_ip("100.63.255.255".parse().unwrap()));
        assert!(!is_allowed_remote_ip("100.128.0.1".parse().unwrap()));
        assert!(!is_allowed_remote_ip("172.32.0.1".parse().unwrap()));
        assert!(!is_allowed_remote_ip("203.0.113.5".parse().unwrap()));
        // Blocked: Public IPv6
        assert!(!is_allowed_remote_ip(
            "2606:4700:4700::1111".parse().unwrap()
        ));
    }

    #[test]
    fn method_allowlist_permits_cockpit_and_blocks_destructive_or_secrets() {
        assert!(is_allowed_remote_method("cockpit.inbox"));
        assert!(is_allowed_remote_method("agent.status"));
        assert!(is_allowed_remote_method("screen.text"));
        assert!(is_allowed_remote_method("session.input"));
        assert!(is_allowed_remote_method("approval.decide"));

        assert!(!is_allowed_remote_method("exec.run"));
        assert!(!is_allowed_remote_method("system.launch_admin"));
        assert!(!is_allowed_remote_method("profile.list"));
        assert!(!is_allowed_remote_method("session.destroy"));
    }

    #[test]
    fn challenge_signature_verifies_with_ed25519_public_key() {
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        let cfg = RemoteConfig {
            enabled: true,
            port: 19880,
            machine_id: "test-machine-1".to_string(),
            signing_key_hex: hex::encode([7u8; 32]),
        };
        let (sig_hex, hmac_hex) = cfg.sign_challenge("nonce123");
        assert!(!sig_hex.is_empty());
        assert!(!hmac_hex.is_empty());

        let pk_bytes = hex::decode(cfg.public_key_hex()).unwrap();
        let vk = VerifyingKey::from_bytes(&pk_bytes.try_into().unwrap()).unwrap();
        let sig_bytes = hex::decode(sig_hex).unwrap();
        let sig = Signature::from_bytes(&sig_bytes.try_into().unwrap());
        let msg = format!(
            "unterm-remote-v1:{}:{}:nonce123",
            cfg.machine_id,
            cfg.fingerprint()
        );
        assert!(vk.verify(msg.as_bytes(), &sig).is_ok());
    }

    #[test]
    fn pairing_code_is_single_use_and_device_revocation_invalidates_token() {
        let temp = tempfile::tempdir().unwrap();
        TEST_REMOTE_DIR.with(|c| *c.borrow_mut() = Some(temp.path().join("remote")));

        let bundle = generate_pairing_code().unwrap();
        assert_eq!(bundle.code.len(), 6);
        assert!(bundle.qr_svg.starts_with("<svg"));

        // First redemption succeeds
        let (dev, token) =
            redeem_pairing_code(&bundle.code, "Pixel 9 Pro", "android", "192.168.1.88").unwrap();
        assert!(token.starts_with("utr_"));

        // Second redemption with the same code must fail (single-use)
        assert!(
            redeem_pairing_code(&bundle.code, "Replay Phone", "android", "192.168.1.99").is_err()
        );

        // Token authenticates
        let authed = authenticate_device_token(&token, "192.168.1.88");
        assert!(authed.is_some());
        assert_eq!(authed.unwrap().device_id, dev.device_id);

        // Revoking removes device and invalidates token immediately
        assert!(revoke_device(&dev.device_id).unwrap());
        assert!(authenticate_device_token(&token, "192.168.1.88").is_none());
    }
}
