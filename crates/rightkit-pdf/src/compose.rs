//! PDF editor sparse composition (Dispatch C, lane C1).
//!
//! Owns `docs/PDF-EDITOR-NOW.md` §8 (layout validation), §9.1-9.3 (the three sparse write
//! strategies), and §9.4 (the `compose_pdf_preview` Tauri command). Implements the frozen
//! cross-lane seam (`docs/PDF-EDITOR-DISPATCH.md`):
//!
//! ```text
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
//! - **Strategy selection.** A `ReplaceSourceText` command takes
//!   `WriteStrategy::ExactOperandPatch` when (a) `before` and `after` appearance are identical
//!   (same font/size/color) and (b) the page graph reports the island `editable`, i.e. its own font
//!   can re-encode text (`inspect::ReverseTable`, one decision shared with the graph) and all its
//!   glyphs share one font, size and spacing. The island may span several operands (kerned `TJ`
//!   text): the whole replacement is written into the earliest span and the later spans are
//!   blanked. Text the font cannot encode is refused with `PdfEditError::MissingGlyphs` naming the
//!   characters; nothing is ever substituted. Every other digest-verified command falls through
//!   to `WriteStrategy::LocalTextObjectRewrite` (a catalog font). `AddText` always uses
//!   `WriteStrategy::AppendTextStream`.
//! - **Font reuse vs. embed.** [`PdfFontResolver`] (§7.2) resolves *catalog* fonts only — it has no
//!   notion of "the font already used at this source location," and `pdf_inspect.rs`'s
//!   content-stream font decoder (`TextStateFont`) is a different, page-local abstraction this
//!   composer never receives. So "reuse source PDF font" (§7.3 rule 1) is only achievable for the
//!   `ExactOperandPatch` path, where the original operand bytes/font resource are never touched at
//!   all — the safest possible form of reuse. Every `LocalTextObjectRewrite`/`AppendTextStream`
//!   resolves `after.appearance.font_id` through `fonts` and embeds a fresh Type0/CIDFontType2
//!   subset (§7.4), even when a caller happens to pass the same font family as the original run.
//! - **Local-rewrite insertion point.** §9.2 says "insert ... at deterministic source boundary"
//!   without specifying which. The new `q/BT/.../ET/Q` block is never written into the middle of
//!   the touched content stream: it goes in its own stream at the END of the page's `/Contents`.
//!   Removing the old glyphs never renumbers an operator, so a second, independent command's
//!   locator (captured against the same pristine source, per §9.4's "Applies staged source
//!   commands to fresh source bytes") stays valid when a prior command in the same batch already
//!   rewrote a different run in the same content stream. A removed glyph's bytes are replaced in
//!   place by the `TJ` number that moves the pen exactly as the glyph did (a `Tj` becomes a `TJ`
//!   at the same operator index), so the text that follows keeps its position; the `'` and `"`
//!   operators carry a line move a `TJ` cannot, so glyphs shown by them are refused.
//! - **Batch order.** All locators of a batch are resolved against the pristine source, so no
//!   command may shift an address another command still needs. The `q`/`Q` isolation wrappers and
//!   the new streams are therefore installed once, after the last command (installing a wrapper
//!   per command shifted every original `contents_index`, which the island digest binds), and the
//!   `ReplaceSourceText` commands of one page are applied back to front, so splitting a `TJ`
//!   string item only ever moves items that a command already applied has finished with.
//! - **Baseline anchor for a rewritten run.** §8 says "Replacement preserves original baseline
//!   anchor." The new run is therefore placed from the island it replaces, never from
//!   `availableBounds` (which `PdfSourceEdit`'s wire shape, §6, carries as the only rectangle, and
//!   which is only the verified room the run must fit). The new stream runs in the default
//!   graphics state, so its `Tm` is the whole of the first glyph's `text matrix x CTM`
//!   (`IslandLayout::placement`; row-vector convention, ISO 32000-1 9.4.2, as `Matrix::then`),
//!   with `Tf` kept at the original text-space size: the glyph origin, rotation, skew and scale
//!   are those of the source whether the producer put them in `Tm`, `Td` or a `cm` outside the
//!   text object. A read-back of the composed page refuses the batch when a replacement does not
//!   land there (`verify_appended_runs`).
//! - **Layout measurement.** `MeasuredRun`'s width for an embedded-font rewrite/add-text run is
//!   measured exactly, by summing the resolved font's real per-glyph advances (via `ttf_parser`,
//!   same source `subset_for_pdf` reads) scaled to `/1000` units. For the in-place exact-operand-patch
//!   path, the source font's own widths (the same `/Widths` or `/W` data the page graph's glyph
//!   boxes are built from) are summed with the walker's pen-advance rules (`Tc`, `Tw`, `Tz`,
//!   text matrix and CTM scale), so a replacement no wider than the original never overflows its
//!   own box (within `OVERFLOW_EPSILON_PT`). Earlier versions estimated `chars * size_pt * 0.5`,
//!   which refused same-width swaps whenever the real text was narrower than half an em per
//!   character.
//! - **`compose_pdf_preview`'s "affected pages as metadata."** Candidate bytes return as a JSON
//!   byte array because WKWebView fails while decoding Tauri's raw-response stream. Frontend derives
//!   affected pages from request commands, so no second metadata channel is required.
use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream, StringFormat};
use serde::Deserialize;

use crate::font::{build_to_unicode_cmap, subset_for_pdf};
use crate::inspect::{
    self, content_stream_ids, encode_middle_for_font, is_line_moving_show, walk_page_graph,
    ContentBudget, ReencodeFailure,
};
use crate::model::{
    document_sha256, effective_pdf_edit_commands, ComposedCandidate, GapNumber, GlyphSourceAddress,
    Matrix, PdfColor, PdfEditCommand, PdfEditError, PdfFontResolver, PdfOcrBackgroundProof,
    PdfPageGraph, PdfPoint, PdfRect, PdfSourceLocator, PdfSourceSpan, PdfTextAppearance,
    PdfTextEditState, PdfTextIsland, ResolvedPdfFont, RunItem,
};

/// Slack, in device-space points, when a replacement's measured width is compared with the
/// available box. A replacement whose advance is no wider than the original's lands within float
/// summation error of the island's own box (the app also round-trips the box through JSON), so
/// an exact `<=` would refuse a same-width swap. 0.05 pt is about 1/1400 inch: invisible, and well
/// below the width of any glyph.
const OVERFLOW_EPSILON_PT: f64 = 0.05;

// -------------------------------------------------------------------------------------------
// §9 — Strategy selection
// -------------------------------------------------------------------------------------------

/// §9's `select_strategy` outcome, concretized per this module's judgment calls (see module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteStrategy {
    ExactOperandPatch,
    LocalTextObjectRewrite,
}

