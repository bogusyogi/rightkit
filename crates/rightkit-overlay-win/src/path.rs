//! Vector paths for icons: a command list, an SVG path-data parser (arcs become cubics) and the
//! flattener the rasteriser uses. Coordinates are plain numbers; `PaintCtx` scales them.

use std::f32::consts::PI;

/// One path command, in the units of the caller (logical pixels when handed to `PaintCtx`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathCmd {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    /// Control point, end point.
    QuadTo(f32, f32, f32, f32),
    /// First control, second control, end point.
    CubicTo(f32, f32, f32, f32, f32, f32),
    /// Straight line back to the start of the current subpath, and close it.
    Close,
}

/// A rectangle with circular corners as path commands.
pub fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Vec<PathCmd> {
    let r = radius.min(w * 0.5).min(h * 0.5).max(0.0);
    let k = 0.552_284_8 * r;
    vec![
        PathCmd::MoveTo(x + r, y),
        PathCmd::LineTo(x + w - r, y),
        PathCmd::CubicTo(x + w - r + k, y, x + w, y + r - k, x + w, y + r),
        PathCmd::LineTo(x + w, y + h - r),
        PathCmd::CubicTo(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h),
        PathCmd::LineTo(x + r, y + h),
        PathCmd::CubicTo(x + r - k, y + h, x, y + h - r + k, x, y + h - r),
        PathCmd::LineTo(x, y + r),
        PathCmd::CubicTo(x, y + r - k, x + r - k, y, x + r, y),
        PathCmd::Close,
    ]
}

