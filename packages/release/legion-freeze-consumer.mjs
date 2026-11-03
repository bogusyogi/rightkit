// RightKit's half of the Legion frozen-scope broker (bogusyogi/rightkit#799; protocol: Orthic-Labs/legion
// docs/architecture/frozen-scope-broker.md "Shared consumers & coverage boundary", pinned at 2a6597a7c932b8a636462a08a28ee9a28e7d49c6,
// implementation engine/bins/legion/src/commands/freeze.rs and freeze_consumer.rs).
//
// What this module is: the pure parts a RightKit owner needs to use the broker, with no authority of its own.
//   1. Broker requests (`legion authority freeze broker`, one JSON request on stdin, one JSON answer): status, check, push, consume.
//      `check` is diagnostic (`deliveryLease: false`) and is never a grant; `push` sends an explicit immutable source object and is
//      the only managed delivery. Enrollment (`admin-enroll`) is root-only and is not reachable from here.
//   2. The `legion-native-consume-v1` frames a native owner adapter speaks on the broker's private pipes (prepare, prepared, commit,
//      complete), the exact PreparedEffect serialization the broker digests, and a one-transaction state machine that refuses any
//      deviation (an unknown key, a wrong nonce, a second prepare, a commit digest that is not the prepared one).
//   3. The publication binding: the typed effect for a `Publish` consume is built from the SEALED release that publication already
//      verifies (verifyPublicationRelease: sealed manifest, signing and notarization receipts, ownership evidence) and from the
//      artifact bytes read once, so the artifact digest the broker authorizes is the digest of the bytes that are uploaded.
//
// What it is not: no broker is installed, no adapter is enrolled and no native adapter executable exists here. The broker accepts only
// a native ELF or Mach-O executable pinned by a root-owned enrollment and refuses scripts, so this module cannot be the adapter; a native
// adapter links this protocol. Nothing in this module runs a build, a push or an upload. See docs/GATE-CONTRACT.md [GC-I23].
import { createHash } from "node:crypto";
import { spawnSync } from "./background-process.mjs";

export const CONSUME_PROTOCOL = "legion-native-consume-v1";
/** Every frame, in both directions, is at most this many bytes (the broker's CAP), newline included. */
export const FRAME_CAP_BYTES = 32 * 1024;
export const CONSUME_OPERATIONS = Object.freeze(["Build", "Dequeue", "Publish"]);
const MAX_BROKER_ANSWER_BYTES = 1024 * 1024;
const DIGEST = /^[0-9a-f]{64}$/;
const OID = /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/;

export class FreezeConsumerError extends Error {
  constructor(code, message) { super(`[${code}] ${message}`); this.name = "FreezeConsumerError"; this.code = code; }
}
const refuse = (code, message) => { throw new FreezeConsumerError(code, message); };
const isRecord = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const sha256Hex = (data) => createHash("sha256").update(data).digest("hex");

/** The broker's text rule: 1 to 1024 bytes, no control character. */
export const isBrokerText = (value) => typeof value === "string" && value.length > 0 && Buffer.byteLength(value) <= 1024 && !/[\u0000-\u001f\u007f-\u009f]/.test(value);
export const isDigest = (value) => typeof value === "string" && DIGEST.test(value);
/** A full object id: 40 or 64 lower-case hex digits and not all zero (an absent object). */
export const isObjectId = (value) => typeof value === "string" && OID.test(value) && !/^0+$/.test(value);

function exactKeys(value, keys, where) {
  if (!isRecord(value)) refuse("FREEZE_FRAME_INVALID", `${where} must be a JSON object`);
  const extra = Object.keys(value).filter((key) => !keys.includes(key));
  const missing = keys.filter((key) => !Object.hasOwn(value, key));
  if (extra.length || missing.length) refuse("FREEZE_FRAME_INVALID", `${where}: unknown key(s) [${extra.join(", ")}] or missing key(s) [${missing.join(", ")}]`);
}

