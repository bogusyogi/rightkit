//! The clipboard kinds' outline marks (the Mac's SF Symbols for them): the launcher's rows and
//! the strip's card headers draw them. Drawn in a unit square (-1 to 1 each way) about a centre.

use crate::model::Kind;
use rightkit_win_ui::canvas::Canvas;
use rightkit_win_ui::glyphs::arc;

type Point = (f32, f32);

/// Which mark: the launcher row's (`doc.text` for text, `doc.richtext` for rich) or the card
/// header's (`text.alignleft` for text; rich draws its own "Aa").
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Row,
    Header,
}

struct Pen<'a> {
    canvas: &'a mut Canvas,
    centre: Point,
    half: f32,
    width: f32,
    ink: u32,
    alpha: f32,
}

impl Pen<'_> {
    fn at(&self, (x, y): Point) -> Point {
        (self.centre.0 + x * self.half, self.centre.1 + y * self.half)
    }

    fn path(&mut self, points: &[Point], closed: bool) {
        let placed: Vec<Point> = points.iter().map(|&p| self.at(p)).collect();
        self.canvas
            .stroke_polyline(&placed, self.width, closed, self.ink, self.alpha);
    }

    fn dot(&mut self, at: Point, radius: f32) {
        let (x, y) = self.at(at);
        let r = radius * self.half;
        self.canvas
            .fill_round_rect(x - r, y - r, 2.0 * r, 2.0 * r, [r; 4], self.ink, self.alpha);
    }
}

/// A rounded rectangle's outline about `middle`, corners of radius `r`.
fn rounded(half: Point, r: f32, middle: Point) -> Vec<Point> {
    let (hx, hy) = half;
    let mut out = arc((middle.0 - hx + r, middle.1 - hy + r), r, 180.0, 270.0);
    out.extend(arc((middle.0 + hx - r, middle.1 - hy + r), r, 270.0, 360.0));
    out.extend(arc((middle.0 + hx - r, middle.1 + hy - r), r, 0.0, 90.0));
    out.extend(arc((middle.0 - hx + r, middle.1 + hy - r), r, 90.0, 180.0));
    out
}

/// A capsule `length` by `thickness` about `at`, turned by `degrees`.
fn capsule(at: Point, length: f32, thickness: f32, degrees: f32) -> Vec<Point> {
    let (sin, cos) = degrees.to_radians().sin_cos();
    rounded((length / 2.0, thickness / 2.0), thickness / 2.0, (0.0, 0.0))
        .into_iter()
        .map(|(x, y)| (at.0 + x * cos - y * sin, at.1 + x * sin + y * cos))
        .collect()
}

/// A page with its top right corner folded: the outline, then the fold's two edges.
fn page(pen: &mut Pen) {
    let (left, right, top, bottom, cut) = (-0.58, 0.58, -0.86, 0.86, 0.5);
    let r = 0.2;
    let mut outline = arc((left + r, top + r), r, 180.0, 270.0);
    outline.push((right - cut, top));
    outline.push((right, top + cut));
    outline.extend(arc((right - r, bottom - r), r, 0.0, 90.0));
    outline.extend(arc((left + r, bottom - r), r, 90.0, 180.0));
    pen.path(&outline, true);
    pen.path(
        &[
            (right - cut, top),
            (right - cut, top + cut),
            (right, top + cut),
        ],
        false,
    );
}

