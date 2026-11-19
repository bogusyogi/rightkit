//! Backend-neutral drawing primitives: colour, rectangle, the display list (`DrawOp`) a painter
//! returns, and the stroke helpers that build common shapes out of fills.
//!
//! Everything here is plain data and arithmetic, so it compiles and tests on every host. Units are
//! points (one logical pixel at 100 % scaling); the Windows backend replays a list unscaled into
//! the overlay's logical coordinate space. Nothing here knows a product, a colour scheme, a size
//! or a motion curve: painters supply those.

#![allow(dead_code)]

use crate::text::Face;

// ---------------------------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------------------------

/// 8-bit sRGB colour with a floating alpha in `0.0..=1.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: f32,
}

impl Rgba {
    pub const fn rgb(hex: u32) -> Self {
        Rgba {
            r: ((hex >> 16) & 0xff) as u8,
            g: ((hex >> 8) & 0xff) as u8,
            b: (hex & 0xff) as u8,
            a: 1.0,
        }
    }

    pub const fn new(r: u8, g: u8, b: u8, a: f32) -> Self {
        Rgba { r, g, b, a }
    }

    pub fn with_alpha(self, a: f32) -> Self {
        Rgba { a, ..self }
    }

    /// Multiply the alpha (an opacity stacked on an existing colour).
    pub fn mul_alpha(self, k: f32) -> Self {
        Rgba {
            a: (self.a * k).clamp(0.0, 1.0),
            ..self
        }
    }

