import { useCallback, useEffect, useRef, useState } from "react";
import type { SettingsBackend, SettingsContext, SettingsSnapshot, SettingsValues } from "./types.js";

const message = (e: unknown) => (e instanceof Error ? e.message : typeof e === "string" ? e : "Could not save the setting.");

export interface UseSettingsOptions {
  /**
   * A snapshot the caller already holds, such as `createMemorySettings(...).snapshot()`. The first render is then loaded,
   * so "Loading settings…" is never shown. The background `load()` still runs and refreshes it.
   */
  initial?: SettingsSnapshot;
}

/** Everything one backend owns. Async work captures its generation and mutates state only while it is current. */
interface Generation {
  backend: SettingsBackend;
  revision: number | undefined;
  chain: Promise<unknown>;
  pending: number;
  // A load that finished while a write was pending is not applied (it would overwrite the optimistic value with
  // the state from before the write); it is repeated once the writes have drained.
  reloadSkipped: boolean;
  // Sequence of the newest load started on this generation; an older load that settles later is discarded.
  loadSeq: number;
  reload: () => Promise<unknown>;
}

function newGeneration(backend: SettingsBackend, revision: number | undefined): Generation {
  return { backend, revision, chain: Promise.resolve(), pending: 0, reloadSkipped: false, loadSeq: 0, reload: () => Promise.resolve() };
}

/**
 * Settings state over a backend: loads once, applies writes optimistically, serialises them (so revisions never race),
 * rolls back and reports on failure, and reloads when another window changes keys. Switching `backend` starts a new
 * generation: work from the old one settles its own promise but never mutates the new backend's state, and queued
 * stale writes never start.
 */
export function useSettings(backend: SettingsBackend, options: UseSettingsOptions = {}): SettingsContext & { loaded: boolean } {
  const initial = options.initial;
  const [values, setValues] = useState<SettingsValues>(() => initial?.values ?? {});
  const [loaded, setLoaded] = useState(() => initial !== undefined);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const genRef = useRef<Generation | null>(null);
  if (genRef.current === null) genRef.current = newGeneration(backend, initial?.revision);
  else if (genRef.current.backend !== backend) genRef.current = newGeneration(backend, undefined);
  const gen = genRef.current;
  const alive = useRef(true);
  const isCurrent = (g: Generation) => alive.current && genRef.current === g;

  useEffect(() => {
    alive.current = true;
    const g = gen;
    // Per-run flag: `alive` is shared, so a StrictMode/backend re-run flips it back to true before the first run's
    // subscribe promise settles, which would keep (and leak) the first subscription. Every load, load failure and
    // subscription callback of this run checks it, and a load also must still be the newest one of its generation,
    // so a cancelled or superseded load (or its error) can never overwrite a newer result.
    let cancelled = false;
    let off: (() => void) | undefined;
    const reload = () => {
      const seq = ++g.loadSeq;
      const live = () => !cancelled && seq === g.loadSeq && isCurrent(g);
      return g.backend.load().then(
        (snap) => {
          if (!live()) return;
          if (g.pending > 0) {
            g.reloadSkipped = true;
            setLoaded(true);
            return;
          }
          g.revision = snap.revision;
          setValues(snap.values);
          setLoaded(true);
        },
        (e) => {
          if (live()) {
            setError(message(e));
            setLoaded(true);
          }
        },
      );
    };
    g.reload = reload;
    setSaving(g.pending > 0);
    void reload();
    void g.backend.subscribe?.(() => {
      if (!cancelled && isCurrent(g) && g.pending === 0) void reload();
    }).then((u) => {
      if (cancelled) u();
      else off = u;
    });
    return () => {
      cancelled = true;
      alive.current = false;
      off?.();
    };
  }, [backend]);

  const patch = useCallback(
    (p: SettingsValues): Promise<void> => {
      const g = gen;
      // A stale closure (held from before a backend switch) settles without touching the new backend or its state.
      if (genRef.current !== g) return Promise.resolve();
      let before: SettingsValues = {};
      setValues((cur) => {
        before = cur;
        const next = { ...cur };
        for (const [k, v] of Object.entries(p)) {
          if (v === null) delete next[k];
          else next[k] = v;
        }
        return next;
      });
      g.pending += 1;
      setSaving(true);
      setError(null);
      const run = g.chain.then(async () => {
        try {
          // Queued before a switch: never start against any backend now.
          if (genRef.current !== g) return;
          let snap;
          try {
            snap = await g.backend.patch(p, g.revision);
          } catch (e) {
            // A stale revision only means another window wrote first; our patch is key-wise, so retry once on the fresh one.
            if ((e as { code?: string }).code !== "revision_conflict") throw e;
            if (genRef.current !== g) return;
            const fresh = await g.backend.load();
            if (genRef.current !== g) return;
            snap = await g.backend.patch(p, fresh.revision);
          }
          if (!isCurrent(g)) return;
          g.revision = snap.revision;
          setValues(snap.values);
          // The write's snapshot is newer than any load still in flight: invalidate those, or a slow load that
          // settles after the write drains (pending is 0 again) would restore the pre-write values and revision.
          // Not done at patch start or on failure: a load that settles while a write is pending (or after a failed
          // one) is handled by the reloadSkipped path / holds the unchanged stored state.
          g.loadSeq += 1;
          setLoaded(true);
        } catch (e) {
          if (!isCurrent(g)) return;
          setValues((cur) => {
            const next = { ...cur };
            for (const k of Object.keys(p)) {
              if (k in before) next[k] = before[k];
              else delete next[k];
            }
            return next;
          });
          setError(message(e));
        } finally {
          g.pending -= 1;
          if (isCurrent(g) && g.pending === 0) {
            setSaving(false);
            if (g.reloadSkipped) {
              g.reloadSkipped = false;
              void g.reload();
            }
          }
        }
      });
      g.chain = run;
      return run;
    },
    [backend],
  );

  const set = useCallback((key: string, value: unknown) => patch({ [key]: value }), [patch]);
  const clearError = useCallback(() => setError(null), []);
  return { values, set, patch, saving, error, clearError, loaded };
}
