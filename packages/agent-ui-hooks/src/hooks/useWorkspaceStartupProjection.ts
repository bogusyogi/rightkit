import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type SetStateAction,
} from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";

export interface WorkspaceStartupTask {
  id: string;
  label: string;
  required: boolean;
  state: string;
}

export interface WorkspaceStartupEntry {
  root: string;
  fingerprint: string;
  state: string;
  approved: boolean;
  autoApprove?: boolean;
  hostAutoApprove?: boolean;
  projectAutoApprove?: boolean;
  commands: WorkspaceStartupTask[];
  warning?: string | null;
  reason?: string | null;
  activeTaskId?: string | null;
}

export interface WorkspaceStartupView {
  entry: WorkspaceStartupEntry | null;
  output: string;
}

export interface WorkspaceStartupClient {
  prepare: (cwd: string) => Promise<WorkspaceStartupStatus | null>;
  approve: (root: string, fingerprint: string) => Promise<WorkspaceStartupStatus>;
  retry: (root: string) => Promise<WorkspaceStartupStatus>;
  continue: (root: string, fingerprint: string) => Promise<WorkspaceStartupStatus>;
}

export interface WorkspaceStartupStatus extends WorkspaceStartupEntry {}

interface WorkspaceStartupEvent {
  root?: string;
  fingerprint?: string;
  state?: string;
  taskId?: string | null;
  taskLabel?: string | null;
  required?: boolean | null;
  reason?: string | null;
}

interface WorkspaceStartupOutputEvent {
  root?: string;
  fingerprint?: string;
  taskId?: string;
  taskLabel?: string;
  stream?: string;
  content?: string;
}

interface PendingPrepare {
  cwd: string;
  resolve: (ready: boolean) => void;
  reject: (error: unknown) => void;
}

export interface UseWorkspaceStartupProjectionOptions {
  config: DaemonConfig | null;
  client?: WorkspaceStartupClient;
}

export interface UseWorkspaceStartupProjectionResult {
  view: WorkspaceStartupView;
  prepareWorkspaceStartup: (cwd: string) => Promise<boolean>;
  approveStartup: () => Promise<void>;
  retryStartup: () => Promise<void>;
  continueStartup: () => Promise<void>;
}

const READY_STATES = new Set(["ready", "continued", "disabled"]);
const BLOCKING_STATES = new Set(["running", "approval_required", "failed"]);
const STARTUP_OUTPUT_MAX = 64 * 1024;

/**
 * Projection of Aethon's workspace-startup state machine. The renderer owns
 * only approval/progress/output state; the client seam delegates preparation
 * and command execution to the daemon/host adapter.
 */
