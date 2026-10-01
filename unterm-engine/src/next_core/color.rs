//! Colours, and the two adjustments a theme actually needs.
//!
//! A config wants a tab bar slightly lifted off the background and inactive
//! text slightly sunk into it, both derived from whatever theme is in effect.
//! In Lua that meant parsing a colour and calling `lighten`/`darken`; here the
//! two operations are built in, so a theme-following config needs no code.
//!
//! Lightening moves a colour toward white by a fraction of the distance
//! remaining, rather than by a fixed amount. Adding a constant blows out
//! colours that are already bright and does nothing visible to dark ones,
//! which is exactly backwards for a tab bar that has to work in both themes.

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Rgb {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Rgb {
    pub fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    /// Move each channel a fraction of the way to white.
    pub fn lighten(self, amount: f64) -> Self {
        self.blend_toward(255, amount)
    }

    /// Move each channel a fraction of the way to black.
    pub fn darken(self, amount: f64) -> Self {
        self.blend_toward(0, amount)
    }

    fn blend_toward(self, target: u8, amount: f64) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        Self {
            red: blend_channel(self.red, target, amount),
            green: blend_channel(self.green, target, amount),
            blue: blend_channel(self.blue, target, amount),
        }
    }

    /// Render as `#rrggbb`, the form a config is written in.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.red, self.green, self.blue)
    }
}

fn blend_channel(from: u8, target: u8, amount: f64) -> u8 {
    let from = from as f64;
    let delta = (target as f64 - from) * amount;
    // Round rather than truncate: truncating makes small adjustments do
    // nothing at all, so a config that asks for a slight lift gets no change.
    (from + delta).round().clamp(0.0, 255.0) as u8
}

