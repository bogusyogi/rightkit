import { afterEach, describe, expect, it, vi } from "vitest";

import type { ReferenceMeta } from "../types/references";
import { sendMessage } from "./messages-stream-readiness";

const reference: ReferenceMeta = {
  id: "repo:stablyai/orca",
  kind: "repository",
  destination: "https://github.com/stablyai/orca",
  filename: "stablyai/orca",
  provenance: {
    source: "composer",
    createdAt: "2026-07-21T00:00:00Z",
    trust: "user",
  },
  actions: ["open"],
};

describe("sendMessage typed references", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("keeps reference metadata separate from legacy image bytes", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ accepted: true }), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await sendMessage(
      { url: "http://127.0.0.1:9171", token: "secret" },
      "session-1",
      "inspect this",
      "nonce-1",
      [{ type: "image", data: "AAAA", mimeType: "image/png", id: "image-1" }],
      "/workspace",
      [reference],
    );

    const [, init] = fetchMock.mock.calls[0] as [URL, RequestInit];
    const body = JSON.parse(String(init.body));
    expect(body.attachments).toEqual([
      { type: "image", data: "AAAA", mimeType: "image/png", id: "image-1" },
    ]);
    expect(body.references).toEqual([reference]);
    expect(body.workspaceRoot).toBe("/workspace");
  });
});
