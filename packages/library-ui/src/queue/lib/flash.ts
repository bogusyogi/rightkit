import { useCallback, useEffect, useRef, useState } from "react";

export interface FlashController {
  /** Turn the flash on and (re)start its timer; a re-trigger extends the window. */
  trigger(): void;
  /** Turn the flash off now and cancel any pending timer. */
  reset(): void;
  /** Cancel any pending timer without touching state (unmount). */
  dispose(): void;
}

export interface FlashTimers {
  setTimeout(fn: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

const browserTimers: FlashTimers = {
  setTimeout: (fn, ms) => globalThis.setTimeout(fn, ms),
  clearTimeout: (handle) => globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>),
};

export function createFlashController(
  durationMs: number,
  setOn: (on: boolean) => void,
  timers: FlashTimers = browserTimers,
): FlashController {
  let handle: unknown = null;
  const clear = () => {
    if (handle !== null) timers.clearTimeout(handle);
    handle = null;
  };
  return {
    trigger() {
      clear();
      setOn(true);
      handle = timers.setTimeout(() => {
        handle = null;
        setOn(false);
      }, durationMs);
    },
    reset() {
      clear();
      setOn(false);
    },
    dispose() {
      clear();
    },
  };
}

/** Copy-confirmation flash: `[on, trigger, reset]`. Timer is cleared on unmount. */
export function useFlash(durationMs: number): [boolean, () => void, () => void] {
  const [on, setOn] = useState(false);
  const ref = useRef<FlashController | null>(null);
  if (ref.current === null) ref.current = createFlashController(durationMs, setOn);
  useEffect(() => () => ref.current?.dispose(), []);
  const trigger = useCallback(() => ref.current?.trigger(), []);
  const reset = useCallback(() => ref.current?.reset(), []);
  return [on, trigger, reset];
}
