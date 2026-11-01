// D1 — useWorkspaceSearch hook.
//
// Cooperative search hook: every distinct query gets a unique request id
// so the daemon's request-id cancellation registry flips the prior
// in-flight scan before the new one starts. Reusing a request id across
// distinct queries is a cancellation-confusion bug — the new walker
// would inherit a flagged flag and exit immediately. Unique ids are
// also how the backend's registry distinguishes "cancel this run" from
// "a fresh run I want to coexist with".

import { useCallback, useEffect, useRef, useState } from "react";
import { postWorkspaceSearch } from "@rightkit/agent-ui-core/lib/daemon/workspace-files";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";
import type { WorkspaceSearchHit, WorkspaceSearchKind } from "@rightkit/agent-ui-core/lib/types/workspace-files";

export type WorkspaceSearchStatus = "idle" | "loading" | "ready" | "cancelled" | "error";

export interface UseWorkspaceSearchOptions {
  config: DaemonConfig | null;
  workspaceRoot: string;
  query: string;
  kind?: WorkspaceSearchKind;
  limit?: number;
  includeBinary?: boolean;
  /**
   * Optional explicit request id — used when a higher-level controller
   * already generated one (e.g. quick-pick reuses). When omitted, a
   * unique id is generated per distinct (workspaceRoot + query + kind)
   * triple.
   */
  requestId?: string;
}

export interface UseWorkspaceSearchResult {
  status: WorkspaceSearchStatus;
  hits: WorkspaceSearchHit[];
  cancelled: boolean;
  truncated: boolean;
  reason: string | null;
  cancel: () => void;
}

function makeRequestId(workspaceRoot: string, query: string, kind: string): string {
  return `ws:${workspaceRoot}:${kind}:${query}:${Date.now()}-${Math.random()
    .toString(36)
    .slice(2, 8)}`;
}

export function useWorkspaceSearch({
  config,
  workspaceRoot,
  query,
  kind = "content",
  limit = 64,
  includeBinary = false,
  requestId,
}: UseWorkspaceSearchOptions): UseWorkspaceSearchResult {
  const [status, setStatus] = useState<WorkspaceSearchStatus>("idle");
  const [hits, setHits] = useState<WorkspaceSearchHit[]>([]);
  const [cancelled, setCancelled] = useState(false);
  const [truncated, setTruncated] = useState(false);
  const [reason, setReason] = useState<string | null>(null);
  const controllerRef = useRef<AbortController | null>(null);

  const cancel = useCallback(() => {
    controllerRef.current?.abort();
    setCancelled(true);
    setStatus("cancelled");
  }, []);

  useEffect(() => {
    if (!config || !workspaceRoot.trim() || !query.trim()) {
      setHits([]);
      setCancelled(false);
      setTruncated(false);
      setReason(null);
      setStatus("idle");
      return;
    }
    controllerRef.current?.abort();
    const controller = new AbortController();
    controllerRef.current = controller;
    const resolvedRequestId = requestId ?? makeRequestId(workspaceRoot, query, kind);
    setStatus("loading");
    setReason(null);
    setCancelled(false);
    setTruncated(false);
    setHits([]);
    void postWorkspaceSearch(
      config,
      {
        workspaceRoot,
        query,
        requestId: resolvedRequestId,
        kind,
        limit,
        includeBinary,
      },
      { signal: controller.signal },
    )
      .then((response) => {
        if (controller.signal.aborted) return;
        setHits(response.hits);
        setCancelled(response.cancelled);
        setTruncated(response.truncated);
        setStatus(response.cancelled ? "cancelled" : "ready");
      })
      .catch((error) => {
        if (controller.signal.aborted) return;
        setStatus("error");
        setReason((error as Error).message ?? "search failed");
        setHits([]);
      });
  }, [config, workspaceRoot, query, kind, limit, includeBinary, requestId]);

  useEffect(
    () => () => {
      controllerRef.current?.abort();
    },
    [],
  );

  return { status, hits, cancelled, truncated, reason, cancel };
}
