import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { TranscriptMessage } from "@rightkit/agent-ui-core/lib/types";
import type { ReferenceMeta } from "@rightkit/agent-ui-core/lib/types/references";

vi.mock("@rightkit/agent-ui-core/lib/daemon", () => ({
  sendMessage: vi.fn().mockResolvedValue(undefined),
  steerMessage: vi.fn().mockResolvedValue(undefined),
}));

import { sendMessage } from "@rightkit/agent-ui-core/lib/daemon";
import { useQueuedMessages } from "./useQueuedMessages.js";

const reference: ReferenceMeta = {
  id: "skill:audit",
  kind: "skill",
  destination: "@audit",
  filename: "Audit",
  provenance: {
    source: "composer.skill-picker",
    createdAt: "2026-07-21T00:00:00Z",
    trust: "workspaceTrusted",
  },
};

describe("useQueuedMessages typed references", () => {
  beforeEach(() => vi.mocked(sendMessage).mockClear());

  it("carries references through queue, optimistic transcript, and send", async () => {
    let rows: TranscriptMessage[] = [];
    const edited = vi.fn();
    const options = {
      config: { url: "http://127.0.0.1:9171", token: "t" },
      sessionId: "session-1",
      hasPendingApproval: false,
      formatError: (_error: unknown, fallback: string) => fallback,
      onEditQueuedMessage: edited,
      onMessagesChanged: (update: React.SetStateAction<TranscriptMessage[]>) => {
        rows = typeof update === "function" ? update(rows) : update;
      },
      onStatus: vi.fn(),
      onWorking: vi.fn(),
    };

    const { result, rerender } = renderHook(
      ({ working }) => useQueuedMessages({ ...options, isWorking: working }),
      { initialProps: { working: true } },
    );

    act(() => result.current.queueMessage("run audit", [], [reference]));
    expect(result.current.queuedMessages[0]?.references).toEqual([reference]);

    rerender({ working: false });
    await waitFor(() => expect(sendMessage).toHaveBeenCalledTimes(1));

    expect(rows.at(-1)?.$kind).toBe("user");
    expect(rows.at(-1)).toMatchObject({ attachments: [reference] });
    expect(vi.mocked(sendMessage).mock.calls[0]?.[6]).toEqual([reference]);
  });

  it("returns reference metadata intact when a queued message is edited", () => {
    const edited = vi.fn();
    const { result } = renderHook(() =>
      useQueuedMessages({
        config: null,
        sessionId: null,
        isWorking: true,
        hasPendingApproval: false,
        formatError: (_error, fallback) => fallback,
        onEditQueuedMessage: edited,
        onMessagesChanged: () => {},
        onStatus: () => {},
        onWorking: () => {},
      }),
    );
    act(() => result.current.queueMessage("revise", [], [reference]));
    const id = result.current.queuedMessages[0]?.id;
    if (!id) throw new Error("queue id missing");
    act(() => result.current.editQueued(id));
    expect(edited).toHaveBeenCalledWith(expect.objectContaining({ references: [reference] }));
    expect(result.current.queuedMessages).toHaveLength(0);
  });
});
