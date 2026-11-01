// Typed access to the UI preferences.
//
// The one store is the `rightkit_settings_*` host document (`rightkit-tauri-shell` host services over a
// `rightkit-app-settings` Document, schema in src-tauri/src/settings_host.rs). The renderer reads the
// document once at boot (`initPreferences`, before the store is evaluated) and then answers every read
// from that in-memory copy, because the store initialisers and ~20 callers read synchronously.
//
// A write updates the in-memory copy at once and goes to the host through one serial queue as a
// single-key patch with `expectedRevision` (compare-and-set); a conflict reloads the document and
// retries. A write the host refuses restores the confirmed document and reports through
// `onPreferenceWriteError`. There is no other storage: nothing is mirrored or read from localStorage.
//
// GAP (docs/rightkit-exceptions/M10.md N1): `@rightkit/settings-ui` publishes the async SettingsBackend
// only, so this module is the synchronous cache over it.
import type { SettingsBackend, SettingsSnapshot } from "@rightkit/settings-ui/core";
import type { SavedSearch } from "../store/types.js";
import type { ImageRule } from "./imageTrust.js";

interface Def<T> {
  make(): T;
  valid(v: unknown): v is T;
}

const bool = (d: boolean): Def<boolean> => ({ make: () => d, valid: (v): v is boolean => typeof v === "boolean" });
const text = (d: string): Def<string> => ({ make: () => d, valid: (v): v is string => typeof v === "string" });
const count = (d: number): Def<number> => ({
  make: () => d,
  valid: (v): v is number => typeof v === "number" && Number.isFinite(v) && v >= 0,
});
const oneOf = <T extends string>(d: T, all: readonly T[]): Def<T> => ({
  make: () => d,
  valid: (v): v is T => typeof v === "string" && (all as readonly string[]).includes(v),
});
const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);
const strings = (): Def<string[]> => ({
  make: () => [],
  valid: (v): v is string[] => Array.isArray(v) && v.every((x) => typeof x === "string"),
});
const objects = <T extends object>(): Def<T[]> => ({
  make: () => [],
  valid: (v): v is T[] => Array.isArray(v) && v.every(isObject),
});
const map = <V extends string | boolean>(kind: "string" | "boolean"): Def<Record<string, V>> => ({
  make: () => ({}),
  valid: (v): v is Record<string, V> => isObject(v) && Object.values(v).every((x) => typeof x === kind),
});

/**
 * Every preference: its default and its shape. The key names and kinds mirror KEYS in
 * src-tauri/src/settings_host.rs (tests/preferences.test.ts checks that they agree).
 */
export const PREFERENCES = {
  // The shared settings panel's own key (standard diagnostics section).
  "diagnostics.verbose": bool(false),
  sound: bool(false),
  notify: bool(true),
  expandAll: bool(false),
  readingPane: bool(false),
  multiInbox: bool(false),
  showPro: bool(false),
  sort: oneOf("newest", ["newest", "oldest"] as const),
  density: oneOf("comfortable", ["comfortable", "compact"] as const),
  calView: oneOf("month", ["month", "week", "day"] as const),
  driveViewMode: oneOf("grid", ["grid", "list"] as const),
  translateProvider: text("google"),
  defaultAlias: text(""),
  undoDelay: count(5),
  recentSearches: strings(),
  savedSearches: objects<SavedSearch>(),
  hiddenLabels: strings(),
  collapsedLabels: strings(),
  shownCals: strings(),
  snippets: objects<{ name: string; body: string }>(),
  senderImageTrust: map<ImageRule>("string"),
  domainImageTrust: map<ImageRule>("string"),
  aliasSigs: map<string>("string"),
  labelColors: map<string>("string"),
  notifyAccounts: map<boolean>("boolean"),
} as const;

export type PreferenceKey = keyof typeof PREFERENCES;
export type PreferenceValue<K extends PreferenceKey> = ReturnType<(typeof PREFERENCES)[K]["make"]>;

type Values = Record<string, unknown>;

class PreferencesNotReady extends Error {
  constructor() {
    super("preferences are not loaded: initPreferences must finish before the first read");
  }
}

let host: SettingsBackend | null = null;
let values: Values = {};
let revision = 0;
let queue: Promise<unknown> = Promise.resolve();
/** Keys with a write not yet confirmed, so a reload never overwrites a newer local value. */
const unconfirmed = new Map<string, number>();
const errorHandlers = new Set<(message: string) => void>();

function require_(): SettingsBackend {
  if (!host) throw new PreferencesNotReady();
  return host;
}

