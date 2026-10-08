import assert from "node:assert/strict";
import { generateKeyPairSync } from "node:crypto";
import test from "node:test";

import {
  buildRuntimeArtifactManifest,
  RIGHT_SUITE_APP_KEYS,
  runtimeArtifactManifestPointerKey,
  runtimeArtifactObjectKey,
  signRuntimeArtifactManifest,
  validateRuntimeArtifactManifest,
  verifyRuntimeArtifactEnvelope,
} from "./runtime-artifact-manifest.mjs";

const digest = "a".repeat(64);
const evidenceDigest = "b".repeat(64);

test("runtime artifact app registry includes existing Suite and QTS consumers", () => {
  assert.deepEqual([...RIGHT_SUITE_APP_KEYS].sort(), [
    "coderight", "genright", "heardright", "mailright", "scraperight", "screenright", "viewright", "voiceright",
  ]);
});

test("GenRight TTS models use its immutable namespace and require private Pro delivery", () => {
  const input = {
    artifactKind: "tts-model",
    filename: "qwen3-tts-1.7b-customvoice-talker-q8_0.gguf",
    entitlement: { appKey: "genright", tier: "pro" },
  };
  const manifest = validManifest(input);
  assert.equal(manifest.object.r2Key, `genright/runtime-artifacts/objects/sha256/${digest}/${input.filename}`);
  assert.equal(manifest.pointerKey, "genright/runtime-artifacts/tts-model/windows/x86_64/current/manifest.json");
  assert.throws(() => validManifest({
    ...input,
    entitlement: { appKey: "genright", tier: "public" },
    distribution: { delivery: "public-r2", bucket: "rightapps-downloads" },
  }), /model.*private-r2.*pro/i);
});

function validManifest(overrides = {}) {
  return buildRuntimeArtifactManifest({
    artifactKind: "asr-model",
    filename: "parakeet-tdt-v3.onnx",
    sha256: digest,
    sizeBytes: 42,
    entitlement: { appKey: "scraperight", tier: "pro" },
    distribution: { delivery: "private-r2", bucket: "rightapps-updates" },
    target: { os: "windows", arch: "x86_64" },
    versions: {
      runtime: "onnxruntime-1.22.0",
      model: "parakeet-tdt-0.6b-v3",
      tokenizer: "sentencepiece-2026-07-14",
      preprocessing: "rightkit-asr-0.1.0",
      license: "cc-by-4.0",
      provenance: "heardright-approved-2026-07-14",
    },
    provenance: {
      source: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3",
      sourceRevision: "0123456789abcdef",
      licenseId: "CC-BY-4.0",
      noticeSha256: "c".repeat(64),
    },
    promotion: {
      authorityAppKey: "heardright",
      promotionId: "hr-asr-2026-07-14-001",
      promotedAt: "2026-07-14T12:00:00.000Z",
      evidenceSha256: evidenceDigest,
    },
    ...overrides,
  });
}

test("builds exact stable-pointer and immutable content-addressed object keys", () => {
  const manifest = validManifest();
  assert.equal(
    manifest.pointerKey,
    "scraperight/runtime-artifacts/asr-model/windows/x86_64/current/manifest.json",
  );
  assert.equal(
    manifest.object.r2Key,
    `scraperight/runtime-artifacts/objects/sha256/${digest}/parakeet-tdt-v3.onnx`,
  );
  assert.equal(runtimeArtifactManifestPointerKey(manifest), manifest.pointerKey);
  assert.equal(runtimeArtifactObjectKey(manifest), manifest.object.r2Key);
  assert.equal(validateRuntimeArtifactManifest(manifest), manifest);
});

test("signs and verifies the canonical manifest with Ed25519", () => {
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const envelope = signRuntimeArtifactManifest(validManifest(), privateKey, "rightapps-runtime-2026-01");
  assert.equal(envelope.signature.algorithm, "Ed25519");
  assert.equal(verifyRuntimeArtifactEnvelope(envelope, publicKey).artifactKind, "asr-model");

  const tampered = structuredClone(envelope);
  tampered.manifest.object.sizeBytes += 1;
  assert.throws(() => verifyRuntimeArtifactEnvelope(tampered, publicKey), /signature verification failed/);
});

test("fails closed on missing or contradictory digest, provenance, version, scope, and promotion metadata", () => {
  const mutations = [
    (m) => delete m.versions.tokenizer,
    (m) => { m.versions.license = ""; },
    (m) => delete m.provenance.sourceRevision,
    (m) => { m.provenance.licenseId = "MIT"; },
    (m) => { m.entitlement.tier = "free"; },
    (m) => { m.object.sha256 = "d".repeat(64); },
    (m) => { m.object.sizeBytes = 0; },
    (m) => { m.pointerKey = "scraperight/runtime-artifacts/asr-model/windows/x86_64/v1/manifest.json"; },
    (m) => { m.promotion.authorityAppKey = "unknown"; },
    (m) => { m.promotion.evidenceSha256 = "not-a-digest"; },
  ];
  for (const mutate of mutations) {
    const manifest = structuredClone(validManifest());
    mutate(manifest);
    assert.throws(() => validateRuntimeArtifactManifest(manifest));
  }
});

test("rejects unknown fields and path traversal instead of guessing", () => {
  const extra = structuredClone(validManifest());
  extra.secretUrl = "https://example.invalid/token";
  assert.throws(() => validateRuntimeArtifactManifest(extra), /unknown field/);
  assert.throws(() => validManifest({ filename: "../model.onnx" }), /filename/);
});

test("requires an explicit compatible entitlement and delivery lane per artifact", () => {
  const publicMedia = validManifest({
    artifactKind: "media-runtime",
    entitlement: { appKey: "scraperight", tier: "public" },
    distribution: { delivery: "public-r2", bucket: "rightapps-downloads" },
  });
  assert.equal(publicMedia.distribution.delivery, "public-r2");

  const bundledOrt = validManifest({
    artifactKind: "ort-runtime",
    entitlement: { appKey: "viewright", tier: "bundled" },
    distribution: { delivery: "bundled", bucket: null },
  });
  assert.equal(bundledOrt.pointerKey, null);
  assert.equal(bundledOrt.object.r2Key, null);

  assert.throws(() => validManifest({
    entitlement: { appKey: "scraperight", tier: "public" },
    distribution: { delivery: "public-r2", bucket: "rightapps-downloads" },
  }), /model.*private-r2.*pro/i);
  assert.throws(() => validManifest({
    artifactKind: "media-runtime",
    entitlement: { appKey: "scraperight", tier: "public" },
    distribution: { delivery: "private-r2", bucket: "rightapps-updates" },
  }), /contradict/);
});
