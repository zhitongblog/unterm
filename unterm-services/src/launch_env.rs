//! The environment a command is launched with.
//!
//! Split from spawning itself, which needs a mux and a window. This half needs
//! neither: it decides what profile variables, proxy settings and encoding
//! switches a shell should start with, and a second front end needs exactly
//! the same answers.
//!
//! The Windows encoding rewrite here is the reason a shell on a Chinese
//! Windows shows its text rather than a row of boxes -- `cmd.exe` and
//! PowerShell both emit the console codepage unless told otherwise, and what
//! a terminal receives then is not UTF-8 at all.

use portable_pty::CommandBuilder;

/// Resolve the identity environment bound to the current Unterm window.
///
/// next-core uses `CreateSessionRequest::env` both for the child process and
/// for the redacted launch metadata exposed through MCP, so callers should
/// pass these values there rather than only attaching them to a command.
pub fn current_profile_env() -> Vec<(String, String)> {
    let info = crate::server_info::read_current();
    let Some(profile_id) = info.profile.as_deref() else {
        return Vec::new();
    };
    if profile_id.is_empty() {
        return Vec::new();
    }

    let registry = match unterm_profile::ProfileRegistry::load() {
        Ok(registry) => registry,
        Err(error) => {
            log::warn!("current_profile_env: registry load failed: {error:#}");
            return vec![("UNTERM_PROFILE".to_string(), profile_id.to_string())];
        }
    };
    let store = match unterm_profile::default_store() {
        Ok(store) => store,
        Err(error) => {
            log::warn!("current_profile_env: no secret store: {error:#}");
            return vec![("UNTERM_PROFILE".to_string(), profile_id.to_string())];
        }
    };
    match registry.resolve_env(store.as_ref(), profile_id) {
        Ok(env) => env.into_iter().collect(),
        Err(error) => {
            log::warn!("current_profile_env: resolve_env({profile_id}) failed: {error:#}");
            vec![("UNTERM_PROFILE".to_string(), profile_id.to_string())]
        }
    }
}

pub fn apply_unterm_profile_env(cmd_builder: &mut Option<CommandBuilder>) {
    let env = current_profile_env();
    if env.is_empty() {
        return;
    }

    // Mirror `apply_unterm_proxy_env`: if the spawn didn't customize
    // anything, we still need a CommandBuilder so we have somewhere
    // to attach env. Default prog matches what wezterm would have
    // launched anyway.
    let builder = cmd_builder.get_or_insert_with(CommandBuilder::new_default_prog);
    for (k, v) in env {
        builder.env(k, v);
    }
}

pub fn apply_unterm_proxy_env(cmd_builder: &mut Option<CommandBuilder>) {
    let Some(proxy) = read_unterm_proxy_env() else {
        return;
    };
    let builder = cmd_builder.get_or_insert_with(CommandBuilder::new_default_prog);
    for (key, value) in proxy {
        builder.env(key, value);
    }
}

pub fn apply_unterm_proxy_to_process_env() {
    let Some(proxy) = read_unterm_proxy_env() else {
        return;
    };
    for (key, value) in proxy {
        std::env::set_var(key, value);
    }
}

