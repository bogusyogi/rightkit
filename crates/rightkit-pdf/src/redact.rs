//! Redaction: remove content under marked areas, fail closed (feature `redact`).
//!
//! A mark is a page and a rectangle in PDF user space (the space islands and quads report, before
//! `/Rotate`). [`apply_marks`] takes the source bytes and returns new bytes in which the marked
//! content no longer exists in the file. It does not draw a box over it: the box is painted last,
//! after the content has been removed and checked, and is only a visual.
//!
//! Design reference: pdfcraft `crates/redact` (MIT OR Apache-2.0; no code is copied): glyphs are
//! cut from `Tj`/`TJ` and replaced by a same-advance `TJ` number, so the text after them does not
//! move. This is a new implementation on this crate's page walker, not a port of its code.
//!
//! # What is guaranteed
//!
//! When [`apply_marks`] returns `Ok`, for every marked page, in the returned bytes:
//!
//! 1. **Text.** No glyph whose bounding quad overlaps a mark (any overlap of non-zero area) is
//!    shown by any `Tj`, `TJ`, `'` or `"` operator of the page's content streams. The bytes of those
//!    glyphs are gone from the stream, so text extraction (`extract`, the island walker, any
//!    other reader of the content stream) cannot return them. The check is a second walk of the
//!    written document, with the marks shrunk by 0.05 pt so rounding of the replacement numbers
//!    cannot fail it.
//! 2. **Images.** No image XObject whose placed unit square overlaps a mark is painted: the `Do`
//!    operator is removed. A partly covered image is removed **whole** (the reference design clears
//!    pixels; this crate does not re-encode images, so it removes more, never less). The image
//!    object itself is dropped from the file by pruning unreferenced objects.
//! 3. **Vector paths.** A path-painting operator (`S s f F f* B B* b b*`) whose path bounds
//!    (widened by half the line width for strokes) overlap a mark is removed together with its
//!    path construction; a path that also sets a clip (`W`, `W*`) is kept as a clip and painted by
//!    nothing (`n`). Bounds are of the control points, so they are never smaller than the path.
//! 4. **Annotations** (links, notes, widgets, stamps) whose `/Rect` overlaps a mark are removed
//!    from the page.
//! 5. **Hidden text on the page.** `/ActualText`, `/Alt` and `/E` entries are removed from inline
//!    marked-content property lists (`BDC`) of every marked page, because extractors read them in
//!    place of the glyphs.
//! 6. **Full rewrite.** The result is written whole from the loaded objects. Earlier revisions of
//!    an incrementally saved file are not carried, replaced content streams are not kept, and
//!    unreferenced objects (removed images, old page content) are pruned.
//! 7. With `sanitize` (default on): the document `/Info`, catalog and page `/Metadata`, `/PieceInfo`,
//!    page `/Thumb` (a thumbnail shows the redacted pixels), the structure tree and `/AF`
//!    (tagged text carries `/ActualText`), and the embedded-file and JavaScript name trees are
//!    removed.
//!
//! # What is refused (fail closed)
//!
//! Anything the walker cannot place or cannot cut is an error and nothing is returned:
//! text it cannot position (`UnlocatableText`: no resolvable font, singular matrix, a code without
//! a width), a glyph whose advance cannot be written as a `TJ` number, a content stream that does
//! not parse strictly, a content stream shared by two pages, a form XObject or shading that
//! overlaps a mark (unless [`RedactOptions::remove_unsupported`], which removes the `Do` or `sh`
//! operator whole), an inline image that overlaps a mark, an encrypted document, and a result whose
//! second walk still finds content under a mark.
//!
//! # What is not covered
//!
//! - Text, images or paths drawn **inside a form XObject** are not read: a form that overlaps a mark
//!   is refused or removed whole, never edited. A form that does not overlap is left alone.
//! - Content the page reaches through `/Properties` resource names in `BDC`, optional-content
//!   hidden layers, and text in other annotation appearance streams that do not overlap a mark.
//! - Pixel content of an image that does not overlap a mark is untouched, as is a page's `/Rotate`.
//! - Sub-glyph precision: a glyph is removed when its bounding quad overlaps, not when its ink does.
//!   Marks are not placed for the user: the caller picks the rectangle (for example from island
//!   quads).
//!
//! The next save must be this output: callers stage the returned bytes through the ordinary save
//! path (`save::stage_prepared_save` with no expected-text checks).

use std::collections::{BTreeMap, HashMap, HashSet};

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat};
use serde::{Deserialize, Serialize};

use crate::inspect::{
    deref_obj, load_pdf, object_number, page_content_stream_ids, resolve_inherited,
    walk_page_graph, ContentBudget,
};
use crate::model::{Matrix, PdfEditError, PdfRect};

/// Errors of redaction. Every one means nothing was produced.
#[derive(Debug, thiserror::Error)]
pub enum RedactError {
    #[error("no marks to apply")]
    NoMarks,
    #[error("invalid mark: {0}")]
    InvalidMark(String),
    #[error("PDF could not be parsed: {0}")]
    Parse(String),
    #[error("encrypted PDF cannot be redacted")]
    Encrypted,
    #[error("page {0} does not exist")]
    PageOutOfRange(u32),
    #[error("page {page}: content is malformed or could not be read strictly")]
    MalformedContent { page: u32 },
    #[error("page {page}: a content stream is shared with another page")]
    SharedContentStream { page: u32 },
    #[error("page {page}: text that cannot be positioned (font or matrix unresolved)")]
    UnlocatableText { page: u32 },
    #[error("page {page}: a glyph under a mark cannot be removed without moving the text after it")]
    CannotKeepPosition { page: u32 },
    #[error("page {page}: {kind} overlaps a mark and cannot be cut")]
    UnsupportedContent { page: u32, kind: &'static str },
    #[error("page {page}: content under a mark remains after redaction ({what})")]
    VerificationFailed { page: u32, what: &'static str },
    #[error("serialize failed: {0}")]
    Write(String),
}

impl RedactError {
    /// Stable snake_case code.
    pub fn code(&self) -> &'static str {
        match self {
            RedactError::NoMarks => "no_marks",
            RedactError::InvalidMark(_) => "invalid_mark",
            RedactError::Parse(_) => "parse",
            RedactError::Encrypted => "encrypted",
            RedactError::PageOutOfRange(_) => "page_out_of_range",
            RedactError::MalformedContent { .. } => "malformed_content",
            RedactError::SharedContentStream { .. } => "shared_content_stream",
            RedactError::UnlocatableText { .. } => "unlocatable_text",
            RedactError::CannotKeepPosition { .. } => "cannot_keep_position",
            RedactError::UnsupportedContent { .. } => "unsupported_content",
            RedactError::VerificationFailed { .. } => "verification_failed",
            RedactError::Write(_) => "write",
        }
    }
}

/// One marked area: page (1-based) and rectangle in PDF user space.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RedactMark {
    pub id: u64,
    pub page: u32,
    pub rect: PdfRect,
}

