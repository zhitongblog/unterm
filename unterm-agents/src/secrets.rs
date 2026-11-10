//! Thin wrapper over [`unterm_profile::SecretStore`] for agent secrets.
//!
//! We deliberately reuse the existing profile secret store (one OS-keychain
//! entry per (profile_id, env_name)) rather than carve out an "agent" vault.
//! Effect: if the user already has `ANTHROPIC_API_KEY` stored under their
//! `work` profile for, say, the gh CLI or as a static env, every agent in
//! that profile picks it up automatically. One key, many consumers.
//!
//! Namespaces in the manifest's `settings_storage.env_at_launch` map to
//! env var names directly:
//!   `{ "from": "secret:anthropic" }` → key under env name `ANTHROPIC_API_KEY`
//!   `{ "from": "secret:openai" }`    → key under env name `OPENAI_API_KEY`
//!   etc.
//!
//! The mapping namespace→env-var is intentionally lossy/conventional: the
//! authoring tool checks that every `secret:<ns>` reference resolves to
//! an env var declared somewhere, so missing entries are caught before
//! signing.

use crate::errors::{AgentError, Result};
use unterm_profile::{default_store, SecretKey, SecretStore};

/// Returns the env-var name we use as the keychain account for a given
/// namespace. This convention is also the env var name that gets exported
/// into the agent process at launch.
pub fn env_var_for_namespace(namespace: &str) -> String {
    match namespace.to_ascii_lowercase().as_str() {
        "anthropic" => "ANTHROPIC_API_KEY".into(),
        "openai" => "OPENAI_API_KEY".into(),
        "google" | "gemini" => "GEMINI_API_KEY".into(),
        "kimi" | "moonshot" => "MOONSHOT_API_KEY".into(),
        "dashscope" | "bailian" | "qwen" => "DASHSCOPE_API_KEY".into(),
        "zhipu" | "glm" | "bigmodel" => "ZHIPU_API_KEY".into(),
        "volcengine" | "ark" | "doubao" => "ARK_API_KEY".into(),
        "siliconflow" => "SILICONFLOW_API_KEY".into(),
        "minimax" => "MINIMAX_API_KEY".into(),
        "azure_openai" => "AZURE_OPENAI_API_KEY".into(),
        "openrouter" => "OPENROUTER_API_KEY".into(),
        "deepseek" => "DEEPSEEK_API_KEY".into(),
        "mistral" => "MISTRAL_API_KEY".into(),
        "groq" => "GROQ_API_KEY".into(),
        "cohere" => "COHERE_API_KEY".into(),
        other => format!("{}_API_KEY", other.to_uppercase().replace('-', "_")),
    }
}

pub struct AgentSecretStore {
    inner: Box<dyn SecretStore>,
}

impl AgentSecretStore {
    pub fn open() -> Result<Self> {
        Ok(Self {
            inner: default_store().map_err(AgentError::Other)?,
        })
    }

    pub fn get(&self, profile_id: &str, env_var: &str) -> Result<Option<String>> {
        let key = SecretKey::new(profile_id, env_var);
        match self.inner.get(&key) {
            Ok(s) => Ok(Some(s)),
            Err(e) => {
                // Distinguish "not found" from a real backend error so the
                // caller can treat "no key set" as a normal state.
                let s = e.to_string();
                if s.contains("not found") {
                    Ok(None)
                } else {
                    Err(AgentError::Other(e))
                }
            }
        }
    }

    pub fn set(&self, profile_id: &str, env_var: &str, value: &str) -> Result<()> {
        let key = SecretKey::new(profile_id, env_var);
        self.inner.set(&key, value).map_err(AgentError::Other)
    }

    pub fn delete(&self, profile_id: &str, env_var: &str) -> Result<()> {
        let key = SecretKey::new(profile_id, env_var);
        self.inner.delete(&key).map_err(AgentError::Other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_mapping() {
        assert_eq!(env_var_for_namespace("anthropic"), "ANTHROPIC_API_KEY");
        assert_eq!(env_var_for_namespace("openai"), "OPENAI_API_KEY");
        assert_eq!(env_var_for_namespace("google"), "GEMINI_API_KEY");
        assert_eq!(env_var_for_namespace("moonshot"), "MOONSHOT_API_KEY");
        assert_eq!(
            env_var_for_namespace("custom-vendor"),
            "CUSTOM_VENDOR_API_KEY"
        );
    }
}