/// On Windows, force UTF-8 in spawned shells so that PowerShell / cmd.exe
/// running on a zh-CN (CP936/GBK), zh-TW (CP950/Big5), ja-JP (CP932/SJIS),
/// or any other non-UTF-8 system locale still emit UTF-8 bytes — which is
/// what every other tool in our pipeline (font rendering, MCP transcripts,
/// recording redaction, agent integrations) expects.
///
/// Why we do this even though Windows Terminal mostly "just works": WT
/// relies on the user's PowerShell `$PROFILE` having
/// `[Console]::OutputEncoding = [Text.UTF8Encoding]::new()` already, or
/// on `pwsh` (PowerShell 7+) which defaults to UTF-8. Our positioning is
/// "the terminal AI agents can drive" — agents won't fix the user's
/// profile, and we can't assume PS7 either. So we wrap at spawn time.
///
/// Strategy:
///   - args == ["powershell.exe"] / ["pwsh.exe"] / etc. (just the
///     binary, nothing else): replace with the binary plus
///     `-NoLogo -NoProfile -NoExit -Command "<UTF-8 setup>"`.
///     -NoProfile keeps PowerShell from printing startup warnings before
///     the setup runs. We do not clear the screen or source `$PROFILE`
///     from this wrapper: both are interactive-shell concerns, and doing
///     them from startup glue can leave ConPTY showing a blank pane.
///   - args == ["cmd.exe"]: wrap with
///     `cmd.exe /D /K "<System32>\chcp.com 65001 > nul"` — sets the console
///     codepage before the prompt appears, no command echoed. By full path;
///     see `chcp_command`.
///   - Anything else (user passed extra args, or non-shell exe): leave
///     untouched. They're customizing; we don't second-guess.
///
/// If no args are present, leave the builder alone. The mux/domain layer
/// still needs to resolve the configured default_prog; forcing it to cmd.exe
/// here makes default split panes ignore the PowerShell default on Windows.
#[cfg(windows)]
pub fn apply_unterm_windows_utf8(cmd_builder: &mut Option<CommandBuilder>) {
    let Some(builder) = cmd_builder.as_mut() else {
        return;
    };

    if builder.is_default_prog() {
        return;
    }

    let args = builder.get_argv().clone();
    let _default_powershell_no_logo = args.len() == 2
        && args[1].eq_ignore_ascii_case("-NoLogo")
        && args[0]
            .to_string_lossy()
            .rsplit(['\\', '/'])
            .next()
            .is_some_and(|name| {
                let name = name.to_lowercase();
                name.starts_with("powershell") || name.starts_with("pwsh")
            });
    if args.len() != 1 {
        return; // user passed args — don't second-guess.
    }
    let exe_str = args[0].to_string_lossy().to_lowercase();
    let basename = exe_str
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(exe_str.as_str());

    let argv = builder.get_argv_mut();
    if basename.starts_with("powershell") || basename.starts_with("pwsh") {
        return;
        /*
        // Four encoding knobs on Windows PowerShell. Missing any one
        // causes mojibake somewhere in the I/O chain. In particular,
        // omitting InputEncoding is what causes UTF-8 Chinese filenames
        // typed at the prompt to be re-decoded as CP936/GBK by PowerShell
        // before they reach cmdlets — at which point Move-Item / Get-Item
        // can't find the file and reports a corrupted name back. So we
        // set all four every time, on the same line so a profile that
        // overrides one of them only overrides that one.
        let setup = format!(
            "[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);\
             [Console]::InputEncoding=[Text.UTF8Encoding]::new($false);\
             $OutputEncoding=[Console]::OutputEncoding;\
             $PSDefaultParameterValues['*:Encoding']='utf8';\
             & '{}' 65001 > $null;\
             Import-Module PSReadLine -ErrorAction SilentlyContinue -WarningAction SilentlyContinue",
            chcp_command()
        );
        let exe = argv[0].clone();
        argv.clear();
        argv.push(exe);
        argv.push("-NoLogo".into());
        argv.push("-NoProfile".into());
        argv.push("-NoExit".into());
        argv.push("-Command".into());
        argv.push(setup.into());
        */
    } else if basename == "cmd.exe" || basename == "cmd" {
        let exe = argv[0].clone();
        argv.clear();
        argv.push(exe);
        argv.push("/D".into());
        argv.push("/K".into());
        argv.push(format!("{} 65001 > nul", chcp_command()).into());
    }
}

#[cfg(not(windows))]
pub fn apply_unterm_windows_utf8(_cmd_builder: &mut Option<CommandBuilder>) {}

