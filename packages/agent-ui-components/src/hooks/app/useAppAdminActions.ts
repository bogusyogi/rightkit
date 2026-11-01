import type React from "react";
import { useCallback } from "react";
import {
  getAwaySummary,
  getExecutionProcessLogs,
  getRuntimeMiddleware,
  getSettingsToolsets,
  listExecutionProcesses,
  listToolsetGateways,
  setRuntimeMiddlewareEnabled,
} from "@rightkit/agent-ui-core/lib/daemon";
import type { ToolsetDraft } from "../../components/settings/types";
import { parseCommaList } from "@rightkit/agent-ui-core/lib/utils";
import type {
  AwaySummary,
  CoreSandboxSettings,
  DaemonConfig,
  ExecutionProcessLogs,
  ExecutionProcessRow,
  SessionRow,
  SettingsToolsetsResponse,
  ToolGatewaySnapshot,
} from "@rightkit/agent-ui-core/lib/types";
import { useDiagnosticsStore } from "@rightkit/agent-ui-core/state/diagnosticsStore";

interface UseAppAdminActionsParams {
  config: DaemonConfig | null;
  selected: SessionRow | null;
  workspaceRoot: string;
  awaySummaryCursor: number;
  toolsetDraft: ToolsetDraft;
  emptyToolsetDraft: ToolsetDraft;
  loadRuntime: () => Promise<void>;
  setStatus: (value: string) => void;
  messageOf: (error: unknown, fallback: string) => string;
  setCoreSandboxSettings: React.Dispatch<React.SetStateAction<CoreSandboxSettings | null>>;
  setTerminalProcesses: React.Dispatch<React.SetStateAction<ExecutionProcessRow[]>>;
  setSelectedTerminalProcessId: React.Dispatch<React.SetStateAction<string | null>>;
  setTerminalLogs: React.Dispatch<React.SetStateAction<ExecutionProcessLogs | null>>;
  setTerminalLoading: React.Dispatch<React.SetStateAction<boolean>>;
  setTerminalError: React.Dispatch<React.SetStateAction<string | null>>;
  setAwaySummaryPending: React.Dispatch<React.SetStateAction<boolean>>;
  setAwaySummary: React.Dispatch<React.SetStateAction<AwaySummary | null>>;
  setAwaySummaryCursor: React.Dispatch<React.SetStateAction<number>>;
  setSettingsToolsets: React.Dispatch<React.SetStateAction<SettingsToolsetsResponse | null>>;
  setToolGatewaySnapshot: React.Dispatch<React.SetStateAction<ToolGatewaySnapshot | null>>;
  setToolsetDraft: React.Dispatch<React.SetStateAction<ToolsetDraft>>;
  setToolsetActionPending: React.Dispatch<React.SetStateAction<string | null>>;
  setToolsetError: React.Dispatch<React.SetStateAction<string | null>>;
  setRuntimeMiddlewarePending: React.Dispatch<React.SetStateAction<string | null>>;
  setRuntimeMiddlewareError: React.Dispatch<React.SetStateAction<string | null>>;
  saveSettingsToolset: typeof import("@rightkit/agent-ui-core/lib/daemon").saveSettingsToolset;
  deleteSettingsToolset: typeof import("@rightkit/agent-ui-core/lib/daemon").deleteSettingsToolset;
}

