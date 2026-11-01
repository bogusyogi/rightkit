import { useEffect } from "react";

import { focusConnectedOrPageDestination } from "../lib/settingsRegistry.js";

// The shell's command palette unmounts without returning focus, so after
// Escape/outside-click (or a palette navigation) focus drops to <body>.
// Remember the last element focused outside the palette; when the palette
// leaves the DOM and nothing holds focus, send focus back to it, else to the
// current page heading (#50).
export function usePaletteFocusRestore(): void {
  useEffect(() => {
    let opener: HTMLElement | null = null;
    const onFocusIn = (event: FocusEvent) => {
      const target = event.target;
      if (target instanceof HTMLElement && !target.closest(".rk-palette")) opener = target;
    };
    document.addEventListener("focusin", onFocusIn);
    let paletteOpen = false;
    const observer = new MutationObserver(() => {
      const open = document.querySelector(".rk-palette") !== null;
      if (paletteOpen && !open) {
        const saved = opener;
        window.setTimeout(() => {
          const active = document.activeElement;
          if (!active || active === document.body) focusConnectedOrPageDestination(saved);
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
