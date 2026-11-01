import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import AwaySummaryCard from "./AwaySummaryCard.js";
import type { AwaySummary } from "@rightkit/agent-ui-core/lib/types";

function baseSummary(overrides: Partial<AwaySummary> = {}): AwaySummary {
  return {
    sessionId: "s1",
    sinceIndex: 0,
    nextIndex: 5,
    messageCount: 5,
    userMessages: 2,
    assistantMessages: 2,
    toolResults: 1,
    errorCount: 0,
    latestAt: "2026-07-01T00:00:00Z",
    hasActivity: true,
    bullets: [],
    ...overrides,
  };
}

describe("AwaySummaryCard", () => {
  it("renders nothing when there is no summary and none is available", () => {
    const html = renderToStaticMarkup(
      <AwaySummaryCard summary={null} pending={false} available={false} onRefresh={() => {}} />,
    );
    expect(html).toBe("");
  });

  it("shows the update count when the summary has activity", () => {
    const summary = baseSummary({ messageCount: 7 });
    const html = renderToStaticMarkup(
      <AwaySummaryCard summary={summary} pending={false} available={true} onRefresh={() => {}} />,
    );

    expect(html).toContain("Return digest");
    expect(html).toContain("7 updates since the last digest");
  });

  it("prompts to summarize when new activity is available but no summary loaded yet", () => {
    const html = renderToStaticMarkup(
      <AwaySummaryCard summary={null} pending={false} available={true} onRefresh={() => {}} />,
    );

    expect(html).toContain("New session activity is ready to summarize");
  });

  it("renders the refresh button as disabled and labeled 'Refreshing' while pending", () => {
    const summary = baseSummary();
    const html = renderToStaticMarkup(
      <AwaySummaryCard summary={summary} pending={true} available={true} onRefresh={() => {}} />,
    );

    expect(html).toContain('disabled=""');
    expect(html).toContain("Refreshing");
  });

  it("lists each bullet from the summary", () => {
    const summary = baseSummary({ bullets: ["Fixed the build", "Reviewed the diff"] });
    const html = renderToStaticMarkup(
      <AwaySummaryCard summary={summary} pending={false} available={true} onRefresh={() => {}} />,
    );

    expect(html).toContain("Fixed the build");
    expect(html).toContain("Reviewed the diff");
  });
});
