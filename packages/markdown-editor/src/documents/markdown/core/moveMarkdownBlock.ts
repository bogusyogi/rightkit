/**
 * Top-level block reorder primitive.
 *
 * A "block" here is one top-level source-map group — the same unit
 * `buildMarkdownSourceMap.ts` records as `topLevelStartIndex[i]`'s
 * first node, whose `.range` already covers every nested descendant.
 * Two adjacent top-level blocks A (earlier) and B (later) are
 * contiguous in the logical source by construction: B's `prefixRange`
 * starts exactly where A's `range` ends (see `priorTopLevelEnd` in
 * buildMarkdownSourceMap.ts). That contiguity is what makes a single,
 * minimal text edit correct: only the byte range
 * `[A.range.from, B.range.to)` — the two block *bodies* plus the gap
 * between them — needs to change. The whitespace *before* A (its own
 * `prefixRange`, e.g. surrounding blank lines) is left untouched, so
 * a reorder never perturbs spacing elsewhere in the document.
 *
 * The edit swaps the two block bodies and keeps the gap between them
 * byte-for-byte: `[gapBefore] + B-body + [gap-that-was-between-A-and-B] + A-body`.
 * This is exactly what a human moving one block past its neighbor
 * would type by hand, and it round-trips a `preserve` insert exactly.
 */

import type { MarkdownCoreSnapshot } from "./MarkdownDocumentTypes.js";
import type { MarkdownTextEdit } from "./types.js";

export type MoveMarkdownBlockDirection = "up" | "down";

/**
 * Returns the top-level block index (an offset into
 * `snapshot.sourceMap.topLevelStartIndex`) whose full range contains
 * `offset`, or `null` when `offset` falls outside every top-level
 * block (e.g. an empty document, or a parser-failure fallback with
 * zero usable top-level nodes).
 */
export function topLevelMarkdownBlockIndexAt(
  snapshot: MarkdownCoreSnapshot,
  offset: number,
): number | null {
  const { nodes, topLevelStartIndex } = snapshot.sourceMap;
  const blockCount = Math.max(0, topLevelStartIndex.length - 1);
  for (let index = 0; index < blockCount; index++) {
    const nodeIndex = topLevelStartIndex[index];
    const node = nodeIndex === undefined ? undefined : nodes[nodeIndex];
    if (!node) continue;
    if (offset >= node.prefixRange.from && offset <= node.range.to) return index;
  }
  return null;
}

/** Total number of top-level blocks in `snapshot`. */
export function topLevelMarkdownBlockCount(snapshot: MarkdownCoreSnapshot): number {
  return Math.max(0, snapshot.sourceMap.topLevelStartIndex.length - 1);
}

/** Whether block `blockIndex` can move in `direction` (not already first/last). */
export function canMoveMarkdownBlock(
  snapshot: MarkdownCoreSnapshot,
  blockIndex: number,
  direction: MoveMarkdownBlockDirection,
): boolean {
  const count = topLevelMarkdownBlockCount(snapshot);
  if (blockIndex < 0 || blockIndex >= count) return false;
  return direction === "up" ? blockIndex > 0 : blockIndex < count - 1;
}

export interface MoveMarkdownBlockResult {
  /** The single source edit that performs the swap. */
  readonly edit: MarkdownTextEdit;
  /**
   * Maps a pre-move logical-source offset to its post-move offset.
   * Offsets inside the moved block track the block; offsets inside
   * the other swapped block, or outside the affected span entirely,
   * are also mapped correctly. Used to restore the caret.
   */
  readonly mapOffset: (offset: number) => number;
  /** The moved block's new top-level index (for re-focusing / re-selecting it). */
  readonly newBlockIndex: number;
}

/**
 * Computes the swap of top-level block `blockIndex` with its
 * neighbor in `direction`. Returns `null` when the move is not
 * possible (already first/last, or the source map has no usable
 * top-level blocks — e.g. a parse-error fallback).
 */
export function computeMoveMarkdownBlock(
  snapshot: MarkdownCoreSnapshot,
  blockIndex: number,
  direction: MoveMarkdownBlockDirection,
): MoveMarkdownBlockResult | null {
  if (!canMoveMarkdownBlock(snapshot, blockIndex, direction)) return null;
  const { nodes, topLevelStartIndex } = snapshot.sourceMap;
  const earlierIndex = direction === "up" ? blockIndex - 1 : blockIndex;
  const laterIndex = earlierIndex + 1;
  const earlierNodeIndex = topLevelStartIndex[earlierIndex];
  const laterNodeIndex = topLevelStartIndex[laterIndex];
  const earlier = earlierNodeIndex === undefined ? undefined : nodes[earlierNodeIndex];
  const later = laterNodeIndex === undefined ? undefined : nodes[laterNodeIndex];
  if (!earlier || !later) return null;
  // Contiguity guard: this primitive is only correct when the two blocks are
  // byte-adjacent (no third block's content is silently included/excluded).
  if (later.prefixRange.from !== earlier.range.to) return null;

  const source = snapshot.logicalSource;
  const earlierBody = source.slice(earlier.range.from, earlier.range.to);
  const gap = source.slice(earlier.range.to, later.range.from);
  const laterBody = source.slice(later.range.from, later.range.to);

  const edit: MarkdownTextEdit = {
    from: earlier.range.from,
    to: later.range.to,
    insert: laterBody + gap + earlierBody,
    inheritMode: "preserve",
  };

  const spanFrom = earlier.range.from;
  const spanTo = later.range.to;
  const laterBodyNewStart = spanFrom;
  const laterBodyNewEnd = laterBodyNewStart + laterBody.length;
  const earlierBodyNewStart = laterBodyNewEnd + gap.length;

  const mapOffset = (offset: number): number => {
    if (offset < spanFrom || offset > spanTo) return offset;
    if (offset >= earlier.range.from && offset <= earlier.range.to) {
      // Was inside the earlier block's body — now lives at earlierBodyNewStart.
      return earlierBodyNewStart + (offset - earlier.range.from);
    }
    if (offset >= later.range.from && offset <= later.range.to) {
      // Was inside the later block's body — now lives at laterBodyNewStart.
      return laterBodyNewStart + (offset - later.range.from);
    }
    // Was inside the gap between them (or the later block's own prefix range
    // up to its range.from, which is the same span) — the gap text itself
    // did not change, only its position: it now sits after the later body.
    return laterBodyNewEnd + (offset - earlier.range.to);
  };

  const newBlockIndex = direction === "up" ? blockIndex - 1 : blockIndex + 1;
  return { edit, mapOffset, newBlockIndex };
}
