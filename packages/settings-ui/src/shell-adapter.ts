/**
 * Shell-backed adapters: the `rightkit_settings_*` and `rightkit_secrets_*` Tauri commands that
 * `rightkit-tauri-shell` (feature `host-services`) registers through `handler!`. Import from
 * `@rightkit/settings-ui/shell-adapter`. The transport is described structurally (it is the
 * `ShellTransport` of `@rightkit/tauri-shell`: `tauriTransport()` or your own), so this module needs
 * no runtime or type import from `@rightkit/tauri-shell`.
 *
 * Only what the host registers is wrapped. The host registers no models command, so there is no
 * models backend here: an app that shows the Models section supplies its own `ModelsBackend`.
 * No command returns a secret, and this module has no secret getter.
 */
import type {
  SecretPresence,
  SecretRef,
  SecretsBackend,
  SettingsBackend,
  SettingsSnapshot,
  WriteIntent,
} from "./types.js";

/**
 * The slice of `@rightkit/tauri-shell`'s `ShellTransport` these adapters need (structural, so no
 * import cycle). `listen` hands the handler the event payload itself.
 */
export interface HostTransport {
  invoke<T = unknown>(command: string, args?: Record<string, unknown>): Promise<T>;
  listen<T = unknown>(event: string, handler: (payload: T) => void): Promise<() => void>;
}

/** What `createShellBackends` accepts: a transport (`tauriTransport()` of `@rightkit/tauri-shell`). */
export type SettingsShell = HostTransport;

/** Tauri command names registered by `rightkit-tauri-shell`'s host services: the wire contract of this package. */
export const METHODS = {
  settingsDescribe: "rightkit_settings_describe",
  settingsGet: "rightkit_settings_get",
  settingsPatch: "rightkit_settings_patch",
  settingsReset: "rightkit_settings_reset",
  settingsSubscribe: "rightkit_settings_subscribe",
  settingsUnsubscribe: "rightkit_settings_unsubscribe",
  secretsPresence: "rightkit_secrets_presence",
  secretsSet: "rightkit_secrets_set",
  secretsClear: "rightkit_secrets_clear",
} as const;

/** Event names the host emits to subscribed windows. */
export const EVENTS = {
  /** Payload `{ keys: string[], revision: number }`, emitted after a commit. */
  settingsChanged: "rightkit://settings-changed",
} as const;

/** What `rightkit_settings_describe` answers. `descriptor` is the app's own UI description, opaque here. */
export interface SettingsDescription {
  schemaVersion: number;
  revision: number;
  descriptor: unknown;
  /** False when the bridge is read-only or the stored document is refused. */
  writable: boolean;
  /** Error code that makes the stored document unreadable (`newer_schema`, `storage`, ...). */
  refusal?: string;
  limits: { maxValueBytes: number; maxDocumentBytes: number };
}

/** Settings backend plus the host-only calls the panel does not need (describe, reset). */
export interface ShellSettingsBackend extends SettingsBackend {
  describe(): Promise<SettingsDescription>;
  /** Remove top-level `keys` from the document (compare-and-set when `expectedRevision` is given). */
  reset(keys: string[], expectedRevision?: number): Promise<SettingsSnapshot>;
}

export interface ShellBackends {
  settings: ShellSettingsBackend;
  secrets: SecretsBackend;
}

/** Backends over the host-services Tauri commands. */
export function createShellBackends(shell: HostTransport | SettingsShell): ShellBackends {
  const { invoke, listen } = shell;
  return {
    settings: {
      describe: () => invoke<SettingsDescription>(METHODS.settingsDescribe),
      load: () => invoke<SettingsSnapshot>(METHODS.settingsGet),
      patch: (patch, expectedRevision) => invoke<SettingsSnapshot>(METHODS.settingsPatch, { patch, expectedRevision: expectedRevision ?? null }),
      reset: (keys, expectedRevision) => invoke<SettingsSnapshot>(METHODS.settingsReset, { keys, expectedRevision: expectedRevision ?? null }),
      // The host sends the event only to windows that subscribed (by their native label), so the
      // listener is attached first, then the window subscribes; unsubscribing reverses both.
      subscribe: async (handler) => {
        const off = await listen<{ keys?: unknown }>(EVENTS.settingsChanged, (change) => {
          const keys = change?.keys;
          if (Array.isArray(keys) && keys.every((k) => typeof k === "string")) handler(keys as string[]);
        });
        try {
          await invoke<number>(METHODS.settingsSubscribe);
        } catch (e) {
          off();
          throw e;
        }
        return () => {
          off();
          void invoke<void>(METHODS.settingsUnsubscribe).catch(() => undefined);
        };
      },
    },
    secrets: {
      presence: (refs) => invoke<SecretPresence[]>(METHODS.secretsPresence, { refs }),
      // The value goes in and nothing comes back. `replace` is the default: the settings field
      // replaces a stored key; pass `create` to refuse an occupied slot (`secret_exists`).
      set: (ref: SecretRef, value: string, intent: WriteIntent = "replace") => invoke<void>(METHODS.secretsSet, { secret: ref, value, intent }),
      clear: (ref) => invoke<void>(METHODS.secretsClear, { secret: ref }),
    },
  };
}
