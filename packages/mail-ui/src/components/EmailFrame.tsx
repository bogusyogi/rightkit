// Locked-down HTML email renderer (sandboxed iframe + CSP). Extracted from
// App.tsx (M5) so ReaderView can own the reading pane. The CSP + DOMPurify +
// data:-only image policy are the untrusted-content security boundary.
//
// Find-in-page: because the iframe is same-origin (no allow-scripts), the app
// can read its DOM to power find. Each frame registers an IframeFind provider
// (search/paint/clear via the CSS Custom Highlight API — paint-only, no DOM
// mutation) and intercepts Ctrl/Cmd+F inside the iframe to open the app FindBar
// instead of WebView2's native find.
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { EMPTY_IMG_SRC, isSafeDataImageSrc, isLocalCachedImg } from "../lib/imageTrust";
import { sanitizeHtml } from "../lib/html";
import { getEmailFrameShortcuts } from "../lib/emailFrameShortcuts";
import {
  applyHighlights,
  clearHighlights,
  findLiteralMatches,
  getVisibleTextNodes,
  rangesForMatches,
  type IframeFind,
  type SearchOptions,
  type TextMatch,
} from "../lib/searchUtils";

function cssVar(name: string, fallback: string): string {
  if (typeof window === "undefined") return fallback;
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback;
}

/** Sanitize email HTML (strip scripts, handlers, javascript: URLs) and remove
 * any direct remote img source. Remote images only render after Rust fetches
 * and rewrites them to data: URLs; the iframe CSP is the security boundary.
 * Inline data: images are kept. */
