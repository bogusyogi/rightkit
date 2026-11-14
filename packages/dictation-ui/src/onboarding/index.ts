// @rightkit/dictation-ui/onboarding: the first-run setup of a dictation app over the host's `DictationClient`
// (`onboarding`, `speech`, `license` groups and the accessibility members): the permission, microphone,
// shortcut, recognition-check and trial hooks, and the pure rules that order and gate the steps.

export * from "./client.js";
export * from "./flow.js";
export * from "./helpers.js";
export * from "./platform.js";
export * from "./pttKey.js";
export * from "./types.js";
export { useOnboardingPermissions } from "./usePermissions.js";
export { useOnboardingRecording } from "./useRecording.js";
export { useOnboardingRecognitionSetup, type OnboardingRecognitionSetupController } from "./useRecognitionSetup.js";
export { useOnboardingTrial } from "./useTrial.js";

