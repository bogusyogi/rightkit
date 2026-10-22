//! A few line icons as SVG path data on a 24-unit grid, plus the helper that places one. They are
//! the glyph set the HeardRight pill used, kept generic: draw them with
//! [`PaintCtx::stroke_path`](crate::PaintCtx::stroke_path) at [`STROKE_WIDTH`] units scaled by the
//! icon size (or [`PaintCtx::fill_path`](crate::PaintCtx::fill_path) for solid shapes such as
//! [`PLAY`] and [`STOP`]).

use crate::path::{parse_svg_path, transform, PathCmd};

/// Stroke width on the 24-unit grid; multiply by `size / 24.0` for the icon at `size`.
pub const STROKE_WIDTH: f32 = 2.25;

pub const MIC: &[&str] = &[
    "M12 3a2.6 2.6 0 0 0 -2.6 2.6 v5.6 a2.6 2.6 0 0 0 5.2 0 V5.6 A2.6 2.6 0 0 0 12 3 Z",
    "M18 11a6 6 0 0 1 -12 0",
    "M12 17 V21",
];
pub const X: &[&str] = &["M17.5 6.5 6.5 17.5", "M6.5 6.5 17.5 17.5"];
pub const CHECK: &[&str] = &["M4.5 12.5 9.5 17.5 19.5 7"];
/// A circle with a cross: cancel.
pub const CANCEL: &[&str] = &[
    "M12 4 A8 8 0 1 1 12 20 A8 8 0 1 1 12 4",
    "M9 9 L15 15",
    "M15 9 L9 15",
];
pub const STOP: &[&str] = &[
    "M9 6.5 H15 A2.5 2.5 0 0 1 17.5 9 V15 A2.5 2.5 0 0 1 15 17.5 H9 A2.5 2.5 0 0 1 6.5 15 V9 A2.5 2.5 0 0 1 9 6.5 Z",
];
/// An arrow pointing right: send.
pub const SEND: &[&str] = &["M5 12 H17", "M12 6.5 L17.5 12 L12 17.5"];
pub const PLAY: &[&str] = &["M8 5.5 L18 12 L8 18.5 Z"];
pub const COPY: &[&str] = &[
    "M8.5 8.5 H16.5 A2 2 0 0 1 18.5 10.5 V17.5 A2 2 0 0 1 16.5 19.5 H10.5 A2 2 0 0 1 8.5 17.5 V10.5 A2 2 0 0 1 10.5 8.5 Z",
    "M5.5 15.5 V6 A1.5 1.5 0 0 1 7 4.5 H15",
];
/// Three rows with bullets: a list or history.
pub const LIST: &[&str] = &[
    "M9 7 H19",
    "M9 12 H19",
    "M9 17 H19",
    "M5 7 H5.01",
    "M5 12 H5.01",
    "M5 17 H5.01",
];

/// Builds the commands for an icon: every path string parsed, scaled so the 24-unit grid spans
/// `size` logical pixels, and placed with its top-left at `(x, y)`.
pub fn build(paths: &[&str], size: f32, x: f32, y: f32) -> Vec<PathCmd> {
    let scale = size / 24.0;
    paths
        .iter()
        .flat_map(|d| transform(&parse_svg_path(d), scale, x, y))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PaintCtx, Rgba};

    const ALL: [(&str, &[&str]); 9] = [
        ("mic", MIC),
        ("x", X),
        ("check", CHECK),
        ("cancel", CANCEL),
        ("stop", STOP),
        ("send", SEND),
        ("play", PLAY),
        ("copy", COPY),
        ("list", LIST),
    ];

    #[test]
    fn every_icon_parses_completely() {
        for (name, paths) in ALL {
            for d in paths {
                let cmds = parse_svg_path(d);
                assert!(cmds.len() >= 2, "{name}: {d} parsed to {cmds:?}");
                assert!(matches!(cmds[0], PathCmd::MoveTo(..)), "{name}");
            }
        }
    }

    #[test]
    fn build_scales_the_grid_to_the_requested_size() {
        let cmds = build(CHECK, 48.0, 10.0, 20.0);
        // The first point (4.5, 12.5) on a 24 grid is (9, 25) at 48 px, then moved by (10, 20).
        assert_eq!(cmds[0], PathCmd::MoveTo(19.0, 45.0));
    }

    #[test]
    fn a_stroked_icon_lights_pixels_inside_its_box_only() {
        let mut ctx = PaintCtx::offscreen(48, 48, 96);
        let size = 24.0;
        let cmds = build(CHECK, size, 12.0, 12.0);
        ctx.stroke_path(&cmds, STROKE_WIDTH * size / 24.0, Rgba::WHITE);
        let lit = ctx.pixels().iter().filter(|&&p| p != 0).count();
        assert!(lit > 20, "{lit}");
        assert_eq!(ctx.pixel(2, 2), 0);
        assert_eq!(ctx.pixel(45, 45), 0);
        // The tick passes through the middle of its box.
        assert!((0..48).any(|y| ctx.pixel(24, y) != 0));
    }

    #[test]
    fn a_filled_play_triangle_has_solid_middle() {
        let mut ctx = PaintCtx::offscreen(48, 48, 96);
        ctx.fill_path(&build(PLAY, 24.0, 12.0, 12.0), Rgba::WHITE);
        assert_eq!(ctx.pixel(22, 24) >> 24, 255);
        assert_eq!(ctx.pixel(2, 24), 0);
    }
}