/// `in_place_editable` is the page graph's own verdict for the island (`PdfTextIsland::editable`):
/// the island's font can re-encode text, and its glyphs share one font, size and spacing. That is
/// the single re-encodability decision; this function does not re-derive it.
fn select_replace_strategy(
    before: &PdfTextEditState,
    after: &PdfTextEditState,
    in_place_editable: bool,
) -> WriteStrategy {
    let same_font_encoding = before.appearance == after.appearance;

    if same_font_encoding && in_place_editable {
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

    let mut doc = inspect::load_pdf(base).map_err(|_| PdfEditError::MalformedContent)?;
    if doc.is_encrypted() {
        return Err(PdfEditError::EncryptedDocument);
    }

    // What every page can name before any edit; checked again below.
    let resources_before = inspect::resource_fingerprint(&doc)?;
    let mut affected_pages: Vec<u32> = Vec::new();

    // Locators are all captured against the pristine source, so the commands run in an order that
    // never lets one shift an address another still needs (see the module docs), and every
    // isolation wrapper and new stream waits until the last command has been applied.
    let effective = effective_pdf_edit_commands(commands);
    let mut isolated: Vec<(usize, Isolated)> = Vec::new();
    let mut appended: Vec<(usize, AppendedRun)> = Vec::new();
    for slot in back_to_front_order(&effective) {
        let command = effective[slot];
        let (page, block) = match command {
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
                let (page, block, run) = apply_replace_source_text(
                    &mut doc,
                    source_sha,
                    locator,
                    before,
                    after,
                    *available_bounds,
                    fonts,
                )?;
                appended.extend(run.map(|run| (slot, run)));
                (page, block)
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
        if let Some(block) = block {
            isolated.push((slot, block));
        }
        if !affected_pages.contains(&page) {
            affected_pages.push(page);
        }
    }
    let streams = install_isolated_content(&mut doc, isolated)?;
    // The structural checks below, and the save-side recheck, only ask whether the text reads back.
    // A replacement drawn somewhere else, at another size, reads back just as well.
    for (slot, run) in &mut appended {
        run.stream = streams.get(slot).copied();
    }
    let appended: Vec<AppendedRun> = appended.into_iter().map(|(_, run)| run).collect();
    verify_appended_runs(&doc, &appended)?;

    // Never trust the composer blindly: an edit may add a resource, but a page must still resolve
    // every font, XObject and graphics state it resolved in the source. (Adding a font to a page
    // whose `/Resources` lived on an ancestor `/Pages` node once hid all of them.)
    if !inspect::resources_preserved(&resources_before, &inspect::resource_fingerprint(&doc)?) {
        return Err(PdfEditError::VerificationFailed);
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

/// A block of operations to run on a page in its own content stream, outside the page's original
/// graphics state: `(page object, operations)`. Collected while the batch runs and installed once
/// at the end, in command order (see [`install_isolated_content`]).
type Isolated = (ObjectId, Vec<Operation>);

/// The first source address a command touches: `(contents_index, operator_index, operand_index,
/// glyph_start)` of its earliest span.
fn command_address(command: &PdfEditCommand) -> (u32, u32, u32, u32) {
    match command {
        PdfEditCommand::ReplaceSourceText { locator, .. } => locator
            .spans
            .iter()
            .map(|span| {
                (
                    span.contents_index,
                    span.operator_index,
                    span.operand_index,
                    span.glyph_start,
                )
            })
            .min()
            .unwrap_or_default(),
        _ => (0, 0, 0, 0),
    }
}

/// The order in which a batch is applied: the original order, except that the `ReplaceSourceText`
/// commands aimed at one page take that page's slots back to front by source address. A command
/// only ever changes bytes at or after its own address, so applying the later ones first leaves
/// every earlier address exactly where the (pristine-source) locators say it is. Ties keep the
/// caller's order, so a second command on the same island still meets a changed island and is
/// refused.
fn back_to_front_order(commands: &[&PdfEditCommand]) -> Vec<usize> {
    let mut by_page: std::collections::BTreeMap<u32, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (index, command) in commands.iter().enumerate() {
        if let PdfEditCommand::ReplaceSourceText { locator, .. } = command {
            by_page.entry(locator.page).or_default().push(index);
        }
    }
    for list in by_page.values_mut() {
        list.sort_by(|a, b| {
            command_address(commands[*b])
                .cmp(&command_address(commands[*a]))
                .then(a.cmp(b))
        });
    }
    let mut taken: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
    commands
        .iter()
        .enumerate()
        .map(|(index, command)| match command {
            PdfEditCommand::ReplaceSourceText { locator, .. } => {
                let next = taken.entry(locator.page).or_insert(0);
                let chosen = by_page[&locator.page][*next];
                *next += 1;
                chosen
            }
            _ => index,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn apply_replace_source_text(
    doc: &mut Document,
    current_sha: &str,
    locator: &PdfSourceLocator,
    before: &PdfTextEditState,
    after: &PdfTextEditState,
    available_bounds: PdfRect,
    fonts: &dyn PdfFontResolver,
) -> Result<(u32, Option<Isolated>, Option<AppendedRun>), PdfEditError> {
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
    // A rejected island has no spans and nothing to remove. Whether the island is `editable` in
    // place is decided per strategy below: a changed appearance redraws in a catalog font and
    // needs no re-encoding through the source font.
    if island.locator.spans.is_empty() {
        return Err(PdfEditError::UnsupportedEncoding);
    }
    // The caller's locator must name exactly the spans the page has now: same count, same order,
    // each equal. Membership alone is not enough - a locator with a span removed still passes it
    // and the edit would delete the named operands, leave the rest of the island and append the
    // replacement. The spans every mutation below works from are the freshly inspected ones, never
    // the caller's copy.
    if locator.spans.len() != island.locator.spans.len()
        || !locator
            .spans
            .iter()
            .zip(&island.locator.spans)
            .all(|(given, current)| spans_equal(given, current))
    {
        return Err(PdfEditError::SourceDigestMismatch);
    }
    let spans: &[PdfSourceSpan] = &island.locator.spans;

    // This composer's sparse strategies only ever touch one content stream per command — every
    // span must agree on which one.
    let stream_ref = spans[0].stream_ref;
    let contents_index = spans[0].contents_index;
    if !spans
        .iter()
        .all(|s| s.stream_ref == stream_ref && s.contents_index == contents_index)
    {
        return Err(PdfEditError::UnsupportedEncoding);
    }
    let stream_id: ObjectId = (stream_ref.0, stream_ref.1);
    let page_id = pages_lookup(doc, locator.page)?;

    // An island the graph calls non-editable is refused here, by the composer, before any
    // strategy is chosen. Unchanged appearance is a request to keep the source font; an island
    // that cannot be written through it (a positioned run, a mixed state, a font that cannot be
    // re-encoded) must not fall through to a catalog-font redraw nobody asked for. A *changed*
    // appearance is the explicit request to redraw in a catalog font (`substitution_editable`).
    if before.appearance == after.appearance && !island.editable {
        return Err(PdfEditError::UnsupportedEncoding);
    }

    // Both strategies rewrite the stream object in place; a page that shares it with another page
    // gets its own copy first, so the other pages stay as they were.
    isolate_shared_content_stream(doc, page_id, stream_id)?;

    let (block, run) = match select_replace_strategy(before, after, island.editable) {
        WriteStrategy::ExactOperandPatch => {
            apply_exact_operand_patch(
                doc,
                page_id,
                locator.page,
                stream_id,
                island,
                after,
                available_bounds,
            )?;
            (None, None)
        }
        WriteStrategy::LocalTextObjectRewrite => {
            let (block, run) = apply_local_text_object_rewrite(
                doc,
                locator.page,
                page_id,
                stream_id,
                island,
                after,
                available_bounds,
                fonts,
            )?;
            (Some(block), run)
        }
    };

    Ok((locator.page, block, run))
}

/// Most content-stream references the pages that share one stream may carry together. Each is
/// copied into the rewritten page arrays, so this is what an isolation may allocate for them.
const MAX_ISOLATED_REFERENCES: usize = 1 << 20;

/// Gives every *other* page that lists `stream_id` in its `/Contents` its own byte-identical copy
/// of the stream, so rewriting `stream_id` in place changes only `page_id`. The edited page keeps
/// the original object id on purpose: every locator in a batch is addressed by it. A later command
/// aimed at one of the other pages therefore sees a different stream id and is refused as stale
/// instead of being applied to the wrong page.
///
/// One copy is made, however many pages share the stream: they keep sharing it with each other
/// (as they did with the original), so the allocation is one stream, not one per sharing page. The
/// page arrays rewritten for them are bounded by [`MAX_ISOLATED_REFERENCES`], checked before they
/// are collected.
fn isolate_shared_content_stream(
    doc: &mut Document,
    page_id: ObjectId,
    stream_id: ObjectId,
) -> Result<(), PdfEditError> {
    let mut sharers: Vec<(ObjectId, Vec<ObjectId>)> = Vec::new();
    let mut references = 0usize;
    for other in doc.get_pages().into_values() {
        if other == page_id {
            continue;
        }
        let Ok(Object::Dictionary(dict)) = doc.get_object(other) else {
            continue;
        };
        let Ok(contents) = dict.get(b"Contents") else {
            continue;
        };
        let ids = content_stream_ids(doc, contents)?;
        if ids.contains(&stream_id) {
            references = references
                .checked_add(ids.len())
                .filter(|total| *total <= MAX_ISOLATED_REFERENCES)
                .ok_or(PdfEditError::MalformedContent)?;
            sharers.push((other, ids));
        }
    }
    if sharers.is_empty() {
        return Ok(());
    }
    let original = doc
        .get_object(stream_id)
        .map_err(|_| PdfEditError::MalformedContent)?
        .clone();
    let copy_id = doc.add_object(original);
    for (other, ids) in sharers {
        let array = Object::Array(
            ids.into_iter()
                .map(|id| Object::Reference(if id == stream_id { copy_id } else { id }))
                .collect(),
        );
        if let Ok(Object::Dictionary(dict)) = doc.get_object_mut(other) {
            dict.set("Contents", array);
        }
    }
    Ok(())
}

/// Whether two spans address the same operand range with the same content digest. `expected_text`
/// is not compared: the digest binds it.
fn spans_equal(a: &PdfSourceSpan, b: &PdfSourceSpan) -> bool {
    a.stream_ref == b.stream_ref
        && a.contents_index == b.contents_index
        && a.operator_index == b.operator_index
        && a.operand_index == b.operand_index
        && a.glyph_start == b.glyph_start
        && a.glyph_end == b.glyph_end
        && a.expected_digest == b.expected_digest
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

/// Which run items an edit replaces, and the new text for them.
struct RunEdit {
    /// `run[first..end]` is replaced; it always contains at least one real glyph.
    first: usize,
    end: usize,
    middle: String,
}

/// Diffs `old` (the island text) against `new` and widens the changed middle to whole run items,
/// so everything outside it - words, kerns and gaps - is left byte for byte.
fn plan_run_edit(run: &[RunItem], old: &[char], new: &[char]) -> Result<RunEdit, PdfEditError> {
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old.len() - prefix
        && suffix < new.len() - prefix
        && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let old_end = old.len() - suffix;

    let mut bounds: Vec<(usize, usize)> = Vec::with_capacity(run.len());
    let mut at = 0;
    for item in run {
        let n = item.text.chars().count();
        bounds.push((at, at + n));
        at += n;
    }
    if at != old.len() {
        return Err(PdfEditError::UnsupportedEncoding);
    }
    let is_glyph = |i: usize| run[i].glyph.is_some();

    let (mut first, mut end);
    if prefix < old_end {
        first = bounds
            .iter()
            .position(|&(s, e)| s <= prefix && prefix < e)
            .ok_or(PdfEditError::UnsupportedEncoding)?;
        end = bounds
            .iter()
            .rposition(|&(s, _)| s < old_end)
            .ok_or(PdfEditError::UnsupportedEncoding)?
            + 1;
    } else {
        // Pure insertion: anchor on the glyph before the cursor, else the glyph after it.
        let before = (prefix > 0)
            .then(|| bounds.iter().position(|&(s, e)| s < prefix && prefix <= e))
            .flatten()
            .filter(|&i| is_glyph(i));
        let after = bounds
            .iter()
            .position(|&(s, e)| s <= prefix && prefix < e)
            .filter(|&i| is_glyph(i));
        let anchor = before.or(after).ok_or(PdfEditError::UnsupportedEncoding)?;
        first = anchor;
        end = anchor + 1;
    }
    if !(first..end).any(is_glyph) {
        if first > 0 && is_glyph(first - 1) {
            first -= 1;
        } else if end < run.len() && is_glyph(end) {
            end += 1;
        } else {
            return Err(PdfEditError::UnsupportedEncoding);
        }
    }
    let region_start = bounds[first].0;
    let region_end = bounds[end - 1].1;
    let middle: String = new[region_start..new.len() - (old.len() - region_end)]
        .iter()
        .collect();
    Ok(RunEdit { first, end, middle })
}

fn gap_sum(item: &RunItem) -> Option<f64> {
    item.gap_numbers
        .as_ref()
        .map(|numbers| numbers.iter().map(|n| n.value).sum())
}

fn number_object(value: f64) -> Object {
    if value.fract() == 0.0 && value.abs() < 1e9 {
        Object::Integer(value as i64)
    } else {
        Object::Real(value as f32)
    }
}

/// Sets the `TJ` number at `number` to 0, keeping the array length (and every other item's index).
fn zero_tj_number(content: &mut Content, number: &GapNumber) -> Result<(), PdfEditError> {
    let op = content
        .operations
        .get_mut(number.operator_index as usize)
        .ok_or(PdfEditError::SourceDigestMismatch)?;
    if op.operator != "TJ" {
        return Err(PdfEditError::SourceDigestMismatch);
    }
    let Some(Object::Array(items)) = op.operands.get_mut(0) else {
        return Err(PdfEditError::SourceDigestMismatch);
    };
    let item = items
        .get_mut(number.item_index as usize)
        .ok_or(PdfEditError::SourceDigestMismatch)?;
    *item = Object::Integer(0);
    Ok(())
}

/// Writes `pieces` (words, in order) into the string holding byte range `[start, end)` of the
/// slot glyph, with `gaps` (TJ numbers) between consecutive pieces. Bytes of that string before
/// and after the range stay as they are. A `Tj` becomes a `TJ` when gaps are needed.
fn write_pieces(
    content: &mut Content,
    slot: &GlyphSourceAddress,
    range: (u32, u32),
    pieces: &[Vec<u8>],
    gaps: &[f64],
) -> Result<(), PdfEditError> {
    if gaps.is_empty() {
        return splice_operand_bytes(
            content,
            slot.operator_index,
            slot.operand_index,
            range.0,
            range.1,
            &pieces[0],
        );
    }
    // Gaps need a TJ array. `'` and `"` carry a line move with them, so they cannot be split.
    let cannot_split = || PdfEditError::MissingGlyphs(vec![' ']);
    let op = content
        .operations
        .get_mut(slot.operator_index as usize)
        .ok_or(PdfEditError::SourceDigestMismatch)?;
    let (bytes, format) = match (op.operator.as_str(), op.operands.first()) {
        ("Tj", Some(Object::String(bytes, format))) => (bytes.clone(), *format),
        ("TJ", Some(Object::Array(items))) => match items.get(slot.operand_index as usize) {
            Some(Object::String(bytes, format)) => (bytes.clone(), *format),
            _ => return Err(PdfEditError::SourceDigestMismatch),
        },
        _ => return Err(cannot_split()),
    };
    let (start, end) = (range.0 as usize, range.1 as usize);
    if start > end || end > bytes.len() {
        return Err(PdfEditError::SourceDigestMismatch);
    }
    let mut new_items: Vec<Object> = Vec::with_capacity(pieces.len() * 2);
    for (k, piece) in pieces.iter().enumerate() {
        let mut text: Vec<u8> = Vec::new();
        if k == 0 {
            text.extend_from_slice(&bytes[..start]);
        }
        text.extend_from_slice(piece);
        if k + 1 == pieces.len() {
            text.extend_from_slice(&bytes[end..]);
        }
        if k > 0 {
            new_items.push(number_object(gaps[k - 1]));
        }
        new_items.push(Object::String(text, format));
    }
    if op.operator == "Tj" {
        op.operator = "TJ".to_string();
        op.operands = vec![Object::Array(new_items)];
    } else if let Some(Object::Array(items)) = op.operands.get_mut(0) {
        let at = slot.operand_index as usize;
        items.splice(at..=at, new_items);
    }
    Ok(())
}

fn apply_exact_operand_patch(
    doc: &mut Document,
    page_id: ObjectId,
    page: u32,
    stream_id: ObjectId,
    island: &PdfTextIsland,
    after: &PdfTextEditState,
    available_bounds: PdfRect,
) -> Result<(), PdfEditError> {
    let layout = island.layout.ok_or(PdfEditError::UnsupportedEncoding)?;
    let old: Vec<char> = island.text.chars().collect();
    let new: Vec<char> = after.text.chars().collect();
    if old == new {
        return Ok(());
    }
    let plan = plan_run_edit(&island.run, &old, &new)?;
    let region = &island.run[plan.first..plan.end];

    // Re-encode through the very table that made the island `editable` (`inspect::ReverseTable`).
    // Nothing is ever substituted: a character the font has no code for is a typed refusal that
    // names it, before any mutation. Only the changed middle is encoded; every other word, kern
    // and gap of the run is left exactly as it is.
    let middle = encode_middle_for_font(doc, page_id, &island.appearance.font_id, &plan.middle)
        .map_err(|failure| match failure {
            ReencodeFailure::Blocked => PdfEditError::UnsupportedEncoding,
            ReencodeFailure::MissingGlyphs(chars) => PdfEditError::MissingGlyphs(chars),
        })?;

    // A word gap in the new middle with no space glyph in the font is regenerated from the gap it
    // replaces (same order), else from the island's first gap: the same TJ numbers, so the
    // original gap width. No reference gap means no faithful space: refuse, never drop it.
    let size = layout.font_size;
    let th = layout.horizontal_scale;
    let gap_text = |value: f64| -(value / 1000.0) * size * th;
    let mut gap_values: Vec<f64> = Vec::new();
    if !middle.space_glyph {
        // Gaps are the only space this font can show, and the page graph reads exactly one gap
        // between neighbouring glyphs. Two spaces in a row (an empty word between them), or a
        // space at either edge of the replaced region, would come back as a different text.
        if middle.pieces.len() > 1 && middle.pieces.iter().any(|piece| piece.char_count() == 0) {
            return Err(PdfEditError::MissingGlyphs(vec![' ']));
        }
        let replaced: Vec<f64> = region.iter().filter_map(gap_sum).collect();
        let reference = island.run.iter().find_map(gap_sum);
        for k in 0..middle.pieces.len().saturating_sub(1) {
            let value = replaced
                .get(k)
                .copied()
                .or(reference)
                .ok_or_else(|| PdfEditError::MissingGlyphs(vec![' ']))?;
            gap_values.push(value);
        }
    }

    // Same number of glyphs in, same number out, no gap involved: write each new code over the
    // old glyph in place. Code widths are equal, so no byte offset or array index moves, and the
    // kerns between glyphs stay where they were (a flattened word would end `sum(kerns)` too far
    // to the right and eat into the gap after it).
    let region_glyphs: Vec<&RunItem> = region.iter().filter(|i| i.glyph.is_some()).collect();
    let code_len = |i: &&RunItem| {
        i.glyph
            .as_ref()
            .map_or(0, |g| (g.glyph_end - g.glyph_start) as usize)
    };
    let positional = !region_glyphs.is_empty()
        && region_glyphs.len() == region.len()
        && region_glyphs.iter().all(|i| i.text.chars().count() == 1)
        && middle.pieces.len() == 1
        && gap_values.is_empty()
        && middle.pieces[0].char_count() == region_glyphs.len()
        && region_glyphs
            .iter()
            .all(|i| code_len(i) * region_glyphs.len() == middle.pieces[0].bytes.len());

    // Fit: the island's length with the region's old extent swapped for the new run's pen length.
    let scale = layout.axis.0.hypot(layout.axis.1);
    if !(scale.is_finite() && scale > 0.0) {
        return Err(PdfEditError::UnsupportedEncoding);
    }
    let advance = |piece: &inspect::EncodedRun| {
        piece.advance(size, layout.char_spacing, layout.word_spacing, th)
    };
    let mut new_region_text: f64 = middle.pieces.iter().map(advance).sum();
    // Between two words the pen also moves by the last glyph's own `Tc` (and `Tw`, when that glyph
    // is single-byte code 32 - a font without a space glyph can still map code 32 to a letter).
    for (piece, value) in middle.pieces.iter().zip(&gap_values) {
        new_region_text += layout.char_spacing * th + gap_text(*value);
        if piece.ends_with_word_space() {
            new_region_text += layout.word_spacing * th;
        }
    }
    let island_len = island.run.last().map_or(0.0, |i| i.end) - island.run[0].start;
    let old_region = region[region.len() - 1].end - region[0].start;
    let region_reaches_island_end = plan.end == island.run.len();
    let new_len_text = if positional {
        // Pen end is unchanged by the in-place write except for the glyph widths themselves and
        // for `Tw`: the single-byte code 32 advances the pen by `Tw` more than any other code, so
        // writing it over another code (or another over it) moves everything after it. This is
        // decided by the *code*, never by the character: code 32 mapped to a letter still takes
        // `Tw`, and a space reached through another code does not. The island's very last glyph
        // has no trailing advance inside the island, so it contributes no `Tw`.
        let old_widths: f64 = region.iter().map(|i| i.end - i.start).sum();
        let new_widths = middle.pieces[0].advance(size, 0.0, 0.0, th);
        let mut tw_delta = 0.0;
        for (k, item) in region_glyphs.iter().enumerate() {
            if region_reaches_island_end && k + 1 == region_glyphs.len() {
                continue;
            }
            let was_space = item.word_space;
            let is_space = middle.pieces[0].word_space_at(k);
            tw_delta += layout.word_spacing * th * (f64::from(is_space) - f64::from(was_space));
        }
        island_len / scale + new_widths - old_widths / scale + tw_delta
    } else {
        // The region's last glyph is followed by its own `Tc`/`Tw`: single-byte code 32 there
        // carries `Tw` across to whatever follows, and the new last code may or may not be it.
        let trailing_tw = if region_reaches_island_end {
            0.0
        } else {
            let was_space = region
                .last()
                .is_some_and(|i| i.glyph.is_some() && i.word_space);
            let is_space = middle
                .pieces
                .last()
                .is_some_and(|piece| piece.ends_with_word_space());
            layout.word_spacing * th * (f64::from(is_space) - f64::from(was_space))
        };
        (island_len - old_region) / scale + new_region_text + trailing_tw
    };
    validate_layout_advance(new_len_text, layout.axis, available_bounds)?;

    let mut content = decode_stream_content(doc, stream_id, &mut ContentBudget::new())?;

    if positional {
        let bytes = &middle.pieces[0].bytes;
        let width = bytes.len() / region_glyphs.len();
        for (item, chunk) in region_glyphs.iter().zip(bytes.chunks(width)) {
            let g = item.glyph.as_ref().expect("filtered to glyphs");
            splice_operand_bytes(
                &mut content,
                g.operator_index,
                g.operand_index,
                g.glyph_start,
                g.glyph_end,
                chunk,
            )?;
        }
        write_stream_content(doc, stream_id, &content)?;
        return verify_written(doc, page, island, &after.text, available_bounds);
    }

    // 1. Gap numbers inside the region are replaced by the regenerated gaps: zero them in place
    //    (array length and every other index unchanged).
    for item in region {
        for number in item.gap_numbers.iter().flatten() {
            zero_tj_number(&mut content, number)?;
        }
    }
    // 2. Region glyphs other than the slot's operand are blanked, last first, so no remaining
    //    byte offset shifts.
    let glyphs: Vec<&GlyphSourceAddress> = region.iter().filter_map(|i| i.glyph.as_ref()).collect();
    let slot = glyphs[0];
    // The region is flattened into one run of new words: every kern that sat between its old
    // glyphs (inside a word, or across the blanked strings) would otherwise survive in front of
    // the new text and push it - and everything after it - sideways. The fit above counts the
    // new region as the new words and the regenerated gaps and nothing else.
    zero_numbers_between(&mut content, slot, glyphs[glyphs.len() - 1])?;
    let same_operand = |g: &GlyphSourceAddress| {
        g.operator_index == slot.operator_index && g.operand_index == slot.operand_index
    };
    for g in glyphs.iter().rev().filter(|g| !same_operand(g)) {
        splice_operand_bytes(
            &mut content,
            g.operator_index,
            g.operand_index,
            g.glyph_start,
            g.glyph_end,
            &[],
        )?;
    }
    // 3. The slot's own bytes become the new words (and gaps).
    let slot_end = glyphs
        .iter()
        .filter(|g| same_operand(g))
        .map(|g| g.glyph_end)
        .max()
        .unwrap_or(slot.glyph_end);
    let pieces: Vec<Vec<u8>> = middle.pieces.iter().map(|p| p.bytes.clone()).collect();
    write_pieces(
        &mut content,
        slot,
        (slot.glyph_start, slot_end),
        &pieces,
        &gap_values,
    )?;
    write_stream_content(doc, stream_id, &content)?;
    verify_written(doc, page, island, &after.text, available_bounds)
}

/// Sets every numeric `TJ` item strictly between glyph `first` and glyph `last` to 0 (array length
/// and every index unchanged). The numbers inside the span a flatten rewrites are the kerns of
/// the text being replaced.
fn zero_numbers_between(
    content: &mut Content,
    first: &GlyphSourceAddress,
    last: &GlyphSourceAddress,
) -> Result<(), PdfEditError> {
    if (last.operator_index, last.operand_index) < (first.operator_index, first.operand_index) {
        return Err(PdfEditError::SourceDigestMismatch);
    }
    for op_index in first.operator_index..=last.operator_index {
        let op = content
            .operations
            .get_mut(op_index as usize)
            .ok_or(PdfEditError::SourceDigestMismatch)?;
        if op.operator != "TJ" {
            continue;
        }
        let Some(Object::Array(items)) = op.operands.get_mut(0) else {
            continue;
        };
        let from = if op_index == first.operator_index {
            first.operand_index as usize + 1
        } else {
            0
        };
        let to = if op_index == last.operator_index {
            (last.operand_index as usize).min(items.len())
        } else {
            items.len()
        };
        for item in items.iter_mut().take(to).skip(from) {
            if matches!(item, Object::Integer(_) | Object::Real(_)) {
                *item = Object::Integer(0);
            }
        }
    }
    Ok(())
}

/// Reads the page back after an in-place write and checks what was actually written against what
/// was asked: the island that starts where the edited one started must read as exactly
/// `wanted` (spaces included - a requested spacing the encoding cannot represent comes back
/// different), and that text must still fit `available`, measured from the pen positions the
/// walker now sees rather than from the arithmetic that predicted them. Any mismatch is a refusal;
/// the caller drops the document, so nothing partial is returned.
fn verify_written(
    doc: &Document,
    page: u32,
    island: &PdfTextIsland,
    wanted: &str,
    available: PdfRect,
) -> Result<(), PdfEditError> {
    if wanted.is_empty() {
        return Ok(());
    }
    let first = island
        .locator
        .spans
        .first()
        .ok_or(PdfEditError::VerificationFailed)?;
    let graph = walk_page_graph(doc, "", page).map_err(|_| PdfEditError::VerificationFailed)?;
    let written = graph
        .islands
        .iter()
        .find(|candidate| {
            candidate.locator.spans.first().is_some_and(|span| {
                span.stream_ref == first.stream_ref
                    && span.contents_index == first.contents_index
                    && span.operator_index == first.operator_index
                    && span.operand_index == first.operand_index
                    && span.glyph_start == first.glyph_start
            })
        })
        .ok_or(PdfEditError::VerificationFailed)?;

    // The run items that make up `wanted`. Anything after them is a neighbour the island may now
    // touch, not part of this edit.
    let wanted_chars = wanted.chars().count();
    let mut text = String::new();
    let mut last_item = None;
    for (index, item) in written.run.iter().enumerate() {
        text.push_str(&item.text);
        if text.chars().count() >= wanted_chars {
            last_item = Some(index);
            break;
        }
    }
    let last_item = last_item.ok_or(PdfEditError::VerificationFailed)?;
    if text != wanted {
        return Err(PdfEditError::VerificationFailed);
    }
    let layout = written.layout.ok_or(PdfEditError::VerificationFailed)?;
    let scale = layout.axis.0.hypot(layout.axis.1);
    if !(scale.is_finite() && scale > 0.0) {
        return Err(PdfEditError::VerificationFailed);
    }
    let extent = written.run[last_item].end - written.run[0].start;
    validate_layout_advance(extent / scale, layout.axis, available)
}

/// Overflow check for an in-place write. `advance` is the replacement's text-space length along
/// the baseline (see `EncodedRun::advance`); `axis` is the device-space vector of one text unit.
/// The projected extents must fit `available`, within [`OVERFLOW_EPSILON_PT`]. The measure is the
/// glyph *advances*, exactly what the island's own box is built from, so a replacement no wider
/// than the original never overflows its own box.
fn validate_layout_advance(
    advance: f64,
    axis: (f64, f64),
    available: PdfRect,
) -> Result<(), PdfEditError> {
    let width = (advance * axis.0).abs();
    let height = (advance * axis.1).abs();
    if !(width.is_finite() && height.is_finite())
        || width > available.width + OVERFLOW_EPSILON_PT
        || height > available.height + OVERFLOW_EPSILON_PT
    {
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
    page: u32,
    page_id: ObjectId,
    stream_id: ObjectId,
    island: &PdfTextIsland,
    after: &PdfTextEditState,
    available_bounds: PdfRect,
    fonts: &dyn PdfFontResolver,
) -> Result<(Isolated, Option<AppendedRun>), PdfEditError> {
    let resolved = fonts.resolve(&after.appearance.font_id)?;
    // Validates full glyph coverage up front (§7.3 rule 3: "fail before candidate creation").
    fonts.glyphs_for_text(&resolved, &after.text)?;

    // The new block runs in the default graphics state, so it must carry the source run's whole
    // placement itself (see `build_text_object_operations`). A matrix that cannot place text is a
    // refusal, never a guess.
    let layout = island.layout.ok_or(PdfEditError::UnsupportedEncoding)?;
    let placement = layout.placement;
    let finite = [
        placement.a,
        placement.b,
        placement.c,
        placement.d,
        placement.e,
        placement.f,
    ]
    .iter()
    .all(|value| value.is_finite());
    let scale = layout.axis.0.hypot(layout.axis.1);
    if !(finite && scale > 0.0 && after.appearance.size_pt.is_finite()) {
        return Err(PdfEditError::UnsupportedTransform);
    }

    // Fit, in the units the block is drawn in: the text-space length of the new advances
    // (`Tf` size x advance / 1000; `Tz`, `Tc` and `Tw` are reset to defaults) is carried to device
    // space by the placement's axis, exactly as the in-place path measures. `Tf` is text-space
    // here, so the matrix's scale is applied once, by `validate_layout_advance`, and not in `Tf`.
    let advance_1000 = measure_advance_1000(&resolved, &after.text);
    validate_layout_advance(
        advance_1000 / 1000.0 * after.appearance.size_pt,
        layout.axis,
        available_bounds,
    )?;

    // Every removed glyph and the `TJ` number that keeps the pen where the glyph left it. Worked
    // out before anything is mutated, so a glyph whose advance cannot be kept is a refusal that
    // leaves the document alone.
    let removed = removed_glyphs(island)?;

    let (font_obj_id, cid_codes) = embed_type0_font(doc, &resolved, &after.text)?;
    let resource_name = add_font_resource(doc, page_id, font_obj_id)?;

    let mut content = decode_stream_content(doc, stream_id, &mut ContentBudget::new())?;
    remove_glyphs_keeping_pen(&mut content, removed)?;
    write_stream_content(doc, stream_id, &content)?;

    // §9.2 step 4/5: the new run goes in its own stream at the end of the page's `/Contents`,
    // outside the original content's graphics state. Appended inside the edited stream it would
    // inherit that stream's clip, alpha and blend mode, and a clip cannot be reset from inside.
    // The stream is installed with the rest of the batch (see `install_isolated_content`).
    let operations =
        build_text_object_operations(&resource_name, &after.appearance, placement, &cid_codes);
    // A run with no text shows no glyph, so there is no first glyph to find on read-back.
    let run = (!after.text.is_empty()).then_some(AppendedRun {
        page,
        stream: None,
        placement,
        size: after.appearance.size_pt,
    });
    Ok(((page_id, operations), run))
}

/// What the composed page must show for one appended replacement run, worked out from the island
/// it replaces and not from the operations that were emitted, so that a mistake in emitting them
/// is caught.
struct AppendedRun {
    page: u32,
    /// The content stream the block was installed as; set once the batch's streams exist. The
    /// run's glyphs are the ones the walker attributes to this stream: an island they were grouped
    /// into may also hold a neighbour's glyphs (the join rule does not compare fonts), so the
    /// island is not the unit to check.
    stream: Option<ObjectId>,
    /// `text matrix x CTM` of the source run's first glyph: where the block's first glyph must be.
    placement: Matrix,
    /// The `Tf` size the block uses, in the text-space units of `placement`.
    size: f64,
}

/// How far, in device points, a replacement's first glyph origin may sit from the source's.
const APPENDED_ORIGIN_TOLERANCE_PT: f64 = 0.5;

/// How far, relative to the expected length, the replacement's em vectors (one `Tf` size along
/// text x and along text y, in device space) may differ from the source's. Covers size, rotation
/// and skew together.
const APPENDED_SIZE_TOLERANCE: f64 = 0.02;

/// Reads the composed page back and refuses the batch unless every appended replacement run sits
/// where the run it replaced sat and is drawn as large and as turned. The text of the run is not
/// compared here (the save-side recheck and the structural walk already do that); this is the
/// geometry, which a structural read-back cannot see: a run drawn at the page origin, ten times
/// too large, reads back as exactly the requested text.
///
/// The run is found by its own content stream, not by the island it was grouped into: the join
/// rule does not compare fonts, so a run that starts within the join window of a preceding
/// same-colour run on the same baseline is part of that run's island, whose first glyph is not
/// the replacement's. The first glyph the walker attributes to the run's stream is compared.
fn verify_appended_runs(doc: &Document, runs: &[AppendedRun]) -> Result<(), PdfEditError> {
    use std::collections::btree_map::Entry;
    let mut graphs: std::collections::BTreeMap<u32, PdfPageGraph> =
        std::collections::BTreeMap::new();
    for run in runs {
        let stream = run.stream.ok_or(PdfEditError::VerificationFailed)?;
        let graph = match graphs.entry(run.page) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(
                walk_page_graph(doc, "", run.page).map_err(|_| PdfEditError::VerificationFailed)?,
            ),
        };
        let written = graph
            .islands
            .iter()
            .flat_map(|island| island.run.iter())
            .find(|item| {
                item.glyph
                    .is_some_and(|glyph| glyph.stream_ref == (stream.0, stream.1))
            })
            .and_then(|item| item.placement)
            .ok_or(PdfEditError::VerificationFailed)?;
        if !appended_run_placed(run, written) {
            return Err(PdfEditError::VerificationFailed);
        }
    }
    Ok(())
}

/// Whether the first glyph the walker now sees for the run (`written`: its text matrix x CTM and
/// `Tf` size) is where `run` says it must be: origin within [`APPENDED_ORIGIN_TOLERANCE_PT`], and
/// the device-space vectors of one `Tf` size along text x and along text y each within
/// [`APPENDED_SIZE_TOLERANCE`] of the expected ones. Text rise is not part of either matrix, so it
/// is compared on neither side.
fn appended_run_placed(run: &AppendedRun, written: (Matrix, f64)) -> bool {
    let (want, (have, have_size)) = (run.placement, written);
    let origin = (have.e - want.e).hypot(have.f - want.f);
    let em_x = |m: Matrix, size: f64| (m.a * size, m.b * size);
    let em_y = |m: Matrix, size: f64| (m.c * size, m.d * size);
    let near = |want: (f64, f64), have: (f64, f64)| {
        (have.0 - want.0).hypot(have.1 - want.1) <= APPENDED_SIZE_TOLERANCE * want.0.hypot(want.1)
    };
    origin <= APPENDED_ORIGIN_TOLERANCE_PT
        && near(em_x(want, run.size), em_x(have, have_size))
        && near(em_y(want, run.size), em_y(have, have_size))
}

/// One glyph an edit removes from a string operand, with the `TJ` number that keeps the pen where
/// the glyph left it.
struct RemovedGlyph {
    operator_index: u32,
    operand_index: u32,
    start: u32,
    end: u32,
    adjust: f64,
}

/// The glyphs of `island`, in reading order, each with its pen-keeping `TJ` number. A glyph whose
/// advance cannot be expressed that way (a zero font size) is refused.
fn removed_glyphs(island: &PdfTextIsland) -> Result<Vec<RemovedGlyph>, PdfEditError> {
    let mut removed = Vec::new();
    for item in &island.run {
        let Some(glyph) = &item.glyph else { continue };
        removed.push(RemovedGlyph {
            operator_index: glyph.operator_index,
            operand_index: glyph.operand_index,
            start: glyph.glyph_start,
            end: glyph.glyph_end,
            adjust: item.pen_adjust.ok_or(PdfEditError::UnsupportedEncoding)?,
        });
    }
    Ok(removed)
}

/// Replaces each removed glyph by the `TJ` number that moves the pen exactly as the glyph did, so
/// every glyph shown after it - in this operator or any later one - stays where it was.
///
/// A `TJ` string item is split around the removed ranges (items after it shift, which is why
/// commands run back to front); a `Tj` becomes a `TJ` at the same operator index, so no operator
/// is ever renumbered. `'` and `"` carry a line move a `TJ` cannot, so glyphs shown by them are
/// refused rather than left with the pen short. Operators are rewritten last to first and, inside
/// one `TJ`, items last to first, so no address still to be rewritten has moved.
fn remove_glyphs_keeping_pen(
    content: &mut Content,
    mut removed: Vec<RemovedGlyph>,
) -> Result<(), PdfEditError> {
    removed.sort_by_key(|g| (g.operator_index, g.operand_index, g.start));
    let mut end = removed.len();
    while end > 0 {
        let operator_index = removed[end - 1].operator_index;
        let mut begin = end;
        while begin > 0 && removed[begin - 1].operator_index == operator_index {
            begin -= 1;
        }
        let glyphs = &removed[begin..end];
        let op = content
            .operations
            .get_mut(operator_index as usize)
            .ok_or(PdfEditError::SourceDigestMismatch)?;
        let operator = op.operator.clone();
        match operator.as_str() {
            "TJ" => {
                let Some(Object::Array(items)) = op.operands.get_mut(0) else {
                    return Err(PdfEditError::SourceDigestMismatch);
                };
                let mut hi = glyphs.len();
                while hi > 0 {
                    let item_index = glyphs[hi - 1].operand_index;
                    let mut lo = hi;
                    while lo > 0 && glyphs[lo - 1].operand_index == item_index {
                        lo -= 1;
                    }
                    let replacement = match items.get(item_index as usize) {
                        Some(Object::String(bytes, format)) => {
                            pieces_around_removed(bytes, *format, &glyphs[lo..hi])?
                        }
                        _ => return Err(PdfEditError::SourceDigestMismatch),
                    };
                    let at = item_index as usize;
                    items.splice(at..=at, replacement).for_each(drop);
                    hi = lo;
                }
            }
            "Tj" => {
                let replacement = match op.operands.first() {
                    Some(Object::String(bytes, format)) => {
                        pieces_around_removed(bytes, *format, glyphs)?
                    }
                    _ => return Err(PdfEditError::SourceDigestMismatch),
                };
                op.operator = "TJ".to_string();
                op.operands = vec![Object::Array(replacement)];
            }
            other if is_line_moving_show(other) => return Err(PdfEditError::UnsupportedEncoding),
            _ => return Err(PdfEditError::SourceDigestMismatch),
        }
        end = begin;
    }
    Ok(())
}

/// `bytes` with each removed range (ascending, non-overlapping) replaced by its pen-keeping `TJ`
/// number; the bytes between and after the ranges stay as strings of the same format.
fn pieces_around_removed(
    bytes: &[u8],
    format: StringFormat,
    glyphs: &[RemovedGlyph],
) -> Result<Vec<Object>, PdfEditError> {
    let mut out = Vec::with_capacity(glyphs.len() * 2 + 1);
    let mut at = 0usize;
    for glyph in glyphs {
        let (start, end) = (glyph.start as usize, glyph.end as usize);
        if start < at || end < start || end > bytes.len() {
            return Err(PdfEditError::SourceDigestMismatch);
        }
        if start > at {
            out.push(Object::String(bytes[at..start].to_vec(), format));
        }
        out.push(number_object(glyph.adjust));
        at = end;
    }
    if at < bytes.len() {
        out.push(Object::String(bytes[at..].to_vec(), format));
    }
    Ok(out)
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
) -> Result<(u32, Option<Isolated>), PdfEditError> {
    if doc.is_encrypted() {
        return Err(PdfEditError::EncryptedDocument);
    }
    let page_id = pages_lookup(doc, page)?;

    let resolved = fonts.resolve(&appearance.font_id)?;
    fonts.glyphs_for_text(&resolved, text)?;

    let (font_obj_id, cid_codes) = embed_type0_font(doc, &resolved, text)?;
    let resource_name = add_font_resource(doc, page_id, font_obj_id)?;

    Ok((
        page,
        Some((
            page_id,
            // Add Text gives a point in page space and an upright size; the block runs in the
            // default state, so the matrix is the plain translation.
            build_text_object_operations(
                &resource_name,
                appearance,
                Matrix::translation(anchor.x, anchor.y),
                &cid_codes,
            ),
        )),
    ))
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
) -> Result<(u32, Option<Isolated>), PdfEditError> {
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

    Ok((
        page,
        Some((
            page_id,
            build_ocr_correction_operations(
                &resource_name,
                appearance,
                rect,
                cover_color,
                &cid_codes,
            ),
        )),
    ))
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
    let mut seen = std::collections::HashSet::new();
    for _ in 0..64 {
        if !seen.insert(page_id) {
            return None; // a /Parent cycle
        }
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
    let mut seen = std::collections::HashSet::new();
    for _ in 0..64 {
        if !seen.insert(page_id) {
            return None; // a /Parent cycle
        }
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

/// Most unbalanced `Q` operators the original content may carry before the page is refused.
const MAX_STRAY_RESTORES: i64 = 1 << 16;

/// Installs the batch's new blocks, each as its own content stream **isolated from the page's
/// original content**, once every command has been applied. Blocks go on a page in command order
/// (`blocks` carries each one's command slot).
///
/// Graphics state carries from one content stream into the next (ISO 32000-1 7.8.2), so a
/// clipping path (`W n`), an `ExtGState` with `/ca 0` or a blend mode, or a leftover CTM in the
/// original content applies to anything appended after it: the added text or OCR cover would be
/// invisible while a structural read-back still succeeded. Resetting parameters from inside the
/// new block cannot undo a clip. So the original streams are wrapped: a stream of `q` goes first
/// and a stream of `Q` follows the last original stream, and the new streams run in the default
/// state. The wrapper is sized from the original content's own balance - one extra `q` for each
/// stray `Q` the original would otherwise use to pop the wrapper, and enough `Q` to close every
/// `q` the original left open - so a new stream always starts at nesting depth zero. Each block is
/// itself balanced, so the blocks of one page do not affect each other.
///
/// Installing at the end of the batch, not per command, is what keeps the originals' addresses
/// still: the wrapper stream is prepended, which shifts every original `contents_index`, and the
/// island digest binds it.
///
/// Original streams are not modified (§9.3: "Do not mutate existing content operators"). Content
/// that cannot be parsed strictly cannot be counted, so it is refused as malformed; the count is
/// charged to a [`ContentBudget`] before anything is decoded or allocated. A stream that merely
/// ends in the operands of an operator that opens the next stream is not malformed (ISO 32000-1
/// 7.8.2 cuts at token boundaries, not operation boundaries) and is counted as it stands.
///
/// Returns the content stream each block became, by the block's command slot.
fn install_isolated_content(
    doc: &mut Document,
    mut blocks: Vec<(usize, Isolated)>,
) -> Result<std::collections::BTreeMap<usize, ObjectId>, PdfEditError> {
    blocks.sort_by_key(|(slot, _)| *slot);
    let mut by_page: std::collections::BTreeMap<ObjectId, Vec<(usize, Vec<Operation>)>> =
        std::collections::BTreeMap::new();
    for (slot, (page_id, operations)) in blocks {
        by_page.entry(page_id).or_default().push((slot, operations));
    }
    let mut streams = std::collections::BTreeMap::new();
    for (page_id, operations) in by_page {
        streams.extend(isolate_page_content(doc, page_id, operations)?);
    }
    Ok(streams)
}

fn isolate_page_content(
    doc: &mut Document,
    page_id: ObjectId,
    blocks: Vec<(usize, Vec<Operation>)>,
) -> Result<Vec<(usize, ObjectId)>, PdfEditError> {
    let page_obj = doc
        .get_object(page_id)
        .map_err(|_| PdfEditError::MalformedContent)?;
    let page_dict = page_obj
        .as_dict()
        .map_err(|_| PdfEditError::MalformedContent)?;

    // `/Contents` may be an indirect reference to an array of streams; expand it so the new page
    // array lists streams, never the array object itself.
    let ids: Vec<ObjectId> = match page_dict.get(b"Contents") {
        Ok(contents) => content_stream_ids(doc, contents)?,
        Err(_) => return Err(PdfEditError::MalformedContent),
    };

    // Nesting balance of the original content: the lowest point it reaches below its start
    // (stray `Q`) and where it ends (unclosed `q`). One budget for the page: a stream listed many
    // times is decoded and charged many times.
    let mut budget = ContentBudget::new();
    let mut net: i64 = 0;
    let mut lowest: i64 = 0;
    for id in &ids {
        // Read, never written back, so a stream may end in the operands of an operator that opens
        // the next one (see `ContentBudget::content_to_read`).
        for operation in budget.content_to_read(doc, *id)?.operations {
            match operation.operator.as_str() {
                "q" => net += 1,
                "Q" => {
                    net -= 1;
                    lowest = lowest.min(net);
                }
                _ => {}
            }
        }
    }
    // Both the stray `Q`s and the unclosed `q`s become operations of the wrapper streams, so both
    // are bounded before those are built.
    if -lowest > MAX_STRAY_RESTORES || net > MAX_STRAY_RESTORES {
        return Err(PdfEditError::MalformedContent);
    }
    let opening = 1 - lowest; // the wrapper's own `q` plus one per stray `Q`
    let closing = opening + net; // every level still open when the original ends

    let repeated = |operator: &str, count: i64| -> Vec<Operation> {
        (0..count)
            .map(|_| Operation::new(operator, vec![]))
            .collect()
    };
    let mut add_stream = |operations: Vec<Operation>| -> Result<ObjectId, PdfEditError> {
        let mut content = Content::decode(&[]).map_err(|_| PdfEditError::MalformedContent)?;
        content.operations = operations;
        let encoded = content
            .encode()
            .map_err(|_| PdfEditError::MalformedContent)?;
        Ok(doc.add_object(Stream::new(dictionary! {}, encoded)))
    };
    let prefix_id = add_stream(repeated("q", opening))?;
    let suffix_id = add_stream(repeated("Q", closing))?;
    let mut added = Vec::with_capacity(blocks.len());
    let mut slots = Vec::with_capacity(blocks.len());
    for (slot, operations) in blocks {
        added.push(add_stream(operations)?);
        slots.push(slot);
    }
    let installed: Vec<(usize, ObjectId)> = slots.into_iter().zip(added.iter().copied()).collect();

    let mut all: Vec<ObjectId> = Vec::with_capacity(ids.len() + 2 + added.len());
    all.push(prefix_id);
    all.extend(ids);
    all.push(suffix_id);
    all.extend(added);
    let array = Object::Array(all.into_iter().map(Object::Reference).collect());
    if let Ok(Object::Dictionary(dict)) = doc.get_object_mut(page_id) {
        dict.set("Contents", array);
    }
    Ok(installed)
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

/// Deepest `/Parent` chain followed when looking for the `/Resources` a page inherits. Real page
/// trees are a handful of levels; the same bound as inspection's inherited-attribute lookup.
const MAX_RESOURCE_INHERIT_DEPTH: usize = 64;

/// The page's *effective* `/Resources`, cloned: the nearest `/Resources` entry on the page itself
/// or, when it has none, on the closest ancestor `/Pages` node that does (a page attribute is
/// inherited from there, ISO 32000-1 7.7.3.4). The nearest one is used whole, never merged with
/// a farther one - exactly what a viewer reads. An empty dictionary means nothing is inherited.
/// A `/Parent` cycle or a chain deeper than [`MAX_RESOURCE_INHERIT_DEPTH`] is refused: the
/// resources it hides cannot be preserved, so nothing may be written over them.
fn effective_resources(doc: &Document, page_id: ObjectId) -> Result<Dictionary, PdfEditError> {
    let mut seen = std::collections::HashSet::new();
    let mut id = page_id;
    for _ in 0..=MAX_RESOURCE_INHERIT_DEPTH {
        if !seen.insert(id) {
            return Err(PdfEditError::MalformedContent); // a /Parent cycle
        }
        let node = doc
            .get_object(id)
            .map_err(|_| PdfEditError::MalformedContent)?
            .as_dict()
            .map_err(|_| PdfEditError::MalformedContent)?;
        if node.has(b"Resources") {
            return Ok(dict_entry_clone(doc, node, b"Resources"));
        }
        match node.get(b"Parent") {
            Ok(Object::Reference(parent)) => id = *parent,
            _ => return Ok(dictionary! {}), // the chain ends: nothing to inherit
        }
    }
    Err(PdfEditError::MalformedContent) // longer than any real page tree
}

/// Adds `font_obj_id` to page `page_id`'s own `/Resources/Font` dictionary under a fresh,
/// collision-free name (§9.3: "Add resource under collision-free generated name"). Always writes a
/// page-owned copy of `/Resources` (and of `/Resources/Font`) rather than mutating whatever
/// dictionary object the page's `/Resources` currently points to, so a `/Resources` dictionary
/// shared with another page is never silently changed for both. The copy starts from the page's
/// effective resources ([`effective_resources`]), so a page that inherits `/Resources` from an
/// ancestor keeps every font, XObject and graphics state it already resolved.
fn add_font_resource(
    doc: &mut Document,
    page_id: ObjectId,
    font_obj_id: ObjectId,
) -> Result<Vec<u8>, PdfEditError> {
    let existing_resources = effective_resources(doc, page_id)?;
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

/// One isolated `q / BT / Tf / Tr Tc Tw Tz Ts / rg / Tm / Tj / ET / Q` block showing `cid_codes`
/// (Identity-H, 2-byte-per-code) through `resource_name`, in
/// `appearance.color`/`appearance.size_pt` - the shared building block for §9.2's appended
/// local rewrite, §9.3's Add Text stream and the OCR correction text.
///
/// `placement` is the block's whole `Tm`, all six numbers, and the block runs in the default
/// graphics state (identity CTM, see [`install_isolated_content`]), so it is also the matrix that
/// takes text space to page space. The text rendering matrix of the first glyph is
/// `[size 0 0; 0 size 0; 0 0 1] x Tm x CTM` (ISO 32000-1 9.4.4; row vectors, `Matrix::then`
/// applies its left operand first). A caller that replaces a source run passes that run's
/// `Tm_source x CTM_source` with `Tf` kept at the source's text-space size: the products are then
/// equal, rotation, skew, negative and non-uniform scale included, whether the producer put the
/// run's position in `Tm`, `Td` or a `cm` outside the text object. Add Text and the OCR text pass
/// an upright `Matrix::translation(x, y)`: the point is already in page space and the size in
/// points.
///
/// Not reproduced: a source run that relied on horizontal scaling (`Tz`) or text rise (`Ts`)
/// draws without them, as this block resets both (and `Tc`, `Tw`, the render mode) to defaults;
/// the replacement is also measured without them.
///
/// The `q`/`Q` pair keeps this block's state from leaking out, but it cannot keep the page's
/// state from leaking in: the text state parameters a page's last text object left behind (`3 Tr`
/// is invisible text, a stray `Tc`/`Tw`/`Tz`/`Ts` moves or scales it) carry into a stream
/// appended after it. So every parameter that changes how the string is drawn is set explicitly.
/// What a block cannot reset - a clipping path, an `ExtGState` alpha or blend mode, a CTM - is
/// removed by [`install_isolated_content`], which closes the original content's graphics state
/// before the block runs.
fn build_text_object_operations(
    resource_name: &[u8],
    appearance: &PdfTextAppearance,
    placement: Matrix,
    cid_codes: &[u16],
) -> Vec<Operation> {
    let mut hex_bytes = Vec::with_capacity(cid_codes.len() * 2);
    for code in cid_codes {
        hex_bytes.extend_from_slice(&code.to_be_bytes());
    }

    let mut operations = vec![Operation::new("q", vec![])];
    operations.extend([
        Operation::new("BT", vec![]),
        Operation::new(
            "Tf",
            vec![
                Object::Name(resource_name.to_vec()),
                Object::Real(appearance.size_pt as f32),
            ],
        ),
        // Fill, no extra character or word spacing, 100 % width, no rise.
        Operation::new("Tr", vec![Object::Integer(0)]),
        Operation::new("Tc", vec![Object::Integer(0)]),
        Operation::new("Tw", vec![Object::Integer(0)]),
        Operation::new("Tz", vec![Object::Integer(100)]),
        Operation::new("Ts", vec![Object::Integer(0)]),
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
            [
                placement.a,
                placement.b,
                placement.c,
                placement.d,
                placement.e,
                placement.f,
            ]
            .map(number_object)
            .to_vec(),
        ),
        Operation::new(
            "Tj",
            vec![Object::String(hex_bytes, StringFormat::Hexadecimal)],
        ),
        Operation::new("ET", vec![]),
        Operation::new("Q", vec![]),
    ]);
    operations
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
    let mut operations = vec![Operation::new("q", vec![])];
    operations.extend([
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
    ]);
    // `rect` is in page space and the block runs in the default state: no text-space value is
    // involved, so the upright translation is exact.
    operations.extend(build_text_object_operations(
        resource_name,
        appearance,
        Matrix::translation(rect.x, rect.y + rect.height * 0.18),
        cid_codes,
    ));
    operations
}

// -------------------------------------------------------------------------------------------
// Content-stream mutation helpers shared by §9.1/§9.2
// -------------------------------------------------------------------------------------------

fn decode_stream_content(
    doc: &Document,
    id: ObjectId,
    budget: &mut ContentBudget,
) -> Result<Content, PdfEditError> {
    // Strict: the lenient decoder silently drops everything after the first byte it cannot
    // parse, and a stream rewritten from a truncated parse would permanently lose that tail. Both
    // the decode and the parse are charged to `budget` before they allocate.
    budget.content(doc, id, true)
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
