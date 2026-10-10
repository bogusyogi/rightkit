import type {
  SecretRef,
  SecretsBackend,
  SettingsBackend,
  SettingsSnapshot,
  SettingsValues,
} from "./types.js";

/**
 * @deprecated The shell-backed adapter moved to `@rightkit/settings-ui/shell-adapter`; this re-export
 * keeps existing root imports working. Blob-backed or test adapters implement `SettingsBackend` directly.
 */
export { createShellBackends, METHODS, EVENTS } from "./shell-adapter.js";
export type { EngineShell, SettingsShell, ShellBackends } from "./shell-adapter.js";

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
