import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  getSessionEventHistory,
  getTranscript,
  isTauriHost,
  openEventStream,
  replayEventPages,
} from "@rightkit/agent-ui-core/lib/daemon";
import { useStreamingStore } from "@rightkit/agent-ui-core/state/streamingStore";
import type {
  DaemonConfig,
  DispatchDecision,
  ProtocolEvent,
  TranscriptMessage,
  UserQuestion,
} from "@rightkit/agent-ui-core/lib/types";
import {
  isInternalRunFailure,
  isInternalTranscriptMessage,
  visibleTranscriptMessages,
} from "@rightkit/agent-ui-core/lib/transcriptMessages";
import {
  insertOrchestrationNotice,
  insertOrchestrationNoticeFromRoster,
  mergeOrchestrationNotices,
} from "@rightkit/agent-ui-core/lib/orchestrationNotices";
import { createRuntimeGuards } from "@rightkit/agent-ui-core/lib/concurrency/runtimeGuards";
import {
  appendProtocolEvent,
  appendTranscriptMessage,
  computeHighWaterSeqFromTranscript,
  createStreamReconciler,
  shouldFlushBeforeEvent,
  type StreamScope,
} from "@rightkit/agent-ui-core/lib/daemon/streamReconcile";
import { createStreamDeltaBatcher, type StreamDeltaBatcher } from "@rightkit/agent-ui-core/lib/streamDeltaBatcher";
import { parseRateLimitEvent } from "@rightkit/agent-ui-core/lib/accounts/accounts";
import { useRateLimitStore } from "@rightkit/agent-ui-core/state/rateLimitStore";

interface TransportDelivery {
  subscriptionId: string;
  event: {
    sessionId: string;
    runtimeId: string;
    epoch: string;
    sequence: number;
    event: ProtocolEvent;
  };
}

interface UsageUpdate {
  costUsd: number;
  promptTokens?: number;
  completionTokens?: number;
  contextTokens?: number;
  contextPercent?: number;
  cacheReadTokens?: number;
  cacheCreationTokens?: number;
}

interface UseSessionEventsOptions {
  config: DaemonConfig | null;
  isConnected: boolean;
  sessionId: string | null;
  formatError: (error: unknown, fallback: string) => string;
  onApprovalEvent: (event: ProtocolEvent) => void;
  onLoadPermissionDenials: () => void | Promise<void>;
  onLoadRuntime: () => void | Promise<void>;
  onLoadTasks: () => void | Promise<void>;
  onRefresh: () => void | Promise<void>;
  onResetApproval: () => void;
  onSessionStarted: () => void;
  onSessionTitleUpdated: (sessionId: string, title: string) => void;
  onStatus: (status: string) => void;
  onSubAgentEvent: (event: ProtocolEvent) => string | null;
  onGoalEvent: (event: ProtocolEvent) => void;
  onUsageUpdated: (usage: UsageUpdate) => void;
  onWorking: (working: boolean, sessionId?: string) => void;
  onRunFailed?: (sessionId: string) => void;
}

