#!/usr/bin/env node
// `right-release qualify cargo --crates a,b [--hosts win,mac]`: qualify crates LOCALLY, on both native hosts, through the
// RustRight broker, and turn the result into a committed-by-hash candidate (Adrian 2026-10-21/22). Nothing here publishes.
//   1. The checkout must be clean and HEAD must be pushed to origin/main; the other host's checkout must be at the same HEAD.
//   2. Each host (Dell for Windows, Mac for macOS, the Mac reached with `ssh mac "<cmd>"`, the Dell with `win "<cmd>"`) runs
//      `local-qualify.mjs host`: the deterministic scans (class A and B) over the units' scope, then for every crate
//      `right-release publish cargo --crate X --local --qualify-only --bundle <dir>` (tests, clippy, fmt, secret scan, package
//      verify, publication gate) and the archive secret scan of the packed .crate. Both hosts run at once.
//   3. The bundles come back over the same channel (`export` prints one hash-checked JSON document), the hosts' archive hashes
//      must agree, and this driver seals one packed asset (manifest + .crate + upload metadata) and one `local-verify`
//      receipt per host.
//   4. A prerelease `candidate-<id>` gets the three assets; the driver prints the candidate JSON and the approval entry for
//      ci/approved-artifacts.json. It never writes the approval: a reviewer commits that, and publish-reviewed.yml then
//      uploads exactly the pinned bytes (docs/CI.md "Local qualification").
// Hosts that cannot run a unit (Linux-only units) stay on the GitHub qualification fallback (publish.yml).
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { managedRuntimeClient, spawnCollect } from "./managed-cargo.mjs";
import { planScans, runScans } from "./deterministic-scans.mjs";
import { pathDependencyDirs, readCargoPackage } from "./crate-graph.mjs";
import { deriveCrateLayers } from "./local-publish.mjs";
import { RUNTIME_BUDGET_CRATES } from "./runtime-budget-crates.mjs";

export const REPOSITORY = "bogusyogi/rightkit";
export class QualifyError extends Error {
  constructor(message) { super(message); this.name = "QualifyError"; }
}
const refuse = (message) => { throw new QualifyError(message); };

// Host table. `remote` is how the OTHER host reaches this one: the Dell runs `ssh mac "<cmd>"`, the Mac runs `win "<cmd>"`.
export const HOSTS = Object.freeze({
  win: Object.freeze({ key: "win", name: "windows", os: "win32", root: "D:/Claude/rightkit", scratch: "D:/scratch/rk-localq", reach: ["win"] }),
  mac: Object.freeze({ key: "mac", name: "macos", os: "darwin", root: "/Volumes/D/claude/rightkit", scratch: "/Volumes/D/scratch/rk-localq", reach: ["ssh", "mac"] }),
});
const CANONICAL_HOST = "mac"; // CI packs crates on the macOS runner; the same host's bytes are the sealed bytes
const CRATE_NAME = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;
const ID = /^[a-z0-9][a-z0-9-]{0,79}$/;
const SHA40 = /^[0-9a-f]{40}$/;
const SHA64 = /^[0-9a-f]{64}$/;
// Arguments reach a remote shell unquoted, so only plain path/list tokens are allowed; anything else refuses.
const SAFE_TOKEN = /^[A-Za-z0-9._:/\\,=@+-]+$/;
const HOST_TIMEOUT_MS = 4 * 60 * 60 * 1000;
const sha256 = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const tail = (text, lines = 40) => String(text ?? "").split(/\r?\n/).slice(-lines).join("\n");
const list = (text, what) => {
  const names = String(text ?? "").split(",").map((name) => name.trim()).filter(Boolean);
  for (const name of names) if (!CRATE_NAME.test(name)) refuse(`${what}: invalid crate name ${JSON.stringify(name)}`);
  return names;
};

export const localHostKey = (platform = process.platform) => (platform === "win32" ? "win" : platform === "darwin" ? "mac" : refuse(`local qualification runs on the Dell (win32) or the Mac (darwin), not ${platform}`));
export const otherHostKey = (key) => (key === "win" ? "mac" : "win");

// ---- arguments --------------------------------------------------------------------------------

/** `qualify cargo --crates a,b [--hosts win,mac] [--id <id>] [--allow-ignored-units a] [--jobs n] [--concurrency n] [--out <dir>] [--win-root <dir>] [--mac-root <dir>] [--no-release]` */
export function parseQualifyArgs(argv, platform = process.platform) {
  const [kind, ...rest] = argv;
  if (kind === "npm") refuse("npm has no local qualification: publish cargo --local --qualify-only is cargo only; qualify npm packages with publish.yml");
  if (kind !== "cargo") refuse("usage: right-release qualify cargo --crates <a,b> [--hosts win,mac] [--id <id>] [--allow-ignored-units <a,b>] [--jobs <n>] [--concurrency <n>] [--out <dir>] [--no-release]");
  const out = { kind, crates: [], hosts: ["win", "mac"], id: undefined, allowIgnoredUnits: [], jobs: undefined, concurrency: 2, outRoot: undefined, roots: { win: HOSTS.win.root, mac: HOSTS.mac.root }, release: true };
  for (let i = 0; i < rest.length; i += 1) {
    const arg = rest[i];
    if (arg === "--crates" || arg === "--crate") out.crates = list(rest[++i], arg);
    else if (arg === "--hosts") {
      out.hosts = String(rest[++i] ?? "").split(",").map((host) => host.trim()).filter(Boolean);
      if (!out.hosts.length || out.hosts.some((host) => !Object.hasOwn(HOSTS, host)) || new Set(out.hosts).size !== out.hosts.length) refuse("--hosts takes win, mac or win,mac");
    } else if (arg === "--id") out.id = rest[++i];
    else if (arg === "--allow-ignored-units") out.allowIgnoredUnits = list(rest[++i], arg);
    else if (arg === "--jobs") { out.jobs = Number(rest[++i]); if (!Number.isInteger(out.jobs) || out.jobs < 1 || out.jobs > 64) refuse("--jobs must be an integer from 1 to 64"); }
    else if (arg === "--concurrency") { out.concurrency = Number(rest[++i]); if (!Number.isInteger(out.concurrency) || out.concurrency < 1 || out.concurrency > 8) refuse("--concurrency must be an integer from 1 to 8"); }
    else if (arg === "--out") out.outRoot = rest[++i];
    else if (arg === "--win-root") out.roots.win = rest[++i];
    else if (arg === "--mac-root") out.roots.mac = rest[++i];
    else if (arg === "--no-release") out.release = false;
    else refuse(`unknown qualify option: ${arg}`);
  }
  if (!out.crates.length) refuse("qualify cargo requires --crates <a,b>");
  if (new Set(out.crates).size !== out.crates.length) refuse("--crates names a crate more than once");
  for (const name of out.allowIgnoredUnits) if (!out.crates.includes(name)) refuse(`--allow-ignored-units names ${name}, which is not in --crates`);
  if (out.id !== undefined && !ID.test(out.id)) refuse("--id must match [a-z0-9][a-z0-9-]{0,79}");
  for (const [key, value] of Object.entries(out.roots)) if (typeof value !== "string" || !SAFE_TOKEN.test(value)) refuse(`--${key}-root must be a plain path`);
  out.outRoot = out.outRoot ?? HOSTS[localHostKey(platform)].scratch;
  return out;
}

