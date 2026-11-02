/**
 * Candidate builder and initial source-map.
 *
 * Walks the mdast tree produced by `parseToTree`, walks every
 * top-level node, and recursively records the kinds listed in
 * Packet 02 §12.5. Stores logical offsets from mdast `position`
 * fields. Validates every recorded range against the logical source.
 * Records the exact top-level prefix `[priorEnd, start)` for each
 * top-level node. On parser failure, returns a parse error and one
 * fallback `other` candidate covering the full source.
 */

import type {
  Root,
  RootContent,
  Heading,
  Paragraph,
  Blockquote,
  List,
  Table,
  Code,
  Link,
  Image,
  InlineCode,
  Html,
  FootnoteDefinition,
  FootnoteReference,
  Yaml,
  Definition,
} from "mdast";
import { buildHeadingSlugs } from "../headingSlugs.js";
import { nodeText } from "../semanticBlocks.js";
import { parseToTree } from "../parseMarkdown.js";
import { markdownHash } from "./markdownHash.js";
import type { MarkdownNodeIdFactory } from "./MarkdownNodeIdFactory.js";
import { rangeOf } from "./MarkdownSourceMap.js";
import type {
  MarkdownSourceMap,
  MarkdownSourceNode,
  MarkdownSourceNodeCandidate,
  MarkdownSourceNodeKind,
  MarkdownSourceRange,
} from "./MarkdownSourceMap.js";

/**
 * Build candidate records for the supplied logical source. Always
 * returns a list of candidates; when the parser throws, the list
 * contains exactly one `"other"` candidate covering the full source.
 */
export function buildMarkdownSourceMapCandidates(logicalSource: string): MarkdownCandidateResult {
  let tree: Root;
  try {
    tree = parseToTree(logicalSource);
  } catch {
    const fallback: MarkdownSourceNodeCandidate = {
      kind: "other",
      mdastType: "fallback",
      parentCandidateIndex: null,
      topLevelCandidateIndex: 0,
      range: rangeOf(0, logicalSource.length),
      prefixFrom: 0,
      prefixTo: logicalSource.length,
      rawHash: markdownHash(logicalSource),
      semanticKey: "fallback",
      ordinal: 0,
    };
    return {
      candidates: [fallback],
      parseError: "parser-failed",
    };
  }
  const slugMap = buildHeadingSlugs(tree);
  const candidates: MarkdownSourceNodeCandidate[] = [];
  let topLevelIndex = 0;
  const topLevelChildren = tree.children;
  for (let i = 0; i < topLevelChildren.length; i++) {
    const node = topLevelChildren[i];
    const priorEnd = priorTopLevelEnd(topLevelChildren, i);
    const prefixFrom = priorEnd;
    const start = node.position?.start.offset;
    const end = node.position?.end.offset;
    if (typeof start !== "number" || typeof end !== "number") {
      continue;
    }
    const candidate = makeTopLevelCandidate(
      node,
      i,
      topLevelIndex,
      rangeOf(start, end),
      rangeOf(prefixFrom, start),
      slugMap,
      logicalSource,
    );
    candidates.push(candidate);
    topLevelIndex++;
    walkChildren(
      node,
      candidates,
      candidates.length - 1,
      topLevelIndex - 1,
      slugMap,
      logicalSource,
    );
  }
  validateCandidates(candidates, logicalSource.length);
  return { candidates, parseError: null };
}

/**
 * Result of candidate build.
 */
export interface MarkdownCandidateResult {
  readonly candidates: readonly MarkdownSourceNodeCandidate[];
  /**
   * `null` on success; a short tag string on parser failure.
   * Callers must treat the presence of this field as a signal to
   * fall back to the single "other" candidate.
   */
  readonly parseError: string | null;
}

/**
 * Create the initial source-map (revision 0) by minting IDs in
 * candidate order. Parent and top-level IDs are filled in after all
 * candidates are allocated.
 */
export function createInitialMarkdownSourceMap(
  logicalSource: string,
  revision: number,
  ids: MarkdownNodeIdFactory,
): MarkdownSourceMap {
  const built = buildMarkdownSourceMapCandidates(logicalSource);
  const nodes: MarkdownSourceNode[] = built.candidates.map((candidate) => ({
    id: ids.create(),
    kind: candidate.kind,
    mdastType: candidate.mdastType,
    parentId: null,
    topLevelIndex: candidate.topLevelCandidateIndex,
    ordinal: candidate.ordinal,
    depth: candidate.depth,
    headingId: candidate.headingId,
    range: candidate.range,
    prefixRange: rangeOf(candidate.prefixFrom, candidate.prefixTo),
    rawHash: candidate.rawHash,
    semanticKey: candidate.semanticKey,
  }));
  // Resolve parent IDs and top-level boundaries.
  const topLevelStarts: number[] = [];
  let lastTopLevelIndex = -1;
  for (let i = 0; i < nodes.length; i++) {
    const candidate = built.candidates[i];
    const node = nodes[i];
    if (candidate.parentCandidateIndex !== null) {
      const parentId = nodes[candidate.parentCandidateIndex]?.id ?? null;
      nodes[i] = { ...node, parentId };
    }
    if (node.topLevelIndex !== lastTopLevelIndex) {
      topLevelStarts.push(i);
      lastTopLevelIndex = node.topLevelIndex;
    }
  }
  // Final terminator so callers can slice the last top-level group.
  topLevelStarts.push(nodes.length);
  return {
    revision,
    nodes,
    topLevelStartIndex: topLevelStarts,
    logicalSource,
  };
}