export function useSessionEvents({
  config,
  isConnected,
  sessionId,
  formatError,
  onApprovalEvent,
  onLoadPermissionDenials,
  onLoadRuntime,
  onLoadTasks,
  onRefresh,
  onResetApproval,
  onSessionStarted,
  onSessionTitleUpdated,
  onStatus,
  onSubAgentEvent,
  onGoalEvent,
  onUsageUpdated,
  onWorking,
  onRunFailed,
}: UseSessionEventsOptions) {
  const [messages, setMessages] = useState<TranscriptMessage[]>([]);
  const [events, setEvents] = useState<ProtocolEvent[]>([]);
  // Streaming state is mirrored to useStreamingStore so the transcript subtree
  // can subscribe per-field. Internal state stays as the canonical writer so
  // the hook's return shape is unchanged for its current App.tsx consumers.
  const [streamingText, setStreamingText] = useState("");
  const [streamingReasoning, setStreamingReasoning] = useState("");
  const [reasoningExpanded, setReasoningExpanded] = useState(false);
  const [pendingQuestion, setPendingQuestion] = useState<{
    questionId: string;
    questions: UserQuestion[];
  } | null>(null);
  const [pendingPlan, setPendingPlan] = useState<{
    planId: string;
    plan: string;
  } | null>(null);
  const [streamingToolName, setStreamingToolName] = useState<string | null>(null);
  const [dispatchDecision, setDispatchDecision] = useState<DispatchDecision | null>(null);
  // Visible failure propagation: surfaced to the user so a missed
  // append_event_at_seq or a stalled catch-up is not silent. The SSE
  // handler reports it, the catch-up loop reports it, and
  // TranscriptStream renders it as a status banner.
  const [lastError, setLastError] = useState<string | null>(null);
  const lastSeq = useRef(0);
  const pendingUserMessageNonces = useRef<Set<string>>(new Set());
  const activeBatchSessionId = useRef<string | null>(null);
  const runtimeGuards = useRef(createRuntimeGuards());
  const streamReconciler = useRef(createStreamReconciler());
  const streamBatcher = useRef<StreamDeltaBatcher | null>(null);
  if (!streamBatcher.current) {
    streamBatcher.current = createStreamDeltaBatcher({
      onFlush: (sessionId, deltas) => {
        if (activeBatchSessionId.current !== sessionId) return;
        for (const delta of deltas) {
          if (delta.type === "reasoning") {
            setStreamingReasoning((text) => text + delta.text);
            useStreamingStore.getState().appendReasoning(delta.text);
          } else {
            setStreamingText((text) => text + delta.text);
            useStreamingStore.getState().appendText(delta.text);
          }
        }
      },
    });
  }
  const handlersRef = useRef({
    formatError,
    onApprovalEvent,
    onLoadPermissionDenials,
    onLoadRuntime,
    onLoadTasks,
    onRefresh,
    onResetApproval,
    onSessionStarted,
    onSessionTitleUpdated,
    onStatus,
    onSubAgentEvent,
    onGoalEvent,
    onUsageUpdated,
    onWorking,
    onRunFailed,
  });

  useEffect(() => {
    handlersRef.current = {
      formatError,
      onApprovalEvent,
      onLoadPermissionDenials,
      onLoadRuntime,
      onLoadTasks,
      onRefresh,
      onResetApproval,
      onSessionStarted,
      onSessionTitleUpdated,
      onStatus,
      onSubAgentEvent,
      onGoalEvent,
      onUsageUpdated,
      onWorking,
      onRunFailed,
    };
  }, [
    formatError,
    onApprovalEvent,
    onLoadPermissionDenials,
    onLoadRuntime,
    onLoadTasks,
    onRefresh,
    onResetApproval,
    onSessionStarted,
    onSessionTitleUpdated,
    onStatus,
    onSubAgentEvent,
    onGoalEvent,
    onUsageUpdated,
    onWorking,
    onRunFailed,
  ]);

  useEffect(() => {
    const batcher = streamBatcher.current;
    if (!batcher) return;
    const detachLifecycle = batcher.attachLifecycleListeners();
    return () => {
      detachLifecycle();
      batcher.dispose();
    };
  }, []);

  const clearStreaming = useCallback(() => {
    setStreamingText("");
    setStreamingReasoning("");
    setReasoningExpanded(false);
    setStreamingToolName(null);
    useStreamingStore.getState().clear();
  }, []);

  const addPendingUserNonce = useCallback((nonce: string) => {
    pendingUserMessageNonces.current.add(nonce);
  }, []);

  const handleRuntimeEvent = useCallback(
    (event: ProtocolEvent) => {
      const handlers = handlersRef.current;
      switch (event.$kind) {
        case "sessionStarted":
          handlers.onStatus("Ready");
          handlers.onSessionStarted();
          break;
        case "sessionTitleUpdated":
          handlers.onSessionTitleUpdated(event.sessionId, event.title);
          break;
        case "assistantDelta":
          streamBatcher.current?.queueDelta(
            event.sessionId,
            event.isReasoning ? "reasoning" : "assistant",
            event.text,
            Date.parse(event.createdAt) || Date.now(),
          );
          handlers.onWorking(true, event.sessionId);
          break;
        case "assistantMessageCompleted":
          setReasoningExpanded(false);
          useStreamingStore.getState().setReasoningExpanded(false);
          setMessages((rows) => appendTranscriptMessage(rows, event.message));
          clearStreaming();
          handlers.onWorking(false, event.sessionId);
          handlers.onResetApproval();
          handlers.onStatus("Ready");
          void handlers.onRefresh();
          void handlers.onLoadTasks();
          break;
        case "toolResultAdded":
          setStreamingToolName(null);
          useStreamingStore.getState().setStreamingToolName(null);
          setMessages((rows) => appendTranscriptMessage(rows, event.result));
          if (
            event.result.isError &&
            event.result.summary.toLowerCase().includes("permission denied")
          ) {
            void handlers.onLoadPermissionDenials();
          }
          void handlers.onLoadRuntime();
          break;
        case "toolCallStarted":
          handlers.onStatus(`Running ${event.call.name}`);
          handlers.onWorking(true, event.sessionId);
          setStreamingToolName(event.call.name);
          useStreamingStore.getState().setStreamingToolName(event.call.name);
          break;
        case "toolApprovalRequested":
          handlers.onApprovalEvent(event);
          break;
        case "runInterrupted":
          clearStreaming();
          handlers.onWorking(false, event.sessionId);
          handlers.onStatus("Interrupted");
          break;
        case "runFailed":
          if (isInternalRunFailure(event)) break;
          clearStreaming();
          handlers.onWorking(false, event.sessionId);
          handlers.onRunFailed?.(event.sessionId);
          handlers.onStatus(event.hint ? `${event.error}: ${event.hint}` : event.error);
          break;
        case "usageUpdated":
          handlers.onUsageUpdated({
            costUsd: event.costUsd,
            promptTokens: event.promptTokens,
            completionTokens: event.completionTokens,
            contextTokens: event.contextTokens,
            contextPercent: event.contextPercent,
            cacheReadTokens: event.cacheReadTokens,
            cacheCreationTokens: event.cacheCreationTokens,
          });
          handlers.onStatus(`Est. $${event.costUsd.toFixed(4)}`);
          break;
        case "budgetExceeded":
          setStreamingText("");
          handlers.onWorking(false, event.sessionId);
          handlers.onStatus(
            `Budget exceeded: $${event.currentUsd.toFixed(4)} / $${event.ceilingUsd.toFixed(4)}`,
          );
          break;
        case "sessionCompacted":
          setMessages((rows) => [
            ...rows,
            {
              $kind: "compact",
              id: `compact-${Date.now()}`,
              createdAt: event.createdAt,
              summary: event.summary,
              droppedCount: event.droppedCount,
              pinned: false,
            },
          ]);
          break;
        case "subAgentSpawned":
          setMessages((rows) => insertOrchestrationNotice(rows, event));
          {
            const ns = handlers.onSubAgentEvent(event);
            if (ns) handlers.onStatus(ns);
          }
          break;
        case "subAgentCompleted": {
          {
            const ns = handlers.onSubAgentEvent(event);
            if (ns) handlers.onStatus(ns);
          }
          break;
        }
        case "subAgentCancelled":
          {
            const ns = handlers.onSubAgentEvent(event);
            if (ns) handlers.onStatus(ns);
          }
          break;
        case "agentRosterUpdated":
          setMessages((rows) =>
            event.agents.reduce(
              (current, agent) =>
                insertOrchestrationNoticeFromRoster(current, agent, event.createdAt),
              rows,
            ),
          );
          handlers.onSubAgentEvent(event);
          break;
        case "goalProgressUpdated":
          handlers.onGoalEvent(event);
          break;
        case "taskGraphUpdated":
        case "taskStatusUpdated":
          // Task rows are a daemon-owned projection. Refresh after both a
          // complete graph snapshot and a single lifecycle transition so the
          // Tasks pane cannot remain stale between turn completions.
          void handlers.onLoadTasks();
          break;
        case "accountRateLimited": {
          const limit = parseRateLimitEvent(event);
          if (limit) useRateLimitStore.getState().record(event.sessionId, limit);
          break;
        }
        case "accountUsageReported":
          // Per-account usage is informational; the status line already tracks session cost.
          break;
        case "dispatchDecided":
          setDispatchDecision({
            strategy: event.strategy,
            rationale: event.rationale,
          });
          break;
        case "userMessageSubmitted":
          if (event.nonce && pendingUserMessageNonces.current.has(event.nonce)) {
            pendingUserMessageNonces.current.delete(event.nonce);
          } else {
            setMessages((rows) => [
              ...rows,
              {
                $kind: "user",
                id: `sse-${event.seq}`,
                createdAt: event.createdAt,
                content: event.text,
                pinned: false,
                attachments: event.attachments?.length ? event.attachments : undefined,
              },
            ]);
          }
          break;
        case "userQuestionAsked":
          setPendingQuestion({
            questionId: event.questionId,
            questions: event.questions,
          });
          handlers.onStatus("Question for you");
          handlers.onWorking(false, event.sessionId);
          break;
        case "userQuestionAnswered":
          setPendingQuestion(null);
          break;
        case "planPresented":
          setPendingPlan({ planId: event.planId, plan: event.plan });
          handlers.onStatus("Plan ready for review");
          handlers.onWorking(false);
          break;
      }
    },
    [clearStreaming],
  );

  useEffect(() => {
    if (!config || !isConnected || !sessionId) {
      runtimeGuards.current.closeSession();
      streamReconciler.current.closeSession();
      activeBatchSessionId.current = null;
      setMessages([]);
      setEvents([]);
      setDispatchDecision(null);
      clearStreaming();
      handlersRef.current.onWorking(false);
      return;
    }

    let cancelled = false;
    const activeConfig = config;
    const activeSessionId = sessionId;
    const runtimeScope = runtimeGuards.current.openSession(activeSessionId);
    activeBatchSessionId.current = activeSessionId;
    clearStreaming();
    handlersRef.current.onWorking(false, activeSessionId);
    handlersRef.current.onResetApproval();
    setPendingQuestion(null);
    setPendingPlan(null);
    setDispatchDecision(null);
    setEvents([]);
    setMessages([]);
    setLastError(null);

    let streamScope: StreamScope | null = null;

    function projectAcceptedEvent(event: ProtocolEvent) {
      if (cancelled || !runtimeGuards.current.acceptsFrame(runtimeScope, event)) return;
      if (shouldFlushBeforeEvent(event)) streamBatcher.current?.flush(event.sessionId);
      setEvents((prev) => appendProtocolEvent(prev, event));
      handleRuntimeEvent(event);
    }

    function applyEvent(event: ProtocolEvent) {
      if (!streamScope || !runtimeGuards.current.acceptsFrame(runtimeScope, event)) return;
      if (!streamReconciler.current.accept(event, streamScope)) return;
      lastSeq.current = streamReconciler.current.snapshot().lastSeq;
      projectAcceptedEvent(event);
    }

    async function catchUpEvents(apply: boolean): Promise<boolean> {
      const requestToken = runtimeGuards.current.beginRequest(activeSessionId);
      const history = await getSessionEventHistory(activeConfig, activeSessionId, lastSeq.current);
      if (cancelled || !runtimeGuards.current.isCurrent(requestToken) || !streamScope) return false;
      const accepted = streamReconciler.current.acceptHistory(history, undefined, streamScope);
      lastSeq.current = streamReconciler.current.snapshot().lastSeq;
      for (const event of accepted) {
        if (
          apply ||
          event.$kind === "subAgentSpawned" ||
          event.$kind === "subAgentCompleted" ||
          event.$kind === "subAgentCancelled" ||
          event.$kind === "agentRosterUpdated" ||
          event.$kind === "taskGraphUpdated" ||
          event.$kind === "taskStatusUpdated"
        ) {
          // Agent startup often races ahead of the first SSE subscription.
          // Replay the selected session's complete worker lifecycle even though
          // ordinary transcript-bearing events are loaded through getTranscript().
          projectAcceptedEvent(event);
        }
      }
      return true;
    }

    let stopStream: (() => void) | null = null;
    let transportConnectionId: string | null = null;
    let transportUnlisten: UnlistenFn | null = null;
    let pollTimer: number | null = null;
    const resyncAbort = new AbortController();
    let resyncing = false;

    // The daemon ring overflowed for the native transport: page
    // /events/replay from the cursor, apply in order, then move the Rust
    // cursor so its reconnect resumes live from `next`.
    async function resyncNative(connectionId: string) {
      if (resyncing || cancelled) return;
      resyncing = true;
      try {
        const next = await replayEventPages(
          activeConfig,
          activeSessionId,
          lastSeq.current,
          applyEvent,
          resyncAbort.signal,
        );
        if (cancelled) return;
        lastSeq.current = Math.max(lastSeq.current, next);
        await tauriInvoke("stream_transport_advance_cursor", {
          connectionId,
          sessionId: activeSessionId,
          runtimeId: "",
          sequence: lastSeq.current,
        });
      } catch (err) {
        if (!cancelled) streamError(err, "Event replay failed");
      } finally {
        resyncing = false;
      }
    }

    const streamError = (error: unknown, fallback: string) => {
      if (cancelled || !runtimeGuards.current.acceptsFrame(runtimeScope, { sessionId: activeSessionId })) return;
      const handlers = handlersRef.current;
      const message = handlers.formatError(error, fallback);
      setLastError(message);
      handlers.onStatus(message);
    };

    async function openNativeStream(): Promise<() => void> {
      const connectionId = await tauriInvoke<string>("stream_transport_connect", {
        address: activeConfig.url,
        token: activeConfig.token,
        resumeFrom: [
          { sessionId: activeSessionId, runtimeId: "", epoch: "", sequence: lastSeq.current },
        ],
      });
      transportConnectionId = connectionId;
      const subscriptionId = `${connectionId}:${activeSessionId}:`;
      transportUnlisten = await listen<TransportDelivery>("coderight-stream-event", (event) => {
        const delivery = event.payload;
        if (
          cancelled ||
          delivery.subscriptionId !== subscriptionId ||
          delivery.event.sessionId !== activeSessionId
        ) {
          return;
        }
        if ((delivery.event.event.$kind as string) === "streamResyncRequired") {
          // Synthetic seq: never feed it to the reconciler or advance the cursor.
          void resyncNative(connectionId);
          return;
        }
        const protocolEvent: ProtocolEvent = {
          ...delivery.event.event,
          sessionId: delivery.event.sessionId,
          seq: Number(delivery.event.sequence),
        };
        applyEvent(protocolEvent);
      });
      if (cancelled) {
        transportUnlisten();
        transportUnlisten = null;
        await tauriInvoke("stream_transport_disconnect", { connectionId }).catch(() => undefined);
        transportConnectionId = null;
        return () => {};
      }
      await tauriInvoke("stream_transport_subscribe", {
        connectionId,
        sessionId: activeSessionId,
        runtimeId: "",
      });
      return () => {
        transportUnlisten?.();
        transportUnlisten = null;
        void tauriInvoke("stream_transport_unsubscribe", {
          connectionId,
          sessionId: activeSessionId,
          runtimeId: "",
        }).catch(() => undefined);
        void tauriInvoke("stream_transport_disconnect", { connectionId }).catch(() => undefined);
        if (transportConnectionId === connectionId) transportConnectionId = null;
      };
    }

    async function bootstrap() {
      const transcriptRequest = runtimeGuards.current.beginRequest(activeSessionId);
      let rows: TranscriptMessage[];
      try {
        rows = await getTranscript(activeConfig, activeSessionId);
      } catch (err) {
        if (runtimeGuards.current.isCurrent(transcriptRequest)) streamError(err, "Transcript failed");
        return;
      }
      if (cancelled || !runtimeGuards.current.isCurrent(transcriptRequest)) return;

      const visibleRows = visibleTranscriptMessages(rows.filter((row) => row.$kind !== "system"));
      setMessages((current) => mergeOrchestrationNotices(visibleRows, current));
      streamScope = streamReconciler.current.seedFromTranscript(activeSessionId, visibleRows);
      lastSeq.current = computeHighWaterSeqFromTranscript(visibleRows);
      if (visibleRows.at(-1)?.$kind === "assistant") {
        handlersRef.current.onWorking(false, activeSessionId);
        handlersRef.current.onStatus("Ready");
      }

      try {
        if (!(await catchUpEvents(false))) return;
      } catch (err) {
        streamError(err, "Event history failed");
        return;
      }
      if (cancelled || !streamScope) return;

      try {
        if (isTauriHost()) {
          stopStream = await openNativeStream();
        } else {
          stopStream = openEventStream(
            activeConfig,
            activeSessionId,
            lastSeq.current,
            applyEvent,
            (reconnectError) => streamError(reconnectError, "Event stream reconnecting..."),
          );
        }
        let catchUpFailing = false;
        pollTimer = window.setInterval(() => {
          void catchUpEvents(true)
            .then((ok) => {
              if (ok && !cancelled && catchUpFailing) {
                catchUpFailing = false;
                handlersRef.current.onStatus("Ready");
              }
            })
            .catch((err) => {
              if (!cancelled) {
                catchUpFailing = true;
                streamError(err, "Event history reconnecting...");
              }
            });
        }, 1000);
      } catch (err) {
        streamError(err, "Event stream failed");
      }
    }

    void bootstrap();

    return () => {
      cancelled = true;
      resyncAbort.abort();
      stopStream?.();
      if (transportConnectionId !== null) {
        const connectionId = transportConnectionId;
        transportUnlisten?.();
        transportUnlisten = null;
        void tauriInvoke("stream_transport_unsubscribe", {
          connectionId,
          sessionId: activeSessionId,
          runtimeId: "",
        }).catch(() => undefined);
        void tauriInvoke("stream_transport_disconnect", { connectionId }).catch(() => undefined);
        transportConnectionId = null;
      }
      if (pollTimer !== null) window.clearInterval(pollTimer);
      streamBatcher.current?.flush(activeSessionId);
      activeBatchSessionId.current = null;
      streamReconciler.current.closeSession();
      runtimeGuards.current.closeSession();
    };
  }, [clearStreaming, config, handleRuntimeEvent, isConnected, sessionId]);

  return {
    messages,
    setMessages,
    events,
    streamingText,
    setStreamingText,
    streamingReasoning,
    setStreamingReasoning,
    reasoningExpanded,
    setReasoningExpanded,
    pendingQuestion,
    setPendingQuestion,
    pendingPlan,
    setPendingPlan,
    dispatchDecision,
    streamingToolName,
    addPendingUserNonce,
    lastError,
    setLastError,
  };
}

