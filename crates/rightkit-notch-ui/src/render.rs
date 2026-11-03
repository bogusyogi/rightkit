//! Draws the notch body, the hover card and the Quit menu into canvases (premultiplied BGRA)
//! using the software rasteriser and GDI text. Dark solid style, like the Mac notch's solid
//! surface. Pure of window state: callers pass data and get pixels.

use std::cell::RefCell;
use std::f32::consts::PI;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::card::{Button, CardContent, Head, Hit, Lead, Live, Mark, Pill, Row, Tone};
use crate::layout::{
    self, Badges, CellView, Edge, Handle, INK_PRIMARY, PROGRESS_STROKE, RING, RING_TRACK_ALPHA,
    TRACK_STROKE, WEEKLY_DASH, WEEKLY_OUTSIDE_RADIUS, WEEKLY_RADIUS, WEEKLY_STROKE,
};
use crate::motion::{self, Pose};
use rightkit_win_ui::canvas::{Canvas, Mask};
use rightkit_win_ui::fileicon;
use rightkit_win_ui::glyphs::{self, Glyph, Symbol, Tile};
use rightkit_win_ui::surface::TextPainter;

const STALE_DIM: f32 = 0.45;
/// Glyph opacity while a limit is spent (the Mac dims it to 0.35).
const BLOCKED_GLYPH: f32 = 0.35;
/// Permission dot on the folded pill (the Mac's four-point amber dot), in DIPs.
const PILL_DOT: f32 = 4.0;
/// The Quit menu's hairline (the Mac's menu border, about #474747).
const BORDER: u32 = 0x474747;
/// The Quit menu's fill: the Mac's dark grey menu (#292929) rather than the card's black.
const MENU_FILL: u32 = 0x292929;

/// The hub's Colour settings, as the Mac's notch reads them from its preferences: the colour of
/// the ample state (`accentColor`) and where the bands turn orange and red (`watchLimit`,
/// `criticalLimit`, in thousandths). Set by the settings bridge at start and on every change.
static ACCENT: AtomicU32 = AtomicU32::new(layout::BAND_AMPLE);
static WATCH_MILLI: AtomicU32 = AtomicU32::new(700);
static CRITICAL_MILLI: AtomicU32 = AtomicU32::new(900);
/// The Colour transition setting: a continuous ramp for the provider rings and bars (`ramp`), or the
/// hard steps. Off by default, as on the Mac.
static RAMP: AtomicBool = AtomicBool::new(false);

/// Applies the Colour settings to everything drawn from now on.
pub fn set_appearance(accent: u32, watch_milli: u16, critical_milli: u16) {
    ACCENT.store(accent, Ordering::Relaxed);
    WATCH_MILLI.store(u32::from(watch_milli), Ordering::Relaxed);
    CRITICAL_MILLI.store(u32::from(critical_milli), Ordering::Relaxed);
}

/// Applies the Colour transition setting (the Mac's `colorTransitionStyle`): `true` is the ramp.
pub fn set_colour_transition(ramp: bool) {
    RAMP.store(ramp, Ordering::Relaxed);
}

/// The ample colour with the chosen accent standing in for it. The warning and critical colours
/// never change (the Mac's `UsageBand.color(accent:)`).
fn tint(color: u32) -> u32 {
    if color == layout::BAND_AMPLE {
        ACCENT.load(Ordering::Relaxed)
    } else {
        color
    }
}

/// The share a reading reaches when its ring turns red: the critical limit, as a fraction.
pub fn critical_fraction() -> f32 {
    CRITICAL_MILLI.load(Ordering::Relaxed) as f32 / 1000.0
}

/// Ring and bar colour for a used fraction: the Mac's three bands at the chosen limits, the
/// ample one in the accent.
fn band_color(fraction: f32) -> u32 {
    let watch_milli = WATCH_MILLI.load(Ordering::Relaxed);
    let critical_milli = CRITICAL_MILLI.load(Ordering::Relaxed);
    let watch = watch_milli as f32 / 1000.0;
    let critical = critical_milli as f32 / 1000.0;
    if fraction < watch {
        tint(layout::BAND_AMPLE)
    } else if fraction < critical {
        layout::BAND_WATCH
    } else {
        layout::BAND_CRITICAL
    }
}

/// The colour of a usage reading. With the ramp setting a provider's ring or bar takes the continuous
/// colour (the Mac's `ProviderRing` and window bars); the system gauges keep the hard steps, as on the Mac.
fn usage_color(fraction: f32, provider: bool) -> u32 {
    if provider && RAMP.load(Ordering::Relaxed) {
        layout::ramp_color(
            fraction,
            WATCH_MILLI.load(Ordering::Relaxed) as f32 / 1000.0,
        )
    } else {
        band_color(fraction)
    }
}

/// The Mac's Quit item: "Quit Pulse" 16pt in from the left of a 180 x 36pt pill. Its ⌘Q hint
/// is not drawn: the Windows notch binds no quit key.
const MENU_PAD: f32 = 16.0;
const MENU_RADIUS: f32 = 12.0;
const MENU_SIZE: f32 = 13.5;
const MENU_LINE: f32 = 18.0;

pub const MENU_WIDTH: f32 = 180.0;
pub const MENU_HEIGHT: f32 = 36.0;

fn px(dip: f32, scale: f32) -> i32 {
    (dip * scale).round() as i32
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    canvas: &mut Canvas,
    text: &mut TextPainter,
    value: &str,
    (x, y): (i32, i32),
    (size_dip, bold): (f32, bool),
    scale: f32,
    color: u32,
    alpha: f32,
) -> i32 {
    match text.render(value, px(size_dip, scale), bold) {
        Some(Mask { width: 0, .. }) | None => 0,
        Some(mask) => {
            canvas.draw_mask(&mask, x, y, color, alpha);
            mask.width as i32
        }
    }
}

/// Width in pixels a string would occupy.
fn text_width(text: &mut TextPainter, value: &str, size_dip: f32, bold: bool, scale: f32) -> i32 {
    text.measure(value, px(size_dip, scale), bold)
}

/// Where points of the notch's own frame (along the screen edge, depth in from it) land on
/// the canvas. `across` is the canvas depth with the bezel band cropped away; `pad` is the
/// band the view shots keep (the Mac's offscreen renders show it as a thin strip).
#[derive(Clone, Copy)]
struct Frame {
    edge: Edge,
    across: f32,
    pad: f32,
    /// Room the window keeps before the start of the shape (the minimise button's zone), in
    /// pixels: every point along the stack is moved by it.
    lead: f32,
}

impl Frame {
    fn at(self, along: f32, depth: f32) -> (f32, f32) {
        self.shift(layout::to_canvas(
            self.edge,
            along + self.lead,
            depth,
            self.across,
        ))
    }

    /// A point already in cropped-canvas pixels, moved for the band.
    fn shift(self, (x, y): (f32, f32)) -> (f32, f32) {
        match self.edge {
            Edge::Top => (x, y + self.pad),
            Edge::Left => (x + self.pad, y),
            Edge::Bottom | Edge::Right => (x, y),
        }
    }
}

/// The Mac's `fluidTurn` walk, a quarter turn whose bend ramps in from nothing and back out
/// (ramp 0.5), as `(u, v, heading)` with `u` and `v` normalised to 0..1.
fn fluid_walk() -> Vec<(f32, f32, f32)> {
    const STEPS: usize = 96;
    let bend = std::f32::consts::FRAC_PI_2 / 0.5;
    let (mut heading, mut u, mut v) = (0.0f32, 0.0f32, 0.0f32);
    let mut walk = vec![(0.0, 0.0, 0.0)];
    for step in 0..STEPS {
        let t = (step as f32 + 0.5) / STEPS as f32;
        let share = if t < 0.5 { t / 0.5 } else { (1.0 - t) / 0.5 };
        heading += bend * share / STEPS as f32;
        u += heading.cos() / STEPS as f32;
        v += heading.sin() / STEPS as f32;
        walk.push((u, v, heading));
    }
    let (end_u, end_v, _) = walk[walk.len() - 1];
    walk.into_iter()
        .map(|(u, v, heading)| (u / end_u, v / end_v, heading))
        .collect()
}

/// A fluid turn from `from`, leaving along `leaving` and arriving along `arriving`, reaching
/// `along` and `across` pixels on those axes.
fn flare(
    from: (f32, f32),
    (leaving, arriving): ((f32, f32), (f32, f32)),
    (along, across): (f32, f32),
) -> Vec<(f32, f32)> {
    fluid_walk()
        .into_iter()
        .map(|(u, v, _)| {
            (
                from.0 + leaving.0 * u * along + arriving.0 * v * across,
                from.1 + leaving.1 * u * along + arriving.1 * v * across,
            )
        })
        .collect()
}

/// Cubic Bezier through `p`, appended without its first point.
fn curve_to(out: &mut Vec<(f32, f32)>, p: [(f32, f32); 4]) {
    const STEPS: usize = 12;
    for step in 1..=STEPS {
        let t = step as f32 / STEPS as f32;
        let m = 1.0 - t;
        let w = [m * m * m, 3.0 * m * m * t, 3.0 * m * t * t, t * t * t];
        out.push((
            (0..4).map(|i| w[i] * p[i].0).sum(),
            (0..4).map(|i| w[i] * p[i].1).sum(),
        ));
    }
}

/// Outline of the notch (the Mac `SideNotchShape`) as `(along, depth)` pixels, `depth`
/// counted in from the top of the shape, which includes the bezel band. Ears flare out to
/// the screen edge at both ends; the far corners are circular.
fn shape_outline(length: f32, depth: f32, s: f32) -> Vec<(f32, f32)> {
    const REACH: f32 = 0.5523;
    let wanted = (layout::CORNER * s).min(depth / 2.0);
    let band = (layout::BAND * s).min(depth - wanted).max(0.0);
    let curl = (layout::CURL * s).min(depth - wanted - band).max(0.0);
    let corner = wanted.min((length - 2.0 * curl) / 2.0).max(0.0);
    let mut points = vec![(0.0, 0.0), (0.0, band)];
    if curl > 0.001 {
        points.extend(flare((0.0, band), ((1.0, 0.0), (0.0, 1.0)), (curl, curl)));
    }
    let (near, far) = (curl + corner, length - curl - corner);
    points.push((curl, depth - corner));
    curve_to(
        &mut points,
        [
            (curl, depth - corner),
            (curl, depth - corner + REACH * corner),
            (near - REACH * corner, depth),
            (near, depth),
        ],
    );
    points.push((far, depth));
    curve_to(
        &mut points,
        [
            (far, depth),
            (far + REACH * corner, depth),
            (length - curl, depth - corner + REACH * corner),
            (length - curl, depth - corner),
        ],
    );
    points.push((length - curl, band + curl));
    if curl > 0.001 {
        points.extend(flare(
            (length - curl, band + curl),
            ((0.0, -1.0), (1.0, 0.0)),
            (curl, curl),
        ));
    }
    points.push((length, 0.0));
    points
}

/// The ends gathered about their rings (liquid-motion pass §1), the Mac `gatheredPath`: `hug`
/// 0 is the resting outline (`None`), 1 hovered at the handle, 1.12 carried, below 0 while it
/// puffs back out. A tighter taper wraps the end past the top of the cap circle, which a flare
/// and a corner cannot draw, so each end is a fillet tangent to the bezel and to the cap
/// circle about the end ring, meeting the bezel √(4·r·taper) past the ring's centre.
fn gathered_outline(length: f32, depth: f32, s: f32, hug: f32) -> Option<Vec<(f32, f32)>> {
    if hug.abs() <= 0.001 {
        return None;
    }
    // The resting outline's own flare and corner, so the gathered ends are the resting ones
    // drawn tighter, about the same rings.
    let wanted = (layout::CORNER * s).min(depth / 2.0);
    let band = (layout::BAND * s).min(depth - wanted).max(0.0);
    let flare = (layout::CURL * s).min(depth - wanted - band).max(0.0);
    let corner = wanted.min((length - 2.0 * flare) / 2.0).max(0.0);
    let radius = (depth - band) / 2.0;
    if radius <= 1.0 || corner <= 0.0 || flare <= 0.0 {
        return None;
    }
    let reach = flare + corner;
    let (first, last) = (reach, length - reach);
    if last < first {
        return None;
    }
    // Never so loose that an end would start past the shape.
    let loosest = reach * reach / (4.0 * radius);
    let taper = (flare + (layout::GATHERED_TAPER * s - flare) * hug)
        .max(0.6 * s)
        .min(loosest);
    let past = (4.0 * radius * taper).sqrt();
    // Worked in the Mac's frame (x from the far side to the screen edge, y along) and turned
    // into this file's `(along, depth)` at the end.
    let bezel = depth - band;
    let centre_x = bezel - radius;
    let mut mac: Vec<(f32, f32)> = Vec::new();
    let arc = |out: &mut Vec<(f32, f32)>, centre: (f32, f32), r: f32, a0: f32, a1: f32| {
        const STEPS: usize = 32;
        for i in 1..=STEPS {
            let a = a0 + (a1 - a0) * i as f32 / STEPS as f32;
            out.push((centre.0 + r * a.cos(), centre.1 + r * a.sin()));
        }
    };
    let angle = |from: (f32, f32), to: (f32, f32)| (to.1 - from.1).atan2(to.0 - from.0);
    let lead_cap = (centre_x, first);
    let lead_fillet = (bezel - taper, first - past);
    mac.push((depth, first - past));
    mac.push((bezel, first - past));
    arc(
        &mut mac,
        lead_fillet,
        taper,
        0.0,
        angle(lead_fillet, lead_cap),
    );
    arc(
        &mut mac,
        lead_cap,
        radius,
        angle(lead_cap, lead_fillet),
        -PI,
    );
    mac.push((centre_x - radius, last));
    let trail_cap = (centre_x, last);
    let trail_fillet = (bezel - taper, last + past);
    arc(
        &mut mac,
        trail_cap,
        radius,
        PI,
        angle(trail_cap, trail_fillet),
    );
    arc(
        &mut mac,
        trail_fillet,
        taper,
        angle(trail_fillet, trail_cap),
        0.0,
    );
    mac.push((depth, last + past));
    Some(mac.into_iter().map(|(x, y)| (y, depth - x)).collect())
}

/// The notch on `edge`: the open body with its five rings, ears and settings arc, or the
/// folded pill. A permissions badge draws as an amber dot (the pill's centre, or the middle
/// of the settings arc); otherwise a pending update draws as a red one on the arc. With the
/// pointer on the settings handle (`handle`) the arc fills in as the settings button with a
/// gear, and the move grip comes out beside it.
pub fn render_notch(
    views: &[CellView],
    edge: Edge,
    folded: bool,
    badges: Badges,
    dpi: u32,
    text: &mut TextPainter,
    (handle, press, pose): (Option<Handle>, Option<Press>, &Pose),
) -> Canvas {
    draw_notch(
        views,
        edge,
        folded,
        badges,
        dpi,
        text,
        (false, handle, press, pose),
    )
}

/// The part of the notch the primary button is held down on: it reads pressed until release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    /// A ring cell (its index in the shown cells).
    Cell(usize),
}

/// A pressed ring cell reads dimmer, as the Mac's pressed orb does.
const CELL_PRESS_DIM: f32 = 0.55;

/// `render_notch` with the bezel band kept: the Mac's view renders show the 2 pt the shape
/// pushes past the screen edge as a thin strip along the welded side. `pose` is the settled
/// state the shot shows (rings emphasised or dulled, the handle out).
#[cfg_attr(not(feature = "qa-views"), allow(dead_code))] // used by the qa-views screenshots
pub fn render_notch_shot(
    views: &[CellView],
    edge: Edge,
    folded: bool,
    badges: Badges,
    dpi: u32,
    text: &mut TextPainter,
    pose: &Pose,
) -> Canvas {
    draw_notch(
        views,
        edge,
        folded,
        badges,
        dpi,
        text,
        (false, None, None, pose),
    )
}

/// The last open notch drawn with nothing on it but its rings (no handle out, no press, no
/// badge), kept so that a tick in which one reading moved redraws that ring alone. `base` is
/// the notch before its rings; a changed ring's square is put back from it and the ring is
/// drawn again at the same place, which gives the very pixels a full drawing gives.
struct Kept {
    /// Edge, DPI, whether the bezel band is kept, and the style count (`restyle`).
    key: KeptKey,
    base: Canvas,
    canvas: Canvas,
    views: Vec<CellView>,
    /// Each ring's square (left, top, right, bottom); `None` when it meets another ring's, so
    /// that ring is never redrawn alone.
    squares: Vec<Option<[usize; 4]>>,
}

type KeptKey = (Edge, u32, bool, u32);

thread_local! {
    static KEPT: std::cell::RefCell<Option<Kept>> = const { std::cell::RefCell::new(None) };
}

/// Counts changes to what rings look like for the same reading (colours, limits, which
/// rings show): a kept notch from before the change is not used again.
static STYLE: AtomicU32 = AtomicU32::new(0);
/// The activity arc's start, in turns from 12 o'clock (f32 bits). The notch's timer moves it
/// while an agent works (`main.rs` `activity_tick`).
static ACTIVITY_START: AtomicU32 = AtomicU32::new(0);
/// The Mac's activity arc: a quarter of the circle, on the 72 px circle inside the 117 px ring.
const ACTIVITY_SWEEP: f32 = 0.25;
const ACTIVITY_RATIO: f32 = 72.0 / 117.0;

/// Sets the activity arc's start, in turns (wrapped to one turn when drawn).
pub fn set_activity_start(turns: f32) {
    ACTIVITY_START.store(turns.to_bits(), Ordering::Relaxed);
}

/// The settings changed: the next notch is drawn in full.
pub fn restyle() {
    STYLE.fetch_add(1, Ordering::Relaxed);
}

/// The percent reading of the ring at `centre`, centred on its line past the ring: further
/// from the screen edge on a top or bottom edge, below the ring on a side edge. The pair
/// "30%/70%" is set smaller, as on the Mac.
fn draw_reading(
    canvas: &mut Canvas,
    text: &mut TextPainter,
    reading: &str,
    (cx, cy): (f32, f32),
    edge: Edge,
    s: f32,
) {
    let size = if reading.contains('/') {
        layout::LABEL_PAIR_SIZE
    } else {
        layout::LABEL_SIZE
    };
    let Some(mask) = text.render(reading, px(size, s), true) else {
        return;
    };
    let reach = (RING / 2.0 + layout::LABEL_GAP + layout::LABEL_LINE / 2.0) * s;
    let middle = if edge == Edge::Bottom {
        cy - reach
    } else {
        cy + reach
    };
    let x = (cx - mask.width as f32 / 2.0).round() as i32;
    let y = (middle - mask.height as f32 / 2.0).round() as i32;
    canvas.draw_mask(&mask, x, y, INK_PRIMARY, 1.0);
}

/// A Claude account's tag ("A", "B") on its ring, bottom right inside the track, so it never
/// reaches a neighbouring ring (#454).
fn draw_badge(
    canvas: &mut Canvas,
    text: &mut TextPainter,
    badge: &str,
    (cx, cy): (f32, f32),
    s: f32,
) {
    let Some(mask) = text.render(badge, px(layout::LABEL_PAIR_SIZE * 0.7, s), true) else {
        return;
    };
    let offset = RING * 0.27 * s;
    let x = (cx + offset - mask.width as f32 / 2.0).round() as i32;
    let y = (cy + offset - mask.height as f32 / 2.0).round() as i32;
    canvas.draw_mask(&mask, x, y, INK_PRIMARY, 1.0);
}

/// The square holding everything `draw_cell` draws for the ring at `centre`.
fn cell_square(canvas: &Canvas, (cx, cy): (f32, f32), s: f32) -> [usize; 4] {
    // The widest stroke's outer edge (the second ring's when it is drawn outside), and the
    // pixel its soft edge spills into.
    let wider = (PROGRESS_STROKE - TRACK_STROKE).max(0.0) / 2.0;
    let outside = WEEKLY_OUTSIDE_RADIUS + WEEKLY_STROKE / 2.0;
    let reach = (RING / 2.0 + wider).max(outside) * s + 1.5;
    let low = |value: f32| (value - reach).floor().max(0.0) as usize;
    let high = |value: f32, limit: usize| ((value + reach).ceil().max(0.0) as usize).min(limit);
    [
        low(cx),
        low(cy),
        high(cx, canvas.width),
        high(cy, canvas.height),
    ]
}

/// The kept notch with the rings whose reading changed drawn again, when it is the same
/// notch and each of those rings has a square of its own.
fn redraw_changed(key: KeptKey, views: &[CellView], frame: Frame, s: f32) -> Option<Canvas> {
    KEPT.with(|cell| {
        let mut slot = cell.borrow_mut();
        let kept = slot
            .as_mut()
            .filter(|kept| kept.key == key && kept.views.len() == views.len())?;
        let changed: Vec<usize> = (0..views.len())
            .filter(|&index| kept.views[index] != views[index])
            .collect();
        // Asked to draw with no reading changed: something else did, so draw in full.
        if changed.is_empty() || changed.iter().any(|&index| kept.squares[index].is_none()) {
            return None;
        }
        let width = kept.base.width;
        for index in changed {
            let [left, top, right, bottom] = kept.squares[index]?;
            for y in top..bottom {
                let row = y * width;
                kept.canvas.pixels[row + left..row + right]
                    .copy_from_slice(&kept.base.pixels[row + left..row + right]);
            }
            let centre = frame.shift(layout::ring_center(key.0, index, key.1));
            draw_cell(
                &mut kept.canvas,
                &views[index],
                centre,
                s,
                false,
                (1.0, 1.0),
            );
            kept.views[index] = views[index].clone();
        }
        Some(kept.canvas.clone())
    })
}

/// Keeps a notch just drawn in full for `redraw_changed`.
fn keep(key: KeptKey, base: Canvas, canvas: &Canvas, views: &[CellView], frame: Frame) {
    let s = layout::scale(key.1);
    let mut squares: Vec<Option<[usize; 4]>> = (0..views.len())
        .map(|index| {
            let centre = frame.shift(layout::ring_center(key.0, index, key.1));
            Some(cell_square(canvas, centre, s))
        })
        .collect();
    for first in 0..squares.len() {
        for second in first + 1..squares.len() {
            if let (Some(a), Some(b)) = (squares[first], squares[second])
                && a[0] < b[2]
                && b[0] < a[2]
                && a[1] < b[3]
                && b[1] < a[3]
            {
                squares[first] = None;
                squares[second] = None;
            }
        }
    }
    KEPT.with(|cell| {
        *cell.borrow_mut() = Some(Kept {
            key,
            base,
            canvas: canvas.clone(),
            views: views.to_vec(),
            squares,
        });
    });
}

