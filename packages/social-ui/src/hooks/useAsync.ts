import { useCallback, useEffect, useRef, useState } from "react";

export type AsyncState<T> =
  | { status: "loading" }
  | { status: "ready"; value: T }
  | { status: "error"; message: string };

/**
 * Runs `load` whenever `deps` change or `reload` is called. Stale responses are dropped so a
 * slow earlier read cannot overwrite a newer one.
 */
export function useAsync<T>(load: () => Promise<T>, deps: readonly unknown[]): {
  state: AsyncState<T>;
  reload: () => void;
  /** Current value, or the last good value while a reload runs. Null before the first success. */
  value: T | null;
} {
  const [state, setState] = useState<AsyncState<T>>({ status: "loading" });
  const [tick, setTick] = useState(0);
  const lastGood = useRef<T | null>(null);
  useEffect(() => {
    if (state.status === "ready") lastGood.current = state.value;
  }, [state]);

  useEffect(() => {
    let live = true;
    setState({ status: "loading" });
    load().then(
      (value) => live && setState({ status: "ready", value }),
      (error: unknown) =>
        live && setState({ status: "error", message: error instanceof Error ? error.message : "Unknown error" }),
    );
    return () => {
      live = false;
    };
    // The caller owns the dependency list; `load` is re-created on every render by design.
  }, [...deps, tick]);

  const reload = useCallback(() => setTick((t) => t + 1), []);
  return { state, reload, value: state.status === "ready" ? state.value : lastGood.current };
}
