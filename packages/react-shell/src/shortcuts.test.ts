import { describe, expect, it, vi } from "vitest";
import { canonicalChord, createShortcutRegistry, type ShortcutDef } from "./shortcuts.js";

interface KeyInit {
  key: string;
  ctrl?: boolean;
  meta?: boolean;
  alt?: boolean;
  shift?: boolean;
  composing?: boolean;
  prevented?: boolean;
}

/** A keydown-shaped object: `handle` reads only these fields and calls `preventDefault`. */
function keyEvent(init: KeyInit): KeyboardEvent {
  const event = {
    key: init.key,
    code: "",
    ctrlKey: init.ctrl ?? false,
    metaKey: init.meta ?? false,
    altKey: init.alt ?? false,
    shiftKey: init.shift ?? false,
    isComposing: init.composing ?? false,
    defaultPrevented: init.prevented ?? false,
    target: null,
    preventDefault() {
      this.defaultPrevented = true;
    },
  };
  return event as unknown as KeyboardEvent;
}

function fixture(platform: "macos" | "windows", defs: (Omit<ShortcutDef, "run"> & { run?: ShortcutDef["run"] })[]) {
  const registry = createShortcutRegistry({ platform });
  const runs = new Map<string, ReturnType<typeof vi.fn>>();
  for (const def of defs) {
    const run = vi.fn(() => undefined);
    runs.set(def.id, run);
    registry.register({ ...def, run: def.run ?? run });
  }
  return { registry, run: (id: string) => runs.get(id)! };
}

describe("shortcut registry: composing and defaultPrevented opt-ins", () => {
  it("keeps ignoring composing and prevented events by default", () => {
    const { registry, run } = fixture("windows", [{ id: "save", chord: "mod+s", label: "Save" }]);
    expect(registry.handle(keyEvent({ key: "s", ctrl: true, composing: true }))).toBe(false);
    expect(registry.handle(keyEvent({ key: "s", ctrl: true, prevented: true }))).toBe(false);
    expect(run("save")).not.toHaveBeenCalled();
  });

  it("fires a shortcut that opts in to allowComposing, and only that one", () => {
    const { registry, run } = fixture("windows", [
      { id: "save", chord: "mod+s", label: "Save", allowComposing: true },
      { id: "open", chord: "mod+o", label: "Open" },
    ]);
    expect(registry.handle(keyEvent({ key: "s", ctrl: true, composing: true }))).toBe(true);
    expect(run("save")).toHaveBeenCalledOnce();
    expect(registry.handle(keyEvent({ key: "o", ctrl: true, composing: true }))).toBe(false);
    expect(run("open")).not.toHaveBeenCalled();
  });

  it("fires a shortcut that opts in to allowDefaultPrevented", () => {
    const { registry, run } = fixture("windows", [
      { id: "close", chord: "mod+w", label: "Close", allowDefaultPrevented: true },
    ]);
    expect(registry.handle(keyEvent({ key: "w", ctrl: true, prevented: true }))).toBe(true);
    expect(run("close")).toHaveBeenCalledOnce();
  });
});

describe("shortcut registry: ctrlormeta modifier", () => {
  it("matches Ctrl or Meta on macOS and Windows, one at a time", () => {
    const mac = fixture("macos", [{ id: "find", chord: "ctrlormeta+k", label: "Find" }]);
    expect(mac.registry.handle(keyEvent({ key: "k", ctrl: true }))).toBe(true);
    expect(mac.registry.handle(keyEvent({ key: "k", meta: true }))).toBe(true);
    expect(mac.run("find")).toHaveBeenCalledTimes(2);

    const win = fixture("windows", [{ id: "find", chord: "CtrlOrMeta+K", label: "Find" }]);
    expect(win.registry.handle(keyEvent({ key: "k", ctrl: true }))).toBe(true);
    expect(win.registry.handle(keyEvent({ key: "k", meta: true }))).toBe(true);
    expect(win.run("find")).toHaveBeenCalledTimes(2);
  });

  it("does not match when neither or both are held, unless extra modifiers are ignored", () => {
    const { registry, run } = fixture("windows", [{ id: "find", chord: "ctrlormeta+k", label: "Find" }]);
    expect(registry.handle(keyEvent({ key: "k" }))).toBe(false);
    expect(registry.handle(keyEvent({ key: "k", ctrl: true, meta: true }))).toBe(false);
    expect(run("find")).not.toHaveBeenCalled();
  });

  it("keeps the ctrlormeta chord distinct from mod in canonical form", () => {
    expect(canonicalChord("CtrlOrMeta+K")).toBe("ctrlormeta+k");
    expect(canonicalChord("ctrlormeta+k")).not.toBe(canonicalChord("mod+k"));
  });

  it("fires in a text field because the chord carries a modifier, as mod chords do", () => {
    const { registry, run } = fixture("windows", [{ id: "a", chord: "ctrlormeta+k", label: "A" }]);
    const ev = keyEvent({ key: "k", ctrl: true });
    Object.defineProperty(ev, "target", { value: { closest: () => ({}) } });
    expect(registry.handle(ev)).toBe(true);
    expect(run("a")).toHaveBeenCalledOnce();
  });
});

describe("shortcut registry: ignoreExtraModifiers", () => {
  it("matches an AltGr chord (Ctrl+Alt) when the shortcut ignores extra modifiers", () => {
    const strict = fixture("windows", [{ id: "q", chord: "ctrl+k", label: "K" }]);
    expect(strict.registry.handle(keyEvent({ key: "k", ctrl: true, alt: true }))).toBe(false);

    const loose = fixture("windows", [{ id: "q", chord: "ctrl+k", label: "K", ignoreExtraModifiers: true }]);
    expect(loose.registry.handle(keyEvent({ key: "k", ctrl: true, alt: true }))).toBe(true);
    expect(loose.run("q")).toHaveBeenCalledOnce();
  });

  it("fires a bare key with any modifier held", () => {
    const { registry, run } = fixture("macos", [{ id: "esc", chord: "esc", label: "Close", ignoreExtraModifiers: true }]);
    expect(registry.handle(keyEvent({ key: "Escape", shift: true }))).toBe(true);
    expect(registry.handle(keyEvent({ key: "Escape", meta: true, alt: true }))).toBe(true);
    expect(registry.handle(keyEvent({ key: "Escape" }))).toBe(true);
    expect(run("esc")).toHaveBeenCalledTimes(3);
  });

  it("still requires the named modifiers and does not fire on a different key", () => {
    const { registry, run } = fixture("windows", [{ id: "k", chord: "mod+k", label: "K", ignoreExtraModifiers: true }]);
    expect(registry.handle(keyEvent({ key: "k" }))).toBe(false);
    expect(registry.handle(keyEvent({ key: "j", ctrl: true, shift: true }))).toBe(false);
    expect(registry.handle(keyEvent({ key: "k", ctrl: true, shift: true }))).toBe(true);
    expect(run("k")).toHaveBeenCalledOnce();
  });

  it("keeps exact matching by default", () => {
    const { registry, run } = fixture("windows", [{ id: "k", chord: "mod+k", label: "K" }]);
    expect(registry.handle(keyEvent({ key: "k", ctrl: true, shift: true }))).toBe(false);
    expect(run("k")).not.toHaveBeenCalled();
  });
});
