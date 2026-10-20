export const RIGHT_SUITE_LEGAL_SCHEMA = 2 as const;

export type LegalDocumentRole =
  | "license"
  | "eula"
  | "acceptable_use"
  | "product_schedule"
  | "privacy_notice"
  | "third_party_notices";

export type LegalDocumentTreatment = "agree" | "acknowledge" | "notice";
export type LegalAcceptanceSurface = "checkout" | "first_run" | "installer" | "enterprise_order";
export type LegalEligibilityBasis = "individual" | "enterprise";
/**
 * `proprietary` (default) requires all six document roles. `oss` is for free
 * or open-source apps: only `privacy_notice` and `third_party_notices` are
 * required; the four agreement roles are optional (but still validated when
 * present).
 */
export type LegalProfile = "proprietary" | "oss";

export interface LegalDocumentDescriptor {
  id: string;
  role: LegalDocumentRole;
  title: string;
  version: string;
  sha256: string;
  path: string;
  publicUrl: string;
  treatment: LegalDocumentTreatment;
  material: boolean;
}

export interface LegalAppendixDescriptor {
  id: string;
  title: string;
  sha256: string;
  path: string;
}

export interface LegalManifest {
  schema: typeof RIGHT_SUITE_LEGAL_SCHEMA;
  suite: "right-suite-desktop";
  appKey: string;
  productName: string;
  licensor: string;
  effectiveDate: string;
  acceptanceVersion: string;
  /** Omitted means `proprietary`. */
  profile?: LegalProfile;
  documents: LegalDocumentDescriptor[];
  appendices?: LegalAppendixDescriptor[];
}

export interface AcceptedLegalDocument {
  id: string;
  role: LegalDocumentRole;
  version: string;
  sha256: string;
  treatment: LegalDocumentTreatment;
  material: boolean;
}

export interface LegalAcceptanceReceipt {
  schema: typeof RIGHT_SUITE_LEGAL_SCHEMA;
  suite: "right-suite-desktop";
  appKey: string;
  acceptanceVersion: string;
  acceptedAt: string;
  surface: LegalAcceptanceSurface;
  eligibilityBasis: LegalEligibilityBasis;
  documents: AcceptedLegalDocument[];
}

const REQUIRED_ROLES: readonly LegalDocumentRole[] = [
  "license",
  "eula",
  "acceptable_use",
  "product_schedule",
  "privacy_notice",
  "third_party_notices",
];
const OSS_REQUIRED_ROLES: readonly LegalDocumentRole[] = ["privacy_notice", "third_party_notices"];
const AGREEMENT_ROLES = new Set<LegalDocumentRole>([
  "license",
  "eula",
  "acceptable_use",
  "product_schedule",
]);
const SHA256 = /^[a-f0-9]{64}$/i;
const APP_KEY = /^[a-z][a-z0-9-]{1,63}$/;
const DATE = /^\d{4}-\d{2}-\d{2}$/;
const CANONICAL_UTC = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/;
const SURFACES: readonly LegalAcceptanceSurface[] = ["checkout", "first_run", "installer", "enterprise_order"];
const ELIGIBILITY_BASES: readonly LegalEligibilityBasis[] = ["individual", "enterprise"];
const TREATMENTS: readonly LegalDocumentTreatment[] = ["agree", "acknowledge", "notice"];

/** Canonical UTC instant: exactly `Date#toISOString()` form, round-tripping unchanged. */
export function isCanonicalUtcTimestamp(value: unknown): value is string {
  if (typeof value !== "string" || !CANONICAL_UTC.test(value)) return false;
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) && new Date(parsed).toISOString() === value;
}

function assertNonEmpty(value: unknown, field: string): asserts value is string {
  if (typeof value !== "string" || !value.trim()) throw new Error(`${field} must be a non-empty string`);
}

function assertSafeRelativePath(value: string, role: LegalDocumentRole): void {
  const normalized = value.replaceAll("\\", "/");
  if (
    normalized.startsWith("/") ||
    /^[a-z]:\//i.test(normalized) ||
    normalized.split("/").includes("..") ||
    normalized.includes("\0")
  ) {
    throw new Error(`${role} path must be a safe relative path with no traversal`);
  }
}

