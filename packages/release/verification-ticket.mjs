import { createHash, verify as verifySignature } from "node:crypto";
import { readFileSync, statSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

const DIGEST = /^sha256:[a-f0-9]{64}$/;
const GIT_ID = /^[a-f0-9]{40,64}$/;
const SIGNATURE = /^[A-Za-z0-9_-]+$/;
export const CLEAN_OVERLAY_SHA256 = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

export function canonicalJson(value) {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(",")}}`;
}

export function signedPayload(document) {
  const copy = structuredClone(document);
  if (!copy?.signing || typeof copy.signing !== "object") throw new Error("verification document signing is required");
  delete copy.signing.signature;
  return Buffer.from(canonicalJson(copy), "utf8");
}

export function documentDigest(document) {
  return `sha256:${createHash("sha256").update(signedPayload(document)).digest("hex")}`;
}

export function readVerificationDocument(file) {
  try { return JSON.parse(readFileSync(file, "utf8")); }
  catch (error) { throw new Error(`invalid verification document ${file}: ${error.message}`); }
}

export function verifyBuildTicket({ ticket, trustedKeys, candidateCommit, candidateTree, commandId, now = new Date() }) {
  validateTicket(ticket);
  verifyDocumentSignature(ticket, trustedKeys, "ticket");
  if (Date.parse(ticket.issued_at) > now.getTime()) throw new Error("verification ticket is not active yet");
  if (Date.parse(ticket.expires_at) <= now.getTime()) throw new Error("verification ticket has expired");
  if (ticket.candidate.commit !== candidateCommit || ticket.candidate.tree !== candidateTree) {
    throw new Error("verification ticket candidate does not exactly match current source");
  }
  if (commandId && ticket.execution.command_id !== commandId) throw new Error(`verification ticket command does not authorize ${commandId}`);
  return { ticket, ticketDigest: documentDigest(ticket) };
}

export function verifyPromotionEvidence({ ticket, hostReceipts, promotionReceipt, trustedKeys, hostTrust, promotionTrustedKeys = trustedKeys, candidateCommit, candidateTree, commandId, now = new Date() }) {
  const verified = verifyBuildTicket({ ticket, trustedKeys, candidateCommit, candidateTree, commandId, now });
  if (!Array.isArray(hostReceipts)) throw new Error("host receipts are required");
  const hostReceiptDigests = hostReceipts.map((receipt) => {
    validateHostReceipt(receipt);
    verifyHostTrust(receipt, hostTrust);
    verifyDocumentSignature(receipt, hostTrust?.keys ?? trustedKeys, "host receipt");
    verifyHostReceipt(receipt, ticket, verified.ticketDigest);
    return documentDigest(receipt);
  });
  const expectedHosts = new Map(ticket.hosts.identities.map((host) => [host.id, host]));
  const actualHosts = new Map(hostReceipts.map((receipt) => [receipt.host.id, receipt]));
  if (actualHosts.size !== hostReceipts.length || actualHosts.size !== expectedHosts.size || hostReceipts.length !== ticket.hosts.required_count) {
    throw new Error("host receipts do not exactly satisfy ticket host count");
  }
  for (const [id, host] of expectedHosts) {
    const receipt = actualHosts.get(id);
    if (!receipt || receipt.host.platform !== host.platform || receipt.host.attestation_sha256 !== host.attestation_sha256) {
      throw new Error(`host receipt does not match ticket host: ${id}`);
    }
  }
  if (!hostReceipts.some((receipt) => /^(mac|macos|darwin)$/i.test(receipt.host.platform))) throw new Error("missing required Mac host receipt");
  if (!hostReceipts.some((receipt) => /^(win|windows)$/i.test(receipt.host.platform))) throw new Error("missing required Windows host receipt");
  validatePromotionReceipt(promotionReceipt);
  if (promotionReceipt.signing.key_id !== "bounded-admin-signer") throw new Error("promotion receipt is not signed by bounded admin");
  verifyDocumentSignature(promotionReceipt, promotionTrustedKeys, "promotion receipt");
  if (promotionReceipt.candidate_commit !== candidateCommit || promotionReceipt.candidate_tree !== candidateTree || promotionReceipt.ticket_sha256 !== verified.ticketDigest) {
    throw new Error("promotion receipt does not exactly match ticket candidate");
  }
  if (promotionReceipt.policy.rubric_id !== ticket.policy.rubric_id || promotionReceipt.policy.rubric_sha256 !== ticket.policy.rubric_sha256) {
    throw new Error("promotion receipt policy does not match ticket policy");
  }
  if (promotionReceipt.sentinel_close.ticket_sha256 !== verified.ticketDigest || promotionReceipt.sentinel_close.candidate_commit !== candidateCommit || promotionReceipt.sentinel_close.candidate_tree !== candidateTree) {
    throw new Error("promotion receipt trusted close does not match ticket candidate");
  }
  if (!sameSet(promotionReceipt.host_receipts, hostReceiptDigests)) throw new Error("promotion receipt host receipt set does not match");
  return { ...verified, hostReceiptDigests, promotionReceipt };
}

export function trustedKeyFromPem(pem) {
  return { default: pem };
}

export function boundedAdminTrustPath(platform = process.platform) {
  return platform === "win32"
    ? "C:\\ProgramData\\RightStudio\\bounded-run\\admin-trust.json"
    : "/Library/Application Support/RightStudio/bounded-run/admin-trust.json";
}

export function loadBoundedAdminTrust() {
  const file = boundedAdminTrustPath();
  const root = path.dirname(file);
  assertBoundedAdminTrustSecurity({ paths: [root, file] });
  let store;
  try { store = JSON.parse(readFileSync(file, "utf8")); }
  catch { throw new Error(`bounded admin trust store is unavailable: ${file}`); }
  exactKeys(store, ["schema", "ticket_keys", "hosts"], "bounded admin trust store");
  if (store.schema !== "bounded-admin-trust.v1" || !store.ticket_keys || typeof store.ticket_keys !== "object" || Array.isArray(store.ticket_keys) || !store.hosts || typeof store.hosts !== "object" || Array.isArray(store.hosts)) throw new Error("bounded admin trust store is invalid");
  const keyPaths = [...Object.values(store.ticket_keys), ...Object.values(store.hosts).flatMap((host) => [host?.receipt_key_path, host?.manifest_key_path])];
  for (const keyPath of keyPaths) { nonempty(keyPath, "bounded admin verification key"); if (!path.isAbsolute(keyPath)) throw new Error("bounded admin verification key must be absolute"); }
  assertBoundedAdminTrustSecurity({ paths: [root, file, ...keyPaths] });
  const readKey = (keyPath, label) => { try { return readFileSync(keyPath, "utf8"); } catch { throw new Error(`${label} is unavailable`); } };
  const ticketKeys = Object.fromEntries(Object.entries(store.ticket_keys).map(([keyId, keyPath]) => {
    nonempty(keyId, "bounded admin ticket key id"); return [keyId, readKey(keyPath, "bounded admin ticket key")];
  }));
  if (!ticketKeys["bounded-admin-signer"]) throw new Error("bounded admin promotion key is unavailable");
  const hosts = Object.fromEntries(Object.entries(store.hosts).map(([hostId, host]) => {
    exactKeys(host, ["platform", "attestation_sha256", "receipt_key_id", "receipt_key_path", "manifest_key_id", "manifest_key_path"], "bounded admin host"); nonempty(hostId, "bounded admin host id"); nonempty(host.platform, "bounded admin host platform"); digest(host.attestation_sha256, "bounded admin host attestation"); nonempty(host.receipt_key_id, "bounded admin host receipt key id"); nonempty(host.manifest_key_id, "bounded admin host manifest key id"); return [hostId, host];
  }));
  const hostKeys = Object.fromEntries(Object.values(hosts).map((host) => [host.receipt_key_id, readKey(host.receipt_key_path, "bounded admin host receipt key")]));
  if (Object.keys(ticketKeys).length === 0 || Object.keys(hosts).length === 0) throw new Error("bounded admin trust store is empty");
  return { ticketKeys, hosts, hostKeys };
}

export function assertBoundedAdminTrustSecurity({ paths, platform = process.platform, stat = statSync, acl = readWindowsAcl }) {
  if (!Array.isArray(paths) || paths.length === 0) throw new Error("bounded admin trust paths are required");
  for (const file of paths) {
    if (platform === "win32") {
      if (windowsAclAllowsUntrustedWrite(acl(file))) throw new Error(`bounded admin trust path is writable by Users or Everyone: ${file}`);
      continue;
    }
    let metadata;
    try { metadata = stat(file); } catch { throw new Error(`bounded admin trust path is unavailable: ${file}`); }
    if (metadata.uid !== 0) throw new Error(`bounded admin trust path is not root-owned: ${file}`);
    if ((metadata.mode & 0o022) !== 0) throw new Error(`bounded admin trust path is group/world writable: ${file}`);
  }
}

function readWindowsAcl(file) {
  const result = spawnSync("icacls", [file], { encoding: "utf8", windowsHide: true });
  if (result.status !== 0) throw new Error(`unable to inspect bounded admin trust ACL: ${file}`);
  return result.stdout;
}

export function windowsAclAllowsUntrustedWrite(value) {
  if (typeof value !== "string" || !value.trim()) throw new Error("bounded admin trust ACL is unavailable");
  return value.split(/\r?\n/).some((line) => /(?:everyone|builtin\\users|\busers\b)/i.test(line) && /\((?:F|M|W|WD|AD)\)/i.test(line));
}

export function assertCleanOverlay({ status, ticket }) {
  if (status.length !== 0) throw new Error("release verification requires a clean working tree");
  if (ticket.candidate.overlay_sha256 !== CLEAN_OVERLAY_SHA256) throw new Error("release verification requires the clean overlay digest");
}

function validateTicket(ticket) {
  exactKeys(ticket, ["schema", "authority_id", "request_envelope_sha256", "request_sha256", "manifest_sha256", "experiment_id", "comparison_group_id", "nonce", "issued_at", "expires_at", "candidate", "runner", "bound_inputs", "execution", "required_outputs", "hosts", "policy", "registries", "signing"], "verification ticket");
  if (ticket.schema !== "verification-ticket.v1") throw new Error("unsupported verification ticket schema");
  for (const key of ["authority_id", "experiment_id", "comparison_group_id", "nonce"]) nonempty(ticket[key], `ticket.${key}`);
  if (!/^[A-Za-z0-9._:-]{16,256}$/.test(ticket.nonce)) throw new Error("ticket nonce is invalid");
  for (const key of ["request_envelope_sha256", "request_sha256", "manifest_sha256"]) digest(ticket[key], `ticket.${key}`);
  timestamp(ticket.issued_at, "ticket.issued_at"); timestamp(ticket.expires_at, "ticket.expires_at"); if (Date.parse(ticket.expires_at) <= Date.parse(ticket.issued_at)) throw new Error("ticket expiry must be after issue time");
  exactKeys(ticket.candidate, ["commit", "tree", "overlay_sha256"], "ticket.candidate"); gitId(ticket.candidate.commit, "ticket candidate commit"); gitId(ticket.candidate.tree, "ticket candidate tree"); digest(ticket.candidate.overlay_sha256, "ticket overlay");
  exactKeys(ticket.runner, ["id", "source_sha256", "binary_sha256"], "ticket.runner"); nonempty(ticket.runner.id, "ticket runner id"); digest(ticket.runner.source_sha256, "ticket runner source"); digest(ticket.runner.binary_sha256, "ticket runner binary");
  if (!Array.isArray(ticket.bound_inputs) || ticket.bound_inputs.length === 0) throw new Error("ticket bound inputs are required");
  for (const input of ticket.bound_inputs) { exactKeys(input, ["kind", "id", "sha256"], "ticket bound input"); if (!new Set(["input", "corpus", "model", "keyword", "resource", "runtime"]).has(input.kind)) throw new Error("invalid ticket bound input kind"); nonempty(input.id, "ticket bound input id"); digest(input.sha256, "ticket bound input"); }
  exactKeys(ticket.execution, ["command_id", "argv", "cwd", "environment", "cadence_seconds", "timeout_seconds", "output_limit_bytes"], "ticket execution"); nonempty(ticket.execution.command_id, "ticket command id"); nonempty(ticket.execution.cwd, "ticket cwd"); if (!Array.isArray(ticket.execution.argv) || ticket.execution.argv.length === 0 || ticket.execution.argv.some((entry) => typeof entry !== "string" || !entry)) throw new Error("ticket argv must be a nonempty array"); if (!Array.isArray(ticket.execution.environment)) throw new Error("ticket environment must be an array"); for (const variable of ticket.execution.environment) { exactKeys(variable, ["name", "value_sha256"], "ticket environment entry"); if (!/^[A-Z_][A-Z0-9_]*$/.test(variable.name)) throw new Error("invalid ticket environment name"); digest(variable.value_sha256, "ticket environment value"); } for (const key of ["cadence_seconds", "timeout_seconds", "output_limit_bytes"]) if (!Number.isInteger(ticket.execution[key]) || ticket.execution[key] < 1) throw new Error(`invalid ticket ${key}`);
  if (!Array.isArray(ticket.required_outputs)) throw new Error("ticket required outputs must be an array"); for (const output of ticket.required_outputs) { const keys = Object.keys(output ?? {}); if (!keys.every((key) => ["path", "sha256", "schema_sha256"].includes(key)) || !keys.includes("path") || (!keys.includes("sha256") && !keys.includes("schema_sha256"))) throw new Error("invalid ticket output"); nonempty(output.path, "ticket output path"); if (output.sha256) digest(output.sha256, "ticket output hash"); if (output.schema_sha256) digest(output.schema_sha256, "ticket output schema hash"); }
  exactKeys(ticket.hosts, ["required_count", "identities"], "ticket hosts"); if (!Number.isInteger(ticket.hosts.required_count) || ticket.hosts.required_count < 1 || !Array.isArray(ticket.hosts.identities) || ticket.hosts.identities.length !== ticket.hosts.required_count) throw new Error("ticket host count is invalid"); for (const host of ticket.hosts.identities) validateTicketHost(host);
  exactKeys(ticket.policy, ["rubric_id", "rubric_sha256"], "ticket policy"); nonempty(ticket.policy.rubric_id, "ticket rubric id"); digest(ticket.policy.rubric_sha256, "ticket rubric hash");
  exactKeys(ticket.registries, ["command_registry_sha256", "skill_registry_sha256"], "ticket registries"); digest(ticket.registries.command_registry_sha256, "ticket command registry"); digest(ticket.registries.skill_registry_sha256, "ticket skill registry"); validateSigning(ticket.signing);
}

function validateHostReceipt(receipt) {
  exactKeys(receipt, ["schema", "status", "host", "ticket_sha256", "preflight", "started_at", "ended_at", "exit_status", "outputs", "postflight", "signing"], "host receipt");
  if (receipt.schema !== "verification-host-receipt.v1" || receipt.status !== "PASS") throw new Error("host receipt is not a passing V1 receipt"); validateTicketHost(receipt.host); digest(receipt.ticket_sha256, "host receipt ticket"); identity(receipt.preflight, "host receipt preflight"); identity(receipt.postflight, "host receipt postflight"); timestamp(receipt.started_at, "host receipt start"); timestamp(receipt.ended_at, "host receipt end"); if (!Number.isInteger(receipt.exit_status) || receipt.exit_status !== 0) throw new Error("host receipt exit status must be zero"); if (!Array.isArray(receipt.outputs)) throw new Error("host receipt outputs must be an array"); for (const output of receipt.outputs) { exactKeys(output, ["path", "sha256"], "host receipt output"); nonempty(output.path, "host receipt output path"); digest(output.sha256, "host receipt output hash"); } validateSigning(receipt.signing);
}

function validatePromotionReceipt(receipt) {
  exactKeys(receipt, ["schema", "status", "candidate_commit", "candidate_tree", "ticket_sha256", "host_receipts", "policy", "evidence_quality", "sentinel_close", "signing"], "promotion receipt");
  if (receipt.schema !== "verification-promotion-receipt.v1" || receipt.status !== "PASS") throw new Error("promotion receipt is not a passing V1 receipt"); gitId(receipt.candidate_commit, "promotion candidate commit"); gitId(receipt.candidate_tree, "promotion candidate tree"); digest(receipt.ticket_sha256, "promotion ticket"); if (!Array.isArray(receipt.host_receipts) || receipt.host_receipts.length === 0) throw new Error("promotion host receipts are required"); receipt.host_receipts.forEach((value) => digest(value, "promotion host receipt")); exactKeys(receipt.policy, ["rubric_id", "rubric_sha256", "pass_count"], "promotion policy"); nonempty(receipt.policy.rubric_id, "promotion rubric id"); digest(receipt.policy.rubric_sha256, "promotion rubric hash"); if (receipt.policy.pass_count !== 114) throw new Error("promotion pass count must be 114"); exactKeys(receipt.evidence_quality, ["partial", "degraded", "stale", "waived", "model_only", "advisory"], "promotion evidence quality"); if (Object.values(receipt.evidence_quality).some((value) => value !== 0)) throw new Error("promotion evidence quality must be fully trusted"); exactKeys(receipt.sentinel_close, ["ticket_sha256", "ledger_sha256", "state_sha256", "candidate_commit", "candidate_tree", "authority"], "promotion trusted close"); digest(receipt.sentinel_close.ticket_sha256, "promotion close ticket"); digest(receipt.sentinel_close.ledger_sha256, "promotion close ledger"); digest(receipt.sentinel_close.state_sha256, "promotion close state"); gitId(receipt.sentinel_close.candidate_commit, "promotion close candidate"); gitId(receipt.sentinel_close.candidate_tree, "promotion close tree"); if (!new Set(["host", "hook", "operator"]).has(receipt.sentinel_close.authority)) throw new Error("promotion trusted close authority is invalid"); validateSigning(receipt.signing);
}

function verifyHostReceipt(receipt, ticket, ticketDigest) {
  if (receipt.ticket_sha256 !== ticketDigest) throw new Error("host receipt ticket digest does not match");
  const expected = { candidate_commit: ticket.candidate.commit, candidate_tree: ticket.candidate.tree, overlay_sha256: ticket.candidate.overlay_sha256, runner_sha256: digestOf(ticket.runner), bound_inputs_sha256: digestOf(ticket.bound_inputs), execution_sha256: digestOf(ticket.execution), policy_sha256: digestOf(ticket.policy), registries_sha256: digestOf(ticket.registries) };
  if (canonicalJson(receipt.preflight) !== canonicalJson(expected) || canonicalJson(receipt.postflight) !== canonicalJson(expected)) throw new Error("host receipt identity does not exactly match ticket");
}

function verifyHostTrust(receipt, trust) {
  if (!trust?.hosts || !trust?.keys) return;
  const expected = trust.hosts[receipt.host.id];
  if (!expected || receipt.host.platform !== expected.platform || receipt.host.attestation_sha256 !== expected.attestation_sha256 || receipt.signing.key_id !== expected.receipt_key_id) throw new Error("host receipt does not match bounded admin trust");
}

function validateTicketHost(host) { exactKeys(host, ["id", "platform", "attestation_sha256"], "ticket host"); nonempty(host.id, "ticket host id"); nonempty(host.platform, "ticket host platform"); digest(host.attestation_sha256, "ticket host attestation"); }
function identity(value, label) { exactKeys(value, ["candidate_commit", "candidate_tree", "overlay_sha256", "runner_sha256", "bound_inputs_sha256", "execution_sha256", "policy_sha256", "registries_sha256"], label); gitId(value.candidate_commit, `${label} commit`); gitId(value.candidate_tree, `${label} tree`); for (const key of ["overlay_sha256", "runner_sha256", "bound_inputs_sha256", "execution_sha256", "policy_sha256", "registries_sha256"]) digest(value[key], `${label} ${key}`); }
function validateSigning(value) { exactKeys(value, ["key_id", "algorithm", "signature"], "verification signing"); nonempty(value.key_id, "signing key id"); if (value.algorithm !== "ed25519" || !SIGNATURE.test(value.signature)) throw new Error("invalid verification signature"); }
function verifyDocumentSignature(document, trustedKeys, label) { const resolved = typeof trustedKeys === "function" ? trustedKeys(document.signing.key_id) : trustedKeys?.[document.signing.key_id] ?? trustedKeys?.default; const keys = Array.isArray(resolved) ? resolved : [resolved]; if (!keys[0]) throw new Error(`untrusted ${label} signing key: ${document.signing.key_id}`); if (!keys.some((key) => verifySignature(null, signedPayload(document), key, Buffer.from(document.signing.signature, "base64url")))) throw new Error(`invalid ${label} signature`); }
function digestOf(value) { return `sha256:${createHash("sha256").update(canonicalJson(value), "utf8").digest("hex")}`; }
function exactKeys(value, keys, label) { if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be an object`); const actual = Object.keys(value); if (actual.length !== keys.length || actual.some((key) => !keys.includes(key))) throw new Error(`${label} has unknown or missing fields`); }
function digest(value, label) { if (typeof value !== "string" || !DIGEST.test(value)) throw new Error(`${label} must be a sha256 digest`); }
function gitId(value, label) { if (typeof value !== "string" || !GIT_ID.test(value)) throw new Error(`${label} must be an exact Git id`); }
function nonempty(value, label) { if (typeof value !== "string" || !value) throw new Error(`${label} is required`); }
function timestamp(value, label) { if (typeof value !== "string" || !Number.isFinite(Date.parse(value))) throw new Error(`${label} must be a timestamp`); }
function sameSet(left, right) { return left.length === right.length && new Set(left).size === left.length && left.every((value) => right.includes(value)); }
