// D4 — Delivery store: typed branch / worktree / stage / commit / push / PR /
// checks / failing-logs state with visible commands and errors.
//
// This store is owned by Agent 3 (desktop capabilities, Wave 5) and reuses
// the existing worktree / git / notification seams. It does NOT touch
// the generated client, or `AppBody`.

import { create } from "zustand";

import {
  cancelBackgroundCommand,
  commitChanges,
  createPullRequest,
  createWorktree,
  getCheckLogs,
  getDeliveryStatus,
  getPullRequest,
  listBranches,
  listPullRequestChecks,
  listPullRequestThreads,
  listWorktrees,
  pullBranch,
  pushBranch,
  removeWorktree,
  rerunCheck,
  runBackgroundCommand,
  stagePaths,
  unstagePaths,
} from "../lib/daemon/delivery-environments-attention.js";
import type { DeliveryCommandResult } from "../lib/daemon/delivery-environments-attention.js";
import type { CommandRecord, DeliveryHostKind, DeliverySnapshot } from "../lib/types.js";
import { messageOf } from "../lib/utils.js";

export interface DeliveryCommandHistory {
  commandId: string;
  command: string;
  args: string[];
  cwd: string;
  status: CommandRecord["status"];
  exitCode: number | null;
  startedAt: string;
  finishedAt?: string | null;
  stdoutTail: string;
  stderrTail: string;
  hostKind: DeliveryHostKind;
  actionLabel: string;
}

export type DeliveryStoreState = {
  snapshot: DeliverySnapshot | null;
  snapshotPending: boolean;
  history: DeliveryCommandHistory[];
  actionPending: string | null;
  error: string | null;
  reset: () => void;
  loadSnapshot: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
    host: DeliveryHostKind,
  ) => Promise<void>;
  loadBranches: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
  ) => Promise<CommandRecord | null>;
  loadWorktrees: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
  ) => Promise<CommandRecord | null>;
  createWorktree: (
    config: import("../lib/types.js").DaemonConfig | null,
    args: {
      workspaceRoot: string;
      branch: string;
      base?: string;
      path?: string;
      force?: boolean;
    },
  ) => Promise<boolean>;
  removeWorktree: (
    config: import("../lib/types.js").DaemonConfig | null,
    args: { workspaceRoot: string; path: string; force?: boolean },
  ) => Promise<boolean>;
  stagePaths: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
    paths: string[],
  ) => Promise<boolean>;
  unstagePaths: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
    paths: string[],
  ) => Promise<boolean>;
  commitChanges: (
    config: import("../lib/types.js").DaemonConfig | null,
    args: {
      workspaceRoot: string;
      message: string;
      amend?: boolean;
      signOff?: boolean;
    },
  ) => Promise<boolean>;
  pushBranch: (
    config: import("../lib/types.js").DaemonConfig | null,
    args: {
      workspaceRoot: string;
      remote?: string;
      branch?: string;
      setUpstream?: boolean;
      force?: boolean;
    },
  ) => Promise<boolean>;
  pullBranch: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
  ) => Promise<boolean>;
  createPullRequest: (
    config: import("../lib/types.js").DaemonConfig | null,
    args: {
      workspaceRoot: string;
      title?: string;
      body?: string;
      base?: string;
      head?: string;
      draft?: boolean;
    },
  ) => Promise<boolean>;
  loadPullRequest: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
    id: string,
  ) => Promise<DeliveryCommandResult | null>;
  listPullRequestChecks: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
    id: string,
  ) => Promise<DeliveryCommandResult | null>;
  rerunCheck: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
    id: string,
    checkId: string,
  ) => Promise<boolean>;
  loadCheckLogs: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
    id: string,
    checkId: string,
  ) => Promise<DeliveryCommandResult | null>;
  listPullRequestThreads: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
    id: string,
  ) => Promise<DeliveryCommandResult | null>;
  runBackgroundCommand: (
    config: import("../lib/types.js").DaemonConfig | null,
    args: {
      workspaceRoot: string;
      command: string;
      args: string[];
      timeoutMs?: number;
    },
  ) => Promise<CommandRecord | null>;
  cancelBackgroundCommand: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
    commandId: string,
  ) => Promise<boolean>;
  clearHistory: () => void;
};

const HISTORY_LIMIT = 50;

