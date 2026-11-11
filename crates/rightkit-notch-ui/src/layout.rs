//! Notch geometry and the view model for its cells (which cells exist is the host's). Metrics are in DIPs (1/96 inch)
//! and scale with the monitor DPI; they are the Mac notch's design frame (a 44 pt ring in a
//! 117 px design ring, so one design px is `DESIGN` DIPs). Pure code.

use crate::host;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock};

/// The cells the notch draws, in order, as the host set them (`set_shown`); cell indices
/// everywhere (hit-testing, hover, cards, drops, rendering) indexes into this list.
static SHOWN: RwLock<Vec<Cell>> = RwLock::new(Vec::new());

/// The cell the notch keeps when minimised (`set_anchor`), if the host has one.
static ANCHOR: RwLock<Option<Cell>> = RwLock::new(None);

/// The notch is minimised to its anchor cell (what `shown` draws, preview included).
static MINIMISED: AtomicBool = AtomicBool::new(false);
/// The view shots draw no room for the minimise button: their frame is the Mac's.
static NO_LEAD: AtomicBool = AtomicBool::new(false);

/// The cells the host has on, in notch order, whatever the notch is showing of them (empty
/// until `set_shown` is called).
pub fn base_shown() -> Vec<Cell> {
    SHOWN.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The anchor cell, when the host set one and it is among the cells that are on.
fn base_anchor() -> Option<Cell> {
    let anchor = (*ANCHOR.read().unwrap_or_else(|e| e.into_inner()))?;
    let list = SHOWN.read().unwrap_or_else(|e| e.into_inner());
    list.contains(&anchor).then_some(anchor)
}

/// Sets the cell the notch keeps when it is minimised (or none: the notch then never minimises).
/// The host decides which; a sharing app keeps its Send cell.
pub fn set_anchor(cell: Option<Cell>) {
    *ANCHOR.write().unwrap_or_else(|e| e.into_inner()) = cell;
}

/// Minimises the notch to its anchor cell, or restores it. Ignored without an anchor to keep.
pub fn set_minimised(on: bool) {
    MINIMISED.store(on, Ordering::Relaxed);
}

/// Whether the notch is drawn as the anchor cell alone (a preview counts).
pub fn minimised() -> bool {
    MINIMISED.load(Ordering::Relaxed) && base_anchor().is_some()
}

/// The shown cells in notch order: the cells that are on, or the anchor cell alone while
/// minimised.
pub fn shown() -> Vec<Cell> {
    if MINIMISED.load(Ordering::Relaxed)
        && let Some(anchor) = base_anchor()
    {
        return vec![anchor];
    }
    base_shown()
}

/// Replaces the shown cells. An empty list is ignored: the notch never has no rings.
pub fn set_shown(cells: Vec<Cell>) {
    if !cells.is_empty() {
        *SHOWN.write().unwrap_or_else(|e| e.into_inner()) = cells;
    }
}

/// How many cells the notch draws: at least one, so the geometry is defined before the host
/// sets its cells.
pub fn count() -> usize {
    if minimised() {
        return 1;
    }
    SHOWN.read().unwrap_or_else(|e| e.into_inner()).len().max(1)
}

/// The notch has a minimise button: the switch is on, the anchor cell is among the cells and
/// there is another cell to put away.
pub fn minimise_available() -> bool {
    if NO_LEAD.load(Ordering::Relaxed) || !host::switch_on("notchMinimizeButton") {
        return false;
    }
    base_anchor().is_some() && SHOWN.read().unwrap_or_else(|e| e.into_inner()).len() > 1
}

/// Room the window keeps before the start of the shape for the minimise button's zone (the
/// end's `HANDLE_OVERHANG` mirrored), in DIPs; none without the button.
pub fn lead() -> f32 {
    if minimise_available() {
        HANDLE_OVERHANG
    } else {
        0.0
    }
}

/// `lead` in device pixels.
pub fn lead_px(dpi: u32) -> f32 {
    lead() * scale(dpi)
}

/// The view shots draw the Mac's frame: no room for the minimise button.
#[cfg_attr(not(feature = "qa-views"), allow(dead_code))] // used by the qa-views screenshots
pub fn no_minimise_room() {
    NO_LEAD.store(true, Ordering::Relaxed);
}

/// For the view shots: whether a shot keeps room for the minimise button (the Mac's frame has
/// it; the shots of the other views draw none).
#[cfg_attr(not(feature = "qa-views"), allow(dead_code))] // used by the qa-views screenshots
pub fn set_minimise_room(on: bool) {
    NO_LEAD.store(!on, Ordering::Relaxed);
}

/// The cell at `index` among the shown cells.
pub fn cell_at(index: usize) -> Option<Cell> {
    shown().get(index).copied()
}

/// Index of the anchor cell among the shown cells, if it is shown.
pub fn anchor_cell() -> Option<usize> {
    let anchor = base_anchor()?;
    shown().iter().position(|c| *c == anchor)
}

/// One cell of the notch, defined by the host: which cells exist, what they read and what a
/// click opens are the application's. The notch only places, hit-tests and draws them.
///
/// ```
/// use rightkit_notch_ui::layout::{self, Cell};
///
/// const CPU: Cell = Cell::new(1, "cpu", "monitor");
/// const SEND: Cell = Cell::new(2, "Snd", "general");
/// layout::set_shown(vec![CPU, SEND]);
/// layout::set_anchor(Some(SEND));
/// assert_eq!(layout::cell_at(1).map(Cell::section), Some("general"));
/// assert_eq!(layout::anchor_cell(), Some(1));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cell {
    /// The host's own id for the cell (unique among the cells it shows).
    pub id: u32,
    /// Key of the mark drawn inside the ring (`rightkit_win_ui::glyphs::Glyph::from_key`).
    pub glyph: &'static str,
    /// What a click opens (the host's page or section name).
    pub section: &'static str,
}

