// Pipeline stage receipts and their verifier (architecture-v3 sections 4-5, "Receipts").
//
// A receipt records a fact about one stage of one request. It never authorizes execution alone: the
// broker (RustRight) signs receipts and issues short-lived capabilities; this module only checks that
// a chain of receipts is well formed, linked, fresh and complete. Signature checking is an injected
// interface (`verifySignature`); this module does no cryptography beyond SHA-256 hashing and invents none.
//
// Chain: ADMISSION -> SCAN_A -> ROUTE -> SCAN_B -> BUILD -> SIGN -> QUALIFIED -> PUBLISH_INTENT -> PUBLISHED.
// Each stage requires exactly its predecessor; `parents` carries that predecessor's receipt hash.
import crypto from "node:crypto";
import { canonicalJson, digestScanSet, scanClassOf } from "./deterministic-scans.mjs";

export const RECEIPT_SCHEMA = "rightkit.pipeline-receipt/1";
export const STAGES = Object.freeze(["ADMISSION", "SCAN_A", "ROUTE", "SCAN_B", "BUILD", "SIGN", "QUALIFIED", "PUBLISH_INTENT", "PUBLISHED"]);
export const OPERATIONS = Object.freeze(["dev", "build", "qualify", "sign", "publish"]);
/** The last stage each operation may reach. */
export const OPERATION_LAST_STAGE = Object.freeze({ dev: "BUILD", build: "BUILD", sign: "SIGN", qualify: "QUALIFIED", publish: "PUBLISHED" });
/** Each stage requires only these predecessors. */
export const PREREQUISITES = Object.freeze({
  ADMISSION: [], SCAN_A: ["ADMISSION"], ROUTE: ["SCAN_A"], SCAN_B: ["ROUTE"], BUILD: ["SCAN_B"],
  SIGN: ["BUILD"], QUALIFIED: ["SIGN"], PUBLISH_INTENT: ["QUALIFIED"], PUBLISHED: ["PUBLISH_INTENT"],
});
const RELEASE_ID_STAGES = new Set(["SIGN", "QUALIFIED", "PUBLISH_INTENT", "PUBLISHED"]);
/** Stages only a publishable request may reach: signing and everything after it. A dev or non-publishable chain stops at BUILD. */
const PUBLISHABLE_ONLY_STAGES = new Set(RELEASE_ID_STAGES);

/**
 * What ADMISSION's evidence allows for `stage`, as [{ code, message }]. ADMISSION is the sole authority:
 * a missing or malformed ADMISSION authorizes nothing, and `publishable` must be exactly true to reach SIGN or later.
 */
function admissionProblems(admission, stage) {
  const out = [];
  if (!admission) return [{ code: "STAGE_NOT_AUTHORIZED", message: `no valid ADMISSION evidence authorizes ${stage}` }];
  if (!Array.isArray(admission.authorizedStages) || !admission.authorizedStages.includes(stage)) out.push({ code: "STAGE_NOT_AUTHORIZED", message: `ADMISSION does not authorize ${stage}` });
  if (admission.publishable !== true && PUBLISHABLE_ONLY_STAGES.has(stage)) out.push({ code: "NOT_PUBLISHABLE", message: `ADMISSION does not mark this request publishable:true; it cannot reach ${stage}` });
  return out;
}

const ENVELOPE_STRINGS = ["receiptId", "jobId", "requestId", "repositoryId", "commit", "inputDigest", "configDigest", "policyDigest", "stagePlanDigest", "runtimeDigest", "executor", "issuedAt", "expiresAt", "keyId", "signature"];
const HEX64 = /^[0-9a-f]{64}$/;
const sha256 = (value) => crypto.createHash("sha256").update(value).digest("hex");

/** Domain-separated bytes a signature covers: receipt type bound in, signature field excluded. */
export function signingBytes(receipt) {
  const { signature, ...rest } = receipt;
  return Buffer.from(`${RECEIPT_SCHEMA}\0${receipt.type}\0${canonicalJson(rest)}`, "utf8");
}

