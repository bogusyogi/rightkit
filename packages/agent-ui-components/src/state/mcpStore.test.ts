import { describe, expect, it } from "vitest";

import { mcpConfigFromDraft, parseMcpChannels } from "./mcpStore";

describe("mcp store helpers", () => {
  it("normalizes channel allowlists into unique lowercase channel names", () => {
    expect(parseMcpChannels(" Desktop,CLI desktop\nweb,, ")).toEqual(["desktop", "cli", "web"]);
  });

  it("builds stdio configs with trimmed commands and split args", () => {
    expect(
      mcpConfigFromDraft({
        name: "local",
        scope: "workspace",
        type: "stdio",
        trustPreset: "local_process",
        channelAllowlist: "desktop",
        allowMutating: false,
        url: "",
        command: "  node  ",
        args: " server.mjs --stdio ",
      }),
    ).toEqual({
      type: "stdio",
      command: "node",
      args: ["server.mjs", "--stdio"],
      env: {},
    });
  });

  it("rejects incomplete transport details before save", () => {
    expect(
      mcpConfigFromDraft({
        name: "remote",
        scope: "user",
        type: "http",
        trustPreset: "remote_http",
        channelAllowlist: "desktop",
        allowMutating: false,
        url: "   ",
        command: "",
        args: "",
      }),
    ).toBeNull();
  });
});
