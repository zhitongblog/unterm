//! Build the launch command + environment for spawning an agent in a pane.
//!
//! Output is a [`LaunchPlan`]: exec + argv + env. The GUI's `spawn.rs`
//! turns this into a real `portable_pty::CommandBuilder`. The CLI's
//! `unterm agent launch <id>` exec's it directly (replacing itself), so
//! Unterm doesn't sit around as a parent process.

use crate::errors::Result;
use crate::manifest::{AgentManifest, EnvBinding, FlagSpec};
use crate::registry::SettingsState;
use crate::secrets::AgentSecretStore;
use crate::template::{expand, expand_args, TemplateCtx};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct LaunchPlan {
    pub exec: String,
    pub args: Vec<String>,
    pub env_set: BTreeMap<String, String>,
    /// env var names to *unset* before launch (e.g. clear inherited
    /// `AWS_PROFILE` if profile isolation is on).
    pub env_unset: Vec<String>,
    pub cwd: Option<String>,
}

/// Connection details for the running Unterm instance's MCP control server,
/// plus the path to the unterm-cli binary that fronts it via the stdio
/// bridge. Supplied by the caller (GUI spawn / CLI launch) so the launcher
/// can auto-wire an agent at `unterm-cli mcp-stdio`. When `None`, MCP
/// auto-registration is skipped entirely.
#[derive(Debug, Clone)]
pub struct McpWireInfo {
    pub host: String,
    pub port: u16,
    pub token: String,
    pub unterm_cli_path: String,
}

pub struct LaunchInputs<'a> {
    pub manifest: &'a AgentManifest,
    pub profile_id: &'a str,
    pub settings: &'a SettingsState,
    pub cwd: Option<&'a str>,
    pub project_root: Option<&'a str>,
    /// Present when the caller wants launched agents auto-wired to Unterm's
    /// MCP. The launcher writes the agent's native MCP config (per the
    /// manifest's `mcp.auto_register_unterm` descriptor) pointing at the
    /// `unterm-cli mcp-stdio` bridge.
    pub mcp: Option<&'a McpWireInfo>,
}

/// Turn the user's selected launch flags into argv tokens, in catalog order
/// (deterministic). `selected` maps flag_id -> `true` (toggle on) or a string
/// (the value for value/choice flags). The arg template is split into tokens
/// first so a `{value}` containing spaces stays a single argv entry; `{{VAR}}`
/// template vars (e.g. `{{CWD}}`) are then expanded per token.
fn compose_flag_args(
    catalog: &[FlagSpec],
    selected: &BTreeMap<String, Value>,
    ctx: &TemplateCtx,
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for spec in catalog {
        let Some(val) = selected.get(&spec.id) else {
            continue;
        };
        let value_str: Option<String> = match val {
            Value::Bool(true) => None,
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            // false toggle, empty string, or unexpected type → not applied.
            _ => continue,
        };
        for token in spec.arg.split_whitespace() {
            // A lone `{value}` token with no value (a misconfigured toggle)
            // would otherwise emit a literal "{value}" arg — drop it.
            if value_str.is_none() && token.contains("{value}") {
                continue;
            }
            let substituted = match &value_str {
                Some(v) => token.replace("{value}", v),
                None => token.to_string(),
            };
            out.push(expand(&substituted, ctx)?);
        }
    }
    Ok(out)
}

