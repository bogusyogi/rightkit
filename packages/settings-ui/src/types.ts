import type { ReactNode } from "react";

export type SettingsValues = Record<string, unknown>;

export interface SettingsSnapshot {
  values: SettingsValues;
  /** Monotonic revision from `rightkit-settings`; used for compare-and-set. */
  revision: number;
}

/** Persistence port. The shell adapter talks to `rightkit-settings`; tests and previews use the memory adapter. */
export interface SettingsBackend {
  load(): Promise<SettingsSnapshot>;
  /** Merge top-level keys. `null` removes a key. Rejects with `code: "revision_conflict"` when `expectedRevision` is stale. */
  patch(patch: SettingsValues, expectedRevision?: number): Promise<SettingsSnapshot>;
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
  fit?: { kind: "measured" | "estimated" | "unsupported" | "unqualified"; reasons?: string[] };
  /** `ErrorCode` from `rightkit-models` plus a human message. */
  error?: { code: string; message: string };
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

/** Pointer to a secret in the platform credential store; the same shape as `rightkit-settings::SecretRef`. */
export interface SecretRef {
  service: string;
  account: string;
}

export type SecretPresence = "present" | "absent" | "unknown";

/** Credential-store port. Values go in, never come back out: only presence is readable. */
export interface SecretsBackend {
  presence(refs: SecretRef[]): Promise<SecretPresence[]>;
  set(ref: SecretRef, value: string): Promise<void>;
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
