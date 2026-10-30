#!/usr/bin/env node
// Local admission: scoped deterministic scans, then brokered compile rows. Runtime qualification stays hosted.
// Diagnostic shortcuts & scan-only CI routing cannot produce publication receipts.
import { spawn, spawnSync } from "node:child_process";
import crypto from "node:crypto";
import os from "node:os";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseYaml } from "./workflow-yaml.mjs";
import { cargoRecipe } from "./consumer-recipes.mjs";
import { routeCrate, readOnlyQueries } from "./rk-routing.mjs";
import { consumerStage } from "./rk-consumer-stage.mjs";
import { brokerClientInvocation } from "../../packages/release/broker-client.mjs";

export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
export const DEFAULT_OUT = "/Volumes/D/scratch/rk-pipeline/receipts";
export const SCHEMA = "rk-preflight.v2";

// Observations select where compilation occurs; hosted qualification remains mandatory.
export async function observeCrateRoute({ root, sha, unit, rows }, deps = {}) {
  const workspace = fs.realpathSync(root);
  for (const row of rows) resolveRowWorkspace({ root: workspace, row, sha });
  const historyFile = process.platform === "win32" ? "D:\\scratch\\rk-pipeline\\state.json" : "/Volumes/D/scratch/rk-pipeline/state.json";
  const pipeline = readJson(path.join(workspace, "ci", "rk-pipeline.json"), {});
  const rustright = nativeForecastClient(pipeline);
  return routeCrate({ workspace, sourceSha: sha, registry: "crates", unit, rows, state: readJson(historyFile, {}) },
    deps.queries ?? readOnlyQueries({ cwd: workspace, rustright }));
}

export function nativeForecastClient(pipeline, platform = process.platform) {
  const client = pipeline.attestation?.[platform]?.clientPath;
  const paths = platform === "win32" ? path.win32 : path.posix;
  if (typeof client !== "string" || !paths.isAbsolute(client) || (platform === "win32" && !/\.exe$/i.test(client))) {
    throw new Error("installed native forecast client path is not enrolled for this host");
  }
  return client;
}

/** Pinned dispatcher calls this only after independently deriving committed identity/rows. */
export async function recheckHostedAdmission({ receipt, root, sha, unit, rows, config, version }, deps = {}) {
  const reason = receiptRefusal(receipt, { sha, registry: "crates", unit, config, version, allowHostedCompile: true });
  if (reason || receipt.mode !== "hosted-compile") throw new Error(reason ?? "explicit hosted-compile receipt required");
  let checked = false;
  const code = await (deps.checks ?? main)(["--registry", "crates", "--unit", unit], {
    root, checksOnly: true, writeReceipt: false,
    onChecksPassed: actual => {
      if (actual.sha !== sha || actual.unit !== unit) throw new Error("deterministic recheck identity changed");
      checked = true;
    },
  });
  if (code !== 0 || !checked) throw new Error("fresh deterministic admission checks did not pass");
  const route = await (deps.observeRoute ?? observeCrateRoute)({ root, sha, unit, rows });
  if (route.route !== "ci" || route.local?.knownHistory !== true || !Number.isSafeInteger(route.local?.totalMs)
      || !Number.isSafeInteger(route.hosted?.estimateMs) || route.hosted.estimateMs < 0 || route.local.totalMs <= route.hosted.estimateMs) {
    throw new Error("fresh broker/API routing requires local compilation; run preflight again");
  }
  return route;
}


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
      rows.push({ id: `consumer-${index}-${targetIndex}`, kind: "consumer", cwd: step.cwd, args: step.args, timeoutMinutes });
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
  return targets.map((target) => ({ command: "cargo", cwd: path.posix.dirname(row.manifestPath), args: [row.kind, "--locked", ...(target ? ["--target", target] : []), ...row.args] }));
}

