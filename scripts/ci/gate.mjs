#!/usr/bin/env node
// RightKit CI gate: verifies the crates, npm packages and Swift packages a push touched.
//
// RightKit is a public repository: every compile and every test runs in GitHub Actions, never on
// a developer host. Locally only `--plan` works: it prints the selection and runs nothing.
//
// A push is compared with the last commit this job passed on (the right-git last-green marker),
// so a red run is repeated in full for the units it covered until they pass. Units listed in
// ci/known-red.json still run and still show red, but do not fail the gate; an entry is a
// recorded defect with an owner, never a waiver for publishing.
//
// Sharded CI (.github/workflows/ci.yml): scripts/ci/ci-plan.mjs plans the run (`--plan --host <windows|macos|linux>` answers for
// another runner OS) and every job names its own units, so independent crates and packages run in parallel jobs:
//   gate.mjs --select "<tokens>" [--steps lock,fmt,test,closure,scans] [--base <sha>]
// Tokens: rust:<crate> (tests, clippy and every feature row of a crate), rust-check:<crate> (a dependent that must still compile),
// npm:<dir>, root:<suite id>, swift:<package id>. The diff and the last-green marker are not read: the plan already applied them.
// `--steps` picks what runs (default: all): lock = Cargo.lock check, fmt = rustfmt, test = tests/clippy/feature rows/npm/root/Swift,
// closure = compile the rust-check dependents, scans = the class A deterministic scans over the changed paths (every tracked
// file without a --base). A job without the scans step needs RK_SCANS_JOB_RESULT=success from the hygiene job. Known-red and the pass/fail report work exactly as in a diff run.
//
// Publish verification (.github/workflows/publish.yml) names its units explicitly:
//   gate.mjs --registry npm|crates --units "<names>" [--allow-host-exclusion "<names>"] [--allow-no-test "<names>"]
//            [--allow-ignored "<names>"] [--allow-missing-assets "<names>"]
// That mode tests exactly those units and compile-checks reverse workspace dependents (no diff,
// no dependent tests, no root or Swift suites) and
// treats every red unit as a failure: known-red is not a waiver for publishing. Nothing is skipped
// silently: a unit this host would not run, or an npm package with no test script, fails the run
// unless the request names it. Test results are READ from the runners' own summaries (libtest, node:test,
// vitest) and SKIP markers, never inferred from an exit code: a unit that executed no test fails, an
// ignored test fails unless the unit is in allow_ignored, a skipped test or SKIP marker fails unless the
// unit is in allow_missing_assets (and a unit not named there runs with RIGHTKIT_REQUIRE_ASSETS=1, so a
// missing asset is a failure rather than a skip). It writes unit-evidence.json, one explicit status per requested unit, which the
// publish verify step turns into this host's receipt.
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { checkArgs, clippyArgs, planScans, runScans, scanDeferrals, scanFailures } from "../../packages/release/deterministic-scans.mjs";
import { scanGatePassed } from "./scan-receipt.mjs";
import { judgeUnit, mergeCounts, ResultScanner, TEST_COMMAND } from "./test-results.mjs";
import { dependentClosureEvidence } from "./gate-evidence.mjs";
import { needsChrome, provisionForTests } from "./provision-chrome.mjs";
import { parseSelectTokens, parseSteps, requiredConsumerLockCommands, swiftXcodebuilds } from "./shards.mjs";
import { changedConfigCrates, changedLockNames, parseLock, pnpmBuildClosureArgs, selectByPaths, selectNpm, selectRust, selectUnits } from "./select.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const WIN = process.platform === "win32";
const PLAN = process.argv.includes("--plan");
const argOf = (flag) => { const i = process.argv.indexOf(flag); return i > 0 ? process.argv[i + 1] : undefined; };
// Only `--plan` may name another host (ci-plan.mjs plans both runner OSes on one Linux job); a run always uses its own.
const PLAN_HOST = PLAN ? argOf("--host") : undefined;
if (PLAN_HOST !== undefined && !["windows", "macos", "linux"].includes(PLAN_HOST)) { console.error(`gate: --host must be windows, macos or linux, got ${JSON.stringify(PLAN_HOST)}`); process.exit(2); }
const HOST = PLAN_HOST ?? (WIN ? "windows" : process.platform === "darwin" ? "macos" : "linux");
const MINUTE = 60_000;
const UNIT_MODE = process.argv.includes("--units") || process.argv.includes("--registry");
const SELECT = argOf("--select");
if (SELECT !== undefined && UNIT_MODE) { console.error("gate: --select cannot be combined with --units or --registry"); process.exit(2); }
const SELECT_MODE = SELECT !== undefined;
let STEPS;
try { STEPS = parseSteps(argOf("--steps")); } catch (error) { console.error(`gate: ${error.message}`); process.exit(2); }

const ROOT_SUITES = [
  { id: "test:ownership-approval", paths: ["ownership-approval"] },
  { id: "test:characterization", paths: ["characterization"] },
  { id: "test:infrastructure", paths: ["infrastructure"] },
  { id: "test:swift-contracts", paths: ["swift/contracts"] },
];
const SCRIPT_TESTS = { id: "scripts", paths: ["scripts"] };
const SWIFT_ROOT_MANIFEST = "swift/root-manifest";
const SWIFT_PACKAGES = [
  { id: "swift/RightKitSwift", paths: ["swift/RightKitSwift"] },
  { id: "swift/RightKitCloudKitHelper", paths: ["swift/RightKitCloudKitHelper"] },
  // The repository-root Package.swift is what apps resolve by URL + tag; it reuses swift/RightKitSwift sources by path.
  { id: SWIFT_ROOT_MANIFEST, paths: ["Package.swift", "swift/RightKitSwift"] },
];

const git = (...args) => spawnSync("git", args, { cwd: ROOT, encoding: "utf8", maxBuffer: 256 * 1024 * 1024 });
const readJson = (file, fallback) => { try { return JSON.parse(fs.readFileSync(path.join(ROOT, file), "utf8")); } catch { return fallback; } };
const isSha = (value) => /^[0-9a-f]{40}$/i.test(value ?? "");

