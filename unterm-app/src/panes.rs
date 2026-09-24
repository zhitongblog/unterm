//! Several shells in one window.
//!
//! The arrangement is next-core's: the registry holds the split tree and hands
//! back a rectangle per pane in cells. What lives here is the part a window
//! needs on top of that -- which session is in which pane, and turning a cell
//! rectangle into pixels.
//!
//! Kept out of the event loop so the arithmetic can be checked, because a
//! rectangle that is one cell out is a line of text disappearing under a
//! divider, and that is easier to assert than to notice.

use crate::keys::Direction;
use unterm_engine::next_core::layout::PaneRect;
use unterm_render::quads::CellMetrics;

/// Where a pane goes, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanePlacement {
    pub session_id: usize,
    pub origin: (f32, f32),
    pub cols: usize,
    pub rows: usize,
}

/// Turn a pane's cell rectangle into pixels.
pub fn place(session_id: usize, rect: PaneRect, metrics: CellMetrics) -> PanePlacement {
    PanePlacement {
        session_id,
        origin: (
            rect.left as f32 * metrics.width,
            rect.top as f32 * metrics.height,
        ),
        // At least one of each: a pane the engine sized to nothing would make
        // the PTY reject its resize and the shell draw nothing at all.
        cols: rect.width.max(1),
        rows: rect.height.max(1),
    }
}

/// How thick the line between two panes is drawn, in pixels.
///
/// A line, not the gap it sits in. The layout keeps a whole cell between
/// panes, because the panes have to land on cell boundaries -- but filling
/// that cell draws a bar as wide as a character, which reads as a window frame
/// between two windows rather than as a seam in one.
///
/// One pixel: Fluent's divider stroke. Two read as a frame between windows at
/// 100% scaling, which is the scaling most Windows desktops still run at.
const WEIGHT: f32 = 1.0;

