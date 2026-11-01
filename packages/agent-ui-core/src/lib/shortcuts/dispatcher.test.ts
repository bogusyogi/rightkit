import { createShortcutRegistry } from "@rightkit/react-shell";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cancelPendingShortcut, registerShortcut, syncShortcutBindings } from "./dispatcher";

function key(keyValue: string, options: Partial<KeyboardEvent> = {}): KeyboardEvent {
  return {
    key: keyValue,
    code: /^[a-z]$/i.test(keyValue) ? `Key${keyValue.toUpperCase()}` : keyValue,
    altKey: false,
    ctrlKey: false,
    metaKey: false,
    shiftKey: false,
    repeat: false,
    isComposing: false,
    defaultPrevented: false,
    target: null,
    preventDefault() {},
    ...options,
  } as KeyboardEvent;
}

const makeRegistry = () => createShortcutRegistry({ platform: "windows", sequenceTimeout: 3000 });

describe("App shortcuts on the RightKit registry", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
  });
  afterEach(() => vi.useRealTimers());

  it("dispatches schema sequences and re-matches after a mismatch", () => {
    const registry = makeRegistry();
    const calls: string[] = [];
    registerShortcut({ id: "open_help", run: () => { calls.push("help"); }, label: "Help" }, registry);
    registerShortcut({ id: "shell.x", chord: "x", run: () => { calls.push("x"); }, label: "X" }, registry);
    expect(registry.handle(key("k", { ctrlKey: true }))).toBe(true);
    expect(registry.handle(key("x"))).toBe(true);
    expect(calls).toEqual(["x"]);
    expect(registry.handle(key("k", { ctrlKey: true }))).toBe(true);
    expect(registry.handle(key("h"))).toBe(true);
    expect(calls).toEqual(["x", "help"]);
  });

  it("expires a sequence after three seconds and clears it on cancel", () => {
    const registry = makeRegistry();
    const calls: string[] = [];
    registerShortcut({ id: "open_help", run: () => { calls.push("help"); }, label: "Help" }, registry);
    registry.handle(key("k", { ctrlKey: true }));
    vi.setSystemTime(2999);
    expect(registry.handle(key("h"))).toBe(true);
    registry.handle(key("k", { ctrlKey: true }));
    vi.setSystemTime(2999 + 3001);
    expect(registry.handle(key("h"))).toBe(false);
    registry.handle(key("k", { ctrlKey: true }));
    cancelPendingShortcut(registry);
    expect(registry.handle(key("h"))).toBe(false);
    expect(calls).toEqual(["help"]);
  });

  it("fires bare keys while typing", () => {
    const registry = makeRegistry();
    const calls: string[] = [];
    registerShortcut({ id: "shell.help", chord: "?", run: () => { calls.push("?"); }, label: "Help" }, registry);
    const typing = { closest: () => ({}) } as unknown as EventTarget;
    expect(registry.handle(key("?", { shiftKey: true, target: typing }))).toBe(true);
    expect(calls).toEqual(["?"]);
  });

  it("does not consume a binding when its handler declines", () => {
    const registry = makeRegistry();
    registerShortcut({ id: "shell.x", chord: "x", run: () => false, label: "X" }, registry);
    expect(registry.handle(key("x"))).toBe(false);
  });

  it("never reports a repeat as handled where the binding would refuse the key", () => {
    const registry = makeRegistry();
    const prevented: string[] = [];
    const press = (options: Partial<KeyboardEvent>) =>
      registry.handle(key("x", { ...options, preventDefault: () => { prevented.push("x"); } }));
    // A binding that leaves the key to the browser: a first press is refused, and so is a repeat, with or without opt-in.
    registerShortcut({ id: "shell.refuses", chord: "x", run: () => false, label: "Refuses" }, registry);
    expect(press({})).toBe(false);
    expect(press({ repeat: true })).toBe(false);
    expect(prevented).toEqual([]);
    const optIn = makeRegistry();
    registerShortcut({ id: "shell.refuses-held", chord: "x", repeat: true, run: () => false, label: "Refuses held" }, optIn);
    expect(optIn.handle(key("x", { repeat: true }))).toBe(false);
  });

  it("resolves stored overrides, unassigned bindings and conflicts", () => {
    const registry = makeRegistry();
    const calls: string[] = [];
    registerShortcut({ id: "open_settings", run: () => { calls.push("settings"); }, label: "Settings" }, registry);
    expect(registry.handle(key(",", { ctrlKey: true }))).toBe(true);
    syncShortcutBindings({ open_settings: "mod+y" }, registry);
    expect(registry.handle(key(",", { ctrlKey: true }))).toBe(false);
    expect(registry.handle(key("y", { ctrlKey: true }))).toBe(true);
    syncShortcutBindings({ open_settings: "__unassigned__" }, registry);
    expect(registry.list()[0].unassigned).toBe(true);
    expect(registry.handle(key("y", { ctrlKey: true }))).toBe(false);
    syncShortcutBindings({}, registry);
    expect(registry.overrides()).toEqual({});
    expect(calls).toEqual(["settings", "settings"]);

    registerShortcut({ id: "open_help", run: () => undefined, label: "Help" }, registry);
    syncShortcutBindings({ open_help: "mod+k" }, registry);
    expect(registry.conflicts()).toEqual([]);
    registerShortcut({ id: "shell.k", chord: "mod+k", run: () => undefined, label: "K" }, registry);
    expect(registry.conflicts().map((c) => c.kind)).toEqual(["exact"]);
  });

  it("decides auto-repeat per binding: ignored by default, runs when the binding opts in", () => {
    const registry = makeRegistry();
    const calls: string[] = [];
    registerShortcut({ id: "shell.once", chord: "mod+j", run: () => { calls.push("once"); }, label: "Once" }, registry);
    registerShortcut({ id: "shell.held", chord: "mod+l", repeat: true, run: () => { calls.push("held"); }, label: "Held" }, registry);
    expect(registry.handle(key("j", { ctrlKey: true }))).toBe(true);
    // A repeat of a binding that did not opt in neither runs nor counts as handled (so it is not default-prevented).
    expect(registry.handle(key("j", { ctrlKey: true, repeat: true }))).toBe(false);
    expect(registry.handle(key("l", { ctrlKey: true }))).toBe(true);
    expect(registry.handle(key("l", { ctrlKey: true, repeat: true }))).toBe(true);
    expect(registry.handle(key("l", { ctrlKey: true, repeat: true }))).toBe(true);
    expect(calls).toEqual(["once", "held", "held", "held"]);
  });
});
