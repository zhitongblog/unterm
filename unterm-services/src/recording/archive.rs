//! Reading what has already been recorded.
//!
//! The recorder needs a live pane; listing and reading finished sessions needs
//! only the files under `~/.unterm/sessions/`. This is that half, so a front
//! end can show someone their recordings without owning a recorder.

use super::index::{self, IndexEntry};
use super::render::{self, RenderConfig, RenderOutput};
use anyhow::{anyhow, Result};
use base64::Engine as _;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Persistent config loaded from `~/.unterm/recording.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordingConfig {
    #[serde(default)]
    pub recording: RecordingFlags,
    #[serde(default)]
    pub redaction: RedactionFlags,
    #[serde(default = "default_idle_minutes")]
    pub idle_rotate_minutes: u64,
}

fn default_idle_minutes() -> u64 {
    5
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordingFlags {
    #[serde(default)]
    pub enabled: bool,
}

impl Default for RecordingFlags {
    fn default() -> Self {
        Self { enabled: false }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RedactionFlags {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub custom_patterns: Vec<String>,
}

impl Default for RedactionFlags {
    fn default() -> Self {
        Self {
            enabled: true,
            custom_patterns: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            recording: RecordingFlags::default(),
            redaction: RedactionFlags::default(),
            idle_rotate_minutes: default_idle_minutes(),
        }
    }
}

fn config_path() -> PathBuf {
    unterm_protocol::state_path("recording.json")
        .unwrap_or_else(|| PathBuf::from(".unterm").join("recording.json"))
}

pub fn load_config() -> RecordingConfig {
    let p = config_path();
    if !p.exists() {
        return RecordingConfig::default();
    }
    match std::fs::read_to_string(&p) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => RecordingConfig::default(),
    }
}

#[allow(dead_code)]

pub fn list_sessions(project_filter: Option<&str>) -> Result<Vec<IndexEntry>> {
    let entries = index::load_index()?;
    let filtered: Vec<IndexEntry> = entries
        .into_iter()
        .filter(|e| match project_filter {
            Some(p) => {
                e.project_slug == p || e.project_path.as_deref().map(|x| x == p).unwrap_or(false)
            }
            None => true,
        })
        .collect();
    Ok(filtered)
}

/// Render a session's markdown on demand by reading its log file.
pub fn read_session_markdown(session_id: &str) -> Result<String> {
    let entry = index::find_entry(session_id)?
        .ok_or_else(|| anyhow!("Unknown session_id {}", session_id))?;
    let log_path = Path::new(&entry.log_path);
    let cfg = load_config();
    let render_cfg = RenderConfig {
        redaction_enabled: cfg.redaction.enabled,
        custom_patterns: cfg.redaction.custom_patterns.clone(),
    };
    let out = render::render_log(log_path, &entry, &render_cfg)?;
    Ok(out.markdown)
}

/// Write a scrollback out as markdown.
///
/// Takes the text rather than a pane: whoever asked already has it, and this
/// only has to decide where the file goes and how it reads.

/// One-shot export from a screen-engine scrollback snapshot.
pub fn export_scrollback_markdown(
    pane_id: usize,
    project_path: Option<String>,
    scroll_text: String,
    target: Option<PathBuf>,
) -> Result<(PathBuf, RenderOutput)> {
    export_scrollback_markdown_with_events(pane_id, project_path, scroll_text, target, Vec::new())
}

/// Engine-neutral wrapper for MCP callers that should not mention WezTerm's
/// pane id type directly.
pub fn export_scrollback_markdown_for_session(
    pane_id: usize,
    project_path: Option<String>,
    scroll_text: String,
    target: Option<PathBuf>,
) -> Result<(PathBuf, RenderOutput)> {
    export_scrollback_markdown(pane_id, project_path, scroll_text, target)
}

pub fn export_scrollback_markdown_with_events(
    pane_id: usize,
    project_path: Option<String>,
    scroll_text: String,
    target: Option<PathBuf>,
    semantic_events: Vec<String>,
) -> Result<(PathBuf, RenderOutput)> {
    let project_slug = project_path
        .as_deref()
        .and_then(|path| {
            Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .map(sanitize_slug)
        })
        .unwrap_or_else(|| "_orphan".to_string());
    let tab_id = pane_id as u64;
    let (_log_path_unused, md_path, started_at_iso, _stem) =
        build_paths(project_path.as_deref(), &project_slug, tab_id);
    let session_id = uuid::Uuid::new_v4().to_string();
    let cfg = load_config();
    let render_cfg = RenderConfig {
        redaction_enabled: cfg.redaction.enabled,
        custom_patterns: cfg.redaction.custom_patterns.clone(),
    };

    // Build a stub log file containing the scrollback so the renderer
    // can run uniformly.
    let log_path = md_path.with_extension("log");
    let micros = Utc::now().timestamp_micros();
    let b64 = base64::engine::general_purpose::STANDARD.encode(scroll_text.as_bytes());
    let log_line = format!("{}\tout\t{}\n", micros, b64);
    if let Some(p) = log_path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    std::fs::write(&log_path, log_line)?;

    // Try semantic zones: emit synthetic OSC 133 events per zone.
    if !semantic_events.is_empty() {
        let mut bytes: Vec<u8> = Vec::new();
        for event in semantic_events {
            let micros = Utc::now().timestamp_micros();
            let line = format!("{}\t{}\t\n", micros, event);
            bytes.extend_from_slice(line.as_bytes());
        }
        // Append the scrollback as the actual output bytes.
        let micros2 = Utc::now().timestamp_micros();
        let b64 = base64::engine::general_purpose::STANDARD.encode(scroll_text.as_bytes());
        bytes.extend_from_slice(format!("{}\tout\t{}\n", micros2, b64).as_bytes());
        std::fs::write(&log_path, &bytes)?;
    }

    let entry = IndexEntry {
        unterm_session_id: session_id.clone(),
        tab_id,
        project_path,
        project_slug,
        started_at: started_at_iso,
        ended_at: Some(Utc::now().to_rfc3339()),
        block_count: 0,
        total_lines: 0,
        bytes_raw: scroll_text.len() as u64,
        log_path: log_path.display().to_string(),
        md_path: md_path.display().to_string(),
        exit_reason: Some("user_export".to_string()),
        parent_session_id: None,
        osc133_active: false,
        redaction_active: cfg.redaction.enabled,
        redaction_count: 0,
        trace_ids: Vec::new(),
        // User-triggered markdown export from a live pane — there's no
        // per-pane env capture here, so we fall back to whatever the GUI
        // process inherited. Usually `None`.
        agent_id: std::env::var("UNTERM_AGENT_ID").ok(),
        agent_manifest_version: std::env::var("UNTERM_AGENT_MANIFEST_VERSION").ok(),
        agent_profile: std::env::var("UNTERM_PROFILE").ok(),
    };
    let out = render::render_log(&log_path, &entry, &render_cfg)?;
    let dest = target.unwrap_or(md_path);
    if let Some(p) = dest.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    std::fs::write(&dest, out.markdown.as_bytes())?;

    let mut entry2 = entry;
    entry2.block_count = out.block_count;
    entry2.total_lines = out.total_lines;
    entry2.osc133_active = out.osc133_active;
    entry2.redaction_count = out.redaction_count;
    index::upsert_entry(entry2).ok();

    Ok((dest, out))
}

pub fn sanitize_slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

pub fn build_paths(
    project_path: Option<&str>,
    project_slug: &str,
    tab_id: u64,
) -> (PathBuf, PathBuf, String, String) {
    let (date, hms, iso) = timestamp_components();
    // Prefer storing recordings inside the project directory itself so they
    // travel with the project (git, archive, share). Fall back to the
    // user-global `~/.unterm/sessions/_orphan/` when there's no project or
    // the project dir is read-only / not writable for any reason.
    let dir = preferred_session_dir(project_path, project_slug, &date);
    let _ = std::fs::create_dir_all(&dir);
    let stem = format!("tab-{}-{}", tab_id, hms);
    let log_path = dir.join(format!("{}.log", stem));
    let md_path = dir.join(format!("{}.md", stem));
    (log_path, md_path, iso, stem)
}

pub fn timestamp_components() -> (String, String, String) {
    let now = chrono::Local::now();
    let date = now.format("%Y-%m-%d").to_string();
    let hms = now.format("%H%M%S").to_string();
    let iso = now.to_rfc3339();
    (date, hms, iso)
}

/// Tell git that `<project>/.unterm` is Unterm's, not the project's.
///
/// Recordings and exports land there, and without this they are untracked
/// files in the user's repo: `git status` fills up, and a fleet refuses to
/// start because the worktree is not clean.
pub fn keep_out_of_git(project: &std::path::Path) {
    let ignore = project.join(".unterm").join(".gitignore");
    if !ignore.exists() {
        let _ = std::fs::write(ignore, "*\n");
    }
}

pub fn preferred_session_dir(
    project_path: Option<&str>,
    project_slug: &str,
    date: &str,
) -> PathBuf {
    if let Some(p) = project_path {
        let path = PathBuf::from(p);
        let in_project = path.join(".unterm").join("sessions").join(date);
        // Only use project-local storage when we can actually write there.
        // Probe by attempting to create the directory; revert on failure.
        if std::fs::create_dir_all(&in_project).is_ok() && is_dir_writable(&in_project) {
            keep_out_of_git(&path);
            return in_project;
        }
        log::info!(
            "project dir {} not writable for recording; falling back to ~/.unterm/sessions",
            path.display()
        );
    }
    let slug = if project_slug.is_empty() {
        "_orphan"
    } else {
        project_slug
    };
    index::sessions_root().join(slug).join(date)
}

pub fn is_dir_writable(dir: &std::path::Path) -> bool {
    // Cheap probe: try to create a hidden tempfile, write 1 byte, delete it.
    let probe = dir.join(".unterm-write-probe");
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(mut f) => {
            use std::io::Write;
            let ok = f.write_all(b"u").is_ok();
            drop(f);
            let _ = std::fs::remove_file(&probe);
            ok
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod keep_out_of_git_tests {
    #[test]
    fn a_recording_in_a_project_does_not_dirty_it() {
        let project = std::env::temp_dir().join(format!("unterm-ignore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&project);
        std::fs::create_dir_all(&project).unwrap();
        let dir = super::preferred_session_dir(project.to_str(), "demo", "2026-09-25");
        assert!(dir.starts_with(&project));
        assert_eq!(
            std::fs::read_to_string(project.join(".unterm").join(".gitignore")).unwrap(),
            "*\n"
        );
        let _ = std::fs::remove_dir_all(&project);
    }
}
