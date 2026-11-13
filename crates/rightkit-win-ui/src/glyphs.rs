//! Vector marks for the notch rings, matching the Mac notch: the Claude starburst and the
//! OpenAI knot (outlines traced for the Mac notch, filled even-odd) and hand-drawn stroke
//! equivalents of the SF Symbols the Mac uses for System (`cpu`), Disks (`internaldrive`)
//! and Send (`paperplane`). Coordinates of the strokes are in DIPs from the ring centre.

use std::f32::consts::SQRT_2;

use crate::canvas::Canvas;

/// The glyph frame the Mac draws a mark in (46 design px).
/// DIPs per design px (the design frame measures the 44 pt ring as 117 px).
pub const DESIGN: f32 = 44.0 / 117.0;

pub const GLYPH_SIZE: f32 = 46.0 * DESIGN;
/// Stroke weight of the symbol marks, in DIPs.
const STROKE: f32 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Claude,
    OpenAi,
    Chip,
    /// The Memory gauge: a memory module, a body with pins top and bottom.
    Memory,
    Drive,
    Plane,
    /// The Tools cell: four small squares.
    Tools,
}

impl Glyph {
    /// Mark for a `Cell::glyph` key; the Send cell's key is `send::GLYPH`.
    pub fn from_key(key: &str) -> Self {
        match key {
            "claude" => Glyph::Claude,
            "openai" => Glyph::OpenAi,
            "cpu" => Glyph::Chip,
            "memory" => Glyph::Memory,
            "disk" => Glyph::Drive,
            "tools" => Glyph::Tools,
            _ => Glyph::Plane,
        }
    }
}

/// Draws `glyph` centred on `centre` at `scale` device pixels per DIP.
pub fn draw(
    canvas: &mut Canvas,
    glyph: Glyph,
    centre: (f32, f32),
    scale: f32,
    color: u32,
    alpha: f32,
) {
    match glyph {
        Glyph::Claude => fill(canvas, CLAUDE, 0.97, centre, scale, color, alpha),
        Glyph::OpenAi => fill(canvas, OPENAI, 0.94, centre, scale, color, alpha),
        Glyph::Chip => chip(canvas, centre, scale, color, alpha),
        Glyph::Memory => memory_module(canvas, centre, scale, color, alpha),
        Glyph::Drive => drive(canvas, centre, scale, color, alpha),
        Glyph::Plane => plane(canvas, centre, scale, color, alpha),
        Glyph::Tools => tools(canvas, centre, scale, color, alpha),
    }
}

/// Four small rounded squares, two by two.
fn tools(canvas: &mut Canvas, (cx, cy): (f32, f32), scale: f32, color: u32, alpha: f32) {
    let square = rounded_rect(2.4, 2.4, 0.8);
    for (dx, dy) in [(-3.4f32, -3.4f32), (3.4, -3.4), (-3.4, 3.4), (3.4, 3.4)] {
        let centre = (cx + dx * scale, cy + dy * scale);
        canvas.stroke_polyline(
            &place(&square, centre, scale),
            STROKE * scale,
            true,
            color,
            alpha,
        );
    }
}

fn fill(
    canvas: &mut Canvas,
    outline: &[&[(f32, f32)]],
    optical: f32,
    (cx, cy): (f32, f32),
    scale: f32,
    color: u32,
    alpha: f32,
) {
    let size = GLYPH_SIZE * optical * scale;
    let loops: Vec<Vec<(f32, f32)>> = outline
        .iter()
        .map(|points| {
            points
                .iter()
                .map(|&(x, y)| (cx + (x - 0.5) * size, cy + (y - 0.5) * size))
                .collect()
        })
        .collect();
    canvas.fill_polygon(&loops, color, alpha);
}

/// Rounded rectangle of half-extents `(hx, hy)` and corner radius `r`, as a closed outline
/// around the origin.
pub fn rounded_rect(hx: f32, hy: f32, r: f32) -> Vec<(f32, f32)> {
    const STEPS: usize = 6;
    let mut points = Vec::new();
    // Corner centres with the angle each arc starts at (clockwise, y down).
    let corners = [
        (hx - r, hy - r, 0.0f32),
        (-hx + r, hy - r, 90.0),
        (-hx + r, -hy + r, 180.0),
        (hx - r, -hy + r, 270.0),
    ];
    for (ox, oy, start) in corners {
        for step in 0..=STEPS {
            let angle = (start + 90.0 * step as f32 / STEPS as f32).to_radians();
            points.push((ox + r * angle.cos(), oy + r * angle.sin()));
        }
    }
    points
}

fn place(points: &[(f32, f32)], (cx, cy): (f32, f32), scale: f32) -> Vec<(f32, f32)> {
    points
        .iter()
        .map(|&(x, y)| (cx + x * scale, cy + y * scale))
        .collect()
}

/// SF Symbol `cpu`: a chip with a square die and three pins on each side. Fitted to the Mac's
/// ring glyph at 2x (mean error 1.4 of 255): the body stroke is 1.21 pt, the die's 1.0 and
/// the pins' 1.22 (1.19 read 2 of ink under the Mac's, 0.95 read 16), the body's corners are nearly square (the stroke's own round join is the
/// corner) and the pins stand 2.5 pt apart.
/// A memory module: a wide rounded body with eight pins, four on each long side.
fn memory_module(canvas: &mut Canvas, centre: (f32, f32), scale: f32, color: u32, alpha: f32) {
    canvas.stroke_polyline(
        &place(&rounded_rect(6.2, 4.0, 0.5), centre, scale),
        1.21 * scale,
        true,
        color,
        alpha,
    );
    for offset in [-3.6f32, -1.2, 1.2, 3.6] {
        for sign in [-1.0f32, 1.0] {
            canvas.stroke_polyline(
                &place(&[(offset, sign * 4.0), (offset, sign * 5.6)], centre, scale),
                1.22 * scale,
                false,
                color,
                alpha,
            );
        }
    }
}

fn chip(canvas: &mut Canvas, centre: (f32, f32), scale: f32, color: u32, alpha: f32) {
    canvas.stroke_polyline(
        &place(&rounded_rect(5.54, 5.54, 0.11), centre, scale),
        1.21 * scale,
        true,
        color,
        alpha,
    );
    canvas.stroke_polyline(
        &place(&rounded_rect(2.86, 2.86, 0.4), centre, scale),
        STROKE * scale,
        true,
        color,
        alpha,
    );
    for offset in [-2.475f32, 0.0, 2.475] {
        for sign in [-1.0f32, 1.0] {
            for ends in [
                [(offset, sign * 5.97), (offset, sign * 6.87)],
                [(sign * 5.97, offset), (sign * 6.87, offset)],
            ] {
                canvas.stroke_polyline(
                    &place(&ends, centre, scale),
                    1.22 * scale,
                    false,
                    color,
                    alpha,
                );
            }
        }
    }
}

/// SF Symbol `internaldrive`: a trapezoid lid over a pill body with five vent ticks. Fitted to
/// the Mac's ring glyph at 2x (mean error 2.3 of 255): a steeper, taller lid, a body 5 pt
/// tall with full round ends, and ticks 0.9 pt wide at 0.86 strength (ink 338 of the Mac's 338 in the ring shots: r7's 0.88 at 0.81 gave 335, round 8's 1.04 at full 350).
fn drive(canvas: &mut Canvas, centre: (f32, f32), scale: f32, color: u32, alpha: f32) {
    let lid = [
        (-6.66, 1.11),
        (-4.37, -4.53),
        (-3.04, -5.23),
        (3.04, -5.23),
        (4.37, -4.53),
        (6.66, 1.11),
    ];
    canvas.stroke_polyline(
        &place(&lid, centre, scale),
        STROKE * scale,
        false,
        color,
        alpha,
    );
    let body: Vec<(f32, f32)> = rounded_rect(7.1, 2.5, 2.5)
        .into_iter()
        .map(|(x, y)| (x, y + 2.77))
        .collect();
    canvas.stroke_polyline(
        &place(&body, centre, scale),
        1.08 * scale,
        true,
        color,
        alpha,
    );
    for x in [-1.49f32, -0.01, 1.48, 2.96, 4.45] {
        canvas.stroke_polyline(
            &place(&[(x, 2.2), (x, 3.36)], centre, scale),
            0.9 * scale,
            false,
            color,
            alpha * 0.86,
        );
    }
}