// ---- 1. broker requests ---------------------------------------------------------------------------------------------------------
const requireId = (id) => { if (!isBrokerText(id)) refuse("FREEZE_REQUEST_INVALID", "the enrollment id must be non-empty text without control characters"); return id; };
const requireSource = (source) => { if (!isObjectId(source)) refuse("FREEZE_REQUEST_INVALID", "source must be an immutable full object id (40 or 64 lower-case hex digits), never a ref or a short id"); return source; };
const requireCandidate = (candidate) => { if (candidate !== undefined && candidate !== null && !isBrokerText(candidate)) refuse("FREEZE_REQUEST_INVALID", "candidate must be a prepared candidate key"); return candidate ?? null; };

/** The typed requests of `legion authority freeze broker`. Field names are the broker's (serde `op`, snake_case, unknown keys refused). */
export const brokerRequests = Object.freeze({
  status: ({ id }) => ({ op: "status", id: requireId(id) }),
  /** Diagnostic only: its answer carries `deliveryLease: false` and grants nothing a later upload may rely on. */
  check: ({ id, source, candidate, operation }) => {
    if (!["Build", "Dequeue", "Publish", "Push"].includes(operation)) refuse("FREEZE_REQUEST_INVALID", `operation must be one of Build, Dequeue, Publish, Push, not ${JSON.stringify(operation)}`);
    return { op: "check", id: requireId(id), source: requireSource(source), candidate: requireCandidate(candidate), operation };
  },
  /** Managed delivery: the broker pushes this exact object to its enrolled URL and ref. The caller names no remote, ref or argv. */
  push: ({ id, source, candidate }) => ({ op: "push", id: requireId(id), source: requireSource(source), candidate: requireCandidate(candidate) }),
  /** A native owner transaction: the enrolled adapter, never a caller-chosen executable, prepares and performs the effect. */
  consume: ({ id, operation, nativeRecordId }) => {
    if (!CONSUME_OPERATIONS.includes(operation)) refuse("FREEZE_REQUEST_INVALID", `operation must be one of ${CONSUME_OPERATIONS.join(", ")}, not ${JSON.stringify(operation)}`);
    if (!isBrokerText(nativeRecordId)) refuse("FREEZE_REQUEST_INVALID", "nativeRecordId must be non-empty text without control characters");
    return { op: "consume", id: requireId(id), operation, native_record_id: nativeRecordId };
  },
});

/** One broker answer: bounded JSON. A non-zero exit, a timeout, an unreadable answer or a failed spawn is a refusal; nothing is retried. */
export function callBroker(request, { launcher, timeoutMs = 120_000, run = spawnSync } = {}) {
  if (!Array.isArray(launcher) || launcher.length === 0 || launcher.some((part) => typeof part !== "string" || part === "")) refuse("FREEZE_LAUNCHER_INVALID", "the restricted broker launcher must be a non-empty argv array (the restricted launcher or the authenticated SSH forced command; never admin-enroll)");
  const body = JSON.stringify(request);
  const result = run(launcher[0], launcher.slice(1), { input: body, encoding: "utf8", timeout: timeoutMs, maxBuffer: MAX_BROKER_ANSWER_BYTES, windowsHide: true });
  if (result.error) refuse("FREEZE_BROKER_UNREACHABLE", `the broker could not be reached: ${result.error.message}; an unreachable broker denies`);
  if (result.status !== 0) refuse("FREEZE_BROKER_DENIED", `the broker refused ${request.op}: ${String(result.stderr || result.stdout || `exit ${result.status}`).trim().slice(0, 500)}`);
  let answer;
  try { answer = JSON.parse(result.stdout); } catch { refuse("FREEZE_BROKER_ANSWER_INVALID", `the broker's ${request.op} answer is not JSON`); }
  if (!isRecord(answer)) refuse("FREEZE_BROKER_ANSWER_INVALID", `the broker's ${request.op} answer is not an object`);
  return answer;
}

/**
 * A `check` answer, read as what it is: a diagnostic. It is refused unless it says `allowed`, names the source that was asked about, and
 * says `deliveryLease: false`; the returned value carries no field a later upload could present as permission.
 */
