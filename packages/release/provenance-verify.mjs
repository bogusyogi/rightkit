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

// ---- RA-03: the cause evidence the broker signs inside `freshness` (rustright-broker cargo_evidence.rs, schema rustright.cause-evidence.v1) ----
// A census that counts rebuilt units proves nothing about WHY they were rebuilt. The broker therefore signs `freshness.causeEvidence`: a state (complete | incomplete |
// unavailable), a verdict (explained | unexplained | unknown), the basis the verdict rests on, the typed gaps and the counts. `verifyCauseEvidence` reads it and refuses what
// is missing, of a schema this gate does not read, or not coherent with itself and with the census fields beside it; `requireExplainedCauses` then refuses every verdict but
// `explained`. An `unknown` verdict is a typed refusal, never read as zero unexplained units, and a broker that signs no causeEvidence (one that predates the schema) is
// refused as well: nothing here relaxes for an older installed broker. One implementation serves every caller, so no second copy of the rule can drift.
export const CAUSE_EVIDENCE_SCHEMA = "rustright.cause-evidence.v1";
/** The refusal codes of a verdict that is not `explained` (the other cause-evidence refusals are `evidence`, like every malformed signed field). */
export const CAUSE_UNKNOWN = "cause-unknown";
export const CAUSE_UNEXPLAINED = "cause-unexplained";
/** cargo_evidence.rs EXPLAINED_CAUSES: the only causes that explain a rebuilt unit; any other (including `unexplained`, `fingerprint-missing` and one this list does not know) does not. */
export const EXPLAINED_CAUSES = Object.freeze([
  "first-build", "toolchain-changed", "source-or-dependency-changed", "env-rustflags-profile-features-or-target-changed",
  "generation-change", "authorized-eviction", "new-unit", "new-namespace",
]);
const CAUSE_STATES = Object.freeze(["complete", "incomplete", "unavailable"]);
const CAUSE_VERDICTS = Object.freeze(["explained", "unexplained", "unknown"]);
const CAUSE_BASES = Object.freeze(["reasons", "census-nothing-rebuilt", "nothing-compiled", "none"]);
const CAUSE_COUNT_FIELDS = Object.freeze(["rows", "rowsDropped", "units", "unexplained", "uncovered"]);
const shownValue = (v) => { let text; try { text = JSON.stringify(v); } catch { text = undefined; } return text === undefined ? "absent" : text.slice(0, 80); };
/** cargo_evidence.rs `counted`: a count in a freshness object is a whole number, or a flag standing for one (`true` is one); anything else is no count. */
const censusCount = (freshness, name) => { const v = freshness[name]; if (typeof v === "boolean") return v ? 1 : 0; return isU64(v) ? v : null; };

/**
 * Reads `freshness.causeEvidence` of a compile's signed (compact) freshness and returns `{ schema, state, verdict, basis, unavailable, gaps, counts, unexplainedUnits,
 * unexplainedUnitsTruncated, unexplainedCount }`. Refuses (ProvenanceError `evidence`) when it is absent or null, not an object, of any schema but
 * rustright.cause-evidence.v1, or when any field is missing or mistyped; when its state, verdict, basis, gaps and counts do not agree with one another
 * (a verdict of `explained` needs no unexplained unit and no gap; `unknown` is the verdict of unexplained == 0 with a gap; the unit lists and their truncation counts add up
 * to the counts; truncated reasons and uncovered units are named as gaps); when a basis that read no reasons carries counts, or claims a census the freshness fields
 * do not show (`census-nothing-rebuilt` needs `available: true` and `notFresh: 0`; `nothing-compiled` needs `available: false` with that reason); when the sibling
 * `unexplainedCount` is not the projection (`null` exactly when the verdict is unknown, else the unexplained count); and, for a basis of `reasons`, when `causeCounts`
 * is missing or disagrees with the counts. It judges nothing: see `requireExplainedCauses`.
 */
