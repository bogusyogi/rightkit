// The renderer side of the host services the shell registers in src-tauri/src/services.rs, all trusted for the main window
// only: settings (M10, @rightkit/settings-ui over `rightkit_settings_*`), the native legal receipt (M14, @rightkit/onboarding
// over `rightkit_legal_receipt_*`) and the licence (M12, @rightkit/license over `rightkit_license_*`). Native code owns every
// store, the receipt's time and every licence credential; the renderer gets views and fixed error codes.
import { listen } from "@tauri-apps/api/event";
import { createLicenseClient, licenseMessage, type LicenseView } from "@rightkit/license";
import { createNativeLegalReceiptPort, licenseBackendFromView } from "@rightkit/onboarding";
import { createShellBackends } from "@rightkit/settings-ui/shell-adapter";
import { invoke, isTauri } from "./host";

const transport = {
  invoke,
  listen: <T,>(event: string, handler: (payload: T) => void) => listen<T>(event, (e) => handler(e.payload)),
};

/** Settings backend: `load`, `patch(expectedRevision)`, `describe`, `reset` and change events. */
export const settings = createShellBackends(transport).settings;

/** The legal receipt: the native service stamps the acceptance time and checks the declaration against its manifest. */
export const legalReceipt = createNativeLegalReceiptPort({ invoke });

/** The licence client. Until the app registers its appKey, host and key natively, `status()` reports `mode: "disabled"`. */
export const license = createLicenseClient({ invoke, listen });

/** The onboarding licence step's backend; trials are offered only once licensing is configured. */
export const licenseBackend = licenseBackendFromView(license, { messageText: licenseMessage });

/** Licensing is configured natively (a registered appKey, host and key). Outside Tauri there is no licence. */
export async function licensingEnabled(): Promise<boolean> {
  if (!isTauri()) return false;
  const view: LicenseView = await license.status();
  return view.mode !== "disabled";
}
