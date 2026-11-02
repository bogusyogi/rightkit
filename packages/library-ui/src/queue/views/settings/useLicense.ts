import { useLicenseSection } from "@rightkit/settings-ui";
import { useEffect, useRef } from "react";
import { licenseClient, onLicenseChange, type LicenseView } from "../../lib/license.js";

export interface LicenseState {
  license: LicenseView | null;
  /** True once the first load finished, so the UI can avoid flashing "Free". */
  loaded: boolean;
  pro: boolean;
  /** Re-read the licence (call after the Settings licence section changes it). */
  refresh: () => Promise<LicenseView | null>;
}

/** Entitlement read for the app's Pro gates. Activation and deactivation run in the Settings licence section. */
export function useLicense(): LicenseState {
  const lic = useLicenseSection({ client: licenseClient, confirmDeactivate: false });
  const { refresh } = lic;
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  // The native side announces every licence change (activation, refresh, deactivation, a bridge
  // activation), so every reader of the licence follows it.
  useEffect(() => {
    let stop: (() => void) | undefined;
    let cancelled = false;
    onLicenseChange(() => void refreshRef.current())
      .then((unlisten) => {
        if (cancelled) unlisten();
        else stop = unlisten;
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
      stop?.();
    };
  }, []);
  return { license: lic.license, loaded: lic.loaded, pro: lic.pro, refresh };
}
