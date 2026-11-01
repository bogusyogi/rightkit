import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { DispatchBadge } from "./DispatchBadge";

describe("DispatchBadge", () => {
  it("renders the execution strategy and exposes the rationale on hover", () => {
    const html = renderToStaticMarkup(
      <DispatchBadge
        decision={{
          strategy: "multi-agent",
          rationale: "repo-wide work benefits from decomposition",
        }}
      />,
    );

    expect(html).toContain("multi-agent");
    expect(html).toContain('title="repo-wide work benefits from decomposition"');
  });

  it("renders nothing before a dispatch decision arrives", () => {
    expect(renderToStaticMarkup(<DispatchBadge decision={null} />)).toBe("");
  });
});
