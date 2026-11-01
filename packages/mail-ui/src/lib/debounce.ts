// Trailing-edge debounce: a burst of calls within `delayMs` of each other
// coalesces into ONE invocation of `fn`, fired `delayMs` after the LAST call.
// Used to stop a burst of per-account sync-complete events from each
// triggering their own refresh (see useAppDataEffects.ts).
export type Debounced = {
  (): void;
  /** Cancel a pending trailing call without firing it. */
  cancel: () => void;
};

export function createTrailingDebounce(fn: () => void, delayMs: number): Debounced {
  // globalThis (not window) so this is unit-testable under plain Node (no DOM)
  // as well as in the browser/webview, where globalThis === window.
  let timerId: ReturnType<typeof globalThis.setTimeout> | null = null;

  const debounced = (() => {
    if (timerId != null) globalThis.clearTimeout(timerId);
    timerId = globalThis.setTimeout(() => {
      timerId = null;
      fn();
    }, delayMs);
  }) as Debounced;

  debounced.cancel = () => {
    if (timerId != null) {
      globalThis.clearTimeout(timerId);
      timerId = null;
    }
  };

  return debounced;
}
