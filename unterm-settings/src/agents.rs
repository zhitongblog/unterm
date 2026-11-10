//! Web Settings panel REST handlers for the AI-agent runtime.
//!
//! These functions back the "AI Agents" tab in the settings UI. They are
//! thin wrappers over the [`unterm_agents`] crate; the CLI's
//! `unterm-cli agent ...` subcommands talk to the same code paths, so the
//! GUI and CLI surfaces stay in lockstep.
//!
//! Sensitive details ride three rules:
//!
//! 1. Secret values (API keys) NEVER come back in any GET response. Boolean
//!    "is set" sentinels only. The frontend can't accidentally exfiltrate
//!    a token even if the SPA is XSS'd.
//! 2. Install / launch operations require the same bearer-token auth as
//!    every other `/api/*` route; no separate trust gate.
//! 3. Errors surface the unterm-agents AgentError variant name so the
//!    frontend can do meaningful UX ("install failed because node missing"
//!    vs "envelope signature bad").

use super::server::Response;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use unterm_agents::{
    fetch_manifests, installer,
    manifest::SettingKind,
    registry::{apply_updates, snapshot_existing, SettingsState},
    AgentManifest,
};

pub fn api_list(query: &str) -> Response {
    let only_installed = query
        .split('&')
        .any(|p| p == "installed=true" || p == "installed=1");

    let set = match fetch_manifests() {
        Ok(s) => s,
        Err(e) => {
            return Response::err(503, "Service Unavailable", &format!("manifest fetch: {e}"))
        }
    };

    let rows: Vec<Value> = set
        .for_current_platform()
        .into_iter()
        .map(|m| {
            let detect = installer::detect(&m.detect);
            json!({
                "id": m.id,
                "name": m.name,
                "vendor": m.vendor,
                "manifest_version": m.version,
                "popularity_rank": m.popularity_rank,
                "icon_url": m.icon_url,
                "homepage": m.homepage,
                "tagline_i18n": m.tagline_i18n,
                "category": m.category,
                "installed": detect.ok,
                "detected_version": detect.version,
                "binary_path": detect.binary_path,
                "mcp_supported": m.mcp.as_ref().map(|x| x.client_supports_mcp).unwrap_or(false),
                "headless_supported": supports_headless(&m.id),
                "headless_default_prompt": headless_default_prompt_value(&m.id),
            })
        })
        .filter(|row| !only_installed || row["installed"].as_bool().unwrap_or(false))
        .collect();

    Response::ok_json(json!({
        "envelope_source": format!("{:?}", set.source),
        "envelope_issued_at": set.envelope.issued_at,
        "envelope_expires_at": set.envelope.expires_at,
        "signing_key_id": set.envelope.signature.key_id,
        "agents": rows,
    }))
}

pub fn api_show(id: &str) -> Response {
    let manifest = match resolve(id) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let detect = installer::detect(&manifest.detect);
    Response::ok_json(json!({
        "manifest": manifest,
        "detect": {
            "ok": detect.ok,
            "version": detect.version,
            "binary_path": detect.binary_path,
        },
        "headless_supported": supports_headless(&manifest.id),
        "headless_default_prompt": headless_default_prompt_value(&manifest.id),
    }))
}

pub fn api_manifest_info() -> Response {
    match fetch_manifests() {
        Ok(s) => Response::ok_json(json!({
            "source":  format!("{:?}", s.source),
            "issued_at":  s.envelope.issued_at,
            "expires_at": s.envelope.expires_at,
            "min_unterm_version": s.envelope.min_unterm_version,
            "signing_key_id": s.envelope.signature.key_id,
            "agent_count": s.envelope.manifests.len(),
        })),
        Err(e) => Response::err(503, "Service Unavailable", &e.to_string()),
    }
}

pub fn api_manifest_refresh() -> Response {
    if let Ok(etag) = unterm_agents::paths::manifest_etag_path() {
        let _ = std::fs::remove_file(etag);
    }
    if let Ok(cache) = unterm_agents::paths::manifest_cache_path() {
        let _ = std::fs::remove_file(cache);
    }
    api_manifest_info()
}