/** `local-qualify.mjs host ...`: what one host is asked to do. Produced by hostArgs, parsed here. */
export function parseHostArgs(argv) {
  const out = { root: undefined, id: undefined, commit: undefined, crates: [], out: undefined, exclude: [], allowIgnoredUnits: [], jobs: undefined, concurrency: 2, canonical: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--root") out.root = argv[++i];
    else if (arg === "--id") out.id = argv[++i];
    else if (arg === "--commit") out.commit = argv[++i];
    else if (arg === "--crates") out.crates = list(argv[++i], arg);
    else if (arg === "--out") out.out = argv[++i];
    else if (arg === "--exclude") out.exclude = list(argv[++i], arg);
    else if (arg === "--allow-ignored-units") out.allowIgnoredUnits = list(argv[++i], arg);
    else if (arg === "--jobs") out.jobs = Number(argv[++i]);
    else if (arg === "--concurrency") out.concurrency = Number(argv[++i]);
    else if (arg === "--canonical") out.canonical = true;
    else refuse(`unknown host option: ${arg}`);
  }
  if (!out.root || !out.out || !ID.test(out.id ?? "") || !SHA40.test(out.commit ?? "") || !out.crates.length) refuse("host needs --root, --out, --id, --commit and --crates");
  if (out.jobs !== undefined && !(Number.isInteger(out.jobs) && out.jobs >= 1 && out.jobs <= 64)) refuse("--jobs must be an integer from 1 to 64");
  if (!Number.isInteger(out.concurrency) || out.concurrency < 1 || out.concurrency > 8) refuse("--concurrency must be an integer from 1 to 8");
  return out;
}

export function hostArgs({ root, id, commit, crates, out, exclude = [], allowIgnoredUnits = [], jobs, concurrency, canonical }) {
  const args = ["host", "--root", root, "--id", id, "--commit", commit, "--crates", crates.join(","), "--out", out, "--concurrency", String(concurrency ?? 2)];
  if (exclude.length) args.push("--exclude", exclude.join(","));
  if (allowIgnoredUnits.length) args.push("--allow-ignored-units", allowIgnoredUnits.join(","));
  if (jobs !== undefined) args.push("--jobs", String(jobs));
  if (canonical) args.push("--canonical");
  return args;
}

/** The one string the other host's `ssh mac` / `win` runs. Every token is checked: nothing is quoted or interpolated. */
export function remoteCommand({ root, args, node = "node" }) {
  const tokens = [node, `${root}/packages/release/local-qualify.mjs`, ...args];
  for (const token of tokens) if (!SAFE_TOKEN.test(token)) refuse(`refusing to send ${JSON.stringify(token)} to a remote shell`);
  return tokens.join(" ");
}

/** Spawn arguments that reach `hostKey` from the host running this process. */
export function reachArgs(fromKey, command) {
  const [bin, ...prefix] = HOSTS[otherHostKey(fromKey)].reach;
  return { command: bin, args: [...prefix, command] };
}

// ---- plan -------------------------------------------------------------------------------------

const defaultGit = (args, cwd) => {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true, maxBuffer: 64 * 1024 * 1024 });
  if (result.error || result.status !== 0) return null;
  return (result.stdout ?? "").trim();
};
const defaultReadJson = (file) => { try { return JSON.parse(fs.readFileSync(file, "utf8")); } catch { return null; } };

/** The checkout this candidate is cut from: clean, HEAD on origin/main. Returns { head, tree }. */
export function checkoutState({ git, root }) {
  const head = git(["rev-parse", "HEAD"], root);
  if (!SHA40.test(head ?? "")) refuse("cannot resolve HEAD");
  const tree = git(["rev-parse", "HEAD^{tree}"], root);
  if (!SHA40.test(tree ?? "")) refuse("cannot resolve the HEAD tree");
  const dirty = git(["status", "--porcelain"], root);
  if (dirty === null) refuse("cannot read git status");
  if (dirty !== "") refuse(`the checkout is not clean; commit or stash first (a qualified candidate is exactly HEAD):\n${dirty}`);
  const remote = String(git(["ls-remote", "origin", "refs/heads/main"], root) ?? "").split(/\s+/)[0];
  if (!SHA40.test(remote)) refuse("cannot read origin/main");
  if (remote !== head && (git(["cat-file", "-e", `${remote}^{commit}`], root) === null || git(["merge-base", "--is-ancestor", head, remote], root) === null)) refuse(`HEAD ${head.slice(0, 12)} is not pushed to origin/main (${remote.slice(0, 12)}); push it first (fetch if origin/main is ahead)`);
  return { head, tree };
}

