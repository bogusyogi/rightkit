#!/usr/bin/env node
// rk-preflight: the local, broker-admitted pre-flight of ONE publication unit at the checked-out commit, run before
// any hosted qualification is dispatched (Adrian, 2026-10-22: "deterministic checks first ... if the local compiler will
// take barely any time because the cache is warm, then it goes local. Otherwise it goes to CI").
//
// Order, cheapest first, stopping at the first red:
//   1. the publish.yml `scans` job, replayed through the same module with the same unit scope (seconds);
//   2. the unit's own gate rows from ci/gate.config.json, every cargo call through the RustRight broker:
//      clippy -D warnings, check, doc (zero warnings; the hosted row denies warnings), then tests;
//   3. a time budget on the first compile row: a cold unit is routed to CI with the deterministic receipt only.
// Consumer rows need the registry version and run only in hosted qualification; the receipt says so.
// The receipt names the commit, the unit, every row with its exit, seconds, warnings and broker request ids, and the
// tool versions. The pipeline driver refuses to dispatch a unit without a green receipt for the exact commit.
//
//   rk-preflight.mjs --registry crates|npm --unit <name> [--out <dir>] [--budget-min N] [--skip-tests] [--rows-only]
// Exit 0 green, 1 red, 2 usage or precondition, 3 routed to CI (cold).
import { spawn, spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
export const DEFAULT_OUT = "/Volumes/D/scratch/rk-pipeline/receipts";
export const SCHEMA = "rk-preflight.v1";
const PIPELINE_CONFIG = path.join(ROOT, "ci", "rk-pipeline.json");

export function readJson(file, fallback) {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {
    return fallback;
  }
}

export function parseArgs(argv) {
  const opts = { out: DEFAULT_OUT, budgetMin: 15, skipTests: false, rowsOnly: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const take = () => {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith("--")) throw new Error(`${arg} needs a value`);
      i += 1;
      return value;
    };
    if (arg === "--registry") opts.registry = take();
    else if (arg === "--unit") opts.unit = take();
    else if (arg === "--out") opts.out = path.resolve(take());
    else if (arg === "--budget-min") opts.budgetMin = Number(take());
    else if (arg === "--skip-tests") opts.skipTests = true;
    else if (arg === "--rows-only") opts.rowsOnly = true;
    else throw new Error(`unknown argument ${arg}`);
  }
  if (!["crates", "npm"].includes(opts.registry)) throw new Error("--registry must be crates or npm");
  if (!/^[@a-z0-9][a-z0-9._/-]*$/i.test(opts.unit ?? "")) throw new Error("--unit names exactly one crate or package");
  if (!Number.isFinite(opts.budgetMin) || opts.budgetMin <= 0) throw new Error("--budget-min must be a positive number");
  return opts;
}

/** The hosted gate's rows for one crate, from ci/gate.config.json, cheapest first (scripts/ci/gate.mjs test loop). */
export function crateRows(unit, config) {
  const entry = config.rust?.[unit] ?? {};
  const featureSets = entry.featureSets ?? [[]];
  const checkSets = featureSets.some((f) => f.includes("--all-features")) ? [] : (entry.checkFeatureSets ?? [["--all-features"]]);
  const docSets = [...new Map([...featureSets, ...checkSets].map((f) => [JSON.stringify(f), f])).values()];
  const timeoutMinutes = entry.timeoutMinutes ?? 25;
  const rows = [];
  for (const f of featureSets) rows.push({ id: `clippy${label(f)}`, kind: "clippy", args: ["clippy", "-p", unit, "--no-deps", "--all-targets", ...f, "--", "-D", "warnings"], timeoutMinutes });
  for (const f of checkSets) rows.push({ id: `check${label(f)}`, kind: "check", args: ["check", "-p", unit, "--all-targets", ...f], timeoutMinutes });
  for (const f of docSets) rows.push({ id: `doc${label(f)}`, kind: "doc", args: ["doc", "-p", unit, "--no-deps", ...f], timeoutMinutes, denyWarnings: true });
  for (const f of featureSets) {
    rows.push({ id: `test${label(f)}`, kind: "test", args: ["test", "-p", unit, ...f, "--no-fail-fast", "--", "--show-output"], timeoutMinutes });
    if (entry.alsoSerial) rows.push({ id: `test-serial${label(f)}`, kind: "test", args: ["test", "-p", unit, ...f, "--no-fail-fast", "--", "--test-threads=1", "--show-output"], timeoutMinutes });
  }
  return rows;
}

function label(features) {
  return features.length ? `[${features.filter((x) => !x.startsWith("--")).join(",") || features.join(" ")}]` : "";
}

/** Rustdoc warnings as the hosted `-D warnings` row would see them: every `warning:` line except cargo's own summary. */
export function countDocWarnings(text) {
  return text.split("\n").filter((line) => /^warning: /.test(line) && !/generated \d+ warning/.test(line)).length;
}

