import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { UseEditorBufferResult } from "@rightkit/agent-ui-hooks/hooks/useEditorBuffer";
import type { WorkspaceFileSaveOutcome } from "@rightkit/agent-ui-core/lib/types/workspace-files";
import WorkspaceFileViewer from "./WorkspaceFileViewer.js";

const hookState = vi.hoisted(() => ({ current: null as UseEditorBufferResult | null }));

vi.mock("@rightkit/agent-ui-hooks/hooks/useEditorBuffer", () => ({
  useEditorBuffer: () => hookState.current,
}));

afterEach(() => {
  cleanup();
  hookState.current = null;
});

function result(overrides: Partial<UseEditorBufferResult> = {}): UseEditorBufferResult {
  const saved: WorkspaceFileSaveOutcome = {
    kind: "saved",
    response: {
      ok: true,
      path: "notes.md",
      fingerprint: "next",
      sizeBytes: 8,
      modifiedAt: "2024-01-01T00:00:00Z",
    },
  };
  return {
    buffer: undefined,
    status: "ready",
    response: {
      workspaceRoot: "/workspace",
      path: "notes.md",
      kind: "file",
      sizeBytes: 8,
      modifiedAt: "2024-01-01T00:00:00Z",
      fingerprint: "base",
      content: "hello",
      permission: "0644",
      truncated: false,
      reason: null,
    },
    remoteConflict: null,
    reason: null,
    content: "hello",
    dirty: false,
    loaded: true,
    loading: false,
    externalChanged: false,
    viewState: null,
    setContent: vi.fn(),
    setViewState: vi.fn(),
    refresh: vi.fn(async () => undefined),
    reloadExternal: vi.fn(async () => undefined),
    keepLocal: vi.fn(),
    revert: vi.fn(),
    save: vi.fn(async () => saved),
    ...overrides,
  };
}

describe("WorkspaceFileViewer", () => {
  it("binds the visible editor to the shared buffer and exposes save/revert", () => {
    const setContent = vi.fn();
    const save = vi.fn(async (): Promise<WorkspaceFileSaveOutcome> => ({
      kind: "saved",
      response: {
        ok: true,
        path: "notes.md",
        fingerprint: "next",
        sizeBytes: 7,
        modifiedAt: "2024-01-01T00:00:00Z",
      },
    }));
    hookState.current = result({ dirty: true, setContent, save });

    render(<WorkspaceFileViewer config={null} workspaceRoot="/workspace" path="notes.md" />);

    expect(screen.getByTestId("workspace-file-viewer-path").textContent).toBe("notes.md");
    fireEvent.change(screen.getByTestId("workspace-file-viewer-textarea"), {
      target: { value: "updated" },
    });
    expect(setContent).toHaveBeenCalledWith("updated");
    fireEvent.click(screen.getByTestId("workspace-file-viewer-save"));
    expect(save).toHaveBeenCalledTimes(1);
  });

  it("keeps the conflict choices in front of a dirty draft", () => {
    const reloadExternal = vi.fn(async () => undefined);
    const keepLocal = vi.fn();
    const response = result().response;
    if (!response) throw new Error("expected response fixture");
    hookState.current = result({
      status: "stale",
      dirty: true,
      remoteConflict: { ...response, fingerprint: "remote" },
      reloadExternal,
      keepLocal,
    });

    render(<WorkspaceFileViewer config={null} workspaceRoot="/workspace" path="notes.md" />);

    expect(screen.getByRole("alert")).toBeTruthy();
    fireEvent.click(screen.getByTestId("workspace-file-viewer-reread"));
    fireEvent.click(screen.getByTestId("workspace-file-viewer-keep-local"));
    expect(reloadExternal).toHaveBeenCalledTimes(1);
    expect(keepLocal).toHaveBeenCalledTimes(1);
  });
});
