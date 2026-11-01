import { describe, expect, it } from "vitest";
import { preferenceManifest, validateApplyResponse } from "./ContextRuntimePanel.js";

const manifest = {
  schema_version: "1.3",
  batch_id: "batch-1",
  created_at: "2026-09-22T00:00:00Z",
  installation_id: "install-1",
  canonical_pool_sha256: "pool-1",
  source_refs: [],
  source_session_ids: [],
  records: [],
  manifest_sha256: "manifest-1",
};

describe("Adapt response contracts", () => {
  it("accepts pending & adjudicated PreferenceManifestV1 responses", () => {
    expect(preferenceManifest(manifest)?.manifest_sha256).toBe("manifest-1");
    expect(
      preferenceManifest({ ...manifest, semantic_adjudication: { verdict: "accept" } })
        ?.manifest_sha256,
    ).toBe("manifest-1");
  });

  it("requires exact ApplyResponse & receipt for admitted records", () => {
    expect(() =>
      validateApplyResponse({
        response: { valid: true, errors: [], accepted_record_ids: [], manifest_hash: "manifest-1" },
      }),
    ).not.toThrow();
    expect(() =>
      validateApplyResponse({
        response: { valid: true, errors: [], accepted_record_ids: ["record-1"] },
        cortex_receipt: { receipt_id: "receipt-1" },
      }),
    ).not.toThrow();
    expect(() =>
      validateApplyResponse({
        response: { valid: true, errors: [], accepted_record_ids: ["record-1"] },
      }),
    ).toThrow("Cortex admission receipt");
    expect(() =>
      validateApplyResponse({ response: { valid: false, errors: ["rejected"] } }),
    ).toThrow("rejected");
  });
});
