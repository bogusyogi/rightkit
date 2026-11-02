// Core licence client + the primary public licensing API, shared by the sibling concern modules
// in this directory (accountSession.ts, windowsLink.ts, proBridge.ts) and re-exported through the
// `../license.ts` facade. See the facade's doc comment for the module-level contract.
//
// The licence lifecycle is native. Credentials, the signed record, the anti-rollback clock, the
// device identity and every network call live in Rust (`rightkit-license`); this module only
// calls the shell's `rightkit_license_*` commands through `@rightkit/license`'s
// `createLicenseClient` and works with the validated display views it returns
// (`view.tier`, `view.features`, `licenseMessage(view.message)`). A view is a display hint:
// native code authorises every paid operation again when it runs.

import {
  createLicenseClient,
  isPro as viewIsPro,
  LicenseClientError,
  type LicenseView,
} from "@rightkit/license";

import { invoke, listen } from "../tauri.js";
import { getLicenseLifecycle, snapshotFromLicenseView } from "../licenseLifecycle.js";
import type { LicenseLifecycleSnapshot } from "../licenseLifecycle.js";
import { setProBridgeConsumptionSuppressed } from "./proBridge.js";

export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export const APP_VERSION: string = typeof __APP_VERSION__ === "string" ? __APP_VERSION__ : "0.0.0";

/** The one licence client of the app. `invoke` is the app's own Tauri IPC bridge. Licence changes reach the UI through explicit reads
 * after each operation, the lifecycle monitor (launch, focus, boundary timers), the
 * `hr-license-updated` window event and the owner's change event (`onLicenseChanged`). */
export const licenseClient = createLicenseClient({
  invoke,
  // The shell emits `rightkit://license-changed`; the hub hears it as the route-manifest event
  // `license-changed` (the host app grants no `core:event:*`, see renderer-event-routes.json).
  listen: <T,>(event: string, handler: (message: { payload: T }) => void) =>
    event === "rightkit://license-changed"
      ? listen<T>("license-changed", (message) => handler({ payload: message.payload }))
      : Promise.resolve(() => undefined),
});

/** Follow native licence changes (the owner's `rightkit://license-changed`, delivered to the hub
 * through the `license-changed` route). Returns the cleanup. */
export function onLicenseChanged(callback: (view: LicenseView) => void): () => void {
  let release: (() => void) | undefined;
  let stopped = false;
  void licenseClient
    .onChange(callback)
    .then((unlisten) => {
      if (stopped) unlisten();
      else release = unlisten;
    })
    .catch(() => undefined);
  return () => {
    stopped = true;
    release?.();
  };
}

/** The renderer's display Pro flag (the lifecycle monitor's last read). Never an authority: every
 * paid command is authorised natively when it runs. The argument is accepted for call-site
 * compatibility and ignored. */
export function isPro(_view?: LicenseView | null): boolean {
  return getLicenseLifecycle().isPro;
}

/** True when a view alone would show Pro (display only, fails closed while busy or unresolved). */
export function viewShowsPro(view: LicenseView | null): boolean {
  return viewIsPro(view);
}

// --- storage state: surfaces "the OS credential store is unavailable" without a retry loop ---

export type LicenseStorageOperation = "read" | "write" | "delete" | "device";
export type LicenseStorageState =
  | { kind: "ready" }
  | { kind: "error"; operation: LicenseStorageOperation; message: string };

export class SecureLicenseStorageError extends Error {
  readonly operation: LicenseStorageOperation;

  constructor(operation: LicenseStorageOperation, cause: unknown) {
    const detail = cause instanceof Error ? cause.message : String(cause);
    super(`Secure license storage is unavailable: ${detail}`);
    this.name = "SecureLicenseStorageError";
    this.operation = operation;
  }
}

let licenseStorageState: LicenseStorageState = { kind: "ready" };
const licenseStorageListeners = new Set<(state: LicenseStorageState) => void>();

function publishLicenseStorageState(state: LicenseStorageState) {
  licenseStorageState = state;
  for (const listener of licenseStorageListeners) listener(state);
}

function storageReady() {
  if (licenseStorageState.kind !== "ready") publishLicenseStorageState({ kind: "ready" });
}

export function getLicenseStorageState(): LicenseStorageState {
  return licenseStorageState;
}

export function subscribeLicenseStorageState(
  listener: (state: LicenseStorageState) => void,
): () => void {
  licenseStorageListeners.add(listener);
  listener(licenseStorageState);
  return () => licenseStorageListeners.delete(listener);
}

/** Run a licence call. A `store` failure (the credential store could not be read or written) is a
 * storage error the settings banner shows; every other rejection passes through unchanged (its
 * `message` is a fixed English text chosen natively, never a server string). */
async function guarded<T>(operation: LicenseStorageOperation, work: () => Promise<T>): Promise<T> {
  try {
    const value = await work();
    storageReady();
    return value;
  } catch (cause) {
    if (cause instanceof LicenseClientError && cause.code === "store") {
      const error = new SecureLicenseStorageError(operation, cause);
      publishLicenseStorageState({ kind: "error", operation, message: error.message });
      throw error;
    }
    throw cause;
  }
}

