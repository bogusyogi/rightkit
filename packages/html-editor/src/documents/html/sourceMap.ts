import * as parse5 from "parse5";
import { incParseCount } from "@rightkit/document-core/documents/perfEventCounters";
import { htmlSourceMapCache } from "./htmlResourceLifecycle";

export interface SourceRange {
  start: number;
  end: number;
}

export interface HtmlInspectTarget {
  text: string;
  selectedText?: string;
  tagName?: string;
  id?: string;
  classNames?: string[];
  href?: string;
  src?: string;
}

export interface HtmlTextSelection {
  from: number;
  to: number;
  text: string;
}

export interface HtmlInlineStyle {
  fontFamily?: string;
  fontSize?: string;
  fontWeight?: string;
  fontStyle?: string;
  lineHeight?: string;
  letterSpacing?: string;
  color?: string;
  backgroundColor?: string;
  textAlign?: string;
  padding?: string;
}

/** A Safe-preview click hint.  It is only ever resolved against current source in the parent. */
export interface HtmlSafeBlockTarget extends HtmlInspectTarget {
  pagePath: string;
  childPath: number[];
  fingerprint: string;
  geometry: { left: number; top: number; width: number; height: number };
  style?: HtmlInlineStyle;
  /** Text offsets captured from native Safe-preview selection before editor mount. */
  textSelection?: Omit<HtmlTextSelection, "text">;
}

export interface HtmlSafeBlockMatch {
  tagName: string;
  range: SourceRange;
  innerRange: SourceRange;
  checksum: string;
  /** Authored attributes on resolved block, preserving source order and values. */
  attributes: Array<{ name: string; value: string }>;
}

interface SafeParseNode {
  tagName?: string;
  childNodes?: SafeParseNode[];
  parentNode?: SafeParseNode | null;
  sourceCodeLocation?: Parse5Location;
  attrs?: Array<{ name: string; value: string }>;
}