/// SF Symbol `paperplane`: the outline of the plane and its fold line. Between round 7's size
/// (ink 227) and round 8's (240) at the Mac's 230 in the Send ring shot.
fn plane(canvas: &mut Canvas, centre: (f32, f32), scale: f32, color: u32, alpha: f32) {
    let width = 1.2 * scale;
    let outline = [(6.45, -6.95), (-6.95, -1.52), (-0.6, 0.5), (1.33, 6.65)];
    canvas.stroke_polyline(&place(&outline, centre, scale), width, true, color, alpha);
    canvas.stroke_polyline(
        &place(&[(6.45, -6.95), (-0.6, 0.5)], centre, scale),
        width,
        false,
        color,
        alpha,
    );
}

// ---- card symbols -------------------------------------------------------------------------------
//
// The SF Symbols the Mac cards use (pills, device rows, transfer discs), redrawn as strokes in a
// unit box: `-1.0..=1.0` spans the symbol's frame.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Symbol {
    Xmark,
    Refresh,
    Stop,
    Clock,
    DownCircle,
    DownApp,
    /// `arrow.triangle.2.circlepath` in a notice pill (the Mac's 9 pt symbol).
    Cycle,
    /// The same symbol in a round header button (the Mac's larger one).
    Sync,
    Undo,
    Box,
    Folder,
    Copy,
    /// SF Symbols' `doc.on.clipboard`.
    Clipboard,
    Compass,
    Plane,
    Phone,
    Laptop,
    Desktop,
    Globe,
    Terminal,
    Server,
    Check,
    Hand,
    Warning,
    /// SF Symbols' `slider.horizontal.3`: the handle's Settings glyph.
    Sliders,
    /// `rectangle.dashed`: the Tools card's Snip.
    SnipDashed,
    /// `rectangle.inset.filled`: the Tools card's Screen.
    ScreenFilled,
    /// `macwindow`: the Tools card's Window.
    MacWindow,
    /// `lock`: the Tools card's Lock screen.
    Lock,
    /// `clipboard`: the Tools card's Clipboard.
    ClipboardPlain,
    /// SF Symbols' `gearshape`: the notices' Open Settings.
    Gear,
}

impl Symbol {
    /// The symbol for an SF Symbol name in a view fixture; the display for any other.
    #[cfg_attr(not(feature = "qa-views"), allow(dead_code))] // used by the qa-views screenshots
    pub fn from_name(name: &str) -> Self {
        match name {
            "iphone" => Symbol::Phone,
            "laptopcomputer" => Symbol::Laptop,
            "globe" => Symbol::Globe,
            "terminal" => Symbol::Terminal,
            "server.rack" => Symbol::Server,
            "checkmark.circle" => Symbol::Check,
            "hand.raised" => Symbol::Hand,
            "exclamationmark.triangle" => Symbol::Warning,
            _ => Symbol::Desktop,
        }
    }
}

/// The large colour icons a card leads with when there is no real file icon to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tile {
    Folder,
    Message,
    Package,
    App,
}

/// Strokes in unit coordinates around a centre.
struct Brush<'a> {
    canvas: &'a mut Canvas,
    centre: (f32, f32),
    half: f32,
    width: f32,
    color: u32,
    alpha: f32,
}

type Point = (f32, f32);

/// Points of a circular arc, angles in degrees clockwise from 3 o'clock (y points down).
pub fn arc(centre: Point, radius: f32, from: f32, to: f32) -> Vec<Point> {
    let steps = ((to - from).abs() / 10.0).ceil().max(1.0) as usize;
    (0..=steps)
        .map(|step| {
            let angle = (from + (to - from) * step as f32 / steps as f32).to_radians();
            (
                centre.0 + radius * angle.cos(),
                centre.1 + radius * angle.sin(),
            )
        })
        .collect()
}

/// A closed ellipse as an outline.
fn ellipse(centre: Point, rx: f32, ry: f32) -> Vec<Point> {
    arc((0.0, 0.0), 1.0, 0.0, 360.0)
        .into_iter()
        .map(|(x, y)| (centre.0 + x * rx, centre.1 + y * ry))
        .collect()
}

fn shifted(points: Vec<Point>, (dx, dy): Point) -> Vec<Point> {
    points.into_iter().map(|(x, y)| (x + dx, y + dy)).collect()
}

impl Brush<'_> {
    fn at(&self, (x, y): Point) -> Point {
        (self.centre.0 + x * self.half, self.centre.1 + y * self.half)
    }

    fn path(&mut self, points: &[Point], closed: bool) {
        let placed: Vec<Point> = points.iter().map(|&p| self.at(p)).collect();
        self.canvas
            .stroke_polyline(&placed, self.width, closed, self.color, self.alpha);
    }

    fn circle(&mut self, centre: Point, radius: f32) {
        let (x, y) = self.at(centre);
        self.canvas.stroke_arc(
            x,
            y,
            radius * self.half,
            self.width,
            1.0,
            self.color,
            self.alpha,
        );
    }

    fn fill(&mut self, points: &[Point]) {
        let placed: Vec<Point> = points.iter().map(|&p| self.at(p)).collect();
        self.canvas.fill_polygon(&[placed], self.color, self.alpha);
    }

    /// `points` filled with `cut` left out of it.
    fn fill_cut(&mut self, points: &[Point], cut: &[Point]) {
        let loops: Vec<Vec<Point>> = [points, cut]
            .iter()
            .map(|outline| outline.iter().map(|&p| self.at(p)).collect())
            .collect();
        self.canvas.fill_polygon(&loops, self.color, self.alpha);
    }

    fn rect(&mut self, (hx, hy): Point, radius: f32, centre: Point) {
        self.path(&shifted(rounded_rect(hx, hy, radius), centre), true);
    }

    /// An arrow head: a chevron with its tip at `tip`, pointing along `dir`.
    fn head(&mut self, tip: Point, dir: Point, len: f32) {
        let side = (-dir.1, dir.0);
        let back = (tip.0 - dir.0 * len, tip.1 - dir.1 * len);
        let wing = len * 0.85;
        self.path(
            &[
                (back.0 + side.0 * wing, back.1 + side.1 * wing),
                tip,
                (back.0 - side.0 * wing, back.1 - side.1 * wing),
            ],
            false,
        );
    }

    /// A solid triangular arrow head on the circle of `radius` about `centre`: its base across
    /// the circle at `angle` degrees, its tip `length` on round the circle clockwise.
    fn arrow_head(&mut self, centre: Point, radius: f32, angle: f32, (half, length): (f32, f32)) {
        let a = angle.to_radians();
        let (radial, tangent) = ((a.cos(), a.sin()), Self::tangent(angle));
        let base = (centre.0 + radius * radial.0, centre.1 + radius * radial.1);
        self.fill(&[
            (base.0 + tangent.0 * length, base.1 + tangent.1 * length),
            (base.0 + radial.0 * half, base.1 + radial.1 * half),
            (base.0 - radial.0 * half, base.1 - radial.1 * half),
        ]);
    }

    /// The tangent of a clockwise circle at `angle` degrees.
    fn tangent(angle: f32) -> Point {
        let a = angle.to_radians();
        (-a.sin(), a.cos())
    }
}

/// Stroke weight (a fraction of the frame) and size of the card symbols: the Mac draws them in
/// medium or semibold SF Symbols, heavier and a little larger than a plain hairline outline.
const CARD_WEIGHT: f32 = 0.115;
const CARD_GROW: f32 = 1.07;

/// Draws `symbol` in a square of `size` device pixels centred on `centre`.
pub fn symbol(
    canvas: &mut Canvas,
    symbol: Symbol,
    centre: (f32, f32),
    size: f32,
    color: u32,
    alpha: f32,
) {
    draw_symbol(
        canvas,
        symbol,
        centre,
        size,
        (CARD_WEIGHT, CARD_GROW),
        (color, alpha),
    );
}

