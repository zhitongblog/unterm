//! Turning a row of cells into glyphs, ligatures included.
//!
//! A terminal is a grid, and shaping is not: `!=` becomes one glyph in a font
//! that has the ligature, Arabic picks a different glyph per position, and a
//! combining mark rides on its base without advancing at all. So the two have
//! to be reconciled, and every terminal reconciles them the same way -- shape
//! the text, then place each glyph at the *cell its cluster came from* rather
//! than at the pen position shaping would have used.
//!
//! Placing by cluster is what keeps the grid honest. A ligature spans the
//! cells its characters occupied and the cursor still lands between them; a
//! font whose advances drift from the cell width cannot pull the row out of
//! alignment, because the row's positions never came from the advances.

/// A stretch of a row that can be shaped as one piece.
///
/// Runs stop at a style change, because a run is also what gets one colour,
/// and at a cell the primary face cannot draw, because shaping is per-face
/// and a run spanning two faces would be shaped by the wrong one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// Column this run starts at.
    pub column: usize,
    /// The column just past the run's last cell.
    pub end: usize,
    /// The text, for the shaper.
    pub text: String,
    /// Which column each byte offset in `text` belongs to.
    ///
    /// The shaper reports clusters as byte offsets; this is how a glyph gets
    /// back to the cell it came from.
    pub columns: Vec<(usize, usize)>,
}

impl Run {
    /// The column a cluster byte offset belongs to.
    ///
    /// Clusters land on character boundaries the shaper chose, which are not
    /// always ones we recorded -- a reordered script can report the start of
    /// the cluster it merged. The nearest recorded offset at or before it is
    /// the cell that character came from.
    pub fn column_of(&self, cluster: usize) -> usize {
        self.columns
            .iter()
            .rev()
            .find(|(offset, _)| *offset <= cluster)
            .map(|(_, column)| *column)
            .unwrap_or(self.column)
    }
}

/// Whether two cells can be shaped together.
///
/// Style, because a run is drawn in one colour. Face, because HarfBuzz shapes
/// with one font and a run crossing two would be shaped by whichever it
/// started with -- which is how a CJK character in the middle of a Latin word
/// comes out as a box.
pub fn same_run(
    a_style: &unterm_engine::CellStyle,
    a_face: usize,
    b_style: &unterm_engine::CellStyle,
    b_face: usize,
) -> bool {
    a_face == b_face && a_style == b_style
}

/// Split a line's cells into shapeable runs.
///
/// `face_of` says which face draws a character. Blank cells end a run: there
/// is nothing to shape across a space, and stopping there keeps runs short,
/// which is what makes shaping a whole screen affordable.
///
/// Characters the renderer draws itself end a run too, and are left out of it.
/// Most monospace fonts ship their own box-drawing and powerline glyphs, so
/// without this the shaper claims the column and the drawn version never
/// runs -- tables come back with the hairline gaps between cells that drawing
/// them was meant to close.
pub fn runs(
    cells: &[unterm_engine::StyledCell],
    mut face_of: impl FnMut(char) -> usize,
) -> Vec<Run> {
    runs_with(cells, |cell| face_of(cell.ch), None)
}

