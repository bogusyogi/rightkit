import DOMPurify from "dompurify";
import { URI_ATTRIBUTES, applyUriPolicy, type SanitizeOptions } from "./security.js";

/** Host policy for a whole HTML document (#395). Every addition defaults to off. Scripts, styles,
 * `base`, `meta`, `link`, `iframe`, `object`, `embed` and `template` are never admitted: the host
 * injects its own base and meta, and CSS is not admitted through the document. */
export interface FullDocumentPolicy extends SanitizeOptions {
  /** Admit `video`, `audio`, `source`, `picture` and `track`. */
  allowMedia?: boolean;
  /** Admit inline `svg` with DOMPurify's svg and svg-filter profiles. */
  allowSvg?: boolean;
  /** Admit form elements. A form `action` is a URL and is removed by the URL policy. */
  allowForms?: boolean;
  /** Runs on the parsed, unsanitized document before anything is removed, so a host can stamp
   * source paths on the tree the user wrote and map edits back to source bytes. */
  beforeSanitize?: (doc: Document) => void;
}

const BASE_TAGS = [
  "a", "abbr", "address", "article", "aside", "b", "bdi", "bdo", "blockquote", "br", "caption", "cite", "code", "col", "colgroup",
  "dd", "del", "details", "dfn", "div", "dl", "dt", "em", "figcaption", "figure", "footer", "h1", "h2", "h3", "h4", "h5", "h6",
  "header", "hgroup", "hr", "i", "img", "ins", "kbd", "li", "main", "mark", "nav", "ol", "p", "pre", "q", "rp", "rt", "ruby", "s",
  "samp", "section", "small", "span", "strong", "sub", "summary", "sup", "table", "tbody", "td", "tfoot", "th", "thead", "time",
  "tr", "u", "ul", "var", "wbr", "html", "head", "body", "title",
];
const BASE_ATTRS = [
  "alt", "class", "colspan", "dir", "height", "href", "id", "lang", "open", "rel", "rowspan", "scope", "src", "srcset", "target",
  "title", "width", "datetime", "cite", "start", "reversed",
];
const MEDIA_TAGS = ["video", "audio", "source", "picture", "track"];
const MEDIA_ATTRS = ["controls", "loop", "muted", "preload", "type", "poster", "kind", "srclang", "label", "default", "media", "sizes"];
const FORM_TAGS = ["form", "input", "button", "select", "option", "optgroup", "textarea", "label", "fieldset", "legend", "output"];
const FORM_ATTRS = ["action", "method", "name", "value", "placeholder", "disabled", "checked", "selected", "for", "readonly", "required", "multiple", "min", "max", "step", "autocomplete"];

/** CSS properties a `style` attribute may keep. Values with a URL, a string, an escape, a
 * function other than a plain number or colour, or a semicolon are dropped. */
