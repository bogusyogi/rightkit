import { useEffect } from "react";

interface HtmlSubPagesDeps {
  /** The originally-opened file — the trusted site root; never changes as you navigate. */
  entryPath: string;
  /** The in-site page currently shown (entry or a navigated sub-page). */
  activePath: string;
  /** `activePath === entryPath` — entry pages never need a seeded sub-page buffer. */
  onEntry: boolean;
  /** Seed a sub-page buffer from disk on first visit (no-op for the entry / already-loaded page). */
  onEnsurePage: (entryPath: string, pagePath: string) => void;
  /** Report the in-site page currently shown (App needs it for Ctrl+S targeting). */
  onSetActivePage: (entryPath: string, pagePath: string) => void;
}

/**
 * Sub-page session buffers: seed the navigated page's buffer from disk on first visit and report
 * the active page up to App. Extracted from HtmlDocument verbatim — no behavior change.
 */
export function useHtmlSubPages({
  entryPath,
  activePath,
  onEntry,
  onEnsurePage,
  onSetActivePage,
}: HtmlSubPagesDeps) {
  // Seed the sub-page buffer on navigation, and report the active page up to App.
  useEffect(() => {
    if (!onEntry) onEnsurePage(entryPath, activePath);
    onSetActivePage(entryPath, activePath);
  }, [activePath, onEntry, entryPath, onEnsurePage, onSetActivePage]);
}
