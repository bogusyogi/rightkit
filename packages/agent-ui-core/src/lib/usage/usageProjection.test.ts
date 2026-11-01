import { describe, expect, it } from "vitest";
import {
  buildUsageWeekGrid,
  formatCost,
  formatTokenCount,
  getDailyValue,
  getMaxDailyValue,
  sortUsageRuns,
  type DailyAggregate,
  type UsageRunSummary,
} from "./usageProjection";

const day = (date: string, inputTokens: number, outputTokens = 0): DailyAggregate => ({
  date,
  costUsd: inputTokens / 100_000,
  costComplete: true,
  runs: 1,
  inputTokens,
  outputTokens,
});

const run = (runId: string, startedAt: string, tokens: number, turns: number): UsageRunSummary => ({
  runId,
  name: runId,
  agent: "coderight",
  status: "completed",
  startedAt,
  totalCostUsd: tokens / 10_000,
  costAvailable: true,
  inputTokens: tokens,
  outputTokens: 0,
  durationMs: 10,
  numTurns: turns,
});

describe("usageProjection", () => {
  it("keeps OpenCovibe's compact token and cost presentation", () => {
    expect(formatTokenCount(1_250)).toBe("1.3k");
    expect(formatTokenCount(2_000_000)).toBe("2.0m");
    expect(formatCost(0.1234)).toBe("$0.1234");
    expect(formatCost(1.2)).toBe("$1.20");
  });

  it("projects chart values and a stable maximum", () => {
    const days = [day("2026-08-01", 100, 20), day("2026-08-02", 500, 50)];
    expect(getDailyValue(days[0], "tokens")).toBe(120);
    expect(getDailyValue({ ...days[0], costComplete: false }, "cost")).toBe(0);
    expect(getMaxDailyValue(days, "tokens")).toBe(550);
  });

  it("sorts runs by the donor's selected column and direction", () => {
    const runs = [run("old", "2026-08-01T00:00:00Z", 100, 1), run("new", "2026-08-02T00:00:00Z", 200, 2)];
    expect(sortUsageRuns(runs, "date", false).map((item) => item.runId)).toEqual(["new", "old"]);
    expect(sortUsageRuns(runs, "tokens", true).map((item) => item.runId)).toEqual(["old", "new"]);
  });

  it("builds a right-aligned UTC seven-row heatmap", () => {
    const grid = buildUsageWeekGrid(
      [day("2026-08-24", 100)],
      "tokens",
      2,
      new Date("2026-08-25T12:00:00Z"),
    );
    expect(grid.weeks).toBe(2);
    expect(grid.cells).toHaveLength(7);
    expect(grid.cells.flat().some((cell) => cell?.date === "2026-08-24")).toBe(true);
  });
});
