// @vitest-environment happy-dom
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import VirtualizedCodeBlock, { VIRTUALIZE_THRESHOLD, type CodeLine } from "@rightkit/agent-ui-workbench/components/VirtualizedCodeBlock";

const lines = (count: number): CodeLine[] =>
  Array.from({ length: count }, (_, index) => ({ text: `line ${index}`, lineNumber: index + 1 }));

describe("VirtualizedCodeBlock", () => {
  it("renders small blocks directly with line numbers and language metadata", () => {
    const html = renderToStaticMarkup(
      <VirtualizedCodeBlock lines={lines(2)} language="typescript" />,
    );
    expect(html).toContain('data-code-block-mode="static"');
    expect(html).toContain('data-language="typescript"');
    expect(html).toContain(">line 0<");
    expect(html).toContain(">1<");
  });

  it("uses an overscanned virtual window for very large blocks", () => {
    const html = renderToStaticMarkup(
      <VirtualizedCodeBlock lines={lines(VIRTUALIZE_THRESHOLD + 1)} language="rust" />,
    );
    expect(html).toContain('data-code-block-mode="virtualized"');
    expect(html).toContain('data-index="0"');
    expect(html).not.toContain(`>line ${VIRTUALIZE_THRESHOLD}<`);
  });

  it("preserves caller styles and can omit line numbers", () => {
    const html = renderToStaticMarkup(
      <VirtualizedCodeBlock
        lines={[{ text: "info", isInfo: true }]}
        language="text"
        showLineNumbers={false}
        lineStyles={() => ({ color: "red" })}
      />,
    );
    expect(html).not.toContain("tools-edit-line-number");
    expect(html).toContain("italic");
    expect(html).toContain("color:red");
  });
});
