// @vitest-environment happy-dom
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import AssistantMessage from "./AssistantMessage.js";

function lastMessage(): HTMLElement {
  const nodes = screen.getAllByTestId("assistant-message");
  return nodes[nodes.length - 1];
}

describe("AssistantMessage", () => {
  it("renders the assistant content on the chat canvas", () => {
    render(<AssistantMessage content="Let me think that through." />);
    const node = lastMessage();
    expect(node.className).toContain("self-start");
    expect(node.className).toContain("max-w-[80%]");
    expect(node.textContent).toContain("Let me think that through.");
    expect(screen.queryAllByTestId("thinking-indicator").length).toBe(0);
  });

  it("renders ThinkingIndicator when isThinking is true", () => {
    render(
      <AssistantMessage
        content="Working on it"
        isThinking
        elapsedSeconds={12}
        tokenCount={85}
        thinkingStatus="thinking..."
      />,
    );
    const indicatorNodes = screen.getAllByTestId("thinking-indicator");
    const indicator = indicatorNodes[indicatorNodes.length - 1];
    expect(indicator.textContent).toContain("12s");
    expect(indicator.textContent).toContain("85");
  });
});
