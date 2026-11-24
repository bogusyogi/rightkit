// A tiny external string store, so two components that are not parent and child (a title-bar search field and the
// page under it) share one value. A module-level store, not React context: the field can stay mounted outside the
// page subtree, and either side keeps working while the other is unmounted. Imports only `react`.
import { useSyncExternalStore } from "react";

/** A string value other components can read, set and subscribe to. */
export interface FieldStore {
  /** The current value. */
  get: () => string;
  /** Sets the value. Listeners run only when it differs from the current one. */
  set: (next: string) => void;
  /** Adds a listener, called after each change; returns the function that removes it. */
  subscribe: (listener: () => void) => () => void;
}

/**
 * Creates a {@link FieldStore}. The host creates one per shared field at module level (for example a search
 * query) and passes it to {@link useField}.
 * @param initial Starting value. Default: the empty string.
 */
export function createFieldStore(initial = ""): FieldStore {
  let value = initial;
  const listeners = new Set<() => void>();
  return {
    get: () => value,
    set: (next: string) => {
      if (next === value) return;
      value = next;
      for (const listener of listeners) listener();
    },
    subscribe: (listener: () => void) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}

/** `[value, setValue]` for a {@link FieldStore}; mirrors the `useState` signature so a page swaps it in one line. */
export function useField(store: FieldStore): [string, (next: string) => void] {
  const value = useSyncExternalStore(store.subscribe, store.get);
  return [value, store.set];
}
