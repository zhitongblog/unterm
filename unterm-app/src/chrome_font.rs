//! The face the window's chrome is drawn in.
//!
//! Not the terminal's. The previous front end drew every tab, sidebar row and
//! status segment in a UI face at 13pt with a looser line height, sized in
//! points against the display rather than in terminal cells -- and that is most
//! of why this window read as a wall of output surrounded by more of the same.
//!
//! Two things follow from it being separate. It does not grow when somebody
//! zooms the terminal, because zooming the terminal means zooming the terminal.
//! And it files its glyphs in the atlas under its own stack, because a face
//! index means nothing without saying whose stack it indexes.

use crate::terminal::{Shape, TerminalFont};

/// Which stack the chrome's glyphs are filed under.
pub const STACK: u8 = 1;

/// The families to try for chrome text, in order.
///
/// A proportional UI face first, because chrome is prose rather than a grid:
/// the old front end used the platform's title font. A monospace face is the
/// fallback rather than the choice -- but it is a fallback that always exists,
/// so the chrome never fails to open.
const FAMILIES: &[&str] = &[
    // Windows
    "Segoe UI Variable Text",
    "Segoe UI",
    // macOS
    "SF Pro Text",
    "SF Pro",
    ".AppleSystemUIFont",
    "Helvetica Neue",
    "PingFang SC",
    // Linux
    "Inter",
    "Cantarell",
    "DejaVu Sans",
    "Noto Sans",
];

const CJK_FALLBACKS: &[&str] = &[
    "PingFang SC",
    "Microsoft YaHei UI",
    "Noto Sans CJK SC",
];

/// Open the chrome face at the display's scale.
pub fn open(fallbacks: &[String], scale: f32) -> anyhow::Result<TerminalFont> {
    let pixels = crate::terminal::pixels_for_points(crate::ui_tokens::UI_FONT_SIZE as f32, scale);
    let index = unterm_engine::next_core::font_discovery::FontIndex::cached();
    let family = FAMILIES
        .iter()
        .find(|family| !index.family(family).is_empty())
        .copied();

    let mut merged_fallbacks: Vec<String> = fallbacks.to_vec();
    for cjk in CJK_FALLBACKS {
        if !merged_fallbacks.iter().any(|existing| existing.eq_ignore_ascii_case(cjk)) {
            merged_fallbacks.push((*cjk).to_string());
        }
    }

    // The chrome's own line height, which is looser than a terminal's: rows
    // that breathe are the difference between a sidebar and a list of output.
    let shape = Shape {
        line_height: crate::ui_tokens::UI_LINE_HEIGHT as f32,
        cell_width: 1.0,
    };
    Ok(TerminalFont::open_named(family, pixels, &merged_fallbacks, shape)?.as_stack(STACK))
}

