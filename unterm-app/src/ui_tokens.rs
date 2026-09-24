//! One scale for every piece of window chrome.
//!
//! Ported unchanged from the previous front end, including the values and the
//! reasons written beside them. The point of a token file is that new chrome
//! picks up the same rhythm instead of growing its own magic numbers -- and the
//! reason to bring it across verbatim rather than re-pick is that every number
//! here was chosen against a running window.
//!
//! Values are in points. Multiply by `dpi / 72` at the place that draws.
//!
//! Kept whole even where nothing reads a token yet: the scale is the point, and
//! deleting the parts not currently used would leave the next piece of chrome
//! picking its own numbers -- which is the habit this file exists to break.
#![allow(dead_code)]

/// Chrome text: tabs, sidebar rows, status bar. Keep this close to the
/// terminal font while using the title/UI font's natural weight; oversizing
/// chrome makes the app read less precise than Warp. The shipped 0.57.4
/// configuration set `window_frame.font_size = 12.0`, so 12pt is what every
/// released window actually drew its chrome at — match it.
pub const UI_FONT_SIZE: f64 = 12.0;
/// Command palette / modal body text.
pub const PALETTE_FONT_SIZE: f64 = 14.0;
/// Small overline / badge text.
pub const OVERLINE_FONT_SIZE: f64 = 10.0;
/// Modal / section header text.
pub const HEADER_FONT_SIZE: f64 = 18.0;
/// Line-height ratio for chrome text. A touch loose (vs 1.2) so sidebar/tab
/// rows breathe instead of reading cramped.
pub const UI_LINE_HEIGHT: f64 = 1.30;
/// Corner radius for selectable rows and buttons: 3pt is Fluent's 4px
/// control radius at 96dpi.
pub const CORNER_RADIUS: f32 = 3.0;
/// Padding inside selectable rows. Generous so sidebar/tab rows don't read
/// cramped, especially at the slightly larger UI font size.
pub const ROW_PADDING: f32 = 8.0;
/// Horizontal inset shared by docked chrome panels.
pub const CHROME_PANEL_INSET: f32 = 6.0;
/// Top breathing room before the first sidebar section.
pub const CHROME_SECTION_GAP: f32 = 10.0;
/// Vertical padding for primary sidebar rows.
pub const CHROME_ROW_PADDING_Y: f32 = 7.0;
/// Vertical padding for compact file/git rows.
pub const CHROME_COMPACT_ROW_PADDING_Y: f32 = 4.5;
/// Vertical padding for sidebar section headers.
pub const CHROME_HEADER_PADDING_Y: f32 = 7.0;
/// Width reserved for the macOS traffic-light cluster in the tab bar.
pub const MACOS_TRAFFIC_LIGHT_RESERVE: f32 = 76.0;
/// Diameter of the custom-drawn macOS traffic-light dots.
pub const MACOS_TRAFFIC_LIGHT_DOT: f32 = 12.0;
/// Extra vertical breathing room around the bottom status-bar text.
pub const STATUS_BAR_VERTICAL_PADDING: f32 = 2.5;
/// Visual baseline compensation for one-line chrome text. Terminal cells
/// include descender space, so geometric centering reads slightly low.
pub const CHROME_TEXT_BASELINE_NUDGE: f32 = -2.0;
/// The top bar centres its text on the native traffic lights rather than on
/// its own geometry: AppKit parks the lights a couple of pixels below the
/// bar's midline, and text centred geometrically reads high beside them.
/// Measured against the lights' ink at 1x — lights centre ~15.5 in a 28px
/// bar; the general chrome nudge put our text at ~11.
pub const TOPBAR_TEXT_NUDGE: f32 = 2.0;
/// Motion, in milliseconds: Fluent's fast, normal and slow durations.
pub const MOTION_FAST_MS: u64 = 83;
pub const MOTION_NORMAL_MS: u64 = 167;
pub const MOTION_SLOW_MS: u64 = 250;
/// Whether the system asks for less motion.
///
/// Read once: it is a setting people change rarely, and asking the OS on
/// every animated frame would cost more than the animation. Windows' "Show
/// animations in Windows" and macOS's "Reduce motion" both answer it.
pub fn reduce_motion() -> bool {
    static ANSWER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ANSWER.get_or_init(system_reduces_motion)
}

