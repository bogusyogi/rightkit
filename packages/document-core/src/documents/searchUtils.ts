import {
  findLiteralMatches as findPlainMatches,
  findRegexMatches,
  regexPatternError,
} from "@rightkit/pdf-ui";

// Literal and regex find live in @rightkit/pdf-ui; this is the one app import site. Regex mode
// follows pdf-ui's strict pattern policy (see `regexPatternError`).

/** True when `pattern` is a regular expression pdf-ui will run (same policy `findRegexMatches` uses). */
export function isValidSearchRegex(pattern: string): boolean {
  return regexPatternError(pattern) === null;
}

export interface TextNodeMapping {
  node: Text;
  start: number; // offset in flat text
  end: number; // offset in flat text
}

export interface TextMatch {
  start: number;
  end: number;
}

const SEARCH_HIGHLIGHT_STYLE_ID = "rk-document-search-highlight-styles";
type HighlightValue = object;
type HighlightCtor = new (...ranges: Range[]) => HighlightValue;
type HighlightRegistryWindow = Window & {
  CSS?: {
    highlights?: {
      delete: (name: string) => void;
      set: (name: string, highlight: HighlightValue) => void;
    };
  };
  Highlight?: HighlightCtor;
};

function ensureSearchHighlightStyles(win: Window) {
  const doc = win.document;
  if (doc.getElementById(SEARCH_HIGHLIGHT_STYLE_ID)) return;

  const style = doc.createElement("style");
  style.id = SEARCH_HIGHLIGHT_STYLE_ID;
  style.textContent = `
::highlight(search-match) {
  background-color: color-mix(in srgb, var(--copper, #0c6aa5) 40%, transparent);
  color: inherit;
}

::highlight(search-match-active) {
  background-color: var(--copper, #0c6aa5);
  color: var(--paper, #faf8f4);
}
`;
  doc.head.appendChild(style);
}

export function findLiteralMatches(
  text: string,
  query: string,
  options?: { matchCase?: boolean; wholeWord?: boolean; regex?: boolean },
): TextMatch[] {
  if (!query) return [];
  return options?.regex
    ? findRegexMatches(text, query, options)
    : findPlainMatches(text, query, options);
}

export function getVisibleTextNodes(root: Node, win: Window = window): TextNodeMapping[] {
  const mappings: TextNodeMapping[] = [];
  let currentOffset = 0;

  function walk(node: Node) {
    if (node.nodeType === Node.TEXT_NODE) {
      const text = node.nodeValue || "";
      if (text.length > 0) {
        mappings.push({
          node: node as Text,
          start: currentOffset,
          end: currentOffset + text.length,
        });
        currentOffset += text.length;
      }
    } else if (node.nodeType === Node.ELEMENT_NODE) {
      const el = node as Element;
      // A childless element (void tags like <img>/<br>, or an empty container) can't contribute a
      // text node either way — the recursion below would be a zero-iteration no-op regardless of its
      // own visibility. Skip the getComputedStyle() call entirely for these: it's a real cost (style
      // recalc) called once per element on every find pass, and this is a correctness-neutral
      // short-circuit, not a heuristic — nothing downstream can be affected by it.
      if (node.childNodes.length === 0) return;
      // Skip hidden nodes and stylesheet/script nodes. Use the element's own view (`win`) so this
      // works for nodes inside a (same-origin) iframe document, not just the top window.
      const style = win.getComputedStyle(el);
      if (style.display === "none" || style.visibility === "hidden") {
        return;
      }
      const tagName = el.tagName.toLowerCase();
      if (tagName !== "script" && tagName !== "style" && tagName !== "noscript") {
        for (let i = 0; i < node.childNodes.length; i++) {
          walk(node.childNodes[i]);
        }
      }
    }
  }

  walk(root);
  return mappings;
}

export function mapOffsetToDOM(
  mappings: TextNodeMapping[],
  offset: number,
): { node: Text; offset: number } {
  for (const m of mappings) {
    if (offset >= m.start && offset <= m.end) {
      return { node: m.node, offset: offset - m.start };
    }
  }
  // Fallback
  if (mappings.length > 0) {
    const last = mappings[mappings.length - 1];
    return { node: last.node, offset: last.end - last.start };
  }
  throw new Error("No text nodes found to map offset");
}

export function clearHighlights(win: Window = window) {
  const highlights = (win as HighlightRegistryWindow).CSS?.highlights;
  highlights?.delete("search-match");
  highlights?.delete("search-match-active");
}

