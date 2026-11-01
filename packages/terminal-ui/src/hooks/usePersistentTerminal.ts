import { useCallback, useEffect, useRef, useState } from "react";
import {
  createExecutionProcess,
  getExecutionProcessLogs,
  killExecutionProcess,
  listExecutionProcesses,
  writeExecutionProcessStdin,
} from "@rightkit/agent-ui-core/lib/daemon";
import { createRuntimeGuards, type RuntimeGuards } from "@rightkit/agent-ui-core/lib/concurrency/runtimeGuards";
import type {
  DaemonConfig,
  ExecutionProcessLogs,
  ExecutionProcessRow,
} from "@rightkit/agent-ui-core/lib/types";

export interface PersistentTerminalOptions {
  config: DaemonConfig | null;
  sessionId: string | null;
  workspaceRoot: string;
  pollIntervalMs?: number;
}

export interface PersistentTerminalState {
  processes: ExecutionProcessRow[];
  selectedProcessId: string | null;
  logs: ExecutionProcessLogs | null;
  input: string;
  loading: boolean;
  error: string | null;
  actionPending: string | null;
  history: string[];
  setInput: (value: string) => void;
  selectProcess: (id: string) => void;
  refresh: () => Promise<void>;
  createTerminal: () => Promise<void>;
  sendInput: (value?: string) => Promise<void>;
  stopProcess: (id: string) => Promise<void>;
}

/**
 * Persistent renderer connector for the daemon-owned terminal process.
 *
 * The process and scrollback remain daemon state. This hook only keeps the
 * selected process and its projection alive while the terminal surface is
 * hidden, and rejects late responses after a session switch or newer request.
 */
export function usePersistentTerminal({
  config,
  sessionId,
  workspaceRoot,
  pollIntervalMs = 1500,
}: PersistentTerminalOptions): PersistentTerminalState {
  const guardsRef = useRef<RuntimeGuards | null>(null);
  if (!guardsRef.current) guardsRef.current = createRuntimeGuards();
  const guards = guardsRef.current!;

  const [processes, setProcesses] = useState<ExecutionProcessRow[]>([]);
  const [selectedProcessId, setSelectedProcessId] = useState<string | null>(null);
  const [logs, setLogs] = useState<ExecutionProcessLogs | null>(null);
  const [input, setInput] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [actionPending, setActionPending] = useState<string | null>(null);
  const [history, setHistory] = useState<string[]>([]);

  const activeWorkspace = workspaceRoot.trim();

  const refresh = useCallback(async () => {
    if (!config || !sessionId || !activeWorkspace) {
      setProcesses([]);
      setSelectedProcessId(null);
      setLogs(null);
      return;
    }

    const token = guards.beginRequest(sessionId);
    setLoading(true);
    setError(null);
    try {
      const response = await listExecutionProcesses(config, activeWorkspace);
      if (!guards.isCurrent(token)) return;
      setProcesses(response.processes);
      setSelectedProcessId((current) =>
        current && response.processes.some((process) => process.id === current)
          ? current
          : response.processes[0]?.id ?? null,
      );
    } catch (reason) {
      if (guards.isCurrent(token)) {
        setError(reason instanceof Error ? reason.message : "Terminal processes failed");
      }
    } finally {
      if (guards.isCurrent(token)) setLoading(false);
    }
  }, [activeWorkspace, config, guards, sessionId]);

  const loadLogs = useCallback(
    async (processId: string) => {
      if (!config || !sessionId || !processId) return;
      const token = guards.beginRequest(sessionId);
      try {
        const response = await getExecutionProcessLogs(config, processId, 128 * 1024);
        if (!guards.isCurrent(token)) return;
        setLogs(response.logs);
        setError(null);
      } catch (reason) {
        if (guards.isCurrent(token)) {
          setError(reason instanceof Error ? reason.message : "Terminal logs failed");
        }
      }
    },
    [config, guards, sessionId],
  );

  useEffect(() => {
    if (!config || !sessionId || !activeWorkspace) {
      guards.closeSession();
      setProcesses([]);
      setSelectedProcessId(null);
      setLogs(null);
      setError(null);
      setLoading(false);
      setActionPending(null);
      return;
    }

    // Open the renderer session before any process or log request starts.
    guards.openSession(sessionId);
    void refresh();
    const interval = window.setInterval(() => void refresh(), pollIntervalMs);
    return () => {
      window.clearInterval(interval);
      guards.closeSession();
    };
  }, [activeWorkspace, config, guards, pollIntervalMs, refresh, sessionId]);

  useEffect(() => {
    setLogs(null);
    if (selectedProcessId) void loadLogs(selectedProcessId);
  }, [loadLogs, selectedProcessId]);

  const createTerminal = useCallback(async () => {
    if (!config || !sessionId) return;
    const token = guards.beginRequest(sessionId);
    setActionPending("create");
    setError(null);
    try {
      const response = await createExecutionProcess(config, { sessionId });
      if (!guards.isCurrent(token)) return;
      setSelectedProcessId(response.process.id);
      setInput("");
      await refresh();
      await loadLogs(response.process.id);
    } catch (reason) {
      if (guards.isCurrent(token)) {
        setError(reason instanceof Error ? reason.message : "Terminal launch failed");
      }
    } finally {
      setActionPending(null);
    }
  }, [config, guards, loadLogs, refresh, sessionId]);

  const sendInput = useCallback(
    async (value = input) => {
      if (!config || !sessionId || !selectedProcessId || !value.length) return;
      const token = guards.beginRequest(sessionId);
      setActionPending(`stdin:${selectedProcessId}`);
      setError(null);
      try {
        await writeExecutionProcessStdin(config, selectedProcessId, {
          input: value,
          appendNewline: true,
        });
        if (!guards.isCurrent(token)) return;
        setHistory((current) => [value.trim(), ...current.filter((item) => item !== value.trim())].filter(Boolean).slice(0, 500));
        setInput("");
        await loadLogs(selectedProcessId);
      } catch (reason) {
        if (guards.isCurrent(token)) {
          setError(reason instanceof Error ? reason.message : "Terminal stdin failed");
        }
      } finally {
        setActionPending(null);
      }
    },
    [config, guards, input, loadLogs, selectedProcessId, sessionId],
  );

  const stopProcess = useCallback(
    async (processId: string) => {
      if (!config || !sessionId) return;
      const token = guards.beginRequest(sessionId);
      setActionPending(`kill:${processId}`);
      setError(null);
      try {
        await killExecutionProcess(config, processId);
        if (!guards.isCurrent(token)) return;
        await refresh();
        if (selectedProcessId === processId) await loadLogs(processId);
      } catch (reason) {
        if (guards.isCurrent(token)) {
          setError(reason instanceof Error ? reason.message : "Terminal stop failed");
        }
      } finally {
        setActionPending(null);
      }
    },
    [config, guards, loadLogs, refresh, selectedProcessId, sessionId],
  );

  return {
    processes,
    selectedProcessId,
    logs,
    input,
    loading,
    error,
    actionPending,
    history,
    setInput,
    selectProcess: setSelectedProcessId,
    refresh,
    createTerminal,
    sendInput,
    stopProcess,
  };
}

export default usePersistentTerminal;
