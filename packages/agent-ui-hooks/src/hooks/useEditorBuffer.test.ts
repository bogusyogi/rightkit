// @vitest-environment happy-dom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  getWorkspaceFile: vi.fn(),
  putWorkspaceFile: vi.fn(),
}));

vi.mock("@rightkit/agent-ui-core/lib/daemon/workspace-files", () => mocks);

import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";
import type {
  WorkspaceFileResponse,
  WorkspaceFileSaveResponse,
} from "@rightkit/agent-ui-core/lib/types/workspace-files";
import { __resetEditorBuffers } from "@rightkit/agent-ui-core/state/editorBufferStore";
import { useEditorBuffer } from "./useEditorBuffer";

const CONFIG: DaemonConfig = { url: "http://127.0.0.1:5057", token: "test-token" };

function file(content: string, fingerprint: string): WorkspaceFileResponse {
  return {
    workspaceRoot: "/repo",
    path: "src/App.tsx",
    kind: "file",
    sizeBytes: content.length,
    modifiedAt: "2026-08-29T00:00:00Z",
    fingerprint,
    content,
    permission: "0644",
    truncated: false,
    reason: null,
  };
}

const SAVED: WorkspaceFileSaveResponse = {
  ok: true,
  path: "src/App.tsx",
  fingerprint: "fp-2",
  sizeBytes: 5,
  modifiedAt: "2026-08-29T00:01:00Z",
};

beforeEach(() => {
  __resetEditorBuffers();
  mocks.getWorkspaceFile.mockReset();
  mocks.putWorkspaceFile.mockReset();
});

afterEach(() => {
  cleanup();
  __resetEditorBuffers();
});

describe("useEditorBuffer", () => {
  it("loads daemon content while retaining the buffer identity and view state", async () => {
    mocks.getWorkspaceFile.mockResolvedValue(file("hello", "fp-1"));
    const { result } = renderHook(() =>
      useEditorBuffer({
        config: CONFIG,
        bufferId: "tab-1",
        workspaceRoot: "/repo",
        path: "src/App.tsx",
      }),
    );

    await waitFor(() => expect(result.current.content).toBe("hello"));
    act(() => {
      result.current.setViewState({ cursorLine: 2, cursorColumn: 3 });
    });
    expect(result.current.loaded).toBe(true);
    expect(result.current.dirty).toBe(false);
    expect(result.current.viewState).toEqual({ cursorLine: 2, cursorColumn: 3 });
  });

  it("flags a changed daemon fingerprint without overwriting a dirty draft", async () => {
    mocks.getWorkspaceFile
      .mockResolvedValueOnce(file("disk-1", "fp-1"))
      .mockResolvedValueOnce(file("disk-2", "fp-2"));
    const { result, rerender } = renderHook(
      ({ reloadToken }: { reloadToken: number }) =>
        useEditorBuffer({
          config: CONFIG,
          bufferId: "tab-1",
          workspaceRoot: "/repo",
          path: "src/App.tsx",
          reloadToken,
        }),
      { initialProps: { reloadToken: 0 } },
    );

    await waitFor(() => expect(result.current.content).toBe("disk-1"));
    act(() => result.current.setContent("my-draft"));
    rerender({ reloadToken: 1 });

    await waitFor(() => expect(result.current.externalChanged).toBe(true));
    expect(result.current.content).toBe("my-draft");
    expect(result.current.remoteConflict?.content).toBe("disk-2");

    await act(async () => {
      await result.current.reloadExternal();
    });
    expect(result.current.content).toBe("disk-2");
    expect(result.current.dirty).toBe(false);
  });

  it("saves through the daemon with the observed fingerprint", async () => {
    mocks.getWorkspaceFile
      .mockResolvedValueOnce(file("disk-1", "fp-1"))
      .mockResolvedValueOnce(file("draft", "fp-2"));
    mocks.putWorkspaceFile.mockResolvedValue(SAVED);
    const { result } = renderHook(() =>
      useEditorBuffer({
        config: CONFIG,
        bufferId: "tab-1",
        workspaceRoot: "/repo",
        path: "src/App.tsx",
      }),
    );

    await waitFor(() => expect(result.current.content).toBe("disk-1"));
    act(() => result.current.setContent("draft"));
    await act(async () => {
      await expect(result.current.save()).resolves.toMatchObject({ kind: "saved" });
    });

    expect(mocks.putWorkspaceFile).toHaveBeenCalledWith(
      CONFIG,
      expect.objectContaining({
        workspaceRoot: "/repo",
        path: "src/App.tsx",
        content: "draft",
        expectedFingerprint: "fp-1",
      }),
      {},
    );
    expect(result.current.dirty).toBe(false);
    expect(result.current.response?.fingerprint).toBe("fp-2");
  });
});
