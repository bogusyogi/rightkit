import { useCallback, useEffect, useRef, useState } from "react";
import {
  stagePaths,
  unstagePaths,
} from "@rightkit/agent-ui-core/lib/daemon/delivery-environments-attention";
import {
  createGitIndexMutationQueue,
  type GitIndexMutationDirection,
  type GitIndexMutationQueue,
  type GitIndexMutationSnapshot,
} from "@rightkit/agent-ui-core/lib/git/gitIndexMutationQueue";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";

export interface GitStatusEntry {
  path: string;
  indexStatus: string;
  worktreeStatus: string;
  staged: boolean;
  unstaged: boolean;
  conflict?: boolean;
}

export interface GitStatusSnapshot {
  workspaceRoot: string;
  branch: string;
  ahead: number;
  behind: number;
  isClean: boolean;
  entries: GitStatusEntry[];
  checkedAt?: string;
}

/**
 * The daemon owns Git status collection. The loader is injected until the
 * file-level status endpoint is available; the hook never shells out or
 * reads an index in the renderer.
 */
export type GitStatusLoader = (
  config: DaemonConfig,
  workspaceRoot: string,
  signal: AbortSignal,
) => Promise<GitStatusSnapshot>;

export type GitStatusProjectionStatus = "idle" | "loading" | "ready" | "error";

export interface UseGitStatusProjectionOptions {
  config: DaemonConfig | null;
  workspaceRoot: string;
  loadStatus: GitStatusLoader;
  reloadToken?: number;
}

export interface UseGitStatusProjectionResult {
  status: GitStatusProjectionStatus;
  snapshot: GitStatusSnapshot | null;
  error: string | null;
  pendingPaths: string[];
  queueSize: number;
  isMutating: boolean;
  refresh: () => Promise<void>;
  stage: (paths: string[]) => void;
  unstage: (paths: string[]) => void;
}

export const applyGitIndexMutation = (
  snapshot: GitStatusSnapshot,
  direction: GitIndexMutationDirection,
  paths: readonly string[],
): GitStatusSnapshot => {
  const changed = new Set(paths.map((path) => path.trim()).filter(Boolean));
  if (changed.size === 0) return snapshot;
  const entries = snapshot.entries.map((entry) => {
    if (!changed.has(entry.path)) return entry;
    const staged = direction === "stage";
    return {
      ...entry,
      staged,
      // A file can have both staged and unstaged hunks. Staging the
      // unstaged portion clears that second state only when the entry had no
      // staged portion before; unstage never discards worktree changes.
      unstaged: staged ? entry.staged && entry.unstaged : entry.unstaged,
    };
  });
  return {
    ...snapshot,
    entries,
    isClean: entries.every((entry) => !entry.staged && !entry.unstaged),
  };
};

const uniquePaths = (paths: string[]): string[] => [
  ...new Set(paths.map((path) => path.trim()).filter(Boolean)),
];