/** The hash a child receipt records in `parents`: covers the whole receipt including its signature. */
export function receiptHash(receipt) {
  return sha256(Buffer.concat([Buffer.from(`${RECEIPT_SCHEMA}.hash\0`, "utf8"), Buffer.from(canonicalJson(receipt), "utf8")]));
}

/** In-memory request ledger for tests and single-process brokers: records which job owns a requestId. */
export function createMemoryLedger() {
  const owners = new Map(); const consumed = new Set();
  return {
    requestOwner: (requestId) => owners.get(requestId),
    isConsumed: (receiptId) => consumed.has(receiptId),
    record(chain) { for (const r of chain) { if (!owners.has(r.requestId)) owners.set(r.requestId, r.jobId); consumed.add(r.receiptId); } },
  };
}

const isObj = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const nonEmptyString = (v) => typeof v === "string" && v.trim() !== "";
const isHex = (v) => typeof v === "string" && HEX64.test(v);
const nonEmptyArray = (v) => Array.isArray(v) && v.length > 0;

/** Stage-specific evidence checks. Each returns a list of problem messages. */
const STAGE_EVIDENCE = {
  ADMISSION(e) {
    const p = [];
    for (const f of ["inputManifestDigest", "dependencyManifestDigest", "toolchainManifestDigest"]) if (!isHex(e[f])) p.push(`${f} must be a sha256 hex digest`);
    if (!nonEmptyArray(e.authorizedStages) || e.authorizedStages.some((s) => !STAGES.includes(s))) p.push("authorizedStages must list known stages");
    if (typeof e.publishable !== "boolean") p.push("publishable must be a boolean");
    return p;
  },
  SCAN_A: (e, ctx) => scanEvidence(e, "A", ctx),
  ROUTE(e, ctx) {
    const p = [];
    const d = e.decision;
    if (!isObj(d)) return ["decision is required"];
    if (!["local", "ci"].includes(d.executor)) p.push("decision.executor must be local or ci");
    if (!["public", "private"].includes(d.visibility)) p.push("decision.visibility must be public or private (unknown visibility is rejected)");
    if (d.visibility === "private" && d.executor !== "local") p.push("a private repository never routes to CI");
    if (typeof d.localGrant !== "boolean") p.push("decision.localGrant must be a boolean");
    if (d.executor === "local" && !nonEmptyString(String(e.leaseFence ?? ""))) p.push("a local route needs a leaseFence");
    if (d.executor === "ci" && !isObj(e.dispatchBinding)) p.push("a CI route needs a dispatchBinding");
    if (!nonEmptyString(d.policyVersion)) p.push("decision.policyVersion is required");
    return p;
  },
  SCAN_B: (e, ctx) => scanEvidence(e, "B", ctx),
  BUILD(e) {
    const p = [];
    if (!isHex(e.artifactManifestDigest)) p.push("artifactManifestDigest must be a sha256 hex digest");
    if (!nonEmptyArray(e.artifacts) || e.artifacts.some((a) => !nonEmptyString(a?.path) || !isHex(a?.sha256))) p.push("artifacts must list path and sha256 for every output");
    for (const f of ["generationEvidenceDigest", "provenanceDigest"]) if (!isHex(e[f])) p.push(`${f} must be a sha256 hex digest`);
    if (!nonEmptyArray(e.targetMatrix)) p.push("targetMatrix must list the built targets");
    return p;
  },
  SIGN(e, ctx) {
    const p = [];
    if (!["SIGN", "SEAL"].includes(e.mode)) p.push("mode must be SIGN or SEAL");
    for (const f of ["inputManifestDigest", "outputManifestDigest", "signingEvidenceDigest"]) if (!isHex(e[f])) p.push(`${f} must be a sha256 hex digest`);
    if (!nonEmptyString(e.signingProfile)) p.push("signingProfile is required");
    const build = ctx.byStage.BUILD;
    if (build && e.inputManifestDigest !== build.evidence?.artifactManifestDigest) p.push("inputManifestDigest does not equal the BUILD artifactManifestDigest");
    return p;
  },
  QUALIFIED(e, ctx) {
    const p = [];
    if (!isHex(e.artifactManifestDigest)) p.push("artifactManifestDigest must be a sha256 hex digest");
    if (!nonEmptyArray(e.testMatrix) || e.testMatrix.some((t) => !nonEmptyString(t?.name) || t?.status !== "pass")) p.push("testMatrix must be non-empty and every row must pass");
    const sign = ctx.byStage.SIGN;
    if (sign && e.artifactManifestDigest !== sign.evidence?.outputManifestDigest) p.push("artifactManifestDigest does not equal the SIGN outputManifestDigest (qualification covers the final signed bytes)");
    return p;
  },
  PUBLISH_INTENT(e, ctx) {
    const p = [];
    if (!isHex(e.releaseManifestDigest)) p.push("releaseManifestDigest must be a sha256 hex digest");
    if (!nonEmptyArray(e.destinations) || e.destinations.some((d) => !nonEmptyString(d?.id) || !nonEmptyString(d?.identity) || !nonEmptyString(d?.version) || !isHex(d?.expectedDigest) || !nonEmptyString(d?.idempotencyKey))) {
      p.push("destinations must name id, identity, version, expectedDigest and idempotencyKey");
    }
    const q = ctx.byStage.QUALIFIED;
    if (q && e.releaseManifestDigest !== q.evidence?.artifactManifestDigest) p.push("releaseManifestDigest does not equal the QUALIFIED artifactManifestDigest");
    return p;
  },
  PUBLISHED(e, ctx) {
    const p = [];
    const intent = ctx.byStage.PUBLISH_INTENT;
    if (!nonEmptyArray(e.entries)) return ["entries must list every destination outcome"];
    for (const d of intent?.evidence?.destinations ?? []) {
      const entry = e.entries.find((x) => x?.destinationId === d.id);
      if (!entry) p.push(`no entry for destination ${d.id}`);
      else if (entry.outcome !== "verified") p.push(`destination ${d.id} is ${entry.outcome}, not verified`);
      else if (entry.remoteDigest !== d.expectedDigest) p.push(`destination ${d.id} readback digest differs from the intent`);
    }
    return p;
  },
};

