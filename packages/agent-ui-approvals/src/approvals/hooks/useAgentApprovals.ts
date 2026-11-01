import { useCallback, useEffect, useReducer, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  AGENT_APPROVAL_EVENT,
  agentApprovalsPending,
} from "@rightkit/mail-ui/lib/ipc/agentApprovals";
import { emptyQueue, isExpired, queueReducer } from "../lib/approvalQueue";

/**
 * The queue of send approvals waiting for the user. The list always comes from
 * the app (on unlock, on the event, on focus, when one expires); the event
 * payload is never used as content. While locked the queue is empty.
 */
export function useAgentApprovals(unlocked: boolean) {
  const [queue, dispatch] = useReducer(queueReducer, emptyQueue);
  const [now, setNow] = useState(() => Date.now());

  const refresh = useCallback(async () => {
    try {
      dispatch({ type: "replace", items: await agentApprovalsPending() });
    } catch {
      // Locked, or the app is busy: keep what is shown; the next refresh retries.
    }
  }, []);

  useEffect(() => {
    if (!unlocked) {
      dispatch({ type: "replace", items: [] });
      return;
    }
    void refresh();
    const uns: Array<() => void> = [];
    let closed = false;
    const keep = (u: () => void) => (closed ? u() : uns.push(u));
    listen(AGENT_APPROVAL_EVENT, () => void refresh())
      .then(keep)
      .catch(() => {});
    const onFocus = () => void refresh();
    window.addEventListener("focus", onFocus);
    return () => {
      closed = true;
      window.removeEventListener("focus", onFocus);
      uns.forEach((u) => u());
    };
  }, [unlocked, refresh]);

  const waiting = queue.items.length > 0;
  useEffect(() => {
    if (!waiting) return;
    setNow(Date.now());
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, [waiting]);

  const expired = queue.items.some((a) => isExpired(a.expires_at, now));
  useEffect(() => {
    if (!expired) return;
    dispatch({ type: "expire", now });
    void refresh();
  }, [expired, now, refresh]);

  return { queue: queue.items, now, refresh, dispatch };
}
