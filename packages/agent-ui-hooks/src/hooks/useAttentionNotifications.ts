import { useEffect, useRef } from "react";

import { buildSessionDeepLink, notifyOs, setBadgeCount } from "@rightkit/agent-ui-core/lib/notifications";
import { useAttentionStore } from "@rightkit/agent-ui-core/state/attentionStore";
import type { PendingApproval } from "./useApprovals.js";
import type { GoalSnapshotV1 } from "@rightkit/agent-ui-core/lib/types";

interface UseAttentionNotificationsOptions {
  pendingApproval: PendingApproval | null;
  activeGoal: GoalSnapshotV1 | null;
  selectedSessionId: string | null;
  // When a protocol event signals a run failure outside the attention store,
  // caller can bump this counter to force a notification.
  runFailedSeq?: number;
}

export function useAttentionNotifications({
  pendingApproval,
  activeGoal,
  selectedSessionId,
  runFailedSeq,
}: UseAttentionNotificationsOptions): void {
  const prevApprovalId = useRef<string | null>(null);
  const prevGoalBlocked = useRef(false);
  const prevFailedCount = useRef(0);
  const prevNeedsInputCount = useRef(0);
  const prevRunFailedSeq = useRef<number | undefined>(undefined);

  // Badge: needsInput + failed actionable count from attentionStore.
  useEffect(() => {
    const unsub = useAttentionStore.subscribe((state) => {
      const counts = state.list?.counts;
      const badge = (counts?.needsInput ?? 0) + (counts?.failed ?? 0);
      setBadgeCount(badge);

      // Emit OS notification when actionable counts grow (debounced by store).
      const failed = counts?.failed ?? 0;
      const needsInput = counts?.needsInput ?? 0;
      const grew = failed > prevFailedCount.current || needsInput > prevNeedsInputCount.current;
      if (grew && (failed > 0 || needsInput > 0)) {
        const sessionId = selectedSessionId ?? state.banner?.deeplink?.params?.sessionId;
        const link =
          typeof sessionId === "string" && sessionId ? buildSessionDeepLink(sessionId) : undefined;
        const title = failed > prevFailedCount.current ? "Run failed" : "Approval needed";
        const body =
          failed > prevFailedCount.current
            ? `${failed} item${failed === 1 ? "" : "s"} need attention`
            : `${needsInput} item${needsInput === 1 ? "" : "s"} need input`;
        void notifyOs(title, body, link);
      }
      prevFailedCount.current = failed;
      prevNeedsInputCount.current = needsInput;
    });
    // Seed badge from current state
    const counts = useAttentionStore.getState().list?.counts;
    setBadgeCount((counts?.needsInput ?? 0) + (counts?.failed ?? 0));
    return () => unsub();
  }, [selectedSessionId]);

  // Approval needed — immediate OS notification (works even before attention sync)
  useEffect(() => {
    if (!pendingApproval) {
      prevApprovalId.current = null;
      return;
    }
    if (prevApprovalId.current === pendingApproval.id) return;
    prevApprovalId.current = pendingApproval.id;
    const link = selectedSessionId ? buildSessionDeepLink(selectedSessionId) : undefined;
    void notifyOs("Approval needed", pendingApproval.name, link);
    const counts = useAttentionStore.getState().list?.counts;
    setBadgeCount((counts?.needsInput ?? 0) + (counts?.failed ?? 0) + 1);
  }, [pendingApproval, selectedSessionId]);

  // Goal blocked
  useEffect(() => {
    const blocked = activeGoal?.status === "blocked";
    if (blocked && !prevGoalBlocked.current) {
      const link = selectedSessionId ? buildSessionDeepLink(selectedSessionId) : undefined;
      void notifyOs("Goal blocked", activeGoal?.objective ?? "A goal needs attention", link);
    }
    prevGoalBlocked.current = blocked;
  }, [activeGoal, selectedSessionId]);

  // Explicit run-failed sequence (callers that detect failure without attention item)
  useEffect(() => {
    if (runFailedSeq === undefined) return;
    if (prevRunFailedSeq.current === runFailedSeq) return;
    if (prevRunFailedSeq.current === undefined) {
      prevRunFailedSeq.current = runFailedSeq;
      return;
    }
    prevRunFailedSeq.current = runFailedSeq;
    const link = selectedSessionId ? buildSessionDeepLink(selectedSessionId) : undefined;
    void notifyOs("Run failed", "A run failed and needs attention", link);
  }, [runFailedSeq, selectedSessionId]);
}