impl Cell {
    /// A cell with the host's `id`, the mark `glyph` and the `section` a click opens.
    pub const fn new(id: u32, glyph: &'static str, section: &'static str) -> Self {
        Self { id, glyph, section }
    }

    /// What a click opens.
    pub fn section(self) -> &'static str {
        self.section
    }

    /// Key of the mark drawn inside the ring.
    pub fn glyph(self) -> &'static str {
        self.glyph
    }
}

/// Screen edge the notch is welded to. Top is the Windows default (the Mac default is right);
/// left and right stack the rings vertically, like the Mac side notch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Edge {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
}

impl Edge {
    pub fn as_str(self) -> &'static str {
        match self {
            Edge::Top => "top",
            Edge::Bottom => "bottom",
            Edge::Left => "left",
            Edge::Right => "right",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "top" => Some(Edge::Top),
            "bottom" => Some(Edge::Bottom),
            "left" => Some(Edge::Left),
            "right" => Some(Edge::Right),
            _ => None,
        }
    }

    /// Left and right edges stack the rings top to bottom.
    pub fn is_vertical(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }
}

/// Notch-wide badges: a newer version is waiting, or an approval is pending.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Badges {
    pub update: bool,
    pub permissions: bool,
}

// Metrics in DIPs.
/// DIPs per Mac design px (the design frame measures the 44 pt ring as 117 px).
pub const DESIGN: f32 = 44.0 / 117.0;
pub const RING: f32 = 44.0;
pub const TRACK_STROKE: f32 = 15.5 * DESIGN;
pub const PROGRESS_STROKE: f32 = 8.0 * DESIGN;
pub const WEEKLY_RADIUS: f32 = 37.5 * DESIGN;
pub const WEEKLY_STROKE: f32 = 5.0 * DESIGN;
/// The second ring's radius when it is drawn outside the main ring (the Mac's 65 px).
pub const WEEKLY_OUTSIDE_RADIUS: f32 = 65.0 * DESIGN;
/// The dashed second ring's dash and gap along the arc (the Mac's `[4, 2]` points).
pub const WEEKLY_DASH: (f32, f32) = (4.0, 2.0);
/// Space between neighbouring rings along the stack.
pub const SPACING: f32 = 26.0 * DESIGN;
/// Space from the end of the straight body to the first ring (and from the last ring).
pub const PAD_ALONG: f32 = 22.0 * DESIGN;
/// The taper where each end meets the bezel (the Mac `curlRadius`, 64 design px): 24pt at rest
/// (liquid-motion pass §2). With a corner of half the depth each end is a half circle about
/// its ring that tapers into the bezel 54pt past the ring's centre.
///
/// `SideNotchShape` draws the flare as one `fluidTurn` that reaches `curlDepth` across and `curl`
/// along, both `curlRadius` (the shape clamps the depth by the body's depth, the corner and the
/// 2pt band, which the open body is clear of), so the ear is as deep as it is long. An earlier
/// round fitted 0.9 of this, then a 1.14 depth to it, from profiles that had the ear's own
/// length wrong; fitting the Mac's top-edge rows with the walk gives 46.5 by 49.5 px (the sum
/// 96 px is 2 x 48.1), and the ring centres only land with the full 64 (first ring 91 px from
/// the tip on the Mac).
///
/// Round 11: the strip's edge row read 618 px against the Mac's 614, the first ring 92 px and
/// the last 94 from their tips against 91 (the thin tail reads past the Mac's). One DIP is
/// taken off each end (the ring distances become 90 and 92, and the row 614), the ends' caps
/// still concentric with their rings.
pub const CURL: f32 = 64.0 * DESIGN - 1.0;
/// Depth of the open body, bezel band included.
pub const DEPTH: f32 = 161.0 * DESIGN;
/// Radius of the body's far corners: half the depth, so each end is a half circle concentric
/// with the end ring, the same 8.3pt margin all round (liquid-motion pass §2).
pub const CORNER: f32 = DEPTH / 2.0;
/// The taper at full gather: the ends all but closed round their rings (liquid-motion pass §1).
pub const GATHERED_TAPER: f32 = 4.0;
/// The band the Mac shape keeps past the screen edge so no hairline of wallpaper shows.
pub const BAND: f32 = 2.0;
/// The percent reading under a ring (`showsNotchReadings`): the Mac's gap from the ring
/// (26.9 px) and its label's line. The line's height is the Mac's font line for the label,
/// taken here as a round figure.
pub const LABEL_GAP: f32 = 26.9 * DESIGN;
pub const LABEL_LINE: f32 = 17.0;
/// The label's sizes: capitals 27 px tall alone, 22 px for the pair "30%/70%".
pub const LABEL_SIZE: f32 = 27.0 * DESIGN / 0.714;
pub const LABEL_PAIR_SIZE: f32 = 22.0 * DESIGN / 0.714;