export function useAppAdminActions(params: UseAppAdminActionsParams) {
  const {
    config,
    selected,
    workspaceRoot,
    awaySummaryCursor,
    toolsetDraft,
    emptyToolsetDraft,
    loadRuntime,
    setStatus,
    messageOf,
    setCoreSandboxSettings,
    setTerminalProcesses,
    setSelectedTerminalProcessId,
    setTerminalLogs,
    setTerminalLoading,
    setTerminalError,
    setAwaySummaryPending,
    setAwaySummary,
    setAwaySummaryCursor,
    setSettingsToolsets,
    setToolGatewaySnapshot,
    setToolsetDraft,
    setToolsetActionPending,
    setToolsetError,
    setRuntimeMiddlewarePending,
    setRuntimeMiddlewareError,
    saveSettingsToolset,
    deleteSettingsToolset,
  } = params;

  const loadDiagnostics = useCallback(async () => {
    await useDiagnosticsStore
      .getState()
      .load(config, workspaceRoot, setStatus, setCoreSandboxSettings);
  }, [config, setCoreSandboxSettings, setStatus, workspaceRoot]);

  const loadTerminalProcesses = useCallback(async () => {
    const terminalWorkspaceRoot = selected?.workspaceRoot || workspaceRoot;
    if (!config || !terminalWorkspaceRoot.trim()) {
      setTerminalProcesses([]);
      setSelectedTerminalProcessId(null);
      setTerminalLogs(null);
      return;
    }
    setTerminalLoading(true);
    setTerminalError(null);
    try {
      const response = await listExecutionProcesses(config, terminalWorkspaceRoot);
      setTerminalProcesses(response.processes);
      setSelectedTerminalProcessId((current: string | null) => {
        if (current && response.processes.some((process) => process.id === current)) return current;
        return response.processes[0]?.id ?? null;
      });
    } catch (err) {
      setTerminalError(messageOf(err, "Terminal processes failed"));
    } finally {
      setTerminalLoading(false);
    }
  }, [
    config,
    messageOf,
    selected?.workspaceRoot,
    setSelectedTerminalProcessId,
    setTerminalError,
    setTerminalLoading,
    setTerminalLogs,
    setTerminalProcesses,
    workspaceRoot,
  ]);

  const loadTerminalLogs = useCallback(
    async (processId: string) => {
      if (!config) return;
      try {
        const response = await getExecutionProcessLogs(config, processId, 128 * 1024);
        setTerminalLogs(response.logs);
        setTerminalError(null);
      } catch (err) {
        setTerminalError(messageOf(err, "Terminal logs failed"));
      }
    },
    [config, messageOf, setTerminalError, setTerminalLogs],
  );

  const loadAwaySummary = useCallback(async () => {
    if (!config || !selected) return;
    setAwaySummaryPending(true);
    try {
      const next = await getAwaySummary(config, selected.id, awaySummaryCursor);
      setAwaySummary(next);
      setAwaySummaryCursor(next.nextIndex);
      setStatus(next.hasActivity ? "Return digest refreshed" : "No new session activity");
    } catch (err) {
      setStatus(messageOf(err, "Away summary failed"));
    } finally {
      setAwaySummaryPending(false);
    }
  }, [
    awaySummaryCursor,
    config,
    messageOf,
    selected,
    setAwaySummary,
    setAwaySummaryCursor,
    setAwaySummaryPending,
    setStatus,
  ]);

  const loadSettingsToolsets = useCallback(async () => {
    if (!config || !workspaceRoot.trim()) {
      setSettingsToolsets(null);
      return;
    }
    setToolsetActionPending("refresh");
    setToolsetError(null);
    try {
      const next = await getSettingsToolsets(config, workspaceRoot.trim());
      const gateways = await listToolsetGateways(config);
      setSettingsToolsets(next);
      setToolGatewaySnapshot(gateways);
    } catch (err) {
      const errorMessage = messageOf(err, "Toolset settings failed");
      setToolsetError(errorMessage);
      setStatus(errorMessage);
    } finally {
      setToolsetActionPending(null);
    }
  }, [
    config,
    messageOf,
    setSettingsToolsets,
    setStatus,
    setToolGatewaySnapshot,
    setToolsetActionPending,
    setToolsetError,
    workspaceRoot,
  ]);

  const submitSettingsToolset = useCallback(async () => {
    if (!config || !workspaceRoot.trim()) return;
    const id = toolsetDraft.id.trim();
    if (!id) {
      setStatus("Toolset id required");
      return;
    }
    setToolsetActionPending("save");
    setToolsetError(null);
    try {
      const next = await saveSettingsToolset(config, workspaceRoot.trim(), id, {
        name: toolsetDraft.name.trim() || id,
        description: toolsetDraft.description.trim(),
        allowedTools: parseCommaList(toolsetDraft.allowedTools, { dedupe: true }),
        disallowedTools: parseCommaList(toolsetDraft.disallowedTools, { dedupe: true }),
        readOnly: toolsetDraft.readOnly,
        defaultBehavioralSkills: parseCommaList(toolsetDraft.defaultBehavioralSkills, {
          dedupe: true,
        }),
      });
      setSettingsToolsets(next);
      setToolsetDraft(emptyToolsetDraft);
      setStatus(`Saved toolset ${id}`);
    } catch (err) {
      const errorMessage = messageOf(err, "Save toolset failed");
      setToolsetError(errorMessage);
      setStatus(errorMessage);
    } finally {
      setToolsetActionPending(null);
    }
  }, [
    config,
    emptyToolsetDraft,
    messageOf,
    saveSettingsToolset,
    setSettingsToolsets,
    setStatus,
    setToolsetActionPending,
    setToolsetDraft,
    setToolsetError,
    toolsetDraft,
    workspaceRoot,
  ]);

  const removeSettingsToolset = useCallback(
    async (id: string) => {
      if (!config || !workspaceRoot.trim()) return;
      setToolsetActionPending(`delete:${id}`);
      setToolsetError(null);
      try {
        const next = await deleteSettingsToolset(config, workspaceRoot.trim(), id);
        setSettingsToolsets(next);
        setStatus(`Deleted toolset ${id}`);
      } catch (err) {
        const errorMessage = messageOf(err, "Delete toolset failed");
        setToolsetError(errorMessage);
        setStatus(errorMessage);
      } finally {
        setToolsetActionPending(null);
      }
    },
    [
      config,
      deleteSettingsToolset,
      messageOf,
      setSettingsToolsets,
      setStatus,
      setToolsetActionPending,
      setToolsetError,
      workspaceRoot,
    ],
  );

  const refreshRuntimeMiddleware = useCallback(async () => {
    if (!config) return;
    setRuntimeMiddlewarePending("refresh");
    setRuntimeMiddlewareError(null);
    try {
      await getRuntimeMiddleware(config);
      await loadRuntime();
      setStatus("Runtime middleware refreshed");
    } catch (err) {
      const errorMessage = messageOf(err, "Runtime middleware refresh failed");
      setRuntimeMiddlewareError(errorMessage);
      setStatus(errorMessage);
    } finally {
      setRuntimeMiddlewarePending(null);
    }
  }, [
    config,
    loadRuntime,
    messageOf,
    setRuntimeMiddlewareError,
    setRuntimeMiddlewarePending,
    setStatus,
  ]);

  const toggleRuntimeMiddleware = useCallback(
    async (id: string, enabled: boolean) => {
      if (!config) return;
      setRuntimeMiddlewarePending(id);
      setRuntimeMiddlewareError(null);
      try {
        await setRuntimeMiddlewareEnabled(config, id, enabled);
        await loadRuntime();
        setStatus(`Runtime middleware ${enabled ? "enabled" : "disabled"}`);
      } catch (err) {
        const errorMessage = messageOf(err, "Runtime middleware update failed");
        setRuntimeMiddlewareError(errorMessage);
        setStatus(errorMessage);
      } finally {
        setRuntimeMiddlewarePending(null);
      }
    },
    [
      config,
      loadRuntime,
      messageOf,
      setRuntimeMiddlewareError,
      setRuntimeMiddlewarePending,
      setStatus,
    ],
  );

  return {
    loadDiagnostics,
    loadTerminalProcesses,
    loadTerminalLogs,
    loadAwaySummary,
    loadSettingsToolsets,
    submitSettingsToolset,
    removeSettingsToolset,
    refreshRuntimeMiddleware,
    toggleRuntimeMiddleware,
  };
}
