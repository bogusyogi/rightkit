//! Authoritative PDF source graph extraction (Dispatch B, lane B1).
//!
//! Owns `docs/PDF-EDITOR-NOW.md` §5.2's stateful per-stream content-stream walker
//! (`visit_text_show`), §5.3's text-island grouping (`may_join`) and coarse painted-bounds
//! inventory, and the `#[tauri::command] inspect_pdf_page` entry point that assembles a
//! [`PdfPageGraph`] for one page.
//!
//! Every wire/model type this file builds against (`PdfPageGraph`, `PdfTextIsland`,
//! `PdfSourceLocator`, `PdfSourceSpan`, `Matrix`, `TextState`, `PositionedGlyph`,
//! `TextStateFont`, digest helpers, `PdfEditError`) is owned by `pdf_model.rs` — this file only
//! produces values of those types; it defines no new wire shape.
//!
//! Deliberate scope/judgment calls not fully pinned by §5.2/§5.3 (see final dispatch report for
//! the complete list):
//! - identity crosses Tauri as a plain `path: String` + `State<'_, PathBroker>`, authorized
//!   in-body via `commands::common::authorize_existing_for_read` — matching every existing
//!   command in this codebase (`pdf_text_edit.rs`), not the `AuthorizedPath`/`AppState` types
//!   sketched in §5.2's illustrative signature, which do not exist in this codebase;
//! - path-authorization failures map to [`PdfEditError::MalformedContent`] for lack of a
//!   dedicated "access denied" variant in the frozen §11 taxonomy;
//! - `quads` emits one device-space quad per glyph (not one bounding box per island), so the
//!   frontend can render exact per-glyph highlight geometry without loss;
//! - fill color is tracked only through `rg`/`g`/`k`; color-space (`cs`/`scn`) operators are out
//!   of scope for now and leave `fill_color` at its last plain-operator value;
//! - writing direction is always reported as `LeftToRight` per §7.3's LTR-only scope — vertical/
//!   RTL detection is not attempted;
//! - a `Tj`/`TJ`/`'`/`"` operand whose active font is unresolved, or whose active CTM/text
//!   matrix is singular, contributes a synthetic uneditable [`PdfTextIsland`] (empty text/spans,
//!   `editable: false`, typed `rejection`) rather than aborting the whole page;
//! - missing/partial `ToUnicode` on a CID font degrades to an empty `unicode` string per
//!   unmapped glyph (still geometrically positioned) rather than rejecting the run outright.

use std::collections::HashMap;
use std::rc::Rc;

use lopdf::content::Content;
use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::model::{
    compute_island_digest, document_sha256, expected_text_digest, sha256_hex, DecodedGlyph, Matrix,
    PaintState, PdfColor, PdfEditError, PdfPageGraph, PdfRect, PdfSourceLocator, PdfSourceSpan,
    PdfTextAppearance, PdfTextIsland, PositionedGlyph, SourceAddress, TextRenderMode, TextState,
    TextStateFont, WritingDirection,
};

/// Mirrors `pdf_text_edit.rs`'s own bound: plenty for real ToUnicode CMaps / simple-font
/// encodings while bounding malicious compressed font streams.
const MAX_FONT_ENCODING_BYTES: usize = 16 * 1024 * 1024;

// -------------------------------------------------------------------------------------------
// Byte entry point
// -------------------------------------------------------------------------------------------

/// Loads `bytes` as a PDF and walks page `page`, deriving the document digest itself.
///
/// This is the seam the app crate's `inspect_pdf_page` command calls, so the Tauri adapter never
/// needs `lopdf` in scope — it authorizes a path, reads it, and hands the bytes here.
pub fn page_graph_from_bytes(bytes: &[u8], page: u32) -> Result<PdfPageGraph, PdfEditError> {
    let doc = Document::load_mem(bytes).map_err(|_| PdfEditError::MalformedContent)?;
    let doc_sha256 = document_sha256(bytes);
    walk_page_graph(&doc, &doc_sha256, page)
}

// -------------------------------------------------------------------------------------------
// Page graph assembly
// -------------------------------------------------------------------------------------------

/// Core inspection logic, separated from the Tauri command so it is directly unit-testable
/// against an in-memory `Document` with no filesystem/broker involved (mirrors
/// `pdf_text_edit.rs`'s `apply_text_edits`/`edit_pdf_text` split).
pub fn walk_page_graph(
    doc: &Document,
    document_sha256: &str,
    page: u32,
) -> Result<PdfPageGraph, PdfEditError> {
    if doc.is_encrypted() {
        return Err(PdfEditError::EncryptedDocument);
    }

    let pages = doc.get_pages();
    let page_id = *pages.get(&page).ok_or(PdfEditError::MalformedContent)?;
    let page_obj = doc
        .get_object(page_id)
        .map_err(|_| PdfEditError::MalformedContent)?;
    let page_dict = page_obj
        .as_dict()
        .map_err(|_| PdfEditError::MalformedContent)?;

    let media_box = resolve_rect(doc, page_dict, b"MediaBox").unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let crop_box = resolve_rect(doc, page_dict, b"CropBox").unwrap_or(media_box);
    let rotation = resolve_rotation(doc, page_dict);

    let streams = page_content_streams(doc, page_id)?;
    let fonts = resolve_page_fonts(doc, page_id);

    let mut glyphs: Vec<PositionedGlyph> = Vec::new();
    let mut painted_bounds: Vec<PaintedBoundsEntry> = Vec::new();
    let mut rejections: Vec<PdfTextIsland> = Vec::new();

    for stream in &streams {
        let content = Content::decode(&stream.bytes).map_err(|_| PdfEditError::MalformedContent)?;
        walk_stream(
            stream,
            &content,
            page,
            document_sha256,
            &fonts,
            &mut glyphs,
            &mut painted_bounds,
            &mut rejections,
        );
    }

    let mut islands = group_islands(page, document_sha256, glyphs, &painted_bounds);
    islands.extend(rejections);

    Ok(PdfPageGraph {
        document_sha256: document_sha256.to_string(),
        page,
        media_box,
        crop_box,
        rotation,
        islands,
    })
}

/// One page `/Contents` entry, kept separate rather than concatenated (per §5.2: "Do not use
/// lopdf get_page_content as source identity because concatenation destroys stream boundaries
/// required for sparse writes").
struct PageStream {
    contents_index: u32,
    stream_ref: (u32, u16),
    bytes: Vec<u8>,
}

fn page_content_streams(
    doc: &Document,
    page_id: ObjectId,
) -> Result<Vec<PageStream>, PdfEditError> {
    let page_obj = doc
        .get_object(page_id)
        .map_err(|_| PdfEditError::MalformedContent)?;
    let page_dict = page_obj
        .as_dict()
        .map_err(|_| PdfEditError::MalformedContent)?;
    let contents = page_dict
        .get(b"Contents")
        .map_err(|_| PdfEditError::MalformedContent)?;

    let ids: Vec<ObjectId> = match contents {
        Object::Reference(id) => vec![*id],
        Object::Array(items) => items
            .iter()
            .filter_map(|o| match o {
                Object::Reference(id) => Some(*id),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };

    let mut streams = Vec::with_capacity(ids.len());
    for (idx, id) in ids.into_iter().enumerate() {
        let obj = doc
            .get_object(id)
            .map_err(|_| PdfEditError::MalformedContent)?;
        let stream = obj
            .as_stream()
            .map_err(|_| PdfEditError::MalformedContent)?;
        let bytes = stream
            .decompressed_content()
            .map_err(|_| PdfEditError::MalformedContent)?;
        streams.push(PageStream {
            contents_index: idx as u32,
            stream_ref: (id.0, id.1),
            bytes,
        });
    }
    Ok(streams)
}

// -------------------------------------------------------------------------------------------
// §5.2 — Page/crop box + rotation, with inheritance
// -------------------------------------------------------------------------------------------

fn resolve_inherited<'a>(
    doc: &'a Document,
    mut dict: &'a Dictionary,
    key: &[u8],
) -> Option<&'a Object> {
    loop {
        if let Ok(v) = dict.get(key) {
            return Some(v);
        }
        let parent = dict.get(b"Parent").ok()?;
        let parent_id = match parent {
            Object::Reference(id) => *id,
            _ => return None,
        };
        let parent_obj = doc.get_object(parent_id).ok()?;
        dict = parent_obj.as_dict().ok()?;
    }
}

