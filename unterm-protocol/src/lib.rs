//! Shared wire types and version identity for every Unterm process.
//!
//! This crate deliberately has no dependency on the GUI, engine, MCP server,
//! or storage. It is the compatibility boundary used before those components
//! are split into independent processes.

use serde::{Deserialize, Serialize};

/// Installed product version. All shipped binaries obtain it from this crate.
pub const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Source revision embedded at build time by `build.rs`.
pub const BUILD_COMMIT: &str = env!("UNTERM_BUILD_COMMIT");
/// Unterm local control-plane protocol. Bump the major version for breaking changes.
pub const PROTOCOL_VERSION: &str = "1.0.0";
/// Durable data schema. M0 has no task database yet, so this is the registry schema.
pub const DATA_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessRole {
    #[default]
    Gui,
    Cli,
    McpBridge,
    Core,
    BrainAdapter,
    Provider,
}

/// Minimum identity returned by every process handshake.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildHandshake {
    pub product_version: String,
    pub build_commit: String,
    pub protocol_version: String,
    pub data_schema_version: u32,
    pub process_role: ProcessRole,
    pub pid: u32,
    pub started_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compatibility {
    Compatible,
    /// A pre-M0 peer with the same product version but no protocol metadata.
    Legacy,
    ProductVersionMismatch,
    ProtocolIncompatible,
    DataSchemaIncompatible,
}

impl Compatibility {
    pub fn is_usable(self) -> bool {
        matches!(self, Self::Compatible | Self::Legacy)
    }

    pub fn error_code(self) -> Option<&'static str> {
        match self {
            Self::Compatible | Self::Legacy => None,
            Self::ProductVersionMismatch => Some("product_version_mismatch"),
            Self::ProtocolIncompatible => Some("protocol_incompatible"),
            Self::DataSchemaIncompatible => Some("data_schema_incompatible"),
        }
    }
}

impl BuildHandshake {
    pub fn current(process_role: ProcessRole, pid: u32, started_at: impl Into<String>) -> Self {
        Self {
            product_version: PRODUCT_VERSION.to_string(),
            build_commit: BUILD_COMMIT.to_string(),
            protocol_version: PROTOCOL_VERSION.to_string(),
            data_schema_version: DATA_SCHEMA_VERSION,
            process_role,
            pid,
            started_at: started_at.into(),
        }
    }

    pub fn is_protocol_compatible(&self) -> bool {
        protocol_major(&self.protocol_version) == protocol_major(PROTOCOL_VERSION)
    }

    /// Decide whether this binary may safely talk to `peer`.
    ///
    /// Product versions are intentionally exact during M0 so an upgraded GUI
    /// cannot silently retain an older long-lived bridge. Pre-M0 peers are
    /// accepted only when their legacy `version` still equals this product.
    pub fn compatibility(&self) -> Compatibility {
        if self.product_version != PRODUCT_VERSION {
            return Compatibility::ProductVersionMismatch;
        }
        if self.protocol_version.is_empty() || self.protocol_version == "legacy" {
            return Compatibility::Legacy;
        }
        if !self.is_protocol_compatible() {
            return Compatibility::ProtocolIncompatible;
        }
        if self.data_schema_version > DATA_SCHEMA_VERSION {
            return Compatibility::DataSchemaIncompatible;
        }
        Compatibility::Compatible
    }
}

fn protocol_major(version: &str) -> Option<&str> {
    version.split('.').next().filter(|part| !part.is_empty())
}

