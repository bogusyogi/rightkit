// D1 — useWorkspaceTree hook.
//
// Loads a path's children either by initial load (root) or by expansion.
// The hook is keyed on the `(workspaceRoot, path, reloadToken)` tuple so the
// panel can force a refresh after a watcher event or a manual `Retry`.
//
// Last-known contract (D1 fix):
//   When a forced refresh fails, the previous entries stay attached to the
//   path with `lastKnown: true` so the user sees the prior tree plus a
//   retry surface — the tree is NEVER rebuilt from scratch and duplicates
//   are NEVER created. When the refresh succeeds, lastKnown flips back
//   to false and the entries are atomically replaced.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { getWorkspaceTree, postWorkspaceTreeExpand } from "@rightkit/agent-ui-core/lib/daemon/workspace-files";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";
import type { WorkspaceEntry, WorkspaceTreeResponse } from "@rightkit/agent-ui-core/lib/types/workspace-files";
import { useWorkspaceStore } from "@rightkit/agent-ui-core/state/workspaceStore";

export type WorkspaceTreeStatus = "idle" | "loading" | "ready" | "unavailable" | "error";

export interface UseWorkspaceTreeOptions {
  config: DaemonConfig | null;
  workspaceRoot: string;
  path?: string;
  /** Bumped by the caller (e.g. after a watcher rescan) to force a reload. */
  reloadToken?: number;
  /** Force a fresh load even when a cached entry exists. */
  force?: boolean;
}

export interface UseWorkspaceTreeResult {
  status: WorkspaceTreeStatus;
  entries: WorkspaceEntry[];
  truncated: boolean;
  lastKnown: boolean;
  reason: string | null;
  refresh: () => Promise<void>;
}

export function useWorkspaceTree({
  config,
  workspaceRoot,
  path = "",
  reloadToken = 0,
  force = false,
}: UseWorkspaceTreeOptions): UseWorkspaceTreeResult {
  const cached = useWorkspaceStore((state) =>
    state.workspaceRoot === workspaceRoot ? state.expanded[normalize(path)] : undefined,
  );
  const rememberPath = useWorkspaceStore((state) => state.rememberPath);
  const markPathLastKnown = useWorkspaceStore((state) => state.markPathLastKnown);
  const [status, setStatus] = useState<WorkspaceTreeStatus>(cached ? "ready" : "idle");
  const [reason, setReason] = useState<string | null>(null);
  const [entries, setEntries] = useState<WorkspaceEntry[]>(cached?.entries ?? []);
  const [truncated, setTruncated] = useState<boolean>(cached?.truncated ?? false);
  const [lastKnown, setLastKnown] = useState<boolean>(cached?.lastKnown ?? false);
  const inFlight = useRef<AbortController | null>(null);

  const stablePath = useMemo(() => normalize(path), [path]);

  const refresh = useCallback(async () => {
    if (!config || !workspaceRoot.trim()) {
      setStatus("idle");
      setReason(null);
      setEntries([]);
      setLastKnown(false);
      return;
    }
    inFlight.current?.abort();
    const controller = new AbortController();
    inFlight.current = controller;
    setStatus("loading");
    setReason(null);
    try {
      const response: WorkspaceTreeResponse =
        stablePath === ""
          ? await getWorkspaceTree(config, workspaceRoot, stablePath, { signal: controller.signal })
          : await postWorkspaceTreeExpand(config, workspaceRoot, stablePath, {
              signal: controller.signal,
            });
      if (controller.signal.aborted) return;
      rememberPath(stablePath, response.entries, response.truncated, { lastKnown: false });
      setEntries(response.entries);
      setTruncated(response.truncated);
      setLastKnown(false);
      setStatus("ready");
    } catch (error) {
      if (controller.signal.aborted) return;
      const message = (error as Error).message ?? "tree failed";
      // Last-known fallback: keep the prior entries + flip the flag so
      // the user can see the prior tree plus a retry surface, rather
      // than an empty rebuild. Both the store and the local state keep
      // their existing entries; only the status + flag flip.
      if (cached && cached.entries.length > 0) {
        markPathLastKnown(stablePath);
        setLastKnown(true);
        setStatus("unavailable");
        setReason(message);
        return;
      }
      setStatus("error");
      setReason(message);
      setEntries([]);
      setLastKnown(false);
    }
  }, [config, workspaceRoot, stablePath, rememberPath, markPathLastKnown, cached]);

  useEffect(() => {
    if (!config || !workspaceRoot.trim()) {
      setStatus("idle");
      setEntries([]);
      setLastKnown(false);
      return;
    }
    if (cached && !force && cached.loadedAt > 0 && reloadToken === 0) {
      setEntries(cached.entries);
      setTruncated(cached.truncated);
      setLastKnown(cached.lastKnown);
      setStatus(cached.lastKnown ? "unavailable" : "ready");
      return;
    }
    void refresh();
  }, [config, workspaceRoot, force, reloadToken, refresh, cached]);

  useEffect(
    () => () => {
      inFlight.current?.abort();
    },
    [],
  );

  return { status, entries, truncated, lastKnown, reason, refresh };
}

function normalize(path: string): string {
  return path
    .trim()
    .replace(/\\/g, "/")
    .replace(/^\/+|\/+$/g, "");
}