fn validate_rect(rect: &PdfRect) -> Result<(), RedactError> {
    let finite = [rect.x, rect.y, rect.width, rect.height]
        .iter()
        .all(|v| v.is_finite() && v.abs() < 1e7);
    if !finite || rect.width <= 0.0 || rect.height <= 0.0 {
        return Err(RedactError::InvalidMark(
            "a mark needs a finite rectangle with positive width and height".to_string(),
        ));
    }
    Ok(())
}

/// The marks a host holds before applying them. Plain data: a host may keep its own list and pass
/// [`RedactMark`] values straight to [`apply_marks`].
#[derive(Clone, Debug, Default)]
pub struct RedactMarks {
    marks: Vec<RedactMark>,
    next_id: u64,
}

impl RedactMarks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a mark and returns its id.
    pub fn add(&mut self, page: u32, rect: PdfRect) -> Result<u64, RedactError> {
        if page == 0 {
            return Err(RedactError::InvalidMark("pages are 1-based".to_string()));
        }
        validate_rect(&rect)?;
        self.next_id += 1;
        self.marks.push(RedactMark {
            id: self.next_id,
            page,
            rect,
        });
        Ok(self.next_id)
    }

    /// Removes one mark; false when no mark has that id.
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.marks.len();
        self.marks.retain(|mark| mark.id != id);
        self.marks.len() != before
    }

    /// Removes every mark of `page`; returns how many.
    pub fn clear_page(&mut self, page: u32) -> usize {
        let before = self.marks.len();
        self.marks.retain(|mark| mark.page != page);
        before - self.marks.len()
    }

    /// Removes every mark.
    pub fn clear(&mut self) {
        self.marks.clear();
    }

    pub fn marks(&self) -> &[RedactMark] {
        &self.marks
    }

    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }
}

/// How [`apply_marks`] behaves.
#[derive(Clone, Debug)]
pub struct RedactOptions {
    /// Colour (RGB, 0..=1) of the box painted over each mark after the content is removed.
    /// `None` paints nothing; the area is then blank (and path checks include every path).
    pub fill: Option<[f64; 3]>,
    /// Remove a form XObject `Do` or a shading `sh` that overlaps a mark whole instead of refusing.
    /// Off by default: it can remove content far outside the mark.
    pub remove_unsupported: bool,
    /// Remove hidden-information entries (see the module documentation). On by default.
    pub sanitize: bool,
}

impl Default for RedactOptions {
    fn default() -> Self {
        RedactOptions {
            fill: Some([0.0, 0.0, 0.0]),
            remove_unsupported: false,
            sanitize: true,
        }
    }
}

/// What [`apply_marks`] did.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RedactReport {
    pub pages: Vec<u32>,
    pub glyphs_removed: usize,
    pub images_removed: usize,
    pub paths_removed: usize,
    pub unsupported_removed: usize,
    pub annotations_removed: usize,
    pub hidden_text_entries_removed: usize,
    /// Names of the hidden-information entries removed by `sanitize`.
    pub sanitized: Vec<String>,
}

#[derive(Debug)]
pub struct RedactOutcome {
    pub bytes: Vec<u8>,
    pub report: RedactReport,
}

// ---------------------------------------------------------------------------------------------
// Planning: one read-only walk of a page that decides what must change.
// ---------------------------------------------------------------------------------------------

/// Deepest `q` nesting followed.
const MAX_DEPTH: usize = 1 << 16;
/// Marks are shrunk by this much when the written document is checked, so the rounding of the
/// replacement `TJ` numbers cannot make a neighbour glyph that only touched a mark overlap it.
const VERIFY_SHRINK: f64 = 0.05;

enum Action {
    Delete,
    Replace(Vec<Operation>),
}

struct StreamPlan {
    id: ObjectId,
    ops: Vec<Operation>,
    actions: BTreeMap<usize, Action>,
}

#[derive(Default)]
struct PagePlan {
    streams: Vec<StreamPlan>,
    annots: Vec<usize>,
    glyphs: usize,
    images: usize,
    paths: usize,
    unsupported: usize,
    hidden_text: usize,
    end_depth: usize,
}

impl PagePlan {
    /// True when nothing under a mark remains to cut (hidden-text entries are idempotent and do
    /// not count).
    fn is_clean(&self) -> bool {
        self.glyphs == 0
            && self.images == 0
            && self.paths == 0
            && self.unsupported == 0
            && self.annots.is_empty()
    }

    fn what(&self) -> &'static str {
        if self.glyphs > 0 {
            "text"
        } else if self.images > 0 {
            "image"
        } else if self.paths > 0 {
            "vector path"
        } else if self.unsupported > 0 {
            "unsupported content"
        } else {
            "annotation"
        }
    }
}

enum XKind {
    Image,
    Form {
        bbox: Option<[f64; 4]>,
        matrix: Matrix,
    },
    Other,
}

