import assert from "node:assert/strict";
import test from "node:test";
import { GRANTS, REQUIRED_RECEIPTS, computeGrant, receiptsFor } from "./verifier.mjs";

const SHA1 = "a".repeat(40);
const SHA256 = "b".repeat(64);
const NOW = Date.parse("2026-09-02T00:00:00.000Z");

function receipts(overrides = {}) {
  const base = {};
  for (const grant of GRANTS) {
    for (const name of REQUIRED_RECEIPTS[grant]) {
      if (name === "source_sha" || name === "verifier_sha") base[name] = SHA1;
      else if (name.endsWith("digest") || name.endsWith("sha256")) base[name] = SHA256;
      else base[name] = `receipt:${name}`;
    }
  }
  return { ...base, ...overrides };
}

function input(overrides = {}) {
  return { issuedAt: "2026-09-02T00:00:00.000Z", receipts: receipts(), ...overrides };
}

test("a complete receipt set authorizes each grant", () => {
  for (const grant of GRANTS) {
    const result = computeGrant(grant, input(), { now: NOW });
    assert.equal(result.decision, "AUTHORIZED", `${grant}: ${result.issues.join("; ")}`);
    assert.equal(result.grant, grant);
    assert.match(result.authorizationDigest, /^[a-f0-9]{64}$/);
  }
});

// The gate exists so nobody can write their way to a grant.
test("a caller-supplied decision is refused outright", () => {
  const result = computeGrant("RELEASE_AUTHORIZED", { ...input(), decision: "AUTHORIZED" }, { now: NOW });
  assert.equal(result.decision, "REFUSED");
  assert.ok(result.issues.some((issue) => issue.includes("must not supply a decision")));
});

test("each grant requires its predecessors' receipts too", () => {
  assert.deepEqual(receiptsFor("BUILD_AUTHORIZED"), [...REQUIRED_RECEIPTS.BUILD_AUTHORIZED]);
  assert.ok(receiptsFor("RELEASE_AUTHORIZED").includes("static_source_closure_receipt"));
  assert.ok(receiptsFor("RELEASE_AUTHORIZED").includes("windows_candidate_sha256"));
  // Signing cannot be authorized without the build-stage receipts.
  const missingBuild = computeGrant("SIGNING_AUTHORIZED", input({ receipts: { ...receipts(), ci_receipt: "" } }), { now: NOW });
  assert.equal(missingBuild.decision, "REFUSED");
  assert.ok(missingBuild.issues.some((issue) => issue.includes("ci_receipt")));
});

test("a missing or malformed digest refuses the grant", () => {
  const missing = computeGrant("RELEASE_AUTHORIZED", input({ receipts: { ...receipts(), windows_signed_sha256: "" } }), { now: NOW });
  assert.equal(missing.decision, "REFUSED");
  const malformed = computeGrant("RELEASE_AUTHORIZED", input({ receipts: { ...receipts(), macos_notarized_sha256: "not-a-digest" } }), { now: NOW });
  assert.equal(malformed.decision, "REFUSED");
  assert.ok(malformed.issues.some((issue) => issue.includes("64-character")));
});

test("authorization is short-lived and cannot be replayed", () => {
  const stale = computeGrant("RELEASE_AUTHORIZED", input({ issuedAt: "2026-09-01T00:00:00.000Z" }), { now: NOW });
  assert.equal(stale.decision, "REFUSED");
  assert.ok(stale.issues.some((issue) => issue.includes("stale")));
  const future = computeGrant("RELEASE_AUTHORIZED", input({ issuedAt: "2026-09-03T00:00:00.000Z" }), { now: NOW });
  assert.equal(future.decision, "REFUSED");
});

// Moving an authorization onto different bytes must change its digest.
test("the authorization digest binds the exact receipts", () => {
  const first = computeGrant("RELEASE_AUTHORIZED", input(), { now: NOW });
  const moved = computeGrant("RELEASE_AUTHORIZED", input({ receipts: { ...receipts(), windows_signed_sha256: "c".repeat(64) } }), { now: NOW });
  assert.equal(moved.decision, "AUTHORIZED");
  assert.notEqual(first.authorizationDigest, moved.authorizationDigest);
});
