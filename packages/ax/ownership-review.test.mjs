import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { buildOwnershipReviewPacket, validateOwnershipReviewReceipt } from "./ax/ownership-review.mjs";

function command(cwd, args) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}

function repoFixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-owner-review-"));
  command(root, ["init", "-q"]);
  command(root, ["config", "user.email", "test@example.invalid"]);
  command(root, ["config", "user.name", "AX test"]);
  const catalogPath = path.join(root, "rightkit/packages/release/rightkit-ownership.json");
  mkdirSync(path.dirname(catalogPath), { recursive: true });
  writeFileSync(catalogPath, JSON.stringify({ schema: 1, capabilities: [{ package: "rightkit-oauth", capability: "oauth-sign-in", summary: "OAuth flow", fingerprints: [{ id: "oauth.source.flow", type: "source", pattern: "device_flow" }] }] }));
  const apiPath = path.join(root, "rightkit/packages/oauth/README.md");
  mkdirSync(path.dirname(apiPath), { recursive: true });
  writeFileSync(apiPath, "# OAuth API\nUse shared device-flow helper.\n");
  const changed = path.join(root, "rightkit/packages/oauth/src/signin.mjs");
  mkdirSync(path.dirname(changed), { recursive: true });
  writeFileSync(changed, "fn login() { shared_oauth(); }\n");
  command(root, ["add", "."]);
  command(root, ["commit", "-qm", "base"]);
  const base = command(root, ["rev-parse", "HEAD"]);
  writeFileSync(changed, "fn login() { local_device_flow(); }\n");
  command(root, ["add", "."]);
  command(root, ["commit", "-qm", "head"]);
  const head = command(root, ["rev-parse", "HEAD"]);
  return { root, base, head };
}

test("prepares exact bounded range packet and dedupes against catalog/API content", () => {
  const fixture = repoFixture();
  const packet = buildOwnershipReviewPacket({ repoPath: fixture.root, base: fixture.base, head: fixture.head });
  assert.equal(packet.state, "prepared");
  assert.equal(packet.advisoryOnly, true);
  assert.equal(packet.authority.mayApproveExceptions, false);
  assert.equal(packet.authority.mayBlockChanges, false);
  assert.deepEqual(packet.range, { base: fixture.base, head: fixture.head });
  assert.deepEqual(packet.changedFiles, ["rightkit/packages/oauth/src/signin.mjs"]);
  assert.match(packet.source.diff.content, /local_device_flow/);
  assert.equal(packet.source.apiSources[0].path, "rightkit/packages/oauth/README.md");
  assert.match(packet.requestId, /^[a-f0-9]{64}$/);
  assert.equal(packet.providerState.status, "not_started");
  assert.equal(packet.limits.maxCostUsd, 1.5);
  assert.equal(readFileSync(path.join(fixture.root, "rightkit/packages/oauth/src/signin.mjs"), "utf8").includes("local_device_flow"), true);
});

test("rejects oversized ranges before a review packet can be emitted", () => {
  const fixture = repoFixture();
  assert.throws(() => buildOwnershipReviewPacket({ repoPath: fixture.root, base: fixture.base, head: fixture.head, limits: { maxDiffBytes: 8 } }), /split review/);
});

test("receipt validator requires supervised evidence and keeps findings advisory", () => {
  const fixture = repoFixture();
  const packet = buildOwnershipReviewPacket({ repoPath: fixture.root, base: fixture.base, head: fixture.head });
  const receipt = {
    requestId: packet.requestId,
    inputHash: packet.inputHash,
    usage: { costUsd: 0.12 },
    supervision: {
      identity: { verified: true, sessionId: "session-1", backend: "claude", tools: "read" },
      nativeReceiptDigest: "c".repeat(64),
      transcript: { verified: true, digest: "a".repeat(64) },
      controlWrite: { verified: true },
      reconnect: { verified: true },
      completion: { verified: true, status: "completed" },
    },
    review: { findings: [{ file: "rightkit/packages/oauth/src/signin.mjs", line: 1, catalogRefs: ["oauth.source.flow"], rationale: "Local flow appears to overlap shared capability.", confidence: 0.78 }] },
  };
  const result = validateOwnershipReviewReceipt(packet, receipt);
  assert.equal(result.valid, true);
  assert.equal(result.state, "completed");
  assert.equal(result.advisoryOnly, true);
  assert.equal(result.findings.length, 1);
  const incomplete = validateOwnershipReviewReceipt(packet, { ...receipt, supervision: { identity: { verified: true } } });
  assert.equal(incomplete.valid, false);
  assert.equal(incomplete.state, "receipt_incomplete");
});

test("receipt findings must ground changed paths and known catalog fingerprints", () => {
  const fixture = repoFixture();
  const packet = buildOwnershipReviewPacket({ repoPath: fixture.root, base: fixture.base, head: fixture.head });
  const receipt = {
    requestId: packet.requestId,
    inputHash: packet.inputHash,
    usage: { costUsd: 0.12 },
    supervision: { identity: { verified: true, sessionId: "s", backend: "pi", tools: "none" }, nativeReceiptDigest: "c".repeat(64), transcript: { verified: true, digest: "b".repeat(64) }, controlWrite: { verified: true }, reconnect: { verified: true }, completion: { verified: true, status: "completed" } },
    review: { findings: [{ file: "outside.rs", line: 3, catalogRefs: ["made.up"], rationale: "Concern.", confidence: 0.5 }] },
  };
  assert.equal(validateOwnershipReviewReceipt(packet, receipt).valid, false);
});

test("receipt is refused if the prepared packet was changed", () => {
  const fixture = repoFixture();
  const packet = buildOwnershipReviewPacket({ repoPath: fixture.root, base: fixture.base, head: fixture.head });
  const tampered = { ...packet, source: { ...packet.source, diff: { ...packet.source.diff, content: "altered" } } };
  const result = validateOwnershipReviewReceipt(tampered, {});
  assert.equal(result.valid, false);
  assert.match(result.errors.join(" "), /diff content digest mismatch/);
});
