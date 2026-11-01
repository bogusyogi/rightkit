import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { RichComposerEditor } from "./RichComposerEditor";

describe("RichComposerEditor", () => {
  it("renders a plain-text editor with donor-compatible Enter and selection callbacks", () => {
    const onChange = vi.fn();
    const onSelectionChange = vi.fn();
    const onKeyDown = vi.fn(() => true);
    render(
      <RichComposerEditor
        value="draft"
        onChange={onChange}
        onSelectionChange={onSelectionChange}
        onKeyDown={onKeyDown}
        aria-label="Message composer"
        data-testid="rich-editor"
      />,
    );
    const input = screen.getByRole("textbox", { name: "Message composer" });
    expect(input).toHaveValue("draft");
    fireEvent.select(input);
    expect(onSelectionChange).toHaveBeenCalled();
    fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(onKeyDown).toHaveBeenCalled();
  });
});
