//! Predictive command text at the terminal cursor.
//!
//! The predictor itself lives in `unterm-services`; this module is the small
//! GUI bridge that translates winit keys and keeps an overlay inside its pane.

use termwiz::cell::unicode_column_width;
use unterm_services::ghost_text::InputEvent;
use winit::keyboard::{Key, NamedKey};

/// Merge the signed on-disk manifest flag catalog once, off the key-event
/// path. Built-in completions remain immediately available while this worker
/// is running; manifest-only agents gain completions when it finishes.
fn ensure_manifest_flags_loading() {
    use std::sync::OnceLock;
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        std::thread::Builder::new()
            .name("ghost-manifest-flags".to_string())
            .spawn(|| {
                let Ok(set) = unterm_agents::fetch_manifests_offline() else {
                    return;
                };
                for manifest in set.for_current_platform() {
                    let args: Vec<String> = manifest
                        .launch
                        .flag_catalog
                        .iter()
                        .map(|flag| flag.arg.clone())
                        .collect();
                    unterm_services::ghost_text::merge_manifest_flags(&manifest.launch.exec, &args);
                }
            })
            .ok();
    });
}

/// Whether keys typed into `pane_id` are a command line worth predicting.
///
/// Not while an agent owns the pane. The predictor only sees keystrokes, so
/// inside Claude Code's prompt box it recorded every prompt as a "command",
/// shared it with every other tab, and drew it at the cursor as grey text --
/// on top of the agent's own suggestion, and sometimes a prompt from another
/// tab. It read as the agent suggesting something it never saw. The agent has
/// its own completion; ours is for the shell.
///
/// From what the status bar already knows about the pane (the foreground
/// process, matched against the agent manifests), so a keystroke never waits
/// on the process table.
pub fn pane_takes_predictions(pane_id: u64) -> bool {
    crate::statsbar::known_facts(pane_id as usize)
        .agent_id
        .is_none()
}

/// Feed a key that reached the PTY to the predictor.
///
/// Returns true when the overlay may have changed and therefore needs a frame.
pub fn observe_key(pane_id: u64, key: &Key, ctrl: bool, alt: bool) -> bool {
    if !pane_takes_predictions(pane_id) {
        // Nothing typed here is kept, and nothing half-typed before the
        // agent started is left hanging at its prompt.
        unterm_services::ghost_text::cancel_input(pane_id);
        return false;
    }
    ensure_manifest_flags_loading();
    match effect_for(key, ctrl, alt) {
        Effect::Events(events) => {
            for event in events {
                unterm_services::ghost_text::observe(pane_id, event, &[]);
            }
            true
        }
        Effect::Cancel => {
            unterm_services::ghost_text::cancel_input(pane_id);
            true
        }
        Effect::Ignore => false,
    }
}

/// Feed text committed by an input method to the predictor.
pub fn observe_text(pane_id: u64, text: &str) -> bool {
    if !pane_takes_predictions(pane_id) {
        unterm_services::ghost_text::cancel_input(pane_id);
        return false;
    }
    let mut changed = false;
    for ch in text.chars().filter(|ch| !ch.is_control()) {
        unterm_services::ghost_text::observe(pane_id, InputEvent::Char(ch), &[]);
        changed = true;
    }
    changed
}

/// Right Arrow and End accept a visible prediction, as in the previous GUI.
pub fn is_accept_key(key: &Key, ctrl: bool, shift: bool, alt: bool) -> bool {
    if ctrl || shift || alt {
        return false;
    }
    matches!(key, Key::Named(NamedKey::ArrowRight | NamedKey::End))
}

/// Keep the overlay within the focused pane, measuring terminal columns rather
/// than Unicode scalar values so CJK and emoji do not spill into a neighbour.
pub fn truncate_to_columns(text: &str, max_columns: usize) -> String {
    let mut used = 0usize;
    text.chars()
        .take_while(|ch| {
            let width = column_width(*ch);
            if used.saturating_add(width) > max_columns {
                return false;
            }
            used += width;
            true
        })
        .collect()
}

/// A theme-derived muted foreground remains legible in both light and dark
/// themes while still reading as a prediction rather than committed output.
pub fn color(foreground: [f32; 4], background: [f32; 4]) -> [f32; 4] {
    const FOREGROUND_SHARE: f32 = 0.48;
    [
        background[0] + (foreground[0] - background[0]) * FOREGROUND_SHARE,
        background[1] + (foreground[1] - background[1]) * FOREGROUND_SHARE,
        background[2] + (foreground[2] - background[2]) * FOREGROUND_SHARE,
        1.0,
    ]
}

enum Effect {
    Events(Vec<InputEvent>),
    Cancel,
    Ignore,
}

