//! Finding providers without knowing where they are.
//!
//! The rule this module exists to keep: **nothing here contains a port**. A
//! provider that moved must still be found, and — more importantly — one that
//! is not running must not be confused with whatever process happens to have
//! taken the port it used last week. Every endpoint comes from something the
//! provider itself wrote down.
//!
//! Three sources, in this order:
//!
//! 1. **The environment.** `UNTERM_PROVIDER_<ID>` points at an endpoint.
//!    First because an operator overriding something has a reason, and a
//!    discovery that quietly outvotes them is one they cannot debug.
//! 2. **Unterm's own descriptor directory**, `providers/*.json` under the
//!    state dir. Any provider — including ones written after this build — can
//!    announce itself by dropping a file there.
//! 3. **A provider's native advertisement.** Unzoo writes its REST port into
//!    its own config directory; reading that is how a user who installed the
//!    browser gets a working binding without configuring anything.

use crate::{Capability, Endpoint, Identity, ProviderManifest};
use std::path::PathBuf;

/// The MCP protocol versions Unterm speaks, newest first.
///
/// A provider that offers none of these is refused rather than guessed at.
pub const PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Where Unterm keeps its own state: the product's, see
/// `unterm_protocol::state_dir`.
fn state_dir() -> PathBuf {
    unterm_protocol::state_dir().unwrap_or_else(|| dirs_home().join(".unterm"))
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Where a provider can drop a descriptor to announce itself.
pub fn descriptor_dir() -> PathBuf {
    state_dir().join("providers")
}

/// Everything that can be found right now.
///
/// Later sources never override earlier ones: the first manifest found for an
/// id wins, so an operator's override survives a provider re-announcing
/// itself.
pub fn discover() -> Vec<ProviderManifest> {
    let mut found: Vec<ProviderManifest> = Vec::new();
    let mut push = |manifest: ProviderManifest| {
        if !found.iter().any(|existing| existing.id == manifest.id) {
            found.push(manifest);
        }
    };

    for manifest in from_environment() {
        push(manifest);
    }
    for manifest in from_descriptors(&descriptor_dir()) {
        push(manifest);
    }
    for manifest in crate::unzoo::advertised() {
        push(manifest);
    }
    found
}

/// Find one by id.
pub fn find(id: &str) -> Option<ProviderManifest> {
    discover().into_iter().find(|manifest| manifest.id == id)
}

/// `UNTERM_PROVIDER_UNZOO=http://127.0.0.1:51234/mcp`
///
/// A bare URL is the common case. A JSON object is accepted for the endpoints
/// a URL cannot express, because an operator pinning a stdio provider should
/// not have to write a descriptor file to do it.
pub fn from_environment() -> Vec<ProviderManifest> {
    let mut found = Vec::new();
    for (key, value) in std::env::vars() {
        let Some(id) = key.strip_prefix("UNTERM_PROVIDER_") else {
            continue;
        };
        if id.is_empty() || value.trim().is_empty() {
            continue;
        }
        let id = id.to_ascii_lowercase();
        let endpoint = if value.trim_start().starts_with('{') {
            match serde_json::from_str::<Endpoint>(&value) {
                Ok(endpoint) => endpoint,
                Err(_) => continue,
            }
        } else {
            Endpoint::Http {
                url: value.trim().to_string(),
            }
        };
        found.push(ProviderManifest {
            name: id.clone(),
            id,
            endpoint,
            protocols: PROTOCOLS.iter().map(|version| version.to_string()).collect(),
            // An override says where, not what. What it can do is settled by
            // the handshake, which is the only honest source for it.
            capabilities: Capability::ALL.to_vec(),
            // An override says where, not what: the handshake settles the
            // capabilities and the descriptor (or a built-in) the families.
            families: crate::unzoo::families(),
            source: format!("environment:{key}"),
            pinned: None,
        });
    }
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

/// Descriptors any provider can write.
pub fn from_descriptors(dir: &std::path::Path) -> Vec<ProviderManifest> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<ProviderManifest> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        match serde_json::from_str::<ProviderManifest>(&text) {
            Ok(mut manifest) => {
                manifest.source = format!("descriptor:{}", path.display());
                found.push(manifest);
            }
            // A malformed descriptor is skipped rather than fatal: one
            // provider writing nonsense must not stop the others being found.
            Err(_) => continue,
        }
    }
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

/// Write a descriptor. What a provider — or a test — uses to announce itself.
pub fn announce(manifest: &ProviderManifest) -> std::io::Result<PathBuf> {
    let dir = descriptor_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", manifest.id));
    std::fs::write(&path, serde_json::to_string_pretty(manifest).unwrap_or_default())?;
    Ok(path)
}

/// Remember who a provider turned out to be, so a change is noticed later.
pub fn pin(id: &str, identity: &Identity) -> std::io::Result<()> {
    let dir = descriptor_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join(format!("{id}.identity.json")),
        serde_json::to_string_pretty(identity).unwrap_or_default(),
    )
}