export function readCheckAnswer(answer, { source }) {
  if (answer.status !== "allowed") refuse("FREEZE_CHECK_NOT_ALLOWED", `the broker did not allow ${source}: ${JSON.stringify(answer.status)}`);
  if (answer.deliveryLease !== false) refuse("FREEZE_CHECK_LEASE", "a check answer must say deliveryLease false; an answer that does not is not the broker's check");
  if (answer.source !== source) refuse("FREEZE_CHECK_SOURCE", `the broker checked ${answer.source}, not the requested ${source}`);
  return { diagnostic: true, revision: answer.revision, sequence: answer.sequence, source: answer.source, closure: answer.closure };
}

/** A `push` answer: delivered only when the broker says `pushed` for exactly the source that was sent. */
export function readPushAnswer(answer, { source }) {
  if (answer.status !== "pushed") refuse("FREEZE_PUSH_NOT_DELIVERED", `the broker did not report a push of ${source}: ${JSON.stringify(answer.status)}`);
  if (answer.source !== source) refuse("FREEZE_PUSH_SOURCE", `the broker pushed ${answer.source}, not the requested ${source}`);
  return { source: answer.source, closure: answer.closure, revision: answer.revision, sequence: answer.sequence };
}

/** Broker-mediated push of one enrolled closure: the source is the immutable commit object, the destination is the enrollment's. */
export function pushThroughBroker({ id, source, candidate, launcher, timeoutMs, run }) {
  const request = brokerRequests.push({ id, source, candidate });
  return readPushAnswer(callBroker(request, { launcher, timeoutMs, run }), { source });
}

// ---- 2. the native consume protocol ---------------------------------------------------------------------------------------------
/** The PreparedEffect fields, in the order the broker serializes (and digests) them. */
export const PREPARED_EFFECT_KEYS = Object.freeze(["repository", "operation", "native_record_id", "source_oid", "candidate", "endpoint", "destination", "native_effect_sha256", "provenance_sha256", "artifact_sha256"]);

/** Validate a typed effect as the broker does and return it with its keys in the broker's order (optional fields as null). */
export function normalizeEffect(effect) {
  if (!isRecord(effect)) refuse("FREEZE_EFFECT_INVALID", "the effect must be an object");
  const unknown = Object.keys(effect).filter((key) => !PREPARED_EFFECT_KEYS.includes(key));
  if (unknown.length) refuse("FREEZE_EFFECT_INVALID", `unknown effect key(s): ${unknown.join(", ")}`);
  const out = {
    repository: effect.repository, operation: effect.operation, native_record_id: effect.native_record_id, source_oid: effect.source_oid,
    candidate: effect.candidate ?? null, endpoint: effect.endpoint, destination: effect.destination,
    native_effect_sha256: effect.native_effect_sha256, provenance_sha256: effect.provenance_sha256, artifact_sha256: effect.artifact_sha256 ?? null,
  };
  const bad = (what) => refuse("FREEZE_EFFECT_INVALID", `effect ${what}`);
  if (!isBrokerText(out.repository)) bad("repository must be text");
  if (!CONSUME_OPERATIONS.includes(out.operation)) bad("operation must be Build, Dequeue or Publish");
  if (!isBrokerText(out.native_record_id)) bad("native_record_id must be text");
  if (!isObjectId(out.source_oid)) bad("source_oid must be a full immutable object id");
  if (out.candidate !== null && !isDigest(out.candidate)) bad("candidate must be a sha256 hex digest when present");
  if (!isBrokerText(out.endpoint) || !isBrokerText(out.destination)) bad("endpoint and destination must be text");
  if (!isDigest(out.native_effect_sha256) || !isDigest(out.provenance_sha256)) bad("native_effect_sha256 and provenance_sha256 must be sha256 hex digests");
  if (out.artifact_sha256 !== null && !isDigest(out.artifact_sha256)) bad("artifact_sha256 must be a sha256 hex digest when present");
  if (out.operation === "Publish" && out.artifact_sha256 === null) bad("a Publish effect must carry artifact_sha256");
  return out;
}

/** The compact serialization the broker digests: the effect's fields in struct order, optional fields as null. */
export const serializeEffect = (effect) => JSON.stringify(normalizeEffect(effect));
/** The sha256 the broker computes over its own serialization and sends in `commit`; a mismatch means the adapter prepared something else. */
export const effectDigest = (effect) => sha256Hex(serializeEffect(effect));

