import { useEffect, useRef } from "react";
import type { ThemeChoice, ThemeStore } from "./theme.js";

/** Optional side effects around a forced theme, for an app that paints more than `data-theme` (a brand palette, a native chrome). */
export interface ForcedThemeHooks {
  /** After `choice` is applied: on mount and again after every change of the store's saved choice. */
  onForce?: () => void;
  /** After the store's own choice is applied again, on unmount. */
  onRestore?: () => void;
}

/**
 * Applies `choice` while the component is mounted, then puts the store's own choice back on unmount (a first-run
 * flow drawn for one theme only). The saved choice is never changed: only what is applied. A save while mounted
 * (the app saving its stored theme after this mounts) is re-forced on the next microtask, after the store's own
 * subscribers have run. `hooks` are read at call time; changing them does not re-run the effect. Imports only `react`.
 */
export function useForcedTheme(store: ThemeStore, choice: ThemeChoice, hooks?: ForcedThemeHooks): void {
  const hooksRef = useRef(hooks);
  hooksRef.current = hooks;
  useEffect(() => {
    const force = () => {
      store.apply(choice);
      hooksRef.current?.onForce?.();
    };
    force();
    const off = store.subscribe(() => queueMicrotask(force));
    return () => {
      off();
      store.apply(store.read());
      hooksRef.current?.onRestore?.();
    };
  }, [store, choice]);
}
