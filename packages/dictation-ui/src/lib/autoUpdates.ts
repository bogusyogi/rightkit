import {
  fetchReleaseStatus,
  isNewerVersion,
  type AutoUpdateStatus,
  type CheckForUpdate,
} from "@rightkit/app-update";
import { check } from "@tauri-apps/plugin-updater";
import {
  APP_VERSION,
  isPro,
  licenseUpdateHeaders,
  licenseUpdateInstallHeaders,
} from "./license.js";
import { invoke, listen } from "./tauri.js";
import { logs } from "./logs.js";
import { getPlatform } from "../hooks/usePlatform.js";
import { createHostAutoUpdateClient } from "./heardRightAutoUpdateClient.js";
import { relaunchForUpdate } from "./relaunchForUpdate.js";
import type { SettingsBlob } from "../types/settings.js";

type AppStatePayload = { state?: string };

const BUSY_STATES = new Set(["recording", "transcribing", "pasting"]);
const RELEASE_STATUS_URL = "https://heardright.app/v1/apps/heardright/releases/status.json";

let started = false;
let manualCheckInProgress = false;
let interactiveInstall: Promise<AutoUpdateStatus> | null = null;
function initialStatus(): AutoUpdateStatus {
  if (!import.meta.env?.DEV || typeof window === "undefined") return { kind: "idle" };
  const params = new URLSearchParams(window.location.search);
  if (params.get("qa") !== "1") return { kind: "idle" };
  switch (params.get("qa-update")) {
    case "available":
      return { kind: "available", version: "0.1.141", tier: "patch", eligible: true };
    case "downloading":
      return { kind: "downloading", version: "0.1.141", tier: "patch", pct: 64 };
    case "ready":
      return { kind: "ready", version: "0.1.141", tier: "patch" };
    case "error":
      return { kind: "error", message: "The update could not be installed." };
    default:
      return { kind: "idle" };
  }
}

let currentStatus: AutoUpdateStatus = initialStatus();
let lastLoggedStatusKind: string | null = null;
let client: ReturnType<typeof createHostAutoUpdateClient> | null = null;
const listeners = new Set<(status: AutoUpdateStatus) => void>();

type NativeBackgroundUpdateStatus =
  | { kind: "idle" | "up_to_date" | "available" }
  | { kind: "error"; message: string };

function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function publish(status: AutoUpdateStatus) {
  currentStatus = status;
  for (const listener of listeners) listener(status);
}

async function canInstallPatch(): Promise<boolean> {
  try {
    const state = await invoke<AppStatePayload>("get_app_state");
    return !BUSY_STATES.has(state?.state ?? "");
  } catch {
    return true;
  }
}

async function shouldAutoInstall(): Promise<boolean> {
  try {
    const settings = await invoke<SettingsBlob>("get_settings");
    return settings.auto_update_install === true;
  } catch {
    return false;
  }
}

async function latestProUpdateVersion(): Promise<string | null> {
  try {
    const hostPlatform = getPlatform();
    if (hostPlatform !== "macos" && hostPlatform !== "windows") return null;
    const platform = hostPlatform === "macos" ? "darwin" : "windows";
    const status = await fetchReleaseStatus(
      `${RELEASE_STATUS_URL}?platform=${encodeURIComponent(platform)}`,
    );
    return status.updateVersion && isNewerVersion(status.updateVersion, APP_VERSION)
      ? status.updateVersion
      : null;
  } catch {
    return null;
  }
}

export function subscribeAutoUpdateStatus(
  listener: (status: AutoUpdateStatus) => void,
): () => void {
  listeners.add(listener);
  listener(currentStatus);
  return () => listeners.delete(listener);
}

export async function installAvailableUpdate(): Promise<AutoUpdateStatus> {
  return getAutoUpdateClient().installAvailable();
}

async function waitUntilInstallSafe(): Promise<void> {
  if (await canInstallPatch()) return;

  await new Promise<void>((resolve, reject) => {
    let settled = false;
    let unlisten: (() => void) | undefined;
    const finish = () => {
      if (settled) return;
      settled = true;
      unlisten?.();
      resolve();
    };

    void listen<AppStatePayload>("app-state-changed", (event) => {
      if (!BUSY_STATES.has(event.payload?.state ?? "")) finish();
    })
      .then((stop) => {
        unlisten = stop;
        if (settled) {
          stop();
          return;
        }
        // Close the race between the first state check and listener setup.
        void canInstallPatch().then((safe) => safe && finish());
      })
      .catch(reject);
  });
}