pub fn build_launch_plan(inputs: &LaunchInputs<'_>) -> Result<LaunchPlan> {
    let mut ctx = TemplateCtx {
        profile_id: inputs.profile_id.to_string(),
        agent_id: inputs.manifest.id.clone(),
        cwd: inputs.cwd.map(|s| s.to_string()),
        unterm_cli: inputs.mcp.map(|m| m.unterm_cli_path.clone()),
        mcp_host: inputs.mcp.map(|m| m.host.clone()),
        mcp_port: inputs.mcp.map(|m| m.port),
        mcp_token: inputs.mcp.map(|m| m.token.clone()),
    };

    let mut exec = expand(&inputs.manifest.launch.exec, &ctx)?;
    let mut args = expand_args(&inputs.manifest.launch.args, &ctx)?;
    if inputs.cwd.is_some() && !inputs.manifest.launch.args_when_cwd_set.is_empty() {
        args.extend(expand_args(
            &inputs.manifest.launch.args_when_cwd_set,
            &ctx,
        )?);
    }
    // Append the user's selected launch flags (from the Web Settings picker or
    // an in-terminal selection), composed from the manifest's flag_catalog.
    args.extend(compose_flag_args(
        &inputs.manifest.launch.flag_catalog,
        &inputs.settings.launch_flags(),
        &ctx,
    )?);

    let mut env_set: BTreeMap<String, String> = BTreeMap::new();
    // If the agent binary lives in a fallback dir (e.g. ~/.local/bin, ~/.npm-global/bin,
    // %LOCALAPPDATA%\agy\bin) that isn't on the caller's inherited $PATH yet, resolve
    // `exec` to its absolute path and prepend its parent directory to PATH.
    if let Some(resolved) = crate::installer::which(&exec) {
        if let Some(bin_dir) = std::path::Path::new(&resolved).parent() {
            let current_path = std::env::var_os("PATH").unwrap_or_default();
            let in_inherited_path = std::env::split_paths(&current_path).any(|d| d == bin_dir);
            if !in_inherited_path {
                exec = resolved.clone();
                let mut dirs = vec![bin_dir.to_path_buf()];
                dirs.extend(std::env::split_paths(&crate::installer::enriched_path()));
                if let Ok(joined) = std::env::join_paths(dirs) {
                    env_set.insert("PATH".into(), joined.to_string_lossy().to_string());
                }
            }
        }
    }
    env_set.insert("UNTERM_AGENT_ID".into(), inputs.manifest.id.clone());
    env_set.insert(
        "UNTERM_AGENT_MANIFEST_VERSION".into(),
        inputs.manifest.version.to_string(),
    );
    env_set.insert("UNTERM_PROFILE".into(), inputs.profile_id.into());

    // Determine current auth mode for this (profile, agent). Settings store
    // it under the synthetic key "_auth_mode"; falls back to the manifest's
    // first declared mode, or "subscription" if no modes are declared (which
    // would be a malformed v0.18+ manifest but we don't want to panic on it).
    let current_mode = current_auth_mode(inputs.manifest, inputs.settings);
    env_set.insert("UNTERM_AGENT_AUTH_MODE".into(), current_mode.clone());

    if let Some(storage) = &inputs.manifest.settings_storage {
        let store = AgentSecretStore::open().ok();
        for (env_name, binding) in &storage.env_at_launch {
            // Critical: only inject env vars whose binding lists the current
            // auth_mode (or has no filter). This is what stops Claude Code's
            // ANTHROPIC_API_KEY from leaking into a subscription-mode session
            // and silently flipping the user off their Pro plan.
            if !binding.applies_to_mode(&current_mode) {
                continue;
            }
            let resolved =
                resolve_env_binding(binding, inputs.profile_id, inputs.settings, store.as_ref())?;
            if let Some(v) = resolved {
                env_set.insert(env_name.clone(), v);
            }
        }
    }

    // Profile isolation: if isolate_env, we don't list every env to unset,
    // we just communicate to the spawn layer that the *inherited* environment
    // should be filtered through `inherit_env`. The caller (GUI spawn or
    // CLI exec) does the actual filtering.
    let mut env_unset = Vec::new();
    if inputs.manifest.profile_defaults.isolate_env {
        env_unset.push("__UNTERM_ISOLATE__".into()); // sentinel
    }

    // Also surface the MCP connection as env vars so user scripts inside the
    // agent's pane can reach Unterm even for agents that don't speak MCP.
    if let Some(mcp) = inputs.mcp {
        env_set.insert("UNTERM_MCP_HOST".into(), mcp.host.clone());
        env_set.insert("UNTERM_MCP_PORT".into(), mcp.port.to_string());
        env_set.insert("UNTERM_MCP_TOKEN".into(), mcp.token.clone());
        env_set.insert("UNTERM_CLI".into(), mcp.unterm_cli_path.clone());
    }

    let cwd = resolve_cwd(&inputs.manifest.profile_defaults.starting_cwd, inputs);

    // Auto-wire the agent at `unterm-cli mcp-stdio` by writing its native MCP
    // config — but only if the caller supplied MCP info AND the manifest opts
    // in via `mcp.auto_register_unterm`. Write failures are non-fatal: the
    // agent still launches, just without the Unterm toolset pre-registered.
    if let (Some(mcp), Some(reg)) = (
        inputs.mcp,
        inputs
            .manifest
            .mcp
            .as_ref()
            .and_then(|m| m.auto_register_unterm.as_ref()),
    ) {
        // Point {{CWD}} at the *resolved* cwd (which falls back to home) so a
        // `{{CWD}}/.mcp.json` template can never expand to a bare `/.mcp.json`
        // when the agent is launched without an explicit cwd.
        if ctx.cwd.is_none() {
            ctx.cwd = cwd.clone();
        }
        if let Err(e) = write_mcp_config(reg, &ctx, mcp, cwd.as_deref()) {
            log::warn!(
                "agent {}: MCP auto-register skipped: {}",
                inputs.manifest.id,
                e
            );
        }
    }

    Ok(LaunchPlan {
        exec,
        args,
        env_set,
        env_unset,
        cwd,
    })
}