/// Whether the rings carry their percent reading under them.
pub fn readings_on() -> bool {
    host::switch_on("showsNotchReadings")
}

/// One cell along the stack: the ring, and on a side edge the reading under it. A top or
/// bottom edge keeps the reading in the body's depth instead.
pub fn cell_along(edge: Edge) -> f32 {
    if edge.is_vertical() && readings_on() {
        RING + LABEL_GAP + LABEL_LINE
    } else {
        RING
    }
}

/// The open body's depth, bezel band included. A top or bottom edge takes the reading into
/// its depth, past the ring.
pub fn body_depth(edge: Edge) -> f32 {
    if !edge.is_vertical() && readings_on() {
        DEPTH + LABEL_GAP + LABEL_LINE
    } else {
        DEPTH
    }
}

/// Length of the whole open shape along the screen edge, ears included, for the shown cells.
pub fn shape_length(edge: Edge) -> f32 {
    let n = count();
    2.0 * CURL + 2.0 * PAD_ALONG + n as f32 * cell_along(edge) + (n - 1) as f32 * SPACING
}
/// The handle (liquid-motion pass §1, the Mac's `NotchBulb`): a bulb that comes out of the end
/// ring's cap on a neck. Along the stack from the end ring's centre to the bulb's: just inside
/// the cap at rest, 41pt out; its radius 14 to 22pt.
pub const BULB_REST_CENTRE: f32 = 8.0;
pub const BULB_OUT_CENTRE: f32 = 41.0;
pub const BULB_REST_RADIUS: f32 = 14.0;
pub const BULB_OUT_RADIUS: f32 = 22.0;
/// What a press or a click on the bulb answers to: its out size and a little over.
pub const BULB_HIT: f32 = BULB_OUT_RADIUS + 6.0;
/// The neck leaves the cap from this far along, `NECK_BASE` wide, pinched toward the bulb to
/// `NECK_PINCH_OUT`, and `NECK_TOP` of the bulb's radius across at the bulb.
pub const NECK_FROM: f32 = 18.0;
pub const NECK_BASE: f32 = 44.0;
pub const NECK_PINCH_OUT: f32 = 30.0;
pub const NECK_TOP: f32 = 1.7;
/// How far deeper into the screen the Mac's bulb, its neck and its glyph sit than the end
/// ring's centre (measured: the bulb's centre 4.5 px lower at 2x on the top edge, 3.5 on the
/// right).
pub const BULB_DROP: f32 = 2.0;
/// How far deeper than the end ring's centre the badges sit at rest (they ride the bulb's frame,
/// whose centre is `notchDepth / 2`): the dot is 35 px from the send glyph across on the Mac's
/// 2x shot where 0.62 of the cap's radius puts it 37.5 px.
pub const BADGE_DROP: f32 = 1.25;
/// The glyph's size in the bulb.
pub const BULB_GLYPH: f32 = 12.0;
/// The pointer calls the handle out from half a ring back of the end ring's middle to this far
/// past the shape's end, across the notch's full depth.
pub const HANDLE_ZONE_REACH: f32 = 16.0;
/// Room the window keeps past the end of the shape: the handle's zone (the bulb, out, ends
/// well inside it).
pub const HANDLE_OVERHANG: f32 = 44.0;
/// The rings' centre across the window, from the screen edge.
pub const RING_ACROSS: f32 = DEPTH / 2.0 - BAND;
/// Gap between the notch and a hover card.
pub const CARD_GAP: f32 = 6.0;
/// A card's face sits 17pt from the notch (3pt gap and 14), joined to it by a neck that
/// reaches 2pt into the notch's black (the Mac's 27fabf97).
pub const NECK_REACH: f32 = 17.0;
/// 2.25: the Mac's card shots keep a faint column of the neck one pixel past the 2 pt mouth (490 px wide to 489).
pub const NECK_INSIDE: f32 = 2.25;
/// A toast's neck still reaches 2pt in: its tail is 430 px on the Mac's shots where the card's
/// 2.25 makes the tooltip's 490.
pub const NECK_INSIDE_TOAST: f32 = 2.0;
/// Where a card's window starts relative to the notch's visible edge, in device pixels (negative
/// reaches into the notch). The Mac hangs the card's face `NECK_REACH` past `notchDrawnDepth`,
/// which keeps the 2pt bezel band the screen hides (NoticeStage.swift 169-180, `face` from the
/// notch's inset): from the visible edge the face is `NECK_REACH + BAND` away. The window's own
/// neck is `NECK_REACH + inside` long, so it starts that much less than the neck inside.
pub fn neck_gap(toast: bool, s: f32) -> i32 {
    let inside = if toast {
        NECK_INSIDE_TOAST
    } else {
        NECK_INSIDE
    };
    let px = |dip: f32| (dip * s).round() as i32;
    px(NECK_REACH + BAND) - px(NECK_REACH + inside)
}
/// Folded pill: depth (band included) and length along the edge, in logical pixels: the
/// Mac's resting side notch, `NotchLayout.pillWidth` (26 design px, about 10pt, its bezel band
/// included, so about 8pt shows) and `pillHeight` (210 design px, about 79pt) long. Its ends'
/// curls are capped by the depth, so they stay small with it. The pointer finds it at the
/// screen's edge.
pub const PILL_DEPTH: f32 = 10.0;
/// The Mac's `pillHeight`, 210 design px (157.9 px). The shape starts `PILL_TAILS` in from each
/// end, where the Mac's top and left pill (152 px) stops showing the tails of its flare.
pub const PILL_LONG: f32 = 210.0 * DESIGN;
/// How far in each end of the resting pill's shape starts, in DIPs.
pub const PILL_TAILS: f32 = 0.5;
pub const BADGE_PERMISSIONS: u32 = 0xFFB340;
pub const BADGE_UPDATE: u32 = 0xA51D24;

