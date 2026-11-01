import { useCallback, useRef, useState, type MutableRefObject } from "react";
import { grantRelativeFile, openExternal, openFileDialog } from "@rightkit/document-core/state/tauriBridge";
import { formatFromPath } from "@rightkit/document-core/documents/types";
import { isLocalRelative, joinRel } from "./htmlInline";
import { writeClipboardText } from "@rightkit/document-core/state/tauriBridge";
import { hostAppName } from "../../appName";

interface HtmlNavigationDeps {
  /** The originally-opened file — the trusted site root; never changes as you navigate. */
  entryPath: string;
  /** Open a linked doc as its own the host app tab. `background` (new-tab gesture) adds it without
   *  switching away from the current document — like a browser's open-in-new-tab. */
  onOpenPath?: (path: string, opts?: { background?: boolean }) => void;
  /** Shared with the preview-build layer: a navigation starts the page at the top (set to 0). */
  scrollYRef: MutableRefObject<number>;
}

/**
 * The in-tab navigation state machine for one local-site session: the current page (`activePath`,
 * which roams under the entry folder), the back/forward history, the pending cross-page `#fragment`,
 * local-link resolution (relative → brokered absolute path, with a mapped-link cache), and the
 * blocked-link prompt. Extracted from HtmlDocument verbatim — no behavior change.
 */
// The cache must not outlive its originating page or site root: the same relative href resolves
// differently from different pages, and a re-granted site root changes what it may resolve to.
const linkKey = (entry: string, page: string, href: string) => `${entry}\u0000${page}\u0000${href}`;

export function useHtmlNavigation({ entryPath, onOpenPath, scrollYRef }: HtmlNavigationDeps) {
  const [activePath, setActivePath] = useState(entryPath);
  const [history, setHistory] = useState<string[]>([entryPath]);
  const [historyIndex, setHistoryIndex] = useState(0);
  const [pendingFragment, setPendingFragment] = useState<string | null>(null); // #anchor to scroll to after a cross-page nav
  const [blocked, setBlocked] = useState<{ href: string; reason: string } | null>(null);
  const mappedLinks = useRef<Map<string, string>>(new Map());
  // Scroll position per history index, so Back/Forward returns to where you were on that page (the
  // preview build reads scrollYRef when it rebuilds). Index 0 is the entry page, starting at the top.
  const scrollByIndex = useRef<number[]>([0]);

  const navigate = useCallback(
    (path: string, fragment: string | null = null) => {
      setBlocked(null);
      scrollByIndex.current[historyIndex] = scrollYRef.current; // remember where we were before leaving
      scrollYRef.current = 0; // a freshly-navigated page starts at the top (unless a fragment scrolls it)
      setPendingFragment(fragment);
      setActivePath(path);
      setHistory((prev) => {
        const next = [...prev.slice(0, historyIndex + 1), path];
        scrollByIndex.current = [...scrollByIndex.current.slice(0, historyIndex + 1), 0]; // new page starts at top
        setHistoryIndex(next.length - 1);
        return next;
      });
    },
    [historyIndex, scrollYRef],
  );

  const handleResolvedPath = useCallback(
    (href: string, path: string, newTab = false, fromPage = activePath) => {
      mappedLinks.current.set(linkKey(entryPath, fromPage, href), path);
      // Preserve a cross-page `#fragment` (e.g. `formats/#txt`) so the new page scrolls to it.
      const fragment = href.includes("#") ? href.slice(href.indexOf("#") + 1) || null : null;
      // newTab (Ctrl/middle-click): open as its own the host app document tab in the BACKGROUND — even an
      // HTML page, which would otherwise navigate this preview in place. Background = don't switch away,
      // so the current document keeps its scroll position (browser open-in-new-tab behavior).
      if (!newTab && formatFromPath(path) === "html") navigate(path, fragment);
      else onOpenPath?.(path, newTab ? { background: true } : undefined);
    },
    [navigate, onOpenPath, activePath, entryPath],
  );

  const handleHref = useCallback(
    async (href: string, newTab = false) => {
      if (!href || href.startsWith("#")) return;
      if (!isLocalRelative(href)) {
        // A document's http(s) link opens in the OS browser. The in-app web tab cannot render most
        // real sites — they refuse framing (X-Frame-Options / frame-ancestors CSP) and render blank —
        // so the OS browser is the correct target for an external link followed from a document.
        // (The explicit Open-URL dialog still uses the in-app web preview; that path is unaffected.)
        await openExternal(href);
        return;
      }
      const mapped = mappedLinks.current.get(linkKey(entryPath, activePath, href));
      if (mapped) {
        handleResolvedPath(href, mapped, newTab);
        return;
      }
      const resolved = await grantRelativeFile(activePath, joinRel("", href), entryPath);
      if (resolved) {
        handleResolvedPath(href, resolved, newTab);
        return;
      }
      setBlocked({
        href,
        reason: `${hostAppName()} could not resolve that linked local file inside this site folder.`,
      });
    },
    [activePath, entryPath, handleResolvedPath],
  );

  const goHistory = useCallback(
    (delta: number) => {
      const next = historyIndex + delta;
      if (next < 0 || next >= history.length) return;
      scrollByIndex.current[historyIndex] = scrollYRef.current; // save current page's scroll before leaving
      setHistoryIndex(next);
      setActivePath(history[next]);
      setPendingFragment(null);
      setBlocked(null);
      scrollYRef.current = scrollByIndex.current[next] ?? 0; // restore the target page's remembered scroll
    },
    [history, historyIndex, scrollYRef],
  );

  const locateBlocked = useCallback(async () => {
    if (!blocked) return;
    const path = await openFileDialog();
    if (!path) return;
    handleResolvedPath(blocked.href, path);
    setBlocked(null);
  }, [blocked, handleResolvedPath]);

  const copyBlocked = useCallback(async () => {
    if (!blocked) return;
    await writeClipboardText(blocked.href).catch(() => {
      // Copy is optional for a blocked navigation; keep the dialog open on permission failure.
    });
  }, [blocked]);

  const canGoBack = historyIndex > 0;
  const canGoForward = historyIndex < history.length - 1;

  return {
    activePath,
    pendingFragment,
    blocked,
    setBlocked,
    navigate,
    handleHref,
    goHistory,
    locateBlocked,
    copyBlocked,
    canGoBack,
    canGoForward,
  };
}
