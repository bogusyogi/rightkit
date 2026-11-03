// Hydrated synchronous preference store (core entry: imports only react and the settings types). One document, one
// serial write queue with revision-conflict retry, and a `panelBackend` that serves the settings panel from the same
// document and queue. The key table and the value makers stay in the app; the store only moves named values.
import { useSyncExternalStore } from "react";
import { RevisionConflict } from "./backends.js";
import type { SettingsBackend, SettingsSnapshot, SettingsValues } from "./types.js";

export interface PreferenceStoreOptions {
  /** Persistence port (the shell adapter, or any `SettingsBackend`). */
  backend: SettingsBackend;
  /**
   * Called when a write fails for good, with the error and the patch that failed. The memory value of each key in that
   * patch is rolled back before this runs.
   */
  onError?: (error: unknown, patch: SettingsValues) => void;
}

export interface PreferenceWriteResult {
  /** True when the write is stored (or, for `set` and `patch`, when it was applied to memory and persisted). */
  ok: boolean;
  /** The failure, when `ok` is false. */
  error?: unknown;
}

export interface PreferenceStore {
  /**
   * Loads the stored document into memory once; later calls return the same promise. Resolves true when `get` is valid,
   * false when the load failed (the failure also goes to `onError`).
   */
  boot(): Promise<boolean>;
  /** True once the stored document is in memory. */
  readonly ready: boolean;
  /** Synchronous read from memory (safe at render). `undefined` before `boot` settles or when the key is unset. */
  get<T = unknown>(key: string): T | undefined;
  /** The current document and revision, or null before `boot` settles. */
  snapshot(): SettingsSnapshot | null;
  /**
   * Synchronous write of one key: memory updates now, the write joins the queue. `null` or `undefined` removes the key.
   * The promise resolves with the outcome; it never rejects, so a fire-and-forget call cannot leak an unhandled rejection.
   */
  set(key: string, value: unknown): Promise<PreferenceWriteResult>;
  /** Merge several keys as one write. Same rules as `set`. */
  patch(patch: SettingsValues): Promise<PreferenceWriteResult>;
  /** Notified after every change to memory (writes, loads, rollbacks). Returns an unsubscribe. Stable across renders. */
  subscribe(listener: () => void): () => void;
  /**
   * The settings panel backend over this same document and queue: pass it as `SettingsPanel`'s `backend`. Its `patch`
   * ignores `expectedRevision`: the store is the only writer, so it checks the revision itself and retries a conflict once.
   */
  readonly panelBackend: SettingsBackend;
  /** Stops the external-change subscription and listeners. Later writes resolve `ok: false`. */
  dispose(): void;
}

const isConflict = (e: unknown) => e instanceof RevisionConflict || (e as { code?: unknown } | null)?.code === "revision_conflict";

function applyPatch(current: SettingsValues, patch: SettingsValues): SettingsValues {
  const next = { ...current };
  for (const [k, v] of Object.entries(patch)) {
    if (v === null || v === undefined) delete next[k];
    else next[k] = v;
  }
  return next;
}

/**
 * Creates a preference store. Writes update memory at once and are serialised through one queue, so revisions never race.
 * A stale revision (another window wrote first) reloads the document and retries the key-wise patch once. A write that
 * still fails rolls its keys back in memory and reports through `onError`. An external change reloads the document when no
 * write is pending; a load that settles while a write is pending is repeated once the queue drains.
 */
