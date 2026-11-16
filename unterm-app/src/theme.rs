//! The colour schemes the product ships.
//!
//! Recovered from the previous front end's `unterm_schemes.rs` rather than
//! re-picked: every one of these was chosen against a terminal, and a theme
//! that is nearly the old one is a theme people notice is wrong without being
//! able to say why.
//!
//! One rule from that file worth keeping visible, because it looks like a
//! mistake otherwise: **no background here is pure black**. An inactive split
//! is dimmed by multiplying its brightness, and pure black multiplied by
//! anything is still pure black -- so a scheme with a black background loses
//! the one cue that says which pane is focused.

use crate::chrome::srgb;

/// A scheme: the frame's two colours, the parts of the terminal that are not
/// text, and the sixteen the programs themselves ask for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub id: &'static str,
    pub name: &'static str,
    pub background: [f32; 4],
    pub foreground: [f32; 4],
    pub cursor: [f32; 4],
    pub selection: [f32; 4],
    /// What text on a selection is drawn in. A scheme that chose a highlight
    /// chose a colour to read on it; deriving one here would undo that.
    pub selection_text: [f32; 4],
    pub divider: [f32; 4],
    pub scrollbar: [f32; 4],
    /// The eight ANSI colours, then their bright halves.
    pub ansi: [[f32; 4]; 16],
}

