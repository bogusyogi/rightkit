//! Font slots and text measurement.
//!
//! The host's painters declare five font slots ([`Fonts`], returned by `Painters::fonts`); the
//! crate registers their bytes with the OS (Windows backend) and measures text against them, so
//! the widths a painter lays out with are the widths the renderer draws. Which typeface sits in a
//! slot, and what the slots are used for, is the painter's decision.

#![allow(dead_code)]

use std::sync::OnceLock;

use crate::config::bound::MAX_TEXT_RUN_BYTES;

/// Most bytes [`wrap_text`] reads from one text. Longer text is cut here before any measuring, so
/// the work of one wrap never grows with the input.
pub const MAX_WRAP_INPUT_BYTES: usize = 65_536;

/// `s` cut to at most `max` bytes at a character boundary (never inside a multi-byte character).
/// A string already within `max` is returned unchanged.
pub fn cut_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// One font slot: the TrueType bytes (also used for text measurement), the names GDI and
/// DirectWrite know it by, and its vertical metrics in em.
#[derive(Clone, Copy, Debug)]
pub struct FaceSpec {
    /// TrueType bytes registered privately in this process. Empty means "use the system font
    /// named `gdi_family`"; text measurement then falls back to a width estimate.
    pub bytes: &'static [u8],
    /// Legacy (GDI, name-id 1) family name.
    pub gdi_family: &'static str,
    /// GDI weight that selects this face inside `gdi_family`.
    pub gdi_weight: u16,
    /// Typographic (name-id 16) family, for backends that load fonts into a private collection.
    pub typographic_family: &'static str,
    pub typographic_weight: u16,
    /// hhea ascender, descender magnitude and line gap, in em.
    pub ascent_em: f32,
    pub descent_em: f32,
    pub gap_em: f32,
}

impl FaceSpec {
    /// The slot before the host's fonts are installed: no bytes, no family, zero metrics.
    pub const UNINSTALLED: FaceSpec = FaceSpec {
        bytes: &[],
        gdi_family: "",
        gdi_weight: 0,
        typographic_family: "",
        typographic_weight: 0,
        ascent_em: 0.0,
        descent_em: 0.0,
        gap_em: 0.0,
    };

    /// A system font slot: no bytes, the named family, estimated metrics.
    pub const fn system(family: &'static str, weight: u16) -> FaceSpec {
        FaceSpec {
            bytes: &[],
            gdi_family: family,
            gdi_weight: weight,
            typographic_family: family,
            typographic_weight: weight,
            ascent_em: 1.0,
            descent_em: 0.303,
            gap_em: 0.0,
        }
    }
}

/// Slots in order: body 400, body 500, body 600, body 700, display.
#[derive(Clone, Copy, Debug)]
pub struct Fonts {
    pub faces: [FaceSpec; 5],
}

static FONTS: OnceLock<Fonts> = OnceLock::new();

/// Record the painters' fonts (called once by `config::install`).
pub(crate) fn install_fonts(fonts: Fonts) {
    let _ = FONTS.set(fonts);
}

fn installed() -> Option<&'static Fonts> {
    FONTS.get()
}

// ---------------------------------------------------------------------------------------------
// Fonts and text metrics
// ---------------------------------------------------------------------------------------------

/// The five font slots a painter draws with: three body weights, one bold body weight and one
/// display face. The face behind each slot is `Fonts::faces`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    /// Body, regular weight.
    Body400,
    /// Body, medium weight (the default body weight).
    Body500,
    /// Body, semi-bold.
    Body600,
    /// Body, bold.
    Body700,
    /// Display face (the recording timer).
    Display,
}

pub const ALL_FACES: [Face; 5] = [
    Face::Body400,
    Face::Body500,
    Face::Body600,
    Face::Body700,
    Face::Display,
];

impl Face {
    fn index(self) -> usize {
        match self {
            Face::Body400 => 0,
            Face::Body500 => 1,
            Face::Body600 => 2,
            Face::Body700 => 3,
            Face::Display => 4,
        }
    }

    fn spec(self) -> FaceSpec {
        match installed() {
            Some(fonts) => fonts.faces[self.index()],
            // Nothing draws before `config::install`; the empty family names no typeface.
            None => FaceSpec::UNINSTALLED,
        }
    }

