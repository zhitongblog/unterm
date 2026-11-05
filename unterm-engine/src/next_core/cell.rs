use crate::{
    CellStyle, StyledBlink, StyledCell, StyledColor, StyledUnderline, StyledVerticalAlign,
};

/// One cell of the screen.
///
/// Its size is a feature. A row does not stop existing when it scrolls off the
/// top -- it moves into the scrollback, 10,000 rows deep by default -- so every
/// byte here is paid `cols * (rows + scrollback_limit)` times over, and paid
/// again on every write, every recycled row and every row copy. This layout is
/// 48 bytes; the obvious one (an inline `String`, a `usize` width, a `usize`
/// hyperlink index) was 80.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ScreenCell {
    pub(super) ch: char,
    /// Marks attached to this base cell: accents, variation selectors, ZWJ
    /// sequences, the second half of a regional-indicator flag.
    ///
    /// Boxed, because it is empty on essentially every cell a terminal paints.
    /// An inline `String` charges 24 bytes to every blank space on the screen
    /// and in the scrollback; a box charges 8 and allocates only for the cells
    /// that really do carry marks.
    combining: Option<Box<String>>,
    pub(super) attr: CellAttributes,
    /// Columns occupied: 0 for the tail of a wide character, 1, or 2. Two bits
    /// of information, so a `u8` -- as a `usize` it cost 8 bytes per cell.
    pub(super) width: u8,
    /// True on the last cell of a row that soft-wrapped into the next one.
    ///
    /// A line property, but carried per-cell so it travels with the row data
    /// through scrolling and into the scrollback without a parallel array to
    /// keep in sync. Deliberately not part of `CellAttributes`: it is not a
    /// style, and putting it there would leak into style comparison and SGR
    /// reporting.
    pub(super) wrapped: bool,
}

impl ScreenCell {
    pub(super) fn new(ch: char, attr: CellAttributes) -> Self {
        Self {
            ch,
            combining: None,
            attr,
            width: Self::char_width(ch),
            wrapped: false,
        }
    }

    pub(super) fn blank(attr: CellAttributes) -> Self {
        Self {
            ch: ' ',
            combining: None,
            attr,
            width: 1,
            wrapped: false,
        }
    }

    pub(super) fn continuation(attr: CellAttributes) -> Self {
        Self {
            ch: ' ',
            combining: None,
            attr,
            width: 0,
            wrapped: false,
        }
    }

    /// How many columns `ch` occupies.
    ///
    /// Printable ASCII short-circuits. It is one column by definition, and it
    /// is very nearly everything a terminal ever prints; the general path
    /// splits the character into grapheme clusters and walks the wcwidth
    /// tables, which a `sample` of the PTY reader thread under a flood put at
    /// about 14% of its working time -- almost all of it spent rediscovering
    /// that `a` is one column wide.
    pub(super) fn char_width(ch: char) -> u8 {
        if ch.is_ascii_graphic() || ch == ' ' {
            return 1;
        }
        let mut buf = [0u8; 4];
        termwiz::cell::unicode_column_width(ch.encode_utf8(&mut buf), None) as u8
    }

    /// The marks on this cell, or `""` when it has none.
    pub(super) fn combining(&self) -> &str {
        self.combining.as_deref().map(String::as_str).unwrap_or("")
    }

    pub(super) fn push_combining(&mut self, ch: char) {
        self.combining.get_or_insert_with(Default::default).push(ch);
    }

    pub(super) fn clear_combining(&mut self) {
        self.combining = None;
    }

