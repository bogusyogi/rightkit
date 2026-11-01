// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import UsageDashboard from "./UsageDashboard.js";
import type { UsageDataSource, UsageOverview } from "@rightkit/agent-ui-core/lib/usage/usageProjection";

const overview: UsageOverview = {
  totalCostUsd: 1.25,
  costComplete: true,
  totalTokens: 15_000,
  totalRuns: 3,
  avgCostPerRun: 0.416,
  activeDays: 2,
  currentStreak: 2,
  longestStreak: 4,
  daily: [
    { date: "2026-08-28", costUsd: 0.5, costComplete: true, runs: 1, inputTokens: 5_000, outputTokens: 500 },
  ],
  byModel: [
    { model: "z-ai/glm-5.2", runs: 3, inputTokens: 10_000, outputTokens: 5_000, cacheReadTokens: 0, cacheWriteTokens: 0, costUsd: 1.25, costComplete: true, pct: 100 },
  ],
  runs: [],
};

function source(): UsageDataSource {
  return {
    getOverview: vi.fn().mockResolvedValue(overview),
    getHeatmapDaily: vi.fn().mockResolvedValue(overview.daily),
    clearCache: vi.fn().mockResolvedValue(undefined),
  };
}

afterEach(cleanup);

describe("UsageDashboard", () => {
  it("ports the overview summary and model table through the injected connector", async () => {
    const dataSource = source();
    render(<UsageDashboard dataSource={dataSource} />);

    await waitFor(() => expect(screen.getByText("Total cost")).toBeTruthy());
    expect(screen.getAllByText("$1.25")).toHaveLength(2);
    expect(screen.getByText("15.0k")).toBeTruthy();
    expect(screen.getByText("z-ai/glm-5.2")).toBeTruthy();
    expect(dataSource.getOverview).toHaveBeenCalledWith("global", undefined);
  });

  it("changes scope and requests the corresponding overview", async () => {
    const dataSource = source();
    render(<UsageDashboard dataSource={dataSource} />);
    await waitFor(() => expect(screen.getByText("Total cost")).toBeTruthy());

    fireEvent.click(screen.getByRole("tab", { name: "This app" }));
    await waitFor(() => expect(dataSource.getOverview).toHaveBeenCalledWith("app", undefined));
    expect(screen.getByRole("tab", { name: "This app" })).toHaveAttribute("aria-selected", "true");
  });

  it("refreshes through the optional cache boundary without inventing persistence", async () => {
    const dataSource = source();
    render(<UsageDashboard dataSource={dataSource} />);
    await waitFor(() => expect(screen.getByText("Total cost")).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "Refresh usage" }));
    await waitFor(() => expect(dataSource.clearCache).toHaveBeenCalledTimes(1));
  });

  it("sorts the run history through the published table header and reports aria-sort", async () => {
    const run = (runId: string, name: string, inputTokens: number) => ({
      runId, name, agent: "coderight", status: "completed", startedAt: "2026-08-28T10:00:00Z",
      totalCostUsd: 0.1, costAvailable: true, inputTokens, outputTokens: 0, durationMs: 1, numTurns: 1,
    });
    const dataSource = source();
    dataSource.getOverview = vi.fn().mockResolvedValue({ ...overview, runs: [run("a", "small run", 10), run("b", "large run", 900)] });
    render(<UsageDashboard dataSource={dataSource} />);
    await waitFor(() => expect(screen.getByText("Total cost")).toBeTruthy());
    fireEvent.click(screen.getByRole("tab", { name: "This app" }));
    const table = await screen.findByRole("table", { name: "Usage history" });
    const tokens = () => screen.getByRole("columnheader", { name: /Tokens/ });
    const order = () => Array.from(table.querySelectorAll("tbody tr")).map((row) => row.textContent ?? "");

    expect(screen.getByRole("columnheader", { name: /Date/ })).toHaveAttribute("aria-sort", "descending");
    expect(tokens()).toHaveAttribute("aria-sort", "none");
    fireEvent.click(tokens().querySelector("button") as HTMLButtonElement);
    expect(tokens()).toHaveAttribute("aria-sort", "descending");
    expect(order()[0]).toContain("large run");
    fireEvent.click(tokens().querySelector("button") as HTMLButtonElement);
    expect(tokens()).toHaveAttribute("aria-sort", "ascending");
    expect(order()[0]).toContain("small run");
  });
});
