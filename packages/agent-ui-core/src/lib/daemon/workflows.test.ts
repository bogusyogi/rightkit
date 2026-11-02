import { describe, expect, it, vi } from "vitest";
import { listWorkflowInbox, listWorkflowRuns, listWorkflows, startWorkflowRun } from "./workflows.js";

const config = { url: "http://127.0.0.1:7101", token: "test" };

describe("workflow control-plane client", () => {
  it("uses versioned authenticated paths", async () => {
    const fetchMock = vi
      .fn()
      .mockImplementation(() => new Response(JSON.stringify({ items: [] }), { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
    await Promise.all([
      listWorkflows(config),
      listWorkflowRuns(config),
      listWorkflowInbox(config),
      startWorkflowRun(config, "wfd_demo"),
    ]);
    expect(fetchMock.mock.calls.map(([url]) => String(url))).toEqual([
      "http://127.0.0.1:7101/v1/workflows",
      "http://127.0.0.1:7101/v1/runs",
      "http://127.0.0.1:7101/v1/workflow-inbox",
      "http://127.0.0.1:7101/v1/workflows/wfd_demo/runs",
    ]);
    expect(fetchMock.mock.calls[0]?.[1]?.headers.get("authorization")).toBe("Bearer test");
  });
});
