import { useCallback, useEffect, useRef, useState } from "react";
import {
  createWorktree,
  listProjectWorktrees,
  removeProjectWorktree,
} from "@rightkit/agent-ui-core/lib/worktrees/worktreeProjectionManager";
import type {
  CreateWorktreeArgs,
  ProjectRef,
  RemoveWorktreeOptions,
  WorktreeMetadata,
} from "@rightkit/agent-ui-core/lib/worktrees/worktreeProjectionManager";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";

export type WorktreeProjectionStatus = "idle" | "loading" | "ready" | "unavailable" | "error";

export interface UseWorktreeProjectionOptions {
  config: DaemonConfig | null;
  project: ProjectRef | null;
  /** Bumped by the caller after a daemon event or an external Git operation. */
  reloadToken?: number;
  /** Keep the last successful listing visible while a refresh is failing. */
  force?: boolean;
}

export interface UseWorktreeProjectionResult {
  status: WorktreeProjectionStatus;
  worktrees: WorktreeMetadata[];
  lastKnown: boolean;
  reason: string | null;
  refresh: () => Promise<void>;
  create: (args: CreateWorktreeArgs) => Promise<WorktreeMetadata>;
  remove: (worktree: WorktreeMetadata, options?: RemoveWorktreeOptions) => Promise<void>;
}

export function useWorktreeProjection({
  config,
  project,
  reloadToken = 0,
  force = false,
}: UseWorktreeProjectionOptions): UseWorktreeProjectionResult {
  const [status, setStatus] = useState<WorktreeProjectionStatus>("idle");
  const [worktrees, setWorktrees] = useState<WorktreeMetadata[]>([]);
  const [lastKnown, setLastKnown] = useState(false);
  const [reason, setReason] = useState<string | null>(null);
  const requestRef = useRef<AbortController | null>(null);
  const worktreesRef = useRef(worktrees);
  worktreesRef.current = worktrees;

  const refresh = useCallback(async () => {
    if (!config || !project || !project.path.trim()) {
      requestRef.current?.abort();
      setStatus("idle");
      setWorktrees([]);
      setLastKnown(false);
      setReason(null);
      return;
    }

    requestRef.current?.abort();
    const controller = new AbortController();
    requestRef.current = controller;
    setStatus("loading");
    setReason(null);
    try {
      const next = await listProjectWorktrees(config, project);
      if (controller.signal.aborted) return;
      setWorktrees(next);
      setLastKnown(false);
      setStatus("ready");
    } catch (error) {
      if (controller.signal.aborted) return;
      const message = error instanceof Error ? error.message : String(error);
      setReason(message || "worktree listing failed");
      if (worktreesRef.current.length > 0) {
        setLastKnown(true);
        setStatus("unavailable");
      } else {
        setLastKnown(false);
        setStatus("error");
      }
    }
  }, [config, project]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: reloadToken & force are external refresh signals.
  useEffect(() => {
    void refresh();
  }, [refresh, reloadToken, force]);

  useEffect(() => () => requestRef.current?.abort(), []);

  const create = useCallback(async (args: CreateWorktreeArgs): Promise<WorktreeMetadata> => {
    if (!config || !project) throw new Error("Worktree project is unavailable");
    const created = await createWorktree(config, project, args);
    await refresh();
    return created;
  }, [config, project, refresh]);

  const remove = useCallback(async (
    worktree: WorktreeMetadata,
    options?: RemoveWorktreeOptions,
  ): Promise<void> => {
    if (!config || !project) throw new Error("Worktree project is unavailable");
    await removeProjectWorktree(config, project, worktree, options);
    await refresh();
  }, [config, project, refresh]);

  return { status, worktrees, lastKnown, reason, refresh, create, remove };
}