pub const BAND_AMPLE: u32 = 0x2E6B4A;
pub const BAND_WATCH: u32 = 0xC2570F;
pub const BAND_CRITICAL: u32 = 0xA51D24;

/// The Mac's `UsageBand.rampColor` in the dark palette: a continuous blend from the ample green to
/// the watch orange at `watch` (a fraction), then from there to the critical red at 100%, each
/// channel blended on its own. Like the Mac's ramp it starts from the palette's green, not the accent.
pub fn ramp_color(fraction: f32, watch: f32) -> u32 {
    let f = fraction.clamp(0.0, 1.0);
    if !(watch > 0.0 && watch < 1.0) {
        return if f < 1.0 { BAND_AMPLE } else { BAND_CRITICAL };
    }
    if f < watch {
        blend_channels(BAND_AMPLE, BAND_WATCH, f / watch)
    } else {
        blend_channels(BAND_WATCH, BAND_CRITICAL, (f - watch) / (1.0 - watch))
    }
}

fn blend_channels(from: u32, to: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let channel = |shift: u32| {
        let a = ((from >> shift) & 0xFF) as f32;
        let b = ((to >> shift) & 0xFF) as f32;
        ((a + (b - a) * t).round() as u32) & 0xFF
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}
pub const INK_PRIMARY: u32 = 0xFFFFFF;
pub const RING_TRACK_ALPHA: f32 = 0.188;

/// Device pixels per DIP for `dpi`.
pub fn scale(dpi: u32) -> f32 {
    dpi.clamp(48, 480) as f32 / 96.0
}

/// Canvas depth (across the notch) in device pixels once the bezel band is cropped away.
fn open_depth(edge: Edge, dpi: u32) -> i32 {
    ((body_depth(edge) - BAND) * scale(dpi)).round() as i32
}

/// Canvas position of the point `along` the stack and `depth` in from the screen edge,
/// both in device pixels, on a canvas `across` pixels deep.
pub fn to_canvas(edge: Edge, along: f32, depth: f32, across: f32) -> (f32, f32) {
    match edge {
        Edge::Top => (along, depth),
        Edge::Bottom => (along, across - depth),
        Edge::Left => (depth, along),
        Edge::Right => (across - depth, along),
    }
}

/// What one cell shows, quantised to whole percents so identical readings compare equal and
/// a redraw happens only when something visible changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellView {
    pub glyph: &'static str,
    /// Main ring, percent used.
    pub main: Option<u8>,
    /// Thin inner ring (a second, shorter window of the same reading), percent used.
    pub inner: Option<u8>,
    /// Latest poll failed; the reading is dimmed.
    pub stale: bool,
    /// Sharing cannot work: the glyph is drawn in the warning colour.
    pub problem: bool,
    /// A limit is spent: both arcs read as exhausted and the glyph is dimmed.
    pub blocked: bool,
    /// Ring colour decided by the reading itself rather than by the used share (the System cell's memory ring).
    /// `None` colours by the share's band.
    pub band: Option<u32>,
    /// A usage ring the host marks as such: the colour transition setting applies to it.
    pub provider: bool,
    /// The reading drawn under the ring (`showsNotchReadings`): a percentage, the pair
    /// "30%/70%", or a dash. `None` draws nothing.
    pub reading: Option<String>,
    /// The cell carries the notch's warning (a gauge is past its level): the band between its
    /// ring and its icon pulses deep red.
    pub warn: bool,
    /// Dots round the ring, one per pending item in order (`true` = selected), such as a copy
    /// burst's items still to paste; empty draws none.
    pub burst: Vec<bool>,
    /// A short tag ("A", "B") drawn on the ring, such as which of several accounts it reads;
    /// `None` draws none.
    pub badge: Option<String>,
    /// The cell's subject is working: a short arc spins round the ring (the Mac's activity
    /// arc).
    pub working: bool,
}

