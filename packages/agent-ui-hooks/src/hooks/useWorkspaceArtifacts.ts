// D1 — useWorkspaceArtifacts hook.

import { useCallback, useEffect, useRef, useState } from "react";
import { getWorkspaceArtifacts } from "@rightkit/agent-ui-core/lib/daemon/workspace-files";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";
import type { WorkspaceArtifact } from "@rightkit/agent-ui-core/lib/types/workspace-files";

export type WorkspaceArtifactsStatus = "idle" | "loading" | "ready" | "error";

export interface UseWorkspaceArtifactsOptions {
  config: DaemonConfig | null;
  workspaceRoot: string;
  reloadToken?: number;
}

export interface UseWorkspaceArtifactsResult {
  status: WorkspaceArtifactsStatus;
  artifacts: WorkspaceArtifact[];
  reason: string | null;
  refresh: () => Promise<void>;
}

export function useWorkspaceArtifacts({
  config,
  workspaceRoot,
  reloadToken = 0,
}: UseWorkspaceArtifactsOptions): UseWorkspaceArtifactsResult {
  const [status, setStatus] = useState<WorkspaceArtifactsStatus>("idle");
  const [artifacts, setArtifacts] = useState<WorkspaceArtifact[]>([]);
  const [reason, setReason] = useState<string | null>(null);
  const inFlight = useRef<AbortController | null>(null);

  const refresh = useCallback(async () => {
    void reloadToken;
    if (!config || !workspaceRoot.trim()) {
      setStatus("idle");
      setArtifacts([]);
      return;
    }
    inFlight.current?.abort();
    const controller = new AbortController();
    inFlight.current = controller;
    setStatus("loading");
    setReason(null);
    try {
      const response = await getWorkspaceArtifacts(config, workspaceRoot);
      if (controller.signal.aborted) return;
      setArtifacts(response.artifacts);
      setStatus("ready");
    } catch (error) {
      if (controller.signal.aborted) return;
      setStatus("error");
      setReason((error as Error).message ?? "artifacts failed");
      setArtifacts([]);
    }
  }, [config, workspaceRoot, reloadToken]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return { status, artifacts, reason, refresh };
}
