#!/usr/bin/env node
// `right-release publish npm|cargo ... --local`: publish ONE @rightkit npm package or crate from the Mac (npm and
// crates) or from Windows (crates only) under Adrian's local-build grant (2026-10-14, committed
// .rightkit-local-build.json). It does not weaken the publication gate: it runs the unit's own CI tests
// (ci/gate.config.json, ci/asset-gated.json, scripts/ci/gate.mjs semantics), packs the artifact, writes a
// schema-1 evidence file with one receipt for the publishing host (hostPolicy "darwin-only" on the Mac,
// "win32-only" on Windows), then calls the EXISTING enforcePublicationGate and the EXISTING publisher
// (publishGatedTarball for npm, runCargoPublish for cargo). There is no way to skip a test or the evidence.
//
// Batch mode (`cargo --crates a,b,c | --crates-file f --local`): fmt, the secret scan and clippy run ONCE for
// the batch at one commit and tree and are recorded in every crate's evidence as a `batch` receipt that the
// publication gate validates (publication-gate.mjs batchReceiptProblems); each crate still runs its own tests,
// all-features check, packaging, gate, publish and registry readback, and writes its own evidence file. Crates
// of one dependency layer publish concurrently (bounded); layer N+1 starts only after every layer-N crate is
// visible in the registry index with the verified checksum.
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { runCargoAsync, runCargoSync, spawnCollect } from "./managed-cargo.mjs";
import { localBuildGranted, LOCAL_BUILD_FILE } from "./execution-policy.mjs";
import { GATE_SCHEMA, MAX_CARGO_JOBS, defaultEvidencePath, enforcePublicationGate, publishGatedTarball } from "./publication-gate.mjs";
import { runCargoPublish, runCargoReadOnly, scanCrateForSecrets } from "./publish-cargo.mjs";
import { cargoIndexUrl } from "./cargo-registry-readback.mjs";
import { refuseHostUpload } from "./host-upload-refusal.mjs";
import { pathDependencyDirs, readCargoPackage } from "./crate-graph.mjs";
import { recordPublished, stageSourceVersions, workspaceFeedOptions } from "./rightkit-sync.mjs";
import { prepareSwapHelper, SWAP_HELPER_CARGO_ARGS, SWAP_HELPER_PACKAGE } from "./swap-helper-bundle.mjs";
import { requireDoorRun, gateStateDir } from "./gate-stages.mjs";

export const LOCAL_OPERATOR = "agent (local-build grant)";
export const LOCAL_HOST_POLICY_REASON = "Adrian 2026-10-14 local-build grant (.rightkit-local-build.json); Windows covered by the next CI run";
export const LOCAL_WIN32_HOST_POLICY_REASON = "Adrian 2026-10-14 local-build grant (.rightkit-local-build.json); published from the Windows host, the macOS run is not part of this receipt";
// The hosts `--local` accepts and the host policy each records in the evidence.
export const LOCAL_HOST_POLICIES = Object.freeze({ darwin: "darwin-only", win32: "win32-only" });
const localHostPolicyReason = (platform) => (platform === "win32" ? LOCAL_WIN32_HOST_POLICY_REASON : LOCAL_HOST_POLICY_REASON);
const STEP_TIMEOUT_MS = 30 * 60 * 1000;

export class LocalPublishError extends Error {
  constructor(message) { super(message); this.name = "LocalPublishError"; }
}
const refuse = (message) => { throw new LocalPublishError(message); };

export const DEFAULT_CONCURRENCY = 4;
const MAX_CONCURRENCY = 8;
const CRATE_NAME = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;
const JOBS_SUBCOMMANDS = new Set(["test", "clippy", "check", "package"]);
// The argv of the broker jobs a unit's compile and package stages launch. Built here, once, for the stages themselves and for
// stage-commands.mjs, which derives what a qualification's signed receipts must show (the hosted admission and the gate share it).
export const qualifyFmtArgs = (unit) => ["fmt", "-p", unit, "--check"];
export const npmPackArgs = (packDir, scratch) => ["--dir", packDir, "pack", "--pack-destination", scratch];
export const cargoPackageArgs = (unit, qualifyOnly) => ["package", "--locked", ...(qualifyOnly ? [] : ["--no-verify"]), "-p", unit];
export const CARGO_METADATA_ARGS = Object.freeze(["metadata", "--offline", "--format-version", "1", "--no-deps"]);
export function withCargoJobs(args, jobs) {
  return jobs !== undefined && JOBS_SUBCOMMANDS.has(args[0])
    ? [args[0], "--jobs", String(jobs), ...args.slice(1)] : args;
}

const crateList = (text, what) => {
  const names = String(text ?? "").split(",").map((name) => name.trim()).filter(Boolean);
  for (const name of names) if (!CRATE_NAME.test(name)) refuse(`${what}: invalid crate name ${JSON.stringify(name)}`);
  return names;
};

/**
 * A crates file: one crate per line, blank lines separate dependency layers (earlier layers publish first),
 * `#` starts a comment. Returns the layers; refuses a bad name, a repeated crate or an empty file.
 */
