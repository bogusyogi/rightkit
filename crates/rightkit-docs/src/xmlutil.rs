//! XML helpers shared by the DOCX and XLSX layers: a depth-capped event walker that refuses
//! DOCTYPE, escaping, relationship parsing, and A1 cell addressing.

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::error::{DocsError, Result};

pub(crate) const MAX_XML_DEPTH: usize = 256;

/// An event walker over one part. It reports each event with its byte range in the source, so an
/// edit can patch exact ranges and leave every other byte alone.
pub(crate) struct Walk<'a> {
    reader: Reader<&'a [u8]>,
    depth: usize,
    part: String,
}

impl<'a> Walk<'a> {
    pub fn new(bytes: &'a [u8], part: &str) -> Self {
        let mut reader = Reader::from_reader(bytes);
        reader.config_mut().check_end_names = true;
        Self {
            reader,
            depth: 0,
            part: part.to_string(),
        }
    }

    /// The next event and its `[start, end)` byte range.
    pub fn next(&mut self) -> Result<(Event<'a>, usize, usize)> {
        let start = self.reader.buffer_position() as usize;
        let event = self
            .reader
            .read_event()
            .map_err(|e| DocsError::Malformed(format!("{}: {e}", self.part)))?;
        let end = self.reader.buffer_position() as usize;
        match &event {
            Event::DocType(_) => {
                return Err(DocsError::ForbiddenXml {
                    part: self.part.clone(),
                    what: "DOCTYPE",
                })
            }
            Event::Start(_) => {
                self.depth += 1;
                if self.depth > MAX_XML_DEPTH {
                    return Err(DocsError::Malformed(format!(
                        "{}: nesting deeper than {MAX_XML_DEPTH}",
                        self.part
                    )));
                }
            }
            Event::End(_) => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
        Ok((event, start, end))
    }
}

/// Local name without a namespace prefix (`w:p` -> `p`), as bytes so callers keep matching on
/// `b"p"` patterns. quick-xml 0.42 hands names out as `&str` (`QName: AsRef<str>`), so every
/// `local(e.name().as_ref())` call site passes a `&str` here and this is the one conversion point.
/// `:` is ASCII, so splitting the bytes never lands inside a multi-byte character.
pub(crate) fn local(name: &str) -> &[u8] {
    let name = name.as_bytes();
    match name.iter().rposition(|b| *b == b':') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

/// The name prefix including its colon (`w:p` -> `w:`), empty when there is none.
pub(crate) fn prefix_of(name: &str) -> String {
    match name.rfind(':') {
        Some(i) => name[..=i].to_owned(),
        None => String::new(),
    }
}

/// An attribute by local name (`val` matches `w:val`).
pub(crate) fn attr(e: &BytesStart<'_>, key: &str) -> Option<String> {
    for a in e.attributes().with_checks(false).flatten() {
        if local(a.key.as_ref()) == key.as_bytes() {
            return a
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned());
        }
    }
    None
}

/// Characters XML 1.0 cannot carry are dropped rather than written.
pub(crate) fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| {
            matches!(*c as u32, 0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)
        })
        .collect()
}

pub(crate) fn esc_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in clean(s).chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

pub(crate) fn esc_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in clean(s).chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            _ => out.push(c),
        }
    }
    out
}

/// Cheap pre-parse refusal for XML parts: a DOCTYPE or an entity declaration anywhere, or a UTF-16
/// byte-order mark (which would hide either from a byte scan and which no Office writer produces).
pub(crate) fn forbidden_xml(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return Some("UTF-16 encoded XML");
    }
    let mut i = 0;
    while let Some(p) = bytes[i..].iter().position(|b| *b == b'<') {
        let at = i + p;
        let rest = &bytes[at..];
        if rest.starts_with(b"<!DOCTYPE") {
            return Some("DOCTYPE");
        }
        if rest.starts_with(b"<!ENTITY") {
            return Some("ENTITY declaration");
        }
        i = at + 1;
    }
    None
}

#[derive(Clone, Debug)]
pub(crate) struct Rel {
    pub id: String,
    pub ty: String,
    pub target: String,
    pub external: bool,
}

pub(crate) fn parse_rels(bytes: &[u8], part: &str) -> Result<Vec<Rel>> {
    let mut walk = Walk::new(bytes, part);
    let mut out = Vec::new();
    loop {
        let (ev, _, _) = walk.next()?;
        match ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) if local(e.name().as_ref()) == b"Relationship" => {
                out.push(Rel {
                    id: attr(&e, "Id").unwrap_or_default(),
                    ty: attr(&e, "Type").unwrap_or_default(),
                    target: attr(&e, "Target").unwrap_or_default(),
                    external: attr(&e, "TargetMode")
                        .map(|m| m.eq_ignore_ascii_case("External"))
                        .unwrap_or(false),
                });
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Resolve a relationship target against the part that owns the relationships. `base_dir` is the
/// directory of the owning part (`word`, `xl`, or empty for the package root).
pub(crate) fn resolve_target(base_dir: &str, target: &str) -> String {
    let mut parts: Vec<&str> = if let Some(t) = target.strip_prefix('/') {
        return normalize(t.split('/'));
    } else {
        base_dir.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn normalize<'a>(segs: impl Iterator<Item = &'a str>) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in segs {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

pub(crate) fn dir_of(part: &str) -> &str {
    part.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

pub(crate) fn rels_name_for(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((d, f)) => format!("{d}/_rels/{f}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

// ---- A1 addressing ---------------------------------------------------------------------------

/// `1 -> A`, `27 -> AA`.
pub(crate) fn col_letters(mut col: u32) -> String {
    let mut s = Vec::new();
    while col > 0 {
        let rem = ((col - 1) % 26) as u8;
        s.push(b'A' + rem);
        col = (col - 1) / 26;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

pub(crate) const MAX_ROW: u32 = 1_048_576;
pub(crate) const MAX_COL: u32 = 16_384;

/// `"B7"` -> `(row 7, col 2)`, both 1-based. `$` markers are accepted.
pub(crate) fn parse_cell(s: &str) -> Option<(u32, u32)> {
    let s = s.trim().replace('$', "");
    let split = s.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = s.split_at(split);
    if letters.is_empty() || !letters.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut col: u32 = 0;
    for c in letters.chars() {
        col = col
            .checked_mul(26)?
            .checked_add((c.to_ascii_uppercase() as u8 - b'A') as u32 + 1)?;
    }
    let row: u32 = digits.parse().ok()?;
    if row == 0 || row > MAX_ROW || col == 0 || col > MAX_COL {
        return None;
    }
    Some((row, col))
}

pub(crate) fn cell_name(row: u32, col: u32) -> String {
    format!("{}{}", col_letters(col), row)
}

/// `"A1:C10"` or `"B2"` -> `(r1, c1, r2, c2)` normalised so r1<=r2 and c1<=c2.
pub(crate) fn parse_range(s: &str) -> Option<(u32, u32, u32, u32)> {
    let (a, b) = match s.split_once(':') {
        Some((a, b)) => (a, b),
        None => (s, s),
    };
    let (r1, c1) = parse_cell(a)?;
    let (r2, c2) = parse_cell(b)?;
    Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
}
