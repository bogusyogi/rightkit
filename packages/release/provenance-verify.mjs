// Verifies a RustRight broker job receipt's signed provenance envelope in pure JS (node:crypto only), exactly as
// rustright-protocol's attestation.rs `verify_receipt` does, so a hosted runner with no broker can authenticate the receipts a local
// gate run embedded in its qualification. Never trusts a value from the receipt that the caller can pin instead.
import { createHash, createPublicKey, verify } from "node:crypto";

export const PROVENANCE_SCHEMA = "rustright.provenance.v1";
export const PROVENANCE_DOMAIN = "rustright-provenance-v1";
export const QUALIFICATION_NONCE_DOMAIN = "rightkit-qualification-v1";
export const CARRIED_CALL_NONCE_DOMAIN = "rightkit-carried-call-v1";
/** The environment variable that carries a release-bound stage's nonce. It is no carrier variable: every carried call, a Cargo call or an
 * adapter job (a pnpm, node, swift or QA step), derives its own per-call nonce from it (carriedCallNonce) and the broker client never
 * sees the stage nonce itself; no step takes it as its RUSTRIGHT_ATTEST_NONCE (local-publish.mjs). */
export const ATTEST_STAGE_NONCE_ENV = "RIGHTKIT_ATTEST_STAGE_NONCE";

export class ProvenanceError extends Error {
  constructor(code, message) { super(message); this.name = "ProvenanceError"; this.code = code; }
}
const refuse = (code, message) => { throw new ProvenanceError(code, message); };

const sha256Hex = (data) => createHash("sha256").update(data).digest("hex");
const isRecord = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

function writeString(text, out) {
  let s = '"';
  for (const ch of text) {
    const code = ch.codePointAt(0);
    if (ch === '"') s += '\\"';
    else if (ch === "\\") s += "\\\\";
    else if (code < 0x20) s += `\\u${code.toString(16).padStart(4, "0")}`;
    else s += ch;
  }
  out.push(`${s}"`);
}

function writeCanonical(value, out) {
  if (value === null) out.push("null");
  else if (typeof value === "boolean") out.push(value ? "true" : "false");
  else if (typeof value === "number") {
    // Integers only: serde_json prints floats with its own algorithm, and an integer beyond 2^53 was already corrupted by JSON.parse.
    // Either would make a signature fail for the wrong reason, so refuse loudly instead.
    if (!Number.isSafeInteger(value)) refuse("canonical", "canonical JSON takes only safe integers");
    out.push(String(value));
  } else if (typeof value === "string") writeString(value, out);
  else if (Array.isArray(value)) {
    out.push("[");
    value.forEach((item, index) => { if (index > 0) out.push(","); writeCanonical(item, out); });
    out.push("]");
  } else if (isRecord(value)) {
    // Rust sorts by the UTF-8 bytes of the key; JavaScript's default sort is by UTF-16 code unit and differs above U+FFFF.
    const keys = Object.keys(value).sort((a, b) => Buffer.compare(Buffer.from(a, "utf8"), Buffer.from(b, "utf8")));
    out.push("{");
    keys.forEach((key, index) => { if (index > 0) out.push(","); writeString(key, out); out.push(":"); writeCanonical(value[key], out); });
    out.push("}");
  } else refuse("canonical", `canonical JSON cannot carry a ${typeof value}`);
}

/** Canonical JSON: keys sorted by byte order, no whitespace, strings escape only `"`, `\` and control characters < 0x20 (\u00xx lower-case). */
export function canonicalJson(value) {
  const out = [];
  writeCanonical(value, out);
  return out.join("");
}

/** The exact bytes the broker signs for a payload. */
export function signedBytes(payload) {
  return Buffer.from(`${PROVENANCE_DOMAIN}\n${canonicalJson(payload)}`, "utf8");
}

const HEX = /^(?:[0-9a-fA-F]{2})+$/;
const keyFromSec1 = (publicKeyHex) => {
  if (typeof publicKeyHex !== "string" || !HEX.test(publicKeyHex)) refuse("pin", "pinned public key is not hex");
  const bytes = Buffer.from(publicKeyHex, "hex");
  if (bytes.length !== 65 || bytes[0] !== 0x04) refuse("pin", "pinned public key is not an uncompressed SEC1 P-256 point");
  try {
    return createPublicKey({ key: { kty: "EC", crv: "P-256", x: bytes.subarray(1, 33).toString("base64url"), y: bytes.subarray(33, 65).toString("base64url") }, format: "jwk" });
  } catch { return refuse("pin", "pinned public key is not a P-256 point"); }
};

