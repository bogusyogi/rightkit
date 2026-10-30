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
import { compareBrokerVersion } from "./broker-pins.mjs";
import { canonicalJson, ProvenanceError, verifyProvenance } from "./provenance-verify.mjs";

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
/** `<toolchain>|cargo:<sha256>|rustc:<sha256>` (rustright toolchain_identity). */
const TOOLCHAIN_FORMAT = /^[^|\s][^|]*\|cargo:[0-9a-f]{64}\|rustc:[0-9a-f]{64}$/;

/**
 * The ONE table of what the broker classifies as a compile (a Capability that builds: its receipt carries freshness, attribution and toolchain), read from
 * rustright b84ae605 crates/rustright-policy/src/command_policy.rs `cargo_capability`: `fix` (:581), `run` (:597, CARGO_RUN = BUILD that stays resident),
 * `package` unless it is the observation shape (:611, `observation_only_cargo`), `publish` (:615) and `build check test bench clippy doc rustc rustdoc` (:619).
 * Both decisions the gate makes from an argv derive from it: which evidence a receipt must carry (`isCompileClass`) and which commands get `--locked`
 * (`lockedArgv`). `package` is a compile only when it is not the exact `-p <crate> --list --locked [--allow-dirty]` listing (`isPackageObservation`).
 */
export const BROKER_COMPILE_COMMANDS = Object.freeze(["fix", "run", "package", "publish", "build", "check", "test", "bench", "clippy", "doc", "rustc", "rustdoc", "nextest", "ndk"]);
/**
 * The registered external compiling commands (command_policy.rs external_policy `nextest`/`ndk` => BUILD, :67-70). They compile, so their receipts carry the compile
 * evidence, but they own their argv (`cargo ndk -t <abi> ... build`, `cargo nextest run`): the gate never inserts `--locked` after their subcommand.
 */
const EXTERNAL_COMPILE_COMMANDS = Object.freeze(["nextest", "ndk"]);
/**
 * The one-letter aliases of Cargo. The broker lists none (an alias is refused there); the gate reads one as the command it expands to, so an alias can only
 * make the gate demand MORE evidence and lock MORE, never less.
 */
export const CARGO_COMMAND_ALIASES = Object.freeze({ b: "build", c: "check", t: "test", r: "run", d: "doc" });
const canonicalCommand = (sub) => (typeof sub === "string" && Object.hasOwn(CARGO_COMMAND_ALIASES, sub) ? CARGO_COMMAND_ALIASES[sub] : sub);
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
const PACKAGE_PLAIN_FLAGS = new Set(["--list", "--locked", "--allow-dirty"]);
/**
 * Whether `argv` is the one package form the broker admits as an observation that compiles nothing (rustright build_rules.rs `observation_only_cargo`, package
 * arm): no `--`, no `+toolchain`, the subcommand as the one positional, `-p`/`--package` once with a value that is not a flag (`--package=x` is read, `-p=x` is not),
 * and only `--list`, `--locked` and `--allow-dirty` besides; it must carry `--list`, `--locked` and one package. Anything else is a compiling package.
 */
export function isPackageObservation(argv) {
  const words = [...argv];
  if (words.includes("--") || words.some((w) => w.startsWith("+")) || cargoSubcommandOf(words) !== "package") return false;
  let seen = false; let list = false; let locked = false; let packages = 0;
  for (let i = 0; i < words.length; i += 1) {
    const w = words[i];
    if (!w.startsWith("-")) { if (seen || w !== "package") return false; seen = true; continue; }
    const eq = w.startsWith("--") ? w.indexOf("=") : -1;
    const name = eq >= 0 ? w.slice(0, eq) : w; const inline = eq >= 0 ? w.slice(eq + 1) : null;
    if (name === "-p" || name === "--package") {
      const value = inline ?? words[++i];
      if (typeof value !== "string" || value === "" || value.startsWith("-")) return false;
      packages += 1; if (packages > 1) return false;
      continue;
    }
    if (!PACKAGE_PLAIN_FLAGS.has(name) || inline !== null) return false;
    if (name === "--list") list = true; else if (name === "--locked") locked = true;
  }
  return seen && list && locked && packages === 1;
}
/** Whether the broker classifies this argv (without the tool name) as a compile. `package` is one unless it is the observation listing. */
export const isCompileClass = (argv) => {
  const sub = canonicalCommand(cargoSubcommandOf(argv));
  return BROKER_COMPILE_COMMANDS.includes(sub) && !(sub === "package" && isPackageObservation(argv));
};
/** The commands that build or run, and so can rewrite Cargo.lock inside a snapshot unless they are locked (rightkit#1301): the same table as `isCompileClass`, minus the external ones that own their argv. */
export const isLockBound = (argv) => isCompileClass(argv) && !EXTERNAL_COMPILE_COMMANDS.includes(canonicalCommand(cargoSubcommandOf(argv)));

