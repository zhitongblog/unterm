use super::DirtyRows;

/// Change tracking for delta readers (`read_render_frame`).
///
/// Rows are marked in *live-line* coordinates -- the index `cursor_y` uses --
/// because that is what every mutation knows. A reader wants *viewport*
/// rows, and the two differ whenever the viewport does not start at the
/// first live line: a pane that grew shows scrollback above its live lines,
/// and a scrolled-back viewport starts higher still. So the tracker also
/// remembers where live line 0 sat in the viewport when tracking began
/// (`live_offset`); the screen converts with it, and if it has moved since,
/// every row moved with it and the whole viewport is dirty.
#[derive(Default)]
pub(super) struct RenderState {
    revision: u64,
    dirty_rows: Option<DirtyRows>,
    dirty_since_revision: Option<u64>,
    /// Everything is dirty, whatever the coordinates.
    all_dirty: bool,
    /// Viewport row of live line 0 when the dirty set was last cleared.
    live_offset: isize,
}

impl RenderState {
    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn set_revision(&mut self, revision: u64) {
        self.revision = revision;
    }

    /// Dirty rows in live-line coordinates.
    pub(super) fn dirty_rows(&self) -> Option<DirtyRows> {
        self.dirty_rows
    }

    pub(super) fn all_dirty(&self) -> bool {
        self.all_dirty
    }

    pub(super) fn live_offset(&self) -> isize {
        self.live_offset
    }

    pub(super) fn can_render_delta_since(&self, since_revision: u64) -> bool {
        self.dirty_since_revision
            .is_some_and(|dirty_since| dirty_since <= since_revision)
    }

    pub(super) fn bump_revision(&mut self) {
        let previous = self.revision;
        if self.dirty_since_revision.is_none() {
            self.dirty_since_revision = Some(previous);
        }
        self.revision = self.revision.wrapping_add(1).max(1);
    }

    /// Start a new dirty set; `live_offset` is where live line 0 now sits in
    /// the viewport.
    pub(super) fn clear_dirty_rows(&mut self, live_offset: isize) {
        self.dirty_rows = None;
        self.dirty_since_revision = None;
        self.all_dirty = false;
        self.live_offset = live_offset;
    }

    pub(super) fn mark_dirty_row(&mut self, row: usize, rows: usize) {
        if rows == 0 {
            return;
        }
        let row = row.min(rows.saturating_sub(1));
        self.dirty_rows = Some(match self.dirty_rows {
            Some(dirty) => DirtyRows {
                start: dirty.start.min(row),
                end: dirty.end.max(row),
            },
            None => DirtyRows {
                start: row,
                end: row,
            },
        });
    }

    pub(super) fn mark_dirty_range(&mut self, start: usize, end: usize, rows: usize) {
        if rows == 0 {
            return;
        }
        let start = start.min(rows - 1);
        let end = end.min(rows - 1);
        if start <= end {
            self.dirty_rows = Some(match self.dirty_rows {
                Some(dirty) => DirtyRows {
                    start: dirty.start.min(start),
                    end: dirty.end.max(end),
                },
                None => DirtyRows { start, end },
            });
        }
    }

    pub(super) fn mark_all_dirty(&mut self, rows: usize) {
        if rows > 0 {
            self.mark_dirty_range(0, rows - 1, rows);
            self.all_dirty = true;
        }
    }
}
