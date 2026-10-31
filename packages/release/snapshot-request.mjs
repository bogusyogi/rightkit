// The gate's half of a broker snapshot build (joint plan step 4d; GATE-CONTRACT "Broker interface"; the reader is RustRight's
// rustright-cli shims.rs `snapshot_carrier`, the verifier rustright-protocol attestation.rs `verify_receipt`).
//
// For each `rustright cargo` request that builds the commit snapshot on a broker host the gate:
//   1. picks a fresh nonce (RUSTRIGHT_ATTEST_NONCE: an attested request; the carrier must carry the same nonce);
//   2. writes the carrier `rustright.snapshot-request/1` as an owner-only file under `<control root>/snapshot-requests/` (the
//      directory the client accepts, never a link) and points RUSTRIGHT_SNAPSHOT_REQUEST at it as `{"path","sha256"}` (at most
//      4096 bytes; the file at most 4 MiB). The carrier binds the commit and tree, the snapshot and its expected tree, the
//      declared inputs, the build-ownership handle, the cargo working directory (primaryCwd) and sha256 of the canonical cargo
//      argv (without `cargo`), so it cannot be replayed onto another request;
//   3. after the run, verifies the signed receipt: the provenance signature against this host's enrolled pins (never from the
//      receipt), the job id, the nonce, the signed result (SUCCEEDED, exit 0), the request digest it recomputes over the EXTENDED core (the base core plus commit,
//      snapshot, declaredInputs and ownership), and the signed snapshotEvidence: the receipt's copies equal the signed ones,
//      the commit and handle are the carrier's, and the snapshot held the expected tree at START and at exit.
// The carrier is set only on that one cargo invocation and is removed after it; nothing the gate exports carries it.
// A pnpm, node or QA step of a snapshot stage is carried the same way as an ADAPTER request (`rustright run <adapter> -- <args>`,
// rustright#190: snapshot-adapter-v1): the same carrier, the adapter's name in the request core, and a signed `nestedChildren` list (in snapshotEvidence, mirrored on the receipt) of the
// compiles that job's own children made (the Tauri CLI's Cargo calls), which the gate verifies as it does the call (rightkit#1301).

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { REQUEST_OVER_CAP_CODE, requestOverCap } from "./broker-wire.mjs";
import { ownerFileOf, writeCarrierOwner } from "./carrier-census.mjs";
import { BROKER_COMPILE_COMMANDS, CARGO_COMMAND_ALIASES, cargoCommandIndex, cargoSubcommandOf, expandCargoAliases, isCompileCommand, isPackageObservation } from "./cargo-shapes.mjs";
import { canonicalJson, CAUSE_EVIDENCE_SCHEMA, CAUSE_UNEXPLAINED, CAUSE_UNKNOWN, ProvenanceError, requireExplainedCauses, verifyBr19Evidence, verifyCauseEvidence, verifyProvenance, verifyReceiptCoverage, verifyResultEvidence } from "./provenance-verify.mjs";

export const SNAPSHOT_REQUEST_SCHEMA = "rustright.snapshot-request/1";
// One spelling per variable, the current one (rustright-cli shims.rs reads RUSTRIGHT_* first, then the legacy RIGHTKIT_* spelling, which task
// L-1 removes): the gate never sets both.
export const SNAPSHOT_REQUEST_ENV = "RUSTRIGHT_SNAPSHOT_REQUEST";
export const ATTEST_NONCE_ENV = "RUSTRIGHT_ATTEST_NONCE";
export const REQUEST_ID_FILE_ENV = "RUSTRIGHT_REQUEST_ID_FILE";
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
/** The result fields the broker signs in payload.resultEvidence and mirrors on the receipt (attestation.rs RESULT_EVIDENCE_FIELDS). */
export const RESULT_EVIDENCE_FIELDS = Object.freeze(["exit", "signal", "timingsMs", "diagnosticsPath"]);
const TIMING_KEYS =Object.freeze(["admission", "queue", "bind", "cargo", "afterExitBind", "diagnostics"]);
/** The broker's code for a compile whose toolchain identity cannot be built (checked_toolchain_identity), and the gate's typed error code for it. */
export const TOOLCHAIN_UNAVAILABLE_CODE = "RK_TOOLCHAIN_IDENTITY_UNAVAILABLE";
export const TOOLCHAIN_UNAVAILABLE = "toolchain-unavailable";
/** `{"state":"UNKNOWN","reason":<non-empty string>}` exactly: the one spelling of an identity the broker could not establish (BR19). */
export const isToolchainUnknown = (v) => v !== null && typeof v === "object" && !Array.isArray(v) && Object.keys(v).length === 2 && v.state === "UNKNOWN" && typeof v.reason === "string" && v.reason !== "";
/** `<toolchain>|cargo:<sha256>|rustc:<sha256>` (rustright toolchain_identity). */
const TOOLCHAIN_FORMAT = /^[^|\s][^|]*\|cargo:[0-9a-f]{64}\|rustc:[0-9a-f]{64}$/;

// GB-01/GB-02: the command-shape table is cargo-shapes.mjs (one versioned table, exercised against RustRight's cargo-shapes vector); these names
// are its views for the carried call. `isCompileClass` decides which evidence a receipt must carry from the broker's own compile rule (the `compile`
// column of the vector), so bench, run, rustdoc and a compiling `package` owe it too and a read-only metadata or `package --list` legitimately carries none.
export { BROKER_COMPILE_COMMANDS, CARGO_COMMAND_ALIASES, cargoSubcommandOf, isPackageObservation };
/** Whether the broker classifies this argv (without the tool name) as a compile. `package` is one unless it is the observation listing. */
export const isCompileClass = (argv) => isCompileCommand(argv);
/** The commands that build or run, and so can rewrite Cargo.lock inside a snapshot unless they are locked (rightkit#1301): the same table as `isCompileClass`. */
export const isLockBound = (argv) => isCompileClass(argv);

/**
 * The argv a carried call sends: a build or run command with neither `--locked` nor `--frozen` (before any `--`) gets `--locked` right
 * after its subcommand. RustRight refuses an unlocked grant-minted build, and an unlocked build could rewrite Cargo.lock in the snapshot.
 * Adding it, not refusing, is the less surprising rule: the call runs as the caller meant it, except that a stale lock fails instead of
 * being rewritten. The ledger and the signed request digest both carry the argv as sent.
 */
