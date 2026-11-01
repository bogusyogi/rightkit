// DOM-dependent HTML helpers (need a browser DOMParser; not unit-testable in
// plain node, unlike lib/mail.ts).
import DOMPurify from "dompurify";

// DOMPurify already strips <script> and event handlers. FORBID_TAGS adds
// defense-in-depth for elements that have no place in a rendered email and that
// flow into the compose seed (useThreadView seedHtml): interactive form
// controls, plugin embeds, and <base>/<link> which can hijack URL resolution or
// pull external CSS. <style> is intentionally NOT forbidden — emails rely on it.
const SANITIZE_CONFIG = {
  ADD_ATTR: ["target", "rel"],
  FORBID_TAGS: ["form", "input", "button", "select", "textarea", "object", "embed", "base", "link"],
};

export function sanitizeHtml(html: string): string {
  const clean = DOMPurify.sanitize(html, SANITIZE_CONFIG);
  try {
    const doc = new DOMParser().parseFromString(clean, "text/html");
    doc.querySelectorAll<HTMLAnchorElement>("a[target]").forEach((a) => {
      const target = (a.getAttribute("target") || "").toLowerCase();
      if (!target || target === "_self") return;
      const rel = new Set(
        (a.getAttribute("rel") || "")
          .split(/\s+/)
          .map((x) => x.trim().toLowerCase())
          .filter(Boolean)
      );
      rel.add("noopener");
      rel.add("noreferrer");
      a.setAttribute("rel", [...rel].join(" "));
    });
    return doc.body.innerHTML;
  } catch {
    return clean.replace(/\starget\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)/gi, "");
  }
}

/** Escape + newline→<br> so a plain seed (signature, quoted reply) lands as
 *  valid HTML inside the contentEditable compose body. */
export function plainToHtml(s: string): string {
  const esc = s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  return esc.replace(/\r?\n/g, "<br>");
}

/** Rewrite `src="cid:…"` inline-image refs to their resolved data URL (when the
 *  cid→url map has an entry). */
export function rewriteCids(html: string, map?: Record<string, string>): string {
  if (!map) return html;
  return html.replace(/src\s*=\s*(["']?)cid:([^"'\s>]+)\1/gi, (whole, _q, cid) => {
    const url = map[cid] || map[decodeURIComponent(cid)];
    return url ? `src="${url}"` : whole;
  });
}

/** Rewrite remote http(s) image srcs to their cached data URL (when fetched). */
export function rewriteRemoteImgs(html: string, map?: Record<string, string>): string {
  if (!map || Object.keys(map).length === 0) return html;
  return html.replace(
    /(<img\b[^>]*?\bsrc\s*=\s*)(["']?)(https?:\/\/[^"'\s>]+)\2/gi,
    (whole, pre, _q, url) => {
      const data = map[url];
      return data ? `${pre}"${data}"` : whole;
    }
  );
}

/** Flatten an HTML email body to plain text for previews/quotes, dropping
 *  non-visible nodes whose text would otherwise leak (CSS rules, VML, scripts). */
export function htmlToText(html: string): string {
  try {
    const doc = new DOMParser().parseFromString(html, "text/html");
    doc.querySelectorAll("style, script, head, title, noscript").forEach((el) => el.remove());
    return (doc.body.textContent || "").replace(/\s+/g, " ").trim();
  } catch {
    return html
      .replace(/<style[\s\S]*?<\/style>/gi, " ")
      .replace(/<script[\s\S]*?<\/script>/gi, " ")
      .replace(/<[^>]+>/g, " ")
      .replace(/\s+/g, " ")
      .trim();
  }
}
