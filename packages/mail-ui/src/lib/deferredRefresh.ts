// L13 — writer-busy refresh skip. Pure decision logic extracted out of
// `useAppDataEffects`'s debounced sync-complete refresh so it's unit
// testable without mounting React: given a `writerBusy` probe and a
// `deferredOnce` flag, decide whether to defer (reschedule once) or run the
// refresh now. Capped at ONE deferred cycle so a persistently busy writer
// can never starve the refresh forever — mirrors the backend's
// never-loop reader-reopen contract (L13 spec item 3).

export type DeferredRefreshDecision =
  | { action: "defer"; deferredOnce: true }
  | { action: "run"; deferredOnce: false };

/**
 * Decide whether the debounced sync-complete refresh should run now or
 * defer one more debounce cycle.
 *
 * - `busy` false -> always run, and clear any prior deferral.
 * - `busy` true and this is the FIRST deferral (`deferredOnce` false) ->
 *   defer once.
 * - `busy` true but a deferral already happened (`deferredOnce` true) ->
 *   run anyway. Never defers a second consecutive time.
 */
export function decideDeferredRefresh(
  busy: boolean,
  deferredOnce: boolean,
): DeferredRefreshDecision {
  if (busy && !deferredOnce) {
    return { action: "defer", deferredOnce: true };
  }
  return { action: "run", deferredOnce: false };
}