    /// Raw TrueType bytes (also handed to GDI as a private in-process font by `gfx.rs`).
    pub fn bytes(self) -> &'static [u8] {
        self.spec().bytes
    }

    /// Legacy (GDI, name-id 1) family name of the face.
    pub fn gdi_family(self) -> &'static str {
        self.spec().gdi_family
    }

    /// GDI weight that selects this face inside [`Face::gdi_family`].
    pub fn gdi_weight(self) -> u16 {
        self.spec().gdi_weight
    }

    /// Typographic (DirectWrite, name-id 16) family and weight, for backends that load the
    /// fonts into a private collection.
    pub fn typographic_family(self) -> &'static str {
        self.spec().typographic_family
    }

    pub fn typographic_weight(self) -> u16 {
        self.spec().typographic_weight
    }

    /// hhea ascender in em.
    pub fn ascent_em(self) -> f32 {
        self.spec().ascent_em
    }

    /// hhea descender magnitude in em.
    pub fn descent_em(self) -> f32 {
        self.spec().descent_em
    }

    /// hhea lineGap in em.
    pub fn gap_em(self) -> f32 {
        self.spec().gap_em
    }

    /// Line height: ascender + descender + line gap.
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
        // The end codes of a format-4 subtable ascend, so the segment holding `code` is the first
        // whose end is not below it: a binary search, O(log segments) per character instead of a
        // scan of every segment.
        let (mut low, mut high) = (0usize, seg);
        while low < high {
            let mid = low + (high - low) / 2;
            match be16(d, end_off + 2 * mid) {
                Some(end) if end < code => low = mid + 1,
                Some(_) => high = mid,
                None => return 0,
            }
        }
        if low >= seg {
            return 0;
        }
        let i = low;
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
        if g == 0 {
            0
        } else {
            g.wrapping_add(delta)
        }
    }

    fn advance(&self, glyph: u16) -> f32 {
        let idx = usize::from(glyph).min(self.num_hmetrics - 1);
        f32::from(be16(self.data, self.hmtx + 4 * idx).unwrap_or(0))
    }
}

fn font_tables(face: Face) -> Option<&'static FontTables> {
    static TABLES: OnceLock<[Option<FontTables>; 5]> = OnceLock::new();
    // Nothing is parsed (or cached) before the painters' fonts are installed.
    installed()?;
    TABLES.get_or_init(|| {
        [
            parse_font(ALL_FACES[0].bytes()),
            parse_font(ALL_FACES[1].bytes()),
            parse_font(ALL_FACES[2].bytes()),
            parse_font(ALL_FACES[3].bytes()),
            parse_font(ALL_FACES[4].bytes()),
        ]
    })[face.index()]
    .as_ref()
}

/// Width of text in one face and size, measured in font units so widths add exactly: the width
/// of `a` followed by `b` is the width of `a` plus the width of `b` (the crate applies no
/// kerning). Wrapping and truncation use that to measure each character once instead of
/// re-measuring a growing string.
struct Measure {
    tables: Option<&'static FontTables>,
    size: f32,
}

impl Measure {
    fn new(face: Face, size: f32) -> Measure {
        Measure {
            tables: font_tables(face),
            size: if size.is_finite() { size } else { 0.0 },
        }
    }

    /// Width of one character in font units (one unit per character without font tables).
    fn unit(&self, ch: char) -> f32 {
        match self.tables {
            Some(t) => t.advance(t.glyph(ch)),
            None => 1.0,
        }
    }

    fn units(&self, text: &str) -> f32 {
        text.chars().map(|ch| self.unit(ch)).sum()
    }

    /// Width of `text` in font units, or `None` as soon as `fits(width so far)` is false. Prefix
    /// widths never shrink, so the scan of a very long word ends at the first character past the
    /// line instead of measuring the whole word.
    fn units_within(&self, text: &str, fits: impl Fn(f32) -> bool) -> Option<f32> {
        let mut total = 0.0f32;
        for ch in text.chars() {
            total += self.unit(ch);
            if !fits(total) {
                return None;
            }
        }
        Some(total)
    }

    /// Font units to points.
    fn px(&self, units: f32) -> f32 {
        match self.tables {
            Some(t) => units / t.upem * self.size,
            None => units * self.size * 0.55,
        }
    }
}

