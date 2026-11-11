import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import DOMPurify from "dompurify";
import type { SearchOptions, SearchProvider, SearchResult } from "@rightkit/document-core/documents/types";
import {
  applySpanHighlights,
  clearSpanHighlights,
  findLiteralMatches,
  getVisibleTextNodes,
  type TextMatch,
  type TextNodeMapping,
} from "@rightkit/document-core/documents/searchUtils";
import type { SiteRootGrant } from "./htmlInline.js";
import { appColorScheme, buildPreviewHtml, isLocalRelative, previewBootHtml } from "./htmlInline.js";
import {
  htmlSrcDocCache,
  trackCancelledHtmlBuild,
  trackHtmlIframe,
  trackHtmlObserver,
} from "./htmlResourceLifecycle.js";
import {
  grantRelativeFile,
  openDevtools,
  openExternal,
  setDevtoolsShortcutScope,
} from "@rightkit/document-core/state/tauriBridge";
import { BackToTopButton } from "@rightkit/document-core/documents/BackToTopButton";
import {
  safeBlockTargetFromElement,
  htmlWordSelectionFromPoint,
  type HtmlInspectTarget,
  type HtmlSafeBlockTarget,
  type HtmlTextSelection,
} from "./sourceMap.js";
import { HTML_INLINE_EDITABLE_SELECTOR, htmlElementPathFromBody, sourceChecksum } from "./sourceMap.js";
import { scrollBehaviorForMotion } from "@rightkit/document-core/state/motion";
import { applyHtmlThemeStyle, htmlThemeStyleHtml } from "./htmlTheme.js";
import {
  clampPreviewZoom,
  inspectTargetFromClick,
  isInsideBlockEditor,
  webKitSelectedWordTarget,
} from "./htmlViewerUtils.js";
import { publishHtmlSurface } from "./htmlBridgeScript.js";
import { hostScheme } from "@rightkit/document-core/host";

function stripSafeResourceHints(html: string): string {
  const doc = new DOMParser().parseFromString(html, "text/html");
  const speculative = new Set([
    "dns-prefetch",
    "modulepreload",
    "preconnect",
    "prefetch",
    "preload",
  ]);
  for (const link of Array.from(doc.querySelectorAll<HTMLLinkElement>("link[rel]"))) {
    const rels = link.rel.toLowerCase().split(/\s+/).filter(Boolean);
    if (rels.some((rel) => speculative.has(rel))) link.remove();
  }
  return `<!doctype html>${doc.documentElement.outerHTML}`;
}

/**
 * Renders opened .html in a sanitized, scriptless, **no-network** iframe.
 *
 * Security model: the markup is DOMPurify-sanitized (no scripts/forms/iframes/embeds) AND a strict
 * CSP `<meta>` is injected so the document can't fetch anything (`default-src 'none'`; only inline
 * styles + data: images/fonts). The iframe uses `sandbox="allow-same-origin"` — never
 * `allow-scripts` — so the page still can't run JS, but the app CAN read its DOM to power find. The
 * combination (no scripts + strict CSP + sanitized) is what makes `allow-same-origin` safe here.
 *
 * Find: registers an `html-preview` SearchProvider that highlights matches via the CSS Custom
 * Highlight API inside the iframe's own window (paint-only, no DOM mutation).
 */