/// `symbol` with its stroke `weight` (a fraction of `size`) and `grow` (a scale on the drawn
/// extent) given.
fn draw_symbol(
    canvas: &mut Canvas,
    symbol: Symbol,
    centre: (f32, f32),
    size: f32,
    (weight, grow): (f32, f32),
    (color, alpha): (u32, f32),
) {
    if symbol == Symbol::Plane {
        plane(canvas, centre, size / 15.0, color, alpha);
        return;
    }
    let mut b = Brush {
        canvas,
        centre,
        half: size / 2.0 * grow,
        width: (size * weight).max(1.0),
        color,
        alpha,
    };
    match symbol {
        Symbol::Xmark => {
            // The Mac's xmark is a small bold cross: short arms and a light stroke.
            b.half = size / 2.0;
            b.width = (size * 0.09).max(1.2);
            b.path(&[(-0.4, -0.4), (0.4, 0.4)], false);
            b.path(&[(0.4, -0.4), (-0.4, 0.4)], false);
        }
        Symbol::Refresh => {
            // `arrow.clockwise`: an open arc from 3 o'clock round the bottom and left to the top,
            // ending in a ">" head (two round-capped strokes meeting at the arc's tip, 45 degrees
            // either side of the way on). Fitted to the Mac's 2x pill symbol (0.39 percent mean
            // coverage error): the arc's centre, radius, extent and stroke, then the head's tip and
            // the ends of its arms, in units of half the frame.
            b.half = size / 2.0;
            b.width = (size * 0.1005).max(1.0);
            b.path(&arc((-0.0591, 0.19), 0.7197, 0.0, 289.0), false);
            b.path(
                &[(-0.0695, -0.8863), (0.2957, -0.5051), (-0.0695, -0.1399)],
                false,
            );
        }
        Symbol::Stop => {
            b.circle((0.0, 0.0), 0.86);
            b.fill(&rounded_rect(0.3, 0.3, 0.08));
        }
        Symbol::Clock => {
            // `clock`: a plain ring with two thinner hands, no arrow; the minute hand points
            // straight up and the hour hand to the left (nine o'clock). Fitted to the Mac's 2x
            // pill symbol: the ring's centre, radius and stroke, then the hands' ends and stroke,
            // in units of half the frame. The hands meet a hair below the ring's centre.
            b.half = size / 2.0;
            b.width = (size * 0.1064).max(1.0);
            b.circle((0.0729, -0.0388), 0.8468);
            b.width = (size * 0.0801).max(1.0);
            b.path(&[(0.072, -0.572), (0.072, 0.006), (-0.36, 0.006)], false);
        }
        Symbol::DownCircle => {
            // `arrow.down.circle`: the same ring as `clock` (the Mac's 9 pt pill symbol), round
            // capped stem and a V head. Fitted to the Mac's 2x pill (0.47 percent mean coverage
            // error): the ring's centre, radius and stroke, then the arrow's stroke, stem, tip
            // and the head's arms, in units of half the frame.
            b.half = size / 2.0;
            b.width = (size * 0.1064).max(1.0);
            b.circle((0.0729, -0.0387), 0.8468);
            b.width = (size * 0.0942).max(1.0);
            b.path(&[(0.0721, -0.4086), (0.0721, 0.3196)], false);
            b.path(
                &[(-0.2320, 0.0752), (0.0721, 0.3196), (0.3762, 0.0752)],
                false,
            );
        }
        Symbol::DownApp => {
            b.rect((0.82, 0.82), 0.3, (0.0, 0.0));
            b.path(&[(0.0, -0.4), (0.0, 0.34)], false);
            b.path(&[(-0.28, 0.06), (0.0, 0.36), (0.28, 0.06)], false);
        }
        Symbol::Sliders => {
            // `slider.horizontal.3`: three rails, each broken by a hollow knob. Fitted to the
            // Mac's 2x bulb glyph (mean error 1.9 of 255), in points at the bulb's 12 pt frame
            // from the glyph's centre: rails 1.23 wide and 3.7 apart, running 5.5 each way
            // and stopping 2 short of a knob centre; knobs wider than tall (1.74 by 1.16, a
            // stroke of 0.85), the first and last 2.17 right of the middle and the second
            // 2.17 left.
            let k = size / 2.0 / b.half / 6.0;
            b.width = size * 0.1025;
            let knob_stroke = size * 0.0708;
            for (y, knob) in [(-3.7f32, 2.17f32), (0.0, -2.17), (3.7, 2.17)] {
                b.path(&[(-5.47 * k, y * k), ((knob - 1.96) * k, y * k)], false);
                b.path(&[((knob + 1.96) * k, y * k), (5.47 * k, y * k)], false);
                let outline = ellipse((knob * k, y * k), 1.74 * k, 1.155 * k);
                let width = b.width;
                b.width = knob_stroke;
                b.path(&outline, true);
                b.width = width;
            }
        }
        Symbol::Cycle | Symbol::Sync => {
            // `arrow.triangle.2.circlepath`: two arcs of 122 degrees, the second the first turned
            // half way round, each ending in a solid arrow head that points on round the circle.
            // Traced from the Mac's 2x renders of its pill symbol and its button symbol: the
            // circle's centre, radius, head `(half width, length)` and stroke, in units of half
            // the frame.
            let (centre, radius, head, weight) = if symbol == Symbol::Cycle {
                ((0.154, -0.039), 0.848, (0.332, 0.45), 0.106)
            } else {
                ((0.048, 0.016), 0.771, (0.296, 0.411), 0.1065)
            };
            b.half = size / 2.0;
            b.width = (size * weight).max(1.0);
            for turn in [0.0, 180.0] {
                b.path(&arc(centre, radius, -133.0 + turn, -11.0 + turn), false);
                b.arrow_head(centre, radius, -11.0 + turn, head);
            }
        }
        Symbol::Undo => {
            let mut turn = vec![(-0.55, -0.45), (0.2, -0.45)];
            turn.extend(arc((0.2, 0.0), 0.45, -90.0, 90.0));
            turn.push((-0.4, 0.45));
            b.path(&turn, false);
            b.head((-0.62, -0.45), (-1.0, 0.0), 0.34);
        }
        Symbol::Box => {
            b.path(
                &[(0.0, -0.88), (0.8, -0.44), (0.0, 0.0), (-0.8, -0.44)],
                true,
            );
            b.path(
                &[
                    (-0.8, -0.44),
                    (-0.8, 0.46),
                    (0.0, 0.9),
                    (0.8, 0.46),
                    (0.8, -0.44),
                ],
                false,
            );
            b.path(&[(0.0, 0.0), (0.0, 0.9)], false);
            // The Mac's `shippingbox` has a seam across the lid.
            b.path(&[(-0.4, -0.66), (0.4, -0.22)], false);
        }
        Symbol::Folder => {
            b.path(
                &[
                    (-0.88, 0.66),
                    (-0.88, -0.66),
                    (-0.2, -0.66),
                    (0.06, -0.38),
                    (0.88, -0.38),
                    (0.88, 0.66),
                ],
                true,
            );
            // The Mac's `folder` has a line under the tab, across the front.
            b.path(&[(-0.88, -0.38), (0.06, -0.38)], false);
        }
        Symbol::Copy => {
            b.half = size / 2.0;
            b.width = (size * 0.102).max(1.0);
            doc_on_doc(&mut b);
        }
        Symbol::Clipboard => {
            b.half = size / 2.0;
            b.width = (size * 0.0617).max(1.0);
            clipboard(&mut b);
        }
        Symbol::Compass => {
            b.circle((0.0, 0.0), 0.86);
            b.path(
                &[(0.38, -0.38), (0.12, 0.12), (-0.38, 0.38), (-0.12, -0.12)],
                true,
            );
        }
        Symbol::Phone | Symbol::Laptop | Symbol::Desktop => {
            b.half = size / 2.0;
            b.width = (size * 0.0764).max(1.0);
            device(&mut b, symbol);
        }
        Symbol::Globe => {
            b.circle((0.0, 0.0), 0.86);
            b.path(&ellipse((0.0, 0.0), 0.38, 0.86), true);
            b.path(&[(-0.86, 0.0), (0.86, 0.0)], false);
        }
        Symbol::Terminal => {
            // `terminal` as the Mac draws it, traced from its 2x render of the launcher's 18.5 pt
            // frame (37 px; centre at x 22, y 18): a box with a small prompt and cursor at its
            // upper left.
            let (centre, half) = ((22.0, 18.0), 18.48);
            b.half = size / 2.0;
            b.width = (size * 0.0682).max(1.0);
            b.path(
                &traced_box((5.26, 5.27, 39.57, 31.51), 3.57, centre, half),
                true,
            );
            b.width = (size * 0.0552).max(1.0);
            let prompt = [(11.3, 10.64), (16.12, 13.78), (11.3, 16.98)];
            b.path(&traced(&prompt, centre, half), false);
            b.width = (size * 0.0541).max(1.0);
            b.path(
                &traced(&[(18.43, 17.81), (24.18, 17.81)], centre, half),
                false,
            );
        }
        Symbol::Server => {
            b.rect((0.82, 0.3), 0.1, (0.0, -0.5));
            b.rect((0.82, 0.3), 0.1, (0.0, 0.5));
            b.path(&[(-0.5, -0.5), (-0.4, -0.5)], false);
            b.path(&[(-0.5, 0.5), (-0.4, 0.5)], false);
        }
        Symbol::Check => {
            b.circle((0.0, 0.0), 0.86);
            b.path(&[(-0.4, 0.02), (-0.1, 0.32), (0.42, -0.3)], false);
        }
        Symbol::Hand => {
            b.rect((0.5, 0.42), 0.3, (0.0, 0.42));
            for (x, top) in [(-0.38, -0.5), (-0.13, -0.8), (0.13, -0.85), (0.38, -0.55)] {
                let half = (0.1, (0.1 - top) / 2.0);
                b.rect(half, 0.1, (x, (top + 0.1) / 2.0));
            }
        }
        Symbol::Warning => {
            b.path(&[(0.0, -0.8), (0.9, 0.74), (-0.9, 0.74)], true);
            b.path(&[(0.0, -0.22), (0.0, 0.26)], false);
            b.path(&[(0.0, 0.5), (0.0, 0.5)], false);
        }
        Symbol::Gear => {
            // `gearshape`: eight teeth round a ring, and the hole at its middle.
            let mut outline: Vec<Point> = Vec::new();
            for tooth in 0..8 {
                let at = tooth as f32 * 45.0;
                for (turn, radius) in [(-17.0, 0.7), (-9.0, 0.96), (9.0, 0.96), (17.0, 0.7)] {
                    let angle = (at + turn).to_radians();
                    outline.push((radius * angle.cos(), radius * angle.sin()));
                }
            }
            b.path(&outline, true);
            b.circle((0.0, 0.0), 0.3);
        }
        Symbol::Plane => {}
        Symbol::SnipDashed
        | Symbol::ScreenFilled
        | Symbol::MacWindow
        | Symbol::Lock
        | Symbol::ClipboardPlain => tool_symbol(&mut b, symbol, size),
    }
}

