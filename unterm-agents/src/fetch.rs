//! Fetch a signed envelope from unterm.app, fall back through on-disk cache,
//! then baked-in fallback. Designed to be called once at Unterm startup or
//! on user-triggered refresh. All failures are non-fatal — the caller
//! always gets *some* manifest set, even if it's the placeholder.
//!
//! Flow:
//!   GET https://unterm.app/api/agents/manifests
//!     ├─ 304        → reuse on-disk cache
//!     ├─ 200 + OK   → verify → persist to cache + return
//!     ├─ 200 + bad  → log warning, fall through to cache
//!     └─ network err → fall through to cache
//!   cache:
//!     ├─ present + verifies + not expired → return
//!     └─ otherwise → fall through to baked
//!   baked:
//!     ├─ verifies + (if expired, skip strict-expiry check) → return
//!     └─ corrupt → AgentError::NoSource

use crate::envelope::verify_envelope;
use crate::errors::{AgentError, Result};
use crate::manifest::Envelope;
use crate::paths;
use std::time::Duration;

const MANIFEST_URL: &str = "https://unterm.app/api/agents/manifests";
const NETWORK_TIMEOUT: Duration = Duration::from_millis(750);

const BAKED_FALLBACK: &[u8] = include_bytes!("../baked/manifests-fallback.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Network,
    Cache,
    Baked,
}

#[derive(Debug, Clone)]
pub struct FetchResult {
    pub envelope: Envelope,
    pub source: Source,
}

pub fn fetch_or_fallback() -> Result<FetchResult> {
    let mut res = match fetch_from_network() {
        Ok(Some(env)) => FetchResult {
            envelope: env,
            source: Source::Network,
        },
        Ok(None) => load_cache_or_baked()?,
        Err(e) => {
            log::warn!("manifest network fetch failed: {e}; trying cache");
            load_cache_or_baked()?
        }
    };
    if res.source != Source::Baked {
        merge_newer_baked_manifests(&mut res.envelope);
    }
    Ok(res)
}

/// Ensure that newly-shipped agents or higher-versioned manifests baked into
/// this binary are never downgraded or hidden by an older cached/remote
/// envelope. If the remote envelope has a strictly newer `version` for an
/// agent (or a brand-new OTA agent), the remote entry is kept.
pub(crate) fn merge_newer_baked_manifests(envelope: &mut Envelope) {
    let Ok(baked) = load_baked_envelope() else {
        return;
    };
    for bm in baked.manifests {
        match envelope.manifests.iter_mut().find(|m| m.id == bm.id) {
            Some(existing) => {
                if bm.version > existing.version {
                    *existing = bm;
                }
            }
            None => {
                envelope.manifests.push(bm);
            }
        }
    }
}

/// Returns:
///   Ok(Some(env)) — fresh envelope from network, already verified + cached
///   Ok(None)      — server returned 304 (cache still good)
///   Err(_)        — network error or verification failure
fn fetch_from_network() -> Result<Option<Envelope>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(NETWORK_TIMEOUT)
        .user_agent(format!("Unterm/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| AgentError::Fetch(e.to_string()))?;

    let mut req = client.get(MANIFEST_URL);
    if let Some(etag) = read_cached_etag() {
        req = req.header("if-none-match", &etag);
    }

    let resp = req.send().map_err(|e| AgentError::Fetch(e.to_string()))?;
    let status = resp.status();
    if status.as_u16() == 304 {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(AgentError::Fetch(format!("HTTP {status}")));
    }
    let etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let bytes = resp.bytes().map_err(|e| AgentError::Fetch(e.to_string()))?;
    let envelope = verify_envelope(&bytes)?;
    if let Err(e) = persist_cache(&bytes, etag.as_deref()) {
        log::warn!("could not persist manifest cache: {e}");
    }
    Ok(Some(envelope))
}

fn load_cache_or_baked() -> Result<FetchResult> {
    if let Some(env) = try_load_cache() {
        return Ok(FetchResult {
            envelope: env,
            source: Source::Cache,
        });
    }
    let env = load_baked_envelope()?;
    Ok(FetchResult {
        envelope: env,
        source: Source::Baked,
    })
}

fn try_load_cache() -> Option<Envelope> {
    let path = paths::manifest_cache_path().ok()?;
    let bytes = std::fs::read(&path).ok()?;
    match verify_envelope(&bytes) {
        Ok(env) => Some(env),
        Err(e) => {
            log::warn!("on-disk manifest cache failed verification: {e}");
            None
        }
    }
}

/// Load the include_bytes! baked fallback. Skips the "expired" check —
/// the baked envelope ships with a far-future expiry so this normally
/// passes, but if a release ever shipped with an expired baked fallback
/// we still want to be able to use it (it's better than nothing).
fn load_baked_envelope() -> Result<Envelope> {
    // First try the strict-verify path.
    match verify_envelope(BAKED_FALLBACK) {
        Ok(env) => Ok(env),
        Err(e) => {
            log::warn!("baked fallback failed strict verification: {e}; reading anyway");
            // Last-resort: parse without signature verification. Only fields
            // we expose are the manifests list, and `installer.rs` / the CLI
            // re-check everything before executing any command — so reading
            // an unsigned local file is OK as a last fallback.
            let parsed: Envelope = serde_json::from_slice(BAKED_FALLBACK)
                .map_err(|e| AgentError::ParseFailed(e.to_string()))?;
            Ok(parsed)
        }
    }
}

fn read_cached_etag() -> Option<String> {
    let path = paths::manifest_etag_path().ok()?;
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

fn persist_cache(raw: &[u8], etag: Option<&str>) -> Result<()> {
    paths::ensure_dirs()?;
    let cache_path = paths::manifest_cache_path()?;
    atomic_write(&cache_path, raw)?;
    if let Some(etag) = etag {
        let etag_path = paths::manifest_etag_path()?;
        atomic_write(&etag_path, etag.as_bytes())?;
    }
    Ok(())
}

fn atomic_write(target: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = target.parent().expect("cache path has parent");
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        target.file_name().unwrap_or_default().to_string_lossy()
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, target)?;
    Ok(())
}
