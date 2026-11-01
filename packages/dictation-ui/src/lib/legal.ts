import { isCanonicalUtcTimestamp } from "@rightkit/legal/browser";
import type { SettingsBlob } from "../types/settings.js";

/**
 * The host app's generated legal acceptance contract (its src/generated/legal-contract.json, produced from
 * legal/legal-manifest.json by scripts/sync-legal-acceptance-contract.mjs). A published package cannot import a file
 * of the app that installs it, so the host hands it over once at startup with configureLegalContract().
 */
export interface LegalContract {
  acceptanceVersion: string;
  materialSha256: string;
  agreementStatement?: string;
  documents?: readonly unknown[];
  [key: string]: unknown;
}

let contract: LegalContract | null = null;

/** Called once by the host before any legal check or the first-run gate renders. */
export function configureLegalContract(next: LegalContract): void {
  if (typeof next?.acceptanceVersion !== "string" || typeof next?.materialSha256 !== "string") {
    throw new TypeError("configureLegalContract: the contract needs acceptanceVersion and materialSha256 strings");
  }
  contract = next;
}

function current(): LegalContract {
  if (!contract) throw new Error("@rightkit/dictation-ui: the host must call configureLegalContract(contract) before legal checks");
  return contract;
}

/** The acceptance version the first-run/re-acceptance gate requires (never hardcode it). */
export const requiredLegalAcceptanceVersion = (): string => current().acceptanceVersion;
export const legalMaterialSha256 = (): string => current().materialSha256;
export const legalAgreementStatement = (): string | undefined => current().agreementStatement;
export const legalDocuments = (): readonly unknown[] => current().documents ?? [];

export type LegalEligibilityBasis = NonNullable<SettingsBlob["legal_eligibility_basis"]>;

export function legalAcceptanceCurrent(settings: SettingsBlob): boolean {
  return (
    settings.legal_acceptance_version === requiredLegalAcceptanceVersion() &&
    isCanonicalUtcTimestamp(settings.legal_accepted_at) &&
    ["individual", "enterprise"].includes(settings.legal_eligibility_basis ?? "") &&
    settings.legal_material_sha256 === legalMaterialSha256()
  );
}