/// The Tools card's five SF Symbols, traced from the Mac's 2x render of its pill (the frame is
/// the 12 pt `BUTTON_SYMBOL`, 24 px, so one trace pixel is `size / 24` and a unit coordinate is
/// 12 trace pixels). Each is centred on its own ink: dashed, screen and window 22 x 17 px, the
/// lock 13 x 19 and the clipboard 16 x 23.
fn tool_symbol(b: &mut Brush<'_>, symbol: Symbol, size: f32) {
    b.half = size / 2.0;
    let px = size / 24.0;
    let u = |v: f32| v / 12.0;
    let pts =
        |list: &[(f32, f32)]| -> Vec<Point> { list.iter().map(|&(x, y)| (u(x), u(y))).collect() };
    let boxed = |hx: f32, hy: f32, r: f32| -> Vec<Point> { rounded_rect(u(hx), u(hy), u(r)) };
    let arc_at = |cx: f32, cy: f32, r: f32, from: f32, to: f32| -> Vec<Point> {
        arc((u(cx), u(cy)), u(r), from, to)
    };
    b.width = (1.9 * px).max(1.0);
    match symbol {
        Symbol::SnipDashed => {
            let (hx, hy, r) = (10.1, 7.6, 3.2);
            // Four corners with a short lead-in each way.
            for (sx, sy, from) in [
                (1.0f32, 1.0f32, 0.0f32),
                (-1.0, 1.0, 90.0),
                (-1.0, -1.0, 180.0),
                (1.0, -1.0, 270.0),
            ] {
                let mut path = arc_at(sx * (hx - r), sy * (hy - r), r, from, from + 90.0);
                // A short lead-in past each end, along the edge the arc leaves.
                let extend = |from: Point, to: Point| -> Point {
                    let (dx, dy) = (from.0 - to.0, from.1 - to.1);
                    let len = dx.hypot(dy).max(1e-6);
                    (from.0 + dx / len * u(1.4), from.1 + dy / len * u(1.4))
                };
                let n = path.len();
                let head = extend(path[0], path[1]);
                let tail = extend(path[n - 1], path[n - 2]);
                path.insert(0, head);
                path.push(tail);
                b.path(&path, false);
            }
            for x in [(-6.2f32, -2.0f32), (1.6, 5.8)] {
                b.path(&pts(&[(x.0, -hy), (x.1, -hy)]), false);
                b.path(&pts(&[(x.0, hy), (x.1, hy)]), false);
            }
            for sx in [-hx, hx] {
                b.path(&pts(&[(sx, -2.6), (sx, 2.6)]), false);
            }
        }
        Symbol::ScreenFilled => {
            b.path(&boxed(10.1, 7.6, 2.8), true);
            b.fill(&shifted(boxed(8.2, 5.2, 0.4), (0.0, u(-0.4))));
        }
        Symbol::MacWindow => {
            b.path(&boxed(10.1, 7.6, 3.2), true);
            let alpha = b.alpha;
            b.alpha = alpha * 0.55;
            for x in [-8.0f32, -5.4, -2.8] {
                b.fill(&shifted(boxed(0.7, 0.7, 0.0), (u(x), u(-4.9))));
            }
            b.alpha = alpha;
        }
        Symbol::Lock => {
            b.path(&shifted(boxed(5.6, 5.2, 1.6), (0.0, u(3.0))), true);
            let mut shackle = vec![(u(-3.7), u(-2.2))];
            shackle.extend(arc_at(0.0, -4.7, 3.7, 180.0, 360.0));
            shackle.push((u(3.7), u(-2.2)));
            b.path(&shackle, false);
        }
        _ => {
            b.path(&shifted(boxed(7.0, 9.6, 2.6), (0.0, u(1.2))), true);
            b.fill(&shifted(boxed(3.1, 1.6, 0.6), (0.0, u(-9.4))));
        }
    }
}

/// Points traced from a Mac render, in its 2x pixels, as unit coordinates: `centre` is the
/// symbol frame's centre in those pixels and `half` its half size.
fn traced(points: &[Point], centre: Point, half: f32) -> Vec<Point> {
    points
        .iter()
        .map(|&(x, y)| ((x - centre.0) / half, (y - centre.1) / half))
        .collect()
}

/// `traced` for the rounded box `(left, top, right, bottom)` with corner radius `r`.
fn traced_box(
    (x0, y0, x1, y1): (f32, f32, f32, f32),
    r: f32,
    centre: Point,
    half: f32,
) -> Vec<Point> {
    let outline = shifted(
        rounded_rect((x1 - x0) / 2.0, (y1 - y0) / 2.0, r),
        ((x0 + x1) / 2.0, (y0 + y1) / 2.0),
    );
    traced(&outline, centre, half)
}

/// The Mac's `iphone`, `laptopcomputer` and `desktopcomputer` as its device rows draw them,
/// traced from its 2x render of the 11 pt frame (22 px; the frame's centre is at x 11, y 14 of
/// the phone's trace, x 17, y 12 of the laptop's and x 15, y 14 of the display's): an outline
/// round a screen that is washed with a quarter of the ink, a speaker and a home bar on the
/// phone, a base joined to the laptop's lid, a neck and foot under the display.
fn device(b: &mut Brush<'_>, symbol: Symbol) {
    let centre = match symbol {
        Symbol::Phone => (11.0, 14.0),
        Symbol::Laptop => (17.0, 12.0),
        _ => (15.0, 14.0),
    };
    // A box `(left, top, right, bottom)` of the trace with corner radius `r`, in unit coordinates.
    let boxed = |(x0, y0, x1, y1): (f32, f32, f32, f32), r: f32| -> Vec<Point> {
        shifted(
            rounded_rect((x1 - x0) / 2.0, (y1 - y0) / 2.0, r),
            ((x0 + x1) / 2.0, (y0 + y1) / 2.0),
        )
        .into_iter()
        .map(|(x, y)| ((x - centre.0) / 11.0, (y - centre.1) / 11.0))
        .collect()
    };
    // A screen: washed with a quarter of the ink, then outlined.
    fn screen(b: &mut Brush<'_>, outline: &[Point]) {
        let alpha = b.alpha;
        b.alpha = alpha * 0.25;
        b.fill(outline);
        b.alpha = alpha;
        b.path(outline, true);
    }
    match symbol {
        Symbol::Phone => {
            screen(b, &boxed((4.85, 3.86, 16.95, 24.87), 2.17));
            b.fill(&boxed((8.89, 5.54, 12.91, 6.95), 0.66));
            b.fill(&boxed((8.27, 22.2, 13.55, 23.19), 0.49));
        }
        Symbol::Laptop => {
            screen(b, &boxed((6.4, 3.83, 28.58, 19.15), 1.83));
            b.fill(&boxed((2.05, 18.06, 32.92, 20.44), 1.19));
        }
        _ => {
            screen(b, &boxed((2.83, 3.84, 26.76, 19.97), 1.96));
            b.fill(&boxed((11.38, 19.92, 18.2, 23.73), 0.0));
            b.fill(&boxed((10.56, 22.91, 19.06, 24.58), 0.72));
        }
    }
}