/// Where this process keeps its durable state.
///
/// `UNTERM_STATE_DIR` replaces `~/.unterm` **wholesale**: a test, a headless
/// Core, or a second install that sets it must not touch the real user's
/// registries, config, recordings, or trust lists. Nothing may reach for
/// `home_dir().join(".unterm")` directly -- every such site is a place where
/// an isolated run quietly writes into the user's home, and there were 55 of
/// them before this existed.
///
/// Returns `None` only when there is no override and no home directory, which
/// is the same condition every caller already had to handle.
/// Where the Core keeps its discovery record and instance lock.
///
/// **Not the same directory as [`state_dir`]**, and that is deliberate: the
/// Core's record is machine state for one process, and it has lived under the
/// platform's data directory since M1. It is here — in the crate both the
/// Core and everything that reads it depend on — because when the two
/// resolutions were written separately they diverged, and the symptom was
/// `supervisor.status` reporting "absent" for a Core that was running.
///
/// `UNTERM_STATE_DIR` overrides both, which is exactly why the divergence
/// stayed hidden: every test that set it saw the two agree.
/// The product name a window shows (its title bar, the window title an agent
/// reads back). `Unterm`, unless the host that launched it says otherwise
/// with `UNTERM_PRODUCT_NAME` -- an application that ships this terminal as
/// one of its own features names its windows after itself.
pub fn product_name() -> String {
    std::env::var("UNTERM_PRODUCT_NAME")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty() && name.len() <= 64 && !name.chars().any(char::is_control))
        .unwrap_or_else(|| "Unterm".to_string())
}

pub fn core_state_dir() -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("UNTERM_STATE_DIR") {
        if !dir.is_empty() {
            return Some(std::path::PathBuf::from(dir));
        }
    }
    dirs_next::data_local_dir().map(|dir| dir.join("Unterm"))
}

/// The Core's discovery record: where it is listening, and who it is.
pub fn core_discovery_path() -> Option<std::path::PathBuf> {
    core_state_dir().map(|dir| dir.join("core.json"))
}

pub fn state_dir() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("UNTERM_STATE_DIR") {
        if !dir.is_empty() {
            return Some(std::path::PathBuf::from(dir));
        }
    }
    dirs_next::home_dir().map(|home| home.join(".unterm"))
}

