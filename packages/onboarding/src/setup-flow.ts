// Pure rules of the setup flow: which step may continue, where the model step leaves to, the "Step N of M"
// count, and the chip name of a pressed key in the shortcut preview. No React, no host names: the step ids,
// labels and wording are the host's.
import type { PermissionState } from "./types.js";
import type { MicCheckState } from "./setup-recording.js";

/** "Step N of M" over the steps the user sees; empty for an id that is not listed. `format` supplies the wording. */
export function setupStepProgressLabel(
  id: string,
  stepIds: readonly string[],
  format: (position: number, total: number) => string,
): string {
  const index = stepIds.indexOf(id);
  if (index < 0) return "";
  return format(index + 1, stepIds.length);
}

/**
 * Microphone step: Continue waits for the Keychain answer (macOS), a settled microphone request and a
 * finished microphone check.
 */
export function setupHardwareReady(input: {
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

/** Shortcuts step: macOS needs Accessibility; other platforms have nothing to grant. */
export function setupControlsReady(input: { mac: boolean; accessibilityState: PermissionState }): boolean {
  return !input.mac || input.accessibilityState === "ready";
}

/**
 * Patch that leaves the model step once its download has installed: the same state the frame's Continue saves
 * (the model step completed, current step moved to the next builtin), without the user pressing Continue.
 * `leadSteps` counts frame steps that come before the listed ones (a hidden agreement stand-in).
 */
export function setupModelAdvancePatch(
  completed: readonly string[],
  flow: { stepIds: readonly string[]; modelStepId: string; leadSteps: number },
): { completed: string[]; currentStep: number } {
  const modelIndex = flow.stepIds.indexOf(flow.modelStepId);
  return {
    completed: completed.includes(flow.modelStepId) ? [...completed] : [...completed, flow.modelStepId],
    currentStep: flow.leadSteps + modelIndex + 1,
  };
}

/**
 * The chip name of a pressed key in the shortcut preview, or null for a key that is not shown.
 * `chordNames` maps a lower-case `KeyboardEvent.key` to a chord token (`"control"` to `"ctrl"`); `format`
 * turns a token into the platform's chip names (the host's `formatChord`).
 */
export function previewChordKey(
  key: string,
  chordNames: ReadonlyMap<string, string>,
  format: (chord: string) => readonly string[],
): string | null {
  const chord = chordNames.get(key.toLowerCase());
  if (!chord) return null;
  return format(chord)[0] ?? null;
}