/// `doc.on.doc` as the Mac draws it in a notice pill, traced from its 2x render of the 9 pt
/// frame (18 px, centre at x 11, y 12 of the trace): a sheet in front, lower left, with a cut
/// and folded top-right corner, and the same sheet behind it, up and right, showing only
/// where the front does not cover it. The diagonal and the fold are a little lighter than the
/// edges.
fn doc_on_doc(b: &mut Brush<'_>) {
    let unit = |points: Vec<Point>| -> Vec<Point> {
        points
            .into_iter()
            .map(|(x, y)| ((x - 11.0) / 9.0, (y - 12.0) / 9.0))
            .collect()
    };
    let weight = b.width;
    const CORNER: f32 = 1.85;
    // One sheet: its centre-line extent `(left, top, right, bottom)`, where its cut corner
    // runs (`x - y = cut`), where its fold stands and ends `(x, y)`, and, for the sheet behind,
    // the `(y, x)` where the front covers its left and bottom edges.
    let mut sheet = |(left, top, right, bottom): (f32, f32, f32, f32),
                     cut: f32,
                     fold: (f32, f32),
                     covered: Option<(f32, f32)>| {
        let mut before = Vec::new();
        if let Some((y, _)) = covered {
            before.push((left, y));
        }
        before.extend(arc((left + CORNER, top + CORNER), CORNER, 180.0, 270.0));
        let diagonal = [(top + cut, top), (right, right - cut)];
        before.push(diagonal[0]);
        let mut after = vec![diagonal[1]];
        after.extend(arc((right - CORNER, bottom - CORNER), CORNER, 0.0, 90.0));
        match covered {
            Some((_, x)) => {
                after.push((x, bottom));
                b.path(&unit(before), false);
                b.path(&unit(after), false);
            }
            None => {
                after.extend(arc((left + CORNER, bottom - CORNER), CORNER, 90.0, 180.0));
                after.extend(before);
                b.path(&unit(after), false);
            }
        }
        b.width = weight * 0.84;
        b.path(&unit(diagonal.to_vec()), false);
        let (x, foot) = fold;
        let mut turn = vec![(x, top), (x, foot - 1.26)];
        turn.extend(arc((x + 1.26, foot - 1.26), 1.26, 180.0, 90.0));
        turn.push((right, foot));
        b.path(&unit(turn), false);
        b.width = weight;
    };
    sheet((4.93, 6.279, 16.38, 21.35), 4.54, (10.2, 12.65), None);
    sheet(
        (9.09, 1.93, 20.56, 17.02),
        13.45,
        (15.24, 7.51),
        Some((4.6, 18.1)),
    );
}

/// `doc.on.clipboard` (`paste` false: a sheet with a folded corner) and
/// `arrow.up.doc.on.clipboard` (`paste` true: a plain sheet with an arrow) as the Mac draws
/// them, traced from its 2x render of the 12 pt frame: a solid clipboard behind with its clip
/// slot, a hairline gap around the sheet in front, and the sheet an outline. The numbers are
/// that render's pixels (the frame is 24 px, its centre at x 15, y 16).
fn clipboard(b: &mut Brush<'_>) {
    // Sheet outline: left, top, right and bottom of its centre line, and its corner radius.
    const SHEET: (f32, f32, f32, f32, f32) = (4.75, 9.37, 17.68, 26.55, 2.2);
    // The clipboard's extent and corner radius, and the gap it keeps from the sheet.
    const BOARD: (f32, f32, f32, f32, f32) = (8.5, 22.93, 3.95, 22.65, 2.8);
    const GAP: f32 = 0.75;
    let unit = |points: Vec<Point>| -> Vec<Point> {
        points
            .into_iter()
            .map(|(x, y)| ((x - 15.0) / 12.0, (y - 16.0) / 12.0))
            .collect()
    };
    let (left, top, right, bottom, corner) = SHEET;
    let (board_left, board_right, board_top, board_bottom, board_corner) = BOARD;
    let (above, beside) = (top - GAP, right + GAP);
    // The clipboard that shows: its outline, the sheet's gap cut out of the lower left.
    let mut back = arc(
        (board_right - board_corner, board_top + board_corner),
        board_corner,
        -90.0,
        0.0,
    );
    back.extend(arc(
        (board_right - board_corner, board_bottom - board_corner),
        board_corner,
        0.0,
        90.0,
    ));
    back.push((beside, board_bottom));
    // The sheet's cut corner runs along x - y = 1.9; the gap keeps off it by `GAP`.
    let cut = 1.9 + GAP * SQRT_2;
    back.extend([(beside, beside - cut), (above + cut, above)]);
    back.push((board_left, above));
    back.extend(arc(
        (board_left + board_corner, board_top + board_corner),
        board_corner,
        180.0,
        270.0,
    ));
    // The clip's slot.
    let slot = shifted(rounded_rect(2.84, 0.64, 0.6), (15.71, 6.53));
    b.fill_cut(&unit(back), &unit(slot));
    let mut sheet = arc((left + corner, top + corner), corner, 180.0, 270.0);
    sheet.extend([(top + 1.9, top), (right, right - 1.9)]);
    sheet.extend(arc((right - corner, bottom - corner), corner, 0.0, 90.0));
    sheet.extend(arc((left + corner, bottom - corner), corner, 90.0, 180.0));
    b.path(&unit(sheet), true);
    let weight = b.width;
    // The folded corner's two edges, rounded where they meet.
    b.width = weight * 0.93;
    let mut fold = vec![(10.68, top), (10.68, 15.14)];
    fold.extend(arc((12.08, 15.14), 1.4, 180.0, 90.0));
    fold.push((right, 16.54));
    b.path(&unit(fold), false);
}

// ---- launcher glyphs ----------------------------------------------------------------------------

/// The outline marks of the launcher: its search field's magnifier and the symbol a row shows
/// where it has no file icon (the Mac's SF Symbol for the row's kind).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowGlyph {
    /// `magnifyingglass`.
    Magnifier,
    /// `app`: a window.
    App,
    /// `note.text`: a page of lines.
    Note,
    /// `square.and.pencil`.
    Compose,
    /// `terminal`.
    Terminal,
    /// `internaldrive`.
    Drive,
    /// `sparkles`.
    Sparkles,
    /// `text.alignleft` with a quotation mark: a snippet.
    Quote,
    /// `equal.square`.
    Calc,
    /// `link`.
    Link,
    /// A speech bubble.
    Bubble,
    /// `safari`: an address.
    Globe,
    /// `waveform.path.ecg`.
    Monitor,
    /// `square.grid.2x2`.
    Grid,
    /// `gauge`: a usage reading.
    Gauge,
    /// `power`.
    Power,
    /// Settings' sliders.
    Sliders,
    /// `folder`.
    Folder,
    /// `doc`.
    Doc,
}