export function brokerRequestIds(text) {
  return [...text.matchAll(/^rustright: request ([0-9a-f-]{36})/gm)].map((m) => m[1]);
}

export function sha256File(file) {
  return crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

/** Tool parity with the hosted scans job: pinned actionlint version, pinned shellcheck binary (actionlint silently skips
 * shell checks without it, which is how SC2016 reached a hosted run), rustfmt and node present. */
export function checkTools({ pins, spawnFn = spawnSync, exists = fs.existsSync, digest = sha256File, platform = process.platform }) {
  const problems = [];
  const observed = {};
  const probe = (cmd, args) => {
    const r = spawnFn(cmd, args, { encoding: "utf8" });
    return r.error || r.status !== 0 ? null : String(r.stdout || r.stderr).trim().split("\n")[0];
  };
  observed.actionlint = probe("actionlint", ["-version"]);
  if (!observed.actionlint) problems.push("actionlint is not on PATH");
  else if (pins.actionlint && !observed.actionlint.startsWith(pins.actionlint)) problems.push(`actionlint ${observed.actionlint} is not the pinned ${pins.actionlint}`);
  const sc = pins.shellcheck?.[platform] ?? {};
  if (!sc.path || !exists(sc.path)) problems.push(`pinned shellcheck is missing at ${sc.path ?? `(no ${platform} path pinned in ci/rk-pipeline.json)`}`);
  else {
    const d = digest(sc.path);
    observed.shellcheck = { path: sc.path, sha256: d, version: probe(sc.path, ["--version"]) };
    if (sc.sha256 && d !== sc.sha256) problems.push(`shellcheck at ${sc.path} has sha256 ${d}, pinned ${sc.sha256}`);
  }
  observed.rustfmt = probe("rustfmt", ["--version"]);
  if (!observed.rustfmt) problems.push("rustfmt is not on PATH");
  observed.node = process.version;
  return { observed, problems };
}

function run(command, args, { cwd, env, timeoutMinutes, logFile }) {
  return new Promise((resolve) => {
    const started = Date.now();
    let out = "";
    const child = spawn(command, args, { cwd, env, stdio: ["ignore", "pipe", "pipe"] });
    const log = fs.createWriteStream(logFile);
    const sink = (chunk) => {
      const text = chunk.toString();
      out += text;
      log.write(text);
    };
    child.stdout.on("data", sink);
    child.stderr.on("data", sink);
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGTERM");
    }, timeoutMinutes * 60_000);
    child.on("exit", (code) => {
      clearTimeout(timer);
      log.end();
      resolve({ code: code ?? 1, timedOut, seconds: Math.round((Date.now() - started) / 1000), out });
    });
    child.on("error", (error) => {
      clearTimeout(timer);
      log.end();
      resolve({ code: 1, timedOut: false, seconds: Math.round((Date.now() - started) / 1000), out: out + String(error) });
    });
  });
}

