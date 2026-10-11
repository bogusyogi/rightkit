// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { useSettings } from "./useSettings.js";
import type { SettingsBackend, SettingsSnapshot, SettingsValues } from "./types.js";

afterEach(cleanup);

/** A backend whose first load() is held open until `releaseLoad` is called. */
function slowLoadBackend(stored: SettingsValues, failPatch = false) {
  let revision = 1;
  let releaseLoad!: () => void;
  let first = true;
  let loads = 0;
  const snapshot = (): SettingsSnapshot => ({ values: { ...stored }, revision });
  const backend: SettingsBackend = {
    load: () => {
      loads += 1;
      if (!first) return Promise.resolve(snapshot());
      first = false;
      const before = snapshot();
      return new Promise((resolve) => (releaseLoad = () => resolve(before)));
    },
    patch: async (p) => {
      if (failPatch) throw new Error("disk full");
      for (const [k, v] of Object.entries(p)) {
        if (v === null) delete stored[k];
        else stored[k] = v;
      }
      revision += 1;
      return snapshot();
    },
  };
  return { backend, releaseLoad: () => releaseLoad(), loads: () => loads };
}

describe("useSettings reload against pending writes", () => {
  it("a load that finishes while a write is pending does not overwrite the written value", async () => {
    const { backend, releaseLoad } = slowLoadBackend({ theme: "light", size: 1 });
    const { result } = renderHook(() => useSettings(backend, { initial: { values: { theme: "light" }, revision: 1 } }));
    let write!: Promise<void>;
    act(() => {
      write = result.current.set("theme", "dark");
    });
    expect(result.current.values.theme).toBe("dark");
    await act(async () => {
      releaseLoad(); // the mount-time load answers with the state from before the write
      await write;
    });
    await waitFor(() => expect(result.current.saving).toBe(false));
    expect(result.current.values).toEqual({ theme: "dark", size: 1 });
  });

  it("a skipped load is repeated after a failed write, so the stored values still arrive", async () => {
    const { backend, releaseLoad, loads } = slowLoadBackend({ theme: "light", size: 1 }, true);
    const { result } = renderHook(() => useSettings(backend, { initial: { values: { theme: "light" }, revision: 1 } }));
    let write!: Promise<void>;
    act(() => {
      write = result.current.set("theme", "dark");
    });
    await act(async () => {
      releaseLoad();
      await write;
    });
    await waitFor(() => expect(result.current.values).toEqual({ theme: "light", size: 1 }));
    expect(result.current.error).toBe("disk full");
    expect(loads()).toBe(2);
  });
});