/// Draws `glyph` in a square of `size` device pixels centred on `centre`.
pub fn row_glyph(
    canvas: &mut Canvas,
    glyph: RowGlyph,
    centre: (f32, f32),
    size: f32,
    color: u32,
    alpha: f32,
) {
    match glyph {
        RowGlyph::Terminal => {
            return draw_symbol(
                canvas,
                Symbol::Terminal,
                centre,
                size,
                (0.085, 1.0),
                (color, alpha),
            );
        }
        RowGlyph::Globe => {
            return draw_symbol(
                canvas,
                Symbol::Globe,
                centre,
                size,
                (0.085, 1.0),
                (color, alpha),
            );
        }
        RowGlyph::Folder => {
            return draw_symbol(
                canvas,
                Symbol::Folder,
                centre,
                size,
                (0.085, 1.0),
                (color, alpha),
            );
        }
        RowGlyph::Sliders => {
            return draw_symbol(
                canvas,
                Symbol::Sliders,
                centre,
                size,
                (0.085, 1.0),
                (color, alpha),
            );
        }
        RowGlyph::Drive => return drive(canvas, centre, size / 15.0, color, alpha),
        _ => {}
    }
    let mut b = Brush {
        canvas,
        centre,
        half: size / 2.0,
        width: (size * 0.085).max(1.0),
        color,
        alpha,
    };
    // A four-pointed star of radius `r` around `at`.
    let star = |at: Point, r: f32| -> Vec<Point> {
        let n = r * 0.2;
        [
            (0.0, -r),
            (n, -n),
            (r, 0.0),
            (n, n),
            (0.0, r),
            (-n, n),
            (-r, 0.0),
            (-n, -n),
        ]
        .iter()
        .map(|&(x, y)| (at.0 + x, at.1 + y))
        .collect()
    };
    match glyph {
        RowGlyph::Magnifier => {
            // `magnifyingglass` as the Mac draws it (a 17 pt regular symbol), traced from its 2x
            // render in the search field (34 px frame, its centre at x 17, y 20 of the trace): the
            // outline of the lens and handle, and the lens's hole. One filled shape, so the handle
            // meets the ring without the ink's alpha reading twice there.
            const OUTLINE: [Point; 56] = [
                (15.75, 4.12),
                (14.5, 4.29),
                (11.88, 5.17),
                (10.12, 6.13),
                (9.5, 6.71),
                (8.46, 7.38),
                (7.38, 8.46),
                (6.71, 9.5),
                (6.12, 10.13),
                (5.28, 11.62),
                (4.29, 14.5),
                (4.02, 17.5),
                (4.4, 20.5),
                (4.75, 21.17),
                (5.22, 22.75),
                (6.17, 24.5),
                (8.07, 26.75),
                (8.8, 27.25),
                (9.25, 27.75),
                (10.62, 28.73),
                (12.0, 29.2),
                (12.88, 29.73),
                (13.5, 29.96),
                (17.5, 30.38),
                (20.75, 29.96),
                (21.5, 29.82),
                (22.38, 29.22),
                (23.62, 28.76),
                (24.5, 28.14),
                (24.88, 28.27),
                (25.88, 28.93),
                (32.5, 35.62),
                (34.5, 36.74),
                (35.5, 36.26),
                (36.1, 35.5),
                (36.22, 34.5),
                (35.85, 33.38),
                (28.03, 25.5),
                (28.12, 24.47),
                (28.75, 23.61),
                (29.24, 22.38),
                (29.83, 21.5),
                (30.42, 17.5),
                (29.96, 13.5),
                (29.74, 12.88),
                (29.22, 12.0),
                (28.68, 10.5),
                (26.88, 8.21),
                (25.87, 7.25),
                (25.12, 6.77),
                (24.5, 6.16),
                (22.81, 5.25),
                (21.17, 4.75),
                (20.5, 4.4),
                (19.62, 4.23),
                (17.5, 4.02),
            ];
            const LENS: [Point; 38] = [
                (16.38, 7.0),
                (14.5, 7.2),
                (12.25, 8.18),
                (10.5, 9.42),
                (9.42, 10.5),
                (8.25, 12.12),
                (7.24, 14.38),
                (6.95, 17.5),
                (7.13, 19.5),
                (7.35, 20.5),
                (7.78, 21.12),
                (8.32, 22.5),
                (9.17, 23.75),
                (9.79, 24.25),
                (10.25, 24.85),
                (11.38, 25.83),
                (12.5, 26.29),
                (13.38, 26.88),
                (17.5, 27.66),
                (20.5, 27.04),
                (21.37, 26.75),
                (22.06, 26.25),
                (23.25, 25.69),
                (24.5, 24.55),
                (25.69, 23.25),
                (26.25, 22.06),
                (26.75, 21.38),
                (27.04, 20.5),
                (27.66, 17.5),
                (27.55, 16.5),
                (26.83, 13.25),
                (26.29, 12.5),
                (25.75, 11.25),
                (23.75, 9.17),
                (22.5, 8.32),
                (21.12, 7.78),
                (20.5, 7.35),
                (18.5, 7.03),
            ];
            b.fill_cut(
                &traced(&OUTLINE, (17.0, 20.0), 17.0),
                &traced(&LENS, (17.0, 20.0), 17.0),
            );
        }
        RowGlyph::App => {
            b.rect((0.84, 0.7), 0.2, (0.0, 0.0));
            b.path(&[(-0.84, -0.3), (0.84, -0.3)], false);
        }
        RowGlyph::Note => {
            // `note.text` as the Mac draws it, traced from its 2x render of the 18.5 pt frame
            // (37 px; centre at x 22, y 16): a card with a heavy top and three lines in it.
            let (centre, half) = ((22.0, 16.0), 18.48);
            b.fill_cut(
                &traced_box((6.01, 2.06, 37.14, 30.7), 4.4, centre, half),
                &traced_box((8.51, 9.18, 34.62, 28.21), 2.28, centre, half),
            );
            b.width = (size * 0.0519).max(1.0);
            for (from, to, y) in [
                (13.51, 29.64, 13.99),
                (13.48, 29.68, 18.7),
                (13.46, 23.66, 23.41),
            ] {
                b.path(&traced(&[(from, y), (to, y)], centre, half), false);
            }
        }
        RowGlyph::Compose => {
            // `square.and.pencil` as the Mac draws it, traced from its 2x render of the 18.5 pt
            // frame (37 px; centre at x 22, y 18): a square open at the pencil's side, the pencil
            // and its cap.
            const SQUARE: [Point; 49] = [
                (26.5, 6.76),
                (15.88, 6.86),
                (13.12, 7.18),
                (11.12, 8.25),
                (10.13, 9.12),
                (9.25, 10.3),
                (8.36, 12.5),
                (8.01, 15.5),
                (8.02, 25.88),
                (8.35, 28.5),
                (9.25, 30.73),
                (10.36, 32.12),
                (11.12, 32.77),
                (12.62, 33.65),
                (13.5, 33.93),
                (16.5, 34.24),
                (27.5, 34.21),
                (30.5, 33.89),
                (32.62, 32.76),
                (33.77, 31.75),
                (34.9, 29.88),
                (35.78, 25.5),
                (35.78, 15.5),
                (35.34, 13.5),
                (35.0, 13.19),
                (34.5, 13.01),
                (33.12, 14.5),
                (33.1, 25.5),
                (32.84, 28.25),
                (31.87, 29.88),
                (30.88, 30.81),
                (29.5, 31.44),
                (27.5, 31.86),
                (16.5, 31.89),
                (14.5, 31.56),
                (12.75, 30.78),
                (11.25, 29.05),
                (11.01, 28.5),
                (10.56, 25.5),
                (10.56, 15.5),
                (11.05, 12.38),
                (12.41, 10.5),
                (14.5, 9.46),
                (16.5, 9.12),
                (27.5, 9.1),
                (28.5, 8.83),
                (29.82, 7.5),
                (29.5, 7.19),
                (28.75, 6.99),
            ];
            const PENCIL: [Point; 15] = [
                (34.38, 6.73),
                (33.5, 6.87),
                (33.16, 7.12),
                (20.12, 20.16),
                (19.17, 21.38),
                (18.75, 22.59),
                (18.23, 23.38),
                (18.18, 23.5),
                (18.49, 23.88),
                (19.58, 23.88),
                (21.81, 22.75),
                (35.75, 8.82),
                (35.96, 8.5),
                (35.73, 8.0),
                (34.88, 7.0),
            ];
            const CAP: [Point; 9] = [
                (37.38, 3.72),
                (36.5, 3.94),
                (36.0, 4.31),
                (35.86, 4.5),
                (35.79, 5.5),
                (36.25, 6.03),
                (37.5, 6.94),
                (38.82, 5.5),
                (38.71, 4.5),
            ];
            let (centre, half) = ((22.0, 18.0), 18.48);
            for outline in [&SQUARE[..], &PENCIL[..], &CAP[..]] {
                b.fill(&traced(outline, centre, half));
            }
        }
        RowGlyph::Sparkles => {
            // The Mac's arrangement (launcher-pulse, 18.7 px a unit at 2x): the two small stars
            // sit above and to the left of the large one, the middle one 11 px left and up of
            // it and the smallest 3 px left and 19 px up; the three centred as the box was.
            b.fill(&star((0.12, 0.22), 0.72));
            b.fill(&star((-0.447, -0.368), 0.34));
            b.fill(&star((-0.04, -0.8), 0.2));
        }
        RowGlyph::Quote => {
            // `text.quote` as the Mac draws it, traced from its 2x render of the 18.5 pt frame
            // (37 px; its centre is at x 22, y 18 of the trace): two opening quotation marks,
            // each a ball with a tail, beside four even lines, the first two cut short by them.
            const MARK: [Point; 28] = [
                (11.88, 4.12),
                (10.5, 4.46),
                (9.12, 5.21),
                (8.08, 6.12),
                (7.1, 7.38),
                (6.38, 9.08),
                (6.08, 10.5),
                (6.11, 11.5),
                (6.31, 12.38),
                (7.12, 13.93),
                (8.35, 15.0),
                (10.5, 15.56),
                (12.69, 14.75),
                (13.62, 13.5),
                (13.9, 12.5),
                (13.86, 11.5),
                (13.62, 10.8),
                (12.5, 9.39),
                (11.5, 9.03),
                (9.5, 9.04),
                (9.12, 8.5),
                (10.5, 7.06),
                (12.38, 6.26),
                (13.5, 5.94),
                (13.88, 5.58),
                (13.72, 4.5),
                (13.5, 4.28),
                (12.5, 4.04),
            ];
            let unit = |(x, y): Point| ((x - 22.0) / 18.48, (y - 18.0) / 18.48);
            b.width = (size * 0.069).max(1.0);
            for shift in [0.0, 9.6] {
                let mark: Vec<Point> = MARK.iter().map(|&(x, y)| unit((x + shift, y))).collect();
                b.fill(&mark);
            }
            for (from, to, y) in [
                (27.6, 36.0, 5.31),
                (27.6, 36.0, 13.45),
                (7.3, 36.0, 21.6),
                (7.3, 36.0, 29.79),
            ] {
                b.path(&[unit((from, y)), unit((to, y))], false);
            }
        }
        RowGlyph::Calc => {
            // The Mac's box is 28 px against 32 and its strokes lighter (r7 shots).
            b.width *= 0.8;
            b.rect((0.71, 0.71), 0.18, (0.0, 0.0));
            b.path(&[(-0.34, -0.14), (0.34, -0.14)], false);
            b.path(&[(-0.34, 0.14), (0.34, 0.14)], false);
        }
        RowGlyph::Link => {
            b.rect((0.46, 0.28), 0.28, (-0.4, 0.0));
            b.rect((0.46, 0.28), 0.28, (0.4, 0.0));
            b.path(&[(-0.14, 0.0), (0.14, 0.0)], false);
        }
        RowGlyph::Bubble => {
            b.path(&ellipse((0.0, -0.1), 0.86, 0.62), true);
            b.path(&[(-0.5, 0.4), (-0.74, 0.86), (-0.1, 0.52)], false);
        }
        RowGlyph::Monitor => {
            b.path(
                &[
                    (-0.9, 0.0),
                    (-0.45, 0.0),
                    (-0.25, -0.6),
                    (0.05, 0.65),
                    (0.3, -0.2),
                    (0.45, 0.0),
                    (0.9, 0.0),
                ],
                false,
            );
        }
        RowGlyph::Grid => {
            for (x, y) in [
                (-0.45f32, -0.45f32),
                (0.45, -0.45),
                (-0.45, 0.45),
                (0.45, 0.45),
            ] {
                b.rect((0.34, 0.34), 0.1, (x, y));
            }
        }
        RowGlyph::Gauge => {
            b.path(&arc((0.0, 0.25), 0.82, 180.0, 360.0), false);
            b.path(&[(0.0, 0.25), (0.42, -0.2)], false);
        }
        RowGlyph::Power => {
            b.path(&arc((0.0, 0.1), 0.7, -60.0, 240.0), false);
            b.path(&[(0.0, -0.85), (0.0, -0.1)], false);
        }
        RowGlyph::Doc => {
            b.path(
                &[
                    (-0.55, -0.85),
                    (0.15, -0.85),
                    (0.6, -0.4),
                    (0.6, 0.85),
                    (-0.55, 0.85),
                ],
                true,
            );
            b.path(&[(0.15, -0.85), (0.15, -0.4), (0.6, -0.4)], false);
            b.path(&[(-0.25, 0.1), (0.3, 0.1)], false);
            b.path(&[(-0.25, 0.45), (0.3, 0.45)], false);
        }
        RowGlyph::Terminal
        | RowGlyph::Drive
        | RowGlyph::Globe
        | RowGlyph::Folder
        | RowGlyph::Sliders => {}
    }
}

