import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { ContextUsageMeterInline } from "./ContextUsageMeterInline";

const baseProps = {
  contextPercent: 0,
  contextTokens: 0,
  promptTokens: 0,
  completionTokens: 0,
  cacheReadTokens: 0,
  cacheCreationTokens: 0,
  costUsd: 0,
};

describe("ContextUsageMeterInline", () => {
  it("renders the explicit breakdown once usage arrives", () => {
    const html = renderToStaticMarkup(
      <ContextUsageMeterInline
        {...baseProps}
        contextPercent={0.42}
        promptTokens={1200}
        completionTokens={300}
        cacheReadTokens={500}
        cacheCreationTokens={100}
        contextTokens={2000}
        costUsd={0.1234}
      />,
    );

    expect(html).toContain("42%");
    expect(html).toContain("Prompt 1.2k");
    expect(html).toContain("Completion 300");
    expect(html).toContain("Cache 600");
    expect(html).toContain("Context 2.0k");
    expect(html).toContain("$0.1234");
  });

  it("omits the Context label when context tokens are not yet reported", () => {
    const html = renderToStaticMarkup(
      <ContextUsageMeterInline {...baseProps} contextPercent={0.1} promptTokens={500} />,
    );
    expect(html).not.toContain("Context 0");
    expect(html).toContain("Prompt 500");
  });

  it("never carries donor auto-compact or capacity copy without supporting fields", () => {
    // The inline mirror must not invent capacity, threshold, or remaining
    // copy that the live usageUpdated payload does not carry.
    const html = renderToStaticMarkup(
      <ContextUsageMeterInline
        {...baseProps}
        contextPercent={0.9}
        promptTokens={2000}
        contextTokens={2000}
        costUsd={0.5}
      />,
    );
    expect(html).not.toContain("tokens remaining");
    expect(html).not.toContain("Auto-compact");
  });

  it("clamps an out-of-range context percent to 0-100", () => {
    const html = renderToStaticMarkup(
      <ContextUsageMeterInline {...baseProps} contextPercent={1.5} promptTokens={10} />,
    );
    expect(html).toContain('aria-valuenow="100"');
  });

  it("clamps a negative context percent up to 0", () => {
    const html = renderToStaticMarkup(
      <ContextUsageMeterInline {...baseProps} contextPercent={-0.2} promptTokens={10} />,
    );
    expect(html).toContain('aria-valuenow="0"');
  });

  it("uses the accent-2 tone below the 80 percent warning inflection", () => {
    const html = renderToStaticMarkup(
      <ContextUsageMeterInline {...baseProps} contextPercent={0.3} promptTokens={10} />,
    );
    expect(html).toContain("bg-accent-2");
  });
});
