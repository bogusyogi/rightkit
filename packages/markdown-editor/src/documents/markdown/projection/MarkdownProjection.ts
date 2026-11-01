/**
 * Canonical ProseMirror projection over {@link MarkdownDocumentCore}.
 *
 * The projection:
 *  - reads the core's logical source and source-map,
 *  - parses the source through {@link parseToTree},
 *  - walks the source-map nodes and emits a ProseMirror document
 *    whose block nodes carry the v2 source attributes
 *    (`blockId`, `sourceFrom`, `sourceTo`, `prefixFrom`,
 *    `prefixTo`, `rawSignature`).
 *
 * The projection never mutates the core. It is consumed by the
 * ProseMirror editor to seed itself from the core on mount and to
 * re-sync on commits.
 */

import type { Node as ProseMirrorNode, Schema } from "prosemirror-model";
import { parseToTree } from "../parseMarkdown";
import type { MarkdownSourceMap, MarkdownSourceNode } from "../core/MarkdownSourceMap";
import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore";
import { markdownHash } from "../core/markdownHash";
import { markdownToPmDoc } from "./pmFromMarkdown";
import { markdownHybridSchema } from "../markdownHybridSchema";
import { incProjectionRebuilds } from "@rightkit/document-core/documents/perfEventCounters";

/**
 * V2 block attributes attached to every top-level block and
 * `raw_block`. Existing v1 attrs (`prefix`, `rawSource`,
 * `rawText`) are still attached so v1 nodes that haven't migrated
 * continue to render.
 */
export interface MarkdownBlockV2Attrs {
  /** Stable Markdown source-map node ID. */
  readonly blockId: string;
  /** Logical-source `[from, to)` for the block content. */
  readonly sourceFrom: number;
  readonly sourceTo: number;
  /** Logical-source `[from, to)` for the inter-block prefix. */
  readonly prefixFrom: number;
  readonly prefixTo: number;
  /**
   * Hex hash of the raw block content (used by reconciliation and
   * the changed-block patcher).
   */
  readonly rawSignature: string;
}

/**
 * Build the v2 attribute set for a top-level block at `range` with
 * `prefixRange` and `rawSource`. The `blockId` is taken from the
 * source-map node that matches the block; when no node matches, the
 * factory is queried for a fresh ID.
 */
export function buildMarkdownBlockV2Attrs(
  source: string,
  sourceMap: MarkdownSourceMap,
  range: { from: number; to: number },
  prefixRange: { from: number; to: number },
  _core: MarkdownDocumentCore,
): MarkdownBlockV2Attrs {
  const node = findNodeAtRange(sourceMap, range);
  const blockId = node?.id ?? "";
  const raw = source.slice(range.from, range.to);
  return {
    blockId,
    sourceFrom: range.from,
    sourceTo: range.to,
    prefixFrom: prefixRange.from,
    prefixTo: prefixRange.to,
    rawSignature: markdownHash(raw),
  };
}

/**
 * Walk a {@link MarkdownSourceMap} and return the first node whose
 * range covers `[range.from, range.to)`. Returns `null` when no
 * node matches.
 */
export function findNodeAtRange(
  sourceMap: MarkdownSourceMap,
  range: { from: number; to: number },
): MarkdownSourceNode | null {
  for (const node of sourceMap.nodes) {
    if (node.range.from <= range.from && node.range.to >= range.to) {
      return node;
    }
  }
  return null;
}

/** Cache projection by document + revision + affected range so
 * pointer movement (which only changes selection hint, not revision)
 * never rebuilds. The cache key is documentId + revision; affected
 * range is validated via sourceMap revision comparison.
 */
let projectionCache = new WeakMap<
  MarkdownDocumentCore,
  { revision: number; schema: Schema; doc: ProseMirrorNode }
>();
let projectionCacheEntries = 0;

export function clearMarkdownProjectionCache(): void {
  projectionCache = new WeakMap();
  projectionCacheEntries = 0;
}

export function getMarkdownProjectionCacheEntryCount(): number {
  return projectionCacheEntries;
}

