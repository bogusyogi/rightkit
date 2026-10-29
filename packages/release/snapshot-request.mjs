// The gate's half of a broker snapshot build (joint plan step 4d; GATE-CONTRACT "Broker interface"; the reader is RustRight's
// rustright-cli shims.rs `snapshot_carrier`, the verifier rustright-protocol attestation.rs `verify_receipt`).
//
// For each `rustright cargo` request that builds the commit snapshot on a broker host the gate:
//   1. picks a fresh nonce (RIGHTKIT_ATTEST_NONCE: an attested request; the carrier must carry the same nonce);
//   2. writes the carrier `rightkit.snapshot-request/1` as an owner-only file under `<control root>/snapshot-requests/` (the
//      directory the client accepts, never a link) and points RIGHTKIT_SNAPSHOT_REQUEST at it as `{"path","sha256"}` (at most
//      4096 bytes; the file at most 4 MiB). The carrier binds the commit and tree, the snapshot and its expected tree, the
//      declared inputs, the build-ownership handle, the cargo working directory (primaryCwd) and sha256 of the canonical cargo
//      argv (without `cargo`), so it cannot be replayed onto another request;
//   3. after the run, verifies the signed receipt: the provenance signature against this host's enrolled pins (never from the
//      receipt), the job id, the nonce, the signed result (SUCCEEDED, exit 0), the request digest it recomputes over the EXTENDED core (the base core plus commit,
//      snapshot, declaredInputs and ownership), and the signed snapshotEvidence: the receipt's copies equal the signed ones,
//      the commit and handle are the carrier's, and the snapshot held the expected tree at START and at exit.
// The carrier is set only on that one cargo invocation and is removed after it; nothing the gate exports carries it.

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { canonicalJson, ProvenanceError, verifyProvenance } from "./provenance-verify.mjs";

export const SNAPSHOT_REQUEST_SCHEMA = "rightkit.snapshot-request/1";
export const SNAPSHOT_REQUEST_ENV = "RIGHTKIT_SNAPSHOT_REQUEST";
export const ATTEST_NONCE_ENV = "RIGHTKIT_ATTEST_NONCE";
export const REQUEST_ID_FILE_ENV = "RIGHTKIT_REQUEST_ID_FILE";
export const REQUEST_REF_SCHEMA = "rustright.request-ref.v1";
export const DESCRIPTOR_MAX_BYTES = 4096;
export const CARRIER_MAX_BYTES = 4 * 1024 * 1024;
/** rustright-cli shims.rs MANAGED_RUST_ENVIRONMENT_KEYS: what the client records as `observedEnv` for a cargo request. */
export const MANAGED_RUST_ENVIRONMENT_KEYS = Object.freeze([
  "CARGO_HOME", "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET", "CARGO_BUILD_TARGET_DIR", "CARGO_BUILD_BUILD_DIR", "CARGO_BUILD_RUSTC",
  "CARGO_BUILD_RUSTDOC", "CARGO_BUILD_RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_RUSTFLAGS",
  "CARGO_ENCODED_RUSTFLAGS", "CARGO_INSTALL_ROOT", "RUSTUP_HOME", "RUSTC", "RUSTDOC", "RUSTFLAGS", "RUSTDOCFLAGS", "RUSTC_WRAPPER",
  "RUSTC_WORKSPACE_WRAPPER",
]);
/** The fields the broker copies onto the receipt and signs in payload.snapshotEvidence (attestation.rs). */
export const SNAPSHOT_EVIDENCE_FIELDS = Object.freeze(["requestId", "brokerVersion", "executor", "commit", "snapshot", "declaredInputDigests", "ownership", "outputDir"]);

export class SnapshotRequestError extends Error {
  constructor(code, message) { super(message); this.name = "SnapshotRequestError"; this.code = code; }
}
const fail = (code, message) => { throw new SnapshotRequestError(code, message); };
const sha256 = (data) => crypto.createHash("sha256").update(data).digest("hex");
const objectId = (s) => typeof s === "string" && /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/.test(s);

/** A fresh attestation nonce: 64 lowercase hex characters. */
export const freshNonce = () => crypto.randomBytes(32).toString("hex");

/** sha256 of the canonical JSON of the cargo argv (without `cargo`), as the client computes it. */
export const argvSha256 = (argv) => sha256(canonicalJson([...argv]));

