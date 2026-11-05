//! Re-wrapping rows when the screen changes width.
//!
//! A row that soft-wrapped carries `wrapped` on its last cell (see
//! `cell.rs`). Joining every run of wrapped rows gives back the logical lines
//! the program printed; splitting those again at the new width is a reflow.
//! It is what xterm, VTE, kitty and alacritty do for the main screen, and it
//! is what makes narrowing a pane lossless: widening it again puts every
//! wrapped line back on one row, scrollback included.
//!
//! The rules, mostly after alacritty's `grid/resize.rs`:
//!
//! - A wide character never straddles two rows. When one does not fit in the
//!   last column, the row ends a column early on a blank spacer and is still
//!   marked wrapped. Joining drops that spacer again: a wrapped row whose last
//!   cell is a plain blank, followed by a row that starts with a wide
//!   character, is read as a spacer. (Cells have no spare bit to mark the
//!   spacer explicitly, so a real space typed into exactly that column is
//!   indistinguishable from one; it is the only lossy case.)
//! - Trailing blanks of a logical line that did not wrap are trimmed before
//!   it is split again, so `ls` output padded to 200 columns does not turn
//!   into 200/40 rows of nothing.
//! - Positions -- the cursor, the saved cursor, any absolute row anchor --
//!   are carried as an offset into their logical line, so they end up on the
//!   same character.
//!
//! Cost: cells are moved, never cloned, and a line that still fits keeps its
//! own allocation; only rows that actually split allocate.

use super::cell::{CellAttributes, ScreenCell};

/// A position in the combined history (scrollback rows first, then the live
/// lines), as row and column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ReflowPoint {
    pub(super) row: usize,
    pub(super) col: usize,
}

pub(super) struct Reflowed {
    /// The new rows. A caller may take them; `total_rows` stays.
    pub(super) rows: Vec<Vec<ScreenCell>>,
    pub(super) total_rows: usize,
    /// Leading rows (0 or 1) that stand in for lines skipped because the
    /// scrollback limit would evict them; the caller trims them.
    pub(super) evicted: usize,
    /// For every old row, the new row its first cell landed on.
    pub(super) row_map: Vec<usize>,
    /// May have `col == new_cols`: the pending-wrap state, carried over.
    pub(super) cursor: ReflowPoint,
    pub(super) saved_cursor: ReflowPoint,
}

impl Reflowed {
    /// Where an old absolute row went. Rows past the old end (a picture that
    /// hangs below the last line) keep their distance from the end.
    pub(super) fn map_row(&self, row: usize) -> usize {
        match self.row_map.get(row) {
            Some(mapped) => *mapped,
            None => self.total_rows + (row - self.row_map.len()),
        }
    }
}

/// A blank that carries nothing visible: safe to trim off the end of a line.
pub(super) fn is_trim_blank(cell: &ScreenCell) -> bool {
    cell.ch == ' '
        && cell.width == 1
        && cell.combining().is_empty()
        && cell.attr.bg.is_none()
        && !cell.attr.inverse
        && !cell.attr.underline
        && !cell.attr.strikethrough
        && !cell.attr.overline
        && cell.attr.hyperlink.is_none()
}

/// A row with nothing on it, which a shrink may drop.
pub(super) fn is_blank_row(row: &[ScreenCell]) -> bool {
    row.iter().all(|cell| is_trim_blank(cell) && !cell.wrapped)
}

fn spacer() -> ScreenCell {
    ScreenCell::blank(CellAttributes::default())
}

fn is_plain_blank(cell: &ScreenCell) -> bool {
    cell.ch == ' ' && cell.width == 1 && cell.combining().is_empty()
}

/// Cut or pad rows to `cols` without re-wrapping: the alternate screen, which
/// a full-screen program repaints anyway. The one thing it must not leave is
/// half a wide character at the new edge.
pub(super) fn fit_rows_to_cols<'a>(
    rows: impl IntoIterator<Item = &'a mut Vec<ScreenCell>>,
    cols: usize,
) {
    for row in rows {
        if row.len() > cols {
            row.truncate(cols);
        }
        if let Some(last) = row.last_mut() {
            // A wrap flag only means something on the last column.
            last.wrapped = false;
        }
        // A head on the last column whose tail was cut off.
        if let Some(last) = row.last_mut() {
            if last.width > 1 {
                blank_in_place(last);
            }
        }
    }
}

