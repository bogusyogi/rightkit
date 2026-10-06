//! PDF editor sparse composition (Dispatch C, lane C1).
//!
//! Owns `docs/PDF-EDITOR-NOW.md` §8 (layout validation), §9.1-9.3 (the three sparse write
//! strategies), and §9.4 (the `compose_pdf_preview` Tauri command). Implements the frozen
//! cross-lane seam (`docs/PDF-EDITOR-DISPATCH.md`):
//!
//! ```ignore
//! pub fn compose_candidate(
//!     source: &[u8],
//!     commands: &[PdfEditCommand],
//!     fonts: &dyn PdfFontResolver,
//! ) -> Result<ComposedCandidate, PdfEditError>;
//! ```
//!
//! `compose_candidate` validates every command's locator/digest against fresh `source` bytes (via
//! `pdf_inspect.rs`'s `walk_page_graph`, lane B1), validates layout (§8), then applies one of the
//! three sparse strategies (§9.1-9.3) per command before returning composed candidate bytes plus
//! the pages a caller must re-render. `compose_pdf_preview` (§9.4) is the memory-only Tauri command
//! wrapping this function for committed-unsaved-edit preview re-render; `prepare_pdf_save` (§10.1,
//! `pdf_save.rs`, lane C2) is expected to call `compose_candidate` too, but owns its own save/token
//! plumbing and is out of scope here.
//!
//! Wire/shared types (`PdfEditCommand`, `PdfEditError`, `ComposedCandidate`, `PdfFontResolver`,
//! `PdfSourceLocator`, `PdfSourceSpan`, `PdfRect`, `PdfPoint`, `PdfTextAppearance`,
//! `PdfTextEditState`, `ResolvedPdfFont`, `GlyphId`) live in `pdf_model.rs` (lane A1); page-graph
//! re-inspection comes from `pdf_inspect.rs` (lane B1); font subsetting/embedding helpers
//! (`subset_for_pdf`, `build_to_unicode_cmap`, `FontCatalog`) come from `pdf_font.rs` (lane B2).
//! This file only implements composition behavior against those.
//!
//! ## Deliberate scope/judgment calls (not fully pinned by §8/§9)
//!
//! - **Strategy selection.** §9's `select_strategy` sketch references `same_font_encoding`,
//!   `same_geometry`, `one_operand`, `locator_is_complete`, `local_rewrite_is_safe` as booleans
//!   without defining them precisely. This implementation resolves them concretely: a
//!   `ReplaceSourceText` command takes [`WriteStrategy::ExactOperandPatch`] only when (a) `before`
//!   and `after` appearance are byte-for-byte identical (same font/size/color — "same font
//!   encoding"), (b) the replacement text is pure ASCII (matches `pdf_text_edit.rs`'s existing
//!   "simple-font glyph code equals ASCII byte" convention — the closest thing this codebase has to
//!   "same geometry, reusable encoding" for a source font this composer never decodes), and (c) the
//!   locator carries exactly one span (`one_operand`). Every other geometrically-valid, digest-
//!   verified `ReplaceSourceText` command falls through to [`WriteStrategy::LocalTextObjectRewrite`].
//!   `AddText` always uses [`WriteStrategy::AppendTextStream`].
//! - **Font reuse vs. embed.** [`PdfFontResolver`] (§7.2) resolves *catalog* fonts only — it has no
//!   notion of "the font already used at this source location," and `pdf_inspect.rs`'s
//!   content-stream font decoder (`TextStateFont`) is a different, page-local abstraction this
//!   composer never receives. So "reuse source PDF font" (§7.3 rule 1) is only achievable for the
//!   `ExactOperandPatch` path, where the original operand bytes/font resource are never touched at
//!   all — the safest possible form of reuse. Every `LocalTextObjectRewrite`/`AppendTextStream`
//!   resolves `after.appearance.font_id` through `fonts` and embeds a fresh Type0/CIDFontType2
//!   subset (§7.4), even when a caller happens to pass the same font family as the original run.
//! - **Local-rewrite insertion point.** §9.2 says "insert ... at deterministic source boundary"
//!   without specifying which. This implementation always appends the new `q/BT/.../ET/Q` block at
//!   the END of the touched content stream's operator list, never in the middle. This is
//!   deterministic and — critically — never renumbers any other operator's `operator_index`, so a
//!   second, independent command's locator (captured against the same pristine source, per §9.4's
//!   "Applies staged source commands to fresh source bytes") stays valid even when a prior command
//!   in the same batch already rewrote a different run in the same content stream. Removed source
//!   glyph bytes are blanked in place (byte range deleted from the operand string) rather than
//!   deleting the whole operator, for the same reason: it never shifts a sibling span's
//!   `operator_index`/`operand_index`.
//! - **Baseline anchor for a rewritten run.** §8 says "Replacement preserves original baseline
//!   anchor," but `PdfSourceEdit`'s wire shape (`pdfEditModel.ts`/`PdfEditCommand`, §6) carries no
//!   explicit origin point — only `availableBounds: PdfRect`. This implementation anchors the new
//!   run's text matrix at `(available_bounds.x, available_bounds.y)`, i.e. the lower-left corner of
//!   the verified-available rect, which is the frontend-computed stand-in for the original run's
//!   baseline start per §8's "Available expansion beyond original island requires verified empty
//!   page space on same baseline."
//! - **Layout measurement.** `MeasuredRun`'s width for an embedded-font rewrite/add-text run is
//!   measured exactly, by summing the resolved font's real per-glyph advances (via `ttf_parser`,
//!   same source `subset_for_pdf` reads) scaled to `/1000` units. For the ASCII exact-operand-patch
//!   path, no font metrics are available at all through this composer's `PdfFontResolver` seam
//!   (§7.2 only resolves catalog fonts, not the page's already-embedded source font), so that path
//!   uses a conservative `chars * size_pt * 0.5` em-width estimate — the standard rough average
//!   glyph width — before falling back to a real check on save/render (`verifyPdfCandidate`, §10.2,
//!   is the actual ground truth downstream).
//! - **`compose_pdf_preview`'s "affected pages as metadata."** Candidate bytes return as a JSON
//!   byte array because WKWebView fails while decoding Tauri's raw-response stream. Frontend derives
//!   affected pages from request commands, so no second metadata channel is required.
use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream, StringFormat};
use serde::Deserialize;

use crate::font::{build_to_unicode_cmap, subset_for_pdf};
use crate::inspect::{simple_font_identity_bytes, walk_page_graph};
use crate::model::{
    document_sha256, effective_pdf_edit_commands, ComposedCandidate, PdfColor, PdfEditCommand,
    PdfEditError, PdfFontResolver, PdfOcrBackgroundProof, PdfPoint, PdfRect, PdfSourceLocator,
    PdfSourceSpan, PdfTextAppearance, PdfTextEditState, ResolvedPdfFont,
};

// -------------------------------------------------------------------------------------------
// §9 — Strategy selection
// -------------------------------------------------------------------------------------------

/// §9's `select_strategy` outcome, concretized per this module's judgment calls (see module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteStrategy {
    ExactOperandPatch,
    LocalTextObjectRewrite,
}

fn select_replace_strategy(
    locator: &PdfSourceLocator,
    before: &PdfTextEditState,
    after: &PdfTextEditState,
) -> WriteStrategy {
    let same_font_encoding = before.appearance == after.appearance;
    let one_operand = locator.spans.len() == 1;
    let ascii_only = after.text.is_ascii();

    if same_font_encoding && one_operand && ascii_only {
        WriteStrategy::ExactOperandPatch
    } else {
        WriteStrategy::LocalTextObjectRewrite
    }
}

// -------------------------------------------------------------------------------------------
// Frozen seam: compose_candidate
// -------------------------------------------------------------------------------------------

