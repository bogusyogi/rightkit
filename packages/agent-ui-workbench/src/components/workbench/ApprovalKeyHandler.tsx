import { useEffect } from "react";
import type { PendingApproval } from "@rightkit/agent-ui-hooks/hooks/useApprovals";
import { registerShortcut } from "@rightkit/agent-ui-core/lib/shortcuts/dispatcher";

export default function ApprovalKeyHandler({
  pendingApproval,
  onApprove,
  onDeny,
}: {
  pendingApproval: PendingApproval | null;
  onApprove: () => void;
  onDeny: () => void;
}) {
  useEffect(() => {
    if (!pendingApproval) return;
    // Same matching as the retired window listener: the bare key fires while typing, extra modifiers are
    // ignored (Shift gives "Y"/"N"), and a binding another handler already claimed still fires.
    const keys = { hidden: true, ignoreExtraModifiers: true, allowDefaultPrevented: true } as const;
    const unregister = [
      registerShortcut({
        id: "approval.approve",
        chord: "y",
        label: "Approve",
        ...keys,
        run: () => {
          onApprove();
          return true;
        },
      }),
      registerShortcut({
        id: "approval.deny",
        chord: "n",
        label: "Deny",
        ...keys,
        run: () => {
          onDeny();
          return true;
        },
      }),
      registerShortcut({
        id: "approval.deny.esc",
        chord: "esc",
        label: "Deny",
        ...keys,
        run: () => {
          onDeny();
          return true;
        },
      }),
    ];
    return () => {
      for (const remove of unregister) remove();
    };
  }, [pendingApproval, onApprove, onDeny]);
  return null;
}
