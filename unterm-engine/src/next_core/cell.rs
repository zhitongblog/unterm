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

    pub(super) fn joins_trailing_cluster_char(&self, ch: char) -> bool {
        self.expects_joined_char()
            || (self.width > 1 && Self::is_emoji_base(self.ch) && Self::is_emoji_modifier(ch))
            || (Self::is_regional_indicator(self.ch)
                && Self::is_regional_indicator(ch)
                && !self.combining().chars().any(Self::is_regional_indicator))
    }

    fn expects_joined_char(&self) -> bool {
        self.combining().ends_with('\u{200d}')
    }

    fn is_emoji_modifier(ch: char) -> bool {
        matches!(ch as u32, 0x1f3fb..=0x1f3ff)
    }

    fn is_emoji_base(ch: char) -> bool {
        matches!(ch as u32, 0x1f000..=0x1faff)
    }

    fn is_regional_indicator(ch: char) -> bool {
        matches!(ch as u32, 0x1f1e6..=0x1f1ff)
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
        let combining = self.combining();
        StyledCell {
            ch: self.ch,
            style,
            width: usize::from(self.width),
            // The rest of the grapheme, so a renderer can draw the accent,
            // the VS16 emoji form, the ZWJ family or the flag the program
            // wrote. The width above is unchanged by it.
            combining: (!combining.is_empty()).then(|| combining.into()),
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
    fn a_styled_cell_carries_its_marks_without_changing_width() {
        let mut cell = ScreenCell::new('e', CellAttributes::default());
        assert_eq!(cell.styled().combining, None, "no marks, no allocation");
        cell.push_combining('\u{301}');
        let styled = cell.styled();
        assert_eq!(styled.ch, 'e');
        assert_eq!(styled.combining(), "\u{301}");
        assert_eq!(styled.text(), "e\u{301}");
        assert_eq!(styled.width, 1);
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
