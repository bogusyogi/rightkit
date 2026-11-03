//! `PaintCtx`: the software canvas a `Painter` draws into.
//!
//! The surface is premultiplied BGRA (`0xAARRGGBB` as a little-endian `u32`), the exact format
//! `UpdateLayeredWindow` takes, so a finished frame is one memory copy into the DIB. Every
//! coordinate a painter passes is in **logical pixels** relative to the window top-left; the
//! context scales by the DPI of the monitor the window is on, so a painter written once is
//! sharp at 100%, 150% and 200%.
//!
//! The rasteriser is lifted in spirit from the Pulse notch canvas (signed-distance rounded
//! rectangles, continuous-curvature squircles, arc strokes, sub-scanline polygon fill,
//! distance-field polyline strokes, the bilinear image blit) and from the HeardRight spike pill
//! (cached glass layer, box blur, SVG icon paths). It has no dependencies and no Win32 calls, so
//! it unit-tests anywhere; only `text` reaches GDI.

use std::f32::consts::TAU;

use rightkit_overlay::Rect;

use crate::color::{blend, blend_premul, Rgba};
use crate::geometry::{scale_for_dpi, Size, MAX_SURFACE_SIDE};
use crate::path::{self, PathCmd, Subpath};
use crate::squircle::Squircle;
use crate::text::{Mask, TextEngine};

/// Glass layers kept for reuse (one per distinct shape and look).
const GLASS_CACHE_MAX: usize = 4;
/// A glass bitmap larger than this is drawn but not cached.
const GLASS_CACHE_MAX_PIXELS: usize = 2_000_000;
/// Rendered strings kept for reuse (a ticking timer re-uses its glyph runs between ticks).
const MASK_CACHE_MAX: usize = 24;

/// Largest glass blur, in device pixels. A larger request is clamped to this.
pub(crate) const GLASS_MAX_BLUR_PX: f32 = 64.0;
/// Longest side of a glass bitmap (body plus its shadow margin), in device pixels.
pub(crate) const GLASS_MAX_SIDE: usize = 4096;
/// Peak working memory one glass build may use. A build holds the pixels plus three float planes
/// (body, shadow, blur scratch), [`GLASS_BYTES_PER_PIXEL`] bytes per bitmap pixel in all. This is
/// the padded-layer area cap of [`GLASS_CACHE_MAX_PIXELS`] (2,000,000 pixels, 32 MB of working
/// memory): a layer above it is refused, never built.
pub(crate) const GLASS_MAX_WORKING_BYTES: usize = GLASS_CACHE_MAX_PIXELS * GLASS_BYTES_PER_PIXEL;
/// Bytes per bitmap pixel while a glass layer is built: one `u32` plus three `f32` planes.
pub(crate) const GLASS_BYTES_PER_PIXEL: usize = 16;
/// Largest font cell height handed to the text engine, in device pixels.
const MAX_FONT_PX: f32 = 1024.0;

/// Horizontal placement of text inside its rectangle (text is always centred vertically).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// A typeface request. `size` is the font cell height in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Font {
    pub family: &'static str,
    pub size: f32,
    /// GDI weight, 400 regular, 600 semibold, 700 bold.
    pub weight: u16,
    pub italic: bool,
}

impl Font {
    pub const fn new(family: &'static str, size: f32, weight: u16) -> Self {
        Self {
            family,
            size,
            weight,
            italic: false,
        }
    }

    /// Segoe UI, regular.
    pub const fn ui(size: f32) -> Self {
        Self::new("Segoe UI", size, 400)
    }

    /// Segoe UI Semibold.
    pub const fn ui_semibold(size: f32) -> Self {
        Self::new("Segoe UI Semibold", size, 600)
    }

    /// Consolas, regular (digits that do not jiggle: timers, counters).
    pub const fn mono(size: f32) -> Self {
        Self::new("Consolas", size, 400)
    }

    pub const fn with_weight(self, weight: u16) -> Self {
        Self { weight, ..self }
    }

    pub const fn with_italic(self, italic: bool) -> Self {
        Self { italic, ..self }
    }
}

/// Look of a level meter drawn by [`PaintCtx::bars`]. Sizes are logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarStyle {
    pub color: Rgba,
    pub bar_width: f32,
    pub gap: f32,
    /// Height of a bar at level 0; a level of 1 fills the rectangle height.
    pub min_height: f32,
    /// Corner radius (clamped to half the bar width).
    pub radius: f32,
}

impl BarStyle {
    /// The recording-meter look: 3 px bars, 2.5 px apart, 3 px minimum, rounded ends.
    pub const fn new(color: Rgba) -> Self {
        Self {
            color,
            bar_width: 3.0,
            gap: 2.5,
            min_height: 3.0,
            radius: 2.0,
        }
    }
}

/// Where the bars of a meter go inside `rect`: one rectangle per level (`0.0..=1.0`, non-finite
/// reads as 0), the group centred horizontally (left-aligned if it is wider than `rect`) and
/// every bar centred vertically. Pure; the same layout [`PaintCtx::bars`] draws.
pub fn bar_rects(levels: &[f32], rect: Rect, style: &BarStyle) -> Vec<Rect> {
    let n = levels.len();
    if n == 0 || !rect.finite() {
        return Vec::new();
    }
    let (bar_w, gap) = (f64::from(style.bar_width), f64::from(style.gap));
    let total = n as f64 * bar_w + (n - 1) as f64 * gap;
    let x0 = if total >= rect.width {
        rect.x
    } else {
        rect.x + (rect.width - total) / 2.0
    };
    let min_h = f64::from(style.min_height).max(0.0);
    let max_h = rect.height.max(min_h);
    levels
        .iter()
        .enumerate()
        .map(|(i, &level)| {
            let level = if level.is_finite() {
                f64::from(level.clamp(0.0, 1.0))
            } else {
                0.0
            };
            let height = min_h + (max_h - min_h) * level;
            Rect {
                x: x0 + i as f64 * (bar_w + gap),
                y: rect.y + (rect.height - height) / 2.0,
                width: bar_w,
                height,
            }
        })
        .collect()
}

/// The hairline rim of a glass layer, drawn just inside the body edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassBorder {
    /// Rim colour. Its alpha is the rim opacity at the top edge; `0` draws no rim.
    pub color: Rgba,
    /// Rim thickness in logical pixels (at most 16 device pixels).
    pub width: f32,
    /// Share of the opacity left at the bottom edge, `0.0..=1.0`: the rim is brightest on top and
    /// fades linearly down to this fraction. `1.0` is an even rim.
    pub bottom_fade: f32,
}

