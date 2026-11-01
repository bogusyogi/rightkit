/**
 * Location-resolution kernel for the HTML operation planners (split out of
 * `htmlOperationPlanner.ts`, code moved verbatim): the memoised
 * authored-element index (`allLocations`), the exact source-range resolvers,
 * and the shared `HtmlOperationError` / `patch` primitives every planner
 * builds on. This module is the dependency leaf of the planner set — the
 * table, inline-text, and dispatcher modules all import from here.
 */
import { decodeHtmlEntities, parseHtmlTree } from "./sourceMap";
import type { HtmlParseNode } from "./sourceMap";
import type { HtmlSourcePatch, HtmlSourceRange } from "./htmlMutationEngine";

export class HtmlOperationError extends Error {
  constructor(
    readonly code: "target-unresolved" | "unsupported" | "ambiguous" | "incompatible",
    message: string,
  ) {
    super(message);
    this.name = "HtmlOperationError";
  }
}

/** Authored element location derived from parse5 source locations. */
export interface HtmlElementLocation {
  tagName: string;
  range: HtmlSourceRange;
  startTag: HtmlSourceRange;
  endTag: HtmlSourceRange | null;
  /** Attribute name -> range of the attribute VALUE (quotes excluded) when parse5 exposes it. */
  attrValues: Record<string, HtmlSourceRange>;
  /** Whole attribute ranges `name="value"` in source order. */
  attrs: Array<{ name: string; range: HtmlSourceRange }>;
}

/** One authored element plus its authored ancestor chain (nearest last). */
export interface LocatedNode {
  location: HtmlElementLocation;
  ancestors: readonly HtmlElementLocation[];
}

/**
 * Every authored element in the document, in source order, each carrying its
 * authored ancestor chain — built with ONE tree walk and memoised per source.
 *
 * Callers used to re-walk the whole tree per node (`locateElement` inside
 * `descendantElements`, `containingAncestor` per row), which made table column
 * planning quadratic: a 400-row table cost seconds of main-thread time. Every
 * lookup below is now a scan over this single precomputed list.
 */
let locationCache: {
  source: string;
  nodes: LocatedNode[];
  byRange: Map<string, LocatedNode>;
} | null = null;

export function rangeKey(range: HtmlSourceRange): string {
  return `${range.start}:${range.end}`;
}

export function allLocations(source: string): {
  nodes: LocatedNode[];
  byRange: Map<string, LocatedNode>;
} {
  if (locationCache && locationCache.source === source)
    return { nodes: locationCache.nodes, byRange: locationCache.byRange };
  let doc: HtmlParseNode;
  try {
    doc = parseHtmlTree(source);
  } catch {
    return { nodes: [], byRange: new Map() };
  }
  const nodes: LocatedNode[] = [];
  const byRange = new Map<string, LocatedNode>();
  const walk = (node: HtmlParseNode, ancestors: HtmlElementLocation[]): void => {
    const loc = node.sourceCodeLocation;
    let nextAncestors = ancestors;
    if (
      node.tagName &&
      loc &&
      typeof loc.startOffset === "number" &&
      typeof loc.endOffset === "number"
    ) {
      const location = locationFromNode(source, node);
      if (location) {
        const entry: LocatedNode = { location, ancestors: [...ancestors] };
        nodes.push(entry);
        // Exact source ranges are unique per authored node; first wins on any tie.
        if (!byRange.has(rangeKey(location.range))) byRange.set(rangeKey(location.range), entry);
        nextAncestors = [...ancestors, location];
      }
    }
    for (const child of node.childNodes ?? []) walk(child, nextAncestors);
  };
  for (const child of doc.childNodes ?? []) walk(child, []);
  nodes.sort((a, b) => a.location.range.start - b.location.range.start);
  locationCache = { source, nodes, byRange };
  return { nodes, byRange };
}

/**
 * Locate the authored element whose source range exactly matches `range`.
 * Exact-match only: ambiguity fails closed.
 */
export function locateElement(source: string, range: HtmlSourceRange): HtmlElementLocation | null {
  return allLocations(source).byRange.get(rangeKey(range))?.location ?? null;
}

