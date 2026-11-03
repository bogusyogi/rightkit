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