static HEADLINE_OUTSIDE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// For the view shots: draw the headline window outside whatever the switches say.
#[cfg_attr(not(feature = "qa-views"), allow(dead_code))] // used by the qa-views screenshots
pub fn draw_headline_outside() {
    HEADLINE_OUTSIDE.store(true, std::sync::atomic::Ordering::Relaxed);
    no_minimise_room();
}

static WARNING: Mutex<Option<String>> = Mutex::new(None);

/// Sets the warning line (or clears it); true when it changed.
pub fn set_warning(line: Option<String>) -> bool {
    let mut held = WARNING.lock().unwrap_or_else(|e| e.into_inner());
    if *held == line {
        return false;
    }
    *held = line;
    true
}

/// The line naming the gauge past its warning level, when there is one.
pub fn warning_line() -> Option<String> {
    WARNING.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

// ---- orientation: the notch on any screen edge ----------------------------------------------

/// Notch body size in device pixels on `edge`. Top and bottom keep the horizontal row; left
/// and right stack the five rings downwards.
pub fn body_size_for(edge: Edge, dpi: u32) -> (i32, i32) {
    let along = ((shape_length(edge) + HANDLE_OVERHANG + lead()) * scale(dpi)).round() as i32;
    let depth = open_depth(edge, dpi);
    if edge.is_vertical() {
        (depth, along)
    } else {
        (along, depth)
    }
}

/// Folded pill size in device pixels on `edge`.
pub fn pill_size_for(edge: Edge, dpi: u32) -> (i32, i32) {
    let s = scale(dpi);
    let (thick, long) = (
        ((PILL_DEPTH - BAND) * s).round() as i32,
        (PILL_LONG * s).round() as i32,
    );
    if edge.is_vertical() {
        (thick, long)
    } else {
        (long, thick)
    }
}

/// Window size for the notch: the pill while folded, the body when open.
pub fn panel_size(edge: Edge, folded: bool, dpi: u32) -> (i32, i32) {
    if folded {
        pill_size_for(edge, dpi)
    } else {
        body_size_for(edge, dpi)
    }
}

/// Centre of cell `index`'s ring in notch-local device pixels.
pub fn ring_center(edge: Edge, index: usize, dpi: u32) -> (f32, f32) {
    let s = scale(dpi);
    let pitch = cell_along(edge) + SPACING;
    let along = (CURL + PAD_ALONG + RING / 2.0 + index as f32 * pitch + lead()) * s;
    let depth = (DEPTH / 2.0 - BAND) * s;
    to_canvas(edge, along, depth, open_depth(edge, dpi) as f32)
}

/// Where the pointer is on the settings handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// In the handle's zone: the bulb comes out and the notch gathers.
    Zone,
    /// On the bulb itself: a click opens Settings, a press that moves carries the notch.
    Bulb,
}