/// Every theme, in the order the picker lists them.
pub const THEMES: &[Theme] = &[
    // The inbox design's own scheme, and the default: the deep neutral
    // ground the amber status language was tuned against.
    Theme {
        id: "agent-inbox",
        name: "Agent Inbox",
        background: srgb(0x16, 0x18, 0x1d),
        // Bright enough to stand apart from an agent's secondary grey.
        //
        // Claude Code and Codex mark their hints, token counts and status
        // lines with `#999999`. Against the old `#d6d3cc` that is a contrast
        // ratio of 1.91 -- below the 3:1 floor where a difference reads at a
        // glance, and the dimmest foreground of any theme here while its
        // siblings sat between 2.28 and 2.46. This lifts the same warm white
        // to 2.48 without going to a clinical pure white.
        foreground: srgb(0xf2, 0xef, 0xe7),
        cursor: srgb(0xe8, 0xb3, 0x4b),
        selection: srgb(0x2c, 0x31, 0x3a),
        selection_text: srgb(0xf0, 0xed, 0xe6),
        divider: srgb(0x27, 0x2b, 0x33),
        scrollbar: srgb(0x6c, 0x72, 0x80),
        ansi: [
            srgb(0x23, 0x25, 0x2a),
            srgb(0xd1, 0x6a, 0x5a),
            srgb(0x7d, 0xb3, 0x7f),
            srgb(0xe8, 0xb3, 0x4b),
            srgb(0x7a, 0xa2, 0xd6),
            srgb(0xb1, 0x89, 0xc9),
            srgb(0x6f, 0xb3, 0xa8),
            srgb(0xd6, 0xd3, 0xcc),
            srgb(0x3a, 0x3f, 0x49),
            srgb(0xe0, 0x85, 0x73),
            srgb(0x9c, 0xcf, 0x9e),
            srgb(0xf0, 0xc7, 0x78),
            srgb(0x9d, 0xbd, 0xe4),
            srgb(0xc9, 0xa8, 0xdd),
            srgb(0x92, 0xca, 0xbf),
            srgb(0xf0, 0xed, 0xe6),
        ],
    },
    Theme {
        id: "standard",
        name: "Standard",
        background: srgb(0x11, 0x13, 0x15),
        foreground: srgb(0xe8, 0xea, 0xed),
        cursor: srgb(0xe8, 0xea, 0xed),
        selection: srgb(0x2b, 0x34, 0x34),
        selection_text: srgb(0xff, 0xff, 0xff),
        divider: srgb(0x30, 0x36, 0x38),
        scrollbar: srgb(0x76, 0x76, 0x76),
        ansi: [
            srgb(0x1c, 0x1c, 0x1c),
            srgb(0xff, 0x5f, 0x57),
            srgb(0x5f, 0xd1, 0x7a),
            srgb(0xe5, 0xc4, 0x63),
            srgb(0x5a, 0xa7, 0xff),
            srgb(0xc6, 0x78, 0xdd),
            srgb(0x63, 0xcd, 0xb8),
            srgb(0xd6, 0xd6, 0xd6),
            srgb(0x73, 0x73, 0x73),
            srgb(0xff, 0x7b, 0x72),
            srgb(0x7e, 0xe7, 0x87),
            srgb(0xf2, 0xd1, 0x6b),
            srgb(0x79, 0xb8, 0xff),
            srgb(0xd2, 0xa8, 0xff),
            srgb(0x82, 0xdf, 0xc9),
            srgb(0xff, 0xff, 0xff),
        ],
    },
    Theme {
        id: "midnight",
        name: "Midnight",
        background: srgb(0x0c, 0x12, 0x20),
        foreground: srgb(0xdf, 0xe7, 0xf1),
        cursor: srgb(0xdf, 0xe7, 0xf1),
        selection: srgb(0x26, 0x3a, 0x4d),
        selection_text: srgb(0xf8, 0xfb, 0xff),
        divider: srgb(0x35, 0x44, 0x5d),
        scrollbar: srgb(0x71, 0x80, 0x98),
        ansi: [
            srgb(0x17, 0x1d, 0x2b),
            srgb(0xff, 0x6b, 0x7a),
            srgb(0x8b, 0xdc, 0x88),
            srgb(0xe6, 0xc4, 0x6a),
            srgb(0x82, 0xaa, 0xff),
            srgb(0xc9, 0x9c, 0xff),
            srgb(0x67, 0xcd, 0xb6),
            srgb(0xcb, 0xd5, 0xe1),
            srgb(0x66, 0x70, 0x85),
            srgb(0xff, 0x87, 0x94),
            srgb(0xa7, 0xec, 0x9f),
            srgb(0xf0, 0xd3, 0x7a),
            srgb(0x9c, 0xc0, 0xff),
            srgb(0xd8, 0xb4, 0xff),
            srgb(0x8a, 0xe0, 0xcc),
            srgb(0xff, 0xff, 0xff),
        ],
    },
    Theme {
        id: "daylight",
        name: "Daylight",
        background: srgb(0xf6, 0xf7, 0xf4),
        foreground: srgb(0x16, 0x1a, 0x1d),
        cursor: srgb(0x16, 0x1a, 0x1d),
        selection: srgb(0xc9, 0xdd, 0xd8),
        selection_text: srgb(0x10, 0x17, 0x15),
        divider: srgb(0xa9, 0xb4, 0xaf),
        scrollbar: srgb(0x69, 0x75, 0x6f),
        ansi: [
            srgb(0x0b, 0x0f, 0x14),
            srgb(0xb4, 0x23, 0x35),
            srgb(0x17, 0x64, 0x3b),
            srgb(0x7a, 0x52, 0x00),
            srgb(0x00, 0x5e, 0xa8),
            srgb(0x65, 0x37, 0xa0),
            srgb(0x0a, 0x76, 0x69),
            srgb(0x3f, 0x47, 0x52),
            srgb(0x60, 0x69, 0x75),
            srgb(0xcf, 0x33, 0x47),
            srgb(0x25, 0x82, 0x4d),
            srgb(0x93, 0x63, 0x00),
            srgb(0x0a, 0x74, 0xc9),
            srgb(0x7b, 0x4c, 0xc2),
            srgb(0x11, 0x8d, 0x7d),
            srgb(0x02, 0x04, 0x06),
        ],
    },
    Theme {
        id: "classic",
        name: "Classic",
        background: srgb(0x12, 0x12, 0x12),
        foreground: srgb(0xee, 0xee, 0xee),
        cursor: srgb(0xee, 0xee, 0xee),
        selection: srgb(0x38, 0x38, 0x38),
        selection_text: srgb(0xff, 0xff, 0xff),
        divider: srgb(0x56, 0x56, 0x56),
        scrollbar: srgb(0x7a, 0x7a, 0x7a),
        ansi: [
            srgb(0x1c, 0x1c, 0x1c),
            srgb(0xef, 0x44, 0x44),
            srgb(0x22, 0xc5, 0x5e),
            srgb(0xea, 0xb3, 0x08),
            srgb(0x3b, 0x82, 0xf6),
            srgb(0xa8, 0x55, 0xf7),
            srgb(0x06, 0xb6, 0xd4),
            srgb(0xd4, 0xd4, 0xd4),
            srgb(0x73, 0x73, 0x73),
            srgb(0xf8, 0x71, 0x71),
            srgb(0x4a, 0xde, 0x80),
            srgb(0xfa, 0xcc, 0x15),
            srgb(0x60, 0xa5, 0xfa),
            srgb(0xc0, 0x84, 0xfc),
            srgb(0x22, 0xd3, 0xee),
            srgb(0xff, 0xff, 0xff),
        ],
    },
    Theme {
        id: "notion-dark",
        name: "Notion Dark",
        background: srgb(0x18, 0x18, 0x18),
        foreground: srgb(0xee, 0xee, 0xec),
        cursor: srgb(0xee, 0xee, 0xec),
        selection: srgb(0x3f, 0x3f, 0x3a),
        selection_text: srgb(0xff, 0xff, 0xff),
        divider: srgb(0x56, 0x54, 0x4d),
        scrollbar: srgb(0x8a, 0x86, 0x7c),
        ansi: [
            srgb(0x25, 0x25, 0x25),
            srgb(0xff, 0x6f, 0x61),
            srgb(0x4f, 0xb2, 0x86),
            srgb(0xe7, 0xb8, 0x4f),
            srgb(0x5a, 0xa7, 0xd6),
            srgb(0xb0, 0x83, 0xd9),
            srgb(0x5f, 0xc6, 0xbd),
            srgb(0xd8, 0xd8, 0xd4),
            srgb(0x66, 0x63, 0x5d),
            srgb(0xff, 0x8b, 0x80),
            srgb(0x6e, 0xd0, 0xa2),
            srgb(0xf2, 0xcc, 0x6b),
            srgb(0x7b, 0xc0, 0xe6),
            srgb(0xc4, 0x9b, 0xe6),
            srgb(0x7d, 0xe0, 0xd7),
            srgb(0xff, 0xff, 0xff),
        ],
    },
    Theme {
        id: "notion-light",
        name: "Notion Light",
        background: srgb(0xf8, 0xf7, 0xf4),
        foreground: srgb(0x1f, 0x1e, 0x1a),
        cursor: srgb(0x1f, 0x1e, 0x1a),
        selection: srgb(0xb8, 0xd4, 0xe6),
        selection_text: srgb(0x10, 0x13, 0x15),
        divider: srgb(0x9f, 0x9a, 0x8f),
        scrollbar: srgb(0x5f, 0x5a, 0x51),
        ansi: [
            srgb(0x1f, 0x1e, 0x1a),
            srgb(0xb8, 0x32, 0x32),
            srgb(0x2f, 0x6f, 0x4f),
            srgb(0x8b, 0x5e, 0x12),
            srgb(0x1f, 0x6f, 0x9f),
            srgb(0x73, 0x4a, 0x9b),
            srgb(0x00, 0x6f, 0x7f),
            srgb(0x5d, 0x5b, 0x55),
            srgb(0x4f, 0x4d, 0x47),
            srgb(0xd5, 0x48, 0x48),
            srgb(0x3a, 0x8a, 0x60),
            srgb(0xa8, 0x74, 0x16),
            srgb(0x2b, 0x82, 0xba),
            srgb(0x86, 0x5b, 0xb0),
            srgb(0x00, 0x89, 0x9a),
            srgb(0x0f, 0x0f, 0x0d),
        ],
    },
];

