import { useReducer, useState } from "react";
import { deriveUpdateOverlay, createUpdateConsent } from "@rightkit/app-update";
import { UpdateConsentCard, updateOverlayText } from "@rightkit/app-update/react";
import { licenseSection } from "@rightkit/settings-ui/standard";
import { useUpdateStatus } from "@rightkit/agent-ui-hooks/hooks/useUpdateStatus";
import { isLicensed, licenseClient, type LicenseView } from "@rightkit/agent-ui-core/state/license";
import { notifyProBridgeAfterActivation, suppressProBridgeConsumption } from "@rightkit/agent-ui-core/state/proBridge";
import type { UpdateState } from "@rightkit/agent-ui-core/state/updates";
import AppleSyncPanel from "./AppleSyncPanel";
import LinkComputerPanel from "./LinkComputerPanel";

/**
 * The licence section (status, activate, deactivate, verify) is `@rightkit/settings-ui`'s
 * `licenseSection` over the licence client (`state/license`); `SettingsModal` renders it above this
 * tab. What stays here is the host app own: the Apple link and sync panels, the update controls with
 * the install-consent card, and the unlocked-feature list. A change the section makes reaches the
 * app through the owner's change event (`useLicenseState` restarts the daemon for it).
 */
export const licenseTabSection: ReturnType<typeof licenseSection> = licenseSection({
  client: licenseClient,
  // Suppress bridge consumption BEFORE the seat is freed, so a launch cannot instantly
  // re-activate this device from the shared bridge.
  beforeDeactivate: () => suppressProBridgeConsumption(true),
  // An explicit activation clears bridge suppression and, if the user opted in, publishes this
  // device's activation for their other Apple devices. Best effort: never affects the licence.
  onChange: (view) => {
    if (isLicensed(view)) void notifyProBridgeAfterActivation().catch(() => undefined);
  },
  labels: {
    sectionLabel: "License",
    statusLabel: "License",
    searchStatus: "License status",
    searchActivate: "Activate license key",
    keyTitle: "License key",
    keyHint: "Enter a purchase key.",
    keyAria: "License key",
    keyPlaceholder: "License key",
    freeDetail: "Free tier. No license key is active on this device.",
  },
  ids: { licenseKey: "settings-license-key" },
});

export default function LicenseTab({
  license,
  actionPending,
  latestVersion,
  updateState,
  error,
  onCheckForUpdate,
  onInstallUpdate,
}: {
  license: LicenseView | null;
  actionPending: "check" | "install" | null;
  latestVersion: string;
  updateState: UpdateState;
  error: string | null;
  onCheckForUpdate: () => void;
  onInstallUpdate: () => void | Promise<void>;
}) {
  const active = isLicensed(license);
  const updateModel = deriveUpdateOverlay(useUpdateStatus());
  const updateLabel = updateOverlayText(updateModel).title || "Idle";
  // Install needs its own approval through the published consent controller; the card shows only while pending or failed.
  const [consent] = useState(() => createUpdateConsent());
  const [, rerender] = useReducer((n: number) => n + 1, 0);
  const requestInstall = () => {
    consent.request("install");
    rerender();
  };
  const allowInstall = async () => {
    await consent.allowOnce(() => onInstallUpdate());
    rerender();
  };
  const denyInstall = () => {
    consent.deny();
    rerender();
  };
  const features = license?.features ?? [];
  const busy = actionPending !== null;
  return (
    <div className="flex flex-col gap-4 p-6 max-w-5xl">
      <LinkComputerPanel active={active} />
      <AppleSyncPanel />

      <div className="rounded-lg border border-line bg-panel p-4">
        <div className="flex items-center justify-between gap-3 flex-wrap">
          <div>
            <h3 className="text-[14px] font-semibold text-text">Updates</h3>
            <p className="text-[12px] text-text-3">
              {latestVersion
                ? `Latest found: ${latestVersion}`
                : "Repair patches are free; feature updates require Pro."}
            </p>
            <p className="text-[12px] text-text-3">{updateLabel}</p>
          </div>
          <div className="flex items-center gap-2">
            <button
              type="button"
              className="inline-flex items-center h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel-2 hover:text-text disabled:opacity-40"
              disabled={busy}
              onClick={onCheckForUpdate}
            >
              {actionPending === "check" ? "Checking" : "Check"}
            </button>
            <button
              type="button"
              className="inline-flex items-center h-8 px-3 rounded-lg border-0 bg-accent text-on-accent font-semibold text-[13px] cursor-pointer disabled:border disabled:border-line disabled:bg-panel-2 disabled:text-text-3 hover:opacity-90"
              disabled={
                busy || (updateState !== "patch-available" && updateState !== "update-available")
              }
              onClick={requestInstall}
            >
              {actionPending === "install" ? "Installing" : "Install"}
            </button>
          </div>
        </div>
        {error ? <div className="mt-3 text-[12px] text-danger">{error}</div> : null}
        <UpdateConsentCard
          state={consent.state()}
          version={latestVersion || undefined}
          onAllow={() => void allowInstall()}
          onDeny={denyInstall}
        />
      </div>

      <div className="rounded-lg border border-line bg-panel p-4">
        <h3 className="text-[14px] font-semibold text-text">Features</h3>
        <div className="mt-3 flex flex-wrap gap-2">
          {features.length > 0 ? (
            features.map((feature) => (
              <span
                className="inline-flex items-center h-7 px-2 rounded-md border border-line bg-bg text-[12px] text-text-2"
                key={feature}
              >
                {feature}
              </span>
            ))
          ) : (
            <span className="text-[12px] text-text-3">Free tier features only</span>
          )}
        </div>
      </div>
    </div>
  );
}
