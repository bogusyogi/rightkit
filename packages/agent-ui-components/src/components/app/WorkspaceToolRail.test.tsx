import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { WorkspaceToolRail } from "./WorkspaceToolRail";

const props = () => ({
  activePanel: "files" as const,
  terminalOpen: false,
  onTogglePanel: vi.fn(),
  onClosePanel: vi.fn(),
  onToggleTerminal: vi.fn(),
});

describe("WorkspaceToolRail", () => {
  it("contains only ordered workspace tools with accessible pressed controls", () => {
    render(<WorkspaceToolRail {...props()} />);
    const labels = [
      "Files",
      "Artifacts",
      "Review / Changes",
      "Execution graph",
      "Bots",
      "Browser",
      "Delivery",
      "Environments",
      "Attention",
      "Terminal",
    ];
    expect(
      screen.getAllByRole("button").map((button) => button.getAttribute("aria-label")),
    ).toEqual(labels);
    expect(screen.getByRole("button", { name: "Files" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("button", { name: "Terminal" })).toHaveAttribute(
      "aria-pressed",
      "false",
    );
    expect(screen.getByTestId("workspace-tool-rail")).toHaveClass(
      "w-10",
      "rounded-xl",
      "bg-panel-2/90",
      "opacity-100",
    );
    expect(screen.getByRole("button", { name: "Files" })).toHaveClass("h-8", "w-8");
    expect(screen.getByRole("button", { name: "Files" })).toHaveClass(
      "border-accent/60",
      "bg-accent/15",
    );
  });

  it("rests at reduced opacity only while every tool is closed", () => {
    render(<WorkspaceToolRail {...props()} activePanel={null} />);
    expect(screen.getByTestId("workspace-tool-rail")).toHaveClass("opacity-70");
  });

  it("dispatches panel switches plus existing terminal dock toggle", () => {
    const input = props();
    render(<WorkspaceToolRail {...input} />);
    fireEvent.click(screen.getByRole("button", { name: "Artifacts" }));
    fireEvent.click(screen.getByRole("button", { name: "Terminal" }));
    expect(input.onTogglePanel).toHaveBeenCalledWith("artifacts");
    expect(input.onToggleTerminal).toHaveBeenCalledOnce();
  });

  it("supports roving focus and restores focus to trigger on Escape", () => {
    const input = { ...props(), activePanel: null, terminalOpen: true };
    render(<WorkspaceToolRail {...input} />);
    const files = screen.getByRole("button", { name: "Files" });
    files.focus();
    fireEvent.keyDown(files, { key: "ArrowDown" });
    expect(screen.getByRole("button", { name: "Artifacts" })).toHaveFocus();
    const focusedArtifact = document.activeElement;
    if (!(focusedArtifact instanceof HTMLElement))
      throw new Error("Artifact did not receive focus");
    fireEvent.keyDown(focusedArtifact, { key: "End" });
    expect(screen.getByRole("button", { name: "Terminal" })).toHaveFocus();
    const focusedTerminal = document.activeElement;
    if (!(focusedTerminal instanceof HTMLElement))
      throw new Error("Terminal did not receive focus");
    fireEvent.keyDown(focusedTerminal, { key: "Escape" });
    expect(input.onToggleTerminal).toHaveBeenCalledOnce();
    expect(screen.getByRole("button", { name: "Terminal" })).toHaveFocus();
  });

  it("closes any open panel on Escape from every rail trigger", () => {
    const input = props();
    render(<WorkspaceToolRail {...input} />);
    const artifact = screen.getByRole("button", { name: "Artifacts" });
    artifact.focus();
    fireEvent.keyDown(artifact, { key: "Escape" });
    expect(input.onClosePanel).toHaveBeenCalledOnce();
    expect(input.onTogglePanel).not.toHaveBeenCalled();
    expect(artifact).toHaveFocus();
  });

  it("closes from panel focus on Escape and restores the active trigger", () => {
    const input = props();
    render(
      <>
        <WorkspaceToolRail {...input} activePanel="browser" />
        <button type="button">Panel control</button>
      </>,
    );
    screen.getByRole("button", { name: "Panel control" }).focus();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(input.onClosePanel).toHaveBeenCalledOnce();
    expect(screen.getByRole("button", { name: "Browser" })).toHaveFocus();
  });
});
