import { onboardingStateKey, readAcceptance, readState } from "./Onboarding.js";
import type { HistoricalLegalReceipt, LegalAcceptanceRecord, LegalReceiptCreateRequest, LegalReceiptPort, SettingsBackend } from "./types.js";

/**
 * The commands of the native legal-receipt host service. The native side registers them once under the host caller policy
 * (`rightkit-tauri-shell` host services); the trusted window is the one that shows the legal gate. Names are provisional
 * until the host service is published; an app whose host registers other names passes `commands`.
 */
export const LEGAL_RECEIPT_COMMANDS = Object.freeze({
  read: "rightkit_legal_receipt_read",
  create: "rightkit_legal_receipt_create",
  reset: "rightkit_legal_receipt_reset",
  /** Imports a renderer-held legacy acceptance once (see `importLegacyAcceptance`). */
  import: "rightkit_legal_receipt_import",
});

/** Tauri's `invoke`, structurally (`@tauri-apps/api/core`). No import, so the package stays free of Tauri. */
export type LegalReceiptInvoke = (command: string, args?: Record<string, unknown>) => Promise<unknown>;

export interface NativeLegalReceiptPortOptions {
  invoke: LegalReceiptInvoke;
  commands?: Partial<typeof LEGAL_RECEIPT_COMMANDS>;
}

/** Host commands reject with `{ code, message }`; the gate and steps show `Error#message`. */
function hostError(error: unknown): Error {
  if (error instanceof Error) return error;
  const message = error && typeof error === "object" ? (error as { message?: unknown }).message : error;
  return new Error(typeof message === "string" && message.trim() ? message : "The legal receipt service failed.");
}

async function call(invoke: LegalReceiptInvoke, command: string, args?: Record<string, unknown>): Promise<unknown> {
  try { return await invoke(command, args); } catch (error) { throw hostError(error); }
}

/** The declaration the native service compares with its manifest: never a time. */
function declaration(request: LegalReceiptCreateRequest): Record<string, unknown> {
  return {
    version: request.version,
    documentsHash: request.documentsHash,
    documents: request.documents,
    eligibilityBasis: request.eligibilityBasis,
    surface: request.surface,
  };
}

/** True for a historical receipt value (`{ record, current: false }`) as the native read may return it. */
function isHistoricalValue(value: unknown): value is { record: unknown; current: false } {
  return typeof value === "object" && value !== null && !Array.isArray(value) && "record" in value && (value as { current?: unknown }).current === false;
}

/**
 * `LegalReceiptPort` over the native legal-receipt service.
 *
 * - `read` returns the stored record, or `null` when there is none or the stored value is not a well-formed record. A
 *   historical receipt (a version or digest that no longer matches) comes back as `{ record, current: false }`, which keeps
 *   the re-acceptance variant and the setup progress; the native service offers it that way.
 * - `create` sends the declaration (`version`, `documentsHash`, `documents`, `eligibilityBasis`, `surface`) and NO time. The
 *   service compares it with the manifest it embeds, stamps native UTC time, stores the receipt and returns the record; a
 *   refusal or a malformed answer throws, which keeps the gate closed.
 * - `reset` clears the receipt.
 * There is no `write`: the renderer never supplies a stored record.
 */
export function createNativeLegalReceiptPort({ invoke, commands }: NativeLegalReceiptPortOptions): LegalReceiptPort {
  const names = { ...LEGAL_RECEIPT_COMMANDS, ...commands };
  return {
    async read(): Promise<LegalAcceptanceRecord | HistoricalLegalReceipt | null> {
      const value = await call(invoke, names.read);
      if (value === null || value === undefined) return null;
      if (isHistoricalValue(value)) {
        const record = readAcceptance(value.record);
        return record ? { record, current: false } : null;
      }
      return readAcceptance(value) ?? null;
    },
    async create(request: LegalReceiptCreateRequest): Promise<LegalAcceptanceRecord> {
      const created = readAcceptance(await call(invoke, names.create, { request: declaration(request) }));
      if (!created) throw new Error("The legal receipt service returned an invalid record.");
      return created;
    },
    async reset() { await call(invoke, names.reset); },
  };
}

