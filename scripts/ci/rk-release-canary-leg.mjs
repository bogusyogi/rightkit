#!/usr/bin/env node
// rk-release-canary-leg: one host's half of the @rightkit/release pipeline canary. Standalone, Node built-ins only: the
// coordinator (rk-release-canary.mjs) copies this single file, two tarballs, smol-toml and a manifest to each host and
// runs it there, so it can import nothing from the workspace it judges.
//
//   node rk-release-canary-leg.mjs --manifest <path> [--out <leg receipt path>]
//
// For every app cell the manifest assigns to this host it runs `right-release doctor` twice, IN PLACE on the app's live
// checkout as it stands (Adrian, 2026-10-27: "why would we clone each app? instead of checking it where it stands?"):
// once with the registry baseline (latest published @rightkit/release) and once with the candidate tarball. Both sides of
// one repository run one after another, never at the same time. It reports facts (status, [FAIL] rows, fatal signature,
// the checkout's HEAD and dirt, whether the doctor changed the tree); the coordinator judges them.
// Hard rules: no clone, no checkout, no fetch, no install: the live checkout is read, never modified; `--admission` is
// never passed; the doctor must leave the tree exactly as it found it (git status and the content hashes of tolerated
// CRLF blobs are compared around the run; see classifyStatus and treeDigest).
import { spawn, spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import zlib from "node:zlib";
import { fileURLToPath, pathToFileURL } from "node:url";

export const LEG_SCHEMA = "rk-release-canary-leg.v1";
export const HOST_FOR_PLATFORM = Object.freeze({ mac: "darwin", ios: "darwin", win: "win32" });
export const DEFAULT_CELL_MS = 5 * 60_000;
export const DEFAULT_TOTAL_MS = 20 * 60_000;
const OUTPUT_CAP = 4 * 1024 * 1024;
export const INFRA_STATUSES = Object.freeze(["timeout", "recoverable", "infra", "budget"]);

/** rmSync that clears the read-only bit first on Windows: git objects are read-only there and rmSync answers EPERM (#1219). */
export function removeTree(dir) {
  if (!fs.existsSync(dir)) return;
  {
    // Clear read-only bits on every platform: git objects are read-only, and a non-writable directory refuses unlink anywhere.
    const walk = (p) => {
      let st; try { st = fs.lstatSync(p); } catch { return; }
      try { fs.chmodSync(p, st.isDirectory() ? 0o777 : 0o666); } catch { /* best effort */ }
      if (st.isDirectory()) for (const e of fs.readdirSync(p)) walk(path.join(p, e));
    };
    walk(dir);
  }
  fs.rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
}
/** Broker admission refusals: the build broker's answer that this checkout may not run Cargo. Not a candidate defect. */
export const ADMISSION_CODES = Object.freeze(["RK_NOT_PRIMARY_CHECKOUT", "RK_BROKER_UNAVAILABLE", "RK_REPOSITORY_BUSY"]);
const ADMISSION = new RegExp(ADMISSION_CODES.join("|"));
export function admissionCodeOf(text) {
  const m = ADMISSION.exec(String(text ?? ""));
  return m ? m[0] : null;
}
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
  removeTree(modules);
  const releaseDir = path.join(modules, "@rightkit", "release");
  extractTarball(release.file, releaseDir);
  extractTarball(dependency.file, path.join(modules, "smol-toml"));
  const manifest = JSON.parse(fs.readFileSync(path.join(releaseDir, "package.json"), "utf8"));
  if (manifest.name !== "@rightkit/release") throw new Error(`extracted package is ${manifest.name}, not @rightkit/release`);
  if (release.version && manifest.version !== release.version) throw new Error(`extracted @rightkit/release is ${manifest.version}, the manifest says ${release.version}`);
  const bin = path.join(releaseDir, "cli", "right-release.mjs");
  if (!fs.existsSync(bin)) throw new Error("extracted @rightkit/release has no cli/right-release.mjs");
  return { sideDir, releaseDir, bin, version: manifest.version, ...detectGates({ releaseDir, bin, node }), ...detectRoute({ releaseDir, node }) };
}

