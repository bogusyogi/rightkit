import { useState } from "react";
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { TranscriptMessage } from "@rightkit/agent-ui-core/lib/types";
import { useComposerStore } from "@rightkit/agent-ui-core/state/composerStore";
import { useReferenceStore } from "@rightkit/agent-ui-core/state/referenceStore";

vi.mock("@rightkit/agent-ui-core/lib/daemon", () => ({
  createExecutionProcess: vi.fn(),
  executeCommand: vi.fn(),
  getTranscript: vi.fn().mockResolvedValue([]),
  interruptRun: vi.fn(),
  killExecutionProcess: vi.fn(),
  orchestrate: vi.fn(),
  rewindSession: vi.fn(),
  sendMessage: vi.fn().mockResolvedValue(undefined),
  writeExecutionProcessStdin: vi.fn(),
}));

import { sendMessage } from "@rightkit/agent-ui-core/lib/daemon";
import { useAppInteractionActions } from "./useAppInteractionActions.js";

describe("useAppInteractionActions typed references", () => {
  beforeEach(() => {
    vi.mocked(sendMessage).mockClear();
    useComposerStore.setState({ composer: "inspect it", attachments: [] });
    useReferenceStore.setState({
      restoreError: null,
      references: [
        {
          id: "repo:stablyai/orca",
          kind: "repository",
          destination: "https://github.com/stablyai/orca",
          filename: "stablyai/orca",
          provenance: {
            source: "composer",
            createdAt: "2026-07-21T00:00:00Z",
            trust: "user",
          },
          actions: ["open"],
        },
      ],
    });
  });

  it("snapshots toWireMeta into the optimistic row and request before clearing", async () => {
    const { result } = renderHook(() => {
      const [messages, setMessages] = useState<TranscriptMessage[]>([]);
      const actions = useAppInteractionActions({
        config: { url: "http://127.0.0.1:9171", token: "t" },
        selected: {
          id: "session-1",
          title: "Session",
          model: "model",
          workspaceRoot: "/work/project",
          createdAt: "2026-07-21T00:00:00Z",
          updatedAt: "2026-07-21T00:00:00Z",
        },
        workspaceRoot: "/work/project",
        providerKeyMissing: false,
        isWorking: false,
        messages,
        terminalInput: "",
        selectedTerminalProcessId: null,
        composerState: useComposerStore.getState(),
        addPendingUserNonce: vi.fn(),
        queueMessage: vi.fn(),
        setMessages,
        setStreamingText: vi.fn(),
        setStreamingReasoning: vi.fn(),
        setIsWorking: vi.fn(),
        setStatus: vi.fn(),
        setPane: vi.fn(),
        setSelectedId: vi.fn(),
        setTerminalActionPending: vi.fn(),
        setTerminalError: vi.fn(),
        setSelectedTerminalProcessId: vi.fn(),
        setTerminalInput: vi.fn(),
        loadTerminalProcesses: vi.fn().mockResolvedValue(undefined),
        loadTerminalLogs: vi.fn().mockResolvedValue(undefined),
        refresh: vi.fn().mockResolvedValue(undefined),
        compact: vi.fn().mockResolvedValue(undefined),
        messageOf: (_error, fallback) => fallback,
        isEditableUserMessage: () => true,
      });
      return { actions, messages };
    });

    await act(async () => result.current.actions.submit());

    const sentReferences = vi.mocked(sendMessage).mock.calls[0]?.[6];
    expect(sentReferences).toEqual([
      expect.objectContaining({ id: "repo:stablyai/orca", kind: "repository" }),
    ]);
    expect(sentReferences?.[0]).not.toHaveProperty("previewUrl");
    expect(result.current.messages[0]).toMatchObject({
      $kind: "user",
      attachments: [expect.objectContaining({ id: "repo:stablyai/orca" })],
    });
    expect(useReferenceStore.getState().references).toEqual([]);
    expect(useComposerStore.getState().composer).toBe("");
  });
});