/// `keep_band` and the handle part shown out: the live notch keeps no band and carries the
/// pointer zones (see `draw_handle_zone`). The view shots keep no band either, as the Mac's
/// stills draw none (its resting depth is `pillWidth` alone); the zones they carry are
/// invisible.
fn draw_notch(
    views: &[CellView],
    edge: Edge,
    folded: bool,
    badges: Badges,
    dpi: u32,
    text: &mut TextPainter,
    (keep_band, handle, press, pose): (bool, Option<Handle>, Option<Press>, &Pose),
) -> Canvas {
    let s = layout::scale(dpi);
    let (width, height) = layout::panel_size(edge, folded, dpi);
    let pad = if keep_band {
        (layout::BAND * s).round() as i32
    } else {
        0
    };
    let vertical = edge.is_vertical();
    let (canvas_width, canvas_height) = if vertical {
        (width + pad, height)
    } else {
        (width, height + pad)
    };
    let mut canvas = Canvas::new(canvas_width as usize, canvas_height as usize);
    let frame = Frame {
        edge,
        across: (if vertical { width } else { height }) as f32,
        pad: pad as f32,
        lead: if folded { 0.0 } else { layout::lead_px(dpi) },
    };
    // Rings only: one whose reading moved is redrawn alone on the notch kept from last time.
    // Not with the percent readings on: their text lies outside the rings' squares.
    let plain = !folded
        && handle.is_none()
        && press.is_none()
        && !badges.permissions
        && !badges.update
        && pose.at_rest()
        && !views
            .iter()
            .any(|view| view.warn || !view.burst.is_empty() || view.badge.is_some())
        && !layout::readings_on();
    let key = (edge, dpi, keep_band, STYLE.load(Ordering::Relaxed));
    if plain && let Some(kept) = redraw_changed(key, views, frame, s) {
        return kept;
    }
    let band = layout::BAND * s;
    let (depth, length) = if folded {
        (layout::PILL_DEPTH * s, layout::PILL_LONG * s)
    } else {
        (layout::body_depth(edge) * s, layout::shape_length(edge) * s)
    };
    // A folded pill's shape starts a little in from the ends of its window (`PILL_TAILS`).
    let tails = if folded { layout::PILL_TAILS * s } else { 0.0 };
    let resting = || shape_outline(length - 2.0 * tails, depth, s);
    let shape = if folded {
        resting()
    } else {
        gathered_outline(length, depth, s, pose.hug).unwrap_or_else(resting)
    };
    let outline: Vec<(f32, f32)> = shape
        .into_iter()
        .map(|(along, down)| frame.at(along + tails, down - band))
        .collect();
    let handle_on = !folded && crate::host::switch_on("showsSettingsHandle");
    // The minimise button sits at the start of the stack, where the handle's bulb is not.
    let mini_on = !folded && frame.lead > 0.0;
    if handle_on && !keep_band {
        draw_handle_zone(&mut canvas, frame, s);
    }
    if mini_on && !keep_band {
        draw_mini_zone(&mut canvas, frame, s);
    }
    // The bulb, under the notch so its neck leaves the cap's black and the end ring stays on
    // top.
    if handle_on && pose.bulb > 0.005 {
        draw_bulb(&mut canvas, frame, s, pose);
    }
    if mini_on && pose.mini > 0.005 {
        draw_knob(
            &mut canvas,
            frame,
            s,
            (layout::first_ring_along(), -1.0, pose.mini, 1.0),
        );
    }
    canvas.fill_polygon(&[outline], 0x000000, 1.0);
    if folded {
        if badges.permissions {
            dot(
                &mut canvas,
                frame.at(length / 2.0, depth / 2.0 - band),
                PILL_DOT / 2.0 * s,
                layout::BADGE_PERMISSIONS,
            );
        }
        return canvas;
    }
    let base = plain.then(|| canvas.clone());
    for (index, view) in views.iter().enumerate() {
        let centre = frame.shift(layout::ring_center(edge, index, dpi));
        if view.warn {
            let (scale, alpha) = pose.ring(index);
            draw_warning_band(&mut canvas, centre, s * scale, pose.warn, alpha);
        }
        draw_cell(
            &mut canvas,
            view,
            centre,
            s,
            press == Some(Press::Cell(index)),
            pose.ring(index),
        );
        if !view.burst.is_empty() {
            draw_burst_dots(&mut canvas, centre, s * pose.ring(index).0, &view.burst);
        }
        if let Some(reading) = &view.reading {
            draw_reading(&mut canvas, text, reading, centre, edge, s);
        }
        if let Some(badge) = &view.badge {
            draw_badge(&mut canvas, text, badge, centre, s);
        }
    }
    if handle_on && pose.bulb > 0.005 {
        draw_bulb_glyph(&mut canvas, frame, s, pose);
    }
    if mini_on && pose.mini > 0.005 {
        draw_mini_glyph(&mut canvas, frame, s, pose.mini);
    }
    let swell = if handle_on { pose.bulb } else { 0.0 };
    draw_bulb_marks(&mut canvas, frame, s, (swell, pose.squeeze), badges);
    if let Some(base) = base {
        keep(key, base, &canvas, views, frame);
    }
    canvas
}

/// Fills the rectangle spanning `from` to `to` (`(along, depth)` points of the notch's frame),
/// its corners rounded by `radius`.
fn fill_frame_rect(
    canvas: &mut Canvas,
    frame: Frame,
    from: (f32, f32),
    to: (f32, f32),
    radius: f32,
    (colour, alpha): (u32, f32),
) {
    let (x0, y0) = frame.at(from.0, from.1);
    let (x1, y1) = frame.at(to.0, to.1);
    let (left, top) = (x0.min(x1), y0.min(y1));
    canvas.fill_round_rect(
        left,
        top,
        x0.max(x1) - left,
        y0.max(y1) - top,
        [radius; 4],
        colour,
        alpha,
    );
}

/// Pixels the pointer can land on beyond the black. A layered window passes the pointer
/// through its fully transparent pixels, so the handle's zone carries one 255th of black,
/// which nothing can see. Drawn first: the notch's own black goes over it.
fn draw_handle_zone(canvas: &mut Canvas, frame: Frame, s: f32) {
    const PLATE: (u32, f32) = (0x000000, 1.0 / 255.0);
    let edge = frame.edge;
    let from = (layout::handle_ring_along(edge) - RING / 2.0) * s;
    let to = (layout::shape_length(edge) + layout::HANDLE_ZONE_REACH) * s;
    let depth = (layout::body_depth(edge) - layout::BAND) * s;
    fill_frame_rect(canvas, frame, (from, 0.0), (to, depth), 0.0, PLATE);
}

/// The bulb's centre along the stack from the end ring's, and its radius, for `swell`.
fn bulb_place(swell: f32) -> (f32, f32) {
    let s = swell.max(0.0);
    (
        layout::BULB_REST_CENTRE + (layout::BULB_OUT_CENTRE - layout::BULB_REST_CENTRE) * s,
        layout::BULB_REST_RADIUS
            + (layout::BULB_OUT_RADIUS - layout::BULB_REST_RADIUS) * s.clamp(0.0, 1.0),
    )
}

/// The minimise button's zone, the handle zone mirrored at the start of the stack: from the
/// room before the shape to the far side of the first ring.
fn draw_mini_zone(canvas: &mut Canvas, frame: Frame, s: f32) {
    const PLATE: (u32, f32) = (0x000000, 1.0 / 255.0);
    let edge = frame.edge;
    let from = -layout::HANDLE_ZONE_REACH * s;
    let to = (layout::first_ring_along() + RING / 2.0) * s;
    let depth = (layout::body_depth(edge) - layout::BAND) * s;
    fill_frame_rect(canvas, frame, (from, 0.0), (to, depth), 0.0, PLATE);
}

/// The handle's bulb (liquid-motion pass §1, the Mac's `NotchBulb`): a black disc on a neck
/// that leaves the end ring's cap, in one goo layer (blurred, then cut back at half strength)
/// that runs into the notch's black. More goo while it moves, less once it is out.
fn draw_bulb(canvas: &mut Canvas, frame: Frame, s: f32, pose: &Pose) {
    let ring = layout::handle_ring_along(frame.edge);
    draw_knob(canvas, frame, s, (ring, 1.0, pose.bulb, pose.squeeze));
}

/// A goo knob off the cap of the ring at `ring` along the stack: out toward the end of the stack
/// (`dir` 1, the handle's bulb) or toward its start (`dir` -1, the minimise button), `swell` out
/// and squeezed by `squeeze`.
fn draw_knob(
    canvas: &mut Canvas,
    frame: Frame,
    s: f32,
    (ring, dir, swell, squeeze): (f32, f32, f32, f32),
) {
    let swell = swell.max(0.0);
    let (centre, radius) = bulb_place(swell);
    let along = (ring + dir * centre) * s;
    let across = (layout::RING_ACROSS + layout::BULB_DROP) * s;
    let r = radius * squeeze * s;
    let mut layer = Canvas::new(canvas.width, canvas.height);
    let (bx, by) = frame.at(along, across);
    layer.fill_round_rect(bx - r, by - r, 2.0 * r, 2.0 * r, [r; 4], 0x000000, 1.0);
    // Where the knob and its neck lie: only this part of the canvas is blurred and composited.
    let mut bounds = (bx - r, by - r, bx + r, by + r);
    // The neck: wide at the cap, pinched toward the knob, its end 1.7 radii across. It is only
    // a neck once the knob is past where it starts.
    let from = (ring + dir * layout::NECK_FROM) * s;
    if (along - from) * dir > 0.0 {
        let base = layout::NECK_BASE / 2.0 * s;
        let pinch = (layout::NECK_BASE
            + (layout::NECK_PINCH_OUT - layout::NECK_BASE) * swell.min(1.0))
            * squeeze
            / 2.0
            * s;
        let top = r * layout::NECK_TOP / 2.0;
        let middle = (from + along) / 2.0;
        let mut neck: Vec<(f32, f32)> = Vec::new();
        let mut quad = |p0: (f32, f32), c: (f32, f32), p1: (f32, f32)| {
            const STEPS: usize = 12;
            for i in 0..=STEPS {
                let t = i as f32 / STEPS as f32;
                let m = 1.0 - t;
                neck.push(frame.at(
                    m * m * p0.0 + 2.0 * m * t * c.0 + t * t * p1.0,
                    m * m * p0.1 + 2.0 * m * t * c.1 + t * t * p1.1,
                ));
            }
        };
        quad(
            (from, across - base),
            (middle, across - pinch),
            (along, across - top),
        );
        quad(
            (along, across + top),
            (middle, across + pinch),
            (from, across + base),
        );
        for &(x, y) in &neck {
            bounds = (
                bounds.0.min(x),
                bounds.1.min(y),
                bounds.2.max(x),
                bounds.3.max(y),
            );
        }
        layer.fill_polygon(&[neck], 0x000000, 1.0);
    }
    let moving = swell > 0.02 && swell < 0.98;
    let sigma = s * if moving { 4.5 } else { 2.6 };
    // The blur reaches about three sigma past the shapes; the rest of the canvas stays zero.
    let pad = (3.0 * sigma).ceil() + 2.0;
    let x0 = ((bounds.0 - pad).floor().max(0.0) as usize).min(layer.width);
    let y0 = ((bounds.1 - pad).floor().max(0.0) as usize).min(layer.height);
    let x1 = ((bounds.2 + pad).ceil().max(0.0) as usize).min(layer.width);
    let y1 = ((bounds.3 + pad).ceil().max(0.0) as usize).min(layer.height);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let mut field = Vec::with_capacity(w * h);
    for y in y0..y1 {
        let row = &layer.pixels[y * layer.width + x0..y * layer.width + x1];
        field.extend(row.iter().map(|pixel| (pixel >> 24) as f32 / 255.0));
    }
    if sigma >= 1.0 {
        crate::blur::blur(&mut field, w, h, sigma);
    }
    let edge = (sigma * 1.2).max(1.0);
    for (index, value) in field.into_iter().enumerate() {
        let a = ((value - 0.5) * edge + 0.5).clamp(0.0, 1.0);
        if a <= 0.0 {
            continue;
        }
        let pixel = &mut canvas.pixels[(y0 + index / w) * canvas.width + x0 + index % w];
        // Black over what is there (premultiplied): colour fades by (1 - a), alpha adds up.
        let keep = 1.0 - a;
        let below = *pixel;
        let fade = |shift: u32| ((((below >> shift) & 0xFF) as f32 * keep).round() as u32) << shift;
        let alpha = ((a + (below >> 24) as f32 / 255.0 * keep) * 255.0).round() as u32;
        *pixel = (alpha.min(255) << 24) | fade(16) | fade(8) | fade(0);
    }
}

/// The minimise button's mark, in over the second half of its swell like the bulb's glyph: two
/// chevrons pointing back at the button (the notch gathers toward it) while the notch is whole,
/// pointing away from it (the notch opens out) while it is minimised. Upright on every edge.
fn draw_mini_glyph(canvas: &mut Canvas, frame: Frame, s: f32, swell: f32) {
    let alpha = smooth((swell - 0.45) / 0.5);
    if alpha <= 0.003 {
        return;
    }
    let reach = if layout::minimised() { 1.0 } else { -1.0 };
    let along = layout::first_ring_along() - bulb_place(swell).0;
    let across = layout::RING_ACROSS + layout::BULB_DROP;
    // Each chevron: its tip `HEIGHT` ahead of the arms' ends, which are `HALF` either side.
    const HEIGHT: f32 = 2.6;
    const HALF: f32 = 3.3;
    const PITCH: f32 = 3.6;
    for step in [-0.5, 0.5] {
        let tip = along + step * PITCH + reach * HEIGHT / 2.0;
        let back = tip - reach * HEIGHT;
        let points: Vec<(f32, f32)> = [(back, across - HALF), (tip, across), (back, across + HALF)]
            .into_iter()
            .map(|(a, c)| frame.at(a * s, c * s))
            .collect();
        canvas.stroke_polyline(&points, 1.5 * s, false, INK_PRIMARY, alpha);
    }
}

/// The band between the Send ring and its icon while a gauge is past its warning level: deep red,
/// on the slow pulse `glow` (0..1), drawn under the ring and the icon.
fn draw_warning_band(canvas: &mut Canvas, (cx, cy): (f32, f32), s: f32, glow: f32, dim: f32) {
    let radius = (RING / 2.0 - TRACK_STROKE + 0.5) * s;
    let alpha = (0.30 + 0.60 * glow.clamp(0.0, 1.0)) * dim;
    canvas.fill_round_rect(
        cx - radius,
        cy - radius,
        2.0 * radius,
        2.0 * radius,
        [radius; 4],
        layout::BAND_CRITICAL,
        alpha,
    );
}

/// How far the copy burst's dots orbit the Send ring's centre, and their radius, in points (the
/// ring's track ends at 22; the notch's body reaches about 28 either side of the centre).
const BURST_ORBIT: f32 = 25.5;
const BURST_DOT: f32 = 1.6;
/// The angle between two dots, in degrees; narrower when a long burst would pass a full turn.
const BURST_STEP: f32 = 28.0;
/// How near a point has to be to a dot to be on it, in points.
const BURST_HIT: f32 = 5.0;

/// Where the burst's `count` dots sit round the ring centred at `centre`: the first at the top,
/// then clockwise in copy order.
pub fn burst_dot_centres((cx, cy): (f32, f32), s: f32, count: usize) -> Vec<(f32, f32)> {
    let step = BURST_STEP.min(360.0 / count.max(1) as f32).to_radians();
    (0..count)
        .map(|index| {
            let angle = -std::f32::consts::FRAC_PI_2 + step * index as f32;
            (
                cx + BURST_ORBIT * s * angle.cos(),
                cy + BURST_ORBIT * s * angle.sin(),
            )
        })
        .collect()
}

/// Where the dismiss cross sits: one step before the first dot, so counter-clockwise of the top.
fn burst_dismiss_centre((cx, cy): (f32, f32), s: f32, count: usize) -> (f32, f32) {
    let step = BURST_STEP.min(360.0 / count.max(1) as f32).to_radians();
    let angle = -std::f32::consts::FRAC_PI_2 - step;
    (
        cx + BURST_ORBIT * s * angle.cos(),
        cy + BURST_ORBIT * s * angle.sin(),
    )
}

/// Whether the point `at` is on the burst's dismiss cross (same arguments as `burst_dot_at`).
pub fn burst_dismiss_at(centre: (f32, f32), s: f32, count: usize, at: (f32, f32)) -> bool {
    let (x, y) = burst_dismiss_centre(centre, s, count);
    count > 0 && (x - at.0).hypot(y - at.1) <= BURST_HIT * s
}

/// The dot of a burst of `count` under the point `at` (notch-local device pixels), when one is;
/// `centre` is the Send ring's (`layout::ring_center`) and `s` the notch's scale.
pub fn burst_dot_at(centre: (f32, f32), s: f32, count: usize, at: (f32, f32)) -> Option<usize> {
    let reach = BURST_HIT * s;
    burst_dot_centres(centre, s, count)
        .into_iter()
        .enumerate()
        .map(|(index, (x, y))| (index, (x - at.0).hypot(y - at.1)))
        .filter(|&(_, distance)| distance <= reach)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
}

/// The copy burst's dots round the Send ring: one per item still to paste, in copy order; a
/// selected one is larger and fully lit (#387).
fn draw_burst_dots(canvas: &mut Canvas, centre: (f32, f32), s: f32, dots: &[bool]) {
    let (x, y) = burst_dismiss_centre(centre, s, dots.len());
    let arm = BURST_DOT * 1.4 * s;
    for line in [
        [(x - arm, y - arm), (x + arm, y + arm)],
        [(x - arm, y + arm), (x + arm, y - arm)],
    ] {
        canvas.stroke_polyline(&line, (0.7 * s).max(1.0), false, INK_PRIMARY, 0.6);
    }
    for ((x, y), &selected) in burst_dot_centres(centre, s, dots.len())
        .into_iter()
        .zip(dots)
    {
        let radius = BURST_DOT * s * if selected { 1.3 } else { 1.0 };
        let alpha = if selected { 1.0 } else { 0.6 };
        canvas.fill_round_rect(
            x - radius,
            y - radius,
            2.0 * radius,
            2.0 * radius,
            [radius; 4],
            INK_PRIMARY,
            alpha,
        );
    }
}

/// The Settings glyph on the bulb, upright on every edge: in over the second half of the swell,
/// at half while carried (a squeeze below the dip's 0.9).
fn draw_bulb_glyph(canvas: &mut Canvas, frame: Frame, s: f32, pose: &Pose) {
    let swell = pose.bulb.max(0.0);
    let shown = smooth((swell - 0.45) / 0.5);
    let carried = pose.squeeze < (motion::DIP + motion::CARRIED_SQUEEZE) / 2.0;
    let alpha = if carried { shown * 0.5 } else { shown };
    if alpha <= 0.003 {
        return;
    }
    let ring = layout::handle_ring_along(frame.edge);
    let along = (ring + bulb_place(swell).0) * s;
    // The Mac's glyph sits 0.45 pt right of the bulb's centre (measured on the top edge).
    let (gx, gy) = frame.at(along, (layout::RING_ACROSS + layout::BULB_DROP) * s);
    glyphs::symbol(
        canvas,
        Symbol::Sliders,
        (gx + 0.45 * s, gy),
        layout::BULB_GLYPH * pose.squeeze * s,
        INK_PRIMARY,
        alpha,
    );
}

