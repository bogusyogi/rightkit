/**
 * Inline-text planner for the HTML operation planner set (split out of
 * `htmlOperationPlanner.ts`, code moved verbatim): narrow, sibling-aligned
 * source surgery for block-editor "Done" commits, built on the shared
 * location kernel in `htmlPlanLocations.ts`.
 */
import { decodeHtmlEntities, parseHtmlFragmentForBody } from "./sourceMap.js";
import type { HtmlParseNode } from "./sourceMap.js";
import type { HtmlSourcePatch, HtmlSourceRange } from "./htmlMutationEngine.js";
import {
  htmlTextStyleCss,
  parseHtmlTextStyle,
  type HtmlTextStyleAttrs,
  type HtmlTextStyleProperty,
} from "./htmlTextStyle.js";
import { locationFromNode, patch, type HtmlElementLocation } from "./htmlPlanLocations.js";

// ── Inline text replace (narrow surgery for block-editor Done commits) ───────

/** One authored node inside a fragment, with own range, decoded text, & source-backed children. */
interface InlineSegment {
  /** Range within the fragment string's OWN coordinate space (not yet offset). */
  range: HtmlSourceRange;
  /** Recursive, decoded plain-text content — a conservative sibling-alignment signal. */
  text: string;
  /** Lowercase tag name, or null for a text/comment segment. */
  tagName: string | null;
  /** Content-only range for an authored non-void element. */
  innerRange: HtmlSourceRange | null;
  children: InlineSegment[];
  element: HtmlElementLocation | null;
}

function plainTextOf(node: HtmlParseNode): string {
  if (node.nodeName === "#text") return decodeHtmlEntities(String(node.value ?? ""));
  if (node.nodeName === "#comment") return "";
  let out = "";
  for (const child of node.childNodes ?? []) out += plainTextOf(child);
  return out;
}

/**
 * Segment a fragment into its ORDERED top-level children, each carrying its own
 * source range (relative to the fragment) and decoded plain-text identity. Returns
 * null when any top-level node lacks concrete source-code location (parser-implied
 * content) — narrow mapping is not safe there, so the caller must fall back.
 */
function segmentsOf(nodes: readonly HtmlParseNode[], html: string): InlineSegment[] | null {
  const segments: InlineSegment[] = [];
  for (const node of nodes) {
    const loc = node.sourceCodeLocation;
    if (!loc || typeof loc.startOffset !== "number" || typeof loc.endOffset !== "number")
      return null;
    const children = segmentsOf(node.childNodes ?? [], html);
    if (!children) return null;
    segments.push({
      range: { start: loc.startOffset, end: loc.endOffset },
      text: plainTextOf(node),
      tagName: node.tagName ? String(node.tagName).toLowerCase() : null,
      innerRange:
        typeof loc.startTag?.endOffset === "number" && typeof loc.endTag?.startOffset === "number"
          ? { start: loc.startTag.endOffset, end: loc.endTag.startOffset }
          : null,
      children,
      element: node.tagName ? locationFromNode(html, node) : null,
    });
  }
  return segments;
}

function topLevelSegments(html: string): InlineSegment[] | null {
  let tree: HtmlParseNode;
  try {
    tree = parseHtmlFragmentForBody(html);
  } catch {
    return null;
  }
  return segmentsOf(tree.childNodes ?? [], html);
}

const STYLE_PROPERTIES: Array<[HtmlTextStyleProperty, string]> = [
  ["fontFamily", "font-family"],
  ["fontSize", "font-size"],
  ["color", "color"],
  ["backgroundColor", "background-color"],
];

function styleDeclarationRanges(style: string) {
  const result = new Map<
    string,
    { whole: HtmlSourceRange; value: HtmlSourceRange; valueText: string }
  >();
  let start = 0;
  while (start < style.length) {
    const semicolon = style.indexOf(";", start);
    const end = semicolon < 0 ? style.length : semicolon + 1;
    const segmentEnd = semicolon < 0 ? style.length : semicolon;
    const colon = style.indexOf(":", start);
    if (colon >= start && colon < segmentEnd) {
      const name = style.slice(start, colon).trim().toLowerCase();
      let valueStart = colon + 1;
      let valueEnd = segmentEnd;
      while (valueStart < valueEnd && /\s/.test(style[valueStart] ?? "")) valueStart++;
      while (valueEnd > valueStart && /\s/.test(style[valueEnd - 1] ?? "")) valueEnd--;
      if (name)
        result.set(name, {
          whole: { start, end },
          value: { start: valueStart, end: valueEnd },
          valueText: style.slice(valueStart, valueEnd),
        });
    }
    if (end <= start) break;
    start = end;
  }
  return result;
}

