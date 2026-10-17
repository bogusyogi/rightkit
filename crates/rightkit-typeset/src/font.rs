//! Font metrics read from the font file itself (TrueType / OpenType `sfnt`).
//!
//! Read real advances to measure text in caller-provided faces: `head.unitsPerEm`,
//! `hhea.numberOfHMetrics`, `hmtx` advances & `cmap` (formats 4 & 12).
//! No dependency: the tables read are small and fixed.
//!
//! Not covered: kerning and ligatures (`GPOS`/`kern`), complex-script shaping, font
//! collections beyond their first face, and cmap formats other than 4 and 12.

use std::fmt;

/// Why a file is not a font this parser reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontError {
    /// Not an sfnt file, or a table lies outside the file.
    Malformed(String),
    /// A table this parser needs is absent.
    MissingTable(&'static str),
    /// No cmap subtable in a supported format.
    NoUsableCmap,
}

impl fmt::Display for FontError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FontError::Malformed(m) => write!(f, "not a readable font: {m}"),
            FontError::MissingTable(t) => write!(f, "font has no {t} table"),
            FontError::NoUsableCmap => f.write_str("font has no cmap subtable in format 4 or 12"),
        }
    }
}

impl std::error::Error for FontError {}

/// The width of a text in font units, with the characters the font has no glyph for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measurement {
    pub units_per_em: u16,
    /// Sum of the advances, in font units. A missing character counts the `.notdef`
    /// advance, as a renderer would draw it.
    pub advance_units: u64,
    /// Characters with no glyph (each listed once, in order of first appearance).
    pub missing: Vec<char>,
}

impl Measurement {
    /// Width in pixels at `font_px`, rounded to the nearest pixel.
    pub fn width_px(&self, font_px: u32) -> u64 {
        let upem = u64::from(self.units_per_em.max(1));
        (self.advance_units * u64::from(font_px) + upem / 2) / upem
    }
}

/// A parsed face. Owns the file bytes; every lookup reads them in place.
#[derive(Debug, Clone)]
pub struct FontFace {
    data: Vec<u8>,
    units_per_em: u16,
    number_of_hmetrics: u16,
    hmtx: usize,
    num_glyphs: u32,
    cmap_offset: usize,
    cmap_format: u16,
}

/// Reject a format-12 subtable whose groups are inverted or map past `numGlyphs` (or overflow).
fn validate_format12(d: &[u8], t: usize, num_glyphs: u32) -> Result<(), FontError> {
    let bad = |m: &str| FontError::Malformed(format!("cmap format 12: {m}"));
    let groups = u32_at(d, t + 12)? as usize;
    for i in 0..groups {
        let g = i
            .checked_mul(12)
            .and_then(|o| o.checked_add(t + 16))
            .ok_or_else(|| bad("group table overflows"))?;
        let (start, end, first) = (u32_at(d, g)?, u32_at(d, g + 4)?, u32_at(d, g + 8)?);
        if start > end {
            return Err(bad("group start is after its end"));
        }
        let last = first
            .checked_add(end - start)
            .ok_or_else(|| bad("group glyph ids overflow"))?;
        if last >= num_glyphs {
            return Err(bad("group maps past numGlyphs"));
        }
    }
    Ok(())
}

