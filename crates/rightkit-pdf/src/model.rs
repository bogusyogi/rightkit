//! Shared PDF-editor wire/model types (Dispatch A, lane A1).
//!
//! Owns every Rust type other PDF-editor lanes compile against, per
//! `docs/PDF-EDITOR-NOW.md` §5.1 (wire models), §5.2 (inspection state types), §7.2 (font catalog
//! API), and §11 (typed errors), plus the digest helpers those sections describe and the shared
//! `PdfEditCommand`/`ComposedCandidate` types the frozen composer signature depends on
//! (`docs/PDF-EDITOR-DISPATCH.md`, "Frozen cross-lane seams"):
//!
//! ```ignore
//! pub fn compose_candidate(
//!     source: &[u8],
//!     commands: &[PdfEditCommand],
//!     fonts: &dyn PdfFontResolver,
//! ) -> Result<ComposedCandidate, PdfEditError>;
//! ```
//!
//! That function itself is defined by `pdf_compose.rs` (Dispatch C, lane C1) — this module only
//! provides the types its signature depends on, so downstream lanes can build and test against a
//! stable contract before the real implementation lands. Likewise `inspect_pdf_page`'s stateful
//! content walker (§5.2's `visit_text_show`, §5.3's `may_join`/painted-bounds inventory) is
//! `pdf_inspect.rs` (lane B1); this module owns only the `Matrix`/`TextState`/`PositionedGlyph`
//! family of types that walker is built from.
//!
//! This file makes no filesystem or Tauri-state assumptions and defines no `#[tauri::command]` —
//! every command in this effort (`inspect_pdf_page`, `list_pdf_fonts`, `compose_pdf_preview`,
//! `prepare_pdf_save`, `commit_pdf_save`) belongs to a later lane's file.

use std::fmt;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------------------------
// §11 — Typed failures
// ---------------------------------------------------------------------------------------------

/// Stable, typed rejection reasons for PDF-editor commands. Crosses Tauri IPC as a JSON tag +
/// message; `thiserror::Error` gives each variant the exact user-facing string from §11. Kept
/// verbatim against the contract — do not add fields or reorder variants without updating §11
/// first, since every lane (B1 inspection, B2 fonts, C1 composition, C2 save) matches on these by
/// name.
///
/// `Serialize` is hand-written, not derived: this codebase's existing typed-error convention
/// (the host error type in `error.rs`) wraps every variant in a `{ code, message }` envelope via
/// `#[serde(tag = "code", content = "message", rename_all = "snake_case")]`. That attribute only
/// populates `content` for variants that carry data; every `PdfEditError` variant is a bare unit
/// variant (the message comes from `thiserror`'s `Display`, not a field), so the derive would
/// serialize as a bare string tag with no `message` key. `pdfEditModel.ts` (A2) already models the
/// frontend side as `{ code: PdfEditErrorCode, message: string }` matching the host error type's
/// envelope — this manual impl produces exactly that shape so the two sides agree without D2
/// needing to reconcile a mismatch later.
#[derive(Debug, thiserror::Error)]
pub enum PdfEditError {
    #[error("PDF changed since edit started")]
    StaleSource,
    #[error("source operand changed")]
    SourceDigestMismatch,
    #[error("visible text has no unique editable source")]
    #[allow(dead_code)] // part of §11's complete error taxonomy; not yet raised by any lane.
    NoUniqueMatch,
    #[error("text transform is unsupported")]
    UnsupportedTransform,
    #[error("font encoding is unsupported")]
    UnsupportedEncoding,
    #[error("selected font lacks a required glyph")]
    MissingGlyph,
    #[error("font license forbids embedding")]
    FontEmbeddingDenied,
    #[error("text exceeds safe bounds")]
    TextOverflow,
    #[error("PDF content stream is malformed")]
    MalformedContent,
    #[error("encrypted PDF cannot be edited")]
    EncryptedDocument,
    #[error("annotations could not be serialized")]
    AnnotationSerializationFailed,
    #[error("candidate verification failed")]
    VerificationFailed,
    #[error("target changed before save")]
    WriteConflict,
}

impl PdfEditError {
    /// Stable snake_case wire code for this variant — the `code` half of the `{ code, message }`
    /// envelope below, matching `pdfEditModel.ts`'s `PdfEditErrorCode` union 1:1 by name.
    pub fn code(&self) -> &'static str {
        match self {
            PdfEditError::StaleSource => "stale_source",
            PdfEditError::SourceDigestMismatch => "source_digest_mismatch",
            PdfEditError::NoUniqueMatch => "no_unique_match",
            PdfEditError::UnsupportedTransform => "unsupported_transform",
            PdfEditError::UnsupportedEncoding => "unsupported_encoding",
            PdfEditError::MissingGlyph => "missing_glyph",
            PdfEditError::FontEmbeddingDenied => "font_embedding_denied",
            PdfEditError::TextOverflow => "text_overflow",
            PdfEditError::MalformedContent => "malformed_content",
            PdfEditError::EncryptedDocument => "encrypted_document",
            PdfEditError::AnnotationSerializationFailed => "annotation_serialization_failed",
            PdfEditError::VerificationFailed => "verification_failed",
            PdfEditError::WriteConflict => "write_conflict",
        }
    }
}

impl Serialize for PdfEditError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("PdfEditError", 2)?;
        state.serialize_field("code", self.code())?;
        state.serialize_field("message", &self.to_string())?;
        state.end()
    }
}

// ---------------------------------------------------------------------------------------------
// §5.1 — Wire models
// ---------------------------------------------------------------------------------------------

/// One point in unrotated PDF user-space (or device space, depending on call site — callers
/// document which). Mirrors frontend `PdfPoint` (§6, owned by `pdfEditModel.ts`).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PdfPoint {
    pub x: f64,
    pub y: f64,
}