#[cfg(windows)]
fn system_reduces_motion() -> bool {
    use winapi::um::winuser::{SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION};
    let mut enabled: winapi::shared::minwindef::BOOL = 1;
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            &mut enabled as *mut _ as *mut std::ffi::c_void,
            0,
        )
    };
    ok != 0 && enabled == 0
}

#[cfg(target_os = "macos")]
fn system_reduces_motion() -> bool {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            return false;
        }
        let reduce: bool = msg_send![workspace, accessibilityDisplayShouldReduceMotion];
        reduce
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn system_reduces_motion() -> bool {
    false
}

/// A list row is two chrome lines tall: the title, then its context.
pub const SIDEBAR_ROW_LINES: f32 = 1.75;
/// Width of chrome scrollbars; rendered with a minimum physical width.
pub const CHROME_SCROLLBAR_WIDTH: f32 = 5.0;
/// Minimum physical scrollbar width, to keep HiDPI and low-DPI output aligned.
pub const CHROME_SCROLLBAR_MIN_WIDTH: f32 = 6.0;
/// Minimum scrollbar thumb height.
pub const CHROME_SCROLLBAR_MIN_THUMB_HEIGHT: f32 = 28.0;
/// Faint track opacity behind chrome/pane scrollbars.
pub const CHROME_SCROLLBAR_TRACK_ALPHA: f32 = 0.16;
/// Thumb opacity for theme-provided scrollbar colors.
pub const CHROME_SCROLLBAR_THUMB_ALPHA: f32 = 0.74;

/// Left tab bar geometry, in points: 186pt is 248px at 96dpi, Warp's
/// vertical-tab width, and 150pt its 200px minimum. Room for a task on the
/// first line and where-and-which-branch on the second.
pub const LEFT_TAB_BAR_WIDTH: f32 = 186.0;
pub const LEFT_TAB_BAR_MIN_WIDTH: f32 = 150.0;
/// Max width as a fraction of the window width.
pub const LEFT_TAB_BAR_MAX_RATIO: f32 = 0.30;
/// Width of the resize grip on the bar's right edge.
pub const LEFT_TAB_BAR_GRIP: f32 = 12.0;

/// Directory tree sidebar geometry.
pub const TREE_SIDEBAR_WIDTH: f32 = 164.0;
pub const TREE_SIDEBAR_MIN_WIDTH: f32 = 120.0;
pub const TREE_SIDEBAR_MAX_RATIO: f32 = 0.30;
pub const TREE_SIDEBAR_GRIP: f32 = 12.0;
/// Combined left chrome should never dominate the terminal area.
pub const LEFT_GUTTER_MAX_RATIO: f32 = 0.42;

/// Right-docked source-control (git) panel geometry.
pub const GIT_PANEL_WIDTH: f32 = 232.0;
pub const GIT_PANEL_MIN_WIDTH: f32 = 160.0;
/// Max width as a fraction of the window width.
pub const GIT_PANEL_MAX_RATIO: f32 = 0.32;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_density_scale_is_ordered() {
        assert!(CHROME_COMPACT_ROW_PADDING_Y < CHROME_ROW_PADDING_Y);
        assert!(CHROME_ROW_PADDING_Y <= CHROME_HEADER_PADDING_Y);
        assert!(ROW_PADDING >= CHROME_ROW_PADDING_Y);
    }

    #[test]
    fn sidebar_defaults_respect_minimums_and_window_budget() {
        assert!(LEFT_TAB_BAR_WIDTH >= LEFT_TAB_BAR_MIN_WIDTH);
        assert!(TREE_SIDEBAR_WIDTH >= TREE_SIDEBAR_MIN_WIDTH);
        assert!(LEFT_TAB_BAR_MAX_RATIO < LEFT_GUTTER_MAX_RATIO);
        assert!(TREE_SIDEBAR_MAX_RATIO < LEFT_GUTTER_MAX_RATIO);
        assert!(LEFT_GUTTER_MAX_RATIO < 0.5);
    }
}
