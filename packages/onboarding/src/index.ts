export { Onboarding, isLegalAccepted, isStepSettled, onboardingStateKey, restartOnboardingSetup } from "./Onboarding.js";
/** Parses the stored onboarding receipt, returning undefined for malformed input. Check freshness separately with isLegalAccepted. */
export { readAcceptance as readLegalAcceptance } from "./Onboarding.js";
export { licenseStep, modelsStep, permissionsStep, signInStep } from "./steps.js";
export type { LicenseStepCopy, LicenseStepOptions, ModelsStepOptions, ModelsStepView } from "./steps.js";
export { nextEntryProgress, trackModelInstall } from "./model-progress.js";
export { createInvokeRecognitionCheckPort, useRecognitionCheck } from "./recognition-check.js";
export type {
  InvokeRecognitionCheckPortOptions,
  RecognitionCheckController,
  RecognitionCheckPort,
  RecognitionCheckProgress,
  RecognitionCheckResult,
  RecognitionCheckStart,
  UseRecognitionCheckOptions,
} from "./recognition-check.js";
export { useTrialActivation } from "./trial-activation.js";
export type {
  TrialActivationMessages,
  TrialActivationPort,
  TrialPhase,
  UseTrialActivationOptions,
} from "./trial-activation.js";
export { licenseBackendFromView, licenseStateFromView, signInBackendFromOAuth } from "./adapters.js";
export type { LicenseViewBackendOptions, LicenseViewClient, LicenseViewLike, OAuthSignInPort } from "./adapters.js";
export { LEGAL_RECEIPT_COMMANDS, createNativeLegalReceiptPort, reopenLegalAcceptance, resetLegalAcceptance } from "./legal-receipt.js";
export type { LegalReceiptInvoke, NativeLegalReceiptPortOptions } from "./legal-receipt.js";
export type * from "./types.js";
export { useOnboardingPermissions, isWindowsMicStatus, windowsMicPermissionState } from "./permissions-flow.js";
export type {
  PermissionBusy,
  PermissionsCopy,
  PermissionsPort,
  UseOnboardingPermissionsArgs,
  WindowsMicStatus,
} from "./permissions-flow.js";
export { useSetupRecording } from "./setup-recording.js";
export type {
  MicCheckState,
  SetupRecordingCopy,
  SetupRecordingEvents,
  SetupRecordingPort,
  ShortcutKind,
  UseSetupRecordingArgs,
} from "./setup-recording.js";
export {
  previewChordKey,
  setupControlsReady,
  setupHardwareReady,
  setupModelAdvancePatch,
  setupStepProgressLabel,
} from "./setup-flow.js";
export { useAccessibilityWatch } from "./accessibility-watch.js";
export type { AccessibilityWatchPort, UseAccessibilityWatchOptions } from "./accessibility-watch.js";
export { clampMicLevel, micMeterActiveBars, micMeterBarHeight, useMicLevel } from "./mic-meter.js";
export type { MicLevelSubscribe, MicMeterBarShape } from "./mic-meter.js";
export { platformPttMatcher, resolvePttKey } from "./ptt.js";
export type { PlatformPttMatchers, ResolvePttKeyConfig } from "./ptt.js";
export {
  activateInstalledModel,
  createModelAutoAdvance,
  createSingleModelBackend,
  modelEntryFromEvent,
  modelEntryFromStatus,
} from "./model-backend.js";
export type {
  InvokeModelsBackendOptions,
  ModelActivationOptions,
  ModelDownloadEvent,
  SingleModelConfig,
} from "./model-backend.js";
export {
  DEFAULT_LEGAL_SETTINGS_FIELDS,
  createReAcceptanceHandlers,
  createSettingsLegalReceiptPort,
  legalAcceptanceCurrentFromSettings,
} from "./legal-settings.js";
export type {
  LegalContract,
  LegalSettingsFields,
  LegalSettingsOptions,
  ReAcceptanceHandlersOptions,
  SettingsLegalReceiptPortOptions,
} from "./legal-settings.js";
export { useStepReady } from "./ready-sync.js";