/** cargo-ndk's own options that take a value (outer grammar): `cargo ndk [-t <abi>] [-p|--platform <api>] [-o <dir>] ... <cargo subcommand> [cargo args]`. */
const NDK_VALUE_OPTIONS = new Set(["-t", "--target", "-o", "--output-dir", "-p", "--platform"]);
const NDK_INNER_LOCKED = new Set(["build", "b", "check", "c", "test", "t", "bench", "clippy", "doc", "rustc", "rustdoc", "run", "r"]);

/**
 * GB-62: `cargo ndk <outer options> build <cargo args>` is two grammars. The immutable-dependency flag belongs to the INNER Cargo command, so it is
 * inserted right after that inner subcommand (never after `ndk`, where cargo-ndk would read it as its own option, and never before it). Returns the argv
 * unchanged when the inner command is not a locked-class command, already carries `--locked`/`--frozen` before `--`, or cannot be found.
 */
function lockedNdkArgv(words) {
  const end = words.indexOf("--");
  const own = end < 0 ? words : words.slice(0, end);
  let i = words.findIndex((w) => w === "ndk");
  if (i < 0) return words;
  for (i += 1; i < own.length; i += 1) {
    const w = own[i];
    if (NDK_VALUE_OPTIONS.has(w)) { i += 1; continue; }
    if (w.startsWith("-")) continue;
    break;
  }
  if (i >= own.length || !NDK_INNER_LOCKED.has(own[i])) return words;
  if (own.slice(i + 1).some((w) => w === "--locked" || w === "--frozen")) return words;
  return [...words.slice(0, i + 1), "--locked", ...words.slice(i + 1)];
}

export function lockedArgv(argv) {
  // GB-01: the standard aliases are expanded BEFORE locking and hashing, so what the gate sends, signs and digests is what the broker classifies
  // (a committed `.cargo/config.toml` [alias] can never redefine it). Only the subcommand token changes.
  const words = expandCargoAliases(argv);
  if (cargoSubcommandOf(words) === "ndk") return lockedNdkArgv(words);
  if (!isLockBound(words)) return words;
  const end = words.indexOf("--");
  if ((end < 0 ? words : words.slice(0, end)).some((w) => w === "--locked" || w === "--frozen")) return words;
  const i = cargoCommandIndex(words);
  if (i === null) return words;
  return [...words.slice(0, i + 1), "--locked", ...words.slice(i + 1)];
}

const CARRIED_REDIRECT = /^(--config|-Z|-C)(=|$)|^-[A-Za-z]*[CZ]/;

/**
 * The one place that turns a caller's cargo arguments (without `cargo`) into the arguments a carried call sends (carried-cargo.mjs
 * `runCarriedCargo`; stage-commands.mjs derives what a stage's signed receipts must show from the same function, so the two cannot drift):
 * `--manifest-path <p>` or `--manifest-path=<p>` before any `--` is removed (the client refuses to rewrite an attested call; the call
 * runs from the manifest's directory instead, the last one wins) and returned as `manifestPath`; `--target-dir` and the redirecting
 * options are refused. Returns `{ words, manifestPath }` (`manifestPath` null when none) or `{ refusal: { code, message } }`.
 * `words` are not yet locked: the signed request carries `lockedArgv(words)` (carryBrokerCargo).
 */
export function splitCarriedArgs(args) {
  const original = [...args];
  const words = [];
  let manifestPath = null;
  let atSeparator = false;
  for (let i = 0; i < original.length; i += 1) {
    const w = original[i];
    if (atSeparator) { words.push(w); continue; }
    if (w === "--") { atSeparator = true; words.push(w); continue; }
    if (w === "--manifest-path" || w.startsWith("--manifest-path=")) {
      const value = w === "--manifest-path" ? original[i + 1] : w.slice("--manifest-path=".length);
      if (w === "--manifest-path") i += 1;
      if (typeof value !== "string" || !value) return { refusal: { code: "RK_CARGO_BROKER_MANIFEST_PATH", message: "--manifest-path names no manifest" } };
      manifestPath = value;
      continue;
    }
    if (w === "--target-dir" || w.startsWith("--target-dir=")) return { refusal: { code: "RK_CARGO_REDIRECT", message: `\`cargo ${original.join(" ")}\` passes --target-dir; the broker pins a carried call's output directory` } };
    if (CARRIED_REDIRECT.test(w)) return { refusal: { code: "RK_CARGO_REDIRECT", message: `\`cargo ${original.join(" ")}\` passes ${w}, which moves its working directory, configuration or output` } };
    words.push(w);
  }
  return { words, manifestPath };
}

/**
 * What a carried call sends the broker for `args` run from `cwd` (a posix path relative to the workspace root, "." for the root): the
 * argv as signed (`requestedArgv`: no --manifest-path, `--locked` added to a build or run) and the directory it runs in (`cwd` joined
 * with the manifest's directory). Returns `{ argv, cwd }` or null when the call would be refused.
 */
export function carriedRequestOf(args, cwd = ".") {
  const split = splitCarriedArgs(args);
  if (split.refusal) return null;
  const dir = split.manifestPath === null ? cwd : path.posix.normalize(path.posix.join(cwd, path.posix.dirname(split.manifestPath.replaceAll("\\", "/"))));
  return { argv: lockedArgv(split.words), cwd: dir === "" ? "." : dir };
}

/**
 * Where the declared compile inputs are listed, as read from the snapshot (the broker's COMPILE_INPUTS; GB-49): `.rustright/compile-inputs.json` first,
 * `.rightkit/compile-inputs.json` ONLY when the canonical file is absent. A present but invalid canonical declaration is an error, never a fallback.
 */
export const COMPILE_INPUTS_CANONICAL = ".rustright/compile-inputs.json";
export const COMPILE_INPUTS_LEGACY = ".rightkit/compile-inputs.json";
export const COMPILE_INPUTS = COMPILE_INPUTS_CANONICAL;
export const COMPILE_INPUT_DECLARATIONS = Object.freeze([COMPILE_INPUTS_CANONICAL, COMPILE_INPUTS_LEGACY]);
export const MAX_DECLARATION_BYTES = 4 * 1024 * 1024;
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
 * The next per-call lease handle of a build ownership: `<ownership-handle>#<uuid>`. A workspace index or a per-process counter is not unique
 * (metadata, check and test reuse a workspace; wrapper children are separate processes), so each invocation draws its own random UUID. The broker accepts one
 * completion per handle on a snapshot, and its status for the bare ownership handle covers every `#` sub-handle.
 */