struct Ctx {
    /// Stand-in rows in front of out for lines skipped as evicted.
    base: usize,
    /// Emptied buffers of skipped rows, reused for split rows: fresh
    /// allocations here are mostly page faults.
    pool: Vec<Vec<ScreenCell>>,
    new_cols: usize,
    out: Vec<Vec<ScreenCell>>,
    row_map: Vec<usize>,
    /// Start offset (in consumed cells) of every new row of the line being
    /// emitted; reused across lines.
    row_starts: Vec<usize>,
    cursor: Option<ReflowPoint>,
    saved_cursor: Option<ReflowPoint>,
}

/// What the current logical line has collected so far.
struct Pending {
    cells: Vec<ScreenCell>,
    /// (old row, offset of its first cell in `cells`).
    members: Vec<(usize, usize)>,
    cursor_offset: Option<(usize, bool)>,
    saved_offset: Option<usize>,
}

/// Re-wrap `old_rows` (scrollback then live lines, oldest first) from
/// `old_cols` to `new_cols`.
///
/// `cursor_pending` says the cursor sits past the last column waiting to wrap
/// (`cursor.col == old_cols`); the result keeps that state when the cursor
/// again ends exactly at the right edge.
///
/// `evictable`, when given, lets the reflow skip lines it can prove the
/// scrollback limit would evict straight afterwards: a narrowing multiplies
/// the row count, so re-wrapping a full scrollback only to throw half of it
/// away was most of the cost of a resize.
pub(super) fn reflow(
    old_rows: Vec<Vec<ScreenCell>>,
    old_cols: usize,
    new_cols: usize,
    cursor: ReflowPoint,
    cursor_pending: bool,
    saved_cursor: ReflowPoint,
    evictable: Option<Evictable>,
) -> Reflowed {
    let new_cols = new_cols.max(1);
    let old_cols = old_cols.max(1);
    let total = old_rows.len();
    let skip = evictable
        .map(|evictable| rows_to_skip(&old_rows, new_cols, cursor.row, evictable))
        .unwrap_or(0);
    let base = usize::from(skip > 0);
    let rows = old_rows.into_iter();
    let mut ctx = Ctx {
        base,
        pool: Vec::new(),
        new_cols,
        out: Vec::with_capacity(total),
        row_map: vec![0; total],
        row_starts: Vec::new(),
        cursor: None,
        saved_cursor: None,
    };
    let mut pending = Pending {
        cells: Vec::new(),
        members: Vec::new(),
        cursor_offset: None,
        saved_offset: None,
    };
    let mut open = false;
    let mut rows = rows.enumerate().peekable();
    while let Some((idx, mut row)) = rows.next() {
        if idx < skip {
            // Evicted anyway; every anchor on it lands on the one stand-in
            // row the caller trims. Its buffer is worth keeping.
            row.clear();
            ctx.pool.push(row);
            continue;
        }
        let wrapped = row.last().is_some_and(|cell| cell.wrapped);
        if wrapped {
            if row.len() < old_cols {
                // Wrapping means the text reached the margin; whatever was
                // never written up to it is blank, not missing.
                let attr = row.last().map(|cell| cell.attr).unwrap_or_default();
                row.resize(old_cols, ScreenCell::blank(attr));
            }
            let next_starts_wide = rows
                .peek()
                .and_then(|(_, next)| next.first())
                .is_some_and(|cell| cell.width > 1);
            if let Some(last) = row.last_mut() {
                last.wrapped = false;
            }
            if next_starts_wide && row.last().is_some_and(is_plain_blank) {
                row.pop();
            }
        }
        let start = if open { pending.cells.len() } else { 0 };
        if idx == cursor.row {
            if cursor_pending {
                // Track the last cell written; the cursor sits just past it.
                pending.cursor_offset = Some((start + cursor.col.saturating_sub(1), true));
            } else {
                pending.cursor_offset = Some((start + cursor.col, false));
            }
        }
        if idx == saved_cursor.row {
            pending.saved_offset = Some(start + saved_cursor.col);
        }
        pending.members.push((idx, start));
        if open {
            pending.cells.append(&mut row);
        } else {
            // A line's first row lends it its allocation.
            pending.cells = row;
            open = true;
        }
        if !wrapped {
            ctx.emit(&mut pending);
            open = false;
        }
    }
    if open {
        ctx.emit(&mut pending);
    }
    if ctx.out.is_empty() {
        ctx.out.push(Vec::new());
    }
    let last_row = base + ctx.out.len() - 1;
    let cursor = ctx.cursor.unwrap_or(ReflowPoint {
        row: last_row,
        col: 0,
    });
    // A saved cursor below the last line (DECSC on a row never written to).
    let saved_cursor = ctx.saved_cursor.unwrap_or(ReflowPoint {
        row: last_row + saved_cursor.row.saturating_sub(total),
        col: saved_cursor.col.min(new_cols - 1),
    });
    Reflowed {
        evicted: base,
        total_rows: base + ctx.out.len(),
        rows: ctx.out,
        row_map: ctx.row_map,
        cursor,
        saved_cursor,
    }
}