function scanEvidence(e, cls, ctx) {
  const p = [];
  const other = cls === "A" ? "B" : "A";
  if (e.scanClass !== cls) p.push(`scanClass must be ${cls}`);
  if (!nonEmptyArray(e.scans)) return [...p, "scans must list every required scan result"];
  if (cls === "A" && !isHex(e.advisorySnapshotDigest)) p.push("advisorySnapshotDigest must be a sha256 hex digest");
  if (cls === "A" && !nonEmptyString(e.advisorySnapshotTime)) p.push("advisorySnapshotTime is required");
  for (const s of e.scans) {
    const derived = scanClassOf(s?.id);
    if (s?.class === other || derived === other) p.push(`${cls === "A" ? "SCAN_A claims" : "SCAN_B carries"} class ${other} scan ${s?.id}`);
    else if (s?.class !== cls) p.push(`scan ${s?.id} has class ${s?.class}, expected ${cls}`);
    if (s?.status !== "pass") p.push(`scan ${s?.id} is ${s?.status}, not pass`);
  }
  const ids = new Set(e.scans.map((s) => s?.id));
  const required = ctx.requiredScans?.[cls];
  const declared = Array.isArray(e.requiredScanIds) ? e.requiredScanIds : null;
  if (!declared) p.push("requiredScanIds is required");
  // The required set comes only from the injected policy matrix. A receipt's own declaration is never trusted:
  // without the policy, verification fails closed.
  if (!Array.isArray(required) || required.some((id) => typeof id !== "string")) {
    p.push(`trusted required scan set for class ${cls} is not supplied by policy; verification fails closed`);
    return p;
  }
  for (const id of required) if (!ids.has(id)) p.push(`required class ${cls} scan ${id} is missing`);
  if (declared && canonicalJson([...required].sort()) !== canonicalJson([...declared].sort())) p.push(`requiredScanIds differ from the trusted class ${cls} set`);
  if (e.scanSetDigest !== digestScanSet(e.scans)) p.push("scanSetDigest does not match the listed scans");
  if (cls === "B" && !nonEmptyArray(required)) p.push("SCAN_B must require at least one class B scan");
  return p;
}

