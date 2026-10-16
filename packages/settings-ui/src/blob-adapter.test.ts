import { describe, expect, it } from "vitest";
import type { SettingsBackend, SettingsSnapshot, SettingsValues } from "./types.js";
import { RevisionConflict } from "./backends.js";

/**
 * Example of an app-supplied adapter: the whole settings object lives in one string blob (for
 * instance an app's own JSON file or a single storage key). It implements `SettingsBackend` directly
 * and needs no `@rightkit/tauri-shell`.
 */
function blobSettings(read: () => string | null, write: (text: string) => void): SettingsBackend {
  const parse = (): SettingsSnapshot => {
    const raw = read();
    const parsed = raw ? (JSON.parse(raw) as { revision?: number; values?: SettingsValues }) : {};
    return { values: parsed.values ?? {}, revision: parsed.revision ?? 0 };
  };
  return {
    async load() {
      return parse();
    },
    async patch(patch, expectedRevision) {
      const current = parse();
      if (expectedRevision !== undefined && expectedRevision !== current.revision) throw Object.assign(new RevisionConflict("stale"), { code: "revision_conflict" });
      const values = { ...current.values };
      for (const [k, v] of Object.entries(patch)) {
        if (v === null) delete values[k];
        else values[k] = v;
      }
      const next = { values, revision: current.revision + 1 };
      write(JSON.stringify(next));
      return next;
    },
  };
}

describe("blob-backed SettingsBackend", () => {
  it("persists patches with revisions and rejects a stale write", async () => {
    let blob: string | null = null;
    const backend = blobSettings(() => blob, (t) => {
      blob = t;
    });
    const first = await backend.patch({ "appearance.theme": "dark" });
    expect(first.revision).toBe(1);
    await expect(backend.patch({ x: 1 }, 0)).rejects.toMatchObject({ code: "revision_conflict" });
    const second = await backend.patch({ "appearance.theme": null }, 1);
    expect(second.values).toEqual({});
    expect((await backend.load()).revision).toBe(2);
  });
});
