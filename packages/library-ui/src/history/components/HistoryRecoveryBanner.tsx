import { useEffect, useState } from "react";
import { invoke } from "@rightkit/dictation-ui/lib/tauri";
import { cx, ui } from "@rightkit/dictation-ui/lib/ui";
import { hostAppName } from "../../appName";

type HistoryRecoveryStatus = {
  reason: string;
  created_at_ms: number;
  state: string;
  backups: { path: string; bytes: number | null }[];
};

// Non-secret UI acknowledgement: the created_at_ms of the recovery the user dismissed, so a newer recovery shows again.
const DISMISSED_KEY = "hr.historyRecoveryDismissed.v1";

function dismissedRecovery(): string | null {
  try {
    return localStorage.getItem(DISMISSED_KEY);
  } catch {
    return null;
  }
}

function rememberDismissedRecovery(createdAtMs: number): void {
  try {
    localStorage.setItem(DISMISSED_KEY, String(createdAtMs));
  } catch {
    /* An unavailable browser store only means the banner may appear again. */
  }
}

/** Shown on the History page when the history database could not be opened
 *  with its key and was set aside (never deleted) as an encrypted backup. */
export default function HistoryRecoveryBanner() {
  const [status, setStatus] = useState<HistoryRecoveryStatus | null>(null);
  const [dismissed, setDismissed] = useState<string | null>(dismissedRecovery);
  const [message, setMessage] = useState("");

  useEffect(() => {
    void invoke<HistoryRecoveryStatus | null>("get_history_recovery_status")
      .then(setStatus)
      .catch(() => undefined);
  }, []);

  async function exportDiagnostics() {
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const path = await save({
        defaultPath: "heardright-history-recovery.txt",
        filters: [{ name: "Plain text", extensions: ["txt"] }],
      });
      if (!path) return;
      await invoke("export_history_recovery_diagnostics", { path });
      setMessage("Diagnostics exported.");
    } catch (e) {
      setMessage(String(e));
    }
  }

  if (!status || status.backups.length === 0 || dismissed === String(status.created_at_ms))
    return null;

  return (
    <aside
      role="status"
      className="mb-[var(--space-3)] flex items-center gap-4 rounded-[var(--radius-md)] border border-[var(--line)] bg-[var(--surface-2)] px-4 py-3 text-[length:var(--size-sm)] text-[var(--paper)]"
    >
      <div className="min-w-0 flex-1">
        <strong>Earlier history was kept as an encrypted backup.</strong> {hostAppName()} could not
        unlock the previous history database, so it started a fresh one. The old file was not
        deleted.
        {message ? <span> {message}</span> : null}
      </div>
      <button
        type="button"
        className={cx(ui.button, ui.buttonCompact)}
        onClick={() => void exportDiagnostics()}
      >
        Export diagnostics
      </button>
      <button
        type="button"
        className={cx(ui.button, ui.buttonCompact)}
        onClick={() => {
          rememberDismissedRecovery(status.created_at_ms);
          setDismissed(String(status.created_at_ms));
        }}
      >
        Dismiss
      </button>
    </aside>
  );
}
