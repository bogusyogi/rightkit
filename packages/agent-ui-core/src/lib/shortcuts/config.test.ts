import { describe, expect, it } from "vitest";
import { SHORTCUT_SCHEMA } from "./config";
import { normalizeCombo, parseShortcut } from "./bindings";

describe("shortcut config", () => {
  it("keeps the declarative inventory grouped and bounded", () => {
    expect(SHORTCUT_SCHEMA.length).toBeGreaterThan(0);
    expect(SHORTCUT_SCHEMA.every((action) => {
      const parsed = parseShortcut(action.defaultBinding);
      return parsed !== undefined && parsed.chords.length <= 2;
    })).toBe(true);
    const categories = SHORTCUT_SCHEMA.map((action) => action.category);
    expect([...new Set(categories)]).toEqual(["session", "models", "panels", "navigation", "application"]);
    expect(categories.every((category, index) => index === 0 || category === categories[index - 1] || categories.indexOf(category) === index)).toBe(true);
  });

  it("uses normalized defaults and unique ids", () => {
    expect(SHORTCUT_SCHEMA.every((action) => normalizeCombo(action.defaultBinding) === action.defaultBinding)).toBe(true);
    const ids = SHORTCUT_SCHEMA.map((action) => action.id);
    expect(new Set(ids).size).toBe(ids.length);
  });
});
