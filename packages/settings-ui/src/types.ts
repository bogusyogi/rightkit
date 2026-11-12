import type { ReactNode } from "react";

export type SettingsValues = Record<string, unknown>;

/**
 * A committed revision. A counter fits a number; a 64-bit value (the content hash a flat store uses as its revision) does
 * not, and travels as a decimal string. The package never computes with it: it is compared by identity and echoed back.
 */
export type SettingsRevision = number | string;

export interface SettingsSnapshot<R extends SettingsRevision = number> {
  values: SettingsValues;
  /** Committed revision from `rightkit-app-settings`; used for compare-and-set. */
  revision: R;
  /** Schema version of the stored document, when the host reports it. */
  schemaVersion?: number;
  /** Set when the write is stored but a later step failed (for example the directory sync). Do not repeat the write. */
  warning?: string;
}

/**
 * Persistence port. The shell adapter talks to `rightkit-app-settings`; tests and previews use the memory adapter.
 * `R` is the revision type the host uses (`number` by default, `string` for a 64-bit revision).
 */
export interface SettingsBackend<R extends SettingsRevision = number> {
  load(): Promise<SettingsSnapshot<R>>;
  /** Merge top-level keys. `null` removes a key. Rejects with `code: "revision_conflict"` when `expectedRevision` is stale. */
  patch(patch: SettingsValues, expectedRevision?: R): Promise<SettingsSnapshot<R>>;
  /** Changed top-level keys from another window/process. Returns an unsubscribe. */
  subscribe?(handler: (keys: string[]) => void): Promise<() => void>;
}

export type ModelState = "missing" | "installed" | "downloading" | "verifying" | "error";

export interface ModelEntry {
  id: string;
  label: string;
  description?: string;
  sizeBytes?: number;
  state: ModelState;
  /** 0..1 while downloading or verifying. */
  progress?: number;
  revision?: string;
  active?: boolean;
  /** Hardware fit from `rightkit-models` (`FitKind`). */
  fit?: { kind: "measured" | "estimated" | "unsupported" | "unqualified" | "rental_only" | "api"; reasons?: string[] };
  /** `ErrorCode` from `rightkit-models` plus a human message. */
  error?: { code: string; message: string };
  /** False hides Remove (e.g. a model this app may not delete). Default true. */
  removable?: boolean;
  /** Owned by the shared Orthic model store and used by other apps: shows a "Shared" badge and never offers Remove. */
  shared?: boolean;
}

/** Local-model port over the `rightkit-models` store and installer. */
export interface ModelsBackend {
  list(): Promise<ModelEntry[]>;
  install(id: string): Promise<void>;
  /** Cancel keeps `.part` files so the next install resumes. */
  cancel(id: string): Promise<void>;
  remove(id: string): Promise<void>;
  /** Pushed entry updates (progress, completion). Returns an unsubscribe. */
  subscribe?(handler: (entry: ModelEntry) => void): Promise<() => void>;
}

/** Pointer to a secret in the platform credential store; the same shape as `rightkit-app-settings::SecretRef`. */
export interface SecretRef {
  service: string;
  account: string;
}

export type SecretPresence = "present" | "absent" | "unknown";

/** What a `set` may do to a slot that already holds a key: `create` refuses (`secret_exists`), `replace` overwrites. */
export type WriteIntent = "create" | "replace";

/** Credential-store port. Values go in, never come back out: only presence is readable. */
export interface SecretsBackend {
  presence(refs: SecretRef[]): Promise<SecretPresence[]>;
  /** Write-only. `intent` defaults to `"replace"`. */
  set(ref: SecretRef, value: string, intent?: WriteIntent): Promise<void>;
  clear(ref: SecretRef): Promise<void>;
}

export interface SettingsContext {
  values: SettingsValues;
  /** Persist one key (optimistic; rolls back and surfaces `error` on failure). */
  set(key: string, value: unknown): Promise<void>;
  patch(patch: SettingsValues): Promise<void>;
  saving: boolean;
  error: string | null;
  clearError(): void;
}

/**
 * How a standard section reads and writes one setting when the host stores it under another key or in another form (for
 * example a theme kept as `app_theme` with its own vocabulary). Without a binding the section uses its own key.
 */
export interface SettingBinding<T> {
  /** The current value from the settings context; `undefined` when unset or not understood. */
  read(ctx: SettingsContext): T | undefined;
  /** Persists the value, usually with `ctx.set` or `ctx.patch`. */
  write(ctx: SettingsContext, value: T): void | Promise<void>;
}

export interface SettingsEntry {
  /** Stable id, rendered as `data-setting-id` on the row so search can reveal it. */
  id: string;
  label: string;
  keywords?: string[];
}

export interface SettingsSection {
  id: string;
  /** Nav label; also the default page heading and the text search matches. */
  label: string;
  /** Pill rendered after the label in the nav (e.g. "FREE"). Not searched. */
  badge?: ReactNode;
  /** Page heading when it should differ from the nav label (defaults to `label`). */
  heading?: ReactNode;
  /** Omit this section's `<h1>`. Overrides the panel's `hideSectionHeading`; the article is labelled by `label` instead. */
  hideSectionHeading?: boolean;
  /** Nav group heading; sections with the same group sit together. */
  group?: string;
  icon?: ReactNode;
  /** Sort key inside the group; standard sections use 100, 200, ... so apps can slot between. */
  order?: number;
  /** Extra search terms for the section itself. */
  keywords?: string[];
  /** Searchable rows inside the section. */
  entries?: SettingsEntry[];
  visible?: (ctx: SettingsContext) => boolean;
  render: (ctx: SettingsContext) => ReactNode;
}