/** One frame as a line: compact JSON and a newline, never over the cap. */
export function encodeFrame(frame) {
  const text = `${JSON.stringify(frame)}\n`;
  if (Buffer.byteLength(text) > FRAME_CAP_BYTES) refuse("FREEZE_FRAME_TOO_LARGE", `a frame is at most ${FRAME_CAP_BYTES} bytes`);
  return text;
}

export function decodeFrame(line) {
  if (typeof line !== "string" || Buffer.byteLength(line) > FRAME_CAP_BYTES || !line.endsWith("\n")) refuse("FREEZE_FRAME_INVALID", `a frame is one newline-terminated line of at most ${FRAME_CAP_BYTES} bytes`);
  let frame;
  try { frame = JSON.parse(line); } catch { refuse("FREEZE_FRAME_INVALID", "a frame is not JSON"); }
  if (!isRecord(frame)) refuse("FREEZE_FRAME_INVALID", "a frame is not an object");
  if (frame.protocol !== CONSUME_PROTOCOL) refuse("FREEZE_FRAME_INVALID", `frame protocol must be ${CONSUME_PROTOCOL}`);
  if (frame.phase === "prepare") exactKeys(frame, ["phase", "protocol", "preparation_nonce", "operation", "native_record_id"], "prepare frame");
  else if (frame.phase === "commit") exactKeys(frame, ["phase", "protocol", "transaction_nonce", "prepared_sha256"], "commit frame");
  else refuse("FREEZE_FRAME_INVALID", `unexpected frame phase ${JSON.stringify(frame.phase)}`);
  return frame;
}

/**
 * One consume transaction, as the native owner sees the broker's pipe. The owner supplies:
 *   operation, endpoint, destination      the adapter's enrolled values (the broker rejects an effect that differs);
 *   prepare({ nativeRecordId })           resolves the authoritative record, takes its lock and returns the typed effect, performing no effect;
 *   commit(effect)                        performs the actual effect of exactly the prepared effect, and returns when it has finished.
 * `receive(line)` takes each broker frame in order and returns the line to write back. The order is prepare, then commit; anything else
 * (a second prepare, a commit first, a wrong nonce, an unknown key, a digest that is not the prepared one) throws and ends the session,
 * and a session never answers after it failed. `complete` is returned only after `commit(effect)` has returned.
 */
export function createConsumeSession({ operation, endpoint, destination, prepare, commit }) {
  if (!CONSUME_OPERATIONS.includes(operation)) refuse("FREEZE_SESSION_INVALID", "operation must be Build, Dequeue or Publish");
  if (!isBrokerText(endpoint) || !isBrokerText(destination)) refuse("FREEZE_SESSION_INVALID", "endpoint and destination must be the adapter's enrolled text");
  let step = "prepare";
  let preparationNonce = null;
  let prepared = null;
  let digest = null;
  const fail = (code, message) => { step = "failed"; throw new FreezeConsumerError(code, message); };
  return {
    get step() { return step; },
    async receive(line) {
      if (step === "failed" || step === "done") fail("FREEZE_SESSION_CLOSED", `the transaction is ${step}; no further frame is answered`);
      let frame;
      try { frame = decodeFrame(line); } catch (error) { step = "failed"; throw error; }
      if (step === "prepare") {
        if (frame.phase !== "prepare") fail("FREEZE_SESSION_ORDER", "the first frame must be prepare");
        if (frame.operation !== operation) fail("FREEZE_SESSION_OPERATION", `the broker asked for ${frame.operation}, this adapter is ${operation}`);
        if (!isBrokerText(frame.preparation_nonce) || !isBrokerText(frame.native_record_id)) fail("FREEZE_FRAME_INVALID", "prepare carries no nonce or record id");
        const effect = normalizeEffect(await prepare({ nativeRecordId: frame.native_record_id }));
        if (effect.operation !== operation || effect.native_record_id !== frame.native_record_id || effect.endpoint !== endpoint || effect.destination !== destination) {
          fail("FREEZE_SESSION_BINDING", "the prepared effect names another operation, record, endpoint or destination than the transaction");
        }
        preparationNonce = frame.preparation_nonce;
        prepared = effect;
        digest = effectDigest(effect);
        step = "commit";
        return encodeFrame({ phase: "prepared", protocol: CONSUME_PROTOCOL, preparation_nonce: preparationNonce, effect });
      }
      if (frame.phase !== "commit") fail("FREEZE_SESSION_ORDER", "the second frame must be commit");
      if (frame.prepared_sha256 !== digest) fail("FREEZE_SESSION_DIGEST", "the broker's commit digest is not the digest of the prepared effect; nothing is committed");
      if (!isBrokerText(frame.transaction_nonce)) fail("FREEZE_FRAME_INVALID", "commit carries no transaction nonce");
      step = "committing";
      try { await commit(prepared); } catch (error) { step = "failed"; throw error; }
      step = "done";
      return encodeFrame({ phase: "complete", protocol: CONSUME_PROTOCOL, transaction_nonce: frame.transaction_nonce, prepared_sha256: digest });
    },
  };
}

