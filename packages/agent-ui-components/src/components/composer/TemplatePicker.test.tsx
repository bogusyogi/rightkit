import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { BUILT_IN_TEMPLATES } from "@rightkit/agent-ui-core/lib/promptTemplates/templates";
import { TemplatePicker } from "./TemplatePicker";

const props = () => ({
  builtInTemplates: BUILT_IN_TEMPLATES.slice(0, 1), userTemplates: [], onSelect: vi.fn(), onDeleteUser: vi.fn(), open: true, onToggle: vi.fn(), pinnedIds: new Set<string>(), onTogglePin: vi.fn(), templateGroups: [], onCreateGroup: vi.fn(), onRenameGroup: vi.fn(), onDeleteGroup: vi.fn(), onMoveToGroup: vi.fn(),
});

describe("TemplatePicker", () => {
  it("shows searchable built-in templates and selects a template", () => {
    const input = props();
    render(<TemplatePicker {...input} />);
    expect(screen.getByRole("dialog", { name: "Prompt templates" })).toBeTruthy();
    expect(screen.getByText("Root Cause Analysis")).toBeTruthy();
    fireEvent.click(screen.getByText("Root Cause Analysis"));
    expect(input.onSelect).toHaveBeenCalledWith(BUILT_IN_TEMPLATES[0]);
    expect(input.onToggle).toHaveBeenCalled();
  });

  it("switches to user templates and supports filtering", () => {
    const input = { ...props(), userTemplates: [{ ...BUILT_IN_TEMPLATES[0], id: "mine", name: "My Template", builtIn: false }] };
    render(<TemplatePicker {...input} />);
    fireEvent.click(screen.getByText(/My templates/));
    fireEvent.change(screen.getByPlaceholderText("Search templates"), { target: { value: "my" } });
    expect(screen.getByText("My Template")).toBeTruthy();
  });
});
