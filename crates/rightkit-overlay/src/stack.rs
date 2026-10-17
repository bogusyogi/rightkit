//! Placement of a stack of transient windows (approval bubbles) beside an anchor.
//!
//! A host reports the anchor (the element or window the stack hangs off), the work area of the
//! monitor it sits on, and each window's measured height. The layout answers where the window at
//! `index` goes: stacked above the anchor or below it, nearest first, one gap apart, held inside
//! the work area by `margin`, and shifted by the offset the user last dragged the stack to.
//! Coordinates are screen pixels with a top-left origin.

use crate::OverlayRect;

/// The shape of one stack: the width of each window, the gap between windows, and the side of
/// the anchor the stack grows toward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackLayout {
    /// Width of every window in the stack.
    pub width: i32,
    /// Vertical gap between neighbouring windows and between the anchor and the nearest window.
    pub gap: i32,
    /// `true` to stack above the anchor, `false` to stack below it.
    pub stack_above: bool,
}

impl StackLayout {
    /// A layout whose side is chosen by [`stack_above`] for a stack `total_height` pixels tall.
    pub fn new(
        anchor: OverlayRect,
        work_area: OverlayRect,
        width: i32,
        total_height: i32,
        gap: i32,
        margin: i32,
    ) -> Self {
        Self {
            width,
            gap,
            stack_above: stack_above(anchor, work_area, total_height, margin),
        }
    }
}

/// Whether a stack `total_height` pixels tall fits above the anchor, which is the preferred side
/// when it does. Otherwise it goes above only when there is more room above the anchor than
/// below it, and that room is not empty.
pub fn stack_above(
    anchor: OverlayRect,
    work_area: OverlayRect,
    total_height: i32,
    margin: i32,
) -> bool {
    let space_above = (anchor.y - work_area.y).max(0);
    let space_below = (work_area.y + work_area.height - (anchor.y + anchor.height)).max(0);
    space_above >= total_height + margin || (space_above >= space_below && space_above > 0)
}

/// The top-left corner `(x, y)` of the window at `index` in a stack of windows with the given
/// `heights`, placed from the anchor under `layout`.
///
/// Windows are stacked from the anchor outward in order: window 0 is nearest the anchor.
///
/// The whole stack is one block. Its origin is set by `offset.1` from the anchor's edge and then
/// clamped so the entire block (every window and the gaps between them) stays `margin` inside
/// the work area. Each window then sits at its fixed spacing inside that block. Because the
/// origin depends on every height rather than on `index`, windows of one stack never overlap.
/// `x` centres the stack on the anchor, moved by `offset.0`, and is kept `margin` inside the work
/// area. An index past the end of `heights` is placed as if its height were 0, so it lands on the
/// slot after the last window.
pub fn place(
    layout: &StackLayout,
    index: usize,
    heights: &[u32],
    anchor: OverlayRect,
    work_area: OverlayRect,
    margin: i32,
    offset: (i32, i32),
) -> (i32, i32) {
    let x = center_x(anchor, work_area, layout.width, margin, offset.0);
    let gap = layout.gap;
    let height = |i: usize| heights.get(i).copied().unwrap_or(0) as i32;
    // Extent of the whole stack: every window plus the gaps between neighbours.
    let total =
        heights.iter().map(|&h| h as i32).sum::<i32>() + gap * (heights.len() as i32 - 1).max(0);
    let low = work_area.y + margin;
    let high = (work_area.y + work_area.height - margin - total).max(low);
    let clamp_origin = |top: i32| top.max(low).min(high);
    if layout.stack_above {
        // The block's bottom edge sits one gap above the anchor, shifted by the offset.
        let top = clamp_origin(anchor.y + offset.1 - gap - total);
        let mut bottom = top + total;
        let mut y = bottom;
        for i in 0..=index {
            y = bottom - height(i);
            bottom = y - gap;
        }
        (x, y.max(low))
    } else {
        let top = clamp_origin(anchor.y + anchor.height + gap + offset.1);
        let mut y = top;
        for i in 0..index {
            y += height(i) + gap;
        }
        let work_bottom = work_area.y + work_area.height - margin;
        (x, y.min(work_bottom))
    }
}