impl GlassBorder {
    /// The built-in look: a white hairline, 32% opaque on top fading to a third of that below.
    pub const DEFAULT: GlassBorder = GlassBorder {
        color: Rgba::new(255, 255, 255, 82),
        width: 1.0,
        bottom_fade: 0.3125,
    };

    /// No rim.
    pub const NONE: GlassBorder = GlassBorder {
        color: Rgba::TRANSPARENT,
        width: 0.0,
        bottom_fade: 1.0,
    };
}

/// The sheen on the upper part of a glass layer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassHighlight {
    /// Sheen colour. Its alpha is the opacity at the top edge, falling off quadratically; `0`
    /// draws no sheen.
    pub color: Rgba,
    /// How far down the sheen reaches, as a fraction of the body height (`0.0..=1.0`). `0.0` draws
    /// no sheen.
    pub extent: f32,
}

impl GlassHighlight {
    /// The built-in look: white, 22% opaque at the top edge, fading out over the top 60%.
    pub const DEFAULT: GlassHighlight = GlassHighlight {
        color: Rgba::new(255, 255, 255, 56),
        extent: 0.6,
    };

    /// No sheen.
    pub const NONE: GlassHighlight = GlassHighlight {
        color: Rgba::TRANSPARENT,
        extent: 0.0,
    };
}

/// The look of a glass layer drawn by [`PaintCtx::glass_layer_styled`]: shadow, body, sheen, rim.
///
/// [`PaintCtx::glass_layer`] is `GlassStyle::new(blur_px, tint)`, the look the crate shipped with.
/// A host that wants another material (a flatter notch, a coloured rim, no shadow) builds the
/// style it needs; every field takes part in the cache key, so each distinct style is its own
/// cached layer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassStyle {
    /// Shadow falloff in logical pixels. `0` draws no shadow. Clamped to 64 device pixels.
    pub blur_px: f32,
    /// Body fill; its alpha sets how see-through the glass is.
    pub tint: Rgba,
    pub border: GlassBorder,
    pub highlight: GlassHighlight,
}

impl GlassStyle {
    /// A style with the default rim and sheen.
    pub const fn new(blur_px: f32, tint: Rgba) -> Self {
        Self {
            blur_px,
            tint,
            border: GlassBorder::DEFAULT,
            highlight: GlassHighlight::DEFAULT,
        }
    }

    pub const fn with_border(self, border: GlassBorder) -> Self {
        Self { border, ..self }
    }

    pub const fn with_highlight(self, highlight: GlassHighlight) -> Self {
        Self { highlight, ..self }
    }
}

/// An integer pixel rectangle, half-open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IRect {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl IRect {
    fn intersect(self, other: IRect) -> IRect {
        IRect {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        }
    }

    fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FillRule {
    NonZero,
    EvenOdd,
}

fn inside(winding: i32, rule: FillRule) -> bool {
    match rule {
        FillRule::NonZero => winding != 0,
        FillRule::EvenOdd => winding & 1 != 0,
    }
}

/// Everything a glass layer is built from, quantised to device pixels so equal looks compare
/// equal: this is the cache key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GlassKey {
    /// Body size in device pixels.
    w: i32,
    h: i32,
    /// Corner radius, blur and rim width in quarter device pixels.
    radius_q: i32,
    blur_q: i32,
    border_width_q: i32,
    tint: Rgba,
    border: Rgba,
    /// Rim fade and sheen extent in thousandths.
    border_fade_m: i32,
    highlight: Rgba,
    highlight_extent_m: i32,
}

/// Where a glass layer's pieces go: its (possibly reduced) blur, the transparent margin around the
/// body that holds the shadow, and the bitmap size.
#[derive(Clone, Copy, Debug, PartialEq)]
struct GlassLayout {
    blur: f32,
    margin: i32,
    width: usize,
    height: usize,
}

fn glass_margin(blur: f32) -> i32 {
    let drop_px = (blur * 0.35).round() as i32;
    (blur * 2.0).ceil() as i32 + drop_px + 2
}

/// Sizes the bitmap for a glass body of `w` x `h` device pixels with a shadow of `blur` device
/// pixels, keeping every dimension and the build memory inside the caps. The blur is clamped to
/// [`GLASS_MAX_BLUR_PX`]; when body plus shadow margin would still be too large it is halved until
/// the layer fits, and `None` is returned when even a shadowless body does not. Arithmetic is done
/// in 64 bits, so no input overflows.
fn glass_layout(w: i32, h: i32, blur: f32) -> Option<GlassLayout> {
    if w <= 0 || h <= 0 {
        return None;
    }
    let mut blur = if blur.is_finite() {
        blur.clamp(0.0, GLASS_MAX_BLUR_PX)
    } else {
        0.0
    };
    loop {
        let margin = glass_margin(blur);
        let width = i64::from(w) + 2 * i64::from(margin);
        let height = i64::from(h) + 2 * i64::from(margin);
        let fits = width <= GLASS_MAX_SIDE as i64
            && height <= GLASS_MAX_SIDE as i64
            && (width * height) as u128 * GLASS_BYTES_PER_PIXEL as u128
                <= GLASS_MAX_WORKING_BYTES as u128;
        if fits {
            return Some(GlassLayout {
                blur,
                margin,
                width: width as usize,
                height: height as usize,
            });
        }
        if blur <= 0.0 {
            return None;
        }
        blur = if blur > 1.0 {
            (blur / 2.0).floor()
        } else {
            0.0
        };
    }
}

struct GlassEntry {
    key: GlassKey,
    width: usize,
    height: usize,
    /// Transparent border around the body that holds the soft shadow.
    margin: i32,
    /// Premultiplied pixels.
    pixels: Vec<u32>,
    stamp: u64,
}

struct MaskEntry {
    text: String,
    family: &'static str,
    px: i32,
    weight: u16,
    italic: bool,
    mask: Mask,
}

/// The canvas a [`Painter`](crate::Painter) draws into for one frame. See the module docs for the
/// coordinate rules.
pub struct PaintCtx {
    width: usize,
    height: usize,
    scale: f32,
    pixels: Vec<u32>,
    clip: IRect,
    glass: Vec<GlassEntry>,
    glass_stamp: u64,
    glass_builds: u32,
    masks: Vec<MaskEntry>,
    text: TextEngine,
}

