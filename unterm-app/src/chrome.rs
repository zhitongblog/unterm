//! The colours the window's frame is built from.
//!
//! Ported from the previous front end's `chrome_colors.rs`, coefficients and
//! all, so the top bar, sidebar and status bar look the way they did rather
//! than the way whoever rebuilt them guessed. The point of it is that the
//! chrome should *frame* the terminal rather than become a large grey slab
//! around it, which is why the amounts are so small.
//!
//! Mixing happens in linear light, as it did before. Blending sRGB values
//! directly is the usual way a carefully chosen frame turns muddy: the same
//! numeric amount lands in a different place, and every tone here was picked
//! against the linear result.
//!
//! The contrast tests came across too. They are the reason the values can be
//! adjusted at all: every bundled theme has to keep its chrome text above
//! WCAG AA and its focus rail visible, and the tests say so rather than
//! leaving it to the eye.

/// The tones one surface of the frame is drawn from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chrome {
    pub surface: [f32; 4],
    /// Two edge tones, so the shared frame reads as deliberate rather than as
    /// one hard separator line.
    pub outer_edge: [f32; 4],
    pub inner_highlight: [f32; 4],
    pub selected_outline: [f32; 4],
    /// Secondary labels: quieter, but never below AA once composited.
    pub dim_text: [f32; 4],
    pub hover_bg: [f32; 4],
    pub selected_bg: [f32; 4],
    pub group_bg: [f32; 4],
    pub footer_bg: [f32; 4],
    pub focus_rail: [f32; 4],
    pub is_light: bool,
}

/// Derive the frame's tones from the terminal's own colours.
///
/// From the terminal's background rather than a fixed grey, which is what
/// makes the window read as one surface in any theme -- and what the old
/// design document called out as the bug when it did not.
///
/// Not yet drawn: the bars that use it are being ported one at a time, and
/// this is the piece they all stand on.
#[allow(dead_code)]
pub fn chrome(background: [f32; 4], foreground: [f32; 4]) -> Chrome {
    let is_light = is_light_surface(background);

    // Linear-light mixing is visually stronger than the numeric amount
    // suggests, so dark schemes need only a very small lift.
    let surface = mix(background, foreground, if is_light { 0.030 } else { 0.020 });
    let hover_bg = if is_light {
        mix(background, foreground, 0.062)
    } else {
        mix(surface, foreground, 0.032)
    };
    let selected_bg = if is_light {
        mix(background, foreground, 0.092)
    } else {
        mix(surface, foreground, 0.060)
    };

    Chrome {
        surface,
        // The seams: under the top bar, down the strip's trailing edge.
        // At 9% of a dim foreground these were a line the code drew and
        // nobody saw -- the chrome then read as one undivided grey area
        // with the terminal cut out of it. Strong enough to find, still
        // far too quiet to be a border.
        outer_edge: with_alpha(foreground, if is_light { 0.20 } else { 0.16 }),
        inner_highlight: with_alpha(foreground, if is_light { 0.055 } else { 0.04 }),
        selected_outline: with_alpha(foreground, if is_light { 0.12 } else { 0.09 }),
        dim_text: with_alpha(foreground, if is_light { 0.72 } else { 0.74 }),
        hover_bg,
        selected_bg,
        group_bg: mix(surface, foreground, if is_light { 0.018 } else { 0.016 }),
        footer_bg: surface,
        // The waiting amber, in a darker optical variant on light
        // surfaces so the focus rail stays crisp instead of glowing.
        // One accent for the whole chrome, and it is the same colour
        // the status language uses for "needs you".
        focus_rail: if is_light {
            srgb(0x8a, 0x65, 0x19)
        } else {
            srgb(0xe8, 0xb3, 0x4b)
        },
        is_light,
    }
}

/// Whether a background counts as a light one.
#[allow(dead_code)]
pub fn is_light_surface(background: [f32; 4]) -> bool {
    luma(to_linear(background)) > 0.48
}