    /// Whether `ch`, arriving right after this cell, continues its grapheme
    /// cluster instead of starting a cell of its own.
    ///
    /// Only ever asked of the cell immediately before the cursor; the rules
    /// are UAX #29's for the sequences that change how many columns a
    /// cluster takes:
    ///
    /// - GB11: after `ExtPict Extend* ZWJ`, only another Extended_Pictographic
    ///   character joins. `a` ZWJ `b` is two clusters, so `b` gets a cell.
    /// - An emoji modifier joins only a cluster that ends in an emoji base:
    ///   `👍🏽` is one cluster, `x🏽` is two.
    /// - GB12/13: a regional indicator pairs with one immediately preceding
    ///   unpaired regional indicator, and never with a third.
    pub(super) fn joins_trailing_cluster_char(&self, ch: char) -> bool {
        (self.expects_joined_char() && Self::is_extended_pictographic(ch))
            || (Self::is_emoji_modifier(ch)
                && self.width > 1
                && Self::is_emoji_modifier_base(self.last_cluster_char()))
            || (Self::is_regional_indicator(self.ch)
                && Self::is_regional_indicator(ch)
                && !self.combining().chars().any(Self::is_regional_indicator))
    }

    /// The cluster ends in a zero-width joiner and began with an
    /// Extended_Pictographic character, so the next pictograph joins it.
    fn expects_joined_char(&self) -> bool {
        self.combining().ends_with('\u{200d}') && Self::is_extended_pictographic(self.ch)
    }

    /// The last character of the cluster this cell holds.
    fn last_cluster_char(&self) -> char {
        self.combining().chars().last().unwrap_or(self.ch)
    }

    fn is_emoji_modifier(ch: char) -> bool {
        matches!(ch as u32, 0x1f3fb..=0x1f3ff)
    }

    /// Emoji_Modifier_Base, approximated: the pictographic planes plus the
    /// handful of BMP hand and person symbols that take skin tones.
    fn is_emoji_modifier_base(ch: char) -> bool {
        matches!(
            ch as u32,
            0x261d | 0x26f9 | 0x270a..=0x270d | 0x1f000..=0x1f3fa | 0x1f400..=0x1faff
        )
    }

    fn is_regional_indicator(ch: char) -> bool {
        matches!(ch as u32, 0x1f1e6..=0x1f1ff)
    }

    /// Extended_Pictographic (emoji-data.txt), as ranges. Coarse at the
    /// edges -- a few unassigned code points in these blocks count too --
    /// which is the same trade every terminal makes to avoid a table.
    pub(super) fn is_extended_pictographic(ch: char) -> bool {
        matches!(
            ch as u32,
            0x00a9
                | 0x00ae
                | 0x203c
                | 0x2049
                | 0x2122
                | 0x2139
                | 0x2194..=0x2199
                | 0x21a9..=0x21aa
                | 0x231a..=0x231b
                | 0x2328
                | 0x2388
                | 0x23cf
                | 0x23e9..=0x23f3
                | 0x23f8..=0x23fa
                | 0x24c2
                | 0x25aa..=0x25ab
                | 0x25b6
                | 0x25c0
                | 0x25fb..=0x25fe
                | 0x2600..=0x27bf
                | 0x2934..=0x2935
                | 0x2b05..=0x2b07
                | 0x2b1b..=0x2b1c
                | 0x2b50
                | 0x2b55
                | 0x3030
                | 0x303d
                | 0x3297
                | 0x3299
                | 0x1f000..=0x1f0ff
                | 0x1f10d..=0x1f10f
                | 0x1f12f
                | 0x1f16c..=0x1f171
                | 0x1f17e..=0x1f17f
                | 0x1f18e
                | 0x1f191..=0x1f19a
                | 0x1f1ad..=0x1f1e5
                | 0x1f201..=0x1f20f
                | 0x1f21a
                | 0x1f22f
                | 0x1f232..=0x1f23a
                | 0x1f23c..=0x1f23f
                | 0x1f249..=0x1f3fa
                | 0x1f400..=0x1f53d
                | 0x1f546..=0x1f64f
                | 0x1f680..=0x1f6ff
                | 0x1f774..=0x1f77f
                | 0x1f7d5..=0x1f7ff
                | 0x1f80c..=0x1f80f
                | 0x1f848..=0x1f84f
                | 0x1f85a..=0x1f85f
                | 0x1f888..=0x1f88f
                | 0x1f8ae..=0x1f8ff
                | 0x1f90c..=0x1f93a
                | 0x1f93c..=0x1f945
                | 0x1f947..=0x1faff
                | 0x1fc00..=0x1fffd
        )
    }