/// Smoothstep.
fn smooth(x: f32) -> f32 {
    let t = x.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The update and permissions badges (permissions, amber, wins): at rest on the foot's lower
/// edge on the bezel's side (0.74 along and 0.62 across the cap's radius from the end ring),
/// riding out with the bulb onto its edge (0.72 of its radius round it, both ways).
fn draw_bulb_marks(
    canvas: &mut Canvas,
    frame: Frame,
    s: f32,
    (swell, squeeze): (f32, f32),
    badges: Badges,
) {
    if !badges.permissions && !badges.update {
        return;
    }
    let swell = swell.clamp(0.0, 1.0);
    let (centre, radius) = bulb_place(swell);
    let r = radius * squeeze;
    let cap = layout::CORNER;
    let foot = (cap * 0.74, cap * 0.62);
    let out = (centre + r * 0.72, r * 0.72);
    let along = foot.0 + (out.0 - foot.0) * swell;
    let aside = foot.1 + (out.1 - foot.1) * swell;
    let at = frame.at(
        (layout::handle_ring_along(frame.edge) + along) * s,
        (layout::RING_ACROSS - aside
            + layout::BADGE_DROP
            + (layout::BULB_DROP - layout::BADGE_DROP) * swell)
            * s,
    );
    if badges.permissions {
        // The Mac's 6pt dot with its notch-black border drawn inside it (4 design px wide). The
        // Mac fills the circle and strokes the border over the same edge (`NotchBulb`), so the
        // fill's anti-aliased edge pixels keep a faint ring of the badge colour outside the
        // border (coverage c times 1 - c): the colour is laid under the border's disc, as it is.
        dot(canvas, at, 3.0 * s, layout::BADGE_PERMISSIONS);
        dot(canvas, at, 3.0 * s, 0x000000);
        dot(
            canvas,
            at,
            (3.0 - 4.0 * layout::DESIGN) * s,
            layout::BADGE_PERMISSIONS,
        );
    } else {
        dot(
            canvas,
            at,
            26.0 * layout::DESIGN / 2.0 * s,
            layout::BADGE_UPDATE,
        );
        dot(canvas, at, 26.0 * layout::DESIGN / 2.0 * s, 0x000000);
        dot(
            canvas,
            at,
            (13.0 * layout::DESIGN - 1.5) * s,
            layout::BADGE_UPDATE,
        );
    }
}

/// A filled dot centred on `(x, y)`.
fn dot(canvas: &mut Canvas, (x, y): (f32, f32), radius: f32, colour: u32) {
    canvas.fill_round_rect(
        x - radius,
        y - radius,
        2.0 * radius,
        2.0 * radius,
        [radius; 4],
        colour,
        1.0,
    );
}

/// One cell: track ring, arcs and the glyph, the ring centred on `(cx, cy)`. The Mac notch
/// draws no percentage under its rings.
/// One ring at `(cx, cy)`, grown by `scale` about its own centre and drawn at `alpha` (the
/// hover emphasis, liquid-motion pass §2: track, arc, inner ring and glyph together).
fn draw_cell(
    canvas: &mut Canvas,
    view: &CellView,
    (cx, cy): (f32, f32),
    s: f32,
    pressed: bool,
    (scale, alpha): (f32, f32),
) {
    let s = s * scale;
    let mut dim = alpha * if view.stale { STALE_DIM } else { 1.0 };
    if pressed {
        dim *= CELL_PRESS_DIM;
    }
    let radius = (RING / 2.0 - TRACK_STROKE / 2.0) * s;
    canvas.stroke_arc(
        cx,
        cy,
        radius,
        TRACK_STROKE * s,
        1.0,
        0xFFFFFF,
        RING_TRACK_ALPHA * dim,
    );
    // A spent limit reads as exhausted whatever the arc says; otherwise the reading's own
    // band (Memory) or the share's band decides.
    let colour = |fraction: f32| {
        if view.blocked {
            layout::BAND_CRITICAL
        } else {
            usage_color(fraction, view.provider)
        }
    };
    if let Some(main) = view.main {
        let fraction = f32::from(main) / 100.0;
        let ring = match view.band {
            Some(band) if !view.blocked => tint(band),
            _ => colour(fraction),
        };
        canvas.stroke_arc(cx, cy, radius, PROGRESS_STROKE * s, fraction, ring, dim);
    }
    // An agent is working: a quarter arc turns on the inner circle, as the Mac's activity arc.
    if view.working {
        let start = f32::from_bits(ACTIVITY_START.load(Ordering::Relaxed));
        canvas.stroke_arc_from(
            cx,
            cy,
            radius * ACTIVITY_RATIO,
            PROGRESS_STROKE * s,
            start,
            ACTIVITY_SWEEP,
            0xFFFFFF,
            0.9 * dim,
        );
    }
    // The second ring: inside the main ring, outside it, or not drawn; solid or dashed.
    let second = match crate::host::weekly_ring() {
        crate::host::WeeklyRing::Off => None,
        crate::host::WeeklyRing::Inside => Some(WEEKLY_RADIUS),
        crate::host::WeeklyRing::Outside => Some(WEEKLY_OUTSIDE_RADIUS),
    };
    if let (Some(inner), Some(second)) = (view.inner, second) {
        let fraction = f32::from(inner) / 100.0;
        let (track, progress) = (RING_TRACK_ALPHA * 0.7 * dim, 0.8 * dim);
        let (radius, width) = (second * s, WEEKLY_STROKE * s);
        if crate::host::switch_on("weeklyRingDashed") {
            let dash = (WEEKLY_DASH.0 * s, WEEKLY_DASH.1 * s);
            canvas.stroke_arc_dashed(cx, cy, radius, width, 1.0, dash, 0xFFFFFF, track);
            canvas.stroke_arc_dashed(
                cx,
                cy,
                radius,
                width,
                fraction,
                dash,
                colour(fraction),
                progress,
            );
        } else {
            canvas.stroke_arc(cx, cy, radius, width, 1.0, 0xFFFFFF, track);
            canvas.stroke_arc(cx, cy, radius, width, fraction, colour(fraction), progress);
        }
    }
    glyphs::draw(
        canvas,
        Glyph::from_key(view.glyph),
        (cx, cy),
        s,
        if view.problem {
            layout::BAND_WATCH
        } else {
            INK_PRIMARY
        },
        if view.blocked {
            dim * BLOCKED_GLYPH
        } else {
            dim
        },
    );
}

/// A single cell on a transparent canvas (no notch body): what the view shots show for a ring.
/// `emphasis` is the hover's: 1 hovered (1.12, on a canvas that fits it), -1 dulled, 0 plain.
#[cfg_attr(not(feature = "qa-views"), allow(dead_code))] // used by the qa-views screenshots
pub fn render_cell(view: &CellView, dpi: u32, _text: &mut TextPainter, emphasis: i32) -> Canvas {
    let s = layout::scale(dpi);
    let ring = match emphasis {
        1.. => (motion::EMPHASIS_SCALE, 1.0),
        ..0 => (1.0, motion::DULLED),
        0 => (1.0, 1.0),
    };
    let size = (RING * ring.0 * s).round() as usize;
    let mut canvas = Canvas::new(size, size);
    draw_cell(
        &mut canvas,
        view,
        (size as f32 / 2.0, size as f32 / 2.0),
        s,
        false,
        ring,
    );
    canvas
}

/// `render_cell` on a canvas as large as the idle ring's with `room` pixels more on every side,
/// the ring centred on it: the Mac's hovered ring is drawn larger than the frame its idle ring
/// has and is not clipped by it.
#[cfg_attr(not(feature = "qa-views"), allow(dead_code))] // used by the qa-views screenshots
pub fn render_cell_padded(view: &CellView, dpi: u32, emphasis: i32, room: usize) -> Canvas {
    let s = layout::scale(dpi);
    let ring = match emphasis {
        1.. => (motion::EMPHASIS_SCALE, 1.0),
        ..0 => (1.0, motion::DULLED),
        0 => (1.0, 1.0),
    };
    let size = (RING * s).round() as usize + 2 * room;
    let mut canvas = Canvas::new(size, size);
    draw_cell(
        &mut canvas,
        view,
        (size as f32 / 2.0, size as f32 / 2.0),
        s,
        false,
        ring,
    );
    canvas
}

// ---- the hover card ---------------------------------------------------------------------------
//
// Geometry follows the Mac card (`TooltipCard.swift`, `NotchLayout`): its design frame is
// 44/117 pt per frame pixel and a Mac point is one DIP here, so every figure below is the Mac
// figure at that scale. The Mac sizes the card from font line boxes (`BUDGET`) and lays the
// text out in SwiftUI's tighter ones (`DRAWN`); keeping both reproduces its bottom margin.

/// Line boxes: body line and title line.
#[derive(Clone, Copy, PartialEq)]
struct Metrics {
    line: f32,
    title: f32,
    /// Extra height of a meter's label row when it has no trailing text: the Mac's row is
    /// taller then (measured, the System card's CPU and memory meters), drawn only.
    bare: f32,
    /// The height the Claude card's sync button gives its header, drawn only: the Mac centres
    /// the button in the header row it draws but sizes the card without it.
    button: f32,
    /// Sizing the card as the Mac's budget does, not laying out what is drawn: a group's box
    /// is budgeted with one block gap between its meters, where it draws two.
    sizing: bool,
}

/// What the text is laid out in.
const DRAWN: Metrics = Metrics {
    line: 11.0,
    title: 16.0,
    bare: 3.0,
    button: PILL_HEIGHT,
    sizing: false,
};
/// The Send card's lines: its rows sit 18.75 pt apart on the Mac (measured, 37.5 px at 2x), which is
/// the 7.5 pt block gap and an 11.25 pt text line.
const SEND: Metrics = Metrics {
    line: 11.25,
    title: 17.0,
    bare: 0.0,
    button: 0.0,
    sizing: false,
};
/// What the card's height is budgeted in.
const BUDGET: Metrics = Metrics {
    line: 12.0,
    title: 17.0,
    bare: 0.0,
    button: 0.0,
    sizing: true,
};

// The toast (liquid-motion pass §4): one anatomy for every card the notch shows on its own (the
// cards with a lead: installer, update, nearby sharing), from the mock's `layoutNew`.
const TOAST_WIDTH: f32 = 196.0;
const TOAST_PAD: f32 = 11.0;
const TOAST_RADIUS: f32 = 14.0;
/// The inline glyph before the title: its size and its centre from the padding corner (the
/// Mac centres it on the title's 15pt line).
const TOAST_GLYPH: f32 = 13.0;
const TOAST_GLYPH_AT: (f32, f32) = (6.5, 7.5);
const TOAST_TEXT_X: f32 = TOAST_PAD + 17.0;
/// The Mac's 12.5pt semibold title in Segoe UI em (as `TITLE_SIZE` is for its 13.7pt); its
/// lines are the Mac's 15pt `NoticeLayout.titleLine` apart.
const TOAST_TITLE_SIZE: f32 = 12.8;
const TOAST_TITLE_BASELINE: f32 = 11.5;
const TOAST_TITLE_LINE: f32 = 15.0;
const TOAST_DETAIL_GAP: f32 = 4.0;
const TOAST_DETAIL_BASELINE: f32 = 8.5;
const TOAST_DETAIL_LINE: f32 = 12.0;
const TOAST_PROGRESS: f32 = 11.0;
const TOAST_CHECK: f32 = 13.0;
const TOAST_ACTIONS_GAP: f32 = 9.0;
const TOAST_PILL: f32 = 22.0;
/// A toast's pills under the Mac's `NoticePill`: a 9pt semibold label (9.9 em in Segoe UI, as
/// its 9.5pt detail is 10.5), an 8.5pt symbol, 9pt sides, 3pt inside, 5pt apart; and the
/// cross of its round close (the Mac's 7.5pt bold xmark).
const TOAST_PILL_SIZE: f32 = 9.9;
const TOAST_PILL_SYMBOL: f32 = 9.0;
const TOAST_PILL_PAD: f32 = 9.0;
const TOAST_PILL_INNER: f32 = 3.5;
const TOAST_PILL_GAP: f32 = 5.0;
const TOAST_X: f32 = 12.0;
/// A toast's device row (the Mac's `SendDeviceRow`): 28pt tall, 3pt apart, 4pt below the text,
/// 9pt side padding, corner 10, the symbol 11pt in a 13pt frame, 6pt to the name; the name
/// (10.5pt semibold, 10.8 em) and model (8pt, 8.8 em) baselines from the row's top.
const TOAST_ROW: f32 = 28.0;
const TOAST_ROW_GAP: f32 = 3.0;
const TOAST_ROWS_TOP: f32 = 4.0;
const TOAST_ROW_PAD: f32 = 9.0;
const TOAST_ROW_RADIUS: f32 = 10.0;
const TOAST_ROW_ICON: f32 = 11.0;
/// The square a file row's thumbnail is fitted in, in points.
const FILE_THUMB: f32 = 22.0;
const TOAST_ROW_TEXT_X: f32 = TOAST_ROW_PAD + TOAST_GLYPH + 6.0;
const TOAST_ROW_NAME: f32 = 10.8;
const TOAST_ROW_MODEL: f32 = 8.8;
const TOAST_ROW_NAME_BASELINE: f32 = 12.0;
const TOAST_ROW_MODEL_BASELINE: f32 = 23.0;
/// The send arrow at a device row's end: its size and its centre in from the row's edge.
const TOAST_ROW_ARROW: f32 = 8.5;
const TOAST_ROW_ARROW_IN: f32 = TOAST_ROW_PAD + 4.0;
/// The empty list's spinner (a 12pt frame) and its gap to the line.
const TOAST_ROW_SPINNER: f32 = 12.0;
const TOAST_ROW_SPINNER_GAP: f32 = 6.0;
/// Title ink (white at 88%, `CARD_INK`), detail ink (white at 68%, `CARD_INK_SECONDARY`), and
/// a warning's ink.
const TOAST_TITLE_INK: u32 = CARD_INK;
const TOAST_DETAIL_INK: u32 = CARD_INK_SECONDARY;
/// Every card's inks (the Mac's `NoticeMetrics`): titles, glyphs and controls white at 88%, details
/// and quiet text white at 68%. Measured on the Mac's 2x shots, over the card's black these are
/// 224 and 173 (flat text strokes in a dozen toasts and hover cards), which is these two
/// constants; over a pill's or a round button's plate (white at 14%) the same inks read 229 and
/// 185, which `over_pill` gives.
const CARD_INK: u32 = 0xE0E0E0;
const CARD_INK_SECONDARY: u32 = 0xADADAD;
const CARD_INK_ALPHA: f32 = 0.88;
const CARD_DETAIL_ALPHA: f32 = 0.68;
/// A tooltip card's inks (the Mac's `TooltipCard`): title, glyph and primary text pure white
/// (`Palette.textPrimary`), details white at 68% as the Mac's notch hands every hover card
/// (`tooltipSecondaryInk` is `NoticeMetrics.detailInk`; 173 on the 2x shots in each tooltip,
/// not `readableTooltipTextSecondary`'s 194). The toasts keep the softer 88% of the Mac's
/// `NoticeCard` for their titles.
const TIP_INK: u32 = 0xFFFFFF;
const TIP_INK_SECONDARY: u32 = 0xADADAD;
const TOAST_WARNING_INK: u32 = 0xE08A47;
/// A limit reached (the Mac's `alertInk`): the warning line and the provider mark on a toast.
const TOAST_ALERT_INK: u32 = 0xE5675F;
/// The line of a toast's small spinners (the Mac's `CardProgress` scaled to 0.7 and 0.55).
const TOAST_SPINNER_STROKE: f32 = 1.4;
/// The Mac's 14pt ring (2pt line, round caps) scaled 0.7 in a title or a list's wait, 0.55 in a
/// pill.
const TOAST_SPINNER_RADIUS: f32 = 4.9;
const PILL_SPINNER_RADIUS: f32 = 3.85;
const PILL_SPINNER_STROKE: f32 = 1.1;
/// The life ring round a toast's ×: its radius (the Mac's 24.4pt frame), its line and its
/// white.
const LIFE_RADIUS: f32 = 12.2;
const LIFE_STROKE: f32 = 1.4;
const LIFE_ALPHA: f32 = 0.45;
/// The most lines a toast's detail runs to (its title runs to two).
const TOAST_DETAIL_LINES: usize = 4;

const CARD_WIDTH: f32 = 225.6;
const CARD_PAD: f32 = 12.0;
const CARD_RADIUS: f32 = 14.0;
/// Room under the Send card's bottom bar (measured on the Mac's card).
const SEND_BOTTOM: f32 = 6.5;
/// The rows after a Send card's progress caption sit 2.5 pt lower on the Mac (measured, 5 px at 2x).
const SEND_METER_AFTER: f32 = 2.5;
/// A flow card's foot is a little taller on the Mac than the padding alone: measured on the
/// Sign-in, Error and Access-denied tooltips, 61.265 pt against 60.9 for 12 + 17 + 7.9 + 12 + 12.
/// The card keeps this height unrounded (`card_rect_exact`): the Mac's bottom edge is a partly
/// covered row, where rounding it to a whole row made the card 1 px taller than the Mac's
/// whenever the Mac's fraction was under a half.
const CARD_BOTTOM_EXTRA: f32 = 0.365;

/// The neck's width where it meets the card and the notch, and its sides' pulled-in control.
const NECK_WIDTH: f32 = 30.0;
/// A card's length along the notch while it is a strip, before it opens (the Mac's `strip`).
const STRIP_LENGTH: f32 = 34.0;
const NECK_WAIST_CONTROL: f32 = 12.0;
/// Room past the card's far side for the slide's overshoot.
const SLIDE_MARGIN: f32 = 8.0;
/// Em sizes in Segoe UI: the title matches the Mac's SF cap height (13.7 pt); the body is
/// sized to the Mac's measured text widths (SF sets about 8% wider at one cap height).
const TITLE_SIZE: f32 = 14.0;
const BODY_SIZE: f32 = 10.5;
/// Segoe UI's ascent in em: cell top to baseline.
const ASCENT: f32 = 1.079;
/// Line top to baseline in the Mac's layout (SF ascender 0.952 em).
const TITLE_BASELINE: f32 = 12.6;
const BODY_BASELINE: f32 = 9.0;
/// Pitch of a wrapped paragraph's lines.
const WRAP_PITCH: f32 = 12.0;
const MARK_SIZE: f32 = 17.3;
const MARK_GAP: f32 = 6.4;
const HEADER_TO_BLOCK: f32 = 7.9;
const BLOCK_GAP: f32 = 7.5;
const LABEL_TO_BAR: f32 = 6.3;
const BAR_TO_USED: f32 = 6.7;
const BAR_HEIGHT: f32 = 3.95;
const STATUS_DOT: f32 = 6.4;
const STATUS_GAP: f32 = 4.1;
const PAUSE_DISC: f32 = 6.15;
const PAUSE_BAR: f32 = 0.78;
const PAUSE_BAR_HEIGHT: f32 = 2.4;
const BUTTON_HEIGHT: f32 = 21.5;
const BUTTON_RADIUS: f32 = 6.0;
/// A button plate's side padding, its symbol, and the gap after the symbol or before the detail.
const BUTTON_PAD: f32 = 8.0;
const BUTTON_SYMBOL: f32 = 12.0;
const BUTTON_INNER: f32 = 5.0;
/// Space kept between a label and its value.
const VALUE_GAP: f32 = 7.5;
/// A grouped limits box (the Mac's `TooltipCard` group): title inset, title to box, box
/// padding, corner radius, outline, and the gap above a group that follows other rows.
const GROUP_TITLE_INSET: f32 = 1.5;
const GROUP_TITLE_GAP: f32 = 4.5;
const GROUP_PAD: f32 = 6.0;
const GROUP_RADIUS: f32 = 7.5;
const GROUP_STROKE: f32 = 0.56;
const GROUP_ALPHA: f32 = 0.25;
const GROUP_GAP: f32 = 10.5;
const GROUP_BOTTOM: f32 = 3.5;
/// White over the black card: bar tracks and button plates (0.176).
const TRACK_ALPHA: f32 = 0.176;

/// The wide card of the disk image and update cards (820 design px).
const WIDE_WIDTH: f32 = 308.3;
/// The line of a toast's box to tick.
const CHECK_STROKE: f32 = 1.0;
/// The toast's box to tick: 8pt with 2pt corners, its label the Mac's 8.5pt caption at the
/// toast's text scale.
const CHECK_BOX: f32 = 8.0;
const CHECK_RADIUS: f32 = 2.0;
const CHECK_LABEL_SIZE: f32 = 9.35;
/// A toast's bar: the Mac's `NoticeBar`, 3.5pt tall, a white 0.16 track under a white 0.9 fill
/// (41 and 234 on the card's black).
const PROGRESS_HEIGHT: f32 = 3.5;
/// The update card's bar: 16 design px tall, a 0.16 track and a 0.9 fill.
const HEAVY_HEIGHT: f32 = 6.0;
const HEAVY_TRACK_ALPHA: f32 = 0.16;
const HEAVY_FILL_ALPHA: f32 = 0.9;
const PROGRESS_TRACK_ALPHA: f32 = 0.16;
const PROGRESS_FILL_ALPHA: f32 = 0.9;
/// The fill of an indeterminate bar in a still, and its width while it moves.
const INDETERMINATE: f32 = 0.35;
const STRIPE: f32 = 0.3;
const PILL_HEIGHT: f32 = 24.8;
const PILL_PAD: f32 = 10.5;
const PILL_GAP: f32 = 5.3;
const PILL_INNER: f32 = 3.8;
const PILL_SYMBOL: f32 = 14.0;
const PILL_ALPHA: f32 = 0.14;
/// The cross of a round close button.
const X_SIZE: f32 = 15.0;
const HEAD_HEIGHT: f32 = 28.6;
const HEAD_GAP: f32 = 5.3;
const DISMISS_BOX: f32 = 16.0;
const DEVICE_HEIGHT: f32 = 31.6;
const DEVICE_GAP: f32 = 3.8;
const DEVICE_RADIUS: f32 = 12.0;
const DEVICE_PAD: f32 = 9.8;
const DEVICE_ICON_FRAME: f32 = 21.0;
const DEVICE_ICON: f32 = 14.0;
const DEVICE_TEXT_GAP: f32 = 8.3;
const DEVICE_ALPHA: f32 = 0.10;
/// The Send hover card's tiles (#471): three to a row, wider than tall (the received items,
/// then the "Paste and send to" boxes).
pub const TILE_COLS: usize = 3;
/// Device boxes are shorter than they are wide; received tiles are square (#474).
const TILE_ASPECT: f32 = 0.66;
const TILE_GAP: f32 = 6.0;
const TILE_RADIUS: f32 = 8.0;
/// A box that cannot be reached, as a share of full strength (the Mac's `opacity(0.4)`).
const OFFLINE_FADE: f32 = 0.4;
/// The default computer's outline (the Mac's `white.opacity(0.25)`).
const DEFAULT_OUTLINE_ALPHA: f32 = 0.25;
const TILE_TEXT_SIZE: f32 = 7.5;
const TILE_TEXT_PITCH: f32 = 9.0;
const TILE_TEXT_LINES: usize = 4;
const TILE_TEXT_PAD: f32 = 5.0;
/// The shell's icon of a file that has no picture, drawn this big (DIPs) in its tile.
const TILE_ICON: f32 = 26.0;
const SPINNER: f32 = 16.5;
const SPINNER_GAP: f32 = 6.0;
const SPINNER_STROKE: f32 = 2.0;
const DEVICE_NAME_GAP: f32 = 0.8;

/// Width in DIPs a string would occupy.
fn dip_width(text: &mut TextPainter, value: &str, size: f32, bold: bool, s: f32) -> f32 {
    text_width(text, value, size, bold, s) as f32 / s
}

/// `value` cut at the end with an ellipsis to at most `max` DIPs (`TextPainter::fit_end`).
fn fit_tail(
    text: &mut TextPainter,
    value: &str,
    (size, bold): (f32, bool),
    max: f32,
    s: f32,
) -> String {
    text.fit_end(value, px(size, s), bold, max * s)
}

/// `value` cut in the middle with an ellipsis to at most `max` DIPs, so the start of a name
/// and the count or extension at its end both stay readable (`TextPainter::fit_middle`).
fn fit_middle(
    text: &mut TextPainter,
    value: &str,
    (size, bold): (f32, bool),
    max: f32,
    s: f32,
) -> String {
    text.fit_middle(value, px(size, s), bold, max * s)
}

/// Greedy word wrap of `value` at body size to `max` DIPs, at most `lines` lines: what does
/// not fit on the last one is cut with an ellipsis (the Mac's `lineLimit(2)`).
fn wrap_measured(
    text: &mut TextPainter,
    value: &str,
    max: f32,
    lines: usize,
    s: f32,
) -> Vec<String> {
    wrap_sized(text, value, max, lines, (BODY_SIZE, false), s)
}

/// `wrap_measured` in another font size and weight (`TextPainter::wrap`).
fn wrap_sized(
    text: &mut TextPainter,
    value: &str,
    max: f32,
    lines: usize,
    (size, bold): (f32, bool),
    s: f32,
) -> Vec<String> {
    text.wrap(value, px(size, s), bold, max * s, lines)
}

/// Where one row sits, in DIPs from the card's top-left, and what its drawing needs measured.
#[derive(Clone, Default)]
struct Placed {
    top: f32,
    height: f32,
    /// The lines of a wrapped text row.
    lines: Vec<String>,
    /// Left edge and width of each button of a button row (the close is the last, a circle).
    spans: Vec<(f32, f32)>,
}

/// A card laid out: its size and where everything goes, from one measuring of the text.
struct Plan {
    width: f32,
    height: f32,
    /// Left edge of a banner's text column.
    text_left: f32,
    /// A toast's wrapped title and detail lines.
    title: Vec<String>,
    detail: Vec<String>,
    rows: Vec<Placed>,
    /// Each header button's rectangle `(x, y, w, h)`.
    head: Vec<(f32, f32, f32, f32)>,
}

/// Left edge and width of each pill of a button row from `left`, then the close circle. Pills
/// that together would pass `right` are all narrowed in proportion (their labels then end in
/// an ellipsis), so a row never spills out of its card. A `toast`'s pills are the Mac's smaller
/// `NoticePill`s.
fn button_spans(
    buttons: &[Button],
    close: bool,
    (left, right): (f32, f32),
    toast: bool,
    text: &mut TextPainter,
    s: f32,
) -> Vec<(f32, f32)> {
    let (pad, symbol_width, gap) = if toast {
        (
            TOAST_PILL_PAD,
            TOAST_PILL_SYMBOL + TOAST_PILL_INNER,
            TOAST_PILL_GAP,
        )
    } else {
        (PILL_PAD, PILL_SYMBOL + PILL_INNER, PILL_GAP)
    };
    let size = if toast { TOAST_PILL_SIZE } else { BODY_SIZE };
    let natural: Vec<f32> = buttons
        .iter()
        .map(|button| {
            let label = dip_width(text, &button.label, size, true, s);
            let symbol = if button.symbol.is_some() || button.glyph.is_some() || button.busy {
                symbol_width
            } else {
                0.0
            };
            2.0 * pad + symbol + label
        })
        .collect();
    let gaps = gap * natural.len().saturating_sub(1) as f32;
    let close_room = if close { gap + PILL_HEIGHT } else { 0.0 };
    let total: f32 = natural.iter().sum();
    let available = (right - left - gaps - close_room).max(0.0);
    let shrink = if total > available && total > 0.0 {
        available / total
    } else {
        1.0
    };
    let mut spans = Vec::new();
    let mut x = left;
    for width in natural.iter().map(|width| width * shrink) {
        spans.push((x, width));
        x += width + gap;
    }
    if close {
        spans.push((x, PILL_HEIGHT));
    }
    spans
}

/// Gap between the Send bar's two plates: the Mac's HStack spacing.
const BAR_GAP: f32 = 8.0;
/// The Tools card's pill grid: the gap between its rows (the Mac's `LazyVGrid` spacing).
const PILL_ROW_GAP: f32 = 8.0;
/// Gap between a Tools pill's symbol and its label.
const TOOL_PILL_INNER: f32 = 7.0;

/// A Tools pill's height: the Mac's plate is its content's, 4 pt above and below, so the
/// taller symbols make a taller plate (traced: 19 pt, the lock 19.5, the clipboard 22).
fn pill_height(symbol: Symbol) -> f32 {
    match symbol {
        Symbol::Lock => 19.5,
        Symbol::ClipboardPlain => 22.0,
        _ => 19.0,
    }
}

/// How wide a Tools pill's symbol reads (the ink of the Mac's 2x render, in DIPs).
fn pill_symbol_width(symbol: Symbol) -> f32 {
    match symbol {
        Symbol::Lock => 6.5,
        Symbol::ClipboardPlain => 8.0,
        _ => 11.0,
    }
}

/// The height of a row of Tools pills: the tallest plate in it.
fn pills_height(pills: &[Pill]) -> f32 {
    pills
        .iter()
        .map(|pill| pill_height(pill.symbol))
        .fold(0.0, f32::max)
}

/// Left edge and width of a pill row's two plates: equal-width buttons that fill the row.
fn bar_spans((left, right): (f32, f32)) -> Vec<(f32, f32)> {
    let width = (right - left - BAR_GAP) / 2.0;
    vec![(left, width), (right - width, width)]
}

/// How much taller a meter's label row is for having no trailing text.
fn bare_extra(trailing: &str, m: Metrics) -> f32 {
    if trailing.is_empty() { m.bare } else { 0.0 }
}

/// Height of one meter: label line, bar (none without a share), summary line.
fn meter_height(fraction: Option<f32>, trailing: &str, m: Metrics) -> f32 {
    let bar = if fraction.is_some() {
        LABEL_TO_BAR + BAR_HEIGHT
    } else {
        0.0
    };
    m.line + bare_extra(trailing, m) + bar + BAR_TO_USED + m.line
}

/// Where each meter of a group sits inside its box (DIPs from the box's top) and the box's
/// height: padding, the meters two block gaps apart (the Mac's VStack spacing and the row's top
/// padding both apply inside a box), padding.
fn group_tops(rows: &[Row], m: Metrics) -> (Vec<f32>, f32) {
    let mut y = GROUP_PAD;
    let mut tops = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            y += 2.0 * BLOCK_GAP;
        }
        tops.push(y);
        if let Row::Meter {
            fraction, trailing, ..
        } = row
        {
            y += meter_height(*fraction, trailing, m);
        }
    }
    (tops, y + GROUP_PAD)
}

