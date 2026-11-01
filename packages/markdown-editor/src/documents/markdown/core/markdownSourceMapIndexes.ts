/**
 * Read-only indexes over a {@link MarkdownSourceMap}.
 *
 * These helpers do not scan React DOM or the live editor state; they
 * only walk the source-map nodes. They are safe to call from React
 * selectors, from inside `useMemo`, and from tests.
 */

import type { MarkdownNodeId } from "./MarkdownNodeIdFactory.js";
import type {
  MarkdownSourceMap,
  MarkdownSourceNode,
  MarkdownSourceNodeKind,
} from "./MarkdownSourceMap.js";

/**
 * Returns every top-level node in source order. A node is
 * "top-level" iff `parentId === null`.
 */
export function topLevelNodes(map: MarkdownSourceMap): readonly MarkdownSourceNode[] {
  return map.nodes.filter((n) => n.parentId === null);
}

/**
 * Returns the immediate children of `parentId`, in source order.
 * Returns an empty list if the parent does not exist or has no
 * children.
 */
export function childNodes(
  map: MarkdownSourceMap,
  parentId: MarkdownNodeId,
): readonly MarkdownSourceNode[] {
  return map.nodes.filter((n) => n.parentId === parentId);
}

/**
 * Returns every node whose range overlaps `[from, to)`. The result
 * is sorted by `range.from`, then by `ordinal`. Both inclusive and
 * exclusive boundaries are supported via the `from <= offset < to`
 * convention.
 */
export function nodesIntersecting(
  map: MarkdownSourceMap,
  from: number,
  to: number,
): readonly MarkdownSourceNode[] {
  const out = map.nodes.filter((n) => n.range.from < to && n.range.to > from);
  return out.sort((a, b) => {
    if (a.range.from !== b.range.from) return a.range.from - b.range.from;
    return a.ordinal - b.ordinal;
  });
}

/**
 * Returns the node nearest to `offset` whose kind is in `kinds` (or
 * any kind when `kinds` is omitted). Preference order:
 *
 *  1. exact-range contains the offset;
 *  2. otherwise, minimum distance from offset to the node's range;
 *  3. ties broken by source order.
 */
export function nearestNodeAt(
  map: MarkdownSourceMap,
  offset: number,
  kinds?: ReadonlySet<MarkdownSourceNodeKind>,
): MarkdownSourceNode | null {
  let best: MarkdownSourceNode | null = null;
  let bestDistance = Number.POSITIVE_INFINITY;
  for (const node of map.nodes) {
    if (kinds && !kinds.has(node.kind)) continue;
    let distance: number;
    if (offset >= node.range.from && offset < node.range.to) {
      distance = 0;
    } else if (offset < node.range.from) {
      distance = node.range.from - offset;
    } else {
      distance = offset - node.range.to + 1;
    }
    if (
      distance < bestDistance ||
      (distance === bestDistance && best && node.range.from < best.range.from)
    ) {
      bestDistance = distance;
      best = node;
    }
  }
  return best;
}
