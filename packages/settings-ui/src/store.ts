// The settings store behind `useSettings` and `createPreferenceStore` (core entry: imports only the settings types).
// One document in memory, one revision, one serial write queue with revision-conflict retry and one backend
// subscription, however many consumers read it.
import type { SettingsBackend, SettingsRevision, SettingsSnapshot, SettingsValues } from "./types.js";

export interface SettingsStoreState {
  readonly values: SettingsValues;
  /** The document is in memory: a load, a write or a seed supplied it. */
  readonly ready: boolean;
  /** `ready`, or the load settled with a failure. */
  readonly loaded: boolean;
  /** A write is queued or in flight. */
  readonly saving: boolean;
  /** The failure of the newest load, until a later load or write succeeds. A new object per failure. */
  readonly loadError: { readonly error: unknown } | null;
}

export interface SettingsWriteResult {
  ok: boolean;
  error?: unknown;
}

export interface SettingsStore<R extends SettingsRevision = number> {
  /** The current state. The same object until something changes, so it is a `useSyncExternalStore` snapshot. */
  getState(): SettingsStoreState;
  /** The revision of the last document the backend returned (or of the seed). */
  revision(): R | undefined;
  /** Notified after every change to the state. Returns an unsubscribe. */
  subscribe(listener: () => void): () => void;
  /**
   * Keeps the backend's change subscription open. The first holder opens it, the last one to release closes it.
   * The returned release is idempotent.
   */
  retain(): () => void;
  /** Reads the document from the backend. Never rejects: a failure goes to `loadError` and `onLoadError`. */
  load(): Promise<void>;
  /** As `load`, but joins a load that is already in flight and still current. */
  refresh(): Promise<void>;
  /** Uses a snapshot the caller already holds until the first load settles. False (and no change) once `ready`. */
  seed(snapshot: SettingsSnapshot<R>): boolean;
  /** Applies the patch to memory now and queues the write. `null` or `undefined` removes a key. Never rejects. */
  write(patch: SettingsValues): Promise<SettingsWriteResult>;
  /** Stops listeners and the backend subscription; results that settle later no longer change the state. */
  close(): void;
}

const isConflict = (e: unknown) => (e as { code?: unknown } | null)?.code === "revision_conflict";

function applyPatch(current: SettingsValues, patch: SettingsValues): SettingsValues {
  const next = { ...current };
  for (const [k, v] of Object.entries(patch)) {
    if (v === null || v === undefined) delete next[k];
    else next[k] = v;
  }
  return next;
}

// A reload usually returns what memory already holds. Keeping the same object then lets every consumer skip a render.
function sameValues(a: SettingsValues, b: SettingsValues): boolean {
  if (a === b) return true;
  const keys = Object.keys(a);
  return keys.length === Object.keys(b).length && keys.every((k) => k in b && Object.is(a[k], b[k]));
}

/**
 * Creates a store over a backend. Writes update memory at once and are serialised through one queue, so revisions never
 * race. A stale revision (another window wrote first) reloads the document and retries the key-wise patch once. A write
 * that still fails puts back the stored value of each key that still holds that write's value (a key a later queued write
 * has set again is left to that write), and the document is read again once the queue drains. An external change reloads
 * the document; while a write is pending the reload waits for the queue to drain.
 */