/** Install from the sidebar, then relaunch once active dictation work is safe. */
export function installAvailableUpdateAndRelaunch(): Promise<AutoUpdateStatus> {
  if (interactiveInstall) return interactiveInstall;

  interactiveInstall = (async () => {
    let status: AutoUpdateStatus;
    do {
      await waitUntilInstallSafe();
      status = await installAvailableUpdate();
    } while (status.kind === "deferred");

    if (status.kind === "ready") await relaunchForUpdate();
    return status;
  })().finally(() => {
    interactiveInstall = null;
  });

  return interactiveInstall;
}

export async function checkForAvailableUpdate(): Promise<AutoUpdateStatus> {
  manualCheckInProgress = true;
  try {
    return await getAutoUpdateClient().checkOnce();
  } finally {
    manualCheckInProgress = false;
  }
}

function getAutoUpdateClient(): ReturnType<typeof createHostAutoUpdateClient> {
  if (client) return client;
  client = createHostAutoUpdateClient({
    check: check as CheckForUpdate,
    getProUpdateVersion: latestProUpdateVersion,
    isPro: () => isPro(null),
    getHeaders() {
      return licenseUpdateHeaders();
    },
    getInstallHeaders(tier) {
      return licenseUpdateInstallHeaders(tier);
    },
    canInstall: canInstallPatch,
    async shouldAutoInstall() {
      return !manualCheckInProgress && shouldAutoInstall();
    },
    deferReason: () => "app_busy",
    onStatus(status) {
      publish(status);
      // Log TRANSITIONS only. `onStatus` fires for every download progress
      // tick, so logging unconditionally can emit ~800 events/second while
      // fetching a large bundle, enough to overflow the log buffer and
      // saturate the IPC bridge the UI needs. Errors always log: two
      // consecutive failures are two distinct events even when the kind
      // repeats.
      if (status.kind === lastLoggedStatusKind && status.kind !== "error") return;
      lastLoggedStatusKind = status.kind;
      logs.info("update_auto_check", {
        action: "auto_check",
        status: status.kind,
        source: "rightkit_updates",
        ...(status.kind === "error" ? { message: status.message } : {}),
      });
    },
    onError(error) {
      logs.info("update_auto_check", {
        action: "auto_check",
        status: "error",
        source: "rightkit_updates",
        message: String(error),
      });
    },
  });
  return client;
}

/// The native loop in `src-tauri/src/background_updates.rs` is the only update
/// scheduler (30s after launch, then every 3h). This side owns no timer: it
/// reads the cached native status once, then reacts to `background-update-status`
/// events, rather than polling on its own schedule (which would double the
/// manifest requests and duplicate the interval constant in two languages).
export function startBackgroundUpdateChecks() {
  if (started || !inTauri()) return;
  started = true;
  const updateClient = getAutoUpdateClient();

  const reconcileNativeStatus = (status: NativeBackgroundUpdateStatus | null | undefined) => {
    if (!status || status.kind === "idle") return;
    if (currentStatus.kind === "checking" || currentStatus.kind === "downloading" || currentStatus.kind === "ready") return;
    if (status.kind === "available") {
      void updateClient.checkOnce();
    } else if (status.kind === "up_to_date") {
      publish({ kind: "up_to_date" });
    } else if (status.kind === "error" && currentStatus.kind !== "available" && currentStatus.kind !== "deferred") {
      publish({ kind: "error", message: status.message });
    }
  };

  // Subscribe first so completion cannot fall between cache read & listener setup.
  let receivedEvent = false;
  void listen<NativeBackgroundUpdateStatus>("background-update-status", (event) => {
    receivedEvent = true;
    reconcileNativeStatus(event.payload);
  }).then(() => invoke<NativeBackgroundUpdateStatus>("background_update_status"))
    .then((status) => { if (!receivedEvent) reconcileNativeStatus(status); })
    .catch(() => undefined);

}