/// Write (read-merge-write) the agent's native MCP config so it spawns
/// `unterm-cli mcp-stdio` as a stdio MCP server named `reg.server_name`.
///
/// We never blind-overwrite: an existing config is parsed, our single server
/// entry is set/replaced, and the rest of the user's file is preserved. The
/// `format` selects both the on-disk syntax (JSON / TOML) and the schema
/// shape each agent expects.
fn write_mcp_config(
    reg: &crate::manifest::McpAutoRegister,
    ctx: &TemplateCtx,
    mcp: &McpWireInfo,
    cwd: Option<&str>,
) -> Result<()> {
    use crate::errors::AgentError;
    use std::path::PathBuf;

    // config_path may be relative (e.g. ".mcp.json"); anchor it at cwd.
    let raw = expand(&reg.config_path, ctx)?;
    let path = {
        let p = PathBuf::from(&raw);
        if p.is_absolute() {
            p
        } else if let Some(c) = cwd {
            PathBuf::from(c).join(p)
        } else {
            p
        }
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let cli = if mcp.unterm_cli_path.is_empty() {
        "unterm-cli".to_string()
    } else {
        mcp.unterm_cli_path.clone()
    };
    let server = reg.server_name.clone();

    match reg.format.as_str() {
        // Claude Code (.mcp.json) and Gemini CLI (~/.gemini/settings.json)
        // both use a top-level `mcpServers` map. Claude requires an explicit
        // `"type": "stdio"`; Gemini infers stdio from the presence of
        // `command`, but tolerates the field, so we emit it for both.
        "claude_code_v1" | "generic_mcp_json" => {
            let mut root = read_json(&path);
            let servers = root
                .as_object_mut()
                .and_then(|o| {
                    o.entry("mcpServers")
                        .or_insert_with(|| serde_json::json!({}))
                        .as_object_mut()
                })
                .ok_or_else(|| AgentError::ParseFailed("mcpServers not an object".into()))?;
            servers.insert(
                server,
                serde_json::json!({
                    "type": "stdio",
                    "command": cli,
                    "args": ["mcp-stdio"],
                }),
            );
            let text = serde_json::to_string_pretty(&root)
                .map_err(|e| AgentError::ParseFailed(format!("serialize mcp json: {e}")))?;
            std::fs::write(&path, text)
                .map_err(|e| AgentError::ParseFailed(format!("write {}: {e}", path.display())))?;
        }
        // OpenCode (opencode.json) uses a *different* shape: the top-level key
        // is `mcp` (not `mcpServers`), each server has `type: "local"`,
        // `enabled: true`, and a single `command` ARRAY that fuses the binary
        // and its args — not a command string + separate args list.
        "opencode_v1" => {
            let mut root = read_json(&path);
            let servers = root
                .as_object_mut()
                .and_then(|o| {
                    o.entry("mcp")
                        .or_insert_with(|| serde_json::json!({}))
                        .as_object_mut()
                })
                .ok_or_else(|| AgentError::ParseFailed("opencode `mcp` not an object".into()))?;
            servers.insert(
                server,
                serde_json::json!({
                    "type": "local",
                    "enabled": true,
                    "command": [cli, "mcp-stdio"],
                }),
            );
            let text = serde_json::to_string_pretty(&root)
                .map_err(|e| AgentError::ParseFailed(format!("serialize opencode json: {e}")))?;
            std::fs::write(&path, text)
                .map_err(|e| AgentError::ParseFailed(format!("write {}: {e}", path.display())))?;
        }
        // Codex CLI (~/.codex/config.toml) — [mcp_servers.<name>] table.
        "codex_v1" => {
            let mut doc = read_toml(&path);
            let tbl = doc
                .as_table_mut()
                .ok_or_else(|| AgentError::ParseFailed("codex config not a table".into()))?;
            let servers = tbl
                .entry("mcp_servers")
                .or_insert_with(|| toml::Value::Table(Default::default()))
                .as_table_mut()
                .ok_or_else(|| AgentError::ParseFailed("mcp_servers not a table".into()))?;
            let mut entry = toml::value::Table::new();
            entry.insert("command".into(), toml::Value::String(cli));
            entry.insert(
                "args".into(),
                toml::Value::Array(vec![toml::Value::String("mcp-stdio".into())]),
            );
            servers.insert(server, toml::Value::Table(entry));
            let text = toml::to_string_pretty(&doc)
                .map_err(|e| AgentError::ParseFailed(format!("serialize codex toml: {e}")))?;
            std::fs::write(&path, text)
                .map_err(|e| AgentError::ParseFailed(format!("write {}: {e}", path.display())))?;
        }
        other => {
            return Err(AgentError::ParseFailed(format!(
                "unknown mcp config format {other:?}"
            )));
        }
    }
    Ok(())
}

/// Read a JSON file into a Value, returning an empty object if it's missing
/// or unparseable (we don't want a corrupt user file to abort the launch —
/// but we also won't clobber a parseable one; the caller merges into it).
fn read_json(path: &std::path::Path) -> serde_json::Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

fn read_toml(path: &std::path::Path) -> toml::Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.parse::<toml::Value>().ok())
        .unwrap_or_else(|| toml::Value::Table(Default::default()))
}