fn deref_obj<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Object> {
    match obj {
        Object::Reference(id) => doc.get_object(*id).ok(),
        other => Some(other),
    }
}

fn object_number(obj: &Object) -> Option<f64> {
    match obj {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) => Some(*r as f64),
        _ => None,
    }
}

fn resolve_rect(doc: &Document, page_dict: &Dictionary, key: &[u8]) -> Option<[f64; 4]> {
    let obj = resolve_inherited(doc, page_dict, key)?;
    let obj = deref_obj(doc, obj)?;
    let arr = obj.as_array().ok()?;
    let nums: Vec<f64> = arr.iter().filter_map(object_number).collect();
    if nums.len() != 4 {
        return None;
    }
    Some([nums[0], nums[1], nums[2], nums[3]])
}

fn resolve_rotation(doc: &Document, page_dict: &Dictionary) -> i32 {
    resolve_inherited(doc, page_dict, b"Rotate")
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_i64().ok())
        .map(|r| (r as i32).rem_euclid(360))
        .unwrap_or(0)
}

// -------------------------------------------------------------------------------------------
// §7-adjacent — page font resolution into owned, 'static decode tables
//
// `TextState::font` is `Option<Rc<dyn TextStateFont>>` with no lifetime parameter, i.e. the
// trait object must be `'static`. lopdf's `Encoding<'a>` borrows from `Document`, so it cannot
// be stored directly on a long-lived font handle; instead every font is resolved ONCE per page,
// up front, into fully owned decode/width tables (`BuiltFont`), and the borrowed `Encoding`/
// `Document` reference is dropped before the content-stream walk begins.
// -------------------------------------------------------------------------------------------

/// Fully owned per-font decode/metrics table, built once per page from its `/Resources/Font`
/// dictionary. Implements [`TextStateFont`] so the walker can decode `Tj`/`TJ` operands and
/// compute advances without holding any borrow into `Document`.
struct BuiltFont {
    id: String,
    /// 1 for simple (Type1/TrueType/MMType1) fonts, 2 for Identity-H CID fonts. No other
    /// codespace is supported (§7.3: LTR-only, current scope).
    code_width: u8,
    default_width_1000: f64,
    widths_1000: HashMap<u32, f64>,
    /// Reject codes absent from an explicit or authoritative fallback table instead of emitting
    /// a guessed or zero-width editable quad.
    reject_missing_metrics: bool,
    unicode: HashMap<u32, String>,
}

impl std::fmt::Debug for BuiltFont {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltFont")
            .field("id", &self.id)
            .field("code_width", &self.code_width)
            .finish()
    }
}

impl TextStateFont for BuiltFont {
    fn id(&self) -> &str {
        &self.id
    }

    fn decode_show_operand(&self, operand: &Object) -> Result<Vec<DecodedGlyph>, PdfEditError> {
        let Object::String(bytes, _fmt) = operand else {
            return Err(PdfEditError::MalformedContent);
        };
        let width = self.code_width as usize;
        if width == 0 || bytes.len() % width != 0 {
            return Err(PdfEditError::MalformedContent);
        }

        let mut out = Vec::with_capacity(bytes.len() / width);
        for (i, chunk) in bytes.chunks_exact(width).enumerate() {
            let code = if width == 1 {
                chunk[0] as u32
            } else {
                u16::from_be_bytes([chunk[0], chunk[1]]) as u32
            };
            if self.reject_missing_metrics && !self.widths_1000.contains_key(&code) {
                return Err(PdfEditError::UnsupportedEncoding);
            }
            let start = (i * width) as u32;
            out.push(DecodedGlyph {
                cid: code,
                unicode: self.unicode.get(&code).cloned().unwrap_or_default(),
                source_range: (start, start + width as u32),
                bbox: None,
            });
        }
        Ok(out)
    }

    fn advance(&self, cid: u32, font_size: f64) -> f64 {
        let w0 = self
            .widths_1000
            .get(&cid)
            .copied()
            .unwrap_or(self.default_width_1000);
        w0 / 1000.0 * font_size
    }
}

/// Public seam for `pdf_compose.rs`'s `ExactOperandPatch` safety check (§9.1). Whether `text` can
/// be written verbatim into the *simple* (Type1/TrueType/MMType1) font resource `resource_name`'s
/// existing byte encoding, reusing this module's own `BuiltFont.unicode` decode table (the same
/// table [`walk_page_graph`] decodes `Tj`/`TJ` operands through) rather than assuming ASCII text
/// happens to equal the font's own byte codes — many embedded subset fonts remap codes via
/// `/Differences`, where that assumption silently produces wrong or blank glyphs instead of the
/// requested text (this is the reverse of what `BuiltFont::decode_show_operand` does forward).
/// Returns `None` for a 2-byte Identity-H CID resource (`ExactOperandPatch` never touches that
/// codespace — ambiguous byte-width splicing there is a `LocalTextObjectRewrite`-only concern) or
/// when any character of `text` has no byte in this font's encoding that decodes back to it.
#[cfg(feature = "edit")]
pub(crate) fn simple_font_identity_bytes(
    doc: &Document,
    page_id: ObjectId,
    resource_name: &[u8],
    text: &str,
) -> Option<Vec<u8>> {
    let fonts = resolve_page_fonts(doc, page_id);
    let font = fonts.get(resource_name)?;
    if font.code_width != 1 {
        return None;
    }
    let mut reverse: HashMap<char, u8> = HashMap::new();
    for (&code, decoded) in &font.unicode {
        if code > 0xFF || (font.reject_missing_metrics && !font.widths_1000.contains_key(&code)) {
            continue;
        }
        let mut chars = decoded.chars();
        if let (Some(only), None) = (chars.next(), chars.next()) {
            // Encoding aliases (notably WinAnsi space at 0x20 & 0xA0) must not depend
            // on randomized HashMap order. Prefer identity bytes, then lowest valid code.
            let byte = code as u8;
            reverse
                .entry(only)
                .and_modify(|current| {
                    if byte as char == only || (*current as char != only && byte < *current) {
                        *current = byte;
                    }
                })
                .or_insert(byte);
        }
    }
    text.chars().map(|ch| reverse.get(&ch).copied()).collect()
}