impl Ctx {
    fn emit(&mut self, pending: &mut Pending) {
        let new_cols = self.new_cols;
        let mut cells = std::mem::take(&mut pending.cells);
        let mut len = cells.len();
        while len > 0 && is_trim_blank(&cells[len - 1]) {
            len -= 1;
        }
        cells.truncate(len);
        let first = self.out.len();
        self.row_starts.clear();
        self.row_starts.push(0);

        if len <= new_cols && line_is_sound(&mut cells) {
            // Fits as it is: keep the allocation.
            self.out.push(cells);
        } else {
            self.split(cells);
        }

        for &(old, start) in &pending.members {
            let rel = self.row_of(start).0.min(self.out.len() - first - 1);
            self.row_map[old] = self.base + first + rel;
        }
        pending.members.clear();

        if let Some((offset, was_pending)) = pending.cursor_offset.take() {
            let (mut rel, mut col) = self.row_of(offset);
            if col >= new_cols {
                rel += col / new_cols;
                col %= new_cols;
            }
            if was_pending {
                // Just past the last cell written; at the right edge that is
                // the pending-wrap column again.
                col += 1;
            }
            // The cursor stands past the end of the text, on a row the text
            // never reached: give it that row, as part of the same line.
            while first + rel >= self.out.len() {
                if let Some(last) = self.out.last_mut() {
                    if last.len() < new_cols {
                        last.resize(new_cols, spacer());
                    }
                    if let Some(cell) = last.last_mut() {
                        cell.wrapped = true;
                    }
                }
                self.out.push(Vec::new());
            }
            self.cursor = Some(ReflowPoint {
                row: self.base + first + rel,
                col,
            });
        }
        if let Some(offset) = pending.saved_offset.take() {
            let (rel, col) = self.row_of(offset);
            let last = self.out.len() - first - 1;
            let (rel, col) = if col >= new_cols {
                let rel = rel + col / new_cols;
                if rel > last {
                    (last, new_cols - 1)
                } else {
                    (rel, col % new_cols)
                }
            } else {
                (rel.min(last), col)
            };
            self.saved_cursor = Some(ReflowPoint {
                row: self.base + first + rel,
                col,
            });
        }
    }

    /// Which new row (relative to the line) holds `offset`, and its column.
    /// An offset past the end lands on the last row, possibly past its end.
    fn row_of(&self, offset: usize) -> (usize, usize) {
        let rel = self
            .row_starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1);
        (rel, offset - self.row_starts[rel])
    }

    /// Split a line that does not fit (or needs repair) into rows.
    ///
    /// Three passes, so that cells are moved in bulk rather than one at a
    /// time: repair wide characters in place, find the row breaks from the
    /// widths alone, then drain each row off the end of the line. The first
    /// row keeps the line's own allocation.
    fn split(&mut self, mut cells: Vec<ScreenCell>) {
        let new_cols = self.new_cols;
        repair_wide_chars(&mut cells, new_cols);

        let mut col = 0usize;
        let mut idx = 0usize;
        while idx < cells.len() {
            let width = usize::from(cells[idx].width).max(1);
            if col + width > new_cols {
                self.row_starts.push(idx);
                col = 0;
            }
            col += width;
            idx += width;
        }

        let breaks = self.row_starts.len() - 1;
        let mut tails: Vec<Vec<ScreenCell>> = Vec::with_capacity(breaks);
        for &start in self.row_starts[1..].iter().rev() {
            let mut row = self.pool.pop().unwrap_or_else(|| Vec::with_capacity(new_cols));
            row.extend(cells.drain(start..));
            tails.push(row);
        }
        let mut rows = std::iter::once(cells).chain(tails.into_iter().rev()).peekable();
        while let Some(mut row) = rows.next() {
            if rows.peek().is_some() {
                // A wide character that did not fit ends the row early.
                while row.len() < new_cols {
                    row.push(spacer());
                }
                if let Some(last) = row.last_mut() {
                    last.wrapped = true;
                }
            }
            self.out.push(row);
        }
    }
}