export function parseCrateLayers(text) {
  const layers = []; let current = [];
  const flush = () => { if (current.length) layers.push(current); current = []; };
  for (const raw of String(text ?? "").split(/\r?\n/)) {
    if (raw.trim().startsWith("#")) continue;
    const line = raw.replace(/\s+#.*$/, "").trim();
    if (!line) { flush(); continue; }
    if (!CRATE_NAME.test(line)) refuse(`crates file: invalid crate name ${JSON.stringify(line)}`);
    current.push(line);
  }
  flush();
  const all = layers.flat();
  const repeated = all.find((name, index) => all.indexOf(name) !== index);
  if (repeated) refuse(`crates file names ${repeated} more than once`);
  if (!all.length) refuse("crates file names no crate");
  return layers;
}

/** An explicit layering must put every in-batch dependency of a crate in a STRICTLY earlier layer. */
export function checkCrateLayers(layers, dependsOn) {
  const layerOf = new Map();
  layers.forEach((layer, index) => layer.forEach((name) => layerOf.set(name, index)));
  for (const [name, index] of layerOf) {
    for (const dependency of dependsOn(name)) {
      if (!layerOf.has(dependency)) continue;
      if (layerOf.get(dependency) >= index) refuse(`${name} (layer ${index + 1}) depends on ${dependency} (layer ${layerOf.get(dependency) + 1}); a dependency must be in an earlier layer`);
    }
  }
  return layers;
}

/** Layers derived from the in-batch dependency edges: layer = 1 + the deepest in-batch dependency. */
export function deriveCrateLayers(crates, dependsOn) {
  const member = new Set(crates);
  const level = new Map(); const visiting = new Set();
  const visit = (name) => {
    if (level.has(name)) return level.get(name);
    if (visiting.has(name)) refuse(`dependency cycle among the batch crates at ${name}`);
    visiting.add(name);
    const depth = Math.max(-1, ...dependsOn(name).filter((dependency) => member.has(dependency) && dependency !== name).map(visit)) + 1;
    visiting.delete(name); level.set(name, depth);
    return depth;
  };
  crates.forEach(visit);
  const layers = [];
  for (const name of crates) (layers[level.get(name)] ??= []).push(name);
  return layers;
}

export function parseLocalPublishArgs(kind, argv, cwd = process.cwd()) {
  const flag = kind === "npm" ? "--package" : "--crate";
  const batchFlags = kind === "cargo" ? ", --crates <a,b,c>, --crates-file <file>, --concurrency <n>, --allow-ignored-units <a,b>" : "";
  let unit; let workspace; let local = false; let allowIgnored = false;
  let qualifyOnly = false; let bundle; let jobsText;
  let crates; let cratesFile; let concurrency = DEFAULT_CONCURRENCY; let allowIgnoredUnits = [];
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--local") local = true;
    else if (arg === flag) unit = argv[++i];
    else if (arg === "--workspace") workspace = argv[++i];
    else if (arg === "--qualify-only") qualifyOnly = true;
    else if (arg === "--bundle") bundle = argv[++i];
    // Same meaning as publish.yml's allow_ignored: this unit may have ignored tests; the count is recorded.
    else if (arg === "--allow-ignored") allowIgnored = true;
    else if (kind === "cargo" && arg === "--crates") crates = crateList(argv[++i], "--crates");
    else if (kind === "cargo" && arg === "--crates-file") cratesFile = argv[++i];
    else if (kind === "cargo" && arg === "--concurrency") concurrency = Number(argv[++i]);
    else if (kind === "cargo" && arg === "--allow-ignored-units") allowIgnoredUnits = crateList(argv[++i], "--allow-ignored-units");
    else if (kind === "cargo" && arg === "--jobs") jobsText = argv[++i] ?? "";
    else refuse(`unknown publish ${kind} --local option: ${arg} (accepted: ${flag} <name>${batchFlags}, --workspace <dir>, --allow-ignored, --local)`);
  }
  if (!local) refuse("--local is required");
  if (!Number.isInteger(concurrency) || concurrency < 1 || concurrency > MAX_CONCURRENCY) refuse(`--concurrency must be an integer from 1 to ${MAX_CONCURRENCY}`);
  const workspaceRoot = path.resolve(workspace ?? cwd);
  if (qualifyOnly && (crates !== undefined || cratesFile !== undefined)) refuse("--qualify-only currently requires one --crate");
  if (qualifyOnly && !String(bundle ?? "").trim()) refuse("--qualify-only requires --bundle <new-directory>");
  if (bundle !== undefined && !qualifyOnly) refuse("--bundle requires --qualify-only");
  const bundleDir = bundle === undefined ? undefined : path.resolve(cwd, bundle);
  let jobs;
  if (jobsText !== undefined) {
    if (!/^[0-9]+$/.test(jobsText) || Number(jobsText) < 1 || Number(jobsText) > MAX_CARGO_JOBS) refuse(`--jobs must be an integer from 1 to ${MAX_CARGO_JOBS}`);
    if (!qualifyOnly) refuse("--jobs requires --qualify-only");
    jobs = Number(jobsText);
  }
  if (kind === "cargo" && (crates !== undefined || cratesFile !== undefined)) {
    if (unit !== undefined) refuse("--crate cannot be combined with --crates or --crates-file");
    if (crates !== undefined && cratesFile !== undefined) refuse("use --crates or --crates-file, not both");
    if (crates !== undefined && !crates.length) refuse("--crates names no crate");
    if (cratesFile !== undefined && !String(cratesFile ?? "").trim()) refuse("--crates-file needs a file");
    const repeated = crates?.find((name, index) => crates.indexOf(name) !== index);
    if (repeated) refuse(`--crates names ${repeated} more than once`);
    return { kind, unit: undefined, allowIgnored, allowIgnoredUnits, workspaceRoot, concurrency, batch: { crates, file: cratesFile === undefined ? undefined : path.resolve(cwd, cratesFile) } };
  }
  if (kind === "npm" && !/^@rightkit\/[a-z0-9-]+$/.test(unit ?? "")) refuse("publish npm --local requires --package @rightkit/<name>");
  if (kind === "cargo" && !CRATE_NAME.test(unit ?? "")) refuse("publish cargo --local requires --crate <name> (or --crates / --crates-file)");
  return { kind, unit, allowIgnored, allowIgnoredUnits, workspaceRoot, concurrency, batch: undefined, ...(qualifyOnly ? { qualifyOnly, bundleDir } : {}), ...(jobs === undefined ? {} : { jobs }) };
}

function defaultGit(args, cwd) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true });
  if (result.error || result.status !== 0) return null;
  return (result.stdout ?? "").trim();
}

const defaultReadJson = (file) => { try { return JSON.parse(fs.readFileSync(file, "utf8")); } catch { return null; } };

async function defaultRegistryHas({ kind, name, version }) {
  if (kind === "npm") {
    const view = spawnSync("npm", ["view", `${name}@${version}`, "version", "--prefer-online", "--registry", "https://registry.npmjs.org/"], { encoding: "utf8", windowsHide: true });
    const out = `${view.stdout ?? ""}${view.stderr ?? ""}`;
    if (!view.error && view.status === 0) return (view.stdout ?? "").trim() === version;
    if (/E404|404 Not Found/i.test(out)) return false;
    throw new Error(`cannot read the npm registry for ${name}@${version}: ${out.trim().slice(0, 200)}`);
  }
  const response = await fetch(cargoIndexUrl(name), { headers: { Accept: "text/plain", "Cache-Control": "no-cache", "User-Agent": "rightkit-release" }, redirect: "error", signal: AbortSignal.timeout(30_000) });
  if (response.status === 404) return false;
  if (!response.ok) throw new Error(`crates.io index answered ${response.status} for ${name}`);
  return (await response.text()).split(/\r?\n/).filter((line) => line.trim()).some((line) => { try { return JSON.parse(line).vers === version; } catch { return false; } });
}

function defaultRun(command, args, options) {
  const settings = { encoding: "utf8", maxBuffer: 512 * 1024 * 1024, timeout: STEP_TIMEOUT_MS, windowsHide: true, ...options };
  return command === "cargo" ? runCargoSync(args, settings) : spawnSync(command, args, settings);
}

// Async twin used by batch mode so crates of one layer can overlap their uploads and index waits. The broker
// still runs builds one at a time; `await` on a synchronous fake works the same.
function defaultRunAsync(command, args, options) {
  const settings = { encoding: "utf8", maxBuffer: 512 * 1024 * 1024, timeout: STEP_TIMEOUT_MS, windowsHide: true, ...options };
  return command === "cargo" ? runCargoAsync(args, settings) : spawnCollect(command, args, settings);
}

/**
 * `execute` for runCargoPublish in batch mode: the async Cargo runner, with the output of a step that would
 * inherit stdio echoed line by line under the crate name (concurrent crates would otherwise interleave raw).
 */
export function batchCargoExecute(label, { run = runCargoAsync, out = process.stdout, err = process.stderr } = {}) {
  return async (_command, args, settings) => {
    const { stdio, ...rest } = settings;
    const result = await run(args, { ...rest, timeout: STEP_TIMEOUT_MS, maxBuffer: 512 * 1024 * 1024 });
    if (stdio === "inherit") {
      for (const [stream, text] of [[out, result.stdout], [err, result.stderr]]) {
        for (const line of String(text ?? "").split(/\r?\n/)) if (line) stream.write(`[${label}] ${line}\n`);
      }
    }
    return result;
  };
}

// Registry versions-file writers (recordPublished, stageSourceVersions) read-modify-write one shared JSON file
// and its consumer copies. They are synchronous, but the lock keeps the whole write one unit even if a writer
// becomes async, so concurrent crates of a layer can never interleave two updates.
let versionsTail = Promise.resolve();
const withVersionsLock = (task) => {
  const result = versionsTail.then(task);
  versionsTail = result.catch(() => {});
  return result;
};

async function defaultLoadWorkspaceModules(workspaceRoot) {
  const load = (file) => import(pathToFileURL(path.join(workspaceRoot, "scripts", "ci", file)).href);
  const [results, upload] = await Promise.all([load("test-results.mjs"), load("upload.mjs")]);
  return { results, checkNpmTarball: upload.checkNpmTarball };
}