/// Axis-aligned rectangle in PDF user-space points. Mirrors frontend `PdfRect` (§6/§8, owned by
/// `pdfEditModel.ts`). Used for §8 layout validation (`available.contains(run.bounds)`) and §5.3's
/// painted-bounds inventory.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PdfRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl PdfRect {
    /// True if `other` fits entirely inside `self`, both edges inclusive.
    pub fn contains(&self, other: PdfRect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.x + other.width <= self.x + self.width
            && other.y + other.height <= self.y + self.height
    }

    /// True if `self` and `other` overlap with nonzero area (touching edges do not count).
    pub fn intersects(&self, other: PdfRect) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }
}

/// RGBA color for text-fill appearance. Mirrors frontend `PdfTextAppearance["color"]` (§6).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PdfColor {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub alpha: f64,
}

/// Requested/applied text appearance for an edit. Mirrors frontend `PdfTextAppearance` (§6)
/// verbatim (field order/names), used by [`PdfTextIsland::appearance`] and by [`PdfEditCommand`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PdfTextAppearance {
    pub font_id: String,
    pub family: String,
    pub size_pt: f64,
    pub color: PdfColor,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // §6 wire shape; not yet constructed by any lane in this crate.
pub struct PdfDocumentRevision {
    pub document_sha256: String,
    pub page_count: u32,
    pub pages: Vec<PdfPageGraph>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfPageGraph {
    pub page: u32,
    /// Revision identity for this page, including scanned pages that have no source text islands.
    pub document_sha256: String,
    pub media_box: [f64; 4],
    pub crop_box: [f64; 4],
    pub rotation: i32,
    pub islands: Vec<PdfTextIsland>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfTextIsland {
    pub id: String,
    pub text: String,
    pub quads: Vec<[f64; 8]>,
    pub baseline: [f64; 4],
    pub appearance: PdfTextAppearance,
    pub locator: PdfSourceLocator,
    pub editable: bool,
    pub rejection: Option<PdfEditError>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfSourceLocator {
    pub document_sha256: String,
    pub page: u32,
    pub spans: Vec<PdfSourceSpan>,
    pub island_digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfSourceSpan {
    pub stream_ref: (u32, u16),
    pub contents_index: u32,
    pub operator_index: u32,
    pub operand_index: u32,
    pub glyph_start: u32,
    pub glyph_end: u32,
    pub expected_text: String,
    pub expected_digest: String,
}

// ---------------------------------------------------------------------------------------------
// §5.2 — Inspection state: Matrix / TextState / PositionedGlyph family
// ---------------------------------------------------------------------------------------------
//
// These types back `visit_text_show` and §5.3's island grouping, both implemented by
// `pdf_inspect.rs` (lane B1). This module provides their concrete shapes plus generic matrix/text-
// state algebra (matching PDF spec 9.4.2-9.4.4); font-specific decoding is abstracted behind
// `TextStateFont` so this module stays independent of any particular font-parsing strategy.

/// 2D affine transform in PDF row-vector convention: `(x' y' 1) = (x y 1) x M`. Used for both the
/// graphics-state CTM (`cm`/`q`/`Q`) and the text matrix/text-line matrix (`Tm`/`Td`/`TD`/`T*`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn translation(tx: f64, ty: f64) -> Matrix {
        Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        }
    }

    /// `self` composed with (applied before) `other`: `self x other`.
    pub fn then(self, other: Matrix) -> Matrix {
        Matrix {
            a: self.a * other.a + self.b * other.c,
            b: self.a * other.b + self.b * other.d,
            c: self.c * other.a + self.d * other.c,
            d: self.c * other.b + self.d * other.d,
            e: self.e * other.a + self.f * other.c + other.e,
            f: self.e * other.b + self.f * other.d + other.f,
        }
    }

    /// Transforms `point` (row vector) through this matrix.
    pub fn transform(self, point: (f64, f64)) -> (f64, f64) {
        (
            point.0 * self.a + point.1 * self.c + self.e,
            point.0 * self.b + point.1 * self.d + self.f,
        )
    }

    /// This matrix's translation component — for a text matrix, the run's PDF-space origin.
    pub fn origin(self) -> (f64, f64) {
        (self.e, self.f)
    }

    /// Post-show horizontal advance (PDF spec 9.4.3): `self = [1 0 0 1 tx ty] x self`.
    pub fn translate_text(&mut self, tx: f64, ty: f64) {
        *self = Matrix::translation(tx, ty).then(*self);
    }
}

/// One decoded glyph from a `Tj`/`TJ` show operand. `source_range` is the glyph's byte range
/// within the *decoded* operand string (start inclusive, end exclusive) and feeds
/// [`SourceAddress::with_glyph`] for later island-span grouping (§5.3). `bbox`, when the font
/// decoder can supply it, is the glyph's font-unit bounding box (`x_min, y_min, x_max, y_max`,
/// scaled to `/1000` em units) used by [`TextState::glyph_quad`] for exact geometry; when absent,
/// `glyph_quad` falls back to an advance-width approximation.
#[derive(Clone, Debug)]
pub struct DecodedGlyph {
    pub cid: u32,
    pub unicode: String,
    pub source_range: (u32, u32),
    pub bbox: Option<[f64; 4]>,
}

/// Per-page active font handle used while walking a content stream (§5.2's `visit_text_show`).
/// Implemented by whatever page-font resolver `pdf_inspect.rs` (lane B1) builds from a page's
/// `/Resources/Font` — distinct from [`PdfFontResolver`] (§7.2), which resolves *catalog* fonts
/// for composition/embedding, not content-stream decoding.
pub trait TextStateFont: fmt::Debug {
    /// Stable identifier for this font within its page's resource dictionary, threaded onto every
    /// [`PositionedGlyph`] this font produces (`font_id`).
    fn id(&self) -> &str;

    /// Decode one `Tj`/`TJ` string operand into glyphs via this font's encoding/ToUnicode map.
    fn decode_show_operand(
        &self,
        operand: &lopdf::Object,
    ) -> Result<Vec<DecodedGlyph>, PdfEditError>;