/** Take the host snapshot as the confirmed state, keeping the local value of every key still being written. */
function adopt(snapshot: SettingsSnapshot): void {
  const next: Values = { ...snapshot.values };
  for (const key of unconfirmed.keys()) {
    if (key in values) next[key] = values[key];
    else delete next[key];
  }
  values = next;
  revision = snapshot.revision;
}

function message(e: unknown): string {
  // A host refusal is `{ code, message }` (HostError), not an Error instance.
  const text = (e as { message?: unknown } | null)?.message;
  return typeof text === "string" ? text : String(e);
}

function isConflict(e: unknown): boolean {
  return typeof e === "object" && e !== null && (e as { code?: unknown }).code === "revision_conflict";
}

/**
 * Load the document and make every read available. Call once, before ./store is first evaluated.
 * Rejects when the host cannot give the document (storage failure, newer schema): the app must not
 * run on defaults over a document it could not read.
 */
export async function initPreferences(backend: SettingsBackend): Promise<void> {
  host = backend;
  values = {};
  revision = 0;
  queue = Promise.resolve();
  unconfirmed.clear();
  adopt(await backend.load());
}

/** Be told when a write the host refused had to be taken back. Returns the unsubscribe. */
export function onPreferenceWriteError(handler: (message: string) => void): () => void {
  errorHandlers.add(handler);
  return () => errorHandlers.delete(handler);
}

/** The current value, or the default when it was never set (or holds a value of the wrong shape). */
export function getPref<K extends PreferenceKey>(key: K): PreferenceValue<K> {
  require_();
  const def = PREFERENCES[key] as unknown as Def<PreferenceValue<K>>;
  const stored = values[key];
  return key in values && def.valid(stored) ? stored : def.make();
}

/** Set a preference: visible to readers now, saved through the host queue. */
export function setPref<K extends PreferenceKey>(key: K, value: PreferenceValue<K>): void {
  require_();
  const def = PREFERENCES[key] as unknown as Def<PreferenceValue<K>>;
  if (!def.valid(value)) throw new TypeError(`preference ${key} has the wrong shape`);
  values = { ...values, [key]: value };
  persist(key, value);
}

/** Reset a preference to its default (removes the key from the document). */
export function clearPref(key: PreferenceKey): void {
  require_();
  const next = { ...values };
  delete next[key];
  values = next;
  persist(key, null);
}

function persist(key: string, value: unknown): void {
  unconfirmed.set(key, (unconfirmed.get(key) ?? 0) + 1);
  void enqueue(() => commit(key, value)).then(
    () => settle(key),
    async (e) => {
      settle(key);
      await restore();
      for (const handler of errorHandlers) handler(`A setting could not be saved: ${message(e)}`);
    },
  );
}

function settle(key: string): void {
  const left = (unconfirmed.get(key) ?? 1) - 1;
  if (left > 0) unconfirmed.set(key, left);
  else unconfirmed.delete(key);
}

function enqueue<T>(job: () => Promise<T>): Promise<T> {
  const run = queue.then(job, job);
  queue = run.catch(() => undefined);
  return run;
}

/** Resolves when every write made so far has been confirmed (or taken back). */
export function flushPreferences(): Promise<void> {
  // `queue` never rejects; a write enqueued after this call is not waited for.
  return queue.then(() => undefined);
}

const CONFLICT_ATTEMPTS = 3;

async function commit(key: string, value: unknown): Promise<void> {
  const backend = require_();
  for (let attempt = 1; ; attempt += 1) {
    try {
      const snapshot = await backend.patch({ [key]: value as never }, revision);
      adopt(snapshot);
      return;
    } catch (e) {
      if (!isConflict(e) || attempt >= CONFLICT_ATTEMPTS) throw e;
      adopt(await backend.load());
    }
  }
}

/** Take the confirmed document back as the readable state after a refused write. */
async function restore(): Promise<void> {
  try {
    adopt(await require_().load());
  } catch {
    /* the host cannot be read: the unconfirmed local values stay until the next successful write */
  }
}

/**
 * The backend of the settings panel: same document, same revision counter and same queue as the
 * preferences, so a panel write and an app write never race on `expectedRevision`. A confirmed panel
 * write is adopted into the readable state. The panel is the only other writer, so there is no
 * subscription (the host's change event is for a second window or process).
 */
export function createPanelBackend(): SettingsBackend {
  return {
    load: () => enqueue(async () => {
      const snapshot = await require_().load();
      adopt(snapshot);
      return { ...snapshot, values: { ...snapshot.values } };
    }),
    patch: (patch, expectedRevision) =>
      enqueue(async () => {
        const snapshot = await require_().patch(patch, expectedRevision);
        adopt(snapshot);
        return { ...snapshot, values: { ...snapshot.values } };
      }),
  };
}
