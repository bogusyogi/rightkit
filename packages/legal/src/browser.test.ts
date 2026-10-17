import { describe, expect, it } from "vitest";
import { materialAcceptanceDigestAsync, sha256HexAsync } from "./browser";
import { materialAcceptanceDigest, sha256Hex, type LegalManifest } from "./index";

const HASH = "a".repeat(64);

describe("browser entry", () => {
  it("hashes like the Node entry", async () => {
    expect(await sha256HexAsync("hello")).toBe(sha256Hex("hello"));
  });

  it("matches materialAcceptanceDigest", async () => {
    const m: LegalManifest = {
      schema: 2,
      suite: "right-suite-desktop",
      appKey: "ember",
      productName: "Ember",
      licensor: "Orthic Labs",
      effectiveDate: "2026-10-17",
      acceptanceVersion: "ember-v1",
      profile: "oss",
      documents: [
        ["privacy_notice", "acknowledge"],
        ["third_party_notices", "notice"],
      ].map(([role, treatment]) => ({
        id: `ember-${role}`,
        role: role as "privacy_notice",
        title: role,
        version: "1",
        sha256: HASH,
        path: `legal/${role}.md`,
        publicUrl: `https://example.com/${role}`,
        treatment: treatment as "acknowledge",
        material: false,
      })),
    };
    expect(await materialAcceptanceDigestAsync(m)).toBe(materialAcceptanceDigest(m));
  });
});
