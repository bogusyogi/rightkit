import {
  createAcceptanceReceipt,
  isCanonicalUtcTimestamp,
  needsLegalAcceptance,
  type LegalManifest,
} from "@rightkit/legal";
import type {
  LegalAcceptanceRecord,
  SettingsBackend,
  SettingsSnapshot,
  SettingsValues,
} from "@rightkit/onboarding";
import type { Settings } from "@rightkit/document-core/settings/settings";

/** The host app's generated acceptance contract (src/generated/legal-contract.json). */
export interface LegalContract {
  acceptanceVersion: string;
  materialSha256: string;
  [key: string]: unknown;
}

// Set once by the host at startup (configureLegal): a published package cannot import files of the app that installs it.
// ES module exports are live bindings, so every importer sees the configured values.
let legalManifest: LegalManifest;
export let REQUIRED_LEGAL_ACCEPTANCE_VERSION: string;
export let LEGAL_MATERIAL_SHA256: string;

export type LegalEligibilityBasis = NonNullable<Settings["legalEligibilityBasis"]>;

/** The four flat settings fields that together form the host app acceptance receipt. */
export type LegalReceiptFields = Pick<
  Settings,
  "legalAcceptanceVersion" | "legalAcceptedAt" | "legalEligibilityBasis" | "legalMaterialSha256"
>;

// The manifest's suite-wide document titles ("Right Suite Desktop License v1.0
// Ownership Notice") are too long for the gate's compact document list — these
// three roles get a short display title instead. product_schedule and
// privacy_notice already carry the host app-specific titles in the manifest
// ("the host app Product Schedule" / "the host app Privacy Notice"), so those pass
// through unchanged.
const GATE_DOCUMENT_SHORT_TITLES: Partial<Record<string, string>> = {
  license: "License",
  eula: "End User License Agreement",
  acceptable_use: "Acceptable Use Policy",
};

/**
 * The checkout-role documents shown on the first-run/re-acceptance gate: every
 * manifest document EXCEPT third_party_notices (a build-provenance appendix —
 * not something a user agrees to at first run). Titles/URLs are sourced from
 * legal-manifest.json, never hardcoded, so a publicUrl change (e.g. VR's
 * privacy_notice lives at /privacy/, not /legal/privacy/) or a title change
 * propagates automatically instead of drifting out of sync with the gate.
 */
export interface LegalGateDocumentLink {
  title: string;
  url: string;
}

function manifestLinks(
  shortTitles: Partial<Record<string, string>>,
  excludeRoles: string[] = [],
): LegalGateDocumentLink[] {
  return legalManifest.documents
    .filter((document) => !excludeRoles.includes(document.role))
    .map((document) => ({
      title: shortTitles[document.role] ?? document.title,
      url: document.publicUrl,
    }));
}

export let LEGAL_GATE_DOCUMENTS: LegalGateDocumentLink[] = [];

/** Permanent legal links in Settings → About: every manifest document, with About's short names. */
export let LEGAL_ABOUT_LINKS: LegalGateDocumentLink[] = [];

/** The host hands over its legal/legal-manifest.json and src/generated/legal-contract.json once at startup. */
export function configureLegal({ manifest, contract }: { manifest: unknown; contract: LegalContract }): void {
  const m = manifest as LegalManifest | null;
  if (!m || !Array.isArray(m.documents)) throw new TypeError("configureLegal: a legal manifest with documents is required");
  if (typeof contract?.acceptanceVersion !== "string" || typeof contract?.materialSha256 !== "string") {
    throw new TypeError("configureLegal: the contract needs acceptanceVersion and materialSha256 strings");
  }
  legalManifest = m;
  REQUIRED_LEGAL_ACCEPTANCE_VERSION = contract.acceptanceVersion;
  LEGAL_MATERIAL_SHA256 = contract.materialSha256;
  LEGAL_GATE_DOCUMENTS = manifestLinks(GATE_DOCUMENT_SHORT_TITLES, ["third_party_notices"]);
  LEGAL_ABOUT_LINKS = manifestLinks({
    license: "Ownership Notice",
    eula: "EULA",
    acceptable_use: "Acceptable Use",
    product_schedule: "Product Schedule",
    privacy_notice: "Privacy",
    third_party_notices: "Third-party Notices",
  });
}

