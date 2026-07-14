import { isDeepStrictEqual } from "node:util";

import { verifyRuntimeArtifactEnvelope } from "./runtime-artifact-manifest.mjs";

const ASR_AUTHORITY = "heardright";
const ASR_CONSUMER = "scraperight";
const ASR_KIND = "asr-model";

export function assertAsrArtifactAdoption({ authorityEnvelope, consumerEnvelope, publicKey, trustedKeyId }) {
  const authority = verifyTrusted(authorityEnvelope, publicKey, trustedKeyId);
  const consumer = verifyTrusted(consumerEnvelope, publicKey, trustedKeyId);

  assertManifestRole(authority, ASR_AUTHORITY, "authority");
  assertManifestRole(consumer, ASR_CONSUMER, "consumer");
  for (const manifest of [authority, consumer]) {
    if (manifest.artifactKind !== ASR_KIND) throw new Error("ASR adoption manifest must be an asr-model");
    if (manifest.promotion.authorityAppKey !== ASR_AUTHORITY) throw new Error("ASR promotion authority must be heardright");
  }

  assertSame(authority.target, consumer.target, "target");
  assertSame(authority.object.filename, consumer.object.filename, "object.filename");
  assertSame(authority.object.sha256, consumer.object.sha256, "object.sha256");
  assertSame(authority.object.sizeBytes, consumer.object.sizeBytes, "object.sizeBytes");
  for (const field of ["runtime", "model", "tokenizer", "preprocessing", "license", "provenance"]) {
    assertSame(authority.versions[field], consumer.versions[field], `versions.${field}`);
  }
  assertSame(authority.provenance, consumer.provenance, "provenance");
  assertSame(authority.promotion, consumer.promotion, "promotion");
  return {
    sha256: authority.object.sha256,
    promotionId: authority.promotion.promotionId,
    target: authority.target,
  };
}

export function assertAsrAdapterPair(authority, consumer) {
  assertAdapter(authority, ASR_AUTHORITY, "authority");
  assertAdapter(consumer, ASR_CONSUMER, "consumer");
  for (const [field, expected] of [
    ["authorityAppKey", ASR_AUTHORITY],
    ["artifactKind", ASR_KIND],
    ["delivery", "private-r2"],
    ["entitlementTier", "pro"],
  ]) {
    assertSame(authority[field], expected, `authority adapter ${field}`);
    assertSame(consumer[field], expected, `consumer adapter ${field}`);
  }
  return true;
}

function verifyTrusted(envelope, publicKey, trustedKeyId) {
  if (envelope?.signature?.keyId !== trustedKeyId) throw new Error("untrusted runtime artifact key id");
  return verifyRuntimeArtifactEnvelope(envelope, publicKey);
}

function assertManifestRole(manifest, appKey, role) {
  if (manifest.entitlement.appKey !== appKey) throw new Error(`${role} manifest must belong to ${appKey}`);
}

function assertAdapter(value, appKey, role) {
  if (!value || value.schema !== 1 || value.appKey !== appKey || value.role !== role) {
    throw new Error(`invalid ${role} ASR adapter`);
  }
}

function assertSame(left, right, field) {
  if (!isDeepStrictEqual(left, right)) throw new Error(`ASR drift: ${field}`);
}