/// A large colour icon filling a square of `size` device pixels centred on `centre`.
pub fn tile(canvas: &mut Canvas, tile: Tile, (cx, cy): (f32, f32), size: f32) {
    let half = size / 2.0;
    let rect = |canvas: &mut Canvas, (x0, y0, x1, y1): (f32, f32, f32, f32), r: f32, color: u32| {
        canvas.fill_round_rect(
            cx + x0 * half,
            cy + y0 * half,
            (x1 - x0) * half,
            (y1 - y0) * half,
            [r * half; 4],
            color,
            1.0,
        );
    };
    match tile {
        Tile::Folder => {
            rect(canvas, (-0.9, -0.72, 0.9, 0.78), 0.14, 0x4BA3E6);
            rect(canvas, (-0.9, -0.82, -0.1, -0.5), 0.1, 0x4BA3E6);
            rect(canvas, (-0.9, -0.42, 0.9, 0.78), 0.14, 0x78C5F6);
        }
        Tile::Message => {
            rect(canvas, (-0.92, -0.92, 0.92, 0.92), 0.4, 0x5CB85C);
            let bubble: Vec<Point> = ellipse((0.0, -0.08), 0.58, 0.44)
                .into_iter()
                .map(|(x, y)| (cx + x * half, cy + y * half))
                .collect();
            canvas.fill_polygon(&[bubble], 0xFFFFFF, 1.0);
            let tail: Vec<Point> = [(-0.38, 0.26), (-0.62, 0.6), (-0.08, 0.36)]
                .iter()
                .map(|&(x, y)| (cx + x * half, cy + y * half))
                .collect();
            canvas.fill_polygon(&[tail], 0xFFFFFF, 1.0);
        }
        Tile::Package => {
            rect(canvas, (-0.92, -0.92, 0.92, 0.92), 0.4, 0x4B5567);
            symbol(canvas, Symbol::Box, (cx, cy), size * 0.56, 0xFFFFFF, 1.0);
        }
        Tile::App => {
            rect(canvas, (-0.92, -0.92, 0.92, 0.92), 0.4, 0x14141A);
            canvas.stroke_arc(cx, cy, 0.38 * half, 0.13 * half, 0.78, 0xFFFFFF, 1.0);
        }
    }
}

#[rustfmt::skip]
const CLAUDE: &[&[(f32, f32)]] = &[
    &[
        (0.2879, 0.0108), (0.2667, 0.0223), (0.2423, 0.0516), (0.2427, 0.0873),
        (0.2611, 0.1275), (0.3425, 0.2606), (0.3879, 0.3474), (0.3888, 0.3670),
        (0.3695, 0.3643), (0.2014, 0.2351), (0.1878, 0.2200), (0.1552, 0.1998),
        (0.1253, 0.1950), (0.1111, 0.1995), (0.0877, 0.2258), (0.0887, 0.2565),
        (0.0953, 0.2714), (0.1180, 0.2961), (0.1661, 0.3284), (0.1791, 0.3426),
        (0.2965, 0.4155), (0.3095, 0.4297), (0.3940, 0.4803), (0.3974, 0.4946),
        (0.3767, 0.5004), (0.2177, 0.4830), (0.0404, 0.4741), (0.0186, 0.4808),
        (0.0129, 0.4990), (0.0276, 0.5245), (0.0649, 0.5375), (0.3801, 0.5449),
        (0.3943, 0.5488), (0.3979, 0.5574), (0.3773, 0.5800), (0.3130, 0.6116),
        (0.2589, 0.6470), (0.2341, 0.6564), (0.1360, 0.7209), (0.1142, 0.7485),
        (0.1149, 0.7681), (0.1416, 0.7866), (0.1912, 0.7787), (0.4120, 0.6320),
        (0.4270, 0.6288), (0.4321, 0.6332), (0.4292, 0.6454), (0.3940, 0.6799),
        (0.3428, 0.7530), (0.2416, 0.8771), (0.2334, 0.9073), (0.2403, 0.9269),
        (0.2558, 0.9336), (0.2734, 0.9308), (0.3431, 0.8618), (0.4700, 0.6899),
        (0.4807, 0.6667), (0.4915, 0.6611), (0.5001, 0.6669), (0.4995, 0.6906),
        (0.4474, 0.9569), (0.4618, 0.9946), (0.4901, 1.0070), (0.5162, 0.9945),
        (0.5228, 0.9833), (0.5369, 0.9018), (0.5520, 0.7109), (0.5593, 0.6981),
        (0.5814, 0.7096), (0.6328, 0.7971), (0.7227, 0.9242), (0.7395, 0.9333),
        (0.7662, 0.9315), (0.7775, 0.9241), (0.7830, 0.9106), (0.7785, 0.8597),
        (0.6882, 0.7238), (0.6753, 0.7115), (0.6765, 0.6956), (0.6875, 0.6957),
        (0.7252, 0.7339), (0.8721, 0.8489), (0.8842, 0.8515), (0.8959, 0.8468),
        (0.9038, 0.8373), (0.9046, 0.8256), (0.8530, 0.7662), (0.6984, 0.6269),
        (0.6775, 0.6016), (0.6778, 0.5933), (0.6885, 0.5908), (0.8106, 0.6247),
        (0.9440, 0.6533), (0.9782, 0.6467), (1.0094, 0.6185), (0.9968, 0.5927),
        (0.9637, 0.5647), (0.8743, 0.5599), (0.8332, 0.5528), (0.7469, 0.5529),
        (0.7252, 0.5475), (0.7138, 0.5382), (0.7174, 0.5299), (0.7308, 0.5244),
        (0.9772, 0.4740), (0.9904, 0.4655), (0.9985, 0.4514), (1.0037, 0.4324),
        (1.0001, 0.4183), (0.9889, 0.4107), (0.9616, 0.4064), (0.8604, 0.4200),
        (0.7578, 0.4394), (0.7164, 0.4523), (0.7010, 0.4468), (0.7391, 0.3769),
        (0.8619, 0.2207), (0.8733, 0.1749), (0.8678, 0.1520), (0.8528, 0.1331),
        (0.8338, 0.1217), (0.8169, 0.1215), (0.7888, 0.1313), (0.7199, 0.2007),
        (0.6224, 0.3290), (0.6034, 0.3517), (0.5941, 0.3541), (0.5878, 0.3448),
        (0.5875, 0.3285), (0.6249, 0.1713), (0.6378, 0.0744), (0.6253, 0.0430),
        (0.6043, 0.0257), (0.5890, 0.0265), (0.5661, 0.0463), (0.5471, 0.0805),
        (0.5369, 0.2279), (0.5259, 0.2877), (0.5223, 0.3461), (0.5165, 0.3730),
        (0.5080, 0.3786), (0.4728, 0.2909), (0.3941, 0.1409), (0.3647, 0.0645),
        (0.3439, 0.0282), (0.3305, 0.0183), (0.3013, 0.0095), (0.2880, 0.0108),
    ],
];