/**
 * The managed Rust variables the client will report as observedEnv, from the environment the gate runs it with. RustRight's
 * capture_managed_rust_environment (shims.rs) matches the name case-insensitively on every platform and keeps the key exactly as
 * spelled (`Cargo_Home` stays `Cargo_Home`), so the digest binds that spelling. On Windows the child sees only one of several
 * names that differ by case: Node passes the first in sorted order (child_process "env" on Windows), so the rest are not observed.
 */
export function observedEnvOf(env, platform = process.platform) {
  const out = {};
  const entries = Object.entries(env ?? {});
  if (platform === "win32") entries.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  const seen = new Set();
  for (const [key, value] of entries) {
    const upper = key.toUpperCase();
    if (platform === "win32") { if (seen.has(upper)) continue; seen.add(upper); }
    if (MANAGED_RUST_ENVIRONMENT_KEYS.includes(upper) && typeof value === "string") out[key] = value;
  }
  return out;
}

/**
 * Writes the carrier for one cargo request and returns `{ file, sha256, descriptor, carrier, env }`; `env` is what that one
 * invocation adds (carrier descriptor, nonce, request-id file). Refuses before writing anything the reader would refuse.
 */
export function writeSnapshotRequest({ controlRoot, commit, snapshot, declaredInputs = [], handle, primaryCwd, argv, nonce = freshNonce(), requestIdFile, uuid = crypto.randomUUID }) {
  if (!objectId(commit?.sha) || !objectId(commit?.tree)) fail("invalid", "commit sha and tree must be full lowercase object ids");
  if (!snapshot?.path || !path.isAbsolute(snapshot.path) || snapshot.expectedTree !== commit.tree) fail("invalid", "the snapshot needs an absolute path and the commit's tree as expectedTree");
  if (typeof handle !== "string" || !handle.trim()) fail("invalid", "the build-ownership handle is missing");
  if (!primaryCwd || !path.isAbsolute(primaryCwd)) fail("invalid", "primaryCwd must be the cargo request's absolute working directory");
  if (!Array.isArray(argv) || !argv.every((a) => typeof a === "string")) fail("invalid", "argv must be the cargo arguments (without cargo)");
  if (typeof nonce !== "string" || !/^[0-9a-f]{64,128}$/.test(nonce)) fail("invalid", "the nonce must be 64 to 128 lowercase hex characters");
  if (!Array.isArray(declaredInputs) || declaredInputs.some((p) => typeof p !== "string" || !p || path.isAbsolute(p) || path.posix.isAbsolute(p))) fail("invalid", "declared inputs must be relative paths");
  if (!controlRoot || !path.isAbsolute(controlRoot)) fail("invalid", "the control root must be absolute");
  const dir = path.join(controlRoot, "snapshot-requests");
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  const meta = fs.lstatSync(dir);
  if (meta.isSymbolicLink() || !meta.isDirectory()) fail("invalid", `${dir} is not a real directory`);
  const carrier = {
    schema: SNAPSHOT_REQUEST_SCHEMA,
    commit: { sha: commit.sha, tree: commit.tree },
    snapshot: { path: snapshot.path, expectedTree: snapshot.expectedTree },
    declaredInputs: { inputs: [...declaredInputs] },
    ownership: { handle },
    primaryCwd,
    argvSha256: argvSha256(argv),
    nonce,
  };
  const bytes = Buffer.from(JSON.stringify(carrier), "utf8");
  if (bytes.length > CARRIER_MAX_BYTES) fail("too-large", `the snapshot request is ${bytes.length} bytes, more than ${CARRIER_MAX_BYTES}`);
  const file = path.join(dir, `${uuid()}.json`);
  fs.writeFileSync(file, bytes, { flag: "wx", mode: 0o600 });
  const digest = sha256(bytes);
  const descriptor = JSON.stringify({ path: file, sha256: digest });
  if (Buffer.byteLength(descriptor) > DESCRIPTOR_MAX_BYTES) { fs.rmSync(file, { force: true }); fail("too-large", "the snapshot request descriptor exceeds 4096 bytes"); }
  const env = { [SNAPSHOT_REQUEST_ENV]: descriptor, [ATTEST_NONCE_ENV]: nonce, ...(requestIdFile ? { [REQUEST_ID_FILE_ENV]: requestIdFile } : {}) };
  return { file, sha256: digest, descriptor, carrier, env };
}