function resolveBase() {
  const explicit = argOf("--base");
  if (explicit) return git("rev-parse", "--verify", `${explicit}^{commit}`).stdout.trim() || null;
  if (process.env.GITHUB_EVENT_NAME === "pull_request" && process.env.GITHUB_BASE_REF) {
    const base = git("merge-base", `origin/${process.env.GITHUB_BASE_REF}`, "HEAD").stdout.trim();
    return isSha(base) ? base : null;
  }
  let marker = "";
  try { marker = fs.readFileSync(path.join(os.homedir(), ".right-git-last-green", "sha"), "utf8").trim(); } catch { /* no marker: full run */ }
  if (!isSha(marker) || git("cat-file", "-e", `${marker}^{commit}`).status !== 0) return null;
  return git("merge-base", "--is-ancestor", marker, "HEAD").status === 0 ? marker : null;
}

function rustMembers(config) {
  const members = [];
  for (const entry of fs.readdirSync(path.join(ROOT, "crates"), { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    let manifest;
    try { manifest = fs.readFileSync(path.join(ROOT, "crates", entry.name, "Cargo.toml"), "utf8"); } catch { continue; }
    const name = /^\[package\][^[]*?^name\s*=\s*"([^"]+)"/ms.exec(manifest)?.[1];
    if (name) members.push({ name, dir: `crates/${entry.name}`, extraPaths: config.rust?.[name]?.paths ?? [] });
  }
  return members;
}

// Directories pnpm-workspace.yaml excludes (`- '!packages/<name>'`, parked packages) are not
// installed by `pnpm install`, so the gate must not select or test them either.
function workspaceExcluded() {
  let text = "";
  try { text = fs.readFileSync(path.join(ROOT, "pnpm-workspace.yaml"), "utf8"); } catch { return new Set(); }
  return new Set([...text.matchAll(/^\s*-\s*['"]?!packages\/([^'"\s/]+)['"]?\s*$/gm)].map((m) => m[1]));
}

function npmPackages() {
  const packages = [];
  const excluded = workspaceExcluded();
  for (const entry of fs.readdirSync(path.join(ROOT, "packages"), { withFileTypes: true })) {
    if (!entry.isDirectory() || excluded.has(entry.name)) continue;
    const manifest = readJson(`packages/${entry.name}/package.json`, null);
    if (manifest?.name) packages.push({ name: manifest.name, dir: `packages/${entry.name}`, scripts: manifest.scripts ?? {}, all: { ...manifest.dependencies, ...manifest.devDependencies, ...manifest.peerDependencies } });
  }
  const names = new Set(packages.map((p) => p.name));
  return packages.map((p) => ({ name: p.name, dir: p.dir, scripts: p.scripts, deps: Object.keys(p.all).filter((d) => names.has(d)) }));
}

function unitPlan() {
  const config = readJson("ci/gate.config.json", {});
  const lock = parseLock(fs.readFileSync(path.join(ROOT, "Cargo.lock"), "utf8"));
  const units = (argOf("--units") ?? "").split(/\s+/).filter(Boolean);
  const packages = npmPackages();
  const names = (flag) => (argOf(flag) ?? "").split(/\s+/).filter(Boolean);
  let picked;
  const allowIgnored = names("--allow-ignored"); const allowMissingAssets = names("--allow-missing-assets");
  try { picked = selectUnits({ registry: argOf("--registry"), units, members: rustMembers(config), packages, lock, host: HOST, config, allowHostExclusion: names("--allow-host-exclusion"), allowNoTest: names("--allow-no-test"), allowIgnored, allowMissingAssets, assetGated: readJson("ci/asset-gated.json", {}) }); } catch (error) {
    console.error(`gate: ${error.message}. Usage: --registry npm|crates --units "<names>"`);
    process.exit(2);
  }
  return { host: HOST, base: null, all: false, changedPaths: 0, config, packages, rust: picked.rust, npm: picked.npm, root: [], swift: [], unitRequest: { registry: argOf("--registry"), units, excluded: picked.excluded, noTest: picked.noTest, allowIgnored, allowMissingAssets } };
}

/** Sharded CI: the units are named by the workflow's plan job, so neither the diff nor the last-green marker is read here. */
function selectPlan() {
  const config = readJson("ci/gate.config.json", {});
  const packages = npmPackages();
  const known = { rust: rustMembers(config).map((m) => m.name), npm: packages.map((p) => p.dir), root: [...ROOT_SUITES, SCRIPT_TESTS].map((s) => s.id), swift: SWIFT_PACKAGES.map((s) => s.id) };
  let picked;
  try { picked = parseSelectTokens(SELECT, known); } catch (error) {
    console.error(`gate: ${error.message}. Usage: --select "rust:<crate> rust-check:<crate> npm:<dir> root:<id> swift:<id>"`);
    process.exit(2);
  }
  return { host: HOST, base: argOf("--base") ?? null, all: false, changedPaths: 0, config, packages, ...picked };
}

function plan() {
  if (UNIT_MODE) return unitPlan();
  if (SELECT_MODE) return selectPlan();
  const config = readJson("ci/gate.config.json", {});
  const base = resolveBase();
  const all = !base || process.argv.includes("--all");
  const changedPaths = all ? [] : git("diff", "--name-only", "--diff-filter=ACDMRTUXB", base, "HEAD").stdout.split("\n").filter(Boolean);
  const lock = parseLock(fs.readFileSync(path.join(ROOT, "Cargo.lock"), "utf8"));
  let lockChanged = new Set(); let configChanged = new Set();
  if (!all) {
    lockChanged = changedLockNames(parseLock(git("show", `${base}:Cargo.lock`).stdout), lock);
    const previous = git("show", `${base}:ci/gate.config.json`);
    configChanged = changedConfigCrates(previous.status === 0 ? JSON.parse(previous.stdout) : {}, config);
  }
  const packages = npmPackages();
  return {
    host: HOST, base, all, changedPaths: changedPaths.length, config, packages,
    rust: selectRust({ changedPaths, members: rustMembers(config), lock, lockChanged, configChanged, all }),
    npm: selectNpm({ changedPaths, packages, all }),
    root: selectByPaths({ changedPaths, suites: [...ROOT_SUITES, SCRIPT_TESTS], all }),
    swift: HOST === "macos" ? selectByPaths({ changedPaths, suites: SWIFT_PACKAGES, all }) : [],
  };
}

// ---- execution -------------------------------------------------------------------------------

const EVIDENCE = path.join(process.env.RUNNER_TEMP ?? os.tmpdir(), "rightkit-ci");
const results = [];
/** How long output is still read after a command has exited or been killed. */
const PIPE_DRAIN_MS = 5000;

function killTree(child) {
  try {
    if (WIN) spawnSync("taskkill", ["/T", "/F", "/PID", String(child.pid)], { stdio: "ignore" });
    else process.kill(-child.pid, "SIGKILL");
  } catch { /* already gone */ }
}

/** Run one command, teeing output to the job log and to the unit's evidence log. */
function run(unit, command, args, { cwd = ROOT, env = {}, timeoutMinutes = 25, discardStdout = false } = {}) {
  const label = `${unit}: ${command} ${args.join(" ")}`;
  return new Promise((resolve) => {
    const started = Date.now();
    const logFile = path.join(EVIDENCE, "logs", `${unit.replace(/[^A-Za-z0-9._-]+/g, "_")}.log`);
    fs.mkdirSync(path.dirname(logFile), { recursive: true });
    const log = fs.createWriteStream(logFile, { flags: "a" });
    log.write(`\n$ ${command} ${args.join(" ")}\n`);
    console.log(`::group::${label}`);
    let timedOut = false; let settled = false;
    // Line-buffered, so a marker or summary split across two chunks is still read as one line.
    const outScan = new ResultScanner(); const errScan = new ResultScanner();
    const finish = (code, note) => {
      if (settled) return; settled = true;
      clearTimeout(timer);
      const seconds = Math.round((Date.now() - started) / 1000);
      const ok = code === 0 && !timedOut;
      if (note) { console.log(note); log.write(`${note}\n`); }
      log.end();
      console.log("::endgroup::");
      console.log(`${ok ? "PASS" : "FAIL"} ${label} (${seconds}s${timedOut ? ", timed out" : ""})`);
      const counts = mergeCounts(outScan.finish(), errScan.finish());
      results.push({ unit, command: `${command} ${args.join(" ")}`, ok, code, timedOut, seconds, isTest: TEST_COMMAND.test(`${command} ${args.join(" ")}`), ...counts });
      // Written after every step: a job that is cancelled at its time limit still uploads what ran.
      fs.writeFileSync(path.join(EVIDENCE, "progress.json"), JSON.stringify(results, null, 1));
      resolve(ok);
    };
    // pnpm is a .cmd shim on Windows and needs a shell there; the arguments here never contain spaces.
    const child = spawn(command, args, { cwd, env: { ...process.env, CARGO_TERM_COLOR: "always", ...(UNIT_MODE ? { RIGHTKIT_REQUIRE_ASSETS: "1" } : {}), ...(UNIT_MODE || process.env.CI ? { RIGHTKIT_REQUIRE_DESKTOP: "1" } : {}), ...env }, stdio: ["ignore", "pipe", "pipe"], shell: WIN && command === "pnpm", detached: !WIN });
    const timeoutNote = `timed out after ${timeoutMinutes} minutes`;
    // The step ends when the command EXITS, not when its output pipes close. "close" waits for
    // every holder of those pipes, and a test that leaves a child process behind keeps them open
    // after cargo has gone: the gate then waited for ever and, on main, blocked every later run
    // (Windows, rightkit-fs e2e, 2026-10-12: cargo had printed its last line after 5 minutes and
    // the job sat for 71). The time limit could not end it either: it kills the command's tree,
    // and a leftover process is no longer in the tree of a command that has exited.
    const release = () => { child.stdout.destroy(); child.stderr.destroy(); };
    const settle = (code, note) => { finish(code, note); release(); };
    const timer = setTimeout(() => {
      timedOut = true; killTree(child);
      setTimeout(() => settle(1, timeoutNote), PIPE_DRAIN_MS).unref();
    }, timeoutMinutes * MINUTE);
    child.stdout.on("data", (chunk) => { if (settled) return; if (!discardStdout) { outScan.push(chunk); process.stdout.write(chunk); log.write(chunk); } });
    child.stderr.on("data", (chunk) => { if (settled) return; errScan.push(chunk); process.stdout.write(chunk); log.write(chunk); });
    child.on("error", (error) => settle(1, `could not start: ${error.message}`));
    child.on("close", (code) => settle(code ?? 1, timedOut ? timeoutNote : ""));
    // Output still in the pipes is read for a moment after the exit; then the step is over.
    child.on("exit", (code) => {
      setTimeout(() => {
        if (!settled) console.log("output pipes still open after the command exited (a process it started is still running); moving on");
        settle(code ?? 1, timedOut ? timeoutNote : "");
      }, PIPE_DRAIN_MS).unref();
    });
  });
}

function savePatch(name, pathspec) {
  const diff = git("diff", "--", ...pathspec).stdout;
  if (!diff) return false;
  fs.mkdirSync(EVIDENCE, { recursive: true });
  fs.writeFileSync(path.join(EVIDENCE, name), diff);
  console.log(`::group::${name} (apply locally with git apply, then commit)`);
  console.log(diff);
  console.log("::endgroup::");
  return true;
}

/** Unit mode is strict (a missing asset fails) unless the request names the unit in allow_missing_assets. */
const assetEnv = (selection, name) => (UNIT_MODE && selection.unitRequest?.allowMissingAssets?.includes(name) ? { RIGHTKIT_REQUIRE_ASSETS: "0" } : {});

// ORT-PROVISION (the single call site): the catalog-pinned Windows ONNX Runtime for the real-runtime
// tests, provisioned once and only when a unit that uses it is tested. A failed provision is a red row.
const ORT_UNITS = new Set(["rightkit-onnx"]);
let ortEnv;
async function ortEnvFor(name) {
  if (!ORT_UNITS.has(name)) return {};
  if (ortEnv === undefined) {
    const started = Date.now();
    try { ortEnv = await (await import("./provision-ort.mjs")).ortRuntimeEnv(); }
    catch (error) {
      console.error(`provision-ort: ${error.message}`);
      results.push({ unit: "ort-runtime", command: "node scripts/ci/provision-ort.mjs", ok: false, code: 1, seconds: Math.round((Date.now() - started) / 1000) });
      ortEnv = {};
    }
  }
  return ortEnv;
}

// ---- consumer rows (mirrored from packages/release/local-publish.mjs: consumerRows, consumerRowInvocations, assertConsumerFiles) ----
// A crate's ci/gate.config.json entry may carry `consumerRows`: each checks the crate through a SEPARATE Cargo
// workspace (its own Cargo.lock) as `cargo check --manifest-path <manifestPath> --locked [--target <t>] <args>`.
// Keep the validation identical to local-publish.mjs so the CI gate and the local gate refuse the same rows. The one
// CI-only rule is the `paths` requirement below (0.4.1 review B1): CI selects crates by changed paths, local publish does not.
const CONSUMER_ROW_KEYS = new Set(["manifestPath", "args", "kind", "targets", "locked"]);
const CONSUMER_KINDS = new Set(["check"]);
const CONSUMER_MANIFEST = /^ci\/consumers\/(?:[A-Za-z0-9._-]+\/)+Cargo\.toml$/;
const CONSUMER_TARGET = /^[A-Za-z0-9_]+(?:-[A-Za-z0-9_.]+){2,3}$/;
// An ALLOWLIST (0.4.1 review A1), identical to local-publish.mjs: every other token is refused.
const CONSUMER_ARG_FLAGS = new Set(["--no-default-features", "--all-features", "--lib", "--bins", "--tests", "--examples", "--benches", "--all-targets"]);
const CONSUMER_FEATURE_FLAGS = new Set(["--features", "-F"]);
const CONSUMER_FEATURE_LIST = /^[A-Za-z0-9_][A-Za-z0-9_.+/?:,-]*$/;
const CONSUMER_ARGS_ALLOWED = `--features <list> (or --features=<list>, -F <list>), ${[...CONSUMER_ARG_FLAGS].join(", ")}`;

function consumerRows(unit, entry = {}) {
  const rows = entry.consumerRows;
  if (rows === undefined) return [];
  const refuse = (message) => { throw new Error(message); };
  const where = (index) => `${unit} consumerRows[${index}]`;
  if (!Array.isArray(rows)) refuse(`${unit}: consumerRows must be an array`);
  const seen = new Set();
  return rows.map((row, index) => {
    if (row === null || typeof row !== "object" || Array.isArray(row)) refuse(`${where(index)} must be an object`);
    for (const key of Object.keys(row)) if (!CONSUMER_ROW_KEYS.has(key)) refuse(`${where(index)}: unknown field ${JSON.stringify(key)} (allowed: ${[...CONSUMER_ROW_KEYS].join(", ")})`);
    const manifestPath = row.manifestPath;
    if (typeof manifestPath !== "string" || !CONSUMER_MANIFEST.test(manifestPath) || manifestPath.split("/").some((part) => part === "." || part === "..")) {
      refuse(`${where(index)}: manifestPath must be a relative ci/consumers/<name>/Cargo.toml path with forward slashes, got ${JSON.stringify(manifestPath)}`);
    }
    // CI runs a crate only when a changed path falls under its own directory or one of its `paths` (scripts/ci/select.mjs);
    // a consumer directory outside both would change without running its row, so it must be listed (0.4.1 review B1).
    const consumerDir = path.posix.dirname(manifestPath);
    const listed = (Array.isArray(entry.paths) ? entry.paths : []).filter((entryPath) => typeof entryPath === "string").map((entryPath) => entryPath.replace(/\/$/, ""));
    if (!listed.some((entryPath) => consumerDir === entryPath || consumerDir.startsWith(`${entryPath}/`))) {
      refuse(`${where(index)}: consumer directory ${consumerDir} must be listed (or sit under a listed directory) in rust.${unit}.paths of ci/gate.config.json, or a change to it selects no crate and its row never runs in CI`);
    }
    const kind = row.kind ?? "check";
    if (!CONSUMER_KINDS.has(kind)) refuse(`${where(index)}: kind must be one of ${[...CONSUMER_KINDS].join(", ")}, got ${JSON.stringify(kind)}`);
    if (row.locked !== undefined && row.locked !== true) refuse(`${where(index)}: locked must be true; a consumer row never runs without its lock file`);
    const args = row.args ?? [];
    if (!Array.isArray(args) || args.some((arg) => typeof arg !== "string" || arg === "")) refuse(`${where(index)}: args must be an array of non-empty strings`);
    for (let position = 0; position < args.length; position += 1) {
      const arg = args[position];
      if (CONSUMER_ARG_FLAGS.has(arg)) continue;
      if (CONSUMER_FEATURE_FLAGS.has(arg)) {
        const list = args[position + 1];
        if (typeof list !== "string" || !CONSUMER_FEATURE_LIST.test(list)) refuse(`${where(index)}: ${arg} needs a feature list (names separated by commas, not starting with "-"), got ${JSON.stringify(list)}`);
        position += 1;
        continue;
      }
      if (arg.startsWith("--features=") && CONSUMER_FEATURE_LIST.test(arg.slice("--features=".length))) continue;
      refuse(`${where(index)}: args may not contain ${JSON.stringify(arg)}; the row sets manifest path, lock, target and output itself, and allows only ${CONSUMER_ARGS_ALLOWED}`);
    }
    const targets = row.targets ?? [];
    if (!Array.isArray(targets) || targets.some((target) => typeof target !== "string" || !CONSUMER_TARGET.test(target))) refuse(`${where(index)}: targets must be an array of target triples`);
    if (new Set(targets).size !== targets.length) refuse(`${where(index)}: targets lists a triple twice`);
    const key = JSON.stringify([manifestPath, args, targets]);
    if (seen.has(key)) refuse(`${where(index)} repeats an earlier row`);
    seen.add(key);
    return { manifestPath, kind, args: [...args], targets: [...targets], locked: true };
  });
}

/** One Cargo step per target (or one host step when no target is named) for one consumer row. */
function consumerRowInvocations(row) {
  const targets = row.targets.length ? row.targets : [null];
  return targets.map((target) => ({ command: "cargo", args: [row.kind, "--manifest-path", row.manifestPath, "--locked", ...(target ? ["--target", target] : []), ...row.args] }));
}

/**
 * Fail closed before any command: every selected crate that applies on this host must have valid consumer rows
 * whose manifest and lock are tracked at their own real paths in the checkout. Returns validated rows by crate; exits 2 on a refusal (never
 * a red unit, so ci/known-red.json cannot waive a malformed row). Also runs under --plan.
 */
function validatedConsumerRows(selection) {
  const config = selection.config;
  const applies = (name) => !(config.rust?.[name]?.os) || config.rust[name].os.includes(HOST);
  const rowsByCrate = new Map();
  try {
    const realRoot = fs.realpathSync(ROOT);
    // Validate path coverage even when no crate was selected: an omitted `paths` entry must not hide the owning row.
    for (const [name, entry] of Object.entries(config.rust ?? {})) consumerRows(name, entry);
    for (const name of new Set([...selection.rust.test, ...selection.rust.check])) {
      if (!applies(name)) continue;
      const rows = consumerRows(name, config.rust?.[name] ?? {});
      for (const row of rows) {
        for (const file of [row.manifestPath, row.manifestPath.replace(/Cargo\.toml$/, "Cargo.lock")]) {
          const absolute = path.join(ROOT, file);
          if (!fs.existsSync(absolute)) throw new Error(`${name}: consumer row needs ${file}, which is not in the workspace`);
          if (path.relative(realRoot, fs.realpathSync(absolute)) !== path.normalize(file)) throw new Error(`${name}: consumer row file ${file} resolves outside its own path in the workspace; a symlink is not allowed`);
          if (git("ls-files", "--error-unmatch", "--", file).status !== 0) throw new Error(`${name}: consumer row file ${file} is not tracked by git; commit it first`);
        }
      }
      if (rows.length) rowsByCrate.set(name, rows);
    }
  } catch (error) {
    console.error(`gate: ${error.message}`);
    process.exit(2);
  }
  return rowsByCrate;
}

async function rustGate(selection, config, consumers = new Map()) {
  const { rust } = selection;
  if (!rust.test.length && !rust.check.length) return;
  const applies = (name) => !(config.rust?.[name]?.os) || config.rust[name].os.includes(HOST);
  // Check the lock first: a manifest edited without a build leaves it stale. The repair is
  // `cargo update --workspace`, and the patch it produces is evidence, not a reason to skip the
  // other units.
  const locked = !STEPS.has("lock") || await run("lock", "cargo", ["metadata", "--locked", "--format-version", "1"], { timeoutMinutes: 15, discardStdout: true });
  if (!locked) {
    await run("lock", "cargo", ["update", "--workspace"], { timeoutMinutes: 15 });
    savePatch("Cargo.lock.patch", ["Cargo.lock"]);
  }

  // Standalone consumers own locks too. Check each manifest once, even when several rows or crates share it.
  // A repair only produces a candidate patch: the failed locked metadata result stays red for this run.
  if (STEPS.has("lock")) {
    for (const { args } of requiredConsumerLockCommands([...consumers.values()].flat())) {
      const manifestPath = args[2];
      const consumerLocked = await run("lock", "cargo", args, { timeoutMinutes: 15, discardStdout: true });
      if (!consumerLocked) {
        await run("lock", "cargo", ["update", "--manifest-path", manifestPath, "--workspace"], { timeoutMinutes: 15 });
        savePatch(`${path.posix.dirname(manifestPath).replaceAll("/", "-")}-Cargo.lock.patch`, [manifestPath.replace(/Cargo\.toml$/, "Cargo.lock")]);
      }
    }
  }

  // rustfmt does not depend on the OS, so every selected crate is formatted wherever the step runs (CI runs it once, on Linux).
  const formatted = STEPS.has("fmt") ? rust.test : [];
  if (formatted.length) {
    // Batch the -p flags: one command line holding every crate overflows Windows' 32,767-char
    // limit (os error 206). Every batch runs even if one fails; each failure is recorded by run().
    const FMT_BATCH = 15;
    for (let i = 0; i < formatted.length; i += FMT_BATCH) {
      await run("fmt", "cargo", ["fmt", ...formatted.slice(i, i + FMT_BATCH).flatMap((name) => ["-p", name])], { timeoutMinutes: 10 });
    }
    const dirty = git("diff", "--name-only").stdout.split("\n").filter((p) => p.endsWith(".rs"));
    if (dirty.length) {
      savePatch("fmt.patch", dirty);
      results.push({ unit: "fmt", command: `rustfmt leaves ${dirty.length} file(s) unchanged`, ok: false, code: 1, seconds: 0 });
    }
  }

  for (const name of STEPS.has("test") ? rust.test : []) {
    if (!applies(name)) continue;
    const entry = config.rust?.[name] ?? {};
    const featureSets = entry.featureSets ?? [[]];
    const timeoutMinutes = entry.timeoutMinutes ?? 25;
    const env = { ...assetEnv(selection, name), ...(await ortEnvFor(name)) };
    for (const features of featureSets) {
      await run(`rust:${name}`, "cargo", ["test", "-p", name, ...features, "--no-fail-fast", "--", "--show-output"], { timeoutMinutes, env });
      if (entry.alsoSerial) await run(`rust:${name}`, "cargo", ["test", "-p", name, ...features, "--no-fail-fast", "--", "--test-threads=1", "--show-output"], { timeoutMinutes, env });
    }
    // Keep every tested feature set linted, matching local publication & the frozen shard command plan.
    // The clippy and check command lines come from the one scan module (packages/release/deterministic-scans.mjs).
    for (const features of featureSets) await run(`rust:${name}`, "cargo", clippyArgs(name, { features }), { timeoutMinutes });
    // Features the tests do not enable must still compile; keep in step with packages/release/local-publish.mjs.
    const checkSets = featureSets.some((features) => features.includes("--all-features")) ? [] : (entry.checkFeatureSets ?? [["--all-features"]]);
    for (const features of checkSets) await run(`rust:${name}`, "cargo", checkArgs(name, { features }), { timeoutMinutes });
    // Keep rustdoc qualification when local pre-push runs only static checks. Use configured rows
    // rather than blanket all-features: several crates have mutually exclusive feature families.
    const docSets = [...new Map([...featureSets, ...checkSets].map((features) => [JSON.stringify(features), features])).values()];
    for (const features of docSets) await run(`rust:${name}`, "cargo", ["doc", "-p", name, "--no-deps", ...features], { timeoutMinutes, env: { ...env, RUSTDOCFLAGS: `${process.env.RUSTDOCFLAGS ?? ""} -D warnings`.trim() } });
    // Consumer compile checks belong to either the test shard or the closure shard of their owner.
    for (const row of consumers.get(name) ?? []) {
      for (const step of consumerRowInvocations(row)) await run(`rust:${name}`, step.command, step.args, { timeoutMinutes, env });
    }
  }
  for (const name of STEPS.has("closure") ? rust.check : []) {
    if (!applies(name)) continue;
    await run(`rust:${name}`, "cargo", checkArgs(name), { timeoutMinutes: 30 });
    // R1: a dependent must also compile with the feature rows its own gate config names (all features unless it says otherwise).
    for (const features of (config.rust?.[name]?.checkFeatureSets ?? [["--all-features"]]).filter((features) => features.length)) await run(`rust:${name}`, "cargo", checkArgs(name, { features }), { timeoutMinutes: 30 });
    for (const row of consumers.get(name) ?? []) {
      for (const step of consumerRowInvocations(row)) await run(`rust:${name}`, step.command, step.args, { timeoutMinutes: config.rust?.[name]?.timeoutMinutes ?? 30, env: assetEnv(selection, name) });
    }
  }
}

/**
 * The paths the class A scans cover. A diff run reads the diff; a sharded or unit run names its units, so every selected
 * crate and package contributes its manifest (and the lock and deny config when Rust is selected), which selects the
 * crate-level scans (metadata, deny, audit, package list, changelog, licence, secret scan, lockfile).
 */
function scanPaths(selection) {
  if (!UNIT_MODE && !SELECT_MODE) {
    return selection.all
      ? git("ls-files").stdout.split("\n").filter(Boolean)
      : git("diff", "--name-only", "--diff-filter=ACMR", selection.base, "HEAD").stdout.split("\n").filter(Boolean);
  }
  // A sharded run with no changed-path base cannot say what changed, so the scans cover every tracked file (Rust, npm, workflow, scripts, docs).
  if (!selection.base && !UNIT_MODE) return git("ls-files").stdout.split("\n").filter(Boolean);
  const paths = new Set();
  if (selection.base) for (const f of git("diff", "--name-only", "--diff-filter=ACMR", selection.base, "HEAD").stdout.split("\n").filter(Boolean)) paths.add(f);
  const crateDirs = new Map(rustMembers(selection.config).map((m) => [m.name, m.dir]));
  const rustNames = [...new Set([...selection.rust.test, ...selection.rust.check])];
  for (const name of rustNames) { const dir = crateDirs.get(name); if (dir) { paths.add(`${dir}/Cargo.toml`); paths.add(`${dir}/CHANGELOG.md`); } }
  if (rustNames.length) { paths.add("Cargo.lock"); paths.add("deny.toml"); }
  for (const dir of selection.npm.test) paths.add(`${dir}/package.json`);
  return [...paths];
}

/**
 * Class A deterministic scans (metadata, deny, audit, package list, changelog, licence, secret scan, parse checks), run
 * through the shared runner BEFORE any class B work. CI may run the Rust tool directly, so the planner's prefix is plain
 * `cargo`. Class B (clippy, check, tests, tsc) stays with the compile and test steps below, which run only when this
 * step passed: a failed scan stops them.
 */
async function scansGate(selection) {
  const started = Date.now();
  const plan = planScans({ changed: scanPaths(selection), contentRoot: ROOT, liveRoot: ROOT, includeTests: false, cargoCommand: ["cargo"], readBytes: (f) => fs.readFileSync(f) });
  console.log(`::group::scans: class A (${plan.scans.filter((x) => x.class === "A").length} scans)`);
  const result = await runScans(plan, { classes: ["A"] });
  for (const line of scanDeferrals(result)) console.log(line);
  const failures = scanFailures(result);
  for (const line of failures) console.log(`FAIL ${line}`);
  console.log("::endgroup::");
  const seconds = Math.round((Date.now() - started) / 1000);
  for (const row of result.scans) results.push({ unit: "scans", command: `scan ${row.id}`, ok: row.status === "pass", code: row.status === "pass" ? 0 : 1, seconds: 0 });
  if (!result.scans.length) results.push({ unit: "scans", command: "scan (none selected)", ok: true, code: 0, seconds });
  // Success means every planned class A scan ran and passed: a deferred, not-run, failed or missing scan is not success.
  return scanGatePassed(plan.scans.filter((x) => x.class === "A"), result) && failures.length === 0;
}

async function npmGate(selection) {
  const { npm, root, packages } = selection;
  if (npm.test.length) {
    // Workspace packages resolve each other through built output, so build them in dependency order before any
    // selected package is tested. A publish verification builds only the requested packages and their workspace
    // dependency closure, so an unrelated package that does not build cannot block them.
    let buildArgs = ["-r", "--workspace-concurrency=1", "run", "build"];
    if (UNIT_MODE || SELECT_MODE) {
      try { buildArgs = pnpmBuildClosureArgs(npm.test.map((dir) => packages.find((p) => p.dir === dir)?.name)); } catch (error) { results.push({ unit: "npm:build", command: `pnpm build closure: ${error.message}`, ok: false, code: 1, seconds: 0 }); buildArgs = null; }
    }
    const built = buildArgs ? await run("npm:build", "pnpm", buildArgs, { timeoutMinutes: 30 }) : false;
    for (const dir of npm.test) {
      const pkg = packages.find((p) => p.dir === dir);
      if (!pkg?.scripts.test) {
        if (UNIT_MODE) results.push({ unit: `npm:${dir}`, command: "no test script", ok: false, code: 1, seconds: 0 });
        continue;
      }
      if (!built) { results.push({ unit: `npm:${dir}`, command: "not run: workspace build failed", ok: false, code: 1, seconds: 0 }); continue; }
      await run(`npm:${dir}`, "pnpm", ["--dir", dir, "run", "test"], { timeoutMinutes: 30, env: assetEnv(selection, pkg.name) });
    }
  }
  for (const id of root) {
    if (id === SCRIPT_TESTS.id) await run("root:scripts", "node", ["--test", "scripts/**/*.test.mjs"], { timeoutMinutes: 10 });
    else await run(`root:${id}`, "pnpm", ["run", id], { timeoutMinutes: 20 });
  }
}

async function swiftGate(selection) {
  for (const dir of selection.swift) {
    if (dir === SWIFT_ROOT_MANIFEST) {
      // Resolves every target path, resource and product of the root manifest without compiling anything.
      await run(`swift:${dir}`, "swift", ["package", "describe", "--type", "json"], { cwd: ROOT, timeoutMinutes: 10, discardStdout: true });
      continue;
    }
    const cwd = path.join(ROOT, dir);
    const built = await run(`swift:${dir}`, "swift", ["build"], { cwd, timeoutMinutes: 45 });
    if (built && fs.existsSync(path.join(cwd, "Tests"))) await run(`swift:${dir}`, "swift", ["test"], { cwd, timeoutMinutes: 60 });
    const { builds, skipped } = swiftXcodebuilds(dir);
    for (const platform of skipped) console.log(`swift:${dir}: generic ${platform} build skipped; Package.swift declares no ${platform} platform`);
    if (built) for (const { args } of builds) await run(`swift:${dir}`, "xcodebuild", args, { cwd, timeoutMinutes: 60 });
  }
}

const unitId = (selection, name) => (selection.unitRequest.registry === "crates" ? `rust:${name}` : `npm:${selection.packages.find((p) => p.name === name)?.dir}`);

/** How many test commands the gate runs for a unit on this host: every feature set, doubled when the crate also reruns serially; one for an npm package. */
function requiredTestCommands(selection, name) {
  if (selection.unitRequest.registry !== "crates") return 1;
  const entry = selection.config.rust?.[name] ?? {};
  return (entry.featureSets ?? [[]]).length * (entry.alsoSerial ? 2 : 1);
}

/** The runner-reported verdict of every requested unit that was actually run on this host. */
function unitVerdicts(selection) {
  const { units, excluded, noTest, allowIgnored, allowMissingAssets } = selection.unitRequest;
  const verdicts = new Map();
  for (const name of units) {
    if (excluded.includes(name) || noTest.includes(name)) continue;
    const id = unitId(selection, name);
    verdicts.set(id, judgeUnit({ name, steps: results.filter((r) => r.unit === id), allowIgnored, allowMissingAssets, requiredCommands: requiredTestCommands(selection, name) }));
  }
  return verdicts;
}

/** One explicit status per requested unit: pass, allowed-skips, allowed-exclusion, allowed-no-test, or fail (never absent). */
function writeUnitEvidence(selection, rows, ok, verdicts, dependentClosure) {
  const { registry, units, excluded, noTest } = selection.unitRequest;
  const byId = new Map(rows.map((r) => [r.id, r]));
  const evidence = units.map((name) => {
    const id = unitId(selection, name);
    const row = byId.get(id);
    const verdict = verdicts.get(id);
    const steps = results.filter((r) => r.unit === id).map((r) => ({ command: r.command, ok: r.ok, seconds: r.seconds, ...(r.isTest ? { executed: r.executed, failed: r.failed, ignored: r.ignored, skipped: r.skipped, binaries: r.binaries } : {}) }));
    let status = "fail";
    if (excluded.includes(name)) status = "allowed-exclusion";
    else if (noTest.includes(name)) status = "allowed-no-test";
    else if (row?.ok && verdict && verdict.status !== "fail") status = verdict.status;
    return { name, id, status, executed: verdict?.executed ?? 0, ignored: verdict?.ignored ?? 0, skipped: verdict?.skipped ?? 0, problems: verdict?.problems ?? [], steps };
  });
  fs.mkdirSync(EVIDENCE, { recursive: true });
  fs.writeFileSync(path.join(EVIDENCE, "unit-evidence.json"), JSON.stringify({ schema: 2, host: HOST, registry, sha: process.env.GITHUB_SHA ?? null, ok: ok && evidence.every((e) => e.status !== "fail"), units: evidence, ...(dependentClosure ? { dependentClosure } : {}) }, null, 2));
  if (evidence.some((e) => e.status === "fail")) console.log(`No pass evidence for: ${evidence.filter((e) => e.status === "fail").map((e) => e.name).join(", ")}`);
}

function report(selection) {
  const knownRed = UNIT_MODE ? {} : readJson("ci/known-red.json", {})[HOST] ?? {};
  const units = new Map();
  for (const result of results) {
    const unit = units.get(result.unit) ?? { ok: true, seconds: 0, failed: [], skipped: 0 };
    unit.ok &&= result.ok; unit.seconds += result.seconds; unit.skipped += result.skipped ?? 0;
    if (!result.ok) unit.failed.push(result.command + (result.timedOut ? " (timed out)" : ""));
    units.set(result.unit, unit);
  }
  const verdicts = UNIT_MODE ? unitVerdicts(selection) : new Map();
  for (const [id, verdict] of verdicts) {
    const unit = units.get(id);
    if (!unit) continue; // never ran: the missing-evidence check below names it
    if (verdict.status === "fail") { unit.ok = false; unit.failed.push(...verdict.problems); }
  }
  const dependentClosure = UNIT_MODE && selection.unitRequest.registry === "crates"
    ? dependentClosureEvidence({ selected: selection.rust.check, requested: selection.unitRequest.units, config: selection.config, results })
    : null;
  if (dependentClosure && !dependentClosure.ok) {
    units.set("dependent-check-evidence", { ok: false, seconds: 0, failed: dependentClosure.problems, skipped: 0 });
  }
  const rows = [...units].map(([id, unit]) => ({ id, ...unit, known: Object.hasOwn(knownRed, id) }));
  const unexpected = rows.filter((r) => !r.ok && !r.known);
  const nowGreen = rows.filter((r) => r.ok && r.known);
  const lines = [
    `## RightKit gate on ${HOST}`,
    "",
    `Base: ${selection.base ?? "none (full run)"}. Units run: ${rows.length}. Passed: ${rows.filter((r) => r.ok).length}. Known red: ${rows.filter((r) => !r.ok && r.known).length}. Unexpected red: ${unexpected.length}.`,
    "",
    "| Unit | Result | Seconds | Failed step |",
    "|---|---|---|---|",
    ...rows.map((r) => `| ${r.id} | ${r.ok ? "pass" : r.known ? "known red" : "**FAIL**"} | ${r.seconds} | ${r.failed.join("; ").replaceAll("|", "\\|")} |`),
    "",
    rows.some((r) => r.skipped) ? `Unproven on this runner (tests skipped for a missing asset or precondition; skipped is not passed${UNIT_MODE ? " unless the unit is named in allow_missing_assets" : ""}): ${rows.filter((r) => r.skipped).map((r) => `${r.id} (${r.skipped})`).join(", ")}` : "",
    nowGreen.length ? `Now green, remove from ci/known-red.json (${HOST}): ${nowGreen.map((r) => r.id).join(", ")}` : "",
    "Not covered by this gate: desktop-app-template, swift/ios-app-template, spikes.",
  ].join("\n");
  fs.mkdirSync(EVIDENCE, { recursive: true });
  fs.writeFileSync(path.join(EVIDENCE, "summary.md"), lines);
  fs.writeFileSync(path.join(EVIDENCE, "summary.json"), JSON.stringify({ host: HOST, sha: process.env.GITHUB_SHA ?? null, base: selection.base, selection: SELECT_MODE ? SELECT : null, steps: [...STEPS].join(","), rows, results }, null, 2));
  if (UNIT_MODE) writeUnitEvidence(selection, rows, unexpected.length === 0, verdicts, dependentClosure);
  if (process.env.GITHUB_STEP_SUMMARY) fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY, `${lines}\n`);
  console.log(lines);
  if (UNIT_MODE) {
    const missing = selection.unitRequest.units.filter((name) => {
      const id = unitId(selection, name);
      return !selection.unitRequest.excluded.includes(name) && !selection.unitRequest.noTest.includes(name) && !(units.get(id)?.ok);
    });
    if (missing.length) { console.log(`Requested units with no passing evidence: ${missing.join(", ")}`); return false; }
  }
  return unexpected.length === 0 && (!dependentClosure || dependentClosure.ok);
}

