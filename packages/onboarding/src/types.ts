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

export type SignInProvider = "google" | "apple" | "oauth";

/**
 * Sign-in owned by `rightkit-oauth`. Every provider opens the user's DEFAULT browser (never an in-app webview, so existing
 * browser sessions are kept); `start` must call the shared owner (`OAuthClient::sign_in_with_browser` through the app's
 * command), not open a window of its own. Orthic-Labs/rightkit#279.
 */
export interface SignInBackend {
  providers: readonly { id: SignInProvider; label?: string }[];
  /** Account already signed in, if any. */
  load(): Promise<{ signedIn: boolean; account?: string }>;
  /** Open the default browser for `provider` and resolve when the sign-in finishes (or reject / throw on failure or cancel). */
  start(provider: SignInProvider): Promise<{ signedIn: boolean; account?: string }>;
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

/** One document of the set the user was shown. Shaped so `@rightkit/legal`'s `acceptanceDeclaration(manifest).documents` fits as is. */
export interface DeclaredLegalDocument {
  id?: string;
  role?: string;
  title?: string;
  url?: string;
  version?: string;
  sha256?: string;
  treatment?: string;
  material?: boolean;
}

/**
 * What the renderer declares to a native receipt service when the user agrees. It carries no time: the service stamps its
 * own UTC `acceptedAt`, compares `version`, `documentsHash` and `documents` with the manifest it embeds, and refuses a
 * declaration that does not match.
 */
export interface LegalReceiptCreateRequest {
  /** Acceptance version the app requires (`requiredVersion`, else "1"). */
  version: string;
  /** Digest of the document set shown (`documentsHash`, else `requiredDocumentsHash`, else a hash of titles and URLs). */
  documentsHash: string;
  /** The declared set (`receiptDocuments`, else the gate's documents as `{ title, url }`). */
  documents: readonly DeclaredLegalDocument[];
  eligibilityBasis: EligibilityBasis;
  /** Always "first_run": onboarding is the first-run and re-acceptance surface. */
  surface: "first_run";
}

/**
 * App-owned persistence for the legal acceptance. Lets an app keep its existing receipt (any shape, any store) and map it
 * to and from the package's record. When supplied it is the source of truth: `read()` is consulted on load and overrides
 * whatever acceptance the settings backend holds (`null` means "not accepted", so the gate shows).
 *
 * Recording the acceptance, in order of preference:
 * - `create(request)`: the NATIVE path. The service owns the time and the stored receipt and returns the record it stored;
 *   the renderer builds nothing. A throw, or a record that does not match the required version or hash, keeps the gate
 *   closed. When `create` exists, `write` is not called.
 * - `write(record)`: LEGACY. The renderer builds the record, including its `acceptedAt` from the webview clock, and the app
 *   stores it. Kept for apps that have not moved to a native receipt service.
 *
 * `reset()` clears the stored receipt (used by `resetLegalAcceptance`). A port needs `create` or `write`; with neither,
 * agreeing fails and the gate stays closed.
 */
export interface LegalReceiptPort {
  read(): LegalAcceptanceRecord | null | Promise<LegalAcceptanceRecord | null>;
  /** Native receipt creation: returns the stored record, stamped with native time. Preferred over `write`. */
  create?(request: LegalReceiptCreateRequest): LegalAcceptanceRecord | Promise<LegalAcceptanceRecord>;
  /** @deprecated Renderer-built record with webview time. Implement `create` on a native receipt service instead. */
  write?(record: LegalAcceptanceRecord): void | Promise<void>;
  /** Clear the stored receipt so the gate shows again as a first run. */
  reset?(): void | Promise<void>;
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
  /**
   * Set by `reopenLegalAcceptance`: the stored acceptance no longer counts, so the gate shows as a re-acceptance. The old
   * record stays until a new acceptance replaces it, which clears the flag.
   */
  legalReopened?: boolean;
  /** Step ids the user skipped with the per-step Skip button (not marked complete). */
  skippedSteps?: string[];
}

/** What `onComplete` receives. `steps` holds values passed to `ctx.setResult` during this mount. */
export interface OnboardingResult {
  completed: string[];
  skipped: string[];
  steps: Record<string, unknown>;
}

/** Button and status copy. Each key defaults to the English text shown in the table. */
export interface OnboardingLabels {
  /** "Set up later". */
  skip: string;
  /** Per-step Skip button. */
  skipStep: string;
  /** "Back". */
  back: string;
  /** "Continue". */
  continue: string;
  /** "Finish setup" on the last step. */
  finish: string;
  /** "Loading setup…". */
  loading: string;
}

export interface OnboardingStepContext {
  state: PersistedOnboardingState;
  settings: SettingsBackend;
  setState: (next: Partial<PersistedOnboardingState>) => Promise<void>;
  permissions?: PermissionsBackend;
  models?: ModelsBackend;
  license?: LicenseBackend;
  signIn?: SignInBackend;
  /** Readiness reported by steps via `setReady`, keyed by step id. */
  ready: Readonly<Record<string, boolean>>;
  /** Reports whether the active step is ready (used by `canContinue`). Stable per step. */
  setReady: (ready: boolean) => void;
  /** Stores a value for this step; it is passed to `onComplete` as `result.steps[step.id]`. */
  setResult: (value: unknown) => void;
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
  /** Primary button text for this step (e.g. "Open CodeRight"). Overrides Continue and Finish setup. */
  continueLabel?: string;
  /**
   * Runs before Continue/Finish advances. Return `false` (or throw) to keep the step; a throw shows its message.
   * `void` or `true` advances. Not run for Back, the rail, or Skip.
   */
  onContinue?: (context: OnboardingStepContext) => void | boolean | Promise<void | boolean>;
  /** Empty title hides the heading (e.g. a Welcome step). */
  render: (context: OnboardingStepContext) => ReactNode;
}

export interface LegalStepConfig extends Omit<LegalGateProps, "variant" | "onAgree" | "onQuit"> {
  onQuit?: () => void;
}

/**
 * How legal documents take part in onboarding.
 * - `gate` (default): the blocking agreement gate comes first and is recorded (needs `legal`).
 * - `notice`: no gate and no acceptance. A first "Notice" step lists the `legal` documents as links; Continue moves on (free or open-source apps that still hold their notice).
 * - `none`: no legal step at all; `legal` is not needed.
 */
export type LegalMode = "gate" | "notice" | "none";

export interface OnboardingProps {
  /** Window-sized by default; use `contained` when the host sizes the setup surface. */
  layout?: "window" | "contained";
  settings: SettingsBackend;
  /** Required when `legalMode` is `gate` (default). Optional for `notice` (the documents shown) and `none`. */
  legal?: LegalStepConfig;
  /** Default `gate`. See `LegalMode`. */
  legalMode?: LegalMode;
  /** Name shown in the step rail. Default: `legal.appName`. */
  appName?: string;
  /** App-specific steps are appended after built-in steps, in supplied order. */
  steps?: readonly OnboardingStep[];
  permissions?: PermissionsBackend;
  models?: ModelsBackend;
  license?: LicenseBackend;
  /**
   * Adds the built-in sign-in step after the licence step and hands the backend to every step as `ctx.signIn`. Build it
   * with `signInBackendFromOAuth` over the app's `rightkit-oauth` commands.
   */
  signIn?: SignInBackend;
  /** The document set declared to `legalReceipt.create`. Default: the gate's documents as `{ title, url }`. */
  receiptDocuments?: readonly DeclaredLegalDocument[];
  /** Called once when setup finishes, with the completed and skipped step ids and the step results. */
  onComplete: (result: OnboardingResult) => void | Promise<void>;
  onSkip?: () => void | Promise<void>;
  /** Override any button or status text. Keys not given keep the English defaults. */
  labels?: Partial<OnboardingLabels>;
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
  /**
   * Read and record the legal acceptance in the app's own (preferably native) receipt instead of (overriding) the settings
   * backend's copy. See `LegalReceiptPort`; `createNativeLegalReceiptPort` builds one over the host's receipt commands.
   */
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
