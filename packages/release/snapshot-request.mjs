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
/** The stage evidence the broker signs in payload.stageEvidence and copies onto the receipt (attestation.rs STAGE_EVIDENCE_FIELDS). */
export const STAGE_EVIDENCE_FIELDS = Object.freeze(["freshness", "attribution", "toolchain"]);
const SIGNED_TOP_LEVEL_FIELDS = Object.freeze(["verdict", "refreshed", "exit", "signal", "timingsMs", "diagnosticsPath"]);
const TIMING_KEYS =Object.freeze(["admission", "queue", "bind", "cargo", "afterExitBind", "diagnostics"]);
/** `<toolchain>|cargo:<sha256>|rustc:<sha256>` (rustright toolchain_identity). */
const TOOLCHAIN_FORMAT = /^[^|\s][^|]*\|cargo:[0-9a-f]{64}\|rustc:[0-9a-f]{64}$/;

/** The compile-class cargo commands (and their aliases): the ones whose receipt carries attribution and toolchain. */
export const COMPILE_CLASS_COMMANDS = Object.freeze(["check", "clippy", "build", "doc", "rustc", "test", "b", "c", "t", "d"]);
const GLOBAL_VALUE_OPTIONS = new Set(["--color", "--explain"]);

/** The cargo subcommand of an argv (without `cargo`): after an optional `+toolchain` and the global options, before `--`; null when there is none. */
export function cargoSubcommandOf(argv) {
  const words = [...argv];
  for (let i = words[0]?.startsWith("+") ? 1 : 0; i < words.length; i += 1) {
    const w = words[i];
    if (w === "--") return null;
    if (GLOBAL_VALUE_OPTIONS.has(w)) { i += 1; continue; }
    if (w.startsWith("-")) continue;
    return w;
  }
  return null;
}
export const isCompileClass = (argv) => COMPILE_CLASS_COMMANDS.includes(cargoSubcommandOf(argv));

/** Where the gate's declared compile inputs are listed, as read from the snapshot (GATE-CONTRACT "Request"; the broker's COMPILE_INPUTS). */
export const COMPILE_INPUTS = ".rightkit/compile-inputs.json";
export const MAX_DECLARATION_BYTES = 1024 * 1024;
const MAX_INPUT_FILES = 200_000;
const MAX_INPUT_BYTES = 4 * 1024 * 1024 * 1024;

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

// ---- declared compile inputs, read and digested the way the broker does (check_admission.rs declared_inputs / snapshot_inputs) ----

const lstatOrNull = (p) => { try { return fs.lstatSync(p); } catch (e) { if (e.code === "ENOENT") return null; throw e; } };

/**
 * The declared inputs of a snapshot: `inputs` of `.rightkit/compile-inputs.json` read FROM THE SNAPSHOT, never the checkout. No
 * manifest means an empty set; a link on its path, a file that is not regular, an oversized or malformed manifest, or an input that
 * is not a plain relative path (absolute, `.`/`..`, inside `.git`, the manifest itself) is refused before any request is made.
 */
export function readDeclaredInputs(snapshotRoot) {
  const parts = COMPILE_INPUTS.split("/");
  let at = snapshotRoot; let meta = null;
  for (const part of parts) {
    at = path.join(at, part);
    meta = lstatOrNull(at);
    if (meta === null) return [];
    if (meta.isSymbolicLink()) fail("declared-inputs", `${COMPILE_INPUTS} runs through a link at ${at}`);
  }
  if (!meta.isFile()) fail("declared-inputs", `${COMPILE_INPUTS} is not a regular file`);
  if (meta.size > MAX_DECLARATION_BYTES) fail("declared-inputs", `${COMPILE_INPUTS} is larger than ${MAX_DECLARATION_BYTES} bytes`);
  let value;
  try { value = JSON.parse(fs.readFileSync(at, "utf8")); } catch (e) { fail("declared-inputs", `${COMPILE_INPUTS} is not JSON (${e.message})`); }
  if (!value || typeof value !== "object" || !Array.isArray(value.inputs)) fail("declared-inputs", `${COMPILE_INPUTS}: \`inputs\` must be a list of paths`);
  for (const input of value.inputs) {
    if (typeof input !== "string" || !input) fail("declared-inputs", `${COMPILE_INPUTS}: every input is a non-empty relative path`);
    const components = input.split(/[\\/]/);
    if (path.isAbsolute(input) || path.posix.isAbsolute(input) || path.win32.isAbsolute(input) || components.some((c) => c === "" || c === "." || c === "..")) fail("declared-inputs", `${COMPILE_INPUTS}: ${input} is not a plain relative path inside the snapshot`);
    if (components.some((c) => c.toLowerCase() === ".git")) fail("declared-inputs", `${COMPILE_INPUTS}: ${input} is inside .git; repository metadata is never a compile input`);
    if (components.length <= parts.length && components.every((c, i) => c === parts[i])) fail("declared-inputs", `${COMPILE_INPUTS}: ${input} overlaps the declaration file itself`);
  }
  return [...value.inputs];
}