/// As `runs`, with the face chosen per cell and an optional column that has
/// to be a run of its own.
///
/// The cell is the unit, not the character: a cell's whole grapheme -- base,
/// accents, VS16, the rest of a ZWJ sequence -- goes to the shaper together,
/// every byte of it mapped back to the base cell's column, so the marks
/// land on the character they belong to and the cell's width never changes.
///
/// `isolate` is the block cursor's column. The glyph under the cursor is
/// drawn in the background colour on the block; a ligature or a negative
/// bearing reaching across the cursor's edge would be half inverted and
/// half not. Shaping that cell on its own, as kitty and wezterm do, means
/// whatever is drawn there belongs to that cell alone.
pub fn runs_with(
    cells: &[unterm_engine::StyledCell],
    mut face_of: impl FnMut(&unterm_engine::StyledCell) -> usize,
    isolate: Option<usize>,
) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut current: Option<(Run, usize, unterm_engine::CellStyle)> = None;
    let mut column = 0usize;

    for cell in cells {
        // The spacer after a wide character: its columns are the wide
        // cell's. Treating it as a space both broke the run after every
        // CJK character and shifted every column after it by one.
        if cell.width == 0 {
            continue;
        }
        let width = cell.width;
        let blank = (cell.ch == ' ' || cell.ch == '\0') && cell.combining().is_empty();
        if blank || cell.style.hidden || unterm_render::box_glyphs::draws(cell.ch) {
            if let Some((run, _, _)) = current.take() {
                runs.push(run);
            }
            column += width;
            continue;
        }
        let face = face_of(cell);
        let isolated = isolate == Some(column);
        match current.as_mut() {
            Some((run, run_face, run_style))
                if !isolated
                    && isolate != Some(run.column)
                    && same_run(run_style, *run_face, &cell.style, face) =>
            {
                run.columns.push((run.text.len(), column));
                run.text.push(cell.ch);
                run.text.push_str(cell.combining());
                run.end = column + width;
            }
            _ => {
                if let Some((run, _, _)) = current.take() {
                    runs.push(run);
                }
                let mut run = Run {
                    column,
                    end: column + width,
                    text: String::new(),
                    columns: Vec::new(),
                };
                run.columns.push((0, column));
                run.text.push(cell.ch);
                run.text.push_str(cell.combining());
                current = Some((run, face, cell.style.clone()));
            }
        }
        column += width;
    }
    if let Some((run, _, _)) = current.take() {
        runs.push(run);
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use unterm_engine::{CellStyle, StyledCell, StyledColor};

    fn cells(text: &str, style: CellStyle) -> Vec<StyledCell> {
        text.chars()
            .map(|ch| StyledCell {
                ch,
                style: style.clone(),
                width: 1,
                combining: None,
            })
            .collect()
    }

    fn latin(_: char) -> usize {
        0
    }

    #[test]
    fn a_plain_word_is_one_run() {
        let runs = runs(&cells("hello", CellStyle::default()), latin);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "hello");
        assert_eq!(runs[0].column, 0);
    }

    #[test]
    fn a_space_ends_a_run() {
        // Nothing to shape across a space, and short runs are what make
        // shaping a whole screen affordable.
        let runs = runs(&cells("ab cd", CellStyle::default()), latin);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "ab");
        assert_eq!(runs[1].text, "cd");
        assert_eq!(runs[1].column, 3, "the run starts after the space");
    }

    #[test]
    fn a_colour_change_ends_a_run() {
        // A run is also what gets one colour.
        let mut coloured = CellStyle::default();
        coloured.fg = Some(StyledColor::Palette(1));
        let mut line = cells("ab", CellStyle::default());
        line.extend(cells("cd", coloured));
        let runs = runs(&line, latin);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "ab");
        assert_eq!(runs[1].text, "cd");
    }

    #[test]
    fn a_font_change_ends_a_run() {
        // A run crossing two faces would be shaped by whichever it started
        // with, which is how a CJK character mid-word comes out as a box.
        let line = cells("ab中", CellStyle::default());
        let runs = runs(&line, |ch| if ch.is_ascii() { 0 } else { 1 });
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "ab");
        assert_eq!(runs[1].text, "中");
    }

    #[test]
    fn a_glyph_finds_the_column_its_cluster_came_from() {
        let runs = runs(&cells("a!=b", CellStyle::default()), latin);
        let run = &runs[0];
        assert_eq!(run.column_of(0), 0);
        assert_eq!(run.column_of(1), 1, "the '!' is in column 1");
        assert_eq!(run.column_of(2), 2);
        assert_eq!(run.column_of(3), 3);
    }

    #[test]
    fn a_cluster_between_recorded_offsets_takes_the_one_before_it() {
        // A reordered script reports the start of the cluster it merged,
        // which is not always an offset we recorded.
        let runs = runs(&cells("中文", CellStyle::default()), |_| 0);
        let run = &runs[0];
        assert_eq!(run.column_of(1), 0, "still inside the first character");
        assert_eq!(run.column_of(3), 1);
    }

    #[test]
    fn wide_characters_advance_the_column_by_their_width() {
        let line = vec![
            StyledCell {
                ch: '中',
                style: CellStyle::default(),
                width: 2,
                combining: None,
            },
            StyledCell {
                ch: 'a',
                style: CellStyle::default(),
                width: 1,
                combining: None,
            },
        ];
        let runs = runs(&line, |_| 0);
        // One run, and the 'a' sits at column 2 -- not column 1.
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].column_of(3), 2);
    }

    #[test]
    fn a_blank_line_produces_nothing_to_shape() {
        assert!(runs(&cells("   ", CellStyle::default()), latin).is_empty());
        assert!(runs(&[], latin).is_empty());
    }

    #[test]
    fn hidden_cells_are_not_shaped() {
        let mut hidden = CellStyle::default();
        hidden.hidden = true;
        let mut line = cells("ab", CellStyle::default());
        line.extend(cells("xy", hidden));
        let runs = runs(&line, latin);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "ab");
    }

    /// The shaper gets the whole grapheme, and every byte of it maps back to
    /// the base cell: the accent rides on the `e`, the `x` after it keeps
    /// its own column.
    #[test]
    fn a_cells_marks_are_shaped_with_it_and_map_to_its_column() {
        let mut line = cells("ex", CellStyle::default());
        line[0].combining = Some("\u{301}".into());
        let runs = runs(&line, latin);
        assert_eq!(runs.len(), 1);
        let run = &runs[0];
        assert_eq!(run.text, "e\u{301}x");
        assert_eq!(run.column_of(1), 0, "the accent belongs to the e");
        assert_eq!(run.column_of(3), 1, "and the x is still column 1");
        assert_eq!(run.end, 2);
    }

    /// The block cursor's cell is shaped alone, so no ligature or bearing
    /// can straddle the cursor's edge.
    #[test]
    fn the_isolated_column_is_a_run_of_its_own() {
        let line = cells("a=>b", CellStyle::default());
        let runs = runs_with(&line, |_| 0, Some(2));
        let texts: Vec<&str> = runs.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(texts, vec!["a=", ">", "b"]);
        assert_eq!((runs[1].column, runs[1].end), (2, 3));
        assert_eq!(runs[2].column, 3);
    }

    #[test]
    fn a_run_records_where_it_ends() {
        let line = vec![
            StyledCell::new('中', CellStyle::default(), 2),
            StyledCell::new(' ', CellStyle::default(), 0),
            StyledCell::new('a', CellStyle::default(), 1),
        ];
        let runs = runs(&line, |_| 0);
        assert_eq!((runs[0].column, runs[0].end), (0, 3));
    }
}

