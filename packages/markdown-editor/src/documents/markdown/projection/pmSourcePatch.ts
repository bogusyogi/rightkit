/**
 * Source-patch a single PM block back into its logical source range.
 *
 * When the ProseMirror editor edits one block (e.g. a heading), the
 * rest of the document's source must stay byte-identical. This
 * module:
 *
 *  - reads the block's `sourceFrom` / `sourceTo` / `prefixFrom` /
 *    `prefixTo` attrs,
 *  - serializes the block back to Markdown via
 *    {@link serializeBlockToMarkdown},
 *  - emits a single edit replacing `[prefixFrom, sourceTo)` with
 *    `prefix + serialized block`.
 *
 * Callers feed the edit to {@link MarkdownDocumentCore.applyCommand}
 * with `origin: "hybrid"` and `label: "BlockEdit"`.
 */

import type { Node as ProseMirrorNode } from "prosemirror-model";
import type { Transaction } from "prosemirror-state";
import { Mapping, ReplaceStep } from "prosemirror-transform";
import type { MarkdownTextEdit } from "../core/types.js";
import {
  canPreservePmRaw,
  pmBlockToMarkdown,
  pmCodeFenceToMarkdown,
  pmListToMarkdown,
  pmTableToMarkdown,
} from "./pmToMarkdown.js";
import { longestFenceRun, parseCodeFenceSource, codeFenceInfoString } from "../codeFenceModel.js";
import { computeMarkdownTableSourcePatch } from "../tables/markdownTableSourcePatch.js";

/**
 * Range fields attached to a PM block per the v2 schema.
 */
export interface PmBlockRangeAttrs {
  readonly sourceFrom: number;
  readonly sourceTo: number;
  readonly prefixFrom: number;
  readonly prefixTo: number;
}

/** Counters used by CE-3A performance probes. */
export let pmDirectStepPatchCount = 0;
export let pmWholeFenceFallbackCount = 0;

export function resetPmSourcePatchCounters(): void {
  pmDirectStepPatchCount = 0;
  pmWholeFenceFallbackCount = 0;
}

/**
 * Read the v2 range attrs from a PM block. Returns `null` when the
 * node does not carry the v2 attrs.
 */
export function readPmBlockRangeAttrs(node: ProseMirrorNode): PmBlockRangeAttrs | null {
  if (typeof node.attrs.sourceFrom !== "number") return null;
  if (typeof node.attrs.sourceTo !== "number") return null;
  if (typeof node.attrs.prefixFrom !== "number") return null;
  if (typeof node.attrs.prefixTo !== "number") return null;
  if (node.attrs.sourceFrom < 0 || node.attrs.sourceTo < 0) return null;
  return {
    sourceFrom: node.attrs.sourceFrom,
    sourceTo: node.attrs.sourceTo,
    prefixFrom: node.attrs.prefixFrom,
    prefixTo: node.attrs.prefixTo,
  };
}

/**
 * Compute the source-patch edit for `node`. The replacement span
 * is `[prefixFrom, sourceTo)`; the inserted text is
 * `prefix + serialize(node)`.
 */
export function computePmSourcePatch(
  node: ProseMirrorNode,
  prefix: string,
  serialize: (node: ProseMirrorNode) => string,
): MarkdownTextEdit | null {
  if (node.type.name === "code_fence") {
    const fencePatch = computeCodeFenceSourcePatch(node);
    // A content/info fast patch wins. Structural or fence-safety changes fall back to
    // a bounded WHOLE-fence regeneration (contract 14 §5.2): the full block range is
    // replaced by the freshly serialized fence, so a widened fence round-trips exactly.
    if (fencePatch) return fencePatch;
    const range = readPmBlockRangeAttrs(node);
    if (!range) return null;
    pmWholeFenceFallbackCount++;
    return {
      from: range.prefixFrom,
      to: range.sourceTo,
      insert: prefix + serialize(node),
      inheritMode: "preserve",
    };
  }
  if (node.type.name === "table") {
    const tablePatch = computeMarkdownTableSourcePatch(node, serialize(node));
    return tablePatch ? { ...tablePatch, inheritMode: "preserve" } : null;
  }
  const range = readPmBlockRangeAttrs(node);
  if (!range) return null;
  const before = String(node.attrs.rawSource ?? "");
  const after = serialize(node);
  if (before.length === range.sourceTo - range.sourceFrom) {
    let start = 0;
    while (start < before.length && before[start] === after[start]) start++;
    let end = 0;
    while (
      end < before.length - start &&
      end < after.length - start &&
      before[before.length - 1 - end] === after[after.length - 1 - end]
    )
      end++;
    return {
      from: range.sourceFrom + start,
      to: range.sourceTo - end,
      insert: after.slice(start, after.length - end),
      inheritMode: "preserve",
    };
  }
  return {
    from: range.prefixFrom,
    to: range.sourceTo,
    insert: prefix + after,
    inheritMode: "preserve",
  };
}