/// The divider between two panes, if there is room for one.
///
/// A line rather than a gap: without something drawn there, two shells with
/// the same background look like one shell with strange wrapping. Centred in
/// the cell the layout reserved, so the space either side of it belongs to
/// neither pane and the line itself is the boundary.
pub fn divider_after(
    rect: PaneRect,
    metrics: CellMetrics,
    vertical: bool,
) -> Option<(f32, f32, f32, f32)> {
    if vertical {
        let cell_left = (rect.left + rect.width) as f32 * metrics.width;
        Some((
            cell_left + ((metrics.width - WEIGHT) / 2.0).round().max(0.0),
            rect.top as f32 * metrics.height,
            WEIGHT.min(metrics.width),
            rect.height as f32 * metrics.height,
        ))
    } else {
        let cell_top = (rect.top + rect.height) as f32 * metrics.height;
        Some((
            rect.left as f32 * metrics.width,
            cell_top + ((metrics.height - WEIGHT) / 2.0).round().max(0.0),
            rect.width as f32 * metrics.width,
            WEIGHT.min(metrics.height),
        ))
    }
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

    fn rect(left: usize, top: usize, width: usize, height: usize) -> PaneRect {
        PaneRect {
            left,
            top,
            width,
            height,
        }
    }

    #[test]
    fn the_first_pane_starts_at_the_top_left() {
        let placed = place(1, rect(0, 0, 40, 24), metrics());

        assert_eq!(placed.origin, (0.0, 0.0));
        assert_eq!((placed.cols, placed.rows), (40, 24));
    }

    #[test]
    fn a_pane_to_the_right_starts_after_the_cells_before_it() {
        let placed = place(2, rect(41, 0, 39, 24), metrics());

        // One cell out here is a column of text sliding under the divider.
        assert_eq!(placed.origin, (410.0, 0.0));
    }

    #[test]
    fn a_pane_below_starts_after_the_rows_above_it() {
        let placed = place(2, rect(0, 13, 80, 11), metrics());

        assert_eq!(placed.origin, (0.0, 260.0));
    }

    #[test]
    fn a_pane_the_engine_sized_to_nothing_still_asks_for_a_cell() {
        let placed = place(3, rect(0, 0, 0, 0), metrics());

        // A zero-sized PTY rejects its resize and the shell draws nothing.
        assert_eq!((placed.cols, placed.rows), (1, 1));
    }

    /// In the reserved column, and thin: the layout keeps a whole cell between
    /// panes because panes land on cell boundaries, but filling that cell
    /// draws a bar as wide as a character -- a frame between two windows
    /// rather than a seam in one.
    #[test]
    fn a_vertical_divider_is_a_line_centred_in_the_column_between_two_panes() {
        let cell = metrics();
        let (left, top, width, height) =
            divider_after(rect(0, 0, 40, 24), cell, true).expect("a divider");

        assert_eq!(top, 0.0);
        assert_eq!(height, 480.0);
        assert!(width < cell.width, "the divider fills its column");
        // Inside the reserved column, with room either side.
        assert!(
            left > 400.0 && left + width < 410.0,
            "at {left} wide {width}"
        );
    }

    #[test]
    fn a_horizontal_divider_is_a_line_centred_in_the_row_between_two_panes() {
        let cell = metrics();
        let (left, top, width, height) =
            divider_after(rect(0, 0, 80, 12), cell, false).expect("a divider");

        assert_eq!(left, 0.0);
        assert_eq!(width, 800.0);
        assert!(height < cell.height, "the divider fills its row");
        assert!(
            top > 240.0 && top + height < 260.0,
            "at {top} high {height}"
        );
    }

    /// It never reaches into either pane. A divider a pixel too wide eats the
    /// first column of the pane beside it, which is where a prompt starts.
    #[test]
    fn a_divider_stays_inside_the_cell_that_was_reserved_for_it() {
        for cell in [
            CellMetrics {
                width: 3.0,
                height: 4.0,
                baseline: 3.0,
            },
            CellMetrics {
                width: 10.0,
                height: 20.0,
                baseline: 16.0,
            },
            CellMetrics {
                width: 1.0,
                height: 1.0,
                baseline: 1.0,
            },
        ] {
            let (left, _, width, _) = divider_after(rect(0, 0, 4, 4), cell, true).expect("one");
            let column = 4.0 * cell.width;
            assert!(left >= column, "{cell:?}: starts before the column");
            assert!(
                left + width <= column + cell.width,
                "{cell:?}: runs into the pane beside it"
            );

            let (_, top, _, height) = divider_after(rect(0, 0, 4, 4), cell, false).expect("one");
            let row = 4.0 * cell.height;
            assert!(top >= row, "{cell:?}: starts above the row");
            assert!(
                top + height <= row + cell.height,
                "{cell:?}: runs into the pane below it"
            );
        }
    }

    #[test]
    fn placements_do_not_overlap() {
        // The arrangement the registry produces for one vertical split of an
        // 80-column window: the divider costs a column.
        let left = place(1, rect(0, 0, 40, 24), metrics());
        let right = place(2, rect(41, 0, 39, 24), metrics());

        let left_edge = left.origin.0 + left.cols as f32 * metrics().width;
        assert!(
            right.origin.0 >= left_edge,
            "panes overlap: {left:?} then {right:?}"
        );
    }
}