/// What was pinned for this provider, if anything.
pub fn pinned(id: &str) -> Option<Identity> {
    let text = std::fs::read_to_string(descriptor_dir().join(format!("{id}.identity.json"))).ok()?;
    serde_json::from_str(&text).ok()
}

/// Forget a pin. Part of unbinding: the next bind starts from nothing known
/// rather than from a comparison against a provider the user rejected.
pub fn unpin(id: &str) -> std::io::Result<()> {
    let path = descriptor_dir().join(format!("{id}.identity.json"));
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolated() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::testing::env_guard();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("UNTERM_STATE_DIR", dir.path());
        (dir, guard)
    }

    #[test]
    fn a_descriptor_says_where_a_provider_is_and_discovery_believes_it() {
        let _isolated = isolated();
        // The port here is arbitrary and never appears in this crate's source.
        let manifest = ProviderManifest {
            id: "somebody".into(),
            name: "Somebody".into(),
            endpoint: Endpoint::Http {
                url: "http://127.0.0.1:54321/mcp".into(),
            },
            protocols: vec!["2025-06-18".into()],
            capabilities: vec![Capability::Browser],
            families: Default::default(),
            source: String::new(),
            pinned: None,
        };
        announce(&manifest).unwrap();

        let found = find("somebody").expect("the descriptor was not discovered");
        assert_eq!(
            found.endpoint,
            Endpoint::Http {
                url: "http://127.0.0.1:54321/mcp".into()
            }
        );
        assert!(found.source.starts_with("descriptor:"), "{}", found.source);
    }

    #[test]
    fn an_operators_override_wins() {
        let _isolated = isolated();
        announce(&ProviderManifest {
            id: "somebody".into(),
            name: "Somebody".into(),
            endpoint: Endpoint::Http {
                url: "http://127.0.0.1:1/mcp".into(),
            },
            protocols: vec!["2025-06-18".into()],
            capabilities: vec![Capability::Browser],
            families: Default::default(),
            source: String::new(),
            pinned: None,
        })
        .unwrap();
        std::env::set_var("UNTERM_PROVIDER_SOMEBODY", "http://127.0.0.1:2/mcp");

        let found = find("somebody").unwrap();
        assert_eq!(
            found.endpoint,
            Endpoint::Http {
                url: "http://127.0.0.1:2/mcp".into()
            },
            "discovery outvoted the operator, who now has no way to point it anywhere"
        );
        std::env::remove_var("UNTERM_PROVIDER_SOMEBODY");
    }

    #[test]
    fn a_descriptor_full_of_nonsense_does_not_hide_the_others() {
        let _isolated = isolated();
        std::fs::create_dir_all(descriptor_dir()).unwrap();
        std::fs::write(descriptor_dir().join("broken.json"), "{ not json").unwrap();
        announce(&ProviderManifest {
            id: "fine".into(),
            name: "Fine".into(),
            endpoint: Endpoint::Http {
                url: "http://127.0.0.1:3/mcp".into(),
            },
            protocols: vec!["2025-06-18".into()],
            capabilities: vec![Capability::Browser],
            families: Default::default(),
            source: String::new(),
            pinned: None,
        })
        .unwrap();

        assert!(find("fine").is_some(), "one bad file hid a good provider");
    }

    #[test]
    fn an_identity_is_remembered_and_can_be_forgotten() {
        let _isolated = isolated();
        assert_eq!(pinned("somebody"), None);
        let identity = Identity {
            name: "unzoo-service".into(),
            version: "2.5.16".into(),
        };
        pin("somebody", &identity).unwrap();
        assert_eq!(pinned("somebody"), Some(identity));
        unpin("somebody").unwrap();
        assert_eq!(
            pinned("somebody"),
            None,
            "unbinding left the old identity behind, so re-binding would compare against it"
        );
        // Forgetting something already forgotten is not an error.
        unpin("somebody").unwrap();
    }

    #[test]
    fn this_crate_never_writes_down_a_port() {
        // The gate, checked where it can actually be checked. A literal port
        // in discovery is how "no fixed ports" quietly becomes "one fixed
        // port, plus discovery".
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let pattern = regex::Regex::new(r"127\.0\.0\.1:\d+|localhost:\d+").unwrap();
        let mut offenders = Vec::new();
        for entry in std::fs::read_dir(&src).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            // Test modules are allowed to name ports; they are asserting that
            // whatever was advertised is what gets used.
            // Comments may show an example — a line in a doc block is not an
            // endpoint anybody connects to. Tests may name ports too: they
            // are asserting that whatever was advertised is what gets used.
            let production: String = source
                .split("#[cfg(test)]")
                .next()
                .unwrap_or_default()
                .lines()
                .filter(|line| {
                    let trimmed = line.trim_start();
                    !trimmed.starts_with("//") && !trimmed.starts_with("*")
                })
                .collect::<Vec<_>>()
                .join("\n");
            if pattern.is_match(&production) {
                offenders.push(path.file_name().unwrap().to_string_lossy().to_string());
            }
        }
        assert!(
            offenders.is_empty(),
            "these hardcode an address instead of reading what the provider advertised: {offenders:?}"
        );
    }
}
