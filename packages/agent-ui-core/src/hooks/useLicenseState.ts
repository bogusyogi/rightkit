import { type MutableRefObject, useCallback, useEffect, useRef, useState } from "react";

import { restartDaemon } from "../lib/daemon.js";
import type { DaemonConfig } from "../lib/types.js";
import {
  entitlementSignature,
  isLicensed,
  licenseClient,
  licenseErrorMessage,
  type LicenseView,
} from "../state/license.js";
import {
  checkForUpdate,
  downloadPendingUpdate,
  installPendingUpdate,
  type UpdateState,
} from "../state/updates.js";

type LicenseAction = "check" | "install" | null;

/**
 * License + update state and actions, extracted from `App.tsx`. The licence itself is the native
 * owner's (`state/license`): this hook holds its latest `LicenseView` and nothing else. A change in
 * what the licence unlocks (tier, features, standing) restarts the daemon so the new entitlements
 * take effect; the owner's change event covers every change, including the licence section's
 * own activate and deactivate (`components/settings/tabs/LicenseTab`). So the daemon-lifecycle pieces
 * (`config`, `applyConfig`, `refreshBridge`, `setStatus`) are threaded in as dependencies.
 */
export interface LicenseStateDeps {
  config: DaemonConfig | null;
  applyConfig: (config: DaemonConfig) => void;
  refreshBridge: MutableRefObject<(config: DaemonConfig) => Promise<void>>;
  setStatus: (status: string) => void;
}

export function useLicenseState(deps: LicenseStateDeps) {
  const { config, applyConfig, refreshBridge, setStatus } = deps;

  const [licenseView, setLicense] = useState<LicenseView | null>(null);
  const [licenseActionPending, setLicenseActionPending] = useState<LicenseAction>(null);
  const [licenseLatestVersion, setLicenseLatestVersion] = useState("");
  const [licenseUpdateState, setLicenseUpdateState] = useState<UpdateState>("idle");
  const [licenseError, setLicenseError] = useState<string | null>(null);

  // The entitlement the daemon was last started for. A view that unlocks the same things is not a
  // change, so an activation's own change event does not restart the daemon a second time.
  const appliedRef = useRef<string | null>(null);

  const restartForEntitlement = useCallback(
    async (view: LicenseView | null, running: DaemonConfig | null) => {
      const signature = entitlementSignature(view);
      const previous = appliedRef.current;
      appliedRef.current = signature;
      if (previous === null || previous === signature || !running) return false;
      const restarted = await restartDaemon();
      applyConfig(restarted);
      await refreshBridge.current(restarted);
      return true;
    },
    [applyConfig, refreshBridge],
  );

  const configRef = useRef(config);
  configRef.current = config;
  const restartRef = useRef(restartForEntitlement);
  restartRef.current = restartForEntitlement;

  useEffect(() => {
    let alive = true;
    void licenseClient
      .status()
      .then((view) => {
        if (!alive) return;
        appliedRef.current ??= entitlementSignature(view);
        setLicense(view);
      })
      .catch((err) => {
        if (alive) setLicenseError(licenseErrorMessage(err, "License load failed"));
      });
    return () => {
      alive = false;
    };
  }, []);

  // Every change the owner makes (the licence section's activate and deactivate, a link-code
  // redemption, the Apple bridge exchange, a lease renewal): take the view the owner emitted and,
  // when it changes what is unlocked, restart the daemon so the new entitlements take effect.
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    void licenseClient
      .onChange(
        (view) => {
          setLicense(view);
          setLicenseError(null);
          void restartRef
            .current(view, configRef.current)
            .then((restarted) => {
              if (restarted && isLicensed(view)) setStatus("License updated");
            })
            .catch((err) => setLicenseError(licenseErrorMessage(err, "License reload failed")));
        },
        (err) => setLicenseError(licenseErrorMessage(err, "License reload failed")),
      )
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch(() => {
        /* no Tauri event bridge (browser QA) */
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [setStatus]);

  async function checkLicenseUpdate() {
    setLicenseActionPending("check");
    setLicenseError(null);
    try {
      await checkForUpdate(setLicenseLatestVersion, setLicenseUpdateState);
    } catch (err) {
      setLicenseError(licenseErrorMessage(err, "Update check failed"));
      setLicenseUpdateState("failed");
    } finally {
      setLicenseActionPending(null);
    }
  }

  async function downloadLicenseUpdate() {
    setLicenseError(null);
    await downloadPendingUpdate();
  }

  async function installLicenseUpdate() {
    setLicenseActionPending("install");
    setLicenseError(null);
    try {
      await installPendingUpdate(setLicenseUpdateState);
    } catch (err) {
      setLicenseError(licenseErrorMessage(err, "Update install failed"));
      setLicenseUpdateState("failed");
    } finally {
      setLicenseActionPending(null);
    }
  }

  return {
    licenseView,
    licenseActionPending,
    licenseLatestVersion,
    licenseUpdateState,
    licenseError,
    checkLicenseUpdate,
    downloadLicenseUpdate,
    installLicenseUpdate,
  };
}

export type UseLicenseStateReturn = ReturnType<typeof useLicenseState>;
