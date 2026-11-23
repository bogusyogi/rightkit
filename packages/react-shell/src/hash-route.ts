// Hash routing for a tabbed shell: the page tab follows `location.hash` both ways, and one hash can open an
// overlay (a modal) over a tab. The tab names, hashes and what happens on a change belong to the host.
import { useCallback, useEffect, useRef, useState } from "react";

export interface UseHashRouteOptions<T extends string> {
  /** Hash (with `#`) to tab. A hash not listed here, and the empty hash, show `fallback`. */
  tabs: Readonly<Record<string, T>>;
  fallback: T;
  /** A hash that opens the overlay instead of a tab (the tab behind it is `overlayTab`). */
  overlay?: { hash: string; tab: T };
  /** Hash written when the overlay closes while its own hash is current. Default: the hash of `overlay.tab`'s entry, else `#<overlay.tab>`. */
  closeHash?: string;
  /** Runs after every `hashchange` event, with the hash. */
  onHashChange?: (hash: string) => void;
}

function readTab<T extends string>(options: Pick<UseHashRouteOptions<T>, "tabs" | "fallback" | "overlay">): T {
  const hash = window.location.hash;
  if (options.overlay && hash === options.overlay.hash) return options.overlay.tab;
  return options.tabs[hash] ?? options.fallback;
}

function writeHash(hash: string): void {
  if (window.location.hash !== hash) history.replaceState(null, "", hash);
}

export function useHashRoute<T extends string>(options: UseHashRouteOptions<T>) {
  const optionsRef = useRef(options);
  optionsRef.current = options;
  const [tab, setTabState] = useState<T>(() => readTab(options));
  const [overlayOpen, setOverlayOpen] = useState(
    () => !!options.overlay && window.location.hash === options.overlay.hash,
  );
  const lastFocusRef = useRef<HTMLElement | null>(null);

  /** Shows `next` and writes its hash (`hash`, default `#<next>`) without adding a history entry. */
  const setTab = useCallback((next: T, hash?: string) => {
    setTabState(next);
    writeHash(hash ?? `#${next}`);
  }, []);

  const closeOverlay = useCallback(() => {
    const { overlay, closeHash } = optionsRef.current;
    setOverlayOpen(false);
    if (overlay && window.location.hash === overlay.hash) {
      history.replaceState(null, "", closeHash ?? `#${overlay.tab}`);
    }
    lastFocusRef.current?.focus?.();
  }, []);

  const openOverlay = useCallback(() => {
    const { overlay } = optionsRef.current;
    if (!overlay) return;
    lastFocusRef.current = document.activeElement as HTMLElement | null;
    setTabState(overlay.tab);
    setOverlayOpen(true);
    writeHash(overlay.hash);
  }, []);

  useEffect(() => {
    const onHashChange = () => {
      const current = optionsRef.current;
      setOverlayOpen(!!current.overlay && window.location.hash === current.overlay.hash);
      setTabState(readTab(current));
      current.onHashChange?.(window.location.hash);
    };
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
  }, []);

  return { tab, setTab, overlayOpen, openOverlay, closeOverlay };
}