/** Prevent Cargo ancestor fallback: every consumer runs beside its exact committed manifest. */
export function resolveRowWorkspace({ root, row, sha, spawnFn = spawnSync }) {
  const cwd = path.resolve(root, row.cwd ?? ".");
  if (row.kind !== "consumer") return cwd;
  const relative = `${row.cwd}/Cargo.toml`;
  if (!CONSUMER_MANIFEST.test(relative) || relative.split("/").some((part) => part === "." || part === "..") || !/^[0-9a-f]{40}$/.test(sha ?? "")) throw new Error("consumer workspace requires validated path & full commit SHA");
  let ancestor = path.resolve(root);
  for (const part of row.cwd.split("/")) {
    ancestor = path.join(ancestor, part);
    const stat = fs.lstatSync(ancestor);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error(`consumer workspace is not a real directory: ${row.cwd}`);
  }
  const manifest = path.join(cwd, "Cargo.toml");
  const stat = fs.lstatSync(manifest);
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`consumer manifest must be a regular non-symlink file: ${relative}`);
  const committed = spawnFn("git", ["--no-replace-objects", "show", `${sha}:${relative}`], { cwd: root, encoding: null, windowsHide: true, timeout: 30_000, maxBuffer: 16 * 1024 * 1024 });
  if (committed.error || committed.status !== 0 || !Buffer.isBuffer(committed.stdout)) throw new Error(`consumer manifest is absent or unreadable in requested commit: ${relative}`);
  if (!fs.readFileSync(manifest).equals(committed.stdout)) throw new Error(`consumer manifest differs from requested commit: ${relative}`);
  return cwd;
}


function label(features) {
  return features.length ? `[${features.filter((x) => !x.startsWith("--")).join(",") || features.join(" ")}]` : "";
}

/** Rustdoc diagnostics for THIS unit, as the hosted `RUSTDOCFLAGS=-D warnings` row fails on them: a `warning:` line is
 * counted only after cargo has started documenting the unit and only when its `-->` location is inside the unit's own
 * crate directory. Compile warnings of dependencies (`Checking rightkit-crash ... warning: function is never used`) are not
 * rustdoc warnings and never fail the hosted row, so they do not fail this one (Astra review item h). */
export function countDocWarnings(text, unit) {
  const lines = text.split("\n");
  let started = !unit;
  let count = 0;
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    // cargo reaches the unit itself only after its dependencies: from its own `Checking`/`Documenting` line on, a
    // warning belongs to the unit unless rustdoc locates it in another crate's directory. A warning without any
    // `-->` line (rustdoc prints module-level `//!` link diagnostics that way: rightkit-onnx provision.rs, hosted run
    // 39878997992 after a local GREEN) therefore counts.
    if (unit && new RegExp(`^\\s*(Checking|Documenting) ${unit.replace(/[-/]/g, "[-/]")} `).test(line)) started = true;
    if (!started || !/^warning: /.test(line) || /generated \d+ warning/.test(line)) continue;
    if (!unit) { count += 1; continue; }
    const location = lines.slice(i + 1, i + 4).find((l) => /^\s*-->\s/.test(l));
    if (location === undefined || new RegExp(`-->\\s*crates[\\/]${unit}[\\/]`).test(location)) count += 1;
  }
  return count;
}

/** The hosted verify's receipt step refuses a publication whose committed registry consumer recipe is missing or differs
 * from the reviewed inventory (scripts/ci/consumer-recipes.mjs cargoRecipe). Same helper, same refusal text, before any
 * cargo row: a valid recipe passes, a missing recipe or a stale inventory hash refuses. */
