// Tiny external string stores shared between the merged title bar and the
// page underneath it: the Commands and History search fields render in the
// title bar while the page below consumes the same query. A module-level
// store (not React context) keeps TitleBar mounted outside the page subtree
// and keeps either side working when the other is unmounted.

import { useSyncExternalStore } from "react";

export interface FieldStore {
  get: () => string;
  set: (next: string) => void;
  subscribe: (listener: () => void) => () => void;
}

function createFieldStore(): FieldStore {
  let value = "";
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

export const commandsSearch = createFieldStore();
export const historySearch = createFieldStore();

/** [value, setValue] — mirrors the useState signature so pages swap in one line. */
export function useField(store: FieldStore): [string, (next: string) => void] {
  const value = useSyncExternalStore(store.subscribe, store.get);
  return [value, store.set];
}