/**
 * The argv a carried call sends: a build or run command with neither `--locked` nor `--frozen` (before any `--`) gets `--locked` right
 * after its subcommand. RustRight refuses an unlocked grant-minted build, and an unlocked build could rewrite Cargo.lock in the snapshot.
 * Adding it, not refusing, is the less surprising rule: the call runs as the caller meant it, except that a stale lock fails instead of
 * being rewritten. The ledger and the signed request digest both carry the argv as sent.
 */
export function lockedArgv(argv) {
  const words = [...argv];
  if (!isLockBound(words)) return words;
  const end = words.indexOf("--");
  if ((end < 0 ? words : words.slice(0, end)).some((w) => w === "--locked" || w === "--frozen")) return words;
  let i = words[0]?.startsWith("+") ? 1 : 0;
  for (; i < words.length; i += 1) {
    if (words[i] === "--") break;
    if (GLOBAL_VALUE_OPTIONS.has(words[i])) { i += 1; continue; }
    if (words[i].startsWith("-")) continue;
    break;
  }
  return [...words.slice(0, i + 1), "--locked", ...words.slice(i + 1)];
}

/**
 * Whether a short-flag cluster reaches `-Z` or `-C` (`-Z...`, `-vZ...`, `-rC/dir`), the broker's command_policy.rs `short_cluster_carries_z_or_c`: the scan
 * stops at a short flag that takes a value (`-j`, `-p`, `-F`: the rest of the argument is that value, so `-pCore` and `-FZed` are packages and features), any
 * other letter keeps it going (which can only refuse more). `--long` options are no cluster.
 */
export function shortClusterCarriesZOrC(arg) {
  if (typeof arg !== "string" || !arg.startsWith("-") || arg.startsWith("--")) return false;
  for (const c of arg.slice(1)) {
    if (c === "Z" || c === "C") return true;
    if (c === "j" || c === "p" || c === "F") return false;
  }
  return false;
}
/** Whether one cargo-own argument moves the configuration or the output (`--config`, `--config=x`, or a short cluster holding `-Z`/`-C`): `has_config_or_unstable` per word. */
export const carriesRedirect = (arg) => arg === "--config" || (typeof arg === "string" && arg.startsWith("--config=")) || shortClusterCarriesZOrC(arg);

/**
 * Where the broker reads the subcommand of an argv (command_policy.rs `command_index`): after `+toolchain`, the global options and the values of
 * `--manifest-path --config -C -Z --color --target-dir`; a `--version`/`-V`/`-vV` is itself the command; null when there is none before `--`.
 */
export function brokerCommandIndex(argv) {
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === "--") return null;
    if (value.startsWith("+") && value.length > 1) continue;
    if (["--version", "-V", "-vV"].includes(value)) return index;
    if (["--manifest-path", "--config", "-C", "-Z", "--color", "--target-dir"].includes(value)) { index += 1; continue; }
    if (value.startsWith("-")) continue;
    return index;
  }
  return null;
}

/** `deny [--frozen] [--metadata-path <file>] [--config <file>] check`, the broker's `deny_check_files` shape (each once, spaced form, nothing after `check`). */
function denyCheckShape(argv, index) {
  if (argv[index] !== "deny") return false;
  const seen = new Set(); let frozen = false;
  for (let i = index + 1; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "check") return i === argv.length - 1;
    if (arg === "--frozen" && !frozen) { frozen = true; continue; }
    if (arg === "--metadata-path" || arg === "--config") {
      const value = argv[i + 1];
      if (seen.has(arg) || typeof value !== "string" || value === "" || value.startsWith("-")) return false;
      seen.add(arg); i += 1; continue;
    }
    return false;
  }
  return false;
}