/// A point of the open notch in DIPs: along the stack from the shape's start, and in depth
/// from the screen edge. `x` and `y` are notch-local device pixels on `edge`.
fn stack_point(edge: Edge, x: i32, y: i32, dpi: u32) -> (f32, f32) {
    let s = scale(dpi);
    let across = open_depth(edge, dpi) as f32;
    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
    let (along, depth) = match edge {
        Edge::Top => (px, py),
        Edge::Bottom => (px, across - py),
        Edge::Left => (py, px),
        Edge::Right => (py, across - px),
    };
    (along / s - lead(), depth / s)
}

/// Along the stack, in DIPs, the centre of the first ring, the one the minimise button hangs off.
pub fn first_ring_along() -> f32 {
    CURL + PAD_ALONG + RING / 2.0
}

/// The minimise button's part under a point in notch-local device pixels on `edge`: the bulb's
/// zone mirrored at the start of the stack (`Zone` while the button comes out, `Bulb` on the
/// button itself). None without the button.
pub fn mini_at(edge: Edge, x: i32, y: i32, dpi: u32) -> Option<Handle> {
    if lead() <= 0.0 {
        return None;
    }
    let (along, across) = stack_point(edge, x, y, dpi);
    let ring = first_ring_along();
    if !(0.0..=body_depth(edge) - BAND).contains(&across)
        || along > ring + RING / 2.0
        || along < -HANDLE_ZONE_REACH
    {
        return None;
    }
    let on_button =
        (along - (ring - BULB_OUT_CENTRE)).hypot(across - RING_ACROSS - BULB_DROP) <= BULB_HIT;
    let on_ring = (along - ring).hypot(across - RING_ACROSS) <= RING / 2.0;
    Some(if on_button && !on_ring {
        Handle::Bulb
    } else {
        Handle::Zone
    })
}

