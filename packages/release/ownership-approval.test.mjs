import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { __testOnly, canonicalJson, canonicalRepositoryIdentity, createDraft, keyIdForPublicKey, provenanceContentDigest, sha256, validateOwnershipFloor, verifyException, verifyProtectedProvenance, verifyRepositoryRegistration } from "./ownership-approval.mjs";

const finding = { fingerprint: "oauth.device", file: "src/auth.rs", line: 12, text: "device grant endpoint" };
const context = { repositoryIdentity: "https://github.com/orthic-labs/example", catalogDigest: "a".repeat(64), findings: [finding], today: "2026-10-07", scannerVersion: "2", catalogVersion: "4" };
const keys = Object.fromEntries(["windows", "macos"].map((host) => {
  const kp = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  const publicKeyPem = kp.publicKey.export({ type: "spki", format: "pem" });
  return [host, { keyId: keyIdForPublicKey(host, publicKeyPem), privateKey: kp.privateKey, publicKeyPem }];
}));
const trust = { schema: 1, minimumScannerVersion: "1", minimumCatalogVersion: "3", revokedKeyIds: [], verifiedTrackingEvidence: [sha256({ repositoryIdentity: context.repositoryIdentity, requestId: "REQ-123", trackingRef: { url: "https://github.com/orthic-labs/example/issues/123", id: "123", verified: true } })], repositories: [{ repositoryIdentity: context.repositoryIdentity, roots: ["."], visibility: "public" }], provenance: [], hosts: Object.fromEntries(Object.entries(keys).map(([host, k]) => [host, { keyId: k.keyId, publicKeyPem: k.publicKeyPem }])) };
function signed(overrides = {}, signingHost = "windows") {
  const draft = createDraft({ repositoryIdentity: context.repositoryIdentity, catalogDigest: context.catalogDigest, fingerprint: finding.fingerprint, findings: [finding], paths: [finding.file], reason: "bounded migration", owner: "Example", expires: "2026-12-31", requestId: "REQ-123", trackingRef: { url: "https://github.com/orthic-labs/example/issues/123", id: "123", verified: true }, ...overrides });
  const data = Buffer.from(canonicalJson(draft.payload));
  return { ...draft, signature: { keyId: keys[signingHost].keyId, host: signingHost, value: sign("sha256", data, keys[signingHost].privateKey).toString("base64") } };
}

