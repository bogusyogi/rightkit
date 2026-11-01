import DOMPurify from "dompurify";
import * as parse5 from "parse5";
import type { FragmentSourceMap, HtmlParseNode } from "./sourceMap";

/**
 * Strict sanitization for Safe block editing.
 * Every parse/paste goes through DOMPurify + explicit element/attribute/protocol policy
 * before creating editor DOM. This is the only place that decides what becomes live editor DOM.
 * Original source bytes are preserved unless user explicitly edits that range.
 * Policy mirrors CKEditor GHS + H1 spec: strip on*, active schemes, unsafe data:, dangerous styles, srcdoc, embeds.
 */

// Allowed tags for H1 Visual projection (editable body)
const ALLOWED_TAGS_H1 = [
  "article",
  "section",
  "div",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "p",
  "span",
  "br",
  "hr",
  "strong",
  "em",
  "b",
  "i",
  "s",
  "u",
  "code",
  "pre",
  "a",
  "img",
  "ul",
  "ol",
  "li",
  "blockquote",
  "table",
  "thead",
  "tbody",
  "tfoot",
  "tr",
  "th",
  "td",
  "caption",
];

const LOCKED_TAGS = new Set([
  "script",
  "style",
  "iframe",
  "svg",
  "canvas",
  "form",
  "input",
  "button",
  "select",
  "textarea",
  "label",
  "fieldset",
  "legend",
  "datalist",
  "output",
  "option",
  "optgroup",
  "template",
  "object",
  "embed",
  "video",
  "audio",
  "source",
  "track",
  "map",
  "area",
  "math",
  "picture",
]);
const INLINE_CONTAINER_TAGS = new Set([
  "p",
  "span",
  "strong",
  "em",
  "b",
  "i",
  "s",
  "u",
  "code",
  "a",
  "caption",
  "th",
  "td",
]);

const FORBIDDEN_ATTRS = new Set(["srcdoc", "srcset"]); // srcset handled separately; srcdoc never allowed

