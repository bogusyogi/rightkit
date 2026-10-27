#!/usr/bin/env node
// rk-release-canary: the pipeline canary for @rightkit/release. It runs BEFORE the unit's pre-flight and answers one
// question the unit's own tests cannot: does the candidate's `right-release doctor` still behave, on every app and both
// hosts, the way the version already on the registry does?
//
// Each cell runs the doctor twice against the same pinned app commit: baseline (registry latest) and candidate (the
// tarball packed at the release SHA). Only DIFFERENCES block: a new [FAIL] row, pass -> not-pass, a changed fatal,
// a write into the app tree, a candidate crash. Failures the baseline already has are reported, never blocking.
// Incomplete is not green: an outage, a timeout or a host that is not representative leaves the receipt un-green.
//
//   rk-release-canary.mjs [--unit @rightkit/release] [--out <receipts dir>]
//   exit 0 green | 10 regression (not retried at the same SHA) | 11 incomplete (retry) | 2 usage/internal
//
// Stages: 0 pins (release-version-pins.mjs) -> matrix/expected validation -> app commits (ls-remote, once) -> candidate
// pack, baseline + smol-toml fetch -> bundle -> Mac leg and Dell leg in parallel -> compareCells -> receipt.
import { spawn } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { DEFAULT_OUT } from "./rk-preflight.mjs";
import { versionPinProblems } from "./release-version-pins.mjs";
import { HOST_FOR_PLATFORM, INFRA_STATUSES, readTarball } from "./rk-release-canary-leg.mjs";

export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
export const UNIT = "@rightkit/release";
export const RECEIPT_SCHEMA = "rk-release-canary.v1";
export const MANIFEST_SCHEMA = "rk-release-canary-manifest.v1";
export const RECEIPT_NAME = "@rightkit__release.canary.json";
export const EXIT = Object.freeze({ green: 0, regression: 10, incomplete: 11, usage: 2 });
/** Files whose digests the receipt binds; the pre-flight refuses a receipt whose scripts or policy changed since. */
export const DIGEST_FILES = Object.freeze([
  "scripts/ci/rk-release-canary.mjs", "scripts/ci/rk-release-canary-leg.mjs", "scripts/ci/release-version-pins.mjs", "ci/release-canary.json",
]);
export const EXPECTED_FILE = "ci/release-canary-expected.json";
export const CHECKS = Object.freeze(["new-fail-row", "pass-to-not-pass", "fatal-signature-changed", "tree-write", "gate-row"]);
const REGISTRY = "https://registry.npmjs.org";
const sha256 = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");

export class CanaryInfraError extends Error {}

export const fileDigest = (root, rel) => { try { return sha256(fs.readFileSync(path.join(root, rel))); } catch { return null; } };
export function canaryDigests(root = ROOT) {
  return { scripts: Object.fromEntries(DIGEST_FILES.map((f) => [f, fileDigest(root, f)])), expected: fileDigest(root, EXPECTED_FILE) };
}
export const receiptPath = (out, sha) => path.join(out, sha, RECEIPT_NAME);

// ---------------------------------------------------------------------------------------------------------------------
// process plumbing (injectable: every test passes its own exec)
// ---------------------------------------------------------------------------------------------------------------------