export function HtmlViewer({
  content,
  docPath,
  rootPath,
  siteRoot,
  registerSearchProvider,
  onRequestFind,
  onOpenPath,
  themeKey,
  showBackToTop = true,
  inspectActive = false,
  onInspectTarget,
  onInspectMenu,
  onSafeBlockTarget,
  onLinkHover,
  active = true,
  externalIframeRef,
}: {
  content: string;
  docPath?: string;
  /** Entry-file path: widens asset containment to the original site root so a navigated subpage can
   *  inline `../assets/…` that lives in a sibling folder under the root (matches Interactive mode). */
  rootPath?: string;
  /** Granted site root for this tab. When present the preview resolves subresources through the
   *  `viewright-site://` protocol instead of inlining them. */
  siteRoot?: SiteRootGrant | null;
  registerSearchProvider?: (provider: SearchProvider | null) => void;
  onRequestFind?: () => void;
  /** Open a brokered absolute path as a host app tab (used for internal relative-link navigation). */
  onOpenPath?: (path: string, opts?: { background?: boolean }) => void;
  /** Changes when the app theme/palette changes — hot-applies iframe theme CSS without rebuilding. */
  themeKey?: string;
  showBackToTop?: boolean;
  inspectActive?: boolean;
  onInspectTarget?: (target: HtmlInspectTarget) => void;
  onInspectMenu?: (target: HtmlInspectTarget, point: { x: number; y: number }) => void;
  onSafeBlockTarget?: (target: HtmlSafeBlockTarget) => void;
  /** Report the URL of the link under the pointer (or null when none) for the status strip. */
  onLinkHover?: (href: string | null) => void;
  /** Inactive keep-alive panes retain rendered DOM but cancel pending asset/srcDoc work. */
  active?: boolean;
  /** When provided, the parent owns the iframe ref so it can resolve the live target element for in-frame mounting. */
  externalIframeRef?: import("react").RefObject<HTMLIFrameElement | null>;
}) {
  const internalIframeRef = useRef<HTMLIFrameElement>(null);
  const iframeRef = externalIframeRef ?? internalIframeRef;
  const [iframeNode, setIframeNode] = useState<HTMLIFrameElement | null>(null);
  const bindIframe = useCallback(
    (node: HTMLIFrameElement | null) => {
      internalIframeRef.current = node;
      if (externalIframeRef) externalIframeRef.current = node;
      setIframeNode((current) => (current === node ? current : node));
    },
    [externalIframeRef],
  );
  const docRef = useRef<Document | null>(null);
  const winRef = useRef<Window | null>(null);
  const selectedElementRef = useRef<Element | null>(null);
  const matchesRef = useRef<TextMatch[]>([]);
  const activeRef = useRef(-1);
  const queryRef = useRef("");
  const optionsRef = useRef<SearchOptions>({ matchCase: false, wholeWord: false });
  // Latest callbacks/path in refs so the iframe event handlers stay referentially stable.
  const onRequestFindRef = useRef(onRequestFind);
  onRequestFindRef.current = onRequestFind;
  const onOpenPathRef = useRef(onOpenPath);
  onOpenPathRef.current = onOpenPath;
  const onInspectTargetRef = useRef(onInspectTarget);
  onInspectTargetRef.current = onInspectTarget;
  const onInspectMenuRef = useRef(onInspectMenu);
  onInspectMenuRef.current = onInspectMenu;
  const onSafeBlockTargetRef = useRef(onSafeBlockTarget);
  onSafeBlockTargetRef.current = onSafeBlockTarget;
  const onLinkHoverRef = useRef(onLinkHover);
  onLinkHoverRef.current = onLinkHover;
  const hoveredHrefRef = useRef<string | null>(null);
  const inspectActiveRef = useRef(inspectActive);
  inspectActiveRef.current = inspectActive;
  const docPathRef = useRef(docPath);
  docPathRef.current = docPath;
  const rootPathRef = useRef(rootPath);
  rootPathRef.current = rootPath;
  const siteRootRef = useRef(siteRoot);
  siteRootRef.current = siteRoot;
  const ownsDevtoolsScopeRef = useRef(false);
  const iframeFocusedRef = useRef(false);
  const iframeHoveredRef = useRef(false);
  const safeZoomRef = useRef(1);
  const zoomHudTimerRef = useRef<number | null>(null);
  const [zoomHud, setZoomHud] = useState<string | null>(null);
  // Mirrors safeZoomRef into render state. The ref alone drives the iframe, but the reset control
  // has to survive the HUD's 900ms timeout, so whether we are off 100% must be a rendered value.
  const [safeZoom, setSafeZoom] = useState(1);

  const applySafeZoom = useCallback((doc: Document | null, zoom = safeZoomRef.current) => {
    if (!doc?.documentElement) return;
    const style = doc.documentElement.style as CSSStyleDeclaration & { zoom: string };
    style.zoom = String(zoom);
    style.transformOrigin = "0 0";
  }, []);

  const showZoomHud = useCallback((zoom: number) => {
    setZoomHud(`${Math.round(zoom * 100)}%`);
    if (zoomHudTimerRef.current !== null) window.clearTimeout(zoomHudTimerRef.current);
    zoomHudTimerRef.current = window.setTimeout(() => setZoomHud(null), 900);
  }, []);

  const adjustSafeZoom = useCallback(
    (deltaY: number) => {
      const next = clampPreviewZoom(safeZoomRef.current + (deltaY < 0 ? 0.1 : -0.1));
      if (next === safeZoomRef.current) return;
      safeZoomRef.current = next;
      setSafeZoom(next);
      applySafeZoom(docRef.current, next);
      showZoomHud(next);
    },
    [applySafeZoom, showZoomHud],
  );

  // Ctrl/Cmd+wheel could zoom the preview but nothing offered a way back, so a mis-scrolled page
  // looked permanently stuck at the wrong size.
  const resetSafeZoom = useCallback(() => {
    safeZoomRef.current = 1;
    setSafeZoom(1);
    applySafeZoom(docRef.current, 1);
    setZoomHud(null);
    if (zoomHudTimerRef.current !== null) window.clearTimeout(zoomHudTimerRef.current);
  }, [applySafeZoom]);

  const scrollSafePreviewToTop = useCallback(() => {
    const win = winRef.current;
    const doc = docRef.current;
    try {
      win?.scrollTo({ top: 0, behavior: scrollBehaviorForMotion() });
    } catch (_) {
      win?.scrollTo(0, 0);
    }
    const root = doc?.scrollingElement ?? doc?.documentElement;
    if (root) root.scrollTop = 0;
  }, []);

  useEffect(() => {
    if (!active) return;
    return () => {
      if (zoomHudTimerRef.current !== null) window.clearTimeout(zoomHudTimerRef.current);
    };
  }, [active]);

  // The iframe srcDoc, built asynchronously: local assets (stylesheets/images/fonts) are inlined
  // from the document's folder, THEN the whole thing is DOMPurify-sanitized and a strict CSP +
  // find-highlight + scrollbar styles are injected. Async because inlining reads sibling files over
  // IPC; empty until the first build completes (a brief blank, fine).
  const [srcDoc, setSrcDoc] = useState(() => previewBootHtml());
  // Stale async renders must not replace newer revision: track build generation.
  const buildIdRef = useRef(0);
  useEffect(() => {
    const buildId = ++buildIdRef.current;
    if (!active) return;
    let cancelled = false;
    void (async () => {
      const cacheKey = `${docPath ?? ""}\u0000${rootPath ?? ""}\u0000${siteRoot?.token ?? ""}\u0000${sourceChecksum(content)}`;
      const cached = htmlSrcDocCache.get(cacheKey);
      if (cached) {
        if (!cancelled && buildId === buildIdRef.current) setSrcDoc(cached);
        return;
      }
      const inlined = await buildPreviewHtml(content, docPath, rootPath, siteRoot);
      if (cancelled || buildId !== buildIdRef.current) {
        trackCancelledHtmlBuild();
        return;
      }
      // Strict CSP: no network of any kind, inline styles + local/data assets only. Injected at the START
      // of <head> so it governs everything that follows.
      const viewport = `<meta name="viewport" content="width=device-width, initial-scale=1">`;
      // Explicit script-src 'none' is redundant for policy but required to suppress WebKit 218086:
      // Safari otherwise reports every parent-owned event listener on scriptless srcdoc as blocked.
      const site = hostScheme("site");
      const csp = `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'none'; img-src ${site}: data: blob:; media-src ${site}: data: blob:; style-src 'unsafe-inline' ${site}:; font-src ${site}: data:; base-uri ${site}:; form-action 'none'">`;
      // DOMPurify flattens locked custom elements (KEEP_CONTENT), so child indices counted on the
      // sanitized DOM drift from source. Stamp each editable element with its source-equivalent
      // body path on the FIRST hook call, while DOMPurify's parsed tree is still unmutated; Safe
      // double-click targets read the stamp instead of recounting the flattened tree.
      let stamped = false;
      const stampSourcePaths = (node: Node) => {
        if (stamped) return;
        stamped = true;
        const body = node.ownerDocument?.body;
        if (!body) return;
        for (const element of Array.from(body.querySelectorAll(HTML_INLINE_EDITABLE_SELECTOR)))
          element.setAttribute("data-vr-edit-path", htmlElementPathFromBody(element).join("."));
      };
      DOMPurify.addHook("beforeSanitizeElements", stampSourcePaths);
      let clean: string;
      try {
        clean = DOMPurify.sanitize(inlined, {
          WHOLE_DOCUMENT: true,
          // <base> is ours (injected by buildPreviewHtml, and any page-authored one was removed
          // there), and it is what makes relative subresources resolve — it must survive sanitizing.
          ADD_TAGS: ["base"],
          FORBID_TAGS: ["script", "iframe", "object", "embed", "form"],
          FORBID_ATTR: ["onerror", "onload", "onclick"],
        });
      } finally {
        DOMPurify.removeHook("beforeSanitizeElements", stampSourcePaths);
      }
      // Safe mode already resolves local assets before commit. Speculative resource hints cannot
      // improve this scriptless frame and otherwise produce CSP/403/unused-preload console noise.
      const brokered = neutralizePreviewLinks(stripSafeResourceHints(clean));
      const hasViewport = /<meta\s+[^>]*name=["']viewport["'][^>]*>/i.test(brokered);
      const inject = `${csp}${hasViewport ? "" : viewport}${htmlThemeStyleHtml()}`;
      const next = brokered.includes("<head>")
        ? brokered.replace("<head>", `<head>${inject}`)
        : inject + brokered;
      htmlSrcDocCache.set(cacheKey, next);
      setSrcDoc(next);
    })();
    return () => {
      cancelled = true;
      buildIdRef.current += 1;
    };
  }, [active, content, docPath, rootPath, siteRoot]);

  useEffect(() => {
    const iframe = iframeRef.current;
    if (iframe) iframe.dataset.viewrightTheme = themeKey ?? "";
    applyHtmlThemeStyle(iframe?.contentDocument);
  }, [themeKey, iframeRef.current]);

  // Build Ranges for the current matches in the iframe document and paint them; scroll the active
  // match into view. Highlights live in the iframe's own CSS.highlights registry.
  const applyMatches = useCallback((precomputedMappings?: TextNodeMapping[]) => {
    const doc = docRef.current;
    const win = winRef.current;
    if (!doc || !win || !doc.body) return;
    applySpanHighlights(doc.body, matchesRef.current, activeRef.current, win, precomputedMappings);
  }, []);

  const runFind = useCallback(
    (q: string, options: SearchOptions): SearchResult => {
      queryRef.current = q;
      optionsRef.current = options;
      const doc = docRef.current;
      const win = winRef.current;
      if (!doc || !win || !doc.body || !q) {
        matchesRef.current = [];
        activeRef.current = -1;
        if (doc?.body) clearSpanHighlights(doc.body);
        return { count: 0, activeIndex: -1 };
      }
      clearSpanHighlights(doc.body); // merge any prior match spans back so offsets map over clean text
      const nodes = getVisibleTextNodes(doc.body, win);
      const fullText = nodes.map((n) => n.node.nodeValue || "").join("");
      matchesRef.current = findLiteralMatches(fullText, q, options);
      activeRef.current = matchesRef.current.length > 0 ? 0 : -1;
      // Reuse the mapping just walked above instead of making applySpanHighlights re-walk the DOM a
      // second time per keystroke — nothing mutates between here and there.
      applyMatches(nodes);
      return { count: matchesRef.current.length, activeIndex: activeRef.current };
    },
    [applyMatches],
  );

  // Register the html-preview search provider (read-only — no replace inside a preview).
  useEffect(() => {
    if (!registerSearchProvider) return;
    const step = (delta: number): SearchResult => {
      const n = matchesRef.current.length;
      if (!n) return { count: 0, activeIndex: -1 };
      activeRef.current = (activeRef.current + delta + n) % n;
      applyMatches();
      return { count: n, activeIndex: activeRef.current };
    };
    const provider: SearchProvider = {
      kind: "html-preview",
      canReplace: false,
      supportsMatchCase: true,
      supportsWholeWord: true,
      supportsRegex: true,
      find: (q, options) => runFind(q, options),
      next: () => step(1),
      prev: () => step(-1),
      close: () => {
        matchesRef.current = [];
        activeRef.current = -1;
        queryRef.current = "";
        if (docRef.current?.body) clearSpanHighlights(docRef.current.body);
      },
    };
    registerSearchProvider(provider);
    return () => {
      if (docRef.current?.body) clearSpanHighlights(docRef.current.body);
      registerSearchProvider(null);
    };
  }, [registerSearchProvider, runFind, applyMatches]);

  // A Safe block commit reloads this iframe. Clear its prior selection during that handoff.
  const contentRevision = sourceChecksum(content);
  useLayoutEffect(() => {
    void contentRevision;
    selectedElementRef.current = null;
  }, [contentRevision]);

  // Capture the iframe's document/window once its srcDoc has loaded, attach an in-iframe Ctrl/Cmd+F
  // interceptor (once the iframe is same-origin + focusable, that keystroke fires in the iframe's
  // document, not the parent window — so we catch it here, open the app FindBar, and suppress
  // WebView2's native find), and re-apply any active query.
  //
  // Done via a real `load` listener + an "already loaded" check rather than React's `onLoad` prop:
  // for `srcDoc` iframes the prop reliably MISSES the first-mount load event (the doc parses before
  // React attaches the handler), which is why find only worked after a source→render round-trip.
  useLayoutEffect(() => {
    const iframe = iframeNode;
    if (!iframe) return;
    let attachedDoc: Document | null = null;
    let attachedWin: Window | null = null;
    let attachedRoot: Element | null = null;
    let lastPublishedDoubleClick: { element: Element; at: number } | null = null;
    let selectionTimer = 0;
    let lastWebKitSelection = "";
    let raf = 0;
    let attempts = 0;
    const isScriptlessWebKit =
      /AppleWebKit/i.test(navigator.userAgent) &&
      !/(Chrome|Chromium|Edg)/i.test(navigator.userAgent);
    const isNativeScriptlessWebKit =
      isScriptlessWebKit && typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
    const publishSelectedTarget = (capturedSelection?: HtmlTextSelection) => {
      const selectedElement = selectedElementRef.current;
      if (!selectedElement?.isConnected || !onSafeBlockTargetRef.current) return;
      const target = safeBlockTargetFromElement(
        selectedElement,
        docPathRef.current ?? "",
        selectedElement.getBoundingClientRect(),
        capturedSelection,
      );
      if (!target) return;
      onSafeBlockTargetRef.current(target);
    };
    const publishDoubleClickTarget = (e: MouseEvent): boolean => {
      // WebKit can expose a text node (or a foreign-realm Element) as event.target. Resolve from
      // composedPath so text hits inside an authored heading still reach its block target.
      const eventPath = typeof e.composedPath === "function" ? e.composedPath() : [];
      const element = [e.target, ...eventPath]
        .find(
          (candidate): candidate is Element =>
            !!candidate && typeof (candidate as Element).closest === "function",
        )
        ?.closest("*");
      if (!element || !onSafeBlockTargetRef.current) return false;
      // Double-clicks inside the open block editor select words for editing; publishing them would
      // address editor-owned nodes that never map to source (and swallow the editor's own gesture).
      if (isInsideBlockEditor(element)) return false;
      const now = Date.now();
      if (
        lastPublishedDoubleClick?.element === element &&
        now - lastPublishedDoubleClick.at < 500
      ) {
        if (!isNativeScriptlessWebKit) e.preventDefault();
        e.stopPropagation();
        return true;
      }
      selectedElementRef.current = element;
      lastPublishedDoubleClick = { element, at: now };
      if (isNativeScriptlessWebKit) {
        // WKWebView finalizes native word selection after dblclick dispatch. Keep Interactive DOM
        // alive for one frame, then capture selection before switching to scriptless Safe.
        const clientX = e.clientX;
        const clientY = e.clientY;
        e.stopPropagation();
        const publish = () => {
          const rawText = element.textContent ?? "";
          const trimmedText = rawText.trim();
          const trimmedFrom = rawText.indexOf(trimmedText);
          const selection =
            htmlWordSelectionFromPoint(element, clientX, clientY) ??
            (trimmedText
              ? {
                  from: Math.max(0, trimmedFrom),
                  to: Math.max(0, trimmedFrom) + trimmedText.length,
                  text: trimmedText,
                }
              : undefined);
          publishSelectedTarget(selection);
        };
        if (attachedWin) attachedWin.requestAnimationFrame(publish);
        else window.requestAnimationFrame(publish);
        return true;
      }
      e.preventDefault();
      e.stopPropagation();
      publishSelectedTarget(htmlWordSelectionFromPoint(element, e.clientX, e.clientY));
      return true;
    };
    const watchWebKitSelection = () => {
      // Scriptless sandbox callbacks run normally in Chromium, but WKWebView blocks them. Its
      // parent and child focus APIs both remain false while native text selection is visibly active,
      // so selection itself is the only reliable signal. A one-node word range is WebKit's native
      // double-click result; broader drag/keyboard ranges are rejected by webKitSelectedWordTarget.
      if (attachedWin) {
        // WKWebView exposes its native iframe selection through Document.getSelection() while
        // Window.getSelection() can remain empty for a scriptless sandbox.
        const documentSelection = attachedDoc?.getSelection?.();
        const selection =
          documentSelection && !documentSelection.isCollapsed
            ? documentSelection
            : attachedWin.getSelection();
        const element = webKitSelectedWordTarget(selection);
        if (!element) lastWebKitSelection = "";
        else {
          const signature = `${selection?.anchorOffset}:${selection?.focusOffset}:${selection?.toString()}`;
          if (signature !== lastWebKitSelection) {
            lastWebKitSelection = signature;
            selectedElementRef.current = element;
            publishSelectedTarget();
          }
        }
      } else lastWebKitSelection = "";
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented) return;
      const openInspector =
        (e.key === "F12" && !e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey) ||
        ((e.ctrlKey || e.metaKey) && (e.altKey || e.shiftKey) && e.code === "KeyI");
      if (openInspector) {
        e.preventDefault();
        e.stopPropagation();
        void openDevtools();
        return;
      }
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
        e.preventDefault();
        onRequestFindRef.current?.();
      }
    };
    const onWheel = (e: WheelEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      e.preventDefault();
      e.stopPropagation();
      adjustSafeZoom(e.deltaY);
    };
    // Broker link navigation: the sandboxed preview never navigates itself. Same-page #anchors work
    // natively; local relative links open as the host app file tabs; http(s) links open as the host app web
    // tabs. mailto/other supported external schemes still go through the OS allowlist.
    const routeAnchor = (e: MouseEvent): boolean => {
      const anchor = anchorFromEvent(e);
      if (!anchor) return false;
      const href = anchor.getAttribute("data-viewright-href") || anchor.getAttribute("href") || "";
      if (!href || href.startsWith("#")) return false; // in-page anchor → native scroll
      e.preventDefault();
      const newTab = e.button === 1 || e.metaKey || e.ctrlKey;
      if (isLocalRelative(href)) {
        const base = docPathRef.current;
        if (base) {
          void grantRelativeFile(base, href, rootPathRef.current, siteRootRef.current?.token)
            .then((p) => {
              if (!p) return;
              if (newTab) onOpenPathRef.current?.(p, { background: true });
              else onOpenPathRef.current?.(p);
            })
            .catch(() => {
              // A denied relative-file grant leaves the current preview unchanged.
            });
        }
      } else {
        // A document's http(s) link opens in the OS browser (both plain and Cmd/Ctrl/middle click).
        // The in-app web tab can't render most real sites — they render blank in a cross-origin
        // iframe — so the OS browser is the correct target for an external link from a document.
        void openExternal(href);
      }
      return true;
    };
    const onClick = (e: MouseEvent) => {
      if (e.defaultPrevented || e.button !== 0) return;
      // WKWebView's native pointer path can emit the second click with detail=2 while omitting
      // dblclick entirely. Treat it as the same edit gesture; onDblClick remains the fallback for
      // engines that do emit the dedicated event.
      if (e.detail >= 2 && publishDoubleClickTarget(e)) return;
      if (inspectActiveRef.current) {
        const target = docRef.current
          ? inspectTargetFromClick(docRef.current, winRef.current, e)
          : null;
        if (target) {
          e.preventDefault();
          e.stopPropagation();
          onInspectTargetRef.current?.(target);
          return;
        }
      }
      // A click on a link opens it (external links go to the OS browser). This is the expected
      // document-viewer behavior and takes priority over the block editor's double-click affordance;
      // link text is edited via the toolbar's link control, not by clicking the link itself.
      routeAnchor(e);
    };
    // Middle-click (button 1) → open in a new tab, same routing as a normal click.
    const onAuxClick = (e: MouseEvent) => {
      if (e.defaultPrevented || e.button !== 1) return;
      routeAnchor(e);
    };
    // WKWebView can miss document-level capture listeners during srcdoc swaps. Bind each rendered
    // anchor too, so its own activation is always brokered before native iframe navigation.
    const onAnchorClick = (e: MouseEvent) => {
      if (e.defaultPrevented || e.button !== 0) return;
      routeAnchor(e);
    };
    const onAnchorAuxClick = (e: MouseEvent) => {
      if (e.defaultPrevented || e.button !== 1) return;
      routeAnchor(e);
    };
    const onContextMenu = (e: MouseEvent) => {
      // Plain right-click must reach WKWebView/WebView2 so native Inspect Element opens DevTools on
      // the exact iframe node. Shift+right-click keeps the host app source/link action menu available.
      if (!e.shiftKey) return;
      const target = docRef.current
        ? inspectTargetFromClick(docRef.current, winRef.current, e)
        : null;
      if (!target) return;
      e.preventDefault();
      e.stopPropagation();
      const rect = iframe.getBoundingClientRect();
      onInspectMenuRef.current?.(target, { x: rect.left + e.clientX, y: rect.top + e.clientY });
    };
    // Hover over a link → report its URL for the status strip; clear it when the pointer leaves.
    const reportHover = (href: string | null) => {
      if (href === hoveredHrefRef.current) return;
      hoveredHrefRef.current = href;
      onLinkHoverRef.current?.(href);
    };
    const onMouseOver = (e: MouseEvent) => {
      const anchor = anchorFromEvent(e);
      reportHover(
        anchor
          ? (
              anchor.getAttribute("data-viewright-href") ||
              anchor.getAttribute("href") ||
              ""
            ).trim() || null
          : null,
      );
    };
    const onMouseOut = (e: MouseEvent) => {
      if (!closestAnchor(e.relatedTarget)) reportHover(null);
    };
    const onDblClick = (e: MouseEvent) => {
      const doc = docRef.current;
      if (!doc) return;
      publishDoubleClickTarget(e);
    };
    let attachedAnchors: HTMLAnchorElement[] = [];
    const detachAnchors = () => {
      for (const anchor of attachedAnchors) {
        anchor.removeEventListener("click", onAnchorClick);
        anchor.removeEventListener("auxclick", onAnchorAuxClick);
      }
      attachedAnchors = [];
    };
    const bindAnchors = (doc: Document) => {
      detachAnchors();
      attachedAnchors = Array.from(doc.querySelectorAll<HTMLAnchorElement>("a[href]"));
      for (const anchor of attachedAnchors) {
        anchor.addEventListener("click", onAnchorClick);
        anchor.addEventListener("auxclick", onAnchorAuxClick);
      }
    };
    const detach = () => {
      detachAnchors();
      attachedDoc?.removeEventListener("keydown", onKey, true);
      attachedDoc?.removeEventListener("auxclick", onAuxClick, true);
      attachedDoc?.removeEventListener("contextmenu", onContextMenu, true);
      attachedDoc?.removeEventListener("click", onClick, true);
      attachedDoc?.removeEventListener("dblclick", onDblClick, true);
      attachedDoc?.removeEventListener("mouseover", onMouseOver, true);
      attachedDoc?.removeEventListener("mouseout", onMouseOut, true);
      reportHover(null);
      attachedWin?.removeEventListener("keydown", onKey, true);
      attachedWin?.removeEventListener("wheel", onWheel, true);
      selectedElementRef.current = null;
      attachedDoc = null;
      attachedWin = null;
      attachedRoot = null;
    };
    const setup = (forceRebind = false) => {
      const doc = iframe.contentDocument;
      const win = iframe.contentWindow;
      if (!doc) return;
      docRef.current = doc;
      winRef.current = win;
      // jsdom does not parse iframe srcDoc automatically. Hydrate its empty document for unit
      // coverage; packaged WebView documents already contain authored nodes and skip this branch.
      if (doc.body && !doc.body.children.length && content) {
        const parsed = new DOMParser().parseFromString(content, "text/html");
        doc.body.innerHTML = parsed.body.innerHTML;
      }
      // WKWebView can keep the same contentDocument wrapper across srcdoc navigation while
      // replacing documentElement and dropping every listener attached to the old document.
      if (
        forceRebind ||
        attachedDoc !== doc ||
        attachedWin !== win ||
        attachedRoot !== doc.documentElement
      ) {
        detach();
        doc.addEventListener("keydown", onKey, true);
        doc.addEventListener("auxclick", onAuxClick, true);
        doc.addEventListener("contextmenu", onContextMenu, true);
        // Own pointer activation at one stable boundary. Registering the same capture handler on
        // window, document, html, and body lets WKWebView stop the event at an outer wrapper before
        // it exposes the authored element target to the document listener.
        doc.addEventListener("click", onClick, true);
        doc.addEventListener("dblclick", onDblClick, true);
        doc.addEventListener("mouseover", onMouseOver, true);
        doc.addEventListener("mouseout", onMouseOut, true);
        win?.addEventListener("keydown", onKey, true);
        win?.addEventListener("wheel", onWheel, { capture: true, passive: false });
        attachedDoc = doc;
        attachedWin = win;
        attachedRoot = doc.documentElement;
      }
      bindAnchors(doc);
      if (doc.documentElement)
        doc.documentElement.style.cursor = inspectActiveRef.current ? "crosshair" : "";
      const bodyColor = win && doc.body ? win.getComputedStyle(doc.body).backgroundColor : "";
      publishHtmlSurface(iframe, bodyColor) ||
        publishHtmlSurface(
          iframe,
          win && doc.documentElement
            ? win.getComputedStyle(doc.documentElement).backgroundColor
            : "",
        );
      applySafeZoom(doc);
      if (queryRef.current) runFind(queryRef.current, optionsRef.current);
    };
    const watchDocumentSwap = () => {
      setup();
      attempts += 1;
      // WebView2 can swap the initial about:blank document for the real srcDoc after React has
      // already committed. Keep watching briefly so first-paint Ctrl+F binds to the final document.
      if (attempts < 120) raf = window.requestAnimationFrame(watchDocumentSwap);
    };
    const restartDocumentWatch = () => {
      if (raf) window.cancelAnimationFrame(raf);
      attempts = 0;
      setup(true);
      raf = window.requestAnimationFrame(watchDocumentSwap);
    };
    const onLoad = () => setup(true);
    iframe.addEventListener("load", onLoad);
    // WKWebView may not emit `load` when React replaces an iframe's async-built srcDoc. Restart
    // document identity watching at the actual srcdoc attribute mutation so listeners attach to
    // the final document rather than the short-lived preview boot document.
    const srcDocObserver = new MutationObserver(restartDocumentWatch);
    srcDocObserver.observe(iframe, { attributes: true, attributeFilter: ["srcdoc"] });
    const pane = iframe.closest("[data-vr-pane-state]");
    const observer = pane ? new MutationObserver(() => setup()) : null;
    if (observer) trackHtmlObserver(1);
    if (pane && observer)
      observer.observe(pane, { attributes: true, attributeFilter: ["data-vr-pane-state"] });
    restartDocumentWatch(); // first-mount race: srcDoc may already have loaded before listeners.
    if (isScriptlessWebKit && active) {
      watchWebKitSelection();
      selectionTimer = window.setInterval(watchWebKitSelection, 100);
    }
    return () => {
      if (raf) window.cancelAnimationFrame(raf);
      if (selectionTimer) window.clearInterval(selectionTimer);
      iframe.removeEventListener("load", onLoad);
      srcDocObserver.disconnect();
      observer?.disconnect();
      if (observer) trackHtmlObserver(-1);
      detach();
    };
  }, [runFind, applySafeZoom, adjustSafeZoom, content, iframeNode, active]);

  useEffect(() => {
    trackHtmlIframe(1);
    return () => {
      if (ownsDevtoolsScopeRef.current) void setDevtoolsShortcutScope(false);
      trackHtmlIframe(-1);
    };
  }, []);

  useEffect(() => {
    const shouldOwn = active && (iframeFocusedRef.current || iframeHoveredRef.current);
    if (shouldOwn === ownsDevtoolsScopeRef.current) return;
    ownsDevtoolsScopeRef.current = shouldOwn;
    void setDevtoolsShortcutScope(shouldOwn);
  }, [active]);

  useEffect(() => {
    if (docRef.current?.documentElement)
      docRef.current.documentElement.style.cursor = inspectActive ? "crosshair" : "";
  }, [inspectActive]);

  return (
    <div className="relative h-full w-full">
      {safeZoom !== 1 ? (
        <button
          type="button"
          onClick={resetSafeZoom}
          aria-label="Reset zoom to 100%"
          className="no-print absolute right-4 top-4 z-10 rounded-md border border-line bg-panel/95 px-2.5 py-1 text-xs font-semibold text-muted shadow-sm hover:text-fg"
        >
          {`${Math.round(safeZoom * 100)}% · Reset`}
        </button>
      ) : zoomHud ? (
        <div className="no-print pointer-events-none absolute right-4 top-4 z-10 rounded-md border border-line bg-panel/95 px-2.5 py-1 text-xs font-semibold text-muted shadow-sm">
          {zoomHud}
        </div>
      ) : null}
      <iframe
        ref={bindIframe}
        title="HTML preview"
        sandbox="allow-same-origin"
        srcDoc={srcDoc}
        onFocus={() => {
          iframeFocusedRef.current = true;
          if (!active || ownsDevtoolsScopeRef.current) return;
          ownsDevtoolsScopeRef.current = true;
          void setDevtoolsShortcutScope(true);
        }}
        onBlur={() => {
          iframeFocusedRef.current = false;
          if (iframeHoveredRef.current || !ownsDevtoolsScopeRef.current) return;
          ownsDevtoolsScopeRef.current = false;
          void setDevtoolsShortcutScope(false);
        }}
        onMouseEnter={() => {
          iframeHoveredRef.current = true;
          if (!active || ownsDevtoolsScopeRef.current) return;
          ownsDevtoolsScopeRef.current = true;
          void setDevtoolsShortcutScope(true);
        }}
        onMouseLeave={() => {
          iframeHoveredRef.current = false;
          if (iframeFocusedRef.current || !ownsDevtoolsScopeRef.current) return;
          ownsDevtoolsScopeRef.current = false;
          void setDevtoolsShortcutScope(false);
        }}
        className="h-full w-full border-0"
        // See HtmlDocument: match the app scheme + ink so a mode switch doesn't flash white.
        style={{ colorScheme: appColorScheme(), background: "var(--ink)" }}
      />
      {showBackToTop && <BackToTopButton onClick={scrollSafePreviewToTop} />}
    </div>
  );
}
function closestAnchor(value: unknown): HTMLAnchorElement | null {
  const node = value as { closest?: (selector: string) => Element | null } | null;
  return (node?.closest?.("a[href]") as HTMLAnchorElement | null) ?? null;
}

function anchorFromEvent(event: MouseEvent): HTMLAnchorElement | null {
  const direct = closestAnchor(event.target);
  if (direct) return direct;
  for (const item of event.composedPath?.() ?? []) {
    const anchor = closestAnchor(item);
    if (anchor) return anchor;
  }
  return null;
}

/** Keep preview links focusable while making self-navigation impossible if WebKit drops listeners. */
function neutralizePreviewLinks(html: string): string {
  const parsed = new DOMParser().parseFromString(html, "text/html");
  for (const anchor of parsed.querySelectorAll<HTMLAnchorElement>("a[href]")) {
    const href = anchor.getAttribute("href") || "";
    if (!href || href.startsWith("#")) continue;
    anchor.setAttribute("data-viewright-href", href);
    if (isLocalRelative(href)) {
      anchor.setAttribute("href", "#");
    } else {
      const broker = new URL("https://open.viewright.invalid/");
      broker.searchParams.set("target", href);
      anchor.setAttribute("href", broker.toString());
    }
  }
  const doctype = parsed.doctype ? `<!doctype ${parsed.doctype.name}>` : "<!doctype html>";
  return `${doctype}${parsed.documentElement.outerHTML}`;
}