export function assertLegalManifest(value: LegalManifest): LegalManifest {
  if (!value || typeof value !== "object") throw new Error("legal manifest must be an object");
  if (value.schema !== RIGHT_SUITE_LEGAL_SCHEMA) throw new Error("legal manifest schema must be 2");
  if (value.suite !== "right-suite-desktop") throw new Error("legal manifest suite must be right-suite-desktop");
  if (!APP_KEY.test(value.appKey)) throw new Error("legal manifest appKey is invalid");
  assertNonEmpty(value.productName, "productName");
  assertNonEmpty(value.licensor, "licensor");
  assertNonEmpty(value.acceptanceVersion, "acceptanceVersion");
  if (!DATE.test(value.effectiveDate)) throw new Error("effectiveDate must use YYYY-MM-DD");
  if (value.profile !== undefined && value.profile !== "proprietary" && value.profile !== "oss") {
    throw new Error("legal manifest profile must be proprietary or oss");
  }
  if (!Array.isArray(value.documents)) throw new Error("documents must be an array");

  const roles = new Set<LegalDocumentRole>();
  const ids = new Set<string>();
  for (const document of value.documents) {
    if (!REQUIRED_ROLES.includes(document.role)) throw new Error(`unsupported document role: ${String(document.role)}`);
    if (roles.has(document.role)) throw new Error(`duplicate document role: ${document.role}`);
    roles.add(document.role);
    assertNonEmpty(document.id, `${document.role}.id`);
    if (ids.has(document.id)) throw new Error(`duplicate document id: ${document.id}`);
    ids.add(document.id);
    assertNonEmpty(document.title, `${document.role}.title`);
    assertNonEmpty(document.version, `${document.role}.version`);
    if (!SHA256.test(document.sha256)) throw new Error(`${document.role} sha256 must be 64 hexadecimal characters`);
    assertNonEmpty(document.path, `${document.role}.path`);
    assertSafeRelativePath(document.path, document.role);
    try {
      const url = new URL(document.publicUrl);
      if (url.protocol !== "https:") throw new Error("not HTTPS");
    } catch {
      throw new Error(`${document.role} publicUrl must be an HTTPS URL`);
    }
    if (AGREEMENT_ROLES.has(document.role)) {
      if (document.treatment !== "agree") throw new Error(`${document.role} treatment must be agree`);
      if (document.material !== true) throw new Error(`${document.role} must be material`);
    }
    if (document.role === "privacy_notice" && document.treatment !== "acknowledge") {
      throw new Error("privacy_notice treatment must be acknowledge, not agreement or consent");
    }
    if (document.role === "third_party_notices" && document.treatment !== "notice") {
      throw new Error("third_party_notices treatment must be notice");
    }
  }
  for (const role of value.profile === "oss" ? OSS_REQUIRED_ROLES : REQUIRED_ROLES) {
    if (!roles.has(role)) throw new Error(`missing required document role: ${role}`);
  }
  const appendixIds = new Set<string>();
  for (const appendix of value.appendices ?? []) {
    assertNonEmpty(appendix.id, "appendix.id");
    if (appendixIds.has(appendix.id)) throw new Error(`duplicate appendix id: ${appendix.id}`);
    appendixIds.add(appendix.id);
    assertNonEmpty(appendix.title, `${appendix.id}.title`);
    if (!SHA256.test(appendix.sha256)) throw new Error(`${appendix.id} sha256 must be 64 hexadecimal characters`);
    assertNonEmpty(appendix.path, `${appendix.id}.path`);
    assertSafeRelativePath(appendix.path, "third_party_notices");
  }
  return value;
}

export function createAcceptanceReceipt(
  manifest: LegalManifest,
  input: {
    acceptedAt: string;
    surface: LegalAcceptanceSurface;
    eligibilityBasis: LegalEligibilityBasis;
  },
): LegalAcceptanceReceipt {
  assertLegalManifest(manifest);
  if (!isCanonicalUtcTimestamp(input.acceptedAt)) {
    throw new Error("acceptedAt must be a canonical ISO-8601 timestamp");
  }
  if (!SURFACES.includes(input.surface)) throw new Error("surface must be an allowed acceptance surface");
  if (!ELIGIBILITY_BASES.includes(input.eligibilityBasis)) {
    throw new Error("eligibilityBasis must be individual or enterprise");
  }
  return {
    schema: RIGHT_SUITE_LEGAL_SCHEMA,
    suite: manifest.suite,
    appKey: manifest.appKey,
    acceptanceVersion: manifest.acceptanceVersion,
    acceptedAt: input.acceptedAt,
    surface: input.surface,
    eligibilityBasis: input.eligibilityBasis,
    documents: acceptedDocuments(manifest),
  };
}

/**
 * What a renderer declares to a native receipt service: the manifest's identity and the
 * document set the user was shown, with NO time and NO eligibility. The native side stamps its
 * own `acceptedAt`, compares the declared set and digest with the manifest it embeds, and builds
 * the persisted receipt itself (`createAcceptanceReceipt` is its JS equivalent for hosts without
 * a native service).
 */
export interface LegalAcceptanceDeclaration {
  schema: typeof RIGHT_SUITE_LEGAL_SCHEMA;
  suite: "right-suite-desktop";
  appKey: string;
  acceptanceVersion: string;
  documents: AcceptedLegalDocument[];
}

function acceptedDocuments(manifest: LegalManifest): AcceptedLegalDocument[] {
  return manifest.documents.map(({ id, role, version, sha256, treatment, material }) => ({
    id,
    role,
    version,
    sha256,
    treatment,
    material,
  }));
}