/// Along the stack, in DIPs, the centre of the end ring the bulb hangs off.
pub fn handle_ring_along(edge: Edge) -> f32 {
    CURL + PAD_ALONG + RING / 2.0 + (count() - 1) as f32 * (cell_along(edge) + SPACING)
}

/// The centre of the bulb when it is out, in notch-local device pixels on `edge`.
pub fn bulb_center(edge: Edge, dpi: u32) -> (f32, f32) {
    let s = scale(dpi);
    let along = (handle_ring_along(edge) + BULB_OUT_CENTRE + lead()) * s;
    to_canvas(
        edge,
        along,
        (RING_ACROSS + BULB_DROP) * s,
        open_depth(edge, dpi) as f32,
    )
}

/// The settings handle under a point in notch-local device pixels on `edge` (the Mac's
/// `isInHandleZone` and `isOnBulb`). The end ring itself stays the ring's, so its card still
/// opens with the bulb out.
pub fn handle_at(edge: Edge, x: i32, y: i32, dpi: u32) -> Option<Handle> {
    let (along, across) = stack_point(edge, x, y, dpi);
    let ring = handle_ring_along(edge);
    if !(0.0..=body_depth(edge) - BAND).contains(&across)
        || along < ring - RING / 2.0
        || along > shape_length(edge) + HANDLE_ZONE_REACH
    {
        return None;
    }
    let on_bulb =
        (along - ring - BULB_OUT_CENTRE).hypot(across - RING_ACROSS - BULB_DROP) <= BULB_HIT;
    let on_ring = (along - ring).hypot(across - RING_ACROSS) <= RING / 2.0;
    Some(if on_bulb && !on_ring {
        Handle::Bulb
    } else {
        Handle::Zone
    })
}

/// Cell under a point in notch-local device pixels on `edge`.
pub fn cell_at_for(edge: Edge, x: i32, y: i32, dpi: u32) -> Option<usize> {
    let (width, height) = body_size_for(edge, dpi);
    if x < 0 || y < 0 || x >= width || y >= height {
        return None;
    }
    let s = scale(dpi);
    let along = (if edge.is_vertical() { y } else { x }) as f32 - lead_px(dpi);
    // Before the start of the shape, past its end, and on either button there is no ring.
    if along < 0.0
        || along >= shape_length(edge) * s
        || handle_at(edge, x, y, dpi) == Some(Handle::Bulb)
        || mini_at(edge, x, y, dpi) == Some(Handle::Bulb)
    {
        return None;
    }
    let first_edge = (CURL + PAD_ALONG - SPACING / 2.0) * s;
    let index = ((along - first_edge) / ((cell_along(edge) + SPACING) * s)).floor();
    Some((index.max(0.0) as usize).min(count() - 1))
}

/// Top-left of a notch of `size` docked to `edge` of `monitor` (`left, top, right, bottom`),
/// its centre `along` per mille of the way along the edge, kept inside the monitor.
pub fn origin_for(
    edge: Edge,
    monitor: (i32, i32, i32, i32),
    size: (i32, i32),
    along: u16,
) -> (i32, i32) {
    let (ml, mt, mr, mb) = monitor;
    let (w, h) = size;
    match edge {
        Edge::Top => (left_for_along(ml, mr - ml, w, along), mt),
        Edge::Bottom => (left_for_along(ml, mr - ml, w, along), (mb - h).max(mt)),
        Edge::Left => (ml, left_for_along(mt, mb - mt, h, along)),
        Edge::Right => ((mr - w).max(ml), left_for_along(mt, mb - mt, h, along)),
    }
}

