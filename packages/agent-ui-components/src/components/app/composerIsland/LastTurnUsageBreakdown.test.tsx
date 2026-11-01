// @vitest-environment happy-dom
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import LastTurnUsageBreakdown from "./LastTurnUsageBreakdown.js";

const usage = {
  inputTokens: 1_200,
  outputTokens: 300,
  cacheReadTokens: 100,
  cacheCreationTokens: 50,
  costUsd: 0.1234,
  contextTokens: 1_500,
  contextPercent: 0.42,
};

describe("LastTurnUsageBreakdown", () => {
  it("renders the App input/output delta and live context reading", () => {
    const html = renderToStaticMarkup(<LastTurnUsageBreakdown usage={usage} />);
    expect(html).toContain("Last turn");
    expect(html).toContain("In 1.2k");
    expect(html).toContain("Out 300");
    expect(html).toContain("Cache read 100");
    expect(html).toContain("Cache write 50");
    expect(html).toContain("Est. $0.1234");
    expect(html).toContain("Context 1.5k");
    expect(html).toContain("42%");
  });

  it("does not render an empty zero-value turn", () => {
    const html = renderToStaticMarkup(
      <LastTurnUsageBreakdown
        usage={{
          inputTokens: 0,
          outputTokens: 0,
          cacheReadTokens: 0,
          cacheCreationTokens: 0,
          costUsd: 0,
          contextTokens: 0,
          contextPercent: 0,
        }}
      />,
    );
    expect(html).toBe("");
  });
});
