import type {
  ModelEntry,
  ModelsBackend,
  SecretPresence,
  SecretRef,
  SecretsBackend,
  SettingsBackend,
  SettingsSnapshot,
  SettingsValues,
} from "./types.js";
import type { Shell } from "@rightkit/shell";

/** The slice of `@rightkit/shell`'s `createShell()` these adapters need (structural, so no import cycle). */
export interface EngineShell {
  engine: {
    call<T = unknown>(method: string, params?: unknown): Promise<T>;
    onEvent(handler: (e: { call: string; event: unknown }) => void): Promise<() => void>;
  };
}

/** Native shell bridge accepted by createShellBackends; exported for app adapters. */
export type SettingsShell = Pick<Shell, "engine">;

/** Engine method names: the wire contract between this package and the host's `rightkit-settings` / `rightkit-models` handlers. */
export const METHODS = {
  settingsGet: "rightkit.settings.get",
  settingsPatch: "rightkit.settings.patch",
  modelsList: "rightkit.models.list",
  modelsInstall: "rightkit.models.install",
  modelsCancel: "rightkit.models.cancel",
  modelsRemove: "rightkit.models.remove",
  secretsPresence: "rightkit.secrets.presence",
  secretsSet: "rightkit.secrets.set",
  secretsClear: "rightkit.secrets.clear",
} as const;

export const EVENTS = {
  settingsChanged: "rightkit.settings.changed",
  modelsEntry: "rightkit.models.entry",
} as const;

export interface ShellBackends {
  settings: SettingsBackend;
  models: ModelsBackend;
  secrets: SecretsBackend;
}

/** Backends over the `@rightkit/shell` engine bridge. */
export function createShellBackends(shell: EngineShell | SettingsShell): ShellBackends {
  const call = shell.engine.call;
  const on = <T>(name: string, pick: (event: unknown) => T | null, handler: (v: T) => void) =>
    shell.engine.onEvent((e) => {
      if (e.call !== name) return;
      const v = pick(e.event);
      if (v !== null) handler(v);
    });
  return {
    settings: {
      load: () => call<SettingsSnapshot>(METHODS.settingsGet),
      patch: (patch, expectedRevision) => call<SettingsSnapshot>(METHODS.settingsPatch, { patch, expectedRevision: expectedRevision ?? null }),
      subscribe: (handler) => on<string[]>(EVENTS.settingsChanged, (e) => (Array.isArray((e as { keys?: unknown }).keys) ? ((e as { keys: string[] }).keys) : null), handler),
    },
    models: {
      list: () => call<ModelEntry[]>(METHODS.modelsList),
      install: (id) => call<void>(METHODS.modelsInstall, { id }),
      cancel: (id) => call<void>(METHODS.modelsCancel, { id }),
      remove: (id) => call<void>(METHODS.modelsRemove, { id }),
      subscribe: (handler) => on<ModelEntry>(EVENTS.modelsEntry, (e) => (e && typeof (e as ModelEntry).id === "string" ? (e as ModelEntry) : null), handler),
    },
    secrets: {
      presence: (refs) => call<SecretPresence[]>(METHODS.secretsPresence, { refs }),
      set: (ref, value) => call<void>(METHODS.secretsSet, { ref, value }),
      clear: (ref) => call<void>(METHODS.secretsClear, { ref }),
    },
  };
}

export class RevisionConflict extends Error {
  readonly code = "revision_conflict";
}

/** In-memory settings backend: same contract as the host (revisions, null-removes, change events). For previews and tests of app sections. */
export function createMemorySettings(initial: SettingsValues = {}): SettingsBackend & { snapshot(): SettingsSnapshot } {
  let values: SettingsValues = { ...initial };
  let revision = 1;
  const listeners = new Set<(keys: string[]) => void>();
  const snapshot = (): SettingsSnapshot => ({ values: { ...values }, revision });
  return {
    snapshot,
    async load() {
      return snapshot();
    },
    async patch(patch, expectedRevision) {
      if (expectedRevision !== undefined && expectedRevision !== revision) throw new RevisionConflict("settings changed elsewhere");
      const next = { ...values };
      for (const [k, v] of Object.entries(patch)) {
        if (v === null) delete next[k];
        else next[k] = v;
      }
      values = next;
      revision += 1;
      for (const l of listeners) l(Object.keys(patch));
      return snapshot();
    },
    async subscribe(handler) {
      listeners.add(handler);
      return () => void listeners.delete(handler);
    },
  };
}

const keyOf = (r: SecretRef) => `${r.service}\u0000${r.account}`;

/** In-memory credential store for previews/tests. Mirrors the real port: values are write-only. */
export function createMemorySecrets(): SecretsBackend & { _has(ref: SecretRef): boolean } {
  const store = new Map<string, string>();
  return {
    async presence(refs) {
      return refs.map((r) => (store.has(keyOf(r)) ? "present" : "absent"));
    },
    async set(ref, value) {
      store.set(keyOf(ref), value);
    },
    async clear(ref) {
      store.delete(keyOf(ref));
    },
    _has: (ref) => store.has(keyOf(ref)),
  };
}