function prepareBody(html: string, loadImages: boolean): string {
  const clean = sanitizeHtml(html);
  const doc = new DOMParser().parseFromString(clean, "text/html");
  const quote = doc.querySelector(".gmail_quote, blockquote");
  if (quote && quote.parentElement && !quote.closest("details")) {
    const details = doc.createElement("details");
    const summary = doc.createElement("summary");
    summary.textContent = "Show quoted text";
    details.appendChild(summary);
    quote.parentElement.insertBefore(details, quote);
    details.appendChild(quote);
  }
  const hideImage = (img: HTMLImageElement, reason: string) => {
    img.setAttribute("src", EMPTY_IMG_SRC);
    img.setAttribute("data-mailright-image-blocked", reason);
  };
  doc.querySelectorAll("img").forEach((img) => {
    const src = (img.getAttribute("src") || "").trim();
    const srcset = img.getAttribute("srcset") || "";
    if (/\bhttps?:|\bdata:image\//i.test(srcset)) img.removeAttribute("srcset");
    if (isLocalCachedImg(src)) {
      // Already fetched + normalized by Rust and served via our protocol
      // (mailright-img:// or, on Windows, http://mailright-img.localhost/).
    } else if (/^https?:/i.test(src)) {
      hideImage(img, loadImages ? "loading" : "blocked");
    } else if (/^data:image\//i.test(src) && !isSafeDataImageSrc(src)) {
      hideImage(img, "unsafe-data");
    }
  });
  doc.querySelectorAll("source").forEach((source) => {
    const srcset = source.getAttribute("srcset") || "";
    if (/\bhttps?:|\bdata:image\//i.test(srcset)) source.removeAttribute("srcset");
  });
  return doc.body.innerHTML;
}

/** Wrap an email HTML body in a locked-down document: sandboxed iframe + a CSP
 * that blocks scripts, fetches, forms, and direct remote image loads. */
function buildReaderDoc(body: string): string {
  const csp =
    "default-src 'none'; img-src data: mailright-img: http://mailright-img.localhost; style-src 'unsafe-inline'; font-src data:";
  const copper = cssVar("--copper", "#c46a3d");
  const paper = cssVar("--paper", "#faf8f4");
  const reset =
    "html,body{margin:0;padding:0;background:#fff}" +
    "body{font:14px/1.5 Inter,system-ui,-apple-system,sans-serif;color:#1a1a1a;" +
    "word-wrap:break-word;overflow-wrap:break-word}" +
    "img{max-width:100%;height:auto}" +
    "img[data-mailright-image-blocked]{display:none!important}" +
    "summary{cursor:pointer;color:#8a8a8a;font-size:13px;margin:8px 0;list-style:none}" +
    "summary::-webkit-details-marker{display:none}" +
    "details>*:not(summary){border-left:2px solid #e5e0d6;padding-left:10px;margin-top:6px}" +
    // Find-in-page highlights (the iframe is its own document — the app's
    // ::highlight rules don't reach it).
    `::highlight(search-match){background-color:color-mix(in srgb, ${copper} 38%, transparent)}` +
    `::highlight(search-match-active){background-color:${copper};color:${paper}}`;
  return `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${csp}"><style>${reset}</style></head><body>${body}</body></html>`;
}

/** HTML email body in a locked-down iframe that grows to fit its content. */
export function EmailFrame({
  html,
  title,
  loadImages,
  findId,
  registerFind,
}: {
  html: string;
  title: string;
  loadImages: boolean;
  /** Stable id (message id) for the find registry; omit to disable find here. */
  findId?: string;
  /** Register/unregister this frame's per-iframe search surface. */
  registerFind?: (id: string, api: IframeFind | null) => void;
}) {
  const ref = useRef<HTMLIFrameElement>(null);
  const obs = useRef<ResizeObserver | null>(null);
  const docRef = useRef<Document | null>(null);
  const winRef = useRef<Window | null>(null);
  const matchesRef = useRef<TextMatch[]>([]);

  function fit() {
    const f = ref.current;
    if (!f) return;
    try {
      const doc = f.contentDocument;
      if (!doc) return;
      const h = Math.max(
        doc.documentElement ? doc.documentElement.scrollHeight : 0,
        doc.body ? doc.body.scrollHeight : 0
      );
      if (h > 0 && Math.abs(h - f.clientHeight) > 1) f.style.height = `${h}px`;
    } catch {
      /* contentDocument blocked — leave the CSS height */
    }
  }

  function observe() {
    obs.current?.disconnect();
    try {
      const body = ref.current?.contentDocument?.body;
      if (body && "ResizeObserver" in window) {
        obs.current = new ResizeObserver(() => fit());
        obs.current.observe(body);
      }
    } catch {
      /* ignore */
    }
  }

  // Per-iframe search surface (created once; methods read refs at call time).
  // Resolve the iframe's live document/window, capturing into refs. Falls back
  // to reading the iframe element directly so search works even if the raf watch
  // hasn't captured the doc yet (e.g. a second message that loaded late).
  const liveDoc = () => {
    const f = ref.current;
    const doc = docRef.current || f?.contentDocument || null;
    const win = winRef.current || f?.contentWindow || null;
    if (doc) docRef.current = doc;
    if (win) winRef.current = win;
    return { doc, win };
  };
  const [api] = useState<IframeFind>(() => ({
    el: () => ref.current,
    search: (q: string, opts: SearchOptions) => {
      const { doc, win } = liveDoc();
      if (!doc?.body || !win || !q) {
        matchesRef.current = [];
        if (win) clearHighlights(win);
        return 0;
      }
      const nodes = getVisibleTextNodes(doc.body, win);
      const full = nodes.map((n) => n.node.nodeValue || "").join("");
      matchesRef.current = findLiteralMatches(full, q, opts);
      return matchesRef.current.length;
    },
    paint: (activeLocalIndex: number) => {
      const { doc, win } = liveDoc();
      if (!doc?.body || !win) return;
      const ranges = rangesForMatches(matchesRef.current, doc, win);
      applyHighlights(ranges, activeLocalIndex, win);
      const active = ranges[activeLocalIndex];
      if (active) active.startContainer.parentElement?.scrollIntoView({ block: "center" });
    },
    clear: () => {
      matchesRef.current = [];
      const { win } = liveDoc();
      if (win) clearHighlights(win);
    },
  }));

  // Register this frame's search surface for the thread-wide coordinator.
  useEffect(() => {
    if (!registerFind || !findId) return;
    registerFind(findId, api);
    return () => registerFind(findId, null);
  }, [registerFind, findId, api]);

  // Size on load + a few post-load ticks (fonts/tables/images settle).
  // Each tick disconnects the old observer before re-attaching to the current
  // body, so a srcDoc change doesn't silently strand the observer on the old
  // document.
  useEffect(() => {
    obs.current?.disconnect();
    obs.current = null;
    const tick = () => {
      fit();
      obs.current?.disconnect();
      try {
        const body = ref.current?.contentDocument?.body;
        if (body && "ResizeObserver" in window) {
          obs.current = new ResizeObserver(() => fit());
          obs.current.observe(body);
        }
      } catch { /* cross-origin guard */ }
    };
    const timers = [0, 80, 300, 800, 1500].map((d) => window.setTimeout(tick, d));
    return () => {
      timers.forEach(window.clearTimeout);
      obs.current?.disconnect();
      obs.current = null;
    };
  }, [html, loadImages]);

  // Capture the iframe's document/window once srcDoc loads and attach the
  // Find-only shortcut registry to the iframe document (same-origin + focusable →
  // keystrokes fire in the iframe doc, not the parent), so Ctrl/Cmd+F opens the
  // app FindBar instead of WebView2's native find. No other shortcut is attached
  // to email content. Uses a real load listener + an "already loaded" check because
  // for srcDoc iframes React's onLoad reliably misses the first-mount load.
  useLayoutEffect(() => {
    const iframe = ref.current;
    if (!iframe) return;
    let attachedDoc: Document | null = null;
    let detachKeys: (() => void) | null = null;
    let raf = 0;
    let attempts = 0;
    const detach = () => {
      detachKeys?.();
      detachKeys = null;
      attachedDoc = null;
    };
    const setup = () => {
      const doc = iframe.contentDocument;
      const win = iframe.contentWindow;
      if (!doc) return;
      docRef.current = doc;
      winRef.current = win;
      if (attachedDoc !== doc) {
        detach();
        detachKeys = getEmailFrameShortcuts().attach(doc);
        attachedDoc = doc;
      }
    };
    const watch = () => {
      setup();
      attempts += 1;
      if (attempts < 120) raf = window.requestAnimationFrame(watch);
    };
    iframe.addEventListener("load", setup);
    setup();
    raf = window.requestAnimationFrame(watch);
    return () => {
      if (raf) window.cancelAnimationFrame(raf);
      iframe.removeEventListener("load", setup);
      detach();
    };
  }, [html, loadImages]);

  return (
    <iframe
      ref={ref}
      className="msg-html"
      sandbox="allow-same-origin"
      srcDoc={buildReaderDoc(prepareBody(html, loadImages))}
      title={title}
      onLoad={() => {
        fit();
        observe();
      }}
    />
  );
}
