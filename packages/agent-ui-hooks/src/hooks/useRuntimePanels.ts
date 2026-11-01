import { useCallback, useRef, useState } from "react";
import {
  addMemory,
  getArtifacts,
  getTaskGraph,
  getTasks,
  listMemory,
  listMessages,
} from "@rightkit/agent-ui-core/lib/daemon";
import type {
  ArtifactRow,
  DaemonConfig,
  MemoryListResponse,
  MessageHistoryRow,
  TaskNode,
  TaskRow,
} from "@rightkit/agent-ui-core/lib/types";

interface UseRuntimePanelsOptions {
  config: DaemonConfig | null;
  sessionId: string | null;
  sessionWorkspaceRoot: string | null;
  workspaceRoot: string;
  formatError: (error: unknown, fallback: string) => string;
  onStatus: (status: string) => void;
}

function parseMemoryKeywords(value: string) {
  return value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

export function useRuntimePanels({
  config,
  sessionId,
  sessionWorkspaceRoot,
  workspaceRoot,
  formatError,
  onStatus,
}: UseRuntimePanelsOptions) {
  const [tasks, setTasks] = useState<TaskRow[]>([]);
  const [artifacts, setArtifacts] = useState<ArtifactRow[]>([]);
  const [taskGraph, setTaskGraph] = useState<TaskNode[]>([]);
  const [runtimeMessages, setRuntimeMessages] = useState<MessageHistoryRow[]>([]);
  const [messageStorePath, setMessageStorePath] = useState("");
  const [memoryEntries, setMemoryEntries] = useState<MemoryListResponse["entries"]>([]);
  const [memoryQuery, setMemoryQuery] = useState("");
  const [memoryDraft, setMemoryDraft] = useState("");
  const [memoryKeywordDraft, setMemoryKeywordDraft] = useState("");
  const [memoryActionPending, setMemoryActionPending] = useState(false);
  const runtimeLoadSeq = useRef(0);

  const loadTasks = useCallback(async () => {
    if (!config) return;
    try {
      setTasks(
        await getTasks(config, sessionId ?? undefined, sessionWorkspaceRoot || workspaceRoot),
      );
    } catch (err) {
      onStatus(formatError(err, "Tasks failed"));
    }
  }, [config, formatError, onStatus, sessionId, sessionWorkspaceRoot, workspaceRoot]);

  const loadRuntimePanels = useCallback(async () => {
    const requestId = ++runtimeLoadSeq.current;
    if (!config) {
      setArtifacts([]);
      setTaskGraph([]);
      return null;
    }
    const sessionRequest = sessionId
      ? Promise.all([getArtifacts(config, sessionId), getTaskGraph(config, sessionId)])
      : Promise.resolve(null);
    const [sessionRuntime, messages, memory] = await Promise.all([
      sessionRequest,
      listMessages(config),
      listMemory(config, memoryQuery, 30),
    ]);
    if (requestId !== runtimeLoadSeq.current) {
      return null;
    }
    if (sessionRuntime) {
      const [nextArtifacts, nextTaskGraph] = sessionRuntime;
      setArtifacts(nextArtifacts);
      setTaskGraph(nextTaskGraph);
    } else {
      setArtifacts([]);
      setTaskGraph([]);
    }
    setRuntimeMessages(messages.messages);
    setMessageStorePath(messages.messageStorePath);
    setMemoryEntries(memory.entries);
    return { messages, memory };
  }, [config, memoryQuery, sessionId]);

  const searchMemory = useCallback(
    async (query: string) => {
      setMemoryQuery(query);
      if (!config) return;
      try {
        const next = await listMemory(config, query, 30);
        setMemoryEntries(next.entries);
      } catch (err) {
        onStatus(formatError(err, "Memory search failed"));
      }
    },
    [config, formatError, onStatus],
  );

  const rememberMemory = useCallback(async () => {
    if (!config || !memoryDraft.trim()) return;
    setMemoryActionPending(true);
    try {
      await addMemory(config, memoryDraft.trim(), parseMemoryKeywords(memoryKeywordDraft));
      setMemoryDraft("");
      setMemoryKeywordDraft("");
      const next = await listMemory(config, memoryQuery, 30);
      setMemoryEntries(next.entries);
      onStatus("Memory added");
    } catch (err) {
      onStatus(formatError(err, "Add memory failed"));
    } finally {
      setMemoryActionPending(false);
    }
  }, [config, formatError, memoryDraft, memoryKeywordDraft, memoryQuery, onStatus]);

  return {
    tasks,
    artifacts,
    taskGraph,
    runtimeMessages,
    messageStorePath,
    memoryEntries,
    memoryQuery,
    setMemoryQuery,
    memoryDraft,
    setMemoryDraft,
    memoryKeywordDraft,
    setMemoryKeywordDraft,
    memoryActionPending,
    loadTasks,
    loadRuntimePanels,
    searchMemory,
    rememberMemory,
  };
}
