//! Background "is there a newer Unterm available" poller.
//!
//! Spawns one thread on app start that hits the GitHub API every 6 hours
//! to read the latest release tag and compare it to the version baked into
//! this binary. Result is written to `~/.unterm/update_check.json` so the
//! ▼ menu, Web Settings, and the status bar can all read the same file
//! without each one reaching out to GitHub.
//!
//! Why not poll from each UI surface independently:
//! - GitHub unauth API is 60 req/hr per IP. Three UI surfaces × every
//!   render would burn it.
//! - The user shouldn't pay any latency for an update check on every menu
//!   open. The file read is microseconds; the API call happens once a
//!   quarter day in the background.
//! - One source of truth = consistent state across surfaces.
//!
//! When does the poller bump the flag from absent → true?
//! - Tag from API > current binary version (semver compare).
//! - The tag string is stored verbatim so the UI can show "v0.6.0
//!   available" rather than just "an update".

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60); // 6h
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(20); // give the GUI a moment to settle
const REPO: &str = "zhitongblog/unterm";

static STARTED: AtomicBool = AtomicBool::new(false);

fn state_path() -> Option<PathBuf> {
    unterm_protocol::state_path("update_check.json")
}

/// Spawn the background poller. Idempotent — multiple calls are no-ops
/// after the first. Call from app startup, after the GUI is up.
pub fn start_background_poller() {
    if !unterm_protocol::self_updating() || STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("unterm-update-check".into())
        .spawn(move || {
            std::thread::sleep(FIRST_CHECK_DELAY);
            loop {
                if let Err(e) = check_once() {
                    log::debug!("update_check: {e:#}");
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        })
        .ok();
}

/// One round of "ask GitHub, compare, write result". Public so the
/// /api/updates POST handler can fire a manual recheck.
pub fn check_once() -> anyhow::Result<()> {
    if !unterm_protocol::self_updating() {
        anyhow::bail!("{} is updated by its own installer", unterm_protocol::product_name());
    }
    let current = unterm_services::VERSION.to_string();
    let latest = fetch_latest_tag()?;
    // Strip leading 'v' on both sides — our tags are vX.Y.Z, the binary
    // version stamp is `<date>-<time>-<sha>` so straight string compare
    // would never match. Use semver-style numeric compare on the X.Y.Z
    // part of the *tag*. The binary's "version" baked at build time is
    // a timestamp-and-commit-hash, not a semver — so we look for the tag
    // identifier in `wezterm-gui` Cargo.toml's [package].version which
    // ships as `crate::wezterm_version()` returning a build stamp. To
    // get the "real" semver we instead read it from CARGO_PKG_VERSION.
    let pkg = env!("CARGO_PKG_VERSION").to_string(); // e.g. "0.5.4"
    let upgrade = is_newer(&latest, &pkg);

    let path = state_path().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::json!({
        "checked_at": chrono::Local::now().to_rfc3339(),
        "current": current,
        "current_pkg": pkg,
        "latest_tag": latest,
        "upgrade_available": upgrade,
    });
    std::fs::write(&path, serde_json::to_string_pretty(&body)?)?;
    log::debug!(
        "update_check: pkg={} latest={} upgrade={}",
        body["current_pkg"],
        latest,
        upgrade
    );
    Ok(())
}

/// Hit api.github.com for the latest release tag. No auth — relies on
/// the unauth rate limit (60/hr/IP) which is plenty for one check
/// every 6h plus the occasional manual recheck.
fn fetch_latest_tag() -> anyhow::Result<String> {
    use http_req::request::Request;
    use http_req::uri::Uri;
    use std::convert::TryFrom;

    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let uri = Uri::try_from(url.as_str())?;
    let mut buf: Vec<u8> = Vec::new();
    let _resp = Request::new(&uri)
        .header("User-Agent", "unterm-update-check")
        .header("Accept", "application/vnd.github+json")
        .send(&mut buf)?;
    let text = String::from_utf8(buf)?;
    parse_tag(&text)
}

fn parse_tag(json_text: &str) -> anyhow::Result<String> {
    let v: serde_json::Value = serde_json::from_str(json_text)?;
    let tag = v
        .get("tag_name")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow::anyhow!("no tag_name in github response"))?;
    Ok(tag.to_string())
}

/// Is `latest_tag` (e.g. "v0.6.0") strictly newer than `current_pkg`
/// (e.g. "0.5.4")? Lightweight three-component numeric compare.
/// Returns false on any parse error so we never raise a phantom upgrade.
fn is_newer(latest_tag: &str, current_pkg: &str) -> bool {
    fn parts(s: &str) -> Option<(u32, u32, u32)> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut it = s.split('.');
        let a: u32 = it.next()?.parse().ok()?;
        let b: u32 = it.next()?.parse().ok()?;
        let c: u32 = it.next()?.parse().ok()?;
        Some((a, b, c))
    }
    match (parts(latest_tag), parts(current_pkg)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Read the cached state for UI surfaces (and /api/updates GET).
pub fn read_state() -> serde_json::Value {
    let Some(path) = state_path() else {
        return serde_json::json!({"upgrade_available": false});
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s)
            .unwrap_or_else(|_| serde_json::json!({"upgrade_available": false})),
        Err(_) => serde_json::json!({"upgrade_available": false}),
    }
}