/// Make every wide character whole, in place: clear wrap flags, blank
/// orphaned continuations and heads that lost their tails mid-line, give a
/// head at the very end its tail back (an old truncation took it), and blank
/// a character wider than the screen.
fn repair_wide_chars(cells: &mut Vec<ScreenCell>, new_cols: usize) {
    let mut idx = 0;
    while idx < cells.len() {
        cells[idx].wrapped = false;
        let width = usize::from(cells[idx].width);
        if width == 0 {
            blank_in_place(&mut cells[idx]);
            idx += 1;
            continue;
        }
        if width == 1 {
            idx += 1;
            continue;
        }
        let mut tails = 0;
        while tails < width - 1
            && idx + 1 + tails < cells.len()
            && cells[idx + 1 + tails].width == 0
        {
            cells[idx + 1 + tails].wrapped = false;
            tails += 1;
        }
        if tails < width - 1 {
            if idx + 1 + tails == cells.len() {
                let attr = cells[idx].attr;
                for _ in tails..width - 1 {
                    cells.push(ScreenCell::continuation(attr));
                }
            } else {
                for cell in &mut cells[idx..=idx + tails] {
                    blank_in_place(cell);
                }
                idx += 1 + tails;
                continue;
            }
        }
        if width > new_cols {
            for cell in &mut cells[idx..idx + width] {
                blank_in_place(cell);
            }
        }
        idx += width;
    }
}

fn blank_in_place(cell: &mut ScreenCell) {
    cell.ch = ' ';
    cell.clear_combining();
    cell.width = 1;
}

/// Whether every wide character in `cells` has its tails and no tail is an
/// orphan. Clears stray wrap flags on the way, since this line is about to
/// stand as one row.
fn line_is_sound(cells: &mut [ScreenCell]) -> bool {
    let mut expect_tails = 0usize;
    for cell in cells.iter_mut() {
        cell.wrapped = false;
        match cell.width {
            0 => {
                if expect_tails == 0 {
                    return false;
                }
                expect_tails -= 1;
            }
            width => {
                if expect_tails > 0 {
                    return false;
                }
                expect_tails = usize::from(width) - 1;
            }
        }
    }
    expect_tails == 0
}

/// Permission for `reflow` to skip lines the scrollback limit is certain to
/// evict: everything that ends more than `budget` rows above the cursor's
/// line, as long as it also ends before old row `protect_from` (the live
/// lines are never skipped).
#[derive(Clone, Copy, Debug)]
pub(super) struct Evictable {
    pub(super) protect_from: usize,
    pub(super) budget: usize,
}

/// How many old rows from the front can be skipped. Counts rows with a lower
/// bound, so a line is only skipped when the rows after it certainly fill
/// the budget.
fn rows_to_skip(rows: &[Vec<ScreenCell>], new_cols: usize, cursor_row: usize, evictable: Evictable) -> usize {
    let is_wrapped = |row: &Vec<ScreenCell>| row.last().is_some_and(|cell| cell.wrapped);
    let mut end = cursor_row.min(rows.len());
    while end > 0 && is_wrapped(&rows[end - 1]) {
        end -= 1;
    }
    let mut after = 0usize;
    while end > 0 {
        if after >= evictable.budget && end <= evictable.protect_from {
            return end;
        }
        let mut start = end - 1;
        while start > 0 && is_wrapped(&rows[start - 1]) {
            start -= 1;
        }
        after += rows_lower_bound(&rows[start..end], new_cols);
        end = start;
    }
    0
}

/// At least how many rows one logical line re-wraps into.
fn rows_lower_bound(line: &[Vec<ScreenCell>], new_cols: usize) -> usize {
    let cells: usize = line.iter().map(Vec::len).sum();
    let mut trim = 0usize;
    'scan: for row in line.iter().rev() {
        for cell in row.iter().rev() {
            if !is_trim_blank(cell) {
                break 'scan;
            }
            trim += 1;
        }
    }
    // Each wrapped row may give up one spacer cell.
    let kept = cells.saturating_sub(trim).saturating_sub(line.len().saturating_sub(1));
    kept.div_ceil(new_cols).max(1)
}