import { useCallback, useEffect, useRef, useState } from "react";
import type { ThemeStorage } from "../theme.js";

/* Focus and dismiss hooks live in @rightkit/platform-ui/headless (no app-shell peer needed). */
export { useFocusTrap, useLatest, useModalDismiss } from "@rightkit/platform-ui/headless";

/** Media-query subscription (narrow layout, reduced motion). */
export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() => (typeof window !== "undefined" && window.matchMedia ? window.matchMedia(query).matches : false));
  useEffect(() => {
    if (typeof window === "undefined" || !window.matchMedia) return;
    const mq = window.matchMedia(query);
    const on = () => setMatches(mq.matches);
    on();
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, [query]);
  return matches;
}

export interface PersistedStateOptions<T> {
  /** Storage; default `window.localStorage` when available. `null` keeps the value in memory only. */
  storage?: ThemeStorage | null;
  /** Parse the stored string. Default `JSON.parse`. A throw falls back to the initial value. */
  parse?: (raw: string) => T;
  /** Serialise for storage. Default `JSON.stringify`. */
  serialize?: (value: T) => string;
}

function defaultPersistStorage(): ThemeStorage | null {
  try {
    return typeof window === "undefined" ? null : window.localStorage;
  } catch {
    return null;
  }
}

/**
 * `useState` that survives a reload. `key` null keeps the value in memory. Reads and writes never throw: unavailable or
 * corrupt storage yields the initial value and the write is dropped. The stored value is read once at mount, so change
 * `key` only together with a remount.
 */
export function usePersistedState<T>(key: string | null, initial: T, options: PersistedStateOptions<T> = {}): [T, (next: T | ((prev: T) => T)) => void] {
  // The setter is memoised on key only, so it reads the current storage and serializer through this ref at call time.
  const optionsRef = useRef(options);
  optionsRef.current = options;
  const storage = () => (optionsRef.current.storage !== undefined ? optionsRef.current.storage : defaultPersistStorage());
  const [value, setValue] = useState<T>(() => {
    if (key === null) return initial;
    try {
      const raw = storage()?.getItem(key);
      if (raw === null || raw === undefined) return initial;
      const { parse } = optionsRef.current;
      return parse ? parse(raw) : (JSON.parse(raw) as T);
    } catch {
      return initial;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  });
  const latest = useRef(value);
  const set = useCallback(
    (next: T | ((prev: T) => T)) => {
      const resolved = typeof next === "function" ? (next as (prev: T) => T)(latest.current) : next;
      latest.current = resolved;
      if (key !== null) {
        try {
          const { serialize } = optionsRef.current;
          storage()?.setItem(key, serialize ? serialize(resolved) : JSON.stringify(resolved));
        } catch {
          /* storage unavailable or full */
        }
      }
      setValue(resolved);
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [key],
  );
  return [value, set];
}
