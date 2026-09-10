//! Who the mouse belongs to.
//!
//! A terminal's mouse has two owners and they take turns. Normally it is the
//! terminal's: dragging selects text, the wheel scrolls the scrollback. But a
//! program can ask for it -- vim, htop, less all do -- and from then on a
//! click is the program's to interpret, and the terminal must keep its hands
//! off or the user gets a selection they did not ask for on top of a click
//! that did work.
//!
//! Shift is the way back. Every terminal treats a shifted drag as "I want the
//! terminal, not the program", which is how you copy text out of vim without
//! leaving it.

use termwiz::input::Modifiers;
use unterm_engine::next_core::mouse_encoding::{
    MouseButton, MouseEvent, MouseEventKind, MouseModes, MouseTracking,
};

/// What the front end should do with a mouse event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Send it to the program.
    ToProgram(MouseEvent),
    /// Keep it: selection, scrollback, the front end's own business.
    ToTerminal,
}

/// Which modifiers were held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Held {
    fn modifiers(self) -> Modifiers {
        let mut modifiers = Modifiers::NONE;
        if self.shift {
            modifiers |= Modifiers::SHIFT;
        }
        if self.ctrl {
            modifiers |= Modifiers::CTRL;
        }
        if self.alt {
            modifiers |= Modifiers::ALT;
        }
        modifiers
    }
}

/// Decide where a mouse event goes.
///
/// `button` is the button pressed or released, or the one held during a
/// motion. `None` on a motion with nothing held.
pub fn route(
    modes: MouseModes,
    kind: MouseEventKind,
    button: Option<MouseButton>,
    column: usize,
    row: usize,
    held: Held,
) -> Route {
    // Shift always means the user wants the terminal, whatever the program
    // asked for. Without this there is no way to select text inside a
    // full-screen program.
    if held.shift {
        return Route::ToTerminal;
    }

    let wanted = match modes.tracking {
        MouseTracking::None => false,
        // Press only: a release or a motion is still the terminal's, and
        // sending one would be a byte the program never asked for.
        MouseTracking::X10 => kind == MouseEventKind::Press,
        MouseTracking::ButtonEvent => kind != MouseEventKind::Motion,
        // Motion, but only while a button is down.
        MouseTracking::ButtonMotion => kind != MouseEventKind::Motion || button.is_some(),
        MouseTracking::AnyEvent => true,
    };
    if !wanted {
        return Route::ToTerminal;
    }

    Route::ToProgram(MouseEvent {
        kind,
        button,
        column,
        row,
        modifiers: held.modifiers(),
    })
}