/**
 * Verify a chain of receipts (ADMISSION first, ending at the stage being authorized).
 *
 * options:
 *   verifySignature({ receipt, keyId, signingBytes, signature }) -> boolean   REQUIRED (the broker supplies keys)
 *   now                 ISO string or Date, default current time
 *   expected            { requestId, operation, inputDigest, stage, repositoryId, commit, jobId, releaseId, targetSet } (each optional but checked when given)
 *   requiredScans       { A: [ids], B: [ids] } trusted required sets from the policy matrix: REQUIRED; absence rejects (NO_SCAN_POLICY)
 *   ledger              { requestOwner(requestId), isConsumed(receiptId) }   REQUIRED: without it verification fails (NO_LEDGER)
 *   keyAllowed          (keyId, receipt) -> boolean, for revocations
 * Returns { ok, problems: [{ code, stage, message }], head }. Never throws on a bad receipt.
 */
export function verifyReceiptChain(chain, options = {}) {
  const problems = [];
  const bad = (code, stage, message) => problems.push({ code, stage: stage ?? null, message });
  if (typeof options.verifySignature !== "function") return { ok: false, problems: [{ code: "NO_SIGNATURE_VERIFIER", stage: null, message: "a signature verifier must be supplied by the broker" }], head: null };
  if (!Array.isArray(chain) || chain.length === 0) return { ok: false, problems: [{ code: "EMPTY_CHAIN", stage: null, message: "no receipts" }], head: null };
  const now = new Date(options.now ?? Date.now()).getTime();
  const expected = options.expected ?? {};
  const byStage = {};
  let previous = null;
  // Replay protection is mandatory: without a ledger the chain cannot be proven fresh, so it fails closed.
  if (typeof options.ledger?.requestOwner !== "function" || typeof options.ledger?.isConsumed !== "function") {
    bad("NO_LEDGER", null, "a request ledger with requestOwner and isConsumed must be supplied; replay cannot be ruled out without it");
  }
  // One release id for the whole chain: the ADMISSION one when it has it, else the first release-stage one.
  const releaseRef = chain.find((r) => isObj(r) && nonEmptyString(r.releaseId))?.releaseId;
  // ADMISSION fixes which stages may run and whether the request may publish at all.
  const admission = isObj(chain[0]) && chain[0].type === "ADMISSION" && isObj(chain[0].evidence) ? chain[0].evidence : null;

  chain.forEach((receipt, index) => {
    const stage = isObj(receipt) ? receipt.type : null;
    if (!isObj(receipt)) { bad("MALFORMED", null, `receipt ${index} is not an object`); return; }
    if (receipt.schema !== RECEIPT_SCHEMA) bad("SCHEMA", stage, `schema must be ${RECEIPT_SCHEMA}`);
    if (!STAGES.includes(stage)) { bad("UNKNOWN_STAGE", stage, `unknown receipt type ${JSON.stringify(stage)}`); return; }
    const wantStage = STAGES[index];
    if (stage !== wantStage) bad("WRONG_STAGE", stage, `position ${index} requires ${wantStage}, got ${stage}`);
    for (const f of ENVELOPE_STRINGS) if (!nonEmptyString(receipt[f])) bad("ENVELOPE", stage, `${f} is required`);
    if (!Number.isInteger(receipt.attempt) || receipt.attempt < 1) bad("ENVELOPE", stage, "attempt must be an integer >= 1");
    if (!Array.isArray(receipt.targetSet)) bad("ENVELOPE", stage, "targetSet must be an array");
    if (!OPERATIONS.includes(receipt.operation)) bad("WRONG_OPERATION", stage, `unknown operation ${JSON.stringify(receipt.operation)}`);
    if (RELEASE_ID_STAGES.has(stage) && !nonEmptyString(receipt.releaseId)) bad("ENVELOPE", stage, "releaseId is required");
    if (!isObj(receipt.evidence)) bad("EVIDENCE", stage, "evidence is required");

    // The operation bounds how far a request may go.
    const last = OPERATION_LAST_STAGE[receipt.operation];
    if (last && STAGES.indexOf(stage) > STAGES.indexOf(last)) bad("WRONG_OPERATION", stage, `operation ${receipt.operation} does not authorize ${stage}`);

    // ADMISSION bounds the chain: only its authorized stages, and nothing from SIGN on unless publishable:true.
    for (const x of admissionProblems(admission, stage)) bad(x.code, stage, x.message);

    // Binding to the caller's expectations.
    if (expected.requestId !== undefined && receipt.requestId !== expected.requestId) bad("WRONG_REQUEST_ID", stage, "requestId differs from the expected request");
    if (expected.operation !== undefined && receipt.operation !== expected.operation) bad("WRONG_OPERATION", stage, `operation ${receipt.operation} differs from expected ${expected.operation}`);
    if (expected.inputDigest !== undefined && receipt.inputDigest !== expected.inputDigest) bad("WRONG_INPUT_DIGEST", stage, "inputDigest differs from the expected input");
    for (const f of ["repositoryId", "commit", "jobId", "releaseId"]) if (expected[f] !== undefined && receipt[f] !== expected[f]) bad("BINDING", stage, `${f} differs from the expected value`);
    if (expected.targetSet !== undefined && canonicalJson(receipt.targetSet ?? null) !== canonicalJson(expected.targetSet)) bad("BINDING", stage, "targetSet differs from the expected value");
    if (releaseRef !== undefined && receipt.releaseId !== undefined && receipt.releaseId !== releaseRef) bad("BINDING", stage, "releaseId differs from the chain's release");

    // Consistency across the chain.
    const first = chain[0];
    if (index > 0 && isObj(first)) {
      for (const f of ["requestId", "jobId", "repositoryId", "commit", "inputDigest", "configDigest", "policyDigest", "stagePlanDigest", "operation", "targetSet"]) {
        if (f === "targetSet" ? canonicalJson(receipt[f] ?? null) !== canonicalJson(first[f] ?? null) : receipt[f] !== first[f]) bad(f === "requestId" ? "WRONG_REQUEST_ID" : f === "inputDigest" ? "WRONG_INPUT_DIGEST" : f === "operation" ? "WRONG_OPERATION" : "BINDING", stage, `${f} differs from the ADMISSION receipt`);
      }
    }

    // Freshness.
    const issued = Date.parse(receipt.issuedAt); const expires = Date.parse(receipt.expiresAt);
    if (Number.isNaN(issued) || Number.isNaN(expires)) bad("ENVELOPE", stage, "issuedAt and expiresAt must be ISO timestamps");
    else {
      if (expires <= issued) bad("ENVELOPE", stage, "expiresAt must be after issuedAt");
      if (expires <= now) bad("EXPIRED", stage, `expired at ${receipt.expiresAt}`);
      if (issued > now + 60_000) bad("ENVELOPE", stage, "issuedAt is in the future");
      if (previous && issued < Date.parse(previous.issuedAt)) bad("ORDER", stage, "issued before its parent receipt");
    }

    // Hash chain.
    const wantParents = previous ? [receiptHash(previous)] : [];
    if (!Array.isArray(receipt.parents) || canonicalJson(receipt.parents) !== canonicalJson(wantParents)) bad("BROKEN_CHAIN", stage, previous ? `parents must be exactly [hash of ${previous.type}]` : "ADMISSION has no parents");

    // Replay (the ledger is required; see NO_LEDGER above).
    if (typeof options.ledger?.requestOwner === "function" && typeof options.ledger?.isConsumed === "function") {
      const owner = options.ledger.requestOwner(receipt.requestId);
      if (owner !== undefined && owner !== receipt.jobId) bad("REPLAYED_REQUEST_ID", stage, "requestId already belongs to another job");
      if (options.ledger.isConsumed(receipt.receiptId)) bad("REPLAYED_RECEIPT", stage, "receipt already consumed");
    }

    // Signature (injected).
    if (options.keyAllowed && !options.keyAllowed(receipt.keyId, receipt)) bad("KEY_REVOKED", stage, `key ${receipt.keyId} is not allowed for this receipt`);
    let signatureOk = false;
    try { signatureOk = options.verifySignature({ receipt, keyId: receipt.keyId, signingBytes: signingBytes(receipt), signature: receipt.signature }) === true; } catch { signatureOk = false; }
    if (!signatureOk) bad("BAD_SIGNATURE", stage, "signature did not verify");

    byStage[stage] = receipt;
    // Stage evidence last: it can look at the earlier stages collected so far.
    if (isObj(receipt.evidence) && STAGE_EVIDENCE[stage]) {
      for (const message of STAGE_EVIDENCE[stage](receipt.evidence, { byStage, requiredScans: options.requiredScans })) {
        const code = /trusted required scan set/.test(message) ? "NO_SCAN_POLICY" : /missing|claims|carries/.test(message) && (stage === "SCAN_A" || stage === "SCAN_B") ? (/claims|carries/.test(message) ? "SCAN_CLASS_MIX" : "SCAN_INCOMPLETE") : "EVIDENCE";
        bad(code, stage, message);
      }
    }
    previous = receipt;
  });

  const head = chain[chain.length - 1];
  if (expected.stage !== undefined) {
    if (!STAGES.includes(expected.stage)) bad("WRONG_STAGE", null, `unknown expected stage ${expected.stage}`);
    else if (head?.type !== expected.stage) bad("WRONG_STAGE", head?.type, `chain ends at ${head?.type}, expected ${expected.stage}`);
  }
  return { ok: problems.length === 0, problems, head: isObj(head) ? { type: head.type, hash: receiptHash(head) } : null };
}