    #[allow(dead_code)]
    pub(super) fn styled(&self) -> StyledCell {
        self.styled_with_reverse_video(false, &[])
    }

    pub(super) fn styled_with_reverse_video(
        &self,
        reverse_video: bool,
        hyperlinks: &[String],
    ) -> StyledCell {
        let mut style = self.attr.style(hyperlinks);
        if reverse_video {
            style.inverse = !style.inverse;
        }
        StyledCell {
            ch: self.ch,
            style,
            width: usize::from(self.width),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct CellAttributes {
    pub(super) bold: bool,
    pub(super) faint: bool,
    pub(super) italic: bool,
    pub(super) underline: bool,
    pub(super) underline_style: Option<StyledUnderline>,
    pub(super) underline_color: Option<TerminalColor>,
    pub(super) strikethrough: bool,
    pub(super) hidden: bool,
    pub(super) overline: bool,
    pub(super) blink: Option<StyledBlink>,
    pub(super) vertical_align: Option<StyledVerticalAlign>,
    pub(super) inverse: bool,
    pub(super) protected: bool,
    pub(super) fg: Option<TerminalColor>,
    pub(super) bg: Option<TerminalColor>,
    /// Index into the screen's hyperlink table.
    ///
    /// A `u32`, not a `usize`: the index rides along in every cell on screen
    /// and in the scrollback, and no session will ever open four billion
    /// distinct URIs.
    pub(super) hyperlink: Option<u32>,
}

impl CellAttributes {
    pub(super) fn set_underline(&mut self, style: StyledUnderline) {
        self.underline = true;
        self.underline_style = Some(style);
    }

    pub(super) fn clear_underline(&mut self) {
        self.underline = false;
        self.underline_style = None;
    }

    pub(super) fn set_protected(&mut self, protected: bool) {
        self.protected = protected;
    }

    fn style(&self, hyperlinks: &[String]) -> CellStyle {
        CellStyle {
            bold: self.bold,
            faint: self.faint,
            italic: self.italic,
            underline: self.underline,
            underline_style: self.underline_style,
            underline_color: self.underline_color.map(Into::into),
            strikethrough: self.strikethrough,
            hidden: self.hidden,
            overline: self.overline,
            blink: self.blink,
            vertical_align: self.vertical_align,
            inverse: self.inverse,
            fg: self.fg.map(Into::into),
            bg: self.bg.map(Into::into),
            hyperlink: self.hyperlink.and_then(|idx| {
                hyperlinks
                    .get(idx as usize)
                    .filter(|uri| !uri.is_empty())
                    .cloned()
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TerminalColor {
    Palette(u8),
    Rgb(u8, u8, u8),
}

impl From<TerminalColor> for StyledColor {
    fn from(color: TerminalColor) -> Self {
        match color {
            TerminalColor::Palette(idx) => StyledColor::Palette(idx),
            TerminalColor::Rgb(r, g, b) => StyledColor::Rgb(r, g, b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cell is the unit that gets multiplied by a whole scrollback, so its
    /// size is pinned rather than left to drift: at 80 columns and the default
    /// 10,000-line limit, one byte added here is another 800 KB of resident
    /// memory per pane, plus a wider copy on every write.
    #[test]
    fn screen_cell_stays_compact() {
        assert_eq!(std::mem::size_of::<ScreenCell>(), 48);
    }

    #[test]
    fn ascii_fast_path_agrees_with_the_unicode_tables() {
        for code in 0x20u32..0x7f {
            let ch = char::from_u32(code).expect("ascii is a char");
            let mut buf = [0u8; 4];
            assert_eq!(
                u32::from(ScreenCell::char_width(ch)),
                termwiz::cell::unicode_column_width(ch.encode_utf8(&mut buf), None) as u32,
                "width of {ch:?} must not depend on which path computed it"
            );
        }
    }
}
