/**
 * Local Markdown inspection.
 *
 * Given a snapshot and a request, returns a structured inspection
 * describing the nodes that contain the cursor. Pure: never calls
 * the filesystem or network.
 */

import { nearestNodeAt } from "../core/markdownSourceMapIndexes";
import type { MarkdownNodeId } from "../core/MarkdownNodeIdFactory";
import type {
  MarkdownInspectionEntry,
  MarkdownInspectionRequest,
  MarkdownInspectionResponse,
  MarkdownInspectionDetail,
} from "./inspectionTypes";
import type { MarkdownCoreSnapshot } from "../core/MarkdownDocumentTypes";
import type { MarkdownSourceNode } from "../core/MarkdownSourceMap";

const DEFAULT_MAX_NODES = 3;
const DEFAULT_SNIPPET_LENGTH = 240;

/**
 * Run inspection against the snapshot.
 */
export function inspectMarkdown(
  snapshot: MarkdownCoreSnapshot,
  request: MarkdownInspectionRequest,
): MarkdownInspectionResponse {
  const maxNodes = request.maxNodes ?? DEFAULT_MAX_NODES;
  const includeSnippets = request.includeSnippets ?? true;
  const snippetMaxLength = request.snippetMaxLength ?? DEFAULT_SNIPPET_LENGTH;
  const offset = request.offset;
  const nearest = nearestNodeAt(snapshot.sourceMap, offset);
  if (!nearest) {
    return { revision: snapshot.revision, entries: [] };
  }
  const ancestors: MarkdownSourceNode[] = [];
  let cursor: MarkdownSourceNode | null = nearest;
  while (cursor) {
    ancestors.unshift(cursor);
    const parentId: MarkdownNodeId | null = cursor.parentId;
    cursor = parentId ? (snapshot.sourceMap.nodes.find((n) => n.id === parentId) ?? null) : null;
  }
  const entries: MarkdownInspectionEntry[] = ancestors
    .slice(-maxNodes)
    .map((node) => buildEntry(snapshot, node, includeSnippets, snippetMaxLength));
  return { revision: snapshot.revision, entries };
}

function buildEntry(
  snapshot: MarkdownCoreSnapshot,
  node: MarkdownSourceNode,
  includeSnippets: boolean,
  snippetMaxLength: number,
): MarkdownInspectionEntry {
  const source = snapshot.logicalSource;
  const snippet = includeSnippets
    ? source.slice(node.range.from, Math.min(node.range.to, node.range.from + snippetMaxLength))
    : "";
  return {
    nodeId: node.id,
    kind: node.kind,
    range: node.range,
    snippet,
    details: buildDetails(node, source),
  };
}

function buildDetails(node: MarkdownSourceNode, source: string): MarkdownInspectionDetail[] {
  const out: MarkdownInspectionDetail[] = [];
  out.push({
    kind: "key-value",
    pairs: [
      { key: "kind", value: node.kind },
      { key: "mdastType", value: node.mdastType },
      ...(node.depth ? [{ key: "depth", value: String(node.depth) }] : []),
      ...(node.headingId ? [{ key: "slug", value: node.headingId }] : []),
      { key: "range", value: `${node.range.from}-${node.range.to}` },
    ],
  });
  if (node.kind === "code") {
    out.push({
      kind: "code",
      language: node.semanticKey.split("\0", 1)[0] ?? "",
      source: source.slice(node.range.from, node.range.to),
    });
  }
  return out;
}