/** The declaration for `manifest` (validated first). Pure: no clock, no hashing. */
export function acceptanceDeclaration(manifest: LegalManifest): LegalAcceptanceDeclaration {
  assertLegalManifest(manifest);
  return {
    schema: RIGHT_SUITE_LEGAL_SCHEMA,
    suite: manifest.suite,
    appKey: manifest.appKey,
    acceptanceVersion: manifest.acceptanceVersion,
    documents: acceptedDocuments(manifest),
  };
}

/** Runtime shape of one persisted receipt document entry. */
function acceptedDocumentIsWellFormed(value: unknown): value is AcceptedLegalDocument {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const document = value as Partial<AcceptedLegalDocument>;
  return (
    typeof document.id === "string" && document.id.trim() !== "" &&
    REQUIRED_ROLES.includes(document.role as LegalDocumentRole) &&
    typeof document.version === "string" && document.version.trim() !== "" &&
    typeof document.sha256 === "string" && SHA256.test(document.sha256) &&
    TREATMENTS.includes(document.treatment as LegalDocumentTreatment) &&
    typeof document.material === "boolean"
  );
}

/**
 * True when `value` is a well-formed persisted receipt for `manifest`:
 * object shape, schema/suite, app + acceptance version, canonical UTC
 * `acceptedAt`, an allowed surface and eligibility basis, and a documents
 * array of well-formed, role-unique entries. Freshness against material
 * document identities is `needsLegalAcceptance`'s separate question.
 * Never throws on receipt shape.
 */
export function receiptIsWellFormed(manifest: LegalManifest, value: unknown): value is LegalAcceptanceReceipt {
  assertLegalManifest(manifest);
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const receipt = value as Partial<LegalAcceptanceReceipt>;
  if (receipt.schema !== RIGHT_SUITE_LEGAL_SCHEMA || receipt.suite !== manifest.suite) return false;
  if (receipt.appKey !== manifest.appKey || receipt.acceptanceVersion !== manifest.acceptanceVersion) return false;
  if (!isCanonicalUtcTimestamp(receipt.acceptedAt)) return false;
  if (!SURFACES.includes(receipt.surface as LegalAcceptanceSurface)) return false;
  if (!ELIGIBILITY_BASES.includes(receipt.eligibilityBasis as LegalEligibilityBasis)) return false;
  if (!Array.isArray(receipt.documents)) return false;
  const roles = new Set<LegalDocumentRole>();
  for (const document of receipt.documents) {
    if (!acceptedDocumentIsWellFormed(document)) return false;
    if (roles.has(document.role)) return false;
    roles.add(document.role);
  }
  return true;
}

/**
 * Whether the user must (re-)accept. `receipt` is persisted, host-loaded
 * data, so it is treated as unknown: any malformed shape — non-object,
 * non-array or malformed `documents`, non-canonical timestamp, disallowed
 * surface/eligibility, wrong app/version — means stale (true), never a
 * throw. A well-formed receipt is stale only when a MATERIAL document's
 * identity (id, version or sha256) no longer matches the manifest.
 */
export function needsLegalAcceptance(manifest: LegalManifest, receipt: unknown): boolean {
  if (!receiptIsWellFormed(manifest, receipt)) return true;
  const accepted = new Map(receipt.documents.map((document) => [document.role, document]));
  return manifest.documents.some((document) => {
    if (!document.material) return false;
    const previous = accepted.get(document.role);
    return !previous || previous.id !== document.id || previous.version !== document.version
      || previous.sha256.toLowerCase() !== document.sha256.toLowerCase();
  });
}

/**
 * A stable digest over only the MATERIAL documents in a legal manifest
 * (license, eula, acceptable_use, product_schedule — see `AGREEMENT_ROLES`).
 * Non-material documents (privacy_notice, third_party_notices, and the
 * hash-bound appendices) regenerate routinely from cargo-about + npm license
 * scans on ordinary dependency bumps; a gate keyed on the WHOLE manifest hash
 * re-prompts users for a byte-identical EULA on every patch. A gate keyed on
 * this digest only changes when a document a user actually agreed to changes.
 *
 * Pure and deterministic: no filesystem access, stable regardless of
 * `documents` array order or JSON key order. Sorted by `role` and serialized
 * as `role|id|version|sha256` per material document, newline-joined, then
 * SHA-256 hex-digested (by `materialAcceptanceDigest` on the Node entry or
 * `materialAcceptanceDigestAsync` on the browser entry).
 */
export function materialDigestInput(manifest: LegalManifest): string {
  assertLegalManifest(manifest);
  return manifest.documents
    .filter((document) => document.material)
    .slice()
    .sort((a, b) => a.role.localeCompare(b.role))
    .map((document) => `${document.role}|${document.id}|${document.version}|${document.sha256.toLowerCase()}`)
    .join("\n");
}