/**
 * `about generate --frozen --fail [--config <file>] [--manifest-path <file>] [--all-features | --features <list>] <template>`, the broker's
 * `about_notices_shape` (cargo-license-notices.mjs cargoAboutArgs): spaced forms, each option once, one template, nothing else.
 */
function aboutNoticesShape(argv, index) {
  const tail = argv.slice(index + 1);
  if (argv[index] !== "about" || tail.length < 4 || tail[0] !== "generate" || tail[1] !== "--frozen" || tail[2] !== "--fail") return false;
  const seen = new Set(); let template = false; let features = false;
  for (let i = 3; i < tail.length; i += 1) {
    const arg = tail[i];
    if (arg === "--config" || arg === "--manifest-path" || arg === "--features") {
      const feature = arg === "--features";
      const value = tail[i + 1];
      if (seen.has(arg) || (feature && features) || typeof value !== "string" || value === "" || value.startsWith("-")) return false;
      features ||= feature; seen.add(arg); i += 1;
    } else if (arg === "--all-features" && !features) features = true;
    else if (!arg.startsWith("-") && !template) template = true;
    else return false;
  }
  return template;
}

/**
 * Whether `argv` (cargo's arguments, no `cargo`) is one of the two tool shapes whose own `--config` and `--manifest-path` are the tool's, not cargo's
 * (`cargo deny ... check`, `cargo about generate ...`): the broker's `cargo_own_args` then takes only what stands BEFORE the subcommand as cargo's own.
 * Returns the subcommand's index in `argv`, or null for every other argv.
 */
export function toolOwnArgsIndex(argv) {
  const index = brokerCommandIndex(argv);
  return index !== null && (denyCheckShape(argv, index) || aboutNoticesShape(argv, index)) ? index : null;
}

/** The exact `cargo fmt` shapes the broker admits as read-only (`fmt_check_shape`): `-p <unit> --check` or `--all --check`, nothing else. `tail` is what follows `fmt`. */
export function isFmtCheckShape(tail) {
  return (tail.length === 3 && tail[0] === "-p" && tail[1] !== "" && !tail[1].startsWith("-") && tail[2] === "--check") || (tail.length === 2 && tail[0] === "--all" && tail[1] === "--check");
}

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
  // `cargo deny ... check` and `cargo about generate ...` carry their own `--config` and `--manifest-path` after the subcommand (the broker's cargo_own_args):
  // from the subcommand on, those words ride untouched and are the tool's; only what stands before it is cargo's.
  const toolAt = toolOwnArgsIndex(original);
  for (let i = 0; i < original.length; i += 1) {
    const w = original[i];
    if (toolAt !== null && i >= toolAt) { words.push(w); continue; }
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
    if (carriesRedirect(w)) return { refusal: { code: "RK_CARGO_REDIRECT", message: `\`cargo ${original.join(" ")}\` passes ${w}, which moves its working directory, configuration or output` } };
    words.push(w);
  }
  // The broker lists no one-letter alias (b, c, t, r, d are RK_UNKNOWN_COMMAND there): the request carries the full name, and the signed argv digest is over it.
  const at = brokerCommandIndex(words);
  if (at !== null && toolAt === null && Object.hasOwn(CARGO_COMMAND_ALIASES, words[at])) words[at] = CARGO_COMMAND_ALIASES[words[at]];
  // `fmt` is read-only for the broker only in its two exact shapes; any other (`--check -- --edition 2021`, a bare `--check`) would be refused there as a must-compile.
  if (at !== null && words[at] === "fmt" && !isFmtCheckShape(words.slice(at + 1))) return { refusal: { code: "RK_CARGO_BROKER_READ_ONLY_SHAPE", message: `\`cargo ${words.join(" ")}\` is not a shape the broker carries: \`fmt\` is admitted as \`fmt -p <unit> --check\` or \`fmt --all --check\` only` } };
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
 * Where the gate's declared compile inputs are listed, as read from the snapshot (GATE-CONTRACT "Request"; the broker's COMPILE_INPUTS and
 * LEGACY_COMPILE_INPUTS, check_admission.rs:32-34): the `.rustright` spelling first, the legacy `.rightkit` one only when the first is absent.
 */