function recordHistory(
  history: DeliveryCommandHistory[],
  record: CommandRecord,
  actionLabel: string,
): DeliveryCommandHistory[] {
  const next: DeliveryCommandHistory = {
    commandId: record.commandId,
    command: record.command,
    args: record.args,
    cwd: record.cwd,
    status: record.status,
    exitCode: record.exitCode ?? null,
    startedAt: record.startedAt,
    finishedAt: record.finishedAt,
    stdoutTail: record.stdoutTail,
    stderrTail: record.stderrTail,
    hostKind: record.hostKind,
    actionLabel,
  };
  const without = history.filter((row) => row.commandId !== next.commandId);
  return [next, ...without].slice(0, HISTORY_LIMIT);
}

function recordResultHistory(
  history: DeliveryCommandHistory[],
  result: DeliveryCommandResult,
  actionLabel: string,
  cwd: string,
): DeliveryCommandHistory[] {
  const now = new Date().toISOString();
  const next: DeliveryCommandHistory = {
    commandId: `delivery-result-${Date.now()}`,
    command: result.command,
    args: [],
    cwd,
    status: result.exitCode === 0 ? "exited" : "spawnFailed",
    exitCode: result.exitCode,
    startedAt: now,
    finishedAt: now,
    stdoutTail: result.stdout,
    stderrTail: result.stderr,
    hostKind: "local",
    actionLabel,
  };
  return [next, ...history].slice(0, HISTORY_LIMIT);
}

