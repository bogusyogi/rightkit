import { useEffect, useState } from "react";
import { Button } from "@rightkit/react-shell/react";
import type { LibraryRecovery, LibraryRecoveryStatus } from "../contract.js";
import { readStored, writeStored } from "../host.js";

/**
 * Shown above the list when the source had to set its store aside (kept, never deleted). The
 * dismissed recovery's `createdAtMs` is remembered, so a newer recovery shows again.
 */
export function RecoveryBanner({ recovery }: { recovery: LibraryRecovery }) {
  const [status, setStatus] = useState<LibraryRecoveryStatus | null>(null);
  const [dismissed, setDismissed] = useState<string | null>(() => readStored("recoveryDismissed"));
  const [message, setMessage] = useState("");

  useEffect(() => {
    void recovery.status().then(setStatus).catch(() => undefined);
  }, [recovery]);

  async function exportDiagnostics() {
    try {
      if (await recovery.exportDiagnostics()) setMessage("Diagnostics exported.");
    } catch (e) {
      setMessage(String(e));
    }
  }

  if (!status || dismissed === String(status.createdAtMs)) return null;

  return (
    <aside
      role="status"
      className="mb-[var(--space-3)] flex items-center gap-4 rounded-[var(--radius-md)] border border-[var(--line)] bg-[var(--surface-2)] px-4 py-3 text-[length:var(--size-sm)] text-[var(--paper)]"
    >
      <div className="min-w-0 flex-1">
        <strong>{status.title}</strong> {status.message}
        {message ? <span> {message}</span> : null}
      </div>
      <Button size="sm" onClick={() => void exportDiagnostics()}>
        Export diagnostics
      </Button>
      <Button
        size="sm"
        onClick={() => {
          writeStored("recoveryDismissed", String(status.createdAtMs));
          setDismissed(String(status.createdAtMs));
        }}
      >
        Dismiss
      </Button>
    </aside>
  );
}
