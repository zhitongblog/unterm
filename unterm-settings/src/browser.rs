//! Which Unzoo Browser profile an agent run here browses in.
//!
//! Unzoo gives every MCP client a profile of its own, named after it: the
//! agents run here present themselves as `unzoo-one-agent`, so theirs is
//! `mcp-unzoo-one-agent`. Profiles count against the person's Unzoo plan
//! (three on the free one), and that is Unzoo's to decide, not ours to work
//! around. So two ways in, and nothing else:
//!
//! - **Reserved**: the agents' own profile, made while there is room for it
//!   ([`ensure_reserved`]) -- the first time the console looks, rather than
//!   the first time an agent needs a browser and finds the plan full.
//! - **Chosen**: a profile the person already has (their own logins, say),
//!   named in the run settings. Unzoo cannot pin a client to a profile, but
//!   every browser tool takes one; the agent is told to name it on every
//!   call and to work only in tabs it opened ([`profile_instructions`]).
//!
//! When neither is possible the console says so and offers both ways out:
//! choose a profile, or upgrade Unzoo.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

/// The client name agents run here present to Unzoo.
pub const CLIENT: &str = "unzoo-one-agent";
/// The profile Unzoo keeps for that client.
pub const RESERVED_PROFILE: &str = "mcp-unzoo-one-agent";

