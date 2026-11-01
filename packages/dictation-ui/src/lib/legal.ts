import { isCanonicalUtcTimestamp } from "@rightkit/legal/browser";
import type { SettingsBlob } from "../types/settings";
import legalContract from "@rightkit-host/src/generated/legal-contract.json";

export const REQUIRED_LEGAL_ACCEPTANCE_VERSION = legalContract.acceptanceVersion;
export const LEGAL_MATERIAL_SHA256 = legalContract.materialSha256;
// Consumed by the in-app first-run/re-acceptance gate (LegalAcceptance.tsx,
// @rightkit/legal-ui's LegalGate). Both are generated from
// legal/legal-manifest.json by scripts/sync-legal-acceptance-contract.mjs —
// never hardcode a document title/URL or the agreement copy here.
export const LEGAL_AGREEMENT_STATEMENT = legalContract.agreementStatement;
export const LEGAL_DOCUMENTS = legalContract.documents;

export type LegalEligibilityBasis = NonNullable<SettingsBlob["legal_eligibility_basis"]>;

export function legalAcceptanceCurrent(settings: SettingsBlob): boolean {
  return (
    settings.legal_acceptance_version === REQUIRED_LEGAL_ACCEPTANCE_VERSION &&
    isCanonicalUtcTimestamp(settings.legal_accepted_at) &&
    ["individual", "enterprise"].includes(settings.legal_eligibility_basis ?? "") &&
    settings.legal_material_sha256 === LEGAL_MATERIAL_SHA256
  );
}