// ---- 3. the publication binding -------------------------------------------------------------------------------------------------
const stable = (value) => (Array.isArray(value) ? `[${value.map(stable).join(",")}]`
  : isRecord(value) ? `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stable(value[key])}`).join(",")}}`
    : JSON.stringify(value) ?? "null");

/**
 * The typed effect for publishing one sealed artifact. `release` is what verifyPublicationRelease returned (the sealed manifest, already
 * checked against its signing, notarization and ownership evidence, and `artifacts`, each read once and hash-checked against the seal);
 * `fileName` names the artifact inside it. The source is the sealed manifest's commit, so the broker authorizes the source whose
 * qualification produced these bytes, and artifact_sha256 is the digest of the bytes this process holds and will upload. Nothing here is
 * taken from the caller except the transaction's names; a caller-supplied digest or commit cannot replace the sealed one.
 */
export function buildPublishEffect({ release, fileName, repository, nativeRecordId, endpoint, destination, candidate = null }) {
  if (!isRecord(release?.manifest) || !(release.artifacts instanceof Map)) refuse("FREEZE_PUBLISH_UNVERIFIED", "the effect is built from a verified sealed release (verifyPublicationRelease), not from caller values");
  const { manifest } = release;
  const entry = (manifest.files ?? []).find((item) => item?.name === fileName);
  const artifact = release.artifacts.get(fileName);
  if (!entry || !artifact) refuse("FREEZE_PUBLISH_ARTIFACT", `${JSON.stringify(fileName)} is not an artifact of the sealed release`);
  if (artifact.sha256 !== entry.sha256 || !isDigest(artifact.sha256)) refuse("FREEZE_PUBLISH_ARTIFACT", `the verified bytes of ${fileName} do not carry the sealed digest`);
  if (!isObjectId(manifest.commit)) refuse("FREEZE_PUBLISH_SOURCE", "the sealed release names no full commit");
  // The qualification provenance: the sealed manifest's source, ownership evidence, signing policy and every sealed file's identity.
  const provenance = {
    commit: manifest.commit, app: manifest.app ?? null, version: manifest.version ?? null, platform: manifest.platform ?? null, releaseId: manifest.releaseId ?? null,
    ownershipEvidence: manifest.ownershipEvidence ?? null, configSha256: manifest.signing?.configSha256 ?? null,
    signingReceipts: (manifest.signing?.receipts ?? []).map((r) => ({ file: r.file, sha256: r.sha256 })),
    files: (manifest.files ?? []).map((f) => ({ name: f.name, sha256: f.sha256, sizeBytes: f.sizeBytes })),
  };
  const nativeEffect = { operation: "Publish", repository, nativeRecordId, source: manifest.commit, artifact: artifact.sha256, endpoint, destination };
  return normalizeEffect({
    repository, operation: "Publish", native_record_id: nativeRecordId, source_oid: manifest.commit, candidate, endpoint, destination,
    native_effect_sha256: sha256Hex(stable(nativeEffect)), provenance_sha256: sha256Hex(stable(provenance)), artifact_sha256: artifact.sha256,
  });
}
