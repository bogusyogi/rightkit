import { createLicenseClient, isPro, type LicenseView } from "@rightkit/license";
import {
  createAutoUpdateClient,
  fetchReleaseStatus,
  isNewerVersion,
  type AutoUpdateClient,
  type AutoUpdateStatus,
  type CheckForUpdate,
  type UpdateDownloadEvent,
} from "@rightkit/app-update";
import { getPlatform } from "@rightkit/platform-ui";
import type { LicenseClientLike } from "@rightkit/settings-ui";
import { Channel } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { version } from "@rightkit-host/package.json";
import { invoke, isQaMode, listen } from "./ipc";
import { osFromPlatform } from "./platform";
import {
  releaseStatusUrl,
  runtimeUpdateFromNative,
  type NativeUpdateInfo,
} from "./updateAuth";

const APP_VERSION = version;
const RELEASE_STATUS_URL = "https://scraperight.cc/v1/apps/scraperight/releases/status.json";

let pendingUpdateClient: AutoUpdateClient | null = null;

// The licence lives natively (`rightkit-license` through the shell's `rightkit_license_*` commands): the
// key is typed once and handed to the native side, every answer is a view without a token, and the
// webview holds no licence state of its own. Both ipc functions are read at call time so the QA mock
// that `initIpc` swaps in answers them.
const client = createLicenseClient({
  invoke: <T>(command: string, args?: Record<string, unknown>) => invoke<T>(command, args),
  listen: <T>(event: string, callback: (event: { payload: T }) => void) =>
    listen<T>(event, callback),
});

/** The Apple-device bridge guard (macOS only: the command is absent elsewhere and in QA, so a failure is the no-op). */
async function setBridgeSuppressed(suppressed: boolean): Promise<void> {
  try {
    await invoke("pro_bridge_suppress_consumption", { suppressed });
  } catch {
    // Not available on this platform; nothing to guard.
  }
}

/** WS-9: on macOS, suppress bridge consumption BEFORE freeing this seat so a launch-time consume can't instantly re-activate it. */
export const suppressBridgeBeforeDeactivation = (): Promise<void> => setBridgeSuppressed(true);

/** The licence client the Settings licence section and the app's Pro gates share. */
export const licenseClient: LicenseClientLike = {
  async activate(key) {
    const view = await client.activate(key);
    // An explicit activation lifts a standing bridge-consumption suppression.
    if (view.status === "active") await setBridgeSuppressed(false);
    return view;
  },
  deactivate: () => client.deactivate(),
  status: () => client.status(),
  refresh: () => client.refresh(),
};

/** Subscribe to licence changes made anywhere (activation, refresh, deactivation, a bridge activation). */
export const onLicenseChange: typeof client.onChange = (callback, onError) =>
  client.onChange(callback, onError);

async function loadLicense(): Promise<LicenseView> {
  return client.status();
}

async function refreshLicense(): Promise<LicenseView> {
  return (await client.refresh()).view;
}

/**
 * fetch-shaped adapter over the Rust `scraperight_request` command (rightkit-http). The
 * webview never talks to scraperight.cc directly, so no CORS or plugin-http is needed.
 */
async function scrapeRightFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
  const method = (init?.method ?? "GET").toUpperCase();
  const body = typeof init?.body === "string" ? init.body : undefined;
  const result = await invoke<{ status: number; body: string }>("scraperight_request", {
    request: { method, url, body },
  });
  const nullBody = result.status === 204 || result.status === 205 || result.status === 304;
  return new Response(nullBody ? null : result.body, {
    status: result.status,
    headers: { "content-type": "application/json" },
  });
}

async function proUpdateVersion(): Promise<string | null> {
  try {
    const status = await fetchReleaseStatus(
      releaseStatusUrl(RELEASE_STATUS_URL, osFromPlatform(getPlatform())),
      scrapeRightFetch,
    );
    return status.updateVersion && isNewerVersion(status.updateVersion, APP_VERSION)
      ? status.updateVersion
      : null;
  } catch {
    return null;
  }
}

/** Update check done natively (tauri-plugin-updater in Rust) so license tokens never
 * reach the webview. Drop-in for the plugin's JS `check`. */
export const nativeCheckForUpdate: CheckForUpdate = async () => {
  const info = await invoke<NativeUpdateInfo | null>("update_native_check");
  if (!info) return null;
  return runtimeUpdateFromNative(info, async (onEvent) => {
    const channel = new Channel<UpdateDownloadEvent>();
    channel.onmessage = onEvent;
    await invoke("update_native_install", { checkId: info.checkId, onEvent: channel });
  });
};

export function createHostUpdateClient(
  license: LicenseView | null,
  check: CheckForUpdate,
): AutoUpdateClient {
  return createAutoUpdateClient({
    check,
    // No headers here: the native update commands attach the Bearer credentials in Rust.
    getHeaders: () => undefined,
    getInstallHeaders: async (tier) => {
      // A Pro update needs a fresh signed entitlement, which the native install reads from
      // the credential store; refresh it first. Patches never carry credentials.
      if (tier !== "patch") await refreshLicense();
      return undefined;
    },
    getProUpdateVersion: proUpdateVersion,
    isEligible: (tier) => tier === "patch" || (tier === "update" && isPro(license)),
    shouldAutoInstall: () => false,
  });
}

export async function checkForUpdate(): Promise<AutoUpdateStatus> {
  if (isQaMode) return invoke<AutoUpdateStatus>("check_for_updates");
  const license = await loadLicense();
  pendingUpdateClient = createHostUpdateClient(license, nativeCheckForUpdate);
  const status = await pendingUpdateClient.checkOnce();
  if (status.kind !== "available" || !status.eligible) pendingUpdateClient = null;
  return status;
}

export async function installAvailableUpdate(): Promise<AutoUpdateStatus> {
  if (isQaMode) return invoke<AutoUpdateStatus>("install_available_update");
  if (!pendingUpdateClient) {
    return { kind: "error", message: "Check for an update first." };
  }
  const status = await pendingUpdateClient.installAvailable();
  if (status.kind === "ready") await relaunch();
  return status;
}

export type { AutoUpdateStatus, LicenseView };
