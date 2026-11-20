//! Unzoo Browser Native Hub — zero-config AI browser synergy, local dev-server
//! preview sniffer, capability lease control, and guided onboarding/download.
//!
//! Instead of bloating the terminal binary with an embedded WebView, Unterm
//! pairs with **Unzoo Browser** over local HTTP MCP (`unterm_providers::unzoo`).
//! This module bridges Unzoo Browser into the native GPU terminal UI:
//! - Detects whether Unzoo Browser is running, installed on disk, or not yet installed.
//! - Sniffs local dev-server URLs (`http://localhost:3000`, `http://127.0.0.1:5173`, etc.)
//!   from terminal panes for 1-click preview inside Unzoo Browser.
//! - Provides a guided download & onboarding flow (`https://unzoo.com`) when
//!   Unzoo Browser is not yet installed.

use std::path::PathBuf;

/// Official download & product page for Unzoo Browser.
pub const UNZOO_DOWNLOAD_URL: &str = "https://unzoo.com";
pub const UNZOO_PROVIDER_ID: &str = "unzoo";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnzooState {
    /// Unzoo Browser is running and advertising its local MCP endpoint.
    Running {
        endpoint: String,
        bound: bool,
        paused: bool,
        active_leases: usize,
    },
    /// Unzoo Browser is installed on disk at `path`, but not currently running.
    Installed { path: PathBuf },
    /// Unzoo Browser is not found on this machine — offer guided download.
    NotInstalled,
}

/// Detect the current state of Unzoo Browser on this machine.
pub fn detect_state() -> UnzooState {
    let statuses = unterm_services::providers::statuses();
    if let Some(s) = statuses
        .into_iter()
        .find(|s| s.id == UNZOO_PROVIDER_ID && s.state != "offline")
    {
        let bound = matches!(s.state.as_str(), "ready" | "paused") || s.pinned.is_some();
        let paused = s.state == "paused";
        return UnzooState::Running {
            endpoint: s.endpoint,
            bound,
            paused,
            active_leases: s.live_leases,
        };
    }

    if let Some(path) = installed_app_path() {
        return UnzooState::Installed { path };
    }

    UnzooState::NotInstalled
}

/// Check standard per-platform installation paths for Unzoo Browser.
pub fn installed_app_path() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from);

    let mut candidates = Vec::new();
    if cfg!(target_os = "macos") {
        candidates.push(PathBuf::from("/Applications/Unzoo Browser.app"));
        candidates.push(PathBuf::from("/Applications/Unzoo.app"));
        if let Some(h) = &home {
            candidates.push(h.join("Applications/Unzoo Browser.app"));
            candidates.push(h.join("Applications/Unzoo.app"));
        }
    } else if cfg!(target_os = "windows") {
        for key in ["LOCALAPPDATA", "ProgramFiles", "ProgramW6432"] {
            if let Ok(base) = std::env::var(key).map(PathBuf::from) {
                candidates.push(base.join("Programs/Unzoo Browser/Unzoo Browser.exe"));
                candidates.push(base.join("Unzoo Browser/Unzoo Browser.exe"));
            }
        }
    } else {
        candidates.push(PathBuf::from("/usr/bin/unzoo-browser"));
        candidates.push(PathBuf::from("/usr/local/bin/unzoo-browser"));
        if let Some(h) = &home {
            candidates.push(h.join(".local/bin/unzoo-browser"));
        }
    }

    candidates.into_iter().find(|p| p.exists())
}

/// Launch Unzoo Browser (optionally navigating to `url`).
pub fn launch_unzoo(url: Option<&str>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let app_name = installed_app_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "Unzoo Browser".to_string());
        let mut cmd = crate::git::hidden_command("open");
        cmd.arg("-a").arg(app_name);
        if let Some(u) = url {
            cmd.arg(u);
        }
        let status = cmd.status().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("Failed to launch Unzoo Browser via open -a".to_string());
        }
        return Ok(());
    }

    #[cfg(not(target_os = "macos"))]
    {
        let Some(bin) = installed_app_path() else {
            return Err("Unzoo Browser executable not found".to_string());
        };
        let mut cmd = crate::git::hidden_command(&bin.to_string_lossy());
        if let Some(u) = url {
            cmd.arg(u);
        }
        cmd.spawn().map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Extract live local dev-server URLs (`http://localhost:3000`, `http://127.0.0.1:5173`,
/// `http://0.0.0.0:8080` -> `http://localhost:8080`) from terminal screen lines.
pub fn extract_dev_urls(lines: &[String]) -> Vec<String> {
    let mut urls = Vec::new();
    for line in lines.iter().rev().take(40) {
        for word in line.split_whitespace() {
            let clean = word
                .trim_matches(|c: char| {
                    matches!(
                        c,
                        '"' | '\'' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';' | '।'
                    )
                })
                .trim_end_matches('.');
            for prefix in [
                "http://localhost:",
                "https://localhost:",
                "http://127.0.0.1:",
                "https://127.0.0.1:",
                "http://0.0.0.0:",
                "http://[::1]:",
            ] {
                if let Some(idx) = clean.find(prefix) {
                    let candidate = &clean[idx..];
                    // Normalize 0.0.0.0 to localhost so the browser can open it directly.
                    let normalized = candidate.replace("http://0.0.0.0:", "http://localhost:");
                    // Ignore internal MCP port endpoints (`/mcp`).
                    if normalized.ends_with("/mcp") {
                        continue;
                    }
                    if !urls.contains(&normalized) {
                        urls.push(normalized);
                    }
                }
            }
        }
    }
    urls
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_and_normalizes_local_dev_server_urls() {
        let lines = vec![
            "  ➜  Local:   http://localhost:5173/".to_string(),
            "  ➜  Network: http://0.0.0.0:3000/dashboard,".to_string(),
            "  MCP internal: http://127.0.0.1:51999/mcp".to_string(),
        ];
        let urls = extract_dev_urls(&lines);
        assert_eq!(
            urls,
            vec![
                "http://localhost:3000/dashboard".to_string(),
                "http://localhost:5173/".to_string(),
            ]
        );
    }
}