const SAFE_BLOCK_TAGS = new Set([
  "p",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "li",
  "blockquote",
  "pre",
  "code",
  "figure",
  "figcaption",
  "table",
  "td",
  "th",
  "div",
  "section",
  "article",
]);
const SOURCE_ONLY_TAGS = new Set([
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
export const HTML_INLINE_EDITABLE_SELECTOR =
  "p,h1,h2,h3,h4,h5,h6,li,blockquote,pre,code,figure,figcaption,table,td,th,div,section,article";

export function htmlBlockFingerprint(element: Element): string {
  const tag = element.tagName.toLowerCase();
  return `${tag}|${element.id}|${element.className}|${(element.textContent ?? "").replace(/\s+/g, " ").trim().slice(0, 180)}`;
}

export function htmlElementPathFromBody(node: Element): number[] {
  return elementPathFromBody(node);
}

function inlineStyleFromElement(element: Element): HtmlInlineStyle | undefined {
  const win = element.ownerDocument.defaultView;
  if (!win) return undefined;
  const style = win.getComputedStyle(element);
  let backgroundElement: Element | null = element;
  let backgroundColor = "";
  while (backgroundElement) {
    const candidate = win.getComputedStyle(backgroundElement).backgroundColor;
    const normalized = candidate.trim().toLowerCase();
    const alpha = normalized.match(/^(?:rgba|hsla)\([^)]*,\s*([\d.]+)\s*\)$/);
    if (
      normalized &&
      normalized !== "transparent" &&
      !(alpha && Number.parseFloat(alpha[1]) === 0)
    ) {
      backgroundColor = candidate;
      break;
    }
    backgroundElement = backgroundElement.parentElement;
  }
  return {
    fontFamily: style.fontFamily,
    fontSize: style.fontSize,
    fontWeight: style.fontWeight,
    fontStyle: style.fontStyle,
    lineHeight: style.lineHeight,
    letterSpacing: style.letterSpacing,
    color: style.color,
    backgroundColor,
    textAlign: style.textAlign,
    padding: style.padding,
  };
}

// Last-value memo: call sites cluster many checksum(source) calls per edit/commit
// cycle against the same unchanged string identity — avoid rehashing the whole
// string each time. Keyed by string identity, not content, so it's O(1) on repeat
// calls with the same reference and falls back to a fresh hash otherwise.
let lastChecksumInput: string | null = null;
let lastChecksumOutput = "";

export function sourceChecksum(value: string): string {
  if (value === lastChecksumInput) return lastChecksumOutput;
  // Stable, non-cryptographic identity used only to detect a stale local source range.
  let hash = 2166136261;
  for (let i = 0; i < value.length; i++) hash = Math.imul(hash ^ value.charCodeAt(i), 16777619);
  const result = (hash >>> 0).toString(36);
  lastChecksumInput = value;
  lastChecksumOutput = result;
  return result;
}

// Cache parsed HTML trees by source checksum so pointer movement
// and repeated target resolution don't re-parse the same revision.
let taggedRangeCache: { checksum: string; source: string; ranges: TaggedRange[] } | null = null;

function getCachedTree(source: string): unknown | null {
  const checksum = sourceChecksum(source);
  const cached = htmlSourceMapCache.get(checksum);
  return cached?.source === source ? cached.tree : null;
}

function setCachedTree(source: string, tree: unknown): void {
  htmlSourceMapCache.set(sourceChecksum(source), { source, tree });
}

/**
 * Full cache entry lookup (tree + diagnostics, when captured), for consumers that
 * need parse diagnostics alongside the tree — e.g. the mutation engine's source-side
 * parse — without re-parsing a string sourceMap already has cached.
 */
export function getCachedParseEntry(
  source: string,
): { tree: HtmlParseNode; errors?: Array<{ code: string; offset: number }> } | null {
  const checksum = sourceChecksum(source);
  const cached = htmlSourceMapCache.get(checksum);
  if (!cached || cached.source !== source) return null;
  return { tree: cached.tree as HtmlParseNode, errors: cached.errors };
}

/** Cache a tree together with the diagnostics collected while parsing it. */
export function setCachedParseEntry(
  source: string,
  tree: HtmlParseNode,
  errors: Array<{ code: string; offset: number }>,
): void {
  htmlSourceMapCache.set(sourceChecksum(source), { source, tree, errors });
}

export function clearHtmlSourceMapCache(): void {
  htmlSourceMapCache.clear();
  taggedRangeCache = null;
}

function elementPathFromBody(node: Element): number[] {
  const path: number[] = [];
  let current: Element | null = node;
  while (current && current.parentElement && current.parentElement !== current.ownerDocument.body) {
    const parent: Element = current.parentElement;
    path.unshift(Array.prototype.indexOf.call(parent.children, current));
    current = parent;
  }
  if (current && current.parentElement === current.ownerDocument.body)
    path.unshift(Array.prototype.indexOf.call(current.ownerDocument.body.children, current));
  return path;
}

/** Prefer the source-equivalent path stamped before sanitizing (custom elements are flattened in the Safe DOM). */
function stampedOrComputedPath(target: Element): number[] {
  const stamp = target.getAttribute("data-vr-edit-path");
  if (stamp) {
    const path = stamp.split(".").filter(Boolean).map(Number);
    if (path.length && path.every((n) => Number.isInteger(n) && n >= 0)) return path;
  }
  return elementPathFromBody(target);
}

export function safeBlockTargetFromElement(
  element: Element,
  pagePath: string,
  _geometry: DOMRect,
  capturedSelection?: HtmlTextSelection,
): HtmlSafeBlockTarget | null {
  const blocked = element.closest([...SOURCE_ONLY_TAGS].join(","));
  const target = blocked ?? element.closest(HTML_INLINE_EDITABLE_SELECTOR) ?? element.closest("*");
  if (!target) return null;
  const tagName = target.tagName.toLowerCase();
  // Capture the inline anchor/image the click landed on, from the ORIGINAL clicked element (not the
  // resolved block): the block editor's Link/Replace-image controls need it, and the parent
  // re-resolves it against source. Interactive mode gets these from the bridge message; Safe mode
  // must supply them here or rich link/image editing is unreachable in Safe mode.
  const anchor = element.closest("a[href]");
  const image = element.closest("img[src]");
  const href =
    anchor?.getAttribute("data-viewright-href") || anchor?.getAttribute("href") || undefined;
  const src =
    image?.getAttribute("data-vr-authored-src") ?? image?.getAttribute("src") ?? undefined;
  const geometry = target.getBoundingClientRect();
  const selection = target.ownerDocument.getSelection();
  let textSelection: HtmlSafeBlockTarget["textSelection"] = capturedSelection
    ? { from: capturedSelection.from, to: capturedSelection.to }
    : undefined;
  let selectedText: string | undefined = capturedSelection?.text;
  if (!capturedSelection && selection && !selection.isCollapsed && selection.rangeCount > 0) {
    const range = selection.getRangeAt(0);
    if (target.contains(range.startContainer) && target.contains(range.endContainer)) {
      const prefix = target.ownerDocument.createRange();
      prefix.selectNodeContents(target);
      prefix.setEnd(range.startContainer, range.startOffset);
      const from = prefix.toString().length;
      prefix.setEnd(range.endContainer, range.endOffset);
      const to = prefix.toString().length;
      if (from < to) {
        textSelection = { from, to };
        selectedText = range.toString();
      }
    }
  }
  // Geometry must be finite; zero-area or off-screen runtime hits still produce a target
  // so the parent can show "Edit in Source" instead of silently ignoring.
  if (
    !Number.isFinite(geometry.left) ||
    !Number.isFinite(geometry.top) ||
    !Number.isFinite(geometry.width) ||
    !Number.isFinite(geometry.height)
  )
    return {
      pagePath,
      childPath: stampedOrComputedPath(target),
      fingerprint: htmlBlockFingerprint(target),
      geometry: { left: 0, top: 0, width: 0, height: 0 },
      text: (target.textContent ?? "").replace(/\s+/g, " ").trim().slice(0, 180),
      tagName,
      id: target.id || undefined,
      classNames: Array.from(target.classList),
      href,
      src,
      selectedText,
      textSelection,
      style: inlineStyleFromElement(target),
    };
  return {
    pagePath,
    childPath: stampedOrComputedPath(target),
    fingerprint: htmlBlockFingerprint(target),
    geometry: {
      left: geometry.left,
      top: geometry.top,
      width: geometry.width,
      height: geometry.height,
    },
    text: (target.textContent ?? "").replace(/\s+/g, " ").trim().slice(0, 180),
    tagName,
    id: target.id || undefined,
    classNames: Array.from(target.classList),
    href,
    src,
    selectedText,
    textSelection,
    style: inlineStyleFromElement(target),
  };
}

/** Resolve one native double-click word before WebKit replaces Interactive with Safe DOM. */
export function htmlTextOffsetFromPoint(
  element: Element,
  clientX: number,
  clientY: number,
): number | undefined {
  const doc = element.ownerDocument as Document & {
    caretRangeFromPoint?: (x: number, y: number) => Range | null;
    caretPositionFromPoint?: (x: number, y: number) => { offsetNode: Node; offset: number } | null;
  };
  let node: Node | null = null;
  let offset = 0;
  const range = doc.caretRangeFromPoint?.(clientX, clientY);
  if (range) {
    node = range.startContainer;
    offset = range.startOffset;
  } else {
    const position = doc.caretPositionFromPoint?.(clientX, clientY);
    node = position?.offsetNode ?? null;
    offset = position?.offset ?? 0;
  }

  if (!node || node.nodeType !== 3 || !element.contains(node)) {
    // WKWebView can paint native selection while exposing neither caret API. Map the hit against
    // character client rects; this runs only for explicit edit-selection gestures.
    const filter = doc.defaultView?.NodeFilter ?? NodeFilter;
    const walker = doc.createTreeWalker(element, filter.SHOW_TEXT);
    let best: { node: Node; offset: number; distance: number } | null = null;
    let visited = 0;
    for (
      let textNode = walker.nextNode();
      textNode && visited < 4096;
      textNode = walker.nextNode()
    ) {
      const value = textNode.nodeValue ?? "";
      for (let index = 0; index < value.length && visited < 4096; index++, visited++) {
        const character = doc.createRange();
        character.setStart(textNode, index);
        character.setEnd(textNode, index + 1);
        if (typeof character.getBoundingClientRect !== "function") continue;
        const rect = character.getBoundingClientRect();
        if (!rect.width && !rect.height) continue;
        const dx =
          clientX < rect.left
            ? rect.left - clientX
            : clientX > rect.right
              ? clientX - rect.right
              : 0;
        const dy =
          clientY < rect.top
            ? rect.top - clientY
            : clientY > rect.bottom
              ? clientY - rect.bottom
              : 0;
        const distance = dx * dx + dy * dy;
        if (!best || distance < best.distance)
          best = {
            node: textNode,
            offset: clientX > rect.left + rect.width / 2 ? index + 1 : index,
            distance,
          };
        if (distance === 0) break;
      }
      if (best?.distance === 0) break;
    }
    node = best?.node ?? null;
    offset = best?.offset ?? 0;
  }
  if (!node || node.nodeType !== 3 || !element.contains(node)) return undefined;
  const prefix = doc.createRange();
  prefix.selectNodeContents(element);
  prefix.setEnd(node, offset);
  return prefix.toString().length;
}

export function htmlWordSelectionFromPoint(
  element: Element,
  clientX: number,
  clientY: number,
): HtmlTextSelection | undefined {
  const value = element.textContent ?? "";
  const offset = htmlTextOffsetFromPoint(element, clientX, clientY);
  if (offset === undefined) return undefined;
  let start = Math.max(0, Math.min(offset, value.length));
  let end = start;
  while (start > 0 && !/\s/.test(value[start - 1] ?? "")) start--;
  while (end < value.length && !/\s/.test(value[end] ?? "")) end++;
  const text = value.slice(start, end).trim();
  if (!text) return undefined;
  return { from: start, to: start + text.length, text };
}

/** Resolve a DOM child path through the source.  Any unsupported or non-locatable target is source-only. */
export function resolveSafeBlockTarget(
  source: string,
  target: HtmlSafeBlockTarget,
  onRefuse?: (reason: string) => void,
): HtmlSafeBlockMatch | null {
  const refuse = (reason: string): null => {
    onRefuse?.(`${reason} tag=${target.tagName} path=${target.childPath?.join(".") ?? ""}`);
    return null;
  };
  // Geometry must be finite; runtime/off-screen hits are not editable.
  if (
    !target.geometry ||
    !Number.isFinite(target.geometry.left) ||
    !Number.isFinite(target.geometry.top) ||
    !Number.isFinite(target.geometry.width) ||
    !Number.isFinite(target.geometry.height)
  )
    return refuse("bad-geometry");
  // Cross-page mismatch is ambiguous by definition.
  if (!target.pagePath || !Array.isArray(target.childPath)) return refuse("bad-path");
  let doc: SafeParseNode;
  try {
    doc = parseHtmlTree(source) as SafeParseNode;
  } catch {
    return refuse("parse-failed");
  }
  const findSafeChild = (parent: SafeParseNode, tag: string) =>
    parent.childNodes?.find((child) => child.tagName?.toLowerCase() === tag) ?? null;
  const html = findSafeChild(doc, "html");
  let node = html ? findSafeChild(html, "body") : null;
  if (!node) return refuse("no-body");
  for (const index of target.childPath) {
    // eslint-disable-next-line @typescript-eslint/no-unnecessary-type-assertion
    const elements: SafeParseNode[] = ((node.childNodes ?? []) as SafeParseNode[]).filter(
      (child) => !!child.tagName,
    );
    node = elements[index] ?? null;
    if (!node) return refuse("path-miss");
  }
  const initialTag = String(node.tagName ?? "").toLowerCase();
  if (SOURCE_ONLY_TAGS.has(initialTag) || initialTag.includes("-"))
    return refuse("source-only-or-custom-element");
  const ancestors: SafeParseNode[] = [];
  for (
    let current: SafeParseNode | null = node;
    current?.tagName;
    current = current.parentNode ?? null
  )
    ancestors.push(current);
  const eligible = ancestors.find((candidate) => {
    const tag = String(candidate.tagName).toLowerCase();
    return SAFE_BLOCK_TAGS.has(tag) && !SOURCE_ONLY_TAGS.has(tag) && !tag.includes("-");
  });
  if (!eligible) return refuse("no-eligible-block");
  const tagName = String(eligible.tagName).toLowerCase();
  // div/section/article containers may hold locked descendants: the visual projection renders
  // them as inert raw_html_island placeholders and serialization re-emits the island's raw bytes
  // verbatim, so the container can stay editable AROUND the protected subtree (see
  // htmlVisualSanitize.projectHtmlForVisual + htmlPmSerialize raw_html_island). The raw bytes are
  // never editable through the visual surface, only preserved.
  const loc: Parse5Location | undefined = eligible.sourceCodeLocation;
  const range = locToRange(loc);
  const startTag = locToRange(loc?.startTag);
  const endTag = locToRange(loc?.endTag);
  if (!range || !startTag || !endTag || endTag.start < startTag.end)
    return refuse("no-source-range");
  const expected = `${tagName}|${eligible.attrs?.find((attribute) => attribute.name === "id")?.value ?? ""}|${eligible.attrs?.find((attribute) => attribute.name === "class")?.value ?? ""}`;
  if (!target.fingerprint.startsWith(expected)) return refuse("fingerprint-tag-id-class");
  const innerRange = { start: startTag.end, end: endTag.start };
  const outerChecksum = sourceChecksum(source.slice(range.start, range.end));
  const innerSlice = source.slice(innerRange.start, innerRange.end);
  // Fingerprint text (last segment after 3 pipes) must appear inside the resolved inner range
  // when present; otherwise a script-mutated or runtime-created DOM text would be accepted.
  const fingerprintText = target.fingerprint.split("|").slice(3).join("|").trim();
  if (fingerprintText) {
    // Strip tags for text comparison so inline markup (e.g., <a>) doesn't break the check.
    const plainInner = innerSlice
      .replace(/<[^>]*>/g, " ")
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 500);
    const normalizedFingerprint = fingerprintText.replace(/\s+/g, " ").trim().slice(0, 180);
    // Inline markup and flattened unknown elements change whitespace between text runs, so compare
    // with all whitespace removed.
    const squash = (value: string) => value.replace(/\s+/g, "");
    const needle = squash(normalizedFingerprint).slice(0, 30);
    if (
      needle &&
      !squash(plainInner).includes(needle) &&
      !squash(normalizeText(decodeHtmlEntities(plainInner)))
        .toLowerCase()
        .includes(squash(normalizeText(decodeHtmlEntities(needle))).toLowerCase())
    )
      return refuse("fingerprint-text");
  }
  return {
    tagName,
    range,
    innerRange,
    checksum: outerChecksum,
    attributes: (eligible.attrs ?? []).map((attribute) => ({
      name: String(attribute.name),
      value: String(attribute.value ?? ""),
    })),
  };
}

