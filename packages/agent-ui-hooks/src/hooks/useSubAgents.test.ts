import { describe, expect, it } from "vitest";

import { reduceSubAgents, type SubAgentState } from "@rightkit/agent-ui-core/hooks/useSubAgents";
import type { ProtocolEvent } from "@rightkit/agent-ui-core/lib/types";

function rosterEvent(status: "running" | "done" | "failed" | "cancelled"): ProtocolEvent {
  return {
    $kind: "agentRosterUpdated",
    sessionId: "parent",
    seq: 2,
    createdAt: "2026-07-08T00:00:00Z",
    agents: [
      {
        subSessionId: "sub-1",
        role: "auditor",
        status,
        tokensIn: 10,
        tokensOut: 5,
        cacheRead: 1,
        toolUses: 3,
        elapsedMs: 1000,
        lastTools: ["read_file"],
      },
    ],
  };
}

const runningAgent: SubAgentState = {
  subSessionId: "sub-1",
  role: "auditor",
  taskTitle: "Audit the repo",
  status: "running",
  startedAt: 1,
};

describe("reduceSubAgents", () => {
  it("uses failed roster status to close a stale running agent", () => {
    const next = reduceSubAgents([runningAgent], rosterEvent("failed"), 2);

    expect(next).toHaveLength(1);
    expect(next[0]).toMatchObject({
      subSessionId: "sub-1",
      status: "completed",
      success: false,
      toolUses: 3,
      lastTools: ["read_file"],
    });
  });

  it("uses done roster status to close a stale running agent", () => {
    const next = reduceSubAgents([runningAgent], rosterEvent("done"), 2);
    expect(next).toHaveLength(1);
    expect(next[0]).toMatchObject({
      subSessionId: "sub-1",
      status: "completed",
      success: true,
    });
  });

  it("does not regress a terminal agent back to running from an older roster row", () => {
    const next = reduceSubAgents(
      [{ ...runningAgent, status: "completed", success: false }],
      rosterEvent("running"),
      2,
    );

    expect(next[0]).toMatchObject({
      status: "completed",
      success: false,
    });
  });

  it("keeps a user-cancelled agent stopped when a later completion event arrives", () => {
    const cancelled = reduceSubAgents(
      [runningAgent],
      {
        $kind: "subAgentCancelled",
        sessionId: "parent",
        seq: 3,
        createdAt: "2026-07-08T00:00:01Z",
        subSessionId: "sub-1",
        role: "auditor",
        cancelledBy: "user",
      },
      2,
    );

    const completedAfterCancel = reduceSubAgents(
      cancelled,
      {
        $kind: "subAgentCompleted",
        sessionId: "parent",
        seq: 4,
        createdAt: "2026-07-08T00:00:02Z",
        subSessionId: "sub-1",
        role: "auditor",
        success: false,
        summary: "sub-agent interrupted",
      },
      3,
    );

    expect(completedAfterCancel[0]).toMatchObject({
      status: "cancelled",
      cancelledBy: "user",
      summary: "sub-agent interrupted",
    });
  });
});