const versionAtLeast = (version, from) => {
  const v = /^(\d+)\.(\d+)\.(\d+)/.exec(String(version));
  const need = from.split(".").map(Number);
  if (!v) return false;
  for (let i = 0; i < 3; i += 1) if (Number(v[i + 1]) !== need[i]) return Number(v[i + 1]) > need[i];
  return true;
};
/** Candidates from this version on must be able to run `doctor --gates`; one that cannot is a broken candidate, not an old one. */
export const GATES_REQUIRED_FROM = "0.6.4";
export const gatesRequired = (version) => versionAtLeast(version, GATES_REQUIRED_FROM);
/** Candidates from this version on must answer `rightkit-gate --to <stage> --route` (rightkit#1297); one that cannot is broken. */
export const ROUTE_REQUIRED_FROM = "0.6.14";
export const routeRequired = (version) => versionAtLeast(version, ROUTE_REQUIRED_FROM);
/** The stages every cell probes, in order; a mobile build has no installed-QA lane, so ios has no qualify. */
export const ROUTE_STAGES = Object.freeze(["dev", "test", "compile", "package", "qualify"]);
export const routeStagesFor = (platform) => ROUTE_STAGES.filter((s) => !(s === "qualify" && platform === "ios"));
export const ROUTE_SCHEMA = "rightkit-gate.route/1";
export const ROUTE_TIMEOUT_MS = 60_000;

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

/** Does this package's gate answer `--route`? Asked of the gate itself (`rightkit-gate --help` lists the flag in its usage), the way
 * detectGates asks the CLI. A gate that does not list it is never run with it: an older gate would treat the flag as an ordinary
 * `--to` run and build. */
export function detectRoute({ releaseDir, node = process.execPath }) {
  const gateBin = path.join(releaseDir, "bin", "rightkit-gate.mjs");
  if (!fs.existsSync(gateBin)) return { supportsRoute: false, gateBin: null };
  const help = spawnSync(node, [gateBin, "--help"], { cwd: releaseDir, encoding: "utf8", timeout: 30_000, windowsHide: true, env: { ...process.env, RIGHT_RELEASE_NO_UPDATE_CHECK: "1" } });
  return { supportsRoute: help.status === 0 && /--route\b/.test(`${help.stdout}\n${help.stderr}`), gateBin };
}

// ---------------------------------------------------------------------------------------------------------------------
// scratch exports of the live app repositories
// ---------------------------------------------------------------------------------------------------------------------

const GIT_ENV = { ...process.env, GIT_TERMINAL_PROMPT: "0", GIT_OPTIONAL_LOCKS: "0", GCM_INTERACTIVE: "never" };
export const GIT_TIMEOUT_MS = 120_000;
function git(cwd, args, { allowFail = false, timeout = GIT_TIMEOUT_MS, env = GIT_ENV, input } = {}) {
  const r = spawnSync("git", args, { cwd, encoding: "utf8", env, input, maxBuffer: 64 * 1024 * 1024, windowsHide: true, timeout, killSignal: "SIGKILL" });
  if (r.status !== 0 && !allowFail) {
    const error = new Error(`git ${args.join(" ")}: ${r.error?.code === "ETIMEDOUT" ? `timed out after ${timeout} ms` : (r.stderr || r.error?.message || "").trim().slice(0, 400)}`);
    error.infra = true;
    throw error;
  }
  return { status: r.status, stdout: r.stdout ?? "", stderr: r.stderr ?? "" };
}

/** The live checkout as it stands: its HEAD, whether it is the pinned commit, its status lines (dirt is reported, never
 * refused) and the tolerated CRLF blobs whose content hashes are compared around the doctor. Nothing is written. */