function patchTextStyle(
  source: string,
  before: InlineSegment,
  after: InlineSegment,
  sourceOffset: number,
  afterHtml: string,
): HtmlSourcePatch[] {
  const beforeElement = before.element;
  const afterElement = after.element;
  if (!beforeElement || !afterElement) return [];
  const beforeStyleRange = beforeElement.attrValues.style;
  const afterStyleRange = afterElement.attrValues.style;
  // Attr-bearing spans stay as source-preserved wrappers in PM; generated text-style marks live
  // beneath them. Missing style on that lossy wrapper is not a user request to clear authored CSS.
  if (
    beforeStyleRange &&
    !afterStyleRange &&
    beforeElement.attrs.some((attribute) => attribute.name !== "style")
  )
    return [];
  const beforeStyle = beforeStyleRange
    ? source.slice(sourceOffset + beforeStyleRange.start, sourceOffset + beforeStyleRange.end)
    : "";
  const afterStyle = afterStyleRange
    ? afterHtml.slice(afterStyleRange.start, afterStyleRange.end)
    : "";
  const beforeAttrs = parseHtmlTextStyle(beforeStyle);
  const afterAttrs = parseHtmlTextStyle(afterStyle);
  if (STYLE_PROPERTIES.every(([property]) => beforeAttrs[property] === afterAttrs[property]))
    return [];
  const afterCss = htmlTextStyleCss(afterAttrs);
  if (!beforeStyleRange) {
    if (!afterCss) return [];
    let insertion = sourceOffset + beforeElement.startTag.end - 1;
    if (source[insertion - 1] === "/") insertion--;
    return [patch({ start: insertion, end: insertion }, "", ` style="${afterCss}"`)];
  }
  const declarations = styleDeclarationRanges(beforeStyle);
  const supportedNames = new Set(STYLE_PROPERTIES.map(([, css]) => css));
  const unsupported = [...declarations.entries()].some(([name]) => !supportedNames.has(name));
  if (!afterCss && !unsupported) {
    const whole = beforeElement.attrs.find((attribute) => attribute.name === "style")?.range;
    if (whole) {
      const range = {
        start: sourceOffset + whole.start,
        end: sourceOffset + whole.end,
      };
      return [patch(range, source.slice(range.start, range.end), "")];
    }
  }
  const patches: HtmlSourcePatch[] = [];
  const additions: Partial<HtmlTextStyleAttrs> = {};
  for (const [property, cssName] of STYLE_PROPERTIES) {
    const declaration = declarations.get(cssName);
    const nextValue = afterAttrs[property];
    if (declaration && nextValue === null) {
      const range = {
        start: sourceOffset + beforeStyleRange.start + declaration.whole.start,
        end: sourceOffset + beforeStyleRange.start + declaration.whole.end,
      };
      patches.push(patch(range, source.slice(range.start, range.end), ""));
    } else if (declaration && nextValue !== null && declaration.valueText !== nextValue) {
      const range = {
        start: sourceOffset + beforeStyleRange.start + declaration.value.start,
        end: sourceOffset + beforeStyleRange.start + declaration.value.end,
      };
      patches.push(patch(range, source.slice(range.start, range.end), nextValue));
    } else if (!declaration && nextValue !== null) additions[property] = nextValue;
  }
  const additionCss = htmlTextStyleCss(additions);
  if (additionCss) {
    const insertion = sourceOffset + beforeStyleRange.end;
    const prefix = beforeStyle.trim() && !beforeStyle.trimEnd().endsWith(";") ? ";" : "";
    patches.push(patch({ start: insertion, end: insertion }, "", `${prefix}${additionCss}`));
  }
  return patches;
}

