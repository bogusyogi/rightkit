import { useCallback, useEffect, useRef, useState } from "react";
import type { SettingsBackend, SettingsContext, SettingsValues } from "./types.js";

const message = (e: unknown) => (e instanceof Error ? e.message : typeof e === "string" ? e : "Could not save the setting.");

/**
 * Settings state over a backend: loads once, applies writes optimistically, serialises them (so revisions never race),
 * rolls back and reports on failure, and reloads when another window changes keys.
 */
export function useSettings(backend: SettingsBackend): SettingsContext & { loaded: boolean } {
  const [values, setValues] = useState<SettingsValues>({});
  const [loaded, setLoaded] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const revision = useRef<number | undefined>(undefined);
  const chain = useRef<Promise<unknown>>(Promise.resolve());
  const pending = useRef(0);
  const alive = useRef(true);

  useEffect(() => {
    alive.current = true;
    // Per-run flag: `alive` is shared, so a StrictMode/backend re-run flips it back to true before the first run's
    // subscribe promise settles, which would keep (and leak) the first subscription.
    let cancelled = false;
    let off: (() => void) | undefined;
    const reload = () =>
      backend.load().then(
        (snap) => {
          if (!alive.current) return;
          revision.current = snap.revision;
          setValues(snap.values);
          setLoaded(true);
        },
        (e) => alive.current && (setError(message(e)), setLoaded(true)),
      );
    void reload();
    void backend.subscribe?.(() => {
      if (pending.current === 0) void reload();
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
      pending.current += 1;
      setSaving(true);
      setError(null);
      const run = chain.current.then(async () => {
        try {
          let snap;
          try {
            snap = await backend.patch(p, revision.current);
          } catch (e) {
            // A stale revision only means another window wrote first; our patch is key-wise, so retry once on the fresh one.
            if ((e as { code?: string }).code !== "revision_conflict") throw e;
            const fresh = await backend.load();
            snap = await backend.patch(p, fresh.revision);
          }
          revision.current = snap.revision;
          if (alive.current) setValues(snap.values);
        } catch (e) {
          if (alive.current) {
            setValues((cur) => {
              const next = { ...cur };
              for (const k of Object.keys(p)) {
                if (k in before) next[k] = before[k];
                else delete next[k];
              }
              return next;
            });
            setError(message(e));
          }
        } finally {
          pending.current -= 1;
          if (alive.current && pending.current === 0) setSaving(false);
        }
      });
      chain.current = run;
      return run;
    },
    [backend],
  );

  const set = useCallback((key: string, value: unknown) => patch({ [key]: value }), [patch]);
  const clearError = useCallback(() => setError(null), []);
  return { values, set, patch, saving, error, clearError, loaded };
}
