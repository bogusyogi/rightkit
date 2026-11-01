// @vitest-environment happy-dom
import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useMemoryFiles } from "@rightkit/agent-ui-hooks/hooks/useMemoryFiles";
import MemoryTab from "./MemoryTab";

vi.mock("@rightkit/agent-ui-hooks/hooks/useMemoryFiles", () => ({ useMemoryFiles: vi.fn() }));

const edit = vi.fn();
const reRead = vi.fn(async () => {});
const keepLocal = vi.fn(async () => {});
const save = vi.fn(async () => ({ kind: "saved" as const, response: {} as never }));

function memoryResult(overrides: Record<string, unknown> = {}) {
  return {
    path: "CLAUDE.md",
    status: "ready",
    response: { kind: "file" },
    reason: null,
    remoteConflict: null,
    content: "project instructions",
    dirty: false,
    draftWorkspaceRoot: null,
    workspaceMismatch: false,
    edit,
    reRead,
    keepLocal,
    save,
    reload: vi.fn(),
    refresh: vi.fn(),
    ...overrides,
  };
}

describe("MemoryTab guarded editor contract", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(useMemoryFiles).mockReturnValue(memoryResult() as never);
  });

  it("renders exact project memory file & edits through live hook", () => {
    render(
      <MemoryTab config={{ url: "http://daemon", token: "token" }} workspaceRoot="D:/workspace" />,
    );
    fireEvent.change(screen.getByLabelText("Project CLAUDE.md"), {
      target: { value: "updated instructions" },
    });

    expect(screen.getByText(/edits existing project-root file only/)).toBeTruthy();
    expect(edit).toHaveBeenCalledWith("updated instructions");
  });

  it("requires confirmation before discarding a dirty draft", () => {
    vi.mocked(useMemoryFiles).mockReturnValue(memoryResult({ dirty: true }) as never);
    render(
      <MemoryTab config={{ url: "http://daemon", token: "token" }} workspaceRoot="D:/workspace" />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Re-read" }));
    expect(screen.getByRole("dialog", { name: "Discard draft confirmation" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Keep local" }));
    expect(reRead).not.toHaveBeenCalled();
  });

  it("offers explicit conflict resolution after stale disk change", () => {
    vi.mocked(useMemoryFiles).mockReturnValue(
      memoryResult({ dirty: true, remoteConflict: { fingerprint: "new" } }) as never,
    );
    render(
      <MemoryTab config={{ url: "http://daemon", token: "token" }} workspaceRoot="D:/workspace" />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Keep local" }));
    expect(keepLocal).toHaveBeenCalledOnce();
  });

  it("renders truthful disconnected & missing states", () => {
    const { rerender } = render(<MemoryTab config={null} workspaceRoot="" />);
    expect(screen.getByText(/Open a connected workspace/)).toBeTruthy();

    vi.mocked(useMemoryFiles).mockReturnValue(
      memoryResult({ status: "error", response: { kind: "missing" } }) as never,
    );
    rerender(
      <MemoryTab config={{ url: "http://daemon", token: "token" }} workspaceRoot="D:/workspace" />,
    );
    expect(screen.getByText(/does not exist/)).toBeTruthy();
  });

  it("disables Save when a dirty draft belongs to another workspace", () => {
    vi.mocked(useMemoryFiles).mockReturnValue(
      memoryResult({
        dirty: true,
        draftWorkspaceRoot: "D:/workspace-a",
        workspaceMismatch: true,
      }) as never,
    );
    render(
      <MemoryTab
        config={{ url: "http://daemon", token: "token" }}
        workspaceRoot="D:/workspace-b"
      />,
    );

    expect(screen.getByRole("alert").textContent).toContain("D:/workspace-a");
    expect(screen.getByRole("button", { name: "Save" })).toHaveProperty("disabled", true);
  });
});