/// How many pixels one point is at this scale, for the chrome's own geometry.
///
/// Every token in `ui_tokens` is in points; this is what turns one into a
/// distance on this display.
pub fn point(scale: f32) -> f32 {
    // macOS points are logical pixels, as the platform and 0.57.4 both had
    // it; elsewhere a point is the 96dpi kind the same tokens mean on
    // Windows.
    if cfg!(target_os = "macos") {
        scale.max(0.1).max(0.5)
    } else {
        (scale.max(0.1) * 96.0 / 72.0).max(0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chrome opens on whatever this machine has. A window that will not
    /// draw its own tabs is not a window.
    #[test]
    fn the_chrome_face_opens() {
        let font = open(&[], 1.0).expect("the chrome has to have a face");
        assert!(font.metrics().height > 0.0);
        assert_eq!(font.stack_id(), STACK);
    }

    /// It is filed under its own stack, or its glyphs and the terminal's
    /// overwrite each other in the atlas whenever the two land on one size.
    #[test]
    fn the_chrome_face_is_a_different_stack_from_the_terminal() {
        let chrome = open(&[], 1.0).expect("a chrome face");
        assert_ne!(
            chrome.stack_id(),
            0,
            "the chrome shares the terminal's stack"
        );
    }

    /// Its rows are looser than a terminal's. Chrome at the terminal's rhythm
    /// reads as output rather than as chrome.
    #[test]
    fn chrome_rows_are_looser_than_terminal_rows() {
        let chrome = open(&[], 1.0).expect("a chrome face");
        let ratio = chrome.metrics().height / chrome.metrics().baseline.max(1.0);
        assert!(
            ratio > 1.0,
            "the chrome's rows are not loose at all: {ratio}"
        );
    }

    /// A point is a point at 1x and grows with the display, because the tokens
    /// are in points and a token that meant device pixels would be a third
    /// smaller on this machine than it was written to be.
    #[test]
    fn a_point_grows_with_the_display() {
        let one = point(1.0);
        let one_and_a_half = point(1.5);
        // On macOS a point IS the logical pixel; elsewhere it is a third
        // bigger, the 96dpi kind.
        assert!(one >= 1.0, "a point is at least a pixel: {one}");
        assert!(
            (one_and_a_half / one - 1.5).abs() < 0.01,
            "{one} then {one_and_a_half}"
        );
    }

    /// Every mark the chrome draws has to exist somewhere in the stack.
    ///
    /// A missing one is not a box here -- it is nothing at all, because the
    /// fallback face has nothing to draw either. Which reads as a layout bug
    /// rather than a font one, and is how `▶` went missing from the top bar
    /// without anything looking broken.
    #[test]
    fn the_marks_the_chrome_draws_are_all_reachable() {
        let Some(stack) = crate::fonts::FontStack::system(16) else {
            return;
        };
        let mut missing = Vec::new();
        for (name, ch) in [
            ("play", '\u{25B6}'),
            ("chevron down", '\u{25BE}'),
            ("chevron right", '\u{25B8}'),
            ("tick", '\u{2713}'),
            ("bolt", '\u{26A1}'),
            ("ellipsis", '\u{2026}'),
            ("branch", '\u{E0A0}'),
            ("folder", '\u{F07B}'),
            // The inbox sidebar's status vocabulary. Each row says its
            // state with exactly one of these.
            ("raised hand", '\u{270B}'),
            ("warning triangle", '\u{25B2}'),
            ("dot", '\u{2022}'),
            ("plus", '+'),
            ("spinner q1", '\u{25D0}'),
            ("spinner q2", '\u{25D3}'),
            ("spinner q3", '\u{25D1}'),
            ("spinner q4", '\u{25D2}'),
            // One mark per agent, so a row says who is in it before
            // its text is read.
            ("claude asterisk", '\u{273B}'),
            ("codex hexagon", '\u{F02D8}'),
            ("gemini sparkle", '\u{2726}'),
            ("aider pencil", '\u{270E}'),
            ("opencode diamond", '\u{25C7}'),
            ("kimi moon", '\u{263E}'),
            ("cursor triangle", '\u{27A4}'),
        ] {
            if !stack.covers(ch) {
                missing.push(format!("{name} ({ch:?})"));
            }
        }
        assert!(missing.is_empty(), "no face carries: {missing:?}");
    }

    #[test]
    fn a_nonsense_scale_still_gives_a_usable_point() {
        for scale in [0.0, -1.0, f32::NAN] {
            assert!(point(scale) >= 0.5, "scale {scale} gave {}", point(scale));
        }
    }
}

#[cfg(test)]
mod raster_tests {
    use super::*;

    /// A mark the stack claims to have must also rasterise to actual pixels.
    ///
    /// These are two different questions and the gap between them is invisible:
    /// a face can report a glyph and produce an empty bitmap -- a colour-bitmap
    /// emoji face does exactly that under a monochrome render -- and the mark
    /// then advances the pen and draws nothing. Which looks like a spacing bug,
    /// not a font one, and is how the play mark vanished from the top bar while
    /// leaving its gap behind.
    #[test]
    fn every_chrome_mark_rasterises_to_something() {
        // A hosted CI runner has no desktop font install to probe -- what
        // this asserts is the machine, and the machines it speaks for are
        // the ones people run the product on.
        if std::env::var_os("GITHUB_ACTIONS").is_some() {
            return;
        }
        let Ok(mut font) = open(&[], 1.0) else {
            return;
        };
        let mut empty = Vec::new();
        for (name, ch) in [
            ("play", '\u{25B6}'),
            ("chevron down", '\u{25BE}'),
            ("chevron right", '\u{25B8}'),
            ("tick", '\u{2713}'),
            ("bolt", '\u{26A1}'),
            ("ellipsis", '\u{2026}'),
            ("branch", '\u{E0A0}'),
            ("folder", '\u{F07B}'),
            ("terminal", '\u{EA85}'),
            ("three bars", '\u{EB6A}'),
            ("gear", '\u{EB51}'),
            ("raised hand", '\u{270B}'),
            ("warning triangle", '\u{25B2}'),
            ("dot", '\u{2022}'),
            ("plus", '+'),
            ("spinner q1", '\u{25D0}'),
            ("spinner q2", '\u{25D3}'),
            ("spinner q3", '\u{25D1}'),
            ("spinner q4", '\u{25D2}'),
            // One mark per agent, so a row says who is in it before
            // its text is read.
            ("claude asterisk", '\u{273B}'),
            ("codex hexagon", '\u{F02D8}'),
            ("gemini sparkle", '\u{2726}'),
            ("aider pencil", '\u{270E}'),
            ("opencode diamond", '\u{25C7}'),
            ("kimi moon", '\u{263E}'),
            ("cursor triangle", '\u{27A4}'),
        ] {
            let drawn = font
                .stack_mut()
                .rasterize(ch)
                .map(|(_, glyph)| glyph.width > 0 && glyph.height > 0)
                .unwrap_or(false);
            if !drawn {
                empty.push(format!("{name} ({ch:?})"));
            }
        }
        assert!(empty.is_empty(), "these draw nothing: {empty:?}");
    }
}