export function consumerRecipePolicy(root, unit) {
  const manifest = path.join(root, "crates", unit, "Cargo.toml");
  if (!fs.existsSync(manifest)) return { ok: false, reason: `crates/${unit}/Cargo.toml is missing` };
  const version = fs.readFileSync(manifest, "utf8").match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  if (!version) return { ok: false, reason: `crates/${unit}/Cargo.toml has no version` };
  try {
    const recipe = cargoRecipe({ root, unit: { name: unit, version } });
    return { ok: true, pkId: recipe.evidence.pkId, sha256: recipe.evidence.sha256, version };
  } catch (error) {
    return { ok: false, reason: String(error?.message ?? error), version };
  }
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

/** The broker's own, signed account of one row. `rustright status <id>` (local broker socket) must return a provenance
 * envelope whose payload binds the row's fresh nonce, its job id, the exact cargo argv and a successful exit; then the
 * envelope's signature bytes are verified by `rustright verify-receipt` against this host's ENROLLED public key, key id
 * and broker digest from ci/rk-pipeline.json (never taken from the receipt). The request digest is passed from the
 * receipt, so that one check is not independent yet; the signature over the canonical payload is. */
/** One synchronous `rustright` call, started the way every gate child starts the client (packages/release/broker-client.mjs): on Windows the
 * `.cmd` shim's target .exe, no shell. A refused shim returns a spawn-shaped `{ error }`. */
function spawnClient(spawnFn, args, options) {
  let launch;
  try { launch = brokerClientInvocation("rustright", args, options, { probe: spawnFn }); } catch (error) { return { error, status: null, signal: null, stdout: "", stderr: "" }; }
  return spawnFn(launch.command, launch.args, launch.options);
}

export function verifyAttestation(requestId, { nonce, argv, pins, expect, workspace, spawnFn = spawnSync, tmpDir = os.tmpdir() }) {
  if (!requestId || !nonce) return { verified: false, requestId: requestId ?? null, nonce: nonce ?? null, reason: "no broker request or nonce" };
  const adapter = expect?.kind === "adapter";
  const executableDigest = adapter ? pins?.nodeSha256 : pins?.cargoSha256;
  if (!pins?.publicKey || !pins.keyId || !pins.host || !pins.brokerDigest || !executableDigest) return { verified: false, requestId, nonce, reason: "no enrollment pins for this host" };
  if (expect && (!adapter || expect.adapter !== "rightkit-pnpm" || !path.isAbsolute(expect.script?.path ?? "") || !/^[0-9a-f]{64}$/.test(expect.script?.sha256 ?? "") || expect.executable?.sha256 !== executableDigest)) return { verified: false, requestId, nonce, reason: "invalid expected adapter enrollment" };
  const r = spawnClient(spawnFn, ["status", requestId], { encoding: "utf8", timeout: 60_000 });
  if (r.error || r.status !== 0) return { verified: false, requestId, nonce, reason: "broker status unavailable" };
  let doc;
  try {
    doc = JSON.parse(r.stdout);
  } catch {
    return { verified: false, requestId, nonce, reason: "unreadable broker status" };
  }
  const receipt = doc.receipt ?? doc;
  const p = receipt.provenance;
  const payload = p?.payload ?? {};
  const request = payload.request ?? {};
  const normalizedWorkspace = (value) => {
    if (typeof value !== "string" || !path.isAbsolute(value)) return null;
    const resolved = path.resolve(value);
    return process.platform === "win32" ? resolved.toLowerCase() : resolved;
  };
  const checks = [
    [p?.schema === "rustright.provenance.v1", "no signed provenance envelope"],
    [p?.signature?.alg === "ES256" && p?.signature?.keyId === pins.keyId, "signature key is not the enrolled key"],
    [payload.host === pins.host && payload.broker?.brokerDigest === pins.brokerDigest, "host or broker digest is not the enrolled one"],
    [payload.nonce === nonce, "nonce mismatch"],
    [payload.jobId === requestId, "job id mismatch"],
    [request.kind === (adapter ? "adapter" : "cargo") && (!adapter || request.adapter === expect.adapter), "signed request kind or adapter mismatch"],
    [JSON.stringify(request.requestedArgv ?? []) === JSON.stringify(argv), "argv mismatch"],
    [workspace === undefined || (normalizedWorkspace(workspace) !== null && normalizedWorkspace(request.workspace) === normalizedWorkspace(workspace)), "signed working directory mismatch"],
    [payload.result?.status === "SUCCEEDED" && payload.result.exitCode === 0, "signed job result did not succeed"],
    [payload.executable?.sha256 === executableDigest, "executable is not this host's enrolled runtime"],
    [!adapter || (payload.executable?.kind === "interpreter" && payload.executable?.script?.path === expect.script.path && payload.executable?.script?.sha256 === expect.script.sha256), "signed adapter script differs from expected policy"],
    [receipt.route !== "ci", "the broker routed this row to hosted CI; it did not run locally"],
    [/^[0-9a-f]{64}$/.test(payload.requestDigest ?? ""), "no request digest"],
  ];
  const failed = checks.find(([ok]) => !ok);
  if (failed) return { verified: false, requestId, nonce, reason: failed[1] };
  const file = path.join(tmpDir, `rk-receipt-${requestId}.json`);
  fs.writeFileSync(file, JSON.stringify(receipt));
  const v = spawnClient(spawnFn, ["verify-receipt", "--receipt", file, "--public-key", pins.publicKey, "--key-id", pins.keyId, "--host", pins.host, "--broker-digest", pins.brokerDigest, "--nonce", nonce, "--request-digest", payload.requestDigest, "--job-id", requestId], { encoding: "utf8", timeout: 60_000 });
  fs.rmSync(file, { force: true });
  if (v.error || v.status !== 0) return { verified: false, requestId, nonce, reason: "signature verification failed" };
  return { verified: true, signatureVerified: true, requestId, nonce, keyId: pins.keyId, brokerDigest: pins.brokerDigest, requestDigest: payload.requestDigest, manifestDigest: request.manifestDigest ?? null, inputBoundary: receipt.inputBoundary?.status ?? "unrequested" };
}

/** `#[ignore]` attributes under a crate's src, tests, benches and examples, by static scan (no compile, no execution). */
export function countIgnoredTests(crateDir) {
  let count = 0;
  const walk = (dir) => {
    if (!fs.existsSync(dir)) return;
    for (const ent of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, ent.name);
      if (ent.isDirectory()) walk(full);
      else if (ent.isFile() && ent.name.endsWith(".rs")) count += (fs.readFileSync(full, "utf8").match(/^\s*#\[ignore\b/gm) ?? []).length;
    }
  };
  for (const sub of ["src", "tests", "benches", "examples"]) walk(path.join(crateDir, sub));
  return count;
}

/** The hosted plan job's contract commands, read from publish.yml itself (job `plan`, the step named below), so the
 * local stage can never drift from a copied list. Every line must be a `node ...` command; anything else refuses. */
export const PLAN_CONTRACT_STEP = "Check the publish route's own contract";
export function planCommandsFromWorkflow(root, { readFile = (f) => fs.readFileSync(f, "utf8"), execPath = process.execPath } = {}) {
  const doc = parseYaml(readFile(path.join(root, ".github", "workflows", "publish.yml")));
  const step = (doc?.jobs?.plan?.steps ?? []).find((s) => s?.name === PLAN_CONTRACT_STEP);
  if (!step || typeof step.run !== "string") throw new Error(`publish.yml has no plan step named "${PLAN_CONTRACT_STEP}" with a run block`);
  const lines = step.run.split("\n").map((l) => l.trim()).filter(Boolean);
  if (!lines.length) throw new Error(`"${PLAN_CONTRACT_STEP}" runs nothing`);
  return lines.map((line) => {
    const words = line.split(/\s+/);
    if (words[0] !== "node" || words.some((w) => /[;&|<>$`'"\\]/.test(w))) throw new Error(`"${PLAN_CONTRACT_STEP}" line is not a plain node command: ${line}`);
    return [execPath, words.slice(1)];
  });
}

function run(command, args, { cwd, env, timeoutMinutes, logFile }) {
  return new Promise((resolve) => {
    const started = Date.now();
    let out = "";
    let launch;
    try { launch = brokerClientInvocation(command, args, { cwd, env, stdio: ["ignore", "pipe", "pipe"] }, { probe: spawnSync }); }
    catch (error) { resolve({ code: 1, timedOut: false, seconds: 0, out: String(error) }); return; }
    const child = spawn(launch.command, launch.args, launch.options);
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
    if (deps.writeReceipt === false) return;
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

  // The hosted plan job's own contract checks, exactly as publish.yml runs them (job `plan`, step "Check the publish
  // route's own contract"): a dispatch that would die there is refused here first. Deterministic, no compile.
  if (!opts.rowsOnly) {
    const planCommands = planCommandsFromWorkflow(root);
    const started = Date.now();
    let failed = null;
    for (const [i, [command, args]] of planCommands.entries()) {
      const r = await run(command, args, { cwd: root, env, timeoutMinutes: 20, logFile: path.join(logDir, `plan-check-${i + 1}.log`) });
      if (r.code !== 0 || r.timedOut) {
        failed = { index: i + 1, command: `node ${args.join(" ")}`, exit: r.code, timedOut: r.timedOut, lines: r.out.split("\n").filter((l) => /^not ok|error:|refused/.test(l.trim())).slice(0, 8) };
        break;
      }
    }
    // Ignored tests are not passes on the hosted verify hosts: a unit that carries any #[ignore] needs the reviewed
    // allowIgnored policy entry in ci/gate.config.json (the dispatcher then names it in allow_ignored). Static, no compile.
    if (!failed && opts.registry === "crates") {
      const recipe = consumerRecipePolicy(root, opts.unit);
      receipt.consumerRecipe = recipe;
      if (!recipe.ok) failed = { index: planCommands.length + 1, command: "consumer-recipe policy", exit: 1, timedOut: false, lines: [recipe.reason] };
    }
    if (!failed && opts.registry === "crates") {
      const ignored = countIgnoredTests(path.join(root, "crates", opts.unit));
      const allowed = Boolean(config.rust?.[opts.unit]?.allowIgnored);
      receipt.ignoredTests = { count: ignored, allowed };
      if (ignored > 0 && !allowed) failed = { index: planCommands.length + 1, command: "ignored-tests policy", exit: 1, timedOut: false, lines: [`${ignored} #[ignore] attribute(s) under crates/${opts.unit} and no reviewed allowIgnored entry in ci/gate.config.json; the hosted verify refuses ignored tests without one`] };
    }
    receipt.planChecks = { ok: !failed && unchanged(), exit: failed ? failed.exit : 0, timedOut: Boolean(failed?.timedOut), seconds: Math.round((Date.now() - started) / 1000), commands: planCommands.length, failed };
    out(`${receipt.planChecks.ok ? "PASS" : "FAIL"} plan-checks (${receipt.planChecks.seconds}s)`);
    if (!receipt.planChecks.ok) {
      receipt.failedAt = "plan-checks";
      receipt.finishedAt = new Date().toISOString();
      save();
      if (failed) { err(`  ${failed.command}`); for (const line of failed.lines) err(`  ${line}`); }
      return 1;
    }
  }

  if (!unchanged()) { receipt.failedAt = "source-changed"; save(); return 2; }
  await deps.onChecksPassed?.({ sha, unit: opts.unit });
  // Internal dispatcher recheck cannot be requested by CLI and never executes a compile row.
  if (deps.checksOnly) return 0;
  let decision;
  try { decision = await (deps.observeRoute ?? observeCrateRoute)({ root, sha, unit: opts.unit, rows }); }
  catch (error) { decision = { route: "local", reason: `routing-unavailable: ${error.message}` }; }
  receipt.routing = decision;
  if (decision.route === "ci") {
    receipt.route = "ci";
    receipt.mode = "hosted-compile";
    receipt.ok = unchanged();
    receipt.finishedAt = new Date().toISOString();
    save();
    out(`${receipt.ok ? "GREEN" : "RED"} ${opts.unit}: hosted-compile selected; pinned dispatcher rechecks admission before dispatch`);
    return receipt.ok ? 0 : 2;
  }
  // Compile only. Runtime tests & OS qualification belong on hosted runners.
  const rowEnv = { ...env, RIGHTKIT_REQUIRE_ASSETS: "1" };
  for (const row of rows) {
    if (!unchanged()) {
      receipt.failedAt = "source-changed"; save(); return 2;
    }
    const command = row.command ?? "rustright";
    const args = row.command ? row.args : ["cargo", ...row.args];
    let rowCwd;
    try {
      rowCwd = resolveRowWorkspace({ root, row, sha });
    } catch (error) {
      receipt.failedAt = row.id;
      receipt.problems = [error.message];
      save();
      err(`rk-preflight: ${error.message}`);
      return 2;
    }
    const logFile = path.join(logDir, `${row.id.replace(/[^a-z0-9.-]+/gi, "_")}.log`);
    const nonce = crypto.randomBytes(32).toString("hex");
    const r = await run(command, args, { cwd: rowCwd, env: { ...rowEnv, RUSTRIGHT_ATTEST_NONCE: nonce }, timeoutMinutes: row.timeoutMinutes, logFile });
    const warnings = row.denyWarnings ? countDocWarnings(r.out, opts.unit) : 0;
    const ids = brokerRequestIds(r.out);
    const attestation = ids.length === 1 ? (deps.verifyAttestation ?? verifyAttestation)(ids[0], { nonce, argv: row.args, pins: pipeline.attestation?.[process.platform], workspace: rowCwd }) : { verified: false, requestId: null, nonce, reason: ids.length ? "one row produced several broker requests" : "no broker request" };
    const ok = r.code === 0 && !r.timedOut && warnings === 0 && attestation.verified === true && unchanged();
    const entry = { id: row.id, kind: row.kind, cwd: row.cwd ?? ".", command: `${command} ${args.join(" ")}`, exit: r.code, timedOut: r.timedOut, seconds: r.seconds, warnings, brokerRequests: ids, attestation, log: logFile, ok };
    receipt.rows.push(entry);
    out(`${ok ? "PASS" : "FAIL"} ${row.id} (${r.seconds}s${warnings ? `, ${warnings} doc warning(s)` : ""}${r.timedOut ? ", timed out" : ""})`);
    save();
    if (!ok) {
      receipt.failedAt = row.id;
      receipt.finishedAt = new Date().toISOString();
      save();
      if (!attestation.verified) err(`  attestation: ${attestation.reason}`);
      for (const line of r.out.split("\n").filter((l) => /^(error|warning)(\[|:)/.test(l)).slice(0, 12)) err(`  ${line}`);
      return 1;
    }
  }
  // The unit's committed registry consumer, built against this checkout through the broker and run as the hosted verify
  // runs it (rk-consumer-stage.mjs). A fixture defect fails here, before any dispatch, instead of on the hosted runner.
  if (opts.registry === "crates") {
    if (!unchanged()) { receipt.failedAt = "source-changed"; save(); return 2; }
    const pins = pipeline.attestation?.[process.platform];
    const attest = (output, { nonce, argv, workspace }) => {
      const ids = brokerRequestIds(output);
      return ids.length === 1 ? (deps.verifyAttestation ?? verifyAttestation)(ids[0], { nonce, argv, pins, workspace }) : { verified: false, requestId: null, nonce, reason: ids.length ? "more than one broker request" : "no broker request" };
    };
    const provisionChrome = deps.provisionChrome ?? (async ({ runnerTemp }) => {
      const { provision } = await import("./provision-chrome.mjs");
      const { resolveR2CftEnv } = await import("./provision-r2-cft-env.mjs");
      const pinsFile = JSON.parse(fs.readFileSync(path.join(root, "ci", "chrome-for-testing.json"), "utf8"));
      const managedRoot = path.join(runnerTemp, "chrome-for-testing");
      await provision({ root: managedRoot, pins: pinsFile, log: () => {} });
      return resolveR2CftEnv({ managedRoot, runnerTemp, pins: pinsFile });
    });
    const stage = await (deps.consumerStage ?? consumerStage)({ root, unit: opts.unit, version: receipt.version, sha, logDir, env: rowEnv, runCommand: run, attest, provisionChrome });
    receipt.registryConsumer = stage;
    out(`${stage.ok ? "PASS" : "FAIL"} registry-consumer (${stage.pkId ?? "no recipe"}; ${stage.runs.map((x) => `${x.id}:${x.status}`).join(", ") || "no runs"})`);
    save();
    if (!stage.ok || !unchanged()) {
      receipt.failedAt = stage.ok ? "source-changed" : "registry-consumer";
      receipt.finishedAt = new Date().toISOString();
      save();
      err(`  registry consumer ${stage.failedAt}: ${stage.reason}`);
      for (const line of stage.lines ?? []) err(`  ${line}`);
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
export function receiptRefusal(receipt, { sha, registry, unit, config, version, expectedRows, packageDir, allowHostedCompile = false }) {
  const npm = registry === "npm";
  if (registry !== "crates" && !npm) return "unsupported pre-flight registry";
  if (!receipt) return "no pre-flight receipt for this commit";
  if (receipt.sha !== sha) return "receipt commit differs from requested commit";
  const hosted = !npm && allowHostedCompile && receipt.mode === "hosted-compile" && receipt.route === "ci";
  if (receipt.schema !== SCHEMA || (!hosted && receipt.mode !== (npm ? "local-test" : "local-compile"))) return "obsolete or diagnostic receipt";
  if (receipt.registry !== registry || receipt.unit !== unit || receipt.version !== version) return "receipt unit/registry/version mismatch";
  if (npm && (receipt.adapter !== "rightkit-pnpm" || receipt.dir !== packageDir)) return "npm adapter or package directory mismatch";
  if ((!hosted && receipt.route !== "local") || receipt.ok !== true || !receipt.finishedAt || receipt.failedAt) return "local compilation is incomplete or red";
  if (receipt.scans?.ok !== true || receipt.scans.exit !== 0 || receipt.scans.timedOut !== false) return "deterministic scans did not pass";
  if (receipt.planChecks?.ok !== true || receipt.planChecks.exit !== 0 || receipt.planChecks.timedOut !== false) return "the hosted plan job's contract checks did not pass locally";
  const expected = npm ? expectedRows : crateRows(unit, config);
  if (!Array.isArray(expected) || !expected.length) return "missing committed row plan";
  if (hosted) {
    if (JSON.stringify(receipt.requiredRows) !== JSON.stringify(expected.map(row => row.id)) || !Array.isArray(receipt.rows) || receipt.rows.length !== 0) return "hosted compile row plan differs";
    return null; // Observation only: pinned dispatcher must re-execute checks and query route independently.
  }
  if (npm && JSON.stringify(receipt.requiredRows) !== JSON.stringify(expected.map((row) => row.id))) return "npm required row order differs";
  if (!Array.isArray(receipt.rows) || receipt.rows.length !== expected.length || !expected.length) return "missing or extra compile rows";
  for (const [i, row] of expected.entries()) {
    const got = receipt.rows[i];
    if (!got || got.cwd !== (row.cwd ?? ".")) return `working directory differs for compile row ${row.id}`;
    const command = npm ? `rustright run rightkit-pnpm -- ${row.args.join(" ")}` : `rustright cargo ${row.args.join(" ")}`;
    if (got.id !== row.id || got.command !== command || got.ok !== true || got.exit !== 0 || got.timedOut !== false || (!npm && got.warnings !== 0) || !got.brokerRequests?.length || !got.brokerRequests.every((id) => /^[0-9a-f-]{36}$/.test(id)) || got.attestation?.verified !== true || got.attestation.signatureVerified !== true || typeof got.attestation.nonce !== "string" || got.attestation.nonce.length !== 64 || got.attestation.requestId !== got.brokerRequests[0]) return `unproven compile row ${row.id}`;
    if (npm && (got.kind !== row.kind || got.brokerRequests.length !== 1 || !/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(got.brokerRequests[0]) || !/^[0-9a-f]{64}$/.test(got.attestation.nonce))) return `unproven npm row ${row.id}`;
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
