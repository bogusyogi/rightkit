#!/usr/bin/env node
// rk-pipeline: the publication loop, in the order Adrian specified (2026-10-22): one entry through the RustRight broker,
// deterministic checks first, warm local compile of each unit before any hosted run, hosted qualification of many units
// in parallel, direct upload on green, consumers released as their dependencies reach the registry.
//
//   rk-pipeline.mjs ready        [--registry crates|npm]           units whose version is not on the registry, in dependency waves
//   rk-pipeline.mjs preflight    <unit...> [--jobs N]               rk-preflight each unit, N at a time, through the broker
//   rk-pipeline.mjs canary       @rightkit/release                 the pipeline canary (candidate vs registry baseline doctor, both hosts); `loop` runs it before pre-flight
//   rk-pipeline.mjs dispatch     <unit...> [--registry crates|npm]  hosted qualification, only with a green receipt for HEAD, HEAD on origin
//   rk-pipeline.mjs watch        [--wait]                          hosted run state per dispatched unit; upload and registry readback on green
//   rk-pipeline.mjs loop         [--registry crates|npm|all] [--jobs N]  ready -> preflight -> dispatch -> watch; `all` runs crates and npm units in ONE coordinator, so npm never queues behind crates
// Every compile and every dispatch goes through the broker. gh is used read-only, by absolute path, to watch runs.
//
// GB-07 (BROKER-CONTRACT section 16): this module derives NO root from its own location. Every function takes the workspace root (the
// control-plane checkout whose HEAD, units and policy it judges) as a parameter, `runPipeline` is the in-gate-process entry (no child
// process from a path under the checkout), and the CLI wrapper at the foot of the file is the only place the root is taken from where the
// script lives, which preserves the command line exactly as it was.
import { spawn, spawnSync } from "node:child_process";
import { AsyncLocalStorage } from "node:async_hooks";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { DEFAULT_OUT, readJson, receiptRefusal, exceptionAllowsets } from "./rk-preflight.mjs";
import { packageRows, readPackages as readNpmPackages } from "./rk-npm-preflight.mjs";
import { brokerClientInvocation } from "../../packages/release/broker-client.mjs";
import { processProbe } from "../../packages/release/build-ownership.mjs";
import { controlPlaneProblem } from "../../packages/release/control-plane.mjs";
import { resolveManagedRoots } from "../../packages/release/managed-roots.mjs";

export const REPO = "bogusyogi/rightkit";
/** The coordinator's state file for a host platform (the managed volume's scratch: a Mac volume, a Windows D: drive). */
export const stateFileFor = (platform = process.platform) => (platform === "win32" ? "D:\\scratch\\rk-pipeline\\state.json" : "/Volumes/D/scratch/rk-pipeline/state.json");
export const STATE_FILE = stateFileFor();
import { readCrateWorkspace, readNpmWorkspace, crateRegistryVersions, npmRegistryVersions, satisfies as versionSatisfies } from "./publish.mjs";
const GH = process.platform === "win32" ? "C:\\Program Files\\GitHub CLI\\gh.exe" : "/opt/homebrew/bin/gh";
const RUSTRIGHT = "rustright";
const NODE = process.execPath;

// Output goes to the sinks of the run in progress: the process's own streams for the command line, the caller's collectors for an in-gate run (runPipeline).
const sinks = new AsyncLocalStorage();
const log = (line) => (sinks.getStore()?.log ?? ((l) => process.stdout.write(`${l}\n`)))(line);
const fail = (line) => (sinks.getStore()?.fail ?? ((l) => process.stderr.write(`rk-pipeline: ${l}\n`)))(line);
/** The environment every child of this run inherits: the in-gate caller's (the release run's), else the process's own. */
const runEnv = () => sinks.getStore()?.env ?? process.env;

/** The workspace root a call is about: always an explicit absolute path, never derived from where this coordinator lives. */
export function rootOf(root) {
  if (typeof root !== "string" || !path.isAbsolute(root) && !path.win32.isAbsolute(root)) throw new Error(`rk-pipeline needs the workspace root as an absolute-path parameter (got ${JSON.stringify(root)}); it never derives one from the coordinator's own location`);
  return root;
}

export function parseArgs(argv) {
  const [command, ...rest] = argv;
  const opts = { command, units: [], registry: "crates", jobs: 2, wait: false };
  for (let i = 0; i < rest.length; i += 1) {
    const arg = rest[i];
    if (arg === "--registry") opts.registry = rest[++i];
    else if (arg === "--qualification") opts.qualification = rest[++i];
    else if (arg === "--sha") opts.sha = rest[++i]; // GB-07: the candidate's commit as an explicit input (the gate's release); HEAD must be exactly it
    else if (arg === "--jobs") opts.jobs = Number(rest[++i]);
    else if (arg === "--wait") opts.wait = true;
    else if (arg.startsWith("--")) throw new Error(`unknown argument ${arg}`);
    else opts.units.push(...arg.split(/\s+/).filter(Boolean)); // one quoted list or many arguments, both fine
  }
  if (!["ready", "preflight", "canary", "dispatch", "watch", "loop"].includes(command)) throw new Error("command must be ready, preflight, canary, dispatch, watch or loop");
  if (!["crates", "npm"].includes(opts.registry) && !(command === "loop" && opts.registry === "all")) throw new Error("--registry must be crates or npm (loop also accepts all)");
  if (!Number.isInteger(opts.jobs) || opts.jobs < 1) throw new Error("--jobs must be a positive integer");
  if (opts.sha !== undefined && !/^[0-9a-f]{40}$/.test(opts.sha)) throw new Error("--sha must be a full lowercase commit id");
  opts.units = [...new Set(opts.units)];
  return opts;
}

// ---------- registry readiness ----------

/** Reuse publication semantics & Cargo metadata, including renamed/target/workspace dependencies. */
export const satisfies = (req, have) => Boolean(have) && versionSatisfies(have, req, "cargo");
export function readCrates(root) {
  return new Map([...readCrateWorkspace(rootOf(root))].map(([name, c]) => [name, {
    ...c, publish: !c.private,
    deps: c.deps.filter((d) => !(d.dev && !d.spec)).map((d) => ({ ...d, req: d.spec })),
  }]));
}
/** Workspace npm packages in the crate-record shape the planner uses: publish = not private; deps = the other workspace
 * packages it depends on (`workspace:` specs resolve to that member's version, so a sibling bump orders the waves). */