function sliceSegments(html: string, segments: readonly InlineSegment[]): string {
  if (segments.length === 0) return "";
  return html.slice(segments[0].range.start, segments[segments.length - 1].range.end);
}

function patchSegments(
  source: string,
  before: readonly InlineSegment[],
  after: readonly InlineSegment[],
  ownerBefore: HtmlSourceRange,
  sourceOffset: number,
  afterHtml: string,
): HtmlSourcePatch[] | null {
  // Exact tag correspondence is safe even when decoded text is equal: output markup may have
  // changed below it (e.g. text -> <strong>text</strong>), while its authored wrapper must stay.
  if (
    before.length === after.length &&
    before.every((b, index) => b.tagName === after[index].tagName)
  ) {
    const patches: HtmlSourcePatch[] = [];
    for (let i = 0; i < before.length; i++) {
      const next = patchSegment(source, before[i], after[i], sourceOffset, afterHtml);
      if (!next) return null;
      patches.push(...next);
    }
    return patches;
  }

  // Formatting can split a text sibling (<strong>Hello</strong>) without changing several
  // authored inline siblings. Align a unique ordered correspondence, scoring exact tag+text above
  // a tag that is unique on both sides (its text may legitimately have changed in this Done).
  // Repeated indistinguishable anchors yield multiple pairings & fail closed rather than guessing.
  const tagCounts = (segments: readonly InlineSegment[]) => {
    const counts = new Map<string, number>();
    for (const segment of segments)
      if (segment.tagName) counts.set(segment.tagName, (counts.get(segment.tagName) ?? 0) + 1);
    return counts;
  };
  const beforeTags = tagCounts(before);
  const afterTags = tagCounts(after);
  const score = (b: InlineSegment, a: InlineSegment) => {
    if (!b.tagName || b.tagName !== a.tagName) return 0;
    if (b.text === a.text) return 2;
    return beforeTags.get(b.tagName) === 1 && afterTags.get(a.tagName) === 1 ? 1 : 0;
  };
  const suffix = Array.from({ length: before.length + 1 }, () =>
    Array<number>(after.length + 1).fill(0),
  );
  for (let i = before.length - 1; i >= 0; i--)
    for (let j = after.length - 1; j >= 0; j--)
      suffix[i][j] = score(before[i], after[j])
        ? score(before[i], after[j]) + suffix[i + 1][j + 1]
        : Math.max(suffix[i + 1][j], suffix[i][j + 1]);
  const prefix = Array.from({ length: before.length + 1 }, () =>
    Array<number>(after.length + 1).fill(0),
  );
  for (let i = 1; i <= before.length; i++)
    for (let j = 1; j <= after.length; j++)
      prefix[i][j] = score(before[i - 1], after[j - 1])
        ? score(before[i - 1], after[j - 1]) + prefix[i - 1][j - 1]
        : Math.max(prefix[i - 1][j], prefix[i][j - 1]);
  const correspondenceScore = suffix[0][0];
  const candidates: Array<[number, number]> = [];
  for (let i = 0; i < before.length; i++)
    for (let j = 0; j < after.length; j++)
      if (
        score(before[i], after[j]) &&
        prefix[i][j] + score(before[i], after[j]) + suffix[i + 1][j + 1] === correspondenceScore
      )
        candidates.push([i, j]);
  const seenBefore = new Set<number>();
  const seenAfter = new Set<number>();
  for (const [i, j] of candidates) {
    if (seenBefore.has(i) || seenAfter.has(j)) return null;
    seenBefore.add(i);
    seenAfter.add(j);
  }
  const pairs = candidates.sort((left, right) => left[0] - right[0]);
  if (pairs.some((pair, index) => index > 0 && pairs[index - 1][1] >= pair[1])) return null;

  const patches: HtmlSourcePatch[] = [];
  const unchangedText =
    before.map((segment) => segment.text).join("") ===
    after.map((segment) => segment.text).join("");
  const formattingSegment = (segment: InlineSegment): boolean =>
    ((segment.tagName === null && segment.text.length > 0) ||
      (segment.tagName !== null &&
        ["strong", "b", "em", "i", "u", "s", "strike", "code"].includes(segment.tagName))) &&
    segment.children.every(formattingSegment);
  const patchGap = (
    beforeStart: number,
    beforeEnd: number,
    afterStart: number,
    afterEnd: number,
    insertion: number,
  ): boolean => {
    const beforeGap = before.slice(beforeStart, beforeEnd);
    const afterGap = after.slice(afterStart, afterEnd);
    // Partial mark removal can move unchanged text outside its authored mark. Preserve matched
    // wrappers & allow only text/formatting gaps with identical overall text; structural nodes
    // still require the explicit fallback.
    if (beforeGap.length === 0 || afterGap.length === 0) {
      if (beforeGap.length === 0 && afterGap.length === 0) return true;
      if (!unchangedText || ![...beforeGap, ...afterGap].every(formattingSegment)) return false;
    }
    const range = beforeGap.length
      ? {
          start: sourceOffset + beforeGap[0].range.start,
          end: sourceOffset + beforeGap[beforeGap.length - 1].range.end,
        }
      : { start: sourceOffset + insertion, end: sourceOffset + insertion };
    patches.push(
      patch(range, source.slice(range.start, range.end), sliceSegments(afterHtml, afterGap)),
    );
    return true;
  };
  let prevBefore = 0;
  let prevAfter = 0;
  for (const [matchedBefore, matchedAfter] of pairs) {
    const insertion = before[matchedBefore]?.range.start ?? ownerBefore.end;
    if (!patchGap(prevBefore, matchedBefore, prevAfter, matchedAfter, insertion)) return null;
    const stable = patchSegment(
      source,
      before[matchedBefore],
      after[matchedAfter],
      sourceOffset,
      afterHtml,
    );
    if (!stable) return null;
    patches.push(...stable);
    prevBefore = matchedBefore + 1;
    prevAfter = matchedAfter + 1;
  }
  const tailInsertion = before.length ? before[before.length - 1].range.end : ownerBefore.end;
  if (!patchGap(prevBefore, before.length, prevAfter, after.length, tailInsertion)) return null;
  return patches;
}