fn endpoint() -> String {
    std::env::var("UNZOO_ENDPOINT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.trim_end_matches('/').to_string())
        .unwrap_or_else(|| "http://127.0.0.1:9399".to_string())
}

fn call(method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
    use http_req::request::{Method, Request};
    use http_req::uri::Uri;
    use std::convert::TryFrom;

    let url = format!("{}{path}", endpoint());
    let uri = Uri::try_from(url.as_str()).with_context(|| format!("not a URL: {url}"))?;
    let bytes = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
    let mut out = Vec::new();
    let mut request = Request::new(&uri);
    request
        .method(if method == "POST" { Method::POST } else { Method::GET })
        .header("Content-Type", "application/json")
        .header("Content-Length", &bytes.len())
        .connect_timeout(Some(Duration::from_secs(2)))
        .read_timeout(Some(Duration::from_secs(10)))
        .timeout(Duration::from_secs(12));
    if body.is_some() {
        request.body(&bytes);
    }
    request.send(&mut out).map_err(|e| anyhow!("Unzoo Browser is not answering: {e}"))?;
    let value: Value = serde_json::from_slice(&out).context("Unzoo Browser answered something that is not JSON")?;
    if value.get("success") == Some(&Value::Bool(false)) {
        let message = value["error"].as_str().unwrap_or("Unzoo Browser refused").to_string();
        bail!("{message}");
    }
    Ok(value)
}

/// One profile, as the console shows it.
fn profiles_of(listing: &Value) -> Vec<Value> {
    listing
        .pointer("/data/profiles")
        .and_then(Value::as_array)
        .map(|profiles| {
            profiles
                .iter()
                .filter_map(|p| {
                    let id = p.get("profile_id").and_then(Value::as_str)?;
                    Some(json!({"id": id, "name": p.get("name").and_then(Value::as_str).unwrap_or(id)}))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Profiles and the plan's limit, from a running Unzoo.
fn snapshot() -> Result<(Vec<Value>, Value)> {
    let profiles = profiles_of(&call("GET", "/api/v1/profiles", None)?);
    let license = call("GET", "/api/v1/license/status", None).unwrap_or(Value::Null);
    let license = license.get("license").or_else(|| license.pointer("/data/license")).cloned().unwrap_or(Value::Null);
    Ok((profiles, license))
}

/// Make the agents' own profile if it is not there and the plan has room.
/// Ok(true) when it exists afterwards.
pub fn ensure_reserved() -> Result<bool> {
    let (profiles, license) = snapshot()?;
    if profiles.iter().any(|p| p["id"] == RESERVED_PROFILE) {
        return Ok(true);
    }
    let room = license
        .pointer("/limits/max_profiles")
        .and_then(Value::as_u64)
        .map_or(true, |max| (profiles.len() as u64) < max);
    if !room {
        return Ok(false);
    }
    call("POST", "/api/v1/profiles/create", Some(&json!({"name": RESERVED_PROFILE})))?;
    let (profiles, _) = snapshot()?;
    Ok(profiles.iter().any(|p| p["id"] == RESERVED_PROFILE))
}

/// What the console shows about the agents' browser: whether Unzoo is here
/// and running, the plan, the profiles, and which one the agents use.
///
/// Looking is also when the reserved profile is made, if there is room and
/// the person has not chosen another.
pub fn status(installed: bool, chosen: &str) -> Value {
    if !installed {
        return json!({"installed": false, "reachable": false});
    }
    let reserve_error = if chosen.is_empty() { ensure_reserved().err().map(|e| format!("{e:#}")) } else { None };
    match snapshot() {
        Err(error) => json!({"installed": true, "reachable": false, "error": format!("{error:#}")}),
        Ok((profiles, license)) => {
            let reserved = profiles.iter().any(|p| p["id"] == RESERVED_PROFILE);
            let chosen_ok = !chosen.is_empty() && profiles.iter().any(|p| p["id"] == chosen);
            let max = license.pointer("/limits/max_profiles").and_then(Value::as_u64);
            json!({
                "installed": true,
                "reachable": true,
                "plan": license.get("tier_name").cloned().unwrap_or(Value::Null),
                "max_profiles": max,
                "profiles": profiles,
                "reserved_profile": RESERVED_PROFILE,
                "reserved": reserved,
                "chosen": if chosen.is_empty() { Value::Null } else { json!(chosen) },
                "chosen_missing": !chosen.is_empty() && !chosen_ok,
                // The agents can browse: in their own profile, or in the
                // one chosen for them.
                "ready": if chosen.is_empty() { reserved } else { chosen_ok },
                "reserve_error": reserve_error,
            })
        }
    }
}

fn tab_number(tab: &str) -> Result<u64> {
    tab.trim().parse().map_err(|_| anyhow!("not a tab id: {tab}"))
}

/// One tab's address and title (from Unzoo's tab list -- this tab only) and
/// a screenshot of it.
pub fn tab_view(tab: &str) -> Result<Value> {
    let id = tab_number(tab)?;
    let listing = call("GET", "/api/v1/tabs", None)?;
    let found = listing
        .pointer("/data/tabs")
        .and_then(Value::as_array)
        .and_then(|tabs| tabs.iter().find(|t| t.get("tab_id").and_then(Value::as_u64) == Some(id)).cloned())
        .ok_or_else(|| anyhow!("the tab is closed"))?;
    let shot = call("POST", "/api/v1/screenshot", Some(&json!({"tab_id": id})))?;
    Ok(json!({
        "tab_id": tab,
        "url": found.get("url").cloned().unwrap_or(Value::Null),
        "title": found.get("title").cloned().unwrap_or(Value::Null),
        "format": shot.pointer("/data/format").cloned().unwrap_or(json!("png")),
        "image_base64": shot.pointer("/data/image_base64").cloned().unwrap_or(Value::Null),
    }))
}

/// Bring a tab to the front of Unzoo Browser.
pub fn activate_tab(tab: &str) -> Result<Value> {
    let id = tab_number(tab)?;
    call("POST", "/api/v1/tabs/activate", Some(&json!({"tab_id": id})))?;
    Ok(json!({"tab_id": tab, "activated": true}))
}

/// What an agent is told when it browses in a profile chosen for it rather
/// than its own. Unzoo routes a call with a profile but no tab to one of
/// that profile's existing tabs -- which may be the one the person is
/// reading -- so the agent opens its own tab first and names it after.
pub fn profile_instructions(profile: &str) -> String {
    format!(
        "Use the Unzoo Browser profile \"{profile}\" for all browsing: it holds the user's sign-ins. \
Pass profile_id=\"{profile}\" on every unzoo tool call. Before anything else, open your own tab with \
tab_create(profile_id=\"{profile}\") and pass the tab_id it returns on every later call. Never navigate, \
type into or close a tab you did not open in this session: the others are the user's."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reserved_profile_is_the_one_unzoo_keeps_for_the_client() {
        assert_eq!(RESERVED_PROFILE, format!("mcp-{CLIENT}"));
    }

    #[test]
    fn profiles_are_read_from_unzoos_listing() {
        let listing = json!({"data": {"profiles": [
            {"profile_id": "default", "name": "work"},
            {"profile_id": "mcp-claude-code"},
            {"name": "no id"},
        ]}, "success": true});
        assert_eq!(
            profiles_of(&listing),
            vec![json!({"id": "default", "name": "work"}), json!({"id": "mcp-claude-code", "name": "mcp-claude-code"})]
        );
    }

    #[test]
    fn a_chosen_profile_is_named_on_every_call_and_tabs_are_the_agents_own() {
        let text = profile_instructions("default");
        assert!(text.contains("profile_id=\"default\""));
        assert!(text.contains("tab_create(profile_id=\"default\")"));
        assert!(text.contains("Never navigate"));
    }

    #[test]
    fn without_unzoo_installed_nothing_is_asked_of_it() {
        assert_eq!(status(false, ""), json!({"installed": false, "reachable": false}));
    }
}
