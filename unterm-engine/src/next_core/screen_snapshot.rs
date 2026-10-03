use crate::{
    CellStyle, CursorSnapshot, DirtyRows, ScreenLine, ScreenSnapshot, StyledCell, StyledScreenLine,
    StyledScreenSnapshot, StyledScrollbackSnapshot,
};

#[derive(Clone, Debug)]
pub(super) struct ScreenSnapshotMeta {
    pub(super) cursor: CursorSnapshot,
    pub(super) cols: usize,
    pub(super) rows: usize,
    pub(super) scrollback_rows: usize,
    pub(super) revision: u64,
    pub(super) dirty_rows: Option<DirtyRows>,
    pub(super) mouse: crate::next_core::mouse_encoding::MouseModes,
    pub(super) bells: u64,
    pub(super) notifications: u64,
    pub(super) last_notification: Option<String>,
    pub(super) progress: Option<crate::TerminalProgress>,
    pub(super) commands: crate::CommandMarks,
    pub(super) focus_reporting: bool,
    pub(super) clipboard_request: Option<String>,
    pub(super) images: Vec<crate::ImagePlacementSnapshot>,
}

pub(super) fn plain_viewport(
    lines: Vec<String>,
    first_row: i64,
    meta: ScreenSnapshotMeta,
) -> ScreenSnapshot {
    let cells = lines
        .iter()
        .enumerate()
        .map(|(idx, text)| ScreenLine {
            row: first_row + idx as i64,
            text: text.clone(),
        })
        .collect();

    ScreenSnapshot {
        lines,
        cells,
        cursor: meta.cursor,
        cols: meta.cols,
        rows: meta.rows,
        scrollback_rows: meta.scrollback_rows,
        revision: meta.revision,
        dirty_rows: meta.dirty_rows,
        mouse: meta.mouse,
        bells: meta.bells,
        notifications: meta.notifications,
        last_notification: meta.last_notification.clone(),
        progress: meta.progress,
        commands: meta.commands,
        focus_reporting: meta.focus_reporting,
        clipboard_request: meta.clipboard_request.clone(),
        images: meta.images.clone(),
    }
}

pub(super) fn styled_viewport(
    lines: Vec<StyledScreenLine>,
    meta: ScreenSnapshotMeta,
) -> StyledScreenSnapshot {
    StyledScreenSnapshot {
        lines,
        cursor: meta.cursor,
        cols: meta.cols,
        rows: meta.rows,
        scrollback_rows: meta.scrollback_rows,
        revision: meta.revision,
        dirty_rows: meta.dirty_rows,
        mouse: meta.mouse,
        bells: meta.bells,
        notifications: meta.notifications,
        last_notification: meta.last_notification.clone(),
        progress: meta.progress,
        commands: meta.commands,
        focus_reporting: meta.focus_reporting,
        clipboard_request: meta.clipboard_request.clone(),
        images: meta.images.clone(),
    }
}

pub(super) fn plain_lines(lines: Vec<String>, start: usize) -> Vec<ScreenLine> {
    lines
        .into_iter()
        .enumerate()
        .map(|(idx, text)| ScreenLine {
            row: (start + idx) as i64,
            text,
        })
        .collect()
}

pub(super) fn escaped_styled_scrollback(
    lines: &[String],
    first_row: i64,
    row_count: i64,
    cols: usize,
    scrollback_top: i64,
    physical_top: i64,
    viewport_rows: usize,
) -> StyledScrollbackSnapshot {
    let lines = lines
        .iter()
        .enumerate()
        .map(|(idx, line)| StyledScreenLine {
            row: first_row + idx as i64,
            // Plain-text fallback: the source has no wrap state to carry.
            wrapped: false,
            cells: line
                .chars()
                .map(|ch| {
                    let mut buf = [0u8; 4];
                    StyledCell {
                        ch,
                        style: CellStyle::default(),
                        width: termwiz::cell::unicode_column_width(ch.encode_utf8(&mut buf), None),
                    }
                })
                .collect(),
        })
        .collect();

    StyledScrollbackSnapshot {
        lines,
        first_row,
        row_count,
        cols,
        scrollback_top,
        physical_top,
        viewport_rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> ScreenSnapshotMeta {
        ScreenSnapshotMeta {
            cursor: CursorSnapshot {
                x: 1,
                y: 2,
                visible: true,
                shape: "Default".to_string(),
            },
            cols: 4,
            rows: 3,
            scrollback_rows: 9,
            revision: 7,
            dirty_rows: Some(DirtyRows { start: 1, end: 1 }),
            mouse: Default::default(),
            bells: 0,
            notifications: 0,
            last_notification: None,
            progress: None,
            commands: Default::default(),
            focus_reporting: false,
            clipboard_request: None,
            images: Vec::new(),
        }
    }

    #[test]
    fn plain_viewport_maps_text_lines_to_absolute_rows() {
        let snapshot = plain_viewport(vec!["one".to_string(), "two".to_string()], 5, meta());

        assert_eq!(snapshot.lines, vec!["one", "two"]);
        assert_eq!(snapshot.cells[0].row, 5);
        assert_eq!(snapshot.cells[1].row, 6);
        assert_eq!(snapshot.revision, 7);
        assert_eq!(snapshot.dirty_rows, Some(DirtyRows { start: 1, end: 1 }));
    }

    #[test]
    fn plain_lines_maps_start_row() {
        let lines = plain_lines(vec!["a".to_string(), "b".to_string()], 10);

        assert_eq!(lines[0].row, 10);
        assert_eq!(lines[1].row, 11);
        assert_eq!(lines[1].text, "b");
    }

    #[test]
    fn escaped_styled_scrollback_uses_default_style_and_unicode_width() {
        let snapshot = escaped_styled_scrollback(&["a你".to_string()], 3, 1, 8, 0, 2, 5);

        assert_eq!(snapshot.lines[0].row, 3);
        assert_eq!(snapshot.lines[0].cells[0].style, CellStyle::default());
        assert_eq!(snapshot.lines[0].cells[0].width, 1);
        assert_eq!(snapshot.lines[0].cells[1].width, 2);
        assert_eq!(snapshot.physical_top, 2);
    }
}
