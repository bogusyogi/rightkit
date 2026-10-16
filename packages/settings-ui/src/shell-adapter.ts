/**
 * Shell-backed adapters: `rightkit-app-settings` / `rightkit-models` / credential store reached through the
 * `@rightkit/tauri-shell` engine bridge. Import from `@rightkit/settings-ui/shell-adapter`. The engine is
 * described structurally, so this module needs no runtime or type import from `@rightkit/tauri-shell`; any
 * object with the same `engine.call` / `engine.onEvent` shape works.
 */
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

/** The slice of `@rightkit/tauri-shell`'s `createShell()` these adapters need (structural, so no import cycle). */
export interface EngineShell {
  engine: {
    call<T = unknown>(method: string, params?: unknown): Promise<T>;
    onEvent(handler: (e: { call: string; event: unknown }) => void): Promise<() => void>;
  };
}

/** Native shell bridge accepted by createShellBackends (structurally `Pick<Shell, "engine">`). */
export type SettingsShell = { engine: EngineShell["engine"] };

/** Engine method names: the wire contract between this package and the host's `rightkit-app-settings` / `rightkit-models` handlers. */
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

/** Backends over the `@rightkit/tauri-shell` engine bridge. */
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