export const useDeliveryStore = create<DeliveryStoreState>((set, get) => ({
  snapshot: null,
  snapshotPending: false,
  history: [],
  actionPending: null,
  error: null,
  reset: () => set({ snapshot: null, history: [], actionPending: null, error: null }),
  async loadSnapshot(config, workspaceRoot, host) {
    if (!config) return;
    set({ snapshotPending: true, error: null });
    try {
      const snapshot = await getDeliveryStatus(config, workspaceRoot, host);
      set({ snapshot, snapshotPending: false });
    } catch (error) {
      set({ snapshotPending: false, error: messageOf(error) });
    }
  },
  async loadBranches(config, workspaceRoot) {
    if (!config) return null;
    set({ actionPending: "branches", error: null });
    try {
      const record = await listBranches(config, workspaceRoot);
      set({
        actionPending: null,
        history: recordHistory(get().history, record, "list branches"),
      });
      return record;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return null;
    }
  },
  async loadWorktrees(config, workspaceRoot) {
    if (!config) return null;
    set({ actionPending: "worktrees", error: null });
    try {
      const record = await listWorktrees(config, workspaceRoot);
      set({
        actionPending: null,
        history: recordHistory(get().history, record, "list worktrees"),
      });
      return record;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return null;
    }
  },
  async createWorktree(config, args) {
    if (!config) return false;
    set({ actionPending: "create-worktree", error: null });
    try {
      const result = await createWorktree(config, args);
      const exitOk = result.exitCode === 0;
      set({
        actionPending: null,
        history: get()
          .history.concat({
            commandId: `local-${Date.now()}`,
            command: result.command,
            args: [],
            cwd: args.workspaceRoot,
            status: exitOk ? "exited" : "spawnFailed",
            exitCode: result.exitCode,
            startedAt: new Date().toISOString(),
            finishedAt: new Date().toISOString(),
            stdoutTail: result.stdout,
            stderrTail: result.stderr,
            hostKind: "local",
            actionLabel: `worktree add ${args.branch}`,
          })
          .slice(0, HISTORY_LIMIT),
        error: exitOk ? null : result.stderr || result.stdout,
      });
      return exitOk;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return false;
    }
  },
  async removeWorktree(config, args) {
    if (!config) return false;
    set({ actionPending: "remove-worktree", error: null });
    try {
      const result = await removeWorktree(config, args);
      const exitOk = result.exitCode === 0;
      set({
        actionPending: null,
        error: exitOk ? null : result.stderr || result.stdout,
      });
      return exitOk;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return false;
    }
  },
  async stagePaths(config, workspaceRoot, paths) {
    if (!config) return false;
    set({ actionPending: "stage", error: null });
    try {
      const result = await stagePaths(config, workspaceRoot, paths);
      const exitOk = result.exitCode === 0;
      set({
        actionPending: null,
        error: exitOk ? null : result.stderr || result.stdout,
      });
      return exitOk;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return false;
    }
  },
  async unstagePaths(config, workspaceRoot, paths) {
    if (!config) return false;
    set({ actionPending: "unstage", error: null });
    try {
      const result = await unstagePaths(config, workspaceRoot, paths);
      const exitOk = result.exitCode === 0;
      set({
        actionPending: null,
        error: exitOk ? null : result.stderr || result.stdout,
      });
      return exitOk;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return false;
    }
  },
  async commitChanges(config, args) {
    if (!config) return false;
    set({ actionPending: "commit", error: null });
    try {
      const result = await commitChanges(config, args);
      const exitOk = result.exitCode === 0;
      set({
        actionPending: null,
        error: exitOk ? null : result.stderr || result.stdout,
      });
      return exitOk;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return false;
    }
  },
  async pushBranch(config, args) {
    if (!config) return false;
    set({ actionPending: "push", error: null });
    try {
      const result = await pushBranch(config, args);
      const exitOk = result.exitCode === 0;
      set({
        actionPending: null,
        error: exitOk ? null : result.stderr || result.stdout,
      });
      return exitOk;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return false;
    }
  },
  async pullBranch(config, workspaceRoot) {
    if (!config) return false;
    set({ actionPending: "pull", error: null });
    try {
      const result = await pullBranch(config, workspaceRoot);
      const exitOk = result.exitCode === 0;
      set({
        actionPending: null,
        error: exitOk ? null : result.stderr || result.stdout,
      });
      return exitOk;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return false;
    }
  },
  async createPullRequest(config, args) {
    if (!config) return false;
    set({ actionPending: "pr-create", error: null });
    try {
      const result = await createPullRequest(config, args);
      const exitOk = result.exitCode === 0;
      set({
        actionPending: null,
        error: exitOk ? null : result.stderr || result.stdout,
      });
      return exitOk;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return false;
    }
  },
  async loadPullRequest(config, workspaceRoot, id) {
    if (!config) return null;
    set({ actionPending: "pr-view", error: null });
    try {
      const record = await getPullRequest(config, workspaceRoot, id);
      set({
        actionPending: null,
        history: recordResultHistory(get().history, record, `view PR ${id}`, workspaceRoot),
      });
      return record;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return null;
    }
  },
  async listPullRequestChecks(config, workspaceRoot, id) {
    if (!config) return null;
    set({ actionPending: "pr-checks", error: null });
    try {
      const record = await listPullRequestChecks(config, workspaceRoot, id);
      set({
        actionPending: null,
        history: recordResultHistory(get().history, record, `list checks ${id}`, workspaceRoot),
      });
      return record;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return null;
    }
  },
  async rerunCheck(config, workspaceRoot, id, checkId) {
    if (!config) return false;
    set({ actionPending: "pr-rerun", error: null });
    try {
      const result = await rerunCheck(config, workspaceRoot, id, checkId);
      const exitOk = result.exitCode === 0;
      set({
        actionPending: null,
        error: exitOk ? null : result.stderr || result.stdout,
      });
      return exitOk;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return false;
    }
  },
  async loadCheckLogs(config, workspaceRoot, id, checkId) {
    if (!config) return null;
    set({ actionPending: "pr-check-logs", error: null });
    try {
      const record = await getCheckLogs(config, workspaceRoot, id, checkId);
      set({
        actionPending: null,
        history: recordResultHistory(
          get().history,
          record,
          `check logs ${id}/${checkId}`,
          workspaceRoot,
        ),
      });
      return record;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return null;
    }
  },
  async listPullRequestThreads(config, workspaceRoot, id) {
    if (!config) return null;
    set({ actionPending: "pr-threads", error: null });
    try {
      const record = await listPullRequestThreads(config, workspaceRoot, id);
      set({
        actionPending: null,
        history: recordResultHistory(get().history, record, `list threads ${id}`, workspaceRoot),
      });
      return record;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return null;
    }
  },
  async runBackgroundCommand(config, args) {
    if (!config) return null;
    set({ actionPending: "command", error: null });
    try {
      const record = await runBackgroundCommand(config, args);
      set({
        actionPending: null,
        history: recordHistory(get().history, record, `command ${args.command}`),
      });
      return record;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return null;
    }
  },
  async cancelBackgroundCommand(config, workspaceRoot, commandId) {
    if (!config) return false;
    set({ actionPending: "cancel-command", error: null });
    try {
      const result = await cancelBackgroundCommand(config, workspaceRoot, commandId);
      set({ actionPending: null });
      return result.cancelled;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return false;
    }
  },
  clearHistory: () => set({ history: [] }),
}));