/// A closed rounded-rectangle outline.
fn round_rect_outline(x: f32, y: f32, w: f32, h: f32, r: f32) -> Vec<(f32, f32)> {
    const STEPS: usize = 8;
    let r = r.min(w / 2.0).min(h / 2.0);
    let corners = [
        (x + w - r, y + r, -std::f32::consts::FRAC_PI_2),
        (x + w - r, y + h - r, 0.0),
        (x + r, y + h - r, std::f32::consts::FRAC_PI_2),
        (x + r, y + r, std::f32::consts::PI),
    ];
    let mut points = Vec::new();
    for (cx, cy, start) in corners {
        for step in 0..=STEPS {
            let angle = start + std::f32::consts::FRAC_PI_2 * step as f32 / STEPS as f32;
            points.push((cx + r * angle.cos(), cy + r * angle.sin()));
        }
    }
    points
}

/// Height in DIPs of the lines of a wrapped paragraph.
fn lines_height(count: usize, m: Metrics) -> f32 {
    (count.max(1) - 1) as f32 * WRAP_PITCH + m.line
}

/// Whether the file is a kind the shell draws a real picture of (an image, a PDF, a video),
/// not just its icon.
fn has_picture(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "png"
                    | "jpg"
                    | "jpeg"
                    | "gif"
                    | "bmp"
                    | "webp"
                    | "tif"
                    | "tiff"
                    | "pdf"
                    | "mp4"
                    | "mov"
                    | "mkv"
                    | "avi"
                    | "heic"
            )
        })
}

/// A tile's width in DIPs for a row `room` DIPs wide.
fn tile_side(room: f32) -> f32 {
    (room - (TILE_COLS - 1) as f32 * TILE_GAP) / TILE_COLS as f32
}

/// A tile's height in DIPs for a row `room` DIPs wide.
fn tile_high(room: f32, items: &[crate::card::Item]) -> f32 {
    let boxes = items
        .iter()
        .any(|i| matches!(i, crate::card::Item::Device { .. }));
    tile_side(room) * if boxes { TILE_ASPECT } else { 1.0 }
}

/// Height of `count` tiles in `room` DIPs.
fn tiles_height(items: &[crate::card::Item], room: f32) -> f32 {
    let lines = items.len().div_ceil(TILE_COLS).max(1);
    lines as f32 * tile_high(room, items) + (lines - 1) as f32 * TILE_GAP
}

/// Tile `n`'s `(x, y, width, height)` relative to the card's left edge and the row's top, in DIPs.
fn tile_rect(n: usize, card_width: f32, items: &[crate::card::Item]) -> (f32, f32, f32, f32) {
    let room = card_width - 2.0 * CARD_PAD;
    let (side, high) = (tile_side(room), tile_high(room, items));
    let (col, line) = (n % TILE_COLS, n / TILE_COLS);
    (
        CARD_PAD + col as f32 * (side + TILE_GAP),
        line as f32 * (high + TILE_GAP),
        side,
        high,
    )
}

/// Height, wrapped lines and button spans of `row` in a plain (non-banner) card.
fn flow_row(
    row: &Row,
    m: Metrics,
    room: f32,
    text: &mut TextPainter,
    s: f32,
) -> (f32, Vec<String>, Vec<(f32, f32)>) {
    // Wrapped by measured width, so a line that fits the column stays on one line (the Mac's
    // `Text` only wraps when it does not fit).
    let paragraph = |text: &mut TextPainter, value: &str, room: f32| {
        let lines = wrap_measured(text, value, room, 8, s);
        (lines_height(lines.len(), m), lines, Vec::new())
    };
    match row {
        Row::Button { .. } => (BUTTON_HEIGHT, Vec::new(), Vec::new()),
        Row::Pills(pills) => (
            pills_height(pills),
            Vec::new(),
            bar_spans((CARD_PAD, CARD_PAD + room)),
        ),
        Row::Pair { .. } => (m.line, Vec::new(), Vec::new()),
        Row::Meter {
            fraction, trailing, ..
        } => (meter_height(*fraction, trailing, m), Vec::new(), Vec::new()),
        Row::Group { rows, .. } => {
            let (_, box_height) = group_tops(rows, m);
            // The Mac's budget: a single block gap between the meters, and the extra 3 pt
            // (px(8)) of bottom room it adds for a group (measured 3.5 against the card).
            let box_height = if m.sizing {
                box_height - (rows.len().max(1) - 1) as f32 * BLOCK_GAP + GROUP_BOTTOM
            } else {
                box_height
            };
            (
                m.line + GROUP_TITLE_GAP + box_height,
                Vec::new(),
                Vec::new(),
            )
        }
        Row::Note(value) | Row::Text(value) | Row::Tinted { text: value, .. } => {
            paragraph(text, value, room)
        }
        Row::Alert(value) => paragraph(text, value, room - STATUS_DOT - STATUS_GAP),
        Row::Buttons { buttons, close } => (
            PILL_HEIGHT,
            Vec::new(),
            button_spans(buttons, *close, (CARD_PAD, CARD_PAD + room), false, text, s),
        ),
        Row::Progress(_) => (PROGRESS_HEIGHT, Vec::new(), Vec::new()),
        Row::Device { .. } | Row::Waiting(_) | Row::File { .. } => {
            (DEVICE_HEIGHT, Vec::new(), Vec::new())
        }
        Row::Tiles(items) => (tiles_height(items, room), Vec::new(), Vec::new()),
    }
}

/// The Claude card's header holds a sync button (or its spinner). The Mac lays that header out
/// as the Codex card's, which has none, with the button centred on the row: the row is the
/// taller of the title block and the button (drawn only; the card is sized without it), and
/// the gap under it is the plain one.
fn claude_head(content: &CardContent) -> bool {
    content.mark == Mark::Claude && content.head.iter().any(|head| *head != Head::Dismiss)
}

/// Whether a round header button makes the header taller and closes the gap under it (the
/// Send card's).
fn round_head(content: &CardContent) -> bool {
    !claude_head(content) && content.head.iter().any(|head| *head != Head::Dismiss)
}

fn header_height(content: &CardContent, m: Metrics) -> f32 {
    let text = m.title
        + if content.subtitle.is_some() {
            m.line
        } else {
            0.0
        };
    let text = if content.mark == Mark::None {
        text
    } else {
        text.max(MARK_SIZE)
    };
    if round_head(content) {
        text.max(HEAD_HEIGHT)
    } else if claude_head(content) {
        text.max(m.button)
    } else {
        text
    }
}

/// Each header button's rectangle: round ones centred on the header's height, a bare dismiss
/// on the title line, right to left from the card's padding.
fn head_rects(content: &CardContent, width: f32, m: Metrics) -> Vec<(f32, f32, f32, f32)> {
    let header = header_height(content, m);
    let mut rects = vec![(0.0, 0.0, 0.0, 0.0); content.head.len()];
    let mut right = width - CARD_PAD;
    for (index, head) in content.head.iter().enumerate().rev() {
        let (side, centre) = if *head == Head::Dismiss {
            (DISMISS_BOX, CARD_PAD + m.title / 2.0)
        } else {
            (PILL_HEIGHT, CARD_PAD + header / 2.0)
        };
        rects[index] = (right - side, centre - side / 2.0, side, side);
        right -= side + PILL_GAP;
    }
    rects
}

/// How many windows the Mac's Tools snapshot holds: a row for each tool (a pill) and the
/// closing hint, which is a row of its own.
fn tools_lines(content: &CardContent) -> usize {
    content
        .rows
        .iter()
        .map(|row| match row {
            Row::Pills(pills) => pills.len(),
            _ => 1,
        })
        .sum()
}

/// A plain card: header, then rows top to bottom.
fn flow_plan(content: &CardContent, m: Metrics, text: &mut TextPainter, s: f32) -> Plan {
    // The Send hover card is the Mac's, whose rows sit a full 12pt line apart: laid out in the
    // budget's lines in the drawn pass too, so what is drawn and what is budgeted agree.
    let send = content.send;
    let tools = matches!(content.rows.first(), Some(Row::Pills(_)));
    let m = if send { SEND } else { m };
    let width = if content.wide { WIDE_WIDTH } else { CARD_WIDTH };
    let round = round_head(content);
    let mut y = CARD_PAD + header_height(content, m);
    let mut rows = Vec::new();
    let mut previous: Option<&Row> = None;
    for row in &content.rows {
        y += match (previous, row) {
            (None, _) if round => HEAD_GAP,
            (None, _) if send => HEADER_TO_BLOCK - 0.5,
            (None, _) => HEADER_TO_BLOCK,
            (Some(Row::Group { .. }), Row::Group { .. }) => GROUP_GAP,
            (Some(_), Row::Group { .. }) if m.sizing => BLOCK_GAP,
            (Some(_), Row::Group { .. }) => GROUP_GAP,
            (Some(Row::Device { .. } | Row::Waiting(_)), Row::Device { .. }) => DEVICE_GAP,
            (Some(Row::File { .. }), Row::File { .. }) => DEVICE_GAP,
            (Some(Row::Pills(_)), Row::Pills(_)) => PILL_ROW_GAP,
            (Some(Row::Meter { .. }), _) if send => BLOCK_GAP + SEND_METER_AFTER,
            _ => BLOCK_GAP,
        };
        let (height, lines, spans) = flow_row(row, m, width - 2.0 * CARD_PAD, text, s);
        rows.push(Placed {
            top: y,
            height,
            lines,
            spans,
        });
        y += height;
        previous = Some(row);
    }
    Plan {
        width,
        height: content.height.unwrap_or(if tools {
            // The Tools card is the Mac's budget, not its content: the header, the gap, one
            // plain line for each tool and the closing hint, a block gap between them
            // (`NotchLayout.cardHeight`).
            let lines = tools_lines(content);
            2.0 * CARD_PAD
                + header_height(content, m)
                + HEADER_TO_BLOCK
                + lines as f32 * m.line
                + lines.saturating_sub(1) as f32 * BLOCK_GAP
        } else {
            y + if send {
                // The Mac budgets the card without the drawn gap under the progress caption.
                SEND_BOTTOM
                    - if content.rows.iter().any(|r| matches!(r, Row::Meter { .. })) {
                        SEND_METER_AFTER - 0.5
                    } else {
                        0.0
                    }
            } else {
                CARD_PAD + CARD_BOTTOM_EXTRA
            }
        }),
        text_left: CARD_PAD,
        title: Vec::new(),
        detail: Vec::new(),
        rows,
        head: head_rects(content, width, m),
    }
}

/// A toast laid out (liquid-motion pass §4, the mock's `layoutNew`, the Mac's `NoticeLayout`):
/// the glyph and the title on one line (two at most), the detail and any notes under it at
/// full width, a bar (only a share draws one), device rows, a box to tick, then the actions
/// row with the × last; without actions the × sits at the top right, beside the title.
fn banner_plan(content: &CardContent, text: &mut TextPainter, s: f32) -> Plan {
    let (width, pad, pill) = (TOAST_WIDTH, TOAST_PAD, TOAST_PILL);
    let actions = toast_actions(content);
    let close_in_row = toast_close(content);
    let title_room = width - pad - TOAST_TEXT_X - if close_in_row { pill + 6.0 } else { 0.0 };
    let title = wrap_sized(
        text,
        &content.title,
        title_room,
        2,
        (TOAST_TITLE_SIZE, true),
        s,
    );
    let room = width - 2.0 * pad;
    let detail = content
        .subtitle
        .as_deref()
        .map(|value| wrap_measured(text, value, room, TOAST_DETAIL_LINES, s))
        .unwrap_or_default();
    // The title-to-text gap belongs to the first line of text: none without any.
    let mut below_title = detail.is_empty();
    let mut y = toast_detail_top(title.len(), close_in_row) - TOAST_DETAIL_GAP
        + if detail.is_empty() {
            0.0
        } else {
            TOAST_DETAIL_GAP + detail.len() as f32 * TOAST_DETAIL_LINE
        };
    let mut rows = Vec::new();
    let mut previous: Option<&Row> = None;
    for row in &content.rows {
        let mut placed = Placed::default();
        match row {
            Row::Tinted { text: value, .. } | Row::Note(value) | Row::Text(value) => {
                if below_title {
                    y += TOAST_DETAIL_GAP;
                    below_title = false;
                }
                placed.lines = wrap_measured(text, value, room, 2, s);
                placed.top = y;
                placed.height = placed.lines.len() as f32 * TOAST_DETAIL_LINE;
                y += placed.height;
            }
            // The Mac draws a bar for a share only: a wait with none draws none.
            Row::Progress(Some(_)) => {
                placed.top = y;
                placed.height = TOAST_PROGRESS;
                y += TOAST_PROGRESS;
            }
            // A box to tick (the incoming card's "don't ask again"): one line.
            Row::Button { label, .. } => {
                placed.lines = vec![label.clone()];
                placed.top = y;
                placed.height = TOAST_CHECK;
                y += TOAST_CHECK;
            }
            // Device rows (the nearby picker): a gap above the block, a smaller one between.
            Row::Device { .. } | Row::Waiting(_) | Row::File { .. } => {
                y += if matches!(previous, Some(Row::Device { .. } | Row::File { .. })) {
                    TOAST_ROW_GAP
                } else {
                    TOAST_ROWS_TOP
                };
                placed.top = y;
                placed.height = TOAST_ROW;
                y += TOAST_ROW;
            }
            _ => {}
        }
        rows.push(placed);
        previous = Some(row);
    }
    let close_x = width - pad - pill;
    for (index, row) in content.rows.iter().enumerate() {
        let Row::Buttons { buttons, close } = row else {
            continue;
        };
        if buttons.is_empty() {
            rows[index].top = pad;
        } else {
            y += TOAST_ACTIONS_GAP;
            rows[index].top = y;
            rows[index].spans = button_spans(
                buttons,
                false,
                (pad, close_x - TOAST_PILL_GAP),
                true,
                text,
                s,
            );
            y += pill;
        }
        rows[index].height = pill;
        if *close {
            rows[index].spans.push((close_x, pill));
        }
    }
    let mut height = y + pad;
    if !actions {
        height = height.max(2.0 * pad + pill);
    }
    Plan {
        width,
        height,
        text_left: pad,
        title,
        detail,
        rows,
        head: Vec::new(),
    }
}

/// Where a toast's detail starts under a title of `lines` lines. With the × in the title's row
/// (no actions row) that row is at least a pill tall and the × clears the detail.
fn toast_detail_top(lines: usize, close_in_row: bool) -> f32 {
    let title = lines.max(1) as f32 * TOAST_TITLE_LINE;
    let title = if close_in_row {
        title.max(TOAST_PILL)
    } else {
        title
    };
    TOAST_PAD + title + TOAST_DETAIL_GAP
}

/// Whether a toast has an actions row of its own (pills besides the ×).
fn toast_actions(content: &CardContent) -> bool {
    content
        .rows
        .iter()
        .any(|row| matches!(row, Row::Buttons { buttons, .. } if !buttons.is_empty()))
}

/// Whether a toast's × sits in its title row: it has one and no actions row to put it in.
fn toast_close(content: &CardContent) -> bool {
    !toast_actions(content)
        && content
            .rows
            .iter()
            .any(|row| matches!(row, Row::Buttons { close: true, .. }))
}

/// A card's corner radius: a toast's, or the hover card's.
fn card_radius(content: &CardContent) -> f32 {
    if content.lead.is_some() {
        TOAST_RADIUS
    } else {
        CARD_RADIUS
    }
}

/// The neck's length in the card's window: from the card's face to 2pt into the notch.
fn neck_length(s: f32, toast: bool) -> i32 {
    px(layout::NECK_REACH + neck_inside(toast), s)
}

/// How far into the notch's black the neck reaches, in DIPs: a toast's and a tooltip's differ.
fn neck_inside(toast: bool) -> f32 {
    if toast {
        layout::NECK_INSIDE_TOAST
    } else {
        layout::NECK_INSIDE
    }
}

/// Plans kept for reuse: sizing, hit testing and every frame of a card's motion ask for the
/// same one, and a plan is a pure function of these inputs (bogusyogi/pulse#558).
const PLANS_KEPT: usize = 6;

/// One kept plan: the content, metrics and scale bits it was made for, and the plan.
type KeptPlan = (CardContent, Metrics, u32, Rc<Plan>);

thread_local! {
    static PLANS: RefCell<Vec<KeptPlan>> = const { RefCell::new(Vec::new()) };
}

fn plan(content: &CardContent, m: Metrics, text: &mut TextPainter, s: f32) -> Rc<Plan> {
    let found = PLANS.with(|kept| {
        let mut kept = kept.borrow_mut();
        let at = kept
            .iter()
            .position(|(c, km, ks, _)| *km == m && *ks == s.to_bits() && c == content)?;
        // Most recently used last.
        let entry = kept.remove(at);
        let laid = entry.3.clone();
        kept.push(entry);
        Some(laid)
    });
    if let Some(laid) = found {
        return laid;
    }
    let laid = Rc::new(if content.lead.is_some() {
        banner_plan(content, text, s)
    } else {
        flow_plan(content, m, text, s)
    });
    PLANS.with(|kept| {
        let mut kept = kept.borrow_mut();
        if kept.len() >= PLANS_KEPT {
            kept.remove(0);
        }
        kept.push((content.clone(), m, s.to_bits(), laid.clone()));
    });
    laid
}

/// The card's rectangle in the window, device pixels: `(x, y, width, height)`; the window is
/// larger by the tail on the side facing the notch.
fn card_rect(content: &CardContent, text: &mut TextPainter, s: f32) -> (i32, i32, i32, i32) {
    card_rect_exact(content, text, s).0
}

/// A plain tooltip (not a toast, not the Send card, whose heights are set) keeps its height
/// unrounded, as the Mac draws it.
fn fractional_height(content: &CardContent) -> bool {
    content.lead.is_none() && content.height.is_none() && !content.send
}

/// `card_rect`, and the card's height in device pixels before it is made whole: the rectangle
/// holds the part-covered last row of a plain tooltip, which `draw_silhouette` covers by the
/// fraction.
fn card_rect_exact(
    content: &CardContent,
    text: &mut TextPainter,
    s: f32,
) -> ((i32, i32, i32, i32), f32) {
    let laid = plan(content, BUDGET, text, s);
    let tail = neck_length(s, content.lead.is_some());
    let far = px(SLIDE_MARGIN, s);
    let exact = laid.height * s;
    let (height, exact) = if fractional_height(content) {
        ((exact - 0.001).ceil() as i32, exact)
    } else {
        let whole = exact.round();
        (whole as i32, whole)
    };
    let width = px(laid.width, s);
    let rect = match content.tail.map(|t| t.edge) {
        Some(Edge::Top) => (0, tail, width, height),
        Some(Edge::Left) => (tail, 0, width, height),
        Some(Edge::Right) => (far, 0, width, height),
        Some(Edge::Bottom) => (0, far, width, height),
        None => (0, 0, width, height),
    };
    (rect, exact)
}

/// The window around `rect`: the card plus its tail.
fn window_size(content: &CardContent, rect: (i32, i32, i32, i32), s: f32) -> (i32, i32) {
    let (x, y, width, height) = rect;
    let tail = neck_length(s, content.lead.is_some()) + px(SLIDE_MARGIN, s);
    match content.tail.map(|t| t.edge) {
        Some(Edge::Left | Edge::Right) => (width + tail, height),
        Some(Edge::Top | Edge::Bottom) => (width, height + tail),
        None => (x + width, y + height),
    }
}

/// Size in device pixels of the card window for `content`: the card plus its tail.
pub fn card_size(content: &CardContent, dpi: u32, text: &mut TextPainter) -> (i32, i32) {
    let s = layout::scale(dpi);
    window_size(content, card_rect(content, text, s), s)
}