/**
 * Feature sets that must at least compile, beyond the tested ones. A crate whose tested sets already
 * include `--all-features` needs none. `checkFeatureSets` (ci/gate.config.json) lists them explicitly for
 * crates with mutually exclusive features; otherwise one `--all-features` check runs. Mirrored in scripts/ci/gate.mjs.
 */
export function allFeatureCheckSets(entry = {}) {
  const tested = entry.featureSets ?? [[]];
  if (tested.some((features) => features.includes("--all-features"))) return [];
  return entry.checkFeatureSets ?? [["--all-features"]];
}

const CONSUMER_ROW_KEYS = new Set(["manifestPath", "args", "kind", "targets", "locked"]);
const CONSUMER_KINDS = new Set(["check"]);
const CONSUMER_MANIFEST = /^ci\/consumers\/(?:[A-Za-z0-9._-]+\/)+Cargo\.toml$/;
const CONSUMER_TARGET = /^[A-Za-z0-9_]+(?:-[A-Za-z0-9_.]+){2,3}$/;
// The row owns manifest, lock, target and output format; arguments may only choose features and target kinds.
// An ALLOWLIST (0.4.1 review A1): any token not listed is refused, so --help/-h/--version (a "passing" no-op),
// --target-dir, -C/--directory, --color, --timings, -Z and every other Cargo option stay out of a row.
// Keep in step with scripts/ci/gate.mjs.
const CONSUMER_ARG_FLAGS = new Set(["--no-default-features", "--all-features", "--lib", "--bins", "--tests", "--examples", "--benches", "--all-targets"]);
const CONSUMER_FEATURE_FLAGS = new Set(["--features", "-F"]);
const CONSUMER_FEATURE_LIST = /^[A-Za-z0-9_][A-Za-z0-9_.+/?:,-]*$/;
const CONSUMER_ARGS_ALLOWED = `--features <list> (or --features=<list>, -F <list>), ${[...CONSUMER_ARG_FLAGS].join(", ")}`;

/**
 * Validate the `consumerRows` of one crate's ci/gate.config.json entry and return them normalised. A consumer row
 * checks the owning crate through a SEPARATE Cargo workspace (its own Cargo.lock), run as
 * `cargo check --manifest-path <manifestPath> --locked [--target <t>] <args>`. Unknown fields are refused.
 */
