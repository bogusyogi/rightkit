//! Software canvas for the notch and its cards: anti-aliased rounded rectangles, ring arcs
//! and text masks composited into a premultiplied `0xAARRGGBB` buffer, ready for
//! `UpdateLayeredWindow`. Pure code (no Win32), so the output can be reasoned about on its
//! own. Colours are `0xRRGGBB`; alpha and coverage are `0.0..=1.0`.

use std::f32::consts::TAU;

pub type Rgb = u32;

/// Grey coverage of rendered text (one byte per pixel).
pub struct Mask {
    pub width: usize,
    pub height: usize,
    pub coverage: Vec<u8>,
}

#[derive(Clone)]
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    /// Premultiplied `0xAARRGGBB`, row-major, top row first.
    pub pixels: Vec<u32>,
}

fn channels(color: Rgb) -> (f32, f32, f32) {
    (
        ((color >> 16) & 0xFF) as f32,
        ((color >> 8) & 0xFF) as f32,
        (color & 0xFF) as f32,
    )
}

/// How far a continuous corner reaches along each side, in radii.
const CONTINUOUS_REACH: f32 = 1.528_665;
/// Apple's three cubic Béziers for one continuous corner, in radii: from where it leaves the top
/// edge to where it meets the side, `(distance from the side, distance from the top edge)`.
const CONTINUOUS_CURVES: [[(f32, f32); 4]; 3] = [
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
/// Points per Bézier in the table of a corner, and samples per side of a pixel's coverage.
const CONTINUOUS_STEPS: usize = 64;
const CONTINUOUS_SAMPLES: usize = 8;

/// A rectangle with the Mac's continuous corners (SwiftUI's `.continuous` style, the shape of an
/// app icon): a corner leaves each side 1.5287 radii from the vertex and eases round, flatter
/// than a circular arc of the same radius.
struct Continuous {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    reach: f32,
    /// One corner as `(u, v)` points with `u` ascending: the distance from the side it
    /// meets and from the top edge it leaves.
    corner: Vec<(f32, f32)>,
}

impl Continuous {
    fn new((x, y, w, h): (f32, f32, f32, f32), radius: f32) -> Self {
        // A corner takes no more than half of the shorter side.
        let radius = radius.min(w.min(h) / 2.0 / CONTINUOUS_REACH).max(0.0);
        let mut corner = Vec::with_capacity(CONTINUOUS_CURVES.len() * CONTINUOUS_STEPS + 1);
        for (index, curve) in CONTINUOUS_CURVES.iter().enumerate() {
            // Each curve starts where the one before it ended.
            let first = usize::from(index > 0);
            for step in first..=CONTINUOUS_STEPS {
                let t = step as f32 / CONTINUOUS_STEPS as f32;
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
            reach: CONTINUOUS_REACH * radius,
            corner,
        }
    }

    /// Whether the point is inside the shape.
    fn contains(&self, sx: f32, sy: f32) -> bool {
        let u = (sx - self.x).min(self.x + self.w - sx);
        let v = (sy - self.y).min(self.y + self.h - sy);
        if u < 0.0 || v < 0.0 {
            return false;
        }
        if u >= self.reach || v >= self.reach {
            return true;
        }
        // In a corner: below the curve's height at the point's distance from the side.
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

    /// The share of pixel `(px, py)` the shape covers.
    fn coverage(&self, px: usize, py: usize) -> f32 {
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
                let n = CONTINUOUS_SAMPLES;
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

impl Canvas {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width * height],
        }
    }

    /// Source-over blend of `color` at `coverage` onto the premultiplied pixel.
    fn blend(&mut self, x: usize, y: usize, color: Rgb, coverage: f32) {
        let a = coverage.clamp(0.0, 1.0);
        if a <= 0.0 || x >= self.width || y >= self.height {
            return;
        }
        let index = y * self.width + x;
        let dst = self.pixels[index];
        let inv = 1.0 - a;
        let (sr, sg, sb) = channels(color);
        let mix = |source: f32, shift: u32| -> u32 {
            let d = ((dst >> shift) & 0xFF) as f32;
            (source * a + d * inv).round().clamp(0.0, 255.0) as u32
        };
        let out_a = (255.0 * a + ((dst >> 24) & 0xFF) as f32 * inv)
            .round()
            .clamp(0.0, 255.0) as u32;
        self.pixels[index] = (out_a << 24) | (mix(sr, 16) << 16) | (mix(sg, 8) << 8) | mix(sb, 0);
    }

    /// Rounded rectangle with per-corner radii `[top-left, top-right, bottom-right,
    /// bottom-left]`.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_round_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radii: [f32; 4],
        color: Rgb,
        alpha: f32,
    ) {
        let x0 = x.floor().max(0.0) as usize;
        let y0 = y.floor().max(0.0) as usize;
        let x1 = ((x + w).ceil().max(0.0) as usize).min(self.width);
        let y1 = ((y + h).ceil().max(0.0) as usize).min(self.height);
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        for py in y0..y1 {
            for px in x0..x1 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let radius = match (fx < cx, fy < cy) {
                    (true, true) => radii[0],
                    (false, true) => radii[1],
                    (false, false) => radii[2],
                    (true, false) => radii[3],
                }
                .min(w / 2.0)
                .min(h / 2.0)
                .max(0.0);
                let qx = (fx - cx).abs() - w / 2.0 + radius;
                let qy = (fy - cy).abs() - h / 2.0 + radius;
                let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
                let distance = outside + qx.max(qy).min(0.0) - radius;
                self.blend(px, py, color, (0.5 - distance).clamp(0.0, 1.0) * alpha);
            }
        }
    }

    /// A rectangle `(x, y, width, height)` with the Mac's continuous corners of `radius`.
    pub fn fill_continuous_rect(
        &mut self,
        rect: (f32, f32, f32, f32),
        radius: f32,
        color: Rgb,
        alpha: f32,
    ) {
        let (x, y, w, h) = rect;
        let shape = Continuous::new(rect, radius);
        let x0 = x.floor().max(0.0) as usize;
        let y0 = y.floor().max(0.0) as usize;
        let x1 = ((x + w).ceil().max(0.0) as usize).min(self.width);
        let y1 = ((y + h).ceil().max(0.0) as usize).min(self.height);
        for py in y0..y1 {
            for px in x0..x1 {
                self.blend(px, py, color, shape.coverage(px, py) * alpha);
            }
        }
    }

    /// A plain rectangle `(x, y, width, height)` cut to the continuous rectangle `shape`
    /// (`radius` its corner): the Mac's `clipShape` over a card's colour band.
    pub fn fill_rect_within(
        &mut self,
        rect: (f32, f32, f32, f32),
        shape: (f32, f32, f32, f32),
        radius: f32,
        color: Rgb,
        alpha: f32,
    ) {
        let (x, y, w, h) = rect;
        let clip = Continuous::new(shape, radius);
        let x0 = x.floor().max(0.0) as usize;
        let y0 = y.floor().max(0.0) as usize;
        let x1 = ((x + w).ceil().max(0.0) as usize).min(self.width);
        let y1 = ((y + h).ceil().max(0.0) as usize).min(self.height);
        for py in y0..y1 {
            let rows = ((py as f32 + 1.0).min(y + h) - (py as f32).max(y)).clamp(0.0, 1.0);
            for px in x0..x1 {
                let columns = ((px as f32 + 1.0).min(x + w) - (px as f32).max(x)).clamp(0.0, 1.0);
                self.blend(
                    px,
                    py,
                    color,
                    rows * columns * clip.coverage(px, py) * alpha,
                );
            }
        }
    }

    /// The border of a continuous rectangle: a ring `width` thick just inside its edge, blended
    /// over what is already there (the Mac's `strokeBorder`, which is drawn over the content).
    /// The inner edge is the same rectangle inset by `width`, its corner radius smaller by it.
    pub fn stroke_continuous_rect(
        &mut self,
        rect: (f32, f32, f32, f32),
        radius: f32,
        width: f32,
        color: Rgb,
        alpha: f32,
    ) {
        let (x, y, w, h) = rect;
        let outer = Continuous::new(rect, radius);
        let inner = Continuous::new(
            (x + width, y + width, w - 2.0 * width, h - 2.0 * width),
            radius - width,
        );
        let x0 = x.floor().max(0.0) as usize;
        let y0 = y.floor().max(0.0) as usize;
        let x1 = ((x + w).ceil().max(0.0) as usize).min(self.width);
        let y1 = ((y + h).ceil().max(0.0) as usize).min(self.height);
        for py in y0..y1 {
            for px in x0..x1 {
                let ring = (outer.coverage(px, py) - inner.coverage(px, py)).max(0.0);
                self.blend(px, py, color, ring * alpha);
            }
        }
    }

    /// Ring stroke centred on `radius`. `fraction` sweeps clockwise from 12 o'clock with
    /// round caps; `1.0` is a closed ring; `0.0` draws nothing.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_arc(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        width: f32,
        fraction: f32,
        color: Rgb,
        alpha: f32,
    ) {
        self.stroke_arc_from(cx, cy, radius, width, 0.0, fraction, color, alpha);
    }

    /// `stroke_arc` starting at `start` turns clockwise from 12 o'clock (`0.25` is 3 o'clock).
    /// The spinning activity arc uses it; `start` of `0.0` is `stroke_arc`.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_arc_from(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        width: f32,
        start: f32,
        fraction: f32,
        color: Rgb,
        alpha: f32,
    ) {
        let sweep = fraction.clamp(0.0, 1.0);
        if sweep <= 0.0 {
            return;
        }
        let half = width / 2.0;
        let closed = sweep >= 0.999;
        let start_angle = start.rem_euclid(1.0) * TAU;
        let end_angle = sweep * TAU;
        let start_cap = (
            cx + radius * start_angle.sin(),
            cy - radius * start_angle.cos(),
        );
        let end_cap = (
            cx + radius * (start_angle + end_angle).sin(),
            cy - radius * (start_angle + end_angle).cos(),
        );
        let reach = radius + half + 1.0;
        let x0 = (cx - reach).floor().max(0.0) as usize;
        let y0 = (cy - reach).floor().max(0.0) as usize;
        let x1 = ((cx + reach).ceil().max(0.0) as usize).min(self.width);
        let y1 = ((cy + reach).ceil().max(0.0) as usize).min(self.height);
        for py in y0..y1 {
            for px in x0..x1 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let (dx, dy) = (fx - cx, fy - cy);
                let radial = (0.5 - ((dx.hypot(dy) - radius).abs() - half)).clamp(0.0, 1.0);
                let coverage = if closed {
                    radial
                } else {
                    // Angle from the arc's own start, clockwise.
                    let theta = (dx.atan2(-dy) - start_angle).rem_euclid(TAU);
                    let along_start = theta * radius;
                    let along_end = (end_angle - theta) * radius;
                    let body = (0.5 + along_start.min(along_end)).clamp(0.0, 1.0) * radial;
                    let cap =
                        |c: (f32, f32)| (0.5 - ((fx - c.0).hypot(fy - c.1) - half)).clamp(0.0, 1.0);
                    body.max(cap(start_cap)).max(cap(end_cap))
                };
                self.blend(px, py, color, coverage * alpha);
            }
        }
    }

    /// `stroke_arc` broken into dashes of `on` then `off` along the arc, with square ends (the
    /// Mac's dashed stroke with butt caps). The first dash starts at 12 o'clock.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_arc_dashed(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        width: f32,
        fraction: f32,
        (on, off): (f32, f32),
        color: Rgb,
        alpha: f32,
    ) {
        let sweep = fraction.clamp(0.0, 1.0);
        if sweep <= 0.0 {
            return;
        }
        let half = width / 2.0;
        let end_angle = sweep * TAU;
        let period = (on + off).max(f32::EPSILON);
        let reach = radius + half + 1.0;
        let x0 = (cx - reach).floor().max(0.0) as usize;
        let y0 = (cy - reach).floor().max(0.0) as usize;
        let x1 = ((cx + reach).ceil().max(0.0) as usize).min(self.width);
        let y1 = ((cy + reach).ceil().max(0.0) as usize).min(self.height);
        for py in y0..y1 {
            for px in x0..x1 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let (dx, dy) = (fx - cx, fy - cy);
                let radial = (0.5 - ((dx.hypot(dy) - radius).abs() - half)).clamp(0.0, 1.0);
                if radial <= 0.0 {
                    continue;
                }
                let mut theta = dx.atan2(-dy);
                if theta < 0.0 {
                    theta += TAU;
                }
                // How far inside its dash the pixel is, along the arc (negative in a gap).
                let phase = (theta * radius).rem_euclid(period);
                let dash = (phase.min(on - phase) + 0.5).clamp(0.0, 1.0);
                // Nothing past the sweep's end.
                let end = ((end_angle - theta) * radius + 0.5).clamp(0.0, 1.0);
                self.blend(px, py, color, radial * dash * end * alpha);
            }
        }
    }

    /// Even-odd fill of one or more closed loops (device pixels), anti-aliased with eight
    /// sub-scanlines per row and exact horizontal coverage.
    // Coverage is accumulated per pixel column; the index is the pixel position.
    #[allow(clippy::needless_range_loop)]
    pub fn fill_polygon(&mut self, loops: &[Vec<(f32, f32)>], color: Rgb, alpha: f32) {
        const SUB: usize = 8;
        let (mut top, mut bottom) = (f32::MAX, f32::MIN);
        for point in loops.iter().flatten() {
            top = top.min(point.1);
            bottom = bottom.max(point.1);
        }
        if top > bottom {
            return;
        }
        let first_row = top.floor().max(0.0) as usize;
        let last_row = (bottom.ceil().max(0.0) as usize).min(self.height);
        let mut cover = vec![0.0f32; self.width];
        let mut crossings: Vec<f32> = Vec::new();
        for row in first_row..last_row {
            cover.fill(0.0);
            for sub in 0..SUB {
                let y = row as f32 + (sub as f32 + 0.5) / SUB as f32;
                crossings.clear();
                for outline in loops {
                    for (i, &(ax, ay)) in outline.iter().enumerate() {
                        let (bx, by) = outline[(i + 1) % outline.len()];
                        if (ay <= y) != (by <= y) {
                            crossings.push(ax + (y - ay) * (bx - ax) / (by - ay));
                        }
                    }
                }
                crossings.sort_by(f32::total_cmp);
                for pair in crossings.as_chunks::<2>().0 {
                    let left = pair[0].max(0.0);
                    let right = pair[1].min(self.width as f32);
                    if right <= left {
                        continue;
                    }
                    let last = (right.ceil() as usize).min(self.width);
                    for column in left.floor() as usize..last {
                        let lo = left.max(column as f32);
                        let hi = right.min(column as f32 + 1.0);
                        if hi > lo {
                            cover[column] += (hi - lo) / SUB as f32;
                        }
                    }
                }
            }
            for (column, &amount) in cover.iter().enumerate() {
                if amount > 0.0 {
                    self.blend(column, row, color, amount * alpha);
                }
            }
        }
    }

    /// Stroked polyline with round joins and caps (`closed` joins the last point to the
    /// first). Coverage comes from each pixel's distance to the nearest segment.
    pub fn stroke_polyline(
        &mut self,
        points: &[(f32, f32)],
        width: f32,
        closed: bool,
        color: Rgb,
        alpha: f32,
    ) {
        if points.is_empty() {
            return;
        }
        let half = width / 2.0;
        let mut segments: Vec<((f32, f32), (f32, f32))> =
            points.windows(2).map(|pair| (pair[0], pair[1])).collect();
        if closed && points.len() > 2 {
            segments.push((points[points.len() - 1], points[0]));
        }
        if segments.is_empty() {
            segments.push((points[0], points[0]));
        }
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for point in points {
            min_x = min_x.min(point.0);
            min_y = min_y.min(point.1);
            max_x = max_x.max(point.0);
            max_y = max_y.max(point.1);
        }
        let reach = half + 1.0;
        let x0 = (min_x - reach).floor().max(0.0) as usize;
        let y0 = (min_y - reach).floor().max(0.0) as usize;
        let x1 = ((max_x + reach).ceil().max(0.0) as usize).min(self.width);
        let y1 = ((max_y + reach).ceil().max(0.0) as usize).min(self.height);
        for py in y0..y1 {
            for px in x0..x1 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let mut nearest = f32::MAX;
                for &((ax, ay), (bx, by)) in &segments {
                    let (dx, dy) = (bx - ax, by - ay);
                    let length_sq = dx * dx + dy * dy;
                    let t = if length_sq > 0.0 {
                        (((fx - ax) * dx + (fy - ay) * dy) / length_sq).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    nearest = nearest.min((fx - (ax + t * dx)).hypot(fy - (ay + t * dy)));
                }
                self.blend(
                    px,
                    py,
                    color,
                    (0.5 + half - nearest).clamp(0.0, 1.0) * alpha,
                );
            }
        }
    }

    /// Composites a premultiplied `0xAARRGGBB` image (`source_width` x `source_height`),
    /// scaled bilinearly to the rectangle `(x, y, w, h)`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_image(
        &mut self,
        source: &[u32],
        (source_width, source_height): (usize, usize),
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    ) {
        if source_width == 0 || source_height == 0 || source.len() < source_width * source_height {
            return;
        }
        let x0 = x.floor().max(0.0) as usize;
        let y0 = y.floor().max(0.0) as usize;
        let x1 = ((x + w).ceil().max(0.0) as usize).min(self.width);
        let y1 = ((y + h).ceil().max(0.0) as usize).min(self.height);
        let at = |column: usize, row: usize| -> [f32; 4] {
            let pixel = source[row * source_width + column];
            [
                ((pixel >> 24) & 0xFF) as f32,
                ((pixel >> 16) & 0xFF) as f32,
                ((pixel >> 8) & 0xFF) as f32,
                (pixel & 0xFF) as f32,
            ]
        };
        for py in y0..y1 {
            for px in x0..x1 {
                let u = ((px as f32 + 0.5 - x) / w * source_width as f32 - 0.5)
                    .clamp(0.0, (source_width - 1) as f32);
                let v = ((py as f32 + 0.5 - y) / h * source_height as f32 - 0.5)
                    .clamp(0.0, (source_height - 1) as f32);
                let (cu, cv) = (u.floor() as usize, v.floor() as usize);
                let (nu, nv) = (
                    (cu + 1).min(source_width - 1),
                    (cv + 1).min(source_height - 1),
                );
                let (fu, fv) = (u - cu as f32, v - cv as f32);
                let (a, b, c, d) = (at(cu, cv), at(nu, cv), at(cu, nv), at(nu, nv));
                let mut out = [0.0f32; 4];
                for (i, slot) in out.iter_mut().enumerate() {
                    let top = a[i] + (b[i] - a[i]) * fu;
                    let bottom = c[i] + (d[i] - c[i]) * fu;
                    *slot = top + (bottom - top) * fv;
                }
                // Source-over of a premultiplied pixel.
                let index = py * self.width + px;
                let dst = self.pixels[index];
                let inv = 1.0 - out[0] / 255.0;
                let mix = |source: f32, shift: u32| -> u32 {
                    let d = ((dst >> shift) & 0xFF) as f32;
                    (source + d * inv).round().clamp(0.0, 255.0) as u32
                };
                self.pixels[index] = (mix(out[0], 24) << 24)
                    | (mix(out[1], 16) << 16)
                    | (mix(out[2], 8) << 8)
                    | mix(out[3], 0);
            }
        }
    }

    /// Composites a text mask with its top-left corner at `(x, y)`.
    pub fn draw_mask(&mut self, mask: &Mask, x: i32, y: i32, color: Rgb, alpha: f32) {
        for row in 0..mask.height {
            let py = y + row as i32;
            if py < 0 {
                continue;
            }
            for column in 0..mask.width {
                let px = x + column as i32;
                if px < 0 {
                    continue;
                }
                let level = mask.coverage[row * mask.width + column];
                if level != 0 {
                    self.blend(
                        px as usize,
                        py as usize,
                        color,
                        f32::from(level) / 255.0 * alpha,
                    );
                }
            }
        }
    }
}
