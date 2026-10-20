import { describe, expect, it, vi } from "vitest";
import { createShellBackends, EVENTS, METHODS } from "./shell-adapter.js";
import * as root from "./index.js";

function fakeHost() {
  const invoke = vi.fn(async (_command: string, _args?: Record<string, unknown>): Promise<unknown> => ({ values: {}, revision: 3 }));
  const handlers = new Map<string, Set<(payload: unknown) => void>>();
  const listen = vi.fn(async (event: string, handler: (payload: unknown) => void) => {
    let set = handlers.get(event);
    if (!set) handlers.set(event, (set = new Set()));
    set.add(handler);
    return () => void set.delete(handler);
  });
  return {
    invoke,
    listen,
    transport: { invoke: invoke as never, listen: listen as never },
    emit: (event: string, payload: unknown) => handlers.get(event)?.forEach((h) => h(payload)),
    listeners: (event: string) => handlers.get(event)?.size ?? 0,
  };
}

describe("shell adapter (@rightkit/settings-ui/shell-adapter)", () => {
  it("targets only names the shell host registers", () => {
    expect(Object.values(METHODS).sort()).toEqual(
      [
        "rightkit_secrets_clear",
        "rightkit_secrets_presence",
        "rightkit_secrets_set",
        "rightkit_settings_describe",
        "rightkit_settings_get",
        "rightkit_settings_patch",
        "rightkit_settings_reset",
        "rightkit_settings_subscribe",
        "rightkit_settings_unsubscribe",
      ].sort(),
    );
    expect(Object.keys(createShellBackends(fakeHost().transport)).sort()).toEqual(["secrets", "settings"]);
  });

  it("routes settings reads and writes through the registered commands", async () => {
    const host = fakeHost();
    const backends = createShellBackends(host.transport);
    await backends.settings.load();
    await backends.settings.patch({ "appearance.theme": "dark" }, 3);
    await backends.settings.reset(["appearance.theme"]);
    await backends.settings.describe();
    expect(host.invoke).toHaveBeenNthCalledWith(1, METHODS.settingsGet);
    expect(host.invoke).toHaveBeenNthCalledWith(2, METHODS.settingsPatch, { patch: { "appearance.theme": "dark" }, expectedRevision: 3 });
    expect(host.invoke).toHaveBeenNthCalledWith(3, METHODS.settingsReset, { keys: ["appearance.theme"], expectedRevision: null });
    expect(host.invoke).toHaveBeenNthCalledWith(4, METHODS.settingsDescribe);
  });

  it("listens, subscribes the window, and forwards only changes that carry keys", async () => {
    const host = fakeHost();
    const backends = createShellBackends(host.transport);
    const keys: string[][] = [];
    const off = await backends.settings.subscribe?.((k) => keys.push(k));
    expect(host.listen).toHaveBeenCalledWith(EVENTS.settingsChanged, expect.any(Function));
    expect(host.invoke).toHaveBeenCalledWith(METHODS.settingsSubscribe);
    host.emit(EVENTS.settingsChanged, { keys: ["a"], revision: 4 });
    host.emit(EVENTS.settingsChanged, { nope: true });
    host.emit(EVENTS.settingsChanged, { keys: ["a", 1] });
    expect(keys).toEqual([["a"]]);
    off?.();
    expect(host.listeners(EVENTS.settingsChanged)).toBe(0);
    expect(host.invoke).toHaveBeenLastCalledWith(METHODS.settingsUnsubscribe);
  });

  it("drops the listener when the host refuses the subscription", async () => {
    const host = fakeHost();
    host.invoke.mockImplementation(async (command: string) => {
      if (command === METHODS.settingsSubscribe) throw { code: "permission_denied", message: "refused" };
      return {};
    });
    await expect(createShellBackends(host.transport).settings.subscribe?.(() => undefined)).rejects.toMatchObject({ code: "permission_denied" });
    expect(host.listeners(EVENTS.settingsChanged)).toBe(0);
  });

  it("sends secrets write-only with a declared intent, and never reads one back", async () => {
    const host = fakeHost();
    host.invoke.mockImplementation(async (command: string) => (command === METHODS.secretsPresence ? ["present"] : undefined));
    const { secrets } = createShellBackends(host.transport);
    const ref = { service: "acme", account: "provider-key" };
    expect(await secrets.presence([ref])).toEqual(["present"]);
    await secrets.set(ref, "sk-test");
    await secrets.set(ref, "sk-test", "create");
    await secrets.clear(ref);
    expect(host.invoke).toHaveBeenCalledWith(METHODS.secretsPresence, { refs: [ref] });
    expect(host.invoke).toHaveBeenCalledWith(METHODS.secretsSet, { secret: ref, value: "sk-test", intent: "replace" });
    expect(host.invoke).toHaveBeenCalledWith(METHODS.secretsSet, { secret: ref, value: "sk-test", intent: "create" });
    expect(host.invoke).toHaveBeenCalledWith(METHODS.secretsClear, { secret: ref });
    expect(Object.keys(secrets).sort()).toEqual(["clear", "presence", "set"]);
  });

  it("is still re-exported from the root for existing imports", () => {
    expect(root.createShellBackends).toBe(createShellBackends);
  });
});