/// The frozen cross-lane seam (`docs/PDF-EDITOR-DISPATCH.md`). Applies `commands` to `source`,
/// validating every locator/digest against fresh bytes first (§9's "reject if stale"), then
/// dispatching each command to its selected sparse-write strategy (§9.1-9.3).
pub fn compose_candidate(
    source: &[u8],
    commands: &[PdfEditCommand],
    fonts: &dyn PdfFontResolver,
) -> Result<ComposedCandidate, PdfEditError> {
    let current_sha = document_sha256(source);
    compose_candidate_on_base(&current_sha, source, commands, fonts)
}

/// Same as [`compose_candidate`] but loads the lopdf document from `base` bytes while treating
/// `source_sha` as the "current" source digest for every locator/staleness check — used by
/// `prepare_pdf_save` (lane C2) to compose sparse-write commands on top of pdf.js's
/// `saveDocument()` incremental-update annotation bytes (verbatim original source bytes plus
/// appended annotation objects, per pdf.js's own incremental-save format) while still validating
/// locators against the *original* source's digest rather than the annotated bytes' digest.
pub fn compose_candidate_on_base(
    source_sha: &str,
    base: &[u8],
    commands: &[PdfEditCommand],
    fonts: &dyn PdfFontResolver,
) -> Result<ComposedCandidate, PdfEditError> {
    if commands.is_empty() {
        return Ok(ComposedCandidate {
            bytes: base.to_vec(),
            affected_pages: Vec::new(),
        });
    }

    let mut doc = Document::load_mem(base).map_err(|_| PdfEditError::MalformedContent)?;
    if doc.is_encrypted() {
        return Err(PdfEditError::EncryptedDocument);
    }

    let mut affected_pages: Vec<u32> = Vec::new();

    for command in effective_pdf_edit_commands(commands) {
        let page = match command {
            PdfEditCommand::ReplaceSourceText {
                locator,
                before,
                after,
                available_bounds,
                ..
            } => {
                if locator.document_sha256 != source_sha {
                    return Err(PdfEditError::StaleSource);
                }
                apply_replace_source_text(
                    &mut doc,
                    source_sha,
                    locator,
                    before,
                    after,
                    *available_bounds,
                    fonts,
                )?
            }
            PdfEditCommand::AddText {
                page,
                anchor,
                text,
                appearance,
                ..
            } => apply_add_text(&mut doc, *page, *anchor, text, appearance, fonts)?,
            PdfEditCommand::CorrectOcrLine {
                page,
                rect,
                text,
                appearance,
                cover_color,
                background_proof,
                ..
            } => apply_correct_ocr_text(
                &mut doc,
                *page,
                *rect,
                text,
                appearance,
                *cover_color,
                *background_proof,
                fonts,
            )?,
        };
        if !affected_pages.contains(&page) {
            affected_pages.push(page);
        }
    }

    doc.compress();
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)
        .map_err(|_| PdfEditError::MalformedContent)?;

    affected_pages.sort_unstable();
    Ok(ComposedCandidate {
        bytes,
        affected_pages,
    })
}

// -------------------------------------------------------------------------------------------
// §9.1/§9.2 — ReplaceSourceText
// -------------------------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn apply_replace_source_text(
    doc: &mut Document,
    current_sha: &str,
    locator: &PdfSourceLocator,
    before: &PdfTextEditState,
    after: &PdfTextEditState,
    available_bounds: PdfRect,
    fonts: &dyn PdfFontResolver,
) -> Result<u32, PdfEditError> {
    if locator.spans.is_empty() {
        return Err(PdfEditError::UnsupportedEncoding);
    }

    // Re-inspect the page fresh (never trust the caller's cached graph) and require an island
    // whose digest still matches exactly — this is what actually catches "same visible text but
    // changed operand digest" once `document_sha256` alone has already passed (§5.1).
    let graph = walk_page_graph(doc, current_sha, locator.page)?;
    let island = graph
        .islands
        .iter()
        .find(|isl| isl.locator.island_digest == locator.island_digest)
        .ok_or(PdfEditError::SourceDigestMismatch)?;
    if !island.editable {
        return Err(PdfEditError::UnsupportedEncoding);
    }
    for span in &locator.spans {
        let matches = island.locator.spans.iter().any(|current| {
            current.stream_ref == span.stream_ref
                && current.contents_index == span.contents_index
                && current.operator_index == span.operator_index
                && current.operand_index == span.operand_index
                && current.glyph_start == span.glyph_start
                && current.glyph_end == span.glyph_end
                && current.expected_digest == span.expected_digest
        });
        if !matches {
            return Err(PdfEditError::SourceDigestMismatch);
        }
    }

    // This composer's sparse strategies only ever touch one content stream per command — every
    // span must agree on which one.
    let stream_ref = locator.spans[0].stream_ref;
    let contents_index = locator.spans[0].contents_index;
    if !locator
        .spans
        .iter()
        .all(|s| s.stream_ref == stream_ref && s.contents_index == contents_index)
    {
        return Err(PdfEditError::UnsupportedEncoding);
    }
    let stream_id: ObjectId = (stream_ref.0, stream_ref.1);
    let page_id = pages_lookup(doc, locator.page)?;

    match select_replace_strategy(locator, before, after) {
        WriteStrategy::ExactOperandPatch => {
            apply_exact_operand_patch(
                doc,
                page_id,
                stream_id,
                &locator.spans[0],
                after,
                available_bounds,
            )?;
        }
        WriteStrategy::LocalTextObjectRewrite => {
            apply_local_text_object_rewrite(
                doc,
                page_id,
                stream_id,
                &locator.spans,
                after,
                available_bounds,
                fonts,
            )?;
        }
    }

    Ok(locator.page)
}

fn pages_lookup(doc: &Document, page: u32) -> Result<ObjectId, PdfEditError> {
    doc.get_pages()
        .get(&page)
        .copied()
        .ok_or(PdfEditError::MalformedContent)
}

// -------------------------------------------------------------------------------------------
// §9.1 — Exact operand patch
// -------------------------------------------------------------------------------------------

fn apply_exact_operand_patch(
    doc: &mut Document,
    page_id: ObjectId,
    stream_id: ObjectId,
    span: &PdfSourceSpan,
    after: &PdfTextEditState,
    available_bounds: PdfRect,
) -> Result<(), PdfEditError> {
    validate_layout_conservative(&after.text, after.appearance.size_pt, available_bounds)?;

    let content = decode_stream_content(doc, stream_id)?;
    let resource_name = active_font_resource_name(&content, span.operator_index)
        .ok_or(PdfEditError::UnsupportedEncoding)?;
    // `after.text.is_ascii()` (§9's strategy selection) only says the Rust string is ASCII-shaped
    // — it says nothing about whether this specific font's byte encoding maps those characters to
    // the same byte values. Many embedded subset fonts remap codes via `/Differences`, where
    // writing raw ASCII bytes directly silently produces wrong or blank glyphs instead of the
    // requested text. Verify against the font's own encoding and fail closed (no fallback exists
    // for a same-font edit whose font_id isn't a resolvable catalog font — see module doc) rather
    // than ever writing an unverified byte sequence.
    let replacement = simple_font_identity_bytes(doc, page_id, &resource_name, &after.text)
        .ok_or(PdfEditError::UnsupportedEncoding)?;

    let mut content = content;
    splice_operand_bytes(
        &mut content,
        span.operator_index,
        span.operand_index,
        span.glyph_start,
        span.glyph_end,
        &replacement,
    )?;
    write_stream_content(doc, stream_id, &content)
}

/// Best-effort resolution of the `Tf` resource name governing `operator_index` — the most recent
/// `Tf` operator at or before it in the stream's linear operator order. Matches this module's
/// existing simplification (the walker's own `TextState.font` tracking is likewise a linear scan,
/// not `q`/`Q`-scoped): good enough for the vast majority of real content streams, and a wrong
/// guess here fails closed via [`simple_font_identity_bytes`]'s reverse-encoding check rather than
/// producing wrong output.
fn active_font_resource_name(content: &Content, operator_index: u32) -> Option<Vec<u8>> {
    let mut name: Option<Vec<u8>> = None;
    for op in content.operations.iter().take(operator_index as usize + 1) {
        if op.operator == "Tf" {
            if let Some(Object::Name(n)) = op.operands.first() {
                name = Some(n.clone());
            }
        }
    }
    name
}