fn numbers(operands: &[Object]) -> Vec<f64> {
    operands.iter().filter_map(object_number).collect()
}

fn matrix_of(numbers: &[f64]) -> Option<Matrix> {
    if numbers.len() != 6 || numbers.iter().any(|v| !v.is_finite()) {
        return None;
    }
    Some(Matrix {
        a: numbers[0],
        b: numbers[1],
        c: numbers[2],
        d: numbers[3],
        e: numbers[4],
        f: numbers[5],
    })
}

fn bounds_of(points: &[(f64, f64)]) -> Option<PdfRect> {
    let mut iter = points.iter().filter(|(x, y)| x.is_finite() && y.is_finite());
    let first = iter.next()?;
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (first.0, first.1, first.0, first.1);
    for (x, y) in iter {
        min_x = min_x.min(*x);
        min_y = min_y.min(*y);
        max_x = max_x.max(*x);
        max_y = max_y.max(*y);
    }
    Some(PdfRect {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    })
}

fn quad_bounds(quad: &[f64; 8]) -> Option<PdfRect> {
    let points: Vec<(f64, f64)> = quad.chunks(2).map(|p| (p[0], p[1])).collect();
    bounds_of(&points)
}

fn hits(rects: &[PdfRect], bounds: &PdfRect) -> bool {
    rects.iter().any(|rect| rect.intersects(*bounds))
}

/// Overlap test for something with unknown or zero-area bounds: a degenerate extent still counts
/// when it lies inside a mark, so a line or a point cannot hide under one.
fn hits_inclusive(rects: &[PdfRect], bounds: &PdfRect) -> bool {
    rects.iter().any(|rect| {
        rect.x <= bounds.x + bounds.width
            && bounds.x <= rect.x + rect.width
            && rect.y <= bounds.y + bounds.height
            && bounds.y <= rect.y + rect.height
    })
}

fn xobject_kinds(doc: &Document, page: &Dictionary) -> HashMap<Vec<u8>, XKind> {
    let mut out = HashMap::new();
    let xobjects = resolve_inherited(doc, page, b"Resources")
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_dict().ok())
        .and_then(|r| r.get(b"XObject").ok())
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_dict().ok());
    let Some(xobjects) = xobjects else {
        return out;
    };
    for (name, value) in xobjects.iter() {
        let Some(stream) = deref_obj(doc, value).and_then(|o| o.as_stream().ok()) else {
            continue;
        };
        let kind = match stream.dict.get(b"Subtype").ok().and_then(|o| o.as_name().ok()) {
            Some(b"Image") => XKind::Image,
            Some(b"Form") => {
                let bbox = stream
                    .dict
                    .get(b"BBox")
                    .ok()
                    .and_then(|o| deref_obj(doc, o))
                    .and_then(|o| o.as_array().ok())
                    .map(|a| numbers(a))
                    .filter(|n| n.len() == 4 && n.iter().all(|v| v.is_finite()))
                    .map(|n| [n[0], n[1], n[2], n[3]]);
                let matrix = stream
                    .dict
                    .get(b"Matrix")
                    .ok()
                    .and_then(|o| deref_obj(doc, o))
                    .and_then(|o| o.as_array().ok())
                    .and_then(|a| matrix_of(&numbers(a)))
                    .unwrap_or(Matrix::IDENTITY);
                XKind::Form { bbox, matrix }
            }
            _ => XKind::Other,
        };
        out.insert(name.clone(), kind);
    }
    out
}

fn annotation_rect(doc: &Document, annot: &Object) -> Option<PdfRect> {
    let dict = deref_obj(doc, annot)?.as_dict().ok()?;
    let rect = deref_obj(doc, dict.get(b"Rect").ok()?)?.as_array().ok()?;
    let n = numbers(rect);
    if n.len() != 4 {
        return None;
    }
    bounds_of(&[(n[0], n[1]), (n[2], n[3])])
}

fn number_object(value: f64) -> Object {
    if value.fract().abs() < 1e-9 && value.abs() < 1e9 {
        Object::Integer(value.round() as i64)
    } else {
        Object::Real(value as f32)
    }
}

/// Splits `bytes` around the glyph ranges in `ranges`: kept runs stay strings, each removed glyph
/// becomes the `TJ` number that moves the pen as the glyph did.
fn split_string(
    bytes: &[u8],
    format: StringFormat,
    ranges: &[(u32, u32, f64)],
) -> Option<Vec<Object>> {
    let mut ranges = ranges.to_vec();
    ranges.sort_by_key(|(start, end, _)| (*start, *end));
    let mut out = Vec::new();
    let mut cursor = 0usize;
    for (start, end, adjust) in ranges {
        let (start, end) = (start as usize, end as usize);
        if start < cursor || end < start || end > bytes.len() {
            return None;
        }
        if start > cursor {
            out.push(Object::String(bytes[cursor..start].to_vec(), format));
        }
        out.push(number_object(adjust));
        cursor = end;
    }
    if cursor < bytes.len() {
        out.push(Object::String(bytes[cursor..].to_vec(), format));
    }
    Some(out)
}

type GlyphTargets = BTreeMap<u32, Vec<(u32, u32, f64)>>;

/// The operations that replace show operator `op` once the glyphs in `targets` (by operand index)
/// are cut. `'` and `"` become their definitions (`T*` then show, plus `Tw` and `Tc`) so the show
/// can be a `TJ`.
fn rewrite_show(op: &Operation, targets: &GlyphTargets) -> Option<Vec<Operation>> {
    match op.operator.as_str() {
        "Tj" | "'" | "\"" => {
            let (string_index, mut out) = match op.operator.as_str() {
                "Tj" => (0usize, Vec::new()),
                "'" => (0usize, vec![Operation::new("T*", vec![])]),
                _ => (
                    2usize,
                    vec![
                        Operation::new("Tw", vec![op.operands.first()?.clone()]),
                        Operation::new("Tc", vec![op.operands.get(1)?.clone()]),
                        Operation::new("T*", vec![]),
                    ],
                ),
            };
            let Object::String(bytes, format) = op.operands.get(string_index)? else {
                return None;
            };
            let items = split_string(bytes, *format, targets.get(&0)?)?;
            out.push(Operation::new("TJ", vec![Object::Array(items)]));
            Some(out)
        }
        "TJ" => {
            let Object::Array(items) = op.operands.first()? else {
                return None;
            };
            let mut new_items = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                match (item, targets.get(&(index as u32))) {
                    (Object::String(bytes, format), Some(ranges)) => {
                        new_items.extend(split_string(bytes, *format, ranges)?)
                    }
                    _ => new_items.push(item.clone()),
                }
            }
            Some(vec![Operation::new("TJ", vec![Object::Array(new_items)])])
        }
        _ => None,
    }
}

