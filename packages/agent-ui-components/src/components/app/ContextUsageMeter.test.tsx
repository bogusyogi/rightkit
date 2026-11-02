import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import ContextUsageMeter from "./ContextUsageMeter.js";

const baseProps = {
  contextPercent: 0,
  contextTokens: 0,
  promptTokens: 0,
  completionTokens: 0,
  cacheReadTokens: 0,
  cacheCreationTokens: 0,
  costUsd: 0,
};

describe("ContextUsageMeter", () => {
  it("renders nothing before the first usage update", () => {
    const html = renderToStaticMarkup(<ContextUsageMeter {...baseProps} />);
    expect(html).toBe("");
  });

  it("renders a cost-only update with a calm 0% bar", () => {
    // A positive cost with zero tokens is unusual but still counts as a
    // usage update — the meter shows the calm band so the user can see the
    // spend starting without a half-populated breakdown row.
    const html = renderToStaticMarkup(<ContextUsageMeter {...baseProps} costUsd={0.0001} />);
    expect(html).toContain("0%");
    expect(html).toContain("Est. $0.0001");
    expect(html).toContain("bg-accent-2");
  });

  it("shows the rounded percent and token breakdown once usage arrives", () => {
    const html = renderToStaticMarkup(
      <ContextUsageMeter
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
    // Older usage payloads report prompt/completion but not contextTokens.
    // The meter must not fabricate a "Context 0" row in that case.
    const html = renderToStaticMarkup(
      <ContextUsageMeter {...baseProps} contextPercent={0.1} promptTokens={500} />,
    );
    expect(html).not.toContain("Context 0");
    expect(html).toContain("Prompt 500");
  });

  it("never carries donor auto-compact or capacity copy without supporting fields", () => {
    // The live usageUpdated payload does not carry context-window capacity
    // or auto-compact threshold, so the meter must not invent
    // "X tokens remaining" or "Auto-compact at Y%" copy.
    const html = renderToStaticMarkup(
      <ContextUsageMeter
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

  it("clamps an out-of-range context percent to the 0-100 bar", () => {
    const html = renderToStaticMarkup(
      <ContextUsageMeter {...baseProps} contextPercent={1.5} promptTokens={10} />,
    );

    expect(html).toContain('aria-valuenow="100"');
    expect(html).toContain('bg-accent"');
  });

  it("clamps a negative context percent up to 0", () => {
    const html = renderToStaticMarkup(
      <ContextUsageMeter {...baseProps} contextPercent={-0.4} promptTokens={10} />,
    );
    expect(html).toContain('aria-valuenow="0"');
    expect(html).toContain("0%");
  });

  it("uses the warning tone once usage passes 50 percent", () => {
    const html = renderToStaticMarkup(
      <ContextUsageMeter {...baseProps} contextPercent={0.6} promptTokens={10} />,
    );

    expect(html).toContain("bg-warning");
  });

  it("keeps the accent tone at exactly the 50 percent warning threshold boundary", () => {
    // The donor shows a 50% inflection; the meter keeps warning only above
    // 0.5 and accent at-or-below, so 0.5 stays in the calm band.
    const html = renderToStaticMarkup(
      <ContextUsageMeter {...baseProps} contextPercent={0.5} promptTokens={10} />,
    );
    expect(html).not.toContain("bg-warning");
  });
});
