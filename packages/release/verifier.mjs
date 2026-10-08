#!/usr/bin/env node
/**
 * Deterministic release verifier (Gate 6).
 *
 * Three capability grants exist so that no single stage can authorize the next
 * one for itself: BUILD_AUTHORIZED admits compilation, SIGNING_AUTHORIZED
 * admits production signing of exact package digests, and RELEASE_AUTHORIZED
 * admits public promotion of exact qualified bytes.
 *
 * The verifier is a pure function of receipts. It never reads a `decision`
 * field supplied by its caller: an input that already claims AUTHORIZED is
 * rejected outright, because the whole point of the gate is that neither a
 * human nor a model can hand-edit its way to a grant.
 */
import { createHash } from "node:crypto";
import { realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const GRANTS = Object.freeze(["BUILD_AUTHORIZED", "SIGNING_AUTHORIZED", "RELEASE_AUTHORIZED"]);

const SHA1_40 = /^[a-f0-9]{40}$/;
const SHA256_64 = /^[a-f0-9]{64}$/;

/** Receipts each grant requires, in the order the pipeline produces them. */
export const REQUIRED_RECEIPTS = Object.freeze({
  BUILD_AUTHORIZED: Object.freeze([
    "source_sha",
    "verifier_sha",
    "obligation_schema_digest",
    "ci_receipt",
    "static_source_closure_receipt",
    "platform_preflight_receipt",
  ]),
  SIGNING_AUTHORIZED: Object.freeze([
    "build_input_digest",
    "windows_candidate_sha256",
    "macos_candidate_sha256",
    "unsigned_transaction_receipt",
  ]),
  RELEASE_AUTHORIZED: Object.freeze([
    "signing_authorization_receipt",
    "windows_signed_sha256",
    "macos_notarized_sha256",
    "windows_install_receipt",
    "macos_install_receipt",
    "compatibility_matrix_receipt",
    "real_client_contract_receipt",
    "channel_stage_receipt",
  ]),
});

/** Fields that must look like a 64-character lowercase SHA-256. */
const DIGEST_FIELDS = new Set([
  "obligation_schema_digest",
  "build_input_digest",
  "windows_candidate_sha256",
  "macos_candidate_sha256",
  "windows_signed_sha256",
  "macos_notarized_sha256",
]);

/** A grant only carries the receipts it and its predecessors require. */
export function receiptsFor(grant) {
  const order = GRANTS.slice(0, GRANTS.indexOf(grant) + 1);
  return order.flatMap((each) => REQUIRED_RECEIPTS[each]);
}

function receiptIssues(receipts, names) {
  const issues = [];
  for (const name of names) {
    const value = receipts[name];
    if (value === undefined || value === null || value === "") {
      issues.push(`missing receipt: ${name}`);
      continue;
    }
    if (typeof value !== "string") {
      issues.push(`receipt must be a string: ${name}`);
      continue;
    }
    if (name === "source_sha" && !SHA1_40.test(value)) issues.push("source_sha must be a 40-character lowercase commit SHA");
    if (name === "verifier_sha" && !SHA1_40.test(value)) issues.push("verifier_sha must be a 40-character lowercase commit SHA");
    if (DIGEST_FIELDS.has(name) && !SHA256_64.test(value)) issues.push(`${name} must be a 64-character lowercase SHA-256`);
  }
  return issues;
}

/**
 * Computes one grant. Returns a decision object; never throws for an ordinary
 * refusal, because a refusal is a normal, expected outcome that must be
 * reportable rather than exceptional.
 */
export function computeGrant(grant, input = {}, { now = Date.now(), maxAgeSeconds = 3600 } = {}) {
  if (!GRANTS.includes(grant)) throw new Error(`unknown grant: ${grant}`);

  const issues = [];
  // A caller-supplied decision is an attempt to skip the gate.
  if (Object.hasOwn(input, "decision")) issues.push("input must not supply a decision; the verifier computes it");
  if (Object.hasOwn(input, "grant")) issues.push("input must not supply a grant; the verifier computes it");

  const receipts = input.receipts ?? {};
  if (typeof receipts !== "object" || receipts === null || Array.isArray(receipts)) {
    return decision(grant, false, ["receipts must be an object"], input);
  }
  issues.push(...receiptIssues(receipts, receiptsFor(grant)));

  // Short-lived: a stale authorization must not be replayable against bytes
  // that were rebuilt or re-signed after it was issued.
  const issuedAt = input.issuedAt;
  if (typeof issuedAt !== "string" || Number.isNaN(Date.parse(issuedAt))) {
    issues.push("issuedAt must be an ISO-8601 timestamp");
  } else {
    const ageSeconds = (now - Date.parse(issuedAt)) / 1000;
    if (ageSeconds < 0) issues.push("issuedAt is in the future");
    else if (ageSeconds > maxAgeSeconds) issues.push(`authorization input is stale: ${Math.round(ageSeconds)}s old, limit ${maxAgeSeconds}s`);
  }

  return decision(grant, issues.length === 0, issues, input);
}

function decision(grant, granted, issues, input) {
  const receipts = input.receipts ?? {};
  const bound = {};
  for (const name of receiptsFor(grant)) if (typeof receipts[name] === "string") bound[name] = receipts[name];
  const body = {
    schemaVersion: 1,
    kind: "rightkit-release-authorization",
    grant,
    decision: granted ? "AUTHORIZED" : "REFUSED",
    issues,
    boundReceipts: bound,
    issuedAt: typeof input.issuedAt === "string" ? input.issuedAt : null,
  };
  // The digest covers the bound receipts, so an authorization cannot be moved
  // onto a different source revision or different artifact bytes.
  body.authorizationDigest = createHash("sha256")
    .update(JSON.stringify({ grant: body.grant, decision: body.decision, boundReceipts: bound, issuedAt: body.issuedAt }))
    .digest("hex");
  return body;
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
const invokedDirectly = process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url));
if (invokedDirectly) {
  const grant = process.argv[2];
  let input = {};
  try { input = JSON.parse(process.argv[3] ?? "{}"); }
  catch (error) { process.stderr.write(`verifier: input is not JSON: ${error.message}\n`); process.exit(2); }
  const result = computeGrant(grant, input);
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  process.exitCode = result.decision === "AUTHORIZED" ? 0 : 1;
}
