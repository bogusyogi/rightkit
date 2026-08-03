import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { assertBoundedAdminTrustSecurity, boundedAdminTrustPath, canonicalJson, documentDigest, verifyBuildTicket, verifyPromotionEvidence, windowsAclAllowsUntrustedWrite } from "./verification-ticket.mjs";

const { privateKey, publicKey } = generateKeyPairSync("ed25519");
const commit = "a".repeat(40);
const tree = "b".repeat(40);
const digest = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const hash = (value) => digest(canonicalJson(value));
const signDocument = (document) => {
  const copy = structuredClone(document);
  const payload = structuredClone(copy);
  delete payload.signing.signature;
  copy.signing.signature = sign(null, Buffer.from(canonicalJson(payload), "utf8"), privateKey).toString("base64url");
  return copy;
};

function ticket(overrides = {}) {
  const value = {
    schema: "verification-ticket.v1", authority_id: "r7", request_envelope_sha256: digest("envelope"), request_sha256: digest("request"), manifest_sha256: digest("manifest"), experiment_id: "fixture", comparison_group_id: "fixture-group", nonce: "fixture-nonce-0001", issued_at: "2026-08-01T00:00:00.000Z", expires_at: "2026-09-01T00:00:00.000Z",
    candidate: { commit, tree, overlay_sha256: digest("overlay") }, runner: { id: "right-release", source_sha256: digest("source"), binary_sha256: digest("binary") }, bound_inputs: [{ kind: "runtime", id: "node", sha256: digest("node") }], execution: { command_id: "right-release.build", argv: ["right-release", "build"], cwd: ".", environment: [], cadence_seconds: 1, timeout_seconds: 60, output_limit_bytes: 1000 }, required_outputs: [], hosts: { required_count: 2, identities: [{ id: "mac-host", platform: "mac", attestation_sha256: digest("mac") }, { id: "windows-host", platform: "windows", attestation_sha256: digest("windows") }] }, policy: { rubric_id: "r7", rubric_sha256: digest("rubric") }, registries: { command_registry_sha256: digest("commands"), skill_registry_sha256: digest("skills") }, signing: { key_id: "fixture", algorithm: "ed25519", signature: "placeholder" },
    ...overrides,
  };
  return signDocument(value);
}

function receipt(ticketValue, host) {
  const identity = { candidate_commit: ticketValue.candidate.commit, candidate_tree: ticketValue.candidate.tree, overlay_sha256: ticketValue.candidate.overlay_sha256, runner_sha256: hash(ticketValue.runner), bound_inputs_sha256: hash(ticketValue.bound_inputs), execution_sha256: hash(ticketValue.execution), policy_sha256: hash(ticketValue.policy), registries_sha256: hash(ticketValue.registries) };
  return signDocument({ schema: "verification-host-receipt.v1", status: "PASS", host, ticket_sha256: documentDigest(ticketValue), preflight: identity, started_at: "2026-08-02T00:00:00.000Z", ended_at: "2026-08-02T00:01:00.000Z", exit_status: 0, outputs: [], postflight: identity, signing: { key_id: "fixture", algorithm: "ed25519", signature: "placeholder" } });
}

function evidence() {
  const ticketValue = ticket();
  const receipts = ticketValue.hosts.identities.map((host) => receipt(ticketValue, host));
  const promotion = signDocument({ schema: "verification-promotion-receipt.v1", status: "PASS", candidate_commit: commit, candidate_tree: tree, ticket_sha256: documentDigest(ticketValue), host_receipts: receipts.map(documentDigest), policy: { ...ticketValue.policy, pass_count: 114 }, evidence_quality: { partial: 0, degraded: 0, stale: 0, waived: 0, model_only: 0, advisory: 0 }, sentinel_close: { ticket_sha256: documentDigest(ticketValue), ledger_sha256: digest("ledger"), state_sha256: digest("state"), candidate_commit: commit, candidate_tree: tree, authority: "host" }, signing: { key_id: "bounded-admin-signer", algorithm: "ed25519", signature: "placeholder" } });
  return { ticket: ticketValue, hostReceipts: receipts, promotionReceipt: promotion, trustedKeys: { fixture: publicKey, "bounded-admin-signer": publicKey }, candidateCommit: commit, candidateTree: tree, now: new Date("2026-08-03T00:00:00.000Z") };
}

test("valid signed ticket admits only its exact candidate", () => {
  const fixture = evidence();
  assert.equal(verifyBuildTicket(fixture).ticket.candidate.commit, commit);
  assert.throws(() => verifyBuildTicket({ ...fixture, candidateCommit: "c".repeat(40) }), /exactly match/);
});

test("ticket mutations, expiry, and receipt mismatches fail closed", () => {
  const fixture = evidence();
  const changed = structuredClone(fixture.ticket); changed.execution.argv.push("--mutated");
  assert.throws(() => verifyBuildTicket({ ...fixture, ticket: changed }), /signature/);
  assert.throws(() => verifyBuildTicket({ ...fixture, now: new Date("2026-10-01T00:00:00.000Z") }), /expired/);
  assert.throws(() => verifyPromotionEvidence({ ...fixture, hostReceipts: fixture.hostReceipts.slice(0, 1) }), /host receipts/);
});

