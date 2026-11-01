import { describe, expect, it, vi } from "vitest";

import { parseGoals } from "./parser";

describe("parseGoals — canonical forms", () => {
  it("parses [goal-XYZ: complete] evidence as a complete update", () => {
    expect(parseGoals("[goal-A: complete] ship landed")).toEqual([
      { id: "A", op: "complete", evidence: "ship landed" },
    ]);
  });

  it("parses [goal-XYZ: block] reason as a block update", () => {
    expect(parseGoals("[goal-B: block] waiting on API key")).toEqual([
      { id: "B", op: "block", reason: "waiting on API key" },
    ]);
  });

  it("parses [goal-XYZ: progress] note as a progress update", () => {
    expect(parseGoals("[goal-C: progress] drafted 3 of 5 sections")).toEqual([
      { id: "C", op: "progress", note: "drafted 3 of 5 sections" },
    ]);
  });

  it("parses a canonical header regardless of op case", () => {
    expect(parseGoals("[goal-A: COMPLETE] done")).toEqual([
      { id: "A", op: "complete", evidence: "done" },
    ]);
    expect(parseGoals("[goal-A: Block] nope")).toEqual([{ id: "A", op: "block", reason: "nope" }]);
  });
});

describe("parseGoals — id variants", () => {
  it("accepts [goal-XYZ:complete] (no space before id) as the same shape", () => {
    expect(parseGoals("[goal-A:complete] shipped")).toEqual([
      { id: "A", op: "complete", evidence: "shipped" },
    ]);
  });

  it("accepts [goal-ABC:complete] with an uppercase id (case-preserved)", () => {
    expect(parseGoals("[goal-ABC:complete] done")).toEqual([
      { id: "ABC", op: "complete", evidence: "done" },
    ]);
  });

  it("accepts [goal-A : complete] with extra spaces inside the header", () => {
    expect(parseGoals("[goal-A : complete] shipped")).toEqual([
      { id: "A", op: "complete", evidence: "shipped" },
    ]);
  });

  it("accepts [  goal-A:complete ] with surrounding whitespace", () => {
    expect(parseGoals("[  goal-A:complete ] shipped")).toEqual([
      { id: "A", op: "complete", evidence: "shipped" },
    ]);
  });
});

describe("parseGoals — malformed input", () => {
  it("returns [] for empty input", () => {
    expect(parseGoals("")).toEqual([]);
    expect(parseGoals("   ")).toEqual([]);
  });

  it("logs a debug message and skips a line with no bracket header", () => {
    const spy = vi.spyOn(console, "debug").mockImplementation(() => {});
    expect(parseGoals("just a plain line")).toEqual([]);
    expect(spy).toHaveBeenCalled();
    spy.mockRestore();
  });

  it("logs a debug message and skips a line with an unknown op", () => {
    const spy = vi.spyOn(console, "debug").mockImplementation(() => {});
    expect(parseGoals("[goal-A: charged] over budget")).toEqual([]);
    expect(spy).toHaveBeenCalled();
    spy.mockRestore();
  });

  it("logs nothing and skips blank lines", () => {
    const spy = vi.spyOn(console, "debug").mockImplementation(() => {});
    expect(parseGoals("\n\n\n")).toEqual([]);
    expect(spy).not.toHaveBeenCalled();
    spy.mockRestore();
  });
});

describe("parseGoals — partial parse + normal turn", () => {
  it("returns valid updates alongside debug logs for malformed lines", () => {
    const spy = vi.spyOn(console, "debug").mockImplementation(() => {});
    const text = [
      "[goal-A: complete] first done",
      "random chatter",
      "[goal-B: progress] halfway",
      "not a goal line either",
      "[goal-C: block] needs review",
    ].join("\n");
    expect(parseGoals(text)).toEqual([
      { id: "A", op: "complete", evidence: "first done" },
      { id: "B", op: "progress", note: "halfway" },
      { id: "C", op: "block", reason: "needs review" },
    ]);
    expect(spy).toHaveBeenCalled();
    spy.mockRestore();
  });

  it("preserves order across multiple updates in a single turn", () => {
    const text = [
      "[goal-A: complete] done",
      "[goal-B: complete] done",
      "[goal-A: progress] earlier note (re-emitted)",
    ].join("\n");
    expect(parseGoals(text)).toEqual([
      { id: "A", op: "complete", evidence: "done" },
      { id: "B", op: "complete", evidence: "done" },
      { id: "A", op: "progress", note: "earlier note (re-emitted)" },
    ]);
  });

  it("treats a normal turn that is entirely chatter as []", () => {
    const spy = vi.spyOn(console, "debug").mockImplementation(() => {});
    expect(parseGoals("Hi there!\nLet's keep going.\n")).toEqual([]);
    expect(spy).toHaveBeenCalled();
    spy.mockRestore();
  });

  it("does not split on a single newline missing the bracket header", () => {
    const spy = vi.spyOn(console, "debug").mockImplementation(() => {});
    expect(parseGoals("hello world")).toEqual([]);
    expect(spy).toHaveBeenCalled();
    spy.mockRestore();
  });
});