export function useGitStatusProjection({
  config,
  workspaceRoot,
  loadStatus,
  reloadToken = 0,
}: UseGitStatusProjectionOptions): UseGitStatusProjectionResult {
  const [status, setStatus] = useState<GitStatusProjectionStatus>("idle");
  const [snapshot, setSnapshot] = useState<GitStatusSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pendingPaths, setPendingPaths] = useState<string[]>([]);
  const requestRef = useRef<AbortController | null>(null);
  const mutationVersionRef = useRef(0);
  const configRef = useRef(config);
  const workspaceRootRef = useRef(workspaceRoot);
  configRef.current = config;
  workspaceRootRef.current = workspaceRoot;
  const snapshotRef = useRef<GitStatusSnapshot | null>(snapshot);
  const pendingRef = useRef(new Set<string>());
  const callbacksRef = useRef<{
    setError: (message: string | null) => void;
    setSnapshot: (next: GitStatusSnapshot | null) => void;
    setPendingPaths: (paths: string[]) => void;
  } | null>(null);
  snapshotRef.current = snapshot;
  callbacksRef.current = { setError, setSnapshot, setPendingPaths };

  const refresh = useCallback(async () => {
    const refreshMutationVersion = mutationVersionRef.current;
    if (!config || !workspaceRoot.trim()) {
      requestRef.current?.abort();
      setStatus("idle");
      setSnapshot(null);
      setError(null);
      return;
    }
    requestRef.current?.abort();
    const controller = new AbortController();
    requestRef.current = controller;
    setStatus("loading");
    setError(null);
    try {
      const next = await loadStatus(config, workspaceRoot, controller.signal);
      if (controller.signal.aborted) return;
      // Do not let a status response that began before an optimistic index
      // mutation erase that mutation when it resolves later.
      if (mutationVersionRef.current === refreshMutationVersion) {
        snapshotRef.current = next;
        setSnapshot(next);
      }
      setStatus("ready");
    } catch (reason) {
      if (controller.signal.aborted) return;
      setStatus("error");
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }, [config, workspaceRoot, loadStatus]);

  const queueRef = useRef<GitIndexMutationQueue | null>(null);
  if (!queueRef.current) {
    let queue!: GitIndexMutationQueue;
    queue = createGitIndexMutationQueue({
      runMutation: async (mutation: GitIndexMutationSnapshot) => {
        const currentConfig = configRef.current;
        if (!currentConfig) throw new Error("Git daemon is unavailable");
        const result = mutation.direction === "stage"
          ? await stagePaths(currentConfig, mutation.directory, mutation.paths)
          : await unstagePaths(currentConfig, mutation.directory, mutation.paths);
        if (result.exitCode !== 0) {
          throw new Error(result.stderr || result.stdout || "Git index mutation failed");
        }
      },
      onMutationComplete: () => callbacksRef.current?.setError(null),
      onMutationError: (mutation, reason) => {
        mutation.rollback?.();
        callbacksRef.current?.setError(reason instanceof Error ? reason.message : String(reason));
      },
      onPathsComplete: (paths) => {
        for (const path of paths) pendingRef.current.delete(path);
        callbacksRef.current?.setPendingPaths([...pendingRef.current]);
      },
      scheduleFlush: () => queue.flush(),
    });
    queueRef.current = queue;
  }

  // biome-ignore lint/correctness/useExhaustiveDependencies: reloadToken is external refresh signal.
  useEffect(() => {
    void refresh();
  }, [refresh, reloadToken]);

  useEffect(() => () => {
    requestRef.current?.abort();
    queueRef.current?.clear();
  }, []);

  const mutate = useCallback((direction: GitIndexMutationDirection, input: string[]) => {
    const paths = uniquePaths(input);
    if (!config || !workspaceRoot.trim() || paths.length === 0) return;
    mutationVersionRef.current += 1;
    const previous = snapshotRef.current;
    setSnapshot((current) => {
      const optimistic = current ? applyGitIndexMutation(current, direction, paths) : current;
      snapshotRef.current = optimistic;
      return optimistic;
    });
    const previousEntries = new Map(
      previous?.entries
        .filter((entry) => paths.includes(entry.path))
        .map((entry) => [entry.path, entry]),
    );
    for (const path of paths) pendingRef.current.add(path);
    setPendingPaths([...pendingRef.current]);
    queueRef.current?.enqueue({
      directory: workspaceRootRef.current,
      direction,
      paths: new Set(paths),
      rollback: () => {
        setSnapshot((latest) => {
          if (!latest || previousEntries.size === 0) return latest;
          const entries = latest.entries.map((entry) => previousEntries.get(entry.path) ?? entry);
          const next = {
            ...latest,
            entries,
            isClean: entries.every((entry) => !entry.staged && !entry.unstaged),
          };
          snapshotRef.current = next;
          return next;
        });
      },
    });
    queueRef.current?.flush();
  }, [config, workspaceRoot]);

  const stage = useCallback((paths: string[]) => mutate("stage", paths), [mutate]);
  const unstage = useCallback((paths: string[]) => mutate("unstage", paths), [mutate]);
  const queue = queueRef.current;

  return {
    status,
    snapshot,
    error,
    pendingPaths,
    queueSize: queue?.size() ?? 0,
    isMutating: queue?.isRunning() ?? false,
    refresh,
    stage,
    unstage,
  };
}