export const nextCallHandle = (handle) => `${handle}#${crypto.randomUUID()}`;
/** Whether `callHandle` is exactly one sub-handle of the ownership `handle` (`<handle>#<token>`, never the bare handle or another's). */
export const isCallHandleOf = (callHandle, handle) => typeof callHandle === "string" && typeof handle === "string" && handle !== "" && callHandle.length > handle.length + 1 && callHandle.startsWith(`${handle}#`) && /^[A-Za-z0-9._-]+$/.test(callHandle.slice(handle.length + 1));
/** The ownership handle a signed (sub-)handle belongs to: everything before the first `#`. */
export const ownershipOfHandle = (handle) => (typeof handle === "string" ? handle.split("#")[0] : handle);

/**
 * Writes the carrier for one cargo request and returns `{ file, sha256, descriptor, carrier, env }`; `env` is what that one
 * invocation adds (carrier descriptor, nonce, request-id file). Refuses before writing anything the reader would refuse.
 */
export function writeSnapshotRequest({ controlRoot, commit, snapshot, declaredInputs = [], handle, callHandle, primaryCwd, argv, nonce = freshNonce(), requestIdFile, uuid = crypto.randomUUID }) {
  if (!objectId(commit?.sha) || !objectId(commit?.tree)) fail("invalid", "commit sha and tree must be full lowercase object ids");
  if (!snapshot?.path || !path.isAbsolute(snapshot.path) || snapshot.expectedTree !== commit.tree) fail("invalid", "the snapshot needs an absolute path and the commit's tree as expectedTree");
  if (typeof handle !== "string" || !handle.trim()) fail("invalid", "the build-ownership handle is missing");
  if (callHandle !== undefined && !isCallHandleOf(callHandle, handle)) fail("invalid", `the call handle ${JSON.stringify(callHandle)} is not a sub-handle (<ownership-handle>#<n>) of the build ownership`);
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
    // One completion per (snapshot, lease handle) at the broker (rustright snapshot.rs:330-343): each carried request names its own
    // sub-handle `<ownership-handle>#<n>`, which the broker's bare-handle lease status aggregates (snapshot.rs:469).
    ownership: { handle: callHandle ?? nextCallHandle(handle) },
    primaryCwd,
    argvSha256: argvSha256(argv),
    nonce,
  };
  const bytes = Buffer.from(JSON.stringify(carrier), "utf8");
  if (bytes.length > CARRIER_MAX_BYTES) fail("too-large", `the snapshot request is ${bytes.length} bytes, more than ${CARRIER_MAX_BYTES}`);
  // GB-53: the request line is 1 MiB, newline included, one unit with the response limit. The declared inputs ride in it with the envelope: a request that cannot fit is
  // refused here, before the carrier is written, before it counts as sent, so no job exists.
  const overCap = requestOverCap(Buffer.byteLength(JSON.stringify(carrier.declaredInputs)) + Buffer.byteLength(JSON.stringify({ commit: carrier.commit, snapshot: carrier.snapshot, ownership: carrier.ownership, primaryCwd, argv })));
  if (overCap) fail(REQUEST_OVER_CAP_CODE, overCap);
  const file = path.join(dir, `${uuid()}.json`);
  const digest = sha256(bytes);
  const descriptor = JSON.stringify({ path: file, sha256: digest });
  if (Buffer.byteLength(descriptor) > DESCRIPTOR_MAX_BYTES) fail("too-large", "the snapshot request descriptor exceeds 4096 bytes");
  // GB-76 (BROKER-CONTRACT section 16): the creator is recorded FIRST, in `<carrier>.owner.json` (the gate's identity as RustRight reads it, the call's nonce and sub-handle, the creation time), then
  // the carrier; both are removed in the call's `finally`. A broker sweep retires a leftover only on proof that the gate is gone and nothing is held under the handle, never on age alone. A
  // record that cannot be written leaves no carrier.
  try { writeCarrierOwner({ file, handle: carrier.ownership.handle, nonce }); } catch (e) { if (e.code !== "EEXIST") fs.rmSync(ownerFileOf(file), { force: true }); fail("invalid", `the carrier's owner record cannot be written (${e.code ?? e.message}), so no carrier is left behind`); }
  try { fs.writeFileSync(file, bytes, { flag: "wx", mode: 0o600 }); } catch (e) { fs.rmSync(ownerFileOf(file), { force: true }); throw e; }
  const env = { [SNAPSHOT_REQUEST_ENV]: descriptor, [ATTEST_NONCE_ENV]: nonce, ...(requestIdFile ? { [REQUEST_ID_FILE_ENV]: requestIdFile } : {}) };
  return { file, sha256: digest, descriptor, carrier, env };
}

/**
 * The request core the broker digests for a snapshot request: the base core plus commit, snapshot, declaredInputs and ownership. A cargo
 * request has `shim: "cargo"` and no adapter; a carried adapter request (rustright#190 D2) has `shim: "adapter"`, the registered adapter's
 * name, no observed environment (the client captures none for an adapter), and `env`: the request's own environment object, exactly the
 * variables RUSTRIGHT_ADAPTER_ENV names (rustright-protocol attestation.rs request_core `environment`; adapter-env.mjs), always empty for Cargo.
 * The adapter spelling is RustRight's; see snapshot-broker.mjs carryBrokerAdapter.
 */
export function snapshotRequestCore({ argv, workspace, observedEnv = {}, carrier, adapter = null, adapterEnv = {} }) {
  return {
    adapter, env: adapter === null ? {} : { ...adapterEnv }, inputManifest: null, nonce: carrier.nonce, observedEnv: adapter === null ? observedEnv : {}, outputs: [], requestedArgv: [...argv], shim: adapter === null ? "cargo" : "adapter", workspace,
    commit: carrier.commit, snapshot: carrier.snapshot, declaredInputs: carrier.declaredInputs, ownership: carrier.ownership,
  };
}
export const requestDigestOf = (core) => sha256(canonicalJson(core));

