//! Minimise, maximise and close, drawn into the top bar.
//!
//! Two styles, because the two desktops that need us to draw them have
//! different ideas of what the buttons are:
//!
//! - **Fluent** (Windows): full-height 46px backplates with a 10px glyph at
//!   100% scaling -- the geometry of Segoe Fluent Icons' E921/E922/E923/E8BB,
//!   which is what Windows Terminal and Warp draw. The fills are Fluent's own
//!   tokens: a 6% white wash on hover, a fainter one while pressed, and the
//!   system's close red, #C42B1C.
//! - **Adwaita** (Linux): round 24px buttons on a neutral fill, 8px glyphs,
//!   close no redder than the others -- GNOME's header-bar buttons. The
//!   Windows backplates on a GNOME desktop read as a Windows app.
//!
//! macOS draws its own traffic lights and never comes here.
//!
//! Glyphs are stroked rather than typed: the codepoints of Segoe Fluent Icons
//! collide with the Nerd Font symbols face in the private-use area, and a
//! stroked cross is the same cross on every machine.

use unterm_render::quads::Quad;
use unterm_render::strokes;

/// Which button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Minimise,
    Maximise,
    Restore,
    Close,
}

/// Which desktop's buttons to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Fluent,
    Adwaita,
}

/// The style for this platform.
pub fn style() -> Style {
    if cfg!(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd", target_os = "openbsd")) {
        Style::Adwaita
    } else {
        Style::Fluent
    }
}

/// How wide one button's slot is, in logical pixels.
pub fn slot_width(style: Style) -> f32 {
    match style {
        // Windows' own caption width: a close button narrower than the
        // system's misses the corner muscle memory aims at.
        Style::Fluent => 46.0,
        // A 24px circle with 5px either side, as GNOME spaces them.
        Style::Adwaita => 34.0,
    }
}

/// Windows' own close-button red (Fluent's `SystemFillColorCritical` on a
/// caption). Not derived from the theme: it means "this closes the window".
pub const CLOSE_HOVER: [f32; 4] = [0.769, 0.169, 0.110, 1.0];

/// How a button looks at one instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct State {
    /// 0 at rest, 1 fully hovered; in between while the hover fades.
    pub hover: f32,
    pub pressed: bool,
    /// The window is in front. A caption in a background window is dimmed,
    /// as every Windows 11 title bar is.
    pub active: bool,
}

impl State {
    pub const REST: State = State { hover: 0.0, pressed: false, active: true };
}

/// The fill behind a button: the backplate on Windows, the circle on Linux.
pub fn fill(style: Style, button: Button, state: State, bar_is_light: bool) -> [f32; 4] {
    let ink = if bar_is_light { [0.0, 0.0, 0.0] } else { [1.0, 1.0, 1.0] };
    match style {
        Style::Fluent => {
            if button == Button::Close {
                let alpha = if state.pressed { 0.9 } else { state.hover };
                return [CLOSE_HOVER[0], CLOSE_HOVER[1], CLOSE_HOVER[2], alpha];
            }
            // SubtleFillColorSecondary on hover, Tertiary while pressed.
            let (hover, pressed) = if bar_is_light { (0.035, 0.024) } else { (0.059, 0.039) };
            let alpha = if state.pressed { pressed } else { hover * state.hover };
            [ink[0], ink[1], ink[2], alpha]
        }
        Style::Adwaita => {
            let alpha = if state.pressed {
                0.20
            } else {
                0.10 + 0.05 * state.hover
            };
            let alpha = if state.active { alpha } else { alpha * 0.6 };
            [ink[0], ink[1], ink[2], alpha]
        }
    }
}

/// The glyph's colour.
pub fn glyph_color(style: Style, button: Button, state: State, bar_is_light: bool) -> [f32; 4] {
    if style == Style::Fluent && button == Button::Close && (state.hover > 0.5 || state.pressed) {
        // White on the red, in both themes; a little muted while pressed.
        return [1.0, 1.0, 1.0, if state.pressed { 0.7 } else { 1.0 }];
    }
    let base = if bar_is_light { [0.0, 0.0, 0.0, 0.894] } else { [1.0, 1.0, 1.0, 1.0] };
    if !state.active {
        // TextFillColorDisabled.
        return [base[0], base[1], base[2], if bar_is_light { 0.361 } else { 0.365 }];
    }
    base
}

