import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  deriveUpdateOverlay,
  fetchReleaseStatus,
  isNewerVersion,
  type AutoUpdateStatus,
  type UpdateTier,
} from "@rightkit/app-update";
import {
  createNativeUpdateClient,
  NativeUpdateClientError,
  UPDATE_MESSAGES,
  type NativeUpdateClient,
  type UpdateCheckView,
  type UpdateMessageCode,
} from "@rightkit/app-update/native";
import { beginBootProbation, bootProbation } from "../lib/bootProbation.js";
import { releasePlatform } from "./platform.js";

export type UpdateState =
  | "idle"
  | "checking"
  | "current"
  | "patch-available"
  | "update-available"
  | "pro-update-available"
  | "installing"
  | "failed";

let appVersionCache: Promise<string> | null = null;
/** The running build's version from the native shell (`getVersion`), never a literal. */
export function appVersion(): Promise<string> {
  appVersionCache ??= import("@tauri-apps/api/app").then(({ getVersion }) => getVersion());
  return appVersionCache;
}
export const RELEASE_STATUS_URL = "https://api.coderight.cc/releases/status.json";

export function releaseStatusUrl(platform: "darwin" | "windows"): string {
  return `${RELEASE_STATUS_URL}?platform=${platform}`;
}

// Latest status the update controller published. AppFrame's UpdateOverlay and LicenseTab render from it.
let updateStatus: AutoUpdateStatus = { kind: "idle" };
const updateStatusListeners = new Set<() => void>();

export function getUpdateStatus(): AutoUpdateStatus {
  return updateStatus;
}

export function subscribeUpdateStatus(listener: () => void): () => void {
  updateStatusListeners.add(listener);
  return () => {
    updateStatusListeners.delete(listener);
  };
}

function publishUpdateStatus(status: AutoUpdateStatus): void {
  updateStatus = status;
  for (const listener of updateStatusListeners) listener();
}

/** UpdateOverlay "Dismiss": the idle status hides the overlay (deriveUpdateOverlay phase `hidden`). */
export function dismissUpdateStatus(): void {
  publishUpdateStatus({ kind: "idle" });
}

async function proUpdateVersion(): Promise<string | null> {
  try {
    const platform = releasePlatform();
    if (!platform) return null;
    const status = await fetchReleaseStatus(releaseStatusUrl(platform));
    return status.updateVersion && isNewerVersion(status.updateVersion, await appVersion())
      ? status.updateVersion
      : null;
  } catch {
    return null;
  }
}

/**
 * Maps the published overlay model (`deriveUpdateOverlay`, @rightkit/app-update) onto the
 * app's `UpdateState` consumed by the settings surfaces. Phases with no settings state (hidden,
 * deferred, downloading, staged, ready) leave the current state unchanged, as before.
 */
function updateStateFromStatus(
  status: AutoUpdateStatus,
  setLatestVersion: (version: string) => void,
  setUpdateState: (state: UpdateState) => void,
): void {
  if (status.kind === "available") setLatestVersion(status.version);
  const { phase } = deriveUpdateOverlay(status);
  switch (phase) {
    case "available":
      setUpdateState(status.kind === "available" && status.tier === "patch" ? "patch-available" : "update-available");
      return;
    case "available_ineligible":
      setUpdateState("pro-update-available");
      return;
    case "up_to_date":
      setUpdateState("current");
      return;
    case "error":
      setUpdateState("failed");
      return;
    case "checking":
      setUpdateState("checking");
      return;
    default:
      return;
  }
}

function messageText(code: UpdateMessageCode | undefined, fallback: string): string {
  const text = code ? UPDATE_MESSAGES[code] : "";
  return text || fallback;
}

function errorStatus(error: unknown, fallback: string): AutoUpdateStatus {
  return {
    kind: "error",
    message: error instanceof NativeUpdateClientError ? messageText(error.messageCode, fallback) : fallback,
  };
}

/**
 * The checked update, as the native host reported it. The handle is an opaque id the host issued
 * for an eligible release; the download and install commands take only it.
 */
interface PendingUpdate {
  handleId: string;
  version: string;
  tier: UpdateTier;
  staged: boolean;
}

export interface UpdateControllerDependencies {
  native: Pick<NativeUpdateClient, "check" | "download" | "install" | "onProgress">;
  /** Public release status: the newest feature version, for a client the host will not offer it to. */
  proUpdateVersion: () => Promise<string | null>;
  /** Boot probation for the version being installed (recorded before the installer can end this process). */
  beginBootProbation: (toVersion: string) => Promise<void>;
  /** Clears the probation record of an install that did not happen. */
  abandonBootProbation: () => Promise<void>;
  relaunch: () => Promise<void>;
  publish: (status: AutoUpdateStatus) => void;
}

export interface UpdateController {
  check(
    setLatestVersion: (version: string) => void,
    setUpdateState: (state: UpdateState) => void,
  ): Promise<void>;
  download(): Promise<void>;
  install(setUpdateState: (state: UpdateState) => void): Promise<void>;
  hasPending(): boolean;
}

