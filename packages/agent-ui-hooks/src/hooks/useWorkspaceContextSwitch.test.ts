// @vitest-environment happy-dom
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useWorkspaceContextSwitch } from "./useWorkspaceContextSwitch.js";
import { useProjectRecentsStore } from "@rightkit/agent-ui-core/state/projectRecentsStore";
import { useWorkspaceStore } from "@rightkit/agent-ui-core/state/workspaceStore";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

describe("useWorkspaceContextSwitch", () => {
  beforeEach(() => {
    useProjectRecentsStore.getState().clear();
    useWorkspaceStore.getState().resetForWorkspace("");
  });

  it("stages a draft workspace and records it as a recent", async () => {
    const activeSessionIdRef = { current: null as string | null };
    const onWorkspaceChange = vi.fn();
    const { result } = renderHook(() =>
      useWorkspaceContextSwitch({
        activeSessionIdRef,
        workspaceRoot: "",
        onWorkspaceChange,
      }),
    );

    let outcome: Awaited<ReturnType<typeof result.current.changeWorkspace>> | undefined;
    await act(async () => {
      outcome = await result.current.changeWorkspace("C:\\Work\\Demo\\");
    });

    expect(outcome).toEqual({ kind: "staged", workspaceRoot: "C:/Work/Demo" });
    expect(onWorkspaceChange).toHaveBeenCalledWith("C:/Work/Demo");
    expect(useWorkspaceStore.getState().workspaceRoot).toBe("C:/Work/Demo");
    expect(useProjectRecentsStore.getState().activePath).toBe("C:/Work/Demo");
  });

  it("ignores late draft normalization after a newer request or session wins", async () => {
    const pending = deferred<{ workspaceRoot?: string; branch?: string }>();
    const activeSessionIdRef = { current: null as string | null };
    const onWorkspaceChange = vi.fn();
    const { result } = renderHook(() =>
      useWorkspaceContextSwitch({
        activeSessionIdRef,
        workspaceRoot: "",
        onWorkspaceChange,
        resolveDraftWorkspace: vi.fn(() => pending.promise),
      }),
    );

    let request!: Promise<unknown>;
    await act(async () => {
      request = result.current.changeWorkspace("/stale-workspace");
    });
    activeSessionIdRef.current = "session-now-focused";
    pending.resolve({ workspaceRoot: "/normalized-stale-workspace", branch: "main" });
    await act(async () => {
      await request;
    });

    expect(onWorkspaceChange).toHaveBeenCalledTimes(1);
    expect(onWorkspaceChange).toHaveBeenCalledWith("/stale-workspace");
    expect(useWorkspaceStore.getState().workspaceRoot).toBe("/stale-workspace");
  });

  it("does not let an older live-session request overwrite a newer request", async () => {
    const first = deferred<{ workspaceRoot?: string; branch?: string }>();
    const second = deferred<{ workspaceRoot?: string; branch?: string }>();
    const setSessionWorkspace = vi
      .fn()
      .mockImplementationOnce(() => first.promise)
      .mockImplementationOnce(() => second.promise);
    const activeSessionIdRef = { current: "session-1" };
    const onWorkspaceChange = vi.fn();
    const { result } = renderHook(() =>
      useWorkspaceContextSwitch({
        activeSessionIdRef,
        workspaceRoot: "/current",
        onWorkspaceChange,
        setSessionWorkspace,
      }),
    );

    let firstRequest!: Promise<unknown>;
    let secondRequest!: Promise<unknown>;
    await act(async () => {
      firstRequest = result.current.changeWorkspace("/first");
      secondRequest = result.current.changeWorkspace("/second");
    });
    first.resolve({ workspaceRoot: "/normalized-first", branch: "old" });
    await act(async () => {
      await firstRequest;
    });
    expect(onWorkspaceChange).not.toHaveBeenCalled();

    second.resolve({ workspaceRoot: "/normalized-second", branch: "new" });
    await act(async () => {
      await secondRequest;
    });
    expect(onWorkspaceChange).toHaveBeenCalledWith("/normalized-second");
  });

  it("fails honestly instead of mutating an active session without a daemon seam", async () => {
    const onWorkspaceChange = vi.fn();
    const onUnavailable = vi.fn();
    const { result } = renderHook(() =>
      useWorkspaceContextSwitch({
        activeSessionIdRef: { current: "session-1" },
        workspaceRoot: "/current",
        onWorkspaceChange,
        onUnavailable,
      }),
    );

    let outcome: Awaited<ReturnType<typeof result.current.changeWorkspace>> | undefined;
    await act(async () => {
      outcome = await result.current.changeWorkspace("/other");
    });

    expect(outcome).toEqual({
      kind: "unavailable",
      reason: "The daemon does not expose session workspace switching yet",
    });
    expect(onWorkspaceChange).not.toHaveBeenCalled();
    expect(onUnavailable).toHaveBeenCalled();
  });
});