export interface TaggedRange extends SourceRange {
  text: string;
  normalizedText: string;
  tagName?: string;
  id?: string;
  classNames: string[];
}

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
  "param",
  "source",
  "track",
  "wbr",
]);
const SKIP_TEXT_TAGS = new Set(["script", "style", "noscript", "template"]);

function normalizeText(value: string): string {
  return value.replace(/\s+/g, " ").trim();
}

export function decodeHtmlEntities(value: string): string {
  if (!/[&]/.test(value)) return value;
  if (typeof document !== "undefined") {
    const textarea = document.createElement("textarea");
    textarea.innerHTML = value;
    return textarea.value;
  }
  return value
    .replace(/&nbsp;/gi, " ")
    .replace(/&amp;/gi, "&")
    .replace(/&lt;/gi, "<")
    .replace(/&gt;/gi, ">")
    .replace(/&quot;/gi, '"')
    .replace(/&#39;/gi, "'");
}

// ── parse5-based core ────────────────────────────────────────────────────────

export interface Parse5Location {
  startOffset: number;
  endOffset: number;
  startLine: number;
  startCol: number;
  endLine: number;
  endCol: number;
  startTag?: Parse5Location;
  endTag?: Parse5Location;
  attrs?: Record<string, Parse5Location>;
}

/** Narrow structural view shared by parse5-backed HTML mapping modules. */
export interface HtmlParseNode {
  nodeName: string;
  tagName?: string;
  value?: string;
  childNodes?: HtmlParseNode[];
  attrs?: Array<{ name: string; value: string }>;
  sourceCodeLocation?: Parse5Location | null;
}

function locToRange(loc: Parse5Location | undefined | null): SourceRange | null {
  if (!loc || typeof loc.startOffset !== "number" || typeof loc.endOffset !== "number") return null;
  return { start: loc.startOffset, end: loc.endOffset };
}

/**
 * Parse HTML with parse5 and return the document tree with locations.
 * This is the direct parse5 foundation for all source maps — never regex.
 */
export function parseHtmlTree(source: string): HtmlParseNode {
  const cached = getCachedTree(source);
  if (cached) return cached as HtmlParseNode;
  incParseCount(1);
  const tree: HtmlParseNode = parse5.parse(source, { sourceCodeLocationInfo: true });
  setCachedTree(source, tree);
  return tree;
}

/**
 * Explicit coordinate transform for a parsed fragment (HTML-EDITING-ARCHITECTURE-FINAL 12.3).
 * `null` means the offset has no canonical-source equivalent and must never be persisted.
 */
export interface FragmentSourceMap {
  fragmentToCanonical(offset: number): number | null;
  canonicalToFragment(offset: number): number | null;
}

/** Unsynthesised canonical slice: a constant base offset. */
export function sliceFragmentSourceMap(base: number, length: number): FragmentSourceMap {
  return {
    fragmentToCanonical: (o) => (o >= 0 && o <= length ? base + o : null),
    canonicalToFragment: (o) => (o >= base && o <= base + length ? o - base : null),
  };
}

/** Synthesised wrapper (or sanitised/rewritten text): offsets are not canonical, so none map. */
export const SYNTHESISED_FRAGMENT_MAP: FragmentSourceMap = {
  fragmentToCanonical: () => null,
  canonicalToFragment: () => null,
};

export function parseHtmlFragmentForBody(bodyInnerHtml: string): HtmlParseNode {
  return parse5.parseFragment(bodyInnerHtml, { sourceCodeLocationInfo: true });
}

// ── TaggedRange inspect map (parse5-backed, replaces regex) ─────────────────

export function parseAndMapSource(source: string): TaggedRange[] {
  const checksum = sourceChecksum(source);
  if (
    taggedRangeCache &&
    taggedRangeCache.checksum === checksum &&
    taggedRangeCache.source === source
  )
    return taggedRangeCache.ranges;
  // Use parse5's precise offsets instead of regex tokenization.
  // Falls back to empty on parse failure (malformed -> source-only fallback upstream).
  let doc: HtmlParseNode;
  try {
    doc = parseHtmlTree(source);
  } catch {
    return [];
  }
  const ranges: TaggedRange[] = [];
  // Walk parse5 tree, maintaining element ancestor stack for tagName/id/class context.
  const walk = (
    node: HtmlParseNode,
    stack: Array<{ tagName: string; id?: string; classNames: string[] }>,
  ) => {
    if (!node) return;
    const name: string = node.nodeName;
    if (name === "#text") {
      const loc = node.sourceCodeLocation;
      if (!loc) return;
      const start = loc.startOffset;
      const end = loc.endOffset;
      if (end <= start) return;
      // Skip text inside script/style/noscript/template — same contract as before.
      if (stack.some((ctx) => SKIP_TEXT_TAGS.has(ctx.tagName))) return;
      const raw = source.slice(start, end);
      const decoded = decodeHtmlEntities(raw);
      const normalized = normalizeText(decoded);
      if (!normalized) return;
      const ctx = stack[stack.length - 1];
      ranges.push({
        start,
        end,
        text: decoded,
        normalizedText: normalized,
        tagName: ctx?.tagName,
        id: ctx?.id,
        classNames: ctx?.classNames ?? [],
      });
      return;
    }
    if (name === "#comment" || name === "#documentType") return;
    // Element or document: recurse into children
    const children = node.childNodes ?? [];
    // Push context for this element before visiting children.
    let pushed = false;
    const tag: string | null = node.tagName ? String(node.tagName).toLowerCase() : null;
    if (tag) {
      const attrs: Array<{ name: string; value: string }> = node.attrs ?? [];
      const id = attrs.find((a) => a.name === "id")?.value;
      const classAttr = attrs.find((a) => a.name === "class")?.value ?? "";
      const classNames = classAttr.split(/\s+/).filter(Boolean);
      // Void elements have no closing tag and should not contribute to stack for descendants
      // (they also have no childNodes, but keep the invariant).
      if (!VOID_TAGS.has(tag)) {
        stack.push({ tagName: tag, id, classNames });
        pushed = true;
      } else if (attrs.length) {
        // For void elements we still track the element itself for completeness, but we don't
        // keep it on stack because it cannot contain text children. No push needed.
      }
    }
    for (const child of children) walk(child, stack);
    if (pushed) stack.pop();
  };
  // Document node may contain "#documentType" + html element.
  walk(doc, []);
  taggedRangeCache = { checksum, source, ranges };
  return ranges;
}

function overlapCount(a: string[], b: string[] = []): number {
  const set = new Set(a);
  return b.filter((value) => set.has(value)).length;
}

export function findSourceRange(
  ranges: TaggedRange[],
  target: HtmlInspectTarget,
): SourceRange | null {
  const needle = normalizeText(decodeHtmlEntities(target.text || "")).toLowerCase();
  const targetClasses = target.classNames ?? [];
  const candidates = ranges
    .map((range) => {
      let score = 0;
      if (needle) {
        const rangeNorm = normalizeText(decodeHtmlEntities(range.normalizedText)).toLowerCase();
        if (rangeNorm.includes(needle)) {
          score += 10;
          if (rangeNorm === needle) score += 30;
        }
      }
      if (target.id && range.id === target.id) score += 50;
      if (target.tagName && range.tagName === target.tagName.toLowerCase()) score += 8;
      score += overlapCount(range.classNames, targetClasses) * 6;
      return score > 0 ? { range, score } : null;
    })
    .filter((value): value is { range: TaggedRange; score: number } => !!value)
    .sort((a, b) => b.score - a.score || a.range.start - b.range.start);

  const best = candidates[0]?.range;
  if (!best) return null;
  const trimmed = (target.text || "").trim();
  if (trimmed) {
    const raw = best.text;
    const exactIndex = raw.indexOf(trimmed);
    if (exactIndex >= 0)
      return { start: best.start + exactIndex, end: best.start + exactIndex + trimmed.length };
    const lowerIndex = raw.toLowerCase().indexOf(trimmed.toLowerCase());
    if (lowerIndex >= 0)
      return { start: best.start + lowerIndex, end: best.start + lowerIndex + trimmed.length };
  }
  return { start: best.start, end: best.end };
}

// ── Shell / body separation (locked shell vs editable body) ─────────────────

/**
 * Precise locations for the document shell vs editable body.
 * Shell = doctype + html/head and their tags + body open/close tags.
 * Body inner = bytes between <body> and </body> (or the synthetic fallback).
 * All offsets are parse5 sourceCodeLocation offsets — never synthesized.
 */
export interface HtmlShellInfo {
  doctype: SourceRange | null;
  htmlOpen: SourceRange | null;
  htmlClose: SourceRange | null;
  head: SourceRange | null;
  headOpen: SourceRange | null;
  headClose: SourceRange | null;
  body: SourceRange | null;
  bodyOpen: SourceRange | null;
  bodyClose: SourceRange | null;
  bodyInner: SourceRange;
}

function findChildByTag(parent: HtmlParseNode, tag: string): HtmlParseNode | null {
  const children = parent.childNodes ?? [];
  return children.find((c) => c.tagName && String(c.tagName).toLowerCase() === tag) ?? null;
}

export function buildHtmlShellInfo(source: string): HtmlShellInfo {
  let doc: HtmlParseNode;
  try {
    doc = parseHtmlTree(source);
  } catch {
    return {
      doctype: null,
      htmlOpen: null,
      htmlClose: null,
      head: null,
      headOpen: null,
      headClose: null,
      body: null,
      bodyOpen: null,
      bodyClose: null,
      bodyInner: { start: 0, end: source.length },
    };
  }
  const locOf = (node: HtmlParseNode | null | undefined): Parse5Location | null =>
    node?.sourceCodeLocation ?? null;
  // Doctype: first child that is #documentType
  const doctypeNode = (doc.childNodes ?? []).find((node) => node.nodeName === "#documentType");
  const doctype = locToRange(locOf(doctypeNode));

  const htmlEl = findChildByTag(doc, "html");
  const htmlLoc: Parse5Location | null = locOf(htmlEl);
  const htmlOpen = htmlLoc?.startTag ? locToRange(htmlLoc.startTag) : null;
  const htmlClose = htmlLoc?.endTag ? locToRange(htmlLoc.endTag) : null;

  const headEl = htmlEl ? findChildByTag(htmlEl, "head") : null;
  const headLoc: Parse5Location | null = locOf(headEl);
  const head = headLoc ? locToRange(headLoc) : null;
  const headOpen = headLoc?.startTag ? locToRange(headLoc.startTag) : null;
  const headClose = headLoc?.endTag ? locToRange(headLoc.endTag) : null;

  const bodyEl = htmlEl ? findChildByTag(htmlEl, "body") : null;
  const bodyLoc: Parse5Location | null = locOf(bodyEl);
  const body = bodyLoc ? locToRange(bodyLoc) : null;
  const bodyOpen = bodyLoc?.startTag ? locToRange(bodyLoc.startTag) : null;
  const bodyClose = bodyLoc?.endTag ? locToRange(bodyLoc.endTag) : null;

  // Body inner: bytes strictly between <body> and </body>
  let bodyInner: SourceRange;
  if (bodyOpen && bodyClose) {
    bodyInner = { start: bodyOpen.end, end: bodyClose.start };
  } else if (bodyOpen && body) {
    // No explicit close (e.g. fragmented source) — inner runs to body end
    bodyInner = { start: bodyOpen.end, end: body.end };
  } else if (body && bodyOpen == null) {
    // Synthesized body (parse5 added one because source had no explicit <body>).
    // Then the whole source is treated as body inner — shell is synthetic and not in source.
    // Detect by bodyLoc being null-ish or bodyOpen null while html exists.
    // Fallback: if head exists, start after headClose, else after htmlOpen/doctype.
    const afterHead = headClose?.end ?? head?.end ?? htmlOpen?.end ?? doctype?.end ?? 0;
    const beforeHtmlClose = htmlClose?.start ?? source.length;
    bodyInner = { start: afterHead, end: beforeHtmlClose };
    // Trim leading/trailing whitespace-only? Keep as-is; caller slices source.
  } else {
    bodyInner = { start: 0, end: source.length };
  }

  // Clamp to source bounds
  bodyInner.start = Math.max(0, Math.min(source.length, bodyInner.start));
  bodyInner.end = Math.max(bodyInner.start, Math.min(source.length, bodyInner.end));

  return {
    doctype,
    htmlOpen,
    htmlClose,
    head,
    headOpen,
    headClose,
    body,
    bodyOpen,
    bodyClose,
    bodyInner,
  };
}
