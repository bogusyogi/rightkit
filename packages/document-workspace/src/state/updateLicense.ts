import { nativeIpc, openExternal } from "@rightkit/document-core/state/tauriBridge";
import { createLicenseClient, type LicenseClient } from "@rightkit/license";
import {
  createPersistedCheckCache,
  isNewerVersion as semverIsNewer,
  type AutoUpdateStatus,
  type CheckCacheStore,
  type PersistedCheckResult,
  type UpdateTier,
} from "@rightkit/app-update";
import { publicInstallerUrl as suitePublicInstallerUrl } from "@rightkit/app-update/suite-api";
import {
  UPDATE_MESSAGES,
  createNativeUpdateClient,
  type NativeUpdateClient,
  type UpdateCheckView,
  type UpdateRefusal,
} from "@rightkit/app-update/native";
import { isMac } from "@rightkit/document-core/platform";
import { logError } from "@rightkit/document-core/state/diagnostics";
import { hostAppName } from "../appName.js";

export type UpdateState =
  | "idle"
  | "checking"
  | "current"
  | "available" // a signed update is ready natively; download + install is a separate, explicit step
  | "available-external" // a newer public release must be downloaded manually
  | "installing"
  | "install-failed"
  | "failed"
  | "license-required";

const FALLBACK_APP_VERSION = "0.0.0-dev";

export function resolvedAppVersion(): string {
  const version = typeof __APP_VERSION__ === "string" ? __APP_VERSION__.trim() : "";
  if (version) return version;

  const message =
    `${hostAppName()} build is missing __APP_VERSION__; release/update version truth is unavailable.`;
  if (import.meta.env?.PROD) {
    throw new Error(message);
  }
  console.warn(`${message} Using ${FALLBACK_APP_VERSION} for this non-production harness.`);
  return FALLBACK_APP_VERSION;
}

export const APP_VERSION = resolvedAppVersion();
// Purchase CTA. /pricing is the real route (→ /checkout); /updates does not exist (was a 404).
export const UPDATE_LICENSE_URL = "https://viewright.cc/pricing/";
// The Right Suite app key: the owners (the native update host and @rightkit/app-update/suite-api)
// derive every host and URL from it, so this file names none.
const APP_KEY = "viewright";

export type { UpdateTier };

// One-time purge of records the pre-cutover webview kept: the token-bearing licence and the
// device-id cache. Native code owns both now; nothing token-bearing may remain in web storage.
for (const legacyKey of ["vr.updateLicense.v1", "vr.deviceId.v1"]) {
  try {
    localStorage.removeItem(legacyKey);
  } catch {
    /* ignore */
  }
}

// ── Licence: the shell-owned native client. The webview holds no key, token, device id or
//    record; it sends commands and receives validated LicenseView display values. ──
let licenseClientPromise: Promise<LicenseClient> | null = null;

export function getLicenseClient(): Promise<LicenseClient> {
  licenseClientPromise ??= Promise.resolve(createLicenseClient(nativeIpc));
  return licenseClientPromise;
}

// ── Updates: the shell-owned native update host. Check yields an opaque handle; download and
//    install take that handle. authorization headers, tokens and licence binding stay native. ──
let nativeUpdates: NativeUpdateClient | null = null;
function updates(): NativeUpdateClient {
  nativeUpdates ??= createNativeUpdateClient(nativeIpc);
  return nativeUpdates;
}

/** Refusals that mean "this licence does not entitle the update" (nudge to the licence panel). */
const LICENSE_REFUSALS = new Set<UpdateRefusal | "stale" | "consumed" | "deferred">([
  "notLicensed",
  "expired",
  "revoked",
  "tierIneligible",
  "disabledNoPublicPatch",
]);

let pendingUpdate: { handleId: string; tier: UpdateTier | null } | null = null;
let pendingExternalUpdateUrl: string | null = null;

function publicInstallerUrl(): string {
  return suitePublicInstallerUrl(
    APP_KEY,
    isMac() ? "mac/current/Host_universal.dmg" : "windows/current/Host_x64-setup.exe",
  );
}

export async function openPendingExternalUpdate(): Promise<void> {
  if (pendingExternalUpdateUrl) await openExternal(pendingExternalUpdateUrl);
}

// ── Release status (app policy: manual-download fallback and the paid-update nudge). The shell's
//    update host reads it natively (`rightkit_update_release_status`, https only, no redirect, closed
//    shape); the webview holds no network capability. The platform filter is set natively. ──
function fetchCurrentReleaseStatus() {
  return updates().releaseStatus();
}

async function latestProUpdateVersion(): Promise<string | null> {
  try {
    const status = await fetchCurrentReleaseStatus();
    return status.updateVersion && isNewerVersion(status.updateVersion, APP_VERSION)
      ? status.updateVersion
      : null;
  } catch {
    return null;
  }
}

// ── Last-known-good update-check cache (@rightkit/app-update createPersistedCheckCache). A
//    network hiccup after an earlier good check shows that result instead of "failed". Only
//    results that need no process-local handle are persisted. ──
export const UPDATE_CHECK_CACHE_KEY = "vr.updateCheck.v3";
// A persisted result younger than this is served without a network round-trip.
const UPDATE_CHECK_DEBOUNCE_MS = 30_000;