/**
 * Units, dependency-first, with the allowsets this request implies. A unit gate.config.json restricts away from Windows is a
 * host exclusion; one restricted away from macOS, or one that needs a Linux row, cannot be qualified locally and refuses.
 */
export function planUnits({ root, crates, readJson = defaultReadJson, readPackage = readCargoPackage, dependsOn }) {
  const gateConfig = readJson(path.join(root, "ci", "gate.config.json")) ?? {};
  const assetGated = (readJson(path.join(root, "ci", "asset-gated.json")) ?? {}).crates ?? {};
  const linux = readJson(path.join(root, "ci", "linux-required.json"));
  if (!linux || linux.schema !== 1 || !Array.isArray(linux.crates)) refuse("ci/linux-required.json is missing or malformed; the Linux requirement cannot be derived");
  const members = new Set(crates);
  const deps = dependsOn ?? ((name) => pathDependencyDirs(path.join(root, "crates", name)).map((dir) => path.basename(dir)).filter((dep) => members.has(dep) && dep !== name));
  const info = new Map();
  for (const name of crates) {
    const dir = `crates/${name}`;
    if (!fs.existsSync(path.join(root, dir))) refuse(`unit directory not found: ${dir}`);
    const pkg = readPackage(path.join(root, dir));
    if (pkg.name !== name) refuse(`Cargo.toml [package].name ${JSON.stringify(pkg.name)} differs from --crates entry ${name}`);
    if (typeof pkg.version !== "string" || !/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(pkg.version)) refuse(`${name}: version ${JSON.stringify(pkg.version)} is not a plain x.y.z release version`);
    if (linux.crates.includes(name)) refuse(`${name} requires a Linux receipt, which local qualification cannot produce; qualify it with publish.yml`);
    // The authenticated isolated runtime proof of a continuous owner exists only on the Actions path; admission would hold it hours from now.
    if (RUNTIME_BUDGET_CRATES.includes(name)) refuse(`${name} is a continuous runtime owner whose runtime proof exists only on the Actions path; qualify it with publish.yml`);
    const hosts = gateConfig.rust?.[name]?.os;
    if (hosts && !hosts.includes("macos")) refuse(`${name}: ci/gate.config.json restricts it to ${hosts.join(", ")}; crates are packed on macOS`);
    info.set(name, { name, version: pkg.version, dir, excludedOn: hosts && !hosts.includes("windows") ? ["win"] : [] });
  }
  const order = deriveCrateLayers(crates, deps).flat();
  return {
    units: order.map((name) => info.get(name)), layers: deriveCrateLayers(crates, deps),
    allow: { hostExclusion: order.filter((name) => info.get(name).excludedOn.length), noTest: [], ignored: [], missingAssets: order.filter((name) => Object.hasOwn(assetGated, name)) },
  };
}

export function candidateIdFor({ crates, version, head }) {
  const slug = crates.map((name) => name.replace(/^rightkit-/, "")).join("-").slice(0, 44).replace(/-+$/, "");
  const id = ["local", slug, crates.length === 1 && version ? version.replaceAll(".", "") : null, head.slice(0, 6)].filter(Boolean).join("-");
  if (!ID.test(id)) refuse(`derived candidate id ${id} is invalid; pass --id`);
  return id;
}

// ---- bundles ----------------------------------------------------------------------------------

const walk = (dir) => fs.readdirSync(dir, { withFileTypes: true }).flatMap((entry) => (entry.isDirectory() ? walk(path.join(dir, entry.name)) : [path.join(dir, entry.name)]));
const BUNDLE_MANIFEST = "bundle.manifest.json";
const SAFE_RELATIVE = /^[A-Za-z0-9][A-Za-z0-9._-]*(?:\/[A-Za-z0-9][A-Za-z0-9._-]*)*$/;

/** Hash every file of a host output directory into bundle.manifest.json (written last). */
export function writeBundleManifest(dir) {
  const files = walk(dir).map((file) => path.relative(dir, file).split(path.sep).join("/")).filter((rel) => rel !== BUNDLE_MANIFEST).sort()
    .map((rel) => ({ path: rel, sha256: sha256(fs.readFileSync(path.join(dir, rel))) }));
  fs.writeFileSync(path.join(dir, BUNDLE_MANIFEST), `${JSON.stringify({ schema: 1, files }, null, 2)}\n`);
  return files;
}

/** Re-hash a bundle against its manifest and refuse extra, missing or altered files. Returns the manifest. */
export function verifyBundle(dir) {
  let manifest;
  try { manifest = JSON.parse(fs.readFileSync(path.join(dir, BUNDLE_MANIFEST), "utf8")); } catch { refuse(`${dir} has no readable ${BUNDLE_MANIFEST}`); }
  if (manifest?.schema !== 1 || !Array.isArray(manifest.files)) refuse(`${dir}: unknown bundle manifest`);
  const listed = new Set();
  for (const row of manifest.files) {
    if (!SAFE_RELATIVE.test(row?.path ?? "") || !SHA64.test(row.sha256 ?? "") || listed.has(row.path)) refuse(`${dir}: malformed bundle manifest row`);
    listed.add(row.path);
    let bytes;
    try { bytes = fs.readFileSync(path.join(dir, ...row.path.split("/"))); } catch { refuse(`${dir}: ${row.path} is missing`); }
    if (sha256(bytes) !== row.sha256) refuse(`${dir}: ${row.path} changed after the host wrote it`);
  }
  for (const file of walk(dir)) {
    const rel = path.relative(dir, file).split(path.sep).join("/");
    if (rel !== BUNDLE_MANIFEST && !listed.has(rel)) refuse(`${dir}: unlisted file ${rel}`);
  }
  return manifest;
}

/** `export --dir <d>`: one JSON document holding a verified bundle, so any exec channel can carry it. */
export function exportBundle(dir) {
  const manifest = verifyBundle(dir);
  return JSON.stringify({ schema: 1, files: [...manifest.files.map((row) => ({ path: row.path, sha256: row.sha256, b64: fs.readFileSync(path.join(dir, ...row.path.split("/"))).toString("base64") })), { path: BUNDLE_MANIFEST, sha256: sha256(fs.readFileSync(path.join(dir, BUNDLE_MANIFEST))), b64: fs.readFileSync(path.join(dir, BUNDLE_MANIFEST)).toString("base64") }] });
}

