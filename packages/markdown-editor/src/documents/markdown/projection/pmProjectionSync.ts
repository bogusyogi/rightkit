/**
 * Bidirectional sync helpers between a ProseMirror doc and the
 * Markdown document core.
 *
 * The sync is **call-site-driven**: the editor calls
 * {@link applyCoreCommitToPm} after every commit notification, and
 * calls {@link commitPmTransactionToCore} for transactions whose
 * `docChanged` is true.
 *
 * The module never touches ProseMirror or the core's lifecycle
 * directly; it operates on plain objects the caller already holds.
 */

import type { Node as ProseMirrorNode, Schema } from "prosemirror-model";
import type { Transaction } from "prosemirror-state";
import { MarkdownDocumentCore } from "../core/MarkdownDocumentCore";
import type { MarkdownCommitSummary, MarkdownCommand } from "../core/MarkdownDocumentTypes";
import { projectMarkdownToPm } from "./MarkdownProjection";
import {
  computePmSourcePatch,
  computeCodeFenceTransactionPatches,
  readPmBlockRangeAttrs,
  serializeBlockToMarkdown,
} from "./pmSourcePatch";
import { MARKDOWN_INTENT_META } from "./markdownTransactionIntent";
import type { MarkdownTextEdit } from "../core/types";

/**
 * Replace a PM document's blocks with the core's current logical
 * source. Returns the new doc; the caller dispatches the
 * replacement transaction.
 */
export function applyCoreCommitToPm(core: MarkdownDocumentCore, schema: Schema): ProseMirrorNode {
  return projectMarkdownToPm(core, schema);
}

/**
 * Inspect a PM transaction. If exactly one block's content
 * changed, return a single-block source patch; otherwise return
 * the supplied `fallback` (typically a `markdownToPmDoc` rebuild +
 * full-source replace).
 */
/** Fallback counters for observability. */
export let pmFallbackCount = 0;
export let pmLocalPatchCount = 0;
export function resetPmProjectionCounters(): void {
  pmFallbackCount = 0;
  pmLocalPatchCount = 0;
}

export function commitPmTransactionToCore(
  core: MarkdownDocumentCore,
  before: ProseMirrorNode,
  after: ProseMirrorNode,
  transactionMeta: {
    origin: MarkdownCommand["origin"];
    label: MarkdownCommand["label"];
    coalesceKey?: string;
    timestampMs?: number;
    expectedRevision?: number;
    fallbackSource?: string | (() => string);
    isolateHistory?: boolean;
    /** Original PM transaction, when available, for direct code-fence steps. */
    transaction?: Transaction;
  },
): MarkdownCommitSummary {
  // Structural table commands mutate PM rows/cells while their source-range attrs
  // still describe pre-command Markdown. A local table diff against those stale attrs
  // can be applied at wrong offsets on second+ operation (row/column/alignment),
  // producing mismatched GFM rows and delimiter widths. Resolve table intent from
  // current PM serialization against core source in one bounded document diff.
  const isTableStructural =
    transactionMeta.transaction?.getMeta(MARKDOWN_INTENT_META) === "table-structural";
  const hasChangedTable = hasChangedTopLevelTable(before, after);
  if ((isTableStructural || hasChangedTable) && transactionMeta.fallbackSource !== undefined) {
    pmFallbackCount++;
    const fallbackSource =
      typeof transactionMeta.fallbackSource === "function"
        ? transactionMeta.fallbackSource()
        : transactionMeta.fallbackSource;
    return core.applyCommand({
      edits: boundedDiff(core.snapshot().logicalSource, fallbackSource),
      origin: transactionMeta.origin,
      label: transactionMeta.label,
      coalesceKey: transactionMeta.coalesceKey,
      isolateHistory: transactionMeta.isolateHistory,
      timestampMs: transactionMeta.timestampMs,
      expectedRevision: transactionMeta.expectedRevision,
    });
  }
  const currentSource = core.snapshot().logicalSource;
  let edits: readonly MarkdownTextEdit[] = transactionMeta.transaction
    ? (computeCodeFenceTransactionPatches(before, transactionMeta.transaction) ??
      collectEditsFromBlocks(before, after, currentSource))
    : collectEditsFromBlocks(before, after, currentSource);
  // Structural split/join can leave source ranges & prefixes from an earlier PM revision.
  // Validate those edits against current serialization; ordinary typing keeps its local path.
  const fallbackString =
    typeof transactionMeta.fallbackSource === "function"
      ? before.childCount !== after.childCount
        ? transactionMeta.fallbackSource()
        : undefined
      : transactionMeta.fallbackSource;
  let localMatches = true;
  if (edits.length > 0 && fallbackString !== undefined) {
    try {
      localMatches = applyLogicalEdits(currentSource, edits) === fallbackString;
    } catch {
      localMatches = false;
    }
  }
  if (!localMatches && fallbackString !== undefined) {
    pmFallbackCount++;
    console.warn("pmProjectionSync: local patch mismatch, falling back", {
      edits: edits.length,
    });
    edits = boundedDiff(currentSource, fallbackString);
  }
  if (edits.length === 0) {
    if (fallbackString !== undefined) {
      pmFallbackCount++;
      const toDiff =
        typeof transactionMeta.fallbackSource === "function"
          ? (transactionMeta.fallbackSource as () => string)()
          : fallbackString;
      if (toDiff !== undefined) {
        return core.applyCommand({
          edits: boundedDiff(core.snapshot().logicalSource, toDiff),
          origin: transactionMeta.origin,
          label: transactionMeta.label,
          coalesceKey: transactionMeta.coalesceKey,
          isolateHistory: transactionMeta.isolateHistory,
          timestampMs: transactionMeta.timestampMs,
          expectedRevision: transactionMeta.expectedRevision,
        });
      }
    } else if (typeof transactionMeta.fallbackSource === "function") {
      // Lazily compute fallback only when we need it (structural change).
      pmFallbackCount++;
      const toDiff = (transactionMeta.fallbackSource as () => string)();
      console.warn("pmProjectionSync: structural fallback (lazy)", {
        beforeBlocks: collectBlockCount(before),
        afterBlocks: collectBlockCount(after),
      });
      return core.applyCommand({
        edits: boundedDiff(core.snapshot().logicalSource, toDiff),
        origin: transactionMeta.origin,
        label: transactionMeta.label,
        coalesceKey: transactionMeta.coalesceKey,
        isolateHistory: transactionMeta.isolateHistory,
        timestampMs: transactionMeta.timestampMs,
        expectedRevision: transactionMeta.expectedRevision,
      });
    }
    return core.applyCommand({
      edits: [{ from: 0, to: 0, insert: "", inheritMode: "preserve" }],
      origin: "selection-only",
      label: "Unknown",
      expectedRevision: transactionMeta.expectedRevision,
    });
  }
  pmLocalPatchCount++;
  return core.applyCommand({
    edits,
    origin: transactionMeta.origin,
    label: transactionMeta.label,
    coalesceKey: transactionMeta.coalesceKey,
    isolateHistory: transactionMeta.isolateHistory,
    timestampMs: transactionMeta.timestampMs,
    expectedRevision: transactionMeta.expectedRevision,
  });
}

