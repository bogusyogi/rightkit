/**
 * Structural outline derived from the source-map.
 *
 * The outline is a tree of:
 *  - top-level headings (always visible)
 *  - any frontmatter block (top-level node, only when present)
 *
 * Each node carries the source-map node ID and the logical-source
 * range so the UI can jump to it on click.
 */

import type { MarkdownSourceNode } from "../core/MarkdownSourceMap.js";

export type OutlineNodeKind = "frontmatter" | "heading";

export interface OutlineNode {
  readonly id: string;
  readonly kind: OutlineNodeKind;
  readonly title: string;
  readonly level: number;
  readonly range: { readonly from: number; readonly to: number };
  readonly children: readonly OutlineNode[];
}

/**
 * Build the outline from the source-map. Headings are nested by
 * depth. The optional frontmatter block is always listed at the
 * top when present.
 */
export function buildMarkdownOutline(
  headings: readonly MarkdownSourceNode[],
  frontmatterNode: { id: string; range: { from: number; to: number } } | null,
): readonly OutlineNode[] {
  const roots: OutlineNode[] = [];
  if (frontmatterNode) {
    roots.push({
      id: frontmatterNode.id,
      kind: "frontmatter",
      title: "Properties",
      level: 0,
      range: frontmatterNode.range,
      children: [],
    });
  }
  const stack: OutlineNode[] = [];
  for (const heading of headings) {
    const depth = heading.depth ?? 1;
    // Heading semantic keys include their level before a NUL separator so
    // reconciliation can distinguish otherwise identical headings. UI labels
    // must expose only authored heading text.
    const semanticParts = heading.semanticKey.split("\0");
    const title = semanticParts[semanticParts.length - 1] ?? heading.semanticKey;
    const node: OutlineNode = {
      id: heading.id,
      kind: "heading",
      title,
      level: depth,
      range: heading.range,
      children: [],
    };
    while (stack.length > 0 && stack[stack.length - 1]?.level >= depth) {
      stack.pop();
    }
    if (stack.length === 0) {
      roots.push(node);
    } else {
      const parent = stack[stack.length - 1] as OutlineNode;
      (parent.children as OutlineNode[]).push(node);
    }
    stack.push(node);
  }
  return roots;
}

/**
 * Find the outline node nearest to `offset`. Used to highlight the
 * currently-focused heading.
 */
export function nearestOutlineNode(
  nodes: readonly OutlineNode[],
  offset: number,
): OutlineNode | null {
  let best: OutlineNode | null = null;
  let bestDistance = Number.POSITIVE_INFINITY;
  const visit = (node: OutlineNode): void => {
    if (offset >= node.range.from && offset < node.range.to) {
      best = node;
      bestDistance = 0;
      return;
    }
    const distance =
      offset < node.range.from ? node.range.from - offset : offset - node.range.to + 1;
    if (distance < bestDistance) {
      best = node;
      bestDistance = distance;
    }
    for (const child of node.children) visit(child);
  };
  for (const node of nodes) visit(node);
  return best;
}