fn resolve_page_fonts(doc: &Document, page_id: ObjectId) -> HashMap<Vec<u8>, Rc<BuiltFont>> {
    let mut out = HashMap::new();
    let Ok(fonts) = doc.get_page_fonts(page_id) else {
        return out;
    };

    for (name, font_dict) in fonts.into_iter() {
        let subtype = font_dict
            .get(b"Subtype")
            .ok()
            .and_then(|o| o.as_name().ok())
            .unwrap_or(b"");

        let code_width: Option<u8> = if subtype == b"Type0" {
            match font_dict
                .get(b"Encoding")
                .ok()
                .and_then(|o| o.as_name().ok())
            {
                Some(b"Identity-H") => Some(2),
                _ => None, // Identity-V / named CMaps / non-CID Type0: unsupported (§7.3).
            }
        } else if matches!(subtype, b"Type1" | b"TrueType" | b"MMType1") {
            Some(1)
        } else {
            None // Type3 and unrecognized subtypes: unsupported.
        };

        let Some(width) = code_width else { continue };
        let font_id = String::from_utf8_lossy(&name).into_owned();

        let built = if width == 1 {
            let unicode: HashMap<u32, String> = font_dict
                .get_font_encoding_with_limit(doc, MAX_FONT_ENCODING_BYTES)
                .ok()
                .map(|encoding| {
                    (0u32..=255)
                        .filter_map(|code| {
                            Document::decode_text(&encoding, &[code as u8])
                                .ok()
                                .filter(|s| !s.is_empty())
                                .map(|s| (code, s))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let (default_width_1000, first_char, last_char, widths) =
                parse_simple_widths(doc, font_dict);
            let mut widths_1000 = HashMap::new();
            if last_char >= first_char {
                for code in first_char..=last_char {
                    let idx = (code - first_char) as usize;
                    if let Some(w) = widths.get(idx) {
                        widths_1000.insert(code as u32, *w);
                    }
                }
            }
            let mut reject_missing_metrics = default_width_1000 <= 0.0;
            if widths_1000.is_empty() {
                if let Some(base_font) = font_dict
                    .get(b"BaseFont")
                    .ok()
                    .and_then(|o| o.as_name().ok())
                {
                    for code in 0..=255 {
                        // AFM entries are keyed by canonical StandardEncoding codes. If
                        // `/Differences` remaps a code to another glyph, raw-code lookup would
                        // place its quad at the wrong width; leave that code uneditable instead.
                        let Some(decoded) = unicode.get(&code) else {
                            continue;
                        };
                        let mut chars = decoded.chars();
                        if chars.next() != char::from_u32(code) || chars.next().is_some() {
                            continue;
                        }
                        // StandardEncoding uses quoteright/quoteleft at these raw codes,
                        // while WinAnsi uses quotesingle/grave. Unicode identity cannot
                        // distinguish those glyph names, so avoid a wrong quad width.
                        if matches!(code, 39 | 96) {
                            continue;
                        }
                        if let Some(width) = standard14_width(base_font, code) {
                            widths_1000.insert(code, width);
                        }
                    }
                    // A named Standard-14 font has authoritative AFM metrics even if a
                    // producer also supplied an arbitrary /MissingWidth fallback.
                    if !widths_1000.is_empty() {
                        reject_missing_metrics = true;
                    }
                }
                reject_missing_metrics |= widths_1000.is_empty() && default_width_1000 <= 0.0;
            }
            BuiltFont {
                id: font_id,
                code_width: 1,
                default_width_1000,
                widths_1000,
                reject_missing_metrics,
                unicode,
            }
        } else {
            let unicode = parse_to_unicode(doc, font_dict);
            let (default_width_1000, widths_1000) = parse_cid_widths(doc, font_dict);
            BuiltFont {
                id: font_id,
                code_width: 2,
                default_width_1000,
                widths_1000,
                reject_missing_metrics: default_width_1000 <= 0.0,
                unicode,
            }
        };

        out.insert(name, Rc::new(built));
    }

    out
}

/// Adobe Standard 14 widths for the printable ASCII subset. PDF simple-font files may omit
/// `/Widths` for these built-in fonts; zero-width quads would make visible text impossible to
/// select. Unknown fonts remain fail-closed instead of receiving guessed metrics.
const STANDARD14_NAMES: [&str; 14] = [
    "Helvetica",
    "Helvetica-Bold",
    "Helvetica-Oblique",
    "Helvetica-BoldOblique",
    "Times-Roman",
    "Times-Bold",
    "Times-Italic",
    "Times-BoldItalic",
    "Courier",
    "Courier-Bold",
    "Courier-Oblique",
    "Courier-BoldOblique",
    "Symbol",
    "ZapfDingbats",
];

const STANDARD14_WIDTHS: [[u16; 95]; 14] = [
    [
        278, 278, 355, 556, 556, 889, 667, 222, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556,
        556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722,
        722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722,
        667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 222, 556, 556, 500, 556, 556, 278, 556,
        556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500,
        500, 334, 260, 334, 584,
    ], // Helvetica
    [
        278, 333, 474, 556, 556, 889, 722, 278, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556,
        556, 556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, 975, 722, 722, 722,
        722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722,
        667, 944, 667, 667, 611, 333, 278, 333, 584, 556, 278, 556, 611, 556, 611, 556, 333, 611,
        611, 278, 278, 556, 278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556,
        500, 389, 280, 389, 584,
    ], // Helvetica-Bold
    [
        278, 278, 355, 556, 556, 889, 667, 222, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556,
        556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722,
        722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722,
        667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 222, 556, 556, 500, 556, 556, 278, 556,
        556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500,
        500, 334, 260, 334, 584,
    ], // Helvetica-Oblique
    [
        278, 333, 474, 556, 556, 889, 722, 278, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556,
        556, 556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, 975, 722, 722, 722,
        722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722,
        667, 944, 667, 667, 611, 333, 278, 333, 584, 556, 278, 556, 611, 556, 611, 556, 333, 611,
        611, 278, 278, 556, 278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556,
        500, 389, 280, 389, 584,
    ], // Helvetica-BoldOblique
    [
        250, 333, 408, 500, 500, 833, 778, 333, 333, 333, 500, 564, 250, 333, 250, 278, 500, 500,
        500, 500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444, 921, 722, 667, 667,
        722, 611, 556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667, 556, 611, 722,
        722, 944, 722, 722, 611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500,
        500, 278, 278, 500, 278, 778, 500, 500, 500, 500, 333, 389, 278, 500, 500, 722, 500, 500,
        444, 480, 200, 480, 541,
    ], // Times-Roman
    [
        250, 333, 555, 500, 500, 1000, 833, 333, 333, 333, 500, 570, 250, 333, 250, 278, 500, 500,
        500, 500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500, 930, 722, 667, 722,
        722, 667, 611, 778, 778, 389, 500, 778, 667, 944, 722, 778, 611, 778, 722, 556, 667, 722,
        722, 1000, 722, 722, 667, 333, 278, 333, 581, 500, 333, 500, 556, 444, 556, 444, 333, 500,
        556, 278, 333, 556, 278, 833, 556, 500, 556, 556, 444, 389, 333, 556, 500, 722, 500, 500,
        444, 394, 220, 394, 520,
    ], // Times-Bold
    [
        250, 333, 420, 500, 500, 833, 778, 333, 333, 333, 500, 675, 250, 333, 250, 278, 500, 500,
        500, 500, 500, 500, 500, 500, 500, 500, 333, 333, 675, 675, 675, 500, 920, 611, 611, 667,
        722, 611, 611, 722, 722, 333, 444, 667, 556, 833, 667, 722, 611, 722, 611, 500, 556, 722,
        611, 833, 611, 556, 556, 389, 278, 389, 422, 500, 333, 500, 500, 444, 500, 444, 278, 500,
        500, 278, 278, 444, 278, 722, 500, 500, 500, 500, 389, 389, 278, 500, 444, 667, 444, 444,
        389, 400, 275, 400, 541,
    ], // Times-Italic
    [
        250, 389, 555, 500, 500, 833, 778, 333, 333, 333, 500, 570, 250, 333, 250, 278, 500, 500,
        500, 500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500, 832, 667, 667, 667,
        722, 667, 667, 722, 778, 389, 500, 667, 611, 889, 722, 722, 611, 722, 667, 556, 611, 722,
        667, 889, 667, 611, 611, 333, 278, 333, 570, 500, 333, 500, 500, 444, 500, 444, 333, 500,
        556, 278, 278, 500, 278, 778, 556, 500, 500, 500, 389, 389, 278, 556, 444, 667, 500, 444,
        389, 348, 220, 348, 570,
    ], // Times-BoldItalic
    [
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600,
    ], // Courier
    [
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600,
    ], // Courier-Bold
    [
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600,
    ], // Courier-Oblique
    [
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600, 600,
        600, 600, 600, 600, 600,
    ], // Courier-BoldOblique
    [
        250, 333, 713, 500, 549, 833, 778, 439, 333, 333, 500, 549, 250, 549, 250, 278, 500, 500,
        500, 500, 500, 500, 500, 500, 500, 500, 278, 278, 549, 549, 549, 444, 549, 722, 667, 722,
        612, 611, 763, 603, 722, 333, 631, 722, 686, 889, 722, 722, 768, 741, 556, 592, 611, 690,
        439, 768, 645, 795, 611, 333, 863, 333, 658, 500, 500, 631, 549, 549, 494, 439, 521, 411,
        603, 329, 603, 549, 549, 576, 521, 549, 549, 521, 549, 603, 439, 576, 713, 686, 493, 686,
        494, 480, 200, 480, 549,
    ], // Symbol
    [
        278, 974, 961, 974, 980, 719, 789, 790, 791, 690, 960, 939, 549, 855, 911, 933, 911, 945,
        974, 755, 846, 762, 761, 571, 677, 763, 760, 759, 754, 494, 552, 537, 577, 692, 786, 788,
        788, 790, 793, 794, 816, 823, 789, 841, 823, 833, 816, 831, 923, 744, 723, 749, 790, 792,
        695, 776, 768, 792, 759, 707, 708, 682, 701, 826, 815, 789, 789, 707, 687, 696, 689, 786,
        787, 713, 791, 785, 791, 873, 761, 762, 762, 759, 759, 892, 892, 788, 784, 438, 138, 277,
        415, 392, 392, 668, 668,
    ], // ZapfDingbats
];

fn standard14_width(base_font: &[u8], code: u32) -> Option<f64> {
    let name = std::str::from_utf8(base_font).ok()?;
    let row = STANDARD14_NAMES.iter().position(|known| *known == name)?;
    let index = code.checked_sub(32)? as usize;
    STANDARD14_WIDTHS[row]
        .get(index)
        .map(|width| f64::from(*width))
}

fn parse_simple_widths(doc: &Document, font_dict: &Dictionary) -> (f64, i64, i64, Vec<f64>) {
    let first_char = font_dict
        .get(b"FirstChar")
        .ok()
        .and_then(|o| o.as_i64().ok())
        .unwrap_or(0);
    let last_char = font_dict
        .get(b"LastChar")
        .ok()
        .and_then(|o| o.as_i64().ok())
        .unwrap_or(-1);
    let widths: Vec<f64> = font_dict
        .get(b"Widths")
        .ok()
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_array().ok())
        .map(|arr| arr.iter().filter_map(object_number).collect())
        .unwrap_or_default();
    let missing_width = font_dict
        .get(b"FontDescriptor")
        .ok()
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_dict().ok())
        .and_then(|d| d.get(b"MissingWidth").ok())
        .and_then(object_number)
        .unwrap_or(0.0);
    (missing_width, first_char, last_char, widths)
}

fn parse_cid_widths(doc: &Document, font_dict: &Dictionary) -> (f64, HashMap<u32, f64>) {
    let mut dw = 1000.0;
    let mut map = HashMap::new();

    let Ok(desc_obj) = font_dict.get(b"DescendantFonts") else {
        return (dw, map);
    };
    let Some(Object::Array(desc_arr)) = deref_obj(doc, desc_obj) else {
        return (dw, map);
    };
    let Some(first) = desc_arr.first() else {
        return (dw, map);
    };
    let Some(cid_obj) = deref_obj(doc, first) else {
        return (dw, map);
    };
    let Ok(cid_dict) = cid_obj.as_dict() else {
        return (dw, map);
    };

    if let Some(v) = cid_dict.get(b"DW").ok().and_then(object_number) {
        dw = v;
    }

    if let Ok(w_obj) = cid_dict.get(b"W") {
        if let Some(Object::Array(items)) = deref_obj(doc, w_obj) {
            let mut i = 0usize;
            while i < items.len() {
                let Some(c_first) = object_number(&items[i]) else {
                    break;
                };
                if i + 1 >= items.len() {
                    break;
                }
                match deref_obj(doc, &items[i + 1]) {
                    Some(Object::Array(widths)) => {
                        for (offset, w) in widths.iter().enumerate() {
                            if let Some(wv) = object_number(w) {
                                map.insert(c_first as u32 + offset as u32, wv);
                            }
                        }
                        i += 2;
                    }
                    _ => {
                        if i + 2 >= items.len() {
                            break;
                        }
                        let Some(c_last) = object_number(&items[i + 1]) else {
                            break;
                        };
                        let Some(wv) = object_number(&items[i + 2]) else {
                            break;
                        };
                        let mut c = c_first as u32;
                        let last = c_last as u32;
                        while c <= last {
                            map.insert(c, wv);
                            c += 1;
                        }
                        i += 3;
                    }
                }
            }
        }
    }

    (dw, map)
}

/// Minimal owned `ToUnicode` CMap parser: `beginbfchar`/`endbfchar` and `beginbfrange`/
/// `endbfrange` blocks, scalar-destination form only (an array-of-individual-mappings `bfrange`
/// destination is rare and treated as absent — best-effort per this module's doc comment).
fn parse_to_unicode(doc: &Document, font_dict: &Dictionary) -> HashMap<u32, String> {
    let mut map = HashMap::new();

    let Ok(tu_obj) = font_dict.get(b"ToUnicode") else {
        return map;
    };
    let Some(obj) = deref_obj(doc, tu_obj) else {
        return map;
    };
    let Ok(stream) = obj.as_stream() else {
        return map;
    };
    let bytes = stream
        .decompressed_content()
        .unwrap_or_else(|_| stream.content.clone());
    let text = String::from_utf8_lossy(&bytes);

    for block in extract_blocks(&text, "beginbfchar", "endbfchar") {
        let tokens = hex_tokens(block);
        #[allow(clippy::chunks_exact_to_as_chunks)] // `as_chunks` availability not pinned here.
        for pair in tokens.chunks_exact(2) {
            if let [src, dst] = pair {
                if let Some(code) = hex_to_u32(src) {
                    map.insert(code, hex_to_unicode_string(dst));
                }
            }
        }
    }

    for block in extract_blocks(&text, "beginbfrange", "endbfrange") {
        let tokens = hex_tokens(block);
        #[allow(clippy::chunks_exact_to_as_chunks)] // `as_chunks` availability not pinned here.
        for triple in tokens.chunks_exact(3) {
            if let [lo, hi, dst] = triple {
                if let (Some(lo_code), Some(hi_code), Some(base)) =
                    (hex_to_u32(lo), hex_to_u32(hi), hex_to_u32(dst))
                {
                    let span = hi_code.saturating_sub(lo_code).min(65535);
                    for offset in 0..=span {
                        map.insert(lo_code + offset, char_from_u32(base + offset));
                    }
                }
            }
        }
    }

    map
}

fn extract_blocks<'a>(text: &'a str, begin: &str, end: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(begin) {
        let after_begin = &rest[start + begin.len()..];
        let Some(end_idx) = after_begin.find(end) else {
            break;
        };
        out.push(&after_begin[..end_idx]);
        rest = &after_begin[end_idx + end.len()..];
    }
    out
}

fn hex_tokens(block: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = block.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            let mut tok = String::new();
            for c2 in chars.by_ref() {
                if c2 == '>' {
                    break;
                }
                if c2.is_ascii_hexdigit() {
                    tok.push(c2);
                }
            }
            out.push(tok);
        }
    }
    out
}