pub fn api_install(id: &str) -> Response {
    let manifest = match resolve(id) {
        Ok(m) => m,
        Err(r) => return r,
    };
    match installer::run_install(&manifest) {
        Ok(reports) => Response::ok_json(json!({
            "id": manifest.id,
            "ok": true,
            "steps": reports.iter().map(|r| json!({
                "label": r.label,
                "success": r.success,
                "exit_code": r.exit_code,
                "stdout_tail": r.stdout_tail,
                "stderr_tail": r.stderr_tail,
            })).collect::<Vec<_>>(),
        })),
        Err(e) => Response::err(500, "Install Failed", &e.to_string()),
    }
}

pub fn api_uninstall(id: &str) -> Response {
    let manifest = match resolve(id) {
        Ok(m) => m,
        Err(r) => return r,
    };
    match installer::run_uninstall(&manifest) {
        Ok(r) => Response::ok_json(json!({
            "id": manifest.id,
            "ok": r.success,
            "label": r.label,
            "exit_code": r.exit_code,
        })),
        Err(e) => Response::err(500, "Uninstall Failed", &e.to_string()),
    }
}

pub fn api_update_agent(id: &str) -> Response {
    let manifest = match resolve(id) {
        Ok(m) => m,
        Err(r) => return r,
    };
    match installer::run_update(&manifest) {
        Ok(r) => Response::ok_json(json!({
            "id": manifest.id,
            "ok": r.success,
            "exit_code": r.exit_code,
        })),
        Err(e) => Response::err(500, "Update Failed", &e.to_string()),
    }
}

pub fn api_auth(id: &str, body: &[u8]) -> Response {
    let manifest = match resolve(id) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return Response::err(400, "Bad Request", &format!("body must be JSON: {e}")),
    };
    let profile_id = parsed
        .get("profile")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .to_string();
    let api_key = parsed
        .get("api_key")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let Some(key) = api_key else {
        // OAuth browser flow can't run inside the Web Settings server (needs
        // a controlling tty for the vendor command). Tell the SPA to run it
        // from a real terminal via the CLI shim.
        return Response::ok_json(json!({
            "method": "oauth_browser_required",
            "hint": format!("Run from a terminal: unterm agent auth {} --profile {profile_id}", manifest.id),
        }));
    };

    match unterm_agents::authn::run_api_key(&manifest.auth, &profile_id, &key) {
        Ok(o) => Response::ok_json(json!({"method": o.method_used, "profile": profile_id})),
        Err(e) => Response::err(500, "Auth Failed", &e.to_string()),
    }
}

pub fn api_settings_get(id: &str, query: &str) -> Response {
    let manifest = match resolve(id) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let profile_id = query_param(query, "profile").unwrap_or_else(|| "default".to_string());
    let mut state = match SettingsState::load(&profile_id, &manifest.id) {
        Ok(s) => s,
        Err(e) => return Response::err(500, "Load Failed", &e.to_string()),
    };
    state.merge_defaults(&manifest.settings_schema);

    let mut display = serde_json::Map::new();
    for (k, v) in &state.values {
        let redacted = if manifest
            .settings_schema
            .iter()
            .any(|s| s.key == *k && matches!(s.kind, SettingKind::Secret))
        {
            json!({"_secret": true, "is_set": v.is_string() && !v.as_str().unwrap_or("").is_empty()})
        } else {
            v.clone()
        };
        display.insert(k.clone(), redacted);
    }

    // Re-run detect so the detail card shows the right install badge.
    // /api/agents/list already pays for this on the way in, but the
    // SPA opens a detail view potentially minutes later and we don't
    // want to render "not installed" while the user can see the binary
    // perfectly fine via `which` in another tab.
    let detect = installer::detect(&manifest.detect);

    Response::ok_json(json!({
        "agent": manifest.id,
        "profile": profile_id,
        "manifest_version": state.manifest_version,
        // Embed the full manifest so the SPA can render storage paths,
        // category metadata, and MCP info without a second round-trip
        // to /api/agents/<id> (which was silently 404'ing because of an
        // off-by-one slash check until 2026-05-20).
        "manifest": &manifest,
        "detect": {
            "ok": detect.ok,
            "version": detect.version,
            "binary_path": detect.binary_path,
        },
        "headless_supported": supports_headless(&manifest.id),
        "headless_default_prompt": headless_default_prompt_value(&manifest.id),
        "schema": manifest.settings_schema,
        "values": Value::Object(display),
    }))
}

