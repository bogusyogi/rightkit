// Pro licensing - on the shared native licence owner.
//
// The lifecycle (activate into a renewable device lease, refresh and re-anchor, deactivate, the
// signed record, the anti-rollback clock, the device identity, the account session and the
// Pro-bridge and link-code transports) is native: `rightkit-license` in Rust, exposed through the
// shell's `rightkit_license_*` commands and `@rightkit/license`'s `createLicenseClient`. The
// renderer holds only validated display views (`view.tier`, `view.features`,
// `licenseMessage(view.message)`); a licence key and a trial e-mail are transient command inputs,
// and no token, record or device id is stored or fetched here.
//
// HR feature-gates (Free = dictation; Pro = wake word + voice commands + BYOK LLM). This module
// answers the product question `isPro()` for DISPLAY. Every paid operation is authorised again
// natively (`license::has_pro`, the owner's `NativeEngineEntitlements`) when it runs, so a forged
// or stale renderer flag cannot unlock anything.
//
// Server: heardright.app/v1/licenses/* (the brand host forwards /v1/ to the rightapps-api
// origin); native code makes every call.
//
// Facade: the implementation is split by concern under `lib/license/` —
//   core.ts           licence client, views, activation / refresh / removal, update headers
//   windowsLink.ts    HR-PRO-015 Windows link-code request/redeem
//   accountSession.ts HR-ACCOUNT-P1..P8 account session + device management
//   proBridge.ts      macOS iCloud-Keychain Pro continuity wrappers
//   errors.ts         wording for the closed errors of the commands above
// This file re-exports the public API so existing `import ... from "(../)*lib/license"` call sites
// are unchanged.

export {
  APP_VERSION,
  SecureLicenseStorageError,
  activateLicense,
  formatLicenseKey,
  getLicenseStorageState,
  isPro,
  licenseClient,
  licenseUpdateHeaders,
  licenseUpdateInstallHeaders,
  loadLicense,
  openPricing,
  onLicenseChanged,
  refreshEntitlements,
  readLicenseSnapshot,
  refreshLicenseForLifecycle,
  removeLicenseWithBridgeSuppression,
  requestTrialKey,
  retryLicenseStorage,
  subscribeLicenseStorageState,
  viewShowsPro,
  type LicenseStorageOperation,
  type LicenseStorageState,
} from "./license/core.js";

export type { LicenseView } from "@rightkit/license";

export {
  redeemWindowsLinkCode,
  requestWindowsLinkCode,
} from "./license/windowsLink.js";

export {
  beginAccountMagicLink,
  completeAccountSession,
  deleteAccount,
  getAccountSessionStatus,
  listAccountDevices,
  refreshAccountSession,
  removeAccountDevice,
  renameAccountDevice,
  signOutAccount,
  type AccountDeviceRow,
  type AccountSessionStatus,
} from "./license/accountSession.js";

export {
  getProBridgeStatus,
  setProBridgeConsumptionSuppressed,
  setProBridgeSyncEnabled,
  type ProBridgeStatus,
} from "./license/proBridge.js";
