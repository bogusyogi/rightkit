import type { ReactNode } from "react";
import type { LegalGateDocument, LegalGateProps, LegalGateTheme } from "@rightkit/legal-ui";

export type SettingsValues = Record<string, unknown>;

export interface SettingsSnapshot {
  values: SettingsValues;
  revision: number;
}

/** Structural port shared with @rightkit/settings-ui and rightkit-app-settings. */
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

/**
 * App-owned persistence for the legal acceptance. Lets an app keep its existing receipt (any shape, any store) and map it
 * to and from the package's record. When supplied it is the source of truth: `read()` is consulted on load and overrides
 * whatever acceptance the settings backend holds (`null` means "not accepted", so the gate shows), and `write()` is
 * awaited when the user agrees, before the settings backend is patched.
 */
export interface LegalReceiptPort {
  read(): LegalAcceptanceRecord | null | Promise<LegalAcceptanceRecord | null>;
  write(record: LegalAcceptanceRecord): void | Promise<void>;
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
  /**
   * Hash the stored acceptance must carry. A stored acceptance with a different (or no) hash re-prompts even when
   * the version is unchanged, so a text change without a version bump is caught. Pass the hash of the MATERIAL
   * documents only (see `@rightkit/legal`), not of appendices that regenerate on dependency bumps.
   */
  requiredDocumentsHash?: string;
  /** Hash of the legal documents recorded with the acceptance. Default: `requiredDocumentsHash`, else a hash of document titles and URLs. Recorded only; never compared. */
  documentsHash?: string;
  /** Runs (and is awaited) before the legal gate is shown, e.g. to resize or reveal the window. */
  beforeLegalGate?: () => void | Promise<void>;
  /** Require at least one downloaded model before Continue/Finish on the models step. Default false. */
  requireModel?: boolean;
  /** Start downloading the first missing model when the models step opens (skipped when any model is installed or downloading). Default false. */
  autoStartDownload?: boolean;
  /** Choose which missing model starts when the models step first opens: the first `state === "missing"` entry for which this returns true. Replaces the `autoStartDownload` rule (no "something is installed" guard; the app decides). Fires at most once per `Onboarding` mount, across re-renders and Back/Next. */
  shouldAutoStart?: (entry: ModelEntry, models: readonly ModelEntry[]) => boolean;
  /** Full control: called once, when the models step first opens and the list has loaded. Replaces `shouldAutoStart` and `autoStartDownload`. Start models through `ctx.start`. Fires at most once per `Onboarding` mount; a throw is shown in the step. */
  onModelsStepEnter?: (ctx: ModelsStepEnterContext) => void | Promise<void>;
  /** App copy shown at the top of the models step. */
  modelsIntro?: ReactNode;
  /** Show "Set up later". Default true. */
  allowSkip?: boolean;
  /** Optional titlebar content rendered in a strip along the top. */
  titlebar?: ReactNode;
  /** Render a `data-tauri-drag-region` strip at the top of the setup steps. Default false. Never applied to the legal stage; see `legalDragRegion`. */
  dragRegion?: boolean;
  /** Also render the drag region on the legal stage (loading and gate). Default false: the legal gate has no strip. */
  legalDragRegion?: boolean;
  /** Read and write the legal acceptance in the app's own receipt instead of (overriding) the settings backend's copy. */
  legalReceipt?: LegalReceiptPort;
}

export type { LegalGateDocument, LegalGateTheme };

/** Context handed to `onModelsStepEnter`. */
export interface ModelsStepEnterContext {
  /** The model list as loaded when the step opened. */
  models: readonly ModelEntry[];
  /** Start installing one model, then refresh the list. */
  start(id: string): Promise<void>;
  /** Re-read the list from the backend (and update the step). */
  refresh(): Promise<ModelEntry[]>;
}

/** The auto-start options shared by `Onboarding` and `modelsStep`. */
export interface ModelsAutoStart {
  autoStartDownload?: boolean;
  shouldAutoStart?: (entry: ModelEntry, models: readonly ModelEntry[]) => boolean;
  onModelsStepEnter?: (ctx: ModelsStepEnterContext) => void | Promise<void>;
}