/// Conservative overflow check for the ASCII exact-patch path, where no real font metrics are
/// reachable (see module doc's "Layout measurement" note): average glyph width ≈ `0.5 * size_pt`.
fn validate_layout_conservative(
    text: &str,
    size_pt: f64,
    available: PdfRect,
) -> Result<(), PdfEditError> {
    let width = text.chars().count() as f64 * size_pt * 0.5;
    let run_bounds = PdfRect {
        x: available.x,
        y: available.y,
        width,
        height: available.height,
    };
    if !available.contains(run_bounds) {
        return Err(PdfEditError::TextOverflow);
    }
    Ok(())
}

// -------------------------------------------------------------------------------------------
// §9.2 — Local text-object rewrite
// -------------------------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn apply_local_text_object_rewrite(
    doc: &mut Document,
    page_id: ObjectId,
    stream_id: ObjectId,
    spans: &[PdfSourceSpan],
    after: &PdfTextEditState,
    available_bounds: PdfRect,
    fonts: &dyn PdfFontResolver,
) -> Result<(), PdfEditError> {
    let resolved = fonts.resolve(&after.appearance.font_id)?;
    // Validates full glyph coverage up front (§7.3 rule 3: "fail before candidate creation").
    fonts.glyphs_for_text(&resolved, &after.text)?;

    let anchor = PdfPoint {
        x: available_bounds.x,
        y: available_bounds.y,
    };
    let advance_1000 = measure_advance_1000(&resolved, &after.text);
    validate_layout_measured(
        advance_1000,
        after.appearance.size_pt,
        anchor,
        available_bounds,
    )?;

    let (font_obj_id, cid_codes) = embed_type0_font(doc, &resolved, &after.text)?;
    let resource_name = add_font_resource(doc, page_id, font_obj_id)?;

    let mut content = decode_stream_content(doc, stream_id)?;

    // Blank covered glyph bytes from the end backward so earlier spans' byte offsets (captured
    // against the pristine operand) stay valid while later ones are removed.
    let mut ordered: Vec<&PdfSourceSpan> = spans.iter().collect();
    ordered.sort_by(|a, b| {
        b.operator_index
            .cmp(&a.operator_index)
            .then(b.operand_index.cmp(&a.operand_index))
            .then(b.glyph_start.cmp(&a.glyph_start))
    });
    for span in ordered {
        splice_operand_bytes(
            &mut content,
            span.operator_index,
            span.operand_index,
            span.glyph_start,
            span.glyph_end,
            &[],
        )?;
    }

    // §9.2 step 4/5: append the new run at a deterministic boundary (this module's own choice:
    // the end of the stream's operator list — see module doc) rather than in the middle.
    content.operations.extend(build_text_object_operations(
        &resource_name,
        &after.appearance,
        anchor,
        &cid_codes,
    ));

    write_stream_content(doc, stream_id, &content)
}

fn validate_layout_measured(
    advance_1000: f64,
    size_pt: f64,
    anchor: PdfPoint,
    available: PdfRect,
) -> Result<(), PdfEditError> {
    // Anchored at `available`'s own lower-left corner (§8: "Available expansion beyond original
    // island requires verified empty page space on same baseline" — `available_bounds` IS that
    // verified, baseline-anchored usable rect already, per `validate_layout_conservative`'s
    // matching convention). No extra guessed descender offset below `anchor.y`/`available.y`: that
    // previously pushed `run_bounds` below `available`'s own bottom edge and produced a false
    // `TextOverflow` even for text that fits comfortably.
    let width = (advance_1000 / 1000.0 * size_pt).max(0.0);
    let run_bounds = PdfRect {
        x: anchor.x,
        y: available.y,
        width,
        height: available.height,
    };
    if !available.contains(run_bounds) {
        return Err(PdfEditError::TextOverflow);
    }
    Ok(())
}

fn measure_advance_1000(resolved: &ResolvedPdfFont, text: &str) -> f64 {
    let Ok(face) = ttf_parser::Face::parse(&resolved.bytes, resolved.face_index) else {
        return text.chars().count() as f64 * 500.0;
    };
    let units_per_em = face.units_per_em() as f64;
    if units_per_em <= 0.0 {
        return text.chars().count() as f64 * 500.0;
    }
    text.chars()
        .filter_map(|c| face.glyph_index(c))
        .map(|gid| face.glyph_hor_advance(gid).unwrap_or(0) as f64 * 1000.0 / units_per_em)
        .sum()
}

// -------------------------------------------------------------------------------------------
// §9.3 — Add Text
// -------------------------------------------------------------------------------------------

fn apply_add_text(
    doc: &mut Document,
    page: u32,
    anchor: PdfPoint,
    text: &str,
    appearance: &PdfTextAppearance,
    fonts: &dyn PdfFontResolver,
) -> Result<u32, PdfEditError> {
    if doc.is_encrypted() {
        return Err(PdfEditError::EncryptedDocument);
    }
    let page_id = pages_lookup(doc, page)?;

    let resolved = fonts.resolve(&appearance.font_id)?;
    fonts.glyphs_for_text(&resolved, text)?;

    let (font_obj_id, cid_codes) = embed_type0_font(doc, &resolved, text)?;
    let resource_name = add_font_resource(doc, page_id, font_obj_id)?;

    let mut content = Content::decode(&[]).map_err(|_| PdfEditError::MalformedContent)?;
    content.operations =
        build_text_object_operations(&resource_name, appearance, anchor, &cid_codes);
    let encoded = content
        .encode()
        .map_err(|_| PdfEditError::MalformedContent)?;
    let new_stream_id = doc.add_object(Stream::new(dictionary! {}, encoded));

    append_content_stream(doc, page_id, new_stream_id)?;

    Ok(page)
}

/// Covers one OCR line's proven near-uniform background, then appends a fresh Type0 text object.
/// Existing page content streams and source operators remain untouched.
#[allow(clippy::too_many_arguments)]
fn apply_correct_ocr_text(
    doc: &mut Document,
    page: u32,
    rect: PdfRect,
    text: &str,
    appearance: &PdfTextAppearance,
    cover_color: PdfColor,
    background_proof: PdfOcrBackgroundProof,
    fonts: &dyn PdfFontResolver,
) -> Result<u32, PdfEditError> {
    let page_id = pages_lookup(doc, page)?;
    let page_bounds = page_bounds(doc, page_id)?;
    validate_ocr_correction(
        page_bounds,
        rect,
        text,
        appearance,
        cover_color,
        background_proof,
        page_rotation(doc, page_id)?,
    )?;

    let resolved = fonts.resolve(&appearance.font_id)?;
    fonts.glyphs_for_text(&resolved, text)?;
    let width = measure_advance_1000(&resolved, text) / 1000.0 * appearance.size_pt;
    if width > rect.width || appearance.size_pt > rect.height {
        return Err(PdfEditError::TextOverflow);
    }
    let (font_obj_id, cid_codes) = embed_type0_font(doc, &resolved, text)?;
    let resource_name = add_font_resource(doc, page_id, font_obj_id)?;

    let mut content = Content::decode(&[]).map_err(|_| PdfEditError::MalformedContent)?;
    content.operations =
        build_ocr_correction_operations(&resource_name, appearance, rect, cover_color, &cid_codes);
    let encoded = content
        .encode()
        .map_err(|_| PdfEditError::MalformedContent)?;
    let new_stream_id = doc.add_object(Stream::new(dictionary! {}, encoded));
    append_content_stream(doc, page_id, new_stream_id)?;
    Ok(page)
}