export function prepareLive({ live, sha = null }) {
  if (!fs.existsSync(path.join(live, ".git"))) throw Object.assign(new Error(`${live} is not a git checkout on this host`), { infra: true });
  const head = git(live, ["rev-parse", "HEAD"]).stdout.trim();
  const status = contentStatus(live).map((e) => e.line);
  const { tolerated, dirt } = classifyStatus(live, status);
  return { live, head, pinned: sha ?? null, headMatchesPin: sha ? head === sha : null, dirty: dirt.length, unnormalizedBlobs: tolerated };
}

/** Where the throwaway index lives: workspace rule, run output only under scratch, never the internal disk. RIGHTKIT_SCRATCH_ROOT,
 * else /Volumes/D/scratch (macOS) or D:\scratch (Windows); other hosts (Linux CI) have no such rule and use the OS temp dir. */
export function scratchRoot(env = process.env, platform = process.platform) {
  if (env.RIGHTKIT_SCRATCH_ROOT) return env.RIGHTKIT_SCRATCH_ROOT;
  if (platform === "darwin") return "/Volumes/D/scratch";
  if (platform === "win32") return "D:\\scratch";
  return os.tmpdir();
}

/** `git status` by CONTENT, read-only. A plain `git status` trusts the index's cached stat (size, mtime, inode), so a file
 * whose cleaned content differs from its index blob but whose stat looks unchanged (an `ident`/eol/filter attribute added
 * after the commit, a same-size rewrite inside the mtime granularity) reads clean. Here the staged entries (`git ls-files
 * --stage -z`: mode, blob id, stage, path) are loaded into a THROWAWAY index outside the checkout, with no stat data at all;
 * git then has nothing to trust and must re-read and re-clean every tracked file (the same attribute-aware conversion `git
 * add` applies) and compare it with the blob id. One ls-files, one update-index, one status: the cost is one read of the
 * tracked tree, no per-file process. `git update-index --refresh` is not used: it rewrites the real index. `hash-object
 * --stdin-paths` takes no per-path `--path`, so a per-file hash-object loop would be one process per file.
 * Limits: skip-worktree and intent-to-add flags are not carried into the throwaway index.
 * Returns [{ xy, path, from, line }] from `status --porcelain -z`. */
export function contentStatus(repoRoot, { ignored = false, untrackedAll = false } = {}) {
  const root = scratchRoot();
  fs.mkdirSync(root, { recursive: true });
  const scratch = fs.mkdtempSync(path.join(root, "rk-leg-idx-"));
  try {
    const env = { ...GIT_ENV, GIT_INDEX_FILE: path.join(scratch, "index") };
    const staged = git(repoRoot, ["ls-files", "--stage", "-z"]).stdout;
    if (staged) git(repoRoot, ["update-index", "-z", "--index-info"], { env, input: staged });
    const args = ["-c", "core.fsmonitor=false", "status", "--porcelain", "-z", untrackedAll ? "--untracked-files=all" : "--untracked-files=normal"];
    if (ignored) args.push("--ignored=matching");
    const parts = git(repoRoot, args, { env }).stdout.split("\0");
    const entries = [];
    for (let i = 0; i < parts.length; i += 1) {
      const raw = parts[i];
      if (!raw) continue;
      const xy = raw.slice(0, 2), p = raw.slice(3);
      if (xy[0] === "R" || xy[0] === "C" || xy[1] === "R" || xy[1] === "C") { const from = parts[i + 1]; i += 1; entries.push({ xy, path: p, from, line: `${xy} ${from} -> ${p}` }); } else entries.push({ xy, path: p, from: null, line: `${xy} ${p}` });
    }
    return entries;
  } finally { removeTree(scratch); }
}

/** Split a checkout's status lines. Tolerated: a modified tracked file whose committed blob is CRLF or mixed and whose
 * attributes include text (or eol=): git's line-ending conversion disagrees with the blob, so checkout alone shows it.
 * Every other line is dirt. */