/// Parse `#rgb` or `#rrggbb`, with or without the leading `#`.
///
/// The short form is doubled per digit, so `#fff` is white rather than a very
/// dark grey -- the reading every other tool gives it.
pub fn parse_hex(text: &str) -> Option<Rgb> {
    let digits = text.trim().strip_prefix('#').unwrap_or(text.trim());
    if !digits.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    match digits.len() {
        3 => {
            let mut chars = digits.chars().map(|ch| {
                let value = ch.to_digit(16).unwrap() as u8;
                value * 17
            });
            Some(Rgb::new(chars.next()?, chars.next()?, chars.next()?))
        }
        6 => Some(Rgb::new(
            u8::from_str_radix(&digits[0..2], 16).ok()?,
            u8::from_str_radix(&digits[2..4], 16).ok()?,
            u8::from_str_radix(&digits[4..6], 16).ok()?,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_long_form() {
        assert_eq!(parse_hex("#1e1e2e"), Some(Rgb::new(0x1e, 0x1e, 0x2e)));
        assert_eq!(parse_hex("1e1e2e"), Some(Rgb::new(0x1e, 0x1e, 0x2e)));
    }

    #[test]
    fn the_short_form_doubles_each_digit() {
        // `#fff` is white everywhere else; reading it as `#0f0f0f` would make a
        // config mean something very different from what it says.
        assert_eq!(parse_hex("#fff"), Some(Rgb::new(255, 255, 255)));
        assert_eq!(parse_hex("#08f"), Some(Rgb::new(0x00, 0x88, 0xff)));
    }

    #[test]
    fn rejects_what_is_not_a_colour() {
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("#gggggg"), None);
        assert_eq!(parse_hex(""), None);
    }

    #[test]
    fn lightening_a_dark_colour_makes_a_visible_difference() {
        let lifted = Rgb::new(0x11, 0x13, 0x15).lighten(0.05);

        // A tab bar lifted off the background has to actually move. Truncating
        // instead of rounding would leave this identical to its input.
        assert_ne!(lifted, Rgb::new(0x11, 0x13, 0x15));
        assert!(lifted.red > 0x11);
    }

    #[test]
    fn lightening_moves_toward_white_without_passing_it() {
        assert_eq!(
            Rgb::new(255, 255, 255).lighten(0.5),
            Rgb::new(255, 255, 255)
        );
        assert_eq!(Rgb::new(0, 0, 0).lighten(1.0), Rgb::new(255, 255, 255));
    }

    #[test]
    fn darkening_moves_toward_black_without_passing_it() {
        assert_eq!(Rgb::new(0, 0, 0).darken(0.5), Rgb::new(0, 0, 0));
        assert_eq!(Rgb::new(255, 255, 255).darken(1.0), Rgb::new(0, 0, 0));
    }

    #[test]
    fn an_adjustment_of_nothing_changes_nothing() {
        let colour = Rgb::new(0x33, 0x66, 0x99);

        assert_eq!(colour.lighten(0.0), colour);
        assert_eq!(colour.darken(0.0), colour);
    }

    #[test]
    fn an_out_of_range_amount_is_clamped_rather_than_wrapping() {
        assert_eq!(Rgb::new(0, 0, 0).lighten(5.0), Rgb::new(255, 255, 255));
        assert_eq!(
            Rgb::new(200, 200, 200).darken(-1.0),
            Rgb::new(200, 200, 200)
        );
    }

    #[test]
    fn a_bright_colour_lightens_less_than_a_dark_one() {
        let dark_shift = Rgb::new(0x10, 0x10, 0x10).lighten(0.2).red - 0x10;
        let bright_shift = Rgb::new(0xf0, 0xf0, 0xf0).lighten(0.2).red - 0xf0;

        // Proportional adjustment is what keeps one setting working in both a
        // dark and a light theme.
        assert!(dark_shift > bright_shift);
    }

    #[test]
    fn round_trips_through_hex() {
        let colour = Rgb::new(0x1e, 0x1e, 0x2e);

        assert_eq!(colour.to_hex(), "#1e1e2e");
        assert_eq!(parse_hex(&colour.to_hex()), Some(colour));
    }

    #[test]
    fn the_shipped_themes_adjustments_stay_distinguishable() {
        // The config this project ships lifts the bar by 0.05 and sinks
        // inactive text by 0.35, both from the theme colour.
        let background = parse_hex("#111315").expect("valid colour");
        let foreground = parse_hex("#e8eaed").expect("valid colour");

        assert_ne!(background.lighten(0.05), background);
        assert_ne!(foreground.darken(0.35), foreground);
        assert_ne!(background.lighten(0.05), foreground.darken(0.35));
    }
}

/// The xterm 256-colour palette, as every terminal agrees on it.
///
/// Indices 0-15 are the ANSI colours a theme may override; 16-231 are a
/// 6x6x6 cube; 232-255 are a 24-step grey ramp. The cube and the ramp are
/// arithmetic, not a table -- writing out 240 literals would be 240 chances
/// to mistype one.
///
/// This belongs to the kernel rather than to a front end: an index in a cell
/// means the same colour whoever is drawing it, and a front end that had to
/// supply its own would be a front end that could disagree with the others.
pub fn palette_rgb(index: u8) -> Rgb {
    const ANSI: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x00, 0x00),
        (0x00, 0xcd, 0x00),
        (0xcd, 0xcd, 0x00),
        (0x00, 0x00, 0xee),
        (0xcd, 0x00, 0xcd),
        (0x00, 0xcd, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x7f, 0x7f, 0x7f),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x5c, 0x5c, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    /// The cube's six levels are not evenly spaced: the first step is a large
    /// one, and the rest are even. Spacing them evenly makes dark colours
    /// visibly wrong against every other terminal.
    const CUBE: [u8; 6] = [0, 0x5f, 0x87, 0xaf, 0xd7, 0xff];

    match index {
        0..=15 => {
            let (red, green, blue) = ANSI[index as usize];
            Rgb::new(red, green, blue)
        }
        16..=231 => {
            let offset = index as usize - 16;
            Rgb::new(CUBE[offset / 36], CUBE[(offset / 6) % 6], CUBE[offset % 6])
        }
        232..=255 => {
            let level = 8 + (index as usize - 232) * 10;
            let level = level as u8;
            Rgb::new(level, level, level)
        }
    }
}

#[cfg(test)]
mod palette_tests {
    use super::*;

    #[test]
    fn the_cube_corners_are_where_every_terminal_puts_them() {
        assert_eq!(palette_rgb(16), Rgb::new(0, 0, 0));
        assert_eq!(palette_rgb(231), Rgb::new(0xff, 0xff, 0xff));
        // 196 is red, 46 green, 21 blue -- the three axes at full.
        assert_eq!(palette_rgb(196), Rgb::new(0xff, 0, 0));
        assert_eq!(palette_rgb(46), Rgb::new(0, 0xff, 0));
        assert_eq!(palette_rgb(21), Rgb::new(0, 0, 0xff));
    }

    #[test]
    fn the_grey_ramp_runs_from_near_black_to_near_white() {
        assert_eq!(palette_rgb(232), Rgb::new(8, 8, 8));
        assert_eq!(palette_rgb(255), Rgb::new(238, 238, 238));
    }

    #[test]
    fn the_cube_steps_unevenly_at_the_bottom() {
        // 0 -> 0x5f is the large first step. Even spacing would put it at
        // 0x33, which reads as a different colour entirely.
        assert_eq!(palette_rgb(16 + 1).blue, 0x5f);
    }
}

/// The colours a program is told when it asks (`OSC 4`, `10`, `11`, `12`).
///
/// A front end decides what a pane looks like; the kernel answers the
/// questions programs send through the pty, and in Core mode those two are
/// different processes. So the front end hands its effective theme down here
/// whenever it changes, and the kernel answers from it. Programs ask for a
/// reason: Claude Code, Codex, Neovim and friends read the background to
/// choose between their light and dark palettes, and a terminal that stays
/// silent leaves them guessing -- usually "dark", which is wrong half the time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TerminalColors {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    pub ansi: [Rgb; 16],
}

impl Default for TerminalColors {
    /// The default theme's frame colours, until a front end says otherwise:
    /// an answer that is right for most people beats no answer at all.
    fn default() -> Self {
        let mut ansi = [Rgb::new(0, 0, 0); 16];
        for (index, slot) in ansi.iter_mut().enumerate() {
            *slot = palette_rgb(index as u8);
        }
        Self {
            foreground: Rgb::new(0xf2, 0xef, 0xe7),
            background: Rgb::new(0x16, 0x18, 0x1d),
            cursor: Rgb::new(0xe8, 0xb3, 0x4b),
            ansi,
        }
    }
}

fn reported() -> &'static parking_lot::RwLock<TerminalColors> {
    static REPORTED: std::sync::OnceLock<parking_lot::RwLock<TerminalColors>> =
        std::sync::OnceLock::new();
    REPORTED.get_or_init(Default::default)
}

/// Replace what programs are told. Process-wide: one theme is in effect.
pub fn set_reported_colors(colors: TerminalColors) {
    *reported().write() = colors;
}

/// What programs are told right now.
pub fn reported_colors() -> TerminalColors {
    *reported().read()
}

impl Rgb {
    /// `rgb:RRRR/GGGG/BBBB`, the 16-bit-per-channel form xterm answers in.
    pub fn to_xparse(self) -> String {
        let wide = |channel: u8| u16::from(channel) * 257;
        format!(
            "rgb:{:04x}/{:04x}/{:04x}",
            wide(self.red),
            wide(self.green),
            wide(self.blue)
        )
    }
}