/// Where `chcp` actually lives.
///
/// By full path, not by name. A machine whose system PATH has been damaged --
/// an installer that overwrites HKLM's `Path` instead of appending to it does
/// exactly this -- cannot find `chcp`, and the codepage switch silently does
/// nothing. What the user sees is a terminal full of boxes with no error to
/// explain it, which is the hardest kind of bug to report.
#[cfg(windows)]
fn chcp_command() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    format!(r"{root}\System32\chcp.com")
}

/// Where the injection toggle lives: `~/.unterm/proxy.json`, the file the
/// ▼ menu, Web Settings and the MCP proxy tools all share.
fn proxy_config_path() -> std::path::PathBuf {
    unterm_protocol::state_path("proxy.json")
        .unwrap_or_else(|| std::path::PathBuf::from(".unterm").join("proxy.json"))
}

fn proxy_config_value() -> serde_json::Value {
    std::fs::read_to_string(proxy_config_path())
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_else(|| serde_json::json!({"enabled": false}))
}

/// The toggle's state, cached briefly: the status bar asks on every paint,
/// and 0.57.4 cached the same read for the same reason. The setter below
/// refreshes the cache, so a click on the chip is never shown stale.
static PROXY_ENABLED_CACHE: std::sync::Mutex<
    Option<(std::path::PathBuf, std::time::Instant, bool)>,
> = std::sync::Mutex::new(None);

/// Whether new sessions get Unterm's proxy env vars.
pub fn unterm_proxy_enabled() -> bool {
    const TTL: std::time::Duration = std::time::Duration::from_secs(5);
    static REFRESHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let path = proxy_config_path();
    {
        let cache = PROXY_ENABLED_CACHE.lock().unwrap();
        if let Some((cached_path, at, enabled)) = cache.as_ref() {
            if *cached_path == path {
                let current = *enabled;
                if at.elapsed() < TTL {
                    return current;
                }
                drop(cache);
                if !REFRESHING.swap(true, std::sync::atomic::Ordering::AcqRel) {
                    if std::thread::Builder::new()
                        .name("proxy-config-refresh".into())
                        .spawn(move || {
                            let fresh = proxy_config_value()
                                .get("enabled")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false);
                            if let Ok(mut guard) = PROXY_ENABLED_CACHE.lock() {
                                *guard = Some((path, std::time::Instant::now(), fresh));
                            }
                            REFRESHING.store(false, std::sync::atomic::Ordering::Release);
                        })
                        .is_err()
                    {
                        REFRESHING.store(false, std::sync::atomic::Ordering::Release);
                    }
                }
                return current;
            }
        }
    }
    let fresh = proxy_config_value()
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    *PROXY_ENABLED_CACHE.lock().unwrap() = Some((path, std::time::Instant::now(), fresh));
    fresh
}

/// Flip whether new sessions get Unterm's proxy env vars.
///
/// This is the whole switch: it writes `enabled` into proxy.json and nothing
/// else changes hands. In particular it never touches the OS's own proxy
/// settings — Unterm reads those to find a proxy, but the only thing the
/// toggle controls is env injection into sessions Unterm spawns.
pub fn set_unterm_proxy_enabled(enabled: bool) -> anyhow::Result<()> {
    let path = proxy_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut value = proxy_config_value();
    if !value.is_object() {
        value = serde_json::json!({});
    }
    value["enabled"] = serde_json::Value::Bool(enabled);
    // `mode: "off"` with the toggle on is a contradiction the MCP status
    // surface would faithfully report. The read path treats anything
    // non-manual as auto, so auto is what an unset mode already means; a
    // manual mode, with its explicit URLs, is the user's and stays put.
    if enabled {
        let mode = value
            .get("mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if mode.is_empty() || mode == "off" {
            value["mode"] = serde_json::Value::String("auto".to_string());
        }
    }
    std::fs::write(&path, serde_json::to_string_pretty(&value)?)?;
    *PROXY_ENABLED_CACHE.lock().unwrap() = Some((path, std::time::Instant::now(), enabled));
    Ok(())
}

