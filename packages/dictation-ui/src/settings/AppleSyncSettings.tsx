// Apple Sync section (macOS) — the ONE user-facing "Sync with Apple devices"
// control (HR-SYNC-001..013 in docs/DOGFOOD.md). It fans out to THREE
// independent backend systems, each with its own status, failure mode, and
// retry — never one shared flag:
//   1. CloudKit transcripts        — cloud_sync.rs        (this lane owns it)
//   2. Synchronizable iCloud Keychain credentials (provider API keys +
//      opaque Pro bootstrap) — pro_bridge.rs, `keychain_sync_enabled`.
//   3. Backend Pro resolution — pro_bridge.rs, `pro_resolution_enabled`.
//
// (2) and (3) are separate state machines with their own setters
// (`pro_bridge_set_keychain_sync_enabled` /
// `pro_bridge_set_pro_resolution_enabled`) and their own status fields
// (`keychainSyncEnabled`, `proResolutionEnabled`, `proResolutionStatus`),
// matching how iOS has always kept them apart (`AppleSyncedCredentialStore`
// vs `ProBridge`). This component therefore renders THREE independent status
// rows and fans out to THREE independent commands. `pro_bridge_set_sync_enabled`
// survives only as a legacy combined entry point and is deliberately NOT called
// here — routing both channels through it would make these rows cosmetic, which
// is exactly the collapse HR-SYNC-002 forbids.
//
// This toggle is the ONLY writable control for the sync decision.
// LicenseSettings.tsx's former duplicate Pro-continuity checkbox has been
// reduced to a read-only status line pointing here (HR-SYNC-001: one
// user-facing control).
//
// macOS-gated per HR-SYNC-012: Windows must never reach Apple private
// CloudKit or iCloud Keychain, and this component must render nothing there.

import { memo, useCallback, useEffect, useState } from "react";
import { getPlatform } from "@rightkit/platform-ui";
import { CompactSettingRow, CompactSettingsGroup } from "../components/settings/CompactSettingsGroup.js";
import ErrorBanner from "../components/ErrorBanner.js";
import { cx } from "../lib/ui.js";
import { sectionClient, type CloudSyncStatus, type ProBridgeStatus } from "./client.js";
import { SectionToggle as Toggle } from "./Toggle.js";

/** Window event the section dispatches after a change, so a sidebar row that shows the sync status re-reads it now. */
export const CLOUD_SYNC_CHANGED_EVENT = "rk-cloud-sync-changed";

function statusLabel(status: CloudSyncStatus | null): { text: string; tone: "ok" | "warn" | "muted" } {
  if (!status) return { text: "Checking…", tone: "muted" };
  switch (status.state) {
    case "disabled":
      return { text: "Off", tone: "muted" };
    case "ready":
      return { text: "Synced", tone: "ok" };
    case "unavailable":
      return { text: status.detail || "Unavailable right now", tone: "warn" };
  }
}

type Tone = "ok" | "warn" | "muted";

/// One line for users. Worst channel wins so a single failing channel can
/// never hide behind "Synced"; the per-channel breakdown stays available
/// under Developer Mode for diagnosis (HR-SYNC-002 is about visibility of
/// divergence, which this preserves).
function summarize(
  enabled: boolean | null,
  transcript: { text: string; tone: Tone },
  bridge: ProBridgeStatus | null,
): { text: string; tone: Tone } {
  if (enabled === null || !bridge) return { text: "Checking…", tone: "muted" };
  if (enabled === false) return { text: "Off", tone: "muted" };
  if (transcript.tone === "warn") return transcript;
  if (bridge.keychainSyncEnabled && !bridge.keychainReady) {
    return { text: bridge.keychainDetail || "Waiting on iCloud Keychain", tone: "warn" };
  }
  if (bridge.proResolutionStatus === "blocked_no_keychain") {
    return { text: "Waiting on iCloud Keychain", tone: "warn" };
  }
  if (transcript.tone === "ok" && bridge.keychainReady) return { text: "Synced", tone: "ok" };
  return { text: "Partly synced", tone: "warn" };
}

