// Pure onboarding rules, kept free of React and Tauri.
import type { MicCheckState, PermissionState } from "./types.js";

/** The setup steps the user sees, in order. The model step's id is the one RightKit's `modelsStep` uses. */
export const ONBOARDING_STEPS = [
  ["welcome", "Welcome"],
  ["models", "Model"],
  ["hardware", "Microphone"],
  ["controls", "Shortcuts"],
  ["commands", "Commands"],
  ["trial", "Pro trial"],
  ["app-lives", "App lives"],
  ["recognition", "Voice check"],
] as const;

export const MODEL_STEP_ID = "models";

/** The RightKit frame's hidden Agreement stand-in occupies builtin index 0 in gate mode. */
export const FRAME_LEAD_STEPS = 1;

/**
 * "Step N of M" over the steps the user sees. The frame's own eyebrow counts the hidden Agreement stand-in
 * ("Step 2 of 9" on Welcome), so the app hides that eyebrow and draws this label as the step description.
 */
export function stepProgressLabel(id: string): string {
  const index = ONBOARDING_STEPS.findIndex(([stepId]) => stepId === id);
  if (index < 0) return "";
  return `Step ${index + 1} of ${ONBOARDING_STEPS.length}`;
}

/** Microphone step: Continue waits for the Keychain answer (macOS), a settled mic request and a finished mic check. */
export function hardwareReady(input: {
  permissionBusy: string | null;
  keychainState: PermissionState;
  micState: PermissionState;
  micCheckState: MicCheckState;
}): boolean {
  return (
    input.permissionBusy !== "mic" &&
    input.keychainState === "ready" &&
    !(input.micState === "ready" && input.micCheckState === "listening")
  );
}

/** Shortcuts step: macOS needs Accessibility; Windows has nothing to grant. */
export function controlsReady(input: { mac: boolean; accessibilityState: PermissionState }): boolean {
  return !input.mac || input.accessibilityState === "ready";
}

/**
 * The hint beside a disabled Continue. The frame takes one static string per step, so it has to be true in every
 * blocked state: on the Microphone step Keychain can already be allowed while the mic check still runs.
 */
export function blockedHintFor(platform: string, id: string): string | undefined {
  if (platform !== "macos") return undefined;
  if (id === "hardware") return "Allow Keychain above, then wait for the microphone check to finish.";
  if (id === "controls") return "Turn on Accessibility above to continue.";
  return undefined;
}

/** Steps whose Continue is held until the step reports ready. */
export function stepHoldsContinue(id: string): boolean {
  return id === "hardware" || id === "controls" || id === "recognition";
}

type ModelProgressEvent = { phase: string; received: number; total: number };

/**
 * The model row's progress bar only moves forward. Verify and install report no byte total, so they hold the
 * download's place (or finish it) instead of dropping to 0; an error resets it for the retry.
 */
export function nextModelProgress(last: number, event: ModelProgressEvent): number {
  if (event.phase === "error") return 0;
  if (event.total > 0) return Math.max(last, event.received / event.total);
  const verifying = event.phase === "extracting" || event.phase === "verifying";
  if (verifying && last > 0) return 1;
  return last;
}

/**
 * Patch that leaves the model step once its download has installed: the same state the frame's Continue saves
 * (the model step completed, current step moved to the next builtin), without the user pressing Continue.
 */
export function modelAdvancePatch(completed: readonly string[]): { completed: string[]; currentStep: number } {
  const modelIndex = ONBOARDING_STEPS.findIndex(([id]) => id === MODEL_STEP_ID);
  return {
    completed: completed.includes(MODEL_STEP_ID) ? [...completed] : [...completed, MODEL_STEP_ID],
    currentStep: FRAME_LEAD_STEPS + modelIndex + 1,
  };
}

/**
 * Model step auto-advance: it fires only when the model goes from not installed to installed while the step is
 * mounted (a download that finished here). A model already installed when the step opens (relaunch, Back to the
 * step) never advances by itself; "unknown" is the state before the first read and decides nothing.
 */
export function trackModelInstall(
  sawNotInstalled: boolean,
  state: string,
): { sawNotInstalled: boolean; advance: boolean } {
  if (state === "unknown") return { sawNotInstalled, advance: false };
  if (state === "installed") return { sawNotInstalled, advance: sawNotInstalled };
  return { sawNotInstalled: true, advance: false };
}
