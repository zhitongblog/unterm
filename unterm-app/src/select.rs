//! Turning mouse positions into a selection.
//!
//! The engine already knows what a selection *is* and what text comes out of
//! one. What is missing between a window and that is arithmetic: which cell a
//! pixel is in, and when a drag has actually begun.
//!
//! Kept out of the event loop so both can be checked without a window, because
//! both are the kind of thing that is off by one cell until someone drags
//! across a line and looks.

use unterm_engine::next_core::selection::{Selection, SelectionPoint, SelectionShape};
use unterm_render::quads::CellMetrics;

/// Which cell a pixel position falls in.
///
/// The column is *not* clamped to the row's text: selecting past the end of a
/// short line and on into the next is how every terminal behaves, and clamping
/// here would stop the selection dead at the last character.
pub fn cell_at(x: f32, y: f32, metrics: CellMetrics, viewport_top: i64) -> SelectionPoint {
    let column = (x.max(0.0) / metrics.width.max(1.0)).floor() as usize;
    let row = (y.max(0.0) / metrics.height.max(1.0)).floor() as i64;
    SelectionPoint::new(column, viewport_top + row)
}

/// A selection being dragged out.
#[derive(Clone, Copy, Debug)]
pub struct Drag {
    anchor: SelectionPoint,
    current: SelectionPoint,
    shape: SelectionShape,
    /// Whether the pointer has left the cell it started in.
    ///
    /// A click that never moves is a click, not an empty selection. Treating
    /// it as a selection clears the clipboard on every stray click.
    moved: bool,
}

impl Drag {
    pub fn start(at: SelectionPoint, shape: SelectionShape) -> Self {
        Self {
            anchor: at,
            current: at,
            shape,
            moved: false,
        }
    }

    pub fn extend(&mut self, to: SelectionPoint) {
        if to != self.anchor {
            self.moved = true;
        }
        self.current = to;
    }

    /// The selection, or None while this is still just a click.
    pub fn selection(&self) -> Option<Selection> {
        if !self.moved {
            return None;
        }
        let mut selection = Selection::new(self.anchor, self.shape);
        selection.extend_to(self.current);
        Some(selection)
    }
}

/// Build the terminal keystroke sequence (Left/Right cursor moves + Backspaces)
/// that deletes `selection` when it lies inside the active logical input line
/// containing the cursor.
///
/// Returns `None` when:
/// - the alternate screen is active (vim / less / htop / tmux),
/// - the selection falls outside the cursor's soft-wrapped logical line
///   (e.g. historical scrollback output above the prompt), or
/// - the selection covers no non-empty characters.
pub fn selection_delete_sequence(
    selection: Selection,
    lines: &[unterm_engine::StyledScreenLine],
    cursor_row_idx: usize,
    cursor_col: usize,
    application_cursor_keys: bool,
    alt_screen_active: bool,
) -> Option<String> {
    if alt_screen_active || selection.is_empty() {
        return None;
    }
    let cur_line = lines.get(cursor_row_idx)?;
    let cur_scroll_row = cur_line.row;

    // Expand across any soft-wrapped rows that belong to the same logical
    // input line as the cursor.
    let mut logical_start = cursor_row_idx;
    while logical_start > 0 && lines[logical_start - 1].wrapped {
        logical_start -= 1;
    }
    let mut logical_end = cursor_row_idx;
    while logical_end + 1 < lines.len() && lines[logical_end].wrapped {
        logical_end += 1;
    }

    let (start, end) = selection.normalized();
    let min_row = lines[logical_start].row;
    let max_row = lines[logical_end].row;
    if start.row < min_row || end.row > max_row {
        return None;
    }

    // Flatten the logical line into non-spacer character entries:
    // `(scroll_row, start_col, end_col_exclusive, ch)`.
    let mut chars: Vec<(i64, usize, usize, char)> = Vec::new();
    for line in &lines[logical_start..=logical_end] {
        for (col, cell) in line.cells.iter().enumerate() {
            if cell.width == 0 {
                // Wide-glyph trailing spacer cell — not a separate character.
                continue;
            }
            let w = (cell.width as usize).max(1);
            chars.push((line.row, col, col + w, cell.ch));
        }
    }

    // Trim trailing padding spaces beyond the cursor and last non-blank glyph
    // so dragging past the end of the line clamps to the actual typed text.
    let last_meaningful = chars
        .iter()
        .enumerate()
        .rev()
        .find(|(_, (r, c, _, ch))| {
            !ch.is_whitespace() || (*r == cur_scroll_row && *c < cursor_col)
        })
        .map(|(i, _)| i + 1)
        .unwrap_or(0);
    chars.truncate(last_meaningful);
    if chars.is_empty() {
        return None;
    }

    // Map `(row, col)` to a character index in `0..=chars.len()`.
    let pos_to_idx = |target_row: i64, target_col: usize| -> usize {
        for (idx, &(r, c_start, c_end, _)) in chars.iter().enumerate() {
            if r > target_row || (r == target_row && target_col < c_end) {
                // If target_col lands inside or before this glyph, snap to it.
                let _ = c_start;
                return idx;
            }
        }
        chars.len()
    };

    let cursor_idx = pos_to_idx(cur_scroll_row, cursor_col);
    let sel_start_idx = pos_to_idx(start.row, start.column);
    // Selection endpoints in Unterm are inclusive on `end.column`.
    let sel_end_idx = pos_to_idx(end.row, end.column.saturating_add(1));

    if sel_end_idx <= sel_start_idx {
        return None;
    }
    let delete_count = sel_end_idx - sel_start_idx;
    if delete_count > 4096 {
        return None;
    }

    let left = if application_cursor_keys {
        "\x1bOD"
    } else {
        "\x1b[D"
    };
    let right = if application_cursor_keys {
        "\x1bOC"
    } else {
        "\x1b[C"
    };

    let mut out = String::with_capacity(delete_count * 4);
    if sel_end_idx > cursor_idx {
        for _ in 0..(sel_end_idx - cursor_idx) {
            out.push_str(right);
        }
    } else if cursor_idx > sel_end_idx {
        for _ in 0..(cursor_idx - sel_end_idx) {
            out.push_str(left);
        }
    }
    for _ in 0..delete_count {
        out.push('\x7f');
    }
    Some(out)
}

