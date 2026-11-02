import { describe, expect, it, vi } from "vitest";
import { ExtensionRegistry, type ExtensionComponent } from "./extensionRegistry.js";

describe("ExtensionRegistry", () => {
  const component = (label: string): ExtensionComponent => {
    void label;
    return () => null;
  };

  it("registers, resolves and lists extension components", () => {
    const registry = new ExtensionRegistry();
    const card = component("card");
    registry.register({ name: "demo", components: { "workbench.card": card } });

    expect(registry.resolve("workbench.card")).toBe(card);
    expect(registry.list().map((entry) => entry.name)).toEqual(["demo"]);
  });

  it("replaces an extension during hot reload and preserves unrelated collisions", () => {
    const registry = new ExtensionRegistry();
    const first = component("first");
    const second = component("second");
    registry.register({ name: "one", components: { slot: first } });
    registry.register({ name: "two", components: { slot: second } });
    registry.register({ name: "one", components: { other: first } });

    expect(registry.resolve("slot")).toBe(second);
    expect(registry.resolve("other")).toBe(first);
    registry.unregister("two");
    expect(registry.resolve("slot")).toBeUndefined();
  });

  it("replaces templates and notifies subscribers", () => {
    const registry = new ExtensionRegistry();
    const listener = vi.fn();
    const unsubscribe = registry.subscribe(listener);
    registry.setTemplates({ "settings.card": { title: "Settings" } });

    expect(registry.resolveTemplate("settings.card")).toEqual({ title: "Settings" });
    expect(listener).toHaveBeenCalledOnce();
    unsubscribe();
    registry.setTemplates({});
    expect(listener).toHaveBeenCalledOnce();
  });

  it("returns the most recently registered layout", () => {
    const registry = new ExtensionRegistry();
    const first = { workbench: "first" };
    const second = { workbench: "second" };
    registry.register({ name: "one", layout: first });
    registry.register({ name: "two", layout: second });
    expect(registry.primaryLayout()).toBe(second);
  });

  it("rejects unnamed extensions", () => {
    const registry = new ExtensionRegistry();
    expect(() => registry.register({ name: "" })).toThrow(/non-empty/);
  });
});
