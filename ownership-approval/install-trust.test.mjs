import assert from "node:assert/strict";
import { generateKeyPairSync } from "node:crypto";
import test from "node:test";
import { evaluateWindowsAcl, keyIdForPublicKey, sha256 } from "./trust-schema.mjs";
import { validatePinnedCandidate } from "./install-trust.mjs";

function enrollment(host) {
  const pair = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  const publicKeyPem = pair.publicKey.export({ type: "spki", format: "pem" });
  return { keyId: keyIdForPublicKey(host, publicKeyPem), publicKeyPem };
}

function candidate() {
  const win = enrollment("windows"), mac = enrollment("macos");
  return {
    schema: 1,
    minimumScannerVersion: "2",
    minimumCatalogVersion: "5",
    revokedKeyIds: [],
    verifiedTrackingEvidence: ["a".repeat(64)],
    repositories: [{ repositoryIdentity: "https://github.com/orthic-labs/example", roots: ["src", "tools"], visibility: "public" }],
    provenance: [{ repositoryIdentity: "https://github.com/orthic-labs/example", paths: ["vendor/lib.rs"], contentDigest: "b".repeat(64), kind: "third-party", requestId: "REQ-123" }],
    hosts: { windows: win, macos: mac },
  };
}
const encode = (value) => Buffer.from(`${JSON.stringify(value, null, 2)}\n`);

const ace = (identitySID, mask, propagationFlags = 0, type = "Allow") => ({ identitySID, mask, propagationFlags, type });
const rootAcl = {
  ownerSID: "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464",
  ownerName: "NT SERVICE\\TrustedInstaller", attributes: "Directory", directory: true, nonempty: true, elevated: true,
  access: [ace("S-1-5-11", 4), ace("S-1-5-11", -536805376, 2), ace("S-1-5-18", 2032127), ace("S-1-5-32-544", 2032127), ace("S-1-5-32-545", 1179817), ace("S-1-15-3-65536-1888954469-739942743-1668119174-2468466756-4239452838-1296943325-355587736-700089176", 1048737)],
};
const programDataAcl = {
  ownerSID: "S-1-5-18", ownerName: "NT AUTHORITY\\SYSTEM", attributes: "Directory", directory: true, nonempty: true, elevated: true,
  access: [ace("S-1-3-0", 268435456, 2), ace("S-1-5-18", 2032127), ace("S-1-5-32-544", 2032127), ace("S-1-5-32-545", 1179817), ace("S-1-5-32-545", 278)],
};

test("numeric Windows ACL policy accepts native C root & ProgramData ancestor descriptors", () => {
  assert.deepEqual(evaluateWindowsAcl(rootAcl, { target: "C:\\", requireNonElevated: false }), { ok: true, problems: [] });
  assert.deepEqual(evaluateWindowsAcl(programDataAcl, { target: "C:\\ProgramData", requireNonElevated: false }), { ok: true, problems: [] });
  assert.equal(evaluateWindowsAcl(rootAcl, { target: "C:\\" }).ok, false);
});

test("numeric Windows ACL policy rejects leaf writes, dangerous ancestor masks & malformed masks", () => {
  const leaf = { ...programDataAcl, ownerSID: "S-1-5-18", access: [ace("S-1-5-32-545", 2), ace("S-1-5-18", 2032127)] };
  assert.equal(evaluateWindowsAcl(leaf, { target: "C:\\ProgramData\\RightKit\\tool.exe", requireNonElevated: false }).ok, false);
  for (const mask of [64, 0x40000, 0x80000, 0x40000000, 0x10000000]) {
    const dangerous = { ...programDataAcl, access: [ace("S-1-5-11", mask)] };
    assert.equal(evaluateWindowsAcl(dangerous, { target: "C:\\ProgramData", requireNonElevated: false }).ok, false);
  }
  assert.equal(evaluateWindowsAcl({ ...programDataAcl, nonempty: false }, { target: "C:\\ProgramData", requireNonElevated: false }).ok, false);
  assert.equal(evaluateWindowsAcl(programDataAcl, { target: "C:\\ProgramData\\RightKit", requireNonElevated: false }).ok, false);
  const malformed = { ...programDataAcl, access: [ace("S-1-5-11", "278")] };
  assert.match(evaluateWindowsAcl(malformed, { target: "C:\\ProgramData", requireNonElevated: false }).problems.join(" "), /malformed ACE/);
});

test("accepts exact pinned bytes only after complete trust and host-key validation", () => {
  const bytes = encode(candidate());
  const validated = validatePinnedCandidate(bytes, sha256(bytes));
  assert.deepEqual(validated.bytes, bytes);
  assert.equal(validated.trust.repositories[0].roots.join(","), "src,tools");
  assert.equal(validated.trust.provenance[0].contentDigest, "b".repeat(64));
});

test("rejects digest mismatch, malformed trust floors/roots, revoked keys, and reused host keys", () => {
  const value = candidate();
  const bytes = encode(value);
  assert.throws(() => validatePinnedCandidate(bytes, "0".repeat(64)), /pinned file digest/);
  const badFloorBytes = encode({ ...value, minimumScannerVersion: "latest" });
  assert.throws(() => validatePinnedCandidate(badFloorBytes, sha256(badFloorBytes)), /floors/);
  const badRoots = { ...value, repositories: [{ ...value.repositories[0], roots: ["../src"] }] };
  const badRootsBytes = encode(badRoots);
  assert.throws(() => validatePinnedCandidate(badRootsBytes, sha256(badRootsBytes)), /roots/);
  const revoked = { ...value, revokedKeyIds: [value.hosts.windows.keyId] };
  const revokedBytes = encode(revoked);
  assert.throws(() => validatePinnedCandidate(revokedBytes, sha256(revokedBytes)), /revoked/);
  const reused = { ...value, hosts: { windows: value.hosts.windows, macos: { keyId: keyIdForPublicKey("macos", value.hosts.windows.publicKeyPem), publicKeyPem: value.hosts.windows.publicKeyPem } } };
  const reusedBytes = encode(reused);
  assert.throws(() => validatePinnedCandidate(reusedBytes, sha256(reusedBytes)), /distinct/);
  const duplicateKeyBytes = Buffer.from('{\n  "schema": 1,\n  "schema": 1\n}\n');
  assert.throws(() => validatePinnedCandidate(duplicateKeyBytes, sha256(duplicateKeyBytes)), /canonical prepared formatting/);
});
