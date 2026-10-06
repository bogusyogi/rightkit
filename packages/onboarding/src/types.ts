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
  cancel(id: string): Promise<void>;
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

export interface PersistedOnboardingState {
  version: 1;
  currentStep: number;
  completed: string[];
  skipped: boolean;
  legalAccepted: boolean;
  eligibilityBasis?: "individual" | "enterprise";
}

export interface OnboardingStepContext {
  state: PersistedOnboardingState;
  settings: SettingsBackend;
  setState: (next: Partial<PersistedOnboardingState>) => Promise<void>;
  permissions?: PermissionsBackend;
  models?: ModelsBackend;
  license?: LicenseBackend;
}

export interface OnboardingStep {
  id: string;
  label: string;
  title: string;
  description?: string;
  skippable?: boolean;
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
}

export type { LegalGateDocument, LegalGateTheme };
