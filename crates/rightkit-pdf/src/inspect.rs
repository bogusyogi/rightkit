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
//!
//! ## Editability (`PdfTextIsland::editable`)
//!
//! `editable` answers "can a replacement be written through this island's own font?". The
//! decision lives in one place, [`ReverseTable`] (built once per font in `resolve_page_fonts`),
//! and the composer's in-place write reads the same table, so the page graph and `compose_candidate`
//! cannot disagree. A font that can never be re-encoded makes the island `editable: false` with a
//! typed [`PdfEditBlocker`]; a replacement that merely needs a glyph the subset lacks stays
//! editable and fails at compose time with `PdfEditError::MissingGlyphs` naming the characters.
#![cfg_attr(not(feature = "edit"), allow(dead_code))]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use lopdf::content::Content;
use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::model::{
    compute_island_digest, document_sha256, expected_text_digest, sha256_hex, DecodedGlyph,
    GapNumber, GlyphSourceAddress, IslandLayout, Matrix, PaintState, PdfColor, PdfEditBlocker,
    PdfEditError, PdfPageGraph, PdfRect, PdfSourceLocator, PdfSourceSpan, PdfTextAppearance,
    PdfTextGap, PdfTextIsland, PositionedGlyph, RunItem, SourceAddress, TextRenderMode, TextState,
    TextStateFont, WritingDirection,
};

/// Mirrors `pdf_text_edit.rs`'s own bound: plenty for real ToUnicode CMaps / simple-font
/// encodings while bounding malicious compressed font streams.
const MAX_FONT_ENCODING_BYTES: usize = 16 * 1024 * 1024;

/// Most bytes any one stream (a page content stream, an object stream) may decode to. A page of
/// text is kilobytes to a few megabytes; a flate stream that claims more is a decompression bomb
/// and is refused as malformed content instead of being allocated.
pub(crate) const MAX_DECOMPRESSED_BYTES: usize = 256 * 1024 * 1024;

/// Loads a document with every stream's decoded size bounded by [`MAX_DECOMPRESSED_BYTES`].
pub(crate) fn load_pdf(bytes: &[u8]) -> Result<Document, lopdf::Error> {
    Document::load_mem_with_options(
        bytes,
        lopdf::LoadOptions::with_max_decompressed_size(MAX_DECOMPRESSED_BYTES),
    )
}

// -------------------------------------------------------------------------------------------
// Byte entry point
// -------------------------------------------------------------------------------------------

/// Loads `bytes` as a PDF and walks page `page`, deriving the document digest itself.
///
/// This is the seam the app crate's `inspect_pdf_page` command calls, so the Tauri adapter never
/// needs `lopdf` in scope — it authorizes a path, reads it, and hands the bytes here.
pub fn page_graph_from_bytes(bytes: &[u8], page: u32) -> Result<PdfPageGraph, PdfEditError> {
    let doc = load_pdf(bytes).map_err(|_| PdfEditError::MalformedContent)?;
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
    let ext_gstates = page_ext_gstates(doc, page_dict);

    let mut glyphs: Vec<PositionedGlyph> = Vec::new();
    let mut painted_bounds: Vec<PaintedBoundsEntry> = Vec::new();
    let mut rejections: Vec<PdfTextIsland> = Vec::new();
    let mut spacing: HashMap<SpacingKey, GlyphSpacing> = HashMap::new();

    // A page's `/Contents` array is ONE logical content stream split at token boundaries (ISO
    // 32000-1 7.8.2): graphics and text state (`Tf`, `Tc`, `Tw`, `cm`, `q`...) carry from one
    // stream into the next. Each stream is still decoded and addressed separately (sparse writes
    // need the boundaries), but the walker state flows through them.
    let mut walk_state = WalkState::new();
    for stream in &streams {
        let content = Content::decode(&stream.bytes).map_err(|_| PdfEditError::MalformedContent)?;
        walk_state = walk_stream(
            stream,
            &content,
            page,
            document_sha256,
            &fonts,
            &ext_gstates,
            walk_state,
            &mut glyphs,
            &mut painted_bounds,
            &mut rejections,
            &mut spacing,
        )?;
    }

    let mut islands = group_islands(
        page,
        document_sha256,
        glyphs,
        &painted_bounds,
        &fonts,
        &spacing,
    );
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

/// The page's `/Resources /ExtGState` entries, each mapped to whether it carries a `/Font`
/// (which switches the text font without a `Tf`). Resources are inherited like any page
/// attribute, through the bounded [`resolve_inherited`].
fn page_ext_gstates(doc: &Document, page_dict: &Dictionary) -> HashMap<Vec<u8>, bool> {
    let mut out = HashMap::new();
    let Some(resources) = resolve_inherited(doc, page_dict, b"Resources")
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_dict().ok())
    else {
        return out;
    };
    let Some(states) = resources
        .get(b"ExtGState")
        .ok()
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_dict().ok())
    else {
        return out;
    };
    for (name, entry) in states.iter() {
        let sets_font = match deref_obj(doc, entry).and_then(|o| o.as_dict().ok()) {
            Some(dict) => dict.has(b"Font"),
            None => true, // unreadable: assume the worst
        };
        out.insert(name.clone(), sets_font);
    }
    out
}

/// The resource categories a content stream names (`Tf`, `Do`, `gs`, `cs`/`scn`, `sh`, `BDC`).
const RESOURCE_CATEGORIES: [&[u8]; 7] = [
    b"Font",
    b"XObject",
    b"ExtGState",
    b"ColorSpace",
    b"Pattern",
    b"Shading",
    b"Properties",
];

/// What one resource name resolves to on a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResourceTarget {
    /// An indirect object that exists in the document.
    Reference(ObjectId),
    /// An inline object.
    Direct,
}

/// Every `(category, name)` a page's content can resolve, keyed by page number.
pub(crate) type ResourceFingerprint = BTreeMap<u32, BTreeMap<(Vec<u8>, Vec<u8>), ResourceTarget>>;

/// What each page's *effective* resources resolve to: the nearest `/Resources` on the page or its
/// `/Parent` chain (bounded, see [`resolve_inherited`]), which is the dictionary a viewer uses,
/// never a merge of levels. A name whose indirect target does not exist is not recorded: it
/// resolved to nothing before and cannot be lost.
pub(crate) fn resource_fingerprint(doc: &Document) -> ResourceFingerprint {
    let mut out = ResourceFingerprint::new();
    for (number, page_id) in doc.get_pages() {
        let mut names = BTreeMap::new();
        let resources = doc
            .get_object(page_id)
            .ok()
            .and_then(|o| o.as_dict().ok())
            .and_then(|page| resolve_inherited(doc, page, b"Resources"))
            .and_then(|o| deref_obj(doc, o))
            .and_then(|o| o.as_dict().ok());
        if let Some(resources) = resources {
            for category in RESOURCE_CATEGORIES {
                let Some(group) = resources
                    .get(category)
                    .ok()
                    .and_then(|o| deref_obj(doc, o))
                    .and_then(|o| o.as_dict().ok())
                else {
                    continue;
                };
                for (name, entry) in group.iter() {
                    let target = match entry {
                        Object::Reference(id) => {
                            if doc.get_object(*id).is_err() {
                                continue;
                            }
                            ResourceTarget::Reference(*id)
                        }
                        _ => ResourceTarget::Direct,
                    };
                    names.insert((category.to_vec(), name.clone()), target);
                }
            }
        }
        out.insert(number, names);
    }
    out
}

