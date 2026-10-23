#!/usr/bin/env node
// rk-release-canary-leg: one host's half of the @rightkit/release pipeline canary. Standalone, Node built-ins only: the
// coordinator (rk-release-canary.mjs) copies this single file, two tarballs, smol-toml and a manifest to each host and
// runs it there, so it can import nothing from the workspace it judges.
//
//   node rk-release-canary-leg.mjs --manifest <path> [--out <leg receipt path>]
//
// For every app cell the manifest assigns to this host it runs `right-release doctor` twice, from the SAME pinned app
// commit: once with the registry baseline (latest published @rightkit/release) and once with the candidate tarball.
// It reports facts (status, [FAIL] rows, fatal signature, whether the app tree changed); the coordinator judges them.
// Hard rules: exports are scratch clones, live repositories are never fetched into or modified; `--admission` is never
// passed; the doctor must leave the export byte-for-byte clean.
import { spawn, spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import { fileURLToPath } from "node:url";

export const LEG_SCHEMA = "rk-release-canary-leg.v1";
export const HOST_FOR_PLATFORM = Object.freeze({ mac: "darwin", ios: "darwin", win: "win32" });
export const DEFAULT_CELL_MS = 5 * 60_000;
export const DEFAULT_TOTAL_MS = 20 * 60_000;
const OUTPUT_CAP = 4 * 1024 * 1024;
export const INFRA_STATUSES = Object.freeze(["timeout", "recoverable", "infra", "budget"]);
const sha256 = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");

// ---------------------------------------------------------------------------------------------------------------------
// tarballs (npm pack output): gzip + ustar/pax, one leading directory stripped, nothing may leave the destination
// ---------------------------------------------------------------------------------------------------------------------

const cString = (buf) => { const end = buf.indexOf(0); return buf.toString("utf8", 0, end === -1 ? buf.length : end); };
const octal = (buf) => { const t = cString(buf).trim(); return t ? Number.parseInt(t, 8) : 0; };

/** Entries of a gzipped tar: { path, type: "file"|"dir", mode, data }. Throws on links, devices and unsafe paths. */
export function readTarball(bytes) {
  const tar = zlib.gunzipSync(bytes);
  const entries = [];
  let offset = 0;
  let longName = null;
  let pax = {};
  while (offset + 512 <= tar.length) {
    const header = tar.subarray(offset, offset + 512);
    if (header.every((b) => b === 0)) break;
    const size = octal(header.subarray(124, 136));
    const type = String.fromCharCode(header[156] || 48);
    const mode = octal(header.subarray(100, 108));
    let name = cString(header.subarray(0, 100));
    if (cString(header.subarray(257, 262)) === "ustar") {
      const prefix = cString(header.subarray(345, 500));
      if (prefix) name = `${prefix}/${name}`;
    }
    const body = tar.subarray(offset + 512, offset + 512 + size);
    offset += 512 + Math.ceil(size / 512) * 512;
    if (type === "x") { pax = Object.fromEntries([...body.toString("utf8").matchAll(/\d+ ([^=]+)=([^\n]*)\n/g)].map((m) => [m[1], m[2]])); continue; }
    if (type === "g") continue;
    if (type === "L") { longName = cString(body); continue; }
    name = pax.path ?? longName ?? name;
    pax = {}; longName = null;
    if (type === "5") { entries.push({ path: name, type: "dir", mode }); continue; }
    if (type !== "0") throw new Error(`tarball entry ${JSON.stringify(name)} has unsupported type ${JSON.stringify(type)}`);
    entries.push({ path: name, type: "file", mode, data: Buffer.from(body) });
  }
  return entries;
}

/** Extract a package tarball into dest (the leading `package/` directory is stripped). */
export function extractTarball(file, dest) {
  const entries = readTarball(fs.readFileSync(file));
  fs.mkdirSync(dest, { recursive: true });
  const root = path.resolve(dest);
  let files = 0;
  for (const entry of entries) {
    const parts = entry.path.split("/").filter(Boolean);
    parts.shift();
    if (!parts.length) continue;
    if (parts.some((p) => p === ".." || p === "." || p.includes("\\") || /^[A-Za-z]:/.test(p))) throw new Error(`tarball path escapes its package: ${JSON.stringify(entry.path)}`);
    const target = path.resolve(root, ...parts);
    if (target !== root && !target.startsWith(root + path.sep)) throw new Error(`tarball path escapes its package: ${JSON.stringify(entry.path)}`);
    if (entry.type === "dir") { fs.mkdirSync(target, { recursive: true }); continue; }
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.writeFileSync(target, entry.data);
    if (process.platform !== "win32") fs.chmodSync(target, entry.mode & 0o111 ? 0o755 : 0o644);
    files += 1;
  }
  return { files };
}

/** `<side>/node_modules/@rightkit/release` and `<side>/node_modules/smol-toml`, from the digest-checked bundle tarballs.
 * The side directory is also the doctor's RIGHTKIT_WORKSPACE_ROOT; app exports sit beside node_modules. */
export function prepareSide({ sideDir, release, dependency, node = process.execPath }) {
  for (const t of [release, dependency]) {
    const actual = sha256(fs.readFileSync(t.file));
    if (actual !== t.sha256) throw new Error(`${path.basename(t.file)} digest ${actual} differs from the manifest's ${t.sha256}`);
  }
  const modules = path.join(sideDir, "node_modules");
  fs.rmSync(modules, { recursive: true, force: true });
  const releaseDir = path.join(modules, "@rightkit", "release");
  extractTarball(release.file, releaseDir);
  extractTarball(dependency.file, path.join(modules, "smol-toml"));
  const manifest = JSON.parse(fs.readFileSync(path.join(releaseDir, "package.json"), "utf8"));
  if (manifest.name !== "@rightkit/release") throw new Error(`extracted package is ${manifest.name}, not @rightkit/release`);
  if (release.version && manifest.version !== release.version) throw new Error(`extracted @rightkit/release is ${manifest.version}, the manifest says ${release.version}`);
  const bin = path.join(releaseDir, "cli", "right-release.mjs");
  if (!fs.existsSync(bin)) throw new Error("extracted @rightkit/release has no cli/right-release.mjs");
  return { sideDir, releaseDir, bin, version: manifest.version, ...detectGates({ releaseDir, bin, node }) };
}

/** Candidates from this version on must be able to run `doctor --gates`; one that cannot is a broken candidate, not an old one. */
export const GATES_REQUIRED_FROM = "0.6.4";
export function gatesRequired(version) {
  const v = /^(\d+)\.(\d+)\.(\d+)/.exec(String(version));
  const need = GATES_REQUIRED_FROM.split(".").map(Number);
  if (!v) return false;
  for (let i = 0; i < 3; i += 1) if (Number(v[i + 1]) !== need[i]) return Number(v[i + 1]) > need[i];
  return true;
}

/** Does this package run `doctor --gates`? Asked of the CLI itself (`right-release --help` lists `doctor ... [--gates]`),
 * so a refactor that moves the flag's parsing cannot silently turn gates off; the source text is only a fallback. */
export function detectGates({ releaseDir, bin, node = process.execPath }) {
  let via = null;
  const help = spawnSync(node, [bin, "--help"], { cwd: releaseDir, encoding: "utf8", timeout: 30_000, windowsHide: true, env: { ...process.env, RIGHT_RELEASE_NO_UPDATE_CHECK: "1" } });
  if (help.status === 0 && /^\s*doctor\b[^\n]*--gates\b/m.test(`${help.stdout}\n${help.stderr}`)) via = "help";
  if (!via) {
    let source = "";
    try { source = fs.readFileSync(path.join(releaseDir, "release.mjs"), "utf8"); } catch { /* an old package without it simply has no gates */ }
    if (/["']--gates["']/.test(source)) via = "source";
  }
  return { supportsGates: via !== null, gatesVia: via };
}

// ---------------------------------------------------------------------------------------------------------------------
// scratch exports of the live app repositories
// ---------------------------------------------------------------------------------------------------------------------

const GIT_ENV = { ...process.env, GIT_TERMINAL_PROMPT: "0", GIT_OPTIONAL_LOCKS: "0", GCM_INTERACTIVE: "never" };
export const GIT_TIMEOUT_MS = 120_000;
function git(cwd, args, { allowFail = false, timeout = GIT_TIMEOUT_MS } = {}) {
  const r = spawnSync("git", args, { cwd, encoding: "utf8", env: GIT_ENV, maxBuffer: 64 * 1024 * 1024, windowsHide: true, timeout, killSignal: "SIGKILL" });
  if (r.status !== 0 && !allowFail) {
    const error = new Error(`git ${args.join(" ")}: ${r.error?.code === "ETIMEDOUT" ? `timed out after ${timeout} ms` : (r.stderr || r.error?.message || "").trim().slice(0, 400)}`);
    error.infra = true;
    throw error;
  }
  return { status: r.status, stdout: r.stdout ?? "", stderr: r.stderr ?? "" };
}

/** A fresh scratch clone of the live repository at exactly `sha`, on an attached `main`, origin = the real remote.
 * `git clone --local` hard-links objects and only reads the live repository; nothing is ever fetched into it. */
export function prepareExport({ live, origin, sha, dest }) {
  if (!/^[0-9a-f]{40}$/.test(sha ?? "")) throw Object.assign(new Error(`invalid app commit ${JSON.stringify(sha)}`), { infra: true });
  if (!origin) throw Object.assign(new Error("no origin URL for the export"), { infra: true });
  if (!fs.existsSync(path.join(live, ".git"))) throw Object.assign(new Error(`${live} is not a git checkout on this host`), { infra: true });
  if (git(live, ["cat-file", "-e", `${sha}^{commit}`], { allowFail: true }).status !== 0) throw Object.assign(new Error(`commit ${sha} is not in the live repository ${live}; update it (the canary never fetches into live repositories)`), { infra: true });
  fs.rmSync(dest, { recursive: true, force: true });
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  git(path.dirname(dest), ["clone", "--local", "--no-checkout", "--quiet", live, dest], { timeout: 300_000 });
  git(dest, ["remote", "set-url", "origin", origin]);
  git(dest, ["checkout", "--quiet", "-B", "main", sha]);
  git(dest, ["update-ref", "refs/remotes/origin/main", sha]);
  git(dest, ["reset", "--quiet", "--hard"]);
  git(dest, ["clean", "-ffdxq"]);
  const tree = treeDigest(dest);
  if (tree.lines.length) throw Object.assign(new Error(`fresh export is not clean: ${tree.lines.slice(0, 3).join("; ")}`), { infra: true });
  return { dest, sha, head: git(dest, ["rev-parse", "HEAD"]).stdout.trim() };
}

/** `git status --porcelain --ignored=matching`, sorted, and its digest. */
export function treeDigest(repoRoot) {
  const r = git(repoRoot, ["-c", "core.fsmonitor=false", "status", "--porcelain", "--ignored=matching"]);
  const lines = r.stdout.split(/\r?\n/).filter(Boolean).sort();
  return { digest: sha256(lines.join("\n")), lines };
}

// ---------------------------------------------------------------------------------------------------------------------
// the doctor
// ---------------------------------------------------------------------------------------------------------------------

/** Kill a child and everything it started. */
function killTree(child) {
  try {
    if (process.platform === "win32") spawnSync("taskkill", ["/PID", String(child.pid), "/T", "/F"], { windowsHide: true });
    else process.kill(-child.pid, "SIGKILL");
  } catch { try { child.kill("SIGKILL"); } catch { /* gone */ } }
}

/** Run `right-release doctor` from the app root. gates is passed only for a candidate that supports it; --admission never. */
export function runDoctor({ node = process.execPath, bin, appRoot, configRel, platform, gates = false, sideDir, tmpDir, timeoutMs = DEFAULT_CELL_MS, env = process.env }) {
  const args = [bin, "doctor", "--config", configRel, "--platform", platform];
  if (gates) args.push("--gates");
  if (args.includes("--admission")) throw new Error("the canary never passes --admission");
  fs.mkdirSync(tmpDir, { recursive: true });
  const childEnv = { ...env, RIGHTKIT_WORKSPACE_ROOT: sideDir, TMPDIR: tmpDir, TEMP: tmpDir, TMP: tmpDir };
  return new Promise((resolve) => {
    const started = Date.now();
    let stdout = "", stderr = "", timedOut = false, settled = false;
    const finish = (extra) => { if (settled) return; settled = true; clearTimeout(timer); resolve({ stdout, stderr, timedOut, seconds: Math.round((Date.now() - started) / 1000), args, ...extra }); };
    const child = spawn(node, args, { cwd: appRoot, env: childEnv, stdio: ["ignore", "pipe", "pipe"], detached: process.platform !== "win32", windowsHide: true });
    const take = (which) => (chunk) => { const text = chunk.toString("utf8"); if (which === "out") { if (stdout.length < OUTPUT_CAP) stdout += text; } else if (stderr.length < OUTPUT_CAP) stderr += text; };
    child.stdout.on("data", take("out"));
    child.stderr.on("data", take("err"));
    const timer = setTimeout(() => { timedOut = true; killTree(child); }, timeoutMs);
    child.on("error", (error) => finish({ exit: null, signal: null, spawnError: String(error.message ?? error) }));
    child.on("close", (code, signal) => finish({ exit: code, signal }));
  });
}

const NOT_FATAL = /^right-release: (?:newer RightKit version|LANE TASK|WARNING|note |devtools allowance)/;
// Preflight rows are `  [ok  ] name: detail`; `right-release doctor --gates` prints each pre-build gate finding as
// `  [FAIL] gate:<name>: <detail>` (packages/release/release.mjs). A detail can continue on following lines; only
// lines that start a row matter here. Gate rows are the rows whose name starts `gate:`.
const ROW = /^\s*\[(ok|OK|ok  |WARN|FAIL)\]\s+(gate:[A-Za-z0-9._-]+|[^:\n]+?):\s?(.*)$/;

/** Classify one doctor run: pass | fail | fatal | recoverable | crash | timeout. */
export function parseDoctor({ stdout = "", stderr = "", exit = null, signal = null, timedOut = false, spawnError = null } = {}) {
  const text = `${stdout}\n${stderr}`.replace(/\r\n/g, "\n");
  const lines = text.split("\n");
  const rows = [];
  for (const line of lines) {
    const m = ROW.exec(line);
    if (!m) continue;
    const level = m[1].trim().toLowerCase() === "ok" ? "ok" : m[1].trim();
    const name = m[2].trim();
    rows.push({ level, name, detail: m[3].trim(), gate: name.startsWith("gate:") });
  }
  const fatalLines = lines.filter((l) => /^right-release: /.test(l) && !NOT_FATAL.test(l));
  const fatalLine = fatalLines.length ? fatalLines[fatalLines.length - 1] : null;
  const failRows = rows.filter((r) => r.level === "FAIL");
  const failNames = [...new Set(failRows.filter((r) => !r.gate).map((r) => r.name))].sort();
  const gateFailNames = [...new Set(failRows.filter((r) => r.gate).map((r) => r.name))].sort();
  const cache = rows.find((r) => r.name === "compiler-cache");
  const base = { exit, signal, rows, failNames, gateFailNames, fatal: null, compilerCache: Boolean(cache && cache.level === "ok" && /^broker-managed/.test(cache.detail)) };
  if (timedOut) return { ...base, status: "timeout" };
  if (spawnError) return { ...base, status: "crash", detail: spawnError };
  if (signal || (exit !== 0 && exit !== 1)) return { ...base, status: "crash", detail: signal ? `signal ${signal}` : `exit ${exit}` };
  if (exit === 0) return { ...base, status: failRows.length ? "fail" : "pass" };
  if (failRows.length) return { ...base, status: "fail" };
  if (fatalLine) {
    const message = fatalLine.slice("right-release: ".length);
    const signature = normalize(message).trim();
    if (/\bRECOVERABLE\b/.test(message)) return { ...base, status: "recoverable", fatal: { signature, recoverable: true } };
    return { ...base, status: "fatal", fatal: { signature, recoverable: false } };
  }
  return { ...base, status: "crash", detail: "exit 1 with neither a [FAIL] row nor a right-release: message" };
}

/** Make two sides' output comparable: no version header, no registry summary, no informational newer-version lines, no paths. */
export function normalize(text) {
  return String(text)
    .replace(/\r\n/g, "\n")
    .split("\n")
    .filter((l) => !/^right-release \d+\.\d+\.\d+/.test(l) && !/^rightkit-registry:/.test(l) && !/newer RightKit version \(informational\)/.test(l))
    .map((l) => l.replace(/(?<![\w./:\\-])(?:[A-Za-z]:[\\/]|\/(?=[\w.~@-]))[^\s'"`,;)\]]*/g, "<path>").replace(/\s+$/, ""))
    .join("\n")
    .trim();
}

// ---------------------------------------------------------------------------------------------------------------------
// one cell, and the whole leg
// ---------------------------------------------------------------------------------------------------------------------

/** Run one cell: tree digest, doctor, tree digest, classify. */
export async function runCell({ side, sidePrep, app, platform, exportDir, node, tmpRoot, timeoutMs = DEFAULT_CELL_MS, gates }) {
  const appRoot = path.join(exportDir, path.dirname(app.configPath));
  const configRel = path.basename(app.configPath);
  if (gates && side === "candidate" && !sidePrep.supportsGates && gatesRequired(sidePrep.version)) {
    throw new Error(`candidate @rightkit/release ${sidePrep.version} cannot run doctor --gates (neither its --help nor its release.mjs offers it); from ${GATES_REQUIRED_FROM} that is required`);
  }
  const gatesRun = Boolean(gates && side === "candidate" && sidePrep.supportsGates);
  const noNodeModules = !fs.existsSync(path.join(exportDir, "node_modules")) && !fs.existsSync(path.join(appRoot, "node_modules"));
  const before = treeDigest(exportDir);
  const run = await runDoctor({ node, bin: sidePrep.bin, appRoot, configRel, platform, gates: gatesRun, sideDir: sidePrep.sideDir, tmpDir: path.join(tmpRoot, `${side}-${app.repoDir}-${platform}`), timeoutMs });
  const after = treeDigest(exportDir);
  const parsed = parseDoctor(run);
  const delta = [...before.lines.filter((l) => !after.lines.includes(l)).map((l) => `-${l}`), ...after.lines.filter((l) => !before.lines.includes(l)).map((l) => `+${l}`)];
  const combined = normalize(`${run.stdout}\n${run.stderr}`);
  return {
    id: app.id, app: app.id, platform, side, repoDir: app.repoDir, host: process.platform,
    status: parsed.status, exit: parsed.exit, signal: parsed.signal, detail: parsed.detail ?? null, timedOut: run.timedOut, seconds: run.seconds,
    failNames: parsed.failNames, gateFailNames: parsed.gateFailNames, fatal: parsed.fatal, compilerCache: parsed.compilerCache,
    rows: parsed.rows.map((r) => ({ level: r.level, name: r.name, gate: r.gate })), gatesRun, noNodeModules, args: run.args.slice(1),
    treeChanged: before.digest !== after.digest, treeDelta: delta.slice(0, 50), treeBefore: before.digest, treeAfter: after.digest,
    normalizedSha256: sha256(combined), normalized: combined.slice(0, 20_000),
  };
}

/** Run `fn` over `items` with at most `limit` in flight. */
export async function pool(items, limit, fn) {
  const queue = [...items];
  const results = [];
  await Promise.all(Array.from({ length: Math.max(1, Math.min(limit, queue.length)) }, async () => {
    while (queue.length) results.push(await fn(queue.shift()));
  }));
  return results;
}

/** The leg: every cell of this host, both sides, bounded in time. */
export async function runLeg(manifest, { manifestDir, now = Date.now, host = process.platform, node = process.execPath } = {}) {
  const hostCfg = manifest.hosts?.[host];
  if (!hostCfg) throw new Error(`the manifest has no ${host} host`);
  const started = now();
  const totalMs = (manifest.limits?.totalMinutes ?? 20) * 60_000;
  const cellMs = (manifest.limits?.cellMinutes ?? 5) * 60_000;
  const root = path.join(hostCfg.scratchRoot, "rk-canary");
  const tmpRoot = path.join(root, "tmp");
  fs.rmSync(tmpRoot, { recursive: true, force: true });
  const resolveFile = (f) => (path.isAbsolute(f) ? f : path.join(manifestDir, f));
  const sides = {};
  const sideErrors = {};
  for (const side of ["baseline", "candidate"]) {
    try {
      sides[side] = prepareSide({
        sideDir: path.join(root, side),
        release: { file: resolveFile(manifest.artifacts[side].file), sha256: manifest.artifacts[side].sha256, version: manifest.artifacts[side].version },
        dependency: { file: resolveFile(manifest.artifacts.dependency.file), sha256: manifest.artifacts.dependency.sha256 },
        node,
      });
    } catch (error) { sideErrors[side] = String(error.message ?? error); }
  }
  const cells = [];
  const units = [];
  for (const app of manifest.apps) {
    const platforms = app.platforms.filter((p) => HOST_FOR_PLATFORM[p] === host);
    if (!platforms.length) continue;
    for (const side of ["baseline", "candidate"]) units.push({ app, side, platforms });
  }
  // One work unit per (side, repository): cells that share a repository run one after another in one export, so a tree
  // write is attributed to the cell that made it. Distinct repositories run in parallel.
  const grouped = new Map();
  for (const u of units) {
    const key = `${u.side}:${u.app.repoDir}`;
    if (!grouped.has(key)) grouped.set(key, { side: u.side, repoDir: u.app.repoDir, items: [] });
    grouped.get(key).items.push(u);
  }
  await pool([...grouped.values()], hostCfg.parallel ?? 4, async (group) => {
    for (const { app, side, platforms } of group.items) {
      for (const platform of platforms) {
        const fixed = { id: app.id, app: app.id, platform, side, repoDir: app.repoDir, host };
        if (sideErrors[side]) { cells.push({ ...fixed, status: "infra", detail: `side not prepared: ${sideErrors[side]}`, failNames: [], gateFailNames: [], fatal: null, treeChanged: false }); continue; }
        const meta = manifest.repos?.[app.repoDir];
        if (!meta || meta.error || !meta.sha) { cells.push({ ...fixed, status: "infra", detail: `no pinned app commit: ${meta?.error ?? "unresolved"}`, failNames: [], gateFailNames: [], fatal: null, treeChanged: false }); continue; }
        let result = null;
        for (let attempt = 1; attempt <= 2; attempt += 1) {
          if (now() - started > totalMs) { result = { ...fixed, status: "budget", detail: `leg budget of ${manifest.limits?.totalMinutes ?? 20} minutes spent`, failNames: [], gateFailNames: [], fatal: null, treeChanged: false }; break; }
          try {
            const exportDir = path.join(root, side, app.repoDir);
            // The export is rebuilt for every cell: a previous cell can never leave state for the next one.
            prepareExport({ live: path.join(hostCfg.workspaceRoot, app.repoDir), origin: meta.origin, sha: meta.sha, dest: exportDir });
            result = await runCell({ side, sidePrep: sides[side], app, platform, exportDir, node, tmpRoot, timeoutMs: cellMs, gates: manifest.gates !== false });
          } catch (error) {
            result = { ...fixed, status: "infra", detail: String(error.message ?? error).slice(0, 500), failNames: [], gateFailNames: [], fatal: null, treeChanged: false };
          }
          result.attempts = attempt;
          if (!INFRA_STATUSES.includes(result.status)) break;
        }
        cells.push(result);
      }
    }
  });
  fs.rmSync(tmpRoot, { recursive: true, force: true });
  cells.sort((a, b) => `${a.id}:${a.platform}:${a.side}`.localeCompare(`${b.id}:${b.platform}:${b.side}`));
  return {
    schema: LEG_SCHEMA, host, node: process.version, sha: manifest.sha, version: manifest.version,
    manifestSha256: manifest.manifestSha256 ?? null, legSha256: sha256(fs.readFileSync(fileURLToPath(import.meta.url))),
    startedAt: new Date(started).toISOString(), finishedAt: new Date(now()).toISOString(), seconds: Math.round((now() - started) / 1000),
    sides: Object.fromEntries(Object.entries(sides).map(([k, v]) => [k, { version: v.version, supportsGates: v.supportsGates, gatesVia: v.gatesVia }])), sideErrors, cells,
  };
}

export async function main(argv) {
  let manifestPath = null, out = null;
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === "--manifest") manifestPath = argv[++i];
    else if (argv[i] === "--out") out = argv[++i];
    else { process.stderr.write(`rk-release-canary-leg: unknown argument ${argv[i]}\n`); return 2; }
  }
  if (!manifestPath) { process.stderr.write("rk-release-canary-leg: --manifest is required\n"); return 2; }
  const manifestFile = path.resolve(manifestPath);
  const raw = fs.readFileSync(manifestFile);
  const manifest = JSON.parse(raw.toString("utf8"));
  manifest.manifestSha256 = sha256(raw);
  const receipt = await runLeg(manifest, { manifestDir: path.dirname(manifestFile) });
  const file = path.resolve(out ?? path.join(path.dirname(manifestFile), `leg-${process.platform}.json`));
  const temp = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(temp, JSON.stringify(receipt, null, 2));
  fs.renameSync(temp, file);
  process.stdout.write(`rk-release-canary-leg: ${receipt.cells.length} cell(s) on ${receipt.host}, receipt ${file}\n`);
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).then((code) => process.exit(code), (error) => { process.stderr.write(`rk-release-canary-leg: ${error.message}\n`); process.exit(2); });
}