    /// Glyph advance width, in text-space units already scaled by `font_size` (i.e. `w0/1000 *
    /// font_size`), for `cid`.
    fn advance(&self, cid: u32, font_size: f64) -> f64;
}

/// Text-object render mode (`Tr`), per PDF spec 9.3.6. Only fill/stroke visibility matters for
/// island grouping's `compatible_paint` (§5.3); modes 4-7 (clipping variants) are represented but
/// current scope treats any clipping mode as unsupported at the call site, not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextRenderMode {
    Fill,
    Stroke,
    FillStroke,
    Invisible,
    FillClip,
    StrokeClip,
    FillStrokeClip,
    Clip,
}

impl TextRenderMode {
    /// Decode a raw `Tr` operand (0-7); unknown values conservatively map to `Invisible` so callers
    /// don't mistake an out-of-range mode for paintable text.
    pub fn from_operand(value: i64) -> TextRenderMode {
        match value {
            0 => TextRenderMode::Fill,
            1 => TextRenderMode::Stroke,
            2 => TextRenderMode::FillStroke,
            4 => TextRenderMode::FillClip,
            5 => TextRenderMode::StrokeClip,
            6 => TextRenderMode::FillStrokeClip,
            7 => TextRenderMode::Clip,
            _ => TextRenderMode::Invisible,
        }
    }
}

/// Paint identity of a run of text, used by §5.3's `compatible_paint` to decide whether two
/// adjacent glyphs may join into the same island.
#[derive(Clone, Debug, PartialEq)]
pub struct PaintState {
    pub render_mode: TextRenderMode,
    pub fill_color: Option<[f64; 4]>,
    pub stroke_color: Option<[f64; 4]>,
}

/// Text writing direction. Current scope supports left-to-right only (§7.3); the other variants
/// exist so a walker can classify and then reject unsupported content with a typed error instead
/// of silently misinterpreting it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WritingDirection {
    LeftToRight,
    #[allow(dead_code)]
    // classification-only per the doc comment above; §7.3 rejects, never builds, one.
    RightToLeft,
    #[allow(dead_code)]
    Vertical,
}

/// Address of one show-operand (`Tj`, or one string item inside a `TJ` array) before any glyph
/// index is known — built once per operator, then narrowed per glyph via
/// [`SourceAddress::with_glyph`].
#[derive(Clone, Debug)]
pub struct SourceAddress {
    pub stream_ref: (u32, u16),
    pub contents_index: u32,
    pub operator_index: u32,
    pub operand_index: u32,
}

impl SourceAddress {
    /// Narrows this operand-level address to one glyph's byte range within it. Adjacent
    /// [`GlyphSourceAddress`] values sharing the same operand fields are merged by §5.3's island
    /// grouping into a single [`PdfSourceSpan`] (min `glyph_start`, max `glyph_end`).
    pub fn with_glyph(&self, glyph_range: (u32, u32)) -> GlyphSourceAddress {
        GlyphSourceAddress {
            stream_ref: self.stream_ref,
            contents_index: self.contents_index,
            operator_index: self.operator_index,
            operand_index: self.operand_index,
            glyph_start: glyph_range.0,
            glyph_end: glyph_range.1,
        }
    }
}

/// One glyph's source address — the same shape as [`PdfSourceSpan`] minus the fields
/// (`expected_text`, `expected_digest`) that only become knowable once island grouping has
/// concatenated a run of glyphs' decoded text.
#[derive(Clone, Copy, Debug)]
pub struct GlyphSourceAddress {
    pub stream_ref: (u32, u16),
    pub contents_index: u32,
    pub operator_index: u32,
    pub operand_index: u32,
    pub glyph_start: u32,
    pub glyph_end: u32,
}

/// One glyph positioned in both PDF user-space (via `text_matrix`/`ctm`) and device space (via
/// `quad`), as produced by `visit_text_show` (§5.2) and consumed by §5.3's `may_join` grouping and
/// painted-bounds inventory.
#[derive(Clone, Debug)]
pub struct PositionedGlyph {
    pub page: u32,
    pub unicode: String,
    #[allow(dead_code)] // populated by the walker for a future CID-aware consumer; not yet read.
    pub cid: u32,
    /// Device-space origin: `ctm.transform(text_matrix.origin())`.
    pub origin: (f64, f64),
    /// Device-space glyph quad, four corners as `[x0,y0, x1,y1, x2,y2, x3,y3]`.
    pub quad: [f64; 8],
    pub source: GlyphSourceAddress,
    pub font_id: String,
    pub font_size: f64,
    pub text_matrix: Matrix,
    pub ctm: Matrix,
    pub writing_direction: WritingDirection,
    pub paint: PaintState,
}

/// Running text-object state for one page's content-stream scan: the CTM, text matrix/text-line
/// matrix, and the spacing/scale/leading/rise/render-mode parameters §5.2 requires tracking across
/// `q`/`Q`/`cm`; `BT`/`ET`; `Tm`/`Td`/`TD`/`T*`; `Tf`/`Tz`/`Tc`/`Tw`/`TL`/`Ts`/`Tr`.
#[derive(Clone, Debug)]
pub struct TextState {
    pub ctm: Matrix,
    pub text_matrix: Matrix,
    pub text_line_matrix: Matrix,
    pub font: Option<Rc<dyn TextStateFont>>,
    /// Current font size (`Tf`'s second operand).
    pub font_size: f64,
    /// Character spacing (`Tc`).
    pub char_spacing: f64,
    /// Word spacing (`Tw`), applied only to single-byte code 32 by the caller.
    pub word_spacing: f64,
    /// Horizontal scaling (`Tz`), stored as a fraction (`Tz` operand / 100); PDF default `1.0`.
    pub horizontal_scale: f64,
    /// Leading (`TL`), used by `T*`/`TD`.
    pub leading: f64,
    /// Text rise (`Ts`).
    pub text_rise: f64,
    pub render_mode: TextRenderMode,
}