function AppleSyncSettingsImpl({ developerMode = false }: { developerMode?: boolean }) {
  // HR-SYNC-012: this entire section exists on macOS only. Windows has no
  // CloudKit/iCloud Keychain build path to reach, and never should.
  const isMac = getPlatform() === "macos";

  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [transcriptStatus, setTranscriptStatus] = useState<CloudSyncStatus | null>(null);
  const [bridgeStatus, setBridgeStatus] = useState<ProBridgeStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  // Independent status refresh: transcript sync and Keychain/Pro sync are
  // two different backend systems queried separately, on purpose — a
  // failure reading one must never hide or block the other (HR-SYNC-002).
  const refresh = useCallback(async () => {
    const [cloudResult, bridgeResult] = await Promise.allSettled([
      sectionClient("sync").cloudSyncStatus(),
      sectionClient("sync").proBridgeStatus(),
    ]);
    if (cloudResult.status === "fulfilled") {
      setTranscriptStatus(cloudResult.value);
      setEnabled((prev) => (cloudResult.value.state !== "disabled" ? true : prev ?? false));
    }
    if (bridgeResult.status === "fulfilled") {
      setBridgeStatus(bridgeResult.value);
    }
    // The toggle itself reflects transcript-sync consent as the primary
    // signal (it is the one this lane owns end to end); Keychain/Pro
    // consent is fanned out alongside it but reported in its own row so a
    // divergence between the two flags (e.g. from the pre-existing
    // LicenseSettings duplicate — see file header) stays visible instead of
    // being silently papered over.
    if (cloudResult.status === "fulfilled") {
      setEnabled(cloudResult.value.state !== "disabled");
    } else if (bridgeResult.status === "fulfilled") {
      setEnabled(bridgeResult.value.syncEnabled ?? bridgeResult.value.keychainSyncEnabled);
    }
  }, []);

  useEffect(() => {
    if (!isMac) return;
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
  }, [isMac, refresh]);

  async function onToggle(next: boolean) {
    if (busy) return;
    setBusy(true);
    setError("");
    setEnabled(next);
    // Two independent commands, two independent systems — neither call is
    // allowed to gate the other. Both are attempted even if one rejects, and
    // any failure is reported without silently reverting the other one's
    // already-persisted state (HR-SYNC-002: never treat this as one
    // state machine that only fully succeeds or fully fails together).
    // THREE systems, three commands, three independent results — not two.
    // `pro_bridge_set_sync_enabled` is the LEGACY COMBINED setter and is
    // deliberately not used here: driving both the Keychain-credential channel
    // and the backend Pro-resolution channel through one call would make the
    // separate status rows below cosmetic, which is exactly the collapse
    // HR-SYNC-002 forbids. `pro_bridge.rs` now exposes
    // `pro_bridge_set_keychain_sync_enabled` and
    // `pro_bridge_set_pro_resolution_enabled` as independent state machines;
    // this is the seam that connects the split backend to the split UI.
    const [cloudResult, keychainResult, proResult] = await Promise.allSettled([
      sectionClient("sync").setCloudSyncEnabled(next),
      sectionClient("sync").setKeychainSyncEnabled(next),
      sectionClient("sync").setProResolutionEnabled(next),
    ]);
    const failures: string[] = [];
    if (cloudResult.status === "rejected") failures.push(`Transcripts: ${String(cloudResult.reason)}`);
    if (keychainResult.status === "rejected") failures.push(`Apple Keychain: ${String(keychainResult.reason)}`);
    if (proResult.status === "rejected") failures.push(`Pro resolution: ${String(proResult.reason)}`);
    if (failures.length > 0) setError(failures.join(" · "));

    // The sidebar's Apple sync row reads the same status; tell it to re-read now, not on the next launch.
    window.dispatchEvent(new Event(CLOUD_SYNC_CHANGED_EVENT));
    await refresh();
    setBusy(false);
  }

  if (!isMac) return null;

  const transcript = statusLabel(transcriptStatus);
  const summary = summarize(enabled, transcript, bridgeStatus);
  const toneClass = (tone: "ok" | "warn" | "muted") =>
    tone === "ok"
      ? "text-[var(--ok)]"
      : tone === "warn"
        ? "text-[var(--warn)]"
        : "text-[var(--muted)]";

  return (
    <CompactSettingsGroup title="Apple Sync" id="settings-apple-sync">
      <ErrorBanner message={error} onDismiss={() => setError("")} />
      <CompactSettingRow
        label="Sync with Apple devices"
        sublabel="Transcripts and saved API keys use your private iCloud. Audio never uploads. Turning this off stops only this device and never deletes iCloud data."
        settingId="settings.sync-with-apple-devices"
      >
        <span className={cx("text-[length:var(--size-sm)]", toneClass(summary.tone))}>
          {summary.text}
        </span>
        <Toggle
          checked={enabled === true}
          disabled={enabled === null || busy}
          ariaLabel="Sync with Apple devices"
          onChange={(next) => void onToggle(next)}
        />
      </CompactSettingRow>

      {/* Three independently-sourced readouts: HR-SYNC-002 requires the UI
          be able to show one succeeded while another failed, so these are
          never merged into one combined "sync status" line. Laid out as a
          strip, not three full-width rows. Users get the one-line summary
          above; this breakdown is a diagnostic, shown under Developer Mode. */}
      {developerMode && (
      <div className="grid divide-[color-mix(in_srgb,var(--line),transparent_22%)] max-[720px]:divide-y min-[721px]:grid-cols-3 min-[721px]:divide-x">
        <div className="flex min-w-0 flex-col gap-[2px] px-[var(--space-4)] py-[var(--space-3)]">
          <span className="text-[length:var(--size-xs)] uppercase tracking-[0.08em] text-[var(--muted)]">Transcripts</span>
          <span className={cx("text-[length:var(--size-sm)]", toneClass(transcript.tone))}>
            {transcript.text}
          </span>
        </div>
        <div className="flex min-w-0 flex-col gap-[2px] px-[var(--space-4)] py-[var(--space-3)]">
          <span className="text-[length:var(--size-xs)] uppercase tracking-[0.08em] text-[var(--muted)]">Apple Keychain</span>
          <span className={cx("text-[length:var(--size-sm)]", toneClass(
              !bridgeStatus
                ? "muted"
                : bridgeStatus.keychainReady
                  ? "ok"
                  : bridgeStatus.keychainSyncEnabled
                    ? "warn"
                    : "muted",
            ))}>
            {!bridgeStatus
            ? "Checking…"
            : !bridgeStatus.keychainSyncEnabled
              ? "Off"
              : bridgeStatus.keychainReady
                ? "Synced"
                : bridgeStatus.keychainDetail || "Unavailable right now"}
          </span>
        </div>
        <div className="flex min-w-0 flex-col gap-[2px] px-[var(--space-4)] py-[var(--space-3)]">
          <span className="text-[length:var(--size-xs)] uppercase tracking-[0.08em] text-[var(--muted)]">Pro on your devices</span>
          <span className={cx("text-[length:var(--size-sm)]", toneClass(
              !bridgeStatus
                ? "muted"
                : bridgeStatus.proResolutionStatus === "ok"
                  ? "ok"
                  : bridgeStatus.proResolutionStatus === "blocked_no_keychain"
                    ? "warn"
                    : "muted",
            ))}>
            {!bridgeStatus
            ? "Checking…"
            : bridgeStatus.proResolutionStatus === "ok"
              ? "On"
              : bridgeStatus.proResolutionStatus === "blocked_no_keychain"
                ? "Waiting on Apple Keychain sync"
                : "Off"}
          </span>
        </div>
      </div>
      )}
      {bridgeStatus?.consumptionSuppressed && (
        <div className="px-[var(--space-4)] py-[var(--space-3)]">
          <span className="block max-w-[560px] text-[length:var(--size-xs)] leading-[1.45] text-[var(--muted)]">
            Pro auto-activation is suppressed on this device since a license was last removed here.
            Turning sync back on clears this.
          </span>
        </div>
      )}

    </CompactSettingsGroup>
  );
}

export const AppleSyncSettings = memo(AppleSyncSettingsImpl);