fn validate_ocr_correction(
    page_bounds: PdfRect,
    rect: PdfRect,
    text: &str,
    appearance: &PdfTextAppearance,
    cover_color: PdfColor,
    background_proof: PdfOcrBackgroundProof,
    rotation: i64,
) -> Result<(), PdfEditError> {
    if rotation != 0 {
        return Err(PdfEditError::UnsupportedTransform);
    }
    if !matches!(background_proof, PdfOcrBackgroundProof::NearUniform)
        || !is_finite_color(cover_color)
        || cover_color.alpha < 1.0
        || !is_finite_color(appearance.color)
        || !appearance.size_pt.is_finite()
        || appearance.size_pt <= 0.0
        || text.is_empty()
        || !is_finite_rect(page_bounds)
        || page_bounds.width <= 0.0
        || page_bounds.height <= 0.0
        || !is_finite_rect(rect)
        || rect.width <= 0.0
        || rect.height <= 0.0
        || !page_bounds.contains(rect)
    {
        return Err(PdfEditError::TextOverflow);
    }
    Ok(())
}

fn is_finite_rect(rect: PdfRect) -> bool {
    rect.x.is_finite() && rect.y.is_finite() && rect.width.is_finite() && rect.height.is_finite()
}

fn is_finite_color(color: PdfColor) -> bool {
    color.r.is_finite()
        && color.g.is_finite()
        && color.b.is_finite()
        && color.alpha.is_finite()
        && (0.0..=1.0).contains(&color.r)
        && (0.0..=1.0).contains(&color.g)
        && (0.0..=1.0).contains(&color.b)
        && (0.0..=1.0).contains(&color.alpha)
}

fn page_bounds(doc: &Document, page_id: ObjectId) -> Result<PdfRect, PdfEditError> {
    let values = inherited_page_array(doc, page_id, b"CropBox")
        .or_else(|| inherited_page_array(doc, page_id, b"MediaBox"))
        .ok_or(PdfEditError::MalformedContent)?;
    if values.len() != 4 {
        return Err(PdfEditError::MalformedContent);
    }
    let x0 = values[0];
    let y0 = values[1];
    let width = values[2] - x0;
    let height = values[3] - y0;
    Ok(PdfRect {
        x: x0,
        y: y0,
        width,
        height,
    })
}

fn page_rotation(doc: &Document, page_id: ObjectId) -> Result<i64, PdfEditError> {
    Ok(inherited_page_number(doc, page_id, b"Rotate").unwrap_or(0))
}

fn inherited_page_array(doc: &Document, mut page_id: ObjectId, key: &[u8]) -> Option<Vec<f64>> {
    for _ in 0..64 {
        let dict = doc.get_object(page_id).ok()?.as_dict().ok()?;
        if let Ok(value) = dict.get(key) {
            let value = match value {
                Object::Reference(id) => doc.get_object(*id).ok()?,
                value => value,
            };
            let array = value.as_array().ok()?;
            return array.iter().map(object_number).collect();
        }
        page_id = match dict.get(b"Parent").ok()? {
            Object::Reference(id) => *id,
            _ => return None,
        };
    }
    None
}

fn object_number(value: &Object) -> Option<f64> {
    match value {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(*value as f64),
        _ => None,
    }
}

fn inherited_page_number(doc: &Document, mut page_id: ObjectId, key: &[u8]) -> Option<i64> {
    for _ in 0..64 {
        let dict = doc.get_object(page_id).ok()?.as_dict().ok()?;
        if let Ok(value) = dict.get(key) {
            let value = match value {
                Object::Reference(id) => doc.get_object(*id).ok()?,
                value => value,
            };
            return value.as_i64().ok();
        }
        page_id = match dict.get(b"Parent").ok()? {
            Object::Reference(id) => *id,
            _ => return None,
        };
    }
    None
}

/// Appends `new_stream_id` to the page's `/Contents` array without touching any existing stream
/// object — §9.3: "Do not mutate existing content operators," and §5.2's stream-boundary rule.
fn append_content_stream(
    doc: &mut Document,
    page_id: ObjectId,
    new_stream_id: ObjectId,
) -> Result<(), PdfEditError> {
    let page_obj = doc
        .get_object(page_id)
        .map_err(|_| PdfEditError::MalformedContent)?;
    let page_dict = page_obj
        .as_dict()
        .map_err(|_| PdfEditError::MalformedContent)?;

    let mut ids: Vec<ObjectId> = match page_dict.get(b"Contents") {
        Ok(Object::Reference(id)) => vec![*id],
        Ok(Object::Array(items)) => items
            .iter()
            .filter_map(|o| match o {
                Object::Reference(id) => Some(*id),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    ids.push(new_stream_id);

    let array = Object::Array(ids.into_iter().map(Object::Reference).collect());
    if let Ok(Object::Dictionary(dict)) = doc.get_object_mut(page_id) {
        dict.set("Contents", array);
    }
    Ok(())
}

// -------------------------------------------------------------------------------------------
// §7.4-adjacent — embedding a fresh Type0/CIDFontType2 subset for a resolved catalog font
// -------------------------------------------------------------------------------------------

/// Embeds a Type0/CIDFontType2/FontFile2/ToUnicode object graph (§7.4) for exactly the glyphs
/// `text` needs, via `pdf_font.rs`'s [`subset_for_pdf`]. Returns the new Type0 font's object id
/// plus `text`'s per-character CID codes (`/CIDToGIDMap /Identity`, so CID == subset glyph id).
///
/// The CID for each character is recomputed here (not read back from [`subset_for_pdf`], which
/// exposes only de-duplicated widths/`ToUnicode`, not a per-position code list) using the exact
/// same "first-appearance order, glyph 0 reserved for `.notdef`" numbering `subset_for_pdf` itself
/// documents and relies on: "assigns CIDs to equal the subset font's own new (post-subsetting)
/// glyph ids ... in the exact order we call `remap`." Recomputing over the same `text.chars()` /
/// `face.glyph_index` sequence that `subset_for_pdf` iterates internally reproduces that same
/// mapping without needing a new shared API between the two modules.
fn embed_type0_font(
    doc: &mut Document,
    resolved: &ResolvedPdfFont,
    text: &str,
) -> Result<(ObjectId, Vec<u16>), PdfEditError> {
    let subset = subset_for_pdf(&resolved.bytes, resolved.face_index, text)?;

    let face = ttf_parser::Face::parse(&resolved.bytes, resolved.face_index)
        .map_err(|_| PdfEditError::UnsupportedEncoding)?;
    let mut seen_old_gids: Vec<u16> = Vec::new();
    let mut cid_codes: Vec<u16> = Vec::with_capacity(text.chars().count());
    for c in text.chars() {
        let old_gid = face.glyph_index(c).ok_or(PdfEditError::MissingGlyph)?.0;
        let new_gid = match seen_old_gids.iter().position(|&g| g == old_gid) {
            Some(pos) => (pos + 1) as u16,
            None => {
                seen_old_gids.push(old_gid);
                seen_old_gids.len() as u16
            }
        };
        cid_codes.push(new_gid);
    }

    let font_file_id = doc.add_object(Stream::new(
        dictionary! { "Length1" => subset.font_program.len() as i64 },
        subset.font_program.clone(),
    ));

    let mut widths_sorted = subset.widths.clone();
    widths_sorted.sort_by_key(|w| w.cid);
    let mut w_array: Vec<Object> = Vec::with_capacity(widths_sorted.len() * 2);
    for w in &widths_sorted {
        w_array.push(Object::Integer(w.cid as i64));
        w_array.push(Object::Array(vec![Object::Integer(
            w.width_1000.round() as i64
        )]));
    }

    let base_name: Vec<u8> = resolved.family.replace(' ', "").into_bytes();

    let descriptor_id = doc.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => Object::Name(base_name.clone()),
        "Flags" => 4,
        "FontBBox" => Object::Array(vec![Object::Integer(0), Object::Integer(0), Object::Integer(1000), Object::Integer(1000)]),
        "ItalicAngle" => 0,
        "Ascent" => 900,
        "Descent" => -200,
        "CapHeight" => 700,
        "StemV" => 80,
        "FontFile2" => font_file_id,
    });

    let descendant_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => Object::Name(base_name.clone()),
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::String(b"Adobe".to_vec(), StringFormat::Literal),
            "Ordering" => Object::String(b"Identity".to_vec(), StringFormat::Literal),
            "Supplement" => 0,
        },
        "FontDescriptor" => descriptor_id,
        "DW" => 1000,
        "W" => Object::Array(w_array),
        "CIDToGIDMap" => "Identity",
    });

    let to_unicode_bytes = build_to_unicode_cmap(&subset.to_unicode).into_bytes();
    let to_unicode_id = doc.add_object(Stream::new(dictionary! {}, to_unicode_bytes));

    let type0_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => Object::Name(base_name),
        "Encoding" => "Identity-H",
        "DescendantFonts" => Object::Array(vec![Object::Reference(descendant_id)]),
        "ToUnicode" => to_unicode_id,
    });

    Ok((type0_id, cid_codes))
}

