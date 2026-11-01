// Find-in-page core (ported from ReadRight). Walks a (same-origin) document's
// visible text nodes, finds literal matches, and paints them with the CSS
// Custom Highlight API — paint-only, no DOM mutation, so it never disturbs the
// sanitized email markup. Works against any window, including a same-origin
// iframe's own window.

export interface TextNodeMapping {
  node: Text;
  start: number; // offset in the flattened text
  end: number;
}

export interface TextMatch {
  start: number;
  end: number;
}

export interface SearchOptions {
  matchCase?: boolean;
  wholeWord?: boolean;
}

/** What the FindBar consumes. The thread-wide coordinator implements this. */
export interface SearchResult {
  count: number;
  activeIndex: number; // 0-based; -1 when no matches
}
export interface SearchProvider {
  find(query: string, opts: SearchOptions): SearchResult;
  next(): SearchResult;
  prev(): SearchResult;
  close(): void;
  supportsMatchCase?: boolean;
  supportsWholeWord?: boolean;
}

/** Per-iframe search surface registered by each EmailFrame; aggregated by the
 *  coordinator. `paint(activeLocalIndex)` highlights this iframe's matches, with
 *  one optionally active (-1 = none active in this iframe). */
export interface IframeFind {
  el(): HTMLIFrameElement | null;
  search(query: string, opts: SearchOptions): number;
  paint(activeLocalIndex: number): void;
  clear(): void;
}

/** Literal find (match-case, whole-word) lives in @rightkit/pdf-ui. */
export { findLiteralMatches } from "@rightkit/pdf-ui";

export function getVisibleTextNodes(root: Node, win: Window = window): TextNodeMapping[] {
  const mappings: TextNodeMapping[] = [];
  let offset = 0;
  function walk(node: Node) {
    if (node.nodeType === Node.TEXT_NODE) {
      const text = node.nodeValue || "";
      if (text.length > 0) {
        mappings.push({ node: node as Text, start: offset, end: offset + text.length });
        offset += text.length;
      }
    } else if (node.nodeType === Node.ELEMENT_NODE) {
      const el = node as Element;
      const style = win.getComputedStyle(el);
      if (style.display === "none" || style.visibility === "hidden") return;
      const tag = el.tagName.toLowerCase();
      if (tag !== "script" && tag !== "style" && tag !== "noscript") {
        for (let i = 0; i < node.childNodes.length; i++) walk(node.childNodes[i]);
      }
    }
  }
  walk(root);
  return mappings;
}

export function mapOffsetToDOM(mappings: TextNodeMapping[], offset: number): { node: Text; offset: number } {
  for (const m of mappings) {
    if (offset >= m.start && offset <= m.end) return { node: m.node, offset: offset - m.start };
  }
  if (mappings.length > 0) {
    const last = mappings[mappings.length - 1];
    return { node: last.node, offset: last.end - last.start };
  }
  throw new Error("No text nodes to map offset");
}

type HighlightWin = Window & {
  CSS?: { highlights?: Map<string, unknown> };
  Highlight?: new (...ranges: Range[]) => unknown;
};

export function clearHighlights(win: Window = window) {
  const w = win as HighlightWin;
  if (w.CSS?.highlights) {
    w.CSS.highlights.delete("search-match");
    w.CSS.highlights.delete("search-match-active");
  }
}

/** Paint `ranges` in `win`'s highlight registry; `activeIndex` gets the active style. */
export function applyHighlights(ranges: Range[], activeIndex: number, win: Window = window) {
  const w = win as HighlightWin;
  if (!w.CSS?.highlights || !w.Highlight) return;
  clearHighlights(win);
  if (ranges.length === 0) return;
  const normal: Range[] = [];
  const active: Range[] = [];
  ranges.forEach((r, i) => (i === activeIndex ? active : normal).push(r));
  if (normal.length) w.CSS.highlights.set("search-match", new w.Highlight(...normal));
  if (active.length) w.CSS.highlights.set("search-match-active", new w.Highlight(...active));
}

/** Build DOM Ranges for matches against the current text nodes of `doc.body`. */
export function rangesForMatches(matches: TextMatch[], doc: Document, win: Window): Range[] {
  if (!doc.body) return [];
  const nodes = getVisibleTextNodes(doc.body, win);
  return matches.map((m) => {
    const s = mapOffsetToDOM(nodes, m.start);
    const e = mapOffsetToDOM(nodes, m.end);
    const r = doc.createRange();
    r.setStart(s.node, s.offset);
    r.setEnd(e.node, e.offset);
    return r;
  });
}