fn handle_unsupported(
    plan: &mut PagePlan,
    page: u32,
    options: &RedactOptions,
    kind: &'static str,
    actions: &mut BTreeMap<usize, Action>,
    op_index: usize,
) -> Result<(), RedactError> {
    if !options.remove_unsupported {
        return Err(RedactError::UnsupportedContent { page, kind });
    }
    plan.unsupported += 1;
    actions.insert(op_index, Action::Delete);
    Ok(())
}

/// Reads page `page` and decides every change. `ignore_paths` leaves vector paths out of the
/// decision: the final check of a written document must not count the box this module painted.
fn plan_page(
    doc: &Document,
    page: u32,
    rects: &[PdfRect],
    options: &RedactOptions,
    ignore_paths: bool,
) -> Result<PagePlan, RedactError> {
    let malformed = || RedactError::MalformedContent { page };
    let page_id = *doc
        .get_pages()
        .get(&page)
        .ok_or(RedactError::PageOutOfRange(page))?;
    let page_dict = doc
        .get_object(page_id)
        .ok()
        .and_then(|o| o.as_dict().ok())
        .ok_or_else(malformed)?;

    let mut plan = PagePlan::default();

    // Annotations first: they do not depend on the content.
    if let Some(Object::Array(annots)) = page_dict.get(b"Annots").ok().and_then(|o| deref_obj(doc, o))
    {
        for (index, annot) in annots.iter().enumerate() {
            if annotation_rect(doc, annot).is_some_and(|rect| hits_inclusive(rects, &rect)) {
                plan.annots.push(index);
            }
        }
    }

    if !page_dict.has(b"Contents") {
        return Ok(plan);
    }
    let stream_ids = page_content_stream_ids(doc, page_id).map_err(|_| malformed())?;
    let mut distinct = HashSet::new();
    if !stream_ids.iter().all(|id| distinct.insert(*id)) {
        return Err(malformed());
    }

    // Text: every glyph the page graph placed, with where it came from.
    let graph = walk_page_graph(doc, "redact", page).map_err(|error| match error {
        PdfEditError::EncryptedDocument => RedactError::Encrypted,
        _ => malformed(),
    })?;
    let mut targets: HashMap<(usize, usize), GlyphTargets> = HashMap::new();
    for island in &graph.islands {
        // A show the walker could not place leaves an island with no glyphs: where its text sits
        // is unknown, so no mark on this page can be promised.
        if island.quads.is_empty() {
            return Err(RedactError::UnlocatableText { page });
        }
        let items: Vec<_> = island.run.iter().filter(|item| item.glyph.is_some()).collect();
        if items.len() != island.quads.len() {
            return Err(RedactError::UnlocatableText { page });
        }
        for (item, quad) in items.iter().zip(&island.quads) {
            let (Some(address), Some(bounds)) = (item.glyph, quad_bounds(quad)) else {
                continue;
            };
            if !hits(rects, &bounds) {
                continue;
            }
            let adjust = item
                .pen_adjust
                .ok_or(RedactError::CannotKeepPosition { page })?;
            let stream_index = address.contents_index as usize;
            if stream_ids
                .get(stream_index)
                .map(|id| (id.0, id.1))
                != Some(address.stream_ref)
            {
                return Err(malformed());
            }
            targets
                .entry((stream_index, address.operator_index as usize))
                .or_default()
                .entry(address.operand_index)
                .or_default()
                .push((address.glyph_start, address.glyph_end, adjust));
            plan.glyphs += 1;
        }
    }

    // Everything else, in one pass over the streams as one logical content stream.
    let xobjects = xobject_kinds(doc, page_dict);
    let mut budget = ContentBudget::new();
    let mut ctm = Matrix::IDENTITY;
    let mut line_width = 1.0f64;
    let mut stack: Vec<(Matrix, f64)> = Vec::new();
    let mut path_points: Vec<(f64, f64)> = Vec::new();
    let mut path_ops: Vec<(usize, usize)> = Vec::new();
    let mut path_clip = false;
    let mut all_actions: Vec<BTreeMap<usize, Action>> = Vec::new();
    let mut all_ops: Vec<Vec<Operation>> = Vec::new();
    for _ in &stream_ids {
        all_actions.push(BTreeMap::new());
    }

    for (stream_index, id) in stream_ids.iter().enumerate() {
        let content = budget.content(doc, *id, true).map_err(|_| malformed())?;
        let ops = content.operations;
        for (op_index, op) in ops.iter().enumerate() {
            if let Some(found) = targets.remove(&(stream_index, op_index)) {
                let replacement = rewrite_show(op, &found).ok_or_else(malformed)?;
                all_actions[stream_index].insert(op_index, Action::Replace(replacement));
                continue;
            }
            let n = numbers(&op.operands);
            match op.operator.as_str() {
                "q" => {
                    if stack.len() >= MAX_DEPTH {
                        return Err(malformed());
                    }
                    stack.push((ctm, line_width));
                }
                "Q" => {
                    if let Some((saved, width)) = stack.pop() {
                        ctm = saved;
                        line_width = width;
                    }
                }
                "cm" => {
                    if let Some(m) = matrix_of(&n) {
                        ctm = m.then(ctm);
                    }
                }
                "w" => {
                    if let Some(w) = n.first().filter(|w| w.is_finite()) {
                        line_width = *w;
                    }
                }
                "m" | "l" if n.len() == 2 => {
                    path_points.push(ctm.transform((n[0], n[1])));
                    path_ops.push((stream_index, op_index));
                }
                "c" if n.len() == 6 => {
                    for pair in n.chunks(2) {
                        path_points.push(ctm.transform((pair[0], pair[1])));
                    }
                    path_ops.push((stream_index, op_index));
                }
                "v" | "y" if n.len() == 4 => {
                    for pair in n.chunks(2) {
                        path_points.push(ctm.transform((pair[0], pair[1])));
                    }
                    path_ops.push((stream_index, op_index));
                }
                "re" if n.len() == 4 => {
                    let (x, y, w, h) = (n[0], n[1], n[2], n[3]);
                    for corner in [(x, y), (x + w, y), (x + w, y + h), (x, y + h)] {
                        path_points.push(ctm.transform(corner));
                    }
                    path_ops.push((stream_index, op_index));
                }
                "h" => path_ops.push((stream_index, op_index)),
                "W" | "W*" => path_clip = true,
                "n" => {
                    path_points.clear();
                    path_ops.clear();
                    path_clip = false;
                }
                "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" => {
                    let stroked = matches!(op.operator.as_str(), "S" | "s" | "B" | "B*" | "b" | "b*");
                    let bounds = bounds_of(&path_points).map(|b| {
                        if !stroked {
                            return b;
                        }
                        let scale = (ctm.a * ctm.d - ctm.b * ctm.c).abs().sqrt();
                        let half = (line_width.abs() * scale / 2.0).max(0.0);
                        PdfRect {
                            x: b.x - half,
                            y: b.y - half,
                            width: b.width + 2.0 * half,
                            height: b.height + 2.0 * half,
                        }
                    });
                    if !ignore_paths && bounds.is_some_and(|b| hits_inclusive(rects, &b)) {
                        plan.paths += 1;
                        if path_clip {
                            // Keep the clip, paint nothing.
                            all_actions[stream_index]
                                .insert(op_index, Action::Replace(vec![Operation::new("n", vec![])]));
                        } else {
                            for (s, o) in path_ops.drain(..) {
                                all_actions[s].insert(o, Action::Delete);
                            }
                            all_actions[stream_index].insert(op_index, Action::Delete);
                        }
                    }
                    path_points.clear();
                    path_ops.clear();
                    path_clip = false;
                }
                "sh" => {
                    // A shading fills the whole clip: its extent is not known here.
                    handle_unsupported(
                        &mut plan,
                        page,
                        options,
                        "shading",
                        &mut all_actions[stream_index],
                        op_index,
                    )?;
                }
                "BI" => {
                    let corners = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|p| ctm.transform(p));
                    if bounds_of(&corners).is_some_and(|b| hits_inclusive(rects, &b)) {
                        // The operator, its data and `EI` are separate operations: no clean cut.
                        return Err(RedactError::UnsupportedContent {
                            page,
                            kind: "inline image",
                        });
                    }
                }
                "Do" => {
                    let kind = op
                        .operands
                        .first()
                        .and_then(|o| o.as_name().ok())
                        .and_then(|name| xobjects.get(name));
                    match kind {
                        Some(XKind::Image) => {
                            let corners =
                                [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].map(|p| ctm.transform(p));
                            if bounds_of(&corners).is_some_and(|b| hits_inclusive(rects, &b)) {
                                plan.images += 1;
                                all_actions[stream_index].insert(op_index, Action::Delete);
                            }
                        }
                        Some(XKind::Form { bbox, matrix }) => {
                            // A form with no usable BBox has an unknown extent: it counts as a hit.
                            let hit = match bbox {
                                None => true,
                                Some([x0, y0, x1, y1]) => {
                                    let to_page = matrix.then(ctm);
                                    let corners =
                                        [(*x0, *y0), (*x1, *y0), (*x1, *y1), (*x0, *y1)]
                                            .map(|p| to_page.transform(p));
                                    bounds_of(&corners).is_none_or(|b| hits_inclusive(rects, &b))
                                }
                            };
                            if hit {
                                handle_unsupported(
                                    &mut plan,
                                    page,
                                    options,
                                    "form XObject",
                                    &mut all_actions[stream_index],
                                    op_index,
                                )?;
                            }
                        }
                        Some(XKind::Other) | None => {}
                    }
                }
                "BDC" => {
                    if let Some(Object::Dictionary(props)) = op.operands.get(1) {
                        let mut cleaned = props.clone();
                        let mut removed = 0;
                        for key in [&b"ActualText"[..], b"Alt", b"E"] {
                            if cleaned.remove(key).is_some() {
                                removed += 1;
                            }
                        }
                        if removed > 0 {
                            plan.hidden_text += removed;
                            let mut operands = op.operands.clone();
                            operands[1] = Object::Dictionary(cleaned);
                            all_actions[stream_index]
                                .insert(op_index, Action::Replace(vec![Operation::new("BDC", operands)]));
                        }
                    }
                }
                _ => {}
            }
        }
        all_ops.push(ops);
    }
    if !targets.is_empty() {
        // An address the strict parse does not have.
        return Err(malformed());
    }
    plan.end_depth = stack.len();
    for ((id, ops), actions) in stream_ids.iter().zip(all_ops).zip(all_actions) {
        plan.streams.push(StreamPlan {
            id: *id,
            ops,
            actions,
        });
    }
    Ok(plan)
}

