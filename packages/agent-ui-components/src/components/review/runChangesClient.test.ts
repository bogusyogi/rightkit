import { afterEach, describe, expect, it, vi } from "vitest";

import { getRunChangesInventory, getRunChangesSummary } from "./runChangesClient";

const config = { url: "http://127.0.0.1:8765", token: "token" };

afterEach(() => vi.unstubAllGlobals());

describe("runChangesClient", () => {
  it("uses daemon snake_case query fields for inventory & summary", async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({
        run_id: "run-1",
        workspace_root: "/workspace",
        entries: [],
        entry_count: 0,
      }),
    });
    vi.stubGlobal("fetch", fetchMock);

    const inventory = await getRunChangesInventory(config, {
      runId: "run-1",
      workspaceRoot: "/workspace",
      maxDiffBytes: 128,
    });
    const summary = await getRunChangesSummary(config, {
      runId: "run-1",
      workspaceRoot: "/workspace",
    });

    expect(String(fetchMock.mock.calls[0]?.[0])).toContain(
      "run_id=run-1&workspace_root=%2Fworkspace&max_diff_bytes=128",
    );
    expect(String(fetchMock.mock.calls[1]?.[0])).toContain(
      "run_id=run-1&workspace_root=%2Fworkspace",
    );
    expect(inventory.runId).toBe("run-1");
    expect(summary.entryCount).toBe(0);
  });
});
