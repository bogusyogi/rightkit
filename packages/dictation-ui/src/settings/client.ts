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

/** The account session. `expiresAt` is a Unix-seconds display estimate. */
export type AccountSessionStatus = { signedIn: boolean; email?: string | null; expiresAt?: number | null };

/**
 * The account session and its devices. Rejections carry the wording to show (an `Error` with the message). The
 * device section needs `sessionStatus`, `devices`, `renameDevice` and `removeDevice`; the account section also
 * needs the members marked optional here and `manageUrl`.
 */
export interface AccountClient {
  sessionStatus(): Promise<AccountSessionStatus>;
  devices(): Promise<AccountDeviceRow[]>;
  renameDevice(id: string, label: string): Promise<void>;
  removeDevice(id: string): Promise<void>;
  /** Re-validates the session with the account service. */
  refresh?(): Promise<AccountSessionStatus>;
  /** Starts the passwordless sign-in: the service e-mails a one-time link. */
  beginMagicLink?(email: string): Promise<void>;
  /** Completes sign-in with the token of the e-mailed link. */
  complete?(token: string): Promise<AccountSessionStatus>;
  signOut?(): Promise<void>;
  /** Permanently deletes the account; resolves only when the service said it is deleted. */
  deleteAccount?(): Promise<void>;
  /** The account's web page (a full URL): the "Manage on the web" link, and its host and path in the sign-in text. */
  manageUrl?: string;
}

/** The credential-store state of the licence (`{ kind: "error" }` shows the storage banner). */
export type LicenseStorageState = { kind: "ready" } | { kind: "error"; operation: string; message: string };

/** The part of a licence view the section reads: `status === "active"` is the only activation success. */
export type LicenseViewLike = { status: string; message?: unknown };

/** The licence lifecycle as a snapshot: `checking` until the first read settles. */
export type LicenseLifecycleLike = { kind: string; isPro: boolean };

/**
 * The licence, as the License section follows it. `@rightkit/license`'s `createAppLicense` result has these members
 * (`...appLicense`); the host adds `words`, `keyPlaceholder` and `trialDays`.
 */
export interface LicenseSettingsClient {
  /** The licence as it stands (native, no network). */
  loadLicense(): Promise<LicenseViewLike>;
  /** Activates a key; a rejection carries the wording to show. */
  activateLicense(key: string): Promise<LicenseViewLike>;
  /** Re-reads after the user repaired the credential store. */
  retryLicenseStorage(): Promise<LicenseViewLike>;
  /** Removes this device's licence. */
  removeLicense(): Promise<void>;
  /** Requests a trial key by e-mail; false on any rejection. */
  requestTrialKey(email: string): Promise<boolean>;
  /** Link code from this already-leased device; `expiresAt` is Unix milliseconds. */
  requestWindowsLinkCode(): Promise<{ code: string; expiresAt: number }>;
  /** Redeems a link code on this device. */
  redeemWindowsLinkCode(code: string): Promise<void>;
  storage: { get(): LicenseStorageState; subscribe(listener: (state: LicenseStorageState) => void): () => void };
  monitor: { get(): LicenseLifecycleLike; subscribe(listener: (state: LicenseLifecycleLike) => void): () => void };
  /** The display Pro flag while the lifecycle is still `checking`. */
  isPro(view: LicenseViewLike | null): boolean;
  /** True when the view alone shows Pro. */
  viewShowsPro(view: LicenseViewLike | null): boolean;
  /** Normalises the key input (trim, upper-case). */
  formatLicenseKey(raw: string): string;
  /** The words for a licence message code. */
  licenseText(code: unknown): string;
  /** Rewords an error text for display (for example to US spelling). Default: unchanged. */
  words?(text: string): string;
  /** The key field's placeholder, for example `SR-APPNAME-…`. */
  keyPlaceholder: string;
  /** Length of the free trial, in days. */
  trialDays: number;
}

/** The client group a section needs, or an error naming the missing group. */
export function sectionClient<K extends "app" | "speech" | "developer" | "sync" | "account" | "license">(
  group: K,
): NonNullable<DictationClient[K]> {
  const found = dictationClient()[group];
  if (!found) {
    throw new Error(`@rightkit/dictation-ui: the DictationClient has no \`${group}\` group (needed by the settings sections)`);
  }
  return found as NonNullable<DictationClient[K]>;
}