export function readPackages(root) {
  const raw = readNpmWorkspace(rootOf(root));
  return new Map([...raw].map(([name, p]) => [name, {
    name, version: p.version, dir: p.dir, publish: !p.private, hasTest: p.hasTest,
    deps: p.deps.filter((d) => raw.has(d.name)).map((d) => ({ name: d.name, req: /^workspace:/.test(d.spec) ? (d.spec === "workspace:*" || d.spec === "workspace:^" || d.spec === "workspace:~" ? `=${raw.get(d.name).version}` : d.spec.slice("workspace:".length)) : d.spec })),
  }]));
}
export function readUnits(registry, root) {
  if (registry === "npm") return readPackages(root);
  if (registry === "crates") return readCrates(root);
  throw new Error(`unknown registry ${registry}`);
}
export async function registryVersion(name, registry, fetchImpl = fetch) {
  if (registry === "npm") return npmRegistryVersions(name, fetchImpl); // Exact versions; transport failure throws.
  if (registry !== "crates") throw new Error(`unknown registry ${registry}`);
  return crateRegistryVersions(name, fetchImpl); // Exact versions, checksums & yanks; transport failure throws.
}
/** npm allows `a || b` alternatives in one requirement; a version satisfying any alternative satisfies the requirement. */
export function satisfiesAny(requirement, version) {
  return String(requirement).split("||").map((part) => part.trim()).filter(Boolean).some((part) => satisfies(part, version));
}
const usable = (versions, requirement) => [...versions].some(([v, meta]) => !meta.yanked && satisfiesAny(requirement, v));
export function waves(crates, registryVersions, failures = new Map()) {
  const unpublished = new Set([...crates.values()].filter((c) => c.publish && !registryVersions.get(c.name)?.has(c.version)).map((c) => c.name));
  const blockers = new Map();
  for (const name of unpublished) {
    const blocked = [];
    if (failures.has(name)) blocked.push(`lookup-failed:${name}`); // unknown to the registry read: never ready, never done
    for (const d of crates.get(name).deps) {
      if (!crates.has(d.name) || d.name === name) continue;
      if (!d.req || (typeof d.source === "string" && d.source.startsWith("git+"))) { blocked.push(`invalid:${d.name}`); continue; }
      if (usable(registryVersions.get(d.name) ?? new Map(), d.req)) continue;
      const future = crates.get(d.name);
      blocked.push(unpublished.has(d.name) && satisfiesAny(d.req, future.version) ? d.name : `unavailable:${d.name}`);
    }
    blockers.set(name, blocked);
  }
  const out = [], done = new Set();
  while (done.size < unpublished.size) {
    const ready = [...unpublished].filter((n) => !done.has(n) && blockers.get(n).every((b) => done.has(b))).sort();
    if (!ready.length) { out.push({ stuck: [...unpublished].filter((n) => !done.has(n)) }); break; }
    out.push(ready);
    for (const n of ready) done.add(n);
  }
  return { unpublished, blockers, waves: out };
}
/** The workspace crates a request needs to reason about: the selected units and every workspace dependency beneath them. */
export function closureOf(crates, selected) {
  const needed = new Set();
  const visit = (name) => {
    if (needed.has(name) || !crates.has(name)) return;
    needed.add(name);
    for (const d of crates.get(name).deps) visit(d.name);
  };
  for (const name of selected.length ? selected : [...crates.keys()]) visit(name);
  return needed;
}

/** Registry truth for the needed closure only. A lookup that fails marks THAT crate unknown (never "unpublished"):
 * it cannot be ready and cannot satisfy a consumer, while every unrelated crate proceeds (Astra review item 6). */
export async function readiness(opts, deps = {}) {
  const crates = deps.crates ?? readUnits(opts.registry ?? "crates", opts.root);
  const lookup = deps.lookup ?? registryVersion;
  const needed = [...closureOf(crates, opts.units ?? [])].filter((n) => crates.get(n).publish);
  const versions = new Map(), failures = new Map();
  const queue = [...needed];
  await Promise.all(Array.from({ length: Math.min(4, queue.length) }, async () => {
    while (queue.length) {
      const name = queue.shift();
      try {
        versions.set(name, await lookup(name, opts.registry));
      } catch (error) {
        failures.set(name, String(error?.message ?? error));
      }
    }
  }));
  const scoped = new Map([...crates].filter(([name]) => needed.includes(name)));
  return { crates: scoped, versions, failures, ...waves(scoped, versions, failures) };
}

/** A unit whose last hosted qualification failed runs again only on evidence that what it is built from changed since
 * that run: its own crate directory, the crate directories of its workspace dependency closure, its committed consumer
 * recipe, or its own entries in the reviewed policy (ci/gate.config.json rust.<unit>, ci/rk-pipeline.json
 * dispatch.exceptions.<unit>). A new workspace SHA alone is not evidence: rightkit-sandbox 0.6.0 ran hosted twice on the
 * same tree (runs 39872794369 and 39878045340). Shared pipeline changes (scripts/ci, workflows, other units' policy)
 * do not release a hold; a reviewed, version-bound `dispatch.releaseHolds.<unit>` entry naming the failed run does. */
export function holdReason(unit, sha, state, crates, { root, pipeline = readJson(path.join(rootOf(root), "ci", "rk-pipeline.json"), {}), changed = (...a) => changedBetween(rootOf(root), ...a), show = (...a) => showAt(rootOf(root), ...a) } = {}) {
  const prior = Object.values(state.dispatches ?? {}).filter((d) => d.unit === unit && d.status === "failed" && d.phase !== "preflight" && d.runId && d.sha && d.sha !== sha);
  if (!prior.length) return null;
  const last = prior.sort((a, b) => String(a.dispatchedAt ?? "").localeCompare(String(b.dispatchedAt ?? ""))).at(-1);
  const release = pipeline.dispatch?.releaseHolds?.[unit];
  if (release && String(release.run) === String(last.runId) && release.version === crates.get(unit)?.version && release.reason) return null;
  const dirs = [...closureOf(crates, [unit])].map((n) => crates.get(n).dir ?? `crates/${n}`);
  // The reviewed recipe lives at ci/consumers/<crate> or ci/consumers/npm/<package short name>; the inventory that admits
  // it (ci/consumer-recipes.json) is part of what the hosted verify checks, so a refreshed entry releases the hold.
  const recipeDir = unit.startsWith("@rightkit/") ? `ci/consumers/npm/${unit.slice("@rightkit/".length)}` : `ci/consumers/${unit}`;
  // Each crate dir holds its own Cargo.toml and Cargo.lock (#1285); there is no root lock. rightkit-typeset patches citationberg from vendor/.
  const vendored = dirs.includes("crates/rightkit-typeset") ? ["vendor/citationberg"] : [];
  if (changed(last.sha, sha, [...dirs, ...vendored, recipeDir, "ci/consumer-recipes.json"])) return null;
  const section = (file, pick) => { try { return JSON.stringify(pick(JSON.parse(show(last.sha, file)))) !== JSON.stringify(pick(JSON.parse(show(sha, file)))); } catch { return true; } };
  if (section("ci/gate.config.json", (j) => j.rust?.[unit]) || section("ci/rk-pipeline.json", (j) => j.dispatch?.exceptions?.[unit])) return null;
  return `held: unchanged since hosted failure at ${last.sha.slice(0, 12)} (run ${last.runId}); its crate closure, consumer recipe and own policy entries are identical, so the same run would repeat`;
}
function changedBetween(root, from, to, paths) {
  const r = spawnSync("git", ["diff", "--quiet", from, to, "--", ...paths], { cwd: root, encoding: "utf8" });
  if (r.status === 0) return false;
  if (r.status === 1) return true;
  throw new Error(`git diff ${from}..${to}: ${(r.stderr ?? "").trim()}`);
}
function showAt(root, sha, file) { return git(root, "show", `${sha}:${file}`); }

// ---------- receipts, allowsets, state ----------

export const QUALIFICATION_SCHEMA = "rightkit.unit-qualification/1";
/** The gate's qualification receipt for exactly this commit, registry and unit set: schema, commit, unit kind/name/version,
 * every local stage ok (check, compile, package, qualify) and the qualified artifact's sha256. Anything else refuses. */
export function readQualification(file, { sha, units, registry, readFile = (f) => fs.readFileSync(f, "utf8") }) {
  if (!file) throw new Error("dispatch requires --qualification <file>: the rightkit-gate unit qualification receipt (rightkit-gate --to release writes it); a dispatch without one is not gated");
  let q;
  try { q = JSON.parse(readFile(file)); } catch (error) { throw new Error(`cannot read the qualification receipt ${file}: ${error.message}`); }
  const kind = registry === "npm" ? "npm" : "cargo";
  if (q?.schema !== QUALIFICATION_SCHEMA) throw new Error(`qualification receipt schema is ${JSON.stringify(q?.schema)}, not ${QUALIFICATION_SCHEMA}`);
  if (q.commit !== sha) throw new Error(`qualification receipt is for ${String(q.commit).slice(0, 12)}, not HEAD ${sha.slice(0, 12)}`);
  if (q.unit?.kind !== kind || !units.includes(q.unit?.name) || units.length !== 1) throw new Error(`qualification receipt covers ${q.unit?.kind}:${q.unit?.name}; this dispatch names ${registry} ${units.join(" ")}`);
  if (!/^[0-9a-f]{40}$/.test(q.tree ?? "") || !/^\d+\.\d+\.\d+/.test(q.unit?.version ?? "")) throw new Error("qualification receipt lacks a tree or a version");
  for (const stage of ["check", "compile", "package", "qualify"]) if (q.stages?.[stage]?.ok !== true) throw new Error(`qualification receipt: stage ${stage} is not ok`);
  if (!/^[0-9a-f]{64}$/.test(q.artifactSha256 ?? "") || q.stages.qualify.artifactSha256 !== q.artifactSha256 || q.stages.package.artifactSha256 !== q.artifactSha256) throw new Error("qualification receipt does not bind one artifact sha256 across package and qualify");
  return { file: path.resolve(file), ...q };
}

