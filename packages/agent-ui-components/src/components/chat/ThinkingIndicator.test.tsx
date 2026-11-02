// @vitest-environment happy-dom
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import ThinkingIndicator, { THINKING_INDICATOR_FORMATTERS } from "./ThinkingIndicator.js";

function lastIndicator(): HTMLElement {
  const nodes = screen.getAllByTestId("thinking-indicator");
  return nodes[nodes.length - 1];
}

describe("ThinkingIndicator", () => {
  it("renders elapsed time + token count + status text", () => {
    render(
      <ThinkingIndicator elapsedSeconds={108} tokenCount={299} status="almost done thinking..." />,
    );
    const node = lastIndicator();
    expect(node.textContent).toContain("1m 48s");
    expect(node.textContent).toContain("299");
    expect(node.textContent).toContain("almost done thinking...");
  });

  it("falls back to 'thinking…' when no status is provided", () => {
    render(<ThinkingIndicator elapsedSeconds={5} tokenCount={120} />);
    expect(lastIndicator().textContent).toContain("thinking…");
  });

  it("formats tokens with k / M suffixes", () => {
    expect(THINKING_INDICATOR_FORMATTERS.formatTokens(299)).toBe("299");
    expect(THINKING_INDICATOR_FORMATTERS.formatTokens(1500)).toBe("1.5k");
    expect(THINKING_INDICATOR_FORMATTERS.formatTokens(2_500_000)).toBe("2.5M");
  });

  it("formats elapsed seconds with m / s segments", () => {
    expect(THINKING_INDICATOR_FORMATTERS.formatElapsed(5)).toBe("5s");
    expect(THINKING_INDICATOR_FORMATTERS.formatElapsed(108)).toBe("1m 48s");
  });
});