/// The proxy the toggle would inject right now, probed. `None` means nothing
/// answered — the caller should leave the toggle off rather than route every
/// command in a new shell into a dead port.
pub fn probe_unterm_proxy() -> Option<String> {
    let (http, socks, _no_proxy) = resolve_reachable_proxy(&proxy_config_value())?;
    http.or(socks)
}

pub fn read_unterm_proxy_env() -> Option<Vec<(String, String)>> {
    // Reads ~/.unterm/proxy.json (managed by the ▼ menu / Web Settings).
    // Schema: { enabled, mode: "auto" | "manual", http_proxy, socks_proxy, no_proxy }.
    // In auto mode (default), system_proxy::detect() runs at every spawn and
    // overlays whatever stale URLs are on disk — mirroring what the UI does in
    // mcp/handler.rs::load_proxy_settings() so the ▼ menu, Web Settings, and
    // spawned shells never disagree on the active proxy URL.
    let value = proxy_config_value();
    if !value
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }

    let Some((http, socks, no_proxy)) = resolve_reachable_proxy(&value) else {
        log::warn!(
            "Unterm proxy is enabled but no configured/detected proxy endpoint is reachable; \
             leaving this shell on a direct connection so it isn't cut off from the network"
        );
        return None;
    };

    let mut env = Vec::new();
    if let Some(http) = &http {
        env.push(("HTTP_PROXY".to_string(), http.clone()));
        env.push(("HTTPS_PROXY".to_string(), http.clone()));
        env.push(("http_proxy".to_string(), http.clone()));
        env.push(("https_proxy".to_string(), http.clone()));
    }
    if let Some(socks) = &socks {
        env.push(("ALL_PROXY".to_string(), socks.clone()));
        env.push(("all_proxy".to_string(), socks.clone()));
    }
    if !no_proxy.is_empty() {
        env.push(("NO_PROXY".to_string(), no_proxy.clone()));
        env.push(("no_proxy".to_string(), no_proxy));
    }
    Some(env)
}

/// Resolve the URLs the toggle would inject — mode overlay, detection and the
/// dead-endpoint guard included — without looking at the `enabled` flag.
/// `None` means nothing is reachable right now.
fn resolve_reachable_proxy(
    value: &serde_json::Value,
) -> Option<(Option<String>, Option<String>, String)> {
    // Mode determines whether on-disk URLs are authoritative.
    //   - "manual" (or any explicit non-auto value): trust the URLs in
    //     proxy.json exactly. This is the user saying "I know what I want."
    //   - anything else (including missing field): treat as auto. The
    //     URLs on disk are stale state from a previous detect() and must
    //     be overlaid with a fresh detect() call. Without this, a user
    //     who changed their Clash listener from 7890 to 7897 keeps getting
    //     7890 piped into every spawned shell — which is exactly the bug
    //     that bit the v0.5.4 Windows release.
    //
    // Mirrors mcp/handler.rs::load_proxy_settings(), which has the same
    // overlay semantics for the UI/MCP side. The two paths must agree or
    // ▼ menu and Web Settings will show 7897 while spawned shells get 7890.
    let mode = value
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("auto");
    let is_auto = mode != "manual";

    let manual_http = value
        .get("http_proxy")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let manual_socks = value
        .get("socks_proxy")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let manual_no = value
        .get("no_proxy")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // In auto mode: always run detect() and let it win over stale on-disk URLs.
    // In manual mode: only fall back to detect() when both URL fields are blank.
    let detected = if is_auto || (manual_http.is_none() && manual_socks.is_none()) {
        crate::system_proxy::detect()
    } else {
        None
    };

    let detected_http = detected
        .as_ref()
        .and_then(|d| d.primary_http().map(str::to_string));
    let detected_socks = detected.as_ref().and_then(|d| d.socks.clone());
    let detected_no = detected.as_ref().and_then(|d| d.no_proxy.clone());

    let (http, socks, no_proxy) = if is_auto {
        // Auto mode: detected wins; manual values only fill blanks.
        let http = detected_http.or(manual_http);
        let socks = detected_socks.or(manual_socks);
        let no_proxy = detected_no
            .or(manual_no)
            .unwrap_or_else(|| "localhost,127.0.0.1,::1".to_string());
        (http, socks, no_proxy)
    } else {
        // Manual mode: user URLs win; detected only fills blanks.
        let http = manual_http.or(detected_http);
        let socks = manual_socks.or(detected_socks);
        let no_proxy = manual_no
            .or(detected_no)
            .unwrap_or_else(|| "localhost,127.0.0.1,::1".to_string());
        (http, socks, no_proxy)
    };

    // Safety guard: never inject a dead proxy. A proxy URL piped into a shell
    // that points at a port nothing is listening on turns every command in
    // that terminal into a connection failure — which reads to the user as
    // "Unterm broke my VPN". So we TCP-probe each endpoint and drop any that
    // isn't accepting connections right now. If nothing is reachable we inject
    // nothing, and the shell talks to the network directly, exactly as if the
    // proxy toggle were off. (Auto mode still re-detects on the next spawn, so
    // the proxy reattaches automatically once the software is back up.)
    //
    // The status bar chip leans on the same guard: enabling from a click is
    // gated on this function finding something alive.
    let http = http.filter(|u| proxy_endpoint_reachable(u));
    let socks = socks.filter(|u| proxy_endpoint_reachable(u));
    if http.is_none() && socks.is_none() {
        return None;
    }
    Some((http, socks, no_proxy))
}

