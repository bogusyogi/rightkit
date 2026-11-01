import { describe, expect, it } from "vitest";
import {
  formatShortcutForDisplay,
  getShortcutConflict,
  isRiskyBrowserShortcut,
  normalizeCombo,
  parseShortcut,
  toRightKitChord,
  UNASSIGNED_SHORTCUT,
} from "./bindings";
import { eventMatchesShortcutPrefix, isShortcutPrefixHeld } from "./prefixChords";

const keydown = (key: string, mods: { meta?: boolean; ctrl?: boolean; shift?: boolean; alt?: boolean } = {}) => ({
  key,
  code: /^[a-z]$/i.test(key) ? `Key${key.toUpperCase()}` : key,
  metaKey: mods.meta ?? false,
  ctrlKey: mods.ctrl ?? false,
  shiftKey: mods.shift ?? false,
  altKey: mods.alt ?? false,
}) as KeyboardEvent;

describe("shortcut bindings", () => {
  it("normalizes, parses, and formats up to two chords", () => {
    expect(normalizeCombo(" command + S   P ")).toBe("mod+s p");
    expect(parseShortcut("mod+s p")?.chords).toHaveLength(2);
    expect(formatShortcutForDisplay("mod+s p", "Unassigned", "other")).toBe("Ctrl + S, P");
  });

  it("rejects bindings with more than two chords and handles unassignment", () => {
    expect(normalizeCombo("mod+s p q")).toBe("");
    expect(parseShortcut("mod+s p q")).toBeUndefined();
    expect(formatShortcutForDisplay(UNASSIGNED_SHORTCUT)).toBe("Unassigned");
    expect(toRightKitChord(UNASSIGNED_SHORTCUT)).toBeNull();
  });

  it("converts stored combos to the RightKit chord vocabulary", () => {
    expect(toRightKitChord("mod+comma")).toBe("mod+,");
    expect(toRightKitChord("mod+k t")).toBe("mod+k t");
    expect(toRightKitChord("mod+alt+arrowleft")).toBe("mod+alt+arrowleft");
    expect(toRightKitChord("ctrl+]")).toBe("ctrl+]");
  });

  it("reports exact and prefix conflicts but allows sibling sequences", () => {
    expect(getShortcutConflict("mod+s", "mod+s")).toBe("exact");
    expect(getShortcutConflict("mod+s", "mod+s p")).toBe("prefix");
    expect(getShortcutConflict("mod+s p", "mod+s")).toBe("prefix");
    expect(getShortcutConflict("mod+s p", "mod+s q")).toBeUndefined();
    expect(getShortcutConflict("mod+alt", "mod+alt+1")).toBeUndefined();
  });

  it("warns for risky browser shortcuts and formats platform modifiers", () => {
    expect(isRiskyBrowserShortcut("mod+s p")).toBe(true);
    expect(formatShortcutForDisplay("mod+ctrl+shift+alt+n", "Unassigned", "macos")).toBe("⌘ + ⌃ + ⇧ + ⌥ + N");
    expect(formatShortcutForDisplay("alt", "Unassigned", "other")).toBe("Alt");
  });
});

// Held-modifier prefix chords come from @rightkit/react-shell 0.7.1 (bogusyogi/rightkit#655 closed for these helpers)
describe("held-modifier prefix chords", () => {
  it("matches and inspects held shortcut prefixes", () => {
    expect(eventMatchesShortcutPrefix(keydown("1", { ctrl: true }), "mod")).toBe(true);
    expect(eventMatchesShortcutPrefix(keydown("1"), "mod")).toBe(false);
    expect(eventMatchesShortcutPrefix(keydown("1", { ctrl: true }), "mod+p", new Set(["control", "p"]))).toBe(true);
    expect(isShortcutPrefixHeld("mod+p", new Set(["control", "p"]))).toBe(true);
    expect(isShortcutPrefixHeld(UNASSIGNED_SHORTCUT, new Set(["control"]))).toBe(false);
  });
});