export async function main(argv, deps = {}) {
  const out = deps.stdout ?? ((line) => process.stdout.write(`${line}\n`));
  const err = deps.stderr ?? ((line) => process.stderr.write(`${line}\n`));
  let opts;
  try {
    opts = parseArgs(argv);
  } catch (error) {
    err(`rk-preflight: ${error.message}`);
    return 2;
  }
  const root = deps.root ?? ROOT;
  const git = (...args) => {
    const r = spawnSync("git", args, { cwd: root, encoding: "utf8" });
    if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr.trim()}`);
    return r.stdout.trim();
  };
  const dirty = git("status", "--porcelain", "--untracked-files=no");
  if (dirty) {
    err(`rk-preflight: tracked files are modified; the pre-flight proves a commit, not a working tree:\n${dirty}`);
    return 2;
  }
  const sha = git("rev-parse", "HEAD");
  const pipeline = readJson(PIPELINE_CONFIG, {});
  const config = readJson(path.join(root, "ci", "gate.config.json"), {});
  const receiptDir = path.join(opts.out, sha);
  fs.mkdirSync(receiptDir, { recursive: true });
  const logDir = path.join(receiptDir, `${opts.unit.replaceAll("/", "__")}.logs`);
  fs.mkdirSync(logDir, { recursive: true });
  const receipt = { schema: SCHEMA, sha, registry: opts.registry, unit: opts.unit, startedAt: new Date().toISOString(), host: process.platform, route: "local", ok: false, tools: null, scans: null, rows: [], gaps: ["consumer rows need the registry version; they run only in hosted qualification"] };
  const save = () => fs.writeFileSync(path.join(receiptDir, `${opts.unit.replaceAll("/", "__")}.json`), JSON.stringify(receipt, null, 2));

  // 1. Tool parity.
  const tools = checkTools({ pins: pipeline.tools ?? {} });
  receipt.tools = tools.observed;
  if (tools.problems.length) {
    receipt.problems = tools.problems;
    save();
    for (const p of tools.problems) err(`rk-preflight: ${p}`);
    return 2;
  }
  const scPath = path.dirname(pipeline.tools.shellcheck[process.platform].path);
  const env = { ...process.env, PATH: `${scPath}${path.delimiter}${process.env.PATH ?? ""}`, CARGO_TERM_COLOR: "never" };

  // 2. Deterministic scans, the hosted `scans` job on the same module with the same unit scope.
  if (!opts.rowsOnly) {
    const scansOut = path.join(logDir, "scans");
    fs.rmSync(scansOut, { recursive: true, force: true });
    const r = await run(process.execPath, [path.join(root, "scripts", "ci", "run-deterministic-scans.mjs"), "--content-root", root, "--live-root", root, "--out", scansOut], {
      cwd: root,
      env: { ...env, PUBLISH_REGISTRY: opts.registry, PUBLISH_UNITS: opts.unit, GITHUB_SHA: sha },
      timeoutMinutes: 20,
      logFile: path.join(logDir, "scans.log"),
    });
    receipt.scans = { ok: r.code === 0, exit: r.code, seconds: r.seconds, log: path.join(logDir, "scans.log") };
    out(`${r.code === 0 ? "PASS" : "FAIL"} scans (${r.seconds}s)`);
    if (r.code !== 0) {
      receipt.failedAt = "scans";
      receipt.finishedAt = new Date().toISOString();
      save();
      for (const line of r.out.split("\n").filter((l) => /FAIL|refused/.test(l)).slice(0, 12)) err(`  ${line}`);
      return 1;
    }
  }

  // 3. Unit rows through the broker, cheapest first; the first compile row carries the local-vs-CI budget.
  const assetGated = readJson(path.join(root, "ci", "asset-gated.json"), {})[opts.registry] ?? {};
  const rowEnv = { ...env, RIGHTKIT_REQUIRE_ASSETS: assetGated[opts.unit] ? "0" : "1" };
  let rows;
  if (opts.registry === "crates") rows = crateRows(opts.unit, config);
  else {
    const pkg = readJson(path.join(root, "packages", opts.unit.replace(/^@rightkit\//, ""), "package.json"), null);
    rows = pkg?.scripts?.test ? [{ id: "npm-test", kind: "test", args: ["--filter", opts.unit, "test"], command: "pnpm", timeoutMinutes: 25 }] : [];
  }
  if (opts.skipTests) rows = rows.filter((row) => row.kind !== "test");
  let first = true;
  for (const row of rows) {
    const command = row.command ?? "rustright";
    const args = row.command ? row.args : ["cargo", ...row.args];
    const logFile = path.join(logDir, `${row.id.replace(/[^a-z0-9.-]+/gi, "_")}.log`);
    const r = await run(command, args, { cwd: root, env: rowEnv, timeoutMinutes: row.timeoutMinutes, logFile });
    const warnings = row.denyWarnings ? countDocWarnings(r.out) : 0;
    const ok = r.code === 0 && !r.timedOut && warnings === 0;
    const entry = { id: row.id, kind: row.kind, command: `${command} ${args.join(" ")}`, exit: r.code, timedOut: r.timedOut, seconds: r.seconds, warnings, brokerRequests: brokerRequestIds(r.out), log: logFile, ok };
    receipt.rows.push(entry);
    out(`${ok ? "PASS" : "FAIL"} ${row.id} (${r.seconds}s${warnings ? `, ${warnings} doc warning(s)` : ""}${r.timedOut ? ", timed out" : ""})`);
    save();
    if (first && row.kind !== "test" && r.seconds > opts.budgetMin * 60 && ok) {
      // Cold here: the compile itself is fine but beyond the local budget, so the rest of the proof is CI's.
      receipt.route = "ci";
      receipt.routeReason = `first compile row took ${r.seconds}s, over the ${opts.budgetMin} min local budget`;
      receipt.finishedAt = new Date().toISOString();
      save();
      out(`ROUTE ci: ${receipt.routeReason}`);
      return 3;
    }
    first = false;
    if (!ok) {
      receipt.failedAt = row.id;
      receipt.finishedAt = new Date().toISOString();
      save();
      for (const line of r.out.split("\n").filter((l) => /^(error|warning)(\[|:)/.test(l)).slice(0, 12)) err(`  ${line}`);
      return 1;
    }
  }
  receipt.ok = true;
  receipt.finishedAt = new Date().toISOString();
  save();
  out(`GREEN ${opts.unit} at ${sha.slice(0, 12)}: ${receipt.rows.length} row(s), receipt ${path.join(receiptDir, `${opts.unit.replaceAll("/", "__")}.json`)}`);
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(await main(process.argv.slice(2)));
}
