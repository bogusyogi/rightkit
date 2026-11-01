// D1 — WorkspaceQuickOpen keyboard + activation tests.

import { describe, expect, it } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import WorkspaceQuickOpen from "./WorkspaceQuickOpen";

const ENTRIES = [
  { name: "README.md", path: "README.md", kind: "file" as const },
  { name: "src", path: "src", kind: "directory" as const },
  { name: "Cargo.toml", path: "Cargo.toml", kind: "file" as const },
];

describe("WorkspaceQuickOpen", () => {
  it("renders every candidate path", () => {
    render(
      <WorkspaceQuickOpen
        workspaceRoot="/root"
        entries={ENTRIES}
        onClose={() => undefined}
        onPick={() => undefined}
      />,
    );
    expect(screen.getByText("README.md")).toBeTruthy();
    expect(screen.getByText("Cargo.toml")).toBeTruthy();
  });

  it("filters candidates by substring match", () => {
    render(
      <WorkspaceQuickOpen
        workspaceRoot="/root"
        entries={ENTRIES}
        onClose={() => undefined}
        onPick={() => undefined}
      />,
    );
    fireEvent.change(screen.getByTestId("files-quick-open-input"), {
      target: { value: "readme" },
    });
    expect(screen.queryByText("Cargo.toml")).toBeNull();
  });

  it("calls onPick with the selected path on Enter", () => {
    let picked: string | null = null;
    render(
      <WorkspaceQuickOpen
        workspaceRoot="/root"
        entries={ENTRIES}
        onClose={() => undefined}
        onPick={(path) => {
          picked = path;
        }}
      />,
    );
    fireEvent.keyDown(screen.getByTestId("files-quick-open-input"), {
      key: "Enter",
    });
    expect(picked).toBe("README.md");
  });

  it("calls onClose on Escape", async () => {
    let closed = false;
    render(
      <WorkspaceQuickOpen
        workspaceRoot="/root"
        entries={ENTRIES}
        onClose={() => {
          closed = true;
        }}
        onPick={() => undefined}
      />,
    );
    fireEvent.keyDown(screen.getByTestId("files-quick-open-input"), {
      key: "Escape",
    });
    // ModalShell plays its exit transition before it calls onClose.
    await waitFor(() => expect(closed).toBe(true));
  });
});
