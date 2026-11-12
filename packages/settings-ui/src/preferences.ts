// Hydrated synchronous preference store (core entry: imports only react and the settings types). One document, one
// serial write queue with revision-conflict retry, and a `panelBackend` that serves the settings panel from the same
// document and queue. The key table and the value makers stay in the app; the store only moves named values.
import { useSyncExternalStore } from "react";
import { createSettingsStore } from "./store.js";
import type { SettingsBackend, SettingsRevision, SettingsSnapshot, SettingsValues } from "./types.js";

export interface PreferenceStoreOptions<R extends SettingsRevision = number> {
  /** Persistence port (the shell adapter, or any `SettingsBackend`). */
  backend: SettingsBackend<R>;
  /**
   * Called when a write fails for good, with the error and the patch that failed. Each key of that patch that still held
   * the write's value is rolled back in memory before this runs. Also called with an empty patch when a load fails.
   */
  onError?: (error: unknown, patch: SettingsValues) => void;
}

export interface PreferenceWriteResult {
  /** True when the write is stored (or, for `set` and `patch`, when it was applied to memory and persisted). */
  ok: boolean;
  /** The failure, when `ok` is false. */
  error?: unknown;
}

export interface PreferenceStore<R extends SettingsRevision = number> {
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
  snapshot(): SettingsSnapshot<R> | null;
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
  readonly panelBackend: SettingsBackend<R>;
  /** Stops the external-change subscription and listeners. Later writes resolve `ok: false`. */
  dispose(): void;
}

/**
 * Creates a preference store. Writes update memory at once and are serialised through one queue, so revisions never race.
 * A stale revision (another window wrote first) reloads the document and retries the key-wise patch once. A write that
 * still fails rolls back in memory each of its keys that still holds its value, reports through `onError`, and the
 * document is read again once the queue drains. An external change reloads the document; while a write is pending the
 * reload waits for the queue to drain.
 */
export function createPreferenceStore<R extends SettingsRevision = number>(options: PreferenceStoreOptions<R>): PreferenceStore<R> {
  const { backend, onError } = options;
  let disposed = false;
  let bootPromise: Promise<boolean> | null = null;
  let release: (() => void) | undefined;
  const core = createSettingsStore(backend, { onLoadError: (e) => onError?.(e, {}) });
  const values = () => core.getState().values;
  // Before the first document arrives there is no revision to report. The panel only echoes it, and `patch` ignores it.
  const current = (): SettingsSnapshot<R> => ({ values: values(), revision: core.revision() ?? (0 as R) });

  const boot = (): Promise<boolean> => {
    if (bootPromise === null) {
      bootPromise = core.load().then(() => core.getState().ready);
      release = core.retain();
    }
    return bootPromise;
  };

  const writeRaw = async (p: SettingsValues): Promise<PreferenceWriteResult> => {
    if (disposed) return { ok: false, error: new Error("preference store is disposed") };
    const result = await core.write(p);
    if (!result.ok) onError?.(result.error, p);
    return result;
  };

  const store: PreferenceStore<R> = {
    boot,
    get ready() {
      return core.getState().ready;
    },
    get<T = unknown>(key: string): T | undefined {
      return values()[key] as T | undefined;
    },
    snapshot(): SettingsSnapshot<R> | null {
      const revision = core.revision();
      return core.getState().ready && revision !== undefined ? { values: values(), revision } : null;
    },
    set(key, value) {
      return writeRaw({ [key]: value });
    },
    patch(p) {
      return writeRaw(p);
    },
    subscribe: core.subscribe,
    panelBackend: {
      async load() {
        if (!core.getState().ready && !(await boot())) throw new Error("Settings could not be loaded.");
        return current();
      },
      async patch(p) {
        const r = await writeRaw(p);
        if (!r.ok) throw r.error;
        return current();
      },
      async subscribe(handler) {
        // Panel subscribers see the same external-change keys through the store's own listener.
        let lastValues = values();
        return core.subscribe(() => {
          const now = values();
          if (now === lastValues) return;
          const changed = [...new Set([...Object.keys(lastValues), ...Object.keys(now)])].filter((k) => lastValues[k] !== now[k]);
          lastValues = now;
          if (changed.length) handler(changed);
        });
      },
    },
    dispose() {
      disposed = true;
      release?.();
      core.close();
    },
  };
  return store;
}

/** Subscribes a component to one key of a preference store. Re-renders only when the store changes. */
export function usePreference<T = unknown>(store: PreferenceStore<SettingsRevision>, key: string): T | undefined {
  return useSyncExternalStore(
    store.subscribe,
    () => store.get<T>(key),
    () => store.get<T>(key),
  );
}