/**
 * The `declaredInputDigests` the broker records for these inputs, computed here from the snapshot: per input, sha256 over, for each
 * regular file in directory order (names sorted by their bytes), the file's relative name, a NUL, its bytes and a NUL. A link
 * anywhere, a special file, or a limit breach is a refusal. Child names are joined with the platform's separator, as the broker's
 * `Path::join` does.
 */
export function declaredInputDigestsOf(snapshotRoot, inputs, { platform = process.platform } = {}) {
  const sep = platform === "win32" ? "\\" : "/";
  let files = 0; let bytes = 0;
  const walk = (full, name, hasher) => {
    const meta = fs.lstatSync(full);
    if (meta.isSymbolicLink()) fail("declared-inputs", `declared compile input holds a link at ${name}`);
    if (meta.isDirectory()) {
      const children = fs.readdirSync(full).sort((a, b) => Buffer.compare(Buffer.from(a, "utf8"), Buffer.from(b, "utf8")));
      for (const child of children) walk(path.join(full, child), name.endsWith("/") || name.endsWith("\\") ? `${name}${child}` : `${name}${sep}${child}`, hasher);
      return;
    }
    if (!meta.isFile()) fail("declared-inputs", `declared compile input ${name} is not a regular file or directory`);
    files += 1;
    if (files > MAX_INPUT_FILES) fail("declared-inputs", `the declared compile inputs exceed ${MAX_INPUT_FILES} files`);
    hasher.update(name); hasher.update(Buffer.from([0]));
    const fd = fs.openSync(full, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
    try {
      const after = fs.fstatSync(fd);
      if (!after.isFile() || (process.platform !== "win32" && (after.dev !== meta.dev || after.ino !== meta.ino))) fail("declared-inputs", `${name} changed while it was read`);
      const chunk = Buffer.allocUnsafe(64 * 1024);
      for (;;) {
        const n = fs.readSync(fd, chunk, 0, chunk.length, null);
        if (n === 0) break;
        bytes += n;
        if (bytes > MAX_INPUT_BYTES) fail("declared-inputs", `the declared compile inputs exceed ${MAX_INPUT_BYTES} bytes`);
        hasher.update(chunk.subarray(0, n));
      }
    } finally { fs.closeSync(fd); }
    hasher.update(Buffer.from([0]));
  };
  return inputs.map((input) => {
    // Every component of the declared path is walked without following a link.
    let at = snapshotRoot;
    for (const part of input.split(/[\\/]/)) {
      at = path.join(at, part);
      let meta;
      try { meta = fs.lstatSync(at); } catch (e) { fail("declared-inputs", `${input}: ${e.message}`); }
      if (meta.isSymbolicLink()) fail("declared-inputs", `${input} runs through a link at ${at}`);
    }
    const hasher = crypto.createHash("sha256");
    walk(at, input, hasher);
    return { path: input, sha256: hasher.digest("hex") };
  });
}

// ---- the receipt ----

const inside = (child, parent, platform) => {
  const norm = (p) => (platform === "win32" ? path.win32.resolve(p).toLowerCase() : path.resolve(p));
  const rel = (platform === "win32" ? path.win32 : path).relative(norm(parent), norm(child));
  return rel === "" || !(rel === ".." || /^\.\.[\\/]/.test(rel) || path.isAbsolute(rel) || path.win32.isAbsolute(rel));
};

/**
 * The generation target the broker pins an app's outputs to: `<root>/<12 hex>/g/<NNN>/target` under one of the managed `ws` roots
 * (`<external data root>/rightkit-build-control/ws`; BROKER-CONTRACT "Source identity", output pinning). The 12-hex key is
 * rustright-state's workspace_key, which the gate never computes (the snapshot's own key is `snapshots/<sha256(repo path)[:16]>`, a
 * different thing); it reads the receipt's outputDir and checks only this shape. The generation number (3+ digits) is the broker's.
 */
export function isGenerationTarget(outputDir, outputRoots, platform = process.platform) {
  const mod = platform === "win32" ? path.win32 : path;
  for (const root of outputRoots) {
    const rel = mod.relative(platform === "win32" ? mod.resolve(root).toLowerCase() : mod.resolve(root), platform === "win32" ? mod.resolve(outputDir).toLowerCase() : mod.resolve(outputDir));
    const segments = rel.split(/[\\/]/);
    if (segments.length === 4 && segments[0] !== ".." && !mod.isAbsolute(rel) && /^[0-9a-f]{12}$/.test(segments[0]) && segments[1] === "g" && /^[0-9]{3,}$/.test(segments[2]) && segments[3] === "target") return true;
  }
  return false;
}

const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

/**
 * Verifies the receipt of one snapshot cargo request. `pins` come from this host's enrollment, never from the receipt.
 * Returns `{ payload, evidence }`; throws ProvenanceError (legacy, signature, pins, job, nonce, request) or SnapshotRequestError (evidence).
 *
 * Beyond the signature, the job, the nonce and the request digest, the receipt must carry (item 7 row 7.22):
 *   - signed SUCCEEDED with exit 0, and signed snapshotEvidence equal to the receipt's copies (commit, ownership handle, request id,
 *     snapshot path, `treeAtStart` = `treeAtExit` = the expected tree);
 *   - `declaredInputDigests` equal to the gate's own digest of the snapshot's declared inputs, read from the snapshot now;
 *   - `outputDir` absolute, outside the snapshot and the checkout, and a managed generation target under one of `outputRoots`;
 *   - signed stageEvidence (freshness, attribution, toolchain) equal to the receipt's;
 *   - verdict, timingsMs, exit, diagnosticsPath, brokerVersion, executor and refreshed present with their types.
 * `checkout` and `outputRoots` are required: without them nothing can be said about where the outputs went, so it refuses.
 */
export function verifySnapshotReceipt(receipt, { pins, jobId, carrier, argv, workspace, observedEnv = {}, checkout, outputRoots, platform = process.platform, canonicalPath = (p) => fs.realpathSync.native(p), digestInputs = declaredInputDigestsOf, readInputs = readDeclaredInputs }) {
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
    fail("evidence", `the snapshot was tree ${snap.treeAtStart} at START and ${snap.treeAtExit} at exit, not ${carrier.snapshot.expectedTree} throughout: the build changed a tracked file (or added or removed one) in the commit's snapshot, so the result is discarded; that is the app owner's defect to fix (write generated output to an ignored path or an output directory, or generate before commit)`);
  }
  // Declared inputs: what the broker digested in place must be what the gate digests in the snapshot, from the same manifest.
  let gateInputs; let gateDigests;
  try {
    gateInputs = readInputs(carrier.snapshot.path);
    gateDigests = digestInputs(carrier.snapshot.path, gateInputs, { platform });
  } catch (e) { fail("evidence", `the gate cannot digest the snapshot's declared inputs (${e.message})`); }
  if (!same(gateInputs, carrier.declaredInputs?.inputs)) fail("evidence", "the snapshot's declared inputs are not the ones this request carried");
  if (!same(evidence.declaredInputDigests, gateDigests)) fail("evidence", "the signed declaredInputDigests are not the gate's own digest of the snapshot's declared inputs");
  // Output directory: where the outputs went must be the broker's managed generation target, never under the snapshot or the checkout.
  const out = evidence.outputDir;
  if (typeof out !== "string" || !path.isAbsolute(out) && !path.win32.isAbsolute(out)) fail("evidence", "the signed evidence names no absolute output directory");
  if (!checkout || !path.isAbsolute(checkout) || !Array.isArray(outputRoots) || outputRoots.length === 0) fail("evidence", "the gate has no checkout or managed output root to judge the output directory against");
  const canon = (p) => { try { return canonicalPath(p); } catch { return p; } };
  if (inside(out, carrier.snapshot.path, platform) || inside(out, canon(carrier.snapshot.path), platform)) fail("evidence", `the output directory ${out} is inside the snapshot`);
  if (inside(out, checkout, platform) || inside(out, canon(checkout), platform)) fail("evidence", `the output directory ${out} is inside the checkout`);
  if (!isGenerationTarget(out, outputRoots, platform)) fail("evidence", `the output directory ${out} is not a managed generation target (<ws root>/<repo-key>/g/<generation>/target) under ${outputRoots.join(", ")}`);
  // Stage evidence is signed with the rest and must equal the receipt's copy. `freshness` is required for every carried command;
  // `attribution` and `toolchain` only for a compile-class command (check, clippy, build, doc, rustc, test: the broker emits them
  // for compiles only), so metadata, fetch --locked, fmt --check or tree --locked pass without them. When either side carries one
  // anyway, both must carry the same value.
  const stage = payload.stageEvidence;
  if (!isObject(stage)) fail("evidence", "the signed payload carries no stageEvidence");
  const compile = isCompileClass(argv);
  for (const name of STAGE_EVIDENCE_FIELDS) {
    const signed = stage[name] ?? null; const copy = receipt[name] ?? null;
    const required = name === "freshness" || compile;
    if (required && (signed === null || copy === null)) fail("evidence", `${name} is missing from the signed stage evidence or the receipt${name === "freshness" ? "" : ` (a compile-class command, cargo ${cargoSubcommandOf(argv)}, carries it)`}`);
    if (!same(copy, signed)) fail("evidence", `receipt ${name} differs from the signed stage evidence`);
  }
  // Shapes RustRight emits (item 4 row 4.18): attribution is `{available, ...}`, and an unavailable one names its reason
  // (`cargo-json-message-format-not-requested` without a JSON message stream; a consumer that needs per-package attribution treats
  // `available:false` as FAILED, not partial); toolchain is `<toolchain>|cargo:<sha256>|rustc:<sha256>`.
  const attribution = receipt.attribution ?? null; const toolchain = receipt.toolchain ?? null;
  if (attribution !== null) {
    if (!isObject(attribution) || typeof attribution.available !== "boolean") fail("evidence", "receipt attribution is not {available, ...}");
    if (attribution.available === false && !(typeof attribution.reason === "string" && attribution.reason !== "")) fail("evidence", "receipt attribution is unavailable and names no reason");
  }
  if (toolchain !== null && !(typeof toolchain === "string" && TOOLCHAIN_FORMAT.test(toolchain))) fail("evidence", "receipt toolchain is not <toolchain>|cargo:<sha256>|rustc:<sha256>");
  // Fields the signature does not cover must still be present with their types (the consumer reads them). Every carried command:
  // verdict, refreshed, exit (an integer, or null with a signal) and signal, timingsMs{admission, queue, bind, cargo, afterExitBind,
  // diagnostics} (each a number or null) and diagnosticsPath (a path, or null). Success needs exit 0 and no signal, as signed.
  const timingsOk = isObject(receipt.timingsMs) && TIMING_KEYS.every((k) => Object.hasOwn(receipt.timingsMs, k) && (receipt.timingsMs[k] === null || Number.isFinite(receipt.timingsMs[k])));
  const typed = [
    ["verdict", isObject(receipt.verdict)], ["timingsMs", timingsOk], ["exit", receipt.exit === null || Number.isSafeInteger(receipt.exit)],
    ["signal", receipt.signal === null || typeof receipt.signal === "string" || Number.isSafeInteger(receipt.signal)],
    ["diagnosticsPath", receipt.diagnosticsPath === null || (typeof receipt.diagnosticsPath === "string" && receipt.diagnosticsPath !== "")], ["brokerVersion", typeof receipt.brokerVersion === "string" && receipt.brokerVersion !== ""],
    ["executor", typeof receipt.executor === "string" && receipt.executor !== ""], ["refreshed", Array.isArray(receipt.refreshed)],
  ];
  for (const [name, ok] of typed) if (!ok) fail("evidence", `receipt ${name} is missing or has the wrong type`);
  if (receipt.exit !== 0 || receipt.signal !== null) fail("evidence", `receipt exit ${receipt.exit} and signal ${receipt.signal} are not a success (exit 0, no signal)`);
  // RustRight signs these top-level fields in the payload (item 4 row 4.18): when the signed payload carries one, the receipt's copy must equal it.
  for (const name of SIGNED_TOP_LEVEL_FIELDS) {
    if (payload[name] !== undefined && !same(receipt[name], payload[name])) fail("evidence", `receipt ${name} differs from the signed payload`);
  }
  return { payload, evidence, attribution, toolchain };
}

export { ProvenanceError };