/// [`state_dir`] joined with `name`, for the common one-liner case.
pub fn state_path(name: impl AsRef<std::path::Path>) -> Option<std::path::PathBuf> {
    state_dir().map(|dir| dir.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_can_name_the_windows_after_itself() {
        std::env::remove_var("UNTERM_PRODUCT_NAME");
        assert_eq!(product_name(), "Unterm");
        std::env::set_var("UNTERM_PRODUCT_NAME", "  Unzoo One ");
        assert_eq!(product_name(), "Unzoo One");
        // Nothing that could break a title bar or impersonate a long string.
        std::env::set_var("UNTERM_PRODUCT_NAME", "bad\nname");
        assert_eq!(product_name(), "Unterm");
        std::env::set_var("UNTERM_PRODUCT_NAME", "x".repeat(65));
        assert_eq!(product_name(), "Unterm");
        std::env::remove_var("UNTERM_PRODUCT_NAME");
    }

    /// The Core's record and the user state directory are two places.
    ///
    /// `core_state_dir` is the platform data directory; `state_dir` is
    /// `~/.unterm`. Code that wants the Core's record and reaches for
    /// `state_path("core.json")` finds nothing, always -- which is how a
    /// running Core got reported as absent, and how the MCP surface's own
    /// reader silently missed every time. `UNTERM_STATE_DIR` collapses the
    /// two, and every test that set it saw them agree, so nothing caught it.
    /// This one deliberately does not set it.
    #[test]
    fn the_core_record_does_not_live_in_the_user_state_directory() {
        let saved = std::env::var_os("UNTERM_STATE_DIR");
        std::env::remove_var("UNTERM_STATE_DIR");

        let core = core_discovery_path();
        let user = state_path("core.json");

        if let Some(value) = saved {
            std::env::set_var("UNTERM_STATE_DIR", value);
        }

        // Either both resolve and differ, or the platform gave us neither.
        if let (Some(core), Some(user)) = (core, user) {
            assert_ne!(
                core, user,
                "reading the Core's record from the user state directory finds nothing"
            );
        }
    }

    #[test]
    fn handshake_contains_every_required_identity_field() {
        let value = serde_json::to_value(BuildHandshake::current(
            ProcessRole::Core,
            42,
            "2026-08-03T00:00:00Z",
        ))
        .unwrap();
        for field in [
            "product_version",
            "build_commit",
            "protocol_version",
            "data_schema_version",
            "process_role",
            "pid",
            "started_at",
        ] {
            assert!(
                value.get(field).is_some(),
                "missing handshake field {field}"
            );
        }
    }

    #[test]
    fn compatibility_is_decided_by_protocol_major() {
        let mut handshake = BuildHandshake::current(ProcessRole::Provider, 7, "now");
        handshake.protocol_version = "1.99.0".into();
        assert!(handshake.is_protocol_compatible());
        handshake.protocol_version = "2.0.0".into();
        assert!(!handshake.is_protocol_compatible());
    }

    #[test]
    fn exact_product_version_prevents_a_stale_bridge() {
        let mut handshake = BuildHandshake::current(ProcessRole::Gui, 7, "now");
        handshake.product_version = "0.57.4".into();
        assert_eq!(
            handshake.compatibility(),
            Compatibility::ProductVersionMismatch
        );
        assert_eq!(
            handshake.compatibility().error_code(),
            Some("product_version_mismatch")
        );
    }

    #[test]
    fn the_state_dir_override_replaces_the_home_directory_entirely() {
        // Serialised against the other env-touching test in this module by
        // running them in one test: env vars are process-global.
        let original = std::env::var_os("UNTERM_STATE_DIR");

        std::env::set_var("UNTERM_STATE_DIR", "");
        let empty_falls_back = state_dir();
        assert_eq!(
            empty_falls_back,
            dirs_next::home_dir().map(|home| home.join(".unterm")),
            "an empty override should mean 'not set', not 'use the empty path'"
        );

        let sandbox = std::env::temp_dir().join("unterm-state-dir-probe");
        std::env::set_var("UNTERM_STATE_DIR", &sandbox);
        assert_eq!(state_dir(), Some(sandbox.clone()));
        assert_eq!(state_path("audit"), Some(sandbox.join("audit")));
        // Not "outside the home directory" -- on Windows the temp dir lives
        // under it. The thing that must never happen is landing back in the
        // real `~/.unterm`, which is what every leak looked like.
        assert_ne!(
            state_dir(),
            dirs_next::home_dir().map(|home| home.join(".unterm")),
            "an override still resolved to the real user's state directory"
        );

        match original {
            Some(value) => std::env::set_var("UNTERM_STATE_DIR", value),
            None => std::env::remove_var("UNTERM_STATE_DIR"),
        }
    }

    #[test]
    fn same_version_legacy_peer_remains_usable_during_migration() {
        let mut handshake = BuildHandshake::current(ProcessRole::Gui, 7, "now");
        handshake.protocol_version = "legacy".into();
        handshake.data_schema_version = 0;
        assert_eq!(handshake.compatibility(), Compatibility::Legacy);
        assert!(handshake.compatibility().is_usable());
    }
}

/// A failure, in the one shape every surface returns it in.
///
/// The point of the shape is `code`. A caller is allowed to branch on it and
/// is not allowed to parse `message`, which means `code` has to be stable in
/// a way prose never is: `message` can be reworded, translated or made more
/// specific at any time, and nothing downstream breaks. Free text alone --
/// which is what the brain's error event carried -- leaves a caller with no
/// way to tell "the model is busy, try again" from "this path is outside the
/// workspace", so the only thing it can do is show the user the raw sentence.
///
/// `retryable` is carried rather than derived, because whether a retry is
/// worth making is a property of the failure and not of its category: a
/// provider that is down and a provider that refused are both `provider.`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    /// For the log and the developer. Never matched on.
    pub message: String,
    pub retryable: bool,
    /// For the person, when there is something useful to say to them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_message: Option<String>,
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            retryable: code.is_retryable_by_default(),
            code,
            message: message.into(),
            user_message: None,
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn user_message(mut self, message: impl Into<String>) -> Self {
        self.user_message = Some(message.into());
        self
    }
}

