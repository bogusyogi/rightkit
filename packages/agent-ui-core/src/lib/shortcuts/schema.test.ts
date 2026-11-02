import { describe, expect, it } from "vitest";
import {
  getCustomizableShortcutActions,
  getEffectiveShortcutCombo,
  getShortcutAction,
  getShortcutBindingConflicts,
  SHORTCUT_SCHEMA,
} from "./schema.js";
import { parseShortcut } from "./bindings.js";

describe("shortcut schema", () => {
  it("declares unique ids and valid bindings in Settings order", () => {
    const ids = SHORTCUT_SCHEMA.map((action) => action.id);
    expect(new Set(ids).size).toBe(ids.length);
    expect(SHORTCUT_SCHEMA.every((action) => {
      const count = parseShortcut(action.defaultBinding)?.chords.length;
      return Boolean(action.category) && count !== undefined && count >= 1 && count <= 2;
    })).toBe(true);
    expect([...new Set(SHORTCUT_SCHEMA.map((action) => action.category))]).toEqual([
      "session", "models", "panels", "navigation", "application",
    ]);
  });

  it("derives labels for every customizable action", () => {
    const customizable = getCustomizableShortcutActions();
    expect(customizable.length).toBeGreaterThan(0);
    expect(customizable.every((action) => action.settingsLabelKey.endsWith(`${action.id}.label`))).toBe(true);
  });

  it("keeps the mod+k leader and held modifier prefixes", () => {
    expect(getShortcutAction("open_draft_project_picker")?.defaultBinding).toBe("mod+k p");
    expect(getShortcutAction("open_session_list")?.defaultBinding).toBe("mod+k l");
    expect(getShortcutAction("open_help")?.defaultBinding).toBe("mod+k h");
    expect(getShortcutAction("switch_session_tab")?.defaultBinding).toBe("mod");
    expect(getShortcutAction("switch_context_surface")?.defaultBinding).toBe("mod+alt");
  });

  it("preserves valid overrides and falls back from malformed or internal overrides", () => {
    expect(getEffectiveShortcutCombo("new_chat", { new_chat: "mod+k" })).toBe("mod+k");
    expect(getEffectiveShortcutCombo("new_chat", { new_chat: "mod+k x y" })).toBe("mod+n");
    expect(getEffectiveShortcutCombo("save_file", { save_file: "__unassigned__" })).toBe("mod+s");
    expect(getEffectiveShortcutCombo("unknown", { unknown: "mod+z" })).toBe("");
  });

  it("detects exact and prefix conflicts against customizable and internal bindings", () => {
    expect(getShortcutBindingConflicts("new_chat", "mod+p").find((c) => c.action.id === "open_command_palette")?.kind).toBe("exact");
    expect(getShortcutBindingConflicts("new_chat", "mod+f").find((c) => c.action.id === "find_in_file")?.kind).toBe("exact");
    expect(getShortcutBindingConflicts("new_chat", "mod+s x").find((c) => c.action.id === "save_file")?.kind).toBe("prefix");
    expect(getShortcutBindingConflicts("new_chat", "mod+k").find((c) => c.action.id === "open_session_list")?.kind).toBe("prefix");
  });
});

describe("shortcut defaults", () => {
  it("does not assign two actions the same normalized default binding", () => {
    const byBinding = new Map<string, string[]>();
    for (const action of SHORTCUT_SCHEMA) {
      const binding = getEffectiveShortcutCombo(action.id);
      const ids = byBinding.get(binding) ?? [];
      ids.push(action.id);
      byBinding.set(binding, ids);
    }
    expect([...byBinding.values()].filter((ids) => ids.length > 1)).toEqual([]);
  });
});