export function useWorkspaceStartupProjection({
  config,
  client,
}: UseWorkspaceStartupProjectionOptions): UseWorkspaceStartupProjectionResult {
  const [state, setState] = useState<ProjectionState>({ entries: {}, outputByRoot: {} });
  const stateRef = useRef(state);
  const pendingRef = useRef<Map<string, PendingPrepare[]>>(new Map());
  stateRef.current = state;

  const startupClient = useMemo(
    () => client ?? (config ? createWorkspaceStartupClient(config) : null),
    [client, config],
  );

  const applyStatus = useCallback(
    (status: WorkspaceStartupStatus): WorkspaceStartupStatus => {
      setState((previous) => {
        const entries = { ...previous.entries, [status.root]: normalizeStatus(status) };
        return {
          ...previous,
          activeRoot: isVisibleStartupState(status.state)
            ? status.root
            : previous.activeRoot === status.root
              ? null
              : previous.activeRoot,
          entries,
        };
      });
      settlePendingIfReady(status.root, status, pendingRef.current);
      return status;
    },
    [],
  );

  const prepareWorkspaceStartup = useCallback(
    async (cwd: string): Promise<boolean> => {
      if (!startupClient || !cwd.trim()) return false;
      const response = await startupClient.prepare(cwd);
      const status = response ?? disabledStatus(cwd);
      applyStatus(status);
      if (READY_STATES.has(status.state)) return true;
      if (!BLOCKING_STATES.has(status.state)) return false;
      return new Promise<boolean>((resolve, reject) => {
        const list = pendingRef.current.get(status.root) ?? [];
        list.push({ cwd, resolve, reject });
        pendingRef.current.set(status.root, list);
      });
    },
    [applyStatus, startupClient],
  );

  const rerunPending = useCallback(
    async (root: string) => {
      const pending = pendingRef.current.get(root);
      const cwd = pending?.[0]?.cwd ?? root;
      if (!startupClient) return;
      try {
        const response = await startupClient.prepare(cwd);
        const status = applyStatus(response ?? disabledStatus(cwd));
        if (!BLOCKING_STATES.has(status.state)) settlePendingIfReady(root, status, pendingRef.current);
      } catch (error) {
        rejectPending(root, error, pendingRef.current);
      }
    },
    [applyStatus, startupClient],
  );

  const approveStartup = useCallback(async () => {
    const entry = activeEntry(stateRef.current);
    if (!entry || !startupClient) return;
    applyStatus(await startupClient.approve(entry.root, entry.fingerprint));
    await rerunPending(entry.root);
  }, [applyStatus, rerunPending, startupClient]);

  const retryStartup = useCallback(async () => {
    const entry = activeEntry(stateRef.current);
    if (!entry || !startupClient) return;
    applyStatus(await startupClient.retry(entry.root));
  }, [applyStatus, startupClient]);

  const continueStartup = useCallback(async () => {
    const entry = activeEntry(stateRef.current);
    if (!entry || !startupClient) return;
    const status = applyStatus(await startupClient.continue(entry.root, entry.fingerprint));
    settlePendingIfReady(entry.root, status, pendingRef.current);
  }, [applyStatus, startupClient]);

  useEffect(() => {
    let cancelled = false;
    const unlisteners: UnlistenFn[] = [];
    void (async () => {
      try {
        const offStatus = await listen<WorkspaceStartupEvent>(
          "workspace-startup-status",
          (event) => applyStartupEvent(event.payload, setState),
        );
        const offOutput = await listen<WorkspaceStartupOutputEvent>(
          "workspace-startup-output",
          (event) => applyStartupOutput(event.payload, setState),
        );
        if (cancelled) {
          safeUnlisten(offStatus);
          safeUnlisten(offOutput);
          return;
        }
        unlisteners.push(offStatus, offOutput);
      } catch {
        // The hook is also usable in the browser preview, outside Tauri.
      }
    })();
    return () => {
      cancelled = true;
      for (const unlisten of unlisteners) safeUnlisten(unlisten);
    };
  }, []);

  const entry = activeEntry(state);
  return useMemo(
    () => ({
      view: { entry, output: entry ? state.outputByRoot[entry.root] ?? "" : "" },
      prepareWorkspaceStartup,
      approveStartup,
      retryStartup,
      continueStartup,
    }),
    [approveStartup, continueStartup, entry, prepareWorkspaceStartup, retryStartup, state.outputByRoot],
  );
}

interface ProjectionState {
  activeRoot?: string | null;
  entries: Record<string, WorkspaceStartupEntry>;
  outputByRoot: Record<string, string>;
}

export function createWorkspaceStartupClient(config: DaemonConfig): WorkspaceStartupClient {
  return {
    prepare: (cwd) => startupRequest<WorkspaceStartupStatus | null>(config, "/workspace/startup/prepare", { cwd }),
    approve: (root, fingerprint) => startupRequest<WorkspaceStartupStatus>(config, "/workspace/startup/approve", { root, fingerprint }),
    retry: (root) => startupRequest<WorkspaceStartupStatus>(config, "/workspace/startup/retry", { root }),
    continue: (root, fingerprint) => startupRequest<WorkspaceStartupStatus>(config, "/workspace/startup/continue", { root, fingerprint }),
  };
}

