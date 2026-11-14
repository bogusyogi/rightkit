// The native calls and events the first-run setup needs, as the `DictationClient.onboarding` group.
// Accessibility, hotkey registration and the model download reuse the client's existing members.
import { dictationClient, type Subscribe } from "../host.js";

/** Result of starting the recognition check; `minSeconds` and `targetSeconds` size the reading. */
export interface RecognitionSetupStart {
  sessionId: string;
  minSeconds: number;
  targetSeconds: number;
}

export interface RecognitionSetupResult {
  status: "passed" | "failed" | "capture_failed";
  message: string;
}

export interface RecognitionSetupProgress {
  sessionId: string;
  phase: "recording" | "captured" | "testing_route" | "complete" | "failed";
  message?: string;
}

/** Events of the setup. The host maps each to its native event. */
export interface OnboardingEventMap {
  /** Live microphone level, 0 to 1, while the microphone check runs. */
  micLevel: { level?: number };
  micMeterError: { message?: string };
  /** `reopening`, `recovered`, `verified`, `no_signal` or `unavailable`. */
  micMeterStatus: { status?: string };
  /** A shortcut was pressed on the shortcuts step. */
  shortcut: { kind?: string };
  fn: { down?: boolean };
  /** The push-to-talk key went down or up. */
  ptt: { down?: boolean };
  middleClick: unknown;
  /** Windows: the microphone consent pane granted access. */
  microphonePermissionGranted: unknown;
  recognitionSetupProgress: RecognitionSetupProgress;
}

/** What the first-run setup needs from the host, beyond the client's accessibility, hotkey and speech members. */
export interface OnboardingClient {
  on: Subscribe<OnboardingEventMap>;

  /** Microphone access: macOS `authorized`, `denied`, `restricted`; Windows one of eight statuses. */
  microphoneAccessStatus(): Promise<string>;
  requestMicrophoneAccess(): Promise<string>;
  /** macOS only. */
  openMicrophoneSettings(): Promise<void>;

  /** Whether the local database key is already stored; never creates it. */
  databaseKeyPresent(): Promise<boolean>;
  /** Creates (or reads) the local database key; the user's "Allow" click. */
  ensureDatabaseKey(): Promise<void>;

  /** `deviceSetting` is the stored input-device choice (undefined: system default). */
  startMicMeter(deviceSetting: string | undefined): Promise<void>;
  stopMicMeter(): Promise<void>;

  /** Turns the setup's shortcut preview on or off; `previewOnly` is omitted when the call does not carry it. */
  setShortcutsActive(active: boolean, previewOnly?: boolean): Promise<void>;

  startRecognitionSetup(): Promise<RecognitionSetupStart>;
  stopRecognitionSetup(sessionId: string): Promise<RecognitionSetupResult>;
  cancelRecognitionSetup(sessionId: string): Promise<void>;

  /** The final settings write of the setup. */
  applySettingsPatch(patch: Record<string, unknown>): Promise<unknown>;
  revealPill(): Promise<void>;
  /** Hides the hub window once the shortcuts are registered. */
  parkIdleHub(): Promise<void>;
  /** The hub opened at the agreement's size; resize it to the surface now showing. */
  resizeHubForSurface(): Promise<void>;
}

/** The `onboarding` client group, or an error naming it. */
export function onboardingClient(): OnboardingClient {
  const found = dictationClient().onboarding;
  if (!found) {
    throw new Error("@rightkit/dictation-ui: the DictationClient has no `onboarding` group (needed by the onboarding entry)");
  }
  return found;
}

/** The macOS accessibility members of the client, which setup requires. */
export function accessibilityClient() {
  const client = dictationClient();
  const { accessibilityGranted, activateShortcuts, openAccessibilitySettings } = client;
  if (!accessibilityGranted || !activateShortcuts || !openAccessibilitySettings) {
    throw new Error(
      "@rightkit/dictation-ui: the DictationClient needs accessibilityGranted, activateShortcuts and openAccessibilitySettings for the onboarding entry",
    );
  }
  return {
    granted: accessibilityGranted.bind(client),
    activate: activateShortcuts.bind(client),
    openSettings: openAccessibilitySettings.bind(client),
  };
}
