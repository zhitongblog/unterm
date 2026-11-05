//! Composing text before it is typed.
//!
//! Chinese, Japanese and Korean input all work the same way: the keys the
//! user presses are not the characters they mean. The system's input method
//! collects the keystrokes, shows what it has so far, and hands over the
//! finished text only when the user picks it. Without this a terminal on a
//! Chinese Windows cannot type Chinese at all -- the keystrokes arrive as
//! Latin letters and the candidate list never appears.
//!
//! Two things have to be right. The half-composed text has to be drawn where
//! the cursor is, because it is not in the shell yet and the shell cannot draw
//! it. And the system has to be told where that is, or it puts its candidate
//! list in the corner of the screen, away from what the user is typing.

/// Text the input method is still composing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preedit {
    pub text: String,
    /// Where the caret sits inside the composed text, in bytes.
    pub caret: Option<usize>,
}

impl Preedit {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// How many columns the composed text takes on the grid.
    ///
    /// By display width, not character count: a Chinese character is two
    /// columns wide, and measuring it as one puts the candidate list and the
    /// caret half a character out on every syllable. Measured by the same
    /// cells `append_text` draws it as, so the inverted background, the caret
    /// and the candidate window cover exactly the text drawn -- a zero-width
    /// jamo or joiner counted as nothing here and drawn as a column there
    /// left the last character hanging off the highlight.
    pub fn columns(&self) -> usize {
        crate::terminal::text_columns(&self.text)
    }

    /// Where the caret sits, in columns from the start of the composed text.
    ///
    /// The end of the text when the input method does not say, which is where
    /// a caret goes while you are still typing.
    pub fn caret_column(&self) -> usize {
        let Some(caret) = self.caret else {
            return self.columns();
        };
        let mut caret = caret.min(self.text.len());
        while !self.text.is_char_boundary(caret) {
            caret -= 1;
        }
        crate::terminal::text_columns(&self.text[..caret])
    }
}

/// Where the composed text is drawn, in pixels.
///
/// Clamped to stay inside the pane: a long composition near the right edge
/// would otherwise run off the window, and the part the user is about to
/// commit is the part they most need to see.
pub fn origin(
    cursor: (usize, usize),
    pane_origin: (f32, f32),
    cell: (f32, f32),
    pane_cols: usize,
    columns: usize,
) -> (f32, f32) {
    let last_start = pane_cols.saturating_sub(columns);
    let column = cursor.0.min(last_start.max(0));
    (
        pane_origin.0 + column as f32 * cell.0,
        pane_origin.1 + cursor.1 as f32 * cell.1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preedit(text: &str, caret: Option<usize>) -> Preedit {
        Preedit {
            text: text.to_string(),
            caret,
        }
    }

    #[test]
    fn a_chinese_character_is_two_columns_wide() {
        assert_eq!(preedit("中", None).columns(), 2);
        assert_eq!(preedit("中文", None).columns(), 4);
        assert_eq!(preedit("ni", None).columns(), 2);
    }

    /// A conjoining jamo is zero columns on the grid and rides on the
    /// syllable before it, here and where the text is drawn alike.
    #[test]
    fn zero_width_characters_are_measured_as_they_are_drawn() {
        let text = "\u{1100}\u{1161}a";
        assert_eq!(preedit(text, None).columns(), crate::terminal::text_columns(text));
        assert_eq!(preedit("e\u{301}", None).columns(), 1);
        assert_eq!(preedit("e\u{301}x", Some(3)).caret_column(), 1);
    }

    #[test]
    fn the_caret_defaults_to_the_end_of_what_is_composed() {
        assert_eq!(preedit("中文", None).caret_column(), 4);
    }

    #[test]
    fn the_caret_is_measured_in_columns_not_bytes() {
        // "中" is three bytes and two columns. A caret after it is at column
        // 2, and reporting 3 would put the candidate list past the character.
        assert_eq!(preedit("中文", Some(3)).caret_column(), 2);
        assert_eq!(preedit("中文", Some(0)).caret_column(), 0);
    }

    #[test]
    fn a_caret_past_the_end_is_clamped_rather_than_panicking() {
        // Input methods have been known to report a caret from a longer
        // string than the one they sent.
        assert_eq!(preedit("中", Some(99)).caret_column(), 2);
    }

    #[test]
    fn composing_near_the_right_edge_stays_inside_the_pane() {
        // 10 columns wide, cursor at column 8, composing 4 columns: drawing
        // at the cursor would run 2 columns past the edge.
        let (x, _) = origin((8, 0), (0.0, 0.0), (10.0, 20.0), 10, 4);
        assert_eq!(x, 60.0, "pulled back so all four columns fit");
    }

    #[test]
    fn composing_with_room_is_drawn_at_the_cursor() {
        let (x, y) = origin((3, 2), (0.0, 0.0), (10.0, 20.0), 40, 4);
        assert_eq!((x, y), (30.0, 40.0));
    }

    #[test]
    fn a_split_panes_composition_is_placed_against_that_pane() {
        let (x, y) = origin((1, 1), (400.0, 20.0), (10.0, 20.0), 40, 2);
        assert_eq!((x, y), (410.0, 40.0));
    }

    #[test]
    fn a_composition_wider_than_the_pane_starts_at_its_edge() {
        let (x, _) = origin((5, 0), (0.0, 0.0), (10.0, 20.0), 4, 20);
        assert_eq!(x, 0.0, "no position fits, so show the start of it");
    }
}