interface CodeFenceStepContext {
  readonly node: ProseMirrorNode;
  /** PM position at start of the node's text content. */
  readonly contentStart: number;
}

function codeFenceAtPosition(doc: ProseMirrorNode, position: number): CodeFenceStepContext | null {
  if (position < 0 || position > doc.content.size) return null;
  const resolved = doc.resolve(position);
  for (let depth = resolved.depth; depth > 0; depth--) {
    const node = resolved.node(depth);
    if (node.type.name === "code_fence") {
      return { node, contentStart: resolved.start(depth) };
    }
  }
  return null;
}

function insertedText(step: ReplaceStep): string | null {
  if (step.slice.openStart !== 0 || step.slice.openEnd !== 0) return null;
  // Native code_fence content is text-only. A block slice is structural and must
  // use the bounded whole-fence fallback instead of guessing Markdown spelling.
  if (
    step.slice.content.childCount > 0 &&
    !step.slice.content.content.every((node) => node.isText)
  ) {
    return null;
  }
  return step.slice.content.textBetween(0, step.slice.content.size, "\n");
}

/** Map one logical PM content offset into its authored raw-content offset. */
function codeFenceRawContentOffset(
  context: CodeFenceStepContext,
  logicalOffset: number,
): number | null {
  const rawText = String(context.node.attrs.rawText ?? "");
  if (logicalOffset < 0 || logicalOffset > rawText.length) return null;
  const rawSource = String(context.node.attrs.rawSource ?? "");
  const contentFrom = Number(context.node.attrs.contentFrom ?? -1);
  const contentTo = Number(context.node.attrs.contentTo ?? -1);
  if (!Number.isInteger(contentFrom) || !Number.isInteger(contentTo)) return null;
  const rawContent = rawSource.slice(contentFrom, contentTo);
  const rawLines = rawContent.split("\n");
  const logicalLines = rawText.split("\n");
  if (rawLines.length !== logicalLines.length) return null;
  const fallbackIndent = String(context.node.attrs.contentIndent ?? "");
  let logicalCursor = 0;
  let rawCursor = 0;
  for (let index = 0; index < logicalLines.length; index++) {
    const logicalLine = logicalLines[index] ?? "";
    const rawLine = rawLines[index] ?? "";
    const prefixLength = rawLine.endsWith(logicalLine)
      ? rawLine.length - logicalLine.length
      : fallbackIndent.length;
    const prefix = rawLine.slice(0, prefixLength);
    if (!/^[ \t]*$/.test(prefix) || !rawLine.endsWith(logicalLine)) return null;
    if (logicalOffset <= logicalCursor + logicalLine.length) {
      return rawCursor + prefixLength + logicalOffset - logicalCursor;
    }
    logicalCursor += logicalLine.length;
    rawCursor += rawLine.length;
    if (index < logicalLines.length - 1) {
      logicalCursor++;
      rawCursor++;
    }
  }
  return rawContent.length;
}

function codeFenceIndentAt(context: CodeFenceStepContext, logicalOffset: number): string {
  const rawText = String(context.node.attrs.rawText ?? "");
  const rawSource = String(context.node.attrs.rawSource ?? "");
  const contentFrom = Number(context.node.attrs.contentFrom ?? -1);
  const contentTo = Number(context.node.attrs.contentTo ?? -1);
  const rawLines = rawSource.slice(contentFrom, contentTo).split("\n");
  const logicalLines = rawText.split("\n");
  let cursor = 0;
  for (let index = 0; index < logicalLines.length; index++) {
    const logicalLine = logicalLines[index] ?? "";
    if (logicalOffset <= cursor + logicalLine.length || index === logicalLines.length - 1) {
      const rawLine = rawLines[index] ?? "";
      if (rawLine.endsWith(logicalLine)) {
        return rawLine.slice(0, rawLine.length - logicalLine.length);
      }
      return String(context.node.attrs.contentIndent ?? "");
    }
    cursor += logicalLine.length + 1;
  }
  return String(context.node.attrs.contentIndent ?? "");
}

