use super::cell::ScreenCell;
use std::collections::VecDeque;

/// What a scrollback push cost, and what it gave back.
#[derive(Default)]
pub(super) struct ScrollbackPush {
    /// Rows that fell off the top, so prompt-row bookkeeping can follow them.
    pub(super) trimmed: usize,
    /// The last evicted row's buffer, emptied and ready to be filled again.
    pub(super) recycled: Option<Vec<ScreenCell>>,
}

#[derive(Default)]
pub(super) struct HistoryBuffer {
    /// A deque, not a `Vec`, because a full scrollback loses its oldest line
    /// for every new one.
    ///
    /// Dropping the front of a `Vec` shifts everything behind it: at the
    /// default 10,000-line limit that was a ~240 KB memmove per line of
    /// output, paid on every line forever once the buffer filled. It was 83%
    /// of the PTY reader thread on a flood, and -- worse -- it meant a
    /// terminal got permanently slower the longer a session ran. Both ends of
    /// a deque are O(1).
    scrollback: VecDeque<Vec<ScreenCell>>,
    viewport_top: Option<usize>,
}

impl HistoryBuffer {
    pub(super) fn scrollback_rows(&self) -> usize {
        self.scrollback.len()
    }

    pub(super) fn viewport_is_pinned(&self) -> bool {
        self.viewport_top.is_some()
    }

    pub(super) fn clear(&mut self) {
        self.scrollback.clear();
        self.viewport_top = None;
    }

    /// Push a row into the scrollback, and hand back a row buffer to reuse.
    ///
    /// A full scrollback evicts a row for every row it takes -- at the very
    /// moment the caller needs a blank row for the bottom of the screen. So
    /// give it the evicted one. In steady state a scrolling terminal then
    /// allocates no rows at all, and the recycled buffer already holds a
    /// screen's worth of cells, so filling it does not climb back up through
    /// half a dozen reallocations.
    pub(super) fn push_scrollback(
        &mut self,
        line: Vec<ScreenCell>,
        max_lines: usize,
    ) -> ScrollbackPush {
        self.scrollback.push_back(line);
        let mut push = ScrollbackPush::default();
        while self.scrollback.len() > max_lines {
            push.recycled = self.evict_oldest();
            push.trimmed += 1;
        }
        push
    }

    fn evict_oldest(&mut self) -> Option<Vec<ScreenCell>> {
        let oldest = self.scrollback.pop_front()?;
        if let Some(top) = self.viewport_top.as_mut() {
            *top = top.saturating_sub(1);
        }
        Some(oldest)
    }

    pub(super) fn extend_scrollback(
        &mut self,
        lines: impl IntoIterator<Item = Vec<ScreenCell>>,
        max_lines: usize,
    ) -> usize {
        self.scrollback.extend(lines);
        self.trim_overflow(max_lines)
    }

    pub(super) fn take_scrollback(&mut self) -> VecDeque<Vec<ScreenCell>> {
        std::mem::take(&mut self.scrollback)
    }

    pub(super) fn replace_scrollback(&mut self, scrollback: VecDeque<Vec<ScreenCell>>) {
        self.scrollback = scrollback;
    }

    pub(super) fn take_viewport_top(&mut self) -> Option<usize> {
        self.viewport_top.take()
    }

    pub(super) fn replace_viewport_top(&mut self, viewport_top: Option<usize>) {
        self.viewport_top = viewport_top;
    }

    pub(super) fn history_len(&self, live_lines: usize) -> usize {
        self.scrollback.len() + live_lines
    }

    pub(super) fn viewport_start(&self, rows: usize, live_lines: usize) -> usize {
        let bottom = self.history_len(live_lines).saturating_sub(rows);
        self.viewport_top
            .map(|top| top.min(bottom))
            .unwrap_or(bottom)
    }

    pub(super) fn set_viewport_top_near(&mut self, target: isize, rows: usize, live_lines: usize) {
        let max_top = self.history_len(live_lines).saturating_sub(rows);
        let target = target.max(0) as usize;
        self.viewport_top = if target >= max_top {
            None
        } else {
            Some(target.saturating_sub(rows / 4).min(max_top))
        };
    }

    /// Move the viewport by `delta` rows, clamped to the scrollback.
    ///
    /// Distinct from `set_viewport_top_near`, which snaps *near* an absolute
    /// target: wheel scrolling needs exact stepping, or every notch drifts.
    /// Reaching the bottom returns to `None` — following the live tail — so a
    /// scrolled-back viewport resumes tracking output instead of freezing one
    /// row short.
    pub(super) fn scroll_viewport_by(&mut self, delta: isize, rows: usize, live_lines: usize) {
        let max_top = self.history_len(live_lines).saturating_sub(rows);
        let current = self.viewport_top.unwrap_or(max_top) as isize;
        let next = (current + delta).clamp(0, max_top as isize) as usize;
        self.viewport_top = if next >= max_top { None } else { Some(next) };
    }

    pub(super) fn history_range<'a>(
        &'a self,
        live_lines: &'a [Vec<ScreenCell>],
        start: usize,
        count: usize,
    ) -> Vec<&'a Vec<ScreenCell>> {
        let end = start
            .saturating_add(count)
            .min(self.history_len(live_lines.len()));
        (start..end)
            .filter_map(|idx| {
                if idx < self.scrollback.len() {
                    self.scrollback.get(idx)
                } else {
                    live_lines.get(idx - self.scrollback.len())
                }
            })
            .collect()
    }

    pub(super) fn history_line<'a>(
        &'a self,
        live_lines: &'a [Vec<ScreenCell>],
        index: usize,
    ) -> Option<&'a Vec<ScreenCell>> {
        if index < self.scrollback.len() {
            self.scrollback.get(index)
        } else {
            live_lines.get(index - self.scrollback.len())
        }
    }

    pub(super) fn scrollback(&self) -> &VecDeque<Vec<ScreenCell>> {
        &self.scrollback
    }

    fn trim_overflow(&mut self, max_lines: usize) -> usize {
        if self.scrollback.len() <= max_lines {
            return 0;
        }

        let overflow = self.scrollback.len() - max_lines;
        self.scrollback.drain(..overflow);
        // A prefix drain on a deque moves the head, not the elements behind
        // it -- the whole reason this type changed.
        if let Some(top) = self.viewport_top.as_mut() {
            *top = top.saturating_sub(overflow);
        }
        overflow
    }
}