/// Advance width of `text` in `size`-point `face` (sum of `hmtx` advances; no kerning). At most
/// `bound::MAX_TEXT_RUN_BYTES` bytes are measured (the same cut a drawn text run gets), so the
/// work never grows with the length of `text`.
pub fn text_width(face: Face, size: f32, text: &str) -> f32 {
    let measure = Measure::new(face, size);
    measure.px(measure.units(cut_bytes(text, MAX_TEXT_RUN_BYTES)))
}

/// Greedy word wrap with tail truncation when `max_lines` is exceeded. At most
/// [`MAX_WRAP_INPUT_BYTES`] bytes of `text` are read. Work is linear in the text that is actually
/// laid out: each character is measured once, a word is measured only until it can no longer fit
/// on one line (a very long word is not scanned to its end to learn that), and wrapping stops as
/// soon as one line more than `max_lines` exists.
pub fn wrap_text(text: &str, face: Face, size: f32, max_w: f32, max_lines: usize) -> Vec<String> {
    let text = cut_bytes(text, MAX_WRAP_INPUT_BYTES);
    let max_lines = max_lines.max(1);
    let measure = Measure::new(face, size);
    let space = measure.unit(' ');
    let fits = |units: f32| measure.px(units) <= max_w;
    let mut lines: Vec<String> = Vec::new();
    'paragraphs: for paragraph in text.split('\n') {
        let mut current = String::new();
        let mut current_units = 0.0f32;
        for word in paragraph.split_whitespace() {
            // `Some` only when the word alone fits one line; a wider word goes straight to the
            // character break below without being measured to its end.
            let joined = measure.units_within(word, fits).map(|word_units| {
                if current.is_empty() {
                    word_units
                } else {
                    current_units + space + word_units
                }
            });
            if let Some(joined) = joined.filter(|units| fits(*units)) {
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(word);
                current_units = joined;
                continue;
            }
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_units = 0.0;
                if lines.len() > max_lines {
                    break 'paragraphs;
                }
            }
            // A single word wider than the line breaks by character.
            let mut chunk = String::new();
            let mut chunk_units = 0.0f32;
            for ch in word.chars() {
                let next_units = chunk_units + measure.unit(ch);
                if !chunk.is_empty() && !fits(next_units) {
                    lines.push(std::mem::take(&mut chunk));
                    if lines.len() > max_lines {
                        break 'paragraphs;
                    }
                    chunk.push(ch);
                    chunk_units = measure.unit(ch);
                } else {
                    chunk.push(ch);
                    chunk_units = next_units;
                }
            }
            current = chunk;
            current_units = chunk_units;
        }
        lines.push(current);
        if lines.len() > max_lines {
            break;
        }
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        let last = lines.pop().unwrap_or_default();
        lines.push(ellipsize(&format!("{last}…"), face, size, max_w));
    }
    lines
}

/// Trim `text` from the tail until it (plus a trailing `…`) fits in `max_w`. At most
/// `bound::MAX_TEXT_RUN_BYTES` bytes of `text` are read, so the work never grows with its length.
pub fn ellipsize(text: &str, face: Face, size: f32, max_w: f32) -> String {
    let text = cut_bytes(text, MAX_TEXT_RUN_BYTES);
    let measure = Measure::new(face, size);
    if measure.px(measure.units(text)) <= max_w {
        return text.to_string();
    }
    let body = text.trim_end_matches('…');
    let ellipsis = measure.unit('…');
    let fits = |units: f32| measure.px(units + ellipsis) <= max_w;
    // The longest prefix of at most `chars - 1` characters whose trailing whitespace is trimmed
    // and which still fits with the ellipsis. Prefix widths only grow, so the first prefix that
    // does not fit ends the search.
    let chars = body.chars().count();
    let mut best_end = if fits(0.0) { Some(0usize) } else { None };
    let mut accumulated = 0.0f32;
    let mut trimmed_units = 0.0f32;
    let mut trimmed_end = 0usize;
    for (taken, (index, ch)) in body.char_indices().enumerate() {
        if taken + 1 >= chars {
            break;
        }
        accumulated += measure.unit(ch);
        if !ch.is_whitespace() {
            trimmed_units = accumulated;
            trimmed_end = index + ch.len_utf8();
        }
        if fits(trimmed_units) {
            best_end = Some(trimmed_end);
        } else {
            break;
        }
    }
    match best_end {
        Some(end) => format!("{}…", &body[..end]),
        None => "…".to_string(),
    }
}