impl TextState {
    pub fn new() -> TextState {
        TextState {
            ctm: Matrix::IDENTITY,
            text_matrix: Matrix::IDENTITY,
            text_line_matrix: Matrix::IDENTITY,
            font: None,
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 1.0,
            leading: 0.0,
            text_rise: 0.0,
            render_mode: TextRenderMode::Fill,
        }
    }

    /// `BT`: both text matrices reset to identity (PDF spec 9.4.1).
    pub fn begin_text(&mut self) {
        self.text_matrix = Matrix::IDENTITY;
        self.text_line_matrix = Matrix::IDENTITY;
    }

    /// `Tm a b c d e f`: both matrices are set directly.
    pub fn set_tm(&mut self, m: Matrix) {
        self.text_matrix = m;
        self.text_line_matrix = m;
    }

    /// `Td`/the translation half of `TD`: `Tlm = [1 0 0 1 tx ty] x Tlm_old`; `Tm = Tlm`.
    pub fn move_line(&mut self, tx: f64, ty: f64) {
        self.text_line_matrix = Matrix::translation(tx, ty).then(self.text_line_matrix);
        self.text_matrix = self.text_line_matrix;
    }

    /// `T*`: equivalent to `0 -TL Td`.
    pub fn next_line(&mut self) {
        self.move_line(0.0, -self.leading);
    }

    /// Device-space origin a `Tj`/`TJ` at the current text matrix would render at — where
    /// `pdf_inspect.rs`'s `visit_text_show` records each glyph's `origin`.
    pub fn origin(&self) -> (f64, f64) {
        self.ctm.transform(self.text_matrix.origin())
    }

    /// Device-space glyph quad for `glyph` shown at `text_matrix` under this state's `ctm`. Uses
    /// `glyph.bbox` when the font decoder supplied one; otherwise approximates an em-box from this
    /// state's font size and the font's advance width for `glyph.cid` — a placeholder good enough
    /// for construction/serialization tests, refined by `pdf_inspect.rs` (B1) once real font
    /// metrics (§7.2) are threaded through.
    pub fn glyph_quad(
        &self,
        glyph: &DecodedGlyph,
        text_matrix: Matrix,
    ) -> Result<[f64; 8], PdfEditError> {
        let font = self.font.as_ref().ok_or(PdfEditError::MalformedContent)?;
        let (x_min, y_min, x_max, y_max) = match glyph.bbox {
            Some([x0, y0, x1, y1]) => (
                x0 / 1000.0 * self.font_size,
                y0 / 1000.0 * self.font_size,
                x1 / 1000.0 * self.font_size,
                y1 / 1000.0 * self.font_size,
            ),
            None => {
                let advance = font.advance(glyph.cid, self.font_size);
                (0.0, self.font_size * -0.25, advance, self.font_size * 0.75)
            }
        };

        let corners = [
            (x_min, y_min),
            (x_max, y_min),
            (x_max, y_max),
            (x_min, y_max),
        ];
        let mut quad = [0.0; 8];
        for (i, (x, y)) in corners.iter().enumerate() {
            let scaled = (x * self.horizontal_scale, y + self.text_rise);
            let (tx, ty) = text_matrix.transform(scaled);
            let (dx, dy) = self.ctm.transform((tx, ty));
            quad[i * 2] = dx;
            quad[i * 2 + 1] = dy;
        }
        Ok(quad)
    }
}

impl Default for TextState {
    fn default() -> Self {
        TextState::new()
    }
}

/// Conservative axis-aligned device-space bounding box for path paint (`S`, `s`, `f`, `F`, `f*`,
/// `B`, `B*`, `b`, `b*`) or `XObject Do` placement, per §5.3's coarse painted-bounds inventory. `B1`
/// records these while walking; `unrelated` flags an entry as belonging to content other than the
/// island/run currently being evaluated, matching §8's `validate_layout` collision check
/// (`item.unrelated && item.bounds.intersects(run.bounds)`).
#[derive(Clone, Debug)]
// pdf_inspect.rs (B1) ended up defining its own equivalent local `PaintedBoundsEntry` for its
// walker's internal §8 collision bookkeeping rather than this frozen type — a lane deviation noted
// for follow-up, not corrected here to avoid a late cross-module rework. Kept for its documented
// §5.3/§8 role.
#[allow(dead_code)]
pub struct PaintedBounds {
    pub page: u32,
    pub bounds: PdfRect,
    pub unrelated: bool,
}

// ---------------------------------------------------------------------------------------------
// §6 — Frontend command mirror (Rust side) + composer types
// ---------------------------------------------------------------------------------------------

/// Rust mirror of frontend `PdfSourceEdit`/`PdfAddText`'s shared `{ text, appearance }` shape
/// (§6). Used for both `before`/`after` on a replace-source-text command.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PdfTextEditState {
    pub text: String,
    pub appearance: PdfTextAppearance,
}

/// Proof supplied by OCR/UI inspection that a line's background is safe to cover with an opaque
/// rectangle. Only near-uniform backgrounds are accepted by the sparse OCR correction composer.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PdfOcrBackgroundProof {
    NearUniform,
}