/// Which pane a direction key should move to.
///
/// By where the panes are on screen, not by where they sit in the split tree:
/// with three panes the tree has a shape the screen does not show, and someone
/// pressing Alt+Right means the pane to the right of this one.
///
/// Distance is weighted across the direction rather than along it, so a pane
/// directly beside this one wins over a nearer one that is off to a side.
pub fn pane_toward(
    placements: &[PanePlacement],
    from: usize,
    direction: Direction,
    metrics: CellMetrics,
) -> Option<usize> {
    let centre = |placement: &PanePlacement| {
        (
            placement.origin.0 + placement.cols as f32 * metrics.width / 2.0,
            placement.origin.1 + placement.rows as f32 * metrics.height / 2.0,
        )
    };
    let (from_x, from_y) = centre(placements.iter().find(|p| p.session_id == from)?);

    placements
        .iter()
        .filter(|placement| placement.session_id != from)
        .filter(|placement| {
            let (x, y) = centre(placement);
            match direction {
                Direction::Left => x < from_x,
                Direction::Right => x > from_x,
                Direction::Up => y < from_y,
                Direction::Down => y > from_y,
            }
        })
        .min_by(|a, b| {
            let cost = |placement: &PanePlacement| {
                let (x, y) = centre(placement);
                let (along, across) = match direction {
                    Direction::Left => (from_x - x, (from_y - y).abs()),
                    Direction::Right => (x - from_x, (from_y - y).abs()),
                    Direction::Up => (from_y - y, (from_x - x).abs()),
                    Direction::Down => (y - from_y, (from_x - x).abs()),
                };
                across * 4.0 + along
            };
            cost(a).total_cmp(&cost(b))
        })
        .map(|placement| placement.session_id)
}

#[cfg(test)]
mod direction_tests {
    use super::*;

    fn metrics() -> CellMetrics {
        CellMetrics {
            width: 10.0,
            height: 20.0,
            baseline: 16.0,
        }
    }

    fn pane(session_id: usize, left: usize, top: usize, cols: usize, rows: usize) -> PanePlacement {
        place(
            session_id,
            PaneRect {
                left,
                top,
                width: cols,
                height: rows,
            },
            metrics(),
        )
    }

    /// Two panes side by side: right goes right, left comes back.
    #[test]
    fn a_split_moves_both_ways() {
        let panes = vec![pane(1, 0, 0, 40, 24), pane(2, 41, 0, 40, 24)];
        assert_eq!(pane_toward(&panes, 1, Direction::Right, metrics()), Some(2));
        assert_eq!(pane_toward(&panes, 2, Direction::Left, metrics()), Some(1));
    }

    /// And there is nothing past the edge, which must be nothing rather than
    /// a wrap round to the far side -- a key that jumps somewhere unexpected
    /// is worse than one that does nothing.
    #[test]
    fn there_is_no_pane_past_the_last_one() {
        let panes = vec![pane(1, 0, 0, 40, 24), pane(2, 41, 0, 40, 24)];
        assert_eq!(pane_toward(&panes, 2, Direction::Right, metrics()), None);
        assert_eq!(pane_toward(&panes, 1, Direction::Up, metrics()), None);
    }

    /// A tall pane on the left, two stacked on the right. From the left one,
    /// Right must reach the one it is actually beside.
    #[test]
    fn the_pane_beside_this_one_wins_over_a_nearer_one_off_to_a_side() {
        let panes = vec![
            pane(1, 0, 12, 40, 12),  // left, lower half
            pane(2, 41, 0, 40, 11),  // right, upper
            pane(3, 41, 12, 40, 12), // right, level with pane 1
        ];
        assert_eq!(pane_toward(&panes, 1, Direction::Right, metrics()), Some(3));
    }

    #[test]
    fn stacked_panes_move_up_and_down() {
        let panes = vec![pane(1, 0, 0, 80, 11), pane(2, 0, 12, 80, 12)];
        assert_eq!(pane_toward(&panes, 1, Direction::Down, metrics()), Some(2));
        assert_eq!(pane_toward(&panes, 2, Direction::Up, metrics()), Some(1));
    }

    /// One pane has nowhere to go, and asking must not panic.
    #[test]
    fn a_single_pane_has_no_neighbours() {
        let panes = vec![pane(1, 0, 0, 80, 24)];
        for direction in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            assert_eq!(pane_toward(&panes, 1, direction, metrics()), None);
        }
    }

    /// A pane id that is not in the list is a stale focus, not a crash.
    #[test]
    fn an_unknown_pane_is_answered_with_nothing() {
        let panes = vec![pane(1, 0, 0, 80, 24)];
        assert_eq!(pane_toward(&panes, 99, Direction::Left, metrics()), None);
    }
}