test("promotion requires exact signed Mac and Windows receipts plus trusted close", () => {
  const fixture = evidence();
  assert.equal(verifyPromotionEvidence(fixture).hostReceiptDigests.length, 2);
  const changed = structuredClone(fixture.promotionReceipt); changed.sentinel_close.authority = "model";
  assert.throws(() => verifyPromotionEvidence({ ...fixture, promotionReceipt: changed }), /trusted close authority/);
  const wrongTree = structuredClone(fixture.promotionReceipt); wrongTree.sentinel_close.candidate_tree = "c".repeat(40);
  assert.throws(() => verifyPromotionEvidence({ ...fixture, promotionReceipt: signDocument(wrongTree) }), /trusted close does not match/);
});

test("stale, degraded, caller-key, and unequal-receipt promotion evidence fails closed", () => {
  const fixture = evidence();
  for (const field of ["partial", "degraded", "waived", "model_only"]) {
    const degraded = structuredClone(fixture.promotionReceipt); degraded.evidence_quality[field] = 1;
    assert.throws(() => verifyPromotionEvidence({ ...fixture, promotionReceipt: signDocument(degraded) }), /evidence quality/);
  }
  const callerKey = structuredClone(fixture.promotionReceipt); callerKey.signing.key_id = "fixture";
  assert.throws(() => verifyPromotionEvidence({ ...fixture, promotionReceipt: signDocument(callerKey) }), /bounded admin/);
  const unequal = structuredClone(fixture.hostReceipts[1]); unequal.ticket_sha256 = digest("other-ticket");
  assert.throws(() => verifyPromotionEvidence({ ...fixture, hostReceipts: [fixture.hostReceipts[0], signDocument(unequal)] }), /ticket digest/);
});

test("host receipts bind platform, attestation, receipt key, and manifest key trust", () => {
  const fixture = evidence();
  const host = (platform, attestation) => ({ platform, attestation_sha256: attestation, receipt_key_id: "fixture", receipt_key_path: "/trusted/receipt.pem", manifest_key_id: "fixture", manifest_key_path: "/trusted/manifest.pem" });
  const hostTrust = { keys: { fixture: publicKey }, hosts: { "mac-host": host("mac", digest("mac")), "windows-host": host("windows", digest("windows")) } };
  assert.doesNotThrow(() => verifyPromotionEvidence({ ...fixture, hostTrust }));
  hostTrust.hosts["windows-host"].platform = "mac";
  assert.throws(() => verifyPromotionEvidence({ ...fixture, hostTrust }), /bounded admin trust/);
});

test("upload gates network work on shared promotion evidence", () => {
  const source = readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), "upload-release.mjs"), "utf8");
  assert.match(source, /verifyPromotionEvidence/);
  assert.match(source, /--verification-ticket, --host-receipt, and --promotion-receipt are required/);
  assert.match(source, /loadBoundedAdminTrust/);
  assert.doesNotMatch(source, /--verification-public-key/);
  assert.doesNotMatch(source, /RightKit[\\/].*verification-trust/);
  assert.match(source, /sealed\.manifest\.commit !== candidateCommit/);
});

test("only bounded admin trust locations are accepted", () => {
  assert.equal(boundedAdminTrustPath("darwin"), "/Library/Application Support/RightStudio/bounded-run/admin-trust.json");
  const previous = process.env.ProgramData;
  process.env.ProgramData = "C:/caller-controlled";
  assert.equal(boundedAdminTrustPath("win32"), "C:\\ProgramData\\RightStudio\\bounded-run\\admin-trust.json");
  if (previous === undefined) delete process.env.ProgramData; else process.env.ProgramData = previous;
});

test("admin trust bootstrap rejects mutable macOS ownership and Windows user ACLs", () => {
  const secureStat = () => ({ uid: 0, mode: 0o100644 });
  assert.doesNotThrow(() => assertBoundedAdminTrustSecurity({ paths: ["/Library/Application Support/RightStudio/bounded-run", "/Library/Application Support/RightStudio/bounded-run/admin-trust.json", "/Library/Application Support/RightStudio/bounded-run/ticket.pem"], platform: "darwin", stat: secureStat }));
  assert.throws(() => assertBoundedAdminTrustSecurity({ paths: ["/trust"], platform: "darwin", stat: () => ({ uid: 501, mode: 0o100644 }) }), /not root-owned/);
  assert.throws(() => assertBoundedAdminTrustSecurity({ paths: ["/trust"], platform: "darwin", stat: () => ({ uid: 0, mode: 0o100666 }) }), /group\/world writable/);
  assert.equal(windowsAclAllowsUntrustedWrite("C:\\trust BUILTIN\\Administrators:(F)"), false);
  assert.equal(windowsAclAllowsUntrustedWrite("C:\\trust BUILTIN\\Users:(OI)(CI)(M)"), true);
  assert.throws(() => assertBoundedAdminTrustSecurity({ paths: ["C:\\ProgramData\\RightStudio\\bounded-run\\admin-trust.json"], platform: "win32", acl: () => "C:\\trust Everyone:(W)" }), /writable by Users or Everyone/);
});
