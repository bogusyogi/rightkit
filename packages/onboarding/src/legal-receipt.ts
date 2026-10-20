import { onboardingStateKey, readAcceptance, readState } from "./Onboarding.js";
import type { LegalAcceptanceRecord, LegalReceiptCreateRequest, LegalReceiptPort, SettingsBackend } from "./types.js";

/**
 * The commands of the native legal-receipt host service. The native side registers them once under the host caller policy
 * (`rightkit-tauri-shell` host services); the trusted window is the one that shows the legal gate. Names are provisional
 * until the host service is published; an app whose host registers other names passes `commands`.
 */
export const LEGAL_RECEIPT_COMMANDS = Object.freeze({
  read: "rightkit_legal_receipt_read",
  create: "rightkit_legal_receipt_create",
  reset: "rightkit_legal_receipt_reset",
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

/**
 * `LegalReceiptPort` over the native legal-receipt service.
 *
 * - `read` returns the stored record, or `null` when there is none or the stored value is not a well-formed record.
 * - `create` sends the declaration (`version`, `documentsHash`, `documents`, `eligibilityBasis`, `surface`) and NO time. The
 *   service compares it with the manifest it embeds, stamps native UTC time, stores the receipt and returns the record; a
 *   refusal or a malformed answer throws, which keeps the gate closed.
 * - `reset` clears the receipt.
 * There is no `write`: the renderer never supplies a stored record.
 */
export function createNativeLegalReceiptPort({ invoke, commands }: NativeLegalReceiptPortOptions): LegalReceiptPort {
  const names = { ...LEGAL_RECEIPT_COMMANDS, ...commands };
  return {
    async read() {
      const value = await call(invoke, names.read);
      return value === null || value === undefined ? null : readAcceptance(value) ?? null;
    },
    async create(request: LegalReceiptCreateRequest): Promise<LegalAcceptanceRecord> {
      const created = readAcceptance(await call(invoke, names.create, {
        request: {
          version: request.version,
          documentsHash: request.documentsHash,
          documents: request.documents,
          eligibilityBasis: request.eligibilityBasis,
          surface: request.surface,
        },
      }));
      if (!created) throw new Error("The legal receipt service returned an invalid record.");
      return created;
    },
    async reset() { await call(invoke, names.reset); },
  };
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
