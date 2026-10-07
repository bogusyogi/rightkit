import type { ReactNode } from "react";
import type { LegalGateDocument, LegalGateProps, LegalGateTheme } from "@rightkit/legal-ui";

export type SettingsValues = Record<string, unknown>;

export interface SettingsSnapshot {
  values: SettingsValues;
  revision: number;
}

/** Structural port shared with @rightkit/settings-ui and rightkit-settings. */
export interface SettingsBackend {
  load(): Promise<SettingsSnapshot>;
  patch(patch: SettingsValues, expectedRevision?: number): Promise<SettingsSnapshot>;
  subscribe?(handler: (keys: string[]) => void): Promise<() => void>;
}

export type PermissionState = "ready" | "needed" | "waiting" | "denied" | "restricted";

export interface PermissionStatus {
  id: string;
  label: string;
  description?: string;
  state: PermissionState;
  detail?: string;
}

export interface PermissionsBackend {
  list(): Promise<PermissionStatus[]>;
  request(id: string): Promise<PermissionStatus>;
}

export type ModelState = "missing" | "installed" | "downloading" | "verifying" | "error";

export interface ModelEntry {
  id: string;
  label: string;
  description?: string;
  sizeBytes?: number;
  state: ModelState;
  progress?: number;
  error?: { code: string; message: string };
}

export interface ModelsBackend {
  list(): Promise<ModelEntry[]>;
  install(id: string): Promise<void>;
  /** Optional. The cancel/pause control is shown only when the backend supplies this. */
  cancel?(id: string): Promise<void>;
  subscribe?(handler: (entry: ModelEntry) => void): Promise<() => void>;
}

export interface LicenseState {
  status: "none" | "trial" | "active" | "expired" | "invalid";
  label?: string;
  detail?: string;
  tier?: string;
  validUntil?: string | null;
}

export interface LicenseBackend {
  load(): Promise<LicenseState>;
  activate?(key: string): Promise<LicenseState>;
  startTrial?(): Promise<LicenseState>;
  purchaseUrl?: string;
  openExternal?: (url: string) => void | Promise<void>;
}

export type EligibilityBasis = "individual" | "enterprise";

/** Full record of one legal acceptance, persisted so version changes can re-prompt. */
export interface LegalAcceptanceRecord {
  version: string;
  documentsHash: string;
  /** UTC ISO-8601 timestamp (`Date#toISOString`). */
  acceptedAt: string;
  eligibilityBasis: EligibilityBasis;
}

export interface PersistedOnboardingState {
  version: 1;
  currentStep: number;
  completed: string[];
  skipped: boolean;
  /** Legacy boolean, still written for older readers. Prefer `legalAcceptance`. */
  legalAccepted: boolean;
  /** Legacy mirror of `legalAcceptance.eligibilityBasis`. */
  eligibilityBasis?: EligibilityBasis;
  legalAcceptance?: LegalAcceptanceRecord;
}

export interface OnboardingStepContext {
  state: PersistedOnboardingState;
  settings: SettingsBackend;
  setState: (next: Partial<PersistedOnboardingState>) => Promise<void>;
  permissions?: PermissionsBackend;
  models?: ModelsBackend;
  license?: LicenseBackend;
  /** Readiness reported by steps via `setReady`, keyed by step id. */
  ready: Readonly<Record<string, boolean>>;
  /** Reports whether the active step is ready (used by `canContinue`). Stable per step. */
  setReady: (ready: boolean) => void;
}

export interface OnboardingStep {
  id: string;
  label: string;
  title: string;
  description?: string;
  skippable?: boolean;
  /** Continue/Finish stay disabled until this returns true. Default: always allowed. */
  canContinue?: (context: OnboardingStepContext) => boolean;
  /** Shown beside the disabled Continue/Finish button. */
  blockedHint?: string;
  render: (context: OnboardingStepContext) => ReactNode;
}

export interface LegalStepConfig extends Omit<LegalGateProps, "variant" | "onAgree" | "onQuit"> {
  onQuit?: () => void;
}

export interface OnboardingProps {
  settings: SettingsBackend;
  legal: LegalStepConfig;
  /** App-specific steps are appended after built-in steps, in supplied order. */
  steps?: readonly OnboardingStep[];
  permissions?: PermissionsBackend;
  models?: ModelsBackend;
  license?: LicenseBackend;
  onComplete: () => void | Promise<void>;
  onSkip?: () => void | Promise<void>;
  className?: string;
  /** Version the user must have accepted. A different (or missing) stored version re-prompts with `variant: "re-acceptance"`. */
  requiredVersion?: string;
  /** Custom acceptance check; takes precedence over `requiredVersion`. */
  isAccepted?: (state: PersistedOnboardingState) => boolean;
  /** Hash of the legal documents recorded with the acceptance. Default: hash of document titles and URLs. */
  documentsHash?: string;
  /** Runs (and is awaited) before the legal gate is shown, e.g. to resize or reveal the window. */
  beforeLegalGate?: () => void | Promise<void>;
  /** Require at least one downloaded model before Continue/Finish on the models step. Default false. */
  requireModel?: boolean;
  /** Start downloading the first missing model when the models step opens. Default false. */
  autoStartDownload?: boolean;
  /** App copy shown at the top of the models step. */
  modelsIntro?: ReactNode;
  /** Show "Set up later". Default true. */
  allowSkip?: boolean;
  /** Optional titlebar content rendered in a strip along the top. */
  titlebar?: ReactNode;
  /** Render a `data-tauri-drag-region` strip at the top. Default false. */
  dragRegion?: boolean;
}

export type { LegalGateDocument, LegalGateTheme };
