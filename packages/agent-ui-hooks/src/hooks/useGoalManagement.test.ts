// @vitest-environment happy-dom
import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { DaemonConfig, GoalSnapshotV1, ProtocolEvent } from "@rightkit/agent-ui-core/lib/types";

vi.mock("@rightkit/agent-ui-core/lib/daemon", () => ({
  clearGoal: vi.fn(),
  editGoal: vi.fn(),
  getGoalSnapshot: vi.fn(),
  orchestrate: vi.fn(),
  pauseGoal: vi.fn(),
  resumeGoal: vi.fn(),
  setGoal: vi.fn(),
}));

import { getGoalSnapshot } from "@rightkit/agent-ui-core/lib/daemon";
import { useGoalManagement } from "@rightkit/agent-ui-core/hooks/useGoalManagement";

const config: DaemonConfig = { url: "http://127.0.0.1:0", token: "test" };

function goal(goalId: string): GoalSnapshotV1 {
  return {
    goalId,
    objective: `Objective ${goalId}`,
    status: "active",
    startedAt: "2026-08-11T00:00:00Z",
    closedAt: null,
    dismissedAt: null,
    decompositionState: "usable",
    planRevision: 2,
    completionPercent: 40,
    now: "Now",
    next: "Next",
    steps: [],
    verificationState: "pending",
    budgetTokens: 100,
    tokensUsed: 40,
    continuationCount: 0,
    noProgressCount: 0,
  };
}

function deferred<T>() {
  let resolve: (value: T) => void = () => undefined;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

describe("useGoalManagement session guards", () => {
  it("does not commit stale hydration after a session change", async () => {
    const alpha = deferred<GoalSnapshotV1 | null>();
    const beta = deferred<GoalSnapshotV1 | null>();
    vi.mocked(getGoalSnapshot).mockImplementation((_config, sessionId) =>
      sessionId === "alpha" ? alpha.promise : beta.promise,
    );
    const { result, rerender } = renderHook(
      ({ sessionId }) =>
        useGoalManagement({ config, sessionId, formatError: String, onStatus: vi.fn() }),
      { initialProps: { sessionId: "alpha" } },
    );

    rerender({ sessionId: "beta" });
    await act(async () => beta.resolve(goal("beta")));
    await waitFor(() => expect(result.current.activeGoal?.goalId).toBe("beta"));
    await act(async () => alpha.resolve(goal("alpha")));
    expect(result.current.activeGoal?.goalId).toBe("beta");
  });

  it("ignores GoalProgressUpdated events from a different session", async () => {
    vi.mocked(getGoalSnapshot).mockResolvedValue(goal("alpha"));
    const { result } = renderHook(() =>
      useGoalManagement({ config, sessionId: "alpha", formatError: String, onStatus: vi.fn() }),
    );
    await waitFor(() => expect(result.current.activeGoal?.goalId).toBe("alpha"));
    const event: ProtocolEvent = {
      $kind: "goalProgressUpdated",
      sessionId: "beta",
      seq: 1,
      createdAt: "2026-08-11T00:00:00Z",
      ...goal("beta"),
    };
    act(() => result.current.applyGoalEvent(event));
    expect(result.current.activeGoal?.goalId).toBe("alpha");
  });
});