/// Draw `button`'s glyph centred in the slot at `left`, `top`. `scale` is the
/// window's DPI scale, so the glyph is 10px at 100% and 15px at 150%.
pub fn glyph(
    style: Style,
    button: Button,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    scale: f32,
    color: [f32; 4],
) -> Vec<Quad> {
    let nominal = match style {
        Style::Fluent => 10.0,
        Style::Adwaita => 8.0,
    };
    let size = (nominal * scale).round().clamp(4.0, height.max(4.0));
    let weight = match style {
        Style::Fluent => scale.round().max(1.0),
        // Adwaita's symbolic icons are drawn a little heavier.
        Style::Adwaita => (1.4 * scale).round().max(1.0),
    };
    let origin = ((left + (width - size) / 2.0).round(), (top + (height - size) / 2.0).round());
    let at = |x: f32, y: f32| (origin.0 + size * x, origin.1 + size * y);

    match button {
        Button::Close => {
            let mut quads = strokes::line(at(0.0, 0.0), at(1.0, 1.0), weight, color);
            quads.extend(strokes::line(at(1.0, 0.0), at(0.0, 1.0), weight, color));
            quads
        }
        // Fluent's rule sits on the glyph's middle; GNOME's low, like the
        // dash of a minimised window.
        Button::Minimise => {
            let y = match style {
                Style::Fluent => 0.5,
                Style::Adwaita => 0.8,
            };
            strokes::line(at(0.0, y), at(1.0, y), weight, color)
        }
        Button::Maximise => strokes::rectangle(origin.0, origin.1, size, size, weight, color),
        // The front square is where the window goes back to; the corner
        // behind it is where it is now.
        Button::Restore => {
            let offset = (size * 0.2).round().max(2.0);
            let front = size - offset;
            let mut quads = strokes::polyline(
                &[
                    (origin.0 + offset, origin.1),
                    (origin.0 + size, origin.1),
                    (origin.0 + size, origin.1 + front),
                ],
                weight,
                color,
            );
            quads.extend(strokes::rectangle(origin.0, origin.1 + offset, front, front, weight, color));
            quads
        }
    }
}

/// Where the round Adwaita button sits inside its slot.
pub fn adwaita_circle(left: f32, top: f32, width: f32, height: f32, scale: f32) -> (f32, f32, f32) {
    let diameter = (24.0 * scale).round().min(height).min(width);
    (
        (left + (width - diameter) / 2.0).round(),
        (top + (height - diameter) / 2.0).round(),
        diameter,
    )
}