fn u16_at(d: &[u8], at: usize) -> Result<u16, FontError> {
    d.get(at..at + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or_else(|| FontError::Malformed(format!("read past the end at byte {at}")))
}

fn u32_at(d: &[u8], at: usize) -> Result<u32, FontError> {
    d.get(at..at + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| FontError::Malformed(format!("read past the end at byte {at}")))
}

impl FontFace {
    /// Parses the first face of `data`.
    pub fn parse(data: Vec<u8>) -> Result<Self, FontError> {
        let d = &data[..];
        let mut base = 0usize;
        let mut magic = u32_at(d, 0)?;
        if magic == 0x7474_6366 {
            // 'ttcf': a collection; use its first face.
            base = u32_at(d, 12)? as usize;
            magic = u32_at(d, base)?;
        }
        if !matches!(magic, 0x0001_0000 | 0x7472_7565 | 0x4F54_544F) {
            return Err(FontError::Malformed("unknown sfnt version".into()));
        }
        let num_tables = usize::from(u16_at(d, base + 4)?);
        let table = |tag: &'static [u8; 4]| -> Result<Option<(usize, usize)>, FontError> {
            for i in 0..num_tables {
                let rec = base + 12 + i * 16;
                let name = d
                    .get(rec..rec + 4)
                    .ok_or_else(|| FontError::Malformed("table directory is cut short".into()))?;
                if name == tag {
                    let off = u32_at(d, rec + 8)? as usize;
                    let len = u32_at(d, rec + 12)? as usize;
                    if off.checked_add(len).is_none_or(|end| end > d.len()) {
                        return Err(FontError::Malformed(format!(
                            "table {} lies outside the file",
                            String::from_utf8_lossy(tag)
                        )));
                    }
                    return Ok(Some((off, len)));
                }
            }
            Ok(None)
        };
        let (head, head_len) = table(b"head")?.ok_or(FontError::MissingTable("head"))?;
        let (hhea, hhea_len) = table(b"hhea")?.ok_or(FontError::MissingTable("hhea"))?;
        let (hmtx, _) = table(b"hmtx")?.ok_or(FontError::MissingTable("hmtx"))?;
        let (cmap, cmap_len) = table(b"cmap")?.ok_or(FontError::MissingTable("cmap"))?;
        if head_len < 20 || hhea_len < 36 {
            return Err(FontError::Malformed(
                "head or hhea table is too short".into(),
            ));
        }
        let units_per_em = u16_at(d, head + 18)?;
        if units_per_em == 0 {
            return Err(FontError::Malformed("unitsPerEm is zero".into()));
        }
        let number_of_hmetrics = u16_at(d, hhea + 34)?;
        if number_of_hmetrics == 0 {
            return Err(FontError::Malformed("numberOfHMetrics is zero".into()));
        }
        let num_glyphs = match table(b"maxp")? {
            Some((maxp, len)) if len >= 6 => u32::from(u16_at(d, maxp + 4)?),
            _ => u32::from(number_of_hmetrics),
        };
        // Prefer a full-Unicode subtable (format 12), then the BMP one (format 4).
        let count = usize::from(u16_at(d, cmap + 2)?);
        let mut best: Option<(u8, usize, u16)> = None;
        for i in 0..count {
            let rec = cmap + 4 + i * 8;
            let platform = u16_at(d, rec)?;
            let encoding = u16_at(d, rec + 2)?;
            let off = cmap + u32_at(d, rec + 4)? as usize;
            if off + 2 > cmap + cmap_len {
                continue;
            }
            let format = u16_at(d, off)?;
            let unicode = matches!((platform, encoding), (0, _) | (3, 1) | (3, 10));
            if !unicode || !matches!(format, 4 | 12) {
                continue;
            }
            let rank = if format == 12 { 2 } else { 1 };
            if best.is_none_or(|(r, _, _)| rank > r) {
                best = Some((rank, off, format));
            }
        }
        let (_, cmap_offset, cmap_format) = best.ok_or(FontError::NoUsableCmap)?;
        if cmap_format == 12 {
            validate_format12(d, cmap_offset, num_glyphs)?;
        }
        Ok(Self {
            data,
            units_per_em,
            number_of_hmetrics,
            hmtx,
            num_glyphs,
            cmap_offset,
            cmap_format,
        })
    }

    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// The glyph id for `c`; `0` (`.notdef`) when the font has none.
    pub fn glyph_id(&self, c: char) -> u32 {
        self.lookup(u32::from(c)).unwrap_or(0)
    }

    fn lookup(&self, c: u32) -> Result<u32, FontError> {
        let d = &self.data[..];
        let t = self.cmap_offset;
        let glyph = if self.cmap_format == 12 {
            let groups = u32_at(d, t + 12)? as usize;
            let (mut lo, mut hi) = (0usize, groups);
            let mut found = 0;
            while lo < hi {
                let mid = (lo + hi) / 2;
                let g = t + 16 + mid * 12;
                let (start, end, first) = (u32_at(d, g)?, u32_at(d, g + 4)?, u32_at(d, g + 8)?);
                if c < start {
                    hi = mid;
                } else if c > end {
                    lo = mid + 1;
                } else {
                    found = first
                        .checked_add(c - start)
                        .ok_or_else(|| FontError::Malformed("cmap glyph id overflow".into()))?;
                    break;
                }
            }
            found
        } else {
            if c > 0xFFFF {
                return Ok(0);
            }
            let seg_x2 = usize::from(u16_at(d, t + 6)?);
            let seg_count = seg_x2 / 2;
            let ends = t + 14;
            let starts = ends + seg_x2 + 2;
            let deltas = starts + seg_x2;
            let ranges = deltas + seg_x2;
            let mut found = 0u32;
            for i in 0..seg_count {
                let end = u32::from(u16_at(d, ends + i * 2)?);
                if end < c {
                    continue;
                }
                let start = u32::from(u16_at(d, starts + i * 2)?);
                if start > c {
                    break;
                }
                let delta = u32::from(u16_at(d, deltas + i * 2)?);
                let range_off = usize::from(u16_at(d, ranges + i * 2)?);
                found = if range_off == 0 {
                    (c + delta) & 0xFFFF
                } else {
                    let at = ranges + i * 2 + range_off + 2 * (c - start) as usize;
                    match u32::from(u16_at(d, at)?) {
                        0 => 0,
                        g => (g + delta) & 0xFFFF,
                    }
                };
                break;
            }
            found
        };
        Ok(glyph)
    }

    /// Advance of `glyph` in font units. Glyphs past `numberOfHMetrics` share the last advance.
    pub fn advance_units(&self, glyph: u32) -> Result<u16, FontError> {
        if glyph >= self.num_glyphs.max(1) {
            return Ok(0);
        }
        let index = glyph.min(u32::from(self.number_of_hmetrics) - 1) as usize;
        u16_at(&self.data, self.hmtx + index * 4)
    }

    /// Width of `text` with this face (no kerning). Control characters count nothing.
    pub fn measure(&self, text: &str) -> Result<Measurement, FontError> {
        let mut advance_units = 0u64;
        let mut missing = Vec::new();
        for c in text.chars().filter(|c| !c.is_control()) {
            let glyph = self.glyph_id(c);
            if glyph == 0 && !missing.contains(&c) {
                missing.push(c);
            }
            advance_units += u64::from(self.advance_units(glyph)?);
        }
        Ok(Measurement {
            units_per_em: self.units_per_em,
            advance_units,
            missing,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be16(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_be_bytes());
    }

    fn be32(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_be_bytes());
    }

    /// A hand-built proportional face: glyph 0 `.notdef` 500, `i` 250, `M` 900 (upem 1000),
    /// with a format 4 cmap. The shipped monospace face cannot show that advances differ
    /// per glyph, and no end-to-end scenario can reach a proportional face without one.
    fn proportional_font() -> Vec<u8> {
        let mut head = vec![0u8; 54];
        head[18..20].copy_from_slice(&1000u16.to_be_bytes());
        let mut hhea = vec![0u8; 36];
        hhea[34..36].copy_from_slice(&3u16.to_be_bytes());
        let mut maxp = vec![0u8; 6];
        maxp[4..6].copy_from_slice(&3u16.to_be_bytes());
        let mut hmtx = Vec::new();
        for adv in [500u16, 250, 900] {
            be16(&mut hmtx, adv);
            be16(&mut hmtx, 0);
        }
        let mut sub = Vec::new();
        be16(&mut sub, 4); // format
        be16(&mut sub, 0); // length (unused by the reader)
        be16(&mut sub, 0); // language
        be16(&mut sub, 6); // segCountX2 (3 segments)
        be16(&mut sub, 0);
        be16(&mut sub, 0);
        be16(&mut sub, 0);
        for end in [0x4Du16, 0x69, 0xFFFF] {
            be16(&mut sub, end);
        }
        be16(&mut sub, 0); // reservedPad
        for start in [0x4Du16, 0x69, 0xFFFF] {
            be16(&mut sub, start);
        }
        for delta in [2u16.wrapping_sub(0x4D), 1u16.wrapping_sub(0x69), 1] {
            be16(&mut sub, delta);
        }
        for _ in 0..3 {
            be16(&mut sub, 0);
        }
        let mut cmap = Vec::new();
        be16(&mut cmap, 0);
        be16(&mut cmap, 1);
        be16(&mut cmap, 3);
        be16(&mut cmap, 1);
        be32(&mut cmap, 12);
        cmap.extend_from_slice(&sub);

        let tables: [(&[u8; 4], Vec<u8>); 5] = [
            (b"cmap", cmap),
            (b"head", head),
            (b"hhea", hhea),
            (b"hmtx", hmtx),
            (b"maxp", maxp),
        ];
        let mut out = Vec::new();
        be32(&mut out, 0x0001_0000);
        be16(&mut out, tables.len() as u16);
        out.extend_from_slice(&[0u8; 6]);
        let mut offset = 12 + tables.len() * 16;
        for (tag, body) in &tables {
            out.extend_from_slice(*tag);
            be32(&mut out, 0);
            be32(&mut out, offset as u32);
            be32(&mut out, body.len() as u32);
            offset += body.len();
        }
        for (_, body) in &tables {
            out.extend_from_slice(body);
        }
        out
    }

    #[test]
    fn advances_follow_each_glyph() {
        let face = FontFace::parse(proportional_font()).unwrap();
        let wide = face.measure("MMMM").unwrap();
        let narrow = face.measure("iiii").unwrap();
        assert_eq!((wide.advance_units, narrow.advance_units), (3600, 1000));
        assert_eq!(face.measure("Mi").unwrap().width_px(100), 115);
        let unknown = face.measure("Mx").unwrap();
        assert_eq!(unknown.advance_units, 900 + 500);
        assert_eq!(unknown.missing, vec!['x']);
    }

    #[test]
    fn format12_group_with_overflowing_glyph_ids_is_refused() {
        let mut bytes = proportional_font();
        // Rebuild the cmap as a format 12 subtable with one group: 'a'..='b' -> u32::MAX.
        let mut sub = Vec::new();
        be16(&mut sub, 12);
        be16(&mut sub, 0);
        be32(&mut sub, 28);
        be32(&mut sub, 0);
        be32(&mut sub, 1);
        be32(&mut sub, 0x61);
        be32(&mut sub, 0x62);
        be32(&mut sub, u32::MAX);
        let sub_at = 12 + 5 * 16 + 12; // cmap table start + header (4) + one record (8)
        let cmap_at = 12 + 5 * 16;
        assert_eq!(&bytes[sub_at..sub_at + 2], &4u16.to_be_bytes());
        // Replace the format-4 body in place; the later tables only need to stay addressable,
        // and the new subtable is no longer than the old one.
        assert!(sub.len() <= 14 + 6 * 4 + 6 * 2 + 4);
        bytes[sub_at..sub_at + sub.len()].copy_from_slice(&sub);
        let _ = cmap_at;
        match FontFace::parse(bytes) {
            Err(FontError::Malformed(m)) => assert!(m.contains("format 12"), "{m}"),
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    #[test]
    fn truncated_files_are_refused_not_read_past() {
        let mut bytes = proportional_font();
        bytes.truncate(bytes.len() - 30);
        assert!(FontFace::parse(bytes).is_err());
        assert!(FontFace::parse(b"not a font".to_vec()).is_err());
    }
}
