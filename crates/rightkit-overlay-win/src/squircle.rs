//! Continuous-curvature ("squircle") rounded rectangles, the corner shape of an app icon and of
//! SwiftUI's `.continuous` style. A circular corner joins a straight side with a jump in
//! curvature; the continuous corner leaves each side 1.5287 radii from the vertex and eases into
//! the turn, so it reads flatter and softer than a circular arc of the same radius.
//!
//! Carried over from the Pulse notch canvas (`windows/src/canvas.rs`, `Continuous`): the same
//! three cubic Bézier segments per corner, a lookup table of the curve, and exact coverage by
//! corner tests plus an 8 x 8 sub-sample grid only on the pixels the edge crosses. Pure code, no
//! Win32.

/// How far a continuous corner reaches along each side, in radii.
pub(crate) const REACH: f32 = 1.528_665;

/// The three cubic Béziers of one continuous corner, in radii: from where it leaves the side to
/// where it meets the other, as `(distance from the side, distance from the top edge)`.
const CURVES: [[(f32, f32); 4]; 3] = [
    [
        (1.528_665, 0.0),
        (1.088_493, 0.0),
        (0.868_407, 0.0),
        (0.631_494, 0.074_911),
    ],
    [
        (0.631_494, 0.074_911),
        (0.372_824, 0.169_060),
        (0.169_060, 0.372_824),
        (0.074_911, 0.631_494),
    ],
    [
        (0.074_911, 0.631_494),
        (0.0, 0.868_407),
        (0.0, 1.088_493),
        (0.0, 1.528_665),
    ],
];

/// Points per Bézier in the corner table.
const STEPS: usize = 64;
/// Sub-samples per side of a pixel when the edge crosses it.
const SAMPLES: usize = 8;

/// A rectangle with continuous corners, in device pixels.
pub(crate) struct Squircle {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    /// Where the curve meets a side, measured from the vertex.
    reach: f32,
    /// One corner as `(u, v)` points with `u` ascending: the distance from the side the point
    /// meets and from the edge it leaves.
    corner: Vec<(f32, f32)>,
}

impl Squircle {
    /// `rect` is `(x, y, width, height)`. A corner takes no more than half of the shorter side,
    /// so `radius` is capped at `min(w, h) / 2 / REACH`; a negative or NaN radius is a plain
    /// rectangle.
    pub(crate) fn new((x, y, w, h): (f32, f32, f32, f32), radius: f32) -> Self {
        let cap = w.min(h).max(0.0) / 2.0 / REACH;
        let radius = if radius.is_nan() {
            0.0
        } else {
            radius.min(cap).max(0.0)
        };
        let mut corner = Vec::with_capacity(CURVES.len() * STEPS + 1);
        for (index, curve) in CURVES.iter().enumerate() {
            // Each curve starts where the one before it ended.
            let first = usize::from(index > 0);
            for step in first..=STEPS {
                let t = step as f32 / STEPS as f32;
                let k = 1.0 - t;
                let at = |a: f32, b: f32, c: f32, d: f32| {
                    a * k * k * k + 3.0 * b * k * k * t + 3.0 * c * k * t * t + d * t * t * t
                };
                let [p0, p1, p2, p3] = *curve;
                corner.push((
                    at(p0.0, p1.0, p2.0, p3.0) * radius,
                    at(p0.1, p1.1, p2.1, p3.1) * radius,
                ));
            }
        }
        corner.reverse();
        Self {
            x,
            y,
            w,
            h,
            reach: REACH * radius,
            corner,
        }
    }

    /// Whether the point is inside the shape.
    pub(crate) fn contains(&self, sx: f32, sy: f32) -> bool {
        let u = (sx - self.x).min(self.x + self.w - sx);
        let v = (sy - self.y).min(self.y + self.h - sy);
        if u < 0.0 || v < 0.0 {
            return false;
        }
        if u >= self.reach || v >= self.reach {
            return true;
        }
        // In a corner: on the inner side of the curve's height at the point's distance from the
        // side.
        let at = self.corner.partition_point(|&(along, _)| along < u);
        let (u1, v1) = self.corner[at.min(self.corner.len() - 1)];
        let (u0, v0) = self.corner[at.saturating_sub(1)];
        let edge = if u1 > u0 {
            v0 + (v1 - v0) * (u - u0) / (u1 - u0)
        } else {
            v1
        };
        v >= edge
    }