/// The commands scaled by `scale` about the origin, then moved by `(dx, dy)`. Use it to place a
/// 24-unit icon: `transform(&icon, size / 24.0, left, top)`.
pub fn transform(cmds: &[PathCmd], scale: f32, dx: f32, dy: f32) -> Vec<PathCmd> {
    let p = |x: f32, y: f32| (x * scale + dx, y * scale + dy);
    cmds.iter()
        .map(|cmd| match *cmd {
            PathCmd::MoveTo(x, y) => {
                let (x, y) = p(x, y);
                PathCmd::MoveTo(x, y)
            }
            PathCmd::LineTo(x, y) => {
                let (x, y) = p(x, y);
                PathCmd::LineTo(x, y)
            }
            PathCmd::QuadTo(cx, cy, x, y) => {
                let (cx, cy) = p(cx, cy);
                let (x, y) = p(x, y);
                PathCmd::QuadTo(cx, cy, x, y)
            }
            PathCmd::CubicTo(c1x, c1y, c2x, c2y, x, y) => {
                let (c1x, c1y) = p(c1x, c1y);
                let (c2x, c2y) = p(c2x, c2y);
                let (x, y) = p(x, y);
                PathCmd::CubicTo(c1x, c1y, c2x, c2y, x, y)
            }
            PathCmd::Close => PathCmd::Close,
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum Token {
    Cmd(char),
    Num(f32),
}

fn tokenize(d: &str) -> Vec<Token> {
    let bytes = d.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_alphabetic() && c != 'e' && c != 'E' {
            out.push(Token::Cmd(c));
            i += 1;
        } else if c.is_ascii_digit() || c == '-' || c == '+' || c == '.' {
            let start = i;
            let mut seen_dot = c == '.';
            i += 1;
            while i < bytes.len() {
                let cc = bytes[i] as char;
                if cc.is_ascii_digit() {
                    i += 1;
                } else if cc == '.' && !seen_dot {
                    seen_dot = true;
                    i += 1;
                } else if (cc == 'e' || cc == 'E')
                    && i + 1 < bytes.len()
                    && ((bytes[i + 1] as char).is_ascii_digit()
                        || ((bytes[i + 1] as char == '-' || bytes[i + 1] as char == '+')
                            && i + 2 < bytes.len()
                            && (bytes[i + 2] as char).is_ascii_digit()))
                {
                    i += 2;
                } else {
                    break;
                }
            }
            if let Ok(n) = d[start..i].parse::<f32>() {
                out.push(Token::Num(n));
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Parses SVG path data: `M m L l H h V v C c Q q A a Z z`, with implicit command repeats. Smooth
/// curves (`S`, `T`) are not supported; parsing stops at the first unsupported command, keeping
/// what came before. Arcs are converted to cubic segments.
pub fn parse_svg_path(d: &str) -> Vec<PathCmd> {
    let tokens = tokenize(d);
    let mut out = Vec::new();
    let mut i = 0;
    let mut cmd = ' ';
    let (mut cx, mut cy) = (0.0f32, 0.0f32);
    let (mut sx, mut sy) = (0.0f32, 0.0f32);

    fn num(tokens: &[Token], i: &mut usize) -> Option<f32> {
        if let Some(Token::Num(n)) = tokens.get(*i) {
            *i += 1;
            Some(*n)
        } else {
            None
        }
    }
    macro_rules! n {
        () => {
            match num(&tokens, &mut i) {
                Some(v) => v,
                None => return out,
            }
        };
    }

    while i < tokens.len() {
        if let Token::Cmd(c) = tokens[i] {
            cmd = c;
            i += 1;
            if matches!(cmd, 'Z' | 'z') {
                out.push(PathCmd::Close);
                cx = sx;
                cy = sy;
                continue;
            }
        }
        match cmd {
            'M' | 'm' => {
                let (x, y) = (n!(), n!());
                if cmd == 'm' {
                    cx += x;
                    cy += y;
                } else {
                    cx = x;
                    cy = y;
                }
                sx = cx;
                sy = cy;
                out.push(PathCmd::MoveTo(cx, cy));
                // Further coordinate pairs after a move are implicit line-tos.
                cmd = if cmd == 'm' { 'l' } else { 'L' };
            }
            'L' | 'l' => {
                let (x, y) = (n!(), n!());
                if cmd == 'l' {
                    cx += x;
                    cy += y;
                } else {
                    cx = x;
                    cy = y;
                }
                out.push(PathCmd::LineTo(cx, cy));
            }
            'H' | 'h' => {
                let x = n!();
                cx = if cmd == 'h' { cx + x } else { x };
                out.push(PathCmd::LineTo(cx, cy));
            }
            'V' | 'v' => {
                let y = n!();
                cy = if cmd == 'v' { cy + y } else { y };
                out.push(PathCmd::LineTo(cx, cy));
            }
            'C' | 'c' => {
                let (a, b, c, d2, e, f) = (n!(), n!(), n!(), n!(), n!(), n!());
                let (ox, oy) = if cmd == 'c' { (cx, cy) } else { (0.0, 0.0) };
                out.push(PathCmd::CubicTo(
                    ox + a,
                    oy + b,
                    ox + c,
                    oy + d2,
                    ox + e,
                    oy + f,
                ));
                cx = ox + e;
                cy = oy + f;
            }
            'Q' | 'q' => {
                let (a, b, c, d2) = (n!(), n!(), n!(), n!());
                let (ox, oy) = if cmd == 'q' { (cx, cy) } else { (0.0, 0.0) };
                out.push(PathCmd::QuadTo(ox + a, oy + b, ox + c, oy + d2));
                cx = ox + c;
                cy = oy + d2;
            }
            'A' | 'a' => {
                let (rx, ry, rot) = (n!(), n!(), n!());
                let (large, sweep) = (n!() != 0.0, n!() != 0.0);
                let (x, y) = (n!(), n!());
                let (ex, ey) = if cmd == 'a' { (cx + x, cy + y) } else { (x, y) };
                arc_to(&mut out, (cx, cy), (rx, ry), rot, large, sweep, (ex, ey));
                cx = ex;
                cy = ey;
            }
            _ => return out,
        }
    }
    out
}

/// SVG endpoint-parameter arc to cubic Béziers (SVG 1.1 implementation notes, F.6).
fn arc_to(
    out: &mut Vec<PathCmd>,
    (x0, y0): (f32, f32),
    (rx, ry): (f32, f32),
    rotation_deg: f32,
    large: bool,
    sweep: bool,
    (x, y): (f32, f32),
) {
    if rx.abs() < 1.0e-6 || ry.abs() < 1.0e-6 || (x0 == x && y0 == y) {
        out.push(PathCmd::LineTo(x, y));
        return;
    }
    let phi = rotation_deg.to_radians();
    let (cos_p, sin_p) = (phi.cos(), phi.sin());
    let dx = (x0 - x) / 2.0;
    let dy = (y0 - y) / 2.0;
    let x1p = cos_p * dx + sin_p * dy;
    let y1p = -sin_p * dx + cos_p * dy;
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    let lambda = x1p * x1p / (rx * rx) + y1p * y1p / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let sign = if large != sweep { 1.0 } else { -1.0 };
    let numerator = (rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p).max(0.0);
    let denominator = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let coefficient = if denominator > 0.0 {
        sign * (numerator / denominator).sqrt()
    } else {
        0.0
    };
    let cxp = coefficient * rx * y1p / ry;
    let cyp = coefficient * -ry * x1p / rx;
    let cx = cos_p * cxp - sin_p * cyp + (x0 + x) / 2.0;
    let cy = sin_p * cxp + cos_p * cyp + (y0 + y) / 2.0;
    let angle = |ux: f32, uy: f32, vx: f32, vy: f32| -> f32 {
        let dot = ux * vx + uy * vy;
        let len = ((ux * ux + uy * uy) * (vx * vx + vy * vy)).sqrt();
        let mut a = (dot / len).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 {
            a = -a;
        }
        a
    };
    let theta1 = angle(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut delta_theta = angle(
        (x1p - cxp) / rx,
        (y1p - cyp) / ry,
        (-x1p - cxp) / rx,
        (-y1p - cyp) / ry,
    );
    if !sweep && delta_theta > 0.0 {
        delta_theta -= 2.0 * PI;
    } else if sweep && delta_theta < 0.0 {
        delta_theta += 2.0 * PI;
    }
    // The small bias keeps an exact half or full turn from rounding up to an extra segment.
    let segments = ((delta_theta.abs() / (PI / 2.0)) - 1.0e-4).ceil().max(1.0) as usize;
    let delta = delta_theta / segments as f32;
    let alpha = 4.0 / 3.0 * (delta / 4.0).tan();
    let point = |theta: f32| -> (f32, f32) {
        let (ex, ey) = (rx * theta.cos(), ry * theta.sin());
        (cos_p * ex - sin_p * ey + cx, sin_p * ex + cos_p * ey + cy)
    };
    let derivative = |theta: f32| -> (f32, f32) {
        let (ex, ey) = (-rx * theta.sin(), ry * theta.cos());
        (cos_p * ex - sin_p * ey, sin_p * ex + cos_p * ey)
    };
    let mut theta = theta1;
    for index in 0..segments {
        let next = theta + delta;
        let (p1x, p1y) = point(theta);
        let (d1x, d1y) = derivative(theta);
        // The last segment ends exactly on the requested end point.
        let (p2x, p2y) = if index + 1 == segments {
            (x, y)
        } else {
            point(next)
        };
        let (d2x, d2y) = derivative(next);
        out.push(PathCmd::CubicTo(
            p1x + alpha * d1x,
            p1y + alpha * d1y,
            p2x - alpha * d2x,
            p2y - alpha * d2y,
            p2x,
            p2y,
        ));
        theta = next;
    }
}

/// One flattened subpath in device pixels.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Subpath {
    pub points: Vec<(f32, f32)>,
    pub closed: bool,
}

/// Flattens commands to polylines in device pixels (`scale` converts the command units). Curves
/// are subdivided by their control-polygon length, about one point per 2 device pixels.
pub(crate) fn flatten(cmds: &[PathCmd], scale: f32) -> Vec<Subpath> {
    let mut out: Vec<Subpath> = Vec::new();
    let mut current: Option<Subpath> = None;
    let mut start = (0.0f32, 0.0f32);
    let mut at = (0.0f32, 0.0f32);

    fn finish(out: &mut Vec<Subpath>, current: &mut Option<Subpath>) {
        if let Some(sub) = current.take() {
            if !sub.points.is_empty() {
                out.push(sub);
            }
        }
    }
    let steps = |length: f32| ((length / 2.0).ceil() as usize).clamp(4, 64);

    for cmd in cmds {
        match *cmd {
            PathCmd::MoveTo(x, y) => {
                finish(&mut out, &mut current);
                at = (x * scale, y * scale);
                start = at;
                current = Some(Subpath {
                    points: vec![at],
                    closed: false,
                });
            }
            PathCmd::LineTo(x, y) => {
                let sub = current.get_or_insert_with(|| Subpath {
                    points: vec![at],
                    closed: false,
                });
                at = (x * scale, y * scale);
                sub.points.push(at);
            }
            PathCmd::QuadTo(cx, cy, x, y) => {
                let sub = current.get_or_insert_with(|| Subpath {
                    points: vec![at],
                    closed: false,
                });
                let c = (cx * scale, cy * scale);
                let e = (x * scale, y * scale);
                let length = dist(at, c) + dist(c, e);
                let n = steps(length);
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let k = 1.0 - t;
                    sub.points.push((
                        k * k * at.0 + 2.0 * k * t * c.0 + t * t * e.0,
                        k * k * at.1 + 2.0 * k * t * c.1 + t * t * e.1,
                    ));
                }
                at = e;
            }
            PathCmd::CubicTo(c1x, c1y, c2x, c2y, x, y) => {
                let sub = current.get_or_insert_with(|| Subpath {
                    points: vec![at],
                    closed: false,
                });
                let c1 = (c1x * scale, c1y * scale);
                let c2 = (c2x * scale, c2y * scale);
                let e = (x * scale, y * scale);
                let length = dist(at, c1) + dist(c1, c2) + dist(c2, e);
                let n = steps(length);
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let k = 1.0 - t;
                    sub.points.push((
                        k * k * k * at.0
                            + 3.0 * k * k * t * c1.0
                            + 3.0 * k * t * t * c2.0
                            + t * t * t * e.0,
                        k * k * k * at.1
                            + 3.0 * k * k * t * c1.1
                            + 3.0 * k * t * t * c2.1
                            + t * t * t * e.1,
                    ));
                }
                at = e;
            }
            PathCmd::Close => {
                if let Some(mut sub) = current.take() {
                    sub.closed = true;
                    if !sub.points.is_empty() {
                        out.push(sub);
                    }
                    at = start;
                    // A command after Close without a MoveTo starts at the old start point.
                }
            }
        }
    }
    finish(&mut out, &mut current);
    out
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_absolute_and_relative_lines() {
        let cmds = parse_svg_path("M 4.5 12.5 L 9.5 17.5 l 10 -10.5 Z");
        assert_eq!(
            cmds,
            vec![
                PathCmd::MoveTo(4.5, 12.5),
                PathCmd::LineTo(9.5, 17.5),
                PathCmd::LineTo(19.5, 7.0),
                PathCmd::Close,
            ]
        );
    }

    #[test]
    fn parses_compact_forms_hv_and_implicit_repeats() {
        // The old pill list icon: "M9 7 H19" and a move followed by implicit line-tos.
        assert_eq!(
            parse_svg_path("M9 7H19"),
            vec![PathCmd::MoveTo(9.0, 7.0), PathCmd::LineTo(19.0, 7.0)]
        );
        assert_eq!(
            parse_svg_path("m1 1 2 3"),
            vec![PathCmd::MoveTo(1.0, 1.0), PathCmd::LineTo(3.0, 4.0)]
        );
        assert_eq!(
            parse_svg_path("M0 0v5h-3"),
            vec![
                PathCmd::MoveTo(0.0, 0.0),
                PathCmd::LineTo(0.0, 5.0),
                PathCmd::LineTo(-3.0, 5.0)
            ]
        );
        // A bare dot-number run splits like SVG does: "5.01.5" is 5.01 then .5.
        assert_eq!(parse_svg_path("M5.01.5"), vec![PathCmd::MoveTo(5.01, 0.5)]);
    }

    #[test]
    fn parses_curves_and_stops_at_unsupported_commands() {
        let cmds = parse_svg_path("M0 0 C 1 2 3 4 5 6 q1 1 2 2");
        assert_eq!(cmds[1], PathCmd::CubicTo(1.0, 2.0, 3.0, 4.0, 5.0, 6.0));
        assert_eq!(cmds[2], PathCmd::QuadTo(6.0, 7.0, 7.0, 8.0));
        // 'S' is not supported: what came before is kept.
        let cmds = parse_svg_path("M0 0 L1 1 S 2 2 3 3");
        assert_eq!(
            cmds,
            vec![PathCmd::MoveTo(0.0, 0.0), PathCmd::LineTo(1.0, 1.0)]
        );
        // Truncated numbers end the parse without panicking.
        assert!(parse_svg_path("M1").is_empty());
        assert!(parse_svg_path("").is_empty());
    }

    #[test]
    fn half_circle_arc_ends_on_the_target_and_stays_on_the_circle() {
        // The microphone cradle from the pill icon set: a half circle of radius 6 about (12, 11).
        let cmds = parse_svg_path("M18 11 a6 6 0 0 1 -12 0");
        assert_eq!(cmds[0], PathCmd::MoveTo(18.0, 11.0));
        let curves: Vec<_> = cmds[1..].to_vec();
        assert_eq!(curves.len(), 2, "a half turn is two quarter segments");
        let PathCmd::CubicTo(.., ex, ey) = *curves.last().unwrap() else {
            panic!("arc must become cubics");
        };
        assert!((ex - 6.0).abs() < 1.0e-4 && (ey - 11.0).abs() < 1.0e-4);
        // Sweep flag 1 runs clockwise on screen (y down): the middle of the arc is below the
        // centre, at (12, 17).
        let PathCmd::CubicTo(.., mx, my) = curves[0] else {
            panic!()
        };
        assert!(
            (mx - 12.0).abs() < 1.0e-3 && (my - 17.0).abs() < 1.0e-3,
            "{mx},{my}"
        );
    }

    #[test]
    fn degenerate_arc_is_a_line() {
        assert_eq!(
            parse_svg_path("M0 0 A0 5 0 0 1 10 10"),
            vec![PathCmd::MoveTo(0.0, 0.0), PathCmd::LineTo(10.0, 10.0)]
        );
    }

    #[test]
    fn flatten_scales_and_closes() {
        let cmds = vec![
            PathCmd::MoveTo(0.0, 0.0),
            PathCmd::LineTo(10.0, 0.0),
            PathCmd::LineTo(10.0, 10.0),
            PathCmd::Close,
        ];
        let subs = flatten(&cmds, 2.0);
        assert_eq!(subs.len(), 1);
        assert!(subs[0].closed);
        assert_eq!(subs[0].points, vec![(0.0, 0.0), (20.0, 0.0), (20.0, 20.0)]);
    }

    #[test]
    fn flatten_subdivides_curves_to_their_endpoint() {
        let cmds = vec![
            PathCmd::MoveTo(0.0, 0.0),
            PathCmd::CubicTo(0.0, 50.0, 100.0, 50.0, 100.0, 0.0),
        ];
        let subs = flatten(&cmds, 1.0);
        let points = &subs[0].points;
        assert!(points.len() > 8);
        let last = *points.last().unwrap();
        assert!((last.0 - 100.0).abs() < 1.0e-4 && last.1.abs() < 1.0e-4);
        // The curve bulges to y = 37.5 at its middle.
        let peak = points.iter().map(|p| p.1).fold(0.0f32, f32::max);
        assert!((peak - 37.5).abs() < 1.0, "{peak}");
    }

    #[test]
    fn several_subpaths_stay_separate() {
        let subs = flatten(&parse_svg_path("M0 0 H5 M10 10 H15"), 1.0);
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[1].points[0], (10.0, 10.0));
    }

    #[test]
    fn transform_moves_every_coordinate() {
        let cmds = vec![
            PathCmd::MoveTo(1.0, 1.0),
            PathCmd::QuadTo(2.0, 2.0, 3.0, 3.0),
            PathCmd::Close,
        ];
        assert_eq!(
            transform(&cmds, 2.0, 10.0, 20.0),
            vec![
                PathCmd::MoveTo(12.0, 22.0),
                PathCmd::QuadTo(14.0, 24.0, 16.0, 26.0),
                PathCmd::Close
            ]
        );
    }

    #[test]
    fn rounded_rect_path_is_closed_and_stays_inside() {
        let cmds = rounded_rect_path(10.0, 20.0, 100.0, 40.0, 50.0);
        assert_eq!(*cmds.last().unwrap(), PathCmd::Close);
        let subs = flatten(&cmds, 1.0);
        for &(x, y) in &subs[0].points {
            assert!((9.99..=110.01).contains(&x) && (19.99..=60.01).contains(&y));
        }
    }
}