/** Build an `HtmlElementLocation` from one parse5 element node. */
export function locationFromNode(source: string, found: HtmlParseNode): HtmlElementLocation | null {
  const loc = found.sourceCodeLocation;
  if (!loc) return null;
  const startTag = loc.startTag
    ? { start: loc.startTag.startOffset, end: loc.startTag.endOffset }
    : null;
  const endTag = loc.endTag ? { start: loc.endTag.startOffset, end: loc.endTag.endOffset } : null;
  const attrValues: Record<string, HtmlSourceRange> = {};
  const attrs: Array<{ name: string; range: HtmlSourceRange }> = [];
  for (const attr of found.attrs ?? []) {
    const name = String(attr.name);
    const attrLoc = loc.attrs?.[name];
    if (attrLoc && typeof attrLoc.startOffset === "number") {
      const whole = { start: attrLoc.startOffset, end: attrLoc.endOffset };
      attrs.push({ name, range: whole });
      // Narrow to the quoted/unquoted value part (mirrors getAttributeValueRange).
      const raw = source.slice(whole.start, whole.end);
      const eq = raw.indexOf("=");
      if (eq >= 0) {
        let vStart = whole.start + eq + 1;
        while (vStart < whole.end && /\s/.test(source[vStart] ?? "")) vStart++;
        const quote = source[vStart];
        if (quote === '"' || quote === "'") {
          const vEnd = source.indexOf(quote, vStart + 1);
          if (vEnd !== -1 && vEnd < whole.end) attrValues[name] = { start: vStart + 1, end: vEnd };
        } else {
          let vEnd = vStart;
          while (vEnd < whole.end && !/\s/.test(source[vEnd] ?? "") && source[vEnd] !== ">") vEnd++;
          attrValues[name] = { start: vStart, end: vEnd };
        }
      }
    }
  }
  if (!startTag) return null;
  return {
    tagName: String(found.tagName).toLowerCase(),
    range: { start: loc.startOffset, end: loc.endOffset },
    startTag,
    endTag,
    attrValues,
    attrs,
  };
}

/** All authored descendant elements (with source locations) inside `rootRange`. */
export function descendantElements(
  source: string,
  rootRange: HtmlSourceRange,
): HtmlElementLocation[] {
  return allLocations(source)
    .nodes.filter(
      (n) => n.location.range.start >= rootRange.start && n.location.range.end <= rootRange.end,
    )
    .map((n) => n.location);
}

/**
 * Resolve the inline element a block-edit click landed on. The bridge already
 * reports the clicked element's `href`/`src` (untrusted); match it against the
 * authored descendants of the resolved block. Exactly one match resolves; zero or
 * multiple is ambiguous and fails closed (§3.5). Attribute values are compared
 * decoded, since `getAttribute` returns entities decoded while the source slice
 * holds the raw authored bytes.
 */
export function resolveInlineAnchor(
  source: string,
  blockRange: HtmlSourceRange,
  reported: { href?: string; src?: string },
): { kind: "link" | "image"; element: HtmlElementLocation } | null {
  if (reported.href !== undefined) {
    const matches = descendantElements(source, blockRange).filter(
      (el) =>
        el.tagName === "a" &&
        el.attrValues.href !== undefined &&
        decodeHtmlEntities(source.slice(el.attrValues.href.start, el.attrValues.href.end)) ===
          reported.href,
    );
    return matches.length === 1 ? { kind: "link", element: matches[0] } : null;
  }
  if (reported.src !== undefined) {
    const matches = descendantElements(source, blockRange).filter(
      (el) =>
        el.tagName === "img" &&
        el.attrValues.src !== undefined &&
        decodeHtmlEntities(source.slice(el.attrValues.src.start, el.attrValues.src.end)) ===
          reported.src,
    );
    return matches.length === 1 ? { kind: "image", element: matches[0] } : null;
  }
  return null;
}

/**
 * Authored descendants inside `rootRange` whose NEAREST ancestor matching
 * `ownerTag` is `rootRange` itself. Structural surgery must never reach into a
 * nested structure of the same kind (a table inside a cell, a list inside an
 * item): those descendants belong to the inner owner, not this one.
 */
export function ownedDescendants(
  source: string,
  rootRange: HtmlSourceRange,
  tags: readonly string[],
  ownerTag: string,
): HtmlElementLocation[] {
  const tagSet = new Set(tags);
  return allLocations(source)
    .nodes.filter((n) => {
      const { range } = n.location;
      if (range.start < rootRange.start || range.end > rootRange.end) return false;
      if (!tagSet.has(n.location.tagName)) return false;
      const nearestOwner = [...n.ancestors].reverse().find((a) => a.tagName === ownerTag);
      return (
        nearestOwner !== undefined &&
        nearestOwner.range.start === rootRange.start &&
        nearestOwner.range.end === rootRange.end
      );
    })
    .map((n) => n.location);
}

export function patch(range: HtmlSourceRange, expectedText: string, text: string): HtmlSourcePatch {
  return { range, expectedText, text };
}
