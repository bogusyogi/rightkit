export { Onboarding, isLegalAccepted, isStepSettled, onboardingStateKey, restartOnboardingSetup } from "./Onboarding.js";
/** Parses the stored onboarding receipt, returning undefined for malformed input. Check freshness separately with isLegalAccepted. */
export { readAcceptance as readLegalAcceptance } from "./Onboarding.js";
export { licenseStep, modelsStep, permissionsStep, signInStep } from "./steps.js";
export type { LicenseStepCopy, LicenseStepOptions, ModelsStepOptions } from "./steps.js";
export { licenseBackendFromView, licenseStateFromView, signInBackendFromOAuth } from "./adapters.js";
export type { LicenseViewBackendOptions, LicenseViewClient, LicenseViewLike, OAuthSignInPort } from "./adapters.js";
export { LEGAL_RECEIPT_COMMANDS, createNativeLegalReceiptPort, reopenLegalAcceptance, resetLegalAcceptance } from "./legal-receipt.js";
export type { LegalReceiptInvoke, NativeLegalReceiptPortOptions } from "./legal-receipt.js";
export type * from "./types.js";
