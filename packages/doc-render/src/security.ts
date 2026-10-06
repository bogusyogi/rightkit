import DOMPurify from "dompurify";

function hasControlCharacter(value: string): boolean {
  for (const char of value) {
    const code = char.charCodeAt(0);
    if (code <= 31 || code === 127) return true;
  }
  return false;
}

/** Allow only links that can leave the application through an explicit host adapter. */
export function safeLinkHref(href?: string): string | undefined {
  if (!href) return undefined;
  const trimmed = href.trim();
  if (!trimmed || hasControlCharacter(trimmed)) return undefined;
  const protocolMatch = trimmed.match(/^([a-z][a-z0-9+.-]*):/i);
  if (!protocolMatch) return undefined;
  const protocol = protocolMatch[1].toLowerCase();
  if (protocol !== "http" && protocol !== "https" && protocol !== "mailto") return undefined;
  if ((protocol === "http" || protocol === "https") && !/^https?:\/\//i.test(trimmed)) return undefined;
  try {
    const parsed = new URL(trimmed);
    return parsed.protocol === `${protocol}:` ? trimmed : undefined;
  } catch {
    return undefined;
  }
}

/** Relative image references are safe in a host-controlled document root; remote images use the
 * same strict URL policy as links. Data URLs are intentionally unavailable. */
export function safeImageHref(href?: string): string | undefined {
  if (!href) return undefined;
  const trimmed = href.trim();
  if (!trimmed || hasControlCharacter(trimmed) || /^data:|^javascript:|^vbscript:/i.test(trimmed)) return undefined;
  if (/^[a-z][a-z0-9+.-]*:/i.test(trimmed)) return safeLinkHref(trimmed);
  if (trimmed.startsWith("//") || trimmed.startsWith("\\\\")) return undefined;
  return trimmed;
}

const FORBIDDEN_TAGS = ["script", "style", "iframe", "object", "embed", "form", "base", "meta", "link", "template", "svg", "math", "canvas"];
const URI_ATTRIBUTES = new Set(["href", "src", "action", "formaction", "xlink:href"]);

/** Without a DOM there is no trustworthy HTML parser, so fail closed: render the raw markup as
 * inert text. Regex sanitizing is never a security boundary (e.g. `<img src=x/onerror=...>`). */
function escapeHtml(html: string): string {
  return html.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}

/** Sanitize raw HTML before it reaches a React HTML boundary. Browser DOMPurify is used when a
 * DOM exists; with no DOM (SSR/node) the markup is escaped to inert text. */
export function sanitizeHtml(html: string): string {
  if (typeof document === "undefined" || !DOMPurify.isSupported) return escapeHtml(html);
  return DOMPurify.sanitize(html, {
    ALLOWED_TAGS: ["a", "abbr", "b", "blockquote", "br", "code", "del", "div", "em", "h1", "h2", "h3", "h4", "h5", "h6", "hr", "i", "img", "li", "ol", "p", "pre", "section", "small", "span", "strong", "sub", "sup", "table", "tbody", "td", "th", "thead", "tr", "u", "ul"],
    ALLOWED_ATTR: ["alt", "class", "colspan", "height", "href", "id", "rel", "rowspan", "src", "target", "title", "width"],
    FORBID_TAGS: FORBIDDEN_TAGS,
    FORBID_ATTR: ["style", "srcdoc"],
    ALLOW_UNKNOWN_PROTOCOLS: false,
    RETURN_TRUSTED_TYPE: false,
    ADD_ATTR: ["target", "rel"],
    ALLOW_DATA_ATTR: false,
  }).replace(/\s+(href|src)=(['"])([^'"\s]*)\2/gi, (full, attribute: string, quote: string, value: string) => {
    const safe = URI_ATTRIBUTES.has(attribute.toLowerCase()) && (attribute.toLowerCase() === "src" ? safeImageHref(value) : safeLinkHref(value));
    return safe ? ` ${attribute}=${quote}${safe}${quote}` : "";
  });
}

export function sanitizeMarkdownHtml(html: string, options: { safeMode?: boolean } = {}): string {
  return options.safeMode === false ? html : sanitizeHtml(html);
}

export function isSafeUri(uri: string): boolean {
  return Boolean(safeLinkHref(uri) ?? safeImageHref(uri));
}
