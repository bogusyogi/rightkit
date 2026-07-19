import { describe, expect, it } from "vitest";
import {
  assertLegalManifest,
  createAcceptanceReceipt,
  materialAcceptanceDigest,
  needsLegalAcceptance,
  type LegalManifest,
} from "./index";

const HASH_A = "a".repeat(64);
const HASH_B = "b".repeat(64);

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

describe("legal manifest", () => {
  it("accepts the complete suite contract", () => {
    expect(assertLegalManifest(manifest())).toEqual(manifest());
  });

  it("rejects a missing required document role", () => {
    const value = manifest();
    value.documents = value.documents.filter((document) => document.role !== "product_schedule");
    expect(() => assertLegalManifest(value)).toThrow(/product_schedule/i);
  });

  it("rejects duplicate roles, traversal paths, and malformed hashes", () => {
    const duplicate = manifest();
    duplicate.documents.push({ ...duplicate.documents[0]!, id: "duplicate-license" });
    expect(() => assertLegalManifest(duplicate)).toThrow(/duplicate.*license/i);

    const traversal = manifest();
    traversal.documents[0]!.path = "../LICENSE.md";
    expect(() => assertLegalManifest(traversal)).toThrow(/relative|traversal|path/i);

    const badHash = manifest();
    badHash.documents[0]!.sha256 = "abc";
    expect(() => assertLegalManifest(badHash)).toThrow(/sha-?256/i);
  });

  it("requires agreement documents to be material and privacy to be acknowledged", () => {
    const value = manifest();
    value.documents.find((document) => document.role === "eula")!.material = false;
    expect(() => assertLegalManifest(value)).toThrow(/eula.*material/i);

    const privacy = manifest();
    privacy.documents.find((document) => document.role === "privacy_notice")!.treatment = "agree";
    expect(() => assertLegalManifest(privacy)).toThrow(/privacy.*acknowledge/i);
  });

  it("validates hash-bound appendices without treating them as assent documents", () => {
    const value = manifest();
    value.appendices = [{ id: "rust-licenses", title: "Rust licenses", sha256: HASH_A, path: "RUST_LICENSES.html" }];
    expect(assertLegalManifest(value)).toEqual(value);
    value.appendices[0]!.path = "../outside.txt";
    expect(() => assertLegalManifest(value)).toThrow(/path|traversal/i);
  });
});

describe("eligibility basis", () => {
  it("accepts only individual or enterprise", () => {
    const value = manifest();
    for (const basis of ["individual", "enterprise"] as const) {
      const receipt = createAcceptanceReceipt(value, {
        acceptedAt: "2026-07-16T12:34:56.000Z",
        surface: "checkout",
        eligibilityBasis: basis,
      });
      expect(receipt.eligibilityBasis).toBe(basis);
    }
  });
});

describe("acceptance receipts", () => {
  it("binds the receipt to app, version, surface, eligibility, and every presented hash", () => {
    const value = manifest();
    const receipt = createAcceptanceReceipt(value, {
      acceptedAt: "2026-07-16T12:34:56.000Z",
      surface: "first_run",
      eligibilityBasis: "enterprise",
    });
    expect(receipt).toMatchObject({
      schema: 2,
      appKey: "heardright",
      acceptanceVersion: value.acceptanceVersion,
      surface: "first_run",
      eligibilityBasis: "enterprise",
    });
    expect(receipt.documents).toHaveLength(value.documents.length);
    expect(needsLegalAcceptance(value, receipt)).toBe(false);
  });

  it("requires reacceptance for a material hash/version change but not a notice-only change", () => {
    const value = manifest();
    const receipt = createAcceptanceReceipt(value, {
      acceptedAt: "2026-07-16T12:34:56.000Z",
      surface: "first_run",
      eligibilityBasis: "individual",
    });

    const materialChange = manifest();
    materialChange.documents.find((document) => document.role === "eula")!.sha256 = "c".repeat(64);
    expect(needsLegalAcceptance(materialChange, receipt)).toBe(true);

    const noticeChange = manifest();
    noticeChange.documents.find((document) => document.role === "third_party_notices")!.sha256 = "d".repeat(64);
    expect(needsLegalAcceptance(noticeChange, receipt)).toBe(false);

    const versionChange = manifest();
    versionChange.acceptanceVersion = "heardright-2026-08-01-v2";
    expect(needsLegalAcceptance(versionChange, receipt)).toBe(true);
  });

  it("rejects a receipt for another app", () => {
    const value = manifest();
    const receipt = createAcceptanceReceipt(value, {
      acceptedAt: "2026-07-16T12:34:56.000Z",
      surface: "checkout",
      eligibilityBasis: "enterprise",
    });
    expect(needsLegalAcceptance({ ...value, appKey: "mailright" }, receipt)).toBe(true);
  });
});

