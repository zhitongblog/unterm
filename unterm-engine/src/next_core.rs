use super::{
    CreateSessionRequest, CursorSnapshot, DirtyRows, EngineHealthSnapshot, HealthEngine,
    InputEngine, ProcessTreeSnapshot, RecordingEngine, RecordingExportResult, RecordingStartResult,
    RecordingStatusSnapshot, RecordingStopResult, RenderFrameSnapshot, ScreenEngine, ScreenLine,
    ScreenSearchMatch, ScreenSnapshot, ScrollbackTextRequest, ScrollbackTextSnapshot,
    SessionActivitySnapshot, SessionEngine, SessionSnapshot, ShellSnapshot, SplitSessionRequest,
    StyledBlink, StyledScreenLine, StyledScreenSnapshot, StyledScrollbackSnapshot, StyledUnderline,
};
use anyhow::Result;
use parking_lot::Mutex;
use portable_pty::{Child, MasterPty};
use std::collections::BTreeSet;
use std::convert::TryFrom;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
#[cfg(test)]
use std::sync::OnceLock;
#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

mod activity;
mod cell;
pub mod color;
pub mod config;
pub mod config_migrate;
pub mod config_schema;
mod csi_params;
pub mod font_discovery;
pub mod font_raster;
pub mod font_shaper;
mod health_snapshot;
mod history;
mod input_dispatch;
mod input_pipeline;
pub mod key_encoding;
pub(crate) mod launch;
pub mod layout;
mod lifecycle;
pub mod mouse_encoding;
mod osc133;
mod osc_params;
mod parser_state;
mod process_tree;
mod pty_io;
mod recording_archive;
mod recording_lifecycle;
mod recording_markdown;
mod recording_output;
mod recording_text;
mod render_frame;
mod render_state;
mod runtime;
mod screen_dispatch;
mod screen_search;
mod screen_snapshot;
mod screen_state;
mod screen_text;
pub mod selection;
mod session_activity;
mod session_creation;
mod session_defaults;
mod session_handles;
mod session_output;
mod session_queries;
mod session_registry;
mod session_runtime;
mod session_snapshots;
mod sgr;
mod styled_snapshot;
pub mod tab_title;
pub mod tabs;
mod terminal_parser;
mod terminal_queries;
#[cfg(test)]
mod test_support;

#[cfg(test)]
use super::{LaunchEnvSource, LaunchPolicyDecision};
#[cfg(test)]
use crate::{CellStyle, StyledVerticalAlign};
use activity::SessionIoActivity;
#[cfg(test)]
use cell::TerminalColor;
use cell::{CellAttributes, ScreenCell};
use history::HistoryBuffer;
use render_state::RenderState;
use screen_state::{MouseTrackingMode, ScreenState};
use terminal_parser::TerminalParser;
#[cfg(test)]
use test_support::{
    mark_dead_for_test, reset_activity_for_test, reset_state_for_test, screen_for_test,
    set_output_for_test, settle_session_for_test, viewport_attrs_for_test,
};

const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_RECORDING_BLOCKS: usize = 256;
pub const DEFAULT_SCROLLBACK_LINES: usize = 10_000;
static NEW_SESSION_SCROLLBACK_LINES: AtomicUsize = AtomicUsize::new(DEFAULT_SCROLLBACK_LINES);

#[derive(Clone, Copy, Debug, Default)]
pub struct NextCoreEngine;

struct NextCoreSession {
    snapshot: SessionSnapshot,
    root_pid: Option<u32>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    output: Arc<Mutex<String>>,
    screen: Arc<Mutex<NextCoreScreen>>,
    recording: Arc<Mutex<Option<NextCoreRecording>>>,
    activity: Arc<Mutex<SessionIoActivity>>,
    dead: Arc<AtomicBool>,
    dead_reason: Arc<Mutex<Option<String>>>,
}

impl Drop for NextCoreSession {
    fn drop(&mut self) {
        self.child.lock().kill().ok();
    }
}

#[derive(Clone, Debug)]
struct NextCoreRecording {
    session_id: String,
    pane_id: usize,
    project_path: Option<String>,
    project_slug: String,
    started_at: String,
    log_path: PathBuf,
    md_path: PathBuf,
    bytes_raw: u64,
    block_count: u64,
    trace_ids: Vec<String>,
    text_preview: String,
    blocks: Vec<NextCoreRecordingBlock>,
    osc133_seen: bool,
    command_blocks: Vec<NextCoreCommandBlock>,
    active_command: Option<NextCoreActiveCommand>,
}

#[derive(Clone, Debug)]
struct NextCoreRecordingBlock {
    index: u64,
    timestamp_micros: u128,
    text: String,
}

#[derive(Clone, Debug)]
struct NextCoreCommandBlock {
    index: u64,
    started_micros: u128,
    ended_micros: Option<u128>,
    exit_code: Option<String>,
    text: String,
}

#[derive(Clone, Debug)]
struct NextCoreActiveCommand {
    index: u64,
    started_micros: u128,
    text: String,
}

#[derive(Default)]
struct NextCoreScreen {
    cols: usize,
    /// The history capacity chosen when this pane was created.
    scrollback_limit: usize,
    /// Absolute history rows at which OSC 133 prompt-start markers arrived.
    prompt_rows: Vec<usize>,
    history: HistoryBuffer,
    lines: Vec<Vec<ScreenCell>>,
    cursor_x: usize,
    cursor_y: usize,
    cursor_visible: bool,
    /// How many times a program has rung the bell.
    bells: u64,
    /// Notifications a program raised (`OSC 9`/`777`), as a running count and
    /// the newest text — a front end compares the count between frames.
    notifications: u64,
    last_notification: Option<String>,
    /// Text a program asked to put on the system clipboard.
    clipboard_request: Option<String>,
    cursor_blinking: bool,
    cursor_shape: String,
    column_132_mode: bool,
    auto_wrap: bool,
    reverse_video: bool,
    application_cursor_keys: bool,
    application_keypad: bool,
    focus_event_reporting: bool,
    mouse_tracking: MouseTrackingMode,
    utf8_mouse: bool,
    urxvt_mouse: bool,
    sgr_mouse: bool,
    alternate_scroll: bool,
    sgr_pixel_mouse: bool,
    meta_sends_escape: bool,
    synchronized_output: bool,
    alternate_screen_modes: BTreeSet<usize>,
    origin_mode: bool,
    insert_mode: bool,
    left_right_margin_mode: bool,
    tab_stops: BTreeSet<usize>,
    bracketed_paste: bool,
    current_attr: CellAttributes,
    hyperlinks: Vec<String>,
    title: Option<String>,
    title_stack: Vec<Option<String>>,
    current_dir: Option<String>,
    render_state: RenderState,
    rows: usize,
    scroll_top: usize,
    scroll_bottom: usize,
    left_margin: usize,
    right_margin: usize,
    saved_cursor_x: usize,
    saved_cursor_y: usize,
    saved_cursor_attr: CellAttributes,
    alternate: Option<ScreenState>,
    parser: TerminalParser,
}

impl NextCoreScreen {
    /// The terminal modes a GUI pane surfaces.
    ///
    /// `alternate_screen_modes` holds every alt-screen mode the application
    /// turned on (47/1047/1049), so a non-empty set means the alternate
    /// screen is up.
    pub(super) fn pane_modes(&self) -> crate::PaneModesSnapshot {
        crate::PaneModesSnapshot {
            mouse_grabbed: self.mouse_tracking != screen_state::MouseTrackingMode::None,
            alt_screen_active: !self.alternate_screen_modes.is_empty(),
            bracketed_paste: self.bracketed_paste,
            application_cursor_keys: self.application_cursor_keys,
        }
    }

    /// A program rang the bell.
    ///
    /// Counted rather than flagged: a front end reads the screen when it
    /// draws, and a flag would be missed whenever two bells landed between
    /// two frames -- or shown twice if the reader forgot to clear it. A
    /// number that only goes up is one both sides can compare.
    fn ring_bell(&mut self) {
        self.bells = self.bells.saturating_add(1);
        self.bump_revision();
    }

    /// The session's mouse reporting state, as the encoder needs it.
    pub fn mouse_modes(&self) -> mouse_encoding::MouseModes {
        use mouse_encoding::MouseTracking;
        mouse_encoding::MouseModes {
            tracking: match self.mouse_tracking {
                screen_state::MouseTrackingMode::None => MouseTracking::None,
                screen_state::MouseTrackingMode::ButtonEvent => MouseTracking::ButtonEvent,
                screen_state::MouseTrackingMode::ButtonMotion => MouseTracking::ButtonMotion,
                screen_state::MouseTrackingMode::AnyEvent => MouseTracking::AnyEvent,
            },
            sgr: self.sgr_mouse,
            urxvt: self.urxvt_mouse,
            utf8: self.utf8_mouse,
        }
    }

    fn new(cols: usize, rows: usize) -> Self {
        Self::new_with_scrollback_limit(
            cols,
            rows,
            NEW_SESSION_SCROLLBACK_LINES.load(Ordering::Relaxed),
        )
    }

    fn new_with_scrollback_limit(cols: usize, rows: usize, scrollback_limit: usize) -> Self {
        let mut screen = Self {
            cols: cols.max(1),
            rows: rows.max(1),
            scrollback_limit,
            cursor_visible: true,
            bells: 0,
            notifications: 0,
            last_notification: None,
            clipboard_request: None,
            cursor_blinking: true,
            cursor_shape: "Default".to_string(),
            auto_wrap: true,
            ..Self::default()
        };
        screen.tab_stops = Self::default_tab_stops(screen.cols);
        screen.scroll_bottom = screen.rows - 1;
        screen.right_margin = screen.cols - 1;
        screen.ensure_cursor_line();
        screen
    }

    fn feed(&mut self, chunk: &str) {
        if !chunk.is_empty() {
            self.bump_revision();
        }
        let mut parser = std::mem::take(&mut self.parser);
        parser.feed(self, chunk);
        self.parser = parser;
    }

    fn bump_revision(&mut self) {
        self.render_state.bump_revision();
    }

    fn clear_dirty_rows(&mut self) {
        self.render_state.clear_dirty_rows();
    }

    fn mark_dirty_row(&mut self, row: usize) {
        self.render_state.mark_dirty_row(row, self.rows);
    }

    fn mark_dirty_range(&mut self, start: usize, end: usize) {
        self.render_state.mark_dirty_range(start, end, self.rows);
    }

    fn mark_all_dirty(&mut self) {
        self.render_state.mark_all_dirty(self.rows);
    }

    fn snapshot_viewport_lines(&self) -> Vec<String> {
        self.history_text_range(self.viewport_start(), self.rows)
    }

    #[allow(dead_code)]
    fn styled_viewport_lines(&self, first_row: i64) -> Vec<StyledScreenLine> {
        let viewport_start = self.viewport_start();
        styled_snapshot::viewport_lines(
            self.rows,
            first_row,
            (0..self.rows).map(|idx| self.history_line(viewport_start + idx)),
            self.cols,
            self.reverse_video,
            &self.hyperlinks,
        )
    }

    fn styled_viewport_dirty_lines(
        &self,
        dirty_rows: DirtyRows,
        first_row: i64,
    ) -> Vec<StyledScreenLine> {
        let viewport_start = self.viewport_start();
        styled_snapshot::viewport_dirty_lines(
            first_row,
            (dirty_rows.start..=dirty_rows.end)
                .map(|row| (row, self.history_line(viewport_start + row))),
            self.cols,
            self.reverse_video,
            &self.hyperlinks,
        )
    }

    fn styled_history_range(&self, start: usize, count: usize) -> Vec<StyledScreenLine> {
        styled_snapshot::history_range(
            start,
            self.history_range(start, count),
            self.reverse_video,
            &self.hyperlinks,
        )
    }

    fn scrollback_rows(&self) -> usize {
        self.history.scrollback_rows()
    }

    fn revision(&self) -> u64 {
        self.render_state.revision()
    }

    fn dirty_rows(&self) -> Option<DirtyRows> {
        self.render_state.dirty_rows()
    }

    fn can_render_delta_since(&self, since_revision: u64) -> bool {
        self.render_state.can_render_delta_since(since_revision)
    }

    fn cursor_snapshot(&self) -> CursorSnapshot {
        // In the viewport's coordinates, which is what the lines are in.
        //
        // `cursor_y` indexes the live lines, but the viewport does not always
        // begin there: a screen that has grown fills the rows above the live
        // lines from scrollback, and a screen scrolled back starts higher
        // still. Reporting the raw index leaves the cursor floating a row
        // above the prompt it is on, and leaves it sitting in the middle of
        // old output when the user scrolls back instead of moving off-screen
        // with the text it belongs to.
        let live_start = self.history_len().saturating_sub(self.lines.len()) as isize;
        CursorSnapshot {
            x: self.cursor_x,
            y: live_start + self.cursor_y as isize - self.viewport_start() as isize,
            visible: self.cursor_visible,
            shape: self.cursor_shape.clone(),
        }
    }

    fn title(&self) -> Option<String> {
        self.title.clone()
    }

    fn current_dir(&self) -> Option<String> {
        self.current_dir.clone()
    }

    fn history_len(&self) -> usize {
        self.history.history_len(self.lines.len())
    }

    fn viewport_start(&self) -> usize {
        self.history.viewport_start(self.rows, self.lines.len())
    }

    fn viewport_first_row(&self) -> i64 {
        self.viewport_start() as i64
    }

    fn set_viewport_top_near(&mut self, target: isize) {
        self.history
            .set_viewport_top_near(target, self.rows, self.lines.len());
        self.bump_revision();
        self.mark_all_dirty();
    }

    fn scroll_viewport_by(&mut self, delta: isize) {
        self.history
            .scroll_viewport_by(delta, self.rows, self.lines.len());
        self.bump_revision();
        self.mark_all_dirty();
    }

    fn scroll_viewport_to_prompt(&mut self, amount: isize) -> bool {
        if amount == 0 || self.alternate.is_some() || self.prompt_rows.is_empty() {
            return false;
        }
        let anchor = self.viewport_start().saturating_add(self.rows / 4);
        let target = if amount < 0 {
            let before = self.prompt_rows.partition_point(|row| *row < anchor);
            before.checked_sub(amount.unsigned_abs())
        } else {
            let after = self.prompt_rows.partition_point(|row| *row <= anchor);
            after.checked_add(amount as usize - 1)
        }
        .and_then(|index| self.prompt_rows.get(index))
        .copied();
        if let Some(row) = target {
            self.set_viewport_top_near(row as isize);
            true
        } else {
            false
        }
    }

    fn trim_prompt_rows(&mut self, removed: usize) {
        if removed == 0 {
            return;
        }
        self.prompt_rows.retain(|row| *row >= removed);
        self.prompt_rows.iter_mut().for_each(|row| *row -= removed);
    }

    fn history_range(&self, start: usize, count: usize) -> Vec<&Vec<ScreenCell>> {
        self.history.history_range(&self.lines, start, count)
    }

    fn history_line(&self, index: usize) -> Option<&Vec<ScreenCell>> {
        self.history.history_line(&self.lines, index)
    }

    fn history_text_range(&self, start: usize, count: usize) -> Vec<String> {
        self.history_range(start, count)
            .into_iter()
            .map(Self::line_text)
            .collect()
    }

    fn line_text(line: &Vec<ScreenCell>) -> String {
        let mut text = String::new();
        for cell in line.iter().filter(|cell| cell.width > 0) {
            text.push(cell.ch);
            text.push_str(cell.combining());
        }
        text.trim_end().to_string()
    }

    fn put_char(&mut self, c: char) {
        self.ensure_cursor_line();
        self.mark_dirty_row(self.cursor_y);
        {
            let line = &mut self.lines[self.cursor_y];
            let end = self.cursor_x.min(line.len());
            if end > 0 {
                if let Some(previous) = line[..end]
                    .iter_mut()
                    .rev()
                    .find(|cell| cell.width > 0 && cell.joins_trailing_cluster_char(c))
                {
                    previous.push_combining(c);
                    return;
                }
            }
        }
        let cell = ScreenCell::new(c, self.current_attr);
        if cell.width == 0 {
            let line = &mut self.lines[self.cursor_y];
            let end = self.cursor_x.min(line.len());
            if end > 0 {
                if let Some(previous) = line[..end].iter_mut().rev().find(|cell| cell.width > 0) {
                    previous.push_combining(c);
                }
            }
            return;
        }
        self.put_cell(cell);
    }

    /// Flag `row` as soft-wrapping into the next one.
    ///
    /// A row that has not reached the right margin yet still needs the flag,
    /// so pad to the margin first: the marker lives on the last cell.
    fn mark_row_wrapped(&mut self, row: usize) {
        let attr = self.current_attr.clone();
        let cols = self.cols;
        let Some(line) = self.lines.get_mut(row) else {
            return;
        };
        if line.is_empty() {
            line.push(ScreenCell::blank(attr));
        } else if line.len() < cols {
            line.resize(cols, ScreenCell::blank(attr));
        }
        if let Some(last) = line.last_mut() {
            last.wrapped = true;
        }
    }

    fn put_cell(&mut self, cell: ScreenCell) {
        let width = usize::from(cell.width);
        let attr = cell.attr;
        let left_margin = self.active_left_margin();
        let right_margin = self.active_right_margin();
        if self.cursor_x > right_margin || self.cursor_x + width - 1 > right_margin {
            if self.auto_wrap {
                // Record that this row continues into the next one before
                // leaving it. Without this a soft-wrapped line reads back as
                // several independent lines, so copying a wrapped command
                // inserts a newline that the user never typed.
                self.mark_row_wrapped(self.cursor_y);
                self.newline();
                self.cursor_x = left_margin;
                self.ensure_cursor_line();
                self.mark_dirty_row(self.cursor_y);
            } else {
                self.cursor_x = right_margin.saturating_sub(width.saturating_sub(1));
            }
        }
        {
            let line = &mut self.lines[self.cursor_y];
            if self.cursor_x > line.len() {
                line.resize(
                    self.cursor_x.min(self.cols),
                    ScreenCell::blank(self.current_attr),
                );
            }
            if self.insert_mode && self.cursor_x < self.cols {
                for _ in 0..width {
                    line.insert(self.cursor_x, ScreenCell::blank(self.current_attr));
                }
                if line.len() > self.cols {
                    line.truncate(self.cols);
                }
                // Insert mode is `ESC[@` arriving one keystroke at a time, and
                // it slides the row for every character typed. Unlike `ESC[@`
                // the slide is always one uninterrupted block, so it can only
                // cut a wide character at its two seams -- the column the
                // blanks went into, and the tail the truncation took off --
                // and this sits on the per-character path, so it repairs those
                // two rather than sweeping the whole row.
                Self::repair_wide_pairs(line, self.cursor_x, self.cursor_x + width.max(1) - 1);
                let tail = line.len().saturating_sub(1);
                Self::repair_wide_pairs(line, tail, tail);
            }
            // Whatever this write lands on, it must not leave half of a wide
            // character behind.
            for offset in 0..width.max(1) {
                Self::split_wide_cell(line, self.cursor_x + offset);
            }
            if self.cursor_x == line.len() {
                line.push(cell);
            } else if self.cursor_x < self.cols {
                line[self.cursor_x] = cell;
            }
            if width > 1 {
                for offset in 1..width {
                    let idx = self.cursor_x + offset;
                    if idx >= self.cols {
                        break;
                    }
                    if idx == line.len() {
                        line.push(ScreenCell::continuation(attr));
                    } else if idx < line.len() {
                        line[idx] = ScreenCell::continuation(attr);
                    }
                }
            }
            if line.len() > self.cols {
                line.truncate(self.cols);
            }
        }
        self.cursor_x += width;
    }

    fn repeat_previous_char(&mut self, count: usize) {
        self.ensure_cursor_line();
        let end = self.cursor_x.min(self.lines[self.cursor_y].len());
        let Some(cell) = self.lines[self.cursor_y][..end]
            .iter()
            .rev()
            .find(|cell| cell.width > 0)
            .cloned()
        else {
            return;
        };
        for _ in 0..count.max(1) {
            self.mark_dirty_row(self.cursor_y);
            self.put_cell(cell.clone());
        }
    }

    fn newline(&mut self) {
        let old_y = self.cursor_y;
        self.cursor_x = 0;
        self.index();
        self.mark_dirty_row(old_y);
    }

    fn index(&mut self) {
        let old_y = self.cursor_y;
        if self.cursor_y == self.scroll_bottom {
            self.scroll_up_region(self.scroll_top, self.scroll_bottom, 1);
        } else if self.cursor_y + 1 < self.rows {
            self.cursor_y += 1;
            self.mark_dirty_row(old_y);
            self.mark_dirty_row(self.cursor_y);
        }
        self.ensure_cursor_line();
    }

    fn next_line(&mut self) {
        self.cursor_x = 0;
        self.index();
    }

    fn reverse_index(&mut self) {
        let old_y = self.cursor_y;
        if self.cursor_y == self.scroll_top {
            self.scroll_down_region(self.scroll_top, self.scroll_bottom, 1);
        } else if self.cursor_y > 0 {
            self.cursor_y = self.cursor_y.saturating_sub(1);
            self.mark_dirty_row(old_y);
            self.mark_dirty_row(self.cursor_y);
        }
        self.ensure_cursor_line();
    }

    fn carriage_return(&mut self) {
        self.mark_dirty_row(self.cursor_y);
        self.cursor_x = 0;
    }

    fn backspace(&mut self) {
        self.mark_dirty_row(self.cursor_y);
        self.cursor_x = self.cursor_x.saturating_sub(1);
    }

    fn horizontal_tab(&mut self) {
        self.cursor_forward_tab(1);
    }

    fn cursor_forward_tab(&mut self, count: usize) {
        self.mark_dirty_row(self.cursor_y);
        for _ in 0..count.max(1) {
            let next_tab = self
                .tab_stops
                .range((self.cursor_x + 1)..)
                .next()
                .copied()
                .unwrap_or_else(|| self.cols.saturating_sub(1));
            self.cursor_x = next_tab.min(self.cols.saturating_sub(1));
        }
    }

    fn reverse_horizontal_tab(&mut self, count: usize) {
        self.mark_dirty_row(self.cursor_y);
        for _ in 0..count.max(1) {
            let previous = self
                .tab_stops
                .range(..self.cursor_x)
                .next_back()
                .copied()
                .unwrap_or(0);
            self.cursor_x = previous;
        }
    }

    fn set_tab_stop(&mut self) {
        if self.cursor_x < self.cols {
            self.tab_stops.insert(self.cursor_x);
        }
    }

    fn clear_tab_stop(&mut self, mode: usize) {
        match mode {
            0 => {
                self.tab_stops.remove(&self.cursor_x);
            }
            3 => self.tab_stops.clear(),
            _ => {}
        }
    }

    fn save_cursor(&mut self) {
        self.saved_cursor_x = self.cursor_x;
        self.saved_cursor_y = self.cursor_y;
        self.saved_cursor_attr = self.current_attr;
    }

    fn restore_cursor(&mut self) {
        self.mark_dirty_row(self.cursor_y);
        self.cursor_x = self.saved_cursor_x;
        self.cursor_y = self.saved_cursor_y;
        self.current_attr = self.saved_cursor_attr;
        self.ensure_cursor_line();
        self.mark_dirty_row(self.cursor_y);
    }

    fn set_cursor_shape(&mut self, shape: usize) {
        self.cursor_shape = match shape {
            1 => "BlinkingBlock",
            2 => "SteadyBlock",
            3 => "BlinkingUnderline",
            4 => "SteadyUnderline",
            5 => "BlinkingBar",
            6 => "SteadyBar",
            _ => "Default",
        }
        .to_string();
        self.mark_dirty_row(self.cursor_y);
    }

    fn push_title(&mut self) {
        self.title_stack.push(self.title.clone());
    }

    fn pop_title(&mut self) {
        if let Some(title) = self.title_stack.pop() {
            self.title = title;
        }
    }

    fn set_bracketed_paste(&mut self, enabled: bool) {
        self.bracketed_paste = enabled;
    }

    fn active_left_margin(&self) -> usize {
        if self.left_right_margin_mode {
            self.left_margin.min(self.cols.saturating_sub(1))
        } else {
            0
        }
    }

    fn active_right_margin(&self) -> usize {
        if self.left_right_margin_mode {
            self.right_margin
                .min(self.cols.saturating_sub(1))
                .max(self.active_left_margin())
        } else {
            self.cols.saturating_sub(1)
        }
    }

    fn active_top_margin(&self) -> usize {
        if self.origin_mode {
            self.scroll_top.min(self.rows.saturating_sub(1))
        } else {
            0
        }
    }

    fn active_bottom_margin(&self) -> usize {
        if self.origin_mode {
            self.scroll_bottom
                .min(self.rows.saturating_sub(1))
                .max(self.active_top_margin())
        } else {
            self.rows.saturating_sub(1)
        }
    }

    fn set_origin_mode(&mut self, enabled: bool) {
        self.origin_mode = enabled;
        self.set_cursor_position(0, 0);
    }

    fn ensure_cursor_line(&mut self) {
        while self.lines.len() <= self.cursor_y {
            self.lines.push(Vec::new());
        }
    }

    fn ensure_rows_through(&mut self, row: usize) {
        while self.lines.len() <= row {
            self.lines.push(Vec::new());
        }
    }

    fn set_cursor(&mut self, row: usize, col: usize) {
        self.mark_dirty_row(self.cursor_y);
        self.cursor_y = row.min(self.rows.saturating_sub(1));
        self.cursor_x = col
            .max(self.active_left_margin())
            .min(self.active_right_margin());
        self.ensure_cursor_line();
        self.mark_dirty_row(self.cursor_y);
    }