/// Adds `font_obj_id` to page `page_id`'s own `/Resources/Font` dictionary under a fresh,
/// collision-free name (§9.3: "Add resource under collision-free generated name"). Always writes a
/// page-owned copy of `/Resources` (and of `/Resources/Font`) rather than mutating whatever
/// dictionary object the page's `/Resources` currently points to, so a `/Resources` dictionary
/// shared with another page is never silently changed for both.
fn add_font_resource(
    doc: &mut Document,
    page_id: ObjectId,
    font_obj_id: ObjectId,
) -> Result<Vec<u8>, PdfEditError> {
    let page_obj = doc
        .get_object(page_id)
        .map_err(|_| PdfEditError::MalformedContent)?;
    let page_dict = page_obj
        .as_dict()
        .map_err(|_| PdfEditError::MalformedContent)?;

    let existing_resources = dict_entry_clone(doc, page_dict, b"Resources");
    let mut fonts_dict = dict_entry_clone(doc, &existing_resources, b"Font");

    let mut counter = 1u32;
    let mut name = format!("VREdit{counter}");
    while fonts_dict.has(name.as_bytes()) {
        counter += 1;
        name = format!("VREdit{counter}");
    }
    fonts_dict.set(name.as_str(), Object::Reference(font_obj_id));

    let mut new_resources = existing_resources;
    new_resources.set("Font", Object::Dictionary(fonts_dict));

    if let Ok(Object::Dictionary(page_dict_mut)) = doc.get_object_mut(page_id) {
        page_dict_mut.set("Resources", Object::Dictionary(new_resources));
    }

    Ok(name.into_bytes())
}

/// Clones the dictionary `dict[key]` resolves to — following one `/Reference` hop — or an empty
/// dictionary when the key is absent, not a dictionary, or points at a broken/non-dictionary
/// object. `add_font_resource` uses it so `/Resources` and `/Resources/Font` are always copied out
/// before mutation (a dictionary shared with another page must never be mutated in place).
fn dict_entry_clone(doc: &Document, dict: &Dictionary, key: &[u8]) -> Dictionary {
    match dict.get(key) {
        Ok(Object::Reference(id)) => doc
            .get_object(*id)
            .ok()
            .and_then(|o| o.as_dict().ok())
            .cloned()
            .unwrap_or_else(|| dictionary! {}),
        Ok(Object::Dictionary(d)) => d.clone(),
        _ => dictionary! {},
    }
}

/// One isolated `q / BT / Tf / rg / Tm / Tj / ET / Q` block showing `cid_codes` (Identity-H,
/// 2-byte-per-code) through `resource_name` at `anchor`, in `appearance.color`/`appearance.size_pt`
/// — the shared building block for both §9.2's appended local rewrite and §9.3's Add Text stream.
fn build_text_object_operations(
    resource_name: &[u8],
    appearance: &PdfTextAppearance,
    anchor: PdfPoint,
    cid_codes: &[u16],
) -> Vec<Operation> {
    let mut hex_bytes = Vec::with_capacity(cid_codes.len() * 2);
    for code in cid_codes {
        hex_bytes.extend_from_slice(&code.to_be_bytes());
    }

    vec![
        Operation::new("q", vec![]),
        Operation::new("BT", vec![]),
        Operation::new(
            "Tf",
            vec![
                Object::Name(resource_name.to_vec()),
                Object::Real(appearance.size_pt as f32),
            ],
        ),
        Operation::new(
            "rg",
            vec![
                Object::Real(appearance.color.r as f32),
                Object::Real(appearance.color.g as f32),
                Object::Real(appearance.color.b as f32),
            ],
        ),
        Operation::new(
            "Tm",
            vec![
                Object::Integer(1),
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(1),
                Object::Real(anchor.x as f32),
                Object::Real(anchor.y as f32),
            ],
        ),
        Operation::new(
            "Tj",
            vec![Object::String(hex_bytes, StringFormat::Hexadecimal)],
        ),
        Operation::new("ET", vec![]),
        Operation::new("Q", vec![]),
    ]
}

/// Isolated correction stream: opaque cover first, then embedded text with the same Type0 font
/// and ToUnicode mapping used by ordinary AddText/local rewrites.
fn build_ocr_correction_operations(
    resource_name: &[u8],
    appearance: &PdfTextAppearance,
    rect: PdfRect,
    cover_color: PdfColor,
    cid_codes: &[u16],
) -> Vec<Operation> {
    let mut operations = vec![
        Operation::new("q", vec![]),
        Operation::new(
            "rg",
            vec![
                Object::Real(cover_color.r as f32),
                Object::Real(cover_color.g as f32),
                Object::Real(cover_color.b as f32),
            ],
        ),
        Operation::new(
            "re",
            vec![
                Object::Real(rect.x as f32),
                Object::Real(rect.y as f32),
                Object::Real(rect.width as f32),
                Object::Real(rect.height as f32),
            ],
        ),
        Operation::new("f", vec![]),
        Operation::new("Q", vec![]),
    ];
    operations.extend(build_text_object_operations(
        resource_name,
        appearance,
        PdfPoint {
            x: rect.x,
            y: rect.y + rect.height * 0.18,
        },
        cid_codes,
    ));
    operations
}

// -------------------------------------------------------------------------------------------
// Content-stream mutation helpers shared by §9.1/§9.2
// -------------------------------------------------------------------------------------------

fn decode_stream_content(doc: &Document, id: ObjectId) -> Result<Content, PdfEditError> {
    let obj = doc
        .get_object(id)
        .map_err(|_| PdfEditError::MalformedContent)?;
    let stream = obj
        .as_stream()
        .map_err(|_| PdfEditError::MalformedContent)?;
    let bytes = stream
        .decompressed_content()
        .map_err(|_| PdfEditError::MalformedContent)?;
    Content::decode(&bytes).map_err(|_| PdfEditError::MalformedContent)
}

/// Replaces the stream object at `id` in place (same object number/generation, so every other
/// span's `stream_ref` in this batch stays valid) with freshly encoded, uncompressed content.
fn write_stream_content(
    doc: &mut Document,
    id: ObjectId,
    content: &Content,
) -> Result<(), PdfEditError> {
    let encoded = content
        .encode()
        .map_err(|_| PdfEditError::MalformedContent)?;
    doc.objects
        .insert(id, Stream::new(dictionary! {}, encoded).into());
    Ok(())
}