/// Blend two colours in linear light.
pub fn mix(a: [f32; 4], b: [f32; 4], amount: f32) -> [f32; 4] {
    let (a, b) = (to_linear(a), to_linear(b));
    to_srgb([
        a[0] * (1.0 - amount) + b[0] * amount,
        a[1] * (1.0 - amount) + b[1] * amount,
        a[2] * (1.0 - amount) + b[2] * amount,
        1.0,
    ])
}

/// A colour from sRGB bytes, as a theme names them.
pub const fn srgb(red: u8, green: u8, blue: u8) -> [f32; 4] {
    [
        red as f32 / 255.0,
        green as f32 / 255.0,
        blue as f32 / 255.0,
        1.0,
    ]
}

fn with_alpha(color: [f32; 4], alpha: f32) -> [f32; 4] {
    [color[0], color[1], color[2], alpha]
}

fn to_linear(color: [f32; 4]) -> [f32; 4] {
    let channel = |value: f32| {
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    [
        channel(color[0]),
        channel(color[1]),
        channel(color[2]),
        color[3],
    ]
}

fn to_srgb(color: [f32; 4]) -> [f32; 4] {
    let channel = |value: f32| {
        if value <= 0.003_130_8 {
            value * 12.92
        } else {
            1.055 * value.powf(1.0 / 2.4) - 0.055
        }
    };
    [
        channel(color[0]),
        channel(color[1]),
        channel(color[2]),
        color[3],
    ]
}

fn luma(linear: [f32; 4]) -> f32 {
    0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2]
}

#[cfg(test)]
mod tests {
    use super::*;

    const WCAG_AA_NORMAL_TEXT: f32 = 4.5;

    /// The six themes the product ships, as the previous front end's own
    /// contrast test listed them. Keeping the list here is what makes a new
    /// theme's chrome checkable before anyone looks at it.
    const BUNDLED: &[(&str, [f32; 4], [f32; 4])] = &[
        ("Standard", srgb(0x11, 0x13, 0x15), srgb(0xe8, 0xea, 0xed)),
        ("Midnight", srgb(0x0c, 0x12, 0x20), srgb(0xdf, 0xe7, 0xf1)),
        ("Daylight", srgb(0xf6, 0xf7, 0xf4), srgb(0x16, 0x1a, 0x1d)),
        ("Classic", srgb(0x12, 0x12, 0x12), srgb(0xee, 0xee, 0xee)),
        (
            "Notion Dark",
            srgb(0x18, 0x18, 0x18),
            srgb(0xee, 0xee, 0xec),
        ),
        (
            "Notion Light",
            srgb(0xf8, 0xf7, 0xf4),
            srgb(0x1f, 0x1e, 0x1a),
        ),
    ];

    fn light() -> ([f32; 4], [f32; 4]) {
        (srgb(0xf6, 0xf7, 0xf4), srgb(0x16, 0x1a, 0x1d))
    }

    fn dark() -> ([f32; 4], [f32; 4]) {
        (srgb(0x0c, 0x12, 0x20), srgb(0xdf, 0xe7, 0xf1))
    }

    #[test]
    fn light_chrome_states_step_down_from_the_surface() {
        let (background, foreground) = light();
        let chrome = chrome(background, foreground);
        assert!(chrome.is_light);
        assert!(brightness(chrome.hover_bg) < brightness(chrome.surface));
        assert!(brightness(chrome.selected_bg) < brightness(chrome.hover_bg));
    }

    #[test]
    fn dark_chrome_has_subtle_ordered_layers() {
        let (background, foreground) = dark();
        let chrome = chrome(background, foreground);
        assert!(!chrome.is_light);
        assert!(brightness(chrome.surface) < brightness(chrome.hover_bg));
        assert!(brightness(chrome.hover_bg) < brightness(chrome.selected_bg));
        assert!(brightness(chrome.group_bg) < brightness(chrome.hover_bg));
        assert!(brightness(chrome.footer_bg) < brightness(chrome.hover_bg));
    }