fn hex_to_u32(tok: &str) -> Option<u32> {
    u32::from_str_radix(tok, 16).ok()
}

/// Decodes a hex token as big-endian UTF-16 code units (the `ToUnicode` CMap convention).
fn hex_to_unicode_string(tok: &str) -> String {
    let bytes: Vec<u8> = (0..tok.len())
        .step_by(2)
        .filter_map(|i| {
            tok.get(i..i + 2)
                .and_then(|b| u8::from_str_radix(b, 16).ok())
        })
        .collect();
    #[allow(clippy::chunks_exact_to_as_chunks)] // `as_chunks` availability not pinned here.
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

fn char_from_u32(code: u32) -> String {
    hex_to_unicode_string(&format!("{code:04X}"))
}

// -------------------------------------------------------------------------------------------
// §5.2/§5.3 — Stream walker: graphics/text state, glyph geometry, painted-bounds inventory
// -------------------------------------------------------------------------------------------

/// One coarse axis-aligned painted-content entry from §5.3's painted-bounds inventory. Kept
/// local to this module (not [`crate::model::PaintedBounds`]'s exact shape, which is
/// `pub(crate)`-visible but not part of the wire graph) since only [`may_join`]'s
/// `has_intervening_painted_content` consumes it here; §8's layout-collision reuse of the same
/// inventory belongs to `pdf_compose.rs`, which will build its own from the same walker inputs.
struct PaintedBoundsEntry {
    page: u32,
    bounds: PdfRect,
}

#[allow(clippy::too_many_arguments)]
fn walk_stream(
    stream: &PageStream,
    content: &Content,
    page: u32,
    document_sha256: &str,
    fonts: &HashMap<Vec<u8>, Rc<BuiltFont>>,
    glyphs: &mut Vec<PositionedGlyph>,
    painted_bounds: &mut Vec<PaintedBoundsEntry>,
    rejections: &mut Vec<PdfTextIsland>,
) {
    let mut gs_stack: Vec<(Matrix, PaintState)> = Vec::new();
    let mut ctm = Matrix::IDENTITY;
    let mut paint = PaintState {
        render_mode: TextRenderMode::Fill,
        fill_color: None,
        stroke_color: None,
    };
    let mut text_state = TextState::new();
    let mut path_points: Vec<(f64, f64)> = Vec::new();

    for (op_idx, op) in content.operations.iter().enumerate() {
        let source = SourceAddress {
            stream_ref: stream.stream_ref,
            contents_index: stream.contents_index,
            operator_index: op_idx as u32,
            operand_index: 0,
        };

        match op.operator.as_str() {
            "q" => gs_stack.push((ctm, paint.clone())),
            "Q" => {
                if let Some((saved_ctm, saved_paint)) = gs_stack.pop() {
                    ctm = saved_ctm;
                    paint = saved_paint;
                }
            }
            "cm" => {
                if let Some(m) = matrix_from_operands(&op.operands) {
                    ctm = m.then(ctm);
                }
            }
            "BT" => {
                text_state.ctm = ctm;
                text_state.begin_text();
            }
            "ET" => {}
            "Tm" => {
                if let Some(m) = matrix_from_operands(&op.operands) {
                    text_state.set_tm(m);
                }
            }
            "Td" => {
                if let [tx, ty] = operand_numbers(&op.operands).as_slice() {
                    text_state.move_line(*tx, *ty);
                }
            }
            "TD" => {
                if let [tx, ty] = operand_numbers(&op.operands).as_slice() {
                    text_state.leading = -*ty;
                    text_state.move_line(*tx, *ty);
                }
            }
            "T*" => text_state.next_line(),
            "TL" => {
                if let [tl] = operand_numbers(&op.operands).as_slice() {
                    text_state.leading = *tl;
                }
            }
            "Tc" => {
                if let [tc] = operand_numbers(&op.operands).as_slice() {
                    text_state.char_spacing = *tc;
                }
            }
            "Tw" => {
                if let [tw] = operand_numbers(&op.operands).as_slice() {
                    text_state.word_spacing = *tw;
                }
            }
            "Tz" => {
                if let [tz] = operand_numbers(&op.operands).as_slice() {
                    text_state.horizontal_scale = *tz / 100.0;
                }
            }
            "Ts" => {
                if let [ts] = operand_numbers(&op.operands).as_slice() {
                    text_state.text_rise = *ts;
                }
            }
            "Tr" => {
                if let [tr] = operand_numbers(&op.operands).as_slice() {
                    let mode = TextRenderMode::from_operand(*tr as i64);
                    text_state.render_mode = mode;
                    paint.render_mode = mode;
                }
            }
            "Tf" => {
                if let Some(Object::Name(name)) = op.operands.first() {
                    text_state.font = fonts
                        .get(name)
                        .map(|f| Rc::clone(f) as Rc<dyn TextStateFont>);
                }
                if let Some(size) = op.operands.get(1).and_then(object_number) {
                    text_state.font_size = size;
                }
            }
            "rg" => {
                let n = operand_numbers(&op.operands);
                if let [r, g, b] = n.as_slice() {
                    paint.fill_color = Some([*r, *g, *b, 1.0]);
                }
            }
            "g" => {
                if let [gray] = operand_numbers(&op.operands).as_slice() {
                    paint.fill_color = Some([*gray, *gray, *gray, 1.0]);
                }
            }
            "k" => {
                let n = operand_numbers(&op.operands);
                if let [c, m, y, kk] = n.as_slice() {
                    paint.fill_color = Some([
                        (1.0 - c) * (1.0 - kk),
                        (1.0 - m) * (1.0 - kk),
                        (1.0 - y) * (1.0 - kk),
                        1.0,
                    ]);
                }
            }
            "Tj" => {
                text_state.ctm = ctm;
                if let Some(operand) = op.operands.first() {
                    show_operand(
                        &mut text_state,
                        &source,
                        0,
                        operand,
                        page,
                        document_sha256,
                        &paint,
                        glyphs,
                        rejections,
                    );
                }
            }
            "'" => {
                text_state.ctm = ctm;
                text_state.next_line();
                if let Some(operand) = op.operands.first() {
                    show_operand(
                        &mut text_state,
                        &source,
                        0,
                        operand,
                        page,
                        document_sha256,
                        &paint,
                        glyphs,
                        rejections,
                    );
                }
            }
            "\"" => {
                text_state.ctm = ctm;
                let nums = operand_numbers(&op.operands);
                if nums.len() >= 2 {
                    text_state.word_spacing = nums[0];
                    text_state.char_spacing = nums[1];
                }
                text_state.next_line();
                if let Some(operand) = op.operands.get(2) {
                    show_operand(
                        &mut text_state,
                        &source,
                        0,
                        operand,
                        page,
                        document_sha256,
                        &paint,
                        glyphs,
                        rejections,
                    );
                }
            }
            "TJ" => {
                text_state.ctm = ctm;
                if let Some(Object::Array(items)) = op.operands.first() {
                    for (item_idx, item) in items.iter().enumerate() {
                        match item {
                            Object::String(..) => {
                                show_operand(
                                    &mut text_state,
                                    &source,
                                    item_idx as u32,
                                    item,
                                    page,
                                    document_sha256,
                                    &paint,
                                    glyphs,
                                    rejections,
                                );
                            }
                            Object::Integer(n) => {
                                let adj = -(*n as f64 / 1000.0)
                                    * text_state.font_size
                                    * text_state.horizontal_scale;
                                text_state.text_matrix.translate_text(adj, 0.0);
                            }
                            Object::Real(n) => {
                                let adj = -(*n as f64 / 1000.0)
                                    * text_state.font_size
                                    * text_state.horizontal_scale;
                                text_state.text_matrix.translate_text(adj, 0.0);
                            }
                            _ => {}
                        }
                    }
                }
            }
            "m" | "l" => {
                let n = operand_numbers(&op.operands);
                if n.len() == 2 {
                    path_points.push(ctm.transform((n[0], n[1])));
                }
            }
            "c" => {
                let n = operand_numbers(&op.operands);
                if n.len() == 6 {
                    path_points.push(ctm.transform((n[0], n[1])));
                    path_points.push(ctm.transform((n[2], n[3])));
                    path_points.push(ctm.transform((n[4], n[5])));
                }
            }
            "v" | "y" => {
                let n = operand_numbers(&op.operands);
                if n.len() == 4 {
                    path_points.push(ctm.transform((n[0], n[1])));
                    path_points.push(ctm.transform((n[2], n[3])));
                }
            }
            "re" => {
                let n = operand_numbers(&op.operands);
                if n.len() == 4 {
                    let (x, y, w, h) = (n[0], n[1], n[2], n[3]);
                    for corner in [(x, y), (x + w, y), (x + w, y + h), (x, y + h)] {
                        path_points.push(ctm.transform(corner));
                    }
                }
            }
            "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" => {
                if let Some(bounds) = bounds_from_points(&path_points) {
                    painted_bounds.push(PaintedBoundsEntry { page, bounds });
                }
                path_points.clear();
            }
            "n" => path_points.clear(),
            "Do" => {
                let corners =
                    [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|p| ctm.transform(p));
                if let Some(bounds) = bounds_from_points(&corners) {
                    painted_bounds.push(PaintedBoundsEntry { page, bounds });
                }
            }
            _ => {}
        }
    }
}

/// Shows one `Tj`/`TJ`-item/`'`/`"` string operand: resolves the active font/matrix validity,
/// then either appends positioned glyphs (success) or records a typed uneditable island
/// (unresolved font, or singular CTM/text matrix) — §5.2's "typed uneditable islands" contract.
#[allow(clippy::too_many_arguments)]
fn show_operand(
    text_state: &mut TextState,
    source: &SourceAddress,
    operand_index: u32,
    operand: &Object,
    page: u32,
    document_sha256: &str,
    paint: &PaintState,
    glyphs: &mut Vec<PositionedGlyph>,
    rejections: &mut Vec<PdfTextIsland>,
) {
    let mut source_op = source.clone();
    source_op.operand_index = operand_index;

    if text_state.font.is_none() {
        rejections.push(rejected_island(
            document_sha256,
            page,
            rejections.len(),
            PdfEditError::UnsupportedEncoding,
        ));
        return;
    }
    if is_singular(text_state.ctm) || is_singular(text_state.text_matrix) {
        rejections.push(rejected_island(
            document_sha256,
            page,
            rejections.len(),
            PdfEditError::UnsupportedTransform,
        ));
        return;
    }

    let before = glyphs.len();
    if visit_text_show(text_state, &source_op, operand, page, paint, glyphs).is_err() {
        glyphs.truncate(before);
        rejections.push(rejected_island(
            document_sha256,
            page,
            rejections.len(),
            PdfEditError::UnsupportedEncoding,
        ));
    }
}

fn is_singular(m: Matrix) -> bool {
    (m.a * m.d - m.b * m.c).abs() < 1e-9
}

/// §5.2's `visit_text_show`: decodes one show operand through the active font and appends one
/// [`PositionedGlyph`] per glyph, advancing `state.text_matrix` per PDF spec 9.4.3/9.4.4.
fn visit_text_show(
    state: &mut TextState,
    source: &SourceAddress,
    operand: &Object,
    page: u32,
    paint: &PaintState,
    out: &mut Vec<PositionedGlyph>,
) -> Result<(), PdfEditError> {
    let font = state.font.clone().ok_or(PdfEditError::MalformedContent)?;
    let decoded = font.decode_show_operand(operand)?;

    for glyph in decoded {
        let text_matrix = state.text_matrix;
        let quad = state.glyph_quad(&glyph, text_matrix)?;
        let origin = state.origin();
        let advance = font.advance(glyph.cid, state.font_size)
            + state.char_spacing
            + if glyph.unicode == " " {
                state.word_spacing
            } else {
                0.0
            };

        out.push(PositionedGlyph {
            page,
            unicode: glyph.unicode,
            cid: glyph.cid,
            origin,
            quad,
            source: source.with_glyph(glyph.source_range),
            font_id: font.id().to_string(),
            font_size: state.font_size,
            text_matrix,
            ctm: state.ctm,
            writing_direction: WritingDirection::LeftToRight,
            paint: paint.clone(),
        });

        state
            .text_matrix
            .translate_text(advance * state.horizontal_scale, 0.0);
    }
    Ok(())
}

fn rejected_island(
    document_sha256: &str,
    page: u32,
    index: usize,
    reason: PdfEditError,
) -> PdfTextIsland {
    PdfTextIsland {
        id: format!("rejected-{page}-{index}"),
        text: String::new(),
        quads: Vec::new(),
        baseline: [0.0, 0.0, 0.0, 0.0],
        appearance: PdfTextAppearance {
            font_id: "unknown".to_string(),
            family: "unknown".to_string(),
            size_pt: 0.0,
            color: PdfColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                alpha: 1.0,
            },
        },
        locator: PdfSourceLocator {
            document_sha256: document_sha256.to_string(),
            page,
            spans: Vec::new(),
            island_digest: sha256_hex(format!("rejected-{page}-{index}").as_bytes()),
        },
        editable: false,
        rejection: Some(reason),
    }
}