function indentInsertedCodeText(
  context: CodeFenceStepContext,
  text: string,
  logicalTo: number,
): string {
  if (!text.includes("\n")) return text;
  const indent = codeFenceIndentAt(context, logicalTo);
  if (!indent) return text;
  let inserted = text.replace(/\n(?=[^\n])/g, `\n${indent}`);
  // A replacement ending at next line's start consumes that line's authored
  // prefix; restore it when inserted text leaves a following line in place.
  const oldText = String(context.node.attrs.rawText ?? "");
  if (
    inserted.endsWith("\n") &&
    logicalTo < oldText.length &&
    !oldText.startsWith("\n", logicalTo)
  ) {
    inserted += indent;
  }
  return inserted;
}

function directPatchWouldCloseFence(
  context: CodeFenceStepContext,
  edits: readonly MarkdownTextEdit[],
): boolean {
  if (edits.length === 0) return false;
  const fenceChar = String(context.node.attrs.fenceChar ?? "`");
  const fenceLength = Math.max(Number(context.node.attrs.fenceLength ?? 3), 3);
  // Projection already stores the pre-edit content in attrs; reading this
  // string avoids traversing the PM fragment on every keystroke.
  const oldContent = String(context.node.attrs.rawText ?? "");
  const localEdits = edits
    .map((edit) => ({
      from: edit.from,
      to: edit.to,
      insert: edit.insert,
    }))
    .sort((a, b) => a.from - b.from || a.to - b.to);
  const changedFrom = Math.max(0, localEdits[0]?.from ?? 0);
  const changedTo = Math.min(
    oldContent.length,
    localEdits.reduce((max, edit) => Math.max(max, edit.to), changedFrom),
  );
  const lineStart = oldContent.lastIndexOf("\n", changedFrom - 1) + 1;
  const lineEndIndex = oldContent.indexOf("\n", changedTo);
  const lineEnd = lineEndIndex < 0 ? oldContent.length : lineEndIndex;
  const windowStart = Math.max(0, Math.min(lineStart, changedFrom - fenceLength));
  const windowEnd = Math.min(oldContent.length, Math.max(lineEnd, changedTo + fenceLength));
  let local = oldContent.slice(windowStart, windowEnd);
  for (let index = localEdits.length - 1; index >= 0; index--) {
    const edit = localEdits[index];
    if (!edit) continue;
    const from = edit.from - windowStart;
    const to = edit.to - windowStart;
    if (from < 0 || to < from || to > local.length) return true;
    local = local.slice(0, from) + edit.insert + local.slice(to);
  }
  return longestFenceRun(local, fenceChar) >= fenceLength;
}

/**
 * Translate ReplaceSteps directly from PM text offsets to logical Markdown
 * content offsets. This is the typing hot path: it never serializes or diffs
 * the complete code fence.
 *
 * `null` means the transaction is structurally ambiguous (multi-block paste,
 * stale provenance, fence safety risk, or non-text step) and the caller should
 * use its bounded whole-fence/document fallback.
 */
