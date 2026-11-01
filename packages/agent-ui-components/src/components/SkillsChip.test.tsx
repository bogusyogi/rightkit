// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import SkillsChip from "./SkillsChip";
import type { SkillItem } from "@rightkit/agent-ui-core/lib/types";

function makeSkill(slug: string, overrides: Partial<SkillItem> = {}): SkillItem {
  return {
    slug,
    name: slug.charAt(0).toUpperCase() + slug.slice(1),
    source: "local",
    description: `Desc ${slug}`,
    trigger: `@${slug}`,
    enabled: true,
    active: true,
    lifecycle: "active",
    category: "tool",
    priority: 0,
    tags: [],
    pathAffinity: [],
    path: `/skills/${slug}.md`,
    directory: "/skills",
    kind: "tool",
    valid: true,
    ...overrides,
  };
}

const sampleSkills: SkillItem[] = [
  makeSkill("audit"),
  makeSkill("review"),
  makeSkill("test", { description: "Run the regression tests" }),
];

function chipButton(): HTMLButtonElement {
  return screen.getByRole("button", { name: "Open skills" });
}

describe("SkillsChip", () => {
  beforeEach(() => {
    const probe = globalThis.localStorage;
    if (probe) probe.clear();
  });
  afterEach(() => {
    cleanup();
  });

  it("renders a chip with the skill count badge", () => {
    render(
      <SkillsChip
        skills={sampleSkills}
        onInsert={() => {}}
        onRefresh={() => {}}
        onOpenManage={() => {}}
      />,
    );
    const chip = chipButton();
    expect(chip).toBeTruthy();
    expect(chip.textContent).toContain(String(sampleSkills.length));
    expect(chip.getAttribute("aria-expanded")).toBe("false");
  });

  it("opens a popover listing the skills when the chip is clicked", () => {
    render(
      <SkillsChip
        skills={sampleSkills}
        onInsert={() => {}}
        onRefresh={() => {}}
        onOpenManage={() => {}}
      />,
    );
    fireEvent.click(chipButton());
    const listbox = screen.getByRole("listbox", { name: "Skills" });
    expect(listbox).toBeTruthy();
    for (const skill of sampleSkills) {
      expect(screen.getByText(skill.name)).toBeTruthy();
    }
  });

  it("filters skills by query", () => {
    render(
      <SkillsChip
        skills={sampleSkills}
        onInsert={() => {}}
        onRefresh={() => {}}
        onOpenManage={() => {}}
      />,
    );
    fireEvent.click(chipButton());
    const search = screen.getByPlaceholderText("Search installed skills");
    fireEvent.change(search, { target: { value: "regression" } });
    expect(screen.queryByText("Audit")).toBeNull();
    expect(screen.getByText("Test")).toBeTruthy();
  });

  it("invokes onInsert with the skill and closes the popover when picked", () => {
    const onInsert = vi.fn();
    render(
      <SkillsChip
        skills={sampleSkills}
        onInsert={onInsert}
        onRefresh={() => {}}
        onOpenManage={() => {}}
      />,
    );
    fireEvent.click(chipButton());
    fireEvent.click(screen.getByRole("option", { name: /Review/ }));
    expect(onInsert).toHaveBeenCalledWith(expect.objectContaining({ slug: "review" }));
    expect(screen.queryByRole("listbox", { name: "Skills" })).toBeNull();
  });

  it("arrow keys move the active selection", () => {
    render(
      <SkillsChip
        skills={sampleSkills}
        onInsert={() => {}}
        onRefresh={() => {}}
        onOpenManage={() => {}}
      />,
    );
    fireEvent.click(chipButton());
    // Popover focuses the search field; the list keys are handled on its container, so the event starts there.
    const field = screen.getByLabelText("Search installed skills");
    fireEvent.keyDown(field, { key: "ArrowDown" });
    fireEvent.keyDown(field, { key: "ArrowDown" });
    // Read aria-selected BEFORE pressing Enter — Enter triggers onInsert,
    // which closes the popover (filtered[2] selects "test").
    const options = screen.getAllByRole("option");
    expect(options[2].getAttribute("aria-selected")).toBe("true");
    expect(options[0].getAttribute("aria-selected")).toBe("false");
    expect(options[1].getAttribute("aria-selected")).toBe("false");
  });

  it("closes the popover when Escape is pressed", () => {
    render(
      <SkillsChip
        skills={sampleSkills}
        onInsert={() => {}}
        onRefresh={() => {}}
        onOpenManage={() => {}}
      />,
    );
    fireEvent.click(chipButton());
    expect(screen.getByRole("listbox", { name: "Skills" })).toBeTruthy();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("listbox", { name: "Skills" })).toBeNull();
  });

  it("invokes onRefresh and onOpenManage", () => {
    const onRefresh = vi.fn();
    const onOpenManage = vi.fn();
    render(
      <SkillsChip
        skills={sampleSkills}
        onInsert={() => {}}
        onRefresh={onRefresh}
        onOpenManage={onOpenManage}
      />,
    );
    fireEvent.click(chipButton());
    fireEvent.click(screen.getByRole("button", { name: "Refresh skills" }));
    expect(onRefresh).toHaveBeenCalled();
    // Re-open if the chip click closed it (outside-click mousedown also fires).
    if (!screen.queryByRole("dialog", { name: "Skills" })) {
      fireEvent.click(chipButton());
    }
    fireEvent.click(screen.getByRole("button", { name: "Manage in Settings" }));
    expect(onOpenManage).toHaveBeenCalled();
  });
});
