//! Engine-neutral terminal access layer.
//!
//! Unterm's product surface should not depend directly on WezTerm internals.
//! This crate is the migration boundary for next-core and any future adapter.

pub mod next_core;

use anyhow::Result;
use portable_pty::CommandBuilder;
use serde::{Deserialize, Serialize};

pub fn next_core() -> next_core::NextCoreEngine {
    next_core::NextCoreEngine
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CursorSnapshot {
    pub x: usize,
    pub y: isize,
    pub visible: bool,
    pub shape: String,
}

/// Terminal modes a GUI pane has to expose without reaching into the engine's
/// internals: whether the application grabbed the mouse, and whether the
/// alternate screen is up.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaneModesSnapshot {
    pub mouse_grabbed: bool,
    pub alt_screen_active: bool,
    pub bracketed_paste: bool,
    pub application_cursor_keys: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneDimensions {
    pub cols: usize,
    pub viewport_rows: usize,
    pub scrollback_rows: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShellSnapshot {
    pub shell_type: String,
    pub process_name: String,
    pub cwd: Option<String>,
    pub launch_env_keys: Vec<String>,
    pub launch_context: LaunchContextSnapshot,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LaunchContextSnapshot {
    pub profile: Option<String>,
    pub proxy_env_keys: Vec<String>,
    pub env_key_count: usize,
    pub policy: LaunchPolicySnapshot,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum LaunchEnvSource {
    Proxy,
    Profile,
    Overlay,
    Explicit,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchEnvBinding {
    pub key: String,
    pub source: LaunchEnvSource,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LaunchPolicyDecision {
    #[default]
    NotRequested,
    Applied,
    Deferred,
    Unsupported,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchPolicyDecisionSnapshot {
    pub decision: LaunchPolicyDecision,
    pub supported: bool,
    pub reason: String,
}

impl Default for LaunchPolicyDecisionSnapshot {
    fn default() -> Self {
        Self {
            decision: LaunchPolicyDecision::NotRequested,
            supported: false,
            reason: "not requested".to_string(),
        }
    }
}

impl LaunchPolicyDecisionSnapshot {
    pub fn new(decision: LaunchPolicyDecision, supported: bool, reason: impl Into<String>) -> Self {
        Self {
            decision,
            supported,
            reason: reason.into(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchPolicySnapshot {
    pub profile: Option<String>,
    pub env: Vec<LaunchEnvBinding>,
    pub proxy_env_keys: Vec<String>,
    pub domain: LaunchPolicyDecisionSnapshot,
    pub privilege: LaunchPolicyDecisionSnapshot,
    pub proxy_rotation: LaunchPolicyDecisionSnapshot,
    pub restart: LaunchPolicyDecisionSnapshot,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub id: usize,
    /// The pane this one was split off from, if it was.
    ///
    /// A front end arranges panes; the kernel only runs them. But when
    /// something *else* asks for a split -- an agent over MCP, say -- the
    /// front end has no other way to tell a new pane beside an existing one
    /// from a whole new tab, and would show the wrong thing.
    pub split_from: Option<usize>,
    /// How the split that made this pane divides its space, when one
    /// did. A front end rebuilding an arrangement it did not create --
    /// after a restart, or in another window -- has no other way to
    /// know: `split_from` says which pane it came from, and these two
    /// say what it looked like.
    pub split_axis: Option<next_core::layout::SplitAxis>,
    /// What fraction of the room the *first* pane keeps. Which pane is
    /// first depends on the direction the split was asked for; the
    /// engine resolves that here so every consumer agrees.
    pub split_ratio: Option<f64>,
    /// Which half of the split this pane took. Splitting left or up puts
    /// it first, right or down second — and without this a front end has
    /// to guess, which is how `split left` used to rebuild on the right.
    #[serde(default)]
    pub split_side: Option<next_core::layout::SplitSide>,
    pub title: String,
    pub cols: usize,
    pub rows: usize,
    pub scrollback_rows: usize,
    pub cursor: CursorSnapshot,
    pub is_dead: bool,
    pub dead_reason: Option<String>,
    pub is_active: bool,
    pub domain_id: usize,
    pub shell: ShellSnapshot,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScreenLine {
    pub row: i64,
    pub text: String,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum StyledColor {
    Palette(u8),
    Rgb(u8, u8, u8),
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum StyledBlink {
    Slow,
    Rapid,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum StyledUnderline {
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum StyledVerticalAlign {
    SuperScript,
    SubScript,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CellStyle {
    pub bold: bool,
    pub faint: bool,
    pub italic: bool,
    pub underline: bool,
    pub underline_style: Option<StyledUnderline>,
    pub underline_color: Option<StyledColor>,
    pub strikethrough: bool,
    pub hidden: bool,
    pub overline: bool,
    pub blink: Option<StyledBlink>,
    pub vertical_align: Option<StyledVerticalAlign>,
    pub inverse: bool,
    pub fg: Option<StyledColor>,
    pub bg: Option<StyledColor>,
    pub hyperlink: Option<String>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StyledCell {
    pub ch: char,
    pub style: CellStyle,
    pub width: usize,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StyledScreenLine {
    pub row: i64,
    pub cells: Vec<StyledCell>,
    /// True when this row soft-wrapped into the next one.
    ///
    /// A consumer that reassembles logical lines needs this: without it a
    /// wrapped command reads back as several lines and copying it inserts a
    /// newline the user never typed.
    pub wrapped: bool,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirtyRows {
    pub start: usize,
    pub end: usize,
}

#[allow(dead_code)]

/// What a front end needs to keep an eye on a pane it is not drawing.
///
/// The Cockpit watches every pane at once: is anything new, did a program
/// ring, does the tail look like an error. It needs a handful of lines of
/// *text* and two counters — and it used to get that by asking for the whole
/// styled screen, four thousand eight hundred cells with colours and
/// attributes, and then throwing all of it away except the characters. At
/// forty panes that read cost 3.8 seconds of a 400 ms poll.
///
/// `unchanged` is the other half. A pane nobody has typed in does not need
/// its tail sent again, and comparing revisions server-side means an idle
/// pane costs an empty envelope.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PanePulse {
    pub revision: u64,
    /// True when `since_revision` matched. `tail` is empty and means nothing.
    pub unchanged: bool,
    /// The last few rows, as text, oldest first.
    pub tail: Vec<String>,
    pub bells: u64,
    pub notifications: u64,
    pub last_notification: Option<String>,
}

impl PanePulse {
    /// Build one from a full snapshot.
    ///
    /// The fallback for engines with nothing cheaper — the local one already
    /// has the cells in memory, so there is nothing to save there.
    pub fn from_snapshot(
        snapshot: &StyledScreenSnapshot,
        since_revision: Option<u64>,
        tail_rows: usize,
    ) -> Self {
        if since_revision == Some(snapshot.revision) {
            return Self {
                revision: snapshot.revision,
                unchanged: true,
                bells: snapshot.bells,
                notifications: snapshot.notifications,
                last_notification: snapshot.last_notification.clone(),
                ..Self::default()
            };
        }
        let tail: Vec<String> = snapshot
            .lines
            .iter()
            .rev()
            .take(tail_rows)
            .map(|line| line.cells.iter().map(|cell| cell.ch).collect::<String>())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Self {
            revision: snapshot.revision,
            unchanged: false,
            tail,
            bells: snapshot.bells,
            notifications: snapshot.notifications,
            last_notification: snapshot.last_notification.clone(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StyledScreenSnapshot {
    /// Text a program asked to be put on the system clipboard.
    ///
    /// `OSC 52`, which is how tmux copies out of a remote session and the
    /// only way anything over ssh reaches the clipboard at all. The kernel
    /// has no clipboard and should not grow one; it reports the request and a
    /// front end decides. A read request -- `OSC 52 ; c ; ?` -- never appears
    /// here: reporting the user's clipboard to any program that can print is
    /// how a terminal leaks a password.
    pub clipboard_request: Option<String>,
    /// The program asked to be told when the terminal gains or loses focus.
    ///
    /// `CSI ? 1004 h`. vim reloads a changed file on it, tmux redraws its
    /// borders, and a shell prompt can dim itself -- all of which stay stuck
    /// in whichever state they were last told about if the terminal never
    /// says anything.
    pub focus_reporting: bool,
    /// How many times a program has rung the bell in this pane.
    ///
    /// A running total, not a flag: a front end compares it with what it saw
    /// last frame, so two bells between frames are not one, and a reader that
    /// forgets to clear anything cannot show the same bell twice.
    pub bells: u64,
    /// Notifications a program raised (`OSC 9`/`777`): a running count, and
    /// the newest text so a front end can show it.
    pub notifications: u64,
    pub last_notification: Option<String>,
    /// What the program in this pane wants from the mouse.
    ///
    /// A front end has to ask before deciding what a click means: with
    /// reporting on, the click belongs to the program -- vim, htop, less --
    /// and the terminal must not also start a selection with it.
    pub mouse: next_core::mouse_encoding::MouseModes,
    pub lines: Vec<StyledScreenLine>,
    pub cursor: CursorSnapshot,
    pub cols: usize,
    pub rows: usize,
    pub scrollback_rows: usize,
    pub revision: u64,
    pub dirty_rows: Option<DirtyRows>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenderFrameSnapshot {
    pub lines: Vec<StyledScreenLine>,
    pub cursor: CursorSnapshot,
    pub cols: usize,
    pub rows: usize,
    pub scrollback_rows: usize,
    pub revision: u64,
    pub dirty_rows: Option<DirtyRows>,
    pub full: bool,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderGlyphRun {
    pub row: usize,
    pub col: usize,
    pub cells: usize,
    pub text: String,
    pub style: CellStyle,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderCellRun {
    pub row: usize,
    pub col: usize,
    pub cells: usize,
    pub style: CellStyle,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderCursorDraw {
    pub row: usize,
    pub col: usize,
    pub visible: bool,
    pub shape: String,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderDrawPlan {
    pub revision: u64,
    pub cols: usize,
    pub rows: usize,
    pub scrollback_rows: usize,
    pub dirty_rows: Option<DirtyRows>,
    pub full: bool,
    pub glyph_runs: Vec<RenderGlyphRun>,
    pub cell_runs: Vec<RenderCellRun>,
    pub cursor: Option<RenderCursorDraw>,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderCellMetrics {
    pub cell_width_px: usize,
    pub cell_height_px: usize,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderRect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderGlyphRunGeometry {
    pub row: usize,
    pub col: usize,
    pub cells: usize,
    pub text: String,
    pub rect: RenderRect,
    pub style: CellStyle,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderCellRunGeometry {
    pub row: usize,
    pub col: usize,
    pub rect: RenderRect,
    pub style: CellStyle,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderCursorGeometry {
    pub row: usize,
    pub col: usize,
    pub rect: RenderRect,
    pub visible: bool,
    pub shape: String,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderGeometryPlan {
    pub revision: u64,
    pub cols: usize,
    pub rows: usize,
    pub scrollback_rows: usize,
    pub dirty_rows: Option<DirtyRows>,
    pub full: bool,
    pub viewport: RenderRect,
    pub glyph_runs: Vec<RenderGlyphRunGeometry>,
    pub cell_runs: Vec<RenderCellRunGeometry>,
    pub cursor: Option<RenderCursorGeometry>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderBackgroundQuad {
    pub rect: RenderRect,
    pub style: CellStyle,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderTextRun {
    pub row: usize,
    pub col: usize,
    pub cells: usize,
    pub text: String,
    pub rect: RenderRect,
    pub style: CellStyle,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderCursorQuad {
    pub rect: RenderRect,
    pub visible: bool,
    pub shape: String,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderSubmissionPlan {
    pub revision: u64,
    pub cols: usize,
    pub rows: usize,
    pub scrollback_rows: usize,
    pub viewport: RenderRect,
    pub full: bool,
    pub damage_rects: Vec<RenderRect>,
    pub background_quads: Vec<RenderBackgroundQuad>,
    pub text_runs: Vec<RenderTextRun>,
    pub cursor: Option<RenderCursorQuad>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderConsumerState {
    submitted_revision: Option<u64>,
    viewport: Option<RenderRect>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderCommitPlan {
    pub submit: bool,
    pub previous_revision: Option<u64>,
    pub revision: u64,
    pub skipped_revisions: u64,
    pub requires_full_repaint: bool,
    pub submission: Option<RenderSubmissionPlan>,
}

impl RenderDrawPlan {
    #[allow(dead_code)]
    pub fn to_geometry_plan(&self, metrics: RenderCellMetrics) -> RenderGeometryPlan {
        let glyph_runs = self
            .glyph_runs
            .iter()
            .map(|run| RenderGlyphRunGeometry {
                row: run.row,
                col: run.col,
                cells: run.cells,
                text: run.text.clone(),
                rect: grid_rect(run.row, run.col, run.cells, 1, metrics),
                style: run.style.clone(),
            })
            .collect();
        let cell_runs = self
            .cell_runs
            .iter()
            .map(|run| RenderCellRunGeometry {
                row: run.row,
                col: run.col,
                rect: grid_rect(run.row, run.col, run.cells, 1, metrics),
                style: run.style.clone(),
            })
            .collect();
        let cursor = self.cursor.as_ref().map(|cursor| RenderCursorGeometry {
            row: cursor.row,
            col: cursor.col,
            rect: grid_rect(cursor.row, cursor.col, 1, 1, metrics),
            visible: cursor.visible,
            shape: cursor.shape.clone(),
        });

        RenderGeometryPlan {
            revision: self.revision,
            cols: self.cols,
            rows: self.rows,
            scrollback_rows: self.scrollback_rows,
            dirty_rows: self.dirty_rows,
            full: self.full,
            viewport: grid_rect(0, 0, self.cols, self.rows, metrics),
            glyph_runs,
            cell_runs,
            cursor,
        }
    }
}

impl RenderConsumerState {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(dead_code)]
    pub fn submitted_revision(&self) -> Option<u64> {
        self.submitted_revision
    }

    #[allow(dead_code)]
    pub fn prepare_commit(&mut self, mut submission: RenderSubmissionPlan) -> RenderCommitPlan {
        let previous_revision = self.submitted_revision;
        if previous_revision == Some(submission.revision)
            && self.viewport == Some(submission.viewport)
        {
            return RenderCommitPlan {
                submit: false,
                previous_revision,
                revision: submission.revision,
                skipped_revisions: 0,
                requires_full_repaint: false,
                submission: None,
            };
        }

        let viewport_changed = self
            .viewport
            .is_some_and(|viewport| viewport != submission.viewport);
        let requires_full_repaint =
            previous_revision.is_none() || viewport_changed || submission.full;
        let skipped_revisions = previous_revision
            .map(|revision| {
                submission
                    .revision
                    .saturating_sub(revision)
                    .saturating_sub(1)
            })
            .unwrap_or(0);

        if requires_full_repaint {
            submission.full = true;
            submission.damage_rects = vec![submission.viewport];
        }

        self.submitted_revision = Some(submission.revision);
        self.viewport = Some(submission.viewport);

        RenderCommitPlan {
            submit: true,
            previous_revision,
            revision: submission.revision,
            skipped_revisions,
            requires_full_repaint,
            submission: Some(submission),
        }
    }
}

impl RenderGeometryPlan {
    #[allow(dead_code)]
    pub fn to_submission_plan(&self) -> RenderSubmissionPlan {
        let damage_rects = self.damage_rects();
        let background_quads = self
            .cell_runs
            .iter()
            .map(|run| RenderBackgroundQuad {
                rect: run.rect,
                style: run.style.clone(),
            })
            .collect();
        let text_runs = self
            .glyph_runs
            .iter()
            .map(|run| RenderTextRun {
                row: run.row,
                col: run.col,
                cells: run.cells,
                text: run.text.clone(),
                rect: run.rect,
                style: run.style.clone(),
            })
            .collect();
        let cursor = self.cursor.as_ref().map(|cursor| RenderCursorQuad {
            rect: cursor.rect,
            visible: cursor.visible,
            shape: cursor.shape.clone(),
        });

        RenderSubmissionPlan {
            revision: self.revision,
            cols: self.cols,
            rows: self.rows,
            scrollback_rows: self.scrollback_rows,
            viewport: self.viewport,
            full: self.full,
            damage_rects,
            background_quads,
            text_runs,
            cursor,
        }
    }

    fn damage_rects(&self) -> Vec<RenderRect> {
        if self.full {
            return vec![self.viewport];
        }

        let Some(dirty_rows) = self.dirty_rows else {
            return Vec::new();
        };
        if self.rows == 0 || dirty_rows.start > dirty_rows.end {
            return Vec::new();
        }

        let cell_height = self.viewport.height / self.rows;
        let row_count = dirty_rows.end.saturating_sub(dirty_rows.start) + 1;
        vec![RenderRect {
            x: 0,
            y: dirty_rows.start.saturating_mul(cell_height),
            width: self.viewport.width,
            height: row_count.saturating_mul(cell_height),
        }]
    }
}

fn grid_rect(
    row: usize,
    col: usize,
    cells: usize,
    rows: usize,
    metrics: RenderCellMetrics,
) -> RenderRect {
    RenderRect {
        x: col.saturating_mul(metrics.cell_width_px),
        y: row.saturating_mul(metrics.cell_height_px),
        width: cells.saturating_mul(metrics.cell_width_px),
        height: rows.saturating_mul(metrics.cell_height_px),
    }
}

impl RenderFrameSnapshot {
    #[allow(dead_code)]
    pub fn to_draw_plan(&self) -> RenderDrawPlan {
        let mut glyph_runs = Vec::new();
        let mut cell_runs = Vec::new();

        for (line_idx, line) in self.lines.iter().enumerate().take(self.rows) {
            let row_idx = self.draw_plan_row_for_line(line_idx);
            let mut active_glyph: Option<RenderGlyphRun> = None;
            let mut active_cell: Option<RenderCellRun> = None;
            for (col_idx, cell) in line.cells.iter().enumerate().take(self.cols) {
                push_cell_run(
                    &mut cell_runs,
                    &mut active_cell,
                    row_idx,
                    col_idx,
                    &cell.style,
                );

                if cell.width == 0 || cell.style.hidden || cell.ch == ' ' {
                    flush_glyph_run(&mut glyph_runs, &mut active_glyph);
                    continue;
                }

                match active_glyph.as_mut() {
                    Some(run) if run.style == cell.style && run.col + run.cells == col_idx => {
                        run.text.push(cell.ch);
                        run.cells += cell.width.max(1);
                    }
                    _ => {
                        flush_glyph_run(&mut glyph_runs, &mut active_glyph);
                        active_glyph = Some(RenderGlyphRun {
                            row: row_idx,
                            col: col_idx,
                            cells: cell.width.max(1),
                            text: cell.ch.to_string(),
                            style: cell.style.clone(),
                        });
                    }
                }
            }
            flush_glyph_run(&mut glyph_runs, &mut active_glyph);
            flush_cell_run(&mut cell_runs, &mut active_cell);
        }

        RenderDrawPlan {
            revision: self.revision,
            cols: self.cols,
            rows: self.rows,
            scrollback_rows: self.scrollback_rows,
            dirty_rows: self.dirty_rows,
            full: self.full,
            glyph_runs,
            cell_runs,
            cursor: self.cursor_draw(),
        }
    }

    fn cursor_draw(&self) -> Option<RenderCursorDraw> {
        if self.cursor.y < 0 {
            return None;
        }
        let row = self.cursor.y as usize;
        (row < self.rows && self.cursor.x < self.cols).then(|| RenderCursorDraw {
            row,
            col: self.cursor.x,
            visible: self.cursor.visible,
            shape: self.cursor.shape.clone(),
        })
    }

    fn draw_plan_row_for_line(&self, line_idx: usize) -> usize {
        if self.full {
            return line_idx;
        }
        self.dirty_rows
            .map(|rows| rows.start.saturating_add(line_idx))
            .unwrap_or(line_idx)
    }
}

fn flush_glyph_run(runs: &mut Vec<RenderGlyphRun>, active: &mut Option<RenderGlyphRun>) {
    if let Some(run) = active.take() {
        runs.push(run);
    }
}

fn flush_cell_run(runs: &mut Vec<RenderCellRun>, active: &mut Option<RenderCellRun>) {
    if let Some(run) = active.take() {
        runs.push(run);
    }
}

fn push_cell_run(
    runs: &mut Vec<RenderCellRun>,
    active: &mut Option<RenderCellRun>,
    row: usize,
    col: usize,
    style: &CellStyle,
) {
    match active.as_mut() {
        Some(run) if run.style == *style && run.col + run.cells == col => {
            run.cells += 1;
        }
        _ => {
            flush_cell_run(runs, active);
            *active = Some(RenderCellRun {
                row,
                col,
                cells: 1,
                style: style.clone(),
            });
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StyledScrollbackSnapshot {
    pub lines: Vec<StyledScreenLine>,
    pub first_row: i64,
    pub row_count: i64,
    pub cols: usize,
    pub scrollback_top: i64,
    pub physical_top: i64,
    pub viewport_rows: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScreenSearchMatch {
    pub row: i64,
    pub col: usize,
    pub text: String,
}

/// How a search pattern reads its haystack.
///
/// Ignore-case is the default because it is the friendlier one for a person
/// typing into a search bar; exact and regex matching are a cycle away.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchMode {
    CaseSensitive,
    #[default]
    CaseInsensitive,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScreenSnapshot {
    /// Text a program asked to be put on the system clipboard.
    ///
    /// `OSC 52`, which is how tmux copies out of a remote session and the
    /// only way anything over ssh reaches the clipboard at all. The kernel
    /// has no clipboard and should not grow one; it reports the request and a
    /// front end decides. A read request -- `OSC 52 ; c ; ?` -- never appears
    /// here: reporting the user's clipboard to any program that can print is
    /// how a terminal leaks a password.
    pub clipboard_request: Option<String>,
    /// The program asked to be told when the terminal gains or loses focus.
    ///
    /// `CSI ? 1004 h`. vim reloads a changed file on it, tmux redraws its
    /// borders, and a shell prompt can dim itself -- all of which stay stuck
    /// in whichever state they were last told about if the terminal never
    /// says anything.
    pub focus_reporting: bool,
    /// How many times a program has rung the bell in this pane.
    ///
    /// A running total, not a flag: a front end compares it with what it saw
    /// last frame, so two bells between frames are not one, and a reader that
    /// forgets to clear anything cannot show the same bell twice.
    pub bells: u64,
    /// Notifications a program raised (`OSC 9`/`777`): a running count, and
    /// the newest text so a front end can show it.
    pub notifications: u64,
    pub last_notification: Option<String>,
    /// What the program in this pane wants from the mouse.
    ///
    /// A front end has to ask before deciding what a click means: with
    /// reporting on, the click belongs to the program -- vim, htop, less --
    /// and the terminal must not also start a selection with it.
    pub mouse: next_core::mouse_encoding::MouseModes,
    pub lines: Vec<String>,
    pub cells: Vec<ScreenLine>,
    pub cursor: CursorSnapshot,
    pub cols: usize,
    pub rows: usize,
    pub scrollback_rows: usize,
    pub revision: u64,
    pub dirty_rows: Option<DirtyRows>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScrollbackTextRequest {
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub tail_lines: Option<i64>,
    pub escapes: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScrollbackTextSnapshot {
    pub text: String,
    pub lines: Vec<String>,
    pub first_row: i64,
    pub row_count: i64,
    pub cols: usize,
    pub escapes: bool,
    pub scrollback_top: i64,
    pub physical_top: i64,
    pub viewport_rows: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputActivitySnapshot {
    pub total_writes: u64,
    pub total_bytes: u64,
    pub last_bytes: usize,
    pub last_duration_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputActivitySnapshot {
    pub total_chunks: u64,
    pub total_bytes: u64,
    pub total_terminal_response_bytes: u64,
    pub recorded_chunks: u64,
    pub last_bytes: usize,
    pub last_terminal_response_bytes: usize,
    pub last_recorded: bool,
    pub last_duration_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PasteActivitySnapshot {
    pub total_pastes: u64,
    pub total_text_bytes: u64,
    pub total_chunks: u64,
    pub last_text_bytes: usize,
    pub last_wire_bytes: usize,
    pub last_chunk_count: usize,
    pub last_bracketed: bool,
    pub last_duration_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScreenActivitySnapshot {
    pub total_reads: u64,
    pub total_viewport_scrolls: u64,
    pub last_read_duration_ms: u64,
    pub last_scroll_duration_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessTreeSnapshot {
    pub root_pid: Option<u32>,
    pub root_process: String,
    pub root_cwd: Option<String>,
    pub foreground_pid: Option<u32>,
    pub foreground_process: String,
    pub foreground_cwd: Option<String>,
    pub foreground_argv: Vec<String>,
    pub child_count: usize,
    pub detected_agent: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionActivitySnapshot {
    pub idle: bool,
    pub foreground_process: String,
    pub process: Option<ProcessTreeSnapshot>,
    pub input: Option<InputActivitySnapshot>,
    pub output: Option<OutputActivitySnapshot>,
    pub paste: Option<PasteActivitySnapshot>,
    pub screen: Option<ScreenActivitySnapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordingStartResult {
    pub session_id: String,
    pub log_path: String,
    pub md_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordingStopResult {
    pub session_id: String,
    pub ended_at: String,
    pub block_count: u64,
    pub exit_reason: String,
    pub md_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordingExportResult {
    pub session_id: String,
    pub path: String,
    pub bytes: usize,
    pub block_count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordingStatusSnapshot {
    pub enabled: bool,
    pub session_id: Option<String>,
    pub started_at: Option<String>,
    pub block_count: Option<u64>,
    pub bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EngineIoHealthSnapshot {
    pub input_writes: u64,
    pub input_bytes: u64,
    pub output_chunks: u64,
    pub output_bytes: u64,
    pub paste_count: u64,
    pub paste_text_bytes: u64,
    pub screen_reads: u64,
    pub viewport_scrolls: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EngineLifecycleHealthSnapshot {
    pub live_sessions: u64,
    pub dead_sessions: u64,
    pub total_created: u64,
    pub total_destroyed: u64,
    pub total_marked_dead: u64,
    pub last_dead_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EngineRuntimeQueueHealthSnapshot {
    pub pending_commands: usize,
    pub pending_input_bytes: usize,
    pub pending_lifecycle_commands: usize,
    pub pending_input_commands: usize,
    pub pending_render_commands: usize,
    pub pending_screen_commands: usize,
    pub pending_background_commands: usize,
    pub rejected_commands: u64,
    pub rejected_input_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EngineRuntimePumpHealthSnapshot {
    pub drain_calls: u64,
    pub dispatched_commands: u64,
    pub dispatched_lifecycle_commands: u64,
    pub dispatched_input_commands: u64,
    pub dispatched_render_commands: u64,
    pub dispatched_screen_commands: u64,
    pub dispatched_background_commands: u64,
    pub waited_for_response: u64,
    pub completed_without_wait: u64,
    pub total_dispatch_elapsed_micros: u64,
    pub max_dispatch_elapsed_micros: u64,
    pub total_drain_elapsed_micros: u64,
    pub max_drain_elapsed_micros: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EngineHealthSnapshot {
    pub engine: String,
    pub ready: bool,
    pub status: String,
    pub detail: String,
    pub pane_count: Option<usize>,
    pub io: Option<EngineIoHealthSnapshot>,
    pub lifecycle: Option<EngineLifecycleHealthSnapshot>,
    pub runtime_queue: Option<EngineRuntimeQueueHealthSnapshot>,
    pub runtime_pump: Option<EngineRuntimePumpHealthSnapshot>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum SplitDirection {
    Right,
    Left,
    Down,
    Up,
}

/// The narrowest and shortest grid a pane may be told it has.
///
/// Narrowing a screen truncates every line and the whole scrollback to the
/// new width; that is the right answer for a window someone dragged in, and
/// the wrong one for a number nobody meant. Chrome that does not fit, a
/// window reporting no size, an agent passing `cols: 1` -- each of those
/// bottoms out at a single cell somewhere, and a single cell is not a narrow
/// view of a pane's output, it is the loss of it.
///
/// So both ends hold the same floor: a front end sizes its grid to at least
/// this, and the engine refuses anything below it. Two, because a terminal
/// one cell wide can hold no line worth keeping.
pub const MIN_SESSION_GRID: usize = 2;

#[derive(Debug)]
pub struct CreateSessionRequest {
    pub cols: usize,
    pub rows: usize,
    pub command_dir: Option<String>,
    pub command: Option<CommandBuilder>,
    pub env: Vec<(String, String)>,
    pub launch_policy: LaunchPolicySnapshot,
}

#[derive(Clone, Debug)]
pub struct SplitSessionRequest {
    pub source_pane_id: usize,
    pub direction: SplitDirection,
    pub size_percent: u8,
    pub command_dir: Option<String>,
    /// What to run in the new pane.
    ///
    /// The caller's choice, not the kernel's: which shell a user gets, and
    /// what encoding switches it needs, is a product decision. `None` means
    /// the platform default, which is what the kernel would have picked
    /// anyway.
    pub command: Option<CommandBuilder>,
    pub env: Vec<(String, String)>,
    pub launch_policy: LaunchPolicySnapshot,
}

pub trait SessionEngine {
    fn list_sessions(&self) -> Result<Vec<SessionSnapshot>>;
    fn get_session(&self, pane_id: usize) -> Result<SessionSnapshot>;
    fn create_session(&self, request: CreateSessionRequest) -> Result<SessionSnapshot>;
    fn split_session(&self, request: SplitSessionRequest) -> Result<SessionSnapshot>;
    fn focus_session(&self, pane_id: usize) -> Result<()>;
    fn shell(&self, pane_id: usize) -> Result<ShellSnapshot>;
    fn activity(&self, pane_id: usize) -> Result<SessionActivitySnapshot>;
    fn resize_session(&self, pane_id: usize, cols: usize, rows: usize) -> Result<()>;
    fn destroy_session(&self, pane_id: usize) -> Result<()>;

    /// Record where the divider of `pane_id`'s split now sits.
    ///
    /// The split is written down when it is made, and never again: a front
    /// end that lets the divider be dragged and keeps the new size to itself
    /// has an arrangement that comes back from a restart at the size it was
    /// created at, not the one the user left it at. `pane_id` is the pane the
    /// split belongs to, which `SplitRatioChange` names.
    ///
    /// Defaults to refusing, in the same spirit as `erase_scrollback`: an
    /// engine that keeps no arrangement should say so rather than accept the
    /// number and drop it.
    fn set_split_ratio(&self, _pane_id: usize, _first_ratio: f64) -> Result<()> {
        anyhow::bail!("this engine does not record split ratios")
    }
}

pub trait ScreenEngine {
    fn read_screen(&self, pane_id: usize) -> Result<ScreenSnapshot>;

    /// Throw away a pane's history, and the visible screen with it when asked.
    ///
    /// Not the `CSI 3 J` a program can send: this is somebody -- a person at
    /// the keyboard or an agent that has just filled a pane with a build log
    /// -- saying they are done with it. The cursor stays where it is rather
    /// than being homed, because nothing about the running command changed.
    ///
    /// Defaults to refusing rather than to quietly doing nothing: an engine
    /// that cannot forget should say so, or a caller has no way to tell the
    /// difference between "cleared" and "ignored".
    fn erase_scrollback(&self, _pane_id: usize, _include_viewport: bool) -> Result<()> {
        anyhow::bail!("this engine cannot clear a pane's history")
    }

    #[allow(dead_code)]
    fn read_styled_screen(&self, pane_id: usize) -> Result<StyledScreenSnapshot>;
    #[allow(dead_code)]
    fn read_render_frame(
        &self,
        pane_id: usize,
        since_revision: Option<u64>,
    ) -> Result<RenderFrameSnapshot> {
        let screen = self.read_styled_screen(pane_id)?;
        if since_revision == Some(screen.revision) {
            return Ok(RenderFrameSnapshot {
                lines: Vec::new(),
                cursor: screen.cursor,
                cols: screen.cols,
                rows: screen.rows,
                scrollback_rows: screen.scrollback_rows,
                revision: screen.revision,
                dirty_rows: None,
                full: false,
            });
        }

        let dirty_rows = if screen.rows == 0 {
            None
        } else {
            Some(DirtyRows {
                start: 0,
                end: screen.rows - 1,
            })
        };
        Ok(RenderFrameSnapshot {
            lines: screen.lines,
            cursor: screen.cursor,
            cols: screen.cols,
            rows: screen.rows,
            scrollback_rows: screen.scrollback_rows,
            revision: screen.revision,
            dirty_rows,
            full: true,
        })
    }
    #[allow(dead_code)]
    fn read_render_draw_plan(
        &self,
        pane_id: usize,
        since_revision: Option<u64>,
    ) -> Result<RenderDrawPlan> {
        Ok(self
            .read_render_frame(pane_id, since_revision)?
            .to_draw_plan())
    }
    #[allow(dead_code)]
    fn read_render_commit_plan(
        &self,
        pane_id: usize,
        metrics: RenderCellMetrics,
        consumer: &mut RenderConsumerState,
    ) -> Result<RenderCommitPlan> {
        let draw_plan = self.read_render_draw_plan(pane_id, consumer.submitted_revision())?;
        Ok(consumer.prepare_commit(draw_plan.to_geometry_plan(metrics).to_submission_plan()))
    }
    fn read_visible_text(&self, pane_id: usize) -> Result<String>;

    /// A cheap look at a pane a front end is watching but not drawing.
    ///
    /// Provided rather than required: every engine already has
    /// `read_styled_screen`, and this default is correct on all of them. An
    /// engine that pays to *serialize* that screen — the one across an IPC
    /// socket — overrides it and sends the handful of lines instead.
    fn read_pane_pulse(
        &self,
        pane_id: usize,
        since_revision: Option<u64>,
        tail_rows: usize,
    ) -> Result<PanePulse> {
        let snapshot = self.read_styled_screen(pane_id)?;
        Ok(PanePulse::from_snapshot(&snapshot, since_revision, tail_rows))
    }
    fn read_lines(&self, pane_id: usize, start: i64, count: usize) -> Result<Vec<ScreenLine>>;
    fn read_scrollback(&self, pane_id: usize, limit: usize) -> Result<Vec<String>>;
    fn read_scrollback_text(
        &self,
        pane_id: usize,
        request: ScrollbackTextRequest,
    ) -> Result<ScrollbackTextSnapshot>;
    #[allow(dead_code)]
    fn read_styled_scrollback(
        &self,
        pane_id: usize,
        request: ScrollbackTextRequest,
    ) -> Result<StyledScrollbackSnapshot> {
        let text = self.read_scrollback_text(pane_id, request)?;
        let lines = text
            .lines
            .iter()
            .enumerate()
            .map(|(idx, line)| StyledScreenLine {
                row: text.first_row + idx as i64,
                // Plain-text fallback: the source has no wrap state to carry.
                wrapped: false,
                cells: line
                    .chars()
                    .map(|ch| {
                        let mut buf = [0u8; 4];
                        StyledCell {
                            ch,
                            style: CellStyle::default(),
                            width: termwiz::cell::unicode_column_width(
                                ch.encode_utf8(&mut buf),
                                None,
                            ),
                        }
                    })
                    .collect(),
            })
            .collect();
        Ok(StyledScrollbackSnapshot {
            lines,
            first_row: text.first_row,
            row_count: text.row_count,
            cols: text.cols,
            scrollback_top: text.scrollback_top,
            physical_top: text.physical_top,
            viewport_rows: text.viewport_rows,
        })
    }
    fn search(
        &self,
        pane_id: usize,
        pattern: &str,
        mode: SearchMode,
        max_results: usize,
    ) -> Result<Vec<ScreenSearchMatch>>;
    fn cursor(&self, pane_id: usize) -> Result<CursorSnapshot>;
}

pub trait InputEngine {
    fn write_input(&self, pane_id: usize, input: &str) -> Result<()>;

    #[allow(dead_code)]
    fn paste_input(&self, pane_id: usize, text: &str) -> Result<()> {
        self.write_input(pane_id, text)
    }
}

/// What focusing a window told us.
///
/// Plain data on purpose: these travel to whoever asked over MCP, and a type
/// that knew about a window would tie that question to one front end.
#[derive(Clone, Debug)]
pub struct WindowFocusResult {
    pub mux_window_id: usize,
    pub window_engine: &'static str,
    pub uses_host_window: bool,
}

#[derive(Clone, Debug)]
pub struct PaneLocation {
    pub window_id: usize,
    pub tab_id: usize,
}

/// One window a front end is showing, as an agent sees it.
///
/// The id is the front end's own, handed out in order from 1. Deliberately
/// not winit's `WindowId` -- an opaque platform handle that means nothing
/// outside the process holding it -- and no longer the pid, which is what
/// identified a window back when every window was a process. One process now
/// has many, which is the point of the whole design; without a number per
/// window an agent can open one and then has no way to name it again.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WindowSummary {
    pub id: u64,
    /// What the title bar says, which is how a person would refer to it.
    pub title: String,
    /// The window this front end is drawing and sending keys to.
    pub focused: bool,
    pub tabs: usize,
    /// The sessions this window is showing, across all of its tabs.
    ///
    /// A session belongs to one window, and until this was reported nothing
    /// outside the front end could tell which. That is what `session.focus`
    /// needs: asked to show a session, it has to raise the window holding it
    /// -- otherwise the call succeeds, the active session changes, and the
    /// user sees nothing at all because the window they are looking at never
    /// had that session in it.
    #[serde(default)]
    pub panes: Vec<usize>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum ViewportScrollResult {
    Scrolled,
    Unsupported { reason: String },
}

/// Window-level questions a front end answers.
///
/// Not every front end has windows in the same sense -- a headless one has
/// none -- so this is separate from the session and screen traits rather than
/// folded into them.
/// Windows asked for but not yet made, by the id each will have.
///
/// Filled from whichever thread took the request -- an MCP call, a second
/// launch handing over -- and drained by the event loop, which is the only
/// place a window can actually be made. A queue of ids rather than the flag
/// this used to be: two callers asking at once want two windows, and each
/// wants to be told which one is theirs.
/// A window somebody asked for, and what they asked it to open on.
///
/// Carries the directory and profile rather than just the id because the
/// caller that needs them most is a second `unterm start --cwd`: without a
/// way to say where, handing the request to the running front end would open
/// a window on the wrong folder, so it used to start a whole second process
/// instead -- ~200 ms of GPU adapter, and a Core left behind when it quit.
#[derive(Clone, Debug, Default)]
pub struct WindowRequest {
    pub id: u64,
    pub cwd: Option<std::path::PathBuf>,
    pub profile: Option<String>,
    pub command: Vec<String>,
}

static WINDOW_REQUESTS: std::sync::Mutex<Vec<WindowRequest>> = std::sync::Mutex::new(Vec::new());

/// The next window id this process will hand out.
///
/// One allocator, and it lives in the front end. A Core forwards the request
/// and is *told* the id rather than inventing one, because two allocators
/// would collide the first time a person opened a window while an agent
/// opened another -- and the collision would be two windows answering to one
/// number, which is worse than no numbers at all.
static NEXT_WINDOW_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Take the next window id without asking for a window.
///
/// For windows the front end makes on its own account -- the first one, or
/// one a keystroke opened -- which still need an id to be addressed by.
pub fn reserve_window_id() -> u64 {
    NEXT_WINDOW_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Ask the front end for another window; returns the id it will have.
///
/// Reserved here rather than reported once the window exists, because the
/// caller is on an MCP thread and a window can only be made where winit hands
/// out an `ActiveEventLoop`. Reserved is enough to answer with: nothing else
/// will ever be given this number.
pub fn request_window() -> u64 {
    request_window_on(None, None, Vec::new())
}

/// Ask for a window that opens on a given directory, profile or command.
pub fn request_window_on(
    cwd: Option<std::path::PathBuf>,
    profile: Option<String>,
    command: Vec<String>,
) -> u64 {
    let id = reserve_window_id();
    if let Ok(mut queue) = WINDOW_REQUESTS.lock() {
        queue.push(WindowRequest {
            id,
            cwd,
            profile,
            command,
        });
    }
    id
}

/// Whether anyone is still waiting for a window.
///
/// The front end only takes the queue when it has a window to park; with
/// none, taking a request would drop it. Asking first lets it leave the
/// request where it is and go build a window to serve it in.
pub fn window_requests_pending() -> bool {
    WINDOW_REQUESTS
        .lock()
        .map(|queue| !queue.is_empty())
        .unwrap_or(false)
}

/// Every window asked for since this was last called, in the order asked.
pub fn take_window_requests() -> Vec<WindowRequest> {
    WINDOW_REQUESTS
        .lock()
        .map(|mut queue| std::mem::take(&mut *queue))
        .unwrap_or_default()
}

pub trait WindowEngine {
    /// Raise a window: the named one, or whichever is in front.
    fn focus_current_instance_window(
        &self,
        window_id: Option<u64>,
    ) -> anyhow::Result<WindowFocusResult>;

    /// Open another window on this front end; returns the id it will have.
    ///
    /// Exists so a second launch does not have to become a second process.
    /// Starting one costs a GPU adapter -- ~200 ms, paid again every time --
    /// while a window on a front end that already has one costs 31 ms.
    ///
    /// Defaults to refusing: a headless engine has no window to add one
    /// beside, and saying so is better than reporting a window that is not
    /// there.
    fn open_window(&self) -> anyhow::Result<u64> {
        self.open_window_on(None, None, Vec::new())
    }

    /// Open a window that starts on a given directory, profile or command.
    fn open_window_on(
        &self,
        cwd: Option<std::path::PathBuf>,
        profile: Option<String>,
        command: Vec<String>,
    ) -> anyhow::Result<u64> {
        // A Core has no window of its own; the front end attached to it does.
        if let Some(id) = mcp_host().and_then(|host| host.open_window_on(cwd, profile, command)) {
            return Ok(id);
        }
        anyhow::bail!("no front end is attached to open a window on")
    }

    /// Every window this front end is showing.
    ///
    /// Empty rather than an error when there is no front end: "this Core has
    /// no windows" is an answer, and an agent deciding whether to open one
    /// should not have to tell that apart from a failure.
    fn list_windows(&self) -> anyhow::Result<Vec<WindowSummary>> {
        Ok(mcp_host().map(|host| host.list_windows()).unwrap_or_default())
    }
    fn active_pane_id(&self) -> anyhow::Result<Option<u64>>;
    fn pane_locations(&self) -> anyhow::Result<std::collections::HashMap<u64, PaneLocation>>;
    fn scroll_viewport_to(
        &self,
        pane_id: usize,
        target: isize,
    ) -> anyhow::Result<ViewportScrollResult>;
}

pub trait RecordingEngine {
    fn start_recording(&self, pane_id: usize) -> Result<RecordingStartResult>;
    fn stop_recording(&self, pane_id: usize) -> Result<RecordingStopResult>;
    fn recording_status(&self, pane_id: usize) -> Result<RecordingStatusSnapshot>;
    fn attach_recording_trace(&self, pane_id: usize, trace_id: String) -> Result<Vec<String>>;
    fn export_markdown(
        &self,
        pane_id: usize,
        target_path: Option<String>,
    ) -> Result<RecordingExportResult>;
}

pub trait HealthEngine {
    fn health(&self) -> Result<EngineHealthSnapshot>;
}

#[allow(dead_code)]
/// Screenshots, in terms nothing outside this crate has to know about.
///
/// Split from the scrollback renderer below because these two return plain
/// JSON while that one needs this crate's PNG types. Keeping them apart is
/// what lets the MCP handler -- which uses these seven times and that once --
/// live somewhere that does not know about a GUI.
pub trait CaptureEngine {
    fn capture_screen_image(&self, include_base64: bool) -> anyhow::Result<serde_json::Value>;
    fn capture_window_image(
        &self,
        title_filter: Option<&str>,
        pid_filter: Option<u32>,
        include_base64: bool,
    ) -> anyhow::Result<serde_json::Value>;

    /// A rectangle of the desktop, in physical pixels.
    ///
    /// The interactive version of this is a person dragging a box. An agent
    /// has no way to drag, so it passes the rectangle instead -- which is the
    /// same picture by a route an agent can take.
    fn capture_region_image(
        &self,
        _left: i32,
        _top: i32,
        _width: usize,
        _height: usize,
        _include_base64: bool,
    ) -> anyhow::Result<serde_json::Value> {
        anyhow::bail!("capturing a region is not supported here")
    }
}

/// Everything a front end has to answer for a full MCP surface.
///
/// Aggregated so the handler can hold one thing rather than a tuple of
/// traits, and so a front end that grows a new capability implements it in
/// one place.
/// `Send + Sync` because an MCP request is answered off the main thread, and
/// the engine goes with it.
pub trait HostEngine: TerminalEngine + WindowEngine + CaptureEngine + Send + Sync {
    /// What to call this engine in anything a user or an agent reads.
    fn name(&self) -> &'static str;
}

impl HostEngine for next_core::NextCoreEngine {
    fn name(&self) -> &'static str {
        "next-core"
    }
}

/// next-core answers window questions by saying it has no window.
///
/// The engine genuinely does not have one -- a front end does. Implementing
/// these here rather than leaving them unimplemented means an MCP surface can
/// serve terminal work before, or without, any window existing, and a front
/// end that does have one installs a provider that answers properly.
impl WindowEngine for next_core::NextCoreEngine {
    /// The kernel has no window; whatever is hosting it does.
    fn focus_current_instance_window(&self, window_id: Option<u64>) -> Result<WindowFocusResult> {
        let Some(host) = mcp_host() else {
            anyhow::bail!("no front end is hosting a window to focus");
        };
        let raised = host.focus_window(window_id)?;
        let identity = host.window_identity();
        Ok(WindowFocusResult {
            // Which window was actually raised. This reported 0 while a
            // window was a process and `instance.list` named it by pid; one
            // process now has many, so the number has to come from the front
            // end that handed it out.
            mux_window_id: raised as usize,
            window_engine: identity.engine,
            uses_host_window: identity.uses_host_window,
        })
    }
    fn active_pane_id(&self) -> Result<Option<u64>> {
        Ok(self
            .list_sessions()?
            .into_iter()
            .find(|session| session.is_active)
            .map(|session| session.id as u64))
    }
    fn pane_locations(&self) -> Result<std::collections::HashMap<u64, PaneLocation>> {
        // One window, and a tab per session. next-core does not group sessions
        // into windows -- a front end does that -- so every session reports
        // window 0 and its own id as its tab.
        Ok(self
            .list_sessions()?
            .into_iter()
            .map(|session| {
                (
                    session.id as u64,
                    PaneLocation {
                        window_id: 0,
                        tab_id: session.id,
                    },
                )
            })
            .collect())
    }
    fn scroll_viewport_to(&self, pane_id: usize, target: isize) -> Result<ViewportScrollResult> {
        next_core::NextCoreEngine::scroll_viewport_to(self, pane_id, target)?;
        Ok(ViewportScrollResult::Scrolled)
    }
}

/// next-core has no window, but whatever is hosting it does.
///
/// The kernel draws into whatever surface a front end gives it and has no idea
/// what that surface is on, so the picture has to come from the front end.
/// Routing it through the host is what makes `capture.window` -- and the
/// `selftest.run` check that watches it -- work again.
impl CaptureEngine for next_core::NextCoreEngine {
    fn capture_screen_image(&self, include_base64: bool) -> Result<serde_json::Value> {
        // No pid: "our own window" has to be resolved by whoever owns
        // one. Naming this process worked only while the surface and the
        // window were the same process -- with the surface in a Core,
        // that pid owns no window and the search finds nothing.
        host_capture(None, None, include_base64)
    }
    fn capture_region_image(
        &self,
        left: i32,
        top: i32,
        width: usize,
        height: usize,
        include_base64: bool,
    ) -> Result<serde_json::Value> {
        match mcp_host() {
            Some(host) => host.capture_region(left, top, width, height, include_base64),
            None => anyhow::bail!("no front end is hosting a screen to capture"),
        }
    }

    fn capture_window_image(
        &self,
        title_filter: Option<&str>,
        pid_filter: Option<u32>,
        include_base64: bool,
    ) -> Result<serde_json::Value> {
        // A bare `capture.window` means "the terminal", which is what it
        // is for -- but which process that is belongs to the host, not
        // to whichever process happens to be running this engine.
        host_capture(title_filter, pid_filter, include_base64)
    }
}

fn host_capture(
    title: Option<&str>,
    pid: Option<u32>,
    include_base64: bool,
) -> Result<serde_json::Value> {
    match mcp_host() {
        Some(host) => host.capture_own_window(title, pid, include_base64),
        None => anyhow::bail!("no front end is hosting a window to capture"),
    }
}

/// What only a front end can answer.
///
/// Three things resist being moved: rendering a pane's scrollback to a PNG
/// (built on one front end's font stack), capturing another application's
/// window (an OS API, and macOS-only), and the key table (the front end owns
/// what its keys do). Rather than let those keep an entire MCP surface tied to
/// one front end, they are asked of whoever is hosting it.
/// How a front end's window relates to the MCP surface.
///
/// The surface reports these to agents so a caller can tell what it is
/// talking to: a window someone else owns and this process only decorates,
/// or one this process owns outright. Baking the answer in as a constant made
/// every front end claim to be the same one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowIdentity {
    /// What draws the window, named for an agent to read.
    pub engine: &'static str,
    /// Who the window belongs to.
    pub window_owner: &'static str,
    /// Who decides when it closes.
    pub native_window_lifecycle: &'static str,
    /// True when this process draws into a window it did not create.
    pub uses_host_window: bool,
}

impl WindowIdentity {
    /// What a surface with no front end reports.
    pub const HEADLESS: Self = Self {
        engine: "next-core",
        window_owner: "none",
        native_window_lifecycle: "none",
        uses_host_window: false,
    };
}

pub trait McpHost: Send + Sync {
    /// How this front end's window relates to the surface.
    fn window_identity(&self) -> WindowIdentity {
        WindowIdentity::HEADLESS
    }

    /// Ask the attached front end for another window; the id it will have.
    ///
    /// The Core has no windows of its own -- it forwards to whichever front
    /// end is attached, which is the whole point of this trait. `None` when
    /// nothing is attached to ask, so the caller can fall back to starting a
    /// front end rather than reporting a window that was never opened.
    /// Open a window, optionally on a given directory, profile or command.
    ///
    /// One method rather than a no-argument one delegating to this: with
    /// both, a front end that overrode only the short form kept compiling
    /// and quietly stopped opening windows the moment anything asked for a
    /// directory.
    fn open_window_on(
        &self,
        _cwd: Option<std::path::PathBuf>,
        _profile: Option<String>,
        _command: Vec<String>,
    ) -> Option<u64> {
        None
    }

    /// Every window the attached front end is showing.
    fn list_windows(&self) -> Vec<WindowSummary> {
        Vec::new()
    }

    /// Whether there is a front end that can actually put a question on
    /// a screen *right now*.
    ///
    /// Distinct from "an `McpHost` is installed". A Core installs one
    /// unconditionally and then forwards to whichever window is
    /// attached; between windows it is installed and cannot ask anyone
    /// anything. The write-confirmation gate turns on this: with no one
    /// to ask it must refuse at once, and parking a worker for the full
    /// confirmation timeout instead is the failure this exists to name.
    fn can_prompt(&self) -> bool {
        true
    }

    /// Put a write-confirmation question in front of the person at the
    /// window and return their answer as one of `allow` / `block` /
    /// `always_allow`.
    ///
    /// Raw JSON rather than typed, because the types belong to the MCP
    /// surface and this trait sits underneath it. Blocking is the whole
    /// point: the caller is a worker thread that must not write to a pty
    /// until a person has said so.
    fn ask_confirmation(&self, _request: &serde_json::Value) -> Result<serde_json::Value> {
        anyhow::bail!("no front end can ask for confirmation")
    }

    /// Render a pane's scrollback to `path`, returning the JSON to reply with.
    fn render_scrollback_png(
        &self,
        pane_id: Option<usize>,
        path: &std::path::Path,
        max_rows: usize,
        dpi: usize,
    ) -> Result<serde_json::Value>;

    /// Capture another application's window. Only macOS has this today.
    fn capture_external_window(&self, _request: &serde_json::Value) -> Result<serde_json::Value> {
        anyhow::bail!("capturing other applications' windows is not supported here")
    }

    /// Put a title on the front end's own window, or take it off again.
    ///
    /// Returns whether it was applied, so a caller can say plainly rather
    /// than claim something it did not do.
    fn set_window_title(&self, _title: Option<&str>) -> bool {
        false
    }

    /// Bring one of this front end's windows to the front.
    ///
    /// An agent that has just written something worth looking at needs a way
    /// to say so; `instance.focus` is it, and without a front end to ask,
    /// there is no window to raise. `None` means whichever window is in
    /// front already -- the old behaviour, and still the right default for a
    /// caller that only has one.
    ///
    /// Returns the id actually raised, so a caller that passed `None` learns
    /// which window its message landed in front of.
    fn focus_window(&self, _window_id: Option<u64>) -> Result<u64> {
        anyhow::bail!("this front end has no window to focus")
    }

    /// Ask the front end to paint a frame soon.
    ///
    /// For state that changes off the window's own threads -- a parked agent
    /// write waiting on its banner, say. An idle window repaints on events;
    /// without this, the question would sit invisible until one happened by.
    fn request_repaint(&self) {}

    /// Photograph a rectangle of the desktop.
    fn capture_region(
        &self,
        _left: i32,
        _top: i32,
        _width: usize,
        _height: usize,
        _include_base64: bool,
    ) -> Result<serde_json::Value> {
        anyhow::bail!("this front end cannot capture the screen")
    }

    /// Photograph this front end's own window, returning the JSON to reply
    /// with. An agent can read the screen as text without this; what it
    /// cannot do is see what a person sees.
    fn capture_own_window(
        &self,
        _title: Option<&str>,
        _pid: Option<u32>,
        _include_base64: bool,
    ) -> Result<serde_json::Value> {
        anyhow::bail!("this front end has no window to capture")
    }

    /// The key assignments this front end has, for the tool catalogue.
    fn key_assignments(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }
}

static MCP_HOST: std::sync::OnceLock<&'static dyn McpHost> = std::sync::OnceLock::new();

/// Install the host once, at startup.
pub fn set_mcp_host(host: &'static dyn McpHost) -> bool {
    MCP_HOST.set(host).is_ok()
}

/// The host, if a front end installed one.
pub fn mcp_host() -> Option<&'static dyn McpHost> {
    MCP_HOST.get().copied()
}

/// How the MCP surface finds the engine to talk to.
///
/// A slot rather than a call into the front end: the handler has no business
/// knowing which front ends exist, and the two that do exist choose
/// differently -- one has an environment switch between engines, the other
/// has only next-core.
static ENGINE_PROVIDER: std::sync::OnceLock<fn() -> Box<dyn HostEngine>> =
    std::sync::OnceLock::new();

/// Name the engine so the provider can be installed once at startup.
///
/// Returns false if one is already installed, which is not an error: two
/// front ends in one process would be a stranger problem than this.
pub fn set_engine_provider(provider: fn() -> Box<dyn HostEngine>) -> bool {
    ENGINE_PROVIDER.set(provider).is_ok()
}

/// How the hosting front end's window relates to the surface.
///
/// Headless when nothing is hosting: an honest "no window" rather than a
/// guess at which front end might be there.
pub fn window_identity() -> WindowIdentity {
    mcp_host()
        .map(|host| host.window_identity())
        .unwrap_or(WindowIdentity::HEADLESS)
}

/// Announce what terminal a spawned program is talking to — `TERM` and
/// `COLORTERM`, each only if not already set.
///
/// Every path that spawns a shell has to call this, and there is more
/// than one: sessions born in the front end go through
/// `next_core::launch::prepare_command`, while those born of an MCP
/// request are built in `unterm-mcp`. That second path used to set
/// `TERM` by hand and knew nothing of `COLORTERM`, so a pane an agent
/// created announced itself differently from one the user opened —
/// invisible on macOS, where the value was usually inherited anyway,
/// and plain in a Linux VM where nothing supplies it. One definition,
/// so the two cannot drift again.
pub fn apply_terminal_identity(command: &mut portable_pty::CommandBuilder) {
    next_core::launch::ensure_term_env(command);
}

/// The engine the front end installed, if it has.
pub fn engine_provider() -> Option<fn() -> Box<dyn HostEngine>> {
    ENGINE_PROVIDER.get().copied()
}

/// The installed engine, or next-core when nothing was installed.
///
/// Background threads and the MCP surface must go through this rather
/// than constructing `NextCoreEngine` directly: with sessions living
/// in a Core process, the process-local engine is empty and a direct
/// construction silently reads the wrong world.
pub fn host_engine() -> Box<dyn HostEngine> {
    match engine_provider() {
        Some(provider) => provider(),
        None => Box::new(next_core()),
    }
}

/// Install next-core itself as the engine, for a process with no front end.
///
/// The MCP surface's own tests need an engine but no window, and next-core is
/// exactly that: it answers every session, screen, input and recording
/// question, and says plainly that it has no window of its own. Idempotent,
/// so every test can call it.
pub fn install_next_core_provider() {
    set_engine_provider(|| Box::new(next_core()));
}

pub trait TerminalEngine:
    SessionEngine + ScreenEngine + InputEngine + RecordingEngine + HealthEngine
{
}

impl<T> TerminalEngine for T where
    T: SessionEngine + ScreenEngine + InputEngine + RecordingEngine + HealthEngine
{
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeScreenEngine;

    impl ScreenEngine for FakeScreenEngine {
        fn read_screen(&self, _pane_id: usize) -> Result<ScreenSnapshot> {
            unimplemented!("not needed by draw-plan fallback test")
        }

        fn read_styled_screen(&self, _pane_id: usize) -> Result<StyledScreenSnapshot> {
            Ok(StyledScreenSnapshot {
                mouse: Default::default(),
                bells: 0,
                notifications: 0,
                last_notification: None,
                focus_reporting: false,
                clipboard_request: None,
                lines: vec![StyledScreenLine {
                    row: 0,
                    wrapped: false,
                    cells: vec![cell('x', CellStyle::default(), 1)],
                }],
                cursor: CursorSnapshot {
                    x: 0,
                    y: 0,
                    visible: true,
                    shape: "block".to_string(),
                },
                cols: 1,
                rows: 1,
                scrollback_rows: 0,
                revision: 11,
                dirty_rows: Some(DirtyRows { start: 0, end: 0 }),
            })
        }

        fn read_visible_text(&self, _pane_id: usize) -> Result<String> {
            Ok("x".to_string())
        }

        fn read_lines(
            &self,
            _pane_id: usize,
            _start: i64,
            _count: usize,
        ) -> Result<Vec<ScreenLine>> {
            Ok(Vec::new())
        }

        fn read_scrollback(&self, _pane_id: usize, _limit: usize) -> Result<Vec<String>> {
            Ok(Vec::new())
        }

        fn read_scrollback_text(
            &self,
            _pane_id: usize,
            _request: ScrollbackTextRequest,
        ) -> Result<ScrollbackTextSnapshot> {
            unimplemented!("not needed by draw-plan fallback test")
        }

        fn search(
            &self,
            _pane_id: usize,
            _pattern: &str,
            _mode: SearchMode,
            _max_results: usize,
        ) -> Result<Vec<ScreenSearchMatch>> {
            Ok(Vec::new())
        }

        fn cursor(&self, _pane_id: usize) -> Result<CursorSnapshot> {
            Ok(CursorSnapshot {
                x: 0,
                y: 0,
                visible: true,
                shape: "block".to_string(),
            })
        }
    }

    fn cell(ch: char, style: CellStyle, width: usize) -> StyledCell {
        StyledCell { ch, style, width }
    }

    fn frame(cells: Vec<StyledCell>) -> RenderFrameSnapshot {
        RenderFrameSnapshot {
            lines: vec![StyledScreenLine {
                row: 0,
                wrapped: false,
                cells,
            }],
            cursor: CursorSnapshot {
                x: 2,
                y: 0,
                visible: true,
                shape: "block".to_string(),
            },
            cols: 6,
            rows: 1,
            scrollback_rows: 0,
            revision: 7,
            dirty_rows: Some(DirtyRows { start: 0, end: 0 }),
            full: false,
        }
    }

    #[test]
    fn render_draw_plan_merges_glyph_and_cell_runs() {
        let red = CellStyle {
            fg: Some(StyledColor::Palette(1)),
            ..CellStyle::default()
        };
        let blue = CellStyle {
            fg: Some(StyledColor::Palette(4)),
            ..CellStyle::default()
        };
        let plan = frame(vec![
            cell('a', red.clone(), 1),
            cell('b', red.clone(), 1),
            cell(' ', red.clone(), 1),
            cell('c', blue.clone(), 1),
            cell('d', blue.clone(), 1),
            cell(' ', CellStyle::default(), 1),
        ])
        .to_draw_plan();

        assert_eq!(plan.revision, 7);
        assert_eq!(plan.dirty_rows, Some(DirtyRows { start: 0, end: 0 }));
        assert_eq!(plan.glyph_runs.len(), 2);
        assert_eq!(plan.glyph_runs[0].text, "ab");
        assert_eq!(plan.glyph_runs[0].col, 0);
        assert_eq!(plan.glyph_runs[0].cells, 2);
        assert_eq!(plan.glyph_runs[1].text, "cd");
        assert_eq!(plan.glyph_runs[1].col, 3);
        assert_eq!(plan.glyph_runs[1].style, blue);
        assert_eq!(plan.cell_runs.len(), 3);
        assert_eq!(plan.cell_runs[0].cells, 3);
        assert_eq!(plan.cell_runs[1].col, 3);
        assert_eq!(plan.cell_runs[1].cells, 2);
        assert_eq!(
            plan.cursor,
            Some(RenderCursorDraw {
                row: 0,
                col: 2,
                visible: true,
                shape: "block".to_string()
            })
        );
    }

    #[test]
    fn screen_engine_default_reads_render_draw_plan() {
        let engine = FakeScreenEngine;
        let plan = engine.read_render_draw_plan(1, None).unwrap();
        assert!(plan.full);
        assert_eq!(plan.revision, 11);
        assert_eq!(plan.glyph_runs.len(), 1);
        assert_eq!(plan.glyph_runs[0].text, "x");

        let unchanged = engine.read_render_draw_plan(1, Some(11)).unwrap();
        assert!(!unchanged.full);
        assert!(unchanged.glyph_runs.is_empty());
        assert_eq!(
            unchanged.cursor,
            Some(RenderCursorDraw {
                row: 0,
                col: 0,
                visible: true,
                shape: "block".to_string()
            })
        );
    }

    #[test]
    fn screen_engine_default_reads_render_commit_plan() {
        let engine = FakeScreenEngine;
        let mut consumer = RenderConsumerState::new();
        let metrics = RenderCellMetrics {
            cell_width_px: 8,
            cell_height_px: 16,
        };

        let first = engine
            .read_render_commit_plan(1, metrics, &mut consumer)
            .unwrap();
        assert!(first.submit);
        assert_eq!(first.revision, 11);
        assert!(first.requires_full_repaint);
        assert_eq!(first.submission.unwrap().damage_rects.len(), 1);

        let repeat = engine
            .read_render_commit_plan(1, metrics, &mut consumer)
            .unwrap();
        assert!(!repeat.submit);
        assert_eq!(repeat.previous_revision, Some(11));
        assert!(repeat.submission.is_none());
    }

    #[test]
    fn render_geometry_plan_maps_runs_to_pixel_rects() {
        let plan = frame(vec![
            cell('a', CellStyle::default(), 1),
            cell('b', CellStyle::default(), 1),
            cell(' ', CellStyle::default(), 1),
        ])
        .to_draw_plan()
        .to_geometry_plan(RenderCellMetrics {
            cell_width_px: 9,
            cell_height_px: 17,
        });

        assert_eq!(
            plan.viewport,
            RenderRect {
                x: 0,
                y: 0,
                width: 54,
                height: 17,
            }
        );
        assert_eq!(plan.glyph_runs.len(), 1);
        assert_eq!(plan.glyph_runs[0].cells, 2);
        assert_eq!(
            plan.glyph_runs[0].rect,
            RenderRect {
                x: 0,
                y: 0,
                width: 18,
                height: 17,
            }
        );
        assert_eq!(plan.cell_runs.len(), 1);
        assert_eq!(
            plan.cell_runs[0].rect,
            RenderRect {
                x: 0,
                y: 0,
                width: 27,
                height: 17,
            }
        );
        assert_eq!(
            plan.cursor.unwrap().rect,
            RenderRect {
                x: 18,
                y: 0,
                width: 9,
                height: 17,
            }
        );
    }

    #[test]
    fn render_geometry_plan_preserves_dirty_row_pixels() {
        let dirty = RenderFrameSnapshot {
            lines: vec![StyledScreenLine {
                row: 42,
                wrapped: false,
                cells: vec![cell('z', CellStyle::default(), 1)],
            }],
            cursor: CursorSnapshot {
                x: 1,
                y: 5,
                visible: true,
                shape: "bar".to_string(),
            },
            cols: 4,
            rows: 8,
            scrollback_rows: 12,
            revision: 9,
            dirty_rows: Some(DirtyRows { start: 5, end: 5 }),
            full: false,
        };

        let plan = dirty.to_draw_plan().to_geometry_plan(RenderCellMetrics {
            cell_width_px: 8,
            cell_height_px: 16,
        });
        assert_eq!(plan.glyph_runs[0].row, 5);
        assert_eq!(
            plan.glyph_runs[0].rect,
            RenderRect {
                x: 0,
                y: 80,
                width: 8,
                height: 16,
            }
        );
        assert_eq!(
            plan.cursor.unwrap().rect,
            RenderRect {
                x: 8,
                y: 80,
                width: 8,
                height: 16,
            }
        );
    }

    #[test]
    fn render_submission_plan_maps_geometry_to_renderer_commands() {
        let fg = CellStyle {
            fg: Some(StyledColor::Palette(2)),
            ..CellStyle::default()
        };
        let bg = CellStyle {
            bg: Some(StyledColor::Palette(4)),
            ..CellStyle::default()
        };
        let geometry = frame(vec![
            cell('o', fg.clone(), 1),
            cell('k', fg.clone(), 1),
            cell(' ', bg.clone(), 1),
        ])
        .to_draw_plan()
        .to_geometry_plan(RenderCellMetrics {
            cell_width_px: 10,
            cell_height_px: 20,
        });

        let submission = geometry.to_submission_plan();
        assert_eq!(submission.revision, 7);
        assert!(!submission.full);
        assert_eq!(
            submission.damage_rects,
            vec![RenderRect {
                x: 0,
                y: 0,
                width: 60,
                height: 20,
            }]
        );
        assert_eq!(submission.text_runs.len(), 1);
        assert_eq!(submission.text_runs[0].text, "ok");
        assert_eq!(submission.text_runs[0].row, 0);
        assert_eq!(submission.text_runs[0].col, 0);
        assert_eq!(submission.text_runs[0].cells, 2);
        assert_eq!(submission.text_runs[0].style, fg);
        assert_eq!(submission.background_quads.len(), 2);
        assert_eq!(submission.background_quads[1].style, bg);
        assert_eq!(
            submission.cursor,
            Some(RenderCursorQuad {
                rect: RenderRect {
                    x: 20,
                    y: 0,
                    width: 10,
                    height: 20,
                },
                visible: true,
                shape: "block".to_string(),
            })
        );
    }

    #[test]
    fn render_submission_plan_uses_full_viewport_damage_for_full_frames() {
        let mut full = frame(vec![cell('x', CellStyle::default(), 1)]);
        full.full = true;
        full.dirty_rows = None;
        full.rows = 2;

        let submission = full
            .to_draw_plan()
            .to_geometry_plan(RenderCellMetrics {
                cell_width_px: 8,
                cell_height_px: 16,
            })
            .to_submission_plan();

        assert!(submission.full);
        assert_eq!(
            submission.damage_rects,
            vec![RenderRect {
                x: 0,
                y: 0,
                width: 48,
                height: 32,
            }]
        );
    }

    #[test]
    fn render_submission_plan_uses_dirty_row_damage_for_partial_frames() {
        let dirty = RenderGeometryPlan {
            revision: 10,
            cols: 5,
            rows: 4,
            scrollback_rows: 0,
            dirty_rows: Some(DirtyRows { start: 1, end: 2 }),
            full: false,
            viewport: RenderRect {
                x: 0,
                y: 0,
                width: 50,
                height: 80,
            },
            glyph_runs: Vec::new(),
            cell_runs: Vec::new(),
            cursor: None,
        };

        assert_eq!(
            dirty.to_submission_plan().damage_rects,
            vec![RenderRect {
                x: 0,
                y: 20,
                width: 50,
                height: 40,
            }]
        );
    }

    #[test]
    fn render_consumer_state_forces_first_commit_to_full_damage() {
        let submission = frame(vec![cell('x', CellStyle::default(), 1)])
            .to_draw_plan()
            .to_geometry_plan(RenderCellMetrics {
                cell_width_px: 8,
                cell_height_px: 16,
            })
            .to_submission_plan();
        let mut state = RenderConsumerState::new();

        let commit = state.prepare_commit(submission);

        assert!(commit.submit);
        assert_eq!(commit.previous_revision, None);
        assert!(commit.requires_full_repaint);
        assert_eq!(commit.skipped_revisions, 0);
        assert_eq!(
            commit.submission.unwrap().damage_rects,
            vec![RenderRect {
                x: 0,
                y: 0,
                width: 48,
                height: 16,
            }]
        );
        assert_eq!(state.submitted_revision(), Some(7));
    }

    #[test]
    fn render_consumer_state_skips_repeated_revision() {
        let submission = frame(vec![cell('x', CellStyle::default(), 1)])
            .to_draw_plan()
            .to_geometry_plan(RenderCellMetrics {
                cell_width_px: 8,
                cell_height_px: 16,
            })
            .to_submission_plan();
        let mut state = RenderConsumerState::new();
        assert!(state.prepare_commit(submission.clone()).submit);

        let repeat = state.prepare_commit(submission);

        assert!(!repeat.submit);
        assert_eq!(repeat.previous_revision, Some(7));
        assert_eq!(repeat.revision, 7);
        assert!(repeat.submission.is_none());
    }

    #[test]
    fn render_consumer_state_preserves_dirty_damage_for_incremental_commit() {
        let mut state = RenderConsumerState::new();
        let mut first = frame(vec![cell('x', CellStyle::default(), 1)]);
        first.full = true;
        first.rows = 3;
        state.prepare_commit(
            first
                .to_draw_plan()
                .to_geometry_plan(RenderCellMetrics {
                    cell_width_px: 8,
                    cell_height_px: 16,
                })
                .to_submission_plan(),
        );

        let dirty = RenderGeometryPlan {
            revision: 9,
            cols: 6,
            rows: 3,
            scrollback_rows: 0,
            dirty_rows: Some(DirtyRows { start: 2, end: 2 }),
            full: false,
            viewport: RenderRect {
                x: 0,
                y: 0,
                width: 48,
                height: 48,
            },
            glyph_runs: Vec::new(),
            cell_runs: Vec::new(),
            cursor: None,
        }
        .to_submission_plan();

        let commit = state.prepare_commit(dirty);

        assert!(commit.submit);
        assert!(!commit.requires_full_repaint);
        assert_eq!(commit.previous_revision, Some(7));
        assert_eq!(commit.skipped_revisions, 1);
        assert_eq!(
            commit.submission.unwrap().damage_rects,
            vec![RenderRect {
                x: 0,
                y: 32,
                width: 48,
                height: 16,
            }]
        );
    }

    #[test]
    fn render_consumer_state_forces_full_damage_when_viewport_changes() {
        let mut state = RenderConsumerState::new();
        state.prepare_commit(
            frame(vec![cell('x', CellStyle::default(), 1)])
                .to_draw_plan()
                .to_geometry_plan(RenderCellMetrics {
                    cell_width_px: 8,
                    cell_height_px: 16,
                })
                .to_submission_plan(),
        );
        let resized = RenderGeometryPlan {
            revision: 8,
            cols: 8,
            rows: 2,
            scrollback_rows: 0,
            dirty_rows: Some(DirtyRows { start: 1, end: 1 }),
            full: false,
            viewport: RenderRect {
                x: 0,
                y: 0,
                width: 64,
                height: 32,
            },
            glyph_runs: Vec::new(),
            cell_runs: Vec::new(),
            cursor: None,
        }
        .to_submission_plan();

        let commit = state.prepare_commit(resized);

        assert!(commit.requires_full_repaint);
        assert_eq!(
            commit.submission.unwrap().damage_rects,
            vec![RenderRect {
                x: 0,
                y: 0,
                width: 64,
                height: 32,
            }]
        );
    }

    #[test]
    fn render_draw_plan_skips_hidden_and_wide_continuation_glyphs() {
        let hidden = CellStyle {
            hidden: true,
            ..CellStyle::default()
        };
        let plan = frame(vec![
            cell('你', CellStyle::default(), 2),
            cell(' ', CellStyle::default(), 0),
            cell('x', hidden, 1),
            cell('y', CellStyle::default(), 1),
        ])
        .to_draw_plan();

        assert_eq!(plan.glyph_runs.len(), 2);
        assert_eq!(plan.glyph_runs[0].text, "你");
        assert_eq!(plan.glyph_runs[0].cells, 2);
        assert_eq!(plan.glyph_runs[1].text, "y");
        assert_eq!(plan.glyph_runs[1].col, 3);
    }

    #[test]
    fn render_draw_plan_preserves_dirty_frame_viewport_rows() {
        let dirty = RenderFrameSnapshot {
            lines: vec![
                StyledScreenLine {
                    row: 10,
                    wrapped: false,
                    cells: vec![cell('a', CellStyle::default(), 1)],
                },
                StyledScreenLine {
                    row: 11,
                    wrapped: false,
                    cells: vec![cell('b', CellStyle::default(), 1)],
                },
            ],
            cursor: CursorSnapshot {
                x: 0,
                y: 4,
                visible: true,
                shape: "block".to_string(),
            },
            cols: 4,
            rows: 8,
            scrollback_rows: 20,
            revision: 8,
            dirty_rows: Some(DirtyRows { start: 3, end: 4 }),
            full: false,
        };

        let plan = dirty.to_draw_plan();
        assert_eq!(plan.glyph_runs.len(), 2);
        assert_eq!(plan.glyph_runs[0].row, 3);
        assert_eq!(plan.glyph_runs[1].row, 4);
        assert_eq!(plan.cell_runs.len(), 2);
        assert_eq!(plan.cell_runs[0].row, 3);
        assert_eq!(plan.cell_runs[1].row, 4);
        assert_eq!(
            plan.cursor,
            Some(RenderCursorDraw {
                row: 4,
                col: 0,
                visible: true,
                shape: "block".to_string()
            })
        );
    }

    #[test]
    fn render_draw_plan_drops_out_of_bounds_cursor() {
        let mut frame = frame(vec![cell('x', CellStyle::default(), 1)]);
        frame.cursor.y = -1;
        assert_eq!(frame.to_draw_plan().cursor, None);

        frame.cursor.y = 0;
        frame.cursor.x = 99;
        assert_eq!(frame.to_draw_plan().cursor, None);
    }
}

#[cfg(test)]
mod host_capture_tests {
    use super::*;

    /// A stand-in front end that records what it was asked for.
    struct Recorder;

    static ASKED: std::sync::Mutex<Vec<(Option<String>, Option<u32>, bool)>> =
        std::sync::Mutex::new(Vec::new());
    static FOCUSED: std::sync::Mutex<Vec<Option<u64>>> = std::sync::Mutex::new(Vec::new());

    impl McpHost for Recorder {
        fn render_scrollback_png(
            &self,
            _pane_id: Option<usize>,
            _path: &std::path::Path,
            _max_rows: usize,
            _dpi: usize,
        ) -> Result<serde_json::Value> {
            anyhow::bail!("not what this test is about")
        }

        fn window_identity(&self) -> WindowIdentity {
            WindowIdentity {
                engine: "recorder",
                window_owner: "recorder",
                native_window_lifecycle: "self_owned",
                uses_host_window: false,
            }
        }

        fn focus_window(&self, window_id: Option<u64>) -> Result<u64> {
            FOCUSED.lock().unwrap().push(window_id);
            // A front end asked for "whatever is in front" still has to say
            // which one that was.
            Ok(window_id.unwrap_or(7))
        }

        fn list_windows(&self) -> Vec<WindowSummary> {
            vec![WindowSummary {
                id: 7,
                title: "recorded".to_string(),
                focused: true,
                tabs: 2,
                panes: vec![1, 2],
            }]
        }

        fn open_window_on(
            &self,
            _cwd: Option<std::path::PathBuf>,
            _profile: Option<String>,
            _command: Vec<String>,
        ) -> Option<u64> {
            Some(8)
        }

        fn capture_own_window(
            &self,
            title: Option<&str>,
            pid: Option<u32>,
            include_base64: bool,
        ) -> Result<serde_json::Value> {
            ASKED
                .lock()
                .unwrap()
                .push((title.map(str::to_string), pid, include_base64));
            Ok(serde_json::json!({ "path": "recorded.png" }))
        }
    }

    /// One test, because the host is installed once for the process. Splitting
    /// it would mean whichever ran second found a host already there.
    #[test]
    fn capture_reaches_the_front_end_that_owns_the_window() {
        assert!(
            set_mcp_host(&Recorder),
            "no other test may install a host first"
        );
        let engine = next_core::NextCoreEngine::default();

        // With nothing named, a capture means this terminal -- and the
        // host is left to work out which process that is. It used to be
        // named here as `std::process::id()`, which was right only while
        // the surface and the window shared a process: with the surface
        // in a Core, that pid owns no window and the search matches
        // nothing at all.
        CaptureEngine::capture_window_image(&engine, None, None, false).unwrap();
        let asked = ASKED.lock().unwrap().pop().unwrap();
        assert_eq!(asked, (None, None, false));

        // A title names somebody else's window, so our pid must not be forced
        // in alongside it -- the two together match nothing.
        CaptureEngine::capture_window_image(&engine, Some("Notepad"), None, true).unwrap();
        let asked = ASKED.lock().unwrap().pop().unwrap();
        assert_eq!(asked, (Some("Notepad".to_string()), None, true));

        // An explicit pid wins over the default either way.
        CaptureEngine::capture_window_image(&engine, None, Some(4242), false).unwrap();
        let asked = ASKED.lock().unwrap().pop().unwrap();
        assert_eq!(asked, (None, Some(4242), false));

        // `capture.screen` is this terminal, always -- and again the
        // host decides which process that is.
        CaptureEngine::capture_screen_image(&engine, true).unwrap();
        let asked = ASKED.lock().unwrap().pop().unwrap();
        assert_eq!(asked, (None, None, true));

        // Focus reaches the same place, and reports the front end's own
        // identity rather than a guess at which one is running.
        let focus = WindowEngine::focus_current_instance_window(&engine, None).unwrap();
        assert_eq!(*FOCUSED.lock().unwrap(), vec![None]);
        assert_eq!(focus.window_engine, "recorder");
        assert!(!focus.uses_host_window);
        // Nothing named: the front end says which window it raised, so the
        // caller learns where its message landed. This was hard-coded 0 back
        // when a window was a process.
        assert_eq!(focus.mux_window_id, 7);

        // A named window is passed through rather than quietly ignored --
        // the failure this replaces answered every id with the same window.
        let focus = WindowEngine::focus_current_instance_window(&engine, Some(3)).unwrap();
        assert_eq!(*FOCUSED.lock().unwrap(), vec![None, Some(3)]);
        assert_eq!(focus.mux_window_id, 3);

        // And the two calls an agent needs to address a window at all.
        let windows = WindowEngine::list_windows(&engine).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].id, 7);
        assert_eq!(windows[0].tabs, 2);
        assert!(windows[0].focused);
        assert_eq!(WindowEngine::open_window(&engine).unwrap(), 8);
    }

    /// The window queue is one process-wide list, and the tests below each
    /// drain it and count what is left. Run side by side -- a plain
    /// `cargo test` does -- one test's request lands in another's count.
    static WINDOW_QUEUE_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Ids are handed out once, in order, whoever asks.
    ///
    /// The reservation happens on the asking thread because a window can only
    /// be made on the event loop, and an MCP caller cannot wait for one. Two
    /// callers racing must still get two numbers.
    #[test]
    fn window_ids_are_never_handed_out_twice() {
        let _queue = WINDOW_QUEUE_TEST.lock().unwrap_or_else(|p| p.into_inner());
        let _ = take_window_requests();
        let first = request_window();
        let second = reserve_window_id();
        let third = request_window();
        assert_ne!(first, second);
        assert_ne!(second, third);
        assert_ne!(first, third);

        // Only the *requested* ones are queued: `reserve_window_id` is for a
        // window the front end is already making for itself.
        let queued: Vec<u64> = take_window_requests()
            .into_iter()
            .map(|request| request.id)
            .collect();
        assert_eq!(queued, vec![first, third]);
        assert!(
            take_window_requests().is_empty(),
            "draining twice must not repeat a window"
        );
    }

    /// What the window was asked to open on has to survive the queue.
    ///
    /// It did not used to: the queue held bare ids, so `unterm start --cwd`
    /// could not hand its directory to the running front end and started a
    /// whole second process rather than open a window on the wrong folder.
    #[test]
    fn a_window_request_carries_where_it_should_open() {
        let _queue = WINDOW_QUEUE_TEST.lock().unwrap_or_else(|p| p.into_inner());
        let _ = take_window_requests();
        let id = request_window_on(
            Some(std::path::PathBuf::from("/tmp/somewhere")),
            Some("work".to_string()),
            vec!["zsh".to_string(), "-l".to_string()],
        );
        let queued = take_window_requests();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].id, id);
        assert_eq!(
            queued[0].cwd.as_deref(),
            Some(std::path::Path::new("/tmp/somewhere"))
        );
        assert_eq!(queued[0].profile.as_deref(), Some("work"));
        assert_eq!(queued[0].command, vec!["zsh", "-l"]);
    }

    /// A request nobody has taken yet must be visible as one.
    ///
    /// The windowless front end reads this instead of draining: it has no
    /// window to park, so a request it took would be one it threw away --
    /// which is what "New Unterm Window Here" on a parked Unterm did.
    #[test]
    fn a_queued_window_request_is_visible_without_taking_it() {
        let _queue = WINDOW_QUEUE_TEST.lock().unwrap_or_else(|p| p.into_inner());
        let _ = take_window_requests();
        assert!(!window_requests_pending(), "the queue starts empty");
        request_window();
        assert!(window_requests_pending(), "a request is waiting");
        assert!(
            window_requests_pending(),
            "asking must not consume the request"
        );
        assert_eq!(take_window_requests().len(), 1);
        assert!(!window_requests_pending(), "and taking it clears the queue");
    }

    /// A plain request still asks for nothing in particular.
    #[test]
    fn a_plain_window_request_carries_no_ask() {
        let _queue = WINDOW_QUEUE_TEST.lock().unwrap_or_else(|p| p.into_inner());
        let _ = take_window_requests();
        request_window();
        let queued = take_window_requests();
        assert_eq!(queued.len(), 1);
        assert!(queued[0].cwd.is_none());
        assert!(queued[0].profile.is_none());
        assert!(queued[0].command.is_empty());
    }
}

#[cfg(test)]
mod pane_pulse_tests {
    use super::*;

    /// A screen with `rows` numbered lines.
    ///
    /// Built field by field rather than from a `Default`: these are public
    /// wire types, and widening their derives to shorten a test is how a
    /// half-built snapshot becomes constructible by accident somewhere that
    /// matters.
    fn screen(revision: u64, rows: usize) -> StyledScreenSnapshot {
        let line = |row: i64, text: String| StyledScreenLine {
            row,
            cells: text
                .chars()
                .map(|ch| StyledCell {
                    ch,
                    style: CellStyle::default(),
                    width: 1,
                })
                .collect(),
            wrapped: false,
        };
        StyledScreenSnapshot {
            clipboard_request: None,
            focus_reporting: false,
            bells: 1,
            notifications: 3,
            last_notification: Some("something happened".into()),
            mouse: next_core::mouse_encoding::MouseModes::default(),
            lines: (0..rows)
                .map(|i| line(i as i64, format!("row {i}")))
                .collect(),
            cursor: CursorSnapshot {
                x: 0,
                y: 0,
                visible: true,
                shape: "Default".into(),
            },
            cols: 80,
            rows,
            scrollback_rows: 0,
            revision,
            dirty_rows: None,
        }
    }

    #[test]
    fn a_pulse_carries_the_tail_as_text_and_nothing_else() {
        // The Cockpit wants the last few lines and two counters. It used to
        // get the whole styled screen and keep only the characters, which at
        // forty panes cost nine times the poll's entire budget.
        let pulse = PanePulse::from_snapshot(&screen(7, 30), None, 3);
        assert_eq!(pulse.revision, 7);
        assert!(!pulse.unchanged);
        assert_eq!(pulse.tail, vec!["row 27", "row 28", "row 29"]);
        assert_eq!(pulse.notifications, 3);
        assert_eq!(
            pulse.last_notification.as_deref(),
            Some("something happened")
        );
    }

    #[test]
    fn a_pane_nobody_typed_in_sends_no_tail_at_all() {
        // The revision gate is the other half of the saving: an idle pane
        // costs an empty envelope rather than eight lines.
        let pulse = PanePulse::from_snapshot(&screen(7, 30), Some(7), 8);
        assert!(pulse.unchanged);
        assert!(pulse.tail.is_empty());
        // The counters still come. A bell is not a screen change, and a
        // notification raised while the screen stood still must still reach
        // the Cockpit.
        assert_eq!(pulse.notifications, 3);
        assert_eq!(pulse.bells, 1);
    }

    #[test]
    fn a_moved_revision_sends_the_tail_again() {
        let pulse = PanePulse::from_snapshot(&screen(8, 30), Some(7), 2);
        assert!(!pulse.unchanged);
        assert_eq!(pulse.tail, vec!["row 28", "row 29"]);
    }

    #[test]
    fn a_short_screen_gives_what_it_has() {
        // Asking for eight rows of a two-row pane is what startup looks like,
        // not an error.
        let pulse = PanePulse::from_snapshot(&screen(1, 2), None, 8);
        assert_eq!(pulse.tail, vec!["row 0", "row 1"]);
    }
}