/** The request reference the client writes at submit (RUSTRIGHT_REQUEST_ID_FILE), or a throw. */
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
 * The declared inputs of a snapshot, read FROM THE SNAPSHOT, never the checkout, by the one rule every RightKit reader shares (the broker's, vector
 * docs/contracts/declared-inputs.fixtures.json): `.rustright/compile-inputs.json` when it exists, else `.rightkit/compile-inputs.json`, else none. Either file
 * is judged by the same checks: no link on its path, a regular file of at most 4 MiB, JSON with `inputs` a list of plain relative paths (not absolute, no `.`,
 * `..` or empty component, not inside `.git`) and no input that is, or holds, either declaration file. A present but invalid canonical declaration is an
 * error: it is never read as the legacy one.
 */
export function readDeclaredInputs(snapshotRoot) {
  let file = null; let at = null; let meta = null;
  for (const candidate of COMPILE_INPUT_DECLARATIONS) {
    let here = snapshotRoot; let found = true;
    for (const part of candidate.split("/")) {
      here = path.join(here, part);
      const m = lstatOrNull(here);
      if (m === null) { found = false; break; }
      if (m.isSymbolicLink()) fail("declared-inputs", `${candidate} runs through a link at ${here}`);
      meta = m;
    }
    if (found) { file = candidate; at = here; break; }
  }
  if (file === null) return [];
  if (!meta.isFile()) fail("declared-inputs", `${file} is not a regular file`);
  if (meta.size > MAX_DECLARATION_BYTES) fail("declared-inputs", `${file} is larger than ${MAX_DECLARATION_BYTES} bytes`);
  let value;
  try { value = JSON.parse(fs.readFileSync(at, "utf8")); } catch (e) { fail("declared-inputs", `${file} is not JSON (${e.message})`); }
  if (!value || typeof value !== "object" || !Array.isArray(value.inputs)) fail("declared-inputs", `${file}: \`inputs\` must be a list of paths`);
  for (const input of value.inputs) {
    if (typeof input !== "string" || !input) fail("declared-inputs", `${file}: every input is a non-empty relative path`);
    const components = input.split(/[\\/]/);
    if (path.isAbsolute(input) || path.posix.isAbsolute(input) || path.win32.isAbsolute(input) || components.some((c) => c === "" || c === "." || c === "..")) fail("declared-inputs", `${file}: ${input} is not a plain relative path inside the snapshot`);
    if (components.some((c) => c.toLowerCase() === ".git")) fail("declared-inputs", `${file}: ${input} is inside .git; repository metadata is never a compile input`);
    for (const declaration of COMPILE_INPUT_DECLARATIONS) {
      const parts = declaration.split("/");
      if (components.length <= parts.length && components.every((c, i) => c === parts[i])) fail("declared-inputs", `${file}: ${input} overlaps the declaration file itself (${declaration})`);
    }
  }
  return [...value.inputs];
}

/** The inputs a request supplied must be exactly the declaration the snapshot holds (the broker's check): a throw naming the difference. */
export function assertSuppliedInputs(snapshotRoot, supplied) {
  const declared = readDeclaredInputs(snapshotRoot);
  if (!same(declared, supplied)) fail("declared-inputs", "the snapshot's declared inputs are not the ones this request carried (declaredInputs differs from the snapshot declaration)");
  return declared;
}

/**
 * The `declaredInputDigests` the broker records for these inputs, computed here from the snapshot: per input, sha256 over, for each
 * regular file in directory order (names sorted by their bytes), the file's relative name, a NUL, its bytes and a NUL. A link
 * anywhere, a special file, or a limit breach is a refusal. The name is the input's path components joined by `/` on EVERY platform (GB-49: RustRight hashes
 * `/` on Windows too, so a `\` in a declaration or a platform separator never reaches the hash), UTF-8, no Unicode normalisation.
 */