/// The stable list. Every code's prefix is one of the ten categories the
/// contract names, and that is enforced by a test rather than by convention.
///
/// Closed on purpose: a code invented at a call site is a code nobody can
/// branch on tomorrow, because nothing stops the next call site inventing a
/// different spelling of the same thing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    ValidationBadRequest,
    ValidationOutOfScope,
    AuthTokenRejected,
    PolicyBlocked,
    ApprovalRequired,
    ApprovalDenied,
    ApprovalExpired,
    ProviderUnavailable,
    ProviderRefused,
    BrainAdapterFailed,
    BrainUnavailable,
    TaskNotFound,
    ArtifactMissing,
    StorageUnavailable,
    InternalUnexpected,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ValidationBadRequest => "validation.bad_request",
            Self::ValidationOutOfScope => "validation.out_of_scope",
            Self::AuthTokenRejected => "auth.token_rejected",
            Self::PolicyBlocked => "policy.blocked",
            Self::ApprovalRequired => "approval.required",
            Self::ApprovalDenied => "approval.denied",
            Self::ApprovalExpired => "approval.expired",
            Self::ProviderUnavailable => "provider.unavailable",
            Self::ProviderRefused => "provider.refused",
            Self::BrainAdapterFailed => "brain.adapter_failed",
            Self::BrainUnavailable => "brain.unavailable",
            Self::TaskNotFound => "task.not_found",
            Self::ArtifactMissing => "artifact.missing",
            Self::StorageUnavailable => "storage.unavailable",
            Self::InternalUnexpected => "internal.unexpected",
        }
    }

    /// Whether a caller that simply tries again has any reason to expect a
    /// different answer. A refusal does not become an acceptance by being
    /// asked twice; a service that is down may well come back.
    pub fn is_retryable_by_default(self) -> bool {
        matches!(
            self,
            Self::ProviderUnavailable | Self::BrainUnavailable | Self::StorageUnavailable
        )
    }

    /// Every code the contract admits, so a test can walk them.
    pub const ALL: &'static [ErrorCode] = &[
        Self::ValidationBadRequest,
        Self::ValidationOutOfScope,
        Self::AuthTokenRejected,
        Self::PolicyBlocked,
        Self::ApprovalRequired,
        Self::ApprovalDenied,
        Self::ApprovalExpired,
        Self::ProviderUnavailable,
        Self::ProviderRefused,
        Self::BrainAdapterFailed,
        Self::BrainUnavailable,
        Self::TaskNotFound,
        Self::ArtifactMissing,
        Self::StorageUnavailable,
        Self::InternalUnexpected,
    ];
}

/// The ten prefixes a code may carry.
pub const ERROR_CATEGORIES: &[&str] = &[
    "validation.",
    "auth.",
    "policy.",
    "approval.",
    "provider.",
    "brain.",
    "task.",
    "artifact.",
    "storage.",
    "internal.",
];

impl Serialize for ErrorCode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        ErrorCode::ALL
            .iter()
            .copied()
            .find(|code| code.as_str() == raw)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown error code {raw:?}")))
    }
}

#[cfg(test)]
mod contract_error_tests {
    use super::*;

    /// The contract lets a caller branch on `code`, which only works if the
    /// codes are a closed list with prefixes somebody agreed to. A code that
    /// belongs to no category is one no consumer can route.
    #[test]
    fn every_error_code_sits_under_one_of_the_ten_categories() {
        for code in ErrorCode::ALL {
            let text = code.as_str();
            let category = ERROR_CATEGORIES
                .iter()
                .find(|prefix| text.starts_with(**prefix));
            assert!(category.is_some(), "{text} belongs to no category");
            assert!(
                text.len() > category.unwrap().len(),
                "{text} is a bare category with nothing after the dot"
            );
        }
    }

    /// Spellings cross the wire; a rename is a silent break for anything
    /// matching on them, so they round-trip rather than being re-derived.
    #[test]
    fn an_error_code_survives_the_wire_unchanged() {
        for code in ErrorCode::ALL {
            let body = ErrorBody::new(*code, "something went wrong");
            let text = serde_json::to_string(&body).expect("serialize");
            assert!(text.contains(code.as_str()), "{text}");
            let back: ErrorBody = serde_json::from_str(&text).expect("deserialize");
            assert_eq!(back.code, *code);
            assert_eq!(back.retryable, code.is_retryable_by_default());
            // Absent rather than null: a consumer checking for the field
            // should not find one holding nothing.
            assert!(!text.contains("user_message"), "{text}");
        }
    }

    /// No two codes may share a spelling, or branching on one catches both.
    #[test]
    fn no_two_error_codes_spell_the_same_thing() {
        let mut seen = std::collections::HashSet::new();
        for code in ErrorCode::ALL {
            assert!(seen.insert(code.as_str()), "duplicate code {}", code.as_str());
        }
        assert_eq!(seen.len(), ErrorCode::ALL.len());
    }
}
