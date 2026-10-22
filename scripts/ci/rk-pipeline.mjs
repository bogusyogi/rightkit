#!/usr/bin/env node
// rk-pipeline: the publication loop, in the order Adrian specified (2026-10-22): one entry through the RustRight broker,
// deterministic checks first, warm local compile of each unit before any hosted run, hosted qualification of many units
// in parallel, direct upload on green, consumers released as their dependencies reach the registry.
//
//   rk-pipeline.mjs ready        [--registry crates|npm]           units whose version is not on the registry, in dependency waves
//   rk-pipeline.mjs preflight    <unit...> [--jobs N]               rk-preflight each unit, N at a time, through the broker
//   rk-pipeline.mjs dispatch     <unit...> [--registry crates|npm]  hosted qualification, only with a green receipt for HEAD, HEAD on origin
//   rk-pipeline.mjs watch        [--wait]                          hosted run state per dispatched unit; upload and registry readback on green
//   rk-pipeline.mjs loop         [--registry crates] [--jobs N]     ready -> preflight -> dispatch -> watch, wave after wave, until nothing is ready
// Every compile and every dispatch goes through the broker. gh is used read-only, by absolute path, to watch runs.
import { spawn, spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { DEFAULT_OUT, ROOT, readJson } from "./rk-preflight.mjs";

export const REPO = "bogusyogi/rightkit";
export const STATE_FILE = "/Volumes/D/scratch/rk-pipeline/state.json";
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
    else opts.units.push(arg);
  }
  if (!["ready", "preflight", "dispatch", "watch", "loop"].includes(command)) throw new Error("command must be ready, preflight, dispatch, watch or loop");
  if (!["crates", "npm"].includes(opts.registry)) throw new Error("--registry must be crates or npm");
  if (!Number.isInteger(opts.jobs) || opts.jobs < 1) throw new Error("--jobs must be a positive integer");
  return opts;
}

// ---------- registry readiness ----------

function ver(s) {
  return s.split(".").map(Number);
}

/** Cargo caret semantics: does the published version satisfy the requirement? */
export function satisfies(req, have) {
  if (!have) return false;
  const r = ver(req);
  const h = ver(have);
  if (r[0] === 0) return h[0] === 0 && h[1] === r[1] && (h[2] ?? 0) >= (r[2] ?? 0);
  return h[0] === r[0] && (h[1] > r[1] || (h[1] === r[1] && (h[2] ?? 0) >= (r[2] ?? 0)));
}

/** Workspace crates: name, version, and the path dependencies with their version requirements. */
export function readCrates(root = ROOT) {
  const crates = new Map();
  for (const dir of fs.readdirSync(path.join(root, "crates"))) {
    const file = path.join(root, "crates", dir, "Cargo.toml");
    if (!fs.existsSync(file)) continue;
    const text = fs.readFileSync(file, "utf8");
    const name = text.match(/^name\s*=\s*"([^"]+)"/m)?.[1];
    const version = text.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
    if (!name || !version) continue;
    const publish = !/^publish\s*=\s*false/m.test(text);
    const deps = [];
    for (const m of text.matchAll(/^(rightkit-[a-z0-9-]+)\s*=\s*\{([^}]*)\}/gm)) {
      if (!/path\s*=/.test(m[2])) continue;
      const req = m[2].match(/version\s*=\s*"([^"]+)"/)?.[1];
      if (req) deps.push({ name: m[1], req });
    }
    for (const m of text.matchAll(/^\[(?:dev-|build-)?dependencies\.(rightkit-[a-z0-9-]+)\]\s*\n((?:(?!\[).*\n)*)/gm)) {
      if (!/path\s*=/.test(m[2])) continue;
      const req = m[2].match(/version\s*=\s*"([^"]+)"/)?.[1];
      if (req) deps.push({ name: m[1], req });
    }
    crates.set(name, { name, version, publish, deps });
  }
  return crates;
}

export async function registryVersion(name, registry, fetchImpl = fetch) {
  try {
    if (registry === "crates") {
      const r = await fetchImpl(`https://crates.io/api/v1/crates/${name}`, { headers: { "User-Agent": "rk-pipeline (adrian@orthiclabs.com)" } });
      if (!r.ok) return null;
      return (await r.json()).crate?.max_version ?? null;
    }
    const r = await fetchImpl(`https://registry.npmjs.org/${name}/latest`);
    if (!r.ok) return null;
    return (await r.json()).version ?? null;
  } catch {
    return null;
  }
}