    fn set_cursor_position(&mut self, row: usize, col: usize) {
        let row = if self.origin_mode {
            self.scroll_top
                .saturating_add(row)
                .min(self.scroll_bottom)
                .min(self.rows.saturating_sub(1))
        } else {
            row.min(self.rows.saturating_sub(1))
        };
        let col = if self.origin_mode && self.left_right_margin_mode {
            self.active_left_margin().saturating_add(col)
        } else {
            col
        };
        self.set_cursor(row, col);
    }

    fn move_cursor_up(&mut self, count: usize) {
        self.mark_dirty_row(self.cursor_y);
        self.cursor_y = self
            .cursor_y
            .saturating_sub(count)
            .max(self.active_top_margin());
        self.mark_dirty_row(self.cursor_y);
    }

    fn move_cursor_down(&mut self, count: usize) {
        self.mark_dirty_row(self.cursor_y);
        self.cursor_y = (self.cursor_y + count).min(self.active_bottom_margin());
        self.ensure_cursor_line();
        self.mark_dirty_row(self.cursor_y);
    }

    fn move_cursor_right(&mut self, count: usize) {
        self.mark_dirty_row(self.cursor_y);
        self.cursor_x = (self.cursor_x + count).min(self.active_right_margin());
    }

    fn move_cursor_left(&mut self, count: usize) {
        self.mark_dirty_row(self.cursor_y);
        self.cursor_x = self
            .cursor_x
            .saturating_sub(count)
            .max(self.active_left_margin());
    }

    fn cursor_next_line(&mut self, count: usize) {
        self.move_cursor_down(count.max(1));
        self.cursor_x = self.active_left_margin();
    }

    fn cursor_previous_line(&mut self, count: usize) {
        self.move_cursor_up(count.max(1));
        self.cursor_x = self.active_left_margin();
    }

    fn set_horizontal_position(&mut self, col: usize) {
        let col = if self.origin_mode && self.left_right_margin_mode {
            self.active_left_margin().saturating_add(col)
        } else {
            col
        };
        self.set_cursor(self.cursor_y, col);
    }

    fn set_vertical_position(&mut self, row: usize) {
        self.set_cursor_position(row, self.cursor_x);
    }

    fn clear_screen(&mut self) {
        self.lines.clear();
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.ensure_cursor_line();
        self.mark_all_dirty();
    }

    /// Drop the scrollback, and optionally the visible screen with it.
    ///
    /// This is the terminal-side "clear scrollback" action, not the `CSI 3 J`
    /// the application can send: the user asked, so the cursor row is kept
    /// where it is rather than homed.
    pub(super) fn erase_scrollback(&mut self, include_viewport: bool) {
        let removed = self.history.scrollback_rows();
        self.history.clear();
        if include_viewport {
            self.prompt_rows.clear();
            self.clear_display();
        } else {
            self.trim_prompt_rows(removed);
        }
        self.bump_revision();
        self.mark_all_dirty();
    }

    fn clear_display(&mut self) {
        self.lines.clear();
        self.ensure_cursor_line();
        self.mark_all_dirty();
    }

    fn fill_alignment_test(&mut self) {
        self.lines = (0..self.rows)
            .map(|_| vec![ScreenCell::new('E', self.current_attr); self.cols])
            .collect();
        self.mark_all_dirty();
    }

    fn fill_rect(&mut self, ch: char, top: usize, left: usize, bottom: usize, right: usize) {
        if self.rows == 0 || self.cols == 0 {
            return;
        }
        let Some((top, left, bottom, right)) = self.clip_rect(top, left, bottom, right) else {
            return;
        };

        self.ensure_rows_through(bottom);
        let cell = ScreenCell::new(ch, self.current_attr);
        for row in top..=bottom {
            let line = &mut self.lines[row];
            if line.len() <= right {
                line.resize(right + 1, ScreenCell::blank(self.current_attr));
            }
            for col in left..=right {
                line[col] = cell.clone();
            }
        }
        self.mark_dirty_range(top, bottom);
    }

    fn erase_rect(&mut self, top: usize, left: usize, bottom: usize, right: usize) {
        if self.rows == 0 || self.cols == 0 {
            return;
        }
        let Some((top, left, bottom, right)) = self.clip_rect(top, left, bottom, right) else {
            return;
        };

        self.ensure_rows_through(bottom);
        let blank = ScreenCell::blank(self.current_attr);
        for row in top..=bottom {
            let line = &mut self.lines[row];
            if line.len() <= right {
                line.resize(right + 1, ScreenCell::blank(self.current_attr));
            }
            for col in left..=right {
                line[col] = blank.clone();
            }
        }
        self.mark_dirty_range(top, bottom);
    }

    fn selective_erase_rect(&mut self, top: usize, left: usize, bottom: usize, right: usize) {
        if self.rows == 0 || self.cols == 0 {
            return;
        }
        let Some((top, left, bottom, right)) = self.clip_rect(top, left, bottom, right) else {
            return;
        };

        self.ensure_rows_through(bottom);
        let blank = ScreenCell::blank(self.current_attr);
        for row in top..=bottom {
            let line = &mut self.lines[row];
            if line.len() <= right {
                line.resize(right + 1, ScreenCell::blank(self.current_attr));
            }
            for col in left..=right {
                if !line[col].attr.protected {
                    line[col] = blank.clone();
                }
            }
        }
        self.mark_dirty_range(top, bottom);
    }

    fn change_rect_attributes(
        &mut self,
        top: usize,
        left: usize,
        bottom: usize,
        right: usize,
        params: &[usize],
    ) {
        if self.rows == 0 || self.cols == 0 {
            return;
        }
        let Some((top, left, bottom, right)) = self.clip_rect(top, left, bottom, right) else {
            return;
        };

        self.ensure_rows_through(bottom);
        let params = if params.is_empty() { &[0][..] } else { params };
        for row in top..=bottom {
            let line = &mut self.lines[row];
            if line.len() <= right {
                line.resize(right + 1, ScreenCell::blank(self.current_attr));
            }
            for col in left..=right {
                Self::apply_deccara_attributes(&mut line[col].attr, params);
            }
        }
        self.mark_dirty_range(top, bottom);
    }

    fn reverse_rect_attributes(
        &mut self,
        top: usize,
        left: usize,
        bottom: usize,
        right: usize,
        params: &[usize],
    ) {
        if self.rows == 0 || self.cols == 0 {
            return;
        }
        let Some((top, left, bottom, right)) = self.clip_rect(top, left, bottom, right) else {
            return;
        };

        self.ensure_rows_through(bottom);
        let params = if params.is_empty() { &[0][..] } else { params };
        for row in top..=bottom {
            let line = &mut self.lines[row];
            if line.len() <= right {
                line.resize(right + 1, ScreenCell::blank(self.current_attr));
            }
            for col in left..=right {
                Self::reverse_decrara_attributes(&mut line[col].attr, params);
            }
        }
        self.mark_dirty_range(top, bottom);
    }

    fn apply_deccara_attributes(attr: &mut CellAttributes, params: &[usize]) {
        for param in params {
            match *param {
                0 => {
                    attr.bold = false;
                    attr.faint = false;
                    attr.clear_underline();
                    attr.blink = None;
                    attr.inverse = false;
                }
                1 => attr.bold = true,
                4 => attr.set_underline(StyledUnderline::Single),
                5 => attr.blink = Some(StyledBlink::Slow),
                7 => attr.inverse = true,
                8 => attr.hidden = true,
                22 => {
                    attr.bold = false;
                    attr.faint = false;
                }
                24 => attr.clear_underline(),
                25 => attr.blink = None,
                27 => attr.inverse = false,
                28 => attr.hidden = false,
                _ => {}
            }
        }
    }

    fn reverse_decrara_attributes(attr: &mut CellAttributes, params: &[usize]) {
        for param in params {
            match *param {
                0 => {
                    attr.bold = !attr.bold;
                    attr.underline = !attr.underline;
                    attr.underline_style = if attr.underline {
                        Some(attr.underline_style.unwrap_or(StyledUnderline::Single))
                    } else {
                        None
                    };
                    attr.blink = if attr.blink.is_some() {
                        None
                    } else {
                        Some(StyledBlink::Slow)
                    };
                    attr.inverse = !attr.inverse;
                }
                1 => attr.bold = !attr.bold,
                4 => {
                    attr.underline = !attr.underline;
                    attr.underline_style = if attr.underline {
                        Some(attr.underline_style.unwrap_or(StyledUnderline::Single))
                    } else {
                        None
                    };
                }
                5 => {
                    attr.blink = if attr.blink.is_some() {
                        None
                    } else {
                        Some(StyledBlink::Slow)
                    };
                }
                7 => attr.inverse = !attr.inverse,
                8 => attr.hidden = !attr.hidden,
                _ => {}
            }
        }
    }

    fn set_character_protection(&mut self, mode: usize) {
        self.current_attr.set_protected(mode == 1);
    }

    fn clip_rect(
        &self,
        top: usize,
        left: usize,
        bottom: usize,
        right: usize,
    ) -> Option<(usize, usize, usize, usize)> {
        let top = top.min(self.rows.saturating_sub(1));
        let bottom = bottom.min(self.rows.saturating_sub(1));
        let left = left.min(self.cols.saturating_sub(1));
        let right = right.min(self.cols.saturating_sub(1));
        if top > bottom || left > right {
            None
        } else {
            Some((top, left, bottom, right))
        }
    }

    fn reset_terminal(&mut self) {
        let cols = self.cols;
        let rows = self.rows;
        let revision = self.revision();
        let title = self.title.take();
        let current_dir = self.current_dir.take();

        *self = Self::new(cols, rows);
        self.render_state.set_revision(revision);
        self.title = title;
        self.current_dir = current_dir;
        self.mark_all_dirty();
    }

    fn soft_reset_terminal(&mut self) {
        self.current_attr = CellAttributes::default();
        self.insert_mode = false;
        self.left_right_margin_mode = false;
        self.origin_mode = false;
        self.auto_wrap = true;
        self.reverse_video = false;
        self.application_cursor_keys = false;
        self.application_keypad = false;
        self.focus_event_reporting = false;
        self.mouse_tracking = MouseTrackingMode::None;
        self.utf8_mouse = false;
        self.urxvt_mouse = false;
        self.sgr_mouse = false;
        self.alternate_scroll = false;
        self.sgr_pixel_mouse = false;
        self.meta_sends_escape = false;
        self.synchronized_output = false;
        self.cursor_visible = true;
        self.cursor_blinking = true;
        self.cursor_shape = "Default".to_string();
        self.tab_stops = Self::default_tab_stops(self.cols);
        self.scroll_top = 0;
        self.scroll_bottom = self.rows.saturating_sub(1);
        self.left_margin = 0;
        self.right_margin = self.cols.saturating_sub(1);
        self.saved_cursor_x = 0;
        self.saved_cursor_y = 0;
        self.saved_cursor_attr = CellAttributes::default();
        if let Some(alternate) = self.alternate.as_mut() {
            alternate.saved_cursor_x = 0;
            alternate.saved_cursor_y = 0;
            alternate.saved_cursor_attr = CellAttributes::default();
        }
        self.ensure_cursor_line();
        self.mark_all_dirty();
    }

    fn erase_line_range(&mut self, row: usize, start: usize, end: usize, selective: bool) {
        if start >= end || row >= self.rows {
            return;
        }
        self.ensure_rows_through(row);
        let line = &mut self.lines[row];
        let end = end.min(self.cols);
        if line.len() < end {
            line.resize(end, ScreenCell::blank(self.current_attr));
        }
        // Either edge of the range can fall between the two halves of a wide
        // character; the half left standing has to give up its second column.
        Self::split_wide_cell(line, start);
        Self::split_wide_cell(line, end.saturating_sub(1));
        let blank = ScreenCell::blank(self.current_attr);
        for cell in line.iter_mut().take(end).skip(start) {
            if !selective || !cell.attr.protected {
                *cell = blank.clone();
            }
        }
        self.mark_dirty_row(row);
    }

    fn erase_in_display(&mut self, mode: usize) {
        self.erase_in_display_with_protection(mode, false);
    }

    fn selective_erase_in_display(&mut self, mode: usize) {
        self.erase_in_display_with_protection(mode, true);
    }

    fn erase_in_display_with_protection(&mut self, mode: usize, selective: bool) {
        match mode {
            0 => {
                self.erase_in_line_with_protection(0, selective);
                let start = self.cursor_y + 1;
                if start < self.rows {
                    if selective {
                        for row in start..self.rows.min(self.lines.len()) {
                            self.erase_line_range(row, 0, self.cols, true);
                        }
                    } else if start < self.lines.len() {
                        for line in self.lines.iter_mut().skip(start) {
                            line.clear();
                        }
                        self.mark_dirty_range(start, self.rows.saturating_sub(1));
                    }
                }
            }
            1 => {
                let end = self.cursor_y.min(self.lines.len().saturating_sub(1));
                if selective {
                    for row in 0..self.cursor_y.min(self.lines.len()) {
                        self.erase_line_range(row, 0, self.cols, true);
                    }
                } else {
                    for line in self.lines.iter_mut().take(end) {
                        line.clear();
                    }
                    self.mark_dirty_range(0, self.cursor_y);
                }
                self.erase_in_line_with_protection(1, selective);
            }
            2 => {
                if selective {
                    for row in 0..self.rows.min(self.lines.len()) {
                        self.erase_line_range(row, 0, self.cols, true);
                    }
                } else {
                    self.clear_display();
                }
            }
            3 => {
                self.history.clear();
                self.mark_all_dirty();
            }
            _ => {}
        }
    }

    fn erase_in_line(&mut self, mode: usize) {
        self.erase_in_line_with_protection(mode, false);
    }

    fn selective_erase_in_line(&mut self, mode: usize) {
        self.erase_in_line_with_protection(mode, true);
    }

    fn erase_in_line_with_protection(&mut self, mode: usize, selective: bool) {
        self.ensure_cursor_line();
        match mode {
            0 => {
                let start = self.cursor_x.min(self.cols);
                self.erase_line_range(self.cursor_y, start, self.cols, selective);
            }
            1 => {
                let end = self.cursor_x.saturating_add(1).min(self.cols);
                self.erase_line_range(self.cursor_y, 0, end, selective);
            }
            2 => {
                self.erase_line_range(self.cursor_y, 0, self.cols, selective);
            }
            _ => {}
        }
    }

    fn insert_chars(&mut self, count: usize) {
        self.ensure_cursor_line();
        let left = self.cursor_x.max(self.active_left_margin());
        let right = self.active_right_margin();
        if left > right {
            return;
        }
        let count = count.max(1).min(right + 1 - left);
        let line = &mut self.lines[self.cursor_y];
        if line.len() < right + 1 {
            line.resize(right + 1, ScreenCell::blank(self.current_attr));
        }
        for idx in (left..=right).rev() {
            line[idx] = if idx >= left + count {
                line[idx - count].clone()
            } else {
                ScreenCell::blank(self.current_attr)
            };
        }
        // Everything from the cursor rightwards just moved one way; whatever
        // wide character that walked through has to be made whole again.
        Self::repair_wide_pairs(line, left, right);
        self.mark_dirty_row(self.cursor_y);
    }

    fn delete_chars(&mut self, count: usize) {
        self.ensure_cursor_line();
        let left = self.cursor_x.max(self.active_left_margin());
        let right = self.active_right_margin();
        if left > right {
            return;
        }
        let count = count.max(1).min(right + 1 - left);
        let line = &mut self.lines[self.cursor_y];
        if line.len() < right + 1 {
            line.resize(right + 1, ScreenCell::blank(self.current_attr));
        }
        for idx in left..=right {
            let source = idx + count;
            line[idx] = if source <= right {
                line[source].clone()
            } else {
                ScreenCell::blank(self.current_attr)
            };
        }
        // Same slide, opposite direction, same repair.
        Self::repair_wide_pairs(line, left, right);
        self.mark_dirty_row(self.cursor_y);
    }

    fn erase_chars(&mut self, count: usize) {
        self.ensure_cursor_line();
        let end = self
            .cursor_x
            .saturating_add(count.max(1))
            .min(self.active_right_margin() + 1);
        let line = &mut self.lines[self.cursor_y];
        if line.len() < end {
            line.resize(end, ScreenCell::blank(self.current_attr));
        }
        // Same hazard as `ESC[K`: either edge can land between the halves of
        // a wide character.
        Self::split_wide_cell(line, self.cursor_x);
        Self::split_wide_cell(line, end.saturating_sub(1));
        for cell in line.iter_mut().take(end).skip(self.cursor_x) {
            *cell = ScreenCell::blank(self.current_attr);
        }
        self.mark_dirty_row(self.cursor_y);
    }

    /// `SL`: the whole scroll region slides left, and the cursor stays put.
    ///
    /// This used to be `ESC[P` wearing a different name. It took its left edge
    /// from the cursor and it moved the cursor's row and nothing else, and
    /// both of those are wrong: ECMA-48 hands `SL` the presentation component,
    /// which is the scroll region bounded by the vertical margins and, under
    /// `DECLRMM`, the horizontal ones. The cursor has no part in it at all.
    /// The single-row half is the one that showed -- `SL` in any full-screen
    /// app tore the frame open along whichever line the cursor happened to be
    /// sitting on.
    ///
    /// Rows nothing was ever written to are skipped rather than filled. There
    /// is nothing on them to move, and materialising a full row of cells for
    /// every blank line on every `SL` would make a cheap operation allocate
    /// the screen.
    fn scroll_left(&mut self, count: usize) {
        self.ensure_cursor_line();
        let left = self.active_left_margin();
        let right = self.active_right_margin();
        if left > right {
            return;
        }
        let count = count.max(1).min(right + 1 - left);
        let blank = ScreenCell::blank(self.current_attr);
        for row in self.scroll_top..=self.scroll_bottom {
            let Some(line) = self.lines.get_mut(row) else {
                continue;
            };
            if line.is_empty() {
                continue;
            }
            if line.len() < right + 1 {
                line.resize(right + 1, blank.clone());
            }
            for idx in left..=right {
                let source = idx + count;
                line[idx] = if source <= right {
                    line[source].clone()
                } else {
                    blank.clone()
                };
            }
            // The slide is the one `ESC[P` performs, so it cuts wide
            // characters in exactly the same places.
            Self::repair_wide_pairs(line, left, right);
            self.mark_dirty_row(row);
        }
    }

    /// `SR`: the same region, the same silence from the cursor, other way.
    fn scroll_right(&mut self, count: usize) {
        self.ensure_cursor_line();
        let left = self.active_left_margin();
        let right = self.active_right_margin();
        if left > right {
            return;
        }
        let count = count.max(1).min(right + 1 - left);
        let blank = ScreenCell::blank(self.current_attr);
        for row in self.scroll_top..=self.scroll_bottom {
            let Some(line) = self.lines.get_mut(row) else {
                continue;
            };
            if line.is_empty() {
                continue;
            }
            if line.len() < right + 1 {
                line.resize(right + 1, blank.clone());
            }
            for idx in (left..=right).rev() {
                line[idx] = if idx >= left + count {
                    line[idx - count].clone()
                } else {
                    blank.clone()
                };
            }
            // And this one is `ESC[@`.
            Self::repair_wide_pairs(line, left, right);
            self.mark_dirty_row(row);
        }
    }

    #[cfg(test)]
    fn attrs_for_viewport(&self) -> Vec<Vec<CellAttributes>> {
        self.lines
            .iter()
            .map(|line| line.iter().map(|cell| cell.attr).collect())
            .collect()
    }

    fn apply_sgr(&mut self, params: &[usize]) {
        sgr::apply(params, &mut self.current_attr);
    }

    fn apply_osc(&mut self, sequence: &str) {
        match osc_params::parse(sequence) {
            Some(osc_params::OscCommand::Title(title)) => self.title = Some(title),
            Some(osc_params::OscCommand::CurrentDir(cwd)) => self.current_dir = Some(cwd),
            Some(osc_params::OscCommand::Hyperlink(uri)) => self.apply_osc8_hyperlink(uri),
            // Held for the front end to collect: the kernel has no clipboard
            // and should not grow one, but it is the only thing that sees
            // the sequence.
            Some(osc_params::OscCommand::Clipboard(text)) => self.clipboard_request = Some(text),
            Some(osc_params::OscCommand::PromptStart) if self.alternate.is_none() => {
                let row = self.history.scrollback_rows() + self.cursor_y;
                if self.prompt_rows.last() != Some(&row) {
                    self.prompt_rows.push(row);
                }
            }
            Some(osc_params::OscCommand::PromptStart) => {}
            Some(osc_params::OscCommand::Notification(text)) => {
                self.notifications = self.notifications.wrapping_add(1);
                self.last_notification = Some(text);
            }
            None => {}
        }
    }

    fn apply_osc8_hyperlink(&mut self, uri: Option<String>) {
        let Some(uri) = uri else {
            self.current_attr.hyperlink = None;
            return;
        };
        let idx = self
            .hyperlinks
            .iter()
            .position(|known| known == &uri)
            .unwrap_or_else(|| {
                self.hyperlinks.push(uri);
                self.hyperlinks.len() - 1
            });
        self.current_attr.hyperlink = u32::try_from(idx).ok();
    }

    fn insert_lines(&mut self, count: usize) {
        self.ensure_cursor_line();
        let bottom = if self.cursor_y >= self.scroll_top && self.cursor_y <= self.scroll_bottom {
            self.scroll_bottom
        } else {
            self.rows.saturating_sub(1)
        };
        self.ensure_rows_through(bottom);
        self.mark_dirty_range(self.cursor_y, bottom);
        for _ in 0..count.max(1) {
            self.lines.insert(self.cursor_y, Vec::new());
            if self.lines.len() > bottom + 1 {
                self.lines.remove(bottom + 1);
            }
        }
    }

    fn delete_lines(&mut self, count: usize) {
        self.ensure_cursor_line();
        let bottom = if self.cursor_y >= self.scroll_top && self.cursor_y <= self.scroll_bottom {
            self.scroll_bottom
        } else {
            self.rows.saturating_sub(1)
        };
        self.ensure_rows_through(bottom);
        self.mark_dirty_range(self.cursor_y, bottom);
        for _ in 0..count.max(1) {
            if self.cursor_y <= bottom && self.cursor_y < self.lines.len() {
                self.lines.remove(self.cursor_y);
            }
            self.lines.insert(bottom, Vec::new());
            if self.lines.len() > self.rows {
                self.lines.truncate(self.rows);
            }
        }
        self.ensure_cursor_line();
    }

    fn scroll_up(&mut self, count: usize) {
        self.scroll_up_region(self.scroll_top, self.scroll_bottom, count);
    }

    fn scroll_up_region(&mut self, top: usize, bottom: usize, count: usize) {
        if top > bottom {
            return;
        }
        self.ensure_rows_through(bottom);
        self.mark_dirty_range(top, bottom);
        for _ in 0..count.max(1) {
            let removed = self.lines.remove(top);
            let mut blank = Vec::new();
            if top == 0 && bottom + 1 >= self.rows && self.alternate.is_none() {
                let push = self.history.push_scrollback(removed, self.scrollback_limit);
                self.trim_prompt_rows(push.trimmed);
                if let Some(mut recycled) = push.recycled {
                    recycled.clear();
                    blank = recycled;
                }
            }
            self.lines.insert(bottom, blank);
        }
        self.cursor_y = self.cursor_y.min(self.rows.saturating_sub(1));
    }

    fn scroll_down(&mut self, count: usize) {
        self.scroll_down_region(self.scroll_top, self.scroll_bottom, count);
    }

    fn scroll_down_region(&mut self, top: usize, bottom: usize, count: usize) {
        if top > bottom {
            return;
        }
        self.ensure_rows_through(bottom);
        self.mark_dirty_range(top, bottom);
        for _ in 0..count.max(1) {
            self.lines.remove(bottom);
            self.lines.insert(top, Vec::new());
        }
        self.cursor_y = self.cursor_y.min(self.rows.saturating_sub(1));
    }

    fn set_scroll_region(&mut self, top: usize, bottom: usize) {
        let top = top.min(self.rows.saturating_sub(1));
        let bottom = bottom.min(self.rows.saturating_sub(1));
        if top >= bottom {
            self.scroll_top = 0;
            self.scroll_bottom = self.rows.saturating_sub(1);
        } else {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
        }
        self.set_cursor_position(0, 0);
        self.mark_all_dirty();
    }

    fn set_horizontal_margins(&mut self, left: usize, right: usize) {
        let left = left.min(self.cols.saturating_sub(1));
        let right = right.min(self.cols.saturating_sub(1));
        if left >= right {
            self.left_margin = 0;
            self.right_margin = self.cols.saturating_sub(1);
        } else {
            self.left_margin = left;
            self.right_margin = right;
        }
        self.set_cursor_position(0, 0);
        self.mark_all_dirty();
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        self.cols = cols.max(1);
        self.rows = rows.max(1);
        self.bump_revision();
        self.clear_dirty_rows();
        self.mark_all_dirty();
        Self::truncate_lines_to_cols(&mut self.lines, self.cols);
        self.history.truncate_scrollback_to_cols(self.cols);
        let cols = self.cols;
        if let Some(alternate) = self.alternate.as_mut() {
            alternate.cols = cols;
            Self::truncate_lines_to_cols(&mut alternate.lines, cols);
            Self::truncate_lines_to_cols(&mut alternate.scrollback, cols);
            alternate.tab_stops.retain(|stop| *stop < cols);
            alternate.cursor_x = alternate.cursor_x.min(cols.saturating_sub(1));
            alternate.saved_cursor_x = alternate.saved_cursor_x.min(cols.saturating_sub(1));
        }
        self.tab_stops.retain(|stop| *stop < cols);
        if self.lines.len() > self.rows {
            let trim = self.lines.len() - self.rows;
            let drained = self.lines.drain(..trim).collect::<Vec<_>>();
            if self.alternate.is_none() {
                let trimmed = self
                    .history
                    .extend_scrollback(drained, self.scrollback_limit);
                self.trim_prompt_rows(trimmed);
            }
            self.cursor_y = self.cursor_y.saturating_sub(trim);
            self.saved_cursor_y = self.saved_cursor_y.saturating_sub(trim);
        }
        self.cursor_x = self.cursor_x.min(self.cols.saturating_sub(1));
        self.saved_cursor_x = self.saved_cursor_x.min(self.cols.saturating_sub(1));
        self.cursor_y = self.cursor_y.min(self.rows.saturating_sub(1));
        self.scroll_top = self.scroll_top.min(self.rows.saturating_sub(1));
        self.scroll_bottom = self.scroll_bottom.min(self.rows.saturating_sub(1));
        if self.scroll_top >= self.scroll_bottom {
            self.scroll_top = 0;
            self.scroll_bottom = self.rows.saturating_sub(1);
        }
        self.left_margin = self.left_margin.min(self.cols.saturating_sub(1));
        self.right_margin = self.right_margin.min(self.cols.saturating_sub(1));
        if self.left_margin >= self.right_margin {
            self.left_margin = 0;
            self.right_margin = self.cols.saturating_sub(1);
        }
        self.ensure_cursor_line();
    }