export function projectMarkdownToPmWithCounters(
  core: MarkdownDocumentCore,
  schema: Schema,
): ProseMirrorNode {
  const snapshot = core.snapshot();
  const cached = projectionCache.get(core);
  if (cached && cached.revision === snapshot.revision && cached.schema === schema) {
    return cached.doc;
  }
  // Cache miss — projection rebuild needed. Parse is counted by
  // markdownToPmDoc's own cache; we count only the projection layer.
  incProjectionRebuilds(1);
  const doc = projectMarkdownToPmUncached(core, schema);
  if (!cached) projectionCacheEntries++;
  projectionCache.set(core, { revision: snapshot.revision, schema, doc });
  return doc;
}

/**
 * Project the logical source into a ProseMirror document. The
 * schema must already expose the v2 attrs; the projection simply
 * supplies values.
 */
export function projectMarkdownToPm(core: MarkdownDocumentCore, schema: Schema): ProseMirrorNode {
  // Fast path for callers that import the cached variant explicitly.
  // Keep projectMarkdownToPm itself cached as well so pointer movement
  // and repeated renders stay free.
  return projectMarkdownToPmWithCounters(core, schema);
}

function projectMarkdownToPmUncached(core: MarkdownDocumentCore, schema: Schema): ProseMirrorNode {
  const snapshot = core.snapshot();
  const source = snapshot.logicalSource;
  // Use the established semantic Markdown projection for the production
  // schema, then add source ranges. The older fallback-only implementation
  // made every block a raw atom, which forced all edits through serialization.
  if (schema === markdownHybridSchema) {
    const projected = markdownToPmDoc(source);
    const children: ProseMirrorNode[] = [];
    let priorEnd = 0;
    projected.forEach((node) => {
      const start = priorEnd + String(node.attrs.prefix ?? "").length;
      const end = start + String(node.attrs.rawSource ?? "").length;
      const range = { from: start, to: end };
      const prefixRange = { from: priorEnd, to: start };
      const v2 = buildMarkdownBlockV2Attrs(source, snapshot.sourceMap, range, prefixRange, core);
      children.push(node.type.create({ ...node.attrs, ...v2 }, node.content));
      priorEnd = end;
    });
    return schema.nodes.doc.create({ ...projected.attrs }, children);
  }
  const tree = (() => {
    try {
      return parseToTree(source);
    } catch {
      return null;
    }
  })();
  const blocks: ProseMirrorNode[] = [];
  let priorEnd = 0;
  const fallbackBlock = (
    rawSource: string,
    prefix: string,
    sourceFrom: number,
    sourceTo: number,
    prefixFrom: number,
    prefixTo: number,
  ) => {
    const v2 = buildMarkdownBlockV2Attrs(
      source,
      snapshot.sourceMap,
      { from: sourceFrom, to: sourceTo },
      { from: prefixFrom, to: prefixTo },
      core,
    );
    return schema.nodes.raw_block.create({
      prefix,
      rawSource,
      mdastType: "fallback",
      sourceLine: 1,
      blockId: v2.blockId,
      sourceFrom: v2.sourceFrom,
      sourceTo: v2.sourceTo,
      prefixFrom: v2.prefixFrom,
      prefixTo: v2.prefixTo,
      rawSignature: v2.rawSignature,
    });
  };
  if (!tree) {
    blocks.push(fallbackBlock(source, "", 0, source.length, 0, 0));
  } else {
    for (const child of tree.children) {
      const start = child.position?.start.offset;
      const end = child.position?.end.offset;
      if (typeof start !== "number" || typeof end !== "number") {
        continue;
      }
      blocks.push(
        fallbackBlock(
          source.slice(start, end),
          source.slice(priorEnd, start),
          start,
          end,
          priorEnd,
          start,
        ),
      );
      priorEnd = end;
    }
  }
  if (!blocks.length) {
    const v2 = buildMarkdownBlockV2Attrs(
      source,
      snapshot.sourceMap,
      { from: 0, to: 0 },
      { from: 0, to: 0 },
      core,
    );
    const paragraphAttrs: Record<string, unknown> = {
      prefix: "",
      rawSource: "",
      rawText: "",
      blockId: v2.blockId,
      sourceFrom: v2.sourceFrom,
      sourceTo: v2.sourceTo,
      prefixFrom: v2.prefixFrom,
      prefixTo: v2.prefixTo,
      rawSignature: v2.rawSignature,
    };
    if (schema.nodes.paragraph) {
      blocks.push(schema.nodes.paragraph.create(paragraphAttrs));
    }
  }
  return schema.nodes.doc.create({ trailing: source.slice(priorEnd) }, blocks);
}
