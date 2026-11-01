import { describe, expect, it } from "vitest";

import {
  initialHighlightIndex,
  orderedTaskIds,
  taskSwitcherColumnCount,
  TaskSwitcherUi,
} from "./taskSwitcher";

describe("TaskSwitcherUi", () => {
  it("contains only the ten most recently visited started tasks", () => {
    const current = "current";
    const recent = Array.from({ length: 12 }, (_, index) => `recent-${index}`);
    const unvisited = "unvisited";
    const missing = "missing";
    const started = [unvisited, current, ...recent];
    const recorded = [missing, ...recent];
    const expected = [current, ...recent.slice(0, 9)];

    expect(orderedTaskIds(current, recorded, started)).toEqual(expected);
  });

  it("first control-tab targets the previous task and reverse wraps", () => {
    const ordered = ["current", "previous", "oldest"];
    expect(initialHighlightIndex(ordered, "current", false)).toBe(1);
    expect(initialHighlightIndex(ordered, "current", true)).toBe(2);
  });

  it("single current task still opens the switcher", () => {
    expect(initialHighlightIndex(["current"], "current", false)).toBe(0);
  });

  it("a draft can switch to the only visited started task", () => {
    const ordered = orderedTaskIds("draft", ["draft", "started"], ["started"]);
    expect(ordered).toEqual(["started"]);
    expect(initialHighlightIndex(ordered, "draft", false)).toBe(0);
  });

  it("caps the grid at ten tasks and two rows", () => {
    expect(taskSwitcherColumnCount(20, 1400)).toBe(5);
    expect(taskSwitcherColumnCount(10, 520)).toBe(5);
    expect(taskSwitcherColumnCount(4, 520)).toBe(2);
    expect(taskSwitcherColumnCount(4, 180)).toBe(2);
  });

  it("snapshots order, cycles highlight, and commits once", () => {
    const switcher = new TaskSwitcherUi();
    switcher.recordAccess("previous");
    switcher.recordAccess("current");
    expect(switcher.openSwitcher("current", ["current", "previous", "oldest"])).toBe(true);
    expect(switcher.snapshot.orderedSessionIds).toEqual(["current", "previous"]);
    switcher.cycle();
    expect(switcher.snapshot.highlightedSessionId).toBe("current");
    expect(switcher.finish(false, "current", ["current", "previous"])).toBe("current");
    expect(switcher.isOpen).toBe(false);
  });
});