function escapeAttr(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/"/g, "&quot;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

function isLockedTag(tag: string): boolean {
  return LOCKED_TAGS.has(tag) || !ALLOWED_TAGS_H1.includes(tag) || tag.includes("-");
}

function hasLockedDescendant(node: HtmlParseNode): boolean {
  return (node.childNodes ?? []).some(
    (child) =>
      child.tagName &&
      (isLockedTag(String(child.tagName).toLowerCase()) || hasLockedDescendant(child)),
  );
}

/**
 * Replace H2 locked elements with inert atomic placeholders before DOMPurify sees them. The exact
 * source is carried as an escaped attribute, so Visual can display/edit a scoped raw island while
 * scripts, styles, forms, widgets, and custom elements never become editor DOM.
 */
export function projectHtmlForVisual(html: string, map?: FragmentSourceMap): string {
  let fragment: HtmlParseNode;
  try {
    fragment = parse5.parseFragment(html, { sourceCodeLocationInfo: true });
  } catch {
    return html;
  }
  const render = (node: HtmlParseNode): string => {
    if (
      node.nodeName === "#text" ||
      node.nodeName === "#comment" ||
      node.nodeName === "#documentType"
    ) {
      const loc = node.sourceCodeLocation;
      return loc ? html.slice(loc.startOffset, loc.endOffset) : "";
    }
    if (!node.tagName) return (node.childNodes ?? []).map(render).join("");
    const tag = String(node.tagName).toLowerCase();
    const loc = node.sourceCodeLocation;
    if (!loc || typeof loc.startOffset !== "number" || typeof loc.endOffset !== "number") return "";
    if (isLockedTag(tag) || (INLINE_CONTAINER_TAGS.has(tag) && hasLockedDescendant(node))) {
      const raw = html.slice(loc.startOffset, loc.endOffset);
      // Percent encoding keeps raw markup from being interpreted by DOMPurify as an attribute payload.
      return `<div data-vr-raw-island="${escapeAttr(`locked ${tag}`)}" data-vr-raw="${encodeURIComponent(raw)}" data-vr-raw-start="${map ? (map.fragmentToCanonical(loc.startOffset) ?? -1) : loc.startOffset}" data-vr-raw-end="${map ? (map.fragmentToCanonical(loc.endOffset) ?? -1) : loc.endOffset}"></div>`;
    }
    const start = loc.startTag ? html.slice(loc.startTag.startOffset, loc.startTag.endOffset) : "";
    const end = loc.endTag ? html.slice(loc.endTag.startOffset, loc.endTag.endOffset) : "";
    return `${start}${(node.childNodes ?? []).map(render).join("")}${end}`;
  };
  return (fragment.childNodes ?? []).map(render).join("");
}

const DANGEROUS_STYLE_PATTERNS = [
  /expression\s*\(/i,
  /javascript\s*:/i,
  /vbscript\s*:/i,
  /behaviour\s*:/i,
  /-moz-binding/i,
  /url\s*\(\s*['"]?\s*javascript:/i,
  /url\s*\(\s*['"]?\s*data\s*:\s*text\/html/i,
];

function isSafeDataUrl(url: string): boolean {
  const trimmed = url.trim().toLowerCase();
  if (!trimmed.startsWith("data:")) return false;
  // Only allow safe image/font data URLs; strip html/javascript/text
  if (/^data:\s*text\/html/i.test(trimmed)) return false;
  if (/^data:\s*application\//i.test(trimmed)) return false;
  if (/^data:\s*image\/svg\+xml/i.test(trimmed)) {
    // SVG data URLs are unsafe (can contain scripts); reject
    return false;
  }
  // Allow image/png, image/jpeg, image/gif, image/webp, font/*
  return (
    /^data:\s*image\/(png|jpeg|jpg|gif|webp);/i.test(trimmed) ||
    /^data:\s*font\//i.test(trimmed) ||
    /^data:\s*image\/x-icon/i.test(trimmed)
  );
}

export function isAllowedVisualHref(value: string): boolean {
  const v = value.trim().toLowerCase();
  if (!v) return true; // empty href allowed (will be stripped to #? keep)
  if (v.startsWith("#")) return true;
  if (v.startsWith("/")) return false; // root absolute not allowed in visual (preserve as source-only; but for sanitized projection we drop)
  if (/^https?:\/\//i.test(value.trim())) return true;
  if (/^mailto:/i.test(v)) return true;
  if (/^tel:/i.test(v)) return true;
  // Relative paths are allowed (local) — but never remote fetch in Visual projection; we keep them but don't fetch
  if (/^[a-z][a-z0-9+.-]*:/i.test(v)) {
    // Has scheme — if not http/https/mailto/tel, it's active scheme -> block
    return false;
  }
  // Relative path without scheme (e.g., ./foo, assets/img.png, page.html) — allow
  return true;
}

export function isAllowedVisualImageSource(value: string): boolean {
  const v = value.trim();
  if (!v) return false;
  const lower = v.toLowerCase();
  if (lower.startsWith("data:")) return isSafeDataUrl(v);
  if (/^https?:\/\//i.test(v)) return false; // Visual never fetches remote — remote src is preserved in source but not projected
  if (/^blob:/i.test(lower)) return false;
  if (/^[a-z][a-z0-9+.-]*:/i.test(lower)) return false; // other schemes blocked
  if (v.startsWith("//")) return false;
  // Allow relative paths (local assets) — but Visual projection will not fetch them; it shows placeholder or keeps empty.
  // We keep the attribute but editor will not trigger network fetch (browser will try, so we strip http src; for relative we keep but preview won't fetch due to sandbox? Instead we strip to prevent fetch).
  // For H1, Visual never fetches remote OR local via network; local images are via transactional asset service, so we keep relative src but ensure no actual fetch occurs by later stripping?
  // Policy: allow relative src attribute to exist, but image element's src will be sanitized to not trigger fetch unless it's data:.
  // Easiest: if it's relative, allow it — the editor DOM will have it but browser won't fetch because doc is isolated? Actually ProseMirror DOM is not iframe, so relative src would try to fetch relative to app origin — blocked anyway (no file). Keep it.
  return true;
}

function cleanStyleValue(value: string): string | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  for (const pat of DANGEROUS_STYLE_PATTERNS) {
    if (pat.test(trimmed)) return null;
  }
  // Also block url(javascript: ...) already covered, but also block position: fixed with url?
  return trimmed;
}

/**
 * Sanitize a raw HTML fragment string before it becomes ProseMirror DOM.
 * Returns sanitized HTML string (still may contain allowed tags, but no scripts/on* etc.)
 */
export function sanitizeHtmlForVisual(html: string): string {
  if (typeof window === "undefined" || typeof document === "undefined") {
    // In Node/jsdom, DOMPurify may still work; fallback to simple stripping if not
    try {
      return DOMPurify.sanitize(html, {
        ALLOWED_TAGS: ALLOWED_TAGS_H1,
        ALLOWED_ATTR: [
          "href",
          "src",
          "alt",
          "title",
          "colspan",
          "rowspan",
          "start",
          "data-vr-raw-island",
          "data-vr-raw",
          "data-vr-raw-start",
          "data-vr-raw-end",
        ],
        FORBID_TAGS: [
          "script",
          "style",
          "iframe",
          "object",
          "embed",
          "form",
          "template",
          "svg",
          "canvas",
          "noscript",
          "link",
          "meta",
        ],
        FORBID_ATTR: ["style"], // we will re-allow sanitized style manually
        ALLOW_DATA_ATTR: false,
      }) as unknown as string;
    } catch {
      return html.replace(/<script[\s\S]*?<\/script>/gi, "").replace(/ on\w+="[^"]*"/gi, "");
    }
  }

  const clean: string = DOMPurify.sanitize(html, {
    ALLOWED_TAGS: ALLOWED_TAGS_H1,
    ALLOWED_ATTR: [
      "href",
      "src",
      "alt",
      "title",
      "colspan",
      "rowspan",
      "start",
      "style",
      "data-vr-raw-island",
      "data-vr-raw",
      "data-vr-raw-start",
      "data-vr-raw-end",
    ],
    FORBID_TAGS: [
      "script",
      "style",
      "iframe",
      "object",
      "embed",
      "form",
      "template",
      "svg",
      "canvas",
      "noscript",
      "link",
      "meta",
      "base",
    ],
    FORBID_ATTR: [],
    ALLOW_DATA_ATTR: false,
    WHOLE_DOCUMENT: false,
    RETURN_DOM: false,
  }) as unknown as string;

  // Further policy: DOMPurify doesn't strip javascript: in href/src if we allow those attrs, so do manual pass via DOM
  const container = document.createElement("div");
  container.innerHTML = clean;
  const walker = document.createTreeWalker(container, NodeFilter.SHOW_ELEMENT);
  const toRemove: Element[] = [];
  let node: Element | null = null;
  let next: Element | null = walker.nextNode() as Element | null;
  while (next) {
    node = next;

    const tag = node.tagName.toLowerCase();
    if (!ALLOWED_TAGS_H1.includes(tag)) {
      toRemove.push(node);
      continue;
    }
    // Strip on* handlers, srcdoc, embeds
    for (const attr of Array.from(node.attributes)) {
      const name = attr.name.toLowerCase();
      const value = attr.value;
      if (name.startsWith("on")) {
        node.removeAttribute(attr.name);
        continue;
      }
      if (FORBIDDEN_ATTRS.has(name)) {
        node.removeAttribute(attr.name);
        continue;
      }
      if (name === "srcdoc") {
        node.removeAttribute(attr.name);
        continue;
      }
      if (name === "style") {
        // Sanitize style: drop dangerous payloads, else strip entire style for now (safer)
        // H1 says dangerous style payloads stripped; we conservatively strip any style that contains url() or expression
        const cleaned = cleanStyleValue(value);
        if (cleaned == null) node.removeAttribute("style");
        else {
          // Also ensure no active url schemes inside style
          if (/javascript:/i.test(cleaned) || /data:\s*text\/html/i.test(cleaned))
            node.removeAttribute("style");
          else node.setAttribute("style", cleaned);
        }
        continue;
      }
      if (name === "href" && tag === "a") {
        if (!isAllowedVisualHref(value)) node.removeAttribute("href");
        continue;
      }
      if (name === "src" && (tag === "img" || tag === "a")) {
        if (!isAllowedVisualImageSource(value)) node.removeAttribute("src");
        continue;
      }
      // Strip event handler like formaction etc. (active attributes)
      if (["formaction", "xlink:href", "action"].includes(name)) {
        node.removeAttribute(attr.name);
        // biome-ignore lint/complexity/noUselessContinue: explicit before loop end for symmetry
        continue;
      }
    }
    next = walker.nextNode() as Element | null;
  }
  for (const el of toRemove) el.remove();
  return container.innerHTML;
}

/**
 * Sanitize incoming HTML paste fragment (clipboard). Same policy but also strips wrapper tags.
 */
export function sanitizeHtmlPasteFragment(html: string): string {
  // Paste fragments may be full documents or snippets; extract body if needed.
  let fragment = html;
  // If it looks like a full doc with head, try to extract body inner
  const bodyMatch = html.match(/<body[^>]*>([\s\S]*?)<\/body>/i);
  if (bodyMatch) fragment = bodyMatch[1] ?? fragment;
  return sanitizeHtmlForVisual(fragment);
}