// ── Span-wrapped highlighting (for static iframe DOM: Safe HTML viewer) ───────────────────────────
// Wrapping matches in inline <span>s paints the text's content-box (tight to the glyphs, like Chrome's
// find highlight) instead of the CSS Custom Highlight API's full line-box, which overlapped adjacent
// lines on pages with generous line-height. Only for non-React DOM — React-owned previews (markdown)
// keep the paint-only ::highlight path so reconciliation isn't fighting injected nodes.
const FIND_SPAN_CLASS = "vr-find-span";
const FIND_SPAN_ACTIVE = "vr-find-span-active";

export function clearSpanHighlights(root: HTMLElement) {
  const spans = root.querySelectorAll<HTMLElement>(`.${FIND_SPAN_CLASS}`);
  spans.forEach((span) => {
    const parent = span.parentNode;
    if (!parent) return;
    while (span.firstChild) parent.insertBefore(span.firstChild, span);
    parent.removeChild(span);
    parent.normalize();
  });
}

// splitText keeps the original node as the LEADING part, so wrapping matches back-to-front leaves
// earlier offsets valid even when two matches share one text node.
function wrapMatchSpans(mappings: TextNodeMapping[], match: TextMatch, win: Window): HTMLElement[] {
  const segs: { node: Text; s: number; e: number }[] = [];
  for (const m of mappings) {
    if (m.end <= match.start || m.start >= match.end) continue;
    const s = Math.max(match.start, m.start) - m.start;
    const e = Math.min(match.end, m.end) - m.start;
    if (e > s) segs.push({ node: m.node, s, e });
  }
  const spans: HTMLElement[] = [];
  for (let i = segs.length - 1; i >= 0; i--) {
    const seg = segs[i];
    try {
      let node = seg.node;
      if (seg.e < node.length) node.splitText(seg.e);
      if (seg.s > 0) node = node.splitText(seg.s);
      const span = win.document.createElement("span");
      span.className = FIND_SPAN_CLASS;
      node.parentNode?.insertBefore(span, node);
      span.appendChild(node);
      spans.unshift(span);
    } catch {
      /* skip an un-splittable segment */
    }
  }
  return spans;
}

export function applySpanHighlights(
  root: HTMLElement,
  matches: TextMatch[],
  activeIndex: number,
  win: Window = window,
  // Callers that just walked the DOM to compute `matches` (e.g. a fresh find pass) can pass that
  // same mapping through to skip a second, redundant getVisibleTextNodes() walk over an unchanged
  // DOM (nothing mutates between the two calls — clearSpanHighlights already ran before either).
  // Callers with no fresh mapping (e.g. next/prev re-highlighting the same match set) omit it and
  // this recomputes, exactly as before.
  precomputedMappings?: TextNodeMapping[],
) {
  clearSpanHighlights(root);
  if (!matches.length) return;
  const mappings = precomputedMappings ?? getVisibleTextNodes(root, win);
  const groups = matches.map(() => [] as HTMLElement[]);
  for (let i = matches.length - 1; i >= 0; i--)
    groups[i] = wrapMatchSpans(mappings, matches[i], win);
  groups.forEach((group, i) => {
    const cls = i === activeIndex ? `${FIND_SPAN_CLASS} ${FIND_SPAN_ACTIVE}` : FIND_SPAN_CLASS;
    group.forEach((span) => {
      span.className = cls;
    });
  });
  const active = activeIndex >= 0 ? groups[activeIndex]?.[0] : undefined;
  if (active) active.scrollIntoView({ block: "center" });
}

export function applyHighlights(ranges: Range[], activeIndex: number, win: Window = window) {
  const highlightWindow = win as HighlightRegistryWindow;
  const highlights = highlightWindow.CSS?.highlights;
  const Highlight = highlightWindow.Highlight;
  if (!highlights || !Highlight) return;

  ensureSearchHighlightStyles(win);
  clearHighlights(win);

  if (ranges.length === 0) return;

  const normalRanges: Range[] = [];
  const activeRanges: Range[] = [];

  ranges.forEach((range, idx) => {
    if (idx === activeIndex) {
      activeRanges.push(range);
    } else {
      normalRanges.push(range);
    }
  });

  if (normalRanges.length > 0) {
    highlights.set("search-match", new Highlight(...normalRanges));
  }
  if (activeRanges.length > 0) {
    highlights.set("search-match-active", new Highlight(...activeRanges));
  }
}