/** GB-07: what a dispatch record keeps of the candidate beside its commit, so a retry reconciles by the same commit, tree and artifact digest (the gate's releaseUnit refuses a record that names another or none). */
export const dispatchIdentity = (qualification) => ({ tree: qualification.tree, artifactSha256: qualification.artifactSha256 });

export function receiptFor(unit, sha, out = DEFAULT_OUT) {
  const file = path.join(out, sha, `${unit.replaceAll("/", "__")}.json`);
  return fs.existsSync(file) ? { file, ...readJson(file, {}) } : null;
}

export function dispatchRefusal(receipt, sha, unit, registry = "crates", config, version, deps = {}) {
  config ??= readJson(path.join(rootOf(deps.root), "ci", "gate.config.json"), {});
  version ??= readUnits(registry, deps.root).get(unit)?.version;
  // The same derivation the pinned dispatcher makes for an npm unit (rightkit-dispatch.mjs requirePreflight): the
  // package directory and the expected row plan come from the committed workspace and policy, never from the receipt.
  let expectedRows, packageDir;
  if (registry === "npm") {
    // The helper's own package record (with its scripts), exactly what the pinned dispatcher reads; the planner's
    // record (readPackages here) has no scripts and made packageRows throw at cf9ef9c3 ("reading 'test'").
    const pkg = (deps.readPackages ?? readNpmPackages)(deps.readPackages ? deps.root : rootOf(deps.root)).get(unit);
    if (!pkg) return "unknown npm package";
    const pipeline = deps.pipeline ?? readJson(path.join(rootOf(deps.root), "ci", "rk-pipeline.json"), {});
    packageDir = pkg.dir;
    try { expectedRows = (deps.packageRows ?? packageRows)(unit, pkg, { storeDir: pipeline.tools?.pnpm?.[process.platform]?.storeDir }); } catch (error) { return String(error.message); }
  }
  return receiptRefusal(receipt, { sha, unit, registry, config, version, expectedRows, packageDir, allowHostedCompile: registry === "crates" });
}

/** Planner refusals remain refusals. Only explicit version-bound policy supplies exceptions. */
export async function allowsetsFor(registry, units, planFn, { root, config = readJson(path.join(rootOf(root), "ci", "gate.config.json"), {}), pipeline = readJson(path.join(rootOf(root), "ci", "rk-pipeline.json"), {}), crates = readUnits(registry, root) } = {}) {
  const allow = { allow_host_exclusion: [], allow_no_test: [], allow_ignored: [], allow_missing_assets: [] };
  for (const unit of units) {
    const policy = exceptionAllowsets(unit, crates.get(unit)?.version, config, pipeline);
    for (const key of Object.keys(allow)) allow[key].push(...policy[key]);
  }
  try {
    const plan = await planFn({ registry, root, unitsText: units.join(" "), dryRun: false, allowHostExclusionText: allow.allow_host_exclusion.join(" "), allowNoTestText: allow.allow_no_test.join(" "), allowIgnoredText: allow.allow_ignored.join(" "), allowMissingAssetsText: allow.allow_missing_assets.join(" ") });
    return { allow, errors: [], plan };
  } catch (error) {
    return { allow, errors: [String(error.message)] };
  }
}
export const stateKey = (registry, sha, unit) => `${registry}:${sha}:${unit}`;
export function readState(stateFile = STATE_FILE) {
  if (!fs.existsSync(stateFile)) return { dispatches: {} };
  const state = JSON.parse(fs.readFileSync(stateFile, "utf8"));
  if (!state.dispatches || typeof state.dispatches !== "object") throw new Error("invalid pipeline state");
  return state;
}

/** Durable claim (registry, unit, version): a dispatch at another SHA whose outcome is not settled (published or
 * failed) still owns that version. An unknown/ambiguous status is a claim too (fail closed). A changed version is not
 * claimed. Returns the claiming record, or null. */
export const RECONCILE_CONCLUSION = /reconcile|ambiguous|provenance mismatch/;
/** A failed record still claims its version when a run or upload may exist (ambiguous intent, watch deadline,
 * provenance mismatch, ambiguous uploads): the watcher marks those `reconcile: true`; older records are recognised by
 * their conclusion. A plain failed run (conclusion failure/cancelled, upload failure) does not claim. */
export const needsReconcile = (d) => d?.status === "failed" && (d.reconcile === true || RECONCILE_CONCLUSION.test(String(d.conclusion ?? "")));
export function openClaim(state, { registry, unit, version, sha }) {
  return Object.values(state?.dispatches ?? {}).find((d) => d && d.unit === unit && (d.registry ?? "crates") === registry
    && d.sha !== sha && (!["published", "failed"].includes(d.status) || needsReconcile(d))
    && (version === undefined || d.version === undefined || d.version === version)) ?? null;
}

export function writeState(state, stateFile = STATE_FILE) {
  fs.mkdirSync(path.dirname(stateFile), { recursive: true });
  const temp = `${stateFile}.${process.pid}.tmp`;
  fs.writeFileSync(temp, JSON.stringify(state, null, 2));
  fs.renameSync(temp, stateFile);
}

