/**
 * Block index (docs/plans/2026-10-17-block-html-editor.md, phase 1).
 *
 * A pure projection of authored source ranges: built from the cached parse5 tree, never the live
 * DOM, never serialized back. Every body element is accounted for as either a typed block or a
 * Locked raw block with a reason; sibling ranges never overlap. Rebuild from source after every
 * commit with `buildHtmlBlockIndex(committedSource)` (or `createHtmlBlockIndexer().get`).
 */
import {
  buildHtmlShellInfo,
  parseHtmlTree,
  sourceChecksum,
  type HtmlParseNode,
  type SourceRange,
} from "../sourceMap";

export type BlockKind =
  | "paragraph"
  | "heading"
  | "list"
  | "quote"
  | "code"
  | "image"
  | "table"
  | "group"
  | "separator"
  | "embed"
  | "raw";

export type LockReason =
  | "source-only-tag"
  | "custom-element"
  | "unmapped"
  | "ambiguous"
  | "mixed-inline"
  | "unrecognized";

export interface HtmlBlock {
  /** Stable within one revision: `${childPath}:${fingerprint}` (runs/clusters use `@offset`). */
  id: string;
  kind: BlockKind;
  /** Outer range (start tag start .. end tag end). */
  range: SourceRange;
  innerRange?: SourceRange;
  /** Element-only path from <body>; same contract as sourceMap.ts. Empty for runs/clusters. */
  childPath: number[];
  fingerprint: string;
  locked: false | LockReason;
  /** element: one authored element; run: stray inline content; cluster: overlapping repair artefacts. */
  origin: "element" | "run" | "cluster";
  tagName?: string;
  children?: HtmlBlock[];
  /** Whitespace/comments between the previous sibling (or parent inner start) and this block. */
  gapBefore: SourceRange;
}

export interface HtmlBlockIndex {
  /** sourceChecksum of the indexed source. */
  revision: string;
  bodyInner: SourceRange;
  blocks: HtmlBlock[];
  /** Depth-first flat list of every block including descendants. */
  flat: HtmlBlock[];
  /** Every locked block range (non-overlapping, sorted). Feed to the engine's `lockedRanges`. */
  lockedRanges: SourceRange[];
  /** Elements with no usable source location; never silently dropped. */
  unplaced: Array<{ childPath: number[]; tagName: string; reason: string }>;
}

const EMBED_TAGS = new Set(["iframe", "video", "audio", "object", "embed"]);
// Mirrors SOURCE_ONLY_TAGS in sourceMap.ts (kept private there).
const SOURCE_ONLY = new Set([
  "script",
  "style",
  "template",
  "iframe",
  "canvas",
  "svg",
  "form",
  "input",
  "select",
  "textarea",
  "button",
  "video",
  "audio",
  "source",
  "track",
  "object",
  "embed",
]);
const VOID_TAGS = new Set([
  "area",
  "base",
  "br",
  "col",
  "embed",
  "hr",
  "img",
  "input",
  "link",
  "meta",
  "source",
  "track",
  "wbr",
]);
const INLINE_TAGS = new Set([
  "a",
  "abbr",
  "b",
  "bdi",
  "bdo",
  "br",
  "cite",
  "code",
  "data",
  "dfn",
  "em",
  "i",
  "kbd",
  "mark",
  "q",
  "s",
  "samp",
  "small",
  "span",
  "strong",
  "sub",
  "sup",
  "time",
  "u",
  "var",
  "wbr",
]);
const CONTAINER_TAGS = new Set(["div", "section", "article", "main"]);
const BLOCK_LEVEL = new Set([
  "p",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "ul",
  "ol",
  "blockquote",
  "pre",
  "figure",
  "table",
  "hr",
  "div",
  "section",
  "article",
  "main",
]);

interface ElementItem {
  range: SourceRange;
  origin: "element";
  node: HtmlParseNode;
  path: number[];
}
interface RunItem {
  range: SourceRange;
  origin: "run";
}
type Item = ElementItem | RunItem;

function rangeOf(node: HtmlParseNode): SourceRange | null {
  const loc = node.sourceCodeLocation;
  if (!loc || typeof loc.startOffset !== "number" || typeof loc.endOffset !== "number") return null;
  return loc.endOffset > loc.startOffset ? { start: loc.startOffset, end: loc.endOffset } : null;
}

function textContent(node: HtmlParseNode): string {
  if (node.nodeName === "#text") return node.value ?? "";
  let out = "";
  for (const child of node.childNodes ?? []) out += textContent(child);
  return out;
}

function attr(node: HtmlParseNode, name: string): string {
  return node.attrs?.find((a) => a.name === name)?.value ?? "";
}

/** parse5 twin of `htmlBlockFingerprint` (sourceMap.ts): tag|id|class|normalized text[0..180]. */
export function htmlBlockFingerprintOfNode(node: HtmlParseNode): string {
  const tag = String(node.tagName ?? "").toLowerCase();
  const text = textContent(node).replace(/\s+/g, " ").trim().slice(0, 180);
  return `${tag}|${attr(node, "id")}|${attr(node, "class")}|${text}`;
}

