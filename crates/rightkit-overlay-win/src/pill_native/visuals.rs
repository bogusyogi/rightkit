//! Pure visual model for the native Windows pill, hint and clip overlays.
//!
//! Everything here is plain data and arithmetic so it compiles and tests on every host.
//! The design source of truth is the macOS Swift pill
//! (the native macOS pill sources):
//!
//! * `PillTokens.swift`        -> [`tok`] (colours) and [`Face`] (fonts)
//! * `PillPanel.swift`         -> [`capsule_size`], [`status_capsule_size`], easing
//! * `PillRootView.swift`      -> [`build_pill`], bars, border and background rules
//! * `AuxiliaryViews.swift`    -> `hint.rs` and `clipboard.rs` (they reuse the primitives here)
//! * `PillModel.swift`         -> bar integration, intro bloom, timings
//!
//! Units: every number is a Swift point, which is one Windows logical pixel at 100 % scaling.
//! The painter multiplies by the monitor scale (`dpi / 96`) when it replays a [`DrawOp`] list.
//! Nothing in this file touches `rightkit_overlay_win`; `gfx.rs` is the only translator.

#![allow(dead_code)]

use std::sync::OnceLock;

// ---------------------------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------------------------

/// 8-bit sRGB colour with a floating alpha in `0.0..=1.0` (SwiftUI `Color` + `.opacity`).
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

    /// Multiply the alpha (SwiftUI `.opacity(k)` stacked on an existing colour).
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

/// `PillTokens.swift` `HRTokens` + `HRAuxiliaryPalette` and the literal colours used by the views.
pub mod tok {
    use super::Rgba;

    /// `HRTokens.accent` (90, 160, 255).
    pub const ACCENT: Rgba = Rgba::rgb(0x5AA0FF);
    /// `HRTokens.ember` 0xFF5630 (recording bars, house icon).
    pub const EMBER: Rgba = Rgba::rgb(0xFF5630);
    /// `HRTokens.coffee` (8, 8, 8): pill body.
    pub const COFFEE: Rgba = Rgba::rgb(0x080808);
    /// `HRTokens.foreground` (245, 242, 238): timer text.
    pub const FOREGROUND: Rgba = Rgba::rgb(0xF5F2EE);
    /// `HRTokens.error` (229, 103, 91).
    pub const ERROR: Rgba = Rgba::rgb(0xE5675B);
    /// `HRTokens.success` (63, 208, 106).
    pub const SUCCESS: Rgba = Rgba::rgb(0x3FD06A);
    /// `Color(hex: 0xFF9F0A)`: stop button and the Cancelled status capsule.
    pub const AMBER: Rgba = Rgba::rgb(0xFF9F0A);
    pub const BLACK: Rgba = Rgba::rgb(0x000000);
    pub const WHITE: Rgba = Rgba::rgb(0xFFFFFF);

    /// `HRAuxiliaryPalette.surface` (.black).
    pub const AUX_SURFACE: Rgba = BLACK;
    /// `HRAuxiliaryPalette.raised` 0x171717.
    pub const AUX_RAISED: Rgba = Rgba::rgb(0x171717);
    /// `HRAuxiliaryPalette.foreground` 0xF5F2EE.
    pub const AUX_FOREGROUND: Rgba = Rgba::rgb(0xF5F2EE);
    /// `HRAuxiliaryPalette.muted` 0x999999.
    pub const AUX_MUTED: Rgba = Rgba::rgb(0x999999);
    /// `HRAuxiliaryPalette.line` 0x333333 (used at .72 opacity).
    pub const AUX_LINE: Rgba = Rgba::rgb(0x333333);
    /// `HRAuxiliaryPalette.accent` (.white).
    pub const AUX_ACCENT: Rgba = WHITE;
    /// `HRAuxiliaryPalette.hover` white at .14.
    pub const AUX_HOVER: Rgba = Rgba::new(255, 255, 255, 0.14);
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
// Fonts and text metrics
// ---------------------------------------------------------------------------------------------

/// The five bundled faces the Swift pill uses (`HRTokens.body/display`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    /// `HankenGrotesk-Regular`
    Hanken400,
    /// `HankenGrotesk-Medium` (the `HRTokens.body` default weight)
    Hanken500,
    /// `HankenGrotesk-SemiBold`
    Hanken600,
    /// `HankenGrotesk-Bold`
    Hanken700,
    /// `Tanker-400.ttf` (PostScript/family name is literally `false`)
    Tanker,
}

pub const ALL_FACES: [Face; 5] = [
    Face::Hanken400,
    Face::Hanken500,
    Face::Hanken600,
    Face::Hanken700,
    Face::Tanker,
];

static HANKEN_400: &[u8] = include_bytes!("../../fonts/HankenGrotesk-400.ttf");
static HANKEN_500: &[u8] = include_bytes!("../../fonts/HankenGrotesk-500.ttf");
static HANKEN_600: &[u8] = include_bytes!("../../fonts/HankenGrotesk-600.ttf");
static HANKEN_700: &[u8] = include_bytes!("../../fonts/HankenGrotesk-700.ttf");
static TANKER_400: &[u8] = include_bytes!("../../fonts/Tanker-400.ttf");

impl Face {
    fn index(self) -> usize {
        match self {
            Face::Hanken400 => 0,
            Face::Hanken500 => 1,
            Face::Hanken600 => 2,
            Face::Hanken700 => 3,
            Face::Tanker => 4,
        }
    }

    /// Raw TrueType bytes (also handed to GDI as a private in-process font by `gfx.rs`).
    pub fn bytes(self) -> &'static [u8] {
        match self {
            Face::Hanken400 => HANKEN_400,
            Face::Hanken500 => HANKEN_500,
            Face::Hanken600 => HANKEN_600,
            Face::Hanken700 => HANKEN_700,
            Face::Tanker => TANKER_400,
        }
    }

    /// Legacy (GDI, name-id 1) family name of the face.
    pub fn gdi_family(self) -> &'static str {
        match self {
            Face::Hanken400 | Face::Hanken700 => "Hanken Grotesk",
            Face::Hanken500 => "Hanken Grotesk Medium",
            Face::Hanken600 => "Hanken Grotesk SemiBold",
            Face::Tanker => "false",
        }
    }

    /// GDI weight that selects this face inside [`Face::gdi_family`].
    pub fn gdi_weight(self) -> u16 {
        match self {
            Face::Hanken700 => 700,
            _ => 400,
        }
    }

    /// Typographic (DirectWrite, name-id 16) family and weight, for backends that load the
    /// bundled fonts into a private collection.
    pub fn typographic_family(self) -> &'static str {
        match self {
            Face::Tanker => "false",
            _ => "Hanken Grotesk",
        }
    }

    pub fn typographic_weight(self) -> u16 {
        match self {
            Face::Hanken400 => 400,
            Face::Hanken500 => 500,
            Face::Hanken600 => 600,
            Face::Hanken700 => 700,
            Face::Tanker => 400,
        }
    }

    /// hhea ascender in em.
    pub fn ascent_em(self) -> f32 {
        1.0
    }

    /// hhea descender magnitude in em.
    pub fn descent_em(self) -> f32 {
        match self {
            Face::Tanker => 0.26,
            _ => 0.303,
        }
    }

    /// hhea lineGap in em.
    pub fn gap_em(self) -> f32 {
        match self {
            Face::Tanker => 0.09,
            _ => 0.0,
        }
    }

    /// SwiftUI `Text` line height: ascender + descender + leading.
    pub fn line_height(self, size: f32) -> f32 {
        (self.ascent_em() + self.descent_em() + self.gap_em()) * size
    }

    /// Height of the glyph box a renderer draws into (ascender + descender).
    pub fn glyph_box(self, size: f32) -> f32 {
        (self.ascent_em() + self.descent_em()) * size
    }
}

struct FontTables {
    data: &'static [u8],
    upem: f32,
    num_hmetrics: usize,
    hmtx: usize,
    cmap4: usize,
}

fn be16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*d.get(o)?, *d.get(o + 1)?]))
}

fn be32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *d.get(o)?,
        *d.get(o + 1)?,
        *d.get(o + 2)?,
        *d.get(o + 3)?,
    ]))
}

fn parse_font(data: &'static [u8]) -> Option<FontTables> {
    let num_tables = be16(data, 4)? as usize;
    let mut head = None;
    let mut hhea = None;
    let mut hmtx = None;
    let mut cmap = None;
    for i in 0..num_tables {
        let rec = 12 + 16 * i;
        let tag = data.get(rec..rec + 4)?;
        let off = be32(data, rec + 8)? as usize;
        match tag {
            b"head" => head = Some(off),
            b"hhea" => hhea = Some(off),
            b"hmtx" => hmtx = Some(off),
            b"cmap" => cmap = Some(off),
            _ => {}
        }
    }
    let (head, hhea, hmtx, cmap) = (head?, hhea?, hmtx?, cmap?);
    let upem = f32::from(be16(data, head + 18)?);
    let num_hmetrics = be16(data, hhea + 34)? as usize;
    let count = be16(data, cmap + 2)? as usize;
    let mut chosen = None;
    for i in 0..count {
        let rec = cmap + 4 + 8 * i;
        let platform = be16(data, rec)?;
        let encoding = be16(data, rec + 2)?;
        let off = cmap + be32(data, rec + 4)? as usize;
        if be16(data, off)? != 4 {
            continue;
        }
        // Prefer Windows Unicode BMP; otherwise take any format-4 subtable.
        if platform == 3 && encoding == 1 {
            chosen = Some(off);
            break;
        }
        if chosen.is_none() {
            chosen = Some(off);
        }
    }
    Some(FontTables {
        data,
        upem: upem.max(1.0),
        num_hmetrics: num_hmetrics.max(1),
        hmtx,
        cmap4: chosen?,
    })
}

