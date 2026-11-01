// D1 — workspaceFiles daemon client tests.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { getWorkspaceTree, postWorkspaceTreeExpand } from "@rightkit/agent-ui-core/lib/daemon/workspace-files";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";

const CONFIG: DaemonConfig = { url: "http://127.0.0.1:5057", token: "abc" };

function mockJsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("workspaceFiles daemon client", () => {
  const originalFetch = global.fetch;
  beforeEach(() => {
    vi.restoreAllMocks();
  });
  afterEach(() => {
    global.fetch = originalFetch;
  });

  it("builds the tree GET URL with workspaceRoot and path", async () => {
    let captured: Request | null = null;
    global.fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      captured = new Request(input, init);
      return mockJsonResponse({
        workspaceRoot: "/root",
        path: "src",
        entries: [],
        truncated: false,
        lastKnown: false,
      });
    }) as typeof fetch;

    const response = await getWorkspaceTree(CONFIG, "/root", "src");
    expect(response.path).toBe("src");
    expect(captured).not.toBeNull();
    const url = new URL((captured as unknown as Request).url);
    expect(url.pathname).toBe("/workspace/tree");
    expect(url.searchParams.get("workspaceRoot")).toBe("/root");
    expect(url.searchParams.get("path")).toBe("src");
    const headers = (captured as unknown as Request).headers;
    expect(headers.get("authorization")).toBe("Bearer abc");
  });

  it("serializes the tree expand body as JSON", async () => {
    let captured: Request | null = null;
    global.fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      captured = new Request(input as RequestInfo, init);
      return mockJsonResponse({
        workspaceRoot: "/root",
        path: "",
        entries: [],
        truncated: false,
        lastKnown: false,
      });
    }) as typeof fetch;

    await postWorkspaceTreeExpand(CONFIG, "/root");
    expect(captured).not.toBeNull();
    const req = captured as unknown as Request;
    expect(req.method).toBe("POST");
    expect(req.headers.get("content-type")).toBe("application/json");
    const text = await req.text();
    expect(JSON.parse(text)).toEqual({ workspaceRoot: "/root", path: null });
  });

  it("throws a typed error with status and reason on a non-OK response", async () => {
    global.fetch = vi.fn(async () => {
      return new Response("forbidden: secret file `.env`", { status: 403 });
    }) as typeof fetch;
    await expect(getWorkspaceTree(CONFIG, "/root")).rejects.toMatchObject({
      status: 403,
    });
  });
});