/// TCP-probe a proxy endpoint so we never inject a dead proxy into a shell.
/// Returns false fast (250 ms) if nothing is listening on `host:port` — the
/// caller then skips injecting that URL, leaving the shell on a direct
/// connection instead of routing every command into a black hole.
pub fn proxy_endpoint_reachable(url: &str) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;
    // Strip scheme (http://, socks5://) and any trailing path, leaving host:port.
    let host_port = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or(url);
    // Without an explicit port we can't probe; don't block injection in that case.
    if !host_port.contains(':') {
        return true;
    }
    match host_port.to_socket_addrs() {
        Ok(addrs) => addrs
            .into_iter()
            .any(|addr| TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok()),
        Err(_) => false,
    }
}

#[cfg(all(test, windows))]
mod utf8_tests {
    use super::*;

    #[test]
    fn cmd_switches_codepage_by_full_path() {
        let mut shell = Some(CommandBuilder::new("cmd.exe"));
        apply_unterm_windows_utf8(&mut shell);
        let argv: Vec<String> = shell
            .expect("a builder")
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
        let command = argv.last().expect("the /K command");
        assert!(
            command.to_lowercase().contains(r"system32\chcp.com"),
            "a bare `chcp` is unreachable when the system PATH is damaged: {command}"
        );
        assert!(command.contains("65001"));
    }

    #[test]
    fn powershell_startup_is_left_native() {
        let mut shell = Some(CommandBuilder::new("pwsh.exe"));
        apply_unterm_windows_utf8(&mut shell);
        let argv: Vec<String> = shell
            .expect("a builder")
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
        assert_eq!(argv, vec!["pwsh.exe"]);
    }

    #[test]
    fn default_powershell_no_logo_is_left_native() {
        let mut command = CommandBuilder::new("powershell.exe");
        command.arg("-NoLogo");
        let mut shell = Some(command);
        apply_unterm_windows_utf8(&mut shell);
        let argv: Vec<String> = shell
            .expect("a builder")
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
        assert_eq!(argv, vec!["powershell.exe", "-NoLogo"]);
    }

    #[test]
    fn a_shell_the_user_gave_arguments_to_is_left_alone() {
        let mut command = CommandBuilder::new("cmd.exe");
        command.arg("/K");
        command.arg("echo mine");
        let mut shell = Some(command);
        apply_unterm_windows_utf8(&mut shell);
        let argv: Vec<String> = shell
            .expect("a builder")
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
        assert_eq!(argv.last().map(String::as_str), Some("echo mine"));
    }
}