impl FontTables {
    fn glyph(&self, ch: char) -> u16 {
        let code = ch as u32;
        if code > 0xFFFF {
            return 0;
        }
        let code = code as u16;
        let d = self.data;
        let c4 = self.cmap4;
        let seg_x2 = match be16(d, c4 + 6) {
            Some(v) => v as usize,
            None => return 0,
        };
        let seg = seg_x2 / 2;
        let end_off = c4 + 14;
        let start_off = end_off + seg_x2 + 2;
        let delta_off = start_off + seg_x2;
        let range_off = delta_off + seg_x2;
        for i in 0..seg {
            let end = match be16(d, end_off + 2 * i) {
                Some(v) => v,
                None => return 0,
            };
            if code > end {
                continue;
            }
            let start = be16(d, start_off + 2 * i).unwrap_or(0xFFFF);
            if code < start {
                return 0;
            }
            let delta = be16(d, delta_off + 2 * i).unwrap_or(0);
            let range = be16(d, range_off + 2 * i).unwrap_or(0) as usize;
            if range == 0 {
                return code.wrapping_add(delta);
            }
            let addr = range_off + 2 * i + range + 2 * usize::from(code - start);
            let g = be16(d, addr).unwrap_or(0);
            return if g == 0 { 0 } else { g.wrapping_add(delta) };
        }
        0
    }

    fn advance(&self, glyph: u16) -> f32 {
        let idx = usize::from(glyph).min(self.num_hmetrics - 1);
        f32::from(be16(self.data, self.hmtx + 4 * idx).unwrap_or(0))
    }
}

fn font_tables(face: Face) -> Option<&'static FontTables> {
    static TABLES: OnceLock<[Option<FontTables>; 5]> = OnceLock::new();
    TABLES.get_or_init(|| {
        [
            parse_font(HANKEN_400),
            parse_font(HANKEN_500),
            parse_font(HANKEN_600),
            parse_font(HANKEN_700),
            parse_font(TANKER_400),
        ]
    })[face.index()]
    .as_ref()
}

/// Advance width of `text` in `size`-point `face` (sum of `hmtx` advances; no kerning).
pub fn text_width(face: Face, size: f32, text: &str) -> f32 {
    match font_tables(face) {
        Some(t) => {
            let units: f32 = text.chars().map(|c| t.advance(t.glyph(c))).sum();
            units / t.upem * size
        }
        None => text.chars().count() as f32 * size * 0.55,
    }
}

/// Greedy word wrap with SwiftUI-like tail truncation when `max_lines` is exceeded.
pub fn wrap_text(text: &str, face: Face, size: f32, max_w: f32, max_lines: usize) -> Vec<String> {
    let max_lines = max_lines.max(1);
    let mut lines: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };
            if text_width(face, size, &candidate) <= max_w {
                current = candidate;
                continue;
            }
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            // A single word wider than the line breaks by character.
            let mut chunk = String::new();
            for ch in word.chars() {
                let mut next = chunk.clone();
                next.push(ch);
                if !chunk.is_empty() && text_width(face, size, &next) > max_w {
                    lines.push(std::mem::take(&mut chunk));
                    chunk.push(ch);
                } else {
                    chunk = next;
                }
            }
            current = chunk;
        }
        lines.push(current);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        let last = lines.pop().unwrap_or_default();
        lines.push(ellipsize(&format!("{last}…"), face, size, max_w));
    }
    lines
}

/// Trim `text` from the tail until it (plus a trailing `…`) fits in `max_w`.
pub fn ellipsize(text: &str, face: Face, size: f32, max_w: f32) -> String {
    if text_width(face, size, text) <= max_w {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.trim_end_matches('…').chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let candidate: String = chars.iter().collect::<String>().trim_end().to_string() + "…";
        if text_width(face, size, &candidate) <= max_w {
            return candidate;
        }
    }
    "…".to_string()
}

// ---------------------------------------------------------------------------------------------
// Drawing primitives (display list)
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
    /// Stroke centred on the rounded-rect outline of `rect` (SwiftUI `.stroke`).
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

/// Text op whose `SwiftUI` line box (`line_height`) is centred on `cy`.
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

/// Scale about `(cx, cy)` by `k`, shift by `dy`, and multiply every alpha by `alpha`.
/// This is SwiftUI `.scaleEffect(k, anchor: .center).opacity(alpha).offset(y: dy)`.
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
// Easing and motion (PillMotion.swift / PillPanel.swift)
// ---------------------------------------------------------------------------------------------

/// `smoothstep`: `PillPanelController.animateCapsule` progress curve `l * l * (3 - 2 * l)`.
pub fn smoothstep(t: f32) -> f32 {
    let l = t.clamp(0.0, 1.0);
    l * l * (3.0 - 2.0 * l)
}

/// CSS-style cubic bezier through (0,0), (x1,y1), (x2,y2), (1,1): SwiftUI `.easeOut` is
/// `(0, 0, 0.58, 1)`.
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let bez = |a: f32, b: f32, u: f32| {
        let inv = 1.0 - u;
        3.0 * inv * inv * u * a + 3.0 * inv * u * u * b + u * u * u
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let mut u = t;
    for _ in 0..16 {
        let x = bez(x1, x2, u);
        if (x - t).abs() < 1e-4 {
            break;
        }
        if x < t {
            lo = u;
        } else {
            hi = u;
        }
        u = (lo + hi) * 0.5;
    }
    bez(y1, y2, u)
}

/// SwiftUI `.easeOut(duration:)`.
pub fn ease_out(t: f32) -> f32 {
    cubic_bezier(0.0, 0.0, 0.58, 1.0, t)
}

/// `PillMotion.settled`: `spring(response: 0.15, dampingFraction: 1)`: critically damped.
pub fn spring_settled(t_sec: f32) -> f32 {
    if t_sec <= 0.0 {
        return 0.0;
    }
    let w = std::f32::consts::TAU / 0.15;
    1.0 - (1.0 + w * t_sec) * (-w * t_sec).exp()
}

/// `PillMotion.state`: `spring(response: 0.15, dampingFraction: 0.85)`: slightly underdamped.
pub fn spring_state(t_sec: f32) -> f32 {
    if t_sec <= 0.0 {
        return 0.0;
    }
    let zeta = 0.85f32;
    let wn = std::f32::consts::TAU / 0.15;
    let wd = wn * (1.0 - zeta * zeta).sqrt();
    let decay = (-zeta * wn * t_sec).exp();
    1.0 - decay * ((wd * t_sec).cos() + (zeta * wn / wd) * (wd * t_sec).sin())
}

/// A scalar eased between two values over a fixed time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fade {
    pub from: f32,
    pub to: f32,
    pub start_ms: f64,
    pub dur_ms: f32,
}

impl Fade {
    pub const fn at(value: f32) -> Fade {
        Fade {
            from: value,
            to: value,
            start_ms: 0.0,
            dur_ms: 0.0,
        }
    }

    pub fn value(&self, now_ms: f64) -> f32 {
        if self.dur_ms <= 0.0 {
            return self.to;
        }
        let t = ((now_ms - self.start_ms) / f64::from(self.dur_ms)) as f32;
        self.from + (self.to - self.from) * ease_out(t)
    }

    pub fn done(&self, now_ms: f64) -> bool {
        self.dur_ms <= 0.0 || now_ms >= self.start_ms + f64::from(self.dur_ms)
    }

    /// Retarget from the current value (SwiftUI re-targets an interrupted animation).
    pub fn retarget(&mut self, to: f32, now_ms: f64, dur_ms: f32) {
        if (self.to - to).abs() < f32::EPSILON {
            return;
        }
        let current = self.value(now_ms);
        *self = Fade {
            from: current,
            to,
            start_ms: now_ms,
            dur_ms,
        };
    }
}

/// `PillModel.introIntensity`: smoothstep up to the 45 % peak, then back down.
pub fn intro_intensity(progress: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    let peak = 0.45;
    let phase = if progress < peak {
        progress / peak
    } else {
        1.0 - (progress - peak) / (1.0 - peak)
    };
    phase * phase * (3.0 - 2.0 * phase)
}