/// The left edge of a window of `width` centred on `anchor`, moved by `offset_x`, and kept
/// `margin` inside the work area.
fn center_x(
    anchor: OverlayRect,
    work_area: OverlayRect,
    width: i32,
    margin: i32,
    offset_x: i32,
) -> i32 {
    let center = anchor.x + anchor.width / 2;
    let x = center - width / 2 + offset_x;
    let min_x = work_area.x + margin;
    let max_x = work_area.x + work_area.width - width - margin;
    x.max(min_x).min(max_x.max(min_x))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: OverlayRect = OverlayRect {
        x: 0,
        y: 0,
        width: 1000,
        height: 800,
    };

    fn anchor_at(y: i32) -> OverlayRect {
        OverlayRect {
            x: 400,
            y,
            width: 200,
            height: 40,
        }
    }

    #[test]
    fn centres_on_the_anchor_and_stacks_above_when_there_is_room() {
        let layout = StackLayout {
            width: 300,
            gap: 10,
            stack_above: true,
        };
        // Anchor top at 500: the first window (100 tall) ends 10 above it.
        assert_eq!(
            place(&layout, 0, &[100], anchor_at(500), WORK, 8, (0, 0)),
            (350, 390)
        );
    }

    #[test]
    fn later_windows_stack_further_out() {
        let layout = StackLayout {
            width: 300,
            gap: 10,
            stack_above: true,
        };
        let heights = [100, 60];
        assert_eq!(
            place(&layout, 1, &heights, anchor_at(500), WORK, 8, (0, 0)),
            (350, 320)
        );
    }

    #[test]
    fn below_the_anchor_stacks_downward() {
        let layout = StackLayout {
            width: 300,
            gap: 10,
            stack_above: false,
        };
        let heights = [100, 60];
        assert_eq!(
            place(&layout, 0, &heights, anchor_at(100), WORK, 8, (0, 0)),
            (350, 150)
        );
        assert_eq!(
            place(&layout, 1, &heights, anchor_at(100), WORK, 8, (0, 0)),
            (350, 260)
        );
    }

    #[test]
    fn drag_offset_moves_the_stack_and_the_margin_holds_it_inside() {
        let layout = StackLayout {
            width: 300,
            gap: 10,
            stack_above: true,
        };
        assert_eq!(
            place(&layout, 0, &[100], anchor_at(500), WORK, 8, (50, 0)),
            (400, 390)
        );
        // Dragged far right: clamped to the work area minus the margin.
        assert_eq!(
            place(&layout, 0, &[100], anchor_at(500), WORK, 8, (5000, 0)),
            (692, 390)
        );
    }

    #[test]
    fn stacks_above_when_the_space_above_is_larger() {
        // Anchor near the bottom: little room below, plenty above.
        assert!(stack_above(anchor_at(700), WORK, 120, 8));
        // Anchor near the top: nothing above, so the stack goes below.
        assert!(!stack_above(anchor_at(10), WORK, 120, 8));
    }

    #[test]
    fn large_persisted_offset_keeps_the_whole_stack_inside_the_work_area() {
        let heights = [100, 60];
        let above = StackLayout {
            width: 300,
            gap: 10,
            stack_above: true,
        };
        // Dragged far up: the block is held on the top margin and keeps its spacing.
        assert_eq!(
            place(&above, 0, &heights, anchor_at(500), WORK, 8, (0, -5000)),
            (350, 78)
        );
        assert_eq!(
            place(&above, 1, &heights, anchor_at(500), WORK, 8, (0, -5000)),
            (350, 8)
        );
        let below = StackLayout {
            width: 300,
            gap: 10,
            stack_above: false,
        };
        // Dragged far down: the block (170 tall) is held on the bottom margin, 800 - 8 - 170.
        assert_eq!(
            place(&below, 0, &heights, anchor_at(100), WORK, 8, (0, 5000)),
            (350, 622)
        );
        assert_eq!(
            place(&below, 1, &heights, anchor_at(100), WORK, 8, (0, 5000)),
            (350, 732)
        );
    }

    #[test]
    fn layout_new_picks_the_side() {
        let layout = StackLayout::new(anchor_at(10), WORK, 300, 120, 10, 8);
        assert!(!layout.stack_above);
        assert_eq!(layout.width, 300);
        assert_eq!(layout.gap, 10);
    }
}