const elementChildren = (node: HtmlParseNode) =>
  (node.childNodes ?? []).filter((child) => !!child.tagName);

function tagOf(node: HtmlParseNode): string {
  return String(node.tagName ?? "").toLowerCase();
}

function hasAuthoredEnd(node: HtmlParseNode): boolean {
  const tag = tagOf(node);
  if (VOID_TAGS.has(tag)) return true;
  return !!node.sourceCodeLocation?.endTag;
}

function classify(node: HtmlParseNode): { kind: BlockKind; locked: false | LockReason } {
  const tag = tagOf(node);
  if (tag.includes("-")) return { kind: "raw", locked: "custom-element" };
  if (SOURCE_ONLY.has(tag))
    return { kind: EMBED_TAGS.has(tag) ? "embed" : "raw", locked: "source-only-tag" };
  if (!hasAuthoredEnd(node)) return { kind: "raw", locked: "unmapped" };
  if (tag === "p") return { kind: "paragraph", locked: false };
  if (/^h[1-6]$/.test(tag)) return { kind: "heading", locked: false };
  if (tag === "ul" || tag === "ol") return { kind: "list", locked: false };
  if (tag === "blockquote") return { kind: "quote", locked: false };
  if (tag === "pre") return { kind: "code", locked: false };
  if (tag === "hr") return { kind: "separator", locked: false };
  if (tag === "img") return { kind: "image", locked: false };
  if (tag === "table") return { kind: "table", locked: false };
  if (tag === "figure") {
    const kids = elementChildren(node);
    const imgs = kids.filter((k) => tagOf(k) === "img").length;
    const others = kids.filter((k) => tagOf(k) !== "img" && tagOf(k) !== "figcaption").length;
    return imgs === 1 && others === 0
      ? { kind: "image", locked: false }
      : { kind: "raw", locked: "unrecognized" };
  }
  if (CONTAINER_TAGS.has(tag)) return { kind: "group", locked: false };
  return { kind: "raw", locked: "unrecognized" };
}

function isInlineLevel(node: HtmlParseNode): boolean {
  if (node.nodeName === "#text") return (node.value ?? "").trim().length > 0;
  const tag = tagOf(node);
  return !!tag && INLINE_TAGS.has(tag);
}

function buildLevel(
  source: string,
  parent: HtmlParseNode,
  parentPath: number[],
  inner: SourceRange,
  unplaced: HtmlBlockIndex["unplaced"],
): HtmlBlock[] {
  const items: Item[] = [];
  let elementIndex = 0;
  let runStart = -1;
  let runEnd = -1;
  const flushRun = () => {
    if (runStart >= 0) items.push({ range: { start: runStart, end: runEnd }, origin: "run" });
    runStart = -1;
    runEnd = -1;
  };
  for (const child of parent.childNodes ?? []) {
    const isElement = !!child.tagName;
    const path = isElement ? [...parentPath, elementIndex++] : [];
    const range = rangeOf(child);
    if (isElement && INLINE_TAGS.has(tagOf(child)) && range) {
      // Inline element at block level: part of a stray-inline run, never a typed block.
      if (runStart < 0) runStart = range.start;
      runEnd = Math.max(runEnd, range.end);
      continue;
    }
    if (!isElement) {
      if (isInlineLevel(child) && range) {
        if (runStart < 0) runStart = range.start;
        runEnd = Math.max(runEnd, range.end);
      }
      continue; // whitespace text and comments belong to gaps
    }
    flushRun();
    if (!range || range.start < inner.start || range.end > inner.end) {
      unplaced.push({
        childPath: path,
        tagName: tagOf(child),
        reason: range ? "outside-parent" : "no-source-location",
      });
      continue;
    }
    items.push({ range, origin: "element", node: child, path });
  }
  flushRun();

  items.sort((a, b) => a.range.start - b.range.start || b.range.end - a.range.end);

  // Merge overlapping items (foster parenting, parser repair) into one locked cluster.
  const groups: Item[][] = [];
  let groupEnd = -1;
  for (const item of items) {
    const last = groups[groups.length - 1];
    if (last && item.range.start < groupEnd) {
      last.push(item);
      groupEnd = Math.max(groupEnd, item.range.end);
    } else {
      groups.push([item]);
      groupEnd = item.range.end;
    }
  }

  const blocks: HtmlBlock[] = [];
  let cursor = inner.start;
  for (const group of groups) {
    const gapBefore = { start: cursor, end: group[0].range.start };
    const range = {
      start: group[0].range.start,
      end: Math.max(...group.map((g) => g.range.end)),
    };
    cursor = range.end;
    if (group.length > 1) {
      const firstElement = group.find((g): g is ElementItem => g.origin === "element");
      blocks.push({
        id: `cluster@${range.start}:${sourceChecksum(source.slice(range.start, range.end))}`,
        kind: "raw",
        range,
        childPath: firstElement?.path ?? [],
        fingerprint: sourceChecksum(source.slice(range.start, range.end)),
        locked: "unmapped",
        origin: "cluster",
        gapBefore,
      });
      continue;
    }
    const item = group[0];
    if (item.origin === "run") {
      const fingerprint = sourceChecksum(source.slice(range.start, range.end));
      blocks.push({
        id: `run@${range.start}:${fingerprint}`,
        kind: "raw",
        range,
        childPath: [],
        fingerprint,
        locked: "mixed-inline",
        origin: "run",
        gapBefore,
      });
      continue;
    }
    const node = item.node;
    const { kind, locked } = classify(node);
    const fingerprint = htmlBlockFingerprintOfNode(node);
    const block: HtmlBlock = {
      id: `${item.path.join(".")}:${fingerprint}`,
      kind,
      range,
      childPath: item.path,
      fingerprint,
      locked,
      origin: "element",
      tagName: tagOf(node),
      gapBefore,
    };
    const loc = node.sourceCodeLocation;
    if (loc?.startTag && loc.endTag && loc.endTag.startOffset >= loc.startTag.endOffset)
      block.innerRange = { start: loc.startTag.endOffset, end: loc.endTag.startOffset };
    // Descend only into containers; a locked block (or locked ancestor) is one opaque range.
    const descend =
      !locked &&
      block.innerRange &&
      (kind === "group" ||
        (kind === "quote" &&
          elementChildren(node).some((k) => BLOCK_LEVEL.has(tagOf(k)))));
    if (descend && block.innerRange)
      block.children = buildLevel(source, node, item.path, block.innerRange, unplaced);
    blocks.push(block);
  }
  return blocks;
}

