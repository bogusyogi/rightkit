import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import ReasoningBlock from "./ReasoningBlock";

describe("ReasoningBlock", () => {
  it("is collapsed by default", () => {
    const html = renderToStaticMarkup(<ReasoningBlock content="long private reasoning" />);

    expect(html).toContain("Thinking");
    expect(html).not.toContain("long private reasoning");
  });

  it("bounds expanded reasoning inside its own scroll area", () => {
    const html = renderToStaticMarkup(<ReasoningBlock content="long private reasoning" expanded />);

    expect(html).toContain("long private reasoning");
    expect(html).toContain("max-h-72");
    expect(html).toContain("overflow-y-auto");
  });
});
