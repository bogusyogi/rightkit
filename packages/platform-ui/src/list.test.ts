import { describe, expect, it } from "vitest";
import { clampIndex, moveIndex, selectByPointer, typeaheadMatch, type ListNavState } from "./list.js";

describe("clampIndex", () => {
  it("clamps to the item range and gives -1 for an empty list", () => {
    expect(clampIndex(9, 3)).toBe(2);
    expect(clampIndex(-4, 3)).toBe(0);
    expect(clampIndex(0, 0)).toBe(-1);
  });
});

describe("moveIndex", () => {
  it("wrap cycles past both ends", () => {
    expect(moveIndex(2, 3, "next", true)).toBe(0);
    expect(moveIndex(0, 3, "prev", true)).toBe(2);
  });
  it("clamp stays at the ends", () => {
    expect(moveIndex(2, 3, "next", false)).toBe(2);
    expect(moveIndex(0, 3, "prev", false)).toBe(0);
    expect(moveIndex(1, 3, "next", false)).toBe(2);
  });
  it("first and last", () => {
    expect(moveIndex(1, 4, "first", true)).toBe(0);
    expect(moveIndex(1, 4, "last", false)).toBe(3);
  });
  it("skips disabled items, including at the ends", () => {
    const dis = (i: number) => i === 1 || i === 3;
    expect(moveIndex(0, 4, "next", true, dis)).toBe(2);
    expect(moveIndex(2, 4, "next", true, dis)).toBe(0);
    expect(moveIndex(2, 4, "next", false, dis)).toBe(2);
    expect(moveIndex(2, 4, "last", true, dis)).toBe(2);
    expect(moveIndex(2, 4, "first", true, dis)).toBe(0);
  });
  it("leaves the index when every item is disabled and returns -1 for an empty list", () => {
    expect(moveIndex(1, 3, "next", true, () => true)).toBe(1);
    expect(moveIndex(0, 0, "next", true)).toBe(-1);
  });
});

describe("selectByPointer (after-pointer-move)", () => {
  const fresh: ListNavState = { index: 0, pointer: null, keyboardDriving: false };
  it("selects the row under a moving pointer", () => {
    expect(selectByPointer(fresh, 2, { x: 5, y: 5 })).toEqual({ index: 2, pointer: { x: 5, y: 5 }, keyboardDriving: false });
  });
  it("a resting pointer never steals the selection after keyboard navigation", () => {
    const kb: ListNavState = { index: 3, pointer: { x: 5, y: 5 }, keyboardDriving: true };
    expect(selectByPointer(kb, 1, { x: 5, y: 5 })).toBe(kb);
    expect(selectByPointer(kb, 1, { x: 6, y: 5 }).index).toBe(1);
  });
  it("the first event after keyboard navigation only records the pointer", () => {
    const kb: ListNavState = { index: 3, pointer: null, keyboardDriving: true };
    expect(selectByPointer(kb, 1, { x: 5, y: 5 })).toEqual({ index: 3, pointer: { x: 5, y: 5 }, keyboardDriving: true });
  });
  it("an unchanged pointer position is ignored when not keyboard driving", () => {
    const s: ListNavState = { index: 0, pointer: { x: 1, y: 1 }, keyboardDriving: false };
    expect(selectByPointer(s, 4, { x: 1, y: 1 })).toBe(s);
  });
});

describe("typeaheadMatch", () => {
  const labels = ["Copy", "Cut", "Paste", "Close", "Clear"];
  it("finds the next label with the prefix after the active item, wrapping", () => {
    expect(typeaheadMatch(labels, "c", 0)).toBe(1);
    expect(typeaheadMatch(labels, "c", 4)).toBe(0);
    expect(typeaheadMatch(labels, "p", 0)).toBe(2);
  });
  it("a longer prefix can stay on the current item", () => {
    expect(typeaheadMatch(labels, "cl", 3)).toBe(3);
    expect(typeaheadMatch(labels, "cl", 0)).toBe(3);
  });
  it("a repeated character cycles", () => {
    expect(typeaheadMatch(labels, "cc", 0)).toBe(1);
  });
  it("skips disabled items and returns -1 without a match", () => {
    expect(typeaheadMatch(labels, "c", 0, (i) => i === 1)).toBe(3);
    expect(typeaheadMatch(labels, "z", 0)).toBe(-1);
    expect(typeaheadMatch([], "a", 0)).toBe(-1);
  });
});