function boundedDiff(oldSource: string, newSource: string): readonly MarkdownTextEdit[] {
  const MAX = 16 * 1024;
  if (oldSource.length > MAX || newSource.length > MAX) {
    console.warn("pmProjectionSync: large doc bounded fallback", {
      oldLen: oldSource.length,
      newLen: newSource.length,
    });
    return [{ from: 0, to: oldSource.length, insert: newSource, inheritMode: "preserve" }];
  }
  return MarkdownDocumentCore.diff(oldSource, newSource);
}

function collectBlockCount(doc: ProseMirrorNode): number {
  let n = 0;
  doc.forEach(() => n++);
  return n;
}

function hasChangedTopLevelTable(before: ProseMirrorNode, after: ProseMirrorNode): boolean {
  if (before.childCount !== after.childCount) return false;
  for (let index = 0; index < before.childCount; index++) {
    const prior = before.child(index);
    const next = after.child(index);
    if (prior.type.name === "table" && !prior.eq(next)) return true;
  }
  return false;
}

function applyLogicalEdits(source: string, edits: readonly MarkdownTextEdit[]): string | null {
  const sorted = edits
    .map((edit, index) => ({ edit, index }))
    .sort((a, b) => a.edit.from - b.edit.from || a.edit.to - b.edit.to || a.index - b.index);
  let cursor = 0;
  let output = "";
  for (const { edit } of sorted) {
    if (edit.from < cursor || edit.from < 0 || edit.to < edit.from || edit.to > source.length) {
      return null;
    }
    output += source.slice(cursor, edit.from) + edit.insert;
    cursor = edit.to;
  }
  return output + source.slice(cursor);
}