/// The font cell height in device pixels for a logical `size`: at least 1, at most
/// [`MAX_FONT_PX`], and 1 for a non-finite request.
fn font_px(size: f32, scale: f32) -> i32 {
    let px = (size * scale).round();
    if px.is_finite() {
        px.clamp(1.0, MAX_FONT_PX) as i32
    } else {
        1
    }
}

/// Signed distance from a point to a rounded rectangle (negative inside).
fn sdf_rounded_rect(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = (px - cx).abs() - hw + r;
    let qy = (py - cy).abs() - hh + r;
    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r
}

impl Clone for PaintCtx {
    /// A copy of the canvas: the pixels, scale and clip. The text and glass caches start empty in the
    /// copy and rebuild on first use, so a clone of a painted canvas costs one buffer copy.
    fn clone(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            scale: self.scale,
            pixels: self.pixels.clone(),
            clip: self.clip,
            glass: Vec::new(),
            glass_stamp: 0,
            glass_builds: 0,
            masks: Vec::new(),
            text: TextEngine::new(),
        }
    }
}

impl PaintCtx {
    /// A free-standing canvas of `width_px` x `height_px` device pixels at `dpi`. The overlay
    /// window builds its own; this exists so a painter can be unit-tested or rendered to a
    /// preview without a window.
    pub fn offscreen(width_px: u32, height_px: u32, dpi: u32) -> Self {
        Self::with_scale(width_px, height_px, scale_for_dpi(dpi))
    }

    pub(crate) fn with_scale(width_px: u32, height_px: u32, scale: f32) -> Self {
        let width = width_px.clamp(1, MAX_SURFACE_SIDE as u32) as usize;
        let height = height_px.clamp(1, MAX_SURFACE_SIDE as u32) as usize;
        Self {
            width,
            height,
            scale: if scale.is_finite() && scale > 0.0 {
                scale
            } else {
                1.0
            },
            pixels: vec![0; width * height],
            clip: IRect {
                x0: 0,
                y0: 0,
                x1: width as i32,
                y1: height as i32,
            },
            glass: Vec::new(),
            glass_stamp: 0,
            glass_builds: 0,
            masks: Vec::new(),
            text: TextEngine::new(),
        }
    }

    /// Changes the surface size in place, keeping the glass and text caches (both are keyed in
    /// device pixels, so they stay valid) and reusing the pixel allocation. The surface comes back
    /// transparent with the clip reset. A no-op when the size is unchanged. The window calls this
    /// when a morph animates its size, instead of building a new canvas every frame.
    pub(crate) fn resize(&mut self, width_px: u32, height_px: u32) {
        let width = width_px.clamp(1, MAX_SURFACE_SIDE as u32) as usize;
        let height = height_px.clamp(1, MAX_SURFACE_SIDE as u32) as usize;
        if (width, height) == (self.width, self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        let len = width * height;
        self.pixels.clear();
        self.pixels.resize(len, 0);
        // Give memory back after a large canvas shrinks a lot.
        if self.pixels.capacity() > len.saturating_mul(4).saturating_add(4096) {
            self.pixels.shrink_to(len.saturating_mul(2));
        }
        self.clip = self.full();
    }

    /// Device pixels per logical pixel.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Surface size in device pixels.
    pub fn pixel_size(&self) -> (u32, u32) {
        (self.width as u32, self.height as u32)
    }

    /// Surface size in logical pixels: the area painter coordinates address.
    pub fn logical_size(&self) -> Size {
        Size {
            width: self.width as f32 / self.scale,
            height: self.height as f32 / self.scale,
        }
    }

    /// The premultiplied `0xAARRGGBB` pixels, row-major, top row first.
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// The canvas over an existing premultiplied BGRA buffer of `width_px` x `height_px` device
    /// pixels at `dpi` (the buffer's own scale). `None` when `pixels` does not hold exactly the
    /// clamped surface, so a canvas never reads past its buffer. The buffer is taken, not copied.
    pub fn from_pixels(width_px: u32, height_px: u32, dpi: u32, pixels: Vec<u32>) -> Option<Self> {
        let mut ctx = Self::offscreen(width_px, height_px, dpi);
        (pixels.len() == ctx.pixels.len()).then(|| {
            ctx.pixels = pixels;
            ctx
        })
    }

    /// The canvas pixels, writable, for a host that copies a frame in or out of its own buffer. The
    /// length is `width * height` and never changes.
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.pixels
    }

    /// One pixel, `0` outside the surface.
    pub fn pixel(&self, x: u32, y: u32) -> u32 {
        if (x as usize) < self.width && (y as usize) < self.height {
            self.pixels[y as usize * self.width + x as usize]
        } else {
            0
        }
    }

    /// Makes every pixel fully transparent and resets the clip. The overlay clears before each
    /// frame, so a painter only needs this to start over mid-frame.
    pub fn clear(&mut self) {
        self.pixels.fill(0);
        self.clear_clip();
    }

    /// Limits all later drawing to `rect` (logical pixels). It replaces any earlier clip; undo it
    /// with [`PaintCtx::clear_clip`].
    pub fn clip(&mut self, rect: Rect) {
        let Some((x, y, w, h)) = self.dev(rect) else {
            return;
        };
        let wanted = IRect {
            x0: x.floor() as i32,
            y0: y.floor() as i32,
            x1: (x + w).ceil() as i32,
            y1: (y + h).ceil() as i32,
        };
        self.clip = wanted.intersect(self.full());
    }

    /// Removes the clip set by [`PaintCtx::clip`].
    pub fn clear_clip(&mut self) {
        self.clip = self.full();
    }

    /// Forgets cached glass layers so the next `glass_layer` call redraws. The window does this
    /// itself on a DPI change (it builds a new canvas); call it when something the cache cannot
    /// see changed.
    pub fn invalidate_glass(&mut self) {
        self.glass.clear();
    }

    /// How many glass layers have been drawn from scratch (cache misses) on this canvas.
    pub fn glass_builds(&self) -> u32 {
        self.glass_builds
    }

    fn full(&self) -> IRect {
        IRect {
            x0: 0,
            y0: 0,
            x1: self.width as i32,
            y1: self.height as i32,
        }
    }

    /// A logical rectangle as device `(x, y, w, h)`, or `None` if it is not finite (in logical
    /// pixels, or once scaled to device pixels).
    fn dev(&self, rect: Rect) -> Option<(f32, f32, f32, f32)> {
        if !rect.finite() {
            return None;
        }
        let s = f64::from(self.scale);
        let device = (
            (rect.x * s) as f32,
            (rect.y * s) as f32,
            (rect.width * s) as f32,
            (rect.height * s) as f32,
        );
        [device.0, device.1, device.2, device.3]
            .iter()
            .all(|v| v.is_finite())
            .then_some(device)
    }