export function verifyCauseEvidence(freshness) {
  if (!isRecord(freshness)) refuse("evidence", "the receipt carries no freshness object to read cause evidence from");
  const cause = freshness.causeEvidence;
  if (cause === undefined || cause === null) refuse("evidence", `freshness carries no causeEvidence: the broker did not say why its units were rebuilt, so nothing is concluded from it (a broker that does not sign ${CAUSE_EVIDENCE_SCHEMA} is refused, never trusted as if it had)`);
  const bad = (what) => refuse("evidence", `freshness causeEvidence ${what}`);
  if (!isRecord(cause)) bad("is not an object");
  if (cause.schema !== CAUSE_EVIDENCE_SCHEMA) bad(`names the schema ${shownValue(cause.schema)}, which this gate does not read (it reads ${CAUSE_EVIDENCE_SCHEMA} only)`);
  const { state, verdict, basis, unavailable, gaps, counts } = cause;
  if (!CAUSE_STATES.includes(state)) bad(`has the unknown state ${shownValue(state)}`);
  if (!CAUSE_VERDICTS.includes(verdict)) bad(`has the unknown verdict ${shownValue(verdict)}`);
  if (!CAUSE_BASES.includes(basis)) bad(`has the unknown basis ${shownValue(basis)}`);
  if (typeof cause.historyMigrationNeeded !== "boolean") bad("does not say whether the history needs migration");
  if (!(unavailable === null || nonEmptyString(unavailable))) bad("unavailable is neither null nor a reason");
  if (!Array.isArray(gaps) || !gaps.every(nonEmptyString)) bad("gaps is not a list of reasons");
  if (!isRecord(counts) || CAUSE_COUNT_FIELDS.some((name) => !isU64(counts[name]))) bad(`counts needs ${CAUSE_COUNT_FIELDS.join(", ")} as whole numbers`);
  for (const [list, truncated, total] of [["unexplainedUnits", "unexplainedUnitsTruncated", counts.unexplained], ["uncoveredUnits", "uncoveredUnitsTruncated", counts.uncovered]]) {
    if (!Array.isArray(cause[list]) || !cause[list].every((label) => typeof label === "string") || !isU64(cause[truncated])) bad(`${list} and ${truncated} are not a list of labels and a count`);
    if (cause[list].length + cause[truncated] !== total) bad(`${list} (${cause[list].length}) and ${truncated} (${cause[truncated]}) do not add up to counts (${total})`);
  }
  const hasGap = (name) => gaps.includes(name);
  const unexplained = counts.unexplained;
  if (basis === "reasons") {
    if (unavailable !== null) bad("names an unavailability reason although its reasons were read");
    if (state !== (gaps.length === 0 ? "complete" : "incomplete")) bad(`state ${state} disagrees with its gaps (${gaps.length})`);
    const expected = unexplained > 0 ? "unexplained" : gaps.length > 0 ? "unknown" : "explained";
    if (verdict !== expected) bad(`verdict ${verdict} disagrees with its ${unexplained} unexplained unit(s) and ${gaps.length} gap(s) (${expected})`);
    if (unexplained > counts.units || counts.units > counts.rows) bad("counts are not nested (unexplained <= units <= rows)");
    if ((counts.rowsDropped > 0) !== hasGap("reasons-truncated")) bad("rowsDropped and the reasons-truncated gap disagree");
    if ((counts.uncovered > 0) !== hasGap("rebuilt-unit-without-reason")) bad("uncovered and the rebuilt-unit-without-reason gap disagree");
  } else {
    if (state !== "unavailable") bad(`state ${state} is not unavailable although no reasons were read (basis ${basis})`);
    if (!nonEmptyString(unavailable)) bad("states no reason why its reasons were not read");
    if (CAUSE_COUNT_FIELDS.some((name) => counts[name] !== 0)) bad(`counts reasons that were never read (basis ${basis})`);
    if (basis === "none") {
      if (verdict !== "unknown" || !hasGap("log-not-read")) bad("basis none is the typed unknown: verdict unknown with the log-not-read gap");
    } else {
      if (verdict !== "explained" || gaps.length !== 0) bad(`basis ${basis} is an explained verdict with no gap`);
      if (basis === "nothing-compiled" && !(freshness.available === false && freshness.reason === "nothing-compiled")) bad("claims nothing was compiled but the census does not say so");
      if (basis === "census-nothing-rebuilt" && !(freshness.available === true && censusCount(freshness, "notFresh") === 0 && (censusCount(freshness, "parseErrors") ?? 0) === 0 && (censusCount(freshness, "notFreshPackagesTruncated") ?? 0) === 0)) bad("claims the census rebuilt nothing but the census does not prove it (available, notFresh 0, no parse errors, no truncated list)");
    }
  }
  const projected = verdict === "unknown" ? null : unexplained;
  if (freshness.unexplainedCount !== projected) refuse("evidence", `freshness unexplainedCount (${shownValue(freshness.unexplainedCount)}) is not the projection of causeEvidence (${projected === null ? "null, because the verdict is unknown" : projected})`);
  if (basis === "reasons") {
    const causes = freshness.causeCounts;
    if (!isRecord(causes) || !Object.values(causes).every(isU64)) refuse("evidence", "freshness causeCounts is missing or not a count per cause although causeEvidence read reasons");
    let total = 0; let unexplainedRows = 0;
    for (const [name, n] of Object.entries(causes)) { total += n; if (!EXPLAINED_CAUSES.includes(name)) unexplainedRows += n; }
    if (total !== counts.rows) refuse("evidence", `freshness causeCounts add up to ${total} reason(s), not causeEvidence counts.rows (${counts.rows})`);
    if ((unexplainedRows > 0) !== (unexplained > 0) || unexplained > unexplainedRows) refuse("evidence", `freshness causeCounts name ${unexplainedRows} row(s) without an explained cause, which cannot give ${unexplained} unexplained unit(s)`);
  }
  return {
    schema: cause.schema, state, verdict, basis, unavailable, gaps: [...gaps], counts: { ...counts },
    unexplainedUnits: [...cause.unexplainedUnits], unexplainedUnitsTruncated: cause.unexplainedUnitsTruncated, unexplainedCount: projected,
  };
}