describe("materialAcceptanceDigest", () => {
  it("is stable across notices/appendix churn", () => {
    const base = manifest();
    const digest = materialAcceptanceDigest(base);

    const noticesChanged = manifest();
    noticesChanged.documents.find((document) => document.role === "third_party_notices")!.sha256 = "c".repeat(64);
    expect(materialAcceptanceDigest(noticesChanged)).toBe(digest);

    const privacyChanged = manifest();
    privacyChanged.documents.find((document) => document.role === "privacy_notice")!.sha256 = "c".repeat(64);
    privacyChanged.documents.find((document) => document.role === "privacy_notice")!.version = "1.1";
    expect(materialAcceptanceDigest(privacyChanged)).toBe(digest);

    const appendixChanged = manifest();
    appendixChanged.appendices = [{ id: "rust-licenses", title: "Rust licenses", sha256: HASH_A, path: "RUST_LICENSES.html" }];
    expect(materialAcceptanceDigest(appendixChanged)).toBe(digest);

    const appendixAdded = manifest();
    appendixAdded.appendices = [
      { id: "rust-licenses", title: "Rust licenses", sha256: HASH_A, path: "RUST_LICENSES.html" },
      { id: "npm-licenses", title: "npm licenses", sha256: HASH_B, path: "NPM_LICENSES.html" },
    ];
    expect(materialAcceptanceDigest(appendixAdded)).toBe(digest);
  });

  it("changes when a material document's sha256, id, or version changes", () => {
    const base = manifest();
    const digest = materialAcceptanceDigest(base);

    const shaChanged = manifest();
    shaChanged.documents.find((document) => document.role === "eula")!.sha256 = "c".repeat(64);
    expect(materialAcceptanceDigest(shaChanged)).not.toBe(digest);

    const versionChanged = manifest();
    versionChanged.documents.find((document) => document.role === "license")!.version = "1.1";
    expect(materialAcceptanceDigest(versionChanged)).not.toBe(digest);

    const idChanged = manifest();
    idChanged.documents.find((document) => document.role === "acceptable_use")!.id = "right-suite-acceptable-use-v2";
    expect(materialAcceptanceDigest(idChanged)).not.toBe(digest);
  });

  it("changes when a material document is added or removed", () => {
    const base = manifest();
    const digest = materialAcceptanceDigest(base);

    const missingRole = manifest();
    missingRole.documents = missingRole.documents.filter((document) => document.role !== "product_schedule");
    expect(() => materialAcceptanceDigest(missingRole)).toThrow(/product_schedule/i);

    // A material document swapped in place (same role, different id/version/sha) counts as a change.
    const swapped = manifest();
    const productSchedule = swapped.documents.find((document) => document.role === "product_schedule")!;
    productSchedule.id = "right-suite-product-schedule-v2";
    productSchedule.version = "2.0";
    productSchedule.sha256 = "c".repeat(64);
    expect(materialAcceptanceDigest(swapped)).not.toBe(digest);
  });

  it("is independent of document array order and object key order", () => {
    const base = manifest();
    const digest = materialAcceptanceDigest(base);

    const reordered = manifest();
    reordered.documents = [...reordered.documents].reverse();
    expect(materialAcceptanceDigest(reordered)).toBe(digest);

    const rekeyed = manifest();
    rekeyed.documents = rekeyed.documents.map((document) => {
      const { sha256, role, id, version, title, path, publicUrl, treatment, material } = document;
      // Same properties, deliberately different insertion order.
      return { sha256, role, id, version, title, path, publicUrl, treatment, material };
    });
    expect(materialAcceptanceDigest(rekeyed)).toBe(digest);
  });
});
