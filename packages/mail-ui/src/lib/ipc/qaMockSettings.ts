// Browser-QA mock for the `rightkit_settings_*` commands that rightkit-tauri-shell answers in the real
// app (the UI preference document). Same contract: a revision per commit, `null` removes a key,
// `expectedRevision` is compare-and-set. State lives for the page session only.

import type { InvokeArgs } from "./qaMockTypes.js";

const values: Record<string, unknown> = {};
let revision = 0;

function snapshot() {
  return { values: JSON.parse(JSON.stringify(values)) as Record<string, unknown>, revision, schemaVersion: 1 };
}

function checkRevision(expected: unknown): void {
  if (typeof expected === "number" && expected !== revision) {
    throw { code: "revision_conflict", message: `settings changed since revision ${expected}; now at revision ${revision}` };
  }
}

export function qaSettingsInvoke(
  command: string,
  args?: InvokeArgs,
): { handled: boolean; value: unknown } {
  switch (command) {
    case "rightkit_settings_describe":
      return {
        handled: true,
        value: {
          schemaVersion: 1,
          revision,
          descriptor: null,
          writable: true,
          limits: { maxValueBytes: 262144, maxDocumentBytes: 2097152 },
        },
      };
    case "rightkit_settings_get":
      return { handled: true, value: snapshot() };
    case "rightkit_settings_patch": {
      checkRevision(args?.expectedRevision);
      const patch = (args?.patch ?? {}) as Record<string, unknown>;
      for (const [key, value] of Object.entries(patch)) {
        if (value === null) delete values[key];
        else values[key] = value;
      }
      revision += 1;
      return { handled: true, value: snapshot() };
    }
    case "rightkit_settings_reset": {
      checkRevision(args?.expectedRevision);
      for (const key of (args?.keys ?? []) as string[]) delete values[key];
      revision += 1;
      return { handled: true, value: snapshot() };
    }
    case "rightkit_settings_subscribe":
      return { handled: true, value: revision };
    case "rightkit_settings_unsubscribe":
      return { handled: true, value: undefined };
    default:
      return { handled: false, value: undefined };
  }
}
