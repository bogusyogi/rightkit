import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { canonicalJson, sha256 } from "./ownership-approval.mjs";
import { __testOnly, readCommittedKeyAuthority } from "./ownership-key-authority.mjs";
import { createKeyEvent, eventHash, keyFingerprint } from "./ownership-key-log.mjs";

function git(root, ...args) {
  const result = spawnSync("git", ["-C", root, ...args], { encoding: "utf8", windowsHide: true });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}

function repo() {
  const root = mkdtempSync(path.join(tmpdir(), "rk-key-authority-"));
  git(root, "init", "-q");
  git(root, "config", "user.email", "test@example.invalid");
  git(root, "config", "user.name", "test");
  git(root, "remote", "add", "origin", "https://github.com/test/authority.git");
  const pair = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  const publicKeyPem = pair.publicKey.export({ type: "spki", format: "pem" });
  const record = { keyId: keyFingerprint(publicKeyPem), publicKeyPem, platform: "windows", deviceName: "root" };
  const event = createKeyEvent({ schema: 1, events: [] }, { operation: "root", subject: record, signerKeyId: record.keyId });
  event.signature = { keyId: record.keyId, host: "windows", value: sign("sha256", Buffer.from(canonicalJson(event.payload)), pair.privateKey).toString("base64") };
  const log = { schema: 1, events: [event] };
  mkdirSync(path.join(root, ".rightkit"), { recursive: true });
  const bytes = Buffer.from(`${JSON.stringify(log)}\n`);
  writeFileSync(path.join(root, ".rightkit", "ownership-keys.json"), bytes);
  git(root, "add", ".");
  git(root, "commit", "-qm", "authority genesis");
  return { root, record, event, log, bytes, pair, commit: git(root, "rev-parse", "HEAD"), tree: git(root, "rev-parse", "HEAD^{tree}") };
}

function trustFor(fixture) {
  return {
    rootFingerprint: fixture.record.keyId,
    keyCheckpoint: { sequence: 0, head: eventHash(fixture.event) },
    keyAuthority: { repositoryIdentity: "https://github.com/test/authority", checkoutPath: fixture.root },
    revokedKeyIds: [],
  };
}

test("authority reads exact committed log, binds fresh HEAD & merges current policy", () => {
  const fixture = repo();
  try {
    const trust = trustFor(fixture);
    const first = readCommittedKeyAuthority(trust);
    assert.equal(first.ok, true, first.problems.join("; "));
    assert.equal(first.digest, sha256(fixture.bytes));
    assert.deepEqual(first.authority, { repositoryIdentity: "https://github.com/test/authority", commit: fixture.commit, tree: fixture.tree });
    const laterPolicy = { ...trust, verifiedTrackingEvidence: ["b".repeat(64)] };
    const second = readCommittedKeyAuthority(laterPolicy);
    assert.deepEqual(second.resolvedTrust.verifiedTrackingEvidence, ["b".repeat(64)]);

    writeFileSync(path.join(fixture.root, ".rightkit", "ownership-keys.json"), '{"schema":1,"events":[]}\n');
    const dirty = readCommittedKeyAuthority(trust);
    assert.equal(dirty.ok, true, dirty.problems.join("; "));
    assert.equal(dirty.digest, first.digest);
    assert.deepEqual(dirty.authority, first.authority);
    assert.equal(readCommittedKeyAuthority({ ...trust, keyAuthority: { ...trust.keyAuthority, repositoryIdentity: "https://github.com/test/other" } }).ok, false);
  } finally { rmSync(fixture.root, { recursive: true, force: true }); }
});

test("authority reader rejects missing, malformed & oversized logs", () => {
  const fixture = repo();
  try {
    const missing = { ...trustFor(fixture), keyAuthority: { ...trustFor(fixture).keyAuthority, checkoutPath: path.join(fixture.root, "missing") } };
    assert.equal(readCommittedKeyAuthority(missing).ok, false);
    assert.equal(readCommittedKeyAuthority({ ...trustFor(fixture), keyAuthority: { repositoryIdentity: "https://github.com/test/authority", checkoutPath: fixture.root, extra: true } }).ok, false);
    writeFileSync(path.join(fixture.root, ".rightkit", "ownership-keys.json"), `${"x".repeat(__testOnly.MAX_LOG_BYTES + 1)}\n`);
    git(fixture.root, "add", ".rightkit/ownership-keys.json");
    git(fixture.root, "commit", "-qm", "oversized authority log");
    const oversized = readCommittedKeyAuthority(trustFor(fixture));
    assert.equal(oversized.ok, false);
    assert.match(oversized.problems.join(" "), /2 MiB/);
  } finally { rmSync(fixture.root, { recursive: true, force: true }); }
});