export function consumerRows(unit, entry = {}) {
  const rows = entry.consumerRows;
  if (rows === undefined) return [];
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
export function consumerRowInvocations(row) {
  const targets = row.targets.length ? row.targets : [null];
  return targets.map((target) => ({ command: "cargo", args: [row.kind, "--manifest-path", row.manifestPath, "--locked", ...(target ? ["--target", target] : []), ...row.args], test: false }));
}

/**
 * Refuse a consumer row whose manifest or lock is missing from the workspace, is not tracked by git, or resolves
 * (through a symlink) to anywhere but its own path inside the workspace (fail closed before any command;
 * 0.4.1 review A5). The clean-tree check covers only the files git sees under the consumer directory, so a
 * symlinked or ignored file would otherwise be a checked file nobody reviewed.
 */
function assertConsumerFiles(workspaceRoot, unit, rows, git = defaultGit) {
  let realRoot;
  try { realRoot = fs.realpathSync(workspaceRoot); } catch { refuse(`${unit}: cannot resolve the workspace ${workspaceRoot}`); }
  for (const row of rows) {
    for (const file of [row.manifestPath, row.manifestPath.replace(/Cargo\.toml$/, "Cargo.lock")]) {
      const absolute = path.join(workspaceRoot, file);
      if (!fs.existsSync(absolute)) refuse(`${unit}: consumer row needs ${file}, which is not in the workspace`);
      let real;
      try { real = fs.realpathSync(absolute); } catch { refuse(`${unit}: consumer row file ${file} cannot be resolved`); }
      if (path.relative(realRoot, real) !== path.normalize(file)) refuse(`${unit}: consumer row file ${file} resolves outside its own path in the workspace (${real}); a symlink is not allowed`);
      if (git(["ls-files", "--error-unmatch", "--", file], workspaceRoot) === null) refuse(`${unit}: consumer row file ${file} is not tracked by git; commit it first`);
    }
  }
}

/** The directories whose cleanliness a set of consumer rows depends on. */
const consumerDirs = (rows) => [...new Set(rows.map((row) => path.posix.dirname(row.manifestPath)))];

// The files that decide WHICH steps a publish runs: an uncommitted edit would change the plan the evidence is bound to
// (0.4.1 review A3). Chosen over reading the HEAD blob so the publisher, the replay and the batch apply one rule:
// the file on disk must be the committed file, or the publish is refused.
export const GATE_CONFIG_FILES = Object.freeze(["ci/gate.config.json", "ci/asset-gated.json"]);

/** The CI gate's commands for one unit (scripts/ci/gate.mjs npmGate/rustGate), mirrored exactly. */
export function localTestPlan({ kind, unit, dir, config, manifest, batch = false, platform = "darwin" }) {
  if (kind === "npm") {
    if (!manifest?.scripts?.test) refuse(`${unit} has no test script; --local never publishes an untested package`);
    return {
      id: `npm:${dir}`, featureLabels: ["default"],
      steps: [
        { command: "pnpm", args: ["-r", "--workspace-concurrency=1", "run", "build"], test: false },
        { command: "pnpm", args: ["--dir", dir, "run", "test"], test: true },
      ],
    };
  }
  const entry = config?.rust?.[unit] ?? {};
  // gate.config.json names hosts "macos" and "windows" (scripts/ci/gate.mjs HOST).
  const gateHost = platform === "win32" ? "windows" : "macos";
  if (entry.os && !entry.os.includes(gateHost)) refuse(`${unit} is restricted to ${entry.os.join(", ")} by ci/gate.config.json; it cannot be qualified on this ${platform === "win32" ? "Windows host" : "Mac"}`);
  const featureSets = entry.featureSets ?? [[]];
  // Stream Cargo artifact records to the broker; libtest summaries remain plain text for ResultScanner.
  const steps = [];
  for (const features of featureSets) {
    steps.push({ command: "cargo", args: ["test", "-p", unit, "--message-format=json-render-diagnostics", ...features, "--no-fail-fast", "--", "--show-output"], test: true });
    if (entry.alsoSerial) steps.push({ command: "cargo", args: ["test", "-p", unit, "--message-format=json-render-diagnostics", ...features, "--no-fail-fast", "--", "--test-threads=1", "--show-output"], test: true });
  }
  // Batch mode lints the whole batch once (batchClippyInvocations) and records it as a batch receipt.
  // Every tested feature set is linted (-D warnings), not only the first.
  if (!batch) for (const features of featureSets) steps.push({ command: "cargo", args: ["clippy", "-p", unit, "--message-format=json-render-diagnostics", "--no-deps", "--all-targets", ...features, "--", "-D", "warnings"], test: false });
  for (const features of allFeatureCheckSets(entry)) steps.push({ command: "cargo", args: ["check", "-p", unit, "--message-format=json-render-diagnostics", "--all-targets", ...features], test: false });
  // Consumer rows (separate Cargo workspaces) gate only this crate: they come from this crate's own entry.
  const consumers = consumerRows(unit, entry);
  for (const row of consumers) steps.push(...consumerRowInvocations(row));
  return { id: `rust:${unit}`, featureLabels: featureSets.map((features) => features.join(" ") || "default"), consumerRows: consumers, steps };
}

/**
 * The clippy invocations that replace each crate's own `cargo clippy -p <crate> --no-deps --all-targets
 * <featureSets[0]> -- -D warnings`: same flags and lint level, so the same lints on the same code. Crates
 * whose first feature set is empty or `--all-features` share one `-p a -p b` invocation (the flags mean the
 * same for every crate); any other feature set runs per crate, because `--features x` must not reach a crate
 * that lacks x. The caller passes ONE dependency layer: no selected crate depends on another, so `-p` does not
 * unify extra features into a selected crate and each is linted with exactly the features it was before.
 * Crates with a different RIGHTKIT_REQUIRE_ASSETS (asset-gated) stay apart so the environment is unchanged.
 */
export function batchClippyInvocations(layer, { featuresOf = () => [], featureSetsOf = (unit) => [featuresOf(unit)], assetGatedOf = () => false } = {}) {
  const groups = new Map();
  for (const unit of layer) {
    // Every tested feature set of a crate is linted, in order; identical sets of one crate collapse to one run.
    const sets = featureSetsOf(unit) ?? [[]];
    const unique = sets.filter((set, index) => sets.findIndex((other) => JSON.stringify(other ?? []) === JSON.stringify(set ?? [])) === index);
    for (const set of unique) {
      const features = set ?? [];
      const shared = features.length === 0 || (features.length === 1 && features[0] === "--all-features");
      const key = shared ? JSON.stringify([features, assetGatedOf(unit)]) : `unit:${unit}:${JSON.stringify(features)}`;
      if (!groups.has(key)) groups.set(key, { crates: [], features, assetGated: assetGatedOf(unit) });
      groups.get(key).crates.push(unit);
    }
  }
  return [...groups.values()].map(({ crates, features, assetGated }) => ({
    crates, assetGated,
    args: ["clippy", ...crates.flatMap((unit) => ["-p", unit]), "--message-format=json-render-diagnostics", "--no-deps", "--all-targets", ...features, "--", "-D", "warnings"],
  }));
}

const sha256 = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const tarballName = (name, version) => `${name.replace(/^@/, "").replace("/", "-")}-${version}.tgz`;
const tail = (text, lines = 40) => String(text ?? "").split(/\r?\n/).slice(-lines).join("\n");

// Tests run under hosted-CI conditions: no inherited credentials (a managed host refuses a token from the
// environment) and no sibling product checkouts (CI holds rightkit alone).
function testEnv(deps, isAssetGated) {
  const inherited = Object.fromEntries(Object.entries(deps.env ?? process.env).filter(([key]) => !/(TOKEN|SECRET|PASSWORD|API_KEY|PRIVATE_KEY)/i.test(key)));
  return { ...inherited, RIGHTKIT_REQUIRE_ASSETS: isAssetGated ? "0" : "1", RIGHTKIT_ISOLATED_CHECKOUT: "1", CARGO_TERM_COLOR: "never" };
}

function assertLocalHost(deps, workspaceRoot, kind) {
  const platform = deps.platform ?? process.platform;
  if (!(deps.localBuildGranted ?? localBuildGranted)(workspaceRoot)) refuse(`--local needs the local-build grant: a committed ${LOCAL_BUILD_FILE} (allow "build-test-publish") naming this repository at ${workspaceRoot}`);
  if (!Object.hasOwn(LOCAL_HOST_POLICIES, platform)) refuse(`--local publishes from the Mac only (host policy: darwin-only) or from Windows (host policy: win32-only); this host is ${platform}`);
  // The npm path reads the registry token from the macOS Keychain and ships a Developer-ID-signed helper: Mac only.
  if (kind === "npm" && platform !== "darwin") refuse(`publish npm --local needs the macOS Keychain npm token and runs on the Mac only; Windows publishes crates (publish cargo --local); this host is ${platform}`);
  return platform;
}

// Host uploads are retired: only --qualify-only (no upload) runs. The upload engine below is kept for direct calls in
// tests; no CLI path reaches it (see host-upload-refusal.mjs).
/** One door: a publish engine entry (the CLI's `publish <kind> --local`, or a direct call of the engine) runs only inside a
 * live rightkit-gate door run for the workspace's HEAD (RK_GATE_RUN; gate-stages requireDoorRun). The unit stages
 * (compileLocalUnit and its successors) never pass through here: they are the gate's own path and carry their own door. */
export function requireEngineDoor(parsed, { env = process.env, git = defaultGit, command = "right-release publish --local" } = {}) {
  const root = parsed.workspaceRoot;
  const common = git(["rev-parse", "--git-common-dir"], root);
  const commit = git(["rev-parse", "HEAD"], root);
  if (!common || !/^[0-9a-f]{40}$/.test(commit ?? "")) refuse(`${command} runs only inside a rightkit-gate door run of a git checkout (no HEAD or common dir in ${root})`);
  const denial = requireDoorRun({ env, commonDir: path.resolve(root, common), commit, command });
  if (denial) refuse(denial);
  return { commit, commonDir: path.resolve(root, common) };
}

export async function runLocalPublish(kind, argv, deps = {}) {
  const parsed = parseLocalPublishArgs(kind, argv, deps.cwd);
  if (!parsed.qualifyOnly) refuseHostUpload(`right-release publish ${kind} --local`);
  return runLocalPublishUploadEngine(kind, argv, deps);
}

export async function runLocalPublishUploadEngine(kind, argv, deps = {}) {
  const parsed = parseLocalPublishArgs(kind, argv, deps.cwd);
  (deps.requireDoor ?? requireEngineDoor)(parsed, { env: deps.env ?? process.env, command: `right-release publish ${kind} --local` });
  if (parsed.batch) return runLocalPublishBatch(parsed, deps);
  return publishUnit(parsed, deps, null);
}

// Stage state never leaves this process: persisted receipts are reports, not authority to skip unit tests.
const unitStages = new WeakMap();
function assertUnitDoor(root, stage, env, identity) {
  const commit = defaultGit(["rev-parse", "HEAD"], root);
  const tree = defaultGit(["rev-parse", "HEAD^{tree}"], root);
  const common = defaultGit(["rev-parse", "--git-common-dir"], root);
  if (!/^[0-9a-f]{40}$/.test(commit ?? "") || !/^[0-9a-f]{40}$/.test(tree ?? "") || !common) refuse("cannot bind unit stage to HEAD and tree");
  if (identity && (identity.commit !== commit || identity.tree !== tree)) refuse("unit stage HEAD or tree changed");
  const dirty = defaultGit(["status", "--porcelain", "--untracked-files=no"], root);
  if (dirty === null || dirty !== "") refuse("unit stage requires a clean tracked checkout");
  const commonDir = path.resolve(root, common);
  const denial = requireDoorRun({ env, commonDir, commit, command: `unit ${stage}` });
  if (denial) refuse(denial);
  let run;
  try { run = JSON.parse(fs.readFileSync(path.join(gateStateDir(commonDir), "runs", `${env.RK_GATE_RUN}.json`), "utf8")); } catch { refuse("cannot read the unit door run"); }
  if (run.stage !== stage || run.tree !== tree) refuse(`unit ${stage} needs its own live door run for this tree`);
  return { commit, tree };
}

export async function compileLocalUnit(kind, argv, { env = process.env } = {}) {
  const parsed = parseLocalPublishArgs(kind, argv);
  if (!parsed.qualifyOnly || parsed.batch) refuse("unit stages require one --qualify-only unit and a new bundle");
  const identity = assertUnitDoor(parsed.workspaceRoot, "compile", env);
  const runner = publishUnitStages(parsed, { env }, null);
  try {
    const step = await runner.next();
    if (step.done || step.value?.stage !== "compile") refuse("unit compile did not complete");
    assertUnitDoor(parsed.workspaceRoot, "compile", env, identity);
    const state = Object.freeze({});
    unitStages.set(state, { runner, parsed, identity, stage: "compile", compileRequests: [...(step.value.brokerRequests ?? [])] });
    return state;
  } catch (error) { await runner.return(); throw error; }
}

async function advanceLocalUnit(state, expected, stage, env) {
  const entry = unitStages.get(state);
  if (!entry || entry.stage !== expected) refuse(`unit ${stage} requires this process's completed ${expected} stage`);
  entry.stage = "running";
  try {
    assertUnitDoor(entry.parsed.workspaceRoot, stage, env, entry.identity);
    const step = await entry.runner.next(env);
    if (stage === "package" ? step.done || step.value?.stage !== "package" : !step.done || !step.value?.qualifiedOnly) refuse(`unit ${stage} did not complete`);
    assertUnitDoor(entry.parsed.workspaceRoot, stage, env, entry.identity);
    entry.stage = stage;
    if (stage === "qualify") unitStages.delete(state);
    return step.value;
  } catch (error) { unitStages.delete(state); await entry.runner.return(); throw error; }
}

export const packageLocalUnit = (state, { env = process.env } = {}) => advanceLocalUnit(state, "compile", "package", env);
export const qualifyLocalUnit = (state, { env = process.env } = {}) => advanceLocalUnit(state, "package", "qualify", env);
/** The broker request ids of this process's completed compile stage (the package stage's ride on its result). */
export const localUnitCompileRequests = (state) => [...(unitStages.get(state)?.compileRequests ?? [])];
export async function closeLocalUnit(state) {
  const entry = unitStages.get(state);
  if (entry) { unitStages.delete(state); await entry.runner.return(); }
}

/**
 * Qualify and publish ONE unit. `batch` (cargo batch mode only) is `{ head, tree, receipt, commands }`: the
 * batch-level fmt, secret scan and clippy already ran at `head`/`tree`, so this unit skips its own clippy and
 * copies the receipt into its evidence; everything else (tests, all-features check, packaging, the gate, the
 * publish and the registry readback) runs for this unit exactly as in single mode.
 */
async function publishUnit(parsed, deps, batch) {
  const runner = publishUnitStages(parsed, deps, batch);
  let result;
  do { result = await runner.next(); } while (!result.done);
  return result.value;
}

async function* publishUnitStages(parsed, deps, batch) {
  const log = deps.log ?? ((line) => process.stdout.write(`${line}\n`));
  const { kind, unit, allowIgnored, workspaceRoot } = parsed;
  const git = deps.git ?? defaultGit;
  const readJson = deps.readJson ?? defaultReadJson;
  // Every broker job a stage launches prints `rustright: request <id>`; the unit gate fetches those jobs' signed receipts, so the ids
  // are collected here per stage and handed over at each stage yield (compile, then package). Nothing about what runs changes.
  const brokerRequests = [];
  const noteBrokerRequests = (result) => { for (const [, id] of `${result?.stdout ?? ""}\n${result?.stderr ?? ""}`.matchAll(/^rustright: request ([0-9a-f-]{36})/gm)) if (!brokerRequests.includes(id)) brokerRequests.push(id); return result; };
  const launch = deps.run ?? (batch ? defaultRunAsync : defaultRun);
  const run = (...args) => { const result = launch(...args); return typeof result?.then === "function" ? result.then(noteBrokerRequests) : noteBrokerRequests(result); };

  // 1. Preconditions, fail closed.
  const platform = assertLocalHost(deps, workspaceRoot, kind);
  if (parsed.qualifyOnly && fs.existsSync(parsed.bundleDir)) refuse(`qualification bundle already exists: ${parsed.bundleDir}`);
  const dir = kind === "npm" ? `packages/${unit.slice("@rightkit/".length)}` : `crates/${unit}`;
  const unitPath = path.join(workspaceRoot, dir);
  if (!fs.existsSync(unitPath)) refuse(`unit directory not found: ${unitPath}`);
  let manifest = null; let version;
  if (kind === "npm") {
    manifest = readJson(path.join(unitPath, "package.json")) ?? refuse(`cannot read ${dir}/package.json`);
    if (manifest.name !== unit) refuse(`${dir}/package.json is ${manifest.name}, not ${unit}`);
    if (manifest.private === true) refuse(`${unit} is private and is never published`);
    version = manifest.version;
  } else {
    const pkg = (deps.readCargoPackage ?? readCargoPackage)(unitPath);
    if (pkg.name !== unit) refuse(`Cargo.toml [package].name ${JSON.stringify(pkg.name)} differs from --crate ${unit}`);
    version = pkg.version;
  }
  if (typeof version !== "string" || !version.trim()) refuse(`${unit} has no literal version`);
  const head = git(["rev-parse", "HEAD"], workspaceRoot);
  if (!head || !/^[0-9a-f]{40}$/.test(head)) refuse("cannot resolve HEAD");
  if (batch) {
    // The batch receipt is valid for one commit and tree only; a moved HEAD would make it describe other code.
    if (head !== batch.head) refuse(`${unit}: HEAD moved to ${head} during the batch (batch commit ${batch.head})`);
    const tree = git(["rev-parse", "HEAD^{tree}"], workspaceRoot);
    if (tree !== batch.tree) refuse(`${unit}: HEAD tree ${tree} differs from the batch tree ${batch.tree}`);
  }
  const feedPath = "packages/release/rightkit-versions.json";
  const guardedPaths = [dir, ...GATE_CONFIG_FILES, ...(parsed.qualifyOnly && kind === "cargo" ? ["Cargo.toml", "Cargo.lock"] : []), ...(parsed.qualifyOnly && kind === "npm" ? [feedPath] : [])];
  const dirty = git(["status", "--porcelain", "--", ...guardedPaths], workspaceRoot);
  if (dirty === null) refuse(`cannot read git status for ${dir}`);
  if (dirty !== "") refuse(`tree is not clean for ${dir} or the gate config (${GATE_CONFIG_FILES.join(", ")}); commit first:\n${dirty}`);
  if (parsed.qualifyOnly && kind === "npm") {
    const feedFile = path.join(workspaceRoot, feedPath);
    if (unit === "@rightkit/release" && !fs.existsSync(feedFile)) refuse(`${unit}: committed ${feedPath} is required before packing`);
    if (fs.existsSync(feedFile)) {
      const committedFeed = git(["show", `HEAD:${feedPath}`], workspaceRoot);
      if (committedFeed === null || committedFeed !== fs.readFileSync(feedFile, "utf8").trim()) refuse(`${feedPath} must match the committed HEAD before qualification`);
      const feed = readJson(feedFile) ?? refuse(`cannot read ${feedPath}`);
      if (feed.stagedNpm?.[unit] !== version && feed.registry?.npm?.[unit] !== version) refuse(`${unit}@${version}: stage the version in ${feedPath} and commit it before qualification`);
    }
  }
  if (await (deps.registryHas ?? defaultRegistryHas)({ kind, name: unit, version })) refuse(`${unit}@${version} is already on the ${kind === "npm" ? "npm" : "crates.io"} registry; bump the version`);
  log(`[right-release] local publish ${kind}:${unit}@${version} at ${head.slice(0, 12)} (local-build grant, ${LOCAL_HOST_POLICIES[platform]} policy)`);

  // 2. The unit's own tests, exactly as the CI gate defines them.
  const gateConfig = readJson(path.join(workspaceRoot, "ci", "gate.config.json")) ?? {};
  const assetGated = readJson(path.join(workspaceRoot, "ci", "asset-gated.json")) ?? {};
  const gatedSection = assetGated[kind === "npm" ? "npm" : "crates"] ?? {};
  const isAssetGated = Object.hasOwn(gatedSection, unit);
  const plan = localTestPlan({ kind, unit, dir, config: gateConfig, manifest, batch: Boolean(batch), platform });
  if (plan.consumerRows?.length) {
    // The consumer workspaces are part of the qualified commit: present, and unmodified like the unit itself.
    assertConsumerFiles(workspaceRoot, unit, plan.consumerRows, git);
    const consumerDirty = git(["status", "--porcelain", "--", ...consumerDirs(plan.consumerRows)], workspaceRoot);
    if (consumerDirty === null) refuse(`cannot read git status for the consumer rows of ${unit}`);
    if (consumerDirty !== "") refuse(`tree is not clean for the consumer rows of ${unit}; commit first:\n${consumerDirty}`);
  }
  if (parsed.qualifyOnly && kind === "cargo") {
    const violations = (deps.scanSecrets ?? scanCrateForSecrets)(unitPath);
    if (violations.length) refuse(`qualification secret scan rejected: ${violations.join(", ")}`);
    plan.steps.unshift({ command: "cargo", args: qualifyFmtArgs(unit), test: false });
  }
  const modules = await (deps.loadWorkspaceModules ?? defaultLoadWorkspaceModules)(workspaceRoot);
  const { ResultScanner, TEST_COMMAND, judgeUnit } = modules.results;
  const env = testEnv(deps, isAssetGated);
  // Ignored tests are refused unless the unit is named: --allow-ignored / --allow-ignored-units, or a
  // written reason in ci/gate.config.json (rust.<crate>.allowIgnored). The count stays in the evidence.
  const configReason = kind === "cargo" && typeof gateConfig?.rust?.[unit]?.allowIgnored === "string" ? gateConfig.rust[unit].allowIgnored.trim() : "";
  const ignoredAllowed = Boolean(allowIgnored || parsed.allowIgnoredUnits?.includes(unit) || configReason);
  const steps = parsed.qualifyOnly && kind === "cargo" ? [{ command: `scanCrateForSecrets ${dir}`, ok: true }] : [];
  for (const step of plan.steps) {
    const commandText = `${step.command} ${step.args.join(" ")}`;
    log(`[right-release] ${plan.id}: ${commandText}`);
    const result = await run(step.command, step.command === "cargo" ? withCargoJobs(step.args, parsed.jobs) : step.args, { cwd: workspaceRoot, env });
    const row = { command: commandText, ok: false, isTest: step.test && TEST_COMMAND.test(commandText) };
    if (result.error || result.status !== 0) {
      steps.push(row);
      refuse(`${plan.id} step failed (${result.error?.message ?? `exit ${result.status}`}): ${commandText}\nstdout:\n${result.stdout ?? ""}\nstderr:\n${result.stderr ?? ""}`);
    }
    row.ok = true;
    if (row.isTest) {
      const out = new ResultScanner(); const err = new ResultScanner();
      out.push(result.stdout ?? ""); err.push(result.stderr ?? "");
      const a = out.finish(); const b = err.finish();
      Object.assign(row, { executed: a.executed + b.executed, failed: a.failed + b.failed, ignored: a.ignored + b.ignored, skipped: a.skipped + b.skipped, binaries: a.binaries + b.binaries });
    }
    steps.push(row);
  }
  const verdict = judgeUnit({ name: unit, steps: steps.map((s) => ({ ...s, unit: plan.id })), allowIgnored: ignoredAllowed ? [unit] : [], allowMissingAssets: isAssetGated ? [unit] : [] });
  if (verdict.status === "fail" || steps.some((s) => s.failed > 0)) refuse(`${plan.id} not qualified: ${verdict.problems.join("; ") || "failed tests reported"}`);
  if (!(verdict.executed > 0)) refuse(`${plan.id} executed no test`);
  log(`[right-release] ${plan.id}: ${verdict.executed} passed, 0 failed, ${verdict.skipped} skipped, ${verdict.ignored} ignored${isAssetGated ? " (asset-gated: RIGHTKIT_REQUIRE_ASSETS=0, skips recorded)" : ""}`);
  const packageEnv = yield { stage: "compile", kind, name: unit, version, commit: head, brokerRequests: brokerRequests.splice(0) };
  if (packageEnv) Object.assign(env, testEnv({ env: packageEnv }, isAssetGated));

  // 3. Pack the artifact and hash it.
  const scratch = fs.mkdtempSync(path.join(deps.tmpdir ?? os.tmpdir(), "rk-local-publish-"));
  const cleanup = () => fs.rmSync(scratch, { recursive: true, force: true });
  try {
    let artifactFile; const packCommands = [];
    let packDir = dir;
    if (kind === "npm" && unit === SWAP_HELPER_PACKAGE) {
      if (parsed.qualifyOnly) {
        // Signing writes its binary and receipt. Keep both in the pack copy, never the source checkout.
        packDir = path.join(scratch, "package-source");
        fs.cpSync(unitPath, packDir, { recursive: true, filter: (source) => path.basename(source) !== "node_modules" });
      }
      // #258: the package ships a Developer-ID-signed bin/darwin/rightkit-swap; build, sign and verify it before packing (fails closed).
      try { (deps.prepareSwapHelper ?? prepareSwapHelper)({ workspaceRoot, packageDir: parsed.qualifyOnly ? packDir : unitPath, platform, run, env, log }); }
      catch (error) { refuse(`${error.code ? `[${error.code}] ` : ""}${error.message}`); }
      packCommands.push(`rustright cargo ${SWAP_HELPER_CARGO_ARGS.join(" ")} && codesign --verify --strict bin/darwin/rightkit-swap`);
    }
    if (kind === "npm") {
      // The feed a tarball ships must list the version it ships with (#319): stage it before packing.
      const feedFile = path.join(workspaceRoot, "packages", "release", "rightkit-versions.json");
      if (!parsed.qualifyOnly && fs.existsSync(feedFile)) await withVersionsLock(() => (deps.stageSourceVersions ?? stageSourceVersions)([{ registry: "npm", name: unit, version }], { file: feedFile, root: workspaceRoot, log }));
      const args = npmPackArgs(packDir, scratch);
      packCommands.push(`pnpm ${args.join(" ")}`);
      const packed = await run("pnpm", args, { cwd: workspaceRoot, env });
      if (packed.error || packed.status !== 0) refuse(`pnpm pack failed for ${unit}\n${tail(`${packed.stdout ?? ""}\n${packed.stderr ?? ""}`)}`);
      artifactFile = path.join(scratch, tarballName(unit, version));
      if (!fs.existsSync(artifactFile)) refuse(`pnpm pack did not produce ${path.basename(artifactFile)}`);
      const checked = modules.checkNpmTarball({ file: artifactFile, name: unit, version, dir, spawn: deps.spawnSync ?? spawnSync });
      if (checked.problems.length) refuse(`${unit}: packed tarball refused:\n  - ${checked.problems.join("\n  - ")}`);
    } else {
      // A combined publish verifies once in `cargo publish`; detached qualification verifies here before
      // its evidence leaves this host, so the publishing host can reuse that exact archive's proof.
      const packageArgs = cargoPackageArgs(unit, parsed.qualifyOnly);
      packCommands.push(`cargo ${packageArgs.join(" ")}`);
      const packed = await run("cargo", withCargoJobs(packageArgs, parsed.jobs), { cwd: workspaceRoot, env });
      if (packed.error || packed.status !== 0) refuse(`cargo package failed for ${unit}\n${tail(`${packed.stdout ?? ""}\n${packed.stderr ?? ""}`)}`);
      if (parsed.qualifyOnly) steps.push({ command: `cargo ${packageArgs.join(" ")}`, ok: true });
      const metadataArgs = [...CARGO_METADATA_ARGS];
      const meta = await runCargoReadOnly(metadataArgs, { cwd: workspaceRoot, env }, (_command, args, settings) => run("cargo", args, settings),
        (failed) => log(`[right-release] offline Cargo metadata missed cache; retrying online once\n${failed.stdout ?? ""}${failed.stderr ? `\n${failed.stderr}` : ""}`));
      const actualMetadataArgs = meta.usedOfflineFallback ? metadataArgs.filter((arg) => arg !== "--offline") : metadataArgs;
      packCommands.push(`cargo ${actualMetadataArgs.join(" ")}`);
      let targetDir;
      try { targetDir = JSON.parse(meta.stdout).target_directory; } catch { /* refused below */ }
      if (meta.error || meta.status !== 0 || typeof targetDir !== "string" || !path.isAbsolute(targetDir)) {
        refuse(`cannot read the Cargo target directory\n${tail(`${meta.error?.message ?? ""}\n${meta.stdout ?? ""}\n${meta.stderr ?? ""}`)}`);
      }
      artifactFile = path.join(targetDir, "package", `${unit}-${version}.crate`);
      if (!fs.existsSync(artifactFile)) refuse(`cargo package did not produce ${artifactFile}`);
    }
    const artifactSha256 = sha256(fs.readFileSync(artifactFile));
    if (parsed.qualifyOnly && (git(["rev-parse", "HEAD"], workspaceRoot) !== head || git(["status", "--porcelain", "--", ...guardedPaths], workspaceRoot) !== "")) refuse("qualification inputs changed while preparing the artifact; no bundle was created");
    if (parsed.qualifyOnly) {
      fs.mkdirSync(parsed.bundleDir, { recursive: false });
      const bundledArtifact = path.join(parsed.bundleDir, path.basename(artifactFile));
      fs.copyFileSync(artifactFile, bundledArtifact, fs.constants.COPYFILE_EXCL);
      if (sha256(fs.readFileSync(bundledArtifact)) !== artifactSha256) refuse("qualification archive changed while copying");
      artifactFile = bundledArtifact;
      yield { stage: "package", kind, name: unit, version, commit: head, artifactFile, artifactSha256, brokerRequests: brokerRequests.splice(0) };
      if (sha256(fs.readFileSync(artifactFile)) !== artifactSha256) refuse("qualification archive changed after packaging");
    }

    // 4. Evidence in the normal schema, then the EXISTING gate.
    const commands = [...plan.steps.map((s) => `${s.command} ${s.args.join(" ")}`), ...packCommands, ...(batch ? batch.commands : [])];
    const timestamp = (deps.now ?? (() => new Date().toISOString()))();
    const evidence = {
      schema: GATE_SCHEMA, kind, name: unit, version, commit: head, artifactSha256,
      hostPolicy: LOCAL_HOST_POLICIES[platform], hostPolicyReason: localHostPolicyReason(platform),
      runs: [{
        host: { os: platform, arch: deps.arch ?? process.arch }, commit: head, features: plan.featureLabels, artifactSha256,
        command: commands.join(" && "), operator: LOCAL_OPERATOR, timestamp,
        passed: verdict.executed, failed: 0, skipped: verdict.skipped, ignored: verdict.ignored, allowIgnored: ignoredAllowed,
        ...(configReason ? { allowIgnoredReason: configReason } : {}),
        requireAssets: env.RIGHTKIT_REQUIRE_ASSETS, assetGated: isAssetGated,
        ...(parsed.jobs === undefined ? {} : { jobs: parsed.jobs }),
        ...(batch ? { batch: structuredClone(batch.receipt) } : {}),
        steps: steps.map(({ command, ok, executed, failed, ignored, skipped, binaries }) => ({ command, ok, ...(executed === undefined ? {} : { executed, failed, ignored, skipped, binaries }) })),
      }],
    };
    // A qualification writes its evidence beside the bytes, in the bundle: the tracked release-evidence/gate file is the record of a
    // host publish, and rewriting it would dirty the checkout the unit door guards (the first `--to release --unit` run stopped
    // there, 2026-10-27). Never overwrite a bundle's evidence.
    const evidencePath = parsed.qualifyOnly ? path.join(parsed.bundleDir, "gate.json") : path.resolve(defaultEvidencePath(workspaceRoot, kind, unit));
    fs.mkdirSync(path.dirname(evidencePath), { recursive: true });
    fs.writeFileSync(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, { flag: parsed.qualifyOnly ? "wx" : "w" });
    log(`[right-release] wrote ${evidencePath} (artifact sha256 ${artifactSha256})`);
    const gateOptions = { kind, name: unit, version, packagePath: unitPath, evidencePath, artifactBytes: fs.readFileSync(artifactFile) };
    (deps.enforceGate ?? enforcePublicationGate)(gateOptions, { write: (line) => log(line.replace(/\n$/, "")) });

    if (parsed.qualifyOnly) {
      // Bytes and evidence stay together outside the checkout.
      const bundledEvidence = evidencePath;
      log(`[right-release] qualified ${unit}@${version}; bundle ${parsed.bundleDir}${kind === "cargo" ? `; publish cargo --crate ${unit} --from-evidence ${bundledEvidence}` : ""}`);
      return { kind, name: unit, version, commit: head, artifactSha256, evidencePath: bundledEvidence, artifactFile, qualifiedOnly: true };
    }

    // 5. The EXISTING publish path.
    if (kind === "npm") {
      (deps.publishGatedTarball ?? publishGatedTarball)({ name: unit, version, tarball: artifactFile, evidencePath, workspaceRoot }, deps.publishDeps ?? {});
    } else {
      // trustEvidenceSteps: the evidence just written (and validated by the gate above) proves this commit's tests
      // and clippy, so the publisher does not repeat them; verifyOnPublish: Cargo's verify build runs in the
      // publish itself. Package contents, the runtime budget, the archive hash and the readback always run.
      const execute = deps.cargoExecute ?? (batch ? batchCargoExecute(unit) : undefined);
      const published = await (deps.runCargoPublish ?? runCargoPublish)({ crate: unit, dryRun: false, allowDirty: false, workspaceRoot, evidence: evidencePath, trustEvidenceSteps: true, verifyOnPublish: true, offlineReadOnly: true }, execute, deps.cargoDeps ?? {});
      await withVersionsLock(() => (deps.recordPublished ?? recordPublished)({ registry: "cargo", name: published.name, version: published.version }, workspaceFeedOptions(workspaceRoot)));
    }
    log(`[right-release] published ${unit}@${version}`);
    return { kind, name: unit, version, commit: head, artifactSha256, evidencePath };
  } finally {
    cleanup();
  }
}

/**
 * Batch publish (cargo only): validate the whole batch and run fmt, the secret scan and clippy once at one
 * commit and tree, failing the batch closed before anything is uploaded; then publish layer by layer, the
 * crates of a layer concurrently (bounded by --concurrency). A failure stops new work: no crate of a later
 * layer starts, crates of the failing layer that were already running finish, and the refusal lists what
 * published. Re-run with the published crates removed from the list.
 */
async function runLocalPublishBatch(parsed, deps) {
  const log = deps.log ?? ((line) => process.stdout.write(`${line}\n`));
  const { workspaceRoot, concurrency } = parsed;
  const git = deps.git ?? defaultGit;
  const readJson = deps.readJson ?? defaultReadJson;
  const run = deps.run ?? defaultRunAsync;
  const platform = assertLocalHost(deps, workspaceRoot, "cargo");

  // 1. The crate list and its dependency layers.
  const requested = parsed.batch.file ? parseCrateLayers((deps.readText ?? ((file) => fs.readFileSync(file, "utf8")))(parsed.batch.file)) : [parsed.batch.crates];
  const crates = requested.flat();
  const dependenciesOf = deps.batchDependencies ?? ((unit) => {
    const members = new Set(crates);
    return pathDependencyDirs(path.join(workspaceRoot, "crates", unit)).map((dir) => path.basename(dir)).filter((name) => members.has(name) && name !== unit);
  });
  const layers = parsed.batch.file ? checkCrateLayers(requested, dependenciesOf) : deriveCrateLayers(crates, dependenciesOf);

  // 2. Preconditions for every crate before anything runs, fail closed.
  const gateConfig = readJson(path.join(workspaceRoot, "ci", "gate.config.json")) ?? {};
  const assetGated = readJson(path.join(workspaceRoot, "ci", "asset-gated.json")) ?? {};
  const isAssetGated = (unit) => Object.hasOwn(assetGated.crates ?? {}, unit);
  const versions = new Map();
  const batchConsumerDirs = new Set();
  for (const unit of crates) {
    const dir = `crates/${unit}`;
    if (!fs.existsSync(path.join(workspaceRoot, dir))) refuse(`unit directory not found: ${path.join(workspaceRoot, dir)}`);
    const pkg = (deps.readCargoPackage ?? readCargoPackage)(path.join(workspaceRoot, dir));
    if (pkg.name !== unit) refuse(`Cargo.toml [package].name ${JSON.stringify(pkg.name)} differs from --crates entry ${unit}`);
    if (typeof pkg.version !== "string" || !pkg.version.trim()) refuse(`${unit} has no literal version`);
    versions.set(unit, pkg.version);
    // Refuses an OS-restricted crate, a malformed consumer row, or an untracked or symlinked consumer file now, not mid-batch.
    const rows = localTestPlan({ kind: "cargo", unit, dir, config: gateConfig, batch: true, platform }).consumerRows ?? [];
    assertConsumerFiles(workspaceRoot, unit, rows, git);
    for (const consumerDir of consumerDirs(rows)) batchConsumerDirs.add(consumerDir);
  }
  const head = git(["rev-parse", "HEAD"], workspaceRoot);
  if (!head || !/^[0-9a-f]{40}$/.test(head)) refuse("cannot resolve HEAD");
  const tree = git(["rev-parse", "HEAD^{tree}"], workspaceRoot);
  if (!tree || !/^[0-9a-f]{40}$/.test(tree)) refuse("cannot resolve the HEAD tree");
  // Every batch crate, the workspace manifest and lock, the gate config and EVERY crate's consumer directories must
  // match the commit the batch checks are bound to, before the first layer publishes (0.4.1 review A3, A4).
  const dirty = git(["status", "--porcelain", "--", ...crates.map((unit) => `crates/${unit}`), "Cargo.toml", "Cargo.lock", ...GATE_CONFIG_FILES, ...batchConsumerDirs], workspaceRoot);
  if (dirty === null) refuse("cannot read git status for the batch");
  if (dirty !== "") refuse(`tree is not clean for the batch crates, the workspace manifest, the gate config or a consumer directory; commit first:\n${dirty}`);
  const registryHas = deps.registryHas ?? defaultRegistryHas;
  const published = (await Promise.all(crates.map(async (unit) => ((await registryHas({ kind: "cargo", name: unit, version: versions.get(unit) })) ? `${unit}@${versions.get(unit)}` : null)))).filter(Boolean);
  if (published.length) refuse(`already on the crates.io registry: ${published.join(", ")}; bump the version or remove them from the batch`);
  log(`[right-release] local publish cargo batch: ${crates.length} crate(s) in ${layers.length} layer(s) at ${head.slice(0, 12)}, concurrency ${concurrency} (local-build grant, ${LOCAL_HOST_POLICIES[platform]} policy)`);

  // 3. Batch-level checks, once, at the batch commit. Each is recorded in every crate's evidence.
  const env = testEnv(deps, false);
  const stamp = { commit: head, tree };
  const checks = []; const commands = [];
  const batchCheck = async (name, command, args, crateNames, runEnv = env) => {
    const text = `${command} ${args.join(" ")}`;
    log(`[right-release] batch ${name}: ${text}`);
    const result = await run(command, args, { cwd: workspaceRoot, env: runEnv });
    if (result.error || result.status !== 0) refuse(`batch ${name} failed (${result.error?.message ?? `exit ${result.status}`}): ${text}\n${tail(`${result.stdout ?? ""}\n${result.stderr ?? ""}`)}`);
    checks.push({ name, command: text, ok: true, ...stamp, ...(crateNames ? { crates: crateNames } : {}) });
    commands.push(text);
  };
  await batchCheck("fmt", "cargo", ["fmt", "--all", "--check"]);
  log("[right-release] batch secret-scan: crate directories of the batch");
  const scan = deps.scanSecrets ?? scanCrateForSecrets;
  const violations = crates.flatMap((unit) => scan(path.join(workspaceRoot, "crates", unit)).map((file) => `${unit}: ${file}`));
  if (violations.length) refuse(`batch secret scan rejected: ${violations.join(", ")}`);
  const scanText = `scanCrateForSecrets ${crates.map((unit) => `crates/${unit}`).join(" ")}`;
  checks.push({ name: "secret-scan", command: scanText, ok: true, ...stamp, crates });
  commands.push(scanText);
  const featureSetsOf = (unit) => gateConfig?.rust?.[unit]?.featureSets ?? [[]];
  for (const layer of layers) {
    for (const invocation of batchClippyInvocations(layer, { featureSetsOf, assetGatedOf: isAssetGated })) {
      await batchCheck("clippy", "cargo", invocation.args, invocation.crates, testEnv(deps, invocation.assetGated));
    }
  }
  const receipt = { id: `batch-${head.slice(0, 12)}-${sha256(JSON.stringify(layers)).slice(0, 8)}`, ...stamp, crates, checks };

  // 4. Publish layer by layer. A crate's publish returns only after its registry readback proved the exact
  // checksum visible in the index, so layer N+1 starts only once every layer-N dependency is visible.
  const batch = { head, tree, receipt, commands };
  const results = [];
  for (const [index, layer] of layers.entries()) {
    log(`[right-release] batch layer ${index + 1}/${layers.length}: ${layer.join(", ")}`);
    const queue = [...layer]; const failures = [];
    const worker = async () => {
      while (!failures.length && queue.length) {
        const unit = queue.shift();
        try { results.push(await publishUnit({ ...parsed, kind: "cargo", unit, batch: undefined }, deps, batch)); }
        catch (error) { failures.push({ unit, message: error.message }); }
      }
    };
    await Promise.all(Array.from({ length: Math.min(concurrency, layer.length) }, worker));
    if (failures.length) {
      const done = results.map((r) => `${r.name}@${r.version}`);
      refuse(`batch stopped in layer ${index + 1}/${layers.length}; published so far: ${done.join(", ") || "none"}\n${failures.map((f) => `- ${f.unit}: ${f.message}`).join("\n")}`);
    }
  }
  log(`[right-release] batch published ${results.length} crate(s): ${results.map((r) => `${r.name}@${r.version}`).join(", ")}`);
  return { kind: "cargo", batch: true, commit: head, tree, results };
}

const realOrSelf = (file) => { try { return fs.realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  const [kind, ...rest] = process.argv.slice(2);
  if (kind !== "npm" && kind !== "cargo") { process.stderr.write("right-release publish --local: expected npm or cargo\n"); process.exitCode = 2; }
  else runLocalPublish(kind, rest).catch((error) => { process.stderr.write(`right-release publish ${kind} --local: ${error.message}\n`); process.exitCode = 1; });
}