/**
 * Ordinary CI (publish verify provisions its own): the @rightkit/qa integration lane has no skip, so a runner without
 * Chrome for Testing fails it. Install the pinned build once per job (hash-marked, so a repeat is free) and export the
 * root and executable that @rightkit/qa-chrome and rightkit-browser resolve first. A failed provision is a red unit.
 */
async function chromeGate(selection) {
  if (UNIT_MODE || !needsChrome(selection)) return;
  const started = Date.now();
  const root = path.join(process.env.RUNNER_TOOL_CACHE || process.env.RUNNER_TEMP || os.tmpdir(), "rightkit-chrome-for-testing");
  console.log("::group::chrome: provision pinned Chrome for Testing");
  try {
    const { env } = await provisionForTests({ root });
    Object.assign(process.env, env);
  } catch (error) {
    console.log(`chrome provisioning failed: ${error.message}`);
    results.push({ unit: "chrome:provision", command: "provision-chrome", ok: false, code: 1, seconds: Math.round((Date.now() - started) / 1000) });
  }
  console.log("::endgroup::");
}

const selection = plan();
const consumers = validatedConsumerRows(selection);
if (PLAN) {
  const { config, packages, ...printable } = selection;
  console.log(JSON.stringify(printable, null, 2));
  process.exit(0);
}
if (process.env.GITHUB_ACTIONS !== "true") {
  console.error("RightKit compiles and tests only in GitHub Actions. Locally: node scripts/ci/gate.mjs --plan [--base <sha>]");
  process.exit(2);
}
console.log(`Selection: ${JSON.stringify({ base: selection.base, all: selection.all, rust: selection.rust, npm: selection.npm.test, root: selection.root, swift: selection.swift })}`);
// Class A scans run once per CI run, in the hygiene job (step lock), before any compile or test work (class B).
// ON by default in CI (rightkit#888, Astra policy C): there is no config switch. scansPassed starts false and is true only when
// this job ran the `scans` step and every planned scan passed, or when the workflow's `needs:` handed this job the hygiene
// job's success (RK_SCANS_JOB_RESULT=success; ci.yml makes every shard and closure job need hygiene). Nothing else is proof.
let scansPassed = false;
if (STEPS.has("scans")) scansPassed = await scansGate(selection);
else if (process.env.RK_SCANS_JOB_RESULT === "success") scansPassed = true;
else if (!STEPS.has("test") && !STEPS.has("closure")) scansPassed = true; // lock/fmt only: no class B work is gated here
else results.push({ unit: "scans", command: "class A scans: no scans step here and no successful hygiene result (RK_SCANS_JOB_RESULT)", ok: false, code: 1, seconds: 0 });
if (!scansPassed) console.log("class A scans did not pass: class B (compile, lint and test steps) is not run");
else {
  if (STEPS.has("test")) await chromeGate(selection);
  await rustGate(selection, selection.config, consumers);
  if (STEPS.has("test")) { await npmGate(selection); await swiftGate(selection); }
}
process.exit(report(selection) ? 0 : 1);