/// Wheel notches as button presses, which is how a terminal reports them.
///
/// A wheel has no release: xterm sends a press for each notch and nothing
/// else, so a program that counts releases would wait forever.
pub fn wheel_button(lines: f32) -> Option<MouseButton> {
    if lines < 0.0 {
        Some(MouseButton::WheelUp)
    } else if lines > 0.0 {
        Some(MouseButton::WheelDown)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes(tracking: MouseTracking) -> MouseModes {
        MouseModes {
            tracking,
            sgr: true,
            urxvt: false,
            utf8: false,
        }
    }

    fn press(modes: MouseModes, held: Held) -> Route {
        route(
            modes,
            MouseEventKind::Press,
            Some(MouseButton::Left),
            3,
            4,
            held,
        )
    }

    #[test]
    fn with_reporting_off_the_terminal_keeps_the_mouse() {
        assert_eq!(
            press(modes(MouseTracking::None), Held::default()),
            Route::ToTerminal
        );
    }

    #[test]
    fn a_program_that_asked_for_clicks_gets_them() {
        match press(modes(MouseTracking::ButtonEvent), Held::default()) {
            Route::ToProgram(event) => {
                assert_eq!(event.column, 3);
                assert_eq!(event.row, 4);
                assert_eq!(event.button, Some(MouseButton::Left));
            }
            other => panic!("expected the program to get it, got {other:?}"),
        }
    }

    #[test]
    fn shift_takes_the_mouse_back_from_the_program() {
        // Without this there is no way to select text inside vim.
        let held = Held {
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            press(modes(MouseTracking::AnyEvent), held),
            Route::ToTerminal
        );
    }

    #[test]
    fn x10_gets_presses_only() {
        let m = modes(MouseTracking::X10);
        assert!(matches!(press(m, Held::default()), Route::ToProgram(_)));
        assert_eq!(
            route(
                m,
                MouseEventKind::Release,
                Some(MouseButton::Left),
                0,
                0,
                Held::default()
            ),
            Route::ToTerminal,
            "X10 has no release; sending one is a byte nobody asked for"
        );
    }

    #[test]
    fn button_event_mode_does_not_get_bare_motion() {
        let m = modes(MouseTracking::ButtonEvent);
        assert_eq!(
            route(m, MouseEventKind::Motion, None, 1, 1, Held::default()),
            Route::ToTerminal
        );
    }

    #[test]
    fn button_motion_mode_gets_motion_only_while_dragging() {
        let m = modes(MouseTracking::ButtonMotion);
        assert_eq!(
            route(m, MouseEventKind::Motion, None, 1, 1, Held::default()),
            Route::ToTerminal,
            "nothing held is not a drag"
        );
        assert!(matches!(
            route(
                m,
                MouseEventKind::Motion,
                Some(MouseButton::Left),
                1,
                1,
                Held::default()
            ),
            Route::ToProgram(_)
        ));
    }

    #[test]
    fn any_event_mode_gets_bare_motion() {
        assert!(matches!(
            route(
                modes(MouseTracking::AnyEvent),
                MouseEventKind::Motion,
                None,
                1,
                1,
                Held::default()
            ),
            Route::ToProgram(_)
        ));
    }

    #[test]
    fn modifiers_travel_with_the_event() {
        let held = Held {
            shift: false,
            ctrl: true,
            alt: true,
        };
        match press(modes(MouseTracking::ButtonEvent), held) {
            Route::ToProgram(event) => {
                assert!(event.modifiers.contains(Modifiers::CTRL));
                assert!(event.modifiers.contains(Modifiers::ALT));
            }
            other => panic!("expected the program to get it, got {other:?}"),
        }
    }

    #[test]
    fn the_wheel_reports_a_direction_and_nothing_for_no_movement() {
        assert_eq!(wheel_button(-1.0), Some(MouseButton::WheelUp));
        assert_eq!(wheel_button(1.0), Some(MouseButton::WheelDown));
        assert_eq!(wheel_button(0.0), None);
    }
}

/// One physical secondary click acts once.
///
/// macOS can deliver a single Control-click twice: as a right press, and as
/// a left press with Control held. Both look like a secondary click, and
/// acting on both runs the gesture twice — which is not merely redundant,
/// it inverts: the first press copies a selection and lets go of it, so the
/// second sees no selection and *pastes*. One motion, and the clipboard
/// lands in whatever program is in the pane. With an agent there, that is
/// its prompt box.
///
/// A genuine second click always has a release between the presses. So:
/// after acting, refuse further secondary presses until one arrives.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SecondaryGesture {
    acted: bool,
}

impl SecondaryGesture {
    /// Whether this secondary press should act. Records that it did.
    pub fn take(&mut self) -> bool {
        if self.acted {
            return false;
        }
        self.acted = true;
        true
    }

    /// A button came up: the gesture is over, whichever button it was.
    pub fn released(&mut self) {
        self.acted = false;
    }
}

/// Whether a secondary press pastes.
///
/// Always, now. It used to copy when something was selected and paste only
/// when nothing was -- two useful things in one motion, and defensible until
/// you watch somebody use it. Selecting already puts the text on the
/// clipboard the moment the button comes up, so the copying half was doing
/// work that had just been done, and its cost was the other half: a press
/// aimed at pasting landed on a selection nobody had cleared and copied
/// instead. On macOS especially, where a right press *is* the paste gesture
/// in muscle memory, that reads as right-click paste being broken.
///
/// The argument stays and is ignored on purpose: it is the record that this
/// once turned on the selection, so the next person to wonder finds the
/// answer here rather than reinventing the condition.
pub fn secondary_press_pastes(_has_selection: bool) -> bool {
    true
}