export const COMPILE_INPUTS = ".rustright/compile-inputs.json";
export const LEGACY_COMPILE_INPUTS = ".rightkit/compile-inputs.json";
export const COMPILE_INPUT_DECLARATIONS = Object.freeze([COMPILE_INPUTS, LEGACY_COMPILE_INPUTS]);
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
  const file = path.join(dir, `${uuid()}.json`);
  fs.writeFileSync(file, bytes, { flag: "wx", mode: 0o600 });
  const digest = sha256(bytes);
  const descriptor = JSON.stringify({ path: file, sha256: digest });
  if (Buffer.byteLength(descriptor) > DESCRIPTOR_MAX_BYTES) { fs.rmSync(file, { force: true }); fail("too-large", "the snapshot request descriptor exceeds 4096 bytes"); }
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
 * The declaration file of a snapshot and its name: `.rustright/compile-inputs.json`, else (only when that is absent) `.rightkit/compile-inputs.json`,
 * exactly as the broker's declared_inputs picks it. `null` when neither exists; a link on a path, or any lstat error other than a missing path, is
 * refused. Returns `{ name, at, meta }`.
 */
export function findDeclaration(snapshotRoot) {
  for (const name of COMPILE_INPUT_DECLARATIONS) {
    let at = snapshotRoot; let meta = null; let missing = false;
    for (const part of name.split("/")) {
      at = path.join(at, part);
      meta = lstatOrNull(at);
      if (meta === null) { missing = true; break; }
      if (meta.isSymbolicLink()) fail("declared-inputs", `${name} runs through a link at ${at}`);
    }
    if (!missing) return { name, at, meta };
  }
  return null;
}

/**
 * The declared inputs of a snapshot: `inputs` of `.rustright/compile-inputs.json` (else the legacy `.rightkit/compile-inputs.json`) read FROM THE
 * SNAPSHOT, never the checkout. No manifest means an empty set; a link on its path, a file that is not regular, an oversized or malformed manifest, or an
 * input that is not a plain relative path (absolute, `.`/`..`, inside `.git`, either declaration file or a directory holding one) is refused before any
 * request is made.
 */
export function readDeclaredInputs(snapshotRoot) {
  const found = findDeclaration(snapshotRoot);
  if (found === null) return [];
  const { name: file, at, meta } = found;
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
    if (overlapsDeclaration(components)) fail("declared-inputs", `${file}: ${input} overlaps the declaration file itself`);
  }
  return [...value.inputs];
}

/** Whether an input (as path components) is a declaration file's path or a directory holding one: the broker's `declaration.starts_with(input)` for BOTH spellings. */
export const overlapsDeclaration = (components) => COMPILE_INPUT_DECLARATIONS.some((declaration) => { const parts = declaration.split("/"); return components.length <= parts.length && components.every((c, i) => c === parts[i]); });

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
  if (!same(gateInputs, carrier.declaredInputs?.inputs)) fail("evidence", "the snapshot's declared inputs are not the ones this request carried");
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
  const attribution = receipt.attribution ?? null; const toolchain = receipt.toolchain ?? null;
  if (attribution !== null) {
    if (!isObject(attribution) || typeof attribution.available !== "boolean") fail("evidence", "receipt attribution is not {available, ...}");
    if (attribution.available === false && !(typeof attribution.reason === "string" && attribution.reason !== "")) fail("evidence", "receipt attribution is unavailable and names no reason");
  }
  if (toolchain !== null && !(typeof toolchain === "string" && TOOLCHAIN_FORMAT.test(toolchain))) fail("evidence", "receipt toolchain is not <toolchain>|cargo:<sha256>|rustc:<sha256>");
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
  // M39: a host enrolled at one broker version must not trust receipts of another silently. A pin that names a brokerVersion refuses a receipt whose version differs (the
  // refusal carries the upgrade warning); a pin that names none compares nothing. The verdict's `warning` is returned for the record.
  const versionCheck = compareBrokerVersion(pins, receipt.brokerVersion);
  if (!versionCheck.ok) fail("broker-version", versionCheck.warning);
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
  return { payload, evidence, attribution, toolchain, nestedChildren, brokerVersionCheck: versionCheck };
}

export { ProvenanceError };