/** True only when every field of the acceptance receipt is current: the
 *  acceptance version and material-document hash match the generated
 *  contract (src/generated/legal-contract.json, synced from legal-manifest.json
 *  by scripts/sync-legal-acceptance-contract.mjs), the timestamp is a
 *  canonical UTC instant, and an eligibility basis was recorded. */
export function legalAcceptanceCurrent(settings: LegalReceiptFields): boolean {
  const basis = settings.legalEligibilityBasis;
  if (
    !isCanonicalUtcTimestamp(settings.legalAcceptedAt) ||
    (basis !== "individual" && basis !== "enterprise") ||
    settings.legalMaterialSha256 !== LEGAL_MATERIAL_SHA256
  ) {
    return false;
  }
  // The stored acceptance is the flat settings fields, not a document-level receipt. Material
  // hash equality (above) proves the agreed documents are unchanged, so the receipt is rebuilt from
  // the manifest carrying the STORED acceptance version, and the package gate checks schema,
  // app key and version against the manifest.
  const manifest = legalManifest;
  const receipt = {
    ...createAcceptanceReceipt(manifest, {
      acceptedAt: settings.legalAcceptedAt,
      surface: "first_run",
      eligibilityBasis: basis,
    }),
    acceptanceVersion: settings.legalAcceptanceVersion,
  };
  return !needsLegalAcceptance(manifest, receipt);
}

/** Map the flat settings receipt to the onboarding package's record (`null` when no receipt is stored). */
export function legalReceiptRecord(settings: LegalReceiptFields): LegalAcceptanceRecord | null {
  const basis = settings.legalEligibilityBasis;
  if (
    !settings.legalAcceptanceVersion ||
    !settings.legalMaterialSha256 ||
    !isCanonicalUtcTimestamp(settings.legalAcceptedAt) ||
    (basis !== "individual" && basis !== "enterprise")
  ) {
    return null;
  }
  return {
    version: settings.legalAcceptanceVersion,
    documentsHash: settings.legalMaterialSha256,
    acceptedAt: settings.legalAcceptedAt,
    eligibilityBasis: basis,
  };
}

/** Inverse of legalReceiptRecord(): the settings patch that stores one acceptance. */
export function legalReceiptFields(record: LegalAcceptanceRecord): LegalReceiptFields {
  return {
    legalAcceptanceVersion: record.version,
    legalAcceptedAt: record.acceptedAt,
    legalEligibilityBasis: record.eligibilityBasis,
    legalMaterialSha256: record.documentsHash,
  };
}

/** Onboarding's `isAccepted`: the stored record must pass every legalAcceptanceCurrent() rule. */
export function legalRecordCurrent(record: LegalAcceptanceRecord | undefined): boolean {
  return record !== undefined && legalAcceptanceCurrent(legalReceiptFields(record));
}

const ONBOARDING_STORE_KEY = "vr.onboarding.v1";

/**
 * Backing store for onboarding's own progress record (`onboarding.state`). The legal receipt is NOT
 * kept here: it stays in the settings store and reaches onboarding through `legalReceipt`. A storage
 * failure throws so the gate surfaces it instead of silently dropping progress.
 */
export function createOnboardingSettingsBackend(): SettingsBackend {
  let revision = 0;
  const read = (): SettingsValues => {
    const raw = localStorage.getItem(ONBOARDING_STORE_KEY);
    return raw ? (JSON.parse(raw) as SettingsValues) : {};
  };
  const snapshot = (values: SettingsValues): SettingsSnapshot => ({ values, revision });
  return {
    async load() {
      return snapshot(read());
    },
    async patch(patch) {
      const values = { ...read(), ...patch };
      localStorage.setItem(ONBOARDING_STORE_KEY, JSON.stringify(values));
      revision += 1;
      return snapshot(values);
    },
  };
}
