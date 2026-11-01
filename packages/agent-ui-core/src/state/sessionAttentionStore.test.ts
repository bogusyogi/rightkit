import { beforeEach, describe, expect, it } from "vitest";

import type { SessionRow } from "../lib/types.js";
import {
  attentionStateForSession,
  isActionableAttention,
  useSessionAttentionStore,
} from "./sessionAttentionStore.js";

const row = (id: string, status = "idle"): SessionRow =>
  ({
    id,
    title: id,
    model: "local",
    workspaceRoot: "/workspace",
    createdAt: "2026-01-01T00:00:00Z",
    updatedAt: "2026-01-01T00:00:00Z",
    status,
  }) as SessionRow;

beforeEach(() => {
  useSessionAttentionStore.getState().reset();
});

describe("session attention", () => {
  it("normalizes daemon row statuses into one shared projection", () => {
    useSessionAttentionStore.getState().syncSessions([
      row("running", "in_progress"),
      row("blocked", "needs_input"),
      row("failed", "failed"),
    ]);
    const states = useSessionAttentionStore.getState().bySessionId;
    expect(states.running).toBe("running");
    expect(states.blocked).toBe("needs-input");
    expect(states.failed).toBe("failed");
  });

  it("persists an unread completion but never marks the focused session unread", () => {
    const store = useSessionAttentionStore.getState();
    store.setFocusedSession("open");
    store.markFinished("open");
    store.markFinished("away");

    expect(useSessionAttentionStore.getState().bySessionId.open).not.toBe("unread");
    expect(useSessionAttentionStore.getState().bySessionId.away).toBe("unread");
    expect(JSON.parse(localStorage.getItem("cr.session-attention.v1") ?? "{}").unreadSessionIds).toEqual([
      "away",
    ]);
  });

  it("clears the durable unread marker when a session is opened", () => {
    const store = useSessionAttentionStore.getState();
    store.markUnread("s1");
    store.setFocusedSession("s1");

    expect(useSessionAttentionStore.getState().bySessionId.s1).toBe("idle");
    expect(JSON.parse(localStorage.getItem("cr.session-attention.v1") ?? "{}").unreadSessionIds).toEqual([]);
  });

  it("retains an away summary independently of the durable unread marker", () => {
    const summary = {
      sessionId: "away",
      sinceIndex: 0,
      nextIndex: 2,
      messageCount: 2,
      userMessages: 1,
      assistantMessages: 1,
      toolResults: 0,
      errorCount: 0,
      latestAt: "2026-01-01T00:00:00Z",
      hasActivity: true,
      bullets: ["Finished the review"],
    };
    const store = useSessionAttentionStore.getState();
    store.setAwaySummary("away", summary);
    expect(useSessionAttentionStore.getState().awaySummaryBySessionId.away).toEqual(summary);
    store.forgetSession("away");
    expect(useSessionAttentionStore.getState().awaySummaryBySessionId.away).toBeUndefined();
  });

  it("keeps live failure and attention states actionable", () => {
    expect(isActionableAttention("failed")).toBe(true);
    expect(isActionableAttention("needs-input")).toBe(true);
    expect(isActionableAttention("background")).toBe(false);
  });

  it("falls back to a draft without a session id and honors a live override", () => {
    expect(attentionStateForSession(null, "running")).toBe("draft");
    expect(attentionStateForSession("s1", "idle", { s1: "background" })).toBe("background");
  });
});