type CachedUpdateCheck = {
  state: "current" | "available-external";
  tier?: UpdateTier | null;
  version: string;
};
type UpdateCheckResult =
  | CachedUpdateCheck
  | { state: "available"; tier: UpdateTier | null; version: string; handleId: string }
  | { state: "license-required"; tier: UpdateTier | null; version: string }
  | { state: "failed" };

const checkCacheStore: CheckCacheStore = {
  async read() {
    try {
      return localStorage.getItem(UPDATE_CHECK_CACHE_KEY);
    } catch {
      return null;
    }
  },
  async write(text) {
    localStorage.setItem(UPDATE_CHECK_CACHE_KEY, text);
  },
};

const updateCheckCache = createPersistedCheckCache<UpdateCheckResult>({
  store: checkCacheStore,
  minIntervalMs: UPDATE_CHECK_DEBOUNCE_MS,
  // Also the write gate: a result that fails validation is returned but never persisted.
  validate: (value): value is UpdateCheckResult => {
    const v = value as Partial<CachedUpdateCheck> | null;
    return (
      typeof v === "object" &&
      v !== null &&
      (v.state === "current" || v.state === "available-external") &&
      typeof v.version === "string"
    );
  },
});

function applyCached(
  cached: CachedUpdateCheck,
  setLatestVersion: (version: string) => void,
  setUpdateState: (state: UpdateState) => void,
  setLatestTier?: (tier: UpdateTier | null) => void,
) {
  setLatestVersion(cached.version);
  setLatestTier?.(cached.tier ?? null);
  if (cached.state === "available-external") pendingExternalUpdateUrl = publicInstallerUrl();
  setUpdateState(cached.state);
}

async function resultFromCheck(view: UpdateCheckView): Promise<UpdateCheckResult> {
  if (!view.available) {
    const nudge = await latestProUpdateVersion();
    return nudge
      ? { state: "license-required", tier: "update", version: nudge }
      : { state: "current", tier: null, version: APP_VERSION };
  }
  const tier: UpdateTier | null = view.kind ?? null;
  const version = view.version ?? "";
  if (view.eligible && view.handleId) {
    return { state: "available", tier, version, handleId: view.handleId };
  }
  if (view.refusal === "offline") throw new Error("offline");
  if (LICENSE_REFUSALS.has(view.refusal)) return { state: "license-required", tier, version };
  return { state: "failed" };
}

// Step 1: check only, through the native update host.
export async function checkForUpdate(
  setLatestVersion: (version: string) => void,
  setUpdateState: (state: UpdateState) => void,
  setLatestTier?: (tier: UpdateTier | null) => void,
): Promise<UpdateState> {
  pendingUpdate = null;
  pendingExternalUpdateUrl = null;
  setUpdateState("checking");

  let outcome: PersistedCheckResult<UpdateCheckResult>;
  try {
    outcome = await updateCheckCache.run(async () => resultFromCheck(await updates().check()));
  } catch {
    return checkPublicReleaseFallback(setLatestVersion, setUpdateState, setLatestTier);
  }

  const result = outcome.value;
  if (outcome.source === "last-known-good") {
    if (result.state === "current") {
      applyCached(result, setLatestVersion, setUpdateState, setLatestTier);
      return "current";
    }
    return checkPublicReleaseFallback(setLatestVersion, setUpdateState, setLatestTier);
  }
  if (outcome.source === "cache" && (result.state === "current" || result.state === "available-external")) {
    applyCached(result, setLatestVersion, setUpdateState, setLatestTier);
    return result.state;
  }

  if (result.state === "available") {
    setLatestVersion(result.version);
    setLatestTier?.(result.tier);
    pendingUpdate = { handleId: result.handleId, tier: result.tier };
    setUpdateState("available");
    return "available";
  }
  if (result.state === "license-required") {
    setLatestVersion(result.version);
    setLatestTier?.(result.tier);
    setUpdateState("license-required");
    return "license-required";
  }
  if (result.state === "current") {
    setLatestTier?.(null);
    setUpdateState("current");
    return "current";
  }
  setUpdateState("failed");
  return "failed";
}

async function checkPublicReleaseFallback(
  setLatestVersion: (version: string) => void,
  setUpdateState: (state: UpdateState) => void,
  setLatestTier?: (tier: UpdateTier | null) => void,
): Promise<UpdateState> {
  try {
    const status = await fetchCurrentReleaseStatus();
    const version = status.patchVersion?.trim() ?? "";
    if (!version || !isNewerVersion(version, APP_VERSION)) {
      if (status.updateVersion && isNewerVersion(status.updateVersion, APP_VERSION)) {
        setLatestVersion(status.updateVersion);
        setLatestTier?.("update");
        setUpdateState("license-required");
        return "license-required";
      }
      setUpdateState("current");
      return "current";
    }

    pendingExternalUpdateUrl = publicInstallerUrl();
    setLatestVersion(version);
    setLatestTier?.(null);
    setUpdateState("available-external");
    await updateCheckCache.run(
      async () => ({ state: "available-external" as const, tier: null, version }),
      { force: true },
    );
    return "available-external";
  } catch {
    setUpdateState("failed");
    return "failed";
  }
}

