import { describe, expect, it } from "vitest";
import type { TranscriptMessage } from "./types";
import {
  assistantToolCalls,
  isInternalRunFailure,
  isInternalTranscriptMessage,
  normalizeTranscriptMessage,
  visibleTranscriptMessages,
} from "./transcriptMessages";

describe("transcript message normalization", () => {
  it("treats persisted assistant messages without toolCalls as no-tool turns", () => {
    const message = {
      $kind: "assistant",
      id: "final",
      createdAt: "2026-07-07T17:36:52Z",
      content: "READY-FOR-DOGFOODING-SMOKE",
      pinned: false,
    } as TranscriptMessage;

    const normalized = normalizeTranscriptMessage(message);

    expect(normalized.$kind).toBe("assistant");
    if (normalized.$kind === "assistant") {
      expect(assistantToolCalls(normalized)).toEqual([]);
      expect(normalized.toolCalls).toEqual([]);
    }
  });

  it("hides blueprint bootstrap notes from user-facing transcripts", () => {
    const messages = [
      {
        $kind: "user",
        id: "u1",
        createdAt: "2026-07-07T18:08:28Z",
        content: "hello",
        pinned: false,
      },
      {
        $kind: "assistant",
        id: "blueprint-init-2026-07-07T18:08:40Z",
        createdAt: "2026-07-07T18:08:40Z",
        content:
          "Blueprint verified: OKF understanding bundle written to D:\\Claude\\coderight\\.coderight/understanding",
        pinned: false,
      },
      {
        $kind: "assistant",
        id: "final",
        createdAt: "2026-07-07T18:08:41Z",
        content: "done",
        pinned: false,
        toolCalls: [],
      },
    ] as TranscriptMessage[];

    expect(isInternalTranscriptMessage(messages[1])).toBe(true);
    expect(visibleTranscriptMessages(messages).map((message) => message.id)).toEqual([
      "u1",
      "final",
    ]);
  });

  it("keeps blueprint initialization failures out of the user-facing status channel", () => {
    expect(
      isInternalRunFailure({
        $kind: "runFailed",
        sessionId: "s1",
        seq: 12,
        createdAt: "2026-07-12T11:01:00Z",
        kind: "blueprint_init",
        error: "conducted blueprint failed: HTTP error 401",
        hint: "The workspace opened normally; blueprint initialization runs in the background.",
      }),
    ).toBe(true);
  });
});