/// Build the Left/Right arrow escape sequence to move the terminal cursor from
/// its live position to `target` when the user clicks inside the active logical
/// input line (shell prompt or Claude Code / Codex / Agy input box).
///
/// Returns `None` when:
/// - the alternate screen is active (`vim` / `less` / `htop` / `tmux`),
/// - the click lands outside the cursor's soft-wrapped logical line, or
/// - the cursor is already at the clicked character.
pub fn cursor_move_sequence(
    target: SelectionPoint,
    lines: &[unterm_engine::StyledScreenLine],
    cursor_row_idx: usize,
    cursor_col: usize,
    application_cursor_keys: bool,
    alt_screen_active: bool,
) -> Option<String> {
    if alt_screen_active {
        return None;
    }
    let cur_line = lines.get(cursor_row_idx)?;
    let cur_scroll_row = cur_line.row;

    let mut logical_start = cursor_row_idx;
    while logical_start > 0 && lines[logical_start - 1].wrapped {
        logical_start -= 1;
    }
    let mut logical_end = cursor_row_idx;
    while logical_end + 1 < lines.len() && lines[logical_end].wrapped {
        logical_end += 1;
    }

    let min_row = lines[logical_start].row;
    let max_row = lines[logical_end].row;
    if target.row < min_row || target.row > max_row {
        return None;
    }

    let mut chars: Vec<(i64, usize, usize, char)> = Vec::new();
    for line in &lines[logical_start..=logical_end] {
        for (col, cell) in line.cells.iter().enumerate() {
            if cell.width == 0 {
                continue;
            }
            let w = (cell.width as usize).max(1);
            chars.push((line.row, col, col + w, cell.ch));
        }
    }

    let last_meaningful = chars
        .iter()
        .enumerate()
        .rev()
        .find(|(_, (r, c, _, ch))| {
            !ch.is_whitespace() || (*r == cur_scroll_row && *c < cursor_col)
        })
        .map(|(i, _)| i + 1)
        .unwrap_or(0);
    chars.truncate(last_meaningful);
    if chars.is_empty() {
        return None;
    }

    let pos_to_idx = |target_row: i64, target_col: usize| -> usize {
        for (idx, &(r, _c_start, c_end, _)) in chars.iter().enumerate() {
            if r > target_row || (r == target_row && target_col < c_end) {
                return idx;
            }
        }
        chars.len()
    };

    let cursor_idx = pos_to_idx(cur_scroll_row, cursor_col);
    let target_idx = pos_to_idx(target.row, target.column);
    if target_idx == cursor_idx {
        return None;
    }
    let steps = target_idx.abs_diff(cursor_idx);
    if steps > 4096 {
        return None;
    }

    let arrow = if target_idx > cursor_idx {
        if application_cursor_keys {
            "\x1bOC"
        } else {
            "\x1b[C"
        }
    } else if application_cursor_keys {
        "\x1bOD"
    } else {
        "\x1b[D"
    };
    Some(arrow.repeat(steps))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> CellMetrics {
        CellMetrics {
            width: 10.0,
            height: 20.0,
            baseline: 16.0,
        }
    }

    #[test]
    fn a_pixel_lands_in_the_cell_that_contains_it() {
        assert_eq!(cell_at(0.0, 0.0, metrics(), 0), SelectionPoint::new(0, 0));
        assert_eq!(cell_at(9.9, 19.9, metrics(), 0), SelectionPoint::new(0, 0));
        assert_eq!(cell_at(10.0, 20.0, metrics(), 0), SelectionPoint::new(1, 1));
    }

    #[test]
    fn the_row_is_measured_from_the_top_of_the_scrollback() {
        // The viewport moves; a selection made while scrolled back has to stay
        // on the text it was made on.
        assert_eq!(
            cell_at(0.0, 40.0, metrics(), 100),
            SelectionPoint::new(0, 102)
        );
    }

    #[test]
    fn a_position_outside_the_window_does_not_go_negative() {
        // A drag that leaves the window reports negative coordinates, and a
        // negative row would select from the wrong end of the scrollback.
        assert_eq!(
            cell_at(-50.0, -50.0, metrics(), 5),
            SelectionPoint::new(0, 5)
        );
    }

    #[test]
    fn a_click_that_never_moves_is_not_a_selection() {
        let drag = Drag::start(SelectionPoint::new(3, 2), SelectionShape::Linear);

        // Treating it as an empty selection clears the clipboard on every
        // stray click.
        assert!(drag.selection().is_none());
    }

    #[test]
    fn moving_within_the_same_cell_is_still_a_click() {
        let mut drag = Drag::start(SelectionPoint::new(3, 2), SelectionShape::Linear);

        drag.extend(SelectionPoint::new(3, 2));

        assert!(drag.selection().is_none());
    }

    #[test]
    fn a_drag_to_another_cell_is_a_selection() {
        let mut drag = Drag::start(SelectionPoint::new(3, 2), SelectionShape::Linear);

        drag.extend(SelectionPoint::new(7, 4));
        let selection = drag.selection().expect("a moved drag selects");

        assert_eq!(selection.rows(), 2..=4);
    }

    #[test]
    fn dragging_backwards_keeps_the_anchor_where_it_started() {
        let mut drag = Drag::start(SelectionPoint::new(7, 4), SelectionShape::Linear);

        drag.extend(SelectionPoint::new(3, 2));
        let selection = drag.selection().expect("a moved drag selects");

        // Dragging up and left is normal, and the anchor must not follow the
        // pointer or the selection jumps as soon as it is reversed.
        assert_eq!(selection.anchor, SelectionPoint::new(7, 4));
        assert_eq!(selection.rows(), 2..=4);
    }

    #[test]
    fn a_block_drag_keeps_its_shape() {
        let mut drag = Drag::start(SelectionPoint::new(1, 1), SelectionShape::Block);

        drag.extend(SelectionPoint::new(5, 5));
        let selection = drag.selection().expect("a moved drag selects");

        assert_eq!(selection.shape, SelectionShape::Block);
    }

    fn make_line(row: i64, text: &str, wrapped: bool) -> unterm_engine::StyledScreenLine {
        let mut cells = Vec::new();
        for ch in text.chars() {
            let is_wide = ch as u32 >= 0x2E80;
            let width = if is_wide { 2 } else { 1 };
            cells.push(unterm_engine::StyledCell {
                ch,
                combining: None,
                width,
                style: unterm_engine::CellStyle::default(),
            });
            if is_wide {
                cells.push(unterm_engine::StyledCell {
                    ch: ' ',
                    combining: None,
                    width: 0,
                    style: unterm_engine::CellStyle::default(),
                });
            }
        }
        unterm_engine::StyledScreenLine {
            row,
            cells,
            wrapped,
        }
    }

    #[test]
    fn deletes_selected_ascii_span_before_cursor() {
        let lines = vec![make_line(10, "$ echo hello world   ", false)];
        let mut sel = Selection::new(SelectionPoint::new(7, 10), SelectionShape::Linear);
        sel.extend_to(SelectionPoint::new(12, 10));
        let seq = selection_delete_sequence(sel, &lines, 0, 18, false, false).unwrap();
        assert_eq!(seq, format!("{}{}", "\x1b[D".repeat(5), "\x7f".repeat(6)));
    }

    #[test]
    fn deletes_selected_span_after_cursor_using_right_arrows() {
        let lines = vec![make_line(10, "$ echo hello world", false)];
        let mut sel = Selection::new(SelectionPoint::new(13, 10), SelectionShape::Linear);
        sel.extend_to(SelectionPoint::new(17, 10));
        let seq = selection_delete_sequence(sel, &lines, 0, 7, true, false).unwrap();
        assert_eq!(seq, format!("{}{}", "\x1bOC".repeat(11), "\x7f".repeat(5)));
    }

    #[test]
    fn counts_cjk_wide_characters_as_single_backspaces() {
        let lines = vec![make_line(5, "> 你好世界", false)];
        let mut sel = Selection::new(SelectionPoint::new(2, 5), SelectionShape::Linear);
        sel.extend_to(SelectionPoint::new(5, 5));
        let seq = selection_delete_sequence(sel, &lines, 0, 10, false, false).unwrap();
        assert_eq!(seq, format!("{}{}", "\x1b[D".repeat(2), "\x7f".repeat(2)));
    }

    #[test]
    fn deletes_across_soft_wrapped_input_rows() {
        let lines = vec![
            make_line(20, "> first ", true),
            make_line(21, "second", false),
        ];
        let mut sel = Selection::new(SelectionPoint::new(5, 20), SelectionShape::Linear);
        sel.extend_to(SelectionPoint::new(2, 21));
        let seq = selection_delete_sequence(sel, &lines, 1, 6, false, false).unwrap();
        assert_eq!(seq, format!("{}{}", "\x1b[D".repeat(3), "\x7f".repeat(6)));
    }

    #[test]
    fn ignores_historical_scrollback_and_alt_screen_selections() {
        let lines = vec![
            make_line(9, "previous output line", false),
            make_line(10, "$ current input", false),
        ];
        let mut sel = Selection::new(SelectionPoint::new(0, 9), SelectionShape::Linear);
        sel.extend_to(SelectionPoint::new(7, 9));
        assert!(selection_delete_sequence(sel, &lines, 1, 15, false, false).is_none());

        let mut on_prompt = Selection::new(SelectionPoint::new(2, 10), SelectionShape::Linear);
        on_prompt.extend_to(SelectionPoint::new(8, 10));
        assert!(selection_delete_sequence(on_prompt, &lines, 1, 15, false, true).is_none());
    }

    #[test]
    fn moves_cursor_to_clicked_column_on_active_input_line() {
        let lines = vec![make_line(10, "$ echo hello world   ", false)];
        // Cursor at end (18), click on 'h' of "hello" (col 7) -> 11 left arrows
        let seq = cursor_move_sequence(SelectionPoint::new(7, 10), &lines, 0, 18, false, false)
            .unwrap();
        assert_eq!(seq, "\x1b[D".repeat(11));

        // Cursor at col 7, click on 'w' of "world" (col 13) -> 6 right arrows (app mode)
        let seq2 = cursor_move_sequence(SelectionPoint::new(13, 10), &lines, 0, 7, true, false)
            .unwrap();
        assert_eq!(seq2, "\x1bOC".repeat(6));
    }

    #[test]
    fn click_move_counts_cjk_wide_chars_and_ignores_scrollback_or_alt_screen() {
        let lines = vec![
            make_line(4, "history output", false),
            make_line(5, "> 你好世界", false),
        ];
        // Cursor at end (col 10), click on '好' (col 4..6) -> 3 left arrows ('界', '世', '好')
        let seq =
            cursor_move_sequence(SelectionPoint::new(4, 5), &lines, 1, 10, false, false).unwrap();
        assert_eq!(seq, "\x1b[D".repeat(3));
        // Clicking history line or in alt-screen does nothing
        assert!(cursor_move_sequence(SelectionPoint::new(2, 4), &lines, 1, 10, false, false).is_none());
        assert!(cursor_move_sequence(SelectionPoint::new(4, 5), &lines, 1, 10, false, true).is_none());
    }
}
