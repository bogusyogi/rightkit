import { beforeEach, describe, expect, it, vi } from "vitest";

import { usePermissionsStore } from "./permissionsStore";

const daemon = vi.hoisted(() => ({
  getWorkspaceTrust: vi.fn(),
  listPermissionRules: vi.fn(),
}));

vi.mock("@rightkit/agent-ui-core/lib/daemon", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@rightkit/agent-ui-core/lib/daemon")>();
  return {
    ...actual,
    getWorkspaceTrust: daemon.getWorkspaceTrust,
    listPermissionRules: daemon.listPermissionRules,
  };
});

describe("permissionsStore workspace trust", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    usePermissionsStore.getState().resetWorkspaceTrust();
    usePermissionsStore.setState({ rules: [] });
  });

  it("ignores a stale trust response after workspace changes", async () => {
    let resolveFirst:
      | ((value: {
          workspaceRoot: string;
          trust: "trusted";
          trusted: true;
          requiresTrust: false;
        }) => void)
      | undefined;
    daemon.getWorkspaceTrust
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveFirst = resolve;
          }),
      )
      .mockResolvedValueOnce({
        workspaceRoot: "/repo-b",
        trust: "sandboxed",
        trusted: false,
        requiresTrust: false,
      });
    const config = { url: "http://127.0.0.1:1", token: "token" };
    const setStatus = vi.fn();

    const first = usePermissionsStore.getState().loadWorkspaceTrust(config, "/repo-a", setStatus);
    await usePermissionsStore.getState().loadWorkspaceTrust(config, "/repo-b", setStatus);
    resolveFirst?.({
      workspaceRoot: "/repo-a",
      trust: "trusted",
      trusted: true,
      requiresTrust: false,
    });
    await first;

    expect(usePermissionsStore.getState().workspaceTrust).toMatchObject({
      workspaceRoot: "/repo-b",
      trust: "sandboxed",
    });
    expect(usePermissionsStore.getState().workspaceTrustRoot).toBe("/repo-b");
  });

  it("clears pending trust state when reset", () => {
    usePermissionsStore.setState({ workspaceTrustPending: "locked" });
    usePermissionsStore.getState().resetWorkspaceTrust();
    expect(usePermissionsStore.getState().workspaceTrustPending).toBeNull();
  });

  it("ignores a stale live-rule response after the selected session changes", async () => {
    let resolveFirst:
      | ((value: Array<{ index: number; pattern: string; effect: "allow"; source: string }>) => void)
      | undefined;
    daemon.listPermissionRules
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            resolveFirst = resolve;
          }),
      )
      .mockResolvedValueOnce([
        { index: 0, pattern: "session-b-tool", effect: "allow", source: "session" },
      ]);
    const config = { url: "http://127.0.0.1:1", token: "token" };
    const setStatus = vi.fn();

    const first = usePermissionsStore.getState().loadRules(config, "session-a", setStatus);
    await usePermissionsStore.getState().loadRules(config, "session-b", setStatus);
    resolveFirst?.([{ index: 0, pattern: "session-a-tool", effect: "allow", source: "session" }]);
    await first;

    expect(usePermissionsStore.getState().rules).toEqual([
      { index: 0, pattern: "session-b-tool", effect: "allow", source: "session" },
    ]);
  });
});