pub fn api_settings_put(id: &str, body: &[u8]) -> Response {
    let manifest = match resolve(id) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return Response::err(400, "Bad Request", &format!("JSON parse: {e}")),
    };
    let profile_id = parsed
        .get("profile")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .to_string();
    let updates_value = parsed.get("values").cloned().unwrap_or(json!({}));
    let updates_map: BTreeMap<String, Value> = match updates_value {
        Value::Object(m) => m.into_iter().collect(),
        _ => return Response::err(400, "Bad Request", "`values` must be a JSON object"),
    };

    let mut state = match SettingsState::load(&profile_id, &manifest.id) {
        Ok(s) => s,
        Err(e) => return Response::err(500, "Load Failed", &e.to_string()),
    };
    state.merge_defaults(&manifest.settings_schema);

    match apply_updates(&manifest, &profile_id, &mut state, updates_map) {
        Ok(outcome) => Response::ok_json(json!({
            "ok": true,
            "written_files": outcome.written_files,
            "written_secrets": outcome.written_secrets,
            "skipped_unknown": outcome.skipped_unknown,
        })),
        Err(e) => Response::err(500, "Apply Failed", &e.to_string()),
    }
}

pub fn api_import(id: &str, query: &str) -> Response {
    let manifest = match resolve(id) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let profile_id = query_param(query, "profile").unwrap_or_else(|| "default".to_string());
    match snapshot_existing(&manifest, &profile_id) {
        Ok(snap) => {
            // Redact secrets in the response — only emit boolean presence.
            let mut display = serde_json::Map::new();
            for (k, v) in snap {
                let redacted = if manifest
                    .settings_schema
                    .iter()
                    .any(|s| s.key == k && matches!(s.kind, SettingKind::Secret))
                {
                    json!({"_secret": true, "is_set": true})
                } else {
                    v
                };
                display.insert(k, redacted);
            }
            Response::ok_json(json!({"imported": Value::Object(display)}))
        }
        Err(e) => Response::err(500, "Import Failed", &e.to_string()),
    }
}

pub fn api_launch_plan(id: &str, body: &[u8]) -> Response {
    let manifest = match resolve(id) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let parsed: Value = serde_json::from_slice(body).unwrap_or(json!({}));
    let profile_id = parsed
        .get("profile")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .to_string();
    let cwd = parsed
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut state = match SettingsState::load(&profile_id, &manifest.id) {
        Ok(s) => s,
        Err(e) => return Response::err(500, "Load Failed", &e.to_string()),
    };
    state.merge_defaults(&manifest.settings_schema);

    let plan =
        match unterm_agents::launcher::build_launch_plan(&unterm_agents::launcher::LaunchInputs {
            manifest: &manifest,
            profile_id: &profile_id,
            settings: &state,
            cwd: cwd.as_deref(),
            project_root: cwd.as_deref(),
            // Preview only — never write MCP config files as a side effect of
            // rendering the launch plan for the SPA. Real launch (CLI) wires it.
            mcp: None,
        }) {
            Ok(p) => p,
            Err(e) => return Response::err(500, "Plan Failed", &e.to_string()),
        };

    // Redact secret-looking env vars before sending to the SPA.
    let env_set: BTreeMap<String, String> = plan
        .env_set
        .into_iter()
        .map(|(k, v)| {
            let redacted = if k.ends_with("_API_KEY") || k.ends_with("_TOKEN") {
                "***".to_string()
            } else {
                v
            };
            (k, redacted)
        })
        .collect();

    Response::ok_json(json!({
        "exec": plan.exec,
        "args": plan.args,
        "env_set": env_set,
        "cwd": plan.cwd,
    }))
}