fn resolve_env_binding(
    binding: &EnvBinding,
    profile_id: &str,
    settings: &SettingsState,
    secret_store: Option<&AgentSecretStore>,
) -> Result<Option<String>> {
    match binding {
        EnvBinding::Literal { literal, .. } => Ok(Some(literal.clone())),
        EnvBinding::Setting {
            from_setting,
            skip_if_empty,
            ..
        } => {
            let val = settings.values.get(from_setting);
            match val {
                Some(serde_json::Value::String(s)) => {
                    if s.is_empty() && *skip_if_empty {
                        Ok(None)
                    } else {
                        Ok(Some(s.clone()))
                    }
                }
                Some(other) => Ok(Some(other.to_string())),
                None => Ok(None),
            }
        }
        EnvBinding::Secret { from, .. } => {
            let Some(ns) = from.strip_prefix("secret:") else {
                return Ok(None);
            };
            let env_var = crate::secrets::env_var_for_namespace(ns);
            let Some(store) = secret_store else {
                return Ok(None);
            };
            store.get(profile_id, &env_var)
        }
    }
}

/// Selected auth mode for the (profile, agent). Reads
/// `settings.values["_auth_mode"]`; falls back to the first `recommended`
/// mode in the manifest, then the first declared mode, then "subscription"
/// as a last-resort sentinel.
pub fn current_auth_mode(
    manifest: &crate::manifest::AgentManifest,
    settings: &SettingsState,
) -> String {
    if let Some(serde_json::Value::String(s)) = settings.values.get("_auth_mode") {
        if !s.is_empty() {
            // Only honour if the mode still exists in the current manifest;
            // otherwise the agent's manifest was updated and our stored
            // mode no longer exists — fall through to the default.
            if manifest.auth_modes.iter().any(|m| m.id == *s) {
                return s.clone();
            }
        }
    }
    if let Some(recommended) = manifest.auth_modes.iter().find(|m| m.recommended) {
        return recommended.id.clone();
    }
    if let Some(first) = manifest.auth_modes.first() {
        return first.id.clone();
    }
    "subscription".into()
}

fn resolve_cwd(setting: &Option<String>, inputs: &LaunchInputs<'_>) -> Option<String> {
    let s = setting.as_deref().unwrap_or("project_root_or_home");
    match s {
        "current" => inputs.cwd.map(|s| s.to_string()),
        "project_root" => inputs.project_root.map(|s| s.to_string()),
        "home" => dirs_next::home_dir().map(|p| p.to_string_lossy().to_string()),
        "project_root_or_home" => inputs
            .project_root
            .map(|s| s.to_string())
            .or_else(|| dirs_next::home_dir().map(|p| p.to_string_lossy().to_string())),
        _ => inputs.cwd.map(|s| s.to_string()),
    }
}

#[cfg(test)]
mod mcp_config_tests {
    use super::*;
    use crate::manifest::McpAutoRegister;
    use crate::template::TemplateCtx;

