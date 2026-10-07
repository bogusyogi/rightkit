import assert from "node:assert/strict";
import { generateKeyPairSync } from "node:crypto";
import test from "node:test";
import { keyIdForPublicKey, prepareTrustCandidate, sha256, validateEnrollmentExport, validateTrustCandidate } from "./trust-schema.mjs";

function enrollment(host) {
  const pair = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  const publicKeyPem = pair.publicKey.export({ type: "spki", format: "pem" });
  return { host, keyId: keyIdForPublicKey(host, publicKeyPem), publicKeyPem };
}

const baseline = {
  schema: 1,
  minimumScannerVersion: "2.4",
  minimumCatalogVersion: "7",
  minimumCatalogDigest: "a".repeat(64),
  revokedKeyIds: [],
  verifiedTrackingEvidence: [sha256("evidence")],
  repositories: [{ repositoryIdentity: "https://github.com/orthic-labs/example", roots: ["src", "tools"], visibility: "public" }],
  provenance: [{ repositoryIdentity: "https://github.com/orthic-labs/example", paths: ["vendor/lib.rs"], contentDigest: "b".repeat(64), kind: "third-party", requestId: "REQ-123" }],
};

test("prepares host trust entries while preserving protected baseline policy", () => {
  const win = enrollment("windows"), mac = enrollment("macos");
  const candidate = prepareTrustCandidate({ baseline, windowsExport: win, macosExport: mac });
  assert.deepEqual(candidate.repositories, baseline.repositories);
  assert.deepEqual(candidate.provenance, baseline.provenance);
  assert.equal(candidate.minimumScannerVersion, baseline.minimumScannerVersion);
  assert.equal(candidate.minimumCatalogVersion, baseline.minimumCatalogVersion);
  assert.equal(candidate.minimumCatalogDigest, baseline.minimumCatalogDigest);
  assert.deepEqual(candidate.hosts.windows, { keyId: win.keyId, publicKeyPem: win.publicKeyPem });
  assert.deepEqual(candidate.hosts.macos, { keyId: mac.keyId, publicKeyPem: mac.publicKeyPem });
  assert.equal(validateTrustCandidate(candidate), candidate);
});

test("rejects mismatched host, extra/private material, invalid key IDs & non-P256 keys", () => {
  const win = enrollment("windows");
  assert.throws(() => validateEnrollmentExport({ ...win, privateKey: "secret" }, "windows"), /exactly/);
  assert.throws(() => validateEnrollmentExport({ ...win, publicKeyPem: win.publicKeyPem.replace("PUBLIC KEY", "PRIVATE KEY") }, "windows"), /public SubjectPublicKeyInfo/);
  assert.throws(() => validateEnrollmentExport({ ...win, host: "macos" }, "windows"), /host or key/);
  assert.throws(() => validateEnrollmentExport({ ...win, keyId: "win-fake" }, "windows"), /key ID/);
  const rsa = generateKeyPairSync("rsa", { modulusLength: 2048 }).publicKey.export({ type: "spki", format: "pem" });
  assert.throws(() => validateEnrollmentExport({ host: "windows", keyId: "win-x", publicKeyPem: rsa }, "windows"), /P-256/);
});

test("rejects reused host keys, incomplete baseline & pre-enrolled host entries", () => {
  const win = enrollment("windows"), mac = enrollment("macos");
  assert.throws(() => prepareTrustCandidate({ baseline, windowsExport: win, macosExport: { ...win, host: "macos", keyId: keyIdForPublicKey("macos", win.publicKeyPem) } }), /distinct keys/);
  assert.throws(() => prepareTrustCandidate({ baseline: { ...baseline, repositories: [{ repositoryIdentity: "https://github.com/orthic-labs/example", roots: [], visibility: "public" }] }, windowsExport: win, macosExport: mac }), /baseline trust is invalid/);
  assert.throws(() => prepareTrustCandidate({ baseline: { ...baseline, hosts: { windows: { keyId: "existing" } } }, windowsExport: win, macosExport: mac }), /already contains/);
  assert.throws(() => prepareTrustCandidate({ baseline: { ...baseline, revokedKeyIds: [win.keyId] }, windowsExport: win, macosExport: mac }), /revocation list/);
  assert.throws(() => validateTrustCandidate({ ...prepareTrustCandidate({ baseline, windowsExport: win, macosExport: mac }), hosts: { windows: { ...win, extra: "ignored" }, macos: { keyId: mac.keyId, publicKeyPem: mac.publicKeyPem } } }), /exactly/);
});