export interface LegacyAcceptanceImportOptions {
  invoke: LegalReceiptInvoke;
  /** Key of the renderer-held legacy receipt, for example `scraperight.legal-acceptance.v1`. */
  storageKey: string;
  /**
   * Maps the stored text to the declaration the native service verifies against its manifest (hashes, versions, documents).
   * `null`: not an importable record; nothing is sent and the legacy key stays in place.
   */
  parse: (raw: string) => LegalReceiptCreateRequest | null;
  /** Storage that holds the legacy key. Default `localStorage` when present. `null`: no legacy storage to read. */
  storage?: Pick<Storage, "getItem" | "removeItem"> | null;
  commands?: Partial<typeof LEGAL_RECEIPT_COMMANDS>;
}

function browserStorage(): Pick<Storage, "getItem" | "removeItem"> | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/**
 * One-time import of a renderer-held (legacy) acceptance into the native receipt. The legacy record is submitted once to the
 * native `import` command, which verifies it against the manifest and stamps the import with its own clock. Only after the
 * native service confirms the record is the legacy key removed. A refusal or a failed call throws and leaves the key in place,
 * so the next launch retries; no acceptance is invented here. Returns the imported record, or `null` when there is nothing to
 * import.
 */
export async function importLegacyAcceptance({ invoke, storageKey, parse, storage, commands }: LegacyAcceptanceImportOptions): Promise<LegalAcceptanceRecord | null> {
  const names = { ...LEGAL_RECEIPT_COMMANDS, ...commands };
  const backing = storage === undefined ? browserStorage() : storage;
  let raw: string | null;
  try {
    raw = backing?.getItem(storageKey) ?? null;
  } catch {
    raw = null;
  }
  if (raw === null) return null;
  const request = parse(raw);
  if (!request) return null;
  const imported = readAcceptance(await call(invoke, names.import, { request: declaration(request) }));
  if (!imported) throw new Error("The legal receipt service returned an invalid record.");
  try {
    backing?.removeItem(storageKey);
  } catch {
    /* the native receipt is stored; the native import is idempotent, so a later run only repeats the confirmation */
  }
  return imported;
}

/**
 * Re-opens the agreement: the stored acceptance stops counting, so the gate shows as a re-acceptance on the next mount of
 * `Onboarding`. The old record (and the app's receipt) stays until the user agrees again; agreeing creates the new receipt and
 * clears the flag. Progress through the other steps is kept.
 */
export async function reopenLegalAcceptance({ settings }: { settings: SettingsBackend }): Promise<void> {
  const key = onboardingStateKey();
  const snapshot = await settings.load();
  const state = readState(snapshot.values[key]);
  await settings.patch({ [key]: { ...state, legalReopened: true } }, snapshot.revision);
}

/**
 * Forgets the acceptance entirely: the app's receipt is cleared first (`legalReceipt.reset`, required when a receipt port is
 * given, so a failed reset cannot leave the receipt accepted), then the settings copy, so the next mount shows the first-run
 * gate. Other steps' progress is kept.
 */
export async function resetLegalAcceptance({ settings, legalReceipt }: { settings: SettingsBackend; legalReceipt?: LegalReceiptPort }): Promise<void> {
  if (legalReceipt) {
    if (!legalReceipt.reset) throw new Error("The legal receipt cannot be reset.");
    await legalReceipt.reset();
  }
  const key = onboardingStateKey();
  const snapshot = await settings.load();
  const next = { ...readState(snapshot.values[key]), legalAccepted: false };
  delete next.legalAcceptance;
  delete next.eligibilityBasis;
  delete next.legalReopened;
  next.completed = next.completed.filter((id) => id !== "legal");
  await settings.patch({ [key]: next }, snapshot.revision);
}