/// Seconds the launch bloom runs (`PillModel.startIntroBloom`).
pub const INTRO_SECONDS: f32 = 2.6;
/// Seconds of the launch swell sound (`BloomSoundPlayer.duration`).
pub const INTRO_SOUND_SECONDS: f32 = 2.55;

/// The launch swell as a 16-bit mono 44.1 kHz WAV: `BloomSoundPlayer.makeBloomWAV` (a sine that
/// glides C5 -> G5 over 1.1 s under a 0.012 -> 0.05 -> 0.0001 gain envelope, 2.55 s long).
pub fn bloom_wav() -> Vec<u8> {
    let sample_rate = 44_100u32;
    let frames = (f64::from(sample_rate) * 2.55) as usize; // INTRO_SOUND_SECONDS
    let payload = (frames * 2) as u32;
    let mut out: Vec<u8> = Vec::with_capacity(44 + frames * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + payload).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&payload.to_le_bytes());
    let mut phase = 0.0f64;
    for frame in 0..frames {
        let time = frame as f64 / f64::from(sample_rate);
        let progress = (time / 1.1).min(1.0);
        let frequency = 523.25 * (783.99f64 / 523.25).powf(progress);
        let gain = if time <= 0.18 {
            0.012 * (0.05f64 / 0.012).powf(time / 0.18)
        } else {
            0.05 * (0.0001f64 / 0.05).powf(((time - 0.18) / 2.32).min(1.0))
        };
        let sample = ((phase.sin() * gain).clamp(-1.0, 1.0) * f64::from(i16::MAX)) as i16;
        out.extend_from_slice(&sample.to_le_bytes());
        phase += std::f64::consts::TAU * frequency / f64::from(sample_rate);
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Pill geometry (PillPanel.swift / PillRootView.swift)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Idle,
    Recording,
    Processing,
    Status,
    Error,
}

/// `PillPanelController.initialSize`.
pub const IDLE_COMPACT: (f32, f32) = (64.0, 10.0);
/// `capsuleSize(for:hovered:)` idle hovered / `clipboardCapsuleSize`.
pub const IDLE_EXPANDED: (f32, f32) = (124.0, 44.0);
pub const COMPACT_RECORDING_BASE: (f32, f32) = (96.0, 32.0);
pub const COMPACT_RECORDING_SCALE: f32 = 1.1;
/// `ceil(96 * 1.1)` x `ceil(32 * 1.1)`.
pub fn compact_recording_size() -> (f32, f32) {
    (
        (COMPACT_RECORDING_BASE.0 * COMPACT_RECORDING_SCALE).ceil(),
        (COMPACT_RECORDING_BASE.1 * COMPACT_RECORDING_SCALE).ceil(),
    )
}
/// `PillPanelController.recordingHoverSize`.
pub const RECORDING_HOVER: (f32, f32) = (189.0, 44.0);
/// `PillRootView.processingCapsuleSize`.
pub const PROCESSING_SIZE: (f32, f32) = (72.0, 32.0);
pub const STATUS_HEIGHT: f32 = 32.0;
pub const STATUS_MIN_WIDTH: f32 = 82.0;
/// Windows has a fixed overlay width, so absurdly long labels are ellipsized to fit.
pub const STATUS_MAX_WIDTH: f32 = 280.0;
pub const BUTTON_SIZE: f32 = 36.0;
pub const BUTTON_FONT: f32 = 15.0;
pub const AUX_SPACING: f32 = 8.0;
/// Capsule bottom sits this many points above the work area (`visible.minY + 4`).
pub const BOTTOM_MARGIN: f32 = 4.0;
/// Hover leave grace (`PillMotion.hoverExitDelay`).
pub const HOVER_EXIT_DELAY_MS: f32 = 100.0;
/// Hover collapse duration (`PillMotion.hoverExitDuration`).
pub const HOVER_EXIT_DURATION_MS: f32 = 260.0;
/// Hover expand / mode-change resize duration.
pub const CAPSULE_RESIZE_MS: f32 = 150.0;
/// Pill window envelope (logical px): wide enough for the widest capsule, tall enough for the
/// 44 pt dock plus the 5 pt reveal offset.
pub const PILL_WINDOW_W: f32 = 300.0;
pub const PILL_WINDOW_H: f32 = 44.0;
pub const PILL_WINDOW_BOTTOM_PAD: f32 = 6.0;

pub fn status_capsule_size(label: &str) -> (f32, f32) {
    let text = text_width(Face::Hanken700, 13.0, label).ceil();
    // 12 pt icon + 7 pt spacing + 26 pt padding + 4 pt rasterisation allowance.
    (
        (text + 49.0).clamp(STATUS_MIN_WIDTH, STATUS_MAX_WIDTH),
        STATUS_HEIGHT,
    )
}

/// `PillPanelController.capsuleSize(for:hovered:clipboardVisible:)` plus the status rule.
pub fn capsule_size(mode: Mode, hovered: bool, clipboard_visible: bool, label: &str) -> (f32, f32) {
    match mode {
        Mode::Idle => {
            if hovered || clipboard_visible {
                IDLE_EXPANDED
            } else {
                IDLE_COMPACT
            }
        }
        Mode::Recording => {
            if hovered {
                RECORDING_HOVER
            } else {
                compact_recording_size()
            }
        }
        Mode::Processing => PROCESSING_SIZE,
        Mode::Status | Mode::Error => status_capsule_size(label),
    }
}

pub fn is_cancelled_label(label: &str) -> bool {
    label.trim().eq_ignore_ascii_case("cancelled")
}

/// `capsuleBackground` incl. the intro tint `(8 + 4i, 8 + 14i, 8 + 30i)`.
pub fn capsule_background(mode: Mode, label: &str, intro: f32) -> Rgba {
    if mode == Mode::Status && is_cancelled_label(label) {
        return tok::AMBER;
    }
    match mode {
        Mode::Status => tok::SUCCESS,
        Mode::Error => tok::ERROR,
        _ => {
            let i = intro.clamp(0.0, 1.0);
            let c = |base: f32, gain: f32| (base + gain * i).round().clamp(0.0, 255.0) as u8;
            Rgba::new(c(8.0, 4.0), c(8.0, 14.0), c(8.0, 30.0), 1.0)
        }
    }
}

/// `borderColor` x the rest-state `0.62 + 0.38 * introBloom` opacity, and `borderWidth + bloom`.
pub fn border_spec(mode: Mode, intro: f32) -> (Rgba, f32) {
    let i = intro.clamp(0.0, 1.0);
    let (base, base_alpha, width) = match mode {
        Mode::Error | Mode::Status => (tok::BLACK, 1.0, 1.0),
        Mode::Recording => (tok::ACCENT, 0.82, 1.5),
        // 30/31 so that x 0.62 the effective idle alpha is exactly 0.60 (WCAG 3:1).
        Mode::Idle => (tok::ACCENT, 0.967_741_9, 1.5),
        Mode::Processing => (tok::ACCENT, 0.78, 1.0),
    };
    (base.with_alpha(base_alpha * (0.62 + 0.38 * i)), width + i)
}

fn erf(x: f32) -> f32 {
    // Abramowitz & Stegun 7.1.26.
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let y = 1.0
        - (((((1.061_405_4 * t - 1.453_152_1) * t) + 1.421_413_7) * t - 0.284_496_72) * t
            + 0.254_829_6)
            * t
            * (-x * x).exp();
    sign * y
}

fn norm_cdf(x: f32) -> f32 {
    0.5 * (1.0 + erf(x / std::f32::consts::SQRT_2))
}

/// The intro bloom overlay: `Capsule().stroke(accent @ .55 + .28b, lineWidth 1 + 8b)
/// .blur(4.5 + 7.5b).opacity(b)` clipped to the capsule, as 1 pt concentric strokes whose alpha
/// follows the Gaussian-blurred band profile.
pub fn intro_glow_ops(cap: VRect, bloom: f32) -> Vec<DrawOp> {
    let b = bloom.clamp(0.0, 1.0);
    if b <= 0.002 {
        return Vec::new();
    }
    let sigma = 4.5 + 7.5 * b;
    let half = (1.0 + 8.0 * b) * 0.5;
    let base_alpha = (0.55 + 0.28 * b) * b;
    let max_depth = (cap.h * 0.5).min(3.0 * sigma + half);
    let mut ops = Vec::new();
    let mut d = 0.0f32;
    while d < max_depth {
        let c = d + 0.5;
        let intensity = norm_cdf((c + half) / sigma) - norm_cdf((c - half) / sigma);
        let alpha = base_alpha * intensity;
        if alpha > 0.004 {
            ops.push(DrawOp::StrokeRRect {
                rect: cap.inset(c),
                radius: (cap.h * 0.5 - c).max(0.5),
                width: 1.0,
                color: tok::ACCENT.with_alpha(alpha),
            });
        }
        d += 1.0;
    }
    ops
}

// ---------------------------------------------------------------------------------------------
// Bars (PillModel.integrateLevels)
// ---------------------------------------------------------------------------------------------

pub const BAR_SLOTS: usize = 25;
pub const RECORDING_BAR_COUNT: usize = 6;
pub const RECORDING_BAR_FIRST: usize = 17;
pub const PROCESSING_BAR_COUNT: usize = 11;