// ---------------------------------------------------------------------------------------------
// Applying
// ---------------------------------------------------------------------------------------------

fn apply_plan(doc: &mut Document, page_id: ObjectId, plan: PagePlan) -> Result<(), RedactError> {
    for stream_plan in plan.streams {
        if stream_plan.actions.is_empty() {
            continue;
        }
        let StreamPlan {
            id,
            ops,
            mut actions,
        } = stream_plan;
        let mut out: Vec<Operation> = Vec::with_capacity(ops.len());
        for (index, op) in ops.into_iter().enumerate() {
            match actions.remove(&index) {
                None => out.push(op),
                Some(Action::Delete) => {}
                Some(Action::Replace(new_ops)) => out.extend(new_ops),
            }
        }
        let bytes = Content { operations: out }
            .encode()
            .map_err(|e| RedactError::Write(e.to_string()))?;
        match doc.get_object_mut(id) {
            Ok(Object::Stream(stream)) => stream.set_plain_content(bytes),
            _ => return Err(RedactError::Write("content stream vanished".to_string())),
        }
    }
    if !plan.annots.is_empty() {
        let remaining: Vec<Object> = {
            let page = doc
                .get_object(page_id)
                .ok()
                .and_then(|o| o.as_dict().ok())
                .ok_or_else(|| RedactError::Write("page vanished".to_string()))?;
            match page.get(b"Annots").ok().and_then(|o| deref_obj(doc, o)) {
                Some(Object::Array(items)) => items
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| !plan.annots.contains(index))
                    .map(|(_, item)| item.clone())
                    .collect(),
                _ => Vec::new(),
            }
        };
        if let Ok(Object::Dictionary(page)) = doc.get_object_mut(page_id) {
            if remaining.is_empty() {
                page.remove(b"Annots");
            } else {
                page.set("Annots", Object::Array(remaining));
            }
        }
    }
    Ok(())
}

