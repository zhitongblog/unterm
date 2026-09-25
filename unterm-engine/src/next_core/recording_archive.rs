use super::NextCoreRecording;
use anyhow::Result;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RecordingIndexEntry {
    unterm_session_id: String,
    tab_id: u64,
    project_path: Option<String>,
    project_slug: String,
    started_at: String,
    ended_at: Option<String>,
    block_count: u64,
    total_lines: u64,
    bytes_raw: u64,
    log_path: String,
    md_path: String,
    exit_reason: Option<String>,
    parent_session_id: Option<String>,
    osc133_active: bool,
    redaction_active: bool,
    redaction_count: u64,
    trace_ids: Vec<String>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    agent_manifest_version: Option<String>,
    #[serde(default)]
    agent_profile: Option<String>,
}

static RECORDING_INDEX_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub(super) fn project_slug(project_path: Option<&str>) -> String {
    project_path
        .and_then(|path| {
            Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .map(sanitize_slug)
        })
        .unwrap_or_else(|| "_orphan".to_string())
}

pub(super) fn paths(
    pane_id: usize,
    project_path: Option<&str>,
    project_slug: &str,
    timestamp: &str,
) -> (PathBuf, PathBuf) {
    let dir = project_path
        .map(PathBuf::from)
        .map(|path| path.join(".unterm").join("sessions").join(timestamp))
        .unwrap_or_else(|| sessions_root().join(project_slug).join(timestamp));
    let _ = std::fs::create_dir_all(&dir);
    // Ours, not the project's: git is told to look away, or a recording
    // leaves the repo dirty and a fleet refuses to start in it.
    if let Some(project) = project_path {
        let ignore = PathBuf::from(project).join(".unterm").join(".gitignore");
        if !ignore.exists() {
            let _ = std::fs::write(ignore, "*\n");
        }
    }
    let stem = format!("tab-{pane_id}-{timestamp}");
    (
        dir.join(format!("{stem}.log")),
        dir.join(format!("{stem}.md")),
    )
}

pub(super) fn upsert_index(recording: &NextCoreRecording, ended_at: Option<String>) -> Result<()> {
    let _guard = recording_index_lock().lock();
    let path = index_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut entries: Vec<RecordingIndexEntry> = if path.exists() {
        let raw = std::fs::read_to_string(&path).unwrap_or_default();
        if raw.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&raw).unwrap_or_default()
        }
    } else {
        Vec::new()
    };
    let entry = index_entry(recording, ended_at);
    if let Some(existing) = entries
        .iter_mut()
        .find(|existing| existing.unterm_session_id == entry.unterm_session_id)
    {
        *existing = entry;
    } else {
        entries.push(entry);
    }
    std::fs::write(path, serde_json::to_string_pretty(&entries)?)?;
    Ok(())
}

fn sanitize_slug(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn sessions_root() -> PathBuf {
    if let Ok(root) = std::env::var("UNTERM_SESSIONS_ROOT") {
        if !root.trim().is_empty() {
            return PathBuf::from(root);
        }
    }
    unterm_protocol::state_dir()
        // With no home to anchor to, keep recording into the working
        // directory rather than dropping the session on the floor.
        .unwrap_or_else(|| PathBuf::from(".").join(".unterm"))
        .join("sessions")
}

fn index_path() -> PathBuf {
    sessions_root().join("index.json")
}

fn recording_index_lock() -> &'static Mutex<()> {
    RECORDING_INDEX_LOCK.get_or_init(|| Mutex::new(()))
}

fn index_entry(recording: &NextCoreRecording, ended_at: Option<String>) -> RecordingIndexEntry {
    RecordingIndexEntry {
        unterm_session_id: recording.session_id.clone(),
        tab_id: recording.pane_id as u64,
        project_path: recording.project_path.clone(),
        project_slug: recording.project_slug.clone(),
        started_at: recording.started_at.clone(),
        ended_at,
        block_count: recording.block_count,
        total_lines: recording.text_preview.lines().count() as u64,
        bytes_raw: recording.bytes_raw,
        log_path: recording.log_path.display().to_string(),
        md_path: recording.md_path.display().to_string(),
        exit_reason: None,
        parent_session_id: None,
        osc133_active: recording.osc133_seen,
        redaction_active: true,
        redaction_count: 0,
        trace_ids: recording.trace_ids.clone(),
        agent_id: std::env::var("UNTERM_AGENT_ID").ok(),
        agent_manifest_version: std::env::var("UNTERM_AGENT_MANIFEST_VERSION").ok(),
        agent_profile: std::env::var("UNTERM_PROFILE").ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_sessions_root(prefix: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        std::env::temp_dir().join(format!("{prefix}-{}-{suffix}", std::process::id()))
    }

    #[test]
    fn project_slug_sanitizes_project_basename() {
        // A backslash only separates on Windows; elsewhere that string is
        // one long basename and the assertion would assert the wrong thing.
        #[cfg(windows)]
        assert_eq!(project_slug(Some("C:\\work\\my app")), "my-app");
        assert_eq!(project_slug(Some("/tmp/app.alpha")), "app.alpha");
        assert_eq!(project_slug(None), "_orphan");
    }

    #[test]
    fn paths_use_project_local_directory_when_project_exists() {
        let root = temp_sessions_root("unterm-next-core-archive-project").join("demo");
        let (log_path, md_path) = paths(7, root.to_str(), "demo", "12345");
        assert_eq!(
            log_path,
            root.join(".unterm")
                .join("sessions")
                .join("12345")
                .join("tab-7-12345.log")
        );
        assert_eq!(
            md_path,
            root.join(".unterm")
                .join("sessions")
                .join("12345")
                .join("tab-7-12345.md")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn upsert_index_replaces_existing_session_entry() -> Result<()> {
        let sessions_root = temp_sessions_root("unterm-next-core-archive");
        let _ = std::fs::remove_dir_all(&sessions_root);
        std::fs::create_dir_all(&sessions_root)?;
        let previous_root = std::env::var("UNTERM_SESSIONS_ROOT").ok();
        std::env::set_var("UNTERM_SESSIONS_ROOT", &sessions_root);

        let mut recording = NextCoreRecording {
            session_id: "session-1".to_string(),
            pane_id: 42,
            project_path: None,
            project_slug: "_orphan".to_string(),
            started_at: "100".to_string(),
            log_path: sessions_root.join("session.log"),
            md_path: sessions_root.join("session.md"),
            bytes_raw: 4,
            block_count: 1,
            trace_ids: vec!["trace-1".to_string()],
            text_preview: "one\n".to_string(),
            blocks: Vec::new(),
            osc133_seen: false,
            command_blocks: Vec::new(),
            active_command: None,
        };
        upsert_index(&recording, None)?;
        recording.bytes_raw = 8;
        recording.trace_ids.push("trace-2".to_string());
        upsert_index(&recording, Some("200".to_string()))?;

        match previous_root {
            Some(value) => std::env::set_var("UNTERM_SESSIONS_ROOT", value),
            None => std::env::remove_var("UNTERM_SESSIONS_ROOT"),
        }

        let raw = std::fs::read_to_string(sessions_root.join("index.json"))?;
        assert_eq!(raw.matches("\"unterm_session_id\"").count(), 1);
        assert!(raw.contains("\"bytes_raw\": 8"));
        assert!(raw.contains("\"trace-2\""));
        assert!(raw.contains("\"ended_at\": \"200\""));

        let _ = std::fs::remove_dir_all(&sessions_root);
        Ok(())
    }
}
