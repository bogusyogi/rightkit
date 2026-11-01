import { ConfirmDialog } from "@rightkit/react-shell/react";
import { useCallback, useRef, useState } from "react";

export interface ConfirmationRequest {
  title: string;
  description: string;
  confirmLabel: string;
  danger?: boolean;
}

/** Promise-based confirmation for action hooks, rendered by the app shell. */
export function useConfirmationDialog() {
  const [request, setRequest] = useState<ConfirmationRequest | null>(null);
  const resolveRef = useRef<((accepted: boolean) => void) | null>(null);

  const confirm = useCallback((nextRequest: ConfirmationRequest) => {
    return new Promise<boolean>((resolve) => {
      resolveRef.current?.(false);
      resolveRef.current = resolve;
      setRequest(nextRequest);
    });
  }, []);

  const finish = useCallback((accepted: boolean) => {
    resolveRef.current?.(accepted);
    resolveRef.current = null;
    setRequest(null);
  }, []);

  return {
    confirm,
    confirmationDialog: request ? (
      <ConfirmDialog
        title={request.title}
        description={request.description}
        confirmLabel={request.confirmLabel}
        danger={request.danger ?? false}
        onCancel={() => finish(false)}
        onConfirm={() => finish(true)}
      />
    ) : null,
  };
}
