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
import { spawn, spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { DEFAULT_OUT, ROOT, readJson, receiptRefusal, exceptionAllowsets } from "./rk-preflight.mjs";
import { packageRows, readPackages as readNpmPackages } from "./rk-npm-preflight.mjs";

export const REPO = "bogusyogi/rightkit";
export const STATE_FILE = "/Volumes/D/scratch/rk-pipeline/state.json";
import { readCrateWorkspace, readNpmWorkspace, crateRegistryVersions, npmRegistryVersions, satisfies as versionSatisfies } from "./publish.mjs";
const GH = process.platform === "win32" ? "C:\\Program Files\\GitHub CLI\\gh.exe" : "/opt/homebrew/bin/gh";
const RUSTRIGHT = "rustright";
const NODE = process.execPath;

const log = (line) => process.stdout.write(`${line}\n`);
const fail = (line) => process.stderr.write(`rk-pipeline: ${line}\n`);

export function parseArgs(argv) {
  const [command, ...rest] = argv;
  const opts = { command, units: [], registry: "crates", jobs: 2, wait: false };
  for (let i = 0; i < rest.length; i += 1) {
    const arg = rest[i];
    if (arg === "--registry") opts.registry = rest[++i];
    else if (arg === "--jobs") opts.jobs = Number(rest[++i]);
    else if (arg === "--wait") opts.wait = true;
    else if (arg.startsWith("--")) throw new Error(`unknown argument ${arg}`);
    else opts.units.push(...arg.split(/\s+/).filter(Boolean)); // one quoted list or many arguments, both fine
  }
  if (!["ready", "preflight", "canary", "dispatch", "watch", "loop"].includes(command)) throw new Error("command must be ready, preflight, canary, dispatch, watch or loop");
  if (!["crates", "npm"].includes(opts.registry) && !(command === "loop" && opts.registry === "all")) throw new Error("--registry must be crates or npm (loop also accepts all)");
  if (!Number.isInteger(opts.jobs) || opts.jobs < 1) throw new Error("--jobs must be a positive integer");
  opts.units = [...new Set(opts.units)];
  return opts;
}

// ---------- registry readiness ----------

/** Reuse publication semantics & Cargo metadata, including renamed/target/workspace dependencies. */
export const satisfies = (req, have) => Boolean(have) && versionSatisfies(have, req, "cargo");
export function readCrates(root = ROOT) {
  return new Map([...readCrateWorkspace(root)].map(([name, c]) => [name, {
    ...c, publish: !c.private,
    deps: c.deps.filter((d) => !(d.dev && !d.spec)).map((d) => ({ ...d, req: d.spec })),
  }]));
}
/** Workspace npm packages in the crate-record shape the planner uses: publish = not private; deps = the other workspace
 * packages it depends on (`workspace:` specs resolve to that member's version, so a sibling bump orders the waves). */
export function readPackages(root = ROOT) {
  const raw = readNpmWorkspace(root);
  return new Map([...raw].map(([name, p]) => [name, {
    name, version: p.version, dir: p.dir, publish: !p.private, hasTest: p.hasTest,
    deps: p.deps.filter((d) => raw.has(d.name)).map((d) => ({ name: d.name, req: /^workspace:/.test(d.spec) ? (d.spec === "workspace:*" || d.spec === "workspace:^" || d.spec === "workspace:~" ? `=${raw.get(d.name).version}` : d.spec.slice("workspace:".length)) : d.spec })),
  }]));
}
export function readUnits(registry, root = ROOT) {
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
  const crates = deps.crates ?? readUnits(opts.registry ?? "crates");
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
export function holdReason(unit, sha, state, crates, { pipeline = readJson(path.join(ROOT, "ci", "rk-pipeline.json"), {}), changed = changedBetween, show = showAt } = {}) {
  const prior = Object.values(state.dispatches ?? {}).filter((d) => d.unit === unit && d.status === "failed" && d.phase !== "preflight" && d.runId && d.sha && d.sha !== sha);
  if (!prior.length) return null;
  const last = prior.sort((a, b) => String(a.dispatchedAt ?? "").localeCompare(String(b.dispatchedAt ?? ""))).at(-1);
  const release = pipeline.dispatch?.releaseHolds?.[unit];
  if (release && String(release.run) === String(last.runId) && release.version === crates.get(unit)?.version && release.reason) return null;
  const dirs = [...closureOf(crates, [unit])].map((n) => crates.get(n).dir ?? `crates/${n}`);
  // The reviewed recipe lives at ci/consumers/<crate> or ci/consumers/npm/<package short name>; the inventory that admits
  // it (ci/consumer-recipes.json) is part of what the hosted verify checks, so a refreshed entry releases the hold.
  const recipeDir = unit.startsWith("@rightkit/") ? `ci/consumers/npm/${unit.slice("@rightkit/".length)}` : `ci/consumers/${unit}`;
  if (changed(last.sha, sha, [...dirs, recipeDir, "ci/consumer-recipes.json", "Cargo.lock"])) return null;
  const section = (file, pick) => { try { return JSON.stringify(pick(JSON.parse(show(last.sha, file)))) !== JSON.stringify(pick(JSON.parse(show(sha, file)))); } catch { return true; } };
  if (section("ci/gate.config.json", (j) => j.rust?.[unit]) || section("ci/rk-pipeline.json", (j) => j.dispatch?.exceptions?.[unit])) return null;
  return `held: unchanged since hosted failure at ${last.sha.slice(0, 12)} (run ${last.runId}); its crate closure, consumer recipe and own policy entries are identical, so the same run would repeat`;
}
function changedBetween(from, to, paths) {
  const r = spawnSync("git", ["diff", "--quiet", from, to, "--", ...paths], { cwd: ROOT, encoding: "utf8" });
  if (r.status === 0) return false;
  if (r.status === 1) return true;
  throw new Error(`git diff ${from}..${to}: ${(r.stderr ?? "").trim()}`);
}
function showAt(sha, file) { return git("show", `${sha}:${file}`); }

// ---------- receipts, allowsets, state ----------

export function receiptFor(unit, sha, out = DEFAULT_OUT) {
  const file = path.join(out, sha, `${unit.replaceAll("/", "__")}.json`);
  return fs.existsSync(file) ? { file, ...readJson(file, {}) } : null;
}

export function dispatchRefusal(receipt, sha, unit, registry = "crates", config = readJson(path.join(ROOT, "ci", "gate.config.json"), {}), version = readUnits(registry).get(unit)?.version, deps = {}) {
  // The same derivation the pinned dispatcher makes for an npm unit (rightkit-dispatch.mjs requirePreflight): the
  // package directory and the expected row plan come from the committed workspace and policy, never from the receipt.
  let expectedRows, packageDir;
  if (registry === "npm") {
    // The helper's own package record (with its scripts), exactly what the pinned dispatcher reads; the planner's
    // record (readPackages here) has no scripts and made packageRows throw at cf9ef9c3 ("reading 'test'").
    const pkg = (deps.readPackages ?? readNpmPackages)(deps.root ?? ROOT).get(unit);
    if (!pkg) return "unknown npm package";
    const pipeline = deps.pipeline ?? readJson(path.join(ROOT, "ci", "rk-pipeline.json"), {});
    packageDir = pkg.dir;
    try { expectedRows = (deps.packageRows ?? packageRows)(unit, pkg, { storeDir: pipeline.tools?.pnpm?.[process.platform]?.storeDir }); } catch (error) { return String(error.message); }
  }
  return receiptRefusal(receipt, { sha, unit, registry, config, version, expectedRows, packageDir, allowHostedCompile: registry === "crates" });
}

/** Planner refusals remain refusals. Only explicit version-bound policy supplies exceptions. */
export async function allowsetsFor(registry, units, planFn, { config = readJson(path.join(ROOT, "ci", "gate.config.json"), {}), pipeline = readJson(path.join(ROOT, "ci", "rk-pipeline.json"), {}), crates = readUnits(registry) } = {}) {
  const allow = { allow_host_exclusion: [], allow_no_test: [], allow_ignored: [], allow_missing_assets: [] };
  for (const unit of units) {
    const policy = exceptionAllowsets(unit, crates.get(unit)?.version, config, pipeline);
    for (const key of Object.keys(allow)) allow[key].push(...policy[key]);
  }
  try {
    const plan = await planFn({ registry, unitsText: units.join(" "), dryRun: false, allowHostExclusionText: allow.allow_host_exclusion.join(" "), allowNoTestText: allow.allow_no_test.join(" "), allowIgnoredText: allow.allow_ignored.join(" "), allowMissingAssetsText: allow.allow_missing_assets.join(" ") });
    return { allow, errors: [], plan };
  } catch (error) {
    return { allow, errors: [String(error.message)] };
  }
}
export const stateKey = (registry, sha, unit) => `${registry}:${sha}:${unit}`;
export function readState() {
  if (!fs.existsSync(STATE_FILE)) return { dispatches: {} };
  const state = JSON.parse(fs.readFileSync(STATE_FILE, "utf8"));
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

export function writeState(state) {
  fs.mkdirSync(path.dirname(STATE_FILE), { recursive: true });
  const temp = `${STATE_FILE}.${process.pid}.tmp`;
  fs.writeFileSync(temp, JSON.stringify(state, null, 2));
  fs.renameSync(temp, STATE_FILE);
}

function git(...args) {
  const r = spawnSync("git", args, { cwd: ROOT, encoding: "utf8" });
  if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr.trim()}`);
  return r.stdout.trim();
}

function gh(args) {
  const r = spawnSync(GH, args, { encoding: "utf8", env: { ...process.env, GH_NO_UPDATE_NOTIFIER: "1", GH_PROMPT_DISABLED: "1" }, timeout: 60_000, maxBuffer: 64 * 1024 * 1024 });
  return { ok: r.status === 0, stdout: r.stdout ?? "", error: r.error?.code ?? null, stderr: (r.stderr ?? "").slice(-2000) };
}

function runChild(command, args, env = process.env) {
  return new Promise((resolve) => {
    let out = "";
    const child = spawn(command, args, { cwd: ROOT, env, stdio: ["ignore", "pipe", "pipe"] });
    child.stdout.on("data", (c) => (out += c));
    child.stderr.on("data", (c) => (out += c));
    child.on("close", (code) => resolve({ code: code ?? 1, out }));
    child.on("error", (e) => resolve({ code: 1, out: out + String(e) }));
  });
}

// ---------- commands ----------

async function cmdReady(opts) {
  const r = await readiness(opts);
  log(`unpublished (${r.unpublished.size}): ${[...r.unpublished].sort().join(" ")}`);
  r.waves.forEach((w, i) => log(Array.isArray(w) ? `wave ${i + 1} (${w.length}): ${w.join(" ")}` : `stuck: ${w.stuck.join(" ")}`));
  return r.waves.some((w) => !Array.isArray(w)) ? 1 : 0;
}

/** Step 1 of docs/PIPELINE-ARCHITECTURE.md (P1): scripts/preflight.mjs over exactly `sha`'s committed files (`--commit`), in
 * seconds, before any canary, pre-flight compile or dispatch. A red check starts nothing. */
function runCheckChild(sha) {
  return runChild(NODE, [path.join(ROOT, "scripts", "preflight.mjs"), "--commit", sha]);
}

/** The check passes for `sha`, and HEAD is still `sha` once it has: another chat may commit to this shared checkout while it
 * runs, and every later stage reads HEAD, so a moved HEAD starts nothing (check A, commit B, canary B is the failure). */
async function checkPasses(check, sha, head) {
  const r = await check(sha);
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
      const r = await runChild(NODE, [path.join(ROOT, "scripts", "ci", script), "--registry", opts.registry, "--unit", unit]);
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
async function runCanaryChild(unit, sha) {
  const r = await runChild(NODE, [path.join(ROOT, "scripts", "ci", "rk-release-canary.mjs"), "--unit", unit, "--sha", sha]);
  const last = r.out.trim().split("\n").filter(Boolean).slice(-1)[0] ?? "";
  return { code: r.code, last };
}

export async function cmdCanary(opts, deps = {}) {
  if (!opts.units.length) { fail("name at least one unit"); return 2; }
  // P1: the canary is expensive work (it builds and runs every app against the candidate); the check runs first, on the same commit.
  const head = deps.head ?? (() => git("rev-parse", "HEAD"));
  const sha = head();
  if (!(await checkPasses(deps.check ?? runCheckChild, sha, head))) return 1;
  let worst = 0;
  for (const unit of opts.units) {
    if (!needsCanary(unit, "npm")) { fail(`${unit}: no pipeline canary for this unit`); worst = Math.max(worst, 2); continue; }
    const result = await (deps.canary ?? runCanaryChild)(unit, sha);
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
  const head = deps.head ?? (() => git("rev-parse", "HEAD"));
  const sha = head();
  if (!(await checkPasses(deps.check ?? runCheckChild, sha, head))) return 1;
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
  const head = deps.head ?? (() => git("rev-parse", "HEAD"));
  const sha = head();
  // P1: a dispatch starts hosted runners; the check runs first, on the commit it dispatches, whatever pre-flight receipts say.
  if (!(await checkPasses(deps.check ?? runCheckChild, sha, head))) return 1;
  if (git("status", "--porcelain", "--untracked-files=no")) throw new Error("tracked files changed before dispatch");
  const remote = git("ls-remote", "origin", "refs/heads/main").split(/\s+/)[0];
  if (remote !== sha) {
    fail(`HEAD ${sha.slice(0, 12)} is not origin/main ${String(remote).slice(0, 12)}; push first, the dispatcher pins the commit`);
    return 2;
  }
  const refused = [];
  const go = [];
  for (const unit of opts.units) {
    const why = dispatchRefusal(receiptFor(unit, sha), sha, unit, opts.registry);
    if (why) refused.push(`${unit}: ${why}`);
    else go.push(unit);
  }
  for (const r of refused) fail(`refused ${r}`);
  if (!go.length) return 2;
  const planFn = deps.planFn ?? (await import("./publish.mjs")).planPublication;
  let exit = refused.length ? 1 : 0;
  for (const unit of go) {
    const { allow, errors, plan } = await allowsetsFor(opts.registry, [unit], planFn);
    if (errors.length) { for (const e of errors) fail(`plan ${unit}: ${e}`); exit = 1; continue; }
    const planned = plan.units.find((u) => u.name === unit);
    if (!planned) { log(`${unit}: exact version already exists; no dispatch`); continue; }
    const key = stateKey(opts.registry, sha, unit);
    const state = readState();
    const claim = openClaim(state, { registry: opts.registry, unit, version: planned.version, sha });
    if (claim) { fail(`${unit}: ${planned.version} is already claimed by the dispatch at ${String(claim.sha).slice(0, 12)} (status ${claim.status}, run ${claim.runId ?? "?"}); not dispatching again`); exit = 1; continue; }
    const previous = state.dispatches[key];
    if (previous && !(previous.status === "failed" && previous.phase === "preflight") && ["dispatched", "dispatching", "unknown", "failed", "published"].includes(previous.status)) {
      fail(`${unit}: attempt already recorded as ${previous.status}; reconcile it before retrying`); exit = 1; continue;
    }
    // Persist intent before external effect. Ambiguous POST/discovery failures must never auto-retry.
    state.dispatches[key] = { unit, registry: opts.registry, sha, version: planned.version, status: "dispatching", ownerPid: process.pid, attempts: 1, dispatchedAt: new Date().toISOString() };
    writeState(state);
    const nonce = crypto.randomBytes(32).toString("hex");
    const args = ["run", "rightkit-dispatch", "--", "--workflow", "publish", "--registry", opts.registry, "--unit", unit, "--sha", sha];
    for (const [key, list] of Object.entries(allow)) if (list.includes(unit)) args.push(`--${key.replaceAll("_", "-")}`, unit);
    const r = await runChild(RUSTRIGHT, args, { ...process.env, RIGHTKIT_ATTEST_NONCE: nonce });
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
      const current = readState(); current.dispatches[key].status = "unknown"; writeState(current);
      exit = 1;
      continue;
    }
    const current = readState();
    const prior = (current.dispatches[key]?.attempts ?? 1) - 1;
    current.dispatches[key] = { unit, version: planned.version, runAttempt: record.runAttempt, registry: opts.registry, sha, runId: record.runId, runUrl: record.runUrl, requestId: record.requestId, brokerRequest: request, nonce, attempts: prior + 1, dispatchedAt: record.dispatchedAt, status: "dispatched" };
    writeState(current);
    log(`dispatched ${unit}: run ${record.runId} (${record.runUrl ?? "no url"}), broker request ${request}, attempt ${prior + 1}`);
  }
  return exit;
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
  const load = deps.readState ?? readState;
  const save = deps.writeState ?? writeState;
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
  const timeoutMs = (readJson(path.join(ROOT, "ci", "rk-pipeline.json"), {}).dispatch?.watchTimeoutMinutes ?? 180) * 60_000;
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
  const head = deps.head ?? (() => git("rev-parse", "HEAD"));
  const sha = head();
  const readyFn = deps.readiness ?? readiness;
  const stateFn = deps.readState ?? readState;
  const save = deps.writeState ?? writeState;
  const watch = deps.watch ?? cmdWatch;
  const preflight = deps.preflight ?? (async (unit, registry = opts.registry) => (await preflightMany([unit], { ...opts, registry })).get(unit));
  const registries = opts.registry === "all" ? ["crates", "npm"] : [opts.registry];
  const slot = (registry, unit) => `${registry}:${unit}`;
  const dispatch = deps.dispatch ?? cmdDispatch;
  const canary = deps.canary ?? runCanaryChild;
  const pause = deps.pause ?? (() => new Promise((r) => setTimeout(r, 15_000)));
  const hold = deps.holdReason ?? holdReason;
  if (!(await checkPasses(deps.check ?? runCheckChild, sha, head))) return 1;
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
      const reason = hold(unit, sha, state, r.crates);
      if (reason) { held.add(id); log(`${unit}: ${reason}`); continue; }
      attempted.add(id);
      const task = (async () => {
        if (head() !== sha) throw new Error(`HEAD moved from ${sha.slice(0, 12)} before ${unit} started; nothing more runs at the checked commit`);
        if (needsCanary(unit, registry)) {
          const verdict = await canary(unit, sha);
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

/** (2) Coordinator lock with stale-owner recovery: a lock whose recorded pid is dead is reclaimed; a live owner excludes. */
export function pidAlive(pid) {
  try { process.kill(pid, 0); return true; } catch (error) { return error.code === "EPERM"; }
}
export function acquireLock(lockFile, { alive = pidAlive, pid = process.pid } = {}) {
  fs.mkdirSync(path.dirname(lockFile), { recursive: true });
  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      const fd = fs.openSync(lockFile, "wx"); fs.writeFileSync(fd, JSON.stringify({ pid, at: new Date().toISOString() })); fs.closeSync(fd);
      return;
    } catch (error) {
      if (error.code !== "EEXIST") throw error;
      let owner = null;
      try { owner = JSON.parse(fs.readFileSync(lockFile, "utf8")).pid; } catch { owner = null; }
      if (owner && alive(owner)) throw new Error(`another coordinator (pid ${owner}) holds ${lockFile}; wait for it or stop it, never delete the lock by hand`);
      fs.unlinkSync(lockFile); // stale: owner gone; the state file is authoritative and the loop skips recorded dispatches
    }
  }
  throw new Error(`could not acquire ${lockFile}`);
}
export function releaseLock(lockFile, { pid = process.pid } = {}) {
  try { if (JSON.parse(fs.readFileSync(lockFile, "utf8")).pid === pid) fs.unlinkSync(lockFile); } catch { /* not ours or gone */ }
}

export async function main(argv) {
  let opts;
  try {
    opts = parseArgs(argv);
  } catch (error) {
    fail(error.message);
    return 2;
  }
  switch (opts.command) {
    case "ready":
      return cmdReady(opts);
    case "preflight":
      return cmdPreflight(opts);
    case "canary":
      return cmdCanary(opts);
    case "dispatch":
      return cmdDispatch(opts);
    case "watch":
      return cmdWatch(opts);
    case "loop":
      return cmdLoop(opts);
    default:
      return 2;
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const mutates = ["dispatch", "watch", "loop"].includes(process.argv[2]);
  const lock = `${STATE_FILE}.lock`;
  let locked = false;
  try {
    if (mutates) { acquireLock(lock); locked = true; }
    process.exitCode = await main(process.argv.slice(2));
  } catch (error) { fail(error.message); process.exitCode = 2; }
  finally { if (locked) releaseLock(lock); }
}