function collectEditsFromBlocks(
  before: ProseMirrorNode,
  after: ProseMirrorNode,
  currentSource: string,
): MarkdownTextEdit[] {
  const out: MarkdownTextEdit[] = [];
  const beforeBlocks: ProseMirrorNode[] = [];
  const afterBlocks: ProseMirrorNode[] = [];
  before.forEach((node) => beforeBlocks.push(node));
  after.forEach((node) => afterBlocks.push(node));
  // Handle text insertion/Enter/split/join without whole-doc fallback.
  // Single-block growth/shrink (typing) and one-to-two / two-to-one
  // block changes (split/join) are expressed as local source edits.
  if (beforeBlocks.length === afterBlocks.length) {
    for (let i = 0; i < beforeBlocks.length; i++) {
      const a = beforeBlocks[i];
      const b = afterBlocks[i];
      if (!a || !b) continue;
      if (a.eq(b)) continue;
      const range = readPmBlockRangeAttrs(a);
      if (!range) continue;
      const prefix = String(b.attrs.prefix ?? "");
      const previous = serializeBlockToMarkdown(a);
      // Own commits preserve the live PM tree, so its original range/rawSource attrs can lag
      // repeated typing. Diff actual pre-edit content against current source rather than replaying
      // an old raw slice. A shifted block uses the existing whole-document fallback.
      if (currentSource.slice(range.sourceFrom, range.sourceFrom + previous.length) !== previous)
        return [];
      const serialized = serializeBlockToMarkdown(b);
      const rebased = b.type.create(
        { ...b.attrs, rawSource: previous, sourceTo: range.sourceFrom + previous.length },
        b.content,
        b.marks,
      );
      const edit = computePmSourcePatch(rebased, prefix, () => serialized);
      if (edit) out.push(edit);
    }
    return out;
  }
  // Count delta
  const delta = afterBlocks.length - beforeBlocks.length;
  // Single block added/removed (Enter split or backspace join) or
  // one block split into two / two merged into one. In all cases we
  // can patch the affected contiguous range without serializing the
  // whole doc.
  if (Math.abs(delta) === 1) {
    // Find first differing index.
    let firstDiff = -1;
    const min = Math.min(beforeBlocks.length, afterBlocks.length);
    for (let i = 0; i < min; i++) {
      if (!beforeBlocks[i].eq(afterBlocks[i])) {
        firstDiff = i;
        break;
      }
    }
    if (firstDiff === -1) {
      // Insertion or deletion at end.
      if (delta === 1) {
        const newBlock = afterBlocks[afterBlocks.length - 1];
        // The new block's prefix is its separator from prior block.
        const prefix = String(newBlock.attrs.prefix ?? "\n\n");
        const edit = computePmSourcePatch(newBlock, prefix, serializeBlockToMarkdown);
        if (edit) out.push(edit);
        return out;
      } else {
        // Deletion at end — remove trailing block plus its prefix.
        const removed = beforeBlocks[beforeBlocks.length - 1];
        const range = readPmBlockRangeAttrs(removed);
        if (range) {
          out.push({
            from: range.prefixFrom,
            to: range.sourceTo,
            insert: "",
            inheritMode: "preserve",
          });
          return out;
        }
      }
      return out;
    }
    // Handle split: 1 old block -> 2 new blocks
    if (delta === 1 && beforeBlocks.length + 1 === afterBlocks.length) {
      const oldBlock = beforeBlocks[firstDiff];
      const newA = afterBlocks[firstDiff];
      const newB = afterBlocks[firstDiff + 1];
      const rangeA = readPmBlockRangeAttrs(oldBlock);
      if (rangeA) {
        // Patch the original range to cover both new blocks.
        const serA = serializeBlockToMarkdown(newA);
        const serB = serializeBlockToMarkdown(newB);
        const prefixA = String(newA.attrs.prefix ?? "");
        // The separator between newA and newB is newB's prefix.
        const sep = String(newB.attrs.prefix ?? "\n\n");
        out.push({
          from: rangeA.prefixFrom,
          to: rangeA.sourceTo,
          insert: prefixA + serA + sep + serB,
          inheritMode: "preserve",
        });
        return out;
      }
    }
    // Handle join: 2 old blocks -> 1 new block
    if (delta === -1 && beforeBlocks.length === afterBlocks.length + 1) {
      const oldA = beforeBlocks[firstDiff];
      const oldB = beforeBlocks[firstDiff + 1];
      const newBlock = afterBlocks[firstDiff];
      const rangeA = readPmBlockRangeAttrs(oldA);
      const rangeB = readPmBlockRangeAttrs(oldB);
      if (rangeA && rangeB) {
        const prefix = String(newBlock.attrs.prefix ?? "");
        const ser = serializeBlockToMarkdown(newBlock);
        out.push({
          from: rangeA.prefixFrom,
          to: rangeB.sourceTo,
          insert: prefix + ser,
          inheritMode: "preserve",
        });
        return out;
      }
    }
    // Generic single-block addition/removal in middle: treat as local.
    if (delta === 1) {
      const newBlock = afterBlocks[firstDiff];
      const prefix = String(newBlock.attrs.prefix ?? "\n\n");
      const edit = computePmSourcePatch(newBlock, prefix, serializeBlockToMarkdown);
      if (edit) out.push(edit);
      // Also check if following blocks shifted due to offset; but for typing
      // Enter the dominant cost is the split itself, which we already handled.
      return out;
    }
    if (delta === -1) {
      const removed = beforeBlocks[firstDiff];
      const range = readPmBlockRangeAttrs(removed);
      if (range) {
        out.push({
          from: range.prefixFrom,
          to: range.sourceTo,
          insert: "",
          inheritMode: "preserve",
        });
        return out;
      }
    }
  }
  // Multi-block structural changes (e.g., paste of many blocks)
  // fall back to bounded diff via the caller.
  return out;
}