/// True when every page in `before` still resolves every resource name it resolved, to the same
/// target. An edit may add resources (a new font); it must never hide one the page already used.
pub(crate) fn resources_preserved(
    before: &ResourceFingerprint,
    after: &ResourceFingerprint,
) -> bool {
    before.iter().all(|(page, names)| {
        after.get(page).is_some_and(|now| {
            names
                .iter()
                .all(|(key, target)| now.get(key) == Some(target))
        })
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

/// Resolves a page's `/Contents` value to its ordered content-stream object ids.
///
/// `/Contents` is a stream reference, a direct array of stream references, **or an indirect
/// reference to an array** of stream references (ISO 32000-1 7.7.3.3: "an array or a stream").
/// The indirect-array form is what Acrobat-class producers emit (e.g. `uscis-i9.pdf`, where
/// `/Contents 1636 0 R` is an array of eight streams); reading every `Reference` as one stream
/// made those pages fail with `MalformedContent`. Shared by inspection, composition and
/// save-verification so they cannot disagree about what a page contains.
pub(crate) fn content_stream_ids(
    doc: &Document,
    contents: &Object,
) -> Result<Vec<ObjectId>, PdfEditError> {
    fn references(items: &[Object]) -> Vec<ObjectId> {
        items
            .iter()
            .filter_map(|o| match o {
                Object::Reference(id) => Some(*id),
                _ => None,
            })
            .collect()
    }
    Ok(match contents {
        Object::Reference(id) => match doc
            .get_object(*id)
            .map_err(|_| PdfEditError::MalformedContent)?
        {
            Object::Array(items) => references(items),
            // A stream (callers verify the type) - or a malformed target they will reject.
            _ => vec![*id],
        },
        Object::Array(items) => references(items),
        _ => Vec::new(),
    })
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
    let ids = content_stream_ids(doc, contents)?;

    let mut streams = Vec::with_capacity(ids.len());
    for (idx, id) in ids.into_iter().enumerate() {
        let obj = doc
            .get_object(id)
            .map_err(|_| PdfEditError::MalformedContent)?;
        let stream = obj
            .as_stream()
            .map_err(|_| PdfEditError::MalformedContent)?;
        let bytes = stream
            .decompressed_content_with_limit(MAX_DECOMPRESSED_BYTES)
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

/// Deepest `/Parent` chain followed when looking up an inherited page attribute. Real page trees
/// are a handful of levels; a chain longer than this is hostile or corrupt.
const MAX_INHERIT_DEPTH: usize = 64;

/// Looks `key` up on `dict`, then up its `/Parent` chain. Bounded two ways: a visited set stops a
/// cycle (a page whose `/Parent` is itself, two nodes pointing at each other) and
/// [`MAX_INHERIT_DEPTH`] stops a very long acyclic chain. Either way the answer is `None`.
fn resolve_inherited<'a>(
    doc: &'a Document,
    mut dict: &'a Dictionary,
    key: &[u8],
) -> Option<&'a Object> {
    let mut seen: HashSet<ObjectId> = HashSet::new();
    for _ in 0..=MAX_INHERIT_DEPTH {
        if let Ok(v) = dict.get(key) {
            return Some(v);
        }
        let parent = dict.get(b"Parent").ok()?;
        let parent_id = match parent {
            Object::Reference(id) => *id,
            _ => return None,
        };
        if !seen.insert(parent_id) {
            return None;
        }
        let parent_obj = doc.get_object(parent_id).ok()?;
        dict = parent_obj.as_dict().ok()?;
    }
    None
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
    /// The one re-encodability decision for this font: `Ok` when replacement text can be written
    /// through it (every usable code, exactly reversible from data in the file), `Err` with the
    /// typed reason when it can never be. The page graph's `editable` flag and the composer's
    /// `ExactOperandPatch` both read this field, so they cannot disagree.
    reverse: Result<ReverseTable, PdfEditBlocker>,
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

// -------------------------------------------------------------------------------------------
// Re-encodability: the single decision shared by the page graph and the composer
// -------------------------------------------------------------------------------------------

/// Which of a font's codes the file itself defines, so that a character-to-code mapping can be
/// inverted exactly without guessing.
enum CodePolicy {
    /// Every code the decode table covers (a named base encoding, a `ToUnicode` CMap, or the
    /// Standard encoding of a non-embedded Standard-14 text font).
    Any,
    /// Only these codes: `/Differences` without a `/BaseEncoding` on an embedded font, where the
    /// engine's Standard-encoding base is a guess for any code the file does not list.
    Only(HashSet<u32>),
    /// The font can never be re-encoded.
    Refuse(PdfEditBlocker),
}

/// Exactly reversible character-to-code table for one font.
///
/// A code is usable only when (1) it decodes to exactly one character through the font's own
/// decode table, (2) the file defines that decode ([`CodePolicy`]), and (3) the file gives it a
/// positive explicit width. Subset producers write a zero or absent width for glyphs they left out
/// of the embedded program, so (3) is the in-file evidence that the glyph exists; it is inferred,
/// not read from the font program (see the README's limits). A character with no usable code is
/// never substituted: [`ReverseTable::encode`] reports it instead.
#[derive(Debug)]
pub(crate) struct ReverseTable {
    code_width: u8,
    by_char: HashMap<char, (u32, f64)>,
}

/// Whether word spacing (`Tw`) applies to a glyph: only to the **single-byte character code 32**
/// (ISO 32000-1 9.3.3), whatever that code decodes to, and never to a multi-byte (CID) code, even
/// one whose value is 32. A Unicode space reached through any other code gets no `Tw`; code 32
/// mapped to another character (a `/Differences` entry, a subset font) still does.
fn takes_word_spacing(code: u32, code_width: u32) -> bool {
    code_width == 1 && code == 32
}

/// [`takes_word_spacing`] for a glyph whose source byte range is known.
fn glyph_takes_word_spacing(code: u32, source_range: (u32, u32)) -> bool {
    takes_word_spacing(code, source_range.1.saturating_sub(source_range.0))
}

/// Replacement text encoded through a font's [`ReverseTable`].
#[derive(Clone, Debug)]
pub(crate) struct EncodedRun {
    /// The string-operand bytes to write (1 byte per code, or 2 bytes big-endian for Identity-H).
    pub bytes: Vec<u8>,
    chars: Vec<char>,
    widths_1000: Vec<f64>,
    /// The code written for each character, in order (`bytes` split every `code_width` bytes).
    codes: Vec<u32>,
    code_width: u8,
}

impl EncodedRun {
    /// Number of encoded characters (one code each).
    pub(crate) fn char_count(&self) -> usize {
        self.chars.len()
    }

    /// True when the code written at `index` is one `Tw` applies to (single-byte code 32).
    pub(crate) fn word_space_at(&self, index: usize) -> bool {
        self.codes
            .get(index)
            .is_some_and(|&code| takes_word_spacing(code, u32::from(self.code_width)))
    }

    /// True when the run's last code is one `Tw` applies to.
    pub(crate) fn ends_with_word_space(&self) -> bool {
        self.codes
            .len()
            .checked_sub(1)
            .is_some_and(|last| self.word_space_at(last))
    }

    /// Text-space length of the run along the baseline, mirroring `visit_text_show`'s pen
    /// advance exactly: each glyph advances `w/1000 * size + Tc (+ Tw for single-byte code 32)`,
    /// scaled by `Tz`; the last glyph contributes only its own width (the island box ends at its
    /// right edge, before any trailing `Tc`/`Tw`).
    pub(crate) fn advance(
        &self,
        font_size: f64,
        char_spacing: f64,
        word_spacing: f64,
        horizontal_scale: f64,
    ) -> f64 {
        let count = self.codes.len();
        let mut total = 0.0;
        for (index, &width) in self.widths_1000.iter().enumerate() {
            total += width / 1000.0 * font_size;
            if index + 1 < count {
                total += char_spacing;
                if self.word_space_at(index) {
                    total += word_spacing;
                }
            }
        }
        total * horizontal_scale
    }
}

impl ReverseTable {
    fn build(
        code_width: u8,
        unicode: &HashMap<u32, String>,
        widths_1000: &HashMap<u32, f64>,
        policy: &CodePolicy,
    ) -> Result<ReverseTable, PdfEditBlocker> {
        if let CodePolicy::Refuse(blocker) = policy {
            return Err(*blocker);
        }
        let max_code: u32 = if code_width == 1 { 0xFF } else { 0xFFFF };

        let mut codes: Vec<u32> = unicode.keys().copied().collect();
        codes.sort_unstable(); // deterministic alias resolution, independent of HashMap order

        // char -> every usable (code, width) that decodes to exactly that char.
        let mut candidates: HashMap<char, Vec<(u32, f64)>> = HashMap::new();
        for code in codes {
            if code > max_code {
                continue;
            }
            if let CodePolicy::Only(allowed) = policy {
                if !allowed.contains(&code) {
                    continue;
                }
            }
            let mut chars = unicode[&code].chars();
            let (Some(ch), None) = (chars.next(), chars.next()) else {
                continue; // unmapped, or a multi-character value such as a ligature
            };
            if ch == '\u{FFFD}' {
                continue; // the decoder's replacement character, not a mapping
            }
            let Some(&width) = widths_1000.get(&code) else {
                continue;
            };
            if !(width.is_finite() && width > 0.0) {
                continue;
            }
            candidates.entry(ch).or_default().push((code, width));
        }

        let mut by_char: HashMap<char, (u32, f64)> = HashMap::new();
        for (ch, options) in candidates {
            if code_width == 1 {
                // Encoding aliases (notably WinAnsi space at 0x20 & 0xA0) must not depend on
                // HashMap order: prefer the identity byte, else the lowest code.
                let chosen = options
                    .iter()
                    .copied()
                    .find(|(code, _)| char::from_u32(*code) == Some(ch))
                    .unwrap_or(options[0]);
                by_char.insert(ch, chosen);
            } else if options.len() == 1 {
                // Two CIDs carrying the same character may be different glyph variants; with no
                // way to tell which one the author sees, the character is left unencodable.
                by_char.insert(ch, options[0]);
            }
        }

        if by_char.is_empty() {
            return Err(PdfEditBlocker::NoReversibleEncoding);
        }
        Ok(ReverseTable {
            code_width,
            by_char,
        })
    }

    /// Encodes `text`, or returns the distinct characters (first-appearance order) the font has no
    /// usable code for. Never substitutes.
    fn encode(&self, text: &str) -> Result<EncodedRun, Vec<char>> {
        let mut bytes = Vec::with_capacity(text.len() * usize::from(self.code_width));
        let mut chars = Vec::new();
        let mut widths_1000 = Vec::new();
        let mut codes = Vec::new();
        let mut missing: Vec<char> = Vec::new();
        for ch in text.chars() {
            match self.by_char.get(&ch) {
                Some(&(code, width)) => {
                    if self.code_width == 1 {
                        bytes.push(code as u8);
                    } else {
                        bytes.extend_from_slice(&(code as u16).to_be_bytes());
                    }
                    chars.push(ch);
                    widths_1000.push(width);
                    codes.push(code);
                }
                None => {
                    if !missing.contains(&ch) {
                        missing.push(ch);
                    }
                }
            }
        }
        if missing.is_empty() {
            Ok(EncodedRun {
                bytes,
                chars,
                widths_1000,
                codes,
                code_width: self.code_width,
            })
        } else {
            Err(missing)
        }
    }
}

/// Why [`encode_for_font`] could not produce bytes.
#[cfg(feature = "edit")]
pub(crate) enum ReencodeFailure {
    /// The font can never be re-encoded (the island is not `editable`).
    Blocked,
    /// The font is re-encodable but lacks these characters.
    MissingGlyphs(Vec<char>),
}

/// Encodes `text` through the page font resource `font_id` using the same [`ReverseTable`] that
/// decided the page graph's `editable` flag. The composer's `ExactOperandPatch` calls this.
#[cfg(all(feature = "edit", test))]
pub(crate) fn encode_for_font(
    doc: &Document,
    page_id: ObjectId,
    font_id: &str,
    text: &str,
) -> Result<EncodedRun, ReencodeFailure> {
    let fonts = resolve_page_fonts(doc, page_id);
    let font = fonts
        .get(font_id.as_bytes())
        .ok_or(ReencodeFailure::Blocked)?;
    let table = font
        .reverse
        .as_ref()
        .map_err(|_| ReencodeFailure::Blocked)?;
    table.encode(text).map_err(ReencodeFailure::MissingGlyphs)
}

/// A replacement middle encoded for in-place writing. With a real space glyph in the font the
/// whole middle is one piece (spaces are glyphs); without one, the middle is split at every space
/// into words and the gaps between them are the composer's to regenerate.
#[cfg(feature = "edit")]
pub(crate) struct EncodedMiddle {
    pub pieces: Vec<EncodedRun>,
    pub space_glyph: bool,
}

/// Encodes `middle` through the page font `font_id` (one table resolution for all pieces).
/// Missing characters are collected across every piece. A space is never "missing" here: if the
/// font has no space glyph, spaces become gaps.
#[cfg(feature = "edit")]
pub(crate) fn encode_middle_for_font(
    doc: &Document,
    page_id: ObjectId,
    font_id: &str,
    middle: &str,
) -> Result<EncodedMiddle, ReencodeFailure> {
    let fonts = resolve_page_fonts(doc, page_id);
    let font = fonts
        .get(font_id.as_bytes())
        .ok_or(ReencodeFailure::Blocked)?;
    let table = font
        .reverse
        .as_ref()
        .map_err(|_| ReencodeFailure::Blocked)?;
    let space_glyph = table.encode(" ").is_ok();
    let texts: Vec<&str> = if space_glyph {
        vec![middle]
    } else {
        middle.split(' ').collect()
    };
    let mut pieces = Vec::with_capacity(texts.len());
    let mut missing: Vec<char> = Vec::new();
    for text in texts {
        match table.encode(text) {
            Ok(run) => pieces.push(run),
            Err(chars) => {
                for ch in chars {
                    if !missing.contains(&ch) {
                        missing.push(ch);
                    }
                }
            }
        }
    }
    if missing.is_empty() {
        Ok(EncodedMiddle {
            pieces,
            space_glyph,
        })
    } else {
        Err(ReencodeFailure::MissingGlyphs(missing))
    }
}

/// Bytes that write `text` verbatim into page font resource `resource_name`, or `None` when the
/// font cannot encode every character. Thin test-facing view over [`encode_for_font`].
#[cfg(all(feature = "edit", test))]
pub(crate) fn simple_font_identity_bytes(
    doc: &Document,
    page_id: ObjectId,
    resource_name: &[u8],
    text: &str,
) -> Option<Vec<u8>> {
    let font_id = String::from_utf8_lossy(resource_name).into_owned();
    encode_for_font(doc, page_id, &font_id, text)
        .ok()
        .map(|run| run.bytes)
}

/// Standard-14 *text* faces: their built-in encoding is StandardEncoding by definition (Symbol and
/// ZapfDingbats have their own and are excluded).
fn is_standard_text_font(base_font: &[u8]) -> bool {
    std::str::from_utf8(base_font)
        .map(|name| STANDARD14_NAMES[..12].contains(&name))
        .unwrap_or(false)
}

fn font_program_is_embedded(doc: &Document, font_dict: &Dictionary) -> bool {
    font_dict
        .get(b"FontDescriptor")
        .ok()
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_dict().ok())
        .is_some_and(|d| d.has(b"FontFile") || d.has(b"FontFile2") || d.has(b"FontFile3"))
}

/// Codes listed in a `/Differences` array (mirrors the engine's own cursor rule: an integer sets
/// the next code, every name takes one code and advances).
fn differences_codes(items: &[Object]) -> HashSet<u32> {
    let mut codes = HashSet::new();
    let mut current: i64 = 0;
    for item in items {
        match item {
            Object::Integer(code) => current = *code,
            Object::Name(_) => {
                if (0..=255).contains(&current) {
                    codes.insert(current as u32);
                }
                current += 1;
            }
            _ => {}
        }
    }
    codes
}

/// Decides which codes of a *simple* font the file defines exactly. `decoded` is what the PDF
/// library actually built from `/Encoding`: it silently falls back to StandardEncoding when it
/// cannot read an encoding dictionary (no `/Type`, no `/Differences`, an unknown glyph name),
/// which would decode and re-encode codes the font does not mean. Each branch therefore also
/// requires the expected decode variant, and refuses otherwise.
fn simple_code_policy(
    doc: &Document,
    font_dict: &Dictionary,
    decoded: Option<&lopdf::Encoding<'_>>,
) -> CodePolicy {
    let refuse = CodePolicy::Refuse(PdfEditBlocker::NoReversibleEncoding);
    let to_unicode_stream = font_dict
        .get(b"ToUnicode")
        .ok()
        .and_then(|o| deref_obj(doc, o))
        .is_some_and(|o| o.as_stream().is_ok());

    let Ok(encoding_entry) = font_dict.get(b"Encoding") else {
        // No /Encoding.
        if to_unicode_stream {
            return if matches!(decoded, Some(lopdf::Encoding::UnicodeMapEncoding(_))) {
                CodePolicy::Any
            } else {
                refuse
            };
        }
        let base_font = font_dict
            .get(b"BaseFont")
            .ok()
            .and_then(|o| o.as_name().ok())
            .unwrap_or(b"");
        return if is_standard_text_font(base_font)
            && !font_program_is_embedded(doc, font_dict)
            && matches!(decoded, Some(lopdf::Encoding::OneByteEncoding(_)))
        {
            CodePolicy::Any
        } else {
            refuse
        };
    };

    match deref_obj(doc, encoding_entry) {
        Some(Object::Name(name)) => {
            if matches!(
                name.as_slice(),
                b"WinAnsiEncoding" | b"MacRomanEncoding" | b"StandardEncoding"
            ) && matches!(decoded, Some(lopdf::Encoding::OneByteEncoding(_)))
            {
                CodePolicy::Any
            } else {
                refuse
            }
        }
        Some(Object::Dictionary(dict)) => {
            let typed = matches!(
                dict.get(b"Type").ok().and_then(|o| o.as_name().ok()),
                Some(b"Encoding")
            );
            let differences = dict
                .get(b"Differences")
                .ok()
                .and_then(|o| deref_obj(doc, o))
                .and_then(|o| o.as_array().ok());
            let (true, Some(differences)) = (typed, differences) else {
                return refuse;
            };
            if !matches!(decoded, Some(lopdf::Encoding::Differences(_))) {
                return refuse;
            }
            match dict.get(b"BaseEncoding") {
                Ok(base) => match base.as_name() {
                    Ok(b"WinAnsiEncoding" | b"MacRomanEncoding" | b"StandardEncoding") => {
                        CodePolicy::Any
                    }
                    _ => refuse,
                },
                Err(_) => CodePolicy::Only(differences_codes(differences)),
            }
        }
        _ => refuse,
    }
}

/// True when the page's `/Parent` chain ends within [`MAX_INHERIT_DEPTH`] hops without revisiting a
/// node. The PDF library's font lookup recurses once per ancestor; a hostile chain thousands of
/// nodes deep must not reach it.
fn parent_chain_is_bounded(doc: &Document, page_id: ObjectId) -> bool {
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut id = page_id;
    for _ in 0..=MAX_INHERIT_DEPTH {
        let Ok(dict) = doc.get_object(id).and_then(|o| o.as_dict()) else {
            return true;
        };
        match dict.get(b"Parent") {
            Ok(Object::Reference(parent)) => {
                if !seen.insert(*parent) {
                    return false;
                }
                id = *parent;
            }
            _ => return true,
        }
    }
    false
}

fn resolve_page_fonts(doc: &Document, page_id: ObjectId) -> HashMap<Vec<u8>, Rc<BuiltFont>> {
    let mut out = HashMap::new();
    if !parent_chain_is_bounded(doc, page_id) {
        return out; // no fonts: every show operator becomes a typed unresolved-font island
    }
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
            let encoding = font_dict
                .get_font_encoding_with_limit(doc, MAX_FONT_ENCODING_BYTES)
                .ok();
            let unicode: HashMap<u32, String> = encoding
                .as_ref()
                .map(|encoding| {
                    (0u32..=255)
                        .filter_map(|code| {
                            Document::decode_text(encoding, &[code as u8])
                                .ok()
                                .filter(|s| !s.is_empty())
                                .map(|s| (code, s))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let code_policy = simple_code_policy(doc, font_dict, encoding.as_ref());
            let (default_width_1000, first_char, last_char, widths) =
                parse_simple_widths(doc, font_dict);
            // Driven by the `/Widths` array the file actually contains, never by the numbers
            // `/FirstChar` and `/LastChar` claim: `/LastChar 9223372036854775807` with one width
            // is one iteration. A simple font has 256 codes; anything outside is ignored.
            let mut widths_1000 = HashMap::new();
            for (idx, w) in widths.iter().enumerate() {
                let Some(code) = first_char.checked_add(idx as i64) else {
                    break;
                };
                if code > last_char {
                    break;
                }
                if (0..=255).contains(&code) {
                    widths_1000.insert(code as u32, *w);
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
            let reverse = ReverseTable::build(1, &unicode, &widths_1000, &code_policy);
            BuiltFont {
                id: font_id,
                code_width: 1,
                default_width_1000,
                widths_1000,
                reject_missing_metrics,
                unicode,
                reverse,
            }
        } else {
            let (unicode, cmap_complete) = parse_to_unicode(doc, font_dict);
            let (default_width_1000, widths_1000, widths_complete) =
                parse_cid_widths(doc, font_dict);
            // Identity-H: the 2-byte code is the CID, `ToUnicode` is the only character table.
            // A CMap with any entry or form this reader does not understand is not an exact
            // table: the font is not reversibly encoded, whatever the entries it did read say.
            // The same holds for a `/W` array that exhausted its work budget: the widths the
            // reader kept are not the widths the file gives, so no fit check can be trusted, and
            // a code with no kept width is rejected rather than measured with a guessed one.
            let reverse = if !cmap_complete {
                Err(PdfEditBlocker::NoReversibleEncoding)
            } else if !widths_complete {
                Err(PdfEditBlocker::MissingMetrics)
            } else {
                ReverseTable::build(2, &unicode, &widths_1000, &CodePolicy::Any)
            };
            BuiltFont {
                id: font_id,
                code_width: 2,
                default_width_1000,
                widths_1000,
                reject_missing_metrics: default_width_1000 <= 0.0 || !widths_complete,
                unicode,
                reverse,
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

/// Largest CID an `Identity-H` font can show: codes are two bytes. `/W` entries beyond it can
/// never be looked up, so they are neither stored nor iterated.
const MAX_CID: u32 = 0xFFFF;

/// A `/W` CID operand as an in-range CID, or `None` (negative, fractional-garbage, NaN, or past
/// [`MAX_CID`]). File numbers are never cast blindly: `f64 as u32` saturates silently.
fn cid_operand(obj: &Object) -> Option<u32> {
    let n = object_number(obj)?;
    (n.is_finite() && (0.0..=f64::from(MAX_CID)).contains(&n)).then_some(n as u32)
}

/// Total work a single font's `/W` array may cost: each CID width assigned counts one. A real
/// `/W` assigns at most 65 536 CIDs; this allows several full passes and stops an array of
/// thousands of overlapping full-width ranges (`0 65535 500` repeated), which clamping each
/// range alone does not: 40 000 repetitions are still 2.6 billion insertions.
const MAX_CID_WIDTH_ASSIGNMENTS: usize = 1 << 20;

/// Reads `/DW` and `/W` of the first descendant font. The third value is `false` when the width
/// table is not exact: the work budget ran out, so CIDs that the file gives a width may be missing
/// from the map. Such a font is displayed (missing codes are rejected, never guessed) but is not
/// reversibly encoded, because a fit check needs every width.
fn parse_cid_widths(doc: &Document, font_dict: &Dictionary) -> (f64, HashMap<u32, f64>, bool) {
    let (dw, map, complete, _) = parse_cid_widths_within(doc, font_dict, MAX_CID_WIDTH_ASSIGNMENTS);
    (dw, map, complete)
}

/// [`parse_cid_widths`] with an explicit total budget. The fourth value counts the assignments
/// actually performed; each is charged against `budget_total` before it is made, so it never
/// exceeds the budget.
fn parse_cid_widths_within(
    doc: &Document,
    font_dict: &Dictionary,
    budget_total: usize,
) -> (f64, HashMap<u32, f64>, bool, usize) {
    let mut dw = 1000.0;
    let mut map = HashMap::new();
    let mut complete = true;

    let Ok(desc_obj) = font_dict.get(b"DescendantFonts") else {
        return (dw, map, complete, 0);
    };
    let Some(Object::Array(desc_arr)) = deref_obj(doc, desc_obj) else {
        return (dw, map, complete, 0);
    };
    let Some(first) = desc_arr.first() else {
        return (dw, map, complete, 0);
    };
    let Some(cid_obj) = deref_obj(doc, first) else {
        return (dw, map, complete, 0);
    };
    let Ok(cid_dict) = cid_obj.as_dict() else {
        return (dw, map, complete, 0);
    };

    if let Some(v) = cid_dict.get(b"DW").ok().and_then(object_number) {
        dw = v;
    }

    let mut budget = budget_total;
    if let Ok(w_obj) = cid_dict.get(b"W") {
        if let Some(Object::Array(items)) = deref_obj(doc, w_obj) {
            let mut i = 0usize;
            'entries: while i < items.len() {
                if object_number(&items[i]).is_none() {
                    break;
                }
                if i + 1 >= items.len() {
                    break;
                }
                match deref_obj(doc, &items[i + 1]) {
                    Some(Object::Array(widths)) => {
                        // `c_first [w0 w1 ...]`: bounded by the array, by the CID space and by the
                        // work budget.
                        if let Some(start) = cid_operand(&items[i]) {
                            for (offset, w) in widths.iter().enumerate() {
                                let Some(cid) = start.checked_add(offset as u32) else {
                                    break;
                                };
                                if cid > MAX_CID {
                                    break;
                                }
                                if budget == 0 {
                                    complete = false;
                                    break 'entries;
                                }
                                budget -= 1;
                                if let Some(wv) = object_number(w) {
                                    map.insert(cid, wv);
                                }
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
                        // `c_first c_last w`: clamp the range to the CID space (at most 65 536
                        // entries however the file words it), skip a range that starts outside
                        // it, and charge the whole range to the work budget.
                        if let Some(start) = cid_operand(&items[i]) {
                            if c_last.is_finite() && c_last >= f64::from(start) {
                                let end = c_last.min(f64::from(MAX_CID)) as u32;
                                let count = (end - start) as usize + 1;
                                if count > budget {
                                    complete = false;
                                    break 'entries;
                                }
                                budget -= count;
                                for cid in start..=end {
                                    map.insert(cid, wv);
                                }
                            }
                        }
                        i += 3;
                    }
                }
            }
        }
    }

    (dw, map, complete, budget_total - budget)
}

/// Total work a single `ToUnicode` CMap may cost: each (range) code assigned counts one. A real
/// CMap assigns at most 65 536 codes; this allows several full passes and stops a stream of
/// millions of overlapping full-width ranges.
const MAX_CMAP_ASSIGNMENTS: usize = 1 << 20;

#[derive(Debug, PartialEq)]
enum CmapToken {
    Hex(String),
    Open,
    Close,
}

/// Tokenizes a `bfchar`/`bfrange` block body: `<hex>` strings and `[` `]`. Anything else is
/// reported through `clean` (false) and skipped, so the caller knows the table is not exact.
fn cmap_tokens(block: &str, clean: &mut bool) -> Vec<CmapToken> {
    let mut out = Vec::new();
    let mut chars = block.chars();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                let mut tok = String::new();
                let mut closed = false;
                for c2 in chars.by_ref() {
                    if c2 == '>' {
                        closed = true;
                        break;
                    }
                    if c2.is_ascii_hexdigit() {
                        tok.push(c2);
                    } else if !c2.is_whitespace() {
                        *clean = false;
                    }
                }
                if !closed {
                    *clean = false;
                }
                out.push(CmapToken::Hex(tok));
            }
            '[' => out.push(CmapToken::Open),
            ']' => out.push(CmapToken::Close),
            c if c.is_whitespace() => {}
            _ => *clean = false,
        }
    }
    out
}

/// UTF-16 code units of a `bfrange` destination. A destination must be whole UTF-16 units (4 hex
/// digits each); a short one is read as one unit, the way a bare number always was.
fn range_destination_units(tok: &str) -> Option<Vec<u16>> {
    if tok.is_empty() || tok.len() > 8 {
        return None;
    }
    let padded = if tok.len() <= 4 {
        format!("{tok:0>4}")
    } else if tok.len().is_multiple_of(4) {
        tok.to_string()
    } else {
        return None;
    };
    let units: Option<Vec<u16>> = (0..padded.len())
        .step_by(4)
        .map(|i| u16::from_str_radix(&padded[i..i + 4], 16).ok())
        .collect();
    units
}

fn units_to_string(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// Minimal owned `ToUnicode` CMap parser: `beginbfchar`/`endbfchar` and `beginbfrange`/
/// `endbfrange` blocks, including the array-of-destinations form of `bfrange`
/// (`<lo> <hi> [<d0> <d1> ...]`, where code `lo + i` maps to `d_i`).
///
/// Returns the map and whether it is *exact*: `false` when any entry was malformed, truncated,
/// of an unsupported form, or cut off by the work budget (and for `usecmap`, which pulls in a
/// table this reader does not load). An inexact CMap may still decode text for display, but the
/// font is not reversibly encoded, so the caller must not write through it. Source codes are
/// limited to the two-byte space (`<= 0xFFFF`) of an `Identity-H` font; destinations are never
/// invented, and all arithmetic on file numbers is checked.
fn parse_to_unicode(doc: &Document, font_dict: &Dictionary) -> (HashMap<u32, String>, bool) {
    let mut map = HashMap::new();
    let mut exact = true;

    let Ok(tu_obj) = font_dict.get(b"ToUnicode") else {
        return (map, exact);
    };
    let Some(obj) = deref_obj(doc, tu_obj) else {
        return (map, exact);
    };
    let Ok(stream) = obj.as_stream() else {
        return (map, exact);
    };
    let bytes = stream
        .decompressed_content_with_limit(MAX_FONT_ENCODING_BYTES)
        .unwrap_or_else(|_| stream.content.clone());
    // A stream that decodes past the limit falls back to its raw bytes, which are not a CMap:
    // the table comes out empty (or inexact) and the font is not editable.
    if bytes.len() > MAX_FONT_ENCODING_BYTES {
        return (map, false);
    }
    let text = String::from_utf8_lossy(&bytes);
    if text.contains("usecmap") {
        exact = false;
    }
    let mut budget = MAX_CMAP_ASSIGNMENTS;

    for block in extract_blocks(&text, "beginbfchar", "endbfchar") {
        let tokens = cmap_tokens(block, &mut exact);
        for pair in tokens.chunks(2) {
            let [CmapToken::Hex(src), CmapToken::Hex(dst)] = pair else {
                exact = false;
                continue;
            };
            let Some(code) = hex_to_u32(src) else {
                exact = false;
                continue;
            };
            if budget == 0 {
                exact = false;
                break;
            }
            budget -= 1;
            if code <= MAX_CID {
                map.insert(code, hex_to_unicode_string(dst));
            }
        }
    }

    for block in extract_blocks(&text, "beginbfrange", "endbfrange") {
        let tokens = cmap_tokens(block, &mut exact);
        let mut at = 0usize;
        while at < tokens.len() {
            let (Some(CmapToken::Hex(lo)), Some(CmapToken::Hex(hi))) =
                (tokens.get(at), tokens.get(at + 1))
            else {
                exact = false;
                break;
            };
            let (Some(lo_code), Some(hi_code)) = (hex_to_u32(lo), hex_to_u32(hi)) else {
                exact = false;
                break;
            };
            // Destination: one scalar, or a bracketed array.
            let (destinations, next): (Vec<String>, usize) = match tokens.get(at + 2) {
                Some(CmapToken::Hex(dst)) => {
                    // Scalar form: `lo + i` maps to dst with its last UTF-16 unit advanced by i.
                    let Some(units) = range_destination_units(dst) else {
                        exact = false;
                        at += 3;
                        continue;
                    };
                    if lo_code > hi_code || lo_code > MAX_CID {
                        exact = false;
                        at += 3;
                        continue;
                    }
                    let hi_code = hi_code.min(MAX_CID);
                    let count = (hi_code - lo_code) as usize + 1;
                    if count > budget {
                        exact = false;
                        break;
                    }
                    budget -= count;
                    let (last, head) = units.split_last().expect("non-empty by construction");
                    for offset in 0..=(hi_code - lo_code) {
                        let Some(unit) =
                            u16::try_from(offset).ok().and_then(|o| last.checked_add(o))
                        else {
                            exact = false; // destination would run past U+FFFF
                            break;
                        };
                        let mut full = head.to_vec();
                        full.push(unit);
                        map.insert(lo_code + offset, units_to_string(&full));
                    }
                    at += 3;
                    continue;
                }
                Some(CmapToken::Open) => {
                    // Array form.
                    let mut dests = Vec::new();
                    let mut idx = at + 3;
                    let mut closed = false;
                    while let Some(tok) = tokens.get(idx) {
                        match tok {
                            CmapToken::Hex(h) => dests.push(hex_to_unicode_string(h)),
                            CmapToken::Close => {
                                closed = true;
                                idx += 1;
                                break;
                            }
                            CmapToken::Open => {
                                exact = false;
                            }
                        }
                        idx += 1;
                    }
                    if !closed {
                        exact = false;
                        break; // unbalanced: the rest of the block cannot be trusted
                    }
                    (dests, idx)
                }
                _ => {
                    exact = false;
                    break;
                }
            };
            // Array form: code `lo + i` maps to the i-th listed destination, and only those.
            if lo_code > hi_code || lo_code > MAX_CID {
                exact = false;
                at = next;
                continue;
            }
            let wanted = (hi_code.min(MAX_CID) - lo_code) as usize + 1;
            if destinations.len() as u64 != u64::from(hi_code - lo_code) + 1 {
                exact = false; // a short or long array: map what is listed, invent nothing
            }
            for (offset, dst) in destinations.into_iter().take(wanted).enumerate() {
                if budget == 0 {
                    exact = false;
                    break;
                }
                budget -= 1;
                map.insert(lo_code + offset as u32, dst);
            }
            at = next;
        }
    }

    (map, exact)
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

/// Deepest `q` nesting the walker follows. Real producers stay in the tens (viewers document 28);
/// a content stream of millions of unbalanced `q` would otherwise grow the stack without bound.
/// A page that nests deeper is refused as malformed content.
const MAX_GRAPHICS_STATE_DEPTH: usize = 1 << 16;

/// The graphics state `q` saves and `Q` restores. ISO 32000-1 8.4.2 and 9.3.1 put the text state
/// parameters - font and size (`Tf`), `Tc`, `Tw`, `Tz`, `TL`, `Ts`, `Tr` - in the graphics state,
/// so `Q` brings them back together with the CTM and the paint; the text matrix and text line
/// matrix are not part of it and `Q` leaves them alone.
struct SavedGraphicsState {
    ctm: Matrix,
    paint: PaintState,
    font: Option<Rc<dyn TextStateFont>>,
    font_size: f64,
    char_spacing: f64,
    word_spacing: f64,
    horizontal_scale: f64,
    leading: f64,
    text_rise: f64,
    render_mode: TextRenderMode,
}

impl SavedGraphicsState {
    fn capture(ctm: Matrix, paint: &PaintState, text: &TextState) -> SavedGraphicsState {
        SavedGraphicsState {
            ctm,
            paint: paint.clone(),
            font: text.font.clone(),
            font_size: text.font_size,
            char_spacing: text.char_spacing,
            word_spacing: text.word_spacing,
            horizontal_scale: text.horizontal_scale,
            leading: text.leading,
            text_rise: text.text_rise,
            render_mode: text.render_mode,
        }
    }

    /// Writes the saved state back. The text matrices are deliberately not touched.
    fn restore(self, ctm: &mut Matrix, paint: &mut PaintState, text: &mut TextState) {
        *ctm = self.ctm;
        *paint = self.paint;
        text.font = self.font;
        text.font_size = self.font_size;
        text.char_spacing = self.char_spacing;
        text.word_spacing = self.word_spacing;
        text.horizontal_scale = self.horizontal_scale;
        text.leading = self.leading;
        text.text_rise = self.text_rise;
        text.render_mode = self.render_mode;
    }
}

/// Most numeric `TJ` items one page may push into gap provenance, however they are grouped. Each
/// item is moved into the gap in front of the next real string, never copied, so retained state is
/// linear in the items; this bound turns "linear in a 256 MiB stream of `0 0 0 ...`" into a fixed
/// few megabytes. Past it a gap simply has no provenance, which only makes its island
/// `positioned_text_run` (not editable).
const MAX_GAP_NUMBERS_PER_PAGE: usize = 1 << 20;

/// Numeric `TJ` items seen since the last non-empty string was shown, with no positioning/state
/// operator in between: the adjustments that act before the next string, across empty strings and
/// across show operators (`[(a) -333] TJ (b) Tj`).
#[derive(Debug)]
struct TrailingGaps {
    numbers: Vec<GapNumber>,
    /// Items were dropped (the page budget ran out) since the gap began, so `numbers` is not the
    /// whole gap.
    truncated: bool,
    /// Items this page may still record.
    budget: usize,
}

impl TrailingGaps {
    fn new() -> TrailingGaps {
        TrailingGaps::with_budget(MAX_GAP_NUMBERS_PER_PAGE)
    }

    fn with_budget(budget: usize) -> TrailingGaps {
        TrailingGaps {
            numbers: Vec::new(),
            truncated: false,
            budget,
        }
    }

    /// A positioning/state operator ends the gap: its numbers explain nothing after it.
    fn clear(&mut self) {
        self.numbers.clear();
        self.truncated = false;
    }

    fn push(&mut self, number: GapNumber) {
        if self.budget == 0 {
            self.truncated = true;
            return;
        }
        self.budget -= 1;
        self.numbers.push(number);
    }

    /// Moves the accumulated numbers out for the next *real* glyph-bearing string and starts a new
    /// gap. An empty string must never call this (it shows nothing, so the numbers still belong
    /// to the next real string): cloning here once per empty operand retained a copy of the whole
    /// gap per operand, which is quadratic. A gap that lost items to the budget is reported empty
    /// rather than partial, because a partial list would pass for the whole gap.
    fn take(&mut self) -> Vec<GapNumber> {
        let numbers = std::mem::take(&mut self.numbers);
        if std::mem::take(&mut self.truncated) {
            return Vec::new();
        }
        numbers
    }
}

/// Walker state that survives from one `/Contents` stream into the next.
struct WalkState {
    gs_stack: Vec<SavedGraphicsState>,
    ctm: Matrix,
    paint: PaintState,
    text_state: TextState,
    path_points: Vec<(f64, f64)>,
    /// Set by any text-positioning/state operator, consumed by the next show operator.
    break_pending: bool,
    trailing: TrailingGaps,
}

impl WalkState {
    fn new() -> WalkState {
        WalkState {
            gs_stack: Vec::new(),
            ctm: Matrix::IDENTITY,
            paint: PaintState {
                render_mode: TextRenderMode::Fill,
                fill_color: None,
                stroke_color: None,
            },
            text_state: TextState::new(),
            path_points: Vec::new(),
            break_pending: true,
            trailing: TrailingGaps::new(),
        }
    }
}

/// `Tc`/`Tw`/`Tz` in force at one show operator, keyed by the operator's address. The island
/// layout reads it so a replacement is measured with the same pen advance the walker used.
#[derive(Clone)]
struct GlyphSpacing {
    char_spacing: f64,
    word_spacing: f64,
    horizontal_scale: f64,
    /// Numeric `TJ` items immediately before this string operand, in the same array.
    preceding: Vec<GapNumber>,
    /// A text-positioning or state operator (`Td`, `Tm`, `Tf`, `cm`, `q`, ...) ran between the
    /// previous show operator and this one.
    break_before: bool,
}

type SpacingKey = ((u32, u16), u32, u32, u32);

fn spacing_key(source: &SourceAddress) -> SpacingKey {
    (
        source.stream_ref,
        source.contents_index,
        source.operator_index,
        source.operand_index,
    )
}

fn glyph_spacing_key(source: &GlyphSourceAddress) -> SpacingKey {
    (
        source.stream_ref,
        source.contents_index,
        source.operator_index,
        source.operand_index,
    )
}

#[allow(clippy::too_many_arguments)]
fn walk_stream(
    stream: &PageStream,
    content: &Content,
    page: u32,
    document_sha256: &str,
    fonts: &HashMap<Vec<u8>, Rc<BuiltFont>>,
    ext_gstates: &HashMap<Vec<u8>, bool>,
    state: WalkState,
    glyphs: &mut Vec<PositionedGlyph>,
    painted_bounds: &mut Vec<PaintedBoundsEntry>,
    rejections: &mut Vec<PdfTextIsland>,
    spacing: &mut HashMap<SpacingKey, GlyphSpacing>,
) -> Result<WalkState, PdfEditError> {
    let WalkState {
        mut gs_stack,
        mut ctm,
        mut paint,
        mut text_state,
        mut path_points,
        mut break_pending,
        mut trailing,
    } = state;

    for (op_idx, op) in content.operations.iter().enumerate() {
        let source = SourceAddress {
            stream_ref: stream.stream_ref,
            contents_index: stream.contents_index,
            operator_index: op_idx as u32,
            operand_index: 0,
        };

        // Anything that can move or restyle the pen between two shows is a break in the run.
        if matches!(
            op.operator.as_str(),
            "Td" | "TD"
                | "Tm"
                | "T*"
                | "'"
                | "\""
                | "Tf"
                | "Tc"
                | "Tw"
                | "Tz"
                | "TL"
                | "Ts"
                | "Tr"
                | "cm"
                | "q"
                | "Q"
                | "BT"
                | "ET"
        ) {
            break_pending = true;
            trailing.clear();
        }
        match op.operator.as_str() {
            "gs" => {
                // An ExtGState may set the font (`/Font [ref size]`), which no `Tf` announces: the
                // walker would keep decoding through the previous font. A gs that sets it - or one
                // this page's resources cannot resolve - drops the active font and is a seam in
                // the run; text shown until the next `Tf` is an unresolved-font island.
                let name = op.operands.first().and_then(|o| o.as_name().ok());
                let sets_font = name
                    .and_then(|n| ext_gstates.get(n).copied())
                    .unwrap_or(true);
                if sets_font {
                    text_state.font = None;
                    break_pending = true;
                    trailing.clear();
                }
            }
            "q" => {
                if gs_stack.len() >= MAX_GRAPHICS_STATE_DEPTH {
                    return Err(PdfEditError::MalformedContent);
                }
                gs_stack.push(SavedGraphicsState::capture(ctm, &paint, &text_state));
            }
            "Q" => {
                if let Some(saved) = gs_stack.pop() {
                    saved.restore(&mut ctm, &mut paint, &mut text_state);
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
                    // An empty string shows nothing: the adjustments before it still belong to
                    // the gap in front of the next real string, so they are neither copied nor
                    // taken here.
                    let preceding = if is_empty_string(operand) {
                        Vec::new()
                    } else {
                        trailing.take()
                    };
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
                        spacing,
                        preceding,
                        &mut break_pending,
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
                        spacing,
                        Vec::new(),
                        &mut break_pending,
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
                        spacing,
                        Vec::new(),
                        &mut break_pending,
                    );
                }
            }
            "TJ" => {
                text_state.ctm = ctm;
                if let Some(Object::Array(items)) = op.operands.first() {
                    for (item_idx, item) in items.iter().enumerate() {
                        match item {
                            Object::String(..) => {
                                // An empty string shows nothing: the adjustments around it still
                                // belong to the gap before the next real string, so they stay in
                                // `trailing` (cloning them here per empty operand is quadratic
                                // in an array of empty strings). Only a real string takes them.
                                let preceding = if is_empty_string(item) {
                                    Vec::new()
                                } else {
                                    trailing.take()
                                };
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
                                    spacing,
                                    preceding,
                                    &mut break_pending,
                                );
                            }
                            Object::Integer(_) | Object::Real(_) => {
                                let value = object_number(item).unwrap_or(0.0);
                                trailing.push(GapNumber {
                                    operator_index: op_idx as u32,
                                    item_index: item_idx as u32,
                                    value,
                                });
                                let adj = -(value / 1000.0)
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

    Ok(WalkState {
        gs_stack,
        ctm,
        paint,
        text_state,
        path_points,
        break_pending,
        trailing,
    })
}

fn is_empty_string(obj: &Object) -> bool {
    matches!(obj, Object::String(bytes, _) if bytes.is_empty())
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
    spacing: &mut HashMap<SpacingKey, GlyphSpacing>,
    preceding: Vec<GapNumber>,
    break_pending: &mut bool,
) {
    let mut source_op = source.clone();
    source_op.operand_index = operand_index;

    if text_state.font.is_none() {
        rejections.push(rejected_island(
            document_sha256,
            page,
            rejections.len(),
            PdfEditError::UnsupportedEncoding,
            Some(PdfEditBlocker::UnsupportedFontType),
        ));
        return;
    }
    if is_singular(text_state.ctm) || is_singular(text_state.text_matrix) {
        rejections.push(rejected_island(
            document_sha256,
            page,
            rejections.len(),
            PdfEditError::UnsupportedTransform,
            None,
        ));
        return;
    }

    // An empty string produces no glyph, so nothing ever looks its spacing up; recording one per
    // empty operand would only grow the map.
    if !is_empty_string(operand) {
        spacing.insert(
            spacing_key(&source_op),
            GlyphSpacing {
                char_spacing: text_state.char_spacing,
                word_spacing: text_state.word_spacing,
                horizontal_scale: text_state.horizontal_scale,
                preceding,
                break_before: false,
            },
        );
    }

    let before = glyphs.len();
    let shown = visit_text_show(text_state, &source_op, operand, page, paint, glyphs);
    if shown.is_ok() && glyphs.len() > before {
        if let Some(entry) = spacing.get_mut(&spacing_key(&source_op)) {
            entry.break_before = std::mem::take(break_pending);
        }
    }
    if let Err(error) = shown {
        glyphs.truncate(before);
        // `decode_show_operand` raises UnsupportedEncoding only for a code the font has no
        // authoritative width for; other failures (odd-length operand, degenerate quad) have no
        // font-level blocker.
        let blocker = matches!(error, PdfEditError::UnsupportedEncoding)
            .then_some(PdfEditBlocker::MissingMetrics);
        rejections.push(rejected_island(
            document_sha256,
            page,
            rejections.len(),
            PdfEditError::UnsupportedEncoding,
            blocker,
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
        // `Tw` follows the encoded byte, not the character: single-byte code 32 only (see
        // `takes_word_spacing`).
        let advance = font.advance(glyph.cid, state.font_size)
            + state.char_spacing
            + if glyph_takes_word_spacing(glyph.cid, glyph.source_range) {
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
    blocker: Option<PdfEditBlocker>,
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
        edit_blocker: blocker,
        substitution_editable: false,
        layout: None,
        gaps: Vec::new(),
        run: Vec::new(),
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
    fonts: &HashMap<Vec<u8>, Rc<BuiltFont>>,
    spacing: &HashMap<SpacingKey, GlyphSpacing>,
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
        .map(|(idx, run)| build_island(page, document_sha256, idx, run, fonts, spacing))
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

/// Device-space length of the text-space x unit at a glyph: the `(a, b)` row of
/// `text matrix x CTM`, i.e. how far one text-space unit along the baseline moves on the page.
fn glyph_axis(glyph: &PositionedGlyph) -> (f64, f64) {
    let combined = glyph.text_matrix.then(glyph.ctm);
    (combined.a, combined.b)
}

/// Relative tolerance when comparing sizes and scales across the glyphs of one island.
const TEXT_STATE_TOLERANCE: f64 = 1e-6;

/// Absolute tolerance (text-space units) when comparing `Tc`/`Tw` across an island. Producers
/// nudge `Tw` by thousandths to justify a line; that is far below anything a fit check can see,
/// and the layout uses the first span's value.
const SPACING_TOLERANCE: f64 = 0.01;

fn same_value(a: f64, b: f64) -> bool {
    (a - b).abs() <= TEXT_STATE_TOLERANCE * a.abs().max(b.abs()).max(1.0)
}

/// The island-level half of the re-encodability decision. `None` means a replacement can be
/// written through the island's own font; `Some` is the typed reason it cannot. One replacement
/// string is written back at the island's first span, so every glyph must share the font, the
/// effective size (font size x text/CTM scale), the spacing and one content stream; otherwise the
/// result would look different from the source. The font-level half is `BuiltFont::reverse`.
fn island_edit_blocker(
    run: &[PositionedGlyph],
    spans: &[PdfSourceSpan],
    items: &[RunItem],
    fonts: &HashMap<Vec<u8>, Rc<BuiltFont>>,
    spacing: &HashMap<SpacingKey, GlyphSpacing>,
) -> Option<PdfEditBlocker> {
    let first = &run[0];
    let first_axis = glyph_axis(first);
    let first_scale = first_axis.0.hypot(first_axis.1);
    let first_spacing = spacing.get(&glyph_spacing_key(&first.source));
    for glyph in run {
        let axis = glyph_axis(glyph);
        let glyph_spacing = spacing.get(&glyph_spacing_key(&glyph.source));
        let same_spacing = match (first_spacing, glyph_spacing) {
            (Some(a), Some(b)) => {
                (a.char_spacing - b.char_spacing).abs() <= SPACING_TOLERANCE
                    && (a.word_spacing - b.word_spacing).abs() <= SPACING_TOLERANCE
                    && same_value(a.horizontal_scale, b.horizontal_scale)
            }
            _ => false,
        };
        if glyph.font_id != first.font_id
            || !same_value(glyph.font_size, first.font_size)
            || !same_value(axis.0.hypot(axis.1), first_scale)
            || !same_spacing
        {
            return Some(PdfEditBlocker::MixedTextState);
        }
    }
    // A code the CMap does not map decodes to nothing, or to U+FFFD when the PDF library's
    // CMap decoder supplied its replacement character; either way the text would lose the glyph.
    if run
        .iter()
        .any(|glyph| glyph.unicode.is_empty() || glyph.unicode.contains('\u{FFFD}'))
    {
        return Some(PdfEditBlocker::UnmappedGlyphs);
    }
    if spans.iter().any(|span| {
        span.stream_ref != spans[0].stream_ref || span.contents_index != spans[0].contents_index
    }) {
        return Some(PdfEditBlocker::SpansAcrossStreams);
    }
    // A positioning or state operator between two operators of the run: the second operator's
    // glyphs are placed by that operator, so merging them into one string would move them.
    for pair in run.windows(2) {
        let (prev, cur) = (&pair[0].source, &pair[1].source);
        let new_operator = prev.stream_ref != cur.stream_ref
            || prev.contents_index != cur.contents_index
            || prev.operator_index != cur.operator_index;
        if new_operator
            && spacing
                .get(&glyph_spacing_key(cur))
                .is_some_and(|s| s.break_before)
        {
            return Some(PdfEditBlocker::PositionedTextRun);
        }
    }
    // A word gap made by a position move cannot be rebuilt if an edit touches it.
    if items
        .iter()
        .any(|item| item.glyph.is_none() && item.gap_numbers.is_none())
    {
        return Some(PdfEditBlocker::PositionedTextRun);
    }
    match fonts.get(first.font_id.as_bytes()) {
        None => Some(PdfEditBlocker::UnsupportedFontType),
        Some(font) => font.reverse.as_ref().err().copied(),
    }
}

/// A gap between two glyphs counts as a word space when its pen distance, beyond what `Tc`
/// already adds, exceeds this fraction of the em (font size x text/CTM scale).
///
/// TeX-class producers write no space glyphs: an inter-word space is a `TJ` adjustment of about
/// -250..-330 thousandths of an em (0.25-0.33 em), while kerns inside words stay under about
/// 0.1 em. Text extractors draw the same line: pdf.js/pdfminer.six treat a gap above roughly 0.1-0.3
/// em as a space (their defaults are expressed against glyph width or the em, from memory and not
/// re-verified here). 0.2 em sits between the two populations; it is the one tunable.
const GAP_SPACE_EM: f64 = 0.2;

/// Tolerance (device points) when deciding that a gap is entirely explained by the `TJ`
/// adjustments in front of the next string rather than by a position move.
const GAP_PROVENANCE_TOLERANCE_PT: f64 = 0.05;

/// Builds the island's run in reading order: real glyphs and, between them, a synthesised space
/// wherever the pen gap passes [`GAP_SPACE_EM`] and neither neighbour is a real space.
fn build_run(run: &[PositionedGlyph], spacing: &HashMap<SpacingKey, GlyphSpacing>) -> Vec<RunItem> {
    let first_axis = glyph_axis(&run[0]);
    let first_scale = first_axis.0.hypot(first_axis.1);
    let unit = if first_scale > 0.0 {
        (first_axis.0 / first_scale, first_axis.1 / first_scale)
    } else {
        (1.0, 0.0)
    };
    let along = |x: f64, y: f64| x * unit.0 + y * unit.1;

    let mut items: Vec<RunItem> = Vec::with_capacity(run.len() * 2);
    for (index, glyph) in run.iter().enumerate() {
        let start = along(glyph.quad[0], glyph.quad[1]);
        let end = along(glyph.quad[2], glyph.quad[3]);
        if index > 0 {
            let prev = &run[index - 1];
            let prev_end = items.last().map(|item| item.end).unwrap_or(start);
            let prev_spacing = spacing.get(&glyph_spacing_key(&prev.source));
            let cur_spacing = spacing.get(&glyph_spacing_key(&glyph.source));
            let prev_axis = glyph_axis(prev);
            let prev_scale = prev_axis.0.hypot(prev_axis.1);
            let tc_dev = prev_spacing
                .map(|s| s.char_spacing * s.horizontal_scale * prev_scale)
                .unwrap_or(0.0);
            // A glyph that takes `Tw` (single-byte code 32) advances the pen by `Tw` as well, and
            // that jump is explained by the text state exactly as `Tc` is: it is not a gap.
            let tw_dev = if glyph_takes_word_spacing(
                prev.cid,
                (prev.source.glyph_start, prev.source.glyph_end),
            ) {
                prev_spacing
                    .map(|s| s.word_spacing * s.horizontal_scale * prev_scale)
                    .unwrap_or(0.0)
            } else {
                0.0
            };
            let excess = start - prev_end - tc_dev - tw_dev;
            let em_dev = prev.font_size * prev_scale;
            let adjacent_space = prev.unicode.ends_with(' ') || glyph.unicode.starts_with(' ');
            if excess > GAP_SPACE_EM * em_dev && !adjacent_space {
                let cur_axis = glyph_axis(glyph);
                let cur_scale = cur_axis.0.hypot(cur_axis.1);
                let gap_numbers = cur_spacing.and_then(|s| {
                    let explained: f64 = s
                        .preceding
                        .iter()
                        .map(|n| -(n.value / 1000.0) * glyph.font_size * s.horizontal_scale)
                        .sum::<f64>()
                        * cur_scale;
                    (!s.preceding.is_empty()
                        && (excess - explained).abs() <= GAP_PROVENANCE_TOLERANCE_PT)
                        .then(|| s.preceding.clone())
                });
                items.push(RunItem {
                    glyph: None,
                    text: " ".to_string(),
                    start: prev_end,
                    end: start,
                    gap_numbers,
                    beyond_tc: excess,
                    word_space: false,
                });
            }
        }
        items.push(RunItem {
            glyph: Some(glyph.source),
            text: glyph.unicode.clone(),
            start,
            end,
            gap_numbers: None,
            beyond_tc: 0.0,
            word_space: glyph_takes_word_spacing(
                glyph.cid,
                (glyph.source.glyph_start, glyph.source.glyph_end),
            ),
        });
    }
    items
}

fn build_island(
    page: u32,
    document_sha256: &str,
    index: usize,
    run: Vec<PositionedGlyph>,
    fonts: &HashMap<Vec<u8>, Rc<BuiltFont>>,
    spacing: &HashMap<SpacingKey, GlyphSpacing>,
) -> PdfTextIsland {
    let items = build_run(&run, spacing);
    let text: String = items.iter().map(|item| item.text.as_str()).collect();
    let mut gaps: Vec<PdfTextGap> = Vec::new();
    let mut char_index = 0usize;
    for item in &items {
        if item.glyph.is_none() {
            gaps.push(PdfTextGap {
                char_index,
                width_pt: item.beyond_tc,
            });
        }
        char_index += item.text.chars().count();
    }
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

    let edit_blocker = island_edit_blocker(&run, &spans, &items, fonts, spacing);
    // The composer removes the old glyphs from one content stream per edit and refuses an island
    // whose spans live in several, so a catalog-font substitution is only promised for one stream.
    let substitution_editable = spans.iter().all(|span| {
        span.stream_ref == spans[0].stream_ref && span.contents_index == spans[0].contents_index
    });
    let first_spacing = spacing.get(&glyph_spacing_key(&first.source)).cloned();
    let layout = first_spacing.map(|glyph_spacing| IslandLayout {
        axis: glyph_axis(first),
        font_size: first.font_size,
        char_spacing: glyph_spacing.char_spacing,
        word_spacing: glyph_spacing.word_spacing,
        horizontal_scale: glyph_spacing.horizontal_scale,
    });

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
        editable: edit_blocker.is_none(),
        rejection: edit_blocker.map(|_| PdfEditError::UnsupportedEncoding),
        edit_blocker,
        substitution_editable,
        layout,
        gaps,
        run: items,
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
            assert_eq!(island.edit_blocker, Some(PdfEditBlocker::MissingMetrics));
            assert!(!island.substitution_editable);
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
            reverse: Err(PdfEditBlocker::NoReversibleEncoding),
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

    // -----------------------------------------------------------------------------------
    // Re-encodability: ReverseTable
    // -----------------------------------------------------------------------------------

    fn unicode_map(entries: &[(u32, &str)]) -> HashMap<u32, String> {
        entries.iter().map(|(c, s)| (*c, s.to_string())).collect()
    }

    fn width_map(entries: &[(u32, f64)]) -> HashMap<u32, f64> {
        entries.iter().copied().collect()
    }

    #[test]
    fn reverse_table_encodes_exactly_and_names_every_missing_character() {
        // 160 aliases the space; "fi" is a ligature value; code 68 has a zero width.
        let table = ReverseTable::build(
            1,
            &unicode_map(&[
                (65, "A"),
                (66, "B"),
                (32, " "),
                (160, " "),
                (67, "fi"),
                (68, "D"),
            ]),
            &width_map(&[
                (65, 500.0),
                (66, 600.0),
                (32, 250.0),
                (160, 250.0),
                (67, 700.0),
                (68, 0.0),
            ]),
            &CodePolicy::Any,
        )
        .expect("table");
        // The identity byte wins the alias.
        assert_eq!(
            table.encode("AB A").expect("encodes").bytes,
            vec![65, 66, 32, 65]
        );
        // Ligature value, zero-width glyph and an unknown character are all unencodable, named
        // once each in first-appearance order. Nothing is substituted.
        assert_eq!(table.encode("ACDA!C").err(), Some(vec!['C', 'D', '!']));
    }

    #[test]
    fn encoded_run_advance_mirrors_the_walker_pen_rules() {
        let table = ReverseTable::build(
            1,
            &unicode_map(&[(65, "A"), (66, "B"), (32, " ")]),
            &width_map(&[(65, 500.0), (66, 600.0), (32, 250.0)]),
            &CodePolicy::Any,
        )
        .expect("table");
        let run = table.encode("A B").expect("encodes");
        // widths 5 + 2.5 + 6 = 13.5; Tc after A and after the space = 2; Tw after the space = 2;
        // nothing after the last glyph; Tz 0.5 scales the lot.
        let advance = run.advance(10.0, 1.0, 2.0, 0.5);
        assert!((advance - 8.75).abs() < 1e-9, "{advance}");
    }

    #[test]
    fn reverse_table_cid_leaves_ambiguous_characters_unencodable() {
        let table = ReverseTable::build(
            2,
            &unicode_map(&[(1, "H"), (2, "i"), (3, "i")]),
            &width_map(&[(1, 600.0), (2, 300.0), (3, 300.0)]),
            &CodePolicy::Any,
        )
        .expect("table");
        assert_eq!(table.encode("H").expect("encodes").bytes, vec![0, 1]);
        assert_eq!(table.encode("Hi").err(), Some(vec!['i']));
    }

    #[test]
    fn reverse_table_honours_policy_and_reports_the_blocker() {
        let unicode = unicode_map(&[(65, "A"), (66, "B")]);
        let widths = width_map(&[(65, 500.0), (66, 500.0)]);
        let only = ReverseTable::build(1, &unicode, &widths, &CodePolicy::Only([65].into()))
            .expect("table");
        assert_eq!(only.encode("B").err(), Some(vec!['B']));
        assert_eq!(
            ReverseTable::build(
                1,
                &unicode,
                &widths,
                &CodePolicy::Refuse(PdfEditBlocker::UnsupportedFontType)
            )
            .err(),
            Some(PdfEditBlocker::UnsupportedFontType)
        );
        // Codes with no explicit width are not evidence the glyph exists.
        assert_eq!(
            ReverseTable::build(1, &unicode, &HashMap::new(), &CodePolicy::Any).err(),
            Some(PdfEditBlocker::NoReversibleEncoding)
        );
    }

    // -----------------------------------------------------------------------------------
    // Re-encodability: page graph
    // -----------------------------------------------------------------------------------

    use crate::test_fixtures::{
        custom_page_pdf, differences_encoding, helvetica_font, identity_h_font,
        strict_to_unicode_cmap, subset_type1_font,
    };

    fn saved(doc: &Document) -> Vec<u8> {
        let mut out = Vec::new();
        doc.clone().save_to(&mut out).expect("fixture saves");
        out
    }

    #[test]
    fn editable_island_serializes_additive_fields_without_changing_existing_names() {
        let graph = inspect(&build_pdf(b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET"));
        let island = &graph.islands[0];
        assert!(island.editable);
        assert_eq!(island.edit_blocker, None);
        assert!(island.substitution_editable);
        let layout = island.layout.expect("layout captured");
        assert_eq!(layout.axis, (1.0, 0.0));
        assert_eq!(layout.font_size, 12.0);

        let json = serde_json::to_value(island).expect("serializes");
        assert_eq!(json["editable"], true);
        assert_eq!(json["rejection"], serde_json::Value::Null);
        assert_eq!(json["editBlocker"], serde_json::Value::Null);
        assert_eq!(json["substitutionEditable"], true);
        assert!(json.get("layout").is_none(), "layout is not on the wire");
    }

    #[test]
    fn embedded_symbolic_font_without_encoding_is_not_editable() {
        // A subset font with a built-in encoding and no /Encoding, no ToUnicode (TeX math fonts):
        // the engine would decode its codes as StandardEncoding, which the file does not say.
        let doc = custom_page_pdf(
            |d| vec![("F1", subset_type1_font(d, 65, &[500, 600], None, None))],
            &[b"BT /F1 12 Tf 72 700 Td (AB) Tj ET"],
            false,
        );
        let graph = inspect(&doc);
        let island = &graph.islands[0];
        assert_eq!(island.text, "AB");
        assert!(!island.quads.is_empty(), "still a real, selectable island");
        assert!(!island.editable);
        assert_eq!(
            island.edit_blocker,
            Some(PdfEditBlocker::NoReversibleEncoding)
        );
        assert!(matches!(
            island.rejection,
            Some(PdfEditError::UnsupportedEncoding)
        ));
        assert!(island.substitution_editable);

        let json = serde_json::to_value(island).expect("serializes");
        assert_eq!(json["editBlocker"], "no_reversible_encoding");
        assert_eq!(json["rejection"]["code"], "unsupported_encoding");
    }

    #[test]
    fn encoding_dictionary_the_library_cannot_read_is_refused() {
        // No /Type: the PDF library silently falls back to StandardEncoding for this dictionary.
        let encoding = differences_encoding(
            None,
            false,
            vec![Object::Integer(65), Object::Name(b"A".to_vec())],
        );
        let doc = custom_page_pdf(
            |d| vec![("F1", subset_type1_font(d, 65, &[500], Some(encoding), None))],
            &[b"BT /F1 12 Tf 72 700 Td (A) Tj ET"],
            false,
        );
        let island = &inspect(&doc).islands[0];
        assert!(!island.editable);
        assert_eq!(
            island.edit_blocker,
            Some(PdfEditBlocker::NoReversibleEncoding)
        );
    }

    #[test]
    fn differences_without_base_encoding_define_only_the_listed_codes() {
        let encoding = differences_encoding(
            None,
            true,
            vec![
                Object::Integer(65),
                Object::Name(b"A".to_vec()),
                Object::Name(b"B".to_vec()),
            ],
        );
        // Widths exist for 65..=67, but only 65 and 66 are in /Differences.
        let doc = custom_page_pdf(
            |d| {
                vec![(
                    "F1",
                    subset_type1_font(d, 65, &[500, 600, 700], Some(encoding), None),
                )]
            },
            &[b"BT /F1 12 Tf 72 700 Td (AB) Tj ET"],
            false,
        );
        let island = &inspect(&doc).islands[0];
        assert!(island.editable, "{:?}", island.edit_blocker);
        assert_eq!(island.edit_blocker, None);

        #[cfg(feature = "edit")]
        {
            let page_id = *doc.get_pages().get(&1).expect("page");
            assert_eq!(
                encode_for_font(&doc, page_id, "F1", "BA")
                    .ok()
                    .map(|r| r.bytes),
                Some(vec![66, 65])
            );
            // 'C' decodes (Standard base) at 67 and has a width, but the file does not define it.
            assert!(matches!(
                encode_for_font(&doc, page_id, "F1", "ABC"),
                Err(ReencodeFailure::MissingGlyphs(chars)) if chars == vec!['C']
            ));
        }
    }

    #[test]
    fn simple_font_with_only_a_to_unicode_cmap_is_editable() {
        let cmap = strict_to_unicode_cmap(&[(1, 'H'), (2, 'i')]);
        let doc = custom_page_pdf(
            |d| {
                vec![(
                    "F1",
                    subset_type1_font(d, 1, &[600, 300], None, Some(&cmap)),
                )]
            },
            &[b"BT /F1 12 Tf 72 700 Td <0102> Tj ET"],
            false,
        );
        let island = &inspect(&doc).islands[0];
        assert_eq!(island.text, "Hi");
        assert!(island.editable, "{:?}", island.edit_blocker);
    }

    #[test]
    fn kerned_tj_island_spans_several_operands_and_is_editable() {
        let graph = inspect(&build_pdf(b"BT /F1 12 Tf 72 700 Td [(Hel) -20 (lo)] TJ ET"));
        let island = &graph.islands[0];
        assert_eq!(island.text, "Hello");
        assert_eq!(island.locator.spans.len(), 2);
        assert!(island.editable, "{:?}", island.edit_blocker);
    }

    #[test]
    fn mixed_fonts_or_sizes_in_one_island_are_not_editable() {
        let two_fonts = custom_page_pdf(
            |d| vec![("F1", helvetica_font(d)), ("F2", helvetica_font(d))],
            &[b"BT /F1 12 Tf 72 700 Td (Hi) Tj /F2 12 Tf (Yo) Tj ET"],
            false,
        );
        let island = &inspect(&two_fonts).islands[0];
        assert_eq!(island.text, "HiYo");
        assert!(!island.editable);
        assert_eq!(island.edit_blocker, Some(PdfEditBlocker::MixedTextState));

        let two_sizes = build_pdf(b"BT /F1 12 Tf 72 700 Td (Hi) Tj /F1 14 Tf (Yo) Tj ET");
        let island = &inspect(&two_sizes).islands[0];
        assert_eq!(island.text, "HiYo");
        assert_eq!(island.edit_blocker, Some(PdfEditBlocker::MixedTextState));
    }

    #[test]
    fn unmapped_glyph_blocks_editing_instead_of_being_dropped_silently() {
        // CID 2 has a width but no ToUnicode entry: the island text would omit it.
        let doc = custom_page_pdf(
            |d| vec![("F1", identity_h_font(d, &[(1, 600), (2, 300)], &[(1, 'H')]))],
            &[b"BT /F1 12 Tf 72 700 Td <00010002> Tj ET"],
            false,
        );
        let island = &inspect(&doc).islands[0];
        assert_eq!(island.text, "H");
        assert!(!island.editable);
        assert_eq!(island.edit_blocker, Some(PdfEditBlocker::UnmappedGlyphs));
    }

    #[test]
    fn identity_h_font_with_to_unicode_and_widths_is_editable_with_two_byte_codes() {
        let doc = custom_page_pdf(
            |d| {
                vec![(
                    "F1",
                    identity_h_font(d, &[(1, 600), (2, 300)], &[(1, 'H'), (2, 'i')]),
                )]
            },
            &[b"BT /F1 12 Tf 72 700 Td <00010002> Tj ET"],
            false,
        );
        let island = &inspect(&doc).islands[0];
        assert_eq!(island.text, "Hi");
        assert!(island.editable, "{:?}", island.edit_blocker);

        #[cfg(feature = "edit")]
        {
            let page_id = *doc.get_pages().get(&1).expect("page");
            assert_eq!(
                encode_for_font(&doc, page_id, "F1", "iH")
                    .ok()
                    .map(|r| r.bytes),
                Some(vec![0, 2, 0, 1])
            );
        }
    }

    #[test]
    fn type3_font_island_is_rejected_with_a_typed_blocker() {
        let doc = custom_page_pdf(
            |d| {
                vec![(
                    "F1",
                    d.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type3" }),
                )]
            },
            &[b"BT /F1 12 Tf 72 700 Td (Hi) Tj ET"],
            false,
        );
        let island = &inspect(&doc).islands[0];
        assert!(!island.editable);
        assert_eq!(
            island.edit_blocker,
            Some(PdfEditBlocker::UnsupportedFontType)
        );
        assert!(!island.substitution_editable);
    }

    #[test]
    fn contents_as_indirect_array_is_walked_with_text_state_carried_across_streams() {
        // `uscis-i9.pdf`: `/Contents 1636 0 R` is an array of eight streams, and `Tf` lives in an
        // earlier stream than the string it selects.
        let doc = custom_page_pdf(
            |d| vec![("F1", helvetica_font(d))],
            &[b"BT /F1 12 Tf 72 700 Td ", b"(Hello) Tj ET"],
            true,
        );
        let page_dict = doc
            .get_object(*doc.get_pages().get(&1).expect("page"))
            .expect("page object")
            .as_dict()
            .expect("page dict")
            .clone();
        assert!(matches!(
            page_dict.get(b"Contents"),
            Ok(Object::Reference(_))
        ));

        let graph = page_graph_from_bytes(&saved(&doc), 1).expect("indirect /Contents array");
        let island = &graph.islands[0];
        assert_eq!(island.text, "Hello");
        assert_eq!(island.locator.spans[0].contents_index, 1);
        assert!(island.editable, "{:?}", island.edit_blocker);
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

    // -------------------------------------------------------------------------------------
    // Astra read of 0.2.1, 2026-10-11: findings 2, 3, 10, 11, 12 (inspection side)
    // -------------------------------------------------------------------------------------

    /// One Identity-H page: `/W` entries `(cid, width)` and the given CMap body, showing `hex`.
    fn cid_page(cmap_body: &str, cids: &[u32], hex: &str) -> Document {
        let widths: Vec<(u32, i64)> = cids.iter().map(|cid| (*cid, 500)).collect();
        let body = cmap_body.to_string();
        let content = format!("BT /F1 12 Tf 72 700 Td <{hex}> Tj ET");
        crate::test_fixtures::custom_page_pdf(
            |d| {
                vec![(
                    "F1",
                    crate::test_fixtures::identity_h_font_with_cmap(
                        d,
                        &widths,
                        crate::test_fixtures::raw_to_unicode_cmap(&body),
                    ),
                )]
            },
            &[content.as_bytes()],
            false,
        )
    }

    #[test]
    fn finding2_array_form_bfrange_maps_each_code_to_its_own_destination() {
        // `<0001> <0003> [<0041> <005A> <0042>]` is A, Z, B. The old reader took the first
        // destination as a scalar start and invented A, B, C.
        let doc = cid_page(
            "1 beginbfrange\n<0001> <0003> [<0041> <005A> <0042>]\nendbfrange",
            &[1, 2, 3],
            "000100020003",
        );
        let island = &inspect(&doc).islands[0];
        assert_eq!(island.text, "AZB");
        assert!(island.editable, "{:?}", island.edit_blocker);
    }

    #[test]
    fn finding2_array_shorter_than_its_range_maps_only_what_is_listed() {
        let doc = cid_page(
            "1 beginbfrange\n<0001> <0003> [<0041>]\nendbfrange",
            &[1, 2, 3],
            "000100020003",
        );
        let island = &inspect(&doc).islands[0];
        // Codes 2 and 3 are unmapped (never B and C), so the island cannot be edited at all.
        assert_eq!(island.text, "A");
        assert!(!island.editable);
        assert_eq!(island.edit_blocker, Some(PdfEditBlocker::UnmappedGlyphs));
    }

    #[test]
    fn finding2_scalar_bfrange_still_increments_and_a_bad_form_makes_the_font_non_editable() {
        let doc = cid_page(
            "1 beginbfrange\n<0001> <0003> <0041>\nendbfrange",
            &[1, 2, 3],
            "000100020003",
        );
        let island = &inspect(&doc).islands[0];
        assert_eq!(island.text, "ABC");
        assert!(island.editable, "{:?}", island.edit_blocker);

        // Unbalanced array: the table is not exact, so the font is not reversibly encoded even
        // though a bfchar before it maps every code on the page.
        let doc = cid_page(
            "3 beginbfchar\n<0001> <0041>\n<0002> <0042>\n<0003> <0043>\nendbfchar\n\
             1 beginbfrange\n<0004> <0005> [<0044>\nendbfrange",
            &[1, 2, 3],
            "000100020003",
        );
        let island = &inspect(&doc).islands[0];
        assert_eq!(island.text, "ABC");
        assert!(!island.editable);
        assert_eq!(
            island.edit_blocker,
            Some(PdfEditBlocker::NoReversibleEncoding)
        );
    }

    /// Two simple fonts that agree on `A`/`B` but not on code 67 (`F1`: C, `F2`: Z), an
    /// ExtGState `/G` that switches the font to `F2`, and a plain `/H` that only sets alpha.
    fn gs_font_doc(content: &[u8]) -> Document {
        use crate::test_fixtures::{custom_page_pdf, differences_encoding, subset_type1_font};
        let mut f2_id = None;
        let mut doc = custom_page_pdf(
            |d| {
                let diff = |third: &str| {
                    differences_encoding(
                        None,
                        true,
                        vec![
                            Object::Integer(65),
                            Object::Name(b"A".to_vec()),
                            Object::Name(b"B".to_vec()),
                            Object::Name(third.as_bytes().to_vec()),
                        ],
                    )
                };
                let f1 = subset_type1_font(d, 65, &[500, 500, 500], Some(diff("C")), None);
                let f2 = subset_type1_font(d, 65, &[500, 500, 500], Some(diff("Z")), None);
                f2_id = Some(f2);
                vec![("F1", f1), ("F2", f2)]
            },
            &[content],
            false,
        );
        let f2 = f2_id.expect("F2 built");
        let page_id = *doc.get_pages().get(&1).expect("page");
        let resources_id = doc
            .get_object(page_id)
            .expect("page")
            .as_dict()
            .expect("dict")
            .get(b"Resources")
            .expect("resources")
            .as_reference()
            .expect("indirect resources");
        let g = doc.add_object(dictionary! {
            "Type" => "ExtGState",
            "Font" => vec![f2.into(), 10.into()],
        });
        let h = doc.add_object(dictionary! { "Type" => "ExtGState", "CA" => 0.5 });
        if let Ok(Object::Dictionary(resources)) = doc.get_object_mut(resources_id) {
            resources.set("ExtGState", dictionary! { "G" => g, "H" => h });
        }
        doc
    }

    #[test]
    fn finding3_gs_that_sets_a_font_is_not_decoded_through_the_previous_font() {
        let doc = gs_font_doc(b"BT /F1 10 Tf 72 700 Td (A) Tj /G gs (B) Tj ET");
        let graph = inspect(&doc);
        // Never one island "AB" read through F1: the B after the gs is drawn in F2.
        assert!(
            graph
                .islands
                .iter()
                .all(|island| !(island.editable && island.text.contains('B'))),
            "{:?}",
            graph
                .islands
                .iter()
                .map(|i| (&i.text, i.editable))
                .collect::<Vec<_>>()
        );
        assert!(graph.islands.iter().any(|i| i.text == "A" && i.editable));
        let unresolved = graph
            .islands
            .iter()
            .find(|island| island.text.is_empty())
            .expect("the text after the gs is an unresolved-font island");
        assert_eq!(
            unresolved.edit_blocker,
            Some(PdfEditBlocker::UnsupportedFontType)
        );

        // A later Tf selects a font again and text decodes again - through F2, which differs
        // from F1, so the joined island is a mixed-state (non-editable) "AB".
        let doc = gs_font_doc(b"BT /F1 10 Tf 72 700 Td (A) Tj /G gs /F2 10 Tf (B) Tj ET");
        let graph = inspect(&doc);
        let island = graph
            .islands
            .iter()
            .find(|i| i.text == "AB")
            .expect("decoding resumes after the Tf");
        assert!(!island.editable);
        assert_eq!(island.edit_blocker, Some(PdfEditBlocker::MixedTextState));
    }

    #[test]
    fn finding3_gs_without_a_font_entry_does_not_split_the_run() {
        let doc = gs_font_doc(b"BT /F1 10 Tf 72 700 Td (A) Tj /H gs (B) Tj ET");
        let island = &inspect(&doc).islands[0];
        assert_eq!(island.text, "AB");
        assert!(island.editable, "{:?}", island.edit_blocker);
    }

    #[test]
    fn finding10_tj_gap_survives_an_empty_string_and_a_show_operator_boundary() {
        for (label, content) in [
            (
                "empty string",
                &b"BT /F1 10 Tf 72 700 Td [(hello) -333 () (big)] TJ ET"[..],
            ),
            (
                "across Tj",
                &b"BT /F1 10 Tf 72 700 Td [(hello) -333] TJ (big) Tj ET"[..],
            ),
            (
                "across two empty strings",
                &b"BT /F1 10 Tf 72 700 Td [(hello) -200 () -133 () (big)] TJ ET"[..],
            ),
        ] {
            let doc = crate::test_fixtures::tex_style_pdf(false, content);
            let island = &inspect(&doc).islands[0];
            assert_eq!(island.text, "hello big", "{label}");
            assert_eq!(island.edit_blocker, None, "{label}: no positioning seam");
            assert!(island.editable, "{label}");
            assert_eq!(island.gaps.len(), 1, "{label}");
        }
    }

    #[test]
    fn finding10_a_real_positioning_move_still_makes_the_gap_a_positioned_run() {
        // `Td` is relative to the start of the *line*, not to the pen: the move lands "big" where
        // the TJ adjustment would have (25 pt of "hello" + 3.33 pt = 28.33 pt from the line
        // start), so the 3.33 pt gap exists - but a positioning operator ran between the show
        // operators, so the -333 is no longer its provenance.
        let doc = crate::test_fixtures::tex_style_pdf(
            false,
            b"BT /F1 10 Tf 72 700 Td [(hello) -333] TJ 28.33 0 Td (big) Tj ET",
        );
        let island = &inspect(&doc).islands[0];
        assert_eq!(island.text, "hello big");
        assert_eq!(island.gaps.len(), 1);
        assert!(!island.editable);
        assert_eq!(island.edit_blocker, Some(PdfEditBlocker::PositionedTextRun));
        assert!(
            island.run.iter().all(|item| item.gap_numbers.is_none()),
            "the gap came from a move, not from the TJ number before it"
        );
    }

    #[test]
    fn td_moves_from_the_line_start_not_from_the_pen() {
        // The old form of the test above, `0 0 Td`, returns to the line start: "big" is drawn on
        // top of "hello", which is two islands, not one run with a gap.
        let doc = crate::test_fixtures::tex_style_pdf(
            false,
            b"BT /F1 10 Tf 72 700 Td [(hello) -333] TJ 0 0 Td (big) Tj ET",
        );
        let graph = inspect(&doc);
        let texts: Vec<&str> = graph.islands.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, vec!["hello", "big"]);
    }

    #[test]
    fn finding11_substitution_is_not_promised_across_streams() {
        let doc = custom_page_pdf(
            |d| vec![("F1", helvetica_font(d))],
            &[b"BT /F1 12 Tf 72 700 Td (Hel) Tj", b"(lo) Tj ET"],
            false,
        );
        let island = &inspect(&doc).islands[0];
        assert_eq!(island.text, "Hello");
        assert_eq!(
            island.edit_blocker,
            Some(PdfEditBlocker::SpansAcrossStreams)
        );
        assert!(!island.substitution_editable);

        // One stream: promised (a Td-positioned run is not editable in place, but it can be
        // redrawn in a catalog font).
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Hel) Tj 18 0 Td (lo) Tj ET");
        let island = &inspect(&doc).islands[0];
        assert!(!island.editable);
        assert!(island.substitution_editable);
    }

    #[test]
    fn finding12_gap_width_is_the_distance_beyond_tc() {
        // 10 pt, Tc 3: "hello" glyphs are 5 pt wide + 3 Tc each; the -333 adjustment adds 3.33 pt
        // beyond the 3 pt of Tc every glyph already advances.
        let doc = crate::test_fixtures::tex_style_pdf(
            false,
            b"BT /F1 10 Tf 3 Tc 72 700 Td [(hello) -333 (big)] TJ ET",
        );
        let island = &inspect(&doc).islands[0];
        assert_eq!(island.text, "hello big");
        assert_eq!(island.gaps.len(), 1);
        assert!(
            (island.gaps[0].width_pt - 3.33).abs() < 0.01,
            "{}",
            island.gaps[0].width_pt
        );
        // No Tc: unchanged.
        let doc = crate::test_fixtures::tex_style_pdf(
            false,
            b"BT /F1 10 Tf 72 700 Td [(hello) -333 (big)] TJ ET",
        );
        let island = &inspect(&doc).islands[0];
        assert!((island.gaps[0].width_pt - 3.33).abs() < 0.01);
    }

    #[test]
    fn hostile_cid_widths_and_cmap_ranges_are_bounded() {
        // Unit-level companion of tests/hostile_input.rs: the parsers themselves.
        let doc = Document::with_version("1.7");
        let mut font = Dictionary::new();
        let descendant = dictionary! {
            "W" => vec![
                Object::Integer(4294967295), Object::Integer(4294967295), Object::Integer(500),
                Object::Integer(0), Object::Integer(4294967294), Object::Integer(400),
            ],
        };
        font.set("DescendantFonts", vec![Object::Dictionary(descendant)]);
        let (_, widths, complete) = parse_cid_widths(&doc, &font);
        assert!(widths.len() <= 65_536);
        assert_eq!(widths.get(&1), Some(&400.0));
        assert_eq!(widths.get(&0xFFFF), Some(&400.0));
        assert!(complete, "two ranges are far inside the work budget");
    }

    /// A font dictionary whose descendant `/W` is `entries` (flattened operands).
    fn cid_font_with_w(entries: Vec<Object>) -> Dictionary {
        let mut font = Dictionary::new();
        font.set(
            "DescendantFonts",
            vec![Object::Dictionary(dictionary! { "W" => entries })],
        );
        font
    }

    #[test]
    fn finding8_repeated_full_width_w_ranges_hit_the_work_budget_not_billions_of_insertions() {
        // 40 000 x `0 65535 500` is 2.6 billion insertions if each range is only clamped.
        let doc = Document::with_version("1.7");
        let mut entries = Vec::with_capacity(120_000);
        for _ in 0..40_000 {
            entries.push(Object::Integer(0));
            entries.push(Object::Integer(65535));
            entries.push(Object::Integer(500));
        }
        let (dw, widths, complete) = parse_cid_widths(&doc, &cid_font_with_w(entries));
        assert_eq!(dw, 1000.0);
        assert!(!complete, "the budget ran out, so the table is not exact");
        assert_eq!(widths.len(), 65_536, "what was read is still a valid table");
        assert!(widths.values().all(|w| *w == 500.0));
    }

    #[test]
    fn finding8_array_form_w_repetitions_are_charged_too() {
        // `0 [500 x 60000]` repeated: each entry is under the clamp, the total is not.
        let doc = Document::with_version("1.7");
        let mut entries = Vec::new();
        for _ in 0..40 {
            entries.push(Object::Integer(0));
            entries.push(Object::Array(vec![Object::Integer(500); 60_000]));
        }
        let (_, widths, complete) = parse_cid_widths(&doc, &cid_font_with_w(entries));
        assert!(!complete);
        assert_eq!(widths.len(), 60_000);
    }

    #[test]
    fn finding8_over_budget_w_array_form_stops_at_the_budget_by_construction() {
        // 40 x `0 [500 x 60000]` is 2.4 million assignments. Every assignment is charged before
        // it is made, so the parser performs exactly the budget and stops at the first entry
        // beyond it, without walking the rest of the array.
        let doc = Document::with_version("1.7");
        let mut entries = Vec::new();
        for _ in 0..40 {
            entries.push(Object::Integer(0));
            entries.push(Object::Array(vec![Object::Integer(500); 60_000]));
        }
        let font = cid_font_with_w(entries);
        let (dw, widths, complete, performed) =
            parse_cid_widths_within(&doc, &font, MAX_CID_WIDTH_ASSIGNMENTS);
        assert_eq!(dw, 1000.0);
        assert!(!complete, "the budget ran out, so the table is not exact");
        assert!(performed <= MAX_CID_WIDTH_ASSIGNMENTS);
        assert_eq!(performed, MAX_CID_WIDTH_ASSIGNMENTS);
        assert_eq!(
            widths.len(),
            60_000,
            "the CIDs 0..60000 are the only ones in the array"
        );
    }

    #[test]
    fn finding8_a_font_whose_w_array_ran_out_of_budget_is_not_editable() {
        let body: Vec<(u32, i64)> = vec![(1, 600)];
        let mut w: Vec<Object> = Vec::with_capacity(120_000);
        for _ in 0..40_000 {
            w.push(Object::Integer(0));
            w.push(Object::Integer(65535));
            w.push(Object::Integer(500));
        }
        let doc = custom_page_pdf(
            |d| {
                let font = identity_h_font(d, &body, &[(1, 'H')]);
                // Replace the descendant's `/W` with the hostile one.
                let descendant = d
                    .get_object(font)
                    .expect("type0")
                    .as_dict()
                    .expect("dict")
                    .get(b"DescendantFonts")
                    .expect("descendants")
                    .as_array()
                    .expect("array")[0]
                    .as_reference()
                    .expect("reference");
                if let Ok(Object::Dictionary(descendant)) = d.get_object_mut(descendant) {
                    descendant.set("W", Object::Array(w));
                }
                vec![("F1", font)]
            },
            &[b"BT /F1 12 Tf 72 700 Td <0001> Tj ET"],
            false,
        );
        let island = &inspect(&doc).islands[0];
        assert_eq!(island.text, "H");
        assert!(!island.editable);
        assert_eq!(island.edit_blocker, Some(PdfEditBlocker::MissingMetrics));
    }
    // -------------------------------------------------------------------------------------
    // Second Astra read, 2026-10-11: findings 1, 3, 4 and 2 (inspection side)
    // -------------------------------------------------------------------------------------

    /// Left-to-right extent of an island's glyph quads in device points.
    fn extent(island: &PdfTextIsland) -> f64 {
        let xs = island
            .quads
            .iter()
            .flat_map(|quad| [quad[0], quad[2], quad[4], quad[6]]);
        let (lo, hi) = xs.fold((f64::MAX, f64::MIN), |(lo, hi), x| (lo.min(x), hi.max(x)));
        hi - lo
    }

    #[test]
    fn finding1_trailing_gaps_move_numbers_out_once_and_respect_the_page_budget() {
        let number = |item: u32| GapNumber {
            operator_index: 0,
            item_index: item,
            value: -100.0,
        };
        let mut gaps = TrailingGaps::with_budget(3);
        gaps.push(number(1));
        gaps.push(number(3));
        assert_eq!(gaps.take().len(), 2);
        assert!(gaps.take().is_empty(), "taking moves the numbers out");

        // The budget is per page, not per gap: one slot was left, the next push is dropped.
        gaps.push(number(5));
        gaps.push(number(7));
        assert!(
            gaps.take().is_empty(),
            "a gap that lost items is reported empty, never partial"
        );
        gaps.push(number(9));
        assert!(gaps.take().is_empty(), "the page budget is spent");

        // A positioning operator clears a pending truncation as well.
        let mut gaps = TrailingGaps::with_budget(1);
        gaps.push(number(1));
        gaps.push(number(2));
        gaps.clear();
        assert!(gaps.numbers.is_empty() && !gaps.truncated);
    }

    #[test]
    fn finding1_empty_strings_leave_every_gap_number_for_the_next_real_string() {
        let doc = crate::test_fixtures::tex_style_pdf(
            false,
            b"BT /F1 10 Tf 72 700 Td [(hello) -100 () -100 () -133 () (big)] TJ ET",
        );
        let island = &inspect(&doc).islands[0];
        assert_eq!(island.text, "hello big");
        assert!(island.editable, "{:?}", island.edit_blocker);
        let numbers = island
            .run
            .iter()
            .find_map(|item| item.gap_numbers.as_ref())
            .expect("the gap keeps its provenance");
        let values: Vec<f64> = numbers.iter().map(|n| n.value).collect();
        assert_eq!(values, vec![-100.0, -100.0, -133.0]);
    }

    #[test]
    fn finding3_q_restores_the_text_state_parameters_but_not_the_text_matrix() {
        // Before the q: F1 at 10 pt, Tc 2, Tw 3, Tz 50, TL 14, Ts 5. Inside it F2 at 20 pt and
        // other values. The reviewer's case: F1 maps 67 -> C, F2 maps 67 -> Z.
        let doc = gs_font_doc(
            b"BT /F1 10 Tf 2 Tc 3 Tw 50 Tz 14 TL 5 Ts 1 Tr ET \
              q BT /F2 20 Tf 9 Tc 8 Tw 100 Tz 7 TL 1 Ts 0 Tr ET Q \
              BT 72 700 Td T* (C) Tj ET",
        );
        let graph = inspect(&doc);
        let island = graph
            .islands
            .iter()
            .find(|island| !island.text.is_empty())
            .expect("a decoded island");
        assert_eq!(island.appearance.font_id, "F1", "Q restores Tf's font");
        assert_eq!(island.text, "C", "decoded through F1, not F2 (67 -> Z)");
        let layout = island.layout.expect("layout");
        assert_eq!(layout.font_size, 10.0);
        assert_eq!(layout.char_spacing, 2.0);
        assert_eq!(layout.word_spacing, 3.0);
        assert_eq!(layout.horizontal_scale, 0.5);
        // T* moves down by the restored TL (14), not the inner one (7).
        assert_eq!(island.baseline[1], 686.0);
        // Ts 5 lifts the glyph box: bottom edge = 686 - 0.25 * 10 + 5.
        assert!((island.quads[0][1] - (686.0 - 2.5 + 5.0)).abs() < 1e-9);

        // The text matrix is not part of the graphics state: the pen carries on past the Q.
        let doc = gs_font_doc(b"BT /F1 10 Tf 72 700 Td (A) Tj q (B) Tj Q (C) Tj ET");
        let island = &inspect(&doc).islands[0];
        assert_eq!(island.text, "ABC");
        let lefts: Vec<f64> = island.quads.iter().map(|quad| quad[0]).collect();
        assert_eq!(lefts, vec![72.0, 77.0, 82.0]);
    }

    #[test]
    fn finding3_an_unmatched_q_is_ignored_and_a_font_set_inside_q_does_not_leak() {
        // Code 67 is C in F1 and Z in F2. The Q at the very start has nothing to restore.
        let doc = gs_font_doc(
            b"Q BT /F1 10 Tf 72 700 Td (A) Tj ET q BT /F2 10 Tf ET Q BT 82 700 Td (C) Tj ET",
        );
        let graph = inspect(&doc);
        assert!(graph.islands.iter().any(|island| island.text.contains('C')));
        assert!(graph
            .islands
            .iter()
            .all(|island| !island.text.contains('Z')));
    }

    #[test]
    fn graphics_state_nesting_beyond_the_cap_is_malformed_content_not_an_unbounded_stack() {
        let deep = "q ".repeat(MAX_GRAPHICS_STATE_DEPTH + 1);
        let doc = build_pdf(deep.as_bytes());
        assert!(matches!(
            walk_page_graph(&doc, "test-sha", 1),
            Err(PdfEditError::MalformedContent)
        ));

        // Balanced, and well past anything a producer writes, still walks.
        let mut content = "q ".repeat(1_000).into_bytes();
        content.extend_from_slice(b"BT /F1 12 Tf 72 700 Td (Hi) Tj ET ");
        content.extend_from_slice("Q ".repeat(1_000).as_bytes());
        let graph = inspect(&build_pdf(&content));
        assert_eq!(graph.islands[0].text, "Hi");
    }

    #[test]
    fn finding4_word_spacing_applies_to_code_32_whatever_it_decodes_to() {
        // Code 32 is the letter x here. Tw 10 moves the pen 10 pt past it, so the island is
        // 5 + (5 + 10) + 5 = 25 pt wide (the old reader said 15), and the jump is not a gap.
        let doc = custom_page_pdf(
            |d| vec![("F1", crate::test_fixtures::code_32_letter_font(d))],
            &[b"BT /F1 10 Tf 10 Tw 72 700 Td (a b) Tj ET"],
            false,
        );
        let island = &inspect(&doc).islands[0];
        assert_eq!(island.text, "axb");
        assert!((extent(island) - 25.0).abs() < 1e-9, "{}", extent(island));
        assert!(island.gaps.is_empty(), "the Tw jump is not a word gap");
        assert!(island.editable, "{:?}", island.edit_blocker);
        let flags: Vec<bool> = island.run.iter().map(|item| item.word_space).collect();
        assert_eq!(flags, vec![false, true, false]);
    }

    #[test]
    fn finding4_a_space_reached_through_another_code_takes_no_word_spacing() {
        // The space is code 5. Tw applies to code 32 only, so the island is 5 + 2.5 + 5 = 12.5 pt
        // (the old reader added Tw because the character was a space).
        let doc = custom_page_pdf(
            |d| vec![("F1", crate::test_fixtures::code_5_space_font(d))],
            &[b"BT /F1 10 Tf 10 Tw 72 700 Td <610562> Tj ET"],
            false,
        );
        let island = &inspect(&doc).islands[0];
        assert_eq!(island.text, "a b");
        assert!((extent(island) - 12.5).abs() < 1e-9, "{}", extent(island));
        assert!(island.run.iter().all(|item| !item.word_space));
    }

    #[test]
    fn finding4_multi_byte_codes_never_take_word_spacing() {
        // CID 32 maps to a space in an Identity-H font; Tw is for single-byte code 32 only.
        let doc = custom_page_pdf(
            |d| {
                vec![(
                    "F1",
                    identity_h_font(
                        d,
                        &[(1, 500), (32, 250), (2, 500)],
                        &[(1, 'A'), (32, ' '), (2, 'B')],
                    ),
                )]
            },
            &[b"BT /F1 10 Tf 10 Tw 72 700 Td <000100200002> Tj ET"],
            false,
        );
        let island = &inspect(&doc).islands[0];
        assert_eq!(island.text, "A B");
        assert!((extent(island) - 12.5).abs() < 1e-9, "{}", extent(island));
    }

    #[test]
    fn finding4_single_byte_code_32_still_takes_word_spacing_when_it_is_a_space() {
        let graph = inspect(&build_pdf(b"BT /F1 10 Tf 10 Tw 72 700 Td (a b) Tj ET"));
        let island = &graph.islands[0];
        assert_eq!(island.text, "a b");
        // a 5.56 + space 2.78 + Tw 10 + b 5.56 (Helvetica: 556, 278, 556).
        assert!((extent(island) - (5.56 + 2.78 + 10.0 + 5.56)).abs() < 1e-9);
        assert!(island.run[1].word_space);
    }

    #[cfg(feature = "edit")]
    #[test]
    fn finding4_encoded_runs_charge_word_spacing_by_code() {
        let doc = custom_page_pdf(
            |d| vec![("F1", crate::test_fixtures::code_32_letter_font(d))],
            &[b"BT /F1 10 Tf 72 700 Td (aab) Tj ET"],
            false,
        );
        let page_id = *doc.get_pages().get(&1).expect("page");
        let run = encode_for_font(&doc, page_id, "F1", "axb")
            .ok()
            .expect("encodes");
        assert_eq!(run.bytes, vec![97, 32, 98]);
        assert!(!run.word_space_at(0) && run.word_space_at(1) && !run.word_space_at(2));
        assert!(!run.ends_with_word_space());
        // widths 15 + Tc 0 + Tw 10 after the middle code; nothing after the last glyph.
        assert!((run.advance(10.0, 0.0, 10.0, 1.0) - 25.0).abs() < 1e-9);
        let lone = encode_for_font(&doc, page_id, "F1", "x")
            .ok()
            .expect("encodes");
        assert!(lone.ends_with_word_space());
    }

    #[test]
    fn finding2_the_resource_fingerprint_reads_inherited_resources_and_notices_a_hidden_font() {
        let doc = crate::test_fixtures::inherited_resources_pdf(
            b"BT /F1 12 Tf 72 700 Td (Existing) Tj ET",
        );
        let before = resource_fingerprint(&doc);
        let names: Vec<(&[u8], &[u8])> = before[&1]
            .keys()
            .map(|(category, name)| (category.as_slice(), name.as_slice()))
            .collect();
        assert_eq!(
            names,
            vec![(&b"Font"[..], &b"F1"[..]), (&b"XObject"[..], &b"Im1"[..])]
        );
        assert!(resources_preserved(&before, &before));

        // What the old composer wrote: page-local resources holding only the new font.
        let mut hidden = doc.clone();
        let page_id = *hidden.get_pages().get(&1).expect("page");
        let font = *hidden
            .objects
            .keys()
            .next()
            .expect("an object to point the new font at");
        if let Ok(Object::Dictionary(page)) = hidden.get_object_mut(page_id) {
            page.set(
                "Resources",
                dictionary! { "Font" => dictionary! { "VREdit1" => font } },
            );
        }
        assert!(!resources_preserved(
            &before,
            &resource_fingerprint(&hidden)
        ));

        // Adding a font next to the inherited ones is fine.
        let mut added = doc.clone();
        let page_id = *added.get_pages().get(&1).expect("page");
        let pages_resources = {
            let parent = added
                .get_object(page_id)
                .expect("page")
                .as_dict()
                .expect("dict")
                .get(b"Parent")
                .expect("parent")
                .as_reference()
                .expect("reference");
            added
                .get_object(parent)
                .expect("pages")
                .as_dict()
                .expect("dict")
                .get(b"Resources")
                .expect("resources")
                .as_dict()
                .expect("dict")
                .clone()
        };
        let mut resources = pages_resources;
        let mut fonts = resources
            .get(b"Font")
            .expect("fonts")
            .as_dict()
            .expect("dict")
            .clone();
        fonts.set("VREdit1", font);
        resources.set("Font", Object::Dictionary(fonts));
        if let Ok(Object::Dictionary(page)) = added.get_object_mut(page_id) {
            page.set("Resources", Object::Dictionary(resources));
        }
        assert!(resources_preserved(&before, &resource_fingerprint(&added)));
    }
}
