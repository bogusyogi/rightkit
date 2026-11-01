import { describe, expect, it } from "vitest";
import { BUILT_IN_STYLES, mergeStyles, validateCustomStyle } from "./styles.js";

describe("prompt template styles", () => {
  it("ports the fourteen five-level built-ins with unique IDs", () => {
    expect(BUILT_IN_STYLES).toHaveLength(14);
    expect(new Set(BUILT_IN_STYLES.map((style) => style.id)).size).toBe(14);
    for (const style of BUILT_IN_STYLES) expect(style.levels).toHaveLength(5);
  });

  it("merges, clamps, and validates style selections", () => {
    const concise = BUILT_IN_STYLES.find((style) => style.id === "concise")!;
    expect(mergeStyles([], BUILT_IN_STYLES)).toBe("");
    expect(mergeStyles([{ id: "concise", level: 1 }], BUILT_IN_STYLES)).toBe(concise.levels[0]);
    expect(mergeStyles([{ id: "concise", level: 10 }, { id: "unknown", level: 1 }], BUILT_IN_STYLES)).toBe(concise.levels[4]);
    expect(validateCustomStyle({ label: "", levels: ["a", "b", "c", "d", "e"] }, BUILT_IN_STYLES).valid).toBe(false);
    expect(validateCustomStyle({ label: "concise", levels: ["a", "b", "c", "d", "e"] }, BUILT_IN_STYLES).valid).toBe(false);
    expect(validateCustomStyle({ label: "Unique", levels: ["a", "b", "c", "d", "e"] }, BUILT_IN_STYLES).valid).toBe(true);
  });
});
