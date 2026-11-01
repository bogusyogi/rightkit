import { useCallback, useEffect, useState } from "react";

import {
  getProBridgeStatus,
  reasonOf,
  setProBridgeSyncEnabled,
  type ProBridgeStatus,
} from "@rightkit/agent-ui-core/state/proBridge";

type Tone = "ok" | "warn" | "muted";

/** One line for users. The keychain and Pro-resolution channels stay separate
 *  in the status payload; the line reports the worst of them so one failing
 *  channel never hides behind "On". */
export function summarizeAppleSync(status: ProBridgeStatus): { text: string; tone: Tone } {
  const enabled = status.keychainSyncEnabled || status.proResolutionEnabled;
  if (!enabled) return { text: "Off", tone: "muted" };
  if (status.keychainSyncEnabled && !status.keychainReady) {
    return { text: status.keychainDetail || "Waiting on iCloud Keychain", tone: "warn" };
  }
  if (status.proResolutionStatus === "blocked_no_keychain") {
    return { text: "Waiting on iCloud Keychain", tone: "warn" };
  }
  if (status.keychainSyncEnabled && status.proResolutionEnabled) return { text: "On", tone: "ok" };
  return { text: "Partly on", tone: "warn" };
}

const TONE_CLASS: Record<Tone, string> = {
  ok: "text-accent-2",
  warn: "text-warning",
  muted: "text-text-3",
};

/** Consent toggle and status for Apple device Pro continuity. Renders nothing
 *  where iCloud Keychain does not exist (the native command reports
 *  `supported: false` off macOS). */
export default function AppleSyncPanel() {
  const [status, setStatus] = useState<ProBridgeStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setStatus(await getProBridgeStatus());
    } catch {
      setStatus(null);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  if (!status || !status.supported) return null;

  const summary = summarizeAppleSync(status);
  const checked = status.keychainSyncEnabled && status.proResolutionEnabled;

  async function onToggle(next: boolean) {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await setProBridgeSyncEnabled(next);
    } catch (err) {
      setError(reasonOf(err, "Could not change Apple device sync"));
    } finally {
      await refresh();
      setBusy(false);
    }
  }

  return (
    <div className="rounded-lg border border-line bg-panel p-4" data-testid="apple-sync-panel">
      <div className="flex items-center justify-between gap-3 flex-wrap">
        <div className="max-w-xl">
          <h3 className="text-[14px] font-semibold text-text">Apple devices</h3>
          <p className="text-[12px] text-text-3">
            Use Pro on your other Apple devices through your private iCloud Keychain. Only an opaque
            sign-in token is shared, never your license key. Turning this off stops this device only
            and never deletes iCloud data.
          </p>
        </div>
        <label className="inline-flex items-center gap-2 text-[13px] text-text-2">
          <span className={TONE_CLASS[summary.tone]}>{summary.text}</span>
          <input
            type="checkbox"
            aria-label="Sync Pro across Apple devices"
            checked={checked}
            disabled={busy}
            onChange={(event) => void onToggle(event.target.checked)}
          />
        </label>
      </div>
      {status.consumptionSuppressed ? (
        <p className="mt-3 text-[12px] text-text-3">
          Pro sign-in is paused on this device because the license was removed here. Turning sync
          back on clears this.
        </p>
      ) : null}
      {error ? <div className="mt-3 text-[12px] text-danger">{error}</div> : null}
    </div>
  );
}
