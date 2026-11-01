// @vitest-environment happy-dom
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { WorkspaceFileSaveOutcome } from "@rightkit/agent-ui-core/lib/types/workspace-files";
import { useWorkspaceFile } from "./useWorkspaceFile";
import { MEMORY_PATH, useMemoryFiles } from "./useMemoryFiles";

vi.mock("./useWorkspaceFile", () => ({ useWorkspaceFile: vi.fn() }));

const refresh = vi.fn(async () => {});
const save = vi.fn<() => Promise<WorkspaceFileSaveOutcome>>(async () => ({
  kind: "saved",
  response: {} as never,
}));

function workspaceFileResult(overrides: Record<string, unknown> = {}) {
  return {
    status: "ready",
    response: {
      workspaceRoot: "D:/workspace",
      path: "CLAUDE.md",
      kind: "file",
      sizeBytes: 7,
      modifiedAt: null,
      fingerprint: "fingerprint-1",
      content: "initial",
      permission: "0644",
      truncated: false,
      reason: null,
    },
    reason: null,
    remoteConflict: null,
    refresh,
    save,
    ...overrides,
  };
}

describe("useMemoryFiles contract", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(useWorkspaceFile).mockReturnValue(workspaceFileResult() as never);
  });

  it("restricts memory editing to project-root CLAUDE.md", () => {
    expect(MEMORY_PATH).toBe("CLAUDE.md");
    renderHook(() => useMemoryFiles({ url: "http://daemon", token: "token" }, "D:/workspace"));
    expect(useWorkspaceFile).toHaveBeenCalledWith(
      expect.objectContaining({ workspaceRoot: "D:/workspace", path: "CLAUDE.md" }),
    );
  });

  it("preserves local content while adopting latest fingerprint after Keep local", async () => {
    const { result } = renderHook(() =>
      useMemoryFiles({ url: "http://daemon", token: "token" }, "D:/workspace"),
    );
    act(() => result.current.edit("local draft"));

    await act(async () => result.current.keepLocal());

    expect(result.current.content).toBe("local draft");
    expect(result.current.dirty).toBe(true);
    expect(refresh).toHaveBeenCalledOnce();
  });

  it("keeps dirty draft after a stale save outcome", async () => {
    save.mockResolvedValueOnce({ kind: "stale" });
    const { result } = renderHook(() =>
      useMemoryFiles({ url: "http://daemon", token: "token" }, "D:/workspace"),
    );
    act(() => result.current.edit("local draft"));

    await act(async () => {
      expect(await result.current.save()).toEqual({ kind: "stale" });
    });

    expect(result.current.content).toBe("local draft");
    expect(result.current.dirty).toBe(true);
  });

  it("binds a dirty draft to its origin workspace & blocks cross-workspace save", async () => {
    const { result, rerender } = renderHook(
      ({ workspaceRoot }) =>
        useMemoryFiles({ url: "http://daemon", token: "token" }, workspaceRoot),
      { initialProps: { workspaceRoot: "D:/workspace-a" } },
    );
    act(() => result.current.edit("workspace A draft"));

    rerender({ workspaceRoot: "D:/workspace-b" });

    expect(result.current.workspaceMismatch).toBe(true);
    expect(result.current.draftWorkspaceRoot).toBe("D:/workspace-a");
    await act(async () => {
      expect(await result.current.save()).toEqual({
        kind: "unsupported",
        reason: "Draft belongs to another workspace. Switch back or re-read before saving.",
      });
    });
    expect(save).not.toHaveBeenCalled();
  });
});
