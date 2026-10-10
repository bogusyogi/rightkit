import { describe, expect, it, vi } from "vitest";
import { createShellBackends, METHODS } from "./shell-adapter.js";
import * as root from "./index.js";

function fakeEngine() {
  const call = vi.fn(async (_method: string, _params?: unknown) => ({ values: {}, revision: 3 }));
  const handlers = new Set<(e: { call: string; event: unknown }) => void>();
  return {
    call,
    engine: {
      call,
      onEvent: async (handler: (e: { call: string; event: unknown }) => void) => {
        handlers.add(handler);
        return () => void handlers.delete(handler);
      },
    },
    emit: (e: { call: string; event: unknown }) => handlers.forEach((h) => h(e)),
  };
}

describe("shell adapter (@rightkit/settings-ui/shell-adapter)", () => {
  it("routes settings reads and writes through the engine wire methods", async () => {
    const shell = fakeEngine();
    const backends = createShellBackends(shell);
    await backends.settings.load();
    await backends.settings.patch({ "appearance.theme": "dark" }, 3);
    expect(shell.call).toHaveBeenNthCalledWith(1, METHODS.settingsGet);
    expect(shell.call).toHaveBeenNthCalledWith(2, METHODS.settingsPatch, { patch: { "appearance.theme": "dark" }, expectedRevision: 3 });
  });

  it("forwards only settings-changed events that carry keys", async () => {
    const shell = fakeEngine();
    const backends = createShellBackends(shell);
    const keys: string[][] = [];
    await backends.settings.subscribe?.((k) => keys.push(k));
    shell.emit({ call: "rightkit.settings.changed", event: { keys: ["a"] } });
    shell.emit({ call: "rightkit.settings.changed", event: { nope: true } });
    expect(keys).toEqual([["a"]]);
  });

  it("is still re-exported from the root for existing imports", () => {
    expect(root.createShellBackends).toBe(createShellBackends);
  });
});