/// Wraps the page's content so the boxes are painted in default user space, and paints them.
fn add_overlay(
    doc: &mut Document,
    page_id: ObjectId,
    rects: &[PdfRect],
    fill: [f64; 3],
    end_depth: usize,
) -> Result<(), RedactError> {
    let existing = page_content_stream_ids(doc, page_id)
        .map_err(|_| RedactError::Write("page contents unreadable".to_string()))
        .or_else(|error| {
            let has_contents = doc
                .get_object(page_id)
                .ok()
                .and_then(|o| o.as_dict().ok())
                .is_some_and(|p| p.has(b"Contents"));
            if has_contents {
                Err(error)
            } else {
                Ok(Vec::new())
            }
        })?;
    let head = doc.add_object(Stream::new(Dictionary::new(), b"q".to_vec()));
    let mut tail = String::new();
    // The head `q` plus any the page left open: back to default user space.
    for _ in 0..=end_depth {
        tail.push_str("Q\n");
    }
    tail.push_str(&format!("q\n{} {} {} rg\n", fill[0], fill[1], fill[2]));
    for rect in rects {
        tail.push_str(&format!(
            "{} {} {} {} re\n",
            rect.x, rect.y, rect.width, rect.height
        ));
    }
    tail.push_str("f\nQ");
    let tail_id = doc.add_object(Stream::new(Dictionary::new(), tail.into_bytes()));
    let mut refs = Vec::with_capacity(existing.len() + 2);
    refs.push(Object::Reference(head));
    refs.extend(existing.into_iter().map(Object::Reference));
    refs.push(Object::Reference(tail_id));
    match doc.get_object_mut(page_id) {
        Ok(Object::Dictionary(page)) => {
            page.set("Contents", Object::Array(refs));
            Ok(())
        }
        _ => Err(RedactError::Write("page vanished".to_string())),
    }
}

/// Removes hidden-information entries; returns the names removed.
fn sanitize_document(doc: &mut Document, redacted_pages: &[ObjectId]) -> Vec<String> {
    let mut removed = Vec::new();
    if doc.trailer.remove(b"Info").is_some() {
        removed.push("Info".to_string());
    }
    let root = doc
        .trailer
        .get(b"Root")
        .ok()
        .and_then(|o| o.as_reference().ok());
    let mut names_ref: Option<ObjectId> = None;
    if let Some(root) = root {
        if let Ok(Object::Dictionary(catalog)) = doc.get_object_mut(root) {
            let keys: [&[u8]; 5] = [b"Metadata", b"PieceInfo", b"StructTreeRoot", b"MarkInfo", b"AF"];
            for key in keys {
                if catalog.remove(key).is_some() {
                    removed.push(String::from_utf8_lossy(key).into_owned());
                }
            }
            match catalog.get_mut(b"Names") {
                Ok(Object::Dictionary(names)) => {
                    for key in [&b"EmbeddedFiles"[..], b"JavaScript"] {
                        if names.remove(key).is_some() {
                            removed.push(format!("Names/{}", String::from_utf8_lossy(key)));
                        }
                    }
                }
                Ok(Object::Reference(id)) => names_ref = Some(*id),
                _ => {}
            }
        }
    }
    if let Some(id) = names_ref {
        if let Ok(Object::Dictionary(names)) = doc.get_object_mut(id) {
            for key in [&b"EmbeddedFiles"[..], b"JavaScript"] {
                if names.remove(key).is_some() {
                    removed.push(format!("Names/{}", String::from_utf8_lossy(key)));
                }
            }
        }
    }
    for id in redacted_pages {
        if let Ok(Object::Dictionary(page)) = doc.get_object_mut(*id) {
            for key in [&b"Thumb"[..], b"Metadata", b"PieceInfo"] {
                if page.remove(key).is_some() {
                    removed.push(format!("Page/{}", String::from_utf8_lossy(key)));
                }
            }
        }
    }
    removed
}

fn group_by_page(marks: &[RedactMark]) -> Result<BTreeMap<u32, Vec<PdfRect>>, RedactError> {
    if marks.is_empty() {
        return Err(RedactError::NoMarks);
    }
    let mut groups: BTreeMap<u32, Vec<PdfRect>> = BTreeMap::new();
    for mark in marks {
        if mark.page == 0 {
            return Err(RedactError::InvalidMark("pages are 1-based".to_string()));
        }
        validate_rect(&mark.rect)?;
        groups.entry(mark.page).or_default().push(mark.rect);
    }
    Ok(groups)
}

