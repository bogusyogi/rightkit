#!/usr/bin/env node
// Generates cases/<name>/{input.json,expected.json} by running the CURRENT Node ownership
// implementation. `--check` recomputes everything and fails (exit 1) if any file drifted.
//   node fixtures/ownership/generate.mjs           write fixtures
//   node fixtures/ownership/generate.mjs --check   fail on drift, write nothing
// Volatile values are pinned: the clock is faked, `host` is replaced, temp paths become <ROOT>,
// the scanner policy digest is a constant, git commits use a fixed identity and dates.
import { execFileSync } from "../../background-process.mjs";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { canonicalRepositoryIdentity } from "../../ownership-approval.mjs";
import { contentInventory } from "../../ownership.mjs";
import { ownershipScanInputsForTest, scanOwnershipForTest } from "../../ownership-scan-worker.mjs";
import { ownershipCacheKey, ownershipVerdict } from "../../ownership-verdict.mjs";
import { OwnershipAdmission, runScanWorker } from "../../ownership-admission.mjs";
import { admissionCases, cacheKeyCases, identityCases, SCANNER_POLICY_DIGEST, scanCases, transportCases, verdictCases } from "./cases.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const casesDir = path.join(here, "cases");
const json = (value) => JSON.stringify(value, null, 2) + "\n";
const plain = (value) => value === undefined ? undefined : JSON.parse(JSON.stringify(value));
const FIXED_HOST = "fixture-host/fixture-os";

// ------------------------------------------------------------------ fake clock
function withClock(iso, fn) {
  const RealDate = Date, fixed = RealDate.parse(iso);
  globalThis.Date = class extends RealDate {
    constructor(...args) { if (args.length === 0) super(fixed); else super(...args); }
    static now() { return fixed; }
  };
  try { return fn(); } finally { globalThis.Date = RealDate; }
}

// ------------------------------------------------------------------ scan harness
const gitEnv = { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: process.platform === "win32" ? "NUL" : os.devNull, GIT_AUTHOR_NAME: "Fixture", GIT_AUTHOR_EMAIL: "fixture@example.invalid", GIT_COMMITTER_NAME: "Fixture", GIT_COMMITTER_EMAIL: "fixture@example.invalid", GIT_AUTHOR_DATE: "2026-01-01T00:00:00Z", GIT_COMMITTER_DATE: "2026-01-01T00:00:00Z" };
const git = (root, ...args) => execFileSync("git", ["-C", root, "-c", "core.autocrlf=false", "-c", "commit.gpgsign=false", ...args], { env: gitEnv, stdio: ["ignore", "pipe", "pipe"], windowsHide: true }).toString();
function put(root, rel, text) { const file = path.join(root, ...rel.split("/")); mkdirSync(path.dirname(file), { recursive: true }); writeFileSync(file, text); }

function materialize(def, base) {
  const root = path.join(base, "repo"); mkdirSync(root, { recursive: true });
  git(root, "init", "-q", "-b", "main");
  if (def.harness.origin) git(root, "remote", "add", "origin", def.harness.origin);
  for (const [rel, text] of Object.entries(def.repo.files)) put(root, rel, text);
  for (const [rel, target] of Object.entries(def.repo.symlinks)) { mkdirSync(path.dirname(path.join(root, ...rel.split("/"))), { recursive: true }); symlinkSync(target, path.join(root, ...rel.split("/"))); }
  if (def.harness.commit !== false) { git(root, "add", "-A"); git(root, "commit", "-q", "-m", "fixture"); }
  for (const [rel, text] of Object.entries(def.repo.untracked)) put(root, rel, text);
  return root;
}