const STYLE_PROPERTIES = new Set([
  "color", "background-color", "border-bottom-width", "display", "font-size", "font-style", "font-weight", "height", "left",
  "margin-left", "margin-right", "max-width", "min-width", "padding-left", "padding-right", "position", "text-align", "top",
  "vertical-align", "width", "fill", "stroke", "stroke-width", "opacity", "transform", "font-family",
]);
const STYLE_VALUE = /^[-+.#%(),\s\w]*$/;

/** Keep only the allowed declarations of a `style` attribute value. */
export function filterStyle(value: string): string | undefined {
  const kept = value
    .split(";")
    .map((declaration) => declaration.trim())
    .filter((declaration) => {
      const colon = declaration.indexOf(":");
      if (colon < 0) return false;
      const property = declaration.slice(0, colon).trim().toLowerCase();
      const body = declaration.slice(colon + 1).trim();
      return STYLE_PROPERTIES.has(property) && body.length > 0 && !/url\(|expression|\\/i.test(body) && STYLE_VALUE.test(body);
    });
  return kept.length > 0 ? kept.join("; ") : undefined;
}

/** Sanitize a fragment with `config`, then apply the URL policy and the style filter to every
 * element. Without a DOM the result is empty: there is no trustworthy parser, so nothing is shown. */
function sanitizeFragment(html: string, config: Record<string, unknown>, options: SanitizeOptions): string {
  if (typeof document === "undefined" || !DOMPurify.isSupported) return "";
  const fragment = DOMPurify.sanitize(html, { ...config, RETURN_DOM_FRAGMENT: true } as never) as unknown as DocumentFragment;
  applyAttributePolicy(fragment, options);
  const holder = fragment.ownerDocument.createElement("div");
  holder.appendChild(fragment);
  return holder.innerHTML;
}

function applyAttributePolicy(root: ParentNode, options: SanitizeOptions): void {
  for (const element of Array.from(root.querySelectorAll("*"))) {
    for (const attribute of Array.from(element.attributes)) {
      const name = attribute.name.toLowerCase();
      if (name === "style") {
        const style = filterStyle(attribute.value);
        if (style === undefined) element.removeAttribute(attribute.name);
        else element.setAttribute(attribute.name, style);
        continue;
      }
      if (!URI_ATTRIBUTES.has(name)) continue;
      const safe = applyUriPolicy(name, attribute.value, options);
      if (safe === undefined) element.removeAttribute(attribute.name);
      else if (safe !== attribute.value) element.setAttribute(attribute.name, safe);
    }
  }
}

/** Sanitize a whole HTML document under `policy` (#395). The result is a full document string
 * with `<!DOCTYPE html>`; the host adds its own base, meta and styles around the body. */
export function sanitizeFullHtmlDocument(html: string, policy: FullDocumentPolicy = {}): string {
  if (typeof document === "undefined" || !DOMPurify.isSupported) return "";
  const parsed = new DOMParser().parseFromString(html, "text/html");
  policy.beforeSanitize?.(parsed);
  const tags = [...BASE_TAGS, ...(policy.allowMedia ? MEDIA_TAGS : []), ...(policy.allowForms ? FORM_TAGS : [])];
  const attrs = [...BASE_ATTRS, ...(policy.allowMedia ? MEDIA_ATTRS : []), ...(policy.allowForms ? FORM_ATTRS : []), "style"];
  const clean = DOMPurify.sanitize(`<!DOCTYPE html>${parsed.documentElement.outerHTML}`, {
    WHOLE_DOCUMENT: true,
    ALLOWED_TAGS: tags,
    ALLOWED_ATTR: attrs,
    USE_PROFILES: policy.allowSvg ? { svg: true, svgFilters: true } : undefined,
    FORBID_TAGS: ["script", "style", "iframe", "object", "embed", "base", "meta", "link", "template"],
    FORBID_ATTR: ["srcdoc"],
    ALLOW_UNKNOWN_PROTOCOLS: false,
    ALLOW_DATA_ATTR: false,
  } as never) as unknown as string;
  const reparsed = new DOMParser().parseFromString(clean, "text/html");
  applyAttributePolicy(reparsed, policy);
  return `<!DOCTYPE html>${reparsed.documentElement.outerHTML}`;
}

const KATEX_TAGS = [
  "annotation", "math", "menclose", "mfrac", "mi", "mn", "mo", "mover", "mpadded", "mphantom", "mroot", "mrow", "ms", "mspace",
  "msqrt", "mstyle", "msub", "msubsup", "msup", "mtable", "mtd", "mtext", "mtr", "munder", "munderover", "semantics", "span",
  "div", "svg", "path", "line", "rect", "g", "use", "defs", "clipPath",
];
const KATEX_ATTRS = [
  "aria-hidden", "class", "display", "encoding", "columnalign", "columnspacing", "rowspacing", "fence", "form", "lspace", "rspace",
  "stretchy", "symmetric", "accent", "accentunder", "linethickness", "mathvariant", "scriptlevel", "width", "height", "viewBox",
  "preserveAspectRatio", "d", "fill", "stroke", "stroke-width", "x", "x1", "x2", "y", "y1", "y2", "transform", "xlink:href", "id",
  "style", "notation", "depth", "voffset", "minsize", "maxsize",
];

/** Sanitize KaTeX output (MathML and styled spans with inline SVG for stretchy symbols). `math`
 * and `svg` are admitted here and nowhere else; `href` and `xlink:href` are dropped by the URL
 * policy and `style` keeps only the allowed declarations. */
export function sanitizeKatexHtml(html: string, options: SanitizeOptions = {}): string {
  return sanitizeFragment(
    html,
    {
      ALLOWED_TAGS: KATEX_TAGS,
      ALLOWED_ATTR: KATEX_ATTRS,
      USE_PROFILES: { svg: true, svgFilters: true, mathMl: true },
      FORBID_TAGS: ["script", "style", "iframe", "object", "embed", "foreignObject", "form", "input", "base", "meta", "link"],
      ALLOW_UNKNOWN_PROTOCOLS: false,
      ALLOW_DATA_ATTR: false,
    },
    options,
  );
}

const MERMAID_ATTRS = [
  "class", "d", "dx", "dy", "fill", "fill-opacity", "font-family", "font-size", "font-weight", "height", "id", "marker-end",
  "marker-start", "markerHeight", "markerWidth", "markerUnits", "orient", "points", "preserveAspectRatio", "r", "refX", "refY",
  "rx", "ry", "stroke", "stroke-dasharray", "stroke-linecap", "stroke-linejoin", "stroke-opacity", "stroke-width", "style",
  "text-anchor", "transform", "viewBox", "width", "x", "x1", "x2", "y", "y1", "y2", "xmlns", "role", "aria-roledescription",
  "aria-labelledby", "data-id", "data-node", "data-points", "tabindex",
];

/** Sanitize a Mermaid SVG diagram. Only SVG drawing elements are admitted: `foreignObject`,
 * `script`, `style`, `image`, links and `use` are removed, so the diagram carries no HTML label
 * markup and no external reference. `style` keeps only the allowed declarations. */
export function sanitizeMermaidSvg(svg: string, options: SanitizeOptions = {}): string {
  return sanitizeFragment(
    svg,
    {
      USE_PROFILES: { svg: true, svgFilters: true },
      ALLOWED_TAGS: ["svg", "g", "path", "rect", "circle", "ellipse", "line", "polyline", "polygon", "text", "tspan", "defs", "marker", "title", "desc"],
      ALLOWED_ATTR: MERMAID_ATTRS,
      FORBID_TAGS: ["foreignObject", "script", "style", "image", "use", "a", "iframe", "object", "embed"],
      ALLOW_UNKNOWN_PROTOCOLS: false,
      ALLOW_DATA_ATTR: false,
    },
    options,
  );
}