/**
 * Verify that a new stage may start: the chain so far must verify and end at the stage `stage` requires.
 * Returns the same result shape; `authorizes` is the stage the chain authorizes next (or null).
 */
export function authorizeNextStage(chain, stage, options = {}) {
  const need = PREREQUISITES[stage];
  if (!need) return { ok: false, problems: [{ code: "UNKNOWN_STAGE", stage, message: `unknown stage ${stage}` }], head: null, authorizes: null };
  if (need.length === 0) return { ok: !chain?.length, problems: chain?.length ? [{ code: "WRONG_STAGE", stage, message: "ADMISSION starts a chain" }] : [], head: null, authorizes: chain?.length ? null : stage };
  const result = verifyReceiptChain(chain, { ...options, expected: { ...(options.expected ?? {}), stage: need[0] } });
  // The requested stage itself must sit inside the operation's boundary and ADMISSION's authorized stages.
  const first = Array.isArray(chain) ? chain[0] : null;
  const last = OPERATION_LAST_STAGE[first?.operation];
  if (!last || STAGES.indexOf(stage) > STAGES.indexOf(last)) result.problems.push({ code: "WRONG_OPERATION", stage, message: `operation ${first?.operation} does not authorize ${stage}` });
  const admission = first?.type === "ADMISSION" && isObj(first.evidence) ? first.evidence : null;
  for (const x of admissionProblems(admission, stage)) result.problems.push({ code: x.code, stage, message: x.message });
  result.ok = result.problems.length === 0;
  return { ...result, authorizes: result.ok ? stage : null };
}