/// How far into a hover fade `elapsed` is, with Fluent's decelerate curve.
/// 150ms for the backplate, as Windows Terminal times it.
pub fn fade(elapsed: std::time::Duration) -> f32 {
    if crate::ui_tokens::reduce_motion() {
        return 1.0;
    }
    let t = (elapsed.as_secs_f32() / 0.150).clamp(0.0, 1.0);
    // cubic-bezier(0, 0, 0, 1), close enough: ease-out cubic.
    1.0 - (1.0 - t).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawn(style: Style, button: Button) -> Vec<Quad> {
        glyph(style, button, 0.0, 0.0, 46.0, 32.0, 1.0, [1.0; 4])
    }

    fn bounds(quads: &[Quad]) -> (f32, f32, f32, f32) {
        let left = quads.iter().map(|q| q.left).fold(f32::MAX, f32::min);
        let top = quads.iter().map(|q| q.top).fold(f32::MAX, f32::min);
        let right = quads.iter().map(|q| q.left + q.width).fold(f32::MIN, f32::max);
        let bottom = quads.iter().map(|q| q.top + q.height).fold(f32::MIN, f32::max);
        (left, top, right, bottom)
    }

    const ALL: [Button; 4] = [Button::Minimise, Button::Maximise, Button::Restore, Button::Close];

    #[test]
    fn every_button_draws_something_in_both_styles() {
        for style in [Style::Fluent, Style::Adwaita] {
            for button in ALL {
                assert!(!drawn(style, button).is_empty(), "{style:?} {button:?} drew nothing");
            }
        }
    }

    /// The Fluent glyph is Segoe Fluent Icons' size: 10px at 100%, 15 at 150%.
    #[test]
    fn a_fluent_glyph_is_ten_pixels_at_one_hundred_percent() {
        for (scale, want) in [(1.0, 10.0), (1.5, 15.0), (2.0, 20.0)] {
            let quads = glyph(Style::Fluent, Button::Maximise, 0.0, 0.0, 69.0, 48.0, scale, [1.0; 4]);
            let (left, top, right, bottom) = bounds(&quads);
            assert!((right - left - want).abs() <= scale.round() + 0.5, "{scale}: {}", right - left);
            assert!((bottom - top - want).abs() <= scale.round() + 0.5, "{scale}: {}", bottom - top);
        }
    }

    /// Centred in the slot, and inside it.
    #[test]
    fn a_glyph_is_centred_and_stays_in_its_slot() {
        for style in [Style::Fluent, Style::Adwaita] {
            for button in ALL {
                let quads = drawn(style, button);
                let (left, top, right, bottom) = bounds(&quads);
                assert!((left - (46.0 - right)).abs() <= 1.5, "{style:?} {button:?} off-centre across");
                assert!(left >= 0.0 && right <= 46.0 && top >= 0.0 && bottom <= 32.0, "{style:?} {button:?} escapes");
            }
        }
    }

    /// Windows 11's minimise dash is on the middle of the glyph, not low.
    #[test]
    fn the_fluent_minimise_rule_is_on_the_middle() {
        let (_, top, _, bottom) = bounds(&drawn(Style::Fluent, Button::Minimise));
        let middle = (top + bottom) / 2.0;
        assert!((middle - 16.0).abs() <= 1.0, "the rule is at {middle}");
    }

    /// Only Windows' close turns red, and its cross goes white on it.
    #[test]
    fn only_the_fluent_close_turns_red() {
        let hovered = State { hover: 1.0, pressed: false, active: true };
        assert_eq!(fill(Style::Fluent, Button::Close, hovered, false)[..3], CLOSE_HOVER[..3]);
        assert_eq!(glyph_color(Style::Fluent, Button::Close, hovered, true), [1.0, 1.0, 1.0, 1.0]);
        for button in [Button::Minimise, Button::Maximise] {
            let f = fill(Style::Fluent, button, hovered, false);
            assert!(f[3] < 0.1, "{button:?} should be a faint wash: {f:?}");
        }
        let gnome = fill(Style::Adwaita, Button::Close, hovered, false);
        assert_eq!(gnome[..3], [1.0, 1.0, 1.0], "GNOME's close is not red");
    }

    /// The cross on the red is white in a light theme and a dark one, dims a
    /// little while held, and goes back to the theme's colour once the
    /// pointer has left.
    #[test]
    fn the_close_cross_is_white_on_its_red() {
        for light in [false, true] {
            let hovered = State { hover: 1.0, pressed: false, active: true };
            assert_eq!(glyph_color(Style::Fluent, Button::Close, hovered, light), [1.0, 1.0, 1.0, 1.0]);
            let held = State { hover: 1.0, pressed: true, active: true };
            let color = glyph_color(Style::Fluent, Button::Close, held, light);
            assert_eq!(color[..3], [1.0, 1.0, 1.0]);
            assert!(color[3] < 1.0);
        }
        assert_eq!(glyph_color(Style::Fluent, Button::Close, State::REST, true)[..3], [0.0, 0.0, 0.0]);
    }

    /// A button at rest has no backplate on Windows; a fade runs 0 -> 1.
    #[test]
    fn a_fluent_button_at_rest_has_no_backplate_and_the_fade_runs_to_full() {
        assert_eq!(fill(Style::Fluent, Button::Minimise, State::REST, false)[3], 0.0);
        assert_eq!(fill(Style::Fluent, Button::Close, State::REST, false)[3], 0.0);
        assert_eq!(fade(std::time::Duration::from_millis(400)), 1.0);
        // With the system's reduce-motion on, every fade is already over.
        if !crate::ui_tokens::reduce_motion() {
            assert_eq!(fade(std::time::Duration::ZERO), 0.0);
            assert!(fade(std::time::Duration::from_millis(75)) > 0.5);
        }
    }

    /// A background window's caption is dimmed.
    #[test]
    fn an_inactive_window_dims_its_glyphs() {
        let inactive = State { hover: 0.0, pressed: false, active: false };
        assert!(glyph_color(Style::Fluent, Button::Minimise, inactive, false)[3] < 0.4);
        assert_eq!(glyph_color(Style::Fluent, Button::Minimise, State::REST, false)[3], 1.0);
    }

    #[test]
    fn the_adwaita_circle_is_centred_and_fits() {
        let (x, y, d) = adwaita_circle(10.0, 0.0, 34.0, 32.0, 1.0);
        assert_eq!(d, 24.0);
        assert_eq!((x, y), (15.0, 4.0));
        let (_, _, small) = adwaita_circle(0.0, 0.0, 34.0, 20.0, 1.0);
        assert_eq!(small, 20.0, "never taller than the bar");
    }
}