/// Rust mirror of frontend `PdfEditCommand` (§6: `PdfSourceEdit | PdfAddText`). `#[serde(tag =
/// "kind", rename_all = "camelCase")]` on a `PascalCase` variant name serializes/deserializes to
/// exactly the same `"replaceSourceText"`/`"addText"` discriminant the frontend sends, so the two
/// sides share one wire shape without a hand-written tag. This is the `commands: &[PdfEditCommand]`
/// parameter of the frozen `compose_candidate` signature (`PDF-EDITOR-DISPATCH.md`) and the
/// `source_commands` field of `prepare_pdf_save`/`compose_pdf_preview` requests (§9.4/§10.1).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
// Boxing the larger variant's fields would change this frozen wire type's construction/matching
// ergonomics for every consumer across dispatches — not worth it for a lint on a command list
// that is never long-lived or copied in bulk.
#[allow(clippy::large_enum_variant)]
pub enum PdfEditCommand {
    ReplaceSourceText {
        id: String,
        locator: PdfSourceLocator,
        before: PdfTextEditState,
        after: PdfTextEditState,
        available_bounds: PdfRect,
    },
    AddText {
        id: String,
        page: u32,
        anchor: PdfPoint,
        text: String,
        appearance: PdfTextAppearance,
    },
    #[serde(rename = "correctOcrText")]
    CorrectOcrLine {
        id: String,
        page: u32,
        line_id: String,
        rect: PdfRect,
        text: String,
        appearance: PdfTextAppearance,
        cover_color: PdfColor,
        background_proof: PdfOcrBackgroundProof,
    },
}

/// Returns commands that remain active after collapsing OCR corrections by line identity. The
/// last correction for a line wins; all existing command kinds retain their original order.
#[cfg(any(feature = "edit", test))]
pub(crate) fn effective_pdf_edit_commands(commands: &[PdfEditCommand]) -> Vec<&PdfEditCommand> {
    use std::collections::HashMap;

    let mut last_correction: HashMap<&str, usize> = HashMap::new();
    for (index, command) in commands.iter().enumerate() {
        if let PdfEditCommand::CorrectOcrLine { line_id, .. } = command {
            last_correction.insert(line_id.as_str(), index);
        }
    }

    commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| match command {
            PdfEditCommand::CorrectOcrLine { line_id, .. }
                if last_correction.get(line_id.as_str()) != Some(&index) =>
            {
                None
            }
            _ => Some(command),
        })
        .collect()
}

/// Result of composing a source-command batch against fresh source bytes — the frozen
/// `compose_candidate`'s success type (`PDF-EDITOR-DISPATCH.md`). `pdf_compose.rs` (lane C1)
/// produces this; `pdf_save.rs` (lane C2) and the `compose_pdf_preview` command (§9.4) consume it —
/// `bytes` becomes candidate/preview bytes, `affected_pages` becomes `PreparedPdfSave.affected_pages`
/// (§10.1) and the pages `compose_pdf_preview` tells the frontend to re-render.
#[derive(Clone, Debug)]
pub struct ComposedCandidate {
    pub bytes: Vec<u8>,
    pub affected_pages: Vec<u32>,
}

// ---------------------------------------------------------------------------------------------
// §7.2 — Font catalog API
// ---------------------------------------------------------------------------------------------

/// Where a catalog font's data comes from — bundled with RightKit (e.g. Source Serif 4) or
/// discovered on the host system via `fontdb` (§7.1).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PdfFontSource {
    Bundled,
    System,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfFontCatalogEntry {
    pub id: String,
    pub family: String,
    pub postscript_name: Option<String>,
    pub styles: Vec<String>,
    pub embeddable: bool,
    pub subsettable: bool,
    pub source: PdfFontSource,
}

/// A catalog font resolved to its actual font-program bytes, ready for glyph mapping (§7.3) and
/// subsetting/embedding (§7.4). `id` matches [`PdfFontCatalogEntry::id`] — the font ID, not the
/// family label, is what crosses the Save boundary (§7.2).
#[derive(Clone, Debug)]
pub struct ResolvedPdfFont {
    #[allow(dead_code)] // documented identity per §7.2; current callers key off `family` instead.
    pub id: String,
    pub family: String,
    /// Raw font-program bytes (TrueType/OpenType), suitable for `ttf_parser::Face::parse`.
    pub bytes: Vec<u8>,
    /// Face index within `bytes`, for font collections (`.ttc`); `0` for a single-face file.
    pub face_index: u32,
    #[allow(dead_code)] // callers re-derive this via `face_embedding_permissions` before use.
    pub embeddable: bool,
    #[allow(dead_code)]
    pub subsettable: bool,
}

/// One glyph identifier within a resolved font's own glyph index space, matching
/// `ttf_parser::GlyphId`'s representation (a 16-bit glyph index) so `pdf_font.rs` (lane B2) and
/// `pdf_compose.rs` (lane C1) can convert between the two without an extra newtype hop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct GlyphId(pub u16);

/// Resolves catalog font IDs to usable font data and maps text to glyphs (§7.2). Implemented by
/// `pdf_font.rs` (lane B2); consumed by the frozen `compose_candidate` signature
/// (`PDF-EDITOR-DISPATCH.md`) as `fonts: &dyn PdfFontResolver`, so `pdf_compose.rs` (lane C1) can
/// build and test against this trait before B2's real implementation lands.
pub trait PdfFontResolver {
    fn resolve(&self, id: &str) -> Result<ResolvedPdfFont, PdfEditError>;
    fn glyphs_for_text(
        &self,
        font: &ResolvedPdfFont,
        text: &str,
    ) -> Result<Vec<GlyphId>, PdfEditError>;
}

// ---------------------------------------------------------------------------------------------
// Digest helpers (§5.1: document_sha256 / expected_digest / island_digest)
// ---------------------------------------------------------------------------------------------

/// Lowercase-hex SHA-256 of `bytes`. Backs every digest helper below and is directly usable for
/// `PdfDocumentRevision::document_sha256`/`PdfSourceLocator::document_sha256`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `document_sha256`: identifies the exact backing bytes a [`PdfDocumentRevision`]/
/// [`PdfSourceLocator`] was computed against, so a command carrying a stale locator can be rejected
/// with [`PdfEditError::StaleSource`] before it touches content streams.
pub fn document_sha256(source_bytes: &[u8]) -> String {
    sha256_hex(source_bytes)
}

/// `expected_digest`: verifies one [`PdfSourceSpan`]'s operand text hasn't changed since the
/// locator was built, independent of the whole-document hash — lets a save distinguish "unrelated
/// edit elsewhere in the document" (safe) from "this exact operand changed" (reject with
/// [`PdfEditError::SourceDigestMismatch`]).
pub fn expected_text_digest(expected_text: &str) -> String {
    sha256_hex(expected_text.as_bytes())
}