    /// Linear blend of colour channels and alpha.
    pub fn lerp(self, other: Rgba, t: f32) -> Rgba {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        Rgba {
            r: mix(self.r, other.r),
            g: mix(self.g, other.g),
            b: mix(self.b, other.b),
            a: self.a + (other.a - self.a) * t,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Rect
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl VRect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        VRect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn cx(&self) -> f32 {
        self.x + self.w * 0.5
    }
    pub fn cy(&self) -> f32 {
        self.y + self.h * 0.5
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.right() && py >= self.y && py <= self.bottom()
    }
    /// Shrink by `d` on every side (negative grows).
    pub fn inset(&self, d: f32) -> VRect {
        VRect {
            x: self.x + d,
            y: self.y + d,
            w: (self.w - 2.0 * d).max(0.0),
            h: (self.h - 2.0 * d).max(0.0),
        }
    }
    pub fn centered(cx: f32, cy: f32, w: f32, h: f32) -> VRect {
        VRect {
            x: cx - w * 0.5,
            y: cy - h * 0.5,
            w,
            h,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Display list
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathOp {
    Move(f32, f32),
    Line(f32, f32),
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DrawOp {
    FillRRect {
        rect: VRect,
        radius: f32,
        color: Rgba,
    },
    /// Stroke centred on the rounded-rect outline of `rect`.
    StrokeRRect {
        rect: VRect,
        radius: f32,
        width: f32,
        color: Rgba,
    },
    /// Convex polygon fill.
    Polygon { pts: Vec<(f32, f32)>, color: Rgba },
    /// One line of text. `baseline` is the y of the text baseline; the renderer draws the glyph
    /// box (`ascent + descent`) from `baseline - ascent` inside `[x, x + w]`.
    Text {
        text: String,
        face: Face,
        size: f32,
        color: Rgba,
        x: f32,
        baseline: f32,
        w: f32,
        align: Align,
    },
    /// Clip every later op to this rectangle.
    Clip(VRect),
}

impl DrawOp {
    /// The top of the glyph box a `Text` op asks the renderer to fill.
    pub fn text_box(&self) -> Option<VRect> {
        match self {
            DrawOp::Text {
                face,
                size,
                x,
                baseline,
                w,
                ..
            } => Some(VRect::new(
                *x,
                baseline - face.ascent_em() * size,
                *w,
                face.glyph_box(*size),
            )),
            _ => None,
        }
    }
}

/// Text op whose line box (`line_height`) is centred on `cy`.
#[allow(clippy::too_many_arguments)] // Preserve the painter API used by app-owned drawing code.
pub fn text_centered(
    text: &str,
    face: Face,
    size: f32,
    color: Rgba,
    x: f32,
    cy: f32,
    w: f32,
    align: Align,
) -> DrawOp {
    let lh = face.line_height(size);
    DrawOp::Text {
        text: text.to_string(),
        face,
        size,
        color,
        x,
        baseline: cy - lh * 0.5 + face.ascent_em() * size,
        w,
        align,
    }
}

/// Text op whose line box top is `top` (multi-line paragraphs).
#[allow(clippy::too_many_arguments)] // Preserve the painter API used by app-owned drawing code.
pub fn text_at_top(
    text: &str,
    face: Face,
    size: f32,
    color: Rgba,
    x: f32,
    top: f32,
    w: f32,
    align: Align,
) -> DrawOp {
    DrawOp::Text {
        text: text.to_string(),
        face,
        size,
        color,
        x,
        baseline: top + face.ascent_em() * size,
        w,
        align,
    }
}

fn map_pt(x: f32, y: f32, cx: f32, cy: f32, k: f32, dy: f32) -> (f32, f32) {
    (cx + (x - cx) * k, cy + (y - cy) * k + dy)
}

fn map_rect(r: VRect, cx: f32, cy: f32, k: f32, dy: f32) -> VRect {
    let (x, y) = map_pt(r.x, r.y, cx, cy, k, dy);
    VRect::new(x, y, r.w * k, r.h * k)
}

/// Scale every op's alpha by `alpha` and nothing else (a fade).
pub fn fade_ops(ops: &[DrawOp], alpha: f32) -> Vec<DrawOp> {
    transform_ops(ops, 0.0, 0.0, 1.0, alpha, 0.0)
}

/// Scale about `(cx, cy)` by `k`, shift by `dy`, and multiply every alpha by `alpha`.
/// That is a scale about the centre, an opacity and a vertical offset, in that order.
pub fn transform_ops(ops: &[DrawOp], cx: f32, cy: f32, k: f32, alpha: f32, dy: f32) -> Vec<DrawOp> {
    ops.iter()
        .map(|op| match op {
            DrawOp::FillRRect {
                rect,
                radius,
                color,
            } => DrawOp::FillRRect {
                rect: map_rect(*rect, cx, cy, k, dy),
                radius: radius * k,
                color: color.mul_alpha(alpha),
            },
            DrawOp::StrokeRRect {
                rect,
                radius,
                width,
                color,
            } => DrawOp::StrokeRRect {
                rect: map_rect(*rect, cx, cy, k, dy),
                radius: radius * k,
                width: width * k,
                color: color.mul_alpha(alpha),
            },
            DrawOp::Polygon { pts, color } => DrawOp::Polygon {
                pts: pts
                    .iter()
                    .map(|(x, y)| map_pt(*x, *y, cx, cy, k, dy))
                    .collect(),
                color: color.mul_alpha(alpha),
            },
            DrawOp::Text {
                text,
                face,
                size,
                color,
                x,
                baseline,
                w,
                align,
            } => {
                let (nx, nb) = map_pt(*x, *baseline, cx, cy, k, dy);
                DrawOp::Text {
                    text: text.clone(),
                    face: *face,
                    size: size * k,
                    color: color.mul_alpha(alpha),
                    x: nx,
                    baseline: nb,
                    w: w * k,
                    align: *align,
                }
            }
            DrawOp::Clip(r) => DrawOp::Clip(map_rect(*r, cx, cy, k, dy)),
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Stroke helpers
// ---------------------------------------------------------------------------------------------

/// A filled circle as a rounded rectangle.
pub fn circle_op(cx: f32, cy: f32, r: f32, color: Rgba) -> DrawOp {
    DrawOp::FillRRect {
        rect: VRect::centered(cx, cy, r * 2.0, r * 2.0),
        radius: r,
        color,
    }
}

/// A round-capped line as a quad plus two discs (each its own fill, so no fill rule matters).
pub fn stroke_line(ops: &mut Vec<DrawOp>, a: (f32, f32), b: (f32, f32), width: f32, color: Rgba) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt();
    let r = width * 0.5;
    if len > 0.001 {
        let (nx, ny) = (-dy / len * r, dx / len * r);
        ops.push(DrawOp::Polygon {
            pts: vec![
                (a.0 + nx, a.1 + ny),
                (b.0 + nx, b.1 + ny),
                (b.0 - nx, b.1 - ny),
                (a.0 - nx, a.1 - ny),
            ],
            color,
        });
    }
    ops.push(circle_op(a.0, a.1, r, color));
    ops.push(circle_op(b.0, b.1, r, color));
}

pub fn stroke_poly(ops: &mut Vec<DrawOp>, pts: &[(f32, f32)], width: f32, color: Rgba) {
    for pair in pts.windows(2) {
        stroke_line(ops, pair[0], pair[1], width, color);
    }
}

/// Most segments one [`arc_pts`] call produces, whatever angles it is given.
pub const MAX_ARC_STEPS: usize = 256;

/// Points along a circular arc; angles in radians, y-down screen space, increasing = clockwise.
/// A non-finite angle gives the minimum arc and the segment count is capped at
/// [`MAX_ARC_STEPS`], so the allocation never grows with the angles.
pub fn arc_pts(cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> Vec<(f32, f32)> {
    let span = (a1 - a0).abs();
    let span = if span.is_finite() { span } else { 0.0 };
    let steps = ((span / (std::f32::consts::PI / 14.0)).ceil() as usize).clamp(2, MAX_ARC_STEPS);
    (0..=steps)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / steps as f32;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}
