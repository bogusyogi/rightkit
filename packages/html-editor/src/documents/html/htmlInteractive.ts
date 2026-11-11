import type { SiteRootGrant } from "./htmlInline.js";
import { readRelativeScript } from "@rightkit/document-core/state/tauriBridge";
import { buildPreviewHtml, isLocalRelative, joinRel, siteRootRelativeReadPath } from "./htmlInline.js";
import { bridgeScript } from "./htmlBridgeScript.js";
import { htmlThemeStyleHtml } from "./htmlTheme.js";
import { HTML_INLINE_EDITABLE_SELECTOR, htmlElementPathFromBody } from "./sourceMap.js";
import { hostAppName } from "../../appName.js";
import { hostScheme } from "@rightkit/document-core/host";

function isClassicScript(type: string | null): boolean {
  const t = (type ?? "").trim().toLowerCase();
  return (
    t === "" ||
    t === "text/javascript" ||
    t === "application/javascript" ||
    t === "application/ecmascript" ||
    t === "text/ecmascript"
  );
}

/** http(s) or protocol-relative URL — a remote script the browser can load like a normal tab. The
 *  opaque-origin sandbox (allow-scripts, never allow-same-origin) keeps even remote code unable to
 *  reach the app or the OS, so Interactive mode may load it the way a browser does. */
function isExternalHttp(url: string | null | undefined): boolean {
  if (!url) return false;
  const u = url.trim();
  return /^https?:\/\//i.test(u) || u.startsWith("//");
}

function blockedScriptPlaceholder(doc: Document, reason: string): HTMLScriptElement {
  const script = doc.createElement("script");
  script.setAttribute("data-vr-blocked-script", reason);
  script.textContent = "";
  return script;
}

