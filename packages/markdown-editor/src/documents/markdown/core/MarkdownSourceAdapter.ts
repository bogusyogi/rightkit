/**
 * Shared helpers for the Markdown source editor's core integration.
 *
 * The live CodeMirror↔core wiring (diff dispatch, projection back into the
 * view, selection hints) lives in `useCodeMirrorSourceEditor.ts`; this module
 * keeps the two pieces of that integration that are reused elsewhere:
 * `DIFF_BUDGET_MS`, the cost cap beyond which a logical diff falls back to a
 * single whole-document edit, and `makeNearestNodeIdLookup`, the
 * offset→node-id lookup used to build selection hints.
 */

import type { MarkdownNodeId } from "./MarkdownNodeIdFactory";

/**
 * Diff budget in milliseconds. Computing the LCS above this cost
 * must fall back to a single replace edit. The default is 50 ms,
 * which is well under a single React frame at 60Hz.
 */
export const DIFF_BUDGET_MS = 50;

/**
 * Convenience: rebuild the source-map node-id lookup for a given
 * source-map revision. Used by callers that need a fresh closure for
 * `nearestNodeId`.
 */
export function makeNearestNodeIdLookup(map: {
  nodes: ReadonlyArray<{ id: MarkdownNodeId; range: { from: number; to: number } }>;
}): (offset: number) => MarkdownNodeId | null {
  const sorted = [...map.nodes].sort((a, b) => a.range.from - b.range.from);
  return (offset) => {
    for (const node of sorted) {
      if (offset >= node.range.from && offset < node.range.to) return node.id;
    }
    return null;
  };
}