#[cfg(test)]
mod url_regression {
    use super::*;
    use unterm_engine::{CellStyle, StyledCell};

    /// Every column of a URL gets a glyph.
    ///
    /// "example.com" rendered as "exampl  com" -- two characters silently
    /// gone. Whatever the cause, the property to hold is that shaping a run
    /// covers every column the run occupies.
    #[test]
    fn shaping_a_url_covers_every_column() {
        let Some(mut stack) = crate::fonts::FontStack::system(16) else {
            return;
        };
        let text = "https://example.com";
        let cells: Vec<StyledCell> = text
            .chars()
            .map(|ch| StyledCell {
                ch,
                style: CellStyle::default(),
                width: 1,
                combining: None,
            })
            .collect();

        let runs = runs(&cells, |ch| stack.face_for(ch));
        assert_eq!(runs.len(), 1, "no spaces, so one run");
        let run = &runs[0];

        let Some(glyphs) = stack.shape(0, &run.text) else {
            return;
        };
        let covered: std::collections::HashSet<usize> = glyphs
            .iter()
            .map(|glyph| run.column_of(glyph.cluster as usize))
            .collect();

        let missing: Vec<usize> = (0..text.chars().count())
            .filter(|column| !covered.contains(column))
            .collect();
        assert!(
            missing.is_empty(),
            "columns {missing:?} of {text:?} got no glyph; \
             shaped {} glyphs for {} characters",
            glyphs.len(),
            text.chars().count()
        );
    }
}

#[cfg(test)]
mod drawn_cell_tests {
    use super::*;
    use unterm_engine::{CellStyle, StyledCell};

    fn cell(ch: char) -> StyledCell {
        StyledCell {
            ch,
            width: 1,
            style: CellStyle::default(),
            combining: None,
        }
    }

    fn line(text: &str) -> Vec<StyledCell> {
        text.chars().map(cell).collect()
    }

    /// The font has its own box-drawing glyphs, and they lose.
    ///
    /// If the shaper claims the column, the renderer's own drawing never runs,
    /// and the table comes back with the hairline gaps that drawing it was
    /// meant to close. So a drawn character must not appear in any run.
    #[test]
    fn a_character_the_renderer_draws_is_left_out_of_every_run() {
        let runs = runs(&line("ab\u{2500}cd"), |_| 0);
        let shaped: String = runs.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(shaped, "abcd", "the line drew {shaped:?}");
    }

    #[test]
    fn it_breaks_the_run_rather_than_joining_across_itself() {
        let runs = runs(&line("ab\u{2500}cd"), |_| 0);
        assert_eq!(runs.len(), 2, "got {runs:?}");
        assert_eq!(runs[0].text, "ab");
        assert_eq!(runs[1].text, "cd");
    }

    /// The column a run starts at still counts the cells that were skipped,
    /// or the text after a box glyph would be drawn one cell to the left.
    #[test]
    fn the_run_after_a_drawn_cell_keeps_its_column() {
        let runs = runs(&line("ab\u{2500}cd"), |_| 0);
        assert_eq!(runs[1].column, 3);
    }

    #[test]
    fn a_powerline_separator_is_skipped_too() {
        let runs = runs(&line("main\u{E0B0}dev"), |_| 0);
        let shaped: String = runs.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(shaped, "maindev");
    }

    /// Only the forms we actually draw. A character in the same block that we
    /// leave to the font must still be shaped, or it turns into a blank cell.
    #[test]
    fn an_undrawn_neighbour_is_still_shaped() {
        let undrawn = ('\u{2500}'..='\u{259F}').find(|ch| !unterm_render::box_glyphs::draws(*ch));
        let Some(undrawn) = undrawn else {
            return; // We draw the whole block; nothing to check.
        };
        let runs = runs(&line(&format!("a{undrawn}b")), |_| 0);
        let shaped: String = runs.iter().map(|run| run.text.as_str()).collect();
        assert!(shaped.contains(undrawn), "{undrawn:?} was dropped by both");
    }
}