export function classifyStatus(repoRoot, lines) {
  const modified = lines.filter((l) => l.startsWith(" M ")).map((l) => l.slice(3)).filter((p) => p && !p.startsWith('"'));
  const tolerated = new Set();
  if (modified.length) {
    for (const line of git(repoRoot, ["ls-files", "--eol", "--", ...modified]).stdout.split("\n")) {
      const m = /^i\/(\S*)\s+w\/\S*\s+attr\/([^\t]*)\t(.*)$/.exec(line);
      if (!m || !modified.includes(m[3])) continue;
      const textual = m[2].trim().split(/\s+/).some((a) => a === "text" || a.startsWith("text=") || a.startsWith("eol="));
      if (["crlf", "mixed"].includes(m[1]) && textual) tolerated.add(m[3]);
    }
  }
  return { tolerated: [...tolerated].sort(), dirt: lines.filter((l) => !(l.startsWith(" M ") && tolerated.has(l.slice(3)))) };
}

/** Raw bytes of one path as sha256: a file by content plus its execute bit, a symlink by its target, anything else by kind. */
/** sha256 of a file read in fixed 1 MiB chunks (bounded memory whatever the file's size). */
function streamSha256(file) {
  const hash = crypto.createHash("sha256"); const buf = Buffer.allocUnsafe(1 << 20);
  const fd = fs.openSync(file, "r");
  try { let n; while ((n = fs.readSync(fd, buf, 0, buf.length, null)) > 0) hash.update(buf.subarray(0, n)); } finally { fs.closeSync(fd); }
  return hash.digest("hex");
}

function rawFingerprint(repoRoot, rel) {
  const full = path.join(repoRoot, rel);
  try { // ENOENT/ENOTDIR is "missing"; any other error fails closed (never reads as clean or missing)
    const st = fs.lstatSync(full);
    if (st.isSymbolicLink()) return `link ${sha256(fs.readlinkSync(full))}`;
    if (st.isFile()) return `file ${streamSha256(full)}${st.mode & 0o111 ? " +x" : " -x"}`;
    return st.isDirectory() ? "dir" : "other";
  } catch (error) {
    if (error?.code === "ENOENT" || error?.code === "ENOTDIR") return "missing";
    throw Object.assign(new Error(`cannot fingerprint ${full}: ${error?.code ?? ""} ${error?.message ?? error}`), { infra: true });
  }
}

/** The content-based status (see contentStatus; ignored output included, collapsed) plus the raw content hash of EVERY
 * non-ignored path it lists (modified, added, untracked, renamed; untracked directories expanded to files) and of each
 * `extra` path (a missing one hashes as "missing"), sorted, and its digest. `lines` is the status text and the `extra`
 * rows; `contentLines` are the per-path content hashes. The digest covers both, so any byte change to an already-dirty
 * file changes it though its status line does not. Hashing with --no-filters makes a doctor write to a tolerated file visible. */
