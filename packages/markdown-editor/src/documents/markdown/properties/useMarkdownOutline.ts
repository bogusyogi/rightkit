/**
 * React hook for the structural outline.
 *
 * Subscribes to the core and rebuilds the outline on every
 * commit. The hook is pure with respect to React state; consumers
 * read the resulting `nodes` list and `activeNodeId`.
 */

import { useEffect, useMemo, useState } from "react";
import { buildMarkdownOutline, nearestOutlineNode } from "./markdownOutline.js";
import type { OutlineNode } from "./markdownOutline.js";
import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore.js";
import { readFrontmatterDocument } from "./frontmatterDocument.js";

export interface UseMarkdownOutlineOptions {
  readonly core: MarkdownDocumentCore | undefined;
  readonly caretOffset: number | null;
}

export interface UseMarkdownOutlineResult {
  readonly nodes: readonly OutlineNode[];
  readonly activeNodeId: string | null;
}

/**
 * Subscribe and rebuild the outline on every commit. Update the
 * active node when the caret offset changes.
 */
export function useMarkdownOutline(options: UseMarkdownOutlineOptions): UseMarkdownOutlineResult {
  const [revision, setRevision] = useState(0);
  const core = options.core;

  useEffect(() => {
    if (!core) return;
    const unsubscribe = core.subscribe(0, () => setRevision((r) => r + 1));
    return unsubscribe;
  }, [core]);

  const snapshot = core?.snapshot();

  const nodes = useMemo<readonly OutlineNode[]>(() => {
    if (!snapshot) return [];
    const fm = readFrontmatterDocument(snapshot.logicalSource);
    const fmNode = fm.range ? { id: "frontmatter", range: fm.range } : null;
    const headings = snapshot.sourceMap.nodes
      .filter((n) => n.kind === "heading")
      .map((node) => ({
        ...node,
        semanticKey: snapshot.logicalSource
          .slice(node.range.from, node.range.to)
          .replace(/^#{1,6}[ \t]*/, "")
          .trim(),
      }));
    void revision;
    return buildMarkdownOutline(headings, fmNode);
  }, [snapshot, revision]);

  const activeNodeId = useMemo<string | null>(() => {
    if (options.caretOffset === null) return null;
    const found = nearestOutlineNode(nodes, options.caretOffset);
    return found?.id ?? null;
  }, [nodes, options.caretOffset]);

  return { nodes, activeNodeId };
}