/** Unpublished units in waves: a unit is ready when every unpublished dependency's requirement is met by the registry. */
export function waves(crates, registryVersions) {
  const unpublished = new Set([...crates.values()].filter((c) => c.publish && registryVersions.get(c.name) !== c.version).map((c) => c.name));
  const blockers = new Map();
  for (const name of unpublished) blockers.set(name, crates.get(name).deps.filter((d) => unpublished.has(d.name) && !satisfies(d.req, registryVersions.get(d.name))).map((d) => d.name));
  const out = [];
  const done = new Set();
  while (done.size < unpublished.size) {
    const ready = [...unpublished].filter((n) => !done.has(n) && blockers.get(n).every((b) => done.has(b))).sort();
    if (!ready.length) {
      out.push({ stuck: [...unpublished].filter((n) => !done.has(n)) });
      break;
    }
    out.push(ready);
    for (const n of ready) done.add(n);
  }
  return { unpublished, blockers, waves: out };
}

async function readiness(opts) {
  const crates = readCrates();
  const versions = new Map();
  await Promise.all([...crates.values()].filter((c) => c.publish).map(async (c) => versions.set(c.name, await registryVersion(c.name, opts.registry))));
  return { crates, versions, ...waves(crates, versions) };
}

// ---------- receipts, allowsets, state ----------

export function receiptFor(unit, sha, out = DEFAULT_OUT) {
  const file = path.join(out, sha, `${unit.replaceAll("/", "__")}.json`);
  return fs.existsSync(file) ? { file, ...readJson(file, {}) } : null;
}

/** Why a unit may not be dispatched at this commit, or null when it may. A green local receipt, or a CI-routed one whose
 * deterministic scans passed, is required; nothing else counts (Adrian: local before hosted). */
export function dispatchRefusal(receipt, sha) {
  if (!receipt) return "no pre-flight receipt for this commit; run rk-pipeline preflight first";
  if (receipt.sha !== sha) return `receipt is for ${String(receipt.sha).slice(0, 12)}, HEAD is ${sha.slice(0, 12)}`;
  if (receipt.route === "ci") return receipt.scans?.ok ? null : "routed to CI but its deterministic scans did not pass";
  if (!receipt.ok) return `pre-flight is red at ${receipt.failedAt ?? "unknown row"}`;
  return null;
}

/** The allowsets the hosted planner demands for these units, derived from its own refusals. */
export async function allowsetsFor(registry, units, planFn) {
  const allow = { allow_host_exclusion: [], allow_no_test: [], allow_ignored: [], allow_missing_assets: [] };
  const config = readJson(path.join(ROOT, "ci", "gate.config.json"), {});
  for (const unit of units) {
    const entry = (registry === "crates" ? config.rust?.[unit] : config.npm?.[unit]) ?? {};
    if (entry.allowIgnored || entry.ignoredTests) allow.allow_ignored.push(unit);
  }
  for (let round = 0; round < 3; round += 1) {
    // planPublication throws one error listing every refusal as "  - <unit>: <reason>" lines.
    let errors = [];
    try {
      await planFn({ registry, unitsText: units.join(" "), dryRun: false, allowHostExclusionText: allow.allow_host_exclusion.join(" "), allowNoTestText: allow.allow_no_test.join(" "), allowIgnoredText: allow.allow_ignored.join(" "), allowMissingAssetsText: allow.allow_missing_assets.join(" ") });
    } catch (error) {
      errors = String(error.message).split("\n").map((l) => l.replace(/^\s*-\s*/, "").trim()).filter((l) => l && !/^publish request refused/.test(l));
    }
    let changed = false;
    for (const e of errors) {
      const m = e.match(/^([^:]+): .*name it in (allow_host_exclusion|allow_no_test|allow_missing_assets)/);
      if (m && !allow[m[2]].includes(m[1])) {
        allow[m[2]].push(m[1]);
        changed = true;
      }
    }
    if (!changed) return { allow, errors };
  }
  return { allow, errors: ["allowset derivation did not converge"] };
}

export function readState() {
  return readJson(STATE_FILE, { dispatches: {} });
}