pub fn api_run_plan(id: &str, body: &[u8]) -> Response {
    let manifest = match resolve(id) {
        Ok(m) => m,
        Err(r) => return r,
    };
    if !supports_headless(&manifest.id) {
        return Response::err(
            400,
            "Bad Request",
            &format!(
                "agent {} does not expose a headless run adapter yet",
                manifest.id
            ),
        );
    }

    let parsed: Value = serde_json::from_slice(body).unwrap_or(json!({}));
    let profile_id = parsed
        .get("profile")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .to_string();
    let cwd = parsed
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let prompt = parsed
        .get("prompt")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| default_headless_prompt(&manifest.id).to_string());

    let cli_path = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("unterm-cli")))
        .filter(|p| p.exists())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "unterm-cli".to_string());

    let mut argv = vec![
        "agent".to_string(),
        "run".to_string(),
        manifest.id.clone(),
        "--profile".to_string(),
        profile_id.clone(),
    ];
    if let Some(cwd) = &cwd {
        argv.push("--cwd".to_string());
        argv.push(cwd.clone());
    }
    argv.push(prompt.clone());

    let mut preview_parts = Vec::with_capacity(argv.len() + 1);
    preview_parts.push(cli_path.clone());
    preview_parts.extend(argv.iter().cloned());

    Response::ok_json(json!({
        "id": manifest.id,
        "profile": profile_id,
        "cwd": cwd,
        "prompt": prompt,
        "cli_path": cli_path,
        "argv": argv,
        "command": shell_join_for_current_platform(&preview_parts),
    }))
}

fn resolve(id: &str) -> Result<AgentManifest, Response> {
    match fetch_manifests() {
        Ok(s) => match s.find(id) {
            Some(m) => Ok(m.clone()),
            None => Err(Response::err(
                404,
                "Not Found",
                &format!("agent {id:?} not in current manifest"),
            )),
        },
        Err(e) => Err(Response::err(503, "Service Unavailable", &e.to_string())),
    }
}

fn supports_headless(id: &str) -> bool {
    matches!(
        id,
        "agy"
            | "antigravity"
            | "codex-cli"
            | "claude-code"
            | "gemini-cli"
            | "opencode"
            | "kimi-code"
            | "qwen-code"
            | "trae-agent"
    )
}

fn default_headless_prompt(id: &str) -> &'static str {
    match id {
        "agy" | "antigravity" => "summarise this repository and suggest the next useful task",
        "codex-cli" => "review this diff and list risky changes",
        "claude-code" => "summarise the last failing test output",
        "gemini-cli" => "summarise this repository and suggest the next useful task",
        "opencode" => "inspect the current project and suggest the next useful task",
        "kimi-code" => "inspect this project and suggest the next useful task",
        "qwen-code" => "inspect this project and suggest the next useful task",
        "trae-agent" => "inspect this project and suggest the next useful task",
        _ => "summarise the current task",
    }
}

fn headless_default_prompt_value(id: &str) -> Value {
    if supports_headless(id) {
        json!(default_headless_prompt(id))
    } else {
        Value::Null
    }
}

fn shell_join_for_current_platform(parts: &[String]) -> String {
    #[cfg(windows)]
    {
        return parts
            .iter()
            .map(|part| cmd_quote(part))
            .collect::<Vec<_>>()
            .join(" ");
    }
    #[cfg(not(windows))]
    {
        parts
            .iter()
            .map(|part| shell_quote(part))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(windows)]
fn cmd_quote(s: &str) -> String {
    if s.is_empty() {
        return "\"\"".to_string();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '\\' | ':' | '='))
    {
        return s.to_string();
    }

    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut backslashes = 0;
    for ch in s.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                if backslashes > 0 {
                    out.push_str(&"\\".repeat(backslashes));
                    backslashes = 0;
                }
                out.push(ch);
            }
        }
    }
    if backslashes > 0 {
        out.push_str(&"\\".repeat(backslashes * 2));
    }
    out.push('"');
    out
}

fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '='))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn query_param(query: &str, key: &str) -> Option<String> {
    for part in query.split('&') {
        if let Some((k, v)) = part.split_once('=') {
            if k == key {
                return Some(v.to_string());
            }
        }
    }
    None
}
