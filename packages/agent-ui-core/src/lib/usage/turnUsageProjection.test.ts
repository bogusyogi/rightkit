import { describe, expect, it } from "vitest";
import type { ProtocolEvent } from "../types";
import {
  projectLastTurnUsage,
  projectTurnUsage,
  usageUpdatedEvents,
  type UsageUpdatedEvent,
} from "./turnUsageProjection";

function usage(seq: number, values: Partial<UsageUpdatedEvent> = {}): UsageUpdatedEvent {
  return {
    $kind: "usageUpdated",
    sessionId: "session-1",
    seq,
    createdAt: `2026-08-29T00:00:0${seq}Z`,
    promptTokens: 100,
    completionTokens: 20,
    costUsd: 0.01,
    contextTokens: 120,
    contextPercent: 0.2,
    cacheReadTokens: 5,
    cacheCreationTokens: 2,
    ...values,
  };
}

describe("turnUsageProjection", () => {
  it("keeps only usageUpdated events from a App protocol stream", () => {
    const compacted: ProtocolEvent = {
      $kind: "sessionCompacted",
      sessionId: "session-1",
      seq: 2,
      createdAt: "2026-08-29T00:00:02Z",
      summary: "summary",
      droppedCount: 1,
    };
    expect(usageUpdatedEvents([usage(1), compacted])).toHaveLength(1);
  });

  it("differences cumulative counters into the last turn", () => {
    const result = projectTurnUsage(
      usage(2, {
        promptTokens: 350,
        completionTokens: 80,
        costUsd: 0.035,
        contextTokens: 430,
        contextPercent: 0.43,
        cacheReadTokens: 15,
        cacheCreationTokens: 6,
      }),
      usage(1),
    );
    expect(result).toEqual({
      inputTokens: 250,
      outputTokens: 60,
      cacheReadTokens: 10,
      cacheCreationTokens: 4,
      costUsd: 0.025,
      contextTokens: 430,
      contextPercent: 0.43,
    });
  });

  it("fails safe on a reset instead of showing negative usage", () => {
    const result = projectLastTurnUsage([usage(1, { promptTokens: 90 }), usage(2, { promptTokens: 4 })]);
    expect(result?.inputTokens).toBe(4);
  });

  it("returns null when a session has no completed usage event", () => {
    expect(projectLastTurnUsage([])).toBeNull();
  });
});