/// Recording branch: per-slot target before smoothing.
pub fn recording_target(level: f32, index: usize, now_sec: f32) -> f32 {
    let base = ((level.max(0.0) - 0.09) / 0.91).max(0.0).powf(0.72);
    let position = index as f32 / 24.0;
    let envelope = 0.30 + 0.70 * (position * std::f32::consts::PI).sin().max(0.0).powf(0.72);
    let flutter = if base > 0.0 {
        (now_sec * (8.4 + (index % 5) as f32 * 0.62) + index as f32 * 1.41).sin() * 0.22
    } else {
        0.0
    };
    (base * envelope + flutter * base).clamp(0.08, 1.0)
}

/// One display tick of the recording buffer. Swift smooths per display-link tick (0.58 rising,
/// 0.22 falling at 60 Hz); `dt_ms` renormalises so the feel does not depend on the refresh rate.
pub fn integrate_recording(buf: &mut [f32; BAR_SLOTS], level: f32, now_sec: f32, dt_ms: f32) {
    let steps = (dt_ms / (1000.0 / 60.0)).clamp(0.25, 6.0);
    for (index, slot) in buf.iter_mut().enumerate() {
        let target = recording_target(level, index, now_sec);
        let k0 = if target > *slot { 0.58f32 } else { 0.22f32 };
        let k = 1.0 - (1.0 - k0).powf(steps);
        *slot += (target - *slot) * k;
    }
}

/// Processing branch: a mic-independent sine sweep.
pub fn processing_levels(now_sec: f32) -> [f32; BAR_SLOTS] {
    let mut out = [0.0f32; BAR_SLOTS];
    for (index, slot) in out.iter_mut().enumerate() {
        let phase = now_sec * 5.6 - index as f32 * 0.48;
        *slot = 0.18 + 0.82 * ((phase.sin() + 1.0) / 2.0).powf(2.8);
    }
    out
}

pub fn recording_bar_height(level: f32) -> f32 {
    (3.5f32).max(3.5 + level.clamp(0.0, 1.0) * 16.5)
}

pub fn processing_bar_height(level: f32) -> f32 {
    (3.0f32).max(3.0 + level.clamp(0.0, 1.0) * 16.0)
}

/// `mm:ss` timer text.
pub fn duration_text(elapsed_secs: u32) -> String {
    format!("{:02}:{:02}", elapsed_secs / 60, elapsed_secs % 60)
}

// ---------------------------------------------------------------------------------------------
// Stroke helpers and icons
// ---------------------------------------------------------------------------------------------