function priorTopLevelEnd(children: RootContent[], i: number): number {
  if (i === 0) return 0;
  const prev = children[i - 1];
  const end = prev?.position?.end.offset;
  return typeof end === "number" ? end : 0;
}

function walkChildren(
  node: RootContent,
  out: MarkdownSourceNodeCandidate[],
  parentIndex: number,
  topLevelIndex: number,
  slugMap: Map<Heading, string>,
  logicalSource: string,
): void {
  const children = (node as { children?: RootContent[] }).children;
  if (!Array.isArray(children)) return;
  for (let i = 0; i < children.length; i++) {
    const child = children[i];
    const start = child.position?.start.offset;
    const end = child.position?.end.offset;
    if (typeof start !== "number" || typeof end !== "number") continue;
    const kind = mapKind(child.type);
    if (!shouldRecordNested(kind, child.type)) continue;
    const candidate = makeNestedCandidate(
      child,
      kind,
      parentIndex,
      topLevelIndex,
      i,
      rangeOf(start, end),
      rangeOf(start, start),
      slugMap,
      logicalSource,
    );
    out.push(candidate);
    walkChildren(child, out, out.length - 1, topLevelIndex, slugMap, logicalSource);
  }
}

function shouldRecordNested(kind: MarkdownSourceNodeKind, mdastType: string): boolean {
  switch (kind) {
    case "heading":
    case "link":
    case "image":
    case "inline-math":
    case "footnote-reference":
    case "footnote-definition":
    case "html":
    case "code":
      return true;
    default:
      // code/html/inline-math arrive here via mdast type checks too.
      return mdastType === "code" || mdastType === "html" || mdastType === "inlineCode";
  }
}

function makeTopLevelCandidate(
  node: RootContent,
  ordinal: number,
  topLevelIndex: number,
  range: MarkdownSourceRange,
  prefixRange: MarkdownSourceRange,
  slugMap: Map<Heading, string>,
  logicalSource: string,
): MarkdownSourceNodeCandidate {
  const kind = mapKind(node.type);
  const raw = logicalSource.slice(range.from, range.to);
  const depth = node.type === "heading" ? headingDepth(node) : undefined;
  const headingId = node.type === "heading" ? slugMap.get(node as Heading) : undefined;
  const semanticKey = makeSemanticKey(kind, node, logicalSource, range);
  return {
    kind,
    mdastType: node.type,
    parentCandidateIndex: null,
    topLevelCandidateIndex: topLevelIndex,
    range,
    prefixFrom: prefixRange.from,
    prefixTo: prefixRange.to,
    rawHash: markdownHash(raw),
    semanticKey,
    ordinal,
    depth,
    headingId,
  };
}

function makeNestedCandidate(
  node: RootContent,
  kind: MarkdownSourceNodeKind,
  parentCandidateIndex: number,
  topLevelIndex: number,
  ordinal: number,
  range: MarkdownSourceRange,
  prefixRange: MarkdownSourceRange,
  slugMap: Map<Heading, string>,
  logicalSource: string,
): MarkdownSourceNodeCandidate {
  const raw = logicalSource.slice(range.from, range.to);
  const depth = node.type === "heading" ? headingDepth(node) : undefined;
  const headingId = node.type === "heading" ? slugMap.get(node as Heading) : undefined;
  const semanticKey = makeSemanticKey(kind, node, logicalSource, range);
  return {
    kind,
    mdastType: node.type,
    parentCandidateIndex,
    topLevelCandidateIndex: topLevelIndex,
    range,
    prefixFrom: prefixRange.from,
    prefixTo: prefixRange.to,
    rawHash: markdownHash(raw),
    semanticKey,
    ordinal,
    depth,
    headingId,
  };
}

function mapKind(mdastType: string): MarkdownSourceNodeKind {
  switch (mdastType) {
    case "yaml":
      return "yaml";
    case "heading":
      return "heading";
    case "paragraph":
      return "paragraph";
    case "blockquote":
      return "blockquote";
    case "list":
      return "list";
    case "table":
      return "table";
    case "code":
      return "code";
    case "math":
      return "math";
    case "html":
      return "html";
    case "footnoteDefinition":
      return "footnote-definition";
    case "link":
      return "link";
    case "image":
      return "image";
    case "inlineMath":
      return "inline-math";
    case "footnoteReference":
      return "footnote-reference";
    case "inlineCode":
      return "inline-code";
    default:
      return "other";
  }
}

/**
 * Loose interface for math/inline-math nodes. Math nodes are added
 * by remark plugins; the official `mdast` package does not ship a
 * `Math` type, so we duck-type it instead of importing from a plugin
 * package.
 */
interface MathLike {
  readonly type: string;
  readonly value?: string;
}

