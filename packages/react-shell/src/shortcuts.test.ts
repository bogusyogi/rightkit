import { describe, expect, it, vi } from "vitest";
import { canonicalChord, chordFromEvent, createShortcutRegistry, eventMatchesShortcutPrefix, isShortcutPrefixHeld, type ShortcutDef } from "./shortcuts.js";

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

describe("shortcut registry: 0.6.9 additions", () => {
  it("matches the digit pseudo-key with the held modifier and hands the event to run", () => {
    const { registry, run } = fixture("windows", [{ id: "tab", chord: "mod+digit", label: "Switch tab" }]);
    expect(registry.handle(keyEvent({ key: "3", ctrl: true }))).toBe(true);
    expect(run("tab")).toHaveBeenCalledOnce();
    expect(registry.handle(keyEvent({ key: "a", ctrl: true }))).toBe(false);
    expect(registry.handle(keyEvent({ key: "3" }))).toBe(false);
  });

  it("reports a one-press chord that starts a sequence as a prefix conflict", () => {
    const { registry } = fixture("windows", [
      { id: "a", chord: "g", label: "A" },
      { id: "b", chord: "g i", label: "B" },
    ]);
    expect(registry.conflicts()).toEqual([{ chord: "g", ids: ["a", "b"], kind: "prefix" }]);
  });

  it("classifies two ids on one chord as an exact conflict", () => {
    const { registry } = fixture("windows", [
      { id: "a", chord: "mod+k", label: "A" },
      { id: "b", chord: "mod+k", label: "B" },
    ]);
    expect(registry.conflicts()).toEqual([{ chord: canonicalChord("mod+k"), ids: ["a", "b"], kind: "exact" }]);
  });

  it("reports an explicitly unbound shortcut as unassigned, and not after the override is removed", () => {
    const { registry } = fixture("windows", [{ id: "save", chord: "mod+s", label: "Save" }]);
    registry.setOverrides({ save: null });
    expect(registry.list()[0]).toMatchObject({ unassigned: true, chords: [], overridden: true });
    registry.setOverrides({});
    expect(registry.list()[0]).toMatchObject({ unassigned: false, chords: ["mod+s"] });
  });

  it("dispatches nothing while suspended and again after resume", () => {
    const { registry, run } = fixture("windows", [{ id: "save", chord: "mod+s", label: "Save" }]);
    registry.suspend();
    expect(registry.isSuspended()).toBe(true);
    expect(registry.handle(keyEvent({ key: "s", ctrl: true }))).toBe(false);
    registry.resume();
    expect(registry.handle(keyEvent({ key: "s", ctrl: true }))).toBe(true);
    expect(run("save")).toHaveBeenCalledOnce();
  });

  it("builds a chord from a keydown in the registry vocabulary", () => {
    expect(chordFromEvent({ key: "P", code: "KeyP", ctrlKey: true, metaKey: false, altKey: false, shiftKey: true }, "windows")).toBe("mod+shift+p");
    expect(chordFromEvent({ key: "s", code: "KeyS", ctrlKey: false, metaKey: true, altKey: false, shiftKey: false }, "macos")).toBe("mod+s");
    expect(chordFromEvent({ key: "Shift", code: "ShiftLeft", ctrlKey: false, metaKey: false, altKey: false, shiftKey: true }, "macos")).toBeNull();
    expect(chordFromEvent({ key: "?", code: "Slash", ctrlKey: false, metaKey: false, altKey: false, shiftKey: true }, "windows")).toBe("?");
  });

  it("held-modifier prefix: held set, event match, and release cancels", () => {
    const held = new Set(["control"]);
    expect(isShortcutPrefixHeld("mod", held, "windows")).toBe(true);
    expect(isShortcutPrefixHeld("mod", held, "macos")).toBe(false);
    expect(isShortcutPrefixHeld("mod", new Set(["meta"]), "macos")).toBe(true);
    expect(isShortcutPrefixHeld("mod+shift", held, "windows")).toBe(false);
    expect(isShortcutPrefixHeld("g i", held, "windows")).toBe(false);
    expect(eventMatchesShortcutPrefix({ metaKey: false, ctrlKey: true, shiftKey: false, altKey: false }, "mod", held, "windows")).toBe(true);
    expect(eventMatchesShortcutPrefix({ metaKey: false, ctrlKey: true, shiftKey: true, altKey: false }, "mod", held, "windows")).toBe(false);
    expect(eventMatchesShortcutPrefix({ metaKey: true, ctrlKey: false, shiftKey: false, altKey: false }, "mod", held, "windows")).toBe(false);
    held.delete("control"); // modifier release cancels the prefix
    expect(isShortcutPrefixHeld("mod", held, "windows")).toBe(false);
  });

  it("uses the physical key for an Option-composed character", () => {
    expect(chordFromEvent({ key: "Ω", code: "KeyZ", ctrlKey: false, metaKey: false, altKey: true, shiftKey: false }, "macos")).toBe("alt+z");
  });
});
