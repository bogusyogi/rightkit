/**
 * Shared checkout (commerce) legal-acceptance construction + validation.
 *
 * Mechanics only: a store resolves the app manifest (its local checkout
 * policy) and passes it with the client's acceptance claim; this module
 * verifies every claim field byte-for-byte against that manifest and builds
 * the persisted order receipt from the CONFIGURED manifest values, never the
 * client's echo. Manifest selection, whether an app is opted in, pricing and
 * order flow stay with each store (extracted from the identical RightSites /
 * SellRight `legal/acceptance.ts` implementations; receipt shape unchanged).
 */
import {
  assertLegalManifest,
  RIGHT_SUITE_LEGAL_SCHEMA,
  type LegalDocumentRole,
  type LegalEligibilityBasis,
  type LegalManifest,
} from "./core.js";

/**
 * Checkout collects exactly these five roles. `third_party_notices` is
 * app-snapshot-only (installer/first-run disclosure), never collected here.
 */
export const CHECKOUT_LEGAL_ROLES = [
  "license",
  "eula",
  "acceptable_use",
  "product_schedule",
  "privacy_notice",
] as const satisfies readonly LegalDocumentRole[];

export type CheckoutLegalRole = (typeof CHECKOUT_LEGAL_ROLES)[number];

const ELIGIBILITY = new Set<string>(["individual", "enterprise"]);
const APP_KEY = /^[a-z][a-z0-9-]{1,63}$/;

/** The client's unverified checkout claim; every field is `unknown`-shaped. */
export interface CheckoutLegalAcceptance {
  acceptanceVersion?: unknown;
  eligibilityBasis?: unknown;
  authorityConfirmed?: unknown;
  agreementStatement?: unknown;
  documents?: unknown;
}

/** The server-attested receipt persisted with an order (snake_case wire shape). */
export interface OrderLegalReceipt {
  schema: typeof RIGHT_SUITE_LEGAL_SCHEMA;
  suite: "right-suite-desktop";
  app_key: string;
  acceptance_version: string;
  eligibility_basis: LegalEligibilityBasis;
  authority_confirmed: true;
  agreement_statement: string;
  accepted_at: string;
  accepted_via: "checkout";
  documents: Array<{
    role: CheckoutLegalRole;
    id: string;
    version: string;
    sha256: string;
    url: string;
    treatment: "agree" | "acknowledge";
  }>;
}

function fail(message: string): never {
  throw new Error(`legal acceptance: ${message}`);
}

function text(value: unknown, field: string, max = 256): string {
  if (typeof value !== "string" || !value.trim() || value.length > max) fail(`${field} is required`);
  return value.trim();
}

/**
 * Builds the server-attested legal acceptance receipt for an order. This is
 * the ONLY legal-acceptance boundary checkout trusts: the client submits a
 * claim, every field is verified against the store-configured `manifest`,
 * and the returned receipt is built from the configured values. Throws
 * `Error("legal acceptance: ...")` on any mismatch; `acceptedAt` is the
 * server clock and is persisted via `toISOString()` (canonical UTC).
 *
 * The caller resolves the manifest per-store; an app with no configured
 * manifest never reaches this function.
 */
export function buildCheckoutLegalReceipt(
  manifest: LegalManifest,
  value: CheckoutLegalAcceptance | null | undefined,
  acceptedAt: Date,
): OrderLegalReceipt {
  assertLegalManifest(manifest);
  const appKey = manifest.appKey;
  if (!APP_KEY.test(appKey)) fail("licensed product app key is invalid");

  if (!value || typeof value !== "object") fail("an affirmative receipt is required for software licenses");
  const acceptanceVersion = text(value.acceptanceVersion, "acceptanceVersion", 128);
  if (acceptanceVersion !== manifest.acceptanceVersion) {
    fail(`acceptanceVersion does not match the current legal acceptance version for "${appKey}"`);
  }
  const eligibilityBasis = value.eligibilityBasis;
  if (typeof eligibilityBasis !== "string" || !ELIGIBILITY.has(eligibilityBasis)) fail("eligibility basis is invalid");
  if (value.authorityConfirmed !== true) fail("authority confirmation is required");
  const agreementStatement = text(value.agreementStatement, "agreementStatement", 1_000);
  if (!/\bagree\b/i.test(agreementStatement)) fail("agreement statement must record affirmative agreement");
  if (!Array.isArray(value.documents)) fail("documents must be an array");

  const canonicalByRole = new Map(manifest.documents.map((document) => [document.role, document]));

  const seen = new Set<string>();
  const documents = value.documents.map((raw): OrderLegalReceipt["documents"][number] => {
    if (!raw || typeof raw !== "object" || Array.isArray(raw)) fail("document descriptor is invalid");
    const document = raw as Record<string, unknown>;
    const role = text(document.role, "document role") as CheckoutLegalRole;
    if (!CHECKOUT_LEGAL_ROLES.includes(role)) fail(`document role ${role} is unsupported`);
    if (seen.has(role)) fail(`document role ${role} is duplicated`);
    seen.add(role);

    // Guaranteed present by assertLegalManifest for a proprietary manifest
    // (every checkout role is required there) — checked anyway, fail closed.
    const canonical = canonicalByRole.get(role);
    if (!canonical) fail(`no configured ${role} document exists for app "${appKey}"`);
    if (canonical.treatment !== "agree" && canonical.treatment !== "acknowledge") {
      fail(`${role} configured treatment is not valid for checkout`);
    }

    const id = text(document.id, `${role}.id`);
    const version = text(document.version, `${role}.version`, 128);
    const sha256 = text(document.sha256, `${role}.sha256`, 64).toLowerCase();
    const url = text(document.url, `${role}.url`, 2_048);

    if (id !== canonical.id) fail(`${role} document id does not match the configured legal document`);
    if (version !== canonical.version) fail(`${role} document version does not match the configured legal document`);
    if (sha256 !== canonical.sha256.toLowerCase()) fail(`${role} document sha256 does not match the configured legal document`);
    if (url !== canonical.publicUrl) fail(`${role} document URL does not match the configured legal document`);

    // Persist the configured values just verified, not the client's echo.
    // Titles/URLs alone never prove document bytes: the sha256 above is the
    // material identity that had to match.
    return {
      role,
      id: canonical.id,
      version: canonical.version,
      sha256: canonical.sha256.toLowerCase(),
      url: canonical.publicUrl,
      treatment: canonical.treatment,
    };
  });
  for (const role of CHECKOUT_LEGAL_ROLES) if (!seen.has(role)) fail(`required ${role} document is missing`);
  if (!Number.isFinite(acceptedAt.getTime())) fail("server acceptance timestamp is invalid");

  return {
    schema: RIGHT_SUITE_LEGAL_SCHEMA,
    suite: manifest.suite,
    app_key: appKey,
    acceptance_version: acceptanceVersion,
    eligibility_basis: eligibilityBasis as LegalEligibilityBasis,
    authority_confirmed: true,
    agreement_statement: agreementStatement,
    accepted_at: acceptedAt.toISOString(),
    accepted_via: "checkout",
    documents,
  };
}