/**
 * Verifies receipt.provenance against pins the caller holds (never from the receipt) and what this invocation expects. Every check
 * fails closed (throws ProvenanceError with a stable `code`) and returns the signed payload on success.
 *   pins:     { publicKey (SEC1 uncompressed hex), keyId, host, brokerDigest }
 *   expected: { jobId, nonce, requestDigest? }   requestDigest is checked only when the caller can recompute it.
 */
export function verifyProvenance(receipt, pins, expected) {
  const envelope = receipt?.provenance;
  if (!isRecord(envelope)) refuse("legacy", "the receipt has no provenance envelope");
  if (envelope.schema !== PROVENANCE_SCHEMA) refuse("schema", "unknown provenance schema");
  const payload = envelope.payload;
  if (!isRecord(payload)) refuse("malformed", "payload is missing");
  const signature = envelope.signature;
  if (!isRecord(signature)) refuse("malformed", "signature is missing");
  if (signature.alg !== "ES256") refuse("alg", "signature algorithm must be ES256");
  if (!isRecord(pins) || typeof pins.publicKey !== "string" || !HEX.test(pins.publicKey)) refuse("pin", "pinned public key is not hex");
  if (sha256Hex(Buffer.from(pins.publicKey, "hex")) !== pins.keyId) refuse("pin", "pinned key id does not match the pinned public key");
  if (signature.keyId !== pins.keyId || payload.keyId !== pins.keyId) refuse("key", "the receipt was not signed by the pinned key");
  const key = keyFromSec1(pins.publicKey);
  if (typeof signature.value !== "string" || !/^[0-9a-fA-F]+$/.test(signature.value)) refuse("signature", "signature is not hex");
  if (signature.value.length !== 128) refuse("signature", "signature is not 64 bytes r||s");
  let good = false;
  try { good = verify("sha256", signedBytes(payload), { key, dsaEncoding: "ieee-p1363" }, Buffer.from(signature.value, "hex")); } catch (error) { if (error instanceof ProvenanceError) throw error; }
  if (!good) refuse("signature", "signature does not verify");
  if (payload.host !== pins.host) refuse("host", "signed for another host");
  if (payload.broker?.brokerDigest !== pins.brokerDigest) refuse("runtime", "signed by another broker runtime");
  if (typeof expected?.jobId !== "string" || payload.jobId !== expected.jobId || receipt.id !== expected.jobId) refuse("job", "the receipt is for another job");
  if (typeof expected.nonce !== "string" || payload.nonce !== expected.nonce) refuse("nonce", "the nonce is not the one this invocation sent");
  if (expected.requestDigest !== undefined && payload.requestDigest !== expected.requestDigest) refuse("request", "the request digest is not the one this invocation computed");
  return payload;
}

// ---- GB-10 / GB-11 / GB-50: the rest of rustright-protocol attestation.rs `verify_receipt` and `verify_receipt_strict`, case for case ----
// `verifyProvenance` above is the signature, pins, job, nonce and request-digest half. The functions below are the coverage half: a field the receipt carries
// and the signature does not cover (or the reverse) is refused like a changed one. `verifyReceiptLenient` = both halves (`verify_receipt`);
// `verifyReceiptStrict` adds the signed result evidence and the BR19 shape (`verify_receipt_strict`), which a gate applies to every compile receipt. The vector
// docs/contracts/receipt-vectors.v1.json (test-support/vectors/, pinned) is judged here case for case, lenient and strict.
export const STAGE_EVIDENCE_FIELDS = Object.freeze(["freshness", "attribution", "toolchain"]);
export const RESULT_EVIDENCE_FIELDS = Object.freeze(["exit", "signal", "timingsMs", "diagnosticsPath"]);
export const RESULT_TIMING_FIELDS = Object.freeze(["admission", "queue", "bind", "cargo", "afterExitBind", "diagnostics"]);
export const BR19_EVIDENCE_FIELDS = Object.freeze(["profile", "stage", "resources", "environment"]);
export const BR19_RESOURCE_FIELDS = Object.freeze(["grantedJobs", "memoryReservation", "priority"]);
export const RETIRED_RESULT_FIELDS = Object.freeze(["verdict", "refreshed"]);
const SNAPSHOT_EVIDENCE_NAMES = Object.freeze(["requestId", "brokerVersion", "executor", "commit", "snapshot", "declaredInputDigests", "ownership", "outputDir"]);

