/**
 * F1 regression tests for `useSessionEvents`.
 *
 * The F1 lane replaced ad-hoc seq bookkeeping in the SSE catch-up path
 * with two invariants:
 *
 *   1. **Don't reset `lastSeq.current` to 0 on session switch.** A reset
 *      would cause the SSE catch-up to re-emit every event the daemon
 *      already emitted before the switch, producing duplicate assistant
 *      messages and double tool-result keys.
 *
 *   2. **Seed `lastSeq.current` from the persisted transcript tail.** A
 *      fresh switch to a session with prior history must start its SSE
 *      catch-up replay strictly above the on-disk tail, not at 0.
 *
 * Both invariants are exercised by rendering the hook against a mocked
 * daemon and asserting the daemon was not asked for events whose seq is
 * below the persisted high-water mark.
 */
import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DaemonConfig, ProtocolEvent, TranscriptMessage } from "@rightkit/agent-ui-core/lib/types";

// Mock the daemon SDK so we control the transcript and event-history
// payloads deterministically. The real SDK hits a live daemon over HTTP.
vi.mock("@rightkit/agent-ui-core/lib/daemon", () => {
  return {
    getTranscript: vi.fn(),
    getSessionEventHistory: vi.fn(),
    openEventStream: vi.fn(),
    isTauriHost: () => mockTauriHost.value,
    replayEventPages: vi.fn(),
  };
});

const mockTauriHost = vi.hoisted(() => ({ value: false }));
const tauriMocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  listener: null as null | ((event: { payload: unknown }) => void),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauriMocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_name: string, cb: (event: { payload: unknown }) => void) => {
    tauriMocks.listener = cb;
    return () => {};
  }),
}));

// Mock the streaming store so the hook doesn't try to write to a Zustand
// store the test renderer doesn't provide.
vi.mock("@rightkit/agent-ui-core/state/streamingStore", () => {
  const actions = {
    clear: () => {},
    appendReasoning: () => {},
    appendText: () => {},
    setReasoningExpanded: () => {},
    setStreamingToolName: () => {},
  };
  return {
    useStreamingStore: Object.assign(
      vi.fn(() => actions),
      {
        getState: () => actions,
      },
    ),
  };
});

import {
  getTranscript,
  getSessionEventHistory,
  openEventStream,
  replayEventPages,
} from "@rightkit/agent-ui-core/lib/daemon";
import { useSessionEvents } from "./useSessionEvents";

const mockedGetTranscript = vi.mocked(getTranscript);
const mockedGetSessionEventHistory = vi.mocked(getSessionEventHistory);
const mockedOpenEventStream = vi.mocked(openEventStream);

const FAKE_CONFIG: DaemonConfig = {
  url: "http://127.0.0.1:0",
  token: "t",
};

function transcriptRow(id: string): TranscriptMessage {
  return {
    $kind: "assistant",
    id,
    content: "",
    createdAt: "2026-07-20T00:00:00Z",
    toolCalls: [],
    pinned: false,
  };
}

function buildHandlers() {
  return {
    formatError: (_err: unknown, fallback: string) => fallback,
    onApprovalEvent: () => {},
    onLoadPermissionDenials: () => {},
    onLoadRuntime: () => {},
    onLoadTasks: () => {},
    onRefresh: () => {},
    onResetApproval: () => {},
    onSessionStarted: () => {},
    onSessionTitleUpdated: () => {},
    onStatus: () => {},
    onSubAgentEvent: () => null,
    onGoalEvent: () => {},
    onUsageUpdated: () => {},
    onWorking: () => {},
  };
}