fn shrunk(rects: &[PdfRect]) -> Vec<PdfRect> {
    rects
        .iter()
        .map(|r| {
            let dx = VERIFY_SHRINK.min(r.width / 4.0);
            let dy = VERIFY_SHRINK.min(r.height / 4.0);
            PdfRect {
                x: r.x + dx,
                y: r.y + dy,
                width: r.width - 2.0 * dx,
                height: r.height - 2.0 * dy,
            }
        })
        .collect()
}

/// Content streams used by more than one page (or twice by one) cannot be edited for one page.
fn shared_streams(doc: &Document) -> HashSet<ObjectId> {
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut shared = HashSet::new();
    for page_id in doc.page_iter() {
        for id in doc.get_page_contents(page_id) {
            if !seen.insert(id) {
                shared.insert(id);
            }
        }
    }
    shared
}

/// Removes the content under `marks` and returns the new document bytes. See the module
/// documentation for exactly what is guaranteed and refused.
pub fn apply_marks(
    source: &[u8],
    marks: &[RedactMark],
    options: &RedactOptions,
) -> Result<RedactOutcome, RedactError> {
    let groups = group_by_page(marks)?;
    let mut doc = load_pdf(source).map_err(|e| RedactError::Parse(e.to_string()))?;
    if doc.is_encrypted() {
        return Err(RedactError::Encrypted);
    }
    let pages = doc.get_pages();
    for page in groups.keys() {
        if !pages.contains_key(page) {
            return Err(RedactError::PageOutOfRange(*page));
        }
    }
    let shared = shared_streams(&doc);
    let mut report = RedactReport::default();
    let mut end_depths: BTreeMap<u32, usize> = BTreeMap::new();

    for (page, rects) in &groups {
        let plan = plan_page(&doc, *page, rects, options, false)?;
        if plan.streams.iter().any(|s| shared.contains(&s.id)) {
            return Err(RedactError::SharedContentStream { page: *page });
        }
        report.glyphs_removed += plan.glyphs;
        report.images_removed += plan.images;
        report.paths_removed += plan.paths;
        report.unsupported_removed += plan.unsupported;
        report.annotations_removed += plan.annots.len();
        report.hidden_text_entries_removed += plan.hidden_text;
        end_depths.insert(*page, plan.end_depth);
        apply_plan(&mut doc, pages[page], plan)?;
        report.pages.push(*page);
    }

    // Check the edited document before anything is painted over it.
    for (page, rects) in &groups {
        let check = plan_page(&doc, *page, &shrunk(rects), options, false)?;
        if !check.is_clean() {
            return Err(RedactError::VerificationFailed {
                page: *page,
                what: check.what(),
            });
        }
    }

    if let Some(fill) = options.fill {
        for (page, rects) in &groups {
            add_overlay(&mut doc, pages[page], rects, fill, end_depths[page])?;
        }
    }
    if options.sanitize {
        let redacted: Vec<ObjectId> = groups.keys().map(|p| pages[p]).collect();
        report.sanitized = sanitize_document(&mut doc, &redacted);
    }
    doc.prune_objects();
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)
        .map_err(|e| RedactError::Write(e.to_string()))?;

    // And the bytes as written: what a reader will open.
    verify_redacted(&bytes, marks, options)?;
    Ok(RedactOutcome { bytes, report })
}

