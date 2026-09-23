//! The last window, brought back.
//!
//! Closing writes what mattered — the window's size, whether it was
//! maximised, and where each tab was — and the next plain launch reads it,
//! so a terminal opens where its owner left off instead of at the factory
//! defaults. A launch that names a directory or a command on the line is
//! asking for something specific and is left alone.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LastSession {
    /// Physical pixels, exactly as the window reported them.
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
    /// One entry per tab, in the strip's order.
    pub cwds: Vec<String>,
}

fn path() -> Option<std::path::PathBuf> {
    // Through the shared resolver so an isolated run (a test, a headless
    // launch) cannot reopen the real user's tabs and then write its own
    // over them.
    unterm_protocol::state_path("last_session.json")
}

pub fn save(state: &LastSession) {
    if let Some(path) = path() {
        save_to(&path, state);
    }
}

fn save_to(path: &std::path::Path, state: &LastSession) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(text) = serde_json::to_string_pretty(state) else {
        return;
    };
    // Beside the real file, then renamed over it: this is now written while
    // the process runs, and a crash in the middle of a plain write would
    // leave half a file -- which `load` rejects, so the next launch would
    // reopen nothing at all.
    let partial = path.with_extension("json.partial");
    if std::fs::write(&partial, text).is_ok() && std::fs::rename(&partial, path).is_err() {
        let _ = std::fs::remove_file(&partial);
    }
}

pub fn load() -> Option<LastSession> {
    let text = std::fs::read_to_string(path()?).ok()?;
    let state: LastSession = serde_json::from_str(&text).ok()?;
    // A window smaller than a postage stamp is stored corruption, not a
    // preference.
    (state.width >= 200 && state.height >= 150).then_some(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_round_trip_keeps_every_field() {
        let state = LastSession {
            width: 1600,
            height: 900,
            maximized: true,
            cwds: vec!["D:/code".into(), "D:/code/unterm".into()],
        };
        let text = serde_json::to_string(&state).unwrap();
        let back: LastSession = serde_json::from_str(&text).unwrap();
        assert_eq!(back, state);
    }

    /// Saving replaces the file whole and leaves nothing beside it.
    ///
    /// It is written every few seconds while the window is up now, so a
    /// crash can land in the middle of a write; the rename is what keeps
    /// that from leaving half a file for the next launch to reject.
    #[test]
    fn saving_replaces_the_file_whole() {
        let dir = std::env::temp_dir().join(format!(
            "unterm-last-session-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let path = dir.join("last_session.json");
        let first = LastSession {
            width: 1600,
            height: 900,
            maximized: false,
            cwds: vec!["/a".into(), "/b".into(), "/c".into()],
        };
        let second = LastSession {
            cwds: vec!["/a".into()],
            ..first.clone()
        };
        save_to(&path, &first);
        save_to(&path, &second);
        let back: LastSession =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name())
            .collect();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(back, second);
        assert_eq!(leftovers, vec![std::ffi::OsString::from("last_session.json")]);
    }
}