fn circle_op(cx: f32, cy: f32, r: f32, color: Rgba) -> DrawOp {
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

/// Points along a circular arc; angles in radians, y-down screen space, increasing = clockwise.
pub fn arc_pts(cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> Vec<(f32, f32)> {
    let steps = (((a1 - a0).abs() / (std::f32::consts::PI / 14.0)).ceil() as usize).max(2);
    (0..=steps)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / steps as f32;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    House,
    Mic,
    Clock,
    XMark,
    Stop,
    ArrowRight,
    Check,
    Warning,
    Pencil,
    Copy,
    Clockwise,
    CounterClockwise,
}

/// Hand-built outlines for the SF Symbols the Swift pill uses, sized in em of `f` (the symbol's
/// font size in points). `cutout` is painted where the symbol has holes (house door, warning
/// exclamation), i.e. the colour of whatever sits behind the glyph.
pub fn icon_ops(icon: Icon, cx: f32, cy: f32, f: f32, color: Rgba, cutout: Rgba) -> Vec<DrawOp> {
    let mut ops = Vec::new();
    let sw = 0.17 * f;
    match icon {
        Icon::XMark => {
            let h = 0.39 * f;
            stroke_line(&mut ops, (cx - h, cy - h), (cx + h, cy + h), sw, color);
            stroke_line(&mut ops, (cx + h, cy - h), (cx - h, cy + h), sw, color);
        }
        Icon::Check => {
            let pts = [
                (cx - 0.42 * f, cy + 0.02 * f),
                (cx - 0.12 * f, cy + 0.31 * f),
                (cx + 0.42 * f, cy - 0.31 * f),
            ];
            stroke_poly(&mut ops, &pts, sw, color);
        }
        Icon::ArrowRight => {
            stroke_line(
                &mut ops,
                (cx - 0.44 * f, cy),
                (cx + 0.42 * f, cy),
                sw,
                color,
            );
            let head = [
                (cx + 0.06 * f, cy - 0.38 * f),
                (cx + 0.44 * f, cy),
                (cx + 0.06 * f, cy + 0.38 * f),
            ];
            stroke_poly(&mut ops, &head, sw, color);
        }
        Icon::Stop => {
            let s = 0.74 * f;
            ops.push(DrawOp::FillRRect {
                rect: VRect::centered(cx, cy, s, s),
                radius: 0.13 * f,
                color,
            });
        }
        Icon::Warning => {
            let tri = vec![
                (cx, cy - 0.46 * f),
                (cx + 0.52 * f, cy + 0.42 * f),
                (cx - 0.52 * f, cy + 0.42 * f),
            ];
            ops.push(DrawOp::Polygon {
                pts: tri.clone(),
                color,
            });
            // Round the corners with a closed stroke of the same colour.
            let mut closed = tri;
            closed.push(closed[0]);
            stroke_poly(&mut ops, &closed, 0.12 * f, color);
            ops.push(DrawOp::FillRRect {
                rect: VRect::new(cx - 0.055 * f, cy - 0.20 * f, 0.11 * f, 0.34 * f),
                radius: 0.055 * f,
                color: cutout,
            });
            ops.push(circle_op(cx, cy + 0.27 * f, 0.065 * f, cutout));
        }
        Icon::House => {
            let roof = vec![
                (cx - 0.56 * f, cy - 0.04 * f),
                (cx, cy - 0.52 * f),
                (cx + 0.56 * f, cy - 0.04 * f),
            ];
            ops.push(DrawOp::Polygon {
                pts: roof.clone(),
                color,
            });
            let mut closed = roof;
            closed.push(closed[0]);
            stroke_poly(&mut ops, &closed, 0.10 * f, color);
            ops.push(DrawOp::FillRRect {
                rect: VRect::new(cx - 0.40 * f, cy - 0.10 * f, 0.80 * f, 0.56 * f),
                radius: 0.06 * f,
                color,
            });
            ops.push(DrawOp::FillRRect {
                rect: VRect::new(cx - 0.11 * f, cy + 0.12 * f, 0.22 * f, 0.34 * f),
                radius: 0.03 * f,
                color: cutout,
            });
        }
        Icon::Mic => {
            ops.push(DrawOp::FillRRect {
                rect: VRect::new(cx - 0.17 * f, cy - 0.52 * f, 0.34 * f, 0.64 * f),
                radius: 0.17 * f,
                color,
            });
            let u = arc_pts(cx, cy + 0.02 * f, 0.36 * f, 0.0, std::f32::consts::PI);
            stroke_poly(&mut ops, &u, 0.11 * f, color);
            stroke_line(
                &mut ops,
                (cx, cy + 0.38 * f),
                (cx, cy + 0.52 * f),
                0.11 * f,
                color,
            );
            stroke_line(
                &mut ops,
                (cx - 0.20 * f, cy + 0.52 * f),
                (cx + 0.20 * f, cy + 0.52 * f),
                0.11 * f,
                color,
            );
        }
        Icon::Clock => {
            let (ccx, ccy, r) = (cx + 0.06 * f, cy, 0.42 * f);
            // Open circle: gap at the upper left where the arrow head sits.
            let start = 3.67; // 210 deg
            let arc = arc_pts(ccx, ccy, r, start, start + 4.9);
            stroke_poly(&mut ops, &arc, 0.11 * f, color);
            let tip = (ccx + r * start.cos(), ccy + r * start.sin());
            // Tangent for decreasing angle (counter-clockwise on screen).
            let (tx, ty) = (start.sin(), -start.cos());
            let (nx, ny) = (-ty, tx);
            ops.push(DrawOp::Polygon {
                pts: vec![
                    (tip.0 + tx * 0.22 * f, tip.1 + ty * 0.22 * f),
                    (tip.0 + nx * 0.17 * f, tip.1 + ny * 0.17 * f),
                    (tip.0 - nx * 0.17 * f, tip.1 - ny * 0.17 * f),
                ],
                color,
            });
            stroke_poly(
                &mut ops,
                &[
                    (ccx, ccy - 0.22 * f),
                    (ccx, ccy),
                    (ccx + 0.17 * f, ccy + 0.10 * f),
                ],
                0.10 * f,
                color,
            );
        }
        Icon::Pencil => {
            // Axis runs lower-left to upper-right.
            let (dx, dy) = (
                std::f32::consts::FRAC_1_SQRT_2,
                -std::f32::consts::FRAC_1_SQRT_2,
            );
            let (nx, ny) = (
                std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
            );
            let pt = |t: f32, n: f32| (cx + dx * t * f + nx * n * f, cy + dy * t * f + ny * n * f);
            ops.push(DrawOp::Polygon {
                pts: vec![
                    pt(-0.12, -0.15),
                    pt(0.46, -0.15),
                    pt(0.46, 0.15),
                    pt(-0.12, 0.15),
                ],
                color,
            });
            ops.push(DrawOp::Polygon {
                pts: vec![pt(-0.12, -0.15), pt(-0.12, 0.15), pt(-0.50, 0.0)],
                color,
            });
        }
        Icon::Copy => {
            ops.push(DrawOp::StrokeRRect {
                rect: VRect::new(cx - 0.18 * f, cy - 0.26 * f, 0.58 * f, 0.76 * f).inset(0.055 * f),
                radius: 0.12 * f,
                width: 0.11 * f,
                color,
            });
            stroke_poly(
                &mut ops,
                &[
                    (cx + 0.12 * f, cy - 0.47 * f),
                    (cx - 0.28 * f, cy - 0.47 * f),
                    (cx - 0.40 * f, cy - 0.36 * f),
                    (cx - 0.40 * f, cy + 0.26 * f),
                ],
                0.11 * f,
                color,
            );
        }
        Icon::Clockwise | Icon::CounterClockwise => {
            let cw = icon == Icon::Clockwise;
            let r = 0.38 * f;
            let a0 = -1.05f32;
            let sweep = 4.7f32;
            let (start, end) = if cw {
                (a0, a0 + sweep)
            } else {
                (std::f32::consts::PI - a0, std::f32::consts::PI - a0 - sweep)
            };
            stroke_poly(&mut ops, &arc_pts(cx, cy, r, start, end), 0.12 * f, color);
            let tip = (cx + r * end.cos(), cy + r * end.sin());
            let (tx, ty) = if cw {
                (-end.sin(), end.cos())
            } else {
                (end.sin(), -end.cos())
            };
            let (nx, ny) = (-ty, tx);
            ops.push(DrawOp::Polygon {
                pts: vec![
                    (tip.0 + tx * 0.22 * f, tip.1 + ty * 0.22 * f),
                    (tip.0 + nx * 0.18 * f, tip.1 + ny * 0.18 * f),
                    (tip.0 - nx * 0.18 * f, tip.1 - ny * 0.18 * f),
                ],
                color,
            });
        }
    }
    ops
}

// ---------------------------------------------------------------------------------------------
// Pill capsule scene -> display list
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ButtonId {
    Hub,
    Start,
    Recent,
    Cancel,
    Stop,
    Send,
}

pub const BUTTONS: [ButtonId; 6] = [
    ButtonId::Hub,
    ButtonId::Start,
    ButtonId::Recent,
    ButtonId::Cancel,
    ButtonId::Stop,
    ButtonId::Send,
];

impl ButtonId {
    pub fn index(self) -> usize {
        match self {
            ButtonId::Hub => 0,
            ButtonId::Start => 1,
            ButtonId::Recent => 2,
            ButtonId::Cancel => 3,
            ButtonId::Stop => 4,
            ButtonId::Send => 5,
        }
    }

    /// Hit-region id inside the pill overlay (`1` is the capsule itself).
    pub fn region(self) -> u32 {
        2 + self.index() as u32
    }

    pub fn from_region(region: u32) -> Option<ButtonId> {
        BUTTONS.iter().copied().find(|b| b.region() == region)
    }

    pub fn tint(self) -> Rgba {
        match self {
            ButtonId::Hub => tok::EMBER,
            ButtonId::Start => tok::SUCCESS,
            ButtonId::Recent => tok::WHITE,
            ButtonId::Cancel => tok::ERROR,
            ButtonId::Stop => tok::AMBER,
            ButtonId::Send => tok::SUCCESS,
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            ButtonId::Hub => Icon::House,
            ButtonId::Start => Icon::Mic,
            ButtonId::Recent => Icon::Clock,
            ButtonId::Cancel => Icon::XMark,
            ButtonId::Stop => Icon::Stop,
            ButtonId::Send => Icon::ArrowRight,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ButtonId::Hub => "Open app",
            ButtonId::Start => "Start recording",
            ButtonId::Recent => "Open recent transcripts",
            ButtonId::Cancel => "Cancel",
            ButtonId::Stop => "Stop",
            ButtonId::Send => "Stop and send",
        }
    }
}

/// Everything the capsule painter needs for one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct PillScene {
    pub mode: Mode,
    pub label: String,
    /// Presentation (possibly mid-animation) capsule size.
    pub cap_w: f32,
    pub cap_h: f32,
    /// `model.isHovered`.
    pub hovered: bool,
    pub clipboard_visible: bool,
    pub hint_visible: bool,
    pub bars: [f32; BAR_SLOTS],
    pub elapsed_secs: u32,
    /// `model.introBloom`.
    pub intro: f32,
    /// Per-button hover amount 0..=1 (`hot` fade).
    pub hot: [f32; 6],
    /// `isVisible` opacity (hide fade).
    pub opacity: f32,
    /// `revealProgress ? 0 : 5` eased.
    pub offset_y: f32,
}

impl PillScene {
    pub fn rest() -> PillScene {
        PillScene {
            mode: Mode::Idle,
            label: String::new(),
            cap_w: IDLE_COMPACT.0,
            cap_h: IDLE_COMPACT.1,
            hovered: false,
            clipboard_visible: false,
            hint_visible: false,
            bars: [0.0; BAR_SLOTS],
            elapsed_secs: 0,
            intro: 0.0,
            hot: [0.0; 6],
            opacity: 1.0,
            offset_y: 0.0,
        }
    }

    /// `PillCapsule.expanded`: idle controls are visible.
    pub fn idle_controls_visible(&self) -> bool {
        self.mode == Mode::Idle && (self.clipboard_visible || (!self.hint_visible && self.hovered))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PillFrame {
    pub ops: Vec<DrawOp>,
    pub capsule: VRect,
    pub buttons: Vec<(ButtonId, VRect)>,
}

fn push_button(
    ops: &mut Vec<DrawOp>,
    buttons: &mut Vec<(ButtonId, VRect)>,
    id: ButtonId,
    cx: f32,
    cy: f32,
    hot: f32,
    behind: Rgba,
) {
    let tint = id.tint();
    let hot = hot.clamp(0.0, 1.0);
    if hot > 0.003 {
        ops.push(circle_op(cx, cy, BUTTON_SIZE * 0.5, tint.mul_alpha(hot)));
    }
    let glyph = tint.lerp(tok::BLACK, hot);
    let cutout = behind.lerp(tint, hot);
    ops.extend(icon_ops(id.icon(), cx, cy, BUTTON_FONT, glyph, cutout));
    buttons.push((id, VRect::centered(cx, cy, BUTTON_SIZE, BUTTON_SIZE)));
}

fn push_bars_recording(ops: &mut Vec<DrawOp>, levels: &[f32; BAR_SLOTS], cx: f32, cy: f32, k: f32) {
    // HStack(spacing: 2.2) of six 2.2 pt capsules centred in a 25 x 22 frame.
    let bar_w = 2.2 * k;
    let gap = 2.2 * k;
    let total = RECORDING_BAR_COUNT as f32 * bar_w + (RECORDING_BAR_COUNT as f32 - 1.0) * gap;
    let mut x = cx - total * 0.5;
    for i in 0..RECORDING_BAR_COUNT {
        let level = levels[RECORDING_BAR_FIRST + i];
        let h = recording_bar_height(level) * k;
        ops.push(DrawOp::FillRRect {
            rect: VRect::new(x, cy - h * 0.5, bar_w, h),
            radius: bar_w * 0.5,
            color: tok::EMBER,
        });
        x += bar_w + gap;
    }
}

fn push_wave_processing(ops: &mut Vec<DrawOp>, levels: &[f32; BAR_SLOTS], cx: f32, cy: f32) {
    // HStack(spacing: 2) of eleven 2 pt white capsules centred in a 44 x 22 frame.
    let total = PROCESSING_BAR_COUNT as f32 * 2.0 + (PROCESSING_BAR_COUNT as f32 - 1.0) * 2.0;
    let first = (BAR_SLOTS - PROCESSING_BAR_COUNT) / 2;
    let mut x = cx - total * 0.5;
    for i in 0..PROCESSING_BAR_COUNT {
        let h = processing_bar_height(levels[first + i]);
        ops.push(DrawOp::FillRRect {
            rect: VRect::new(x, cy - h * 0.5, 2.0, h),
            radius: 1.0,
            color: tok::WHITE,
        });
        x += 4.0;
    }
}

/// Width of the recording timer text.
pub fn timer_width(elapsed_secs: u32) -> f32 {
    text_width(Face::Tanker, 18.0, &duration_text(elapsed_secs))
}

/// Natural width of the hovered recording dock content (`RecordingControls` HStack).
pub fn recording_hover_content_width(elapsed_secs: u32) -> f32 {
    let pad = (RECORDING_HOVER.1 - BUTTON_SIZE) / 2.0;
    pad + BUTTON_SIZE
        + 3.0
        + (25.0 + 6.0 + timer_width(elapsed_secs))
        + 3.0
        + BUTTON_SIZE
        + 3.0
        + BUTTON_SIZE
        + pad
}

/// Build the pill capsule for one frame. `win_w`/`win_h` are the overlay window size and
/// `bottom_pad` the transparent strip under the capsule's resting bottom edge.
pub fn build_pill(scene: &PillScene, win_w: f32, win_h: f32, bottom_pad: f32) -> PillFrame {
    let cap = VRect::new(
        (win_w - scene.cap_w) * 0.5,
        win_h - bottom_pad - scene.cap_h + scene.offset_y,
        scene.cap_w,
        scene.cap_h,
    );
    let radius = cap.h * 0.5;
    let bg = capsule_background(scene.mode, &scene.label, scene.intro);
    let (border, border_w) = border_spec(scene.mode, scene.intro);
    let mut ops: Vec<DrawOp> = Vec::new();
    let mut buttons: Vec<(ButtonId, VRect)> = Vec::new();

    ops.push(DrawOp::FillRRect {
        rect: cap,
        radius,
        color: bg,
    });
    ops.extend(intro_glow_ops(cap, scene.intro));
    ops.push(DrawOp::StrokeRRect {
        rect: cap.inset(border_w * 0.5),
        radius: (radius - border_w * 0.5).max(0.0),
        width: border_w,
        color: border,
    });
    // Content never reaches outside the capsule at rest; during the resize morph the clip keeps
    // the 36 pt buttons inside the (rounded) capsule bounds.
    ops.push(DrawOp::Clip(cap));

    let cx = cap.cx();
    let cy = cap.cy();
    match scene.mode {
        Mode::Idle => {
            if scene.idle_controls_visible() {
                // HStack(spacing: 4) of three 36 pt circles, centred.
                let x0 = cx - (3.0 * BUTTON_SIZE + 2.0 * 4.0) * 0.5;
                for (i, id) in [ButtonId::Hub, ButtonId::Start, ButtonId::Recent]
                    .into_iter()
                    .enumerate()
                {
                    let bx = x0 + BUTTON_SIZE * 0.5 + i as f32 * (BUTTON_SIZE + 4.0);
                    push_button(
                        &mut ops,
                        &mut buttons,
                        id,
                        bx,
                        cy,
                        scene.hot[id.index()],
                        bg,
                    );
                }
            }
        }
        Mode::Recording => {
            let timer = duration_text(scene.elapsed_secs);
            let tw = text_width(Face::Tanker, 18.0, &timer);
            if scene.hovered {
                let total = recording_hover_content_width(scene.elapsed_secs);
                let pad = (RECORDING_HOVER.1 - BUTTON_SIZE) / 2.0;
                let mut x = cx - total * 0.5 + pad;
                push_button(
                    &mut ops,
                    &mut buttons,
                    ButtonId::Cancel,
                    x + BUTTON_SIZE * 0.5,
                    cy,
                    scene.hot[ButtonId::Cancel.index()],
                    bg,
                );
                x += BUTTON_SIZE + 3.0;
                push_bars_recording(&mut ops, &scene.bars, x + 12.5, cy, 1.0);
                ops.push(text_centered(
                    &timer,
                    Face::Tanker,
                    18.0,
                    tok::FOREGROUND,
                    x + 25.0 + 6.0,
                    cy,
                    tw + 2.0,
                    Align::Left,
                ));
                x += 25.0 + 6.0 + tw + 3.0;
                push_button(
                    &mut ops,
                    &mut buttons,
                    ButtonId::Stop,
                    x + BUTTON_SIZE * 0.5,
                    cy,
                    scene.hot[ButtonId::Stop.index()],
                    bg,
                );
                x += BUTTON_SIZE + 3.0;
                push_button(
                    &mut ops,
                    &mut buttons,
                    ButtonId::Send,
                    x + BUTTON_SIZE * 0.5,
                    cy,
                    scene.hot[ButtonId::Send.index()],
                    bg,
                );
            } else {
                // EqualGapRecordingLayout in a 96 x 32 frame, then scaleEffect(1.1) about the
                // capsule centre.
                let k = COMPACT_RECORDING_SCALE;
                let gap = ((COMPACT_RECORDING_BASE.0 - 25.0 - tw) / 3.0).max(0.0);
                let bars_cx = cx + (-COMPACT_RECORDING_BASE.0 * 0.5 + gap + 12.5) * k;
                push_bars_recording(&mut ops, &scene.bars, bars_cx, cy, k);
                let timer_x = cx + (-COMPACT_RECORDING_BASE.0 * 0.5 + gap * 2.0 + 25.0) * k;
                ops.push(text_centered(
                    &timer,
                    Face::Tanker,
                    18.0 * k,
                    tok::FOREGROUND,
                    timer_x,
                    cy,
                    (tw + 2.0) * k,
                    Align::Left,
                ));
            }
        }
        Mode::Processing => push_wave_processing(&mut ops, &scene.bars, cx, cy),
        Mode::Status | Mode::Error => {
            let ink = tok::BLACK.with_alpha(0.86);
            let max_label = (scene.cap_w - 49.0).max(10.0);
            let label = ellipsize(&scene.label, Face::Hanken700, 13.0, max_label);
            let tw = text_width(Face::Hanken700, 13.0, &label);
            let total = 12.0 + 7.0 + tw;
            let x0 = cx - total * 0.5;
            let icon = if scene.mode == Mode::Error {
                Icon::Warning
            } else if is_cancelled_label(&scene.label) {
                Icon::XMark
            } else {
                Icon::Check
            };
            ops.extend(icon_ops(icon, x0 + 6.0, cy, 12.0, ink, bg));
            ops.push(text_centered(
                &label,
                Face::Hanken700,
                13.0,
                ink,
                x0 + 19.0,
                cy,
                tw + 2.0,
                Align::Left,
            ));
        }
    }

    if scene.opacity < 0.999 {
        ops = transform_ops(
            &ops,
            cap.cx(),
            cap.cy(),
            1.0,
            scene.opacity.clamp(0.0, 1.0),
            0.0,
        );
    }
    PillFrame {
        ops,
        capsule: cap,
        buttons,
    }
}

// ---------------------------------------------------------------------------------------------
// Shared aux primitives (hint + clip)
// ---------------------------------------------------------------------------------------------

/// `AuxIcon`: 26 x 26 circle, 13 pt bold glyph in a 16 x 16 frame; hot fills with the tint and
/// turns the glyph black.
pub fn push_aux_icon(ops: &mut Vec<DrawOp>, cx: f32, cy: f32, icon: Icon, tint: Rgba, hot: f32) {
    let hot = hot.clamp(0.0, 1.0);
    if hot > 0.003 {
        ops.push(circle_op(cx, cy, 13.0, tint.mul_alpha(hot)));
    }
    let glyph = tint.lerp(tok::BLACK, hot);
    let cutout = tok::AUX_SURFACE.lerp(tint, hot);
    ops.extend(icon_ops(icon, cx, cy, 13.0, glyph, cutout));
}

pub const AUX_ICON_SIZE: f32 = 26.0;

/// Width of an `AuxTextButton` (11 pt bold, 8 pt horizontal padding).
pub fn aux_text_button_width(title: &str) -> f32 {
    text_width(Face::Hanken700, 11.0, title) + 16.0
}

/// `AuxTextButton`: 24 pt tall capsule; hot fills with `AUX_HOVER`.
pub fn push_aux_text_button(ops: &mut Vec<DrawOp>, rect: VRect, title: &str, hot: f32) {
    let hot = hot.clamp(0.0, 1.0);
    if hot > 0.003 {
        ops.push(DrawOp::FillRRect {
            rect,
            radius: rect.h * 0.5,
            color: tok::AUX_HOVER.mul_alpha(hot),
        });
    }
    ops.push(text_centered(
        title,
        Face::Hanken700,
        11.0,
        tok::AUX_ACCENT,
        rect.x + 8.0,
        rect.cy(),
        rect.w - 16.0,
        Align::Left,
    ));
}

/// Card surface shared by hint and clip: black fill, 1 pt `AUX_LINE` stroke at .72, continuous
/// corner `radius`.
pub fn push_aux_card(ops: &mut Vec<DrawOp>, rect: VRect, radius: f32) {
    ops.push(DrawOp::FillRRect {
        rect,
        radius,
        color: tok::AUX_SURFACE,
    });
    ops.push(DrawOp::StrokeRRect {
        rect,
        radius,
        width: 1.0,
        color: tok::AUX_LINE.with_alpha(0.72),
    });
}

/// `CenteredAuxiliaryEntrance`: opacity 0 -> 1 and scale 0.965 -> 1 on `PillMotion.settled`.
pub fn aux_entrance(elapsed_ms: f32) -> (f32, f32) {
    let p = spring_settled(elapsed_ms / 1000.0).clamp(0.0, 1.0);
    (p, 0.965 + 0.035 * p)
}

// ---------------------------------------------------------------------------------------------
// Hint catalog (src/shared/hint-catalog.json, resolved for Windows)
// ---------------------------------------------------------------------------------------------

static HINT_CATALOG: &str = include_str!("../../../src/shared/hint-catalog.json");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintText {
    pub title: String,
    pub body: String,
}

fn resolve_catalog_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(_) => value
            .get("windows")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

/// Feature hint text for `kind` with the Windows variant selected; `None` for hints scoped to
/// another OS, bespoke hints (screenshot/prompt) and unknown kinds.
pub fn feature_hint(kind: &str) -> Option<HintText> {
    if kind.starts_with('_') {
        return None;
    }
    let root: serde_json::Value = serde_json::from_str(HINT_CATALOG).ok()?;
    let entry = root.get(kind)?;
    if let Some(platform) = entry.get("platform").and_then(serde_json::Value::as_str) {
        if platform != "both" && platform != "windows" {
            return None;
        }
    }
    Some(HintText {
        title: resolve_catalog_text(entry.get("title")?)?,
        body: resolve_catalog_text(entry.get("body")?)?,
    })
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn capsule_sizes_match_swift_tokens() {
        assert_eq!(capsule_size(Mode::Idle, false, false, ""), (64.0, 10.0));
        assert_eq!(capsule_size(Mode::Idle, true, false, ""), (124.0, 44.0));
        assert_eq!(capsule_size(Mode::Idle, false, true, ""), (124.0, 44.0));
        // ceil(96 * 1.1) = 106, ceil(32 * 1.1) = ceil(35.2) = 36
        assert_eq!(
            capsule_size(Mode::Recording, false, false, ""),
            (106.0, 36.0)
        );
        assert_eq!(
            capsule_size(Mode::Recording, true, false, ""),
            (189.0, 44.0)
        );
        assert_eq!(
            capsule_size(Mode::Processing, false, false, ""),
            (72.0, 32.0)
        );
    }

    #[test]
    fn status_width_is_text_plus_49_with_82_floor() {
        let (w, h) = status_capsule_size("Pasted");
        assert_eq!(h, 32.0);
        assert!(approx(
            w,
            text_width(Face::Hanken700, 13.0, "Pasted").ceil() + 49.0
        ));
        assert_eq!(status_capsule_size("OK").0, 82.0);
        assert!(status_capsule_size(&"x".repeat(200)).0 <= STATUS_MAX_WIDTH);
    }

    #[test]
    fn text_metrics_read_the_bundled_fonts() {
        let t = timer_width(0);
        assert!(t > 25.0 && t < 45.0, "Tanker 18pt 00:00 width {t}");
        let say = text_width(Face::Hanken700, 12.0, "Say");
        assert!(say > 12.0 && say < 28.0, "Hanken bold 12pt Say width {say}");
        assert!(
            text_width(Face::Hanken600, 12.0, "iiii") < text_width(Face::Hanken600, 12.0, "mmmm")
        );
    }

    #[test]
    fn border_alpha_follows_swift_rest_state_arithmetic() {
        let (idle, idle_w) = border_spec(Mode::Idle, 0.0);
        assert!(
            approx(idle.a * 100.0, 60.0),
            "idle border .60, got {}",
            idle.a
        );
        assert_eq!(idle_w, 1.5);
        let (rec, rec_w) = border_spec(Mode::Recording, 0.0);
        assert!(approx(rec.a * 1000.0, 0.82 * 0.62 * 1000.0));
        assert_eq!(rec_w, 1.5);
        let (proc, proc_w) = border_spec(Mode::Processing, 0.0);
        assert!(approx(proc.a * 1000.0, 0.78 * 0.62 * 1000.0));
        assert_eq!(proc_w, 1.0);
        let (status, _) = border_spec(Mode::Status, 0.0);
        assert_eq!((status.r, status.g, status.b), (0, 0, 0));
        assert!(approx(status.a * 100.0, 62.0));
        let (bloom, bloom_w) = border_spec(Mode::Idle, 1.0);
        assert!(approx(bloom.a, 0.967_741_9));
        assert_eq!(bloom_w, 2.5);
    }

    #[test]
    fn backgrounds_follow_mode_and_intro_tint() {
        assert_eq!(capsule_background(Mode::Idle, "", 0.0), Rgba::rgb(0x080808));
        let tinted = capsule_background(Mode::Idle, "", 1.0);
        assert_eq!((tinted.r, tinted.g, tinted.b), (12, 22, 38));
        assert_eq!(
            capsule_background(Mode::Status, "Pasted", 0.0),
            tok::SUCCESS
        );
        assert_eq!(
            capsule_background(Mode::Status, "Cancelled", 0.0),
            tok::AMBER
        );
        assert_eq!(
            capsule_background(Mode::Status, "cancelled", 0.0),
            tok::AMBER
        );
        assert_eq!(capsule_background(Mode::Error, "Mic", 0.0), tok::ERROR);
    }

    #[test]
    fn bar_heights_match_swift_frame_math() {
        assert!(approx(recording_bar_height(0.0), 3.5));
        assert!(approx(recording_bar_height(1.0), 20.0));
        assert!(approx(recording_bar_height(2.0), 20.0));
        assert!(approx(processing_bar_height(0.0), 3.0));
        assert!(approx(processing_bar_height(1.0), 19.0));
        let levels = processing_levels(1.234);
        assert!(levels.iter().all(|l| (0.18..=1.0).contains(l)));
    }

    #[test]
    fn recording_buffer_attacks_fast_and_releases_slowly() {
        let mut buf = [0.0f32; BAR_SLOTS];
        integrate_recording(&mut buf, 1.0, 0.5, 1000.0 / 60.0);
        let attacked = buf[12];
        assert!(attacked > 0.3, "fast attack, got {attacked}");
        integrate_recording(&mut buf, 0.0, 0.52, 1000.0 / 60.0);
        let released = buf[12];
        assert!(
            released < attacked && released > attacked * 0.5,
            "slow release, got {released}"
        );
        // Silence settles on the 0.08 floor, never zero.
        for i in 0..400 {
            integrate_recording(&mut buf, 0.0, 1.0 + i as f32 * 0.016, 16.7);
        }
        assert!(buf.iter().all(|l| (l - 0.08).abs() < 0.01));
    }

    #[test]
    fn dt_normalisation_makes_refresh_rate_irrelevant() {
        let mut a = [0.0f32; BAR_SLOTS];
        let mut b = [0.0f32; BAR_SLOTS];
        integrate_recording(&mut a, 0.8, 1.0, 1000.0 / 30.0);
        integrate_recording(&mut b, 0.8, 1.0, 1000.0 / 60.0);
        integrate_recording(&mut b, 0.8, 1.0, 1000.0 / 60.0);
        for i in 0..BAR_SLOTS {
            assert!((a[i] - b[i]).abs() < 0.05, "slot {i}: {} vs {}", a[i], b[i]);
        }
    }

    #[test]
    fn intro_curve_peaks_at_45_percent() {
        assert!(approx(intro_intensity(0.0), 0.0));
        assert!(approx(intro_intensity(0.45), 1.0));
        assert!(approx(intro_intensity(1.0), 0.0));
        assert!(intro_intensity(0.2) < intro_intensity(0.4));
    }

    #[test]
    fn springs_settle() {
        assert!(approx(spring_settled(0.0), 0.0));
        assert!(spring_settled(0.35) > 0.99);
        assert!(spring_state(0.6) > 0.98 && spring_state(0.6) < 1.02);
        assert!(approx(smoothstep(0.5), 0.5));
        assert!(approx(ease_out(1.0), 1.0));
        assert!(ease_out(0.5) > 0.5);
    }

    #[test]
    fn duration_text_pads_minutes_and_seconds() {
        assert_eq!(duration_text(0), "00:00");
        assert_eq!(duration_text(75), "01:15");
        assert_eq!(duration_text(3600), "60:00");
    }

    #[test]
    fn idle_compact_pill_is_a_bottom_centred_bordered_capsule() {
        let scene = PillScene::rest();
        let frame = build_pill(&scene, 300.0, 44.0, 6.0);
        assert!(approx(frame.capsule.w, 64.0) && approx(frame.capsule.h, 10.0));
        assert!(approx(frame.capsule.cx(), 150.0));
        assert!(approx(frame.capsule.bottom(), 38.0));
        assert!(frame.buttons.is_empty(), "no controls at rest");
        match &frame.ops[0] {
            DrawOp::FillRRect { color, radius, .. } => {
                assert_eq!(*color, tok::COFFEE);
                assert!(approx(*radius, 5.0));
            }
            other => panic!("first op must be the capsule fill, got {other:?}"),
        }
    }

    #[test]
    fn idle_hover_shows_three_36pt_buttons_four_apart() {
        let mut scene = PillScene::rest();
        scene.hovered = true;
        scene.cap_w = 124.0;
        scene.cap_h = 44.0;
        let frame = build_pill(&scene, 300.0, 44.0, 6.0);
        assert_eq!(frame.buttons.len(), 3);
        let ids: Vec<ButtonId> = frame.buttons.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![ButtonId::Hub, ButtonId::Start, ButtonId::Recent]);
        for (_, r) in &frame.buttons {
            assert!(approx(r.w, 36.0) && approx(r.h, 36.0));
        }
        let gap = frame.buttons[1].1.x - frame.buttons[0].1.right();
        assert!(approx(gap, 4.0));
        // Row is centred in the 124 pt capsule: 4 pt padding each side.
        assert!(approx(frame.buttons[0].1.x - frame.capsule.x, 4.0));
        assert!(approx(
            frame.capsule.right() - frame.buttons[2].1.right(),
            4.0
        ));
    }

    #[test]
    fn a_visible_hint_suppresses_idle_hover_controls() {
        let mut scene = PillScene::rest();
        scene.hovered = true;
        scene.hint_visible = true;
        assert!(!scene.idle_controls_visible());
        scene.hint_visible = false;
        assert!(scene.idle_controls_visible());
        scene.hovered = false;
        scene.clipboard_visible = true;
        assert!(
            scene.idle_controls_visible(),
            "an open clip keeps the dock expanded"
        );
    }

    #[test]
    fn recording_hover_dock_has_cancel_stop_send() {
        let mut scene = PillScene::rest();
        scene.mode = Mode::Recording;
        scene.hovered = true;
        scene.cap_w = 189.0;
        scene.cap_h = 44.0;
        let frame = build_pill(&scene, 300.0, 44.0, 6.0);
        let ids: Vec<ButtonId> = frame.buttons.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![ButtonId::Cancel, ButtonId::Stop, ButtonId::Send]);
        // Swift: HStack spacing 3 between the 36 pt buttons around bars + timer.
        let stop_to_send = frame.buttons[2].1.x - frame.buttons[1].1.right();
        assert!(approx(stop_to_send, 3.0));
        let content = recording_hover_content_width(0);
        assert!(content > 150.0 && content < 200.0, "dock content {content}");
    }

    #[test]
    fn compact_recording_draws_six_ember_bars_and_scales_them() {
        let mut scene = PillScene::rest();
        scene.mode = Mode::Recording;
        scene.cap_w = 106.0;
        scene.cap_h = 36.0;
        scene.bars = [1.0; BAR_SLOTS];
        let frame = build_pill(&scene, 300.0, 44.0, 6.0);
        let bars: Vec<&DrawOp> = frame
            .ops
            .iter()
            .filter(|op| matches!(op, DrawOp::FillRRect { color, .. } if *color == tok::EMBER))
            .collect();
        assert_eq!(bars.len(), 6);
        if let DrawOp::FillRRect { rect, .. } = bars[0] {
            // 2.2 pt x 1.1 scale, full level = 20 pt x 1.1.
            assert!(approx(rect.w, 2.42));
            assert!(approx(rect.h, 22.0));
        }
        assert!(frame.buttons.is_empty());
        let text = frame
            .ops
            .iter()
            .find_map(|op| match op {
                DrawOp::Text { text, size, .. } => Some((text.clone(), *size)),
                _ => None,
            })
            .expect("timer text");
        assert_eq!(text.0, "00:00");
        assert!(approx(text.1, 19.8));
    }

    #[test]
    fn processing_draws_eleven_white_bars() {
        let mut scene = PillScene::rest();
        scene.mode = Mode::Processing;
        scene.cap_w = 72.0;
        scene.cap_h = 32.0;
        scene.bars = processing_levels(0.3);
        let frame = build_pill(&scene, 300.0, 44.0, 6.0);
        let bars = frame
            .ops
            .iter()
            .filter(|op| matches!(op, DrawOp::FillRRect { color, .. } if *color == tok::WHITE))
            .count();
        assert_eq!(bars, 11);
    }

    #[test]
    fn status_and_error_capsules_use_black_ink_on_filled_capsules() {
        let mut scene = PillScene::rest();
        scene.mode = Mode::Status;
        scene.label = "Pasted".to_string();
        let (w, h) = status_capsule_size("Pasted");
        scene.cap_w = w;
        scene.cap_h = h;
        let frame = build_pill(&scene, 300.0, 44.0, 6.0);
        match &frame.ops[0] {
            DrawOp::FillRRect { color, .. } => assert_eq!(*color, tok::SUCCESS),
            other => panic!("{other:?}"),
        }
        let label = frame
            .ops
            .iter()
            .find_map(|op| match op {
                DrawOp::Text {
                    text, color, size, ..
                } => Some((text.clone(), *color, *size)),
                _ => None,
            })
            .expect("status label");
        assert_eq!(label.0, "Pasted");
        assert!(approx(label.2, 13.0));
        assert_eq!((label.1.r, label.1.g, label.1.b), (0, 0, 0));
        assert!(approx(label.1.a, 0.86));
        scene.mode = Mode::Error;
        scene.label = "Mic unavailable".to_string();
        let frame = build_pill(&scene, 300.0, 44.0, 6.0);
        match &frame.ops[0] {
            DrawOp::FillRRect { color, .. } => assert_eq!(*color, tok::ERROR),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn text_ops_are_baseline_anchored_inside_their_glyph_box() {
        let op = text_centered(
            "Hi",
            Face::Hanken700,
            13.0,
            tok::WHITE,
            10.0,
            20.0,
            30.0,
            Align::Left,
        );
        let b = op.text_box().unwrap();
        let lh = Face::Hanken700.line_height(13.0);
        // The SwiftUI line box (ascent + descent + gap) is centred on cy = 20.
        assert!(approx(b.y, 20.0 - lh * 0.5));
        assert!(approx(b.h, Face::Hanken700.glyph_box(13.0)));
    }

    #[test]
    fn intro_glow_is_empty_at_rest_and_stays_inside_the_capsule() {
        let cap = VRect::new(100.0, 10.0, 64.0, 10.0);
        assert!(intro_glow_ops(cap, 0.0).is_empty());
        let ops = intro_glow_ops(cap, 1.0);
        assert!(!ops.is_empty());
        for op in &ops {
            if let DrawOp::StrokeRRect { rect, .. } = op {
                assert!(rect.x >= cap.x && rect.right() <= cap.right());
                assert!(rect.y >= cap.y && rect.bottom() <= cap.bottom());
            }
        }
    }

    #[test]
    fn transform_scales_about_the_centre_and_fades() {
        let ops = vec![DrawOp::FillRRect {
            rect: VRect::new(0.0, 0.0, 100.0, 40.0),
            radius: 10.0,
            color: tok::WHITE,
        }];
        let out = transform_ops(&ops, 50.0, 20.0, 0.5, 0.5, 0.0);
        match &out[0] {
            DrawOp::FillRRect {
                rect,
                radius,
                color,
            } => {
                assert!(approx(rect.x, 25.0) && approx(rect.w, 50.0));
                assert!(approx(rect.y, 10.0) && approx(rect.h, 20.0));
                assert!(approx(*radius, 5.0));
                assert!(approx(color.a, 0.5));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn wrap_truncates_with_an_ellipsis_at_the_line_limit() {
        let text =
            "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron";
        let lines = wrap_text(text, Face::Hanken600, 12.0, 90.0, 2);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with('…'));
        for line in &lines {
            assert!(text_width(Face::Hanken600, 12.0, line) <= 90.5);
        }
        assert_eq!(
            wrap_text("one\ntwo", Face::Hanken600, 12.0, 500.0, 5),
            vec!["one", "two"]
        );
        assert_eq!(
            wrap_text("short", Face::Hanken600, 12.0, 500.0, 1),
            vec!["short"]
        );
    }

    #[test]
    fn hint_catalog_resolves_the_windows_variant() {
        let stop = feature_hint("stop_and_send").expect("stop_and_send");
        assert!(stop.body.contains("Ctrl+Shift+Space"));
        assert!(feature_hint("apple_shortcuts").is_none(), "macOS-only hint");
        assert!(
            feature_hint("screenshot_prompt").is_none(),
            "bespoke container"
        );
        assert_eq!(
            feature_hint("cancel_saved").unwrap().title,
            "Saved to History"
        );
        assert!(feature_hint("_comment").is_none());
    }

    #[test]
    fn bloom_wav_is_a_well_formed_2550ms_mono_wav() {
        let wav = bloom_wav();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(&wav[36..40], b"data");
        let frames = (44_100.0f64 * 2.55) as usize;
        assert_eq!(wav.len(), 44 + frames * 2);
        let payload = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]) as usize;
        assert_eq!(payload, wav.len() - 44);
        // Quiet by design: peak gain is 0.05 of full scale.
        let peak = wav[44..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
            .max()
            .unwrap();
        assert!(peak > 100 && f64::from(peak) < 0.06 * f64::from(i16::MAX));
    }

    #[test]
    fn button_regions_round_trip() {
        for b in BUTTONS {
            assert_eq!(ButtonId::from_region(b.region()), Some(b));
        }
        assert_eq!(ButtonId::from_region(1), None);
    }
}