/// The mark for `kind` at `place`, `size` device pixels square about `centre`.
pub fn draw(
    canvas: &mut Canvas,
    kind: Kind,
    place: Place,
    centre: Point,
    size: f32,
    (ink, alpha): (u32, f32),
) {
    let mut pen = Pen {
        canvas,
        centre,
        half: size / 2.0,
        width: (size * 0.085).max(1.0),
        ink,
        alpha,
    };
    match kind {
        Kind::Text if place == Place::Header => {
            for (y, end) in [(-0.62, 0.9), (-0.21, 0.5), (0.21, 0.9), (0.62, 0.3)] {
                pen.path(&[(-0.9, y), (end, y)], false);
            }
        }
        Kind::Text | Kind::Rich => {
            page(&mut pen);
            for (y, end) in [(0.0, 0.3), (0.3, 0.3), (0.6, 0.0)] {
                pen.path(&[(-0.3, y), (end, y)], false);
            }
        }
        Kind::File => page(&mut pen),
        Kind::Link => {
            // Two rings, end to end along the diagonal.
            pen.path(&capsule((-0.22, 0.22), 0.98, 0.5, -45.0), true);
            pen.path(&capsule((0.22, -0.22), 0.98, 0.5, -45.0), true);
        }
        Kind::Colour => {
            // paintpalette: a rounded blob with a thumb notch, a hole and four dabs of paint.
            pen.path(
                &[
                    (-1.0, 0.0),
                    (-0.92, -0.38),
                    (-0.68, -0.66),
                    (-0.3, -0.8),
                    (0.15, -0.82),
                    (0.6, -0.74),
                    (0.92, -0.52),
                    (1.0, -0.3),
                    (0.9, -0.1),
                    (0.62, 0.04),
                    (0.5, 0.2),
                    (0.52, 0.45),
                    (0.62, 0.68),
                    (0.5, 0.84),
                    (0.2, 0.86),
                    (-0.25, 0.8),
                    (-0.65, 0.6),
                    (-0.92, 0.32),
                ],
                true,
            );
            pen.path(&arc((0.0, 0.26), 0.17, 0.0, 360.0), true);
            for (at, radius) in [
                ((-0.28, -0.33), 0.15),
                ((0.16, -0.45), 0.1),
                ((0.55, -0.34), 0.09),
                ((-0.6, 0.0), 0.1),
            ] {
                pen.dot(at, radius);
            }
        }
        Kind::Code => {
            pen.path(&[(-0.38, -0.46), (-0.86, 0.0), (-0.38, 0.46)], false);
            pen.path(&[(0.38, -0.46), (0.86, 0.0), (0.38, 0.46)], false);
            pen.path(&[(0.14, -0.6), (-0.14, 0.6)], false);
        }
        Kind::Image => {
            pen.path(&rounded((0.95, 0.74), 0.22, (0.0, 0.0)), true);
            pen.dot((-0.42, -0.26), 0.14);
            pen.path(
                &[
                    (-0.95, 0.5),
                    (-0.35, -0.04),
                    (0.1, 0.36),
                    (0.4, 0.08),
                    (0.95, 0.52),
                ],
                false,
            );
        }
    }
}

/// `doc.fill`: the solid page with its folded corner set off by a thin gap, `height` device
/// pixels tall about `centre` (the large mark of a file card without its own icon).
pub fn page_fill(canvas: &mut Canvas, centre: Point, height: f32, (ink, alpha): (u32, f32)) {
    // Drawn in points on a 27 by 36 page, then scaled.
    let unit = height / 36.0;
    let at = |(x, y): Point| (centre.0 + (x - 13.5) * unit, centre.1 + (y - 18.0) * unit);
    let r = 2.7;
    let mut body: Vec<Point> = vec![(12.5, 0.0), (12.5, 11.2)];
    body.extend([(12.8, 12.2), (13.6, 12.8), (14.4, 12.9), (27.0, 12.9)]);
    body.extend(arc((27.0 - r, 36.0 - r), r, 0.0, 90.0));
    body.extend(arc((r, 36.0 - r), r, 90.0, 180.0));
    body.extend(arc((r, r), r, 180.0, 270.0));
    let body: Vec<Point> = body.into_iter().map(at).collect();
    let fold: Vec<Point> = [(14.4, 0.0), (27.0, 11.4), (14.4, 11.4)]
        .into_iter()
        .map(at)
        .collect();
    canvas.fill_polygon(&[body], ink, alpha);
    canvas.fill_polygon(&[fold], ink, alpha);
}
