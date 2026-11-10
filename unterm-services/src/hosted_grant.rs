//! "This connection's tool calls were already approved by a host."
//!
//! A *governed* hosted session runs an agent with its own shell tools
//! switched off and Unterm's MCP mounted instead, so every command it runs
//! goes through Unterm. Each of those MCP tool calls is a permission question
//! the host has already put to a person (or to a standing rule) before the
//! CLI makes it. Unterm's PTY write gate would then ask a second time -- in a
//! window, or, headless, simply refuse -- for the same call.
//!
//! The grant closes that gap without opening a new one. The session layer
//! issues a random token when a governed session starts and hands it to the
//! agent in `UNTERM_HOSTED_GRANT`; the MCP bridge the agent spawns inherits
//! it and presents it at `auth.login`; the server marks the connection, and
//! the write gate lets it through (the gateway's own policy still applies)
//! with an audit entry naming the session.
//!
//! Why this does not let an agent approve itself: the agent's shell tools are
//! off, so the only way it reaches Unterm is through MCP tool calls, and
//! every one of those is a question the host answers first. The token is
//! worthless outside that loop, and it dies with the session.
//!
//! Only a hash of the token is written, under the state directory -- the
//! session may live in one process (the GUI's settings server) and the MCP
//! connection land in another (the Core), so the record has to be readable
//! by both. A grant whose issuing process is gone is treated as revoked.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const ENV: &str = "UNTERM_HOSTED_GRANT";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    pub session_id: String,
    pub owner_pid: u32,
    pub issued_at: String,
}

fn dir() -> Result<PathBuf> {
    unterm_protocol::state_path("hosted_grants").context("could not resolve the state directory")
}

fn digest(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A fresh grant for `session_id`. Returns the token -- the only copy.
pub fn issue(session_id: &str) -> Result<String> {
    let token = format!(
        "hg_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let dir = dir()?;
    std::fs::create_dir_all(&dir)?;
    let grant = Grant {
        session_id: session_id.to_string(),
        owner_pid: std::process::id(),
        issued_at: chrono::Utc::now().to_rfc3339(),
    };
    std::fs::write(dir.join(format!("{}.json", digest(&token))), serde_json::to_vec(&grant)?)?;
    Ok(token)
}

/// The grant behind `token`, if it is live.
pub fn verify(token: &str) -> Option<Grant> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let path = dir().ok()?.join(format!("{}.json", digest(token)));
    let grant: Grant = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    if !crate::server_info::pid_alive(grant.owner_pid) {
        // The process that hosted the session is gone, and the session
        // with it. Clean up rather than honour it.
        let _ = std::fs::remove_file(&path);
        return None;
    }
    Some(grant)
}

/// End the grant. Idempotent.
pub fn revoke(token: &str) {
    if let Ok(dir) = dir() {
        let _ = std::fs::remove_file(dir.join(format!("{}.json", digest(token))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_is_live_until_revoked_and_only_its_hash_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("UNTERM_STATE_DIR", dir.path());
        let token = issue("ags_1").unwrap();
        let grant = verify(&token).expect("live");
        assert_eq!(grant.session_id, "ags_1");
        assert_eq!(grant.owner_pid, std::process::id());

        // The token itself is nowhere on disk.
        for entry in std::fs::read_dir(dir.path().join("hosted_grants")).unwrap() {
            let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
            assert!(!text.contains(&token));
        }

        assert!(verify("hg_not_a_grant").is_none());
        assert!(verify("").is_none());
        revoke(&token);
        assert!(verify(&token).is_none());
        revoke(&token); // idempotent
    }

    #[test]
    fn a_grant_whose_owner_is_gone_is_void() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("UNTERM_STATE_DIR", dir.path());
        let token = issue("ags_2").unwrap();
        let path = dir.path().join("hosted_grants").join(format!("{}.json", digest(&token)));
        let mut grant: Grant = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        grant.owner_pid = u32::MAX - 7; // no such process
        std::fs::write(&path, serde_json::to_vec(&grant).unwrap()).unwrap();
        assert!(verify(&token).is_none());
        assert!(!path.exists(), "a dead grant is cleaned up");
    }
}
