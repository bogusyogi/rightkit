import { createHash } from "node:crypto";
import type { LegalDocumentDescriptor, LegalDocumentRole } from "./core.js";

/**
 * Suite-wide company facts and default policy texts (issue Orthic-Labs/rightkit#266).
 * Every fact below cites its source. A fact with no source is NOT defaulted: it is a
 * required per-app field (see `refundPolicy`). Never add an unsourced fact.
 */

export interface CompanyAddress {
  readonly lines: readonly string[];
}

export interface CompanyFacts {
  readonly legalName: string;
  readonly entityType: string;
  readonly address: CompanyAddress;
  /** Legal and rights-request contact for the company. */
  readonly contactEmail: string;
  /** Support address. Suite default hello@orthiclabs.com; apps may override (HeardRight: hello@heardright.app). */
  readonly supportEmail: string;
  readonly governingLaw: string;
  readonly venue: string;
  readonly minimumAge: number;
}

/** The suite default fact set. */
export type SuiteCompanyFacts = CompanyFacts;

export const RIGHT_SUITE_COMPANY_FACTS: Readonly<SuiteCompanyFacts> = Object.freeze({
  // Source: D:\Claude\rightkit\legal\EULA.md line 5 ("Damned Ventures LLC, a Wyoming limited liability company").
  // Source: RK3 Mac lead decision D1, 2026-10-17 (suite default hello@orthiclabs.com; D:\Claude\rightkit\legal\products\cutright\PRIVACY_NOTICE.md line 17).
  supportEmail: "hello@orthiclabs.com",
  legalName: "Damned Ventures LLC",
  entityType: "Wyoming limited liability company",
  // Source: D:\Claude\rightkit\legal\EULA.md line 5 and section 18 (169 Madison Ave, New York, NY 10016, United States).
  address: Object.freeze({
    lines: Object.freeze(["169 Madison Ave", "New York, NY 10016", "United States"]),
  }),
  // Source: D:\Claude\rightkit\legal\EULA.md section 18 (legal@damnedventures.com).
  contactEmail: "legal@damnedventures.com",
  // Source: D:\Claude\rightkit\legal\EULA.md section 16 (Wyoming law, conflict rules excluded).
  governingLaw: "Wyoming",
  // Source: D:\Claude\rightkit\legal\EULA.md section 16 (state and federal courts located in Wyoming);
  // D:\Claude\rightkit\legal\WEBSITE_TERMS.md section 13 (Laramie County, Wyoming).
  venue: "the state and federal courts located in Wyoming (Laramie County)",
  // Source: Orthic-Labs/rightkit#266 and the RK3 Mac lead's instruction of 2026-10-17 ("minimum age 18").
  // NOTE: D:\Claude\rightkit\legal\products\heardright\PRIVACY_NOTICE.md line 112 says "under 13"; that is a
  // children's-privacy statement and is reported as a conflict, not reconciled here.
  minimumAge: 18,
}) as Readonly<SuiteCompanyFacts>;

/**
 * Refund terms are per-app configuration, never a suite default (RK3 Mac lead, 2026-10-17).
 * - `window`: refund request window in days (HeardRight: 14; source
 *   D:\Claude\rightkit\legal\products\heardright\PRODUCT_SCHEDULE.md section 10).
 * - `none`: no refunds except where the law requires, with an optional free trial
 *   (GenRight: 30-day Pro trial, no refunds; per the RK3 Mac lead).
 */
export type RefundPolicy =
  | { readonly kind: "window"; readonly days: number }
  | { readonly kind: "none"; readonly trialDays?: number };

export type DefaultPolicyKind = "eula" | "privacy" | "refund" | "terms";

export const DEFAULT_DATA_STATEMENT = "data we receive: update checks only";

export interface DefaultPolicyInput {
  readonly appName: string;
  /** Optional override of the suite support email. */
  readonly supportEmail?: string;
  /** Required: no suite default. */
  readonly refundPolicy: RefundPolicy;
  /** Overrides for suite facts, only where the app truly differs. */
  readonly facts?: Partial<SuiteCompanyFacts>;
  /** Replaces the default privacy data statement. */
  readonly dataStatement?: string;
  /** Text appended to a default document. */
  readonly append?: Partial<Record<DefaultPolicyKind, string>>;
  /** Replaces a default document entirely (the digest then follows the new text). */
  readonly replace?: Partial<Record<DefaultPolicyKind, string>>;
}

