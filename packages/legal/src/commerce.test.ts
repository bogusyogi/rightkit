import { describe, expect, it } from "vitest";
import { buildCheckoutLegalReceipt, CHECKOUT_LEGAL_ROLES, type CheckoutLegalAcceptance } from "./commerce";
import type { LegalManifest } from "./core";

const HASH_A = "a".repeat(64);
const HASH_B = "b".repeat(64);
const AT = new Date("2026-07-16T12:34:56.000Z");

function manifest(): LegalManifest {
  return {
    schema: 2,
    suite: "right-suite-desktop",
    appKey: "heardright",
    productName: "HeardRight",
    licensor: "Damned Ventures LLC",
    effectiveDate: "2026-07-16",
    acceptanceVersion: "heardright-2026-07-16-v1",
    documents: [
      ["license", "agree", true],
      ["eula", "agree", true],
      ["acceptable_use", "agree", true],
      ["product_schedule", "agree", true],
      ["privacy_notice", "acknowledge", false],
      ["third_party_notices", "notice", false],
    ].map(([role, treatment, material], index) => ({
      id: `right-suite-${role}`,
      role: role as LegalManifest["documents"][number]["role"],
      title: String(role),
      version: "1.0",
      sha256: index === 0 ? HASH_A : HASH_B,
      path: `${role}.md`,
      publicUrl: `https://heardright.app/legal/${String(role).replaceAll("_", "-")}/`,
      treatment: treatment as LegalManifest["documents"][number]["treatment"],
      material: Boolean(material),
    })),
  };
}

/** A truthful client claim echoing the manifest's five checkout documents. */
function claim(m: LegalManifest): CheckoutLegalAcceptance {
  return {
    acceptanceVersion: m.acceptanceVersion,
    eligibilityBasis: "enterprise",
    authorityConfirmed: true,
    agreementStatement: "I agree to the documents listed.",
    documents: CHECKOUT_LEGAL_ROLES.map((role) => {
      const document = m.documents.find((d) => d.role === role)!;
      return { role, id: document.id, version: document.version, sha256: document.sha256, url: document.publicUrl };
    }),
  };
}

describe("buildCheckoutLegalReceipt", () => {
  it("builds the order receipt from configured manifest values, canonical UTC", () => {
    const m = manifest();
    const receipt = buildCheckoutLegalReceipt(m, claim(m), AT);
    expect(receipt).toMatchObject({
      schema: 2,
      suite: "right-suite-desktop",
      app_key: "heardright",
      acceptance_version: m.acceptanceVersion,
      eligibility_basis: "enterprise",
      authority_confirmed: true,
      accepted_at: "2026-07-16T12:34:56.000Z",
      accepted_via: "checkout",
    });
    expect(receipt.documents.map((d) => d.role)).toEqual([...CHECKOUT_LEGAL_ROLES]);
    expect(receipt.documents[0]).toMatchObject({ id: "right-suite-license", sha256: HASH_A, treatment: "agree" });
    expect(receipt.documents.find((d) => d.role === "privacy_notice")?.treatment).toBe("acknowledge");
  });

  it("normalizes an uppercase client sha256 but persists the configured hash", () => {
    const m = manifest();
    const value = claim(m);
    (value.documents as Array<Record<string, unknown>>)[0]!.sha256 = HASH_A.toUpperCase();
    expect(buildCheckoutLegalReceipt(m, value, AT).documents[0]!.sha256).toBe(HASH_A);
  });

  it.each([
    ["no claim", (v: CheckoutLegalAcceptance) => undefined as never, /affirmative receipt/i],
    ["wrong acceptance version", (v: CheckoutLegalAcceptance) => ({ ...v, acceptanceVersion: "old" }), /acceptanceVersion/i],
    ["bad eligibility", (v: CheckoutLegalAcceptance) => ({ ...v, eligibilityBasis: "reseller" }), /eligibility/i],
    ["authority not confirmed", (v: CheckoutLegalAcceptance) => ({ ...v, authorityConfirmed: false }), /authority/i],
    ["no affirmative agreement", (v: CheckoutLegalAcceptance) => ({ ...v, agreementStatement: "I have seen these." }), /affirmative agreement/i],
    ["documents not an array", (v: CheckoutLegalAcceptance) => ({ ...v, documents: "all" }), /documents must be an array/i],
  ])("rejects %s", (_name, mutate, pattern) => {
    const m = manifest();
    expect(() => buildCheckoutLegalReceipt(m, mutate(claim(m)), AT)).toThrow(pattern);
  });

  it("rejects duplicate, missing, unsupported and mismatched documents", () => {
    const m = manifest();
    const duplicate = claim(m);
    (duplicate.documents as unknown[]).push((duplicate.documents as unknown[])[0]);
    expect(() => buildCheckoutLegalReceipt(m, duplicate, AT)).toThrow(/duplicated/i);

    const missing = claim(m);
    missing.documents = (missing.documents as unknown[]).filter(
      (d) => (d as { role: string }).role !== "eula",
    );
    expect(() => buildCheckoutLegalReceipt(m, missing, AT)).toThrow(/required eula/i);

    const notices = claim(m);
    const thirdParty = m.documents.find((d) => d.role === "third_party_notices")!;
    (notices.documents as unknown[]).push({
      role: "third_party_notices", id: thirdParty.id, version: thirdParty.version,
      sha256: thirdParty.sha256, url: thirdParty.publicUrl,
    });
    expect(() => buildCheckoutLegalReceipt(m, notices, AT)).toThrow(/unsupported/i);

    for (const [field, wrong] of [["id", "other-doc"], ["version", "9.9"], ["sha256", "c".repeat(64)], ["url", "https://evil.test/"]] as const) {
      const value = claim(m);
      (value.documents as Array<Record<string, unknown>>)[1]![field] = wrong;
      expect(() => buildCheckoutLegalReceipt(m, value, AT), field).toThrow(/does not match/i);
    }
  });

  it("rejects an invalid server timestamp and a malformed manifest", () => {
    const m = manifest();
    expect(() => buildCheckoutLegalReceipt(m, claim(m), new Date(NaN))).toThrow(/timestamp/i);
    const broken = manifest();
    broken.documents = broken.documents.filter((d) => d.role !== "eula");
    expect(() => buildCheckoutLegalReceipt(broken, claim(m), AT)).toThrow(/eula/i);
  });
});