    fn set_column_mode(&mut self, wide: bool) {
        self.column_132_mode = wide;
        self.cols = if wide { 132 } else { 80 };
        self.lines.clear();
        self.history.clear();
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.saved_cursor_x = 0;
        self.saved_cursor_y = 0;
        self.scroll_top = 0;
        self.scroll_bottom = self.rows.saturating_sub(1);
        self.left_right_margin_mode = false;
        self.left_margin = 0;
        self.right_margin = self.cols.saturating_sub(1);
        self.tab_stops = Self::default_tab_stops(self.cols);
        self.ensure_cursor_line();
        self.mark_all_dirty();
    }

    /// Erase the other half of a wide character when one half is overwritten.
    ///
    /// A TUI that repaints by jumping to a column and rewriting a single cell
    /// -- which is exactly how Claude Code and Codex redraw, `ESC[3G` then one
    /// glyph -- lands on half of a CJK character all the time. Leaving the
    /// other half behind gives the row either a continuation cell that owns no
    /// character, or a two-column glyph still painting over the character that
    /// replaced it. On screen both read as characters in the wrong place.
    ///
    /// The surviving half keeps its own attributes: it was not written to, so
    /// its colours are still the ones the application asked for.
    fn split_wide_cell(line: &mut [ScreenCell], idx: usize) {
        let Some(cell) = line.get(idx) else {
            return;
        };
        if cell.width == 0 {
            // A continuation. Its owner is the nearest earlier cell that is
            // not itself a continuation.
            let mut owner = idx;
            while owner > 0 {
                owner -= 1;
                if line[owner].width != 0 {
                    break;
                }
            }
            if line[owner].width > 1 {
                Self::blank_cell_in_place(&mut line[owner]);
            }
            return;
        }
        if cell.width > 1 {
            for offset in 1..usize::from(cell.width) {
                let Some(tail) = line.get_mut(idx + offset) else {
                    break;
                };
                if tail.width != 0 {
                    break;
                }
                Self::blank_cell_in_place(tail);
            }
        }
    }

    fn blank_cell_in_place(cell: &mut ScreenCell) {
        cell.ch = ' ';
        cell.clear_combining();
        cell.width = 1;
    }

    /// Put a row back together after a shift moved cells past each other.
    ///
    /// `ESC[@`, `ESC[P`, `ESC[ @`, `ESC[ A` and insert mode never overwrite a
    /// cell, they slide it -- and a slide can pass straight between the two
    /// columns of a wide character. zsh's line editor reaches for one of them
    /// every time you type or delete in the middle of a line, so on a row of
    /// Chinese this is not a corner case, it is what editing a line looks
    /// like.
    ///
    /// Splitting at the two ends the way `ESC[K` does is not enough here. The
    /// ends are only one of the ways a shift breaks a pair: the other half can
    /// be the one that falls off the right margin, or the one dragged in from
    /// beyond the deleted span, and both of those land in the middle of the
    /// row rather than at its edges. So this sweeps everything the shift
    /// rewrote and asks each cell the only question that matters -- is your
    /// other half still beside you? -- which is also why it does not need to
    /// be told which way the row moved, or by how much.
    ///
    /// The sweep reaches one column past each end, because a pair can straddle
    /// a boundary: the half inside the span is the one that moved, and the
    /// orphan it leaves sits just outside. It runs left to right on purpose --
    /// blanking a wide cell shrinks it to one column, and the continuation
    /// behind it has to be judged against the cell as it now is.
    ///
    /// Survivors are blanked in place and keep their own attributes. Nothing
    /// was written over them, the shift only carried the neighbour away, so
    /// the colours the application asked for are still the ones it wants.
    fn repair_wide_pairs(line: &mut [ScreenCell], start: usize, end: usize) {
        if line.is_empty() {
            return;
        }
        let first = start.saturating_sub(1);
        let last = end.saturating_add(1).min(line.len() - 1);
        for idx in first..=last {
            match line[idx].width {
                0 => {
                    if !Self::has_wide_owner(line, idx) {
                        Self::blank_cell_in_place(&mut line[idx]);
                    }
                }
                width if width > 1 => {
                    let width = usize::from(width);
                    let whole = (1..width).all(|offset| {
                        matches!(line.get(idx + offset), Some(tail) if tail.width == 0)
                    });
                    if !whole {
                        Self::blank_cell_in_place(&mut line[idx]);
                    }
                }
                _ => {}
            }
        }
    }

    /// Whether a continuation cell still has a wide cell reaching it.
    ///
    /// Walks back over any continuations in between, because a wider-than-two
    /// character owns several of them, and a row that begins with one owns
    /// nothing at all -- that is the orphan a delete leaves at column one.
    fn has_wide_owner(line: &[ScreenCell], idx: usize) -> bool {
        let mut owner = idx;
        while owner > 0 {
            owner -= 1;
            let width = line[owner].width;
            if width == 0 {
                continue;
            }
            return owner + usize::from(width) > idx;
        }
        false
    }

    /// Takes anything that hands out its rows by mutable reference, so the
    /// live lines (a `Vec`) and the scrollback (a `VecDeque`) share one body.
    fn truncate_lines_to_cols<'a>(
        lines: impl IntoIterator<Item = &'a mut Vec<ScreenCell>>,
        cols: usize,
    ) {
        for line in lines {
            if line.len() > cols {
                line.truncate(cols);
            }
        }
    }

    fn default_tab_stops(cols: usize) -> BTreeSet<usize> {
        (8..cols).step_by(8).collect()
    }

    fn enter_alternate_screen(&mut self, mode: usize, clear: bool) {
        if self.alternate.is_some() {
            self.alternate_screen_modes.insert(mode);
            if clear {
                self.clear_screen();
            }
            return;
        }

        let main = ScreenState {
            cols: self.cols,
            scrollback: self.history.take_scrollback(),
            lines: std::mem::take(&mut self.lines),
            viewport_top: self.history.take_viewport_top(),
            cursor_x: self.cursor_x,
            cursor_y: self.cursor_y,
            cursor_visible: self.cursor_visible,
            cursor_blinking: self.cursor_blinking,
            cursor_shape: self.cursor_shape.clone(),
            column_132_mode: self.column_132_mode,
            auto_wrap: self.auto_wrap,
            reverse_video: self.reverse_video,
            application_cursor_keys: self.application_cursor_keys,
            application_keypad: self.application_keypad,
            focus_event_reporting: self.focus_event_reporting,
            mouse_tracking: self.mouse_tracking,
            utf8_mouse: self.utf8_mouse,
            urxvt_mouse: self.urxvt_mouse,
            sgr_mouse: self.sgr_mouse,
            alternate_scroll: self.alternate_scroll,
            sgr_pixel_mouse: self.sgr_pixel_mouse,
            meta_sends_escape: self.meta_sends_escape,
            synchronized_output: self.synchronized_output,
            alternate_screen_modes: std::mem::take(&mut self.alternate_screen_modes),
            origin_mode: self.origin_mode,
            insert_mode: self.insert_mode,
            left_right_margin_mode: self.left_right_margin_mode,
            tab_stops: std::mem::take(&mut self.tab_stops),
            bracketed_paste: self.bracketed_paste,
            current_attr: self.current_attr,
            scroll_top: self.scroll_top,
            scroll_bottom: self.scroll_bottom,
            left_margin: self.left_margin,
            right_margin: self.right_margin,
            saved_cursor_x: self.saved_cursor_x,
            saved_cursor_y: self.saved_cursor_y,
            saved_cursor_attr: self.saved_cursor_attr,
        };
        self.alternate = Some(main);
        self.alternate_screen_modes.insert(mode);
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.saved_cursor_x = 0;
        self.saved_cursor_y = 0;
        self.saved_cursor_attr = CellAttributes::default();
        self.cursor_shape = "Default".to_string();
        self.cursor_blinking = true;
        self.auto_wrap = true;
        self.reverse_video = false;
        self.application_cursor_keys = false;
        self.application_keypad = false;
        self.focus_event_reporting = false;
        self.mouse_tracking = MouseTrackingMode::None;
        self.utf8_mouse = false;
        self.urxvt_mouse = false;
        self.sgr_mouse = false;
        self.alternate_scroll = false;
        self.sgr_pixel_mouse = false;
        self.meta_sends_escape = false;
        self.synchronized_output = false;
        self.origin_mode = false;
        self.insert_mode = false;
        self.left_right_margin_mode = false;
        self.tab_stops = Self::default_tab_stops(self.cols);
        self.bracketed_paste = false;
        self.scroll_top = 0;
        self.scroll_bottom = self.rows.saturating_sub(1);
        self.left_margin = 0;
        self.right_margin = self.cols.saturating_sub(1);
        self.lines.clear();
        self.ensure_cursor_line();
        self.mark_all_dirty();
    }

    fn leave_alternate_screen(&mut self, mode: usize) {
        self.alternate_screen_modes.remove(&mode);
        if !self.alternate_screen_modes.is_empty() {
            return;
        }
        if let Some(main) = self.alternate.take() {
            self.restore_main_screen(main);
        }
    }

    fn restore_main_screen(&mut self, main: ScreenState) {
        self.cols = main.cols;
        self.history.replace_scrollback(main.scrollback);
        self.lines = main.lines;
        self.history.replace_viewport_top(main.viewport_top);
        self.cursor_x = main.cursor_x;
        self.cursor_y = main.cursor_y;
        self.cursor_visible = main.cursor_visible;
        self.cursor_blinking = main.cursor_blinking;
        self.cursor_shape = main.cursor_shape;
        self.column_132_mode = main.column_132_mode;
        self.auto_wrap = main.auto_wrap;
        self.reverse_video = main.reverse_video;
        self.application_cursor_keys = main.application_cursor_keys;
        self.application_keypad = main.application_keypad;
        self.focus_event_reporting = main.focus_event_reporting;
        self.mouse_tracking = main.mouse_tracking;
        self.utf8_mouse = main.utf8_mouse;
        self.urxvt_mouse = main.urxvt_mouse;
        self.sgr_mouse = main.sgr_mouse;
        self.alternate_scroll = main.alternate_scroll;
        self.sgr_pixel_mouse = main.sgr_pixel_mouse;
        self.meta_sends_escape = main.meta_sends_escape;
        self.synchronized_output = main.synchronized_output;
        self.alternate_screen_modes = main.alternate_screen_modes;
        self.origin_mode = main.origin_mode;
        self.insert_mode = main.insert_mode;
        self.left_right_margin_mode = main.left_right_margin_mode;
        self.tab_stops = main.tab_stops;
        self.bracketed_paste = main.bracketed_paste;
        self.current_attr = main.current_attr;
        self.scroll_top = main.scroll_top;
        self.scroll_bottom = main.scroll_bottom;
        self.left_margin = main.left_margin;
        self.right_margin = main.right_margin;
        self.saved_cursor_x = main.saved_cursor_x;
        self.saved_cursor_y = main.saved_cursor_y;
        self.saved_cursor_attr = main.saved_cursor_attr;
        if self.lines.len() > self.rows {
            let trim = self.lines.len() - self.rows;
            self.lines.drain(..trim);
            self.cursor_y = self.cursor_y.saturating_sub(trim);
            self.saved_cursor_y = self.saved_cursor_y.saturating_sub(trim);
        }
        self.ensure_cursor_line();
        self.mark_all_dirty();
    }
}

impl NextCoreEngine {
    /// Choose the history capacity captured by subsequently-created panes.
    ///
    /// Existing panes deliberately keep their original capacity, matching the
    /// settings page's "restart/new pane to apply" contract.
    pub fn set_new_session_scrollback_lines(lines: usize) {
        NEW_SESSION_SCROLLBACK_LINES.store(lines, Ordering::Relaxed);
    }

    pub fn new_session_scrollback_lines() -> usize {
        NEW_SESSION_SCROLLBACK_LINES.load(Ordering::Relaxed)
    }

    #[doc(hidden)]
    pub fn debug_output(&self, pane_id: usize) -> Result<String> {
        runtime::output(pane_id)
    }

    pub fn scroll_viewport_to(&self, pane_id: usize, target: isize) -> Result<()> {
        runtime::scroll_viewport_to(pane_id, target)
    }

    /// Move the viewport by `delta` rows. Negative scrolls back into history.
    pub fn scroll_viewport_by(&self, pane_id: usize, delta: isize) -> Result<()> {
        runtime::scroll_viewport_by(pane_id, delta)
    }

    pub fn scroll_viewport_to_prompt(&self, pane_id: usize, amount: isize) -> Result<()> {
        runtime::scroll_viewport_to_prompt(pane_id, amount)
    }

    /// Terminal modes a GUI pane has to expose (mouse grab, alternate screen).
    pub fn pane_modes(&self, pane_id: usize) -> Result<crate::PaneModesSnapshot> {
        runtime::pane_modes(pane_id)
    }

    /// The screen revision counter, for cheap change detection.
    pub fn screen_revision(&self, pane_id: usize) -> Result<u64> {
        runtime::screen_revision(pane_id)
    }
}

impl SessionEngine for NextCoreEngine {
    fn list_sessions(&self) -> Result<Vec<SessionSnapshot>> {
        runtime::list_sessions()
    }

    fn get_session(&self, pane_id: usize) -> Result<SessionSnapshot> {
        runtime::get_session(pane_id)
    }

    fn create_session(&self, request: CreateSessionRequest) -> Result<SessionSnapshot> {
        runtime::create_session(request)
    }

    fn split_session(&self, request: SplitSessionRequest) -> Result<SessionSnapshot> {
        runtime::split_session(request)
    }

    fn focus_session(&self, pane_id: usize) -> Result<()> {
        runtime::focus(pane_id)
    }

    fn shell(&self, pane_id: usize) -> Result<ShellSnapshot> {
        runtime::shell_snapshot(pane_id)
    }

    fn activity(&self, pane_id: usize) -> Result<SessionActivitySnapshot> {
        runtime::session_activity(pane_id)
    }

    fn resize_session(&self, pane_id: usize, cols: usize, rows: usize) -> Result<()> {
        runtime::resize(pane_id, cols, rows)
    }

    fn destroy_session(&self, pane_id: usize) -> Result<()> {
        runtime::destroy(pane_id)
    }

    fn set_split_ratio(&self, pane_id: usize, first_ratio: f64) -> Result<()> {
        runtime::set_split_ratio(pane_id, first_ratio)
    }
}

impl ScreenEngine for NextCoreEngine {
    fn read_screen(&self, pane_id: usize) -> Result<ScreenSnapshot> {
        runtime::read_screen(pane_id)
    }

    fn read_styled_screen(&self, pane_id: usize) -> Result<StyledScreenSnapshot> {
        runtime::read_styled_screen(pane_id)
    }

    fn read_render_frame(
        &self,
        pane_id: usize,
        since_revision: Option<u64>,
    ) -> Result<RenderFrameSnapshot> {
        runtime::read_render_frame(pane_id, since_revision)
    }

    fn read_visible_text(&self, pane_id: usize) -> Result<String> {
        runtime::read_visible_text(pane_id)
    }

    fn read_lines(&self, pane_id: usize, start: i64, count: usize) -> Result<Vec<ScreenLine>> {
        runtime::read_lines(pane_id, start, count)
    }

    fn read_scrollback(&self, pane_id: usize, limit: usize) -> Result<Vec<String>> {
        runtime::read_scrollback(pane_id, limit)
    }

    fn read_scrollback_text(
        &self,
        pane_id: usize,
        request: ScrollbackTextRequest,
    ) -> Result<ScrollbackTextSnapshot> {
        runtime::read_scrollback_text(pane_id, request)
    }

    fn read_styled_scrollback(
        &self,
        pane_id: usize,
        request: ScrollbackTextRequest,
    ) -> Result<StyledScrollbackSnapshot> {
        runtime::read_styled_scrollback(pane_id, request)
    }

    fn search(
        &self,
        pane_id: usize,
        pattern: &str,
        mode: crate::SearchMode,
        max_results: usize,
    ) -> Result<Vec<ScreenSearchMatch>> {
        runtime::search_screen(pane_id, pattern, mode, max_results)
    }

    fn cursor(&self, pane_id: usize) -> Result<CursorSnapshot> {
        runtime::cursor(pane_id)
    }
    fn erase_scrollback(&self, pane_id: usize, include_viewport: bool) -> Result<()> {
        runtime::erase_scrollback(pane_id, include_viewport)
    }
}

impl InputEngine for NextCoreEngine {
    fn write_input(&self, pane_id: usize, input: &str) -> Result<()> {
        runtime::write_input(pane_id, input)
    }

    fn paste_input(&self, pane_id: usize, text: &str) -> Result<()> {
        runtime::paste_input(pane_id, text)
    }
}

impl NextCoreEngine {
    /// Offer a mouse event to the session.
    ///
    /// Whether anything reaches the PTY depends on the modes the application
    /// negotiated; with reporting off this is a no-op and the terminal keeps
    /// the mouse for selection and scrollback.
    pub fn report_mouse(&self, pane_id: usize, event: mouse_encoding::MouseEvent) -> Result<()> {
        runtime::report_mouse(pane_id, event)
    }
}

impl RecordingEngine for NextCoreEngine {
    fn start_recording(&self, pane_id: usize) -> Result<RecordingStartResult> {
        runtime::start_recording(pane_id)
    }

    fn stop_recording(&self, pane_id: usize) -> Result<RecordingStopResult> {
        runtime::stop_recording(pane_id)
    }

    fn recording_status(&self, pane_id: usize) -> Result<RecordingStatusSnapshot> {
        runtime::recording_status(pane_id)
    }

    fn attach_recording_trace(&self, pane_id: usize, trace_id: String) -> Result<Vec<String>> {
        runtime::attach_recording_trace(pane_id, trace_id)
    }

    fn export_markdown(
        &self,
        pane_id: usize,
        target_path: Option<String>,
    ) -> Result<RecordingExportResult> {
        runtime::export_recording_markdown(pane_id, target_path)
    }
}

impl HealthEngine for NextCoreEngine {
    fn health(&self) -> Result<EngineHealthSnapshot> {
        runtime::health_snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StyledColor;
    use parking_lot::MutexGuard;

    struct SharedWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.bytes.lock().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn answer_terminal_queries(
        chunk: &str,
        screen: &NextCoreScreen,
        writer: &Arc<Mutex<Box<dyn Write + Send>>>,
    ) {
        let mut pending = String::new();
        answer_terminal_queries_with_pending(chunk, screen, writer, &mut pending);
    }

    fn answer_terminal_queries_with_pending(
        chunk: &str,
        screen: &NextCoreScreen,
        writer: &Arc<Mutex<Box<dyn Write + Send>>>,
        pending: &mut String,
    ) {
        terminal_queries::answer_with_pending(chunk, screen, writer, pending);
    }

    fn test_guard() -> MutexGuard<'static, ()> {
        static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        TEST_LOCK.get_or_init(|| Mutex::new(())).lock()
    }

    fn quiet_wait_command_for_test() -> portable_pty::CommandBuilder {
        #[cfg(windows)]
        {
            let mut command = portable_pty::CommandBuilder::new("cmd.exe");
            command.args(["/c", "ping -n 5 127.0.0.1 >nul"]);
            command
        }
        #[cfg(not(windows))]
        {
            let mut command = portable_pty::CommandBuilder::new("sh");
            command.args(["-c", "sleep 5"]);
            command
        }
    }

