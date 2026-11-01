import type { StoreApi } from "zustand";
import type { AppState } from "./types";

// Store set/get types shared by the field builder and the action slices.
export type StoreSet = StoreApi<AppState>["setState"];
export type StoreGet = StoreApi<AppState>["getState"];

// Build a useState-compatible setter for a field: accepts a value OR an updater
// `(prev) => next`. (No store field holds a function value, so the
// `typeof === "function"` check unambiguously detects the updater form — this is
// exactly how React's own setState distinguishes the two.)
export function makeMk(set: StoreSet) {
  return <K extends keyof AppState>(k: K) =>
    (v: unknown) =>
      set(
        (s) =>
          ({
            [k]:
              typeof v === "function"
                ? (v as (p: unknown) => unknown)(
                    (s as unknown as Record<string, unknown>)[k as string]
                  )
                : v,
          }) as Partial<AppState>
      );
}