export function computeCodeFenceTransactionPatches(
  before: ProseMirrorNode,
  transaction: Transaction,
): readonly MarkdownTextEdit[] | null {
  if (!transaction.docChanged || transaction.steps.length === 0) return null;
  const edits: MarkdownTextEdit[] = [];
  const logicalEdits: MarkdownTextEdit[] = [];
  const priorMaps = new Mapping();
  let context: CodeFenceStepContext | null = null;

  for (const rawStep of transaction.steps) {
    if (!(rawStep instanceof ReplaceStep)) return null;
    const step = rawStep;
    const inverse = priorMaps.invert();
    const from = inverse.map(step.from, -1);
    const to = inverse.map(step.to, 1);
    const fromContext = codeFenceAtPosition(before, from);
    const toContext = codeFenceAtPosition(before, to);
    if (!fromContext || !toContext || fromContext.node !== toContext.node) return null;
    if (!context) context = fromContext;
    if (context.node !== fromContext.node) return null;
    const range = readPmBlockRangeAttrs(context.node);
    const contentFrom = Number(context.node.attrs.contentFrom ?? -1);
    const contentTo = Number(context.node.attrs.contentTo ?? -1);
    const sourceFrom = range?.sourceFrom ?? -1;
    if (
      !range ||
      !Number.isInteger(contentFrom) ||
      !Number.isInteger(contentTo) ||
      contentFrom < 0 ||
      contentTo < contentFrom ||
      from < context.contentStart ||
      to < from ||
      to > context.contentStart + context.node.content.size
    ) {
      return null;
    }
    const text = insertedText(step);
    if (text === null) return null;
    const localFrom = from - context.contentStart;
    const localTo = to - context.contentStart;
    if (localTo > contentTo - contentFrom) return null;
    const rawFrom = codeFenceRawContentOffset(context, localFrom);
    const rawTo = codeFenceRawContentOffset(context, localTo);
    if (rawFrom === null || rawTo === null || rawTo < rawFrom) return null;
    logicalEdits.push({ from: localFrom, to: localTo, insert: text, inheritMode: "preserve" });
    edits.push({
      from: sourceFrom + contentFrom + rawFrom,
      to: sourceFrom + contentFrom + rawTo,
      insert: indentInsertedCodeText(context, text, localTo),
      inheritMode: "preserve",
    });
    priorMaps.appendMap(step.getMap());
  }

  // Direct patches must be disjoint in original source coordinates.
  const sorted = [...edits].sort((a, b) => a.from - b.from || a.to - b.to);
  // IME composition may expose several zero-width ReplaceSteps at one logical
  // offset. Keep them direct by preserving step order in one insertion; a
  // repeated replacement range remains ambiguous and takes bounded fallback.
  const coalesced: MarkdownTextEdit[] = [];
  for (const edit of sorted) {
    const previous = coalesced[coalesced.length - 1];
    if (
      previous &&
      previous.from === previous.to &&
      edit.from === edit.to &&
      previous.from === edit.from
    ) {
      coalesced[coalesced.length - 1] = { ...previous, insert: previous.insert + edit.insert };
    } else {
      coalesced.push(edit);
    }
  }
  for (let i = 1; i < coalesced.length; i++) {
    const previous = coalesced[i - 1];
    const current = coalesced[i];
    if (!previous || !current || current.from < previous.to) return null;
  }
  if (context && directPatchWouldCloseFence(context, logicalEdits)) return null;
  if (coalesced.length > 0) pmDirectStepPatchCount++;
  return coalesced;
}

/**
 * Code-fence fast path (contract 14 §5.2.1): a transaction inside one
 * `code_fence` content region must not serialize the whole fence to discover a
 * one-character change. This maps the changed content directly through the
 * node's `contentFrom`/`contentTo` provenance.
 *
 * - Content-only edits → one narrowed edit over the content range.
 * - Info-string (language/meta) edits → one edit over the info range.
 * - Fence-safety or malformed-mapping cases → `null` (caller falls back to the
 *   bounded whole-fence regeneration path).
 */
