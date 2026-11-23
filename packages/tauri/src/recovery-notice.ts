import { useEffect, useState } from 'react';

/** The part of a store-recovery status the notice reads. */
export interface RecoveryNoticeStatus {
  created_at_ms: number;
  backups: readonly unknown[];
}

export interface UseRecoveryNoticeOptions<Status extends RecoveryNoticeStatus> {
  /** Read the recovery status (`null` = no recovery recorded). Called once per mount. */
  read: () => Promise<Status | null>;
  /** Write the diagnostics text to `path` (native side). */
  exportDiagnostics: (path: string) => Promise<unknown>;
  /** Ask the user where to save; `null` = cancelled. Typically `pickSavePath` of `@rightkit/tauri/dialog`. */
  pickSavePath: (options: {
    defaultPath: string;
    filters: { name: string; extensions: string[] }[];
  }) => Promise<string | null>;
  /** Browser storage key holding the `created_at_ms` of the recovery the user dismissed. */
  dismissedKey: string;
  /** Suggested file name for the exported diagnostics. */
  exportFileName: string;
  /** Save dialog filter label. Defaults to 'Plain text'. */
  exportFilterName?: string;
  /** Shown after a successful export. */
  exportedMessage: string;
}

export interface RecoveryNotice<Status extends RecoveryNoticeStatus> {
  /** The status, or `null` until read or when none exists. */
  status: Status | null;
  /** True when a recovery with at least one preserved backup exists and was not dismissed. */
  visible: boolean;
  /** Result line of the last export (success text or the error text). */
  message: string;
  exportDiagnostics: () => Promise<void>;
  /** Hide this recovery; a NEWER one (different `created_at_ms`) shows again. */
  dismiss: () => void;
}

function dismissedValue(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function rememberDismissed(key: string, createdAtMs: number): void {
  try {
    localStorage.setItem(key, String(createdAtMs));
  } catch {
    /* An unavailable browser store only means the notice may appear again. */
  }
}

/**
 * State and actions of a "your earlier data was kept as an encrypted backup" notice: the status
 * read, the per-recovery dismissal (a non-secret UI acknowledgement kept in browser storage) and the
 * diagnostics export through a save dialog. The wording and markup stay in the app.
 */
export function useRecoveryNotice<Status extends RecoveryNoticeStatus>(
  options: UseRecoveryNoticeOptions<Status>,
): RecoveryNotice<Status> {
  const { read, dismissedKey } = options;
  const [status, setStatus] = useState<Status | null>(null);
  const [dismissed, setDismissed] = useState<string | null>(() => dismissedValue(dismissedKey));
  const [message, setMessage] = useState('');

  useEffect(() => {
    void read()
      .then(setStatus)
      .catch(() => undefined);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- read once per mount, as before
  }, []);

  async function exportDiagnostics() {
    try {
      const path = await options.pickSavePath({
        defaultPath: options.exportFileName,
        filters: [{ name: options.exportFilterName ?? 'Plain text', extensions: ['txt'] }],
      });
      if (!path) return;
      await options.exportDiagnostics(path);
      setMessage(options.exportedMessage);
    } catch (e) {
      setMessage(String(e));
    }
  }

  const visible = !!status && status.backups.length > 0 && dismissed !== String(status.created_at_ms);

  function dismiss() {
    if (!status) return;
    rememberDismissed(dismissedKey, status.created_at_ms);
    setDismissed(String(status.created_at_ms));
  }

  return { status, visible, message, exportDiagnostics, dismiss };
}
