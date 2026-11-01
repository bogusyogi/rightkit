import { useCallback, useState } from "react";
import { resolveToolApproval, stageDiffHunk } from "@rightkit/agent-ui-core/lib/daemon";
import {
  buildStageDiffHunkPayload,
  type DiffHunk,
  type DiffHunkDecision,
} from "../components/workbench/DiffView";
import type { DaemonConfig, MiddlewareDecisionEvidence, ProtocolEvent } from "@rightkit/agent-ui-core/lib/types";

export interface PendingApproval {
  id: string;
  name: string;
  detail: string;
  policySource?: string;
  riskLabel?: string;
  middlewareDecisions: MiddlewareDecisionEvidence[];
}

interface UseApprovalsOptions {
  config: DaemonConfig | null;
  sessionId: string | null;
  formatError: (error: unknown, fallback: string) => string;
  onStatus: (status: string) => void;
  onWorking: (working: boolean) => void;
}

export function useApprovals({
  config,
  sessionId,
  formatError,
  onStatus,
  onWorking,
}: UseApprovalsOptions) {
  const [pendingApproval, setPendingApproval] = useState<PendingApproval | null>(null);

  const resetApproval = useCallback(() => {
    setPendingApproval(null);
  }, []);

  const applyApprovalEvent = useCallback(
    (event: ProtocolEvent) => {
      if (event.$kind !== "toolApprovalRequested") return;
      setPendingApproval({
        id: event.call.id,
        name: event.call.name,
        detail: event.proposedChange ?? event.call.argumentsJson,
        policySource: event.policySource,
        riskLabel: event.riskLabel,
        middlewareDecisions: event.middlewareDecisions,
      });
      onStatus(`Approval needed: ${event.call.name}`);
      onWorking(false);
    },
    [onStatus, onWorking],
  );

  const decideApproval = useCallback(
    async (approved: boolean) => {
      if (!config || !sessionId || !pendingApproval) return;
      try {
        await resolveToolApproval(
          config,
          sessionId,
          pendingApproval.id,
          approved,
          approved ? "approved in UI" : "denied in UI",
        );
        setPendingApproval(null);
        onWorking(true);
        onStatus(approved ? "Approved" : "Denied");
      } catch (err) {
        onStatus(formatError(err, "Approval failed"));
      }
    },
    [config, formatError, onStatus, onWorking, pendingApproval, sessionId],
  );

  const stageApprovalHunk = useCallback(
    async (hunk: DiffHunk, decision: DiffHunkDecision) => {
      if (!config || !sessionId || !pendingApproval) return;
      const payload = buildStageDiffHunkPayload(pendingApproval.id, hunk, decision);
      try {
        await stageDiffHunk(config, sessionId, payload);
        onStatus(
          decision === "approve"
            ? `Approved hunk ${hunk.index + 1}`
            : `Rejected hunk ${hunk.index + 1}`,
        );
      } catch (err) {
        onStatus(formatError(err, "Hunk decision failed"));
        throw err;
      }
    },
    [config, formatError, onStatus, pendingApproval, sessionId],
  );

  return {
    pendingApproval,
    applyApprovalEvent,
    decideApproval,
    stageApprovalHunk,
    resetApproval,
  };
}
