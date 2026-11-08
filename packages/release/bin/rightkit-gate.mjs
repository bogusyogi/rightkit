#!/usr/bin/env node
// rightkit-gate: the local deterministic gate for any repository that uses RightKit. The gate is for building: commit and push
// run no gate and no local hook check (docs/GATE-CONTRACT.md), and `rightkit-gate install-hook` writes no git hook. Every
// applicable scan for the gated change runs through the one scan module (../deterministic-scans.mjs); Rust units come from `cargo metadata` (one run per Cargo workspace), npm units from
// the pnpm roots' workspace files, so the layout is the repository's own. CI still compiles, lints and tests everything.
//
// Layout it covers (nothing is partly covered silently):
//   - Rust: every tracked Cargo.toml that is a workspace root or a standalone package not inside another workspace is its own Cargo
//     workspace (`cargo metadata --no-deps --locked` there, through the broker). A unit of a workspace below the repository root is
//     named `<cargoDir>:<crate>`; its scans run with that directory as cwd, its reverse dependents and cache keys come from that
//     workspace's own Cargo.lock, and ci/gate.config.json `rust` may key its feature sets by `<cargoDir>:<crate>` or by the plain crate
//     name. One workspace and lock per crate (RightKit itself, rightkit#1285): a crate whose workspace is its own crates/<crate> directory keeps
//     its plain name and unsuffixed scan ids; a selected crate also selects every crate that path-depends on it, across workspaces, read from
//     the manifests (packages/release/crate-workspaces.mjs); a vendored source a crate patches in selects that crate. A tracked Cargo.toml that is not a member and cannot stand alone, a member that is untracked or ignored, and an untracked
//     Cargo.toml are skipped with a note on the first lines of the run, never silently.
//   - npm: every pnpm root (discoverPnpmRoots: the repository root, and any tracked directory with a pnpm-lock.yaml or pnpm-workspace.yaml
//     beside a package.json, outside node_modules) has its own units: the pnpm-workspace.yaml globs over tracked package.json files; when that
//     file is absent, has no `packages:` or declares ".", the root's package.json is a unit too. pnpm-lockfile and pnpm-audit run once per
//     root in its directory (`pnpm-lockfile:<root>` below the repository root), and each unit gets tsc, the linter it configures (eslint
//     or biome, by config file) and its tests like any package. A unit of a root below the repository root is named from its directory.
//
// Release evidence for a PRODUCT repository (an app): the pin's "a release qualified locally on both hosts" is the app's own
// `right-release` release QA run on both hosts (docs/CI.md "Release evidence for an app"). `right-release qualify cargo` qualifies
// CRATES for a registry upload (`.rightkit-gate.json` configures it for a repository other than RightKit); it is not an app's evidence.
//
// Content checks run on an export of HEAD (exactly what is pushed), never on uncommitted work in the checkout.
//
//   rightkit-gate [--base <ref>] [--plan]
//                                             run the gate (default base origin/main); --plan prints the scans and runs none.
//                                             There is no cache bypass (GC-T14; Adrian: no no-cache option): a result is reused only under a key
//                                             naming every input it read, so a changed input always reruns.
//   rightkit-gate --to check|compile|package|qualify|dev|test --route [--platform p] [--commit <rev>]
//                                             prints one JSON line (schema rightkit-gate.route/1) naming the route this host takes for the
//                                             stage and the refusal it would meet, and runs nothing: no ownership, snapshot, check, run record
//                                             or receipt (rightkit#1297; the release canary probes it for every app). Exit 0 answered, 2 not
//   rightkit-gate check [--repo <dir>] [--commit <rev> | --worktree] [--rule-tier compile|release] [--only <rule,...>] [--list] [--print-baseline] [--baseline <file>]
//                                             the deterministic stage: the app check (../app-check.mjs), then every class A scan
//                                             (lockfiles, cargo-deny/audit, secrets, licences, manifests, workflow lint, syntax) over the
//                                             whole tree of the commit, not a diff. Nothing compiles; without a broker class A uses
//                                             plain cargo for metadata/deny/audit only. The RustRight broker runs this before it admits
//                                             any compile. --list/--print-baseline/--only show the app check alone. Exit 0 pass,
//                                             1 findings, 2 could not run
//   rightkit-gate install-hook                retire the git hooks an earlier release wrote and add the agent hook: it removes
//                                             .githooks/pre-push, the .githooks/<name> forwarders and the advisory ownership pre-commit
//                                             hook, each only when its own header or marker says RightKit wrote it (a foreign hook is never
//                                             touched; pre-commit.rightkit-foreign goes back to pre-commit), unsets a local core.hooksPath
//                                             of `.githooks` once nothing foreign remains there, and registers the one-door agent hook in
//                                             .claude/settings.json. It writes no pre-push, pre-commit or other git hook
//                                             (`right-release hooks install` is an alias of this command; --force is ignored)
//
// Git ref lines on stdin (the form git hands a pre-push hook; nothing here installs one) are still read when given: the commit
// pushed must be the checked-out HEAD (broker scans read the live checkout), and the remote's current commit is the diff base.
// The executor is chosen by the gate, never the caller (gate-executor.mjs resolveExecutor): the RustRight broker where this machine
// has it (`rustright` on PATH, or its managed cargo shim), a refusal on a broker-managed machine whose broker is unreachable, and
// direct cargo on any other host (a cloud container, a CI runner). Every host runs the same gates in the same order (app rules, class A,
// the compile scans); an unmanaged host is never told to find a broker.
// ci/gate.config.json `rust` (optional) sets per-crate feature sets, exactly as CI's gate reads it; `npm.<unit>` sets `quietTests` and
// `requiresBroker: { "reason": "..." }` (a test the package scripts do not show reaching the broker: node -e, node scripts, sh -c, pnpm --dir).
// Result cache: a passing class B scan is reused only when its scan id, command, tool version, environment and input digest
// (the gated commit's git object ids for the crate's workspace closure, the shared build inputs, ci/gate.config.json and every
// path the closure's sources include from outside their crate; the whole HEAD tree for Rust tests unless ci/gate.config.json
// rust.<crate>.testInputs lists the paths they read, for npm, node and docs scans, and whenever a reference cannot be resolved
// statically) are all identical. Stored in <git common dir>/rightkit-gate-cache; a hit prints
// `cached` and is listed in the result's cacheHits. A deterministic failure (NN41: a compile's authenticated or diagnosed failure, a file-only rule's violations) is cached too and prints `cached-fail`; timeouts, aborts, refusals, infrastructure errors and test runs never are.
// RK_GATE_JOBS (default 4) sets class A scan concurrency. Class B on a broker host (rightkit#1367 step 2): the gate does not count builds; every
// component's broker-carried scans (cargo, rustright, carried Swift, and npm suites that declare a broker call) start without a limit, each component in plan order,
// and the broker queues and orders them. A carried scan's run limit is a stopwatch that stops while the broker holds all its requests QUEUED (broker-admission.mjs),
// so queue time is neither charged to the scan nor bounded by a gate allowance. Only class B suites the broker never sees (npm or node tests run on this host)
// keep a local limit of 3, one slot per suite. On a host with no broker RK_GATE_CLASS_B_JOBS (default 3) sets how many components' class B scans run at once; one
// component's scans stay in order (rightkit#1285).
// A crate whose ci/gate.config.json entry says `quietTests: true` has its test scans run last,
// one at a time, under the RustRight quiet lease (`rustright quiet acquire`, RUSTRIGHT_QUIET_LEASE in their environment).
// SIGINT/SIGTERM stop scheduling, kill every running scan's process tree and exit non-zero listing the scans not run.
import fs from "node:fs";
import { createHash, randomUUID } from "node:crypto";
import { envNamed, gateRunsRoot, resolveManagedRoots } from "../managed-roots.mjs";
import { resolveBrokerClient, spawnBrokerClient } from "../broker-client.mjs";
import os from "node:os";
import path from "node:path";
import { hostTarget } from "../cross-targets.mjs";
import { FOREIGN_PRE_COMMIT, retireHooksIn } from "../hook-retirement.mjs";
import { fileURLToPath } from "node:url";
import { installIdentity, installStateBytes } from "../dependency-identity.mjs";
import { BIOME_CONFIG_FILES, CHECKER_IDENTITY, OXLINT_CONFIG_FILES, ESLINT_CONFIG_FILES, LOCAL_CLASS_B_JOBS, WORKSPACE_RUST_FILE, ancestorConfigChanged, ancestorDirsOf, canonicalJson, defaultSpawn, defaultSpawnAsync, digestScanSet, fileScanCache, findRustfmt, metadataArgs, planScans, runScans, scanToolPolicyFromPipeline, scanDeferrals, scanFailures } from "../deterministic-scans.mjs";
import { changedLockNames, parseLock, reach } from "../lock-graph.mjs";
import { resolveExecutor } from "../gate-executor.mjs";
// The gate's own executor: an outer gate's cargo wrapper on PATH is never this gate's cargo (gate-executor.mjs `gateWrappers`).
const gateResolveExecutor = (args) => resolveExecutor({ ...args, gateWrappers: true });
import { ALL_UNITS_ERRORS, ALL_UNITS_EXCEPTIONS_FILE, BROKER_EXCEPTIONS_FILE, BROKER_EXCEPTION_ERRORS, CARGO_WORKSPACE_LIST_FILE, CARGO_WORKSPACE_LIST_KEY, RIGHTKIT_IDENTITY_FILE, authorizeAllUnits, authorizeNoBroker, checkCargoWorkspaceList, describeAllUnitsException, describeBrokerException, isRightKitIdentity, unitsOfSelector } from "../broker-exception.mjs";
import { SIGNING_REFUSAL_CODES, suiteSigningRefusals } from "../suite-signing-policy.mjs";
import { gitListRustFiles } from "../signing-files.mjs";
import { nativeHelpersFromConfig } from "../native-helpers.mjs";
import { buildCoverageReport, seconds, writeCoverageReport } from "../gate-coverage.mjs";
import { renderViews, renderViewsOf } from "../render-views.mjs";
import { LINUX_BROKER_ROUTE, NO_BROKER_ENV, directCargoPath, directPlatform, linuxBrokerDeclared, managedRuntimeClient, realCargoOf, runCargoSync } from "../managed-cargo.mjs";
import { createBrokerLive } from "../broker-admission.mjs";
import { bindingOf, compileOutputsRecord, compileReuseProblem, endRun, readReceipt, RECEIPT_SCHEMA, receiptKey, sameBinding, sameSource, sealedFor, stagesTo, startRun, STAGES, STAGE_SOURCES, writeReceipt } from "../gate-stages.mjs";
import { spawnSync as spawnInherit } from "../background-process.mjs";
// GC-A21: a run that stamps its own lines also stamps its inherited-output children's lines, through stamp-relay.mjs in front of the child
// (the launch stays synchronous and the output still streams). Set by main() when the run stamps; off for machine-readable runs.
let stampChildLines = false;
const STAMP_RELAY = fileURLToPath(new URL("../stamp-relay.mjs", import.meta.url));
const inheritLaunch = (cmd, a, opts) => (stampChildLines
  ? spawnInherit(process.execPath, [STAMP_RELAY, cmd, ...a], { ...opts, stdio: "inherit" })
  : spawnInherit(cmd, a, { ...opts, stdio: "inherit" }));
import { readDiskPolicy, scratchRoots } from "../disk-policy.mjs";
import { buildDirectReceipt, directVerdict, resolveTargetDirectory, verifyDirectReceipt, writeDirectDiagnostics } from "../direct-receipt.mjs";
import { createFreshnessCensus } from "../freshness-census.mjs";
import { platformDirOf } from "../platform-dir.mjs";
import { buildInventory, classifyUnitResults, dependencyFirstOrder, selectAffectedAcrossInventories, unitListReport } from "../affected-units.mjs";
import { createRequire } from "node:module";
import { nativeToolRefusal, nativeToolUses } from "../native-tool-planning.mjs";
import { BUILD_CONTROL_PROTOCOL, HANDSHAKE_PROBE_ID, RUNTIME_MANIFEST_FILE, mergeCapabilityViews, parseRuntimeIdentity, queryBrokerHandshake, snapshotReclaimJudge } from "../broker-handshake.mjs";
import net from "node:net";
import { installSpawnLedger } from "../spawn-ledger.mjs";
import { processIdentity, snapshotLockBody } from "../gate-seam.mjs";
import { cargoWidthRouteProblem, cargoWidthScanProblem, cargoWidthUnitsProblem, parseCargoWidth, validateCargoWidthPool } from "../cargo-width.mjs";

const PRIVATE_SCOPED_COMPILE = Symbol("private scoped compile");
const UNIT_LIST_UNION = Symbol("unit list union");
const CARGO_WIDTH = Symbol("Cargo width");
// Class B row input digests per tree (see rowDigest in the compile stage); counted so the run summary shows the time keying took.
const ROW_DIGEST_MEMO = new Map();
const ROW_DIGEST_STATS = { hits: 0, misses: 0, ms: 0 };
const ZERO = /^0+$/;
const SAFE_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const posix = (p) => String(p).replaceAll("\\", "/");
const text = (r) => String(r?.stdout ?? "");
const failed = (r) => !r || Boolean(r.error) || r.status !== 0;
const why = (r) => String(r?.error?.message ?? r?.stderr ?? "").trim().split(/\r?\n/).slice(-3).join(" | ");

// A green `check --only` view: app rules passed but no class A scan ran. Distinct from 0 so no caller can admit it as a full check.
export const PARTIAL_EXIT = 3;
/** The platforms a door run can package: desktop (signed, sealed, installed QA, publisher upload) and mobile (signed, sealed, store test track). */
export const PLATFORMS = Object.freeze(["mac", "win", "ios", "android"]);
export const MOBILE_PLATFORMS = Object.freeze(new Set(["ios", "android"]));
// The gate's code identity for result keys: the checker (deterministic-scans and scan-digest, CHECKER_IDENTITY) and this file, where
// the keys are built. An unidentifiable checker gets a per-process value, so nothing is reused across runs.
// The identity a class B (compile) result key carries (GC-A24): a key-schema version, bumped by hand whenever what a key covers changes,
// and the code of the runtime cargo wrapper and broker carrier, which decides what a carried compile ran and how its receipt is verified.
// The gate's own file and the checker are not in it: the verdict is cargo's, the command and tool versions are in every key already, and
// an edit elsewhere in the gate must not discard every compile result (rightkit#1506: Pulse, RK, RustRight and Legion kept 0 class B hits).
// /3 (0.6.54, rightkit#1538): every class B pass stored under /2 is left behind. Until 0.6.53 a grouped invocation stored a pass under each member's
// own key (a projected pass that never proved the member's own command) and a test group's pass was stored without its members' own runs; a
// /2 entry cannot be told from one that ran alone by its key, so none is read. The warm target directory is untouched: a rerun is a warm compile.
const CLASS_B_KEY_SCHEMA = "rightkit.class-b-key/3";
// GC-A24: passing rule-engine verdicts (app rules, RightKit's preflight) of a snapshot, under the checkout's git common directory.
const RULE_VERDICT_SCHEMA = "rightkit.rule-verdict/1";
function ruleVerdictCache({ checkout, spawn }) {
  try {
    const common = spawn("git", ["rev-parse", "--path-format=absolute", "--git-common-dir"], { cwd: checkout });
    if (failed(common) || !text(common).trim()) return null;
    const dir = path.join(text(common).trim(), "rightkit-gate-cache", "rule-verdicts");
    const file = (k) => path.join(dir, `${k}.json`);
    return {
      key: (parts) => createHash("sha256").update(canonicalJson({ schema: RULE_VERDICT_SCHEMA, node: process.version, platform: `${process.platform}-${process.arch}`, ...parts })).digest("hex"),
      passed: (k) => { try { return JSON.parse(fs.readFileSync(file(k), "utf8"))?.status === "pass"; } catch { return false; } },
      pass: (k) => { try { fs.mkdirSync(dir, { recursive: true }); const tmp = `${file(k)}.${process.pid}.tmp`; fs.writeFileSync(tmp, JSON.stringify({ schema: RULE_VERDICT_SCHEMA, status: "pass", at: new Date().toISOString() })); fs.renameSync(tmp, file(k)); } catch { /* a cache */ } },
    };
  } catch { return null; }
}
/** sha256 over every file of the gate package (the rule code a rule verdict depends on). */
const gatePackageMemo = new Map();
function gatePackageIdentity(gatePackage = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..")) {
  if (!gatePackageMemo.has(gatePackage)) {
    const h = createHash("sha256");
    const walk = (d) => fs.readdirSync(d, { withFileTypes: true }).sort((a, b) => (a.name < b.name ? -1 : 1)).forEach((e) => { if (e.name === "node_modules" || e.name.startsWith(".")) return; const p = path.join(d, e.name); if (e.isDirectory()) walk(p); else if (e.isFile()) { h.update(path.relative(gatePackage, p)); h.update("\0"); h.update(fs.readFileSync(p)); } });
    walk(gatePackage);
    gatePackageMemo.set(gatePackage, h.digest("hex"));
  }
  return gatePackageMemo.get(gatePackage);
}
// GC-A24: the class A key schema a standalone-tool row carries in place of the environment digest; bumped when what a class A key covers changes.
// Bump when any class A rule's verdict for the same inputs changes (NN23): the class A key names the rules' version, never every gate file's bytes.
const CLASS_A_KEY_SCHEMA = "rightkit.class-a-key/3";
/** Whether a class A row's verdict depends on the installed dependencies (GC-A24): it executes repository code, or its tool is pnpm. */
function classAEnvironmentBound(scan) { return scan.executes === true || scan.tool === "pnpm"; }
let classBKeyIdentityMemo = null;
function classBKeyIdentity() {
  if (classBKeyIdentityMemo === null) {
    const h = createHash("sha256");
    h.update(CLASS_B_KEY_SCHEMA);
    for (const f of ["cargo-target-shim.mjs", "snapshot-broker.mjs", "snapshot-request.mjs", "snapshot-metadata.mjs", "provenance-verify.mjs", "cargo-ledger.mjs"]) { h.update(`\0${f}\0`); h.update(fs.readFileSync(new URL(`../${f}`, import.meta.url))); }
    classBKeyIdentityMemo = h.digest("hex").slice(0, 24);
  }
  return classBKeyIdentityMemo;
}
let gateCodeIdentityMemo = null;
function gateCodeIdentity() {
  if (gateCodeIdentityMemo === null) {
    const h = createHash("sha256");
    h.update(CHECKER_IDENTITY ?? `unidentified-${process.pid}-${Date.now()}`);
    h.update("\0");
    h.update(fs.readFileSync(fileURLToPath(import.meta.url)));
    // The runtime cargo wrapper and the broker carrier it runs: their code decides what a carried compile did.
    for (const f of ["cargo-target-shim.mjs", "snapshot-broker.mjs", "snapshot-request.mjs", "snapshot-metadata.mjs", "provenance-verify.mjs", "cargo-ledger.mjs"]) { h.update(`\0${f}\0`); h.update(fs.readFileSync(new URL(`../${f}`, import.meta.url))); }
    gateCodeIdentityMemo = h.digest("hex").slice(0, 24);
  }
  return gateCodeIdentityMemo;
}

/**
 * The real cargo a broker host's runtime wrapper runs non-compiling calls with (GATE-CONTRACT GC-A11: metadata, fmt --check, deny,
 * audit, package --list never go to the broker, which refuses them since rustright a2972f0): the broker's registered toolchain first
 * (managed-cargo.mjs directCargoPath), else PATH without the RustRight shims; null when none exists.
 */
function directCargoBinOf(o) {
  const pk = o.platform === "win32" ? (Object.keys(o.env).find((k) => k.toUpperCase() === "PATH") ?? "Path") : "PATH";
  const d = directCargoPath({ env: { ...o.env, PATH: o.env[pk] }, exists: o.exists, platform: o.platform, gateWrappers: true });
  const names = o.platform === "win32" ? ["cargo.exe"] : ["cargo"];
  for (const dir of String(d.path ?? "").split(o.platform === "win32" ? ";" : ":").filter(Boolean)) for (const n of names) { const c = path.join(dir, n); if (o.exists(c)) return c; }
  return null;
}

const USAGE = `usage: rightkit-gate --to check|compile|package|qualify|release [--platform mac|win|ios|android] [--tier patch|update] [--receipt-in <file>] [--receipt-out <file>] | rightkit-gate --to check|compile|package|qualify|release --unit npm:@rightkit/<name>|cargo:rightkit-<name> [--receipt-out <file>] | rightkit-gate --to tool --unit cargo:rightkit-qa|cargo:rightkit-credentials [--no-broker <exception-id>] [--receipt-out <file>] [--commit <rev>] (builds the unit's release binary from the commit and enrols it: cargo:rightkit-qa as the managed rightkit-qa runner, cargo:rightkit-credentials as the managed OS credential provider updater signing runs; Mac and Windows) | rightkit-gate --to check|compile|package|qualify --changed [--base <ref>] [--receipt-dir <absolute dir>] | rightkit-gate --to check|compile|package|qualify --units-file <json {"units":[<selector>,...]} written for this run> [--receipt-dir <absolute dir>] | rightkit-gate --to check|compile|package|qualify --all-units-exception <id> (RightKit only: its unit is named on every run, or every unit by Adrian's entry in ci/all-units-exceptions.json of the commit) | rightkit-gate --to check|compile|package|qualify [--unit <selector>] --no-broker <exception-id> (Mac and Dell only; Adrian's recorded exception in .rightkit/broker-exceptions.json of the commit) | rightkit-gate --to dev|test [--commit <rev>] [-- <right-release args>] | rightkit-gate --to render [--commit <rev>] (builds the app's declared renderViews target from ci/gate.config.json through the carrier, runs it headless and stores the PNGs and a hashed receipt) | rightkit-gate --to check|compile|package|qualify|dev|test --route [--platform mac|win|ios|android] [--commit <rev>] | rightkit-gate stage --platform mac|win [--tier patch|update] | rightkit-gate [--base <ref>] [--plan] | rightkit-gate install-hook | rightkit-gate env-digest | rightkit-gate snapshot [--commit <rev>] [--verify [--env <sha256>]] [--json] | rightkit-gate live-scans --repo <root> | rightkit-gate clippy [<cargo clippy args>] | rightkit-gate env-digest [--snapshot <dir> --commit <sha>] | rightkit-gate check [--repo <dir>] [--commit <rev> | --worktree] [--rule-tier compile|release] [--only <rule,...>] [--list] [--print-baseline] [--baseline <file>] | rightkit-gate check --commit <sha> --full-a --snapshot <dir> | --version`;
/** The host platform (process.platform) that constructs each platform's artifact; a platform missing here (android) builds on any host. */
const STAGE_NATIVE_HOST = Object.freeze({ mac: "darwin", ios: "darwin", win: "win32" });

// ---- pure helpers (exported for tests) ----------------------------------------------------------------------

/** The `packages:` globs of a pnpm-workspace.yaml (block or flow list; the subset pnpm repositories use). */
export function parseWorkspaceGlobs(source) {
  const out = []; let inPackages = false;
  for (const raw of String(source).split(/\r?\n/)) {
    const line = raw.replace(/\s+#.*$/, "");
    const flow = /^packages\s*:\s*\[(.*)\]\s*$/.exec(line);
    if (flow) { for (const item of flow[1].split(",")) { const v = item.trim().replace(/^["']|["']$/g, ""); if (v) out.push(v); } inPackages = false; continue; }
    if (/^packages\s*:/.test(line)) { inPackages = true; continue; }
    if (/^[^\s#-]/.test(line)) { inPackages = false; continue; }
    if (!inPackages) continue;
    const m = /^\s*-\s*(?:"([^"]*)"|'([^']*)'|(\S+))\s*$/.exec(line);
    if (m) out.push(m[1] ?? m[2] ?? m[3]);
  }
  return out;
}

/** A pnpm workspace glob as a RegExp over a repo-relative posix directory: `*` one segment, `**` any depth. */
export function globToRegExp(glob) {
  const g = posix(glob).replace(/^\.\//, "").replace(/\/+$/, "");
  let re = "";
  for (let i = 0; i < g.length;) {
    if (g.startsWith("**/", i)) { re += "(?:.+/)?"; i += 3; }
    else if (g.startsWith("**", i)) { re += ".*"; i += 2; }
    else if (g[i] === "*") { re += "[^/]*"; i += 1; }
    else { re += g[i].replace(/[.+?^${}()|[\]\\]/g, "\\$&"); i += 1; }
  }
  return new RegExp(`^${re}$`);
}

/** npm unit name from a package name: `@scope/x` -> `scope-x`; anything still unsafe falls back to the directory. */
const unitName = (pkgName, dir) => {
  const n = String(pkgName ?? "").replace(/^@/, "").replace(/\//g, "-");
  return SAFE_NAME.test(n) ? n : dir === "." ? "root" : dir.replace(/\//g, "-");
};

// The bounds of the pnpm root walk. A lock deeper than this is a buried fixture, not an app; more roots than this is refused, because covering
// some of them would hide the rest.
const PNPM_ROOT_MAX_DEPTH = 6;
const PNPM_ROOT_MAX_COUNT = 64;

/** Tracked files with exactly this name (a lister that ignores the name is filtered here). */
const trackedNamed = (tracked, name) => tracked(name).map(posix).filter((p) => path.posix.basename(p) === name);

/**
 * The pnpm roots of the repository, sorted: "." when the repository root holds a pnpm-workspace.yaml or a package.json, and every other
 * tracked directory (outside node_modules, at most 6 levels deep) that holds a pnpm-lock.yaml or a pnpm-workspace.yaml beside a
 * package.json. A repository such as CodeRight (apps/coderight-tauri and apps/coderight-web, each with its own lock, nothing at the root) has
 * two; a plain repository has "." only. A walk over the tracked files, never the file system: nothing in node_modules or untracked is a root.
 */
export function discoverPnpmRoots({ root, tracked, exists }) {
  const manifestDirs = new Set(trackedNamed(tracked, "package.json").map((p) => path.posix.dirname(p)));
  const roots = new Set();
  if (exists(path.join(root, "pnpm-workspace.yaml")) || exists(path.join(root, "package.json"))) roots.add(".");
  for (const file of [...trackedNamed(tracked, "pnpm-lock.yaml"), ...trackedNamed(tracked, "pnpm-workspace.yaml")]) {
    const dir = path.posix.dirname(file);
    const parts = dir.split("/");
    if (dir === "." || parts.includes("node_modules") || parts.length > PNPM_ROOT_MAX_DEPTH || !manifestDirs.has(dir)) continue;
    roots.add(dir);
  }
  if (roots.size > PNPM_ROOT_MAX_COUNT) throw new Error(`${roots.size} pnpm roots (the gate covers at most ${PNPM_ROOT_MAX_COUNT}, and covering some would hide the rest)`);
  return [...roots].sort();
}

/**
 * npm units, per pnpm root (discoverPnpmRoots). Within a root: pnpm-workspace.yaml globs over the tracked package.json files, else the root's
 * own package.json. The root package.json is a unit as well when the workspace file is absent, has no `packages:` (or an empty one) or declares
 * "." (or "./"); with a `packages:` list that does not name the root it is only the workspace's own manifest and is not a unit. A directory
 * belongs to the deepest root that holds it. The repository root's units are named from their package names (`@scope/x` -> `scope-x`, a
 * collision falls back to the directory) and carry no `root`, exactly as in a repository with one root. The units of any other root are named
 * from their directory (`apps/coderight-web` -> `apps-coderight-web`), so their scan ids carry the root path, and carry `root`.
 */
export function discoverNpmUnits({ root, tracked, readFile, exists }) {
  const roots = discoverPnpmRoots({ root, tracked, exists });
  const manifestDirs = [...new Set(trackedNamed(tracked, "package.json").map((p) => path.posix.dirname(p)))];
  const within = (rootDir, dir) => (rootDir === "." ? dir : dir === rootDir ? "." : dir.startsWith(`${rootDir}/`) ? dir.slice(rootDir.length + 1) : null);
  const joined = (rootDir, rel) => (rootDir === "." ? rel : rel === "." ? rootDir : `${rootDir}/${rel}`);
  const ownerOf = (dir) => roots.reduce((best, r) => (within(r, dir) !== null && (best === null || r.length > best.length) ? r : best), null);
  const found = new Map();
  for (const rootDir of roots) {
    let dirs;
    const workspaceFile = path.join(root, joined(rootDir, "pnpm-workspace.yaml"));
    if (exists(workspaceFile)) {
      const globs = parseWorkspaceGlobs(readFile(workspaceFile));
      const inc = globs.filter((g) => !g.startsWith("!")).map(globToRegExp);
      const exc = globs.filter((g) => g.startsWith("!")).map((g) => globToRegExp(g.slice(1)));
      const positive = globs.filter((g) => !g.startsWith("!"));
      const rootDeclared = positive.length === 0 || positive.some((g) => g === "." || g === "./");
      dirs = manifestDirs.map((d) => within(rootDir, d)).filter((rel) => rel !== null && (rel === "." ? rootDeclared : inc.some((r) => r.test(rel))) && !exc.some((r) => r.test(rel))).map((rel) => joined(rootDir, rel));
    } else dirs = exists(path.join(root, joined(rootDir, "package.json"))) ? [rootDir] : [];
    for (const dir of dirs) if (ownerOf(dir) === rootDir) found.set(dir, rootDir);
  }
  const rows = [...found.keys()].sort().map((dir) => {
    let manifest;
    try { manifest = JSON.parse(readFile(path.join(root, dir, "package.json")).replace(/^﻿/, "")); } catch (e) { throw new Error(`${dir}/package.json: ${e.message}`); }
    const rootDir = found.get(dir);
    return { dir, root: rootDir, name: rootDir === "." ? unitName(manifest.name, dir) : dir.replace(/\//g, "-") };
  });
  const count = new Map();
  for (const r of rows) count.set(r.name, (count.get(r.name) ?? 0) + 1);
  const named = rows.map((r) => ({ ...r, name: count.get(r.name) > 1 && r.root === "." ? unitName("", r.dir === "." ? "root" : r.dir) : r.name }));
  const taken = new Set();
  for (const r of named) {
    if (taken.has(r.name)) throw new Error(`two npm units are named ${r.name} (${r.dir}); rename one of the packages`);
    taken.add(r.name);
  }
  return named.map((r) => ({ name: r.name, dir: r.dir, ...(r.root === "." ? {} : { root: r.root }) }));
}

const dirOfManifest = (m) => path.posix.dirname(m);
const byDepthThenName = (a, b) => dirOfManifest(a).split("/").length - dirOfManifest(b).split("/").length || (a < b ? -1 : 1);

// `cargo metadata --no-deps --format-version 1 --locked` in one directory, through the resolved cargo command. Returns
// { cargoDir, members: [{ name, dir, manifest, publish }] } with every path repository-relative.
function workspaceMetadata({ root, spawn, cargoCommand, dir, realpath }) {
  const r = spawn(cargoCommand[0], [...cargoCommand.slice(1), ...metadataArgs()], { cwd: path.join(root, dir) });
  if (failed(r)) throw new Error(`cargo metadata failed: ${why(r) || `exit ${r?.status}`}${dir === "." ? "" : ` (in ${dir})`}`);
  let meta;
  try { meta = JSON.parse(text(r)); } catch (e) { throw new Error(`cargo metadata output is not JSON: ${e.message}${dir === "." ? "" : ` (in ${dir})`}`); }
  const real = (p) => { try { return realpath(p); } catch { return path.resolve(p); } };
  const rootReal = real(root);
  const rel = (abs) => posix(path.relative(rootReal, real(abs))) || ".";
  const cargoDir = rel(meta.workspace_root);
  if (cargoDir === ".." || cargoDir.startsWith("../")) throw new Error(`the Cargo workspace root ${meta.workspace_root} is outside the repository`);
  const members = (meta.packages ?? []).map((p) => {
    const pkgDir = rel(path.dirname(p.manifest_path));
    if (pkgDir === ".." || pkgDir.startsWith("../")) throw new Error(`workspace member ${p.name} lives outside the repository (${pkgDir})`);
    // Bin targets (rustright#209 item 5) and whether `cargo test -p <name>` links them: Cargo builds a package's bins only for the integration tests
    // it selects, so a package with none leaves `debug/<bin>` to whatever an earlier build wrote. A bin with required-features is not counted.
    const kinds = (t) => (Array.isArray(t.kind) ? t.kind : []);
    const bins = (p.targets ?? []).filter((t) => kinds(t).includes("bin") && !(t["required-features"] ?? t.required_features ?? []).length).map((t) => t.name).sort();
    // Only an integration test `cargo test` selects (`test` not false, no required-features) makes it link the bins; a bench is test = false by default.
    const testLinksBins = (p.targets ?? []).some((t) => kinds(t).includes("test") && t.test !== false && !(t["required-features"] ?? t.required_features ?? []).length);
    return { name: p.name, dir: pkgDir, manifest: pkgDir === "." ? "Cargo.toml" : `${pkgDir}/Cargo.toml`, publish: !(Array.isArray(p.publish) && p.publish.length === 0), bins, testLinksBins };
  }).sort((a, b) => (a.name < b.name ? -1 : 1));
  return { cargoDir, members };
}

/**
 * Rust units of ONE Cargo workspace from `cargo metadata --no-deps --format-version 1 --locked`: several workspaces are refused
 * here. The gate itself uses discoverRustWorkspaces, which handles every workspace of the repository. Returns
 * { present, units: [{ name, dir, publish }], cargoDir, uncovered }.
 */
/**
 * Where the gate's own temporary files live (cargo-deny's metadata file and config overlay, read by the broker by absolute path). The
 * RustRight broker refuses paths on the system volume (RK_INTERNAL_DRIVE_REFUSED: agents never write to the internal drive), and
 * os.tmpdir() is on it on every Mac and Dell, so on a host with a disk-policy scratch root (/Volumes/D/scratch, D:\scratch) they go
 * there. RK_GATE_TMPDIR overrides; os.tmpdir() is the fallback where no scratch root exists (CI runners, cloud containers). The gate
 * exports nothing and copies no source: this is not a source location (docs/GATE-CONTRACT.md, 2026-10-29: no export or copy).
 */
export function gateTmpRoot({ env = process.env, platform = process.platform, exists = fs.existsSync, policy = null } = {}) {
  if (env.RK_GATE_TMPDIR) return env.RK_GATE_TMPDIR;
  try {
    const roots = scratchRoots(policy ?? readDiskPolicy(), { platform, env });
    const found = roots.find((root) => exists(root));
    if (found) return found;
  } catch { /* no readable policy: the system temp dir */ }
  return os.tmpdir();
}

/**
 * Tool caches inside node_modules that nothing installs: test-result caches (vitest), the vite optimizer and bundled-config temp
 * files, transpile and other tool caches (.cache). Some hold code a later step executes, so the environment a verdict carries binds
 * their bytes as they stand when class A ends, and the broker's START recomputes it: a cache edited, added or removed after the
 * verdict (by a dev server, another tool, or by hand) voids the ticket and class A runs again. Nothing is deleted. Inside one run
 * the start/end comparison leaves them out, because the run itself writes them. pnpm's task-run state holds only completion
 * markers pnpm reads, never code, and is not bound.
 */
/** Longest the class A environment waits for `pnpm --version`. */
export const PNPM_PROBE_TIMEOUT_MS = 60_000;
export const PNPM_PIN_REFUSAL = "RK_PNPM_VERSION_MISMATCH";
export const TOOL_CACHE_DIRS = new Set([".vite", ".vite-temp", ".vitest-cache", ".cache"]);
export { installStateBytes };
export const INSTALL_STATE_NOT_COVERED = new Set([".pnpm-task-run-state-v1"]);
const nodeModulesDirs = (root, files) => [...new Set(files.filter((f) => /(^|\/)package\.json$/.test(f)).map((f) => path.join(path.dirname(path.join(root, f)), "node_modules")))].sort();
function isLink(p) { try { return fs.lstatSync(p).isSymbolicLink(); } catch { return false; } }

/**
 * Members of the root pnpm workspace holding their own pnpm virtual store (node_modules/.pnpm) while the root has one, without
 * being an install root (no pnpm-lock.yaml of their own): leftovers of a standalone install from before the package joined the workspace. Their dependencies resolve to
 * the root store, but the leftover can still be read, and vitest hashes the nearest lockfile it finds (node_modules/.pnpm/
 * lock.yaml) for a module cache it shares across the workspace, so one such package makes concurrent vitest runs clear the
 * shared cache under each other (ENOTEMPTY, fullA-16).
 */
export function orphanedPnpmStores({ root, files }) {
  // Only proven orphans: the repository root is a pnpm workspace with its own virtual store, and the package is a member of it by
  // the root pnpm-workspace.yaml globs (negations honoured) and not an install root of its own. A standalone nested project is
  // not judged.
  const ws = path.join(root, "pnpm-workspace.yaml");
  if (!fs.existsSync(ws) || !fs.existsSync(path.join(root, "node_modules", ".pnpm"))) return [];
  const globs = parseWorkspaceGlobs(fs.readFileSync(ws, "utf8"));
  const include = globs.filter((g) => !g.startsWith("!")).map(globToRegExp);
  const exclude = globs.filter((g) => g.startsWith("!")).map((g) => globToRegExp(g.slice(1)));
  return nodeModulesDirs(root, files).filter((nm) => {
    const pkg = path.dirname(nm); const rel = path.relative(root, pkg).split(path.sep).join("/");
    if (!rel || !include.some((re) => re.test(rel)) || exclude.some((re) => re.test(rel))) return false;
    return fs.existsSync(path.join(nm, ".pnpm")) && !fs.existsSync(path.join(pkg, "pnpm-lock.yaml"));
  }).map((nm) => path.relative(root, path.join(nm, ".pnpm")).split(path.sep).join("/"));
}

/**
 * Cache files that hold only a tool's own bookkeeping, never code a check executes, at exact paths: vitest's result ordering,
 * node_modules/.vite/vitest/results.json and node_modules/.vite/vitest/<40-hex project hash>/results.json, which vitest reads only
 * with JSON.parse to order test files (vitest 5.0.5 dist/chunks/cli-api.*.js ResultsCache.readFromCache).
 */
export const CACHE_NON_CODE = [/(^|\/)node_modules\/\.vite\/vitest\/([0-9a-f]{40}\/)?results\.json$/];
/** Every file in the workspace's tool caches (TOOL_CACHE_DIRS, not following links) with its content hash, keyed by path. */
export function toolCacheManifest({ root, files }) {
  const out = new Map();
  const walk = (dir) => {
    let entries; try { entries = fs.readdirSync(dir, { withFileTypes: true }); } catch (error) { if (error?.code === "ENOENT") return; throw new Error(`cannot list the tool cache ${path.relative(root, dir)} (${error?.code ?? error?.message})`); }
    for (const e of entries) {
      const p = path.join(dir, e.name); const rel = path.relative(root, p).split(path.sep).join("/");
      if (e.isDirectory()) walk(p);
      else if (e.isSymbolicLink()) out.set(rel, `link:${fs.readlinkSync(p)}`);
      else if (e.isFile()) { try { out.set(rel, createHash("sha256").update(fs.readFileSync(p)).digest("hex")); } catch (error) { if (error?.code !== "ENOENT") throw new Error(`cannot read the tool cache file ${rel} (${error?.code ?? error?.message})`); } }
    }
  };
  for (const d of nodeModulesDirs(root, files)) for (const name of TOOL_CACHE_DIRS) walk(path.join(d, name));
  return out;
}
/**
 * What a run did to the tool caches: a pre-existing cache file that changed or vanished means the checks may have read bytes that
 * are no longer there (a concurrent writer, an interrupted or partial write), so the verdict cannot name them. Files added while
 * it ran are listed and bound in env, though another process may have written them. Limits: a file changed and restored between
 * the two manifests is not observed, and a file added and read by a check is bound only as it stands at the end. CACHE_NON_CODE
 * files may change.
 */
export function toolCacheDrift(before, after) {
  const changed = []; const added = [];
  for (const [p, h] of before) if (!CACHE_NON_CODE.some((re) => re.test(p)) && after.get(p) !== h) changed.push(p);
  for (const p of after.keys()) if (!before.has(p)) added.push(p);
  return { changed: changed.sort(), added: added.sort() };
}

/**
 * The installed dependency bytes a class A verdict executes: every node_modules directory of the workspace (the root one holds
 * pnpm's virtual store) walked without following links, plus everything a link there resolves to outside those directories (a
 * workspace package's built and ignored files, a linked package elsewhere on disk), each resolved target hashed once. A real file
 * is its path, exec bit and content hash; a link is its target (a dangling one is bound as dangling). A cycle is cut by the
 * resolved-path set; a target that is the root, an ancestor of it or the filesystem root, any other entry type, an unreadable
 * entry, or more than `maxFiles` files throws. Tool caches (TOOL_CACHE_DIRS) leave no trace. Bytes that change while it hashes are caught by comparing the digest at the start
 * and end of the run and again at START.
 */
export function installedDependencyDigest({ root, files, includeCaches = true }) {
  // GC-A24 (Adrian, 2026-11-05): the installed dependencies are identified by pnpm's own install record (dependency-identity.mjs
  // installIdentity: install-state files with their timestamps removed, .pnpm/lock.yaml, top-level links, and whole bytes only of linked
  // packages from outside the repository), not by reading every installed file. `includeCaches` no longer applies: tool caches are
  // not part of the install record. The roots are the node_modules directories of every committed package.json (nodeModulesDirs).
  void includeCaches;
  return installIdentity(root, nodeModulesDirs(root, files));
}

/**
 * What a class A verdict depends on outside the checked tree: every file of this gate package (code at any depth, the advisory
 * database, contracts, assets), node and pnpm versions, platform/arch, and each package's installed state
 * (node_modules/.modules.yaml) and the installed dependency bytes (installedDependencyDigest). The broker binds an admission ticket to it and recomputes it when the compile starts
 * (`rightkit-gate env-digest`), so a dependency or tool change while a compile waits invalidates the ticket.
 */
export function classAEnvironmentDigest({ root, files, spawn = defaultSpawn, includeCaches = true, gatePackage = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..") }) {
  // Only a file that does not exist is "absent"; any other read failure throws: an input whose bytes cannot be read is never
  // identified by a placeholder that would match the same failure later.
  // `optional`: only an install-state file may legitimately not exist; a gate file found by the walk must be readable.
  const fileDigest = (f, { optional = false } = {}) => { try { return createHash("sha256").update(fs.readFileSync(f)).digest("hex"); } catch (error) { if (optional && (error?.code === "ENOENT" || error?.code === "ENOTDIR")) return "absent"; throw new Error(`cannot read ${f} for the class A environment (${error?.code ?? error?.message})`); } };
  // `gatePackage` defaults to this very package; only a test that must not be disturbed by other writers of the live package injects another.
  const gatePkg = gatePackage;
  const walkGate = (dir) => fs.readdirSync(dir, { withFileTypes: true }).flatMap((e) => (e.name === "node_modules" || e.name.startsWith(".") ? [] : e.isDirectory() ? walkGate(path.join(dir, e.name)) : [path.join(dir, e.name)]));
  // NN23 (approved by Adrian 2026-11-06 with its one cold run): the gate's CODE enters the key as CLASS_A_KEY_SCHEMA, bumped by hand when a
  // class A rule's verdict changes, not as the bytes of every file (any release re-keyed every row). Data the scans read (advisory database,
  // rule tables, configs) still enters by content; release-only files (changelog, docs, version manifests, the package's own tests) do not.
  const releaseOnly = (rel) => /\.(?:mjs|cjs|js|ts)$/.test(rel) || /\.md$/i.test(rel) || /(^|\/)(?:package\.json|rightkit-versions\.json)$/.test(rel) || /\.test\.(?:mjs|js)$/.test(rel);
  const gateFiles = [`class-a-key-schema:${CLASS_A_KEY_SCHEMA}`, ...walkGate(gatePkg).sort().map((f) => path.relative(gatePkg, f)).filter((rel) => !releaseOnly(rel)).map((rel) => `${rel}:${fileDigest(path.join(gatePkg, rel))}`)];
  // Bounded: a pnpm that switches itself to the repository's packageManager version on first use, or waits on the network,
  // must fail the probe (and its process tree) within PNPM_PROBE_TIMEOUT_MS, never hang an admission.
  const pnpmVersion = (() => {
    const r = spawn("pnpm", ["--version"], { cwd: root, boundedMs: PNPM_PROBE_TIMEOUT_MS });
    if (r?.timedOut) throw new Error(`cannot identify pnpm for the class A environment (pnpm --version timed out after ${PNPM_PROBE_TIMEOUT_MS} ms; run it once in ${root} so pnpm can switch to the packageManager version)`);
    if (failed(r) || !text(r).trim()) throw new Error(`cannot identify pnpm for the class A environment (${r?.error ? `could not run pnpm: ${r.error.code ?? r.error.message}` : why(r)})`);
    return text(r).trim();
  })();
  const installState = files.filter((f) => /(^|\/)package\.json$/.test(f)).map((f) => path.join(path.dirname(path.join(root, f)), "node_modules", ".modules.yaml")).map((m) => { let d; try { d = createHash("sha256").update(installStateBytes(".modules.yaml", fs.readFileSync(m))).digest("hex"); } catch (error) { d = fileDigest(m, { optional: true }); } return `${path.relative(root, m)}:${d}`; });
  return createHash("sha256").update([`node ${process.version}`, `pnpm ${pnpmVersion}`, `platform ${process.platform}-${process.arch}`, ...gateFiles, ...installState, `installed ${installedDependencyDigest({ root, files, includeCaches })}`].join("\n")).digest("hex");
}

/** The pnpm a package.json pins (`packageManager: "pnpm@12.16.1+sha512.."` -> `12.16.1`), or null for no pnpm pin. */
export function pnpmPinOf(packageJsonText) {
  let pm; try { pm = JSON.parse(packageJsonText)?.packageManager; } catch { return null; }
  const m = typeof pm === "string" ? /^pnpm@(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)(?:\+.*)?$/.exec(pm.trim()) : null;
  return m ? m[1] : null;
}

/**
 * Class A runs the pnpm it bound, never a pnpm that relaunches another one through a shell: on Windows a pnpm older than a package's
 * `packageManager` pin switches itself to the pinned version through `cmd /d /s /c` with ^-escaped text, which the class A launch guard
 * refuses mid-row as an unreadable launch. This checks every pnpm pin of the checked tree BEFORE any class A row launches and returns a
 * typed problem naming both versions and the fix, or null. Nothing is admitted: the guard keeps refusing any relaunch.
 *
 * Pins under a declared fixture directory (ci/gate.config.json `fixtures`) are not the repository's own: a fixture's pnpm pin is
 * evidence content that every other scan skips, so this check skips it too (rightkit#1468).
 *
 * Policy for a repository pinned differently from the host default (rightkit#1468): the pnpm on PATH is first asked from a directory
 * with no package.json (it cannot switch there). When it differs from a pin, the pinned version is probed directly, by running
 * `pnpm --version` inside the pinning package's own directory, where pnpm's packageManager self-switch (or corepack) resolves the pin;
 * the pin is admitted only when that probe prints exactly the pinned version. Only when the probe fails does the check refuse, with the
 * migration path. On Windows the self-switch itself is the refused shell relaunch, so there the pin must equal the neutral version.
 */
export function pnpmPinProblem({ root, files, spawn = defaultSpawn, readFile = (f) => fs.readFileSync(f, "utf8"), neutralCwd = os.tmpdir(), fixtures = [], platform = process.platform }) {
  const fixtureDirs = fixtures.map((d) => String(d).replace(/\\/g, "/").replace(/\/+$/, "")).filter(Boolean);
  const underFixture = (f) => fixtureDirs.some((d) => f === d || f.startsWith(`${d}/`));
  const pins = new Map();
  for (const f of files.filter((x) => /(^|\/)package\.json$/.test(x) && !underFixture(x))) {
    let pin = null; try { pin = pnpmPinOf(readFile(path.join(root, f))); } catch { pin = null; }
    if (pin) pins.set(f, pin);
  }
  if (!pins.size) return null;
  const r = spawn("pnpm", ["--version"], { cwd: neutralCwd, boundedMs: PNPM_PROBE_TIMEOUT_MS });
  const onPath = failed(r) ? "" : text(r).trim();
  const fix = (pin) => `install the pinned pnpm as the pnpm on PATH (for example \`pnpm self-update ${pin}\`), or make the repository's own pin resolvable (corepack enable, network access to the pnpm release), and run again`;
  const bad = [...pins].filter(([, pin]) => pin !== onPath);
  if (!bad.length) return null;
  if (platform !== "win32") {
    // One probe per distinct pin, from the first package.json that carries it: the self-switch is per directory.
    const probed = new Map(); const unresolved = [];
    for (const [file, pin] of bad) {
      if (!probed.has(pin)) {
        const dir = path.join(root, path.dirname(file));
        const pr = spawn("pnpm", ["--version"], { cwd: dir, boundedMs: PNPM_PROBE_TIMEOUT_MS });
        probed.set(pin, failed(pr) ? null : text(pr).trim());
      }
      if (probed.get(pin) !== pin) unresolved.push([file, pin]);
    }
    if (!unresolved.length) return null;
    const [file, pin] = unresolved[0];
    const got = probed.get(pin);
    return { code: PNPM_PIN_REFUSAL, message: `${PNPM_PIN_REFUSAL}: ${file} pins packageManager pnpm@${pin} and the pnpm on PATH is ${onPath || "unreadable"}; running pnpm inside ${path.dirname(file)} to resolve the pin ${got ? `printed ${got}` : "failed"}${unresolved.length > 1 ? ` (${unresolved.length} package.json files differ)` : ""}; ${fix(pin)}` };
  }
  if (!onPath) { const [, pin] = bad[0]; return { code: PNPM_PIN_REFUSAL, message: `${PNPM_PIN_REFUSAL}: cannot read the version of the pnpm on PATH (${r?.error ? r.error.code ?? r.error.message : why(r)}); ${fix(pin)}` }; }
  const [file, pin] = bad[0];
  return { code: PNPM_PIN_REFUSAL, message: `${PNPM_PIN_REFUSAL}: the pnpm on PATH is ${onPath} but ${file} pins packageManager pnpm@${pin}${bad.length > 1 ? ` (${bad.length} package.json files differ)` : ""}; a different pnpm relaunches the pinned one through a shell, which class A refuses (RK_CLASS_A_COMPILE_LAUNCH); ${fix(pin)}` };
}

/** Split pathspecs into batches whose joined length stays under `limit` characters (well below Windows' 32767 command line). */
export function pathspecBatches(paths, limit = 8000) {
  const batches = []; let current = []; let size = 0;
  for (const p of paths) {
    if (current.length && size + p.length + 1 > limit) { batches.push(current); current = []; size = 0; }
    current.push(p); size += p.length + 1;
  }
  if (current.length) batches.push(current);
  return batches;
}

export function discoverRustUnits({ root, spawn, cargoCommand, tracked, readFile, realpath = fs.realpathSync }) {
  const manifests = tracked("Cargo.toml");
  if (!manifests.length) return { present: false, units: [], cargoDir: ".", uncovered: [] };
  const roots = [];
  for (const m of [...manifests].sort(byDepthThenName)) {
    const d = dirOfManifest(m);
    if (!roots.some((r) => r === "." || d === r || d.startsWith(`${r}/`))) roots.push(d);
  }
  if (roots.length > 1) throw new Error(`more than one Cargo workspace (${roots.join(", ")}); discoverRustUnits covers one, so none is partly covered (rightkit-gate uses discoverRustWorkspaces)`);
  const { cargoDir, members } = workspaceMetadata({ root, spawn, cargoCommand, dir: roots[0], realpath });
  const units = members.map(({ name, dir, publish }) => ({ name, dir, publish }));
  const covered = new Set(members.map((m) => m.manifest));
  const uncovered = manifests.filter((m) => !covered.has(m) && /^\s*\[package\]/m.test(readFile(path.join(root, m))));
  return { present: true, units, cargoDir, uncovered };
}

/**
 * The unit name in scan ids and ci/gate.config.json: the plain crate in a root workspace and in a workspace that is exactly that crate's
 * own directory (`crates/rightkit-fs` holding rightkit-fs: one workspace and lock per crate, rightkit#1285), `<cargoDir>:<crate>` below
 * any other workspace.
 */
export const rustUnitName = (cargoDir, crate) => (cargoDir === "." || cargoDir === `crates/${crate}` ? crate : `${cargoDir}:${crate}`);

/**
 * The vendored sources a crate compiles: `{ "vendor/<name>": [unit names] }`, from every quoted `../../vendor/<name>` path in the crate
 * manifests (a [patch.crates-io] or dependency `path`, rightkit-typeset's vendor/citationberg). A changed file under the directory
 * selects those units. `readManifest(unit)` returns the manifest text, or null.
 */
export function vendorOwners(units, readManifest) {
  const owners = {};
  for (const u of units) {
    let manifest = null;
    try { manifest = readManifest(u); } catch { manifest = null; }
    for (const m of String(manifest ?? "").matchAll(/["'](?:\.\.\/)+vendor\/([A-Za-z0-9_.@+-]+)\/?["']/g)) {
      const list = (owners[`vendor/${m[1]}`] ??= []);
      if (!list.includes(u.name)) list.push(u.name);
    }
  }
  return owners;
}

/**
 * The top-level TOML tables of a Cargo manifest: name -> body text (the lines under `[name]` up to the next header, trimmed).
 * `preamble` holds the lines before the first header. Enough to compare manifests table by table; no value parsing.
 */
export function manifestTables(text) {
  const tables = new Map(); let current = "preamble"; let body = [];
  const flush = () => { tables.set(current, (tables.get(current) ?? "") + body.join("\n").trim()); body = []; };
  for (const line of String(text ?? "").replace(/\r\n/g, "\n").split("\n")) {
    const header = /^\s*\[\[?([^\]]+)\]\]?\s*(#.*)?$/.exec(line);
    if (header) { flush(); current = header[1].trim(); continue; }
    body.push(line);
  }
  flush();
  return tables;
}

/**
 * The value text of `key` in the top-level table `table` of a manifest read by manifestTables (everything from the `=` on), whether it is
 * written under `[table]` or as the dotted root key `table.key = ...`; null when absent. No value parsing: tomlString and tomlStrings read it.
 */
export function tomlValue(tables, table, key) {
  const esc = (x) => x.replace(/[.*+?^${}()|[\]\\-]/g, "\\$&");
  const own = tables.get(table) ?? "";
  const inTable = new RegExp(`^[ \\t]*${esc(key)}[ \\t]*=[ \\t]*`, "m").exec(own);
  if (inTable) return own.slice(inTable.index + inTable[0].length);
  const root = tables.get("preamble") ?? "";
  const dotted = new RegExp(`^[ \\t]*${esc(table)}\\.${esc(key)}[ \\t]*=[ \\t]*`, "m").exec(root);
  return dotted ? root.slice(dotted.index + dotted[0].length) : null;
}

/** True when the manifest has the top-level table `name`: as `[name]`, as a `[name.sub]` table, or as dotted root keys `name.key = ...`. */
export const hasTomlTable = (tables, name) => tables.has(name) || [...tables.keys()].some((k) => k.startsWith(`${name}.`)) || new RegExp(`^[ \\t]*${name.replace(/\W/g, "\\$&")}\\.`, "m").test(tables.get("preamble") ?? "");

/** The string a TOML value starts with ("basic" or 'literal'); null when the value is not a string. */
export function tomlString(value) {
  const m = /^(?:"((?:[^"\\\n]|\\.)*)"|'([^'\n]*)')/.exec(String(value ?? ""));
  return m ? (m[1] !== undefined ? m[1].replace(/\\(.)/g, "$1") : m[2]) : null;
}

/**
 * The strings of the TOML array a value starts with: "basic" and 'literal' strings, across lines, with `#` comments (and any `]` in
 * them or in a string) skipped. [] when the value is not an array.
 */
export function tomlStrings(value) {
  const s = String(value ?? "");
  if (s[0] !== "[") return [];
  const out = [];
  for (let i = 1; i < s.length;) {
    const c = s[i];
    if (c === "]") break;
    if (c === "#") { while (i < s.length && s[i] !== "\n") i += 1; continue; }
    if (c === '"' || c === "'") {
      let j = i + 1; let v = "";
      while (j < s.length && s[j] !== c && s[j] !== "\n") { if (c === '"' && s[j] === "\\" && j + 1 < s.length) { v += s[j + 1]; j += 2; } else { v += s[j]; j += 1; } }
      out.push(v); i = j + 1; continue;
    }
    i += 1;
  }
  return out;
}

/** The repository-relative directories a workspace manifest `exclude`s (`[workspace] exclude = [...]`); [] when none or unreadable. */
export function workspaceExcludes(readFile, root, cargoDir) {
  let text;
  try { text = readFile(path.join(root, cargoDir === "." ? "Cargo.toml" : `${cargoDir}/Cargo.toml`)); } catch { return []; }
  return tomlStrings(tomlValue(manifestTables(text), "workspace", "exclude")).map((d) => posix(d).replace(/\/+$/, "")).map((d) => (cargoDir === "." ? d : `${cargoDir}/${d}`));
}

// The manifest tables whose every effect on a build is also written into Cargo.lock: a changed source ([patch.*]) and workspace
// membership ([workspace] members/exclude: a new member is a new lock package, a removed one disappears). A [workspace.dependencies]
// change is lock-reflected only when it is a version change (the lock records the resolved version): a feature list reaches every
// `workspace = true` dependent without touching the lock. Anything else in the root manifest ([profile.*], [workspace.package],
// [workspace.lints], [workspace.metadata] read by build scripts) can change what every crate compiles to, so it keeps every crate in scope.
const LOCK_REFLECTED_TABLE = /^patch\..+$/;
const WORKSPACE_MEMBERSHIP_KEYS = /^\s*(members|exclude|default-members)\s*=\s*\[[\s\S]*?\]\s*(#.*)?$/gm;
// `serde = "1.0.2"` and `serde = { version = "1.0.2", ... }` with the version neutralised, so two dependency tables compare equal when
// only versions differ.
const versionNeutral = (t) => String(t ?? "").replace(/(\bversion\s*=\s*)"[^"]*"/g, '$1"*"').replace(/^(\s*[\w.-]+\s*=\s*)"[^"]*"/gm, '$1"*"');

/**
 * True when every difference between two root manifests lives in a lock-reflected table (above), so the lock graph already says
 * which crates the change reaches. False for any other difference, a malformed manifest, or no difference at all.
 */
export function manifestChangeIsLockReflected(before, after) {
  if (typeof before !== "string" || typeof after !== "string" || before === after) return false;
  const a = manifestTables(before); const b = manifestTables(after);
  // A comment-only line and trailing blank lines change no build; a trailing `# ...` on a value line is kept (a `#` may sit in a string).
  const text = (t) => String(t ?? "").split("\n").filter((l) => !/^\s*(#.*)?$/.test(l)).join("\n");
  const strip = (t) => text(t).replace(WORKSPACE_MEMBERSHIP_KEYS, "").trim();
  for (const name of new Set([...a.keys(), ...b.keys()])) {
    if (text(a.get(name)) === text(b.get(name))) continue;
    if (LOCK_REFLECTED_TABLE.test(name)) continue;
    if (name === "workspace" && strip(a.get(name)) === strip(b.get(name))) continue;
    if (name === "workspace.dependencies" && versionNeutral(text(a.get(name))) === versionNeutral(text(b.get(name)))) continue;
    return false;
  }
  return true;
}

/**
 * Every Cargo workspace of the repository. A tracked Cargo.toml is a workspace root, or a standalone package not inside another
 * workspace, unless an earlier (shallower) workspace already lists it as a member; each root is read with its own
 * `cargo metadata --no-deps --locked` (cwd = its directory, through the broker). A manifest below another workspace that is not
 * a member is tried the same way (an `exclude`d or self-contained crate is a real workspace); when cargo refuses it there it is a
 * fixture or vendored crate, reported in `skipped` and not scanned. A manifest outside every workspace that cargo refuses throws.
 * A member whose Cargo.toml is not tracked (untracked or ignored) is dropped into `skipped`, as are `untracked()` manifests.
 * Returns { present, workspaces: [{ cargoDir, units }], units, cargoDir, skipped: [{ manifest, why }] }; `units` is every workspace's
 * [{ name, crate, dir, publish, cargoDir }] and `cargoDir` the first workspace's (the repository root's when it has one).
 * `metadata(dir)` (optional) replaces the cargo call with a reader of the same { cargoDir, members } shape (extra fields are ignored).
 */
export function discoverRustWorkspaces({ root, spawn, cargoCommand, tracked, untracked = () => [], realpath = fs.realpathSync, fixtures = [], readFile = (f) => fs.readFileSync(f, "utf8"), metadata = null }) {
  const manifests = tracked("Cargo.toml");
  const trackedSet = new Set(manifests);
  const locks = new Set(tracked("Cargo.lock"));
  const skipped = [];
  for (const m of untracked()) if (!trackedSet.has(m)) skipped.push({ manifest: m, why: "untracked (not committed, so not part of the gated commit)" });
  if (!manifests.length) return { present: false, workspaces: [], units: [], cargoDir: ".", skipped };
  const fixtureDirs = fixtures.map((d) => posix(String(d)).replace(/\/+$/, "")).filter(Boolean);
  const underFixture = (dir) => fixtureDirs.find((f) => dir === f || dir.startsWith(`${f}/`));
  const workspaces = []; const covered = new Set();
  for (const m of [...manifests].sort(byDepthThenName)) {
    if (covered.has(m)) continue;
    const dir = dirOfManifest(m);
    const parent = workspaces.find((w) => w.cargoDir === "." || dir === w.cargoDir || dir.startsWith(`${w.cargoDir}/`));
    // A manifest under a declared fixture directory (ci/gate.config.json `fixtures`) is a consumer or sample crate: it is gated by the
    // consumer rows of the crate it exercises, never as a unit of its own. Nothing under it is asked of cargo.
    const fixture = underFixture(dir);
    if (fixture) { skipped.push({ manifest: m, why: `under the fixture directory ${fixture} (ci/gate.config.json fixtures); gated through the consumer rows of the crate it exercises` }); continue; }
    // A manifest the parent workspace's own manifest `exclude`s (a vendored or patched crate) is compiled only as a dependency of the
    // parent's members: the parent's lock closure gates it, so it is no unit of its own.
    if (parent) {
      const excluded = workspaceExcludes(readFile, root, parent.cargoDir).find((x) => dir === x || dir.startsWith(`${x}/`));
      if (excluded) { skipped.push({ manifest: m, why: `excluded from the Cargo workspace ${parent.cargoDir} by its manifest (${excluded}); it compiles only inside that workspace's lock closure, which gates it` }); continue; }
      // A nested manifest with no committed Cargo.lock has no reproducible build to gate: its dependency resolution is whatever the
      // registry answers today. It is a fixture or sample; the crate it exercises carries the consumer row that compiles it.
      if (!locks.has(`${dir}/Cargo.lock`)) { skipped.push({ manifest: m, why: `below the Cargo workspace ${parent.cargoDir} with no committed Cargo.lock, so its build is not reproducible; gated through the consumer rows of the crate it exercises` }); continue; }
    }
    let found;
    try { found = metadata ? metadata(dir) : workspaceMetadata({ root, spawn, cargoCommand, dir, realpath }); } catch (e) {
      if (!parent) throw e;
      skipped.push({ manifest: m, why: `below the Cargo workspace ${parent.cargoDir} but not one of its members, and cargo refuses it on its own (${String(e.message).replace(/^cargo metadata failed: /, "").replace(/ \(in [^)]*\)$/, "")})` });
      continue;
    }
    if (workspaces.some((w) => w.cargoDir === found.cargoDir)) { covered.add(m); continue; }
    const units = [];
    // The workspace's own root manifest counts as covered even when it is virtual.
    covered.add(found.cargoDir === "." ? "Cargo.toml" : `${found.cargoDir}/Cargo.toml`);
    for (const member of found.members) {
      covered.add(member.manifest);
      if (!trackedSet.has(member.manifest)) { skipped.push({ manifest: member.manifest, why: "a workspace member that is untracked or ignored, so it is not part of the gated commit" }); continue; }
      units.push({ name: rustUnitName(found.cargoDir, member.name), crate: member.name, dir: member.dir, publish: member.publish, cargoDir: found.cargoDir, bins: member.bins ?? [], testLinksBins: member.testLinksBins === true });
    }
    workspaces.push({ cargoDir: found.cargoDir, units });
  }
  workspaces.sort((a, b) => (a.cargoDir === "." ? -1 : b.cargoDir === "." ? 1 : a.cargoDir < b.cargoDir ? -1 : 1));
  const units = workspaces.flatMap((w) => w.units);
  const names = new Map();
  for (const u of units) { if (names.has(u.name)) throw new Error(`two Cargo workspaces both define the unit ${u.name}`); names.set(u.name, true); }
  return { present: true, workspaces, units, cargoDir: workspaces[0]?.cargoDir ?? ".", skipped };
}

// ---- pre-push input and lock scope ------------------------------------------------------------------------

/**
 * git's pre-push stdin: one "<local ref> <local sha> <remote ref> <remote sha>" per line, the four fields joined by single ASCII spaces.
 * Git forbids an ASCII space in a ref name but allows Unicode whitespace (U+00A0, U+2003, U+3000, ...), so the split is on " " alone: a
 * whitespace split would cut such a ref into extra fields and the line would vanish. Blank lines are skipped; any other line that is not
 * four non-empty fields is returned in `malformed` ({ number, line }), never dropped. Returns { lines: [{ localRef, localSha, remoteRef,
 * remoteSha }], malformed }.
 */
export function parsePushLines(stdin) {
  const lines = []; const malformed = [];
  String(stdin ?? "").split("\n").forEach((raw, index) => {
    const line = raw.replace(/\r$/, "");
    if (line === "") return;
    const f = line.split(" ");
    if (f.length !== 4 || f.some((x) => x === "")) { malformed.push({ number: index + 1, line }); return; }
    lines.push({ localRef: f[0], localSha: f[1], remoteRef: f[2], remoteSha: f[3] });
  });
  return { lines, malformed };
}

/**
 * The crates a lock-reflected workspace change reaches (the lock graph CI's selection uses): a Cargo.lock change, a vendor/ change
 * (the vendored crate is a lock package), and a workspace manifest change confined to the tables the lock also records
 * (manifestChangeIsLockReflected: [patch.*], [workspace.dependencies], membership). null when any other workspace Rust file or
 * manifest table changed or the base has no lock: every crate stays in scope. `readManifest` reads the gated commit's manifest.
 */
export function lockScope({ changed, cargoDir = ".", unitNames, showBase, readLock, readManifest = null }) {
  // Rust configuration of any ancestor directory (repository .cargo, intermediate .cargo, toolchain, clippy, rustfmt) reaches
  // every crate of the workspace, whatever the lock graph says.
  if (ancestorConfigChanged(cargoDir, changed)) return null;
  const rel = (f) => (cargoDir === "." ? f : f.startsWith(`${cargoDir}/`) ? f.slice(cargoDir.length + 1) : null);
  const wide = changed.map((f) => [f, rel(f)]).filter(([, r]) => r !== null && WORKSPACE_RUST_FILE.test(r));
  if (!wide.length || wide.some(([, r]) => r !== "Cargo.lock" && r !== "Cargo.toml" && !r.startsWith("vendor/"))) return null;
  const inWs = (f) => (cargoDir === "." ? f : `${cargoDir}/${f}`);
  if (wide.some(([, r]) => r === "Cargo.toml")) {
    if (typeof readManifest !== "function") return null;
    let after;
    try { after = readManifest(); } catch { return null; }
    if (!manifestChangeIsLockReflected(showBase(inWs("Cargo.toml")), after)) return null;
  }
  const before = showBase(inWs("Cargo.lock"));
  if (before === null) return null;
  let nextText;
  try { nextText = readLock(); } catch { return null; }
  const next = parseLock(nextText);
  const touched = changedLockNames(parseLock(before), next);
  // A member the lock no longer lists was removed: nothing left to scan for it. A package present before and absent now reaches
  // nothing; a package new now is in `touched` already.
  for (const [, r] of wide) if (r.startsWith("vendor/")) touched.add(r.split("/")[1]);
  const memo = new Map(); const scope = new Set();
  for (const [name] of next) {
    if (!unitNames.has(name)) continue;
    if (touched.has(name) || [...touched].some((t) => reach(next, name, memo).has(t))) scope.add(name);
  }
  return scope;
}

// ---- result cache inputs ------------------------------------------------------------------------------------

// The build inputs that change how every crate compiles, relative to the Cargo workspace root, plus the gate's own config.
const SHARED_RUST_INPUTS = ["Cargo.lock", "Cargo.toml", "rust-toolchain", "rust-toolchain.toml", ".cargo", "deny.toml", "clippy.toml", ".clippy.toml", "rustfmt.toml", ".rustfmt.toml", "vendor"];
const GATE_CONFIG = "ci/gate.config.json";
// What cargo, clippy, rustfmt and cargo-deny also read from EVERY ancestor directory of the workspace (the repository root and each
// directory between it and the workspace: they search upward), relative to that directory.
const ANCESTOR_RUST_INPUTS = [".cargo", "rust-toolchain", "rust-toolchain.toml", "clippy.toml", ".clippy.toml", "rustfmt.toml", ".rustfmt.toml", "deny.toml"];

/**
 * The unit a cargo scan id belongs to (clippy:x, check:x, check-features:x:0, check-dependent:x, doc:x, test:rust:x,
 * test:rust-serial:x:1), else null. A unit below the root is named `<cargoDir>:<crate>`, so the colon no longer ends the name:
 * pass `known` (the set of unit names) and the longest known name wins, with or without the trailing feature-set index. Without
 * `known` the name is the first segment, as for a repository with one root workspace.
 */
export function rustUnitOfScanId(id, known = null) {
  // A cross-target scan id ends in `@<target triple>` (cross-targets.mjs); a crate name never contains `@`.
  const parts = String(id).replace(/@[^@:]+$/, "").split(":");
  let rest;
  if (["clippy", "check", "check-features", "check-dependent", "doc"].includes(parts[0])) rest = parts.slice(1);
  else if (parts[0] === "test" && (parts[1] === "rust" || parts[1] === "rust-serial")) rest = parts.slice(2);
  // A publisher row (`publisher:cargo:<crate>:<n>`) reads the same crate closure as that crate's own rows (NN02: an unchanged crate reruns none of them).
  else if (parts[0] === "publisher" && parts[1] === "cargo" && /^\d+$/.test(parts[parts.length - 1] ?? "")) rest = parts.slice(2, -1);
  else return null;
  if (!(known instanceof Set)) return rest[0] ?? null;
  for (let take = rest.length; take >= 1; take -= 1) { const name = rest.slice(0, take).join(":"); if (known.has(name)) return name; }
  return null;
}

// Compile scans (clippy, check, check-features, check-dependent, doc) read the crate's closure plus whatever its sources pull
// in from outside it: include!, include_str!, include_bytes!, a path literal with "../", and a build.rs that reads files.
const COMPILE_SCAN_PREFIXES = new Set(["clippy", "check", "check-features", "check-dependent", "doc"]);
const INCLUDE_MACRO = /\binclude(?:_str|_bytes)?\s*!\s*\(/g;
const STRING_BODY = String.raw`((?:[^"\\\n]|\\.)*)`;
const INCLUDE_LITERAL = new RegExp(String.raw`^\s*"` + STRING_BODY + String.raw`"\s*[,)]`);
const INCLUDE_MANIFEST = new RegExp(String.raw`^\s*concat!\s*\(\s*env!\s*\(\s*"CARGO_MANIFEST_DIR"\s*\)\s*,\s*"` + STRING_BODY + String.raw`"\s*\)\s*[,)]`);
const INCLUDE_OUT_DIR = /^\s*concat!\s*\(\s*env!\s*\(\s*"OUT_DIR"\s*\)/;
// Any path-like token holding a "../" segment, inside a string literal or not: a stray quote cannot hide one.
const PARENT_PATH = /(?:[\w@.-]+[\\/])*\.\.(?:[\\/][\w@.-]*)+/g;
const BUILD_READS_FILES = /\b(?:fs::(?:read|read_to_string|read_dir|metadata|File)|File::open|OpenOptions|read_to_string|read_dir|walkdir|glob)\b/;
const BUILD_LEAVES_CRATE = /\.(?:parent|ancestors|pop)\s*\(|CARGO_WORKSPACE_DIR|workspace_root|current_dir/;
const RERUN_LITERAL = /cargo::?rerun-if-changed=([^"\\{}\n]+)/g;
// `#[path = "x.rs"]` on a module, and a plain `mod x;` declaration (no body): both name another source file.
const PATH_ATTRIBUTE = new RegExp(String.raw`#\s*!?\s*\[\s*path\s*=\s*"` + STRING_BODY + String.raw`"\s*\]`, "g");
const MOD_DECLARATION = /(?:^|[;{}\s\]])(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;/g;
// The most source files one crate's traversal reads before it gives up and keys the scan on the whole tree.
export const EXTERNAL_INPUT_FILE_LIMIT = 10_000;

const within = (dir, p) => dir === "." || p === dir || p.startsWith(`${dir}/`);

/**
 * Repository paths a crate's sources reach OUTSIDE their own directory, found statically: the literal targets of include!,
 * include_str! and include_bytes! (relative to the including file, or to CARGO_MANIFEST_DIR through concat!), every string
 * literal containing "../" (tried against the including file's and the crate's directory; only paths that exist count), and the
 * rerun-if-changed literals of a build.rs that reads files. `unresolved` names the first reference that cannot be resolved
 * statically (a computed include path, a build.rs that reads files and walks out of its crate, a path that leaves the
 * repository, an unreadable file, more than EXTERNAL_INPUT_FILE_LIMIT files); the caller then keys on the whole tree. `exists(path)`
 * says whether a repo path exists.
 *
 * The traversal runs to a fixed point: every out-of-crate .rs file reached is itself scanned for the same references (include!,
 * include_str!, include_bytes!, `#[path = ".."]`, `mod x;` declarations of an out-of-crate file, "../" literals), whatever the
 * depth (lib.rs -> shared/one.rs -> shared/two.rs -> shared/three.rs reaches three.rs), with a visited set so cycles end.
 */
// A source (a test, usually) that navigates out of its crate at run time without naming the path: the workspace root, the
// current directory, a bare ".." segment, or the manifest directory's parent. Its tests may read anything in the repository.
// A comparison with ".." (`name != ".."`), a method whose name only ends in current_dir (`get_current_dir()`), `.ancestors()` of a runtime path and
// comment text do not leave the crate; they keyed whole crates' tests on the whole tree (rightkit-fs, rightkit-child-process; NN20, 2026-11-06).
const TEST_LEAVES_CRATE = /CARGO_WORKSPACE_DIR|workspace_root|(?<![A-Za-z0-9_])current_dir\s*\(|(?<![=!]=\s{0,4})"\.\.\/?"|CARGO_MANIFEST_DIR[^;\n]{0,120}\.(?:parent|ancestors)\s*\(/;

export function externalInputs({ dir, files, readBlob, exists, includeRuntimePaths = true }) {
  const paths = new Set(); let unresolved = null; let leavesCrate = null;
  const stop = (why) => { unresolved ??= why; };
  // Resolve `rel` against each base in turn; the path counts when it exists inside the repository and outside the crate. A
  // reference that leaves the repository from EVERY base is unresolvable (a path literal is tried against two bases, only
  // one of which is the one the code means).
  const consider = (from, bases, rel) => {
    const raw = String(rel).replaceAll("\\", "/").replace(/^\/+/, "");
    if (!raw || /^[A-Za-z]:/.test(raw)) return;
    let escapes = 0;
    for (const base of bases) {
      const resolved = path.posix.normalize(path.posix.join(base, raw)).replace(/\/+$/, "");
      if (resolved === ".." || resolved.startsWith("../")) { escapes += 1; continue; }
      if (within(dir, resolved) || paths.has(resolved)) continue;
      if (exists(resolved)) { paths.add(resolved); if (resolved.endsWith(".rs")) enqueue(resolved, { external: true }); }
    }
    if (escapes === bases.length) stop(`${from}: ${JSON.stringify(rel)} leaves the repository`);
  };
  const seen = new Set(); const queue = [];
  const enqueue = (file, options = {}) => {
    if (seen.has(file)) return;
    if (seen.size >= EXTERNAL_INPUT_FILE_LIMIT) { stop(`more than ${EXTERNAL_INPUT_FILE_LIMIT} source files are reachable`); return; }
    seen.add(file); queue.push([file, options]);
  };
  const scan = (file, { build = false, external = false } = {}) => {
    let text;
    try { text = readBlob(file); } catch (e) { stop(`${file}: unreadable (${e.message})`); return; }
    if (typeof text !== "string") { stop(`${file}: unreadable`); return; }
    const fileDir = path.posix.dirname(file);
    for (const m of text.matchAll(INCLUDE_MACRO)) {
      const rest = text.slice(m.index + m[0].length, m.index + m[0].length + 600);
      let hit;
      if ((hit = INCLUDE_LITERAL.exec(rest))) consider(file, [fileDir], hit[1]);
      else if ((hit = INCLUDE_MANIFEST.exec(rest))) consider(file, [dir], hit[1]);
      else if (INCLUDE_OUT_DIR.test(rest)) continue;
      else stop(`${file}: include macro with a computed path`);
    }
    // Publication content follows compile inputs; cache callers also retain conservative runtime-path reach.
    if (includeRuntimePaths || build) for (const m of text.matchAll(PARENT_PATH)) {
      consider(file, [fileDir, dir], m[0]);
    }
    // A module attribute names its file relative to the including file's directory (tried against the crate directory too).
    for (const m of text.matchAll(PATH_ATTRIBUTE)) consider(file, [fileDir, dir], m[1]);
    // An out-of-crate file's own `mod x;` declarations: x.rs / x/mod.rs beside a mod-rs file (mod.rs, lib.rs, main.rs), else under the file's own directory.
    if (external) {
      const base = path.posix.basename(file); const stem = base.slice(0, -3);
      const home = ["mod.rs", "lib.rs", "main.rs"].includes(base) ? fileDir : path.posix.join(fileDir, stem);
      for (const m of text.matchAll(MOD_DECLARATION)) for (const candidate of [`${m[1]}.rs`, `${m[1]}/mod.rs`]) consider(file, [home], candidate);
    }
    if (build && BUILD_READS_FILES.test(text)) {
      for (const m of text.matchAll(RERUN_LITERAL)) consider(file, [dir], m[1].trim());
      if (BUILD_LEAVES_CRATE.test(text)) stop(`${file}: a build script that reads files and navigates out of its crate`);
    }
    // Only TEST code can make a test read files outside the crate: an integration test or bench file, or the #[cfg(test)] part of a source file.
    // Library code that resolves a caller's relative path (std::env::current_dir) does not (rightkit-fs; it keyed every dependent's tests on the whole tree, NN20).
    if (includeRuntimePaths && !build && !external && leavesCrate === null) {
      const at = text.indexOf("#[cfg(test)]");
      const testText = /(^|\/)(?:tests|benches)\//.test(file) ? text : at >= 0 ? text.slice(at) : "";
      if (testText && TEST_LEAVES_CRATE.test(testText.replace(/\/\/[^\n]*/g, ""))) leavesCrate = file;
    }
  };
  for (const file of files) if (file.endsWith(".rs")) enqueue(file, { build: path.posix.basename(file) === "build.rs" && path.posix.dirname(file) === dir });
  // Fixed point: an out-of-crate .rs file that is reached is itself a source, so its own references are followed, and theirs.
  while (queue.length) { const [file, options] = queue.pop(); scan(file, options); }
  // `leavesCrate`: the first in-crate source that navigates out of the crate at run time without a literal path (tests key on
  // the whole tree then, unless ci/gate.config.json testInputs names what they read). Compile scans ignore it.
  return { paths: [...paths].sort(), unresolved, leavesCrate };
}

/**
 * The root manifest as a compile input: every table except `[patch.*]` (a patch changes a lock package's source, which the lock
 * projection records) and the membership arrays of `[workspace]` (a member is a lock package); comment-only lines dropped.
 * `[workspace.dependencies]` stays whole: its feature lists reach every `workspace = true` dependent without touching the lock.
 */
export function manifestCompileProjection(text) {
  const out = {};
  const strip = (t) => String(t ?? "").split("\n").filter((l) => !/^\s*(#.*)?$/.test(l)).join("\n");
  for (const [name, body] of manifestTables(text)) {
    if (/^patch\./.test(name)) continue;
    const kept = (name === "workspace" ? strip(body).replace(WORKSPACE_MEMBERSHIP_KEYS, "") : strip(body)).trim();
    if (kept) out[name] = kept;
  }
  return out;
}

const plainRelative = (p) => typeof p === "string" && p !== "" && !path.posix.isAbsolute(p) && !/^[A-Za-z]:/.test(p) && !p.includes("\\") && path.posix.normalize(p) === p && p !== "." && p !== ".." && !p.startsWith("../");

/**
 * The input digest function for the result cache: scan => string, or null when the scan must not be cached.
 * `objectId(path)` is the git blob/tree id of a repo-relative path in the gated commit, or null when the path is absent
 * (it throws when git cannot answer, which makes the scan uncacheable); `headTree` is that commit's tree id.
 * `listFiles(dir)` lists the gated commit's files under a directory and `readBlob(path)` reads one (both are required for any
 * per-crate key: without them a crate's reach outside its own directory is unknown and the whole tree is used).
 *
 * - A compile scan (clippy, check, check-features, check-dependent, doc) of crate X digests the object ids of X and of every
 *   workspace member in X's lock closure, the shared build inputs projected to what X consumes (the lock entries of X's closure,
 *   the root manifest without its [patch] tables and membership arrays, the `.cargo` files except audit.toml, the toolchain,
 *   clippy and rustfmt files of the workspace and every ancestor, the vendored crates in the closure, X's own ci/gate.config.json
 *   entry) and every repository path those members' sources reach outside their own directories (externalInputs). A reference
 *   that cannot be resolved statically keys the scan on the whole tree. deny.toml is no compile input (it feeds class A, never cached).
 * - A Rust test scan (test:rust*, test:rust-serial*) is keyed the same way, plus ci/gate.config.json `rust.<crate>.testInputs`
 *   when declared. Without a declaration, a closure source that navigates out of its crate at run time without a literal path
 *   (externalInputs `leavesCrate`: workspace root, current_dir, a bare "..", the manifest directory's parent) keys the scan on the
 *   whole HEAD tree, since its tests may read anything.
 * - An npm package scan (test:npm, tsc, eslint, biome) keys on the package's workspace closure (every dependency that is a workspace
 *   package, whatever its spec, transitively, by package name) and the root npm inputs; (the scripts/ci self-test scan no longer exists: Adrian, 2026-11-03, end-to-end tests only). Every other class B scan (generated-docs, swift-build) uses the whole tree.
 * - Class A: pnpm-lockfile and advisory-exceptions are never cached (live answers). cargo-deny, cargo-audit and pnpm-audit are cached by
 *   their inputs and the identity of the advisory data they read (rightkit#1464, ADR-001 C5; advisoryKey below): a Cargo workspace's lock
 *   blob, every Cargo.toml of the commit, the deny and cargo configuration files, and the pinned RustSec commit (cargo-deny and cargo-audit
 *   read exactly that commit); a pnpm root's lock, manifests and npm configuration. cargo-deny also reads the crate index (yanked
 *   versions) and pnpm audit asks the registry, neither of which a commit pins, so those keys carry the UTC day: a rerun the same day is a
 *   hit, the next day is a run. A changed pin, lock, manifest or configuration is a different key; rustfmt,
 *   node-check, actionlint and the parse scans key on their files' blob ids; cargo-package-list on the crate, the manifest projection
 *   and its lock entry; the rest on the whole tree.
 */
// Root npm inputs every package scan reads: the install graph and the shared tool configuration.
const SHARED_NPM_INPUTS = ["package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml", ".npmrc", "tsconfig.json", "tsconfig.base.json", "eslint.config.js", "eslint.config.mjs", ".eslintrc.json", "biome.json", "vitest.config.ts", "vitest.config.mjs", "vitest.workspace.ts"];
// ESLint and Biome look their configuration up from the package directory through every ancestor. A config in an ancestor directory (or of another
// name than the shared list above) is part of a package's key only when the commit has it, so a repository without one keeps the key it had.
const LINT_CONFIG_NAMES = [...ESLINT_CONFIG_FILES, ...BIOME_CONFIG_FILES];
// The gate's own test suite reads the CI scripts, the gate config and the release package it imports from.

/** ci/shards.json `solo` (crate names that always get an invocation of their own), read from the gated tree; empty when the file is absent or unreadable. */
function shardSoloOf(o, repo) {
  try {
    const solo = JSON.parse(o.readFile(path.join(repo, "ci", "shards.json"))).solo;
    return Array.isArray(solo) ? solo.filter((n) => typeof n === "string") : [];
  } catch { return []; }
}

export function buildCacheDigest({ objectId, headTree, rustUnits = [], npmUnits = [], cargoDir = ".", rustLock = null, rustLocks = null, crateFeatures = {}, listFiles = null, readBlob = null, today = () => new Date().toISOString().slice(0, 10), onKeyError = null }) {
  const memo = new Map(); const reachMemos = new Map(); const externalMemo = new Map();
  const idOf = (p) => { if (!memo.has(p)) memo.set(p, p === "." ? headTree : objectId(p)); return memo.get(p); };
  // npm: a package's test, tsc and eslint scans read the package, every workspace package it depends on (package.json
  // `workspace:` dependencies, transitively) and the root npm inputs. Package names come from the manifests themselves.
  const npmByUnit = new Map(npmUnits.map((u) => [u.name, u]));
  const npmManifestMemo = new Map();
  const npmManifest = (u) => {
    if (!npmManifestMemo.has(u.dir)) {
      let m = null;
      try { m = JSON.parse(String(readBlob(u.dir === "." ? "package.json" : `${u.dir}/package.json`)).replace(/^﻿/, "")); } catch { m = null; }
      npmManifestMemo.set(u.dir, m);
    }
    return npmManifestMemo.get(u.dir);
  };
  let npmByPkgName = null;
  const npmClosure = (unit) => {
    if (npmByPkgName === null) { npmByPkgName = new Map(); for (const u of npmUnits) { const m = npmManifest(u); if (m?.name) npmByPkgName.set(`${u.root ?? "."}\0${m.name}`, u); } }
    const seen = new Map([[unit.name, unit]]); const stack = [unit]; let unresolved = false;
    while (stack.length) {
      const u = stack.pop(); const m = npmManifest(u);
      if (!m) { unresolved = true; continue; }
      for (const section of ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"]) {
        for (const [name, spec] of Object.entries(m[section] ?? {})) {
          // A workspace package is a member whatever the spec: pnpm links it for a version range too (linkWorkspacePackages), and the
          // tsc row checks it from its committed source (deterministic-scans.mjs TSC_PROJECT). Only an explicit local spec must resolve.
          const dep = npmByPkgName.get(`${u.root ?? "."}\0${name}`);
          if (!dep) { if (/^(workspace:|link:|file:)/.test(String(spec))) unresolved = true; continue; }
          if (!seen.has(dep.name)) { seen.set(dep.name, dep); stack.push(dep); }
        }
      }
    }
    return { members: [...seen.values()].sort((a, b) => (a.name < b.name ? -1 : 1)), unresolved };
  };
  // The npm inputs shared by a set of units: the repository root's (always, null when absent: the key a one-root repository has always had),
  // the same files of every other pnpm root the units belong to, and the linter configuration of their ancestor directories that the commit has.
  const sharedNpmInputs = (members) => {
    const required = new Set(SHARED_NPM_INPUTS); const optional = new Set();
    for (const m of members) {
      if ((m.root ?? ".") !== ".") for (const p of SHARED_NPM_INPUTS) required.add(inWs(m.root, p));
      for (const dir of ancestorDirsOf(m.dir)) if (dir !== m.dir) for (const f of LINT_CONFIG_NAMES) optional.add(inWs(dir, f));
    }
    const nested = [...required].filter((p) => !SHARED_NPM_INPUTS.includes(p)).sort();
    const extra = [...optional].filter((p) => !required.has(p)).sort();
    return [...SHARED_NPM_INPUTS, ...nested].map((p) => [p, idOf(p)]).concat(extra.map((p) => [p, idOf(p)]).filter(([, id]) => id !== null && id !== undefined));
  };
  // A unit belongs to the Cargo workspace it names (rustUnits[].cargoDir, else the global one) and is a package of that workspace's
  // own lock under its crate name (rustUnits[].crate, else its name); its closure never crosses into another workspace.
  const wsOf = (u) => u.cargoDir ?? cargoDir;
  const crateOf = (u) => u.crate ?? u.name;
  const lockFor = (ws) => (rustLocks?.[ws] instanceof Map ? rustLocks[ws] : ws === cargoDir && rustLock instanceof Map ? rustLock : null);
  const inWs = (ws, f) => (ws === "." ? f : `${ws}/${f}`);
  const byName = new Map(rustUnits.map((u) => [u.name, u]));
  const known = new Set(byName.keys());
  const byCrate = new Map(rustUnits.map((u) => [`${wsOf(u)}\0${crateOf(u)}`, u]));
  const siblingByCrate = new Map(rustUnits.filter((u) => wsOf(u) === `crates/${crateOf(u)}`).map((u) => [crateOf(u), u]));
  const whole = () => canonicalJson({ kind: "tree", tree: headTree });
  const externalOf = (member) => {
    if (!externalMemo.has(member.dir)) {
      externalMemo.set(member.dir, externalInputs({ dir: member.dir, files: listFiles(member.dir), readBlob, exists: (p) => { const id = idOf(p); return id !== null && id !== undefined; } }));
    }
    return externalMemo.get(member.dir);
  };
  // Class A: the live checks are never cached (their answer depends on the advisory databases and the registry of the day); a
  // content scan with an explicit file list is keyed on those files' blob ids; a crate's package list on the crate, its manifest
  // projection and its own lock entry; anything else (cargo metadata, licence, ownership catalog, secret scan) on the whole tree,
  // so it is reused for a rerun of the same commit and nothing else.
  const liveScan = (id) => /^(cargo-deny|cargo-audit|pnpm-audit|pnpm-lockfile|advisory-exceptions)(:|$)/.test(id);
  // GC-A24: pnpm-lockfile asks the registry as pnpm-audit does, so it takes the same day-bounded key (GC-A19).
  // GC-A24 table: ghsa-audit and advisory-exceptions take GC-A19 keys (their inputs plus the UTC day) like cargo-deny and cargo-audit.
  const ADVISORY_SCAN = /^(cargo-deny|cargo-audit|pnpm-audit|pnpm-lockfile|ghsa-audit|advisory-exceptions)(:|$)/;
  // The key of an advisory scan (rightkit#1464), or null when an input cannot be identified (the scan then runs). Every input that decides the
  // verdict and is not in the scan's id, command or environment is here; the advisory database's commit (cargo) is in the scan's own environment
  // (RK_ADVISORY_DB_COMMIT) and its pin object, the gate's code and advisory-db.json are in the environment digest the caller appends.
  const advisoryKey = (scan) => {
    if (typeof listFiles !== "function") return null;
    const dir = typeof scan.cwd === "string" && scan.cwd !== "" ? scan.cwd : ".";
    const idsOf = (paths) => [...new Set(paths)].sort().map((p) => [p, idOf(p) ?? null]);
    const all = listFiles(".");
    if (/^pnpm-(audit|lockfile)(:|$)/.test(scan.id)) {
      const lock = inWs(dir, "pnpm-lock.yaml");
      if (idOf(lock) === null || idOf(lock) === undefined) return null;
      const manifests = all.filter((f) => /(^|\/)package\.json$/.test(f) && (dir === "." || f === inWs(dir, "package.json") || f.startsWith(`${dir}/`)));
      return canonicalJson({ kind: "pnpm-audit", id: scan.id, day: today(), inputs: idsOf([lock, inWs(dir, "pnpm-workspace.yaml"), inWs(dir, ".npmrc"), ".npmrc", ...manifests]) });
    }
    if (scan.internal === "ghsa-audit") {
      // The verdict reads the committed lockfile and OSV's answer of the day (ghsa-audit.mjs); its rule code is the checker identity.
      if (typeof scan.lockFile !== "string" || idOf(scan.lockFile) === null || idOf(scan.lockFile) === undefined) return null;
      return canonicalJson({ kind: "ghsa-audit", id: scan.id, day: today(), inputs: idsOf([scan.lockFile]) });
    }
    if (scan.internal === "advisory-exceptions") {
      // Exceptions must match deny.toml, .cargo/audit.toml and the locked versions, and an expired one fails, so the day is part of the key.
      const at = typeof scan.dir === "string" && scan.dir !== "" ? scan.dir : ".";
      const files = [inWs(at, "deny.toml"), inWs(at, ".cargo/audit.toml"), inWs(at, "Cargo.lock"), "ci/advisory-exceptions.json"];
      if (typeof scan.exceptionsFile === "string") files.push(inWs(at, scan.exceptionsFile));
      return canonicalJson({ kind: "advisory-exceptions", id: scan.id, day: today(), inputs: idsOf(files) });
    }
    const lock = inWs(dir, "Cargo.lock");
    if (idOf(lock) === null || idOf(lock) === undefined) return null;
    const ancestors = ancestorDirsOf(dir);
    const config = [dir, ...ancestors].flatMap((d) => [".cargo", "deny.toml", "audit.toml", "rust-toolchain", "rust-toolchain.toml"].map((f) => inWs(d, f)));
    const manifests = all.filter((f) => /(^|\/)Cargo\.toml$/.test(f));
    return canonicalJson({ kind: scan.id.startsWith("cargo-deny") ? "cargo-deny" : "cargo-audit", id: scan.id, day: today(), pin: scan.advisoryPin ?? null, inputs: idsOf([lock, ...manifests, ...config]) });
  };
  const fileList = (scan) => {
    if (Array.isArray(scan.files) && scan.files.length) return scan.files;
    const args = Array.isArray(scan.args) ? scan.args : [];
    if (/^rustfmt:/.test(scan.id)) {
      const rs = args.filter((x) => /\.rs$/.test(x));
      const configs = new Set(); for (const f of rs) for (const d of ancestorDirsOf(path.posix.dirname(f))) for (const c of ["rustfmt.toml", ".rustfmt.toml"]) configs.add(inWs(d, c));
      return [...rs, ...configs];
    }
    if (/^cargo-metadata(:|$)/.test(scan.id) && typeof listFiles === "function") {
      // GC-A24 table: every Cargo.toml, Cargo.lock, .cargo and toolchain file of the workspace and its ancestors, so an edit in one
      // workspace never reruns another's metadata (GC-A19; acceptance run 3 saw app/engine rerun for an rk edit).
      const ws = typeof scan.cwd === "string" && scan.cwd !== "" ? scan.cwd : ".";
      const cargoFile = (f) => /(^|\/)(Cargo\.toml|Cargo\.lock|rust-toolchain|rust-toolchain\.toml)$/.test(f) || /(^|\/)\.cargo\//.test(f);
      const own = listFiles(ws).filter(cargoFile);
      const above = ancestorDirsOf(ws).flatMap((d) => ["Cargo.toml", "Cargo.lock", "rust-toolchain", "rust-toolchain.toml", ".cargo/config", ".cargo/config.toml"].map((f) => inWs(d, f)));
      return [...own, ...above];
    }
    // feature-coverage reads each planned crate's Cargo.toml and the sets ci/gate.config.json declares for it.
    if (scan.internal === "feature-coverage" && Array.isArray(scan.crates)) return [...scan.crates.map((c) => c.manifest).filter((m) => typeof m === "string"), "ci/gate.config.json"];
    if (/^oxlint:/.test(scan.id) && typeof listFiles === "function" && typeof scan.cwd === "string" && scan.cwd !== "") {
      // Oxlint reads the package's own files and the nearest config at or above it; the enrolled binary is in the row's tool identity.
      return [...listFiles(scan.cwd), ...ancestorDirsOf(scan.cwd).flatMap((d) => OXLINT_CONFIG_FILES.map((f) => inWs(d, f)))];
    }
    if (/^node-check:/.test(scan.id)) { const f = scan.id.slice("node-check:".length); return [f, ...ancestorDirsOf(path.posix.dirname(f)).map((d) => inWs(d, "package.json"))]; }
    if (scan.internal === "secret-scan" && Array.isArray(scan.dirs) && typeof listFiles === "function") return [...new Set(scan.dirs.flatMap((d) => listFiles(d)))];
    // The lock checks read the lock and manifest files of the one directory their id names (NN20: on the whole tree, any commit reran them).
    { const m = /^(?:gradle-lock|python-lock):(.+)$/.exec(scan.id); if (m && typeof listFiles === "function") return listFiles(m[1]); }
    if (scan.id === "shellcheck") return [...args.filter((x) => /\.(sh|bash)$/.test(x)), ".shellcheckrc"];
    if (scan.id === "actionlint") return [...args.filter((x) => /\.ya?ml$/.test(x)), ".github/actionlint.yaml"];
    return null;
  };
  // npm package scans (class A tsc, eslint, biome; class B test:npm): the package's workspace closure plus the root npm inputs, or
  // undefined when the scan is not one of them or its closure does not resolve (the caller then keys on the whole tree).
  const npmScanKey = (scan) => {
    if (typeof readBlob !== "function") return undefined;
    const npmMatch = /^(test:npm|tsc|eslint|biome):(.+)$/.exec(scan.id);
    if (!npmMatch || !npmByUnit.has(npmMatch[2])) return undefined;
    const { members, unresolved } = npmClosure(npmByUnit.get(npmMatch[2]));
    if (unresolved) return undefined;
    return canonicalJson({ kind: "npm", id: scan.id, members: members.map((m) => [m.name, m.dir, idOf(m.dir)]), shared: sharedNpmInputs(members) });
  };
  const unitKeyMemo = new Map();
  return (scan) => {
    if (scan.class === "A") {
      if (liveScan(scan.id) && !ADVISORY_SCAN.test(scan.id)) return null;
      try {
        if (ADVISORY_SCAN.test(scan.id)) return advisoryKey(scan);
        const files = fileList(scan);
        // node --check reads a package.json above the file only for its "type" (module or commonjs): a version bump there is not an input (NN20; every
        // release reran 423 node-check rows). Any other read of the manifest, or an unreadable one, keys on its whole content as before.
        const nodeType = (p) => { if (!/^node-check:/.test(scan.id) || !/(^|\/)package\.json$/.test(p) || typeof readBlob !== "function") return null; try { const v = JSON.parse(String(readBlob(p))); return `type:${v?.type ?? "commonjs"}`; } catch { return null; } };
        if (files) return canonicalJson({ kind: "files", id: scan.id, files: [...new Set(files)].sort().map((p) => [p, nodeType(p) ?? idOf(p)]) });
        if (/^cargo-package-list:/.test(scan.id)) {
          const unit = scan.id.slice("cargo-package-list:".length);
          const head = byName.get(unit);
          const ws = head ? wsOf(head) : null; const lock = ws === null ? null : lockFor(ws);
          if (!head || !lock || typeof readBlob !== "function") return whole();
          const manifestPath = inWs(ws, "Cargo.toml");
          let manifest;
          try { manifest = manifestCompileProjection(readBlob(manifestPath)); } catch { manifest = ["raw", manifestPath, idOf(manifestPath)]; }
          return canonicalJson({ kind: "package-list", unit, dir: [head.dir, idOf(head.dir)], manifest, lock: [[crateOf(head), lock.get(crateOf(head))?.signatures ?? []]] });
        }
        // GC-A24: a package's tsc, eslint and biome rows are keyed on its workspace closure, not the whole tree (acceptance S3 saw tsc rerun for an rk edit).
        return npmScanKey(scan) ?? whole();
      } catch (error) { onKeyError?.(scan, error); return null; }
    }
    if (scan.class !== "B") return null;
    try {
      // A batched compile (clippy:batch:*, from planScans `clippyBatching` or runScans' class B batching, and check:batch:*) is keyed on the inputs of
      // every member it names: each member's own compile key, in member order, plus the batch's own id and command (its member list, target and
      // environment). Any member that falls back to the whole tree makes the batch fall back too. A changed member changes the key, so the
      // batch runs again as a whole (cargo is incremental); no member's result is ever taken from another invocation's.
      if (/^(?:clippy|check|doc|test):batch:/.test(String(scan.id)) && Array.isArray(scan.batch?.units) && scan.batch.units.length && scan.batch.units.every((n) => known.has(n))) {
        const keys = scan.batch.units.map((n) => rustUnitKey(n, scan));
        return keys.includes(undefined) ? whole() : canonicalJson({ kind: "rust-batch", id: scan.id, units: scan.batch.units.map((n, i) => [n, keys[i]]) });
      }
      // A grouped publisher invocation (publisher-batch:*) is keyed on every crate it names (`-p`), each crate's own key, like a class B batch: on the
      // whole tree it missed on every commit, whatever the commit touched (NN20).
      if (/^publisher-batch:/.test(String(scan.id)) && Array.isArray(scan.args)) {
        const names = scan.args.flatMap((a, i) => (a === "-p" && typeof scan.args[i + 1] === "string" ? [scan.args[i + 1]] : []));
        if (names.length && names.every((n) => known.has(n))) {
          const keys = names.map((n) => rustUnitKey(n, scan));
          if (!keys.includes(undefined)) return canonicalJson({ kind: "rust-batch", id: scan.id, units: names.map((n, i) => [n, keys[i]]) });
        }
      }
      const unit = rustUnitOfScanId(scan.id, known);
      const debug = process.env.RK_GATE_KEY_DEBUG && new RegExp(process.env.RK_GATE_KEY_DEBUG).test(scan.id);
      if (!unit) {
        if (debug) process.stderr.write(`key-debug ${scan.id}: no Rust unit (known ${known.size})\n`);
        return npmScanKey(scan) ?? whole();
      }
      const keyed = rustUnitKey(unit, scan);
      if (debug) process.stderr.write(`key-debug ${scan.id}: unit ${unit}, publisherPhase ${scan.publisherPhase}, ${keyed === undefined ? "falls back to the whole tree" : "crate key"}\n`);
      return keyed === undefined ? whole() : keyed;
    } catch (error) { onKeyError?.(scan, error); return null; }
  };
  // The cache key of one Rust unit's compile or test scan, or undefined when the scan must fall back to the whole tree.
  // A unit's key is a function of the unit, whether the scan is a test scan and the commit; it never names the scan, so the 5 to 10 scans of one
  // crate share one computation (B7: per-job prep, one git read of the manifest and one closure walk per scan before).
  // (unitKeyMemo is declared before the returned key function; declared here it was never initialised and every Rust key threw.)
  function rustUnitKey(unit, scan) {
    const memoKey = `${unit}\0${/^test:(?:rust(?:-serial)?|batch):/.test(scan.id) || scan.publisherPhase === "test" ? "test" : "compile"}`;
    if (unitKeyMemo.has(memoKey)) return unitKeyMemo.get(memoKey);
    const keyed = rustUnitKeyOf(unit, scan);
    unitKeyMemo.set(memoKey, keyed);
    return keyed;
  }
  function rustUnitKeyOf(unit, scan) {
      const head = byName.get(unit);
      const ws = wsOf(head); const lock = lockFor(ws);
      if (!lock || typeof listFiles !== "function" || typeof readBlob !== "function") return undefined;
      const testScan = /^test:(?:rust(?:-serial)?|batch):/.test(scan.id) || scan.publisherPhase === "test";
      const ownConfig = crateFeatures?.[unit] ?? crateFeatures?.[crateOf(head)] ?? null;
      let testInputs = []; let declaredInputs = false;
      if (testScan) {
        const declared = ownConfig?.testInputs;
        if (declared !== undefined) {
          if (!Array.isArray(declared) || declared.length === 0 || !declared.every(plainRelative)) return undefined;
          testInputs = [...new Set(declared)].sort(); declaredInputs = true;
        }
      } else if (!COMPILE_SCAN_PREFIXES.has(String(scan.id).split(":")[0]) && scan.publisherPhase !== "compile") return undefined;
      if (!reachMemos.has(ws)) reachMemos.set(ws, new Map());
      const reached = [...reach(lock, crateOf(head), reachMemos.get(ws))];
      // A crate that is its own workspace (one workspace and lock per crate) holds its sibling crates in its lock as path packages: the
      // sibling's source is part of the closure whichever workspace it lives in, or a change to it would leave a stale key.
      const closure = [...new Set([head, ...reached.map((n) => byCrate.get(`${ws}\0${n}`) ?? siblingByCrate.get(n)).filter(Boolean)])].sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
      const extra = new Set(testInputs);
      for (const member of closure) {
        const found = externalOf(member);
        if (found.unresolved) return undefined;
        // A dependency's own tests never run here: only the tested crate's test code decides whether its tests leave the crate.
        if (testScan && !declaredInputs && found.leavesCrate && member === head) return undefined;
        for (const p of found.paths) extra.add(p);
      }
      // The shared inputs, projected to what this crate consumes.
      const dirs = ancestorDirsOf(ws);
      const ancestors = dirs.filter((dir) => dir !== ws);
      const lockNames = [crateOf(head), ...reached].sort();
      const lockEntries = lockNames.map((n) => [n, lock.get(n)?.signatures ?? []]);
      const vendored = lockNames.map((n) => `vendor/${n}`).filter((p) => idOf(p) !== null && idOf(p) !== undefined).map((p) => [p, idOf(p)]);
      const manifestPath = inWs(ws, "Cargo.toml");
      let manifest;
      try { manifest = manifestCompileProjection(readBlob(manifestPath)); } catch { manifest = ["raw", manifestPath, idOf(manifestPath)]; }
      const cargoConfig = dirs.flatMap((dir) => {
        const cfg = inWs(dir, ".cargo");
        if (idOf(cfg) === null || idOf(cfg) === undefined) return [];
        try { return listFiles(cfg).filter((f) => !f.endsWith("/audit.toml")).sort().map((f) => [f, idOf(f)]); } catch { return [[cfg, idOf(cfg)]]; }
      });
      const toolFiles = [...SHARED_RUST_INPUTS.filter((p) => !["Cargo.lock", "Cargo.toml", ".cargo", "deny.toml", "vendor"].includes(p)).map((p) => inWs(ws, p)),
        ...ancestors.flatMap((dir) => ANCESTOR_RUST_INPUTS.filter((p) => ![".cargo", "deny.toml"].includes(p)).map((p) => inWs(dir, p)))];
      return canonicalJson({
        kind: testScan ? "rust-test" : "rust", unit, cargoDir: ws,
        members: closure.map((m) => [m.name, m.dir, idOf(m.dir)]),
        lock: lockEntries, manifest, cargoConfig, vendored, config: ownConfig,
        shared: [...new Set(toolFiles)].sort().map((p) => [p, idOf(p)]),
        external: [...extra].sort().map((p) => [p, idOf(p)]),
      });
  }
}

// ---- quiet lease ------------------------------------------------------------------------------------------------

// The broker's quiet lease TTL ceiling is 30 minutes (RustRight, monotonic clock); an expired lease is not refused, the lane just
// loses its protection, so a quiet lane longer than this is reported by the gate when it happens, never hidden.
export const QUIET_LEASE_TTL_MS = 30 * 60 * 1000;
export const QUIET_LEASE_WAIT_DRAIN_MS = 2 * 60 * 1000;

/**
 * The runScans `quietLane` for a host with RustRight (the reason carries no spaces: a .cmd shim on Windows re-splits its arguments): `rustright quiet acquire` before the quiet test scans (waits up to two
 * minutes for running broker jobs to drain), `RUSTRIGHT_QUIET_LEASE=<id>` in their environment, `rustright quiet release` after.
 * Without `rustright` on PATH, or when the broker refuses, the lane runs without a lease and the gate says so: the tests still run,
 * their verdict is simply taken on a possibly busy host. `o.spawn` is the gate's synchronous spawner.
 */
export function quietLeaseLane(o, { tag = "rightkit-gate:", reason = "rightkit-gate-quiet-tests", ttlMs: baseTtlMs = QUIET_LEASE_TTL_MS, waitDrainMs = QUIET_LEASE_WAIT_DRAIN_MS, pid = process.pid, executor = null } = {}) {
  // The declared Linux broker route (managed-cargo.mjs): its validated client and endpoint, never a PATH lookup.
  const linuxRoute = executor?.route === LINUX_BROKER_ROUTE && executor.kind === "broker" ? executor : null;
  const which = (cmd) => {
    const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]);
    return failed(r) ? null : (text(r).split(/\r?\n/).map((l) => l.trim()).find(Boolean) ?? null);
  };
  // The client is started the one way every gate child starts it (broker-client.mjs): on Windows `rustright` is rustright.cmd, which node cannot
  // spawn, so its target .exe is read from the shim and started directly, without a shell.
  const clientCall = (client, args) => {
    const platform = o.platform ?? process.platform;
    if (linuxRoute) return spawnBrokerClient(o.spawn, linuxRoute.client, args, { env: { ...o.env, ...linuxRoute.env } }, { platform, readFile: o.readFile });
    return spawnBrokerClient(o.spawn, platform === "win32" ? client : "rustright", args, {}, { platform, readFile: o.readFile });
  };
  return {
    // `want.ttlMs` (from the scan runner): the lane's own bound, every quiet scan's timeout plus a margin, so the lease cannot
    // expire while a quiet suite still runs (each scan is killed at its timeout first).
    acquire: async (want = {}) => {
      const ttlMs = Number.isFinite(want.ttlMs) && want.ttlMs > baseTtlMs ? want.ttlMs : baseTtlMs;
      if (o.brokerException) { o.out(`${tag} note: ${o.brokerException.note} (${o.brokerException.exceptionId}); quiet test scans run without a broker lease`); return null; }
      // A direct (Linux) host never takes a broker lease, whatever is on PATH (managed-cargo directPlatform); a declared Linux broker route does.
      if (directPlatform(o.platform ?? process.platform) && !linuxRoute) { o.out(`${tag} note: direct host (no broker on ${o.platform ?? process.platform}); quiet test scans run without a broker lease`); return null; }
      const client = linuxRoute ? linuxRoute.client : which("rustright");
      if (!client) { o.out(`${tag} note: no rustright on PATH; quiet test scans run without a broker lease`); return null; }
      const started = Date.now();
      const r = clientCall(client, ["quiet", "acquire", "--ttl-ms", String(ttlMs), "--reason", reason, "--wait-drain-ms", String(waitDrainMs), "--holder-pid", String(pid), "--json"]);
      if (failed(r)) { o.out(`${tag} note: broker quiet lease refused (${(why(r) || `exit ${r?.status}`).slice(0, 200)}); quiet test scans run without a lease`); return null; }
      let id = null;
      // The CLI prints the broker's QUIET_LEASE event: { type, granted, state, id } when held; a refusal exits non-zero above.
      try { const j = JSON.parse(text(r)); id = j?.granted === false ? null : (j?.id ?? j?.lease?.id ?? null); } catch { id = null; }
      if (typeof id !== "string" || !id) { o.out(`${tag} note: broker quiet lease answer has no lease id (${text(r).trim().slice(0, 200)}); quiet test scans run without a lease`); return null; }
      o.out(`${tag} broker quiet lease ${id} held (ttl ${Math.round(ttlMs / 60000)} min; waited ${Math.round((Date.now() - started) / 1000)} s for the broker to drain)`);
      return { id, env: { RUSTRIGHT_QUIET_LEASE: id }, at: Date.now() };
    },
    release: async (id) => {
      const r = clientCall(linuxRoute ? linuxRoute.client : (which("rustright") ?? "rustright"), ["quiet", "release", String(id)]);
      if (failed(r)) o.out(`${tag} note: broker quiet lease ${id} release failed (${(why(r) || `exit ${r?.status}`).slice(0, 200)}); it expires on its own`);
      else o.out(`${tag} broker quiet lease ${id} released`);
    },
  };
}



// ---- install-hook -------------------------------------------------------------------------------------------

/**
 * `rightkit-gate install-hook` installs no git hook: commit and push run no gate and no local hook check (docs/GATE-CONTRACT.md).
 * It retires what an earlier release wrote, each file only by its own header or marker (../hook-retirement.mjs), never by name or place:
 *   - .githooks/pre-push, the gate hook, and the .githooks/<name> forwarders that chained .git/hooks beside it;
 *   - the advisory ownership pre-commit hook (in .githooks, in .git/hooks or in the directory core.hooksPath names), with the foreign
 *     pre-commit it displaced (pre-commit.rightkit-foreign) put back as pre-commit;
 *   - the repository-local core.hooksPath, only when it is exactly `.githooks` and nothing foreign remains in that directory.
 * A foreign hook (any other file in .githooks, .git/hooks or the configured directory) is never read for change, moved or deleted, and
 * a foreign .githooks directory keeps its core.hooksPath. The global and system git configuration is never touched. It still registers
 * the one-door agent hook in .claude/settings.json (the agent side, not a git hook).
 */
async function installHook(o, root) {
  const git = (...a) => o.spawn("git", a, { cwd: root });
  const ops = { readdir: o.readdir, readFile: o.readFile, lstat: o.lstat ?? fs.lstatSync, unlink: o.unlink ?? fs.unlinkSync, rename: o.rename ?? fs.renameSync, rmdir: o.rmdir ?? fs.rmdirSync };
  const done = [];
  const isRealDirectory = (dir) => { try { const stat = ops.lstat(dir); return stat.isDirectory() && !stat.isSymbolicLink(); } catch { return false; } };
  const label = (dir) => posix(path.relative(root, dir)) || ".";
  const retire = (dir, options) => {
    if (!isRealDirectory(dir)) return null;
    const result = retireHooksIn(dir, ops, options);
    for (const name of result.removed) done.push(`retired ${label(dir)}/${name}`);
    for (const name of result.restored) done.push(`restored ${label(dir)}/${name} from ${FOREIGN_PRE_COMMIT}`);
    if (result.removedDir) done.push(`removed the empty ${label(dir)}`);
    return result;
  };

  // The gate hook, its forwarders and the ownership step in .githooks, then the ownership step wherever else git ran hooks from.
  const hooksDir = path.join(root, ".githooks");
  retire(hooksDir, { scripts: true });
  const gitHooksResult = git("rev-parse", "--git-path", "hooks");
  const gitHooksDir = !failed(gitHooksResult) && text(gitHooksResult).trim() ? path.resolve(root, text(gitHooksResult).trim()) : null;
  const configured = git("config", "--local", "--get", "core.hooksPath");
  const hooksPath = failed(configured) ? "" : text(configured).trim();
  const seen = new Set([path.resolve(hooksDir)]);
  for (const dir of [gitHooksDir, hooksPath && hooksPath !== ".githooks" ? path.resolve(root, hooksPath) : null]) {
    if (!dir || seen.has(path.resolve(dir))) continue;
    seen.add(path.resolve(dir));
    retire(dir, { scripts: false });
  }

  // core.hooksPath: unset only when it is `.githooks` and that directory holds nothing foreign (it is gone, or empty).
  let remaining = [];
  if (hooksPath === ".githooks") {
    if (isRealDirectory(hooksDir)) { try { remaining = [...o.readdir(hooksDir)].map(String).sort(); } catch { remaining = ["(unreadable)"]; } }
    else if (o.exists(hooksDir)) remaining = ["(not a plain directory)"];
    if (!remaining.length) {
      const unset = git("config", "--local", "--unset", "core.hooksPath");
      if (failed(unset)) { o.err(`rightkit-gate: git config --unset core.hooksPath failed: ${why(unset)}`); return 2; }
      done.push("unset core.hooksPath (was .githooks)");
    } else {
      done.push(`core.hooksPath stays .githooks: foreign file(s) remain there (${remaining.join(", ")}); the hooks in .git/hooks do not run while it is set`);
    }
  } else if (hooksPath) done.push(`core.hooksPath is ${JSON.stringify(hooksPath)}: left as it is`);
  if (!done.length) done.push("no git hook of RightKit's to retire");

  // The agent side of the one door: a Claude Code PreToolUse hook in the repository's .claude/settings.json runs door-policy.mjs on every
  // shell command, on every host (the broker-less cloud included), and denies any build that does not go through rightkit-gate.
  const agentHookFile = "node_modules/@rightkit/release/bin/rightkit-agent-hook.mjs";
  const agentHook = o.exists(path.join(root, agentHookFile)) ? agentHookFile : posix(path.relative(root, fileURLToPath(new URL("./rightkit-agent-hook.mjs", import.meta.url))));
  const settingsPath = path.join(root, ".claude", "settings.json");
  let settings = {};
  if (o.exists(settingsPath)) { try { settings = JSON.parse(o.readFile(settingsPath)); } catch { o.err("rightkit-gate: .claude/settings.json is not JSON; the agent hook was not added"); settings = null; } }
  if (settings) {
    const command = `node ${agentHook}`;
    const pre = Array.isArray(settings.hooks?.PreToolUse) ? settings.hooks.PreToolUse : [];
    const has = pre.some((entry) => (entry.hooks ?? []).some((h) => typeof h.command === "string" && h.command.includes("rightkit-agent-hook")));
    if (has) done.push(".claude/settings.json agent hook already present");
    else {
      settings.hooks = { ...(settings.hooks ?? {}), PreToolUse: [...pre, { matcher: "Bash", hooks: [{ type: "command", command }] }] };
      try { o.mkdir(path.dirname(settingsPath), { recursive: true }); } catch { /* exists */ }
      o.writeFile(settingsPath, `${JSON.stringify(settings, null, 2)}\n`);
      done.push("added the one-door agent hook to .claude/settings.json (commit it)");
    }
  }
  for (const line of done) o.out(`rightkit-gate: ${line}`);
  o.out("rightkit-gate: no git hook is written: commit and push run no gate and no local hook check (the gate is for building)");
  return 0;
}

// ---- the stages: rightkit-gate --to <stage> --------------------------------------------------------------------------------

/**
 * Runs every stage up to `--to`, in order, for HEAD: check (class A), compile (the whole gate over every file), then, through the existing
 * owners, package (`right-release build`, which seals the exact artifact set; no source QA, GATE-CONTRACT GC-A07), qualify (installed QA on that sealed set)
 * and release (`right-release upload` of that same sealed set). Stops at the first stage that fails. check and compile are reused from
 * the receipt only when every bound input is unchanged; the right-release stages run under a live door run, so they refuse any other
 * caller (gate-stages.mjs requireDoorRun).
 */
/**
 * The app a door run is for: the directory the gate was run from when it holds a release config (a nested app such as
 * apps/coderight-tauri), else the repository root. Every right-release stage runs there, the sealed set is read from there, and the
 * receipt is keyed to it (receiptKey). Returns { appRoot, appDir } with appDir relative to the repository root, "." at the root.
 */
export function appRootOf({ root, cwd, exists = fs.existsSync, configDirs = null }) {
  const inside = cwd && (cwd === root || cwd.startsWith(root + path.sep));
  // With `configDirs` (the directories whose committed tree holds a release config: `committedConfigDirs`) the scope is the SELECTED COMMIT's, read from
  // its tree alone; the checkout's files are never asked, so a newer, dirty or edited checkout cannot move an older commit's app. Without it (callers that
  // hold no commit) the live checkout answers, as before. Either way the directory the gate was run from is the explicit identity.
  const hasConfig = configDirs
    ? (d) => configDirs.has(path.relative(root, d).split(path.sep).join("/") || ".")
    : (d) => ["right-release.config.json", "right-release.config.mjs"].some((f) => exists(path.join(d, f)));
  // The nearest directory at or above cwd (below the root) that carries a release config is the app; a deeper cwd (apps/foo/src) is apps/foo.
  let appRoot = root;
  if (inside) for (let d = path.resolve(cwd); d.startsWith(root) && d !== root; d = path.dirname(d)) { if (hasConfig(d)) { appRoot = d; break; } }
  return { appRoot, appDir: path.relative(root, appRoot).split(path.sep).join("/") || "." };
}

/**
 * The directories (posix, relative to the repository root, "." for the root) that hold a release config in `commit`'s tree: `{ dirs }`, or `{ refusal }` when the
 * tree cannot be listed. Read with `git ls-tree` of the selected commit, so the answer is the commit's, whatever the checkout holds.
 */
export function committedConfigDirs({ git, commit }) {
  const listed = git("ls-tree", "-r", "--name-only", "-z", commit);
  if (failed(listed)) return { refusal: `cannot list the files of ${String(commit).slice(0, 12)}: ${why(listed)}` };
  const dirs = new Set(text(listed).split("\0").filter((f) => /(^|\/)right-release\.config\.(json|mjs)$/.test(f)).map((f) => path.posix.dirname(f)));
  return { dirs };
}

/** True when `commit` is RightKit's own repository: its tree holds packages/release/package.json named @rightkit/release (the identity `check` uses for RightKit's own rules). Read from the commit, never the checkout. */
export function rightkitIdentityAt(git, commit) {
  const shown = git("show", `${commit}:${RIGHTKIT_IDENTITY_FILE}`);
  return !failed(shown) && isRightKitIdentity(text(shown));
}

const UNIT_SELECTOR = /^(?:npm:@rightkit\/[a-z0-9-]+|cargo:rightkit-[a-z0-9-]+)$/;

/**
 * The unit list of `--units-file <file>`: JSON `{ "units": [ "cargo:rightkit-x", "npm:@rightkit/y", ... ] }` written for this run (not a
 * committed file: Adrian, 2026-10-31: "a JSON file that is created at that time"). Every entry is a `--unit` selector, once. Throws a usage error.
 */
export function readUnitList(readFile, file) {
  const usage = (what) => Object.assign(new Error(`RK_UNITS_FILE: ${file} ${what}`), { code: "RK_UNITS_FILE" });
  let parsed;
  try { parsed = JSON.parse(readFile(file)); } catch (e) { throw usage(`cannot be read as JSON (${e.message})`); }
  const units = parsed?.units;
  if (!Array.isArray(units) || !units.length || !units.every((u) => typeof u === "string" && UNIT_SELECTOR.test(u))) throw usage('must be { "units": [ "cargo:rightkit-<name>" | "npm:@rightkit/<name>", ... ] } with at least one unit');
  const duplicate = units.find((u, i) => units.indexOf(u) !== i);
  if (duplicate !== undefined) throw usage(`names ${duplicate} more than once`);
  return units;
}

const unitDirOfSelector = (sel) => { const m = /^(?:npm:@rightkit\/([a-z0-9-]+)|cargo:(rightkit-[a-z0-9-]+))$/.exec(sel); return m ? (m[1] ? `packages/${m[1]}` : `crates/${m[2]}`) : null; };

/**
 * The fixed unit inventory of one commit, read from its object database alone (never the checkout): every
 * `crates/<x>/Cargo.toml` and `packages/<x>/package.json`, with sibling path dependencies (crate-workspaces.mjs) and
 * `@rightkit/*` npm dependencies. Throws when the commit's tree or a manifest cannot be read: a selection computed
 * from a partial inventory would silently skip units.
 */
export function inventoryAtCommit({ git, commit }) {
  const listed = git("ls-tree", "-r", "--name-only", "-z", commit);
  if (failed(listed)) throw new Error(`cannot list the tree of ${commit.slice(0, 12)}: ${why(listed)}`);
  const files = new Set(text(listed).split("\0").filter(Boolean));
  const show = (file) => { const r = git("show", `${commit}:${file}`); if (failed(r)) throw new Error(`cannot read ${file} at ${commit.slice(0, 12)}: ${why(r)}`); return text(r); };
  const parse = createRequire(import.meta.url)("smol-toml").parse;
  const manifests = new Map();
  for (const file of [...files].filter((f) => f === "Cargo.toml" || /^crates\/[^/]+\/Cargo\.toml$/.test(f)).sort()) {
    try { manifests.set(path.posix.dirname(file), parse(show(file))); }
    catch (e) { throw new Error(`cannot parse ${file}: ${e.message}`); }
  }
  const crates = [...manifests].filter(([dir, m]) => dir.startsWith("crates/") && m.package);
  const byDir = new Map(crates.map(([dir, m]) => [dir, m.package.name]));
  const byName = new Set(byDir.values());
  const cargo = crates.map(([dir, m]) => {
    if (typeof m.package.name !== "string") throw new Error(`${dir}/Cargo.toml has no package name`);
    let workspaceDir = typeof m.package.workspace === "string" ? path.posix.normalize(path.posix.join(dir, m.package.workspace)) : dir;
    if (typeof m.package.workspace !== "string") while (workspaceDir !== "." && !manifests.get(workspaceDir)?.workspace) workspaceDir = path.posix.dirname(workspaceDir);
    const workspace = manifests.get(workspaceDir)?.workspace;
    const tables = [];
    for (const holder of [m, ...Object.values(m.target ?? {})]) for (const key of ["dependencies", "dev-dependencies", "build-dependencies"]) if (holder?.[key]) tables.push(holder[key]);
    const deps = new Set();
    for (const table of tables) for (const [alias, original] of Object.entries(table)) {
      let spec = original; let base = dir;
      if (spec && typeof spec === "object" && spec.workspace === true) {
        spec = workspace?.dependencies?.[alias]; base = workspaceDir;
        if (!spec) throw new Error(`${dir}/Cargo.toml cannot resolve workspace dependency ${alias}`);
      }
      const name = typeof spec === "object" ? (spec.package ?? alias) : alias;
      if (typeof spec === "object" && spec.path !== undefined) {
        if (typeof spec.path !== "string") throw new Error(`${dir}: invalid path for ${alias}`);
        const target = path.posix.normalize(path.posix.join(base, spec.path));
        if (target.startsWith("crates/") && !byDir.has(target)) throw new Error(`${dir}: dependency ${alias} points to unknown crate ${target}`);
        if (byDir.has(target)) { if (byDir.get(target) !== name) throw new Error(`${dir}: dependency ${alias} names ${name} but ${target} is ${byDir.get(target)}`); deps.add(name); }
      } else if (byName.has(name) || name.startsWith("rightkit-")) deps.add(name);
    }
    return { dir, name: m.package.name, deps: [...deps] };
  });
  const npm = [];
  for (const file of [...files].filter((f) => /^packages\/[^/]+\/package\.json$/.test(f)).sort()) {
    let parsed;
    try { parsed = JSON.parse(show(file)); } catch (e) { throw new Error(`cannot parse ${file}: ${e.message}`); }
    if (typeof parsed?.name !== "string") throw new Error(`${file} has no package name`);
    const names = [];
    for (const key of ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"]) {
      const table = parsed[key] ?? {};
      if (!table || typeof table !== "object" || Array.isArray(table)) throw new Error(`${file}: invalid ${key}`);
      for (const [name, version] of Object.entries(table)) {
        const alias = typeof version === "string" && version.match(/^npm:(@rightkit\/[^@]+)(?:@|$)/)?.[1];
        const target = alias || name;
        if (target.startsWith("@rightkit/")) names.push(target);
        else if (typeof version === "string" && version.startsWith("workspace:")) throw new Error(`${file}: unsupported workspace dependency ${name}`);
      }
    }
    npm.push({ dir: file.slice(0, -"/package.json".length), name: parsed.name, deps: names });
  }
  return buildInventory({ cargo, npm });
}

/** The paths changed between `base` and `commit` (three-dot: what the commit side changed since the merge base). */
/** Classify a committed private unit without publication-only version rules. */
export function privateUnitManifest({ selector, content }) {
  const match = /^(?:npm:(@rightkit\/[a-z0-9-]+)|cargo:(rightkit-[a-z0-9-]+))$/.exec(selector);
  if (!match) throw new Error(`invalid unit selector ${selector}`);
  const kind = match[1] ? "npm" : "cargo";
  let parsed;
  try { parsed = kind === "npm" ? JSON.parse(content) : createRequire(import.meta.url)("smol-toml").parse(content); }
  catch (error) { throw new Error(`${selector}: cannot parse manifest: ${error.message}`); }
  const pkg = kind === "npm" ? parsed : parsed.package;
  const name = match[1] ?? match[2];
  if (pkg?.name !== name) throw new Error(`${selector}: manifest name does not match`);
  return { kind, name, private: kind === "npm" ? pkg.private === true : pkg.publish === false || (Array.isArray(pkg.publish) && pkg.publish.length === 0) };
}

export function changedPathsAt({ git, base, commit }) {
  const diff = git("diff", "--name-status", "-z", "--find-renames", `${base}...${commit}`);
  if (failed(diff)) throw new Error(`cannot diff ${base}...${commit.slice(0, 12)}: ${why(diff)}; pass --base <ref>`);
  const fields = text(diff).split("\0"); if (fields.at(-1) === "") fields.pop();
  const paths = [];
  while (fields.length) {
    const status = fields.shift();
    if (!/^(?:[ACDMRT]|R[0-9]+|C[0-9]+)$/.test(status) || !fields.length) throw new Error(`invalid diff status ${status}`);
    paths.push(fields.shift());
    if (/^[RC][0-9]+$/.test(status)) { if (!fields.length) throw new Error(`missing rename path for ${status}`); paths.push(fields.shift()); }
  }
  return [...new Set(paths)];
}

/**
 * One shared execution for a selected unit list (rightkit#1505): dependency-first order, one shared Class A (the
 * first unit's, scoped to every selected directory and remembered for the rest), every independent unit run even
 * when another fails, a unit whose selected dependency failed recorded blocked and not run, one receipt per unit
 * (with `--receipt-dir`) plus one aggregate report. Returns the exit the door run ends with: 0 only when every
 * selected unit passed, else the first failing unit's code (1 when only blocked units failed the run).
 */
export async function runSharedUnitList({ o, io, stage, commit, selected, inventory, receiptDir = undefined, base = null, unmatched = [], runUnit = null }) {
  const tag = "rightkit-gate:";
  if (io[CARGO_WIDTH] !== undefined) {
    const problem = cargoWidthUnitsProblem(selected);
    if (problem) { o.err(`${tag} BLOCKED: ${problem}`); return 2; }
  }
  const bySelector = new Map((inventory?.units ?? []).map((u) => [u.selector, u]));
  const rows = selected.map((s) => ({ dir: unitDirOfSelector(s.selector), reason: "listed", ...bySelector.get(s.selector), ...s }));
  const order = inventory ? dependencyFirstOrder(inventory, new Set(rows.map((r) => r.selector))) : rows.map((r) => r.selector);
  const ordered = order.map((sel) => rows.find((r) => r.selector === sel));
  const unitListClassA = { dirs: [...new Set(ordered.map((r) => r.dir).filter(Boolean))], memo: new Map() };
  if (receiptDir !== undefined) {
    try { o.mkdir(receiptDir); } catch (e) { o.err(`${tag} BLOCKED: receipt directory must be new: ${receiptDir} (${e.message}); nothing ran`); return 2; }
  }
  if (stage === "compile" && !runUnit && !io.runUnitForList && ordered.length) {
    const union = { selected: ordered, inventory, rows: null, coverage: null, internal: [], commands: [], commit, runId: randomUUID() };
    const exit = await main(["--to", "compile", "--unit", ordered[0].selector, "--commit", commit], { ...io, unitListClassA, [UNIT_LIST_UNION]: union });
    const results = ordered.map((unit) => {
      const evidence = union.coverage?.filter((r) => r.selector === unit.selector) ?? [];
      const internal = union.internal.filter((r) => r.selector === unit.selector);
      const expected = union.expected?.get(unit.selector);
      const all = [...internal, ...evidence];
      const failed = !expected || all.length !== expected || all.some((r) => r.status === "failed" || r.status === "fail");
      const blocked = !failed && all.some((r) => r.status === "blocked" || r.status === "not-run");
      // A unit passes only on a run that exited 0: a scheduler or stage exception never leaves earlier evidence standing as a pass (cloud 2026-11-06:
      // a lookup's coverage was reported as 132 passed after "scheduleClassB: invalid projection").
      const status = failed ? "failed" : blocked ? "blocked" : exit === 0 && union.verified && all.every((r) => r.status === "passed" || r.status === "pass") ? "passed" : "failed";
      return { selector: unit.selector, status, ...(status === "failed" ? { code: exit || 1 } : {}) };
    });
    const report = unitListReport({ commit, stage, base, selected: ordered, results, unmatched }); report.runId = union.runId;
    for (const row of report.units) {
      const evidence = { schema: "rightkit.unit-list-compile/1", runId: union.runId, commit, unit: row.selector, status: row.status, requirements: [...union.internal.filter((r) => r.selector === row.selector), ...(union.coverage?.filter((r) => r.selector === row.selector) ?? [])], cwdProofs: (union.cwdProofs ?? []).filter((r) => union.owner?.get(r.id) === row.selector), commands: union.commands.filter((r) => r.selector === row.selector), scans: (union.rows ?? []).filter((r) => r.publisherUnit === row.selector || r.sharedOrigin === true) };
      if (receiptDir === undefined) row.receipt = { status: "not-requested" };
      else {
        const name = `gate-${row.selector.replace(/^npm:@rightkit\//, "npm-").replace(/^cargo:/, "")}.json`;
        const data = `${JSON.stringify(evidence, null, 2)}\n`; o.writeFile(path.join(receiptDir, name), data);
        row.receipt = { status: "written", file: name, sha256: createHash("sha256").update(data).digest("hex") };
      }
      o.out(`${tag} unit ${row.selector}: ${row.status}`);
    }
    o.out(`${tag} unit report: ${report.counts.passed} passed, ${report.counts.failed} failed, ${report.counts.blocked} blocked, ${report.counts["not-run"]} not-run of ${ordered.length} on ${commit.slice(0, 12)}`);
    if (receiptDir !== undefined) o.writeFile(path.join(receiptDir, "units-report.json"), `${JSON.stringify(report, null, 2)}\n`);
    return report.ok ? 0 : (exit || 1);
  }
  const run = runUnit ?? io.runUnitForList ?? (async (selector, receipt) => main(["--to", stage, "--unit", selector, "--commit", commit, ...receipt], { ...io, unitListClassA }));
  const outcomes = new Map();
  const receiptEvidence = new Map();
  const runId = randomUUID();
  const bad = new Set(); // failed or blocked selectors, for the dependency check of later units
  for (const [i, row] of ordered.entries()) {
    const blocker = (bySelector.get(row.selector)?.deps ?? []).find((d) => bad.has(d));
    if (blocker) {
      bad.add(row.selector);
      o.err(`${tag} unit list ${i + 1}/${ordered.length}: ${row.selector} blocked: its dependency ${blocker} did not pass; not run`);
      continue;
    }
    o.out(`${tag} unit list ${i + 1}/${ordered.length}: ${row.selector} (${row.reason}${row.via ? ` via ${row.via}` : ""}) on commit ${commit.slice(0, 12)}`);
    const receipt = receiptDir === undefined ? [] : ["--receipt-out", path.join(receiptDir, `gate-${row.selector.replace(/^npm:@rightkit\//, "npm-").replace(/^cargo:/, "")}.json`)];
    let code = await run(row.selector, receipt, unitListClassA);
    if (receiptDir !== undefined) {
      const file = receipt[1];
      try {
        const bytes = (o.readFile ?? fs.readFileSync)(file);
        receiptEvidence.set(row.selector, { status: "written", file: path.basename(file), sha256: createHash("sha256").update(bytes).digest("hex") });
      } catch (error) {
        receiptEvidence.set(row.selector, { status: "missing", file: path.basename(file) });
        if (code === 0) { o.err(`${tag} ${row.selector}: successful run did not produce a readable receipt (${error.message})`); code = 1; }
      }
    }
    outcomes.set(row.selector, { ran: true, code });
    if (code !== 0) bad.add(row.selector);
  }
  const results = classifyUnitResults({ selected: ordered, inventory: inventory ?? { units: rows }, outcomes });
  const report = unitListReport({ commit, stage, base, selected: ordered, results, unmatched });
  report.runId = runId;
  for (const row of report.units) row.receipt = receiptDir === undefined ? { status: "not-requested" } : (receiptEvidence.get(row.selector) ?? { status: "not-run" });
  for (const r of report.units) o.out(`${tag} unit ${r.selector}: ${r.status}${r.status === "failed" ? ` (exit ${r.code})` : ""}${r.blockedBy ? ` (dependency ${r.blockedBy} did not pass)` : ""}`);
  o.out(`${tag} unit report: ${report.counts.passed} passed, ${report.counts.failed} failed, ${report.counts.blocked} blocked, ${report.counts["not-run"]} not-run of ${ordered.length} on ${commit.slice(0, 12)}`);
  if (receiptDir !== undefined) o.writeFile(path.join(receiptDir, "units-report.json"), `${JSON.stringify(report, null, 2)}\n`);
  if (report.ok) return 0;
  return results.find((r) => r.status === "failed")?.code || 1;
}

/**
 * RightKit names its unit on every run (#1325, Adrian 2026-10-31: "the unit has to be mentioned. Okay, so either it is one specific unit, or it is
 * a JSON file that is created at that time, or all with my exception"). A door run with no `--unit` is accepted in RightKit only with
 * `--changed [--base <ref>]` (the changed units plus their dependents, planned from the commit's fixed inventory: the routine
 * route, no handwritten list), `--units-file` (an explicit list, parsed into the same shared run) or
 * `--all-units-exception <id>` (the whole-repository run, by Adrian's entry in the commit's ci/all-units-exceptions.json). All flags name RightKit
 * units, so a repository that is not RightKit refuses them. Returns `{ exit }` when the run ends here, or `{}` to go on as a whole door run (the
 * all-units flag and its id are removed from `args`). An unresolvable commit is runStages' own refusal, so it goes on.
 */
/** The RightKit sample app the run's working directory is in, when the commit's ci/sample-apps.json lists it; else null. */
export function sampleAppOf({ o, root, git, commit }) {
  const shown = git("show", `${commit}:ci/sample-apps.json`);
  if (failed(shown)) return null;
  let apps;
  try { apps = JSON.parse(text(shown)).apps; } catch { return null; }
  if (!Array.isArray(apps)) return null;
  const rel = path.relative(root, path.resolve(o.cwd)).split(path.sep).join("/");
  return apps.find((dir) => typeof dir === "string" && dir && !dir.includes("..") && (rel === dir || rel.startsWith(`${dir}/`))) ?? null;
}

export async function rightkitDoorRule(o, args, root, git, io, { unitsFileFlag, allUnitsFlag, unitList, changedFlag = false }) {
  const tag = "rightkit-gate:";
  const option = (name) => { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; };
  let pinned;
  try { pinned = (await import("../snapshot.mjs")).resolveCommit(root, option("--commit") ?? "HEAD"); } catch { return {}; }
  const inRightKit = rightkitIdentityAt(git, pinned.commit);
  if (!inRightKit) {
    if (!unitsFileFlag && !allUnitsFlag && !changedFlag) return {};
    const flag = unitsFileFlag ? "--units-file" : changedFlag ? "--changed" : "--all-units-exception";
    o.err(`${tag} BLOCKED: ${flag} names RightKit units and ${pinned.commit.slice(0, 12)} is not RightKit; every other repository is built whole from the ${CARGO_WORKSPACE_LIST_KEY} list committed in ${CARGO_WORKSPACE_LIST_FILE}, with no unit named; nothing ran`);
    return { exit: 1 };
  }
  if (unitsFileFlag || changedFlag) {
    const stage = option("--to");
    const ownFlag = unitsFileFlag ? "--units-file" : "--changed";
    const allowed = unitsFileFlag ? ["--to", "--commit", "--receipt-dir"] : ["--to", "--commit", "--receipt-dir", "--base", "--changed"];
    const given = args.filter((a, i) => a.startsWith("--") && !(unitsFileFlag && args[i - 1] === ownFlag) && a !== ownFlag);
    if (given.some((a) => !allowed.includes(a))) { o.err(`${tag} a unit list run takes only --to, ${ownFlag} and optional --commit and --receipt-dir${changedFlag ? " and --base" : ""} (one receipt file per unit is written into the directory)`); return { exit: 2 }; }
    const receiptDir = option("--receipt-dir");
    if (receiptDir !== undefined && (receiptDir.startsWith("--") || !path.isAbsolute(receiptDir))) { o.err(`${tag} --receipt-dir needs an absolute directory`); return { exit: 2 }; }
    // A complete inventory is required for both selection and dependency blocking.
    let inventory = null;
    try { inventory = inventoryAtCommit({ git, commit: pinned.commit }); }
    catch (e) {
      o.err(`${tag} BLOCKED: cannot plan ${ownFlag} on ${pinned.commit.slice(0, 12)}: ${e.message}; nothing ran`); return { exit: 2 };
    }
    if (unitsFileFlag) {
      const unknown = unitList.filter((selector) => !inventory.units.some((u) => u.selector === selector));
      if (unknown.length) { o.err(`${tag} BLOCKED: units missing from inventory: ${unknown.join(", ")}; nothing ran`); return { exit: 2 }; }
      const selected = unitList.map((selector) => ({ selector, reason: "listed" }));
      return { exit: await runSharedUnitList({ o, io, stage, commit: pinned.commit, selected, inventory, receiptDir }) };
    }
    // --changed (rightkit#1505): no handwritten list for routine work. Changed paths against the base select their
    // units plus every transitive dependent (affected-units.mjs); the plan is printed before anything runs.
    const base = option("--base") ?? "origin/main";
    let selection;
    try {
      const mergeBase = git("merge-base", base, pinned.commit);
      if (failed(mergeBase)) throw new Error(`cannot resolve merge base: ${why(mergeBase)}`);
      const oldInventory = inventoryAtCommit({ git, commit: text(mergeBase).trim() });
      const changedPaths = changedPathsAt({ git, base, commit: pinned.commit });
      selection = selectAffectedAcrossInventories({ changedPaths, oldInventory, inventory });
      if (selection.unmatched.length) throw new Error(`unclassified changed inputs: ${selection.unmatched.join(", ")}; classify their ownership before running`);
    }
    catch (e) { o.err(`${tag} BLOCKED: cannot plan --changed on ${pinned.commit.slice(0, 12)}: ${e.message}; nothing ran`); return { exit: 2 }; }
    o.out(`${tag} changed plan: ${selection.selected.length} unit(s) affected by ${base}...${pinned.commit.slice(0, 12)}${selection.unmatched.length ? `; ${selection.unmatched.length} changed path(s) select no unit (${selection.unmatched.slice(0, 5).join(", ")}${selection.unmatched.length > 5 ? ", ..." : ""})` : ""}`);
    for (const s of selection.selected) o.out(`${tag} planned ${s.selector} (${s.reason}${s.via ? ` via ${s.via}` : ""})`);
    if (!selection.selected.length) {
      if (receiptDir !== undefined) { try { o.mkdir(receiptDir); } catch (e) { o.err(`${tag} BLOCKED: receipt directory must be new: ${receiptDir} (${e.message}); nothing ran`); return { exit: 2 }; } o.writeFile(path.join(receiptDir, "units-report.json"), `${JSON.stringify({ ...unitListReport({ commit: pinned.commit, stage, base, selected: [], results: [], unmatched: selection.unmatched }), runId: randomUUID() }, null, 2)}\n`); }
      o.out(`${tag} unit report: 0 passed, 0 failed, 0 blocked, 0 not-run of 0 on ${pinned.commit.slice(0, 12)}`);
      return { exit: 0 };
    }
    return { exit: await runSharedUnitList({ o, io, stage, commit: pinned.commit, selected: selection.selected, inventory, receiptDir, base, unmatched: selection.unmatched }) };
  }
  if (allUnitsFlag) {
    const id = option("--all-units-exception");
    let granted;
    try {
      const shown = git("show", `${pinned.commit}:${ALL_UNITS_EXCEPTIONS_FILE}`);
      granted = authorizeAllUnits({ id, record: failed(shown) ? null : text(shown), now: o.now ? o.now() : Date.now() });
    } catch (e) {
      if (!e.code?.startsWith("RK_ALL_UNITS_")) throw e;
      o.err(`${tag} BLOCKED: ${e.message}; nothing ran`);
      return { exit: e.code === ALL_UNITS_ERRORS.usage ? 2 : 1 };
    }
    args.splice(args.indexOf("--all-units-exception"), 2);
    o.out(`${tag} ALL UNITS EXCEPTION ${describeAllUnitsException(granted)}`);
    return {};
  }
  // RightKit's own sample apps (ci/sample-apps.json of the commit: the all-packages e2e app) are built as apps, from inside their directory:
  // the run is scoped to that app (appRootOf), never the whole repository, exactly as a product app's run is.
  const sample = sampleAppOf({ o, root, git, commit: pinned.commit });
  if (sample) {
    o.out(`${tag} RightKit sample app ${sample}: built as an app, scoped to ${sample} (ci/sample-apps.json)`);
    return {};
  }
  o.err(`${tag} BLOCKED: RK_UNIT_NOT_NAMED: RightKit names its unit on every run: --unit <cargo:rightkit-name|npm:@rightkit/name>, --changed [--base <ref>] (changed units plus dependents, planned from the commit), --units-file <json written for this run>, or --all-units-exception <id> (Adrian's entry in ${ALL_UNITS_EXCEPTIONS_FILE}); a whole-repository run is never the default; nothing ran`);
  return { exit: 1 };
}

async function runStages(o, args, root, git, io) {
  const option = (name) => { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; };
  const tag = "rightkit-gate:";
  const to = option("--to");
  // dev and test are door stages like the rest: the same commit-only route (select a commit, sync its snapshot under ownership, class A
  // of the snapshot), then right-release dev|test with the snapshot's app root as its cwd. They have no stage list of their own.
  // render (rightkit#1288) is a door stage of the same family: class A of the snapshot, then the app's declared view-render target built through the carrier and run headless.
  const devTest = to === "dev" || to === "test" || to === "render";
  let stages = devTest ? [] : stagesTo(to);
  if (!stages) { o.err(`${tag} --to must be one of ${[...STAGES, "dev", "test", "render"].join(", ")}${to === "tool" ? " (--to tool builds one named tool unit: --to tool --unit cargo:rightkit-qa or --to tool --unit cargo:rightkit-credentials)" : ""}`); return 2; }
  // Every stage the door runs has a row naming where it reads its source from (gate-stages.mjs STAGE_SOURCES, drift-tested in
  // door-stages.test.mjs); a stage added without one is refused rather than run with an unstated source.
  if (!STAGE_SOURCES[to]) { o.err(`${tag} BLOCKED: stage ${to} has no STAGE_SOURCES row naming its source; nothing ran`); return 2; }
  const platform = option("--platform");
  const tier = option("--tier");
  if (stages.includes("package") && !PLATFORMS.includes(platform)) { o.err(`${tag} --to ${to} needs --platform ${PLATFORMS.join("|")}`); return 2; }
  // iOS and Android (Adrian, 2026-10-26: every app build goes through the door, class A first): check, compile and package run here as
  // for desktop; there is no installed-QA lane for them, so the qualify stage does not exist for a mobile platform and is never
  // recorded as passed. Qualification is the store's test track, after the release stage uploads the sealed build from this host
  // (TestFlight, Play internal track; right-release upload --apply, no patch/update tier).
  const mobile = MOBILE_PLATFORMS.has(platform);
  if (mobile && to === "qualify") { o.err(`${tag} --to qualify has no stage for ${platform}: there is no installed-QA lane for a mobile build; qualification is the store's test track after --to release. Run --to package or --to release.`); return 2; }
  if (mobile) stages = stages.filter((s) => s !== "qualify");
  if (stages.includes("release") && !mobile && !["patch", "update"].includes(tier)) { o.err(`${tag} --to release needs --tier patch|update (chosen explicitly, never defaulted)`); return 2; }
  // The commit this run is for is selected FIRST (--commit <rev>, default HEAD; the door snapshot's commit inside a door run; HEAD on hosted CI, whose runner
  // checkout is the commit), and the app scope is read from THAT commit's tree (committedConfigDirs): the checkout's present files never decide it.
  const hosted = o.env.GITHUB_ACTIONS === "true";
  let selected = null;
  if (!o.doorSnapshot && !hosted) {
    const snapModule = await import("../snapshot.mjs");
    try { selected = snapModule.resolveCommit(root, option("--commit") ?? "HEAD"); }
    catch (e) {
      if (o.routeProbe) return await runRouteProbe(o, args, root, git, { to, platform, stages: devTest ? [] : stages, devTest, appDir: "." }); // the probe names the same refusal in its own words
      o.err(`${tag} cannot resolve ${option("--commit") ?? "HEAD"} to a commit: ${e.message}`); return 2;
    }
  }
  const scopeCommit = o.doorSnapshot ? o.doorSnapshot.commit : selected ? selected.commit : text(git("rev-parse", "HEAD")).trim();
  const inventory = committedConfigDirs({ git, commit: scopeCommit });
  if (inventory.refusal) { o.err(`${tag} cannot read the app configuration of the selected commit: ${inventory.refusal}`); return 2; }
  const { appRoot, appDir } = appRootOf({ root, cwd: o.cwd, configDirs: inventory.dirs });
  // `--route`: which route this stage takes here and whether it refuses, answered without running anything (rightkit#1297).
  if (o.routeProbe) return await runRouteProbe(o, args, root, git, { to, platform, stages: devTest ? [] : stages, devTest, appDir });
  // Commit-only build (GATE-CONTRACT §1): on every host (not hosted CI, whose runner checkout is the commit) the door selects a commit
  // (--commit <rev>, default HEAD), takes the repository's build ownership, syncs the commit's gate-owned snapshot once, and every stage
  // below reads that snapshot; the checkout may hold anything and is never a build source. Every non-hosted host takes this route: Mac
  // and Dell compile it through the RustRight snapshot carrier (`deps.snapshotBroker`, wired by `productionEntryDeps`; item 7 rows 7.20
  // to 7.24, rightkit#1301), right-release's own children included. Once it is selected any failure refuses; there is no retry on the
  // checkout, and no flag or environment variable selects another route.
  // The route and every refusal the door can meet before it takes ownership are `routePlan`'s, the one function the `--route` probe
  // also answers from (rightkit#1297 L1): the probe and the real run cannot drift, and the door-route parity test compares the two.
  if (!o.doorSnapshot) {
    const whichRoute = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
    const routeExecutor = (o.resolveExecutor ?? gateResolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which: whichRoute, readFile: o.readFile });
    let decided;
    try { decided = routePlan(o, { to, stages: devTest ? [] : stages, devTest, executor: routeExecutor, stagePlatform: platform, capabilities: await brokerCapabilityStateWithHandshake(o, { to, executor: routeExecutor }), appConfig: selected && to !== "check" ? routeAppConfig({ git, commit: selected.commit, appDir, platform: devTestPlatform(args) }) : null }); }
    catch (e) { o.err(`${tag} ${e.message}`); return 2; }
    if (decided.refusal) { o.err(`${tag} BLOCKED: ${decided.refusal.code}: ${decided.refusal.message}; nothing ran${decided.route === "snapshot" ? " and the checkout is never built" : ""}`); return 1; }
    const notPublisher = decided.route === "snapshot" ? releaseRouteRefusal(o, { stages, platform }) : null;
    if (notPublisher) { o.err(`${tag} BLOCKED: ${notPublisher.code}: ${notPublisher.message}; nothing ran and the checkout is never built`); return 1; }
    // The declared Linux broker admits snapshots only under its own data root: refused before any ownership or sync when the gate's root is elsewhere.
    if (decided.route === "snapshot" && !hosted) {
      const snapSync = await import("../snapshot.mjs");
      const mismatch = await declaredSnapshotRootRefusal(o, routeExecutor, o.snapshotRoot ?? snapSync.snapshotRoot({ platform: o.platform, env: o.env }));
      if (mismatch) { o.err(`${tag} BLOCKED: ${mismatch.code}: ${mismatch.message}; nothing ran`); return 1; }
    }
  }
  if (!o.doorSnapshot && !hosted) {
    const snap = await import("../snapshot.mjs");
    const { acquireBuildOwnership, ownershipFile } = await import("../build-ownership.mjs");
    const snapshotsAt = o.snapshotRoot ?? snap.snapshotRoot({ platform: o.platform, env: o.env });
    let own;
    try { own = (o.acquireOwnership ?? acquireBuildOwnership)({ file: ownershipFile({ platform: o.platform, beside: o.snapshotRoot, repository: snap.repositoryOf(root).key }), stage: `door:${to}`, snapshot: path.join(snapshotsAt, snap.repositoryOf(root).key), commit: selected.commit, platform: o.platform, env: o.env }); }
    catch (e) { if (e.code !== "busy" && e.code !== "refused") throw e; o.err(`${tag} BLOCKED: ${e.message}; nothing ran${e.code === "refused" ? ", nothing created" : ""}`); return 1; }
    o.out(`${tag} build ownership ${own.joined ? "joined" : "held"} ${own.handle}${own.reclaimed ? ` (reclaimed from ${own.reclaimed.from}: ${own.reclaimed.proofs.join("; ")})` : ""}`);
    let synced = null;
    try {
      // One sync per door run: a second one would prune what class A installed into the snapshot.
      // The seam (reset plan section 3): the lock body is { pid, identity, commit, token } and the lock is held through the whole door run, every job and
      // every test, released in the finally below; the broker refuses a request whose snapshot is not locked for its commit by a live holder.
      let seamLock = {};
      if (/^[0-9a-f]{40}$/.test(String(selected.commit))) {
        const identity = processIdentity(process.pid, { platform: o.platform, spawnSync: (c, a, opts) => o.spawn(c, a, opts) });
        // A dead holder's lock is reclaimed only on the broker's SNAPSHOT_STATUS proof that the snapshot is IDLE for exactly that owner (gate-seam.mjs); no broker, no proof.
        const snapshotPath = path.join(snapshotsAt, snap.repositoryOf(root).key);
        // Everything the judge needs is resolved HERE and bound into it (snapshotReclaimJudge): the route's executor and this function's `which` are out of scope or
        // not yet initialized when the sync calls the judge (astra, 2026-11-09, twice), and a caught ReferenceError would read as "no proof" forever.
        let seamClient = null;
        try { seamClient = (o.resolveExecutor ?? gateResolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which: whichIn(o), readFile: o.readFile })?.client ?? null; } catch { seamClient = null; }
        const judge = snapshotReclaimJudge({ o, client: seamClient, snapshotPath });
        if (identity) seamLock = { body: snapshotLockBody({ identity, commit: selected.commit }), judge, hold: true };
        else o.err(`${tag} note: this process's birth identity could not be read; the snapshot lock is the legacy one and is released after the sync`);
      }
      try { synced = (o.syncSnapshot ?? snap.syncSnapshot)({ checkout: root, rev: selected.commit, root: snapshotsAt, lock: seamLock }); }
      catch (e) { o.err(`${tag} ${e.code === "error" ? "RK_CHECK_ERROR" : "BLOCKED"}: snapshot ${e.message}; nothing ran`); return e.code === "error" ? 2 : 1; }
      o.out(`${tag} snapshot ${synced.path} is commit ${synced.commit} tree ${synced.tree} (written=${synced.written} removed=${synced.removed} unchanged=${synced.unchanged})`);
      if (synced.identity) o.out(`${tag} source identity ${synced.identity.file}: ${synced.identity.written ? "written" : synced.identity.kept ? "kept" : `not written (${synced.identity.reason})`}`);
      return await runStages({ ...o, buildOwnership: own, doorSnapshot: { own, path: synced.path, commit: synced.commit, tree: synced.tree, root: snapshotsAt, ledger: { entries: [] } } }, args, root, git, { ...io, buildOwnership: own });
    } finally {
      // The seam lock, held through the batch, is released only here, after every job and test of the door run ended.
      try { synced?.release?.(); } catch { /* a lock is removed by its holder only; a failed removal leaves it for the stale-holder rule */ }
      const r = own.release();
      if (r?.released === false && !own.joined) o.err(`${tag} build ownership ${own.handle} kept: ${r.why}`);
    }
  }
  if (devTest) return await runDevTest(o, args, root, git, io, to, appRoot, appDir);
  const ds = o.doorSnapshot ?? null;
  const commit = ds ? ds.commit : text(git("rev-parse", "HEAD")).trim();
  const tree = ds ? ds.tree : text(git("rev-parse", "HEAD^{tree}")).trim();
  const common = text(git("rev-parse", "--git-common-dir")).trim();
  if (!/^[0-9a-f]{40}$/.test(commit) || !tree || !common) { o.err(`${tag} cannot read HEAD and the git directory`); return 2; }
  const commonDir = path.resolve(root, common);
  // The snapshot route reads only the commit; the checkout may hold anything (GATE-CONTRACT §1: no dirty-tree condition).
  const dirty = ds ? null : git("status", "--porcelain", "--untracked-files=no");
  if (!ds && (failed(dirty) || text(dirty).trim())) { o.err(`${tag} BLOCKED: the checkout differs from HEAD; every stage works on the commit, so commit or stash first`); return 1; }

  const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
  const executor = (o.resolveExecutor ?? gateResolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile }); applyExecutorEnv(o, executor);
  if (executor.kind === "refuse") { o.err(`${tag} BLOCKED: ${executor.problem}`); return 1; }
  const version = (cmd, a) => { const r = o.spawn(cmd, a, { cwd: root }); return failed(r) ? null : text(r).trim().split(/\r?\n/)[0]; };
  const listed = git("ls-tree", "-r", "--name-only", "-z", commit);
  const lockFiles = {};
  for (const f of failed(listed) ? [] : text(listed).split("\0").filter((x) => /(^|\/)(Cargo\.lock|pnpm-lock\.yaml)$/.test(x))) {
    const shown = git("show", `${commit}:${f}`); if (!failed(shown)) lockFiles[f] = text(shown);
  }
  const binding = bindingOf({
    tree, lockFiles, gateFile: fileURLToPath(import.meta.url), executor: o.brokerException ? `${executor.kind}:exception:${o.brokerException.exceptionId}` : executor.kind,
    toolchain: { classA: { "cargo-deny": version("cargo-deny", ["--version"]), "cargo-audit": version("cargo-audit", ["--version"]), actionlint: version("actionlint", ["-version"]), shellcheck: version("shellcheck", ["--version"]) }, node: process.version, rustc: version(executor.kind === "broker" && executor.cargo.length > 1 ? executor.cargo[0] : "rustc", executor.kind === "broker" && executor.cargo.length > 1 ? ["rustc", "-V"] : ["-V"]), pnpm: version("pnpm", ["--version"]) },
  });
  // Release writes to R2. The write credential lives only in the trusted publisher: a GitHub Actions job in the protected
  // publication environment, which sets RIGHTKIT_PUBLISHER=github-environment. Hosts and agents never hold it, so the door refuses the
  // stage anywhere else rather than fail half way through an upload.
  if (stages.includes("release") && !mobile && !(o.env.GITHUB_ACTIONS === "true" && o.env.RIGHTKIT_PUBLISHER === "github-environment")) {
    o.err(`${tag} BLOCKED: --to release runs only in the trusted publisher (the release workflow's protected publication job). Run --to qualify here, then dispatch the release workflow for ${commit.slice(0, 12)}`);
    return 1;
  }
  // --receipt-in <file>: the receipt an earlier job of the same pipeline wrote (another runner: CI's package job, then its qualify and
  // publish jobs). It is taken only for this commit and the same source binding (tree, lockfiles, gate); it then stands in for the
  // stages it records, and the sealed bytes on disk must still hash to its manifest digest before anything uses them.
  const receiptIn = option("--receipt-in");
  if (receiptIn !== undefined) {
    let imported = null;
    try { imported = JSON.parse(o.readFile(path.resolve(o.cwd, receiptIn))); } catch { /* refused below */ }
    if (imported?.brokerException) { o.err(`${tag} BLOCKED: --receipt-in ${receiptIn} was built outside the broker by exception ${imported.brokerException.exceptionId}; it is never taken as a broker-built receipt`); return 1; }
    if (!imported || imported.schema !== RECEIPT_SCHEMA || imported.commit !== commit || !sameSource(imported.binding, binding)) {
      o.err(`${tag} BLOCKED: --receipt-in ${receiptIn} is not a ${RECEIPT_SCHEMA} receipt for ${commit.slice(0, 12)} with this tree, these lockfiles and this gate`);
      return 1;
    }
    (o.writeReceipt ?? writeReceipt)({ commonDir, receipt: imported });
  }
  const previous = (o.readReceipt ?? readReceipt)({ commonDir, commit, app: appDir });
  // Same host and inputs: every recorded stage counts. Same source on another host: only the stages bound to sealed bytes (package and
  // later) carry over; check and compile run again here.
  const receipt = previous && sameBinding(previous.binding, binding) ? previous
    : previous && sameSource(previous.binding, binding) ? { ...previous, binding, stages: Object.fromEntries(Object.entries(previous.stages ?? {}).filter(([k]) => k !== "check" && k !== "compile")) }
      : { schema: RECEIPT_SCHEMA, commit, app: appDir, binding, stages: {} };
  // Every run under a broker exception records it in the receipt (broker-exception.mjs): id, Adrian's quote, issue, units and host.
  if (o.brokerException) receipt.brokerException = o.brokerException;
  const receiptOut = option("--receipt-out");
  const save = () => {
    (o.writeReceipt ?? writeReceipt)({ commonDir, receipt });
    if (receiptOut !== undefined) o.writeFile(path.resolve(o.cwd, receiptOut), `${JSON.stringify(receipt, null, 2)}\n`);
  };

  const cli = fileURLToPath(new URL("../cli/right-release.mjs", import.meta.url));
  // On the snapshot route right-release runs INSIDE the snapshot (its sources, config and every command it spawns resolve there);
  // the door run records the split, so the child can prove which snapshot it may read and which checkout holds its outputs
  // (source-split.mjs). Anywhere else it runs in the app root of the checkout, which hosted CI's runner holds at the commit.
  const sourceOf = ds ? sourceSplitOf({ root, ds, appDir }) : null;
  const rightRelease = async (stage, cliArgs, extraEnv = {}) => {
    // A broker host's child reaches the broker only through this stage's carrier (carried-cargo.mjs): its Cargo calls, and its pnpm, node and QA
    // steps as adapter jobs with the third-party children's compiles nested under them. Every call is in the gate-owned ledger and is verified
    // again after the child ends.
    const carrier = await carrierForChildren({ o, ds, root, git, executor, stage, tag });
    if (carrier.refusal) { o.err(`${tag} BLOCKED: ${carrier.refusal}; nothing ran`); return false; }
    const run = startRun({ commonDir, commit, tree, stage, ...(sourceOf ? { source: sourceOf } : {}) });
    let childOk = false;
    try {
      o.out(`${tag} [${stage}] right-release ${cliArgs.join(" ")}${sourceOf ? ` (from the snapshot ${ds.path})` : ""}`);
      const r = (o.spawnInherit ?? inheritLaunch)(process.execPath, [cli, ...cliArgs], { cwd: sourceOf ? snapshotAppRootOf(ds.path, appDir) : appRoot, env: carriedEnv(carrier, { ...o.env, ...run.env }, extraEnv) });
      childOk = r.status === 0;
    } finally { endRun(run); }
    const carriedOk = await carrier.finish({ stage });
    return childOk && carriedOk;
  };

  o.out(`${tag} --to ${to} for ${commit.slice(0, 12)}${appDir === "." ? "" : ` (app ${appDir})`}: ${stages.join(" -> ")}; executor ${executor.kind}`);
  // A later stage on the sealed bytes: check, compile and package already passed for them (in this receipt) and the bytes on disk are
  // the ones they sealed, so they are not run again.
  // The release configs of the commit (under this app) that declare `sidecars`: the commit's config decides on the snapshot route, never the checkout's.
  const sidecarConfigsOf = () => committedSidecarConfigs({ o, git, commit, appDir, base: ds ? ds.path : root });
  // right-release keeps its outputs (`.right-release`: sealed sets, QA receipts, state) at the checkout's git root, never a nested app's
  // directory (build-release.mjs gitRootOf): every lookup of a sealed set or a QA receipt reads there.
  const releaseRoot = root;
  const sealedNow = receipt.stages.package?.ok ? (o.sealedFor ?? sealedFor)({ root: releaseRoot, commit, platform }) : null;
  // The protected-receipt verifier pulls in the ownership policy modules (and their dependencies): loaded only for a run that qualifies or releases,
  // so an installed copy that only checks or compiles never needs them (GB-14).
  const qualifyReuse = stages.includes("qualify") || stages.includes("release") ? await import("../qualify-reuse.mjs") : null;
  const installedQaReceiptDigest = (...a) => qualifyReuse.installedQaReceiptDigest(...a);
  const qualifyReuseProblem = (...a) => qualifyReuse.qualifyReuseProblem(...a);
  const qualifyRecordProblem = (recorded) => (o.qualifyReuseProblem ?? qualifyReuseProblem)({ recorded, sealedNow, repoRoot: releaseRoot, platform, ...(o.verifyQa !== undefined ? { verify: o.verifyQa } : (o.env.GITHUB_ACTIONS === "true" && o.env.RIGHTKIT_PUBLISHER === "github-environment" ? { verify: null } : {})) });
  const sealedIntact = Boolean(sealedNow && sealedNow.id === receipt.stages.package.sealed && sealedNow.digest === receipt.stages.package.manifestSha256);
  // Every deterministic scan (full class A) runs before any compile, for every door stage. The app rules run at the tier the stage
  // owes: dev, test and package take the compile-tier rules; qualify and release add the release-only rules (approvals, ratified
  // rulings, notice digests), which are conditions for publishing, not for compiling (Adrian, 2026-10-30, rightkit#1296; the
  // 2026-10-28 change to release tier everywhere was never his decision and blocked HeardRight's dev and test builds).
  const earlyTier = ["qualify", "release"].includes(to) ? "release" : "compile";
  const tierAtLeast = (have, need) => have === "release" || need === "compile";
  // GB-12 (GATE 36/44/86, E18): Class A admission runs at every stage boundary a door run crosses; an outer receipt boolean or an intact
  // sealed set never stands in for it. The deterministic scans reuse through their own complete result-cache key (never this receipt's
  // coarse binding, which names the entry file only), and the live scans (pnpm-lockfile, advisory-exceptions) are never cached; cargo-deny, cargo-audit
  // and pnpm-audit are keyed on lock, configuration, advisory identity and the UTC day (GC-A19), and the door's `live-scans` refresh re-asks the registry. The one exception is the trusted
  // publisher's upload (`--to release` on a desktop platform): it runs no build environment, only uploads bytes whose earlier stages
  // (Class A included) another job of the same pipeline recorded in the receipt it imported (--receipt-in).
  const publisherUploadOnly = to === "release" && !mobile;
  // GB-15: a nested app's Class A runs its executing scans for that app's dependency closure; an app at the repository root is the repository.
  const appScope = appDir === "." ? [] : ["--scope", appDir];
  // rightkit#1491: an app whose own @rightkit/release is below the host gate's floor (DIRECT_ROUTE_RELEASE_FLOOR) is refused before any compile
  // on a broker host: its child would carry the non-compiling cargo calls to the broker, which refuses them (GC-A11). The pin is judged from the
  // commit's package.json before the check stage; the version installed in the snapshot (Class A installs it) before the compile stage.
  const appPackageJson = () => { const shown = git("show", `${commit}:${appDir === "." ? "" : `${appDir}/`}package.json`); return failed(shown) ? null : text(shown); };
  const pinFloor = async (installed) => {
    if (executor.kind !== "broker" || !stages.includes("compile") || !ds) return null;
    const packageJsonText = appPackageJson();
    if (packageJsonText === null) return null;
    const { releasePinFloorProblem } = await import("../release-pin-floor.mjs");
    let installedVersion = null;
    if (installed) {
      for (const dir of new Set([snapshotAppRootOf(ds.path, appDir), ds.path])) {
        try { installedVersion = JSON.parse(fs.readFileSync(path.join(dir, "node_modules", "@rightkit", "release", "package.json"), "utf8")).version ?? null; break; } catch { /* not installed here */ }
      }
    }
    return releasePinFloorProblem({ packageJsonText, installedVersion, where: appDir === "." ? "package.json" : `${appDir}/package.json` });
  };
  const phaseTimings = [];
  for (const stage of stages) {
    if (stage === "check" && publisherUploadOnly && receipt.stages.check?.ok && tierAtLeast(receipt.stages.check.ruleTier ?? "compile", earlyTier)) { o.out(`${tag} [check] already passed (${receipt.stages.check.ruleTier ?? "compile"} tier) for this exact input (commit, tree, lockfiles, gate, toolchain)`); continue; }
    if (stage === "compile" && receipt.stages.compile?.ok) {
      // GB-13 (GATE E6/E7/E16): a recorded compile stands only while the outputs it recorded are still on disk and the compile environment is
      // the one it ran in; otherwise the compile runs again, warm (never a cleared target), unless the sealed bytes below carry it.
      const outputsProblem = (o.compileReuseProblem ?? compileReuseProblem)(receipt.stages.compile, { env: o.env });
      if (!outputsProblem) { o.out(`${tag} [${stage}] already passed for this exact input (commit, tree, lockfiles, gate, toolchain) and its outputs are intact`); continue; }
      if (!sealedIntact) o.out(`${tag} [${stage}] the recorded compile no longer stands: ${outputsProblem}; running it again (warm)`);
    }
    if (((stage === "check" && publisherUploadOnly) || stage === "compile" || stage === "package") && sealedIntact) {
      o.out(`${tag} [${stage}] already passed for the sealed set ${sealedNow.id} (manifest ${sealedNow.digest.slice(0, 12)})`); continue;
    }
    // The publisher (another runner, often another OS) cannot repeat installed QA; it takes the qualify result recorded for exactly
    // these sealed bytes. --to qualify itself always runs it.
    // GB-14: the record is reused only while the protected installed-QA receipt it names still verifies over the current state (the existing
    // verifier: policy bundle, floor, required checks, source, artifact hashes); the publisher, which holds no receipt key, is held to the
    // recorded receipt digest and the manifest digest, and the upload admission verifies the receipt itself.
    if (stage === "qualify" && to !== "qualify" && sealedIntact && receipt.stages.qualify?.ok && receipt.stages.qualify.manifestSha256 === sealedNow.digest) {
      const problem = qualifyRecordProblem(receipt.stages.qualify);
      if (!problem) { o.out(`${tag} [qualify] already passed for the sealed set ${sealedNow.id} (manifest ${sealedNow.digest.slice(0, 12)}; protected QA receipt ${receipt.stages.qualify.qaReceiptDigest.slice(0, 12)} verified)`); continue; }
      o.out(`${tag} [qualify] the recorded result no longer stands: ${problem}; it runs again`);
    }
    // The publisher only uploads. It never checks out a build environment, signs or runs installed QA, so every earlier stage must
    // already stand in its receipt for exactly the sealed bytes on disk; anything else is refused, never rebuilt or requalified here.
    if (to === "release" && !mobile && stage !== "release") {
      o.err(`${tag} BLOCKED: [${stage}] --to release only uploads: the receipt must already record ${stage} for the sealed bytes on disk (${sealedNow ? `manifest ${sealedNow.digest.slice(0, 12)}` : "none found"}); run --to qualify on the host and stage again`);
      return 1;
    }
    // The start of every stage: the snapshot is exactly tree + dependencies + the ledger of the stages before it (7.08, decision 2 Q1).
    if (ds && !(await openSnapshotStage({ o, ds, root, stage, tag }))) { receipt.stages[stage] = { ok: false, at: new Date().toISOString(), snapshot: { path: ds.path, tree: ds.tree, intactAfter: false }, ownership: ds.own.handle }; save(); o.err(`${tag} --to ${to} stopped before ${stage}: the snapshot is not the commit's bytes plus the ledger; nothing ran`); return 2; }
    let ok = false; const at = new Date().toISOString(); const stageStartedMs = Date.now(); let extra = {}; let directReceipt = null;
    if (stage === "check") {
      { const problem = await pinFloor(false); if (problem) { o.err(`${tag} BLOCKED: ${problem.message}; no class A row ran and nothing compiled`); return 1; } }
      ok = (await (o.runCheck ?? (() => main(["check", "--commit", ds ? commit : "HEAD", "--full-a", ...(ds ? ["--snapshot", ds.path] : []), ...appScope, "--rule-tier", earlyTier], { ...io, cwd: root, ...(ds ? { buildOwnership: ds.own } : {}) })))(earlyTier)) === 0;
      extra = { ruleTier: earlyTier };
    }
    else if (stage === "compile") {
      { const problem = await pinFloor(true); if (problem) { o.err(`${tag} BLOCKED: ${problem.message}; nothing compiled`); return 1; } }
      // A Tauri crate with externalBin needs its sidecars in src-tauri/binaries before it compiles (its build script refuses
      // otherwise), so every release config that declares `sidecars` is staged first, through right-release under this door run.
      const sidecarConfigs = sidecarConfigsOf();
      ok = true;
      // rightkit#1462 (ADR-001 section 7): a run that goes on to package runs the release admission, read-only, before anything compiles: the same rows `right-release build`
      // runs at its start (files: config and inputs; host: tools, credentials, signing hosts; network: pin freshness against the registries), asked through its own
      // `--dry-run`, which prints the rows and exits non-zero on a finding before any lock, cache, install or child build exists. A refusal then costs seconds, not a class B.
      if (stages.includes("package") && !mobile && PLATFORMS.includes(platform)) {
        o.out(`${tag} [compile] release admission dry-run for ${platform} before any compile (right-release build --platform ${platform} --dry-run)`);
        if (!await rightRelease(stage, ["build", "--platform", platform, "--dry-run"])) { o.err(`${tag} BLOCKED: [compile] the release admission (pin freshness, host tools, credentials, signing hosts) refused ${platform}; nothing compiled`); ok = false; }
      }
      // On the snapshot route the sidecars are staged from the snapshot's own config and sources through right-release's source split
      // (the staged files and receipt land beside the sources the compile reads; the caches and build trees stay the checkout's), before
      // `main --all` compiles. A broker host's stage-sidecar child reaches Cargo through the stage's carrier (carried-cargo.mjs).
      // right-release runs in appRoot (nested apps), so the repository-relative config is handed over absolute (Astra, second pass).
      // On the snapshot route sidecars are staged ONCE per door run by the ledger bookkeeping (attribution diff, immutable staged entries).
      // rightkit#1331: the declared compile inputs that are the app's web build output are produced in the snapshot before ANY compile reads them,
      // the sidecar builds included: the broker admits a compile only when every declared input is in the snapshot (HeardRight Mac 2026-11-02:
      // stage-sidecar's cargo build REJECTED "snapshot admission: tauri-app-next/dist: No such file or directory").
      if (ok && ds) ok = await stageWebBuildOnce({ o, ds, root, git, appDir, cli, commonDir, commit, tree, stage, executor });
      // The Windows package-identity MSIX a Tauri app bundles (its release config's windows.identity), produced in the snapshot on Windows only, for the same reason.
      if (ok && ds) ok = await stageWindowsIdentityOnce({ o, ds, root, git, appDir, cli, commonDir, commit, tree, stage, executor });
      if (ok && ds) ok = await stageSidecarsOnce({ o, ds, root, git, configs: sidecarConfigs, appDir, cli, commonDir, commit, tree, stage, executor });
      else for (const config of ok ? sidecarConfigs : []) if (!await rightRelease(stage, ["stage-sidecar", "--config", path.join(root, config)])) { ok = false; break; }
      let compiledDirs = []; let compiledBins = null; let qualifyOwed = [];
      const reportOutputs = (dirs, bins = null) => { compiledDirs = [...dirs]; compiledBins = bins; };
      if (ok) ok = (await (o.runCompile ?? (() => main(ds ? ["--all", "--commit", commit, "--snapshot", ds.path] : ["--all"], { ...io, cwd: root, readStdin: () => "", ...(ds ? { buildOwnership: ds.own, classAEarlier: true, snapshotLedger: ds.ledger?.entries ?? [], onCompileLedger: (entries) => { ds.compileAdmitted = entries; }, onDirectReceipt: (r) => { directReceipt = r; }, onCompileOutputs: reportOutputs, onQualifyOwed: (list) => { qualifyOwed = list; } } : {}) })))({ reportOutputs })) === 0;
      if (directReceipt) extra = { ...extra, direct: directReceipt };
      // What the plan left to the qualify stage is recorded with the compile, so qualification can prove it is not lost (rightkit#1332, #1333).
      if (qualifyOwed.length) extra = { ...extra, qualifyOwed };
      // What this compile produced, recorded with its result so a later run can prove the outputs are still there (GB-13).
      if (ok) extra = { ...extra, outputs: compileOutputsRecord({ dirs: compiledDirs, env: o.env, builtBins: compiledBins }) };
      // The cross-check with `.rightkit/compile-inputs.json`: a declared input that covers ledger entries must carry the digest of those
      // very bytes in the compile receipt (the broker digests declared inputs in place in the snapshot, BC:39).
      if (ok && ds && directReceipt?.declaredInputDigests) {
        const { declaredInputProblems } = await import("../stage-ledger.mjs");
        const { judgeCompileInputs } = await import("../compile-inputs.mjs");
        const bad = declaredInputProblems({ snapshot: ds.path, entries: ds.ledger?.entries ?? [], receiptDigests: directReceipt.declaredInputDigests, judge: (dir) => judgeCompileInputs(dir) });
        if (bad.length) { for (const p of bad.slice(0, 10)) o.err(`${tag} RK_CHECK_ERROR: [compile] ${p}`); ok = false; }
      }
    }
    else if (stage === "package") {
      // On the snapshot route the sidecars are staged ONCE per door run, before the first stage that builds the Tauri crate: here when
      // compile was already passed (a separately invoked `--to package` re-stages after the sync, so it never meets a missing externalBin).
      // right-release then takes its identity from the selected commit and the verified snapshot (release-source-identity.mjs, row 7.04),
      // never the checkout's HEAD, status or branch; its outputs, state and the sealed directory stay in the checkout's `.right-release`.
      // The declared web build and Windows identity inputs first, as in the compile stage: the sidecar builds are broker-admitted only when every declared input is in the snapshot.
      ok = true;
      if (ds) ok = await stageWebBuildOnce({ o, ds, root, git, appDir, cli, commonDir, commit, tree, stage, executor });
      if (ok && ds) ok = await stageWindowsIdentityOnce({ o, ds, root, git, appDir, cli, commonDir, commit, tree, stage, executor });
      if (ok && ds) {
        // The package triple: a Mac recipe with `stageSidecars` builds for its own triple (universal-apple-darwin by default), so the gate stages that triple here, under its own
        // producer carrier, and the recipe's prepare step (`stage-sidecar --target <triple>`) then finds it current and changes nothing.
        const packageConfigs = sidecarConfigsOf();
        const packageTarget = await packageSidecarTarget({ o, ds, configs: packageConfigs });
        ok = await stageSidecarsOnce({ o, ds, root, git, configs: packageConfigs, appDir, cli, commonDir, commit, tree, stage, executor, target: packageTarget });
      }
      // Row 7.04c (R-T1, 7.09): the snapshot holds no Cargo target. On a direct host the checkout's output directory is resolved here, in the primary
      // checkout, and handed to right-release absolute: it binds every manifest's target for this stage, and the sealed manifest must record it back
      // (the door never trusts the child's value alone). One workspace is pinned as a whole; several are refused, naming them. A broker host's target
      // is the carrier's (rows 7.20 to 7.24): the carried calls' signed receipts name it (carried-cargo.mjs), and no CARGO_TARGET_DIR is passed.
      let packagePin = { dir: null };
      if (ok && ds && executor.kind !== "broker") {
        packagePin = await cargoOutputPinForScope({ o, git, root, commit, executor, snapshot: ds.path, ...(appDir === "." ? {} : { scopeDirs: [appDir] }) });
        if (packagePin.refusal) { o.err(`${tag} BLOCKED: RK_SNAPSHOT_OUTPUT_UNPINNED: [package] ${packagePin.refusal}; nothing was built`); ok = false; }
        else if (packagePin.dir) o.out(`${tag} [package] output directory for the Cargo workspace: ${packagePin.dir} (the checkout's)`);
      }
      // GB-17: several Cargo workspaces: each of right-release's and its children's Cargo calls is bound to ITS workspace's target through the per-call wrapper.
      let packageMulti = null;
      if (ok && packagePin.dirs?.length > 1) {
        // The package stage builds the workspace that holds the app's release config (its Tauri app), never a test-only workspace beside it.
        const builtWorkspaces = [...new Set(sidecarConfigsOf().map((c) => path.posix.dirname(String(c).replaceAll("\\", "/"))))];
        packageMulti = await bindUnitCargo({ o, git, root, ds, executor, which: whichIn(o), onlyWorkspaces: builtWorkspaces, ...(appDir === "." ? {} : { scopeDirs: [appDir] }) });
        if (packageMulti.refusal) { o.err(`${tag} BLOCKED: RK_SNAPSHOT_OUTPUT_UNPINNED: [package] ${packageMulti.refusal}; nothing was built`); ok = false; }
        else if (packageMulti.note) o.out(`${tag} [package] ${packageMulti.note}`);
      }
      const packageEnv = packagePin.dir ? { CARGO_TARGET_DIR: packagePin.dir } : (packageMulti?.env ?? {});
      ok = ok && await rightRelease(stage, ["build", "--platform", platform], packageEnv);
      const sealed = ok ? (o.sealedFor ?? sealedFor)({ root: releaseRoot, commit, platform }) : null;
      if (ok && !sealed) { o.err(`${tag} [package] the build reported success but sealed nothing for ${commit.slice(0, 8)} on ${platform}`); ok = false; }
      if (sealed) extra = { platform, sealed: sealed.id, manifestSha256: sealed.digest };
      // The cross-check: the Cargo targets the sealed manifest records are the directory this door resolved itself. A different one voids the stage.
      try { packageMulti?.cleanup?.(); } catch { /* the wrapper's map is the gate's own temp directory */ }
      if (ok && sealed && ds && (packagePin.dir || packagePin.dirs?.length > 1)) {
        let recorded = null;
        try { recorded = JSON.parse(String(o.readFile(sealed.manifest))).boundTargets ?? null; } catch { recorded = null; }
        const { crossCheckBoundTargets, SPLIT_TARGET_DRIFT_CODE } = await import("../release-artifact-path.mjs");
        const drift = crossCheckBoundTargets({ recorded, expected: packagePin.dirs });
        if (drift.length) {
          for (const problem of drift.slice(0, 10)) o.err(`${tag} ${SPLIT_TARGET_DRIFT_CODE}: [package] ${problem}`);
          const closedDrift = await closeSnapshotStage({ o, ds, root, stage, step: stage, tag });
          receipt.stages[stage] = { ok: false, at, voided: SPLIT_TARGET_DRIFT_CODE, ...extra, snapshot: { path: ds.path, tree: ds.tree, intactAfter: closedDrift.ok }, ownership: ds.own.handle };
          save();
          o.err(`${tag} --to ${to} stopped at ${stage}: ${SPLIT_TARGET_DRIFT_CODE}; the sealed set is void and nothing after it ran`);
          return 2;
        }
      }
      // A broker host's Cargo target is the carrier's: every target the sealed manifest records is a directory a verified, signed receipt of this
      // stage named (`outputDir`), never one the child alone reported.
      if (ok && sealed && ds && executor.kind === "broker") {
        let recorded = null;
        try { recorded = JSON.parse(String(o.readFile(sealed.manifest))).boundTargets ?? null; } catch { recorded = null; }
        const { crossCheckBoundTargets, SPLIT_TARGET_DRIFT_CODE } = await import("../release-artifact-path.mjs");
        // The commit's workspaces decide whether a bound target is owed; the receipts compared are THIS package stage's own (rightkit#1301 L1).
        const owed = Object.keys(await snapshotCargoWorkspaces({ git, ds, root })).length > 0;
        const want = [...(ds.carriedByStage?.[stage] ?? [])];
        let drift = [];
        if (owed) {
          if (!Array.isArray(recorded) || !recorded.length) drift = ["the sealed manifest records no bound Cargo target; a broker host's package stage binds the output directory its carried build's receipt named"];
          else if (!want.length) drift = ["no verified receipt of this package stage names an output directory, so the recorded target cannot be checked"];
          else drift = crossCheckBoundTargets({ recorded, expected: want });
        }
        if (drift.length) {
          for (const problem of drift.slice(0, 10)) o.err(`${tag} ${SPLIT_TARGET_DRIFT_CODE}: [package] ${problem}`);
          const closedDrift = await closeSnapshotStage({ o, ds, root, stage, step: stage, tag });
          receipt.stages[stage] = { ok: false, at, voided: SPLIT_TARGET_DRIFT_CODE, ...extra, snapshot: { path: ds.path, tree: ds.tree, intactAfter: closedDrift.ok }, ownership: ds.own.handle };
          save();
          o.err(`${tag} --to ${to} stopped at ${stage}: ${SPLIT_TARGET_DRIFT_CODE}; the sealed set is void and nothing after it ran`);
          return 2;
        }
      }
    } else {
      const sealed = receipt.stages.package?.ok ? receipt.stages.package : null;
      const current = sealed ? (o.sealedFor ?? sealedFor)({ root: releaseRoot, commit, platform }) : null;
      if (!sealed || !current || current.id !== sealed.sealed || current.digest !== sealed.manifestSha256) {
        o.err(`${tag} BLOCKED: [${stage}] needs the sealed set the package stage produced for this commit, unchanged (sealed manifest digest)`);
        ok = false;
      } else if (stage === "release" && mobile) {
        // The host uploads the sealed mobile build to the store's test track itself (the credentials live in the host's credential
        // store); the release tier is owed first, as for desktop qualification.
        const releaseTier = receipt.stages.check?.ok && receipt.stages.check.ruleTier === "release"
          ? (o.out(`${tag} [release] the release tier already passed at check for this exact input; not run again`), 0)
          : await (o.runReleaseCheck ?? (() => main(["check", "--commit", ds ? commit : "HEAD", "--full-a", ...(ds ? ["--snapshot", ds.path] : []), ...appScope, "--rule-tier", "release"], { ...io, cwd: root, ...(ds ? { buildOwnership: ds.own } : {}) })))();
        if (releaseTier !== 0) { o.err(`${tag} [release] the release tier (every rule and scan) is red for ${commit.slice(0, 12)}; nothing was uploaded`); ok = false; }
        else ok = await rightRelease(stage, ["upload", "--platform", platform, "--release", sealed.sealed, "--apply"]);
        extra = { sealed: sealed.sealed, manifestSha256: sealed.manifestSha256, track: platform === "ios" ? "testflight" : "play-internal", ruleTier: "release" };
      } else if (stage === "qualify") {
        // Qualification owes the release tier: every rule and scan, the ones held out of compile admission included (app-check.mjs TIERS).
        const releaseTier = receipt.stages.check?.ok && receipt.stages.check.ruleTier === "release"
          ? (o.out(`${tag} [qualify] the release tier already passed at check for this exact input; not run again`), 0)
          : await (o.runReleaseCheck ?? (() => main(["check", "--commit", ds ? commit : "HEAD", "--full-a", ...(ds ? ["--snapshot", ds.path] : []), ...appScope, "--rule-tier", "release"], { ...io, cwd: root, ...(ds ? { buildOwnership: ds.own } : {}) })))();
        if (releaseTier !== 0) { o.err(`${tag} [qualify] the release tier (every rule and scan) is red for ${commit.slice(0, 12)}; nothing was qualified`); ok = false; }
        else {
          // rightkit#1332, #1333: what compile left to qualification (end-to-end crates, declared Swift packages) must have a declared run route here.
          const owedProblems = qualifyOwedProblem({ o, git, commit, appDir, base: ds ? ds.path : root, owed: receipt.stages.compile?.qualifyOwed ?? [] });
          for (const p of owedProblems) o.err(`${tag} BLOCKED: [qualify] ${p}`);
          ok = owedProblems.length === 0 && await rightRelease(stage, ["qa", "--stage", "installed", "--platform", platform, "--release", sealed.sealed]);
        }
        // The protected installed-QA receipt this run produced is journaled by digest (GB-14), so the record can be re-verified before any reuse.
        extra = { sealed: sealed.sealed, manifestSha256: sealed.manifestSha256, ruleTier: "release", ...(ok ? { qaReceiptDigest: (o.installedQaReceiptDigest ?? installedQaReceiptDigest)({ repoRoot: releaseRoot, releaseId: sealed.sealed, platform }) } : {}) };
      } else {
        if (!receipt.stages.qualify?.ok || receipt.stages.qualify.manifestSha256 !== sealed.manifestSha256) { o.err(`${tag} BLOCKED: [release] the sealed set was not qualified (installed QA) in this door run's receipt`); ok = false; }
        else if (qualifyRecordProblem(receipt.stages.qualify)) { o.err(`${tag} BLOCKED: [release] the recorded qualification no longer stands: ${qualifyRecordProblem(receipt.stages.qualify)}; nothing was uploaded`); ok = false; }
        else { ok = await rightRelease(stage, ["upload", "--release", sealed.sealed, "--platform", platform, "--tier", tier]); extra = { sealed: sealed.sealed, manifestSha256: sealed.manifestSha256, tier }; }
      }
    }
    if (ds) {
      // The stage covers the commit only if the snapshot is still its tree plus the ledger and the host is still this door's (E6, 7.08):
      // what the stage's children wrote may remain only as ledger entries, recorded by this gate's own walk and kept in the receipt (never
      // in the snapshot); a staged entry changed since staging voids the stage.
      const closed = await closeSnapshotStage({ o, ds, root, stage, step: stage, tag });
      const { receiptLedger } = await import("../stage-ledger.mjs");
      extra = { ...extra, snapshot: { path: ds.path, tree: ds.tree, intactAfter: closed.ok, ledger: receiptLedger(closed.entries) }, ownership: ds.own.handle };
      if (!closed.ok) { receipt.stages[stage] = { ok: false, at, ...extra }; save(); o.err(`${tag} --to ${to} stopped at ${stage}: the result covers no known bytes`); return 2; }
    }
    receipt.stages[stage] = { ok, at, ...extra };
    save();
    // rightkit#1463: each stage's own wall-clock time, and the run's phases in one line at the end.
    phaseTimings.push(`${stage} ${seconds(Date.now() - stageStartedMs)}${ok ? "" : " (failed)"}`);
    o.out(`${tag} [${stage}] ${ok ? "passed" : "failed"} in ${seconds(Date.now() - stageStartedMs)}`);
    if (!ok) { o.err(`${tag} --to ${to} stopped at ${stage}: nothing after it ran; phase timings: ${phaseTimings.join(", ")}`); return 1; }
  }
  o.out(`${tag} --to ${to} passed: ${stages.join(", ")}; phase timings: ${phaseTimings.join(", ") || "none ran"}`);
  return 0;
}

/**
 * rightkit-gate stage --platform mac|win [--tier patch|update]: hands a qualified sealed set to the trusted publisher. The host signed,
 * sealed and qualified it (--to qualify); only the publisher holds the R2 write credential. Requires this host's receipt for HEAD with
 * package and qualify passed on the sealed bytes as they are now. Packs the sealed directory and the receipt into the draft GitHub
 * release `rightkit-staging-<commit>` (replacing that platform's asset), then, with --tier, dispatches RightKit's publish-app workflow
 * (the one trusted publisher for every app) for HEAD.
 */
const PUBLISHER_REPO = "bogusyogi/rightkit";
function stageForPublisher(o, args, root, git) {
  const option = (name) => { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; };
  const tag = "rightkit-gate:";
  const platform = option("--platform");
  const tier = option("--tier");
  if (!["mac", "win"].includes(platform)) { o.err(`${tag} stage needs --platform mac|win`); return 2; }
  if (tier !== undefined && !["patch", "update"].includes(tier)) { o.err(`${tag} stage --tier must be patch or update`); return 2; }
  const commit = text(git("rev-parse", "HEAD")).trim();
  const common = text(git("rev-parse", "--git-common-dir")).trim();
  if (!/^[0-9a-f]{40}$/.test(commit) || !common) { o.err(`${tag} cannot read HEAD and the git directory`); return 2; }
  const commonDir = path.resolve(root, common);
  const inventory = committedConfigDirs({ git, commit });
  if (inventory.refusal) { o.err(`${tag} cannot read the app configuration of ${commit.slice(0, 12)}: ${inventory.refusal}`); return 2; }
  const { appRoot, appDir } = appRootOf({ root, cwd: o.cwd, configDirs: inventory.dirs });
  const receipt = (o.readReceipt ?? readReceipt)({ commonDir, commit, app: appDir });
  const pack = receipt?.stages?.package; const qual = receipt?.stages?.qualify;
  // right-release's outputs live at the checkout's git root (see runStages).
  const releaseRoot = root;
  const sealed = pack?.ok ? (o.sealedFor ?? sealedFor)({ root: releaseRoot, commit, platform }) : null;
  if (!pack?.ok || !qual?.ok || pack.platform !== platform || qual.manifestSha256 !== pack.manifestSha256 || !sealed || sealed.id !== pack.sealed || sealed.digest !== pack.manifestSha256) {
    o.err(`${tag} BLOCKED: stage needs this host's receipt for ${commit.slice(0, 12)} with package and qualify passed on ${platform} and the sealed set unchanged; run rightkit-gate --to qualify --platform ${platform} first`);
    return 1;
  }
  // One draft per commit and app (receiptKey): two apps of one repository staged at one commit never clobber each other's assets.
  const releaseTag = `rightkit-staging-${receiptKey(commit, appDir)}`;
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "rightkit-stage-"));
  try {
    const receiptFile = path.join(work, `receipt-${platform}.json`);
    fs.writeFileSync(receiptFile, `${JSON.stringify(receipt, null, 2)}\n`);
    const archive = path.join(work, `sealed-${platform}.tar.gz`);
    const tar = o.spawn("tar", ["-czf", archive, "-C", releaseRoot, path.join(".right-release", "sealed", sealed.id, platformDirOf(platform))]);
    if (failed(tar)) { o.err(`${tag} could not pack ${sealed.id}/${platform}`); return 1; }
    const gh = (...a) => o.spawn("gh", a, { cwd: root });
    if (failed(gh("release", "view", releaseTag))) {
      const made = gh("release", "create", releaseTag, "--draft", "--target", commit, "--title", `staging ${commit.slice(0, 12)}`, "--notes", "Sealed and qualified sets for the trusted publisher; not a release.");
      if (failed(made)) { o.err(`${tag} could not create the draft ${releaseTag}: ${text(made).trim()}`); return 1; }
    }
    const up = gh("release", "upload", releaseTag, archive, receiptFile, "--clobber");
    if (failed(up)) { o.err(`${tag} could not upload to ${releaseTag}: ${text(up).trim()}`); return 1; }
    o.out(`${tag} staged ${sealed.id}/${platform} (manifest ${sealed.digest.slice(0, 12)}) in draft ${releaseTag}`);
    if (tier) {
      // The trusted publisher is RightKit's publish-app workflow: app repositories are private and run no Actions.
      const slug = text(gh("repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner")).trim();
      const run = gh("workflow", "run", "publish-app.yml", "-R", PUBLISHER_REPO, "-f", `app=${slug}`, "-f", `app_dir=${appDir}`, "-f", `source_revision=${commit}`, "-f", `tier=${tier}`, "-f", `platforms=${platform}`);
      if (!slug || failed(run)) { o.err(`${tag} staged, but could not dispatch ${PUBLISHER_REPO} publish-app.yml: ${text(run).trim()}`); return 1; }
      o.out(`${tag} dispatched ${PUBLISHER_REPO} publish-app.yml for ${slug}@${commit.slice(0, 12)} (${platform}, ${tier}); the publisher re-runs class A, verifies the receipt and the bytes, then uploads`);
    }
    return 0;
  } finally { fs.rmSync(work, { recursive: true, force: true }); }
}

// ---- the stage-output ledger (stage-ledger.mjs; decision 2 Q1, rows 7.05/7.05b/7.07/7.08) ----------------------------------------

/** What the snapshot holds beyond the commit right now: { entries, problems } by this gate's own walk. */
async function walkDoorSnapshot(o, ds, root) {
  const snap = await import("../snapshot.mjs");
  const tracked = new Set((o.treeEntries ?? snap.treeEntries)(root, ds.commit).map((e) => e.path));
  const dependencyRoots = (o.dependencyDirs ?? snap.dependencyDirs)((o.treeEntries ?? snap.treeEntries)(root, ds.commit), o.platform);
  const { walkSnapshot } = await import("../stage-ledger.mjs");
  return (o.walkSnapshot ?? walkSnapshot)({ dir: ds.path, tracked, dependencyRoots, env: o.env, platform: o.platform });
}

/** `verifySnapshot` with the ledger (item 5 row 5.09d: `ledger` admits exactly these entries, `admitNew` only at a stage's end). */
async function verifyDoorSnapshot(o, ds, root, { ledger, admitNew = false }) {
  const verify = o.verifySnapshot ?? (await import("../snapshot.mjs")).verifySnapshot;
  return verify({ checkout: root, rev: ds.commit, root: ds.root, dir: ds.path, ledger, admitNew });
}

/**
 * The start of a stage: the snapshot is exactly tree + dependencies + the ledger of the stages before it, no entry new, changed or
 * missing, and the host's ownership is still held (7.08, decision 2 Q1). An ignored file planted before the stage is a new entry here.
 */
export async function openSnapshotStage({ o, ds, root, stage, tag = "rightkit-gate:" }) {
  if (!ds.own.held()) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] the build ownership ${ds.own.handle} is no longer held`); return false; }
  const { diffEntries, sameEntries } = await import("../stage-ledger.mjs");
  let walked;
  try { walked = await walkDoorSnapshot(o, ds, root); } catch (e) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] ${e.message}`); return false; }
  const prior = ds.ledger?.entries ?? [];
  if (walked.problems.length) { for (const p of walked.problems.slice(0, 10)) o.err(`${tag} RK_CHECK_ERROR: [${stage}] before the stage: ${p}`); return false; }
  if (!sameEntries(prior, walked.entries)) {
    const d = diffEntries(prior, walked.entries);
    o.err(`${tag} RK_CHECK_ERROR: [${stage}] before the stage the snapshot differs from the ledger of the stages before it (new: ${d.added.join(", ") || "none"}; changed: ${d.changed.join(", ") || "none"}; missing: ${d.removed.join(", ") || "none"})`);
    return false;
  }
  try { await verifyDoorSnapshot(o, ds, root, { ledger: prior }); } catch (e) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] before the stage: ${e.message}`); return false; }
  return true;
}

/**
 * The end of a stage: what its children wrote may remain only as ledger entries. A staged entry is immutable for the rest of the run
 * (the bytes the compile read are the bytes sealed); a generated entry may be rewritten or removed and is re-recorded here. Returns
 * `{ ok, entries }` and replaces the door's ledger with the entries just recorded.
 */
export async function closeSnapshotStage({ o, ds, root, stage, step = "stage", tag = "rightkit-gate:" }) {
  if (!ds.own.held()) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] the build ownership ${ds.own.handle} is no longer held`); return { ok: false, entries: ds.ledger?.entries ?? [] }; }
  const { entryKey } = await import("../stage-ledger.mjs");
  let walked;
  try { walked = await walkDoorSnapshot(o, ds, root); } catch (e) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] ${e.message}`); return { ok: false, entries: ds.ledger?.entries ?? [] }; }
  const prior = new Map((ds.ledger?.entries ?? []).map((e) => [e.path, e]));
  const now = new Map(walked.entries.map((e) => [e.path, e]));
  const problems = [...walked.problems];
  // The package stage builds for the package triple (universal-apple-darwin on a Mac recipe with `stageSidecars`), so right-release's own
  // stage-sidecar may stage again there. A staged entry it changed or removed is admitted only through the receipt it wrote: a valid receipt
  // of the same config, every file it names present with that digest; the files it names become this stage's staged entries.
  const restaged = stage === "package" ? await restagedSidecars({ o, ds, prior, now }) : { paths: new Set(), receipts: new Set(), producers: new Set(), problems: [] };
  problems.push(...restaged.problems);
  for (const [p, e] of prior) if (e.kind === "staged" && !restaged.paths.has(p) && !restaged.receipts.has(p) && !(restaged.producers.has(e.producer?.step) && !now.has(p)) && (!now.has(p) || entryKey(now.get(p)) !== entryKey(e))) problems.push(`${p} is a staged sidecar output and ${now.has(p) ? "changed" : "was removed"} after it was staged (a staged entry is immutable for the door run)`);
  // Evidence (Pulse Windows dev, 2026-11-06): for each staged entry that changed, its sha256 then and now; for a receipt, the fields that differ.
  for (const [p, e] of prior) {
    if (e.kind !== "staged" || !now.has(p) || entryKey(now.get(p)) === entryKey(e) || restaged.paths.has(p) || restaged.receipts.has(p)) continue;
    o.err(`${tag} staged-change evidence: ${p}: sha256 ${String(e.sha256).slice(0, 16)} at staging (${e.size} bytes) -> ${String(now.get(p).sha256).slice(0, 16)} now (${now.get(p).size} bytes)`);
    const before = ds.stagedReceipts?.get(p);
    if (before) {
      try {
        const a = JSON.parse(before); const b = JSON.parse(fs.readFileSync(path.join(ds.path, p), "utf8"));
        for (const k of new Set([...Object.keys(a), ...Object.keys(b)])) if (k !== "files" && JSON.stringify(a[k]) !== JSON.stringify(b[k])) o.err(`${tag} staged-change evidence: ${p}: ${k}: ${JSON.stringify(a[k]).slice(0, 200)} -> ${JSON.stringify(b[k]).slice(0, 200)}`);
        const byName = (list) => new Map((Array.isArray(list) ? list : []).map((f) => [f.name ?? f.dest, f]));
        const fa = byName(a.files); const fb = byName(b.files);
        for (const n of new Set([...fa.keys(), ...fb.keys()])) {
          const x = fa.get(n) ?? {}; const y = fb.get(n) ?? {};
          const diff = [...new Set([...Object.keys(x), ...Object.keys(y)])].filter((k) => !["changed", "cached"].includes(k) && JSON.stringify(x[k]) !== JSON.stringify(y[k]));
          if (diff.length) o.err(`${tag} staged-change evidence: ${p}: files[${n}]: ${diff.map((k) => `${k} ${JSON.stringify(x[k]).slice(0, 80)} -> ${JSON.stringify(y[k]).slice(0, 80)}`).join("; ")}`);
        }
      } catch (error) { o.err(`${tag} staged-change evidence: ${p}: the receipts could not be compared (${error.message})`); }
    }
  }
  if (problems.length) { for (const p of problems.slice(0, 10)) o.err(`${tag} RK_CHECK_ERROR: [${stage}] ${p}`); return { ok: false, entries: ds.ledger?.entries ?? [] }; }
  // The compile's own verify (snapshot-check.mjs, 5.09d) returned the entries it admitted: they must be the entries this walk found.
  if (Array.isArray(ds.compileAdmitted) && stage === "compile") {
    const { admittedMismatch } = await import("../stage-ledger.mjs");
    const bad = admittedMismatch(ds.compileAdmitted, walked.entries);
    if (bad) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] the compile's snapshot verify admitted other ledger entries than the door's own walk recorded: ${bad}`); return { ok: false, entries: ds.ledger?.entries ?? [] }; }
  }
  ds.compileAdmitted = null;
  const entries = walked.entries.map((e) => {
    const was = prior.get(e.path);
    if (restaged.paths.has(e.path) || restaged.receipts.has(e.path)) return { ...e, kind: "staged", producer: { stage, step: "stage-sidecar" } };
    if (was?.kind === "staged") return { ...e, kind: "staged", producer: was.producer };
    return { ...e, kind: "generated", producer: was && entryKey(was) === entryKey(e) ? was.producer : { stage, step } };
  });
  try {
    const verified = await verifyDoorSnapshot(o, ds, root, { ledger: entries, admitNew: true });
    // The verify side (item 5 row 5.09d) returns the entries it admitted; if it does, they must be the ones this walk recorded.
    if (Array.isArray(verified?.ledger)) {
      const { admittedMismatch } = await import("../stage-ledger.mjs");
      const bad = admittedMismatch(verified.ledger, entries);
      if (bad) throw new Error(`the snapshot verify admitted other ledger entries than the door's own walk recorded: ${bad}`);
    }
  } catch (e) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] ${e.message}`); return { ok: false, entries: ds.ledger?.entries ?? [] }; }
  ds.ledger = { entries };
  return { ok: true, entries };
}

/**
 * The sidecar re-staging a package stage did, proven by its receipts: for each staged receipt (`<config dir>/.../stage-sidecar.json`) that
 * changed, the new receipt must be a valid stage-sidecar receipt and every file it names must be on disk with its recorded digest. Returns the
 * snapshot paths it names (`paths`), the receipts themselves (`receipts`), the producer steps whose earlier staged files it replaces
 * (`producers`, whose removed files are admitted) and the problems found.
 */
async function restagedSidecars({ o, ds, prior, now }) {
  const { entryKey } = await import("../stage-ledger.mjs");
  const { validateStageSidecarReceipt } = await import("../stage-sidecar.mjs");
  const out = { paths: new Set(), receipts: new Set(), producers: new Set(), problems: [] };
  for (const [p, e] of prior) {
    if (e.kind !== "staged" || !/(^|\/)stage-sidecar\.json$/.test(p) || !now.has(p) || entryKey(now.get(p)) === entryKey(e)) continue;
    const config = e.producer?.step;
    const configDir = typeof config === "string" ? path.posix.dirname(config.replaceAll("\\", "/")) : ".";
    let receipt;
    try { receipt = validateStageSidecarReceipt(JSON.parse(String(o.readFile(path.join(ds.path, ...p.split("/")))))); }
    catch (error) { out.problems.push(`${p} was rewritten in the package stage but is not a valid stage-sidecar receipt (${error.message})`); continue; }
    const named = [];
    for (const file of receipt.files) {
      const rel = (configDir === "." ? file.path : `${configDir}/${file.path}`).replaceAll("\\", "/");
      const entry = now.get(rel);
      if (!entry) { out.problems.push(`${p} names ${rel}, which is not in the snapshot`); continue; }
      if (entry.sha256 !== undefined && entry.sha256 !== file.sha256) { out.problems.push(`${rel} does not have the digest ${p} records for it`); continue; }
      named.push(rel);
    }
    if (named.length !== receipt.files.length) continue;
    out.receipts.add(p);
    for (const rel of named) out.paths.add(rel);
    if (typeof config === "string") out.producers.add(config);
  }
  return out;
}

/**
 * The --target the package stage stages sidecars for: `package` on a Mac host, which right-release stage-sidecar resolves itself from the
 * snapshot's config (the Mac package recipe's own triple, e.g. universal-apple-darwin), so this gate never imports app code; null elsewhere (host).
 */
export async function packageSidecarTarget({ o, ds, configs = [] }) {
  if (o.platform !== "darwin" || !ds) return null;
  // Read as text, never imported (as committedSidecarConfigs does): only a config whose Mac package recipe stages sidecars gets a package-triple staging.
  return configs.some((config) => { try { return /\bstageSidecars\b/.test(o.readFile(path.join(ds.path, config))); } catch { return false; } }) ? "package" : null;
}

/** The release configs of `commit` (under `appDir`) that declare `sidecars`, read from `base` (the snapshot on the snapshot route): the commit's config decides, never the checkout's. */
export function committedSidecarConfigs({ o, git, commit, appDir = ".", base }) {
  const listedConfigs = git("ls-tree", "-r", "--name-only", "-z", commit);
  return (failed(listedConfigs) ? [] : text(listedConfigs).split("\0").filter((f) => /(^|\/)right-release\.config\.(json|mjs)$/.test(f)).filter((f) => appDir === "." || f.startsWith(`${appDir}/`)))
    .filter((f) => { try { return /\bsidecars\b/.test(o.readFile(path.join(base, f))); } catch { return false; } });
}

/**
 * Sidecars are staged ONCE per door run, after the sync and before the first stage that builds the Tauri crate. The step is
 * right-release's own `stage-sidecar --json` run inside the snapshot (the split); this gate then walks the snapshot itself and the
 * step's additions must equal the files it reported plus its receipt (anything else is an unattributed write, RK_CHECK_ERROR). The
 * staged files become immutable `staged` ledger entries. Returns true when nothing is owed or staging passed.
 */
export async function stageSidecarsOnce({ o, ds, root, git = null, configs, appDir, cli, commonDir, commit, tree, stage, executor = null, tag = "rightkit-gate:", target = null }) {
  // The `dev` stage stages in dev mode (RIGHTKIT_BUILD_MODE=dev, the mode `right-release dev` gives every step it runs): the app's own `stage-sidecar` step in its dev build then finds the
  // same mode, features and bytes already staged and changes nothing. Staged in release mode first, its dev re-stage rewrote pulse-engine, membrane and the receipt, which the ledger refuses as a
  // changed staged entry (Pulse Mac dev install, 2026-11-06; docs/GATE-CONTRACT.md [GC-A28]).
  // Once per door run PER TARGET TRIPLE: `target` null is the host (the compile stage: stage-sidecar's own default, no `--target` passed), anything else is the triple the
  // package recipe stages for (universal-apple-darwin on a Mac recipe with `stageSidecars`), passed to stage-sidecar as `--target`. stage-sidecar is a no-op for a target whose
  // receipt and files are already current, so the recipe's own prepare step afterwards changes nothing.
  const staged = ds.sidecarsStaged instanceof Set ? ds.sidecarsStaged : (ds.sidecarsStaged = new Set());
  const key = target ?? "host";
  if (staged.has(key)) return true;
  staged.add(key);
  if (!configs.length) return true;
  const { attributionProblems, reportedPaths } = await import("../stage-ledger.mjs");
  const source = sourceSplitOf({ root, ds, appDir });
  const { TAURI_CONFIG: inherited, ...cleanEnv } = o.env; void inherited; // never set or pass one (refused at the door's start)
  for (const config of configs) {
    const before = ds.ledger?.entries ?? [];
    // The stage-sidecar child's Cargo calls (its builds and metadata queries) are carried on a broker host (carried-cargo.mjs).
    const carrier = await carrierForChildren({ o, ds, root, git, executor, stage, tag, producer: true });
    if (carrier.refusal) { o.err(`${tag} BLOCKED: ${carrier.refusal}; nothing was staged`); return false; }
    const run = startRun({ commonDir, commit, tree, stage, source });
    let r;
    try {
      o.out(`${tag} [${stage}] right-release stage-sidecar --config ${path.join(ds.path, config)}${target ? ` --target ${target}` : ""} (from the snapshot ${ds.path})`);
      r = (o.spawnCapture ?? ((cmd, a, opts) => spawnInherit(cmd, a, { ...opts, encoding: "utf8", maxBuffer: 1 << 26 })))(process.execPath, [cli, "stage-sidecar", "--config", path.join(ds.path, config), ...(target ? ["--target", target] : []), "--json"], { cwd: snapshotAppRootOf(ds.path, appDir), env: carriedEnv(carrier, { ...cleanEnv, ...run.env, ...(stage === "dev" ? { RIGHTKIT_BUILD_MODE: "dev" } : {}) }) });
    } finally { endRun(run); }
    const carriedOk = await carrier.finish({ stage });
    if (r?.stderr) o.err(String(r.stderr).trimEnd());
    if (!r || r.status !== 0) { o.err(`${tag} [${stage}] stage-sidecar for ${config} failed${r?.error ? ` (${r.error.message})` : ""}`); return false; }
    if (!carriedOk) return false;
    let result;
    try { result = JSON.parse(String(r.stdout ?? "").trim()); } catch { o.err(`${tag} RK_CHECK_ERROR: [${stage}] stage-sidecar --json for ${config} did not print its result; its writes cannot be attributed`); return false; }
    let walked;
    try { walked = await walkDoorSnapshot(o, ds, root); } catch (e) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] ${e.message}`); return false; }
    // stage-sidecar reports its files relative to ITS app root, the directory of the config it was given (stage-sidecar-command.mjs
    // appRoot = dirname(config)), not the gate's appDir: a gate run at the repository root with the config in tauri-app-next/ would
    // otherwise find every staged file "not reported".
    const configDir = path.posix.dirname(String(config).replaceAll("\\", "/"));
    const underApp = (rel) => (configDir === "." ? rel : `${configDir}/${rel}`).replaceAll("\\", "/");
    const receiptAbs = typeof result.receiptPath === "string" ? path.relative(ds.path, result.receiptPath).split(path.sep).join("/") : null;
    const problems = [...walked.problems, ...attributionProblems({ before, after: walked.entries, reported: [...reportedPaths(result)].map(underApp), receipt: receiptAbs, removable: before.filter((e) => e.kind === "staged" && e.producer?.stage === "sidecar" && e.producer?.step === config).map((e) => e.path) /* only THIS config's earlier staged files (Oracle: another config's build must never delete them unreported) */ })];
    if (problems.length) { for (const p of problems.slice(0, 10)) o.err(`${tag} RK_CHECK_ERROR: [${stage}] stage-sidecar ${config}: ${p}`); return false; }
    const priorPaths = new Set(before.map((e) => e.path));
    const priorByPath = new Map(before.map((e) => [e.path, e]));
    ds.ledger = { entries: walked.entries.map((e) => (priorPaths.has(e.path) && priorByPath.get(e.path).sha256 === e.sha256 ? priorByPath.get(e.path) : { ...e, kind: "staged", producer: { stage: "sidecar", step: config } })) };
    // Evidence for a later "changed after it was staged": the receipt exactly as this staging wrote it.
    if (receiptAbs) { try { (ds.stagedReceipts ??= new Map()).set(receiptAbs, fs.readFileSync(path.join(ds.path, receiptAbs), "utf8")); } catch { /* the close names the change without the field diff */ } }
  }
  return true;
}

/**
 * rightkit#1332, #1333: what the compile stage left to qualification must not be lost. Each owed item (an end-to-end crate whose tests need
 * the built app; a Swift package declared `qualifyOnly`) needs the run route the pipeline has for it: the app's release config declares
 * `qa.journeys`, so `right-release qa --stage installed` runs `rightkit-qa journeys run` against the sealed app and validates its receipt
 * (release-qa-command.mjs). Without it each owed item is named and qualification stops. Returns the problems (empty when nothing is owed or covered).
 */
export function qualifyOwedProblem({ o, git, commit, appDir = ".", base, owed }) {
  if (!Array.isArray(owed) || !owed.length) return [];
  const listed = git("ls-tree", "-r", "--name-only", "-z", commit);
  const configs = (failed(listed) ? [] : text(listed).split("\0").filter((f) => /(^|\/)right-release\.config\.(json|mjs)$/.test(f)).filter((f) => appDir === "." || f.startsWith(`${appDir}/`)));
  const declared = configs.some((f) => { try { return /\bqa\b[\s\S]*(^|[\s,{])["']?journeys["']?\s*:/m.test(o.readFile(path.join(base, f))); } catch { return false; } });
  if (declared) return [];
  return owed.map((x) => `${x.id} (${x.kind}${x.dir ? ` ${x.dir}` : ""}) is owed to qualification (${x.reason}) but no release config under ${appDir} declares qa.journeys, so nothing would run it; declare qa.journeys with a journeys.toml that covers it`);
}

/** The release configs of `commit` (under `appDir`) that declare a `web` block (`web.build` steps), read from `base`: the commit's config decides, never the checkout's. */
export function committedWebBuildConfigs({ o, git, commit, appDir = ".", base }) {
  const listedConfigs = git("ls-tree", "-r", "--name-only", "-z", commit);
  return (failed(listedConfigs) ? [] : text(listedConfigs).split("\0").filter((f) => /(^|\/)right-release\.config\.(json|mjs)$/.test(f)).filter((f) => appDir === "." || f.startsWith(`${appDir}/`)))
    .filter((f) => { try { return /(^|[\s,{])["']?web["']?\s*:\s*\{/.test(o.readFile(path.join(base, f))); } catch { return false; } });
}

/**
 * rightkit#1331: the declared compile inputs (`.rightkit/compile-inputs.json`) that are an app's web build output (a Tauri `frontendDist`)
 * exist in the snapshot before the compile reads them. The broker's capture copies a declared path in from the checkout; the snapshot route
 * has no copy ("digested in place", GATE-CONTRACT GC-I14), so the door produces them the one way the contract allows: the app's own
 * `right-release web-build` (its release config's `web.build` steps) runs INSIDE the snapshot, once per door run, before the compile.
 * What it wrote may remain only under a declared input (declaredOutputProblems), ignored by the commit's own rules and recorded by this
 * gate's own walk as `generated` ledger entries (never `staged`: the package stage's own web build rewrites them). The compile's declared
 * input digest (compile-inputs.mjs, the broker's algorithm) then covers those very bytes and declaredInputProblems cross-checks it.
 * Nothing runs when every declared input is already present or the app declares no `web.build`; an input still absent afterwards is left
 * to the compile, which refuses a missing input and names it. Returns true when nothing is owed or the build passed.
 */
/**
 * The web build's cache key (the `dist` cache, GC-T14 result reuse): everything vite can read from the commit, without the Rust crates,
 * plus the Node version and the vite-visible environment. Committed blobs are listed from the commit itself (`ls-tree -r`); a directory
 * whose Cargo.toml declares a [package] is a Rust crate and is left out (a workspace-only Cargo.toml, like an app root's, is not), so a
 * Rust-only commit reuses the bundle. Returns null when the tree cannot be read (no cache then, the build runs).
 */
export function webBuildCacheKey({ git, commit, config, env = process.env, nodeVersion = process.version }) {
  const listed = git("ls-tree", "-r", "-z", commit);
  if (failed(listed)) return null;
  const entries = text(listed).split("\0").filter(Boolean).map((line) => { const tab = line.indexOf("\t"); const oid = line.slice(0, tab).split(" ")[2]; return { path: line.slice(tab + 1), oid }; });
  const crateDirs = entries.filter((e) => /(^|\/)Cargo\.toml$/.test(e.path) && e.path.includes("/")).filter((e) => {
    const blob = git("cat-file", "-p", e.oid);
    return !failed(blob) && /^\s*\[package\]/m.test(text(blob));
  }).map((e) => `${e.path.slice(0, e.path.lastIndexOf("/"))}/`);
  const kept = entries.filter((e) => !crateDirs.some((dir) => e.path.startsWith(dir))).map((e) => [e.path, e.oid]);
  const viteEnv = Object.entries(env ?? {}).filter(([k]) => /^(VITE_|NODE_ENV$)/.test(k)).sort(([a], [b]) => (a < b ? -1 : 1));
  return createHash("sha256").update(JSON.stringify({ schema: "rightkit.web-build-cache/1", config, node: nodeVersion, env: viteEnv, files: kept })).digest("hex");
}

/** `<managed root>/web-cache/<repo key>`, beside the snapshot (`<managed root>/snapshots/<repo key>`). */
export const webBuildCacheRoot = (snapshotPath) => path.join(path.dirname(path.dirname(snapshotPath)), "web-cache", path.basename(snapshotPath));

/** Copies every declared output from `from` to `to`, keeping modification times (a restored bundle looks unchanged to Cargo). */
function copyDeclared(declared, from, to) {
  for (const rel of declared) {
    const src = path.join(from, ...rel.split("/"));
    if (!fs.existsSync(src)) return false;
    const dest = path.join(to, ...rel.split("/"));
    fs.rmSync(dest, { recursive: true, force: true });
    fs.mkdirSync(path.dirname(dest), { recursive: true });
    fs.cpSync(src, dest, { recursive: true, preserveTimestamps: true, verbatimSymlinks: true });
  }
  return true;
}

export async function stageWebBuildOnce({ o, ds, root, git = null, appDir, cli, commonDir, commit, tree, stage, executor = null, tag = "rightkit-gate:" }) {
  if (ds.webBuilt) return true;
  ds.webBuilt = true;
  const { readDeclaredInputs, SnapshotRequestError } = await import("../snapshot-request.mjs");
  let declared;
  try { declared = readDeclaredInputs(ds.path).map((x) => x.split(/[\\/]/).join("/")); }
  catch (e) { if (e instanceof SnapshotRequestError) return true; throw e; } // a malformed declaration refuses in the compile, which names it
  const absent = () => declared.filter((rel) => !(o.exists ?? fs.existsSync)(path.join(ds.path, ...rel.split("/"))));
  // Children of this run (the app's own `right-release web-build` step) reuse these outputs instead of rebuilding them (web-build.mjs).
  const markBuilt = () => { if (declared.length && o.env) o.env.RK_GATE_WEB_BUILT = snapshotAppRootOf(ds.path, appDir); };
  if (!absent().length) { markBuilt(); return true; }
  const configs = git ? committedWebBuildConfigs({ o, git, commit, appDir, base: ds.path }) : [];
  if (!configs.length) { o.out(`${tag} [${stage}] declared compile input(s) ${absent().join(", ")} are absent from the snapshot and no release config under ${appDir} declares web.build; the compile will report them missing`); return true; }
  const { declaredOutputProblems } = await import("../stage-ledger.mjs");
  const source = sourceSplitOf({ root, ds, appDir });
  const { TAURI_CONFIG: inherited, ...cleanEnv } = o.env; void inherited;
  for (const config of configs) {
    if (!absent().length) break;
    const before = ds.ledger?.entries ?? [];
    // Adrian, 2026-11-02: the web build (vite, any bundling) runs outside the broker, directly in the snapshot; no carrier, no carried call.
    const carrier = { carried: false, env: {}, finish: async () => true };
    // The dist cache: same committed web inputs, Node and vite env as an earlier build -> restore its bundle with its original modification
    // times and skip vite, so the Tauri crate that embeds it stays fresh (Adrian, 2026-11-03: vite output was rebuilt on every gate run).
    const key = git ? (o.webBuildCacheKey ?? webBuildCacheKey)({ git, commit, config, env: o.env }) : null;
    const cacheDir = key ? path.join(webBuildCacheRoot(ds.path), key) : null;
    const restored = Boolean(cacheDir && fs.existsSync(path.join(cacheDir, ".complete")) && copyDeclared(declared, cacheDir, ds.path));
    if (restored) o.out(`${tag} [${stage}] web-build ${config}: restored from the dist cache (${key.slice(0, 12)}); vite not run`);
    const run = restored ? null : startRun({ commonDir, commit, tree, stage, source });
    let r = restored ? { status: 0 } : null;
    if (!restored) try {
      o.out(`${tag} [${stage}] right-release web-build --config ${path.join(ds.path, config)} (from the snapshot ${ds.path}; declared input(s) ${absent().join(", ")} are absent)`);
      r = (o.spawnInherit ?? inheritLaunch)(process.execPath, [cli, "web-build", "--config", path.join(ds.path, config)], { cwd: snapshotAppRootOf(ds.path, appDir), env: carriedEnv(carrier, { ...cleanEnv, ...run.env }) });
    } finally { endRun(run); }
    const carriedOk = await carrier.finish({ stage });
    if (!r || r.status !== 0) { o.err(`${tag} [${stage}] web-build for ${config} failed${r?.error ? ` (${r.error.message})` : ""}`); return false; }
    if (!carriedOk) return false;
    let walked;
    try { walked = await walkDoorSnapshot(o, ds, root); } catch (e) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] ${e.message}`); return false; }
    const problems = [...walked.problems, ...declaredOutputProblems({ before, after: walked.entries, declared })];
    if (problems.length) { for (const p of problems.slice(0, 10)) o.err(`${tag} RK_CHECK_ERROR: [${stage}] web-build ${config}: ${p}`); return false; }
    const priorByPath = new Map(before.map((e) => [e.path, e]));
    ds.ledger = { entries: walked.entries.map((e) => (priorByPath.get(e.path)?.sha256 === e.sha256 ? priorByPath.get(e.path) : { ...e, kind: "generated", producer: { stage: "web-build", step: config } })) };
    // A fresh, verified bundle is stored for the next run (written beside, then renamed: never a half cache). Older keys are pruned to 3.
    if (!restored && cacheDir) {
      try {
        const staging = `${cacheDir}.tmp-${process.pid}`;
        fs.rmSync(staging, { recursive: true, force: true });
        if (copyDeclared(declared, ds.path, staging)) {
          fs.writeFileSync(path.join(staging, ".complete"), `${new Date().toISOString()}\n`);
          fs.rmSync(cacheDir, { recursive: true, force: true });
          fs.renameSync(staging, cacheDir);
          const root = webBuildCacheRoot(ds.path);
          const keys = fs.readdirSync(root).filter((n) => /^[0-9a-f]{64}$/.test(n)).map((n) => ({ n, t: fs.statSync(path.join(root, n)).mtimeMs })).sort((a, b) => b.t - a.t);
          for (const old of keys.slice(3)) fs.rmSync(path.join(root, old.n), { recursive: true, force: true });
        } else fs.rmSync(staging, { recursive: true, force: true });
      } catch (e) { o.out(`${tag} [${stage}] web-build ${config}: the dist cache was not written (${e.message}); the next run builds again`); }
    }
  }
  if (!absent().length) markBuilt();
  return true;
}

/**
 * The Windows package-identity (sparse MSIX) an app bundles, produced INSIDE the snapshot before the compile, like stageWebBuildOnce: the
 * app's committed release config declares `windows.identity` (windows-identity-plan.mjs) and windows-identity.mjs (RightKit's msix tooling, the
 * provisioned signer; never app code) builds and signs it. Windows hosts only: elsewhere nothing runs and a declared input that stays absent is
 * left to the compile, which names it (platform scoping of the declared input itself is a separate decision). The child reports what it wrote
 * (`--json`); this gate's own walk must then find additions only under the producer's `out`, which must lie under a declared compile input
 * (identityOutputProblems). They become `generated` ledger entries, and the compile's declared-input digest covers those very bytes.
 * Nothing runs when every declared input is present or no committed config declares windows.identity (the package itself already being
 * present is the child's skip). Returns true when nothing is owed or the build passed.
 */
export async function stageWindowsIdentityOnce({ o, ds, root, git = null, appDir, cli, commonDir, commit, tree, stage, executor = null, tag = "rightkit-gate:" }) {
  if (ds.identityBuilt) return true;
  ds.identityBuilt = true;
  const { readDeclaredInputs, SnapshotRequestError } = await import("../snapshot-request.mjs");
  let declared;
  try { declared = readDeclaredInputs(ds.path).map((x) => x.split(/[\\/]/).join("/")); }
  catch (e) { if (e instanceof SnapshotRequestError) return true; throw e; }
  const absent = () => declared.filter((rel) => !(o.exists ?? fs.existsSync)(path.join(ds.path, ...rel.split("/"))));
  if (!absent().length || !git) return true;
  const { committedWindowsIdentityConfigs } = await import("../windows-identity-plan.mjs");
  const listed = git("ls-tree", "-r", "--name-only", "-z", commit);
  const all = failed(listed) ? [] : text(listed).split("\0").filter((f) => /(^|\/)right-release\.config\.(json|mjs)$/.test(f)).filter((f) => appDir === "." || f.startsWith(`${appDir}/`));
  const configs = committedWindowsIdentityConfigs({ readFile: o.readFile, listConfigs: all, base: ds.path });
  if (!configs.length) return true;
  if ((o.platform ?? process.platform) !== "win32") { o.out(`${tag} [${stage}] ${configs.join(", ")} declares windows.identity; not a Windows host, so the identity package is not built here`); return true; }
  const { identityOutputProblems } = await import("../windows-identity-plan.mjs");
  const source = sourceSplitOf({ root, ds, appDir });
  const { TAURI_CONFIG: inherited, ...cleanEnv } = o.env; void inherited;
  const producer = path.join(path.dirname(cli), "..", "windows-identity.mjs");
  for (const config of configs) {
    const before = ds.ledger?.entries ?? [];
    const carrier = await carrierForChildren({ o, ds, root, git, executor, stage, tag, producer: true });
    if (carrier.refusal) { o.err(`${tag} BLOCKED: ${carrier.refusal}; the Windows identity package did not build`); return false; }
    const run = startRun({ commonDir, commit, tree, stage, source });
    let r;
    try {
      o.out(`${tag} [${stage}] windows-identity --config ${path.join(ds.path, config)} (from the snapshot ${ds.path})`);
      r = (o.spawnCapture ?? ((cmd, a, opts) => spawnInherit(cmd, a, { ...opts, encoding: "utf8", maxBuffer: 1 << 26 })))(process.execPath, [producer, "--config", path.join(ds.path, config), "--json"], { cwd: snapshotAppRootOf(ds.path, appDir), env: carriedEnv(carrier, { ...cleanEnv, ...run.env }) });
    } finally { endRun(run); }
    const carriedOk = await carrier.finish({ stage });
    if (r?.stderr) o.err(String(r.stderr).trimEnd());
    if (!r || r.status !== 0) { o.err(`${tag} [${stage}] windows-identity for ${config} failed${r?.error ? ` (${r.error.message})` : ""}`); return false; }
    if (!carriedOk) return false;
    let result;
    try { result = JSON.parse(String(r.stdout ?? "").trim()); } catch { o.err(`${tag} RK_CHECK_ERROR: [${stage}] windows-identity --json for ${config} did not print its result; its writes cannot be attributed`); return false; }
    let walked;
    try { walked = await walkDoorSnapshot(o, ds, root); } catch (e) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] ${e.message}`); return false; }
    const configDir = path.posix.dirname(String(config).replaceAll("\\", "/"));
    const underApp = (rel) => (configDir === "." ? rel : `${configDir}/${rel}`);
    let problems = walked.problems;
    if (result.skipped) problems = [...problems, ...diffProblems(before, walked.entries)];
    else problems = [...problems, ...identityOutputProblems({ before, after: walked.entries, out: underApp(result.out), declared, packageFile: result.package })];
    if (problems.length) { for (const p of problems.slice(0, 10)) o.err(`${tag} RK_CHECK_ERROR: [${stage}] windows-identity ${config}: ${p}`); return false; }
    const priorByPath = new Map(before.map((e) => [e.path, e]));
    ds.ledger = { entries: walked.entries.map((e) => (priorByPath.get(e.path)?.sha256 === e.sha256 ? priorByPath.get(e.path) : { ...e, kind: "generated", producer: { stage: "windows-identity", step: config } })) };
  }
  return true;
}
const diffProblems = (before, after) => { const b = new Map(before.map((e) => [e.path, e.sha256])); return after.filter((e) => b.get(e.path) !== e.sha256).map((e) => `${e.path} was written although the identity build reported a skip`); };

/** Hosted CI scans its committed runner checkout. Raw-tree verification defeats filters or a stale index hiding edits. */
async function verifyCommittedCheckout({ git, root, commit, tree, when }) {
  const head = git("rev-parse", "--verify", "HEAD^{commit}");
  const dirty = git("status", "--porcelain", "--untracked-files=all");
  if (failed(head) || text(head).trim() !== commit || failed(dirty) || text(dirty).trim()) throw new Error(`hosted CI checkout is not clean commit ${commit} ${when}`);
  const { commitOf } = await import("../snapshot-check.mjs");
  const identity = commitOf(root, commit);
  if (identity.commit !== commit || identity.tree !== tree) throw new Error(`hosted CI commit identity changed ${when}`);
  const { snapshotTree } = await import("../snapshot.mjs");
  const actual = snapshotTree({ checkout: root, dir: root, baseTree: identity.tree });
  if (actual !== tree) throw new Error(`hosted CI checkout is raw tree ${actual}, not tree ${tree} of commit ${commit} ${when}`);
}

/** Select output-binding workspaces from committed source before any Cargo metadata process runs. */
async function committedCargoManifests({ o, git, root, commit, scopeDirs, extraWorkspaces = [] }) {
  const listed = git("ls-tree", "-r", "--name-only", "-z", commit);
  if (failed(listed)) throw new Error(`cannot list the files of ${commit.slice(0, 12)}: ${why(listed)}`);
  const files = text(listed).split("\0").filter(Boolean).map(posix);
  const tracked = (name) => files.filter((f) => f === name || f.endsWith(`/${name}`));
  const allManifests = tracked("Cargo.toml");
  if (!scopeDirs?.length) return { manifests: allManifests, hasCargo: allManifests.length > 0 };
  const fileSet = new Set(files);
  const readCommitted = (file) => {
    if (!fileSet.has(file)) throw new Error(`RK_SCOPE_CLOSURE: ${file} is absent from commit ${commit}`);
    const r = git("show", `${commit}:${file}`);
    if (failed(r)) throw new Error(`RK_SCOPE_CLOSURE: cannot read ${file} of commit ${commit}: ${why(r)}`);
    return text(r);
  };
  const relative = (file) => posix(path.relative(root, file));
  const readFile = (file) => readCommitted(relative(file));
  const exists = (file) => { const rel = relative(file); return fileSet.has(rel) || files.some((f) => f.startsWith(`${rel}/`)); };
  let fixtures = [];
  if (fileSet.has(GATE_CONFIG)) {
    const config = JSON.parse(readCommitted(GATE_CONFIG));
    if (Array.isArray(config.fixtures)) fixtures = config.fixtures.filter((d) => typeof d === "string");
  }
  const tools = await import("../app-closure.mjs");
  const sourceMetadata = tools.cargoMetadataFromManifests({ manifests: tracked("Cargo.toml").filter((m) => !fixtures.some((d) => m.startsWith(`${d.replace(/\/$/, "")}/`))), readFile: readCommitted });
  const inventory = discoverRustWorkspaces({ root, tracked, fixtures, readFile, metadata: sourceMetadata });
  const npm = discoverNpmUnits({ root, tracked, readFile, exists });
  const closure = tools.closureOf({ scopeDirs, npmUnits: npm, rustUnits: inventory.units, readFile: readCommitted });
  // Workspaces the unit's own compile builds beside its closure (a crate's consumer rows under ci/consumers/): bound when the commit tracks their manifest.
  const extra = extraWorkspaces.filter((dir) => fileSet.has(`${dir}/Cargo.toml`));
  const workspaces = [...new Set([...inventory.units.filter((u) => closure.rust.has(u.name)).map((u) => u.cargoDir), ...extra])].sort();
  o.out(`rightkit-gate: Cargo output scope ${scopeDirs.join(", ")}: ${workspaces.length} workspace(s) (${workspaces.join(", ") || "none"}); dependency directories ${closure.dependencyDirs.join(", ")}`);
  return { manifests: workspaces.map((dir) => path.posix.join(dir, "Cargo.toml")), hasCargo: allManifests.length > 0 };
}

async function cargoOutputPinForScope(options) {
  try { return cargoOutputPin({ ...options, manifests: (await committedCargoManifests(options)).manifests }); }
  catch (error) { return { refusal: error.message }; }
}

/** Bind only committed closure workspaces to checkout outputs; runtime Cargo outside that map refuses. */
async function bindUnitCargo({ o, git, root, ds, executor, which, scopeDirs, extraWorkspaces = [], onlyWorkspaces = null }) {
  let selection;
  try { selection = await committedCargoManifests({ o, git, root, commit: ds.commit, scopeDirs, extraWorkspaces }); }
  catch (error) { return { refusal: error.message }; }
  const { manifests, hasCargo } = selection;
  if (!hasCargo) return { env: {}, note: null, cleanup() {} };
  // A scoped npm stage with no declared Rust dependency still gets an empty runtime map: reaching undeclared Cargo refuses.
  let checkoutReal = root; try { checkoutReal = fs.realpathSync.native(root); } catch { /* as given */ }
  const targetDirs = {};
  if (executor.kind === "broker") {
    // A broker host never asks Cargo anything about the checkout: the broker pins every output directory (the receipt's `outputDir`), so
    // all this binding needs is which directories of the commit are Cargo workspaces, and the commit's own manifests say so (read with the
    // object database, never the editable checkout: GATE-CONTRACT "Commit-only build"). The same rule the carried seam applies to a call
    // (cargo-target-shim.mjs locateWorkspaceByWalk): the nearest enclosing manifest that declares `[workspace]`, else the nearest manifest.
    const tracked = new Set(manifests);
    const bodies = new Map();
    const declaresWorkspace = (m) => {
      if (!bodies.has(m)) { const r = git("show", `${ds.commit}:${m}`); bodies.set(m, failed(r) ? null : text(r)); }
      return bodies.get(m);
    };
    for (const m of manifests) {
      let dir = path.posix.dirname(m); let nearest = null; let found = null;
      for (;;) {
        const candidate = dir === "." ? "Cargo.toml" : `${dir}/Cargo.toml`;
        if (tracked.has(candidate)) {
          nearest ??= dir;
          const body = declaresWorkspace(candidate);
          if (body === null) return { refusal: `cannot read ${candidate} of ${ds.commit.slice(0, 12)}` };
          if (/^\s*\[workspace(?:\.[^\]]*)?\]/m.test(body)) { found = dir; break; }
        }
        if (dir === ".") break;
        dir = path.posix.dirname(dir);
      }
      targetDirs[found ?? nearest ?? path.posix.dirname(m)] = null;
    }
  }
  // GB-16/GB-17: a direct host's workspaces and target directories are resolved from the SELECTED COMMIT's snapshot (its committed manifests and `.cargo/config.toml`) and mapped
  // lexically to the checkout's counterpart, exactly as cargoOutputPin does: the live checkout's manifests are never read, so a directory the newer checkout deleted or moved, or a dirty or
  // untracked manifest there, changes nothing.
  if (executor.kind !== "broker") {
    const resolved = cargoOutputPin({ o, git, root, commit: ds.commit, executor, snapshot: ds.path, manifests });
    if (resolved.refusal) return { refusal: resolved.refusal };
    const relOf = (w) => { for (const base of [root, checkoutReal]) { const r = path.relative(base, w); if (!r.startsWith("..") && !path.isAbsolute(r)) return posix(r) || "."; } return posix(w); };
    for (const [workspaceRoot, target] of resolved.workspaces ?? []) targetDirs[relOf(workspaceRoot)] = target;
  }
  const workspaces = Object.keys(targetDirs);
  if (o.platform === "win32" && executor.kind !== "broker") {
    if (workspaces.length === 0) return { refusal: "the selected closure has no Cargo workspace; a direct Windows host cannot wrap undeclared Cargo calls" };
    // A stage that builds exactly one of them (the package stage builds the app's own workspace, the one holding its release config) pins
    // that workspace's target; the others are not built by it (HeardRight: tauri-app-next built, tauri-app-next/qa-e2e not).
    const only = Array.isArray(onlyWorkspaces) ? workspaces.filter((w) => onlyWorkspaces.includes(w)) : [];
    if (workspaces.length > 1 && only.length === 1) return { targetDirs, env: { CARGO_TARGET_DIR: targetDirs[only[0]] }, note: `output directory for the Cargo workspace ${only[0]}: ${targetDirs[only[0]]} (the checkout's); this stage builds no other workspace (${workspaces.filter((w) => w !== only[0]).join(", ")})`, cleanup() {} };
    if (workspaces.length > 1) return { refusal: `the commit holds ${workspaces.length} Cargo workspaces (${workspaces.join(", ")}) and a direct Windows host has no per-call wrapper to bind each to its own target directory` };
    return { targetDirs, env: { CARGO_TARGET_DIR: Object.values(targetDirs)[0] }, note: `output directory for the Cargo workspace: ${Object.values(targetDirs)[0]} (the checkout's)`, cleanup() {} };
  }
  const shimModule = await import("../cargo-target-shim.mjs");
  let broker = null; let realCargo = null;
  if (executor.kind === "broker") {
    if (!o.snapshotBroker) return { refusal: "a broker host reaches Cargo from a unit stage only through the snapshot carrier (deps.snapshotBroker), which this gate was started without" };
    let sb;
    try { sb = await carrierBroker({ o, root, ds, executor, which }); } catch (error) { return { refusal: `the snapshot carrier cannot be built (${error.code ?? "error"}: ${error.message})` }; }
    // Installing the carrier map sends nothing: the ownership records each carried request's sub-handle just before it is sent (snapshot-broker.mjs).
    const { brokerSection } = await import("../snapshot-broker.mjs");
    broker = { checkoutDirs: Object.fromEntries(workspaces.map((w) => [w, path.join(checkoutReal, ...(w === "." ? [] : w.split("/")))])), section: (o.brokerSection ?? brokerSection)(sb) };
    // The unit stages' non-compiling cargo (the `cargo fmt --check` step, metadata) runs directly with the broker's real cargo, as class A
    // does (GC-A11). Without it the wrapper sent `cargo fmt -p <crate> --check` to the broker, which refuses non-compiling requests: every
    // unit compile on the Mac and Dell stopped there (rightkit-document-workspace on the Dell, 2026-11-03, exit 125).
    realCargo = directCargoBinOf(o);
  } else {
    realCargo = realCargoOf(which("cargo"));
    if (!realCargo) return { refusal: "no cargo on PATH to wrap for the unit stages" };
  }
  let installed; let dir = null;
  try { installed = (o.installCargoShim ?? shimModule.installCargoShim)({ base: os.tmpdir(), snapshotRoot: ds.path, targetDirs, broker, realCargo, platform: o.platform, onDir: (d) => { dir = d; } }); }
  catch (error) { if (dir) fs.rmSync(dir, { recursive: true, force: true }); return { refusal: `the runtime cargo wrapper cannot be installed (${error.message})` }; }
  const scan = shimModule.shimScanEnv(o.env, installed);
  const delta = Object.fromEntries(Object.entries(scan).filter(([k, v]) => o.env[k] !== v));
  const { CARRIED_CARGO_MAP_ENV } = await import("../carried-cargo.mjs");
  return {
    targetDirs, // { [cargoDir]: the checkout's target directory } on a direct host (null per workspace on a broker host, whose carried receipt names the output directory)
    env: { ...delta, ...(installed.mode === "broker" ? { [CARRIED_CARGO_MAP_ENV]: installed.mapFile } : {}) }, note: `runtime cargo wrapper ${installed.shim} (${installed.mode}): each cargo call a unit stage reaches binds its workspace's checkout target directory or is refused`,
    // Every call the wrapper decided is in the gate-owned ledger: a refused or unverified call fails the stage even when the test ignored cargo's exit (7.10, decision C).
    check: async () => { const { readLedger, UNSCOPED } = await import("../cargo-ledger.mjs"); return readLedger(installed.ledgerDir, UNSCOPED).problems ?? []; },
    cleanup: () => { fs.rmSync(installed.dir ?? dir, { recursive: true, force: true }); },
  };
}

/**
 * `--to <stage> --unit <selector> [--commit <rev>]` on the snapshot route (row 7.06a): select a commit (HEAD by default), take the
 * repository's build ownership, sync its snapshot, run class A of the snapshot (`check --commit <sha> --full-a --snapshot <dir>`), and build
 * the unit from the snapshot (`--workspace <snapshot>`). Commit and tree come from the selection; no HEAD or status is read. The unit
 * manifest and the local-build grant are read from the selected commit, so one present only in the checkout is refused. Each stage is
 * bracketed by the ledger verify (7.08); Cargo's output is pinned to the checkout's target directory (7.09); `--to release` keeps its
 * publication path and takes only commit and tree from the selection.
 */
async function runUnitSnapshot(o, args, root, git, io, option) {
  const tag = "rightkit-gate:";
  const selector = option("--unit");
  if (o.env.TAURI_CONFIG !== undefined) { o.err(`${tag} BLOCKED: RK_TAURI_CONFIG_OVERRIDE: TAURI_CONFIG is set in the environment; a commit-only build refuses it; nothing ran`); return 1; }
  const match = /^(?:npm:@rightkit\/([a-z0-9-]+)|cargo:(rightkit-[a-z0-9-]+))$/.exec(selector);
  const manifest = match && (match[1] ? `packages/${match[1]}/package.json` : `crates/${match[2]}/Cargo.toml`);
  if (!manifest) { o.err(`${tag} BLOCKED: unit needs npm:@rightkit/<name> or cargo:rightkit-<name>`); return 1; }
  // `--to tool` (tool-build.mjs) builds and enrols one named tool binary; its unit and host are decided here, before ownership, a snapshot or any process.
  const toolRun = option("--to") === "tool";
  if (toolRun) {
    const { toolPlan } = await import("../tool-build.mjs");
    try { toolPlan({ selector, platform: o.platform }); } catch (e) { o.err(`${tag} BLOCKED: ${e.message}; nothing ran`); return 1; }
  }
  const snap = await import("../snapshot.mjs");
  const { acquireBuildOwnership, ownershipFile } = await import("../build-ownership.mjs");
  let selected;
  try { selected = snap.resolveCommit(root, option("--commit") ?? "HEAD"); }
  catch (e) { o.err(`${tag} cannot resolve ${option("--commit") ?? "HEAD"} to a commit: ${e.message}`); return 2; }
  const commit = selected.commit;
  // The canonical manifest and the local-build grant must be tracked in the SELECTED COMMIT (read with the object database, never the checkout).
  for (const file of [".rightkit-local-build.json", manifest]) if (failed(git("cat-file", "-e", `${commit}:${file}`))) { o.err(`${tag} BLOCKED: unit and local grant must be tracked canonical files of ${commit.slice(0, 12)} (${file} is not)`); return 1; }
  const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
  const executor = (o.resolveExecutor ?? gateResolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile }); applyExecutorEnv(o, executor);
  if (executor.kind === "refuse") { o.err(`${tag} BLOCKED: ${executor.problem}`); return 1; }
  if (option("--ci-proof-out") && executor.kind !== "broker") { o.err(`${tag} BLOCKED: --ci-proof-out requires the managed snapshot broker`); return 1; }
  { const decided = routePlan(o, { to: option("--to"), stages: [], devTest: true, executor, capabilities: await brokerCapabilityStateWithHandshake(o, { to: option("--to"), executor }) });
    if (decided.refusal?.code === "RK_BROKER_CAPABILITY_MISSING") { o.err(`${tag} BLOCKED: ${decided.refusal.code}: ${decided.refusal.message}; nothing ran`); return 1; } }
  if (executor.kind === "broker" && !o.snapshotBroker) { o.err(`${tag} BLOCKED: RK_SNAPSHOT_CARRIER_NOT_WIRED: a broker host builds a unit snapshot only through the snapshot carrier; nothing ran`); return 1; }
  const common = git("rev-parse", "--git-common-dir");
  if (failed(common) || !text(common).trim()) { o.err(`${tag} cannot read the git directory`); return 2; }
  const snapshotsAt = o.snapshotRoot ?? snap.snapshotRoot({ platform: o.platform, env: o.env });
  { const mismatch = await declaredSnapshotRootRefusal(o, executor, snapshotsAt); if (mismatch) { o.err(`${tag} BLOCKED: ${mismatch.code}: ${mismatch.message}; nothing ran`); return 1; } }
  let own;
  try { own = (o.acquireOwnership ?? acquireBuildOwnership)({ file: ownershipFile({ platform: o.platform, beside: o.snapshotRoot, repository: snap.repositoryOf(root).key }), stage: `door:unit:${option("--to")}`, snapshot: path.join(snapshotsAt, snap.repositoryOf(root).key), commit, platform: o.platform, env: o.env }); }
  catch (e) { if (e.code !== "busy" && e.code !== "refused") throw e; o.err(`${tag} BLOCKED: ${e.message}; nothing ran${e.code === "refused" ? ", nothing created" : ""}`); return 1; }
  o.out(`${tag} build ownership ${own.joined ? "joined" : "held"} ${own.handle}${own.reclaimed ? ` (reclaimed from ${own.reclaimed.from}: ${own.reclaimed.proofs.join("; ")})` : ""}`);
  try {
    let synced;
    try { synced = (o.syncSnapshot ?? snap.syncSnapshot)({ checkout: root, rev: commit, root: snapshotsAt }); }
    catch (e) { o.err(`${tag} ${e.code === "error" ? "RK_CHECK_ERROR" : "BLOCKED"}: snapshot ${e.message}; nothing ran`); return e.code === "error" ? 2 : 1; }
    o.out(`${tag} snapshot ${synced.path} is commit ${synced.commit} tree ${synced.tree} (written=${synced.written} removed=${synced.removed} unchanged=${synced.unchanged})`);
    if (synced.identity) o.out(`${tag} source identity ${synced.identity.file}: ${synced.identity.written ? "written" : synced.identity.kept ? "kept" : `not written (${synced.identity.reason})`}`);
    const ds = { own, path: synced.path, commit: synced.commit, tree: synced.tree, root: snapshotsAt, ledger: { entries: [] } };
    if (io[UNIT_LIST_UNION]) return await runUnionSnapshot({ o, io, root, git, ds, union: io[UNIT_LIST_UNION], unitListClassA: io.unitListClassA });
    // A unit's test or pack that reaches Cargo must never write `target/` into the snapshot (npm units included): see bindUnitCargo.
    // A check starts no unit command: its own Class A path binds read-only Cargo calls. Resolving every unit's compile output
    // here would run unrelated metadata before the scoped check even begins.
    // A crate's consumer rows (ci/gate.config.json rust.<crate>.consumerRows, local-publish.mjs) build their own Cargo workspaces under ci/consumers/: they
    // are part of this unit's compile, so they are bound beside the crate's workspace (read from the snapshot, which is the commit's tree).
    let privateUnit;
    try { privateUnit = privateUnitManifest({ selector, content: o.readFile(path.join(ds.path, manifest)) }); }
    catch (e) { o.err(`${tag} BLOCKED: ${e.message}; nothing ran`); return 1; }
    if (privateUnit.private && !["check", "compile"].includes(option("--to"))) { o.err(`${tag} BLOCKED: ${selector} is private and cannot be packaged, qualified or published`); return 1; }
    let consumerScope = [];
    if (match[2] && !toolRun) {
      try { consumerScope = [...new Set((JSON.parse(fs.readFileSync(path.join(ds.path, "ci", "gate.config.json"), "utf8")).rust?.[match[2]]?.consumerRows ?? []).map((row) => path.posix.dirname(String(row.manifestPath))))]; }
      catch { consumerScope = []; }
    }
    const bound = option("--to") === "check" || privateUnit.private ? { env: {}, cleanup() {} } : await bindUnitCargo({ o, git, root, ds, executor, which, scopeDirs: [path.posix.dirname(manifest)], extraWorkspaces: consumerScope, ...(toolRun ? { onlyWorkspaces: [path.posix.dirname(manifest)] } : {}) });
    if (bound.refusal) { o.err(`${tag} BLOCKED: RK_SNAPSHOT_OUTPUT_UNPINNED: ${bound.refusal}; nothing ran`); return 1; }
    if (bound.note) o.out(`${tag} ${bound.note}`);
    const envExtra = bound.env;
    try {
    let checkout = root; try { checkout = fs.realpathSync.native(root); } catch { /* as given */ }
    const source = { checkout, commit: ds.commit, tree: ds.tree };
    const reportOut = option("--receipt-out");
    const { runUnitGate, resolveUnit } = await import("../unit-gate.mjs");
    const { localBuildGrantedAt } = await import("../local-publish.mjs");
    const stageGuard = { start: (stage) => openSnapshotStage({ o, ds, root, stage: `unit:${stage}`, tag }), end: async (stage) => {
        if (!(await closeSnapshotStage({ o, ds, root, stage: `unit:${stage}`, step: `unit:${stage}`, tag })).ok) return false;
        const calls = bound.check ? await bound.check() : [];
        for (const p of calls.slice(0, 10)) o.err(`${tag} RK_CHECK_ERROR: [unit:${stage}] a cargo call the unit stage made did not complete: ${typeof p === "string" ? p : JSON.stringify(p)}`);
        return calls.length === 0;
      } };
    // A unit-list run (--units-file) shares one Class A: scoped to every listed unit's directory, run by the first unit, remembered for the rest.
    const unitDir = path.posix.dirname(manifest);
    const shared = io.unitListClassA?.dirs?.includes(unitDir) ? io.unitListClassA : null;
    const runCheck = async (tier) => {
      const memoKey = `${ds.commit}:${tier}`;
      // One Class A verdict per commit and tier for the whole list, pass or fail: the scope is every listed unit's
      // directory, so re-running it per unit after a failure would repeat the same red scans once per unit.
      if (shared && shared.memo.has(memoKey)) { const remembered = shared.memo.get(memoKey); o.out(`${tag} class A (${tier} tier) of this unit list already ${remembered === 0 ? "passed" : `failed (exit ${remembered})`} on ${ds.commit.slice(0, 12)}: not run again`); return remembered; }
      const scopes = shared ? shared.dirs : [unitDir];
      const code = await (o.runCheck ?? (() => main(["check", "--commit", ds.commit, "--full-a", "--snapshot", ds.path, "--rule-tier", tier, ...scopes.flatMap((d) => ["--scope", d])], { ...io, cwd: root, buildOwnership: own })))(tier);
      if (shared) shared.memo.set(memoKey, code);
      return code;
    };
    if (privateUnit.private) {
      if (!localBuildGrantedAt(source)) { o.err(`${tag} BLOCKED: ${selector} requires the committed local-build grant`); return 1; }
      if (option("--to") === "compile") {
        try {
          const { knownRedEntry } = await import("../unit-gate.mjs");
          const red = knownRedEntry({ unit: privateUnit, root: ds.path, platform: o.platform });
          if (red) { o.err(`${tag} BLOCKED: ${selector} is known red (${red.host}: ${red.reason}); nothing compiled`); return 1; }
        } catch (e) { o.err(`${tag} BLOCKED: ${e.message}; nothing compiled`); return 1; }
      }
      const report = { schema: "rightkit.unit-gate-report/1", unit: privateUnit, commit: ds.commit, tree: ds.tree, executor: executor.kind, stages: {} };
      const save = () => { if (reportOut) o.writeFile(path.resolve(o.cwd, reportOut), `${JSON.stringify(report, null, 2)}\n`); };
      for (const stage of option("--to") === "compile" ? ["check", "compile"] : ["check"]) {
        let code = 1;
        if (await openSnapshotStage({ o, ds, root, stage, tag })) {
          try {
            code = stage === "check" ? await runCheck("compile") : await main(["--all", "--commit", ds.commit, "--snapshot", ds.path, "--scope", unitDir], {
              ...io, cwd: root, readStdin: () => "", buildOwnership: own, classAEarlier: true, snapshotLedger: ds.ledger?.entries ?? [],
              onCompileLedger: (entries) => { ds.compileAdmitted = entries; }, [PRIVATE_SCOPED_COMPILE]: { selector, unitDir },
            });
          } catch (e) { o.err(`${tag} ${selector} ${stage}: ${e.message}`); code = 1; }
          if (!(await closeSnapshotStage({ o, ds, root, stage, step: `unit:${stage}`, tag })).ok) code = 1;
        }
        report.stages[stage] = { ok: code === 0, code }; save();
        if (code !== 0) return code;
      }
      return 0;
    }
    if (toolRun) {
      const { runUnitTool } = await import("../tool-build.mjs");
      return await runUnitTool({ selector, root: ds.path, commonDir: path.resolve(root, text(common).trim()), commit: ds.commit, tree: ds.tree, executor, env: o.env, platform: o.platform, source, envExtra, targetDirs: bound.targetDirs ?? {},
        resolve: (sel, r) => resolveUnit(sel, r, { granted: () => localBuildGrantedAt(source) }),
        ...(o.toolBuildDeps ?? {}), // test seam: run, enroll, dataRoot, readKnownRed
        stageGuard, check: runCheck, out: o.out, err: o.err,
        ...(reportOut ? { writeReport: (report) => o.writeFile(path.resolve(o.cwd, reportOut), `${JSON.stringify(o.brokerException ? { ...report, brokerException: o.brokerException } : report, null, 2)}\n`) } : {}),
      });
    }
    return await runUnitGate({ selector, to: option("--to"), root: ds.path, commonDir: path.resolve(root, text(common).trim()), commit: ds.commit, tree: ds.tree, executor, env: o.env, platform: o.platform, source, envExtra, ciProofOut: option("--ci-proof-out") ?? null,
      resolve: (sel, r) => resolveUnit(sel, r, { granted: () => localBuildGrantedAt(source) }),
      ...(o.loadUnitOwner ? { loadOwner: o.loadUnitOwner } : {}),
      ...(o.unitGateDeps ?? {}), // test seam: fetchReceipt, stageCommands, verifyReceipts, selfGit
      stageGuard, check: runCheck, out: o.out, err: o.err,
      ...(reportOut ? { writeReport: (report) => o.writeFile(path.resolve(o.cwd, reportOut), `${JSON.stringify(o.brokerException ? { ...report, brokerException: o.brokerException } : report, null, 2)}\n`) } : {}),
    });
    } finally { bound.cleanup(); }
  } finally {
    const r = own.release();
    if (r?.released === false && !own.joined) o.err(`${tag} build ownership ${own.handle} kept: ${r.why}`);
  }
}

/** One owner, one snapshot and one shared Class A before a unit-list compile. */
async function runUnionSnapshot({ o, io, root, git, ds, union, unitListClassA }) {
  const tag = "rightkit-gate:";
  const { localBuildGrantedAt, consumerRows } = await import("../local-publish.mjs");
  const { knownRedEntry } = await import("../unit-gate.mjs");
  const source = { checkout: root, commit: ds.commit, tree: ds.tree };
  if (!localBuildGrantedAt(source)) { o.err(`${tag} BLOCKED: unit list requires the committed local-build grant`); return 1; }
  let config;
  try {
    config = JSON.parse(o.readFile(path.join(ds.path, "ci/gate.config.json")));
    union.consumerDirs = [];
    for (const row of union.selected) {
      const file = row.selector.startsWith("cargo:") ? `${row.dir}/Cargo.toml` : `${row.dir}/package.json`;
      if (failed(git("cat-file", "-e", `${ds.commit}:${file}`))) throw new Error(`${file} is absent from the selected commit`);
      const unit = privateUnitManifest({ selector: row.selector, content: o.readFile(path.join(ds.path, file)) });
      const red = knownRedEntry({ unit, root: ds.path, platform: o.platform });
      if (red) throw new Error(`${row.selector} is known red (${red.host}: ${red.reason})`);
      if (unit.kind === "cargo") for (const consumer of consumerRows(unit.name, config.rust?.[unit.name] ?? {})) {
        for (const required of [consumer.manifestPath, consumer.manifestPath.replace(/Cargo\.toml$/, "Cargo.lock")]) if (failed(git("cat-file", "-e", `${ds.commit}:${required}`))) throw new Error(`${required} is absent from the selected commit`);
        const dir = path.posix.dirname(consumer.manifestPath);
        union.consumerDirs.push(dir); unitListClassA.dirs.push(dir);
      }
    }
    unitListClassA.dirs = [...new Set(unitListClassA.dirs)];
  } catch (error) { o.err(`${tag} BLOCKED: cannot prepare unit-list scope: ${error.message}`); return 1; }
  if (!await openSnapshotStage({ o, ds, root, stage: "check", tag })) return 1;
  let check = 1;
  try { check = await (o.runCheck ?? (() => main(["check", "--commit", ds.commit, "--full-a", "--snapshot", ds.path, "--rule-tier", "compile", ...unitListClassA.dirs.flatMap((dir) => ["--scope", dir])], { ...io, cwd: root, buildOwnership: ds.own, [UNIT_LIST_UNION]: union, ...((ds.ledger?.entries ?? []).length === 0 ? { snapshotProvedBefore: ds.path } : {}) })))("compile"); }
  catch (error) { o.err(`${tag} unit-list check: ${error.message}`); }
  if (!(await closeSnapshotStage({ o, ds, root, stage: "check", tag })).ok || check !== 0) return check || 1;
  if (!await openSnapshotStage({ o, ds, root, stage: "compile", tag })) return 1;
  let code = 1;
  try {
    code = await main(["--all", "--commit", ds.commit, "--snapshot", ds.path, ...unitListClassA.dirs.flatMap((dir) => ["--scope", dir])], {
      ...io, cwd: root, readStdin: () => "", buildOwnership: ds.own, classAEarlier: true, snapshotLedger: ds.ledger?.entries ?? [],
      onCompileLedger: (entries) => { ds.compileAdmitted = entries; }, [UNIT_LIST_UNION]: union,
      verifyUnionSnapshot: async () => { await verifyDoorSnapshot(o, ds, root, { ledger: ds.ledger?.entries ?? [] }); return true; },
    });
  } catch (error) { o.err(`${tag} unit-list compile: ${error.message}`); }
  const closed = await closeSnapshotStage({ o, ds, root, stage: "compile", tag });
  union.verified = closed.ok && union.completed === true && union.sharedClean === true;
  return closed.ok ? code : 1;
}

/**
 * Whether the door takes the snapshot route on this host: every host but hosted CI, whose runner checkout is the commit. Linux
 * compiles it directly; Mac and Dell compile it through the RustRight snapshot carrier. There is no other route and no flag or
 * environment variable that selects one (rightkit#1301: the checkout route is deleted).
 */
export function snapshotRouteSelected(o) {
  return o.env?.GITHUB_ACTIONS !== "true";
}

/** The app's root inside the snapshot: `<snapshot>/<appDir>`, the snapshot itself for an app at the repository root. */
export function snapshotAppRootOf(snapshotDir, appDir = ".") {
  return appDir === "." ? snapshotDir : path.join(snapshotDir, ...appDir.split("/"));
}

/** What the door run records for a right-release child (source-split.mjs): the checkout it ran in and the snapshot it may read. */
function sourceSplitOf({ root, ds, appDir }) {
  let checkout = root;
  try { checkout = fs.realpathSync.native(root); } catch { /* the path as given */ }
  return { checkout, snapshot: ds.path, commit: ds.commit, tree: ds.tree, appDir };
}

/**
 * Why the snapshot route cannot start on this host, or null. The route is the executor's: `direct` hosts (Linux always, any host
 * with no broker) take it as they are. A broker host compiles the snapshot only through the RustRight snapshot carrier, which the
 * gate reaches through `deps.snapshotBroker` (production wires it on Mac and Dell: `productionEntryDeps`); a gate started without
 * it is refused (RK_SNAPSHOT_CARRIER_NOT_WIRED: tests only, never production), and a broker that cannot be reached is refused too
 * (RK_SNAPSHOT_BROKER_UNAVAILABLE). Neither ever falls back to compiling the checkout.
 */
export function snapshotRouteRefusal(o) {
  const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
  const executor = (o.resolveExecutor ?? gateResolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile });
  if (executor.kind === "refuse") return { code: "RK_SNAPSHOT_BROKER_UNAVAILABLE", message: executor.problem };
  if (executor.kind === "broker" && !o.snapshotBroker) return { code: "RK_SNAPSHOT_CARRIER_NOT_WIRED", message: `a ${o.platform} host compiles a commit snapshot only through the RustRight snapshot carrier (item 7 rows 7.20 to 7.24), and this gate was started without it` };
  return null;
}

/** The refusal codes a route probe can answer with: the codes the stages themselves print (`BLOCKED: <code>: ...`). */
export const ROUTE_REFUSAL_CODES = Object.freeze(["RK_TAURI_CONFIG_OVERRIDE", "RK_STAGE_HOST_MISMATCH", "RK_SNAPSHOT_BROKER_UNAVAILABLE", "RK_SNAPSHOT_CARRIER_NOT_WIRED", "RK_BROKER_CAPABILITY_MISSING", "RK_BROKER_UNAVAILABLE",
  // One signing identity per product (rightkit#1303): the Mac and the Dell refuse an unsigned, ad hoc or re-identified test or dev build.
  ...SIGNING_REFUSAL_CODES,
  // The Mac's shared native helper list (targets.mac.macPackage.nativeHelpers) must validate before a Mac test, dev or package stage runs (rightkit#1308).
  "NATIVE_HELPERS_INVALID"]);
/** The stages `--route` can answer for (`release` writes to the publisher and has no route of its own on a host). */
export const ROUTE_PROBE_STAGES = Object.freeze(["check", "compile", "package", "qualify", "dev", "test", "render"]);
export const ROUTE_SCHEMA = "rightkit-gate.route/1";

/**
 * The capabilities a broker must advertise for the carried stages (rightkit#1301 B2). RustRight reports them in `rustright doctor --json`
 * as `.broker.capabilities` (the HANDSHAKE's `capabilities`: `journal-v1`, `snapshot-readonly-v1`, `snapshot-adapter-v1`, ...; constants
 * in rustright-protocol response.rs). The two required strings ship with RustRight's upcoming change: today's broker (78eda7e) reports no
 * `.broker.capabilities`, so this release refuses every stage that builds until that broker is installed. `snapshot-adapter-v1`
 * (rustright#190, rightkit#1301 B10) is the broker's acceptance of a snapshot-bound adapter job (pnpm, node, QA) inside a gate snapshot,
 * under which a Tauri CLI's compiles run nested: without it every pnpm step of a snapshot stage (an app's dev and package steps, an npm
 * unit's compile and package) is refused by the broker, so the gate refuses first. `stage-grant-v1` is no longer required: the gate mints
 * no stage grant (one model: every compile of a snapshot stage is a carried request or nested under one).
 */
export const REQUIRED_BROKER_CAPABILITIES = Object.freeze(["snapshot-readonly-v1", "snapshot-adapter-v1"]);

/**
 * The broker's capabilities, asked without a build through `rustright doctor --json`: `{ ok: true, capabilities }` or `{ ok: false, problem }`.
 * A missing client, a client that cannot run, a hung (20 s), unparsable or field-less answer is `ok: false`, which the route refuses (fail
 * closed). A doctor that reports an unhealthy host still prints its JSON: the field decides, not the exit status.
 */
export function queryBrokerCapabilities(o, { executor = null } = {}) {
  const client = brokerClientOf(o, executor);
  if (!client) return { ok: false, problem: "the RustRight client is not on PATH" };
  const r = spawnBrokerClient(o.spawn, client, ["doctor", "--json"], { env: brokerEnvOf(o, executor), encoding: "utf8", timeout: 20_000, windowsHide: true }, { platform: o.platform ?? process.platform, readFile: o.readFile });
  if (r?.error || r?.status === null || r?.status === undefined) return { ok: false, problem: `\`rustright doctor --json\` did not finish: ${why(r)}` };
  let doc;
  try { doc = JSON.parse(text(r)); } catch { return { ok: false, problem: `\`rustright doctor --json\` printed no JSON${r.status ? ` (exit ${r.status})` : ""}` }; }
  const caps = doc?.broker?.capabilities;
  if (!Array.isArray(caps) || !caps.every((c) => typeof c === "string")) return { ok: false, problem: "`rustright doctor --json` reports no `.broker.capabilities` (a broker that predates snapshot-readonly-v1 and snapshot-adapter-v1)" };
  // The list is the broker's HANDSHAKE.capabilities only when the client received that handshake: an unreachable broker (doctor reports an empty list for it) advertises nothing, which is a
  // different statement from a broker that advertises few capabilities, so it is a problem naming why, never a list. Each name is advertised once.
  if (doc.broker.reachable !== true) return { ok: false, problem: `the broker is not reachable, so its HANDSHAKE.capabilities were not received (\`rustright doctor --json\` .broker.reachable is ${JSON.stringify(doc.broker.reachable ?? null)}${typeof doc.broker.error === "string" && doc.broker.error ? `: ${doc.broker.error}` : ""})` };
  if (new Set(caps).size !== caps.length) return { ok: false, problem: "`rustright doctor --json` lists a capability twice: the broker's HANDSHAKE.capabilities are not read from an answer that repeats itself" };
  return { ok: true, capabilities: caps };
}

/**
 * What a route decision needs of the broker: `null` when nothing is asked (not a broker executor, hosted CI, `--to check`, or a gate
 * whose broker is an injected fixture without a declared capability query), else the answer. The production carrier asks the real
 * client; a test's injected broker declares its own through `o.brokerCapabilities`.
 */
export function brokerCapabilityState(o, { to, executor }) {
  if (executor?.kind !== "broker" || to === "check" || o.env.GITHUB_ACTIONS === "true") return null;
  if (typeof o.brokerCapabilities === "function") return o.brokerCapabilities(o);
  return typeof o.snapshotBroker?.create === "function" ? queryBrokerCapabilities(o, { executor }) : null;
}

/**
 * GB-51 / GB-62: the capability state a route plans from, with the broker's HANDSHAKE received by the gate itself (broker-handshake.mjs) beside `rustright doctor --json`: both are asked, they must agree,
 * and a plan sees only names advertised by what answered. Not asked (null) exactly when `brokerCapabilityState` asks nothing; an injected fixture broker that declares its own capabilities
 * (`o.brokerCapabilities`) keeps them unless it also declares a handshake (`o.brokerHandshake`, a function returning a parsed handshake or `{ ok: false, problem }`).
 */
export async function brokerCapabilityStateWithHandshake(o, { to, executor }) {
  const doctor = brokerCapabilityState(o, { to, executor });
  if (doctor === null) return null;
  const injected = typeof o.brokerHandshake === "function";
  if (!injected && typeof o.brokerCapabilities === "function") return doctor;
  let handshake;
  try {
    handshake = injected ? await o.brokerHandshake(o) : await queryBrokerHandshake({ ...o, env: brokerEnvOf(o, executor) }, { client: brokerClientOf(o, executor) });
  } catch (error) { handshake = { ok: false, problem: `the broker's HANDSHAKE could not be read (${error?.message ?? error})` }; }
  return mergeCapabilityViews({ doctor, handshake });
}

/**
 * The refusal of `--to release` on a host that is not the trusted publisher, or null: a release uploads sealed bytes and runs only in the release workflow's
 * protected publication job (GITHUB_ACTIONS with RIGHTKIT_PUBLISHER=github-environment); a mobile platform's store upload has its own tier-less route. Asked by a real
 * run before it takes ownership or syncs the snapshot (E2E L3-03); the probe has no release route, so this is not a probe code.
 */
export function releaseRouteRefusal(o, { stages, platform }) {
  if (!stages.includes("release") || MOBILE_PLATFORMS.has(platform)) return null;
  if (o.env.GITHUB_ACTIONS === "true" && o.env.RIGHTKIT_PUBLISHER === "github-environment") return null;
  return { code: "RK_RELEASE_PUBLISHER_ONLY", message: "--to release runs only in the trusted publisher (the release workflow's protected publication job). Run --to qualify on the native host, then dispatch the release workflow for the commit" };
}

/**
 * Which route this host's door takes for `--to <stage>` and whether it refuses, WITHOUT running anything: the decisions of `runStages`
 * in the order it makes them. `runStages` takes its own pre-ownership route and refusal from this very function (rightkit#1297 L1), so
 * the probe and a real run cannot drift (the door-route parity test compares them over the carrier, executor, platform and Tauri
 * combinations):
 *   1. hosted CI (GITHUB_ACTIONS) builds the runner's checkout, which is the commit: no snapshot route;
 *   2. every other host takes the snapshot route (Linux direct; Mac and Dell through the carrier; there is no checkout route);
 *   3. the snapshot route refuses an inherited TAURI_CONFIG;
 *   4. then `snapshotRouteRefusal` (skipped for `--to check`, which compiles nothing): an unreachable broker, a gate without its carrier;
 *   5. then an executor that cannot start (a broker-managed host whose broker is unreachable), on any route.
 * `executor` is a resolved executor. Returns { route, refusal } with
 * refusal null or { code, stage, message }. Reads no checkout, takes no ownership, syncs no snapshot and spawns nothing itself.
 */
export function routePlan(o, { to, stages, devTest, executor, capabilities = null, appConfig = null, stagePlatform = null }) {
  const refuse = (code, stage, message) => ({ code, stage, message });
  const hosted = o.env.GITHUB_ACTIONS === "true";
  const snapshot = !hosted && snapshotRouteSelected(o);
  const route = hosted ? "hosted" : "snapshot";
  if (snapshot) {
    if (o.env.TAURI_CONFIG !== undefined) return { route, refusal: refuse("RK_TAURI_CONFIG_OVERRIDE", to, "TAURI_CONFIG is set in the environment; it would replace the commit's Tauri configuration (externalBin included) for every build below, so a commit-only build refuses it") };
    // A stage that packages or qualifies a platform's artifact runs on that platform's own host: Mac and Windows artifacts are constructed on their
    // native platform and never cross-built (GATE-CONTRACT s1 "Native build target"; PLATFORM-MATRIX: packages only on the matching native host).
    // Refused here, before ownership, a snapshot or any process, so `--route` and a real run say the same thing (E2E L3-03).
    const hostNeeded = STAGE_NATIVE_HOST[stagePlatform];
    if (stagePlatform && (stages.includes("package") || stages.includes("qualify")) && hostNeeded && o.platform !== hostNeeded) {
      return { route, refusal: refuse("RK_STAGE_HOST_MISMATCH", to, `--to ${to} --platform ${stagePlatform} builds the ${stagePlatform} artifact, which is constructed only on a ${hostNeeded === "darwin" ? "Mac" : "Windows"} host (this host is ${o.platform}); a Mac or Windows artifact is never cross-built from another platform, so nothing was started`) };
    }
    const refused = to === "check" ? null : snapshotRouteRefusal(o);
    if (refused) return { route, refusal: refuse(refused.code, to, refused.message) };
    // The broker must accept what the carried stages ask of it (rightkit#1301 B2): a missing or failing capability query refuses.
    if (capabilities && executor?.kind === "broker" && to !== "check") {
      const missing = capabilities.ok ? REQUIRED_BROKER_CAPABILITIES.filter((c) => !capabilities.capabilities.includes(c)) : REQUIRED_BROKER_CAPABILITIES;
      if (!capabilities.ok || missing.length) return { route, refusal: refuse("RK_BROKER_CAPABILITY_MISSING", to, `the broker ${capabilities.ok ? `does not advertise ${missing.join(", ")}` : `cannot be asked for its capabilities (${capabilities.problem})`} (\`rustright doctor --json\` \`.broker.capabilities\`); this release builds only against a broker that accepts snapshot-bound read-only Cargo and snapshot-bound adapter jobs (rightkit#1301, rustright#188, rustright#190)`) };
    }
  }
  // GB-51: a declared native build tool with no carried adapter (xcodegen, xcodebuild, Gradle, CMake) refuses here, at planning, naming the missing
  // capability, instead of after the earlier stages built (the door's launch-time refusal remains the backstop).
  if (snapshot && executor?.kind === "broker" && appConfig?.config && to !== "check") {
    const refusalText = nativeToolRefusal(nativeToolUses({ config: appConfig.config, to, devPlatform: appConfig.nativePlatform ?? appConfig.devPlatform ?? null, platform: appConfig.nativePlatform ?? null }), { capabilities, readFile: appConfig.readFile ?? null, listFiles: appConfig.listFiles ?? null });
    if (refusalText) return { route, refusal: refuse("RK_BROKER_CAPABILITY_MISSING", to, refusalText) };
  }
  // One signing identity per product, every build (rightkit#1303): on the Mac and the Dell a test or dev build that would run unsigned, ad hoc or under
  // another identifier is refused here, from the commit's own config, so `--route` and a real run decide alike. Linux and hosted CI are exempt.
  if (snapshot && appConfig?.config && (to === "test" || to === "dev")) {
    const [first] = suiteSigningRefusals({ config: appConfig.config, platform: o.platform, env: o.env, to, devPlatform: appConfig.devPlatform ?? null, readFile: appConfig.readFile, listFiles: appConfig.listFiles ?? null });
    if (first) return { route, refusal: refuse(first.code, to, first.message) };
  }
  // rightkit#1308: the shared native helper list is validated at run time by the Mac qa-sign step (suite-signing -> development-signing), the dev install
  // (development.mjs) and the Mac package recipe (package-recipe-targets.mjs), where a retired `sharedPackage` refuses NATIVE_HELPERS_INVALID mid-run. The
  // route says so before anything runs, on a Mac, for a test or dev run (whose platform is mac) and a mac package.
  if (snapshot && appConfig?.config && o.platform === "darwin") {
    const macRun = (to === "test" || to === "dev") ? (appConfig.devPlatform ?? "mac") === "mac" : stages.includes("package") && stagePlatform === "mac";
    if (macRun) {
      const problem = nativeHelpersRefusalText(appConfig.config);
      if (problem) return { route, refusal: refuse("NATIVE_HELPERS_INVALID", to, problem) };
    }
  }
  if (executor?.kind === "refuse") return { route, refusal: refuse("RK_BROKER_UNAVAILABLE", to, executor.problem) };
  return { route, refusal: null };
}

/** The message validateNativeHelpers refuses `config.targets.mac.macPackage.nativeHelpers` with (NATIVE_HELPERS_INVALID), or null when the list is absent or valid. */
export function nativeHelpersRefusalText(config) {
  try { nativeHelpersFromConfig(config); return null; }
  catch (error) { if (error?.code === "NATIVE_HELPERS_INVALID") return String(error.message); throw error; }
}

/**
 * The app config of the selected commit for the route's signing refusals, read from the object database (never the checkout): `{ config, readFile }`,
 * `readFile(rel)` being an app-relative file of the commit (text or null). Null when the commit has no readable JSON config (class A judges that).
 * The probe covers JSON configs only: a legacy right-release.config.mjs is not evaluated here (that would run app code in the probe), so `--route` is
 * silent about it. The same policy (suiteSigningRefusals) is enforced at run time on whatever config loaded: test-command.mjs before any suite runs and
 * development.mjs before a dev target builds, so a .mjs config's unsigned product build is refused there, on the Mac and the Dell.
 */
export function routeAppConfig({ git, commit, appDir = ".", platform = null }) {
  if (!commit) return null;
  const show = (rel) => { const r = git("show", `${commit}:${path.posix.normalize(appDir === "." ? rel : `${appDir}/${rel}`)}`); return failed(r) ? null : text(r); };
  const raw = show("right-release.config.json");
  if (raw === null) return null;
  let config;
  try { config = JSON.parse(raw); } catch { return null; }
  const list = (c, rel) => { const r = git("ls-tree", "-r", "--name-only", c, "--", rel); return failed(r) ? null : text(r).split(/\r?\n/).filter(Boolean); };
  return { config, readFile: show, listFiles: gitListRustFiles(list, commit, appDir), devPlatform: platform === "mac" || platform === "win" ? platform : null, nativePlatform: platform ?? null };
}

/** The `--platform` a dev or test run was given: before `--`, or in the arguments after it (a platform after `--` wins, as runDevTest hands it on). */
export function devTestPlatform(args) {
  const at = args.indexOf("--");
  const find = (list) => { const i = list.lastIndexOf("--platform"); return i < 0 ? null : list[i + 1] ?? null; };
  return (at >= 0 ? find(args.slice(at + 1)) : null) ?? find(at >= 0 ? args.slice(0, at) : args);
}

/**
 * `rightkit-gate --to <stage> --route [--platform p] [--commit rev]`: prints one JSON line (`rightkit-gate.route/1`) naming the route
 * the stage would take and the refusal it would meet, and runs nothing: no ownership, no snapshot sync, no check, no run record, no
 * receipt, no right-release. Exit 0 when it answered (a refusal is an answer), 2 when it could not compute one.
 */
async function runRouteProbe(o, args, root, git, { to, platform, stages, devTest, appDir }) {
  const tag = "rightkit-gate: --route:";
  const option = (name) => { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; };
  const snap = await import("../snapshot.mjs");
  let selected;
  try { selected = snap.resolveCommit(root, option("--commit") ?? "HEAD"); }
  catch (e) { o.err(`${tag} cannot resolve ${option("--commit") ?? "HEAD"} to a commit: ${e.message}`); return 2; }
  const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
  const executor = (o.resolveExecutor ?? gateResolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile });
  let plan;
  const capabilities = await brokerCapabilityStateWithHandshake(o, { to, executor });
  try { plan = routePlan(o, { to, stages, devTest, executor, stagePlatform: platform, capabilities, appConfig: to !== "check" ? routeAppConfig({ git, commit: selected.commit, appDir, platform: devTestPlatform(args) }) : null }); }
  catch (e) { o.err(`${tag} ${e.message}`); return 2; }
  // The declared Linux broker's snapshot root: the probe reports the refusal a real run would meet (read-only: one status probe of the broker).
  if (!plan.refusal && plan.route === "snapshot") {
    let at = null; try { at = o.snapshotRoot ?? snap.snapshotRoot({ platform: o.platform, env: o.env }); } catch { at = null; }
    const mismatch = at ? await declaredSnapshotRootRefusal(o, executor, at) : null;
    if (mismatch) plan = { ...plan, refusal: { code: mismatch.code, stage: to, message: mismatch.message } };
  }
  // Read-only: GIT_OPTIONAL_LOCKS=0, so the probe's `git status` never refreshes the index or contends with a writer (rightkit#1297 L4).
  const dirty = o.spawn("git", ["status", "--porcelain", "--untracked-files=no"], { cwd: root, env: { ...o.env, GIT_OPTIONAL_LOCKS: "0" } });
  const version = JSON.parse(fs.readFileSync(new URL("../package.json", import.meta.url), "utf8")).version;
  o.out(JSON.stringify({
    schema: ROUTE_SCHEMA, gate: version, to, platform: platform ?? null, host: o.platform, app: appDir, commit: selected.commit,
    executor: executor.kind, carrier: o.snapshotBroker ? (o.snapshotBroker.kind ?? "injected") : null, route: plan.route,
    brokerCapabilities: capabilities?.ok ? capabilities.capabilities : null, stages: devTest ? ["check", to] : stages, refusal: plan.refusal, checkoutClean: failed(dirty) ? null : !text(dirty).trim(),
  }));
  return 0;
}

// ---- the carrier of a snapshot stage's right-release children (carried-cargo.mjs; rightkit#1301) --------------------------------------

/**
 * The environment of a carried stage's child: the base, the stage's carrier and the caller's extras, without an output directory.
 * The broker pins every Cargo output, so the Tauri CLI and every other child must not look for bundles in a directory the environment
 * names (rightkit#1301 F5): CARGO_TARGET_DIR and CARGO_BUILD_TARGET_DIR are dropped, in any case.
 */
export function carriedEnv(carrier, base, extra = {}) {
  if (!carrier.carried) return { ...base, ...carrier.env, ...extra };
  const strip = (env) => {
    const out = { ...env };
    for (const key of Object.keys(out)) {
      if (/^(CARGO_TARGET_DIR|CARGO_BUILD_TARGET_DIR)$/i.test(key)) delete out[key];
    }
    return out;
  };
  return { ...strip(base), ...carrier.env, ...strip(extra) };
}

// ---- the declared Linux broker's snapshot root --------------------------------------------------------------------------------------------------
// The broker admits a snapshot only under `<its data root>/snapshots`. On Linux the gate's snapshot root is `~/.config/rightkit/gate.json` `snapshotRoot`, else
// `~/.local/state/rightkit/snapshots` (snapshot.mjs snapshotRoot); a host whose broker keeps its data elsewhere would sync thousands of files, take build ownership and submit,
// only to be refused at admission with the ownership left unknown. So the declared route compares the two BEFORE ownership or a sync, and refuses typed.
export const LINUX_SNAPSHOT_ROOT_MISMATCH = "RK_LINUX_BROKER_SNAPSHOT_ROOT_MISMATCH";
const ANSWERING_ROOTS_TIMEOUT_MS = 10_000;

/**
 * What the answering broker says its roots are: the health probe's `STATUS` carries `roots` (`rustright.answering-roots.v1`: dataRoot, controlRoot, ...), after the HANDSHAKE the first event
 * is. Reads lines until a STATUS with roots (or any other end) and resolves `{ ok: true, dataRoot }` or `{ ok: false, problem }`; never rejects. `connect` is the transport (tests, `net.connect`).
 */
export function readAnsweringRoots({ endpoint, identity, connect = (target) => net.connect({ path: target }), timeoutMs = ANSWERING_ROOTS_TIMEOUT_MS }) {
  return new Promise((resolve) => {
    let settled = false; let conn = null; let timer = null; let buffer = "";
    const finish = (result) => { if (settled) return; settled = true; clearTimeout(timer); try { conn?.destroy(); } catch { /* gone */ } resolve(result); };
    const problem = (why) => finish({ ok: false, problem: why });
    try { conn = connect(endpoint); } catch (e) { problem(`cannot connect to the broker at ${endpoint} (${e?.code ?? e?.message ?? e})`); return; }
    timer = setTimeout(() => problem(`the broker at ${endpoint} reported no roots within ${timeoutMs} ms`), timeoutMs);
    conn.on("connect", () => { try { conn.write(`${JSON.stringify({ type: "STATUS", protocol: BUILD_CONTROL_PROTOCOL, runtime: identity, id: HANDSHAKE_PROBE_ID })}\n`); } catch (e) { problem(`cannot send the probe to the broker at ${endpoint} (${e?.message ?? e})`); } });
    conn.on("data", (chunk) => {
      buffer += String(chunk);
      if (buffer.length > (1 << 21)) { problem("the broker's reply is too large"); return; }
      for (let at = buffer.indexOf("\n"); at >= 0; at = buffer.indexOf("\n")) {
        const line = buffer.slice(0, at); buffer = buffer.slice(at + 1);
        let event; try { event = JSON.parse(line); } catch { problem("the broker's reply is not JSON"); return; }
        if (event?.type === "STATUS" && event.roots && typeof event.roots === "object") {
          const root = event.roots.dataRoot;
          if (event.roots.schema !== "rustright.answering-roots.v1" || typeof root !== "string" || !path.isAbsolute(root)) { problem("the broker's STATUS roots are not rustright.answering-roots.v1 with an absolute dataRoot"); return; }
          finish({ ok: true, dataRoot: root }); return;
        }
        if (event?.type !== "HANDSHAKE" && event?.type !== "STATUS") { problem(`the broker answered ${event?.type ?? "something"} to the roots probe`); return; }
      }
    });
    const closed = () => problem(`the broker at ${endpoint} closed without reporting its roots (a broker that predates the answering roots)`);
    conn.on("error", (e) => problem(`the broker connection at ${endpoint} failed (${e?.code ?? e?.message ?? e})`));
    conn.on("close", closed); conn.on("end", closed);
  });
}

/**
 * NN41: the installed broker runtime's identity as one string (`<version>:<brokerDigest>:<clientDigest>` from the `runtime-manifest.json` beside the client), or null when it cannot
 * be read. A cached compile failure is keyed on it, so a broker or toolchain change inside the runtime reruns the failure; with no identity no failure is ever cached.
 * `clients` are tried in order; a bare name is looked up on PATH (a .exe/.cmd/.bat on win32).
 */
export function brokerRuntimeIdentityText({ clients, env = {}, platform = process.platform, readFile = (f) => fs.readFileSync(f, "utf8"), realpath = (f) => fs.realpathSync.native(f), exists = fs.existsSync } = {}) {
  const sep = platform === "win32" ? ";" : ":";
  const pathKey = Object.keys(env).find((k) => k.toLowerCase() === "path") ?? "PATH";
  const candidates = (client) => {
    const name = String(client ?? ""); if (!name) return [];
    if (/[\\/]/.test(name)) return [name];
    const exts = platform === "win32" ? ["", ".exe", ".cmd", ".bat"] : [""];
    return String(env[pathKey] ?? "").split(sep).filter(Boolean).flatMap((dir) => exts.map((e) => path.join(dir, `${name}${e}`)));
  };
  for (const client of clients ?? []) {
    for (const candidate of candidates(client)) {
      try {
        if (!exists(candidate)) continue;
        let file = candidate;
        try { file = resolveBrokerClient(candidate, { platform, readFile }).file; } catch { continue; }
        let real = file; try { real = realpath(file); } catch { real = file; }
        const identity = parseRuntimeIdentity(readFile(path.join(path.dirname(real), RUNTIME_MANIFEST_FILE)));
        if (identity.ok) return `${identity.identity.version}:${identity.identity.brokerDigest}:${identity.identity.clientDigest}`;
      } catch { /* the next candidate */ }
    }
  }
  return null;
}

/** The declared Linux broker's data root: what it reports on the wire (the validated client, its runtime manifest, the declared endpoint), `{ ok, dataRoot }` or `{ ok: false, problem }`. */
export async function queryBrokerDataRoot(o, executor) {
  try {
    const exe = resolveBrokerClient(executor.client, { platform: o.platform, readFile: o.readFile }).file;
    let real = exe; try { real = fs.realpathSync.native(exe); } catch { real = exe; }
    const identity = parseRuntimeIdentity(fs.readFileSync(path.join(path.dirname(real), RUNTIME_MANIFEST_FILE), "utf8"));
    if (!identity.ok) return { ok: false, problem: identity.problem };
    return await readAnsweringRoots({ endpoint: executor.socket, identity: identity.identity, ...(o.connectBroker ? { connect: o.connectBroker } : {}) });
  } catch (e) { return { ok: false, problem: e.message }; }
}

/**
 * null, or `{ code, message }` when the declared Linux broker would refuse this gate's snapshots. Only the declared route is asked (a Linux host with no declared broker stays direct and
 * has no broker root). The broker's own report decides; when it reports none (a broker that predates it, or an unreadable answer) the DECLARED data root (RUSTRIGHT_DATA_ROOT, else
 * RIGHTKIT_DATA_ROOT, MANAGED_ROOT or EXTERNAL_ROOT) is the comparison, and the message says it is only a declaration; with neither nothing is known and nothing is refused here.
 * `snapshotsAt` is the gate's snapshot root.
 */
export async function declaredSnapshotRootRefusal(o, executor, snapshotsAt) {
  if (executor?.kind !== "broker" || executor.route !== LINUX_BROKER_ROUTE) return null;
  const answered = await (o.queryBrokerDataRoot ?? queryBrokerDataRoot)(o, executor);
  let dataRoot; let basis;
  if (answered?.ok) { dataRoot = answered.dataRoot; basis = "the broker reports its data root"; }
  else {
    dataRoot = envNamed(o.env, "DATA_ROOT") || envNamed(o.env, "MANAGED_ROOT") || envNamed(o.env, "EXTERNAL_ROOT");
    basis = `the broker did not report its roots (${answered?.problem ?? "no answer"}), so the DECLARED data root from the environment is the comparison (a declaration, not the broker's word)`;
  }
  if (!dataRoot) return null;
  const real = (p) => { try { return fs.realpathSync.native(p); } catch { return path.resolve(p); } };
  const expected = path.join(dataRoot, "snapshots");
  if (real(expected) === real(snapshotsAt)) return null;
  return { code: LINUX_SNAPSHOT_ROOT_MISMATCH, message: `the gate's snapshot root is ${snapshotsAt}, but the declared Linux broker admits snapshots only under ${expected} (${basis}); set "snapshotRoot": ${JSON.stringify(expected)} in ~/.config/rightkit/gate.json (the gate host configuration), then run again. Nothing was synced and no build ownership was taken` };
}

/** The RustRight client this executor drives: the declared Linux route's validated client, else the absolute `rustright` on PATH (Mac, Dell). */
const brokerClientOf = (o, executor) => (executor?.route === LINUX_BROKER_ROUTE && executor.kind === "broker" ? executor.client : whichIn(o)("rustright").find((c) => path.isAbsolute(c)));
/** The environment the client runs in: the gate's, plus the endpoint the executor declared (the declared Linux route's socket). */
const brokerEnvOf = (o, executor) => (executor?.route === LINUX_BROKER_ROUTE && executor.env ? { ...o.env, ...executor.env } : o.env);
const whichIn = (o) => (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };

/**
 * The gate's snapshot broker for this door run: `o.snapshotBroker` is the production carrier statement (`{ kind, create }`, which
 * builds the broker once: the host's enrolled pins, the declared inputs read from the snapshot, the build ownership and the RustRight
 * client) or, in a test, a ready broker object. Built once per door run.
 */
async function carrierBroker({ o, root, ds, executor = null, which = whichIn(o) }) {
  if (ds.carrierBroker) return ds.carrierBroker;
  const carrier = o.snapshotBroker;
  let sb = carrier;
  if (typeof carrier?.create === "function") {
    const client = executor?.route === LINUX_BROKER_ROUTE && executor.kind === "broker" ? executor.client : which("rustright").find((c) => path.isAbsolute(c));
    const { brokerLeaseState } = await import("../build-ownership.mjs");
    sb = carrier.create({ lease: brokerLeaseState, env: brokerEnvOf(o, executor), platform: o.platform, checkout: root, commit: { sha: ds.commit, tree: ds.tree }, snapshot: { path: ds.path, expectedTree: ds.tree }, ownership: ds.own, client, gateFile: callingGatePath(), spawn: o.spawn });
  }
  ds.carrierBroker = sb;
  return sb;
}

/** The commit's Cargo workspaces in the snapshot, each with its counterpart in the primary checkout: `{ [cargoDir]: checkout path }`, from the files alone. */
async function snapshotCargoWorkspaces({ git, ds, root }) {
  const listed = git("ls-tree", "-r", "--name-only", "-z", ds.commit);
  if (failed(listed)) throw new Error(`cannot list the files of ${ds.commit.slice(0, 12)}: ${why(listed)}`);
  const manifests = text(listed).split("\0").filter((f) => f === "Cargo.toml" || f.endsWith("/Cargo.toml"));
  const { locateWorkspaceByWalk } = await import("../cargo-target-shim.mjs");
  let checkout = root; try { checkout = fs.realpathSync.native(root); } catch { /* as given */ }
  const dirs = {};
  for (const m of manifests) {
    const file = path.join(ds.path, ...m.split("/"));
    let rel;
    try { rel = posix(path.relative(ds.path, path.dirname(locateWorkspaceByWalk(file, path.dirname(file))))) || "."; } catch { rel = posix(path.dirname(m)); }
    if (rel === ".." || rel.startsWith("../")) rel = posix(path.dirname(m)) || ".";
    dirs[rel] = path.join(checkout, ...(rel === "." ? [] : rel.split("/")));
  }
  return dirs;
}

/**
 * The carrier of one snapshot stage's right-release child on a broker host: the stage's workspaces, the broker section, one map and
 * ledger. Returns `{ env, refusal, finish({ stage }), cleanup }`: `env` is added to the
 * child's environment; `finish` runs after the child ended, fails on any call that was refused, unverified or unfinished and on any
 * receipt the gate cannot verify again itself, and removes the carrier. A direct executor, or no snapshot, has nothing to carry.
 */
async function carrierForChildren({ o, ds, root, git, executor, stage, tag = "rightkit-gate:", producer = false }) {
  const none = { env: {}, carried: false, refusal: null, async finish() { return true; }, cleanup() {} };
  if (!ds || executor?.kind !== "broker") return none;
  let base = null;
  try {
    if (!git) throw new Error("the gate's git reader is missing");
    const carried = await import("../carried-cargo.mjs");
    const { brokerSection } = await import("../snapshot-broker.mjs");
    const sb = await carrierBroker({ o, root, ds, executor });
    // Only a PRODUCER carrier (the gate's own web-build, windows-identity and stage-sidecar steps, which write declared inputs by design) takes the gate's digest of the
    // declared inputs at stage open (absent included); the end-of-stage ledger re-verify then accepts any receipt state for an input that digest says the step changed.
    // Every other carrier passes none: each receipt, adapter receipts included, must equal the stage-end digest of every declared input (strict), so a child that swaps
    // an input around an admission and restores it is refused. A malformed declaration or an unreadable input is the refusal below, as before.
    let openInputDigests = null;
    if (producer) {
      const { readDeclaredInputs: readInputsOf, declaredInputDigestsOf: digestInputsOf } = await import("../snapshot-request.mjs");
      openInputDigests = digestInputsOf(ds.path, readInputsOf(ds.path), { allowAbsent: true });
    }
    // Installing the carrier map sends nothing, so it records nothing: the child's own uncarried probes (`rustc -vV`, `--version`) never
    // reach the broker under a snapshot, and each carried request records its sub-handle just before it is sent (snapshot-broker.mjs).
    const checkoutDirs = await snapshotCargoWorkspaces({ git, ds, root });
    base = fs.mkdtempSync(path.join(os.tmpdir(), "rk-carrier-"));
    // The carrier is the map and the ledger on every platform: no PATH wrapper and no stage grant. The child's pnpm, node and QA steps are carried
    // adapter jobs and its Cargo calls carried requests; whatever the Tauri CLI compiles runs nested under the adapter job that started it, and the
    // broker's signed list of those children is what the gate verifies and reads the bundle root from (carried-cargo.mjs).
    const installed = (o.installCarrier ?? carried.installCarrier)({ base, snapshotRoot: ds.path, checkoutDirs, section: (o.brokerSection ?? brokerSection)(sb), known: Object.fromEntries(ds.carriedOutputs ?? []), platform: o.platform, env: o.env });
    o.out(`${tag} [${stage}] carrier (${installed.mode}): ${Object.keys(checkoutDirs).length} workspace(s); each right-release cargo call and each pnpm, node or QA step is one carried request with its own signed receipt, and a compile a step's child makes is nested under it`);
    let finished = false;
    const cleanup = () => { try { installed.cleanup?.(); } catch { /* removed below */ } fs.rmSync(base, { recursive: true, force: true }); };
    return {
      env: installed.env, carried: true, refusal: null, cleanup,
      async finish({ stage: at = stage } = {}) {
        if (finished) return true;
        finished = true;
        let problems;
        try {
          problems = await installed.problems(sb, openInputDigests ? { openInputDigests } : {});
          // The output directories the stage's verified receipts name: the package stage's sealed manifest must record only these.
          for (const dir of installed.outputDirs?.() ?? []) ((ds.carriedByStage ??= {})[at] ??= new Set()).add(dir);
          // And by workspace, for the next stage's children: a receipt already names the output directory, so none asks Cargo for it.
          for (const [root, dir] of installed.outputsByWorkspace?.() ?? []) (ds.carriedOutputs ??= new Map()).set(root, dir);
        } catch (error) { problems = [`the cargo ledger cannot be read (${error.message})`]; } finally { cleanup(); }
        for (const p of problems.slice(0, 10)) o.err(`${tag} RK_CHECK_ERROR: [${at}] a broker call the right-release child made did not complete: ${typeof p === "string" ? p : JSON.stringify(p)}`);
        return problems.length === 0;
      },
    };
  } catch (error) {
    if (base) fs.rmSync(base, { recursive: true, force: true });
    return { ...none, refusal: `${error.code ?? "RK_CARGO_CARRIER"}: the stage's cargo carrier cannot be installed (${error.message})` };
  }
}

/**
 * The checkout's Cargo output directory for the snapshot of `commit`: `{ dir }` (null when the commit has no Cargo manifest) or
 * `{ refusal }`. Each Cargo.toml the COMMIT tracks (git ls-tree of the selected commit, never the checkout's files) is asked, in the
 * commit's SNAPSHOT with the stage's environment, for its `target_directory` (which honours CARGO_TARGET_DIR and the committed
 * `.cargo/config.toml` build.target-dir) and its workspace root. A path the snapshot holds is mapped lexically to the same
 * snapshot-relative path under the checkout (the output directory is the checkout's, GATE-CONTRACT "Output directory & cache"), so a
 * directory the newer checkout deleted or moved, or a dirty or untracked manifest there, changes nothing: the checkout is not read. A
 * path outside the snapshot (an absolute CARGO_TARGET_DIR) stands as reported. Optional manifests restrict discovery to a resolved
 * committed closure. Several workspaces retain independent output directories for the runtime wrapper; they never share one target.
 */
export function cargoOutputPin({ o, git, root, commit, executor, snapshot, manifests: selectedManifests }) {
  const listed = git("ls-tree", "-r", "--name-only", "-z", commit);
  if (failed(listed)) return { refusal: `cannot list the files of ${commit.slice(0, 12)}: ${why(listed)}` };
  const trackedManifests = text(listed).split("\0").filter((f) => f === "Cargo.toml" || f.endsWith("/Cargo.toml"));
  if (selectedManifests && selectedManifests.some((m) => !trackedManifests.includes(m))) return { refusal: "the scoped Cargo output map names a manifest outside the selected commit" };
  const manifests = selectedManifests ?? trackedManifests;
  if (manifests.length && (typeof snapshot !== "string" || !path.isAbsolute(snapshot))) return { refusal: "the output directory is resolved in the commit's snapshot and none was given" };
  const cargo = executor.cargo ?? ["cargo"];
  // The snapshot as the tool reports it may be a realpath of the spelling the gate holds (a symlinked temp root): both spellings map.
  const snapshots = [path.resolve(snapshot ?? "/")];
  try { snapshots.push(fs.realpathSync.native(snapshot)); } catch { /* an absent snapshot fails at the metadata call */ }
  const toCheckout = (p) => {
    for (const base of snapshots) { const rel = path.relative(base, p); if (rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel))) return path.join(root, rel); }
    return p;
  };
  const workspaces = new Map();
  for (const m of manifests) {
    const file = path.join(snapshot, ...m.split("/"));
    const r = o.spawn(cargo[0], [...cargo.slice(1), "metadata", "--offline", "--format-version", "1", "--no-deps", "--manifest-path", file], { cwd: path.dirname(file), env: o.env });
    let doc = null;
    try { doc = failed(r) ? null : JSON.parse(text(r)); } catch { doc = null; }
    if (!doc || typeof doc.target_directory !== "string" || !path.isAbsolute(doc.target_directory) || typeof doc.workspace_root !== "string") return { refusal: `cargo metadata in the snapshot reported no absolute target_directory for ${m}${failed(r) ? ` (${why(r)})` : ""}` };
    workspaces.set(path.resolve(toCheckout(path.resolve(doc.workspace_root))), path.resolve(toCheckout(path.resolve(doc.target_directory))));
  }
  // GB-17: several Cargo workspaces are independent components with their own target caches (GATE-CONTRACT, mixed workspaces never share a target): the per-workspace mapping
  // (workspace root of the checkout -> its own target directory, all resolved from the commit's snapshot) is returned, never one shared directory and no refusal. The caller binds
  // each of right-release's and its children's Cargo calls to its own workspace through the per-call wrapper (bindUnitCargo).
  if (workspaces.size > 1) return { dir: null, workspaces, dirs: [...workspaces.values()] };
  return { dir: workspaces.size ? [...workspaces.values()][0] : null, workspaces, dirs: [...workspaces.values()] };
}

/**
 * `--to render`'s stage (rightkit#1288): the app's declared `renderViews` target (the commit's ci/gate.config.json, read from the verified snapshot) is built
 * through the stage's carrier and run headless outside the broker, one case at a time; the images and a hashed receipt are stored under the host's run-output
 * root (`rightkit-gate-render/<repo>-<commit>-<time>/`). Returns 0 or 1; every refusal is named and nothing is partially passed.
 */
export async function runRenderStage({ o, ds, root, appDir, commit, tree, env, pinnedTarget = null, targetDirs = {}, tag = "rightkit-gate:" }) {
  let gateConfig = null;
  try { gateConfig = JSON.parse(o.readFile(path.join(ds.path, GATE_CONFIG))); } catch { gateConfig = null; }
  const declared = renderViewsOf(gateConfig, appDir);
  if (declared.problems) { for (const p of declared.problems) o.err(`${tag} BLOCKED: [render] ${p}`); o.err(`${tag} nothing compiled`); return 1; }
  if (declared.none) { o.err(`${tag} BLOCKED: [render] ${appDir === "." ? "the repository" : appDir} declares no renderViews target in ${GATE_CONFIG}; nothing compiled`); return 1; }
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  let outDir;
  try { outDir = path.join(gateRunsRoot({ env: o.env, platform: o.platform }), "rightkit-gate-render", `${path.basename(root)}-${commit.slice(0, 12)}-${stamp}-${process.pid}`); }
  catch (error) { o.err(`${tag} BLOCKED: [render] no run-output root to store the images in (${error.message}); nothing compiled`); return 1; }
  o.out(`${tag} [render] ${declared.package}:${declared.bin}, ${declared.cases.length} case(s): ${declared.cases.join(", ")}; evidence in ${outDir}`);
  const result = renderViews({
    target: declared, snapshotRoot: ds.path, commit, tree, outDir, env, platform: o.platform, arch: process.arch,
    targetDir: targetDirs[declared.cargoDir] ?? pinnedTarget,
    runCargo: (cargoArgs, options) => runCargoSync(cargoArgs, { ...options, encoding: "utf8", maxBuffer: 512 * 1024 * 1024 }),
    runExecutable: (exe, exeArgs, options) => o.spawn(exe, exeArgs, { cwd: options.cwd, env: options.env, boundedMs: options.timeoutMs }),
    log: (m) => o.out(`${tag} [render] ${m}`),
  });
  if (!result.ok) { for (const p of result.problems) o.err(`${tag} BLOCKED: [render] ${p}`); return 1; }
  for (const c of result.receipt.cases) o.out(`${tag} [render] ${c.name}: ${c.png} ${c.width}x${c.height} sha256 ${c.sha256}`);
  o.out(`${tag} [render] receipt ${result.receiptFile} (${result.receipt.cases.length} image(s); executable sha256 ${result.receipt.executable.sha256})`);
  return 0;
}

/**
 * rightkit-gate --to dev|test [--commit <rev>] [-- <right-release dev|test arguments>]: the local compiles, on a commit like every other
 * stage (docs/GATE-CONTRACT.md "Commit-only build": `--to dev` and `--to test` build a commit; `--worktree` never admits a build). The
 * door has selected the commit and synced its snapshot under ownership (runStages); this runs full class A of that snapshot
 * (`check --commit <sha> --full-a --snapshot <dir>`, never `--worktree`), then right-release dev|test with the snapshot's app root as
 * its cwd under a live door run, which carries the source split. Hosted CI, whose runner checkout is the commit, keeps its own
 * clean checkout. The snapshot is verified before right-release starts and after it ends (E6): a result that covers no known bytes
 * is refused.
 */
async function runDevTest(o, args, root, git, io, to, appRoot, appDir) {
  const tag = "rightkit-gate:";
  const separator = args.indexOf("--");
  const passthrough = separator >= 0 ? args.slice(separator + 1) : [];
  // `--to dev --platform mac` (the documented form) hands the platform to right-release as `--platform mac`; a platform given after
  // `--` wins (Pulse, 2026-10-27: the flag before `--` was dropped and right-release refused "requires --platform").
  const gateArgs = separator >= 0 ? args.slice(0, separator) : args;
  const platformAt = gateArgs.indexOf("--platform");
  if (platformAt >= 0 && gateArgs[platformAt + 1] && !passthrough.includes("--platform")) passthrough.unshift("--platform", gateArgs[platformAt + 1]);
  const ds = o.doorSnapshot ?? null;
  const commit = ds ? ds.commit : text(git("rev-parse", "HEAD")).trim();
  const tree = ds ? ds.tree : text(git("rev-parse", "HEAD^{tree}")).trim();
  const common = text(git("rev-parse", "--git-common-dir")).trim();
  if (!/^[0-9a-f]{40}$/.test(commit) || !common) { o.err(`${tag} cannot read HEAD and the git directory`); return 2; }
  // Hosted CI's runner checkout is the commit; anywhere else the snapshot is, and the checkout may hold anything.
  if (!ds) {
    const dirty = git("status", "--porcelain", "--untracked-files=no");
    if (failed(dirty) || text(dirty).trim()) { o.err(`${tag} BLOCKED: the checkout differs from HEAD; every stage works on the commit, so commit or stash first`); return 1; }
  }
  const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
  const executor = (o.resolveExecutor ?? gateResolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile }); applyExecutorEnv(o, executor);
  if (executor.kind === "refuse") { o.err(`${tag} BLOCKED: ${executor.problem}`); return 1; }
  o.out(`${tag} --to ${to} for ${commit.slice(0, 12)}${appDir === "." ? "" : ` (app ${appDir})`}: check (class A) -> right-release ${to}; executor ${executor.kind}${ds ? `; snapshot ${ds.path}` : ""}`);
  // Full class A of the commit before right-release compiles anything: of the snapshot, the checks that execute repository code included.
  const checked = await (o.runCheck ?? (() => main(["check", "--commit", ds ? commit : "HEAD", "--full-a", ...(ds ? ["--snapshot", ds.path] : []), ...(appDir === "." ? [] : ["--scope", appDir])], { ...io, cwd: root, ...(ds ? { buildOwnership: ds.own } : {}) })))();
  if (checked !== 0) { o.err(`${tag} --to ${to} stopped at check: nothing compiled`); return 1; }
  // The snapshot is exactly tree + dependencies + the ledger before right-release starts; what it wrote may remain only as ledger
  // entries (a generated `gen/schemas`, a dist), recorded by this gate's own walk (stage-ledger.mjs, decision 2 Q1).
  if (ds && !(await openSnapshotStage({ o, ds, root, stage: to, tag }))) { o.err(`${tag} --to ${to} stopped: the snapshot is not the commit's bytes plus the ledger; nothing compiled`); return 2; }
  // Cargo would resolve `target/` (or a relative build.target-dir) against the snapshot: its output directory is resolved in the checkout
  // and passed absolute, so the warm outputs stay the checkout's and the snapshot holds no target/ (docs/GATE-CONTRACT.md "Output
  // directory & cache"). One Cargo workspace is pinned as a whole; several would share one directory (refused, naming them).
  // A broker host's output is the broker's (the receipts' `outputDir`); only a direct host pins the checkout's target directory.
  const pin = ds && executor.kind !== "broker" ? await cargoOutputPinForScope({ o, git, root, commit, executor, snapshot: ds.path, ...(appDir === "." ? {} : { scopeDirs: [appDir] }) }) : { dir: null };
  if (pin.refusal) { o.err(`${tag} BLOCKED: RK_SNAPSHOT_OUTPUT_UNPINNED: ${pin.refusal}; nothing compiled`); return 1; }
  if (pin.dir) o.out(`${tag} output directory for the Cargo workspace: ${pin.dir} (the checkout's)`);
  // GB-17: several workspaces, each with its own target (never one shared directory): the per-call wrapper binds every Cargo call to its workspace's target, from the snapshot's mapping.
  let pinMulti = null;
  if (pin.dirs?.length > 1) {
    pinMulti = await bindUnitCargo({ o, git, root, ds, executor, which: whichIn(o), ...(appDir === "." ? {} : { scopeDirs: [appDir] }) });
    if (pinMulti.refusal) { o.err(`${tag} BLOCKED: RK_SNAPSHOT_OUTPUT_UNPINNED: ${pinMulti.refusal}; nothing compiled`); return 1; }
    if (pinMulti.note) o.out(`${tag} ${pinMulti.note}`);
  }
  const commonDir = path.resolve(root, common);
  const source = ds ? sourceSplitOf({ root, ds, appDir }) : null;
  // The snapshot's `.git` is a config-only stub, so right-release cannot count commits there (assembled-app.mjs commitCount, ASSEMBLY_VERSION_UNAVAILABLE): the gate
  // counts the commit in the checkout and hands the number down (docs/GATE-CONTRACT.md [GC-A28]). A shallow history or an unreadable count hands down nothing and says
  // so; only an app whose build stamps CFBundleVersion needs the number, and right-release refuses there (it cannot count in the stub either).
  let buildNumber = {};
  if (ds) {
    const shallow = git("rev-parse", "--is-shallow-repository");
    const count = failed(shallow) || text(shallow).trim() === "true" ? null : git("rev-list", "--count", commit);
    if (count && !failed(count) && /^[1-9][0-9]*$/.test(text(count).trim())) buildNumber = { RIGHT_BUILD_NUMBER: text(count).trim() };
    else o.out(`${tag} note: no build number for ${commit.slice(0, 12)} (the checkout is shallow or cannot count its history); an app that stamps CFBundleVersion will refuse with ASSEMBLY_VERSION_UNAVAILABLE`);
  }
  // GB-58 (GATE dev/test exact stage; E19): the selected app's declared sidecars are staged ONCE under this snapshot's ownership before the dependent
  // build, through the same planner and ledger as the compile and package stages (a Tauri crate with externalBin refuses to build without them). The
  // app's own prepare step may stage them too; the ledger's attribution check, not an environment marker, is what keeps that single and exact.
  if (ds) {
    const sidecarConfigs = committedSidecarConfigs({ o, git, commit, appDir, base: ds.path });
    // The declared web build and Windows identity inputs first: the sidecar builds are broker-admitted only when every declared input is in the snapshot.
    const rr = fileURLToPath(new URL("../cli/right-release.mjs", import.meta.url));
    if (!(await stageWebBuildOnce({ o, ds, root, git, appDir, cli: rr, commonDir, commit, tree, stage: to, executor, tag })) || !(await stageWindowsIdentityOnce({ o, ds, root, git, appDir, cli: rr, commonDir, commit, tree, stage: to, executor, tag }))) {
      o.err(`${tag} --to ${to} stopped: the declared compile inputs could not be produced; nothing compiled`); return 1;
    }
    const staged = await stageSidecarsOnce({ o, ds, root, git, configs: sidecarConfigs, appDir, cli: fileURLToPath(new URL("../cli/right-release.mjs", import.meta.url)), commonDir, commit, tree, stage: to, executor, tag });
    if (!staged) { o.err(`${tag} --to ${to} stopped: the declared sidecars could not be staged; nothing compiled`); return 1; }
  }
  // right-release dev and test, and the Tauri CLI and every other child they start, reach Cargo only through the stage's carrier on a broker host.
  const carrier = await carrierForChildren({ o, ds, root, git, executor, stage: to, tag });
  if (carrier.refusal) { o.err(`${tag} BLOCKED: ${carrier.refusal}; nothing compiled`); return 1; }
  const run = startRun({ commonDir, commit, tree, stage: to, ...(source ? { source } : {}) });
  try {
    const cli = fileURLToPath(new URL("../cli/right-release.mjs", import.meta.url));
    // right-release reads its config from its cwd: the app root (appRootOf), inside the snapshot on the snapshot route, not the
    // directory the command was typed in (apps/foo/src is apps/foo; Astra, third pass).
    // The door's own RightKit runs every sub-step: a dev target's `right-release web-build|stage-sidecar` steps resolve `right-release`
    // on PATH, which found the app's node_modules copy (HeardRight on the Dell, 2026-10-27: the app's 0.6.8 guard refused what the
    // door's 0.6.9 admitted). An installed gate (node_modules/@rightkit/release/bin) has its package's bin shims two levels up.
    const childEnv = carriedEnv(carrier, { ...o.env, ...run.env, ...buildNumber }, { ...doorPath({ ...o, env: { ...o.env, ...carrier.env } }), ...(pin.dir ? { CARGO_TARGET_DIR: pin.dir } : (pinMulti?.env ?? {})) });
    // rightkit#1288: `--to render` builds the app's declared view-render target through the carrier and runs it headless; no right-release child is involved.
    if (to === "render") {
      const rendered = ds ? await runRenderStage({ o, ds, root, appDir, commit, tree, env: childEnv, pinnedTarget: pin.dir ?? null, targetDirs: pinMulti?.targetDirs ?? {}, tag }) : (o.err(`${tag} BLOCKED: --to render runs on the snapshot route only; nothing compiled`), 1);
      const carriedRendered = await carrier.finish({ stage: to });
      if (ds && !(await closeSnapshotStage({ o, ds, root, stage: to, step: to, tag })).ok) { o.err(`${tag} --to ${to} stopped: the result covers no known bytes`); return 2; }
      return rendered === 0 && carriedRendered ? 0 : 1;
    }
    const r = (o.spawnInherit ?? inheritLaunch)(process.execPath, [cli, to, ...passthrough], { cwd: source ? snapshotAppRootOf(ds.path, appDir) : appRoot, env: childEnv });
    const carriedOk = await carrier.finish({ stage: to });
    if (ds && !(await closeSnapshotStage({ o, ds, root, stage: to, step: to, tag })).ok) { o.err(`${tag} --to ${to} stopped: the result covers no known bytes`); return 2; }
    return r.status === 0 && carriedOk ? 0 : 1;
  } finally { endRun(run); carrier.cleanup(); try { pinMulti?.cleanup?.(); } catch { /* the wrapper map is the gate's own temp directory */ } }
}

/** A direct (Linux) executor's environment (PATH without RustRight shim directories) for every child this run starts. */
function applyExecutorEnv(o, executor) {
  if (executor?.env && o.env) Object.assign(o.env, executor.env);
  // Outside the broker by Adrian's recorded exception: every child's `rustright cargo|rustc|rustdoc` runs direct, never through the installed
  // client (managed-cargo.mjs NO_BROKER_ENV; it holds only inside this live door run). Without it a child on the Mac or the Dell still reaches the broker.
  if (o.brokerException && o.env && executor?.kind === "direct") o.env[NO_BROKER_ENV] = o.brokerException.exceptionId;
  if (executor?.note && o.out) o.out(`rightkit-gate: ${executor.note}`);
}

/** PATH with the door's own `right-release` shims first (`<node_modules>/.bin` beside an installed gate), or nothing for a checkout. */
export function doorPath(o, gateFile = o.gateFile ?? fileURLToPath(import.meta.url), platform = o.platform ?? process.platform) {
  const bin = path.resolve(path.dirname(gateFile), "..", "..", "..", ".bin");
  const shim = path.join(bin, platform === "win32" ? "right-release.cmd" : "right-release");
  if (!(o.exists ?? fs.existsSync)(shim)) return {};
  const key = platform === "win32" ? Object.keys(o.env ?? {}).find((k) => k.toUpperCase() === "PATH") ?? "Path" : "PATH";
  return { [key]: `${bin}${path.delimiter}${(o.env ?? {})[key] ?? ""}` };
}

// ---- main ---------------------------------------------------------------------------------------------------

/**
 * Run the CLI. Returns the exit code (0 pass, 1 blocked or failed, 2 cannot run, 128+n cancelled). `io` overrides the
 * process, fs and output bindings so tests execute nothing real: spawn, exists, readFile, writeFile, mkdir, chmod,
 * readStdin, out, err, cwd, env, platform, abort (an AbortController), runScans and findRustfmt (the scan runner and the
 * lookup of a usable rustfmt, which the deferred Rust gate needs).
 */
/** The gate flags a `--to <stage>` invocation takes (before a `--` separator, after which the arguments belong to right-release), and whether each takes a value. */
const STAGE_VALUE_FLAGS = Object.freeze(["--to", "--platform", "--tier", "--commit", "--receipt-in", "--receipt-out", "--unit", "--units-file", "--all-units-exception", "--receipt-dir", "--base", "--ci-proof-out"]);
const STAGE_BOOLEAN_FLAGS = Object.freeze(["--route", "--changed"]);

/**
 * Why `args` (a `--to <stage>` invocation) is not a well-formed request, or null. Every value flag needs a value that is not another flag and may be
 * given once; `--flag=value`, unknown flags and stray words are refused. The arguments after a `--` separator are right-release's and are not read.
 */
export function stageArgsProblem(args) {
  const at = args.indexOf("--");
  const gate = at < 0 ? args : args.slice(0, at);
  const seen = new Set();
  for (let i = 0; i < gate.length; i += 1) {
    const a = gate[i];
    if (STAGE_BOOLEAN_FLAGS.includes(a)) continue;
    if (STAGE_VALUE_FLAGS.includes(a)) {
      const value = gate[i + 1];
      if (value === undefined || value.startsWith("--")) return a === "--to" ? `--to needs a stage (${[...STAGES, "dev", "test", "render"].join("|")})` : `${a} needs a value`;
      if (seen.has(a)) return `${a} was given twice`;
      seen.add(a); i += 1; continue;
    }
    return a.startsWith("-") ? `unknown option ${JSON.stringify(a)} (a value follows its flag as the next word: \`--commit <rev>\`, not \`--commit=<rev>\`)` : `unexpected argument ${JSON.stringify(a)}`;
  }
  return null;
}

export async function main(argv, io = {}) {
  const o = {
    spawn: defaultSpawn, exists: fs.existsSync, readFile: (f) => fs.readFileSync(f, "utf8"), writeFile: fs.writeFileSync, mkdir: fs.mkdirSync, chmod: fs.chmodSync, readdir: fs.readdirSync,
    readStdin: () => { try { return process.stdin.isTTY ? "" : fs.readFileSync(0, "utf8"); } catch { return ""; } },
    out: (m) => console.log(m), err: (m) => console.error(m), cwd: process.cwd(), env: process.env, platform: process.platform,
    // Where this gate runs from (the door puts its own shims first on PATH when it is an installed package: doorPath).
    gateFile: fileURLToPath(import.meta.url), ...io,
  };
  const args = [...argv];
  if (args.includes("--ci-proof-out")) {
    try {
      const problem = stageArgsProblem(args);
      if (problem) throw new Error(problem);
      const value = (flag) => { const i = args.indexOf(flag); return i < 0 ? null : args[i + 1]; };
      if (args.some((arg, i) => i % 2 === 0 && !["--to", "--unit", "--commit", "--receipt-out", "--ci-proof-out"].includes(arg)) || value("--to") !== "compile" || value("--unit") !== "cargo:rightkit-decimal" || args.includes("--cargo-jobs")) throw new Error("--ci-proof-out requires the single cargo:rightkit-decimal snapshot compile route");
      if (o.doorSnapshot || !snapshotRouteSelected(o) || !["darwin", "win32"].includes(o.platform)) throw new Error("--ci-proof-out requires the managed Mac or Windows snapshot broker route");
      const paths = o.platform === "win32" ? path.win32 : path.posix;
      const scratch = o.platform === "win32" ? "D:\\scratch" : "/Volumes/D/scratch";
      const file = value("--ci-proof-out");
      if (!file || !paths.isAbsolute(file)) throw new Error("--ci-proof-out needs an absolute path inside D scratch");
      const scratchReal = fs.realpathSync.native(scratch);
      const parentReal = fs.realpathSync.native(paths.dirname(file));
      const relative = paths.relative(scratchReal, parentReal);
      if (relative === ".." || relative.startsWith(`..${paths.sep}`) || paths.isAbsolute(relative)) throw new Error("--ci-proof-out parent is outside D scratch");
      try { fs.lstatSync(file); throw new Error("--ci-proof-out already exists"); }
      catch (error) { if (error.code !== "ENOENT") throw error; }
    } catch (error) { o.err(`rightkit-gate: ${error.message}`); return 2; }
  }
  const widthFlags = args.reduce((found, arg, i) => arg === "--cargo-jobs" ? [...found, i] : found, []);
  if (widthFlags.length) {
    try {
      if (widthFlags.length !== 1 || io[CARGO_WIDTH] !== undefined) throw new Error("--cargo-jobs may be given once");
      const index = widthFlags[0];
      if (args.includes("--") && index > args.indexOf("--")) throw new Error("--cargo-jobs must precede --");
      const width = parseCargoWidth(args[index + 1]);
      const routeProblem = cargoWidthRouteProblem(args);
      if (routeProblem) throw new Error(routeProblem);
      args.splice(index, 2);
      io = { ...io, [CARGO_WIDTH]: width };
      o[CARGO_WIDTH] = width;
    } catch (error) { o.err(`rightkit-gate: ${error.message}`); return 2; }
  }
  // rightkit#1463: a run that does work (a door stage, or a class A / full pass) stamps every log line with its UTC time, so phase times no longer have to be
  // rebuilt from broker receipts. Machine-readable output is never stamped: the --route JSON, a list or baseline, --json, the version, and the one
  // `verdict deterministic` marker line other tools read.
  const stampsLines = (args.includes("--to") || args.includes("--full-a") || args.includes("--all"))
    && !args.some((a) => ["--route", "--list", "--print-baseline", "--json", "--plan", "--version", "-v", "--help", "-h"].includes(a));
  if (stampsLines) {
    // Every line of a multi-line message carries the stamp, not only its first.
    const stamped = (write) => (m) => { if (/^rightkit-gate: verdict deterministic/.test(String(m))) return write(m); const at = new Date().toISOString(); return write(String(m).split("\n").map((l) => `${at} ${l}`).join("\n")); };
    o.out = stamped(o.out); o.err = stamped(o.err);
    stampChildLines = true;
  }
  // The cache bypass is retired (GC-T14; workspace rule: never write or run a no-cache option). Refused by name so an old caller learns why.
  if (args.includes("--no-cache") || (io.env ?? process.env).RK_GATE_CACHE === "0") {
    (io.err ?? console.error)("rightkit-gate: --no-cache and RK_GATE_CACHE=0 are retired: results are reused only under a key naming every input they read, so a changed input always reruns (GC-T14)");
    return 2;
  }
  const option = (name) => { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; };
  // `clippy`: the Clippy route (../clippy-route.mjs; docs/GATE-CONTRACT.md §1 "Clippy route & batching"). Dispatched before the global
  // flags below because everything after `clippy` is cargo's own argv, verbatim (`-v`, `-h` and `--version` are cargo's).
  // `candidate-receipt` (GB-73): the candidate build receipt the RustRight installer takes in place of a clean primary checkout.
  if (args[0] === "candidate-receipt") return (await import("../candidate-receipt-command.mjs")).candidateReceiptCommand(o, args.slice(1));
  if (args[0] === "clippy") return (await import("../clippy-route.mjs")).clippyCommand(o, args.slice(1), { main, io, absoluteEnv: SNAPSHOT_ABSOLUTE_ENV });
  if (args.includes("--version") || args.includes("-v")) {
    o.out(JSON.parse(fs.readFileSync(new URL("../package.json", import.meta.url), "utf8")).version);
    return 0;
  }
  if (args.includes("--help") || args.includes("-h")) { o.out(USAGE); return 0; }
  // `--pre-push` (the plan mode of the retired pre-push hook) no longer exists: commit and push run no gate (docs/GATE-CONTRACT.md).
  if (args.includes("--pre-push")) { o.err(`rightkit-gate: unknown option "--pre-push"\n${USAGE}`); return 2; }
  // `snapshot`: sync the selected commit into the gate-owned source directory and verify it is exactly the commit's tree
  // (../snapshot.mjs; docs/GATE-CONTRACT.md §1). Reads the checkout's commits only, never its working files.
  // `--verify --env <digest>` also requires the installed dependency environment to still be the digest `env-digest --snapshot` recorded.
  if (args[0] === "snapshot") {
    const sc = await import("../snapshot-check.mjs");
    return (await import("../snapshot.mjs")).snapshotCommand(o, args.slice(1), { environmentDigest: ({ checkout, dir, commit }) => classAEnvironmentDigest({ root: dir, gatePackage: o.gatePackage, files: sc.commitFiles(checkout, commit, o.spawn).files, spawn: o.spawn }) });
  }
  // `live-scans`: re-run only the live remote scans (pnpm-audit) of a checked tree, answering the broker's verdict-reuse refresh
  // (../snapshot-check.mjs; GATE-CONTRACT §1, E18; rightkit#1257).
  if (args[0] === "live-scans") return (await import("../snapshot-check.mjs")).liveScansCommand(o, args.slice(1), { discoverPnpmRoots, discoverNpmUnits });
  const sub = args[0] && !args[0].startsWith("-") ? args[0] : null;
  if (sub && sub !== "install-hook" && sub !== "check" && sub !== "stage" && sub !== "env-digest") { o.err(`rightkit-gate: unknown command ${JSON.stringify(sub)}\n${USAGE}`); return 2; }
  // `check` is the deterministic stage (Adrian, 2026-10-25: deterministic always first, one door): the app-check rules, then every
  // class A scan (lockfiles, cargo-deny, cargo-audit, secrets, licences, manifests, workflow lint, syntax...) over the whole tree of the
  // commit, never a diff. Nothing compiles. The RustRight broker runs exactly this before it admits any compile. `--list`,
  // `--print-baseline` and `--only` stay app-check-only views.
  // `check --commit <sha>` without `--snapshot` reads the gate's one warm snapshot of that commit, selected the way
  // every `--to` stage selects it: per-repository build ownership first, then one sync of the commit into the gate-owned source directory, then the
  // check of that snapshot below. There is no export and no copy of the source (docs/GATE-CONTRACT.md, 2026-10-29); the checkout's own files are
  // never read, so an uncommitted edit in it is ignored. Hosted CI has no warm snapshot: its runner checkout is the commit, scanned in place.
  // `check --commit <sha> --full-a` takes the same route (GB-48, GATE 34/36/44, E17): the selected commit's snapshot, never a conversion to
  // `--worktree` that reads the live checkout after a momentary cleanliness check. Only hosted CI keeps the in-place full check.
  const commitCheck = sub === "check"
    && !["--snapshot", "--worktree", "--plan", "--list", "--print-baseline", "--only"].some((f) => args.includes(f)) && o.classA !== false && snapshotRouteSelected(o);
  if (commitCheck) {
    const sc = await import("../snapshot-check.mjs");
    const snap = await import("../snapshot.mjs");
    const { acquireBuildOwnership, ownershipFile } = await import("../build-ownership.mjs");
    const at = option("--repo") ? path.resolve(o.cwd, option("--repo")) : o.cwd;
    const top0 = o.spawn("git", ["rev-parse", "--show-toplevel"], { cwd: at });
    if (failed(top0) || !text(top0).trim()) { o.err("rightkit-gate: not inside a git repository"); return 2; }
    let own = o.buildOwnership ?? null; let ownHere = false;
    try {
      const checkout = fs.realpathSync.native(path.resolve(text(top0).trim()));
      const selected = sc.commitOf(checkout, option("--commit") ?? "HEAD", o.spawn);
      const snapshotsAt = o.snapshotRoot ?? snap.snapshotRoot({ platform: o.platform, env: o.env });
      if (!own) {
        try { own = (o.acquireOwnership ?? acquireBuildOwnership)({ file: ownershipFile({ platform: o.platform, beside: o.snapshotRoot, repository: snap.repositoryOf(checkout).key }), stage: "check", snapshot: path.join(snapshotsAt, snap.repositoryOf(checkout).key), commit: selected.commit, platform: o.platform, env: o.env }); ownHere = true; }
        catch (e) { if (e.code !== "busy" && e.code !== "refused") throw e; o.err(`rightkit-gate: RK_CHECK_ERROR: ${e.message}; nothing ran${e.code === "refused" ? ", nothing created" : ""}`); return 2; }
        o.out(`rightkit-gate: build ownership ${own.joined ? "joined" : "held"} ${own.handle}${own.reclaimed ? ` (reclaimed from ${own.reclaimed.from}: ${own.reclaimed.proofs.join("; ")})` : ""}`);
      }
      let synced;
      try { synced = (o.syncSnapshot ?? snap.syncSnapshot)({ checkout, rev: selected.commit, root: snapshotsAt }); }
      catch (e) { o.err(`rightkit-gate: ${e.code === "error" ? sc.CHECK_ERROR : "BLOCKED"}: snapshot ${e.message}; nothing ran`); return e.code === "error" ? 2 : 1; }
      o.out(`rightkit-gate: snapshot ${synced.path} is commit ${synced.commit} tree ${synced.tree} (written=${synced.written} removed=${synced.removed} unchanged=${synced.unchanged})`);
      if (synced.identity) o.out(`rightkit-gate: source identity ${synced.identity.file}: ${synced.identity.written ? "written" : synced.identity.kept ? "kept" : `not written (${synced.identity.reason})`}`);
      const next = [...argv]; const ci = next.indexOf("--commit");
      if (ci >= 0) next.splice(ci, 2, "--commit", synced.commit); else next.push("--commit", synced.commit);
      next.push("--snapshot", synced.path);
      return await main(next, { ...io, buildOwnership: own });
    } catch (e) { if (e instanceof sc.CheckError) { o.err(`rightkit-gate: ${sc.CHECK_ERROR}: ${e.message}`); return 2; } throw e; }
    finally { if (ownHere) { const r = own.release(); if (r?.released === false && !own.joined) o.err(`rightkit-gate: build ownership ${own.handle} kept: ${r.why}`); } }
  }
  if ((sub === "check" || (!sub && args.includes("--all"))) && args.includes("--snapshot") && !o.buildOwnership) {
    // Per-repository build ownership (../build-ownership.mjs; GATE-CONTRACT §1): held from before the first snapshot read through the
    // end of the check, so no other request can sync the snapshot while class A runs in it. A nested gate whose
    // RIGHTKIT_BUILD_OWNERSHIP names the current owner joins it. Busy (or liveness unknown) runs nothing.
    const { acquireBuildOwnership, ownershipFile } = await import("../build-ownership.mjs");
    const snap = await import("../snapshot.mjs");
    let key;
    try { key = snap.repositoryOf(option("--repo") ? path.resolve(o.cwd, option("--repo")) : o.cwd).key; }
    catch (e) { o.err(`rightkit-gate: RK_CHECK_ERROR: the repository whose build ownership this check takes cannot be identified: ${e.message}; nothing ran`); return 2; }
    // The record is keyed by the repository --repo/cwd names while --snapshot names a separate directory: they must be one repository.
    const snapshotRootHere = o.snapshotRoot ?? snap.snapshotRoot({ platform: o.platform, env: o.env });
    if (path.basename(option("--snapshot") ?? "") !== key || path.resolve(o.cwd, path.dirname(option("--snapshot"))) !== path.resolve(o.cwd, snapshotRootHere)) { o.err(`rightkit-gate: RK_CHECK_ERROR: --snapshot ${option("--snapshot")} is not the snapshot of repository ${key} (its directory name must be the repository key, inside the snapshot root ${snapshotRootHere}); nothing ran`); return 2; }
    let own;
    try { own = (o.acquireOwnership ?? acquireBuildOwnership)({ file: ownershipFile({ platform: o.platform, beside: o.snapshotRoot, repository: key }), stage: sub ?? "compile", snapshot: option("--snapshot") ?? null, commit: option("--commit") ?? null, platform: o.platform, env: o.env }); }
    catch (e) { if (e.code !== "busy" && e.code !== "refused") throw e; o.err(`rightkit-gate: RK_CHECK_ERROR: ${e.message}; nothing ran${e.code === "refused" ? ", nothing created" : ""}`); return 2; }
    o.out(`rightkit-gate: build ownership ${own.joined ? "joined" : "held"} ${own.handle}${own.reclaimed ? ` (reclaimed from ${own.reclaimed.from}: ${own.reclaimed.proofs.join("; ")})` : ""}`);
    try { return await main(argv, { ...io, buildOwnership: own }); }
    finally { const r = own.release(); if (r?.released === false && !own.joined) o.err(`rightkit-gate: build ownership ${own.handle} kept: ${r.why}`); }
  }
  if (sub === "check") {
    // `--snapshot <dir>`: full class A of the gate-owned snapshot of a selected commit (../snapshot-check.mjs). The checkout is only
    // where the commit comes from: its HEAD and status are never read, and the snapshot must be exactly the commit's tree before and
    // after the check, or the check is RK_CHECK_ERROR (exit 2).
    let snapshotDir = null; let snapshotCommit = null; let snapshotCheckout = null; let sc = null;
    if (args.includes("--snapshot")) {
      sc = await import("../snapshot-check.mjs");
      try {
        const at = option("--repo") ? path.resolve(o.cwd, option("--repo")) : o.cwd;
        const top0 = o.spawn("git", ["rev-parse", "--show-toplevel"], { cwd: at });
        if (failed(top0) || !text(top0).trim()) throw new sc.CheckError(`${at} is not a git checkout`);
        snapshotCheckout = fs.realpathSync.native(path.resolve(text(top0).trim()));
        snapshotDir = sc.snapshotOption(args, { checkout: snapshotCheckout });
        snapshotCommit = sc.commitOf(snapshotCheckout, option("--commit"), o.spawn);
        // NN07 (no repeated work): the door's stage opener has just proved this snapshot against the commit with an empty ledger (nothing staged
        // before the check), which is this same proof; the check does not walk the tree a second time.
        if (o.snapshotProvedBefore === snapshotDir) o.out(`rightkit-gate: snapshot ${snapshotDir} is commit ${snapshotCommit.commit} tree ${snapshotCommit.tree} (proved by the door before the check)`);
        else {
          sc.verifySnapshotDir({ checkout: snapshotCheckout, dir: snapshotDir, ...snapshotCommit, when: "before the check" });
          o.out(`rightkit-gate: snapshot ${snapshotDir} is commit ${snapshotCommit.commit} tree ${snapshotCommit.tree} (verified before the check)`);
        }
      } catch (e) { o.err(`rightkit-gate: ${sc.CHECK_ERROR}: ${e.message}`); return 2; }
    }
    if (args.filter((arg) => arg === "--rule-tier").length > 1 || (args.includes("--rule-tier") && option("--rule-tier") === undefined)) {
      o.err("rightkit-gate: check requires exactly one value for --rule-tier"); return 2;
    }
    const requestedTier = option("--rule-tier") ?? "compile";
    if (!["compile", "release"].includes(requestedTier)) { o.err(`rightkit-gate: check --rule-tier must be compile or release, not ${JSON.stringify(requestedTier)}`); return 2; }
    // Full admission runs every deterministic scan; its app rules run at the tier the stage owes (compile unless named): dev, test
    // and package compile on the compile-tier rules, qualify and release name the release tier (rightkit#1296).
    const fullAdmission = args.includes("--full-a") && !args.includes("--list") && !args.includes("--print-baseline") && !args.includes("--only");
    const stripSnapshot = (list) => { const i = list.indexOf("--snapshot"); return i < 0 ? list : [...list.slice(0, i), ...list.slice(i + 2)]; };
    const scopeForward = args.flatMap((a, i) => a === "--scope" ? [a, args[i + 1]] : []);
    const withoutScope = args.slice(1).filter((a, i, list) => a !== "--scope" && list[i - 1] !== "--scope");
    const appArgs = stripSnapshot(withoutScope.filter((a) => a !== "--full-a"));
    if (fullAdmission && !appArgs.includes("--rule-tier")) appArgs.push("--rule-tier", requestedTier);
    // GC-A24: a passing app-rule verdict of a snapshot is reused for the same commit tree, rule code, tier and arguments (ruleVerdictCache).
    const ruleCache = snapshotDir && snapshotCommit && !args.includes("--worktree") && !o.appCheck ? ruleVerdictCache({ checkout: snapshotCheckout, spawn: o.spawn }) : null;
    const appKey = ruleCache ? ruleCache.key({ rule: "app-check", tree: snapshotCommit.tree, args: appArgs, gate: gatePackageIdentity(o.gatePackage) }) : null;
    const appCheck = appKey && ruleCache.passed(appKey)
      ? (o.out(`check: ${snapshotCommit.commit.slice(0, 12)} app rules: cached pass (tree ${snapshotCommit.tree.slice(0, 12)})`), 0)
      : await (o.appCheck ?? (await import("../app-check.mjs")).main)(appArgs, { out: o.out, err: o.err, label: "check", cwd: o.cwd, ...(snapshotDir && !args.includes("--worktree") ? { snapshot: snapshotDir } : {}) });
    if (appKey && appCheck === 0) ruleCache.pass(appKey);
    if (args.includes("--list") || args.includes("--print-baseline") || args.includes("--only") || o.classA === false) {
      // A view of the app rules, never a class A verdict: say so, so no caller can take its exit 0 for a full check.
      // A green partial view exits PARTIAL_EXIT (3), never 0, so a caller that admits only exit 0 rejects it by status alone.
      if (args.includes("--list") || args.includes("--print-baseline")) return appCheck;
      o.err("rightkit-gate: PARTIAL: app rules only (--only); no class A scan ran, so this is not a class A result");
      return appCheck === 0 ? PARTIAL_EXIT : appCheck;
    }
    if (appCheck !== 0) {
      o.err("rightkit-gate: BLOCKED: app rules failed; no further scans ran");
      return appCheck;
    }
    const repoArg = option("--repo");
    const checkCwd = repoArg ? path.resolve(o.cwd, repoArg) : o.cwd;
    // The app rules' tier is the stage's (rightkit#1296); class A's deterministic scans are the same at either tier.
    const ruleTier = requestedTier;
    const classA = await main(["--class-a", "--rule-tier", ruleTier, ...(args.includes("--full-a") ? ["--full-a"] : []), ...(args.includes("--worktree") ? ["--worktree"] : ["--commit", snapshotCommit?.commit ?? option("--commit") ?? "HEAD"]), ...(snapshotDir ? ["--snapshot", snapshotDir] : []), ...scopeForward], { ...io, cwd: checkCwd });
    // RightKit checking itself: its own rules (line endings, consumer recipes, npm surface, version copies and the version feed, dispatcher
    // pins, consumer locks, generated docs, temp paths: scripts/preflight.mjs) and its exports' import safety are class A for the RightKit repository.
    let selfRules = 0;
    const top = o.spawn("git", ["rev-parse", "--show-toplevel"], { cwd: checkCwd });
    const root = failed(top) ? null : path.resolve(text(top).trim());
    const self = o.selfPath ?? fileURLToPath(import.meta.url);
    // RightKit's own scripts are the checked repository's code: they run only after the app rules and every class A scan passed,
    // never after a failure, so a red tree executes nothing of its own.
    // Compared as real, long paths (and case-blind on Windows): git prints the long form of an 8.3 short path such as RUNNER~1.
    const canon = (p) => { let c; try { c = fs.realpathSync.native(p); } catch { c = path.resolve(p); } return process.platform === "win32" ? c.toLowerCase() : c; };
    // In snapshot mode RightKit's own scripts are the snapshot's (the commit's code), never the live checkout's.
    const selfRoot = snapshotDir ?? root;
    const rightkitRepository = root && (() => {
      try { return isRightKitIdentity(o.readFile(path.join(selfRoot, ...RIGHTKIT_IDENTITY_FILE.split("/")))); }
      catch { return false; }
    })();
    if (rightkitRepository && (!o.exists(path.join(selfRoot, "scripts", "preflight.mjs")) || !o.exists(path.join(selfRoot, "scripts", "ci", "exports-import-effects.mjs")))) {
      o.err("rightkit-gate: BLOCKED: RightKit's preflight or exports check is missing");
      selfRules = 1;
    }
    if (root && selfRules === 0 && snapshotDir && rightkitRepository && appCheck === 0 && classA === 0) {
      // The commit's own preflight reads the commit from the checkout's object database; its exports probe imports from the snapshot,
      // whose dependencies class A installed, and reports beside (never over) the commit-bound report publication reads.
      const launch = o.spawnInherit ?? inheritLaunch;
      // GC-A24: RightKit's own preflight and exports probe execute the commit's code with its installed dependencies; a pass is reused
      // for the same commit tree, installed-dependency bytes, node and platform (and scope), and only a pass.
      const effects = path.join(snapshotDir, "scripts", "ci", "exports-import-effects.mjs");
      let selfKey = null;
      if (ruleCache) { try { selfKey = ruleCache.key({ rule: "rightkit-self-rules", tree: snapshotCommit.tree, scope: scopeForward, installed: installedDependencyDigest({ root: snapshotDir, files: sc.commitFiles(snapshotCheckout, snapshotCommit.commit, o.spawn).files, includeCaches: false }), effects: o.exists(effects) }); } catch { selfKey = null; } }
      const selfCached = Boolean(selfKey && ruleCache.passed(selfKey));
      if (selfCached) o.out(`preflight: ${snapshotCommit.commit.slice(0, 12)} cached pass (tree ${snapshotCommit.tree.slice(0, 12)}, same installed dependencies)`);
      const r = selfCached ? { status: 0 } : launch(process.execPath, [path.join(snapshotDir, "scripts", "preflight.mjs"), "--repo", root, "--commit", snapshotCommit.commit], { cwd: root, env: o.env });
      selfRules = r.status === 0 ? 0 : 1;
      if (!selfCached && selfRules === 0 && o.exists(effects) && scopeForward.length === 0) {
        const common = o.spawn("git", ["rev-parse", "--path-format=absolute", "--git-common-dir"], { cwd: root });
        const report = path.join(failed(common) ? path.join(root, ".git") : text(common).trim(), "rightkit-gate", "import-effects", `snapshot-${snapshotCommit.commit}.json`);
        const e = launch(process.execPath, [effects, "--root", snapshotDir, "--no-export", "--report", report], { cwd: snapshotDir, env: o.env });
        if (e.status !== 0) selfRules = 1;
      }
      if (selfKey && !selfCached && selfRules === 0) ruleCache.pass(selfKey);
    } else if (root && selfRules === 0 && !snapshotDir && (rightkitRepository || canon(self).startsWith(canon(root) + path.sep)) && o.exists(path.join(root, "scripts", "preflight.mjs")) && appCheck === 0 && classA === 0) {
      const launch = o.spawnInherit ?? inheritLaunch;
      const selector = args.includes("--worktree") ? ["--worktree"] : ["--commit", option("--commit") ?? "HEAD"];
      const r = launch(process.execPath, [path.join(root, "scripts", "preflight.mjs"), "--repo", root, ...selector], { cwd: root, env: o.env });
      selfRules = r.status === 0 ? 0 : 1;
      // And its exports import cleanly (scripts/ci/exports-import-effects.mjs: every exported module imported in a fresh, permission-
      // limited process; an import that prints, exits or reads its argv fails), which publish.yml's scans job ran alone before. That
      // check probes the checkout's HEAD, so it runs only when HEAD is the requested commit (or the working tree was asked for);
      // any other commit is refused rather than proven by other bytes.
      const effects = path.join(root, "scripts", "ci", "exports-import-effects.mjs");
      if (selfRules === 0 && o.exists(effects)) {
        const requested = args.includes("--worktree") ? null : option("--commit") ?? "HEAD";
        const at = (rev) => { const v = o.spawn("git", ["rev-parse", "--verify", "--quiet", `${rev}^{commit}`], { cwd: root }); return failed(v) ? null : text(v).trim(); };
        // The probe exports HEAD, so a working tree whose tracked or untracked (unignored) files differ from HEAD is refused too.
        const dirty = () => { const st = o.spawn("git", ["status", "--porcelain", "--untracked-files=all"], { cwd: root }); return failed(st) || text(st).trim() !== ""; };
        if (requested !== null && at(requested) !== at("HEAD")) {
          o.err(`rightkit-gate: BLOCKED: the exports import check probes the checkout's HEAD (${String(at("HEAD")).slice(0, 12)}), not the requested ${requested}; check out that commit to check it`);
          selfRules = 1;
        } else {
          // A working-tree check of a dirty checkout probes the working tree's own bytes (--worktree: staged into a throwaway index
          // and exported like a commit), never HEAD's, so local development is checked on what it actually is.
          const worktreeProbe = requested === null && dirty();
          // Like CI's scans job: exports that resolve to build output are INCOMPLETE here and handed, through --report, to the
          // sealed-byte check that publication runs; without --report the probe exits 3 and a green tree could never pass.
          const common = o.spawn("git", ["rev-parse", "--path-format=absolute", "--git-common-dir"], { cwd: root });
          const report = path.join(failed(common) ? path.join(root, ".git") : text(common).trim(), "rightkit-gate", "import-effects", "report.json");
          const e = launch(process.execPath, [effects, "--report", report, ...(worktreeProbe ? ["--worktree"] : [])], { cwd: root, env: o.env });
          if (e.status !== 0) selfRules = 1;
        }
      }
    }
    // A RustRight broker's check names the tree it captured for the compile: the declared inputs there (ignored files the
    // commit does not hold) are judged too, as the bytes the compile will read.
    let inputs = 0;
    const captured = o.env.RIGHTKIT_COMPILE_INPUTS;
    if (captured) {
      const { judgeCompileInputs } = await import("../compile-inputs.mjs");
      const judged = judgeCompileInputs(path.resolve(captured));
      for (const i of judged.inputs) o.out(`rightkit-gate: compile input ${i.path}: ${i.files} file(s), sha256 ${i.sha256}`);
      for (const f of judged.findings) o.err(`rightkit-gate: compile input FAIL ${f}`);
      if (!judged.inputs.length && !judged.findings.length) o.out("rightkit-gate: no declared compile inputs in the captured tree");
      inputs = judged.findings.length ? 1 : 0;
    }
    if (snapshotDir) {
      try { sc.verifySnapshotDir({ checkout: snapshotCheckout, dir: snapshotDir, ...snapshotCommit, when: "after the check" }); }
      catch (e) { o.err(`rightkit-gate: ${sc.CHECK_ERROR}: ${e.message}; the result covers no known bytes`); return 2; }
      o.out(`rightkit-gate: snapshot ${snapshotDir} is still tree ${snapshotCommit.tree} after the check`);
    }
    return Math.max(appCheck, classA, selfRules, inputs);
  }
  const classAOnly = args.includes("--class-a");
  // --full-a: every class A check, the ones that execute the repository's code (tests, typecheck, lint, generated docs) included,
  // on the live working tree, before anything compiles. The door's dev/compile stages require it: a content-only check never
  // authorizes a build (Adrian: class A is everything mechanical and deterministic; reviewer audit 2026-10-28).
  const fullA = classAOnly && args.includes("--full-a");
  // Full class A of a gate-owned snapshot (`check --snapshot`, which verified it): the snapshot is the commit's bytes, so neither the
  // checkout's HEAD nor its cleanliness matters, and the repository's code runs in the snapshot, never in the checkout.
  const snapshotA = classAOnly ? (option("--snapshot") ?? null) : null;
  // The door's compile of a selected commit (`--all --commit <sha> --snapshot <dir>`, joint plan step 4c): every scan, class B's
  // compiles included, reads the verified gate-owned snapshot, never the checkout; outputs go to the checkout's own target
  // directory. A direct host only, until the broker's snapshot carrier lands (step 4d).
  const snapshotB = !classAOnly && args.includes("--all") ? (option("--snapshot") ?? null) : null;
  if (snapshotB && !option("--commit")) { o.err(`${tag} --all --snapshot <dir> needs --commit <sha>`); return 2; }
  let hostedCommitA = null;
  if (fullA && !args.includes("--worktree") && !snapshotA) {
    // A commit is checked in full only where its bytes are the live checkout: it is HEAD and nothing tracked or untracked differs
    // (the unit and app stages already refuse a dirty tree). Then the working tree is that commit, and the checks run there.
    const at = (...a) => o.spawn("git", a, { cwd: o.cwd });
    const want = at("rev-parse", "--verify", `${option("--commit") ?? "HEAD"}^{commit}`), head = at("rev-parse", "--verify", "HEAD^{commit}");
    const dirty = at("status", "--porcelain", "--untracked-files=all");
    if (failed(want) || failed(head) || text(want).trim() !== text(head).trim()) { o.err("rightkit-gate: --full-a checks a commit only when it is HEAD of this checkout: the repository's checks run on the live tree"); return 2; }
    if (failed(dirty) || text(dirty).trim()) { o.err("rightkit-gate: --full-a on a commit needs a clean checkout (the live tree must be exactly that commit); commit or remove the changes"); return 2; }
    if (!snapshotRouteSelected(o)) {
      const tree = at("rev-parse", "--verify", `${text(want).trim()}^{tree}`);
      if (failed(tree) || !text(tree).trim()) { o.err("rightkit-gate: RK_CHECK_ERROR: cannot identify hosted CI commit tree"); return 2; }
      hostedCommitA = { commit: text(want).trim(), tree: text(tree).trim() };
      const i = args.indexOf("--commit"); if (i >= 0) args.splice(i, 2, "--commit", hostedCommitA.commit); else args.push("--commit", hostedCommitA.commit);
    } else {
      const i = args.indexOf("--commit"); if (i >= 0) args.splice(i, 2);
      args.push("--worktree");
    }
  }
  const classACommit = classAOnly && !args.includes("--worktree") ? (option("--commit") ?? "HEAD") : null;
  const scopeDirs = args.flatMap((a, i) => a === "--scope" ? [args[i + 1]] : []);
  if (scopeDirs.length) {
    const bad = scopeDirs.find((d) => typeof d !== "string" || !d || d.startsWith("--") || path.isAbsolute(d) || /^[A-Za-z]:/.test(d) || /(^|[\\/])\.\.([\\/]|$)/.test(d));
    // Full Class A needs a snapshot or the committed hosted checkout. The content pass (`check --commit <sha> --scope <dir>`: publication
    // admission, GC-A03) reads the commit the same way it always did (the warm snapshot, or the runner checkout proven to be that commit
    // in place) and only narrows which executing workspace scans run, so it is the one other form --scope accepts; never the live tree.
    const scopeForm = fullA ? Boolean(snapshotA || hostedCommitA) : (classAOnly && !args.includes("--worktree")) || Boolean(snapshotB && (o[PRIVATE_SCOPED_COMPILE] || o[UNIT_LIST_UNION]) && o.classAEarlier && o.buildOwnership);
    if (!scopeForm || bad !== undefined || scopeDirs.some((d) => d === undefined)) { o.err("rightkit-gate: --scope needs full Class A of a snapshot or committed hosted CI checkout & repository-relative directories"); return 2; }
  }
  if (args.includes("--base") && (!option("--base") || option("--base").startsWith("--"))) { o.err("rightkit-gate: --base needs a ref"); return 2; }

  const top = o.spawn("git", ["rev-parse", "--show-toplevel"], { cwd: o.cwd });
  if (failed(top) || !text(top).trim()) { o.err("rightkit-gate: not inside a git repository"); return 2; }
  const root = path.resolve(text(top).trim());
  const git = (...a) => o.spawn("git", a, { cwd: root });
  if (hostedCommitA) {
    try { await verifyCommittedCheckout({ git, root, ...hostedCommitA, when: "before class A discovery" }); }
    catch (error) { o.err(`rightkit-gate: RK_CHECK_ERROR: ${error.message}; nothing scanned`); return 2; }
  }
  if (sub === "install-hook") return await installHook(o, root);
  // `env-digest`: only the environment identity a full class A verdict depends on (classAEnvironmentDigest), no scans. The broker
  // recomputes it when a queued compile starts and re-runs the check if it changed since the verdict.
  if (sub === "env-digest") {
    // `env-digest --snapshot <dir> --commit <sha>`: the same identity over a verified snapshot (the broker's START recheck of a
    // snapshot-aware verdict): the commit's files, the snapshot's installed dependencies.
    if (args.includes("--snapshot")) {
      const sc = await import("../snapshot-check.mjs");
      try {
        const dir = option("--snapshot"); const rev = option("--commit");
        if (!dir || !path.isAbsolute(dir) || !rev || rev.startsWith("--")) throw new sc.CheckError("env-digest --snapshot needs an absolute directory and --commit <sha>");
        const checkout = fs.realpathSync.native(root);
        const { commit, tree } = sc.commitOf(checkout, rev, o.spawn);
        const real = fs.realpathSync.native(dir);
        sc.verifySnapshotDir({ checkout, dir: real, commit, tree, when: "for env-digest" });
        o.out(`env=${classAEnvironmentDigest({ root: real, gatePackage: o.gatePackage, files: sc.commitFiles(checkout, commit, o.spawn).files, spawn: o.spawn })}`);
        return 0;
      } catch (e) { o.err(`rightkit-gate: ${e.code === sc.CHECK_ERROR ? `${sc.CHECK_ERROR}: ` : "env-digest: "}${e.message}`); return 2; }
    }
    const listed = git("ls-files", "-z", "--cached", "--others", "--exclude-standard");
    if (failed(listed)) { o.err(`rightkit-gate: env-digest cannot list the working tree: ${why(listed)}`); return 2; }
    try { const files = text(listed).split("\0").filter(Boolean).map(posix); o.out(`env=${classAEnvironmentDigest({ root, gatePackage: o.gatePackage, files, spawn: o.spawn })}`); }
    catch (error) { o.err(`rightkit-gate: env-digest: ${error.message}`); return 2; }
    return 0;
  }
  if (args.includes("--plan") && option("--to") !== undefined) { o.err("rightkit-gate: --plan plans the class A scans and runs nothing; it takes no --to stage (Astra, second pass)"); return 2; }
  // `--unit` is a gate flag only before a `--` separator: after it the arguments belong to right-release (an app's `--to dev -- …`).
  const separator = args.indexOf("--");
  const unitFlag = args.includes("--unit") && (separator < 0 || args.indexOf("--unit") < separator);
  // RightKit names its unit on every run (rightkitDoorRule): `--unit <selector>`, `--units-file <json written for this run>` or
  // `--all-units-exception <id>`; exactly one. The unit list is read here, before anything runs, so a bad file is a usage error (exit 2).
  const gateFlag = (name) => args.includes(name) && (separator < 0 || args.indexOf(name) < separator);
  const unitsFileFlag = gateFlag("--units-file"); const allUnitsFlag = gateFlag("--all-units-exception"); const changedFlag = gateFlag("--changed");
  let unitList = null;
  if (unitsFileFlag || allUnitsFlag || changedFlag) {
    const flag = unitsFileFlag ? "--units-file" : changedFlag ? "--changed" : "--all-units-exception";
    if (option("--to") === undefined) { o.err(`rightkit-gate: ${flag} applies to a --to stage\n${USAGE}`); return 2; }
    if ([unitFlag, unitsFileFlag, allUnitsFlag, changedFlag].filter(Boolean).length > 1) { o.err("rightkit-gate: name the unit once: --unit, --changed, --units-file or --all-units-exception, not several"); return 2; }
    if (!changedFlag && (!option(flag) || option(flag).startsWith("--"))) { o.err(`rightkit-gate: ${flag} needs a value\n${USAGE}`); return 2; }
    if (unitsFileFlag) {
      try { unitList = readUnitList((f) => o.readFile(path.resolve(o.cwd, f)), option("--units-file")); }
      catch (e) { o.err(`rightkit-gate: ${e.message}`); return 2; }
    }
  }
  if (option("--to") !== undefined && gateFlag("--base") && !changedFlag) { o.err("rightkit-gate: --base on a --to stage selects the diff base of --changed; pass --changed or drop --base"); return 2; }
  // `--no-broker <exception-id>`: the one documented exception to the mandatory broker on Mac and Dell (broker-exception.mjs). The record is
  // read from the selected commit, never the working tree; every refusal is typed and nothing runs. The run then takes the same stages
  // with the direct executor, and prints and records the exception.
  const noBrokerAt = args.indexOf("--no-broker");
  if (noBrokerAt >= 0 && (separator < 0 || noBrokerAt < separator)) {
    const brokerId = args[noBrokerAt + 1];
    if (option("--to") === undefined) { o.err(`rightkit-gate: ${BROKER_EXCEPTION_ERRORS.usage}: --no-broker <exception-id> applies to a --to stage\n${USAGE}`); return 2; }
    args.splice(noBrokerAt, typeof brokerId === "string" && !brokerId.startsWith("--") ? 2 : 1);
    let granted;
    try {
      let record = null; let inRightKit = true;
      if (typeof brokerId === "string" && !brokerId.startsWith("--")) {
        const pinned = (await import("../snapshot.mjs")).resolveCommit(root, option("--commit") ?? "HEAD");
        const shown = git("show", `${pinned.commit}:${BROKER_EXCEPTIONS_FILE}`);
        record = failed(shown) ? null : text(shown);
        // The repository identity of the commit decides what an exception must name: RightKit's units, or (every other repository, built whole) nothing.
        inRightKit = rightkitIdentityAt(git, pinned.commit);
      }
      // A unit list run covers every listed unit; a run that names no unit (the whole repository) selects every unit.
      const selected = unitFlag ? unitsOfSelector(option("--unit")) : unitList ? unitList.flatMap((u) => unitsOfSelector(u)) : unitsOfSelector(undefined);
      granted = authorizeNoBroker({ id: brokerId, record, platform: o.platform, units: selected, now: o.now ? o.now() : Date.now(), inRightKit });
    } catch (e) {
      if (!e.code?.startsWith("RK_BROKER_EXCEPTION_")) { o.err(`rightkit-gate: cannot resolve ${option("--commit") ?? "HEAD"} to a commit: ${e.message}`); return 2; }
      o.err(`rightkit-gate: BLOCKED: ${e.message}; nothing ran`); return e.code === BROKER_EXCEPTION_ERRORS.usage ? 2 : 1;
    }
    const brokerResolver = o.resolveExecutor ?? gateResolveExecutor;
    o.resolveExecutor = (a) => brokerResolver({ ...a, forceDirect: true });
    o.brokerException = granted;
    io = { ...io, resolveExecutor: o.resolveExecutor, brokerException: granted };
    o.out(`rightkit-gate: BROKER EXCEPTION ${describeBrokerException(granted)}`);
  }
  // The door's stage arguments are read strictly (E2E L3-02): a flag with no value, `--flag=value`, an unknown flag, a repeated value flag or a stray
  // word is refused before git, ownership or any process, never ignored (`--commit=<old sha>` used to build HEAD, a trailing `--to` ran the default gate).
  // A unit run (`--unit`) validates its own, narrower vocabulary below ("unit stages take only ...").
  if (!sub && !unitFlag && args.some((a) => a === "--to" || a.startsWith("--to="))) {
    const problem = stageArgsProblem(args);
    if (problem) { o.err(`rightkit-gate: ${problem}\n${USAGE}`); return 2; }
  }
  // `--route` is a gate flag only before a `--` separator (after it the arguments belong to right-release). It answers which route a
  // stage takes and runs nothing, so it takes no unit, receipt or release stage (rightkit#1297).
  const gateArgs = separator < 0 ? args : args.slice(0, separator);
  if (gateArgs.includes("--route")) {
    const routeTo = option("--to");
    const clash = ["--unit", "--units-file", "--all-units-exception", "--changed", "--receipt-in", "--receipt-out"].filter((f) => gateArgs.includes(f));
    if (routeTo === undefined) { o.err(`rightkit-gate: --route needs --to ${ROUTE_PROBE_STAGES.join("|")}\n${USAGE}`); return 2; }
    if (routeTo === "release") { o.err("rightkit-gate: --route answers for --to check|compile|package|qualify|dev|test; release uploads and has no route of its own"); return 2; }
    if (clash.length) { o.err(`rightkit-gate: --route runs nothing and takes no ${clash.join(" or ")}`); return 2; }
    o.routeProbe = true;
  }
  if (option("--to") !== undefined && unitFlag) {
    const selector = option("--unit");
    const validArgs = args.length % 2 === 0 && args.every((a, i) => i % 2 ? !a.startsWith("--") : ["--to", "--unit", "--receipt-out", "--commit", "--ci-proof-out"].includes(a)) && ["--to", "--unit", "--receipt-out", "--commit", "--ci-proof-out"].every((flag) => args.filter((a) => a === flag).length <= 1);
    if (!validArgs || !selector) {
      o.err("rightkit-gate: unit stages take only --to, --unit and optional --receipt-out, --commit and --ci-proof-out"); return 2;
    }
    // Row 7.06a: on every host but hosted CI a unit stage builds the selected commit's snapshot (Linux direct, Mac and Dell through the carrier).
    if (!o.doorSnapshot && snapshotRouteSelected(o)) return await runUnitSnapshot(o, args, root, git, io, option);
    // Hosted CI only: the runner's checkout is the commit.
    if (option("--commit") !== undefined) { o.err("rightkit-gate: BLOCKED: --commit selects a commit for the snapshot route; hosted CI builds the runner's checkout, which is the commit"); return 2; }
    const head = git("rev-parse", "HEAD"), treeResult = git("rev-parse", "HEAD^{tree}"), common = git("rev-parse", "--git-common-dir"), dirty = git("status", "--porcelain", "--untracked-files=all");
    const commit = text(head).trim(), tree = text(treeResult).trim();
    if (failed(head) || failed(treeResult) || failed(common) || !text(common).trim() || !/^[0-9a-f]{40}$/.test(commit) || !/^[0-9a-f]{40}$/.test(tree) || failed(dirty) || text(dirty).trim()) { o.err("rightkit-gate: BLOCKED: unit stages need clean committed HEAD and tree"); return 1; }
    const match = /^(?:npm:@rightkit\/([a-z0-9-]+)|cargo:(rightkit-[a-z0-9-]+))$/.exec(selector);
    const manifest = match && (match[1] ? `packages/${match[1]}/package.json` : `crates/${match[2]}/Cargo.toml`);
    if (!manifest || failed(git("ls-files", "--error-unmatch", "--", ".rightkit-local-build.json", manifest))) { o.err("rightkit-gate: BLOCKED: unit and local grant must be tracked canonical files"); return 1; }
    const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
    const executor = (o.resolveExecutor ?? gateResolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile }); applyExecutorEnv(o, executor);
    if (executor.kind === "refuse") { o.err(`rightkit-gate: BLOCKED: ${executor.problem}`); return 1; }
    const reportOut = option("--receipt-out");
    // Loaded here, not at the top: the unit path reads Cargo manifests (smol-toml), which an installed copy of this package without
    // node_modules does not carry; the app paths of this gate must keep loading there (app-check.test: the installed copy).
    const { runUnitGate } = await import("../unit-gate.mjs");
    return await runUnitGate({ selector, to: option("--to"), root, commonDir: path.resolve(root, text(common).trim()), commit, tree, executor, env: o.env, platform: o.platform,
      check: async (tier) => {
        await verifyCommittedCheckout({ git, root, commit, tree, when: "before unit:check" });
        const result = await main(["check", "--commit", commit, "--full-a", "--rule-tier", tier, "--scope", path.posix.dirname(manifest)], { ...io, cwd: root });
        await verifyCommittedCheckout({ git, root, commit, tree, when: "after unit:check" });
        return result;
      }, out: o.out, err: o.err,
      ...(reportOut ? { writeReport: (report) => o.writeFile(path.resolve(o.cwd, reportOut), `${JSON.stringify(report, null, 2)}\n`) } : {}),
    });
  }
  if (unitFlag) { o.err("rightkit-gate: --unit requires --to"); return 2; }
  if (option("--to") !== undefined) {
    // A door run that names no unit: RightKit refuses it unless Adrian's exception or a unit list names what runs (rightkitDoorRule).
    const door = await rightkitDoorRule(o, args, root, git, io, { unitsFileFlag, allUnitsFlag, unitList, changedFlag });
    if (door.exit !== undefined) return door.exit;
    if (o[CARGO_WIDTH] !== undefined) { o.err("rightkit-gate: BLOCKED: --cargo-jobs requires the controlled unit-list route"); return 2; }
    return await runStages(o, args, root, git, io);
  }
  if (sub === "stage") return stageForPublisher(o, args, root, git);

  const planOnly = args.includes("--plan");
  let base = option("--base") ?? "origin/main";
  const tag = "rightkit-gate:";

  // git pre-push: "<local ref> <local sha> <remote ref> <remote sha>" per pushed ref. A plan reads no stdin.
  // git's push lines are split on single ASCII spaces only (parsePushLines): a ref name may hold Unicode whitespace. A line that does not read as four fields
  // (git hands the local field over verbatim, so a rev expression such as `HEAD@{1 minute ago}` or `HEAD^{/one thing}` holds spaces)
  // refuses the push on every host and in every repository, never a line that disappears: its pushed commit could not be compared with
  // HEAD and its remote ref could not be judged. `pushedRemoteShas` are the remote commits being replaced.
  // A push of ref deletions alone carries no content: nothing to check, nothing to plan (the hook skips its check the same way).
  let deletionsOnly = false;
  if (!planOnly && !classAOnly) {
    const head = text(git("rev-parse", "HEAD")).trim();
    const parsed = parsePushLines(o.readStdin());
    // Every line is read before any shortcut: a malformed line hides what is pushed, so it blocks even beside deletions (Astra, third pass).
    if (parsed.malformed.length) {
      o.err(`${tag} BLOCKED: pre-push stdin line ${parsed.malformed[0].number} is not "<local ref> <local sha> <remote ref> <remote sha>" (four fields separated by single spaces), so the pushed refs cannot all be read and the pushed commit cannot be compared with HEAD; push a plain ref (git push <remote> HEAD:<branch>)`);
      return 1;
    }
    deletionsOnly = parsed.lines.length > 0 && parsed.lines.every((l) => ZERO.test(l.localSha));
    if (deletionsOnly) { o.out(`${tag} only deletions are pushed; nothing to check, nothing planned`); return 0; }
    for (const { localSha, remoteRef, remoteSha } of parsed.lines) {
      if (ZERO.test(localSha)) continue; // a delete pushes no content
      if (localSha !== head) {
        o.err(`${tag} BLOCKED: pushing ${localSha.slice(0, 10)} to ${remoteRef}, but HEAD is ${head.slice(0, 10)}; broker scans read the checkout, so check the pushed commit out first (git checkout --detach ${localSha.slice(0, 10)}), push HEAD:<branch>, then return`);
        return 1;
      }
      if (!ZERO.test(remoteSha) && !failed(git("cat-file", "-e", `${remoteSha}^{commit}`))) base = remoteSha;
    }
  }

  // ---- one door: the executor, then the exact snapshot every scan plans from and reads -----------------------------------
  // The executor is chosen here, never by the caller (Adrian, 2026-10-25): the RustRight broker where this machine has it, a refusal
  // on a broker-managed machine whose broker cannot be reached, direct cargo on any other host (a cloud container, a CI runner).
  const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
  const executor = (o.resolveExecutor ?? gateResolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile }); applyExecutorEnv(o, executor);
  // A broker-managed host whose broker is unreachable refuses the moment a Cargo manifest is in the snapshot (below): a repository
  // with no Rust owes nothing to the broker (a git hook's minimal PATH, a JS-only app), and the class A scans never compile.
  if (o[CARGO_WIDTH] !== undefined && !classAOnly) {
    try {
      if (executor.kind !== "broker") throw new Error("--cargo-jobs requires a broker reporting its compile-token pool");
      const client = executor.client ?? "rustright";
      const result = spawnBrokerClient(o.spawn, client, ["list"], { env: brokerEnvOf(o, executor), encoding: "utf8", timeout: 20_000, windowsHide: true }, { platform: o.platform, readFile: o.readFile });
      if (failed(result)) throw new Error("cannot read broker compile-token pool");
      const size = validateCargoWidthPool(o[CARGO_WIDTH], JSON.parse(text(result)));
      o.out(`rightkit-gate: Cargo width ${o[CARGO_WIDTH]} (broker token pool ${size})`);
    } catch (error) { o.err(`rightkit-gate: BLOCKED: ${error.message}`); return 1; }
  }
  const cargoCommand = executor.cargo ?? ["cargo"];
  // The declared Linux broker route's cargo is `[<absolute validated client>, "cargo"]`, which the scan planner (planScans: `["rustright","cargo"]` or
  // `["cargo"]`) refuses, and a PATH `rustright` is never the client. So the scans are PLANNED with the logical `["rustright","cargo"]` and every spawn the
  // gate hands the scans, the discovery of the snapshot's workspaces and the tool probes resolves that one name to the executor's validated client,
  // with the endpoint the executor declared (`withDeclaredClient`); nothing starts a `rustright` found on PATH. The carried requests
  // (snapshotCargoSpawn's carrySpawn, the snapshot carrier) name the client by absolute path already. Every other route plans `cargoCommand` itself.
  const declaredClient = executor.kind === "broker" && executor.route === LINUX_BROKER_ROUTE && typeof executor.client === "string" ? executor.client : null;
  const planCargoCommand = declaredClient ? ["rustright", ...cargoCommand.slice(1)] : cargoCommand;
  const withDeclaredClient = (spawn) => (declaredClient
    ? (command, args, opts = {}) => (command === "rustright" ? spawn(declaredClient, args, { ...opts, env: { ...(opts.env ?? o.env), ...(executor.env ?? {}) } }) : spawn(command, args, opts))
    : spawn);
  // A broker host (Mac, Dell) compiles a commit snapshot through the RustRight snapshot carrier: `deps.snapshotBroker`, built in the
  // snapshot-compile region below (item 7 rows 7.20 to 7.24). A broker-managed host whose broker cannot be reached is refused with
  // the executor's own problem, never compiled around.
  if (snapshotB && executor.kind === "refuse") { o.err(`${tag} BLOCKED: ${executor.problem}; nothing ran`); return 1; }
  // The snapshot: the commit or tree being checked (check --commit), HEAD for the gate, or the working tree (check --worktree, --plan).
  const allFilesMode = classAOnly || args.includes("--all");
  const snapshot = planOnly ? null : classAOnly ? classACommit : snapshotB ? option("--commit") : "HEAD";
  // Every effectful gate entry owes the app rules, not only `check` (Astra, 2026-10-26): a bare or --all run on HEAD runs them
  // first, in the tier asked for (compile by default), and plans nothing while they are red. --plan and --class-a (which runs them in
  // `check`) are untouched.
  if (snapshot && !classAOnly) {
    // NN07/NN02: the same app-rule verdict for the same tree, rules, tier and arguments is reused (ruleVerdictCache, as `check` does): a warm run reran it every time.
    const appArgv = ["--repo", root, "--commit", snapshot, "--rule-tier", option("--rule-tier") ?? "compile"];
    const appTree = o.appCheck ? null : git("rev-parse", `${snapshot}^{tree}`);
    const appCache = appTree && !failed(appTree) ? ruleVerdictCache({ checkout: root, spawn: o.spawn }) : null;
    const appRulesKey = appCache ? appCache.key({ rule: "app-check-entry", tree: text(appTree).trim(), args: appArgv.slice(2), gate: gatePackageIdentity(o.gatePackage) }) : null;
    const appRules = appRulesKey && appCache.passed(appRulesKey)
      ? (o.out(`check: ${String(snapshot).slice(0, 12)} app rules (${option("--rule-tier") ?? "compile"} tier): cached pass (tree ${text(appTree).trim().slice(0, 12)})`), 0)
      : await (o.appCheck ?? (await import("../app-check.mjs")).main)(appArgv, { out: o.out, err: o.err, label: "check", cwd: o.cwd });
    if (appRulesKey && appRules === 0) appCache.pass(appRulesKey);
    if (appRules !== 0) { const id = git("rev-parse", String(snapshot)); o.err(`${tag} BLOCKED: the app rules are red for ${(failed(id) ? String(snapshot) : text(id).trim()).slice(0, 12)}; no scan was planned or run`); return appRules; }
  }

  const ac = o.abort ?? new AbortController();
  let snapshotGitConfig = null;
  let cargoShimDir = null;
  const cleanup = () => {
    if (cargoShimDir) fs.rmSync(cargoShimDir, { recursive: true, force: true });
    // GC-A24: the config-only origin file stays (snapshotProblem admits exactly it, and the sync keeps exactly it), so an unchanged
    // snapshot is not rewritten on every run; a snapshot of a checkout whose origin changed gets it pruned by the next sync.
    void snapshotGitConfig;
  };
  const signals = [];
  try {
    let repo = root;
    let snapshotBCommit = null;
    // On a broker host the gate's snapshot is the broker's managed one, where RustRight refuses a root request ("a root request cannot run inside a
    // managed snapshot", rightkit#1311): the gate's own Cargo reads there (unit discovery, workspace members) are carried read-only requests,
    // with the broker's signed receipt verified (snapshot-metadata.mjs). The checkout, git and a direct host keep the plain spawn.
    let snapshotSpawn = withDeclaredClient((c, a, opts) => o.spawn(c, a, opts));
    let discoveryBroker = null;
    // On a broker host, the spawn every Class A Cargo scan of the snapshot runs through: a Cargo call inside the snapshot is a carried read-only
    // request of one of the exact admitted shapes (snapshot-metadata.mjs), anything else passes through (node, git, the client's probe).
    let scanSpawn = null;
    if (snapshot && snapshotB) {
      // Direct host: no broker grant to satisfy, so no config-only .git; the snapshot is scanned and compiled in place.
      repo = snapshotB;
    } else if (snapshot && snapshotA) {
      // The verified gate-owned snapshot is the commit's bytes: it is scanned in place, no export.
      repo = snapshotA;
      // As for an export (rightkit#1215): rustright binds the tracked .rightkit-local-build.json grant to the origin URL in .git/config,
      // so a locked cargo metadata in the snapshot needs a config-only .git naming exactly the checkout's origin. It is not a
      // repository (no HEAD, no objects), the seeded identity never reads .git, and it is removed when the check ends.
      const origin = o.spawn("git", ["config", "--get", "remote.origin.url"], { cwd: root });
      const originUrl = failed(origin) ? "" : text(origin).trim();
      if (originUrl && !o.exists(path.join(repo, ".git"))) {
        o.mkdir(path.join(repo, ".git"), { recursive: true });
        snapshotGitConfig = path.join(repo, ".git", "config");
        o.writeFile(snapshotGitConfig, `[remote "origin"]\n\turl = ${originUrl}\n`);
      }
    } else if (snapshot) {
      // No export, no copy of the source (docs/GATE-CONTRACT.md, 2026-10-29): a host that has the gate's warm snapshot reads the commit
      // through it (`check --commit` selects it above, `--to` stages hand it over). What reaches here is a run whose checkout IS the
      // commit: hosted CI's runner checkout, or HEAD for the legacy gate run. It is scanned in place, only after proving it is that commit.
      if (classAOnly && snapshotRouteSelected(o)) { o.err(`${tag} BLOCKED: class A of a commit reads the gate's warm snapshot of it, never the checkout; run \`rightkit-gate check --commit <sha>\` (it selects the snapshot) or pass \`--snapshot <dir>\``); return 2; }
      const want = git("rev-parse", "--verify", `${snapshot}^{commit}`); const head = git("rev-parse", "--verify", "HEAD^{commit}");
      if (snapshot !== "HEAD" && (failed(want) || failed(head) || text(want).trim() !== text(head).trim())) {
        o.err(`${tag} BLOCKED: ${snapshot} is not HEAD of this checkout, and this run has no gate-owned snapshot of it: only the commit the checkout already is can be scanned in place (hosted CI's runner checkout); check another commit with \`check --commit <sha>\` on a host with the gate's warm snapshot`);
        return 2;
      }
    }
    // Every file of the snapshot, once: units, manifests, lockfiles and the changed set are all read from what is scanned, never from
    // the live checkout (a live edit cannot change what gets planned).
    const listed = snapshot ? git("ls-tree", "-r", "--name-only", "-z", snapshot) : git("ls-files", "-z", "--cached", ...(classAOnly ? ["--others", "--exclude-standard"] : []));
    if (failed(listed)) { o.err(`${tag} cannot list the files of ${snapshot ?? "the working tree"}: ${why(listed)}`); return 2; }
    const files = text(listed).split("\0").filter(Boolean).map(posix);
    const tracked = (name) => files.filter((f) => f === name || f.endsWith(`/${name}`));
    const readSnap = (f) => o.readFile(f);

    let rust; let npm; let npmRoots = []; let installRoots = []; let manifests = []; let closure = null; let closureTools = null;
    try {
      manifests = tracked("Cargo.toml");
      if (executor.kind === "refuse" && manifests.length) { o.err(`${tag} BLOCKED: ${executor.problem}`); return 1; }
      // ci/gate.config.json `fixtures` (optional): directories whose manifests are consumer or sample crates, gated through the
      // consumer rows of the crates they exercise (CI treats them the same), never as units.
      let fixtures = [];
      try { const f = JSON.parse(o.readFile(path.join(repo, GATE_CONFIG))).fixtures; if (Array.isArray(f)) fixtures = f.filter((d) => typeof d === "string"); } catch { /* no config: no fixtures */ }
      // Selected publisher consumer workspaces must participate in discovery, scoped Class A and target binding.
      if (o[UNIT_LIST_UNION]?.consumerDirs?.length) fixtures = fixtures.filter((d) => !o[UNIT_LIST_UNION].consumerDirs.some((c) => c === d.replace(/\/$/, "") || c.startsWith(`${d.replace(/\/$/, "")}/`)));
      // A snapshot has nothing untracked; the working tree (--plan, check --worktree) names its untracked manifests as skipped.
      const untracked = () => {
        if (snapshot) return [];
        const r = git("ls-files", "-z", "--others", "--exclude-standard", "--", ":(glob)**/Cargo.toml");
        return failed(r) ? [] : text(r).split("\0").filter(Boolean).map(posix);
      };
      if (executor.kind === "broker" && snapshot && (snapshotA || snapshotB) && manifests.length) {
        const { createSnapshotBroker } = await import("../snapshot-broker.mjs");
        const snapshotCheck = await import("../snapshot-check.mjs");
        const snapshotCommitOf = () => snapshotCheck.commitOf(fs.realpathSync.native(root), String(snapshot), o.spawn);
        // The broker is built before anything is read: a missing enrollment, ownership or client refuses here, naming its code (as the compile does).
        try {
          if (o.snapshotBroker && typeof o.snapshotBroker.create !== "function") discoveryBroker = o.snapshotBroker; // a fixture broker (tests)
          else {
            const sc = snapshotCommitOf();
            discoveryBroker = createSnapshotBroker({ env: o.env, platform: o.platform, checkout: root, commit: { sha: sc.commit, tree: sc.tree }, snapshot: { path: repo, expectedTree: sc.tree }, ownership: o.buildOwnership, client: executor.route === LINUX_BROKER_ROUTE ? executor.client : which("rustright").find((c) => path.isAbsolute(c)), gateFile: callingGatePath(), spawn: o.spawn });
          }
        } catch (error) { o.err(`${tag} BLOCKED: ${error.code ?? "RK_SNAPSHOT_BROKER"}: ${error.message}; nothing compiled`); return 1; }
        // GATE-CONTRACT GC-A11 (Adrian, 2026-11-02): Class A never runs in the broker. The snapshot's discovery metadata and every Class A Cargo
        // read (metadata, fetch, deny, audit, fmt --check) run DIRECTLY with the host's real toolchain cargo (RustRight shims and outer gate
        // wrappers taken off PATH), each into its own scratch target under the gate's tmp root; nothing is carried.
        const { directReadSpawn } = await import("../snapshot-metadata.mjs");
        const pathKey = o.platform === "win32" ? (Object.keys(o.env).find((k) => k.toUpperCase() === "PATH") ?? "Path") : "PATH";
        const direct = directCargoPath({ env: { ...o.env, PATH: o.env[pathKey] }, exists: o.exists, platform: o.platform, gateWrappers: true });
        const readOpts = { cargoCommand: planCargoCommand, snapshotRoot: repo, directPath: direct.path, pathKey, tmpRoot: gateTmpRoot({ env: o.env, platform: o.platform, exists: o.exists }), env: o.env, err: o.err };
        // One memo for the run (rightkit#1460): the unit discovery's `metadata --no-deps --locked` and the Class A `cargo-metadata` scan of the same
        // workspace are the same call on the same verified snapshot, so the scan reuses the discovery's result instead of running Cargo again.
        const readMemo = new Map();
        snapshotSpawn = directReadSpawn({ spawn: withDeclaredClient((c, a, opts) => o.spawn(c, a, opts)), ...readOpts, memo: readMemo });
        scanSpawn = directReadSpawn({ spawn: withDeclaredClient(defaultSpawnAsync), ...readOpts, memo: readMemo, isAsync: true });
      }
      npm = discoverNpmUnits({ root: repo, tracked, readFile: readSnap, exists: o.exists });
      npmRoots = discoverPnpmRoots({ root: repo, tracked, exists: o.exists });
      let metadata = null;
      if (scopeDirs.length) {
        closureTools = await import("../app-closure.mjs");
        const readCommitted = (file) => {
          if (!files.includes(file)) throw new Error(`${file} is absent from the committed tree`);
          return readSnap(path.join(repo, file));
        };
        const sourceMetadata = closureTools.cargoMetadataFromManifests({ manifests: manifests.filter((m) => !fixtures.some((d) => m.startsWith(`${d.replace(/\/$/, "")}/`))), readFile: readCommitted });
        const inventory = discoverRustWorkspaces({ root: repo, tracked, fixtures, readFile: readSnap, metadata: sourceMetadata });
        closure = closureTools.closureOf({ scopeDirs, npmUnits: npm, rustUnits: inventory.units, readFile: readCommitted });
        const selectedWorkspaces = new Set(inventory.units.filter((u) => closure.rust.has(u.name)).map((u) => u.cargoDir));
        metadata = (dir) => {
          const source = sourceMetadata(dir);
          return selectedWorkspaces.has(source.cargoDir) ? workspaceMetadata({ root: repo, spawn: snapshotSpawn, cargoCommand: planCargoCommand, dir, realpath: fs.realpathSync }) : source;
        };
      }
      rust = discoverRustWorkspaces({ root: repo, spawn: snapshotSpawn, cargoCommand: planCargoCommand, tracked, untracked, fixtures, readFile: readSnap, ...(metadata ? { metadata } : {}) });
      if (closure) {
        // Confirm membership discovered by Cargo; its closure must resolve without unrelated execution.
        closure = closureTools.closureOf({ scopeDirs, npmUnits: npm, rustUnits: rust.units, readFile: (file) => { if (!files.includes(file)) throw new Error(`${file} is absent from the committed tree`); return readSnap(path.join(repo, file)); } });
        installRoots = [...new Set(npm.filter((u) => closure.npm.has(u.name)).map((u) => u.root ?? "."))].sort();
      } else installRoots = npmRoots;
    } catch (e) { o.err(`${tag} BLOCKED: cannot discover the repository's units: ${e.message}`); return 1; }
    for (const k of rust.skipped) o.out(`${tag} note: ${k.manifest} is skipped, not scanned: ${k.why}`);
    // Every repository but RightKit commits one fixed build definition that lists every Cargo workspace; a run builds all of them and nothing is
    // chosen per run (#1325, Adrian 2026-10-31). The workspaces this commit holds and the committed list must be the same set, missing or extra,
    // or nothing runs. RightKit is judged per unit instead (rightkitDoorRule). `--plan` builds nothing, so it only says what a run would meet.
    {
      let rightkitHere = false;
      if (files.includes(RIGHTKIT_IDENTITY_FILE)) { try { rightkitHere = isRightKitIdentity(readSnap(path.join(repo, ...RIGHTKIT_IDENTITY_FILE.split("/")))); } catch { rightkitHere = false; } }
      if (!rightkitHere) {
        try { checkCargoWorkspaceList({ discovered: rust.workspaces.map((w) => w.cargoDir), record: files.includes(CARGO_WORKSPACE_LIST_FILE) ? readSnap(path.join(repo, CARGO_WORKSPACE_LIST_FILE)) : null }); }
        catch (e) {
          if (!e.code?.startsWith("RK_CARGO_WORKSPACE_LIST_")) throw e;
          if (args.includes("--plan")) o.out(`${tag} note: a run would be refused: ${e.message}`);
          else { o.err(`${tag} BLOCKED: ${e.message}; nothing ran`); return 1; }
        }
      }
    }

    // The snapshot compile: verified as the commit's tree before anything runs, dependencies installed into it offline from the
    // checkout's store, and every Cargo workspace's output pinned to the target directory cargo resolves for it IN THE CHECKOUT
    // (CARGO_TARGET_DIR or build.target-dir honoured), so the warm outputs are the checkout's and the snapshot holds no target/.
    let targetDirs = null; let cargoShim = null; let snapshotBroker = null;
    // The direct (Linux) compile's evidence (item 7 rows 7.09, 7.18, 7.19): how each output directory was resolved, the declared inputs
    // digested in the snapshot, and the stage's own timings.
    let outputResolution = {}; let directDeclared = []; let directDeclaredNotes = [];
    const directTiming = { bindStart: 0, bindEnd: 0, cargoStart: 0, cargoEnd: 0, afterStart: 0, afterEnd: 0, diagnostics: 0 };
    // The wrapper and each workspace's output directory, resolved before ANY scan runs (rightkit#1328): Class A runs once, before the compile,
    // under this wrapper and its per-scan sentinel, so a cargo that bypasses it fails the Class A row, not a later pass. Nothing compiles here.
    // `wrapRoot` is the verified snapshot, `wrapBroker` the broker of a broker host (the compile's, or the check's discovery broker).
    const bindCargoWrapper = async (wrapRoot, wrapBroker) => {
      targetDirs = {};
      // A scoped run (--unit or a nested app) wraps its closure's workspaces (GC-A03) and, always, every workspace inside a scope
      // directory: a unit's own workspace is wrapped even when closure membership names its crates differently.
      const inScope = (ws) => scopeDirs.some((d) => { const s = d.replace(/\/+$/, ""); return ws.cargoDir === s || ws.cargoDir.startsWith(`${s}/`) || s.startsWith(`${ws.cargoDir}/`); });
      const wrapped = closure ? rust.workspaces.filter((ws) => inScope(ws) || ws.units.some((u) => closure.rust.has(u.name))) : rust.workspaces;
      for (const ws of wrapBroker ? [] : wrapped) {
        const manifest = path.join(root, ws.cargoDir, "Cargo.toml");
        if (!o.exists(manifest)) { o.err(`${tag} BLOCKED: the checkout has no ${posix(path.relative(root, manifest))}, so the output directory of workspace ${ws.cargoDir} cannot be resolved; nothing compiled`); return 1; }
        // Resolved in the PRIMARY CHECKOUT with the stage's environment, before the source location changes (row 7.09: CARGO_TARGET_DIR,
        // a relative or absolute build.target-dir, or the default target/ all resolve as for a developer's own build); the cwd and
        // environment used are recorded. A directory inside the snapshot is refused before anything compiles (a second output).
        let resolved;
        try { resolved = resolveTargetDirectory({ spawn: (c, a, opts) => o.spawn(c, a, opts), cargoCommand, manifest, env: o.env, checkoutRoot: root, snapshotRoot: wrapRoot, workspace: ws.cargoDir }); }
        catch (error) { o.err(`${tag} BLOCKED: ${error.message}; nothing compiled`); return 1; }
        targetDirs[ws.cargoDir] = resolved.dir; outputResolution[ws.cargoDir] = resolved.inputs;
        o.out(`${tag} output directory for workspace ${ws.cargoDir}: ${targetDirs[ws.cargoDir]} (the checkout's)`);
      }
      // CARGO_HOME, sccache & temp are inherited as absolute values, never snapshot-relative (GATE-CONTRACT "Output directory & cache"):
      // a relative one would be resolved inside the snapshot, a second cache or output location. Refused before anything compiles.
      const relativeEnv = SNAPSHOT_ABSOLUTE_ENV.filter((name) => typeof o.env[name] === "string" && o.env[name] !== "" && !path.isAbsolute(o.env[name]));
      if (relativeEnv.length) { o.err(`${tag} BLOCKED: ${relativeEnv.map((n) => `${n}=${o.env[n]}`).join(", ")} ${relativeEnv.length > 1 ? "are" : "is"} relative; a snapshot compile inherits CARGO_HOME, sccache and temp only as absolute paths, never resolved inside the snapshot; nothing compiled`); return 1; }
      // The runtime cargo wrapper (item 7): first on PATH and named in CARGO for every scan, it binds each real cargo invocation to its
      // own workspace's checkout target directory or refuses it (RK_CARGO_*), whatever path reaches cargo. Direct POSIX hosts only (a
      // broker host never reaches here; a direct Windows host keeps the fail-closed static walk). Removed when the gate ends (cleanup).
      if (o.platform !== "win32" && rust.workspaces.length) {
        // GC-A11: the wrapper runs non-compiling calls directly, so a broker-mode wrapper also needs the real toolchain cargo (shims off PATH).
        const directCargoBin = () => directCargoBinOf(o);
        const realCargo = wrapBroker ? (directCargoBin() ?? undefined) : realCargoOf(which("cargo"));
        if (!realCargo && !wrapBroker) { o.err(`${tag} BLOCKED: no cargo on PATH to wrap for the snapshot compile; nothing compiled`); return 1; }
        try {
          const shimModule = await import("../cargo-target-shim.mjs");
          let broker = null;
          if (wrapBroker) {
            // Broker mode (the Mac, decision C): every cargo a test reaches is its own carried request. The gate's own snapshot-to-checkout
            // map is the only source of a request's primaryCwd; each carried request records its sub-handle in the ownership just before it is sent.
            const checkoutRoot = fs.realpathSync.native(root);
            const checkoutDirs = Object.fromEntries(rust.workspaces.map((ws) => [ws.cargoDir, path.join(checkoutRoot, ...(ws.cargoDir === "." ? [] : ws.cargoDir.split("/")))]));
            const { brokerSection } = await import("../snapshot-broker.mjs");
            broker = { checkoutDirs, section: brokerSection(wrapBroker) };
          }
          // Its own base directory: the wrapper's directory is named by its identity, and a unit stage's outer wrapper over the same
          // workspaces has the same identity, so a shared base would let this run's cleanup delete the wrapper the next stage runs under.
          const shimBase = fs.mkdtempSync(path.join(os.tmpdir(), "rk-gate-shim-"));
          cargoShimDir = shimBase;
          cargoShim = shimModule.installCargoShim({ base: shimBase, snapshotRoot: wrapRoot, targetDirs, broker, realCargo: realCargo ?? null, platform: o.platform });
          cargoShim.scanEnv = shimModule.shimScanEnv(o.env, cargoShim);
        } catch (error) { o.err(`${tag} BLOCKED: the runtime cargo wrapper cannot be installed (${error.message}); nothing compiled`); return 1; }
        o.out(`${tag} runtime cargo wrapper ${cargoShim.shim} (identity ${cargoShim.identity.slice(0, 16)}, ${cargoShim.mode}) -> ${cargoShim.mode === "broker" ? "the RustRight broker, one carried request per call" : realCargo}: each cargo invocation ${cargoShim.mode === "broker" ? "is carried with its own receipt, verified by the gate," : "binds its workspace's checkout target directory"} or is refused`);
      }
      return 0;
    };
    if (snapshotB) {
      const sc = await import("../snapshot-check.mjs");
      try {
        const checkout = fs.realpathSync.native(root);
        directTiming.bindStart = Date.now();
        snapshotBCommit = sc.commitOf(checkout, String(snapshot), o.spawn);
        // With a ledger the verify returns an object ({ tree, ledger }), without one the tree id: only the refusal (a throw) matters here.
        sc.verifySnapshotDir({ checkout, dir: snapshotB, ...snapshotBCommit, when: "before the compile", ...(o.snapshotLedger ? { ledger: o.snapshotLedger } : {}) });
        directTiming.bindEnd = Date.now();
        // A direct host digests the declared compile inputs in place (the snapshot's own .rightkit/compile-inputs.json, never the
        // checkout's); a malformed manifest refuses before anything compiles. The broker digests them for a broker host.
        if (executor.kind === "direct") {
          const inputsModule = await import("../compile-inputs.mjs");
          const judged = inputsModule.judgeCompileInputs(snapshotB);
          // Only a malformed manifest refuses here (the broker's request does the same); a declared path the snapshot does not hold, or a
          // finding in one, is recorded with the diagnostics and left to the check stage that judges the captured inputs.
          const malformed = judged.findings.filter((f) => f.startsWith(`${inputsModule.COMPILE_INPUTS_FILE}:`));
          if (malformed.length) throw Object.assign(new Error(`the snapshot's declared compile inputs are malformed: ${malformed.join("; ")}`), { code: "RK_COMPILE_INPUTS" });
          directDeclared = judged.inputs;
          directDeclaredNotes = judged.findings.map((f) => `declared compile input: ${f}`);
        }
        o.out(`${tag} snapshot ${snapshotB} is commit ${snapshotBCommit.commit} tree ${snapshotBCommit.tree} (verified before the compile)`);
        for (const r of sc.installSnapshotDependencies({ checkout, snapshot: snapshotB, ...snapshotBCommit, ...(o.snapshotLedger !== undefined ? { ledger: o.snapshotLedger } : {}), roots: installRoots, ...(closure ? { packageDirs: npm.filter((u) => closure.npm.has(u.name)).map((u) => u.dir) } : {}), env: o.env, spawn: o.spawn })) o.out(`${tag} ${sc.describeInstall(r)}`);
      } catch (error) { o.err(`${tag} ${error.code === "RK_CHECK_ERROR" ? "RK_CHECK_ERROR" : "BLOCKED"}: ${error.message}`); return error.code === "RK_CHECK_ERROR" ? 2 : 1; }
      // A broker host (item 7 rows 7.20 to 7.24): the compile goes through the RustRight snapshot carrier. The broker pins the output
      // directory (its managed generation target), so no checkout target is resolved here; the gate's broker is built once, with the
      // host's enrolled pins, the declared inputs read from the snapshot and the build ownership; a missing piece refuses before
      // anything compiles (an app repository with no enrollment included), never a compile around the broker.
      if (executor.kind === "broker" && manifests.length) {
        try {
          const { createSnapshotBroker } = await import("../snapshot-broker.mjs");
          const client = executor.route === LINUX_BROKER_ROUTE ? executor.client : which("rustright").find((c) => path.isAbsolute(c));
          snapshotBroker = discoveryBroker ?? createSnapshotBroker({ env: o.env, platform: o.platform, checkout: root, commit: { sha: snapshotBCommit.commit, tree: snapshotBCommit.tree }, snapshot: { path: snapshotB, expectedTree: snapshotBCommit.tree }, ownership: o.buildOwnership, client, gateFile: callingGatePath(), spawn: o.spawn });
        } catch (error) { o.err(`${tag} BLOCKED: ${error.code ?? "RK_SNAPSHOT_BROKER"}: ${error.message}; nothing compiled`); return 1; }
        o.out(`${tag} broker snapshot compile: pins ${snapshotBroker.pins.keyId.slice(0, 12)} from ${snapshotBroker.pinsSource}, ${snapshotBroker.declaredInputs.length} declared input(s) read from the snapshot, ownership ${snapshotBroker.handle}`);
      }
      { const rc = await bindCargoWrapper(snapshotB, snapshotBroker); if (rc) return rc; }
    }

    // Full class A of a snapshot (the door's check stage): the same wrapper, sentinel and ledger as the compile, installed BEFORE the scans so
    // the one Class A pass is the one that sees a cargo bypass (rightkit#1328). The compile pass runs class B only.
    if (!planOnly && fullA && snapshotA && rust.workspaces.length) {
      const rc = await bindCargoWrapper(snapshotA, executor.kind === "broker" ? discoveryBroker : null);
      if (rc) return rc;
    }

    // Class B scans (compile, lint, tests) run in the live checkout, so it must equal HEAD where they read it: an uncommitted fix could
    // otherwise hide a committed failure. Class A reads only the snapshot.
    if (!planOnly && !classAOnly && !snapshotB) {
      const npmRootFiles = ["package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml", ".npmrc", ...ESLINT_CONFIG_FILES, ...BIOME_CONFIG_FILES];
      const inputs = [...new Set([...rust.units.map((u) => u.dir), ...npm.map((u) => u.dir), ...rust.workspaces.map((w) => w.cargoDir), "ci", "scripts", "vendor",
        // The root manifest, lock and deny.toml only where a workspace is rooted there; a crate-per-workspace repository binds each crate's own
        // (its directory is an input above, which holds its Cargo.toml, Cargo.lock, deny.toml, .cargo and .rightkit).
        ...(rust.workspaces.some((w) => w.cargoDir === ".") ? ["Cargo.toml", "Cargo.lock", "deny.toml"] : []),
        ".cargo", "rust-toolchain.toml", "rust-toolchain", "clippy.toml", "rustfmt.toml", ...npmRootFiles, ...npmRoots.filter((r) => r !== ".").flatMap((r) => npmRootFiles.map((f) => `${r}/${f}`))])];
      // Batched (pathspecBatches): one argv with every input path overflows Windows' command line in a large repository.
      const dirtyRuns = pathspecBatches(inputs).map((batch) => git("status", "--porcelain", "--untracked-files=all", "--", ...batch));
      const dirtyFailed = dirtyRuns.find((r) => failed(r));
      const dirtyFiles = dirtyFailed ? [`git status failed: ${why(dirtyFailed)}`] : dirtyRuns.flatMap((r) => text(r).split(/\r?\n/)).filter(Boolean).filter((l) => !l.includes("node_modules"));
      if (dirtyFiles.length) {
        o.err(`${tag} BLOCKED: the checkout differs from HEAD where compile scans read it; commit or stash first:`);
        for (const l of dirtyFiles.slice(0, 20)) o.err(`  ${l}`);
        return 1;
      }
    }

    const diff = allFilesMode ? null : git("diff", "--name-only", "--diff-filter=ACMR", `${base}...HEAD`);
    if (diff && failed(diff)) { o.err(`${tag} cannot diff against ${base}: ${why(diff)}; pass --base <ref> or --all`); return 2; }
    const changed = allFilesMode ? files : [...new Set(text(diff).split(/\r?\n/).map((f) => f.trim()).filter(Boolean))].filter((f) => o.exists(path.join(repo, f)));
    if (allFilesMode) base = snapshot ?? "the working tree";

    let crateFeatures = {}; let packageFeatures = {}; let swiftPackages = {};
    try { const gateConfig = JSON.parse(o.readFile(path.join(repo, "ci/gate.config.json"))); crateFeatures = gateConfig.rust ?? {}; packageFeatures = gateConfig.npm ?? {}; swiftPackages = gateConfig.swift && typeof gateConfig.swift === "object" ? gateConfig.swift : {}; } catch { /* no config: default feature sets */ }
    // rightkit#1333 (P7): the end-to-end crates of the tree (a non-optional rightkit-qa dependency and a driver module), found from the files alone by the
    // same reader the G22 check rule uses; their test scans compile with --no-run and their tests are owed to the qualify stage (planScans e2eCrateDirs).
    let e2eCrateDirs = null;
    if (rust.units.length) {
      try { const { e2eCrates } = await import("../app-check.mjs"); e2eCrateDirs = new Set(e2eCrates(repo, files).crates.map((c) => c.dir)); }
      catch (e) { o.err(`${tag} BLOCKED: cannot tell which crates are end-to-end crates (${e.message}); their tests would run as class B unit tests, so nothing is planned`); return 1; }
    }
    // One lock and one lock scope per Cargo workspace. A workspace whose lock-only change reaches a subset puts only that subset's
    // units (by unit name) in scope; every other workspace keeps all of its units in scope (planScans reads the scope only for the
    // workspaces whose own root files changed).
    const rustLocks = {}; const scopeNames = new Set(); let restricted = false;
    for (const ws of rust.workspaces) {
      const lockPath = ws.cargoDir === "." ? "Cargo.lock" : `${ws.cargoDir}/Cargo.lock`;
      try { rustLocks[ws.cargoDir] = parseLock(o.readFile(path.join(repo, lockPath))); } catch { if (ws.units.length) o.out(`${tag} note: no ${lockPath}; reverse dependents are not checked`); }
      const nameOf = new Map(ws.units.map((u) => [u.crate, u.name]));
      const scope = ws.units.length ? lockScope({
        changed, cargoDir: ws.cargoDir, unitNames: new Set(nameOf.keys()),
        showBase: (f) => { const r = git("show", `${base}:${f}`); return failed(r) ? null : text(r); },
        readLock: () => o.readFile(path.join(repo, lockPath)),
        readManifest: () => o.readFile(path.join(repo, ws.cargoDir === "." ? "Cargo.toml" : `${ws.cargoDir}/Cargo.toml`)),
      }) : null;
      if (scope === null) for (const u of ws.units) scopeNames.add(u.name);
      else { restricted = true; for (const crate of scope) if (nameOf.has(crate)) scopeNames.add(nameOf.get(crate)); }
    }
    const rustScope = restricted ? scopeNames : null;

    // A broker host: the class A Cargo scans of the snapshot (metadata, fetch, cargo-deny, cargo-audit) are carried read-only requests that run IN
    // THE SNAPSHOT (scanSpawn above), so an uncommitted edit in the checkout is ignored, never refused (docs/GATE-CONTRACT.md: no dirty-tree
    // condition; class A reads the snapshot). The checkout is only where the RustRight client starts (it refuses to start anywhere else) and
    // where class B's carried compiles name their working directory; `cargoLiveRoot` is that directory, never a source.
    let cargoLiveRoot = null;
    if (executor.kind === "broker" && manifests.length && snapshot && (snapshotA || snapshotB)) cargoLiveRoot = root;
    // One Cargo workspace and lock per crate (rightkit#1285): the reach between crates is the manifests' path dependencies, read from the
    // gated snapshot. A crate that is selected selects its transitive dependents (checked, not re-linted); a vendored source it patches
    // in selects the crate. Neither is derivable from a single lock, so a layout that cannot read them is refused, never planned narrower.
    let crateDependents = null; let externalInputs = null;
    const crateUnits = rust.units.filter((u) => u.cargoDir === `crates/${u.crate}` && u.name === u.crate);
    if (crateUnits.length) {
      try {
        const { crateWorkspaces, pathDependencies, reverseDependents } = await import("../crate-workspaces.mjs");
        const io = { readFile: (f) => o.readFile(f), exists: (f) => o.exists(f), readdir: (d) => o.readdir(d) };
        const known = new Set(crateUnits.map((u) => u.name));
        const graph = new Map();
        for (const [name, set] of pathDependencies(repo, io, crateWorkspaces(repo, io))) if (known.has(name)) graph.set(name, new Set([...set].filter((d) => known.has(d))));
        crateDependents = (names) => reverseDependents([...names].filter((n) => known.has(n)), graph);
        externalInputs = vendorOwners(crateUnits, (u) => o.readFile(path.join(repo, u.dir, "Cargo.toml")));
      } catch (e) { o.err(`${tag} BLOCKED: cannot read the path dependencies between the crate workspaces (${e.message}); their reverse dependents would go unchecked, so nothing is planned`); return 1; }
    }
    let fullPlan;
    fullPlan = planScans({
      // Class A of a commit or tree reads only that snapshot: every scan, cargo's included, runs in the snapshot, never in the live
      // checkout (which may hold other bytes). Compiles (class B) use the live checkout, whose tree the dirty check pinned to HEAD.
      cargoJobs: classAOnly ? null : (o[CARGO_WIDTH] ?? null),
      changed, contentRoot: repo, liveRoot: (classAOnly && snapshot) || snapshotB ? repo : root, trustedRoot: snapshotA ?? snapshotB ?? root, includeTests: true, rustScope, cargoCommand: planCargoCommand, targetDirs, cargoShim: Boolean(cargoShim),
      tier: fullA ? "release" : (option("--rule-tier") ?? "compile"), cargoLiveRoot,
      // The quiet tests (port, pipe, desktop and wall-clock bound) are proven by the CI shards, one crate per runner, unless
      // RK_GATE_QUIET_TESTS=local asks for the serial quiet lane here (no CI, or a CI outage).
      // Full class A runs every test before a compile: the quiet suites run here in the quiet lane, never left to CI.
      // The door's snapshot compile already executes the repository's own code (its tests) in the verified snapshot, guarded like
      // full class A, so the docs generator runs there too instead of being deferred (step 4c review).
      quietTests: fullA || o.env.RK_GATE_QUIET_TESTS === "local" ? "local" : "ci", packageFeatures, liveExecution: (fullA && (args.includes("--worktree") || Boolean(snapshotA))) || Boolean(snapshotB),
      e2eCrateDirs, swiftPackages,
      rustUnits: rust.units, npmUnits: npm, npmRoots, cargoDir: ".", rustLocks, crateFeatures, crateDependents, externalInputs,
      // B1 (rightkit#1458): compile scans of one Cargo workspace run as one cargo invocation per configuration; ci/shards.json `solo` crates never join one.
      // Batching is always on (the RK_GATE_CLASS_B_BATCH switch is gone, 2026-11-08): the batch-equivalence run (GATE-CONTRACT GC-I11; Pulse 3b31b4ca, 2026-11-03) gave the same verdict on all 193 scans batched and per crate.
      classBBatching: !o[PRIVATE_SCOPED_COMPILE], shardSolo: shardSoloOf(o, repo),
      // The broker (or, on a direct host, the gate itself) admits compiles; no separate grant file decides it here.
      localCompile: true,
      exists: o.exists, readFile: o.readFile, platform: o.platform,
    });
    // A broker-managed host whose broker is unreachable runs no class B scan: npm tests and scripts can compile native code the
    // snapshot lists no manifest for (Astra, second pass); the class A scans never compile and still run.
    // Class A tests run code too (Astra, second pass: a test script can compile native code its text does not show), so a host whose
    // broker is unreachable runs neither; the gate stops rather than skip them.
    if (executor.kind === "refuse" && fullPlan.scans.some((x) => x.class === "B" || (x.kind === "test" && (!classAOnly || fullA)))) { o.err(`${tag} BLOCKED: ${executor.problem}`); return 1; }
    let plan = fullPlan;
    if (closure) {
      let kept;
      try {
        if (o[PRIVATE_SCOPED_COMPILE] && fullPlan.scans.some((scan) => scan.class === "B" && Array.isArray(scan.batch?.units))) throw new Error("scoped private compilation cannot admit pre-grouped Class B scans; use ungrouped planning");
        kept = closureTools.scopeScans(fullPlan.scans, closure, { rustUnits: rust.units, npmUnits: npm });
        plan = closureTools.scopedPlan(fullPlan, kept.scans, closure);
        if (o[PRIVATE_SCOPED_COMPILE]) {
          const { selector, unitDir } = o[PRIVATE_SCOPED_COMPILE];
          const npmUnit = npm.find((u) => u.dir === unitDir);
          const rustUnit = rust.units.find((u) => u.dir === unitDir);
          const owner = selector.startsWith("npm:") ? npmUnit?.name : rustUnit?.name;
          const names = selector.startsWith("npm:") ? [`test:npm:${owner}`] : [`check:${owner}`, `check-features:${owner}`, `clippy:${owner}`];
          if (!owner || !plan.scans.some((scan) => scan.class === "B" && names.some((prefix) => scan.id === prefix || scan.id.startsWith(`${prefix}:`)))) throw new Error(`${selector}: no unit-specific Class B compile or test scan was planned; declare an executable unit test or compile route`);
        }
      } catch (error) { o.err(`${tag} BLOCKED: ${error.message}`); return 1; }
      plan.notes = plan.notes.filter((n) => !n.includes("every Rust unit is in scope") && !n.startsWith("class B batching:"));
      const batchCandidates = plan.scans.filter((scan) => scan.class === "B" && scan.batchable);
      const batchCounts = new Map();
      for (const scan of batchCandidates) batchCounts.set(scan.batchable.key, (batchCounts.get(scan.batchable.key) ?? 0) + 1);
      const sharedGroups = [...batchCounts.values()].filter((count) => count > 1);
      if (batchCandidates.length) plan.notes.push(`class B batching: scoped plan has ${batchCandidates.length} eligible scan(s), including ${sharedGroups.reduce((sum, count) => sum + count, 0)} in ${sharedGroups.length} compatible group(s); actual batches depend on readiness and execution`);
      const classA = plan.scans.filter((s) => s.class === "A");
      // What actually executes: tests, executing rows and every Cargo scan (metadata, package list, deny, audit run cargo, through the broker
      // on a broker host), never only the rows that carry `executes`. The Cargo counts are named so a scoped run shows how much cargo it starts.
      const executing = classA.filter((s) => closureTools.isExecutingScan(s)).length;
      const cargoRun = classA.filter((s) => closureTools.isCargoScan(s)).length;
      const cargoExcluded = kept.dropped.filter((s) => closureTools.isCargoScan(s)).length;
      o.out(`${tag} scope ${closure.dirs.join(", ")}: dependency directories ${closure.dependencyDirs.join(", ")}; ${closure.npm.size} npm & ${closure.rust.size} Rust unit(s); Class A ${executing} executing scan(s) (${cargoRun} Cargo), ${classA.length - executing} repository file-only scan(s); ${kept.dropped.length} unrelated executing scan(s) excluded (${cargoExcluded} Cargo); ${plan.scans.length - classA.length} closure Class B scan(s) do not run in this check; install roots ${installRoots.join(", ") || "none"}; scope digest ${plan.scopeDigest}`);
    }
    if (o[CARGO_WIDTH] !== undefined && !classAOnly) {
      const uncontrolled = plan.scans.find((scan) => scan.class === "B" && cargoWidthScanProblem(scan, planCargoCommand, o[CARGO_WIDTH]));
      if (uncontrolled) { o.err(`${tag} BLOCKED: --cargo-jobs cannot control Class B scan ${uncontrolled.id}`); return 1; }
    }
    let unionScheduler = null;
    const unionContext = o[UNIT_LIST_UNION];
    if (unionContext && snapshotB) {
      try {
        const { publisherRequirementsForUnits, planUnitListCompile, publisherUnionScheduler, runPublisherSecretRequirement, proveWorkspaceCwd } = await import("../unit-list-compile.mjs");
        const { publisherRequirementScan } = await import("../deterministic-scans.mjs");
        const requirements = await publisherRequirementsForUnits({ selected: unionContext.selected, snapshot: repo, rustUnits: rust.units, npmUnits: npm, platform: o.platform, env: o.env });
        unionContext.expected = new Map(requirements.map((entry) => [entry.selector, entry.requirements.length]));
        if (o[CARGO_WIDTH] !== undefined && requirements.some((entry) => entry.requirements.some((r) => r.executable !== "cargo" && r.internal !== "scanCrateForSecrets"))) throw new Error("--cargo-jobs cannot control a nested non-Cargo publisher requirement");
        const external = []; const scoped = []; const owner = new Map(); const verdicts = new Map(); const initialFailures = new Set();
        const { directReadSpawn } = await import("../snapshot-metadata.mjs");
        const pathKey = o.platform === "win32" ? (Object.keys(o.env).find((k) => k.toUpperCase() === "PATH") ?? "Path") : "PATH";
        const direct = directCargoPath({ env: { ...o.env, PATH: o.env[pathKey] }, exists: o.exists, platform: o.platform, gateWrappers: true });
        const directSpawn = directReadSpawn({ spawn: withDeclaredClient((c, a, opts) => o.spawn(c, a, opts)), cargoCommand: planCargoCommand, snapshotRoot: repo, directPath: direct.path, pathKey, tmpRoot: gateTmpRoot({ env: o.env, platform: o.platform, exists: o.exists }), env: o.env, err: o.err });
        for (const entry of requirements) {
          const rows = []; verdicts.set(entry.selector, entry.verdict);
          for (const requirement of entry.requirements) {
            owner.set(requirement.id, entry.selector);
            if (requirement.internal === "scanCrateForSecrets") {
              const evidence = await runPublisherSecretRequirement({ requirement, snapshot: repo, commit: snapshotBCommit.commit, tree: snapshotBCommit.tree, verifySnapshot: o.verifyUnionSnapshot });
              unionContext.internal.push({ selector: entry.selector, id: requirement.id, coverageKey: requirement.coverageKey, status: evidence.status, evidenceHash: evidence.evidenceHash, ...(evidence.findings.length ? { findings: evidence.findings } : {}) });
              if (evidence.status !== "pass") { initialFailures.add(entry.selector); o.err(`${tag} ${entry.selector}: publisher secret scan failed (${evidence.findings.join(", ")})`); }
            } else if (requirement.executable === "cargo" && requirement.argv[0] === "fmt") {
              if (!await o.verifyUnionSnapshot()) throw new Error("snapshot changed before publisher fmt");
              const result = directSpawn(planCargoCommand[0], [...planCargoCommand.slice(1), ...requirement.argv], { cwd: path.join(repo, requirement.cwd), env: requirement.env });
              if (!await o.verifyUnionSnapshot()) throw new Error("snapshot changed after publisher fmt");
              const status = failed(result) ? "fail" : "pass";
              const evidence = { id: requirement.id, coverageKey: requirement.coverageKey, commit: snapshotBCommit.commit, tree: snapshotBCommit.tree, status };
              unionContext.internal.push({ selector: entry.selector, ...evidence, evidenceHash: createHash("sha256").update(JSON.stringify(evidence)).digest("hex") });
              if (status !== "pass") { initialFailures.add(entry.selector); o.err(`${tag} ${entry.selector}: publisher fmt failed: ${why(result)}`); }
            } else { external.push(requirement); rows.push(requirement); }
          }
          if (!rows.length) throw new Error(`${entry.selector}: no executable publisher compile requirements`);
          scoped.push({ selector: entry.selector, requirements: rows });
        }
        const normalizedCwds = new Map(); const cwdProofs = [];
        const toolchains = new Map();
        // A direct cargo that is a toolchain's own binary (…/toolchains/<name>/bin/cargo), not the rustup proxy, runs that toolchain in every directory:
        // no directory override can change it, so the proof needs no rustup (Dell 2026-11-07: `rustup show active-toolchain` hung, no answer in 30 s).
        const fixedToolchain = /[\\/]toolchains[\\/]([^\\/]+)[\\/]bin[\\/]cargo(?:\.exe)?$/i.exec(String(direct.cargo ?? ""))?.[1] ?? null;
        const toolchainOf = (dir) => {
          if (fixedToolchain) return { toolchain: fixedToolchain };
          if (!toolchains.has(dir)) {
            // rustup on the direct PATH, else the real one in CARGO_HOME/bin: on a broker host the only rustup on PATH may be the broker's managed shim,
            // which the direct PATH drops (Dell 2026-11-07: every proof read "effective toolchain unavailable (spawn-error)" and no publisher row could group).
            const sep = o.platform === "win32" ? ";" : ":"; const exe = o.platform === "win32" ? "rustup.exe" : "rustup";
            const onPath = String(direct.path ?? "").split(sep).filter(Boolean).some((d) => o.exists(path.join(d, exe)));
            const home = o.env.CARGO_HOME || path.join(o.env.USERPROFILE || o.env.HOME || "", ".cargo");
            const rustupCmd = onPath ? "rustup" : (o.exists(path.join(home, "bin", exe)) ? path.join(home, "bin", exe) : "rustup");
            const result = o.spawn(rustupCmd, ["show", "active-toolchain"], { cwd: dir, env: { ...o.env, [pathKey]: direct.path }, timeout: 20000 });
            toolchains.set(dir, failed(result) ? { error: result?.error ? "spawn-error" : `exit-${Number.isInteger(result?.status) ? result.status : "unknown"}` } : { toolchain: text(result).trim().split(/\s+/)[0] });
          }
          return toolchains.get(dir);
        };
        for (const requirement of external) if (requirement.executable === "cargo" && ((requirement.phase === "compile" && ["check", "clippy", "doc"].includes(requirement.argv[0])) || (requirement.phase === "test" && requirement.argv[0] === "test"))) {
          const proof = proveWorkspaceCwd({ requirement, snapshot: repo, checkout: root, toolchainOf, exists: o.exists });
          if (proof.normalized) normalizedCwds.set(requirement.id, proof.cwd);
          cwdProofs.push({ id: requirement.id, cwd: proof.cwd, normalized: proof.normalized, reason: proof.reason });
        }
        unionContext.cwdProofs = cwdProofs;
        // Why a publisher row could not move to its workspace root (and so cannot join a group) is evidence, printed with the plan (Dell 2026-11-07: 0 grouped jobs).
        { const by = new Map(); for (const proof of cwdProofs) { const k = proof.normalized ? "moved" : proof.reason; by.set(k, (by.get(k) ?? 0) + 1); }
          o.out(`${tag} note: publisher working-directory proofs: ${[...by].map(([k, n]) => `${n} ${k}`).join("; ") || "none"}`); }
        const jobs = planUnitListCompile(scoped, { normalizedCwds });
        // The plan's shape is evidence, printed before anything runs (brosef 2026-11-07: a speed-up from grouping must be shown on the plan): how many publisher jobs, how many are
        // grouped (more than one crate in one invocation) and their sizes, by command.
        { const grouped = jobs.filter((j) => j.batchAt !== undefined && new Set(j.members.map((m) => m.package)).size > 1); const singles = jobs.length - grouped.length;
          const byCmd = new Map(); for (const j of jobs) { const k = `${j.argv[0]}${j.batchAt !== undefined ? "*" : ""}`; byCmd.set(k, (byCmd.get(k) ?? 0) + 1); }
          o.out(`${tag} note: unit-list publisher plan: ${jobs.length} job(s) for ${jobs.reduce((n, j) => n + j.members.length, 0)} requirement(s); ${grouped.length} grouped job(s) [${grouped.map((j) => new Set(j.members.map((m) => m.package)).size).sort((x, y) => y - x).slice(0, 12).join(", ")}${grouped.length > 12 ? ", ..." : ""} crates], ${singles} single; by command ${[...byCmd].map(([k, n]) => `${k}:${n}`).join(" ")} (* = batchable)`); }
        const config = JSON.parse(o.readFile(path.join(repo, "ci/gate.config.json")));
        const adapt = (requirement) => {
          const sel = owner.get(requirement.id) ?? jobs.find((job) => job.members.some((member) => member.coverageKey === requirement.coverageKey))?.members[0]?.selector;
          const unit = sel?.replace(/^cargo:/, "");
          const quiet = sel?.startsWith("cargo:") ? config.rust?.[unit]?.quietTests === true : false;
          return publisherRequirementScan(requirement, { platform: o.platform, cargoCommand: planCargoCommand, cargoLiveRoot, targetDirs, quiet, allowIgnored: verdicts.get(sel)?.allowIgnored === true, cargoJobs: o[CARGO_WIDTH] ?? null });
        };
        for (const requirement of external) plan.scans.push(adapt(requirement));
        const dependencies = Object.fromEntries(unionContext.selected.map((unit) => [unit.selector, (unit.deps ?? []).filter((dep) => unionContext.selected.some((u) => u.selector === dep))]));
        unionScheduler = publisherUnionScheduler({ reuseProofs: o.env.RK_GATE_PUBLISHER_REUSE !== "0", concurrency: 1, originConcurrency: 8, jobs, dependencies, initialFailures, requirements: external, adapt, signal: ac.signal, onCoverage: (coverage) => { unionContext.coverage = coverage; }, verify: async ({ row, member, attempts, requirement }) => {
          const verdict = verdicts.get(member.selector);
          const proof = { commit: snapshotBCommit.commit, tree: snapshotBCommit.tree, requirement: member.coverageKey, command: row?.command ?? null, attempts, snapshotEvidence: row?.snapshotEvidence ?? null, testRun: row?.testRun ?? null, results: row?.results ?? null };
          unionContext.commands.push({ selector: member.selector, id: member.id, ...proof });
          if (!row || row.status !== "pass" || (requirement.phase === "test" && (row.testRun?.outputTruncated || (!verdict.allowMissingAssets && (row.testRun?.skipMarkers ?? 0) > 0) || (!verdict.allowIgnored && (row.results?.skipped ?? 0) > 0)))) return { ok: false };
          if (executor.kind === "broker" && requirement.executable === "cargo" && !/^[0-9a-f]{64}$/.test(row.snapshotEvidence?.receiptSha256 ?? "")) return { ok: false };
          return { ok: true, evidenceHash: createHash("sha256").update(JSON.stringify(proof)).digest("hex") };
        } });
        unionContext.owner = owner;
      } catch (error) { o.err(`${tag} BLOCKED: cannot prepare canonical unit-list compile: ${error.message}`); return 1; }
    }
    // What the plan leaves to the qualify stage (end-to-end crates, declared Swift packages): handed to the door, which records it in the compile
    // receipt and refuses to qualify without a declared run route (rightkit#1332, #1333). Never dropped silently.
    o.onQualifyOwed?.((plan.qualifyOwed ?? []).filter((x) => x.kind !== "e2e-crate" || plan.scans.some((s) => s.id === x.id)));
    // GC-A24: class A lanes default to the host's cores (measured, Mac, cold RK class A rows: 4 lanes 14.4 s, 8 lanes 7.0 s, 12 lanes 5.9 s).
    const defaultJobs = typeof os.availableParallelism === "function" ? os.availableParallelism() : os.cpus().length;
    const jobs = Math.max(1, Number(o.env.RK_GATE_JOBS ?? defaultJobs) || defaultJobs);
    // A broker host has no gate-side build count: the broker decides it. What remains is the local limit on class B processes the broker never sees.
    const brokerHost = executor.kind === "broker";
    const classBJobs = brokerHost ? LOCAL_CLASS_B_JOBS : Math.max(1, Number(o.env.RK_GATE_CLASS_B_JOBS ?? 3) || 3);
    // The broker's live state of a request (queued or running), asked through the executor's own client with an asynchronous spawn: a carried scan's run limit does not tick
    // while the broker holds it queued. No client to ask (none resolves): no reader, and a carried scan's limit simply runs, counting its queue time.
    // The client is the declared Linux route's, else the managed runtime client, else the executor's own: a broker host whose executor is the `rustright` found on PATH
    // (`["rustright", ...]`) asks through that one. No client to ask (a shim-only executor with none resolved): no reader, and the scan's limit simply runs.
    const executorClient = path.basename(String(cargoCommand[0] ?? "")).replace(/\.(exe|cmd|bat)$/i, "") === "rustright" ? (brokerClientOf(o, executor) ?? cargoCommand[0]) : null;
    const brokerClient = !brokerHost ? null : declaredClient ? "rustright" : managedRuntimeClient(o.env, o.platform, o.exists) ?? executorClient;
    const brokerLive = brokerClient ? createBrokerLive({ client: brokerClient, env: { ...o.env, ...(executor.env ?? {}) }, spawn: withDeclaredClient(defaultSpawnAsync), platform: o.platform }) : null;

    if (planOnly) {
      o.out(`${tag} plan: ${plan.scans.length} scan(s) for ${changed.length} changed file(s) vs ${base.slice(0, 12)} (reads the working tree; nothing runs)`);
      // A crate that is its own workspace is named by its unit (rust units below); only the other workspaces are listed.
      const wsList = rust.workspaces.filter((w) => !(w.units.length === 1 && w.units[0].name === w.units[0].crate && w.cargoDir === `crates/${w.units[0].crate}`)).map((w) => w.cargoDir);
      const wsNote = wsList.length === 1 ? (wsList[0] === "." ? "" : ` (workspace ${wsList[0]})`) : wsList.length > 1 ? ` (workspaces ${wsList.join(", ")})` : "";
      o.out(`${tag} rust units: ${rust.units.map((u) => `${u.name}=${u.dir}`).join(", ") || "none"}${wsNote}; npm units: ${npm.map((u) => `${u.name}=${u.dir}`).join(", ") || "none"}`);
      for (const s of plan.scans) o.out(`  [${s.class}] ${s.id} :: ${s.command}${plan.deferred.includes(s.id) ? " [deferred: no local compile grant]" : ""}`);
      for (const n of plan.notes) o.out(`${tag} note: ${n}`);
      return 0;
    }

    for (const n of plan.notes) o.out(`${tag} note: ${n}`);
    o.out(`${tag} executor: ${executor.kind}${executor.kind === "broker" ? ` (${cargoCommand.join(" ")})` : " (no broker on this host: cargo runs directly, through the same gates)"}`);
    const quietCount = plan.scans.filter((s) => s.quiet === true).length;
    o.out(`${tag} ${plan.scans.length} scan(s) for ${changed.length} changed file(s) vs ${base.slice(0, 12)}; class A scans ${jobs} at a time, ${brokerHost ? `class B: the broker queues and orders the carried scans of every component, no gate count (local test suites the broker does not see: ${classBJobs} at a time)` : `class B scans ${classBJobs} component(s) at a time`}${quietCount ? `, ${quietCount} quiet test scan(s) last under the broker's quiet lease` : ""}`);
    let signalCode = 0;
    for (const sig of ["SIGINT", "SIGTERM"]) {
      const handler = () => {
        if (ac.signal.aborted) process.exit(128 + (sig === "SIGINT" ? 2 : 15)); // a second signal: leave now
        signalCode = 128 + (sig === "SIGINT" ? 2 : 15);
        o.err(`${tag} ${sig}: stopping; running scans are being killed`);
        ac.abort();
      };
      process.on(sig, handler); signals.push([sig, handler]);
    }
    // The result cache keys on the gated commit's object ids; the stdin check above made HEAD the pushed commit.
    let cache = null;
    const nodeCheckRejects = new Map();
    let nodeCheckDiagnosticId = null; let nodeCheckDiagnosticState = "not-seen";
    const countNodeCheckRejection = (reason, scanId) => {
      if (!["setup-error", "row-shape", "runtime-path", "source-path", "shim-proof", "node-options", "node-environment", "loader-environment", "runtime-config-environment", "row-error"].includes(reason)) return;
      nodeCheckRejects.set(reason, (nodeCheckRejects.get(reason) ?? 0) + 1);
      if (scanId === nodeCheckDiagnosticId) nodeCheckDiagnosticState = `rejected:${reason}`;
    };
    // rightkit#1463: the input key each scan was looked up under, for the coverage report (gate-coverage.mjs).
    const scanKeys = new Map();
    // Why a scan got no cache key (a key builder that threw, or a cache that could not open): reported, never hidden (GC-A24).
    const keyErrors = new Map(); let cacheDisabled = null;
    const keyError = (scan, error) => keyErrors.set(scan.id, String(error?.message ?? error).slice(0, 300));
    // Full class A on the working tree (the door, and broker admission of the live checkout) caches on the LIVE bytes: every
    // tracked and untracked (not ignored) file is hashed raw, so the door's run and a repeat admission of the same bytes hit the
    // cache, and any edit misses it.
    const liveFullA = fullA && args.includes("--worktree");
    // Full class A of a snapshot binds like the live tree does: its bytes are the commit's (blob ids from the object database), its
    // environment includes the dependencies installed into it below.
    const snapFullA = fullA && Boolean(snapshotA);
    const hostedFullA = fullA && Boolean(hostedCommitA);
    const boundFullA = liveFullA || snapFullA || hostedFullA;
    const liveBase = snapFullA ? snapshotA : root;
    // The live bytes full class A judges: every tracked and untracked (not ignored) file, hashed raw. Computed whether or not the
    // cache is used; a failure to compute it blocks (the verdict must name the bytes it covers).
    const liveTree = () => {
      const listed = git("ls-files", "-z", "--cached", "--others", "--exclude-standard");
      if (failed(listed)) throw new Error(`cannot list the working tree: ${why(listed)}`);
      const files = [...new Set(text(listed).split("\0").filter(Boolean).map(posix))].filter((f) => { try { return fs.statSync(path.join(root, f)).isFile(); } catch { return false; } }).sort();
      const ids = new Map();
      for (const batch of pathspecBatches(files, 60000)) {
        const r = o.spawn("git", ["hash-object", "--no-filters", "--stdin-paths"], { cwd: root, input: `${batch.join("\n")}\n` });
        if (failed(r)) throw new Error(`git hash-object failed: ${why(r)}`);
        const out = text(r).split(/\r?\n/).filter(Boolean);
        if (out.length !== batch.length) throw new Error("git hash-object returned a short list");
        batch.forEach((f, i) => ids.set(f, out[i]));
      }
      const digestOf = (list) => createHash("sha256").update(list.map((f) => `${f}\t${ids.get(f)}\n`).join("")).digest("hex");
      return { files, ids, digestOf, digest: digestOf(files) };
    };
    let live = null;
    if (liveFullA) {
      try { live = liveTree(); } catch (error) { o.err(`${tag} BLOCKED: cannot bind full class A to the working tree's bytes (${error.message})`); return 1; }
    }
    if (hostedFullA) {
      try {
        await verifyCommittedCheckout({ git, root, ...hostedCommitA, when: "before class A" });
        live = (await import("../snapshot-check.mjs")).commitFiles(root, hostedCommitA.commit, o.spawn);
      } catch (error) { o.err(`${tag} RK_CHECK_ERROR: ${error.message}; nothing scanned`); return 2; }
    }
    let snapshotCommitId = null;
    if (snapFullA) {
      const sc = await import("../snapshot-check.mjs");
      try {
        const checkout = fs.realpathSync.native(root);
        const ct = sc.commitOf(checkout, String(snapshot), o.spawn);
        snapshotCommitId = ct;
        sc.verifySnapshotDir({ checkout, dir: snapshotA, ...ct, when: "before class A" });
        live = sc.commitFiles(checkout, ct.commit, o.spawn);
        // Dependencies for the checks that execute repository code: installed into the snapshot from the commit's lockfile and the
        // primary checkout's existing store (coordinator decision A, rightkit#1257). A store miss stops here, naming the package.
        const installed = sc.installSnapshotDependencies({ checkout, snapshot: snapshotA, ...ct, roots: installRoots, ...(closure ? { packageDirs: npm.filter((u) => closure.npm.has(u.name)).map((u) => u.dir) } : {}), env: o.env, spawn: o.spawn });
        for (const r of installed) o.out(`${tag} ${sc.describeInstall(r)}`);
      } catch (error) { o.err(`${tag} ${error.code === "RK_CHECK_ERROR" ? "RK_CHECK_ERROR" : "BLOCKED"}: ${error.message}`); return error.code === "RK_CHECK_ERROR" ? 2 : 1; }
    } else if (classAOnly && snapshotA && !planOnly && npmRoots.length) {
      // A content check (no --full-a) installs nothing, so a package the offline store lacks in one of the commit's pnpm roots showed only at the
      // compile stage's install (ViewRight workers/waitlist, workerd, rightkit#1488). Every root's lockfile is proved against the store here, with no
      // install: `pnpm fetch --offline` into a scratch virtual store, the snapshot verified unchanged after.
      const sc = await import("../snapshot-check.mjs");
      try {
        const checkout = fs.realpathSync.native(root);
        const ct = sc.commitOf(checkout, String(snapshot), o.spawn);
        for (const r of sc.verifySnapshotStore({ checkout, snapshot: snapshotA, ...ct, roots: npmRoots, tmpRoot: gateTmpRoot({ env: o.env, platform: o.platform, exists: o.exists }), env: o.env, spawn: o.spawn })) o.out(`${tag} ${sc.describeStoreCheck(r)}`);
      } catch (error) { o.err(`${tag} ${error.code === "RK_CHECK_ERROR" ? "RK_CHECK_ERROR" : "BLOCKED"}: ${error.message}`); return error.code === "RK_CHECK_ERROR" ? 2 : 1; }
    }
    const liveSnapshot = live?.digest ?? null;
    // The environment, installed dependencies included, is bound at the start and compared at the end, so a dependency that
    // changes while class A runs voids the verdict.
    // envStartWithCaches records, for audit, the tool caches as the run found them (what its checks could read); the pass line
    // carries it next to the env the verdict binds.
    let envAtStart = null; let envStartWithCaches = null; let cachesAtStart = null;
    if (boundFullA) {
      const orphans = orphanedPnpmStores({ root: liveBase, files: live.files });
      if (orphans.length) { o.err(`${tag} BLOCKED: orphaned nested pnpm store(s) ${orphans.join(", ")}: left by a standalone install in a workspace member; they can hold stale dependencies and make concurrent vitest runs clear their shared module cache under each other. Move them aside (or reinstall) and run again.`); return 1; }
      // ci/gate.config.json `fixtures`: their package.json pins are evidence content, skipped here as every other scan skips them (rightkit#1468).
      let pinFixtures = [];
      try { const f = JSON.parse(o.readFile(path.join(liveBase, GATE_CONFIG))).fixtures; if (Array.isArray(f)) pinFixtures = f.filter((d) => typeof d === "string"); } catch { /* no config: no fixtures */ }
      const pinProblem = pnpmPinProblem({ root: liveBase, files: live.files, spawn: o.spawn, readFile: o.readFile, fixtures: pinFixtures, platform: o.platform });
      if (pinProblem) { o.err(`${tag} BLOCKED: ${pinProblem.message}; no class A row ran`); return 1; }
      try { envAtStart = classAEnvironmentDigest({ root: liveBase, gatePackage: o.gatePackage, files: live.files, spawn: o.spawn, includeCaches: false }); envStartWithCaches = classAEnvironmentDigest({ root: liveBase, gatePackage: o.gatePackage, files: live.files, spawn: o.spawn }); cachesAtStart = toolCacheManifest({ root: liveBase, files: live.files }); }
      catch (error) { o.err(`${tag} BLOCKED: ${error.message}`); return 1; }
    }
    if (boundFullA) {
      try {
        const common = git("rev-parse", "--git-common-dir");
        if (failed(common)) throw new Error("no git directory");
        const { files, ids, digestOf } = live;
        const under = (dir) => (dir === "." || dir === "" ? files : files.filter((f) => f === dir || f.startsWith(`${dir}/`)));
        const objectId = (p) => (ids.has(p) ? ids.get(p) : under(p).length ? digestOf(under(p)) : null);
        const store = fileScanCache(o.env.RK_GATE_CACHE_DIR ? path.resolve(o.env.RK_GATE_CACHE_DIR) : path.join(path.resolve(root, text(common).trim()), "rightkit-gate-cache"));
        const listFiles = (dir) => under(dir);
        const readBlob = (p) => fs.readFileSync(path.join(liveBase, p), "utf8");
        // The verdict also depends on what is not in the tree: the gate's own code and advisory data, the node and pnpm that run
        // the checks, the installed dependencies (pnpm's install state) and the platform. Any change there is a cache miss.
        // Tool caches (vitest, tsc build info, ...) are left out: the scans write them themselves, so keying on them made every rerun miss.
        // A6: the digest bound at the start of this run is this very computation (same root, gate package, files and tool-cache setting), so it is
        // reused: each call walks every installed dependency file, and the run made three of these walks before its first scan.
        const environment = envAtStart ?? classAEnvironmentDigest({ root: liveBase, gatePackage: o.gatePackage, files, spawn: o.spawn, includeCaches: false });
        const digest = buildCacheDigest({ objectId, headTree: digestOf(files), rustUnits: rust.units, npmUnits: npm, cargoDir: ".", rustLocks, crateFeatures, listFiles, readBlob, onKeyError: keyError });
        // A row's key is its own inputs plus the environment; the run's scope and scan set are not inputs of one row, and keying on them
        // missed every row whenever one file joined or left the plan. build-helpers is never reused: it reads the host's trace list,
        // which is outside every key (and takes seconds).
        // A row field that shapes what the scan checks without changing its id or command is part of its key: a tsc row's tscWorkspace
        // (the sibling packages its check project maps; scopeScans narrows it per scope) changes the verdict on the same tree.
        const rowShape = (scan) => (scan.tscWorkspace ? `:tscws=${createHash("sha256").update(canonicalJson([...scan.tscWorkspace].sort())).digest("hex")}` : "");
        nodeCheckDiagnosticId = plan.scans.filter((scan) => scan.class === "A" && scan.id.startsWith("node-check:")).map((scan) => scan.id).sort()[0];
        const nodeCheckModule = null; // GC-A24: node-check rows use the generic file key (file, ancestor package.json, node identity)
        const nodeCheckDigest = nodeCheckModule ? nodeCheckModule.createNodeCheckCacheDigest({ snapshot: liveBase, tree: digestOf(files), files, gateIdentity: gateCodeIdentity(), gatePackage: o.gatePackage, nodePath: process.execPath, onReject: countNodeCheckRejection, ...(o.env.RIGHTKIT_PREPARATION_DIAGNOSTIC_DIR && nodeCheckDiagnosticId ? { onDiagnostic: (record) => { nodeCheckDiagnosticState = "eligible"; nodeCheckModule.writeNodeCheckDiagnostic(o.env.RIGHTKIT_PREPARATION_DIAGNOSTIC_DIR, record); }, diagnosticScanId: nodeCheckDiagnosticId } : {}) }) : null;
        // entryNow: a path's content id in this run's tree, for carryOverProof (a pass stored under an earlier key form is carried when its files are unchanged).
        cache = { ...store, narrowNodeCheck: false, entryNow: (p) => objectId(p), digest: (scan, context) => {
          if (/^build-helpers(:|$)/.test(scan.id)) return null;
          // This narrow key applies only to the helper's proven plain snapshot node --check.
          // Uncertain rows miss rather than falling back to a less specific key.
          const key = digest(scan); const full = key === null ? null : `${key}:${classAEnvironmentBound(scan) ? environment : CLASS_A_KEY_SCHEMA}${rowShape(scan)}`; scanKeys.set(scan.id, full); return full;
        } };
      } catch (error) { cache = null; cacheDisabled = `class A result cache disabled: ${error?.message ?? error}`; o.err(`${tag} note: ${cacheDisabled}`); }
    }
    if (!classAOnly) {
      try {
        const headSha = snapshotBCommit?.commit ?? text(git("rev-parse", "HEAD")).trim();
        const treeId = text(git("rev-parse", `${headSha}^{tree}`)).trim();
        const common = git("rev-parse", "--git-common-dir");
        if (headSha && treeId && !failed(common) && text(common).trim()) {
          const objectId = (p) => {
            const r = git("ls-tree", headSha, "--", p);
            if (failed(r)) throw new Error(`git ls-tree failed: ${why(r)}`);
            const line = text(r).split(/\r?\n/).find(Boolean);
            return line ? (line.split("\t")[0].split(/\s+/)[2] ?? null) : null;
          };
          const store = fileScanCache(o.env.RK_GATE_CACHE_DIR ? path.resolve(o.env.RK_GATE_CACHE_DIR) : path.join(path.resolve(root, text(common).trim()), "rightkit-gate-cache"));
          const listFiles = (dir) => {
            const r = git("ls-tree", "-r", "--name-only", "-z", headSha, "--", dir);
            if (failed(r)) throw new Error(`git ls-tree failed: ${why(r)}`);
            return text(r).split("\0").filter(Boolean).map(posix);
          };
          const readBlob = (p) => {
            const r = git("show", `${headSha}:${p}`);
            if (failed(r)) throw new Error(`git show failed: ${why(r)}`);
            return text(r);
          };
          // The class B key identity (key schema and carrier code, classBKeyIdentity) is part of every class B key.
          const rawRowDigest = buildCacheDigest({ objectId, headTree: treeId, rustUnits: rust.units, npmUnits: npm, cargoDir: ".", rustLocks, crateFeatures, listFiles, readBlob, onKeyError: keyError });
          // A row's input digest reads only the selected commit's tree (git ls-tree/show at headSha), so one answer per tree and row serves the lookup pass and
          // every per-crate probe of the run (Mac 2026-11-07, brosef: the broker sat idle ~22 s after each grouped job while the next groups' members were
          // re-keyed one at a time on the gate's thread; 103 s of a 422 s run). Keyed by the tree and the row's whole scan identity; a null answer is not kept.
          const rowDigest = (scan) => {
            const k = `${treeId}\0${canonicalJson({ id: scan.id, args: scan.args ?? null, env: scan.env ?? null, cwd: scan.cwd ?? null, root: scan.root ?? null, kind: scan.kind ?? null, coverageKey: scan.coverageKey ?? null, runOutside: scan.runOutside ?? null, batch: scan.batch ?? null, command: scan.command ?? null })}`;
            if (ROW_DIGEST_MEMO.has(k)) { ROW_DIGEST_STATS.hits += 1; return ROW_DIGEST_MEMO.get(k); }
            const t0 = performance.now(); const d = rawRowDigest(scan); ROW_DIGEST_STATS.ms += performance.now() - t0; ROW_DIGEST_STATS.misses += 1;
            if (d !== null && d !== undefined) ROW_DIGEST_MEMO.set(k, d);
            return d;
          };
          const gateIdentity = classBKeyIdentity();
          cache = { ...store, digest: (scan) => { const key = rowDigest(scan); const full = key === null ? null : `${key}:${gateIdentity}`; scanKeys.set(scan.id, full); for (const member of scan.batch?.members ?? []) scanKeys.set(member, full); return full; } };
        } else {
          cacheDisabled = `class B result cache disabled: ${!headSha ? "no commit" : !treeId ? "no tree id" : `git common dir unreadable (${why(common)})`}`;
          o.err(`${tag} note: ${cacheDisabled}`);
        }
      } catch (error) { cache = null; cacheDisabled = `class B result cache disabled: ${error?.message ?? error}`; o.err(`${tag} note: ${cacheDisabled}`); }
    }
    // The scan-tool policy (RA-18 / L9-03): the analyzers class A accepts (shellcheck, actionlint, cargo-deny, cargo-audit) run only from an enrolled
    // executable. It comes from this host's enrolment record under the managed data root, else (RightKit's own checkout only) from the RUNNING
    // gate's revision of ci/rk-pipeline.json `tools` (scan-tool-enrolment.mjs), never the checked repository, the snapshot or an environment variable.
    // An unreadable or malformed record refuses the run: a scan tool is never judged against a policy that could not be read.
    let scanTools;
    try {
      scanTools = await (o.loadScanToolPolicy ?? (async (q) => (await import("../scan-tool-enrolment.mjs")).loadScanToolPolicy(q)))({
        dataRoot: resolveManagedRoots({ env: o.env, platform: o.platform }).gateRoot, platform: o.platform, gateFile: callingGatePath(), checkout: root, fromPipeline: scanToolPolicyFromPipeline, // reads the gate's own repository with real git, never the run's (possibly injected) spawn
      });
    } catch (error) { o.err(`${tag} BLOCKED: ${error.code ?? "RK_SCAN_TOOLS"}: ${error.message}; no scan ran`); return 1; }
    const enrolledTools = Object.keys(scanTools?.policy ?? {});
    if (enrolledTools.length) o.out(`${tag} scan-tool policy: ${enrolledTools.map((name) => `${name} (${scanTools.sources?.[name] ?? "enrolled"})`).join(", ")}`);
    // The direct compile's freshness census (row 7.19): the fingerprint units before the scans, and the fingerprint log of each cargo
    // process that compiles (CARGO_LOG on that process only, read from its own standard error). A broker host's census is the broker's.
    // The door's compile stage runs only after its check stage passed full class A of this same snapshot, once, before the compile and under the
    // cargo wrapper (rightkit#1328): this pass runs class B alone. Set by the door (io option), never by a command-line flag.
    const classAEarlier = Boolean(snapshotB && o.classAEarlier === true);
    if (classAEarlier) o.out(`${tag} class A is not repeated: it ran once before the compile, under the cargo wrapper, on this snapshot`);
    // The seam (reset plan section 3; gate-seam.mjs): on a broker host compiling a snapshot, every class B cargo row is one `rustright cargo` spawn in the snapshot
    // with RUSTRIGHT_GATE_COMMIT and RUSTRIGHT_CLASS_A; the Class A verdict lives under the data root, beside the snapshots. The legacy carrier is not taken.
    // The same seam is wired for the door's full class A run on the snapshot (snapshotA): that run writes the root verdict the compile (classAEarlier) reads.
    let gateSeam = null;
    const seamSnapshot = snapshotB && snapshotBCommit ? { path: snapshotB, id: snapshotBCommit, what: "class B runs as rustright cargo in" } : snapFullA && snapshotA && snapshotCommitId ? { path: snapshotA, id: snapshotCommitId, what: "class A writes the verdict for" } : null;
    if (seamSnapshot && brokerHost) {
      try {
        const snapshotPath = path.resolve(seamSnapshot.path);
        const state = JSON.parse(o.readFile(`${snapshotPath}.state.json`));
        if (state?.schema !== "rightkit.snapshot-state/1" || state.commit !== seamSnapshot.id.commit || state.tree !== seamSnapshot.id.tree || typeof state.repoKey !== "string") throw new Error(`${snapshotPath}.state.json does not describe commit ${seamSnapshot.id.commit} tree ${seamSnapshot.id.tree}`);
        gateSeam = { commit: seamSnapshot.id.commit, tree: seamSnapshot.id.tree, repoKey: state.repoKey, snapshot: snapshotPath, verdictDir: path.join(path.dirname(path.dirname(snapshotPath)), "class-a-verdicts"), outputRoots: snapshotBroker?.outputRoots ?? null };
        o.out(`${tag} seam: ${seamSnapshot.what} ${snapshotPath}, commit ${seamSnapshot.id.commit.slice(0, 12)}; Class A verdicts under ${gateSeam.verdictDir}`);
      } catch (error) { o.err(`${tag} BLOCKED: RK_SEAM: ${error.message}; nothing ran`); return 1; }
    }
    const census = snapshotB && !snapshotBroker ? createFreshnessCensus({ targetDirs }) : null;
    directTiming.cargoStart = Date.now();
    census?.start();
    const scanOptions = {
      // The commit's own files (rightkit#1393): a snapshot is not a git toplevel, so the secret scan needs this list to judge tracked files only and never what a stage wrote there.
      ...(snapshot ? { trackedFiles: files } : {}),
      classes: classAOnly ? ["A"] : classAEarlier ? ["B"] : ["A", "B"], ...(classAEarlier ? { classAEarlier: true } : {}), jobs, classBJobs, classBBatching: !o[PRIVATE_SCOPED_COMPILE], groupTests: true, classBPipeline: 2, ...(brokerHost ? { brokerHost: true } : {}), ...(brokerLive ? { brokerLive } : {}), signal: ac.signal, ...(cache ? { cache } : {}),
      // NN41: the installed broker runtime's identity keys every cached compile failure; null (a direct host, an unreadable manifest) means no failure is cached.
      ...(brokerHost && cache ? (() => { const identity = brokerRuntimeIdentityText({ clients: [brokerClient, executor?.client, cargoCommand[0]], env: { ...o.env, ...(executor.env ?? {}) }, platform: o.platform }); return identity ? { failureIdentity: identity } : {}; })() : {}),
      ...(census ? { spawn: census.wrapSpawn(defaultSpawnAsync) } : {}),
      // The declared Linux broker route: the scans are planned with the logical `rustright`, which this spawn (and scanSpawn's pass-through below) starts as the
      // executor's validated client, never a PATH lookup (withDeclaredClient).
      ...(declaredClient && !scanSpawn ? { spawn: withDeclaredClient(census ? census.wrapSpawn(defaultSpawnAsync) : defaultSpawnAsync) } : {}),
      ...(gateSeam ? { gateSeam } : {}),
      toolPolicy: scanTools?.policy ?? {},
      // The class A cargo reads (deny, audit, metadata, fetch) are carried requests the broker executes (scanSpawn): cargo-deny and cargo-audit are then the
      // broker's registered tools, in the check stage too (which has no snapshotBroker). Only when the reads really go to the broker.
      // GC-A11: never carried; scanSpawn runs the class A cargo reads directly, and cargo-deny and cargo-audit are the gate's enrolled tools.
      // A broker host: every Cargo call a class A scan makes inside the snapshot is a carried read-only request; the gate's own temporary files
      // (cargo-deny's metadata and config) live in a gate-owned directory the broker can read, outside the checkout.
      ...(scanSpawn ? { spawn: scanSpawn, tmpRoot: gateTmpRoot({ env: o.env, platform: o.platform, exists: o.exists }) } : {}),
      // A host with no broker runs its Class A Cargo scans itself, in the snapshot: cargo would write target/.rustc_info.json beside each manifest
      // it reads and the snapshot would no longer be the commit's tree. Each such scan gets an output directory of its own under the gate's
      // scratch root, removed when it ends (deterministic-scans.mjs cargoReadTargetDir). A broker host carries these reads and the broker owns the output.
      ...(!scanSpawn ? { cargoReadTargetRoot: gateTmpRoot({ env: o.env, platform: o.platform, exists: o.exists }) } : {}),
      // The runtime cargo wrapper is first on PATH and named in CARGO for every scan of a snapshot compile; CARGO (a path named by the
      // wrapper's identity) is in every scan's cache key, so no result cached without the wrapper or with another map is reused.
      ...(cargoShim ? { env: cargoShim.scanEnv, nodeCheckShimContext: { shim: cargoShim.shim, bin: cargoShim.bin, sentinel: cargoShim.sentinel, originalEnv: o.env }, ...(cargoShim.sentinel ? { cargoSentinel: cargoShim.sentinel } : {}), cargoLedger: { dir: cargoShim.ledgerDir } } : {}),
      // The broker snapshot compile's seam (item 7 rows 7.20 to 7.24): the commit, snapshot, ownership, enrolled pins and receipt reader.
      ...(snapshotBroker ? { snapshotBroker } : {}),
      // `check` (and the broker's compile admission, which runs it on a captured copy of the tree without node_modules) judges the
      // content: it never executes the repository's own code. The class A checks that do (tests, typecheck, lint, generated docs:
      // `executes`) run in the full gate (--all, the push hook, the door's compile stage), after the content checks and before any compile.
      ...(classAOnly && !fullA ? { exclude: (s) => s.executes === true } : {}),
      // A snapshot check names its commit; it never reads the checkout's HEAD.
      quietLane: quietLeaseLane(o, { tag, executor, reason: `rightkit-gate-quiet-tests@${snapshotCommitId ? snapshotCommitId.commit.slice(0, 12) : snapshotBCommit ? snapshotBCommit.commit.slice(0, 12) : text(git("rev-parse", "--short", "HEAD")).trim()}` }),
      // On a broker-managed host the quiet suites need the lease or they are refused (never run unleashed).
      requireQuietLease: executor.kind === "broker",
      // Failing scans' complete output: never inside the repository, on the host's scratch root (RIGHTKIT_SCRATCH_ROOT, else the
      // managed data root's runs/), in a folder unique to this run so evidence is never overwritten.
      scanLogDir: (() => {
        try {
          const base = gateRunsRoot({ env: o.env, platform: o.platform });
          const stamp = new Date().toISOString().replace(/[:.]/g, "-");
          return path.join(base, "rightkit-gate-scan-logs", `${path.basename(root)}-${stamp}-${process.pid}`);
        } catch { return null; }
      })(),
      ...(unionScheduler ? { scheduleClassB: unionScheduler } : {}),
      onResult: (row, p) => o.out(`${tag} [${p.done}/${p.total}] ${(row.cached ? (row.status === "pass" ? "cached" : "cached-fail") : row.status).padEnd(11)} ${row.id} (${p.seconds}s)${row.batchedWith ? ` [batch of ${row.batchedWith.units.length}]` : ""}${row.status === "fail" ? ` :: ${String(row.detail ?? "").slice(0, 300)}` : row.advice ? ` :: ${String(row.advice).slice(0, 300)}` : ""}`),
    };
    // Adrian 2026-11-06: "every time should be warm". Before anything runs, every planned row is looked up in the cache. If any would miss, the
    // run stops with a miss report (each row, its cause and the evidence: changed inputs, tool or environment, no stored result, or nothing
    // changed at all). Every miss is explained in the report and the rows run; a miss where nothing changed is a cache bug to file with the
    // report, never a reason to stop the run or to ask Adrian (2026-11-08: explained invalidation replaces approval).
    if (!o.runScans && cache) {
      const lookup = await runScans(plan, { ...scanOptions, cacheLookupOnly: true, onResult: () => {} });
      const allMisses = lookup.scans.filter((r) => r.lookupMiss);
      // Adrian 2026-11-06: "when we change something, we know exactly which rows are going to be called. Anything other than that needs approval."
      // A row that misses ONLY because files in its own key changed (new content ids, no tool, toolchain, broker or environment change) is one this
      // change must call: it runs, listed. Every other miss (nothing changed, no stored result, a tool or environment change) is reported as unexpected, with its cause.
      const expected = (r) => r.cacheMiss?.kind === "changed" && (r.cacheMiss.why?.inputsChanged ?? 0) > 0 && r.cacheMiss.why.contentOnly === true && !r.cacheMiss.why.tool && !r.cacheMiss.why.env && !r.cacheMiss.why.inheritedEnv;
      const called = allMisses.filter(expected);
      if (called.length) o.out(`${tag} cache lookup: ${called.length} row(s) rerun because their own files changed: ${called.slice(0, 12).map((r) => `${r.id} (${(r.cacheMiss.why.inputs ?? []).slice(0, 2).map((k) => k.replace(/^[^/]*\//, "")).join(", ")})`).join("; ")}${called.length > 12 ? "; ..." : ""}`);
      const misses = allMisses.filter((r) => !expected(r));
      if (misses.length) {
        const prior = new Map(lookup.scans.map((r) => [r.id, r]));
        const kindOf = (r) => r.cacheMiss?.kind ?? "new";
        const entries = misses.map((r) => ({ id: r.id, cause: kindOf(r) === "unchanged" ? "nothing changed: the cache failed" : kindOf(r) === "new" ? "no stored result (a new row, or the cache is empty)" : "inputs or environment changed", ...(r.cacheMiss?.why ? { evidence: r.cacheMiss.why } : {}), ...(r.cacheMiss?.last ? { lastPass: r.cacheMiss.last } : {}), ...(r.cacheMiss?.now ? { now: r.cacheMiss.now } : {}) })).sort((a, b) => a.id.localeCompare(b.id));
        const byCause = entries.reduce((m, e) => ((m[e.cause] = (m[e.cause] ?? 0) + 1), m), {});
        const changedInputs = {}; for (const e of entries) for (const k of e.evidence?.inputs ?? []) changedInputs[k] = (changedInputs[k] ?? 0) + 1;
        const tools = [...new Set(entries.filter((e) => e.evidence?.tool).map((e) => `${e.evidence.tool.from} -> ${e.evidence.tool.to}`))];
        const envs = [...new Set(entries.flatMap((e) => e.evidence?.env ?? []))];
        const commitOfRun = snapshotBCommit?.commit ?? snapshotCommitId?.commit ?? null;
        const body = { schema: "rightkit.gate-miss-report/1", commit: commitOfRun, planned: lookup.scans.length, hit: lookup.scans.filter((r) => r.cached).length, uncacheable: lookup.scans.filter((r) => r.uncacheable).length, misses: entries.length, byCause, topChangedInputs: Object.entries(changedInputs).sort((a, b) => b[1] - a[1]).slice(0, 20), tools, env: envs, rows: entries };
        // The digest names WHAT would run and WHY (commit, row ids, causes), never evidence noise, so the same misses on a rerun match the same approval.
        const digest = createHash("sha256").update(JSON.stringify({ schema: body.schema, commit: body.commit, rows: entries.map((e) => [e.id, e.cause]) })).digest("hex");
        const reportFile = path.join(gateRunsRoot({ env: o.env, platform: o.platform }), "rightkit-gate-misses", `${path.basename(root)}-${new Date().toISOString().replace(/[:.]/g, "-")}-${digest.slice(0, 12)}.json`);
        try { fs.mkdirSync(path.dirname(reportFile), { recursive: true }); fs.writeFileSync(reportFile, `${JSON.stringify({ ...body, digest }, null, 2)}\n`); } catch { /* the summary below still names everything */ }
        o.out(`${tag} cache lookup: ${body.planned} planned, ${body.hit} hit, ${body.misses} unexpected miss(es) (${Object.entries(byCause).map(([c, n]) => `${n} ${c}`).join("; ")}), ${body.uncacheable} uncacheable by design; report ${reportFile} (digest ${digest})`);
        if (body.topChangedInputs.length) o.out(`${tag} cache lookup: most-changed inputs ${body.topChangedInputs.slice(0, 8).map(([k, n]) => `${k} (${n})`).join(", ")}`);
        if (tools.length) o.out(`${tag} cache lookup: tool changes ${tools.slice(0, 5).join("; ")}`);
        if (envs.length) o.out(`${tag} cache lookup: environment changes ${envs.slice(0, 10).join(", ")}`);
        o.err(`${tag} RK_CACHE_MISS: ${body.misses} row(s) miss the cache without a change to their own files; they run now. Each miss is explained in the report; a miss where nothing changed is a cache bug: file it with the report`);
        void prior;
      } else o.out(`${tag} cache lookup: ${lookup.scans.length} planned, ${lookup.scans.filter((r) => r.cached).length} hit, ${called.length} rerun for changed files, 0 unexpected; ${lookup.scans.filter((r) => r.uncacheable).length} uncacheable by design`);
    }
    const scanResult = await (o.runScans ?? runScans)(plan, scanOptions);
    if (nodeCheckRejects.size) o.out(`${tag} node-check cache misses: ${[...nodeCheckRejects].sort(([a], [b]) => a.localeCompare(b)).map(([reason, count]) => `${reason}=${count}`).join(", ")}`);
    if (snapFullA && o.env.RIGHTKIT_PREPARATION_DIAGNOSTIC_DIR && nodeCheckDiagnosticId) o.out(`${tag} node-check diagnostic row: ${nodeCheckDiagnosticState}`);
    if (unionContext) {
      const scanById = new Map(plan.scans.map((scan) => [scan.id, scan]));
      unionContext.rows = scanResult.scans.filter((row) => row.class === "B").map((row) => {
        const scan = scanById.get(row.id);
        const command = scan?.tool ? [scan.tool, ...(scan.args ?? [])].join(" ") : null;
        return { id: row.id, status: row.status, ...(command ? { command } : {}), ...(row.batchCommand ? { batchCommand: row.batchCommand } : {}), ...(row.batchedWith ? { batchedWith: row.batchedWith } : {}), ...(row.batchFailure ? { batchFailure: row.batchFailure } : {}), ...(row.batchAttempts ? { batchAttempts: row.batchAttempts } : {}), ...(row.individualStatus ? { individualStatus: row.individualStatus, ...(row.individualCode ? { individualCode: row.individualCode } : {}) } : {}), ...(row.authenticatedFailure ? { authenticatedFailure: row.authenticatedFailure } : {}), ...(row.snapshotEvidence ? { snapshotEvidence: row.snapshotEvidence } : {}), ...(row.testRun ? { testRun: row.testRun } : {}), ...(row.results ? { results: row.results } : {}), ...(unionContext.owner?.has(row.id) ? { publisherUnit: unionContext.owner.get(row.id) } : { sharedOrigin: true }) };
      });
    }
    if (unionContext) unionContext.sharedClean = unionContext.rows.filter((row) => row.sharedOrigin).every((row) => row.status === "pass");
    directTiming.cargoEnd = Date.now();
    const freshness = census ? census.finish() : null;
    directTiming.diagnostics = Date.now() - directTiming.cargoEnd;
    if (ac.signal.aborted) {
      const notRun = scanResult.scans.filter((r) => r.status !== "pass" && r.status !== "fail").map((r) => r.id);
      o.err(`${tag} CANCELLED (${signalCode || "aborted"}); ${notRun.length} scan(s) not run:`);
      for (const id of notRun) o.err(`  - ${id}`);
      for (const line of scanFailures(scanResult).filter((l) => !/\(not run\)/.test(l))) o.err(`  - ${line}`);
      return signalCode || 130;
    }
    const failures = scanFailures(scanResult);
    const deferrals = scanDeferrals(scanResult);
    for (const line of deferrals) failures.push(`DEFERRED ${line}`);
    for (const note of scanResult.notes) o.out(`${tag} ${note}`);
    // rightkit#1463: the per-scan coverage report (hit / ran / skipped, with the cache key and the reason), written beside the scan logs; a cache that never hits shows here.
    try {
      const report = buildCoverageReport({ plan, scanResult, keys: scanKeys, keyErrors, cacheDisabled, classes: classAOnly ? ["A"] : classAEarlier ? ["B"] : ["A", "B"], commit: snapshotBCommit?.commit ?? snapshotCommitId?.commit ?? hostedCommitA?.commit ?? null, startedAt: new Date(directTiming.cargoStart).toISOString(), endedAt: new Date(directTiming.cargoEnd).toISOString() });
      const base = gateRunsRoot({ env: o.env, platform: o.platform });
      const file = (o.writeCoverage ?? writeCoverageReport)({ dir: path.join(base, "rightkit-gate-coverage"), name: `${path.basename(root)}-${new Date().toISOString().replace(/[:.]/g, "-")}-${process.pid}`, report });
      const otherPass = (report.scans ?? []).filter((s) => s.coverage === "skipped" && /is not part of this pass/.test(String(s.reason ?? ""))).length;
      o.out(`${tag} coverage report ${file}: ${report.totals.planned - otherPass} scan(s) in this pass (class ${classAOnly ? "A" : classAEarlier ? "B" : "A+B"}), ${report.totals.hit} hit, ${report.totals.ran} ran, ${report.totals.skipped - otherPass} skipped; ${otherPass} belong to the other pass; scans took ${seconds(directTiming.cargoEnd - directTiming.cargoStart)}`);
    } catch (error) { o.out(`${tag} note: the coverage report could not be written (${error.message})`); }
    // What a full class A verdict binds, green or red: the bytes it judged, unchanged from start to end, and the environment it judged
    // in (gate, tools, installed dependencies, tool caches), unchanged too. A verdict whose binding moved covers nothing: a green one is
    // refused and a red one names no reusable deterministic cause (rightkit#1257). Returns { env, notes } or { prefix, problem, exit }.
    const judgedBinding = async () => {
      if (hostedFullA) {
        try { await verifyCommittedCheckout({ git, root, ...hostedCommitA, when: "after class A" }); }
        catch (error) { return { prefix: "RK_CHECK_ERROR", exit: 2, problem: `${error.message}; the verdict covers no known bytes` }; }
      }
      if (liveFullA) {
        let digestNow = null;
        try { digestNow = liveTree().digest; } catch { digestNow = null; }
        if (digestNow !== liveSnapshot) return { prefix: "BLOCKED", exit: 1, problem: "the working tree changed while class A ran; the verdict does not cover the current bytes. Run again." };
      }
      if (snapFullA) {
        try { (await import("../snapshot-check.mjs")).verifySnapshotDir({ checkout: fs.realpathSync.native(root), dir: snapshotA, ...snapshotCommitId, when: "after class A" }); }
        catch (error) { return { prefix: "RK_CHECK_ERROR", exit: 2, problem: `${error.message}; the verdict covers no known bytes` }; }
      }
      if (!live) return { env: null, notes: [] };
      let envDigest; let envNoCaches;
      try { envDigest = classAEnvironmentDigest({ root: liveBase, gatePackage: o.gatePackage, files: live.files, spawn: o.spawn }); envNoCaches = classAEnvironmentDigest({ root: liveBase, gatePackage: o.gatePackage, files: live.files, spawn: o.spawn, includeCaches: false }); }
      catch (error) { return { prefix: "BLOCKED", exit: 1, problem: error.message }; }
      const notes = [];
      if (cachesAtStart !== null) {
        let drift;
        try { drift = toolCacheDrift(cachesAtStart, toolCacheManifest({ root: liveBase, files: live.files })); } catch (error) { return { prefix: "BLOCKED", exit: 1, problem: error.message }; }
        if (drift.changed.length) return { prefix: "BLOCKED", exit: 1, problem: `${drift.changed.length} tool cache file(s) that existed when class A started changed or vanished while it ran, so the checks may have read bytes the verdict cannot name: ${drift.changed.slice(0, 5).join(", ")}${drift.changed.length > 5 ? ", …" : ""}. Run again.` };
        if (drift.added.length) notes.push(`note: ${drift.added.length} tool cache file(s) appeared while class A ran (by it or another process), bound in env as they stand: ${drift.added.slice(0, 5).join(", ")}${drift.added.length > 5 ? ", …" : ""}`);
      }
      if (envAtStart !== null && envNoCaches !== envAtStart) return { prefix: "BLOCKED", exit: 1, problem: "the class A environment (gate, tools or installed dependencies) changed while class A ran; the verdict does not cover it. Run again." };
      return { env: envDigest, notes };
    };
    if (snapshotB) {
      // The compile covers the commit only if the snapshot still is its tree after every scan ran (E6).
      directTiming.afterStart = Date.now();
      try {
        // With the door's ledger the verify returns { tree, ledger: admitted } (item 5 row 5.09d, snapshot-check.mjs); the entries it admitted
        // are handed to the door, which holds them against its own walk at the stage's end (closeSnapshotStage). Without a ledger it returns the tree id.
        const verified = (await import("../snapshot-check.mjs")).verifySnapshotDir({ checkout: fs.realpathSync.native(root), dir: snapshotB, ...snapshotBCommit, when: "after the compile", ...(o.snapshotLedger ? { ledger: o.snapshotLedger, admitNew: true } : {}) });
        if (o.snapshotLedger) o.onCompileLedger?.(Array.isArray(verified?.ledger) ? verified.ledger : null);
      }
      catch (error) { o.err(`${tag} RK_CHECK_ERROR: ${error.message}; the compile covers no known bytes`); return 2; }
      directTiming.afterEnd = Date.now();
      o.out(`${tag} snapshot ${snapshotB} is still tree ${snapshotBCommit.tree} after the compile`);
      // A broker host's evidence is the broker's own signed receipts (each carried scan's row.snapshotEvidence, verified above); the
      // gate writes no receipt that says `executor: direct` for it.
      // The output directories this compile's evidence names (GB-13): the broker's signed receipts' `outputDir`, or the direct host's resolved targets.
      // With them, per output directory, the binaries this compile linked there (rustright#209 item 5). The plan has no `cargo build`: a bin is
      // linked only by a PASSED, not cached `cargo test -p <crate>` scan (test:rust:<unit>...) of a package with a selected integration test, which Cargo
      // rebuilds or proves fresh for this commit. A unit only checked, linted, documented, deferred to CI or without such a target counts for nothing.
      const unitByName = new Map(rust.units.map((u) => [u.name, u]));
      const known = new Set(unitByName.keys());
      const builtBins = {};
      // Where Cargo put them: `<dir>/debug` on a direct host, `<dir>/<triple>/debug` on a broker host, whose jobs build with --target <triple> (the receipt
      // names it, `targetTriple`). A broker row with no triple records nothing for its directory: a guessed path could vouch for a stale binary.
      const addBins = (dir, unit, profile) => {
        if (!dir || !profile || !unit?.testLinksBins || !unit.bins?.length) return;
        const prior = builtBins[dir];
        if (prior && prior.profile !== profile) { builtBins[dir] = { profile: null, bins: [] }; return; }
        builtBins[dir] = { profile, bins: [...new Set([...(prior?.bins ?? []), ...unit.bins])].sort() };
      };
      for (const r of scanResult.scans) {
        // A row answered from the scan cache compiled nothing in this run: the bytes in the target are not vouched for by it.
        if (r.status !== "pass" || r.cached === true || !/^test:rust(?:-serial)?:/.test(String(r.id))) continue;
        const unit = unitByName.get(rustUnitOfScanId(r.id, known));
        // Every compile is release for the host target (GC-A15, cross-targets.mjs oneOutput): the test executables are under <triple>/release.
        if (snapshotBroker) addBins(r.snapshotEvidence?.outputDir, unit, r.snapshotEvidence?.targetTriple ? `${r.snapshotEvidence.targetTriple}/release` : null);
        else addBins(targetDirs?.[unit?.cargoDir ?? "."], unit, `${hostTarget()}/release`);
      }
      const outputWorkspaces = (snapshotBroker ? scanResult.scans.map((r) => r.snapshotEvidence?.outputDir) : Object.values(targetDirs ?? {})).filter(Boolean).map((dir) => [dir]);
      o.onCompileOutputs?.(outputWorkspaces.map(([dir]) => dir), builtBins);
      o.out(`${tag} class B keying: ${ROW_DIGEST_STATS.misses} row digest(s) computed in ${(ROW_DIGEST_STATS.ms / 1000).toFixed(1)} s, ${ROW_DIGEST_STATS.hits} reused`);
      if (snapshotBroker) o.out(`${tag} broker compile: ${new Set(scanResult.scans.map((r) => r.snapshotEvidence?.requestId).filter(Boolean)).size} carried request(s) with verified signed receipts (${scanResult.scans.filter((r) => r.snapshotEvidence).length} scan row(s)); no direct receipt on a broker host`);
      else {
        // The direct host's own receipt (GATE-CONTRACT "Broker interface", row 7.18): the contract's returned-evidence names minus the
        // broker-only ones (admission, queue, reusedFrom): the run, the commit, the snapshot's tree at start and exit, the declared
        // inputs digested in place, the verdict, the freshness census (7.19), the timings, the exit status, the structured
        // diagnostics, the resolved output directory (7.09, with its inputs) and `refreshed` (nothing is reused on a direct host).
        // The gate refuses its own receipt unless it verifies, so a consumer never meets a receipt with a name missing.
        const requestId = randomUUID();
        const exitCode = failures.length ? 1 : 0;
        const diagnosticsDir = (() => {
          try { return path.join(gateRunsRoot({ env: o.env, platform: o.platform }), "rightkit-gate-direct-diagnostics"); } catch { return path.join(os.tmpdir(), "rightkit-gate-direct-diagnostics"); }
        })();
        let directReceipt;
        try {
          const wrote = Date.now();
          const diagnosticsPath = writeDirectDiagnostics({ dir: diagnosticsDir, requestId, commit: { sha: snapshotBCommit.commit, tree: snapshotBCommit.tree }, exit: exitCode, scans: scanResult.scans, failures, deferrals, notes: [...scanResult.notes, ...directDeclaredNotes], freshness });
          directReceipt = buildDirectReceipt({
            requestId, issuedAt: new Date().toISOString(), commit: { sha: snapshotBCommit.commit, tree: snapshotBCommit.tree },
            snapshotPath: snapshotB, treeAtStart: snapshotBCommit.tree, treeAtExit: snapshotBCommit.tree, ownershipHandle: o.buildOwnership?.handle ?? null,
            declaredInputDigests: directDeclared, verdict: directVerdict({ scans: scanResult.scans, failures, deferrals }), freshness,
            timingsMs: { bind: directTiming.bindEnd - directTiming.bindStart, cargo: directTiming.cargoEnd - directTiming.cargoStart, afterExitBind: directTiming.afterEnd - directTiming.afterStart, diagnostics: directTiming.diagnostics + (Date.now() - wrote) },
            exit: exitCode, diagnosticsPath, outputDirs: targetDirs ?? {}, outputResolution,
            cargoWrapper: cargoShim ? { identity: cargoShim.identity, workspaces: cargoShim.workspaces } : null,
          });
        } catch (error) { o.err(`${tag} BLOCKED: RK_DIRECT_RECEIPT: the direct receipt cannot be written (${error.message}); the compile's evidence is incomplete`); return 1; }
        const receiptProblems = verifyDirectReceipt(directReceipt);
        if (receiptProblems.length) { o.err(`${tag} BLOCKED: RK_DIRECT_RECEIPT: the direct receipt does not verify: ${receiptProblems.join("; ")}`); return 1; }
        o.onDirectReceipt?.(directReceipt);
        o.out(`${tag} direct receipt ${JSON.stringify(directReceipt)}`);
      }
    }
    if (unionContext) unionContext.completed = true;
    if (failures.length) {
      o.err(`${tag} BLOCKED (${failures.length})`);
      for (const f of failures) o.err(`  - ${f}`);
      // A red full class A names its cause when every failure is one a re-run of the same inputs reproduces AND the bytes and
      // environment it was judged in held from start to end, so the broker may reuse it (rightkit#1257); a live scan, a timeout, a
      // missing tool, anything undecided, or a binding that moved names none.
      if (boundFullA) {
        const binding = await judgedBinding();
        if (binding.problem) {
          o.err(`${tag} ${binding.prefix}: ${binding.problem} No deterministic verdict is named.`);
          if (binding.exit === 2) return 2;
        } else {
          for (const n of binding.notes) o.out(`${tag} ${n}`);
          const marker = (await import("../snapshot-check.mjs")).deterministicMarker(scanResult, plan.scopeDigest ? createHash("sha256").update(`${binding.env}:${plan.scopeDigest}:${plan.scanSetDigest}`).digest("hex") : binding.env);
          if (marker) o.out(marker);
        }
      }
      o.err(classAOnly ? "Class A is red: nothing compiles until it is green." : "Fix and push again. Never bypass with --no-verify.");
      return 1;
    }
    if (fullA) {
      // The verdict is for the bytes that were checked, in the environment that was checked: either moving makes it a verdict for nothing.
      const binding = await judgedBinding();
      if (binding.problem) { o.err(`${tag} ${binding.prefix}: ${binding.problem}`); return binding.exit; }
      for (const n of binding.notes) o.out(`${tag} ${n}`);
      // The bytes this verdict covers, for the broker to compare with the snapshot it compiles: the tracked files only (what a
      // captured snapshot holds), and every file including untracked ones. Each is sha256 over sorted "<path>\t<git blob id of the
      // raw bytes>\n" lines (git hash-object --no-filters), so the broker can recompute it over its snapshot without this gate.
      if (live) {
        // A snapshot holds only the commit's files: every one is tracked.
        const trackedList = snapFullA || hostedFullA ? null : git("ls-files", "-z", "--cached");
        if (trackedList && failed(trackedList)) { o.err(`${tag} BLOCKED: cannot list tracked files to name the checked bytes`); return 1; }
        const tracked = new Set(snapFullA || hostedFullA ? live.files : text(trackedList).split("\0").filter(Boolean).map(posix));
        o.out(`${tag} class A bytes tracked=${live.digestOf(live.files.filter((f) => tracked.has(f)))} all=${live.digest}${envStartWithCaches ? ` env-at-start=${envStartWithCaches}` : ""} env=${plan.scopeDigest ? createHash("sha256").update(`${binding.env}:${plan.scopeDigest}:${plan.scanSetDigest}`).digest("hex") : binding.env}${plan.scopeDigest ? ` scope=${plan.scopeDigest} scans=${plan.scanSetDigest}` : ""}`);
      }
      if (snapshotA && snapshotCommitId && o.env.RIGHTKIT_PREPARATION_DIAGNOSTIC_DIR) {
        try { await (await import("../snapshot-dependency-reuse.mjs")).capturePostClassADiagnostics({ checkout: root, snapshot: snapshotA, commit: snapshotCommitId.commit, roots: installRoots, diagnosticDir: o.env.RIGHTKIT_PREPARATION_DIAGNOSTIC_DIR }); }
        catch { /* Optional diagnostics never change the gate verdict. */ }
      }
      o.out(`${tag} class A ${plan.scopeDigest ? "scoped" : "full"} pass: ${scanResult.scans.length} deterministic check(s), the ones that execute repository code included, over ${changed.length} file(s) of ${base}`);
      return 0;
    }
    if (classAOnly) {
      // `check` judged the content only: the class A checks that execute the repository's code did not run here, so this is not a
      // full class A pass, and it says so (reviewer, 2026-10-27). The full gate runs them before any compile.
      const notRun = plan.scans.filter((s) => s.class === "A" && s.executes === true).map((s) => s.id);
      if (notRun.length) o.out(`${tag} class A partial: ${notRun.length} check(s) that execute repository code (tests, typecheck, lint, generated docs) run in the full gate, not in check: ${notRun.slice(0, 8).join(", ")}${notRun.length > 8 ? ", …" : ""}`);
      o.out(`${tag} class A content pass: ${scanResult.scans.length} deterministic scan(s) over ${changed.length} file(s) of ${base}`);
      return 0;
    }
    o.out(`${tag} pass (${changed.length} changed file(s) vs ${base}); ${deferrals.length} scan(s) deferred, not passed${scanResult.cacheHits.length ? `; ${scanResult.cacheHits.length} reused from the result cache` : ""}`);
    return 0;
  } finally {
    for (const [sig, handler] of signals) process.off(sig, handler);
    cleanup();
  }
}

// The inherited locations a snapshot compile requires as absolute paths (GATE-CONTRACT "Output directory & cache").
const SNAPSHOT_ABSOLUTE_ENV = ["CARGO_HOME", "RUSTUP_HOME", "SCCACHE_DIR", "TMPDIR", "TMP", "TEMP"];

/**
 * What the production entry (`node rightkit-gate.mjs ...`) hands `main` as its injected dependencies, and what `--route` reports: the
 * snapshot carrier on Mac and Dell (rightkit#1301: right-release's own Cargo calls, the Tauri CLI's and cargo-about's are carried, so
 * every stage compiles the commit's snapshot through the broker); nothing on Linux, which compiles direct with no broker. Tests inject
 * their own carrier through `main`; the contract test (door-route.test.mjs) runs the real entry. `platform` is the host whose deps are
 * asked for (the real entry passes none).
 */
export async function productionEntryDeps({ platform = process.platform, env = process.env } = {}) {
  // A direct Linux host carries nothing. A Linux host that DECLARED the broker route (managed-cargo.mjs linuxBrokerDeclared) gets the same carrier as the
  // Mac and the Dell, valid declaration or not: an invalid one is refused by the executor (kind "refuse"), never left to compile direct without a carrier.
  if (directPlatform(platform) && !linuxBrokerDeclared(env, platform)) return {};
  const { PRODUCTION_SNAPSHOT_CARRIER } = await import("../snapshot-broker.mjs");
  return { snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER };
}

const invokedDirectly = (() => {
  try { return fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url)); } catch { return false; }
})();
/** This gate's own resolved entry file: the broker admits a compile only when the request names the pinned trusted gate by this
 * path (rustright reads it from RUSTRIGHT_CALLING_GATE; a missing or different path is refused). */
export const callingGatePath = () => fs.realpathSync(fileURLToPath(import.meta.url));
if (invokedDirectly) {
  // GC-A27 NN07-NN12: with RK_GATE_SPAWN_LEDGER set, every child process of this run is recorded (spawn-ledger.mjs). Off otherwise.
  installSpawnLedger();
  // Every child this run starts that can reach rustright (scans, unit stages, right-release dev/test, their builds) inherits it.
  process.env.RUSTRIGHT_CALLING_GATE = callingGatePath();
  delete process.env.RIGHTKIT_CALLING_GATE; // one spelling, never both
  // Production carries the snapshot carrier on Mac and Dell (rightkit#1301): every child that reaches Cargo is carried, so every stage
  // compiles the commit's snapshot. Linux takes the snapshot route direct, with no carrier. Tests inject theirs.
  productionEntryDeps().then((deps) => main(process.argv.slice(2), deps)).then((code) => process.exit(code), (e) => { console.error(`rightkit-gate: ${e?.stack ?? e}`); process.exit(2); });
}