export interface RenderedPolicyDocument {
  readonly kind: DefaultPolicyKind;
  readonly title: string;
  /** Markdown source. */
  readonly content: string;
  readonly sha256: string;
}

export function resolveCompanyFacts(input: Pick<DefaultPolicyInput, "supportEmail" | "facts">): CompanyFacts {
  const facts: CompanyFacts = {
    ...RIGHT_SUITE_COMPANY_FACTS,
    ...input.facts,
    ...(input.supportEmail === undefined ? {} : { supportEmail: input.supportEmail.trim() }),
  };
  if (!Number.isInteger(facts.minimumAge) || facts.minimumAge < 1) throw new Error("minimumAge must be a positive integer");
  for (const key of ["legalName", "entityType", "contactEmail", "supportEmail", "governingLaw", "venue"] as const) {
    if (!facts[key].trim()) throw new Error(`${key} must be non-empty`);
  }
  if (facts.address.lines.length === 0) throw new Error("address must have at least one line");
  return facts;
}

export function assertRefundPolicy(policy: RefundPolicy): RefundPolicy {
  if (!policy || typeof policy !== "object") throw new Error("refundPolicy is required: it has no suite default");
  if (policy.kind === "window") {
    if (!Number.isInteger(policy.days) || policy.days < 1) throw new Error("refundPolicy.days must be a positive integer");
  } else if (policy.kind === "none") {
    if (policy.trialDays !== undefined && (!Number.isInteger(policy.trialDays) || policy.trialDays < 1)) {
      throw new Error("refundPolicy.trialDays must be a positive integer");
    }
  } else {
    throw new Error("refundPolicy.kind must be window or none");
  }
  return policy;
}

function addressBlock(facts: CompanyFacts): string {
  return [facts.legalName, ...facts.address.lines].join("  \n");
}

function refundBody(app: string, facts: CompanyFacts, policy: RefundPolicy): string {
  const law =
    "Nothing here limits a statutory refund, withdrawal, conformity, repair or replacement right that cannot lawfully be waived.";
  if (policy.kind === "window") {
    return [
      `${app} offers a ${policy.days}-day window to request a refund of a paid license. Email ${facts.supportEmail} within ${policy.days} days of purchase.`,
      "A refund revokes the affected paid key.",
      law,
    ].join("\n\n");
  }
  const trial = policy.trialDays
    ? `${app} offers a ${policy.trialDays}-day Pro trial so you can evaluate it before you pay.\n\n`
    : "";
  return `${trial}No refunds except where the law requires.\n\n${law}`;
}

function build(kind: DefaultPolicyKind, input: DefaultPolicyInput, facts: CompanyFacts): { title: string; content: string } {
  const app = input.appName;
  switch (kind) {
    case "eula":
      return {
        title: `${app} End User License Agreement`,
        content: [
          `# ${app} End User License Agreement`,
          `This agreement is between **${facts.legalName}**, a ${facts.entityType} with a mailing address at ${facts.address.lines.join(", ")} (**we**, **us**), and the person or organization installing or using ${app} (**you**).`,
          "## Eligibility",
          `You must be at least ${facts.minimumAge} years old to accept this agreement and use ${app}. If you accept for an organization, you represent that you have authority to bind it.`,
          "## Purchases and refunds",
          `Refund rights are set out in the ${app} Refund Policy and any mandatory consumer law.`,
          "## Governing law",
          `This agreement is governed by ${facts.governingLaw} law, excluding its conflict-of-law rules, and ${facts.venue} have jurisdiction. For an individual consumer, that law and venue apply only to the extent the mandatory law of the consumer's place of residence allows.`,
          "## Contact",
          `${addressBlock(facts)}  \n${facts.contactEmail}`,
        ].join("\n\n"),
      };
    case "privacy":
      return {
        title: `${app} Privacy Notice`,
        content: [
          `# ${app} Privacy Notice`,
          `**${facts.legalName}** publishes ${app}.`,
          "## What we receive",
          input.dataStatement ?? DEFAULT_DATA_STATEMENT,
          "## Children",
          `${app} is not directed to anyone under ${facts.minimumAge}.`,
          "## Contact",
          `Questions or rights requests: ${facts.supportEmail}. Company contact: ${facts.contactEmail}.`,
        ].join("\n\n"),
      };
    case "refund":
      return {
        title: `${app} Refund Policy`,
        content: [
          `# ${app} Refund Policy`,
          refundBody(app, facts, assertRefundPolicy(input.refundPolicy)),
          "## Contact",
          facts.supportEmail,
        ].join("\n\n"),
      };
    case "terms":
      return {
        title: `${app} Terms`,
        content: [
          `# ${app} Terms`,
          `These terms apply to ${app}, published by **${facts.legalName}**, a ${facts.entityType} (${facts.address.lines.join(", ")}).`,
          `You must be at least ${facts.minimumAge} years old to use ${app}.`,
          `These terms are governed by ${facts.governingLaw} law, subject to mandatory consumer rights. ${facts.venue} have jurisdiction.`,
          `Support: ${facts.supportEmail}. Legal: ${facts.contactEmail}.`,
        ].join("\n\n"),
      };
  }
}