/// `island_digest`: binds an island's ordered spans, decoded glyph sequence, font identity, active
/// matrices, paint, and geometry into one digest (§5.1) — a stronger identity check than
/// `expected_digest` alone, since two visibly-identical runs of text (e.g. repeated labels) can
/// still be told apart by their distinct source spans and transforms. Field order below is
/// significant and part of this function's contract: callers on either side of a save must hash the
/// same canonicalization to agree on a digest.
#[allow(clippy::too_many_arguments)]
pub fn compute_island_digest(
    spans: &[PdfSourceSpan],
    decoded_text: &str,
    font_id: &str,
    text_matrix: &Matrix,
    ctm: &Matrix,
    paint: &PaintState,
    quads: &[[f64; 8]],
) -> String {
    let mut buf = String::new();
    for span in spans {
        buf.push_str(&format!(
            "span:{}.{}/{}/{}/{}-{}|",
            span.stream_ref.0,
            span.stream_ref.1,
            span.contents_index,
            span.operator_index,
            span.operand_index,
            span.glyph_start
        ));
        buf.push_str(&format!("{}\n", span.glyph_end));
    }
    buf.push_str("text:");
    buf.push_str(decoded_text);
    buf.push_str("\nfont:");
    buf.push_str(font_id);
    buf.push_str(&format!(
        "\ntm:{},{},{},{},{},{}",
        text_matrix.a, text_matrix.b, text_matrix.c, text_matrix.d, text_matrix.e, text_matrix.f
    ));
    buf.push_str(&format!(
        "\nctm:{},{},{},{},{},{}",
        ctm.a, ctm.b, ctm.c, ctm.d, ctm.e, ctm.f
    ));
    buf.push_str(&format!(
        "\npaint:{:?}/{:?}/{:?}",
        paint.render_mode, paint.fill_color, paint.stroke_color
    ));
    for quad in quads {
        buf.push_str("\nquad:");
        for value in quad {
            buf.push_str(&format!("{value},"));
        }
    }
    sha256_hex(buf.as_bytes())
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_of_empty_input_is_64_hex_chars_and_stable() {
        // Don't hardcode the literal SHA-256("") digest here (easy to transcribe wrong by hand);
        // determinism + shape is what callers actually rely on, and is covered precisely.
        let a = sha256_hex(b"");
        let b = sha256_hex(b"");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn sha256_hex_is_deterministic_and_64_hex_chars() {
        let a = sha256_hex(b"viewright pdf editor");
        let b = sha256_hex(b"viewright pdf editor");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn sha256_hex_differs_for_different_input() {
        assert_ne!(
            sha256_hex(b"CAR YOU LIST."),
            sha256_hex(b"CAR YOU LIST, YOU WIN.")
        );
    }

    #[test]
    fn document_sha256_matches_sha256_hex() {
        let bytes = b"%PDF-1.7 fixture bytes";
        assert_eq!(document_sha256(bytes), sha256_hex(bytes));
    }

    #[test]
    fn expected_text_digest_is_stable_and_distinct() {
        let a = expected_text_digest("CAR YOU LIST.");
        let b = expected_text_digest("CAR YOU LIST.");
        let c = expected_text_digest("CAR YOU LIST, YOU WIN.");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    fn sample_span(operator_index: u32, glyph_start: u32, glyph_end: u32) -> PdfSourceSpan {
        PdfSourceSpan {
            stream_ref: (7, 0),
            contents_index: 0,
            operator_index,
            operand_index: 0,
            glyph_start,
            glyph_end,
            expected_text: "CAR".to_string(),
            expected_digest: expected_text_digest("CAR"),
        }
    }

    #[test]
    fn island_digest_is_stable_for_identical_inputs() {
        let spans = vec![sample_span(0, 0, 3)];
        let paint = PaintState {
            render_mode: TextRenderMode::Fill,
            fill_color: Some([0.0, 0.0, 0.0, 1.0]),
            stroke_color: None,
        };
        let tm = Matrix::IDENTITY;
        let ctm = Matrix::IDENTITY;
        let quads = vec![[0.0, 0.0, 10.0, 0.0, 10.0, 10.0, 0.0, 10.0]];

        let a = compute_island_digest(&spans, "CAR", "F1", &tm, &ctm, &paint, &quads);
        let b = compute_island_digest(&spans, "CAR", "F1", &tm, &ctm, &paint, &quads);
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn island_digest_distinguishes_repeated_text_by_span_position() {
        // Two visibly-identical runs of "CAR" at different operator indices must produce distinct
        // digests, matching §5.1's requirement that repeated visible text yield distinct locators.
        let paint = PaintState {
            render_mode: TextRenderMode::Fill,
            fill_color: None,
            stroke_color: None,
        };
        let tm = Matrix::IDENTITY;
        let ctm = Matrix::IDENTITY;
        let quads = vec![[0.0; 8]];

        let first = compute_island_digest(
            &[sample_span(0, 0, 3)],
            "CAR",
            "F1",
            &tm,
            &ctm,
            &paint,
            &quads,
        );
        let second = compute_island_digest(
            &[sample_span(5, 0, 3)],
            "CAR",
            "F1",
            &tm,
            &ctm,
            &paint,
            &quads,
        );
        assert_ne!(first, second);
    }

    #[test]
    fn island_digest_distinguishes_by_matrix() {
        let spans = vec![sample_span(0, 0, 3)];
        let paint = PaintState {
            render_mode: TextRenderMode::Fill,
            fill_color: None,
            stroke_color: None,
        };
        let ctm = Matrix::IDENTITY;
        let quads = vec![[0.0; 8]];

        let a = compute_island_digest(&spans, "CAR", "F1", &Matrix::IDENTITY, &ctm, &paint, &quads);
        let b = compute_island_digest(
            &spans,
            "CAR",
            "F1",
            &Matrix::translation(12.0, 0.0),
            &ctm,
            &paint,
            &quads,
        );
        assert_ne!(a, b);
    }

    #[test]
    fn matrix_identity_transform_is_noop() {
        let (x, y) = Matrix::IDENTITY.transform((3.0, 4.0));
        assert_eq!((x, y), (3.0, 4.0));
    }

    #[test]
    fn matrix_translation_then_composes_in_order() {
        let m = Matrix::translation(10.0, 0.0).then(Matrix::translation(0.0, 5.0));
        assert_eq!(m.transform((0.0, 0.0)), (10.0, 5.0));
    }

    #[test]
    fn text_state_move_line_updates_origin() {
        let mut state = TextState::new();
        state.begin_text();
        state.move_line(12.0, -20.0);
        assert_eq!(state.text_matrix.origin(), (12.0, -20.0));
        assert_eq!(state.origin(), (12.0, -20.0));
    }

    #[test]
    fn text_state_next_line_uses_leading() {
        let mut state = TextState::new();
        state.leading = 14.0;
        state.next_line();
        assert_eq!(state.text_matrix.origin(), (0.0, -14.0));
    }

    #[test]
    fn pdf_rect_contains_and_intersects() {
        let outer = PdfRect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 50.0,
        };
        let inner = PdfRect {
            x: 10.0,
            y: 10.0,
            width: 20.0,
            height: 10.0,
        };
        let outside = PdfRect {
            x: 200.0,
            y: 200.0,
            width: 10.0,
            height: 10.0,
        };
        assert!(outer.contains(inner));
        assert!(!outer.contains(outside));
        assert!(outer.intersects(inner));
        assert!(!outer.intersects(outside));
    }

    #[test]
    fn text_render_mode_from_operand_covers_spec_range() {
        assert_eq!(TextRenderMode::from_operand(0), TextRenderMode::Fill);
        assert_eq!(TextRenderMode::from_operand(1), TextRenderMode::Stroke);
        assert_eq!(TextRenderMode::from_operand(2), TextRenderMode::FillStroke);
        assert_eq!(TextRenderMode::from_operand(7), TextRenderMode::Clip);
        assert_eq!(TextRenderMode::from_operand(99), TextRenderMode::Invisible);
    }

    #[test]
    fn pdf_source_locator_round_trips_through_json() {
        let locator = PdfSourceLocator {
            document_sha256: document_sha256(b"fixture"),
            page: 1,
            spans: vec![sample_span(0, 0, 3)],
            island_digest: sha256_hex(b"island"),
        };
        let json = serde_json::to_string(&locator).expect("serialize");
        assert!(json.contains("\"documentSha256\""));
        assert!(json.contains("\"islandDigest\""));
        let round_tripped: PdfSourceLocator = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(round_tripped.page, locator.page);
        assert_eq!(round_tripped.spans.len(), locator.spans.len());
        assert_eq!(round_tripped.island_digest, locator.island_digest);
    }

    fn sample_appearance() -> PdfTextAppearance {
        PdfTextAppearance {
            font_id: "source-serif-4".to_string(),
            family: "Source Serif 4".to_string(),
            size_pt: 34.0,
            color: PdfColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                alpha: 1.0,
            },
        }
    }

    #[test]
    fn pdf_edit_command_replace_source_text_round_trips() {
        let command = PdfEditCommand::ReplaceSourceText {
            id: "edit-1".to_string(),
            locator: PdfSourceLocator {
                document_sha256: document_sha256(b"fixture"),
                page: 1,
                spans: vec![sample_span(0, 0, 3)],
                island_digest: sha256_hex(b"island"),
            },
            before: PdfTextEditState {
                text: "CAR YOU LIST.".to_string(),
                appearance: sample_appearance(),
            },
            after: PdfTextEditState {
                text: "CAR YOU LIST, YOU WIN.".to_string(),
                appearance: sample_appearance(),
            },
            available_bounds: PdfRect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 40.0,
            },
        };

        let json = serde_json::to_string(&command).expect("serialize");
        assert!(json.contains("\"kind\":\"replaceSourceText\""));
        assert!(json.contains("\"availableBounds\""));

        let round_tripped: PdfEditCommand = serde_json::from_str(&json).expect("deserialize");
        match round_tripped {
            PdfEditCommand::ReplaceSourceText { id, after, .. } => {
                assert_eq!(id, "edit-1");
                assert_eq!(after.text, "CAR YOU LIST, YOU WIN.");
            }
            PdfEditCommand::AddText { .. } => panic!("expected ReplaceSourceText"),
            PdfEditCommand::CorrectOcrLine { .. } => panic!("expected ReplaceSourceText"),
        }
    }

    #[test]
    fn pdf_edit_command_add_text_round_trips() {
        let command = PdfEditCommand::AddText {
            id: "add-1".to_string(),
            page: 2,
            anchor: PdfPoint { x: 72.0, y: 144.0 },
            text: "New line".to_string(),
            appearance: sample_appearance(),
        };

        let json = serde_json::to_string(&command).expect("serialize");
        assert!(json.contains("\"kind\":\"addText\""));

        let round_tripped: PdfEditCommand = serde_json::from_str(&json).expect("deserialize");
        match round_tripped {
            PdfEditCommand::AddText { id, page, text, .. } => {
                assert_eq!(id, "add-1");
                assert_eq!(page, 2);
                assert_eq!(text, "New line");
            }
            PdfEditCommand::ReplaceSourceText { .. } => panic!("expected AddText"),
            PdfEditCommand::CorrectOcrLine { .. } => panic!("expected AddText"),
        }
    }

    #[test]
    fn pdf_edit_command_correct_ocr_text_round_trips() {
        let command = PdfEditCommand::CorrectOcrLine {
            id: "ocr-1".to_string(),
            page: 1,
            line_id: "line-7".to_string(),
            rect: PdfRect {
                x: 72.0,
                y: 700.0,
                width: 120.0,
                height: 18.0,
            },
            text: "Corrected".to_string(),
            appearance: sample_appearance(),
            cover_color: PdfColor {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                alpha: 1.0,
            },
            background_proof: PdfOcrBackgroundProof::NearUniform,
        };
        let json = serde_json::to_string(&command).expect("serialize");
        assert!(json.contains("\"kind\":\"correctOcrText\""));
        assert!(json.contains("\"lineId\":\"line-7\""));
        assert!(json.contains("\"backgroundProof\":\"nearUniform\""));
        let round_tripped: PdfEditCommand = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(
            round_tripped,
            PdfEditCommand::CorrectOcrLine {
                background_proof: PdfOcrBackgroundProof::NearUniform,
                ..
            }
        ));
        let invalid = json.replace("nearUniform", "gradient");
        assert!(serde_json::from_str::<PdfEditCommand>(&invalid).is_err());
    }

    #[test]
    fn effective_commands_last_ocr_correction_wins() {
        let appearance = sample_appearance();
        let correction = |id: &str, text: &str| PdfEditCommand::CorrectOcrLine {
            id: id.to_string(),
            page: 1,
            line_id: "line-1".to_string(),
            rect: PdfRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 20.0,
            },
            text: text.to_string(),
            appearance: appearance.clone(),
            cover_color: PdfColor {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                alpha: 1.0,
            },
            background_proof: PdfOcrBackgroundProof::NearUniform,
        };
        let commands = vec![correction("old", "old"), correction("new", "new")];
        let active = effective_pdf_edit_commands(&commands);
        assert_eq!(active.len(), 1);
        assert!(matches!(active[0], PdfEditCommand::CorrectOcrLine { text, .. } if text == "new"));
    }

    #[test]
    fn pdf_font_source_round_trips() {
        for (source, wire) in [
            (PdfFontSource::Bundled, "\"bundled\""),
            (PdfFontSource::System, "\"system\""),
        ] {
            let json = serde_json::to_string(&source).expect("serialize");
            assert_eq!(json, wire, "frontend expects a string source discriminator");
            let round_tripped: PdfFontSource = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(round_tripped, source);
        }
    }

    #[test]
    fn pdf_font_catalog_entry_serializes_camel_case() {
        let entry = PdfFontCatalogEntry {
            id: "source-serif-4".to_string(),
            family: "Source Serif 4".to_string(),
            postscript_name: Some("SourceSerif4-Regular".to_string()),
            styles: vec!["Regular".to_string(), "Bold".to_string()],
            embeddable: true,
            subsettable: true,
            source: PdfFontSource::Bundled,
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        assert!(json.contains("\"postscriptName\""));
        assert!(json.contains("\"subsettable\""));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap()["source"],
            "bundled"
        );
    }

    #[test]
    fn pdf_edit_error_serializes_as_code_message_envelope() {
        // Matches the host error type's existing `{ code, message }` wire convention (error.rs) and
        // `pdfEditModel.ts`'s `PdfEditErrorCode` union (A2) — not a bare variant-name string.
        let json = serde_json::to_string(&PdfEditError::StaleSource).expect("serialize");
        assert_eq!(
            json,
            r#"{"code":"stale_source","message":"PDF changed since edit started"}"#
        );
        assert_eq!(
            PdfEditError::TextOverflow.to_string(),
            "text exceeds safe bounds"
        );
        assert_eq!(PdfEditError::TextOverflow.code(), "text_overflow");
    }

    /// A minimal `TextStateFont` used only to exercise `glyph_quad`'s fallback (bbox-less) path.
    #[derive(Debug)]
    struct FixedAdvanceFont {
        advance: f64,
    }

    impl TextStateFont for FixedAdvanceFont {
        fn id(&self) -> &str {
            "fixed"
        }

        fn decode_show_operand(
            &self,
            _operand: &lopdf::Object,
        ) -> Result<Vec<DecodedGlyph>, PdfEditError> {
            Ok(Vec::new())
        }

        fn advance(&self, _cid: u32, _font_size: f64) -> f64 {
            self.advance
        }
    }

    #[test]
    fn glyph_quad_without_bbox_uses_advance_fallback() {
        let mut state = TextState::new();
        state.font = Some(Rc::new(FixedAdvanceFont { advance: 10.0 }));
        state.font_size = 12.0;

        let glyph = DecodedGlyph {
            cid: 1,
            unicode: "A".to_string(),
            source_range: (0, 1),
            bbox: None,
        };

        let quad = state
            .glyph_quad(&glyph, Matrix::IDENTITY)
            .expect("font is set");
        // Bottom-left corner should sit at the origin (identity matrices, no rise/scale).
        assert_eq!((quad[0], quad[1]), (0.0, state.font_size * -0.25));
        // Bottom-right corner should sit at the fallback advance width.
        assert_eq!((quad[2], quad[3]), (10.0, state.font_size * -0.25));
    }

    #[test]
    fn glyph_quad_without_font_is_malformed_content() {
        let state = TextState::new();
        let glyph = DecodedGlyph {
            cid: 1,
            unicode: "A".to_string(),
            source_range: (0, 1),
            bbox: None,
        };
        let err = state
            .glyph_quad(&glyph, Matrix::IDENTITY)
            .expect_err("no font set");
        assert!(matches!(err, PdfEditError::MalformedContent));
    }

    #[test]
    fn source_address_with_glyph_narrows_correctly() {
        let address = SourceAddress {
            stream_ref: (7, 0),
            contents_index: 1,
            operator_index: 2,
            operand_index: 0,
        };
        let glyph_address = address.with_glyph((3, 6));
        assert_eq!(glyph_address.stream_ref, (7, 0));
        assert_eq!(glyph_address.contents_index, 1);
        assert_eq!(glyph_address.operator_index, 2);
        assert_eq!(glyph_address.glyph_start, 3);
        assert_eq!(glyph_address.glyph_end, 6);
    }
}