export function exec(command, args, { cwd, env, timeoutMs = 120_000 } = {}) {
  return new Promise((resolve) => {
    let stdout = "", stderr = "", timedOut = false;
    const child = spawn(command, args, { cwd, env: env ?? process.env, stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
    const timer = setTimeout(() => { timedOut = true; child.kill("SIGKILL"); }, timeoutMs);
    child.stdout.on("data", (c) => { stdout += c; });
    child.stderr.on("data", (c) => { stderr += c; });
    child.on("error", (e) => { clearTimeout(timer); resolve({ code: 127, stdout, stderr: `${stderr}${e.message}`, timedOut }); });
    child.on("close", (code) => { clearTimeout(timer); resolve({ code: code ?? 1, stdout, stderr, timedOut }); });
  });
}
const ok = (r) => r.code === 0 && !r.timedOut;

/** One retry for an infrastructure failure, then the error stands (the caller reports incomplete). */
async function once(fn, label) {
  try { return await fn(); } catch (first) {
    try { return await fn(); } catch (second) { throw new CanaryInfraError(`${label}: ${second.message ?? second} (after one retry; first: ${first.message ?? first})`); }
  }
}

// ---------------------------------------------------------------------------------------------------------------------
// matrix and expected findings
// ---------------------------------------------------------------------------------------------------------------------

export const readJson = (file) => JSON.parse(fs.readFileSync(file, "utf8"));
export const loadMatrix = (root = ROOT) => readJson(path.join(root, "ci", "release-canary.json"));
export const loadExpected = (root = ROOT) => readJson(path.join(root, EXPECTED_FILE));

/** Problems with the matrix itself. `exists` is injectable; the default checks the Mac workspace. */
export function matrixProblems(matrix, { exists = (p) => fs.existsSync(p) } = {}) {
  const problems = [];
  if (matrix?.schema !== 1 || matrix.unit !== UNIT || !Array.isArray(matrix.apps) || !matrix.apps.length) return ["ci/release-canary.json: schema 1, unit and apps are required"];
  const ids = new Set();
  for (const app of matrix.apps) {
    if (!/^[a-z][a-z0-9-]*$/.test(app.id ?? "") || ids.has(app.id)) problems.push(`app id ${JSON.stringify(app.id)} is missing or duplicated`);
    ids.add(app.id);
    for (const field of ["repoDir", "configPath"]) if (typeof app[field] !== "string" || !app[field] || path.isAbsolute(app[field]) || app[field].split("/").includes("..")) problems.push(`${app.id}: ${field} must be a relative path`);
    if (!Array.isArray(app.platforms) || !app.platforms.length || !app.platforms.every((p) => HOST_FOR_PLATFORM[p])) problems.push(`${app.id}: platforms must be from mac, win, ios`);
    if (typeof app.blocking !== "boolean") problems.push(`${app.id}: blocking must be true or false`);
    if (!exists(path.join(matrix.hosts.darwin.workspaceRoot, app.repoDir, app.configPath))) problems.push(`${app.id}: ${app.repoDir}/${app.configPath} does not exist under ${matrix.hosts.darwin.workspaceRoot}`);
  }
  for (const host of ["darwin", "win32"]) if (!matrix.hosts?.[host]?.workspaceRoot || !matrix.hosts[host].scratchRoot) problems.push(`hosts.${host} needs workspaceRoot and scratchRoot`);
  if (!matrix.hosts?.win32?.node || !/node\.exe$/i.test(matrix.hosts.win32.node)) problems.push("hosts.win32.node must pin the Dell's node.exe");
  if (matrix.dependency?.name !== "smol-toml" || !/^\d+\.\d+\.\d+$/.test(matrix.dependency?.version ?? "")) problems.push("dependency must pin smol-toml to an exact version");
  return problems;
}

/** Expected findings are exact: bound to one candidate version, app, platform, check and match. No wildcards. */
export function expectedProblems(file) {
  const problems = [];
  if (file?.schema !== 1 || !Array.isArray(file.entries)) return ["ci/release-canary-expected.json: schema 1 with an entries array is required"];
  const seen = new Set();
  file.entries.forEach((e, i) => {
    const at = `entries[${i}]`;
    for (const f of ["version", "app", "platform", "check", "match", "reason", "issue"]) if (typeof e?.[f] !== "string" || !e[f].trim()) problems.push(`${at}.${f} is required`);
    if (problems.some((p) => p.startsWith(at))) return;
    if (!/^\d+\.\d+\.\d+$/.test(e.version)) problems.push(`${at}.version must be one exact x.y.z`);
    if (!HOST_FOR_PLATFORM[e.platform]) problems.push(`${at}.platform must be mac, win or ios`);
    if (!CHECKS.includes(e.check)) problems.push(`${at}.check must be one of ${CHECKS.join(", ")} (a candidate crash can never be expected)`);
    for (const f of ["version", "app", "platform", "check", "match"]) if (/[*?]/.test(e[f])) problems.push(`${at}.${f} contains a wildcard`);
    if (e.reason.trim().length < 10) problems.push(`${at}.reason must say why`);
    if (!/^(?:#\d+|[\w.-]+\/[\w.-]+#\d+|https:\/\/github\.com\/[\w.-]+\/[\w.-]+\/issues\/\d+)$/.test(e.issue)) problems.push(`${at}.issue must be an issue reference`);
    const key = [e.version, e.app, e.platform, e.check, e.match].join("\0");
    if (seen.has(key)) problems.push(`${at} duplicates an earlier entry`);
    seen.add(key);
  });
  return problems;
}

/** The entry that excuses exactly this finding for exactly this candidate version, or null. A candidate crash never matches. */
export function expectedMatch(finding, entries, { version, app, platform }) {
  if (finding.kind === "candidate-crash") return null;
  return (entries ?? []).find((e) => e.version === version && e.app === app && e.platform === platform && e.check === finding.kind && e.match === finding.match) ?? null;
}

// ---------------------------------------------------------------------------------------------------------------------
// comparison
// ---------------------------------------------------------------------------------------------------------------------

const isInfra = (cell) => !cell || INFRA_STATUSES.includes(cell.status);
const notPass = (cell) => cell.status === "fail" || cell.status === "fatal";

/** Judge one (candidate, baseline) cell pair. Returns { verdict: "ok"|"regression"|"incomplete", findings }.
 * Finding severities: block (a regression), incomplete (never green), report (information). */
/** Broker admission: a crash or fail whose output carries an admission code is infrastructure, not a verdict, when the leg's
 * probe refused that same code or the baseline cell for the same app and platform shows it too. A baseline cell that shows
 * the code is itself infrastructure. A candidate-only refusal keeps its blocking verdict. */
export function admissionInfra(cell, { side, baseline, refused = null }) {
  if (!cell?.admissionCode || (cell.status !== "crash" && cell.status !== "fail")) return cell;
  const code = cell.admissionCode;
  if (refused !== code && side !== "baseline" && baseline?.admissionCode !== code) return cell;
  return { ...cell, refusedAs: cell.status, status: "infra", detail: `broker admission: ${code}` };
}

/** The doctor's refusal at its own app-check guard: it evaluated nothing of the app, so a baseline cell with this fatal cannot be
 * compared row by row (rightkit#1218). */
export const GUARD_REFUSED = /^the app check found problems in .*; nothing was started\./;
const guardRefused = (cell) => cell?.status === "fatal" && GUARD_REFUSED.test(String(cell.fatal?.signature ?? ""));

export function compareCells({ candidate: c, baseline: b, expected = [], version, app, platform, blocking = true, hostRepresentative = true, baselineCompleted = false }) {
  const findings = [];
  const add = (severity, kind, match, detail) => findings.push({ severity, kind, match, detail });
  const host = HOST_FOR_PLATFORM[platform];
  // A baseline refused at its guard evaluated nothing: every row the candidate reports is newly visible app state, not a
  // difference in doctor behaviour. Such rows are reported (never block); a candidate crash or tree write still blocks below.
  const unevaluable = b && !isInfra(b) && guardRefused(b) && c && !isInfra(c) && !guardRefused(c);
  const rowSeverity = (kind) => (unevaluable && kind !== "candidate-crash" && kind !== "tree-write" ? "report" : "block");

  if (!c) add("incomplete", "candidate-missing", app, "the candidate cell never reported");
  else if (isInfra(c)) add("incomplete", `candidate-${c.status}`, c.status, c.detail ?? "infrastructure failure after one retry");
  if (!b) add("incomplete", "baseline-missing", app, "the baseline cell never reported");
  else if (isInfra(b)) add("incomplete", `baseline-${b.status}`, b.status, b.detail ?? "infrastructure failure after one retry");
  if (!hostRepresentative) add("incomplete", "host-not-representative", platform, baselineCompleted ? `baseline completed without broker-managed compiler cache on ${host}` : `no baseline cell completed on ${host}`);

  if (c && !isInfra(c)) {
    if (c.status === "crash") add("block", "candidate-crash", c.detail ?? "crash", "the candidate doctor crashed; a crash can never be listed as expected");
    if (c.treeChanged) add("block", "tree-write", (c.treeDelta ?? []).join(";"), `the candidate doctor changed the app tree: ${(c.treeDelta ?? []).slice(0, 5).join("; ")}`);
    if (c.gatesRun && !b?.gatesRun) for (const name of c.gateFailNames ?? []) add(rowSeverity("gate-row"), "gate-row", name, unevaluable ? `candidate gate row ${name} fails; the baseline was refused at its guard and evaluated nothing (rightkit#1218)` : `candidate gate row ${name} fails and the baseline cannot run --gates`);

    if (b && !isInfra(b)) {
      if (b.status === "crash") {
        // Nothing to compare against: a passing candidate is merely unproven, but a candidate that fails or is fatal here
        // cannot be shown to be no worse than a baseline that crashed, so the cell is incomplete (never green).
        if (c.status === "pass") add("report", "unproven", app, "the baseline doctor crashed here, so this cell cannot show the candidate is no worse (the candidate passes)");
        else if (c.status !== "crash") add("incomplete", "baseline-crash-unproven", c.status, `the baseline doctor crashed and the candidate is ${c.status}; the candidate cannot be shown no worse`);
      } else if (c.status !== "crash") {
        if (b.status === "pass" && notPass(c)) add("block", "pass-to-not-pass", `pass->${c.status}`, `passed on the baseline, ${c.status} on the candidate`);
        const known = new Set(b.failNames ?? []);
        for (const name of c.failNames ?? []) {
          if (known.has(name)) add("report", "pre-existing-fail", name, "fails on the baseline too");
          else add(rowSeverity("new-fail-row"), "new-fail-row", name, unevaluable ? `[FAIL] ${name} on the candidate; the baseline was refused at its guard and evaluated nothing (rightkit#1218)` : `new [FAIL] ${name}`);
        }
        if (c.status === "fatal") {
          if (b.status === "fatal" && b.fatal?.signature === c.fatal?.signature) add("report", "pre-existing-fatal", c.fatal.signature, "same fatal on the baseline");
          else if (b.status !== "pass") add(rowSeverity("fatal-signature-changed"), "fatal-signature-changed", c.fatal?.signature ?? "", `fatal changed from ${b.status === "fatal" ? JSON.stringify(b.fatal?.signature) : b.status} to ${JSON.stringify(c.fatal?.signature)}${unevaluable ? " (the baseline was refused at its guard; rightkit#1218)" : ""}`);
        }
        for (const name of b.failNames ?? []) if (!(c.failNames ?? []).includes(name) && c.status !== "fatal") add("report", "fixed-row", name, "fails on the baseline, not on the candidate");
        if (b.status !== "pass" && c.status === "pass") add("report", "fixed", app, `${b.status} on the baseline, pass on the candidate`);
      }
    }
  }

  const excused = [];
  for (const f of findings) {
    if (f.severity !== "block") continue;
    const entry = expectedMatch(f, expected, { version, app, platform });
    if (entry) { f.severity = "report"; f.excusedBy = { issue: entry.issue, reason: entry.reason }; excused.push(entry); }
  }
  if (!blocking) for (const f of findings) if (f.severity !== "report") { f.reportOnly = f.severity; f.severity = "report"; }
  const verdict = findings.some((f) => f.severity === "block") ? "regression" : findings.some((f) => f.severity === "incomplete") ? "incomplete" : "ok";
  return { verdict, findings, excused };
}

/** Pair every declared cell, decide host representativeness, run compareCells, and roll up. */
export function compareAll({ matrix, candidateCells, baselineCells, expected, version, repos = {}, admissionRefused = {} }) {
  const key = (c) => `${c.id}:${c.platform}`;
  const refusedOn = (c) => admissionRefused[HOST_FOR_PLATFORM[c.platform]] ?? null;
  // Admission is classified against the raw baseline, then the baseline itself is classified; both feed the verdict.
  const rawBase = new Map(baselineCells.map((c) => [key(c), c]));
  const baseCells = baselineCells.map((c) => admissionInfra(c, { side: "baseline", refused: refusedOn(c) }));
  const base = new Map(baseCells.map((c) => [key(c), c]));
  const cand = new Map(candidateCells.map((c) => [key(c), admissionInfra(c, { side: "candidate", baseline: rawBase.get(key(c)), refused: refusedOn(c) })]));
  const representative = {};
  const completed = {};
  const guardOnly = {};
  for (const c of baseCells) {
    representative[c.host] ??= false;
    completed[c.host] ??= false;
    guardOnly[c.host] ??= true;
    if (c.compilerCache === true && c.status !== "crash") representative[c.host] = true;
    if (!isInfra(c) && c.status !== "crash") completed[c.host] = true;
    if (!guardRefused(c)) guardOnly[c.host] = false;
  }
  // A baseline refused at its guard on every app of a host evaluated nothing there, so it can never show the broker-managed
  // compiler cache; a completed candidate cell that ran with it proves the host just as well (#1219). A baseline that ran
  // (even to a crash) keeps the old rule: only its own cells make the host representative.
  for (const c of cand.values()) {
    representative[c.host] ??= false;
    if (guardOnly[c.host] === true && c.compilerCache === true && c.status !== "crash" && !isInfra(c)) representative[c.host] = true;
  }
  const entries = (expected?.entries ?? []).filter((e) => e.version === version);
  const used = new Set();
  const cells = [];
  for (const app of matrix.apps) {
    for (const platform of app.platforms) {
      const host = HOST_FOR_PLATFORM[platform];
      const pair = { candidate: cand.get(`${app.id}:${platform}`), baseline: base.get(`${app.id}:${platform}`) };
      const unresolved = repos[app.repoDir]?.error;
      const r = compareCells({ ...pair, expected: entries, version, app: app.id, platform, blocking: app.blocking, hostRepresentative: representative[host] !== false, baselineCompleted: completed[host] === true });
      for (const cell of [pair.candidate, pair.baseline]) if (cell?.unnormalizedBlobs?.length) r.findings.push({ severity: "report", kind: "unnormalized-blob", match: cell.unnormalizedBlobs.join(","), detail: "CRLF or mixed blob under a text attribute: tolerated, listed and hashed around the doctor" });
      if (unresolved && !pair.candidate && !pair.baseline) r.findings.unshift({ severity: app.blocking ? "incomplete" : "report", kind: "unresolved-app-commit", match: app.repoDir, detail: unresolved });
      const verdict = r.findings.some((f) => f.severity === "block") ? "regression" : r.findings.some((f) => f.severity === "incomplete") ? "incomplete" : "ok";
      for (const e of r.excused) used.add(e);
      const summary = (cell) => cell && ({ status: cell.status, refusedAs: cell.refusedAs ?? null, failNames: cell.failNames, gateFailNames: cell.gateFailNames, fatal: cell.fatal?.signature ?? null, treeChanged: cell.treeChanged, head: cell.head ?? null, headMatchesPin: cell.headMatchesPin ?? null, dirty: cell.dirty ?? null, gatesRun: cell.gatesRun ?? false, compilerCache: cell.compilerCache ?? null, normalizedSha256: cell.normalizedSha256 ?? null, noNodeModules: cell.noNodeModules ?? null, seconds: cell.seconds, attempts: cell.attempts ?? 1, detail: cell.detail ?? null, admissionCode: cell.admissionCode ?? null, unnormalizedBlobs: cell.unnormalizedBlobs ?? [] });
      cells.push({ id: app.id, platform, host, blocking: app.blocking, verdict, findings: r.findings, candidate: summary(pair.candidate), baseline: summary(pair.baseline) });
    }
  }
  const blockingCells = cells.filter((c) => c.blocking);
  const verdict = blockingCells.some((c) => c.verdict === "regression") ? "regression" : blockingCells.some((c) => c.verdict === "incomplete") ? "incomplete" : "green";
  return { verdict, cells, representative, staleExpected: entries.filter((e) => !used.has(e)).map((e) => ({ app: e.app, platform: e.platform, check: e.check, match: e.match, issue: e.issue })) };
}

// ---------------------------------------------------------------------------------------------------------------------
// inputs: app commits, candidate, baseline, dependency
// ---------------------------------------------------------------------------------------------------------------------

/** The commit each app repository's origin main points at, read once per repository on the Mac. */
export async function resolveAppShas(apps, { workspaceRoot, run = exec } = {}) {
  const repos = {};
  await Promise.all([...new Set(apps.map((a) => a.repoDir))].map(async (repoDir) => {
    try {
      const live = path.join(workspaceRoot, repoDir);
      const originRun = await run("git", ["-C", live, "remote", "get-url", "origin"], { timeoutMs: 30_000 });
      const origin = originRun.stdout.trim();
      if (!ok(originRun) || !origin) throw new Error(`no origin remote in ${live}`);
      const sha = await once(async () => {
        const r = await run("git", ["ls-remote", origin, "refs/heads/main"], { timeoutMs: 60_000 });
        const line = r.stdout.split("\n").find((l) => /\trefs\/heads\/main$/.test(l));
        if (!ok(r) || !line) throw new Error(`git ls-remote ${origin} refs/heads/main: ${(r.stderr || r.stdout || "no output").trim().slice(0, 200)}`);
        const value = line.split("\t")[0];
        if (!/^[0-9a-f]{40}$/.test(value)) throw new Error(`ls-remote returned ${JSON.stringify(value)}`);
        return value;
      }, `${repoDir} ls-remote`);
      repos[repoDir] = { origin, sha };
    } catch (error) { repos[repoDir] = { error: String(error.message ?? error) }; }
  }));
  return repos;
}

/** `pnpm pack` of packages/release exactly as committed at `sha` (never the working tree). */
export async function packCandidate({ root = ROOT, sha, version, outDir, run = exec, pnpm = "pnpm" }) {
  const work = fs.mkdtempSync(path.join(outDir, "pack-"));
  try {
    const tar = path.join(work, "src.tar");
    const archive = await run("git", ["-C", root, "archive", "--format=tar", "-o", tar, sha, "packages/release"], { timeoutMs: 120_000 });
    if (!ok(archive)) throw new CanaryInfraError(`git archive ${sha}: ${archive.stderr.trim().slice(0, 300)}`);
    const src = path.join(work, "src");
    fs.mkdirSync(src);
    const untar = await run("tar", ["-xf", tar, "-C", src], { timeoutMs: 120_000 });
    if (!ok(untar)) throw new CanaryInfraError(`tar -x: ${untar.stderr.trim().slice(0, 300)}`);
    const dir = path.join(src, "packages", "release");
    const packed = path.join(work, "out");
    fs.mkdirSync(packed);
    const r = await run(pnpm, ["pack", "--pack-destination", packed], { cwd: dir, timeoutMs: 300_000, env: { ...process.env, npm_config_manage_package_manager_versions: "false", COREPACK_ENABLE_STRICT: "0" } });
    if (!ok(r)) throw new CanaryInfraError(`pnpm pack at ${sha}: ${(r.stderr || r.stdout).trim().slice(0, 400)}`);
    const tgz = fs.readdirSync(packed).filter((f) => f.endsWith(".tgz"));
    if (tgz.length !== 1) throw new CanaryInfraError(`pnpm pack produced ${tgz.length} tarballs`);
    const file = path.join(outDir, "candidate.tgz");
    fs.copyFileSync(path.join(packed, tgz[0]), file);
    const bytes = fs.readFileSync(file);
    const pkg = JSON.parse(readTarball(bytes).find((e) => e.path === "package/package.json")?.data.toString("utf8") ?? "null");
    if (pkg?.name !== UNIT || pkg.version !== version) throw new CanaryInfraError(`packed ${pkg?.name}@${pkg?.version}, expected ${UNIT}@${version}`);
    return { file, sha256: sha256(bytes), version, dependencies: pkg.dependencies ?? {} };
  } finally { fs.rmSync(work, { recursive: true, force: true }); }
}

async function fetchBytes(fetchImpl, url, what) {
  const r = await fetchImpl(url, { signal: AbortSignal.timeout(120_000) });
  if (!r.ok) throw new Error(`${what}: HTTP ${r.status}`);
  return Buffer.from(await r.arrayBuffer());
}
function checkIntegrity(bytes, dist, what) {
  const m = /^(sha512)-(.+)$/.exec(dist?.integrity ?? "");
  if (!m) throw new Error(`${what}: the packument carries no sha512 integrity`);
  const actual = crypto.createHash("sha512").update(bytes).digest("base64");
  if (actual !== m[2]) throw new Error(`${what}: tarball integrity ${actual.slice(0, 16)}... differs from the packument's ${m[2].slice(0, 16)}...`);
  return dist.integrity;
}
async function fetchVersion({ fetchImpl, name, version, file, what }) {
  const doc = await (async () => {
    const r = await fetchImpl(`${REGISTRY}/${name.replace("/", "%2F")}`, { signal: AbortSignal.timeout(60_000) });
    if (!r.ok) throw new Error(`${what}: packument HTTP ${r.status}`);
    return r.json();
  })();
  const wanted = version ?? doc["dist-tags"]?.latest;
  const dist = doc.versions?.[wanted]?.dist;
  if (!wanted || !dist?.tarball) throw new Error(`${what}: the registry lists no ${name}@${wanted}`);
  const bytes = await fetchBytes(fetchImpl, dist.tarball, what);
  const integrity = checkIntegrity(bytes, dist, what);
  fs.writeFileSync(file, bytes);
  return { file, version: wanted, sha256: sha256(bytes), integrity };
}
/** The registry's latest @rightkit/release tarball, verified against the packument integrity. */
export const fetchBaseline = ({ fetchImpl = fetch, outDir }) => once(() => fetchVersion({ fetchImpl, name: UNIT, file: path.join(outDir, "baseline.tgz"), what: "baseline" }), "baseline fetch");
/** The pinned smol-toml tarball, verified the same way. */
export const fetchDependency = ({ fetchImpl = fetch, outDir, name = "smol-toml", version = "1.9.1" }) => once(() => fetchVersion({ fetchImpl, name, version, file: path.join(outDir, "smol-toml.tgz"), what: `${name} ${version}` }), `${name} fetch`);

/** The leg's whole input: apps, pinned app commits, artifact digests, limits and per-host paths. */
export function buildManifest({ sha, version, matrix, repos, candidate, baseline, dependency }) {
  return {
    schema: MANIFEST_SCHEMA, sha, version, unit: UNIT, gates: true,
    limits: matrix.limits, hosts: Object.fromEntries(Object.entries(matrix.hosts).map(([h, v]) => [h, { workspaceRoot: v.workspaceRoot, scratchRoot: v.scratchRoot, parallel: v.parallel }])),
    apps: matrix.apps, repos,
    artifacts: {
      candidate: { file: "candidate.tgz", sha256: candidate.sha256, version: candidate.version },
      baseline: { file: "baseline.tgz", sha256: baseline.sha256, version: baseline.version, integrity: baseline.integrity },
      dependency: { file: "smol-toml.tgz", sha256: dependency.sha256, version: dependency.version, integrity: dependency.integrity },
    },
  };
}

// ---------------------------------------------------------------------------------------------------------------------
// legs
// ---------------------------------------------------------------------------------------------------------------------

function verifyLegReceipt(receipt, { sha, host, bundleDir }) {
  const problems = [];
  if (receipt?.schema !== "rk-release-canary-leg.v1") problems.push("schema");
  if (receipt?.host !== host) problems.push(`host ${receipt?.host}`);
  if (receipt?.sha !== sha) problems.push("sha");
  if (receipt?.manifestSha256 !== sha256(fs.readFileSync(path.join(bundleDir, "manifest.json")))) problems.push("manifest digest");
  if (receipt?.legSha256 !== sha256(fs.readFileSync(path.join(bundleDir, "canary-leg.mjs")))) problems.push("leg script digest");
  return problems;
}

/** On a timeout, ssh dies but the Dell's node process tree does not: find this SHA's leg by command line and kill the tree. */
export async function killRemoteLeg({ run, winCommand = "win", sha }) {
  const command = `Get-CimInstance Win32_Process -Filter "Name='node.exe'" | Where-Object { $_.CommandLine -like '*${sha}*canary-leg.mjs*' } | ForEach-Object { & taskkill.exe /PID $_.ProcessId /T /F | Out-Null }`;
  try { return await run(winCommand, [command], { timeoutMs: 60_000 }); } catch { return null; }
}

/** Run one host's leg and return its verified receipt: { host, receipt } or { host, error }. One retry. */
export async function runLeg(host, { bundleDir, matrix, sha, run = exec, node = process.execPath, winCommand = "win" }) {
  const attempt = async () => {
    const budgetMs = ((matrix.limits?.totalMinutes ?? 20) + 4) * 60_000;
    if (host === "darwin") {
      const out = path.join(bundleDir, "leg-darwin.json");
      fs.rmSync(out, { force: true });
      const r = await run(node, [path.join(bundleDir, "canary-leg.mjs"), "--manifest", path.join(bundleDir, "manifest.json"), "--out", out], { timeoutMs: budgetMs });
      if (!ok(r) || !fs.existsSync(out)) throw new Error(`mac leg failed (exit ${r.code}${r.timedOut ? ", timed out" : ""}): ${(r.stderr || r.stdout).trim().slice(-300)}`);
      return JSON.parse(fs.readFileSync(out, "utf8"));
    }
    const cfg = matrix.hosts.win32;
    const remote = `${cfg.scratchRoot}/rk-canary/${sha}`;
    const remoteWin = remote.replaceAll("/", "\\");
    const ssh = cfg.ssh ?? "win";
    const mk = await run(winCommand, [`New-Item -ItemType Directory -Force -Path '${remoteWin}' | Out-Null`], { timeoutMs: 60_000 });
    if (!ok(mk)) throw new Error(`dell: cannot create ${remoteWin}: ${(mk.stderr || mk.stdout).trim().slice(0, 300)}`);
    const files = ["candidate.tgz", "baseline.tgz", "smol-toml.tgz", "canary-leg.mjs", "manifest.json"].map((f) => path.join(bundleDir, f));
    const put = await run("scp", ["-q", ...files, `${ssh}:${remote}/`], { timeoutMs: 300_000 });
    if (!ok(put)) throw new Error(`dell: scp bundle: ${put.stderr.trim().slice(0, 300)}`);
    const command = `& '${cfg.node}' '${remoteWin}\\canary-leg.mjs' --manifest '${remoteWin}\\manifest.json' --out '${remoteWin}\\leg-win32.json'`;
    const r = await run(winCommand, [command], { timeoutMs: budgetMs });
    if (r.timedOut) await killRemoteLeg({ run, winCommand, sha });
    if (!ok(r)) throw new Error(`dell leg failed (exit ${r.code}${r.timedOut ? ", timed out" : ""}): ${(r.stderr || r.stdout).trim().slice(-300)}`);
    const back = path.join(bundleDir, "leg-win32.json");
    fs.rmSync(back, { force: true });
    const get = await run("scp", ["-q", `${ssh}:${remote}/leg-win32.json`, back], { timeoutMs: 120_000 });
    if (!ok(get) || !fs.existsSync(back)) throw new Error(`dell: scp receipt: ${get.stderr.trim().slice(0, 300)}`);
    return JSON.parse(fs.readFileSync(back, "utf8"));
  };
  try {
    const receipt = await once(async () => {
      const r = await attempt();
      const problems = verifyLegReceipt(r, { sha, host, bundleDir });
      if (problems.length) throw new Error(`${host} leg receipt does not match the bundle (${problems.join(", ")})`);
      return r;
    }, `${host} leg`);
    return { host, receipt };
  } catch (error) { return { host, error: String(error.message ?? error) }; }
}

// ---------------------------------------------------------------------------------------------------------------------
// receipt and the whole run
// ---------------------------------------------------------------------------------------------------------------------

export function writeReceipt(out, sha, receipt) {
  const file = receiptPath(out, sha);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const temp = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(temp, JSON.stringify(receipt, null, 2));
  fs.renameSync(temp, file);
  return file;
}

/** The whole canary for one release SHA. Never throws for a judged outcome: returns { code, verdict, receiptFile, lines }.
 * `expectSha` (--sha) is the commit the caller already checked: the canary refuses unless HEAD is that commit when it
 * starts, and a run whose HEAD moved before the verdict is incomplete, never green (a second chat may commit to this
 * checkout while it runs). */
export async function runCanary({ root = ROOT, out = DEFAULT_OUT, unit = UNIT, expectSha = null, deps = {} } = {}) {
  const lines = [];
  const say = (l) => lines.push(l);
  const run = deps.exec ?? exec;
  if (unit !== UNIT) return { code: EXIT.usage, verdict: "usage", lines: [`rk-release-canary: only ${UNIT} has a canary`] };
  const headRun = await run("git", ["-C", root, "rev-parse", "HEAD"]);
  const dirty = await run("git", ["-C", root, "status", "--porcelain", "--untracked-files=no"]);
  const sha = headRun.stdout.trim();
  if (!ok(headRun) || !/^[0-9a-f]{40}$/.test(sha)) return { code: EXIT.usage, verdict: "usage", lines: ["rk-release-canary: cannot read HEAD"] };
  if (expectSha && sha !== expectSha) return { code: EXIT.usage, verdict: "usage", lines: [`rk-release-canary: HEAD is ${sha.slice(0, 12)}, not the checked ${String(expectSha).slice(0, 12)}; nothing canaried`] };
  if (!ok(dirty) || dirty.stdout.trim()) return { code: EXIT.usage, verdict: "usage", lines: ["rk-release-canary: tracked files are modified; the canary proves a commit, not a working tree"] };

  const pkg = readJson(path.join(root, "packages", "release", "package.json"));
  const version = pkg.version;
  const receipt = { schema: RECEIPT_SCHEMA, sha, unit, version, startedAt: new Date().toISOString(), host: process.platform, verdict: "incomplete", ok: false, digests: canaryDigests(root), stages: {} };
  const finish = (verdict, extra = {}) => {
    Object.assign(receipt, extra, { verdict, ok: verdict === "green", finishedAt: new Date().toISOString() });
    const file = writeReceipt(out, sha, receipt);
    say(`${verdict === "green" ? "GREEN" : verdict.toUpperCase()} canary ${unit}@${version} at ${sha.slice(0, 12)}: receipt ${file}`);
    return { code: verdict === "green" ? EXIT.green : verdict === "regression" ? EXIT.regression : EXIT.incomplete, verdict, receiptFile: file, lines, receipt };
  };

  // Stage 0: the version pins, then the canary's own policy files.
  const pins = versionPinProblems(root, unit);
  receipt.stages.pins = { ok: pins.length === 0, problems: pins };
  let matrix, expected;
  try { matrix = loadMatrix(root); expected = loadExpected(root); } catch (error) { receipt.stages.policy = { ok: false, problems: [`cannot read the canary policy files: ${error.message}`] }; say(receipt.stages.policy.problems[0]); return finish("regression"); }
  const policy = [...matrixProblems(matrix, { exists: deps.exists }), ...expectedProblems(expected)];
  receipt.stages.policy = { ok: policy.length === 0, problems: policy };
  if (pins.length || policy.length) {
    for (const p of [...pins, ...policy]) say(`stage 0: ${p}`);
    return finish("regression");
  }

  const bundleDir = path.join(out, sha, "canary");
  fs.rmSync(bundleDir, { recursive: true, force: true });
  fs.mkdirSync(bundleDir, { recursive: true });
  let repos, candidate, baseline, dependency;
  try {
    repos = await (deps.resolveAppShas ?? resolveAppShas)(matrix.apps, { workspaceRoot: matrix.hosts.darwin.workspaceRoot, run });
    const blockingMissing = matrix.apps.filter((a) => a.blocking && repos[a.repoDir]?.error).map((a) => `${a.id}: ${repos[a.repoDir].error}`);
    receipt.appShas = Object.fromEntries(Object.entries(repos).map(([k, v]) => [k, v.sha ?? null]));
    if (blockingMissing.length) { receipt.stages.appShas = { ok: false, problems: blockingMissing }; for (const m of blockingMissing) say(`app commit: ${m}`); return finish("incomplete", { repos }); }
    candidate = await (deps.packCandidate ?? packCandidate)({ root, sha, version, outDir: bundleDir, run });
    const spec = candidate.dependencies?.[matrix.dependency.name];
    if (spec !== matrix.dependency.version) { receipt.stages.dependency = { ok: false, problems: [`candidate depends on ${matrix.dependency.name} ${spec}, the canary pins ${matrix.dependency.version}`] }; say(receipt.stages.dependency.problems[0]); return finish("regression"); }
    [baseline, dependency] = await Promise.all([(deps.fetchBaseline ?? fetchBaseline)({ fetchImpl: deps.fetchImpl, outDir: bundleDir }), (deps.fetchDependency ?? fetchDependency)({ fetchImpl: deps.fetchImpl, outDir: bundleDir, ...matrix.dependency })]);
  } catch (error) { receipt.stages.inputs = { ok: false, problems: [String(error.message ?? error)] }; say(`inputs: ${error.message}`); return finish("incomplete"); }
  receipt.artifacts = { candidate: { sha256: candidate.sha256, version: candidate.version }, baseline: { sha256: baseline.sha256, version: baseline.version, integrity: baseline.integrity }, dependency: { sha256: dependency.sha256, version: dependency.version } };

  const manifest = buildManifest({ sha, version, matrix, repos, candidate, baseline, dependency });
  fs.writeFileSync(path.join(bundleDir, "manifest.json"), JSON.stringify(manifest, null, 2));
  fs.copyFileSync(path.join(root, "scripts", "ci", "rk-release-canary-leg.mjs"), path.join(bundleDir, "canary-leg.mjs"));

  const hosts = ["darwin", "win32"].filter((h) => matrix.apps.some((a) => a.platforms.some((p) => HOST_FOR_PLATFORM[p] === h)));
  const legs = await Promise.all(hosts.map((h) => (deps.runLeg ?? runLeg)(h, { bundleDir, matrix, sha, run, node: deps.node })));
  const cells = { candidate: [], baseline: [] };
  const admissionRefused = {};
  receipt.legs = {};
  for (const leg of legs) {
    if (leg.error) { receipt.legs[leg.host] = { ok: false, error: leg.error }; say(`${leg.host} leg: ${leg.error}`); continue; }
    receipt.legs[leg.host] = { ok: true, node: leg.receipt.node, seconds: leg.receipt.seconds, sides: leg.receipt.sides, sideErrors: leg.receipt.sideErrors, admissionRefused: leg.receipt.admissionRefused ?? null, admissionProbe: leg.receipt.admissionProbe ?? null };
    admissionRefused[leg.host] = leg.receipt.admissionRefused ?? null;
    for (const c of leg.receipt.cells) cells[c.side]?.push(c);
  }
  const result = compareAll({ matrix, candidateCells: cells.candidate, baselineCells: cells.baseline, expected, version, repos, admissionRefused });
  const headAfter = (await run("git", ["-C", root, "rev-parse", "HEAD"])).stdout.trim();
  if (headAfter !== sha) {
    receipt.stages.head = { ok: false, problems: [`HEAD moved from ${sha.slice(0, 12)} to ${headAfter.slice(0, 12)} while the canary ran`] };
    say(receipt.stages.head.problems[0]);
    return finish("incomplete", { cells: result.cells });
  }
  for (const c of result.cells) for (const f of c.findings) if (f.severity !== "report" || ["pre-existing-fail", "pre-existing-fatal", "fixed", "fixed-row", "unproven"].includes(f.kind) || f.excusedBy) say(`${f.severity.toUpperCase().padEnd(10)} ${c.id}/${c.platform} ${f.kind}${f.match && f.kind !== "unproven" ? ` ${JSON.stringify(f.match)}` : ""}${f.excusedBy ? ` (expected: ${f.excusedBy.issue})` : ""}${f.reportOnly ? " [report-only app]" : ""}`);
  return finish(result.verdict, { cells: result.cells, representative: result.representative, staleExpected: result.staleExpected });
}

export function parseArgs(argv) {
  const opts = { unit: UNIT, out: DEFAULT_OUT };
  for (let i = 0; i < argv.length; i += 1) {
    const take = () => { const v = argv[i + 1]; if (v === undefined || v.startsWith("--")) throw new Error(`${argv[i]} needs a value`); i += 1; return v; };
    if (argv[i] === "--unit") opts.unit = take();
    else if (argv[i] === "--sha") { opts.expectSha = take(); if (!/^[0-9a-f]{40}$/.test(opts.expectSha)) throw new Error("--sha needs a full 40-hex commit"); }
    else if (argv[i] === "--out") opts.out = path.resolve(take());
    else throw new Error(`unknown argument ${argv[i]}`);
  }
  return opts;
}

export async function main(argv, deps = {}) {
  let opts;
  try { opts = parseArgs(argv); } catch (error) { process.stderr.write(`rk-release-canary: ${error.message}\n`); return EXIT.usage; }
  const r = await runCanary({ ...opts, deps });
  for (const line of r.lines) process.stdout.write(`${line}\n`);
  return r.code;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).then((code) => process.exit(code), (error) => { process.stderr.write(`rk-release-canary: ${error.message}\n`); process.exit(EXIT.usage); });
}