/**
 * The gate's warm judgment over a compile's signed freshness: `verifyCauseEvidence`, then a refusal for every verdict but `explained`. `unknown` (a quiet, caller-logged
 * or artifact-running root whose log was not read, dropped output, an unattributable or uncovered unit) is refused as `cause-unknown`; `unexplained` (a unit
 * rebuilt with no explained cause) as `cause-unexplained`, naming the units the receipt kept. Returns the verified evidence only when the verdict is `explained`.
 */
export function requireExplainedCauses(freshness) {
  const cause = verifyCauseEvidence(freshness);
  if (cause.verdict === "unknown") {
    const why = cause.unavailable ?? cause.gaps[0] ?? "unknown";
    refuse(CAUSE_UNKNOWN, `the broker could not say why its rebuilt units were rebuilt (${why}${cause.gaps.length > 0 ? `; gaps: ${cause.gaps.join(", ")}` : ""}): cause evidence is mandatory for a compile, and an unknown cause is never read as no unexplained unit, so the call is unverified`);
  }
  if (cause.verdict === "unexplained") {
    const named = cause.unexplainedUnits.slice(0, 5).map((label) => label.slice(0, 120)).join("; ");
    refuse(CAUSE_UNEXPLAINED, `${cause.counts.unexplained} rebuilt unit(s) have no explained cause (${named}${cause.counts.unexplained > 5 || cause.unexplainedUnitsTruncated > 0 ? "; more not listed" : ""}): a rebuild the broker cannot explain is not a success`);
  }
  return cause;
}