fn matrix_from_operands(operands: &[Object]) -> Option<Matrix> {
    let nums = operand_numbers(operands);
    if let [a, b, c, d, e, f] = nums.as_slice() {
        Some(Matrix {
            a: *a,
            b: *b,
            c: *c,
            d: *d,
            e: *e,
            f: *f,
        })
    } else {
        None
    }
}

fn operand_numbers(operands: &[Object]) -> Vec<f64> {
    operands.iter().filter_map(object_number).collect()
}

fn bounds_from_points(points: &[(f64, f64)]) -> Option<PdfRect> {
    if points.is_empty() {
        return None;
    }
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for &(x, y) in points {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    Some(PdfRect {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    })
}

// -------------------------------------------------------------------------------------------
// §5.3 — Text-island grouping + painted-bounds collision
// -------------------------------------------------------------------------------------------

fn quad_x_extent(quad: &[f64; 8]) -> (f64, f64) {
    // Corners are stored as (x,y) pairs, so every even index is an x component.
    let xs = [quad[0], quad[2], quad[4], quad[6]];
    (
        xs.iter().cloned().fold(f64::INFINITY, f64::min),
        xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    )
}

fn projected_gap(left: &PositionedGlyph, right: &PositionedGlyph) -> f64 {
    let (_, left_end) = quad_x_extent(&left.quad);
    let (right_start, _) = quad_x_extent(&right.quad);
    right_start - left_end
}

fn baseline_distance(left: &PositionedGlyph, right: &PositionedGlyph) -> f64 {
    (left.origin.1 - right.origin.1).abs()
}

fn has_intervening_painted_content(
    left: &PositionedGlyph,
    right: &PositionedGlyph,
    painted_bounds: &[PaintedBoundsEntry],
) -> bool {
    let (_, left_end) = quad_x_extent(&left.quad);
    let (right_start, _) = quad_x_extent(&right.quad);
    if right_start <= left_end {
        return false;
    }
    let em = left.font_size.max(right.font_size).max(1.0);
    let y_min = left.origin.1.min(right.origin.1) - em * 0.5;
    let y_max = left.origin.1.max(right.origin.1) + em * 0.5;
    let gap_rect = PdfRect {
        x: left_end,
        y: y_min,
        width: right_start - left_end,
        height: (y_max - y_min).max(0.01),
    };
    painted_bounds
        .iter()
        .any(|pb| pb.page == left.page && pb.bounds.intersects(gap_rect))
}

/// §5.3's `may_join`, adapted to concrete [`PositionedGlyph`] fields.
fn may_join(
    left: &PositionedGlyph,
    right: &PositionedGlyph,
    painted_bounds: &[PaintedBoundsEntry],
) -> bool {
    let em = left.font_size.max(right.font_size).max(1.0);
    let baseline_tolerance = 0.5_f64.max(0.20 * em);
    let gap = projected_gap(left, right);

    left.page == right.page
        && left.writing_direction == right.writing_direction
        && baseline_distance(left, right) <= baseline_tolerance
        && gap >= -0.25 * em
        && gap <= 1.50 * em
        && left.paint == right.paint
        && !has_intervening_painted_content(left, right, painted_bounds)
}

fn group_islands(
    page: u32,
    document_sha256: &str,
    mut glyphs: Vec<PositionedGlyph>,
    painted_bounds: &[PaintedBoundsEntry],
) -> Vec<PdfTextIsland> {
    let mut runs: Vec<Vec<PositionedGlyph>> = Vec::new();

    for glyph in glyphs.drain(..) {
        let can_join = runs
            .last()
            .and_then(|run| run.last())
            .map(|last| may_join(last, &glyph, painted_bounds))
            .unwrap_or(false);
        if can_join {
            runs.last_mut().expect("checked above").push(glyph);
        } else {
            runs.push(vec![glyph]);
        }
    }

    runs.into_iter()
        .enumerate()
        .map(|(idx, run)| build_island(page, document_sha256, idx, run))
        .collect()
}

fn build_spans(run: &[PositionedGlyph]) -> Vec<PdfSourceSpan> {
    let mut spans: Vec<PdfSourceSpan> = Vec::new();
    for glyph in run {
        let addr = &glyph.source;
        if let Some(last) = spans.last_mut() {
            if last.stream_ref == addr.stream_ref
                && last.contents_index == addr.contents_index
                && last.operator_index == addr.operator_index
                && last.operand_index == addr.operand_index
                && last.glyph_end == addr.glyph_start
            {
                last.glyph_end = addr.glyph_end;
                last.expected_text.push_str(&glyph.unicode);
                last.expected_digest = expected_text_digest(&last.expected_text);
                continue;
            }
        }
        spans.push(PdfSourceSpan {
            stream_ref: addr.stream_ref,
            contents_index: addr.contents_index,
            operator_index: addr.operator_index,
            operand_index: addr.operand_index,
            glyph_start: addr.glyph_start,
            glyph_end: addr.glyph_end,
            expected_text: glyph.unicode.clone(),
            expected_digest: expected_text_digest(&glyph.unicode),
        });
    }
    spans
}

fn build_island(
    page: u32,
    document_sha256: &str,
    index: usize,
    run: Vec<PositionedGlyph>,
) -> PdfTextIsland {
    let text: String = run.iter().map(|g| g.unicode.as_str()).collect();
    let quads: Vec<[f64; 8]> = run.iter().map(|g| g.quad).collect();
    let first = &run[0];
    let last = &run[run.len() - 1];
    let baseline = [first.origin.0, first.origin.1, last.origin.0, last.origin.1];

    let spans = build_spans(&run);
    let island_digest = compute_island_digest(
        &spans,
        &text,
        &first.font_id,
        &first.text_matrix,
        &first.ctm,
        &first.paint,
        &quads,
    );

    let appearance = PdfTextAppearance {
        font_id: first.font_id.clone(),
        family: first.font_id.clone(),
        size_pt: first.font_size,
        color: first
            .paint
            .fill_color
            .map(|[r, g, b, a]| PdfColor { r, g, b, alpha: a })
            .unwrap_or(PdfColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                alpha: 1.0,
            }),
    };

    PdfTextIsland {
        id: format!("island-{page}-{index}"),
        text,
        quads,
        baseline,
        appearance,
        locator: PdfSourceLocator {
            document_sha256: document_sha256.to_string(),
            page,
            spans,
            island_digest,
        },
        editable: true,
        rejection: None,
    }
}