// Windows resolves the virtual /fixture roots onto a drive and joins with backslashes; expected files stay POSIX.
function posixFixturePaths(s) {
  return s.replace(/(?:[A-Za-z]:)?\\fixture((?:\\[^\\"\s]+)*)/g, (_, rest) => `/fixture${rest.replaceAll("\\", "/")}`)
    .replace(/<SCANNER>((?:\\[^\\"\s]+)+)/g, (_, rest) => `<SCANNER>${rest.replaceAll("\\", "/")}`);
}

const posixPaths = (value) => typeof value === "string" ? posixFixturePaths(value) : Array.isArray(value) ? value.map(posixPaths)
  : value && typeof value === "object" ? Object.fromEntries(Object.entries(value).map(([k, v]) => [k, posixPaths(v)])) : value;

function scrub(value, replacements) {
  if (typeof value === "string") { let s = value; for (const [from, to] of replacements) s = s.split(from).join(to); return s; }
  if (Array.isArray(value)) return value.map((item) => scrub(item, replacements));
  if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, k === "host" && typeof v === "string" ? FIXED_HOST : scrub(v, replacements)]));
  return value;
}

// Mirrors the CLI wrapper in ownership-scan-worker.mjs:97-99 (the CLI cannot run here: its trust file path is fixed).
const cliShape = (error) => ({ error: error.message, code: error.code?.startsWith("RK_OWNERSHIP_") ? error.code : "RK_OWNERSHIP_UNVERIFIED" });

function runScan(def) {
  const base = realpathSync(mkdtempSync(path.join(os.tmpdir(), "rk-own-fixture-")));
  try {
    const manifestPath = path.join(base, "catalog.json");
    writeFileSync(manifestPath, JSON.stringify(def.catalog));
    const common = { manifestPath, trustLoader: () => structuredClone(def.trustState), policyDigest: () => SCANNER_POLICY_DIGEST };
    const replacements = [];
    const run = (mode) => {
      const root = materialize(def, path.join(base, mode)); replacements.push([root, "<ROOT>"]);
      const options = { ...common };
      if (mode === "scan") Object.assign(options, { committed: true, requireCleanCheckout: true, allowPendingAdvisory: true });
      if (mode === "scan" && def.harness.mutateOnSecondInventory) { let calls = 0; options.inventory = (r) => { if (++calls === 2) writeFileSync(path.join(r, "fixture-mutation.txt"), "changed\n"); return contentInventory(r); }; }
      try { return withClock(def.harness.now, () => mode === "scan" ? scanOwnershipForTest(root, options) : ownershipScanInputsForTest(root, options)); }
      catch (error) { return cliShape(error); }
    };
    const scan = run("scan"), inputsMode = run("inputs");
    replacements.push([base, "<TMP>"]);
    const cacheKey = scan.inputs ? ownershipCacheKey(scan.inputs) : null;
    return { scan: scrub(plain(scan), replacements), inputsMode: scrub(plain(inputsMode), replacements), cacheKey };
  } finally { rmSync(base, { recursive: true, force: true }); }
}

// ------------------------------------------------------------------ other kinds
function runVerdict(def) { return { verdict: ownershipVerdict(structuredClone(def.report), structuredClone(def.trust)) }; }
function runCacheKey(def) {
  try { return { key: ownershipCacheKey(structuredClone(def.inputs)) }; }
  catch (error) { return { error: { code: error.code ?? null, message: error.message } }; }
}
function runIdentity(def) {
  try { return { canonical: canonicalRepositoryIdentity(def.value) }; }
  catch (error) { return { error: error.message }; }
}

async function runAdmission(def) {
  const s = def.scenario, clock = { t: 0 }, calls = { inputs: 0, scan: 0 };
  const scans = s.steps.filter((step) => step.scanned !== undefined).map((step) => step.scanned);
  let cursor, lastObserved;
  const gate = new OwnershipAdmission({
    mode: s.mode, scannerRoot: s.scannerRoot, now: () => clock.t, timeoutMs: 30000,
    runInputs: async () => { calls.inputs++; if (cursor.observedThrows) throw new Error(cursor.observedThrows); if (cursor.observed !== undefined) lastObserved = cursor.observed; return structuredClone(lastObserved ?? { error: "fixture: no scripted inputs response" }); },
    runScan: async () => { calls.scan++; await Promise.resolve(); return structuredClone(scans.length ? scans.shift() : { error: "fixture: no scripted scan response" }); },
  });
  const assertShape = (result) => { try { gate.assertAdmitted(result); return { admitted: true }; } catch (error) { return { admitted: false, code: error.code ?? null, message: error.message }; } };
  const one = (step) => { if (step.advanceClockMs) clock.t += step.advanceClockMs; cursor = step; return gate.evaluate({ repositoryRoot: step.repositoryRoot, workspaceRoot: step.workspaceRoot }); };
  const results = [];
  if (s.concurrent) {
    const pending = s.steps.map(one);
    for (const result of await Promise.all(pending)) results.push(result);
  } else for (const step of s.steps) results.push(await one(step));
  return {
    steps: results.map((result) => ({ result: plain(result), assertAdmitted: assertShape(result) })),
    ...(s.assertOnly ? { assertOnly: s.assertOnly.map((item) => ({ input: item, assertAdmitted: assertShape(plain(item)) })) } : {}),
    calls,
  };
}

async function runTransport(def) {
  const base = realpathSync(mkdtempSync(path.join(os.tmpdir(), "rk-own-transport-")));
  try {
    writeFileSync(path.join(base, "ownership-scan-worker.mjs"), def.workerSource);
    const result = await runScanWorker({ scannerRoot: base, scanRoot: "/fixture/repo", timeoutMs: def.timeoutMs, inputsOnly: def.inputsOnly, execPath: def.execPath === "node" ? process.execPath : def.execPath });
    const out = scrub(plain(result), [[base, "<SCANNER>"]]);
    // The JSON parser message is engine specific (not contractual): keep only the stable prefix.
    if (typeof out.error === "string" && out.error.startsWith("scanner returned invalid JSON: ")) out.error = "scanner returned invalid JSON: <parser message>";
    return { result: out };
  } finally { rmSync(base, { recursive: true, force: true }); }
}

function actualCode(def, out) {
  switch (def.kind) {
    case "scan": return out.scan.error ? out.scan.code : out.scan.verdict.code;
    case "verdict": return out.verdict.code;
    case "cache-key": return out.key ? "OK" : out.error.code;
    default: return undefined;
  }
}

// ------------------------------------------------------------------ driver
async function buildAll() {
  const defs = [...verdictCases(), ...cacheKeyCases(), ...identityCases(), ...admissionCases(), ...transportCases(), ...scanCases()];
  const names = new Set(), files = new Map(), problems = [];
  for (const [name, raw] of defs) {
    if (names.has(name)) throw new Error(`duplicate fixture case name: ${name}`);
    names.add(name);
    const def = plain(raw);
    const out = def.kind === "scan" ? runScan(def) : def.kind === "verdict" ? runVerdict(def) : def.kind === "cache-key" ? runCacheKey(def)
      : def.kind === "identity" ? runIdentity(def) : def.kind === "transport" ? await runTransport(def) : await runAdmission(def);
    if (process.env.FIXTURE_DEBUG === name) console.error(JSON.stringify(out, null, 1));
    if (def.expectCode !== undefined && actualCode(def, out) !== def.expectCode) problems.push(`${name}: expected ${def.expectCode}, implementation returned ${actualCode(def, out)}`);
    files.set(`${name}/input.json`, json(def));
    files.set(`${name}/expected.json`, json(process.platform === "win32" ? posixPaths(out) : out));
  }
  return { files, problems };
}

function listDisk(dir, prefix = "") {
  if (!existsSync(dir)) return [];
  return readdirSync(dir).flatMap((entry) => { const rel = prefix ? `${prefix}/${entry}` : entry; return statSync(path.join(dir, entry)).isDirectory() ? listDisk(path.join(dir, entry), rel) : [rel]; });
}

export async function main(argv) {
  const check = argv.includes("--check");
  const { files, problems } = await buildAll();
  if (problems.length) { console.error("fixture intent mismatches (author expectation vs current implementation):\n" + problems.join("\n")); return 2; }
  if (check) {
    const drift = [];
    for (const [rel, content] of files) {
      const file = path.join(casesDir, rel);
      if (!existsSync(file)) drift.push(`missing ${rel}`);
      else if (readFileSync(file, "utf8") !== content) drift.push(`changed ${rel}`);
    }
    for (const rel of listDisk(casesDir)) if (!files.has(rel)) drift.push(`stale ${rel}`);
    if (drift.length) { console.error(`ownership fixtures drifted (${drift.length}); run \`node fixtures/ownership/generate.mjs\` after confirming the behaviour change is intended:\n` + drift.slice(0, 40).join("\n")); return 1; }
    console.log(`ownership fixtures up to date (${files.size / 2} cases)`);
    return 0;
  }
  for (const rel of listDisk(casesDir)) if (!files.has(rel)) rmSync(path.join(casesDir, rel));
  for (const [rel, content] of files) { const file = path.join(casesDir, rel); mkdirSync(path.dirname(file), { recursive: true }); writeFileSync(file, content); }
  console.log(`wrote ${files.size / 2} ownership fixture cases`);
  return 0;
}

if (process.argv[1] && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url))) process.exitCode = await main(process.argv.slice(2));
