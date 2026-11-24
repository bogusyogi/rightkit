// Focus restore for a command palette that unmounts without returning focus. Imports `react` and `focus-page`.
import { useEffect, useRef } from "react";
import { focusConnectedOrPageDestination, type FocusPageOptions } from "./focus-page.js";

/** Options of {@link usePaletteFocusRestore}. */
export interface UsePaletteFocusRestoreOptions {
  /** Selector of the palette root. Default `.rk-palette` (the shell's palette). */
  paletteSelector?: string;
  /**
   * Called with the element that last held focus outside the palette, when the palette has left the DOM and
   * nothing holds focus. Default: {@link focusConnectedOrPageDestination} (that element if still usable, else the
   * current page's heading, using `focusPage`).
   */
  restore?: (opener: HTMLElement | null) => void;
  /** Options for the default restore: the page attribute and heading selector. Ignored when `restore` is given. */
  focusPage?: FocusPageOptions;
}

/**
 * Remembers the last element focused outside the palette; when the palette leaves the DOM (Escape, outside click, a
 * palette navigation) and focus has dropped to the body, restores it on the next task. A host that already moved
 * focus is left alone. Mount once, high in the tree.
 */
export function usePaletteFocusRestore(options: UsePaletteFocusRestoreOptions = {}): void {
  const optionsRef = useRef(options);
  optionsRef.current = options;
  useEffect(() => {
    const selector = () => optionsRef.current.paletteSelector ?? ".rk-palette";
    let opener: HTMLElement | null = null;
    const onFocusIn = (event: FocusEvent) => {
      const target = event.target;
      if (target instanceof HTMLElement && !target.closest(selector())) opener = target;
    };
    document.addEventListener("focusin", onFocusIn);
    let paletteOpen = false;
    const observer = new MutationObserver(() => {
      const open = document.querySelector(selector()) !== null;
      if (paletteOpen && !open) {
        const saved = opener;
        window.setTimeout(() => {
          const active = document.activeElement;
          if (active && active !== document.body) return;
          const { restore, focusPage } = optionsRef.current;
          if (restore) restore(saved);
          else focusConnectedOrPageDestination(saved, focusPage);
        }, 0);
      }
      paletteOpen = open;
    });
    observer.observe(document.body, { childList: true, subtree: true });
    return () => {
      document.removeEventListener("focusin", onFocusIn);
      observer.disconnect();
    };
  }, []);
}