const equalValue = (a, b) => (a === undefined || b === undefined ? a === b : canonicalJson(a) === canonicalJson(b));
const isU64 = (v) => Number.isSafeInteger(v) && v >= 0;

function covered(receipt, payload, key, fields, what) {
  const evidence = payload[key];
  if (evidence !== undefined) {
    for (const name of fields) if (!equalValue(receipt[name], isRecord(evidence) ? evidence[name] : undefined)) refuse("evidence", `receipt ${name} differs from signed ${what}`);
  } else {
    const name = fields.find((f) => receipt[f] !== undefined);
    if (name !== undefined) refuse("evidence", `receipt ${name} is not covered by signed ${what}`);
  }
}

/** The coverage half of `verify_receipt`: signed snapshot evidence and nested children equal the receipt's copies; stage, result and BR19 evidence are never one-sided; the retired verdict fields are refused. */
export function verifyReceiptCoverage(receipt, payload) {
  const snapshotEvidence = payload.snapshotEvidence;
  if (snapshotEvidence !== undefined) {
    for (const name of SNAPSHOT_EVIDENCE_NAMES) if (!isRecord(snapshotEvidence) || snapshotEvidence[name] === undefined || !equalValue(receipt[name], snapshotEvidence[name])) refuse("evidence", `receipt ${name} differs from signed snapshot evidence`);
    if (!equalValue(receipt.nestedChildren, snapshotEvidence.nestedChildren)) refuse("evidence", "receipt nestedChildren differs from signed snapshot evidence");
  }
  covered(receipt, payload, "stageEvidence", STAGE_EVIDENCE_FIELDS, "stage evidence");
  covered(receipt, payload, "resultEvidence", RESULT_EVIDENCE_FIELDS, "result evidence");
  covered(receipt, payload, "br19Evidence", BR19_EVIDENCE_FIELDS, "BR19 evidence");
  for (const name of RETIRED_RESULT_FIELDS) {
    if (receipt[name] !== undefined || (isRecord(payload.resultEvidence) && payload.resultEvidence[name] !== undefined)) refuse("evidence", `receipt ${name} is a retired verdict-reuse field`);
  }
}

/** `verify_receipt`: the signature half and the coverage half. Returns the signed payload. */
export function verifyReceiptLenient(receipt, pins, expected) {
  const payload = verifyProvenance(receipt, pins, expected);
  verifyReceiptCoverage(receipt, payload);
  return payload;
}

/** GB-10 `verify_result_evidence`: the signed payload carries `resultEvidence` with exactly exit, signal, timingsMs, diagnosticsPath (null is a value; a missing or extra key is not), timingsMs exactly the six phases, every receipt copy equal. */
export function verifyResultEvidence(receipt, payload) {
  const evidence = payload.resultEvidence;
  if (!isRecord(evidence)) refuse("evidence", "the signed payload carries no result evidence");
  const extra = Object.keys(evidence).find((name) => !RESULT_EVIDENCE_FIELDS.includes(name));
  if (extra !== undefined) refuse("evidence", `signed result evidence carries an unknown field ${extra}`);
  for (const name of RESULT_EVIDENCE_FIELDS) {
    if (evidence[name] === undefined) refuse("evidence", `signed result evidence lacks ${name}`);
    if (!equalValue(receipt[name], evidence[name])) refuse("evidence", `receipt ${name} differs from signed result evidence`);
  }
  const timings = evidence.timingsMs;
  if (!isRecord(timings)) refuse("evidence", "signed result evidence timingsMs is not an object");
  if (Object.keys(timings).length !== RESULT_TIMING_FIELDS.length || RESULT_TIMING_FIELDS.some((name) => !(timings[name] === null || isU64(timings[name])))) refuse("evidence", "signed result evidence timingsMs must hold exactly the contract phases as numbers or null");
}