/// What is under `(x, y)` (window-local device pixels): a header button, a pill or round
/// button of a button row, or a row that is one thing to press (a text row is not).
pub fn hit_at(
    content: &CardContent,
    dpi: u32,
    text: &mut TextPainter,
    (x, y): (i32, i32),
) -> Option<Hit> {
    let s = layout::scale(dpi);
    let (ox, oy, _, _) = card_rect(content, text, s);
    let (x, y) = ((x - ox) as f32 / s, (y - oy) as f32 / s);
    let laid = plan(content, DRAWN, text, s);
    let inside = |(rx, ry, rw, rh): (f32, f32, f32, f32)| {
        x >= rx - 2.0 && x < rx + rw + 2.0 && y >= ry - 2.0 && y < ry + rh + 2.0
    };
    if let Some(index) = laid.head.iter().position(|rect| inside(*rect)) {
        return Some(Hit::Head(index));
    }
    for (index, (row, placed)) in content.rows.iter().zip(&laid.rows).enumerate() {
        if let Row::Tiles(items) = row {
            let found = (0..items.len()).find(|n| {
                let (tx, ty, side, high) = tile_rect(*n, laid.width, items);
                inside((tx, placed.top + ty, side, high))
            });
            if let Some(tile) = found {
                return Some(Hit::Row(index, tile));
            }
            continue;
        }
        if let Row::Buttons { .. } | Row::Pills(_) = row {
            let found = placed
                .spans
                .iter()
                .position(|&(bx, bw)| inside((bx, placed.top, bw, placed.height)));
            if let Some(button) = found {
                return Some(Hit::Row(index, button));
            }
            continue;
        }
        // A toast's device rows are 3pt apart: each is exactly its own height.
        let slack =
            if content.lead.is_some() && matches!(row, Row::Device { .. } | Row::File { .. }) {
                0.0
            } else {
                BLOCK_GAP / 2.0
            };
        let band = y >= placed.top - slack && y < placed.top + placed.height + slack;
        // On a banner only a row to press is one: a box to tick across the text column, a
        // device across the card.
        let pressable = match row {
            _ if content.lead.is_none() => true,
            Row::Button { .. } => x >= laid.text_left - ROW_HOVER_X,
            Row::Device { .. } | Row::File { .. } => true,
            _ => false,
        };
        if band && pressable {
            return Some(Hit::Row(index, 0));
        }
    }
    None
}

/// The card moves (a spinner or a travelling bar), so the owner should redraw it on a timer.
pub fn animated(content: &CardContent) -> bool {
    content.life.is_some()
        || content.spinner
        || content.head.contains(&Head::Scanning)
        || content.rows.iter().any(|row| match row {
            Row::Progress(None) | Row::Waiting(_) => true,
            Row::Buttons { buttons, .. } => buttons.iter().any(|button| button.busy),
            _ => false,
        })
}

/// Coverage (0..=1) of the polygon at the pixel centre `(px, py)`.
fn polygon_coverage(poly: &[(f32, f32)], (px, py): (f32, f32)) -> f32 {
    let mut inside = false;
    let mut nearest = f32::MAX;
    for (index, &(ax, ay)) in poly.iter().enumerate() {
        let (bx, by) = poly[(index + 1) % poly.len()];
        if (ay > py) != (by > py) && px < (bx - ax) * (py - ay) / (by - ay) + ax {
            inside = !inside;
        }
        let (dx, dy) = (bx - ax, by - ay);
        let length = dx * dx + dy * dy;
        let t = if length > 0.0 {
            (((px - ax) * dx + (py - ay) * dy) / length).clamp(0.0, 1.0)
        } else {
            0.0
        };
        nearest = nearest.min((px - ax - t * dx).hypot(py - ay - t * dy));
    }
    let distance = if inside { -nearest } else { nearest };
    (0.5 - distance).clamp(0.0, 1.0)
}

/// A toast's corner radius in DIPs: the Mac rounds by 14 but no more than the card's length along
/// the notch leaves beside the neck (`NoticeEmergence.moment`, `rx`), so a short card (the
/// 44pt "Copied") has corners of 8.8.
fn toast_corner(content: &CardContent, (width, height): (i32, i32), s: f32) -> f32 {
    let along = match content.tail.map(|t| t.edge) {
        Some(edge) if edge.is_vertical() => height,
        _ => width,
    } as f32
        / s;
    TOAST_RADIUS.min(((along - NECK_WIDTH) / 2.0 - 0.5).max(along * 0.2))
}

/// The card and its neck as one silhouette.
fn draw_silhouette(
    canvas: &mut Canvas,
    content: &CardContent,
    rect: (i32, i32, i32, i32),
    exact_height: f32,
    s: f32,
) {
    let (x, y, width, height) = rect;
    let mut radius = card_radius(content);
    let rect = (x as f32, y as f32, width as f32, exact_height);
    if content.lead.is_some() {
        radius = toast_corner(content, (width, height), s);
    }
    // The Mac's toast outline is built from circular arcs of this radius (`NoticeStage.swift`
    // `outline`, `addArc(radius: rx)`, drawn by `NoticeOutlineShape`), not continuous corners.
    canvas.fill_round_rect(
        rect.0,
        rect.1,
        rect.2,
        rect.3,
        [radius * s; 4],
        0x000000,
        1.0,
    );
    if let Some(tail) = content.tail {
        let body = (x as f32, y as f32, width as f32, exact_height);
        // The Mac's toast neck leaves a card face one point narrower than the straight left by the
        // corners (`NoticeEmergence.outline`).
        let slack = if content.lead.is_some() { 0.5 } else { 0.0 };
        draw_neck(
            canvas,
            tail,
            body,
            (radius + slack) * s,
            s,
            (
                neck_length(s, content.lead.is_some()) as f32,
                neck_inside(content.lead.is_some()),
            ),
        );
    }
}

/// The neck that joins the card to its ring, black over the card's edge (the Mac's 27fabf97):
/// it leaves the card's face at 30pt (no wider than the face's straight part), pinches to a
/// waist of about 21pt (both sides quadratic, the control pulled to 12pt) and widens to 30pt
/// where it reaches 2pt into the notch's black, `length` device pixels from the card's face.
fn draw_neck(
    canvas: &mut Canvas,
    tail: crate::card::Tail,
    (x, y, width, height): (f32, f32, f32, f32),
    radius: f32,
    s: f32,
    (length, inside): (f32, f32),
) {
    let extent = if tail.edge.is_vertical() {
        height
    } else {
        width
    };
    let full = NECK_WIDTH * s;
    let face = full.min((extent - 2.0 * radius).max(0.0));
    if face <= 0.0 {
        return;
    }
    // Along the card's face: on the ring's line, slid no nearer a corner than the face allows.
    let limit = (extent / 2.0 - radius - face / 2.0).max(0.0);
    let slide = (tail.offset as f32).clamp(-limit, limit);
    // A point `out` from the card's face toward the notch and `side` across it.
    let place = |out: f32, side: f32| match tail.edge {
        Edge::Right => (x + width + out, y + height / 2.0 + slide + side),
        Edge::Left => (x - out, y + height / 2.0 + slide + side),
        Edge::Bottom => (x + width / 2.0 + slide + side, y + height + out),
        Edge::Top => (x + width / 2.0 + slide + side, y - out),
    };
    let quadratic = |(a, b, c): (f32, f32, f32), t: f32| {
        let u = 1.0 - t;
        u * u * a + 2.0 * u * t * b + t * t * c
    };
    let waist = (
        (length - inside * s).max(0.0) / 2.0,
        NECK_WAIST_CONTROL / 2.0 * s,
    );
    // One pixel into the card so the two meet without a seam.
    let ends = (-1.0, length);
    let side = |sign: f32, t: f32| {
        let out = quadratic((ends.0, waist.0, ends.1), t);
        let across = quadratic((face / 2.0, waist.1, full / 2.0), t);
        place(out, sign * across)
    };
    let mut outline: Vec<(f32, f32)> = (0..=24).map(|step| side(1.0, step as f32 / 24.0)).collect();
    outline.extend((0..=24).rev().map(|step| side(-1.0, step as f32 / 24.0)));
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(px, py) in &outline {
        (x0, y0, x1, y1) = (x0.min(px), y0.min(py), x1.max(px), y1.max(py));
    }
    let x0 = x0.floor().max(0.0) as usize;
    let y0 = y0.floor().max(0.0) as usize;
    let x1 = (x1.ceil().max(0.0) as usize).min(canvas.width);
    let y1 = (y1.ceil().max(0.0) as usize).min(canvas.height);
    for py in y0..y1 {
        for px in x0..x1 {
            let coverage = polygon_coverage(&outline, (px as f32 + 0.5, py as f32 + 0.5));
            if coverage > 0.0 {
                // Black source-over: premultiplied colour stays zero, only alpha grows.
                let pixel = &mut canvas.pixels[py * canvas.width + px];
                let alpha = (*pixel >> 24) as f32 / 255.0;
                let alpha = coverage + alpha * (1.0 - coverage);
                *pixel = ((alpha * 255.0).round() as u32) << 24;
            }
        }
    }
}

/// Text drawing for the card, on a baseline in device pixels.
struct Pen<'a> {
    canvas: &'a mut Canvas,
    text: &'a mut TextPainter,
    s: f32,
    /// Where an animation is in its loop (0..1); `None` draws a still.
    phase: Option<f32>,
    /// A pill's height: a toast's are 22pt.
    pill: f32,
    /// The buttons held down: each draws at `PRESS_SCALE` about its middle.
    pressed: &'a [Hit],
    /// The buttons under the pointer or held down: a pill's label and glyph read white.
    lit: &'a [Hit],
    /// The press shrink in force inside `scaled` (1 otherwise): text is fitted at full size.
    squeeze: f32,
}

#[derive(Clone, Copy)]
struct Style {
    size: f32,
    bold: bool,
    color: u32,
}

fn body(color: u32) -> Style {
    Style {
        size: BODY_SIZE,
        bold: false,
        color,
    }
}

/// A pressed button, plate and label, scales down to this about its middle (the Mac's
/// `CardButtonStyle`).
const PRESS_SCALE: f32 = 0.96;

/// A button that does nothing reads at 0.4: the Mac's `.opacity(enabled ? 1 : 0.4)`.
const DISABLED_ALPHA: f32 = 0.4;

