//! Unterm AI-agent integration runtime.
//!
//! This crate is the engine behind `unterm agent ...` CLI commands, the
//! Web Settings "AI Agents" panel, and the spawn-time env injection for
//! agent-bound panes. Everything an external integrator might need is
//! re-exported below.
//!
//! See `project_positioning.md` — Unterm's bet is to be "the terminal AI
//! agents can drive". This crate is the install/auth/configure half of
//! that bet (the MCP server is the drive half).
//!
//! High-level surface for callers:
//!
//! ```ignore
//! use unterm_agents::{fetch_manifests, AgentManifest, registry::SettingsState};
//!
//! let manifests = unterm_agents::fetch_manifests()?;
//! let manifest = manifests.find("claude-code").unwrap();
//! let mut state = SettingsState::load("work", "claude-code")?;
//! state.merge_defaults(&manifest.settings_schema);
//! ```

pub mod authn;
pub mod canonical;
pub mod envelope;
pub mod errors;
pub mod fetch;
pub mod installer;
pub mod launcher;
pub mod manifest;
pub mod mcp_meta;
pub mod paths;
pub mod registry;
pub mod secrets;
pub mod storage;
pub mod template;

pub use errors::{AgentError, Result};
pub use fetch::{fetch_or_fallback, FetchResult, Source};
pub use manifest::{
    AgentManifest, AuthMethod, AuthSpec, DetectSpec, EnumValue, EnvBinding, Envelope, InstallSpec,
    InstallStep, LaunchSpec, McpAutoRegister, McpSpec, PlatformInstall, ProfileDefaults,
    SettingKind, SettingSpec, Signature, StorageFile, ValidateCmd,
};

/// Convenience wrapper: fetch the envelope and index manifests by id.
pub struct ManifestSet {
    pub envelope: Envelope,
    pub source: Source,
}

impl ManifestSet {
    pub fn find(&self, agent_id: &str) -> Option<&AgentManifest> {
        let canonical = match agent_id.trim().to_ascii_lowercase().as_str() {
            "antigravity" | "antigravity-cli" => "agy",
            "claude" => "claude-code",
            "codex" => "codex-cli",
            "gemini" => "gemini-cli",
            "kimi" => "kimi-code",
            "qwen" => "qwen-code",
            "trae" | "trae-cli" | "trae_agent" => "trae-agent",
            _ => agent_id,
        };
        self.envelope
            .manifests
            .iter()
            .find(|m| m.id == canonical || m.id == agent_id)
    }

    pub fn for_current_platform(&self) -> Vec<&AgentManifest> {
        let mut v: Vec<&AgentManifest> = self
            .envelope
            .manifests
            .iter()
            .filter(|m| m.supports_current_platform())
            .collect();
        v.sort_by_key(|m| (m.popularity_rank, m.id.clone()));
        v
    }
}

/// Fetch + verify + index. Single-call API for the rest of the workspace.
pub fn fetch_manifests() -> Result<ManifestSet> {
    let FetchResult { envelope, source } = fetch_or_fallback()?;
    Ok(ManifestSet { envelope, source })
}

/// Same as [`fetch_manifests`] but never touches the network — useful for
/// unit tests + offline CI runs.
pub fn fetch_manifests_offline() -> Result<ManifestSet> {
    // Try cache, fall through to baked.
    let cache_path = paths::manifest_cache_path()?;
    if cache_path.exists() {
        if let Ok(bytes) = std::fs::read(&cache_path) {
            if let Ok(mut env) = envelope::verify_envelope(&bytes) {
                fetch::merge_newer_baked_manifests(&mut env);
                return Ok(ManifestSet {
                    envelope: env,
                    source: Source::Cache,
                });
            }
        }
    }
    let baked: &[u8] = include_bytes!("../baked/manifests-fallback.json");
    let env = envelope::verify_envelope(baked)?;
    Ok(ManifestSet {
        envelope: env,
        source: Source::Baked,
    })
}