function headingDepth(node: RootContent): 1 | 2 | 3 | 4 | 5 | 6 | undefined {
  if (node.type !== "heading") return undefined;
  const d = (node as Heading).depth;
  if (d < 1 || d > 6) return undefined;
  return d as 1 | 2 | 3 | 4 | 5 | 6;
}

/**
 * Build the semantic key per the spec in Packet 02 §12.6.
 *
 * - `yaml`                  → `yaml`
 * - `heading`               → `<depth>\0<plain heading text>`
 * - `paragraph`, `blockquote` → first 80 normalized plain-text code units
 * - `list`                  → `ordered:<start>` or `unordered`
 * - `table`                 → `columns:<count>`
 * - `code`                  → `<language>\0<first 80 code code units>`
 * - `math`                  → first 80 value code units
 * - `html`                  → first 80 raw code units
 * - `footnote-definition`   → identifier
 * - `link`                  → `URL\0plain label`
 * - `image`                 → `URL\0alt`
 * - `inline-math`           → first 80 value code units
 * - `footnote-reference`    → identifier
 * - `other`                 → mdastType
 */
function makeSemanticKey(
  kind: MarkdownSourceNodeKind,
  node: RootContent,
  logicalSource: string,
  range: MarkdownSourceRange,
): string {
  switch (kind) {
    case "yaml":
      return "yaml";
    case "heading": {
      const d = headingDepth(node);
      const text = plainText(node);
      return `${d ?? "?"}\0${normalizeSemanticText(text)}`;
    }
    case "paragraph":
    case "blockquote":
      return normalizeSemanticText(plainText(node));
    case "list":
      return listSemanticKey(node as List);
    case "table":
      return tableSemanticKey(node as Table);
    case "code":
      return codeSemanticKey(node as Code);
    case "math":
      return slice80((node as MathLike).value ?? "");
    case "html":
      return slice80(rawValue(node, logicalSource, range));
    case "footnote-definition":
      return ((node as FootnoteDefinition).identifier ?? "").toString();
    case "link":
      return linkSemanticKey(node as Link);
    case "image":
      return imageSemanticKey(node as Image);
    case "inline-math":
      return slice80((node as MathLike).value ?? "");
    case "footnote-reference":
      return ((node as FootnoteReference).identifier ?? "").toString();
    case "inline-code":
      return slice80((node as InlineCode).value ?? "");
    default:
      return node.type;
  }
}

function rawValue(node: RootContent, logicalSource: string, range: MarkdownSourceRange): string {
  const v = (node as { value?: unknown }).value;
  if (typeof v === "string") return v;
  return logicalSource.slice(range.from, range.to);
}

function plainText(node: RootContent): string {
  return nodeText(node);
}

function normalizeSemanticText(text: string): string {
  const collapsed = text.replace(/\s+/g, " ").trim();
  return slice80PreservingSurrogates(collapsed.toLowerCase());
}

function listSemanticKey(node: List): string {
  if (node.ordered) return `ordered:${node.start ?? 1}`;
  return "unordered";
}

function tableSemanticKey(node: Table): string {
  const columns = Array.isArray(node.align) ? node.align.length : countColumns(node);
  return `columns:${columns}`;
}

function countColumns(node: Table): number {
  const firstRow = node.children?.[0];
  return Array.isArray(firstRow?.children) ? firstRow.children.length : 0;
}

function codeSemanticKey(node: Code): string {
  const lang = node.lang ?? "";
  return `${lang}\0${slice80PreservingSurrogates(node.value ?? "")}`;
}

function linkSemanticKey(node: Link): string {
  const label = nodeText(node as unknown as RootContent);
  return `${node.url}\0${normalizeSemanticText(label)}`;
}

function imageSemanticKey(node: Image): string {
  return `${node.url}\0${node.alt ?? ""}`;
}

function slice80(text: string): string {
  return slice80PreservingSurrogates(text);
}

function slice80PreservingSurrogates(text: string): string {
  if (text.length <= 80) return text;
  const last = text.charCodeAt(79);
  if (last >= 0xd800 && last <= 0xdbff) return text.slice(0, 79);
  return text.slice(0, 80);
}

function validateCandidates(
  candidates: readonly MarkdownSourceNodeCandidate[],
  logicalLength: number,
): void {
  for (let i = 0; i < candidates.length; i++) {
    const c = candidates[i];
    if (c.range.from < 0 || c.range.to > logicalLength) {
      throw new Error(
        `candidate ${i}: range out of bounds (${c.range.from}, ${c.range.to} vs ${logicalLength})`,
      );
    }
    if (c.prefixFrom < 0 || c.prefixTo > logicalLength) {
      throw new Error(`candidate ${i}: prefix out of bounds (${c.prefixFrom}, ${c.prefixTo})`);
    }
  }
}

// Type-only re-exports for consumers who need mdast types.
export type {
  Root,
  RootContent,
  Heading,
  Paragraph,
  Blockquote,
  List,
  Table,
  Code,
  Link,
  Image,
  Html,
  FootnoteDefinition,
  FootnoteReference,
  Yaml,
  Definition,
};