/// Whether a left press is really the platform's secondary click.
///
/// AppKit normally translates a Control-click into a right press before an
/// app sees it, but some pointing-device drivers deliver it as Control plus
/// a left press; macOS users mean the same thing by both. Exact match only:
/// Ctrl combined with further modifiers is somebody's modified left click --
/// a shifted drag, an Alt block selection -- and must stay one. On the other
/// platforms a Ctrl+Left click is just a Ctrl+Left click.
pub fn ctrl_left_is_secondary(held: Held) -> bool {
    cfg!(target_os = "macos") && held.ctrl && !held.shift && !held.alt
}

/// Whether a secondary press runs the copy/paste gesture, or belongs to the
/// program.
///
/// Always the gesture. The product's right-click rule is one motion with no
/// exceptions: selection copies, no selection pastes, on every platform in
/// every pane. Deferring to a mouse-reporting TUI here read as "right-click
/// paste stopped working" the moment an agent was in front — which is most
/// of the time, and the agents do nothing with the click anyway.
pub fn secondary_click_acts(_reporting: bool, _has_selection: bool) -> bool {
    true
}

#[cfg(test)]
mod secondary_click_tests {
    use super::*;

    /// Ctrl alone is the macOS secondary click; Ctrl with anything else is
    /// somebody's modified left click and must stay one.
    #[cfg(target_os = "macos")]
    #[test]
    fn ctrl_left_is_secondary_only_with_exactly_ctrl() {
        assert!(ctrl_left_is_secondary(Held {
            ctrl: true,
            ..Default::default()
        }));
        assert!(!ctrl_left_is_secondary(Held {
            ctrl: true,
            shift: true,
            ..Default::default()
        }));
        assert!(!ctrl_left_is_secondary(Held {
            ctrl: true,
            alt: true,
            ..Default::default()
        }));
        assert!(!ctrl_left_is_secondary(Held::default()));
    }

    #[test]
    fn the_gesture_runs_in_every_pane_reporting_or_not() {
        assert!(secondary_click_acts(false, false));
        assert!(secondary_click_acts(true, false));
        assert!(secondary_click_acts(true, true));
    }
}

#[cfg(test)]
mod right_click_tests {
    use super::*;

    /// The rule, and the whole of it.
    #[test]
    fn a_secondary_press_pastes() {
        assert!(secondary_press_pastes(false));
    }

    /// Including when something is selected. Selecting already copied it --
    /// that happens when the button comes up -- so a press that copied again
    /// spent the gesture on work just done, and the paste it was aimed at
    /// never happened.
    #[test]
    fn a_selection_does_not_turn_the_press_back_into_a_copy() {
        assert!(secondary_press_pastes(true));
    }
}

#[cfg(test)]
mod secondary_gesture_tests {
    use super::*;

    #[test]
    fn one_physical_click_acts_once_even_when_delivered_twice() {
        // The macOS Control-click case: a right press and a ctrl+left press
        // for the same motion. Acting on both copies, then pastes — and the
        // paste lands in whatever is running in the pane.
        let mut gesture = SecondaryGesture::default();
        assert!(gesture.take(), "the first delivery must act");
        assert!(!gesture.take(), "the duplicate must not");
    }

    #[test]
    fn a_release_starts_a_new_gesture() {
        // A genuine second click always has a release between the presses,
        // which is what tells the two cases apart without guessing at timing.
        let mut gesture = SecondaryGesture::default();
        assert!(gesture.take());
        gesture.released();
        assert!(gesture.take(), "a second click must still work");
    }

    #[test]
    fn releases_without_a_press_change_nothing() {
        let mut gesture = SecondaryGesture::default();
        gesture.released();
        gesture.released();
        assert!(gesture.take());
    }
}
