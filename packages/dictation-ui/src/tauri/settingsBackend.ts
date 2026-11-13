// The settings backend of the Tauri dictation client on its own: a `SettingsBackend` over the app's
// `get_settings_snapshot` / `apply_settings_patch_at` commands. An app that renders `@rightkit/settings-ui`
// without the dictation hub imports this entry-point function and nothing of the 55-command client.
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import type { SettingsBackend, SettingsSnapshot, SettingsValues } from "@rightkit/settings-ui/core";

export interface TauriSettingsOptions {
  /**
   * Maps the native settings values to the keys the pages use (`load`, `patch` results) and a patch back to
   * native keys (before `apply_settings_patch_at`). Default: identity. HeardRight uses it for the
   * `appearance.theme` choice.
   */
  settingsKeys?: {
    fromNative(values: SettingsValues): SettingsValues;
    toNative(patch: SettingsValues): SettingsValues;
  };
  /** Name of the window event that fans settings changes out inside one window. Default `"rk-dictation-settings-updated"`. */
  settingsEvent?: string;
  /** Native call, for an app that routes IPC through its own bridge. Default: Tauri's `invoke`. */
  invoke?: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
}

interface NativeSnapshot {
  values: SettingsValues;
  /** A 64-bit revision as a decimal string. */
  revision: string;
}

/**
 * The native revision is a 64-bit value sent as a decimal string: a JS number rounds it above 2^53 and the
 * rounded value would never match the store again. The settings port types the revision as a number and
 * only echoes it, so it is handed a small opaque handle; the exact string stays here.
 */
export function tauriDictationSettingsBackend(options: TauriSettingsOptions = {}): SettingsBackend {
  const maxHandles = 64;
  const eventName = options.settingsEvent ?? "rk-dictation-settings-updated";
  const keys = options.settingsKeys ?? { fromNative: (v: SettingsValues) => v, toNative: (p: SettingsValues) => p };
  const invoke = options.invoke ?? tauriInvoke;
  let nextHandle = 1;
  const revisionByHandle = new Map<number, string>();
  const handleByRevision = new Map<string, number>();

  function revisionHandle(revision: string): number {
    const known = handleByRevision.get(revision);
    if (known !== undefined) return known;
    const handle = nextHandle++;
    handleByRevision.set(revision, handle);
    revisionByHandle.set(handle, revision);
    if (revisionByHandle.size > maxHandles) {
      const oldest = revisionByHandle.keys().next().value as number;
      const old = revisionByHandle.get(oldest);
      revisionByHandle.delete(oldest);
      if (old !== undefined) handleByRevision.delete(old);
    }
    return handle;
  }

  const snapshot = ({ values, revision }: NativeSnapshot): SettingsSnapshot => ({
    values: keys.fromNative(values),
    revision: revisionHandle(revision),
  });

  const conflictError = (error: unknown): Error => {
    const message = error instanceof Error ? error.message : String(error);
    if (!message.startsWith("revision_conflict")) return error instanceof Error ? error : new Error(message);
    return Object.assign(new Error(message), { code: "revision_conflict" });
  };

  return {
    async load() {
      return snapshot(await invoke<NativeSnapshot>("get_settings_snapshot"));
    },
    async patch(patch, expectedRevision) {
      let native: NativeSnapshot;
      try {
        native = await invoke<NativeSnapshot>("apply_settings_patch_at", {
          patch: keys.toNative({ ...patch }),
          // An unknown handle sends text the native side refuses as stale (null would drop the guard).
          expectedRevision: expectedRevision === undefined ? null : (revisionByHandle.get(expectedRevision) ?? "stale"),
        });
      } catch (error) {
        throw conflictError(error);
      }
      window.dispatchEvent(new CustomEvent(eventName, { detail: native.values }));
      return snapshot(native);
    },
    async subscribe(handler) {
      const onUpdate = ((event: Event) => {
        const detail = (event as CustomEvent<SettingsValues | undefined>).detail;
        if (detail !== undefined) handler(Object.keys(detail));
      }) as EventListener;
      window.addEventListener(eventName, onUpdate);
      return () => window.removeEventListener(eventName, onUpdate);
    },
  };
}