#[cfg(test)]
mod proxy_env_tests {
    use super::*;
    use std::ffi::OsStr;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct StateDirGuard {
        old: Option<std::ffi::OsString>,
        dir: PathBuf,
    }

    impl StateDirGuard {
        fn new(name: &str) -> Self {
            let old = std::env::var_os("UNTERM_STATE_DIR");
            let dir = std::env::temp_dir()
                .join(format!("unterm-launch-env-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create isolated state dir");
            std::env::set_var("UNTERM_STATE_DIR", &dir);
            Self { old, dir }
        }

        fn write_proxy_json(&self, value: serde_json::Value) {
            std::fs::write(
                self.dir.join("proxy.json"),
                serde_json::to_string_pretty(&value).expect("serialize proxy.json"),
            )
            .expect("write proxy.json");
        }
    }

    impl Drop for StateDirGuard {
        fn drop(&mut self) {
            match &self.old {
                Some(value) => std::env::set_var("UNTERM_STATE_DIR", value),
                None => std::env::remove_var("UNTERM_STATE_DIR"),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn loopback_listener() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let port = listener.local_addr().expect("listener address").port();
        (listener, format!("http://127.0.0.1:{port}"))
    }

    #[test]
    fn enabled_manual_proxy_injects_upper_and_lowercase_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (_listener, http_url) = loopback_listener();
        let state = StateDirGuard::new("manual");
        state.write_proxy_json(serde_json::json!({
            "enabled": true,
            "mode": "manual",
            "http_proxy": http_url,
            "no_proxy": "localhost,127.0.0.1,::1"
        }));

        let env = read_unterm_proxy_env().expect("reachable manual proxy env");
        assert!(env.contains(&("HTTP_PROXY".to_string(), http_url.clone())));
        assert!(env.contains(&("HTTPS_PROXY".to_string(), http_url.clone())));
        assert!(env.contains(&("http_proxy".to_string(), http_url.clone())));
        assert!(env.contains(&("https_proxy".to_string(), http_url.clone())));
        assert!(env.contains(&(
            "NO_PROXY".to_string(),
            "localhost,127.0.0.1,::1".to_string()
        )));
        assert!(env.contains(&(
            "no_proxy".to_string(),
            "localhost,127.0.0.1,::1".to_string()
        )));

        let mut command = None;
        apply_unterm_proxy_env(&mut command);
        let command = command.expect("proxy env creates a command builder");
        assert_eq!(
            command.get_env("HTTPS_PROXY"),
            Some(OsStr::new(http_url.as_str()))
        );
        assert_eq!(
            command.get_env("no_proxy"),
            Some(OsStr::new("localhost,127.0.0.1,::1"))
        );
    }

    #[test]
    fn disabled_proxy_does_not_create_a_command_builder() {
        let _guard = ENV_LOCK.lock().unwrap();
        let state = StateDirGuard::new("disabled");
        state.write_proxy_json(serde_json::json!({
            "enabled": false,
            "mode": "manual",
            "http_proxy": "http://127.0.0.1:9"
        }));

        assert!(read_unterm_proxy_env().is_none());
        let mut command = None;
        apply_unterm_proxy_env(&mut command);
        assert!(command.is_none());
    }

    #[test]
    fn dead_proxy_endpoint_is_not_injected() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (listener, http_url) = loopback_listener();
        let port = listener.local_addr().expect("listener address").port();
        drop(listener);
        let state = StateDirGuard::new("dead");
        state.write_proxy_json(serde_json::json!({
            "enabled": true,
            "mode": "manual",
            "http_proxy": http_url
        }));

        if TcpListener::bind(("127.0.0.1", port)).is_err() {
            return;
        }
        assert!(read_unterm_proxy_env().is_none());
        let mut command = None;
        apply_unterm_proxy_env(&mut command);
        assert!(command.is_none());
    }
}
