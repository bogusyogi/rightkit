import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CommandRecord } from "../types/delivery-environments-attention";

const { listWorktrees, createWorktree, removeWorktree } = vi.hoisted(() => ({
  listWorktrees: vi.fn(),
  createWorktree: vi.fn(),
  removeWorktree: vi.fn(),
}));

vi.mock("../daemon/delivery-environments-attention", () => ({
  listWorktrees,
  createWorktree,
  removeWorktree,
}));

import {
  createWorktree as createProjectionWorktree,
  invalidateWorktreeList,
  listProjectWorktrees,
  normalizePath,
  parseWorktreeList,
  partitionWorktreesByRegisteredProject,
  worktreeMapsEqual,
} from "./worktreeProjectionManager";
import type { WorktreeMetadata } from "./worktreeProjectionManager";

const config = { url: "http://127.0.0.1:5057", token: "token" };
const project = { id: "project-1", path: "/repo" };
const record = (stdoutTail: string, exitCode = 0): CommandRecord => ({
  commandId: "cmd-1",
  command: "git worktree list --porcelain",
  args: [],
  cwd: "/repo",
  startedAt: "2026-01-01T00:00:00Z",
  finishedAt: "2026-01-01T00:00:00Z",
  exitCode,
  status: "exited",
  stdoutTail,
  stderrTail: "",
  hostKind: "local",
});

const worktree = (path: string, branch: string): WorktreeMetadata => ({
  source: "daemon",
  path,
  projectDirectory: "/repo",
  branch,
  label: branch || path,
  name: path.split("/").pop() ?? "",
  worktreeRoot: path,
  worktreeStatus: "ready",
  headState: branch ? "branch" : "detached",
  worktreeSource: "existing",
});

describe("worktreeProjectionManager", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    invalidateWorktreeList("/repo");
  });

  it("parses branch, detached, and unborn porcelain entries", () => {
    const entries = parseWorktreeList(
      "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n" +
      "worktree /repo-detached\nHEAD def\n\n" +
      "worktree /repo-empty\n\n",
    );
    expect(entries.map((entry) => [entry.path, entry.branch, entry.headState])).toEqual([
      ["/repo", "main", "branch"],
      ["/repo-detached", "", "detached"],
      ["/repo-empty", "", "unborn"],
    ]);
  });

  it("deduplicates an in-flight daemon listing and omits the project checkout", async () => {
    listWorktrees.mockResolvedValueOnce(record(
      "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n" +
      "worktree /repo-feature\nHEAD def\nbranch refs/heads/feature\n\n",
    ));
    const first = listProjectWorktrees(config, project);
    const second = listProjectWorktrees(config, project);
    await expect(first).resolves.toEqual([expect.objectContaining({ path: "/repo-feature", branch: "feature" })]);
    await expect(second).resolves.toEqual(await first);
    expect(listWorktrees).toHaveBeenCalledTimes(1);
  });

  it("retries a listing when creation invalidates its generation before resolution", async () => {
    let resolveFirst!: (value: CommandRecord) => void;
    listWorktrees.mockReturnValueOnce(new Promise<CommandRecord>((resolve) => { resolveFirst = resolve; }));
    listWorktrees.mockResolvedValueOnce(record("worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n"));
    const listing = listProjectWorktrees(config, project);
    createWorktree.mockResolvedValueOnce({ exitCode: 0, path: "/repo-feature", branch: "feature" });
    await createProjectionWorktree(config, project, { branch: "feature" });
    resolveFirst(record("worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n"));
    await expect(listing).resolves.toEqual([]);
    expect(listWorktrees).toHaveBeenCalledTimes(2);
  });

  it("compares discovery fields and partitions shared topology once", () => {
    expect(normalizePath("\\repo\\")).toBe("/repo");
    expect(worktreeMapsEqual(
      new Map([["/repo", [worktree("/feature", "main")]]]),
      new Map([["/repo", [worktree("/feature", "other")]]]),
    )).toBe(false);

    const result = partitionWorktreesByRegisteredProject(
      [{ path: "/repo" }, { path: "/feature" }],
      new Map([
        ["/repo", [worktree("/feature", "feature"), worktree("/loose", "loose")]],
        ["/feature", [worktree("/repo", "main"), worktree("/loose", "loose")]],
      ]),
    );
    expect([...result.keys()]).toEqual(["/repo"]);
    expect(result.get("/repo")?.map((entry) => entry.path)).toEqual(["/loose"]);
  });
});
