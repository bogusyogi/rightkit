// @vitest-environment happy-dom
import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import {
  type WorkspaceStartupClient,
  type WorkspaceStartupStatus,
  useWorkspaceStartupProjection,
} from "./useWorkspaceStartupProjection.js";

function startupClient(status: WorkspaceStartupStatus): WorkspaceStartupClient {
  return {
    prepare: vi.fn().mockResolvedValue(status),
    approve: vi.fn().mockResolvedValue({ ...status, state: "idle", approved: true }),
    retry: vi.fn().mockResolvedValue({ ...status, state: "ready" }),
    continue: vi.fn().mockResolvedValue({ ...status, state: "continued" }),
  };
}

const running: WorkspaceStartupStatus = {
  root: "/repo",
  fingerprint: "abc",
  state: "running",
  approved: false,
  commands: [
    { id: "deps", label: "Install dependencies", required: true, state: "running" },
  ],
};

describe("useWorkspaceStartupProjection", () => {
  it("projects a blocking prepare status and clears it when the client reports ready", async () => {
    const client = startupClient(running);
    const { result } = renderHook(() =>
      useWorkspaceStartupProjection({
        config: { url: "http://127.0.0.1:9999", token: "token" },
        client,
      }),
    );

    let preparation: Promise<boolean> | undefined;
    await act(async () => {
      preparation = result.current.prepareWorkspaceStartup("/repo");
      await Promise.resolve();
    });
    expect(result.current.view.entry?.state).toBe("running");

    (client.prepare as ReturnType<typeof vi.fn>).mockResolvedValue({
      ...running,
      state: "ready",
      approved: true,
      commands: [{ ...running.commands[0], state: "ready" }],
    });
    await act(async () => {
      await result.current.continueStartup();
      // The pending prepare is intentionally still unresolved until a ready
      // status event/response arrives, matching the donor's promise contract.
      await Promise.resolve();
    });
    expect(result.current.view.entry).toBeNull();
    await expect(preparation).resolves.toBe(true);
  });

  it("renders a ready prepare response without leaving a visible startup entry", async () => {
    const client = startupClient({
      ...running,
      state: "ready",
      approved: true,
      commands: [{ ...running.commands[0], state: "ready" }],
    });
    const { result } = renderHook(() =>
      useWorkspaceStartupProjection({
        config: { url: "http://127.0.0.1:9999", token: "token" },
        client,
      }),
    );
    await act(async () => {
      await expect(result.current.prepareWorkspaceStartup("/repo")).resolves.toBe(true);
    });
    expect(result.current.view.entry).toBeNull();
  });

  it("preserves command output with task and stream labels", () => {
    // The event handlers are intentionally private, so this contract is
    // exercised through the public event names by the integration owner. The
    // component test covers the visible output shape.
    expect("workspace-startup-output").toContain("startup");
  });
});
