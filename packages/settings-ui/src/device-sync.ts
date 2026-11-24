// Cross-device sync settings: one toggle over three independent channels (transcripts, keychain credential,
// Pro resolution), each with its own status, and the one-line summary the settings row and a sidebar row share.
// The commands and every word are inputs. Imports only `react`.
import { useCallback, useEffect, useState } from "react";

export type SyncTone = "ok" | "warn" | "muted";

export type TranscriptSyncStatus =
  | { state: "disabled" }
  | { state: "unavailable"; detail?: string | null }
  | { state: "ready" };

/** The part of the credential and Pro-resolution status the summary reads. */
export interface SyncBridgeStatus {
  keychainSyncEnabled: boolean;
  keychainReady: boolean;
  keychainDetail?: string | null;
  proResolutionStatus?: string | null;
  /** Combined legacy flag, when the native side reports one. */
  syncEnabled?: boolean | null;
}

export interface SyncLabels {
  checking: string;
  off: string;
  synced: string;
  partlySynced: string;
  /** Transcript channel unavailable without a detail. */
  unavailable: string;
  /** Credential channel enabled but not ready, without a detail. */
  waitingOnKeychain: string;
}

/** The transcript channel's status word and tone. */
export function syncStatusLabel(status: TranscriptSyncStatus | null, labels: SyncLabels): { text: string; tone: SyncTone } {
  if (!status) return { text: labels.checking, tone: "muted" };
  switch (status.state) {
    case "disabled":
      return { text: labels.off, tone: "muted" };
    case "ready":
      return { text: labels.synced, tone: "ok" };
    case "unavailable":
      return { text: status.detail || labels.unavailable, tone: "warn" };
  }
}

/**
 * One line for users. The worst channel wins, so a single failing channel can never hide behind "Synced"; the
 * per-channel breakdown stays available to the host for diagnosis.
 */
export function summarizeSync(
  enabled: boolean | null,
  transcript: { text: string; tone: SyncTone },
  bridge: SyncBridgeStatus | null,
  labels: SyncLabels,
  blockedNoKeychain = "blocked_no_keychain",
): { text: string; tone: SyncTone } {
  if (enabled === null || !bridge) return { text: labels.checking, tone: "muted" };
  if (enabled === false) return { text: labels.off, tone: "muted" };
  if (transcript.tone === "warn") return transcript;
  if (bridge.keychainSyncEnabled && !bridge.keychainReady) {
    return { text: bridge.keychainDetail || labels.waitingOnKeychain, tone: "warn" };
  }
  if (bridge.proResolutionStatus === blockedNoKeychain) {
    return { text: labels.waitingOnKeychain, tone: "warn" };
  }
  if (transcript.tone === "ok" && bridge.keychainReady) return { text: labels.synced, tone: "ok" };
  return { text: labels.partlySynced, tone: "warn" };
}

export interface DeviceSyncPort<B extends SyncBridgeStatus> {
  transcriptStatus(): Promise<TranscriptSyncStatus>;
  bridgeStatus(): Promise<B>;
  setTranscripts(enabled: boolean): Promise<unknown>;
  setKeychain(enabled: boolean): Promise<unknown>;
  setProResolution(enabled: boolean): Promise<unknown>;
}

export interface UseDeviceSyncOptions {
  /** False off the platforms that have sync: nothing is read. */
  active: boolean;
  /** Names of the channels in the failure text. */
  channelNames: { transcripts: string; keychain: string; proResolution: string };
  /** Runs after a toggle, before the status is re-read (a host event for other screens). */
  onChanged?: () => void;
}

/**
 * The toggle and the two status reads. The channels are separate systems, queried and set independently: a
 * failure on one never hides or blocks the other, and a failed channel does not revert another's persisted
 * state. The toggle reflects transcript consent as the primary signal; the credential and Pro channels are
 * fanned out alongside it but reported in their own status, so a divergence between them stays visible.
 */
export function useDeviceSync<B extends SyncBridgeStatus>(port: DeviceSyncPort<B>, options: UseDeviceSyncOptions) {
  const { active, channelNames, onChanged } = options;
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [transcriptStatus, setTranscriptStatus] = useState<TranscriptSyncStatus | null>(null);
  const [bridgeStatus, setBridgeStatus] = useState<B | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const refresh = useCallback(async () => {
    const [cloudResult, bridgeResult] = await Promise.allSettled([port.transcriptStatus(), port.bridgeStatus()]);
    if (cloudResult.status === "fulfilled") {
      setTranscriptStatus(cloudResult.value);
      setEnabled((prev) => (cloudResult.value.state !== "disabled" ? true : prev ?? false));
    }
    if (bridgeResult.status === "fulfilled") {
      setBridgeStatus(bridgeResult.value);
    }
    if (cloudResult.status === "fulfilled") {
      setEnabled(cloudResult.value.state !== "disabled");
    } else if (bridgeResult.status === "fulfilled") {
      setEnabled(bridgeResult.value.syncEnabled ?? bridgeResult.value.keychainSyncEnabled);
    }
    // The port is the host's module-level binding.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!active) return;
    let alive = true;
    void (async () => {
      try {
        await refresh();
      } catch {
        if (alive) setEnabled(false);
      }
    })();
    return () => {
      alive = false;
    };
  }, [active, refresh]);

  async function toggle(next: boolean) {
    if (busy) return;
    setBusy(true);
    setError("");
    setEnabled(next);
    // Three systems, three commands, three independent results. Every one is attempted even if another
    // rejects, and any failure is reported without reverting the others' already-persisted state.
    const [cloudResult, keychainResult, proResult] = await Promise.allSettled([
      port.setTranscripts(next),
      port.setKeychain(next),
      port.setProResolution(next),
    ]);
    const failures: string[] = [];
    if (cloudResult.status === "rejected") failures.push(`${channelNames.transcripts}: ${String(cloudResult.reason)}`);
    if (keychainResult.status === "rejected") failures.push(`${channelNames.keychain}: ${String(keychainResult.reason)}`);
    if (proResult.status === "rejected") failures.push(`${channelNames.proResolution}: ${String(proResult.reason)}`);
    if (failures.length > 0) setError(failures.join(" · "));
    onChanged?.();
    await refresh();
    setBusy(false);
  }

  return { enabled, transcriptStatus, bridgeStatus, busy, error, setError, toggle, refresh };
}