export function writeState(state) {
  fs.mkdirSync(path.dirname(STATE_FILE), { recursive: true });
  fs.writeFileSync(STATE_FILE, JSON.stringify(state, null, 2));
}

function git(...args) {
  const r = spawnSync("git", args, { cwd: ROOT, encoding: "utf8" });
  if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr.trim()}`);
  return r.stdout.trim();
}

function gh(args) {
  const r = spawnSync(GH, args, { encoding: "utf8", env: { ...process.env, GH_NO_UPDATE_NOTIFIER: "1", GH_PROMPT_DISABLED: "1" }, timeout: 60_000 });
  return { ok: r.status === 0, stdout: r.stdout ?? "" };
}

function runChild(command, args, env = process.env) {
  return new Promise((resolve) => {
    let out = "";
    const child = spawn(command, args, { cwd: ROOT, env, stdio: ["ignore", "pipe", "pipe"] });
    child.stdout.on("data", (c) => (out += c));
    child.stderr.on("data", (c) => (out += c));
    child.on("exit", (code) => resolve({ code: code ?? 1, out }));
    child.on("error", (e) => resolve({ code: 1, out: out + String(e) }));
  });
}

// ---------- commands ----------

async function cmdReady(opts) {
  const r = await readiness(opts);
  log(`unpublished (${r.unpublished.size}): ${[...r.unpublished].sort().join(" ")}`);
  r.waves.forEach((w, i) => log(Array.isArray(w) ? `wave ${i + 1} (${w.length}): ${w.join(" ")}` : `stuck: ${w.stuck.join(" ")}`));
  return 0;
}

async function preflightMany(units, opts) {
  const results = new Map();
  const queue = [...units];
  const worker = async () => {
    while (queue.length) {
      const unit = queue.shift();
      const r = await runChild(NODE, [path.join(ROOT, "scripts", "ci", "rk-preflight.mjs"), "--registry", opts.registry, "--unit", unit]);
      const last = r.out.trim().split("\n").filter(Boolean).slice(-1)[0] ?? "";
      results.set(unit, { code: r.code, last });
      log(`preflight ${unit}: ${r.code === 0 ? "GREEN" : r.code === 3 ? "ROUTE-CI" : "RED"} (${last.slice(0, 140)})`);
    }
  };
  await Promise.all(Array.from({ length: Math.min(opts.jobs, units.length) }, worker));
  return results;
}

async function cmdPreflight(opts) {
  if (!opts.units.length) {
    fail("name at least one unit");
    return 2;
  }
  const results = await preflightMany(opts.units, opts);
  return [...results.values()].every((r) => r.code === 0 || r.code === 3) ? 0 : 1;
}

async function cmdDispatch(opts, deps = {}) {
  if (!opts.units.length) {
    fail("name at least one unit");
    return 2;
  }
  const sha = git("rev-parse", "HEAD");
  const remote = git("ls-remote", "origin", "refs/heads/main").split(/\s+/)[0];
  if (remote !== sha) {
    fail(`HEAD ${sha.slice(0, 12)} is not origin/main ${String(remote).slice(0, 12)}; push first, the dispatcher pins the commit`);
    return 2;
  }
  const refused = [];
  const go = [];
  for (const unit of opts.units) {
    const why = dispatchRefusal(receiptFor(unit, sha), sha);
    if (why) refused.push(`${unit}: ${why}`);
    else go.push(unit);
  }
  for (const r of refused) fail(`refused ${r}`);
  if (!go.length) return 2;
  const planFn = deps.planFn ?? (await import("./publish.mjs")).planPublication;
  const { allow, errors } = await allowsetsFor(opts.registry, go, planFn);
  if (errors.length) {
    for (const e of errors) fail(`plan: ${e}`);
    return 2;
  }
  const state = readState();
  let exit = 0;
  for (const unit of go) {
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
      exit = 1;
      continue;
    }
    const prior = state.dispatches[unit]?.attempts ?? 0;
    state.dispatches[unit] = { registry: opts.registry, sha, runId: record.runId, runUrl: record.runUrl, requestId: record.requestId, brokerRequest: request, nonce, attempts: prior + 1, dispatchedAt: record.dispatchedAt, status: "dispatched" };
    writeState(state);
    log(`dispatched ${unit}: run ${record.runId} (${record.runUrl ?? "no url"}), broker request ${request}, attempt ${prior + 1}`);
  }
  return exit;
}

function runState(runId) {
  const r = gh(["api", `repos/${REPO}/actions/runs/${runId}`, "--jq", "{status: .status, conclusion: .conclusion, head_sha: .head_sha}"]);
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

function uploadRun(sha, dispatchedAt) {
  const r = gh(["api", `repos/${REPO}/actions/runs?event=workflow_run&head_sha=${sha}&per_page=30`, "--jq", `[.workflow_runs[] | select(.path | endswith("publish-direct.yml")) | select(.created_at >= "${dispatchedAt}") | {id, status, conclusion, created_at}]`]);
  if (!r.ok) return [];
  try {
    return JSON.parse(r.stdout);
  } catch {
    return [];
  }
}

async function cmdWatch(opts) {
  const state = readState();
  const open = Object.entries(state.dispatches).filter(([, d]) => d.status !== "published" && d.status !== "failed");
  if (!open.length) {
    log("nothing dispatched is pending");
    return 0;
  }
  for (;;) {
    let pending = 0;
    for (const [unit, d] of open) {
      if (d.status === "published" || d.status === "failed") continue;
      const run = runState(d.runId);
      if (!run) {
        log(`${unit}: run ${d.runId} not readable`);
        pending += 1;
        continue;
      }
      if (run.status !== "completed") {
        log(`${unit}: run ${d.runId} ${run.status}`);
        pending += 1;
        continue;
      }
      if (run.conclusion !== "success") {
        d.status = "failed";
        d.conclusion = run.conclusion;
        d.failedSteps = failedSteps(d.runId);
        log(`${unit}: run ${d.runId} ${run.conclusion} <- ${d.failedSteps}`);
        continue;
      }
      const uploads = uploadRun(d.sha, d.dispatchedAt);
      const done = uploads.find((u) => u.status === "completed");
      if (!done) {
        log(`${unit}: qualification green, upload ${uploads[0]?.status ?? "not started"}`);
        pending += 1;
        continue;
      }
      const published = await registryVersion(unit, d.registry);
      const crate = readCrates().get(unit);
      if (done.conclusion === "success" && crate && published === crate.version) {
        d.status = "published";
        d.published = published;
        d.uploadRunId = done.id;
        log(`${unit}: PUBLISHED ${published} (upload run ${done.id})`);
      } else {
        d.status = "failed";
        d.conclusion = `upload ${done.conclusion}, registry has ${published ?? "nothing"}`;
        d.uploadRunId = done.id;
        log(`${unit}: upload run ${done.id} ${done.conclusion}; registry has ${published ?? "nothing"}`);
      }
    }
    writeState(state);
    if (!pending || !opts.wait) return pending ? 1 : 0;
    await new Promise((r) => setTimeout(r, 60_000));
  }
}

async function cmdLoop(opts) {
  const pipeline = readJson(path.join(ROOT, "ci", "rk-pipeline.json"), {});
  const maxAttempts = pipeline.dispatch?.maxAttemptsPerUnit ?? 3;
  for (let wave = 1; wave <= 12; wave += 1) {
    const r = await readiness(opts);
    const state = readState();
    const ready = (Array.isArray(r.waves[0]) ? r.waves[0] : []).filter((u) => !opts.units.length || opts.units.includes(u));
    const candidates = ready.filter((u) => state.dispatches[u]?.status !== "published" && (state.dispatches[u]?.attempts ?? 0) < maxAttempts && state.dispatches[u]?.status !== "dispatched");
    if (!candidates.length) {
      log(`wave ${wave}: nothing ready to dispatch (${ready.length} ready, all published, in flight, or over ${maxAttempts} attempts)`);
      return 0;
    }
    log(`wave ${wave}: ${candidates.length} unit(s) ready: ${candidates.join(" ")}`);
    const results = await preflightMany(candidates, opts);
    const green = candidates.filter((u) => [0, 3].includes(results.get(u)?.code));
    if (green.length) await cmdDispatch({ ...opts, units: green });
    const watch = await cmdWatch({ ...opts, wait: true });
    if (watch !== 0) log(`wave ${wave}: some units are red; fix them and run again`);
  }
  return 0;
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
  process.exit(await main(process.argv.slice(2)));
}