/** Write an exported bundle into a NEW directory, checking every path and hash, then re-verify it as a bundle. */
export function importBundle(text, dest) {
  let doc;
  try { doc = JSON.parse(text); } catch { refuse("the remote export is not JSON"); }
  if (doc?.schema !== 1 || !Array.isArray(doc.files) || !doc.files.length) refuse("the remote export is empty or of an unknown schema");
  if (fs.existsSync(dest)) refuse(`bundle destination already exists: ${dest}`);
  const seen = new Set();
  for (const row of doc.files) {
    if (!SAFE_RELATIVE.test(row?.path ?? "") || seen.has(row.path) || !SHA64.test(row.sha256 ?? "") || typeof row.b64 !== "string") refuse("the remote export holds a malformed file row");
    seen.add(row.path);
    const bytes = Buffer.from(row.b64, "base64");
    if (sha256(bytes) !== row.sha256) refuse(`the remote export altered ${row.path} in transit`);
    const file = path.join(dest, ...row.path.split("/"));
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, bytes, { flag: "wx" });
  }
  verifyBundle(dest);
  return dest;
}

// ---- one host ---------------------------------------------------------------------------------

function brokerIdentity(deps) {
  const client = (deps.managedRuntimeClient ?? managedRuntimeClient)();
  if (!client) refuse("the RustRight client is not installed on this host; qualification runs only through the broker, never plain cargo");
  return { name: "rustright", client: String(client).replaceAll("\\", "/"), sha256: sha256((deps.readFile ?? fs.readFileSync)(client)) };
}

// Cargo only through the broker: `rightkit cargo`, or a `cargo` on PATH that is the managed shim (scripts/local-gate.mjs).
function detectCargoCommand(deps) {
  const which = (command) => {
    const result = (deps.run ?? spawnSync)(process.platform === "win32" ? "where" : "which", [command], { encoding: "utf8", windowsHide: true });
    return result.status === 0 ? String(result.stdout).split(/\r?\n/).map((line) => line.trim()).filter(Boolean) : [];
  };
  if (which("rightkit").length) return ["rightkit", "cargo"];
  const first = which("cargo")[0];
  if (first) { try { if (fs.readFileSync(first, "utf8").includes("cargo-guard.mjs")) return ["cargo"]; } catch { /* not the managed shim */ } }
  return refuse("neither `rightkit` nor the managed cargo shim (cargo-guard.mjs) is first on PATH; plain cargo is never used");
}

const loadFromRoot = (root, file) => import(pathToFileURL(path.join(root, "scripts", "ci", file)).href);

/** The deterministic scans over the units' scope (class A then class B), recorded the way scan-receipt.mjs verifies them. */
async function localScanRecord({ root, commit, crates, jobs, scratch, deps, log }) {
  const modules = await (deps.loadScanModules ?? ((workspace) => loadFromRoot(workspace, "scan-receipt.mjs")))(root);
  const run = deps.run ?? spawnSync;
  const exportRoot = fs.mkdtempSync(path.join(scratch, "export-"));
  try {
    const tarFile = path.join(exportRoot, "head.tar");
    const archived = run("git", ["archive", "--format=tar", "--prefix=rightkit/", "-o", tarFile, commit], { cwd: root, encoding: "utf8", windowsHide: true });
    const extracted = archived.status === 0 ? run("tar", ["-xf", "head.tar"], { cwd: exportRoot, encoding: "utf8", windowsHide: true }) : archived;
    if (extracted.status !== 0) refuse(`cannot export ${commit.slice(0, 12)} for the content scans: ${String(extracted.stderr ?? "").trim()}`);
    const repo = path.join(exportRoot, "rightkit");
    const tracked = String((deps.git ?? defaultGit)(["ls-files", "-z"], root) ?? "").split("\0").filter(Boolean);
    const unitDirs = crates.map((name) => `crates/${name}/`);
    // Workspace-level scans (lock, deny, audit, workflows, YAML) plus every scan of the units; other crates and npm packages stay out of scope.
    const files = tracked.filter((file) => (!file.startsWith("crates/") && !file.startsWith("packages/")) || unitDirs.some((dir) => file.startsWith(dir)));
    const crateFeatures = (deps.readJson ?? defaultReadJson)(path.join(repo, "ci", "gate.config.json"))?.rust ?? {};
    const plan = (deps.planScans ?? planScans)({ changed: files, contentRoot: repo, liveRoot: root, includeTests: false, localCompile: true, cargoCommand: detectCargoCommand(deps), crateFeatures, readBytes: (file) => fs.readFileSync(file) });
    const observedTools = modules.observeToolVersions();
    log(`[local-qualify] scans: ${plan.scans.length} planned over ${files.length} files (class A, then class B through the broker)`);
    const result = await (deps.runScans ?? runScans)(plan, { classes: ["A", "B"], jobs: Math.max(1, Number(jobs ?? 4) || 4), onResult: (row, progress) => log(`[local-qualify] scan [${progress.done}/${progress.total}] ${row.status.padEnd(7)} ${row.id}`) });
    return modules.buildLocalScanRecord({ result, planned: plan.scans, sourceCommit: commit, headCommit: commit, cargoLockSha256: sha256(fs.readFileSync(path.join(root, "Cargo.lock"))), observedTools, units: crates, files: files.length });
  } finally { fs.rmSync(exportRoot, { recursive: true, force: true }); }
}

/**
 * Qualify every unit on THIS host and write a verified bundle into `out`: host-result.json, one directory per unit with the
 * packed .crate and the gate evidence (from `publish cargo --local --qualify-only`), and on the canonical host the upload
 * metadata. Any failure refuses the whole host: a partial bundle is never a candidate.
 */