const isUnknownValue = (v) => isRecord(v) && Object.keys(v).length === 2 && v.state === "UNKNOWN" && typeof v.reason === "string" && v.reason !== "";
const nonEmptyString = (v) => typeof v === "string" && v !== "";

/** GB-11 `verify_br19_evidence`: signed `br19Evidence` has exactly profile, stage, resources, environment, equal to the receipt's copies, each well formed (a value or `{state:"UNKNOWN",reason}`; never absent, never an invented zero). */
export function verifyBr19Evidence(receipt, payload) {
  const evidence = payload.br19Evidence;
  if (!isRecord(evidence)) refuse("evidence", "the signed payload carries no BR19 evidence");
  const extra = Object.keys(evidence).find((name) => !BR19_EVIDENCE_FIELDS.includes(name));
  if (extra !== undefined) refuse("evidence", `signed BR19 evidence carries an unknown field ${extra}`);
  for (const name of BR19_EVIDENCE_FIELDS) {
    if (evidence[name] === undefined) refuse("evidence", `signed BR19 evidence lacks ${name}`);
    if (!equalValue(receipt[name], evidence[name])) refuse("evidence", `receipt ${name} differs from signed BR19 evidence`);
  }
  const bad = (what) => refuse("evidence", `signed BR19 evidence ${what}`);
  if (!nonEmptyString(evidence.profile)) bad("profile is not a non-empty string");
  if (!isRecord(evidence.stage) || !nonEmptyString(evidence.stage.name) || !nonEmptyString(evidence.stage.source)) bad("stage needs a name and a source");
  const resources = isRecord(evidence.resources) ? evidence.resources : {};
  for (const name of BR19_RESOURCE_FIELDS) {
    const value = resources[name];
    if (value === undefined) bad(`resources lacks ${name}`);
    const typed = name === "priority" ? nonEmptyString(value) : isU64(value);
    if (!typed && !isUnknownValue(value)) bad(`resources.${name} is neither a value nor UNKNOWN with a reason`);
  }
  const environment = isRecord(evidence.environment) ? evidence.environment : {};
  const names = (key) => Array.isArray(environment[key]) && environment[key].every(nonEmptyString);
  if (!(typeof environment.digest === "string" && /^[0-9a-f]{64}$/.test(environment.digest) && names("forwarded") && names("dropped"))) bad("environment needs a 64-hex digest and forwarded and dropped name lists");
}

/** `verify_receipt_strict`: `verifyReceiptLenient`, then the signed result evidence and the BR19 shape. A gate applies it to every compile receipt, never the lenient form alone. */
export function verifyReceiptStrict(receipt, pins, expected) {
  const payload = verifyReceiptLenient(receipt, pins, expected);
  verifyResultEvidence(receipt, payload);
  verifyBr19Evidence(receipt, payload);
  return payload;
}

/** The stage nonce a unit gate run sets (RIGHTKIT_ATTEST_STAGE_NONCE) for the broker jobs of one stage; the hosted admission recomputes it from
 * what it knows of the dispatched commit, so a receipt of another commit, tree, unit or stage cannot match. 64 lowercase hex. */
export function qualificationNonce({ commit, tree, unit, stage }) {
  return sha256Hex(`${QUALIFICATION_NONCE_DOMAIN}\n${commit}\n${tree}\n${unit}\n${stage}`);
}

/** The nonce of one carried cargo call of a stage (rightkit#1301 B8): derived from the stage nonce, the directory of the repository the call
 * runs in (`.` or `crates/x`, posix, relative) and the argv it sends (the locked words, no `cargo`), so a call's receipt binds the stage,
 * the directory and the command, and a stage nonce is never itself a carrier nonce. 64 lowercase hex. The gate derives it per call
 * (snapshot-broker.mjs carryBrokerCargo) and the hosted admission recomputes it from each stage row. */
export function carriedCallNonce({ stageNonce, cwd, argv }) {
  return sha256Hex(`${CARRIED_CALL_NONCE_DOMAIN}\n${stageNonce}\n${cwd}\n${canonicalJson([...argv])}`);
}

/** Broker request ids the client prints (`rustright: request <id>`) in one command's output. */
export function brokerRequestIds(text) {
  return [...String(text ?? "").matchAll(/^rustright: request ([0-9a-f-]{36})/gm)].map((m) => m[1]);
}