    fn reg(format: &str, config_path: &str) -> McpAutoRegister {
        McpAutoRegister {
            config_path: config_path.into(),
            format: format.into(),
            server_name: "unterm".into(),
            transport: "stdio".into(),
            scopes_default: vec!["read".into()],
            scopes_optional: vec!["write".into()],
        }
    }

    fn wire(cli: &str) -> McpWireInfo {
        McpWireInfo {
            host: "127.0.0.1".into(),
            port: 19876,
            token: "tok".into(),
            unterm_cli_path: cli.into(),
        }
    }

    fn ctx() -> TemplateCtx {
        TemplateCtx {
            unterm_cli: Some("/opt/unterm-cli".into()),
            mcp_host: Some("127.0.0.1".into()),
            mcp_port: Some(19876),
            mcp_token: Some("tok".into()),
            ..Default::default()
        }
    }

    fn fspec(id: &str, arg: &str, kind: crate::manifest::FlagKind) -> FlagSpec {
        FlagSpec {
            id: id.into(),
            arg: arg.into(),
            label_i18n: Default::default(),
            description_i18n: Default::default(),
            kind,
            choices: vec![],
            risk: crate::manifest::FlagRisk::Safe,
            default_on: false,
        }
    }

    fn auth_mode(id: &str, recommended: bool) -> crate::manifest::AuthModeSpec {
        crate::manifest::AuthModeSpec {
            id: id.into(),
            recommended,
            label_i18n: Default::default(),
            description_i18n: Default::default(),
            oauth_trigger: None,
            oauth_ready_marker: None,
            oauth_timeout_s: 180,
            console_url: None,
            reveals_settings: vec![],
        }
    }

    fn manifest_with_modes(auth_modes: Vec<crate::manifest::AuthModeSpec>) -> AgentManifest {
        AgentManifest {
            id: "claude-code".into(),
            version: 1,
            name: "Claude Code".into(),
            vendor: "Anthropic".into(),
            category: "first-party".into(),
            popularity_rank: 0,
            icon_url: None,
            homepage: None,
            tagline_i18n: Default::default(),
            description_i18n: Default::default(),
            license: None,
            platforms: vec!["macos".into()],
            detect: crate::manifest::DetectSpec {
                command: "claude".into(),
                version_args: vec!["--version".into()],
                version_regex: None,
                min_version: None,
            },
            install: crate::manifest::InstallSpec::default(),
            auth: crate::manifest::AuthSpec {
                primary: crate::manifest::AuthMethod::OauthBrowser {
                    trigger: crate::manifest::ShellCmd {
                        cmd: vec!["claude".into(), "login".into()],
                    },
                    ready_marker: None,
                    timeout_s: 180,
                },
                fallback: None,
            },
            launch: crate::manifest::LaunchSpec {
                exec: "claude".into(),
                args: vec![],
                args_when_cwd_set: vec![],
                respects_unterm_split: false,
                flag_catalog: vec![],
            },
            mcp: None,
            profile_defaults: crate::manifest::ProfileDefaults::default(),
            auth_modes,
            settings_schema: vec![],
            settings_storage: None,
            telemetry_notice: None,
        }
    }

    #[test]
    fn compose_flag_args_toggle_value_and_unselected() {
        use crate::manifest::FlagKind;
        let cat = vec![
            fspec("skip-trust", "--skip-trust", FlagKind::Toggle),
            fspec("model", "--model {value}", FlagKind::Value),
            fspec("add-dir", "--add-dir {value}", FlagKind::Value),
        ];
        let mut sel = BTreeMap::new();
        sel.insert("skip-trust".into(), Value::Bool(true));
        // A value containing a space must stay a single argv token.
        sel.insert("model".into(), Value::String("gpt-5 pro".into()));
        // "add-dir" not selected → omitted; catalog order is preserved.
        let args = compose_flag_args(&cat, &sel, &TemplateCtx::default()).unwrap();
        assert_eq!(args, vec!["--skip-trust", "--model", "gpt-5 pro"]);
    }

    #[test]
    fn compose_flag_args_skips_false_toggle() {
        use crate::manifest::FlagKind;
        let cat = vec![fspec("yolo", "--yolo", FlagKind::Toggle)];
        let mut sel = BTreeMap::new();
        sel.insert("yolo".into(), Value::Bool(false));
        let args = compose_flag_args(&cat, &sel, &TemplateCtx::default()).unwrap();
        assert!(args.is_empty(), "false toggle must not emit args");
    }