async function startupRequest<T>(config: DaemonConfig, path: string, body: unknown): Promise<T> {
  const response = await fetch(`${config.url.replace(/\/+$/, "")}${path}`, {
    method: "POST",
    headers: {
      accept: "application/json",
      "content-type": "application/json",
      ...(config.token ? { authorization: `Bearer ${config.token}` } : {}),
    },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    const detail = await response.text().catch(() => "");
    throw new Error(detail || `workspace startup request failed (${response.status})`);
  }
  return (await response.json()) as T;
}

function normalizeStatus(status: WorkspaceStartupStatus): WorkspaceStartupEntry {
  return {
    ...status,
    commands: status.commands ?? [],
    warning: status.warning ?? null,
    reason: status.reason ?? null,
    activeTaskId: status.activeTaskId ?? null,
  };
}

function applyStartupEvent(
  event: WorkspaceStartupEvent,
  setState: (action: SetStateAction<ProjectionState>) => void,
): void {
  if (!event.root || !event.state) return;
  setState((previous) => {
    const existing = previous.entries[event.root!] ?? {
      root: event.root!,
      fingerprint: event.fingerprint ?? "",
      state: event.state!,
      approved: false,
      commands: [],
      warning: null,
      reason: null,
      activeTaskId: null,
    };
    const commands = existing.commands.slice();
    if (event.taskId) {
      const task = {
        id: event.taskId,
        label: event.taskLabel ?? event.taskId,
        required: event.required ?? true,
        state: event.state!,
      };
      const index = commands.findIndex((candidate) => candidate.id === task.id);
      if (index >= 0) commands[index] = { ...commands[index], ...task };
      else commands.push(task);
    }
    return {
      ...previous,
      activeRoot: isVisibleStartupState(event.state!)
        ? event.root
        : previous.activeRoot === event.root
          ? null
          : previous.activeRoot,
      entries: {
        ...previous.entries,
        [event.root!]: {
          ...existing,
          fingerprint: event.fingerprint ?? existing.fingerprint,
          state: event.state!,
          commands,
          reason: event.reason ?? existing.reason ?? null,
          activeTaskId: event.taskId ?? existing.activeTaskId ?? null,
        },
      },
    };
  });
}

function applyStartupOutput(
  event: WorkspaceStartupOutputEvent,
  setState: (action: SetStateAction<ProjectionState>) => void,
): void {
  if (!event.root || !event.content) return;
  const prefix = event.taskLabel && event.stream
    ? `[startup:${event.taskLabel}:${event.stream}] `
    : "[startup] ";
  const content = `${prefix}${event.content}`.replace(/\r?\n/g, "\r\n");
  setState((previous) => ({
    ...previous,
    activeRoot: previous.activeRoot ?? event.root,
    outputByRoot: {
      ...previous.outputByRoot,
      [event.root!]: trimOutput(`${previous.outputByRoot[event.root!] ?? ""}${content}`),
    },
  }));
}

function activeEntry(state: ProjectionState): WorkspaceStartupEntry | null {
  return state.activeRoot ? state.entries[state.activeRoot] ?? null : null;
}

function isVisibleStartupState(state: string): boolean {
  return state === "running" || state === "approval_required" || state === "failed";
}

function disabledStatus(root: string): WorkspaceStartupStatus {
  return { root, fingerprint: "", state: "disabled", approved: true, commands: [] };
}

function settlePendingIfReady(root: string, status: WorkspaceStartupStatus, pending: Map<string, PendingPrepare[]>): void {
  if (!READY_STATES.has(status.state)) return;
  const list = pending.get(root);
  if (!list) return;
  pending.delete(root);
  for (const item of list) item.resolve(true);
}

function rejectPending(root: string, error: unknown, pending: Map<string, PendingPrepare[]>): void {
  const list = pending.get(root);
  if (!list) return;
  pending.delete(root);
  for (const item of list) item.reject(error);
}

function trimOutput(value: string): string {
  return value.length > STARTUP_OUTPUT_MAX ? value.slice(value.length - STARTUP_OUTPUT_MAX) : value;
}

function safeUnlisten(unlisten: UnlistenFn): void {
  try {
    unlisten();
  } catch {
    // Cleanup must not turn an already-disposed Tauri listener into an error.
  }
}
