import type { SettingsBackend, SettingsSnapshot, SettingsValues } from "@rightkit/settings-ui";
import { errorCode, errorMessage } from "../../lib/engine.js";
import { settingsGet, settingsSet } from "../../lib/media.js";
import { themeStore } from "./shell.js";

/**
 * RightKit settings port over the engine's app-level settings file (the same
 * store the engine reads for finishing defaults, project folder, the host app
 * path). `appearance.theme` is mirrored from the app-shell theme store, which
 * stays the single owner of the theme choice. `null` removes a key.
 */
const asError = (e: unknown): never => {
  throw e instanceof Error ? e : Object.assign(new Error(errorMessage(e)), { code: errorCode(e) });
};

export function createHostSettingsBackend(): SettingsBackend {
  // The store's revision is a fingerprint of the file that can exceed 2^53, so the engine reports
  // it as a string. The kit sees a number: each snapshot gets a token, remembered with the engine's
  // revision it stands for, and a patch that names a token we did not hand out is stale.
  let token = 0;
  const revisions = new Map<number, string>();
  const snapshot = (values: SettingsValues, engineRevision: string): SettingsSnapshot => {
    token += 1;
    revisions.set(token, engineRevision);
    revisions.delete(token - 32);
    return { values: { ...values, "appearance.theme": themeStore.read() }, revision: token };
  };
  return {
    // The engine rejects with a {code, message} object; the settings kit shows
    // `Error.message`, so every failure leaves here as an Error.
    async load() {
      const s = await settingsGet().catch(asError);
      return snapshot(s.app, s.revision);
    },
    async patch(patch, expectedRevision) {
      const rest: SettingsValues = {};
      for (const [k, v] of Object.entries(patch)) {
        if (k === "appearance.theme") continue; // themeStore persisted it
        rest[k] = v;
      }
      let expected: string | undefined;
      if (expectedRevision !== undefined) {
        expected = revisions.get(expectedRevision);
        if (expected === undefined) {
          throw Object.assign(new Error("Settings changed since they were read."), { code: "revision_conflict" });
        }
      }
      const r = await (Object.keys(rest).length
        ? settingsSet(rest, undefined, expected)
        : settingsGet().then((s) => ({ settings: s.app, revision: s.revision }))
      ).catch((e: unknown) => {
        if (e && typeof e === "object" && (e as { code?: unknown }).code === "revision_conflict") {
          throw Object.assign(new Error(errorMessage(e)), { code: "revision_conflict" });
        }
        return asError(e);
      });
      return snapshot(r.settings, r.revision);
    },
  };
}
