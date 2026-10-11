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
// Publish verification (.github/workflows/publish.yml) names its units explicitly:
//   gate.mjs --registry npm|crates --units "<names>" [--allow-host-exclusion "<names>"] [--allow-no-test "<names>"]
// That mode selects exactly those units (no diff, no dependents, no root or Swift suites) and
// treats every red unit as a failure: known-red is not a waiver for publishing. Nothing is skipped
// silently: a unit this host would not run, or an npm package with no test script, fails the run
// unless the request names it; a unit whose tests reported a skip fails; a unit with no executed
// test step fails. It writes unit-evidence.json, one explicit status per requested unit, which the
// publish verify step turns into this host's receipt.
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { changedConfigCrates, changedLockNames, parseLock, selectByPaths, selectNpm, selectRust, selectUnits } from "./select.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const WIN = process.platform === "win32";
const HOST = WIN ? "windows" : process.platform === "darwin" ? "macos" : "linux";
const PLAN = process.argv.includes("--plan");
const argOf = (flag) => { const i = process.argv.indexOf(flag); return i > 0 ? process.argv[i + 1] : undefined; };
const MINUTE = 60_000;
const UNIT_MODE = process.argv.includes("--units") || process.argv.includes("--registry");

const ROOT_SUITES = [
  { id: "test:ownership-approval", paths: ["ownership-approval"] },
  { id: "test:characterization", paths: ["characterization"] },
  { id: "test:infrastructure", paths: ["infrastructure"] },
  { id: "test:swift-contracts", paths: ["swift/contracts"] },
];
const SCRIPT_TESTS = { id: "scripts", paths: ["scripts"] };
const SWIFT_PACKAGES = [
  { id: "swift/RightKitSwift", paths: ["swift/RightKitSwift"] },
  { id: "swift/RightKitCloudKitHelper", paths: ["swift/RightKitCloudKitHelper"] },
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

function npmPackages() {
  const packages = [];
  for (const entry of fs.readdirSync(path.join(ROOT, "packages"), { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const manifest = readJson(`packages/${entry.name}/package.json`, null);
    if (manifest?.name) packages.push({ name: manifest.name, dir: `packages/${entry.name}`, scripts: manifest.scripts ?? {}, all: { ...manifest.dependencies, ...manifest.devDependencies, ...manifest.peerDependencies } });
  }
  const names = new Set(packages.map((p) => p.name));
  return packages.map((p) => ({ name: p.name, dir: p.dir, scripts: p.scripts, deps: Object.keys(p.all).filter((d) => names.has(d)) }));
}

function unitPlan() {
  const config = readJson("ci/gate.config.json", {});
  const units = (argOf("--units") ?? "").split(/\s+/).filter(Boolean);
  const packages = npmPackages();
  const names = (flag) => (argOf(flag) ?? "").split(/\s+/).filter(Boolean);
  let picked;
  try { picked = selectUnits({ registry: argOf("--registry"), units, members: rustMembers(config), packages, host: HOST, config, allowHostExclusion: names("--allow-host-exclusion"), allowNoTest: names("--allow-no-test") }); } catch (error) {
    console.error(`gate: ${error.message}. Usage: --registry npm|crates --units "<names>"`);
    process.exit(2);
  }
  return { host: HOST, base: null, all: false, changedPaths: 0, config, packages, rust: picked.rust, npm: picked.npm, root: [], swift: [], unitRequest: { registry: argOf("--registry"), units, excluded: picked.excluded, noTest: picked.noTest } };
}

function plan() {
  if (UNIT_MODE) return unitPlan();
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
    let timedOut = false; let settled = false; let skipped = 0;
    const countSkips = (chunk) => { skipped += (String(chunk).match(/^\s*(SKIP |# skipped [1-9]|ℹ skipped [1-9]|Tests\s.*\b[1-9]\d* skipped|.*\u2193 .*\[skipped\])/gm) ?? []).length; };
    const finish = (code, note) => {
      if (settled) return; settled = true;
      clearTimeout(timer);
      const seconds = Math.round((Date.now() - started) / 1000);
      const ok = code === 0 && !timedOut;
      if (note) { console.log(note); log.write(`${note}\n`); }
      log.end();
      console.log("::endgroup::");
      console.log(`${ok ? "PASS" : "FAIL"} ${label} (${seconds}s${timedOut ? ", timed out" : ""})`);
      results.push({ unit, command: `${command} ${args.join(" ")}`, ok, code, timedOut, seconds, skipped });
      // Written after every step: a job that is cancelled at its time limit still uploads what ran.
      fs.writeFileSync(path.join(EVIDENCE, "progress.json"), JSON.stringify(results, null, 1));
      resolve(ok);
    };
    // pnpm is a .cmd shim on Windows and needs a shell there; the arguments here never contain spaces.
    const child = spawn(command, args, { cwd, env: { ...process.env, CARGO_TERM_COLOR: "always", ...(UNIT_MODE ? { RIGHTKIT_REQUIRE_ASSETS: "1" } : {}), ...env }, stdio: ["ignore", "pipe", "pipe"], shell: WIN && command === "pnpm", detached: !WIN });
    const timer = setTimeout(() => { timedOut = true; killTree(child); }, timeoutMinutes * MINUTE);
    child.stdout.on("data", (chunk) => { if (!discardStdout) { countSkips(chunk); process.stdout.write(chunk); log.write(chunk); } });
    child.stderr.on("data", (chunk) => { countSkips(chunk); process.stdout.write(chunk); log.write(chunk); });
    child.on("error", (error) => finish(1, `could not start: ${error.message}`));
    child.on("close", (code) => finish(code ?? 1, timedOut ? `timed out after ${timeoutMinutes} minutes` : ""));
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

async function rustGate(selection, config) {
  const { rust } = selection;
  if (!rust.test.length && !rust.check.length) return;
  const applies = (name) => !(config.rust?.[name]?.os) || config.rust[name].os.includes(HOST);
  // Check the lock first: a manifest edited without a build leaves it stale. The repair is
  // `cargo update --workspace`, and the patch it produces is evidence, not a reason to skip the
  // other units.
  const locked = await run("lock", "cargo", ["metadata", "--locked", "--format-version", "1"], { timeoutMinutes: 15, discardStdout: true });
  if (!locked) {
    await run("lock", "cargo", ["update", "--workspace"], { timeoutMinutes: 15 });
    savePatch("Cargo.lock.patch", ["Cargo.lock"]);
  }

  const formatted = rust.test.filter(applies);
  if (formatted.length) {
    await run("fmt", "cargo", ["fmt", ...formatted.flatMap((name) => ["-p", name])], { timeoutMinutes: 10 });
    const dirty = git("diff", "--name-only").stdout.split("\n").filter((p) => p.endsWith(".rs"));
    if (dirty.length) {
      savePatch("fmt.patch", dirty);
      results.push({ unit: "fmt", command: `rustfmt leaves ${dirty.length} file(s) unchanged`, ok: false, code: 1, seconds: 0 });
    }
  }

  for (const name of rust.test) {
    if (!applies(name)) continue;
    const entry = config.rust?.[name] ?? {};
    const featureSets = entry.featureSets ?? [[]];
    const timeoutMinutes = entry.timeoutMinutes ?? 25;
    for (const features of featureSets) {
      await run(`rust:${name}`, "cargo", ["test", "-p", name, ...features, "--no-fail-fast", "--", "--show-output"], { timeoutMinutes });
      if (entry.alsoSerial) await run(`rust:${name}`, "cargo", ["test", "-p", name, ...features, "--no-fail-fast", "--", "--test-threads=1", "--show-output"], { timeoutMinutes });
    }
    await run(`rust:${name}`, "cargo", ["clippy", "-p", name, "--no-deps", "--all-targets", ...featureSets[0], "--", "-D", "warnings"], { timeoutMinutes });
  }
  for (const name of rust.check) {
    if (applies(name)) await run(`rust:${name}`, "cargo", ["check", "-p", name, "--all-targets"], { timeoutMinutes: 30 });
  }
}

async function npmGate(selection) {
  const { npm, root, packages } = selection;
  if (npm.test.length) {
    // Workspace packages resolve each other through built output, so build all of them in
    // dependency order before any selected package is tested.
    const built = await run("npm:build", "pnpm", ["-r", "--workspace-concurrency=1", "run", "build"], { timeoutMinutes: 30 });
    for (const dir of npm.test) {
      const pkg = packages.find((p) => p.dir === dir);
      if (!pkg?.scripts.test) {
        if (UNIT_MODE) results.push({ unit: `npm:${dir}`, command: "no test script", ok: false, code: 1, seconds: 0 });
        continue;
      }
      if (!built) { results.push({ unit: `npm:${dir}`, command: "not run: workspace build failed", ok: false, code: 1, seconds: 0 }); continue; }
      await run(`npm:${dir}`, "pnpm", ["--dir", dir, "run", "test"], { timeoutMinutes: 30 });
    }
  }
  for (const id of root) {
    if (id === SCRIPT_TESTS.id) await run("root:scripts", "node", ["--test", "scripts/**/*.test.mjs"], { timeoutMinutes: 10 });
    else await run(`root:${id}`, "pnpm", ["run", id], { timeoutMinutes: 20 });
  }
}

async function swiftGate(selection) {
  for (const dir of selection.swift) {
    const cwd = path.join(ROOT, dir);
    const built = await run(`swift:${dir}`, "swift", ["build"], { cwd, timeoutMinutes: 45 });
    if (built && fs.existsSync(path.join(cwd, "Tests"))) await run(`swift:${dir}`, "swift", ["test"], { cwd, timeoutMinutes: 60 });
  }
}

/** One explicit status per requested unit: pass, allowed-exclusion, allowed-no-test, or fail (never absent). */
function writeUnitEvidence(selection, rows, ok) {
  const { registry, units, excluded, noTest } = selection.unitRequest;
  const byId = new Map(rows.map((r) => [r.id, r]));
  const evidence = units.map((name) => {
    const id = registry === "crates" ? `rust:${name}` : `npm:${selection.packages.find((p) => p.name === name)?.dir}`;
    const row = byId.get(id);
    const steps = results.filter((r) => r.unit === id).map((r) => ({ command: r.command, ok: r.ok, seconds: r.seconds }));
    const ranTests = steps.some((s) => /\b(cargo test|run test)\b/.test(s.command));
    let status = "fail";
    if (excluded.includes(name)) status = "allowed-exclusion";
    else if (noTest.includes(name)) status = "allowed-no-test";
    else if (row?.ok && ranTests) status = "pass";
    return { name, id, status, steps };
  });
  fs.mkdirSync(EVIDENCE, { recursive: true });
  fs.writeFileSync(path.join(EVIDENCE, "unit-evidence.json"), JSON.stringify({ schema: 1, host: HOST, registry, sha: process.env.GITHUB_SHA ?? null, ok: ok && evidence.every((e) => e.status !== "fail"), units: evidence }, null, 2));
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
  if (UNIT_MODE) {
    for (const unit of units.values()) {
      if (unit.skipped) { unit.ok = false; unit.failed.push(`${unit.skipped} test(s) reported as skipped; a skipped test is not a pass`); }
    }
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
    rows.some((r) => r.skipped) ? `Unproven on this runner (tests skipped for a missing asset or precondition; skipped is not passed): ${rows.filter((r) => r.skipped).map((r) => `${r.id} (${r.skipped})`).join(", ")}` : "",
    nowGreen.length ? `Now green, remove from ci/known-red.json (${HOST}): ${nowGreen.map((r) => r.id).join(", ")}` : "",
    "Not covered by this gate: desktop-app-template, swift/ios-app-template, spikes.",
  ].join("\n");
  fs.mkdirSync(EVIDENCE, { recursive: true });
  fs.writeFileSync(path.join(EVIDENCE, "summary.md"), lines);
  fs.writeFileSync(path.join(EVIDENCE, "summary.json"), JSON.stringify({ host: HOST, sha: process.env.GITHUB_SHA ?? null, base: selection.base, rows, results }, null, 2));
  if (UNIT_MODE) writeUnitEvidence(selection, rows, unexpected.length === 0);
  if (process.env.GITHUB_STEP_SUMMARY) fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY, `${lines}\n`);
  console.log(lines);
  if (UNIT_MODE) {
    const missing = selection.unitRequest.units.filter((name) => {
      const id = selection.unitRequest.registry === "crates" ? `rust:${name}` : `npm:${selection.packages.find((p) => p.name === name)?.dir}`;
      return !selection.unitRequest.excluded.includes(name) && !selection.unitRequest.noTest.includes(name) && !(units.get(id)?.ok);
    });
    if (missing.length) { console.log(`Requested units with no passing evidence: ${missing.join(", ")}`); return false; }
  }
  return unexpected.length === 0;
}

const selection = plan();
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
await rustGate(selection, selection.config);
await npmGate(selection);
await swiftGate(selection);
process.exit(report(selection) ? 0 : 1);
