import { describe, expect, it } from "vitest";
import {
  clampComposerRows,
  composerMaxHeight,
  insertedTextBetween,
  writeComposerValue,
  isComposerComposing,
  shouldSubmitComposerEnter,
} from "./editorProjection";

describe("composer editor projection", () => {
  it("reports inserted text for typing, replacement, and deletion", () => {
    expect(insertedTextBetween("abc", "abXc")).toBe("X");
    expect(insertedTextBetween("abc", "aZ")).toBe("Z");
    expect(insertedTextBetween("abc", "ac")).toBe("");
  });

  it("keeps the donor Enter and IME contract", () => {
    expect(shouldSubmitComposerEnter({ key: "Enter", shiftKey: false, isComposing: false, keyCode: 13 })).toBe(true);
    expect(shouldSubmitComposerEnter({ key: "Enter", shiftKey: true, isComposing: false, keyCode: 13 })).toBe(false);
    expect(shouldSubmitComposerEnter({ key: "Enter", shiftKey: false, isComposing: true, keyCode: 13 })).toBe(false);
    expect(isComposerComposing({ isComposing: false, keyCode: 229 })).toBe(true);
  });

  it("does not write through an active composition, then places external rewrites at the end", () => {
    const input = document.createElement("textarea");
    input.value = "ime draft";
    expect(writeComposerValue(input, "parent echo", true)).toBe(false);
    expect(input.value).toBe("ime draft");
    expect(writeComposerValue(input, "parent echo", false)).toBe(true);
    expect(input.selectionStart).toBe("parent echo".length);
    expect(input.selectionEnd).toBe("parent echo".length);
  });

  it("clamps grow sizing and exposes the max-height contract", () => {
    expect(clampComposerRows(0, 8)).toBe(1);
    expect(clampComposerRows(12, 8)).toBe(8);
    expect(composerMaxHeight(8, 22)).toBe("calc(8 * 22px)");
  });
});