export function createPreferenceStore(options: PreferenceStoreOptions): PreferenceStore {
  const { backend, onError } = options;
  let values: SettingsValues = {};
  let revision: number | undefined;
  let ready = false;
  let disposed = false;
  let bootPromise: Promise<boolean> | null = null;
  let pending = 0;
  let loadSeq = 0;
  let reloadSkipped = false;
  let chain: Promise<unknown> = Promise.resolve();
  let unsubscribe: (() => void) | undefined;
  const listeners = new Set<() => void>();
  const emit = () => {
    for (const l of [...listeners]) l();
  };

  const load = (): Promise<void> => {
    const seq = ++loadSeq;
    return backend.load().then(
      (snap) => {
        if (disposed || seq !== loadSeq) return;
        if (pending > 0) {
          // Writes are still queued and their optimistic values are in memory: repeat the load after the queue drains.
          reloadSkipped = true;
          ready = true;
          emit();
          return;
        }
        revision = snap.revision;
        values = snap.values;
        ready = true;
        emit();
      },
      (e) => {
        if (!disposed) onError?.(e, {});
      },
    );
  };

  const boot = (): Promise<boolean> => {
    if (bootPromise === null) {
      bootPromise = load().then(() => ready);
      void backend.subscribe?.(() => {
        if (!disposed && pending === 0) void load();
      }).then(
        (u) => {
          if (disposed) u();
          else unsubscribe = u;
        },
        () => {},
      );
    }
    return bootPromise;
  };

  const writeRaw = (p: SettingsValues): Promise<PreferenceWriteResult> => {
    if (disposed) return Promise.resolve({ ok: false, error: new Error("preference store is disposed") });
    const prior = Object.keys(p).map((k) => [k, k in values, values[k]] as const);
    values = applyPatch(values, p);
    pending += 1;
    emit();
    const run = chain.then(async (): Promise<PreferenceWriteResult> => {
      try {
        let snap: SettingsSnapshot;
        try {
          snap = await backend.patch(p, revision);
        } catch (e) {
          if (!isConflict(e)) throw e;
          // Another window wrote first: the patch is key-wise, so retry once on the fresh revision.
          const fresh = await backend.load();
          snap = await backend.patch(p, fresh.revision);
        }
        revision = snap.revision;
        // Any load still in flight predates this write: invalidate it so it cannot restore the old revision or values.
        loadSeq += 1;
        // Adopt the host document only when no other write is queued behind this one (their optimistic values are not in it).
        if (pending === 1) values = snap.values;
        return { ok: true };
      } catch (error) {
        const next = { ...values };
        for (const [k, had, v] of prior) {
          if (had) next[k] = v;
          else delete next[k];
        }
        values = next;
        onError?.(error, p);
        return { ok: false, error };
      } finally {
        pending -= 1;
        emit();
        if (pending === 0 && reloadSkipped && !disposed) {
          reloadSkipped = false;
          void load();
        }
      }
    });
    chain = run;
    return run;
  };

  const store: PreferenceStore = {
    boot,
    get ready() {
      return ready;
    },
    get<T = unknown>(key: string): T | undefined {
      return values[key] as T | undefined;
    },
    snapshot(): SettingsSnapshot | null {
      return ready && revision !== undefined ? { values, revision } : null;
    },
    set(key, value) {
      return writeRaw({ [key]: value });
    },
    patch(p) {
      return writeRaw(p);
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    panelBackend: {
      async load() {
        if (!ready && !(await boot())) throw new Error("Settings could not be loaded.");
        return { values, revision: revision ?? 0 };
      },
      async patch(p) {
        const r = await writeRaw(p);
        if (!r.ok) throw r.error;
        return { values, revision: revision ?? 0 };
      },
      async subscribe(handler) {
        // Panel subscribers see the same external-change keys through the store's own listener.
        let lastValues = values;
        return store.subscribe(() => {
          if (values === lastValues) return;
          const changed = [...new Set([...Object.keys(lastValues), ...Object.keys(values)])].filter((k) => lastValues[k] !== values[k]);
          lastValues = values;
          if (changed.length) handler(changed);
        });
      },
    },
    dispose() {
      disposed = true;
      unsubscribe?.();
      listeners.clear();
    },
  };
  return store;
}

/** Subscribes a component to one key of a preference store. Re-renders only when the store changes. */
export function usePreference<T = unknown>(store: PreferenceStore, key: string): T | undefined {
  return useSyncExternalStore(
    store.subscribe,
    () => store.get<T>(key),
    () => store.get<T>(key),
  );
}