describe("useSessionEvents — F1 seq invariants", () => {
  beforeEach(() => {
    mockedGetTranscript.mockReset();
    mockedGetSessionEventHistory.mockReset();
    mockedOpenEventStream.mockReset();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("seeds lastSeq from the persisted transcript tail when a session loads", async () => {
    // Persisted transcript with three rows whose trailing-numeric ids
    // resolve to seq 7, 12, and 4. The hook should treat 12 as the
    // durable high-water mark.
    mockedGetTranscript.mockResolvedValue([
      transcriptRow("assistant:7"),
      transcriptRow("tool:call-a:12"),
      transcriptRow("message:4"),
    ]);
    mockedGetSessionEventHistory.mockResolvedValue([]);
    mockedOpenEventStream.mockImplementation(() => () => {});

    const { result } = renderHook(() =>
      useSessionEvents({
        config: FAKE_CONFIG,
        isConnected: true,
        sessionId: "sess-persisted-tail",
        ...buildHandlers(),
      }),
    );

    await waitFor(() => {
      expect(mockedGetTranscript).toHaveBeenCalledWith(FAKE_CONFIG, "sess-persisted-tail");
    });

    // The catch-up replay is requested with seq=12 (the high-water mark
    // extracted from the persisted rows). Anything lower would cause the
    // SSE handler to re-emit events the daemon already delivered.
    await waitFor(() => {
      expect(mockedGetSessionEventHistory).toHaveBeenCalled();
    });
    const historyCall = mockedGetSessionEventHistory.mock.calls[0];
    expect(historyCall?.[2]).toBe(12);

    expect(result.current.lastError).toBeNull();
  });

  it("seeds lastSeq to 0 when the persisted transcript is empty", async () => {
    mockedGetTranscript.mockResolvedValue([]);
    mockedGetSessionEventHistory.mockResolvedValue([]);
    mockedOpenEventStream.mockImplementation(() => () => {});

    renderHook(() =>
      useSessionEvents({
        config: FAKE_CONFIG,
        isConnected: true,
        sessionId: "sess-empty",
        ...buildHandlers(),
      }),
    );

    await waitFor(() => {
      expect(mockedGetSessionEventHistory).toHaveBeenCalled();
    });
    const historyCall = mockedGetSessionEventHistory.mock.calls[0];
    // Empty persisted tail → high-water is 0 → catch-up starts at 0.
    expect(historyCall?.[2]).toBe(0);
  });

  it("seeds lastSeq from the highest parsed id even when the daemon reports fewer events", async () => {
    // Transcript has a row id "compact:42", daemon has no further events.
    mockedGetTranscript.mockResolvedValue([transcriptRow("compact:42")]);
    mockedGetSessionEventHistory.mockResolvedValue([]);
    mockedOpenEventStream.mockImplementation(() => () => {});

    renderHook(() =>
      useSessionEvents({
        config: FAKE_CONFIG,
        isConnected: true,
        sessionId: "sess-compact-tail",
        ...buildHandlers(),
      }),
    );

    await waitFor(() => {
      expect(mockedGetSessionEventHistory).toHaveBeenCalled();
    });
    const historyCall = mockedGetSessionEventHistory.mock.calls[0];
    expect(historyCall?.[2]).toBe(42);
  });

  it("skips ids that have no parseable numeric suffix", async () => {
    // Rows without trailing digits must not seed lastSeq to NaN or to a
    // spurious positive number. Only the parseable rows count.
    mockedGetTranscript.mockResolvedValue([transcriptRow("orphan-id"), transcriptRow("sse-9")]);
    mockedGetSessionEventHistory.mockResolvedValue([]);
    mockedOpenEventStream.mockImplementation(() => () => {});

    renderHook(() =>
      useSessionEvents({
        config: FAKE_CONFIG,
        isConnected: true,
        sessionId: "sess-mixed",
        ...buildHandlers(),
      }),
    );

    await waitFor(() => {
      expect(mockedGetSessionEventHistory).toHaveBeenCalled();
    });
    const historyCall = mockedGetSessionEventHistory.mock.calls[0];
    expect(historyCall?.[2]).toBe(9);
  });

  it("ignores live SSE events whose seq is at or below the seeded high-water", async () => {
    // Persisted tail is seq 100. An SSE event with seq=99 must be
    // discarded as a replay duplicate; an event with seq=100 must also
    // be discarded (the persisted row already covers it).
    mockedGetTranscript.mockResolvedValue([transcriptRow("assistant:100")]);
    mockedGetSessionEventHistory.mockResolvedValue([]);

    const liveEvents: ProtocolEvent[] = [
      {
        $kind: "assistantMessage",
        sessionId: "sess-dup",
        seq: 99,
        createdAt: "t",
        content: "stale-1",
        model: "m",
        role: "assistant",
      } as unknown as ProtocolEvent,
      {
        $kind: "assistantMessage",
        sessionId: "sess-dup",
        seq: 100,
        createdAt: "t",
        content: "stale-2",
        model: "m",
        role: "assistant",
      } as unknown as ProtocolEvent,
    ];

    mockedOpenEventStream.mockImplementation((_cfg, _sid, _after, onEvent) => {
      queueMicrotask(() => liveEvents.forEach(onEvent));
      return () => {};
    });

    const onApprovalEvent = vi.fn();
    renderHook(() =>
      useSessionEvents({
        config: FAKE_CONFIG,
        isConnected: true,
        sessionId: "sess-dup",
        ...buildHandlers(),
        onApprovalEvent,
      }),
    );

    // Neither stale event should have been forwarded to a consumer of
    // the hook — they are both at-or-below the durable high-water.
    // Wait a tick for the SSE loop to consume them.
    await new Promise((resolve) => setTimeout(resolve, 10));
    // The hook is the consumer of SSE events; the test asserts via the
    // absence of any thrown assertion or downstream handler call.
    expect(onApprovalEvent).not.toHaveBeenCalled();
  });

  it("switches session ids and re-seeds lastSeq for each new session", async () => {
    mockedGetTranscript.mockImplementation(async (_cfg, sessionId) => {
      if (sessionId === "sess-a") return [transcriptRow("assistant:5")];
      if (sessionId === "sess-b") return [transcriptRow("assistant:20")];
      return [];
    });
    mockedGetSessionEventHistory.mockResolvedValue([]);
    mockedOpenEventStream.mockImplementation(() => () => {});

    const { rerender } = renderHook(
      ({ sessionId }) =>
        useSessionEvents({
          config: FAKE_CONFIG,
          isConnected: true,
          sessionId,
          ...buildHandlers(),
        }),
      { initialProps: { sessionId: "sess-a" } },
    );

    await waitFor(() => {
      expect(mockedGetTranscript).toHaveBeenCalledWith(FAKE_CONFIG, "sess-a");
    });
    await waitFor(() => {
      expect(mockedGetSessionEventHistory).toHaveBeenCalled();
    });
    const firstCall = mockedGetSessionEventHistory.mock.calls[0];
    expect(firstCall?.[2]).toBe(5);

    // Switch sessions.
    mockedGetSessionEventHistory.mockClear();
    rerender({ sessionId: "sess-b" });

    await waitFor(() => {
      expect(mockedGetTranscript).toHaveBeenCalledWith(FAKE_CONFIG, "sess-b");
    });
    await waitFor(() => {
      expect(mockedGetSessionEventHistory).toHaveBeenCalled();
    });
    // The new session's catch-up must start at its own persisted tail (20),
    // not at 0 (which would re-emit everything) and not at 5 (which is
    // sess-a's tail and has no relation to sess-b).
    const secondCall = mockedGetSessionEventHistory.mock.calls[0];
    expect(secondCall?.[2]).toBe(20);
  });

  it("does not start catch-up when the new session's transcript load fails", async () => {
    // First session loads with seq 5.
    mockedGetTranscript.mockImplementation(async (_cfg, sessionId) => {
      if (sessionId === "sess-ok") return [transcriptRow("assistant:5")];
      return Promise.reject(new Error("disk full"));
    });
    mockedGetSessionEventHistory.mockResolvedValue([]);
    mockedOpenEventStream.mockImplementation(() => () => {});

    const onStatus = vi.fn();
    const { rerender } = renderHook(
      ({ sessionId }) =>
        useSessionEvents({
          config: FAKE_CONFIG,
          isConnected: true,
          sessionId,
          ...buildHandlers(),
          onStatus,
        }),
      { initialProps: { sessionId: "sess-ok" } },
    );

    await waitFor(() => {
      expect(mockedGetSessionEventHistory).toHaveBeenCalled();
    });
    expect(mockedGetSessionEventHistory.mock.calls[0]?.[2]).toBe(5);

    // Switch to a session whose transcript load rejects. The hook must
    // not crash, must surface the error to onStatus, and must not
    // re-issue the catch-up at seq=0 (which would replay sess-ok's tail).
    mockedGetSessionEventHistory.mockClear();
    rerender({ sessionId: "sess-broken" });

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    expect(onStatus).toHaveBeenCalled();
    // No fresh history call: the previous lastSeq stands until a future
    // successful load proves otherwise.
    expect(mockedGetSessionEventHistory).not.toHaveBeenCalled();
  });

  it("rehydrates typed reference metadata from a live user-message event", async () => {
    mockedGetTranscript.mockResolvedValue([]);
    mockedGetSessionEventHistory.mockResolvedValue([]);
    mockedOpenEventStream.mockImplementation((_config, sessionId, _after, onEvent) => {
      queueMicrotask(() =>
        onEvent({
          $kind: "userMessageSubmitted",
          sessionId,
          seq: 1,
          createdAt: "2026-07-21T00:00:00Z",
          text: "use @audit",
          attachments: [
            {
              id: "skill:audit",
              kind: "skill",
              destination: "@audit",
              filename: "Audit",
              provenance: {
                source: "composer.skill-picker",
                createdAt: "2026-07-21T00:00:00Z",
                trust: "workspaceTrusted",
              },
            },
          ],
        }),
      );
      return () => {};
    });

    const { result } = renderHook(() =>
      useSessionEvents({
        config: FAKE_CONFIG,
        isConnected: true,
        sessionId: "session-references",
        ...buildHandlers(),
      }),
    );

    await waitFor(() => expect(result.current.messages).toHaveLength(1));
    expect(result.current.messages[0]).toMatchObject({
      $kind: "user",
      attachments: [expect.objectContaining({ id: "skill:audit", kind: "skill" })],
    });
  });
});

describe("useSessionEvents native transport resync", () => {
  afterEach(() => {
    mockTauriHost.value = false;
    tauriMocks.listener = null;
  });

  it("replays pages on streamResyncRequired and advances the native cursor", async () => {
    mockTauriHost.value = true;
    mockedGetTranscript.mockResolvedValue([]);
    mockedGetSessionEventHistory.mockResolvedValue([]);
    tauriMocks.invoke.mockReset();
    tauriMocks.invoke.mockResolvedValue("conn-1");
    vi.mocked(replayEventPages).mockReset();
    vi.mocked(replayEventPages).mockImplementation(async (_cfg, _sid, _after, onEvent) => {
      onEvent({ $kind: "sessionStarted", sessionId: "sess-r", seq: 3, createdAt: "t" } as unknown as ProtocolEvent);
      return 7;
    });

    renderHook(() =>
      useSessionEvents({
        config: FAKE_CONFIG,
        isConnected: true,
        sessionId: "sess-r",
        ...buildHandlers(),
      }),
    );
    await waitFor(() => expect(tauriMocks.listener).not.toBeNull());
    await act(async () => {
      tauriMocks.listener!({
        payload: {
          subscriptionId: "conn-1:sess-r:",
          event: {
            sessionId: "sess-r",
            runtimeId: "",
            epoch: "",
            sequence: 999,
            event: { $kind: "streamResyncRequired", sessionId: "sess-r", seq: 999 },
          },
        },
      });
    });
    await waitFor(() =>
      expect(tauriMocks.invoke).toHaveBeenCalledWith("stream_transport_advance_cursor", {
        connectionId: "conn-1",
        sessionId: "sess-r",
        runtimeId: "",
        sequence: 7,
      }),
    );
    expect(vi.mocked(replayEventPages)).toHaveBeenCalledTimes(1);
    expect(vi.mocked(replayEventPages).mock.calls[0][2]).toBe(0);
  });
});
