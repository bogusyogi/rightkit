// Host contract of the settings sections (`@rightkit/dictation-ui/settings`): one optional group per
// area on `DictationClient`, one method per native operation. A group is only needed by the sections
// that use it; a section whose group is missing fails with a message naming it (`sectionClient`).
import { dictationClient, type DictationClient } from "../host.js";

export type SyncProviderChoice = "icloud" | "google" | "off";

/** The sections' picks of the native dialog and OS-level app behaviour. */
export interface AppSettingsClient {
  /** Whether the OS starts the app at login. */
  launchAtLoginEnabled(): Promise<boolean>;
  setLaunchAtLogin(enabled: boolean): Promise<void>;
  /** Re-encrypts the local history with a new key. */
  rotateHistoryKey(): Promise<void>;
  /** A native yes/no dialog. */
  confirm(message: string, options: { title: string; kind: "info" | "warning" | "error" }): Promise<boolean>;
}

export type LocalBackend = "parakeet-unified" | "parakeet-tdt";
export type LocalAsrPhase = "" | "downloading" | "extracting" | "preparing";
export type WhisperModelStatus = { installed: boolean; prepared: boolean };
export type LocalAsrModelStatus = {
  backend: LocalBackend;
  label: string;
  installed: boolean;
  source: "app-data" | "missing";
  replacement_required: boolean;
};

/** Local speech models: status, download, warm-up. Progress arrives as the `localAsrModel` and `whisperModel` events. */
export interface SpeechClient {
  localAsrModelStatus(backend: LocalBackend): Promise<LocalAsrModelStatus>;
  downloadLocalAsrModel(backend: LocalBackend): Promise<LocalAsrModelStatus>;
  whisperModelStatus(): Promise<WhisperModelStatus>;
  downloadWhisperModel(): Promise<void>;
  /** Loads the active engine so the next recording starts at once. */
  prewarmEngine(): Promise<void>;
  /** "Later" on the model-update prompt: the host asks again on its next launch. */
  dismissModelUpdate(): Promise<void>;
}

export interface WakeLabStatus {
  marker_present: boolean;
  enabled: boolean;
  threshold: number;
}

/** Developer tools: diagnostics, experiments. */
export interface DeveloperClient {
  wakeLabStatus(): Promise<WakeLabStatus>;
  setWakeLabEnabled(enabled: boolean): Promise<void>;
  /** Whether recordings saved by "Save recording audio locally" exist. */
  diagnosticCaptureAudioPresent(): Promise<boolean>;
  openDiagnosticCapturesFolder(): Promise<string>;
  /** macOS only. */
  recompileCoreMlCache(): Promise<void>;
  /** Resolves the report id. */
  sendDiagnostics(): Promise<string>;
  /** Resolves where the archive was saved. */
  exportDiagnostics(includeNativeDumps: boolean): Promise<string>;
}

export type CloudSyncStatus =
  | { state: "disabled" }
  | { state: "unavailable"; detail?: string | null }
  | { state: "ready" };

export type ProResolutionStatus = "off" | "blocked_no_keychain" | "ok";

/** Apple-device Pro continuity, as the native status reports it (the same shape as `@rightkit/license`'s `ProBridgeStatus`). */
export interface ProBridgeStatus {
  syncEnabled?: boolean;
  keychainSyncEnabled: boolean;
  proResolutionEnabled: boolean;
  keychainReady: boolean;
  keychainDetail?: string | null;
  consumptionSuppressed: boolean;
  proResolutionStatus: ProResolutionStatus;
  supported?: boolean;
}

/** Sync with other devices: the provider choice and the three Apple channels (transcripts, Keychain, Pro resolution). */
export interface SyncClient {
  /** The stored provider; null or undefined when none is set. */
  syncProvider(): Promise<SyncProviderChoice | null | undefined>;
  setSyncProvider(provider: SyncProviderChoice): Promise<void>;
  cloudSyncStatus(): Promise<CloudSyncStatus>;
  setCloudSyncEnabled(enabled: boolean): Promise<void>;
  setKeychainSyncEnabled(enabled: boolean): Promise<void>;
  setProResolutionEnabled(enabled: boolean): Promise<void>;
  proBridgeStatus(): Promise<ProBridgeStatus>;
}

/** One device leased to the signed-in account. `state` is open: unknown values render literally. */
export type AccountDeviceRow = {
  id: string;
  licenseSuffix: string | null;
  appKey: string;
  platform: string | null;
  pool: string | null;
  label: string | null;
  state: string;
  /** Unix milliseconds. */
  lastSeenAt: number | null;
  activatedAt: number | null;
  generation: number;
};

/** The account session and its devices. Rejections carry the wording to show (an `Error` with the message). */
export interface AccountClient {
  sessionStatus(): Promise<{ signedIn: boolean }>;
  devices(): Promise<AccountDeviceRow[]>;
  renameDevice(id: string, label: string): Promise<void>;
  removeDevice(id: string): Promise<void>;
}

/** The client group a section needs, or an error naming the missing group. */
export function sectionClient<K extends "app" | "speech" | "developer" | "sync" | "account">(
  group: K,
): NonNullable<DictationClient[K]> {
  const found = dictationClient()[group];
  if (!found) {
    throw new Error(`@rightkit/dictation-ui: the DictationClient has no \`${group}\` group (needed by the settings sections)`);
  }
  return found as NonNullable<DictationClient[K]>;
}
