//! Google Cloud SDK configuration sniffer.
//!
//! Unlike the other sniffers, gcloud doesn't store credentials in a
//! single file — auth tokens live in a SQLite database
//! (`~/.config/gcloud/credentials.db`) plus encrypted blobs, and
//! they refresh frequently. Rather than reach into SQLite, we shell
//! out to `gcloud config configurations list --format=json` which
//! gives us the *names* of configured projects/accounts. The wizard
//! then knows "you have a 'work' gcloud configuration; would you
//! like a profile that runs `gcloud config configurations activate
//! work` on shell launch?"
//!
//! This means we don't import the gcloud token into Unterm's
//! keychain — the gcloud SDK manages its own refresh. We *do*
//! expose `CLOUDSDK_ACTIVE_CONFIG_NAME` as the suggested env var so
//! a profile-bound shell automatically switches gcloud config
//! without an extra command in `.zshrc`.

use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Command;

use super::{Candidate, CandidateSource};

#[derive(Debug, Deserialize)]
struct GcloudConfiguration {
    name: String,
    #[serde(default)]
    properties: serde_json::Value,
}

pub fn sniff() -> Result<Vec<Candidate>> {
    // Fast probe: is `gcloud` even on PATH? If not, skip silently.
    if which_gcloud().is_none() {
        return Ok(Vec::new());
    }
    let mut cmd = Command::new("gcloud");
    cmd.args([
        "config",
        "configurations",
        "list",
        "--format=json",
        "--quiet",
    ])
    .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let output = match cmd.output() {
        Ok(o) => o,
        Err(_) => return Ok(Vec::new()),
    };
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Vec<GcloudConfiguration> = match serde_json::from_str(stdout.trim()) {
        Ok(p) => p,
        Err(_) => return Ok(Vec::new()),
    };

    // The configurations file itself lives at this path; we use it
    // as the candidate's origin so wizard tooltips can point users
    // at something concrete.
    let origin = dirs_next::home_dir()
        .map(|h| h.join(".config").join("gcloud").join("configurations"))
        .unwrap_or_else(|| PathBuf::from("gcloud"));

    Ok(parsed
        .into_iter()
        .map(|c| {
            let account = c
                .properties
                .get("core")
                .and_then(|core| core.get("account"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let project = c
                .properties
                .get("core")
                .and_then(|core| core.get("project"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let label = match (&account, &project) {
                (Some(a), Some(p)) => format!("{} ({a}, project={p})", c.name),
                (Some(a), None) => format!("{} ({a})", c.name),
                (None, Some(p)) => format!("{} (project={p})", c.name),
                (None, None) => c.name.clone(),
            };
            Candidate {
                source: CandidateSource::Gcloud,
                label,
                suggested_env_name: "CLOUDSDK_ACTIVE_CONFIG_NAME".to_string(),
                suggested_value: Some(c.name.clone()),
                host: None,
                user: account,
                origin: origin.clone(),
            }
        })
        .collect())
}

fn which_gcloud() -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join(if cfg!(windows) {
            "gcloud.cmd"
        } else {
            "gcloud"
        });
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// No unit tests here — the parser logic is mostly a pass-through of
// gcloud's JSON, and gcloud itself isn't installed in CI. The wizard
// integration test (#36) exercises this code via a stubbed gcloud
// binary on PATH.