export async function qualifyHost(options, deps = {}) {
  const log = deps.log ?? ((line) => process.stdout.write(`${line}\n`));
  const { root, id, commit, crates, exclude = [], allowIgnoredUnits = [], jobs, concurrency = 2, canonical = false } = options;
  const out = path.resolve(options.out);
  const platform = deps.platform ?? process.platform;
  const hostKey = localHostKey(platform);
  const git = deps.git ?? defaultGit;
  const head = git(["rev-parse", "HEAD"], root);
  if (head !== commit) refuse(`${hostKey}: HEAD is ${String(head).slice(0, 12)}, not the candidate commit ${commit.slice(0, 12)}; bring this checkout to it first`);
  const tree = git(["rev-parse", "HEAD^{tree}"], root);
  const dirty = git(["status", "--porcelain"], root);
  if (!SHA40.test(tree ?? "") || dirty !== "") refuse(`${hostKey}: the checkout is not clean at ${commit.slice(0, 12)}${dirty ? `:\n${dirty}` : ""}`);
  const broker = brokerIdentity(deps);
  if (fs.existsSync(out)) refuse(`host output already exists: ${out}`);
  fs.mkdirSync(out, { recursive: true });
  const readJson = deps.readJson ?? defaultReadJson;
  const unitInfo = crates.map((name) => { const pkg = (deps.readPackage ?? readCargoPackage)(path.join(root, "crates", name)); return { name, version: pkg.version }; });
  const archiveScan = await (deps.loadArchiveScan ?? ((workspace) => loadFromRoot(workspace, "archive-secret-scan.mjs")))(root);
  const scans = await localScanRecord({ root, commit, crates, jobs, scratch: out, deps: { ...deps, git, readJson }, log });
  if (scans.passed !== true) refuse(`${hostKey}: deterministic scans did not pass:\n  - ${scans.problems.join("\n  - ")}`);
  // Async so a long unit never stops this process draining the other host's output.
  const run = deps.runAsync ?? ((command, args, options) => spawnCollect(command, args, options));
  const cli = path.join(root, "packages", "release", "cli", "right-release.mjs");
  const rows = new Array(unitInfo.length);
  const queue = unitInfo.map((unit, index) => ({ unit, index }));
  const failures = [];
  const worker = async () => {
    while (!failures.length && queue.length) {
      const { unit, index } = queue.shift();
      const file = `${unit.name}-${unit.version}.crate`;
      try {
        if (exclude.includes(unit.name)) { rows[index] = { name: unit.name, version: unit.version, file, sha256: null, bytesVerified: false, status: "allowed-exclusion", executed: 0, ignored: 0, skipped: 0, steps: [] }; continue; }
        const bundle = path.join(out, unit.name);
        const argv = [cli, "publish", "cargo", "--crate", unit.name, "--local", "--qualify-only", "--bundle", bundle, "--workspace", root, ...(allowIgnoredUnits.includes(unit.name) ? ["--allow-ignored"] : []), ...(jobs === undefined ? [] : ["--jobs", String(jobs)])];
        log(`[local-qualify] ${hostKey}: qualify ${unit.name}@${unit.version}`);
        const result = await run(process.execPath, argv, { cwd: root, encoding: "utf8", windowsHide: true, maxBuffer: 512 * 1024 * 1024, timeout: HOST_TIMEOUT_MS, env: process.env });
        if (result.error || result.status !== 0) refuse(`${unit.name}: qualify-only failed (${result.error?.message ?? `exit ${result.status}`})\n${tail(`${result.stdout ?? ""}\n${result.stderr ?? ""}`)}`);
        const crateFile = path.join(bundle, file);
        const bytes = fs.readFileSync(crateFile);
        const evidence = JSON.parse(fs.readFileSync(path.join(bundle, "gate.json"), "utf8"));
        const sha = sha256(bytes);
        const row = evidence.runs?.[0];
        if (evidence.artifactSha256 !== sha || evidence.commit !== commit || evidence.name !== unit.name || evidence.version !== unit.version || evidence.runs?.length !== 1 || row?.commit !== commit) refuse(`${unit.name}: the gate evidence does not describe the bundled archive at ${commit.slice(0, 12)}`);
        const scanned = await archiveScan.scanArchiveForSecrets({ file: crateFile, registry: "cargo", name: unit.name, version: unit.version, scratchRoot: out });
        if (canonical) {
          const metadata = await (deps.uploadMetadata ?? uploadMetadataFor)({ root, run: deps.runSync ?? spawnSync, crateFile, name: unit.name, version: unit.version, deps });
          fs.writeFileSync(path.join(bundle, `${unit.name}-${unit.version}.upload.json`), `${JSON.stringify(metadata)}\n`);
        }
        const ignored = row.ignored ?? 0; const skipped = row.skipped ?? 0;
        rows[index] = { name: unit.name, version: unit.version, file, sha256: sha, bytesVerified: true, archiveScan: scanned, status: ignored > 0 || skipped > 0 ? "allowed-skips" : "pass", executed: row.passed, ignored, skipped, steps: row.steps ?? [], evidence };
      } catch (error) { failures.push(`${unit.name}: ${error.message}`); }
    }
  };
  await Promise.all(Array.from({ length: Math.min(concurrency, unitInfo.length) }, worker));
  if (failures.length) refuse(`${hostKey}: qualification failed:\n  - ${failures.join("\n  - ")}`);
  const result = { schema: 1, kind: "local-host-result", host: HOSTS[hostKey].name, hostOs: platform, arch: deps.arch ?? process.arch, candidateId: id, commit, tree, broker, qualifiedAt: (deps.now ?? (() => new Date().toISOString()))(), scans, units: rows };
  fs.writeFileSync(path.join(out, "host-result.json"), `${JSON.stringify(result, null, 2)}\n`);
  writeBundleManifest(out);
  log(`[local-qualify] ${hostKey}: ${rows.length} unit(s) qualified; bundle ${out}`);
  return { out, result };
}