export function createSettingsStore<R extends SettingsRevision = number>(
  backend: SettingsBackend<R>,
  hooks: { onLoadError?: (error: unknown) => void } = {},
): SettingsStore<R> {
  let state: SettingsStoreState = { values: {}, ready: false, loaded: false, saving: false, loadError: null };
  // The last document the backend returned (or the seed): what a failed write falls back to.
  let stored: SettingsValues = {};
  let revision: R | undefined;
  let closed = false;
  let pending = 0;
  let writeSeq = 0;
  // Key -> the newest queued write that set it. That write's optimistic value is what memory holds for the key.
  const owner = new Map<string, number>();
  let loadSeq = 0;
  let inFlight: { seq: number; promise: Promise<void> } | null = null;
  let reloadWanted = false;
  let chain: Promise<unknown> = Promise.resolve();
  let holders = 0;
  let connection: { cancelled: boolean; off?: () => void } | null = null;
  const listeners = new Set<() => void>();

  const emit = () => {
    for (const l of [...listeners]) l();
  };
  const update = (change: Partial<SettingsStoreState>) => {
    const next = { ...state, ...change };
    if ((Object.keys(next) as (keyof SettingsStoreState)[]).every((k) => Object.is(next[k], state[k]))) return;
    state = next;
    emit();
  };
  // The backend's document becomes the stored truth; a key a queued write owns keeps its optimistic value.
  const adopt = (doc: SettingsValues, change: Partial<SettingsStoreState>) => {
    stored = doc;
    let next = doc;
    if (owner.size > 0) {
      next = { ...doc };
      for (const k of owner.keys()) {
        if (k in state.values) next[k] = state.values[k];
        else delete next[k];
      }
    }
    update({ ...change, values: sameValues(state.values, next) ? state.values : next });
  };

  const load = (): Promise<void> => {
    const seq = ++loadSeq;
    const promise = backend
      .load()
      .then(
        (snap) => {
          if (closed || seq !== loadSeq) return;
          // While writes are queued the revision is theirs to set, and the load is repeated once the queue drains.
          if (pending > 0) reloadWanted = true;
          else revision = snap.revision;
          adopt(snap.values, { ready: true, loaded: true, loadError: null });
        },
        (error) => {
          if (closed || seq !== loadSeq) return;
          update({ loaded: true, loadError: { error } });
          hooks.onLoadError?.(error);
        },
      )
      .finally(() => {
        if (inFlight?.seq === seq) inFlight = null;
      });
    inFlight = { seq, promise };
    return promise;
  };

  const write = (p: SettingsValues): Promise<SettingsWriteResult> => {
    const id = ++writeSeq;
    const keys = Object.keys(p);
    for (const k of keys) owner.set(k, id);
    pending += 1;
    update({ values: applyPatch(state.values, p), saving: true });
    const run = chain.then(async (): Promise<SettingsWriteResult> => {
      try {
        let snap: SettingsSnapshot<R>;
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
        for (const k of keys) if (owner.get(k) === id) owner.delete(k);
        pending -= 1;
        if (closed) return { ok: true };
        adopt(snap.values, { ready: true, loaded: true, loadError: null, saving: pending > 0 });
        return { ok: true };
      } catch (error) {
        // The value a key had before this write may itself be an optimistic value of an earlier write that failed, so
        // it is never restored. A key this write still owns goes back to the stored value; the reload settles the rest.
        const next = { ...state.values };
        for (const k of keys) {
          if (owner.get(k) !== id) continue;
          owner.delete(k);
          if (k in stored) next[k] = stored[k];
          else delete next[k];
        }
        pending -= 1;
        reloadWanted = true;
        if (!closed) update({ values: next, saving: pending > 0 });
        return { ok: false, error };
      } finally {
        if (pending === 0 && reloadWanted && !closed) {
          reloadWanted = false;
          void load();
        }
      }
    });
    chain = run;
    return run;
  };

  return {
    getState: () => state,
    revision: () => revision,
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    retain() {
      holders += 1;
      if (holders === 1 && backend.subscribe && !closed) {
        const c: { cancelled: boolean; off?: () => void } = { cancelled: false };
        connection = c;
        void backend
          .subscribe(() => {
            if (c.cancelled) return;
            if (pending === 0) void load();
            else reloadWanted = true;
          })
          .then(
            (off) => {
              if (c.cancelled) off();
              else c.off = off;
            },
            () => {},
          );
      }
      let released = false;
      return () => {
        if (released) return;
        released = true;
        holders -= 1;
        if (holders > 0 || !connection) return;
        connection.cancelled = true;
        connection.off?.();
        connection = null;
      };
    },
    load,
    refresh: () => (inFlight && inFlight.seq === loadSeq ? inFlight.promise : load()),
    seed(snapshot) {
      if (state.ready || pending > 0 || closed) return false;
      stored = snapshot.values;
      revision = snapshot.revision;
      // No synchronous notification: a consumer may seed while it renders. Consumers already subscribed are told next.
      state = { ...state, values: snapshot.values, ready: true, loaded: true };
      if (listeners.size > 0) queueMicrotask(emit);
      return true;
    },
    write,
    close() {
      closed = true;
      if (connection) {
        connection.cancelled = true;
        connection.off?.();
        connection = null;
      }
      listeners.clear();
    },
  };
}

const shared = new WeakMap<object, SettingsStore<SettingsRevision>>();

/**
 * The one store of a backend in this window: every caller that passes the same backend object gets the same store, so
 * they share its values, its single load, its revision and its write queue.
 */
export function sharedSettingsStore<R extends SettingsRevision = number>(backend: SettingsBackend<R>): SettingsStore<R> {
  let store = shared.get(backend) as SettingsStore<R> | undefined;
  if (!store) {
    store = createSettingsStore(backend);
    shared.set(backend, store as unknown as SettingsStore<SettingsRevision>);
  }
  return store;
}