#[rustfmt::skip]
// Traced outline coordinates; values near pi/4 are coincidence, not the constant.
#[allow(clippy::approx_constant)]
const OPENAI: &[&[(f32, f32)]] = &[
    &[
        (0.4234, -0.0272), (0.3541, -0.0187), (0.3013, 0.0040), (0.2422, 0.0495),
        (0.2012, 0.1069), (0.1823, 0.1455), (0.1435, 0.1661), (0.1104, 0.1756),
        (0.0468, 0.2232), (0.0013, 0.2863), (-0.0216, 0.3430), (-0.0275, 0.4147),
        (-0.0212, 0.4800), (0.0021, 0.5354), (0.0380, 0.5897), (0.0271, 0.6302),
        (0.0242, 0.6634), (0.0281, 0.7123), (0.0453, 0.7686), (0.0889, 0.8376),
        (0.1131, 0.8638), (0.1823, 0.9074), (0.2413, 0.9266), (0.3060, 0.9300),
        (0.3346, 0.9264), (0.3516, 0.9307), (0.4021, 0.9710), (0.4298, 0.9808),
        (0.4562, 0.9973), (0.5234, 1.0093), (0.5785, 1.0094), (0.6468, 0.9924),
        (0.7148, 0.9513), (0.7772, 0.8832), (0.8042, 0.8303), (0.8365, 0.8205),
        (0.8821, 0.7975), (0.9458, 0.7449), (0.9740, 0.7075), (0.9957, 0.6627),
        (1.0095, 0.5968), (1.0090, 0.5479), (0.9945, 0.4764), (0.9451, 0.3970),
        (0.9551, 0.3353), (0.9516, 0.2607), (0.9295, 0.1997), (0.8836, 0.1329),
        (0.8279, 0.0900), (0.7854, 0.0679), (0.7109, 0.0532), (0.6410, 0.0590),
        (0.6009, 0.0243), (0.5658, 0.0021), (0.5325, -0.0075), (0.5138, -0.0192),
        (0.4237, -0.0273),
    ],
    &[
        (0.5915, 0.6193), (0.6003, 0.6198), (0.6051, 0.6294), (0.6065, 0.6987),
        (0.6030, 0.7094), (0.5791, 0.7311), (0.5493, 0.7434), (0.4991, 0.7768),
        (0.4755, 0.7856), (0.3803, 0.8423), (0.3101, 0.8617), (0.2639, 0.8607),
        (0.2101, 0.8447), (0.1602, 0.8126), (0.1332, 0.7824), (0.1099, 0.7456),
        (0.0966, 0.6858), (0.0971, 0.6532), (0.1028, 0.6439), (0.1253, 0.6483),
        (0.1529, 0.6679), (0.1798, 0.6772), (0.2300, 0.7111), (0.2579, 0.7211),
        (0.3071, 0.7554), (0.3359, 0.7601), (0.3530, 0.7554), (0.5913, 0.6194),
    ],
    &[
        (0.1549, 0.2337), (0.1661, 0.2362), (0.1713, 0.2520), (0.1740, 0.4890),
        (0.2443, 0.5383), (0.3815, 0.6098), (0.3978, 0.6261), (0.4244, 0.6356),
        (0.4284, 0.6478), (0.4203, 0.6616), (0.3691, 0.6910), (0.3509, 0.6950),
        (0.3314, 0.6910), (0.3149, 0.6770), (0.2011, 0.6118), (0.1787, 0.6039),
        (0.1148, 0.5618), (0.0696, 0.5126), (0.0463, 0.4673), (0.0413, 0.4222),
        (0.0428, 0.3713), (0.0661, 0.3121), (0.1072, 0.2634), (0.1340, 0.2422),
        (0.1549, 0.2337),
    ],
    &[
        (0.6595, 0.4590), (0.6735, 0.4607), (0.7335, 0.5002), (0.7409, 0.5112),
        (0.7407, 0.7673), (0.7335, 0.8119), (0.6907, 0.8831), (0.6628, 0.9073),
        (0.6193, 0.9293), (0.5683, 0.9410), (0.4861, 0.9339), (0.4346, 0.9106),
        (0.4283, 0.8998), (0.4330, 0.8914), (0.4606, 0.8724), (0.5577, 0.8212),
        (0.5741, 0.8071), (0.5975, 0.7986), (0.6123, 0.7854), (0.6347, 0.7751),
        (0.6475, 0.7604), (0.6525, 0.7361), (0.6513, 0.4738), (0.6592, 0.4591),
    ],
    &[
        (0.4201, 0.0407), (0.4901, 0.0454), (0.5415, 0.0677), (0.5485, 0.0825),
        (0.5360, 0.1008), (0.5091, 0.1112), (0.4589, 0.1456), (0.3563, 0.1987),
        (0.3290, 0.2258), (0.3275, 0.5024), (0.3233, 0.5161), (0.3135, 0.5208),
        (0.3039, 0.5183), (0.2881, 0.5027), (0.2618, 0.4935), (0.2379, 0.4671),
        (0.2377, 0.2259), (0.2419, 0.1878), (0.2623, 0.1376), (0.3041, 0.0883),
        (0.3313, 0.0668), (0.3713, 0.0482), (0.4197, 0.0407),
    ],
    &[
        (0.6793, 0.1223), (0.7157, 0.1231), (0.7641, 0.1328), (0.8047, 0.1561),
        (0.8276, 0.1768), (0.8625, 0.2233), (0.8807, 0.2686), (0.8876, 0.3190),
        (0.8831, 0.3376), (0.8696, 0.3399), (0.8290, 0.3206), (0.7808, 0.2871),
        (0.7555, 0.2771), (0.7028, 0.2432), (0.6491, 0.2198), (0.6010, 0.2420),
        (0.3950, 0.3609), (0.3770, 0.3587), (0.3753, 0.2836), (0.3855, 0.2637),
        (0.6109, 0.1354), (0.6793, 0.1223),
    ],
    &[
        (0.6168, 0.2853), (0.6369, 0.2862), (0.8593, 0.4128), (0.9080, 0.4616),
        (0.9290, 0.4972), (0.9405, 0.5690), (0.9306, 0.6410), (0.9050, 0.6876),
        (0.8550, 0.7354), (0.8203, 0.7467), (0.8106, 0.7381), (0.8105, 0.5166),
        (0.8003, 0.4834), (0.7040, 0.4293), (0.6883, 0.4155), (0.6661, 0.4077),
        (0.5469, 0.3401), (0.5506, 0.3260), (0.6001, 0.2999), (0.6168, 0.2853),
    ],
    &[
        (0.4779, 0.3668), (0.5139, 0.3713), (0.5650, 0.4061), (0.5917, 0.4164),
        (0.6023, 0.4290), (0.6061, 0.4482), (0.6032, 0.5543), (0.5631, 0.5821),
        (0.4922, 0.6155), (0.4806, 0.6145), (0.4134, 0.5801), (0.3827, 0.5553),
        (0.3749, 0.5304), (0.3747, 0.4480), (0.3821, 0.4243), (0.4778, 0.3668),
    ],
];