/// Splices `replacement` into the exact glyph byte range `[glyph_start, glyph_end)` of the string
/// operand identified by `operator_index`/`operand_index` (§5.1's `PdfSourceSpan` addressing
/// scheme) — a `Tj`/`'` operand's sole string, `"`'s third operand, or one string item inside a
/// `TJ` array. An empty `replacement` blanks the covered bytes (§9.2 step 2: "remove only source
/// glyph bytes covered by locator").
fn splice_operand_bytes(
    content: &mut Content,
    operator_index: u32,
    operand_index: u32,
    glyph_start: u32,
    glyph_end: u32,
    replacement: &[u8],
) -> Result<(), PdfEditError> {
    let op = content
        .operations
        .get_mut(operator_index as usize)
        .ok_or(PdfEditError::SourceDigestMismatch)?;

    match op.operator.as_str() {
        "Tj" | "'" => {
            let operand = op
                .operands
                .get_mut(0)
                .ok_or(PdfEditError::SourceDigestMismatch)?;
            splice_string_object(operand, glyph_start, glyph_end, replacement)
        }
        "\"" => {
            let operand = op
                .operands
                .get_mut(2)
                .ok_or(PdfEditError::SourceDigestMismatch)?;
            splice_string_object(operand, glyph_start, glyph_end, replacement)
        }
        "TJ" => {
            let Some(Object::Array(items)) = op.operands.get_mut(0) else {
                return Err(PdfEditError::SourceDigestMismatch);
            };
            let item = items
                .get_mut(operand_index as usize)
                .ok_or(PdfEditError::SourceDigestMismatch)?;
            splice_string_object(item, glyph_start, glyph_end, replacement)
        }
        _ => Err(PdfEditError::SourceDigestMismatch),
    }
}

fn splice_string_object(
    obj: &mut Object,
    start: u32,
    end: u32,
    replacement: &[u8],
) -> Result<(), PdfEditError> {
    let Object::String(bytes, _fmt) = obj else {
        return Err(PdfEditError::SourceDigestMismatch);
    };
    let (start, end) = (start as usize, end as usize);
    if start > end || end > bytes.len() {
        return Err(PdfEditError::SourceDigestMismatch);
    }
    let mut new_bytes = Vec::with_capacity(bytes.len() - (end - start) + replacement.len());
    new_bytes.extend_from_slice(&bytes[..start]);
    new_bytes.extend_from_slice(replacement);
    new_bytes.extend_from_slice(&bytes[end..]);
    *bytes = new_bytes;
    Ok(())
}

// -------------------------------------------------------------------------------------------
// §9.4 — preview wire types (the Tauri command itself lives in the app crate)
// -------------------------------------------------------------------------------------------

/// Wire request for the app crate's `compose_pdf_preview` command (§9.4). Field
/// names/shape match the frontend's
/// `ComposePdfPreviewRequest` comment on the frozen signature: `source_path`,
/// `expected_source_sha256`, `source_commands`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposePdfPreviewRequest {
    pub source_path: String,
    pub expected_source_sha256: String,
    pub source_commands: Vec<PdfEditCommand>,
}

