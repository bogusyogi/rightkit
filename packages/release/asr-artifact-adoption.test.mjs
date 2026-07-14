import assert from "node:assert/strict";
import { generateKeyPairSync } from "node:crypto";
import test from "node:test";

import { assertAsrAdapterPair, assertAsrArtifactAdoption } from "./asr-artifact-adoption.mjs";
import { buildRuntimeArtifactManifest, signRuntimeArtifactManifest } from "./runtime-artifact-manifest.mjs";

const digest = "a".repeat(64);
const notice = "b".repeat(64);
const evidence = "c".repeat(64);
const { privateKey, publicKey } = generateKeyPairSync("ed25519");

function envelope(appKey, overrides = {}) {
  const manifest = buildRuntimeArtifactManifest({
    artifactKind: "asr-model",
    entitlement: { appKey, tier: "pro" },
    distribution: { delivery: "private-r2", bucket: "rightapps-updates" },
    target: { os: "windows", arch: "x86_64" },
    filename: "parakeet-unified-en-0.6b.tar.zst",
    sha256: digest,
    sizeBytes: 1234,
    versions: {
      runtime: "rightkit-asr@0.1.0",
      model: "parakeet-unified-en-0.6b",
      tokenizer: "sha256:tokenizer-v1",
      preprocessing: "heardright-frontend-v1",
      license: "CC-BY-4.0",
      provenance: "nvidia/parakeet-tdt-0.6b-v3",
    },
    provenance: {
      source: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3",
      sourceRevision: "revision-1",
      licenseId: "CC-BY-4.0",
      noticeSha256: notice,
    },
    promotion: {
      authorityAppKey: "heardright",
      promotionId: "hr-asr-2026-07-14",
      promotedAt: "2026-07-14T00:00:00.000Z",
      evidenceSha256: evidence,
    },
    ...overrides,
  });
  return signRuntimeArtifactManifest(manifest, privateKey, "rightkit-runtime-artifacts-2026-07");
}

test("accepts app-scoped pointers with one exact HeardRight-certified ASR identity", () => {
  const result = assertAsrArtifactAdoption({
    authorityEnvelope: envelope("heardright"),
    consumerEnvelope: envelope("scraperight"),
    publicKey,
    trustedKeyId: "rightkit-runtime-artifacts-2026-07",
  });

  assert.equal(result.sha256, digest);
  assert.equal(result.promotionId, "hr-asr-2026-07-14");
});

test("rejects a validly signed ScrapeRight manifest with ASR drift", () => {
  const drifted = envelope("scraperight", {
    versions: {
      runtime: "rightkit-asr@0.1.1",
      model: "parakeet-unified-en-0.6b",
      tokenizer: "sha256:tokenizer-v1",
      preprocessing: "heardright-frontend-v1",
      license: "CC-BY-4.0",
      provenance: "nvidia/parakeet-tdt-0.6b-v3",
    },
  });

  assert.throws(() => assertAsrArtifactAdoption({
    authorityEnvelope: envelope("heardright"),
    consumerEnvelope: drifted,
    publicKey,
    trustedKeyId: "rightkit-runtime-artifacts-2026-07",
  }), /ASR drift: versions\.runtime/);
});

test("rejects wrong roles, authority, target, signature key, and artifact kind", () => {
  const base = {
    authorityEnvelope: envelope("heardright"),
    consumerEnvelope: envelope("scraperight"),
    publicKey,
    trustedKeyId: "rightkit-runtime-artifacts-2026-07",
  };
  assert.throws(() => assertAsrArtifactAdoption({ ...base, authorityEnvelope: envelope("scraperight") }), /authority manifest must belong to heardright/);
  assert.throws(() => assertAsrArtifactAdoption({ ...base, consumerEnvelope: envelope("heardright") }), /consumer manifest must belong to scraperight/);
  assert.throws(() => assertAsrArtifactAdoption({ ...base, trustedKeyId: "rotated-without-app-update" }), /untrusted runtime artifact key id/);
  assert.throws(() => assertAsrArtifactAdoption({
    ...base,
    consumerEnvelope: envelope("scraperight", { target: { os: "darwin", arch: "aarch64" } }),
  }), /ASR drift: target/);
  assert.throws(() => assertAsrArtifactAdoption({
    ...base,
    consumerEnvelope: envelope("scraperight", { artifactKind: "ocr-model" }),
  }), /must be an asr-model/);
});

test("rejects a consumer signed by a different key", () => {
  const other = generateKeyPairSync("ed25519");
  const consumer = signRuntimeArtifactManifest(
    envelope("scraperight").manifest,
    other.privateKey,
    "rightkit-runtime-artifacts-2026-07",
  );
  assert.throws(() => assertAsrArtifactAdoption({
    authorityEnvelope: envelope("heardright"),
    consumerEnvelope: consumer,
    publicKey,
    trustedKeyId: "rightkit-runtime-artifacts-2026-07",
  }), /signature verification failed/);
});

test("locks the thin app adapters to HeardRight authority and private Pro delivery", () => {
  const authority = {
    schema: 1, appKey: "heardright", role: "authority", authorityAppKey: "heardright",
    artifactKind: "asr-model", delivery: "private-r2", entitlementTier: "pro",
  };
  const consumer = { ...authority, appKey: "scraperight", role: "consumer" };
  assert.equal(assertAsrAdapterPair(authority, consumer), true);
  assert.throws(() => assertAsrAdapterPair(authority, { ...consumer, authorityAppKey: "scraperight" }), /ASR drift/);
});
