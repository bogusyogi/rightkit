import { useEffect, useState } from "react";

/**
 * True once `active` has stayed true for `delayMs`. It is false at once when `active` turns false, and a change of
 * `active` or `delayMs` restarts the wait. Use it to keep a loading surface off screen for fast fetches:
 * `const showLoading = useDeferred(loading, 150)`.
 */
export function useDeferred(active: boolean, delayMs: number): boolean {
  const [ready, setReady] = useState(false);

  useEffect(() => {
    setReady(false);
    if (!active) return;
    if (delayMs <= 0) {
      setReady(true);
      return;
    }
    const timer = setTimeout(() => setReady(true), delayMs);
    return () => clearTimeout(timer);
  }, [active, delayMs]);

  return active && (ready || delayMs <= 0);
}
