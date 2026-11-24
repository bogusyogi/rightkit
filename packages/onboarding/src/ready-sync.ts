import { type MutableRefObject, useEffect } from "react";

/**
 * Keeps a step's readiness in sync: calls `setReady(ready)` whenever it changes (the step context's `setReady`) and,
 * when `ref` is given, mirrors it into the ref for code outside the step (e.g. a Continue veto).
 */
export function useStepReady(setReady: (ready: boolean) => void, ready: boolean, ref?: MutableRefObject<boolean>): void {
  useEffect(() => {
    if (ref) ref.current = ready;
    setReady(ready);
  }, [setReady, ready, ref]);
}
