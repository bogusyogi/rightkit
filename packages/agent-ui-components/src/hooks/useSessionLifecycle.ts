import { useCallback, useRef, useState } from "react";
import type { Dispatch, SetStateAction } from "react";
import type { NewSessionControls } from "../components/Sidebar";
import {
  createSession,
  deleteSession,
  getHealth,
  isTauriHost,
  listCommands,
  listSessions,
  resumeSession,
} from "@rightkit/agent-ui-core/lib/daemon";
import type { CommandInfo } from "@rightkit/agent-ui-core/lib/daemon";
import type { DaemonConfig, DriverKind, HealthResponse, SessionRow } from "@rightkit/agent-ui-core/lib/types";
import { useComposerStore } from "@rightkit/agent-ui-core/state/composerStore";
import { useLayoutStore } from "@rightkit/agent-ui-core/state/layoutStore";

interface UseSessionLifecycleOptions {
  config: DaemonConfig | null;
  workspaceRoot: string;
  selectedId: string | null;
  conductorModel: string;
  formatError: (error: unknown, fallback: string) => string;
  maybeLaunchAfterRefreshFailure: () => Promise<boolean>;
  onCommands: (commands: CommandInfo[]) => void;
  onHealth: (health: HealthResponse | null) => void;
  onPane: (pane: "session") => void;
  onSelectedId: (id: string | null) => void;
  onSessions: Dispatch<SetStateAction<SessionRow[]>>;
  onStatus: (status: string) => void;
  onWorkspace: (path: string) => void;
}

export function resolveNewSessionModel(
  sessionModel: string,
  conductorModel: string,
): string | undefined {
  return sessionModel.trim() || conductorModel.trim() || undefined;
}

export function useSessionLifecycle({
  config,
  workspaceRoot,
  selectedId,
  conductorModel,
  formatError,
  maybeLaunchAfterRefreshFailure,
  onCommands,
  onHealth,
  onPane,
  onSelectedId,
  onSessions,
  onStatus,
  onWorkspace,
}: UseSessionLifecycleOptions) {
  const [newSessionControls, setNewSessionControlsState] = useState<NewSessionControls>({
    driverKind: "native",
    model: localStorage.getItem("coderight.conductorModel") ?? "",
    effort: "",
    thinkingBudgetTokens: "",
    disableThinking: false,
    fableMode: false,
    executionBackend: "",
    executionWorkspaceId: "",
    allowedTools: "",
    disallowedTools: "",
  });
  const newSessionControlsRef = useRef(newSessionControls);
  const setNewSessionControls: Dispatch<SetStateAction<NewSessionControls>> = useCallback(
    (update) => {
      const next = typeof update === "function" ? update(newSessionControlsRef.current) : update;
      newSessionControlsRef.current = next;
      setNewSessionControlsState(next);
    },
    [],
  );

  const refresh = useCallback(
    async (nextConfig = config) => {
      if (!nextConfig) return;
      try {
        onStatus("Connecting...");
        const [health, rows, commandRows] = await Promise.all([
          getHealth(nextConfig),
          listSessions(nextConfig),
          listCommands(nextConfig).catch(() => []),
        ]);
        onHealth(health);
        onSessions(rows);
        onCommands(commandRows);
        if (!workspaceRoot && rows[0]?.workspaceRoot) {
          onWorkspace(rows[0].workspaceRoot);
        }
        onStatus("Ready");
      } catch (err) {
        onHealth(null);
        onSessions([]);
        if (await maybeLaunchAfterRefreshFailure()) return;
        if (!isTauriHost()) {
          onStatus("Preview mode");
          return;
        }
        onStatus(formatError(err, "Connection failed"));
      }
    },
    [
      config,
      formatError,
      maybeLaunchAfterRefreshFailure,
      onCommands,
      onHealth,
      onSessions,
      onStatus,
      onWorkspace,
      workspaceRoot,
    ],
  );

  const newSession = useCallback(
    async (
      workspaceOverride?: string,
      activeSkillsOverride: string[] = [],
      driverKindOverride?: DriverKind,
    ): Promise<string | null> => {
      const effectiveWorkspace = workspaceOverride?.trim() || workspaceRoot.trim();
      if (!config || !effectiveWorkspace) {
        onStatus("Choose a workspace first");
        return null;
      }
      try {
        const controls = newSessionControlsRef.current;
        const row = await createSession(
          config,
          effectiveWorkspace,
          "New session",
          resolveNewSessionModel(controls.model, conductorModel),
          {
            allowedTools: parseToolList(controls.allowedTools),
            disallowedTools: parseToolList(controls.disallowedTools),
            activeSkills: activeSkillsOverride,
            effort: controls.effort || undefined,
            executionBackend: controls.executionBackend.trim() || undefined,
            executionWorkspaceId: controls.executionWorkspaceId.trim() || undefined,
            driverKind: driverKindOverride ?? controls.driverKind,
            thinkingBudgetTokens: parseThinkingBudget(controls.thinkingBudgetTokens),
            disableThinking: controls.disableThinking,
            fableMode: controls.fableMode,
          },
        );
        // A new task starts as a writing surface. Reopening a workspace must
        // not resurrect docks, draft text, or attachments left by a previous task.
        useLayoutStore.getState().resetToDefault(effectiveWorkspace);
        useComposerStore.getState().clear();
        onSessions((rows) => [row, ...rows.filter((existing) => existing.id !== row.id)]);
        onSelectedId(row.id);
        onPane("session");
        onStatus("Session created");
        return row.id;
      } catch (err) {
        onStatus(formatError(err, "Session creation failed"));
        return null;
      }
    },
    [
      config,
      conductorModel,
      formatError,
      onPane,
      onSelectedId,
      onSessions,
      onStatus,
      workspaceRoot,
    ],
  );

  const deleteSessionById = useCallback(
    async (id: string) => {
      if (!config) return;
      try {
        await deleteSession(config, id);
        onSessions((rows) => rows.filter((row) => row.id !== id));
        if (selectedId === id) onSelectedId(null);
        onStatus("Session deleted");
      } catch (err) {
        onStatus(formatError(err, "Delete failed"));
      }
    },
    [config, formatError, onSelectedId, onSessions, onStatus, selectedId],
  );

  const resumeDormant = useCallback(
    async (sessionId: string | null) => {
      if (!config || !sessionId) return;
      try {
        onStatus("Resuming...");
        await resumeSession(config, sessionId);
        await refresh();
        onStatus("Resumed");
      } catch (err) {
        onStatus(formatError(err, "Resume failed"));
      }
    },
    [config, formatError, onStatus, refresh],
  );

  return {
    newSessionControls,
    setNewSessionControls,
    refresh,
    newSession,
    deleteSessionById,
    resumeDormant,
  };
}

function parseToolList(value: string) {
  return value
    .split(/[\s,]+/)
    .map((tool) => tool.trim())
    .filter(Boolean);
}

function parseThinkingBudget(value: string) {
  const trimmed = value.trim();
  if (!trimmed) return undefined;
  const parsed = Number.parseInt(trimmed, 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : undefined;
}