fn effect_for(key: &Key, ctrl: bool, alt: bool) -> Effect {
    match key {
        Key::Character(text) if ctrl => {
            let Some(ch) = text.chars().next().map(|ch| ch.to_ascii_lowercase()) else {
                return Effect::Ignore;
            };
            match ch {
                'c' | 'g' => Effect::Events(vec![InputEvent::Cancel]),
                'u' | 'w' => Effect::Events(vec![InputEvent::ClearLine]),
                _ => Effect::Ignore,
            }
        }
        Key::Character(_) if alt => Effect::Ignore,
        Key::Character(text) => {
            let events = text
                .chars()
                .filter(|ch| !ch.is_control())
                .map(InputEvent::Char)
                .collect::<Vec<_>>();
            if events.is_empty() {
                Effect::Ignore
            } else {
                Effect::Events(events)
            }
        }
        Key::Named(NamedKey::Space) if !ctrl && !alt => Effect::Events(vec![InputEvent::Char(' ')]),
        Key::Named(NamedKey::Enter) => Effect::Events(vec![InputEvent::Enter]),
        Key::Named(NamedKey::Backspace) => Effect::Events(vec![InputEvent::Backspace]),
        Key::Named(NamedKey::Escape | NamedKey::Cancel) => Effect::Events(vec![InputEvent::Cancel]),
        // These keys let the shell rewrite or reposition within the line.
        // Since the GUI cannot see the resulting readline buffer, discard its
        // heuristic copy rather than draw a prediction at the wrong position.
        Key::Named(
            NamedKey::ArrowUp
            | NamedKey::ArrowDown
            | NamedKey::ArrowLeft
            | NamedKey::ArrowRight
            | NamedKey::Home
            | NamedKey::End
            | NamedKey::Tab
            | NamedKey::Delete,
        ) => Effect::Cancel,
        _ => Effect::Ignore,
    }
}

fn column_width(ch: char) -> usize {
    let mut encoded = [0u8; 4];
    unicode_column_width(ch.encode_utf8(&mut encoded), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn character(text: &str) -> Key {
        Key::Character(text.into())
    }

    fn named(key: NamedKey) -> Key {
        Key::Named(key)
    }

    #[test]
    fn plain_text_reaches_the_predictor() {
        let pane = 910_001;
        unterm_services::ghost_text::forget(pane);
        assert!(observe_key(pane, &character("cod"), false, false));
        let snapshot = unterm_services::ghost_text::debug_snapshot(pane).unwrap();
        assert_eq!(snapshot.input_buffer, "cod");
        assert_eq!(snapshot.ghost.as_deref(), Some("ex"));
        unterm_services::ghost_text::forget(pane);
    }

    #[test]
    fn control_editing_has_the_same_meaning_as_readline() {
        let pane = 910_002;
        unterm_services::ghost_text::forget(pane);
        observe_key(pane, &character("abc"), false, false);
        observe_key(pane, &character("w"), true, false);
        assert_eq!(
            unterm_services::ghost_text::debug_snapshot(pane)
                .unwrap()
                .input_buffer,
            ""
        );
        unterm_services::ghost_text::forget(pane);
    }

    #[test]
    fn navigation_invalidates_the_heuristic_line() {
        let pane = 910_003;
        unterm_services::ghost_text::forget(pane);
        observe_key(pane, &character("cod"), false, false);
        observe_key(pane, &named(NamedKey::ArrowLeft), false, false);
        assert_eq!(
            unterm_services::ghost_text::debug_snapshot(pane)
                .unwrap()
                .input_buffer,
            ""
        );
        unterm_services::ghost_text::forget(pane);
    }

    #[test]
    fn only_plain_right_and_end_accept() {
        assert!(is_accept_key(
            &named(NamedKey::ArrowRight),
            false,
            false,
            false
        ));
        assert!(is_accept_key(&named(NamedKey::End), false, false, false));
        assert!(!is_accept_key(
            &named(NamedKey::ArrowRight),
            true,
            false,
            false
        ));
        assert!(!is_accept_key(
            &named(NamedKey::ArrowLeft),
            false,
            false,
            false
        ));
    }

    #[test]
    fn truncation_counts_wide_characters_as_two_columns() {
        assert_eq!(truncate_to_columns("a中文b", 4), "a中");
        assert_eq!(truncate_to_columns("a中文b", 5), "a中文");
        assert_eq!(truncate_to_columns("abc", 0), "");
    }

    #[test]
    fn dim_color_stays_between_theme_background_and_foreground() {
        assert_eq!(
            color([1.0, 1.0, 1.0, 1.0], [0.0, 0.0, 0.0, 1.0]),
            [0.48, 0.48, 0.48, 1.0]
        );
        assert_eq!(
            color([0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]),
            [0.52, 0.52, 0.52, 1.0]
        );
    }
}