export function sha256Hex(text: string): string {
  return createHash("sha256").update(text, "utf8").digest("hex");
}

export function renderDefaultPolicy(kind: DefaultPolicyKind, input: DefaultPolicyInput): RenderedPolicyDocument {
  if (typeof input.appName !== "string" || !input.appName.trim()) throw new Error("appName must be a non-empty string");
  assertRefundPolicy(input.refundPolicy);
  const facts = resolveCompanyFacts(input);
  const base = build(kind, input, facts);
  let content = input.replace?.[kind] ?? base.content;
  const extra = input.append?.[kind];
  if (extra) content = `${content}\n\n${extra}`;
  content = `${content.replace(/\r\n?/g, "\n").trimEnd()}\n`;
  return { kind, title: base.title, content, sha256: sha256Hex(content) };
}

export const DEFAULT_POLICY_KINDS: readonly DefaultPolicyKind[] = ["eula", "privacy", "refund", "terms"];

export function renderDefaultPolicies(input: DefaultPolicyInput): RenderedPolicyDocument[] {
  return DEFAULT_POLICY_KINDS.map((kind) => renderDefaultPolicy(kind, input));
}

/**
 * The manifest role each default document fills. The manifest schema has no `terms` or `refund` role,
 * so they ride on existing material roles: terms -> license, refund -> product_schedule (HeardRight keeps
 * refund terms in its Product Schedule, section 10). Adding roles would be a schema change.
 * eula, license and product_schedule are material, so any facts change moves `materialAcceptanceDigest`.
 */
export const DEFAULT_POLICY_ROLE: Readonly<Record<DefaultPolicyKind, LegalDocumentRole>> = Object.freeze({
  eula: "eula",
  privacy: "privacy_notice",
  refund: "product_schedule",
  terms: "license",
});

export interface DefaultDescriptorOptions {
  readonly version: string;
  /** Relative path prefix, e.g. "legal/". */
  readonly pathPrefix?: string;
  /** HTTPS base ending in a slash, e.g. "https://example.app/legal/". */
  readonly publicUrlBase: string;
}

/** Manifest descriptors for the rendered defaults, hash-bound to the rendered text. */
export function defaultPolicyDescriptors(
  input: DefaultPolicyInput,
  options: DefaultDescriptorOptions,
): LegalDocumentDescriptor[] {
  const prefix = options.pathPrefix ?? "";
  return renderDefaultPolicies(input).map((doc) => {
    const role = DEFAULT_POLICY_ROLE[doc.kind];
    return {
      id: doc.kind,
      role,
      title: doc.title,
      version: options.version,
      sha256: doc.sha256,
      path: `${prefix}${doc.kind}.md`,
      publicUrl: `${options.publicUrlBase}${doc.kind}/`,
      treatment: role === "privacy_notice" ? "acknowledge" : "agree",
      material: role !== "privacy_notice",
    };
  });
}