/** The crates.io upload metadata of a packed crate: cargo metadata of its unpacked Cargo.toml, exactly as publish.mjs packs it. */
async function uploadMetadataFor({ root, run, crateFile, name, version, deps }) {
  const scratch = fs.mkdtempSync(path.join(path.dirname(path.dirname(crateFile)), "meta-"));
  try {
    const unpacked = run("tar", ["-xzf", path.basename(crateFile), "-C", scratch.replaceAll("\\", "/")], { cwd: path.dirname(crateFile), encoding: "utf8", windowsHide: true });
    if (unpacked.status !== 0) refuse(`cannot unpack ${path.basename(crateFile)}`);
    const inner = path.join(scratch, `${name}-${version}`);
    const metadata = await (deps.cargoMetadata ?? (async (manifest) => {
      const { runCargoReadOnly } = await import("./publish-cargo.mjs");
      const { runCargoSync } = await import("./managed-cargo.mjs");
      const result = await runCargoReadOnly(["metadata", "--format-version", "1", "--no-deps", "--manifest-path", manifest], { cwd: root, encoding: "utf8", maxBuffer: 512 * 1024 * 1024, windowsHide: true }, (_command, args, settings) => runCargoSync(args, settings), () => {});
      if (result.error || result.status !== 0) refuse(`cargo metadata failed for ${name}: ${tail(result.stderr)}`);
      return JSON.parse(result.stdout);
    }))(path.join(inner, "Cargo.toml"));
    const pkg = metadata.packages.find((item) => item.name === name && item.version === version);
    if (!pkg) refuse(`${path.basename(crateFile)}: packaged Cargo.toml does not describe ${name}@${version}`);
    const { buildCrateUploadMetadata } = await (deps.loadPublish ?? ((workspace) => loadFromRoot(workspace, "publish.mjs")))(root);
    return buildCrateUploadMetadata(pkg, pkg.readme ? fs.readFileSync(path.join(inner, pkg.readme), "utf8") : null);
  } finally { fs.rmSync(scratch, { recursive: true, force: true }); }
}

// ---- seal -------------------------------------------------------------------------------------

/** A stored (uncompressed) ZIP with fixed timestamps and flat names, so the same inputs give the same bytes. */
export function writeStoredZip(entries) {
  const locals = []; const central = []; let offset = 0;
  for (const { name, data } of entries) {
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,200}$/.test(name)) refuse(`unsafe ZIP entry name ${JSON.stringify(name)}`);
    const nameBytes = Buffer.from(name); const crc = zlib.crc32(data) >>> 0;
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0); local.writeUInt16LE(20, 4); local.writeUInt16LE(0, 6); local.writeUInt16LE(0, 8); local.writeUInt16LE(0, 10); local.writeUInt16LE(0x0021, 12);
    local.writeUInt32LE(crc, 14); local.writeUInt32LE(data.length, 18); local.writeUInt32LE(data.length, 22); local.writeUInt16LE(nameBytes.length, 26);
    const header = Buffer.alloc(46);
    header.writeUInt32LE(0x02014b50, 0); header.writeUInt16LE((3 << 8) | 20, 4); header.writeUInt16LE(20, 6); header.writeUInt16LE(0x0021, 14);
    header.writeUInt32LE(crc, 16); header.writeUInt32LE(data.length, 20); header.writeUInt32LE(data.length, 24); header.writeUInt16LE(nameBytes.length, 28);
    header.writeUInt32LE(((0o100644) << 16) >>> 0, 38); header.writeUInt32LE(offset, 42);
    locals.push(local, nameBytes, data); central.push(header, nameBytes);
    offset += local.length + nameBytes.length + data.length;
  }
  const directory = Buffer.concat(central); const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(entries.length, 8); end.writeUInt16LE(entries.length, 10); end.writeUInt32LE(directory.length, 12); end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, directory, end]);
}

/** The sealed manifest (loadSealed shape) over the canonical host's archives and upload metadata, in dependency order. */
export function buildManifest({ id, commit, units }) {
  return {
    schema: 1, registry: "crates", commit, repository: REPOSITORY, source: "local", candidateId: id,
    units: units.map(({ name, version, dir, crate, metadata }) => ({ name, version, dir, file: `${name}-${version}.crate`, size: crate.length, sha256: sha256(crate), metaFile: `${name}-${version}.upload.json`, metaSha256: sha256(metadata) })),
  };
}

/** Map one host's raw result into the `local-verify` receipt checkLocalReceipts reads. */
export function buildReceipt({ result, candidateId, manifestSha256, allow }) {
  return {
    schema: 1, kind: "local-verify", host: result.host, hostOs: result.hostOs, arch: result.arch, registry: "crates", candidateId,
    commit: result.commit, tree: result.tree, manifestSha256, broker: result.broker, qualifiedAt: result.qualifiedAt,
    allowHostExclusion: [...allow.hostExclusion], allowNoTest: [...allow.noTest], allowIgnored: [...allow.ignored], allowMissingAssets: [...allow.missingAssets],
    scans: result.scans,
    units: result.units.map((row) => ({ ...row })),
  };
}

/**
 * Seal the candidate from the pulled bundles. `bundles` maps host key to a verified directory. Refuses when a host's commit,
 * tree or archive bytes differ from the canonical host's: every host must have qualified the bytes that will be uploaded.
 * Returns the files to write and the candidate JSON, and runs the same loadSealed/checkLocalReceipts the promotion runs.
 */