/** The status a native check answer means. Entitlement is decided natively; this only words it. */
async function statusFromCheck(
  view: UpdateCheckView,
  proVersion: () => Promise<string | null>,
): Promise<{ status: AutoUpdateStatus; pending: PendingUpdate | null }> {
  if (!view.available || !view.version) {
    const feature = await proVersion();
    return {
      status: feature
        ? { kind: "available", version: feature, tier: "update", eligible: false, reason: "pro_required" }
        : { kind: "up_to_date" },
      pending: null,
    };
  }
  const tier: UpdateTier = view.kind === "patch" ? "patch" : "update";
  if (view.eligible) {
    if (!view.handleId) return { status: { kind: "error", message: UPDATE_MESSAGES.unknown }, pending: null };
    return {
      status: { kind: "available", version: view.version, tier, eligible: true, reason: "auto_install_off" },
      pending: { handleId: view.handleId, version: view.version, tier, staged: false },
    };
  }
  switch (view.refusal) {
    case "busy":
      return { status: { kind: "error", message: UPDATE_MESSAGES.busy }, pending: null };
    case "unresolved":
      return { status: { kind: "error", message: UPDATE_MESSAGES.unresolved }, pending: null };
    case "offline":
      return { status: { kind: "error", message: UPDATE_MESSAGES.networkUnavailable }, pending: null };
    default:
      // Not entitled (no Pro licence, expired, revoked, wrong tier): reported, never installable.
      return {
        status: { kind: "available", version: view.version, tier, eligible: false, reason: "pro_required" },
        pending: null,
      };
  }
}

/**
 * The update flow over the native host (`rightkit_update_*`, @rightkit/app-update/native). The
 * host decides entitlement, authenticates the download with the licence and verifies the artifact;
 * this controller only sequences check, consent-gated download, install and relaunch, and publishes
 * status for the overlay. No token, header or URL passes through it.
 */
export function createUpdateController(dependencies: UpdateControllerDependencies): UpdateController {
  const { native, publish } = dependencies;
  let pending: PendingUpdate | null = null;
  // A newer check or a finished install supersedes the one a late progress event belongs to.
  let generation = 0;

  async function stage(update: PendingUpdate, token: number): Promise<boolean> {
    if (update.staged) return true;
    publish({ kind: "downloading", version: update.version, tier: update.tier, pct: 0 });
    const unlisten = await native.onProgress((progress) => {
      if (token !== generation || progress.handleId !== update.handleId || !progress.total) return;
      publish({
        kind: "downloading",
        version: update.version,
        tier: update.tier,
        pct: Math.floor((progress.received / progress.total) * 100),
      });
    });
    try {
      const result = await native.download(update.handleId);
      if (result.state === "downloaded") {
        update.staged = true;
        publish({ kind: "staged", version: update.version, tier: update.tier });
        return true;
      }
      publish({
        kind: "error",
        message:
          result.state === "cancelled"
            ? "Update download cancelled"
            : messageText(result.message, UPDATE_MESSAGES.updateFailed),
      });
      return false;
    } finally {
      unlisten();
    }
  }

  return {
    hasPending: () => pending !== null,

    async check(setLatestVersion, setUpdateState) {
      generation += 1;
      pending = null;
      setUpdateState("checking");
      publish({ kind: "checking" });
      try {
        const answer = await statusFromCheck(await native.check(), dependencies.proUpdateVersion);
        publish(answer.status);
        updateStateFromStatus(answer.status, setLatestVersion, setUpdateState);
        pending = answer.pending;
      } catch (error) {
        publish(errorStatus(error, UPDATE_MESSAGES.updateFailed));
        setUpdateState("failed");
      }
    },

    // Optional prepare step for the overlay "Download update" action: downloads and verifies, installs nothing.
    async download() {
      const update = pending;
      if (!update) return;
      try {
        if (!(await stage(update, generation))) pending = null;
      } catch (error) {
        pending = null;
        publish(errorStatus(error, UPDATE_MESSAGES.updateFailed));
      }
    },

    // Explicit install of the update found by check(). A staged handle installs without network;
    // otherwise the download runs first. Relaunches once the host reports the installer started.
    async install(setUpdateState) {
      const update = pending;
      if (!update) return;
      setUpdateState("installing");
      const token = generation;
      let probation = false;
      try {
        if (!(await stage(update, token))) {
          pending = null;
          setUpdateState("failed");
          return;
        }
        // Recorded before the install: the installer may end this process before it returns.
        await dependencies.beginBootProbation(update.version);
        probation = true;
        const result = await native.install(update.handleId);
        // The handle is spent or refused either way: a fresh check recaptures the licence binding.
        pending = null;
        if (result.state !== "installing") {
          await dependencies.abandonBootProbation();
          publish({
            kind: "error",
            message: messageText(result.message, UPDATE_MESSAGES.updateRefused),
          });
          setUpdateState("failed");
          return;
        }
        // The installer started: the probation record now belongs to the new version.
        probation = false;
        generation += 1;
        publish({ kind: "ready", version: update.version, tier: update.tier });
        await dependencies.relaunch();
      } catch (error) {
        pending = null;
        if (probation) await dependencies.abandonBootProbation().catch(() => undefined);
        publish(errorStatus(error, UPDATE_MESSAGES.updateFailed));
        setUpdateState("failed");
      }
    },
  };
}

let controller: UpdateController | null = null;

function defaultController(): UpdateController {
  controller ??= createUpdateController({
    native: createNativeUpdateClient({ invoke, listen }),
    proUpdateVersion,
    beginBootProbation,
    abandonBootProbation: () => bootProbation.acknowledgeHealthy(),
    relaunch: async () => {
      const { relaunch } = await import("@tauri-apps/plugin-process");
      await relaunch();
    },
    publish: publishUpdateStatus,
  });
  return controller;
}

export function hasPendingUpdate(): boolean {
  return controller?.hasPending() ?? false;
}

export function checkForUpdate(
  setLatestVersion: (version: string) => void,
  setUpdateState: (state: UpdateState) => void,
): Promise<void> {
  return defaultController().check(setLatestVersion, setUpdateState);
}

export function downloadPendingUpdate(): Promise<void> {
  return defaultController().download();
}

export function installPendingUpdate(setUpdateState: (state: UpdateState) => void): Promise<void> {
  return defaultController().install(setUpdateState);
}
