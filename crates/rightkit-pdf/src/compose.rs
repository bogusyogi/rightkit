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
//! - **Strategy selection.** A `ReplaceSourceText` command takes
//!   [`WriteStrategy::ExactOperandPatch`] when (a) `before` and `after` appearance are identical
//!   (same font/size/color) and (b) the page graph reports the island `editable`, i.e. its own font
//!   can re-encode text (`inspect::ReverseTable`, one decision shared with the graph) and all its
//!   glyphs share one font, size and spacing. The island may span several operands (kerned `TJ`
//!   text): the whole replacement is written into the earliest span and the later spans are
//!   blanked. Text the font cannot encode is refused with `PdfEditError::MissingGlyphs` naming the
//!   characters; nothing is ever substituted. Every other digest-verified command falls through
//!   to [`WriteStrategy::LocalTextObjectRewrite`] (a catalog font). `AddText` always uses
//!   [`WriteStrategy::AppendTextStream`].
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

// -------------------------------------------------------------------------------------------
// Tests (§14.1 composition fixtures)
// -------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Object as LObject};

    use crate::inspect::simple_font_identity_bytes;
    use crate::model::{GlyphId, PdfColor, PdfEditCommand, PdfOcrBackgroundProof, PdfPageGraph};

    // -----------------------------------------------------------------------------------
    // Fixture builders — shared with the rest of the crate via `crate::test_fixtures`.
    // -----------------------------------------------------------------------------------

    use crate::test_fixtures::{build_one_page_pdf as build_pdf, pdf_bytes as doc_bytes};

    /// [`super::decode_stream_content`] with a fresh page budget, for reading results back.
    fn decode_stream_content(doc: &Document, id: ObjectId) -> Result<Content, PdfEditError> {
        super::decode_stream_content(doc, id, &mut ContentBudget::new())
    }

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
    // -----------------------------------------------------------------------------------
    // Honest editability, typed missing glyphs, exact overflow, multi-span and CID writes
    // -----------------------------------------------------------------------------------

    use crate::test_fixtures::{
        custom_page_pdf, differences_encoding, helvetica_font, identity_h_font, subset_type1_font,
    };

    /// Axis-aligned bounds of an island's glyph quads: exactly what a UI derives for "the
    /// island's own box".
    fn island_box(island: &crate::model::PdfTextIsland) -> PdfRect {
        let mut min_x = f64::MAX;
        let mut min_y = f64::MAX;
        let mut max_x = f64::MIN;
        let mut max_y = f64::MIN;
        for quad in &island.quads {
            for corner in 0..4 {
                min_x = min_x.min(quad[corner * 2]);
                max_x = max_x.max(quad[corner * 2]);
                min_y = min_y.min(quad[corner * 2 + 1]);
                max_y = max_y.max(quad[corner * 2 + 1]);
            }
        }
        PdfRect {
            x: min_x,
            y: min_y,
            width: max_x - min_x,
            height: max_y - min_y,
        }
    }

    const PAGE_WIDE: PdfRect = PdfRect {
        x: 0.0,
        y: 0.0,
        width: 612.0,
        height: 792.0,
    };

    #[test]
    fn same_glyph_swap_fits_the_islands_own_box() {
        // Ledger #93: "Page 1: external link below" -> "Page 1: xternale link below" (same length,
        // same glyph multiset) was `text_overflow` in the island's own box and fine page-wide,
        // because the old fit check guessed half an em per character (27 x 12 = 324 pt) against a
        // box that is 286.8 pt of real Helvetica advances.
        let source = include_bytes!("../tests/fixtures/proof-text-50page.pdf");
        let doc = Document::load_mem(source).expect("fixture");
        let sha = document_sha256(source);
        let island = locate_island(&doc, &sha, 1, "Page 1: external link below");
        assert!(island.editable, "{:?}", island.edit_blocker);
        let own = island_box(&island);
        assert!((own.width - 286.8).abs() < 0.01, "{}", own.width);

        for (label, bounds) in [("own box", own), ("page wide", PAGE_WIDE)] {
            let command = replace_cmd(
                "swap",
                &island,
                "Page 1: xternale link below",
                island.appearance.clone(),
                bounds,
            );
            let candidate = compose_candidate(source, &[command], &FailingFontResolver)
                .unwrap_or_else(|error| panic!("{label}: {error}"));
            let (_, graph) = reinspect(&candidate, 1);
            assert!(
                graph
                    .islands
                    .iter()
                    .any(|isl| isl.text == "Page 1: xternale link below"),
                "{label}"
            );
        }

        // The check is still real: one point narrower than the true width overflows.
        let narrow = PdfRect {
            width: own.width - 1.0,
            ..own
        };
        let command = replace_cmd(
            "swap",
            &island,
            "Page 1: xternale link below",
            island.appearance.clone(),
            narrow,
        );
        assert!(matches!(
            compose_candidate(source, &[command], &FailingFontResolver),
            Err(PdfEditError::TextOverflow)
        ));
    }

    #[test]
    fn narrower_replacement_never_overflows_its_own_box_under_character_and_word_spacing() {
        let doc = build_pdf(b"BT /F1 12 Tf 1 Tc 3 Tw 72 700 Td (a b c) Tj ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "a b c");
        let own = island_box(&island);
        // Same widths in a different order: the spacing terms must match the walker's exactly.
        let command = replace_cmd("order", &island, "c b a", island.appearance.clone(), own);
        compose_candidate(&source, &[command], &FailingFontResolver)
            .expect("same advance fits its own box");
    }

    fn subset_with_differences(first_widths: &[i64]) -> Document {
        let encoding = differences_encoding(
            None,
            true,
            vec![
                LObject::Integer(65),
                LObject::Name(b"A".to_vec()),
                LObject::Name(b"B".to_vec()),
            ],
        );
        custom_page_pdf(
            |d| {
                vec![(
                    "F1",
                    subset_type1_font(d, 65, first_widths, Some(encoding), None),
                )]
            },
            &[b"BT /F1 12 Tf 72 700 Td (AB) Tj ET"],
            false,
        )
    }

    #[test]
    fn missing_glyph_error_names_every_character_the_subset_lacks() {
        let doc = subset_with_differences(&[500, 600, 700]);
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "AB");
        assert!(island.editable);

        let command = replace_cmd(
            "miss",
            &island,
            "ABC!C",
            island.appearance.clone(),
            PAGE_WIDE,
        );
        let error = compose_candidate(&source, &[command], &FailingFontResolver)
            .expect_err("missing glyphs are refused");
        assert_eq!(error.missing_chars(), Some(&['C', '!'][..]));
        assert_eq!(error.code(), "missing_glyph");
        assert!(error.to_string().contains("'C', '!'"), "{error}");
    }

    #[test]
    fn graph_editable_and_compose_agree_for_every_island() {
        // F1: Helvetica (re-encodable). F2: an embedded symbolic subset with a built-in encoding
        // (never re-encodable). Replacing an island with its own text, same appearance, in its own
        // box must succeed exactly when the graph says `editable`.
        let doc = custom_page_pdf(
            |d| {
                vec![
                    ("F1", helvetica_font(d)),
                    ("F2", subset_type1_font(d, 65, &[500, 600], None, None)),
                ]
            },
            &[b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET BT /F2 12 Tf 72 600 Td (AB) Tj ET"],
            false,
        );
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let graph = walk_page_graph(&doc, &sha, 1).expect("graph");
        assert_eq!(graph.islands.len(), 2);
        for island in &graph.islands {
            let command = replace_cmd(
                "same",
                island,
                &island.text,
                island.appearance.clone(),
                island_box(island),
            );
            let outcome = compose_candidate(&source, &[command], &FailingFontResolver);
            if island.editable {
                assert!(outcome.is_ok(), "{}: {:?}", island.text, outcome.err());
            } else {
                assert!(
                    matches!(outcome, Err(PdfEditError::UnsupportedEncoding)),
                    "{}",
                    island.text
                );
            }
        }
        assert_eq!(
            graph.islands.iter().filter(|i| i.editable).count(),
            1,
            "exactly the Helvetica island is editable"
        );
    }

    #[test]
    fn kerned_tj_replacement_writes_one_run_into_the_first_span() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td [(Hel) -20 (lo)] TJ ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "Hello");
        assert_eq!(island.locator.spans.len(), 2);

        let command = replace_cmd(
            "kern",
            &island,
            "Help!",
            island.appearance.clone(),
            PAGE_WIDE,
        );
        let candidate = compose_candidate(&source, &[command], &FailingFontResolver)
            .expect("multi-operand island composes in place");
        let (out, graph) = reinspect(&candidate, 1);
        assert!(graph.islands.iter().any(|isl| isl.text == "Help!"));
        assert!(!graph.islands.iter().any(|isl| isl.text.contains("lo")));

        // The numeric adjustment survives; the second string is blanked, not removed.
        let page_id = out.get_pages()[&1];
        let stream_id = content_stream_ids(
            &out,
            out.get_object(page_id)
                .expect("page")
                .as_dict()
                .expect("dict")
                .get(b"Contents")
                .expect("contents"),
        )
        .expect("ids")[0];
        let content = decode_stream_content(&out, stream_id).expect("content");
        let tj = content
            .operations
            .iter()
            .find(|op| op.operator == "TJ")
            .expect("TJ kept");
        let Some(Object::Array(items)) = tj.operands.first() else {
            panic!("TJ array");
        };
        // Only the changed tail is rewritten; "Hel" and the kern in front of it are untouched.
        assert!(matches!(&items[0], Object::String(b, _) if b == b"Hel"));
        assert!(matches!(&items[1], Object::Integer(-20)));
        assert!(matches!(&items[2], Object::String(b, _) if b == b"p!"));
    }

    #[test]
    fn adjacent_tj_operators_replace_in_place_when_the_font_can_encode() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Hello) Tj (World) Tj ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "HelloWorld");
        assert_eq!(island.locator.spans.len(), 2);
        let command = replace_cmd(
            "two-tj",
            &island,
            "Hi there",
            island.appearance.clone(),
            PAGE_WIDE,
        );
        let candidate = compose_candidate(&source, &[command], &FailingFontResolver)
            .expect("two Tj operators compose in place");
        let (_, graph) = reinspect(&candidate, 1);
        assert!(graph.islands.iter().any(|isl| isl.text == "Hi there"));
        assert!(!graph.islands.iter().any(|isl| isl.text.contains("World")));
    }

    #[test]
    fn identity_h_island_is_rewritten_with_two_byte_codes_and_refuses_unknown_characters() {
        let doc = custom_page_pdf(
            |d| {
                vec![(
                    "F1",
                    identity_h_font(
                        d,
                        &[(1, 600), (2, 300), (3, 500)],
                        &[(1, 'H'), (2, 'i'), (3, '!')],
                    ),
                )]
            },
            &[b"BT /F1 12 Tf 72 700 Td <00010002> Tj ET"],
            false,
        );
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "Hi");
        assert!(island.editable);

        let command = replace_cmd("cid", &island, "i!H", island.appearance.clone(), PAGE_WIDE);
        let candidate = compose_candidate(&source, &[command], &FailingFontResolver)
            .expect("CID island composes in place");
        let (_, graph) = reinspect(&candidate, 1);
        assert!(graph.islands.iter().any(|isl| isl.text == "i!H"));

        let command = replace_cmd(
            "cid-miss",
            &island,
            "Hx",
            island.appearance.clone(),
            PAGE_WIDE,
        );
        let error = compose_candidate(&source, &[command], &FailingFontResolver)
            .expect_err("unknown character refused");
        assert_eq!(error.missing_chars(), Some(&['x'][..]));
    }

    #[test]
    fn add_text_on_an_indirect_array_contents_page_keeps_the_stream_list_flat() {
        let doc = custom_page_pdf(
            |d| vec![("F1", helvetica_font(d))],
            &[b"BT /F1 12 Tf 72 700 Td ", b"(Existing) Tj ET"],
            true,
        );
        let source = doc_bytes(&doc);
        let command = PdfEditCommand::AddText {
            id: "add".to_string(),
            page: 1,
            anchor: PdfPoint { x: 100.0, y: 100.0 },
            text: "Hi".to_string(),
            appearance: appearance("test-font", 18.0),
        };
        let candidate =
            compose_candidate(&source, &[command], &test_resolver()).expect("add text composes");
        let (out, graph) = reinspect(&candidate, 1);
        assert!(graph.islands.iter().any(|isl| isl.text == "Existing"));
        assert!(graph.islands.iter().any(|isl| isl.text == "Hi"));

        let page_id = out.get_pages()[&1];
        let contents = out
            .get_object(page_id)
            .expect("page")
            .as_dict()
            .expect("dict")
            .get(b"Contents")
            .expect("contents")
            .as_array()
            .expect("a direct array now");
        // q wrapper, the two original streams, Q wrapper, the added stream.
        assert_eq!(contents.len(), 5);
        for item in contents {
            let id = item.as_reference().expect("reference");
            out.get_object(id)
                .expect("object")
                .as_stream()
                .expect("every entry is a stream, never the old array object");
        }
    }

    #[test]
    fn unparseable_stream_tail_is_refused_rather_than_truncated_on_write() {
        // The lenient parser stops at the unterminated string and would drop it on rewrite.
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET (unterminated");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "Hello");
        let command = replace_cmd(
            "tail",
            &island,
            "Howdy",
            island.appearance.clone(),
            PAGE_WIDE,
        );
        assert!(matches!(
            compose_candidate(&source, &[command], &FailingFontResolver),
            Err(PdfEditError::MalformedContent)
        ));
    }
    // -----------------------------------------------------------------------------------
    // TJ-positioned word gaps: synthesised spaces, byte-for-byte preservation, pen positions
    // -----------------------------------------------------------------------------------

    use crate::test_fixtures::tex_style_pdf;

    const KERNED: &[u8] = b"BT /F1 10 Tf 72 700 Td [(hello) -333 (big) -333 (world)] TJ ET";

    fn lefts(island: &crate::model::PdfTextIsland) -> Vec<f64> {
        island.quads.iter().map(|q| q[0]).collect()
    }

    /// The first `TJ` array of the first content stream, as strings (`"<n>"` for numbers).
    fn tj_shape(candidate: &ComposedCandidate) -> Vec<String> {
        let (out, _) = reinspect(candidate, 1);
        let page_id = out.get_pages()[&1];
        let ids = content_stream_ids(
            &out,
            out.get_object(page_id)
                .expect("page")
                .as_dict()
                .expect("dict")
                .get(b"Contents")
                .expect("contents"),
        )
        .expect("ids");
        let content = decode_stream_content(&out, ids[0]).expect("content");
        let tj = content
            .operations
            .iter()
            .find(|op| op.operator == "TJ")
            .expect("TJ");
        let Some(Object::Array(items)) = tj.operands.first() else {
            panic!("TJ array");
        };
        items
            .iter()
            .map(|item| match item {
                Object::String(bytes, _) => String::from_utf8_lossy(bytes).into_owned(),
                Object::Integer(n) => format!("<{n}>"),
                Object::Real(n) => format!("<{n}>"),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    fn edit_text(
        doc: &Document,
        island: &crate::model::PdfTextIsland,
        after: &str,
    ) -> Result<ComposedCandidate, PdfEditError> {
        let command = replace_cmd(
            "gap",
            island,
            after,
            island.appearance.clone(),
            island_box_wide(island),
        );
        compose_candidate(&doc_bytes(doc), &[command], &FailingFontResolver)
    }

    /// The island's own box widened to the page, as the app does when the line has room.
    fn island_box_wide(island: &crate::model::PdfTextIsland) -> PdfRect {
        let own = island_box(island);
        PdfRect {
            width: PAGE_WIDE.width - own.x,
            ..own
        }
    }

    #[test]
    fn tj_word_gaps_become_spaces_in_island_text_with_recorded_widths() {
        let doc = tex_style_pdf(false, KERNED);
        let sha = document_sha256(&doc_bytes(&doc));
        let island = locate_island(&doc, &sha, 1, "hello big world");
        assert!(island.editable, "{:?}", island.edit_blocker);
        assert_eq!(island.quads.len(), 13, "gaps are not glyphs");
        let gaps: Vec<(usize, f64)> = island
            .gaps
            .iter()
            .map(|g| (g.char_index, g.width_pt))
            .collect();
        assert_eq!(gaps.len(), 2);
        assert_eq!((gaps[0].0, gaps[1].0), (5, 9));
        assert!(
            (gaps[0].1 - 3.33).abs() < 0.01 && (gaps[1].1 - 3.33).abs() < 0.01,
            "{gaps:?}"
        );
        let json = serde_json::to_value(&island).expect("serializes");
        assert_eq!(json["gaps"][0]["charIndex"], 5);
    }

    #[test]
    fn small_kerns_letter_spacing_and_real_spaces_do_not_become_gaps() {
        let doc = build_pdf(b"BT /F1 12 Tf 3 Tc 72 700 Td [(h) (e) (l) (l) (o)] TJ ET");
        let sha = document_sha256(&doc_bytes(&doc));
        let island = locate_island(&doc, &sha, 1, "hello");
        assert!(island.gaps.is_empty());

        let doc = tex_style_pdf(true, b"BT /F1 10 Tf 72 700 Td [(hello ) -333 (big)] TJ ET");
        let sha = document_sha256(&doc_bytes(&doc));
        let island = locate_island(&doc, &sha, 1, "hello big");
        assert!(
            island.gaps.is_empty(),
            "a real space already marks the word break"
        );
    }

    #[test]
    fn word_rotation_keeps_every_gap_and_every_glyph_position() {
        for space_glyph in [false, true] {
            let doc = tex_style_pdf(space_glyph, KERNED);
            let sha = document_sha256(&doc_bytes(&doc));
            let island = locate_island(&doc, &sha, 1, "hello big world");
            let candidate = edit_text(&doc, &island, "hello big orldw").expect("composes");
            let (_, graph) = reinspect(&candidate, 1);
            let after = graph
                .islands
                .iter()
                .find(|i| i.text == "hello big orldw")
                .expect("island text equals the request, spaces included");
            assert_eq!(after.gaps.len(), 2);
            let (before_x, after_x) = (lefts(&island), lefts(after));
            assert_eq!(before_x.len(), after_x.len());
            for (b, a) in before_x.iter().zip(&after_x) {
                assert!((b - a).abs() < 0.01, "glyph moved: {b} -> {a}");
            }
            // The untouched words and both adjustments are byte for byte what they were.
            assert_eq!(
                tj_shape(&candidate),
                ["hello", "<-333>", "big", "<-333>", "orldw"]
            );
        }
    }

    #[test]
    fn inserting_a_word_regenerates_a_gap_of_the_original_width() {
        let doc = tex_style_pdf(false, KERNED);
        let sha = document_sha256(&doc_bytes(&doc));
        let island = locate_island(&doc, &sha, 1, "hello big world");
        let candidate = edit_text(&doc, &island, "hello a big world").expect("composes");
        assert_eq!(
            tj_shape(&candidate),
            ["hello", "<-333>", "a", "<-333>", "big", "<-333>", "world"]
        );
        let (_, graph) = reinspect(&candidate, 1);
        let after = graph
            .islands
            .iter()
            .find(|i| i.text == "hello a big world")
            .expect("exact text");
        let (b, a) = (lefts(&island), lefts(after));
        for i in 0..5 {
            assert!((b[i] - a[i]).abs() < 0.01, "hello moved");
        }
        // "big" and "world" move by exactly the inserted word plus one regenerated gap.
        let shift = 500.0 / 1000.0 * 10.0 + 3.33;
        for i in 5..13 {
            assert!((a[i + 1] - b[i] - shift).abs() < 0.01, "glyph {i}");
        }
    }

    #[test]
    fn removing_a_word_drops_its_gap_instead_of_leaving_two() {
        let doc = tex_style_pdf(false, KERNED);
        let sha = document_sha256(&doc_bytes(&doc));
        let island = locate_island(&doc, &sha, 1, "hello big world");
        let candidate = edit_text(&doc, &island, "hello world").expect("composes");
        assert_eq!(
            tj_shape(&candidate),
            ["hello", "<-333>", "", "<0>", "world"]
        );
        let (_, graph) = reinspect(&candidate, 1);
        let after = graph
            .islands
            .iter()
            .find(|i| i.text == "hello world")
            .expect("exact text");
        let (b, a) = (lefts(&island), lefts(after));
        let shift = 3.0 * 5.0 + 3.33;
        for i in 0..5 {
            assert!((b[i] - a[i]).abs() < 0.01, "hello moved");
        }
        for i in 8..13 {
            assert!((b[i] - a[i - 3] - shift).abs() < 0.01, "world glyph {i}");
        }
    }

    #[test]
    fn a_real_space_glyph_is_used_when_the_font_has_one() {
        let doc = tex_style_pdf(true, b"BT /F1 10 Tf 72 700 Td [(hello) -333 (big)] TJ ET");
        let sha = document_sha256(&doc_bytes(&doc));
        let island = locate_island(&doc, &sha, 1, "hello big");
        let candidate = edit_text(&doc, &island, "hello a big").expect("composes");
        assert_eq!(tj_shape(&candidate), ["hello", "<-333>", "a big"]);
        let (_, graph) = reinspect(&candidate, 1);
        assert!(graph.islands.iter().any(|i| i.text == "hello a big"));
    }

    #[test]
    fn a_new_space_with_no_glyph_and_no_reference_gap_is_refused_not_dropped() {
        let doc = tex_style_pdf(false, b"BT /F1 10 Tf 72 700 Td (hello) Tj ET");
        let sha = document_sha256(&doc_bytes(&doc));
        let island = locate_island(&doc, &sha, 1, "hello");
        let error = edit_text(&doc, &island, "hello a").expect_err("refused");
        assert_eq!(error.missing_chars(), Some(&[' '][..]));
    }

    #[test]
    fn a_gap_made_by_a_position_move_is_a_positioned_run_and_not_editable() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Hello) Tj 35 0 Td (World) Tj ET");
        let sha = document_sha256(&doc_bytes(&doc));
        let island = locate_island(&doc, &sha, 1, "Hello World");
        assert_eq!(island.gaps.len(), 1);
        assert!(!island.editable);
        assert_eq!(
            island.edit_blocker,
            Some(crate::model::PdfEditBlocker::PositionedTextRun)
        );
        let error = edit_text(&doc, &island, "Hallo World").expect_err("refused");
        assert!(
            matches!(error, PdfEditError::UnsupportedEncoding),
            "{error}"
        );
    }
    #[test]
    fn a_positioning_operator_between_two_show_operators_makes_the_run_positioned() {
        // "Hel" is 18 pt wide, so the Td lands "lo" flush against it: no gap, but a seam. Merging
        // both strings into one would move whatever the second operator positioned.
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (Hel) Tj 18 0 Td (lo) Tj ET");
        let sha = document_sha256(&doc_bytes(&doc));
        let island = locate_island(&doc, &sha, 1, "Hello");
        assert!(!island.editable);
        assert_eq!(
            island.edit_blocker,
            Some(crate::model::PdfEditBlocker::PositionedTextRun)
        );
    }

    #[test]
    fn same_length_edit_overwrites_glyphs_in_place_and_keeps_inner_kerns() {
        let doc = tex_style_pdf(
            false,
            b"BT /F1 10 Tf 72 700 Td [(h) 40 (e) 40 (llo) -333 (big)] TJ ET",
        );
        let sha = document_sha256(&doc_bytes(&doc));
        let island = locate_island(&doc, &sha, 1, "hello big");
        let candidate = edit_text(&doc, &island, "elloh big").expect("composes");
        assert_eq!(
            tj_shape(&candidate),
            ["e", "<40>", "l", "<40>", "loh", "<-333>", "big"]
        );
        let (_, graph) = reinspect(&candidate, 1);
        let after = graph
            .islands
            .iter()
            .find(|i| i.text == "elloh big")
            .expect("gap survives: exact text");
        for (b, a) in lefts(&island).iter().zip(lefts(after)) {
            assert!((b - a).abs() < 0.01);
        }
    }

    // -----------------------------------------------------------------------------------
    // Astra read of 0.2.1, 2026-10-11: findings 2, 4, 5, 6, 7, 8 (composition side)
    // -----------------------------------------------------------------------------------

    fn rect(width: f64) -> PdfRect {
        PdfRect {
            x: 72.0,
            y: 0.0,
            width,
            height: 800.0,
        }
    }

    /// The island whose text is `text` in the candidate's page 1, with its box width.
    fn island_after(
        candidate: &ComposedCandidate,
        text: &str,
    ) -> (crate::model::PdfTextIsland, f64) {
        let (_, graph) = reinspect(candidate, 1);
        let island = graph
            .islands
            .into_iter()
            .find(|i| i.text == text)
            .unwrap_or_else(|| panic!("no island {text:?} in the candidate"));
        let width = island_box(&island).width;
        (island, width)
    }

    #[test]
    fn finding2_array_cmap_edit_writes_the_codes_the_reader_sees() {
        // Codes 1,2,3 are A, Z, B. Only codes that decode to the requested letters may be written.
        let doc = custom_page_pdf(
            |d| {
                vec![(
                    "F1",
                    crate::test_fixtures::identity_h_font_with_cmap(
                        d,
                        &[(1, 500), (2, 500), (3, 500)],
                        crate::test_fixtures::raw_to_unicode_cmap(
                            "1 beginbfrange\n<0001> <0003> [<0041> <005A> <0042>]\nendbfrange",
                        ),
                    ),
                )]
            },
            &[b"BT /F1 12 Tf 72 700 Td <000100020003> Tj ET"],
            false,
        );
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "AZB");

        // The old reader read ABC, so "ACB" was writable and landed on A, B, Z.
        let command = replace_cmd("c", &island, "ACB", island.appearance.clone(), PAGE_WIDE);
        let error = compose_candidate(&source, &[command], &FailingFontResolver)
            .expect_err("C has no code in this font");
        assert_eq!(error.missing_chars(), Some(&['C'][..]));

        let command = replace_cmd("c", &island, "BZA", island.appearance.clone(), PAGE_WIDE);
        let candidate = compose_candidate(&source, &[command], &FailingFontResolver)
            .expect("letters the font has");
        let (_, graph) = reinspect(&candidate, 1);
        assert!(graph.islands.iter().any(|i| i.text == "BZA"));
    }

    #[test]
    fn finding4_flattened_edit_leaves_no_internal_kerns_and_fits_what_it_wrote() {
        // Replacing the kerned word "hello" with "aa" used to leave -100 -100 behind between the
        // blanked strings: the fit predicted 28.33 pt, the page drew 30.33 pt.
        let doc = tex_style_pdf(
            false,
            b"BT /F1 10 Tf 72 700 Td [(h) -100 (e) -100 (llo) -333 (big)] TJ ET",
        );
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "hello big");
        // 5 glyphs x 5 pt + 2 pt of kern + 3.33 pt gap + 3 x 5 pt.
        assert!((island_box(&island).width - 45.33).abs() < 0.01);

        let command = replace_cmd(
            "flat",
            &island,
            "aa big",
            island.appearance.clone(),
            rect(28.33),
        );
        let candidate = compose_candidate(&source, &[command], &FailingFontResolver)
            .expect("fits the width it predicted");
        let (written, width) = island_after(&candidate, "aa big");
        assert!(
            (width - 28.33).abs() < 0.01,
            "predicted 28.33 pt, page draws {width} pt"
        );
        assert_eq!(written.gaps.len(), 1);
        // Every number between the old glyphs is zero; the gap before "big" is untouched.
        let shape = tj_shape(&candidate);
        assert!(shape.contains(&"<-333>".to_string()), "{shape:?}");
        assert!(!shape.contains(&"<-100>".to_string()), "{shape:?}");
    }

    #[test]
    fn finding4_flatten_across_two_operators_zeroes_the_kerns_between_them() {
        let doc = tex_style_pdf(
            false,
            b"BT /F1 10 Tf 72 700 Td [(he) -100 (l)] TJ [(lo) -333 (big)] TJ ET",
        );
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "hello big");
        let command = replace_cmd(
            "flat2",
            &island,
            "aa big",
            island.appearance.clone(),
            rect(28.33),
        );
        let candidate =
            compose_candidate(&source, &[command], &FailingFontResolver).expect("composes");
        let (_, width) = island_after(&candidate, "aa big");
        assert!((width - 28.33).abs() < 0.01, "{width}");
    }

    #[test]
    fn finding5_same_length_edit_that_adds_a_space_accounts_for_word_spacing() {
        // 10 Tw: the new space (2.5 pt wide) advances the pen 12.5 pt, so "a b" is 22.5 pt wide,
        // not the 12.5 pt the glyph-width difference alone predicts.
        let doc = tex_style_pdf(true, b"BT /F1 10 Tf 10 Tw 72 700 Td (axb) Tj ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "axb");
        let own = island_box(&island);
        assert!((own.width - 15.0).abs() < 0.01);

        let command = replace_cmd("tw", &island, "a b", island.appearance.clone(), own);
        assert!(matches!(
            compose_candidate(&source, &[command], &FailingFontResolver),
            Err(PdfEditError::TextOverflow)
        ));

        let command = replace_cmd("tw", &island, "a b", island.appearance.clone(), rect(22.5));
        let candidate = compose_candidate(&source, &[command], &FailingFontResolver)
            .expect("fits when given the width it needs");
        let (_, width) = island_after(&candidate, "a b");
        assert!((width - 22.5).abs() < 0.01, "{width}");
    }

    #[test]
    fn finding5_removing_a_space_shrinks_by_word_spacing_too() {
        let doc = tex_style_pdf(true, b"BT /F1 10 Tf 10 Tw 72 700 Td (a b) Tj ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "a b");
        assert!((island_box(&island).width - 22.5).abs() < 0.01);
        let command = replace_cmd("tw", &island, "axb", island.appearance.clone(), rect(15.0));
        let candidate =
            compose_candidate(&source, &[command], &FailingFontResolver).expect("narrower fits");
        let (_, width) = island_after(&candidate, "axb");
        assert!((width - 15.0).abs() < 0.01, "{width}");
    }

    #[test]
    fn finding6_editing_one_page_leaves_a_page_that_shares_its_stream_alone() {
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let font = helvetica_font(&mut doc);
        let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
        let shared = doc.add_object(Stream::new(
            dictionary! {},
            b"BT /F1 12 Tf 72 700 Td (Hello) Tj ET".to_vec(),
        ));
        let mut page_ids = Vec::new();
        for _ in 0..2 {
            page_ids.push(doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => shared,
                "Resources" => resources,
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Integer(612),
                    Object::Integer(792),
                ]),
            }));
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => page_ids.iter().map(|id| Object::Reference(*id)).collect::<Vec<_>>(),
                "Count" => 2,
            }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);

        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "Hello");
        let command = replace_cmd("p1", &island, "Howdy", island.appearance.clone(), PAGE_WIDE);
        let candidate =
            compose_candidate(&source, &[command], &FailingFontResolver).expect("composes");
        // Truthful: only page 1's content changed; page 2 got its own byte-identical copy.
        assert_eq!(candidate.affected_pages, vec![1]);

        let (_, page1) = reinspect(&candidate, 1);
        let (_, page2) = reinspect(&candidate, 2);
        assert!(page1.islands.iter().any(|i| i.text == "Howdy"));
        assert!(!page1.islands.iter().any(|i| i.text == "Hello"));
        assert!(
            page2.islands.iter().any(|i| i.text == "Hello"),
            "page 2 must still read Hello: {:?}",
            page2.islands.iter().map(|i| &i.text).collect::<Vec<_>>()
        );
        assert!(!page2.islands.iter().any(|i| i.text == "Howdy"));

        // A second command aimed at the other page, captured against the same pristine source,
        // is refused as stale (its stream was copied) rather than applied to the wrong page.
        let island2 = {
            let graph = walk_page_graph(&doc, &sha, 2).expect("page 2");
            graph.islands.into_iter().next().expect("island")
        };
        let c1 = replace_cmd("p1", &island, "Howdy", island.appearance.clone(), PAGE_WIDE);
        let c2 = replace_cmd("p2", &island2, "Hi", island2.appearance.clone(), PAGE_WIDE);
        let outcome = compose_candidate(&source, &[c1, c2], &FailingFontResolver);
        assert!(
            matches!(outcome, Err(PdfEditError::SourceDigestMismatch)),
            "{:?}",
            outcome.map(|c| c.affected_pages)
        );
    }

    #[test]
    fn finding7_non_editable_island_with_unchanged_appearance_is_refused_by_the_composer() {
        // A Td between the strings makes this a positioned run. A *working* resolver for the
        // island's own font id is provided, so the refusal can only come from the blocker.
        let doc = build_pdf(b"BT /F1 10 Tf 72 700 Td (A) Tj 6.67 0 Td (B) Tj ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "AB");
        assert!(!island.editable);
        assert_eq!(
            island.edit_blocker,
            Some(crate::model::PdfEditBlocker::PositionedTextRun)
        );
        let resolver = TestFontResolver {
            id: "F1",
            bytes: installable_font(),
        };
        // The resolver really would have produced a candidate for this text and appearance.
        assert!(resolver.resolve("F1").is_ok());
        assert!(resolver
            .glyphs_for_text(&resolver.resolve("F1").unwrap(), "AC")
            .is_ok());

        let command = replace_cmd("ac", &island, "AC", island.appearance.clone(), PAGE_WIDE);
        let outcome = compose_candidate(&source, &[command], &resolver);
        assert!(
            matches!(outcome, Err(PdfEditError::UnsupportedEncoding)),
            "{:?}",
            outcome.map(|c| c.bytes.len())
        );
    }

    #[test]
    fn finding7_every_blocker_is_refused_before_strategy_selection() {
        // F1 Helvetica; F2 an embedded symbolic subset; mixed sizes; cross-stream split.
        let doc = custom_page_pdf(
            |d| {
                vec![
                    ("F1", helvetica_font(d)),
                    ("F2", subset_type1_font(d, 65, &[500, 600], None, None)),
                ]
            },
            &[
                b"BT /F2 12 Tf 72 600 Td (AB) Tj ET BT /F1 12 Tf 72 500 Td (Hel) Tj ET \
                  BT /F1 12 Tf 72 400 Td (Big) Tj /F1 20 Tf (Bad) Tj ET",
            ],
            false,
        );
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let graph = walk_page_graph(&doc, &sha, 1).expect("graph");
        let resolver = TestFontResolver {
            id: "F1",
            bytes: installable_font(),
        };
        let mut refused = 0;
        for island in graph.islands.iter().filter(|i| !i.editable) {
            let command = replace_cmd(
                "same",
                island,
                &island.text,
                island.appearance.clone(),
                PAGE_WIDE,
            );
            assert!(
                matches!(
                    compose_candidate(&source, &[command], &resolver),
                    Err(PdfEditError::UnsupportedEncoding)
                ),
                "{:?} ({:?})",
                island.text,
                island.edit_blocker
            );
            refused += 1;
        }
        assert!(refused >= 2, "fixture must contain non-editable islands");
    }

    #[test]
    fn finding7_a_changed_appearance_still_redraws_a_non_editable_island() {
        // The explicit substitution request is unchanged (see also the split-run test above).
        let doc = build_pdf(b"BT /F1 10 Tf 72 700 Td (A) Tj 6.67 0 Td (B) Tj ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "AB");
        assert!(!island.editable && island.substitution_editable);
        let command = replace_cmd(
            "sub",
            &island,
            "Hi",
            appearance("test-font", 12.0),
            PdfRect {
                x: 72.0,
                y: 690.0,
                width: 200.0,
                height: 30.0,
            },
        );
        compose_candidate(&source, &[command], &test_resolver()).expect("substitution composes");
    }

    #[test]
    fn finding8_repeated_spaces_in_a_space_less_font_are_refused_not_returned() {
        let doc = tex_style_pdf(false, b"BT /F1 10 Tf 72 700 Td [(hello) -333 (big)] TJ ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "hello big");
        for after in ["hello  big", "hello big ", " hello big"] {
            let outcome = edit_text(&doc, &island, after);
            let error = outcome.expect_err(after);
            assert_eq!(error.missing_chars(), Some(&[' '][..]), "{after:?}");
        }
        // One space is still fine.
        edit_text(&doc, &island, "hello bog").expect("unchanged spacing");
    }

    #[test]
    fn composed_text_always_reads_back_as_requested_or_is_refused() {
        // The backstop behind 4, 5 and 8: verify_written rejects a write whose read-back differs.
        let doc = tex_style_pdf(true, b"BT /F1 10 Tf 72 700 Td (ab) Tj ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "ab");
        // A document that does not contain the wanted text fails the check.
        let outcome = verify_written(&doc, 1, &island, "zz", PAGE_WIDE);
        assert!(matches!(outcome, Err(PdfEditError::VerificationFailed)));
        verify_written(&doc, 1, &island, "ab", PAGE_WIDE).expect("matches what is there");
    }
    // -----------------------------------------------------------------------------------
    // Second Astra read, 2026-10-11: findings 2, 4 and 7 (composition side)
    // -----------------------------------------------------------------------------------

    use crate::test_fixtures::{code_32_letter_font, code_5_space_font, inherited_resources_pdf};

    fn add_hi(id: &str) -> PdfEditCommand {
        PdfEditCommand::AddText {
            id: id.to_string(),
            page: 1,
            anchor: PdfPoint { x: 100.0, y: 100.0 },
            text: "Hi".to_string(),
            appearance: appearance("test-font", 18.0),
        }
    }

    /// The operations of the last content stream of `page` (the one `AddText` appends).
    fn appended_operations(doc: &Document, page: u32) -> Vec<Operation> {
        let page_id = doc.get_pages()[&page];
        let contents = doc
            .get_object(page_id)
            .expect("page")
            .as_dict()
            .expect("dict")
            .get(b"Contents")
            .expect("contents");
        let ids = content_stream_ids(doc, contents).expect("ids");
        decode_stream_content(doc, *ids.last().expect("a stream"))
            .expect("content")
            .operations
    }

    #[test]
    fn finding2_adding_a_font_to_a_page_that_inherits_its_resources_keeps_what_it_inherited() {
        // /Resources (F1 and Im1) live on the /Pages node only. The old composer gave the page a
        // /Resources holding just the new font, so F1 and Im1 stopped resolving.
        let doc = inherited_resources_pdf(b"BT /F1 12 Tf 72 700 Td (Existing) Tj ET /Im1 Do");
        let source = doc_bytes(&doc);
        let candidate = compose_candidate(&source, &[add_hi("a")], &test_resolver())
            .expect("add text composes");
        let (out, graph) = reinspect(&candidate, 1);

        let existing = graph
            .islands
            .iter()
            .find(|island| island.text == "Existing")
            .expect("the inherited font still decodes the existing line");
        assert!(existing.editable, "{:?}", existing.edit_blocker);
        assert!(graph.islands.iter().any(|island| island.text == "Hi"));

        let fingerprint = inspect::resource_fingerprint(&out).expect("fingerprint");
        let names: Vec<String> = fingerprint[&1]
            .keys()
            .map(|(category, name)| {
                format!(
                    "{}/{}",
                    String::from_utf8_lossy(category),
                    String::from_utf8_lossy(name)
                )
            })
            .collect();
        assert_eq!(names, ["Font/F1", "Font/VREdit1", "XObject/Im1"]);

        // The ancestor's own resources are untouched: a sibling page would still see only F1.
        let page_id = out.get_pages()[&1];
        let parent = out
            .get_object(page_id)
            .expect("page")
            .as_dict()
            .expect("dict")
            .get(b"Parent")
            .expect("parent")
            .as_reference()
            .expect("reference");
        let inherited_fonts = out
            .get_object(parent)
            .expect("pages")
            .as_dict()
            .expect("dict")
            .get(b"Resources")
            .expect("resources")
            .as_dict()
            .expect("dict")
            .get(b"Font")
            .expect("fonts")
            .as_dict()
            .expect("dict");
        assert_eq!(inherited_fonts.len(), 1);
        assert!(inherited_fonts.has(b"F1"));
    }

    #[test]
    fn finding2_a_second_added_text_extends_the_page_local_copy() {
        let doc = inherited_resources_pdf(b"BT /F1 12 Tf 72 700 Td (Existing) Tj ET");
        let source = doc_bytes(&doc);
        let candidate = compose_candidate(&source, &[add_hi("a"), add_hi("b")], &test_resolver())
            .expect("two adds compose");
        let (out, graph) = reinspect(&candidate, 1);
        assert!(graph.islands.iter().any(|island| island.text == "Existing"));
        let fingerprint = inspect::resource_fingerprint(&out).expect("fingerprint");
        let fonts: Vec<&[u8]> = fingerprint[&1]
            .keys()
            .filter(|(category, _)| category.as_slice() == b"Font")
            .map(|(_, name)| name.as_slice())
            .collect();
        assert_eq!(fonts, [&b"F1"[..], &b"VREdit1"[..], &b"VREdit2"[..]]);
    }

    #[test]
    fn finding2_an_unresolvable_resource_chain_is_refused_not_overwritten() {
        // /Pages -> page -> /Pages: the inherited /Resources cannot be found, so nothing may be
        // written over whatever they hold.
        let mut doc = inherited_resources_pdf(b"BT /F1 12 Tf 72 700 Td (Existing) Tj ET");
        let page_id = doc.get_pages()[&1];
        let pages_id = doc
            .get_object(page_id)
            .expect("page")
            .as_dict()
            .expect("dict")
            .get(b"Parent")
            .expect("parent")
            .as_reference()
            .expect("reference");
        if let Ok(Object::Dictionary(pages)) = doc.get_object_mut(pages_id) {
            pages.remove(b"Resources");
            pages.set("Parent", Object::Reference(page_id));
        }
        let outcome = compose_candidate(&doc_bytes(&doc), &[add_hi("a")], &test_resolver());
        assert!(
            matches!(outcome, Err(PdfEditError::MalformedContent)),
            "{:?}",
            outcome.map(|c| c.bytes.len())
        );
    }

    #[test]
    fn finding7_added_text_does_not_inherit_the_pages_trailing_text_state() {
        // The page's last text object leaves invisible (3 Tr), spaced, narrowed and raised text
        // state behind; the appended stream used to inherit all of it.
        let doc = build_pdf(b"BT /F1 10 Tf 3 Tr 5 Tc 7 Tw 50 Tz 4 Ts (hidden) Tj ET");
        let source = doc_bytes(&doc);
        let candidate = compose_candidate(&source, &[add_hi("a")], &test_resolver())
            .expect("add text composes");
        let (out, graph) = reinspect(&candidate, 1);

        // What the walker measures for the added line: no extra spacing, full width, no rise.
        let added = graph
            .islands
            .iter()
            .find(|island| island.text == "Hi")
            .expect("added island");
        let layout = added.layout.expect("layout");
        assert_eq!(
            (
                layout.char_spacing,
                layout.word_spacing,
                layout.horizontal_scale
            ),
            (0.0, 0.0, 1.0)
        );
        assert!(
            (added.quads[0][1] - (100.0 - 0.25 * 18.0)).abs() < 1e-9,
            "no inherited text rise: {}",
            added.quads[0][1]
        );

        // What the page will draw: every parameter set explicitly inside an isolating q/Q.
        let ops = appended_operations(&out, 1);
        let names: Vec<&str> = ops.iter().map(|op| op.operator.as_str()).collect();
        assert_eq!(
            names,
            ["q", "BT", "Tf", "Tr", "Tc", "Tw", "Tz", "Ts", "rg", "Tm", "Tj", "ET", "Q"]
        );
        let operand = |name: &str| {
            ops.iter()
                .find(|op| op.operator == name)
                .map(|op| op.operands.clone())
        };
        assert_eq!(operand("Tr"), Some(vec![Object::Integer(0)]));
        assert_eq!(operand("Tc"), Some(vec![Object::Integer(0)]));
        assert_eq!(operand("Tw"), Some(vec![Object::Integer(0)]));
        assert_eq!(operand("Tz"), Some(vec![Object::Integer(100)]));
        assert_eq!(operand("Ts"), Some(vec![Object::Integer(0)]));
    }

    #[test]
    fn finding7_the_ocr_correction_text_block_resets_the_same_state() {
        let doc = build_pdf(b"BT /F1 10 Tf 3 Tr 5 Tc (hidden) Tj ET");
        let source = doc_bytes(&doc);
        let command = ocr_command(
            "ocr",
            "line-1",
            "Hi",
            PdfRect {
                x: 100.0,
                y: 100.0,
                width: 100.0,
                height: 30.0,
            },
        );
        let candidate = compose_candidate(&source, &[command], &test_resolver()).expect("composes");
        let (out, _) = reinspect(&candidate, 1);
        let ops = appended_operations(&out, 1);
        let names: Vec<&str> = ops.iter().map(|op| op.operator.as_str()).collect();
        assert_eq!(
            names,
            [
                "q", "rg", "re", "f", "Q", // cover
                "q", "BT", "Tf", "Tr", "Tc", "Tw", "Tz", "Ts", "rg", "Tm", "Tj", "ET", "Q"
            ]
        );
    }

    // -----------------------------------------------------------------------------------
    // Third Astra read, 2026-10-11: findings 4 and 5
    // -----------------------------------------------------------------------------------

    /// Walks every `/Contents` stream of `page` in order and returns the nesting depth reached
    /// just before the last stream, the lowest depth seen, and how many streams the page has.
    fn nesting_before_last_stream(doc: &Document, page: u32) -> (i64, i64, usize) {
        let page_id = doc.get_pages()[&page];
        let contents = doc
            .get_object(page_id)
            .expect("page")
            .as_dict()
            .expect("dict")
            .get(b"Contents")
            .expect("contents");
        let ids = content_stream_ids(doc, contents).expect("ids");
        let (mut depth, mut lowest) = (0_i64, 0_i64);
        for id in &ids[..ids.len() - 1] {
            for op in decode_stream_content(doc, *id).expect("content").operations {
                match op.operator.as_str() {
                    "q" => depth += 1,
                    "Q" => {
                        depth -= 1;
                        lowest = lowest.min(depth);
                    }
                    _ => {}
                }
            }
        }
        (depth, lowest, ids.len())
    }

    #[test]
    fn finding4_added_text_runs_outside_the_original_graphics_state() {
        // A leftover clip, a transparency ExtGState, unclosed `q`s and stray `Q`s: each leaks into
        // a stream appended after it, and a clip cannot be undone from inside the new block.
        let cases: [(&str, &[u8]); 5] = [
            (
                "clip",
                b"0 0 1 1 re W n BT /F1 12 Tf 72 700 Td (Existing) Tj ET",
            ),
            ("alpha", b"/GS0 gs BT /F1 12 Tf 72 700 Td (Existing) Tj ET"),
            (
                "open q",
                b"q q 0 0 1 1 re W n BT /F1 12 Tf 72 700 Td (Existing) Tj ET",
            ),
            ("stray Q", b"Q Q BT /F1 12 Tf 72 700 Td (Existing) Tj ET"),
            (
                "balanced",
                b"q 2 0 0 2 0 0 cm BT /F1 12 Tf 36 350 Td (Existing) Tj ET Q",
            ),
        ];
        for (label, content) in cases {
            let doc = build_pdf(content);
            let source = doc_bytes(&doc);
            let candidate = compose_candidate(&source, &[add_hi("a")], &test_resolver())
                .unwrap_or_else(|e| panic!("{label}: {e:?}"));
            let (out, graph) = reinspect(&candidate, 1);
            assert!(
                graph.islands.iter().any(|island| island.text == "Hi"),
                "{label}"
            );
            let (depth, lowest, streams) = nesting_before_last_stream(&out, 1);
            assert_eq!(depth, 0, "{label}: the added stream starts at depth zero");
            assert_eq!(lowest, 0, "{label}: the wrapper is never popped early");
            assert_eq!(streams, 4, "{label}: q wrapper, original, Q wrapper, added");
            // The added block itself is unchanged.
            let names: Vec<String> = appended_operations(&out, 1)
                .iter()
                .map(|op| op.operator.clone())
                .collect();
            assert_eq!(
                names,
                ["q", "BT", "Tf", "Tr", "Tc", "Tw", "Tz", "Ts", "rg", "Tm", "Tj", "ET", "Q"],
                "{label}"
            );
        }
    }

    #[test]
    fn finding4_the_original_content_stream_is_not_modified() {
        let doc = build_pdf(b"0 0 1 1 re W n BT /F1 12 Tf 72 700 Td (Existing) Tj ET");
        let page_id = doc.get_pages()[&1];
        let original_ids = content_stream_ids(
            &doc,
            doc.get_object(page_id)
                .expect("page")
                .as_dict()
                .expect("dict")
                .get(b"Contents")
                .expect("contents"),
        )
        .expect("ids");
        let original = decode_stream_content(&doc, original_ids[0]).expect("content");
        let source = doc_bytes(&doc);
        let candidate =
            compose_candidate(&source, &[add_hi("a")], &test_resolver()).expect("composes");
        let (out, _) = reinspect(&candidate, 1);
        let kept = decode_stream_content(&out, original_ids[0]).expect("content");
        assert_eq!(
            kept.encode().expect("encode"),
            original.encode().expect("encode")
        );
    }

    #[test]
    fn finding4_a_replacement_in_a_new_font_is_isolated_from_the_streams_clip() {
        let doc = build_pdf(b"0 0 1 1 re W n BT /F1 12 Tf 72 700 Td (Hello) Tj ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "Hello");
        let command = replace_cmd(
            "new-font",
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
        let candidate = compose_candidate(&source, &[command], &test_resolver()).expect("composes");
        let (out, graph) = reinspect(&candidate, 1);
        assert!(graph.islands.iter().any(|isl| isl.text == "Hi"));
        let (depth, lowest, _) = nesting_before_last_stream(&out, 1);
        assert_eq!((depth, lowest), (0, 0));
    }

    #[test]
    fn finding5_a_locator_missing_one_of_the_islands_spans_is_refused() {
        let doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (AB) Tj (CD) Tj ET");
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "ABCD");
        assert_eq!(island.locator.spans.len(), 2);
        let bounds = PdfRect {
            x: 72.0,
            y: 690.0,
            width: 400.0,
            height: 40.0,
        };
        let changed_font = appearance("test-font", 24.0);

        // The island digest is retained; only the second span is removed.
        let mut partial = replace_cmd("partial", &island, "Hi", changed_font.clone(), bounds);
        if let PdfEditCommand::ReplaceSourceText { locator, .. } = &mut partial {
            locator.spans.truncate(1);
        }
        assert!(matches!(
            compose_candidate(&source, &[partial], &test_resolver()),
            Err(PdfEditError::SourceDigestMismatch)
        ));

        // The same holds for a reordered locator and for one with a repeated span.
        let mut reordered = replace_cmd("reordered", &island, "Hi", changed_font.clone(), bounds);
        if let PdfEditCommand::ReplaceSourceText { locator, .. } = &mut reordered {
            locator.spans.reverse();
        }
        assert!(matches!(
            compose_candidate(&source, &[reordered], &test_resolver()),
            Err(PdfEditError::SourceDigestMismatch)
        ));
        let mut repeated = replace_cmd("repeated", &island, "Hi", changed_font.clone(), bounds);
        if let PdfEditCommand::ReplaceSourceText { locator, .. } = &mut repeated {
            let first = locator.spans[0].clone();
            locator.spans = vec![first.clone(), first];
        }
        assert!(matches!(
            compose_candidate(&source, &[repeated], &test_resolver()),
            Err(PdfEditError::SourceDigestMismatch)
        ));

        // The complete locator still composes, and removes the whole island.
        let whole = replace_cmd("whole", &island, "Hi", changed_font, bounds);
        let candidate =
            compose_candidate(&source, &[whole], &test_resolver()).expect("complete locator");
        let (_, graph) = reinspect(&candidate, 1);
        assert!(graph.islands.iter().any(|isl| isl.text == "Hi"));
        assert!(!graph
            .islands
            .iter()
            .any(|isl| isl.text.contains("AB") || isl.text.contains("CD")));
    }

    #[test]
    fn finding4_replacing_with_code_32_pays_the_word_spacing_the_page_will_draw() {
        // /Differences [32 /x 97 /a /b]: writing x writes byte 32, and byte 32 takes Tw.
        let doc = custom_page_pdf(
            |d| vec![("F1", code_32_letter_font(d))],
            &[b"BT /F1 10 Tf 10 Tw 72 700 Td (aab) Tj ET"],
            false,
        );
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "aab");
        assert!(island.editable, "{:?}", island.edit_blocker);
        let own = island_box(&island);
        assert!((own.width - 15.0).abs() < 0.01, "{}", own.width);

        // In its own 15 pt the edit would draw 5 + (5 + 10) + 5 = 25 pt: refused before writing.
        let command = replace_cmd("tw", &island, "axb", island.appearance.clone(), own);
        assert!(matches!(
            compose_candidate(&source, &[command], &FailingFontResolver),
            Err(PdfEditError::TextOverflow)
        ));

        // Given 25 pt it is written, and the page reads back exactly that wide.
        let command = replace_cmd("tw", &island, "axb", island.appearance.clone(), rect(25.0));
        let candidate = compose_candidate(&source, &[command], &FailingFontResolver)
            .expect("fits the width it needs");
        let (written, width) = island_after(&candidate, "axb");
        assert!((width - 25.0).abs() < 0.01, "{width}");
        assert!(
            written.gaps.is_empty(),
            "the Tw jump is not read back as a gap"
        );
    }

    #[test]
    fn finding4_removing_code_32_gives_back_its_word_spacing() {
        let doc = custom_page_pdf(
            |d| vec![("F1", code_32_letter_font(d))],
            &[b"BT /F1 10 Tf 10 Tw 72 700 Td (a b) Tj ET"],
            false,
        );
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "axb");
        assert!((island_box(&island).width - 25.0).abs() < 0.01);
        let command = replace_cmd("tw", &island, "aab", island.appearance.clone(), rect(15.0));
        let candidate = compose_candidate(&source, &[command], &FailingFontResolver)
            .expect("15 pt is enough once the code-32 glyph is gone");
        let (_, width) = island_after(&candidate, "aab");
        assert!((width - 15.0).abs() < 0.01, "{width}");
    }

    #[test]
    fn finding4_a_space_through_another_code_is_not_charged_word_spacing_in_the_fit() {
        // The space is code 5, so Tw never applies to it: "a b" is 12.5 pt and "aab" is 15 pt.
        let doc = custom_page_pdf(
            |d| vec![("F1", code_5_space_font(d))],
            &[b"BT /F1 10 Tf 10 Tw 72 700 Td <610562> Tj ET"],
            false,
        );
        let source = doc_bytes(&doc);
        let sha = document_sha256(&source);
        let island = locate_island(&doc, &sha, 1, "a b");
        let own = island_box(&island);
        assert!((own.width - 12.5).abs() < 0.01, "{}", own.width);

        let command = replace_cmd("tw", &island, "aab", island.appearance.clone(), own);
        assert!(
            matches!(
                compose_candidate(&source, &[command], &FailingFontResolver),
                Err(PdfEditError::TextOverflow)
            ),
            "the letter is 2.5 pt wider than the space it replaces, with no Tw to give back"
        );
        let command = replace_cmd("tw", &island, "aab", island.appearance.clone(), rect(15.0));
        let candidate =
            compose_candidate(&source, &[command], &FailingFontResolver).expect("fits 15 pt");
        let (_, width) = island_after(&candidate, "aab");
        assert!((width - 15.0).abs() < 0.01, "{width}");
    }
    #[test]
    fn finding2_a_composed_add_text_passes_staging_against_its_inheriting_base() {
        // The save-side check agrees with the composer: Add Text keeps F1 and Im1 and adds a font.
        let source = doc_bytes(&inherited_resources_pdf(
            b"BT /F1 12 Tf 72 700 Td (Existing) Tj ET /Im1 Do",
        ));
        let sha = document_sha256(&source);
        let command = add_hi("a");
        let candidate =
            compose_candidate(&source, std::slice::from_ref(&command), &test_resolver())
                .expect("composes");
        let store = crate::save::PdfSaveTokenStore::default();
        crate::save::stage_prepared_save_against_base(
            &store,
            "/doc.pdf",
            &sha,
            &sha,
            &source,
            candidate.bytes,
            candidate.affected_pages,
            crate::save::expected_text_checks(std::slice::from_ref(&command)),
        )
        .expect("an Add Text hides nothing the page resolved");
    }

    // -----------------------------------------------------------------------------------
    // Fourth Astra read, 2026-10-11: findings 9 and 12 (batches against one page)
    // -----------------------------------------------------------------------------------

    /// The islands of `bytes`' page 1, in reading order.
    fn page_islands(bytes: &[u8]) -> Vec<crate::model::PdfTextIsland> {
        let doc = Document::load_mem(bytes).expect("reopen");
        walk_page_graph(&doc, &document_sha256(bytes), 1)
            .expect("inspect")
            .islands
    }

    fn changed_font_cmd(
        id: &str,
        island: &crate::model::PdfTextIsland,
        text: &str,
        x: f64,
    ) -> PdfEditCommand {
        replace_cmd(
            id,
            island,
            text,
            appearance("test-font", 24.0),
            PdfRect {
                x,
                y: 650.0,
                width: 200.0,
                height: 40.0,
            },
        )
    }

    fn add_text_cmd(id: &str, text: &str) -> PdfEditCommand {
        PdfEditCommand::AddText {
            id: id.to_string(),
            page: 1,
            anchor: PdfPoint { x: 100.0, y: 100.0 },
            text: text.to_string(),
            appearance: appearance("test-font", 18.0),
        }
    }

    /// Three islands on one baseline, told apart by colour: `AB` black, `CD` red, `AB` blue.
    const THREE_COLOURS: &[u8] =
        b"BT /F1 12 Tf 72 700 Td (AB) Tj 1 0 0 rg (CD) Tj 0 0 1 rg (AB) Tj ET";

    #[test]
    fn finding9_a_changed_font_replacement_keeps_the_neighbouring_quads_bit_for_bit() {
        let source = doc_bytes(&build_pdf(THREE_COLOURS));
        let before = page_islands(&source);
        assert_eq!(
            before.iter().map(|i| i.text.as_str()).collect::<Vec<_>>(),
            ["AB", "CD", "AB"]
        );

        let command = changed_font_cmd("first", &before[0], "Hi", 72.0);
        let candidate = compose_candidate(&source, &[command], &test_resolver()).expect("composes");
        let after = page_islands(&candidate.bytes);

        // Stream reference, contents index, operator, operand and glyph range of one span.
        type SpanPlace = ((u32, u16), u32, u32, u32, u32, u32);
        // The red island's quads and where its glyphs sit in the source.
        let red = |islands: &[crate::model::PdfTextIsland]| {
            islands
                .iter()
                .find(|i| i.text == "CD")
                .map(|i| {
                    let spans: Vec<SpanPlace> = i
                        .locator
                        .spans
                        .iter()
                        .map(|s| {
                            (
                                s.stream_ref,
                                s.contents_index,
                                s.operator_index,
                                s.operand_index,
                                s.glyph_start,
                                s.glyph_end,
                            )
                        })
                        .collect();
                    (i.quads.clone(), spans)
                })
                .expect("the red island survives")
        };
        let ((quads_before, spans_before), (quads_after, spans_after)) =
            (red(&before), red(&after));
        // What the replacement guarantees about the text after the edited run: its quads are the
        // same, bit for bit, not close. The old glyphs' advance was deleted with them, which slid
        // CD left by the width of AB.
        assert_eq!(quads_after, quads_before);
        // What it does not keep is the island digest. The digest binds every span's
        // `contents_index`, and the isolation wrapper puts one stream in front of the original, so
        // every island of the page is addressed one stream later (the same stream object, operator
        // and operand). A locator taken before the edit is stale by design; inspect the page again.
        let moved_one_stream: Vec<_> = spans_before
            .iter()
            .map(|&(stream, index, operator, operand, start, end)| {
                (stream, index + 1, operator, operand, start, end)
            })
            .collect();
        assert_eq!(spans_after, moved_one_stream);
        let blue = after
            .iter()
            .filter(|i| i.text == "AB")
            .map(|i| i.quads.clone())
            .collect::<Vec<_>>();
        assert_eq!(blue, [before[2].quads.clone()]);
        assert!(after.iter().any(|i| i.text == "Hi"));
        assert!(!after
            .iter()
            .any(|i| i.text == "AB" && i.quads == before[0].quads));
    }

    #[test]
    fn finding9_a_removed_string_inside_a_tj_keeps_the_pen_through_kerns_and_following_runs() {
        // CD's own island begins after a -300 kern; EF follows in another colour.
        let source = doc_bytes(&build_pdf(
            b"BT /F1 12 Tf 72 700 Td [(AB) -300 (CD)] TJ 1 0 0 rg (EF) Tj ET",
        ));
        let before = page_islands(&source);
        let first = before
            .iter()
            .find(|i| i.text.contains("AB"))
            .expect("first island");
        let command = changed_font_cmd("tj", first, "Hi", 72.0);
        let candidate = compose_candidate(&source, &[command], &test_resolver()).expect("composes");
        let after = page_islands(&candidate.bytes);
        let ef = |islands: &[crate::model::PdfTextIsland]| {
            islands
                .iter()
                .find(|i| i.text == "EF")
                .map(|i| i.quads.clone())
                .expect("EF survives")
        };
        assert_eq!(ef(&before), ef(&after));
    }

    // A replacement is drawn on the source run's own baseline, so a replacement beside a neighbour
    // of the same colour is grouped with it by the walker ("Hi" and "CA" 3 pt apart read as
    // "HiCA"), as a reader of the page sees them. The finding12 tests are about the batch: every
    // command resolves and is applied, in whatever order, and the page reads right. They therefore
    // look at the glyphs' origins and at the page's text, not at how many islands the walker forms.

    /// Device-space origin of every shown glyph reading `glyph`, across all islands, in order.
    fn glyph_origins(islands: &[crate::model::PdfTextIsland], glyph: &str) -> Vec<(f64, f64)> {
        islands
            .iter()
            .flat_map(|island| island.run.iter())
            .filter(|item| item.glyph.is_some() && item.text == glyph)
            .filter_map(|item| item.placement)
            .map(|(matrix, _)| (matrix.e, matrix.f))
            .collect()
    }

    /// Whether some origin in `origins` is within 0.5 pt of the first two numbers of `at`.
    fn has_origin(origins: &[(f64, f64)], at: [f64; 4]) -> bool {
        origins
            .iter()
            .any(|origin| (origin.0 - at[0]).hypot(origin.1 - at[1]) <= 0.5)
    }

    /// The text of every island of the page, one string, with the walker's synthesised spaces and
    /// island boundaries left out.
    fn page_text(islands: &[crate::model::PdfTextIsland]) -> String {
        islands
            .iter()
            .flat_map(|island| island.text.chars())
            .filter(|c| *c != ' ')
            .collect()
    }

    #[test]
    fn finding12_add_text_and_two_replacements_on_one_page_all_resolve_in_one_batch() {
        let source = doc_bytes(&build_pdf(THREE_COLOURS));
        let before = page_islands(&source);
        // The add goes first: its isolation wrapper used to be installed on the spot, shifting every
        // original `contents_index` and with it every later island digest.
        let commands = [
            add_text_cmd("add", "HH"),
            changed_font_cmd("one", &before[0], "Hi", 72.0),
            changed_font_cmd("two", &before[2], "CA", 300.0),
        ];
        let candidate = compose_candidate(&source, &commands, &test_resolver())
            .expect("every command resolves");
        assert_eq!(candidate.affected_pages, [1]);
        let after = page_islands(&candidate.bytes);
        let texts: Vec<&str> = after.iter().map(|i| i.text.as_str()).collect();
        // The untouched and the added islands are islands of their own; the two replacements sit
        // on the page's baseline 3 pt apart in one colour and read as one island, "HiCA".
        for wanted in ["CD", "HH"] {
            assert!(texts.contains(&wanted), "{wanted}: {texts:?}");
        }
        let text = page_text(&after);
        assert!(
            text.contains("Hi") && text.contains("CA") && !text.contains("AB"),
            "{texts:?}"
        );
        // Each replacement is at the origin of the run it replaced (the H of "Hi", the C of "CA").
        assert!(has_origin(&glyph_origins(&after, "H"), before[0].baseline));
        assert!(has_origin(&glyph_origins(&after, "C"), before[2].baseline));
        assert!(has_origin(
            &glyph_origins(&after, "H"),
            [100.0, 100.0, 0.0, 0.0]
        ));

        // The untouched middle island did not move.
        let red = after.iter().find(|i| i.text == "CD").expect("CD");
        assert_eq!(red.quads, before[1].quads);

        // One wrapper pair around the original, then the three new streams in command order.
        let (out, _) = reinspect(&candidate, 1);
        let page_id = out.get_pages()[&1];
        let contents = out
            .get_object(page_id)
            .expect("page")
            .as_dict()
            .expect("dict")
            .get(b"Contents")
            .expect("contents");
        assert_eq!(content_stream_ids(&out, contents).expect("ids").len(), 6);
    }

    #[test]
    fn finding12_replacements_apply_back_to_front_whatever_order_they_are_given_in() {
        let source = doc_bytes(&build_pdf(THREE_COLOURS));
        let before = page_islands(&source);
        for order in [[0_usize, 2], [2, 0]] {
            let commands: Vec<PdfEditCommand> = order
                .iter()
                .map(|&i| changed_font_cmd(&format!("c{i}"), &before[i], "Hi", 72.0 + i as f64))
                .collect();
            let candidate = compose_candidate(&source, &commands, &test_resolver())
                .unwrap_or_else(|e| panic!("{order:?}: {e:?}"));
            let after = page_islands(&candidate.bytes);
            // Both commands were applied: two "Hi", no "AB" left, each at its run's own origin.
            // Whether they read as one island ("HiHi", applied 0 then 2) or two (2 then 0: the
            // second starts left of where the first ends) depends on the order of the appended
            // streams, which is the order of the commands, so the island count is not asserted.
            let text = page_text(&after);
            assert_eq!(text.matches("Hi").count(), 2, "{order:?}: {text}");
            assert!(!text.contains("AB"), "{order:?}: {text}");
            let origins = glyph_origins(&after, "H");
            assert_eq!(origins.len(), 2, "{order:?}");
            assert!(has_origin(&origins, before[0].baseline), "{order:?}");
            assert!(has_origin(&origins, before[2].baseline), "{order:?}");
            let red = after.iter().find(|i| i.text == "CD").expect("CD");
            assert_eq!(red.quads, before[1].quads, "{order:?}");
        }
    }

    #[test]
    fn finding12_two_islands_in_one_tj_array_both_resolve_when_the_first_is_split() {
        // A 24 pt gap is wider than 1.5 em: AB (item 0) and CD (item 2) are two islands of one TJ.
        // Splitting item 0 first would move CD to item 4 and break its locator.
        let source = doc_bytes(&build_pdf(
            b"BT /F1 12 Tf 72 700 Td [(AB) -2000 (CD)] TJ 1 0 0 rg (AB) Tj ET",
        ));
        let before = page_islands(&source);
        assert_eq!(
            before.iter().map(|i| i.text.as_str()).collect::<Vec<_>>(),
            ["AB", "CD", "AB"]
        );
        let commands = [
            changed_font_cmd("ab", &before[0], "Hi", 72.0),
            changed_font_cmd("cd", &before[1], "CA", 300.0),
        ];
        let candidate = compose_candidate(&source, &commands, &test_resolver())
            .expect("both islands of the array resolve");
        let after = page_islands(&candidate.bytes);
        let texts: Vec<&str> = after.iter().map(|i| i.text.as_str()).collect();
        // The replacements are on the baseline 11 pt apart in one colour: "Hi CA" is one island.
        // What matters is that both resolved and sit where their runs were.
        let text = page_text(&after);
        assert!(text.contains("Hi") && text.contains("CA"), "{texts:?}");
        assert!(has_origin(&glyph_origins(&after, "H"), before[0].baseline));
        assert!(has_origin(&glyph_origins(&after, "C"), before[1].baseline));
        // The run after the array kept its place through both removals.
        let tail = after
            .iter()
            .find(|i| i.text == "AB")
            .map(|i| i.quads.clone())
            .expect("the trailing AB");
        assert_eq!(tail, before[2].quads);
    }

    #[test]
    fn finding9_glyphs_shown_by_a_line_moving_operator_are_refused_not_left_with_the_pen_short() {
        let source = doc_bytes(&build_pdf(
            b"BT /F1 12 Tf 72 700 Td 14 TL (AB) ' 1 0 0 rg (CD) Tj ET",
        ));
        let before = page_islands(&source);
        let first = before.iter().find(|i| i.text == "AB").expect("AB");
        let command = changed_font_cmd("quote", first, "Hi", 72.0);
        assert!(matches!(
            compose_candidate(&source, &[command], &test_resolver()),
            Err(PdfEditError::UnsupportedEncoding)
        ));
    }

    #[test]
    fn finding16_inspection_advertises_a_substitution_exactly_when_composition_admits_it() {
        // The same two letters shown by each of the four show operators. The island may say
        // `substitution_editable` only where the composer will remove its glyphs, and the composer
        // must succeed exactly there: one predicate, read by both.
        let cases: [(&[u8], bool); 4] = [
            (b"BT /F1 12 Tf 72 700 Td (AB) Tj ET", true),
            (b"BT /F1 12 Tf 72 700 Td [(AB)] TJ ET", true),
            (b"BT /F1 12 Tf 72 700 Td 14 TL (AB) ' ET", false),
            (b"BT /F1 12 Tf 72 700 Td 14 TL 0 0 (AB) \" ET", false),
        ];
        for (content, advertised) in cases {
            let label = String::from_utf8_lossy(content).into_owned();
            let source = doc_bytes(&build_pdf(content));
            let before = page_islands(&source);
            let island = before
                .iter()
                .find(|i| i.text == "AB")
                .unwrap_or_else(|| panic!("{label}: no AB island"));
            assert_eq!(island.substitution_editable, advertised, "{label}");

            let command = changed_font_cmd("substitute", island, "Hi", 72.0);
            let composed = compose_candidate(&source, &[command], &test_resolver());
            assert_eq!(
                composed.is_ok(),
                advertised,
                "{label}: composition said {:?}",
                composed.as_ref().err()
            );
            if !advertised {
                assert!(
                    matches!(composed, Err(PdfEditError::UnsupportedEncoding)),
                    "{label}"
                );
            }
        }
    }

    #[test]
    fn finding12_an_island_listed_twice_in_a_batch_is_still_refused_the_second_time() {
        let source = doc_bytes(&build_pdf(THREE_COLOURS));
        let before = page_islands(&source);
        let commands = [
            changed_font_cmd("a", &before[0], "Hi", 72.0),
            changed_font_cmd("b", &before[0], "CA", 300.0),
        ];
        assert!(matches!(
            compose_candidate(&source, &commands, &test_resolver()),
            Err(PdfEditError::SourceDigestMismatch)
        ));
    }

    #[test]
    fn finding10_a_page_sharing_a_stream_with_many_pages_costs_one_copy() {
        // Four pages share one content stream; editing page 1 copies it once for the other three.
        let mut doc = build_pdf(b"BT /F1 12 Tf 72 700 Td (AB) Tj ET");
        let page_id = doc.get_pages()[&1];
        let (contents, parent, resources) = {
            let page = doc
                .get_object(page_id)
                .expect("page")
                .as_dict()
                .expect("dict");
            (
                page.get(b"Contents").expect("contents").clone(),
                page.get(b"Parent").expect("parent").clone(),
                page.get(b"Resources").expect("resources").clone(),
            )
        };
        let Object::Reference(pages_id) = parent.clone() else {
            panic!("parent reference");
        };
        let mut kids = vec![Object::Reference(page_id)];
        for _ in 0..3 {
            let extra = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => parent.clone(),
                "Contents" => contents.clone(),
                "Resources" => resources.clone(),
                "MediaBox" => Object::Array(vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Integer(612),
                    Object::Integer(792),
                ]),
            });
            kids.push(Object::Reference(extra));
        }
        if let Ok(Object::Dictionary(pages)) = doc.get_object_mut(pages_id) {
            pages.set("Kids", Object::Array(kids));
            pages.set("Count", 4);
        }
        let source = doc_bytes(&doc);
        let before = page_islands(&source);
        let before_objects = Document::load_mem(&source).expect("reopen").objects.len();
        let command = changed_font_cmd("shared", &before[0], "Hi", 72.0);
        let candidate = compose_candidate(&source, &[command], &test_resolver()).expect("composes");
        let out = Document::load_mem(&candidate.bytes).expect("reopen candidate");
        let mut streams: std::collections::BTreeSet<ObjectId> = Default::default();
        for page in 2..=4 {
            let id = out.get_pages()[&page];
            let contents = out
                .get_object(id)
                .expect("page")
                .as_dict()
                .expect("dict")
                .get(b"Contents")
                .expect("contents");
            streams.extend(content_stream_ids(&out, contents).expect("ids"));
        }
        assert_eq!(streams.len(), 1, "the three other pages share one copy");
        assert!(out.objects.len() > before_objects);
    }

    #[test]
    fn finding10_unclosed_q_operators_are_refused_not_expanded_into_a_wrapper() {
        // The wrapper closes every `q` the original leaves open, one `Q` operation each; the count
        // comes from the document, so it is bounded before the wrapper is built.
        let content = format!(
            "{}BT /F1 12 Tf 72 700 Td (Existing) Tj ET",
            "q ".repeat(70_000)
        );
        let source = doc_bytes(&build_pdf(content.as_bytes()));
        assert!(matches!(
            compose_candidate(&source, &[add_hi("a")], &test_resolver()),
            Err(PdfEditError::MalformedContent)
        ));
    }

    // -----------------------------------------------------------------------------------
    // Corpus probe of 0.2.1, 2026-10-12: Add Text and changed-font replacement returned
    // `malformed_content` on irs-1040.pdf, irs-p17.pdf and irs-w9.pdf (0.2.0 composed them)
    // -----------------------------------------------------------------------------------

    /// The first page of those forms: `/Contents` is cut inside the operation `/Artifact
    /// <</MCID 0>> BDC`, between its operands. ISO 32000-1 7.8.2 lets a producer cut at any
    /// boundary between tokens, so the page is well formed, yet the middle stream alone ends in an
    /// operand with no operator.
    const CUT_INSIDE_AN_OPERATION: [&[u8]; 3] = [
        b"BT /F1 12 Tf 72 700 Td (Existing) Tj ET ",
        b"q 1 0 0 1 0 0 cm Q /Artifact ",
        b"<< /MCID 0 >> BDC EMC ",
    ];

    fn page_cut_inside_an_operation() -> Document {
        // The fixture is what it claims: refused alone, well formed as the one stream it is.
        assert!(Content::decode_strict(CUT_INSIDE_AN_OPERATION[1]).is_err());
        assert!(Content::decode_strict(&CUT_INSIDE_AN_OPERATION.concat()).is_ok());
        custom_page_pdf(
            |d| vec![("F1", helvetica_font(d))],
            &CUT_INSIDE_AN_OPERATION,
            false,
        )
    }

    /// Page 1's `/Contents` streams, in order.
    fn page_one_streams(doc: &Document) -> Vec<ObjectId> {
        let page_id = doc.get_pages()[&1];
        let contents = doc
            .get_object(page_id)
            .expect("page")
            .as_dict()
            .expect("dict")
            .get(b"Contents")
            .expect("contents");
        content_stream_ids(doc, contents).expect("ids")
    }

    fn stream_bytes(doc: &Document, id: ObjectId) -> Vec<u8> {
        doc.get_object(id)
            .expect("object")
            .as_stream()
            .expect("a stream")
            .decompressed_content()
            .expect("decodes")
    }

    /// The nesting depth reached just before the last stream and the lowest depth seen, counted
    /// with the lenient parser (a stream here may end in operands, which the strict one refuses).
    fn nesting_before_the_last_stream_leniently(doc: &Document) -> (i64, i64) {
        let ids = page_one_streams(doc);
        let (mut depth, mut lowest) = (0_i64, 0_i64);
        for id in &ids[..ids.len() - 1] {
            let content = Content::decode(&stream_bytes(doc, *id)).expect("parses leniently");
            for op in content.operations {
                match op.operator.as_str() {
                    "q" => depth += 1,
                    "Q" => {
                        depth -= 1;
                        lowest = lowest.min(depth);
                    }
                    _ => {}
                }
            }
        }
        (depth, lowest)
    }

    #[test]
    fn add_text_on_a_page_cut_inside_an_operation_composes_and_leaves_every_stream_alone() {
        let doc = page_cut_inside_an_operation();
        let original = page_one_streams(&doc);
        let source = doc_bytes(&doc);
        let candidate = compose_candidate(&source, &[add_hi("a")], &test_resolver())
            .expect("a page is not malformed because its streams are cut inside an operation");
        let (out, graph) = reinspect(&candidate, 1);
        assert!(graph.islands.iter().any(|isl| isl.text == "Existing"));
        assert!(graph.islands.iter().any(|isl| isl.text == "Hi"));

        // q wrapper, the three originals, Q wrapper, the added stream.
        let ids = page_one_streams(&out);
        assert_eq!(ids.len(), 6);
        assert_eq!(&ids[1..4], &original[..]);
        for (id, bytes) in ids[1..4].iter().zip(CUT_INSIDE_AN_OPERATION) {
            assert_eq!(stream_bytes(&out, *id), bytes);
        }
        assert_eq!(nesting_before_the_last_stream_leniently(&out), (0, 0));
    }

    #[test]
    fn a_changed_font_replacement_on_a_page_cut_inside_an_operation_composes() {
        let doc = page_cut_inside_an_operation();
        let source = doc_bytes(&doc);
        let before = page_islands(&source);
        assert_eq!(
            before.iter().map(|i| i.text.as_str()).collect::<Vec<_>>(),
            ["Existing"]
        );
        let command = changed_font_cmd("redraw", &before[0], "Hi", 72.0);
        let candidate = compose_candidate(&source, &[command], &test_resolver())
            .expect("a page is not malformed because its streams are cut inside an operation");
        let (out, graph) = reinspect(&candidate, 1);
        assert!(graph.islands.iter().any(|isl| isl.text == "Hi"));
        assert!(!graph.islands.iter().any(|isl| isl.text == "Existing"));

        // The edit rewrote the first stream. The cut ones were not edited and are as they were.
        let ids = page_one_streams(&out);
        assert_eq!(ids.len(), 6);
        assert_eq!(stream_bytes(&out, ids[2]), CUT_INSIDE_AN_OPERATION[1]);
        assert_eq!(stream_bytes(&out, ids[3]), CUT_INSIDE_AN_OPERATION[2]);
        assert_eq!(nesting_before_the_last_stream_leniently(&out), (0, 0));
    }

    #[test]
    fn add_text_still_refuses_a_stream_that_ends_in_anything_but_operands() {
        // The allowance for a cut between operands and operator is not a way past the strict parse:
        // a string left open, or a byte no tokenizer reads, is still malformed content.
        for (label, tail) in [
            ("open string", &b"q (open"[..]),
            ("stray byte", &b"q \x01 Q"[..]),
        ] {
            let streams: [&[u8]; 2] = [b"BT /F1 12 Tf 72 700 Td (Existing) Tj ET ", tail];
            let doc = custom_page_pdf(|d| vec![("F1", helvetica_font(d))], &streams, false);
            let source = doc_bytes(&doc);
            assert!(
                matches!(
                    compose_candidate(&source, &[add_hi("a")], &test_resolver()),
                    Err(PdfEditError::MalformedContent)
                ),
                "{label}"
            );
        }
    }

    // -----------------------------------------------------------------------------------
    // A block in the default graphics state carries the source run's whole placement
    // -----------------------------------------------------------------------------------

    /// Page content like a producer that places every run with `cm` outside the text object: a
    /// leading `0.1` scale and a `7920` shift (a 792 pt page in tenths), then one
    /// `q <cm> cm BT /F1 100 Tf <colour> rg 0 0 Td (<text>) Tj ET Q` per `(cm, rg, text)`. The run
    /// colours differ, so neighbouring runs stay separate islands.
    fn cm_positioned_page(runs: &[(&str, &str, &str)]) -> Vec<u8> {
        let mut content = String::from("0.1 0 0 0.1 0 0 cm\n1 0 0 1 0 7920 cm\n");
        for (cm, rg, text) in runs {
            content.push_str(&format!(
                "q {cm} cm BT /F1 100 Tf {rg} rg 0 0 Td ({text}) Tj ET Q\n"
            ));
        }
        content.into_bytes()
    }

    /// The three-run page with the middle run (`middle_text`) placed by `middle_cm`.
    fn three_cm_runs(middle_cm: &str, middle_text: &str) -> Vec<u8> {
        cm_positioned_page(&[
            ("1 0 0 1 1000 -1000", "0 0 0", "AB"),
            (middle_cm, "1 0 0", middle_text),
            ("1 0 0 1 3000 -1000", "0 0 1", "CA"),
        ])
    }

    struct ReplacedMiddle {
        target: String,
        replacement: String,
        before: Vec<crate::model::PdfTextIsland>,
        after: Vec<crate::model::PdfTextIsland>,
        appended: Vec<Operation>,
        bytes: Vec<u8>,
    }

    /// Replaces `BC` of `content` with `Hi` in the catalog font (`Tf` 100, the source run's own
    /// text-space size) and reads the saved bytes back.
    fn replace_middle_run_in_a_new_font(content: &[u8]) -> ReplacedMiddle {
        replace_run_in_a_new_font(content, "BC", "Hi")
    }

    /// The same for any island of `content` reading `target`, replaced by `replacement`. Text
    /// turned a quarter is grouped by page axes (see the README's limits): it falls apart into one
    /// island per glyph, so the rotated fixtures use single-glyph runs.
    fn replace_run_in_a_new_font(
        content: &[u8],
        target: &str,
        replacement: &str,
    ) -> ReplacedMiddle {
        let source = doc_bytes(&build_pdf(content));
        let before = page_islands(&source);
        let found = before
            .iter()
            .find(|island| island.text == target)
            .expect("the middle run is an island of its own");
        let command = replace_cmd(
            "middle",
            found,
            replacement,
            appearance("test-font", 100.0),
            PdfRect {
                x: 190.0,
                y: 680.0,
                width: 200.0,
                height: 40.0,
            },
        );
        let candidate = compose_candidate(&source, &[command], &test_resolver()).expect("composes");
        let (out, graph) = reinspect(&candidate, 1);
        ReplacedMiddle {
            target: target.to_string(),
            replacement: replacement.to_string(),
            before,
            after: graph.islands,
            appended: appended_operations(&out, 1),
            bytes: candidate.bytes,
        }
    }

    fn operands_of(ops: &[Operation], operator: &str) -> Vec<f64> {
        ops.iter()
            .find(|op| op.operator == operator)
            .unwrap_or_else(|| panic!("no {operator} in {ops:?}"))
            .operands
            .iter()
            .filter_map(object_number)
            .collect()
    }

    fn assert_numbers(label: &str, got: &[f64], want: &[f64]) {
        assert_eq!(got.len(), want.len(), "{label}: {got:?} vs {want:?}");
        for (got_value, want_value) in got.iter().zip(want) {
            // PDF reals are f32.
            assert!(
                (got_value - want_value).abs() <= 1e-4,
                "{label}: {got:?} vs {want:?}"
            );
        }
    }

    /// The font size of an island's first glyph on the page, in device space: the `Tf` size times
    /// the length of one text-space unit up the page.
    fn device_size(island: &crate::model::PdfTextIsland) -> f64 {
        let layout = island.layout.expect("layout");
        layout.font_size * layout.placement.c.hypot(layout.placement.d)
    }

    fn island_named<'a>(
        islands: &'a [crate::model::PdfTextIsland],
        text: &str,
    ) -> &'a crate::model::PdfTextIsland {
        islands
            .iter()
            .find(|island| island.text == text)
            .unwrap_or_else(|| panic!("no island {text:?}"))
    }

    /// What both placement tests assert once the appended `Tm` and `Tf` are checked.
    fn assert_replacement_sits_on_the_source_run(run: &ReplacedMiddle) {
        let source = island_named(&run.before, &run.target);
        let written = island_named(&run.after, &run.replacement);
        // (c) The saved page, read back with the crate's own walker: same glyph origin, same
        // size in device space.
        let origin_error = (written.baseline[0] - source.baseline[0])
            .hypot(written.baseline[1] - source.baseline[1]);
        assert!(
            origin_error <= 0.5,
            "origin {:?} vs source {:?}",
            &written.baseline[..2],
            &source.baseline[..2]
        );
        let (have, want) = (device_size(written), device_size(source));
        assert!(
            (have - want).abs() <= 0.02 * want,
            "device size {have} vs source {want}"
        );
        assert!(
            (want - 10.0).abs() < 1e-6,
            "the fixture's run is 10 pt: {want}"
        );
        // (d) The neighbours are exactly where they were.
        for neighbour in ["AB", "CA"] {
            assert_eq!(
                island_named(&run.after, neighbour).quads,
                island_named(&run.before, neighbour).quads,
                "{neighbour}"
            );
        }
        assert!(!run.after.iter().any(|island| island.text == run.target));
    }

    #[test]
    fn a_changed_font_replacement_on_a_cm_positioned_page_carries_the_runs_placement() {
        let run = replace_middle_run_in_a_new_font(&three_cm_runs("1 0 0 1 2000 -1000", "BC"));
        // (a) Tm_source (the identity: `0 0 Td`) x CTM_source, row vectors. The CTM of the middle
        // run is `[1 0 0 1 2000 -1000] x [1 0 0 1 0 7920] x [0.1 0 0 0.1 0 0]`: a text-space point
        // (x, y) lands at (0.1 x + 0.1 * 2000, 0.1 y + 0.1 * (7920 - 1000)) = (0.1 x + 200,
        // 0.1 y + 692).
        assert_numbers(
            "Tm",
            &operands_of(&run.appended, "Tm"),
            &[0.1, 0.0, 0.0, 0.1, 200.0, 692.0],
        );
        // (b) Tf stays the source's text-space size, not the 10 pt it is drawn at.
        assert_numbers("Tf", &operands_of(&run.appended, "Tf"), &[100.0]);
        assert_replacement_sits_on_the_source_run(&run);
    }

    #[test]
    fn a_changed_font_replacement_keeps_the_rotation_of_a_cm_positioned_run() {
        // `0 1 -1 0 2000 -1000 cm` turns the run a quarter turn. Its CTM is
        // `[0 1 -1 0 2000 -1000] x [0.1 0 0 0.1 0 792]` = [0 0.1 -0.1 0 200 692].
        // One glyph in, one glyph out: a quarter-turned run of two or more glyphs is not one island.
        let run = replace_run_in_a_new_font(&three_cm_runs("0 1 -1 0 2000 -1000", "B"), "B", "H");
        assert_numbers(
            "Tm",
            &operands_of(&run.appended, "Tm"),
            &[0.0, 0.1, -0.1, 0.0, 200.0, 692.0],
        );
        assert_numbers("Tf", &operands_of(&run.appended, "Tf"), &[100.0]);
        assert_replacement_sits_on_the_source_run(&run);
        // The walker reads the new glyph's whole matrix back, rotation included.
        let placement = island_named(&run.after, "H")
            .layout
            .expect("layout")
            .placement;
        assert_numbers(
            "read back",
            &[placement.a, placement.b, placement.c, placement.d],
            &[0.0, 0.1, -0.1, 0.0],
        );
    }

    #[test]
    fn a_changed_font_replacement_keeps_the_text_matrix_and_the_cm_together() {
        // Both carry placement: `Tm` turns and moves the run, the `cm` scales it.
        let content = b"0.1 0 0 0.1 0 0 cm\n1 0 0 1 0 7920 cm\n\
            q 1 0 0 1 1000 -1000 cm BT /F1 100 Tf 0 0 0 rg 0 0 Td (AB) Tj ET Q\n\
            q 1 0 0 1 2000 -1000 cm BT /F1 100 Tf 1 0 0 rg 0 1 -1 0 30 40 Tm (B) Tj ET Q\n\
            q 1 0 0 1 3000 -1000 cm BT /F1 100 Tf 0 0 1 rg 0 0 Td (CA) Tj ET Q\n";
        let run = replace_run_in_a_new_font(content, "B", "H");
        // Tm [0 1 -1 0 30 40] x CTM [0.1 0 0 0.1 200 692]: a = 0, b = 0.1, c = -0.1, d = 0,
        // e = 30 * 0.1 + 40 * 0 + 200 = 203, f = 30 * 0 + 40 * 0.1 + 692 = 696.
        assert_numbers(
            "Tm",
            &operands_of(&run.appended, "Tm"),
            &[0.0, 0.1, -0.1, 0.0, 203.0, 696.0],
        );
        assert_numbers("Tf", &operands_of(&run.appended, "Tf"), &[100.0]);
        assert_replacement_sits_on_the_source_run(&run);
    }

    #[test]
    fn add_text_on_a_page_with_a_leading_scale_stays_an_upright_point_in_page_space() {
        let source = doc_bytes(&build_pdf(&three_cm_runs("1 0 0 1 2000 -1000", "BC")));
        let command = PdfEditCommand::AddText {
            id: "add".to_string(),
            page: 1,
            anchor: PdfPoint { x: 100.0, y: 100.0 },
            text: "Hi".to_string(),
            appearance: appearance("test-font", 18.0),
        };
        let candidate = compose_candidate(&source, &[command], &test_resolver()).expect("composes");
        let (out, graph) = reinspect(&candidate, 1);
        let ops = appended_operations(&out, 1);
        assert_numbers(
            "Tm",
            &operands_of(&ops, "Tm"),
            &[1.0, 0.0, 0.0, 1.0, 100.0, 100.0],
        );
        assert_numbers("Tf", &operands_of(&ops, "Tf"), &[18.0]);
        // Read back: the text is at the requested page point, 18 pt, whatever the page's own CTM.
        let added = island_named(&graph.islands, "Hi");
        assert!(
            (added.baseline[0] - 100.0).abs() < 1e-3 && (added.baseline[1] - 100.0).abs() < 1e-3
        );
        assert!((device_size(added) - 18.0).abs() < 1e-6);
    }

    #[test]
    fn the_composer_read_back_refuses_a_replacement_drawn_away_from_the_source_run() {
        let run = replace_middle_run_in_a_new_font(&three_cm_runs("1 0 0 1 2000 -1000", "BC"));
        let placement = island_named(&run.before, "BC")
            .layout
            .expect("layout")
            .placement;
        let mut doc = Document::load_mem(&run.bytes).expect("reopen");
        let stream = last_content_stream(&doc, 1);
        let expected = |placement: Matrix, size: f64| AppendedRun {
            page: 1,
            stream: Some(stream),
            placement,
            size,
        };
        let shifted = |dx: f64, dy: f64| Matrix {
            e: placement.e + dx,
            f: placement.f + dy,
            ..placement
        };

        verify_appended_runs(&doc, &[expected(placement, 100.0)])
            .expect("where the source run was");
        // Origin: within 0.5 pt passes, 1 pt does not.
        verify_appended_runs(&doc, &[expected(shifted(0.3, -0.3), 100.0)]).expect("0.42 pt");
        for (dx, dy) in [(1.0, 0.0), (0.0, -1.0), (-200.0, -692.0)] {
            assert!(
                matches!(
                    verify_appended_runs(&doc, &[expected(shifted(dx, dy), 100.0)]),
                    Err(PdfEditError::VerificationFailed)
                ),
                "({dx}, {dy})"
            );
        }
        // Size: 1 % passes, 3 % and ten times do not.
        verify_appended_runs(&doc, &[expected(placement, 101.0)]).expect("1 %");
        for size in [103.0, 1000.0] {
            assert!(matches!(
                verify_appended_runs(&doc, &[expected(placement, size)]),
                Err(PdfEditError::VerificationFailed)
            ));
        }
        // Rotation counts as placement.
        let turned = Matrix {
            a: 0.0,
            b: 0.1,
            c: -0.1,
            d: 0.0,
            ..placement
        };
        assert!(matches!(
            verify_appended_runs(&doc, &[expected(turned, 100.0)]),
            Err(PdfEditError::VerificationFailed)
        ));

        // The output of 0.2.0 and 0.2.1: the same block with `1 0 0 1 0 0 Tm`. Its text reads back
        // as "Hi"; only the geometry says it is wrong.
        let last = last_content_stream(&doc, 1);
        let mut content = decode_stream_content(&doc, last).expect("content");
        for op in content
            .operations
            .iter_mut()
            .filter(|op| op.operator == "Tm")
        {
            op.operands = [1_i64, 0, 0, 1, 0, 0].map(Object::Integer).to_vec();
        }
        write_stream_content(&mut doc, last, &content).expect("write");
        let graph = walk_page_graph(&doc, "", 1).expect("inspect");
        assert!(graph.islands.iter().any(|island| island.text == "Hi"));
        assert!(matches!(
            verify_appended_runs(&doc, &[expected(placement, 100.0)]),
            Err(PdfEditError::VerificationFailed)
        ));
    }

    fn last_content_stream(doc: &Document, page: u32) -> ObjectId {
        let page_id = doc.get_pages()[&page];
        let contents = doc
            .get_object(page_id)
            .expect("page")
            .as_dict()
            .expect("dict")
            .get(b"Contents")
            .expect("contents");
        *content_stream_ids(doc, contents)
            .expect("ids")
            .last()
            .expect("a stream")
    }

    // -----------------------------------------------------------------------------------
    // The appended run is found by its own stream, not by the island it was grouped into
    // -----------------------------------------------------------------------------------

    #[test]
    fn a_replacement_that_joins_a_preceding_same_colour_run_is_still_read_back() {
        // "42.00" is shown first, then "Total:" ends about 10 pt before where the replacement
        // starts. The join rule does not compare fonts, so the appended "Hi" (same colour, same
        // baseline, inside the join window) is grouped into "Total:"'s island and is not its
        // first glyph.
        let source = doc_bytes(&build_pdf(
            b"BT /F1 12 Tf 0 g 120 700 Td (42.00) Tj 1 0 0 1 80 700 Tm (Total:) Tj ET",
        ));
        let before = page_islands(&source);
        let target = island_named(&before, "42.00");
        let command = replace_cmd(
            "price",
            target,
            "Hi",
            appearance("test-font", 12.0),
            PdfRect {
                x: 120.0,
                y: 690.0,
                width: 100.0,
                height: 30.0,
            },
        );
        let candidate = compose_candidate(&source, &[command], &test_resolver())
            .expect("a correct edit is not refused because it was grouped with its neighbour");
        let after = page_islands(&candidate.bytes);
        assert!(
            after
                .iter()
                .any(|island| island.text.contains("Total:") && island.text.contains("Hi")),
            "the fixture must put the replacement in the neighbour's island: {:?}",
            after.iter().map(|i| i.text.as_str()).collect::<Vec<_>>()
        );
        let hi = after
            .iter()
            .flat_map(|island| island.run.iter())
            .filter_map(|item| item.glyph.zip(item.placement))
            .find(|(glyph, _)| glyph.stream_ref == last_stream_ref(&candidate.bytes))
            .map(|(_, placement)| placement)
            .expect("the appended run's first glyph");
        assert_numbers(
            "origin of the replacement",
            &[hi.0.e, hi.0.f],
            &[120.0, 700.0],
        );
    }

    #[test]
    fn two_adjacent_replacements_in_one_batch_are_both_read_back() {
        // "AB" and "CA" are separate islands at 12 pt (34 pt apart, join window 18). Replaced at
        // 24 pt, "Hi" is 28.8 pt wide, so the second appended run starts about 21 pt after the
        // first ends, inside the 36 pt window: both land in one island, the second mid-island.
        let source = doc_bytes(&build_pdf(
            b"BT /F1 12 Tf 0 g 100 700 Td (AB) Tj 1 0 0 1 150 700 Tm (CA) Tj ET",
        ));
        let before = page_islands(&source);
        assert_eq!(
            before.len(),
            2,
            "{:?}",
            before.iter().map(|i| &i.text).collect::<Vec<_>>()
        );
        let bounds = PdfRect {
            x: 100.0,
            y: 690.0,
            width: 100.0,
            height: 40.0,
        };
        let commands = [
            replace_cmd(
                "first",
                island_named(&before, "AB"),
                "Hi",
                appearance("test-font", 24.0),
                bounds,
            ),
            replace_cmd(
                "second",
                island_named(&before, "CA"),
                "Hi",
                appearance("test-font", 24.0),
                bounds,
            ),
        ];
        let candidate = compose_candidate(&source, &commands, &test_resolver())
            .expect("both replacements read back");
        let after = page_islands(&candidate.bytes);
        assert!(
            after
                .iter()
                .any(|island| island.text.matches("Hi").count() == 2),
            "the fixture must group the two replacements: {:?}",
            after.iter().map(|i| i.text.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn replacements_located_in_the_pristine_file_compose_in_one_call_in_either_order() {
        // The consumer's invariant: every locator of a page comes from one inspection of the
        // file on disk and all of them go to one call. "AB" and "CA" are separate islands (34 pt
        // apart at 12 pt, join window 18); replaced at 24 pt, the results sit in one island
        // (about 21 pt apart, window 36) - in the output only. Resolution happens before anything
        // is appended, so a merge in the output cannot touch a later command's locator, and the
        // order of the list changes only the order of the appended streams.
        let source = doc_bytes(&build_pdf(
            b"BT /F1 12 Tf 0 g 100 700 Td (AB) Tj 1 0 0 1 150 700 Tm (CA) Tj \
              1 0 0 rg 1 0 0 1 400 700 Tm (CD) Tj ET",
        ));
        let before = page_islands(&source);
        assert_eq!(
            before.iter().map(|i| i.text.as_str()).collect::<Vec<_>>(),
            ["AB", "CA", "CD"]
        );
        let bounds = PdfRect {
            x: 100.0,
            y: 690.0,
            width: 100.0,
            height: 40.0,
        };
        let command = |id: &str, text: &str| {
            replace_cmd(
                id,
                island_named(&before, text),
                "Hi",
                appearance("test-font", 24.0),
                bounds,
            )
        };
        for order in [["AB", "CA"], ["CA", "AB"]] {
            let commands = [command("one", order[0]), command("two", order[1])];
            let candidate = compose_candidate(&source, &commands, &test_resolver())
                .unwrap_or_else(|e| panic!("{order:?}: {e:?}"));
            let after = page_islands(&candidate.bytes);
            let texts: Vec<&str> = after.iter().map(|i| i.text.as_str()).collect();

            // Both applied, each at the origin of its own source run, and the old text is gone.
            let text = page_text(&after);
            assert_eq!(text.matches("Hi").count(), 2, "{order:?}: {texts:?}");
            assert!(
                !text.contains("AB") && !text.contains("CA"),
                "{order:?}: {texts:?}"
            );
            let origins = glyph_origins(&after, "H");
            assert_eq!(origins.len(), 2, "{order:?}");
            assert!(has_origin(&origins, island_named(&before, "AB").baseline));
            assert!(has_origin(&origins, island_named(&before, "CA").baseline));

            // The untouched run is intact, bit for bit.
            assert_eq!(
                island_named(&after, "CD").quads,
                island_named(&before, "CD").quads,
                "{order:?}"
            );
            // In the order of the page the two results are neighbours of one colour and read as
            // one island; given the other way round the second starts left of the first's end.
            if order[0] == "AB" {
                assert!(
                    after.iter().any(|i| i.text.matches("Hi").count() == 2),
                    "{texts:?}"
                );
            }
        }
    }

    /// The object of the last `/Contents` stream of page 1 of `bytes`, as the walker names it.
    fn last_stream_ref(bytes: &[u8]) -> (u32, u16) {
        let doc = Document::load_mem(bytes).expect("reopen");
        let id = last_content_stream(&doc, 1);
        (id.0, id.1)
    }
}
