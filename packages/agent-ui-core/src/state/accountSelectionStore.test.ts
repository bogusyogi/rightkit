import { afterEach, describe, expect, it, vi } from "vitest";
import { useAccountSelectionStore } from "./accountSelectionStore.js";

const config = { url: "http://127.0.0.1:1", token: "t" };

afterEach(() => {
  vi.unstubAllGlobals();
  useAccountSelectionStore.setState({ chat: {}, project: {} });
});

describe("account selection persistence", () => {
  it("PUTs to the daemon before caching, and does not cache a rejected choice", async () => {
    const ok = vi.fn().mockImplementation(async () => new Response("{}"));
    vi.stubGlobal("fetch", ok);
    await useAccountSelectionStore
      .getState()
      .selectChatAccount(config, "s1", "subscription/claude/a");
    expect(ok.mock.calls[0][0]).toBe("http://127.0.0.1:1/accounts/session/s1");
    expect(useAccountSelectionStore.getState().resolveAccount("s1", null)).toBe(
      "subscription/claude/a",
    );

    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation(async () => new Response("{}", { status: 404 })),
    );
    await expect(
      useAccountSelectionStore
        .getState()
        .selectChatAccount(config, "s2", "subscription/claude/ghost"),
    ).rejects.toThrow();
    expect(useAccountSelectionStore.getState().resolveAccount("s2", null)).toBeNull();
  });

  it("persists the project default through the daemon", async () => {
    const fetchMock = vi.fn().mockImplementation(async () => new Response("{}"));
    vi.stubGlobal("fetch", fetchMock);
    await useAccountSelectionStore
      .getState()
      .selectProjectDefault(config, "/p", "api_key/openai/team");
    expect(fetchMock.mock.calls[0][0]).toBe("http://127.0.0.1:1/accounts/project-default");
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({
      project: "/p",
      accountId: "api_key/openai/team",
    });
    expect(useAccountSelectionStore.getState().resolveAccount(null, "/p")).toBe(
      "api_key/openai/team",
    );
  });

  it("syncs from the daemon and drops a vanished selection", async () => {
    useAccountSelectionStore.setState({ chat: { s1: "subscription/claude/gone" }, project: {} });
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation(
        async () =>
          new Response(
            JSON.stringify({
              accountId: "subscription/claude/gone",
              source: "session",
              account: null,
              unavailable: true,
            }),
          ),
      ),
    );
    await useAccountSelectionStore.getState().syncFromDaemon(config, "s1", null);
    expect(useAccountSelectionStore.getState().resolveAccount("s1", null)).toBeNull();
  });
});
