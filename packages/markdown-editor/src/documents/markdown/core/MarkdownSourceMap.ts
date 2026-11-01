/**
 * Markdown source-map data structures.
 *
 * The source-map is the long-lived identity layer of the Markdown
 * pipeline. It survives every reconciliation pass and exposes:
 *
 *  - stable {@link MarkdownNodeId}s minted in source order,
 *  - exact logical-source ranges for each tracked node,
 *  - parent/child topology,
 *  - a per-revision revision number,
 *  - the parent ID of each node so reconciliation can apply the
 *    same-parent constraint without re-walking the tree.
 *
 * Source-map nodes are *distinct* from mdast nodes. Mdast nodes are
 * produced by the parser and re-derived on every change; source-map
 * nodes are what survives across reconciliations.
 */

import type { MarkdownNodeId } from "./MarkdownNodeIdFactory";

/**
 * The semantic kind of a tracked Markdown node. Unknown mdast types
 * collapse to `"other"`; reconciliation matches strictly by kind.
 */
export type MarkdownSourceNodeKind =
  | "yaml"
  | "heading"
  | "paragraph"
  | "blockquote"
  | "list"
  | "table"
  | "code"
  | "math"
  | "html"
  | "footnote-definition"
  | "link"
  | "image"
  | "inline-math"
  | "footnote-reference"
  | "inline-code"
  | "other";

/**
 * Half-open logical-source range `[from, to)`. Offsets use the
 * logical-source convention from Packet 01 (line endings are always
 * `\n` inside the range).
 */
export interface MarkdownSourceRange {
  readonly from: number;
  readonly to: number;
}

/**
 * One tracked node in the source-map.
 */
export interface MarkdownSourceNode {
  /** Stable identity. */
  readonly id: MarkdownNodeId;
  /** Semantic kind. Reconciliation matches strictly by kind. */
  readonly kind: MarkdownSourceNodeKind;
  /** Underlying mdast type (e.g. "heading", "paragraph"). */
  readonly mdastType: string;
  /** Source-map ID of the parent, or null for top-level nodes. */
  readonly parentId: MarkdownNodeId | null;
  /**
   * Index into the top-level candidate list at the time the node
   * was created. Used as a stable tie-breaker during reconciliation.
   */
  readonly topLevelIndex: number;
  /**
   * Sibling position inside its parent's child list. Top-level
   * nodes use their position in the top-level list.
   */
  readonly ordinal: number;
  /** Heading depth 1–6 for heading nodes; undefined otherwise. */
  readonly depth?: 1 | 2 | 3 | 4 | 5 | 6;
  /**
   * GitHub-style slug for headings. Computed once during candidate
   * build and never re-derived at reconcile time.
   */
  readonly headingId?: string;
  /** Exact logical-source range. */
  readonly range: MarkdownSourceRange;
  /**
   * Logical-source range that this node "owns" for reconciliation.
   * Top-level nodes own their inter-block prefix `[priorEnd, start)`.
   * Nested nodes own their own range start.
   */
  readonly prefixRange: MarkdownSourceRange;
  /** Internal hash of the raw payload. Used for exact-match scoring. */
  readonly rawHash: string;
  /** Stable semantic key (per the spec in Packet 02 §12.6). */
  readonly semanticKey: string;
}

/**
 * Internal candidate form, used by `buildMarkdownSourceMapCandidates`
 * before IDs are minted. Mirrors {@link MarkdownSourceNode} without
 * the ID, parent ID, or top-level index.
 */
export interface MarkdownSourceNodeCandidate {
  readonly kind: MarkdownSourceNodeKind;
  readonly mdastType: string;
  readonly parentCandidateIndex: number | null;
  readonly topLevelCandidateIndex: number;
  readonly range: MarkdownSourceRange;
  readonly prefixFrom: number;
  readonly prefixTo: number;
  readonly rawHash: string;
  readonly semanticKey: string;
  readonly ordinal: number;
  readonly depth?: 1 | 2 | 3 | 4 | 5 | 6;
  readonly headingId?: string;
}

/**
 * A complete source-map for one revision of one document.
 */
export interface MarkdownSourceMap {
  readonly revision: number;
  readonly nodes: readonly MarkdownSourceNode[];
  /**
   * Index of the first node of each top-level group. The slice
   * `[topLevelStartIndex[i], topLevelStartIndex[i+1])` is the i-th
   * top-level group of nodes (a top-level node plus any nested
   * nodes that follow it before the next top-level node).
   */
  readonly topLevelStartIndex: readonly number[];
  /**
   * Logical source the map was built from. Useful for tests and
   * for diagnostics; reconciliation does not depend on it.
   */
  readonly logicalSource: string;
}

/**
 * Returns `true` when a {@link MarkdownSourceRange} is well-formed:
 * `from <= to`, both non-negative, and `to` does not exceed the
 * document length.
 */
export function isValidRange(range: MarkdownSourceRange, logicalLength: number): boolean {
  return range.from >= 0 && range.to >= range.from && range.to <= logicalLength;
}

/**
 * Convenience constructor for {@link MarkdownSourceRange}.
 */
export function rangeOf(from: number, to: number): MarkdownSourceRange {
  return { from, to };
}
