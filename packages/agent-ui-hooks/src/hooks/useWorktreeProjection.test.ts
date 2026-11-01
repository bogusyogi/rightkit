// @vitest-environment happy-dom
import { renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  createWorktree,
  listProjectWorktrees,
  removeProjectWorktree,
} from "@rightkit/agent-ui-core/lib/worktrees/worktreeProjectionManager";
import type { WorktreeMetadata } from "@rightkit/agent-ui-core/lib/worktrees/worktreeProjectionManager";
import { useWorktreeProjection } from "./useWorktreeProjection.js";

vi.mock("@rightkit/agent-ui-core/lib/worktrees/worktreeProjectionManager", () => ({
  createWorktree: vi.fn(),
  listProjectWorktrees: vi.fn(),
  removeProjectWorktree: vi.fn(),
}));

const config = { url: "http://127.0.0.1:5057", token: "token" };
const project = { id: "project-1", path: "/repo" };
const feature: WorktreeMetadata = {
  source: "daemon",
  path: "/repo-feature",
  projectDirectory: "/repo",
  branch: "feature",
  label: "feature",
  name: "repo-feature",
  worktreeRoot: "/repo-feature",
  worktreeStatus: "ready",
  headState: "branch",
  worktreeSource: "existing",
};

describe("useWorktreeProjection", () => {
  beforeEach(() => vi.clearAllMocks());

  it("loads the daemon projection and preserves it as last-known on refresh failure", async () => {
    vi.mocked(listProjectWorktrees)
      .mockResolvedValueOnce([feature])
      .mockRejectedValueOnce(new Error("daemon unavailable"));
    const { result, rerender } = renderHook(
      ({ reloadToken }) => useWorktreeProjection({ config, project, reloadToken }),
      { initialProps: { reloadToken: 0 } },
    );

    await waitFor(() => expect(result.current.status).toBe("ready"));
    expect(result.current.worktrees).toEqual([feature]);
    rerender({ reloadToken: 1 });
    await waitFor(() => expect(result.current.status).toBe("unavailable"));
    expect(result.current.worktrees).toEqual([feature]);
    expect(result.current.lastKnown).toBe(true);
  });

  it("refreshes after create and remove without owning the Git effect", async () => {
    vi.mocked(listProjectWorktrees).mockResolvedValue([]);
    vi.mocked(createWorktree).mockResolvedValue(feature);
    vi.mocked(removeProjectWorktree).mockResolvedValue(undefined);
    const { result } = renderHook(() => useWorktreeProjection({ config, project }));
    await waitFor(() => expect(result.current.status).toBe("ready"));

    await result.current.create({ branch: "feature" });
    expect(createWorktree).toHaveBeenCalledWith(config, project, { branch: "feature" });
    await result.current.remove(feature, { force: true });
    expect(removeProjectWorktree).toHaveBeenCalledWith(config, project, feature, { force: true });
  });
});
