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
  SettingsRevision,
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
  /** Payload `{ keys: string[], revision?: number, revisionText: string }`, emitted after a commit. */
  settingsChanged: "rightkit://settings-changed",
} as const;

/** What `rightkit_settings_describe` answers. `descriptor` is the app's own UI description, opaque here. */
export interface SettingsDescription {
  schemaVersion: number;
  /** Absent for a flat settings file, whose revision is a content hash: see `revisionText`. */
  revision?: number;
  /** The revision as text (decimal for a counter, 16 hex digits for a flat file's hash). */
  revisionText?: string;
  descriptor: unknown;
  /** False when the bridge is read-only or the stored document is refused. */
  writable: boolean;
  /** Error code that makes the stored document unreadable (`newer_schema`, `storage`, ...). */
  refusal?: string;
  limits: { maxValueBytes: number; maxDocumentBytes: number };
}

/** Settings backend plus the host-only calls the panel does not need (describe, reset). */
export interface ShellSettingsBackend<R extends SettingsRevision = number> extends SettingsBackend<R> {
  describe(): Promise<SettingsDescription>;
  /** Remove top-level `keys` from the document (compare-and-set when `expectedRevision` is given). */
  reset(keys: string[], expectedRevision?: R): Promise<SettingsSnapshot<R>>;
}

export interface ShellBackends<R extends SettingsRevision = number> {
  settings: ShellSettingsBackend<R>;
  secrets: SecretsBackend;
}

/** The snapshot as the host sends it: `revision` is absent for a flat file and `revisionText` carries the revision. */
type WireSnapshot = Omit<SettingsSnapshot, "revision"> & { revision?: number; revisionText?: string };

/**
 * Backends over the host-services Tauri commands.
 *
 * A host with a versioned (counter) document answers a numeric `revision`, and this adapter hands that number on, so `R`
 * stays `number`. A host with a flat settings file (revision = 64-bit content hash) answers only `revisionText`, a string
 * that is sent back unchanged as `expectedRevision`; call `createShellBackends<string>(shell)` for it.
 */
export function createShellBackends<R extends SettingsRevision = number>(shell: HostTransport | SettingsShell): ShellBackends<R> {
  const { invoke, listen } = shell;
  const snapshot = ({ revision, revisionText, ...rest }: WireSnapshot): SettingsSnapshot<R> =>
    ({ ...rest, revision: (revision ?? revisionText) as R });

  // The host keeps one subscription per window and drops it on `settingsUnsubscribe`, so every consumer of this instance
  // shares one listener and one host subscription; the host is told only when the last handler leaves. The host sends the
  // event only to subscribed windows, so the listener is attached first, then the window subscribes; leaving reverses both.
  const handlers = new Set<(keys: string[]) => void>();
  let connection: Promise<() => void> | null = null;
  // Connects and disconnects run one after another, so a subscribe right after the last unsubscribe cannot be undone by it.
  let queue: Promise<unknown> = Promise.resolve();
  const connect = async (): Promise<() => void> => {
    const off = await listen<{ keys?: unknown }>(EVENTS.settingsChanged, (change) => {
      const keys = change?.keys;
      if (Array.isArray(keys) && keys.every((k) => typeof k === "string")) for (const h of [...handlers]) h(keys as string[]);
    });
    try {
      await invoke<number | string>(METHODS.settingsSubscribe);
    } catch (e) {
      off();
      throw e;
    }
    return () => {
      off();
      void invoke<void>(METHODS.settingsUnsubscribe).catch(() => undefined);
    };
  };
  const subscribe: SettingsBackend["subscribe"] = async (handler) => {
    handlers.add(handler);
    const mine = (connection ??= queue.then(connect));
    queue = mine.catch(() => undefined);
    try {
      await mine;
    } catch (e) {
      handlers.delete(handler);
      if (connection === mine) connection = null;
      throw e;
    }
    let left = false;
    return () => {
      if (left) return;
      left = true;
      handlers.delete(handler);
      if (handlers.size > 0 || connection !== mine) return;
      connection = null;
      queue = mine.then((disconnect) => disconnect(), () => undefined);
    };
  };

  return {
    settings: {
      describe: () => invoke<SettingsDescription>(METHODS.settingsDescribe),
      load: () => invoke<WireSnapshot>(METHODS.settingsGet).then(snapshot),
      patch: (patch, expectedRevision) => invoke<WireSnapshot>(METHODS.settingsPatch, { patch, expectedRevision: expectedRevision ?? null }).then(snapshot),
      reset: (keys, expectedRevision) => invoke<WireSnapshot>(METHODS.settingsReset, { keys, expectedRevision: expectedRevision ?? null }).then(snapshot),
      subscribe,
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