// -------------------------------------------------------------------------------------------
// Tests (§14.1 composition fixtures)
// -------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Object as LObject};

    use crate::model::{GlyphId, PdfColor, PdfEditCommand, PdfOcrBackgroundProof, PdfPageGraph};

    // -----------------------------------------------------------------------------------
    // Fixture builders — shared with the rest of the crate via `crate::test_fixtures`.
    // -----------------------------------------------------------------------------------

    use crate::test_fixtures::{build_one_page_pdf as build_pdf, pdf_bytes as doc_bytes};

    fn locate_island(
        doc: &Document,
        sha: &str,
        page: u32,
        text: &str,
    ) -> crate::model::PdfTextIsland {
        let graph = walk_page_graph(doc, sha, page).expect("inspect fixture page");
        graph
            .islands
            .into_iter()
            .find(|isl| isl.text == text)
            .unwrap_or_else(|| panic!("no island with text {text:?}"))
    }

    struct FailingFontResolver;
    impl PdfFontResolver for FailingFontResolver {
        fn resolve(&self, _id: &str) -> Result<ResolvedPdfFont, PdfEditError> {
            Err(PdfEditError::UnsupportedEncoding)
        }
        fn glyphs_for_text(
            &self,
            _font: &ResolvedPdfFont,
            _text: &str,
        ) -> Result<Vec<GlyphId>, PdfEditError> {
            Err(PdfEditError::UnsupportedEncoding)
        }
    }

    fn installable_font() -> Vec<u8> {
        // fsType 0 = installable; the shared builder carries the proven sfnt layout.
        crate::test_fixtures::build_test_ttf(
            0,
            &[('A', 1), ('B', 2), ('C', 3), ('H', 4), ('i', 5)],
            600,
        )
    }

    struct TestFontResolver {
        id: &'static str,
        bytes: Vec<u8>,
    }

    impl PdfFontResolver for TestFontResolver {
        fn resolve(&self, id: &str) -> Result<ResolvedPdfFont, PdfEditError> {
            if id != self.id {
                return Err(PdfEditError::UnsupportedEncoding);
            }
            Ok(ResolvedPdfFont {
                id: self.id.to_string(),
                family: "Test Font".to_string(),
                bytes: self.bytes.clone(),
                face_index: 0,
                embeddable: true,
                subsettable: true,
            })
        }

        fn glyphs_for_text(
            &self,
            font: &ResolvedPdfFont,
            text: &str,
        ) -> Result<Vec<GlyphId>, PdfEditError> {
            // The mock shares `pdf_font.rs`'s real mapping so tests exercise the same
            // MissingGlyph/UnsupportedEncoding behavior the production resolver has.
            crate::font::glyph_ids_for_text(&font.bytes, font.face_index, text)
        }
    }

    fn appearance(font_id: &str, size_pt: f64) -> PdfTextAppearance {
        PdfTextAppearance {
            font_id: font_id.to_string(),
            family: font_id.to_string(),
            size_pt,
            color: PdfColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                alpha: 1.0,
            },
        }
    }

    /// The resolver every successful-embed test uses: catalog id `"test-font"` backed by the
    /// installable five-glyph fixture font.
    fn test_resolver() -> TestFontResolver {
        TestFontResolver {
            id: "test-font",
            bytes: installable_font(),
        }
    }

    /// A `ReplaceSourceText` command whose `before` mirrors `island` exactly (its text and
    /// appearance — what a real edit session captures) and whose `after` carries the new
    /// text/appearance. Mutate `island.locator` in place before calling to model a stale or
    /// tampered locator.
    fn replace_cmd(
        id: &str,
        island: &crate::model::PdfTextIsland,
        after_text: &str,
        after_appearance: PdfTextAppearance,
        available_bounds: PdfRect,
    ) -> PdfEditCommand {
        PdfEditCommand::ReplaceSourceText {
            id: id.to_string(),
            locator: island.locator.clone(),
            before: PdfTextEditState {
                text: island.text.clone(),
                appearance: island.appearance.clone(),
            },
            after: PdfTextEditState {
                text: after_text.to_string(),
                appearance: after_appearance,
            },
            available_bounds,
        }
    }

    /// Reopen a composed candidate's bytes and re-walk `page`'s island graph — the shared tail of
    /// every compose-then-verify test. Also returns the reopened `Document` for tests that inspect
    /// raw page objects (annotations, appended content streams).
    fn reinspect(candidate: &ComposedCandidate, page: u32) -> (Document, PdfPageGraph) {
        let out = Document::load_mem(&candidate.bytes).expect("reopen candidate");
        let graph =
            walk_page_graph(&out, &document_sha256(&candidate.bytes), page).expect("reinspect");
        (out, graph)
    }

    // -----------------------------------------------------------------------------------
    // §9.1 Exact operand patch
    // -----------------------------------------------------------------------------------

    #[test]
    fn exact_operand_patch_replaces_single_tj_text_in_place() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "Hello");

        let command = replace_cmd(
            "edit-1",
            &island,
            "Howdy",
            island.appearance.clone(),
            PdfRect {
                x: 72.0,
                y: 695.0,
                width: 400.0,
                height: 20.0,
            },
        );

        let candidate = compose_candidate(&source, &[command], &FailingFontResolver)
            .expect("exact patch composes");
        assert_eq!(candidate.affected_pages, vec![1]);

        let (_, graph) = reinspect(&candidate, 1);
        assert!(graph.islands.iter().any(|isl| isl.text == "Howdy"));
        assert!(!graph.islands.iter().any(|isl| isl.text == "Hello"));
    }

    #[test]
    fn corrected_source_text_survives_native_staging_verification() {
        let source = include_bytes!("../tests/fixtures/source-correction-2page.pdf");
        let doc = Document::load_mem(source).expect("fixture");
        let sha = document_sha256(source);
        let island = locate_island(&doc, &sha, 1, "ORIGINAL SOURCE TEXT");
        let command = replace_cmd(
            "correction",
            &island,
            "SOURCE TEST 930",
            island.appearance.clone(),
            PdfRect {
                x: 72.0,
                y: 717.0,
                width: 146.028,
                height: 12.0,
            },
        );
        // Fresh font HashMaps previously chose WinAnsi's unmeasured 0xA0 space alias randomly.
        for _ in 0..32 {
            assert_eq!(
                simple_font_identity_bytes(
                    &doc,
                    *doc.get_pages().get(&1).expect("page"),
                    b"F1",
                    "SOURCE TEST 930"
                ),
                Some(b"SOURCE TEST 930".to_vec())
            );
            let candidate =
                compose_candidate(source, std::slice::from_ref(&command), &FailingFontResolver)
                    .expect("compose");
            let (_, graph) = reinspect(&candidate, 1);
            assert!(graph
                .islands
                .iter()
                .any(|island| island.text.contains("SOURCE TEST 930")));
            assert!(graph
                .islands
                .iter()
                .any(|island| island.text == "SIBLING SOURCE TEXT"));
            let store = crate::save::PdfSaveTokenStore::default();
            crate::save::stage_prepared_save(
                &store,
                "/fixture.pdf",
                &sha,
                &sha,
                candidate.bytes,
                candidate.affected_pages,
                crate::save::expected_text_checks(std::slice::from_ref(&command)),
            )
            .expect("verified stage");
        }
    }

    #[test]
    fn exact_operand_patch_rejects_overflow_before_mutating() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Hi) Tj ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "Hi");

        // Tiny available rect: the long replacement cannot fit.
        let command = replace_cmd(
            "edit-overflow",
            &island,
            "This replacement text is far too long",
            island.appearance.clone(),
            PdfRect {
                x: 72.0,
                y: 695.0,
                width: 10.0,
                height: 10.0,
            },
        );

        let err = compose_candidate(&source, &[command], &FailingFontResolver)
            .expect_err("overflow must reject");
        assert!(matches!(err, PdfEditError::TextOverflow));
    }

    // -----------------------------------------------------------------------------------
    // compose_candidate_on_base — composing on top of pdf.js's annotation incremental update
    // -----------------------------------------------------------------------------------

    #[test]
    fn compose_candidate_on_base_preserves_annotations_while_editing_text() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "Hello");

        // Simulate pdf.js's `saveDocument()` incremental update landing on the frontend: same
        // document, but with a FreeText annotation now present on page 1's /Annots. This is the
        // `base` prepare_pdf_save composes on top of; `sha`/`island.locator` are still captured
        // against the original, unannotated `source` bytes, exactly as the real edit session does.
        let mut annotated = doc.clone();
        let page_id = *annotated.get_pages().get(&1).expect("page 1 exists");
        let annot_id = annotated.add_object(dictionary! {
            "Type" => "Annot",
            "Subtype" => "FreeText",
            "Rect" => LObject::Array(vec![
                LObject::Real(10.0),
                LObject::Real(10.0),
                LObject::Real(100.0),
                LObject::Real(30.0),
            ]),
            "Contents" => LObject::String(b"note".to_vec(), lopdf::StringFormat::Literal),
        });
        if let Ok(LObject::Dictionary(page_dict)) = annotated.get_object_mut(page_id) {
            page_dict.set("Annots", LObject::Array(vec![LObject::Reference(annot_id)]));
        }
        let base = doc_bytes(&annotated);

        let command = replace_cmd(
            "edit-1",
            &island,
            "Howdy",
            island.appearance.clone(),
            PdfRect {
                x: 72.0,
                y: 695.0,
                width: 400.0,
                height: 20.0,
            },
        );

        let candidate = compose_candidate_on_base(&sha, &base, &[command], &FailingFontResolver)
            .expect("compose on annotated base");

        let (out, graph) = reinspect(&candidate, 1);
        let out_page_id = *out.get_pages().get(&1).expect("page 1 exists in candidate");
        let out_page_dict = out
            .get_object(out_page_id)
            .expect("page object")
            .as_dict()
            .expect("page dict");
        assert!(
            out_page_dict.has(b"Annots"),
            "FreeText annotation must survive text composition"
        );

        assert!(graph.islands.iter().any(|isl| isl.text == "Howdy"));
        assert!(!graph.islands.iter().any(|isl| isl.text == "Hello"));
    }

    #[test]
    fn compose_candidate_on_base_with_no_commands_returns_base_verbatim() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET");
        let base = doc_bytes(&doc);
        let sha = document_sha256(&base);
        let candidate = compose_candidate_on_base(&sha, &base, &[], &FailingFontResolver)
            .expect("empty commands compose trivially");
        assert_eq!(candidate.bytes, base);
        assert!(candidate.affected_pages.is_empty());
    }

    // -----------------------------------------------------------------------------------
    // Stale source / digest mismatch
    // -----------------------------------------------------------------------------------

    #[test]
    fn stale_document_sha256_is_rejected() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let mut island = locate_island(&doc, &sha, 1, "Hello");
        island.locator.document_sha256 =
            "0000000000000000000000000000000000000000000000000000000000000000".to_string();

        let command = replace_cmd(
            "edit-stale",
            &island,
            "Howdy",
            island.appearance.clone(),
            PdfRect {
                x: 72.0,
                y: 695.0,
                width: 400.0,
                height: 20.0,
            },
        );

        let err = compose_candidate(&source, &[command], &FailingFontResolver)
            .expect_err("stale source must reject");
        assert!(matches!(err, PdfEditError::StaleSource));
    }

    #[test]
    fn tampered_island_digest_with_valid_document_sha_is_source_digest_mismatch() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let mut island = locate_island(&doc, &sha, 1, "Hello");

        // `document_sha256` still matches (nothing about the document actually changed), but the
        // specific island_digest given doesn't correspond to any island the fresh walk produces —
        // models "same visible text but changed operand digest" (§14.1).
        island.locator.island_digest = format!("{}-tampered", island.locator.island_digest);

        let command = replace_cmd(
            "edit-tampered",
            &island,
            "Howdy",
            island.appearance.clone(),
            PdfRect {
                x: 72.0,
                y: 695.0,
                width: 400.0,
                height: 20.0,
            },
        );

        let err = compose_candidate(&source, &[command], &FailingFontResolver)
            .expect_err("tampered digest must reject");
        assert!(matches!(err, PdfEditError::SourceDigestMismatch));
    }

    // -----------------------------------------------------------------------------------
    // §9.2 Local text-object rewrite (split runs / changed appearance)
    // -----------------------------------------------------------------------------------

    #[test]
    fn local_rewrite_replaces_adjacent_tj_operators_via_embedded_font_round_trip() {
        // Standard-14 metrics place the second run after the first; Td 0 0 would overlap it.
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (CAR YOU) Tj 55 0 Td ( LIST.) Tj ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "CAR YOU LIST.");
        // This fixture's grouping produces >1 span (two Tj operators), so exact-patch's
        // `one_operand` gate cannot apply regardless of appearance — always local rewrite.
        assert!(island.locator.spans.len() > 1);

        let resolver = test_resolver();
        let command = replace_cmd(
            "edit-split",
            &island,
            "Hi",
            appearance("test-font", 20.0),
            PdfRect {
                x: 72.0,
                y: 690.0,
                width: 400.0,
                height: 40.0,
            },
        );

        let candidate =
            compose_candidate(&source, &[command], &resolver).expect("local rewrite composes");
        let (_, graph) = reinspect(&candidate, 1);

        // The old run's text is gone (bytes blanked in place) and the new run round-trips through
        // the freshly embedded Type0/CIDFontType2/ToUnicode subset exactly as "Hi".
        assert!(!graph.islands.iter().any(|isl| isl.text == "CAR YOU LIST."));
        assert!(graph
            .islands
            .iter()
            .any(|isl| isl.text == "Hi" && isl.editable));
    }

    #[test]
    fn local_rewrite_used_when_appearance_changes_even_for_single_span() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "Hello");
        assert_eq!(island.locator.spans.len(), 1);

        let resolver = test_resolver();
        // Different font_id/size than `before` — same_font_encoding is false, so this must
        // route to LocalTextObjectRewrite despite a single-span locator.
        let command = replace_cmd(
            "edit-appearance",
            &island,
            "Hi",
            appearance("test-font", 24.0),
            PdfRect {
                x: 72.0,
                y: 690.0,
                width: 400.0,
                height: 40.0,
            },
        );

        let candidate =
            compose_candidate(&source, &[command], &resolver).expect("compose succeeds");
        let (_, graph) = reinspect(&candidate, 1);
        assert!(graph.islands.iter().any(|isl| isl.text == "Hi"));
    }

    // -----------------------------------------------------------------------------------
    // §9.3 Add Text
    // -----------------------------------------------------------------------------------

    #[test]
    fn add_text_appends_isolated_stream_without_touching_existing_content() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Existing) Tj ET");
        let source = doc_bytes(&doc);

        let resolver = test_resolver();
        let command = PdfEditCommand::AddText {
            id: "add-1".to_string(),
            page: 1,
            anchor: PdfPoint { x: 100.0, y: 100.0 },
            text: "Hi".to_string(),
            appearance: appearance("test-font", 18.0),
        };

        let candidate =
            compose_candidate(&source, &[command], &resolver).expect("add text composes");
        assert_eq!(candidate.affected_pages, vec![1]);

        let (_, graph) = reinspect(&candidate, 1);

        assert!(graph.islands.iter().any(|isl| isl.text == "Existing"));
        assert!(graph.islands.iter().any(|isl| isl.text == "Hi"));
    }

    #[test]
    fn add_text_missing_glyph_rejects_before_any_mutation() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Existing) Tj ET");
        let source = doc_bytes(&doc);
        // `installable_font` only maps A, B, C, H, i.
        let resolver = test_resolver();
        let command = PdfEditCommand::AddText {
            id: "add-missing".to_string(),
            page: 1,
            anchor: PdfPoint { x: 100.0, y: 100.0 },
            text: "Zzz".to_string(),
            appearance: appearance("test-font", 18.0),
        };

        let err = compose_candidate(&source, &[command], &resolver)
            .expect_err("missing glyph must reject");
        assert!(matches!(err, PdfEditError::MissingGlyph));
    }

    #[test]
    fn add_text_unknown_font_id_rejects_with_unsupported_encoding() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Existing) Tj ET");
        let source = doc_bytes(&doc);
        let command = PdfEditCommand::AddText {
            id: "add-unknown-font".to_string(),
            page: 1,
            anchor: PdfPoint { x: 100.0, y: 100.0 },
            text: "Hi".to_string(),
            appearance: appearance("does-not-exist", 18.0),
        };

        let err = compose_candidate(&source, &[command], &FailingFontResolver)
            .expect_err("unknown font id must reject");
        assert!(matches!(err, PdfEditError::UnsupportedEncoding));
    }

    fn ocr_command(id: &str, line_id: &str, text: &str, rect: PdfRect) -> PdfEditCommand {
        PdfEditCommand::CorrectOcrLine {
            id: id.to_string(),
            page: 1,
            line_id: line_id.to_string(),
            rect,
            text: text.to_string(),
            appearance: appearance("test-font", 18.0),
            cover_color: PdfColor {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                alpha: 1.0,
            },
            background_proof: PdfOcrBackgroundProof::NearUniform,
        }
    }

    #[test]
    fn correct_ocr_text_appends_cover_and_type0_text_stream() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Existing) Tj ET");
        let source = doc_bytes(&doc);
        let resolver = test_resolver();
        let command = ocr_command(
            "ocr-1",
            "line-1",
            "Hi",
            PdfRect {
                x: 100.0,
                y: 100.0,
                width: 100.0,
                height: 30.0,
            },
        );
        let candidate = compose_candidate(&source, &[command], &resolver).expect("compose OCR");
        let (out, graph) = reinspect(&candidate, 1);
        let page_id = out.get_pages()[&1];
        let page = out
            .get_object(page_id)
            .expect("page")
            .as_dict()
            .expect("dict");
        let contents = page
            .get(b"Contents")
            .expect("contents")
            .as_array()
            .expect("array");
        let stream_id = contents
            .last()
            .expect("new stream")
            .as_reference()
            .expect("ref");
        let stream = out
            .get_object(stream_id)
            .expect("stream")
            .as_stream()
            .expect("stream");
        let content = Content::decode(&stream.decompressed_content().expect("decode stream"))
            .expect("content operations");
        let operators: Vec<&str> = content
            .operations
            .iter()
            .map(|operation| operation.operator.as_str())
            .collect();
        assert_eq!(&operators[..5], &["q", "rg", "re", "f", "Q"]);
        assert!(operators.windows(2).any(|window| window == ["BT", "Tf"]));

        assert!(graph.islands.iter().any(|island| island.text == "Hi"));
    }

    #[test]
    fn correct_ocr_text_duplicate_line_id_last_wins() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Existing) Tj ET");
        let source = doc_bytes(&doc);
        let resolver = test_resolver();
        let rect = PdfRect {
            x: 100.0,
            y: 100.0,
            width: 100.0,
            height: 30.0,
        };
        let commands = vec![
            ocr_command("ocr-old", "line-1", "Hi", rect),
            ocr_command("ocr-new", "line-1", "Hi", rect),
        ];
        let candidate = compose_candidate(&source, &commands, &resolver).expect("compose OCR");
        let (_, graph) = reinspect(&candidate, 1);
        assert_eq!(
            graph
                .islands
                .iter()
                .filter(|island| island.text == "Hi")
                .count(),
            1
        );
    }

    #[test]
    fn correct_ocr_text_rejects_rotation_and_out_of_bounds_rect() {
        let mut rotated = build_pdf(b"BT /F1 12 Tf 72 700 Td (Existing) Tj ET");
        let page_id = rotated.get_pages()[&1];
        rotated
            .get_object_mut(page_id)
            .expect("page")
            .as_dict_mut()
            .expect("dict")
            .set("Rotate", 90);
        let rotated_source = doc_bytes(&rotated);
        let resolver = test_resolver();
        let rect = PdfRect {
            x: 100.0,
            y: 100.0,
            width: 100.0,
            height: 30.0,
        };
        assert!(matches!(
            compose_candidate(
                &rotated_source,
                &[ocr_command("r", "line", "Hi", rect)],
                &resolver
            ),
            Err(PdfEditError::UnsupportedTransform)
        ));

        let source = doc_bytes(&build_pdf(b"BT /F1 12 Tf 72 700 Td (Existing) Tj ET"));
        let bad_rect = PdfRect {
            x: 600.0,
            y: 100.0,
            width: 100.0,
            height: 30.0,
        };
        assert!(matches!(
            compose_candidate(
                &source,
                &[ocr_command("b", "line", "Hi", bad_rect)],
                &resolver
            ),
            Err(PdfEditError::TextOverflow)
        ));
    }

    #[test]
    fn empty_command_list_returns_source_bytes_unchanged() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET");
        let source = doc_bytes(&doc);
        let candidate = compose_candidate(&source, &[], &FailingFontResolver)
            .expect("empty batch always succeeds");
        assert_eq!(candidate.bytes, source);
        assert!(candidate.affected_pages.is_empty());
    }
}