function flatten(blocks: HtmlBlock[], out: HtmlBlock[] = []): HtmlBlock[] {
  for (const block of blocks) {
    out.push(block);
    if (block.children) flatten(block.children, out);
  }
  return out;
}

export function buildHtmlBlockIndex(source: string): HtmlBlockIndex {
  const revision = sourceChecksum(source);
  const bodyInner = { ...buildHtmlShellInfo(source).bodyInner };
  const empty: HtmlBlockIndex = {
    revision,
    bodyInner,
    blocks: [],
    flat: [],
    lockedRanges: [],
    unplaced: [],
  };
  let doc: HtmlParseNode;
  try {
    doc = parseHtmlTree(source);
  } catch {
    return empty;
  }
  const html = (doc.childNodes ?? []).find((n) => tagOf(n) === "html");
  const body = (html?.childNodes ?? []).find((n) => tagOf(n) === "body");
  if (!body) return empty;
  const unplaced: HtmlBlockIndex["unplaced"] = [];
  const blocks = buildLevel(source, body, [], bodyInner, unplaced);
  const flat = flatten(blocks);
  // Locked blocks are never descended into, so their ranges are non-overlapping by construction.
  const lockedRanges = flat
    .filter((b) => b.locked)
    .map((b) => ({ ...b.range }))
    .sort((a, b) => a.start - b.start);
  return { revision, bodyInner, blocks, flat, lockedRanges, unplaced };
}

/** Violations of the phase-1 invariants; empty means the index is sound. */
export function findBlockIndexViolations(index: HtmlBlockIndex): string[] {
  const out: string[] = [];
  const check = (siblings: HtmlBlock[], parent: SourceRange, label: string) => {
    let cursor = parent.start;
    for (const block of siblings) {
      if (block.range.start < cursor)
        out.push(`${label}: ${block.id} overlaps previous sibling or precedes parent`);
      if (block.range.end > parent.end) out.push(`${label}: ${block.id} exceeds parent`);
      if (block.range.end <= block.range.start) out.push(`${label}: ${block.id} empty range`);
      if (block.gapBefore.end !== block.range.start || block.gapBefore.start > block.range.start)
        out.push(`${label}: ${block.id} gapBefore not adjacent`);
      if (block.locked && block.children) out.push(`${label}: locked ${block.id} has children`);
      cursor = Math.max(cursor, block.range.end);
      if (block.children && block.innerRange) check(block.children, block.innerRange, block.id);
    }
  };
  check(index.blocks, index.bodyInner, "body");
  return out;
}

/**
 * Rebuild hook for the page buffer: call `get(committedSource)` after each commit. Memoized on
 * the last source string so repeated reads within one revision are free; there is no long-lived
 * tree that can drift (plan R1).
 */
export function createHtmlBlockIndexer(): { get(source: string): HtmlBlockIndex } {
  let lastSource: string | null = null;
  let lastIndex: HtmlBlockIndex | null = null;
  return {
    get(source: string): HtmlBlockIndex {
      if (lastIndex && lastSource === source) return lastIndex;
      lastIndex = buildHtmlBlockIndex(source);
      lastSource = source;
      return lastIndex;
    },
  };
}
