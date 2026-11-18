import { useMemo, useState, type RefCallback, type RefObject } from "react";

export type PillElementRef<T> = RefCallback<T> & RefObject<T | null>;

/** Callback ref tracks actual host attachment; current also supports imperative commands. */
export function usePillElementRef<T>() {
  const [element, setElement] = useState<T | null>(null);
  const ref = useMemo(() => {
    const callback = ((node: T | null) => {
      callback.current = node;
      setElement(node);
    }) as PillElementRef<T>;
    callback.current = null;
    return callback;
  }, []);
  return [ref, element] as const;
}