function computeCodeFenceSourcePatch(node: ProseMirrorNode): MarkdownTextEdit | null {
  const range = readPmBlockRangeAttrs(node);
  if (!range) return null;
  // Markdown strips per-line fence indentation from PM text. Those source prefixes cannot map
  // safely through direct offsets, so bounded whole-fence regeneration owns these edits.
  if (String(node.attrs.contentIndent ?? "")) return null;

  // The fast path is O(changed-range), not O(block-size): content/info ranges are
  // stored as node attrs from projection, so we never parse or serialize the whole
  // fence to discover a one-character edit (contract 14 §5.2.1).
  const contentFrom = Number(node.attrs.contentFrom ?? -1);
  const contentTo = Number(node.attrs.contentTo ?? -1);
  const infoFrom = Number(node.attrs.infoFrom ?? -1);
  const infoTo = Number(node.attrs.infoTo ?? -1);
  if (
    !Number.isInteger(contentFrom) ||
    !Number.isInteger(contentTo) ||
    contentFrom < 0 ||
    contentTo < contentFrom
  ) {
    return null;
  }

  const newContent = node.textContent;
  const oldContent = String(node.attrs.rawText ?? "");

  // Content edit only: narrow the content range by common prefix/suffix. This is
  // the per-keystroke hot path — O(changed-range), never parses the whole fence.
  if (newContent !== oldContent) {
    // Compute changed range first — O(changed-range) prefix/suffix scan, no whole-fence parsing.
    let start = 0;
    while (
      start < oldContent.length &&
      start < newContent.length &&
      oldContent[start] === newContent[start]
    )
      start++;
    let end = 0;
    while (
      end < oldContent.length - start &&
      end < newContent.length - start &&
      oldContent[oldContent.length - 1 - end] === newContent[newContent.length - 1 - end]
    )
      end++;
    const fenceChar = String(node.attrs.fenceChar ?? "`");
    const fenceLength = Math.max(Number(node.attrs.fenceLength ?? 3), 3);
    // Fence safety: O(changed-range) — only scan changed line(s) ± fenceLength window.
    // Widening to a longer fence is deferred to the bounded whole-fence fallback (return null).
    // Expanding to line boundaries + fenceLength catches merged runs like "``" + "`" → "```".
    const changedStart = start;
    const changedEnd = newContent.length - end;
    const lineStart = newContent.lastIndexOf("\n", changedStart - 1) + 1;
    const nextNewline = newContent.indexOf("\n", changedEnd);
    const lineEnd = nextNewline === -1 ? newContent.length : nextNewline;
    const windowStart = Math.max(0, Math.min(lineStart, changedStart - fenceLength));
    const windowEnd = Math.min(newContent.length, Math.max(lineEnd, changedEnd + fenceLength));
    const windowSlice = newContent.slice(windowStart, windowEnd);
    if (longestFenceRun(windowSlice, fenceChar) >= fenceLength) return null;
    return {
      from: range.sourceFrom + contentFrom + start,
      to: range.sourceFrom + contentTo - end,
      insert: newContent.slice(start, newContent.length - end),
      inheritMode: "preserve",
    };
  }

  // Info-string change only (language/meta attrs edited via a compact control).
  // This is a discrete toolbar action, not the typing hot path, so parsing the
  // pre-edit raw source to learn the old language/meta is acceptable.
  if (
    Number.isInteger(infoFrom) &&
    Number.isInteger(infoTo) &&
    infoFrom >= 0 &&
    infoTo >= infoFrom
  ) {
    const rawBefore = String(node.attrs.rawSource ?? "");
    const parsed = parseCodeFenceSource(rawBefore);
    if (parsed && parsed.content === oldContent) {
      const oldLanguage = parsed.language;
      const oldMeta = parsed.meta;
      const newLanguage = String(node.attrs.language ?? "");
      const newMeta = String(node.attrs.meta ?? "");
      if (newLanguage !== oldLanguage || newMeta !== oldMeta) {
        return {
          from: range.sourceFrom + infoFrom,
          to: range.sourceFrom + infoTo,
          insert: codeFenceInfoString(newLanguage, newMeta),
          inheritMode: "preserve",
        };
      }
    }
  }

  return null;
}

/**
 * Default block-to-markdown serializer. Mirrors the v1 serializer's
 * output for the supported block types (`paragraph`, `heading`,
 * `bullet_list`, `ordered_list`, `blockquote`, `raw_block`).
 */
export function serializeBlockToMarkdown(node: ProseMirrorNode): string {
  const name = node.type.name;
  if (name === "raw_block") return String(node.attrs.rawSource ?? "");
  if (name === "code_fence") return pmCodeFenceToMarkdown(node);
  if (name === "table") return pmTableToMarkdown(node);
  const raw = String(node.attrs.rawSource ?? "");
  if (raw && canPreservePmRaw(node)) {
    return raw;
  }
  if (name === "heading" || name === "blockquote" || name === "paragraph") {
    return pmBlockToMarkdown(node, false);
  }
  if (name === "bullet_list") {
    return pmListToMarkdown(node);
  }
  if (name === "ordered_list") {
    return pmListToMarkdown(node);
  }
  return node.textContent;
}