    #[test]
    fn answers_cursor_position_queries_from_screen_state() {
        let _guard = test_guard();
        let mut screen = NextCoreScreen::new(80, 10);
        screen.set_cursor(2, 4);
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter {
                bytes: Arc::clone(&bytes),
            })));

        answer_terminal_queries("\x1b[6n", &screen, &writer);

        assert_eq!(bytes.lock().as_slice(), b"\x1b[3;5R");
    }

    #[test]
    fn answers_dec_private_cursor_position_queries_from_screen_state() {
        let _guard = test_guard();
        let mut screen = NextCoreScreen::new(80, 10);
        screen.set_cursor(2, 4);
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter {
                bytes: Arc::clone(&bytes),
            })));

        answer_terminal_queries("\x1b[?6n", &screen, &writer);

        assert_eq!(bytes.lock().as_slice(), b"\x1b[?3;5R");
    }

    #[test]
    fn answers_terminal_status_queries() {
        let _guard = test_guard();
        let screen = NextCoreScreen::new(80, 10);
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter {
                bytes: Arc::clone(&bytes),
            })));

        answer_terminal_queries("\x1b[5n", &screen, &writer);

        assert_eq!(bytes.lock().as_slice(), b"\x1b[0n");
    }

    #[test]
    fn answers_text_area_size_queries_from_screen_dimensions() {
        let _guard = test_guard();
        let screen = NextCoreScreen::new(132, 43);
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter {
                bytes: Arc::clone(&bytes),
            })));

        answer_terminal_queries("\x1b[18t", &screen, &writer);

        assert_eq!(bytes.lock().as_slice(), b"\x1b[8;43;132t");
    }

    #[test]
    fn answers_window_pixel_size_queries_from_headless_cell_dimensions() {
        let _guard = test_guard();
        let screen = NextCoreScreen::new(132, 43);
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter {
                bytes: Arc::clone(&bytes),
            })));

        answer_terminal_queries("\x1b[14t", &screen, &writer);

        assert_eq!(bytes.lock().as_slice(), b"\x1b[4;688;1056t");
    }

    #[test]
    fn answers_mode_report_queries_from_screen_state() {
        let _guard = test_guard();
        let mut screen = NextCoreScreen::new(80, 10);
        screen.feed(
            "\x1b[?1047h\x1b[?3h\x1b[?1h\x1b[?5h\x1b[?6h\x1b[?12l\x1b[?25l\x1b[?66h\x1b[?69h\x1b[?1002h\x1b[?1004h\x1b[?1005h\x1b[?1006h\x1b[?1007h\x1b[?1015h\x1b[?1016h\x1b[?1034h\x1b[?2004h\x1b[?2026h\x1b[4h",
        );
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter {
                bytes: Arc::clone(&bytes),
            })));

        answer_terminal_queries(
            "\x1b[?1$p\x1b[?3$p\x1b[?5$p\x1b[?6$p\x1b[?7$p\x1b[?12$p\x1b[?25$p\x1b[?66$p\x1b[?69$p\x1b[?1000$p\x1b[?1002$p\x1b[?1003$p\x1b[?1004$p\x1b[?1005$p\x1b[?1006$p\x1b[?1007$p\x1b[?1015$p\x1b[?1016$p\x1b[?1034$p\x1b[?47$p\x1b[?1047$p\x1b[?1049$p\x1b[?2004$p\x1b[?2026$p\x1b[4$p",
            &screen,
            &writer,
        );

        assert_eq!(
            bytes.lock().as_slice(),
            b"\x1b[?1;1$y\x1b[?3;1$y\x1b[?5;1$y\x1b[?6;1$y\x1b[?7;1$y\x1b[?12;2$y\x1b[?25;2$y\x1b[?66;1$y\x1b[?69;1$y\x1b[?1000;2$y\x1b[?1002;1$y\x1b[?1003;2$y\x1b[?1004;1$y\x1b[?1005;1$y\x1b[?1006;1$y\x1b[?1007;1$y\x1b[?1015;1$y\x1b[?1016;1$y\x1b[?1034;1$y\x1b[?47;2$y\x1b[?1047;1$y\x1b[?1049;2$y\x1b[?2004;1$y\x1b[?2026;1$y\x1b[4;1$y"
        );
    }

    #[test]
    fn answers_reset_mode_report_queries_from_default_screen_state() {
        let _guard = test_guard();
        let screen = NextCoreScreen::new(80, 10);
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter {
                bytes: Arc::clone(&bytes),
            })));

        answer_terminal_queries(
            "\x1b[?1$p\x1b[?3$p\x1b[?5$p\x1b[?6$p\x1b[?12$p\x1b[?66$p\x1b[?69$p\x1b[?1000$p\x1b[?1002$p\x1b[?1003$p\x1b[?1004$p\x1b[?1005$p\x1b[?1006$p\x1b[?1007$p\x1b[?1015$p\x1b[?1016$p\x1b[?1034$p\x1b[?47$p\x1b[?1047$p\x1b[?1049$p\x1b[?2004$p\x1b[?2026$p\x1b[4$p",
            &screen,
            &writer,
        );

        assert_eq!(
            bytes.lock().as_slice(),
            b"\x1b[?1;2$y\x1b[?3;2$y\x1b[?5;2$y\x1b[?6;2$y\x1b[?12;1$y\x1b[?66;2$y\x1b[?69;2$y\x1b[?1000;2$y\x1b[?1002;2$y\x1b[?1003;2$y\x1b[?1004;2$y\x1b[?1005;2$y\x1b[?1006;2$y\x1b[?1007;2$y\x1b[?1015;2$y\x1b[?1016;2$y\x1b[?1034;2$y\x1b[?47;2$y\x1b[?1047;2$y\x1b[?1049;2$y\x1b[?2004;2$y\x1b[?2026;2$y\x1b[4;2$y"
        );
    }

    #[test]
    fn answers_alternate_screen_mode_reports_independently() {
        let _guard = test_guard();
        let mut screen = NextCoreScreen::new(80, 10);
        screen.feed("\x1b[?47h\x1b[?1049h");
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter {
                bytes: Arc::clone(&bytes),
            })));

        answer_terminal_queries("\x1b[?47$p\x1b[?1047$p\x1b[?1049$p", &screen, &writer);

        assert_eq!(
            bytes.lock().as_slice(),
            b"\x1b[?47;1$y\x1b[?1047;2$y\x1b[?1049;1$y"
        );
    }

    #[test]
    fn answers_multiple_terminal_queries_in_chunk() {
        let _guard = test_guard();
        let mut screen = NextCoreScreen::new(80, 10);
        screen.feed("\x1b[?1h");
        screen.set_cursor(2, 4);
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter {
                bytes: Arc::clone(&bytes),
            })));

        answer_terminal_queries(
            "\x1b[5n\x1b[?1$p\x1b[?6n\x1b[14t\x1b[18t\x1b[6n\x1b[>c\x1b[c",
            &screen,
            &writer,
        );

        assert_eq!(
            bytes.lock().as_slice(),
            b"\x1b[0n\x1b[?1;1$y\x1b[?3;5R\x1b[4;160;640t\x1b[8;10;80t\x1b[3;5R\x1b[>0;0;0c\x1b[?64;1;2;6;9;15;18;21;22c"
        );
    }

    #[test]
    fn answers_terminal_queries_in_input_order() {
        let _guard = test_guard();
        let mut screen = NextCoreScreen::new(80, 10);
        screen.set_cursor(2, 4);
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter {
                bytes: Arc::clone(&bytes),
            })));

        answer_terminal_queries("x\x1b[c\x1b[6ny\x1b[5n\x1b[>0c", &screen, &writer);

        assert_eq!(
            bytes.lock().as_slice(),
            b"\x1b[?64;1;2;6;9;15;18;21;22c\x1b[3;5R\x1b[0n\x1b[>0;0;0c"
        );
    }

    #[test]
    fn answers_terminal_queries_across_split_chunks() {
        let _guard = test_guard();
        let mut screen = NextCoreScreen::new(80, 10);
        screen.feed("\x1b[?1h\x1b[4h");
        screen.set_cursor(2, 4);
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter {
                bytes: Arc::clone(&bytes),
            })));
        let mut pending = String::new();

        answer_terminal_queries_with_pending("x\x1b[?", &screen, &writer, &mut pending);
        assert_eq!(bytes.lock().as_slice(), b"");
        assert_eq!(pending, "\x1b[?");

        answer_terminal_queries_with_pending("1$p y\x1b[", &screen, &writer, &mut pending);
        answer_terminal_queries_with_pending("6", &screen, &writer, &mut pending);
        answer_terminal_queries_with_pending("n\x1b[>0", &screen, &writer, &mut pending);
        answer_terminal_queries_with_pending("c\x1b[4", &screen, &writer, &mut pending);
        answer_terminal_queries_with_pending("$p", &screen, &writer, &mut pending);

        assert_eq!(
            bytes.lock().as_slice(),
            b"\x1b[?1;1$y\x1b[3;5R\x1b[>0;0;0c\x1b[4;1$y"
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn terminal_query_pending_buffer_drops_overlong_incomplete_sequences() {
        let _guard = test_guard();
        let screen = NextCoreScreen::new(80, 10);
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter {
                bytes: Arc::clone(&bytes),
            })));
        let mut pending = String::new();

        answer_terminal_queries_with_pending(
            &format!(
                "\x1b[{}",
                "1".repeat(terminal_queries::MAX_PENDING_TERMINAL_QUERY_BYTES + 1)
            ),
            &screen,
            &writer,
            &mut pending,
        );

        assert!(pending.is_empty());
        assert!(bytes.lock().is_empty());
    }

    #[test]
    fn answers_primary_device_attributes_with_xterm_capabilities() {
        let _guard = test_guard();
        let screen = NextCoreScreen::new(80, 10);
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter {
                bytes: Arc::clone(&bytes),
            })));

        answer_terminal_queries("\x1b[c", &screen, &writer);

        assert_eq!(bytes.lock().as_slice(), b"\x1b[?64;1;2;6;9;15;18;21;22c");
    }

    #[test]
    fn answers_parameterized_primary_device_attributes() {
        let _guard = test_guard();
        let screen = NextCoreScreen::new(80, 10);
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter {
                bytes: Arc::clone(&bytes),
            })));

        answer_terminal_queries("\x1b[0c", &screen, &writer);

        assert_eq!(bytes.lock().as_slice(), b"\x1b[?64;1;2;6;9;15;18;21;22c");
    }

    #[test]
    fn answers_secondary_device_attributes() {
        let _guard = test_guard();
        let screen = NextCoreScreen::new(80, 10);
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter {
                bytes: Arc::clone(&bytes),
            })));

        answer_terminal_queries("\x1b[>c", &screen, &writer);

        assert_eq!(bytes.lock().as_slice(), b"\x1b[>0;0;0c");
    }

    #[test]
    fn answers_parameterized_secondary_device_attributes_without_primary() {
        let _guard = test_guard();
        let screen = NextCoreScreen::new(80, 10);
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedWriter {
                bytes: Arc::clone(&bytes),
            })));

        answer_terminal_queries("\x1b[>0c", &screen, &writer);

        assert_eq!(bytes.lock().as_slice(), b"\x1b[>0;0;0c");
    }

    #[test]
    fn decodes_pty_utf8_across_chunk_boundaries() {
        let _guard = test_guard();
        let mut pending = Vec::new();
        let bytes = "你A".as_bytes();

        assert_eq!(pty_io::decode_pty_chunk(&mut pending, &bytes[..1]), None);
        assert_eq!(
            pty_io::decode_pty_chunk(&mut pending, &bytes[1..3]),
            Some("你".to_string())
        );
        assert_eq!(
            pty_io::decode_pty_chunk(&mut pending, &bytes[3..]),
            Some("A".to_string())
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn chunks_paste_payload_without_splitting_utf8() {
        let text = format!(
            "{}{}",
            "a".repeat(input_pipeline::PASTE_CHUNK_BYTES),
            "你".repeat(3)
        );
        let chunks = input_pipeline::paste_chunks(&text, false);

        assert!(chunks.len() > 1);
        assert_eq!(chunks.concat(), text);
        assert!(chunks
            .iter()
            .all(|chunk| chunk.is_char_boundary(chunk.len())));
    }

    #[test]
    fn chunks_bracketed_paste_with_intact_markers() {
        let text = "x".repeat(input_pipeline::PASTE_CHUNK_BYTES + 10);
        let chunks = input_pipeline::paste_chunks(&text, true);

        assert_eq!(chunks.first().map(String::as_str), Some("\x1b[200~"));
        assert_eq!(chunks.last().map(String::as_str), Some("\x1b[201~"));
        assert_eq!(chunks[1..chunks.len() - 1].concat(), text);
    }

    #[test]
    fn translates_arrow_keys_in_application_cursor_mode() {
        let _guard = test_guard();

        assert_eq!(
            input_pipeline::application_cursor_input("\x1b[A\x1b[B\x1b[C\x1b[D", true),
            "\x1bOA\x1bOB\x1bOC\x1bOD"
        );
        assert_eq!(
            input_pipeline::application_cursor_input("x\x1b[C你", true),
            "x\x1bOC你"
        );
        assert_eq!(
            input_pipeline::application_cursor_input("\x1b[C", false),
            "\x1b[C"
        );
    }

    #[test]
    fn translates_home_end_keys_in_application_cursor_mode() {
        let _guard = test_guard();

        assert_eq!(
            input_pipeline::application_cursor_input("\x1b[H\x1b[F\x1b[1~\x1b[4~", true),
            "\x1bOH\x1bOF\x1bOH\x1bOF"
        );
        assert_eq!(
            input_pipeline::application_cursor_input("x\x1b[H你\x1b[F", true),
            "x\x1bOH你\x1bOF"
        );
        assert_eq!(
            input_pipeline::application_cursor_input("\x1b[H\x1b[F", false),
            "\x1b[H\x1b[F"
        );
    }

    /// Poll the visible screen until `needle` shows up, or give up.
    ///
    /// A real PTY answers on its own schedule, so a fixed sleep either flakes
    /// on a loaded machine or wastes seconds on a fast one.
    fn wait_for_screen_line(
        engine: &NextCoreEngine,
        pane_id: usize,
        mut accept: impl FnMut(&str) -> bool,
        description: &str,
        timeout: std::time::Duration,
    ) -> Result<String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let screen = engine.read_screen(pane_id)?;
            if let Some(line) = screen.lines.iter().find(|line| accept(line)) {
                return Ok(line.clone());
            }
            if std::time::Instant::now() >= deadline {
                let rendered = screen.lines.join("\n");
                anyhow::bail!("timed out waiting for {description}; screen was:\n{rendered}");
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    /// The GUI key path end to end: a termwiz key event becomes bytes, the
    /// bytes reach a real shell, and the shell's echo comes back on screen.
    ///
    /// The unit tests above cover encoding in isolation; this is the one that
    /// would catch an encoder that is individually correct but wired to the
    /// PTY wrong.
    #[test]
    fn encoded_keys_reach_a_real_shell_and_echo_back() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 12,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        // Type "echo nc-key-path" one key at a time, exactly as the GUI would
        // hand each keystroke over, then press Enter.
        let marker = "nc-key-path";
        let typed = format!("echo {marker}");
        for c in typed.chars() {
            let encoded = key_encoding::encode_key(
                termwiz::input::KeyCode::Char(c),
                termwiz::input::Modifiers::NONE,
            )
            .unwrap_or_else(|| panic!("no encoding for {:?}", c));
            engine.write_input(session.id, &encoded)?;
        }
        let enter = key_encoding::encode_key(
            termwiz::input::KeyCode::Enter,
            termwiz::input::Modifiers::NONE,
        )
        .expect("Enter encodes");
        engine.write_input(session.id, &enter)?;

        // Wait for the shell to *run* the command, not merely echo it back.
        // The echoed line contains "echo <marker>"; the result line contains
        // the marker alone. Accepting the echo would let a broken Enter
        // encoding pass, since the characters would still be on screen.
        let line = wait_for_screen_line(
            &engine,
            session.id,
            |line| line.contains(marker) && !line.contains("echo "),
            "the shell's output line for the typed command",
            std::time::Duration::from_secs(30),
        )?;
        assert_eq!(line.trim(), marker);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// Clearing the scrollback drops history but leaves the visible screen
    /// alone, unless the caller asks for both.
    #[test]
    fn erase_scrollback_drops_history_and_optionally_the_viewport() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 20,
            rows: 4,
            command_dir: None,
            command: Some(quiet_wait_command_for_test()),
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        // 12 rows through a 4-row viewport leaves 8 rows of scrollback.
        let lines: String = (0..12).map(|i| format!("row{i}\r\n")).collect();
        set_output_for_test(session.id, &lines)?;
        let before = engine.read_screen(session.id)?;
        // `scrollback_rows` counts history rows only; the viewport is separate.
        assert!(
            before.scrollback_rows > 0,
            "expected rows to have scrolled into history"
        );
        let visible_before = before.lines.join("\n");

        engine.erase_scrollback(session.id, false)?;
        let after = engine.read_screen(session.id)?;
        assert_eq!(after.scrollback_rows, 0, "history should be gone");
        assert_eq!(
            after.lines.join("\n"),
            visible_before,
            "clearing scrollback must not disturb what is on screen"
        );

        engine.erase_scrollback(session.id, true)?;
        let cleared = engine.read_screen(session.id)?;
        assert!(
            cleared.lines.iter().all(|line| line.trim().is_empty()),
            "clearing scrollback and viewport must empty the screen, got {:?}",
            cleared.lines
        );

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// A line too long for the window is one logical line, not several.
    /// Consumers reassemble it from this marker; without it, copying a
    /// wrapped command yields a newline the user never typed.
    #[test]
    fn soft_wrapped_rows_are_marked_and_hard_newlines_are_not() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 6,
            command_dir: None,
            command: Some(quiet_wait_command_for_test()),
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        // 25 chars across a 10-column screen: two soft wraps, then a hard
        // newline before a short line that does not wrap.
        set_output_for_test(session.id, &format!("{}\r\nshort", "x".repeat(25)))?;

        let styled = engine.read_styled_screen(session.id)?;
        let wrapped: Vec<bool> = styled.lines.iter().map(|line| line.wrapped).collect();

        assert_eq!(
            &wrapped[..4],
            &[true, true, false, false],
            "expected two soft-wrapped rows, then the hard-terminated row, \
             then the short row; got {wrapped:?}"
        );

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// Wheel scrolling steps the viewport exactly and returns to following
    /// the live tail at the bottom.
    #[test]
    fn relative_viewport_scroll_steps_and_resumes_following() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 20,
            rows: 4,
            command_dir: None,
            command: Some(quiet_wait_command_for_test()),
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        // 12 lines of history behind a 4-row viewport.
        let lines: String = (0..12).map(|i| format!("line{i}\r\n")).collect();
        set_output_for_test(session.id, &lines)?;

        let top_line = |engine: &NextCoreEngine| -> Result<String> {
            Ok(engine.read_screen(session.id)?.lines[0].trim().to_string())
        };
        let following = top_line(&engine)?;

        // Each step moves by exactly the requested number of rows.
        engine.scroll_viewport_by(session.id, -3)?;
        let up_three = top_line(&engine)?;
        engine.scroll_viewport_by(session.id, -1)?;
        let up_four = top_line(&engine)?;
        assert_ne!(up_three, following, "scrolling back must move the viewport");
        assert_ne!(up_four, up_three, "each notch must step again");

        // Scrolling forward past the bottom resumes following the live tail
        // rather than stopping one row short.
        engine.scroll_viewport_by(session.id, 100)?;
        assert_eq!(top_line(&engine)?, following);

        // ...and clamping back is symmetric: no amount of scrolling back runs
        // off the top of the scrollback.
        engine.scroll_viewport_by(session.id, -1000)?;
        let pinned = top_line(&engine)?;
        engine.scroll_viewport_by(session.id, -1000)?;
        assert_eq!(top_line(&engine)?, pinned);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// A mouse click only reaches the PTY once the application turns
    /// reporting on, and it goes through the same input lane as typing.
    #[test]
    fn mouse_reports_follow_the_session_modes() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 6,
            command_dir: None,
            command: Some(quiet_wait_command_for_test()),
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        let click = mouse_encoding::MouseEvent {
            kind: mouse_encoding::MouseEventKind::Press,
            button: Some(mouse_encoding::MouseButton::Left),
            column: 4,
            row: 2,
            modifiers: termwiz::input::Modifiers::NONE,
        };

        // Reporting is off by default, so the click is a no-op on the wire.
        reset_activity_for_test(session.id)?;
        engine.report_mouse(session.id, click)?;
        assert!(
            engine.activity(session.id)?.input.is_none(),
            "a click must not reach the PTY while mouse reporting is off"
        );

        // The application enables SGR button tracking.
        set_output_for_test(session.id, "\x1b[?1000h\x1b[?1006h")?;
        engine.report_mouse(session.id, click)?;
        let input = engine
            .activity(session.id)?
            .input
            .expect("input metrics after a reported click");
        assert_eq!(input.total_bytes as usize, "\x1b[<0;5;3M".len());

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn paste_activity_reports_last_write_metrics() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 3,
            command_dir: None,
            command: Some(quiet_wait_command_for_test()),
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        let text = format!(
            "{}{}",
            "a".repeat(input_pipeline::PASTE_CHUNK_BYTES + 1),
            "你"
        );
        engine.paste_input(session.id, &text)?;
        let paste = engine
            .activity(session.id)?
            .paste
            .expect("paste metrics after paste");

        assert_eq!(paste.total_pastes, 1);
        assert_eq!(paste.total_text_bytes, text.len() as u64);
        assert!(paste.total_chunks > 1);
        assert_eq!(paste.last_text_bytes, text.len());
        assert_eq!(paste.last_wire_bytes, text.len());
        assert_eq!(paste.last_chunk_count as u64, paste.total_chunks);
        assert!(!paste.last_bracketed);

        set_output_for_test(session.id, "\x1b[?2004h")?;
        engine.paste_input(session.id, "token")?;
        let paste = engine
            .activity(session.id)?
            .paste
            .expect("paste metrics after bracketed paste");

        assert_eq!(paste.total_pastes, 2);
        assert_eq!(paste.last_text_bytes, 5);
        assert_eq!(paste.last_wire_bytes, "\x1b[200~token\x1b[201~".len());
        assert_eq!(paste.last_chunk_count, 3);
        assert!(paste.last_bracketed);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn input_activity_reports_regular_writes() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        engine.write_input(session.id, "abc")?;
        engine.write_input(session.id, "你")?;
        let activity = engine.activity(session.id)?;
        let input = activity.input.expect("input metrics after writes");

        assert_eq!(input.total_writes, 2);
        assert_eq!(input.total_bytes, "abc你".len() as u64);
        assert_eq!(input.last_bytes, "你".len());
        assert!(activity.paste.is_none());

        engine.paste_input(session.id, "token")?;
        let activity = engine.activity(session.id)?;
        let input = activity.input.expect("input metrics after paste");
        let paste = activity.paste.expect("paste metrics after paste");

        assert_eq!(input.total_writes, 3);
        assert_eq!(input.last_bytes, paste.last_wire_bytes);
        assert_eq!(paste.total_pastes, 1);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn shell_uses_process_cwd_fallback_until_osc7_updates() -> Result<()> {
        // Hosted runners have no real desktop shell environment to probe;
        // developer machines and the Windows gate still run this.
        if std::env::var_os("GITHUB_ACTIONS").is_some() {
            return Ok(());
        }

        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: Some(quiet_wait_command_for_test()),
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        let process_cwd = engine
            .activity(session.id)?
            .process
            .and_then(|process| process.foreground_cwd.or(process.root_cwd));
        let shell_cwd = engine.shell(session.id)?.cwd;
        assert_eq!(shell_cwd, process_cwd);
        assert_eq!(engine.get_session(session.id)?.shell.cwd, shell_cwd);

        // The URL and the path it should become are platform-shaped: drive
        // letters and backslashes are Windows' answer, a rooted unix path is
        // everyone else's.
        #[cfg(windows)]
        let (osc7, expected) = (
            "\x1b]7;file://localhost/C:/Users/alex/osc-project\x07",
            "C:\\Users\\alex\\osc-project",
        );
        #[cfg(not(windows))]
        let (osc7, expected) = (
            "\x1b]7;file://localhost/home/alex/osc-project\x07",
            "/home/alex/osc-project",
        );
        set_output_for_test(session.id, osc7)?;
        assert_eq!(engine.shell(session.id)?.cwd.as_deref(), Some(expected));
        assert_eq!(
            engine.get_session(session.id)?.shell.cwd.as_deref(),
            Some(expected)
        );

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn output_activity_reports_screen_updates() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        // The default shell, not a timed command: `idle` is
        // `is_dead || is_idle`, so a command that exits early makes the
        // session report idle no matter how recent its output was, and the
        // assertion below races the process lifetime.
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        reset_activity_for_test(session.id)?;
        let before = engine.activity(session.id)?.output;
        let before_chunks = before.as_ref().map_or(0, |output| output.total_chunks);
        let before_bytes = before.as_ref().map_or(0, |output| output.total_bytes);

        set_output_for_test(session.id, "first")?;
        let activity = engine.activity(session.id)?;
        assert!(!activity.idle);
        let output = activity.output.expect("output metrics after screen update");
        assert!(output.total_chunks >= before_chunks + 1);
        assert!(output.total_bytes >= before_bytes + 5);
        assert!(output.last_bytes > 0);

        set_output_for_test(session.id, "second line")?;
        let output = engine
            .activity(session.id)?
            .output
            .expect("output metrics after second screen update");
        assert!(output.total_chunks >= before_chunks + 2);
        assert!(output.total_bytes >= before_bytes + 16);
        assert!(output.last_bytes > 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn health_reports_aggregate_io_metrics() -> Result<()> {
        // Hosted runners have no real desktop shell environment to probe;
        // developer machines and the Windows gate still run this.
        if std::env::var_os("GITHUB_ACTIONS").is_some() {
            return Ok(());
        }

        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        // A quiet command, not the default shell: the counts below are exact,
        // and a shell that gets its prompt out before the assertions adds an
        // output chunk of its own.
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 3,
            command_dir: None,
            command: Some(quiet_wait_command_for_test()),
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        // Output counters first, before anything is typed: the tty echoes
        // written input back as output chunks of its own, so once input is in
        // flight the output counts stop being exact.
        set_output_for_test(session.id, "one\ntwo\nthree\nfour")?;
        let _ = engine.read_screen(session.id)?;
        engine.scroll_viewport_to(session.id, 1)?;

        let health = engine.health()?;
        let io = health.io.expect("next-core io health");
        assert_eq!(health.pane_count, Some(1));
        assert_eq!(io.output_chunks, 1);
        assert_eq!(io.output_bytes, 18);
        assert_eq!(io.screen_reads, 1);
        assert_eq!(io.viewport_scrolls, 1);

        engine.write_input(session.id, "abc")?;
        engine.paste_input(session.id, "token")?;

        let health = engine.health()?;
        let io = health.io.expect("next-core io health");
        assert_eq!(io.input_writes, 2);
        assert_eq!(io.input_bytes, 8);
        assert_eq!(io.paste_count, 1);
        assert_eq!(io.paste_text_bytes, 5);
        let health = engine.health()?;
        let lifecycle = health.lifecycle.expect("next-core lifecycle health");
        assert_eq!(lifecycle.live_sessions, 1);
        assert_eq!(lifecycle.dead_sessions, 0);
        assert_eq!(lifecycle.total_created, 1);
        assert_eq!(lifecycle.total_destroyed, 0);
        assert_eq!(lifecycle.total_marked_dead, 0);

        engine.destroy_session(session.id)?;
        let lifecycle = engine
            .health()?
            .lifecycle
            .expect("next-core lifecycle health");
        assert_eq!(lifecycle.live_sessions, 0);
        assert_eq!(lifecycle.dead_sessions, 0);
        assert_eq!(lifecycle.total_created, 1);
        assert_eq!(lifecycle.total_destroyed, 1);
        assert_eq!(lifecycle.total_marked_dead, 1);
        assert_eq!(lifecycle.last_dead_reason.as_deref(), Some("destroyed"));
        Ok(())
    }

    #[test]
    fn prepare_command_applies_launch_env_overlay() {
        let expected_cwd = std::env::current_dir()
            .expect("current dir")
            .display()
            .to_string();
        let (command, cwd) = launch::prepare_command(
            None,
            Some(expected_cwd.clone()),
            vec![
                ("UNTERM_PROFILE".to_string(), "work-acme".to_string()),
                (
                    "HTTPS_PROXY".to_string(),
                    "http://127.0.0.1:7890".to_string(),
                ),
            ],
        );

        assert!(command.is_default_prog());
        assert_eq!(cwd.as_deref(), Some(expected_cwd.as_str()));
        assert_eq!(
            command
                .get_env("UNTERM_PROFILE")
                .and_then(|value| value.to_str()),
            Some("work-acme")
        );
        assert_eq!(
            command
                .get_env("HTTPS_PROXY")
                .and_then(|value| value.to_str()),
            Some("http://127.0.0.1:7890")
        );
    }

    #[test]
    fn launch_context_summarizes_profile_and_proxy_env_without_values() {
        let env = [
            ("GITHUB_TOKEN".to_string(), "secret-token".to_string()),
            ("UNTERM_PROFILE".to_string(), "work-acme".to_string()),
            (
                "HTTPS_PROXY".to_string(),
                "http://127.0.0.1:7890".to_string(),
            ),
            ("NO_PROXY".to_string(), "localhost".to_string()),
        ];
        let context = launch::launch_context(&env, &Default::default());

        assert_eq!(context.profile.as_deref(), Some("work-acme"));
        assert_eq!(context.proxy_env_keys, vec!["HTTPS_PROXY", "NO_PROXY"]);
        assert_eq!(context.env_key_count, 4);
        assert_eq!(context.policy.profile.as_deref(), Some("work-acme"));
        assert_eq!(
            context.policy.domain.decision,
            LaunchPolicyDecision::NotRequested
        );
        assert_eq!(context.policy.domain.supported, false);
        assert_eq!(
            context.policy.privilege.decision,
            LaunchPolicyDecision::NotRequested
        );
        assert_eq!(
            context.policy.proxy_rotation.decision,
            LaunchPolicyDecision::Deferred
        );
        assert_eq!(context.policy.proxy_rotation.supported, false);
        assert_eq!(
            context.policy.restart.decision,
            LaunchPolicyDecision::NotRequested
        );
        assert_eq!(
            context
                .policy
                .env
                .iter()
                .map(|binding| (binding.key.as_str(), binding.source))
                .collect::<Vec<_>>(),
            vec![
                ("GITHUB_TOKEN", LaunchEnvSource::Explicit),
                ("UNTERM_PROFILE", LaunchEnvSource::Profile),
                ("HTTPS_PROXY", LaunchEnvSource::Proxy),
                ("NO_PROXY", LaunchEnvSource::Proxy)
            ]
        );
    }

    #[test]
    fn session_snapshot_records_launch_env_keys_without_values() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: vec![
                ("UNTERM_PROFILE".to_string(), "work-acme".to_string()),
                ("GITHUB_TOKEN".to_string(), "secret-token".to_string()),
                (
                    "HTTPS_PROXY".to_string(),
                    "http://127.0.0.1:7890".to_string(),
                ),
            ],
            launch_policy: Default::default(),
        })?;

        let mut keys = engine.shell(session.id)?.launch_env_keys;
        keys.sort();
        assert_eq!(keys, vec!["GITHUB_TOKEN", "HTTPS_PROXY", "UNTERM_PROFILE"]);
        let launch_context = engine.shell(session.id)?.launch_context;
        assert_eq!(launch_context.profile.as_deref(), Some("work-acme"));
        assert_eq!(launch_context.proxy_env_keys, vec!["HTTPS_PROXY"]);
        assert_eq!(launch_context.env_key_count, 3);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn manages_session_metadata_lifecycle() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let cwd = std::env::current_dir()
            .ok()
            .map(|path| path.display().to_string());

        let first = engine.create_session(CreateSessionRequest {
            cols: 120,
            rows: 30,
            command_dir: cwd,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        assert_eq!(first.id, 1);
        assert!(first.is_active);

        let second = engine.split_session(SplitSessionRequest {
            source_pane_id: first.id,
            direction: crate::SplitDirection::Right,
            size_percent: 50,
            command_dir: None,
            command: None,
            env: vec![("UNTERM_PROFILE".to_string(), "work-acme".to_string())],
            launch_policy: Default::default(),
        })?;
        assert_eq!(second.id, 2);
        assert_eq!(
            second.shell.launch_context.profile.as_deref(),
            Some("work-acme")
        );
        assert_eq!(second.shell.launch_env_keys, vec!["UNTERM_PROFILE"]);
        assert!(engine.get_session(second.id)?.is_active);
        assert!(!engine.get_session(first.id)?.is_active);

        engine.focus_session(first.id)?;
        assert!(engine.get_session(first.id)?.is_active);
        assert!(!engine.get_session(second.id)?.is_active);

        engine.resize_session(first.id, 100, 25)?;
        let resized = engine.get_session(first.id)?;
        assert_eq!(resized.cols, 100);
        assert_eq!(resized.rows, 25);

        engine.destroy_session(first.id)?;
        assert!(engine.get_session(first.id).is_err());
        assert!(engine.get_session(second.id)?.is_active);
        assert_eq!(engine.list_sessions()?.len(), 1);
        engine.destroy_session(second.id)?;

        Ok(())
    }

    /// The snapshot has to say which side the new pane took, or a front end
    /// rebuilding the arrangement can only guess -- and guessing "second" is
    /// what put a `split left` pane on the right.
    #[test]
    fn a_leftward_split_records_the_side_it_took() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;

        let first = engine.create_session(CreateSessionRequest {
            cols: 120,
            rows: 30,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        let left = engine.split_session(SplitSessionRequest {
            source_pane_id: first.id,
            direction: crate::SplitDirection::Left,
            size_percent: 30,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        assert_eq!(
            left.split_side,
            Some(crate::next_core::layout::SplitSide::First)
        );
        // The ratio is the *first* half's share, and the new pane is that
        // half: 30% asked for is 30% recorded.
        assert_eq!(left.split_ratio, Some(0.3));

        let right = engine.split_session(SplitSessionRequest {
            source_pane_id: first.id,
            direction: crate::SplitDirection::Right,
            size_percent: 30,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        assert_eq!(
            right.split_side,
            Some(crate::next_core::layout::SplitSide::Second)
        );
        // Same 30% for the new pane, counted from the other end.
        assert_eq!(right.split_ratio, Some(0.7));

        for pane in [right.id, left.id, first.id] {
            engine.destroy_session(pane)?;
        }
        Ok(())
    }

    /// A split is written down once, when it is made. A divider the user
    /// then drags has to reach the engine too, or the pane comes back from
    /// a restart at the size it was born at rather than the one it was left
    /// at -- and the front end that knew better is exactly the process that
    /// is no longer there to be asked.
    #[test]
    fn a_moved_divider_replaces_the_ratio_the_split_was_made_with() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;

        let first = engine.create_session(CreateSessionRequest {
            cols: 120,
            rows: 30,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        let second = engine.split_session(SplitSessionRequest {
            source_pane_id: first.id,
            direction: crate::SplitDirection::Right,
            size_percent: 50,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        assert_eq!(second.split_ratio, Some(0.5));

        engine.set_split_ratio(second.id, 0.72)?;
        assert_eq!(engine.get_session(second.id)?.split_ratio, Some(0.72));
        // The axis is a property of the split, not of the divider's
        // position, so moving one must not disturb the other.
        assert_eq!(
            engine.get_session(second.id)?.split_axis,
            Some(crate::next_core::layout::SplitAxis::Horizontal)
        );

        // A pane with no cells is not a pane: whatever a front end sends,
        // the recorded ratio stays inside the range a split can be drawn at.
        engine.set_split_ratio(second.id, 0.0)?;
        assert_eq!(engine.get_session(second.id)?.split_ratio, Some(0.05));
        engine.set_split_ratio(second.id, 4.0)?;
        assert_eq!(engine.get_session(second.id)?.split_ratio, Some(0.95));

        assert!(
            engine.set_split_ratio(404, 0.5).is_err(),
            "a pane that does not exist has no divider to move"
        );

        engine.destroy_session(second.id)?;
        engine.destroy_session(first.id)?;
        Ok(())
    }

    #[test]
    fn propagates_reader_dead_marker_to_session_snapshots() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: Some(quiet_wait_command_for_test()),
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        mark_dead_for_test(session.id)?;
        let snapshot = engine.get_session(session.id)?;
        assert!(snapshot.is_dead);
        assert_eq!(snapshot.dead_reason.as_deref(), Some("test_dead_marker"));
        let lifecycle = engine
            .health()?
            .lifecycle
            .expect("next-core lifecycle health");
        assert_eq!(lifecycle.live_sessions, 0);
        assert_eq!(lifecycle.dead_sessions, 1);
        assert_eq!(lifecycle.total_marked_dead, 1);
        assert_eq!(
            lifecycle.last_dead_reason.as_deref(),
            Some("test_dead_marker")
        );
        assert!(engine.activity(session.id)?.idle);
        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn session_activity_tracks_recent_next_core_io() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: Some(quiet_wait_command_for_test()),
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        reset_activity_for_test(session.id)?;

        set_output_for_test(session.id, "recent output")?;
        let _ = engine.read_visible_text(session.id)?;
        engine.scroll_viewport_to(session.id, 0)?;
        let activity = engine.activity(session.id)?;
        assert!(!activity.idle);
        assert!(!activity.foreground_process.is_empty());
        assert!(activity.process.is_some());
        let screen = activity.screen.expect("screen activity");
        assert_eq!(screen.total_reads, 1);
        assert_eq!(screen.total_viewport_scrolls, 1);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_range_reads_update_activity_metrics() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "one\ntwo\nthree\nfour")?;
        reset_activity_for_test(session.id)?;

        let _ = engine.read_lines(session.id, 0, 2)?;
        let _ = engine.read_scrollback(session.id, 2)?;
        let _ = engine.read_scrollback_text(
            session.id,
            ScrollbackTextRequest {
                start_line: Some(0),
                end_line: Some(2),
                tail_lines: None,
                escapes: false,
            },
        )?;
        let _ = engine.read_styled_scrollback(
            session.id,
            ScrollbackTextRequest {
                start_line: Some(0),
                end_line: Some(2),
                tail_lines: None,
                escapes: false,
            },
        )?;
        let _ = engine.search(session.id, "two", crate::SearchMode::CaseSensitive, 10)?;

        let activity = engine.activity(session.id)?;
        let screen = activity.screen.expect("screen activity");
        assert_eq!(screen.total_reads, 5);

        let health = engine.health()?;
        assert_eq!(health.io.expect("next-core io health").screen_reads, 5);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn exposes_buffered_output_for_screen_reads() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "line-one\r\nnext-core-output\r\nline-three\r\n")?;

        let text = engine.read_visible_text(session.id)?;
        assert!(text.contains("next-core-output"));
        assert!(!engine
            .search(
                session.id,
                "next-core-output",
                crate::SearchMode::CaseSensitive,
                1
            )?
            .is_empty());

        let scrollback = engine.read_scrollback_text(
            session.id,
            ScrollbackTextRequest {
                start_line: None,
                end_line: None,
                tail_lines: Some(10),
                escapes: false,
            },
        )?;
        assert!(scrollback.text.contains("next-core-output"));

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_snapshots_report_revision_changes() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 4,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        let initial = engine.read_screen(session.id)?;
        assert_eq!(initial.revision, 0);
        assert_eq!(initial.dirty_rows, None);

        set_output_for_test(session.id, "first")?;
        let first = engine.read_screen(session.id)?;
        let styled = engine.read_styled_screen(session.id)?;
        assert!(first.revision > 0);
        assert_eq!(first.dirty_rows, Some(DirtyRows { start: 0, end: 0 }));
        assert_eq!(styled.revision, first.revision);
        assert_eq!(styled.dirty_rows, first.dirty_rows);

        set_output_for_test(session.id, "second")?;
        let second = engine.read_screen(session.id)?;
        assert!(second.revision > first.revision);
        assert_eq!(second.dirty_rows, Some(DirtyRows { start: 0, end: 0 }));

        engine.resize_session(session.id, 80, 3)?;
        let resized = engine.read_screen(session.id)?;
        assert!(resized.revision > second.revision);
        assert_eq!(resized.dirty_rows, Some(DirtyRows { start: 0, end: 2 }));

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn render_frames_report_full_then_dirty_delta() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 12,
            rows: 4,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        let screen_handle = screen_for_test(session.id)?;

        screen_handle.lock().feed("alpha");
        let full = engine.read_render_frame(session.id, None)?;
        assert!(full.full);
        assert_eq!(full.dirty_rows, Some(DirtyRows { start: 0, end: 3 }));
        assert_eq!(full.lines.len(), 4);
        assert!(full.lines.iter().all(|line| line.cells.len() == 12));
        assert_eq!(full.lines[0].row, 0);
        assert_eq!(
            full.lines[0]
                .cells
                .iter()
                .map(|cell| cell.ch)
                .collect::<String>(),
            "alpha       "
        );
        assert_eq!(
            full.lines[3]
                .cells
                .iter()
                .map(|cell| cell.ch)
                .collect::<String>(),
            "            "
        );

        let unchanged = engine.read_render_frame(session.id, Some(full.revision))?;
        assert!(!unchanged.full);
        assert_eq!(unchanged.dirty_rows, None);
        assert!(unchanged.lines.is_empty());

        screen_handle.lock().feed("!");
        let delta = engine.read_render_frame(session.id, Some(full.revision))?;
        assert!(!delta.full);
        assert_eq!(delta.dirty_rows, Some(DirtyRows { start: 0, end: 0 }));
        assert_eq!(delta.lines.len(), 1);
        assert_eq!(delta.lines[0].cells.len(), 12);
        assert_eq!(delta.lines[0].row, 0);
        assert_eq!(
            delta.lines[0]
                .cells
                .iter()
                .map(|cell| cell.ch)
                .collect::<String>(),
            "alpha!      "
        );

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn render_frames_accumulate_dirty_rows_across_chunks() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 12,
            rows: 4,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        let screen_handle = screen_for_test(session.id)?;

        screen_handle.lock().feed("seed");
        let baseline = engine.read_render_frame(session.id, None)?;
        assert!(baseline.full);

        screen_handle.lock().feed("\x1b[1;1HA");
        screen_handle.lock().feed("\x1b[2;1HB");
        let delta = engine.read_render_frame(session.id, Some(baseline.revision))?;
        assert!(!delta.full);
        assert_eq!(delta.dirty_rows, Some(DirtyRows { start: 0, end: 1 }));
        assert_eq!(delta.lines.len(), 2);
        assert!(delta.lines.iter().all(|line| line.cells.len() == 12));
        assert_eq!(delta.lines[0].row, 0);
        assert_eq!(delta.lines[0].cells[0].ch, 'A');
        assert_eq!(delta.lines[1].row, 1);
        assert_eq!(delta.lines[1].cells[0].ch, 'B');

        let stale = engine.read_render_frame(session.id, Some(baseline.revision))?;
        assert!(stale.full);
        assert_eq!(stale.dirty_rows, Some(DirtyRows { start: 0, end: 3 }));
        assert_eq!(stale.lines.len(), 4);
        assert!(stale.lines.iter().all(|line| line.cells.len() == 12));

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn render_frames_mark_cursor_only_moves_dirty() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 12,
            rows: 4,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        let screen_handle = screen_for_test(session.id)?;

        screen_handle.lock().feed("cursor");
        let baseline = engine.read_render_frame(session.id, None)?;
        assert_eq!(baseline.cursor.x, 6);

        screen_handle.lock().feed("\x1b[2D");
        let delta = engine.read_render_frame(session.id, Some(baseline.revision))?;
        assert!(!delta.full);
        assert_eq!(delta.dirty_rows, Some(DirtyRows { start: 0, end: 0 }));
        assert_eq!(delta.cursor.x, 4);
        assert_eq!(delta.lines.len(), 1);
        assert_eq!(delta.lines[0].row, 0);
        assert_eq!(delta.lines[0].cells.len(), 12);
        assert_eq!(
            delta.lines[0]
                .cells
                .iter()
                .map(|cell| cell.ch)
                .collect::<String>(),
            "cursor      "
        );

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_wraps_text_at_configured_columns() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 5,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "abcdefgh")?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.cols, 5);
        assert_eq!(screen.lines, vec!["abcde", "fgh"]);
        assert_eq!(screen.cursor.x, 3);
        assert_eq!(screen.cursor.y, 1);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_honors_decawm_auto_wrap_mode() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 5,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "\x1b[?7labcdef")?;
        let disabled = engine.read_screen(session.id)?;
        assert_eq!(disabled.lines, vec!["abcdf"]);
        assert_eq!(disabled.cursor.x, 5);
        assert_eq!(disabled.cursor.y, 0);

        set_output_for_test(session.id, "\x1b[?7labcde\x1b[?7hf")?;
        let reenabled = engine.read_screen(session.id)?;
        assert_eq!(reenabled.lines, vec!["abcde", "f"]);
        assert_eq!(reenabled.cursor.x, 1);
        assert_eq!(reenabled.cursor.y, 1);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_treats_tab_as_cursor_movement() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "a\tb")?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["a       b"]);
        assert_eq!(screen.cursor.x, 9);
        assert_eq!(screen.cursor.y, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_treats_vertical_tab_and_form_feed_as_newline() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "one\x0btwo\x0cthree\x0bfour")?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["two", "three", "four"]);
        assert_eq!(screen.scrollback_rows, 1);
        assert_eq!(engine.read_scrollback(session.id, 10)?, vec!["one"]);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_clamps_tab_at_right_edge_without_wrapping() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 5,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "abcd\tZ")?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["abcdZ"]);
        assert_eq!(screen.cursor.x, 5);
        assert_eq!(screen.cursor.y, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_supports_custom_tab_stops() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 12,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "\x1b[5G\x1bH\rA\tB")?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["A   B"]);
        assert_eq!(screen.cursor.x, 5);
        assert_eq!(screen.cursor.y, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_forward_tab_csi() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 20,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "A\x1b[2IB")?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["A               B"]);
        assert_eq!(screen.cursor.x, 17);
        assert_eq!(screen.cursor.y, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_forward_tab_csi_uses_custom_tab_stops() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 12,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "\x1b[5G\x1bH\x1b[9G\x1bH\rA\x1b[2IB")?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["A       B"]);
        assert_eq!(screen.cursor.x, 9);
        assert_eq!(screen.cursor.y, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_forward_tab_csi_clamps_at_right_edge() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "A\x1b[5IZ")?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["A        Z"]);
        assert_eq!(screen.cursor.x, 10);
        assert_eq!(screen.cursor.y, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_clears_current_tab_stop() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 12,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "\x1b[5G\x1bH\x1b[0g\rA\tB")?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["A       B"]);
        assert_eq!(screen.cursor.x, 9);
        assert_eq!(screen.cursor.y, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_clears_all_tab_stops() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 6,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "\x1b[3gA\tB")?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["A    B"]);
        assert_eq!(screen.cursor.x, 6);
        assert_eq!(screen.cursor.y, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_wraps_wide_cells_before_right_edge() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 5,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "abcd你")?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["abcd", "你"]);
        assert_eq!(screen.cursor.x, 2);
        assert_eq!(screen.cursor.y, 1);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_truncates_existing_lines_on_column_resize() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "abcdef")?;
        engine.resize_session(session.id, 4, 3)?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.cols, 4);
        assert_eq!(screen.lines, vec!["abcd"]);
        assert_eq!(screen.cursor.x, 3);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// Growing a pane brings back what shrinking it took away.
    ///
    /// Pinned rather than fixed: this already works, and the point of writing
    /// it down is that a full-screen program depends on both halves and the
    /// dependency is invisible from the resize code alone. Such a program
    /// anchors its interface to the *bottom* of the screen, so if growing
    /// handed out blank rows instead of the banked ones, the frame it drew
    /// before the resize would sit stranded at the old bottom while its input
    /// line moved to the new one -- with the height the pane gained lying
    /// blank in between. That symptom was reported and this is what ruled the
    /// resize path out of it.
    #[test]
    fn growing_a_pane_restores_the_rows_shrinking_it_banked() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 6,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix")?;
        let full = engine.read_screen(session.id)?;
        assert_eq!(full.lines, vec!["one", "two", "three", "four", "five", "six"]);
        let from_bottom = full.rows as isize - 1 - full.cursor.y;

        engine.resize_session(session.id, 10, 3)?;
        let small = engine.read_screen(session.id)?;
        assert_eq!(
            small.lines,
            vec!["four", "five", "six"],
            "shrinking banks the top rows"
        );

        engine.resize_session(session.id, 10, 6)?;
        let back = engine.read_screen(session.id)?;
        assert_eq!(
            back.lines,
            vec!["one", "two", "three", "four", "five", "six"],
            "growing must take the banked rows back, not hand out blank ones"
        );
        assert_eq!(
            back.rows as isize - 1 - back.cursor.y,
            from_bottom,
            "the cursor must keep its distance from the bottom, which is what \
             a full-screen program positions its interface against"
        );

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// A window that reported no size used to cost a pane everything it held.
    ///
    /// Every fallback between a window's size and a pane's grid floored at
    /// one cell, so a window reporting 0x0 -- unmapped, minimised, caught
    /// mid display change -- told the pane it was one column wide, which
    /// truncated each line and the scrollback with them to a single
    /// character. Nothing could undo it afterwards. The narrowing itself is
    /// right; believing that width is not.
    #[test]
    fn resize_refuses_a_grid_too_narrow_to_hold_the_pane() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 40,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "== Novel Studio ==")?;

        let err = engine
            .resize_session(session.id, 1, 3)
            .expect_err("a one-column grid is a number nobody meant");
        assert!(
            err.to_string().contains("refusing to resize"),
            "unexpected error: {}",
            err
        );
        let err = engine
            .resize_session(session.id, 40, 1)
            .expect_err("a one-row grid is the same mistake sideways");
        assert!(
            err.to_string().contains("refusing to resize"),
            "unexpected error: {}",
            err
        );

        // Refused, so the pane still has its line and the size it was drawn at.
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.cols, 40);
        assert_eq!(screen.lines, vec!["== Novel Studio =="]);

        // And a real narrowing still narrows.
        engine.resize_session(session.id, 4, 3)?;
        assert_eq!(engine.read_screen(session.id)?.lines, vec!["== N"]);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_strips_terminal_control_sequences() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(
            session.id,
            "\x1b[1t\x1b[6n\x1b[c\x1b[31mred\x1b[0m\rOK\nplain\x1b]0;title\x07\n",
        )?;

        let text = engine.read_visible_text(session.id)?;
        assert!(text.contains("OKd"));
        assert!(text.contains("plain"));
        assert!(!text.contains("\x1b["));
        assert!(!text.contains("title"));

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_osc_title_updates() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        // The shell announces itself with a title of its own within the first
        // moments of starting. Injecting before that lands means it overwrites
        // this a moment later -- which failed about one run in ten, with a
        // title nobody in this test had written.
        settle_session_for_test(session.id)?;
        set_output_for_test(
            session.id,
            "before\x1b]0;Claude Session\x07after\x1b]2;Codex Pane\x1b\\",
        )?;

        assert_eq!(engine.read_visible_text(session.id)?, "beforeafter");
        assert_eq!(engine.get_session(session.id)?.title, "Codex Pane");
        assert_eq!(engine.list_sessions()?[0].title, "Codex Pane");

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_title_stack_window_operations() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            "pre\x1b]2;Original\x07\x1b[22;0t\x1b]2;Temporary\x1b\\mid\x1b[22;2t\x1b]2;Nested\x07\x1b[23;2t\x1b[23;0tpost",
        )?;

        assert_eq!(engine.read_visible_text(session.id)?, "premidpost");
        assert_eq!(engine.get_session(session.id)?.title, "Original");

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_tracks_osc8_hyperlinks() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 2,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            "pre \x1b]8;id=one;https://example.test/item\x07link\x1b]8;;\x07 post\nnext",
        )?;

        assert_eq!(engine.read_visible_text(session.id)?, "pre link post\nnext");
        let styled = engine.read_styled_screen(session.id)?;
        let first = &styled.lines[0].cells;
        assert_eq!(first[4].ch, 'l');
        assert_eq!(
            first[4].style.hyperlink.as_deref(),
            Some("https://example.test/item")
        );
        assert_eq!(
            first[7].style.hyperlink.as_deref(),
            Some("https://example.test/item")
        );
        assert_eq!(first[8].style.hyperlink, None);

        let scrollback = engine.read_styled_scrollback(
            session.id,
            ScrollbackTextRequest {
                start_line: Some(0),
                end_line: Some(1),
                tail_lines: None,
                escapes: false,
            },
        )?;
        assert_eq!(
            scrollback.lines[0].cells[4].style.hyperlink.as_deref(),
            Some("https://example.test/item")
        );

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_osc7_cwd_updates() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let launch_cwd = std::env::current_dir()?.display().to_string();
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: Some(launch_cwd),
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            "before\x1b]7;file://localhost/D:/code/unterm%20next\x07after",
        )?;

        #[cfg(windows)]
        let expected = "D:\\code\\unterm next";
        #[cfg(not(windows))]
        let expected = "/D:/code/unterm next";

        assert_eq!(engine.read_visible_text(session.id)?, "beforeafter");
        assert_eq!(engine.shell(session.id)?.cwd.as_deref(), Some(expected));
        assert_eq!(
            engine.get_session(session.id)?.shell.cwd.as_deref(),
            Some(expected)
        );

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_ignores_invalid_osc7_cwd_updates() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let launch_cwd = std::env::current_dir()?.display().to_string();
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: Some(launch_cwd),
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        let initial = engine.shell(session.id)?.cwd;

        set_output_for_test(session.id, "\x1b]7;https://example.test/D:/bad\x1b\\")?;

        assert_eq!(engine.shell(session.id)?.cwd, initial);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_tracks_sgr_cell_attributes() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(
            session.id,
            concat!(
                "\x1b[1;31mR",
                "\x1b[0mN",
                "\x1b[2mF",
                "\x1b[1mB",
                "\x1b[22mI",
                "\x1b[9mS",
                "\x1b[29mT",
                "\x1b[8mH",
                "\x1b[28mV",
                "\x1b[5mL",
                "\x1b[6mR",
                "\x1b[25mQ",
                "\x1b[53mO",
                "\x1b[55mP",
                "\x1b[73mA",
                "\x1b[74mB",
                "\x1b[75mC",
                "\x1b[3;4;7;38;5;202;48;2;1;2;3mX",
                "\x1b[22;23;24;25;27;28;29;55;75;39;49mY"
            ),
        )?;

        assert_eq!(engine.read_visible_text(session.id)?, "RNFBISTHVLRQOPABCXY");
        let attrs = viewport_attrs_for_test(session.id)?;
        let line = &attrs[0];

        assert!(line[0].bold);
        assert!(!line[0].faint);
        assert_eq!(line[0].fg, Some(TerminalColor::Palette(1)));
        assert_eq!(line[0].bg, None);

        assert_eq!(line[1], CellAttributes::default());

        assert!(line[2].faint);
        assert!(!line[2].bold);
        assert!(line[3].bold);
        assert!(line[3].faint);
        assert!(!line[4].bold);
        assert!(!line[4].faint);

        assert!(line[5].strikethrough);
        assert!(!line[6].strikethrough);
        assert!(line[7].hidden);
        assert!(!line[8].hidden);

        assert_eq!(line[9].blink, Some(StyledBlink::Slow));
        assert_eq!(line[10].blink, Some(StyledBlink::Rapid));
        assert_eq!(line[11].blink, None);
        assert!(line[12].overline);
        assert!(!line[13].overline);
        assert_eq!(
            line[14].vertical_align,
            Some(StyledVerticalAlign::SuperScript)
        );
        assert_eq!(
            line[15].vertical_align,
            Some(StyledVerticalAlign::SubScript)
        );
        assert_eq!(line[16].vertical_align, None);

        assert!(line[17].italic);
        assert!(line[17].underline);
        assert_eq!(line[17].underline_style, Some(StyledUnderline::Single));
        assert!(line[17].inverse);
        assert_eq!(line[17].fg, Some(TerminalColor::Palette(202)));
        assert_eq!(line[17].bg, Some(TerminalColor::Rgb(1, 2, 3)));

        assert_eq!(line[18], CellAttributes::default());

        let styled = engine.read_styled_screen(session.id)?;
        assert_eq!(styled.lines[0].row, 0);
        assert_eq!(styled.lines[0].cells[0].ch, 'R');
        assert!(styled.lines[0].cells[0].style.bold);
        assert!(styled.lines[0].cells[2].style.faint);
        assert!(!styled.lines[0].cells[4].style.bold);
        assert!(!styled.lines[0].cells[4].style.faint);
        assert!(styled.lines[0].cells[5].style.strikethrough);
        assert!(!styled.lines[0].cells[6].style.strikethrough);
        assert!(styled.lines[0].cells[7].style.hidden);
        assert!(!styled.lines[0].cells[8].style.hidden);
        assert_eq!(
            styled.lines[0].cells[9].style.blink,
            Some(StyledBlink::Slow)
        );
        assert_eq!(
            styled.lines[0].cells[10].style.blink,
            Some(StyledBlink::Rapid)
        );
        assert_eq!(styled.lines[0].cells[11].style.blink, None);
        assert!(styled.lines[0].cells[12].style.overline);
        assert!(!styled.lines[0].cells[13].style.overline);
        assert_eq!(
            styled.lines[0].cells[14].style.vertical_align,
            Some(StyledVerticalAlign::SuperScript)
        );
        assert_eq!(
            styled.lines[0].cells[15].style.vertical_align,
            Some(StyledVerticalAlign::SubScript)
        );
        assert_eq!(styled.lines[0].cells[16].style.vertical_align, None);
        assert_eq!(
            styled.lines[0].cells[0].style.fg,
            Some(StyledColor::Palette(1))
        );
        assert_eq!(
            styled.lines[0].cells[17].style.bg,
            Some(StyledColor::Rgb(1, 2, 3))
        );
        assert_eq!(styled.lines[0].cells[18].style, CellStyle::default());

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_reverse_video_mode_to_styled_cells() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let mut screen = NextCoreScreen::new(80, 24);
        screen.feed("\x1b[?5hA\x1b[7mB\x1b[27mC");

        assert_eq!(screen.snapshot_viewport_lines()[0], "ABC");
        let styled = screen.styled_viewport_lines(0);
        let cells = &styled[0].cells;
        assert!(cells[0].style.inverse);
        assert!(!cells[1].style.inverse);
        assert!(cells[2].style.inverse);

        screen.feed("\x1b[?5lD");
        assert_eq!(screen.snapshot_viewport_lines()[0], "ABCD");
        let styled = screen.styled_viewport_lines(0);
        let cells = &styled[0].cells;
        assert!(!cells[0].style.inverse);
        assert!(cells[1].style.inverse);
        assert!(!cells[2].style.inverse);
        assert!(!cells[3].style.inverse);

        Ok(())
    }

    #[test]
    fn screen_buffer_keeps_reverse_video_isolated_across_alternate_screen() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let mut screen = NextCoreScreen::new(80, 3);

        screen.feed("\x1b[?5hM\x1b[?1049hA");
        assert_eq!(screen.snapshot_viewport_lines(), vec!["A"]);
        let styled = screen.styled_viewport_lines(0);
        assert!(!styled[0].cells[0].style.inverse);

        screen.feed("\x1b[?5hB\x1b[?1049lC");
        assert_eq!(screen.snapshot_viewport_lines(), vec!["MC"]);
        let styled = screen.styled_viewport_lines(0);
        assert!(styled[0].cells[0].style.inverse);
        assert!(styled[0].cells[1].style.inverse);

        Ok(())
    }

    #[test]
    fn screen_buffer_tracks_colon_sgr_extended_colors() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(
            session.id,
            concat!(
                "\x1b[38:5:196mR",
                "\x1b[48:5:25mB",
                "\x1b[38:2::1:2:3mF",
                "\x1b[48:2:0:4:5:6mG",
                "\x1b[0mN"
            ),
        )?;

        assert_eq!(engine.read_visible_text(session.id)?, "RBFGN");
        let attrs = viewport_attrs_for_test(session.id)?;
        let line = &attrs[0];

        assert_eq!(line[0].fg, Some(TerminalColor::Palette(196)));
        assert_eq!(line[1].fg, Some(TerminalColor::Palette(196)));
        assert_eq!(line[1].bg, Some(TerminalColor::Palette(25)));
        assert_eq!(line[2].fg, Some(TerminalColor::Rgb(1, 2, 3)));
        assert_eq!(line[2].bg, Some(TerminalColor::Palette(25)));
        assert_eq!(line[3].fg, Some(TerminalColor::Rgb(1, 2, 3)));
        assert_eq!(line[3].bg, Some(TerminalColor::Rgb(4, 5, 6)));
        assert_eq!(line[4], CellAttributes::default());

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_tracks_sgr_underline_colors() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(
            session.id,
            concat!(
                "\x1b[4;58;5;220mP",
                "\x1b[58;2;1;2;3mR",
                "\x1b[58:5:45mC",
                "\x1b[58:2::4:5:6mT",
                "\x1b[59mD",
                "\x1b[24mN"
            ),
        )?;

        let attrs = viewport_attrs_for_test(session.id)?;
        let line = &attrs[0];
        assert!(line[0].underline);
        assert_eq!(line[0].underline_color, Some(TerminalColor::Palette(220)));
        assert_eq!(line[1].underline_color, Some(TerminalColor::Rgb(1, 2, 3)));
        assert_eq!(line[2].underline_color, Some(TerminalColor::Palette(45)));
        assert_eq!(line[3].underline_color, Some(TerminalColor::Rgb(4, 5, 6)));
        assert!(line[4].underline);
        assert_eq!(line[4].underline_color, None);
        assert_eq!(line[5], CellAttributes::default());

        let styled = engine.read_styled_screen(session.id)?;
        assert_eq!(
            styled.lines[0].cells[0].style.underline_color,
            Some(StyledColor::Palette(220))
        );
        assert_eq!(
            styled.lines[0].cells[1].style.underline_color,
            Some(StyledColor::Rgb(1, 2, 3))
        );
        assert_eq!(
            styled.lines[0].cells[2].style.underline_color,
            Some(StyledColor::Palette(45))
        );
        assert_eq!(
            styled.lines[0].cells[3].style.underline_color,
            Some(StyledColor::Rgb(4, 5, 6))
        );
        assert_eq!(styled.lines[0].cells[4].style.underline_color, None);
        assert_eq!(styled.lines[0].cells[5].style, CellStyle::default());

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_tracks_extended_underline_styles() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(
            session.id,
            concat!(
                "\x1b[4:3mC",
                "\x1b[4:4mD",
                "\x1b[4:5mS",
                "\x1b[4:0mN",
                "\x1b[21mB",
                "\x1b[24mR"
            ),
        )?;

        let attrs = viewport_attrs_for_test(session.id)?;
        let line = &attrs[0];
        assert!(line[0].underline);
        assert_eq!(line[0].underline_style, Some(StyledUnderline::Curly));
        assert!(!line[0].italic);
        assert!(line[1].underline);
        assert_eq!(line[1].underline_style, Some(StyledUnderline::Dotted));
        assert!(line[2].underline);
        assert_eq!(line[2].underline_style, Some(StyledUnderline::Dashed));
        assert!(!line[3].underline);
        assert_eq!(line[3].underline_style, None);
        assert!(line[4].underline);
        assert_eq!(line[4].underline_style, Some(StyledUnderline::Double));
        assert!(!line[5].underline);
        assert_eq!(line[5].underline_style, None);

        let styled = engine.read_styled_screen(session.id)?;
        assert_eq!(
            styled.lines[0].cells[0].style.underline_style,
            Some(StyledUnderline::Curly)
        );
        assert_eq!(
            styled.lines[0].cells[1].style.underline_style,
            Some(StyledUnderline::Dotted)
        );
        assert_eq!(
            styled.lines[0].cells[2].style.underline_style,
            Some(StyledUnderline::Dashed)
        );
        assert_eq!(
            styled.lines[0].cells[4].style.underline_style,
            Some(StyledUnderline::Double)
        );
        assert_eq!(styled.lines[0].cells[5].style, CellStyle::default());

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_tracks_wide_character_cells() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "你A")?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines[0], "你A");
        assert_eq!(screen.cursor.x, 3);

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(cells[0].ch, '你');
        assert_eq!(cells[0].width, 2);
        assert_eq!(cells[1].width, 0);
        assert_eq!(cells[2].ch, 'A');
        assert_eq!(cells[2].width, 1);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// A TUI patching one cell must not leave half a wide character behind.
    ///
    /// Claude Code and Codex redraw by jumping to an absolute column and
    /// rewriting a single character (`ESC[3G` then the glyph). When the
    /// column it lands on is the left half of a CJK character, the right half
    /// has to go with it -- otherwise the row keeps a stale continuation cell
    /// that owns no character.
    #[test]
    fn screen_buffer_clears_the_right_half_when_a_wide_cell_is_overwritten() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "\u{4f60}\u{597d}\x1b[1GA")?;

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(cells[0].ch, 'A');
        assert_eq!(cells[0].width, 1);
        assert_eq!(
            cells[1].width, 1,
            "the right half of the overwritten wide cell is still a continuation"
        );
        assert_eq!(cells[1].ch, ' ');
        assert_eq!(cells[2].ch, '\u{597d}');

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// `ESC[nX` erases in place rather than to the end of the line, and cuts
    /// between the halves of a wide character exactly the same way.
    #[test]
    fn screen_buffer_clears_the_left_half_when_chars_are_erased_mid_wide_cell() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        // Erase one cell starting at column 2 -- the right half of the first
        // wide character.
        set_output_for_test(session.id, "\u{4f60}\u{597d}\x1b[2G\x1b[1X")?;

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(
            cells[0].width, 1,
            "the surviving left half still claims two columns"
        );
        assert_eq!(cells[0].ch, ' ');
        assert_eq!(cells[1].ch, ' ');
        assert_eq!(cells[2].ch, '\u{597d}');

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// `ESC[K` is the other half of how these TUIs repaint, and it can cut
    /// between the halves of a wide character just as a write can.
    #[test]
    fn screen_buffer_clears_the_left_half_when_a_line_is_erased_mid_wide_cell() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        // Erase from column 2 -- the right half of the first wide character.
        set_output_for_test(session.id, "\u{4f60}\u{597d}\x1b[2G\x1b[K")?;

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(
            cells[0].width, 1,
            "the surviving left half still claims two columns"
        );
        assert_eq!(cells[0].ch, ' ');
        assert_eq!(cells[1].ch, ' ');

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// The mirror case: landing on the right half has to erase the left one,
    /// or the wide glyph keeps painting over the character that replaced it.
    #[test]
    fn screen_buffer_clears_the_left_half_when_a_wide_cell_is_overwritten() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "\u{4f60}\u{597d}\x1b[2GA")?;

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(
            cells[0].width, 1,
            "the left half of the overwritten wide cell is still two columns wide"
        );
        assert_eq!(cells[0].ch, ' ');
        assert_eq!(cells[1].ch, 'A');
        assert_eq!(cells[1].width, 1);
        assert_eq!(cells[2].ch, '\u{597d}');

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// `ESC[@` never overwrites a cell, it slides one -- and a slide that
    /// starts on the right half of a wide character passes straight between
    /// its two columns. zsh's line editor emits this every time you type into
    /// the middle of a line, so a row of Chinese only has to be edited once.
    #[test]
    fn screen_buffer_splits_a_wide_cell_when_chars_are_inserted_mid_cell() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        // Insert one blank at column 2 -- the right half of the first wide
        // character.
        set_output_for_test(session.id, "\u{4f60}\u{597d}\x1b[2G\x1b[1@")?;

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(
            cells[0].width, 1,
            "the left half still claims the column the insert took away from it"
        );
        assert_eq!(cells[0].ch, ' ');
        assert_eq!(cells[1].ch, ' ');
        assert_eq!(
            cells[2].width, 1,
            "the right half the insert pushed aside is still a continuation"
        );
        assert_eq!(cells[2].ch, ' ');
        assert_eq!(cells[3].ch, '\u{597d}');
        assert_eq!(cells[3].width, 2);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// The far end of an insert is just as dangerous: the cells shifted right
    /// run out of row, and the half that falls off the margin leaves the half
    /// still on screen painting two columns it no longer owns.
    #[test]
    fn screen_buffer_splits_a_wide_cell_pushed_past_the_right_edge_by_inserted_chars() -> Result<()>
    {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 6,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        // The row is exactly full: A B then two wide characters. One inserted
        // blank shoves the last continuation off the right edge.
        set_output_for_test(session.id, "AB\u{4f60}\u{597d}\x1b[1G\x1b[1@")?;

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(cells[1].ch, 'A');
        assert_eq!(cells[3].ch, '\u{4f60}');
        assert_eq!(cells[3].width, 2);
        assert_eq!(cells[4].width, 0);
        assert_eq!(
            cells[5].width, 1,
            "the last wide cell kept both columns after one of them fell off the row"
        );
        assert_eq!(cells[5].ch, ' ');

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// `ESC[P` is the same hazard running the other way. Deleting from the
    /// right half of a wide character drags its neighbour into the column the
    /// left half is still painting over.
    #[test]
    fn screen_buffer_splits_a_wide_cell_when_chars_are_deleted_mid_cell() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        // Delete one cell at column 2 -- the right half of the first wide
        // character.
        set_output_for_test(session.id, "\u{4f60}\u{597d}\x1b[2G\x1b[1P")?;

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(
            cells[0].width, 1,
            "the left half still claims the column the next character moved into"
        );
        assert_eq!(cells[0].ch, ' ');
        assert_eq!(cells[1].ch, '\u{597d}');
        assert_eq!(cells[1].width, 2);
        assert_eq!(cells[2].width, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// The other edge of a delete: the span ends between the halves of a wide
    /// character, so the continuation is dragged left on its own and lands at
    /// the cursor as a cell that owns no character at all.
    #[test]
    fn screen_buffer_splits_a_wide_cell_when_the_deleted_range_ends_mid_cell() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        // Delete one cell at column 1 -- half of the first wide character.
        set_output_for_test(session.id, "\u{4f60}\u{597d}\x1b[1G\x1b[1P")?;

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(
            cells[0].width, 1,
            "the orphaned right half is still a continuation of nothing"
        );
        assert_eq!(cells[0].ch, ' ');
        assert_eq!(cells[1].ch, '\u{597d}');
        assert_eq!(cells[1].width, 2);
        assert_eq!(cells[2].width, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// The repair only fires on pairs a shift actually cut. A shift that moves
    /// both halves together, or removes both of them together, has to leave
    /// the row exactly as wide as it found it.
    #[test]
    fn screen_buffer_keeps_wide_pairs_whole_when_a_shift_moves_them_together() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        // Row 1 inserts one blank ahead of both wide characters; row 2 deletes
        // one wide character whole.
        set_output_for_test(
            session.id,
            "\u{4f60}\u{597d}\x1b[1G\x1b[1@\x1b[2;1H\u{4f60}\u{597d}\x1b[2;1H\x1b[2P",
        )?;

        let styled = engine.read_styled_screen(session.id)?;
        let shifted = &styled.lines[0].cells;
        assert_eq!(shifted[0].ch, ' ');
        assert_eq!(shifted[1].ch, '\u{4f60}');
        assert_eq!(shifted[1].width, 2);
        assert_eq!(shifted[2].width, 0);
        assert_eq!(shifted[3].ch, '\u{597d}');
        assert_eq!(shifted[3].width, 2);
        assert_eq!(shifted[4].width, 0);

        let deleted = &styled.lines[1].cells;
        assert_eq!(deleted[0].ch, '\u{597d}');
        assert_eq!(deleted[0].width, 2);
        assert_eq!(deleted[1].width, 0);
        assert_eq!(deleted[2].ch, ' ');
        assert_eq!(deleted[2].width, 1);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// Insert mode (`ESC[4h`) slides the row for every single character it
    /// writes, so it is the same hazard as `ESC[@` arriving one keystroke at a
    /// time. Landing on the right half of a wide character splits it and
    /// leaves the left half painting over the character just typed.
    #[test]
    fn screen_buffer_splits_a_wide_cell_when_insert_mode_shifts_the_row() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        // Type into column 2 -- the right half of the first wide character.
        set_output_for_test(session.id, "\x1b[4h\u{4f60}\u{597d}\x1b[2GA")?;

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(
            cells[0].width, 1,
            "the left half still claims the column the typed character took"
        );
        assert_eq!(cells[0].ch, ' ');
        assert_eq!(cells[1].ch, 'A');
        assert_eq!(
            cells[2].width, 1,
            "the right half the insert pushed aside is still a continuation"
        );
        assert_eq!(cells[2].ch, ' ');
        assert_eq!(cells[3].ch, '\u{597d}');
        assert_eq!(cells[3].width, 2);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// The far seam of an insert-mode write is the truncation back to the
    /// column count: the row is already full, so one typed character shoves a
    /// continuation off the end and strands the half still on screen.
    #[test]
    fn screen_buffer_splits_a_wide_cell_pushed_off_the_row_by_insert_mode() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 6,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "\x1b[4hAB\u{4f60}\u{597d}\x1b[1GX")?;

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(cells[0].ch, 'X');
        assert_eq!(cells[1].ch, 'A');
        assert_eq!(cells[3].ch, '\u{4f60}');
        assert_eq!(cells[3].width, 2);
        assert_eq!(cells[4].width, 0);
        assert_eq!(
            cells[5].width, 1,
            "the last wide cell kept both columns after one of them fell off the row"
        );
        assert_eq!(cells[5].ch, ' ');

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// `ESC[ @` slides the row sideways too, and the cell it drags in over the
    /// left edge can be a continuation whose owner has just left the row --
    /// the one orphan with no earlier cell to look back at.
    #[test]
    fn screen_buffer_splits_a_wide_cell_when_the_row_scrolls_left() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        // Cursor parked on column 1 so the shift starts at the left margin
        // whichever way `SL` ends up resolving its left edge.
        set_output_for_test(session.id, "\u{4f60}\u{597d}\x1b[1G\x1b[1 @")?;

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(
            cells[0].width, 1,
            "the orphaned right half is still a continuation of nothing"
        );
        assert_eq!(cells[0].ch, ' ');
        assert_eq!(cells[1].ch, '\u{597d}');
        assert_eq!(cells[1].width, 2);
        assert_eq!(cells[2].width, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// `ESC[ A` is the mirror, and so is its damage: the row runs out of
    /// columns and the half that falls off the right margin leaves the other
    /// half claiming a column that is no longer there.
    #[test]
    fn screen_buffer_splits_a_wide_cell_pushed_past_the_right_edge_by_scrolling_right() -> Result<()>
    {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 6,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "AB\u{4f60}\u{597d}\x1b[1G\x1b[1 A")?;

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(cells[1].ch, 'A');
        assert_eq!(cells[3].ch, '\u{4f60}');
        assert_eq!(cells[3].width, 2);
        assert_eq!(cells[4].width, 0);
        assert_eq!(
            cells[5].width, 1,
            "the last wide cell kept both columns after one of them fell off the row"
        );
        assert_eq!(cells[5].ch, ' ');

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_preserves_combining_marks_on_base_cells() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 8,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "e\u{0301}X")?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["e\u{0301}X"]);
        assert_eq!(screen.cursor.x, 2);

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(cells[0].ch, 'e');
        assert_eq!(cells[0].width, 1);
        assert_eq!(cells[1].ch, 'X');
        assert_eq!(cells[1].width, 1);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_attaches_combining_marks_to_previous_wide_cell() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 8,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "你\u{0301}A")?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["你\u{0301}A"]);
        assert_eq!(screen.cursor.x, 3);

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(cells[0].ch, '你');
        assert_eq!(cells[0].width, 2);
        assert_eq!(cells[1].width, 0);
        assert_eq!(cells[2].ch, 'A');
        assert_eq!(cells[2].width, 1);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_preserves_emoji_variation_selector_width() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 8,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "☕\u{fe0f}A")?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["☕\u{fe0f}A"]);
        assert_eq!(screen.cursor.x, 3);

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(cells[0].ch, '☕');
        assert_eq!(cells[0].width, 2);
        assert_eq!(cells[1].width, 0);
        assert_eq!(cells[2].ch, 'A');
        assert_eq!(cells[2].width, 1);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_keeps_zwj_emoji_sequence_in_one_wide_cell() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 8,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "👨\u{200d}💻A")?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["👨\u{200d}💻A"]);
        assert_eq!(screen.cursor.x, 3);

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(cells[0].ch, '👨');
        assert_eq!(cells[0].width, 2);
        assert_eq!(cells[1].width, 0);
        assert_eq!(cells[2].ch, 'A');
        assert_eq!(cells[2].width, 1);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_keeps_emoji_modifier_in_base_wide_cell() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 8,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "👍\u{1f3fd}A")?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["👍\u{1f3fd}A"]);
        assert_eq!(screen.cursor.x, 3);

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(cells[0].ch, '👍');
        assert_eq!(cells[0].width, 2);
        assert_eq!(cells[1].width, 0);
        assert_eq!(cells[2].ch, 'A');
        assert_eq!(cells[2].width, 1);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_keeps_regional_indicator_flag_in_one_wide_cell() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 8,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "🇺🇸A")?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["🇺🇸A"]);
        assert_eq!(screen.cursor.x, 3);

        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(cells[0].ch, '🇺');
        assert_eq!(cells[0].width, 2);
        assert_eq!(cells[1].width, 0);
        assert_eq!(cells[2].ch, 'A');
        assert_eq!(cells[2].width, 1);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_repeats_previous_character_with_rep() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 12,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "\x1b[31mA\x1b[3b")?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["AAAA"]);
        assert_eq!(screen.cursor.x, 4);

        let styled = engine.read_styled_screen(session.id)?;
        assert_eq!(
            styled.lines[0].cells[0].style.fg,
            Some(StyledColor::Palette(1))
        );
        assert_eq!(
            styled.lines[0].cells[3].style.fg,
            Some(StyledColor::Palette(1))
        );

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_repeats_wide_character_with_rep() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 8,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "你\x1b[2b")?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["你你你"]);
        assert_eq!(screen.cursor.x, 6);

        let styled = engine.read_styled_screen(session.id)?;
        assert_eq!(styled.lines[0].cells[0].width, 2);
        assert_eq!(styled.lines[0].cells[1].width, 0);
        assert_eq!(styled.lines[0].cells[4].ch, '你');
        assert_eq!(styled.lines[0].cells[5].width, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn read_styled_scrollback_preserves_history_cell_attributes() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 2,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "one \x1b[31mred\x1b[0m\nplain\n")?;

        let styled = engine.read_styled_scrollback(
            session.id,
            ScrollbackTextRequest {
                start_line: Some(0),
                end_line: Some(1),
                tail_lines: None,
                escapes: false,
            },
        )?;

        assert_eq!(styled.first_row, 0);
        assert_eq!(styled.row_count, 1);
        assert_eq!(styled.lines[0].row, 0);
        assert_eq!(styled.lines[0].cells[0].ch, 'o');
        assert_eq!(
            styled.lines[0].cells[4].style.fg,
            Some(StyledColor::Palette(1))
        );

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_basic_csi_screen_operations() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(
            session.id,
            "old line\n\x1b[2J\x1b[Hhello\nworld\x1b[1A\x1b[3G!\x1b[K",
        )?;

        let lines = engine.read_screen(session.id)?.lines;
        assert!(!lines.iter().any(|line| line.contains("old line")));
        assert!(lines.iter().any(|line| line == "he!"));
        assert!(lines.iter().any(|line| line == "world"));

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_character_edit_and_erase_modes() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 6,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(
            session.id,
            concat!(
                "abcde",
                "\x1b[1;3H",
                "\x1b[2@",
                "XY",
                "\x1b[1;5H",
                "\x1b[2P",
                "\x1b[2;1Hkeep",
                "\x1b[3;1Hprefix-tail",
                "\x1b[3;7H",
                "\x1b[K",
                "\x1b[4;1Herase-left",
                "\x1b[4;6H",
                "\x1b[1K",
                "\x1b[5;1Herase-all",
                "\x1b[2K",
                "\x1b[6;1Herase-chars",
                "\x1b[6;6H",
                "\x1b[3X"
            ),
        )?;

        let lines = engine.read_screen(session.id)?.lines;
        assert_eq!(
            lines,
            vec!["abXYe", "keep", "prefix", "      left", "", "erase   ars"]
        );

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_delete_chars_backfills_cells_to_right_margin() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 6,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "abcdef\x1b[31m\x1b[1;3H\x1b[2P")?;

        assert_eq!(engine.read_screen(session.id)?.lines, vec!["abef"]);
        let styled = engine.read_styled_screen(session.id)?;
        let cells = &styled.lines[0].cells;
        assert_eq!(cells.len(), 6);
        assert_eq!(
            cells.iter().map(|cell| cell.ch).collect::<String>(),
            "abef  "
        );
        assert_eq!(cells[4].style.fg, Some(StyledColor::Palette(1)));
        assert_eq!(cells[5].style.fg, Some(StyledColor::Palette(1)));

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_insert_chars_preserves_cells_outside_right_margin() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "0123456789\x1b[?69h\x1b[3;8s\x1b[1;4H\x1b[2@")?;

        assert_eq!(engine.read_screen(session.id)?.lines, vec!["012  34589"]);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_delete_chars_preserves_cells_outside_right_margin() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "0123456789\x1b[?69h\x1b[3;8s\x1b[1;4H\x1b[2P")?;

        assert_eq!(engine.read_screen(session.id)?.lines, vec!["012567  89"]);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_horizontal_scroll() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "abcdefghij\x1b[1;3H\x1b[2 @\x1b[1;5H\x1b[3 A")?;
        let screen = engine.read_screen(session.id)?;

        // `SL 2` takes the whole row left two columns -- `cdefghij` plus two
        // blanks -- and `SR 3` then takes that three columns right, dropping
        // the `j` off the right margin. The cursor moves are irrelevant to
        // both; they are here only to prove that.
        assert_eq!(screen.lines, vec!["   cdefghi"]);
        assert_eq!(screen.cursor.x, 4);
        assert_eq!(screen.cursor.y, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_limits_horizontal_scroll_to_margins() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            "0123456789\x1b[?69h\x1b[3;8s\x1b[1;4H\x1b[2 @\x1b[1;5H\x1b[1 A",
        )?;
        let screen = engine.read_screen(session.id)?;

        // Margins are columns 3-8, so the shifts run over `234567` and leave
        // `01` and `89` alone. `SL 2` makes that `4567  `, `SR 1` makes it
        // ` 4567 `.
        assert_eq!(screen.lines, vec!["01 4567 89"]);
        assert_eq!(screen.cursor.x, 4);
        assert_eq!(screen.cursor.y, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// `SL` moves the whole scroll region, not the cursor's row. A boxed frame
    /// is the honest test: an implementation that shifts one row tears the box
    /// open, and no amount of squinting at a single line would show it.
    #[test]
    fn screen_buffer_scrolls_left_across_every_row_of_the_region() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 8,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        // The cursor is parked mid-row on the middle line; `SL` must ignore it
        // entirely and start every row at the left margin.
        set_output_for_test(
            session.id,
            "+------+\r\n|abcdef|\r\n+------+\x1b[2;5H\x1b[1 @",
        )?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["------+", "abcdef|", "------+"]);
        assert_eq!(screen.cursor.x, 4, "SL does not move the cursor");
        assert_eq!(screen.cursor.y, 1);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// The mirror: `SR` moves every row of the region too, and the column that
    /// falls off the right margin is gone from all of them.
    #[test]
    fn screen_buffer_scrolls_right_across_every_row_of_the_region() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 8,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(
            session.id,
            "+------+\r\n|abcdef|\r\n+------+\x1b[3;2H\x1b[1 A",
        )?;

        let screen = engine.read_screen(session.id)?;
        // Every row loses exactly its last column: `+------+` keeps six
        // dashes and one `+`, `|abcdef|` keeps the letters and its opening
        // bar.
        assert_eq!(screen.lines, vec![" +------", " |abcdef", " +------"]);
        assert_eq!(screen.cursor.x, 1, "SR does not move the cursor");
        assert_eq!(screen.cursor.y, 2);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    /// The region is the vertical one as well. `DECSTBM` fences off rows 2-3,
    /// and `SL` has to leave the rows outside it alone -- including row 1,
    /// which is where setting the region just put the cursor.
    #[test]
    fn screen_buffer_limits_horizontal_scroll_to_the_scroll_region() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 6,
            rows: 4,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "aaa\r\nbbb\r\nccc\r\nddd\x1b[2;3r\x1b[1 @")?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["aaa", "bb", "cc", "ddd"]);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_erase_line_modes_backfill_styled_cells() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 6,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(
            session.id,
            concat!(
                "abcdef",
                "\x1b[31m\x1b[1;4H\x1b[K",
                "\x1b[2;1Hghijkl",
                "\x1b[32m\x1b[2;4H\x1b[1K",
                "\x1b[3;1Hmnopqr",
                "\x1b[34m\x1b[3;3H\x1b[2K"
            ),
        )?;

        assert_eq!(
            engine.read_screen(session.id)?.lines,
            vec!["abc", "    kl", ""]
        );
        let styled = engine.read_styled_screen(session.id)?;

        let first = &styled.lines[0].cells;
        assert_eq!(
            first.iter().map(|cell| cell.ch).collect::<String>(),
            "abc   "
        );
        assert_eq!(first[3].style.fg, Some(StyledColor::Palette(1)));
        assert_eq!(first[5].style.fg, Some(StyledColor::Palette(1)));

        let second = &styled.lines[1].cells;
        assert_eq!(
            second.iter().map(|cell| cell.ch).collect::<String>(),
            "    kl"
        );
        assert_eq!(second[0].style.fg, Some(StyledColor::Palette(2)));
        assert_eq!(second[3].style.fg, Some(StyledColor::Palette(2)));

        let third = &styled.lines[2].cells;
        assert_eq!(
            third.iter().map(|cell| cell.ch).collect::<String>(),
            "      "
        );
        assert!(third
            .iter()
            .all(|cell| cell.style.fg == Some(StyledColor::Palette(4))));

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_escape_index_sequences() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 8,
            rows: 4,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "A\x1bDB\x1bEC")?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["A", " B", "C"]);
        assert_eq!(screen.cursor.x, 1);
        assert_eq!(screen.cursor.y, 2);

        set_output_for_test(
            session.id,
            "\x1b[2J\x1b[Hr0\nr1\nr2\nr3\x1b[2;4r\x1b[2;1H\x1bM",
        )?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["r0", "", "r1", "r2"]);
        assert_eq!(screen.cursor.x, 0);
        assert_eq!(screen.cursor.y, 1);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_index_sequences_do_not_scroll_when_cursor_outside_region() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 8,
            rows: 5,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            "top\r\none\r\ntwo\r\nthree\r\nbottom\x1b[2;4r\x1b[5;1H\x1bD!",
        )?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["top", "one", "two", "three", "!ottom"]);
        assert_eq!(screen.cursor.x, 1);
        assert_eq!(screen.cursor.y, 4);

        set_output_for_test(
            session.id,
            "\x1b[2J\x1b[Htop\r\none\r\ntwo\r\nthree\r\nbottom\x1b[2;4r\x1b[1;1H\x1bM!",
        )?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["!op", "one", "two", "three", "bottom"]);
        assert_eq!(screen.cursor.x, 1);
        assert_eq!(screen.cursor.y, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_ignores_charset_and_utf8_designators() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 16,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "ab\x1b(Bcd\x1b)0ef\x1b%Ggh")?;
        assert_eq!(engine.read_screen(session.id)?.lines, vec!["abcdefgh"]);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_ignores_non_osc_string_controls() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 24,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            "ab\x1bP1;payload\x1b\\cd\x1b_should-not-print\x07ef\x1b^hidden\x1b\\gh\x1bXmore\x07ij",
        )?;
        assert_eq!(engine.read_screen(session.id)?.lines, vec!["abcdefghij"]);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn terminal_parser_preserves_state_across_split_chunks() {
        let mut screen = NextCoreScreen::new(24, 3);

        screen.feed("ab\x1b[31");
        screen.feed("mR\x1b[0");
        screen.feed("m\x1b]2;Split");
        screen.feed(" title\x07Z");

        assert_eq!(screen.snapshot_viewport_lines(), vec!["abRZ"]);
        assert_eq!(screen.title.as_deref(), Some("Split title"));
        assert_eq!(screen.lines[0][2].attr.fg, Some(TerminalColor::Palette(1)));
        assert_eq!(screen.lines[0][3].attr, CellAttributes::default());
    }

    #[test]
    fn terminal_parser_ignores_split_string_controls() {
        let mut screen = NextCoreScreen::new(24, 3);

        screen.feed("A\x1bPpayload");
        screen.feed("\x1b\\B\x1b_more");
        screen.feed("\x07C");

        assert_eq!(screen.snapshot_viewport_lines(), vec!["ABC"]);
    }

    #[test]
    fn screen_buffer_handles_c1_control_forms() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 16,
            rows: 4,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            "ab\u{009b}2GZ\u{009d}0;C1 Title\u{009c}\u{0090}hidden\u{009c}\u{0085}next\u{0084}ind\u{008d}ri",
        )?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["aZ", "next   ri", "    ind"]);
        assert_eq!(engine.get_session(session.id)?.title, "C1 Title");

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_decaln_alignment_test() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 5,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "old\x1b#8")?;
        assert_eq!(
            engine.read_screen(session.id)?.lines,
            vec!["EEEEE", "EEEEE", "EEEEE"]
        );

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_decfra_rectangular_fill() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 4,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            "0123456789\r\nabcdefghij\r\nABCDEFGHIJ\x1b[1;6H\x1b[31m\x1b[88;2;3;3;8$x",
        )?;
        let screen = engine.read_screen(session.id)?;
        let styled = engine.read_styled_screen(session.id)?;

        assert_eq!(screen.lines, vec!["0123456789", "abXXXXXXij", "ABXXXXXXIJ"]);
        assert_eq!(screen.cursor.x, 5);
        assert_eq!(screen.cursor.y, 0);
        assert_eq!(
            styled.lines[1].cells[2].style.fg,
            Some(StyledColor::Palette(1))
        );
        assert_eq!(
            styled.lines[2].cells[7].style.fg,
            Some(StyledColor::Palette(1))
        );
        assert_eq!(styled.lines[1].cells[1].style.fg, None);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_decfra_clips_to_viewport_and_defaults_to_space() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 6,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "abcdef\r\nABCDEF\x1b[20320;2;5;9;9$x")?;
        let screen = engine.read_screen(session.id)?;
        let styled = engine.read_styled_screen(session.id)?;

        assert_eq!(screen.lines, vec!["abcdef", "ABCD", ""]);
        assert_eq!(
            styled.lines[1]
                .cells
                .iter()
                .map(|cell| cell.ch)
                .collect::<String>(),
            "ABCD  "
        );
        assert_eq!(
            styled.lines[2]
                .cells
                .iter()
                .map(|cell| cell.ch)
                .collect::<String>(),
            "      "
        );

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_decera_rectangular_erase() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 4,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            "0123456789\r\nabcdefghij\r\nABCDEFGHIJ\x1b[1;6H\x1b[32m\x1b[2;3;3;8$z",
        )?;
        let screen = engine.read_screen(session.id)?;
        let styled = engine.read_styled_screen(session.id)?;

        assert_eq!(screen.lines, vec!["0123456789", "ab      ij", "AB      IJ"]);
        assert_eq!(screen.cursor.x, 5);
        assert_eq!(screen.cursor.y, 0);
        assert_eq!(
            styled.lines[1].cells[2].style.fg,
            Some(StyledColor::Palette(2))
        );
        assert_eq!(
            styled.lines[2].cells[7].style.fg,
            Some(StyledColor::Palette(2))
        );
        assert_eq!(styled.lines[1].cells[1].style.fg, None);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_decera_clips_and_defaults_to_full_viewport() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 6,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "abcdef\r\nABCDEF\x1b[2;5;9;9$z")?;
        let screen = engine.read_screen(session.id)?;
        let styled = engine.read_styled_screen(session.id)?;

        assert_eq!(screen.lines, vec!["abcdef", "ABCD", ""]);
        assert_eq!(
            styled.lines[1]
                .cells
                .iter()
                .map(|cell| cell.ch)
                .collect::<String>(),
            "ABCD  "
        );
        assert_eq!(
            styled.lines[2]
                .cells
                .iter()
                .map(|cell| cell.ch)
                .collect::<String>(),
            "      "
        );

        set_output_for_test(session.id, "abcdef\r\nABCDEF\x1b[$z")?;
        let styled = engine.read_styled_screen(session.id)?;
        assert!(styled
            .lines
            .iter()
            .all(|line| line.cells.iter().all(|cell| cell.ch == ' ')));

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_decsera_selective_rectangular_erase() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 4,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            concat!(
                "0123456789\r\n",
                "ab",
                "\x1b[1\"q",
                "PROT",
                "\x1b[0\"q",
                "ghij\r\n",
                "ABCDEFGHIJ",
                "\x1b[1;6H",
                "\x1b[2;1;3;10${"
            ),
        )?;
        let screen = engine.read_screen(session.id)?;

        assert_eq!(screen.lines, vec!["0123456789", "  PROT", ""]);
        assert_eq!(screen.cursor.x, 5);
        assert_eq!(screen.cursor.y, 0);

        set_output_for_test(
            session.id,
            concat!(
                "0123456789\r\n",
                "ab",
                "\x1b[1\"q",
                "PROT",
                "\x1b[0\"q",
                "ghij",
                "\x1b[2;1;2;10$z"
            ),
        )?;
        assert_eq!(
            engine.read_screen(session.id)?.lines,
            vec!["0123456789", ""]
        );

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_decsel_selective_line_erase() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            concat!(
                "ab",
                "\x1b[1\"q",
                "PROT",
                "\x1b[0\"q",
                "ghij",
                "\x1b[1;1H",
                "\x1b[?0K"
            ),
        )?;
        assert_eq!(engine.read_screen(session.id)?.lines, vec!["  PROT"]);

        set_output_for_test(
            session.id,
            concat!(
                "ab",
                "\x1b[1\"q",
                "PROT",
                "\x1b[0\"q",
                "ghij",
                "\x1b[1;1H",
                "\x1b[0K"
            ),
        )?;
        assert_eq!(engine.read_screen(session.id)?.lines, vec![""]);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_decsed_selective_display_erase() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 4,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            concat!(
                "0123456789\r\n",
                "ab",
                "\x1b[1\"q",
                "PROT",
                "\x1b[0\"q",
                "ghij\r\n",
                "ABCDEFGHIJ",
                "\x1b[2;1H",
                "\x1b[?0J"
            ),
        )?;
        assert_eq!(
            engine.read_screen(session.id)?.lines,
            vec!["0123456789", "  PROT", ""]
        );

        set_output_for_test(
            session.id,
            concat!(
                "0123456789\r\n",
                "ab",
                "\x1b[1\"q",
                "PROT",
                "\x1b[0\"q",
                "ghij\r\n",
                "ABCDEFGHIJ",
                "\x1b[2;1H",
                "\x1b[0J"
            ),
        )?;
        assert_eq!(
            engine.read_screen(session.id)?.lines,
            vec!["0123456789", "", ""]
        );

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_decsca_mode_two_returns_to_erasable_cells() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 8,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "\x1b[1\"qAB\x1b[2\"qCD\x1b[1;1;1;4${")?;

        assert_eq!(engine.read_screen(session.id)?.lines, vec!["AB"]);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_deccara_rectangular_attributes() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 4,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            "0123456789\r\nabcdefghij\r\nABCDEFGHIJ\x1b[1;6H\x1b[2;3;3;8;1;4;7;8$r",
        )?;
        let screen = engine.read_screen(session.id)?;
        let styled = engine.read_styled_screen(session.id)?;

        assert_eq!(screen.lines, vec!["0123456789", "abcdefghij", "ABCDEFGHIJ"]);
        assert_eq!(screen.cursor.x, 5);
        assert_eq!(screen.cursor.y, 0);
        assert!(styled.lines[1].cells[2].style.bold);
        assert!(styled.lines[1].cells[2].style.underline);
        assert!(styled.lines[1].cells[2].style.inverse);
        assert!(styled.lines[1].cells[2].style.hidden);
        assert!(styled.lines[2].cells[7].style.bold);
        assert!(styled.lines[2].cells[7].style.hidden);
        assert!(!styled.lines[1].cells[1].style.bold);
        assert!(!styled.lines[2].cells[8].style.inverse);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_deccara_resets_rectangular_attributes() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 8,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            "\x1b[1;4;5;7;8mabcdefgh\r\nABCDEFGH\x1b[2;3;2;6;0;28$r",
        )?;
        let styled = engine.read_styled_screen(session.id)?;

        let preserved = &styled.lines[0].cells[2].style;
        assert!(preserved.bold);
        assert!(preserved.underline);
        assert!(preserved.inverse);
        assert!(preserved.hidden);

        let reset = &styled.lines[1].cells[2].style;
        assert!(!reset.bold);
        assert!(!reset.underline);
        assert!(!reset.inverse);
        assert!(!reset.hidden);

        let outside = &styled.lines[1].cells[6].style;
        assert!(outside.bold);
        assert!(outside.underline);
        assert!(outside.inverse);
        assert!(outside.hidden);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_decrara_rectangular_attribute_reverse() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 4,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            "0123456789\r\nabcdefghij\r\nABCDEFGHIJ\x1b[1;6H\x1b[2;3;3;8;1;4;7;8$t",
        )?;
        let screen = engine.read_screen(session.id)?;
        let styled = engine.read_styled_screen(session.id)?;

        assert_eq!(screen.lines, vec!["0123456789", "abcdefghij", "ABCDEFGHIJ"]);
        assert_eq!(screen.cursor.x, 5);
        assert_eq!(screen.cursor.y, 0);
        assert!(styled.lines[1].cells[2].style.bold);
        assert!(styled.lines[1].cells[2].style.underline);
        assert!(styled.lines[1].cells[2].style.inverse);
        assert!(styled.lines[1].cells[2].style.hidden);
        assert!(styled.lines[2].cells[7].style.bold);
        assert!(styled.lines[2].cells[7].style.hidden);
        assert!(!styled.lines[1].cells[1].style.bold);
        assert!(!styled.lines[2].cells[8].style.inverse);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_decrara_toggles_existing_rectangular_attributes() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 8,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            "\x1b[1;4;5;7;8mabcdefgh\r\nABCDEFGH\x1b[2;3;2;6;0;8$t",
        )?;
        let styled = engine.read_styled_screen(session.id)?;

        let preserved = &styled.lines[0].cells[2].style;
        assert!(preserved.bold);
        assert!(preserved.underline);
        assert!(preserved.inverse);
        assert!(preserved.hidden);

        let toggled = &styled.lines[1].cells[2].style;
        assert!(!toggled.bold);
        assert!(!toggled.underline);
        assert!(!toggled.inverse);
        assert!(toggled.blink.is_none());
        assert!(!toggled.hidden);

        let outside = &styled.lines[1].cells[6].style;
        assert!(outside.bold);
        assert!(outside.underline);
        assert!(outside.inverse);
        assert!(outside.hidden);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_csi_save_and_restore_cursor() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 12,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "one\x1b[sXX\x1b[uY")?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["oneYX"]);
        assert_eq!(screen.cursor.x, 4);
        assert_eq!(screen.cursor.y, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_save_and_restore_cursor_preserves_sgr_attributes() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 12,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "\x1b[31mA\x1b7\x1b[0mB\x1b8C")?;
        let screen = engine.read_screen(session.id)?;
        let styled = engine.read_styled_screen(session.id)?;

        assert_eq!(screen.lines, vec!["AC"]);
        assert_eq!(
            styled.lines[0].cells[0].style.fg,
            Some(StyledColor::Palette(1))
        );
        assert_eq!(
            styled.lines[0].cells[1].style.fg,
            Some(StyledColor::Palette(1))
        );

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_save_and_restore_cursor_preserves_hyperlinks() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 12,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            "\x1b]8;;https://example.test\x07A\x1b7\x1b]8;;\x07B\x1b8C",
        )?;
        let styled = engine.read_styled_screen(session.id)?;

        assert_eq!(engine.read_screen(session.id)?.lines, vec!["AC"]);
        assert_eq!(
            styled.lines[0].cells[0].style.hyperlink.as_deref(),
            Some("https://example.test")
        );
        assert_eq!(
            styled.lines[0].cells[1].style.hyperlink.as_deref(),
            Some("https://example.test")
        );

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_dec_private_cursor_save_and_restore() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 8,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "ab\x1b[?1048h\x1b[3;5HZ\x1b[?1048lY")?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["abY", "", "    Z"]);
        assert_eq!(screen.cursor.x, 3);
        assert_eq!(screen.cursor.y, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_extended_cursor_positioning() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 12,
            rows: 5,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            concat!("A", "\x1b[2EB", "\x1b[1FC", "\x1b[6`D", "\x1b[2aE", "\x1b[4dF", "\x1b[1eG"),
        )?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(
            screen.lines,
            vec!["A", "C    D  E", "B", "         F", "          G"]
        );
        assert_eq!(screen.cursor.x, 11);
        assert_eq!(screen.cursor.y, 4);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_reverse_tab_positioning() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 16,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "\x1b[5G\x1bH\x1b[13GX\x1b[ZY\x1b[2ZZ")?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["    Z   Y   X"]);
        assert_eq!(screen.cursor.x, 5);
        assert_eq!(screen.cursor.y, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_display_erase_modes() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;

        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "one\nprefix-tail\nthree\x1b[2;7H\x1b[J")?;
        assert_eq!(
            engine.read_screen(session.id)?.lines,
            vec!["one", "prefix", ""]
        );
        engine.destroy_session(session.id)?;

        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "one\ntwo-three\nthree\x1b[2;4H\x1b[1J")?;
        assert_eq!(
            engine.read_screen(session.id)?.lines,
            vec!["", "    three", "three"]
        );
        engine.destroy_session(session.id)?;

        Ok(())
    }

    #[test]
    fn screen_buffer_display_erase_mode_2_preserves_cursor_position() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 12,
            rows: 4,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "one\ntwo\nthree\x1b[3;5H\x1b[2JZ")?;
        let screen = engine.read_screen(session.id)?;

        assert_eq!(screen.lines, vec!["", "", "    Z"]);
        assert_eq!(screen.cursor.x, 5);
        assert_eq!(screen.cursor.y, 2);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_clears_scrollback_with_display_erase_mode_3() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "one\ntwo\nthree\nfour\nfive")?;
        assert_eq!(engine.read_screen(session.id)?.scrollback_rows, 2);
        assert_eq!(engine.read_scrollback(session.id, 10)?, vec!["one", "two"]);
        engine.destroy_session(session.id)?;

        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "one\ntwo\nthree\nfour\nfive\x1b[3J")?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["three", "four", "five"]);
        assert_eq!(screen.scrollback_rows, 0);
        assert!(engine.read_scrollback(session.id, 10)?.is_empty());

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_reports_cursor_state() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(
            session.id,
            "abc\nxx\x1b[1A\x1b[3G!\x1b7\x1b[3;4Hq\x1b8z\x1b[?25l",
        )?;

        let screen = engine.read_screen(session.id)?;
        assert!(screen.lines.iter().any(|line| line == "ab!z"));
        assert_eq!(screen.cursor.x, 4);
        assert_eq!(screen.cursor.y, 0);
        assert!(!screen.cursor.visible);

        let cursor = engine.cursor(session.id)?;
        assert_eq!(cursor.x, screen.cursor.x);
        assert_eq!(cursor.y, screen.cursor.y);
        assert_eq!(cursor.visible, screen.cursor.visible);

        let listed = engine.list_sessions()?.remove(0);
        assert_eq!(listed.cursor.x, screen.cursor.x);
        assert_eq!(listed.cursor.y, screen.cursor.y);
        assert_eq!(listed.cursor.visible, screen.cursor.visible);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_tracks_cursor_shape() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "abc\x1b[5 q")?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.cursor.shape, "BlinkingBar");
        assert_eq!(engine.cursor(session.id)?.shape, "BlinkingBar");
        assert_eq!(
            engine.list_sessions()?.remove(0).cursor.shape,
            "BlinkingBar"
        );

        set_output_for_test(
            session.id,
            concat!("main\x1b[4 q", "\x1b[?1049h", "\x1b[2 q", "\x1b[?1049l"),
        )?;

        assert_eq!(engine.cursor(session.id)?.shape, "SteadyUnderline");

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_tracks_bracketed_paste_mode() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        assert!(!session_queries::bracketed_paste_enabled(session.id)?);
        set_output_for_test(session.id, "\x1b[?2004h")?;
        assert!(session_queries::bracketed_paste_enabled(session.id)?);
        set_output_for_test(session.id, "\x1b[?2004h\x1b[?2004l")?;
        assert!(!session_queries::bracketed_paste_enabled(session.id)?);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_tracks_cursor_blink_mode() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let mut screen = NextCoreScreen::new(80, 3);

        assert!(screen.cursor_blinking);
        screen.feed("\x1b[?12l");
        assert!(!screen.cursor_blinking);
        screen.feed("\x1b[?12h");
        assert!(screen.cursor_blinking);
        screen.feed("\x1b[?12l\x1b[!p");
        assert!(screen.cursor_blinking);

        Ok(())
    }

    #[test]
    fn screen_buffer_applies_column_mode_switching() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 4,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "main\x1b[2;3r\x1b[3;5Hbefore\x1b[?3hwide")?;
        let wide = engine.read_screen(session.id)?;
        assert_eq!(wide.cols, 132);
        assert_eq!(wide.cursor.x, 4);
        assert_eq!(wide.cursor.y, 0);
        assert_eq!(wide.lines, vec!["wide"]);

        set_output_for_test(session.id, "\x1b[?3lnarrow")?;
        let narrow = engine.read_screen(session.id)?;
        assert_eq!(narrow.cols, 80);
        assert_eq!(narrow.cursor.x, 6);
        assert_eq!(narrow.cursor.y, 0);
        assert_eq!(narrow.lines, vec!["narrow"]);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_honors_left_right_margins() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 4,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            "\x1b[?69h\x1b[3;6s\x1b[1;6HAB\x1b[?69l\x1b[1;1HC",
        )?;
        let screen = engine.read_screen(session.id)?;

        assert_eq!(screen.lines, vec!["C    A", "  B"]);
        assert_eq!(screen.cursor.x, 1);
        assert_eq!(screen.cursor.y, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_tracks_application_cursor_key_mode() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let mut screen = NextCoreScreen::new(80, 3);

        assert!(!screen.application_cursor_keys);
        screen.feed("\x1b[?1h");
        assert!(screen.application_cursor_keys);
        screen.feed("\x1b[?1l");
        assert!(!screen.application_cursor_keys);
        screen.feed("\x1b[?1h\x1b[!p");
        assert!(!screen.application_cursor_keys);

        Ok(())
    }

    #[test]
    fn screen_buffer_tracks_application_keypad_mode() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let mut screen = NextCoreScreen::new(80, 3);

        assert!(!screen.application_keypad);

        screen.feed("\x1b=");
        assert!(screen.application_keypad);
        screen.feed("\x1b>");
        assert!(!screen.application_keypad);

        screen.feed("\x1b[?66h");
        assert!(screen.application_keypad);
        screen.feed("\x1b[?66l");
        assert!(!screen.application_keypad);

        screen.feed("\x1b=\x1b[!p");
        assert!(!screen.application_keypad);

        Ok(())
    }

    #[test]
    fn screen_buffer_tracks_focus_event_reporting_mode() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let mut screen = NextCoreScreen::new(80, 3);

        assert!(!screen.focus_event_reporting);
        screen.feed("\x1b[?1004h");
        assert!(screen.focus_event_reporting);
        screen.feed("\x1b[?1004l");
        assert!(!screen.focus_event_reporting);
        screen.feed("\x1b[?1004h\x1b[!p");
        assert!(!screen.focus_event_reporting);

        Ok(())
    }

    #[test]
    fn screen_buffer_tracks_synchronized_output_mode() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let mut screen = NextCoreScreen::new(80, 3);

        assert!(!screen.synchronized_output);
        screen.feed("\x1b[?2026h");
        assert!(screen.synchronized_output);
        screen.feed("\x1b[?2026l");
        assert!(!screen.synchronized_output);
        screen.feed("\x1b[?2026h\x1b[!p");
        assert!(!screen.synchronized_output);

        Ok(())
    }

    #[test]
    fn screen_buffer_tracks_meta_sends_escape_mode() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let mut screen = NextCoreScreen::new(80, 3);

        assert!(!screen.meta_sends_escape);
        screen.feed("\x1b[?1034h");
        assert!(screen.meta_sends_escape);
        screen.feed("\x1b[?1034l");
        assert!(!screen.meta_sends_escape);
        screen.feed("\x1b[?1034h\x1b[!p");
        assert!(!screen.meta_sends_escape);

        Ok(())
    }

    #[test]
    fn screen_buffer_tracks_mouse_reporting_modes() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let mut screen = NextCoreScreen::new(80, 3);

        assert_eq!(screen.mouse_tracking, MouseTrackingMode::None);
        assert!(!screen.utf8_mouse);
        assert!(!screen.urxvt_mouse);
        assert!(!screen.sgr_mouse);
        assert!(!screen.alternate_scroll);
        assert!(!screen.sgr_pixel_mouse);

        screen.feed("\x1b[?1000h");
        assert_eq!(screen.mouse_tracking, MouseTrackingMode::ButtonEvent);
        screen.feed("\x1b[?1002h");
        assert_eq!(screen.mouse_tracking, MouseTrackingMode::ButtonMotion);
        screen.feed("\x1b[?1003h\x1b[?1005h\x1b[?1006h\x1b[?1007h\x1b[?1015h\x1b[?1016h");
        assert_eq!(screen.mouse_tracking, MouseTrackingMode::AnyEvent);
        assert!(screen.utf8_mouse);
        assert!(screen.sgr_mouse);
        assert!(screen.alternate_scroll);
        assert!(screen.urxvt_mouse);
        assert!(screen.sgr_pixel_mouse);

        screen.feed("\x1b[?1002l");
        assert_eq!(screen.mouse_tracking, MouseTrackingMode::AnyEvent);
        screen.feed("\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1007l\x1b[?1015l\x1b[?1016l");
        assert_eq!(screen.mouse_tracking, MouseTrackingMode::None);
        assert!(!screen.utf8_mouse);
        assert!(!screen.sgr_mouse);
        assert!(!screen.alternate_scroll);
        assert!(!screen.urxvt_mouse);
        assert!(!screen.sgr_pixel_mouse);

        screen.feed("\x1b[?1000h\x1b[?1005h\x1b[?1006h\x1b[?1007h\x1b[?1015h\x1b[?1016h\x1b[!p");
        assert_eq!(screen.mouse_tracking, MouseTrackingMode::None);
        assert!(!screen.utf8_mouse);
        assert!(!screen.sgr_mouse);
        assert!(!screen.alternate_scroll);
        assert!(!screen.urxvt_mouse);
        assert!(!screen.sgr_pixel_mouse);

        Ok(())
    }

    #[test]
    fn screen_buffer_applies_insert_mode() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 8,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "abcd\x1b[1;3H\x1b[4hXY\x1b[4lZ")?;
        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["abXYZd"]);
        assert_eq!(screen.cursor.x, 5);
        assert_eq!(screen.cursor.y, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_combined_modes() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "\x1b[?1049;2004halt")?;
        assert_eq!(engine.read_screen(session.id)?.lines, vec!["alt"]);
        assert!(session_queries::bracketed_paste_enabled(session.id)?);

        set_output_for_test(session.id, "\x1b[?25;2004lmain")?;
        let screen = engine.read_screen(session.id)?;
        assert!(!screen.cursor.visible);
        assert!(!session_queries::bracketed_paste_enabled(session.id)?);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_ris_terminal_reset() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        let (cwd_osc, expected_cwd) = if cfg!(windows) {
            (
                "\x1b]7;file://localhost/D:/code/unterm\x07",
                "D:\\code\\unterm",
            )
        } else {
            ("\x1b]7;file://localhost/tmp/unterm\x07", "/tmp/unterm")
        };
        set_output_for_test(
            session.id,
            &format!(
                "{cwd_osc}\x1b]0;Reset Test\x07main\x1b[?1049halt\x1b[31m\x1b[?7l\x1b[?6h\x1b[?2004h\x1b[3g\x1b[2;3r\x1b[?25l\x1bcZ\tT"
            ),
        )?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["Z       T"]);
        assert_eq!(screen.cursor.x, 9);
        assert_eq!(screen.cursor.y, 0);
        assert!(screen.cursor.visible);
        assert_eq!(screen.cursor.shape, "Default");
        assert_eq!(screen.scrollback_rows, 0);
        assert!(!session_queries::bracketed_paste_enabled(session.id)?);
        assert_eq!(engine.get_session(session.id)?.title, "Reset Test");
        assert_eq!(engine.shell(session.id)?.cwd.as_deref(), Some(expected_cwd));

        let styled = engine.read_styled_screen(session.id)?;
        assert_eq!(styled.lines[0].cells[0].style, CellStyle::default());
        assert_eq!(styled.lines[0].cells[8].style, CellStyle::default());

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_decstr_soft_reset() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(
            session.id,
            concat!(
                "\x1b]0;Soft Reset\x07",
                "\x1b[31mA",
                "\x1b[?7l",
                "\x1b[4h",
                "\x1b[3g",
                "\x1b[?25l",
                "\x1b[!p",
                "B\tC"
            ),
        )?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["AB      C"]);
        assert!(screen.cursor.visible);
        assert_eq!(screen.cursor.shape, "Default");
        assert_eq!(engine.get_session(session.id)?.title, "Soft Reset");

        let styled = engine.read_styled_screen(session.id)?;
        assert_eq!(
            styled.lines[0].cells[0].style.fg,
            Some(StyledColor::Palette(1))
        );
        assert_eq!(styled.lines[0].cells[1].style, CellStyle::default());
        assert_eq!(styled.lines[0].cells[8].style, CellStyle::default());

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_decstr_keeps_current_alternate_screen_active() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let mut screen = NextCoreScreen::new(20, 3);

        screen.feed(concat!("main", "\x1b[?1049h", "alt", "\x1b[!p", "stay"));
        assert_eq!(screen.snapshot_viewport_lines(), vec!["altstay"]);
        assert!(screen.alternate_screen_modes.contains(&1049));

        screen.feed("\x1b[?1049lback");
        assert_eq!(screen.snapshot_viewport_lines(), vec!["mainback"]);
        assert!(screen.alternate_screen_modes.is_empty());

        Ok(())
    }

    #[test]
    fn screen_buffer_handles_alternate_screen_and_line_mutations() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 24,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(
            session.id,
            concat!(
                "main-one\nmain-two",
                "\x1b[?1049h",
                "alt-only\n",
                "\x1b[?1049l",
                "\x1b[H",
                "\x1b[1L",
                "inserted",
                "\x1b[3;1H",
                "\x1b[1M",
                "\x1b7",
                "\x1b[2;6H!",
                "\x1b8",
                "."
            ),
        )?;

        let lines = engine.read_screen(session.id)?.lines;
        assert!(lines.iter().any(|line| line == "inserted"));
        assert!(lines.iter().any(|line| line == "main-!ne"));
        assert!(lines.iter().any(|line| line == "."));
        assert!(!lines.iter().any(|line| line.contains("alt-only")));
        assert!(!lines.iter().any(|line| line.contains("main-two")));

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_scrolls_when_output_exceeds_viewport() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "one\ntwo\nthree\nfour\nfive")?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.rows, 3);
        assert_eq!(screen.scrollback_rows, 2);
        assert_eq!(screen.lines, vec!["three", "four", "five"]);
        assert!(!screen.lines.iter().any(|line| line == "one"));
        assert!(!screen.lines.iter().any(|line| line == "two"));

        assert_eq!(engine.read_scrollback(session.id, 10)?, vec!["one", "two"]);
        let lines = engine.read_lines(session.id, 0, 5)?;
        let text = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(text, vec!["one", "two", "three", "four", "five"]);
        let middle = engine.read_lines(session.id, 1, 3)?;
        assert_eq!(
            middle
                .iter()
                .map(|line| (line.row, line.text.as_str()))
                .collect::<Vec<_>>(),
            vec![(1, "two"), (2, "three"), (3, "four")]
        );
        let scrollback_text = engine.read_scrollback_text(
            session.id,
            ScrollbackTextRequest {
                start_line: Some(1),
                end_line: Some(4),
                tail_lines: None,
                escapes: false,
            },
        )?;
        assert_eq!(scrollback_text.lines, vec!["two", "three", "four"]);
        assert_eq!(scrollback_text.first_row, 1);
        assert_eq!(scrollback_text.row_count, 3);
        assert_eq!(
            engine.search(session.id, "one", crate::SearchMode::CaseSensitive, 1)?[0].row,
            0
        );

        engine.scroll_viewport_to(session.id, 1)?;
        let scrolled = engine.read_screen(session.id)?;
        assert_eq!(scrolled.lines, vec!["two", "three", "four"]);
        assert_eq!(
            scrolled
                .cells
                .iter()
                .map(|line| (line.row, line.text.as_str()))
                .collect::<Vec<_>>(),
            vec![(1, "two"), (2, "three"), (3, "four")]
        );
        assert!(scrolled.revision > screen.revision);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_keeps_viewport_stable_when_scrollback_is_trimmed() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let mut screen = NextCoreScreen::new(80, 3);

        let initial = (0..10_010)
            .map(|idx| format!("line-{idx:05}"))
            .collect::<Vec<_>>()
            .join("\n");
        screen.feed(&initial);
        assert_eq!(screen.scrollback_rows(), DEFAULT_SCROLLBACK_LINES);

        screen.set_viewport_top_near(107);
        let before = screen.snapshot_viewport_lines();
        assert_ne!(
            before,
            vec![
                "line-10007".to_string(),
                "line-10008".to_string(),
                "line-10009".to_string()
            ]
        );

        let more = (10_010..10_060)
            .map(|idx| format!("line-{idx:05}"))
            .collect::<Vec<_>>()
            .join("\n");
        screen.feed(&format!("\n{more}"));

        assert_eq!(screen.scrollback_rows(), DEFAULT_SCROLLBACK_LINES);
        assert_eq!(screen.snapshot_viewport_lines(), before);

        Ok(())
    }

    #[test]
    fn each_screen_keeps_the_scrollback_capacity_it_was_created_with() {
        let mut small = NextCoreScreen::new_with_scrollback_limit(20, 2, 3);
        small.feed("one\ntwo\nthree\nfour\nfive\nsix");
        assert_eq!(small.scrollback_rows(), 3);

        let mut none = NextCoreScreen::new_with_scrollback_limit(20, 2, 0);
        none.feed("one\ntwo\nthree\nfour");
        assert_eq!(none.scrollback_rows(), 0);
    }

    #[test]
    fn osc133_prompts_can_be_navigated_and_survive_history_trimming() {
        let mut screen = NextCoreScreen::new_with_scrollback_limit(20, 3, 12);
        for idx in 0..4 {
            screen.feed(&format!("\x1b]133;A\x07prompt-{idx}\r\nout-{idx}\r\n"));
        }
        screen.feed("tail-0\r\ntail-1\r\ntail-2\r\ntail-3\r\ntail-4");

        assert!(screen.prompt_rows.len() >= 2);
        assert!(screen.scroll_viewport_to_prompt(-1));
        let newest_prompt_top = screen.viewport_start();
        assert!(screen.scroll_viewport_to_prompt(-1));
        assert!(screen.viewport_start() < newest_prompt_top);
        assert!(screen.scroll_viewport_to_prompt(1));
        assert_eq!(screen.viewport_start(), newest_prompt_top);

        let before = screen.prompt_rows.clone();
        screen.feed("\r\nmore-0\r\nmore-1\r\nmore-2\r\nmore-3");
        assert!(screen
            .prompt_rows
            .iter()
            .all(|row| *row < screen.history_len()));
        assert_ne!(screen.prompt_rows, before);
    }

    #[test]
    fn screen_buffer_scroll_to_bottom_follows_new_output() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let mut screen = NextCoreScreen::new(80, 3);

        screen.feed("one\ntwo\nthree\nfour\nfive");
        screen.set_viewport_top_near(1);
        assert_eq!(
            screen.snapshot_viewport_lines(),
            vec!["two".to_string(), "three".to_string(), "four".to_string()]
        );

        screen.set_viewport_top_near(2);
        assert!(!screen.history.viewport_is_pinned());
        assert_eq!(
            screen.snapshot_viewport_lines(),
            vec!["three".to_string(), "four".to_string(), "five".to_string()]
        );

        screen.feed("\nsix");
        assert_eq!(
            screen.snapshot_viewport_lines(),
            vec!["four".to_string(), "five".to_string(), "six".to_string()]
        );

        Ok(())
    }

    #[test]
    fn search_reports_multiple_matches_and_character_columns() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "你abc abc\nabc\n")?;

        let matches = engine.search(session.id, "abc", crate::SearchMode::CaseSensitive, 10)?;
        assert_eq!(
            matches
                .iter()
                .map(|m| (m.row, m.col, m.text.as_str()))
                .collect::<Vec<_>>(),
            vec![(0, 1, "你abc abc"), (0, 5, "你abc abc"), (1, 0, "abc")]
        );
        assert_eq!(
            engine
                .search(session.id, "abc", crate::SearchMode::CaseSensitive, 2)?
                .len(),
            2
        );
        assert!(engine
            .search(session.id, "", crate::SearchMode::CaseSensitive, 10)?
            .is_empty());
        assert!(engine
            .search(session.id, "abc", crate::SearchMode::CaseSensitive, 0)?
            .is_empty());

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_explicit_scroll_commands() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(session.id, "a\nb\nc\x1b[S")?;

        let lines = engine.read_screen(session.id)?.lines;
        assert_eq!(lines, vec!["b", "c", ""]);

        set_output_for_test(session.id, "a\nb\nc\x1b[T")?;
        let lines = engine.read_screen(session.id)?.lines;
        assert_eq!(lines, vec!["", "a", "b"]);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_applies_scroll_regions() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 5,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(
            session.id,
            concat!(
                "\x1b[1;1Htop",
                "\x1b[2;1Hone",
                "\x1b[3;1Htwo",
                "\x1b[4;1Hthree",
                "\x1b[5;1Hbottom",
                "\x1b[2;4r",
                "\x1b[4;1H\n"
            ),
        )?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["top", "two", "three", "", "bottom"]);
        assert_eq!(screen.scrollback_rows, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_limits_line_insert_delete_to_scroll_region() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;

        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 5,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(
            session.id,
            concat!(
                "\x1b[1;1Htop",
                "\x1b[2;1Hone",
                "\x1b[3;1Htwo",
                "\x1b[4;1Hthree",
                "\x1b[5;1Hbottom",
                "\x1b[2;4r",
                "\x1b[3;1H",
                "\x1b[L"
            ),
        )?;
        assert_eq!(
            engine.read_screen(session.id)?.lines,
            vec!["top", "one", "", "two", "bottom"]
        );
        engine.destroy_session(session.id)?;

        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 5,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(
            session.id,
            concat!(
                "\x1b[1;1Htop",
                "\x1b[2;1Hone",
                "\x1b[3;1Htwo",
                "\x1b[4;1Hthree",
                "\x1b[5;1Hbottom",
                "\x1b[2;4r",
                "\x1b[2;1H",
                "\x1b[M"
            ),
        )?;
        assert_eq!(
            engine.read_screen(session.id)?.lines,
            vec!["top", "two", "three", "", "bottom"]
        );
        engine.destroy_session(session.id)?;

        Ok(())
    }

    #[test]
    fn screen_buffer_honors_origin_mode_with_scroll_region() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 5,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(
            session.id,
            concat!(
                "\x1b[1;1Htop",
                "\x1b[2;1Hone",
                "\x1b[3;1Htwo",
                "\x1b[4;1Hthree",
                "\x1b[5;1Hbottom",
                "\x1b[2;4r",
                "\x1b[?6h",
                "\x1b[1;1HX",
                "\x1b[9;1HY",
                "\x1b[?6l",
                "Z"
            ),
        )?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["Zop", "Xne", "two", "Yhree", "bottom"]);
        assert_eq!(screen.cursor.x, 1);
        assert_eq!(screen.cursor.y, 0);
        assert_eq!(screen.scrollback_rows, 0);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_origin_mode_limits_vertical_cursor_motion_to_scroll_region() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 12,
            rows: 5,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(session.id, "\x1b[2;4r\x1b[?6h\x1b[9Bbottom\r\x1b[9Atop")?;
        let screen = engine.read_screen(session.id)?;

        assert_eq!(screen.lines, vec!["", "top", "", "bottom"]);
        assert_eq!(screen.cursor.x, 3);
        assert_eq!(screen.cursor.y, 1);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_line_cursor_motions_return_to_active_left_margin() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 10,
            rows: 5,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            "\x1b[?69h\x1b[3;8s\x1b[2;4r\x1b[?6h\x1b[1;4H\x1b[2EX\x1b[1FY",
        )?;
        let screen = engine.read_screen(session.id)?;

        assert_eq!(screen.lines, vec!["", "", "  Y", "  X"]);
        assert_eq!(screen.cursor.x, 3);
        assert_eq!(screen.cursor.y, 2);

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_keeps_alternate_screen_out_of_main_scrollback() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 2,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;
        set_output_for_test(
            session.id,
            concat!(
                "main-one\nmain-two",
                "\x1b[?1049h",
                "alt-one\nalt-two\nalt-three",
                "\x1b[?1049l"
            ),
        )?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["main-one", "main-two"]);
        assert!(engine
            .search(session.id, "alt-three", crate::SearchMode::CaseSensitive, 1)?
            .is_empty());
        assert!(engine.read_scrollback(session.id, 10)?.is_empty());

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn screen_buffer_keeps_alternate_screen_until_all_active_modes_leave() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 20,
            rows: 3,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        set_output_for_test(
            session.id,
            concat!(
                "main",
                "\x1b[?1047h",
                "alt",
                "\x1b[?1049h",
                "clear",
                "\x1b[?1047l",
                "still-alt",
                "\x1b[?1049l",
                "back"
            ),
        )?;

        let screen = engine.read_screen(session.id)?;
        assert_eq!(screen.lines, vec!["mainback"]);
        assert!(engine
            .search(session.id, "still-alt", crate::SearchMode::CaseSensitive, 1)?
            .is_empty());

        engine.destroy_session(session.id)?;
        Ok(())
    }

    #[test]
    fn recording_status_is_inactive_without_session() {
        let engine = NextCoreEngine;
        let status = engine.recording_status(123).unwrap();

        assert!(!status.enabled);
        assert_eq!(status.session_id, None);
        assert_eq!(status.started_at, None);
        assert_eq!(status.block_count, None);
        assert_eq!(status.bytes, None);
    }

    #[test]
    fn recording_lifecycle_taps_next_core_output() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let sessions_root = std::env::temp_dir().join(format!(
            "unterm-next-core-recording-{}-{suffix}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&sessions_root);
        std::fs::create_dir_all(&sessions_root)?;
        let project_dir = sessions_root.join("project");
        std::fs::create_dir_all(&project_dir)?;
        let previous_root = std::env::var("UNTERM_SESSIONS_ROOT").ok();
        std::env::set_var("UNTERM_SESSIONS_ROOT", &sessions_root);

        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 4,
            command_dir: Some(project_dir.display().to_string()),
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        let started = engine.start_recording(session.id)?;
        set_output_for_test(
            session.id,
            "\x1b[31mhello from next-core\x1b[0m token=super-secret-value\n",
        )?;
        let traces = engine.attach_recording_trace(session.id, "trace-1".to_string())?;
        let status = engine.recording_status(session.id)?;
        let stopped = engine.stop_recording(session.id)?;

        match previous_root {
            Some(value) => std::env::set_var("UNTERM_SESSIONS_ROOT", value),
            None => std::env::remove_var("UNTERM_SESSIONS_ROOT"),
        }

        assert_eq!(started.session_id, stopped.session_id);
        assert_eq!(traces, vec!["trace-1".to_string()]);
        assert!(status.enabled);
        assert!(status.block_count.unwrap_or_default() >= 1);
        assert!(std::fs::read_to_string(&started.log_path)?.contains("\tout\t"));
        let markdown = std::fs::read_to_string(&stopped.md_path)?;
        assert!(markdown.starts_with("---\n"));
        assert!(markdown.contains("unterm_session_id: "));
        assert!(markdown.contains("exit_reason: recording_stopped"));
        assert!(markdown.contains("ended_at: "));
        assert!(markdown.contains("osc133_active: false"));
        assert!(markdown.contains("block_render: chunked_output"));
        assert!(markdown.contains("## Output Blocks"));
        assert!(markdown.contains("### Block 1 `"));
        assert!(markdown.contains("## Aggregated Preview"));
        assert!(markdown.contains("trace_ids: [\"trace-1\"]"));
        assert!(markdown.contains("redaction_count: 1"));
        assert!(markdown.contains("hello from next-core [REDACTED]"));
        assert!(!markdown.contains("\x1b[31m"));
        assert!(!markdown.contains("super-secret-value"));
        assert!(std::fs::read_to_string(sessions_root.join("index.json"))?.contains("trace-1"));

        engine.destroy_session(session.id)?;
        let _ = std::fs::remove_dir_all(&sessions_root);
        Ok(())
    }

    #[test]
    fn recording_markdown_renders_osc133_command_blocks() -> Result<()> {
        let _guard = test_guard();
        let _runtime_guard = reset_state_for_test();
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let sessions_root = std::env::temp_dir().join(format!(
            "unterm-next-core-osc133-recording-{}-{suffix}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&sessions_root);
        std::fs::create_dir_all(&sessions_root)?;
        let previous_root = std::env::var("UNTERM_SESSIONS_ROOT").ok();
        std::env::set_var("UNTERM_SESSIONS_ROOT", &sessions_root);

        let engine = NextCoreEngine;
        let session = engine.create_session(CreateSessionRequest {
            cols: 80,
            rows: 4,
            command_dir: None,
            command: None,
            env: Vec::new(),
            launch_policy: Default::default(),
        })?;

        let started = engine.start_recording(session.id)?;
        set_output_for_test(
            session.id,
            concat!(
                "prompt> echo hi\r\n",
                "\x1b]133;C\x07",
                "command output token=super-secret-value\r\n",
                "\x1b]133;D;0\x07",
                "prompt> "
            ),
        )?;
        let stopped = engine.stop_recording(session.id)?;

        match previous_root {
            Some(value) => std::env::set_var("UNTERM_SESSIONS_ROOT", value),
            None => std::env::remove_var("UNTERM_SESSIONS_ROOT"),
        }

        assert_eq!(started.session_id, stopped.session_id);
        let markdown = std::fs::read_to_string(&stopped.md_path)?;
        assert!(markdown.contains("osc133_active: true"));
        assert!(markdown.contains("block_render: osc133_command_blocks"));
        assert!(markdown.contains("command_block_count: 1"));
        assert!(markdown.contains("## Command Blocks"));
        assert!(markdown.contains("### Command 1 `"));
        assert!(markdown.contains("exit_code: `0`"));
        assert!(markdown.contains("command output [REDACTED]"));
        assert!(!markdown.contains("OSC133 command markers are not available yet"));
        assert!(!markdown.contains("super-secret-value"));
        let index = std::fs::read_to_string(sessions_root.join("index.json"))?;
        assert!(index.contains("\"osc133_active\": true"));

        engine.destroy_session(session.id)?;
        let _ = std::fs::remove_dir_all(&sessions_root);
        Ok(())
    }

    /// A flood fed straight into the screen model: no PTY, no shell, no lock.
    ///
    /// `--bench-flood-lines` measures a pipeline -- awk generating lines, the
    /// kernel moving them through a pty, the probe's main thread polling the
    /// output under the same mutex the reader holds. On a busy machine the
    /// reader spends most of its samples parked in `read()`, and a change to
    /// the terminal model disappears into the noise. This measures only the
    /// part this file is responsible for.
    ///
    /// Ignored by default: it is a stopwatch, not an assertion.
    /// `cargo test --release -p unterm-engine feed_flood_throughput --
    /// --ignored --nocapture`
    #[test]
    #[ignore]
    fn feed_flood_throughput() {
        let lines: usize = std::env::var("UNTERM_BENCH_LINES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(200_000);
        let mut payload = String::new();
        for index in 0..lines {
            payload.push_str(&format!("UNTERM_NEXT_CORE_FLOOD_{index}\r\n"));
        }

        // Chunked at 8 KiB, the size of the pty reader's own buffer, so the
        // parser sees the same shape of input it sees in production.
        let mut chunks = Vec::new();
        let mut start = 0;
        while start < payload.len() {
            let mut end = (start + 8192).min(payload.len());
            while !payload.is_char_boundary(end) {
                end -= 1;
            }
            chunks.push(&payload[start..end]);
            start = end;
        }

        let mut screen = NextCoreScreen::new(80, 24);
        let started = std::time::Instant::now();
        for chunk in &chunks {
            screen.feed(chunk);
        }
        let elapsed = started.elapsed();
        eprintln!(
            "feed_flood lines={} bytes={} elapsed_ms={} lines_per_sec={:.1}",
            lines,
            payload.len(),
            elapsed.as_millis(),
            lines as f64 / elapsed.as_secs_f64().max(0.000_001)
        );
        assert_eq!(screen.scrollback_rows(), DEFAULT_SCROLLBACK_LINES);
    }
}