// ---- GB-50: the full attribution a Clippy (or check) route needs, read from the signed detail descriptor, never from the compact counts ----
export const STAGE_EVIDENCE_SCHEMA = "rustright.stage-evidence/1";

/**
 * `rustright_protocol::attestation::verify_attribution_detail(receipt, file_bytes)`, case for case (vector attribution-detail-vectors.v1.json). `receipt` is a
 * receipt `verifyReceiptStrict` already accepted (so its compact `attribution`, `toolchain` and the descriptor's sha256 are signed) and `bytes` the evidence file the
 * descriptor names. Checks in order: the descriptor is present and `bytes` hash to its sha256 (`detail`); the schema is rustright.stage-evidence/1 (`schema`); the
 * file's `id` is this receipt's request (`job`) and its `toolchain` the receipt's (`evidence`); its attribution is available, `truncated` is exactly 0 in the file and in
 * the compact twin (`truncated`); every row names package, target and kind with numeric errors and warnings; the compact `units`, `packages`, `errors`, `warnings`,
 * `otherMessages` and `freshUnits` equal the values recomputed from the rows. Returns `{ rows, failedUnits }`. Counts alone never pass.
 */
export function verifyAttributionDetail(receipt, bytes) {
  const compact = receipt?.attribution;
  if (!isRecord(compact)) refuse("evidence", "the receipt carries no attribution");
  const descriptor = compact.detail;
  if (!isRecord(descriptor) || typeof descriptor.sha256 !== "string" || typeof descriptor.path !== "string") refuse("detail", "the attribution names no evidence file");
  if (sha256Hex(bytes) !== descriptor.sha256) refuse("detail", "the evidence file is not the one the receipt signed");
  let document;
  try { document = JSON.parse(Buffer.from(bytes).toString("utf8")); } catch { refuse("malformed", "the evidence file is not JSON"); }
  if (!isRecord(document) || document.schema !== STAGE_EVIDENCE_SCHEMA) refuse("schema", "unknown stage evidence schema");
  if (typeof document.id !== "string" || document.id !== receipt.id) refuse("job", "the evidence file is for another request");
  if (typeof document.toolchain !== "string" || document.toolchain !== receipt.toolchain) refuse("evidence", "the evidence file names another toolchain than the receipt");
  const attribution = document.attribution;
  if (!isRecord(attribution) || attribution.available !== true) refuse("evidence", "the evidence file holds no available attribution");
  if (!isU64(attribution.truncated)) refuse("evidence", "the evidence file does not say whether the attribution is complete");
  if (attribution.truncated !== 0 || compact.truncated !== 0) refuse("truncated", "the attribution is truncated or its completeness is not stated");
  const rows = attribution.units;
  if (!Array.isArray(rows)) refuse("evidence", "the evidence file holds no unit rows");
  const packages = new Set(); let errors = 0; let warnings = 0; let other = 0; let fresh = 0; let failedUnits = 0;
  for (const row of rows) {
    const text = (name) => (isRecord(row) && typeof row[name] === "string" && row[name] !== "" ? row[name] : null);
    const count = (name) => (isRecord(row) && isU64(row[name]) ? row[name] : null);
    const [pkg, target, kind, e, w] = [text("package"), text("target"), text("kind"), count("errors"), count("warnings")];
    if (pkg === null || target === null || kind === null || e === null || w === null) refuse("evidence", "a unit row lacks its package, target, kind, errors or warnings");
    packages.add(pkg); errors += e; warnings += w; other += count("otherMessages") ?? 0; fresh += row.fresh === true ? 1 : 0; failedUnits += e > 0 ? 1 : 0;
  }
  const derived = { units: rows.length, packages: packages.size, errors, warnings, otherMessages: other, freshUnits: fresh };
  for (const [name, value] of Object.entries(derived)) if (compact[name] !== value) refuse("evidence", `the receipt's compact attribution ${name} disagrees with the evidence file's rows`);
  return { rows, failedUnits };
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