function isBrowserJavaScriptSource(src: string): boolean {
  return /\.(?:js|mjs|cjs)(?:[?#].*)?$/i.test(src);
}

function isBuildTimeSource(src: string): boolean {
  return /\.(?:ts|tsx|jsx|vue|svelte|astro)(?:[?#].*)?$/i.test(src);
}

function addPreviewIssueNotice(doc: Document, sources: string[]): void {
  if (sources.length === 0) return;
  doc.getElementById("vr-boot")?.remove();
  const notice = doc.createElement("aside");
  notice.setAttribute("data-vr-preview-issue", "development-server-required");
  notice.setAttribute("role", "status");
  notice.setAttribute(
    "style",
    "position:fixed;z-index:2147483647;top:50%;left:50%;transform:translate(-50%,-50%);max-width:min(620px,calc(100vw - 32px));box-sizing:border-box;padding:16px 18px;border:1px solid #b77a40;border-radius:9px;background:#fff8ea;color:#33251a;font:13px/1.45 system-ui,-apple-system,sans-serif;box-shadow:0 8px 28px #0003",
  );
  const title = doc.createElement("strong");
  title.textContent = "Development server required";
  const body = doc.createElement("div");
  body.textContent = `This source page depends on build-time modules (${sources.join(", ")}). Start its dev server, then open its localhost URL in ${hostAppName()}.`;
  body.setAttribute("style", "margin-top:3px");
  notice.append(title, body);
  doc.body.prepend(notice);
}

function ensureHead(doc: Document): HTMLHeadElement {
  if (doc.head) return doc.head;
  const head = doc.createElement("head");
  doc.documentElement.insertBefore(head, doc.body ?? null);
  return head;
}

function ensureResponsiveViewport(doc: Document): void {
  const head = ensureHead(doc);
  if (head.querySelector('meta[name="viewport" i]')) return;
  const viewport = doc.createElement("meta");
  viewport.setAttribute("name", "viewport");
  viewport.setAttribute("content", "width=device-width, initial-scale=1");
  head.insertBefore(viewport, head.firstChild);
}

/**
 * Build a trusted-but-confined interactive local HTML document. Unlike HtmlViewer's safe mode this
 * preserves inline scripts/event handlers, inlines same-folder classic script files, and lets remote
 * http(s) scripts/styles/fonts load like a browser tab — so CDN-driven pages (GSAP, three.js, etc.)
 * render fully. Confinement is the opaque-origin iframe (`sandbox="allow-scripts"`, never
 * `allow-same-origin`): even remote code cannot reach the app (no IPC/parent) or the OS (no file
 * access). The CSP still pins `default-src 'none'` and blocks `base-uri`/`form-action`.
 */
export async function buildInteractivePreviewHtml(
  rawHtml: string,
  docPath: string,
  sessionId: string,
  rootPath = docPath,
  scrollTarget?: string,
  scrollY?: number,
  siteRoot?: SiteRootGrant | null,
): Promise<string> {
  const inlinedAssets = await buildPreviewHtml(rawHtml, docPath, rootPath, siteRoot);
  if (typeof DOMParser === "undefined") return inlinedAssets;
  const doc = new DOMParser().parseFromString(inlinedAssets, "text/html");
  const developmentServerSources: string[] = [];

  // Preserve source-backed identity before page scripts run. Runtime DOM remains untrusted: these
  // hints only let parent resolve current source through parse5 + checksum before an editor opens.
  for (const element of Array.from(doc.body.querySelectorAll(HTML_INLINE_EDITABLE_SELECTOR))) {
    element.setAttribute("data-vr-edit-path", htmlElementPathFromBody(element).join("."));
  }

  for (const meta of Array.from(doc.querySelectorAll("meta[http-equiv]"))) {
    if ((meta.getAttribute("http-equiv") || "").trim().toLowerCase() === "refresh") meta.remove();
  }

  // Each <script src> is decided/mutated independently (no shared budget, no cross-script ordering
  // dependency — `replaceWith` swaps each node in place, so parallel completion order can't change
  // the resulting tree). Promise.all replaces the old one-at-a-time IPC round-trips per T1-2.
  await Promise.all(
    Array.from(doc.querySelectorAll("script[src]")).map(async (script) => {
      const src = script.getAttribute("src") || "";
      const localReadPath = isLocalRelative(src)
        ? joinRel("", src)
        : siteRoot
          ? siteRootRelativeReadPath(src, docPath, siteRoot)
          : null;
      // Same-folder classic scripts are INLINED (the preview document has no site base unless a root
      // is granted, so a relative src can't resolve — we read it off disk via Rust and inline it; works with no network).
      if (
        localReadPath &&
        isClassicScript(script.getAttribute("type")) &&
        isBrowserJavaScriptSource(src)
      ) {
        const text = await readRelativeScript(docPath, localReadPath, rootPath, siteRoot?.token);
        if (text == null) {
          script.replaceWith(blockedScriptPlaceholder(doc, "unreadable source"));
          return;
        }
        const inline = doc.createElement("script");
        inline.textContent = text;
        for (const attr of Array.from(script.attributes)) {
          if (attr.name.toLowerCase() !== "src") inline.setAttribute(attr.name, attr.value);
        }
        inline.setAttribute("data-vr-inlined-script", src);
        script.replaceWith(inline);
        return;
      }
      // Remote http(s) scripts (CDN libraries like GSAP/three.js) are LEFT IN PLACE — the relaxed CSP
      // lets the browser load them, and the opaque-origin sandbox still blocks any reach into the app.
      if (isExternalHttp(src)) return;
      if (
        script.getAttribute("type")?.trim().toLowerCase() === "module" &&
        localReadPath &&
        siteRoot &&
        isBrowserJavaScriptSource(src)
      ) {
        if (!isLocalRelative(src)) script.setAttribute("src", localReadPath);
        return;
      }
      if ((localReadPath && !isBrowserJavaScriptSource(src)) || isBuildTimeSource(src)) {
        developmentServerSources.push(src);
        script.replaceWith(blockedScriptPlaceholder(doc, "development server required"));
        return;
      }
      // Anything else (file:, exotic schemes, local non-classic/module we can't safely inline) → blocked.
      script.replaceWith(blockedScriptPlaceholder(doc, "blocked source"));
    }),
  );
  addPreviewIssueNotice(doc, developmentServerSources);

  const head = ensureHead(doc);
  ensureResponsiveViewport(doc);
  const site = hostScheme("site");
  const csp = doc.createElement("meta");
  csp.setAttribute("http-equiv", "Content-Security-Policy");
  csp.setAttribute(
    "content",
    // AUTHORITATIVE copy is the response header in src-tauri/src/preview_protocol.rs
    // (INTERACTIVE_CSP); this meta must stay identical and mainly backs the plain-browser srcdoc
    // fallback. the host's `<id>-site:` scheme is granted for SUBRESOURCES only, and `base-uri` is narrowed to it so our
    // injected <base> is allowed while a page-authored one still cannot point anywhere else.
    // `connect-src` deliberately omits it: a page may DISPLAY files from the granted root, but may
    // not read their bytes back via fetch/XHR and ship them somewhere. That is exactly what a
    // browser permits for a `file://` document, and it is what stops a hostile page from
    // exfiltrating the project tree the tab was granted.
    `default-src 'none'; script-src 'unsafe-inline' ${site}: blob: http: https:; style-src 'unsafe-inline' ${site}: http: https:; img-src ${site}: data: blob: http: https:; font-src ${site}: data: http: https:; media-src ${site}: data: blob: http: https:; connect-src http: https:; base-uri ${site}:; form-action 'none'`,
  );
  head.insertBefore(csp, head.firstChild);

  // App-themed scrollbar + find colors. Theme changes hot-apply this style over postMessage, so
  // palette switching does not rebuild srcDoc or rerun page scripts.
  const themeStyle = new DOMParser().parseFromString(htmlThemeStyleHtml(), "text/html").head
    .firstElementChild;
  if (themeStyle) head.insertBefore(doc.importNode(themeStyle, true), csp.nextSibling);

  const bridge = doc.createElement("script");
  bridge.setAttribute("data-vr-link-bridge", "");
  bridge.textContent = bridgeScript(sessionId, docPath);
  doc.body.appendChild(bridge);

  // Cross-page anchor (e.g. `formats/#txt`): the parent can't scroll into an opaque-origin iframe, so
  // bake a scroll-to-fragment into the page itself, run after layout. id first, then [name=].
  if (scrollTarget) {
    const scroll = doc.createElement("script");
    scroll.setAttribute("data-vr-scroll", "");
    const id = JSON.stringify(scrollTarget);
    scroll.textContent = `requestAnimationFrame(()=>{try{var el=document.getElementById(${id})||document.querySelector('[name='+CSS.escape(${id})+']');if(el)el.scrollIntoView();}catch(e){}});`;
    doc.body.appendChild(scroll);
  } else if (scrollY && scrollY > 0) {
    // Restore the scroll position across a rebuild / fresh mount (tab switch). A single rAF lands at the
    // top on content-heavy / async pages whose height grows after first paint, so STABILIZE-then-restore:
    // each frame re-applies scrollTo (clamped to the reachable max) and keeps going until the document
    // height is stable AND the target was reached, or a ~120-frame (~2s) cap. Never overshoots or throws.
    const scroll = doc.createElement("script");
    scroll.setAttribute("data-vr-scroll-y", "");
    scroll.textContent = `(function(){var t=${Math.round(scrollY)},n=0,p=-1;function s(){var de=document.documentElement,h=de.scrollHeight,m=Math.max(0,h-de.clientHeight),y=Math.min(t,m);try{window.scrollTo(0,y);}catch(e){}if(n++<120&&(h!==p||y<t)){p=h;requestAnimationFrame(s);}}requestAnimationFrame(s);})();`;
    doc.body.appendChild(scroll);
  }

  return `<!doctype html>${doc.documentElement.outerHTML}`;
}

/** Concatenate the text of every inline `<style>` block in an HTML string. Used to hot-apply CSS
 *  edits to the running interactive preview without a reload. Linked `.css` files and `url()` assets
 *  are NOT resolved here — those update on Save/reload (full rebuild). "" when DOMParser is absent. */
export function extractInlineStyleCss(html: string): string {
  if (typeof DOMParser === "undefined") return "";
  const doc = new DOMParser().parseFromString(html, "text/html");
  return Array.from(doc.querySelectorAll("style"))
    .map((s) => s.textContent ?? "")
    .join("\n");
}
