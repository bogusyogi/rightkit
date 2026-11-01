// @vitest-environment happy-dom
import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { UtilityOverflowMenu } from "./AppBody.js";

const onSelect = vi.fn();

function trigger() {
  const all = screen.getAllByTestId("utility-overflow-trigger");
  return all[all.length - 1] as HTMLElement;
}

function menu() {
  const all = screen.queryAllByRole("menu");
  return all[all.length - 1] ?? null;
}

function item(tab: string) {
  const all = screen.queryAllByTestId(`utility-overflow-item-${tab}`);
  return all[all.length - 1] ?? null;
}

// Focus, arrows, Home/End, Escape and outside press belong to RightKit's ContextMenu; these tests
// cover the host app items, current-tab marking and selection.
describe("UtilityOverflowMenu", () => {
  beforeEach(() => {
    onSelect.mockReset();
  });

  it("uses a named menu with current-item semantics", () => {
    render(<UtilityOverflowMenu active="browser" onSelect={onSelect} />);
    fireEvent.click(trigger());

    expect(menu()?.getAttribute("aria-label")).toBe("More workspace tools");
    expect(item("browser")?.getAttribute("aria-current")).toBe("true");
    expect(item("artifacts")?.getAttribute("aria-current")).toBeNull();
  });

  it("selects an item and closes the menu", () => {
    render(<UtilityOverflowMenu active="files" onSelect={onSelect} />);
    fireEvent.click(trigger());
    fireEvent.click(item("delivery") as HTMLElement);

    expect(onSelect).toHaveBeenCalledWith("delivery", "delivery");
    expect(menu()).toBeNull();
  });
});
