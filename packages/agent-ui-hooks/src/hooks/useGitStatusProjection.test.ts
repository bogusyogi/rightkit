// @vitest-environment happy-dom
import { renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import {
  stagePaths,
  unstagePaths,
} from "@rightkit/agent-ui-core/lib/daemon/delivery-environments-attention";
import {
  applyGitIndexMutation,
  useGitStatusProjection,
} from "./useGitStatusProjection.js";
import type { GitStatusSnapshot } from "./useGitStatusProjection.js";

vi.mock("@rightkit/agent-ui-core/lib/daemon/delivery-environments-attention", () => ({
  stagePaths: vi.fn(),
  unstagePaths: vi.fn(),
}));

const config = { url: "http://127.0.0.1:5057", token: "token" };
const initial: GitStatusSnapshot = {
  workspaceRoot: "/repo",
  branch: "main",
  ahead: 0,
  behind: 0,
  isClean: false,
  entries: [
    { path: "src/a.ts", indexStatus: " ", worktreeStatus: "M", staged: false, unstaged: true },
    { path: "src/b.ts", indexStatus: "M", worktreeStatus: " ", staged: true, unstaged: false },
  ],
};

const successful = () => ({ exitCode: 0, command: "git", stdout: "", stderr: "", status: "completed" });

describe("useGitStatusProjection", () => {
  it("applies the donor snapshot transition without mutating the input", () => {
    const next = applyGitIndexMutation(initial, "stage", ["src/a.ts"]);
    expect(next.entries[0]).toMatchObject({ staged: true, unstaged: false });
    expect(initial.entries[0]).toMatchObject({ staged: false, unstaged: true });
  });

  it("serializes daemon stage calls and keeps the optimistic projection", async () => {
    vi.mocked(stagePaths).mockResolvedValue(successful());
    vi.mocked(unstagePaths).mockResolvedValue(successful());
    const loadStatus = vi.fn().mockResolvedValue(initial);
    const { result } = renderHook(() => useGitStatusProjection({
      config,
      workspaceRoot: "/repo",
      loadStatus,
    }));
    await waitFor(() => expect(result.current.status).toBe("ready"));

    result.current.stage(["src/a.ts"]);
    await waitFor(() => {
      expect(stagePaths).toHaveBeenCalledWith(config, "/repo", ["src/a.ts"]);
      expect(result.current.snapshot?.entries[0]).toMatchObject({ staged: true, unstaged: false });
      expect(result.current.pendingPaths).toEqual([]);
    });
  });

  it("rolls back the optimistic transition when the daemon rejects it", async () => {
    vi.mocked(stagePaths).mockRejectedValue(new Error("authority denied"));
    const loadStatus = vi.fn().mockResolvedValue(initial);
    const { result } = renderHook(() => useGitStatusProjection({
      config,
      workspaceRoot: "/repo",
      loadStatus,
    }));
    await waitFor(() => expect(result.current.status).toBe("ready"));

    result.current.stage(["src/a.ts"]);
    await waitFor(() => expect(result.current.error).toBe("authority denied"));
    expect(result.current.snapshot?.entries[0]).toMatchObject({ staged: false, unstaged: true });
  });
});