    #[test]
    fn chrome_text_meets_aa_in_light_and_dark() {
        for (background, foreground) in [light(), dark()] {
            let chrome = chrome(background, foreground);
            assert!(
                contrast(foreground, chrome.selected_bg) >= WCAG_AA_NORMAL_TEXT,
                "a selected label must stay readable"
            );
            assert!(
                contrast(composite(chrome.dim_text, chrome.surface), chrome.surface)
                    >= WCAG_AA_NORMAL_TEXT,
                "a secondary label must stay readable"
            );
        }
    }

    /// Every bundled theme, not only the two extremes: a frame derived from
    /// the terminal's colours has to hold up for all of them, and the one it
    /// fails on is the one somebody is using.
    #[test]
    fn every_bundled_theme_keeps_its_text_and_focus_visible() {
        for (name, background, foreground) in BUNDLED {
            let chrome = chrome(*background, *foreground);
            assert!(
                contrast(*foreground, chrome.surface) >= WCAG_AA_NORMAL_TEXT,
                "{name}: chrome text is not readable"
            );
            assert!(
                contrast(chrome.focus_rail, chrome.selected_bg) >= 3.0,
                "{name}: the focus rail disappears"
            );
        }
    }

    /// The frame follows the terminal. A fixed grey is what made the old
    /// window read as three stacked strips instead of one surface.
    #[test]
    fn the_frame_follows_the_terminals_own_colours() {
        let midnight = chrome(srgb(0x0c, 0x12, 0x20), srgb(0xdf, 0xe7, 0xf1));
        let daylight = chrome(srgb(0xf6, 0xf7, 0xf4), srgb(0x16, 0x1a, 0x1d));
        assert_ne!(midnight.surface, daylight.surface);
        assert!(brightness(daylight.surface) > brightness(midnight.surface));
    }

    /// And it stays close to it: a frame that is a long way from the
    /// terminal's background is a slab, which is the thing this is not.
    #[test]
    fn the_frame_stays_close_to_the_terminals_background() {
        for (name, background, foreground) in BUNDLED {
            let chrome = chrome(*background, *foreground);
            let apart = (brightness(chrome.surface) - brightness(*background)).abs();
            assert!(apart < 0.08, "{name}: the frame is {apart} away, a slab");
            assert_ne!(chrome.surface, *background, "{name}: no frame at all");
        }
    }

    /// Mixing in linear light, as the tones were chosen against. Blending
    /// sRGB values directly lands somewhere else entirely, and the difference
    /// is exactly what makes a considered frame look muddy.
    #[test]
    fn mixing_is_done_in_linear_light() {
        let black = srgb(0, 0, 0);
        let white = srgb(0xff, 0xff, 0xff);
        let half = mix(black, white, 0.5);
        // Half the *light* is around 0.73 in sRGB, not 0.5.
        assert!(half[0] > 0.70 && half[0] < 0.76, "{half:?}");
    }

    #[test]
    fn mixing_nothing_changes_nothing() {
        let colour = srgb(0x12, 0x34, 0x56);
        let mixed = mix(colour, srgb(0xff, 0, 0), 0.0);
        for channel in 0..3 {
            assert!(
                (mixed[channel] - colour[channel]).abs() < 0.002,
                "{mixed:?}"
            );
        }
    }

    #[test]
    fn light_and_dark_backgrounds_are_told_apart() {
        assert!(is_light_surface(srgb(0xf6, 0xf7, 0xf4)));
        assert!(!is_light_surface(srgb(0x0c, 0x12, 0x20)));
        assert!(!is_light_surface(srgb(0x12, 0x12, 0x12)));
    }

    fn composite(foreground: [f32; 4], background: [f32; 4]) -> [f32; 4] {
        let alpha = foreground[3];
        [
            foreground[0] * alpha + background[0] * (1.0 - alpha),
            foreground[1] * alpha + background[1] * (1.0 - alpha),
            foreground[2] * alpha + background[2] * (1.0 - alpha),
            1.0,
        ]
    }

    fn contrast(foreground: [f32; 4], background: [f32; 4]) -> f32 {
        let lighter = brightness(foreground).max(brightness(background));
        let darker = brightness(foreground).min(brightness(background));
        (lighter + 0.05) / (darker + 0.05)
    }

    fn brightness(color: [f32; 4]) -> f32 {
        luma(to_linear(color))
    }
}