/** Notify listeners (licenseLifecycle.ts and any UI observers) that licence state changed.
 * Centralized here so every concern module emits the identical event after its mutation. */
export function notifyLicenseUpdated() {
  window.dispatchEvent(new Event("hr-license-updated"));
}

/** Re-push entitlement-derived config to the engine so Pro features (file length, voice commands)
 * flip on/off immediately, without an app restart. */
async function syncEntitlements() {
  await refreshEntitlements();
}

/** Re-push entitlements without failing the caller's licence operation, but never silently: a
 * failed push leaves Pro features in their old state until the next refresh. */
export async function refreshEntitlements() {
  await invoke("refresh_entitlements").catch((error: unknown) => {
    console.warn("refresh_entitlements failed", error);
  });
}

/** The licence as it stands (native, no network). */
export async function loadLicense(): Promise<LicenseView> {
  return guarded("read", () => licenseClient.status());
}

/** Retry the native licence read after the user unlocks or repairs their OS credential store. A
 * failed retry remains Free and republishes the error. */
export async function retryLicenseStorage(): Promise<LicenseView> {
  return loadLicense();
}

/** The lifecycle monitor's native read: the licence view projected to the UI lifecycle snapshot. */
export async function readLicenseSnapshot(): Promise<LicenseLifecycleSnapshot> {
  return snapshotFromLicenseView(await loadLicense());
}

/** Activate a licence key. The key goes to native code once and is never stored in the renderer.
 * Native code enters the renewable device-lease lifecycle (new activations are never
 * forever-local tokens). Answers the new view; `view.status === "active"` is the only success. */
export async function activateLicense(key: string): Promise<LicenseView> {
  const view = await guarded("write", () => licenseClient.activate(key));
  if (view.status === "active") {
    // An explicit key activation clears any standing Pro-bridge consumption suppression on this
    // device, the same "explicit activation clears it" rule pro_bridge.rs applies to its own
    // bridge-exchange success path.
    if (inTauri) {
      await setProBridgeConsumptionSuppressed(false).catch(() => undefined);
    }
    await syncEntitlements();
    notifyLicenseUpdated();
  }
  return view;
}

/** Re-validate with the licence service (a lease record renews its lease). Offline failure keeps
 * the stored licence, which native code still verifies against its own grace window. */
export async function refreshLicenseForLifecycle(): Promise<void> {
  await licenseClient.refresh();
}

/** Open the pricing / buy-Pro page in the system browser (URL fixed in Rust). */
export async function openPricing(): Promise<void> {
  await invoke("open_pricing_page").catch(() => undefined);
}

/** Request a Pro trial KEY by email. The server mints a trial licence tied to the email and EMAILS
 * the key - it is NOT returned here, so a junk email that never receives it can't activate. The
 * trial length depends on the platform, which native code adds itself. Returns true if the request
 * was accepted; the user then pastes the emailed key and activates it with `activateLicense`. A
 * rejection (rate limit, network) is false: the UI prompts a retry. */
export async function requestTrialKey(email: string): Promise<boolean> {
  try {
    const trial = await licenseClient.requestTrial(email);
    return trial.accepted;
  } catch {
    return false;
  }
}

/** Remove this device's licence: suppress this Mac's Apple-device Pro-bridge consumption first, so
 * it cannot instantly re-activate itself from the shared bridge the moment its own seat is freed.
 * Native code persists the durable server-release queue before it tombstones the local record
 * (offline-first), so the local removal never depends on the network. */
export async function removeLicenseWithBridgeSuppression(): Promise<void> {
  if (inTauri) {
    await setProBridgeConsumptionSuppressed(true).catch(() => undefined);
  }
  await guarded("delete", () => licenseClient.deactivate());
  await syncEntitlements();
  notifyLicenseUpdated();
}

/** Headers for the update CHECK, or undefined when no active licence can authenticate it. They
 * are built natively and handed only to the updater transport. */
export async function licenseUpdateHeaders(): Promise<Record<string, string> | undefined> {
  return (await invoke<Record<string, string> | null>("license_update_headers")) ?? undefined;
}

/** Resolve artifact authorization at install time. Public patches deliberately carry no artifact
 * credential; paid updates require a fresh signed Pro token, which native code verifies first. */
export async function licenseUpdateInstallHeaders(
  tier: "patch" | "update",
): Promise<Record<string, string> | undefined> {
  return (
    (await invoke<Record<string, string> | null>("license_update_install_headers", { tier })) ??
    undefined
  );
}

/** Normalize raw key input for the activation field. Issued keys are
 *  `SR-<APPKEY>-<32 hex>` (e.g. `SR-HEARDRIGHT-...`) — uppercase with real
 *  dashes — so the ONLY safe transforms are trimming whitespace and
 *  uppercasing. Never reshape or truncate: the old `AD-0000-0000-0000`
 *  formatter (built around the 14-char master key) mangled real keys. */
export function formatLicenseKey(raw: string): string {
  return raw.toUpperCase().replace(/\s+/g, "");
}
