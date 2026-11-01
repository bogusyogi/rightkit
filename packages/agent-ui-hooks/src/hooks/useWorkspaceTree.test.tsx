// @vitest-environment happy-dom
import { renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { getWorkspaceTree } from "@rightkit/agent-ui-core/lib/daemon/workspace-files";
import { useWorkspaceStore } from "@rightkit/agent-ui-core/state/workspaceStore";
import { useWorkspaceTree } from "./useWorkspaceTree";

vi.mock("@rightkit/agent-ui-core/lib/daemon/workspace-files", () => ({
  getWorkspaceTree: vi.fn(),
  postWorkspaceTreeExpand: vi.fn(),
}));

describe("useWorkspaceTree", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useWorkspaceStore.getState().resetForWorkspace("/workspace/previous");
    useWorkspaceStore
      .getState()
      .rememberPath("", [{ name: "stale.ts", path: "stale.ts", kind: "file" }], false);
  });

  it("never renders a previous workspace tree for a failed new workspace", async () => {
    vi.mocked(getWorkspaceTree).mockRejectedValueOnce(new Error("workspace request failed (400)"));

    const { result } = renderHook(() =>
      useWorkspaceTree({
        config: { url: "http://127.0.0.1:9999", token: "token" },
        workspaceRoot: "/workspace/missing",
      }),
    );

    await waitFor(() => expect(result.current.status).toBe("error"));
    expect(result.current.entries).toEqual([]);
    expect(result.current.lastKnown).toBe(false);
    expect(getWorkspaceTree).toHaveBeenCalledWith(
      { url: "http://127.0.0.1:9999", token: "token" },
      "/workspace/missing",
      "",
      expect.any(Object),
    );
  });
});