/// `color` at `alpha` over a button's plate (white at `TRACK_ALPHA` times `alpha`): the Mac
/// fades the plate and the ink separately, so a dimmed label is its ink at 0.4 over the
/// dimmed plate, not the ink at 0.4 over black.
fn over_plate(color: u32, alpha: f32) -> u32 {
    let plate = 255.0 * TRACK_ALPHA * alpha;
    let channel = |shift: u32| {
        (((color >> shift) & 0xFF) as f32 * alpha + plate * (1.0 - alpha)).round() as u32
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

/// White at `alpha` over a plate of white at `plate_alpha` on the card's black, as a grey: the
/// Mac draws a label in `NoticeMetrics.titleInk` or `detailInk` over its plate, so the ink reads
/// 229 or 185 on a pill (224 and 173 over the black card) and 181 for a device row's model. The
/// plate's own level is taken down to a whole number first, as the Mac's 8-bit plate is.
fn over_white(plate_alpha: f32, alpha: f32) -> u32 {
    let plate = (255.0 * plate_alpha).floor();
    let level = (255.0 * alpha + plate * (1.0 - alpha)).round() as u32;
    level * 0x01_0101
}

/// `over_white` over a card pill's or round button's plate.
fn over_pill(alpha: f32) -> u32 {
    over_white(PILL_ALPHA, alpha)
}

/// `(x, y, w, h)` shrunk by `k` about its middle.
fn about((x, y, w, h): (f32, f32, f32, f32), k: f32) -> (f32, f32, f32, f32) {
    (
        x + w * (1.0 - k) / 2.0,
        y + h * (1.0 - k) / 2.0,
        w * k,
        h * k,
    )
}

impl Pen<'_> {
    /// How big `hit` draws: `PRESS_SCALE` while it is held down, else 1.
    fn press(&self, hit: Hit) -> f32 {
        if self.pressed.contains(&hit) {
            PRESS_SCALE
        } else {
            1.0
        }
    }

    /// `draw` with every size in it scaled by `k` (the position is the caller's to shrink).
    fn scaled(&mut self, k: f32, draw: impl FnOnce(&mut Self)) {
        let (kept, squeeze) = (self.s, self.squeeze);
        self.s = kept * k;
        self.squeeze = squeeze * k;
        draw(self);
        self.s = kept;
        self.squeeze = squeeze;
    }

    fn width(&mut self, value: &str, style: Style) -> i32 {
        text_width(self.text, value, style.size, style.bold, self.s)
    }

    /// Draws `value` with its baseline at `baseline`; `x` is its left edge, or its right edge
    /// when `right`. Returns the width.
    fn put(&mut self, value: &str, x: f32, baseline: f32, style: Style, right: bool) -> i32 {
        let em = px(style.size, self.s) as f32;
        let top = (baseline - ASCENT * em).round() as i32;
        let width = self.width(value, style);
        let left = x.round() as i32 - if right { width } else { 0 };
        draw_text(
            self.canvas,
            self.text,
            value,
            (left, top),
            (style.size, style.bold),
            self.s,
            style.color,
            1.0,
        );
        width
    }

    /// `value` cut to `max` pixels with an ellipsis. Inside a press shrink the text is fitted
    /// at full size (against `max` unshrunk), so a pressed label keeps what it had at rest.
    fn fit(&mut self, value: &str, style: Style, max: i32) -> String {
        let full = self.s / self.squeeze;
        let max = (max as f32 / self.squeeze).round() as i32;
        self.text
            .fit_end(value, px(style.size, full), style.bold, max as f32)
    }

    fn bar(&mut self, (left, right): (f32, f32), top: f32, fraction: Option<f32>, provider: bool) {
        // The Mac's frames sit on whole pixels (SwiftUI snaps them): the bar's box and its fill
        // are drawn on whole pixels too, so its edges are not soft.
        // Both edges snap, so a bar whose top lands on a half pixel is 7 rows, not 8 (the Mac's
        // Spark bars in the group box, 364 and 472 on its 2x shot).
        let bottom = (top + BAR_HEIGHT * self.s).round();
        let (left, right, top) = (left.round(), right.round(), top.round());
        let height = (bottom - top).max(1.0);
        let radius = height / 2.0;
        let width = right - left;
        self.canvas
            .fill_round_rect(left, top, width, height, [radius; 4], 0xFFFFFF, TRACK_ALPHA);
        if let Some(f) = fraction.filter(|f| *f > 0.0) {
            let f = f.clamp(0.0, 1.0);
            self.canvas.fill_round_rect(
                left,
                top,
                (width * f).round().max(height).min(width),
                height,
                [radius; 4],
                usage_color(f, provider),
                1.0,
            );
        }
    }

    /// One meter at `y` between `columns`: label and trailing text, the bar, the summary.
    fn meter(&mut self, columns: (f32, f32), y: f32, row: &Row) {
        self.meter_at(columns, y, 0.0, row);
    }

    /// `meter` with its bar `drop` DIPs lower (the Mac's bars in a group's box sit half a point lower).
    fn meter_at(&mut self, (left, right): (f32, f32), y: f32, drop: f32, row: &Row) {
        let Row::Meter {
            label,
            trailing,
            fraction,
            summary,
            ramp,
        } = row
        else {
            return;
        };
        let s = self.s;
        let base = |extra: f32| y + (extra + BODY_BASELINE) * s;
        let extra = bare_extra(trailing, DRAWN);
        self.put(label, left, base(extra / 2.0), body(TIP_INK), false);
        self.put(trailing, right, base(0.0), body(TIP_INK_SECONDARY), true);
        let mut next = DRAWN.line + extra;
        if let Some(share) = *fraction {
            self.bar(
                (left, right),
                y + (next + LABEL_TO_BAR + drop) * s,
                Some(share),
                *ramp,
            );
            next += LABEL_TO_BAR + BAR_HEIGHT;
        }
        next += BAR_TO_USED;
        let (summary, style) = self.squeezed(summary, body(TIP_INK), right - left);
        self.put(&summary, left, base(next), style, false);
    }

    /// `value` on one line in `room` pixels as the Mac's `minimumScaleFactor(0.85)` sets it:
    /// shrunk by up to 15% to fit, and cut with an ellipsis only past that.
    fn squeezed(&mut self, value: &str, style: Style, room: f32) -> (String, Style) {
        let width = self.width(value, style) as f32;
        let scale = if width > room && width > 0.0 {
            (room / width).max(0.85)
        } else {
            1.0
        };
        let style = Style {
            size: style.size * scale,
            ..style
        };
        (self.fit(value, style, room.max(0.0) as i32), style)
    }

    /// The paused-circle mark in front of a limit line.
    fn pause_mark(&mut self, (cx, cy): (f32, f32)) {
        let s = self.s;
        // Measured on the Mac's `pause.circle.fill`: a disc 6.15 pt across, half a point right
        // of and 1.25 pt below the line's centre, its two bars 0.78 pt wide and 2.4 pt tall,
        // 1.5 pt apart, cut out of it.
        let (cx, cy) = (cx + 0.5 * s, cy + 1.25 * s);
        let radius = PAUSE_DISC / 2.0 * s;
        self.canvas.fill_round_rect(
            cx - radius,
            cy - radius,
            2.0 * radius,
            2.0 * radius,
            [radius; 4],
            layout::BAND_CRITICAL,
            1.0,
        );
        for dx in [-0.75, 0.75] {
            self.canvas.fill_round_rect(
                cx + (dx - PAUSE_BAR / 2.0) * s,
                cy - PAUSE_BAR_HEIGHT / 2.0 * s,
                PAUSE_BAR * s,
                PAUSE_BAR_HEIGHT * s,
                [0.0; 4],
                0x000000,
                1.0,
            );
        }
    }

    /// The icon in front of a card's title.
    fn mark(&mut self, mark: Mark, (x, y): (f32, f32)) {
        let glyph = match mark {
            Mark::None => return,
            Mark::Claude => Glyph::Claude,
            Mark::Codex => Glyph::OpenAi,
            Mark::System => Glyph::Chip,
            Mark::Disks => Glyph::Drive,
            Mark::Send => Glyph::Plane,
            Mark::Tools => Glyph::Tools,
        };
        let centre = (x + MARK_SIZE * self.s / 2.0, y + MARK_SIZE * self.s / 2.0);
        glyphs::draw(self.canvas, glyph, centre, self.s, TIP_INK, 1.0);
    }

    /// A progress bar: white on a faint track. `None` is indeterminate: a stripe that travels
    /// while the card animates, a fixed share in a still.
    fn progress(
        &mut self,
        (left, right): (f32, f32),
        top: f32,
        fraction: Option<f32>,
        heavy: bool,
    ) {
        let (thick, track, fill_alpha, fill_ink) = if heavy {
            (HEAVY_HEIGHT, HEAVY_TRACK_ALPHA, HEAVY_FILL_ALPHA, CARD_INK)
        } else {
            (
                PROGRESS_HEIGHT,
                PROGRESS_TRACK_ALPHA,
                PROGRESS_FILL_ALPHA,
                TIP_INK,
            )
        };
        // Whole device pixels: 3.5pt at 1x is drawn as 4 crisp rows (7 at 2x, the Mac's 3.5pt
        // at 2x exactly). Antialiased edges on a fractional top smeared it over 4 to 6 rows.
        let height = (thick * self.s).round().max(1.0);
        let top = top.round();
        let radius = height / 2.0;
        let width = right - left;
        self.canvas
            .fill_round_rect(left, top, width, height, [radius; 4], 0xFFFFFF, track);
        let (from, share) = match (fraction, self.phase) {
            (Some(f), _) => (0.0, f.clamp(0.0, 1.0)),
            (None, None) => (0.0, INDETERMINATE),
            (None, Some(phase)) => {
                let start = (1.0 + STRIPE) * phase - STRIPE;
                let end = (start + STRIPE).min(1.0);
                (start.max(0.0), end - start.max(0.0))
            }
        };
        if share > 0.0 {
            // A share's fill is a whole number of pixels, as SwiftUI snaps the Mac's frame
            // (146.16 px is drawn 146).
            let wanted = if fraction.is_some() {
                (width * share).round()
            } else {
                width * share
            };
            let fill = wanted.max(height).min(width - width * from);
            self.canvas.fill_round_rect(
                left + width * from,
                top,
                fill,
                height,
                [radius; 4],
                fill_ink,
                fill_alpha,
            );
        }
    }

    /// A ring with a quarter-turn gap at the upper right (the Mac's 0.75 trim from 3 o'clock),
    /// turning while the card animates.
    fn spinner(&mut self, centre: (f32, f32), radius: f32) {
        self.spinner_ring(centre, radius, SPINNER_STROKE * self.s);
    }

    /// `spinner` with its own line width in pixels (the small ones are finer).
    fn spinner_ring(&mut self, (cx, cy): (f32, f32), radius: f32, stroke: f32) {
        let start = self.phase.unwrap_or(0.0) * 360.0;
        let points: Vec<(f32, f32)> = (0..=18)
            .map(|step| {
                let angle = (start + 270.0 * step as f32 / 18.0).to_radians();
                (cx + radius * angle.cos(), cy + radius * angle.sin())
            })
            .collect();
        // The Mac's `CardProgress` ring is drawn in `Palette.textPrimary`: pure white, whatever
        // the ink round it.
        self.canvas
            .stroke_polyline(&points, stroke, false, TIP_INK, 1.0);
    }

    /// A toast draws the Mac's smaller pills and cross (`pill` is its 22pt height).
    fn toast(&self) -> bool {
        self.pill == TOAST_PILL
    }

    /// A pill: a faint plate, the symbol and the label (dimmed when it does nothing). Under the
    /// pointer or held down (`lit`) the Mac's `CardButtonStyle` brightens the whole pill, so
    /// its ink reads pure white.
    fn pill(&mut self, (x, y, width): (f32, f32, f32), button: &Button, (live, lit): (bool, bool)) {
        let s = self.s;
        let height = self.pill * s;
        self.canvas
            .fill_round_rect(x, y, width, height, [height / 2.0; 4], 0xFFFFFF, PILL_ALPHA);
        let ink = if (live || button.busy) && lit {
            0xFFFFFF
        } else if live || button.busy {
            over_pill(CARD_INK_ALPHA)
        } else {
            over_pill(CARD_DETAIL_ALPHA)
        };
        let centre = y + height / 2.0;
        let (pad, symbol, inner, size) = if self.toast() {
            (
                TOAST_PILL_PAD,
                TOAST_PILL_SYMBOL,
                TOAST_PILL_INNER,
                TOAST_PILL_SIZE,
            )
        } else {
            (PILL_PAD, PILL_SYMBOL, PILL_INNER, BODY_SIZE)
        };
        let mut cursor = x + pad * s;
        if button.busy {
            // Working: a spinner where the symbol goes (the Mac's 9pt frame), not pressable.
            let centre = (cursor + symbol * s / 2.0, centre);
            self.spinner_ring(centre, PILL_SPINNER_RADIUS * s, PILL_SPINNER_STROKE * s);
            cursor += (symbol + inner) * s;
        } else if let Some(mark) = button.symbol {
            glyphs::symbol(
                self.canvas,
                mark,
                (cursor + symbol * s / 2.0, centre),
                symbol * s,
                ink,
                1.0,
            );
            cursor += (symbol + inner) * s;
        } else if let Some(glyph) = button.glyph {
            let centre = (cursor + symbol * s / 2.0, centre);
            glyphs::draw(
                self.canvas,
                glyph,
                centre,
                symbol * s / glyphs::GLYPH_SIZE,
                ink,
                1.0,
            );
            cursor += (symbol + inner) * s;
        }
        let style = Style {
            size,
            bold: true,
            color: ink,
        };
        let baseline = centre + (BODY_BASELINE - DRAWN.line / 2.0) * size / BODY_SIZE * s;
        // A label wider than its pill (the pill was cut to fit the card) ends in an ellipsis.
        let room = (x + width - pad * s - cursor).max(0.0).round() as i32;
        let label = self.fit(&button.label, style, room);
        self.put(&label, cursor, baseline, style, false);
    }

    /// A full-width button plate (the Mac's "Copy last" and "Paste clipboard" rows): a faint
    /// plate, the symbol and label, and the detail at the right. With no detail the symbol and
    /// label sit in the middle. The detail keeps up to half the plate; whatever does not fit
    /// is cut with an ellipsis. The whole plate reads at 0.4 when the button does nothing.
    fn button_plate(
        &mut self,
        (left, right, y): (f32, f32, f32),
        (symbol, label, detail): (Option<Symbol>, &str, &str),
        live: bool,
    ) {
        let s = self.s;
        let height = BUTTON_HEIGHT * s;
        let alpha = if live { 1.0 } else { DISABLED_ALPHA };
        self.canvas.fill_round_rect(
            left,
            y,
            right - left,
            height,
            [BUTTON_RADIUS * s; 4],
            0xFFFFFF,
            TRACK_ALPHA * alpha,
        );
        // The tooltip card's pure white and its quiet grey, as the Mac's `Palette.textPrimary`.
        let (ink, quiet) = (
            body(over_plate(TIP_INK, alpha)),
            body(over_plate(TIP_INK_SECONDARY, alpha)),
        );
        let pad = BUTTON_PAD * s;
        let icon = if symbol.is_some() {
            (BUTTON_SYMBOL + BUTTON_INNER) * s
        } else {
            0.0
        };
        let mut room = right - left - 2.0 * pad - icon;
        let mut detail_text = String::new();
        if !detail.is_empty() {
            detail_text = self.fit(detail, quiet, (room / 2.0) as i32);
            room -= self.width(&detail_text, quiet) as f32 + BUTTON_INNER * s;
        }
        // Half a pixel of tolerance: the plate's width came from a measured label through
        // DIPs and back, and a truncating cast must not cost a letter.
        let label = self.fit(label, ink, (room + 0.5).max(0.0) as i32);
        let label_width = self.width(&label, ink) as f32;
        let start = if detail.is_empty() {
            (left + right - icon - label_width) / 2.0
        } else {
            left + pad
        };
        if let Some(symbol) = symbol {
            let centre = (start + BUTTON_SYMBOL * s / 2.0, y + height / 2.0);
            glyphs::symbol(
                self.canvas,
                symbol,
                centre,
                BUTTON_SYMBOL * s,
                TIP_INK,
                alpha,
            );
        }
        let baseline = y + (BUTTON_HEIGHT - DRAWN.line - 1.0) / 2.0 * s + BODY_BASELINE * s;
        self.put(&label, start + icon, baseline, ink, false);
        if !detail_text.is_empty() {
            self.put(&detail_text, right - pad, baseline, quiet, true);
        }
    }

    /// A Tools pill: the plate at `height` DIPs, then its symbol and label together in the
    /// middle (the Mac's `sendBarButton`, a dimmed one at 0.4).
    fn pill_plate(
        &mut self,
        (left, right, y): (f32, f32, f32),
        height: f32,
        pill: &Pill,
        live: bool,
    ) {
        let s = self.s;
        let alpha = if live { 1.0 } else { DISABLED_ALPHA };
        self.canvas.fill_round_rect(
            left,
            y,
            right - left,
            height * s,
            [BUTTON_RADIUS * s; 4],
            0xFFFFFF,
            TRACK_ALPHA * alpha,
        );
        let ink = body(over_plate(TIP_INK, alpha));
        let icon = pill_symbol_width(pill.symbol) * s;
        let gap = TOOL_PILL_INNER * s;
        let room = right - left - 2.0 * BUTTON_PAD * s - icon - gap;
        let label = self.fit(&pill.label, ink, (room + 0.5).max(0.0) as i32);
        let label_width = self.width(&label, ink) as f32;
        let start = (left + right - icon - gap - label_width) / 2.0;
        let centre = (start + icon / 2.0, y + height * s / 2.0);
        glyphs::symbol(
            self.canvas,
            pill.symbol,
            centre,
            BUTTON_SYMBOL * s,
            TIP_INK,
            alpha,
        );
        let baseline = y + (height - DRAWN.line - 1.0) / 2.0 * s + BODY_BASELINE * s;
        self.put(&label, start + icon + gap, baseline, ink, false);
    }

    /// A round header or row button: a faint disc with a cross, a refresh arrow or a spinner;
    /// a dismiss is the bare cross.
    fn round(&mut self, (x, y, side): (f32, f32, f32), head: Head, live: bool) {
        let s = self.s;
        let centre = (x + side / 2.0, y + side / 2.0);
        // A toast's round buttons take its ink over their plate; the tooltip header's sync and
        // refresh draw the Mac's pure white (`Palette.textPrimary`) over theirs.
        let ink = match (head, live) {
            (Head::Sync | Head::Refresh, true) => TIP_INK,
            (Head::Sync | Head::Refresh, false) => TIP_INK_SECONDARY,
            (_, true) => over_pill(CARD_INK_ALPHA),
            (_, false) => over_pill(CARD_DETAIL_ALPHA),
        };
        if head == Head::Dismiss {
            glyphs::symbol(
                self.canvas,
                Symbol::Xmark,
                centre,
                X_SIZE * s,
                CARD_INK_SECONDARY,
                1.0,
            );
            return;
        }
        self.canvas
            .fill_round_rect(x, y, side, side, [side / 2.0; 4], 0xFFFFFF, PILL_ALPHA);
        match head {
            Head::Scanning => self.spinner(centre, (SPINNER - SPINNER_STROKE) / 2.0 * s),
            Head::Refresh => {
                glyphs::symbol(
                    self.canvas,
                    Symbol::Refresh,
                    centre,
                    PILL_SYMBOL * s,
                    ink,
                    1.0,
                );
            }
            Head::Sync => {
                glyphs::symbol(self.canvas, Symbol::Sync, centre, PILL_SYMBOL * s, ink, 1.0);
            }
            Head::Close | Head::Dismiss => {
                let cross = if self.toast() { TOAST_X } else { X_SIZE };
                glyphs::symbol(self.canvas, Symbol::Xmark, centre, cross * s, ink, 1.0);
            }
        }
    }

    /// A toast's inline glyph before the title: the app's icon where there is an app, a
    /// tinted mark for done (green) or a problem (amber), else the symbol in white.
    fn lead(&mut self, lead: &Lead, centre: (f32, f32)) {
        let s = self.s;
        let side = TOAST_GLYPH * s;
        match lead {
            Lead::Tile(tile) => glyphs::tile(self.canvas, *tile, centre, side),
            Lead::File(path) => match fileicon::of(path) {
                Some(icon) => {
                    self.canvas.draw_image(
                        &icon.pixels,
                        (icon.width, icon.height),
                        centre.0 - side / 2.0,
                        centre.1 - side / 2.0,
                        side,
                        side,
                    );
                }
                None => glyphs::tile(self.canvas, Tile::Package, centre, side),
            },
            Lead::Glyph { glyph, problem } => {
                let (ink, alpha) = if *problem {
                    (TOAST_ALERT_INK, 1.0)
                } else {
                    (CARD_INK, 0.88)
                };
                let scale = side / glyphs::GLYPH_SIZE;
                glyphs::draw(self.canvas, *glyph, centre, scale, ink, alpha);
            }
            Lead::Disc { symbol, problem } => {
                let tint = if *problem {
                    Some(0xD8762B)
                } else if *symbol == Symbol::Check {
                    Some(0x3F9D68)
                } else {
                    None
                };
                match tint {
                    // The Mac's disc: 22% of the tint, a heavy check or "!" inside in the tint.
                    Some(tint) => {
                        let r = side / 2.0;
                        self.canvas.fill_round_rect(
                            centre.0 - r,
                            centre.1 - r,
                            side,
                            side,
                            [r; 4],
                            tint,
                            0.22,
                        );
                        if *problem {
                            let (x, y) = (centre.0 - 0.7 * s, centre.1 - 3.3 * s);
                            self.canvas.fill_round_rect(
                                x,
                                y,
                                1.4 * s,
                                4.2 * s,
                                [0.7 * s; 4],
                                tint,
                                1.0,
                            );
                            let (x, y) = (centre.0 - 0.8 * s, centre.1 + 1.7 * s);
                            self.canvas.fill_round_rect(
                                x,
                                y,
                                1.6 * s,
                                1.6 * s,
                                [0.8 * s; 4],
                                tint,
                                1.0,
                            );
                        } else {
                            let tick = [
                                (centre.0 - 2.5 * s, centre.1 + 0.1 * s),
                                (centre.0 - 0.7 * s, centre.1 + 1.9 * s),
                                (centre.0 + 2.6 * s, centre.1 - 1.9 * s),
                            ];
                            self.canvas
                                .stroke_polyline(&tick, 1.5 * s, false, tint, 1.0);
                        }
                    }
                    None => glyphs::symbol(self.canvas, *symbol, centre, 11.0 * s, CARD_INK, 0.88),
                }
            }
        }
    }

    /// A device row: a faint plate, its symbol, the name in bold with the model beneath, and
    /// the send arrow at the right.
    fn device(
        &mut self,
        (left, right, y): (f32, f32, f32),
        device: (Symbol, &str, &str),
        live: bool,
    ) {
        let (symbol, alias, model) = device;
        let s = self.s;
        let height = DEVICE_HEIGHT * s;
        self.canvas.fill_round_rect(
            left,
            y,
            right - left,
            height,
            [DEVICE_RADIUS * s; 4],
            0xFFFFFF,
            DEVICE_ALPHA,
        );
        let centre = y + height / 2.0;
        let icon = (left + (DEVICE_PAD + DEVICE_ICON_FRAME / 2.0) * s, centre);
        glyphs::symbol(self.canvas, symbol, icon, DEVICE_ICON * s, CARD_INK, 1.0);
        let arrow = (right - (DEVICE_PAD + DEVICE_ICON / 2.0) * s, centre);
        let arrow_alpha = if live { 0.35 } else { 0.2 };
        glyphs::symbol(
            self.canvas,
            Symbol::Plane,
            arrow,
            DEVICE_ICON * s,
            CARD_INK,
            arrow_alpha,
        );
        let x = left + (DEVICE_PAD + DEVICE_ICON_FRAME + DEVICE_TEXT_GAP) * s;
        let room = (arrow.0 - DEVICE_ICON * s - x) / s;
        let name = fit_tail(self.text, alias, (TITLE_SIZE, true), room, s);
        let title = Style {
            size: TITLE_SIZE,
            bold: true,
            color: CARD_INK,
        };
        if model.is_empty() {
            let baseline = centre + (TITLE_BASELINE - DRAWN.title / 2.0) * s;
            self.put(&name, x, baseline, title, false);
            return;
        }
        let block = DRAWN.title + DEVICE_NAME_GAP + DRAWN.line;
        let top = centre - block / 2.0 * s;
        self.put(&name, x, top + TITLE_BASELINE * s, title, false);
        let model = fit_tail(self.text, model, (BODY_SIZE, false), room, s);
        let baseline = top + (DRAWN.title + DEVICE_NAME_GAP + BODY_BASELINE) * s;
        self.put(&model, x, baseline, body(CARD_INK_SECONDARY), false);
    }

    /// A toast's device row (the Mac's `SendDeviceRow`): a 10% plate 28pt tall, the symbol, the
    /// name in semibold with the model beneath in the quiet ink, and the send arrow at the end.
    fn toast_device(
        &mut self,
        (left, right, y): (f32, f32, f32),
        device: (Symbol, &str, &str),
        live: bool,
    ) {
        let (symbol, alias, model) = device;
        let s = self.s;
        let height = TOAST_ROW * s;
        self.canvas.fill_round_rect(
            left,
            y,
            right - left,
            height,
            [TOAST_ROW_RADIUS * s; 4],
            0xFFFFFF,
            DEVICE_ALPHA,
        );
        let centre = y + height / 2.0;
        let icon = (left + (TOAST_ROW_PAD + TOAST_GLYPH / 2.0) * s, centre);
        glyphs::symbol(self.canvas, symbol, icon, TOAST_ROW_ICON * s, 0xFFFFFF, 1.0);
        let arrow = (right - TOAST_ROW_ARROW_IN * s, centre);
        let arrow_alpha = if live { 0.35 } else { 0.2 };
        glyphs::symbol(
            self.canvas,
            Symbol::Plane,
            arrow,
            TOAST_ROW_ARROW * s,
            0xFFFFFF,
            arrow_alpha,
        );
        let x = left + TOAST_ROW_TEXT_X * s;
        let room = (arrow.0 - TOAST_ROW_ARROW * s - x) / s;
        let name_style = Style {
            size: TOAST_ROW_NAME,
            bold: true,
            color: 0xFFFFFF,
        };
        let name = fit_tail(self.text, alias, (TOAST_ROW_NAME, true), room, s);
        if model.is_empty() {
            // Alone, the name is centred on the row.
            let baseline = centre + TOAST_ROW_NAME * 0.35 * s;
            self.put(&name, x, baseline, name_style, false);
            return;
        }
        self.put(&name, x, y + TOAST_ROW_NAME_BASELINE * s, name_style, false);
        let model = fit_tail(self.text, model, (TOAST_ROW_MODEL, false), room, s);
        let quiet = Style {
            size: TOAST_ROW_MODEL,
            bold: false,
            // The Mac's `detailInk` over the row's plate: 181 on its shot, not the 173 it is
            // over the black card.
            color: over_white(DEVICE_ALPHA, CARD_DETAIL_ALPHA),
        };
        self.put(&model, x, y + TOAST_ROW_MODEL_BASELINE * s, quiet, false);
    }

    /// A file row of a banner: the thumbnail (or the shell's icon) in a square, then the name.
    /// One received item as a square tile at `(x, y)`, `side` pixels (#467): an image as its
    /// thumbnail, text as a short preview, any other file as its icon. No names.
    fn tile(&mut self, (x, y, side, high): (f32, f32, f32, f32), item: &crate::card::Item) {
        let s = self.s;
        // The Mac's box opacity: 40% offline, 60% when the clipboard is empty, else full.
        let fade = match item {
            crate::card::Item::Device { offline: true, .. } => OFFLINE_FADE,
            crate::card::Item::Device { enabled: false, .. } => 0.6,
            _ => 1.0,
        };
        self.canvas.fill_round_rect(
            x,
            y,
            side,
            high,
            [TILE_RADIUS * s; 4],
            0xFFFFFF,
            DEVICE_ALPHA * fade,
        );
        match item {
            crate::card::Item::Device {
                symbol,
                name,
                detail,
                default,
                ..
            } => {
                if *default {
                    // The Mac's `strokeBorder(white 0.25, 1)` on the default computer.
                    self.canvas.stroke_continuous_rect(
                        (x, y, side, high),
                        TILE_RADIUS * s,
                        s.max(1.0),
                        0xFFFFFF,
                        DEFAULT_OUTLINE_ALPHA,
                    );
                }
                let alpha = fade;
                // The card is black, so a fainter ink is the ink scaled down.
                let ink = |color: u32| {
                    let k = |shift: u32| ((((color >> shift) & 0xFF) as f32) * fade).round() as u32;
                    (k(16) << 16) | (k(8) << 8) | k(0)
                };
                glyphs::symbol(
                    self.canvas,
                    *symbol,
                    (x + side / 2.0, y + high * 0.30),
                    TOAST_ROW_ICON * s,
                    0xFFFFFF,
                    alpha,
                );
                let room = side / s - 2.0 * TILE_TEXT_PAD;
                let name = fit_tail(self.text, name, (8.5, true), room, s);
                let detail = fit_tail(self.text, detail, (7.0, false), room, s);
                let bold = Style {
                    size: 8.5,
                    bold: true,
                    color: ink(TIP_INK),
                };
                let light = Style {
                    size: 7.0,
                    bold: false,
                    color: ink(TIP_INK),
                };
                let name_w = dip_width(self.text, &name, 8.5, true, s) * s;
                let detail_w = dip_width(self.text, &detail, 7.0, false, s) * s;
                self.put(
                    &name,
                    x + (side - name_w) / 2.0,
                    y + high * 0.62 + 8.5 * 0.8 * s,
                    bold,
                    false,
                );
                self.put(
                    &detail,
                    x + (side - detail_w) / 2.0,
                    y + high * 0.62 + (11.0 + 7.0 * 0.8) * s,
                    light,
                    false,
                );
            }
            crate::card::Item::Text(value) => {
                let lines = wrap_sized(
                    self.text,
                    value.trim(),
                    side / s - 2.0 * TILE_TEXT_PAD,
                    TILE_TEXT_LINES,
                    (TILE_TEXT_SIZE, false),
                    s,
                );
                let style = Style {
                    size: TILE_TEXT_SIZE,
                    bold: false,
                    color: TIP_INK,
                };
                let room = side / s - 2.0 * TILE_TEXT_PAD;
                for (n, line) in lines.iter().enumerate() {
                    let line = fit_tail(self.text, line, (TILE_TEXT_SIZE, false), room, s);
                    let baseline =
                        y + (TILE_TEXT_PAD + TILE_TEXT_SIZE * 0.8 + n as f32 * TILE_TEXT_PITCH) * s;
                    self.put(&line, x + TILE_TEXT_PAD * s, baseline, style, false);
                }
            }
            crate::card::Item::File(path) => {
                let picture = if has_picture(path) {
                    crate::host::thumbnail(path, true)
                } else {
                    None
                };
                // An image fills its tile (the Mac's scaledToFill); other pictures sit inset.
                let fills = picture.is_some() && crate::host::is_image_file(path);
                let big = picture.is_some();
                let picture = picture.or_else(|| fileicon::of(path));
                match picture {
                    Some(icon) if icon.width > 0 && icon.height > 0 => {
                        let (box_w, box_h) = if fills {
                            (side, high)
                        } else if big {
                            (side - 6.0 * s, high - 6.0 * s)
                        } else {
                            (TILE_ICON * s, TILE_ICON * s)
                        };
                        let fit = (box_w / icon.width as f32).min(box_h / icon.height as f32);
                        let (w, h) = (icon.width as f32 * fit, icon.height as f32 * fit);
                        self.canvas.draw_image(
                            &icon.pixels,
                            (icon.width, icon.height),
                            x + (side - w) / 2.0,
                            y + (high - h) / 2.0,
                            w,
                            h,
                        );
                    }
                    _ => glyphs::symbol(
                        self.canvas,
                        Symbol::Folder,
                        (x + side / 2.0, y + high / 2.0),
                        TOAST_ROW_ICON * s,
                        0xFFFFFF,
                        1.0,
                    ),
                }
            }
        }
    }

    fn toast_file(&mut self, (left, right, y): (f32, f32, f32), (name, path): (&str, &str)) {
        let s = self.s;
        let height = TOAST_ROW * s;
        self.canvas.fill_round_rect(
            left,
            y,
            right - left,
            height,
            [TOAST_ROW_RADIUS * s; 4],
            0xFFFFFF,
            DEVICE_ALPHA,
        );
        let centre = y + height / 2.0;
        let side = FILE_THUMB * s;
        let icon_left = left + TOAST_ROW_PAD * s / 1.5;
        let picture = crate::host::thumbnail(path, false).or_else(|| fileicon::of(path));
        match picture {
            Some(icon) if icon.width > 0 && icon.height > 0 => {
                // Fitted inside the square, keeping the picture's proportions.
                let fit = (side / icon.width as f32).min(side / icon.height as f32);
                let (w, h) = (icon.width as f32 * fit, icon.height as f32 * fit);
                self.canvas.draw_image(
                    &icon.pixels,
                    (icon.width, icon.height),
                    icon_left + (side - w) / 2.0,
                    centre - h / 2.0,
                    w,
                    h,
                );
            }
            _ => glyphs::symbol(
                self.canvas,
                Symbol::Folder,
                (icon_left + side / 2.0, centre),
                TOAST_ROW_ICON * s,
                0xFFFFFF,
                1.0,
            ),
        }
        let x = icon_left + side + 6.0 * s;
        let room = (right - TOAST_ROW_PAD * s - x) / s;
        let style = Style {
            size: TOAST_ROW_NAME,
            bold: true,
            color: 0xFFFFFF,
        };
        let name = fit_tail(self.text, name, (TOAST_ROW_NAME, true), room, s);
        let baseline = centre + TOAST_ROW_NAME * 0.35 * s;
        self.put(&name, x, baseline, style, false);
    }

    /// A plain card: the header (icon, title, plan line, note, round buttons), then rows.
    fn flow(&mut self, content: &CardContent, laid: &Plan, live: &Live, (ox, oy): (f32, f32)) {
        let s = self.s;
        let pad = CARD_PAD * s;
        let columns = (ox + pad, ox + (laid.width - CARD_PAD) * s);
        let (left, right) = columns;
        let top = oy;
        // Header: the icon centred on the title block, the note on the title's line.
        let text_height = DRAWN.title
            + if content.subtitle.is_some() {
                DRAWN.line
            } else {
                0.0
            };
        let header = header_height(content, DRAWN);
        let text_top = top + (CARD_PAD + (header - text_height) / 2.0) * s;
        let title_x = if content.mark == Mark::None {
            left
        } else {
            self.mark(
                content.mark,
                (left, top + (CARD_PAD + (header - MARK_SIZE) / 2.0) * s),
            );
            left + (MARK_SIZE + MARK_GAP) * s
        };
        // The text stops short of the header's buttons.
        let limit = laid
            .head
            .iter()
            .map(|rect| rect.0)
            .fold(laid.width - CARD_PAD, f32::min);
        // The note (how old the reading is) sits on the title's line, right-aligned short of
        // the header's buttons. The title names the model and keeps its room (the Mac's
        // `layoutPriority(1)`); a long note yields first, cut with an ellipsis.
        let note_right = if laid.head.is_empty() {
            right
        } else {
            ox + (limit - VALUE_GAP) * s
        };
        let text_room = (ox + (limit - VALUE_GAP) * s - title_x) / s;
        let title_baseline = text_top + TITLE_BASELINE * s;
        let title_style = Style {
            size: TITLE_SIZE,
            bold: true,
            color: TIP_INK,
        };
        let title = if laid.head.is_empty() && content.accessory.is_none() {
            content.title.clone()
        } else {
            fit_tail(self.text, &content.title, (TITLE_SIZE, true), text_room, s)
        };
        let title_width = self.put(&title, title_x, title_baseline, title_style, false);
        if let Some(note) = &content.accessory {
            let room = text_room - title_width as f32 / s - VALUE_GAP;
            let note = fit_tail(self.text, note, (BODY_SIZE, false), room, s);
            self.put(
                &note,
                note_right,
                title_baseline,
                body(TIP_INK_SECONDARY),
                true,
            );
        }
        if let Some(subtitle) = &content.subtitle {
            let ink = if content.problem {
                layout::BAND_WATCH
            } else {
                TIP_INK_SECONDARY
            };
            let subtitle = if laid.head.is_empty() {
                subtitle.clone()
            } else {
                fit_middle(self.text, subtitle, (BODY_SIZE, false), text_room, s)
            };
            self.put(
                &subtitle,
                title_x,
                text_top + (DRAWN.title + BODY_BASELINE) * s,
                body(ink),
                false,
            );
        }
        for (index, (head, rect)) in content.head.iter().zip(&laid.head).enumerate() {
            let on = live.head.get(index) == Some(&true);
            let k = self.press(Hit::Head(index));
            let place = (ox + rect.0 * s, oy + rect.1 * s, rect.2 * s, rect.3 * s);
            let (x, y, side, _) = about(place, k);
            self.scaled(k, |pen| pen.round((x, y, side), *head, on));
        }
        for (index, (row, placed)) in content.rows.iter().zip(&laid.rows).enumerate() {
            let on = live.row(index, 0);
            let y = top + placed.top * s;
            // Baseline of the line `extra` DIPs down the row.
            let base = |extra: f32| y + (extra + BODY_BASELINE) * s;
            match row {
                Row::Button {
                    symbol,
                    label,
                    detail,
                } => {
                    let k = self.press(Hit::Row(index, 0));
                    let plate = (left, y, right - left, BUTTON_HEIGHT * s);
                    let (x, top, width, _) = about(plate, k);
                    self.scaled(k, |pen| {
                        pen.button_plate(
                            (x, x + width, top),
                            (*symbol, label.as_str(), detail.as_str()),
                            on,
                        );
                    });
                }
                Row::Pills(pills) => {
                    for (n, (pill, (x, width))) in pills.iter().zip(&placed.spans).enumerate() {
                        let k = self.press(Hit::Row(index, n));
                        let height = pill_height(pill.symbol);
                        let plate = (ox + x * s, y, width * s, height * s);
                        let (x, top, width, _) = about(plate, k);
                        let on = live.row(index, n);
                        self.scaled(k, |pen| {
                            pen.pill_plate((x, x + width, top), height, pill, on);
                        });
                    }
                }
                Row::Pair { label, value } => {
                    // Both sides are cut to fit: the value keeps what the label leaves (and
                    // at least half the line when it needs it).
                    let (primary, quiet) = (body(TIP_INK), body(TIP_INK_SECONDARY));
                    let total = (right - left) as i32;
                    let gap = (VALUE_GAP * s) as i32;
                    let label_width = self.width(label, primary);
                    let value_room = (total - label_width - gap).max(total / 2);
                    let value = self.fit(value, quiet, value_room);
                    let value_width = self.width(&value, quiet);
                    // No value: the label keeps the whole line.
                    let apart = if value.is_empty() { 0 } else { gap };
                    let label = self.fit(label, primary, total - value_width - apart);
                    self.put(&label, left, base(0.0), primary, false);
                    self.put(&value, right, base(0.0), quiet, true);
                }
                Row::Meter { .. } => self.meter(columns, y, row),
                Row::Group { title, rows } => {
                    let bold = Style {
                        size: BODY_SIZE,
                        bold: true,
                        color: TIP_INK,
                    };
                    self.put(title, left + GROUP_TITLE_INSET * s, base(0.0), bold, false);
                    let box_top = y + (DRAWN.line + GROUP_TITLE_GAP) * s;
                    let (tops, height) = group_tops(rows, DRAWN);
                    // The outline's line centred between pixel rows, so it reads as the Mac's two
                    // dim rows (38 each) and not one bright one (61).
                    let (edge_left, edge_top) = (left.round(), box_top.round());
                    let outline = round_rect_outline(
                        edge_left,
                        edge_top,
                        right.round() - edge_left,
                        (box_top + height * s).round() - edge_top,
                        GROUP_RADIUS * s,
                    );
                    let stroke = GROUP_STROKE * s;
                    self.canvas
                        .stroke_polyline(&outline, stroke, true, 0xFFFFFF, GROUP_ALPHA);
                    let inner = (left + GROUP_PAD * s, right - GROUP_PAD * s);
                    for (meter, top) in rows.iter().zip(tops) {
                        self.meter_at(inner, box_top + top * s, 0.5, meter);
                    }
                }
                Row::Note(_) | Row::Text(_) => {
                    let ink = if matches!(row, Row::Note(_)) {
                        TIP_INK_SECONDARY
                    } else {
                        TIP_INK
                    };
                    for (n, line) in placed.lines.iter().enumerate() {
                        self.put(line, left, base(n as f32 * WRAP_PITCH), body(ink), false);
                    }
                }
                Row::Tinted { tone, .. } => {
                    for (n, line) in placed.lines.iter().enumerate() {
                        let ink = body(tone_color(*tone));
                        self.put(line, left, base(n as f32 * WRAP_PITCH), ink, false);
                    }
                }
                Row::Alert(_) => {
                    self.pause_mark((left + STATUS_DOT / 2.0 * s, y + DRAWN.line / 2.0 * s));
                    let indent = left + (STATUS_DOT + STATUS_GAP) * s;
                    for (n, line) in placed.lines.iter().enumerate() {
                        let ink = body(layout::BAND_CRITICAL);
                        self.put(line, indent, base(n as f32 * WRAP_PITCH), ink, false);
                    }
                }
                Row::Buttons { buttons, .. } => {
                    self.button_row(buttons, placed, live, (index, ox, y));
                }
                Row::Progress(fraction) => self.progress(columns, y, *fraction, false),
                Row::Tiles(items) => {
                    for (n, item) in items.iter().enumerate() {
                        let (tx, ty, side, high) = tile_rect(n, laid.width, items);
                        self.tile((ox + tx * s, y + ty * s, side * s, high * s), item);
                    }
                }
                Row::File { name, path } => {
                    let k = self.press(Hit::Row(index, 0));
                    let (x, top, width, _) = about((left, y, right - left, TOAST_ROW * s), k);
                    self.scaled(k, |pen| {
                        pen.toast_file((x, x + width, top), (name.as_str(), path.as_str()));
                    });
                }
                Row::Device {
                    symbol,
                    alias,
                    model,
                } => {
                    let k = self.press(Hit::Row(index, 0));
                    let (x, top, width, _) = about((left, y, right - left, DEVICE_HEIGHT * s), k);
                    self.scaled(k, |pen| {
                        pen.device(
                            (x, x + width, top),
                            (*symbol, alias.as_str(), model.as_str()),
                            on,
                        );
                    });
                }
                Row::Waiting(label) => {
                    let ink = body(TIP_INK_SECONDARY);
                    let label_width = self.width(label, ink) as f32;
                    let total = (SPINNER + SPINNER_GAP) * s + label_width;
                    let x0 = (left + right - total) / 2.0;
                    let centre = y + DEVICE_HEIGHT * s / 2.0;
                    let radius = (SPINNER - SPINNER_STROKE) / 2.0 * s;
                    self.spinner((x0 + SPINNER * s / 2.0, centre), radius);
                    let baseline = centre + (BODY_BASELINE - DRAWN.line / 2.0) * s;
                    self.put(
                        label,
                        x0 + (SPINNER + SPINNER_GAP) * s,
                        baseline,
                        ink,
                        false,
                    );
                }
            }
        }
    }

    /// The pills of a button row then its round close, at the spans the plan measured.
    fn button_row(
        &mut self,
        buttons: &[Button],
        placed: &Placed,
        live: &Live,
        (index, ox, y): (usize, f32, f32),
    ) {
        let s = self.s;
        for (n, (x, width)) in placed.spans.iter().enumerate() {
            let on = live.row(index, n);
            let k = self.press(Hit::Row(index, n));
            let lit = self.lit.contains(&Hit::Row(index, n));
            let (x, top, width, _) = about((ox + x * s, y, width * s, self.pill * s), k);
            self.scaled(k, |pen| match buttons.get(n) {
                Some(button) => pen.pill((x, top, width), button, (on, lit)),
                None => pen.round((x, top, width), Head::Close, on),
            });
        }
    }

    /// A toast (liquid-motion pass §4): the inline glyph and the title, the detail and notes at
    /// full width, a bar, a box to tick, and the actions row with the × last.
    fn banner(&mut self, content: &CardContent, laid: &Plan, live: &Live, (ox, oy): (f32, f32)) {
        let s = self.s;
        let pad = TOAST_PAD;
        if let Some(lead) = &content.lead {
            let centre = (
                ox + (pad + TOAST_GLYPH_AT.0) * s,
                oy + (pad + TOAST_GLYPH_AT.1) * s,
            );
            if content.spinner {
                // The Mac's `.working` mark: the small spinner in place of the icon.
                self.spinner_ring(centre, TOAST_SPINNER_RADIUS * s, TOAST_SPINNER_STROKE * s);
            } else {
                self.lead(lead, centre);
            }
        }
        let title_style = Style {
            size: TOAST_TITLE_SIZE,
            bold: true,
            color: TOAST_TITLE_INK,
        };
        for (n, line) in laid.title.iter().enumerate() {
            let baseline = oy + (pad + TOAST_TITLE_BASELINE + n as f32 * TOAST_TITLE_LINE) * s;
            self.put(line, ox + TOAST_TEXT_X * s, baseline, title_style, false);
        }
        let column = ox + pad * s;
        let right = ox + (laid.width - pad) * s;
        let detail_ink = if content.problem {
            TOAST_WARNING_INK
        } else {
            TOAST_DETAIL_INK
        };
        let detail_top = toast_detail_top(laid.title.len(), toast_close(content));
        for (n, line) in laid.detail.iter().enumerate() {
            let line_top = detail_top + n as f32 * TOAST_DETAIL_LINE;
            let baseline = oy + (line_top + TOAST_DETAIL_BASELINE) * s;
            self.put(line, column, baseline, body(detail_ink), false);
        }
        for (index, (row, placed)) in content.rows.iter().zip(&laid.rows).enumerate() {
            let y = oy + placed.top * s;
            match row {
                Row::Note(_) | Row::Text(_) | Row::Tinted { .. } => {
                    let ink = match row {
                        Row::Tinted {
                            tone: Tone::Warning,
                            ..
                        } => TOAST_WARNING_INK,
                        Row::Tinted {
                            tone: Tone::Critical,
                            ..
                        } => TOAST_ALERT_INK,
                        Row::Tinted { tone, .. } => tone_color(*tone),
                        Row::Text(_) => TOAST_TITLE_INK,
                        _ => TOAST_DETAIL_INK,
                    };
                    for (n, line) in placed.lines.iter().enumerate() {
                        let baseline =
                            y + (TOAST_DETAIL_BASELINE + n as f32 * TOAST_DETAIL_LINE) * s;
                        self.put(line, column, baseline, body(ink), false);
                    }
                }
                // Only a share draws a bar, as on the Mac.
                Row::Progress(Some(fraction)) => {
                    self.progress((column, right), y + 3.0 * s, Some(*fraction), false);
                }
                Row::Buttons { buttons, close } => {
                    self.button_row(buttons, placed, live, (index, ox, y));
                    // The life ring: what is left of a toast that goes by itself, round its ×.
                    if let (true, Some(life), Some(&(x, width))) =
                        (*close, content.life, placed.spans.last())
                    {
                        let centre = (ox + (x + width / 2.0) * s, y + placed.height * s / 2.0);
                        self.canvas.stroke_arc(
                            centre.0,
                            centre.1,
                            LIFE_RADIUS * s,
                            LIFE_STROKE * s,
                            life,
                            0xFFFFFF,
                            LIFE_ALPHA,
                        );
                    }
                }
                Row::Button { symbol, label, .. } => {
                    // A box to tick (ticked when the row carries a symbol), then its label: the
                    // Mac's 8pt box (1pt line inside its edge, 2pt corners) and 8.5pt caption,
                    // centred on the row's 13pt slot.
                    let side = CHECK_BOX * s;
                    let line = CHECK_STROKE * s;
                    let (left, top) = (column, y + (TOAST_CHECK - CHECK_BOX) / 2.0 * s);
                    let inset = line / 2.0;
                    let outline = round_rect_outline(
                        left + inset,
                        top + inset,
                        side - line,
                        side - line,
                        CHECK_RADIUS * s - inset,
                    );
                    self.canvas
                        .stroke_polyline(&outline, line, true, TOAST_DETAIL_INK, 1.0);
                    if symbol.is_some() {
                        let tick = [
                            (left + side * 0.22, top + side * 0.52),
                            (left + side * 0.42, top + side * 0.72),
                            (left + side * 0.78, top + side * 0.28),
                        ];
                        self.canvas
                            .stroke_polyline(&tick, line, false, CARD_INK, 1.0);
                    }
                    let caption = Style {
                        size: CHECK_LABEL_SIZE,
                        bold: false,
                        color: TOAST_DETAIL_INK,
                    };
                    let middle = y + TOAST_CHECK / 2.0 * s;
                    let baseline = middle
                        + (BODY_BASELINE - DRAWN.line / 2.0) * CHECK_LABEL_SIZE / BODY_SIZE * s;
                    self.put(
                        label,
                        column + (CHECK_BOX + 4.0) * s,
                        baseline,
                        caption,
                        false,
                    );
                }
                Row::Device {
                    symbol,
                    alias,
                    model,
                } => {
                    let k = self.press(Hit::Row(index, 0));
                    let (x, top, width, _) = about((column, y, right - column, TOAST_ROW * s), k);
                    let on = live.row(index, 0);
                    self.scaled(k, |pen| {
                        pen.toast_device(
                            (x, x + width, top),
                            (*symbol, alias.as_str(), model.as_str()),
                            on,
                        );
                    });
                }
                Row::File { name, path } => {
                    let k = self.press(Hit::Row(index, 0));
                    let (x, top, width, _) = about((column, y, right - column, TOAST_ROW * s), k);
                    self.scaled(k, |pen| {
                        pen.toast_file((x, x + width, top), (name.as_str(), path.as_str()));
                    });
                }
                Row::Waiting(label) => {
                    // The Mac's empty list: a 12pt spinner and the line, centred at the top.
                    let ink = body(TOAST_DETAIL_INK);
                    let label_width = self.width(label, ink) as f32;
                    let total = (TOAST_ROW_SPINNER + TOAST_ROW_SPINNER_GAP) * s + label_width;
                    let x0 = (column + right - total) / 2.0;
                    let centre = (
                        x0 + TOAST_ROW_SPINNER / 2.0 * s,
                        y + TOAST_ROW_SPINNER / 2.0 * s,
                    );
                    self.spinner_ring(centre, TOAST_SPINNER_RADIUS * s, TOAST_SPINNER_STROKE * s);
                    let at = x0 + (TOAST_ROW_SPINNER + TOAST_ROW_SPINNER_GAP) * s;
                    self.put(label, at, y + TOAST_DETAIL_BASELINE * s, ink, false);
                }
                _ => {}
            }
        }
    }
}

/// The colour a tone reads in.
fn tone_color(tone: Tone) -> u32 {
    match tone {
        Tone::Warning => layout::BAND_WATCH,
        Tone::Good => tint(layout::BAND_AMPLE),
        Tone::Critical => layout::BAND_CRITICAL,
    }
}

/// The card: a plain one (header, then rows) or a banner (large icon beside the title block,
/// the buttons below), in a rounded shape with a tail toward the notch. `live` marks which
/// buttons have an action; a dead button reads dimmed. `phase` (0..1, looping) moves a
/// spinner or an indeterminate bar; `None` draws them still.
#[cfg_attr(not(feature = "qa-views"), allow(dead_code))] // used by the qa-views screenshots
pub fn render_card(
    content: &CardContent,
    live: &Live,
    dpi: u32,
    text: &mut TextPainter,
    phase: Option<f32>,
) -> Canvas {
    render_card_hover(content, live, dpi, text, phase, None)
}

/// The handle's caption (liquid-motion pass §1): a black capsule 16pt tall, 8pt either side
/// of an 8.5pt semibold white label (8.7 em in Segoe UI, as the toast's title is scaled).
const CAPTION_SIZE: f32 = 8.7;
const CAPTION_HEIGHT: f32 = 16.0;
const CAPTION_PAD: f32 = 8.0;

/// The handle's caption, `label` in its capsule.
pub fn render_caption(label: &str, dpi: u32, text: &mut TextPainter) -> Canvas {
    let s = layout::scale(dpi);
    let pad = px(CAPTION_PAD, s);
    let width = text_width(text, label, CAPTION_SIZE, true, s) + 2 * pad;
    let height = px(CAPTION_HEIGHT, s);
    let mut canvas = Canvas::new(width.max(1) as usize, height.max(1) as usize);
    let round = height as f32 / 2.0;
    canvas.fill_round_rect(
        0.0,
        0.0,
        width as f32,
        height as f32,
        [round; 4],
        0x000000,
        1.0,
    );
    let top = (height - px(CAPTION_SIZE, s)) / 2;
    draw_text(
        &mut canvas,
        text,
        label,
        (pad, top),
        (CAPTION_SIZE, true),
        s,
        0xFFFFFF,
        1.0,
    );
    canvas
}

/// How far the card on show has slid out of the notch, and how far it has opened (section 4,
/// the Mac's option A): each 0 to 1, a little past 1 on the spring's overshoot. Set by the card
/// window before each draw; stills draw it out and open.
static SLIDE: AtomicU32 = AtomicU32::new(0x3F80_0000);
static OPEN: AtomicU32 = AtomicU32::new(0x3F80_0000);

pub fn set_swell(slide: f32, open: f32) {
    SLIDE.store(slide.to_bits(), Ordering::Relaxed);
    OPEN.store(open.to_bits(), Ordering::Relaxed);
}

/// The card part way out of the notch at `slide` and `open`: `render_emerging` for one card
/// at rest in its own window.
fn render_sliding(
    content: &CardContent,
    live: &Live,
    dpi: u32,
    text: &mut TextPainter,
    phase: Option<f32>,
    swell: (f32, f32),
) -> Canvas {
    let pose = card_body(content, dpi, text, (0, 0));
    render_emerging(&[(content, live, 1.0)], &pose, swell, dpi, text, phase)
}

/// A card part way out of the notch at `slide` and `open`, as the Mac's `NoticeEmergence` has
/// it: one solid black body, the notch's own colour, a strip that slides out from under the
/// notch and then opens along it. `pose` is the body (a card at rest, or a gliding one) and
/// `layers` the contents drawn over it, each with its opacity (the last is the card on show).
/// - Across, out from the notch's face: always the card's width, its near side going from
///   wholly under the notch (`-across - 2`) to the face 17pt out with `slide`.
/// - Along: `lerp(34, length, open)`, the strip on the neck's anchor, its centre moving to the
///   card's own with `open`.
/// - Corner the card's, at most `(length - 30) / 2 - 0.5` or a fifth of the length, so a
///   strip is round enough with a straight side left for the neck.
/// - The neck, anchored at `tail` (no nearer a corner than the card's straight side allows),
///   joins it to the notch once its near side is more than 1pt out of the mouth.
/// - While it opens the body is blurred (3.5pt, gone by 0.95) and cut at half, one liquid.
/// - The contents sit against the near side in a frame the size of the card on show (an
///   outgoing block centred in it), clipped to the body, fading in over 0.55 to 0.95.
pub fn render_emerging(
    layers: &[(&CardContent, &Live, f32)],
    pose: &Body,
    (slide, open): (f32, f32),
    dpi: u32,
    text: &mut TextPainter,
    phase: Option<f32>,
) -> Canvas {
    let s = layout::scale(dpi);
    let (wx, wy, width, height) = pose.window;
    let empty = || Canvas::new(width.max(1) as usize, height.max(1) as usize);
    let Some(&(lead, ..)) = layers.last() else {
        return empty();
    };
    let Some(tail) = lead.tail else {
        return empty();
    };
    let vertical = tail.edge.is_vertical();
    let Some((mut canvas, start, opened)) =
        emerging_shape(tail, pose, (slide, open), card_radius(lead) * s, s)
    else {
        return empty();
    };
    let origin_along = (if vertical { wy } else { wx }) as f32;
    let shown = smooth((opened - 0.55) / 0.4);
    if shown <= 0.0 {
        return canvas;
    }
    // Every contents block is laid out inside a frame the size of the card on show.
    let frame = natural_size(lead, text, s, vertical);
    for &(content, live, level) in layers {
        let level = level * shown;
        if level <= 0.0 {
            continue;
        }
        let own = natural_size(content, text, s, vertical);
        let place = contents_place(tail.edge, start, pose, origin_along, frame.0, own);
        lay_contents(&mut canvas, (content, live), text, (s, phase), place, level);
    }
    canvas
}

/// The black body and its neck of an emerging card, at `slide` and `open`, blurred while it
/// opens: the window's canvas, where the body's low side across starts, and how far open it is
/// (0 to 1). `radius` is the card's corner in device pixels. `None` while it is still wholly
/// inside the notch. The clipboard strip is drawn through the same shape.
fn emerging_shape(
    tail: crate::card::Tail,
    pose: &Body,
    (slide, open): (f32, f32),
    radius: f32,
    s: f32,
) -> Option<(Canvas, f32, f32)> {
    let (wx, wy, width, height) = pose.window;
    let mut canvas = Canvas::new(width.max(1) as usize, height.max(1) as usize);
    let vertical = tail.edge.is_vertical();
    let slide = slide.max(0.0);
    if slide <= 0.004 {
        return None;
    }
    let open = open.max(0.0);
    let opened = open.min(1.0);
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let face = layout::NECK_REACH * s;
    let inside = layout::NECK_INSIDE_TOAST * s;
    let neck = NECK_WIDTH * s;
    // In, the strip is wholly inside the notch; out, its near side is at the face.
    let near = lerp(-pose.across - inside, face, slide);
    let long = lerp(STRIP_LENGTH * s, pose.length, open);
    // The neck's anchor on the ring's line, kept off the card's corners; the strip sits on it
    // and opens to the card's middle.
    let most = (pose.length / 2.0 - radius - neck / 2.0).max(0.0);
    let anchor = pose.centre + (pose.tail - pose.centre).clamp(-most, most);
    let origin_along = (if vertical { wy } else { wx }) as f32;
    let middle = anchor + (pose.centre - anchor) * opened - origin_along;
    let corner = radius.min(((long - neck) / 2.0 - 0.5 * s).max(long * 0.2));
    // The body's low side across, in the window: the notch's face sits 2pt in from the
    // window's edge on the notch's side.
    let start = body_start(tail.edge, (width, height), pose.across, near, s);
    let body = if vertical {
        (start, middle - long / 2.0, pose.across, long)
    } else {
        (middle - long / 2.0, start, long, pose.across)
    };
    canvas.fill_round_rect(body.0, body.1, body.2, body.3, [corner; 4], 0x000000, 1.0);
    if near > -s {
        let joint = crate::card::Tail {
            edge: tail.edge,
            offset: (anchor - origin_along - middle).round() as i32,
        };
        draw_neck(
            &mut canvas,
            joint,
            body,
            corner,
            s,
            (near + inside, layout::NECK_INSIDE_TOAST),
        );
    }
    let sigma = 3.5 * s * (1.0 - smooth((opened - 0.6) / 0.35));
    if sigma >= 1.0 {
        let mut field = crate::blur::coverage(&canvas);
        crate::blur::blur(&mut field, canvas.width, canvas.height, sigma);
        let edge = (sigma * 1.2).max(1.0);
        for (pixel, value) in canvas.pixels.iter_mut().zip(field) {
            let alpha = ((value - 0.5) * edge + 0.5).clamp(0.0, 1.0);
            *pixel = ((alpha * 255.0).round() as u32) << 24;
        }
    }
    Some((canvas, start, opened))
}

/// The body of a picture hung from the notch `notch` (`left, top, right, bottom`) on `edge`:
/// its window (the picture's, the neck's room between it and the notch, the far margin the
/// spring overshoots into, and `slack` device pixels either end along the notch), the picture's
/// centre and length along the notch (absolute device pixels), its depth `across` and where the
/// neck is anchored along the notch. The window is placed as a card's window is.
pub fn emerging_body(
    edge: Edge,
    notch: (i32, i32, i32, i32),
    (centre, length, across): (f32, f32, f32),
    tail: f32,
    slack: i32,
    dpi: u32,
) -> Body {
    let s = layout::scale(dpi);
    let inside = px(layout::NECK_INSIDE_TOAST, s);
    let deep = neck_length(s, true) + across.round() as i32 + px(SLIDE_MARGIN, s);
    let long = length.round() as i32 + 2 * slack;
    let start = (centre - length / 2.0).round() as i32 - slack;
    let (left, top, right, bottom) = notch;
    let window = match edge {
        Edge::Top => (start, bottom - inside, long, deep),
        Edge::Bottom => (start, top + inside - deep, long, deep),
        Edge::Left => (right - inside, start, deep, long),
        Edge::Right => (left + inside - deep, start, deep, long),
    };
    Body {
        window,
        centre,
        length,
        across,
        tail,
    }
}

/// A picture of its own (the clipboard strip's contents, `across` by `length` device pixels)
/// put through the card's emergence: the black body and neck at `slide` and `open`, the picture
/// against the body's near side, clipped to the body and fading in with it. `radius` is the
/// body's corner in device pixels.
pub fn render_emerging_picture(
    picture: &Canvas,
    tail: crate::card::Tail,
    pose: &Body,
    swell: (f32, f32),
    radius: f32,
    dpi: u32,
) -> Canvas {
    let s = layout::scale(dpi);
    let (wx, wy, width, height) = pose.window;
    let Some((mut canvas, start, opened)) = emerging_shape(tail, pose, swell, radius, s) else {
        return Canvas::new(width.max(1) as usize, height.max(1) as usize);
    };
    let shown = smooth((opened - 0.55) / 0.4);
    if shown <= 0.0 {
        return canvas;
    }
    let origin_along = (if tail.edge.is_vertical() { wy } else { wx }) as f32;
    let own = if tail.edge.is_vertical() {
        (picture.width as f32, picture.height as f32)
    } else {
        (picture.height as f32, picture.width as f32)
    };
    let place = contents_place(tail.edge, start, pose, origin_along, pose.across, own);
    composite_clipped(&mut canvas, picture, place, shown);
    canvas
}

/// Where the strip's picture sits in its window once it is out and open: the origin
/// `render_emerging_picture` draws it at, for the pointer.
pub fn picture_origin(
    tail: crate::card::Tail,
    pose: &Body,
    picture: (usize, usize),
    dpi: u32,
) -> (i32, i32) {
    let s = layout::scale(dpi);
    let (wx, wy, width, height) = pose.window;
    let near = layout::NECK_REACH * s;
    let start = body_start(tail.edge, (width, height), pose.across, near, s);
    let origin_along = (if tail.edge.is_vertical() { wy } else { wx }) as f32;
    let own = if tail.edge.is_vertical() {
        (picture.0 as f32, picture.1 as f32)
    } else {
        (picture.1 as f32, picture.0 as f32)
    };
    let place = contents_place(tail.edge, start, pose, origin_along, pose.across, own);
    (place.0.round() as i32, place.1.round() as i32)
}

/// `ink` (premultiplied, with its own alpha) laid over `canvas` at `origin`, clipped to what
/// `canvas` already covers, at opacity `shown`.
fn composite_clipped(canvas: &mut Canvas, ink: &Canvas, origin: (f32, f32), shown: f32) {
    let (ox, oy) = (origin.0.round() as i32, origin.1.round() as i32);
    for y in 0..ink.height {
        let ty = y as i32 + oy;
        if ty < 0 || ty as usize >= canvas.height {
            continue;
        }
        for x in 0..ink.width {
            let tx = x as i32 + ox;
            if tx < 0 || tx as usize >= canvas.width {
                continue;
            }
            let over = ink.pixels[y * ink.width + x];
            let under = &mut canvas.pixels[ty as usize * canvas.width + tx as usize];
            let keep = shown * (*under >> 24) as f32 / 255.0;
            let a = (over >> 24) as f32 / 255.0 * keep;
            if a <= 0.0 {
                continue;
            }
            let below = *under;
            let mix = |shift: u32| {
                let top = ((over >> shift) & 0xFF) as f32 * keep;
                let rest = ((below >> shift) & 0xFF) as f32 * (1.0 - a);
                ((top + rest).round().clamp(0.0, 255.0) as u32) << shift
            };
            *under = mix(24) | mix(16) | mix(8) | mix(0);
        }
    }
}

/// The body's low side across, in the window: the notch's face sits 2pt in from the window's
/// edge on the notch's side, and the body's near side is `near` out from it.
fn body_start(edge: Edge, (width, height): (i32, i32), across: f32, near: f32, s: f32) -> f32 {
    let inside = layout::NECK_INSIDE_TOAST * s;
    match edge {
        Edge::Right => width as f32 - inside - near - across,
        Edge::Bottom => height as f32 - inside - near - across,
        Edge::Left | Edge::Top => inside + near,
    }
}

/// A card's size as it lays out, `(across, along)` the notch.
fn natural_size(
    content: &CardContent,
    text: &mut TextPainter,
    s: f32,
    vertical: bool,
) -> (f32, f32) {
    let rect = card_rect(content, text, s);
    if vertical {
        (rect.2 as f32, rect.3 as f32)
    } else {
        (rect.3 as f32, rect.2 as f32)
    }
}

/// Where a contents block of size `own` (across, along) is drawn in the window: against the
/// body's near side inside a frame `frame_across` wide (the card on show's), centred on the
/// card's middle along the notch. A gliding card is a mask moving over contents that stay put.
fn contents_place(
    edge: Edge,
    start: f32,
    pose: &Body,
    origin_along: f32,
    frame_across: f32,
    (across, along): (f32, f32),
) -> (f32, f32) {
    let toward = match edge {
        Edge::Right | Edge::Bottom => start + pose.across - frame_across,
        Edge::Left | Edge::Top => start,
    };
    let across_at = toward + (frame_across - across) / 2.0;
    let along_at = pose.centre - origin_along - along / 2.0;
    if edge.is_vertical() {
        (across_at, along_at)
    } else {
        (along_at, across_at)
    }
}

/// A point of a gliding card's window (`x, y`), moved to where the same point of the card
/// on show would be in its own settled window, for `hit_at`: the contents are not where they
/// settle until the glide is over, and a click is on where they are.
pub fn gliding_point(
    content: &CardContent,
    pose: &Body,
    slide: f32,
    dpi: u32,
    text: &mut TextPainter,
    (x, y): (i32, i32),
) -> (i32, i32) {
    let s = layout::scale(dpi);
    let Some(tail) = content.tail else {
        return (x, y);
    };
    let (wx, wy, width, height) = pose.window;
    let vertical = tail.edge.is_vertical();
    let near = -pose.across - layout::NECK_INSIDE_TOAST * s
        + (layout::NECK_REACH * s + pose.across + layout::NECK_INSIDE_TOAST * s) * slide;
    let start = body_start(tail.edge, (width, height), pose.across, near, s);
    let origin_along = (if vertical { wy } else { wx }) as f32;
    let own = natural_size(content, text, s, vertical);
    let place = contents_place(tail.edge, start, pose, origin_along, own.0, own);
    let rect = card_rect(content, text, s);
    (
        (x as f32 - place.0 + rect.0 as f32).round() as i32,
        (y as f32 - place.1 + rect.1 as f32).round() as i32,
    )
}

/// `content` drawn at `origin` over the black body already on `canvas`, clipped to it, at
/// opacity `shown`.
fn lay_contents(
    canvas: &mut Canvas,
    (content, live): (&CardContent, &Live),
    text: &mut TextPainter,
    (s, phase): (f32, Option<f32>),
    origin: (f32, f32),
    shown: f32,
) {
    let mut ink = Canvas::new(canvas.width, canvas.height);
    let laid = plan(content, DRAWN, text, s);
    let mut pen = Pen {
        canvas: &mut ink,
        text,
        s,
        phase,
        pill: if content.lead.is_some() {
            TOAST_PILL
        } else {
            PILL_HEIGHT
        },
        pressed: &[],
        lit: &[],
        squeeze: 1.0,
    };
    if content.lead.is_some() {
        pen.banner(content, &laid, live, origin);
    } else {
        pen.flow(content, &laid, live, origin);
    }
    // Premultiplied, over the body and clipped to it.
    for (under, over) in canvas.pixels.iter_mut().zip(&ink.pixels) {
        let keep = shown * (*under >> 24) as f32 / 255.0;
        let a = (*over >> 24) as f32 / 255.0 * keep;
        if a <= 0.0 {
            continue;
        }
        let below = *under;
        let mix = |shift: u32| {
            let top = ((*over >> shift) & 0xFF) as f32 * keep;
            let rest = ((below >> shift) & 0xFF) as f32 * (1.0 - a);
            ((top + rest).round().clamp(0.0, 255.0) as u32) << shift
        };
        *under = mix(24) | mix(16) | mix(8) | mix(0);
    }
}

/// A card's body as a gliding card has it (the Mac's `NoticePose`), in screen device pixels:
/// the card window it lives in, the body's centre along the notch, its length along it and
/// its extent across, and where its neck is anchored along the notch (the ring's own place,
/// absolute, as the Mac's `tail`).
#[derive(Clone, Copy, Debug)]
pub struct Body {
    pub window: (i32, i32, i32, i32),
    pub centre: f32,
    pub length: f32,
    pub across: f32,
    pub tail: f32,
}

/// The body of `content` at rest with its window's top-left at `origin` (its tail is set).
pub fn card_body(
    content: &CardContent,
    dpi: u32,
    text: &mut TextPainter,
    origin: (i32, i32),
) -> Body {
    let s = layout::scale(dpi);
    let rect = card_rect(content, text, s);
    let (width, height) = window_size(content, rect, s);
    let tail = content.tail.unwrap_or(crate::card::Tail {
        edge: Edge::Right,
        offset: 0,
    });
    let (centre, length, across) = if tail.edge.is_vertical() {
        (origin.1 + rect.1 + rect.3 / 2, rect.3, rect.2)
    } else {
        (origin.0 + rect.0 + rect.2 / 2, rect.2, rect.3)
    };
    Body {
        window: (origin.0, origin.1, width, height),
        centre: centre as f32,
        length: length as f32,
        across: across as f32,
        tail: centre as f32 + tail.offset as f32,
    }
}

/// Hover lift over a pressed-able control: white at 16% (the Mac's `NoticeCard` pill fill).
const HOVER_ALPHA: f32 = 0.16;
/// A pressed pill or round button reads lighter than a hovered one.
const PRESS_ALPHA: f32 = 0.26;
/// How far the hover plate of a text row that acts reaches beyond its text, and its corners.
const ROW_HOVER_X: f32 = 6.0;
const ROW_HOVER_Y: f32 = 3.0;
const ROW_HOVER_RADIUS: f32 = 10.0;
/// The highlighted item's plate: the system accent, as the Mac's menu takes macOS's; a little
/// deeper while the button is held.
const MENU_HOVER_ALPHA: f32 = 1.0;
const MENU_PRESS_ALPHA: f32 = 0.82;
/// Windows' own accent blue, for a PC whose colour cannot be read and for the view shots.
pub const DEFAULT_ACCENT: u32 = 0x0078D4;
/// A fixed accent for the menu (the view shots set it so their pictures do not follow the
/// PC's colour); 0 reads the system's.
static MENU_ACCENT: AtomicU32 = AtomicU32::new(0);

#[cfg_attr(not(feature = "qa-views"), allow(dead_code))] // used by the qa-views screenshots
pub fn set_menu_accent(rgb: u32) {
    MENU_ACCENT.store(rgb, Ordering::Relaxed);
}

/// The menu highlight: the fixed accent if one is set, else Windows' accent colour (DWM's
/// colorization colour), else Windows' default blue.
fn menu_accent() -> u32 {
    let fixed = MENU_ACCENT.load(Ordering::Relaxed);
    if fixed != 0 {
        return fixed;
    }
    let (mut color, mut opaque) = (0u32, windows::core::BOOL::default());
    // SAFETY: two out-parameters owned here.
    match unsafe { windows::Win32::Graphics::Dwm::DwmGetColorizationColor(&mut color, &mut opaque) }
    {
        Ok(()) => color & 0x00FF_FFFF,
        Err(_) => DEFAULT_ACCENT,
    }
}
/// Inset of the Quit plate from the menu's outer edge (the Mac's: the hairline and a 5 pt gap
/// all round, a 24 pt plate in the 36 pt menu) and its corner radius.
const MENU_PLATE_INSET: f32 = 6.0;
const MENU_PLATE_RADIUS: f32 = 5.0;
/// The Send bar's plates under the Mac's `CardButtonStyle`: its +0.22 (hover) and +0.32
/// (pressed) brightness on the 0.176 plate, as white laid over that plate.
const BAR_HOVER_ALPHA: f32 = 0.22 / (1.0 - TRACK_ALPHA);
const BAR_PRESS_ALPHA: f32 = 0.32 / (1.0 - TRACK_ALPHA);

/// The primary button is held down over the card (set by the card window, read by every
/// `render_card_hover` so the plate under the pointer reads pressed).
static PRESSED: AtomicBool = AtomicBool::new(false);

/// Records whether the button is held down; true when that changed the picture.
pub fn set_pressed(down: bool) -> bool {
    PRESSED.swap(down, Ordering::Relaxed) != down
}

/// `render_card` with a hover plate over `hover` (a live pill, round button or device row).
pub fn render_card_hover(
    content: &CardContent,
    live: &Live,
    dpi: u32,
    text: &mut TextPainter,
    phase: Option<f32>,
    hover: Option<Hit>,
) -> Canvas {
    let slide = f32::from_bits(SLIDE.load(Ordering::Relaxed));
    let open = f32::from_bits(OPEN.load(Ordering::Relaxed));
    if (slide - 1.0).abs() > 0.001 || (open - 1.0).abs() > 0.001 {
        return render_sliding(content, live, dpi, text, phase, (slide, open));
    }
    let pressed = PRESSED.load(Ordering::Relaxed);
    let hits: Vec<Hit> = hover.into_iter().collect();
    draw_card_plates(content, live, dpi, text, phase, (&hits, pressed), &[])
}

/// `render_card` with every live button of the card forced to its hover look (or its pressed
/// look), as a view shot shows it; nothing on the live notch calls it.
#[cfg_attr(not(feature = "qa-views"), allow(dead_code))] // used by the qa-views screenshots
pub fn render_card_forced(
    content: &CardContent,
    live: &Live,
    dpi: u32,
    text: &mut TextPainter,
    pressed: bool,
) -> Canvas {
    let mut hits: Vec<Hit> = live
        .head
        .iter()
        .enumerate()
        .filter(|(_, on)| **on)
        .map(|(index, _)| Hit::Head(index))
        .collect();
    // A pill with nothing to do has no plate to light, but the Mac's pressed capture shrinks it
    // like the others.
    let mut idle: Vec<Hit> = Vec::new();
    for (row, buttons) in live.rows.iter().enumerate() {
        if pressed && matches!(content.rows.get(row), Some(Row::Pills(_))) {
            idle.extend(
                buttons
                    .iter()
                    .enumerate()
                    .filter(|(_, on)| !**on)
                    .map(|(button, _)| Hit::Row(row, button)),
            );
        }
        // On a toast the plain button row is the "Don't ask again" checkbox, which the Mac
        // does not draw as a card button: no plate over it.
        if content.lead.is_some() && matches!(content.rows.get(row), Some(Row::Button { .. })) {
            continue;
        }
        hits.extend(
            buttons
                .iter()
                .enumerate()
                .filter(|(_, on)| **on)
                .map(|(button, _)| Hit::Row(row, button)),
        );
    }
    draw_card_plates(content, live, dpi, text, None, (&hits, pressed), &idle)
}

/// The card with a hover (or pressed) plate laid over each of `hits`.
fn draw_card_plates(
    content: &CardContent,
    live: &Live,
    dpi: u32,
    text: &mut TextPainter,
    phase: Option<f32>,
    (hits, pressed): (&[Hit], bool),
    idle: &[Hit],
) -> Canvas {
    let s = layout::scale(dpi);
    let (rect, exact_height) = card_rect_exact(content, text, s);
    let (width, height) = window_size(content, rect, s);
    let mut canvas = Canvas::new(width as usize, height as usize);
    draw_silhouette(&mut canvas, content, rect, exact_height, s);
    let laid = plan(content, DRAWN, text, s);
    let origin = (rect.0 as f32, rect.1 as f32);
    // A held button shrinks, label and plate together.
    let squeezed: Vec<Hit> = hits
        .iter()
        .chain(idle)
        .copied()
        .filter(|hit| pressed && squeezes(content, *hit))
        .collect();
    // The plates go down first so the labels draw over them: a held button's ink is the
    // same as at rest (the Mac's plate sits under its label).
    for hit in hits {
        let Some((x, y, w, h, radius)) = hover_rect(content, &laid, *hit) else {
            continue;
        };
        let bar = matches!(
            match hit {
                Hit::Row(index, _) => content.rows.get(*index),
                Hit::Head(_) => None,
            },
            Some(Row::Button { .. } | Row::Pills(_))
        );
        let alpha = match (bar, pressed) {
            (true, true) => BAR_PRESS_ALPHA,
            (true, false) => BAR_HOVER_ALPHA,
            (false, true) => PRESS_ALPHA,
            (false, false) => HOVER_ALPHA,
        };
        let k = if squeezed.contains(hit) {
            PRESS_SCALE
        } else {
            1.0
        };
        let (x, y, w, h) = about((x, y, w, h), k);
        canvas.fill_round_rect(
            origin.0 + x * s,
            origin.1 + y * s,
            w * s,
            h * s,
            [radius * k * s; 4],
            0xFFFFFF,
            alpha,
        );
    }
    let mut pen = Pen {
        canvas: &mut canvas,
        text,
        s,
        phase,
        pill: if content.lead.is_some() {
            TOAST_PILL
        } else {
            PILL_HEIGHT
        },
        pressed: &squeezed,
        lit: hits,
        squeeze: 1.0,
    };
    if content.lead.is_some() {
        pen.banner(content, &laid, live, origin);
    } else {
        pen.flow(content, &laid, live, origin);
    }
    canvas
}

/// Whether `hit` is a button under the Mac's `CardButtonStyle`, which shrinks when pressed. A
/// text row that merely acts (the hover card's device line) only gets its plate.
fn squeezes(content: &CardContent, hit: Hit) -> bool {
    match hit {
        Hit::Head(_) => true,
        Hit::Row(index, _) => match content.rows.get(index) {
            Some(Row::Button { .. }) => content.lead.is_none(),
            Some(Row::Buttons { .. } | Row::Pills(_) | Row::Device { .. }) => true,
            _ => false,
        },
    }
}

/// The plate for `hit` in card DIPs `(x, y, width, height, radius)`: a header button, a pill
/// or round button, or a whole device row.
fn hover_rect(content: &CardContent, laid: &Plan, hit: Hit) -> Option<(f32, f32, f32, f32, f32)> {
    match hit {
        Hit::Head(index) => {
            let (x, y, w, h) = *laid.head.get(index)?;
            Some((x, y, w, h, w.min(h) / 2.0))
        }
        Hit::Row(index, button) => {
            let placed = laid.rows.get(index)?;
            match content.rows.get(index)? {
                Row::Buttons { .. } => {
                    let (x, w) = *placed.spans.get(button)?;
                    Some((x, placed.top, w, placed.height, w.min(placed.height) / 2.0))
                }
                Row::Pills(pills) => {
                    let (x, w) = *placed.spans.get(button)?;
                    Some((
                        x,
                        placed.top,
                        w,
                        pill_height(pills.get(button)?.symbol),
                        BUTTON_RADIUS,
                    ))
                }
                Row::Tiles(items) if button < items.len() => {
                    let (x, y, side, high) = tile_rect(button, laid.width, items);
                    Some((x, placed.top + y, side, high, TILE_RADIUS))
                }
                Row::Device { .. } | Row::File { .. } => Some((
                    laid.text_left,
                    placed.top,
                    laid.width - 2.0 * laid.text_left,
                    placed.height,
                    if content.lead.is_some() {
                        TOAST_ROW_RADIUS
                    } else {
                        DEVICE_RADIUS
                    },
                )),
                // A plain button row (the Send card's Cancel) is a plate over its whole width.
                Row::Button { .. } if content.lead.is_some() => Some((
                    laid.text_left - ROW_HOVER_X,
                    placed.top - ROW_HOVER_Y,
                    laid.width - CARD_PAD - laid.text_left + 2.0 * ROW_HOVER_X,
                    placed.height + 2.0 * ROW_HOVER_Y,
                    ROW_HOVER_RADIUS,
                )),
                Row::Button { .. } => Some((
                    CARD_PAD,
                    placed.top,
                    laid.width - 2.0 * CARD_PAD,
                    placed.height,
                    BUTTON_RADIUS,
                )),
                // A nearby device on the Send card's hover card is a text row that acts: the
                // Mac's row hover, a flat plate the row's own size (no reach past the text).
                Row::Pair { .. } => Some((
                    CARD_PAD,
                    placed.top,
                    laid.width - 2.0 * CARD_PAD,
                    placed.height,
                    ROW_HOVER_RADIUS.min(placed.height / 2.0),
                )),
                _ => None,
            }
        }
    }
}

fn menu_background(canvas: &mut Canvas, scale: f32) {
    let (w, h) = (canvas.width as f32, canvas.height as f32);
    let r = MENU_RADIUS * scale;
    canvas.fill_round_rect(0.0, 0.0, w, h, [r; 4], BORDER, 1.0);
    let inset = scale.max(1.0).round();
    canvas.fill_round_rect(
        inset,
        inset,
        w - 2.0 * inset,
        h - 2.0 * inset,
        [(r - inset).max(0.0); 4],
        MENU_FILL,
        1.0,
    );
}

/// Menu size in device pixels.
pub fn menu_size(dpi: u32) -> (i32, i32) {
    let s = layout::scale(dpi);
    (px(MENU_WIDTH, s), px(MENU_HEIGHT, s))
}

/// The right-click menu: the Mac's single "Quit Pulse" item, white on a dark grey pill with a
/// hairline border.
pub fn render_menu(dpi: u32, text: &mut TextPainter) -> Canvas {
    render_menu_hover(dpi, text, false)
}

/// `render_menu` with the Quit plate drawn when the pointer is `over` it (lighter while the
/// primary button is held, as the Send bar's buttons).
pub fn render_menu_hover(dpi: u32, text: &mut TextPainter, over: bool) -> Canvas {
    let s = layout::scale(dpi);
    let (width, height) = menu_size(dpi);
    let mut canvas = Canvas::new(width as usize, height as usize);
    menu_background(&mut canvas, s);
    if over {
        let alpha = if PRESSED.load(Ordering::Relaxed) {
            MENU_PRESS_ALPHA
        } else {
            MENU_HOVER_ALPHA
        };
        let inset = MENU_PLATE_INSET * s;
        canvas.fill_round_rect(
            inset,
            inset,
            width as f32 - 2.0 * inset,
            height as f32 - 2.0 * inset,
            [MENU_PLATE_RADIUS * s; 4],
            menu_accent(),
            alpha,
        );
    }
    draw_text(
        &mut canvas,
        text,
        crate::host::t(&crate::host::quit_label()),
        (
            px(MENU_PAD, s),
            px((MENU_HEIGHT - MENU_LINE) / 2.0 - 1.0, s),
        ),
        (MENU_SIZE, false),
        s,
        INK_PRIMARY,
        1.0,
    );
    canvas
}
