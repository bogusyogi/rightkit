#!/usr/bin/env node
// Local admission: scoped deterministic scans, then brokered compile rows. Runtime qualification stays hosted.
// Diagnostic shortcuts & scan-only CI routing cannot produce publication receipts.
import { spawn, spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
export const DEFAULT_OUT = "/Volumes/D/scratch/rk-pipeline/receipts";
export const SCHEMA = "rk-preflight.v2";


export function readJson(file, fallback) {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {
    return fallback;
  }
}

export function parseArgs(argv) {
  const opts = { out: DEFAULT_OUT, budgetMin: 15 };
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
    else if (arg === "--skip-tests" || arg === "--rows-only") throw new Error("partial preflights are not publication evidence");
    else throw new Error(`unknown argument ${arg}`);
  }
  if (opts.registry !== "crates") throw new Error("--registry must be crates; npm needs a broker-admitted build adapter");
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(opts.unit ?? "")) throw new Error("--unit names exactly one crate or package");
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
    rows.push({ id: `test${label(f)}`, kind: "test", args: ["test", "-p", unit, ...f, "--no-run", "--locked"], timeoutMinutes });
  }
  for (const [index, consumer] of consumerRows(unit, entry).entries()) {
    for (const [targetIndex, step] of consumerRowInvocations(consumer).entries()) {
      rows.push({ id: `consumer-${index}-${targetIndex}`, kind: "consumer", args: step.args, timeoutMinutes });
    }
  }
  for (const row of rows) if (!row.args.includes("--locked")) row.args.splice(1, 0, "--locked");
  return rows;
}

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
  else if (pins.actionlint && observed.actionlint !== pins.actionlint) problems.push(`actionlint ${observed.actionlint} is not the pinned ${pins.actionlint}`);
  const sc = pins.shellcheck?.[platform] ?? {};
  if (!sc.path || !exists(sc.path)) problems.push(`pinned shellcheck is missing at ${sc.path ?? `(no ${platform} path pinned in ci/rk-pipeline.json)`}`);
  else {
    const d = digest(sc.path);
    observed.shellcheck = { path: sc.path, sha256: d, version: probe(sc.path, ["--version"]) };
    if (sc.sha256 && d !== sc.sha256) problems.push(`shellcheck at ${sc.path} has sha256 ${d}, pinned ${sc.sha256}`);
  }
  // rustfmt: the exact binary the hosted jobs use (pinned toolchain), never "whatever is newest under $HOME".
  const fmt = pins.rustfmt ?? {};
  const fmtPath = fmt[platform]?.path;
  if (!fmtPath || !exists(fmtPath)) problems.push(`pinned rustfmt is missing at ${fmtPath ?? `(no ${platform} path pinned in ci/rk-pipeline.json)`}`);
  else {
    observed.rustfmt = { path: fmtPath, version: probe(fmtPath, ["--version"]) };
    if (fmt.version && observed.rustfmt.version !== fmt.version) problems.push(`rustfmt at ${fmtPath} is "${observed.rustfmt.version}", pinned "${fmt.version}" (toolchain ${fmt.toolchain ?? "?"})`);
  }
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
    child.on("close", (code) => {
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
  const pipeline = JSON.parse(fs.readFileSync(path.join(root, "ci", "rk-pipeline.json"), "utf8"));
  const config = readJson(path.join(root, "ci", "gate.config.json"), {});
  const receiptDir = path.join(opts.out, sha);
  fs.mkdirSync(receiptDir, { recursive: true });
  const logDir = fs.mkdtempSync(path.join(receiptDir, `${opts.unit.replaceAll("/", "__")}.logs-`));
  const receipt = { schema: SCHEMA, sha, registry: opts.registry, unit: opts.unit, startedAt: new Date().toISOString(), host: process.platform, route: "local", ok: false, tools: null, scans: null, rows: [], mode: "local-compile", gaps: ["runtime tests, other OS targets & sealed archive consumers remain hosted"] };
  const save = () => {
    const file = path.join(receiptDir, `${opts.unit.replaceAll("/", "__")}.json`);
    const temp = `${file}.${process.pid}.tmp`;
    fs.writeFileSync(temp, JSON.stringify(receipt, null, 2));
    fs.renameSync(temp, file);
  };
  save(); // Revoke any previous green before this attempt can fail or be interrupted.
  const unchanged = () => git("rev-parse", "HEAD") === sha && !git("status", "--porcelain", "--untracked-files=no");
  const rows = crateRows(opts.unit, config);
  receipt.requiredRows = rows.map((r) => r.id);
  const manifest = fs.readFileSync(path.join(root, "crates", opts.unit, "Cargo.toml"), "utf8");
  receipt.version = manifest.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  if (!receipt.version) throw new Error("unit has no explicit package version");

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
  const env = { ...process.env, PATH: `${scPath}${path.delimiter}${process.env.PATH ?? ""}`, CARGO_TERM_COLOR: "never", RUSTFMT: tools.observed.rustfmt.path, RUSTUP_TOOLCHAIN: pipeline.tools.rustfmt.toolchain };

  // 2. Deterministic scans, the hosted `scans` job on the same module with the same unit scope.
  {
    const scansOut = path.join(logDir, "scans");
    fs.rmSync(scansOut, { recursive: true, force: true });
    const r = await run(process.execPath, [path.join(root, "scripts", "ci", "run-deterministic-scans.mjs"), "--content-root", root, "--live-root", root, "--out", scansOut], {
      cwd: root,
      env: { ...env, PUBLISH_REGISTRY: opts.registry, PUBLISH_UNITS: opts.unit, GITHUB_SHA: sha },
      timeoutMinutes: 20,
      logFile: path.join(logDir, "scans.log"),
    });
    receipt.scans = { ok: r.code === 0 && !r.timedOut && unchanged(), timedOut: r.timedOut, exit: r.code, seconds: r.seconds, log: path.join(logDir, "scans.log") };
    out(`${receipt.scans.ok ? "PASS" : "FAIL"} scans (${r.seconds}s)`);
    if (!receipt.scans.ok) {
      receipt.failedAt = "scans";
      receipt.finishedAt = new Date().toISOString();
      save();
      for (const line of r.out.split("\n").filter((l) => /FAIL|refused/.test(l)).slice(0, 12)) err(`  ${line}`);
      return 1;
    }
  }

  // Compile only. Runtime tests & OS qualification belong on hosted runners.
  const rowEnv = { ...env, RIGHTKIT_REQUIRE_ASSETS: "1" };
  for (const row of rows) {
    if (!unchanged()) {
      receipt.failedAt = "source-changed"; save(); return 2;
    }
    const command = row.command ?? "rustright";
    const args = row.command ? row.args : ["cargo", ...row.args];
    const logFile = path.join(logDir, `${row.id.replace(/[^a-z0-9.-]+/gi, "_")}.log`);
    const r = await run(command, args, { cwd: root, env: rowEnv, timeoutMinutes: row.timeoutMinutes, logFile });
    const warnings = row.denyWarnings ? countDocWarnings(r.out) : 0;
    const ok = r.code === 0 && !r.timedOut && warnings === 0 && brokerRequestIds(r.out).length > 0 && unchanged();
    const entry = { id: row.id, kind: row.kind, command: `${command} ${args.join(" ")}`, exit: r.code, timedOut: r.timedOut, seconds: r.seconds, warnings, brokerRequests: brokerRequestIds(r.out), log: logFile, ok };
    receipt.rows.push(entry);
    out(`${ok ? "PASS" : "FAIL"} ${row.id} (${r.seconds}s${warnings ? `, ${warnings} doc warning(s)` : ""}${r.timedOut ? ", timed out" : ""})`);
    save();
    if (!ok) {
      receipt.failedAt = row.id;
      receipt.finishedAt = new Date().toISOString();
      save();
      for (const line of r.out.split("\n").filter((l) => /^(error|warning)(\[|:)/.test(l)).slice(0, 12)) err(`  ${line}`);
      return 1;
    }
  }
  receipt.ok = unchanged();
  receipt.seconds = Math.round((Date.now() - Date.parse(receipt.startedAt)) / 1000);
  receipt.budgetMinutes = opts.budgetMin;
  receipt.overBudget = receipt.seconds > opts.budgetMin * 60;
  receipt.finishedAt = new Date().toISOString();
  save();
  out(`${receipt.ok ? "GREEN" : "RED"} ${opts.unit} at ${sha.slice(0, 12)}: ${receipt.rows.length} row(s), receipt ${path.join(receiptDir, `${opts.unit.replaceAll("/", "__")}.json`)}`);
  return receipt.ok ? 0 : 2;
}

/** Structural proof, never a signature. Expected rows come from commit-bound policy, not receipt claims. */
export function receiptRefusal(receipt, { sha, registry, unit, config, version }) {
  if (!receipt) return "no pre-flight receipt for this commit";
  if (receipt.sha !== sha) return "receipt commit differs from requested commit";
  if (receipt.schema !== SCHEMA || receipt.mode !== "local-compile") return "obsolete or diagnostic receipt";
  if (receipt.registry !== registry || receipt.unit !== unit || receipt.version !== version) return "receipt unit/registry/version mismatch";
  if (receipt.route !== "local" || receipt.ok !== true || !receipt.finishedAt || receipt.failedAt) return "local compilation is incomplete or red";
  if (receipt.scans?.ok !== true || receipt.scans.exit !== 0 || receipt.scans.timedOut !== false) return "deterministic scans did not pass";
  const expected = crateRows(unit, config);
  if (!Array.isArray(receipt.rows) || receipt.rows.length !== expected.length || !expected.length) return "missing or extra compile rows";
  for (const [i, row] of expected.entries()) {
    const got = receipt.rows[i];
    if (got.id !== row.id || got.command !== `rustright cargo ${row.args.join(" ")}` || got.ok !== true || got.exit !== 0 || got.timedOut !== false || got.warnings !== 0 || !got.brokerRequests?.length || !got.brokerRequests.every((id) => /^[0-9a-f-]{36}$/.test(id))) return `unproven compile row ${row.id}`;
  }
  return null;
}

export function exceptionAllowsets(unit, version, config, pipeline) {
  const allow = { allow_host_exclusion: [], allow_no_test: [], allow_ignored: [], allow_missing_assets: [] };
  if (config.rust?.[unit]?.allowIgnored) allow.allow_ignored.push(unit);
  const policy = pipeline.dispatch?.exceptions?.[unit];
  if (policy) {
    if (policy.version !== version || !policy.reason || !Array.isArray(policy.allow)) throw new Error(`${unit}: exception policy needs exact version, reason & allow list`);
    for (const key of policy.allow) {
      if (!Object.hasOwn(allow, key)) throw new Error(`${unit}: unknown exception ${key}`);
      if (!allow[key].includes(unit)) allow[key].push(unit);
    }
  }
  return allow;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(await main(process.argv.slice(2)));
}