function patchSegment(
  source: string,
  before: InlineSegment,
  after: InlineSegment,
  sourceOffset: number,
  afterHtml: string,
): HtmlSourcePatch[] | null {
  if (before.tagName !== after.tagName) return null;
  if (before.tagName && after.tagName && before.innerRange && after.innerRange) {
    // Reconcile content even if decoded text is equal: serialisers can add/remove marks without
    // changing characters. Source tags stay completely untouched.
    const stylePatches = patchTextStyle(source, before, after, sourceOffset, afterHtml);
    const innerPatches = patchSegments(
      source,
      before.children,
      after.children,
      before.innerRange,
      sourceOffset,
      afterHtml,
    );
    return innerPatches ? [...stylePatches, ...innerPatches] : null;
  }
  if (before.text === after.text) return [];
  const range = { start: sourceOffset + before.range.start, end: sourceOffset + before.range.end };
  return [
    patch(
      range,
      source.slice(range.start, range.end),
      afterHtml.slice(after.range.start, after.range.end),
    ),
  ];
}

/**
 * Plan the narrowest patch set for a block-editor "Done" commit: segment the
 * ORIGINAL inner HTML and freshly reserialized inner HTML into source-backed ordered
 * children. Corresponding authored wrappers keep exact start/end-tag bytes while
 * descendant text or markup changes patch their inner content. Unchanged siblings keep
 * exact attributes, quoting, entities, & spacing even when visual output omits them.
 *
 * Returns null when the top-level shape cannot be matched 1:1 (a genuine
 * structural edit — inserted/removed/reordered top-level nodes, or content that
 * failed to parse with concrete locations): the caller must escalate to
 * whole-range fragment regeneration, since narrow mapping is not sound there.
 */
export function planInlineTextReplace(
  source: string,
  innerRange: HtmlSourceRange,
  newInnerHtml: string,
): HtmlSourcePatch[] | null {
  const originalInner = source.slice(innerRange.start, innerRange.end);
  const before = topLevelSegments(originalInner);
  const after = topLevelSegments(newInnerHtml);
  if (!before || !after) return null;
  return patchSegments(source, before, after, innerRange, innerRange.start, newInnerHtml);
}
