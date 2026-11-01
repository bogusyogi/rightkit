// @vitest-environment happy-dom
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import UserMessage from "./UserMessage.js";

function lastMessage(): HTMLElement {
  const nodes = screen.getAllByTestId("user-message");
  return nodes[nodes.length - 1];
}

describe("UserMessage", () => {
  it("renders the user content inside a self-end bubble", () => {
    render(<UserMessage content="Hello there" />);
    const node = lastMessage();
    expect(node.className).toContain("self-end");
    expect(node.className).toContain("max-w-[80%]");
    expect(node.textContent).toContain("Hello there");
  });

  it("renders multiline content faithfully", () => {
    render(<UserMessage content={"line 1\nline 2"} />);
    const node = lastMessage();
    expect(node.textContent).toContain("line 1");
    expect(node.textContent).toContain("line 2");
  });
});
