// @vitest-environment happy-dom
import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import KebabMenu, { type KebabAction } from "./KebabMenu";

const MENU_ITEMS: { key: string; label: string }[] = [
  { key: "open-artifacts", label: "Artifacts" },
  { key: "open-files", label: "Files" },
  { key: "open-background-tasks", label: "Agents" },
  { key: "open-in-terminal", label: "Open in Terminal" },
  { key: "rename", label: "Rename" },
  { key: "fork", label: "Fork" },
  { key: "archive", label: "Archive" },
  { key: "delete", label: "Delete" },
];

// React.StrictMode renders the same tree twice under happy-dom. Helper pickers grab the LAST
// element so the click reaches the live instance.
function trigger() {
  const all = screen.getAllByTestId("kebab-trigger");
  return all[all.length - 1] as HTMLElement;
}
function menu() {
  const all = screen.queryAllByRole("menu");
  return all[all.length - 1] ?? null;
}
function item(testId: string) {
  const all = screen.queryAllByTestId(testId);
  return all[all.length - 1] ?? null;
}

// The menu itself (RightKit ContextMenu) owns focus, arrows, Home/End and Escape; these tests cover
// the host app items, selection and current-tab marking.
describe("KebabMenu", () => {
  let onSelect: (action: KebabAction) => void;
  beforeEach(() => {
    onSelect = vi.fn();
  });

  it("renders the kebab trigger", () => {
    render(<KebabMenu activeTab={null} onSelect={onSelect} />);
    const buttons = screen.getAllByTestId("kebab-trigger");
    expect(buttons.length).toBeGreaterThan(0);
    expect(buttons[0].getAttribute("aria-label")).toBeTruthy();
  });

  it("opens on trigger click and lists all expected items", () => {
    render(<KebabMenu activeTab={null} onSelect={onSelect} />);
    fireEvent.click(trigger());
    expect(menu()).toBeTruthy();
    expect(menu()?.getAttribute("aria-label")).toBe("Chat actions");
    for (const menuItem of MENU_ITEMS) {
      expect(
        screen.getAllByTestId(`kebab-item-${menuItem.key}`).length,
        `expected kebab item "${menuItem.key}"`,
      ).toBeGreaterThan(0);
    }
  });

  it("marks the Files item current when the files tab is active", () => {
    render(<KebabMenu activeTab="files" onSelect={onSelect} />);
    fireEvent.click(trigger());
    const filesItem = item("kebab-item-open-files");
    expect(filesItem?.className).toContain("text-accent");
    expect(filesItem?.getAttribute("aria-current")).toBe("true");
  });

  it("fires onSelect with the matching action when an item is clicked", () => {
    render(<KebabMenu activeTab={null} onSelect={onSelect} />);
    fireEvent.click(trigger());
    fireEvent.click(item("kebab-item-open-files") as HTMLElement);
    expect(onSelect).toHaveBeenCalledWith("open-files");
  });

  it("fires onSelect for Open in Terminal directly", () => {
    render(<KebabMenu activeTab={null} onSelect={onSelect} />);
    fireEvent.click(trigger());
    fireEvent.click(item("kebab-item-open-in-terminal") as HTMLElement);
    expect(onSelect).toHaveBeenCalledWith("open-in-terminal");
  });
});