export function sealCandidate({ id, commit, tree, plan, bundles, readFile = fs.readFileSync, stage }) {
  const results = {};
  for (const [key, dir] of Object.entries(bundles)) {
    const result = JSON.parse(readFile(path.join(dir, "host-result.json"), "utf8"));
    if (result.kind !== "local-host-result" || result.commit !== commit || result.tree !== tree || result.candidateId !== id || result.hostOs !== HOSTS[key].os || result.host !== HOSTS[key].name) refuse(`${key}: the host result is not for ${id} at ${commit.slice(0, 12)} on ${HOSTS[key].name}`);
    results[key] = result;
  }
  const canonical = bundles[CANONICAL_HOST];
  if (!canonical) refuse("the macOS host is the canonical packer; --hosts must include mac to cut a candidate");
  const units = plan.units.map((unit) => {
    const file = `${unit.name}-${unit.version}.crate`;
    const crate = readFile(path.join(canonical, unit.name, file));
    const metadata = readFile(path.join(canonical, unit.name, `${unit.name}-${unit.version}.upload.json`));
    for (const [key, result] of Object.entries(results)) {
      const row = result.units.find((item) => item.name === unit.name);
      if (!row) refuse(`${key}: no result for ${unit.name}`);
      if (row.status !== "allowed-exclusion" && row.sha256 !== sha256(crate)) refuse(`${unit.name}: ${key} packed ${String(row.sha256).slice(0, 12)} but the canonical macOS archive is ${sha256(crate).slice(0, 12)}; the hosts qualified different bytes, so no candidate is cut (use publish.yml for this unit)`);
    }
    return { name: unit.name, version: unit.version, dir: unit.dir, crate, metadata };
  });
  const allow = { ...plan.allow, ignored: plan.units.filter((unit) => Object.values(results).some((result) => (result.units.find((row) => row.name === unit.name)?.ignored ?? 0) > 0)).map((unit) => unit.name) };
  const manifest = buildManifest({ id, commit, units });
  const manifestBytes = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`);
  const manifestSha256 = sha256(manifestBytes);
  const files = [{ name: "manifest.json", data: manifestBytes }, ...units.flatMap((unit) => [{ name: `${unit.name}-${unit.version}.crate`, data: unit.crate }, { name: `${unit.name}-${unit.version}.upload.json`, data: unit.metadata }])];
  const receipts = Object.fromEntries(Object.keys(results).map((key) => [HOSTS[key].name, Buffer.from(`${JSON.stringify(buildReceipt({ result: results[key], candidateId: id, manifestSha256, allow }), null, 2)}\n`)]));
  // Same checks the promotion repeats, before anything is uploaded.
  stage({ files, receipts, allow });
  return { manifest, manifestSha256, files, receipts, allow, units, zip: writeStoredZip(files) };
}

/** Candidate JSON and the approval skeleton. reviewer/reviewRef are null on purpose: null fails the approval loader until a reviewer fills it. */
export function candidateDocument({ id, commit, units, allow, names, hashes }) {
  const candidate = { id, source: "local", sourceSha: commit, registry: "crates", units: units.map((unit) => unit.name), allow, release: { tag: `candidate-${id}` }, assets: Object.fromEntries(Object.entries(names).map(([kind, name]) => [kind, { name, sha256: hashes[kind] }])) };
  const artifacts = units.map((unit) => ({ registry: "crates", name: unit.name, version: unit.version, sha256: sha256(unit.crate), reviewer: null, reviewRef: null }));
  return { candidate, approval: { candidates: [candidate], artifacts } };
}

// ---- driver -----------------------------------------------------------------------------------

/**
 * `right-release qualify cargo ...`. `deps` injects every effect (git, spawn, ssh, gh, file reads, the host run) so a test can
 * observe the plan: which commands run on which host, that both hosts start before either finishes, what the release gets.
 */
export async function runQualify(argv, deps = {}) {
  const log = deps.log ?? ((line) => process.stdout.write(`${line}\n`));
  const platform = deps.platform ?? process.platform;
  const parsed = parseQualifyArgs(argv, platform);
  const here = localHostKey(platform);
  const root = deps.root ?? path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
  const git = deps.git ?? defaultGit;
  const { head, tree } = checkoutState({ git, root });
  const plan = planUnits({ root, crates: parsed.crates, readJson: deps.readJson, readPackage: deps.readPackage, dependsOn: deps.dependsOn });
  const id = parsed.id ?? candidateIdFor({ crates: parsed.crates, version: plan.units[0].version, head });
  const outDir = path.join(parsed.outRoot, id);
  const exec = deps.exec ?? ((command, args, options) => spawnCollect(command, args, { encoding: "utf8", windowsHide: true, maxBuffer: 1024 * 1024 * 1024, timeout: HOST_TIMEOUT_MS, ...options }));
  const remoteNode = deps.remoteNode ?? process.env.RK_LOCALQ_REMOTE_NODE ?? "node";
  log(`[qualify] ${id}: ${plan.units.map((unit) => `${unit.name}@${unit.version}`).join(", ")} at ${head.slice(0, 12)} on ${parsed.hosts.join(" + ")}`);

  const runHost = async (key) => {
    const host = HOSTS[key];
    const hostOut = `${key === here ? outDir : `${host.scratch}/${id}`}/${key}`;
    const args = hostArgs({ root: parsed.roots[key], id, commit: head, crates: parsed.crates, out: hostOut, exclude: plan.units.filter((unit) => unit.excludedOn.includes(key)).map((unit) => unit.name), allowIgnoredUnits: parsed.allowIgnoredUnits, jobs: parsed.jobs, concurrency: parsed.concurrency, canonical: key === CANONICAL_HOST });
    if (key === here) {
      const options = parseHostArgs(args.slice(1));
      const { out } = await (deps.qualifyHost ?? qualifyHost)({ ...options, root }, deps.host ?? {});
      return out;
    }
    const reach = reachArgs(here, remoteCommand({ root: parsed.roots[key], args, node: remoteNode }));
    const ran = await exec(reach.command, reach.args, {});
    if (ran.error || ran.status !== 0) refuse(`${key}: remote qualification failed (${ran.error?.message ?? `exit ${ran.status}`})\n${tail(`${ran.stdout ?? ""}\n${ran.stderr ?? ""}`)}`);
    const pull = reachArgs(here, remoteCommand({ root: parsed.roots[key], args: ["export", "--dir", hostOut], node: remoteNode }));
    const exported = await exec(pull.command, pull.args, {});
    if (exported.error || exported.status !== 0) refuse(`${key}: cannot pull the bundle (${exported.error?.message ?? `exit ${exported.status}`})\n${tail(exported.stderr)}`);
    return importBundle(exported.stdout, path.join(outDir, key));
  };
  // Both hosts run at once; one host failing does not hide the other's result.
  const settled = await Promise.allSettled(parsed.hosts.map(runHost));
  const failed = settled.map((row, index) => {
    if (row.status !== "rejected") return null;
    const message = String(row.reason?.message ?? row.reason);
    return message.startsWith(`${parsed.hosts[index]}:`) ? message : `${parsed.hosts[index]}: ${message}`;
  }).filter(Boolean);
  if (failed.length) refuse(`qualification failed:\n${failed.join("\n")}`);
  const bundles = Object.fromEntries(parsed.hosts.map((key, index) => [key, settled[index].value]));
  for (const dir of Object.values(bundles)) verifyBundle(dir);
  if (!(parsed.hosts.includes("win") && parsed.hosts.includes("mac"))) {
    log(`[qualify] ${id}: only ${parsed.hosts.join(", ")} ran; bundles are in ${outDir}. A candidate needs both hosts, so none is cut.`);
    return { id, outDir, bundles, candidate: null };
  }
  const upload = await (deps.loadUpload ?? ((workspace) => loadFromRoot(workspace, "upload.mjs")))(root);
  const reviewed = await (deps.loadReviewed ?? ((workspace) => loadFromRoot(workspace, "reviewed-candidate.mjs")))(root);
  const assetsDir = path.join(outDir, "assets"); const stageDir = path.join(outDir, "stage");
  // The staged layout is the one promotion rebuilds from the release assets; the same loaders judge it before anything is uploaded.
  const stage = ({ files, receipts, allow }) => {
    fs.mkdirSync(path.join(stageDir, "packed"), { recursive: true });
    for (const { name, data } of files) fs.writeFileSync(path.join(stageDir, "packed", name), data, { flag: "wx" });
    for (const [host, data] of Object.entries(receipts)) { fs.mkdirSync(path.join(stageDir, "receipts", host), { recursive: true }); fs.writeFileSync(path.join(stageDir, "receipts", host, `receipt-${host}.json`), data, { flag: "wx" }); }
    const staged = upload.loadSealed({ packDir: path.join(stageDir, "packed"), registry: "crates", unitsText: plan.units.map((unit) => unit.name).join(" ") });
    const problems = [...staged.problems];
    if (staged.manifest) problems.push(...upload.checkLocalReceipts({ manifest: staged.manifest, manifestSha256: staged.manifestSha256, receiptsDir: path.join(stageDir, "receipts"), allow, candidate: { id } }));
    if (problems.length) refuse(`the sealed candidate would be refused at promotion:\n  - ${problems.join("\n  - ")}`);
  };
  const sealed = sealCandidate({ id, commit: head, tree, plan, bundles, stage });
  fs.mkdirSync(assetsDir, { recursive: true });
  const names = { packed: reviewed.localAssetName(id, "packed"), windows: reviewed.localAssetName(id, "windows"), macos: reviewed.localAssetName(id, "macos") };
  const contents = { packed: sealed.zip, windows: sealed.receipts.windows, macos: sealed.receipts.macos };
  const hashes = {};
  for (const [kind, name] of Object.entries(names)) { fs.writeFileSync(path.join(assetsDir, name), contents[kind], { flag: "wx" }); hashes[kind] = sha256(contents[kind]); }
  const document = candidateDocument({ id, commit: head, units: sealed.units, allow: sealed.allow, names, hashes });
  reviewed.validateCandidate(structuredClone(document.candidate));
  fs.writeFileSync(path.join(outDir, "candidate.json"), `${JSON.stringify(document, null, 2)}\n`, { flag: "wx" });
  if (parsed.release) {
    const tag = document.candidate.release.tag;
    const created = (deps.spawn ?? spawnSync)("gh", ["release", "create", tag, "--repo", REPOSITORY, "--prerelease", "--target", head, "--title", `Local qualification ${id}`, "--notes", `Locally qualified candidate ${id} at ${head}. Not a release; promoted only by publish-reviewed.yml after its pins are committed.`, ...Object.values(names).map((name) => path.join(assetsDir, name))], { encoding: "utf8", windowsHide: true });
    if (created.error || created.status !== 0) refuse(`gh release create failed: ${tail(`${created.stdout ?? ""}\n${created.stderr ?? ""}`)}`);
    const view = (deps.spawn ?? spawnSync)("gh", ["api", `repos/${REPOSITORY}/releases/tags/${tag}`], { encoding: "utf8", windowsHide: true });
    let release; try { release = JSON.parse(view.stdout); } catch { refuse("cannot read the created prerelease back"); }
    const have = new Map((release.assets ?? []).map((asset) => [asset.name, asset.size]));
    if (release.prerelease !== true || Object.values(names).some((name) => have.get(name) !== fs.statSync(path.join(assetsDir, name)).size) || have.size !== Object.keys(names).length) refuse(`the prerelease ${tag} does not hold exactly the three assets`);
    log(`[qualify] prerelease ${tag} created and read back`);
  }
  log(`[qualify] candidate ${id} sealed. Add this to ci/approved-artifacts.json (a reviewer sets reviewer and reviewRef, adds the two publish.test.mjs lines, commits; nothing was written for you):`);
  log(JSON.stringify(document, null, 2));
  return { id, outDir, bundles, candidate: document.candidate, document };
}

const realOrSelf = (file) => { try { return fs.realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  const [first, ...rest] = process.argv.slice(2);
  const fail = (error) => { process.stderr.write(`right-release qualify: ${error.message}\n`); process.exitCode = 1; };
  try {
    if (first === "host") await qualifyHost(parseHostArgs(rest));
    else if (first === "export") { const dir = rest[0] === "--dir" ? rest[1] : undefined; if (!dir || rest.length !== 2) refuse("usage: local-qualify.mjs export --dir <bundle>"); process.stdout.write(exportBundle(path.resolve(dir))); }
    else await runQualify(process.argv.slice(2));
  } catch (error) { fail(error); }
}