    #[test]
    fn current_auth_mode_prefers_recommended_subscription_mode() {
        let manifest = manifest_with_modes(vec![
            auth_mode("subscription", true),
            auth_mode("byo_key", false),
        ]);
        let settings = SettingsState::default();
        assert_eq!(current_auth_mode(&manifest, &settings), "subscription");
    }

    #[test]
    fn current_auth_mode_keeps_existing_mode_when_valid() {
        let manifest = manifest_with_modes(vec![
            auth_mode("subscription", true),
            auth_mode("byo_key", false),
        ]);
        let mut settings = SettingsState::default();
        settings
            .values
            .insert("_auth_mode".into(), Value::String("byo_key".into()));
        assert_eq!(current_auth_mode(&manifest, &settings), "byo_key");
    }

    #[test]
    fn current_auth_mode_falls_back_to_recommended_when_stale() {
        let manifest = manifest_with_modes(vec![
            auth_mode("subscription", true),
            auth_mode("byo_key", false),
        ]);
        let mut settings = SettingsState::default();
        settings
            .values
            .insert("_auth_mode".into(), Value::String("legacy".into()));
        assert_eq!(current_auth_mode(&manifest, &settings), "subscription");
    }

    // Claude Code: project-scope .mcp.json with mcpServers + type:stdio.
    #[test]
    fn claude_code_mcp_json_shape() {
        let dir = std::env::temp_dir().join(format!("uw-claude-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cwd = dir.to_string_lossy().to_string();
        let mut c = ctx();
        c.cwd = Some(cwd.clone());
        write_mcp_config(
            &reg("claude_code_v1", "{{CWD}}/.mcp.json"),
            &c,
            &wire("/opt/unterm-cli"),
            Some(&cwd),
        )
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(".mcp.json")).unwrap()).unwrap();
        let s = &v["mcpServers"]["unterm"];
        assert_eq!(s["type"], "stdio");
        assert_eq!(s["command"], "/opt/unterm-cli");
        assert_eq!(s["args"][0], "mcp-stdio");
        std::fs::remove_dir_all(&dir).ok();
    }

    // OpenCode: top-level `mcp` (not mcpServers), command is a fused ARRAY,
    // type:"local", enabled:true. This is the conflict the web check caught.
    #[test]
    fn opencode_mcp_shape() {
        let dir = std::env::temp_dir().join(format!("uw-oc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("opencode.json");
        write_mcp_config(
            &reg("opencode_v1", &path.to_string_lossy()),
            &ctx(),
            &wire("/opt/unterm-cli"),
            None,
        )
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            v.get("mcpServers").is_none(),
            "opencode must not use mcpServers"
        );
        let s = &v["mcp"]["unterm"];
        assert_eq!(s["type"], "local");
        assert_eq!(s["enabled"], true);
        assert_eq!(s["command"][0], "/opt/unterm-cli");
        assert_eq!(s["command"][1], "mcp-stdio");
        std::fs::remove_dir_all(&dir).ok();
    }

    // Codex: TOML [mcp_servers.unterm] with command + args.
    #[test]
    fn codex_toml_shape() {
        let dir = std::env::temp_dir().join(format!("uw-codex-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        write_mcp_config(
            &reg("codex_v1", &path.to_string_lossy()),
            &ctx(),
            &wire("/opt/unterm-cli"),
            None,
        )
        .unwrap();
        let doc: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        let s = &doc["mcp_servers"]["unterm"];
        assert_eq!(s["command"].as_str(), Some("/opt/unterm-cli"));
        assert_eq!(s["args"][0].as_str(), Some("mcp-stdio"));
        std::fs::remove_dir_all(&dir).ok();
    }

    // Merge, don't clobber: an existing unrelated server survives our write.
    #[test]
    fn merge_preserves_existing_servers() {
        let dir = std::env::temp_dir().join(format!("uw-merge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"other":{"command":"x","args":[]}},"extra":42}"#,
        )
        .unwrap();
        write_mcp_config(
            &reg("claude_code_v1", &path.to_string_lossy()),
            &ctx(),
            &wire("/opt/unterm-cli"),
            None,
        )
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            v["mcpServers"]["other"]["command"], "x",
            "existing server clobbered"
        );
        assert_eq!(
            v["mcpServers"]["unterm"]["type"], "stdio",
            "our server missing"
        );
        assert_eq!(v["extra"], 42, "unrelated key dropped");
        std::fs::remove_dir_all(&dir).ok();
    }
}
