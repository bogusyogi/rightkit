// The one deterministic-scan module (Adrian 2026-10-22, #833 #888 #824; architecture-v3 section 4):
// every scan that applies to a change runs here, before any artifact build, local or CI.
//
//   Class A: compile-free, executes no tests and no repository-supplied plugin or generator.
//   Class B: needs the compiler or runs tests or repository code (clippy, check, doc, tsc, eslint, biome,
//            package tests, the repository's own docs generator).
//
// A required tool that is missing FAILS (status "fail", code TOOL_MISSING); a scan is never skipped.
// The one allowed gap is status "deferred": a class B scan that needs a local compile grant when the
// repository has none. A deferred scan is listed by name (result.deferred, scanDeferrals), is never a
// pass (result.ok and result.complete are false while any scan is deferred) and cannot become a SCAN_B receipt.
// Content scans run on the exported HEAD (contentRoot). Only a scan that inherently needs the live
// checkout uses liveRoot: `rustright cargo` scans (the broker binds to the registered checkout), the
// tsc, eslint, biome and package tests (need installed node_modules).
// Class B runs only when every selected class A scan passed (SCAN_B is distinct from SCAN_A and
// follows it). Rust commands go through `rustright cargo`, JavaScript tools through `pnpm exec`.
// The runner takes injectable spawn/exists/readFile/readdir so tests execute nothing real. The
// result is a plain object (per-scan status, tool versions, scanSetDigest) usable as receipt content
// (packages/release/pipeline-receipts.mjs); it carries no authority by itself.
import crypto from "node:crypto";
import { cargoWidthArgs } from "./cargo-width.mjs";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawn as spawnChild, spawnSync } from "./background-process.mjs";
import { spawnUnderOwnership } from "./build-ownership.mjs";
import { checkAdvisoryExceptions } from "./advisory-exceptions.mjs";
import { crateRunsOn, crossCompileEnv, oneOutput, scopedCrossTargets } from "./cross-targets.mjs";
import { declaredFeatureNames, effectiveCrateFeatures, featureCoverageProblems } from "./feature-coverage.mjs";
import { CLIPPY_UNIVERSAL_FEATURE_FLAGS, canonicalJson, clippyBatchPlan, clippySelectionOf, digestScanSet, requiredScanIds, rustCratePlan, rustScanId } from "./scan-digest.mjs";
import { reach } from "./lock-graph.mjs";
import { cargoManifestState } from "./cargo-manifest-text.mjs";
import { GRADLE_BUILD_FILE, gradleLockProblems, gradleOwnerOf } from "./gradle-lock.mjs";
import { PYTHON_LOCK_FILE, pythonLockProblems } from "./python-lock.mjs";
import { lockOwnerOf, packageLockProblems, xcodeProjectLockProblems } from "./swift-lock.mjs";
import { LAUNCH_GUARD_IMPORT } from "./class-a-launch-guard.mjs";
import { advisoryCacheRoot, ensureAdvisoryDb, prepareDenyOverlay, readAdvisoryPin } from "./advisory-db.mjs";
import { ghsaFindings } from "./ghsa-audit.mjs";
import { OutdatedUnavailableError, resolveOutdatedBin } from "./outdated-command.mjs";
import { isInsideGenerationTarget, observedEnvOf, readRequestRef, verifySnapshotReceipt, writeSnapshotRequest } from "./snapshot-request.mjs";
import { SCAN_TOKEN_ENV, appendLedger, readLedger, scanTokenOf, unclaimedLedgers } from "./cargo-ledger.mjs";
import { brokerSection, carryBrokerAdapterAsync, verifyBrokerLedger } from "./snapshot-broker.mjs";
import { ADMISSION_POLL_MS, CARRIED_CANCEL_GRACE_MS, QUEUED_CEILING_FACTOR, createBrokerLive, createRunClock, createStatusPoller, watchBrokerAdmission } from "./broker-admission.mjs";
import { CANCEL_WAIT_ENV, operationDeadlineEnv, refusalInCapturedOutput } from "./broker-wire.mjs";
import { removeCarrier } from "./carrier-census.mjs";

export const SCAN_RESULT_SCHEMA = "rightkit.deterministic-scans/1";
/**
 * The checker that produced a scan result: `<name>@<version>#<digest>` where the digest covers this module and scan-digest.mjs,
 * so a changed checker at the same version is a different identity. null when it cannot be computed; prior class A evidence
 * then never unlocks class B (an unknown checker is not trusted, it is refused).
 */
export const CHECKER_IDENTITY = (() => {
  try {
    const p = JSON.parse(fs.readFileSync(new URL("./package.json", import.meta.url), "utf8"));
    const h = crypto.createHash("sha256");
    for (const f of ["./deterministic-scans.mjs", "./scan-digest.mjs", "./swift-lock.mjs", "./gradle-lock.mjs", "./python-lock.mjs", "./ghsa-audit.mjs"]) { h.update(f); h.update("\0"); h.update(fs.readFileSync(new URL(f, import.meta.url))); h.update("\0"); }
    return `${p.name}@${p.version}#${h.digest("hex").slice(0, 16)}`;
  } catch { return null; }
})();
export const CHECKER_VERSION = CHECKER_IDENTITY;
export const SCAN_CLASS = Object.freeze({ A: "A", B: "B" });
/**
 * Recorded in the result of every wrapped snapshot compile (item 7 row 7.10, lead re-decision F7): cargo hands a build script its own
 * path in CARGO (measured, cargo 1.98.0), so a cargo a build script runs through $CARGO never reaches the wrapper's ledger.
 */
export const UNOBSERVED_NESTED_CARGO_NOTE = "cargo calls that a build script makes through $CARGO are unobserved: cargo hands build scripts its own path, not the gate's wrapper, so they are not in the cargo ledger; they inherit the parent's bound CARGO_TARGET_DIR, and the per-scan sentinel and the after-compile snapshot verify are the backstops";

/** The ledger token of the gate's own tool version probes (they reach the cargo wrapper like any scan does). */
const PROBE_LEDGER_TOKEN = scanTokenOf("tool-probe");

/** Scan id prefixes (the text before the first ":") that belong to each class. */
export const CLASS_A_SCAN_PREFIXES = Object.freeze([
  "rustfmt", "node-check", "json-parse", "toml-parse", "yaml-parse", "ownership-catalog",
  "actionlint", "shellcheck", "pnpm-lockfile", "swift-lock", "gradle-lock", "python-lock", "pnpm-audit", "cargo-metadata", "cargo-deny", "cargo-audit", "cargo-package-list",
  "changelog-version", "license", "secret-scan", "path-policy", "advisory-exceptions", "suite-api", "build-helpers",
  // Everything mechanical and deterministic that does not compile is class A (Adrian, 2026-10-27): typecheck, lint, generated docs.
  "tsc", "eslint", "biome", "oxlint", "generated-docs",
  // The GitHub advisory database beside RustSec (rightkit#1461), dependency freshness as advice (rightkit#1475), the Swift type-check
  // (rightkit#1289) and the Cargo feature coverage check (rightkit#1231).
  "ghsa-audit", "dependency-freshness", "swift-typecheck", "feature-coverage",
]);
// Class B includes compiles and tests that execute project code.
export const CLASS_B_SCAN_PREFIXES = Object.freeze([
  "clippy", "check", "check-all-features", "check-features", "check-dependent", "doc", "swift-build",
]);
// Test scripts can invoke a compiler indirectly, so all tests run after Class A.
export const TEST_SCAN_CLASS = Object.freeze({ "test:rust": "B", "test:rust-serial": "B", "test:npm": "B" });

/**
 * Rollout switch for the CI integration (ci/gate.config.json: { "deterministicScans": { "enabled": true } }).
 * Absent or anything but boolean true means OFF. A disabled run is never scan success or qualification evidence.
 *
 * deferredBeforeEnable (these gate ENABLING the CI integration, not shipping the dormant code):
 *  - deferred doc:* scans have no CI path yet;
 *  - changed-path / base coverage for workflow-only and package-only changes in ci.yml (the hygiene job has no --base);
 *  - pinned provisioning of cargo-deny, cargo-audit and actionlint via right-git;
 *  - class B wiring: CI's compile and test steps do not yet run through the shared runner with class A evidence.
 */
export function deterministicScansEnabled(config) {
  return config?.deterministicScans?.enabled === true;
}

let neutralDir = null;
/** An empty directory outside every repository, for tool version probes. */
function probeDir() {
  if (!neutralDir || !fs.existsSync(neutralDir)) neutralDir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-probe-"));
  return neutralDir;
}

/** JSONC to JSON: drops line and block comments outside strings, and trailing commas before } or ]. */
export function stripJsonComments(text) {
  let out = ""; let i = 0; let inString = false;
  while (i < text.length) {
    const c = text[i];
    if (inString) { out += c; if (c === "\\") { out += text[i + 1] ?? ""; i += 2; continue; } if (c === "\"") inString = false; i++; continue; }
    if (c === "\"") { inString = true; out += c; i++; continue; }
    if (c === "/" && text[i + 1] === "/") { while (i < text.length && text[i] !== "\n") i++; continue; }
    if (c === "/" && text[i + 1] === "*") { i += 2; while (i < text.length && !(text[i] === "*" && text[i + 1] === "/")) i++; i += 2; continue; }
    out += c; i++;
  }
  return out.replace(/,(\s*[}\]])/g, "$1");
}

/** "A", "B" or null for an id such as "clippy:rightkit-fs". */
export function scanClassOf(id, scan = null) {
  const parts = String(id).split(":");
  const prefix = parts[0];
  if (prefix === "test") {
    const kind = TEST_SCAN_CLASS[`test:${parts[1]}`] ?? null;
    if (kind === "A" && scan?.brokerCall) return SCAN_CLASS.B;
    return kind === null ? null : SCAN_CLASS[kind];
  }
  if (CLASS_A_SCAN_PREFIXES.includes(prefix)) return SCAN_CLASS.A;
  if (CLASS_B_SCAN_PREFIXES.includes(prefix)) return SCAN_CLASS.B;
  return null;
}

const sha256 = (value) => crypto.createHash("sha256").update(value).digest("hex");

// canonicalJson and digestScanSet live in the dependency-free scan-digest.mjs, so credentialed CI code that verifies
// scan receipts never loads this scanner.
export { canonicalJson, digestScanSet, requiredScanIds };

// ---- gate result cache --------------------------------------------------------------------------------
// A passing class B scan is reused on a later run only when every input is identical: the key is the sha256 of the
// canonical JSON of the scan id, its command text, the probed tool version, the environment the scan sets, a digest of the full environment its child inherits and an input
// digest the caller computes from the gated commit's object ids (bin/rightkit-gate.mjs). Only "pass" rows are stored
// (never fail, deferred or not-run). Class A scans are cached too, except the live checks whose answer depends on the day
// (cargo-deny, cargo-audit, pnpm-audit, pnpm-lockfile: the caller's digest returns null for them). A changed input changes the key, so
// there is no separate invalidation; a corrupt or foreign file is a miss.

// The environment names that can change what a build, lint, doc or test proves: compiler and cargo configuration, the toolchain,
// linkers and C toolchains, sccache, the RightKit/RustRight switches, CI, node and pnpm configuration. Nothing else the child
// inherits is part of the key: a session id, a socket path, a window handle or a temp directory differs from one process to the
// next and proves nothing, and keying on the whole environment made every cached result unreadable after a restart (2026-10-22).
// PATH is deliberately out: which tool runs is bound by the probed tool version in the key. RK_GATE_* (jobs, cache switches) and
// RUSTRIGHT_QUIET_LEASE (a lease id) do not change a scan's verdict.
const BUILD_ENV_NAMES = new Set(["CI", "GITHUB_ACTIONS", "CC", "CXX", "AR", "LD", "RANLIB", "RUSTC", "RUSTDOC", "RUSTFLAGS", "RUSTDOCFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
  "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "CARGO_HOME", "CARGO", "TARGET", "HOST", "NODE_OPTIONS", "NODE_ENV", "SOURCE_DATE_EPOCH", "MACOSX_DEPLOYMENT_TARGET", "SDKROOT", "DEVELOPER_DIR",
  "VCINSTALLDIR", "LIB", "INCLUDE", "LIBPATH", "CFLAGS", "CXXFLAGS", "LDFLAGS", "CPPFLAGS", "PKG_CONFIG_PATH", "PKG_CONFIG_LIBDIR", "OPENSSL_DIR", "OPENSSL_LIB_DIR", "OPENSSL_INCLUDE_DIR",
  "LIBCLANG_PATH", "LLVM_CONFIG_PATH", "ORT_LIB_LOCATION", "ORT_STRATEGY", "PROTOC", "WEBVIEW2_", "TAURI_"]);
const BUILD_ENV_PREFIXES = ["CARGO_", "RUST", "SCCACHE_", "RK_", "RIGHTKIT_", "RUSTRIGHT_", "PNPM_", "NPM_CONFIG_", "CMAKE_", "CFLAGS_", "CXXFLAGS_", "OPENSSL_", "ORT_", "ONNX", "TAURI_", "WEBVIEW2_", "WINDOWS_SDK", "MSVC"];
// Per-run identities never key a result (they change on every run, so the cache could never hit; measured 2026-11-02: HR Class A wrote
// 3,809 entries and hit none across identical runs): the run's ownership handle, the attest nonce, the request-id file, the carried call's
// snapshot request and cargo map, the direct receipt path.
// A network proxy (NPM_CONFIG_HTTPS_PROXY, NPM_CONFIG_NOPROXY, ...) changes how a download is routed, never what a scan proves, and a
// cloud host sets it per session (measured 2026-11-06: the cloud's NPM_CONFIG_HTTPS_PROXY moved between two runs and 1,115 rows missed).
const BUILD_ENV_EXCLUDED = /^(RK_GATE_|RUSTRIGHT_QUIET_LEASE$|RIGHTKIT_BUILD_OWNERSHIP$|RIGHTKIT_CARRIED_CARGO_MAP$|RUSTRIGHT_ATTEST_NONCE$|RUSTRIGHT_REQUEST_ID_FILE$|RUSTRIGHT_SNAPSHOT_REQUEST$|RK_DIRECT_RECEIPT$|RIGHTKIT_CROSS_CC_DIR$|.*PROXY)/;
// PNPM_HOME (where pnpm keeps its own binaries; the calling app profile picks it) is not an input of any scan and is to leave this key once every row's stored index carries
// its environment shape (a row that only hits never refreshed its index; excluding it first made 74 rows that had no shape miss; Mac 2026-11-07).
// RIGHTKIT_CROSS_CC_DIR is where the zig wrappers live, not what they are: a cross clippy row keys on its own CC_/CXX_/AR_ variables
// (scan env), and no Class A row uses it (measured 2026-11-06: setting it on the cloud host made 1,121 Class A rows miss).
// Per-run parts of a value are normalised: the runtime cargo wrapper's fresh parent (`rk-gate-shim-XXXXXX`) and its folder name
// (`rk-cargo-shim-<identity>`), whose identity hashes the run's broker section and so differs on every run (measured 2026-11-03: two identical
// Class A runs of rightkit-asr shared no key, only CARGO differed). The wrapper's CODE keys the result through the gate identity instead
// (the gate package files in the Class A environment digest; gateCodeIdentity for class B), so a changed wrapper still misses.
const normalizeRunTemp = (value) => String(value).replace(/rk-gate-shim-[A-Za-z0-9]+/g, "rk-gate-shim-*").replace(/rk-cargo-shim-[0-9a-f]+/g, "rk-cargo-shim-*");
export const buildEnvName = (name) => {
  const upper = String(name).toUpperCase();
  if (BUILD_ENV_EXCLUDED.test(upper)) return false;
  return BUILD_ENV_NAMES.has(upper) || BUILD_ENV_PREFIXES.some((prefix) => upper.startsWith(prefix));
};

/** sha256 of the build-relevant part (buildEnvName) of the environment a scan's child receives. Pass the merged `{ ...deps.env, ...scan.env }`. */
/** The build-relevant variables of `env` as {NAME: sha256 of the normalised value}: miss evidence names which ones moved, never their values. */
export function inheritedEnvShape(env = {}) {
  return Object.fromEntries(Object.entries(env).filter(([name, value]) => value !== undefined && value !== null && buildEnvName(name)).map(([name, value]) => [String(name).toUpperCase(), sha256(normalizeRunTemp(value)).slice(0, 16)]).sort((x, y) => (x[0] < y[0] ? -1 : 1)));
}

export function inheritedEnvDigest(env = {}) {
  const rows = Object.entries(env).filter(([name, value]) => value !== undefined && value !== null && buildEnvName(name)).map(([name, value]) => [String(name).toUpperCase(), normalizeRunTemp(value)]);
  rows.sort((x, y) => (x[0] < y[0] ? -1 : x[0] > y[0] ? 1 : 0));
  return sha256(canonicalJson(rows));
}

/**
 * The cache key for one scan; `digest` is the caller's input digest, `env` the scan's own environment and `inheritedEnv`
 * the digest of the whole environment the child receives (inheritedEnvDigest). A key without the inherited digest is refused.
 */
export function scanCacheKey({ id, command, toolVersion, env = {}, inheritedEnv, digest }) {
  if (typeof inheritedEnv !== "string" || !/^[0-9a-f]{64}$/.test(inheritedEnv)) throw new TypeError("scanCacheKey: inheritedEnv must be the inheritedEnvDigest of the child environment");
  return sha256(canonicalJson({ id: String(id), command: String(command), toolVersion: String(toolVersion), env: { ...env }, inheritedEnv, digest: String(digest) }));
}

const CACHE_KEY_SHAPE = /^[0-9a-f]{64}$/;

/** A cache directory (`<git dir>/rightkit-gate-cache`): { read(key) => entry | null, write(key, row) }. Never throws. */
export function fileScanCache(dir, { now = () => new Date(), fsApi = fs } = {}) {
  const file = (key) => (CACHE_KEY_SHAPE.test(key) ? path.join(dir, `${key}.json`) : null);
  return {
    read(key) {
      try { const f = file(key); return f ? JSON.parse(fsApi.readFileSync(f, "utf8")) : null; } catch { return null; }
    },
    write(key, row) {
      try {
        const f = file(key); if (!f) return;
        fsApi.mkdirSync(dir, { recursive: true });
        const tmp = `${f}.${process.pid}.${crypto.randomBytes(4).toString("hex")}.tmp`;
        fsApi.writeFileSync(tmp, JSON.stringify({ key, at: now().toISOString(), row }));
        fsApi.renameSync(tmp, f);
      } catch { /* a cache that cannot be written is only a miss next time */ }
    },
  };
}

// ---- command builders shared with scripts/ci/gate.mjs and packages/release/local-publish.mjs -------------

const packageFlags = (units) => [].concat(units).flatMap((unit) => ["-p", unit]);

/** `cargo clippy -p <units> [--message-format=...] --no-deps --all-targets [--target <t>] <features> [--locked] -- -D warnings` (argv after `cargo`). */
export function clippyArgs(units, { features = [], messageFormat, noDeps = true, locked = false, target = null } = {}) {
  return ["clippy", ...packageFlags(units), ...(messageFormat ? [`--message-format=${messageFormat}`] : []), ...(noDeps ? ["--no-deps"] : []), "--all-targets", ...(target ? ["--target", target] : []), ...features, ...(locked ? ["--locked"] : []), "--", "-D", "warnings"];
}

/** `cargo check -p <unit> [--message-format=...] --all-targets <features> [--locked]`. */
export function checkArgs(unit, { features = [], messageFormat, locked = false } = {}) {
  return ["check", "-p", unit, ...(messageFormat ? [`--message-format=${messageFormat}`] : []), "--all-targets", ...features, ...(locked ? ["--locked"] : [])];
}

/** `cargo doc -p <unit> --no-deps --all-features --locked`; run it with RUSTDOCFLAGS=DOC_RUSTDOCFLAGS (CI). */
// Every compile reports every failure (Adrian, 2026-10-29: "even to ship, why isn't it better to get all errors and fix them all
// wholesale?"; rightkit#1293): a compile keeps going past a failed crate (`--keep-going`) and a test run past a failed test binary
// (`--no-fail-fast`). The run still fails if anything failed; the crates that compiled stay warm for the next run.
// Cargo 1.98: build, check, clippy (through its cargo check) and doc take `--keep-going`; test and bench reject it and take
// `--no-fail-fast` instead (the broker's BR29 adds the same split).
const KEEP_GOING_SUBCOMMANDS = new Set(["build", "check", "clippy", "doc"]);
const NO_FAIL_FAST_SUBCOMMANDS = new Set(["test", "bench"]);
export function everyFailure(args) {
  const [sub, ...rest] = args;
  const until = rest.indexOf("--"); const own = until === -1 ? rest : rest.slice(0, until);
  const flag = KEEP_GOING_SUBCOMMANDS.has(sub) ? "--keep-going" : NO_FAIL_FAST_SUBCOMMANDS.has(sub) ? "--no-fail-fast" : null;
  return flag && !own.includes(flag) ? [sub, flag, ...rest] : args;
}
export function docArgs(unit) { return ["doc", "-p", unit, "--no-deps", "--all-features", "--locked"]; }
export const DOC_RUSTDOCFLAGS = "-D warnings";
/** The same check as rustdoc arguments (`cargo rustdoc ... -- -D warnings`), for the broker path. */
export const DOC_RUSTDOC_ARGS = Object.freeze(["-D", "warnings"]);

/** `cargo fmt --check` for one crate, or the whole workspace when `unit` is null. */
export function fmtCheckArgs(unit = null) { return unit ? ["fmt", "-p", unit, "--check"] : ["fmt", "--all", "--check"]; }

/** `cargo metadata` that validates manifests and the lock without resolving anything new. */
export function metadataArgs({ noDeps = true } = {}) { return ["metadata", ...(noDeps ? ["--no-deps"] : []), "--format-version", "1", "--locked"]; }

/** Translate one authenticated publisher requirement into a normal Class B scan. */
export function publisherRequirementScan(requirement, { platform = process.platform, nativeArch = process.arch, cargoCommand = ["cargo"], cargoLiveRoot = null, targetDirs = null, quiet = false, allowIgnored = false, cargoJobs = null } = {}) {
  const { id, workspace, cwd, executable, argv, env, phase, coverageKey } = requirement ?? {};
  if (typeof id !== "string" || !id || typeof coverageKey !== "string" || !coverageKey || !Array.isArray(argv) || !argv.every((arg) => typeof arg === "string") || !env || typeof env !== "object" || Array.isArray(env) || !["compile", "test"].includes(phase)) throw new TypeError("invalid publisher requirement");
  const plainDir = (value) => typeof value === "string" && /^(?:\.|[A-Za-z0-9_@+.-]+(?:\/[A-Za-z0-9_@+.-]+)*)$/.test(value) && !value.split("/").includes("..");
  if (!plainDir(workspace) || !plainDir(cwd) || !["cargo", "pnpm"].includes(executable) || !Array.isArray(cargoCommand) || !cargoCommand.length || !cargoCommand.every((part) => typeof part === "string" && part)) throw new TypeError("invalid publisher requirement command or workspace");
  const isRustTest = executable === "cargo" && phase === "test" && argv[0] === "test";
  let args = [...argv]; let runOutside;
  if (isRustTest && cargoLiveRoot) {
    const separator = args.indexOf("--");
    const runArgs = separator < 0 ? [] : args.slice(separator + 1);
    args = (separator < 0 ? args : args.slice(0, separator)).filter((arg) => arg !== "--no-fail-fast" && !arg.startsWith("--message-format="));
    args.push("--message-format=json");
    runOutside = { args: runArgs };
  }
  if (executable === "cargo") {
    if (cargoJobs !== null) args = cargoWidthArgs(args, cargoJobs);
    args = oneOutput(isRustTest && cargoLiveRoot ? args : everyFailure(args), { platform, arch: nativeArch });
  }
  const scan = { id, coverageKey, ...(executable === "cargo" ? { executionWorkspace: workspace } : {}), publisherPhase: phase, class: SCAN_CLASS.B, root: executable === "cargo" && cargoLiveRoot ? "cargoLive" : "live", cwd, tool: executable === "cargo" ? cargoCommand[0] : executable, args: executable === "cargo" ? [...cargoCommand.slice(1), ...args] : args, env: { ...env, ...(targetDirs?.[workspace] ? { CARGO_TARGET_DIR: targetDirs[workspace] } : {}) }, ...(quiet && phase === "test" ? { quiet: true } : {}), ...(runOutside ? { runOutside } : {}), ...(isRustTest ? { publisherRustTest: true, allowIgnored } : {}), ...(executable === "pnpm" && phase === "test" ? { kind: "test" } : {}) };
  // Do not render the inherited publisher environment into logs or receipts.
  scan.command = commandText({ ...scan, env: {} });
  return scan;
}

/** Deduplicate concurrent identical probes; transient outcomes are never retained. */
// The command text a cache key binds. Cargo's build width (`--jobs N`, from --cargo-jobs) changes how many processes compile, never what compiles or
// a verdict, so it is not part of the key: a row passed at one width is the same row at another (Mac 2026-11-07, run 17 --cargo-jobs 6: all 295
// stored passes missed as 'nothing changed: the cache failed').
/** Whether a stored row's command is this scan's command: the same text once Cargo's build width is dropped (67515ffb dropped it from the key but
 * the lookup still compared the raw text, so a different --jobs missed every stored pass). */
export function sameCommand(stored, s) { return keyCommand({ command: stored, runOutside: s.runOutside }) === keyCommand(s); }
export function keyCommand(s) {
  const base = String(s.command ?? "").replace(/(^|\s)--jobs\s+\d+(?=\s|$)/g, "");
  return s.runOutside ? `${base} ;; run ${canonicalJson(s.runOutside)}` : base;
}

export function singleFlightProbe(probe) {
  const pending = new Map(); const completed = new Map();
  return async (key, ...args) => {
    if (completed.has(key)) return completed.get(key);
    if (pending.has(key)) return pending.get(key);
    const job = Promise.resolve().then(() => probe(...args));
    pending.set(key, job);
    try { const outcome = await job; if (!outcome?.transient) completed.set(key, outcome); return outcome; }
    finally { pending.delete(key); }
  };
}

// ---- Clippy batching (GATE-CONTRACT §1 "Clippy route & batching"; item 8 rows 8.6, 8.8, 8.15) -----------------------------------------

// The batch planner itself (clippyBatchPlan, clippySelectionOf, CLIPPY_UNIVERSAL_FEATURE_FLAGS) lives in scan-digest.mjs, beside the scan
// ids, and is re-exported here.
export { CLIPPY_UNIVERSAL_FEATURE_FLAGS, clippyBatchPlan, clippySelectionOf };

/**
 * Does this crate's directory declare sidecars its build needs staged first? A Tauri crate with `bundle.externalBin` refuses to build
 * until its sidecars are in place (bin/rightkit-gate.mjs, the compile stage), so it is scheduled after the independent crates. The
 * declaration is read from the crate's own Tauri configuration (JSON, JSON5 or TOML), in its directory or its `src-tauri`.
 */
export function sidecarBoundCrate({ dir, exists = fs.existsSync, readFile = (f) => fs.readFileSync(f, "utf8") }) {
  for (const base of [dir, path.join(dir, "src-tauri")]) {
    for (const name of ["tauri.conf.json", "tauri.conf.json5", "Tauri.toml"]) {
      const file = path.join(base, name);
      let body;
      try { if (!exists(file)) continue; body = readFile(file); } catch { continue; }
      try {
        const conf = JSON.parse(body);
        const list = conf?.bundle?.externalBin ?? conf?.tauri?.bundle?.externalBin;
        if (Array.isArray(list) && list.length) return { bound: true, file, externalBin: list };
        continue;
      } catch { /* not strict JSON: read as text */ }
      if (/(^|[\s{,"'])externalBin["']?\s*[:=]\s*\[\s*["']/.test(body)) return { bound: true, file, externalBin: null };
    }
  }
  return { bound: false };
}

// ---- plan ---------------------------------------------------------------------------------------------

const posix = (p) => p.replaceAll("\\", "/");
// Crate and package directory names reach argv (and, on Windows, possibly a .cmd shim); accept only
// plain names. Any other name is refused: it becomes a failing path-policy scan and is never run.
const SAFE_UNIT_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const ABSOLUTE_PATH = /^(?:[A-Za-z]:|[\\/])/;
/** Normalized posix form of a changed path, or { refused } when it is absolute, escapes the root, or is unusable. */
function containedPath(raw, contentRoot) {
  const text = String(raw);
  if (text === "" || text.includes("\0")) return { refused: "empty or contains a NUL" };
  const unified = posix(text);
  if (ABSOLUTE_PATH.test(unified) || path.isAbsolute(text) || path.win32.isAbsolute(text)) return { refused: "absolute path" };
  const norm = path.posix.normalize(unified);
  if (norm === ".." || norm.startsWith("../")) return { refused: "escapes the content root" };
  const rel = path.relative(path.resolve(contentRoot), path.resolve(contentRoot, norm));
  if (rel === ".." || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel)) return { refused: "escapes the content root" };
  return { path: norm };
}
const PNPM_NO_HOOKS_ENV = Object.freeze({ npm_config_ignore_pnpmfile: "true", npm_config_ignore_scripts: "true", pnpm_config_ignore_pnpmfile: "true", pnpm_config_ignore_scripts: "true" });
// pnpm 11 checks, before every `pnpm run` and `pnpm exec`, that node_modules matches pnpm-lock.yaml (`verify-deps-before-run`, default
// `install`) and answers a stale tree by running `pnpm install` in it. That install wants to purge the modules directory and, with no TTY (a
// gate child has none), aborts: `[ERR_PNPM_ABORTED_REMOVE_MODULES_DIR_NO_TTY] Aborted removal of modules directory due to no TTY` and a
// stack through runDepsStatusCheck, before the tool or the test runs at all (CodeRight, 2026-10-25: tsc, biome and test:npm failed in 2 s on a
// tree installed before the last lockfile change). A gate scan must not install into, or purge, the checkout it reads, and the lockfile is
// verified where it belongs, by `pnpm-lockfile` (`install --frozen-lockfile --lockfile-only`) in the clean export, so the class B pnpm scans
// turn the check off and run against the tree as installed, as pnpm 10 (default `false`) always did; a tree that is really missing something
// fails in the tool or the test with its own error. The `pnpm_config_` spelling is the one pnpm 11.25 honours for this setting
// (`npm_config_verify_deps_before_run=false` still aborted, checked on a stale tree), the lowercase spelling PNPM_NO_HOOKS_ENV uses for its settings.
const PNPM_RUN_ENV = Object.freeze({ pnpm_config_verify_deps_before_run: "false" });

// The placeholder a tsc row passes as its project when its package depends on a workspace sibling (planScans; runScans replaces it).
export const TSC_PROJECT = "{tscProject}";
const DIST_TYPES = /^\.\/dist\/(.+)\.d\.ts$/;
/**
 * Writes the project a tsc row checks through: it extends the package's own tsconfig (its include, exclude and options, resolved from
 * there), emits nothing, widens rootDir to the checked tree, keeps the package's own @types lookup chain (typeRoots), and maps every
 * exported entry point of each workspace sibling whose types
 * are built output (`types: ./dist/<x>.d.ts`) to that sibling's committed source (`src/<x>.ts`, `.tsx`, `/index.ts`, `/index.tsx`).
 * `workspace` is the siblings' tree-relative directories. Returns { dir, file }; the caller removes `dir`. Throws when a sibling's
 * manifest cannot be read or an exported entry point has no committed source: such a sibling could only be checked against a build.
 */
export function writeTscProject({ contentRoot, packageDir, workspace, tmpdir = os.tmpdir() }) {
  const paths = {};
  for (const rel of workspace) {
    const dir = path.join(contentRoot, rel);
    let manifest;
    try { manifest = JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8").replace(/^﻿/, "")); }
    catch (e) { throw new Error(`${rel}/package.json: ${e.message}`); }
    if (typeof manifest.name !== "string" || path.resolve(dir) === path.resolve(packageDir)) continue;
    const exported = manifest.exports && typeof manifest.exports === "object" ? Object.entries(manifest.exports) : [];
    for (const [sub, target] of exported) {
      // Types sit on the target, or under one export condition (`development` for a test-only subpath, then `default`).
      const types = target && typeof target === "object" ? (target.types ?? target.development?.types ?? target.default?.types ?? null) : null;
      const m = typeof types === "string" ? DIST_TYPES.exec(types) : null;
      if (!m) continue;
      // A subpath pattern (`./*` -> `./dist/*.d.ts`) maps as a TypeScript paths pattern onto the same pattern in src; the sibling
      // must commit a src directory for it to resolve against.
      if (sub.includes("*")) {
        if (!sub.endsWith("*") || (sub.match(/\*/g) ?? []).length !== 1 || (m[1].match(/\*/g) ?? []).length !== 1) throw new Error(`${manifest.name} exports ${sub} with types ${types}: only one trailing * pattern maps to src`);
        if (!fs.existsSync(path.join(dir, "src"))) throw new Error(`${manifest.name} exports ${sub} with types ${types}, and ${rel}/src does not exist`);
        paths[`${manifest.name}${sub.slice(1)}`] = [`src/${m[1]}.ts`, `src/${m[1]}.tsx`, `src/${m[1]}/index.ts`, `src/${m[1]}/index.tsx`].map((f) => path.join(dir, f));
        continue;
      }
      const source = [`src/${m[1]}.ts`, `src/${m[1]}.tsx`, `src/${m[1]}/index.ts`, `src/${m[1]}/index.tsx`].map((f) => path.join(dir, f)).find((f) => fs.existsSync(f));
      if (!source) throw new Error(`${manifest.name} exports ${sub} with types ${types}, and ${rel}/src has no committed source for it`);
      paths[sub === "." ? manifest.name : `${manifest.name}${sub.slice(1)}`] = [source];
    }
  }
  // TypeScript looks type libraries up (`types`, and every @types package by default) in node_modules/@types of the CONFIG's directory
  // and its ancestors. The project lives outside the tree, so it names the chain the package's own tsconfig would have used.
  const typeRoots = [];
  for (let d = path.resolve(packageDir); ; d = path.dirname(d)) {
    const at = path.join(d, "node_modules", "@types");
    if (fs.existsSync(at)) typeRoots.push(at);
    if (path.dirname(d) === d) break;
  }
  const out = fs.mkdtempSync(path.join(tmpdir, "rk-tsc-"));
  const file = path.join(out, "tsconfig.json");
  fs.writeFileSync(file, `${JSON.stringify({ extends: path.join(packageDir, "tsconfig.json"), compilerOptions: { noEmit: true, rootDir: path.resolve(contentRoot), typeRoots, paths } }, null, 2)}\n`);
  return { dir: out, file };
}
const CONFIG_CRATE_FILE = /(^|\/)Cargo\.(toml|lock)$/;
const JS_TOOLING_FILE = /(^|\/)(package\.json|pnpm-lock\.yaml|pnpm-workspace\.yaml)$/;
// Root files that change how EVERY crate (or package) builds: a change to one puts every unit in scope,
// so a dependency or toolchain update never reaches a push without local qualification.
// `.cargo/audit.toml` is cargo-audit policy (an advisory ignore list), read by no compiler: it is ANCESTOR_DENY_FILE below.
export const WORKSPACE_RUST_FILE = /^(Cargo\.(toml|lock)|rust-toolchain(\.toml)?|\.?clippy\.toml|\.?rustfmt\.toml|\.cargo\/(?!audit\.toml$).+|vendor\/.+)$/;
// What cargo, clippy, rustfmt and cargo-deny read from EVERY ancestor directory of a workspace (cargo merges `.cargo/config*` and
// finds the toolchain file, clippy and rustfmt search upward): a change to one at any ancestor of a workspace, the workspace's own
// root included, puts that workspace's whole crate set in scope. Patterns are relative to the ancestor directory.
export const ANCESTOR_RUST_CONFIG_FILE = /^(rust-toolchain(\.toml)?|\.?clippy\.toml|\.?rustfmt\.toml|\.cargo\/(?!audit\.toml$).+)$/;
// cargo-deny reads deny.toml and cargo-audit reads .cargo/audit.toml from the workspace root and its ancestors, but neither changes
// compilation: a change anywhere above a workspace runs that workspace's metadata, cargo-deny and cargo-audit scans (the deny scan
// covers the workspace's whole crate set), not its compile or test scans.
export const ANCESTOR_DENY_FILE = /^(deny\.toml|\.cargo\/audit\.toml)$/;
/** The directories whose Rust configuration reaches workspace `ws`: "." (the repository root), every directory between, and `ws` itself. */
export const ancestorDirsOf = (ws) => {
  const parts = ws === "." ? [] : String(ws).split("/").filter(Boolean);
  return [".", ...parts.map((_, i) => parts.slice(0, i + 1).join("/"))];
};
/** True when a changed repo-relative file is Rust configuration of some ancestor directory (or the root) of workspace `ws`. */
export const ancestorConfigChanged = (ws, changed, pattern = ANCESTOR_RUST_CONFIG_FILE) => ancestorDirsOf(ws).some((dir) => changed.some((f) => {
  const rel = dir === "." ? f : f.startsWith(`${dir}/`) ? f.slice(dir.length + 1) : null;
  return rel !== null && pattern.test(rel);
}));
// Genuinely shared inputs of a repository whose every crate is its own Cargo workspace (rightkit#1285): no crate directory holds them,
// so a change to one reaches every unit. Anything else outside a crate directory reaches no crate (a vendored source maps to the crates
// that patch or path-depend on it through `externalInputs`; a repository-level toolchain, .cargo, clippy or rustfmt file is an
// ANCESTOR_RUST_CONFIG_FILE of every workspace). The gate configuration, the pipeline's toolchain pins and the gate itself are named here.
export const SHARED_RUST_SCOPE_FILE = /^(ci\/gate\.config\.json|ci\/rk-pipeline\.json|packages\/release\/(bin\/rightkit-gate|deterministic-scans|scan-digest|advisory-exceptions|crate-workspaces|lock-graph|cargo-target-shim)\.mjs)$/;
const WORKSPACE_JS_FILE = /^(package\.json|pnpm-lock\.yaml|pnpm-workspace\.yaml|\.npmrc)$/;
// The linter a package has is the one it configures. ESLint reads a flat `eslint.config.*` or a legacy `.eslintrc*` from the package directory or
// any ancestor of it; Biome reads `biome.json` / `biome.jsonc` the same way. A Biome-only app (ViewRight, ScrapeRight: `"lint": "biome lint ."`)
// has no ESLint config and no ESLint dependency, so `eslint .` there fails with "couldn't find an eslint.config" (a gate false positive):
// planScans plans eslint only where an ESLint config exists and biome only where a Biome config exists, both where both do.
export const ESLINT_CONFIG_FILES = Object.freeze([
  "eslint.config.js", "eslint.config.mjs", ".eslintrc.json",
  "eslint.config.cjs", "eslint.config.ts", "eslint.config.mts", "eslint.config.cts",
  ".eslintrc", ".eslintrc.js", ".eslintrc.cjs", ".eslintrc.yaml", ".eslintrc.yml",
]);
export const BIOME_CONFIG_FILES = Object.freeze(["biome.json", "biome.jsonc"]);
// Oxlint reads `.oxlintrc.json` (JSON with comments) or an `oxlint.config.*` from the directory it runs in (rightkit#1402). It is JS/TS only (Rust stays Clippy)
// and never replaces ESLint or Biome: a package with several of these configs runs every one of them, so no unmatched rule is silently dropped.
export const OXLINT_CONFIG_FILES = Object.freeze([".oxlintrc.json", "oxlint.config.ts", "oxlint.config.mts", "oxlint.config.cts", "oxlint.config.js", "oxlint.config.mjs", "oxlint.config.cjs"]);
// Everything scripts/generate-index.mjs reads or writes (its header and loadInventory): crate and package manifests and
// READMEs, the root Cargo.toml (workspace version), the versions and ownership data, the ownership-availability helper it imports, itself,
// the port matrix generator it imports, and its outputs (the port matrix's MD and HTML are checked for consistency with its JSON).
// The Right Suite API contract and every projection scripts/gen-suite-api.mjs writes from it (the test-vector copy included).
const SUITE_API_INPUT = /^(contracts\/right-suite-api\.v1\.json|scripts\/gen-suite-api\.mjs|crates\/rightkit-license\/src\/suite_api_generated\.rs|packages\/app-update\/src\/suite-api\.generated\.ts|packages\/release\/suite-api\.generated\.mjs|swift\/RightKitSwift\/Sources\/RightKitLicense\/RightSuiteAPI\.generated\.swift)$/;
const GENERATOR_INPUT = /^(crates\/[^/]+\/(Cargo\.toml|README\.md)|packages\/[^/]+\/(package\.json|README\.md)|Cargo\.toml|packages\/release\/(rightkit-versions\.json|rightkit-ownership\.json|ownership-availability\.mjs)|scripts\/generate-(index|port-matrix)\.mjs|INDEX\.md|docs\/(ADOPTION|CAPABILITY-MAP|PORT-MATRIX)\.md|docs\/port-matrix\.(html|json))$/;
const PROBES = Object.freeze({
  rustfmt: ["--version"], node: ["--version"], rustright: ["cargo", "--version"], pnpm: ["--version"],
  actionlint: ["-version"], shellcheck: ["--version"], oxlint: ["--version"], "right-git": ["--version"], swift: ["--version"],
  "cargo-deny": ["--version"], "cargo-audit": ["audit", "--version"], // as cargo itself starts the subcommand binaries
});
/**
 * The one version of each analyzer whose verdict class A accepts. A class A finding must not depend on which build of the
 * analyzer a host happens to have: ShellCheck 0.10.0 fails SellRight's repaired deploy scripts on SC2015 and a documented
 * `# shellcheck disable=SC2329` on SC2317 (0.11.0 split that code), so the same commit was red on one host and green on another.
 * A scan (or a scan's `requires`) whose tool answers another version fails with TOOL_VERSION; it is never passed or skipped.
 * The installers (.github/workflows/publish.yml, publish-app.yml, ci.yml) and the managed host path in ci/rk-pipeline.json
 * provision exactly this version; scripts/ci/scan-receipt.test.mjs holds them to it.
 */
export const PINNED_SCAN_TOOLS = Object.freeze({ shellcheck: "0.11.0", oxlint: "1.89.0" });
/** Scans whose answer depends on a remote service at run time, not only on their inputs: `pnpm-audit` asks the registry's advisory
 * service. A verdict that includes them is reusable only after they are re-run (`rightkit-gate live-scans`; GATE-CONTRACT §1, E18),
 * and a red caused by one is never deterministic. Ids are `<prefix>` or `<prefix>:<root>`; rustright's REMOTE_SCANS mirrors this list. */
export const LIVE_SCAN_IDS = Object.freeze(["pnpm-audit", "ghsa-audit"]);
/** Internal rules whose verdict is a function of the files they read and the rule code only (GC-A24): reused by input key. */
// ghsa-audit and advisory-exceptions are keyed by lock, files and UTC day (GATE-CONTRACT GC-A24 table, GC-A19 keys), so only a same-day pass is reused.
export const CACHEABLE_INTERNAL = new Set(["ghsa-audit", "advisory-exceptions", "feature-coverage", "secret-scan", "json-parse", "toml-parse", "yaml-parse", "license", "changelog-version", "ownership-catalog", "swift-lock", "gradle-lock", "python-lock"]);
/** NN41: internal rules whose failure is never reused: their answer depends on the UTC day or a remote service, not only on their inputs. */
export const FAILURE_UNCACHEABLE_INTERNAL = new Set(["ghsa-audit", "advisory-exceptions"]);
/** NN41: the key namespace of a cached failure, so a failure never shares a key with a pass of the same scan. */
// /2: a broker-authenticated compile failure is stored only when its output shows compiler diagnostics and no tool or environment failure (a build-script tool panic is environmental; Dell pulse-hub, 2026-11-06).
// /3 (2026-11-06): a missing system library (pkg-config, system-deps) is the host, not the inputs; failures stored under /2 may be one.
/** planScans file digests by file identity (path, inode, size, mtime) for this process: one run plans its stages over the same snapshot. */
const PLAN_DIGEST_MEMO = new Map();
export const FAILURE_KEY_SCHEMA = "deterministic-failure/3";
/** NN23: the version of the internal (file-only) rules; bump when any internal rule's verdict for the same inputs changes. */
export const INTERNAL_RULES_SCHEMA = "rightkit.internal-rules/1";
/** Why a row's cache key moved, from the stored pass's key parts to this run's: the input entries that differ (paths and the like), the tool
 * version, the environment variables that differ. Bounded lists; evidence for Adrian's approval of a miss. */
/** A key's raw inputs as {path-like entry: value} (the files list entries become `files.N/<path>`), with the key suffix as "(key suffix)". */
export function flatKeyInputs(text) { const out = new Map(); const walk = (v, at) => { if (v && typeof v === "object") { if (Array.isArray(v) && v.length === 2 && typeof v[0] === "string" && typeof v[1] !== "object") { out.set(`${String(at).replace(/\.\d+$/, "")}/${v[0]}`, String(v[1])); return; } for (const [k, x] of Object.entries(v)) walk(x, at ? `${at}.${k}` : k); } else out.set(at, String(v)); }; const s = String(text); const m = /^([\[{][\s\S]*[\]}]):([^:]+)$/.exec(s); try { walk(JSON.parse(m ? m[1] : s), ""); if (m) out.set("(key suffix)", m[2]); } catch { out.set("inputs", s); } return out; }
const isBlobId = (v) => typeof v === "string" && /^[0-9a-f]{40}([0-9a-f]{24})?$/.test(v);
/**
 * Carry-over (Adrian 2026-11-07: a key change must not rerun or ask for approval what did not change). A stored pass keyed under an earlier key FORM is
 * the same result under the new key when the row's real inputs provably did not change: the same tool and scan environment; inherited variables
 * that differ only in names the key no longer includes (buildEnvName false now); and every input entry that differs going from a file's content id
 * (the old form) to a value derived from that file (the new form, e.g. a package.json's "type") while the file's content id NOW is still the old
 * one. Anything else (an edited file, an entry added or removed, a schema suffix change, a tool or environment change) is not carried.
 */
export function carryOverProof(before, after, { entryNow }) {
  const no = (why) => ({ ok: false, why });
  if (!before || !after) return no("no stored key parts");
  if (before.toolVersion !== after.toolVersion) return no("tool changed");
  // The scan's own environment is compared on the names that can change a result (buildEnvName): a publisher row once keyed the calling session's whole environment.
  const relevant = (env) => Object.fromEntries(Object.entries(env ?? {}).filter(([k]) => buildEnvName(k)));
  if (canonicalJson(relevant(before.env)) !== canonicalJson(relevant(after.env))) return no("scan environment changed");
  if (before.inherited !== after.inherited) {
    if (!before.inheritedShape || !after.inheritedShape) return no("inherited environment changed (no shape to compare)");
    const names = [...new Set([...Object.keys(before.inheritedShape), ...Object.keys(after.inheritedShape)])].filter((k) => before.inheritedShape[k] !== after.inheritedShape[k]);
    const kept = names.filter((k) => buildEnvName(k));
    if (kept.length) return no(`inherited environment changed: ${kept.slice(0, 5).join(", ")}`);
  }
  const a = flatKeyInputs(before.inputs ?? ""); const b = flatKeyInputs(after.inputs ?? "");
  const changed = [...new Set([...a.keys(), ...b.keys()])].filter((k) => a.get(k) !== b.get(k));
  const carried = []; const edited = []; const file0 = (k) => k.slice(k.indexOf("/") + 1);
  for (const k of changed) {
    if (!a.has(k) || !b.has(k)) return no(`input entry ${k} added or removed`);
    if (k === "(key suffix)" || k === "inputs") return no(`${k} changed`);
    const was = a.get(k); const now = b.get(k);
    // A file edit (one content id becoming another): the row's own file changed. It is never carried, but it is recorded so the caller can tell a
    // row whose only other differences are proven form changes (it must rerun, as any edited row does, with no approval) from an unexplained miss.
    if (isBlobId(was) && isBlobId(now)) { edited.push(file0(k)); continue; }
    if (!isBlobId(was) || isBlobId(now)) return no(`${k}: not a content id becoming a derived value`);
    const file = k.slice(k.indexOf("/") + 1);
    let id = null; try { id = entryNow(file); } catch { id = null; }
    if (id !== was) return no(`${k}: the file changed since the stored pass`);
    carried.push(file);
  }
  // ok: the stored pass IS this row's pass (nothing real changed). edited: the row's own files changed and everything else is a proven form change,
  // so it reruns like any edited row (contentOnly), with no approval.
  return edited.length ? { ok: false, why: "own files edited", edited, carried } : { ok: true, carried };
}

export function explainKeyChange(before, after) {
  const flat = (text) => { const out = new Map(); const walk = (v, at) => { if (v && typeof v === "object") { if (Array.isArray(v) && v.length === 2 && typeof v[0] === "string" && typeof v[1] !== "object") { out.set(`${String(at).replace(/\.\d+$/, "")}/${v[0]}`, String(v[1])); return; } for (const [k, x] of Object.entries(v)) walk(x, at ? `${at}.${k}` : k); } else out.set(at, String(v)); }; const s = String(text); const m = /^([\[{][\s\S]*[\]}]):([^:]+)$/.exec(s); try { walk(JSON.parse(m ? m[1] : s), ""); if (m) out.set("(key suffix)", m[2]); } catch { out.set("inputs", s); } return out; };
  const a = flat(before?.inputs ?? ""); const b = flat(after?.inputs ?? "");
  const inputs = [...new Set([...a.keys(), ...b.keys()])].filter((k) => a.get(k) !== b.get(k)).sort();
  // Only names that can change a result count (buildEnvName); a publisher row's stored environment is the old whole-session one.
  const envKeys = [...new Set([...Object.keys(before?.env ?? {}), ...Object.keys(after?.env ?? {})])].filter((k) => buildEnvName(k) && (before?.env ?? {})[k] !== (after?.env ?? {})[k]).sort();
  // A real file edit changes one content id into another (git blob ids: 40 or 64 hex). Anything else (a key entry whose form changed) is not a file change.
  const blob = (v) => typeof v === "string" && /^[0-9a-f]{40}([0-9a-f]{24})?$/.test(v);
  // A file entry is keyed by its path, never its position in the list (one added file shifted every later position and read as thousands of key-form
  // changes: secret-scan, Mac 2026-11-07). An added or removed file is the row's own change, like an edited one.
  // The advisory rows (cargo-audit, cargo-deny, ghsa-audit, advisory-exceptions, pnpm audit) key on the UTC day by design (the advisory databases move
  // daily): a new day is their own declared input changing, like a file edit, never an unexplained miss (Mac 2026-11-07: 9 rows stopped a run at 00:02Z).
  const day = (k) => k === "day" && /^\d{4}-\d{2}-\d{2}$/.test(String(a.get(k))) && /^\d{4}-\d{2}-\d{2}$/.test(String(b.get(k)));
  const contentOnly = inputs.length > 0 && inputs.every((k) => day(k) || ((blob(a.get(k)) || !a.has(k)) && (blob(b.get(k)) || !b.has(k)) && (a.has(k) || b.has(k))));
  return {
    inputs: inputs.slice(0, 12), inputsChanged: inputs.length, contentOnly,
    ...(before?.toolVersion !== after?.toolVersion ? { tool: { from: before?.toolVersion ?? null, to: after?.toolVersion ?? null } } : {}),
    ...(envKeys.length ? { env: envKeys.slice(0, 12) } : {}),
    ...(before?.inherited !== after?.inherited ? { inheritedEnv: before?.inheritedShape && after?.inheritedShape ? [...new Set([...Object.keys(before.inheritedShape), ...Object.keys(after.inheritedShape)])].filter((k) => before.inheritedShape[k] !== after.inheritedShape[k]).sort().slice(0, 20) : true } : {}),
  };
}
/** NN41: the compile scans whose failure can be deterministic: check, clippy and doc rows (single or batched, native or cross). Test runs are never among them. */
export const isFailureCacheableCompileId = (id) => /^(?:check|check-features|check-dependent|clippy|doc)(?::|$)/.test(String(id));
/** NN41: a plain (non-broker) compile exit whose output carries compiler error diagnostics and no sign of the environment failing (network, disk, signal, internal compiler error). */
export function isDeterministicCompileDiagnostics(text) {
  const t = String(text ?? "");
  const diagnostics = /(?:^|\n)\s*error(?:\[[A-Za-z]\d+\])?: /.test(t) || /could not compile|aborting due to/.test(t);
  const environmental = /failed to (?:download|fetch|get|update|load source|create|write|open|remove)|network|spurious|timed out|No space left|Too many open files|Permission denied|Resource temporarily|Text file busy|internal compiler error|panicked at|pkg-config|PKG_CONFIG|system library|was not found in the pkg-config search path|Package [^\n]* was not found|linker `[^`]*` not found|unable to find library|cannot find -l|signal: |SIGKILL|SIGSEGV|Killed/i.test(t);
  return diagnostics && !environmental;
}
export const isLiveScan = (id) => LIVE_SCAN_IDS.some((p) => id === p || String(id).startsWith(`${p}:`));
/** Scans whose FAILURE may come from the registry rather than their inputs: `pnpm-lockfile` (`install --frozen-lockfile --lockfile-only`)
 * re-verifies lockfile entries against the registry when pnpm's cache is cold, so an outage fails it. Their pass is deterministic, but a
 * red they cause is never a reusable deterministic verdict (rightkit#1257; Oracle review of 79c4427b). */
export const REGISTRY_SCAN_IDS = Object.freeze(["pnpm-lockfile"]);
export const isRegistryScan = (id) => REGISTRY_SCAN_IDS.some((p) => id === p || String(id).startsWith(`${p}:`));
/** Files per rustfmt scan: 120 paths of ~100 bytes stay under Windows' 32 KB command line with the tool and flags. */
export const RUSTFMT_BATCH = 120;
/** FNV-1a of a path: the stable bucket hash of rustfmtBuckets. */
function pathBucketHash(text) {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) { h ^= text.charCodeAt(i); h = Math.imul(h, 0x01000193) >>> 0; }
  return h;
}
/** Most bytes of file arguments one rustfmt bucket passes: Windows refuses a command line past 32,767 characters. */
export const RUSTFMT_BUCKET_BYTES = 24000;
/**
 * Splits `files` into rustfmt buckets: bucket = path hash mod a power-of-two count, the smallest with at most 100 files per bucket on
 * average whose every bucket's arguments fit RUSTFMT_BUCKET_BYTES. Empty buckets are dropped; each bucket is sorted. A file's bucket
 * depends only on its path and the count, which changes only when the file count crosses a power of two.
 */
export function rustfmtBuckets(files) {
  const sorted = [...files].sort();
  let count = 1;
  while (count * 100 < sorted.length) count *= 2;
  for (;;) {
    const buckets = Array.from({ length: count }, () => []);
    for (const f of sorted) buckets[pathBucketHash(f) % count].push(f);
    if (buckets.every((b) => b.reduce((n, f) => n + f.length + 3, 0) <= RUSTFMT_BUCKET_BYTES)) return buckets.filter((b) => b.length);
    count *= 2;
  }
}

/** True when this module is the checked repository's own in-tree copy (packages/release of `contentRoot`), where its scripts/ live. */
export function inTreeGate(contentRoot, exists = fs.existsSync) {
  const here = path.dirname(fileURLToPath(import.meta.url));
  const real = (p) => { try { return fs.realpathSync(p); } catch { return path.resolve(p); } };
  const own = real(path.resolve(here, "..", ".."));
  return own === real(contentRoot) && exists(path.join(own, "scripts", "gen-suite-api.mjs"));
}
// The version a probe's output names, for a tool whose first line is not its version (`shellcheck --version` opens with a banner).
const PROBE_VERSION = Object.freeze({
  shellcheck: (text) => { const m = /^version:\s*(\S+)\s*$/m.exec(text); return m ? `shellcheck ${m[1]}` : null; },
  oxlint: (text) => { const m = /\b(\d+\.\d+\.\d+)\b/.exec(String(text).split(/\r?\n/).find((l) => l.trim()) ?? ""); return m ? `oxlint ${m[1]}` : null; },
});
/** null when `version` (a probe's answer) is the pinned one or the tool has no pin, else the problem. */
export function pinnedVersionProblem(tool, version) {
  const required = PINNED_SCAN_TOOLS[tool];
  if (!required || version === `${tool} ${required}`) return null;
  return `${tool} found ${version ? `"${version}"` : "with no readable version"}, class A requires ${tool} ${required} (PINNED_SCAN_TOOLS in @rightkit/release deterministic-scans.mjs); another version judges scripts differently, so it is refused, never passed or skipped`;
}

/**
 * RA-18 / L9-03: the analyzers whose verdict class A accepts run only from an ENROLLED executable. The scan-tool policy is data the host
 * owner enrols (ci/rk-pipeline.json `tools.<name>.<platform>`: an absolute `path`, the file's `sha256` and the `version`), handed to
 * runScans as `options.toolPolicy` (scanToolPolicyFromPipeline reads it). A tool is resolved ONCE per run from that entry, never from PATH: the
 * file's real path must be an executable regular file whose SHA-256 is the enrolled one, its `--version` must name the enrolled version, and the
 * same digest is read again immediately before every scan that runs it. The path, digest and version are bound into the scan's evidence
 * (`row.identities`, `result.toolIdentities`) and into its cache key. A tool with no enrolment is TOOL_UNTRUSTED, an enrolled file that
 * is absent or not runnable is TOOL_MISSING, a file whose bytes are not the enrolled ones (at resolution or after the probe) is TOOL_REPLACED,
 * another version is TOOL_VERSION: never a pass, never a skip, and a PATH stub that answers `--version` convincingly changes none of this.
 * A scan the broker carries (a snapshot-broker host's cargo-deny / cargo-audit) runs the broker's own registered tool: its identity is the
 * broker's tool registration (RustRight), recorded here as `source: "broker"` and never claimed as bound by the gate.
 */
export const TRUSTED_SCAN_TOOLS = Object.freeze(["actionlint", "shellcheck", "oxlint", "cargo-deny", "cargo-audit"]);
const SHA256_HEX = /^[0-9a-f]{64}$/i;

/** The scan-tool policy of a pipeline file's `tools` object for `platform`: `{ [tool]: { path, sha256, version } }` for each trusted tool that has a `<platform>` entry (shape unchecked here; resolveTrustedScanTool judges it). */
export function scanToolPolicyFromPipeline(tools, platform = process.platform) {
  const policy = {};
  for (const name of TRUSTED_SCAN_TOOLS) {
    const entry = tools && typeof tools === "object" && Object.hasOwn(tools, name) ? tools[name] : null;
    const host = entry && typeof entry === "object" && Object.hasOwn(entry, platform) ? entry[platform] : null;
    if (host && typeof host === "object") policy[name] = { path: host.path, sha256: host.sha256, version: host.version ?? entry.version };
  }
  return policy;
}

const sha256OfFile = (file) => {
  const hash = crypto.createHash("sha256"); const fd = fs.openSync(file, "r");
  try { const buffer = Buffer.alloc(1 << 20); for (let n = fs.readSync(fd, buffer, 0, buffer.length, null); n > 0; n = fs.readSync(fd, buffer, 0, buffer.length, null)) hash.update(buffer.subarray(0, n)); } finally { fs.closeSync(fd); }
  return hash.digest("hex");
};
const DEFAULT_TOOL_OPS = Object.freeze({ realpath: (p) => fs.realpathSync.native(p), stat: (p) => fs.statSync(p), digestFile: sha256OfFile });

/** `{ ok: true, identity: { tool, path, enrolledPath, sha256, version, source: "policy" } }` or `{ ok: false, code, why }` (TOOL_UNTRUSTED | TOOL_MISSING | TOOL_REPLACED). `ops` (realpath/stat/digestFile) is for tests of the judging only. */
export function resolveTrustedScanTool(tool, { policy, platform = process.platform, ops } = {}) {
  const o = { ...DEFAULT_TOOL_OPS, ...ops };
  const where = `ci/rk-pipeline.json tools.${tool}.${platform}`;
  const entry = policy && typeof policy === "object" && Object.hasOwn(policy, tool) ? policy[tool] : null;
  const untrusted = (why) => ({ ok: false, code: "TOOL_UNTRUSTED", why: `${why}; a binary found on PATH is never trusted and an unenrolled scan tool is never passed (the host's scan-tool policy owner enrols it: ${where} with an absolute path, the file's sha256 and its version, handed to the gate as options.toolPolicy)` });
  if (!entry || typeof entry !== "object") return untrusted(`${tool} is not enrolled in the scan-tool policy`);
  const paths = platform === "win32" ? path.win32 : path.posix;
  if (typeof entry.path !== "string" || !paths.isAbsolute(entry.path) || entry.path.split(/[\\/]/).includes("..") || entry.path.includes("\0")) return untrusted(`the ${tool} enrolment has no absolute path without '..'`);
  if (typeof entry.sha256 !== "string" || !SHA256_HEX.test(entry.sha256)) return untrusted(`the ${tool} enrolment has no sha256`);
  if (typeof entry.version !== "string" || !entry.version.trim()) return untrusted(`the ${tool} enrolment has no version`);
  let real; let meta;
  try { real = o.realpath(entry.path); meta = o.stat(real); } catch (e) { return { ok: false, code: "TOOL_MISSING", why: `the enrolled ${tool} ${entry.path} cannot be resolved (${e.code ?? e.message})` }; }
  if (!meta.isFile?.()) return { ok: false, code: "TOOL_MISSING", why: `the enrolled ${tool} ${real} is not a regular file` };
  if (platform !== "win32" && (meta.mode & 0o111) === 0) return { ok: false, code: "TOOL_MISSING", why: `the enrolled ${tool} ${real} is not executable` };
  let digest;
  try { digest = o.digestFile(real); } catch (e) { return { ok: false, code: "TOOL_MISSING", why: `the enrolled ${tool} ${real} cannot be read for its identity (${e.code ?? e.message})` }; }
  if (digest !== entry.sha256.toLowerCase()) return { ok: false, code: "TOOL_REPLACED", why: `the ${tool} at ${real} has digest ${digest}, not the enrolled ${entry.sha256.toLowerCase()}` };
  return { ok: true, identity: Object.freeze({ tool, path: real, enrolledPath: entry.path, sha256: digest, version: entry.version.trim(), source: "policy" }) };
}

/** null while the executable still is the one resolved (same real path, same digest), else `{ code: "TOOL_REPLACED", detail }`: read again immediately before a scan runs it. */
export function trustedScanToolProblem(identity, { ops } = {}) {
  const o = { ...DEFAULT_TOOL_OPS, ...ops };
  try {
    const real = o.realpath(identity.enrolledPath);
    if (real !== identity.path) return { code: "TOOL_REPLACED", detail: `${identity.tool}: ${identity.enrolledPath} now resolves to ${real}, not the ${identity.path} that was verified` };
    const digest = o.digestFile(real);
    if (digest !== identity.sha256) return { code: "TOOL_REPLACED", detail: `${identity.tool}: ${real} was replaced after it was verified (digest ${digest}, verified ${identity.sha256})` };
  } catch (e) { return { code: "TOOL_REPLACED", detail: `${identity.tool}: ${identity.path} can no longer be verified (${e.code ?? e.message})` }; }
  return null;
}

/** null when the probe's answer names the enrolled version (a whole token of its first line, a leading `v` ignored), else the problem. */
export function enrolledVersionProblem(identity, probeText) {
  const bare = (text) => String(text).trim().replace(/^v(?=\d)/i, "");
  const line = String(probeText ?? "").split(/\r?\n/).find((l) => l.trim()) ?? "";
  if (line.split(/\s+/).some((token) => bare(token) === bare(identity.version))) return null;
  return `${identity.tool} at ${identity.path} answers ${line.trim() ? `"${line.trim().slice(0, 120)}"` : "with no readable version"}, the enrolment names ${identity.version}`;
}

function commandText(d) {
  if (d.internal) return `internal:${d.internal}`;
  const env = Object.keys(d.env ?? {}).sort().map((k) => `${k}=${d.env[k]} `).join("");
  return `${env}${d.tool} ${d.args.join(" ")}`.trim();
}

// ---- package scripts that call the broker -----------------------------------------------------------------------------------
// A package's `test` script may run Rust through the broker (`rustright storage run --app x -- right-release test`, `rustright cargo test`) or
// through plain cargo, which on a host with no broker means the unmanaged toolchain. On such a host the `test:npm:<unit>` scan cannot pass, and
// its failure says nothing about the code: rightkit-gate defers it with the Rust gate. scriptBrokerCall only reads the scripts; it runs nothing.
const SCRIPT_TEXT_LIMIT = 16 * 1024;
const BROKER_TOOLS = new Set(["rustright", "cargo"]);
const ENV_ASSIGNMENT = /^[A-Za-z_][A-Za-z0-9_]*=/;
// Words that run the command after them.
const COMMAND_WRAPPERS = new Set(["cross-env", "env", "time", "exec", "command", "sudo", "nice", "nohup"]);
const PACKAGE_MANAGERS = new Set(["pnpm", "npm", "yarn"]);
const PACKAGE_RUNNERS = new Set(["npx", "bunx"]);
// Package manager options that point the run at another package or at every package: the script that runs is not in this package.json.
const RETARGET_FLAGS = new Set(["-r", "--recursive", "-w", "--workspace-root", "--workspaces", "-ws", "--include-workspace-root"]);
const RETARGET_VALUE_OPTIONS = new Set(["-F", "--filter", "--filter-prod", "-C", "--dir", "--prefix", "--workspace"]);
const SAME_DIR_VALUES = new Set([".", "./"]);
// Options that take the next word as their value (the rest are flags).
const VALUE_OPTIONS = new Set(["--reporter", "--loglevel", "--color", "--workspace-concurrency", "--stream", "--shell-emulator", "--test-pattern"]);

/** The simple commands of a shell line, each as its words: split on `&&`, `||`, `;`, `|`, `&`, `(`, `)` and newlines outside quotes. A backslash outside quotes is a path separator (Windows), not an escape. */
function shellCommands(text) {
  const commands = []; let words = []; let word = ""; let inWord = false; let quote = null;
  const endWord = () => { if (inWord) words.push(word); word = ""; inWord = false; };
  const endCommand = () => { endWord(); if (words.length) commands.push(words); words = []; };
  for (let i = 0; i < text.length; i += 1) {
    const c = text[i];
    if (quote) {
      if (c === quote) quote = null;
      else if (c === "\\" && quote === '"' && i + 1 < text.length) word += text[++i];
      else word += c;
      continue;
    }
    if (c === '"' || c === "'") { quote = c; inWord = true; continue; }
    if (c === "&" || c === "|" || c === ";" || c === "\n" || c === "(" || c === ")") { endCommand(); continue; }
    if (/\s/.test(c)) { endWord(); continue; }
    word += c; inWord = true;
  }
  endCommand();
  return commands;
}

/** What `pnpm|npm|yarn <args>` does in this package: { run: <script name> } or {} (another package, a built-in, an unknown word). */
function inspectPackageManager(args, scripts) {
  let i = 0;
  while (i < args.length && args[i].startsWith("-") && args[i] !== "--") {
    const [name, ...inline] = args[i].split("=");
    const value = inline.length ? inline.join("=") : undefined;
    if (RETARGET_FLAGS.has(name)) return {};
    if (RETARGET_VALUE_OPTIONS.has(name)) {
      if (!SAME_DIR_VALUES.has(value ?? args[i + 1])) return {};
      i += value === undefined ? 2 : 1;
    } else i += VALUE_OPTIONS.has(name) && value === undefined ? 2 : 1;
  }
  const sub = args[i];
  if (sub === undefined || sub === "--") return {};
  const rest = args.slice(i + 1);
  if (sub === "exec" || sub === "dlx") return inspectCommand(rest, scripts);
  if (sub === "run" || sub === "run-script" || sub === "rum" || sub === "urun") {
    let j = 0;
    while (j < rest.length && rest[j].startsWith("-") && rest[j] !== "--") j += VALUE_OPTIONS.has(rest[j]) ? 2 : 1;
    return rest[j] !== undefined && rest[j] !== "--" ? { run: rest[j] } : {};
  }
  if (sub === "test" || sub === "t" || sub === "tst") return { run: "test" };
  return Object.hasOwn(scripts, sub) ? { run: sub } : {};
}

/** What one simple command does: { call: "rustright" | "cargo" } when it runs the tool (through env words and wrappers), { run: <script> } for a package script, else {}. */
function inspectCommand(words, scripts) {
  let i = 0;
  for (let guard = 0; guard < 16; guard += 1) {
    while (i < words.length && ENV_ASSIGNMENT.test(words[i])) i += 1;
    if (i >= words.length) return {};
    const word = path.posix.basename(words[i].replaceAll("\\", "/")).replace(/\.(exe|cmd|bat)$/i, "");
    if (BROKER_TOOLS.has(word)) return { call: word };
    if (PACKAGE_MANAGERS.has(word)) return inspectPackageManager(words.slice(i + 1), scripts);
    if (COMMAND_WRAPPERS.has(word) || PACKAGE_RUNNERS.has(word)) {
      i += 1;
      while (i < words.length && words[i].startsWith("-")) i += 1;
      continue;
    }
    return {};
  }
  return {};
}

/**
 * Whether the package script `entry` runs `rustright` or `cargo`, directly or through the scripts it starts in the same package.json
 * (`pnpm <script>`, `pnpm run <script>`, `npm run <script>`, `yarn [run] <script>`, `npm test`), their `pre<script>` and `post<script>` hooks, and
 * `exec`/`dlx`/`npx` wrappers. A run pointed at another package (`--filter`, `-r`, `--dir` elsewhere) is not followed. Bounded to `maxDepth` nested
 * scripts and cycle-safe. Returns `{ tool, via, command }` for the first call in execution order (`via` is the scripts walked, `command` the command
 * line that calls the tool), or null. Reads package.json `scripts` data only.
 */
export function scriptBrokerCall(scripts, entry = "test", { maxDepth = 8 } = {}) {
  if (scripts === null || typeof scripts !== "object" || Array.isArray(scripts)) return null;
  const textOf = (name) => (Object.hasOwn(scripts, name) && typeof scripts[name] === "string" ? scripts[name].slice(0, SCRIPT_TEXT_LIMIT) : null);
  const seen = new Set();
  const visit = (name, via) => {
    if (via.length >= maxDepth || seen.has(name)) return null;
    seen.add(name);
    for (const part of [`pre${name}`, name, `post${name}`]) {
      const text = textOf(part);
      if (text === null) continue;
      for (const words of shellCommands(text)) {
        const found = inspectCommand(words, scripts);
        if (found.call) return { tool: found.call, via: [...via, part], command: words.join(" ").slice(0, 200) };
        if (found.run !== undefined) {
          const deeper = visit(found.run, [...via, part]);
          if (deeper) return deeper;
        }
      }
    }
    return null;
  };
  return visit(String(entry), []);
}

/**
 * Whether the package script `entry` builds (emits) code before or while it tests: `tsc` without --noEmit, or a package-manager
 * run of a `build` / `build:*` script in this package or another (`pnpm run build`, `pnpm --filter <pkg> build`), followed through
 * the scripts it starts in the same package.json like scriptBrokerCall. Such a test is not a pre-compile check: it runs after
 * class A, with the compile scans (Codex reviewer 2026-10-28). Returns `{ via, command }` or null.
 */
export function scriptBuildCall(scripts, entry = "test", { maxDepth = 8 } = {}) {
  if (scripts === null || typeof scripts !== "object" || Array.isArray(scripts)) return null;
  const textOf = (name) => (Object.hasOwn(scripts, name) && typeof scripts[name] === "string" ? scripts[name].slice(0, SCRIPT_TEXT_LIMIT) : null);
  const builds = (words) => {
    let i = 0;
    while (i < words.length && (ENV_ASSIGNMENT.test(words[i]) || COMMAND_WRAPPERS.has(path.posix.basename(words[i])) || PACKAGE_RUNNERS.has(path.posix.basename(words[i])))) i += 1;
    const word = path.posix.basename(String(words[i] ?? "").replaceAll("\\", "/")).replace(/\.(exe|cmd|bat)$/i, "");
    if (word === "tsc") return !words.slice(i + 1).some((w) => /^--?noEmit(=true)?$/i.test(w));
    if (!PACKAGE_MANAGERS.has(word)) return false;
    // pnpm's `-w` is `--workspace-root` (a flag); npm's `-w` is `--workspace <name>` (takes a value).
    const rest = words.slice(i + 1); const values = new Set(word === "pnpm" ? ["--filter", "-F", "--dir", "-C"] : word === "npm" ? ["--workspace", "-w", "--prefix"] : ["--cwd", "--filter"]);
    for (let j = 0; j < rest.length; j += 1) {
      if (values.has(rest[j])) { j += 1; continue; }
      if (rest[j].startsWith("-") || rest[j] === "run" || rest[j] === "run-script") continue;
      return /^build(:|$)/.test(rest[j]);
    }
    return false;
  };
  const seen = new Set();
  const visit = (name, via) => {
    if (via.length >= maxDepth || seen.has(name)) return null;
    seen.add(name);
    for (const part of [`pre${name}`, name, `post${name}`]) {
      const text = textOf(part);
      if (text === null) continue;
      for (const words of shellCommands(text)) {
        if (builds(words)) return { via: [...via, part], command: words.join(" ").slice(0, 200) };
        const found = inspectCommand(words, scripts);
        if (found.run !== undefined) { const deeper = visit(found.run, [...via, part]); if (deeper) return deeper; }
      }
    }
    return null;
  };
  return visit(String(entry), []);
}

// Cargo redirections on a reachable command line or script: an explicit manifest or output directory, or a working-directory change.
const CARGO_REDIRECT = /--manifest-path|--target-dir|(^|[\s;&|(])(CARGO_TARGET_DIR|CARGO_BUILD_TARGET_DIR)=|(^|[\s;&|(])(cd|pushd)\s/;

// The forms a test's path to cargo may take for its target directory to be bound (cargoReachOf). Everything else is unprovable.
// Tools that run no repository code and start no cargo: any arguments.
const REACH_SAFE_TOOLS = new Set(["tsc", "eslint", "prettier", "biome", "oxlint", "echo", "true"]);
// cargo options that move its manifest, workspace or output, or change its configuration (a short-option cluster holding C or Z too).
const CARGO_REDIRECT_ARG = /^(--manifest-path|--target-dir|--config)(=|$)|^-[A-Za-z]*[CZ]/;
// The cargo subcommands a bound test may run; anything else (an alias, cargo make, xtask, install, a plugin) is unprovable.
const CARGO_REACH_SUBCOMMANDS = new Set(["test", "build", "check", "clippy"]);
// Package-manager built-ins: never one of the package's scripts, whatever its package.json names (npm pack runs pack, not a script).
const PM_BUILTINS = new Set(["install", "i", "add", "ci", "pack", "publish", "version", "link", "unlink", "rebuild", "prune", "update", "up", "upgrade", "uninstall", "remove", "rm", "init", "create", "exec", "dlx", "x", "workspace", "workspaces", "config", "set", "import", "patch", "deploy", "audit", "outdated", "why", "store", "cache", "env", "setup", "self-update", "fetch"]);
// Characters a shell gives meaning outside quotes beyond the separators shellCommands splits on: an escape, a glob, a home or
// redirection, a brace or history expansion, a comment, and cmd.exe's ^ and %VAR%. A script holding one unquoted is unprovable.
const UNQUOTED_SHELL_META = new Set(["\\", "*", "?", "[", "]", "~", "<", ">", "{", "}", "!", "#", "^", "%"]);
/** The first character a shell would read specially outside quotes in `text`, or null. */
function unquotedShellMeta(text) {
  let quote = null;
  for (const c of String(text)) {
    if (quote) { if (c === quote) quote = null; continue; }
    if (c === '"' || c === "'") { quote = c; continue; }
    if (UNQUOTED_SHELL_META.has(c)) return c;
  }
  return null;
}
// Package-manager options the walk resolves; any other option is unprovable.
const PM_DIR_OPTIONS = new Set(["--dir", "-C", "--prefix", "--cwd"]);
const PM_PACKAGE_OPTIONS = new Set(["--filter", "-F", "--filter-prod", "--workspace"]);
const PM_PLAIN_FLAGS = new Set(["-s", "--silent", "--if-present", "--color", "--no-color", "--stream"]);
const PM_VALUE_FLAGS = new Set(["--reporter", "--loglevel"]);

/**
 * Where a package's `test` script makes cargo run, for a commit-snapshot compile (joint plan step 4c): `{ none: true }` when it reaches
 * no cargo, `{ dir }` (repository-relative) when every cargo it reaches runs from that one directory, `{ dirs }` when from several (the
 * caller requires them to be one workspace), or `{ unprovable }` with the reason. One CARGO_TARGET_DIR binds the whole test.
 *
 * Fail-closed allowlist (root reviews of 431fbeae, f883d5ca, 15300d82): a test is bound only when every command on every path it
 * reaches (each script with its pre/post hooks) parses completely into one of these forms:
 *   - `cargo <args>` or `rustright cargo <args>` with no option that moves the manifest, workspace or output (--manifest-path,
 *     --target-dir, --config, -C, -Z): cargo runs from the script's package directory;
 *   - `pnpm|npm|yarn` with only resolvable options in any position before the script (--dir/-C/--prefix/--cwd, --filter/-F/
 *     --filter-prod/--workspace naming one package of this repository, pnpm -w, npm -w <workspace>, and output flags) running
 *     `run <script>`, `test`, `<script>` with no further arguments, or `exec <command>` (walked the same way in that package);
 *   - a tool that runs no repository code (tsc, eslint, prettier, biome, oxlint, echo, true).
 * Anything else is unprovable: `cd`, an environment assignment, a shell expansion, node or any test runner, an unknown command or
 * option, a recursive run, an unresolvable package, arguments passed on to a script, a script past the size limit, the depth limit,
 * and a requiresBroker declaration (the declared path is not in the scripts). Over-refusal is the accepted cost.
 */
export function cargoReachOf({ unitDir, scripts, declared = null, readScripts = () => null, packageDirOf = () => null, maxDepth = 8 }) {
  const dirs = new Set();
  // Keyed on package and script alone: the walk refuses everything that could change how a script runs (cd, environment), so a
  // script reached twice from one package runs the same way both times.
  const seen = new Set();
  const unprovable = (why) => { throw Object.assign(new Error(why), { reach: true }); };
  const walkScript = (dirScripts, name, dir, via) => {
    const key = `${dir}\0${name}`;
    if (seen.has(key)) return;
    if (via.length >= maxDepth) unprovable(`the path to ${dir}'s ${name} (${via.join(" -> ")}) is deeper than ${maxDepth} scripts`);
    seen.add(key);
    for (const part of [`pre${name}`, name, `post${name}`]) {
      if (!Object.hasOwn(dirScripts, part)) continue;
      const text = dirScripts[part];
      if (typeof text !== "string") unprovable(`${dir}'s ${part} script is not text`);
      if (text.length > SCRIPT_TEXT_LIMIT) unprovable(`${dir}'s ${part} script is longer than ${SCRIPT_TEXT_LIMIT} characters`);
      const meta = unquotedShellMeta(text);
      if (meta !== null) unprovable(`${dir}'s ${part} script has an unquoted ${JSON.stringify(meta)}, which a shell would expand or redirect`);
      const step = [...via, dir === unitDir ? part : `${dir}:${part}`];
      for (const words of shellCommands(text)) walkCommand(words, dirScripts, dir, step);
    }
  };
  const walkCommand = (words, dirScripts, dir, via) => {
    const where = `${via.join(" -> ")}`;
    const shown = words.join(" ").slice(0, 120);
    if (words.some((w) => /[`$]/.test(w))) unprovable(`${where} runs \`${shown}\`, whose shell expansion this check cannot follow`);
    if (ENV_ASSIGNMENT.test(words[0])) unprovable(`${where} runs \`${shown}\` with its own environment`);
    const tool = path.posix.basename(words[0].replaceAll("\\", "/")).replace(/\.(exe|cmd|bat)$/i, "");
    if (tool === "cargo" || tool === "rustright") {
      const args = tool === "rustright" ? (words[1] === "cargo" ? words.slice(2) : unprovable(`${where} runs \`${shown}\`, a rustright command other than cargo`)) : words.slice(1);
      const end = args.indexOf("--");
      const own = end === -1 ? args : args.slice(0, end);
      if (own.some((w) => CARGO_REDIRECT_ARG.test(w))) unprovable(`the path to cargo (${where}) redirects it`);
      const sub = own.find((w) => !w.startsWith("-") && !w.startsWith("+"));
      if (!CARGO_REACH_SUBCOMMANDS.has(sub)) unprovable(`${where} runs \`${shown}\`, a cargo command (${sub ?? "none"}) other than ${[...CARGO_REACH_SUBCOMMANDS].join(", ")}`);
      dirs.add(dir);
      return;
    }
    if (PACKAGE_MANAGERS.has(tool)) return walkPackageManager(tool, words.slice(1), dirScripts, dir, via, shown);
    if (REACH_SAFE_TOOLS.has(tool)) return;
    unprovable(`${where} runs \`${shown}\` (${tool}), which this check cannot follow to cargo`);
  };
  const walkPackageManager = (tool, args, dirScripts, dir, via, shown) => {
    const where = `${via.join(" -> ")}`;
    let target = dir;
    let i = 0;
    let retargets = 0;
    // Options, in any position before the script or command: each one resolved, or the command is unprovable. At most one of them
    // may name the package or directory (two would leave which one wins to the package manager).
    const options = () => {
      while (i < args.length && args[i].startsWith("-")) {
        const [name, ...inline] = args[i].split("=");
        const value = inline.length ? inline.join("=") : args[i + 1];
        const width = inline.length ? 1 : 2;
        if (PM_DIR_OPTIONS.has(name) || PM_PACKAGE_OPTIONS.has(name) || ((name === "-w" || name === "--workspace-root") && (tool === "pnpm" || tool === "npm"))) {
          retargets += 1;
          if (retargets > 1) unprovable(`${where}: \`${shown}\` names its package or directory more than once`);
        }
        if (PM_DIR_OPTIONS.has(name)) { if (value === undefined) unprovable(`${where}: \`${shown}\` names no directory`); target = path.posix.normalize(path.posix.join(dir, String(value))); i += width; }
        else if (PM_PACKAGE_OPTIONS.has(name)) { const d = packageDirOf(String(value)); if (d === null || d === undefined) unprovable(`${where}: \`${shown}\` runs a package (${value}) this repository cannot resolve`); target = d; i += width; }
        else if ((name === "-w" || name === "--workspace-root") && tool === "pnpm") { target = "."; i += 1; }
        else if ((name === "-w") && tool === "npm") { const d = packageDirOf(String(value)); if (d === null || d === undefined) unprovable(`${where}: \`${shown}\` runs a workspace (${value}) this repository cannot resolve`); target = d; i += width; }
        else if (PM_PLAIN_FLAGS.has(name) && !inline.length) i += 1;
        else if (PM_VALUE_FLAGS.has(name)) i += width;
        else unprovable(`${where}: \`${shown}\` has an option (${name}) this check does not resolve`);
      }
    };
    options();
    const sub = args[i++];
    if (sub === undefined) unprovable(`${where}: \`${shown}\` runs no script`);
    // Options after `run`/`exec` count as well: the target is known only once every option before the script has been read.
    if (sub === "run" || sub === "run-script" || (sub === "exec" && tool === "pnpm")) options();
    if (target.startsWith("..")) unprovable(`${where}: \`${shown}\` runs outside this repository (${target})`);
    const targetScripts = target === dir ? dirScripts : readScripts(target);
    if (targetScripts === null || targetScripts === undefined) unprovable(`${where}: \`${shown}\` runs scripts of ${target}, which has no readable package.json`);
    const hop = target === dir ? via : [...via, `${tool} ${args.join(" ")}`.slice(0, 120)];
    if (sub === "exec" && tool === "pnpm") {
      if (i >= args.length) unprovable(`${where}: \`${shown}\` executes nothing`);
      return walkCommand(args.slice(i), targetScripts, target, hop);
    }
    let script;
    if (sub === "run" || sub === "run-script") { script = args[i++]; if (script === undefined) unprovable(`${where}: \`${shown}\` names no script`); }
    else if (sub === "test" || sub === "t" || sub === "tst") script = "test";
    else if (!PM_BUILTINS.has(sub) && Object.hasOwn(targetScripts, sub)) script = sub;
    else unprovable(`${where}: \`${shown}\` is a ${tool} command (${sub}) this check cannot follow`);
    if (i < args.length) unprovable(`${where}: \`${shown}\` passes arguments to ${target}'s ${script}`);
    walkScript(targetScripts, script, target, hop);
  };
  if (declared?.call) return { unprovable: "it is declared to reach the broker in ways its scripts do not show (requiresBroker)" };
  try {
    walkScript(scripts ?? {}, "test", unitDir, []);
  } catch (e) {
    if (e.reach) return { unprovable: e.message };
    throw e;
  }
  const list = [...dirs];
  return list.length === 0 ? { none: true } : list.length === 1 ? { dir: list[0] } : { dirs: list };
}

// A test script can reach the broker in ways scriptBrokerCall cannot read from package.json alone: `node -e "...spawnSync('pnpm', ['run', n])..."`,
// `node ./scripts/verify-local.mjs` (a script that spawns `rustright cargo test`), `sh -c`, `pnpm --dir <elsewhere> run verify`, `pnpm --filter`. HeardRight's
// root `test` is a `node -e` over `verify`, which is `pnpm --dir tauri-app-next run verify`, which is `node ./scripts/verify-local.mjs`, which runs
// `rustright cargo test`; guessing from the text of such a script would defer tests that merely mention the tool, so the app says it instead.
// `ci/gate.config.json` `npm.<unit>.requiresBroker` is `{ "reason": "<why this unit's test needs the broker>" }`, committed with the app and reviewed in the
// diff that adds it. The reason is required: a declaration without one, or of any other shape, is ignored (the test is planned and runs where this host can
// run it, the way the fail-closed default goes) and planScans says so in a note. `false` and `null` are plain "no declaration". It changes nothing on a host
// with the broker, in CI, on a protected ref or with the opt-out: the scan runs there exactly as before. Only where rightkit-gate defers the Rust gate does it
// take the unit's `test:npm:<unit>` scan out, with the Rust scans, named with its reason and never passed.
const REQUIRES_BROKER_REASON_LIMIT = 400;
export function declaredBrokerCall(unit, value) {
  if (value === undefined || value === null || value === false) return {};
  const reason = typeof value === "object" && !Array.isArray(value) && typeof value.reason === "string" ? value.reason.replace(/\s+/g, " ").trim() : "";
  if (!reason) return { problem: `ci/gate.config.json npm.${unit}.requiresBroker is ignored: it must be { "reason": "<why this unit's test needs the broker>" } with a non-empty reason; test:npm:${unit} is planned as if it were not there` };
  const text = reason.length > REQUIRES_BROKER_REASON_LIMIT ? `${reason.slice(0, REQUIRES_BROKER_REASON_LIMIT)}...` : reason;
  return { call: { tool: "rustright", via: ["test"], command: `ci/gate.config.json npm.${unit}.requiresBroker: ${text}`, declared: true } };
}

/**
 * The scan plan for a change: derived from the changed files plus the manifests they belong to.
 * `contentRoot` is a clean export of the commit (pure content checks); `liveRoot` is the checkout the
 * managed `rustright cargo` and `pnpm` run in. A scan whose files did not change is not planned.
 */
export function planScans({
  changed = [], contentRoot, liveRoot = contentRoot, trustedRoot = null, exists = fs.existsSync, readFile = (f) => fs.readFileSync(f, "utf8"),
  platform = process.platform, includeTests = true, localCompile = true,
  // The host's CPU architecture for its own Rust target (oneOutput, cross-targets.mjs).
  nativeArch = process.arch, cargoJobs = null,
  // Where the quiet test scans (ci/gate.config.json `quietTests`) run: "local" plans them for the quiet lane; "ci" leaves them to the
  // CI shards (one crate per runner, where the port, pipe, desktop and wall-clock conflicts that force the serial lane do not exist)
  // and records a note naming them (Adrian 2026-10-22: compile locally on the warm cache, qualify the conflicting tests on VMs).
  quietTests = "local",
  // Which scans this plan owes (Adrian, 2026-10-26; app-check.mjs TIERS): "compile" leaves out the scans added after @rightkit/release
  // 0.6.6 (shellcheck, and actionlint's shellcheck requirement) until every app builds through the door again; "release" plans everything.
  tier = "compile",
  // A broker host (rustright): the primary checkout's directory. The RustRight client refuses to start anywhere that is not a Cargo workspace
  // of the app's primary checkout (RK_NOT_PRIMARY_CHECKOUT), so (a) every CLASS B cargo scan (a compile the snapshot carrier admits) is
  // started from here, naming the snapshot as its source, and (b) the client's version probe runs here. CLASS A cargo scans (metadata,
  // package list, cargo-deny, cargo-audit) never read the checkout: they keep the content root (the snapshot) and run there as carried
  // read-only requests (snapshot-metadata.mjs), so an uncommitted edit in the checkout cannot reach them. null on a direct host.
  cargoLiveRoot = null,
  // { [cargoDir]: absolute directory }: the checkout's resolved target directory per Cargo workspace, set as CARGO_TARGET_DIR on
  // that workspace's cargo scans when the scans read a snapshot (the door's commit compile, joint plan step 4c).
  targetDirs = null,
  // True when the gate's runtime cargo wrapper (cargo-target-shim.mjs) is first on PATH for every scan: each cargo invocation binds its
  // own workspace's checkout target directory or is refused at run time, so the static walk (cargoReachOf) is advisory, never a refusal.
  cargoShim = false,
  // The command prefix every cargo scan runs through: `rustright cargo` on a managed host, plain `cargo` in CI.
  cargoCommand = ["rustright", "cargo"],
  // Physical containment: a changed path whose real path leaves the real content root (a symlink) is refused.
  realpath = (p) => fs.realpathSync(p),
  // Bytes of a changed file for the input digest (defaults to readFile; the gates pass the raw reader).
  readBytes = readFile,
  // Directory listing, for the every-unit scope a workspace file change brings in.
  listDir = (d) => { try { return fs.readdirSync(d); } catch { return []; } },
  // Per-crate feature sets, shaped like ci/gate.config.json `rust` ({ [crate]: { featureSets, checkFeatureSets } }).
  crateFeatures = {},
  // ci/gate.config.json `npm`: per-package gate settings (`quietTests`), read by the package's unit name (packages/<dir> → rightkit-<dir>).
  packageFeatures = {},
  // rightkit#1333 (PIPELINE-ARCHITECTURE P7: no e2e through the cargo test runner): the repository-relative directories of the end-to-end crates
  // (app-check.mjs e2eCrates: a non-optional rightkit-qa dependency and a driver module), computed by the caller from the commit's files. Such a
  // crate's test scan keeps its id and its class B compile but runs with --no-run: its tests need a built app and run only in the qualify stage
  // (`qa.journeys` of the release config, outside the broker), where the gate owes them (result.qualifyOwed).
  e2eCrateDirs = null,
  // rightkit#1332: ci/gate.config.json `swift`: { "<package dir>": { qualifyOnly: { reason } } }. A Swift package that cannot build from the bare
  // snapshot (a prep step, a prebuilt native library) is not built by class B; it stays in result.qualifyOwed with its reason. Never silent.
  swiftPackages = {},
  // True for full class A on the live checkout, which runs the repository's own code anyway (tests, typecheck, lint).
  liveExecution = false,
  // Crates a workspace-wide Rust change (Cargo.lock, vendor/) actually reaches, computed by the caller from
  // the lock graph (scripts/ci/select.mjs). null keeps every crate in scope.
  rustScope = null,
  // Repository-specific units for any layout: [{ name, dir, publish? }], dir repo-relative ("." for a root unit). When given they
  // replace the crates/<name> and packages/<name> layout (rightkit-gate). `cargoDir` is the Cargo workspace root the cargo scans run in.
  // A rust unit may name its own workspace: { name, dir, cargoDir, crate, publish? } runs its scans with cwd cargoDir and `-p <crate>`;
  // `name` is then the unit's identity in scan ids and feature config (rightkit-gate names it `<cargoDir>:<crate>` below the root, and the plain crate for a `crates/<crate>` workspace of its own), and
  // `crateFeatures` is read by `name` first, then by `crate`. Units of different workspaces never share a lock, a deny/audit run or a
  // `cargo metadata`; a unit that sets no cargoDir belongs to the global `cargoDir` above.
  rustUnits = null, npmUnits = null, cargoDir = ".",
  // pnpm roots: repository-relative directories ("." for the repository root) that hold their own lock or workspace file. null keeps the single
  // root "." (every npm unit belongs to it; one pnpm-lockfile and one pnpm-audit scan run there). With a list, each root gets its own
  // `pnpm-lockfile:<root>` and `pnpm-audit:<root>` scans, run in that directory ("." keeps the unsuffixed ids), planned when one of its units is in
  // scope or one of its own package.json / pnpm-lock.yaml / pnpm-workspace.yaml changed. An npm unit names its root as `root` (default "."); a
  // repository with no pnpm root at all (npmRoots: []) plans no pnpm scan (CodeRight: two independent roots and nothing at the repository root).
  npmRoots = null,
  // Parsed Cargo.lock graph (lock-graph.mjs parseLock). With it, every workspace crate that depends on a crate in scope and is
  // not itself in scope gets a `check-dependent` scan, as CI's rust-check rows do. null plans none.
  rustLock = null,
  // The same per Cargo workspace: { [cargoDir]: parsedLock }. A workspace without an entry here (and not the global `cargoDir`) plans
  // no reverse dependents.
  rustLocks = null,
  // The pinned RustSec advisory database ({ url, commit }, advisory-db.json): cargo-deny and cargo-audit read exactly that commit and never fetch.
  advisoryPin = null,
  // Cross-workspace reach of a crate-per-workspace repository (rightkit#1285): `crateDependents(unitNames: Set) -> Set` answers the
  // units that path-depend on any of `unitNames`, transitively, across Cargo workspaces (the gate builds it from packages/release/
  // crate-workspaces.mjs; manifests only). Every such unit that is not itself in scope gets a `check-dependent` scan. null plans none beyond
  // the per-workspace lock graph (rustLocks).
  crateDependents = null,
  // `{ "<repo-relative directory>": [unit names] }`: sources outside every unit that a unit compiles (vendor/citationberg is patched into
  // rightkit-typeset). A changed file under the directory puts those units in scope. null maps none.
  externalInputs = null,
  // Clippy batching (item 8; clippyBatchPlan). false keeps the production path: one Clippy scan per crate and lint feature set. It is
  // switched on only after the batch-equivalence run (clippy-equivalence.mjs, row 8.7) has passed on the hosts the map names; then the
  // planner emits one scan per compatible batch and no per-crate Clippy scan for a crate that belongs to one (row 8.15).
  // `shardSolo` is ci/shards.json `solo`; `clippySeparate` maps a unit name to the reason equivalence found batching changed it.
  clippyBatching = false, shardSolo = [], clippySeparate = new Map(),
  // Class B execution batching (B1, rightkit#1458): the compile scans (check, check-features, check-dependent, clippy, each cross target) of
  // one Cargo workspace that differ only in their `-p <crate>` are marked `batchable`, and runScans runs each group as ONE cargo invocation
  // (`-p a -p b ...`). Unlike clippyBatching above, ids, commands and the scan-set digest are unchanged: the plan still lists one scan per
  // crate and configuration, so required ids (GC-A14's three targets, scan-receipt) and GC-T14 keys are untouched; only the run is shared.
  // `shardSolo` and `clippySeparate` keep a crate out of every group, as they do for clippyBatching. Off unless the caller (rightkit-gate) asks.
  classBBatching = false,
  // Benchmark only (RK_GATE_MERGE_FEATURE_ROWS=1, Adrian 2026-11-07 parallelism benchmark; never a release default): rows whose only bar to a group is their
  // crate-specific feature set join the group too, each crate's features named as <crate>/<feature>. One invocation unifies features across its crates
  // (GC-I11), so a pass here does not prove each crate alone; it measures what maximal grouping buys.
  mergeFeatureRows = false,
  // Dependency-freshness advice (rightkit#1475): planned only when the caller asks for advice in this run. It is a live
  // registry answer that can never fail a run, and its uncached cost (a 120 s timeout in 41 of 81 crate runs,
  // 2026-11-04) must not sit on the blocking path to a compile; update reporting (@rightkit/outdated) owns asking.
  // A caller that wants the row passes true and gets exactly the historical scan, timeouts recorded as not measured.
  freshnessAdvice = false,
} = {}) {
  if (!contentRoot) throw new TypeError("planScans: contentRoot is required");
  if (!Array.isArray(cargoCommand) || cargoCommand.length === 0 || !["rustright", "cargo"].includes(cargoCommand[0]) || cargoCommand.slice(1).some((a) => a !== "cargo")) {
    throw new TypeError('planScans: cargoCommand must be ["rustright", "cargo"] or ["cargo"]');
  }
  const refusals = []; const accepted = [];
  for (const raw of new Set(changed)) {
    const c = containedPath(raw, contentRoot);
    if (c.refused) refusals.push({ raw: String(raw), why: c.refused });
    else accepted.push(c.path);
  }
  const files = [];
  let realRoot = null; let realRootError = null;
  try { realRoot = realpath(contentRoot); } catch (e) { realRootError = e; }
  for (const f of new Set(accepted)) {
    const abs = path.join(contentRoot, f);
    if (!exists(abs)) continue;
    let real = null;
    try { if (!realRoot) throw realRootError ?? new Error("no real content root"); real = realpath(abs); } catch (e) { refusals.push({ raw: f, why: `real path cannot be resolved (${e.message})` }); continue; }
    const rel = path.relative(realRoot, real);
    if (rel === ".." || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel)) { refusals.push({ raw: f, why: "real path (symlink) leaves the content root" }); continue; }
    files.push(f);
  }
  // Content digest of the input the scans cover: every accepted changed file's bytes plus every refused name.
  // NN07: a run plans twice (check stage, compile stage) over the same snapshot; a file whose identity (path, inode, size, mtime) is unchanged is
  // not read and hashed again in this process (measured 2026-11-07: 1.0 s of file reads per plan of RightKit's 13,894 files).
  const bytesDigest = (f) => {
    const abs = path.join(contentRoot, f);
    let id = null; try { const st = fs.statSync(abs, { bigint: true }); id = `${abs}\0${st.ino}\0${st.size}\0${st.mtimeNs}`; } catch { /* read decides */ }
    if (id && PLAN_DIGEST_MEMO.has(id)) return PLAN_DIGEST_MEMO.get(id);
    let d; try { d = sha256(readBytes(abs)); } catch { return "unreadable"; }
    if (id) PLAN_DIGEST_MEMO.set(id, d);
    return d;
  };
  const inputDigest = sha256(canonicalJson({ files: [...files].sort().map((f) => [f, bytesDigest(f)]), refused: refusals.map((r) => r.raw).sort() }));
  const under = (prefix) => files.filter((f) => f.startsWith(prefix));
  if (!["compile", "release"].includes(tier)) throw new Error(`planScans: tier must be compile or release, not ${JSON.stringify(tier)}`);
  let scans = []; const notes = []; const qualifyOwed = [];
  if (cargoLiveRoot) notes.push(`broker host: class A cargo scans run in the snapshot as carried read-only requests, class B compiles are carried requests started from the primary checkout ${cargoLiveRoot} (uncommitted edits there never reach a scan)`);
  // A snapshot compile's scans carry its target map (item 7): the map is part of every scan's cache key, so no result cached by a
  // live-checkout run (or a compile with another map) is taken for one that writes to these directories.
  const snapshotTargets = targetDirs ? { RK_SNAPSHOT_TARGETS: canonicalJson(targetDirs) } : {};
  const add = (d) => {
    const entry = { root: "live", args: [], ...d, env: { ...snapshotTargets, ...(d.env ?? {}) } };
    entry.command = commandText(entry);
    scans.push(entry);
  };
  const A = SCAN_CLASS.A; const B = SCAN_CLASS.B;
  // `unit` names the npm or Rust unit a refusal belongs to, so a unit-scoped check drops it with that unit (app-closure.mjs scopeScans).
  const refuse = (what, why, unit = null) => add({ id: `path-policy:${sha256(`${what}\0${why}`).slice(0, 12)}`, class: A, internal: "refuse", detail: `${what}: ${why}; refused, never run`, ...(unit ? { unit } : {}) });
  for (const r of refusals) refuse(`changed path ${JSON.stringify(r.raw)}`, r.why);
  // A rust unit's name may be `<cargoDir>:<crate>`; the part that reaches argv (`-p`) is the crate, and the workspace directory is
  // checked as a plain path where the unit is registered.
  const safeUnits = (names, kind) => names.filter((n) => {
    if (SAFE_UNIT_NAME.test(kind === "crate" ? crateOf(n) : n)) return true;
    refuse(`${kind} directory name ${JSON.stringify(n)}`, "not a plain name");
    return false;
  });
  const cleanDir = (d) => { const n = path.posix.normalize(posix(String(d))).replace(/\/+$/, ""); return n === "" ? "." : n; };
  const SAFE_UNIT_DIR = /^(?:\.|[A-Za-z0-9_@+.-]+(?:\/[A-Za-z0-9_@+.-]+)*)$/;
  const plainDir = (d) => !ABSOLUTE_PATH.test(d) && !d.split("/").includes("..") && SAFE_UNIT_DIR.test(d);
  const cargoWs = cleanDir(cargoDir);
  if (!plainDir(cargoWs)) throw new TypeError("planScans: cargoDir must be a plain repository-relative directory");
  // Every compile lands in the one output directory (oneOutput, cross-targets.mjs): release profile, the host's target unless another is named.
  // A refused argv (RK_PROFILE_REFUSED, RK_TEST_MODE_REFUSED) becomes a scan that fails with the refusal: in scope it fails the run, out of
  // scope it is dropped with the rest, and it never aborts planning for the whole repository.
  const refusedScan = (error) => ({ tool: "node", args: ["-e", `console.error(${JSON.stringify(String(error.message))}); process.exit(1)`] });
  const cargoBase = (ws, args, { raw = false } = {}) => { let argv; try { argv = oneOutput(raw ? args : everyFailure(args), { platform, arch: nativeArch }); } catch (error) { return { ...refusedScan(error), ...(ws !== "." ? { cwd: ws } : {}) }; } return { tool: cargoCommand[0], args: [...cargoCommand.slice(1), ...argv], ...(ws !== "." ? { cwd: ws } : {}), ...(targetDirs?.[ws] ? { env: { CARGO_TARGET_DIR: targetDirs[ws] } } : {}) }; };
  // Class B (compiles): started from the primary checkout on a broker host, the snapshot carrier names the source.
  const cargoIn = (ws, ...args) => ({ ...cargoBase(ws, cargoJobs === null ? args : cargoWidthArgs(args, cargoJobs)), ...(cargoLiveRoot ? { root: "cargoLive" } : {}) });
  // A Rust test scan on a BROKER host (cargoLiveRoot set; GATE-CONTRACT GC-A08): the broker compiles with the plain test command plus --locked and
  // --message-format=json (the broker adds --no-run itself and never executes a test), and the gate runs the listed executables outside it. --no-fail-fast and
  // --test-threads=1 are RUN arguments (runOutside.args), never part of the compile. A direct host (no broker, cargoLiveRoot unset) has nothing to keep tests
  // out of: its test scan stays the one-step `test ... --locked --no-fail-fast`, compile and run together, with no runOutside.
  const cargoTestIn = (ws, ...args) => ({ ...cargoBase(ws, [...(cargoJobs === null ? args : cargoWidthArgs(args, cargoJobs)), "--message-format=json"], { raw: true }), root: "cargoLive" });
  // Class A (reads Cargo inputs, compiles nothing): always the content root, the snapshot or the commit's own tree. These still execute
  // cargo (through the broker on a broker host), so a unit-scoped check follows the Rust closure for them (app-closure.mjs scopeScans reads
  // the ids `cargo-metadata|cargo-deny|cargo-audit[:<label>]` and `cargo-package-list:<unit>` and the scan's `cwd`; rename one there too).
  const cargoReadIn = (ws, ...args) => cargoBase(ws, args);
  const unitMap = (units, kind) => {
    if (units === null) return null;
    if (!Array.isArray(units)) throw new TypeError(`planScans: ${kind}Units must be an array of { name, dir }`);
    const map = new Map();
    for (const u of units) {
      if (typeof u?.name !== "string" || typeof u?.dir !== "string") throw new TypeError(`planScans: every ${kind} unit needs a string name and dir`);
      if (map.has(u.name)) throw new TypeError(`planScans: duplicate ${kind} unit ${u.name}`);
      const dir = cleanDir(u.dir);
      if (!plainDir(dir)) { refuse(`${kind} unit ${JSON.stringify(u.name)} directory ${JSON.stringify(u.dir)}`, "not a plain repository-relative path"); continue; }
      const entry = { dir, publish: u.publish !== false };
      if (kind === "npm") {
        entry.root = ".";
        if (u.root !== undefined) {
          if (typeof u.root !== "string") throw new TypeError(`planScans: npm unit ${u.name} needs a string root`);
          const r = cleanDir(u.root);
          if (!plainDir(r) || !(r === "." || dir === r || dir.startsWith(`${r}/`))) { refuse(`npm unit ${JSON.stringify(u.name)} root ${JSON.stringify(u.root)}`, "not a plain repository-relative directory that holds the unit"); continue; }
          entry.root = r;
        }
      }
      if (kind === "rust") {
        if (u.crate !== undefined && typeof u.crate !== "string") throw new TypeError(`planScans: rust unit ${u.name} needs a string crate`);
        entry.crate = u.crate ?? u.name;
        entry.cargoDir = cargoWs;
        if (u.cargoDir !== undefined) {
          if (typeof u.cargoDir !== "string") throw new TypeError(`planScans: rust unit ${u.name} needs a string cargoDir`);
          entry.cargoDir = cleanDir(u.cargoDir);
          if (!plainDir(entry.cargoDir)) { refuse(`rust unit ${JSON.stringify(u.name)} cargoDir ${JSON.stringify(u.cargoDir)}`, "not a plain repository-relative path"); continue; }
        }
      }
      map.set(u.name, entry);
    }
    return map;
  };
  const rustMap = unitMap(rustUnits, "rust"); const npmMap = unitMap(npmUnits, "npm");
  // The pnpm roots (see npmRoots): the listed ones and the roots the units name.
  let pnpmRoots = ["."];
  if (npmRoots !== null) {
    if (!Array.isArray(npmRoots)) throw new TypeError("planScans: npmRoots must be null or an array of repository-relative directories");
    const set = new Set(npmMap ? [...npmMap.values()].map((u) => u.root) : []);
    for (const raw of npmRoots) {
      const r = cleanDir(String(raw));
      if (plainDir(r)) set.add(r); else refuse(`pnpm root ${JSON.stringify(raw)}`, "not a plain repository-relative path");
    }
    pnpmRoots = [...set].sort();
  }
  const rootOfUnit = (n) => (npmRoots !== null && npmMap ? npmMap.get(n).root : ".");
  // The deepest pnpm root that holds a repository-relative file (every file is under "." when "." is a root).
  const rootOfFile = (f) => {
    let best = null;
    for (const r of pnpmRoots) if ((r === "." || f.startsWith(`${r}/`)) && (best === null || r.length > best.length)) best = r;
    return best;
  };
  // Units of an explicit-workspace layout (some unit names its cargoDir) get workspace-suffixed ids for the per-workspace scans.
  const perWorkspaceIds = Array.isArray(rustUnits) && rustUnits.some((u) => u?.cargoDir !== undefined);
  // The label a per-workspace scan id carries: the workspace directory, except for a workspace that is exactly one crate-named unit
  // (`crates/rightkit-fs` holding rightkit-fs, rightkit#1285 D2), which is labelled by the crate (`cargo-deny:rightkit-fs`).
  const wsLabel = (ws) => {
    if (rustMap) for (const [name, u] of rustMap) if (u.cargoDir === ws && name === u.crate && ws === `crates/${u.crate}`) return name;
    return ws;
  };
  const wsIdSuffix = (ws) => (perWorkspaceIds && ws !== "." ? `:${wsLabel(ws)}` : "");
  const wsOf = (n) => (rustMap ? rustMap.get(n).cargoDir : cargoWs);
  const crateOf = (n) => (rustMap?.get(n)?.crate ?? n);
  // The crate's feature plan input: the declared entry of ci/gate.config.json `rust`, plus `autoAllFeatures` for a crate that declares no featureSets but has Cargo
  // features (feature-coverage.mjs; rightkit#1231), read from the committed Cargo.toml.
  const manifestTextOf = (n) => { try { return readFile(path.join(contentRoot, rustDir(n), "Cargo.toml")); } catch { return null; } };
  const featuresOf = (n) => {
    const key = crateFeatures?.[n] !== undefined ? n : crateOf(n);
    return effectiveCrateFeatures({ crateFeatures: { [key]: crateFeatures?.[key] ?? {} }, units: [key], manifestOf: () => manifestTextOf(n) })[key];
  };
  // Per-crate platform scope (rightkit#1248, ci/gate.config.json rust.<crate>.platforms): a crate scoped to other hosts plans no compile or test
  // row here (check, check-features, clippy and its cross targets, doc, test, check-dependent). Its class A rows (package list, changelog,
  // licence, secrets) are file-only and stay. Said once per crate in the notes; the scoped host's own gate runs the rows.
  const platformSkipped = new Set();
  const outOfScope = (n) => {
    if (crateRunsOn(featuresOf(n), platform)) return false;
    if (!platformSkipped.has(n)) { platformSkipped.add(n); notes.push(`${crateOf(n)} is scoped to ${featuresOf(n).platforms.join(", ")} (ci/gate.config.json platforms); its compile and test scans are not planned on ${platform} and run on the scoped host`); }
    return true;
  };
  // With no Rust unit in play the default workspace counts only when it has a Cargo.toml: a root rust-toolchain.toml (or other Rust config)
  // in a crate-per-workspace repository must not turn the repository root into a phantom workspace whose cargo scans have no manifest.
  const workspaces = rustMap && rustMap.size ? [...new Set([...rustMap.values()].map((u) => u.cargoDir))].sort() : (exists(path.join(contentRoot, cargoWs === "." ? "Cargo.toml" : `${cargoWs}/Cargo.toml`)) ? [cargoWs] : []);
  const lockOf = (ws) => (rustLocks?.[ws] instanceof Map ? rustLocks[ws] : ws === cargoWs && rustLock instanceof Map ? rustLock : null);
  const rustDir = (n) => (rustMap ? rustMap.get(n).dir : `crates/${n}`);
  const npmDir = (n) => (npmMap ? npmMap.get(n).dir : `packages/${n}`);
  const inDir = (dir, f) => (dir === "." ? f : `${dir}/${f}`);
  // The unit whose directory holds `f`; the deepest directory wins and a root unit (".") only takes what no other unit does.
  const unitOf = (map, f) => {
    let best = null; let score = -1;
    for (const [name, u] of map) {
      const s = u.dir === "." ? 0 : u.dir.length;
      if ((u.dir === "." || f === u.dir || f.startsWith(`${u.dir}/`)) && s > score) { best = name; score = s; }
    }
    return best;
  };

  // ---- class A: content checks on the exported commit
  // A vendor/ file keeps upstream formatting only when vendor/upstream-files.json records it and its
  // bytes still match that upstream digest; any other vendor/ .rs (new, patched) is rustfmt-checked.
  // A patched vendor/ file (rightkit#915) keeps upstream formatting only while its bytes match the
  // reviewed `patched` record, which names its upstream base digest and the note describing the patch;
  // reformatting it would bury the patch. Any edit changes the digest, so the file is checked until re-recorded.
  let upstream = {}; let patched = {};
  try {
    const manifest = JSON.parse(readFile(path.join(contentRoot, "vendor/upstream-files.json")));
    upstream = manifest.files ?? {}; patched = manifest.patched ?? {};
  } catch { /* no manifest: nothing exempt */ }
  const DIGEST = /^[0-9a-f]{64}$/;
  const verbatimUpstream = (f) => f.startsWith("vendor/") && typeof upstream[f] === "string" && bytesDigest(f) === upstream[f];
  const recordedPatch = (f) => f.startsWith("vendor/") && DIGEST.test(patched[f]?.base ?? "") && typeof patched[f]?.note === "string"
    && patched[f].note.startsWith("vendor/") && DIGEST.test(patched[f]?.sha256 ?? "") && bytesDigest(f) === patched[f].sha256;
  // Declared fixture files (Adrian, 2026-10-30: "there should be a .rkignore list that somehow allows for those to be passed";
  // rightkit#1291): `.rightkit/fixture-files.json` { schema: 1, files: { "<path>": { sha256, note } } } lists files that are
  // deliberately malformed (an audit's known-answer inputs). A listed file whose bytes still match its sha256 skips the per-file
  // format and parse scans only (rustfmt, node --check, json-parse, toml-parse); every other scan still reads it. Any edit changes
  // the digest, so the file is checked again until re-recorded. A malformed manifest, a bad digest or an empty note refuses.
  let fixtureFiles = {};
  if (exists(path.join(contentRoot, ".rightkit/fixture-files.json"))) {
    try {
      const manifest = JSON.parse(readFile(path.join(contentRoot, ".rightkit/fixture-files.json")));
      if (manifest?.schema !== 1 || !manifest.files || typeof manifest.files !== "object" || Array.isArray(manifest.files)) throw new Error("schema 1 with a files object is required");
      for (const [f, row] of Object.entries(manifest.files)) {
        if (!/^[0-9a-f]{64}$/.test(row?.sha256 ?? "")) throw new Error(`${f}: sha256 must be 64 lowercase hex characters`);
        if (typeof row?.note !== "string" || !row.note.trim()) throw new Error(`${f}: a note saying why the file is malformed is required`);
        if (f.startsWith("/") || f.split("/").includes("..")) throw new Error(`${f}: paths are repository-relative`);
      }
      fixtureFiles = manifest.files;
    } catch (error) { refuse(".rightkit/fixture-files.json", error.message); }
  }
  const declaredFixture = (f) => Object.hasOwn(fixtureFiles, f) && bytesDigest(f) === fixtureFiles[f].sha256;
  // Generated Rust is not hand-formatted: bindgen output, or rustfmt's own `@generated` marker in the first five lines.
  const generatedRust = (f) => {
    let head = "";
    try { head = readFile(path.join(contentRoot, f)).split("\n", 5).join("\n"); } catch { return false; }
    return /@generated\b|automatically generated by rust-bindgen\b/.test(head);
  };
  const rustFiles = files.filter((f) => f.endsWith(".rs") && !verbatimUpstream(f) && !recordedPatch(f) && !generatedRust(f) && !declaredFixture(f));
  const byEdition = new Map();
  const editionOf = rustEditionResolver({ contentRoot, exists, readFile });
  const refusedEditions = new Set();
  for (const f of rustFiles) {
    const found = editionOf(f);
    if (found.problem) {
      // Never guess an edition: a file whose edition cannot be resolved is not rustfmt-checked under a default, the gate fails on it.
      if (!refusedEditions.has(found.manifest)) { refusedEditions.add(found.manifest); refuse(`rustfmt edition of ${found.manifest}`, found.problem); }
      continue;
    }
    byEdition.set(found.edition, [...(byEdition.get(found.edition) ?? []), f]);
  }
  // rustfmt runs per edition in buckets: Windows refuses a command line past ~32 KB (spawn ENAMETOOLONG on a large repository,
  // HeardRight on the Dell, 2026-10-27), so a bucket's arguments stay under RUSTFMT_BUCKET_BYTES. A file's bucket is a hash of its path over a
  // power-of-two bucket count (GC-A24, rightkit#1506): an edited or added file changes only its own bucket's key, where the former
  // sorted slices shifted every later batch. The first bucket keeps the plain id.
  for (const [edition, group] of [...byEdition].sort((a, b) => (a[0] < b[0] ? -1 : 1))) {
    for (const [i, bucket] of rustfmtBuckets(group).entries()) {
      add({ id: `rustfmt:${edition}${i ? `:${i + 1}` : ""}`, class: A, tool: "rustfmt", args: ["--check", "--edition", edition, ...bucket], root: "content" });
    }
  }
  for (const f of files.filter((f) => /\.(mjs|cjs|js)$/.test(f) && !f.includes("node_modules") && !declaredFixture(f)).sort()) {
    add({ id: `node-check:${f}`, class: A, tool: "node", args: ["--check", f], root: "content" });
  }
  const jsonFiles = files.filter((f) => f.endsWith(".json") && !f.includes("node_modules") && !declaredFixture(f)).sort();
  if (jsonFiles.length) add({ id: "json-parse", class: A, internal: "json-parse", files: jsonFiles });
  const tomlFiles = files.filter((f) => f.endsWith(".toml") && !f.includes("node_modules") && !declaredFixture(f)).sort();
  if (tomlFiles.length) add({ id: "toml-parse", class: A, internal: "toml-parse", files: tomlFiles });
  // The strict workflow YAML parser covers the subset GitHub workflows and actions use; other YAML is not parsed with it.
  const yamlFiles = files.filter((f) => /^\.github\/.+\.ya?ml$/.test(f)).sort();
  if (yamlFiles.length) add({ id: "yaml-parse", class: A, internal: "yaml-parse", files: yamlFiles });

  if (exists(path.join(contentRoot, "packages/release/rightkit-ownership.json")) && exists(path.join(contentRoot, "crates/rightkit-ownership/catalog/ownership-catalog.json"))) {
    add({ id: "ownership-catalog", class: A, internal: "ownership-catalog", files: ["packages/release/rightkit-ownership.json", "crates/rightkit-ownership/catalog/ownership-catalog.json"] });
  }
  const wsRel = (ws, f) => (ws === "." ? f : f.startsWith(`${ws}/`) ? f.slice(ws.length + 1) : null);
  // Workspaces whose own root files (lock, manifest, toolchain, .cargo, vendor) changed: every unit of such a workspace is in scope.
  // A workspace is also wide when Rust configuration changed in ANY ancestor directory of it (a repository-level .cargo/config.toml,
  // an intermediate apps/.cargo, a root rust-toolchain.toml, clippy.toml, rustfmt.toml or deny.toml reaches every nested workspace).
  const wideWs = new Set(workspaces.filter((ws) => ancestorConfigChanged(ws, files) || files.some((f) => { const r = wsRel(ws, f); return r !== null && WORKSPACE_RUST_FILE.test(r); })));
  // A pnpm root's own install graph (package.json, lock, workspace file, .npmrc) changed: every unit of that root is in scope.
  const wideJsRoots = new Set(pnpmRoots.filter((r) => files.some((f) => { const rel = wsRel(r, f); return rel !== null && WORKSPACE_JS_FILE.test(rel); })));
  const wideCrates = wideWs.size ? (rustMap ? [...rustMap.keys()].filter((n) => wideWs.has(wsOf(n))) : listDir(path.join(contentRoot, "crates"))).filter((n) => !rustScope || rustScope.has(n)) : [];
  if (wideWs.size && rustScope) notes.push(`workspace Rust change reaches ${wideCrates.length} crate(s) through the lock graph`);
  // A source outside every unit that a unit compiles (externalInputs), and a shared input, reach their units; nothing else outside a
  // unit's directory does (a crate-per-workspace repository has no root manifest or lock to widen the scope).
  const externalUnits = rustMap && externalInputs ? Object.entries(externalInputs).flatMap(([dir, names]) => (files.some((f) => f === dir || f.startsWith(`${dir.replace(/\/+$/, "")}/`)) ? names : [])).filter((n) => rustMap.has(n)) : [];
  const sharedUnits = rustMap && files.some((f) => SHARED_RUST_SCOPE_FILE.test(f)) ? [...rustMap.keys()] : [];
  if (sharedUnits.length) notes.push(`a shared build input changed (${files.filter((f) => SHARED_RUST_SCOPE_FILE.test(f)).join(", ")}); every Rust unit is in scope`);
  const crateNames = safeUnits([...new Set([...(rustMap ? files.map((f) => unitOf(rustMap, f)).filter(Boolean) : under("crates/").map((f) => f.split("/")[1])), ...wideCrates, ...externalUnits, ...sharedUnits])].filter((n) => exists(path.join(contentRoot, rustDir(n), "Cargo.toml"))).sort(), "crate");
  // The workspace a changed file belongs to is the deepest one that holds it (a root workspace holds only what no nested one does).
  const wsOfFile = (f) => { let best = null; for (const ws of workspaces) if ((ws === "." || f.startsWith(`${ws}/`)) && (best === null || ws.length > best.length)) best = ws; return best; };
  const configWs = new Set();
  for (const f of files) {
    const isConfig = CONFIG_CRATE_FILE.test(f); const ws = wsOfFile(f);
    if (ws !== null) { if (isConfig || ANCESTOR_DENY_FILE.test(wsRel(ws, f) ?? "")) configWs.add(ws); }
    else if (isConfig && workspaces.length === 1) configWs.add(workspaces[0]);
  }
  // Ancestor configuration is configuration of the workspace too: its metadata, cargo-deny and cargo-audit scans run as well.
  for (const ws of workspaces) if (ancestorConfigChanged(ws, files) || ancestorConfigChanged(ws, files, ANCESTOR_DENY_FILE)) configWs.add(ws);
  const rustConfigChanged = configWs.size > 0;
  const rustTouched = crateNames.length > 0 || rustConfigChanged;
  const touchedWs = [...new Set([...crateNames.map(wsOf), ...configWs])].sort();
  // A root npm package (dir ".") is every file's last resort, so a file inside a non-root Rust unit does not trigger it.
  const npmOwner = (f) => {
    const n = unitOf(npmMap, f);
    if (n !== null && npmMap.get(n).dir === "." && rustMap) { const r = unitOf(rustMap, f); if (r !== null && rustMap.get(r).dir !== ".") return null; }
    return n;
  };
  const pkgNames = safeUnits([...new Set([...(npmMap ? files.map(npmOwner).filter(Boolean) : under("packages/").map((f) => f.split("/")[1])), ...(wideJsRoots.size ? (npmMap ? [...npmMap.keys()].filter((n) => wideJsRoots.has(rootOfUnit(n))) : wideJsRoots.has(".") ? listDir(path.join(contentRoot, "packages")) : []) : [])])].filter((n) => exists(path.join(contentRoot, npmDir(n), "package.json"))).sort(), "package");
  // The pnpm roots with something to check: a unit in scope, or a package.json / lock / workspace file that belongs to the root (the deepest one).
  const jsRoots = [...new Set([...pkgNames.map(rootOfUnit), ...files.filter((f) => JS_TOOLING_FILE.test(f)).map(rootOfFile).filter((r) => r !== null)])].sort();

  // Explicit files: the exported commit has no .git for actionlint's project discovery.
  const workflowFiles = files.filter((f) => /^\.github\/workflows\/[^/]+\.ya?ml$/.test(f)).sort();
  const actionlintConfig = exists(path.join(contentRoot, ".github/actionlint.yaml")) ? ["-config-file", ".github/actionlint.yaml"] : [];
  // actionlint lints every run: script with shellcheck only when shellcheck is found, and otherwise passes silently; class A
  // requires it (`requires`), so a host without it fails the scan instead of skipping the shell checks.
  if (workflowFiles.length) add({ id: "actionlint", class: A, tool: "actionlint", requires: ["shellcheck"], args: [...actionlintConfig, ...workflowFiles], root: "content", runScriptLimit: { files: workflowFiles, bytes: RUN_SCRIPT_LIMIT_BYTES } });
  // Standalone shell scripts (SellRight's deploy scripts failed SC2154, SC2030/2031 and SC2016 in CI): every tracked .sh/.bash file,
  // with ShellCheck's own defaults and the repository's .shellcheckrc (data, not code). actionlint covers only workflow run: blocks.
  // A script whose shebang names another shell (zsh, fish) is not ShellCheck's to judge, so it is left out, never failed.
  const otherShell = (f) => { try { return /^#!.*\b(zsh|fish|pwsh|powershell)\b/.test(String(readFile(path.join(contentRoot, f))).split("\n", 1)[0]); } catch { return false; } };
  const shellFiles = files.filter((f) => /\.(sh|bash)$/.test(f) && !/(^|\/)node_modules\//.test(f) && !otherShell(f)).sort();
  // GB-83 (GATE rule-tier correction, E1/E17, B1/B14): the deterministic scan inventory does not depend on the app-rule tier. Only the app's
  // release rules (approvals, ratified rulings, notice digests) are deferred at the compile tier; every approved Class A scan, shellcheck
  // and the locked package listing included, is planned for dev, test, package, qualify and release alike.
  if (shellFiles.length) add({ id: "shellcheck", class: A, tool: "shellcheck", args: ["--format=gcc", "--external-sources", ...shellFiles], root: "content" });
  // No right-git drift scan: the one workflow right-git renders here, ci.yml, is hand-owned (its header;
  // docs/CI.md) and held by scripts/ci/ci-scans-contract.mjs instead, so drift is always reported by design.

  // Exceptions expire by date, not by change: checked on every run, whatever changed.
  if (exists(path.join(contentRoot, "ci/advisory-exceptions.json"))) add({ id: "advisory-exceptions", class: A, internal: "advisory-exceptions" });
  // Dependency freshness (rightkit#1475): `right-update check --json` once per repository that commits `.rightkit/update.json`. Advice only: it never
  // fails a run (latest is a live registry answer), and the row carries "N behind latest". Planned only on request
  // (`freshnessAdvice`): optional advice never blocks a compile (reconciled scope, rightkit#1505).
  if (freshnessAdvice && exists(path.join(contentRoot, ".rightkit/update.json"))) add({ id: "dependency-freshness", class: A, internal: "dependency-freshness", advise: true });
  // An app's advisory exceptions follow the same policy as RightKit's own: a Cargo workspace whose deny.toml or .cargo/audit.toml
  // ignores an advisory needs its ledger, <workspace>/.rightkit/advisory-exceptions.json, with owner, evidence, an unexpired
  // reviewBy, the exact affected versions and dependents in that workspace's Cargo.lock, and a person's approval; deny.toml and
  // audit.toml must match it exactly.
  for (const ws of workspaces) {
    if (typeof ws !== "string") continue;
    const dir = ws === "." ? "" : ws;
    // A non-empty list only: `[^\]\s]` is one character that is neither whitespace nor the closing bracket, so `ignore = []` followed by
    // a later `]` (the next table header) is not an ignore (Pulse, 2026-10-27: `\S` matched the `]` itself).
    // Comments go first: the @rightkit/config template keeps a commented example inside an empty `ignore = [ ... ]`, which is no
    // ignore (CutRight, 2026-11-03).
    const ignores = ["deny.toml", ".cargo/audit.toml"].some((f) => { try { return /\bignore\s*=\s*\[[^\]]*[^\]\s][^\]]*\]/s.test(stripTomlComments(readFile(path.join(contentRoot, dir, f)))); } catch { return false; } });
    if (ignores && !(dir === "" && exists(path.join(contentRoot, "ci/advisory-exceptions.json")))) add({ id: `advisory-exceptions${dir ? `:${wsLabel(ws)}` : ""}`, class: A, internal: "advisory-exceptions", dir, exceptionsFile: ".rightkit/advisory-exceptions.json" });
  }
  // Like generated-docs, planned only when a changed file is one of the contract's inputs or projections. The generator runs from
  // the trusted scanner checkout (the import below); the exported contract and projections are data it checks.
  if (files.some((f) => SUITE_API_INPUT.test(f)) && exists(path.join(contentRoot, "contracts/right-suite-api.v1.json"))) {
    // RightKit's own generator contract (scripts/gen-suite-api.mjs) is the checked repository's code: like scripts/preflight.mjs it
    // runs only when this gate is that repository's in-tree copy (and in CI), never from a pinned or installed copy checking the
    // repository (the broker's trusted gate refused RightKit's own compile on a module it could not find, 2026-10-27).
    // The trusted home is the gate's own repository (`trustedRoot`, the checkout rightkit-gate runs for: a commit check exports the
    // commit to a snapshot and hands that as contentRoot and liveRoot); the generator is code and runs only from that checkout's
    // in-tree gate, reading the snapshot as data. Without a trusted root (library callers), the live checkout or content root decides.
    const home = trustedRoot ?? (path.resolve(liveRoot) !== path.resolve(contentRoot) ? liveRoot : contentRoot);
    if (inTreeGate(home, exists)) add({ id: "suite-api", class: A, internal: "suite-api" });
    else notes.push("suite-api: RightKit's own generator contract runs from the in-tree gate and CI, not from this installed copy");
  }
  if (rustTouched) {
    for (const ws of touchedWs) add({ id: `cargo-metadata${wsIdSuffix(ws)}`, class: A, ...cargoReadIn(ws, ...metadataArgs()) });
    // The build-helper trace check (rightkit#1450) is removed from Class A (Adrian, 2026-11-03): the Mac tracer misattributed writes and
    // its sessions filled the disk (rightkit#1494), and apps waited on traces for hours. No host blocks on build-helper traces.
    // Locked package listings are observations in RustRight (#1216). They run through the selected executor
    // from the checked Cargo inputs, just like metadata; an executor refusal remains a failed scan.
    for (const name of crateNames.filter((n) => rustMap?.get(n).publish !== false)) add({ id: rustScanId.packageList(name), class: A, ...cargoReadIn(wsOf(name), "package", "-p", crateOf(name), "--list", "--allow-dirty", "--locked") });
  }
  for (const ws of [...configWs].sort()) {
    // Offline against the pinned database. The pin commit is part of the command (env), so a pin change is a different scan, and the
    // placeholders are filled at run time from the materialised checkout (runScans).
    const pin = advisoryPin ?? (advisoryPin = readAdvisoryPin());
    const pinEnv = { RK_ADVISORY_DB_COMMIT: pin.commit };
    add({ id: `cargo-deny${wsIdSuffix(ws)}`, class: A, ...cargoReadIn(ws, "deny", "--frozen", "--metadata-path", "{advisoryMetadata}", "--config", "{advisoryDenyConfig}", "check"), env: pinEnv, advisoryDb: "deny", advisoryPin: pin });
    add({ id: `cargo-audit${wsIdSuffix(ws)}`, class: A, ...cargoReadIn(ws, "audit", "--db", "{advisoryDb}", "--no-fetch"), env: pinEnv, advisoryDb: "audit", advisoryPin: pin });
    // The GitHub advisory database (rightkit#1461): cargo-audit and cargo-deny read RustSec only, so a GHSA RustSec has not copied yet passed the
    // gate (zip GHSA-m665-v7pm-7grj). One scan per workspace lock asks OSV about its crates.io packages (ghsa-audit.mjs; GATE-CONTRACT GC-A19).
    if (exists(path.join(contentRoot, inDir(ws, "Cargo.lock")))) add({ id: `ghsa-audit${wsIdSuffix(ws)}`, class: A, internal: "ghsa-audit", lockFile: inDir(ws, "Cargo.lock"), ...(ws === "." ? {} : { cwd: ws }) });
  }
  // The audit policy is the consumer's own: every package.json script that is exactly `pnpm audit <flags>` (only --prod, --dev,
  // --audit-level <low|moderate|high|critical> are accepted) declares one, and the scan runs the strictest of them all: a dependency
  // scope only when every declaration names that same scope, and the lowest audit level any declares (one without a level means low).
  // With none declared, every dependency at every severity. The gate never runs a weaker audit than the repository declares
  // (RightSites declares `pnpm audit --prod`: production dependencies, all severities).
  const LEVELS = ["low", "moderate", "high", "critical"];
  const consumerAuditFlags = (r) => {
    let scripts = {};
    try { scripts = JSON.parse(readFile(path.join(contentRoot, r, "package.json"))).scripts ?? {}; } catch { /* none: the strict default */ }
    const declared = [];
    for (const cmd of Object.values(scripts)) {
      const m = /^\s*pnpm\s+audit((?:\s+(?:--prod|--dev|--audit-level(?:=|\s+)(?:low|moderate|high|critical)))*)\s*$/.exec(String(cmd));
      if (!m) continue;
      const words = m[1].trim().split(/\s+|=/).filter(Boolean);
      const at = words.indexOf("--audit-level");
      const scope = words.includes("--prod") && !words.includes("--dev") ? "--prod" : words.includes("--dev") && !words.includes("--prod") ? "--dev" : null;
      declared.push({ scope, level: at < 0 ? 0 : LEVELS.indexOf(words[at + 1]) });
    }
    if (!declared.length) return [];
    const scope = declared.every((d) => d.scope === declared[0].scope) ? declared[0].scope : null;
    const level = Math.min(...declared.map((d) => d.level));
    return [...(scope ? [scope] : []), ...(level > 0 ? ["--audit-level", LEVELS[level]] : [])];
  };
  for (const r of jsRoots) {
    // Class A runs no repository hooks: `install` accepts --ignore-pnpmfile and --ignore-scripts; `audit` accepts
    // neither flag, so every class A pnpm scan also sets the same two settings through the environment.
    // One pair per pnpm root, run in the root's directory; the repository root keeps the unsuffixed ids.
    const where = r === "." ? {} : { cwd: r }; const suffix = r === "." ? "" : `:${r}`;
    add({ id: `pnpm-lockfile${suffix}`, class: A, tool: "pnpm", args: ["install", "--frozen-lockfile", "--lockfile-only", "--ignore-pnpmfile", "--ignore-scripts"], env: PNPM_NO_HOOKS_ENV, root: "content", ...where });
    add({ id: `pnpm-audit${suffix}`, class: A, tool: "pnpm", args: ["audit", ...consumerAuditFlags(r)], env: PNPM_NO_HOOKS_ENV, root: "content", ...where });
  }
  const crateDirs = crateNames.map(rustDir); const pkgDirs = pkgNames.map(npmDir);
  const withChangelog = crateNames.filter((n) => exists(path.join(contentRoot, rustDir(n), "CHANGELOG.md")) && exists(path.join(contentRoot, rustDir(n), "Cargo.toml")));
  for (const name of withChangelog) add({ id: rustScanId.changelog(name), class: A, internal: "changelog-version", files: [inDir(rustDir(name), "Cargo.toml"), inDir(rustDir(name), "CHANGELOG.md")] });
  if (crateDirs.length || pkgDirs.length) {
    add({ id: "license", class: A, internal: "license", files: [...crateDirs.map((d) => inDir(d, "Cargo.toml")), ...pkgDirs.map((d) => inDir(d, "package.json"))] });
  }
  // Every tracked file of the commit, not only crate and package directories (GATE-CONTRACT GC-A20 "the commit's tracked files"; a token in
  // docs/ passed before, found by the GC-A27 acceptance run N02, rightkit#1514). Ignored and stage-written paths are skipped as before.
  add({ id: "secret-scan", class: A, internal: "secret-scan", dirs: ["."] });

  // ---- class B: needs the compiler or runs tests or repository code
  // The docs generator is code. It runs only from the TRUSTED checkout (liveRoot), with the exported commit
  // handed to it as data (`--root`); the content root's own copy is never executed. When no separate
  // trusted checkout exists (liveRoot is contentRoot) or it lacks the generator, the scan is marked as
  // requiring a trusted-root run: deferred (never a pass), not run from submitted code.
  // Planned only when a changed file is one of the generator's inputs (GENERATOR_INPUT).
  if (exists(path.join(contentRoot, "scripts/generate-index.mjs")) && files.some((f) => GENERATOR_INPUT.test(f))) {
    // Full class A on the live checkout (`liveExecution`) already executes the repository's own code (its tests): there the
    // generator runs from that same checkout. Everywhere else the separation above holds.
    const trusted = (liveExecution || path.resolve(liveRoot) !== path.resolve(contentRoot)) && exists(path.join(liveRoot, "scripts/generate-index.mjs"));
    add({
      id: "generated-docs", class: A, executes: true, tool: "node", args: ["scripts/generate-index.mjs", "--check", "--root", "{contentRoot}"],
      ...(trusted ? {} : { deferred: "requires a trusted-root run: the generator is repository code and no separate trusted checkout provides it; the content root's copy is never executed" }),
    });
  }
  const clippyEntries = []; const coverageCrates = [];
  for (const name of crateNames) {
    if (outOfScope(name)) continue;
    // Feature sets as CI's gate runs them (scripts/ci/gate.mjs, ci/gate.config.json rust.<crate>), plus --all-features for a crate that declares none but has features (rightkit#1231):
    // clippy per `featureSets` (default features when absent); check per `checkFeatureSets` (default
    // --all-features), none when a lint set already uses --all-features. Some crates' features cannot
    // all be on together (rightkit-db sqlcipher vs sqlcipher-system; qa-driver wv2 generations).
    const own = featuresOf(name);
    const { lintSets, baseSets, defaultCheck, checkSets } = rustCratePlan(own);
    // Every feature the crate declares must be enabled by one of these sets (the feature-coverage scan below, rightkit#1231).
    coverageCrates.push({ name, manifest: inDir(rustDir(name), "Cargo.toml"), sets: [...(defaultCheck ? [[]] : []), ...lintSets, ...checkSets] });
    const ws = wsOf(name); const crate = crateOf(name);
    // `cargo clippy --all-targets` type-checks exactly what `cargo check --all-targets` does: the default
    // check runs only when no clippy set covers default features.
    if (defaultCheck) add({ id: rustScanId.check(name), class: B, batchUnit: name, ...cargoIn(ws, ...checkArgs(crate, { locked: true })) });
    checkSets.forEach((set, i) => add({ id: rustScanId.checkFeatures(name, i), class: B, batchUnit: name, ...cargoIn(ws, ...checkArgs(crate, { features: set, locked: true })) }));
    if (clippyBatching) lintSets.forEach((set) => clippyEntries.push({ name, crate, ws, set, count: lintSets.length, env: targetDirs?.[ws] ?? null }));
    else lintSets.forEach((set, i) => add({ id: rustScanId.clippy(name, lintSets.length, i), class: B, batchUnit: name, ...cargoIn(ws, ...clippyArgs(crate, { features: set, locked: true })) }));
    // The other two OS targets, every lint set, -D warnings, on every host (cross-targets.mjs; Adrian 2026-11-02/03, no skip). The zig C
    // toolchain must be provisioned (crossCcDir); without it the scan fails naming the fix, never skipped. On a broker host the broker must
    // admit the cross target and its CC_/CXX_/AR_ variables; a refusal there fails the scan.
    for (const target of scopedCrossTargets(own, platform)) {
      const crossEnv = crossCompileEnv(target);
      baseSets.forEach((set, i) => {
        const id = `${rustScanId.clippy(name, baseSets.length, i)}@${target}`;
        if (!crossEnv) { add({ id, class: B, tool: "node", args: ["-e", `console.error("RK_CROSS_CC_UNPROVISIONED: the zig C toolchain for ${target} is not provisioned; run node scripts/ci/provision-zig.mjs <managed data root>/zig (or set RIGHTKIT_CROSS_CC_DIR)"); process.exit(1)`] }); return; }
        const scan = cargoIn(ws, ...clippyArgs(crate, { features: set, locked: true, target }));
        add({ id, class: B, batchUnit: name, ...scan, env: { ...(scan.env ?? {}), ...crossEnv } });
      });
    }
    // NN40 (GC-A27): docs build as one command per workspace and configuration. The row is the doc subcommand with --no-deps, --lib, one `-p <crate>`,
    // the lint set and --locked, with a batchUnit, so class B batching groups the doc rows of one workspace and configuration into one invocation
    // naming every crate. The request never sets RUSTDOCFLAGS: the broker sets "-D warnings" itself on doc and refuses any other value.
    // A crate with no library has no API docs to check.
    if (exists(path.join(contentRoot, rustDir(name), "src", "lib.rs"))) {
      add({ id: rustScanId.doc(name), class: B, batchUnit: name, ...cargoIn(ws, "doc", "--no-deps", "--lib", "-p", crate, ...lintSets[0], "--locked") });
    }
  }
  // Class A, file-only (rightkit#1231): no feature of a crate's Cargo.toml may be left out of every planned feature set, so feature-gated code is never
  // unchecked. Crates without Cargo features need no row.
  const featureful = coverageCrates.filter((c) => { const t = manifestTextOf(c.name); return typeof t === "string" && declaredFeatureNames(t).length > 0; });
  if (featureful.length) add({ id: "feature-coverage", class: A, internal: "feature-coverage", crates: featureful });
  if (clippyBatching && clippyEntries.length) {
    // One Clippy scan per batch (row 8.6): the independent batches first, the sidecar-bound ones after them (row 8.8; the compile lane
    // drains in plan order). The reason a configuration stays separate is recorded in the plan notes.
    const sidecarBound = new Set(clippyEntries.filter((e) => sidecarBoundCrate({ dir: path.join(contentRoot, rustDir(e.name)), exists, readFile }).bound).map((e) => e.name));
    const plan = clippyBatchPlan(clippyEntries, { solo: shardSolo, separate: clippySeparate, sidecarBound, toolchain: cargoCommand.join(" ") });
    for (const b of plan.batches) add({ id: b.id, class: B, batch: { units: b.units, order: b.order, reason: b.reason }, ...cargoIn(b.ws, ...clippyArgs(b.crates, { features: b.set, locked: true })) });
    notes.push(...plan.notes);
  }
  // Reverse dependents: a workspace crate that is not in scope but whose lock closure reaches one that is must still compile.
  // A unit only depends on units of its own Cargo workspace (its own lock), so each workspace is judged by its own lock.
  const inScope = new Set(crateNames); const planned = new Set();
  for (const ws of [...new Set(crateNames.map(wsOf))].sort()) {
    const lock = lockOf(ws);
    if (!lock) continue;
    const memo = new Map();
    const scoped = crateNames.filter((n) => wsOf(n) === ws).map(crateOf);
    const workspace = (rustMap ? [...rustMap.keys()] : listDir(path.join(contentRoot, "crates"))).filter((n) => wsOf(n) === ws);
    const dependents = safeUnits(workspace.filter((n) => !inScope.has(n) && !outOfScope(n) && exists(path.join(contentRoot, rustDir(n), "Cargo.toml")) && scoped.some((t) => reach(lock, crateOf(n), memo).has(t))).sort(), "crate");
    for (const name of dependents) { planned.add(name); add({ id: `check-dependent:${name}`, class: B, batchUnit: name, ...cargoIn(ws, ...checkArgs(crateOf(name), { locked: true })) }); }
  }
  // Across Cargo workspaces (one workspace and lock per crate): a unit that path-depends on a unit in scope, transitively, is judged by
  // the manifests' path dependencies, since no lock holds both.
  if (typeof crateDependents === "function" && rustMap && crateNames.length) {
    const across = [...crateDependents(new Set(crateNames))].filter((n) => !inScope.has(n) && !planned.has(n) && rustMap.has(n) && !outOfScope(n) && exists(path.join(contentRoot, rustDir(n), "Cargo.toml"))).sort();
    for (const name of safeUnits(across, "crate")) add({ id: `check-dependent:${name}`, class: B, batchUnit: name, ...cargoIn(wsOf(name), ...checkArgs(crateOf(name), { locked: true })) });
  }
  // A linter config in the unit's directory or any ancestor of it (a repository-level config covers every unit, as the tools look it up).
  const hasConfig = (dir, names) => ancestorDirsOf(dir).some((d) => names.some((f) => exists(path.join(liveRoot, inDir(d, f)))));
  // tsc reads a workspace sibling's types where its package.json exports point: built output (dist/*.d.ts) that no commit holds, so a
  // check of the committed tree (CI, a commit snapshot) cannot find them, and a checkout with an old build checks against stale types.
  // A package that depends on another workspace package is therefore checked through a generated project that maps each sibling's
  // exported entry points to its committed sources (TSC_PROJECT, built by the runner); every other package keeps its own tsconfig.
  // `--noEmit` stays on the command line as well as in the project: the class A launch guard judges a tsc launch by its argv alone.
  // Every workspace package is a possible sibling, whatever subset this plan checks (a publication plans only its units, and their
  // siblings' sources must still be mapped: publish scans of @rightkit/workspace-ui could not resolve @rightkit/mail-ui/*).
  const workspaceDirs = [...new Set([...(npmMap ? [...npmMap.values()].map((u) => u.dir) : []), ...listDir(path.join(liveRoot, "packages")).map((n) => `packages/${n}`)])].sort();
  const manifestOf = (dir) => { try { return JSON.parse(readFile(path.join(liveRoot, dir, "package.json")).replace(/^﻿/, "")); } catch { return null; } };
  const workspaceNames = new Map(workspaceDirs.map((d) => [manifestOf(d)?.name, d]).filter(([n]) => typeof n === "string"));
  const usesWorkspaceSibling = (dir) => {
    const m = manifestOf(dir);
    return ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"].some((k) => Object.keys(m?.[k] ?? {}).some((n) => n !== m.name && workspaceNames.has(n)));
  };
  for (const name of pkgNames) {
    const dir = npmDir(name);
    if (exists(path.join(liveRoot, dir, "tsconfig.json"))) {
      add(usesWorkspaceSibling(dir)
        ? { id: `tsc:${name}`, class: A, executes: true, tool: "pnpm", args: ["exec", "tsc", "--noEmit", "-p", TSC_PROJECT], tscWorkspace: [...workspaceNames.values()].sort(), env: PNPM_RUN_ENV, cwd: dir }
        : { id: `tsc:${name}`, class: A, executes: true, tool: "pnpm", args: ["exec", "tsc", "--noEmit", "-p", "tsconfig.json"], env: PNPM_RUN_ENV, cwd: dir });
    }
    if (hasConfig(dir, ESLINT_CONFIG_FILES)) add({ id: `eslint:${name}`, class: A, executes: true, tool: "pnpm", args: ["exec", "eslint", "."], env: PNPM_RUN_ENV, cwd: dir });
    if (hasConfig(dir, BIOME_CONFIG_FILES)) add({ id: `biome:${name}`, class: A, executes: true, tool: "pnpm", args: ["exec", "biome", "lint", "."], env: PNPM_RUN_ENV, cwd: dir });
    // Oxlint (rightkit#1402): the pinned, enrolled native binary (PINNED_SCAN_TOOLS; ci/oxlint.json), never `pnpm exec`, so it needs no installed
    // dependencies and a package's own copy never judges it. It reads the config in the package directory. A JSON config with no `jsPlugins` is data
    // and the row is a content check; a script config (`oxlint.config.*`) or `jsPlugins` make Oxlint run repository code, so the row `executes` (full class A only).
    // Not enrolled on the host: TOOL_UNTRUSTED, as for actionlint; never skipped, never passed.
    // The nearest directory at or above the package that holds a config decides (the config applies to the whole package; an ancestor's is named with -c).
    const oxlintDir = [...ancestorDirsOf(dir)].reverse().find((d) => OXLINT_CONFIG_FILES.some((f) => exists(path.join(liveRoot, inDir(d, f)))));
    if (oxlintDir !== undefined) {
      const configFile = OXLINT_CONFIG_FILES.find((f) => exists(path.join(liveRoot, inDir(oxlintDir, f))));
      let runsCode = configFile !== ".oxlintrc.json";
      if (!runsCode) { try { runsCode = /"jsPlugins"/.test(readFile(path.join(liveRoot, inDir(oxlintDir, configFile)))); } catch { runsCode = true; } }
      const configArgs = oxlintDir === dir ? [] : ["-c", path.posix.relative(dir, inDir(oxlintDir, configFile))];
      add({ id: `oxlint:${name}`, class: A, tool: "oxlint", args: [...configArgs, "."], root: "content", cwd: dir, ...(runsCode ? { executes: true } : {}) });
    }
  }
  // One `swift build` per Swift package a changed file belongs to: the nearest Package.swift at or above the file, built in its own
  // directory (rightkit#1229: Pulse's package is swift/RightKitHub, and a root `swift build` found no package and blocked every push).
  const swiftFiles = files.filter((f) => f.startsWith("swift/") || f === "Package.swift" || f.endsWith("/Package.swift"));
  if (swiftFiles.length) {
    const packages = new Set();
    for (const f of swiftFiles) {
      const owner = ancestorDirsOf(path.posix.dirname(f)).reverse().find((d) => exists(path.join(contentRoot, inDir(d, "Package.swift"))));
      if (owner !== undefined) packages.add(owner);
      else notes.push(`${f}: no Package.swift at or above it, so no Swift package builds it`);
    }
    for (const dir of [...packages].sort()) {
      const id = dir === "" || dir === "." ? "swift-build" : `swift-build:${dir}`;
      const declared = swiftPackages?.[dir === "" ? "." : dir]?.qualifyOnly;
      const reason = typeof declared?.reason === "string" && declared.reason.trim() !== "" ? declared.reason.replace(/\s+/g, " ").trim() : null;
      if (declared && !reason) notes.push(`ci/gate.config.json swift.${dir}.qualifyOnly is ignored: it must be { "reason": "<why this package cannot build from the bare snapshot>" } with a non-empty reason; ${id} is planned`);
      // The Swift type-check (rightkit#1289): Class A, the Mac host only; a package class B does not build (qualify-only) cannot be type-checked from the bare snapshot either.
      if (!reason) {
        if (platform === "darwin") add({ id: dir === "" || dir === "." ? "swift-typecheck" : `swift-typecheck:${dir}`, class: A, internal: "swift-typecheck", dir: dir === "" ? "." : dir, executes: true });
        else notes.push(`swift-typecheck skipped for ${dir === "" || dir === "." ? "the root package" : dir}: the Swift type-check runs on the Mac host only (platform ${platform})`);
      }
      if (reason) { qualifyOwed.push({ id, kind: "swift-build", dir: dir === "" ? "." : dir, reason }); notes.push(`${id} is not built by class B (ci/gate.config.json swift.${dir}.qualifyOnly: ${reason}); it is owed to the qualify stage`); }
      // The committed Package.resolved is the lockfile: like cargo --locked and pnpm --frozen-lockfile, a stale one fails here with
      // SwiftPM's own message instead of SwiftPM rewriting it inside the snapshot (seen only after the compile as a changed tree).
      else if (platform === "darwin") add({ id, class: B, tool: "swift", args: ["build", "--disable-automatic-resolution"], ...(dir === "" || dir === "." ? {} : { cwd: dir }) });
      else notes.push(`swift files changed in ${dir === "" || dir === "." ? "the root package" : dir}; \`swift build\` is required on the Mac host before its push`);
    }
  }
  // Class A, file-only (rightkit swift-lock.mjs): the committed Package.resolved (or an Xcode project's) must match the declared dependencies, like
  // cargo --locked and pnpm --frozen-lockfile (Adrian, 2026-11-01: a stale lockfile must never reach the compile). One scan per Swift package or
  // .xcodeproj a changed file belongs to, planned on every platform and for qualify-only packages too: it reads files and builds nothing. It sets no
  // `executes`, so a scoped class A run keeps it (app-closure.mjs scopeScans keeps file-only checks), and it never needs a live checkout.
  const lockOwners = new Map();
  for (const f of files) {
    if (!(f.startsWith("swift/") || /(^|\/)(Package\.swift|Package\.resolved)$/.test(f) || /(^|\/)[^/]+\.xcodeproj\//.test(f))) continue;
    const o = lockOwnerOf(f, { exists: (d) => exists(path.join(contentRoot, d)), ancestorDirs: ancestorDirsOf });
    if (o) lockOwners.set(`${o.kind}:${o.dir}`, o);
  }
  for (const o of [...lockOwners.values()].sort((a, b) => (a.dir < b.dir ? -1 : a.dir > b.dir ? 1 : 0))) {
    const resolved = o.kind === "xcode" ? `${o.dir}/project.xcworkspace/xcshareddata/swiftpm/Package.resolved` : inDir(o.dir, "Package.resolved");
    add({ id: o.dir === "." ? "swift-lock" : `swift-lock:${o.dir}`, class: A, internal: "swift-lock", lock: o, files: [o.kind === "xcode" ? `${o.dir}/project.pbxproj` : inDir(o.dir, "Package.swift"), resolved] });
  }
  // The same file-only lock check for Gradle (`gradle-lock`: committed dependency locking must cover every direct dependency) and Python
  // (`python-lock`: pins and uv/poetry locks must agree with their requirements), one scan per build root or directory a changed file belongs to.
  const gradleRoots = new Set();
  for (const f of files) {
    if (!(GRADLE_BUILD_FILE.test(f) || /(^|\/)(gradle\.lockfile|libs\.versions\.toml)$/.test(f) || /(^|\/)gradle\/dependency-locks\//.test(f))) continue;
    const g = f.includes("/gradle/") && !GRADLE_BUILD_FILE.test(f) ? f.slice(0, f.indexOf("/gradle/") + 1) + "x" : f;
    const root = gradleOwnerOf(g, { exists: (d) => exists(path.join(contentRoot, d)), ancestorDirs: ancestorDirsOf });
    if (root !== null) gradleRoots.add(root);
  }
  for (const dir of [...gradleRoots].sort()) add({ id: dir === "." ? "gradle-lock" : `gradle-lock:${dir}`, class: A, internal: "gradle-lock", dir, files: [] });
  const pythonDirs = new Set();
  for (const f of files) if (PYTHON_LOCK_FILE.test(f)) pythonDirs.add(path.posix.dirname(f));
  for (const dir of [...pythonDirs].sort()) add({ id: dir === "." ? "python-lock" : `python-lock:${dir}`, class: A, internal: "python-lock", dir, files: [] });
  if (includeTests) {
    const deferredToCi = [];
    // Tests per declared feature set, serially too where the crate asks for it, as CI's gate runs them. `quietTests: true` marks
    // every test scan of the crate for the quiet lane (laneOf): run alone, under the broker's quiet lease.
    for (const name of crateNames) {
      if (outOfScope(name)) continue;
      const own = featuresOf(name);
      const sets = rustCratePlan(own).baseSets;
      const quiet = own.quietTests === true ? { quiet: true } : {};
      if (own.quietTests === true && quietTests === "ci") {
        deferredToCi.push(...sets.map((set, i) => rustScanId.testRust(name, sets.length, i)), ...(own.alsoSerial ? sets.map((set, i) => rustScanId.testRustSerial(name, sets.length, i)) : []));
        continue;
      }
      // An end-to-end crate compiles here and runs in qualify (P7): --no-run, the same id, the same feature sets, the same class B compile.
      const e2e = e2eCrateDirs instanceof Set && e2eCrateDirs.has(rustDir(name));
      if (e2e) { qualifyOwed.push({ id: rustScanId.testRust(name, sets.length, 0), kind: "e2e-crate", dir: rustDir(name), crate: crateOf(name), reason: "end-to-end crate: its tests drive a built app and never run as class B unit tests (P7)" }); notes.push(`${crateOf(name)} is an end-to-end crate: its test scan compiles with --no-run here; its tests run in the qualify stage against the built app (qa.journeys), never as class B unit tests`); }
      sets.forEach((set, i) => {
        // NN17/NN39: the plain test rows of one workspace and configuration are batchable: one grouped `cargo test` compile on a broker host (runScans runTestGroup).
        // A quiet suite keeps its own lease and runs alone; an end-to-end crate only compiles (--no-run); the serial re-run (alsoSerial, below) keeps its own run.
        add({ id: rustScanId.testRust(name, sets.length, i), class: B, ...quiet, ...(!e2e && cargoLiveRoot && quiet.quiet !== true ? { batchUnit: name } : {}), ...(e2e ? { e2e: true } : {}), ...(e2e ? cargoIn(wsOf(name), "test", "-p", crateOf(name), ...set, "--locked", "--no-run") : cargoLiveRoot ? { ...cargoTestIn(wsOf(name), "test", "-p", crateOf(name), ...set, "--locked"), runOutside: { args: [] } } : cargoIn(wsOf(name), "test", "-p", crateOf(name), ...set, "--locked", "--no-fail-fast")) });
        if (own.alsoSerial) add({ id: rustScanId.testRustSerial(name, sets.length, i), class: B, ...quiet, ...(e2e ? { e2e: true } : {}), ...(e2e ? cargoIn(wsOf(name), "test", "-p", crateOf(name), ...set, "--locked", "--no-run") : cargoLiveRoot ? { ...cargoTestIn(wsOf(name), "test", "-p", crateOf(name), ...set, "--locked"), runOutside: { args: ["--test-threads=1"] } } : cargoIn(wsOf(name), "test", "-p", crateOf(name), ...set, "--locked", "--no-fail-fast", "--", "--test-threads=1")) });
      });
    }
    for (const name of pkgNames) {
      let manifest = null;
      try { manifest = JSON.parse(readFile(path.join(liveRoot, npmDir(name), "package.json"))); } catch { /* unreadable manifest: no test row */ }
      if (!manifest?.scripts?.test) continue;
      // `npm.<package>.quietTests: true` (ci/gate.config.json) marks a package whose tests are wall-clock bound (vitest timeouts,
      // Chrome for Testing shutdown): under "ci" they ride the CI shards, under "local" the quiet lane, never the 4-wide test lane.
      const own = packageFeatures?.[name] ?? {};
      if (own.quietTests === true && quietTests === "ci") { deferredToCi.push(`test:npm:${name}`); continue; }
      // A gate proves the repository's own content: sibling checkouts beside it (RightKit's suite-contract tests read the app
      // repositories when present) are not inputs, so the package tests run as an isolated checkout, as they do in CI.
      // `brokerCall` marks a test script that runs the broker (or cargo) through its script chain (scriptBrokerCall), or a unit whose app declares that
      // its test needs the broker (declaredBrokerCall): rightkit-gate defers such a scan with the Rust gate where this host has no broker.
      // A declaration (`npm.<unit>.requiresBroker`, declaredBrokerCall) covers what the script text does not show; a call the scripts do show wins.
      const declared = declaredBrokerCall(name, own.requiresBroker);
      if (declared.problem) notes.push(declared.problem);
      // `npm.<unit>.afterClassA.reason` (ci/gate.config.json): the unit's tests launch stand-ins named like compilers (stub scripts,
      // which can run anything), so they cannot run under the class A launch guard; they run after class A, with the compile scans.
      // A reason is required.
      const afterClassA = typeof own.afterClassA?.reason === "string" && own.afterClassA.reason.trim() !== "" ? own.afterClassA.reason.trim() : null;
      const brokerCall = scriptBrokerCall(manifest.scripts, "test") ?? declared.call ?? null;
      const buildCall = brokerCall ? null : scriptBuildCall(manifest.scripts, "test");
      const laterReason = brokerCall ? null : buildCall ? `its test script builds before testing (${buildCall.command}); it runs after class A, with the compile scans` : afterClassA ? `declared to run after class A (ci/gate.config.json npm.${name}.afterClassA): ${afterClassA}` : null;
      // Isolated only where the tests run on an export (contentRoot is not the live checkout): there sibling repositories are
      // absent by construction. On the live checkout (full class A, broker admission) the cross-app contract tests run for real.
      const isolatedCheckout = path.resolve(contentRoot) !== path.resolve(liveRoot);
      // `npm.<unit>.classATests.evidence` (ci/gate.config.json): the unit's tests were shown, by import inspection and launch observation
      // under the class A launch guard, to compile and build nothing (docs/GATE-CLASS-A-INVENTORY.md "Test units"); they run in class A,
      // still under the guard, which refuses any compile at run time. Never when the script reaches the broker, builds, or is declared after class A.
      const classAEvidence = typeof own.classATests?.evidence === "string" && own.classATests.evidence.trim() !== "" ? own.classATests.evidence.trim() : null;
      const inClassA = classAEvidence && !brokerCall && !buildCall && !afterClassA;
      if (classAEvidence && !inClassA) notes.push(`npm.${name}.classATests is ignored: ${brokerCall ? "its test script reaches the broker" : buildCall ? `its test script builds (${buildCall.command})` : "it is declared afterClassA"}; it runs after class A`);
      // A commit snapshot's compile (targetDirs): a test that reaches cargo writes where cargo puts the outputs of the workspace it
      // finds from the directory it runs in, which would be inside the snapshot. Bound to that workspace's checkout target directory
      // when the reachable path proves the directory (cargoReachOf); refused before launch only when it reaches Rust unprovably.
      let cargoTarget = null;
      if (targetDirs && cargoShim) {
        // Advisory only: the wrapper binds or refuses each cargo the test starts, whatever path reaches it.
        const reach = cargoReachOf({ unitDir: npmDir(name), scripts: manifest.scripts, declared });
        if (reach.unprovable && Object.keys(targetDirs).length) notes.push(`npm unit ${name}: the static walk cannot prove where its cargo writes (${reach.unprovable}); the runtime cargo wrapper binds each invocation to its workspace's checkout target directory or refuses it`);
      } else if (targetDirs) {
        const unitDir = npmDir(name);
        const scriptsOf = (dir) => { try { const m = JSON.parse(readFile(path.join(contentRoot, ...(dir === "." ? [] : dir.split("/")), "package.json"))); return m?.scripts && typeof m.scripts === "object" ? m.scripts : {}; } catch { return null; } };
        const packageDirOf = (pkg) => {
          for (const [unit, entry] of npmMap ?? []) {
            if (unit === pkg) return entry.dir;
            try { if (JSON.parse(readFile(path.join(contentRoot, ...(entry.dir === "." ? [] : entry.dir.split("/")), "package.json"))).name === pkg) return entry.dir; } catch { /* unreadable: not this one */ }
          }
          return null;
        };
        // Every call the test reaches, not the first: one CARGO_TARGET_DIR binds the whole test, so all of them must be one workspace.
        const reach = cargoReachOf({ unitDir, scripts: manifest.scripts, declared, readScripts: scriptsOf, packageDirOf });
        const hasRust = Object.keys(targetDirs).length > 0;
        if (reach.unprovable) {
          if (hasRust) { refuse(`npm unit ${name}`, `its test reaches cargo while the compile reads a commit snapshot, and which workspace's outputs it writes cannot be proven (${reach.unprovable}); outputs must be the checkout's`, { npm: name }); continue; }
        } else if (reach.dir !== undefined || reach.dirs) {
          const workspaceOf = (from) => {
            let manifestDir = null;
            for (let d = from; ; d = path.posix.dirname(d) === "." || d === "." ? "." : path.posix.dirname(d)) {
              if (exists(path.join(contentRoot, ...(d === "." ? [] : d.split("/")), "Cargo.toml"))) { manifestDir = d; break; }
              if (d === ".") break;
            }
            const ws = manifestDir === null ? null : Object.hasOwn(targetDirs, manifestDir) ? manifestDir : ([...(rustMap?.values() ?? [])].find((u) => u.dir === manifestDir)?.cargoDir ?? null);
            return ws && targetDirs[ws] ? { ws } : { why: `its test reaches cargo from ${from}, ${manifestDir === null ? `which no Cargo.toml encloses` : `whose ${manifestDir}/Cargo.toml is not a known workspace or member`}; outputs must be the checkout's` };
          };
          const found = (reach.dirs ?? [reach.dir]).map(workspaceOf);
          const unknown = found.find((f) => f.why);
          if (unknown) { refuse(`npm unit ${name}`, unknown.why, { npm: name }); continue; }
          const workspaces = [...new Set(found.map((f) => f.ws))];
          if (workspaces.length > 1) { refuse(`npm unit ${name}`, `its test reaches cargo in more than one workspace (${workspaces.join(", ")} from ${reach.dirs.join(", ")}); one CARGO_TARGET_DIR binds the whole test, so one of them would write into another's outputs`, { npm: name }); continue; }
          cargoTarget = targetDirs[workspaces[0]];
        }
      }
      add({ id: `test:npm:${name}`, class: inClassA ? A : B, kind: "test", executes: true, classReason: inClassA ? `class A on recorded evidence (${classAEvidence}); run under the launch guard` : brokerCall ? "its test script reaches the broker (compiles); it runs with the compile scans" : laterReason ?? "test scripts execute project code and run after class A", ...(own.quietTests === true ? { quiet: true } : {}), ...(brokerCall ? { brokerCall } : {}), tool: "pnpm", args: ["--dir", npmDir(name), "run", "test"], env: { ...PNPM_RUN_ENV, ...(isolatedCheckout ? { RIGHTKIT_ISOLATED_CHECKOUT: "1" } : {}), ...(cargoTarget ? { CARGO_TARGET_DIR: cargoTarget } : {}) } });
    }
    if (deferredToCi.length) notes.push(`${deferredToCi.length} quiet test scan(s) left to the CI shards (one crate per runner), not run here: ${deferredToCi.join(", ")}`);
    // End-to-end tests only (Adrian, 2026-11-03): the scripts/ci self-tests were removed, so no `test:scripts-ci` scan is planned.
  }

  // Without a local compile grant the managed broker path is closed: every broker scan (class A
  // deny/audit/package list included) stays in the plan marked `deferred`, never dropped, never a pass
  // and never run outside the broker. A local gate treats any deferral as a failure (#888).
  const deferred = [];
  if (!localCompile) {
    scans = scans.map((x) => {
      if (x.tool !== "rustright" && !(x.tool === "cargo" && cargoCommand.length === 1)) return x;
      deferred.push(x.id);
      return { ...x, deferred: "no local compile grant; the broker refuses this scan" };
    });
    if (deferred.length) notes.push(`deferred, not passed (no local compile grant): ${deferred.join(", ")}`);
  }
  // Class B execution batching (see classBBatching above). A scan is batchable only when it is a carried-or-direct cargo compile with exactly one
  // `-p <crate>`, names no crate-specific feature (`--features`: unifying them across crates changes what compiles; the same rule as
  // clippyBatchPlan's crate-specific feature sets), is not a solo, separate or sidecar-bound crate, and is not deferred. Its group key is the
  // scan with the `-p` pair removed (tool, directory, root, environment, remaining argv, and where the pair sat), so only identical
  // configurations of one workspace and target share an invocation. The plan carries no argv for the group: runScans rebuilds it from members.
  const batchNotes = new Map(); const groupCount = new Map();
  scans = scans.map((x) => {
    if (x.batchUnit === undefined) return x;
    const { batchUnit: unit, ...plain } = x;
    if (!classBBatching || x.deferred || !Array.isArray(x.args) || (x.tool !== "rustright" && x.tool !== "cargo")) return plain;
    const at = x.args.indexOf("-p");
    const crate = at >= 0 ? x.args[at + 1] : null;
    const rest = at >= 0 ? [...x.args.slice(0, at), ...x.args.slice(at + 2)] : [];
    let why = null;
    if (at < 0 || typeof crate !== "string" || x.args.indexOf("-p", at + 1) >= 0) why = "it does not name exactly one `-p <crate>`";
    else if (rest.some((w) => /^(?:--features(?:=.*)?|-F.*)$/.test(w))) why = "its feature set names crate-specific features";
    else if (shardSolo.includes(unit) || shardSolo.includes(crateOf(unit))) why = "ci/shards.json solo";
    else if (clippySeparate instanceof Map && clippySeparate.has(unit)) why = String(clippySeparate.get(unit));
    else if (sidecarBoundCrate({ dir: path.join(contentRoot, rustDir(unit)), exists, readFile }).bound) why = "sidecar-bound";
    // NN42 (LOCKED 2026-11-06): one check/clippy per stage names every --target. The target is not part of the group key: rows that differ only
    // in their `--target <triple>` and that triple's own variables (CC_/CXX_/AR_/PKG_CONFIG_/RC_<triple>) share a key, and runScans runs crates
    // with the same set of targets as ONE invocation naming all of them, so Cargo overlaps the triples' graphs under one build lock.
    const ti = rest.indexOf("--target");
    const target = ti >= 0 && typeof rest[ti + 1] === "string" ? rest[ti + 1] : null;
    const tripleKey = target ? target.replace(/-/g, "_") : null;
    const keyRest = target ? [...rest.slice(0, ti), "--target", "*", ...rest.slice(ti + 2)] : rest;
    const keyEnv = target ? Object.fromEntries(Object.entries(x.env ?? {}).filter(([k]) => !(k.endsWith(`_${tripleKey}`) || k.toUpperCase().endsWith(`_${tripleKey.toUpperCase()}`)))) : x.env;
    if (why === "its feature set names crate-specific features" && mergeFeatureRows) {
      // Cargo's own flags only: after `--` come rustc/clippy flags, where -F is a lint level, never a feature.
      const feats = []; const plainRest = []; const stop = keyRest.indexOf("--") < 0 ? keyRest.length : keyRest.indexOf("--");
      for (let i = 0; i < keyRest.length; i += 1) {
        const w = keyRest[i];
        if (i >= stop) { plainRest.push(w); continue; }
        if (w === "--features" || w === "-F") { feats.push(...String(keyRest[++i] ?? "").split(/[ ,]+/).filter(Boolean)); continue; }
        const m = /^(?:--features=|-F)(.+)$/.exec(w); if (m) { feats.push(...m[1].split(/[ ,]+/).filter(Boolean)); continue; }
        plainRest.push(w);
      }
      const key = sha256(canonicalJson({ tool: x.tool, cwd: x.cwd ?? ".", root: x.root, env: keyEnv, rest: plainRest, at, mergedFeatures: true })).slice(0, 16);
      groupCount.set(key, (groupCount.get(key) ?? 0) + 1);
      return { ...plain, batchable: { key, at, crate, unit, ...(target ? { target } : {}), features: feats.map((f) => (f.includes("/") ? f : `${crate}/${f}`)) } };
    }
    if (why) {
      batchNotes.set(why, (batchNotes.get(why) ?? 0) + 1);
      // A row that cannot join other crates (its own features, solo, separate, sidecar-bound) still names every target with its own crate's
      // other target rows: the key keeps the crate, so only this crate's rows that differ by --target share an invocation (Mac 2026-11-07:
      // rightkit-tauri-shell's feature sets ran one target at a time, 1.8 rows/min).
      if (!target || at < 0 || typeof crate !== "string") return plain;
      const ownKey = sha256(canonicalJson({ tool: x.tool, cwd: x.cwd ?? ".", root: x.root, env: keyEnv, rest: keyRest, at, crate, only: "targets" })).slice(0, 16);
      groupCount.set(ownKey, (groupCount.get(ownKey) ?? 0) + 1);
      return { ...plain, batchable: { key: ownKey, at, crate, unit, target } };
    }
    const key = sha256(canonicalJson({ tool: x.tool, cwd: x.cwd ?? ".", root: x.root, env: keyEnv, rest: keyRest, at })).slice(0, 16);
    groupCount.set(key, (groupCount.get(key) ?? 0) + 1);
    return { ...plain, batchable: { key, at, crate, unit, ...(target ? { target } : {}) } };
  });
  if (groupCount.size) {
    const grouped = [...groupCount.values()].filter((n) => n > 1);
    notes.push(`class B batching: ${scans.filter((x) => x.batchable).length} compile scan(s) in ${groupCount.size} configuration group(s); each group of one Cargo workspace and target runs as one cargo invocation (${grouped.reduce((a, n) => a + n, 0)} scan(s) in ${grouped.length} shared group(s)), a failed group is bisected to the crate that fails, every row keeps its own id and command; the batch-equivalence run (GATE-CONTRACT GC-I11, item 8 row 8.7) has NOT been run, so a crate whose own features differ when unified with its group's can pass here and fail alone`);
    for (const [why, n] of batchNotes) notes.push(`class B batching: ${n} compile scan(s) stay their own invocation: ${why}`);
  }
  const ids = scans.map((s) => s.id);
  const dup = ids.find((id, i) => ids.indexOf(id) !== i);
  if (dup) throw new Error(`planScans: duplicate scan id ${dup}`);
  const classDigest = (cls) => digestScanSet(scans.filter((s) => s.class === cls));
  return {
    schema: SCAN_RESULT_SCHEMA, roots: { contentRoot, liveRoot, ...(cargoLiveRoot ? { cargoLiveRoot } : {}) }, changed: files, inputDigest, scans, notes, deferred, qualifyOwed,
    scanSetDigest: digestScanSet(scans), classDigests: { A: classDigest(A), B: classDigest(B) },
  };
}

// ---- runner -------------------------------------------------------------------------------------------

// An argument that may reach a .cmd/.bat shim (which cmd.exe parses) must be plain; anything else is rejected, never escaped.
const SAFE_SHIM_ARG = /^[A-Za-z0-9_.,:=@+\/\\-]+$/;
const SAFE_SHIM_PATH = /^[A-Za-z0-9_.,:=@+ \/\\()-]+$/;

/**
 * How to start `command` without a shell. POSIX: as given. Windows: resolve it on PATH here (never the
 * current directory), prefer .exe/.com, run an npm-style pnpm.cmd through node and its pnpm.cjs, and
 * run any other .cmd/.bat shim only when its path and every argument pass a strict allowlist.
 * Returns { file, args, verbatim } or { error }.
 */
export function resolveInvocation(command, args, { platform = process.platform, env = process.env, exists = fs.existsSync, execPath = process.execPath, readFile = (f) => fs.readFileSync(f, "utf8") } = {}) {
  if (platform !== "win32") return { file: command, args, verbatim: false };
  const hasExt = /\.[A-Za-z0-9]{1,4}$/.test(command);
  const pathKey = Object.keys(env).find((k) => k.toLowerCase() === "path");
  const dirs = /[\\/]/.test(command) ? [""] : String(env[pathKey] ?? "").split(";").filter(Boolean);
  const exts = String(env.PATHEXT || ".COM;.EXE;.BAT;.CMD").split(";").filter(Boolean).map((e) => e.toLowerCase());
  let found = null;
  for (const dir of dirs) {
    const base = dir ? path.win32.join(dir, command) : command;
    for (const ext of hasExt ? [""] : exts) if (!found && exists(base + ext)) found = base + ext;
  }
  if (!found) return { error: Object.assign(new Error(`spawn ${command} ENOENT`), { code: "ENOENT" }) };
  if (!/\.(cmd|bat)$/i.test(found)) return { file: found, args, verbatim: false };
  const cjs = path.win32.join(path.win32.dirname(found), "node_modules", "pnpm", "bin", "pnpm.cjs");
  if (/^pnpm\.cmd$/i.test(path.win32.basename(found)) && exists(cjs)) return { file: execPath, args: [cjs, ...args], verbatim: false };
  // A Node cmd-shim (what npm, pnpm and nvm install: `"%~dp0\...\entry.js" %*`, npm's `SET "NPM_CLI_JS=%~dp0\...npm-cli.js"`) runs
  // its JS entry with node: started here as node + that entry, no shell, so any argument passes unchanged (rightkit#1261).
  const entry = cmdShimEntry(found, { readFile, exists });
  if (entry) return { file: execPath, args: [entry, ...args], verbatim: false };
  const bad = args.find((a) => !SAFE_SHIM_ARG.test(a));
  if (bad !== undefined) return { error: new Error(`refusing to pass ${JSON.stringify(bad)} to the shell shim ${found}`) };
  if (!SAFE_SHIM_PATH.test(found)) return { error: new Error(`refusing to run shell shim ${found}: unsafe path`) };
  // Outer quotes around the whole line: with /s cmd strips exactly one leading and one trailing quote.
  return { file: env.ComSpec || "cmd.exe", args: ["/d", "/s", "/c", `""${found}"${args.map((a) => ` ${a}`).join("")}"`], verbatim: true };
}

/**
 * The JS entry a Node cmd-shim at `shim` runs, or null: the line that passes the caller's arguments (`%*`) names it, either as a
 * `%~dp0`-relative path (`"%~dp0\\..\\pnpm.mjs" %*`) or through a variable set to one (npm's `"%NODE_EXE%" "%NPM_CLI_JS%" %*` with
 * `SET "NPM_CLI_JS=%~dp0\\node_modules\\npm\\bin\\npm-cli.js"`). Helpers the shim runs on other lines (npm's npm-prefix.js) are not it.
 */
export function cmdShimEntry(shim, { readFile = (f) => fs.readFileSync(f, "utf8"), exists = fs.existsSync } = {}) {
  let text;
  try { text = String(readFile(shim)); } catch { return null; }
  const vars = new Map();
  // Every value a variable is set to (npm's NPM_CLI_JS is reassigned to a global-prefix copy when one exists): the %~dp0 one is used.
  for (const m of text.matchAll(/^\s*@?SET\s+"?([A-Za-z_][A-Za-z0-9_]*)=([^"\r\n]*)"?\s*$/gim)) vars.set(m[1].toUpperCase(), [...(vars.get(m[1].toUpperCase()) ?? []), m[2]]);
  const fromDp0 = (value) => {
    const m = /^%~dp0\\?(.+\.(?:mjs|cjs|js))$/i.exec(String(value ?? "").trim());
    if (!m) return null;
    const entry = path.win32.resolve(path.win32.dirname(shim), m[1]);
    return exists(entry) ? entry : null;
  };
  for (const line of text.split(/\r?\n/)) {
    if (!line.includes("%*")) continue;
    for (const q of line.matchAll(/"([^"]+)"/g)) {
      const token = q[1];
      const direct = fromDp0(token);
      if (direct) return direct;
      const v = /^%([A-Za-z_][A-Za-z0-9_]*)%$/.exec(token);
      if (v) for (const value of vars.get(v[1].toUpperCase()) ?? []) { const viaVar = fromDp0(value); if (viaVar) return viaVar; }
    }
  }
  return null;
}

/** Default spawn: synchronous, no console window, never `shell: true`. */
export function defaultSpawn(command, args, options = {}) {
  if (options.boundedMs) return boundedSpawn(command, args, options);
  const inv = resolveInvocation(command, args, { env: options.env ?? process.env });
  if (inv.error) return { status: null, stdout: "", stderr: "", error: inv.error };
  return spawnSync(inv.file, inv.args, { encoding: "utf8", windowsHide: true, maxBuffer: 64 * 1024 * 1024, windowsVerbatimArguments: inv.verbatim, ...options, shell: false });
}

const OUTPUT_KEEP = 4 * 1024 * 1024;
/**
 * defaultSpawn with a deadline (`options.boundedMs`), for short identity probes that must never hang the caller: the command runs
 * in a helper node process through defaultSpawnAsync, which on the deadline kills the command's whole process tree
 * (killProcessTree) and reports `timedOut: true`. The helper records the command's pid first; if the helper itself overruns its
 * backstop (`backstopMs`, default deadline + 15 s), it is killed and the command's tree is killed from here by that pid, so no
 * probe outlives the call. A launch failure keeps its `error` (e.g. ENOENT); a command that ran keeps its `status`.
 */
export function boundedSpawn(command, args, { boundedMs, backstopMs = boundedMs + 15000, cwd, env, pidFile = null } = {}) {
  const dir = pidFile ? null : fs.mkdtempSync(path.join(os.tmpdir(), "rk-probe-"));
  const pids = pidFile ?? path.join(dir, "pid");
  const script = `import(${JSON.stringify(new URL(import.meta.url).href)}).then(async (m) => {
    const [command, args, ms, pidFile] = JSON.parse(process.argv[1]); const fs = await import("node:fs");
    const ac = new AbortController(); const timer = setTimeout(() => ac.abort(), ms);
    const r = await m.defaultSpawnAsync(command, args, { signal: ac.signal, onSpawn: (pid) => fs.writeFileSync(pidFile, String(pid)) }); clearTimeout(timer);
    process.stdout.write(JSON.stringify({ status: r.status, stdout: r.stdout, stderr: r.stderr, timedOut: r.cancelled === true, error: r.cancelled ? null : (r.error ? { code: r.error.code ?? null, message: r.error.message } : null) }));
  });`;
  try {
    const r = spawnSync(process.execPath, ["--input-type=module", "-e", script, JSON.stringify([command, args, boundedMs, pids])], { cwd, env: env ?? process.env, encoding: "utf8", windowsHide: true, stdio: ["ignore", "pipe", "pipe"], timeout: backstopMs, killSignal: "SIGKILL", maxBuffer: 16 * 1024 * 1024 });
    const timeout = (why) => ({ status: null, stdout: "", stderr: "", timedOut: true, error: Object.assign(new Error(`timed out after ${boundedMs} ms${why}`), { code: "ETIMEDOUT" }) });
    if (r.error?.code === "ETIMEDOUT") {
      let pid = null; try { pid = Number(fs.readFileSync(pids, "utf8")) || null; } catch { /* never started */ }
      if (pid) killProcessTree({ pid });
      return timeout(" (backstop)");
    }
    try {
      const o = JSON.parse(r.stdout);
      if (o.timedOut) return timeout("");
      return { status: o.status, stdout: o.stdout ?? "", stderr: o.stderr ?? "", ...(o.error ? { error: Object.assign(new Error(o.error.message), { code: o.error.code }) } : {}) };
    } catch { return { status: null, stdout: "", stderr: r.stderr ?? "", error: new Error(`the bounded probe gave no result (${r.error?.message ?? `exit ${r.status}`})`) }; }
  } finally { if (dir) fs.rmSync(dir, { recursive: true, force: true }); }
}

/** The environment additions that load the class A launch guard into a test row's Node processes (NODE_OPTIONS appended, not replaced). */
export function launchGuardEnv(env = process.env) {
  const current = String(env.NODE_OPTIONS ?? "");
  return { RIGHTKIT_CLASS_A_LAUNCH_GUARD: "1", NODE_OPTIONS: current.includes(LAUNCH_GUARD_IMPORT) ? current : `${current} ${LAUNCH_GUARD_IMPORT}`.trim() };
}

/** Kill `child` and everything it started: taskkill /T /F on Windows, the whole process group elsewhere (the child leads one when spawned with a signal). */
export function killProcessTree(child, { platform = process.platform, run = spawnSync, kill = (pid, sig) => process.kill(pid, sig) } = {}) {
  if (!child?.pid) return;
  if (platform === "win32") {
    try { run("taskkill", ["/T", "/F", "/PID", String(child.pid)], { windowsHide: true, stdio: "ignore" }); } catch { /* already gone */ }
    return;
  }
  try { kill(-child.pid, "SIGKILL"); } catch { try { child.kill("SIGKILL"); } catch { /* already gone */ } }
}

/**
 * The same invocation as defaultSpawn without blocking, so scans can run side by side. Keeps the last 4 MiB of each stream.
 * With `options.signal` (an AbortSignal), an abort kills the whole process tree and the result carries `cancelled: true`.
 * With `options.stdoutFile`, stdout is written to that file whole instead of kept in memory.
 */
export function defaultSpawnAsync(command, args, options = {}) {
  const { signal, stdoutFile, onSpawn, ...rest } = options;
  const inv = resolveInvocation(command, args, { env: rest.env ?? process.env });
  if (inv.error) return Promise.resolve({ status: null, stdout: "", stderr: "", error: inv.error });
  if (signal?.aborted) return Promise.resolve({ status: null, stdout: "", stderr: "", error: new Error("cancelled"), cancelled: true });
  return new Promise((resolve) => {
    let stdout = ""; let stderr = ""; let error = null;
    let truncated = false;
    const keep = (buf, chunk) => { const next = buf + chunk; if (next.length > OUTPUT_KEEP) { truncated = true; return next.slice(-OUTPUT_KEEP); } return next; };
    let child;
    try {
      // stdin is closed: no scan reads it, and an open pipe nobody writes can block a tool that probes it (Windows).
      // A detached child (POSIX, its own group for the abort kill) is recorded under the held build ownership (build-ownership.mjs).
      child = spawnUnderOwnership(spawnChild, inv.file, inv.args, { windowsHide: true, windowsVerbatimArguments: inv.verbatim, stdio: ["ignore", "pipe", "pipe"], ...(signal && process.platform !== "win32" ? { detached: true } : {}), ...rest, shell: false, stdio: ["ignore", "pipe", "pipe"] });
    } catch (e) { resolve({ status: null, stdout: "", stderr: "", error: e }); return; }
    if (onSpawn && child.pid) onSpawn(child.pid);
    const onAbort = () => killProcessTree(child);
    signal?.addEventListener("abort", onAbort, { once: true });
    child.stdout.setEncoding("utf8"); child.stderr.setEncoding("utf8");
    const sink = stdoutFile ? fs.createWriteStream(stdoutFile) : null;
    if (sink) child.stdout.pipe(sink); else child.stdout.on("data", (c) => { stdout = keep(stdout, c); });
    child.stderr.on("data", (c) => { stderr = keep(stderr, c); });
    child.on("error", (e) => { error = e; });
    child.on("close", async (status) => {
      if (sink) await new Promise((done) => (sink.writableFinished ? done() : sink.on("finish", done).on("error", done)));
      signal?.removeEventListener("abort", onAbort);
      const cancelled = signal?.aborted === true;
      resolve({ status, stdout, stderr, ...(truncated ? { truncated: true } : {}), ...(error ? { error } : cancelled ? { error: new Error("cancelled") } : {}), ...(cancelled ? { cancelled: true } : {}) });
    });
    if (signal?.aborted) onAbort();
  });
}

// Lanes for concurrent class B runs. The broker admits check and clippy side by side; rustdoc and
// package are serialized by the broker anyway. Tests and repository code run after the compile lanes drain, `testJobs` at a
// time; a test scan marked `quiet` (ci/gate.config.json `rust.<crate>.quietTests`: the crate's tests share ports, devices,
// wall-clock budgets or the broker's own queue, so they fail under load) runs last, one at a time, under the broker's quiet
// lease when the gate can take one (runScans `quietLane`).
/** The counts a test runner printed last: node:test's `ℹ tests N` / `ℹ pass` / `ℹ fail` / `ℹ skipped` / `ℹ todo` lines, or vitest's
 * `Tests  N passed | M skipped | K failed (T)` line. Null when neither is present. Several node:test summaries (a script that runs
 * the runner more than once) are summed. */
/** null when a test summary is evidence of a pass; otherwise { code, detail }. Used for fresh and cached test rows alike. */
/** TOML text without its comments: a `#` outside a basic or literal string starts one (a `#` inside quotes is kept). */
export function stripTomlComments(text) {
  return String(text).split("\n").map((line) => {
    let quote = null;
    for (let i = 0; i < line.length; i += 1) {
      const ch = line[i];
      if (quote) { if (ch === "\\" && quote === '"') i += 1; else if (ch === quote) quote = null; }
      else if (ch === '"' || ch === "'") quote = ch;
      else if (ch === "#") return line.slice(0, i);
    }
    return line;
  }).join("\n");
}

export function testResultsProblem(results) {
  if (!results) return { code: "NO_TEST_SUMMARY", detail: "no test counts recorded" };
  // End-to-end tests only (Adrian, 2026-11-03: "End to end tests ONLY any unit tests need strict approval along with evidence and
  // reason"): a Rust crate whose test build runs no test is a pass, recorded with its zero counts; so is an npm test scan whose runner (node:test, vitest) reports zero tests and no failure. A test that failed still fails, and a
  // run whose tests were all skipped or ignored still proves nothing (ALL_SKIPPED below).
  if (["libtest", "node:test", "vitest"].includes(results.runner) && !(results.tests > 0) && !(results.fail > 0)) return null;
  if (!(results.tests > 0)) return { code: "NO_TESTS", detail: "the test scan executed no test" };
  if (results.fail > 0) return { code: "SUMMARY_FAILED", detail: `the test runner exited 0 but its summary reports ${results.fail} failed test(s)` };
  // A suite that ran tests but passed none of them (every test skipped or todo) proves nothing (D17, rightkit#1236).
  if (!(results.pass > 0)) return { code: "ALL_SKIPPED", detail: `the test scan passed no test: ${results.skipped ?? 0} skipped, ${results.todo ?? 0} todo of ${results.tests}` };
  return null;
}

export function parseTestSummary(text) {
  const lines = String(text ?? "").split(/\r?\n/);
  const node = { tests: 0, pass: 0, fail: 0, skipped: 0, todo: 0 }; let sawNode = false;
  for (const line of lines) {
    const m = /^[ℹ#]\s+(tests|pass|fail|skipped|todo)\s+(\d+)\s*$/.exec(line.trim());
    if (m) { node[m[1]] += Number(m[2]); sawNode = true; }
  }
  if (sawNode) return { runner: "node:test", ...node };
  const vt = lines.map((l) => l.replace(/\x1b\[[0-9;]*m/g, "")).reverse().find((l) => /^\s*Tests\s+.*\(\d+\)\s*$/.test(l));
  if (vt) {
    const n = (re) => Number((re.exec(vt) ?? [0, 0])[1]);
    const tests = Number(/\((\d+)\)\s*$/.exec(vt)[1]);
    return { runner: "vitest", tests, pass: n(/(\d+) passed/), fail: n(/(\d+) failed/), skipped: n(/(\d+) skipped/), todo: n(/(\d+) todo/) };
  }
  return null;
}

// ---- Rust tests run outside the broker (GATE-CONTRACT GC-A08) -------------------------------------------------------------------------
// On a broker host a Rust test scan compiles through the broker with the plain test command and --message-format=json (the broker adds
// --no-run itself) and the gate then runs the listed test executables itself, never through the broker. A direct host (no broker) keeps the
// one-step test command and uses none of this. Doctests
// are not planned: the broker refuses `test --doc`, and a --no-run listing has no doctest executable. These are the pure
// parts of that second phase; they mirror crates/rightkit-qa/src/run_tests (artifacts.rs, exec.rs, libtest.rs).

/** The package name and version of a cargo package id: `path+file:///w/fx#fx@0.1.0`, `path+file:///w/fx#0.1.0` (the name is the last
 * path segment) or the old `fx 0.1.0 (path+file:///w/fx)`. Null parts when the id says nothing usable. */
export function packageOfId(id) {
  const text = String(id ?? "");
  const old = /^([^\s#@]+) (\d[^\s]*) \(/.exec(text);
  if (old) return { name: old[1], version: old[2] };
  const hash = text.indexOf("#");
  if (hash === -1) return { name: null, version: null };
  const fragment = text.slice(hash + 1);
  const at = fragment.indexOf("@");
  if (at > 0) return { name: fragment.slice(0, at), version: fragment.slice(at + 1) || null };
  if (/^\d/.test(fragment)) {
    const segments = text.slice(0, hash).replace(/\/+$/, "").split("/");
    return { name: segments.at(-1) || null, version: fragment };
  }
  return { name: fragment || null, version: null };
}

/**
 * What a `cargo test --no-run --message-format=json` listing says. Only `compiler-artifact` records of the test profile that name an
 * executable are test executables (every unit-test binary and integration test; never a build script, library or plain binary). Lines
 * that are not JSON objects (a broker banner) are ignored. `buildFinished` is true when every `build-finished` record said success,
 * false when one said otherwise, null when the listing has none (a compile cut short).
 */
export function parseTestListing(text) {
  const out = { binaries: [], buildFinished: null, artifacts: 0 };
  const seen = new Set();
  for (const raw of String(text ?? "").split(/\r?\n/)) {
    const line = raw.trimStart();
    if (!line.startsWith("{")) continue;
    let v; try { v = JSON.parse(line); } catch { continue; }
    if (v?.reason === "build-finished") out.buildFinished = (out.buildFinished ?? true) && v.success === true;
    else if (v?.reason === "compiler-artifact") {
      out.artifacts += 1;
      if (v.profile?.test !== true || typeof v.executable !== "string" || v.executable === "" || seen.has(v.executable)) continue;
      seen.add(v.executable);
      const pkg = packageOfId(v.package_id);
      const manifest = typeof v.manifest_path === "string" ? v.manifest_path : "";
      const target = typeof v.target?.name === "string" ? v.target.name : "";
      out.binaries.push({ package: pkg.name ?? target, version: pkg.version, target, kind: Array.isArray(v.target?.kind) ? v.target.kind.filter((k) => typeof k === "string") : [], executable: v.executable, manifestDir: manifest ? path.dirname(manifest) : "" });
    }
  }
  out.binaries.sort((a, b) => (a.executable < b.executable ? -1 : a.executable > b.executable ? 1 : 0));
  return out;
}

/** What a cargo `--message-format=json` stream says per package (NN40): the error diagnostics and whether a build artifact was reported. `{ records, errors: {name: {count, rendered[]}}, artifacts: {name: count} }`. */
export function summarizeCargoJson(text) {
  const out = { records: 0, errors: {}, artifacts: {} };
  for (const raw of String(text ?? "").split(/\r?\n/)) {
    const line = raw.trimStart();
    if (!line.startsWith("{")) continue;
    let v; try { v = JSON.parse(line); } catch { continue; }
    if (!v || typeof v.reason !== "string") continue;
    out.records += 1;
    const name = packageOfId(v.package_id).name;
    if (!name) continue;
    if (v.reason === "compiler-message") {
      const level = String(v.message?.level ?? "");
      if (level !== "error" && !level.startsWith("error: internal")) continue;
      const e = (out.errors[name] ??= { count: 0, rendered: [] });
      e.count += 1;
      if (e.rendered.length < 4 && typeof v.message?.rendered === "string") e.rendered.push(v.message.rendered.trimEnd().slice(0, 700));
    } else if (v.reason === "compiler-artifact") {
      if ((Array.isArray(v.target?.kind) ? v.target.kind : []).includes("custom-build")) continue;
      out.artifacts[name] = (out.artifacts[name] ?? 0) + 1;
    }
  }
  return out;
}

/**
 * The per-crate verdicts of a failed grouped doc invocation, from its JSON summary: `[{ unit, fail, detail }]` in member order, or null when the messages
 * cannot attribute (no records, or no member reported an artifact, so "no artifact" would prove nothing): the caller then bisects as for any grouped failure.
 */
export function docVerdicts(summary, units, crates) {
  if (!summary || !(summary.records > 0) || !Array.isArray(units) || units.length !== crates.length) return null;
  if (!crates.some((c) => summary.artifacts?.[c] > 0)) return null;
  return units.map((unit, k) => {
    const err = summary.errors?.[crates[k]];
    if (err?.count) return { unit, fail: true, detail: `${err.count} error(s) in ${crates[k]}: ${err.rendered.join(" | ")}` };
    if (!(summary.artifacts?.[crates[k]] > 0)) return { unit, fail: true, detail: `no documentation artifact for ${crates[k]}: it did not build (a dependency or build script failed before it; see the invocation's output)` };
    return { unit, fail: false, detail: "" };
  });
}

/** The text a failed compile leaves for a reader: the rendered error diagnostics of the JSON listing plus every non-JSON line (a broker refusal). */
export function listingFailureText(text, limit = 8000) {
  const out = [];
  for (const raw of String(text ?? "").split(/\r?\n/)) {
    const line = raw.trimEnd();
    if (!line.trim()) continue;
    if (!line.trimStart().startsWith("{")) { out.push(line); continue; }
    try { const v = JSON.parse(line); if (v?.reason === "compiler-message" && v.message?.level === "error" && typeof v.message.rendered === "string") out.push(v.message.rendered.trimEnd()); } catch { out.push(line.slice(0, 300)); }
  }
  return out.join("\n").slice(-limit);
}

/** Sum of every `test result: ok. N passed; M failed; ...` line a libtest binary printed; null when it printed none (a custom harness, a crash). A line counts only when it starts the line and carries both a passed and a failed count. */
export function parseLibtestSummary(text) {
  const sum = { results: 0, ok: true, passed: 0, failed: 0, ignored: 0, measured: 0, filteredOut: 0 };
  for (const line of String(text ?? "").split(/\r?\n/)) {
    const m = /^test result: (ok|FAILED)\. (.*)$/.exec(line.trimEnd());
    if (!m) continue;
    const counts = {}; for (const c of m[2].matchAll(/(\d+) (passed|failed|ignored|measured|filtered out)/g)) counts[c[2]] = Number(c[1]);
    if (counts.passed === undefined || counts.failed === undefined) continue;
    sum.results += 1; sum.ok = sum.ok && m[1] === "ok";
    sum.passed += counts.passed; sum.failed += counts.failed; sum.ignored += counts.ignored ?? 0; sum.measured += counts.measured ?? 0; sum.filteredOut += counts["filtered out"] ?? 0;
  }
  return sum.results > 0 ? sum : null;
}

const TEST_RUN_OPS = Object.freeze({
  realpath: (p) => fs.realpathSync.native(p),
  isFile: (p) => fs.statSync(p).isFile(),
  isDir: (p) => fs.statSync(p).isDirectory(),
  digestFile: (p) => sha256(fs.readFileSync(p)),
});

const insideDir = (real, dirReal, platform) => {
  const mod = platform === "win32" ? path.win32 : path;
  const fold = (p) => (platform === "win32" ? p.toLowerCase() : p);
  const rel = mod.relative(fold(dirReal), fold(real));
  return rel !== "" && !mod.isAbsolute(rel) && rel.split(/[\\/]/)[0] !== "..";
};

/**
 * Every test executable must be a regular file whose real path is inside the verified output directory (`boundDirs`: the receipt's
 * outputDir on a broker host, the scan's bound target directory otherwise); with `generationRoots` its lexical directory must also be a
 * managed generation target. An executable anywhere else is refused: the gate never runs a binary the verified compile did not write.
 * Returns { accepted: [binary + real + sha256], problems: [text] }.
 */
export function containTestExecutables(binaries, { boundDirs, generationRoots = null, platform = process.platform, ops = TEST_RUN_OPS } = {}) {
  const mod = platform === "win32" ? path.win32 : path;
  const problems = []; const accepted = [];
  const dirs = [];
  for (const d of boundDirs ?? []) { try { dirs.push(ops.realpath(d)); } catch { /* an output directory that does not exist contains nothing */ } }
  if (dirs.length === 0) return { accepted, problems: [`no verified output directory exists (${(boundDirs ?? []).join(", ") || "none bound"}); no test executable can be shown to come from the compile`] };
  for (const b of binaries) {
    const exe = b.executable;
    if (!mod.isAbsolute(exe)) { problems.push(`${exe} is not an absolute path`); continue; }
    if (generationRoots && !isInsideGenerationTarget(mod.dirname(exe), generationRoots, platform)) { problems.push(`${exe} is not inside a managed generation target`); continue; }
    let real;
    try { real = ops.realpath(exe); if (!ops.isFile(real)) throw new Error("not a regular file"); } catch (e) { problems.push(`${exe} is not a readable file (${e.message})`); continue; }
    if (!dirs.some((d) => insideDir(real, d, platform))) { problems.push(`${exe} resolves to ${real}, outside the verified output directory ${dirs.join(", ")}`); continue; }
    let sha = null; try { sha = ops.digestFile(real); } catch (e) { problems.push(`${exe} cannot be read (${e.message})`); continue; }
    accepted.push({ ...b, real, sha256: sha });
  }
  return { accepted, problems };
}

/**
 * Run the accepted test executables one after another, outside the broker, each from its package directory with the CARGO_MANIFEST_DIR,
 * CARGO_PKG_NAME and CARGO_PKG_VERSION cargo would set, with `scan.runOutside.args`, under one deadline (`limitMs`) that kills the
 * process tree. Every executable runs (no-fail-fast); the row fails if any failed. The judgment is per crate (the scan), not per
 * executable: the libtest summaries are summed and the same rules as a node:test row apply (testResultsProblem), because many test
 * binaries legitimately run 0 tests. Returns { status, code?, detail?, results, executables }.
 */
/**
 * Every accepted test executable must be in the broker's signed list (receipt.testExecutables: [{path, sha256, ...}]) with the same sha256.
 * A null list (the broker could not enumerate them, e.g. no JSON output) refuses everything, naming the broker's reason.
 */
export function signedTestExecutableProblems(accepted, signed, reason = null, platform = process.platform) {
  if (!Array.isArray(signed)) return [`the broker signed no test executable list (${reason ?? "none given"})`];
  const norm = (p) => { const r = (platform === "win32" ? path.win32 : path.posix).normalize(String(p)); return platform === "win32" ? r.toLowerCase() : r; };
  const byPath = new Map(signed.map((e) => [norm(e?.path), e]));
  const problems = [];
  for (const b of accepted) {
    const e = byPath.get(norm(b.executable)) ?? byPath.get(norm(b.real));
    if (!e) problems.push(`${b.executable} is not in the broker's signed test executables`);
    else if (e.sha256 !== b.sha256) problems.push(`${b.executable} has sha256 ${b.sha256}, the broker signed ${e.sha256}`);
  }
  return problems;
}

/** Test executables run at once (ADR-001 C2, rightkit#1454): the host's physical cores minus 2, at least 1. */
export function testRunConcurrency({ cpus = os.cpus().length } = {}) {
  return Math.max(1, cpus - 2);
}

export async function runTestBinaries({ scan, binaries, spawn, env, limitMs, signal = null, ops = TEST_RUN_OPS, concurrency = testRunConcurrency() }) {
  const args = [...(scan.runOutside?.args ?? [])];
  const ctl = new AbortController();
  const startedAt = Date.now();
  const clock = setTimeout(() => ctl.abort(), limitMs);
  const runSignal = signal ? AbortSignal.any([signal, ctl.signal]) : ctl.signal;
  const executables = []; const failures = []; const total = { tests: 0, pass: 0, fail: 0, skipped: 0, todo: 0 };
  let stopped = null;
  // ADR-001 C2 (kept by Adrian 2026-11-02, rightkit#1454): the executables run in parallel, `concurrency` at a time (physical cores minus 2), each
  // recording into its own slot so the report keeps the listing order.
  const slots = new Array(binaries.length);
  // rightkit#1512: each executable's own stdout/stderr, kept for the row's failure log (the cargo compile output is not the test run's).
  const outputs = new Array(binaries.length);
  const runOne = async (b, index) => {
    const executables = { push: (e) => { slots[index] = e; } };
    {
      const label = `${b.package}::${b.target}`;
      if (runSignal.aborted) { stopped = signal?.aborted ? "CANCELLED" : "TIMEOUT"; executables.push({ label, sha256: b.sha256, status: "not-run" }); return; }
      try { if (!b.manifestDir || !ops.isDir(b.manifestDir)) { failures.push(`${label}: its package directory ${b.manifestDir || "(none)"} does not exist`); executables.push({ label, sha256: b.sha256, status: "fail" }); return; } } catch (e) { failures.push(`${label}: its package directory ${b.manifestDir} is unreadable (${e.message})`); executables.push({ label, sha256: b.sha256, status: "fail" }); return; }
      const childEnv = { ...env, CARGO_MANIFEST_DIR: b.manifestDir, CARGO_PKG_NAME: b.package, ...(b.version ? { CARGO_PKG_VERSION: b.version } : {}) };
      const remaining = Math.max(1, limitMs - (Date.now() - startedAt));
      let r;
      try { r = await spawn(b.real, args, { cwd: b.manifestDir, env: childEnv, signal: runSignal, boundedMs: remaining }); } catch (e) { r = { error: e }; }
      if (runSignal.aborted && (!r || r.error || r.status !== 0)) { stopped = signal?.aborted ? "CANCELLED" : "TIMEOUT"; executables.push({ label, sha256: b.sha256, status: "not-run" }); return; }
      if (r?.error?.code === "ETIMEDOUT" || r?.timedOut === true) { stopped = "TIMEOUT"; executables.push({ label, sha256: b.sha256, status: "fail" }); failures.push(`${label}: killed with its process tree at the run limit`); return; }
      outputs[index] = { label, exit: r?.status ?? null, signal: r?.signal ?? null, stdout: String(r?.stdout ?? ""), stderr: String(r?.stderr ?? "") };
      const summary = parseLibtestSummary(`${r?.stdout ?? ""}\n${r?.stderr ?? ""}`);
      if (summary) { total.tests += summary.passed + summary.failed + summary.ignored; total.pass += summary.passed; total.fail += summary.failed; total.skipped += summary.ignored; }
      const output = `${r?.stdout ?? ""}\n${r?.stderr ?? ""}`.replace(/\u001b\[[0-9;?]*[ -/]*[@-~]/g, "");
      const skipMarkers = output.split(/\r\n|\n|\r/).filter((line) => /(?:^|[\s\]:])SKIP(?=[\s:]|$)/.test(line)).length;
      const entry = { label, sha256: b.sha256, outputTruncated: r?.truncated === true, skipMarkers, status: "pass", ...(summary ? { passed: summary.passed, failed: summary.failed, ignored: summary.ignored } : {}) };
      if (!r || r.error) { entry.status = "fail"; failures.push(`${label}: ${r?.error?.message ?? "no result"}`); }
      else if (r.status !== 0) { entry.status = "fail"; failures.push(`${label}: ${failureDetail(r) || `exit ${r.status}`}`); }
      else if (summary && (!summary.ok || summary.failed > 0)) { entry.status = "fail"; failures.push(`${label}: exited 0 but its summary reports ${summary.failed} failed test(s)`); }
      executables.push(entry);
    }
  };
  try {
    let next = 0;
    const worker = async () => { while (next < binaries.length) { const i = next; next += 1; await runOne(binaries[i], i); } };
    await Promise.all(Array.from({ length: Math.max(1, Math.min(concurrency, binaries.length)) }, () => worker()));
    for (const e of slots) if (e) executables.push(e);
  } finally { clearTimeout(clock); }
  const results = { runner: "libtest", ...total, binaries: executables.length };
  if (stopped === "CANCELLED") return { status: "not-run", code: "CANCELLED", detail: "cancelled while running", results, executables, outputs: outputs.filter(Boolean) };
  if (stopped === "TIMEOUT") return { status: "fail", code: "TIMEOUT", detail: `${scan.id}: its test executables ran longer than the ${Math.round(limitMs / 1000)} s run limit and were killed with their process tree${failures.length ? `; ${failures.slice(0, 3).join(" | ")}` : ""}`, results, executables, outputs: outputs.filter(Boolean) };
  if (failures.length) return { status: "fail", code: "TEST_FAILED", detail: `${failures.length} of ${executables.length} test executable(s) failed: ${failures.slice(0, 5).join(" | ")}`.slice(0, 4000), results, executables, outputs: outputs.filter(Boolean) };
  const problem = testResultsProblem(results);
  if (problem) return { status: "fail", code: problem.code, detail: `${problem.detail} (summed over ${executables.length} test executable(s) of ${scan.id})`, results, executables, outputs: outputs.filter(Boolean) };
  return { status: "pass", results, executables, outputs: outputs.filter(Boolean) };
}

/** The largest inline `run:` script a workflow may carry while the pinned actionlint is 1.7.12 (rhysd/actionlint#702). */
export const RUN_SCRIPT_LIMIT_BYTES = 3800;
/** Every block `run:` script (`run: |` or `run: >`) of a workflow text larger than `limit` bytes, as "<file>:<line> <bytes> bytes".
 * Line-based and conservative: the block is every following line indented deeper than the `run:` key (blank lines included),
 * with that common indentation removed, as YAML reads a literal block. */
export function oversizedRunScripts(text, file, limit = RUN_SCRIPT_LIMIT_BYTES) {
  const lines = String(text).split(/\r?\n/);
  const out = [];
  for (let i = 0; i < lines.length; i += 1) {
    const m = /^(\s*)(?:-\s+)?run:\s*[|>][-+0-9]*\s*$/.exec(lines[i]);
    if (!m) continue;
    const keyIndent = lines[i].indexOf("run:");
    const body = [];
    let j = i + 1;
    for (; j < lines.length; j += 1) {
      const line = lines[j];
      if (line.trim() === "") { body.push(""); continue; }
      if (line.length - line.trimStart().length <= keyIndent) break;
      body.push(line);
    }
    while (body.length && body[body.length - 1] === "") body.pop();
    const indent = Math.min(...body.filter(Boolean).map((l) => l.length - l.trimStart().length));
    const script = body.map((l) => l.slice(Number.isFinite(indent) ? indent : 0)).join("\n") + "\n";
    const bytes = Buffer.byteLength(script);
    if (bytes > limit) out.push(`${file}:${i + 1} ${bytes} bytes`);
    i = j - 1;
  }
  return out;
}

/** The time limit of one scan: its own `timeoutMs`, then the run's `scanTimeoutMs`, else 5 min for actionlint, 30 min for class A and
 * 60 min for class B (a cold compile of the largest crate). */
/** The broker's quiet lease ceiling (rustright-state quiet.rs QUIET_LEASE_MAX_TTL_MS) and the quiet suite timeout under it. */
export const QUIET_LEASE_MAX_MS = 30 * 60_000;
export const QUIET_SCAN_MAX_MS = 26 * 60_000;
/** A tool version probe's bound; with QUIET_SCAN_MAX_MS it keeps a whole leased quiet suite (probes + run) under the 30 min lease. */
export const PROBE_TIMEOUT_MS = 60_000;
export function scanTimeoutMs(scan, options = {}) {
  if (Number.isFinite(scan?.timeoutMs) && scan.timeoutMs > 0) return scan.timeoutMs;
  if (Number.isFinite(options.scanTimeoutMs) && options.scanTimeoutMs > 0) return options.scanTimeoutMs;
  if (scan?.tool === "actionlint") return 5 * 60_000;
  return scan?.class === SCAN_CLASS.B ? 60 * 60_000 : 30 * 60_000;
}

/**
 * A scan the broker carries (rightkit#1367 step 2): on a broker host (`brokerHost`; a snapshot carrier implies one) a cargo or rustright scan, a carried Swift package build
 * (only with the snapshot carrier: a Swift scan with no carrier is a local process), and an npm or node suite that declares a broker call (`brokerCall`: its compiles are
 * carried requests). The broker queues and orders these; the gate neither counts them nor puts a clock on their queue time. This is the ONE test: the scheduler, the
 * run clock, the snapshot carrier (`viaSnapshotBroker`) and the carried Swift route (`swiftBrokered`) all derive from it.
 */
export function isBrokerCarriedScan(scan, { brokerHost = false, snapshotBroker = null } = {}) {
  if (brokerHost !== true && !snapshotBroker) return false;
  if (scan?.tool === "rustright" || scan?.tool === "cargo") return true;
  if (scan?.tool === "swift") return Boolean(snapshotBroker);
  return Boolean(scan?.brokerCall);
}

/** Class B processes the broker never sees (npm or node test suites run on this host) are bounded here, one slot per suite; the broker bounds everything it carries (rightkit#1367 step 2). */
export const LOCAL_CLASS_B_JOBS = 3;

/**
 * Whether a class B scan is the BROKER's to queue for scheduling: a cargo or rustright scan and a carried Swift build (isBrokerCarriedScan). An npm or node suite that merely makes broker
 * calls (`brokerCall`) is still a process of this host, so it holds one of the local slots; only its broker calls are carried and watched.
 */
export function isBrokerScheduledScan(scan, ctx) {
  return isBrokerCarriedScan(scan, ctx) && (scan?.tool === "rustright" || scan?.tool === "cargo" || scan?.tool === "swift");
}

/** The directory a class B scan works in: its `cwd`, else the package an `--dir <package>` argument names (an npm suite); null when it names none. */
function scanDirectory(scan) {
  if (typeof scan?.cwd === "string") return scan.cwd;
  const at = Array.isArray(scan?.args) ? scan.args.indexOf("--dir") : -1;
  return at >= 0 && typeof scan.args[at + 1] === "string" ? scan.args[at + 1] : null;
}
/**
 * The chain a class B scan belongs to on a broker host. Scans of one component's directory share its output directory and run in plan order. A carried scan chains by its
 * directory (the repository root, `.`, when it names none). A scan the broker never sees chains by ITS OWN package directory (else its id), never the shared `.` chain, so the
 * local limit counts real suites and a root build never waits behind them.
 */
export function classBChainKey(scan, carried) {
  const dir = scanDirectory(scan);
  if (carried && typeof scan?.executionWorkspace === "string") return scan.executionWorkspace;
  return carried ? (dir ?? ".") : `local:${dir ?? scan?.id ?? "."}`;
}

/**
 * Class B on a broker host (rightkit#1367 step 2): one chain per component, its scans in plan order (compile, doc, tests: they share the component's output directory),
 * every chain started at once. A broker-carried scan starts without a count; the broker queues and orders it. A scan the broker never sees (an npm or node suite) holds one
 * of `localJobs` local slots while it runs. `run(index)` runs one scan and never rejects.
 */
export async function scheduleBrokerHostClassB({ scans, lanes, carried, localJobs, run, pipeline = 1 }) {
  const chains = new Map();
  for (const lane of ["compile", "doc", "tests"]) {
    for (const i of lanes[lane] ?? []) { const key = classBChainKey(scans[i], carried(scans[i])); if (!chains.has(key)) chains.set(key, []); chains.get(key).push(i); }
  }
  const limit = Math.max(1, Number(localJobs) || 1);
  let held = 0; const waiters = [];
  const take = async () => { while (held >= limit) await new Promise((resolve) => waiters.push(resolve)); held += 1; };
  const free = () => { held -= 1; waiters.shift()?.(); };
  // B4 (rightkit#1458 audit): with `pipeline` 1 a chain runs one scan at a time, so the gate's work after one job (receipt read, signed-evidence
  // verification, ledger check) and before the next (cache key, request file) was dead time at the broker (about 10.6 s a job on the Dell). With
  // `pipeline` n, up to n carried scans of one chain are in flight: the next is submitted while the previous is still running or being verified, and the
  // broker (which owns ordering and holds one build per repository) queues it. Starts stay in plan order; a scan the broker never sees keeps its
  // local slot and runs alone, after the carried scans started before it have finished.
  const depth = Math.max(1, Number(pipeline) || 1);
  await Promise.all([...chains.values()].map(async (chain) => {
    const flight = new Set();
    for (const i of chain) {
      const local = !carried(scans[i]);
      if (local) {
        await Promise.all(flight);
        await take();
        try { await run(i); } finally { free(); }
        continue;
      }
      const scanDepth = scans[i].executionWorkspace ? 1 : depth;
      while (flight.size >= scanDepth) await Promise.race(flight);
      const job = run(i).finally(() => flight.delete(job));
      flight.add(job);
    }
    await Promise.all(flight);
  }));
}

export function laneOf(scan) {
  if (scan.publisherPhase === "compile") return "compile";
  if (/^(clippy|check|check-features|check-dependent):/.test(scan.id)) return "compile";
  if (/^doc:/.test(scan.id)) return "doc";
  return scan.quiet === true ? "quiet" : "tests";
}

// The lines a reader needs from a failed scan: every line that names a failure (a failed test, a panic, a compiler or tool error,
// cargo test's `failures:` block header, pnpm's ERR_ codes), at most twelve of them, then the last four lines of stdout and of
// stderr. cargo test writes results to stdout and "Running ..." to stderr, so a plain tail showed only harness chatter.
const FAILURE_LINE = /(^|[\s\[])(FAILED|panicked at|error(\[E\d+\])?:|error\[|^failures:|ERR_PNPM_\w+|Error:|assertion `?left|^thread '.*' panicked)|^\[ERROR\]|^\w+Error:|error \(E[A-Z]+\)/;
export function failureDetail(r) {
  const lines = (text) => String(text ?? "").split(/\r?\n/).map((l) => l.trimEnd()).filter((l) => l.trim());
  const out = lines(r?.stdout); const err = lines(r?.stderr);
  // Compiler errors lead: a warning's quoted tool output (rustc's "warning: linker stderr: cc: error: ...") also matches, and listed first it hid
  // the real errors (Mac 2026-11-07: four rows read as a broker sandbox failure; the errors were unresolved imports and a cmake archiver).
  const hits = [...out, ...err].filter((l) => FAILURE_LINE.test(l));
  const quoted = (l) => /^\s*(warning:|\|)/.test(l) || /warning: linker std(err|out)/.test(l);
  const named = [...hits.filter((l) => !quoted(l)), ...hits.filter(quoted)].slice(0, 12);
  const ends = [...out.slice(-4), ...err.slice(-4)];
  // `cargo audit` / `cargo deny` without the cargo-audit / cargo-deny binary: cargo's own "no such command" text told a reader to
  // search crates.io; the scan's tool is required and missing, which is what actionlint's row already says (E2E L9-02).
  const missing = [...out, ...err].map((l) => l.match(/^error: no such command: `(audit|deny)`/)?.[1]).find(Boolean);
  const lead = missing ? [`required tool cargo-${missing} is missing or not runnable (cargo has no \`${missing}\` subcommand); a required scan is never skipped`] : [];
  return [...new Set([...lead, ...auditVulnerabilities(r?.stdout), ...named, ...ends])].join(" | ");
}
// cargo-audit prints one `Crate:/Version:/.../ID:` block per advisory: a vulnerability has no `Warning:` line, an unmaintained or unsound
// crate has one and does not fail the scan. The failing blocks lead the detail (E2E L9-01); a tail of the output alone named a warning.
function auditVulnerabilities(text) {
  const out = [];
  for (const block of String(text ?? "").split(/\r?\n[ \t]*\r?\n/)) {
    const m = block.match(/^Crate:[ \t]+(\S+)[ \t]*\r?\nVersion:[ \t]+(\S+)/m);
    if (!m || /^Warning:/m.test(block)) continue;
    const id = block.match(/^ID:[ \t]+(\S+)/m)?.[1];
    out.push(`vulnerable: ${m[1]} ${m[2]}${id ? ` ${id}` : ""}`);
  }
  return out.slice(0, 6);
}
const tail = failureDetail;

/** rustfmt: $RUSTFMT, else the newest installed rustup toolchain, else PATH (null when none). */
export function findRustfmt({ env = process.env, exists = fs.existsSync, readdir = fs.readdirSync, platform = process.platform, home = os.homedir(), spawn = defaultSpawn } = {}) {
  if (env.RUSTFMT && exists(env.RUSTFMT)) return env.RUSTFMT;
  const exe = platform === "win32" ? "rustfmt.exe" : "rustfmt";
  const toolchains = path.join(env.RUSTUP_HOME || path.join(home, ".rustup"), "toolchains");
  if (exists(toolchains)) {
    const found = readdir(toolchains)
      .map((d) => ({ d, v: (d.match(/^(\d+)\.(\d+)(?:\.(\d+))?-/) || []).slice(1).map((n) => Number(n || 0)) }))
      .filter((t) => t.v.length && exists(path.join(toolchains, t.d, "bin", exe)))
      .sort((a, b) => b.v[0] - a.v[0] || b.v[1] - a.v[1] || b.v[2] - a.v[2]);
    if (found.length) return path.join(toolchains, found[0].d, "bin", exe);
  }
  const which = spawn(platform === "win32" ? "where" : "which", [exe]);
  // A Promise has no status: an async spawn here would report a present rustfmt as missing.
  if (typeof which?.then === "function") throw new TypeError("findRustfmt needs a synchronous spawn");
  return which && which.status === 0 ? String(which.stdout).split(/\r?\n/)[0].trim() : null;
}

async function defaultLoadToml() {
  const mod = await import("smol-toml");
  return mod.parse;
}
// Parsers come from this package's own trusted location (the runner's copy), never from the content root,
// which is submitted code. `smol-toml` resolves through this package's own dependencies.
const TRUSTED_YAML_PARSER = fileURLToPath(new URL("./workflow-yaml.mjs", import.meta.url));
async function defaultLoadYaml() {
  if (!fs.existsSync(TRUSTED_YAML_PARSER)) throw new Error(`strict YAML parser not found at ${TRUSTED_YAML_PARSER}`);
  return (await import(pathToFileURL(TRUSTED_YAML_PARSER).href)).parseYaml;
}
async function defaultScanSecrets() {
  return (await import("./publish-cargo.mjs")).scanCrateForSecrets;
}

/**
 * For a content root that is a git worktree's toplevel: a predicate over absolute paths that is true for every git-ignored file or
 * directory there (`git ls-files --others --ignored --exclude-standard --directory`, which lists an ignored directory once, with a
 * trailing slash). Null when the root is not a git toplevel (a committed export) or git cannot answer: then nothing is skipped.
 */
export async function ignoredPathSkip(ctx) {
  const git = async (args) => { try { return await ctx.deps.spawn("git", args, { cwd: ctx.contentRoot, encoding: "utf8", env: ctx.deps.env }); } catch { return null; } };
  const top = await git(["rev-parse", "--show-toplevel"]);
  const same = (a, b) => { try { return fs.realpathSync(a) === fs.realpathSync(b); } catch { return path.resolve(a) === path.resolve(b); } };
  if (!top || top.status !== 0 || !same(String(top.stdout ?? "").trim(), ctx.contentRoot)) {
    // Not a git toplevel: a snapshot, which holds the commit's files and also whatever a stage wrote into it (dist/stage, build output), and
    // `git ls-files --ignored` cannot tell them apart there. Class A judges the commit's tracked files only (rightkit#1393): with the
    // gate's list of them, everything else, and every directory with none of them, is skipped. Without the list nothing is skipped.
    if (!Array.isArray(ctx.trackedFiles)) return null;
    const trackedSet = new Set(ctx.trackedFiles);
    const parents = new Set();
    for (const f of trackedSet) for (let i = f.indexOf("/"); i !== -1; i = f.indexOf("/", i + 1)) parents.add(f.slice(0, i));
    const base = ctx.contentRoot;
    return (absolute) => {
      const rel = path.relative(base, absolute).replaceAll("\\", "/");
      if (!rel || rel.startsWith("..")) return false;
      return !trackedSet.has(rel) && !parents.has(rel);
    };
  }
  const listed = await git(["ls-files", "-z", "--others", "--ignored", "--exclude-standard", "--directory"]);
  if (!listed || listed.status !== 0) return null;
  const entries = String(listed.stdout ?? "").split("\0").filter(Boolean);
  const dirs = entries.filter((e) => e.endsWith("/"));
  const files = new Set(entries.filter((e) => !e.endsWith("/")));
  const root = ctx.contentRoot;
  return (absolute) => {
    const rel = path.relative(root, absolute).replaceAll("\\", "/");
    if (!rel || rel.startsWith("..")) return false;
    return files.has(rel) || dirs.some((d) => rel === d.slice(0, -1) || rel.startsWith(d));
  };
}

// What rustfmt needs from a Cargo.toml, read by table so an `edition` under [package.metadata.x] or [dependencies.x] is not the
// package's: `pkg` is the [package] edition (a literal year, or "workspace" for `edition.workspace = true` / `edition = { workspace = true }`),
// `ws` the [workspace.package] edition literal, `isWorkspace` whether the manifest declares a [workspace]. An `edition` entry in one of
// those two tables that is none of the recognised spellings (`edition = 2021`, `edition.workspace = false`) is kept as `pkgBad` / `wsBad`
// (the line), never skipped, so it cannot fall back to a default. Keys before any table header count as [package] keys (the fixtures of
// this package's tests write manifests that way; cargo itself rejects them).
export function cargoEditions(text) {
  const out = { pkg: undefined, ws: undefined, pkgBad: undefined, wsBad: undefined, isWorkspace: false };
  let table = "";
  for (const raw of String(text).split(/\r?\n/)) {
    const header = /^\s*\[\[?([^\]]+)\]\]?\s*(?:#.*)?$/.exec(raw);
    if (header) {
      table = header[1].trim();
      if (table === "workspace" || table.startsWith("workspace.")) out.isWorkspace = true;
      continue;
    }
    if (table !== "" && table !== "package" && table !== "workspace.package") continue;
    const line = raw.replace(/^(\s*)(["'])edition\2/, "$1edition");
    if (!/^\s*edition\s*(?:[.=]|$)/.test(line)) continue;
    let value;
    if (/^\s*edition\.workspace\s*=\s*true\s*(?:#.*)?$/.test(line) || /^\s*edition\s*=\s*\{\s*workspace\s*=\s*true\s*\}\s*(?:#.*)?$/.test(line)) value = "workspace";
    else value = (/^\s*edition\s*=\s*(?:"(\d{4})"|'(\d{4})')\s*(?:#.*)?$/.exec(line) || []).slice(1).find(Boolean);
    if (table === "workspace.package") {
      if (value !== undefined && value !== "workspace") out.ws = value; else out.wsBad = raw.trim();
    } else if (value !== undefined) out.pkg = value;
    else out.pkgBad = raw.trim();
  }
  return out;
}

/**
 * The edition rustfmt must run a Rust file under: `{ edition }`, or `{ manifest, problem }` when it cannot be resolved.
 * The nearest Cargo.toml at or above the file decides. A literal `edition = "NNNN"` is that edition; `edition.workspace = true`
 * (or `edition = { workspace = true }`) is the `[workspace.package] edition` of the owning workspace root, the nearest manifest at or
 * above it that declares `[workspace]`, which is the manifest itself for a root package. An inheritance that cannot be followed
 * (no workspace root, an unreadable manifest, or a root with no literal `[workspace.package] edition`) is a `problem`, never a default.
 * A manifest that names no edition at all, and a file below no manifest, keep rustfmt's "2021" as before.
 */
function rustEditionResolver({ contentRoot, exists, readFile }) {
  const root = path.normalize(contentRoot);
  const inside = (dir) => { const rel = path.relative(root, dir); return rel === "" || !(rel === ".." || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel)); };
  const rel = (dir) => posix(path.relative(root, path.join(dir, "Cargo.toml")));
  const parsed = new Map();
  const manifestAt = (dir) => {
    if (!parsed.has(dir)) {
      let value;
      try { value = cargoEditions(readFile(path.join(dir, "Cargo.toml"))); } catch (e) { value = { unreadable: String(e?.message ?? e).split("\n")[0] }; }
      parsed.set(dir, value);
    }
    return parsed.get(dir);
  };
  const ancestors = function* (start) {
    for (let dir = start; inside(dir); dir = path.dirname(dir)) {
      yield dir;
      if (path.dirname(dir) === dir) return;
    }
  };
  return (file) => {
    const dir = [...ancestors(path.dirname(path.join(root, file)))].find((d) => exists(path.join(d, "Cargo.toml")));
    if (dir === undefined) return { edition: "2021" };
    const manifest = rel(dir);
    const info = manifestAt(dir);
    if (info.unreadable) return { manifest, problem: `${manifest} cannot be read (${info.unreadable}), so its edition is unknown` };
    if (info.pkgBad !== undefined) return { manifest, problem: `${manifest} has an edition entry the scan cannot read (${info.pkgBad}); write edition = "NNNN" or edition.workspace = true` };
    if (info.pkg === undefined) return { edition: info.ws ?? "2021" };
    if (info.pkg !== "workspace") return { edition: info.pkg };
    const inherits = `${manifest} sets edition.workspace = true`;
    for (const up of ancestors(dir)) {
      if (!exists(path.join(up, "Cargo.toml"))) continue;
      const w = manifestAt(up);
      if (w.unreadable) return { manifest, problem: `${inherits}, but ${rel(up)} cannot be read (${w.unreadable}), so its edition is unknown` };
      if (!w.isWorkspace) continue;
      if (w.wsBad !== undefined) return { manifest, problem: `${inherits}, but its workspace root ${rel(up)} has a [workspace.package] edition entry the scan cannot read (${w.wsBad})` };
      if (w.ws === undefined) return { manifest, problem: `${inherits}, but its workspace root ${rel(up)} declares no literal [workspace.package] edition = "NNNN" to inherit` };
      return { edition: w.ws };
    }
    return { manifest, problem: `${inherits}, but no Cargo.toml at or above it declares a [workspace], so there is no [workspace.package] edition to inherit` };
  };
}

// A crate that can never be published (`publish = false` or `publish = []` in its [package] table) needs no license key, like a
// package.json with `"private": true`; cargo-manifest-text.mjs holds the one reading of that rule, which the secret scan shares.
function cargoUnpublished(text) { return cargoManifestState(text).unpublished; }

const INTERNAL = {
  "json-parse": (s, ctx) => {
    const bad = [];
    for (const f of s.files) {
      // tsconfig/jsconfig and editor settings are JSON with comments by their own spec (TypeScript reads them that way).
      const jsonc = /(^|\/)(tsconfig|jsconfig)(\.[^/]*)?\.json$|(^|\/)\.vscode\/[^/]+\.json$/.test(f);
      try { const text = ctx.read(f).replace(/^\uFEFF/, ""); JSON.parse(jsonc ? stripJsonComments(text) : text); } catch (e) { bad.push(`${f}: ${e.message}`); }
    }
    return bad;
  },
  "toml-parse": async (s, ctx) => {
    let parse;
    try { parse = await ctx.deps.loadToml(); } catch (e) { return { missing: `TOML parser unavailable: ${e.message}` }; }
    const bad = [];
    for (const f of s.files) { try { parse(ctx.read(f)); } catch (e) { bad.push(`${f}: ${String(e.message).split("\n")[0]}`); } }
    return bad;
  },
  // The GitHub advisory database (ghsa-audit.mjs, rightkit#1461): a GHSA advisory RustSec lacks fails like a RustSec vulnerability; no network and no
  // same-day answer fails (ADVISORY_DB) exactly as cargo-audit does without its database.
  "ghsa-audit": async (s, ctx) => {
    let lockText;
    try { lockText = ctx.read(s.lockFile); } catch (e) { return { code: "ADVISORY_DB", detail: `${s.lockFile} is unreadable (${e.message}); the GitHub advisory scan needs the committed lockfile` }; }
    try { return (await ghsaFindings({ lockText, fetchFn: ctx.deps.fetch ?? globalThis.fetch, cacheDir: ctx.ghsaCacheDir ?? null })).findings; }
    catch (e) { return { code: "ADVISORY_DB", detail: `the GitHub advisory database (OSV) could not be asked: ${e.message}; class A refuses to pass a lockfile it could not check against it, as it refuses cargo-audit without its database (connect once; the answer is then kept for the UTC day)` }; }
  },
  // Dependency freshness (rightkit#1475): advice, never a failure. `right-update check --json` over the committed repository; the row carries "N behind latest".
  "dependency-freshness": async (s, ctx) => {
    let bin;
    try { bin = resolveOutdatedBin(); } catch (e) { return { advice: `dependency freshness not measured: ${e instanceof OutdatedUnavailableError ? e.message : `right-update could not be located (${e.message})`}` }; }
    // The answer is a property of the repository's lock files and of the registry at this hour, not of the unit being gated: a run per unit of one
    // commit asks the registry again for the same locks (88 s per crate on 2026-11-04, 6795 s over 77 crate runs). One answer per lock set and UTC hour.
    let cachePath = null;
    try {
      const names = [".rightkit/update.json", "Cargo.lock", "pnpm-lock.yaml", "package.json"];
      const h = crypto.createHash("sha256");
      for (const n of names) { h.update(`${n}\0`); try { h.update(fs.readFileSync(path.join(ctx.contentRoot, n))); } catch { h.update("absent"); } }
      const dir = path.join(ctx.tmpRoot ?? os.tmpdir(), "rightkit-freshness-cache");
      cachePath = path.join(dir, `${h.digest("hex").slice(0, 32)}-${new Date().toISOString().slice(0, 13)}.json`);
      const hit = JSON.parse(fs.readFileSync(cachePath, "utf8"));
      if (typeof hit?.advice === "string") return { advice: hit.advice };
    } catch { /* no cached answer: measure */ }
    const remember = (result) => {
      // A measurement that timed out is cached too: it is the 120 s case (41 of 81 crate runs on 2026-11-04), and asking again for every unit of the same
      // lock set within the hour is the cost this cache removes. The advice text says "not measured", so nobody reads it as a pass.
      if (cachePath && typeof result?.advice === "string") {
        try { fs.mkdirSync(path.dirname(cachePath), { recursive: true }); fs.writeFileSync(cachePath, JSON.stringify({ advice: result.advice })); } catch { /* the cache is an optimisation only */ }
      }
      return result;
    };
    let r;
    try { r = await ctx.deps.spawn(process.execPath, [bin, "check", "--json", "--repo", ctx.contentRoot], { cwd: ctx.contentRoot, encoding: "utf8", env: { ...ctx.deps.env, ...PNPM_NO_HOOKS_ENV }, timeout: 120_000, maxBuffer: 1 << 26 }); }
    catch (e) { return remember({ advice: `dependency freshness not measured: right-update check did not run (${e.message})` }); }
    let report = null;
    try { report = JSON.parse(String(r?.stdout ?? "")); } catch { report = null; }
    if (!report || !Array.isArray(report.entries)) return remember({ advice: `dependency freshness not measured: right-update check printed no report${r?.status ? ` (exit ${r.status})` : ""}` });
    const behind = report.entries.filter((e) => e.status === "outdated");
    const unjudged = report.entries.filter((e) => e.status === "unknown" || e.error).length;
    const named = behind.slice(0, 5).map((e) => `${e.name} ${e.pinned ?? "?"} -> ${e.latest ?? "?"}`).join(", ");
    return remember({ advice: `${behind.length} behind latest${named ? ` (${named}${behind.length > 5 ? ", ..." : ""})` : ""}${unjudged ? `; ${unjudged} not judged` : ""}` });
  },
  // The Swift type-check (rightkit#1289): Mac host only, Class A, before any compile stage. SwiftPM builds the package (`swift build`) into the scratch root:
  // every source compiles against the real SDK and the target's dependencies, so a labelled-argument order error fails here in seconds instead of at the
  // end of the Xcode compile (Pulse, 2026-10-29, exit 65 after the Rust stages). Not `-Xswiftc -typecheck`: with Swift 6.4 it writes no object and SwiftPM still links an executable target, so every package with one failed with "unexpected input file" (rightkit#1514 acceptance run). The committed Package.resolved decides the dependencies (`--disable-automatic-resolution`);
  // SwiftPM's state lives under the gate's scratch root, never in the snapshot. A pass is exit 0. A failure with Swift diagnostics lists them; a failure with none is the
  // build system's (it did not complete) and still fails: never a pass that checked nothing.
  "swift-typecheck": async (s, ctx) => {
    let scratch;
    try { scratch = swiftScratchPath({ tmpRoot: ctx.tmpRoot, roots: [ctx.contentRoot, ctx.liveRoot, ctx.cargoLiveRoot], id: s.id }); }
    catch (e) { return { code: "SWIFT_SCRATCH", detail: `${s.id}: ${e.message}` }; }
    fs.mkdirSync(scratch, { recursive: true });
    const packageDir = path.join(ctx.contentRoot, s.dir === "." ? "" : s.dir);
    let r;
    try { r = await ctx.deps.spawn("swift", ["build", "--disable-automatic-resolution", "--package-path", packageDir, "--scratch-path", scratch], { cwd: packageDir, encoding: "utf8", env: { ...ctx.deps.env, ...PNPM_NO_HOOKS_ENV }, timeout: 10 * 60_000, maxBuffer: 1 << 26 }); }
    catch (e) { return { code: "TOOL_MISSING", detail: `${s.id}: swift could not be started (${e.message}); the Swift type-check is required on the Mac host and is never skipped` }; }
    if (r?.error) return { code: "TOOL_MISSING", detail: `${s.id}: swift could not be started (${r.error.message}); the Swift type-check is required on the Mac host and is never skipped` };
    if (r?.status === 0) return [];
    // Swift 6.4 colours its diagnostics even into a pipe; strip the escapes before reading them.
    const output = `${r?.stdout ?? ""}\n${r?.stderr ?? ""}`.replace(/\x1b\[[0-9;]*m/g, "");
    const diagnostics = [...new Set(output.split(/\r?\n/).filter((l) => /\.swift:\d+:\d+: error:/.test(l)).map((l) => l.trim().replace(/^.*?(?=[^/\\\s]+\.swift:\d+:\d+: error:)/, "")))];
    if (diagnostics.length) return diagnostics.slice(0, 8).map((d) => `${s.dir === "." ? "" : `${s.dir}: `}${d}`);
    return [`${s.dir === "." ? "swift package" : s.dir}: the Swift type-check did not complete (exit ${r?.status ?? "none"}) and printed no Swift diagnostic: ${failureDetail(r)}`];
  },
  // Every Cargo feature of a planned crate is enabled by one of its planned feature sets (feature-coverage.mjs, rightkit#1231).
  "feature-coverage": (s, ctx) => s.crates.flatMap((c) => featureCoverageProblems({ name: c.name, manifestText: ctx.read(c.manifest), sets: c.sets })),
  // File-only SwiftPM lockfile check (swift-lock.mjs): runs no SwiftPM, reads Package.swift / project.pbxproj and Package.resolved as committed.
  "swift-lock": (s, ctx) => {
    const exists = (f) => ctx.deps.exists(path.join(ctx.contentRoot, f));
    return s.lock.kind === "xcode" ? xcodeProjectLockProblems({ project: s.lock.dir, read: ctx.read, exists }) : packageLockProblems({ dir: s.lock.dir, read: ctx.read, exists });
  },
  // File-only Gradle dependency-locking check (gradle-lock.mjs): no Gradle is run.
  "gradle-lock": (s, ctx) => gradleLockProblems({
    dir: s.dir, read: ctx.read, exists: (f) => ctx.deps.exists(path.join(ctx.contentRoot, f)),
    list: (d) => ctx.deps.readdir(path.join(ctx.contentRoot, d), { withFileTypes: true }).map((e) => ({ name: e.name, isDir: e.isDirectory() })),
  }),
  // File-only Python lock check (python-lock.mjs): no pip, uv or poetry is run.
  "python-lock": async (s, ctx) => {
    let parseToml;
    try { parseToml = await ctx.deps.loadToml(); } catch (e) { return { missing: `TOML parser unavailable: ${e.message}` }; }
    return pythonLockProblems({ dir: s.dir, names: ctx.deps.readdir(path.join(ctx.contentRoot, s.dir)).map(String), read: ctx.read, parseToml });
  },
  "yaml-parse": async (s, ctx) => {
    let parse;
    try { parse = await ctx.deps.loadYaml(); } catch (e) { return { missing: `YAML parser unavailable: ${e.message}` }; }
    const bad = [];
    for (const f of s.files) { try { parse(ctx.read(f)); } catch (e) { bad.push(`${f}: ${e.message}`); } }
    return bad;
  },
  refuse: (s) => [s.detail],
  // Every RustSec exception is dated, scoped to the locked versions and identical in deny.toml and
  // .cargo/audit.toml (ci/advisory-exceptions.json; Astra 2026-10-22). Expiry fails every run.
  "advisory-exceptions": (s, ctx) => (s.dir === undefined
    ? checkAdvisoryExceptions({ root: ctx.contentRoot, readFile: ctx.read })
    : checkAdvisoryExceptions({ root: path.join(ctx.contentRoot, s.dir), readFile: (p) => ctx.read(path.join(s.dir, p)), exceptionsFile: s.exceptionsFile }).map((problem) => `${s.dir || "."}: ${problem}`)),
  // Every Right Suite API projection must equal what the generator writes from the contract. The pure check is imported from the
  // scanner's own checkout (scripts/ sits beside packages/release), never from the content root.
  "suite-api": async (s, ctx) => {
    const { suiteApiProblems } = await import("../../scripts/gen-suite-api.mjs");
    return suiteApiProblems((f) => ctx.read(f));
  },
  "ownership-catalog": (s, ctx) => (ctx.read(s.files[0]) === ctx.read(s.files[1]) ? [] : ["copy packages/release/rightkit-ownership.json to crates/rightkit-ownership/catalog/ownership-catalog.json"]),
  "changelog-version": (s, ctx) => {
    const version = (ctx.read(s.files[0]).match(/^version\s*=\s*"([^"]+)"/m) || [])[1];
    if (!version) return [];
    const heading = new RegExp(`^##\\s+\\[?${version.replace(/\./g, "\\.")}\\]?(\\s|$)`, "m");
    return heading.test(ctx.read(s.files[1])) ? [] : [`${s.files[1]} has no "## ${version}" heading`];
  },
  license: (s, ctx) => {
    const bad = [];
    for (const f of s.files) {
      const text = ctx.read(f);
      if (f.endsWith("Cargo.toml")) {
        if (!/^\s*(license|license-file)\s*(=|\.workspace)/m.test(text) && !cargoUnpublished(text)) bad.push(`${f}: no license or license-file`);
      } else {
        try { const m = JSON.parse(text); if (m.private !== true && !(typeof m.license === "string" && m.license.trim()) && !m.licenses) bad.push(`${f}: no license field`); } catch (e) { bad.push(`${f}: ${e.message}`); }
      }
    }
    return bad;
  },
  // The repository gate reads env templates and test vectors against the reviewed records of the gated commit
  // (.rightkit/secret-scan-records.json, read from the content root by the scanner); crate packaging stays strict.
  // Each violation names its repository-relative path, so a unit that is both a crate and a package reports a file once.
  "secret-scan": async (s, ctx) => {
    const scan = await ctx.deps.scanSecrets();
    // A committed export holds only tracked files. A live tree (--to dev|test: the content root is the checkout itself) also holds
    // git-ignored build output (.right-release/, DerivedData, a vendored checkout under it), which the gate never judges: the scanner
    // skips what `git ls-files --ignored` names there. Outside a git toplevel nothing is skipped (Pulse, 2026-10-27).
    const skip = await ignoredPathSkip(ctx);
    const bad = new Set();
    for (const dir of s.dirs) {
      const abs = path.join(ctx.contentRoot, dir);
      if (!ctx.deps.exists(abs)) { bad.add(`${dir}: directory not found`); continue; }
      for (const v of scan(abs, { envTemplates: true, repoRoot: ctx.contentRoot, skip })) bad.add(v);
    }
    return [...bad];
  },
};

/**
 * SwiftPM's build directory for one Swift package scan: `<tmpRoot>/rightkit-swift-scratch/<sha256(repository root)[:16]>/<scan id>`, outside every root
 * the scan reads (the commit snapshot, the checkout), the same path every run (so the build stays warm). Throws when it would land inside one of them.
 */
/**
 * GB-52: a carried scan's client gets an operation deadline BEFORE the scan's own limit, so the client cancels the job by id and watches its terminal state (within a 15 s cancel wait)
 * instead of being killed with the job still running; a user-set RUSTRIGHT_OPERATION_DEADLINE_MS or cancel wait is never replaced.
 */
/** A span for a message: whole seconds under a minute, minutes from then on. */
export const spanText = (ms) => (ms < 60_000 ? `${Math.max(0, Math.round(ms / 1000))} s` : `${Math.round(ms / 60_000)} min`);
export const scanOperationDeadlineMs = (limitMs) => Math.max(Math.floor(limitMs / 2), limitMs - 20_000);
/**
 * What a carried Swift scan gives carryBrokerAdapterAsync: an UNWATCHED one passes its own limit as the client's operation deadline (the env, and `deadlineMs`, which also bounds the
 * retry wait); a watched one passes neither (the stopwatch and the ceiling bound it, and the client's deadline is absolute from its start).
 */
export function carriedCallBudget({ env, limitMs, watched }) {
  return watched ? { env: {} } : { env: scanOperationEnv(env, limitMs), deadlineMs: scanOperationDeadlineMs(limitMs) };
}
export function scanOperationEnv(env, limitMs) {
  const deadline = operationDeadlineEnv({ env, deadlineMs: scanOperationDeadlineMs(limitMs) });
  if (Object.keys(deadline).length === 0) return {};
  return { ...deadline, ...(Object.keys(env ?? {}).some((k) => k.toUpperCase() === CANCEL_WAIT_ENV) ? {} : { [CANCEL_WAIT_ENV]: "15000" }) };
}

export function swiftScratchPath({ tmpRoot, roots, id }) {
  const real = (p) => { try { return fs.realpathSync.native(p); } catch { return path.resolve(p); } };
  const repo = real(roots.find(Boolean));
  const dir = path.join(real(tmpRoot), "rightkit-swift-scratch", crypto.createHash("sha256").update(repo).digest("hex").slice(0, 16), id.replace(/[^A-Za-z0-9._-]/g, "_"));
  for (const root of roots.filter(Boolean)) {
    const rel = path.relative(real(root), dir);
    if (rel === "" || (rel !== ".." && !rel.startsWith(`..${path.sep}`) && !path.isAbsolute(rel))) throw new Error(`the SwiftPM build directory ${dir} would be inside ${real(root)}; set RK_GATE_TMPDIR outside the repository`);
  }
  return dir;
}

/**
 * The output directory of ONE Class A Cargo scan that runs natively (no broker carries it): a fresh directory under `tmpRoot`, outside every
 * root the scan reads (the commit snapshot, the checkout). Cargo writes `target/.rustc_info.json` (and CACHEDIR.TAG) beside the manifest it
 * reads, so a scan run with no CARGO_TARGET_DIR leaves a `target/` in the snapshot, which must stay exactly the commit's tree (E6). The caller
 * removes the directory when the scan ends. Throws, removing it, when it would land inside one of `roots`.
 */
export function cargoReadTargetDir({ tmpRoot, roots }) {
  const real = (p) => { try { return fs.realpathSync.native(p); } catch { return path.resolve(p); } };
  const dir = real(fs.mkdtempSync(path.join(real(tmpRoot), "rk-cargo-read-")));
  for (const root of roots.filter(Boolean)) {
    const rel = path.relative(real(root), dir);
    if (rel === "" || (rel !== ".." && !rel.startsWith(`..${path.sep}`) && !path.isAbsolute(rel))) {
      fs.rmSync(dir, { recursive: true, force: true });
      throw new Error(`the Cargo output directory ${dir} would be inside ${real(root)}; set RK_GATE_TMPDIR outside the repository`);
    }
  }
  return dir;
}

/**
 * Run a plan. Options: classes (default ["A","B"]), exclude(scan) => boolean (an explicit, recorded
 * omission such as a caller that runs its own tests), plus the injectable spawn/exists/readFile/readdir/env.
 * Class B runs only when every selected class A scan passed; otherwise each B scan is "not-run".
 * `cargoReadTargetRoot` (default `tmpRoot`) is where a natively run Class A Cargo scan's own output directory is made (cargoReadTargetDir).
 */
export async function runScans(plan, options = {}) {
  const deps = {
    // Concurrent runs need a spawn that does not block the event loop.
    // Always asynchronous (A9): a synchronous spawn holds the event loop, so no scan's time limit can fire while it runs. The 2026-11-02 runs
    // show it: every scan that finished at the same moment reported the same inflated time (shellcheck and actionlint 1,655 s, against a
    // 5 min limit), held by a synchronous broker call. One lane used the synchronous spawn, so a single-lane run had no working limit.
    spawn: defaultSpawnAsync,
    // Locating a tool (rustfmt's `which`/`where` fallback) is synchronous whatever `spawn` is.
    lookupSpawn: defaultSpawn, exists: fs.existsSync, readFile: (f) => fs.readFileSync(f, "utf8"), readdir: fs.readdirSync,
    env: process.env, platform: process.platform, execPath: process.execPath,
    loadToml: defaultLoadToml, loadYaml: defaultLoadYaml, scanSecrets: defaultScanSecrets, findRustfmt,
    ...options,
  };
  const classes = options.classes ?? [SCAN_CLASS.A, SCAN_CLASS.B];
  const exclude = options.exclude ?? (() => false);
  // An AbortSignal: once aborted, no further scan starts, running children are killed (process tree) and every unfinished scan is "not-run" with code CANCELLED.
  const signal = options.signal; const sig = signal ? { signal } : {};
  const { contentRoot, liveRoot, cargoLiveRoot = null } = plan.roots;
  // Where the gate's own temporary files live (cargo-deny's metadata file and config overlay): a directory the gate owns, never inside the
  // checkout or the snapshot; the broker reads them by absolute path (options.tmpRoot, else the system temp directory).
  const tmpRoot = options.tmpRoot ?? os.tmpdir();
  const rootDir = (r) => (r === "content" ? contentRoot : r === "parent" ? path.dirname(contentRoot) : r === "cargoLive" ? (cargoLiveRoot ?? liveRoot) : liveRoot);
  const selected = plan.scans.filter((s) => classes.includes(s.class) && !exclude(s));
  const ordered = [...selected.filter((s) => s.class === SCAN_CLASS.A), ...selected.filter((s) => s.class === SCAN_CLASS.B)];
  // `trackedFiles` (the gate's list of the commit's own files): where the content root is not a git toplevel (a snapshot), the secret scan judges
  // exactly these and never what a stage wrote there (rightkit#1393).
  const ctx = { deps, contentRoot, liveRoot, cargoLiveRoot, tmpRoot, read: (f) => deps.readFile(path.join(contentRoot, f)), trackedFiles: Array.isArray(options.trackedFiles) ? options.trackedFiles : null };
  // GATE-CONTRACT GC-A11 (Adrian, 2026-11-02): Class A never runs in the broker. Class A cargo reads are never carried, in any stage; the gate
  // runs them directly (snapshot-metadata.mjs directReadSpawn), and cargo-deny / cargo-audit are its own enrolled tools.
  const cargoReadsAreCarried = false;
  // A broker host: the gate says so (options.brokerHost, from its executor) and a snapshot carrier implies it. One predicate (isBrokerCarriedScan) names the scans the broker carries.
  const brokerHost = options.brokerHost === true || Boolean(deps.snapshotBroker);
  const carriedCtx = { brokerHost, snapshotBroker: deps.snapshotBroker ?? null };
  // The broker's live state of a request (queued or running), through an ASYNCHRONOUS client call: the gate's own reader when it hands one, else the carrier's client.
  const brokerLive = options.brokerLive ?? (brokerHost && typeof deps.snapshotBroker?.client === "string" ? createBrokerLive({ client: deps.snapshotBroker.client, env: deps.env, spawn: defaultSpawnAsync, platform: deps.platform }) : null);
  // One reader for the whole run: every watched scan registers with it (first read after 1 s, then every 5 s, two reads in flight at most), instead of a client spawn per scan.
  const pollMs = options.brokerPollMs ?? ADMISSION_POLL_MS;
  const statusPoller = brokerLive ? createStatusPoller({ live: brokerLive, intervalMs: pollMs }) : null;
  const toolVersions = {}; const toolIdentities = {}; const advisoryDbs = new Map();
  const results = [];
  // Failed grouped invocations that were bisected for attribution (runGroup below): retained as run notes and on each
  // member row (`batchFailure`), so a combined failure stays visible even when every diagnostic split then passes.
  const batchFailureNotes = [];
  // The gate-owned cargo ledger of a wrapped snapshot compile (options.cargoLedger = { dir }): every call the wrapper decided, filed
  // under its scan's token. `ledgerSeen` is the run's one-use record of carriers and request ids across every scan.
  const cargoLedger = options.cargoLedger?.dir ? options.cargoLedger : null;
  const claimedTokens = new Set(); const ledgerSeen = { nonces: new Set(), ids: new Set() };
  // options.cache ={ read(key), write(key, row), digest(scan) => string | null } (null: not cacheable, which is how the live class A checks opt out).
  const probeScans = new WeakSet(); // scans looked up for runGroup's per-crate cache (answered like a lookup, never run)
  const cache = options.cache && typeof options.cache.read === "function" && typeof options.cache.write === "function" && typeof options.cache.digest === "function" ? options.cache : null;
  const cacheHits = [];
  // The GitHub advisory scan's same-UTC-day answers live beside the pinned RustSec checkout; a run with no cache (the door's live-scans refresh) asks again.
  if (cache) { try { ctx.ghsaCacheDir = path.join(deps.advisoryCacheRoot ?? advisoryCacheRoot({ env: deps.env, platform: deps.platform }), "ghsa-audit"); } catch { ctx.ghsaCacheDir = null; } }
  let classAFailed = false;
  // Class B needs class A first: in this call (classes includes A) or valid prior class A evidence for the
  // same class A scan set (options.priorClassA: an earlier runScans result whose class A rows all passed).
  const priorA = options.priorClassA;
  const priorRows = Array.isArray(priorA?.scans) ? priorA.scans.filter((r) => r.class === SCAN_CLASS.A) : [];
  // Prior evidence binds to the input content (inputDigest of the changed files), the class A scan set, and a
  // class A run that actually included class A.
  // ... and to the checker that produced it: evidence from another checker (other version, or same version with changed bytes)
  // does not unlock class B, and an unknown identity on either side refuses.
  const checker = CHECKER_IDENTITY;
  const priorClassAValid = Boolean(priorA) && priorA.schema === SCAN_RESULT_SCHEMA && Boolean(plan.classDigests?.A) && Boolean(plan.inputDigest)
    && priorA.inputDigest === plan.inputDigest && Array.isArray(priorA.classes) && priorA.classes.includes(SCAN_CLASS.A)
    && priorA.classDigests?.A === plan.classDigests.A && digestScanSet(priorRows) === plan.classDigests.A && priorRows.every((r) => r.status === "pass")
    && typeof checker === "string" && priorA.checker === checker;
  // Class A ran "in this call" only when it was selected, the plan has class A scans, and none was excluded:
  // excluding every class A scan (or a plan with none) never unlocks class B; explicit prior evidence is needed.
  const planA = plan.scans.filter((x) => x.class === SCAN_CLASS.A);
  const inCallA = classes.includes(SCAN_CLASS.A) && planA.length > 0 && selected.filter((x) => x.class === SCAN_CLASS.A).length === planA.length;
  // `classAEarlier`: the caller ran full class A of these same bytes once, before this call (the door's check stage before its compile pass, rightkit#1328).
  const classBBlocked = !inCallA && !priorClassAValid && options.classAEarlier !== true;

  const toolCommand = (tool) => (tool === "node" ? deps.execPath : tool === "rustfmt" ? deps.findRustfmt({ env: deps.env, exists: deps.exists, readdir: deps.readdir, platform: deps.platform, spawn: deps.lookupSpawn }) : tool);
  const probeFlight = singleFlightProbe(async (tool, scanDir = null) => {
    let outcome;
    // A trusted analyzer is resolved ONCE, from its enrolment (never PATH), and its bytes verified before this probe runs it.
    const trust = TRUSTED_SCAN_TOOLS.includes(tool) ? resolveTrustedScanTool(tool, { policy: options.toolPolicy, platform: deps.platform, ops: options.toolOps }) : null;
    const command = trust ? (trust.ok ? trust.identity.path : null) : toolCommand(tool);
    if (trust && !trust.ok) outcome = { ok: false, code: trust.code, why: trust.why };
    else if (!command) outcome = { ok: false, why: "not found" };
    else {
      // A version probe never runs inside the repository: a tool started there reads the repository's own configuration first
      // (pnpm's .npmrc and pnpmfile, corepack's packageManager pin, cargo's .cargo/config), before any scan's protections apply.
      // rustright is the one exception: the RustRight client refuses to run anywhere that is not a Cargo workspace inside a repository
      // (RK_NOT_PRIMARY_CHECKOUT, "workspace discovery failed"), so from a neutral directory every cargo scan read as TOOL_MISSING on
      // every broker host (rightkit#1215). Its probe runs where the scan itself will run; `cargo --version` reads no repository config.
      const cwd = (tool === "rustright" || (tool === "cargo" && cargoLiveRoot)) && scanDir ? scanDir : (deps.probeDir ?? probeDir());
      // A version probe is bounded (PROBE_TIMEOUT_MS): a hung tool reads as unavailable instead of holding a lane, or a quiet
      // lease, open.
      const probeMs = Number.isFinite(options.probeTimeoutMs) && options.probeTimeoutMs > 0 ? options.probeTimeoutMs : PROBE_TIMEOUT_MS;
      const signalFor = (ms) => (signal ? AbortSignal.any([signal, AbortSignal.timeout(ms)]) : AbortSignal.timeout(ms));
      // The gate's own version probes reach the wrapper too (cargo is first on PATH): they are filed under the probe token, which the
      // gate claims and checks like a scan's.
      const probeLedgerEnv = cargoLedger ? { [SCAN_TOKEN_ENV]: PROBE_LEDGER_TOKEN } : {};
      if (cargoLedger) claimedTokens.add(PROBE_LEDGER_TOKEN);
      const probeOnce = (ms) => deps.spawn(command, PROBES[tool] ?? ["--version"], { cwd, env: { ...deps.env, ...PNPM_NO_HOOKS_ENV, COREPACK_ENABLE_AUTO_PIN: "0", ...probeLedgerEnv }, signal: signalFor(ms) });
      // One retry, with three times the bound, when only the bound stopped it (the run itself was not aborted).
      let r = await probeOnce(probeMs);
      if (r?.cancelled && !signal?.aborted) r = await probeOnce(probeMs * 3);
      // A probe stopped by its own timeout (not by the run being aborted) says the host was busy, not that the tool is missing: it is
      // named as a timeout and not remembered, so the next scan probes again (on a loaded Dell `cargo --version` outlived the 60 s
      // bound while three compiles ran, and the cached failure then failed every later class B row at 0 s, hrwin 4ccadec2).
      const timedOut = Boolean(r?.cancelled) && !signal?.aborted;
      if (!r || r.error || r.status !== 0) outcome = { ok: false, transient: timedOut, why: timedOut ? `${tool} --version did not answer within ${Math.round(probeMs / 1000)} s and again within ${Math.round((probeMs * 3) / 1000)} s (host busy)` : r?.error?.message ?? `exit ${r?.status}` };
      else {
        const text = String(r.stdout || r.stderr || "");
        const version = PROBE_VERSION[tool] ? PROBE_VERSION[tool](text) : text.split(/\r?\n/).find((l) => l.trim())?.trim() ?? "unknown";
        const identity = trust?.identity ?? null;
        outcome = { ok: true, command, version: version ?? "unknown", pinProblem: pinnedVersionProblem(tool, version) ?? (identity ? enrolledVersionProblem(identity, version ?? text) : null), ...(identity ? { identity } : {}) };
        if (identity) toolIdentities[tool] = { ...identity, probed: version ?? "unknown" };
      }
    }
    toolVersions[tool] = outcome.ok ? outcome.version : null;
    return outcome;
  });
  // rustright's `--version` names the client, which no directory changes; it is probed where the scan runs only because the client refuses
  // outside a Cargo workspace (rightkit#1215). Once it has answered in one directory that answer serves every directory: each fresh probe is a
  // broker CONTROL-lane request, and one sent while this run holds the broker's quiet lease waits the lease out and fails (rightkit#1526).
  let rustrightAnswer = null;
  const probe = async (tool, scanDir = null) => {
    if (tool === "rustright" && rustrightAnswer) return rustrightAnswer;
    const trust = TRUSTED_SCAN_TOOLS.includes(tool) ? resolveTrustedScanTool(tool, { policy: options.toolPolicy, platform: deps.platform, ops: options.toolOps }) : null;
    const command = trust ? (trust.ok ? trust.identity.path : null) : toolCommand(tool);
    const cwd = (tool === "rustright" || (tool === "cargo" && cargoLiveRoot)) && scanDir ? scanDir : (deps.probeDir ?? probeDir());
    const key = sha256(canonicalJson({ tool, command, cwd, args: PROBES[tool] ?? ["--version"], env: deps.env }));
    const outcome = await probeFlight(key, tool, scanDir);
    if (tool === "rustright" && outcome?.ok && !outcome.pinProblem) rustrightAnswer = outcome;
    return outcome;
  };

  const jobs = Math.max(1, Number(options.jobs ?? 1) || 1);
  const testJobs = Math.max(1, Number(options.testJobs ?? jobs) || jobs);
  // { acquire: async () => ({ id, env }) | null, release: async (id) => {} }: the broker's quiet lease around the quiet lane. A
  // failed acquire runs the lane without one (the caller reports it); the lease is released however the lane ends.
  const quietLane = options.quietLane && typeof options.quietLane.acquire === "function" ? options.quietLane : null;
  const onResult = typeof options.onResult === "function" ? options.onResult : () => {};
  const total = ordered.length; let done = 0;
  // Every class A failure latches, whichever branch produced it (Astra, 2026-10-26: two early returns bypassed the latch below, so a
  // missing advisory database or a failed deny metadata could be followed by a class B compile). Class B reads the latch before it starts.
  async function runOne(s, extraEnv = null) {
    const row = await runOneBody(s, extraEnv);
    if (row.status === "fail" && s.class === SCAN_CLASS.A) classAFailed = true;
    return row;
  }
  async function runOneBody(s, extraEnv = null) {
    if (signal?.aborted) return { id: s.id, class: s.class, command: s.command, tool: s.tool ?? null, toolVersion: null, status: "not-run", code: "CANCELLED", detail: "cancelled before it started" };
    const started = Date.now();
    const scanStarted = performance.now();
    let row = await scanRow(s, extraEnv);
    // rightkit#1459: a class A scan that timed out is run once more before it fails Class A (a hung tool, e.g. actionlint on 2026-11-02, ran
    // 27 min and failed a run whose rerun passed in 1 s). Class B and carried scans are not retried: a compile that times out is a real failure.
    // Test suites are not retried either: a slow test is a finding, not a hung tool.
    if (row.status === "fail" && row.code === "TIMEOUT" && s.class === SCAN_CLASS.A && s.kind !== "test" && !signal?.aborted) {
      const first = row;
      row = await scanRow(s, extraEnv);
      row = { ...row, retried: { code: first.code, detail: first.detail, timing: first.timing ?? null } };
    }
    row = { ...row, scanElapsedMs: performance.now() - scanStarted };
    done += 1;
    try { onResult(row, { done, total, seconds: Math.round((Date.now() - started) / 1000) }); } catch { /* reporting never fails a scan */ }
    return row;
  }
  // Class A may run concurrently. Class B runs one scan per component at a time, `classBJobs` components at once (below).
  const aScans = ordered.filter((s) => s.class === SCAN_CLASS.A);
  const bScans = ordered.filter((s) => s.class === SCAN_CLASS.B);
  // Class A scans are independent content checks (and short broker calls: metadata, package list, deny, audit): they run
  // `classAJobs` at a time (default: `jobs`); the rows keep plan order. Class B starts only once every class A row is in.
  const aJobs = Math.max(1, Number(options.classAJobs ?? jobs) || jobs);
  {
    // Class A is everything mechanical and deterministic that does not compile (Adrian, 2026-10-27): the content checks first,
    // `classAJobs` at a time, then the class A tests (kind "test"), `testJobs` at a time, then the quiet ones alone under the
    // broker's quiet lease. A failed content check stops the tests: they run on content class A already judged.
    const slots = new Array(aScans.length);
    const content = []; const tests = []; const quietTests = [];
    aScans.forEach((s, i) => (s.kind === "test" ? (s.quiet === true ? quietTests : tests) : content).push(i));
    const drain = (queue, extraEnv = null) => async () => {
      while (queue.length) {
        const i = queue.shift();
        slots[i] = aScans[i].kind === "test" && classAFailed
          ? { id: aScans[i].id, class: aScans[i].class, command: aScans[i].command, tool: aScans[i].tool ?? null, toolVersion: null, status: "not-run", detail: "a class A content check failed; the class A tests start only after it passes" }
          : await runOne(aScans[i], extraEnv);
      }
    };
    await Promise.all(Array.from({ length: aJobs }, drain(content)));
    await Promise.all(Array.from({ length: testJobs }, drain(tests)));
    if (quietTests.length) {
      // One broker quiet lease per quiet suite (the broker caps a lease at 30 min and has no renewal): each suite runs alone,
      // under its own lease, with a timeout shorter than the lease, so a lease never expires while its suite still runs.
      for (const i of quietTests) {
        if (classAFailed || signal?.aborted) {
          slots[i] = { id: aScans[i].id, class: aScans[i].class, command: aScans[i].command, tool: aScans[i].tool ?? null, toolVersion: null, status: "not-run", detail: signal?.aborted ? "cancelled" : "a class A check failed; the quiet suites start only on a green class A" };
          continue;
        }
        let lease = null;
        if (quietLane) { try { lease = (await quietLane.acquire({ ttlMs: QUIET_LEASE_MAX_MS })) ?? null; } catch { lease = null; } }
        if (!lease && options.requireQuietLease) {
          slots[i] = { id: aScans[i].id, class: aScans[i].class, command: aScans[i].command, tool: aScans[i].tool ?? null, toolVersion: null, status: "fail", code: "QUIET_LEASE_UNAVAILABLE", detail: "the broker's quiet lease could not be held; this conflict-prone suite does not run alongside other jobs on a managed host. Run again when the broker can grant the lease." };
          classAFailed = true; // a refused class A suite is a red class A: no class B scan starts after it
          continue;
        }
        try { slots[i] = await runOne({ ...aScans[i], timeoutMs: Math.min(scanTimeoutMs(aScans[i], options), QUIET_SCAN_MAX_MS) }, lease?.env ?? null); } finally {
          if (lease && typeof quietLane.release === "function") { try { await quietLane.release(lease.id); } catch { /* the lease expires on its own */ } }
        }
      }
    }
    results.push(...slots);
  }
  {
    const slots = new Array(bScans.length);
    const lanes = { compile: [], doc: [], tests: [], quiet: [] };
    bScans.forEach((s, i) => lanes[laneOf(s)].push(i));
    // RightKit is many components, never one build (Adrian, 2026-10-29; rightkit#1285). A component is the directory a scan runs in (`cwd`, its
    // own Cargo workspace and output directory); scans of one component stay in plan order, because they share its output directory and cargo
    // locks it.
    // Direct host: class B scans of different components run `classBJobs` at a time (nothing else bounds work there), compile, doc, then tests.
    // Broker host (options.brokerHost; rightkit#1367 step 2, Adrian 2026-11-01: "the broker manages the how many builds happen. That is not the
    // job of the gate"): the scans the broker carries start with no count and no lane-wide wait, each component running its own chain while the
    // broker queues and orders them. A scan the broker never sees (an npm or node test suite) is a process of this host: those keep the local limit.
    const bJobs = Math.max(1, Number(options.classBJobs ?? (brokerHost ? LOCAL_CLASS_B_JOBS : 1)) || 1);
    const failedRow = (i, error) => ({ id: bScans[i].id, class: bScans[i].class, command: bScans[i].command, tool: bScans[i].tool ?? null, toolVersion: null, status: "fail", code: "SCAN_ERROR", detail: String(error?.message ?? error) });
    // Class B batching (B1, rightkit#1458; planScans `classBBatching`). The compile scans of one Cargo workspace that differ only in their `-p <crate>`
    // are one group; a group of two or more runs as ONE cargo invocation naming every member (`-p a -p b ...`) and the invocation's verdict becomes each
    // member's row, under the member's own id and command (so required scan ids and the scan-set digest are what the plan said). The invocation is a scan
    // of its own for everything else: it takes its own cache entry (the gate keys it on every member's inputs, `check:batch:` / `clippy:batch:`), cargo
    // ledger token, signed broker receipt and run limit. A cargo failure (code EXIT: a compile diagnostic) is bisected for attribution: each
    // half runs again as a smaller invocation, down to one crate, which runs as its own per-crate scan, so a failure always names the crate that fails and
    // the crates that pass still pass; the grouped failure itself is retained on every member row (`batchFailure`) and in the run notes, so a combined
    // failure stays visible even when the splits pass. Any other failure (a refused or unverified receipt, a timeout, a missing tool) is the group's and is not retried:
    // running it N times would repeat it N times. Equivalence caveat (GC-I11): one invocation unifies features across its crates, so a pass here is not
    // proof that every member passes alone; the plan notes say the equivalence run has not been done.
    const groupOf = new Map();
    if (options.classBBatching === true) {
      const byKey = new Map();
      bScans.forEach((s, i) => { if (s.batchable && !s.deferred && (laneOf(s) === "compile" || laneOf(s) === "doc" || (options.groupTests !== false && laneOf(s) === "tests" && s.runOutside && /^test:rust/.test(s.id)))) { if (!byKey.has(s.batchable.key)) byKey.set(s.batchable.key, []); byKey.get(s.batchable.key).push(i); } });
      for (const all of byKey.values()) {
        // NN42: within one key, crates with the same set of targets form one invocation (crates x targets is exactly its members).
        const targetsOf = new Map();
        for (const i of all) { const c = bScans[i].batchable.crate; if (!targetsOf.has(c)) targetsOf.set(c, new Set()); targetsOf.get(c).add(bScans[i].batchable.target ?? ""); }
        const parts = new Map();
        for (const i of all) { const sig = [...targetsOf.get(bScans[i].batchable.crate)].sort().join("\0"); if (!parts.has(sig)) parts.set(sig, []); parts.get(sig).push(i); }
        for (const members of parts.values()) if (members.length > 1) { const group = { members, promise: null }; for (const i of members) groupOf.set(i, group); }
      }
    }
    const batchScanOf = (idx) => {
      const lead = bScans[idx[0]];
      const { batchable: _batchable, ...plain } = lead; void _batchable;
      const crates = [...new Set(idx.map((i) => bScans[i].batchable.crate))];
      const targets = [...new Set(idx.map((i) => bScans[i].batchable.target).filter(Boolean))];
      let args = [...lead.args.slice(0, lead.batchable.at), ...crates.flatMap((c) => ["-p", c]), ...lead.args.slice(lead.batchable.at + 2)];
      // Benchmark-only merged feature rows: drop the lead's own feature flags and name every member's features as <crate>/<feature>.
      if (idx.some((i) => Array.isArray(bScans[i].batchable.features))) {
        const all = [...new Set(idx.flatMap((i) => bScans[i].batchable.features ?? []))];
        const stop = args.indexOf("--") < 0 ? args.length : args.indexOf("--");
        const out = []; for (let i = 0; i < args.length; i += 1) { const w = args[i]; if (i < stop && (w === "--features" || w === "-F")) { i += 1; continue; } if (i < stop && /^(?:--features=|-F.)/.test(w)) continue; out.push(w); }
        const cut = out.indexOf("--"); args = all.length ? (cut < 0 ? [...out, "--features", all.join(",")] : [...out.slice(0, cut), "--features", all.join(","), ...out.slice(cut)]) : out;
      }
      if (targets.length > 1) { const t = args.indexOf("--target"); if (t >= 0) args = [...args.slice(0, t), ...targets.flatMap((x) => ["--target", x]), ...args.slice(t + 2)]; }
      if (targets.length > 1) plain.env = Object.assign({}, ...idx.map((i) => bScans[i].env ?? {}));
      const units = [...new Set(idx.map((i) => bScans[i].batchable.unit))].map((u) => targets.length > 1 ? u : u);
      const memberKey = idx.map((i) => `${bScans[i].batchable.unit}@${bScans[i].batchable.target ?? ""}`);
      const stage = /^clippy/.test(lead.id) ? "clippy" : /^doc:/.test(lead.id) ? "doc" : /^test:rust/.test(lead.id) ? "test" : "check";
      const id = `${stage}:batch:${sha256(`${lead.batchable.key}\0${memberKey.join("\0")}`).slice(0, 8)}`;
      // A doc group asks for cargo's JSON messages (NN40): each crate's verdict is read from them (docVerdicts), never found by bisection.
      if (stage === "doc") { const jargs = [...args, "--message-format=json"]; return { ...plain, id, args: jargs, captureJson: true, command: commandText({ ...plain, env: {}, args: jargs }), batch: { units, members: idx.map((i) => bScans[i].id) } }; }
      // NN17/NN39: a test group compiles once (one broker command naming every crate) and each member's own test executables then run in
      // parallel outside the broker; each member keeps its own verdict (runOne `testGroup`, memberRuns).
      if (stage === "test") return { ...plain, id, args, testGroup: idx.map((i) => ({ id: bScans[i].id, crate: bScans[i].batchable.crate })), command: commandText({ ...plain, env: {}, args }), batch: { units, members: idx.map((i) => bScans[i].id) } };
      // NN40 for check and clippy too (Mac 2026-11-07: one crate's Linux error failed a 186-crate batch whole): the group asks for cargo's JSON messages,
      // so each crate's verdict is read from them (docVerdicts: its own errors, or no artifact because a dependency failed), never one verdict for all.
      if (!args.some((a) => /^--message-format/.test(a))) {
        const cut = args.indexOf("--"); const jargs = cut < 0 ? [...args, "--message-format=json"] : [...args.slice(0, cut), "--message-format=json", ...args.slice(cut)];
        return { ...plain, id, args: jargs, captureJson: true, command: commandText({ ...plain, env: {}, args: jargs }), batch: { units, members: idx.map((i) => bScans[i].id) } };
      }
      return { ...plain, id, args, command: commandText({ ...plain, env: {}, args }), batch: { units, members: idx.map((i) => bScans[i].id) } };
    };
    // Per-crate cache (Mac 2026-11-07: a batch's pass was stored only under the batch's own key, its exact set of crates, so any change in how
    // crates were grouped reran every one of them). Before a group runs, each member is looked up on its own key; a member that hits is its own
    // cached row, only the members that miss run (as a smaller group, or alone). A member that passes inside a group is NOT stored under its own key:
    // the group compiled with the features, targets and dependencies Cargo unified across it, which is not what the member compiles alone
    // (2026-11-07: 296 conflicting pairs across 28 crates; a member's projected pass hid its own error). The group's row is stored under the
    // group's own key by its own scanRow; a member hits alone only after it ran alone (store rule, Adrian 2026-11-08). In a cache lookup the members are reported one by one, each with its own cause.
    const runGroup = async (idx, extraEnv, budget, probed = false) => {
      if (probed || !cache || idx.length < 2 || signal?.aborted) return runGroupBody(idx, extraEnv, budget);
      const started = Date.now();
      const looked = await Promise.all(idx.map(async (i) => { const copy = { ...bScans[i] }; probeScans.add(copy); try { return [i, await scanRow(copy, extraEnv)]; } catch (error) { return [i, { ...failedRow(i, error) }]; } }));
      const missing = [];
      for (const [i, r] of looked) {
        if (r.cached === true) { slots[i] = { ...r, id: bScans[i].id, command: bScans[i].command }; done += 1; try { onResult(slots[i], { done, total, seconds: Math.round((Date.now() - started) / 1000) }); } catch { /* reporting never fails a scan */ } }
        else if (options.cacheLookupOnly) slots[i] = { ...r, id: bScans[i].id, command: bScans[i].command };
        else missing.push(i);
      }
      if (options.cacheLookupOnly || !missing.length) return;
      const run = missing;
      if (run.length === 1) slots[run[0]] = await runOne(bScans[run[0]], extraEnv).catch((error) => failedRow(run[0], error));
      else await runGroupBody(run, extraEnv, budget);
    };
    const runGroupBody = async (idx, extraEnv, budget = { remaining: Math.min(2 * idx.length - 1, 31) }) => {
      if (budget.remaining <= 0 || signal?.aborted) {
        for (const i of idx) slots[i] = { ...failedRow(i, "batch attribution was not run"), code: signal?.aborted ? "CANCELLED" : "BATCH_BUDGET", detail: signal?.aborted ? "batch attribution was cancelled" : "batch attribution budget exhausted" };
        return;
      }
      budget.remaining -= 1;
      if (idx.length === 1) { const i = idx[0]; slots[i] = await runOne(bScans[i], extraEnv).catch((error) => failedRow(i, error)); return; }
      const started = Date.now();
      const bs = batchScanOf(idx);
      let row;
      try { row = await scanRow(bs, extraEnv); } catch (error) { row = { id: bs.id, status: "fail", code: "SCAN_ERROR", detail: String(error?.message ?? error) }; }
      if (row.memberRuns && !signal?.aborted) {
        idx.forEach((i) => {
          const m = bScans[i]; const v = row.memberRuns[m.id] ?? { status: "fail", code: "TEST_GROUP", detail: `${m.id}: the test group ${row.id} produced no run for this member` };
          const own = { id: m.id, class: m.class, command: m.command, batchCommand: row.command, batchedWith: { id: row.id, units: bs.batch.units }, tool: m.tool ?? null, toolVersion: row.toolVersion ?? null, status: v.status, ...(v.code ? { code: v.code } : {}), ...(v.detail ? { detail: v.detail } : {}), ...(v.results ? { results: v.results } : {}), ...(v.testRun ? { testRun: v.testRun } : {}), ...(row.snapshotEvidence ? { snapshotEvidence: row.snapshotEvidence } : {}) };
          slots[i] = own; done += 1;
          try { onResult(own, { done, total, seconds: Math.round((Date.now() - started) / 1000) }); } catch { /* reporting never fails a scan */ }
        });
        return;
      }
      // NN40: a failed doc invocation is attributed from its own JSON messages: a crate with an error message fails, a crate with no artifact while
      // another member has one fails, the rest pass. Without usable messages (none, or no artifact for any member) every member carries the group's failure (below); nothing is bisected
      // (Adrian 2026-11-08, store rule: a row is stored only for the invocation that ran it; blame comes from Cargo's messages, not from reruns).
      const verdicts = row.status === "fail" && bs.captureJson && row.jsonSummary?.records > 0 && !signal?.aborted ? docVerdicts(row.jsonSummary, idx.map((i) => bScans[i].batchable.unit), idx.map((i) => bScans[i].batchable.crate)) : null;
      if (verdicts && !signal?.aborted) {
        batchFailureNotes.push(`class B batching: grouped doc invocation ${bs.id} (${idx.length} crates: ${bs.batch.units.join(", ")}) failed (EXIT) and was attributed per crate from its --message-format=json output, not bisected; failing: ${verdicts.filter((v) => v.fail).map((v) => v.unit).join(", ") || "none named"}`);
        idx.forEach((i, k) => {
          const m = bScans[i]; const v = verdicts[k];
          const own = { id: m.id, class: m.class, command: m.command, batchCommand: row.command, tool: m.tool ?? null, toolVersion: row.toolVersion ?? null, status: v.fail ? "fail" : "pass", ...(v.fail ? { code: "EXIT", detail: `${row.id} (${idx.length} crates: ${bs.batch.units.join(", ")}): ${v.detail}` } : {}), ...(row.cached ? { cached: true } : {}), timing: row.timing ?? null, batchedWith: { id: row.id, units: bs.batch.units }, batchFailure: { id: bs.id, units: bs.batch.units, command: row.command, ...(row.authenticatedFailure ? { authenticatedFailure: row.authenticatedFailure } : {}), ...(row.snapshotEvidence ? { snapshotEvidence: row.snapshotEvidence } : {}), detail: "the grouped doc invocation failed; this row's verdict is its own, read from the invocation's JSON messages" }, ...(row.identities ? { identities: row.identities } : {}) };
          slots[i] = own; done += 1;
          try { onResult(own, { done, total, seconds: Math.round((Date.now() - started) / 1000) }); } catch { /* reporting never fails a scan */ }
        });
        // NN42: cargo's messages name the crate, not the target. A crate that failed in an invocation naming several targets runs each of its
        // targets alone, so the verdict names the target that fails and the others pass on their own.
        const multi = new Set(idx.map((i) => bScans[i].batchable.target).filter(Boolean)).size > 1;
        if (multi && !signal?.aborted) {
          const redo = idx.filter((i) => slots[i]?.status === "fail");
          await Promise.all(redo.map(async (i) => { const alone = await runOne(bScans[i], extraEnv).catch((error) => failedRow(i, error)); slots[i] = { ...alone, batchFailure: { id: bs.id, units: bs.batch.units, command: row.command, detail: "the multi-target invocation failed for this crate; this target ran alone" } }; }));
        }
        return;
      }
      for (const i of idx) {
        const m = bScans[i];
        const own = { id: m.id, class: m.class, command: m.command, batchCommand: row.command, tool: m.tool ?? null, toolVersion: row.toolVersion ?? null, status: row.status, ...(row.code ? { code: row.code } : {}), ...(row.detail ? { detail: row.status === "fail" ? `${row.id} (${idx.length} crates: ${bs.batch.units.join(", ")}): ${row.detail}` : row.detail } : {}), ...(row.cached ? { cached: true } : {}), timing: row.timing ?? null, batchedWith: { id: row.id, units: bs.batch.units }, ...(row.authenticatedFailure ? { authenticatedFailure: row.authenticatedFailure } : {}), ...(row.snapshotEvidence ? { snapshotEvidence: row.snapshotEvidence } : {}), ...(row.identities ? { identities: row.identities } : {}), ...(row.log ? { log: row.log } : {}), ...(options.cacheLookupOnly ? { lookupMiss: row.lookupMiss === true, uncacheable: row.uncacheable === true, ...(row.cacheMiss ? { cacheMiss: row.cacheMiss } : {}) } : {}) };
        slots[i] = own; done += 1;
        try { onResult(own, { done, total, seconds: Math.round((Date.now() - started) / 1000) }); } catch { /* reporting never fails a scan */ }
      }
    };
    // The one entry both schedulers use: a scan outside every group runs as before; the first member of a group to start runs the whole group, and a later
    // member (same component, so it starts after) finds its row already in place.
    const runB = async (i, extraEnv = null) => {
      const group = groupOf.get(i);
      if (!group) return runOne(bScans[i], extraEnv);
      if (!group.promise) group.promise = runGroup(group.members, extraEnv);
      await group.promise;
      return slots[i] ?? failedRow(i, "its batched invocation produced no row");
    };
    const drain = async (queue, extraEnv = null) => {
      const busy = new Set(); const running = new Set();
      const keyOf = (i) => bScans[i].executionWorkspace ?? bScans[i].cwd ?? ".";
      while (queue.length || running.size) {
        const at = running.size < bJobs ? queue.findIndex((i) => !busy.has(keyOf(i))) : -1;
        if (at === -1) { await Promise.race(running); continue; }
        const [i] = queue.splice(at, 1); const key = keyOf(i); busy.add(key);
        // A scan that throws becomes a failed row: with several components in flight, a rejection must not abandon the others.
        const job = runB(i, extraEnv)
          .catch((error) => failedRow(i, error))
          .then((row) => { slots[i] = row; }).finally(() => { busy.delete(key); running.delete(job); });
        running.add(job);
      }
    };
    if (typeof options.scheduleClassB === "function") {
      // A caller-supplied dependency scheduler uses this initialized executor: every job still passes Class A,
      // tool, carrier, ledger, receipt, timeout and quiet-lease checks in the same scan path.
      const issued = new WeakSet();
      const remember = (row) => { issued.add(row); return row; };
      const executeRaw = async (scan) => {
        if (!scan.quiet) return runOne(scan);
        if (classAFailed || classBBlocked || signal?.aborted) return runOne(scan);
        let lease = null;
        if (quietLane) { try { lease = (await quietLane.acquire({ ttlMs: QUIET_LEASE_MAX_MS })) ?? null; } catch { lease = null; } }
        if (!lease && options.requireQuietLease) return { id: scan.id, class: scan.class, command: scan.command, status: "fail", code: "QUIET_LEASE_UNAVAILABLE", detail: "the broker quiet lease could not be held" };
        try { return await runOne({ ...scan, timeoutMs: Math.min(scanTimeoutMs(scan, options), QUIET_SCAN_MAX_MS) }, lease?.env ?? null); }
        finally { if (lease && typeof quietLane.release === "function") { try { await quietLane.release(lease.id); } catch { /* lease expires */ } } }
      };
      const execute = async (scan) => remember(Object.freeze(await executeRaw(scan)));
      // A cache probe of one scan (publisher batches, unit-list-compile): answers from the scan's OWN key (a cached pass, or a miss carrying what a later
      // per-crate store needs), never runs it. `store` writes a member's pass, found in a batch, under that member's own key and row index, as if it ran alone.
      const probe = async (scan) => {
        if (!cache || signal?.aborted) return null;
        const copy = { ...scan }; probeScans.add(copy);
        try { const r = await scanRow(copy, null); return remember(Object.freeze({ ...r, id: scan.id, command: scan.command })); } catch { return null; }
      };
      const store = (write, own) => {
        if (!cache || !write || own?.status !== "pass" || signal?.aborted) return;
        try {
          cache.write(write.key, { id: own.id, class: own.class, command: own.command, tool: own.tool ?? null, toolVersion: own.toolVersion ?? null, status: "pass", snapshotEvidence: own.snapshotEvidence ?? null, testRun: own.testRun ?? null, ...(own.results ? { results: own.results } : {}) });
          if (write.index) cache.write(write.index.key, { id: own.id, inputs: write.index.inputs, env: write.index.env, raw: write.index.raw, at: new Date().toISOString() });
        } catch { /* the pass stands; the next run runs the member again */ }
      };
      const canGroup = (a, b) => { const i = bScans.indexOf(a); const j = bScans.indexOf(b); return i >= 0 && j >= 0 && Boolean(groupOf.get(i)) && groupOf.get(i) === groupOf.get(j); };
      // The caller supplies only members ready on its current frontier. Reuse the existing
      // signed batch/bisection path without admitting other members of their planned group.
      const executeGroup = async (scans) => {
        if (!Array.isArray(scans) || scans.length < 2) throw new Error("scheduleClassB: a batch needs at least two scans");
        const indices = scans.map((scan) => bScans.indexOf(scan));
        const group = groupOf.get(indices[0]);
        if (!group || new Set(indices).size !== indices.length || indices.some((i) => i < 0 || groupOf.get(i) !== group)) throw new Error("scheduleClassB: incompatible batch members");
        await runGroup(indices, null);
        return indices.map((i) => remember(Object.freeze(slots[i] ?? failedRow(i, "batch produced no row"))));
      };
      const project = (row, scan) => {
        if (!issued.has(row) || !bScans.includes(scan)) throw new Error("scheduleClassB: invalid projection");
        return remember(Object.freeze({ ...row, id: scan.id, command: scan.command }));
      };
      // A grouped publisher test row is one issued row for the whole group; each member's own verdict (its executables' status, results, test run)
      // is taken from that row's memberRuns, never invented: the member row is the issued row with that member's run in place of the group's.
      const projectMember = (row, scan, memberId) => {
        if (!issued.has(row) || !bScans.includes(scan) || !row.memberRuns || !Object.hasOwn(row.memberRuns, memberId)) throw new Error("scheduleClassB: invalid member projection");
        const v = row.memberRuns[memberId]; const { memberRuns: _all, results: _sum, code: _code, detail: _detail, ...shared } = row; void _all; void _sum; void _code; void _detail;
        return remember(Object.freeze({ ...shared, id: scan.id, command: scan.command, status: v.status, results: v.results ?? null, testRun: v.testRun ?? null, ...(v.code ? { code: v.code } : {}), ...(v.detail ? { detail: v.detail } : {}), batchedWith: { id: row.id } }));
      };
      const notRun = (scan, detail) => {
        if (!bScans.includes(scan)) throw new Error("scheduleClassB: unknown scan");
        return remember(Object.freeze({ id: scan.id, class: scan.class, command: scan.command, status: "not-run", detail: String(detail) }));
      };
      // lookupOnly: the scheduler looks up every row it would run, treating a lookup as the pass the real run needs to reach later rows.
      const rows = await options.scheduleClassB({ scans: bScans, execute, executeGroup, canGroup, project, projectMember, notRun, probe, store, lookupOnly: options.cacheLookupOnly === true });
      if (!Array.isArray(rows) || rows.length !== bScans.length || rows.some((row, i) => !row || !issued.has(row) || row.id !== bScans[i].id || typeof row.status !== "string")) throw new Error("scheduleClassB must return executor-issued results in plan order");
      rows.forEach((row, i) => { slots[i] = row; });
    } else if (brokerHost) {
      await scheduleBrokerHostClassB({
        scans: bScans, lanes, localJobs: bJobs, carried: (scan) => isBrokerScheduledScan(scan, carriedCtx), pipeline: options.classBPipeline ?? 1,
        run: async (i) => { try { slots[i] = await runB(i); } catch (error) { slots[i] = failedRow(i, error); } },
      });
    } else {
      await drain(lanes.compile);
      await drain(lanes.doc);
      await drain(lanes.tests);
    }
    if (typeof options.scheduleClassB !== "function" && lanes.quiet.length) {
      // One lease per quiet suite, as in class A above.
      for (const i of lanes.quiet) {
        if (classAFailed || classBBlocked || signal?.aborted) {
          slots[i] = { id: bScans[i].id, class: bScans[i].class, command: bScans[i].command, tool: bScans[i].tool ?? null, toolVersion: null, status: "not-run", detail: signal?.aborted ? "cancelled" : classAFailed ? "class A is red; nothing in class B starts" : "class B is blocked" };
          continue;
        }
        let lease = null;
        if (quietLane) { try { lease = (await quietLane.acquire({ ttlMs: QUIET_LEASE_MAX_MS })) ?? null; } catch { lease = null; } }
        if (!lease && options.requireQuietLease) {
          slots[i] = { id: bScans[i].id, class: bScans[i].class, command: bScans[i].command, tool: bScans[i].tool ?? null, toolVersion: null, status: "fail", code: "QUIET_LEASE_UNAVAILABLE", detail: "the broker's quiet lease could not be held; this conflict-prone suite does not run alongside other jobs on a managed host. Run again when the broker can grant the lease." };
          continue;
        }
        try { slots[i] = await runOne({ ...bScans[i], timeoutMs: Math.min(scanTimeoutMs(bScans[i], options), QUIET_SCAN_MAX_MS) }, lease?.env ?? null); } finally {
          if (lease && typeof quietLane.release === "function") { try { await quietLane.release(lease.id); } catch { /* the lease expires on its own */ } }
        }
      }
    }
    results.push(...slots);
  }

  // The pinned advisory database, materialised once per run on first use (a failure is remembered and refuses every advisory scan).
  function prepareAdvisory(s) {
    const key = s.advisoryPin.commit;
    if (!advisoryDbs.has(key)) {
      try {
        const ensure = deps.ensureAdvisoryDb ?? ensureAdvisoryDb;
        advisoryDbs.set(key, { dir: ensure({ cacheRoot: deps.advisoryCacheRoot ?? advisoryCacheRoot({ env: deps.env, platform: deps.platform }), pin: s.advisoryPin }) });
      } catch (e) { advisoryDbs.set(key, { error: e }); }
    }
    const db = advisoryDbs.get(key);
    if (db.error) throw db.error;
    if (s.advisoryDb !== "deny") return { dbDir: db.dir };
    const prepare = deps.prepareDenyOverlay ?? prepareDenyOverlay;
    let denyToml = "";
    try { denyToml = deps.readFile(path.join(rootDir(s.root), s.cwd ?? "", "deny.toml")); } catch {
      // No deny.toml in the workspace: RightKit's own policy (deny-default.toml) applies, never cargo-deny's empty default,
      // which allows no licence at all.
      denyToml = deps.defaultDenyToml ?? fs.readFileSync(new URL("./deny-default.toml", import.meta.url), "utf8");
    }
    const overlay = prepare({ denyToml, dbDir: db.dir, pin: s.advisoryPin, tmpRoot });
    return { dbDir: db.dir, configPath: overlay.configPath, cleanup: overlay.cleanup };
  }

  // `extraEnv` (the quiet lease id) reaches the child but never the cache key: a lease changes nothing a scan proves.
  async function scanRow(s, extraEnv = null) {
    // One invocation owns one timing, including cache lookup, probes, execution and verification.
    const started = performance.now();
    const timing = { invocationId: crypto.randomUUID(), elapsedMs: null, cacheLookupMs: null, executionMs: null };
    const owned = { cargoReadTarget: null };
    try {
      const row = await scanRowIn(s, extraEnv, owned, timing);
      return { ...row, timing: { ...timing, elapsedMs: performance.now() - started } };
    } finally { if (owned.cargoReadTarget) fs.rmSync(owned.cargoReadTarget, { recursive: true, force: true }); }
  }
  async function scanRowIn(s, extraEnv, owned, timing) {
    const row = { id: s.id, class: s.class, command: s.command, tool: s.tool ?? null, toolVersion: null, status: "fail" };
    const scanStartedAt = Date.now();
    if (s.class === SCAN_CLASS.B && classBBlocked) {
      return { ...row, status: "not-run", code: "CLASS_A_REQUIRED", detail: "class B refused: class A did not run in this call and no valid prior class A evidence for the same scan set was supplied" };
    }
    if (s.class === SCAN_CLASS.B && classAFailed) {
      return { ...row, status: "not-run", detail: "a class A scan failed; class B starts only after class A passes" };
    }
    let status = "pass"; let detail = ""; let code; let advice;
    if (s.deferred) {
      // A class A scan that did not run is not a class A pass: class B waits.
      if (s.class === SCAN_CLASS.A) classAFailed = true;
      return { ...row, status: "deferred", detail: s.deferred };
    }
    try {
      // GC-A24: a deterministic internal rule (its verdict a function of the files it reads and the rule code) is reused by its input key,
      // with the rule code's identity (CHECKER_IDENTITY) as its tool version. Network and advice rules are never reused this way.
      let internalKey = null; let internalFailKey = null; let internalIndex = null;
      if (s.internal && cache && CACHEABLE_INTERNAL.has(s.internal) && typeof CHECKER_IDENTITY === "string") {
        try {
          const digest = cache.digest(s, {});
          if (digest) {
            // NN23: keyed on the internal rules' schema (bumped by hand when a rule's verdict changes), not on the checker's source bytes.
            internalKey = scanCacheKey({ id: s.id, command: s.command ?? `internal:${s.internal}`, toolVersion: INTERNAL_RULES_SCHEMA, env: {}, inheritedEnv: inheritedEnvDigest({}), digest });
            internalIndex = { key: sha256(`rk-row-index/1\0${s.id}`), inputs: sha256(String(digest)), env: INTERNAL_RULES_SCHEMA, raw: { inputs: String(digest), toolVersion: INTERNAL_RULES_SCHEMA, env: {}, inherited: null } };
            // NN41: a file-only rule's violation list is a function of the same inputs, so it is keyed like its pass plus the rule code (CHECKER_IDENTITY);
            // a rule whose answer depends on the day or a remote service (FAILURE_UNCACHEABLE_INTERNAL) never has its failure reused.
            if (!FAILURE_UNCACHEABLE_INTERNAL.has(s.internal)) internalFailKey = scanCacheKey({ id: s.id, command: `${s.command ?? `internal:${s.internal}`} ;; ${FAILURE_KEY_SCHEMA}`, toolVersion: INTERNAL_RULES_SCHEMA, env: {}, inheritedEnv: inheritedEnvDigest({}), digest });
          }
        } catch (error) { internalKey = null; internalFailKey = null; row.cacheKeyError = String(error?.message ?? error).slice(0, 300); }
        const hit = internalKey ? cache.read(internalKey) : null;
        if (internalIndex) { const idx = cache.read(internalIndex.key)?.row ?? null; row.cacheMiss = { kind: !idx ? "new" : idx.inputs === internalIndex.inputs && idx.env === internalIndex.env ? "unchanged" : "changed", ...(idx ? { last: idx.at ?? null } : {}), ...(idx?.raw ? { why: explainKeyChange(idx.raw, internalIndex.raw) } : {}), now: { inputs: internalIndex.raw.inputs.slice(0, 4000) } }; }
        if (hit && hit.key === internalKey && hit.row?.status === "pass" && hit.row.id === s.id) {
          cacheHits.push(s.id);
          return { ...row, status: "pass", cached: true, detail: `cached ${String(hit.at ?? "").slice(0, 10) || "earlier"}` };
        }
        const failHit = internalFailKey ? cache.read(internalFailKey) : null;
        if (failHit && failHit.key === internalFailKey && failHit.row?.status === "fail" && failHit.row.id === s.id && typeof failHit.row.detail === "string" && failHit.row.detail !== "") {
          cacheHits.push(s.id);
          if (s.class === SCAN_CLASS.A) classAFailed = true;
          return { ...row, status: "fail", cached: true, failureCached: true, detail: failHit.row.detail };
        }
      }
      if (options.cacheLookupOnly && s.internal) return { ...row, status: "not-run", lookupMiss: Boolean(internalKey), detail: internalKey ? "would miss the cache" : "has no cache key" };
      if (s.internal) {
        const out = await INTERNAL[s.internal](s, ctx);
        if (out && out.missing) { status = "fail"; code = "TOOL_MISSING"; detail = out.missing; }
        else if (out && !Array.isArray(out) && out.code) { status = "fail"; code = out.code; detail = out.detail; }
        else if (out && !Array.isArray(out) && typeof out.advice === "string") { advice = out.advice; }
        else if (out.length) { status = "fail"; detail = out.join("; "); }
        if (status === "pass" && internalKey && !advice && !signal?.aborted && internalIndex) cache.write(internalIndex.key, { id: s.id, inputs: internalIndex.inputs, env: internalIndex.env, raw: internalIndex.raw, at: new Date().toISOString() });
        if (status === "pass" && internalKey && !advice && !signal?.aborted) cache.write(internalKey, { id: s.id, class: s.class, command: s.command ?? `internal:${s.internal}`, tool: null, toolVersion: CHECKER_IDENTITY, status: "pass" });
        // NN41: only a violation list (the rule ran and named what is wrong) is a deterministic failure; a missing tool or a coded refusal is the environment's.
        else if (status === "fail" && !code && internalFailKey && Array.isArray(out) && out.length && !signal?.aborted) cache.write(internalFailKey, { id: s.id, class: s.class, command: s.command ?? `internal:${s.internal}`, tool: null, toolVersion: CHECKER_IDENTITY, status: "fail", detail });
      } else {
        const scanDir = path.join(rootDir(s.root), s.cwd ?? "");
        // The RustRight client's version probe starts in a Cargo workspace of the primary checkout; a class A cargo scan's own directory is
        // in the snapshot, so its probe uses the checkout's counterpart (reading no file of it), or the checkout when it has none.
        let probeAt = scanDir;
        if (s.class !== SCAN_CLASS.A && cargoLiveRoot && s.root !== "cargoLive" && (s.tool === "rustright" || s.tool === "cargo")) {
          const counterpart = path.join(cargoLiveRoot, s.cwd ?? "");
          let isDir = false; try { isDir = fs.statSync(counterpart).isDirectory(); } catch { isDir = false; }
          probeAt = isDir ? counterpart : cargoLiveRoot;
        }
        const p = await probe(s.tool, probeAt);
        row.toolVersion = p.ok ? p.version : null;
        // The broker runs the cargo-deny / cargo-audit a carried scan names: that identity is the broker's tool registration, not the gate's to bind.
        // `options.cargoReadsCarried === true` (the gate, when its executor is the broker and cargo reads go through carried requests, as in the check stage that has no snapshotBroker) makes them carried exactly as snapshotBroker does; it waives nothing else (actionlint and shellcheck stay locally enrolled).
        const brokerCarried = cargoReadsAreCarried && s.class === SCAN_CLASS.A && (s.tool === "rustright" || s.tool === "cargo");
        const advisoryTool = s.advisoryDb === "deny" ? "cargo-deny" : s.advisoryDb === "audit" ? "cargo-audit" : null;
        const trusts = advisoryTool && !brokerCarried ? [advisoryTool] : [];
        const identities = []; if (p.identity) identities.push(p.identity);
        let missingDep = null; let depVersionProblem = null; let depTrust = null; const depVersions = [];
        for (const dep of [...(s.requires ?? []), ...trusts]) {
          const d = await probe(dep, scanDir);
          if (!d.ok) { if (d.code && d.code !== "TOOL_MISSING") depTrust = { code: d.code, why: d.why, dep }; else missingDep = `${dep} (${d.why})`; break; }
          if (d.pinProblem) { depVersionProblem = d.pinProblem; break; }
          depVersions.push(d.identity ? `${d.version}@${d.identity.sha256}` : d.version);
          if (d.identity) identities.push(d.identity);
        }
        if (advisoryTool && brokerCarried) row.identities = [{ tool: advisoryTool, source: "broker", note: "the broker runs its own registered tool; its identity is the broker's tool registration, not bound by the gate" }];
        else if (identities.length) row.identities = identities.map((i) => ({ tool: i.tool, path: i.path, sha256: i.sha256, version: i.version, source: i.source }));
        if (!p.ok) { status = "fail"; code = p.code ?? "TOOL_MISSING"; detail = p.code && p.code !== "TOOL_MISSING" ? `${p.code}: ${p.why}` : `required tool ${s.tool} is missing or not runnable (${p.why}); a required scan is never skipped`; }
        else if (p.pinProblem) { status = "fail"; code = "TOOL_VERSION"; detail = `TOOL_VERSION: ${p.pinProblem} (at ${p.command})`; }
        else if (depTrust) { status = "fail"; code = depTrust.code; detail = `${depTrust.code}: ${s.tool} needs ${depTrust.dep}: ${depTrust.why}`; }
        else if (missingDep) { status = "fail"; code = "TOOL_MISSING"; detail = `${s.tool} needs ${missingDep}, which is missing or not runnable; without it the scan would pass unchecked, so it fails`; }
        else if (depVersionProblem) { status = "fail"; code = "TOOL_VERSION"; detail = `TOOL_VERSION: ${s.tool} runs ${depVersionProblem}`; }
        else {
          let cacheKey = null; let failKey = null; let ledgerProblem = false; let keyParts = null;
          // GATE-CONTRACT GC-T14: every unchanged scan result is reused by its full input key, a broker-carried compile included (measured
          // 2026-11-02: HR class B re-ran 92 broker jobs, most with no dirty unit, because carried compiles were never cached). The key covers the
          // crate's closure, its lock entries, the toolchain files, the tool version and the build environment; a miss runs the compile and
          // verifies the broker's signed evidence as before, and only a verified pass is written.
          const carriedScan = isBrokerCarriedScan(s, carriedCtx);
          const viaSnapshotBroker = carriedScan && Boolean(deps.snapshotBroker) && s.root === "cargoLive" && (s.tool === "rustright" || s.tool === "cargo");
          const carriedRead = cargoReadsAreCarried && s.class === SCAN_CLASS.A && (s.tool === "rustright" || s.tool === "cargo");
          // NN02: a publisher row (coverageKey) is cached too, with the signed broker receipt and test-run evidence its publisher verify needs.
          if (cache && !carriedRead) {
            const cacheStarted = performance.now();
            try {
              // A plain node --check row has no guard, carrier, advisory or
              // trusted-tool environment additions. Include its actual ledger
              // token and sentinel (when present) in the narrow key.
              const nodeSentinel = options.cargoSentinel && !Object.hasOwn(s.env ?? {}, "CARGO_TARGET_DIR") ? path.join(options.cargoSentinel, sha256(s.id).slice(0, 16)) : null;
              const nodeEnv = { ...deps.env, ...(nodeSentinel ? { CARGO_TARGET_DIR: nodeSentinel, CARGO_BUILD_TARGET_DIR: nodeSentinel } : {}), ...s.env, ...(extraEnv ?? {}), ...(cargoLedger ? { [SCAN_TOKEN_ENV]: scanTokenOf(s.id) } : {}) };
              const digest = cache.digest(s, { childEnv: nodeEnv, cwd: scanDir, command: p.command, shimProof: options.nodeCheckShimContext ?? null });
              // The analyzers a scan runs through (actionlint's shellcheck) judge it too, so their versions key it as well.
              const toolVersion = [p.identity ? `${p.version}@${p.identity.sha256}` : p.version, ...depVersions].join(" + ");
              // The narrow node-check digest already binds the complete actual
              // child environment (with its one explicit ownership exception).
              // NN29: a publisher row carries the gate's whole environment, per-run identities included (build ownership handle, carrier map, run markers):
              // its key drops them, or every run would miss.
              // Only build-relevant names key a publisher row (buildEnvName: compiler, cargo, toolchain, RK_/RIGHTKIT_ switches ...): the rest of the gate's environment (the
              // calling session's ids, sockets, PATH order, locale) is not an input of the compile (Mac 2026-11-07: a restarted session changed CLAUDE_* and 73 passes missed).
              const keyEnv = (env) => (s.coverageKey ? Object.fromEntries(Object.entries(env ?? {}).filter(([k]) => buildEnvName(k) && !/^(?:RIGHTKIT_BUILD_OWNERSHIP|RK_GATE_|RIGHTKIT_CARRIED_|RIGHTKIT_CARGO_SCAN|RIGHTKIT_RUN_|RUSTRIGHT_OWNERSHIP|TMPDIR$|PWD$|OLDPWD$|SHLVL$|_$)/.test(k))) : env);
              const inherited = cache.narrowNodeCheck && /^node-check:/.test(s.id) ? sha256('node-check-child-env-in-digest/1') : inheritedEnvDigest(keyEnv({ ...deps.env, ...s.env }));
              if (digest) {
                cacheKey = scanCacheKey({ id: s.id, command: keyCommand(s), toolVersion, env: keyEnv(s.env ?? {}), inheritedEnv: inherited, digest });
                keyParts = { inputs: sha256(String(digest)), env: sha256(canonicalJson({ toolVersion, env: keyEnv(s.env ?? {}), inherited })), raw: { inputs: String(digest), toolVersion, env: keyEnv(s.env ?? {}), inherited, inheritedShape: inheritedEnvShape(keyEnv({ ...deps.env, ...s.env })) } };
                // NN41: a deterministic compile failure is keyed like the pass plus the tool version (the client's, its identity digest), the installed broker runtime
                // identity (options.failureIdentity: version, clientDigest, brokerDigest) and the failure namespace; the digest already binds the unit closure, lock entries and toolchain files.
                // No identity from the caller means no failure is ever stored or reused.
                if (typeof options.failureIdentity === "string" && options.failureIdentity !== "" && isFailureCacheableCompileId(s.id) && !s.runOutside && s.kind !== "test" && !s.publisherRustTest && (s.tool === "rustright" || s.tool === "cargo")) failKey = scanCacheKey({ id: s.id, command: `${keyCommand({ command: s.command })} ;; ${FAILURE_KEY_SCHEMA}`, toolVersion: `${toolVersion} ;; broker-runtime ${options.failureIdentity}`, env: s.env ?? {}, inheritedEnv: inherited, digest });
              }
            } catch (error) { cacheKey = null; row.cacheKeyError = String(error?.message ?? error).slice(0, 300); }
            let hit = cacheKey ? cache.read(cacheKey) : null;
            // Adrian 2026-11-06: a rerun that misses the cache fails unless the miss is explained. Each stored pass leaves a per-row index
            // (its inputs digest and environment digest); a later miss is "unchanged" (inputs and environment the same: the cache failed),
            // "changed" (an input or the environment moved: a legitimate rerun) or "new" (no stored pass: a new row, or a cold run).
            if (cacheKey && keyParts) {
              const rowIndexKey = sha256(`rk-row-index/1\0${s.id}`);
              const inputsDigest = keyParts.inputs;
              const envDigest = keyParts.env;
              const idx = cache.read(rowIndexKey)?.row ?? null;
              row.cacheMiss = { kind: !idx ? "new" : idx.inputs === inputsDigest && idx.env === envDigest ? "unchanged" : "changed", ...(idx ? { last: idx.at ?? null } : {}), ...(idx?.raw ? { why: explainKeyChange(idx.raw, keyParts.raw) } : {}), now: { inputs: keyParts.raw.inputs.slice(0, 4000), toolVersion: keyParts.raw.toolVersion, inherited: keyParts.raw.inherited } };
              row.rowIndex = { key: rowIndexKey, inputs: inputsDigest, env: envDigest, raw: keyParts.raw };
              // A hit refreshes a row index that lacks the environment shape (stored before the shape existed), so a later key change can carry this row over.
              if (hit && hit.key === cacheKey && hit.row?.status === "pass" && !options.cacheLookupOnly && !probeScans.has(s) && !idx?.raw?.inheritedShape) {
                try { cache.write(rowIndexKey, { id: s.id, inputs: inputsDigest, env: envDigest, raw: keyParts.raw, at: new Date().toISOString() }); } catch { /* the hit stands */ }
              }
              // Carry-over (carryOverProof): a pass stored under an earlier key form, whose real inputs provably did not change, is this row's pass.
              if (!(hit && hit.key === cacheKey && hit.row?.status === "pass") && idx?.raw && row.cacheMiss.kind === "changed") {
                // entryNow (a path's content id now) exists on the class A cache; a class B row has none and its inputs digest is one string, so only an
                // environment-only difference can carry over there (no changed entry is ever looked up).
                const proof = carryOverProof(idx.raw, keyParts.raw, { entryNow: typeof cache.entryNow === "function" ? cache.entryNow : () => null });
                if (!proof.ok && proof.edited?.length && row.cacheMiss.why) row.cacheMiss.why = { ...row.cacheMiss.why, contentOnly: true, formChangesProven: proof.carried };
                if (proof.ok) {
                  const oldKey = scanCacheKey({ id: s.id, command: keyCommand(s), toolVersion: idx.raw.toolVersion, env: idx.raw.env ?? {}, inheritedEnv: idx.raw.inherited, digest: idx.raw.inputs });
                  const old = cache.read(oldKey);
                  if (old && old.key === oldKey && old.row?.status === "pass" && old.row.id === s.id && sameCommand(old.row.command, s)) {
                    hit = { ...old, key: cacheKey };
                    row.carriedOver = { from: oldKey.slice(0, 16), files: proof.carried };
                    if (!options.cacheLookupOnly) { try { cache.write(cacheKey, old.row); cache.write(rowIndexKey, { id: s.id, inputs: inputsDigest, env: envDigest, raw: keyParts.raw, at: new Date().toISOString() }); } catch { /* the pass stands; the next run carries it again */ } }
                  }
                }
              }
            }
            timing.cacheLookupMs = performance.now() - cacheStarted;
            // A cached test pass counts only with the counts it recorded: without them its coverage is unknown, so it runs again.
            // ...and only if it passed at least one test: a cached all-skipped "pass" (written before D17) runs again (rightkit#1236).
            // A Rust test scan (runOutside) is judged the same way: the executables are unknown until the compile, so its key is the source digest and the cached row must carry the summed libtest counts.
            const testEvidence = (s.kind !== "test" && !s.runOutside) || testResultsProblem(hit?.row?.results) === null;
            const publisherEvidence = !s.coverageKey || !viaSnapshotBroker || /^[0-9a-f]{64}$/.test(hit?.row?.snapshotEvidence?.receiptSha256 ?? "");
            if (hit && hit.key === cacheKey && hit.row?.status === "pass" && hit.row.id === s.id && sameCommand(hit.row.command, s) && testEvidence && publisherEvidence) {
              cacheHits.push(s.id);
              return { ...row, status: "pass", cached: true, detail: `cached ${String(hit.at ?? "").slice(0, 10) || "earlier"}`, ...(s.kind === "test" || s.runOutside || s.coverageKey ? { results: hit.row.results } : {}), ...(s.coverageKey ? { snapshotEvidence: hit.row.snapshotEvidence ?? null, testRun: hit.row.testRun ?? null } : {}) };
            }
            // NN41: the same inputs, toolchain and broker runtime already failed this compile deterministically: report that failure, marked cached, without running it.
            const failHit = failKey ? cache.read(failKey) : null;
            if (failHit && failHit.key === failKey && failHit.row?.status === "fail" && failHit.row.id === s.id && sameCommand(failHit.row.command, s) && failHit.row.failureIdentity === options.failureIdentity && (failHit.row.code === "EXIT" || failHit.row.code === "BROKER_COMPILE_FAILURE") && typeof failHit.row.detail === "string" && failHit.row.detail !== "") {
              cacheHits.push(s.id);
              return { ...row, status: "fail", code: failHit.row.code, cached: true, failureCached: true, detail: `cached failure ${String(failHit.at ?? "").slice(0, 10) || "earlier"} (same inputs, toolchain and broker runtime): ${failHit.row.detail}`, ...(failHit.row.authenticatedFailure ? { authenticatedFailure: failHit.row.authenticatedFailure } : {}), ...(failHit.row.jsonSummary ? { jsonSummary: failHit.row.jsonSummary } : {}) };
            }
          }
          // Adrian 2026-11-06: the cache is looked up for every planned row BEFORE anything runs (options.cacheLookupOnly): a miss is reported with its cause, never run.
          // A probe (runGroup's per-crate lookup) answers like a lookup.
          if (options.cacheLookupOnly || probeScans.has(s)) return { ...row, status: "not-run", lookupMiss: Boolean(cacheKey), uncacheable: !cacheKey, detail: cacheKey ? "would miss the cache" : "has no cache key" };
          // Every cargo call the wrapper decides during this scan is filed under this token in the gate's ledger: the scan's own call AND the
          // calls the scan itself makes before it (the cargo-deny row's prefetch and its full metadata), so none is filed "unscoped" and fails the run.
          const ledgerToken = cargoLedger ? scanTokenOf(s.id) : null;
          if (ledgerToken) claimedTokens.add(ledgerToken);
          const ledgerEnv = ledgerToken ? { [SCAN_TOKEN_ENV]: ledgerToken } : {};
          // A Class A Cargo scan that runs here, not through the broker, never writes into the tree it reads: cargo puts target/.rustc_info.json
          // beside the manifest, and the snapshot must stay exactly the commit's tree after class A. It gets an output directory of its own
          // outside every root, removed when the scan ends (scanRow). Left alone: a scan that names its own CARGO_TARGET_DIR (a compile's
          // checkout-pinned output), a wrapped snapshot compile (the wrapper binds every cargo call; the sentinel asserts none escapes), a
          // carried read (the broker owns the output and a carried child gets no CARGO_TARGET_DIR) and the broker client itself.
          let readTargetEnv = {};
          if (s.class === SCAN_CLASS.A && s.tool === "cargo" && !cargoReadsAreCarried && !cargoLedger && !options.cargoSentinel && !Object.hasOwn(s.env ?? {}, "CARGO_TARGET_DIR")) {
            try { owned.cargoReadTarget = cargoReadTargetDir({ tmpRoot: options.cargoReadTargetRoot ?? tmpRoot, roots: [contentRoot, liveRoot, cargoLiveRoot] }); }
            catch (e) { return { ...row, status: "fail", code: "CARGO_READ_TARGET", detail: `${s.id}: ${e.message}` }; }
            readTargetEnv = { CARGO_TARGET_DIR: owned.cargoReadTarget };
          }
          let advisory = null; let args = s.args;
          if (s.advisoryDb) {
            try { advisory = prepareAdvisory(s); } catch (e) { return { ...row, status: "fail", code: "ADVISORY_DB", detail: e.message }; }
            // cargo-deny --frozen (--offline + --locked: the nested `cargo fetch` it runs may not rewrite Cargo.lock, so the broker admits it as analysis) must not fetch the advisory database, but its own `cargo metadata` would then be offline too and
            // fail on a runner whose cargo cache is cold. The workspace's metadata is resolved first, online and --locked (the
            // lockfile decides every version), and handed to cargo-deny as a file.
            if (s.advisoryDb === "deny") {
              const at = args.indexOf("deny");
              // The whole document is needed (the concurrent spawn keeps only the tail of stdout), so it streams to the file; a
              // spawn that does not know `stdoutFile` returns stdout, which is written below.
              // cargo-deny --frozen reads crate sources only from the local cargo cache, and `cargo metadata` does not download them
              // (fullA-19: rightkit-qa 0.9.1 was missing). They are fetched first, online and --locked (bound to the lockfile, so the
              // broker admits it as analysis). A fetch that fails, is refused or times out fails the row before metadata or deny run:
              // an offline or unreachable registry is red, never green.
              const fetchLimit = scanTimeoutMs(s, options);
              const fetchTimer = new AbortController();
              const fetchClock = setTimeout(() => fetchTimer.abort(), fetchLimit);
              let fetched;
              try { fetched = await deps.spawn(p.command, [...args.slice(0, at), "fetch", "--locked"], { cwd: path.join(rootDir(s.root), s.cwd ?? ""), env: { ...deps.env, ...readTargetEnv, ...s.env, ...ledgerEnv }, signal: signal ? AbortSignal.any([signal, fetchTimer.signal]) : fetchTimer.signal, timeout: fetchLimit }); } finally { clearTimeout(fetchClock); }
              if (!fetched || fetched.error || fetched.status !== 0) {
                advisory.cleanup?.();
                if (signal?.aborted) return { ...row, status: "not-run", code: "CANCELLED", detail: "cancelled while running" };
                if (fetchTimer.signal.aborted || fetched?.error?.code === "ETIMEDOUT") return { ...row, status: "fail", code: "TIMEOUT", detail: `cargo fetch --locked for cargo-deny ran longer than ${Math.round(fetchLimit / 60000)} min and was killed with its process tree` };
                return { ...row, status: "fail", code: "FETCH", detail: `cargo fetch --locked for cargo-deny failed (crate sources must be local before deny): ${fetched?.error?.message ?? (tail(fetched) || `exit ${fetched?.status}`)}` };
              }
              const metadataDir = fs.mkdtempSync(path.join(tmpRoot, "rk-deny-metadata-"));
              const metadataFile = path.join(metadataDir, "metadata.json");
              const metadata = await deps.spawn(p.command, [...args.slice(0, at), ...metadataArgs({ noDeps: false })], { cwd: path.join(rootDir(s.root), s.cwd ?? ""), env: { ...deps.env, ...readTargetEnv, ...s.env, ...ledgerEnv }, stdoutFile: metadataFile, ...sig });
              // AUX_METADATA: the auxiliary metadata that feeds deny could not be produced; not a deny finding, so never a deterministic red (rightkit#1257).
              if (!metadata || metadata.error || metadata.status !== 0) { fs.rmSync(metadataDir, { recursive: true, force: true }); advisory.cleanup?.(); return { ...row, status: "fail", code: "AUX_METADATA", detail: `cargo metadata for cargo-deny failed: ${metadata?.error?.message ?? (tail(metadata) || `exit ${metadata?.status}`)}` }; }
              if (!fs.existsSync(metadataFile)) fs.writeFileSync(metadataFile, String(metadata.stdout ?? ""));
              advisory.metadataPath = metadataFile;
              const overlayCleanup = advisory.cleanup;
              advisory.cleanup = () => { fs.rmSync(metadataDir, { recursive: true, force: true }); overlayCleanup?.(); };
            }
            args = args.map((a) => (a === "{advisoryDb}" ? advisory.dbDir : a === "{advisoryDenyConfig}" ? advisory.configPath : a === "{advisoryMetadata}" ? advisory.metadataPath : a));
          }
          let r;
          // actionlint 1.7.12 writes each run: script to shellcheck's stdin before starting it (rhysd/actionlint#702): a script over the
          // pipe buffer (~4 KB on Windows) deadlocks. Oversized scripts are refused here, before actionlint starts, so it cannot hang.
          if (s.runScriptLimit) {
            const over = s.runScriptLimit.files.flatMap((file) => { try { return oversizedRunScripts(ctx.read(file), file, s.runScriptLimit.bytes); } catch { return [`${file}: unreadable`]; } });
            if (over.length) return { ...row, status: "fail", code: "RUN_SCRIPT_TOO_LARGE", detail: `run: scripts over ${s.runScriptLimit.bytes} bytes (actionlint 1.7.12 deadlocks on them, rhysd/actionlint#702; split the step): ${over.join("; ")}` };
          }
          // Every scan has a time limit (fails closed, names the scan, kills the process tree): a hung tool is a check that never
          // fails, i.e. a run that never ends (actionlint on the Dell, 2026-10-27).
          // A tsc row that names TSC_PROJECT checks its package against the committed sources of its workspace siblings: the project is
          // generated outside the checked tree and removed after the run, so the check reads nothing a commit lacks and writes nothing.
          let tscProject = null;
          if (args.includes(TSC_PROJECT)) {
            // The siblings are mapped in the same tree tsc runs in (a publication scans a content checkout and a live checkout of
            // one verified commit; mixing them puts the package outside the project's rootDir, TS6059).
            try { tscProject = writeTscProject({ contentRoot: rootDir(s.root), packageDir: path.join(rootDir(s.root), s.cwd ?? ""), workspace: s.tscWorkspace ?? [] }); }
            catch (e) { advisory?.cleanup?.(); return { ...row, status: "fail", code: "TSC_PROJECT", detail: `${s.id}: the check project could not be built from the committed workspace: ${e.message}` }; }
            args = args.map((a) => (a === TSC_PROJECT ? tscProject.file : a));
          }
          const limitMs = scanTimeoutMs(s, options);
          const timer = new AbortController();
          // The scan's run limit is a stopwatch. A scan the broker carries (class B, not a quiet suite under its lease) is watched: the stopwatch does not tick while
          // every request of the scan is QUEUED at the broker (broker-admission.mjs), so queue time is neither charged to the scan nor bounded by a gate allowance.
          // Without a live reader (no client to ask) the stopwatch simply runs, and every unwatched carried scan gives its client the scan's own limit as the operation deadline
          // (RUSTRIGHT_OPERATION_DEADLINE_MS: the snapshot carrier, a rustright or cargo call, a carried Swift call through deadlineMs), so the client cancels by id before the gate stops it.
          const watched = carriedScan && s.class === SCAN_CLASS.B && s.quiet !== true && brokerLive !== null;
          let admission = null; let graceTimer = null; let genRefDir = null; let clockExpired = false;
          // A carried scan is cancelled by id first and its process tree stopped only after a grace; any other scan is stopped at once.
          const stopScan = () => {
            clockExpired = true;
            if (admission) { admission.cancelKnown().catch(() => {}); graceTimer = setTimeout(() => timer.abort(), options.brokerCancelGraceMs ?? CARRIED_CANCEL_GRACE_MS); } else timer.abort();
          };
          // A watched scan also has ONE wall-clock ceiling, its own limit x QUEUED_CEILING_FACTOR: a safety stop for a wedged broker queue (the broker's job deadlines are off by default), not a count.
          const runClock = createRunClock({ limitMs, onExpire: stopScan, ...(watched ? { ceilingMs: limitMs * QUEUED_CEILING_FACTOR, onCeiling: stopScan } : {}) });
          const stopClock = () => { runClock.stop(); admission?.stop(); if (graceTimer) clearTimeout(graceTimer); if (genRefDir) { fs.rmSync(genRefDir, { recursive: true, force: true }); genRefDir = null; } };
          const scanSignal = signal ? AbortSignal.any([signal, timer.signal]) : timer.signal;
          // A class A test row runs under the launch guard (class-a-launch-guard.mjs): its Node processes may not launch a compile.
          // Every class A row that executes repository code (tests, lint/typecheck with repository config, generators) runs under the
          // launch guard: class A runs before any compile, so none of them may start one (joint plan step 4; inventory gap 3).
          const guardEnv = s.class === SCAN_CLASS.A && (s.kind === "test" || s.executes === true) ? launchGuardEnv({ ...deps.env, ...s.env, ...(extraEnv ?? {}) }) : {};
          // The broker snapshot carrier (joint plan step 4d): this one cargo request names the commit, the snapshot, the ownership
          // handle and its own argv and working directory, under a fresh nonce; the ownership records the submit first.
          const finalArgs = args.map((a) => (a === "{contentRoot}" ? contentRoot : a));
          // SwiftPM writes its `.build/` inside the package by default: that must never land in the commit snapshot or the checkout, so the
          // build directory is the gate's own, keyed by the repository and the scan so it stays warm.
          // GB-67: on a broker host the scan is the carried `rightkit-swift` adapter job and the broker owns SwiftPM's scratch (it appends the one managed path and the
          // product location is `build --show-bin-path`): the gate sends no scratch or build path, and never adds one. Only a host with no broker (Linux direct) keeps its own.
          const swiftBrokered = carriedScan && s.tool === "swift" && Boolean(deps.snapshotBroker);
          // The adapter takes exactly one relative --package-path (adapter.rs check_swift_package); the scan runs in its package directory, so that is `.`.
          if (swiftBrokered && finalArgs[0] === "build" && !finalArgs.some((a) => a === "--package-path" || a.startsWith("--package-path="))) finalArgs.splice(1, 0, "--package-path", ".");
          if (s.tool === "swift" && finalArgs[0] === "build" && !swiftBrokered && !finalArgs.some((a) => a === "--scratch-path" || a === "--build-path" || a.startsWith("--scratch-path=") || a.startsWith("--build-path="))) {
            try { finalArgs.push("--scratch-path", swiftScratchPath({ tmpRoot, roots: [contentRoot, liveRoot, cargoLiveRoot], id: s.id })); }
            catch (e) { stopClock(); advisory?.cleanup?.(); return { ...row, status: "fail", code: "SWIFT_SCRATCH", detail: `${s.id}: ${e.message}` }; }
          }
          let carrier = null;
          if (viaSnapshotBroker) {
            const sb = deps.snapshotBroker;
            const cwd = path.join(rootDir(s.root), s.cwd ?? "");
            const refDir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-snapreq-"));
            try {
              const cargoArgv = s.tool === "rustright" ? finalArgs.slice(1) : finalArgs;
              const made = writeSnapshotRequest({ controlRoot: sb.controlRoot, commit: sb.commit, snapshot: sb.snapshot, declaredInputs: sb.declaredInputs ?? [], handle: sb.handle, primaryCwd: fs.realpathSync.native(cwd), argv: cargoArgv, requestIdFile: path.join(refDir, "request.json") });
              carrier = { made, refDir, cargoArgv };
            } catch (e) { fs.rmSync(refDir, { recursive: true, force: true }); stopClock(); advisory?.cleanup?.(); return { ...row, status: "fail", code: "SNAPSHOT_REQUEST", detail: `the snapshot request for ${s.id} could not be written: ${e.message}` }; }
          }
          // From here to the end of its verification the carrier and the request reference are this request's: every exit (cancel,
          // timeout, a thrown spawn, a refused receipt) removes them.
          // A wrapped snapshot compile with several Cargo workspaces (options.cargoSentinel, item 7): a scan whose own env binds no
          // CARGO_TARGET_DIR gets an empty directory of its own as one. Every cargo reached through the wrapper is bound elsewhere; only
          // a cargo that bypassed it (an absolute path, `rustup run`, a reset PATH) can write here, and the scan then fails below.
          let sentinel = null;
          // (A scan that is itself the carried broker request is bound by the broker: no sentinel, so the client's observed environment is the scan's own.)
          if (options.cargoSentinel && !viaSnapshotBroker && !Object.hasOwn(s.env ?? {}, "CARGO_TARGET_DIR")) {
            sentinel = path.join(options.cargoSentinel, sha256(s.id).slice(0, 16));
            fs.rmSync(sentinel, { recursive: true, force: true });
            fs.mkdirSync(sentinel, { recursive: true });
          }
          const sentinelEnv = sentinel ? { CARGO_TARGET_DIR: sentinel, CARGO_BUILD_TARGET_DIR: sentinel } : {};
          // A Rust test scan's JSON listing (every compiler-artifact record) streams whole to a file of its own: the kept tail of a spawn would lose the early artifact records of a noisy compile.
          let listingDir = null; let listingFile = null; let listingText = ""; let listingCut = false;
          let testOutputs = null;
          if (s.runOutside || s.captureJson) { listingDir =fs.mkdtempSync(path.join(tmpRoot, "rk-testlist-")); listingFile = path.join(listingDir, "listing.jsonl"); }
          try {
          // The ownership records this request's sub-handle immediately before it is sent (the reclaim proof reads broker.submitted): a
          // missing hook or a mark that did not land sends nothing.
          if (carrier) {
            const sub = carrier.made.carrier.ownership.handle;
            let marked = null; let why = null;
            try { if (typeof deps.snapshotBroker.markBrokerSubmit !== "function") throw new Error("no build-ownership submit hook"); marked = deps.snapshotBroker.markBrokerSubmit(sub, s.id); } catch (e) { why = e.message; }
            if (marked?.broker?.submitted !== true || marked.handle !== deps.snapshotBroker.handle || !marked.broker.handles?.includes(sub)) {
              stopClock(); advisory?.cleanup?.();
              return { ...row, status: "fail", code: "SNAPSHOT_REQUEST", detail: `the snapshot request for ${s.id} was not sent: the build ownership did not record the broker submit under ${deps.snapshotBroker.handle}${why ? ` (${why})` : ""}` };
            }
          }
          if (swiftBrokered) {
            // One signed, ledgered adapter request in the commit's snapshot (the ownership records its sub-handle before it is sent); a refused or unverified call fails the scan.
            // The carry is asynchronous (the gate's other scans keep running while this one queues or builds) and takes the scan's own signal: the run limit, or a cancel, stops it.
            const swiftRefs = [];
            const swiftBudget = carriedCallBudget({ env: { ...deps.env, ...s.env }, limitMs, watched });
            if (watched) { admission = watchBrokerAdmission({ clock: runClock, refFiles: () => swiftRefs, live: brokerLive, poller: statusPoller }); admission.start(); }
            try {
              const carried = await carryBrokerAdapterAsync({ adapter: "rightkit-swift", words: finalArgs, cwd: path.join(rootDir(s.root), s.cwd ?? ""), env: { ...deps.env, ...s.env, ...(extraEnv ?? {}), ...swiftBudget.env }, ...(swiftBudget.deadlineMs !== undefined ? { deadlineMs: swiftBudget.deadlineMs } : {}), section: brokerSection(deps.snapshotBroker), record: (entry) => { if (typeof entry.requestRef === "string" && !swiftRefs.includes(entry.requestRef)) swiftRefs.push(entry.requestRef); if (cargoLedger && ledgerToken) appendLedger(cargoLedger.dir, ledgerToken, entry); }, err: () => {}, stdio: "capture", signal: scanSignal, spawn: deps.adapterSpawn ?? defaultSpawnAsync });
              r = { status: carried.status, stdout: carried.stdout, stderr: carried.stderr };
            } catch (e) { r = { error: e }; } finally { stopClock(); advisory?.cleanup?.(); }
          } else {
          // The executables this scan runs are the ones resolved and verified above: read their bytes again now, immediately before the launch
          // (a replaced binary is TOOL_REPLACED), and hand the scan only those paths: actionlint its shellcheck, cargo its cargo-deny / cargo-audit.
          for (const identity of identities) {
            const swapped = trustedScanToolProblem(identity, { ops: options.toolOps });
            if (swapped) { stopClock(); advisory?.cleanup?.(); return { ...row, status: "fail", code: swapped.code, detail: `${swapped.code}: ${swapped.detail}; the scan did not run` }; }
          }
          const shellcheckIdentity = s.tool === "actionlint" ? identities.find((i) => i.tool === "shellcheck") : null;
          const spawnArgs = shellcheckIdentity ? ["-shellcheck", shellcheckIdentity.path, ...finalArgs] : finalArgs;
          const trustedDirs = trusts.map((tool) => identities.find((i) => i.tool === tool)).filter(Boolean).map((i) => path.dirname(i.path));
          const pathKey = Object.keys({ ...deps.env, ...s.env }).find((k) => k.toLowerCase() === "path") ?? "PATH";
          // The gate's cargo wrapper stays first on PATH, ahead of the trusted directories: an enrolled analyzer's directory may also hold
          // a real cargo (cargo-deny in ~/.cargo/bin beside rustup's cargo), which would otherwise answer the scan's `cargo` unbound and,
          // with several workspaces, write into the sentinel (rightkit#1327).
          const scanPath = ({ ...deps.env, ...s.env })[pathKey];
          const pathSep = deps.platform === "win32" ? ";" : ":";
          const scanCargo = ({ ...deps.env, ...s.env }).CARGO;
          const wrapperBin = (cargoLedger || options.cargoSentinel) && typeof scanCargo === "string" && typeof scanPath === "string" ? (deps.platform === "win32" ? path.win32 : path.posix).dirname(scanCargo) : null;
          const wrapperFirst = wrapperBin !== null && scanPath.split(pathSep)[0] === wrapperBin;
          const trustEnv = trustedDirs.length ? { [pathKey]: (wrapperFirst ? [wrapperBin, ...trustedDirs, scanPath.slice(wrapperBin.length + 1)] : [...trustedDirs, scanPath]).filter(Boolean).join(pathSep) } : {};
          // A watched scan names where its client writes the request id (RUSTRIGHT_REQUEST_ID_FILE) so the gate can ask the broker for it: the carrier's own reference, a file of its own for
          // a scan that reaches the broker through `rustright cargo`, or the reference files its carried calls record in the cargo ledger (an npm suite with a broker call).
          let refEnv = {};
          if (watched) {
            let refFiles;
            if (carrier) refFiles = () => [path.join(carrier.refDir, "request.json")];
            else if (s.tool === "rustright" || s.tool === "cargo") { genRefDir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-scanreq-")); const file = path.join(genRefDir, "request.json"); refEnv = { RUSTRIGHT_REQUEST_ID_FILE: file }; refFiles = () => [file]; }
            else refFiles = () => { try { return ledgerToken && cargoLedger ? readLedger(cargoLedger.dir, ledgerToken).entries.filter((e) => e.status === "pending" && typeof e.requestRef === "string").map((e) => e.requestRef) : []; } catch { return []; } };
            admission = watchBrokerAdmission({ clock: runClock, refFiles, live: brokerLive, poller: statusPoller });
            admission.start();
          }
          try { r = await deps.spawn(p.command, spawnArgs, { cwd: path.join(rootDir(s.root), s.cwd ?? ""), env: { ...deps.env, ...sentinelEnv, ...readTargetEnv, ...s.env, ...(extraEnv ?? {}), ...guardEnv, ...ledgerEnv, ...(carrier ? carrier.made.env : {}), ...refEnv, ...(carriedScan && !watched ? scanOperationEnv({ ...deps.env, ...s.env }, limitMs) : {}), ...trustEnv }, signal: scanSignal, ...(listingFile ? { stdoutFile: listingFile } : {}), ...(watched ? {} : { timeout: limitMs }) }); } finally { stopClock(); advisory?.cleanup?.(); }
          if ((s.runOutside || s.captureJson) && r && !r.error) {
            // The whole listing: the file the spawn streamed, else (a spawn that does not know stdoutFile) its stdout. What a reader sees of a failed compile is the rendered errors.
            let fromFile = false;
            try { if (fs.existsSync(listingFile) && fs.statSync(listingFile).size > 0) { listingText = fs.readFileSync(listingFile, "utf8"); fromFile = true; } } catch { /* falls back to stdout */ }
            if (!fromFile) { listingText = String(r.stdout ?? ""); listingCut = r.truncated === true; }
            // A grouped doc invocation's per-crate verdicts come from this stream (NN40); a cut stream proves nothing per crate, so it is not summarized.
            if (s.captureJson && !listingCut) row.jsonSummary = summarizeCargoJson(listingText);
            r = { ...r, stdout: listingFailureText(listingText) };
          }
          }
          if (signal?.aborted && (!r || r.error || r.status !== 0)) return { ...row, status: "not-run", code: "CANCELLED", detail: "cancelled while running" };
          if ((clockExpired || timer.signal.aborted || r?.error?.code === "ETIMEDOUT") && (!r || r.error || r.status !== 0)) {
            const how = `was ${admission ? "cancelled at the broker by id and stopped" : "killed"} with its process tree; a scan that does not finish fails`;
            if (runClock.ceilingHit) return { ...row, status: "fail", code: "TIMEOUT", detail: `${s.id} hit its wall-clock ceiling while it waited in the broker's queue (${spanText(limitMs * QUEUED_CEILING_FACTOR)}: ${QUEUED_CEILING_FACTOR} x its ${spanText(limitMs)} run limit, a safety stop for a wedged broker queue, not a count; it had waited ${spanText(runClock.queuedMs)} in the queue) and ${how}` };
            return { ...row, status: "fail", code: "TIMEOUT", detail: `${s.id} ran longer than its ${spanText(limitMs)} run limit${runClock.queuedMs > 0 ? ` (the ${spanText(runClock.queuedMs)} it waited in the broker's queue does not count)` : ""} and ${how}` };
          }
          if (!r || r.error) { status = "fail"; code = "SPAWN_ERROR"; detail = r?.error?.message ?? "no result"; }
          else if (r.status !== 0) { status = "fail"; code = "EXIT"; detail = tail(r) || `exit ${r.status}`; }
          // cargo-deny reports some failures only as an [ERROR] line and still exits 0 (under the broker, its nested offline
          // `cargo fetch` of crate sources is refused: "failed to fetch crates"). A deny row passes only with no [ERROR] line in
          // its complete output; output cut to the kept tail cannot prove that, so it fails too (RR, reviewer 2026-10-28).
          if (sentinel) {
            let written = [];
            try { written = fs.readdirSync(sentinel); } catch (e) { written = [`(unreadable: ${e.message})`]; }
            if (written.length) { status = "fail"; code = "RK_CARGO_UNBOUND"; detail = `${s.id}: a cargo that bypassed the gate's wrapper wrote ${written.slice(0, 5).join(", ")} into the unbound sentinel ${sentinel}; with several Cargo workspaces no output directory can be inferred for it, so the scan proves nothing about where its outputs went`; }
          }
          // The wrapper's ledger (rows 7.10 and 7.24): a refused, unverified or unfinished cargo call fails this scan even when the code
          // under test ignored cargo's exit status; on a broker host each call's signed receipt is verified again here, by the gate.
          if (ledgerToken) {
            let problems;
            try {
              const ledger = readLedger(cargoLedger.dir, ledgerToken);
              problems = [...ledger.problems, ...(deps.snapshotBroker ? await verifyBrokerLedger(ledger.entries, deps.snapshotBroker, { seen: ledgerSeen }) : [])];
            } catch (e) { problems = [`the cargo ledger cannot be read (${e.message})`]; }
            if (problems.length) {
              ledgerProblem = true;
              const text = `${s.id}: the gate's cargo ledger records ${problems.length} cargo call(s) that did not complete as a verified success: ${problems.slice(0, 5).join("; ")}`;
              if (status === "pass") { status = "fail"; code = "RK_CARGO_LEDGER"; detail = text; } else detail = `${detail ? `${detail}; ` : ""}${text}`;
            }
          }
          if (s.advisoryDb === "deny" && code !== "RK_CARGO_UNBOUND") {
            const denyError = `${r.stdout ?? ""}\n${r.stderr ?? ""}`.split(/\r?\n/).find((line) => /\[ERROR\]/.test(line));
            if (r.truncated) { status = "fail"; code = "DENY_ERROR"; detail = "cargo-deny output exceeded the kept tail, so the absence of an [ERROR] line cannot be shown"; }
            else if (denyError) { status = "fail"; code = "DENY_ERROR"; detail = `cargo-deny exited 0 but reported an error: ${denyError.trim().slice(0, 300)}`; }
          }
          // The broker's signed account of that request: the receipt verifies against this host's pins and its snapshotEvidence names
          // this commit, this snapshot unchanged from START to exit, this ownership and the output directory. Anything less fails the row.
          if (carrier) {
            // A typed broker refusal with no request reference (admission paused, a cutover drain): nothing was admitted under this scan's sub-handle, so it is
            // recorded unsent at once, as a busy connection is; otherwise a dead owner's record could never be reclaimed (HeardRight Mac, 2026-11-02).
            if (!fs.existsSync(path.join(carrier.refDir, "request.json"))) {
              const refused = refusalInCapturedOutput(`${r?.stdout ?? ""}\n${r?.stderr ?? ""}`);
              if (refused && typeof deps.snapshotBroker?.markBrokerUnsent === "function") {
                try { deps.snapshotBroker.markBrokerUnsent(carrier.made.carrier.ownership.handle, `the broker refused the submit before admitting it (${refused.code}): nothing was spawned`); } catch { /* the reclaim then judges the sub-handle as before */ }
              }
            }
            try {
              const ref = readRequestRef(path.join(carrier.refDir, "request.json"));
              const receipt = await deps.snapshotBroker.receiptOf(ref.requestId);
              const receiptOptions = { pins: deps.snapshotBroker.pins, jobId: ref.requestId, carrier: carrier.made.carrier, argv: carrier.cargoArgv, workspace: ref.workspace, observedEnv: observedEnvOf({ ...deps.env, ...s.env, ...(extraEnv ?? {}) }, deps.platform ?? process.platform), checkout: deps.snapshotBroker.checkout, outputRoots: deps.snapshotBroker.outputRoots, platform: deps.snapshotBroker.platform ?? deps.platform ?? process.platform };
              let verified;
              try { verified = verifySnapshotReceipt(receipt, receiptOptions); }
              catch (original) {
                if (original?.code !== "result" || status !== "fail" || code !== "EXIT" || signal?.aborted || !Number.isSafeInteger(r?.status) || r.status <= 0) throw original;
                verified = verifySnapshotReceipt(receipt, { ...receiptOptions, allowFailedCompile: true });
                if (verified.signedResult?.status !== "FAILED" || verified.signedResult.failureClass !== "compile" || verified.signedResult.exitCode !== r.status || verified.signedResult.signal !== null) throw original;
                code = "BROKER_COMPILE_FAILURE";
                // Bisection needs the fully verified, signed post-run verdict as well as the matching client exit.
                row.authenticatedFailure = { status: verified.signedResult.status, failureClass: verified.signedResult.failureClass, exitCode: verified.signedResult.exitCode, postChecks: verified.signedResult.postChecks, retryable: verified.signedResult.postChecks?.verified === true };
              }
              const { evidence } = verified;
              row.snapshotEvidence = { receiptSha256: sha256(canonicalJson(receipt)), requestId: ref.requestId, outputDir: evidence.outputDir, cargoDir: s.cwd ?? ".", targetTriple: typeof receipt?.targetTriple === "string" && /^[A-Za-z0-9_.]+(?:-[A-Za-z0-9_.]+){1,4}$/.test(receipt.targetTriple) ? receipt.targetTriple : null, treeAtStart: evidence.snapshot.treeAtStart, treeAtExit: evidence.snapshot.treeAtExit, ...(receipt && Object.hasOwn(receipt, "testExecutables") ? { testExecutables: receipt.testExecutables, testExecutablesReason: receipt.testExecutablesReason ?? null } : {}) };
            } catch (e) {
              status = "fail"; code = "SNAPSHOT_EVIDENCE"; detail = `${s.id}: the broker's snapshot evidence did not verify (${e.code ?? "error"}: ${e.message})`;
            }
          }
          } finally {
            if (carrier) { removeCarrier(carrier.made.file); fs.rmSync(carrier.refDir, { recursive: true, force: true }); }
            if (tscProject) fs.rmSync(tscProject.dir, { recursive: true, force: true });
            if (listingDir) fs.rmSync(listingDir, { recursive: true, force: true });
          }
          // GATE-CONTRACT GC-A08: the compile above never ran a test. The gate runs the listed test executables here, outside the broker.
          // Quiet lane: a quiet suite keeps the broker's quiet lease (taken by runScans around this whole scan) through this run phase. The lease exists so nothing else
          // runs beside a conflict-prone suite, and the suite's tests now execute here, so releasing it after the compile would defeat it; the run phase is bounded
          // to what is left of QUIET_SCAN_MAX_MS so compile + run stay under the broker's lease ceiling.
          if (s.runOutside && status === "pass" && r && !r.error) {
            const fail = (c, d) => { status = "fail"; code = c; detail = d; };
            const listing = parseTestListing(listingText);
            const platform = deps.snapshotBroker?.platform ?? deps.platform ?? process.platform;
            if (listingCut) fail("TEST_LISTING", `${s.id}: the compile listing was cut at the kept output tail, so the test executables cannot be enumerated`);
            else if (listing.buildFinished !== true) fail("TEST_LISTING", `${s.id}: the compile exited 0 but its listing has no successful build-finished record (${listing.buildFinished === false ? "a build-finished record said failure" : "none found"})`);
            else if (listing.binaries.length === 0) {
              // End-to-end tests only (Adrian, 2026-11-03): a crate whose test build lists no test executable has no test to run; the row passes
              // with zero libtest counts, the same as a test build that runs none (testResultsProblem).
              row.results = { runner: "libtest", tests: 0, pass: 0, fail: 0, skipped: 0, todo: 0, binaries: 0 };
              detail = `${s.id}: no test executable (${listing.artifacts} artifact record(s)); end-to-end tests only, nothing to run`;
            } else {
              let boundDirs; let generationRoots = null;
              if (viaSnapshotBroker) { boundDirs = row.snapshotEvidence?.outputDir ? [row.snapshotEvidence.outputDir] : []; generationRoots = deps.snapshotBroker.outputRoots ?? []; }
              else {
                const scanDir = path.join(rootDir(s.root), s.cwd ?? "");
                const target = s.env?.CARGO_TARGET_DIR ?? deps.env?.CARGO_TARGET_DIR ?? "target";
                boundDirs = [path.resolve(scanDir, target)];
              }
              const ops = { ...TEST_RUN_OPS, ...(options.testRunOps ?? {}) };
              const contained = containTestExecutables(listing.binaries, { boundDirs, generationRoots, platform, ops });
              // ADR-001 C2 (rightkit#1454): a broker that signs the compile's test executables (receipt.testExecutables, rustright) binds each one;
              // an executable whose path or sha256 is not in that signed list never runs. An older broker that signs no list leaves containment only.
              const signedProblems = viaSnapshotBroker && row.snapshotEvidence && Object.hasOwn(row.snapshotEvidence, "testExecutables") ? signedTestExecutableProblems(contained.accepted, row.snapshotEvidence.testExecutables, row.snapshotEvidence.testExecutablesReason, platform) : [];
              if (s.publisherRustTest && viaSnapshotBroker && !Object.hasOwn(row.snapshotEvidence ?? {}, "testExecutables")) fail("TEST_EXECUTABLE_UNSIGNED", `${s.id}: the broker did not sign a test executable list`);
              else if (contained.problems.length) fail("TEST_EXECUTABLE_OUTSIDE_OUTPUT", `${s.id}: ${contained.problems.slice(0, 5).join("; ")}; the row fails and nothing was run`);
              else if (signedProblems.length) fail("TEST_EXECUTABLE_UNSIGNED", `${s.id}: ${signedProblems.slice(0, 5).join("; ")}; the row fails and nothing was run`);
              else if (Array.isArray(s.testGroup)) {
                // NN17/NN39: the group's members run their own executables in parallel (a pool the size of the test concurrency), each with its own verdict.
                const byCrate = new Map();
                for (const b of contained.accepted) { if (!byCrate.has(b.package)) byCrate.set(b.package, []); byCrate.get(b.package).push(b); }
                const memberRuns = {}; const list = s.testGroup; let next = 0;
                const worker = async () => {
                  while (next < list.length) {
                    const m = list[next++]; const bins = byCrate.get(m.crate) ?? [];
                    if (!bins.length) { memberRuns[m.id] = { status: "pass", results: { runner: "libtest", tests: 0, pass: 0, fail: 0, skipped: 0, todo: 0, binaries: 0 }, detail: `${m.id}: no test executable; end-to-end tests only, nothing to run` }; continue; }
                    const ran = await runTestBinaries({ scan: { ...s, id: m.id }, binaries: bins, spawn: deps.spawn, env: { ...deps.env, ...s.env, ...(extraEnv ?? {}) }, limitMs, signal, ops, concurrency: 1 });
                    memberRuns[m.id] = { status: ran.status, ...(ran.code ? { code: ran.code } : {}), ...(ran.detail ? { detail: ran.detail } : {}), results: ran.results, testRun: { executables: ran.executables, outputTruncated: ran.executables.some((e) => e.outputTruncated), skipMarkers: ran.executables.reduce((n, e) => n + (e.skipMarkers ?? 0), 0) } };
                  }
                };
                await Promise.all(Array.from({ length: Math.max(1, Math.min(testRunConcurrency(), list.length)) }, worker));
                row.memberRuns = memberRuns;
                const bad = list.filter((m) => memberRuns[m.id]?.status !== "pass");
                if (bad.length) { status = "fail"; code = "TEST_GROUP_MEMBER_FAILED"; detail = `${bad.length} of ${list.length} member(s) failed their tests: ${bad.slice(0, 8).map((m) => m.id).join(", ")}`; }
                else row.results = { runner: "libtest", tests: list.reduce((n, m) => n + (memberRuns[m.id].results?.tests ?? 0), 0), pass: list.reduce((n, m) => n + (memberRuns[m.id].results?.pass ?? 0), 0), fail: 0, skipped: list.reduce((n, m) => n + (memberRuns[m.id].results?.skipped ?? 0), 0), todo: 0 };
              }
              else {
                const budget = s.quiet === true ? Math.max(1, Math.min(limitMs, QUIET_SCAN_MAX_MS - (Date.now() - scanStartedAt))) : limitMs;
                const ran = await runTestBinaries({ scan: s, binaries: contained.accepted, spawn: deps.spawn, env: { ...deps.env, ...s.env, ...(extraEnv ?? {}) }, limitMs: budget, signal, ops, ...(s.publisherRustTest && s.runOutside.args.includes("--test-threads=1") ? { concurrency: 1 } : {}) });
                testOutputs = ran.outputs ?? null;
                row.results = ran.results; row.testRun = { executables: ran.executables, outputTruncated: ran.executables.some((e) => e.outputTruncated), skipMarkers: ran.executables.reduce((n, e) => n + (e.skipMarkers ?? 0), 0) };
                if (s.publisherRustTest && row.testRun.outputTruncated) fail("TEST_OUTPUT_TRUNCATED", `${s.id}: test output was truncated; skip evidence is incomplete`);
                if (ran.status !== "pass") { status = ran.status; code = ran.code; detail = ran.detail; }
              }
            }
          }
          if (s.coverageKey && s.publisherPhase === "test" && !s.runOutside) {
            const output = `${r?.stdout ?? ""}\n${r?.stderr ?? ""}`.replace(/\u001b\[[0-9;?]*[ -/]*[@-~]/g, "");
            row.testRun = { outputTruncated: r?.truncated === true, skipMarkers: output.split(/\r\n|\n|\r/).filter((line) => /(?:^|[\s\]:])SKIP(?=[\s:]|$)/.test(line)).length };
            if (status === "pass" && row.testRun.outputTruncated) { status = "fail"; code = "TEST_OUTPUT_TRUNCATED"; detail = `${s.id}: test output was truncated; skip evidence is incomplete`; }
          }
          if (s.publisherRustTest && status === "pass") {
            if (!s.runOutside) {
              const counts = parseLibtestSummary(`${r?.stdout ?? ""}\n${r?.stderr ?? ""}`);
              if (counts && counts.results > 0) row.results = { runner: "libtest", tests: counts.passed + counts.failed + counts.ignored, pass: counts.passed, fail: counts.failed, skipped: counts.ignored, todo: 0 };
              else { status = "fail"; code = "NO_TEST_SUMMARY"; detail = "the Rust test command printed no recognisable libtest summary"; }
            }
            if (status === "pass" && !s.allowIgnored && (row.results?.skipped ?? 0) > 0) { status = "fail"; code = "IGNORED_TESTS"; detail = `${row.results.skipped} ignored Rust test(s) require an approved reason`; }
          }
          if (status === "fail" && options.scanLogDir && r) {
            try { fs.mkdirSync(options.scanLogDir, { recursive: true }); const file = path.join(options.scanLogDir, `${s.id.replace(/[^A-Za-z0-9._-]+/g, "_")}.log`); fs.writeFileSync(file, `$ ${s.command ?? s.id}\nexit ${r.status}\n--- ${s.runOutside ? "cargo (compile) " : ""}stdout\n${r.stdout ?? ""}\n--- ${s.runOutside ? "cargo (compile) " : ""}stderr\n${r.stderr ?? ""}\n${s.runOutside ? (testOutputs?.length ? testOutputs.map((o) => `=== test executable ${o.label} (exit ${o.exit ?? "none"}${o.signal ? `, signal ${o.signal}` : ""}) ${s.runOutside.args.join(" ")}\n--- stdout\n${o.stdout}\n--- stderr\n${o.stderr}\n`).join("") : "=== test executables: none ran (the row failed before or without running them)\n") : ""}`); row.log = file; detail = `${detail} (full output: ${file})`; } catch { /* the tail in the row remains */ }
          }
          if (s.kind === "test" && r && !r.error && r.status === 0) {
            // What the suite executed and skipped is evidence, not decoration: a later stage reuses this row only with these counts.
            const counts = parseTestSummary(`${r.stdout ?? ""}\n${r.stderr ?? ""}`);
            if (counts) row.results = counts;
            else if (status === "pass") { status = "fail"; code = "NO_TEST_SUMMARY"; detail = "the test scan passed but printed no recognisable summary (node:test or vitest); its coverage cannot be recorded"; }
            else row.results = null;
            // One rule for fresh and cached rows (testResultsProblem): tests ran, none failed, at least one passed.
            const problem = status === "pass" && row.results ? testResultsProblem(row.results) : null;
            if (problem) { status = "fail"; code = problem.code; detail = problem.detail; }
          }
          // NN41: store a deterministic compile failure only: the broker's authenticated compile failure, or a plain exit of a compile with compiler error diagnostics and no sign of
          // the environment failing. Never a timeout, abort, SCAN_ERROR, refusal, unverified receipt, ledger problem, test run or infrastructure error (those return or carry another code).
          if (status === "fail" && failKey && !signal?.aborted && !ledgerProblem && typeof detail === "string" && detail !== "" && ((code === "BROKER_COMPILE_FAILURE" && row.authenticatedFailure?.failureClass === "compile") || (code === "EXIT" && !carriedScan && !carrier)) && isDeterministicCompileDiagnostics(`${r?.stdout ?? ""}\n${r?.stderr ?? ""}`)) {
            cache.write(failKey, { id: s.id, class: s.class, command: s.command, tool: s.tool ?? null, toolVersion: row.toolVersion, status: "fail", code, detail: detail.slice(0, 6000), failureIdentity: options.failureIdentity, ...(row.authenticatedFailure ? { authenticatedFailure: row.authenticatedFailure } : {}), ...(row.jsonSummary ? { jsonSummary: row.jsonSummary } : {}) });
          }
          if (status === "pass" && cacheKey && !signal?.aborted && row.rowIndex) cache.write(row.rowIndex.key, { id: s.id, inputs: row.rowIndex.inputs, env: row.rowIndex.env, raw: row.rowIndex.raw, at: new Date().toISOString() });
          if (status === "pass" && cacheKey && !signal?.aborted) cache.write(cacheKey, { id: s.id, class: s.class, command: s.command, tool: s.tool ?? null, toolVersion: row.toolVersion, status: "pass", ...(s.coverageKey ? { snapshotEvidence: row.snapshotEvidence ?? null, testRun: row.testRun ?? null } : {}), ...(row.results ? { results: row.results } : {}) });
        }
      }
    } catch (e) { status = "fail"; code = "SCAN_ERROR"; detail = e.message; }
    if (status === "fail" && s.class === SCAN_CLASS.A) classAFailed = true;
    return { ...row, status, ...(code ? { code } : {}), ...(detail ? { detail } : {}), ...(advice ? { advice } : {}) };
  }

  // Cargo calls the gate cannot attribute to a scan (the code under test cleared or replaced the scan token): never skipped.
  if (cargoLedger) {
    // The gate's own version probes: anything but a completed call (a refused probe) fails the run too.
    try {
      const probeProblems = readLedger(cargoLedger.dir, PROBE_LEDGER_TOKEN).problems;
      if (probeProblems.length) results.push({ id: "cargo-ledger:tool-probe", class: SCAN_CLASS.B, command: "cargo ledger", tool: null, toolVersion: null, status: "fail", code: "RK_CARGO_LEDGER", detail: `the gate's tool version probes reached the wrapper and did not complete: ${probeProblems.slice(0, 5).join("; ")}` });
    } catch { /* an unreadable probe ledger is named by the unclaimed check only when it holds records */ }
    let unclaimed;
    try { unclaimed = unclaimedLedgers(cargoLedger.dir, claimedTokens); } catch (e) { unclaimed = [`unreadable (${e.message})`]; }
    if (unclaimed.length) {
      const calls = [];
      for (const token of unclaimed) { try { for (const e of readLedger(cargoLedger.dir, token).entries) calls.push(`cargo ${(e.argv ?? []).join(" ")} (${e.status}, from ${e.cwd})`.trim()); } catch { /* named by its token below */ } }
      results.push({ id: "cargo-ledger", class: SCAN_CLASS.B, command: "cargo ledger", tool: null, toolVersion: null, status: "fail", code: "RK_CARGO_LEDGER", detail: `cargo calls were filed under ${unclaimed.length} scan token(s) no scan owns (${unclaimed.slice(0, 3).join(", ")}), so they cannot be tied to a scan (${SCAN_TOKEN_ENV} was cleared, or the call came from outside every scan): ${calls.slice(0, 5).join("; ") || "unreadable"}` });
    }
  }
  const cancelled = signal?.aborted === true;
  return {
    schema: SCAN_RESULT_SCHEMA, ok: !cancelled && results.every((r) => r.status === "pass"), complete: !cancelled && results.every((r) => r.status === "pass"), classes, checker, ...(cancelled ? { cancelled: true } : {}),
    inputDigest: plan.inputDigest ?? null,
    deferred: results.filter((r) => r.status === "deferred").map((r) => r.id),
    scans: results, toolVersions, toolIdentities, cacheHits: [...cacheHits].sort(), notes: [...(plan.notes ?? []), ...batchFailureNotes, ...(cargoLedger ? [UNOBSERVED_NESTED_CARGO_NOTE] : [])],
    scanSetDigest: digestScanSet(results),
    classDigests: { A: digestScanSet(results.filter((r) => r.class === "A")), B: digestScanSet(results.filter((r) => r.class === "B")) },
  };
}

/** One line per scan that failed or did not run, for a gate's failure list. A deferred scan is not a failure; see scanDeferrals. */
export function scanFailures(result) {
  return result.scans.filter((r) => r.status !== "pass" && r.status !== "deferred").map((r) => `${r.id}${r.status === "not-run" ? " (not run)" : ""}: ${r.detail ?? r.status}`);
}

/** One line per deferred scan, by name. A gate prints these; they are never reported as passed. */
export function scanDeferrals(result) {
  return result.scans.filter((r) => r.status === "deferred").map((r) => `${r.id}: deferred, not passed (${r.detail})`);
}
