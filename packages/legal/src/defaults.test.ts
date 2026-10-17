import { describe, expect, it } from "vitest";
import {
  DEFAULT_DATA_STATEMENT,
  RIGHT_SUITE_COMPANY_FACTS,
  assertLegalManifest,
  defaultPolicyDescriptors,
  materialAcceptanceDigest,
  renderDefaultPolicies,
  renderDefaultPolicy,
  resolveCompanyFacts,
  type DefaultPolicyInput,
  type LegalManifest,
} from "./index";

const base: DefaultPolicyInput = {
  appName: "HeardRight",
  supportEmail: "hello@heardright.app",
  refundPolicy: { kind: "window", days: 14 },
};

function manifest(input: DefaultPolicyInput): LegalManifest {
  const documents = defaultPolicyDescriptors(input, { version: "1", publicUrlBase: "https://example.app/legal/", pathPrefix: "legal/" });
  const filler = (role: "acceptable_use" | "third_party_notices") => ({
    id: role,
    role,
    title: role,
    version: "1",
    sha256: "c".repeat(64),
    path: `legal/${role}.md`,
    publicUrl: `https://example.app/legal/${role}/`,
    treatment: role === "acceptable_use" ? ("agree" as const) : ("notice" as const),
    material: role === "acceptable_use",
  });
  return {
    schema: 2,
    suite: "right-suite-desktop",
    appKey: "heardright",
    productName: "HeardRight",
    licensor: "Damned Ventures LLC",
    effectiveDate: "2026-10-17",
    acceptanceVersion: "1",
    documents: [...documents, filler("acceptable_use"), filler("third_party_notices")],
  };
}

describe("RIGHT_SUITE_COMPANY_FACTS", () => {
  it("is frozen and carries minimum age 18", () => {
    expect(Object.isFrozen(RIGHT_SUITE_COMPANY_FACTS)).toBe(true);
    expect(Object.isFrozen(RIGHT_SUITE_COMPANY_FACTS.address)).toBe(true);
    expect(RIGHT_SUITE_COMPANY_FACTS.minimumAge).toBe(18);
    expect(RIGHT_SUITE_COMPANY_FACTS.legalName).toBe("Damned Ventures LLC");
  });

  it("has no default support email or refund policy", () => {
    expect("supportEmail" in RIGHT_SUITE_COMPANY_FACTS).toBe(false);
    expect("refundPolicy" in RIGHT_SUITE_COMPANY_FACTS).toBe(false);
  });

  it("requires supportEmail", () => {
    expect(() => resolveCompanyFacts({ supportEmail: " " })).toThrow(/supportEmail/);
  });
});

describe("default policies", () => {
  it("renders eula, privacy, refund and terms from the facts", () => {
    const docs = renderDefaultPolicies(base);
    expect(docs.map((d) => d.kind)).toEqual(["eula", "privacy", "refund", "terms"]);
    const eula = docs[0]!.content;
    expect(eula).toContain("Damned Ventures LLC");
    expect(eula).toContain("at least 18 years old");
    expect(eula).toContain("169 Madison Ave");
    expect(docs[1]!.content).toContain(DEFAULT_DATA_STATEMENT);
  });

  it("renders the days-window refund text", () => {
    expect(renderDefaultPolicy("refund", base).content).toContain("14-day window");
  });

  it("renders the none-with-trial refund text", () => {
    const doc = renderDefaultPolicy("refund", { ...base, appName: "GenRight", refundPolicy: { kind: "none", trialDays: 30 } });
    expect(doc.content).toContain("30-day Pro trial");
    expect(doc.content).toContain("No refunds except where the law requires.");
    expect(doc.content).not.toContain("14");
  });

  it("requires a refund policy", () => {
    expect(() => renderDefaultPolicies({ ...base, refundPolicy: undefined as never })).toThrow(/refundPolicy/);
    expect(() => renderDefaultPolicies({ ...base, refundPolicy: { kind: "window", days: 0 } })).toThrow(/days/);
  });

  it("applies per-app overrides", () => {
    const privacy = renderDefaultPolicy("privacy", { ...base, dataStatement: "data we receive: none" });
    expect(privacy.content).toContain("data we receive: none");
    const terms = renderDefaultPolicy("terms", { ...base, append: { terms: "Extra clause." } });
    expect(terms.content).toContain("Extra clause.");
    expect(renderDefaultPolicy("terms", { ...base, replace: { terms: "Custom." } }).content).toBe("Custom.\n");
  });

  it("is deterministic", () => {
    expect(renderDefaultPolicies(base)).toEqual(renderDefaultPolicies(base));
  });
});

describe("manifest and digest", () => {
  it("produces descriptors that satisfy assertLegalManifest", () => {
    expect(() => assertLegalManifest(manifest(base))).not.toThrow();
  });

  it("a facts change moves the material digest", () => {
    const before = materialAcceptanceDigest(manifest(base));
    const after = materialAcceptanceDigest(manifest({ ...base, facts: { governingLaw: "Delaware" } }));
    expect(after).not.toBe(before);
  });

  it("a refund policy change moves the material digest", () => {
    const before = materialAcceptanceDigest(manifest(base));
    const after = materialAcceptanceDigest(manifest({ ...base, refundPolicy: { kind: "none" } }));
    expect(after).not.toBe(before);
  });

  it("a data statement change does not move the material digest", () => {
    const before = materialAcceptanceDigest(manifest(base));
    const after = materialAcceptanceDigest(manifest({ ...base, dataStatement: "data we receive: nothing" }));
    expect(after).toBe(before);
  });
});