    /// Whole pixels covering a float box, cut to the clip; `None` when nothing is left.
    fn bounds(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> Option<IRect> {
        let wanted = IRect {
            x0: x0.floor() as i32,
            y0: y0.floor() as i32,
            x1: x1.ceil() as i32,
            y1: y1.ceil() as i32,
        }
        .intersect(self.clip);
        if wanted.is_empty() {
            None
        } else {
            Some(wanted)
        }
    }

    /// Blends `color` into every pixel of `b` at the coverage `cover(x, y)` returns for the pixel
    /// centre.
    fn paint_coverage<F: FnMut(f32, f32) -> f32>(&mut self, b: IRect, color: Rgba, mut cover: F) {
        for y in b.y0..b.y1 {
            let row = y as usize * self.width;
            for x in b.x0..b.x1 {
                let c = cover(x as f32 + 0.5, y as f32 + 0.5);
                if c > 0.0 {
                    blend(&mut self.pixels[row + x as usize], color, c);
                }
            }
        }
    }

    // ---- shapes ----------------------------------------------------------------------------

    /// Anti-aliased rounded rectangle. `radius` is clamped to half the shorter side, so a large
    /// radius makes a capsule.
    pub fn fill_rounded_rect(&mut self, rect: Rect, radius: f32, color: Rgba) {
        self.fill_rounded_rect_corners(rect, [radius; 4], color);
    }

    /// Rounded rectangle with a radius per corner: top-left, top-right, bottom-right,
    /// bottom-left.
    pub fn fill_rounded_rect_corners(&mut self, rect: Rect, radii: [f32; 4], color: Rgba) {
        let Some((x, y, w, h)) = self.dev(rect) else {
            return;
        };
        if w <= 0.0 || h <= 0.0 || color.a == 0 {
            return;
        }
        let s = self.scale;
        let radii = radii.map(|r| (r * s).max(0.0));
        let Some(b) = self.bounds(x, y, x + w, y + h) else {
            return;
        };
        let (cx, cy, hw, hh) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
        self.paint_coverage(b, color, move |fx, fy| {
            let r = match (fx < cx, fy < cy) {
                (true, true) => radii[0],
                (false, true) => radii[1],
                (false, false) => radii[2],
                (true, false) => radii[3],
            }
            .min(hw)
            .min(hh);
            (0.5 - sdf_rounded_rect(fx, fy, cx, cy, hw, hh, r)).clamp(0.0, 1.0)
        });
    }

    /// A border `width` thick drawn just inside the edge of the rounded rectangle (the inner edge
    /// is the same shape inset by `width`), blended over what is already there.
    pub fn stroke_rounded_rect(&mut self, rect: Rect, radius: f32, width: f32, color: Rgba) {
        let Some((x, y, w, h)) = self.dev(rect) else {
            return;
        };
        let stroke = width * self.scale;
        if w <= 0.0 || h <= 0.0 || stroke <= 0.0 || color.a == 0 {
            return;
        }
        let (hw, hh) = (w / 2.0, h / 2.0);
        let r = (radius * self.scale).max(0.0).min(hw).min(hh);
        let Some(b) = self.bounds(x, y, x + w, y + h) else {
            return;
        };
        let (cx, cy) = (x + hw, y + hh);
        self.paint_coverage(b, color, move |fx, fy| {
            let d = sdf_rounded_rect(fx, fy, cx, cy, hw, hh, r);
            let outer = (0.5 - d).clamp(0.0, 1.0);
            let inner = (0.5 - (d + stroke)).clamp(0.0, 1.0);
            (outer - inner).max(0.0)
        });
    }

    /// A rectangle with continuous-curvature corners (the "squircle" of an app icon and SwiftUI's
    /// `.continuous` style): the corner leaves each side 1.53 radii from the vertex and eases into
    /// the turn, softer than the circular corner of [`PaintCtx::fill_rounded_rect`]. `radius` is
    /// in logical pixels and capped so a corner takes at most half the shorter side.
    pub fn fill_squircle(&mut self, rect: Rect, radius: f32, color: Rgba) {
        let Some((x, y, w, h)) = self.dev(rect) else {
            return;
        };
        if w <= 0.0 || h <= 0.0 || color.a == 0 {
            return;
        }
        let Some(b) = self.bounds(x, y, x + w, y + h) else {
            return;
        };
        let shape = Squircle::new((x, y, w, h), radius * self.scale);
        self.paint_coverage(b, color, move |fx, fy| shape.coverage(fx as i32, fy as i32));
    }

    /// The border of a squircle: a ring `width` thick just inside its edge (its inner edge is the
    /// same shape inset by `width`), blended over what is already there.
    pub fn stroke_squircle(&mut self, rect: Rect, radius: f32, width: f32, color: Rgba) {
        let Some((x, y, w, h)) = self.dev(rect) else {
            return;
        };
        let stroke = width * self.scale;
        if w <= 0.0 || h <= 0.0 || !stroke.is_finite() || stroke <= 0.0 || color.a == 0 {
            return;
        }
        let Some(b) = self.bounds(x, y, x + w, y + h) else {
            return;
        };
        let radius = radius * self.scale;
        let outer = Squircle::new((x, y, w, h), radius);
        let inner = Squircle::new(
            (x + stroke, y + stroke, w - 2.0 * stroke, h - 2.0 * stroke),
            radius - stroke,
        );
        self.paint_coverage(b, color, move |fx, fy| {
            let (px, py) = (fx as i32, fy as i32);
            (outer.coverage(px, py) - inner.coverage(px, py)).max(0.0)
        });
    }

    /// Draws a straight-alpha RGBA8 image (`rgba` is `width * height * 4` bytes, row-major, top
    /// row first, the layout image decoders produce) scaled bilinearly into `dest` (logical
    /// pixels), source-over what is already there. Nothing is drawn, and nothing panics, when the
    /// image is empty or `rgba` is shorter than the size says, when `dest` is empty or not finite,
    /// or when `dest` lies outside the surface or clip; pixels outside `dest` are never touched.
    pub fn blit_rgba(&mut self, rgba: &[u8], width: u32, height: u32, dest: Rect) {
        let (width, height) = (width as usize, height as usize);
        let Some(len) = width.checked_mul(height).and_then(|n| n.checked_mul(4)) else {
            return;
        };
        if width == 0 || height == 0 || rgba.len() < len {
            return;
        }
        self.blit_with((width, height), dest, |column, row| {
            let at = (row * width + column) * 4;
            let a = f32::from(rgba[at + 3]);
            // Premultiply on the fly: the canvas composites premultiplied pixels.
            let k = a / 255.0;
            [
                a,
                f32::from(rgba[at]) * k,
                f32::from(rgba[at + 1]) * k,
                f32::from(rgba[at + 2]) * k,
            ]
        });
    }

    /// Like [`PaintCtx::blit_rgba`] for a premultiplied `0xAARRGGBB` image (`width * height`
    /// pixels, the format of [`PaintCtx::pixels`] and of GDI DIB sections): bilinear, source-over.
    pub fn blit_premultiplied(&mut self, pixels: &[u32], width: u32, height: u32, dest: Rect) {
        let (width, height) = (width as usize, height as usize);
        let Some(len) = width.checked_mul(height) else {
            return;
        };
        if width == 0 || height == 0 || pixels.len() < len {
            return;
        }
        self.blit_with((width, height), dest, |column, row| {
            let px = pixels[row * width + column];
            [
                ((px >> 24) & 0xFF) as f32,
                ((px >> 16) & 0xFF) as f32,
                ((px >> 8) & 0xFF) as f32,
                (px & 0xFF) as f32,
            ]
        });
    }

    /// The bilinear scaler behind the blits. `sample(column, row)` returns the premultiplied
    /// `[a, r, g, b]` (0..=255) of a source pixel; both indices are always inside the image.
    fn blit_with<F: Fn(usize, usize) -> [f32; 4]>(
        &mut self,
        (sw, sh): (usize, usize),
        dest: Rect,
        sample: F,
    ) {
        let Some((x, y, w, h)) = self.dev(dest) else {
            return;
        };
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let Some(b) = self.bounds(x, y, x + w, y + h) else {
            return;
        };
        let (max_u, max_v) = ((sw - 1) as f32, (sh - 1) as f32);
        for py in b.y0..b.y1 {
            // Share of this pixel row inside `dest` (partly covered edge rows fade).
            let rows = (((py + 1) as f32).min(y + h) - (py as f32).max(y)).clamp(0.0, 1.0);
            let v = ((py as f32 + 0.5 - y) / h * sh as f32 - 0.5).clamp(0.0, max_v);
            let (row0, row1) = (v.floor() as usize, (v.floor() as usize + 1).min(sh - 1));
            let fv = v - row0 as f32;
            for px in b.x0..b.x1 {
                let columns = (((px + 1) as f32).min(x + w) - (px as f32).max(x)).clamp(0.0, 1.0);
                let u = ((px as f32 + 0.5 - x) / w * sw as f32 - 0.5).clamp(0.0, max_u);
                let (col0, col1) = (u.floor() as usize, (u.floor() as usize + 1).min(sw - 1));
                let fu = u - col0 as f32;
                let (p00, p10) = (sample(col0, row0), sample(col1, row0));
                let (p01, p11) = (sample(col0, row1), sample(col1, row1));
                let k = rows * columns;
                let mut out = [0.0f32; 4];
                for (i, slot) in out.iter_mut().enumerate() {
                    let top = p00[i] + (p10[i] - p00[i]) * fu;
                    let bottom = p01[i] + (p11[i] - p01[i]) * fu;
                    *slot = (top + (bottom - top) * fv) * k;
                }
                if out[0] <= 0.0 {
                    continue;
                }
                let index = py as usize * self.width + px as usize;
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

    /// Anti-aliased disc centred on `(cx, cy)` (logical pixels).
    pub fn fill_circle(&mut self, cx: f32, cy: f32, radius: f32, color: Rgba) {
        let (cx, cy, r) = (f64::from(cx), f64::from(cy), f64::from(radius));
        self.fill_rounded_rect(
            Rect {
                x: cx - r,
                y: cy - r,
                width: 2.0 * r,
                height: 2.0 * r,
            },
            radius,
            color,
        );
    }

    /// A ring stroke centred on `radius` with round caps. `start` and `sweep` are in turns
    /// (`1.0` is a full circle): the arc begins `start` turns clockwise from 12 o'clock and runs
    /// `sweep` turns clockwise. A sweep of `1.0` is a closed ring; `0.0` draws nothing. A spinner
    /// is this with `start` advancing with the animation.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_arc(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        width: f32,
        start: f32,
        sweep: f32,
        color: Rgba,
    ) {
        let s = self.scale;
        let (cx, cy, radius, width) = (cx * s, cy * s, radius * s, width * s);
        let sweep = sweep.clamp(0.0, 1.0);
        if sweep <= 0.0 || radius <= 0.0 || width <= 0.0 || color.a == 0 {
            return;
        }
        let half = width / 2.0;
        let closed = sweep >= 0.999;
        let end_angle = sweep * TAU;
        let start_angle = start.rem_euclid(1.0) * TAU;
        let cap_at = |angle: f32| (cx + radius * angle.sin(), cy - radius * angle.cos());
        let (cap_a, cap_b) = (cap_at(start_angle), cap_at(start_angle + end_angle));
        let reach = radius + half + 1.0;
        let Some(b) = self.bounds(cx - reach, cy - reach, cx + reach, cy + reach) else {
            return;
        };
        self.paint_coverage(b, color, move |fx, fy| {
            let (dx, dy) = (fx - cx, fy - cy);
            let radial = (0.5 - ((dx.hypot(dy) - radius).abs() - half)).clamp(0.0, 1.0);
            if radial <= 0.0 {
                return 0.0;
            }
            if closed {
                return radial;
            }
            let theta = (dx.atan2(-dy) - start_angle).rem_euclid(TAU);
            let along_start = theta * radius;
            let along_end = (end_angle - theta) * radius;
            let body = (0.5 + along_start.min(along_end)).clamp(0.0, 1.0) * radial;
            let cap = |c: (f32, f32)| (0.5 - ((fx - c.0).hypot(fy - c.1) - half)).clamp(0.0, 1.0);
            body.max(cap(cap_a)).max(cap(cap_b))
        });
    }

    // ---- paths -----------------------------------------------------------------------------

    /// Fills a path (logical pixels) with the non-zero winding rule: nested shapes drawn in the
    /// opposite direction cut holes, in the same direction fill solid.
    pub fn fill_path(&mut self, cmds: &[PathCmd], color: Rgba) {
        let subs = path::flatten(cmds, self.scale);
        self.fill_polygons(&subs, FillRule::NonZero, color);
    }

    /// Fills a path with the even-odd rule: every nested shape toggles fill, whatever its
    /// direction.
    pub fn fill_path_even_odd(&mut self, cmds: &[PathCmd], color: Rgba) {
        let subs = path::flatten(cmds, self.scale);
        self.fill_polygons(&subs, FillRule::EvenOdd, color);
    }

    /// Strokes a path with round joins and caps; `width` is in logical pixels. This is how
    /// line-style icons (the old pill glyph set) are drawn.
    pub fn stroke_path(&mut self, cmds: &[PathCmd], width: f32, color: Rgba) {
        let subs = path::flatten(cmds, self.scale);
        self.stroke_subpaths(&subs, width * self.scale, color);
    }

    fn fill_polygons(&mut self, polys: &[Subpath], rule: FillRule, color: Rgba) {
        const SUB: usize = 8;
        if color.a == 0 {
            return;
        }
        let (mut top, mut bottom, mut left, mut right) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for &(x, y) in polys.iter().flat_map(|p| p.points.iter()) {
            top = top.min(y);
            bottom = bottom.max(y);
            left = left.min(x);
            right = right.max(x);
        }
        if top > bottom {
            return;
        }
        let Some(b) = self.bounds(left, top, right, bottom) else {
            return;
        };
        let (clip_l, clip_r) = (b.x0 as f32, b.x1 as f32);
        let mut cover = vec![0.0f32; (b.x1 - b.x0) as usize];
        let mut crossings: Vec<(f32, i32)> = Vec::new();
        for row in b.y0..b.y1 {
            cover.fill(0.0);
            let mut any = false;
            for sub in 0..SUB {
                let y = row as f32 + (sub as f32 + 0.5) / SUB as f32;
                crossings.clear();
                for poly in polys {
                    let pts = &poly.points;
                    let n = pts.len();
                    if n < 2 {
                        continue;
                    }
                    for i in 0..n {
                        let a = pts[i];
                        let c = pts[(i + 1) % n];
                        if (a.1 <= y) != (c.1 <= y) {
                            let x = a.0 + (y - a.1) * (c.0 - a.0) / (c.1 - a.1);
                            crossings.push((x, if c.1 > a.1 { 1 } else { -1 }));
                        }
                    }
                }
                if crossings.is_empty() {
                    continue;
                }
                crossings.sort_by(|p, q| p.0.total_cmp(&q.0));
                let mut winding = 0i32;
                let mut span_start = 0.0f32;
                for &(x, dir) in &crossings {
                    let was_inside = inside(winding, rule);
                    winding += dir;
                    let now_inside = inside(winding, rule);
                    if !was_inside && now_inside {
                        span_start = x;
                    } else if was_inside && !now_inside {
                        let l = span_start.max(clip_l);
                        let r = x.min(clip_r);
                        if r > l {
                            any = true;
                            for col in (l.floor() as i32)..(r.ceil() as i32) {
                                let lo = l.max(col as f32);
                                let hi = r.min(col as f32 + 1.0);
                                if hi > lo {
                                    cover[(col - b.x0) as usize] += (hi - lo) / SUB as f32;
                                }
                            }
                        }
                    }
                }
            }
            if any {
                let base = row as usize * self.width + b.x0 as usize;
                for (i, &c) in cover.iter().enumerate() {
                    if c > 0.0 {
                        blend(&mut self.pixels[base + i], color, c);
                    }
                }
            }
        }
    }

    fn stroke_subpaths(&mut self, subs: &[Subpath], width: f32, color: Rgba) {
        if width <= 0.0 || color.a == 0 {
            return;
        }
        let half = width / 2.0;
        let reach = half + 1.0;
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in subs.iter().flat_map(|s| s.points.iter()) {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
        if min_x > max_x {
            return;
        }
        let Some(b) = self.bounds(min_x - reach, min_y - reach, max_x + reach, max_y + reach)
        else {
            return;
        };
        let bw = (b.x1 - b.x0) as usize;
        let bh = (b.y1 - b.y0) as usize;
        let mut cover = vec![0.0f32; bw * bh];
        for sub in subs {
            let pts = &sub.points;
            match pts.len() {
                0 => {}
                1 => stroke_segment(&mut cover, bw, b, pts[0], pts[0], half),
                n => {
                    let segments = if sub.closed && n > 2 { n } else { n - 1 };
                    for i in 0..segments {
                        stroke_segment(&mut cover, bw, b, pts[i], pts[(i + 1) % n], half);
                    }
                }
            }
        }
        for y in 0..bh {
            let base = (b.y0 as usize + y) * self.width + b.x0 as usize;
            for x in 0..bw {
                let c = cover[y * bw + x];
                if c > 0.0 {
                    blend(&mut self.pixels[base + x], color, c);
                }
            }
        }
    }

    // ---- text and meters -------------------------------------------------------------------

    /// Draws `text` inside `rect`, aligned horizontally by `align` and centred vertically. Text
    /// that does not fit is clipped at the rectangle (there is no ellipsis). Rendered strings are
    /// cached, so a label that repeats across animation frames is rasterised once.
    pub fn text(&mut self, text: &str, font: Font, color: Rgba, rect: Rect, align: Align) {
        if text.is_empty() || color.a == 0 {
            return;
        }
        let Some((rx, ry, rw, rh)) = self.dev(rect) else {
            return;
        };
        let Some(index) = self.mask_index(text, font) else {
            return;
        };
        let mask = &self.masks[index].mask;
        let (mw, mh) = (mask.width as f32, mask.height as f32);
        let x = match align {
            Align::Left => rx,
            Align::Center => rx + (rw - mw) / 2.0,
            Align::Right => rx + rw - mw,
        }
        .round() as i32;
        let y = (ry + (rh - mh) / 2.0).round() as i32;
        let area = IRect {
            x0: rx.floor() as i32,
            y0: ry.floor() as i32,
            x1: (rx + rw).ceil() as i32,
            y1: (ry + rh).ceil() as i32,
        }
        .intersect(self.clip);
        let width = self.width;
        for row in 0..mask.height {
            let py = y + row as i32;
            if py < area.y0 || py >= area.y1 {
                continue;
            }
            for col in 0..mask.width {
                let px = x + col as i32;
                if px < area.x0 || px >= area.x1 {
                    continue;
                }
                let level = mask.coverage[row * mask.width + col];
                if level != 0 {
                    blend(
                        &mut self.pixels[py as usize * width + px as usize],
                        color,
                        f32::from(level) / 255.0,
                    );
                }
            }
        }
    }

    /// Width and height of `text` in logical pixels, for layout (sizing a pill to its timer text).
    pub fn measure_text(&mut self, text: &str, font: Font) -> Size {
        let px = font_px(font.size, self.scale);
        let (w, h) = self
            .text
            .measure(text, font.family, px as f32, font.weight, font.italic);
        Size {
            width: w / self.scale,
            height: h / self.scale,
        }
    }

    fn mask_index(&mut self, text: &str, font: Font) -> Option<usize> {
        let px = font_px(font.size, self.scale);
        let found = self.masks.iter().position(|e| {
            e.px == px
                && e.weight == font.weight
                && e.italic == font.italic
                && e.family == font.family
                && e.text == text
        });
        if let Some(i) = found {
            // Most recently used goes last.
            let entry = self.masks.remove(i);
            self.masks.push(entry);
            return Some(self.masks.len() - 1);
        }
        let mask = self
            .text
            .render(text, font.family, px as f32, font.weight, font.italic)?;
        if self.masks.len() >= MASK_CACHE_MAX {
            self.masks.remove(0);
        }
        self.masks.push(MaskEntry {
            text: text.to_owned(),
            family: font.family,
            px,
            weight: font.weight,
            italic: font.italic,
            mask,
        });
        Some(self.masks.len() - 1)
    }

    /// A row of rounded bars, one per level in `levels` (`0.0..=1.0`), centred in `rect`: a voice
    /// or level meter. See [`bar_rects`] for the exact layout.
    pub fn bars(&mut self, levels: &[f32], rect: Rect, style: BarStyle) {
        for bar in bar_rects(levels, rect, &style) {
            let radius = style
                .radius
                .min(style.bar_width / 2.0)
                .min(bar.height as f32 / 2.0);
            self.fill_rounded_rect(bar, radius, style.color);
        }
    }

    // ---- glass -----------------------------------------------------------------------------

    /// A frosted-glass capsule or card with the default look: a soft drop shadow (`blur_px` of
    /// falloff), a body filled with `tint` (its alpha sets how see-through the glass is), a top
    /// sheen and a hairline rim. Same as [`PaintCtx::glass_layer_styled`] with
    /// `GlassStyle::new(blur_px, tint)`.
    pub fn glass_layer(&mut self, rect: Rect, radius: f32, blur_px: f32, tint: Rgba) {
        self.glass_layer_styled(rect, radius, GlassStyle::new(blur_px, tint));
    }

    /// A glass layer drawn with an explicit [`GlassStyle`] (blur, tint, rim, sheen).
    ///
    /// The layer is drawn once per distinct `(size, radius, style)` at the current scale and then
    /// copied, so redrawing it every animation frame is cheap. It stays cached until its
    /// parameters change, [`PaintCtx::invalidate_glass`] is called, or the window changes DPI. A
    /// morph that resizes the glass every frame rebuilds it every frame; keep the glass at its
    /// final size and animate what is drawn on it when that cost matters.
    ///
    /// Memory is bounded whatever the input: the blur is clamped to 64 device pixels, a layer
    /// (body plus shadow margin) is at most 4096 pixels on a side and 2,000,000 pixels of area
    /// (32 MB of build memory), and when a large body will not fit with its full shadow the
    /// shadow is narrowed until it does.
    /// A body too large even without a shadow is not drawn. Non-finite input draws nothing.
    pub fn glass_layer_styled(&mut self, rect: Rect, radius: f32, style: GlassStyle) {
        let Some((x, y, w, h)) = self.dev(rect) else {
            return;
        };
        let (wi, hi) = (w.round() as i32, h.round() as i32);
        if wi <= 0 || hi <= 0 || wi > MAX_SURFACE_SIDE || hi > MAX_SURFACE_SIDE {
            return;
        }
        let scale = self.scale;
        // Device-pixel quantities, each clamped before anything is sized from it.
        let quarter = |v: f32, max: f32| (v.max(0.0).min(max) * 4.0).round() as i32;
        let thousandth = |v: f32| (v.clamp(0.0, 1.0) * 1000.0).round() as i32;
        let key = GlassKey {
            w: wi,
            h: hi,
            radius_q: quarter(radius * scale, MAX_SURFACE_SIDE as f32),
            blur_q: quarter(style.blur_px * scale, GLASS_MAX_BLUR_PX),
            border_width_q: quarter(style.border.width * scale, 16.0),
            tint: style.tint,
            border: style.border.color,
            border_fade_m: thousandth(style.border.bottom_fade),
            highlight: style.highlight.color,
            highlight_extent_m: thousandth(style.highlight.extent),
        };
        self.glass_stamp += 1;
        let (left, top) = (x.round() as i32, y.round() as i32);
        let index = match self.glass.iter().position(|e| e.key == key) {
            Some(i) => {
                self.glass[i].stamp = self.glass_stamp;
                i
            }
            None => {
                let Some(layout) = glass_layout(key.w, key.h, key.blur_q as f32 / 4.0) else {
                    return;
                };
                let mut entry = build_glass(key, layout);
                entry.stamp = self.glass_stamp;
                self.glass_builds += 1;
                if entry.pixels.len() > GLASS_CACHE_MAX_PIXELS {
                    blit_glass(&mut self.pixels, self.width, self.clip, &entry, left, top);
                    return;
                }
                if self.glass.len() >= GLASS_CACHE_MAX {
                    if let Some(oldest) = self
                        .glass
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, e)| e.stamp)
                        .map(|(i, _)| i)
                    {
                        self.glass.swap_remove(oldest);
                    }
                }
                self.glass.push(entry);
                self.glass.len() - 1
            }
        };
        let entry = &self.glass[index];
        blit_glass(&mut self.pixels, self.width, self.clip, entry, left, top);
    }
}

/// Distance-field coverage of one round-capped segment, maximised into `cover` (so overlapping
/// segments of one stroke never double-blend).
fn stroke_segment(cover: &mut [f32], bw: usize, b: IRect, a: (f32, f32), c: (f32, f32), half: f32) {
    let reach = half + 1.0;
    let x0 = ((a.0.min(c.0) - reach).floor() as i32).max(b.x0);
    let x1 = ((a.0.max(c.0) + reach).ceil() as i32).min(b.x1);
    let y0 = ((a.1.min(c.1) - reach).floor() as i32).max(b.y0);
    let y1 = ((a.1.max(c.1) + reach).ceil() as i32).min(b.y1);
    let (dx, dy) = (c.0 - a.0, c.1 - a.1);
    let length_sq = dx * dx + dy * dy;
    for py in y0..y1 {
        for px in x0..x1 {
            let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
            let t = if length_sq > 0.0 {
                (((fx - a.0) * dx + (fy - a.1) * dy) / length_sq).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let distance = (fx - (a.0 + t * dx)).hypot(fy - (a.1 + t * dy));
            let coverage = (0.5 + half - distance).clamp(0.0, 1.0);
            let index = (py - b.y0) as usize * bw + (px - b.x0) as usize;
            if coverage > cover[index] {
                cover[index] = coverage;
            }
        }
    }
}

/// Source-over of a cached glass bitmap, its body top-left at `(left, top)`.
fn blit_glass(
    pixels: &mut [u32],
    width: usize,
    clip: IRect,
    entry: &GlassEntry,
    left: i32,
    top: i32,
) {
    let ox = left - entry.margin;
    let oy = top - entry.margin;
    for row in 0..entry.height {
        let py = oy + row as i32;
        if py < clip.y0 || py >= clip.y1 {
            continue;
        }
        for col in 0..entry.width {
            let px = ox + col as i32;
            if px < clip.x0 || px >= clip.x1 {
                continue;
            }
            let src = entry.pixels[row * entry.width + col];
            if src >> 24 != 0 {
                blend_premul(&mut pixels[py as usize * width + px as usize], src);
            }
        }
    }
}

fn build_glass(key: GlassKey, layout: GlassLayout) -> GlassEntry {
    let radius = key.radius_q as f32 / 4.0;
    let GlassLayout {
        blur,
        margin,
        width: bw,
        height: bh,
    } = layout;
    let drop_px = (blur * 0.35).round() as i32;
    let (hw, hh) = (key.w as f32 / 2.0, key.h as f32 / 2.0);
    let radius = radius.min(hw).min(hh);
    let (cx, cy) = (margin as f32 + hw, margin as f32 + hh);

    let mut body = vec![0.0f32; bw * bh];
    for y in 0..bh {
        for x in 0..bw {
            let d = sdf_rounded_rect(x as f32 + 0.5, y as f32 + 0.5, cx, cy, hw, hh, radius);
            body[y * bw + x] = (0.5 - d).clamp(0.0, 1.0);
        }
    }

    let mut pixels = vec![0u32; bw * bh];

    // Soft shadow, cut away under the body so it never tints the glass itself.
    if blur > 0.0 && key.tint.a > 0 {
        let mut shadow = vec![0.0f32; bw * bh];
        for y in 0..bh {
            for x in 0..bw {
                let d = sdf_rounded_rect(
                    x as f32 + 0.5,
                    y as f32 + 0.5,
                    cx,
                    cy + drop_px as f32,
                    hw,
                    hh,
                    radius,
                );
                shadow[y * bw + x] = (0.5 - d).clamp(0.0, 1.0);
            }
        }
        box_blur(&mut shadow, bw, bh, ((blur / 2.0).round() as usize).max(1));
        for (i, px) in pixels.iter_mut().enumerate() {
            let a = shadow[i] * 0.32 * (1.0 - body[i]);
            if a > 0.0 {
                blend(px, Rgba::BLACK, a);
            }
        }
    }

    let body_h = key.h as f32;
    // Rim and sheen colours are opaque here; their alpha is the strength factor below.
    let border_color = Rgba {
        a: 255,
        ..key.border
    };
    let border_strength = f32::from(key.border.a) / 255.0;
    let border_width = key.border_width_q as f32 / 4.0;
    let border_fade = key.border_fade_m as f32 / 1000.0;
    let draw_border = border_strength > 0.0 && border_width > 0.0;
    let highlight_color = Rgba {
        a: 255,
        ..key.highlight
    };
    let highlight_strength = f32::from(key.highlight.a) / 255.0;
    let highlight_extent = key.highlight_extent_m as f32 / 1000.0;
    let draw_highlight = highlight_strength > 0.0 && highlight_extent > 0.0;
    for y in 0..bh {
        for x in 0..bw {
            let i = y * bw + x;
            let cover = body[i];
            if cover <= 0.0 {
                continue;
            }
            blend(&mut pixels[i], key.tint, cover);
            // 0 at the top edge of the body, 1 at the bottom edge.
            let yn = ((y as f32 + 0.5) - margin as f32) / body_h;
            if draw_highlight {
                let sheen = (1.0 - yn / highlight_extent).max(0.0);
                let sheen = sheen * sheen * highlight_strength;
                if sheen > 0.0 {
                    blend(&mut pixels[i], highlight_color, sheen * cover);
                }
            }
            if draw_border {
                let d = sdf_rounded_rect(x as f32 + 0.5, y as f32 + 0.5, cx, cy, hw, hh, radius);
                let ring = ((0.5 - d).clamp(0.0, 1.0) - (0.5 - (d + border_width)).clamp(0.0, 1.0))
                    .max(0.0);
                if ring > 0.0 {
                    let a = border_strength
                        * (border_fade + (1.0 - border_fade) * (1.0 - yn).clamp(0.0, 1.0));
                    blend(&mut pixels[i], border_color, ring * a);
                }
            }
        }
    }

    GlassEntry {
        key,
        width: bw,
        height: bh,
        margin,
        pixels,
        stamp: 0,
    }
}

/// Three box-blur passes (a close match to a Gaussian) of `radius` pixels, edges clamped.
fn box_blur(buf: &mut [f32], w: usize, h: usize, radius: usize) {
    if radius == 0 || w == 0 || h == 0 {
        return;
    }
    let mut tmp = vec![0.0f32; buf.len()];
    for _ in 0..3 {
        blur_pass(buf, &mut tmp, w, h, radius, true);
        blur_pass(&tmp, buf, w, h, radius, false);
    }
}

fn blur_pass(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize, horizontal: bool) {
    let window = (2 * r + 1) as f32;
    let (outer, inner) = if horizontal { (h, w) } else { (w, h) };
    let (ri, n) = (r as isize, inner as isize);
    for o in 0..outer {
        let at = |i: usize| if horizontal { o * w + i } else { i * w + o };
        let mut sum = 0.0f32;
        for k in -ri..=ri {
            sum += src[at(k.clamp(0, n - 1) as usize)];
        }
        for i in 0..inner {
            dst[at(i)] = sum / window;
            let add = (i as isize + ri + 1).clamp(0, n - 1) as usize;
            let sub = (i as isize - ri).clamp(0, n - 1) as usize;
            sum += src[at(add)] - src[at(sub)];
        }
    }
}