export function treeDigest(repoRoot, { extra = [] } = {}) {
  const entries = contentStatus(repoRoot, { ignored: true, untrackedAll: true });
  const lines = entries.map((e) => e.line);
  const contentLines = entries.filter((e) => e.xy !== "!!").map((e) => `content ${e.path} ${rawFingerprint(repoRoot, e.path)}`);
  // The raw bytes of EVERY tracked file too: a change git's conversion hides (LF to CRLF under text normalization, a filter that
  // cleans it back) leaves status empty but changes what a doctor reads, so it must change the digest.
  const tracked = git(repoRoot, ["ls-files", "-z"]).stdout.split("\0").filter(Boolean);
  const reported = new Set(entries.map((e) => e.path));
  for (const file of tracked) if (!reported.has(file)) contentLines.push(`raw ${file} ${rawFingerprint(repoRoot, file)}`);
  for (const file of extra) {
    const h = git(repoRoot, ["hash-object", "--no-filters", file], { allowFail: true });
    let exec = "";
    try { exec = (fs.statSync(path.join(repoRoot, file)).mode & 0o111) ? " +x" : " -x"; } catch { /* missing: the hash says so */ }
    lines.push(`hash ${file} ${h.status === 0 ? h.stdout.trim() : "missing"}${exec}`);
  }
  lines.sort();
  contentLines.sort();
  // Ignored paths (build output, node_modules, an ignored directory collapsed to one line) are NOT fingerprinted: a doctor may
  // write inside them unseen. They are named here so evidence built on this digest states that coverage is incomplete for them.
  const ignoredNotCovered = entries.filter((e) => e.xy === "!!").map((e) => e.path).sort();
  return { digest: sha256([...lines, ...contentLines].join("\n")), lines, contentLines, ignoredNotCovered };
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

/** Run `rightkit-gate --to <stage> --route` from the app root. It throws when this gate does not list `--route` (re-asked here, so
 * no caller can start a real build by probing an old gate). */
export async function runRouteProbe({ node = process.execPath, gateBin, appRoot, platform, stage, env = process.env, timeoutMs = ROUTE_TIMEOUT_MS }) {
  if (!gateBin || !fs.existsSync(gateBin)) throw new Error("this side has no rightkit-gate to probe");
  const help = spawnSync(node, [gateBin, "--help"], { cwd: appRoot, encoding: "utf8", timeout: 30_000, windowsHide: true, env });
  if (help.status !== 0 || !/--route\b/.test(`${help.stdout}\n${help.stderr}`)) throw new Error("this side's rightkit-gate does not support --route; it is never run with it");
  const args = [gateBin, "--to", stage, "--route", "--platform", platform];
  return await new Promise((resolve) => {
    const started = Date.now();
    let stdout = "", stderr = "", timedOut = false, settled = false;
    const finish = (extra) => { if (settled) return; settled = true; clearTimeout(timer); resolve({ stdout, stderr, timedOut, seconds: Math.round((Date.now() - started) / 1000), args, ...extra }); };
    const child = spawn(node, args, { cwd: appRoot, env, stdio: ["ignore", "pipe", "pipe"], detached: process.platform !== "win32", windowsHide: true });
    child.stdout.on("data", (c) => { if (stdout.length < OUTPUT_CAP) stdout += c.toString("utf8"); });
    child.stderr.on("data", (c) => { if (stderr.length < OUTPUT_CAP) stderr += c.toString("utf8"); });
    const timer = setTimeout(() => { timedOut = true; killTree(child); }, timeoutMs);
    child.on("error", (error) => finish({ exit: null, signal: null, spawnError: String(error.message ?? error) }));
    child.on("close", (code, signal) => finish({ exit: code, signal }));
  });
}

/** One probe's answer: { status: "ok", route } for exit 0 and a schema line for this stage, { status: "timeout", detail } when it timed out, else { status: "crash", detail }. Only a
 * line that is exactly a `rightkit-gate.route/1` object is accepted. */
export function parseRoute({ stdout = "", stderr = "", exit = null, signal = null, timedOut = false, spawnError = null, stage = null } = {}) {
  if (timedOut) return { status: "timeout", detail: "route probe timed out" };
  if (spawnError) return { status: "crash", detail: spawnError };
  if (signal || exit !== 0) return { status: "crash", detail: signal ? `signal ${signal}` : `exit ${exit}: ${String(stderr).trim().split(/\r?\n/).slice(-1)[0] ?? ""}`.trim() };
  for (const line of String(stdout).split(/\r?\n/)) {
    let v; try { v = JSON.parse(line); } catch { continue; }
    if (v?.schema !== ROUTE_SCHEMA) continue;
    const refusalOk = v.refusal === null || (v.refusal && typeof v.refusal.code === "string" && typeof v.refusal.message === "string");
    if (typeof v.route !== "string" || typeof v.executor !== "string" || !refusalOk || (stage && v.to !== stage)) return { status: "crash", detail: "malformed route line" };
    return { status: "ok", route: { to: v.to, platform: v.platform ?? null, host: v.host ?? null, gate: v.gate ?? null, commit: v.commit ?? null, executor: v.executor, carrier: v.carrier ?? null, route: v.route, stages: v.stages ?? [], refusal: v.refusal, checkoutClean: v.checkoutClean ?? null } };
  }
  return { status: "crash", detail: "no rightkit-gate.route/1 line in the output" };
}

// A fatal line is `right-release: <message>` or, from a command that refuses before it starts (the app check from 0.6.5,
// `right-release doctor: the app check found problems in <repo>; nothing was started…`), `right-release <command>: <message>`.
const FATAL_LINE = /^right-release(?: [a-z][a-z0-9-]*)?: (.*)$/;
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
  const fatalLines = lines.filter((l) => FATAL_LINE.test(l) && !NOT_FATAL.test(l));
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
    const message = FATAL_LINE.exec(fatalLine)[1];
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
export async function runCell({ side, sidePrep, app, platform, exportDir, node, tmpRoot, timeoutMs = DEFAULT_CELL_MS, gates, unnormalizedBlobs = [], checkout = null }) {
  const appRoot = path.join(exportDir, path.dirname(app.configPath));
  const configRel = path.basename(app.configPath);
  if (gates && side === "candidate" && !sidePrep.supportsGates && gatesRequired(sidePrep.version)) {
    throw new Error(`candidate @rightkit/release ${sidePrep.version} cannot run doctor --gates (neither its --help nor its release.mjs offers it); from ${GATES_REQUIRED_FROM} that is required`);
  }
  if (side === "candidate" && !sidePrep.supportsRoute && routeRequired(sidePrep.version)) {
    throw new Error(`candidate @rightkit/release ${sidePrep.version} cannot run rightkit-gate --route (its gate's --help does not list it); from ${ROUTE_REQUIRED_FROM} that is required`);
  }
  const routeSupported = Boolean(sidePrep.supportsRoute);
  const gatesRun = Boolean(gates && side === "candidate" && sidePrep.supportsGates);
  const noNodeModules = !fs.existsSync(path.join(exportDir, "node_modules")) && !fs.existsSync(path.join(appRoot, "node_modules"));
  const before = treeDigest(exportDir, { extra: unnormalizedBlobs });
  const run = await runDoctor({ node, bin: sidePrep.bin, appRoot, configRel, platform, gates: gatesRun, sideDir: sidePrep.sideDir, tmpDir: path.join(tmpRoot, `${side}-${app.repoDir}-${platform}`), timeoutMs });
  // The route probes run in the same live checkout, so the tree digest below covers them: a probe that wrote anything is a tree write.
  const routes = [];
  if (routeSupported) {
    const env = { ...process.env, RIGHTKIT_WORKSPACE_ROOT: sidePrep.sideDir, TMPDIR: path.join(tmpRoot, `${side}-${app.repoDir}-${platform}`), TEMP: path.join(tmpRoot, `${side}-${app.repoDir}-${platform}`), TMP: path.join(tmpRoot, `${side}-${app.repoDir}-${platform}`) };
    for (const stage of routeStagesFor(platform)) {
      try {
        const probe = await runRouteProbe({ node, gateBin: sidePrep.gateBin, appRoot, platform, stage, env, timeoutMs: ROUTE_TIMEOUT_MS });
        routes.push({ stage, seconds: probe.seconds, ...parseRoute({ ...probe, stage }) });
      } catch (error) { routes.push({ stage, status: "crash", detail: String(error.message ?? error).slice(0, 300) }); }
    }
  }
  const after = treeDigest(exportDir, { extra: unnormalizedBlobs });
  const parsed = parseDoctor(run);
  // Only the orchestrator decides whether an admission code is infrastructure: it needs the baseline cell and the probe.
  const admissionCode = ["crash", "fail"].includes(parsed.status) ? admissionCodeOf(`${run.stdout}\n${run.stderr}`) : null;
  const beforeAll = [...before.lines, ...before.contentLines], afterAll = [...after.lines, ...after.contentLines];
  const delta = [...beforeAll.filter((l) => !afterAll.includes(l)).map((l) => `-${l}`), ...afterAll.filter((l) => !beforeAll.includes(l)).map((l) => `+${l}`)];
  const combined = normalize(`${run.stdout}\n${run.stderr}`);
  return {
    id: app.id, app: app.id, platform, side, repoDir: app.repoDir, host: process.platform,
    status: parsed.status, exit: parsed.exit, signal: parsed.signal, detail: parsed.detail ?? null, timedOut: run.timedOut, seconds: run.seconds,
    failNames: parsed.failNames, gateFailNames: parsed.gateFailNames, fatal: parsed.fatal, compilerCache: parsed.compilerCache,
    rows: parsed.rows.map((r) => ({ level: r.level, name: r.name, gate: r.gate })), gatesRun, noNodeModules, args: run.args.slice(1), routeSupported, routes,
    treeChanged: before.digest !== after.digest, treeDelta: delta.slice(0, 50), treeBefore: before.digest, treeAfter: after.digest,
    // Writes inside ignored paths are not detected; the cell says so instead of implying full coverage.
    treeIgnoredNotCovered: [...new Set([...(before.ignoredNotCovered ?? []), ...(after.ignoredNotCovered ?? [])])].sort().slice(0, 50),
    normalizedSha256: sha256(combined), normalized: combined.slice(0, 20_000), admissionCode, unnormalizedBlobs: [...unnormalizedBlobs],
    head: checkout?.head ?? null, pinned: checkout?.pinned ?? null, headMatchesPin: checkout?.headMatchesPin ?? null, dirty: checkout?.dirty ?? null,
  };
}

/** One `cargo metadata --no-deps --offline` through the candidate's own managed-cargo runCargoSync, in a scratch export of
 * the first app that has a Cargo.toml. A refusal with an admission code is returned as admissionRefused for the whole leg.
 * A probe that cannot run says why and refuses nothing. The probe export is removed afterwards. */
export async function admissionProbe({ sidePrep, candidates, probeRoot }) {
  if (!sidePrep) return { ran: false, admissionRefused: null, detail: "candidate side not prepared" };
  const problems = [];
  try {
    for (const c of candidates) {
      // In place, like the cells: `cargo metadata` is read-only and the live checkout is what the broker admits.
      const dir = c.live;
      let manifest = null;
      try {
        if (!fs.existsSync(path.join(dir, ".git"))) throw new Error(`${dir} is not a git checkout on this host`);
        manifest = cargoManifestIn(dir);
      } catch (error) { problems.push(`${c.repoDir}: ${error.message}`); continue; }
      if (!manifest) { problems.push(`${c.repoDir}: no Cargo.toml`); continue; }
      const cargo = await import(pathToFileURL(path.join(sidePrep.releaseDir, "managed-cargo.mjs")).href);
      const r = cargo.runCargoSync(["metadata", "--no-deps", "--offline", "--locked", "--format-version", "1", "--manifest-path", path.join(dir, manifest)], { cwd: dir, encoding: "utf8", timeout: 120_000, windowsHide: true });
      const text = [r?.stdout, r?.stderr, r?.error?.code, r?.error?.message].filter(Boolean).join("\n");
      return { ran: true, app: c.repoDir, manifest, admissionRefused: admissionCodeOf(text) };
    }
    return { ran: false, admissionRefused: null, detail: problems.join("; ") || "no app in this leg has a Cargo.toml" };
  } catch (error) {
    return { ran: false, admissionRefused: null, detail: String(error.message ?? error).slice(0, 300) };
  } finally {
    removeTree(probeRoot);
  }
}

/** The shallowest tracked Cargo.toml of a checkout, or null. */
function cargoManifestIn(dir) {
  const found = git(dir, ["ls-files", "--", "*Cargo.toml"]).stdout.split("\n").filter((f) => path.posix.basename(f) === "Cargo.toml");
  return found.sort((a, b) => a.split("/").length - b.split("/").length || a.localeCompare(b))[0] ?? null;
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
  removeTree(tmpRoot);
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
  // One work unit per repository: every cell of a repository, both sides, runs one after another in the live checkout,
  // so two doctors never share a tree and a tree write is attributed to the cell that made it. Distinct repositories
  // run in parallel, up to hostCfg.parallel.
  const grouped = new Map();
  for (const u of units) {
    const key = u.app.repoDir;
    if (!grouped.has(key)) grouped.set(key, { repoDir: u.app.repoDir, items: [] });
    grouped.get(key).items.push(u);
  }
  // Fail fast: one probe, before any cell, says whether the broker admits this host's Cargo at all.
  const probeApps = [];
  for (const u of units) {
    const meta = manifest.repos?.[u.app.repoDir];
    if (meta?.sha && !meta.error && !probeApps.some((c) => c.repoDir === u.app.repoDir)) probeApps.push({ repoDir: u.app.repoDir, live: path.join(hostCfg.workspaceRoot, u.app.repoDir), origin: meta.origin, sha: meta.sha });
  }
  const probe = await admissionProbe({ sidePrep: sides.candidate, candidates: probeApps, probeRoot: path.join(root, "probe") });
  if (probe.admissionRefused) {
    for (const u of units) for (const platform of u.platforms) cells.push({ id: u.app.id, app: u.app.id, platform, side: u.side, repoDir: u.app.repoDir, host, status: "infra", detail: `broker admission: ${probe.admissionRefused}`, failNames: [], gateFailNames: [], fatal: null, treeChanged: false });
  } else await pool([...grouped.values()], hostCfg.parallel ?? 4, async (group) => {
    for (const { app, side, platforms } of group.items) {
      for (const platform of platforms) {
        const fixed = { id: app.id, app: app.id, platform, side, repoDir: app.repoDir, host };
        if (sideErrors[side]) { cells.push({ ...fixed, status: "infra", detail: `side not prepared: ${sideErrors[side]}`, failNames: [], gateFailNames: [], fatal: null, treeChanged: false }); continue; }
        const meta = manifest.repos?.[app.repoDir] ?? {};
        let result = null;
        for (let attempt = 1; attempt <= 2; attempt += 1) {
          if (now() - started > totalMs) { result = { ...fixed, status: "budget", detail: `leg budget of ${manifest.limits?.totalMinutes ?? 20} minutes spent`, failNames: [], gateFailNames: [], fatal: null, treeChanged: false }; break; }
          try {
            const live = path.join(hostCfg.workspaceRoot, app.repoDir);
            // Read where the checkout stands right before the doctor; a cell before it may have been written to (that is the
            // earlier cell's finding, and it stays in the tree: the live checkout is never cleaned).
            const checkout = prepareLive({ live, sha: meta.sha ?? null });
            result = await runCell({ side, sidePrep: sides[side], app, platform, exportDir: live, node, tmpRoot, timeoutMs: cellMs, gates: manifest.gates !== false, unnormalizedBlobs: checkout.unnormalizedBlobs, checkout });
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
  removeTree(tmpRoot);
  cells.sort((a, b) => `${a.id}:${a.platform}:${a.side}`.localeCompare(`${b.id}:${b.platform}:${b.side}`));
  return {
    schema: LEG_SCHEMA, host, node: process.version, sha: manifest.sha, version: manifest.version,
    manifestSha256: manifest.manifestSha256 ?? null, legSha256: sha256(fs.readFileSync(fileURLToPath(import.meta.url))),
    startedAt: new Date(started).toISOString(), finishedAt: new Date(now()).toISOString(), seconds: Math.round((now() - started) / 1000),
    sides: Object.fromEntries(Object.entries(sides).map(([k, v]) => [k, { version: v.version, supportsGates: v.supportsGates, gatesVia: v.gatesVia, supportsRoute: v.supportsRoute }])), sideErrors, cells,
    admissionRefused: probe.admissionRefused ?? null, admissionProbe: probe,
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
