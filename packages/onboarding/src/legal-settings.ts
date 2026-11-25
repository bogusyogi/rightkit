// Legal acceptance kept as flat fields in the host's settings: the "is it current" rule, a `LegalReceiptPort` over
// them, and the handlers of a standalone re-acceptance gate. Field names, the contract (version, documents hash),
// the timestamp check and every side effect are inputs. No React, no host names.
import type { LegalAcceptanceRecord, LegalReceiptPort } from "./types.js";

/** The settings keys holding the acceptance. Defaults are the `legal_*` names. */
export interface LegalSettingsFields {
  version: string;
  acceptedAt: string;
  eligibilityBasis: string;
  documentsHash: string;
}

export const DEFAULT_LEGAL_SETTINGS_FIELDS: LegalSettingsFields = Object.freeze({
  version: "legal_acceptance_version",
  acceptedAt: "legal_accepted_at",
  eligibilityBasis: "legal_eligibility_basis",
  documentsHash: "legal_material_sha256",
});

/** What the current agreement is: its version and the hash of its documents. */
export interface LegalContract {
  version: string;
  documentsHash: string;
}

export interface LegalSettingsOptions {
  contract: LegalContract;
  /** True for a canonical UTC timestamp (e.g. `isCanonicalUtcTimestamp` from `@rightkit/legal`). */
  isCanonicalTimestamp: (value: unknown) => boolean;
  /** Settings keys; each one missing falls back to the default. */
  fields?: Partial<LegalSettingsFields>;
  /** Eligibility bases that count. Default `["individual", "enterprise"]`. */
  bases?: readonly string[];
}

function resolveFields(fields?: Partial<LegalSettingsFields>): LegalSettingsFields {
  return { ...DEFAULT_LEGAL_SETTINGS_FIELDS, ...fields };
}

/**
 * True when the flat settings hold an acceptance of the current agreement: same version, a canonical timestamp,
 * a known eligibility basis and the same documents hash. A changed version or hash makes it false, so the gate re-prompts.
 */
export function legalAcceptanceCurrentFromSettings(settings: Record<string, unknown>, options: LegalSettingsOptions): boolean {
  const f = resolveFields(options.fields);
  const bases = options.bases ?? ["individual", "enterprise"];
  return (
    settings[f.version] === options.contract.version &&
    options.isCanonicalTimestamp(settings[f.acceptedAt]) &&
    bases.includes(String(settings[f.eligibilityBasis] ?? "")) &&
    settings[f.documentsHash] === options.contract.documentsHash
  );
}

export interface SettingsLegalReceiptPortOptions extends LegalSettingsOptions {
  /** Loads the settings values. */
  load(): Promise<Record<string, unknown>>;
  /** Patches settings keys. */
  patch(patch: Record<string, unknown>): Promise<unknown>;
  /** Runs after the receipt is written (e.g. resize the window to the next surface); a failure is ignored. */
  afterWrite?(): Promise<unknown>;
}

/**
 * `LegalReceiptPort` over flat settings fields. `read` returns the record only when `legalAcceptanceCurrentFromSettings`
 * holds, else `null`, so a changed version or hash re-prompts. `write` patches the four fields, then runs `afterWrite`.
 * There is no `reset` and no `create`.
 */
export function createSettingsLegalReceiptPort(options: SettingsLegalReceiptPortOptions): LegalReceiptPort {
  const f = resolveFields(options.fields);
  return {
    async read() {
      const values = await options.load();
      if (!legalAcceptanceCurrentFromSettings(values, options)) return null;
      return {
        version: String(values[f.version]),
        documentsHash: String(values[f.documentsHash]),
        acceptedAt: String(values[f.acceptedAt]),
        eligibilityBasis: values[f.eligibilityBasis] as LegalAcceptanceRecord["eligibilityBasis"],
      } as LegalAcceptanceRecord;
    },
    async write(record) {
      await options.patch({
        [f.version]: record.version,
        [f.acceptedAt]: record.acceptedAt,
        [f.eligibilityBasis]: record.eligibilityBasis,
        [f.documentsHash]: record.documentsHash,
      });
      await options.afterWrite?.().catch(() => undefined);
    },
  };
}

export interface ReAcceptanceHandlersOptions {
  contract: LegalContract;
  fields?: Partial<LegalSettingsFields>;
  /** Writes the four fields (a settings patch). A rejection propagates and keeps the gate open. */
  applyPatch(patch: Record<string, unknown>): Promise<unknown>;
  /** Runs after the save, errors ignored. The host passes it only when it has work (e.g. re-arming global hotkeys on an already set-up install). */
  afterSave?(): Promise<unknown>;
  /** Runs after `afterSave`, errors ignored (e.g. resize the window to the next surface). */
  afterAccept?(): Promise<unknown>;
  /** Called last, once the acceptance is saved. */
  onAccepted(): void;
  /** Quits the app. A failure is ignored: with no quit available there is nothing else to close. */
  quit(): Promise<unknown>;
  /** Clock for the timestamp. Default `new Date().toISOString()`. */
  now?(): string;
}

/**
 * The `onAgree` and `onQuit` of a standalone legal gate (e.g. `LegalGate` from `@rightkit/legal-ui`). `onAgree` saves the
 * acceptance through `applyPatch`, runs `afterSave` and `afterAccept`, then `onAccepted`. `onQuit` calls `quit`.
 */
export function createReAcceptanceHandlers(options: ReAcceptanceHandlersOptions): {
  onAgree(input: { eligibilityBasis: string }): Promise<void>;
  onQuit(): Promise<void>;
} {
  const f = resolveFields(options.fields);
  return {
    async onAgree({ eligibilityBasis }) {
      await options.applyPatch({
        [f.version]: options.contract.version,
        [f.acceptedAt]: (options.now ?? (() => new Date().toISOString()))(),
        [f.eligibilityBasis]: eligibilityBasis,
        [f.documentsHash]: options.contract.documentsHash,
      });
      await options.afterSave?.().catch(() => undefined);
      await options.afterAccept?.().catch(() => undefined);
      options.onAccepted();
    },
    async onQuit() {
      try {
        await options.quit();
      } catch {
        // Quit is unavailable; there is nothing else to close.
      }
    },
  };
}
