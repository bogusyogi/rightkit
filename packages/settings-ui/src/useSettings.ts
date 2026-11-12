import { useCallback, useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { sharedSettingsStore, type SettingsStoreState } from "./store.js";
import type { SettingsBackend, SettingsContext, SettingsRevision, SettingsSnapshot, SettingsValues } from "./types.js";

const message = (e: unknown) => (e instanceof Error ? e.message : typeof e === "string" ? e : "Could not save the setting.");

export interface UseSettingsOptions<R extends SettingsRevision = number> {
  /**
   * A snapshot the caller already holds, such as `createMemorySettings(...).snapshot()`. The first render is then loaded,
   * so "Loading settings…" is never shown. The background `load()` still runs and refreshes it.
   */
  initial?: SettingsSnapshot<R>;
}

/** What one consumer keeps to itself: the failure of its own write, and the load failure it has dismissed. */
interface OwnError {
  store: object;
  write: string | null;
  dismissed: SettingsStoreState["loadError"];
}

/**
 * Settings state over a backend. Every `useSettings` of the same backend object in a window reads one store: one load,
 * one revision, one serial write queue (so revisions never race) and one backend subscription, and a write by one
 * consumer shows in all of them at once. Writes are optimistic; a failed write is rolled back, the document is read
 * again, and `error` is set for the consumer that wrote. A failed load sets `error` for every consumer. Another window's
 * change reloads the document. Switching `backend` moves the consumer to that backend's store; writes already queued on
 * the old backend still complete there.
 */
export function useSettings<R extends SettingsRevision = number>(
  backend: SettingsBackend<R>,
  options: UseSettingsOptions<R> = {},
): SettingsContext & { loaded: boolean } {
  const store = sharedSettingsStore(backend);
  if (options.initial) store.seed(options.initial);
  const state = useSyncExternalStore(store.subscribe, store.getState, store.getState);
  const [kept, setOwn] = useState<OwnError>({ store, write: null, dismissed: null });
  const own = kept.store === store ? kept : { store, write: null, dismissed: null };

  useEffect(() => {
    const release = store.retain();
    // Joins the load another consumer of this backend already started.
    void store.refresh();
    return release;
  }, [store]);

  const patch = useCallback(
    (p: SettingsValues): Promise<void> => {
      setOwn({ store, write: null, dismissed: store.getState().loadError });
      return store.write(p).then((result) => {
        if (!result.ok) setOwn((cur) => ({ store, write: message(result.error), dismissed: cur.store === store ? cur.dismissed : null }));
      });
    },
    [store],
  );
  const set = useCallback((key: string, value: unknown) => patch({ [key]: value }), [patch]);
  const clearError = useCallback(() => setOwn({ store, write: null, dismissed: store.getState().loadError }), [store]);

  const error = own.write ?? (state.loadError && state.loadError !== own.dismissed ? message(state.loadError.error) : null);
  return useMemo(
    () => ({ values: state.values, set, patch, saving: state.saving, error, clearError, loaded: state.loaded }),
    [state.values, state.saving, state.loaded, error, set, patch, clearError],
  );
}