/** The request core the broker digests for a snapshot cargo request: the base core plus commit, snapshot, declaredInputs and ownership. */
export function snapshotRequestCore({ argv, workspace, observedEnv = {}, carrier }) {
  return {
    adapter: null, env: {}, inputManifest: null, nonce: carrier.nonce, observedEnv, outputs: [], requestedArgv: [...argv], shim: "cargo", workspace,
    commit: carrier.commit, snapshot: carrier.snapshot, declaredInputs: carrier.declaredInputs, ownership: carrier.ownership,
  };
}
export const requestDigestOf = (core) => sha256(canonicalJson(core));

/** The request reference the client writes at submit (RIGHTKIT_REQUEST_ID_FILE), or a throw. */
export function readRequestRef(file) {
  let ref;
  try { ref = JSON.parse(fs.readFileSync(file, "utf8")); } catch (e) { fail("request", `no request reference at ${file} (${e.message})`); }
  if (ref?.schema !== REQUEST_REF_SCHEMA || typeof ref.requestId !== "string" || !ref.requestId || typeof ref.workspace !== "string" || !path.isAbsolute(ref.workspace)) fail("request", `${file} is not a ${REQUEST_REF_SCHEMA}`);
  return ref;
}

const same = (a, b) => canonicalJson(a ?? null) === canonicalJson(b ?? null);

/**
 * Verifies the receipt of one snapshot cargo request. `pins` come from this host's enrollment, never from the receipt.
 * Returns `{ payload, evidence }`; throws ProvenanceError (signature, pins, job, nonce, request) or SnapshotRequestError (evidence).
 */
export function verifySnapshotReceipt(receipt, { pins, jobId, carrier, argv, workspace, observedEnv = {}, canonicalPath = (p) => fs.realpathSync.native(p) }) {
  const requestDigest = requestDigestOf(snapshotRequestCore({ argv, workspace, observedEnv, carrier }));
  const payload = verifyProvenance(receipt, pins, { jobId, nonce: carrier.nonce, requestDigest });
  // The signed outcome, not the client's exit code: a passing row needs the broker to have signed SUCCEEDED with exit 0 and no
  // signal (service.rs provenance_payload `result`). An authentic FAILED, or a payload without a result, is not a success.
  const result = payload.result;
  if (!result || typeof result !== "object") fail("result", "the signed payload carries no result: the broker did not sign this request's outcome");
  if (result.status !== "SUCCEEDED" || result.exitCode !== 0 || (result.signal !== undefined && result.signal !== null)) {
    fail("result", `the broker signed ${result.status ?? "no status"} (exit ${result.exitCode ?? "none"}${result.signal ? `, signal ${result.signal}` : ""}), not SUCCEEDED with exit 0`);
  }
  const evidence = payload.snapshotEvidence;
  if (!evidence || typeof evidence !== "object") fail("evidence", "the signed payload carries no snapshotEvidence: the broker did not build this as a snapshot request");
  for (const name of SNAPSHOT_EVIDENCE_FIELDS) {
    if (evidence[name] === undefined || !same(receipt[name], evidence[name])) fail("evidence", `receipt ${name} differs from the signed snapshot evidence`);
  }
  if (!same(evidence.commit, carrier.commit)) fail("evidence", "the signed commit is not the one this request carried");
  if (evidence.ownership?.handle !== carrier.ownership.handle) fail("evidence", "the signed ownership handle is not this gate's");
  if (evidence.requestId !== jobId) fail("evidence", "the signed request id is not this job");
  const snap = evidence.snapshot ?? {};
  let samePath = false;
  try { samePath = canonicalPath(snap.path) === canonicalPath(carrier.snapshot.path); } catch { samePath = false; }
  if (!samePath) fail("evidence", `the signed snapshot ${snap.path} is not ${carrier.snapshot.path}`);
  if (snap.treeAtStart !== carrier.snapshot.expectedTree || snap.treeAtExit !== carrier.snapshot.expectedTree) {
    fail("evidence", `the snapshot was tree ${snap.treeAtStart} at START and ${snap.treeAtExit} at exit, not ${carrier.snapshot.expectedTree} throughout`);
  }
  if (typeof evidence.outputDir !== "string" || !path.isAbsolute(evidence.outputDir)) fail("evidence", "the signed evidence names no absolute output directory");
  return { payload, evidence };
}

export { ProvenanceError };
