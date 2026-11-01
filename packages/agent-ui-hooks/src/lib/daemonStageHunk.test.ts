import { afterEach, describe, expect, it, vi } from "vitest";

import * as daemon from "@rightkit/agent-ui-core/lib/daemon";
import type { StageDiffHunkPayload } from "../components/workbench/DiffView";

describe("stageDiffHunk", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("posts a hunk decision to the session tool-approval hunk endpoint", async () => {
    const stageDiffHunk = (
      daemon as {
        stageDiffHunk?: (
          config: { url: string; token?: string },
          sessionId: string,
          body: StageDiffHunkPayload,
        ) => Promise<void>;
      }
    ).stageDiffHunk;

    expect(stageDiffHunk).toBeTypeOf("function");

    const fetchMock = vi.fn(async () => new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);

    const body: StageDiffHunkPayload = {
      toolCallId: "tool-7",
      decision: "approve",
      hunkId: "src/example.ts:0",
      hunkIndex: 0,
      filePath: "src/example.ts",
      hunkHeader: "@@ -10,2 +10,2 @@",
      patch: "@@ -10,2 +10,2 @@\n-old\n+new",
    };

    await stageDiffHunk?.({ url: "http://daemon.test/", token: "secret" }, "session 1", body);

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe("http://daemon.test/sessions/session%201/tool-approval/hunks");
    expect(init.method).toBe("POST");
    expect(init.body).toBe(JSON.stringify(body));
    const headers = new Headers(init.headers);
    expect(headers.get("authorization")).toBe("Bearer secret");
    expect(headers.get("content-type")).toBe("application/json");
  });
});