export function declaredInputDigestsOf(snapshotRoot, inputs) {
  let files = 0; let bytes = 0;
  const walk = (full, name, hasher) => {
    const meta = fs.lstatSync(full);
    if (meta.isSymbolicLink()) fail("declared-inputs", `declared compile input holds a link at ${name}`);
    if (meta.isDirectory()) {
      const children = fs.readdirSync(full).sort((a, b) => Buffer.compare(Buffer.from(a, "utf8"), Buffer.from(b, "utf8")));
      for (const child of children) walk(path.join(full, child), `${name}/${child}`, hasher);
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
    walk(at, input.split(/[\\/]/).join("/"), hasher);
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
 * The leaf names of a managed generation (`g/<gen>/<leaf>`). The leaf is per GENERATION, not per platform (rustright-state lib.rs:52-62 and
 * :422-461): a generation keeps the leaf it was created with, and only new generations default to `t` on Windows and `target` elsewhere, so
 * either may appear on either platform. The gate takes the leaf from the signed outputDir; it never infers it and never picks a directory.
 */
export const GENERATION_LEAVES = Object.freeze(["target", "t"]);
const isGenerationLeaf = (segment) => GENERATION_LEAVES.includes(segment);

/**
 * The generation target the broker pins an app's outputs to: `<root>/<12 hex>/g/<NNN>/<leaf>` (leaf `target` or `t`, GENERATION_LEAVES) under one of the managed `ws` roots
 * (`<external data root>/rightkit-build-control/ws`; BROKER-CONTRACT "Source identity", output pinning). The 12-hex key is
 * rustright-state's workspace_key, which the gate never computes (the snapshot's own key is `snapshots/<sha256(repo path)[:16]>`, a
 * different thing); it reads the receipt's outputDir and checks only this shape. The generation number (3+ digits) is the broker's.
 */
export function isGenerationTarget(outputDir, outputRoots, platform = process.platform) {
  const mod = platform === "win32" ? path.win32 : path;
  for (const root of outputRoots) {
    const rel = mod.relative(platform === "win32" ? mod.resolve(root).toLowerCase() : mod.resolve(root), platform === "win32" ? mod.resolve(outputDir).toLowerCase() : mod.resolve(outputDir));
    const segments = rel.split(/[\\/]/);
    if (segments.length === 4 && segments[0] !== ".." && !mod.isAbsolute(rel) && /^[0-9a-f]{12}$/.test(segments[0]) && segments[1] === "g" && /^[0-9]{3,}$/.test(segments[2]) && isGenerationLeaf(segments[3])) return true;
  }
  return false;
}

const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

/** Whether `dir` is a managed generation target or a directory inside one (a nested compile's own directory under the adapter job's target). */
export function isInsideGenerationTarget(dir, outputRoots, platform = process.platform) {
  const mod = platform === "win32" ? path.win32 : path;
  for (const root of outputRoots) {
    const rel = mod.relative(platform === "win32" ? mod.resolve(root).toLowerCase() : mod.resolve(root), platform === "win32" ? mod.resolve(dir).toLowerCase() : mod.resolve(dir));
    const segments = rel.split(/[\\/]/);
    if (segments.length >= 4 && segments[0] !== ".." && !mod.isAbsolute(rel) && /^[0-9a-f]{12}$/.test(segments[0]) && segments[1] === "g" && /^[0-9]{3,}$/.test(segments[2]) && isGenerationLeaf(segments[3])) return true;
  }
  return false;
}


/**
 * The filesystem-free identity of a signed `snapshotEvidence` (rightkit#1301 B9), shared by the gate's own verification of a receipt
 * (verifySnapshotReceipt) and the hosted admission (qualification-verify.mjs): the commit is `commit` ({ sha, tree }), the ownership
 * handle is `handle` when one is known, the request id is the job's, the snapshot path is `snapshotPath` when one is known (compared with
 * `samePath`, which the gate gives a real-path comparison), and the snapshot held `tree` at START and at exit. Throws a
 * SnapshotRequestError (`evidence`); returns the evidence's `snapshot`.
 */
export function verifySnapshotEvidenceIdentity(evidence, { commit, tree = commit?.tree, jobId, handle, snapshotPath, samePath = (a, b) => a === b }) {
  if (!evidence || typeof evidence !== "object") fail("evidence", "the signed payload carries no snapshotEvidence: the broker did not build this as a snapshot request");
  if (!same(evidence.commit, commit)) fail("evidence", "the signed commit is not the one this request carried");
  if (handle !== undefined && evidence.ownership?.handle !== handle) fail("evidence", "the signed ownership handle is not this gate's");
  if (typeof evidence.requestId !== "string" || evidence.requestId !== jobId) fail("evidence", "the signed request id is not this job");
  const snap = evidence.snapshot ?? {};
  if (snapshotPath !== undefined && !samePath(snap.path, snapshotPath)) fail("evidence", `the signed snapshot ${snap.path} is not ${snapshotPath}`);
  if (snap.treeAtStart !== tree || snap.treeAtExit !== tree) {
    fail("evidence", `the snapshot was tree ${snap.treeAtStart} at START and ${snap.treeAtExit} at exit, not ${tree} throughout: the build changed a tracked file (or added or removed one) in the commit's snapshot, so the result is discarded; that is the app owner's defect to fix (write generated output to an ignored path or an output directory, or generate before commit)`);
  }
  return snap;
}

export const NESTED_CHILD_FIELDS = Object.freeze(["requestId", "parentRequestId", "shim", "argv", "cwd", "exitCode", "status", "outputDir"]);
/** The most children a root job's list may carry (rustright#190: the broker's cap; a refused child is never listed). */
export const NESTED_CHILDREN_CAP = 8192;

/**
 * The nested children of a snapshot-bound adapter job (rustright#190, final): the list the broker signs at `payload.snapshotEvidence.nestedChildren`
 * and mirrors at the receipt's `nestedChildren`; BOTH are required and must be equal. Entries are in admission order, each exactly
 * `{ requestId, parentRequestId, shim, argv, cwd, exitCode, status, outputDir }`: `argv` is redacted (cargo only), `exitCode` is an integer or
 * null, `status` is "UNFINISHED" for a child that had not ended when the root's receipt was built, `outputDir` is null for a child that does not
 * compile (`rustc -vV`, `metadata`). A refused child is not listed. Returns the normalised list or throws a SnapshotRequestError (`nested`).
 * Refused: a missing or differing list, more than the cap, a child whose `parentRequestId` is neither the root job nor an earlier listed child, a repeated request id, an
 * UNFINISHED child, a null or non-zero exit code, a cwd outside the snapshot, and a non-null `outputDir` that is not absolute, is inside the
 * snapshot or the checkout, or is not a managed generation target or inside one.
 */
export function verifyNestedChildren(evidence, receipt, { snapshotPath, checkout, outputRoots, platform = process.platform, canonicalPath = (p) => fs.realpathSync.native(p), jobId }) {
  const list = evidence?.nestedChildren;
  if (!Array.isArray(list)) fail("nested", "the signed snapshotEvidence carries no nestedChildren list: the broker did not sign what this adapter job's children ran");
  if (!Array.isArray(receipt?.nestedChildren)) fail("nested", "the receipt carries no nestedChildren list (the mirror of the signed one)");
  if (!same(receipt.nestedChildren, list)) fail("nested", "receipt nestedChildren differs from the signed list");
  if (list.length > NESTED_CHILDREN_CAP) fail("nested", `the nestedChildren list holds ${list.length} entries, more than the cap ${NESTED_CHILDREN_CAP}`);
  const canon = (p) => { try { return canonicalPath(p); } catch { return p; } };
  const ids = new Set();
  return list.map((child, index) => {
    const where = `nestedChildren[${index}]`;
    if (!isObject(child) || Object.keys(child).sort().join() !== [...NESTED_CHILD_FIELDS].sort().join()) fail("nested", `${where} is not exactly { ${NESTED_CHILD_FIELDS.join(", ")} }`);
    if (typeof child.requestId !== "string" || !child.requestId || child.requestId === jobId || ids.has(child.requestId)) fail("nested", `${where} has no request id of its own`);
    const name = `${where} (${child.requestId})`;
    // The parent is the job the request named: this job, or a child listed before it (admission order; depth >= 2, rustright#190 ad9ecb37).
    if (child.parentRequestId !== jobId && !ids.has(child.parentRequestId)) fail("nested", `${name} names parent ${JSON.stringify(child.parentRequestId)}, neither this job ${jobId} nor an earlier listed child`);
    ids.add(child.requestId);
    if (typeof child.shim !== "string" || !child.shim) fail("nested", `${name} names no shim`);
    if (!Array.isArray(child.argv) || !child.argv.every((a) => typeof a === "string")) fail("nested", `${name} argv is not a list of strings`);
    if (child.status === "UNFINISHED") fail("nested", `${name} (${child.argv.join(" ")}) had not finished when the job's receipt was built (UNFINISHED)`);
    if (typeof child.status !== "string" || !child.status) fail("nested", `${name} has no status`);
    if (!Number.isSafeInteger(child.exitCode) || child.exitCode !== 0) fail("nested", `${name} (${child.argv.join(" ")}) exited ${child.exitCode === null ? "with no exit code" : child.exitCode}, not 0: a child of this job failed or never finished`);
    if (typeof child.cwd !== "string" || (!path.isAbsolute(child.cwd) && !path.win32.isAbsolute(child.cwd))) fail("nested", `${name} names no absolute working directory`);
    if (!inside(child.cwd, snapshotPath, platform) && !inside(canon(child.cwd), canon(snapshotPath), platform)) fail("nested", `${name} ran in ${child.cwd}, outside the snapshot ${snapshotPath}`);
    const out = child.outputDir;
    if (out !== null) {
      if (typeof out !== "string" || (!path.isAbsolute(out) && !path.win32.isAbsolute(out))) fail("nested", `${name} names an output directory that is neither null nor absolute`);
      if (inside(out, snapshotPath, platform) || inside(out, canon(snapshotPath), platform)) fail("nested", `${name} output directory ${out} is inside the snapshot`);
      if (inside(out, checkout, platform) || inside(out, canon(checkout), platform)) fail("nested", `${name} output directory ${out} is inside the checkout`);
      if (!isInsideGenerationTarget(out, outputRoots, platform)) fail("nested", `${name} output directory ${out} is not inside a managed generation target under ${outputRoots.join(", ")}`);
    }
    return { requestId: child.requestId, parentRequestId: child.parentRequestId, shim: child.shim, argv: [...child.argv], cwd: child.cwd, exitCode: 0, status: child.status, outputDir: out };
  });
}

/**
 * Verifies the receipt of one snapshot request (a cargo call, or with `adapter` an adapter job). `pins` come from this host's enrollment, never from the receipt.
 * Returns `{ payload, evidence, attribution, toolchain, nestedChildren, requestDigest, causeEvidence }` (`causeEvidence` is null for a command that carries none); throws ProvenanceError (legacy, signature, pins, job, nonce, request, evidence, cause-unknown, cause-unexplained) or SnapshotRequestError (evidence).
 *
 * Beyond the signature, the job, the nonce and the request digest, the receipt must carry (item 7 row 7.22):
 *   - signed SUCCEEDED with exit 0, and signed snapshotEvidence equal to the receipt's copies (commit, ownership handle, request id,
 *     snapshot path, `treeAtStart` = `treeAtExit` = the expected tree);
 *   - `declaredInputDigests` equal to the gate's own digest of the snapshot's declared inputs, read from the snapshot now;
 *   - `outputDir` absolute, outside the snapshot and the checkout, and a managed generation target under one of `outputRoots`;
 *   - signed stageEvidence (freshness, attribution, toolchain) equal to the receipt's;
 *   - for a compile, freshness.causeEvidence (rustright.cause-evidence.v1) coherent with its census and with a verdict of `explained`: refused as `evidence` when missing,
 *     of another schema or incoherent, `cause-unknown` when the broker could not say why units were rebuilt, `cause-unexplained` when a rebuilt unit has no explained cause (RA-03);
 *   - timingsMs, exit, diagnosticsPath, brokerVersion and executor present with their types; the retired `verdict` and `refreshed` are refused (GB-30: Class A and its verdict reuse are the gate's).
 * `checkout` and `outputRoots` are required: without them nothing can be said about where the outputs went, so it refuses.
 */
export function verifySnapshotReceipt(receipt, { pins, jobId, carrier, argv, workspace, observedEnv = {}, adapter = null, adapterEnv = {}, checkout, outputRoots, platform = process.platform, canonicalPath = (p) => fs.realpathSync.native(p), digestInputs = declaredInputDigestsOf, readInputs = readDeclaredInputs }) {
  const requestDigest = requestDigestOf(snapshotRequestCore({ argv, workspace, observedEnv, carrier, adapter, adapterEnv }));
  const payload = verifyProvenance(receipt, pins, { jobId, nonce: carrier.nonce, requestDigest });
  // The signed outcome, not the client's exit code: a passing row needs the broker to have signed SUCCEEDED with exit 0 and no
  // signal (service.rs provenance_payload `result`). An authentic FAILED, or a payload without a result, is not a success.
  const result = payload.result;
  if (!result || typeof result !== "object") fail("result", "the signed payload carries no result: the broker did not sign this request's outcome");
  if (result.status !== "SUCCEEDED" || result.exitCode !== 0 || (result.signal !== undefined && result.signal !== null)) {
    // Any terminal status but SUCCEEDED fails the call, whatever the broker says about why (a tracked write is refused at exit with a
    // `snapshot exit bind: ...` message; the field that carries it is pending RustRight's confirmation, so the usual spellings are read).
    // The signed result of a broker verdict after a clean run (FAILED with exit 0: RR_UNEXPLAINED_COLD, ...) carries no reason; the receipt's own
    // unsigned `error` names it. It is shown for diagnosis only and decides nothing: the verdict is the signed status.
    // GB-66: a compile the broker refused to run because it could not name its toolchain fails with this code (and an UNKNOWN toolchain on its receipt).
    if ([result.code, receipt?.code].includes(TOOLCHAIN_UNAVAILABLE_CODE)) fail(TOOLCHAIN_UNAVAILABLE, `the broker did not run this compile: it could not build the toolchain identity (${TOOLCHAIN_UNAVAILABLE_CODE}${isToolchainUnknown(receipt?.toolchain) ? `: ${receipt.toolchain.reason}` : ""}); register the toolchain's cargo and rustc and its name, then ask again`);
    const why = [result.message, result.error, result.reason].find((v) => typeof v === "string" && v !== "");
    const unsigned = why ? "" : [receipt?.error, receipt?.code].find((v) => typeof v === "string" && v !== "");
    fail("result", `the broker signed ${result.status ?? "no status"} (exit ${result.exitCode ?? "none"}${result.signal ? `, signal ${result.signal}` : ""}${why ? `; ${why}` : ""}${unsigned ? `; the receipt's unsigned error: ${unsigned}` : ""}), not SUCCEEDED with exit 0`);
  }
  const evidence = payload.snapshotEvidence;
  if (!evidence || typeof evidence !== "object") fail("evidence", "the signed payload carries no snapshotEvidence: the broker did not build this as a snapshot request");
  for (const name of SNAPSHOT_EVIDENCE_FIELDS) {
    if (evidence[name] === undefined || !same(receipt[name], evidence[name])) fail("evidence", `receipt ${name} differs from the signed snapshot evidence`);
  }
  verifySnapshotEvidenceIdentity(evidence, {
    commit: carrier.commit, tree: carrier.snapshot.expectedTree, jobId, handle: carrier.ownership.handle, snapshotPath: carrier.snapshot.path,
    samePath: (a, b) => { try { return canonicalPath(a) === canonicalPath(b); } catch { return false; } },
  });
  // Declared inputs: what the broker digested in place must be what the gate digests in the snapshot, from the same manifest.
  let gateInputs; let gateDigests;
  try {
    gateInputs = readInputs(carrier.snapshot.path);
    gateDigests = digestInputs(carrier.snapshot.path, gateInputs, { platform });
  } catch (e) { fail("evidence", `the gate cannot digest the snapshot's declared inputs (${e.message})`); }
  if (!same(gateInputs, carrier.declaredInputs?.inputs)) fail("evidence", "the snapshot's declared inputs are not the ones this request carried (declaredInputs differs from the snapshot declaration)");
  if (!same(evidence.declaredInputDigests, gateDigests)) fail("evidence", "the signed declaredInputDigests are not the gate's own digest of the snapshot's declared inputs");
  // Output directory: where the outputs went must be the broker's managed generation target, never under the snapshot or the checkout.
  const out = evidence.outputDir;
  if (typeof out !== "string" || !path.isAbsolute(out) && !path.win32.isAbsolute(out)) fail("evidence", "the signed evidence names no absolute output directory");
  if (!checkout || !path.isAbsolute(checkout) || !Array.isArray(outputRoots) || outputRoots.length === 0) fail("evidence", "the gate has no checkout or managed output root to judge the output directory against");
  const canon = (p) => { try { return canonicalPath(p); } catch { return p; } };
  if (inside(out, carrier.snapshot.path, platform) || inside(out, canon(carrier.snapshot.path), platform)) fail("evidence", `the output directory ${out} is inside the snapshot`);
  if (inside(out, checkout, platform) || inside(out, canon(checkout), platform)) fail("evidence", `the output directory ${out} is inside the checkout`);
  if (!isGenerationTarget(out, outputRoots, platform)) fail("evidence", `the output directory ${out} is not a managed generation target (<ws root>/<repo-key>/g/<generation>/{target|t}) under ${outputRoots.join(", ")}`);
  // Stage evidence is signed with the rest and must equal the receipt's copy. `freshness`, `attribution` and `toolchain` are
  // required only for a compile-class command (check, clippy, build, doc, rustc, test): the broker emits them for compiles only, so a
  // read-only metadata, fetch --locked, fmt --check or tree --locked passes without them (rightkit#1311: a carried metadata job has
  // no freshness census; the Mac witness, request ed37214e). When either side carries one
  // anyway, both must carry the same value.
  const stage = payload.stageEvidence;
  if (!isObject(stage)) fail("evidence", "the signed payload carries no stageEvidence");
  // An adapter job is not a cargo command: whether it signs attribution and toolchain is the adapter's; when either side carries one,
  // both must carry the same value (below).
  const compile = adapter === null && isCompileClass(argv);
  for (const name of STAGE_EVIDENCE_FIELDS) {
    const signed = stage[name] ?? null; const copy = receipt[name] ?? null;
    const required = compile;
    if (required && (signed === null || copy === null)) fail("evidence", `${name} is missing from the signed stage evidence or the receipt (a compile-class command, cargo ${cargoSubcommandOf(argv)}, carries it)`);
    if (!same(copy, signed)) fail("evidence", `receipt ${name} differs from the signed stage evidence`);
  }
  // Shapes RustRight emits (item 4 row 4.18): attribution is `{available, ...}`, and an unavailable one names its reason
  // (`cargo-json-message-format-not-requested` without a JSON message stream; a consumer that needs per-package attribution treats
  // `available:false` as FAILED, not partial); toolchain is `<toolchain>|cargo:<sha256>|rustc:<sha256>`.
  // GB-02 (BROKER-CONTRACT section 14): a compile's freshness is an object with a boolean `available`, and `available: false` always names a reason: absence is never an
  // unavailable census, and an unavailable one on a SUCCEEDED compile is the broker's own failure (RR_FRESHNESS_UNAVAILABLE), never evidence.
  if (compile) {
    const freshness = receipt.freshness;
    if (!isObject(freshness) || typeof freshness.available !== "boolean") fail("evidence", "receipt freshness is not {available, ...}");
    if (freshness.available === false && !(typeof freshness.reason === "string" && freshness.reason !== "")) fail("evidence", "receipt freshness is unavailable and names no reason");
  }
  const attribution = receipt.attribution ?? null; const toolchain = receipt.toolchain ?? null;
  if (attribution !== null) {
    if (!isObject(attribution) || typeof attribution.available !== "boolean") fail("evidence", "receipt attribution is not {available, ...}");
    if (attribution.available === false && !(typeof attribution.reason === "string" && attribution.reason !== "")) fail("evidence", "receipt attribution is unavailable and names no reason");
  }
  // GB-66: the broker signs `<name>|cargo:<sha256>|rustc:<sha256>` only when it could build that identity; when it could not (no configured name, an unregistered or
  // changed cargo or rustc) the receipt's toolchain is exactly {"state":"UNKNOWN","reason":...}. That is a typed unavailable identity, never evidence and never a
  // string with an empty component.
  if (isToolchainUnknown(toolchain)) fail(TOOLCHAIN_UNAVAILABLE, `the broker could not name this compile's toolchain (${TOOLCHAIN_UNAVAILABLE_CODE}: UNKNOWN, ${toolchain.reason}); an unknown toolchain identity is not evidence, so the call is unverified and nothing is concluded from it`);
  if (toolchain !== null && !(typeof toolchain === "string" && TOOLCHAIN_FORMAT.test(toolchain))) fail("evidence", "receipt toolchain is not <toolchain>|cargo:<sha256>|rustc:<sha256>");
  // RA-03: a census that counts rebuilt units says nothing about why they were rebuilt. A compile's signed freshness must carry `causeEvidence`
  // (rustright.cause-evidence.v1: provenance-verify.mjs requireExplainedCauses, the one implementation) whose verdict is `explained`: a missing or unrecognized
  // schema or an incoherent one is `evidence`, an `unknown` verdict (a quiet, caller-logged or artifact-running root whose log was not read, dropped output, an
  // uncovered unit) is `cause-unknown`, and a unit rebuilt with no explained cause is `cause-unexplained`; none is a success, and none is relaxed for an older
  // broker that signs no cause evidence. The judged object is the SIGNED copy (the receipt's equals it, checked above). A command that is not a compile
  // legitimately carries no census; when one carries cause evidence anyway it must still be well formed, but nothing is judged for it (nothing was rebuilt).
  const signedFreshness = stage.freshness ?? null;
  let causeEvidence = null;
  if (compile) causeEvidence = requireExplainedCauses(signedFreshness);
  else if (isObject(signedFreshness) && signedFreshness.causeEvidence !== undefined) causeEvidence = verifyCauseEvidence(signedFreshness);
  // Fields the signature does not cover must still be present with their types (the consumer reads them). Every carried command:
  // exit (an integer, or null with a signal) and signal, timingsMs{admission, queue, bind, cargo, afterExitBind, diagnostics} (each a
  // number or null) and diagnosticsPath (a path, or null). Success needs exit 0 and no signal, as signed. `verdict` and `refreshed`
  // are retired: the gate owns Class A, and RustRight refuses a receipt that carries either (rustright bc24d55, attestation.rs
  // RETIRED_RESULT_FIELDS), so the gate refuses them too.
  const timingsOk = isObject(receipt.timingsMs) && TIMING_KEYS.every((k) => Object.hasOwn(receipt.timingsMs, k) && (receipt.timingsMs[k] === null || Number.isFinite(receipt.timingsMs[k])));
  const typed = [
    ["timingsMs", timingsOk], ["exit", receipt.exit === null || Number.isSafeInteger(receipt.exit)],
    ["signal", receipt.signal === null || typeof receipt.signal === "string" || Number.isSafeInteger(receipt.signal)],
    ["diagnosticsPath", receipt.diagnosticsPath === null || (typeof receipt.diagnosticsPath === "string" && receipt.diagnosticsPath !== "")], ["brokerVersion", typeof receipt.brokerVersion === "string" && receipt.brokerVersion !== ""],
    ["executor", typeof receipt.executor === "string" && receipt.executor !== ""],
  ];
  for (const [name, ok] of typed) if (!ok) fail("evidence", `receipt ${name} is missing or has the wrong type`);
  for (const name of ["verdict", "refreshed"]) if (Object.hasOwn(receipt, name)) fail("evidence", `receipt carries the retired field ${name}`);
  if (receipt.exit !== 0 || receipt.signal !== null) fail("evidence", `receipt exit ${receipt.exit} and signal ${receipt.signal} are not a success (exit 0, no signal)`);
  // RustRight signs the result fields under payload.resultEvidence (service.rs provenance_payload; attestation.rs RESULT_EVIDENCE_FIELDS), never at
  // the payload's top level. Every receipt copy must equal its signed counterpart, and a receipt with no signed resultEvidence is refused: the
  // exit, signal, timings and diagnostics path it shows would otherwise be this receipt's own say-so.
  const signedResult = payload.resultEvidence;
  if (!isObject(signedResult)) fail("evidence", "the signed payload carries no resultEvidence: the receipt's exit, signal, timingsMs and diagnosticsPath are not covered by the signature");
  for (const name of RESULT_EVIDENCE_FIELDS) {
    if (!Object.hasOwn(signedResult, name) || !same(receipt[name], signedResult[name])) fail("evidence", `receipt ${name} differs from the signed result evidence`);
  }
  for (const name of ["verdict", "refreshed"]) if (Object.hasOwn(signedResult, name)) fail("evidence", `the signed result evidence carries the retired field ${name}`);
  // Symmetric both ways: a field the broker signed that the protocol does not name is as unexplained as one the receipt lacks (an added field).
  for (const name of Object.keys(signedResult)) if (!RESULT_EVIDENCE_FIELDS.includes(name)) fail("evidence", `the signed result evidence carries the unknown field ${name}`);
  for (const name of Object.keys(isObject(receipt.timingsMs) ? receipt.timingsMs : {})) if (!TIMING_KEYS.includes(name)) fail("evidence", `receipt timingsMs carries the unknown key ${name}`);
  // A snapshot-bound adapter job signs the compiles its own children made (rustright#190 D8).
  const nestedChildren = adapter === null ? null : verifyNestedChildren(evidence, receipt, { snapshotPath: carrier.snapshot.path, checkout, outputRoots, platform, canonicalPath, jobId });
  // GB-10 / GB-11: whatever the checks above did not already name, the production strict verifier (`verify_receipt_strict`, provenance-verify.mjs, judged
  // against RustRight's receipt vector case for case) must also accept: no signed-only or receipt-only field anywhere, the signed result evidence with exactly
  // its four fields and six phases, and the BR19 shape (profile, stage, resources, environment; a value or a typed UNKNOWN, never absent).
  verifyReceiptCoverage(receipt, payload);
  verifyResultEvidence(receipt, payload);
  verifyBr19Evidence(receipt, payload);
  return { payload, evidence, attribution, toolchain, nestedChildren, requestDigest, causeEvidence };
}

export { CAUSE_EVIDENCE_SCHEMA, CAUSE_UNEXPLAINED, CAUSE_UNKNOWN, ProvenanceError };