function git(root, ...args) {
  const r = spawnSync("git", args, { cwd: rootOf(root), encoding: "utf8" });
  if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr.trim()}`);
  return r.stdout.trim();
}

function gh(args) {
  const r = spawnSync(GH, args, { encoding: "utf8", env: { ...runEnv(), GH_NO_UPDATE_NOTIFIER: "1", GH_PROMPT_DISABLED: "1" }, timeout: 60_000, maxBuffer: 64 * 1024 * 1024 });
  return { ok: r.status === 0, stdout: r.stdout ?? "", error: r.error?.code ?? null, stderr: (r.stderr ?? "").slice(-2000) };
}

function runChild(command, args, env = runEnv(), cwd) {
  return new Promise((resolve) => {
    let out = "";
    // `rustright` starts the one way every gate child starts it (packages/release/broker-client.mjs): on Windows it is rustright.cmd, which
    // node cannot start, so the shim's target .exe is started directly, without a shell.
    let launch;
    try { launch = brokerClientInvocation(command, args, { cwd: rootOf(cwd), env, stdio: ["ignore", "pipe", "pipe"] }, { probe: spawnSync }); }
    catch (e) { resolve({ code: 1, out: String(e) }); return; }
    const child = spawn(launch.command, launch.args, launch.options);
    child.stdout.on("data", (c) => (out += c));
    child.stderr.on("data", (c) => (out += c));
    child.on("close", (code) => resolve({ code: code ?? 1, out }));
    child.on("error", (e) => resolve({ code: 1, out: out + String(e) }));
  });
}

// ---------- commands ----------

export async function cmdReady(opts) {
  const r = await readiness(opts);
  log(`unpublished (${r.unpublished.size}): ${[...r.unpublished].sort().join(" ")}`);
  r.waves.forEach((w, i) => log(Array.isArray(w) ? `wave ${i + 1} (${w.length}): ${w.join(" ")}` : `stuck: ${w.stuck.join(" ")}`));
  return r.waves.some((w) => !Array.isArray(w)) ? 1 : 0;
}

/** Step 1 of docs/PIPELINE-ARCHITECTURE.md (P1): scripts/preflight.mjs over exactly `sha`'s committed files (`--commit`), in
 * seconds, before any canary, pre-flight compile or dispatch. A red check starts nothing. */
function runCheckChild(sha, opts) {
  const root = rootOf(opts?.root);
  return runChild(NODE, [path.join(root, "scripts", "preflight.mjs"), "--commit", sha], runEnv(), root);
}

/** The check passes for `sha`, and HEAD is still `sha` once it has: another chat may commit to this shared checkout while it
 * runs, and every later stage reads HEAD, so a moved HEAD starts nothing (check A, commit B, canary B is the failure). */
async function checkPasses(check, sha, head, opts) {
  const r = await check(sha, opts);
  if (r.code !== 0) {
    for (const line of String(r.out ?? "").split("\n").filter((l) => /^(FAIL |     fix: |preflight: )/.test(l))) log(line);
    log(`check RED at ${String(sha).slice(0, 12)}: nothing canaried, compiled or dispatched; fix every FAIL, commit, then rerun`);
    return false;
  }
  return stillAt(sha, head, "while the check ran: nothing canaried, compiled or dispatched; rerun at the new commit");
}

function stillAt(sha, head, when) {
  const now = head ? head() : sha;
  if (now === sha) return true;
  log(`HEAD moved from ${String(sha).slice(0, 12)} to ${String(now).slice(0, 12)} ${when}`);
  return false;
}

async function preflightMany(units, opts) {
  const results = new Map();
  const queue = [...units];
  const worker = async () => {
    while (queue.length) {
      const unit = queue.shift();
      const script = opts.registry === "npm" ? "rk-npm-preflight.mjs" : "rk-preflight.mjs";
      const root = rootOf(opts.root);
      const r = await runChild(NODE, [path.join(root, "scripts", "ci", script), "--registry", opts.registry, "--unit", unit], runEnv(), root);
      const last = r.out.trim().split("\n").filter(Boolean).slice(-1)[0] ?? "";
      results.set(unit, { code: r.code, last });
      log(`preflight ${unit}: ${r.code === 0 ? "GREEN" : "RED"} (${last.slice(0, 140)})`);
    }
  };
  await Promise.all(Array.from({ length: Math.min(opts.jobs, units.length) }, worker));
  return results;
}

/** Units that run the pipeline canary before their pre-flight. */
export const CANARY_UNITS = new Set(["@rightkit/release"]);
/** Exit codes of scripts/ci/rk-release-canary.mjs: 0 green, 10 regression, 11 incomplete (infrastructure). */
export const CANARY_REGRESSION = 10;
export const CANARY_INCOMPLETE = 11;
export const needsCanary = (unit, registry) => registry === "npm" && CANARY_UNITS.has(unit);

/** The canary of the commit the check passed: --sha makes it refuse any other HEAD, and a HEAD that moves before its verdict. */
async function runCanaryChild(unit, sha, opts) {
  const root = rootOf(opts?.root);
  const r = await runChild(NODE, [path.join(root, "scripts", "ci", "rk-release-canary.mjs"), "--unit", unit, "--sha", sha], runEnv(), root);
  const last = r.out.trim().split("\n").filter(Boolean).slice(-1)[0] ?? "";
  return { code: r.code, last };
}

/** GB-07: a call made with an explicit candidate commit (`--sha`, from the gate's release) proves and dispatches exactly that commit: another HEAD refuses before anything starts. */
export function explicitShaRefusal(opts, sha) {
  return opts.sha !== undefined && opts.sha !== sha ? `HEAD ${String(sha).slice(0, 12)} is not the explicit candidate commit ${opts.sha.slice(0, 12)} (--sha); nothing was started` : null;
}


export async function cmdCanary(opts, deps = {}) {
  if (!opts.units.length) { fail("name at least one unit"); return 2; }
  // P1: the canary is expensive work (it builds and runs every app against the candidate); the check runs first, on the same commit.
  const head = deps.head ?? (() => git(opts.root, "rev-parse", "HEAD"));
  const sha = head();
  { const refused = explicitShaRefusal(opts, sha); if (refused) { fail(refused); return 2; } }
  if (!(await checkPasses(deps.check ?? runCheckChild, sha, head, opts))) return 1;
  let worst = 0;
  for (const unit of opts.units) {
    if (!needsCanary(unit, "npm")) { fail(`${unit}: no pipeline canary for this unit`); worst = Math.max(worst, 2); continue; }
    const result = await (deps.canary ?? runCanaryChild)(unit, sha, opts);
    log(`canary ${unit}: ${result.code === 0 ? "GREEN" : result.code === CANARY_REGRESSION ? "REGRESSION" : result.code === CANARY_INCOMPLETE ? "INCOMPLETE" : "ERROR"} (${String(result.last ?? "").slice(0, 140)})`);
    worst = Math.max(worst, result.code === 0 ? 0 : 1);
  }
  return worst;
}

export async function cmdPreflight(opts, deps = {}) {
  if (!opts.units.length) {
    fail("name at least one unit");
    return 2;
  }
  const head = deps.head ?? (() => git(opts.root, "rev-parse", "HEAD"));
  const sha = head();
  { const refused = explicitShaRefusal(opts, sha); if (refused) { fail(refused); return 2; } }
  if (!(await checkPasses(deps.check ?? runCheckChild, sha, head, opts))) return 1;
  const results = await (deps.preflightMany ?? preflightMany)(opts.units, opts);
  // The pre-flight children (digest-pinned) read HEAD themselves: results from a moved HEAD prove nothing about the checked commit.
  if (!stillAt(sha, head, "while the pre-flights ran: their results are not for the checked commit; rerun")) return 1;
  return [...results.values()].every((r) => r.code === 0) ? 0 : 1;
}

export async function cmdDispatch(opts, deps = {}) {
  if (!opts.units.length) {
    fail("name at least one unit");
    return 2;
  }
  const head = deps.head ?? (() => git(opts.root, "rev-parse", "HEAD"));
  const sha = head();
  { const refused = explicitShaRefusal(opts, sha); if (refused) { fail(refused); return 2; } }
  // P1: a dispatch starts hosted runners; the check runs first, on the commit it dispatches, whatever pre-flight receipts say.
  if (!(await checkPasses(deps.check ?? runCheckChild, sha, head, opts))) return 1;
  // Runner-aware (Adrian, 2026-10-26: "managed properly with how many runners are available"): every hosted qualification takes one
  // macOS and one Windows runner for its native verify. The dispatcher counts the publish runs GitHub already has queued or running
  // (publish.yml and publish-app.yml) and holds, exit 3, while that count is at the limit (ci/rk-pipeline.json dispatch.maxInFlightRuns,
  // default 2), one dispatch at a time as the count allows. A hold is not a failure: nothing was sent, run it again when a run ends.
  const pipeline = deps.pipeline ?? readJson(path.join(rootOf(opts.root), "ci", "rk-pipeline.json"), {});
  const maxInFlight = Number(pipeline.dispatch?.maxInFlightRuns ?? 2);
  if (!Number.isInteger(maxInFlight) || maxInFlight < 1) { fail(`ci/rk-pipeline.json dispatch.maxInFlightRuns must be a positive integer, not ${JSON.stringify(pipeline.dispatch?.maxInFlightRuns)}`); return 2; }
  let inFlight = await (deps.inFlight ?? (() => hostedRunsInFlight(runChild, opts.root)))();
  if (inFlight === null) { fail("cannot count the hosted runs in flight (gh run list failed); not dispatching blind"); return 2; }
  if (inFlight >= maxInFlight) { fail(`hold: ${inFlight} hosted publish run(s) queued or running, limit ${maxInFlight}; nothing dispatched, run again when one ends`); return 3; }
  if (git(opts.root, "status", "--porcelain", "--untracked-files=no")) throw new Error("tracked files changed before dispatch");
  const remote = git(opts.root, "ls-remote", "origin", "refs/heads/main").split(/\s+/)[0];
  if (remote !== sha) {
    fail(`HEAD ${sha.slice(0, 12)} is not origin/main ${String(remote).slice(0, 12)}; push first, the dispatcher pins the commit`);
    return 2;
  }
  // The gate's qualification receipt (rightkit-gate --to release writes it after the local check, compile, package and qualify
  // stages passed): the hosted run is bound to the locally qualified bytes. Without it nothing is dispatched; `loop` has none.
  let qualification;
  try { qualification = readQualification(opts.qualification, { sha, units: opts.units, registry: opts.registry }); }
  catch (error) { fail(`dispatch refused: ${error.message}`); return 2; }
  const refused = [];
  const go = [];
  for (const unit of opts.units) {
    const why = dispatchRefusal(receiptFor(unit, sha), sha, unit, opts.registry, undefined, undefined, { root: opts.root });
    if (why) refused.push(`${unit}: ${why}`);
    else go.push(unit);
  }
  for (const r of refused) fail(`refused ${r}`);
  if (!go.length) return 2;
  const planFn = deps.planFn ?? (await import("./publish.mjs")).planPublication;
  let exit = refused.length ? 1 : 0;
  const load = deps.readState ?? (() => readState(opts.stateFile)), save = deps.writeState ?? ((state) => writeState(state, opts.stateFile));
  for (const unit of go) {
    const { allow, errors, plan } = await allowsetsFor(opts.registry, [unit], planFn, { root: opts.root });
    if (errors.length) { for (const e of errors) fail(`plan ${unit}: ${e}`); exit = 1; continue; }
    const planned = plan.units.find((u) => u.name === unit);
    if (!planned) { log(`${unit}: exact version already exists; no dispatch`); continue; }
    const key = stateKey(opts.registry, sha, unit);
    const state = load();
    const claim = openClaim(state, { registry: opts.registry, unit, version: planned.version, sha });
    if (claim) { fail(`${unit}: ${planned.version} is already claimed by the dispatch at ${String(claim.sha).slice(0, 12)} (status ${claim.status}, run ${claim.runId ?? "?"}); not dispatching again`); exit = 1; continue; }
    const previous = state.dispatches[key];
    if (previous && !(previous.status === "failed" && previous.phase === "preflight") && ["dispatched", "dispatching", "unknown", "failed", "published"].includes(previous.status)) {
      fail(`${unit}: attempt already recorded as ${previous.status}; reconcile it before retrying`); exit = 1; continue;
    }
    // Persist intent before external effect. Ambiguous POST/discovery failures must never auto-retry.
    state.dispatches[key] = { unit, registry: opts.registry, sha, ...dispatchIdentity(qualification), version: planned.version, status: "dispatching", ownerPid: process.pid, attempts: 1, dispatchedAt: new Date().toISOString() };
    save(state);
    if (inFlight >= maxInFlight) { fail(`hold: ${unit} not dispatched, ${inFlight} hosted publish run(s) in flight, limit ${maxInFlight}; run again when one ends`); exit = exit || 3; const held = load(); delete held.dispatches[key]; save(held); break; }
    const nonce = crypto.randomBytes(32).toString("hex");
    const args = ["run", "rightkit-dispatch", "--", "--workflow", "publish", "--registry", opts.registry, "--unit", unit, "--sha", sha, "--qualification", qualification.file];
    for (const [key, list] of Object.entries(allow)) if (list.includes(unit)) args.push(`--${key.replaceAll("_", "-")}`, unit);
    const r = await runChild(RUSTRIGHT, args, { ...runEnv(), RUSTRIGHT_ATTEST_NONCE: nonce }, opts.root);
    const record = r.out.split("\n").map((l) => l.trim()).filter((l) => l.startsWith("{")).map((l) => {
      try {
        return JSON.parse(l);
      } catch {
        return null;
      }
    }).find((o) => o && o.runId);
    const request = r.out.match(/^rustright: request ([0-9a-f-]{36})/m)?.[1] ?? null;
    if (r.code !== 0 || !record) {
      fail(`dispatch ${unit} failed (exit ${r.code}): ${r.out.trim().split("\n").slice(-3).join(" | ").slice(0, 300)}`);
      const current = load(); current.dispatches[key].status = "unknown"; save(current);
      exit = 1;
      inFlight += 1; // an ambiguous POST may have started a run: count it rather than overshoot
      continue;
    }
    const current = load();
    const prior = (current.dispatches[key]?.attempts ?? 1) - 1;
    current.dispatches[key] = { unit, version: planned.version, runAttempt: record.runAttempt, registry: opts.registry, sha, ...dispatchIdentity(qualification), runId: record.runId, runUrl: record.runUrl, requestId: record.requestId, brokerRequest: request, nonce, attempts: prior + 1, dispatchedAt: record.dispatchedAt, status: "dispatched" };
    save(current);
    log(`dispatched ${unit}: run ${record.runId} (${record.runUrl ?? "no url"}), broker request ${request}, attempt ${prior + 1}`);
    inFlight += 1;
  }
  return exit;
}

/** The hosted publish runs GitHub has queued, waiting or in progress (publish.yml and publish-app.yml), or null when gh cannot say. */
export async function hostedRunsInFlight(run = runChild, root) {
  let total = 0;
  for (const workflow of ["publish.yml", "publish-app.yml"]) {
    for (const status of ["queued", "in_progress", "waiting"]) {
      const r = await run("gh", ["run", "list", "-R", "bogusyogi/rightkit", "--workflow", workflow, "--status", status, "--limit", "50", "--json", "databaseId"], runEnv(), root);
      if (r.code !== 0) return null;
      try { const parsed = JSON.parse(r.out || "[]"); if (!Array.isArray(parsed)) return null; total += parsed.length; } catch { return null; }
    }
  }
  return total;
}

function runState(runId) {
  const r = gh(["api", `repos/${REPO}/actions/runs/${runId}`, "--jq", "{status, conclusion, head_sha, run_attempt, path, event, display_title}"]);
  if (!r.ok) return null;
  try {
    return JSON.parse(r.stdout);
  } catch {
    return null;
  }
}

function failedSteps(runId) {
  const r = gh(["api", `repos/${REPO}/actions/runs/${runId}/jobs`, "--jq", '[.jobs[] | select(.conclusion == "failure") | {job: .name, steps: [.steps[] | select(.conclusion == "failure") | .name]}]']);
  return r.ok ? r.stdout.trim() : "";
}

export function matchingUploadRuns(runs, dispatch) {
  return runs.filter((r) => r.event === "workflow_run" && r.path === ".github/workflows/publish-direct.yml" && r.display_title === `publish-direct ${dispatch.runId} ${dispatch.runAttempt}`);
}
/** Upload runs created from 15 minutes before the dispatch on; matching is still by run id & attempt in matchingUploadRuns. */
export function uploadRunsQuery(dispatch) {
  const since = new Date(Date.parse(dispatch.dispatchedAt) - 15 * 60_000).toISOString();
  return `repos/${REPO}/actions/workflows/publish-direct.yml/runs?event=workflow_run&per_page=100&created=%3E%3D${since}`;
}
function uploadRun(dispatch) {
  const r = gh(["api", "--paginate", "--slurp", uploadRunsQuery(dispatch)]);
  if (!r.ok) throw new Error(`upload runs are unreadable: ${r.error ?? "exit"} ${r.stderr.trim().slice(-500)}`);
  return matchingUploadRuns(JSON.parse(r.stdout).flatMap((p) => p.workflow_runs ?? []), dispatch);
}

export async function cmdWatch(opts, deps = {}) {
  const load = deps.readState ?? (() => readState(opts.stateFile));
  const save = deps.writeState ?? ((state) => writeState(state, opts.stateFile));
  const inspect = deps.runState ?? runState;
  const uploadsFor = deps.uploadRun ?? uploadRun;
  const versionsFor = deps.registryVersion ?? registryVersion;
  const state = load();
  const open = Object.values(state.dispatches).filter((d) => d.status !== "published" && d.status !== "failed");
  let hadFailure = Object.values(state.dispatches).some((d) => d.status === "failed");
  if (!open.length) {
    log("nothing dispatched is pending");
    return hadFailure ? 1 : 0;
  }
  const timeoutMs = (readJson(path.join(rootOf(opts.root), "ci", "rk-pipeline.json"), {}).dispatch?.watchTimeoutMinutes ?? 180) * 60_000;
  for (;;) {
    let pending = 0;
    for (const d of open) {
      const unit = d.unit ?? "legacy-state";
      if (d.status === "dispatching" && d.ownerPid === process.pid) { pending += 1; continue; }
      if (!d.runId || !d.runAttempt || !d.version || !d.unit || !Number.isFinite(Date.parse(d.dispatchedAt))) {
        d.status = "failed"; d.conclusion = "dispatch intent is ambiguous or legacy; reconcile by broker request before retry"; d.reconcile = true; hadFailure = true; continue;
      }
      if (Date.now() - Date.parse(d.dispatchedAt) > timeoutMs) {
        d.status = "failed"; d.conclusion = "watch deadline exceeded; reconcile existing run before retry"; d.reconcile = true; hadFailure = true; continue;
      }
      if (d.status === "published" || d.status === "failed") continue;
      const run = inspect(d.runId);
      if (!run) {
        log(`${unit}: run ${d.runId} not readable`);
        pending += 1;
        continue;
      }
      if (run.head_sha !== d.sha || Number(run.run_attempt) !== Number(d.runAttempt) || run.event !== "workflow_dispatch" || run.path !== ".github/workflows/publish.yml" || !String(run.display_title).split(/\s+/).includes(d.requestId)) {
        d.status = "failed"; d.conclusion = "qualification provenance mismatch"; d.reconcile = true; hadFailure = true; continue;
      }
      if (run.status !== "completed") {
        log(`${unit}: run ${d.runId} ${run.status}`);
        pending += 1;
        continue;
      }
      if (run.conclusion !== "success") {
        d.status = "failed";
        hadFailure = true;
        d.conclusion = run.conclusion;
        d.failedSteps = failedSteps(d.runId);
        log(`${unit}: run ${d.runId} ${run.conclusion} <- ${d.failedSteps}`);
        continue;
      }
      let uploads;
      try { uploads = uploadsFor(d); } catch (e) { log(`${unit}: upload runs unreadable: ${e.message}`); pending += 1; continue; }
      if (uploads.length > 1) { d.status = "failed"; d.conclusion = "ambiguous upload runs"; d.reconcile = true; hadFailure = true; continue; }
      const done = uploads.find((u) => u.status === "completed");
      if (!done) {
        log(`${unit}: qualification green, upload ${uploads[0]?.status ?? "not started"}`);
        pending += 1;
        continue;
      }
      if (done.conclusion !== "success") {
        d.status = "failed"; d.conclusion = `upload ${done.conclusion}`; d.uploadRunId = done.id; hadFailure = true; continue;
      }
      let versions;
      try { versions = await versionsFor(unit, d.registry); } catch (e) { log(`${unit}: registry read failed: ${e.message}`); pending += 1; continue; }
      const published = versions.get(d.version);
      if (!published || published.yanked) { pending += 1; log(`${unit}: awaiting usable ${d.version} in registry`); continue; }
      d.status = "published"; d.published = d.version; d.registryChecksum = published.cksum; d.uploadRunId = done.id;
      log(`${unit}: PUBLISHED ${d.version} (upload run ${done.id})`);
    }
    const current = load();
    for (const d of open) {
      const key = d.unit ? stateKey(d.registry, d.sha, d.unit) : Object.keys(state.dispatches).find((k) => state.dispatches[k] === d);
      current.dispatches[key] = d;
    }
    save(current);
    if (!pending || !opts.wait) return pending || hadFailure ? 1 : 0;
    await new Promise((r) => setTimeout(r, 60_000));
  }
}

/** Stream ready units without a wave barrier; one attempt per SHA/unit, including local failures. */
export async function cmdLoop(opts, deps = {}) {
  const head = deps.head ?? (() => git(opts.root, "rev-parse", "HEAD"));
  const sha = head();
  const readyFn = deps.readiness ?? readiness;
  const stateFn = deps.readState ?? (() => readState(opts.stateFile));
  const save = deps.writeState ?? ((state) => writeState(state, opts.stateFile));
  const watch = deps.watch ?? cmdWatch;
  const preflight = deps.preflight ?? (async (unit, registry = opts.registry) => (await preflightMany([unit], { ...opts, registry })).get(unit));
  const registries = opts.registry === "all" ? ["crates", "npm"] : [opts.registry];
  const slot = (registry, unit) => `${registry}:${unit}`;
  const dispatch = deps.dispatch ?? cmdDispatch;
  const canary = deps.canary ?? runCanaryChild;
  const pause = deps.pause ?? (() => new Promise((r) => setTimeout(r, 15_000)));
  const hold = deps.holdReason ?? holdReason;
  if (!(await checkPasses(deps.check ?? runCheckChild, sha, head, opts))) return 1;
  const active = new Map(), attempted = new Set(), held = new Set();
  let dispatchTail = Promise.resolve();
  let failed = false;
  // Every exit path drains the live preflight workers and the serialized dispatch tail first: no worker may dispatch
  // after the coordinator has stopped and released its lock (Astra review item 1).
  const drain = async () => { await Promise.allSettled([...active.values(), dispatchTail]); };
  try {
  for (;;) {
    if (head() !== sha) throw new Error("HEAD moved during release; retain receipts, reconcile before continuing");
    await watch({ ...opts, registry: registries[0], wait: false });
    for (const registry of registries) {
    const r = await readyFn({ ...opts, registry });
    const selected = [...r.unpublished].filter((u) => !opts.units.length || opts.units.includes(u));
    const state = stateFn();
    const ready = (Array.isArray(r.waves[0]) ? r.waves[0] : []).filter((u) => selected.includes(u));
    for (const unit of ready) {
      // Slots are per registry: crate pre-flights never hold the npm units back (and the reverse).
      if ([...active.keys()].filter((k) => k.startsWith(`${registry}:`)).length >= opts.jobs) break;
      const key = stateKey(registry, sha, unit);
      const id = slot(registry, unit);
      // A record at this SHA ends the unit's attempts here, except a preflight-phase failure: that is local evidence
      // only (nothing was dispatched), and the dispatcher itself admits a retry of it; the loop at 14ac0759 skipped
      // qa and tauri-shell after a tree-change refusal and exited with nothing to do. A canary-incomplete record
      // (outage, timeout, unrepresentative host) is retried the same way; a canary regression is not: the same SHA
      // would reproduce it, and only a new commit changes the answer.
      const previous = state.dispatches[key];
      if (attempted.has(id) || held.has(id) || (previous && !(previous.status === "failed" && ["preflight", "canary-incomplete"].includes(previous.phase)))) continue;
      const claim = openClaim(state, { registry, unit, version: r.crates?.get?.(unit)?.version, sha });
      if (claim) { held.add(id); log(`${unit}: ${claim.version ?? ""} already claimed at ${String(claim.sha).slice(0, 8)} (status ${claim.status}, run ${claim.runId ?? "?"}); watching it, not dispatching again`); continue; }
      const reason = hold(unit, sha, state, r.crates, { root: opts.root });
      if (reason) { held.add(id); log(`${unit}: ${reason}`); continue; }
      attempted.add(id);
      const task = (async () => {
        if (head() !== sha) throw new Error(`HEAD moved from ${sha.slice(0, 12)} before ${unit} started; nothing more runs at the checked commit`);
        if (needsCanary(unit, registry)) {
          const verdict = await canary(unit, sha, opts);
          if (verdict.code !== 0) {
            failed = true;
            const phase = verdict.code === CANARY_REGRESSION ? "canary" : "canary-incomplete";
            log(`${unit}: canary ${phase === "canary" ? "found a regression" : "is incomplete"} at ${sha.slice(0, 12)}: ${String(verdict.last ?? "").slice(0, 140)}`);
            const current = stateFn(); current.dispatches[key] = { unit, sha, registry, status: "failed", phase, detail: verdict.last }; save(current);
            return;
          }
        }
        const result = await preflight(unit, registry);
        if (head() !== sha) throw new Error(`HEAD moved from ${sha.slice(0, 12)} while ${unit} was pre-flighted; its result is not for the checked commit`);
        if (result.code !== 0) {
          failed = true;
          const current = stateFn(); current.dispatches[key] = { unit, sha, registry, status: "failed", phase: "preflight", detail: result.last }; save(current);
          return;
        }
        // Serialized dispatch writes, independent preflight workers.
        const launched = dispatchTail.then(async () => {
          if (head() !== sha) throw new Error("HEAD changed before dispatch");
          return dispatch({ ...opts, registry, units: [unit] });
        });
        dispatchTail = launched.catch(() => {});
        if (await launched !== 0) failed = true;
      })().catch((error) => { failed = true; fail(`${unit}: ${error.message}`); }).finally(() => active.delete(id));
      active.set(id, task);
    }
    }
    const current = stateFn();
    // Unsettled runs from any SHA in these registries keep the loop watching (they own their versions).
    const pending = Object.values(current.dispatches).some((d) => (d.sha === sha || registries.includes(d.registry ?? "crates")) && !["published", "failed"].includes(d.status));
    if (!active.size && !pending) {
      const unfinished = [];
      for (const registry of registries) {
        const remaining = await readyFn({ ...opts, registry });
        unfinished.push(...[...remaining.unpublished].filter((u) => !opts.units.length || opts.units.includes(u)));
      }
      if (unfinished.length) fail(`unfinished: ${unfinished.join(" ")}`);
      return failed || unfinished.length ? 1 : 0;
    }
    await Promise.race([...active.values(), pause()]);
  }
  } catch (error) {
    failed = true;
    await drain();
    throw error;
  } finally {
    await drain();
  }
}

/**
 * (2) Coordinator lock (GB-46; GATE E9, durable coordination). The lock file is published COMPLETE or not at all: the full record (pid, process start,
 * boot, host, nonce) is written to a private temp file, fsynced, and hard-linked to the lock path, which fails if the path exists (a no-clobber
 * reservation), so no reader ever meets a created-but-empty or partial record. Identity is pid + process start + boot + nonce, not a pid alone: a recorded
 * pid whose process started at another time (pid reuse) or before another boot is dead. A record that cannot be read is UNKNOWN, never dead: it is
 * refused, never deleted. Reclaiming a dead owner is serialized by a reclaim reservation and re-checks, under it, that the file still is the record that
 * was judged dead (nonce and inode), so two coordinators reclaiming at once never delete each other's live lock. Release removes the lock only when
 * its nonce is ours. Waiting is bounded.
 */
export function pidAlive(pid) {
  try { process.kill(pid, 0); return true; } catch (error) { return error.code === "EPERM"; }
}
const sleepSync = (ms) => { try { Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms); } catch { const end = Date.now() + ms; while (Date.now() < end) { /* spin */ } } };
let probeCache;
function defaultIdentity(pid) {
  try {
    probeCache ??= processProbe({ platform: process.platform });
    const { alive, startedMs } = probeCache.identity(pid);
    let bootMs = null; try { bootMs = probeCache.bootTime?.() ?? null; } catch { bootMs = null; }
    return { alive, startedMs: Number.isFinite(startedMs) ? startedMs : null, bootMs: Number.isFinite(bootMs) ? bootMs : null };
  } catch { return { alive: null, startedMs: null, bootMs: null }; }
}
const SAME_START_MS = 2000;
/** Whether a lock record names a live owner: alive, and not a reused pid (another start time) nor a pre-reboot one. */
function ownerLive(record, { alive, identity }) {
  if (!alive(record.pid)) return false;
  const now = identity(record.pid);
  if (Number.isFinite(record.startedMs) && Number.isFinite(now?.startedMs) && Math.abs(record.startedMs - now.startedMs) > SAME_START_MS) return false; // the pid was reused
  const mine = identity(process.pid);
  if (Number.isFinite(record.bootMs) && Number.isFinite(mine?.bootMs) && Math.abs(record.bootMs - mine.bootMs) > SAME_START_MS * 5) return false; // another boot
  return true;
}
/** Publishes `record` at `file` complete, or throws EEXIST (the no-clobber reservation); the private temp file never survives. */
function publishComplete(file, record) {
  const temp = `${file}.${process.pid}.${crypto.randomBytes(6).toString("hex")}.tmp`;
  try {
    const fd = fs.openSync(temp, "wx", 0o600);
    try { fs.writeSync(fd, `${JSON.stringify(record)}\n`); fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
    fs.linkSync(temp, file);
  } finally { try { fs.unlinkSync(temp); } catch { /* gone */ } }
}
function readRecord(file) {
  let text;
  try { text = fs.readFileSync(file, "utf8"); } catch (error) { if (error.code === "ENOENT") return { gone: true }; return { unknown: true, why: error.message }; }
  try {
    const record = JSON.parse(text);
    if (record && Number.isInteger(record.pid) && record.pid > 0) {
      // A schema-1 record ({ pid, at }) written by an earlier coordinator is a complete record with a derived identity, judged by its pid alone.
      return { record: typeof record.nonce === "string" ? record : { ...record, nonce: `legacy-${record.pid}-${record.at ?? ""}` }, ino: fs.statSync(file).ino };
    }
    return { unknown: true, why: "no owner identity in it" };
  }
  catch { return { unknown: true, why: "it is not a complete record" }; }
}
export function acquireLock(lockFile, { alive = pidAlive, pid = process.pid, identity = defaultIdentity, wait = sleepSync, maxWaitMs = 2000, hooks = {} } = {}) {
  fs.mkdirSync(path.dirname(lockFile), { recursive: true });
  const me = identity(pid);
  const mine = { schema: 2, pid, nonce: crypto.randomBytes(16).toString("hex"), startedMs: me?.startedMs ?? null, bootMs: me?.bootMs ?? null, host: os.hostname(), at: new Date().toISOString() };
  const reclaimFile = `${lockFile}.reclaim`;
  const deadline = Date.now() + maxWaitMs;
  for (;;) {
    hooks.beforePublish?.(lockFile);
    try { publishComplete(lockFile, mine); return { nonce: mine.nonce }; } catch (error) { if (error.code !== "EEXIST") throw error; }
    const found = readRecord(lockFile);
    if (found.gone) continue; // released between the attempt and the read
    if (found.unknown) throw new Error(`${lockFile} holds a record whose owner cannot be read (${found.why}); its owner is UNKNOWN, not dead: inspect it, never delete the lock by hand`);
    if (ownerLive(found.record, { alive, identity })) throw new Error(`another coordinator (pid ${found.record.pid}) holds ${lockFile}; wait for it or stop it, never delete the lock by hand`);
    hooks.afterJudgedDead?.(lockFile);
    // The owner is dead: reclaim under a reservation, re-checking under it that the file is still the record judged dead.
    try { publishComplete(reclaimFile, { ...mine, reclaim: true }); } catch (error) {
      if (error.code !== "EEXIST") throw error;
      const holder = readRecord(reclaimFile);
      if (holder.record && !ownerLive(holder.record, { alive, identity })) { try { fs.unlinkSync(reclaimFile); } catch { /* another reclaimer got it */ } continue; }
      if (Date.now() >= deadline) throw new Error(`could not acquire ${lockFile}: another coordinator is reclaiming it`);
      wait(25); continue;
    }
    try {
      const again = readRecord(lockFile);
      if (again.record && again.record.nonce === found.record.nonce && again.ino === found.ino) fs.unlinkSync(lockFile);
    } finally {
      const own = readRecord(reclaimFile);
      if (own.record?.nonce === mine.nonce) { try { fs.unlinkSync(reclaimFile); } catch { /* gone */ } }
    }
    if (Date.now() >= deadline) throw new Error(`could not acquire ${lockFile}`);
  }
}
export function releaseLock(lockFile, { pid = process.pid, nonce } = {}) {
  try {
    const found = readRecord(lockFile);
    if (!found.record || found.record.pid !== pid) return; // not ours, unreadable or gone
    if (nonce !== undefined && found.record.nonce !== nonce) return; // a successor's lock
    const again = readRecord(lockFile);
    if (again.record?.nonce === found.record.nonce && again.ino === found.ino) fs.unlinkSync(lockFile);
  } catch { /* not ours or gone */ }
}

/** The commands that change the coordinator's state hold its lock for the whole command (GB-46). */
const MUTATING = new Set(["dispatch", "watch", "loop"]);
async function withCoordinatorLock(command, stateFile, action) {
  if (!MUTATING.has(command)) return action();
  const lock = `${stateFile}.lock`;
  const held = acquireLock(lock);
  try { return await action(); } finally { releaseLock(lock, { nonce: held?.nonce }); }
}

/**
 * Runs one command for the workspace `root` (the control-plane checkout): parse, then dispatch to the command. Nothing here reads a location of its
 * own; `deps` are the commands' injection points (check, canary, preflightMany, head, ...).
 */
export async function main(argv, { root, stateFile = stateFileFor(), deps = {} } = {}) {
  let opts;
  try {
    opts = parseArgs(argv);
    opts.root = rootOf(root);
    opts.stateFile = stateFile;
  } catch (error) {
    fail(error.message);
    return 2;
  }
  switch (opts.command) {
    case "ready":
      return cmdReady(opts);
    case "preflight":
      return cmdPreflight(opts, deps);
    case "canary":
      return cmdCanary(opts, deps);
    case "dispatch":
      return cmdDispatch(opts, deps);
    case "watch":
      return cmdWatch(opts, deps);
    case "loop":
      return cmdLoop(opts, deps);
    default:
      return 2;
  }
}

// ---------- the in-gate-process entry (GB-07) ----------

/** The last non-empty line of a captured output, as the child-process route reports it. */
const lastLine = (text) => String(text ?? "").trim().split("\n").filter(Boolean).slice(-1)[0] ?? "";
const capture = () => { const lines = []; return { lines, put: (l) => lines.push(String(l)), text: () => lines.join("\n") }; };

/** The directory of the installed gate tooling beside THIS module (scripts/ci/..): the tools the in-gate steps run are the gate's own installed ones, never files under the workspace root. */
export const installedToolsRoot = (moduleUrl = import.meta.url) => path.resolve(path.dirname(fileURLToPath(moduleUrl)), "..", "..");

/**
 * The stages that run as child processes from the workspace root on the command line, run in THIS process from the installed gate modules
 * instead: the deterministic check (scripts/preflight.mjs `--commit`), each unit's local pre-flight (rk-preflight / rk-npm-preflight) and the pipeline
 * canary (rk-release-canary). The workspace root is only ever the data they judge (`--repo`, `deps.root`); the code is the installed gate's.
 */
export function inGateDeps(opts, { toolsRoot = installedToolsRoot(), env = runEnv(), load = (rel) => import(pathToFileURL(path.join(toolsRoot, rel)).href) } = {}) {
  const root = rootOf(opts.root);
  const check = async (sha) => {
    const sink = capture();
    const { main: checkMain } = await load("scripts/preflight.mjs");
    const code = await checkMain(["--commit", sha, "--repo", root], { out: sink.put, err: sink.put, cwd: root });
    return { code, out: sink.text() };
  };
  const preflightOne = async (unit, registry) => {
    const sink = capture();
    const { main: preflightMain } = await load(registry === "npm" ? "scripts/ci/rk-npm-preflight.mjs" : "scripts/ci/rk-preflight.mjs");
    const code = await preflightMain(["--registry", registry, "--unit", unit], { root, toolsRoot, env, stdout: sink.put, stderr: sink.put });
    return { code, last: lastLine(sink.text()) };
  };
  const preflightMany = async (units, o) => {
    const results = new Map();
    const queue = [...units];
    const worker = async () => {
      while (queue.length) {
        const unit = queue.shift();
        let r;
        try { r = await preflightOne(unit, o.registry); } catch (error) { r = { code: 1, last: String(error?.message ?? error) }; }
        results.set(unit, r);
        log(`preflight ${unit}: ${r.code === 0 ? "GREEN" : "RED"} (${r.last.slice(0, 140)})`);
      }
    };
    await Promise.all(Array.from({ length: Math.min(o.jobs, units.length) }, worker));
    return results;
  };
  const canary = async (unit, sha) => {
    const { runCanary } = await load("scripts/ci/rk-release-canary.mjs");
    const r = await runCanary({ root, toolsRoot, unit, expectSha: sha });
    return { code: r.code, last: lastLine(r.lines.join("\n")) };
  };
  return { check, preflightMany, canary, preflight: async (unit, registry = opts.registry) => (await preflightMany([unit], { ...opts, registry })).get(unit) };
}

/**
 * The in-gate-process entry to the coordinator: `argv` is a command line (`canary @rightkit/release --sha <sha>`, `preflight ...`, `dispatch ...
 * --qualification <file>`, `watch --wait`), `root` the control-plane checkout the call is about, both explicit. It runs in the calling process, takes the
 * coordinator's lock for the state-changing commands, runs the check, pre-flight and canary stages from the installed gate modules (inGateDeps), and returns
 * `{ status, stdout, stderr }` as a child process would have, so the gate's release judges it identically. `deps` overrides the stages (tests).
 */
export async function runPipeline({ argv, root, env = process.env, platform = process.platform, stateFile = stateFileFor(platform), deps, toolsRoot, load }) {
  const out = capture(), err = capture();
  let status;
  await sinks.run({ log: out.put, fail: (l) => err.put(`rk-pipeline: ${l}`), env }, async () => {
    try {
      const probe = parseArgs(argv);
      const parsed = { ...probe, root: rootOf(root), stateFile };
      // The broker refuses an uncarried root request inside a managed snapshot (RK_CHECK_ERROR), so the control plane is never one: refused before anything starts.
      let snapshotsRoot = null;
      try { snapshotsRoot = (platform === "win32" ? path.win32 : path.posix).join(resolveManagedRoots({ env, platform }).dataRoot, "snapshots"); } catch { snapshotsRoot = null; }
      const apart = controlPlaneProblem({ checkout: parsed.root, snapshotsRoot, pathApi: platform === "win32" ? path.win32 : path.posix });
      if (apart) throw new Error(`${apart}; nothing was started`);
      const stages = deps ?? inGateDeps(parsed, { env, ...(toolsRoot ? { toolsRoot } : {}), ...(load ? { load } : {}) });
      status = await withCoordinatorLock(parsed.command, stateFile, () => main(argv, { root, stateFile, deps: stages }));
    } catch (error) {
      fail(error.message);
      status = 2;
    }
  });
  const text = (sink) => (sink.lines.length ? `${sink.text()}\n` : "");
  return { status, stdout: text(out), stderr: text(err) };
}

// ---------- the command line ----------

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  // The CLI wrapper keeps today's behavior: the workspace is the checkout this script is installed in, and the stages run as child processes from it.
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
  try {
    // A malformed command line is refused before the coordinator's lock (and with it the workspace scratch directory) is touched, as runPipeline does: usage errors need no state volume.
    parseArgs(process.argv.slice(2));
    process.exitCode = await withCoordinatorLock(process.argv[2], STATE_FILE, () => main(process.argv.slice(2), { root, stateFile: STATE_FILE }));
  } catch (error) { fail(error.message); process.exitCode = 2; }
}