// -------------------------------------------------------------------------------------------
// Tests
// -------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Stream, StringFormat};

    use crate::test_fixtures::{build_one_page_pdf as build_pdf, one_page_pdf, PageFixture};

    fn inspect(doc: &Document) -> PdfPageGraph {
        walk_page_graph(doc, "test-sha", 1).expect("inspection should succeed")
    }

    #[test]
    fn single_tj_produces_one_editable_island() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET");
        let graph = inspect(&doc);
        assert_eq!(graph.islands.len(), 1);
        let island = &graph.islands[0];
        assert!(island.editable);
        assert_eq!(island.text, "Hello");
        assert_eq!(island.quads.len(), 5);
    }

    #[test]
    fn standard14_without_widths_uses_builtin_glyph_metrics() {
        // This is the ordinary Base-14 shape emitted by many producers: Helvetica omits
        // /Encoding, /FirstChar, /LastChar, /Widths, and /FontDescriptor.
        let mut doc = build_pdf(b"BT /F1 24 Tf 72 700 Td (PA) Tj ET");
        let page_id = *doc.get_pages().get(&1).expect("fixture page");
        let resources_id = doc
            .get_object(page_id)
            .expect("fixture page object")
            .as_dict()
            .expect("page dictionary")
            .get(b"Resources")
            .expect("resources")
            .as_reference()
            .expect("resources reference");
        let font_id = doc
            .get_object(resources_id)
            .expect("resources object")
            .as_dict()
            .expect("resources dictionary")
            .get(b"Font")
            .expect("font dictionary")
            .as_dict()
            .expect("font resources")
            .get(b"F1")
            .expect("F1")
            .as_reference()
            .expect("F1 reference");
        doc.get_object_mut(font_id)
            .expect("font object")
            .as_dict_mut()
            .expect("font dictionary")
            .remove(b"Encoding");

        let graph = inspect(&doc);
        let island = &graph.islands[0];
        assert!(island.editable);
        assert_eq!(island.text, "PA");
        assert_eq!(island.quads.len(), 2);
        assert!(island.quads[0][2] > island.quads[0][0] + 10.0);
        assert!(island.quads[1][0] > island.quads[0][0]);
    }

    #[test]
    fn standard14_fallback_rejects_ambiguous_quote_codes_without_metrics() {
        for content in [
            b"BT /F1 12 Tf 72 700 Td (') Tj ET".as_slice(),
            b"BT /F1 12 Tf 72 700 Td (`) Tj ET".as_slice(),
        ] {
            let graph = inspect(&build_pdf(content));
            let island = &graph.islands[0];
            assert!(!island.editable);
            assert!(island.quads.is_empty());
            assert!(matches!(
                island.rejection,
                Some(PdfEditError::UnsupportedEncoding)
            ));
        }
    }

    #[test]
    fn standard14_fallback_rejects_encoding_difference_without_metrics() {
        let mut doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (A) Tj ET");
        let page_id = *doc.get_pages().get(&1).expect("fixture page");
        let resources_id = doc
            .get_object(page_id)
            .expect("fixture page object")
            .as_dict()
            .expect("page dictionary")
            .get(b"Resources")
            .expect("resources")
            .as_reference()
            .expect("resources reference");
        let font_id = doc
            .get_object(resources_id)
            .expect("resources object")
            .as_dict()
            .expect("resources dictionary")
            .get(b"Font")
            .expect("font dictionary")
            .as_dict()
            .expect("font resources")
            .get(b"F1")
            .expect("F1")
            .as_reference()
            .expect("F1 reference");
        doc.get_object_mut(font_id)
            .expect("font object")
            .as_dict_mut()
            .expect("font dictionary")
            .set(
                "Encoding",
                dictionary! {
                    "Type" => "Encoding",
                    "BaseEncoding" => "WinAnsiEncoding",
                    "Differences" => Object::Array(vec![
                        Object::Integer(65),
                        Object::Name(b"i".to_vec()),
                    ]),
                },
            );

        let graph = inspect(&doc);
        let island = &graph.islands[0];
        assert!(!island.editable);
        assert!(island.quads.is_empty());
        assert!(matches!(
            island.rejection,
            Some(PdfEditError::UnsupportedEncoding)
        ));
    }

    #[test]
    fn unknown_font_without_metrics_is_uneditable_in_page_graph() {
        let mut doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (A) Tj ET");
        let page_id = *doc.get_pages().get(&1).expect("fixture page");
        let resources_id = doc
            .get_object(page_id)
            .expect("fixture page object")
            .as_dict()
            .expect("page dictionary")
            .get(b"Resources")
            .expect("resources")
            .as_reference()
            .expect("resources reference");
        let font_id = doc
            .get_object(resources_id)
            .expect("resources object")
            .as_dict()
            .expect("resources dictionary")
            .get(b"Font")
            .expect("font dictionary")
            .as_dict()
            .expect("font resources")
            .get(b"F1")
            .expect("F1")
            .as_reference()
            .expect("F1 reference");
        doc.get_object_mut(font_id)
            .expect("font object")
            .as_dict_mut()
            .expect("font dictionary")
            .set("BaseFont", "Unknown");

        let graph = inspect(&doc);
        let island = &graph.islands[0];
        assert!(!island.editable);
        assert!(island.quads.is_empty());
        assert!(matches!(
            island.rejection,
            Some(PdfEditError::UnsupportedEncoding)
        ));
    }

    #[test]
    fn unknown_font_without_metrics_rejects_glyphs() {
        let font = BuiltFont {
            id: "Unknown".to_string(),
            code_width: 1,
            default_width_1000: 0.0,
            widths_1000: HashMap::new(),
            reject_missing_metrics: true,
            unicode: HashMap::new(),
        };
        let result = font.decode_show_operand(&Object::String(vec![b'A'], StringFormat::Literal));
        assert!(matches!(result, Err(PdfEditError::UnsupportedEncoding)));
    }

    #[test]
    fn split_tj_array_with_numeric_adjustment_joins_into_one_island() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td [(CAR YOU) -50 ( LIST.)] TJ ET");
        let graph = inspect(&doc);
        assert_eq!(graph.islands.len(), 1);
        assert_eq!(graph.islands[0].text, "CAR YOU LIST.");
    }

    #[test]
    fn adjacent_tj_operators_on_same_baseline_join_into_one_island() {
        // Reposition by the preceding run's Standard-14 advance so runs are truly adjacent.
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (CAR YOU) Tj 55 0 Td ( LIST.) Tj ET");
        let graph = inspect(&doc);
        assert_eq!(graph.islands.len(), 1);
        assert_eq!(graph.islands[0].text, "CAR YOU LIST.");
    }

    #[test]
    fn repeated_text_on_different_lines_yields_distinct_locator_digests() {
        let doc = build_pdf(
            b"BT /F1 12 Tf 72 700 Td (Repeat) Tj ET BT /F1 12 Tf 72 600 Td (Repeat) Tj ET",
        );
        let graph = inspect(&doc);
        assert_eq!(graph.islands.len(), 2);
        assert_eq!(graph.islands[0].text, "Repeat");
        assert_eq!(graph.islands[1].text, "Repeat");
        assert_ne!(
            graph.islands[0].locator.island_digest,
            graph.islands[1].locator.island_digest
        );
    }

    #[test]
    fn large_gap_between_runs_does_not_join_into_one_island() {
        // A huge horizontal jump between two Td-repositioned runs must not be treated as
        // continuous, even though both are technically on the same nominal baseline.
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Left) Tj 400 0 Td (Right) Tj ET");
        let graph = inspect(&doc);
        assert_eq!(graph.islands.len(), 2);
        assert_eq!(graph.islands[0].text, "Left");
        assert_eq!(graph.islands[1].text, "Right");
    }

    #[test]
    fn cm_rotation_changes_device_space_glyph_origin() {
        let unrotated = build_pdf(b"BT /F1 12 Tf 72 700 Td (A) Tj ET");
        let rotated = build_pdf(b"q 0 1 -1 0 0 0 cm BT /F1 12 Tf 72 700 Td (A) Tj ET Q");

        let a = inspect(&unrotated);
        let b = inspect(&rotated);
        assert_ne!(a.islands[0].baseline, b.islands[0].baseline);
    }

    #[test]
    fn singular_text_matrix_produces_uneditable_island_with_typed_rejection() {
        let doc = build_pdf(b"BT /F1 12 Tf 0 0 0 0 0 0 Tm (Broken) Tj ET");
        let graph = inspect(&doc);
        assert_eq!(graph.islands.len(), 1);
        assert!(!graph.islands[0].editable);
        assert!(matches!(
            &graph.islands[0].rejection,
            Some(PdfEditError::UnsupportedTransform)
        ));
    }

    #[test]
    fn missing_font_resource_produces_uneditable_island_with_typed_rejection() {
        let doc = build_pdf(b"BT /F9 12 Tf 72 700 Td (Nope) Tj ET");
        let graph = inspect(&doc);
        assert_eq!(graph.islands.len(), 1);
        assert!(!graph.islands[0].editable);
        assert!(matches!(
            &graph.islands[0].rejection,
            Some(PdfEditError::UnsupportedEncoding)
        ));
    }

    #[test]
    fn malformed_content_stream_is_rejected() {
        // lopdf's content-stream tokenizer is lenient about unbalanced/unterminated string
        // operands (it does not error on `b"BT (unterminated"`), so that no longer reaches this
        // rejection path. `decompress_zlib` is also lenient by design (source-verified: on a bad
        // zlib/deflate header it just `warn!`s and falls through with whatever partial/empty
        // output it has, never returning `Err`) — so a bogus `FlateDecode` payload doesn't error
        // either. lopdf's `decode_filters` DOES return a real `Err` for a filter name it has no
        // decoder for at all (`_ => Err(Error::Unimplemented("decompression algorithms"))`), so an
        // unimplemented filter is what deterministically fails here.
        let doc = one_page_pdf(PageFixture {
            content: Stream::new(
                dictionary! { "Filter" => "RunLengthDecode" },
                b"irrelevant: RunLengthDecode has no decoder in lopdf".to_vec(),
            ),
            helvetica_f1: false,
            ..PageFixture::for_content(b"")
        });

        let result = walk_page_graph(&doc, "test-sha", 1);
        assert!(matches!(result, Err(PdfEditError::MalformedContent)));
    }

    #[test]
    fn unknown_page_number_is_malformed_content() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Hi) Tj ET");
        let result = walk_page_graph(&doc, "test-sha", 99);
        assert!(matches!(result, Err(PdfEditError::MalformedContent)));
    }

    #[test]
    fn inherited_media_box_from_parent_pages_node_is_used() {
        // No MediaBox on the page itself: must inherit from Parent.
        let doc = one_page_pdf(PageFixture {
            media_box: None,
            pages_media_box: Some([0, 0, 300, 400]),
            ..PageFixture::for_content(b"BT /F1 12 Tf 10 10 Td (Hi) Tj ET")
        });

        let graph = inspect(&doc);
        assert_eq!(graph.media_box, [0.0, 0.0, 300.0, 400.0]);
    }

    #[test]
    fn page_rotation_is_read_and_normalized() {
        let doc = one_page_pdf(PageFixture {
            helvetica_f1: false,
            page_extras: vec![("Rotate", 450.into())], // normalizes to 90
            ..PageFixture::for_content(b"")
        });

        let graph = inspect(&doc);
        assert_eq!(graph.rotation, 90);
    }

    #[test]
    fn empty_page_has_no_islands() {
        let doc = build_pdf(b"");
        let graph = inspect(&doc);
        assert!(graph.islands.is_empty());
    }

    #[test]
    fn build_spans_merges_contiguous_glyphs_and_splits_on_gap() {
        let addr_a = super::SourceAddress {
            stream_ref: (1, 0),
            contents_index: 0,
            operator_index: 0,
            operand_index: 0,
        };
        let make_glyph = |unicode: &str, glyph_start: u32, glyph_end: u32| PositionedGlyph {
            page: 1,
            unicode: unicode.to_string(),
            cid: unicode.as_bytes().first().copied().unwrap_or(0) as u32,
            origin: (0.0, 0.0),
            quad: [0.0; 8],
            source: addr_a.with_glyph((glyph_start, glyph_end)),
            font_id: "F1".to_string(),
            font_size: 12.0,
            text_matrix: Matrix::IDENTITY,
            ctm: Matrix::IDENTITY,
            writing_direction: WritingDirection::LeftToRight,
            paint: PaintState {
                render_mode: TextRenderMode::Fill,
                fill_color: None,
                stroke_color: None,
            },
        };
        let run = vec![
            make_glyph("A", 0, 1),
            make_glyph("B", 1, 2),
            make_glyph("C", 5, 6),
        ];
        let spans = build_spans(&run);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].expected_text, "AB");
        assert_eq!(spans[1].expected_text, "C");
    }
}