/// Re-reads `bytes` and confirms no glyph, image, form, shading or (without a painted box) path
/// remains under `marks`. [`apply_marks`] runs this on its own output; hosts can run it on a
/// file they are about to publish. A page that cannot be read fails closed.
pub fn verify_redacted(
    bytes: &[u8],
    marks: &[RedactMark],
    options: &RedactOptions,
) -> Result<(), RedactError> {
    let groups = group_by_page(marks)?;
    let doc = load_pdf(bytes).map_err(|e| RedactError::Parse(e.to_string()))?;
    if doc.is_encrypted() {
        return Err(RedactError::Encrypted);
    }
    for (page, rects) in &groups {
        let check = plan_page(&doc, *page, &shrunk(rects), options, options.fill.is_some())?;
        if !check.is_clean() {
            return Err(RedactError::VerificationFailed {
                page: *page,
                what: check.what(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspect::page_graph_from_bytes;
    use crate::test_fixtures::build_one_page_pdf;
    use lopdf::dictionary;

    fn save(mut doc: Document) -> Vec<u8> {
        let mut out = Vec::new();
        doc.save_to(&mut out).expect("fixture saves");
        out
    }

    fn mark(page: u32, x: f64, y: f64, width: f64, height: f64) -> RedactMark {
        RedactMark {
            id: 1,
            page,
            rect: PdfRect {
                x,
                y,
                width,
                height,
            },
        }
    }

    fn page_text(bytes: &[u8]) -> String {
        let graph = page_graph_from_bytes(bytes, 1).expect("page reads");
        graph
            .islands
            .iter()
            .map(|i| i.text.as_str())
            .collect::<Vec<_>>()
            .join("|")
    }

    #[test]
    fn marks_validate_and_clear() {
        let mut marks = RedactMarks::new();
        assert!(marks
            .add(
                1,
                PdfRect {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 5.0
                }
            )
            .is_err());
        let rect = PdfRect {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
        };
        let a = marks.add(1, rect).unwrap();
        let _b = marks.add(2, rect).unwrap();
        assert!(marks.remove(a));
        assert!(!marks.remove(a));
        assert_eq!(marks.clear_page(2), 1);
        assert!(marks.is_empty());
    }

    #[test]
    fn cuts_the_text_under_a_mark_and_keeps_the_rest_in_place() {
        // Helvetica 12 pt from x = 72: "Hello " ends at about 102.7 pt, "World" follows.
        let source = save(build_one_page_pdf(
            b"BT /F1 12 Tf 72 700 Td (Hello World) Tj ET",
        ));
        let outcome = apply_marks(
            &source,
            &[mark(1, 100.0, 695.0, 100.0, 20.0)],
            &RedactOptions::default(),
        )
        .expect("redaction applies");
        assert!(outcome.report.glyphs_removed >= 5);
        let text = page_text(&outcome.bytes);
        assert!(text.contains("Hello"), "kept text: {text}");
        assert!(!text.contains("World"), "removed text: {text}");
        // The bytes of the removed word are not in any stream either.
        let doc = load_pdf(&outcome.bytes).unwrap();
        let raw: Vec<u8> = doc
            .objects
            .values()
            .filter_map(|o| o.as_stream().ok())
            .flat_map(|s| s.decompressed_content().unwrap_or_default())
            .collect();
        assert!(!raw.windows(5).any(|w| w == b"World"));
    }

    #[test]
    fn text_outside_every_mark_is_untouched() {
        let source = save(build_one_page_pdf(
            b"BT /F1 12 Tf 72 700 Td (Keep) Tj ET BT /F1 12 Tf 72 100 Td (Drop) Tj ET",
        ));
        let outcome = apply_marks(
            &source,
            &[mark(1, 60.0, 90.0, 200.0, 30.0)],
            &RedactOptions::default(),
        )
        .unwrap();
        let text = page_text(&outcome.bytes);
        assert!(text.contains("Keep") && !text.contains("Drop"), "{text}");
    }

    #[test]
    fn quote_operators_are_expanded_not_refused() {
        let source = save(build_one_page_pdf(
            b"BT /F1 12 Tf 14 TL 72 700 Td (Line one) Tj (Secret) ' ET",
        ));
        // The second line sits one leading (14 pt) below the first.
        let outcome = apply_marks(
            &source,
            &[mark(1, 60.0, 680.0, 200.0, 12.0)],
            &RedactOptions::default(),
        )
        .unwrap();
        let text = page_text(&outcome.bytes);
        assert!(text.contains("Line one") && !text.contains("Secret"), "{text}");
    }

    #[test]
    fn an_image_under_a_mark_is_removed_and_the_object_pruned() {
        let mut doc = build_one_page_pdf(b"q 100 0 0 100 200 200 cm /Im0 Do Q");
        let image = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1,
                "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8,
            },
            vec![0x7f],
        ));
        let page_id = *doc.get_pages().get(&1).unwrap();
        let resources_id = doc
            .get_object(page_id)
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"Resources")
            .unwrap()
            .as_reference()
            .unwrap();
        if let Ok(Object::Dictionary(resources)) = doc.get_object_mut(resources_id) {
            resources.set("XObject", dictionary! { "Im0" => image });
        }
        let source = save(doc);
        let outcome = apply_marks(
            &source,
            &[mark(1, 250.0, 250.0, 20.0, 20.0)],
            &RedactOptions::default(),
        )
        .unwrap();
        assert_eq!(outcome.report.images_removed, 1);
        let out = load_pdf(&outcome.bytes).unwrap();
        assert!(!out
            .objects
            .values()
            .any(|o| o.as_stream().is_ok_and(|s| s.dict.get(b"Subtype").is_ok_and(|v| v.as_name().is_ok_and(|n| n == b"Image")))));
    }

    #[test]
    fn a_covered_path_is_removed() {
        let source = save(build_one_page_pdf(b"0.5 g 100 100 50 50 re f"));
        let outcome = apply_marks(
            &source,
            &[mark(1, 90.0, 90.0, 80.0, 80.0)],
            &RedactOptions::default(),
        )
        .unwrap();
        assert_eq!(outcome.report.paths_removed, 1);
    }

    #[test]
    fn text_that_cannot_be_placed_refuses_the_page() {
        // No `Tf`: the walker has no font, so where the text sits is unknown.
        let source = save(build_one_page_pdf(b"BT 72 700 Td (x) Tj ET"));
        let error = apply_marks(
            &source,
            &[mark(1, 0.0, 0.0, 10.0, 10.0)],
            &RedactOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(error, RedactError::UnlocatableText { page: 1 }), "{error}");
    }

    #[test]
    fn a_form_under_a_mark_is_refused_unless_removal_is_allowed() {
        let mut doc = build_one_page_pdf(b"/Fm0 Do");
        let form = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Form",
                "BBox" => Object::Array(vec![Object::Integer(0), Object::Integer(0), Object::Integer(100), Object::Integer(100)]),
            },
            b"0 0 50 50 re f".to_vec(),
        ));
        let page_id = *doc.get_pages().get(&1).unwrap();
        let resources_id = doc
            .get_object(page_id)
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"Resources")
            .unwrap()
            .as_reference()
            .unwrap();
        if let Ok(Object::Dictionary(resources)) = doc.get_object_mut(resources_id) {
            resources.set("XObject", dictionary! { "Fm0" => form });
        }
        let source = save(doc);
        let marks = [mark(1, 10.0, 10.0, 10.0, 10.0)];
        let error = apply_marks(&source, &marks, &RedactOptions::default()).unwrap_err();
        assert!(matches!(error, RedactError::UnsupportedContent { kind: "form XObject", .. }));
        let allowed = RedactOptions {
            remove_unsupported: true,
            ..RedactOptions::default()
        };
        let outcome = apply_marks(&source, &marks, &allowed).unwrap();
        assert_eq!(outcome.report.unsupported_removed, 1);
    }

    #[test]
    fn no_marks_and_missing_pages_are_typed_errors() {
        let source = save(build_one_page_pdf(b"BT /F1 12 Tf 72 700 Td (x) Tj ET"));
        assert!(matches!(
            apply_marks(&source, &[], &RedactOptions::default()),
            Err(RedactError::NoMarks)
        ));
        assert!(matches!(
            apply_marks(&source, &[mark(9, 0.0, 0.0, 1.0, 1.0)], &RedactOptions::default()),
            Err(RedactError::PageOutOfRange(9))
        ));
    }
}