async function relaunchAfterInstall(): Promise<void> {
  try {
    const { relaunch } = await import("@tauri-apps/plugin-process");
    await relaunch();
  } catch {
    /* the host may already be restarting; the install itself succeeded */
  }
}

// Step 2: explicit download + install of the update found by checkForUpdate(), then relaunch.
export async function installPendingUpdate(
  setUpdateState: (state: UpdateState) => void,
  setInstallError?: (message: string) => void,
): Promise<UpdateState> {
  if (!pendingUpdate) return "idle";
  const { handleId, tier } = pendingUpdate;
  pendingUpdate = null;
  setUpdateState("installing");

  let failure = UPDATE_MESSAGES.updateFailed as string;
  try {
    const downloaded = await updates().download(handleId);
    if (downloaded.state === "downloaded") {
      const installed = await updates().install(handleId);
      if (installed.state === "installing") {
        await relaunchAfterInstall();
        return "installing";
      }
      if (installed.state === "refused" && installed.reason && LICENSE_REFUSALS.has(installed.reason)) {
        setUpdateState("license-required");
        return "license-required";
      }
      failure = UPDATE_MESSAGES[installed.message] || failure;
    } else {
      if (downloaded.state === "refused" && downloaded.reason && LICENSE_REFUSALS.has(downloaded.reason)) {
        setUpdateState("license-required");
        return "license-required";
      }
      failure = UPDATE_MESSAGES[downloaded.message] || failure;
    }
  } catch (error) {
    // NativeUpdateClientError text is a fixed, closed message; native detail never crosses IPC.
    if (error instanceof Error && error.message) failure = error.message;
  }
  // Surface the failure two ways: setInstallError feeds the UI (ephemeral) and logError lands it as
  // renderer_error in the diagnostics export (durable). On a patch, also stage the public installer
  // URL so the user can recover via a manual download.
  if (tier === "patch") pendingExternalUpdateUrl = publicInstallerUrl();
  setInstallError?.(failure);
  logError("update:install-failed");
  setUpdateState("install-failed");
  return "install-failed";
}

/** True while a manual-download installer URL is staged (public fallback or failed patch install). */
export function hasPendingExternalUpdate(): boolean {
  return pendingExternalUpdateUrl !== null;
}

/**
 * the host app update flow as the function-style client `@rightkit/settings-ui` `updatesSection`
 * expects (`UpdatesAdapter`): check()/install() resolve to an `AutoUpdateStatus`. The explicit
 * check -> install split, the last-known-good cache and the public-release fallback stay in
 * checkForUpdate()/installPendingUpdate(); this only maps their UpdateState into the shared status.
 * `onChange` fires after each call so UI outside the section can re-read hasPendingExternalUpdate().
 */
export function createHostUpdatesAdapter(onChange?: () => void): {
  check(): Promise<AutoUpdateStatus>;
  install(): Promise<AutoUpdateStatus>;
} {
  let version = "";
  let tier: UpdateTier | null = null;
  const toStatus = (state: UpdateState, error = ""): AutoUpdateStatus => {
    switch (state) {
      case "current":
        return { kind: "up_to_date" };
      case "available":
        return { kind: "available", version, tier, eligible: true };
      case "license-required":
        return { kind: "available", version, tier, eligible: false, reason: "pro_required" };
      case "available-external":
        return { kind: "deferred", version, tier: null, reason: "manual_download" };
      case "installing":
        return { kind: "ready", version, tier };
      case "install-failed":
        return { kind: "error", message: error || "The updater could not finish installing this release." };
      case "failed":
        return { kind: "error", message: "Couldn't check releases." };
      default:
        return { kind: "idle" };
    }
  };
  return {
    async check() {
      try {
        const state = await checkForUpdate(
          (v) => (version = v),
          () => {},
          (t) => (tier = t),
        );
        return toStatus(state);
      } finally {
        onChange?.();
      }
    },
    async install() {
      let error = "";
      try {
        const state = await installPendingUpdate(() => {}, (m) => (error = m));
        return state === "idle"
          ? { kind: "error", message: "No update is ready to install. Check for updates first." }
          : toStatus(state, error);
      } finally {
        onChange?.();
      }
    },
  };
}

/** Startup check through the native host. The host reads the licence record, so a paid update is
 *  announced for a licensed user; a failure is advisory and returns null. */
export async function checkForBackgroundUpdate(): Promise<{
  version: string;
  tier: UpdateTier | null;
} | null> {
  const view = await updates().check();
  return view.available && view.eligible && view.version
    ? { version: view.version, tier: view.kind ?? null }
    : null;
}

/** Package semver compare, but an unparseable version never counts as newer (it throws on garbage). */
function isNewerVersion(candidate: string, current: string): boolean {
  try {
    return semverIsNewer(candidate, current);
  } catch {
    return false;
  }
}