/// Inverse of `origin_for` along the edge: the per-mille position of the notch's centre for a
/// notch whose top-left is `origin`.
pub fn along_for_origin(
    edge: Edge,
    monitor: (i32, i32, i32, i32),
    size: (i32, i32),
    origin: (i32, i32),
) -> u16 {
    let (ml, mt, mr, mb) = monitor;
    if edge.is_vertical() {
        along_for_left(mt, mb - mt, size.1, origin.1)
    } else {
        along_for_left(ml, mr - ml, size.0, origin.0)
    }
}

/// Edge of `monitor` nearest the cursor, staying on `current` unless another edge is clearly
/// closer (a margin of 16 DIPs stops the notch flickering between edges near a corner).
pub fn edge_for_cursor(
    current: Edge,
    monitor: (i32, i32, i32, i32),
    cursor: (i32, i32),
    dpi: u32,
) -> Edge {
    let (ml, mt, mr, mb) = monitor;
    let distance = |edge: Edge| -> i64 {
        i64::from(match edge {
            Edge::Top => cursor.1 - mt,
            Edge::Bottom => mb - 1 - cursor.1,
            Edge::Left => cursor.0 - ml,
            Edge::Right => mr - 1 - cursor.0,
        })
    };
    let margin = (16.0 * scale(dpi)) as i64;
    let mut best = current;
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        if distance(edge) + margin < distance(best) {
            best = edge;
        }
    }
    best
}

/// Top-left of a card of `card` size beside a notch (`left, top, right, bottom`) on `edge`,
/// centred on `centre` (screen coordinate of the cell along the edge) and kept inside the
/// monitor. The card opens away from the screen edge.
pub fn card_origin(
    edge: Edge,
    notch: (i32, i32, i32, i32),
    centre: i32,
    card: (i32, i32),
    gap: i32,
    monitor: (i32, i32, i32, i32),
) -> (i32, i32) {
    let (nl, nt, nr, nb) = notch;
    let (cw, ch) = card;
    let (ml, mt, mr, mb) = monitor;
    let across = |value: i32, size: i32, lo: i32, hi: i32| value.clamp(lo, (hi - size).max(lo));
    match edge {
        Edge::Top => (across(centre - cw / 2, cw, ml, mr), nb + gap),
        Edge::Bottom => (across(centre - cw / 2, cw, ml, mr), nt - gap - ch),
        Edge::Left => (nr + gap, across(centre - ch / 2, ch, mt, mb)),
        Edge::Right => (nl - gap - cw, across(centre - ch / 2, ch, mt, mb)),
    }
}

/// Left edge of the notch for a placement along a monitor's top edge. `along` is the
/// per-mille position of the notch's centre across the monitor width.
pub fn left_for_along(monitor_left: i32, monitor_width: i32, notch_width: i32, along: u16) -> i32 {
    let centre = monitor_left as i64 + monitor_width as i64 * i64::from(along.min(1000)) / 1000;
    let left = centre - i64::from(notch_width) / 2;
    let max_left = (monitor_left as i64 + monitor_width as i64 - i64::from(notch_width))
        .max(monitor_left as i64);
    left.clamp(monitor_left as i64, max_left) as i32
}

/// Inverse of `left_for_along`: where the notch centre sits across the monitor, per mille.
pub fn along_for_left(monitor_left: i32, monitor_width: i32, notch_width: i32, left: i32) -> u16 {
    if monitor_width <= 0 {
        return 500;
    }
    let centre = i64::from(left) + i64::from(notch_width) / 2 - i64::from(monitor_left);
    (centre * 1000 / i64::from(monitor_width)).clamp(0, 1000) as u16
}
