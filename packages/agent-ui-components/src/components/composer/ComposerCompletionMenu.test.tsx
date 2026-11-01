import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { ComposerCompletion } from "@rightkit/agent-ui-core/lib/composer/completionRanking";
import { ComposerCompletionMenu } from "./ComposerCompletionMenu";

const item: ComposerCompletion = { kind: "command", key: "command:new", label: "new", description: "Start", insertText: "/new ", score: 1 };

describe("ComposerCompletionMenu", () => {
  it("renders accessible options and reports hover and selection", () => {
    const onSelectIndex = vi.fn();
    const onPick = vi.fn();
    render(<ComposerCompletionMenu items={[item]} selectedIndex={0} onSelectIndex={onSelectIndex} onPick={onPick} />);
    expect(screen.getByRole("listbox", { name: "Composer completions" })).toBeTruthy();
    const option = screen.getByRole("option");
    fireEvent.mouseEnter(option);
    fireEvent.click(option);
    expect(onSelectIndex).toHaveBeenCalledWith(0);
    expect(onPick).toHaveBeenCalledWith(item);
  });

  it("does not mount an empty menu", () => {
    const { container } = render(<ComposerCompletionMenu items={[]} selectedIndex={0} onSelectIndex={vi.fn()} onPick={vi.fn()} />);
    expect(container.firstChild).toBeNull();
  });
});