/// The one used when nothing says otherwise.
pub fn default_theme() -> &'static Theme {
    &THEMES[0]
}

/// Find a theme by the id the config and the CLI use.
pub fn by_id(id: &str) -> Option<&'static Theme> {
    THEMES.iter().find(|theme| theme.id == id)
}

/// Where the chosen theme is written, so the next launch opens in it.
///
/// `~/.unterm/theme.json`, the same file the CLI has always used -- the two
/// have to agree, or switching in one is undone by the other.
fn remembered_path() -> Option<std::path::PathBuf> {
    unterm_protocol::state_path("theme.json")
}

/// The theme chosen last time, if there was one.
pub fn remembered() -> Option<String> {
    let text = std::fs::read_to_string(remembered_path()?).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let id = value.get("theme")?.as_str()?.to_string();
    by_id(&id).map(|theme| theme.id.to_string())
}

/// Modification timestamp of `~/.unterm/theme.json`, used to detect live theme
/// switches written by `unterm-core` (Web Settings) or `unterm-cli` in another
/// process without re-reading the file on every tick.
pub fn remembered_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(remembered_path()?).ok()?.modified().ok()
}

/// Remember a theme for next time.
pub fn remember(id: &str) -> anyhow::Result<()> {
    if remembered().as_deref() == Some(id) {
        return Ok(());
    }
    let path = remembered_path().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = by_id(id).map(|t| t.name).unwrap_or(id);
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "theme": id,
            "name": name,
            "color_scheme": name,
        }))?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A theme nobody ships is not restored: an id left over from an older
    /// build would otherwise leave the window in no theme at all.
    #[test]
    fn only_a_theme_that_exists_is_restored() {
        assert!(by_id("standard").is_some());
        assert!(by_id("a-theme-from-another-product").is_none());
    }

    #[test]
    fn every_theme_is_findable_by_its_id() {
        for theme in THEMES {
            assert_eq!(by_id(theme.id).map(|found| found.id), Some(theme.id));
        }
        assert_eq!(by_id("nothing-like-this"), None);
    }

    /// The six the product documents, under the ids the CLI and the settings
    /// page already use.
    #[test]
    fn the_bundled_themes_are_all_here() {
        let ids: Vec<&str> = THEMES.iter().map(|theme| theme.id).collect();
        assert_eq!(
            ids,
            vec![
                "agent-inbox",
                "standard",
                "midnight",
                "daylight",
                "classic",
                "notion-dark",
                "notion-light"
            ]
        );
    }

    /// No background is pure black. An inactive split is dimmed by
    /// multiplying, and black multiplied by anything is black -- so a black
    /// background loses the cue that says which pane is focused.
    #[test]
    fn no_background_is_pure_black() {
        for theme in THEMES {
            let sum = theme.background[0] + theme.background[1] + theme.background[2];
            assert!(sum > 0.0, "{} has a pure black background", theme.name);
        }
    }

    /// Sixteen colours, all distinct enough to tell apart: a scheme where two
    /// of them match is a scheme where `ls --color` loses a category.
    #[test]
    fn every_theme_has_sixteen_usable_colours() {
        for theme in THEMES {
            assert_eq!(theme.ansi.len(), 16);
            for (index, colour) in theme.ansi.iter().enumerate() {
                for other in &theme.ansi[index + 1..] {
                    assert_ne!(colour, other, "{} repeats a colour", theme.name);
                }
            }
        }
    }

    /// Text has to be readable on its own background, in every theme. This is
    /// the one that fails first when a scheme is edited by eye.
    #[test]
    fn text_is_readable_on_its_own_background() {
        for theme in THEMES {
            let contrast = contrast(theme.foreground, theme.background);
            assert!(
                contrast >= 7.0,
                "{}: contrast is only {contrast}",
                theme.name
            );
        }
    }

    /// Selected text has to be readable on its own highlight -- the pair was
    /// chosen together, and using the ordinary foreground on the highlight is
    /// how a selection becomes a smear.
    #[test]
    fn selected_text_is_readable_on_its_highlight() {
        for theme in THEMES {
            let contrast = contrast(theme.selection_text, theme.selection);
            assert!(
                contrast >= 4.5,
                "{}: selected text contrast is {contrast}",
                theme.name
            );
        }
    }

    /// And the cursor has to be visible against it, or it disappears in the
    /// middle of a line.
    #[test]
    fn the_cursor_stands_out_from_the_background() {
        for theme in THEMES {
            let contrast = contrast(theme.cursor, theme.background);
            assert!(contrast >= 3.0, "{}: the cursor is invisible", theme.name);
        }
    }

    fn contrast(a: [f32; 4], b: [f32; 4]) -> f32 {
        let luma = |c: [f32; 4]| {
            let channel = |v: f32| {
                if v <= 0.040_45 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(c[0]) + 0.7152 * channel(c[1]) + 0.0722 * channel(c[2])
        };
        let (x, y) = (luma(a), luma(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }
}
