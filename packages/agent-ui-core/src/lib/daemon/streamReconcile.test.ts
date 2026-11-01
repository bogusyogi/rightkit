import { describe, expect, it } from "vitest";

import type { ProtocolEvent, TranscriptMessage } from "../types";
import {
  appendProtocolEvent,
  appendTranscriptMessage,
  computeHighWaterSeqFromTranscript,
  createStreamReconciler,
  isTerminalStreamEvent,
  shouldFlushBeforeEvent,
} from "./streamReconcile";

const createdAt = "2026-08-29T00:00:00Z";

function event($kind: ProtocolEvent["$kind"], seq: number, sessionId = "s1"): ProtocolEvent {
  return { $kind, seq, sessionId, createdAt } as ProtocolEvent;
}

function userMessage(id: string): TranscriptMessage {
  return { $kind: "user", id, createdAt, content: id, pinned: false };
}

describe("stream transcript reconciliation", () => {
  it("deduplicates transcript rows by message id, not event sequence", () => {
    const first = userMessage("message-7");
    const replacement = { ...first, content: "replayed content" };

    expect(appendTranscriptMessage([first], replacement)).toEqual([first]);
    expect(appendTranscriptMessage([first], userMessage("message-8"))).toHaveLength(2);
  });

  it("filters internal blueprint rows at the renderer boundary", () => {
    const internal: TranscriptMessage = {
      $kind: "assistant",
      id: "blueprint-init-1",
      createdAt,
      content: "internal",
      toolCalls: [],
      pinned: false,
    };

    expect(appendTranscriptMessage([], internal)).toEqual([]);
  });

  it("deduplicates replayed protocol frames by durable sequence", () => {
    const first = event("sessionStarted", 4);
    const replay = { ...first, title: "changed by replay" } as ProtocolEvent;

    expect(appendProtocolEvent([first], replay)).toEqual([first]);
    expect(appendProtocolEvent([first], event("sessionTurnFinished", 5))).toHaveLength(2);
  });

  it("recovers the greatest parseable transcript cursor conservatively", () => {
    const rows = [
      userMessage("message-12"),
      userMessage("assistant:18"),
      userMessage("tool:call:21"),
      userMessage("unparseable-id"),
      userMessage("message-4"),
    ];

    expect(computeHighWaterSeqFromTranscript(rows)).toBe(21);
  });

  it("marks terminal and synchronous tool frames as flush boundaries", () => {
    expect(isTerminalStreamEvent(event("assistantMessageCompleted", 1))).toBe(true);
    expect(isTerminalStreamEvent(event("sessionTurnFinished", 2))).toBe(true);
    expect(isTerminalStreamEvent(event("runInterrupted", 3))).toBe(true);
    expect(isTerminalStreamEvent(event("runFailed", 4))).toBe(true);
    expect(isTerminalStreamEvent(event("budgetExceeded", 5))).toBe(true);
    expect(shouldFlushBeforeEvent(event("toolCallStarted", 6))).toBe(true);
    expect(shouldFlushBeforeEvent(event("usageUpdated", 7))).toBe(false);
  });
});

describe("StreamReconciler", () => {
  it("accepts only new frames for the active session and generation", () => {
    const reconciler = createStreamReconciler();
    const scope = reconciler.openSession("s1", 4);

    expect(reconciler.accept(event("sessionStarted", 4), scope)).toBe(false);
    expect(reconciler.accept(event("assistantDelta", 5), scope)).toBe(true);
    expect(reconciler.accept(event("assistantDelta", 5), scope)).toBe(false);
    expect(reconciler.accept(event("assistantDelta", 6, "s2"), scope)).toBe(false);
    expect(reconciler.snapshot().lastSeq).toBe(5);
  });

  it("rejects late callbacks from a closed/reopened stream", () => {
    const reconciler = createStreamReconciler();
    const oldScope = reconciler.openSession("s1");
    const newScope = reconciler.openSession("s1");

    expect(reconciler.accept(event("assistantDelta", 1), oldScope)).toBe(false);
    expect(reconciler.accept(event("assistantDelta", 1), newScope)).toBe(true);
  });

  it("sorts a catch-up page and admits each event once", () => {
    const reconciler = createStreamReconciler();
    const scope = reconciler.seedFromTranscript("s1", [userMessage("message-3")]);
    const accepted: number[] = [];

    expect(
      reconciler.acceptHistory(
        [event("assistantDelta", 5), event("assistantDelta", 4), event("assistantDelta", 5)],
        (frame) => accepted.push(frame.seq),
        scope,
      ),
    ).toHaveLength(2);
    expect(accepted).toEqual([4, 5]);
    expect(reconciler.snapshot().lastSeq).toBe(5);
  });
});