test("valid signed approval matches exact repo, catalog, path & occurrence baseline", () => {
  assert.deepEqual(verifyException(signed(), { ...context, trust }), { ok: true, problems: [] });
  assert.equal(createDraft({ repositoryIdentity: "git@github.com:Orthic-Labs/Example.git", catalogDigest: context.catalogDigest, fingerprint: finding.fingerprint, findings: [finding], paths: [finding.file], reason: "migration", owner: "Example", expires: "2026-12-31", requestId: "REQ-123", trackingRef: { url: "https://github.com/orthic-labs/example/issues/123", id: "123", verified: true } }).payload.repositoryIdentity, context.repositoryIdentity);
});
test("unsigned, malformed, and payload-tampered records fail", () => {
  const draft = createDraft({ repositoryIdentity: context.repositoryIdentity, catalogDigest: context.catalogDigest, fingerprint: finding.fingerprint, findings: [finding], paths: [finding.file], reason: "r", owner: "o", expires: "2026-12-31", requestId: "REQ-123", trackingRef: { url: "https://example.com/t/1", id: "1", verified: true } });
  assert.match(verifyException(draft, { ...context, trust }).problems.join(" "), /unsigned/);
  const changed = signed(); changed.payload.reason = "broadened";
  assert.match(verifyException(changed, { ...context, trust }).problems.join(" "), /invalid signature/);
});
test("changed identity, catalog, path occurrence, expiry, or revoked key fail", () => {
  const record = signed();
  for (const change of [
    { ...context, repositoryIdentity: "github.com/other/repo" },
    { ...context, catalogDigest: "b".repeat(64) },
    { ...context, findings: [{ ...finding, file: "src/other.rs" }] },
    { ...context, today: "2027-01-01" },
  ]) assert.equal(verifyException(record, { ...change, trust }).ok, false);
  assert.match(verifyException(record, { ...context, trust: { ...trust, revokedKeyIds: [keys.windows.keyId] } }).problems.join(" "), /revoked/);
  assert.match(verifyException(record, { ...context, today: "2026-02-31", trust }).problems.join(" "), /verification date/);
  assert.match(verifyException(signed({ expires: "2026-02-31" }), { ...context, trust }).problems.join(" "), /expiry date/);
});
test("glob paths, unverified tracking, and missing second-host enrollment fail", () => {
  const broad = signed({ paths: ["src/**"] });
  assert.match(verifyException(broad, { ...context, trust }).problems.join(" "), /exact/);
  const tracking = signed({ trackingRef: { url: "https://example.com/1", id: "1", verified: false } });
  assert.match(verifyException(tracking, { ...context, trust }).problems.join(" "), /verified/);
  const oneHost = { ...trust, hosts: { windows: trust.hosts.windows } };
  assert.match(verifyException(signed(), { ...context, trust: oneHost }).problems.join(" "), /Windows and macOS/);
  const unregistered = { ...trust, verifiedTrackingEvidence: [] };
  assert.match(verifyException(signed(), { ...context, trust: unregistered }).problems.join(" "), /protected verification evidence/);
  const reusedKey = { ...trust, hosts: { windows: trust.hosts.windows, macos: trust.hosts.windows } };
  assert.match(verifyException(signed(), { ...context, trust: reusedKey }).problems.join(" "), /distinct/);
});
test("protected scanner and catalog floors reject downgrade and accept current versions", () => {
  assert.deepEqual(validateOwnershipFloor(trust, { scannerVersion: "2", catalogVersion: "4" }), { ok: true, problems: [] });
  assert.equal(validateOwnershipFloor(trust, { scannerVersion: "0.9", catalogVersion: "4" }).ok, false);
  assert.equal(validateOwnershipFloor(trust, { scannerVersion: "2", catalogVersion: "2" }).ok, false);
  assert.match(validateOwnershipFloor(trust, { scannerVersion: "2" }).problems.join(" "), /catalog version missing/);
  assert.match(validateOwnershipFloor(trust, { catalogVersion: "4" }).problems.join(" "), /scanner version missing/);
  assert.equal(verifyException(signed(), { ...context, scannerVersion: undefined, trust }).ok, false);
});
test("byte hashing uses raw SHA-256 bytes and public-key IDs use SPKI DER", () => {
  assert.equal(sha256(Buffer.from("abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
  assert.equal(keyIdForPublicKey("windows", keys.windows.publicKeyPem), keys.windows.keyId);
});
test("protected repository registrations canonicalize identity and bound scan roots", () => {
  assert.equal(canonicalRepositoryIdentity("git@github.com:Orthic-Labs/Example.git"), context.repositoryIdentity);
  assert.equal(verifyRepositoryRegistration(trust, context.repositoryIdentity, { files: ["src/auth.rs"] }).ok, true);
  assert.equal(verifyRepositoryRegistration(trust, context.repositoryIdentity, { files: ["../other.rs"] }).ok, false);
  assert.equal(verifyRepositoryRegistration({ ...trust, repositories: [] }, context.repositoryIdentity).ok, false);
});
test("protected provenance requires exact path set and content digest", () => {
  const files = [{ path: "vendor/lib.rs", content: "third-party source" }];
  const entry = { repositoryIdentity: context.repositoryIdentity, paths: ["vendor/lib.rs"], contentDigest: provenanceContentDigest(files), kind: "third-party", requestId: "REQ-VEND-1" };
  const withProvenance = { ...trust, provenance: [entry] };
  assert.equal(verifyProtectedProvenance(withProvenance, { repositoryIdentity: context.repositoryIdentity, paths: entry.paths, files, kind: entry.kind, requestId: entry.requestId }).ok, true);
  assert.equal(verifyProtectedProvenance(withProvenance, { repositoryIdentity: context.repositoryIdentity, paths: entry.paths, files: [{ ...files[0], content: "changed" }], kind: entry.kind, requestId: entry.requestId }).ok, false);
  assert.equal(verifyProtectedProvenance(withProvenance, { repositoryIdentity: context.repositoryIdentity, paths: ["vendor"], files, kind: entry.kind, requestId: entry.requestId }).ok, false);
});
test("protected trust audit includes trust file itself and rejects a user-owned temporary file", { skip: process.platform !== "win32" }, () => {
  const dir = mkdtempSync(path.join(tmpdir(), "rk-trust-audit-"));
  const file = path.join(dir, "trust.json");
  try {
    writeFileSync(file, "{}\n");
    const result = __testOnly.protectedLocation(file, "win32");
    assert.equal(result.ok, false);
    assert.ok(result.problems.some((problem) => problem.includes(file)) || result.problems.some((problem) => /owner/.test(problem)));
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
