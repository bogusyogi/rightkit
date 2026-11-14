// @rightkit/dictation-ui/onboarding: the first-run setup of a dictation app over the host's `DictationClient`
// (`onboarding`, `speech`, `license` groups and the accessibility members): the permission, microphone,
// shortcut, recognition-check and trial hooks, and the pure rules that order and gate the steps.

export * from "./chord.js";
export * from "./client.js";
export * from "./constants.js";
export * from "./flow.js";
export * from "./helpers.js";
export * from "./model.js";
export * from "./platform.js";
export * from "./pttKey.js";
export * from "./types.js";
export { useOnboardingPermissions } from "./usePermissions.js";
export { useOnboardingRecording } from "./useRecording.js";
export { useOnboardingRecognitionSetup, type OnboardingRecognitionSetupController } from "./useRecognitionSetup.js";
export { useOnboardingTrial } from "./useTrial.js";

// Steps
export { SectionIntro, FeatureIcon } from "./Chrome.js";
export { PermissionBadge, ShortcutCard, ControlCard } from "./Controls.js";
export { MicSetupStep } from "./MicSetupStep.js";
export { ControlsStep } from "./ControlsStep.js";
export { MicMeter, IdlePillPreview } from "./PillPreview.js";
export { ModelIntro, ModelDownloadExtras, type ModelStateSource } from "./ModelStep.js";
export { RecognitionSetupStep } from "./RecognitionSetupStep.js";
export { WelcomeStep, CommandsStep, AppLivesStep } from "./StaticSteps.js";
export { TrialStep } from "./TrialStep.js";
export { DictationOnboarding, type DictationOnboardingProps } from "./DictationOnboarding.js";