    /// The share of the device pixel `(px, py)` the shape covers, `0.0..=1.0`.
    pub(crate) fn coverage(&self, px: i32, py: i32) -> f32 {
        let (x0, y0) = (px as f32, py as f32);
        let across = (x0 - self.x).min(self.x + self.w - x0 - 1.0);
        let down = (y0 - self.y).min(self.y + self.h - y0 - 1.0);
        // Well inside: past the corners on one axis.
        if across >= 0.0 && down >= 0.0 && (across >= self.reach || down >= self.reach) {
            return 1.0;
        }
        let corners = [
            (x0, y0),
            (x0 + 1.0, y0),
            (x0, y0 + 1.0),
            (x0 + 1.0, y0 + 1.0),
        ];
        let inside = corners
            .iter()
            .filter(|&&(cx, cy)| self.contains(cx, cy))
            .count();
        match inside {
            4 => 1.0,
            0 => 0.0,
            _ => {
                let n = SAMPLES;
                let mut hits = 0u32;
                for j in 0..n {
                    for i in 0..n {
                        let sx = x0 + (i as f32 + 0.5) / n as f32;
                        let sy = y0 + (j as f32 + 0.5) / n as f32;
                        if self.contains(sx, sy) {
                            hits += 1;
                        }
                    }
                }
                hits as f32 / (n * n) as f32
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_are_cut_and_sides_are_straight() {
        let s = Squircle::new((0.0, 0.0, 40.0, 40.0), 10.0);
        // The middle of the shape is inside and straight edges are full.
        assert_eq!(s.coverage(20, 20), 1.0);
        assert_eq!(s.coverage(20, 0), 1.0);
        assert_eq!(s.coverage(0, 20), 1.0);
        assert_eq!(s.coverage(39, 20), 1.0);
        assert_eq!(s.coverage(20, 39), 1.0);
        // All four vertex pixels are cut away.
        for (x, y) in [(0, 0), (39, 0), (0, 39), (39, 39)] {
            assert_eq!(s.coverage(x, y), 0.0, "corner pixel ({x}, {y})");
        }
        // The four corners are mirror images of each other.
        for (a, b) in [((3, 1), (36, 1)), ((3, 1), (3, 38)), ((1, 3), (38, 3))] {
            assert_eq!(s.coverage(a.0, a.1), s.coverage(b.0, b.1), "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn the_corner_leaves_the_side_later_than_a_circle_of_the_same_radius() {
        let r = 10.0;
        let s = Squircle::new((0.0, 0.0, 60.0, 60.0), r);
        // A circular corner of radius 10 has met the top edge 10 px from the vertex; the
        // continuous corner is still cut away there and only meets it at REACH * r = 15.3 px.
        assert!(!s.contains(10.0, 0.05));
        assert!(!s.contains(0.05, 10.0));
        assert!(s.contains(12.0, 0.05));
        assert!(s.contains(REACH * r + 0.1, 0.001));
        assert!(s.contains(0.001, REACH * r + 0.1));
        // Close to the vertex it is cut as deep as any rounded corner, and well inside it is not.
        assert!(!s.contains(2.0, 0.05));
        assert!(s.contains(6.0, 1.0));
        assert!(!s.contains(6.0, 0.5));
    }

    #[test]
    fn the_radius_is_capped_by_the_shorter_side() {
        // A capsule-ish request on a 20 px tall bar: the corner may take at most half the height.
        let s = Squircle::new((0.0, 0.0, 100.0, 20.0), 500.0);
        assert!((s.reach - 10.0).abs() < 1.0e-3, "reach {}", s.reach);
        assert_eq!(s.coverage(50, 10), 1.0);
        assert_eq!(s.coverage(0, 0), 0.0);
        // Degenerate and hostile radii never panic and draw a plain rectangle.
        for radius in [0.0, -4.0, f32::NAN, f32::NEG_INFINITY] {
            let plain = Squircle::new((0.0, 0.0, 8.0, 8.0), radius);
            assert_eq!(plain.coverage(0, 0), 1.0, "radius {radius}");
            assert_eq!(plain.coverage(7, 7), 1.0);
            assert_eq!(plain.coverage(8, 8), 0.0, "outside the rect");
        }
        let infinite = Squircle::new((0.0, 0.0, 8.0, 8.0), f32::INFINITY);
        assert_eq!(infinite.coverage(4, 4), 1.0);
    }

    #[test]
    fn edge_pixels_get_partial_coverage() {
        let s = Squircle::new((0.0, 0.0, 40.0, 40.0), 12.0);
        // Somewhere along the corner arc there is a pixel that is neither empty nor full.
        let partial = (0..8)
            .flat_map(|y| (0..8).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let c = s.coverage(x, y);
                c > 0.0 && c < 1.0
            })
            .count();
        assert!(partial >= 3, "{partial} anti-aliased pixels");
    }

    #[test]
    fn a_zero_size_rectangle_covers_nothing() {
        let s = Squircle::new((5.0, 5.0, 0.0, 0.0), 4.0);
        assert_eq!(s.coverage(5, 5), 0.0);
        let negative = Squircle::new((5.0, 5.0, -3.0, 10.0), 4.0);
        assert_eq!(negative.coverage(4, 8), 0.0);
    }
}
