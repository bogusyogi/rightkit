import DOMPurify from "dompurify";

function hasControlCharacter(value: string): boolean {
  for (const char of value) {
    const code = char.charCodeAt(0);
    if (code <= 31 || code === 127) return true;
  }
  return false;
}

/** Host-enabled additions to the link policy. Both default to off. */
export interface LinkPolicy {
  /** Also admit `tel:` links made only of digits and `+ ( ) . - ; , # *`. */
  allowTel?: boolean;
  /** Also admit scheme-less relative references (`a/b`, `./a`, `/a`, `#id`, `?q`); never `//host` or `\\host`. */
  allowRelative?: boolean;
}

const TEL = /^tel:\+?[0-9().\-;,#*]+$/i;

/** Allow only links that can leave the application through an explicit host adapter. */
export function safeLinkHref(href?: string, policy: LinkPolicy = {}): string | undefined {
  if (!href) return undefined;
  const trimmed = href.trim();
  if (!trimmed || hasControlCharacter(trimmed)) return undefined;
  const protocolMatch = trimmed.match(/^([a-z][a-z0-9+.-]*):/i);
  if (!protocolMatch) {
    if (!policy.allowRelative) return undefined;
    if (trimmed.startsWith("//") || trimmed.startsWith("\\") || trimmed.includes("\\") || /[\s<>"'`]/.test(trimmed)) return undefined;
    return trimmed;
  }
  if (policy.allowTel && protocolMatch[1].toLowerCase() === "tel") return TEL.test(trimmed) ? trimmed : undefined;
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
/** Every attribute that can carry a URL. Anything but `href`/`src` is never admitted. */
const URI_ATTRIBUTES = new Set(["href", "src", "srcset", "poster", "action", "formaction", "xlink:href", "background", "data", "ping", "cite", "longdesc", "manifest"]);

/** Shared image resolution for the React and HTML-string renderers: image policy, then the host
 * hook for relative sources. Returns `undefined` when the image must be dropped. */
export function resolveImageSource(src: string | undefined, options: SanitizeOptions = {}): string | undefined {
  let safe = safeImageHref(src);
  if (safe && options.resolveImageSrc && !/^[a-z][a-z0-9+.-]*:/i.test(safe)) {
    const resolved = options.resolveImageSrc(safe);
    safe = resolved && !hasControlCharacter(resolved) ? resolved : undefined;
  }
  return safe;
}

/** Apply the URL policy to one decoded attribute value; `undefined` means remove the attribute. */
export function applyUriPolicy(name: string, value: string, options: SanitizeOptions = {}): string | undefined {
  switch (name.toLowerCase()) {
    case "href": return safeLinkHref(value, options);
    case "src": return resolveImageSource(value, options);
    default: return undefined;
  }
}

/** Without a DOM there is no trustworthy HTML parser, so fail closed: render the raw markup as
 * inert text. Regex sanitizing is never a security boundary (e.g. `<img src=x/onerror=...>`). */
function escapeHtml(html: string): string {
  return html.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}

/** Host policy for sanitized HTML. Nothing here widens the tag, attribute or CSS allowlists. */
export interface SanitizeOptions extends LinkPolicy {
  /** Rewrites a relative `<img src>` (e.g. to a local-asset URL). Called only for sources that pass
   * the image policy as relative; return `undefined` to drop the attribute. The returned URL is
   * host-trusted and is not re-validated, so a hook must never echo unvetted input into a scheme. */
  resolveImageSrc?: (src: string) => string | undefined;
}

/** Sanitize raw HTML before it reaches a React HTML boundary. Browser DOMPurify is used when a
 * DOM exists; with no DOM (SSR/node) the markup is escaped to inert text. */
export function sanitizeHtml(html: string, options: SanitizeOptions = {}): string {
  if (typeof document === "undefined" || !DOMPurify.isSupported) return escapeHtml(html);
  const fragment = DOMPurify.sanitize(html, {
    ALLOWED_TAGS: ["a", "abbr", "b", "blockquote", "br", "code", "del", "div", "em", "h1", "h2", "h3", "h4", "h5", "h6", "hr", "i", "img", "li", "ol", "p", "pre", "section", "small", "span", "strong", "sub", "sup", "table", "tbody", "td", "th", "thead", "tr", "u", "ul"],
    ALLOWED_ATTR: ["alt", "class", "colspan", "height", "href", "id", "rel", "rowspan", "src", "target", "title", "width"],
    FORBID_TAGS: FORBIDDEN_TAGS,
    FORBID_ATTR: ["style", "srcdoc"],
    ALLOW_UNKNOWN_PROTOCOLS: false,
    RETURN_TRUSTED_TYPE: false,
    ADD_ATTR: ["target", "rel"],
    ALLOW_DATA_ATTR: false,
    RETURN_DOM_FRAGMENT: true,
  }) as unknown as DocumentFragment;
  const doc = fragment.ownerDocument;
  for (const element of Array.from(fragment.querySelectorAll("*"))) {
    for (const attribute of Array.from(element.attributes)) {
      const name = attribute.name.toLowerCase();
      if (!URI_ATTRIBUTES.has(name)) continue;
      const safe = applyUriPolicy(name, attribute.value, options);
      if (safe === undefined) element.removeAttribute(attribute.name);
      else if (safe !== attribute.value) element.setAttribute(attribute.name, safe);
    }
  }
  const holder = doc.createElement("div");
  holder.appendChild(fragment);
  return holder.innerHTML;
}

export function sanitizeMarkdownHtml(html: string, options: SanitizeOptions & { safeMode?: boolean } = {}): string {
  return options.safeMode === false ? html : sanitizeHtml(html, options);
}

export function isSafeUri(uri: string): boolean {
  return Boolean(safeLinkHref(uri) ?? safeImageHref(uri));
}
