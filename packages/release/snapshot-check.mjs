// Snapshot-aware Class A (joint plan step 2; docs/GATE-CONTRACT.md §1 "Broker interface", E17, E18; rightkit#1257).
//
// `rightkit-gate check --commit <sha> --full-a --snapshot <dir>` judges the gate-owned snapshot of a selected commit: the checkout
// is only where the commit and its objects come from (cwd), never what is checked. Its HEAD and status are not read. The snapshot is
// verified to be exactly the commit's tree (seeded identity, snapshot.mjs) before and after the check; a mismatch or an error is
// RK_CHECK_ERROR, exit 2.
//
// Dependencies for the checks that execute repository code (tests, typecheck, lint) are installed INTO the snapshot from the commit's
// lockfile (coordinator decision A, recorded in decisions.md with rightkit#1257): the pinned pnpm, `--offline --frozen-lockfile
// --ignore-scripts --ignore-pnpmfile`, and the store the primary checkout's own install recorded (its node_modules/.modules.yaml
// storeDir), resolved before any cwd change. No new store, no network, no code of the live checkout. A store miss is RK_CHECK_ERROR
// naming the package and root, never a skipped check. The next exact sync prunes this install.
//
// `rightkit-gate live-scans --repo <root>` re-runs only the live remote scans (pnpm-audit, per pnpm root) and answers the broker's
// parser (rustright verdict_reuse.rs parse_live_scans): one JSON line, exit 0 when every scan passed, 1 otherwise.
//
// A red full Class A whose every failure is deterministic prints `rightkit-gate: verdict deterministic failed=<ids> env=<digest>`;
// a timeout, a missing tool, a fetch or lease problem, a cancellation, a deferral, or any live scan among the failures prints none.

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { LIVE_SCAN_IDS, defaultSpawn, isLiveScan, planScans, runScans } from "./deterministic-scans.mjs";
import { SnapshotError, snapshotTree } from "./snapshot.mjs";

export const CHECK_ERROR = "RK_CHECK_ERROR";
export const LIVE_SCANS_SCHEMA = "rightkit.live-scans/1";
export const DETERMINISTIC_MARKER = "rightkit-gate: verdict deterministic";

/** Failure codes a re-run of the same inputs reproduces: an exit of the check itself, its own findings, a test summary problem. */
const DETERMINISTIC_CODES = new Set(["EXIT", "RUN_SCRIPT_TOO_LARGE", "NO_TESTS", "ALL_SKIPPED", "SUMMARY_FAILED", "NO_TEST_SUMMARY"]);

export class CheckError extends Error {
  constructor(message) { super(message); this.code = CHECK_ERROR; }
}

const text = (r) => String(r?.stdout ?? "");
const failed = (r) => !r || Boolean(r.error) || r.status !== 0;
const tail = (r) => `${r?.stderr ?? ""}\n${r?.stdout ?? ""}`.trim().split(/\r?\n/).slice(-6).join(" | ").slice(-600);
const inside = (child, parent) => { const rel = path.relative(parent, child); return rel === "" || (!!rel && !rel.startsWith("..") && !path.isAbsolute(rel)); };

/** The `--snapshot <dir>` of a check: absolute, an existing real directory, not inside the checkout and not holding it. */
export function snapshotOption(args, { checkout }) {
  const i = args.indexOf("--snapshot");
  if (i < 0) return null;
  const dir = args[i + 1];
  if (!dir || dir.startsWith("--")) throw new CheckError("check --snapshot needs a directory");
  if (!path.isAbsolute(dir)) throw new CheckError(`check --snapshot needs an absolute directory, not ${JSON.stringify(dir)}`);
  if (!args.includes("--full-a") || !args.includes("--commit")) throw new CheckError("check --snapshot is full class A of a selected commit: it needs --commit <sha> and --full-a");
  if (args.includes("--worktree")) throw new CheckError("check --snapshot never checks the working tree: drop --worktree");
  let st;
  try { st = fs.lstatSync(dir); } catch { throw new CheckError(`snapshot ${dir} does not exist`); }
  if (!st.isDirectory()) throw new CheckError(`snapshot ${dir} is not a directory (a link is never followed)`);
  const real = fs.realpathSync.native(dir);
  if (inside(real, checkout) || inside(checkout, real)) throw new CheckError(`snapshot ${real} and checkout ${checkout} overlap`);
  return real;
}

/** `{ commit, tree }` of a revision in the checkout's object database. */
export function commitOf(checkout, rev, spawn = defaultSpawn) {
  const c = spawn("git", ["rev-parse", "--verify", "--quiet", `${rev}^{commit}`], { cwd: checkout });
  if (failed(c)) throw new CheckError(`commit ${rev} does not resolve in ${checkout}`);
  const commit = text(c).trim();
  const t = spawn("git", ["rev-parse", "--verify", "--quiet", `${commit}^{tree}`], { cwd: checkout });
  if (failed(t)) throw new CheckError(`commit ${commit} has no tree`);
  return { commit, tree: text(t).trim() };
}

/** The snapshot holds exactly the commit's tree (seeded identity, the broker's method); otherwise CheckError. */
export function verifySnapshotDir({ checkout, dir, commit, tree, when }) {
  let actual;
  try { actual = snapshotTree({ checkout, dir, baseTree: tree }); }
  catch (e) { throw new CheckError(`cannot compute the tree of snapshot ${dir} ${when}: ${e.message}`); }
  if (actual !== tree) throw new CheckError(`snapshot ${dir} is tree ${actual} ${when}, not tree ${tree} of commit ${commit}`);
  return actual;
}

/** Every file of the commit with its blob id, from the checkout's object database: what the snapshot holds. */
export function commitFiles(checkout, commit, spawn = defaultSpawn) {
  const r = spawn("git", ["ls-tree", "-r", "-z", "--full-tree", commit], { cwd: checkout, maxBuffer: 1 << 30 });
  if (failed(r)) throw new CheckError(`cannot list commit ${commit}: ${tail(r)}`);
  const ids = new Map();
  for (const record of text(r).split("\0").filter(Boolean)) {
    const tab = record.indexOf("\t");
    ids.set(record.slice(tab + 1), record.slice(0, tab).split(" ")[2]);
  }
  const files = [...ids.keys()].sort();
  const digestOf = (list) => crypto.createHash("sha256").update(list.map((f) => `${f}\t${ids.get(f)}\n`).join("")).digest("hex");
  return { files, ids, digestOf, digest: digestOf(files) };
}

/** `storeDir` from a node_modules/.modules.yaml (pnpm writes it as `"storeDir": "<abs>"` or `storeDir: <abs>`). */
export function modulesStoreDir(yamlText) {
  const m = /^\s*"?storeDir"?\s*:\s*"?([^"\n]+?)"?\s*,?\s*$/m.exec(String(yamlText ?? ""));
  return m ? m[1].trim() : null;
}

/**
 * The existing warm store a pnpm root of the PRIMARY checkout installs from: the storeDir its own install recorded (the root's
 * node_modules/.modules.yaml, else the repository root's), else what `pnpm store path` answers in that root with the primary
 * environment. It must be an absolute directory that exists; nothing is created.
 */
export function resolvePrimaryStore({ checkout, root = ".", env = process.env, spawn = defaultSpawn, readFile = (f) => fs.readFileSync(f, "utf8"), exists = fs.existsSync }) {
  const candidates = [...new Set([path.join(checkout, root, "node_modules", ".modules.yaml"), path.join(checkout, "node_modules", ".modules.yaml")])];
  for (const file of candidates) {
    let dir = null;
    try { dir = modulesStoreDir(readFile(file)); } catch { dir = null; }
    if (dir && path.isAbsolute(dir) && exists(dir)) return { storeDir: dir, source: path.relative(checkout, file).split(path.sep).join("/") };
  }
  const r = spawn("pnpm", ["store", "path"], { cwd: path.join(checkout, root), env: { ...env, COREPACK_ENABLE_AUTO_PIN: "0" } });
  const dir = failed(r) ? null : text(r).trim().split(/\r?\n/).pop();
  if (dir && path.isAbsolute(dir) && exists(dir)) return { storeDir: dir, source: "pnpm store path (primary checkout)" };
  throw new CheckError(`no existing pnpm store for root ${root} of ${checkout}: its install recorded none and \`pnpm store path\` answered ${dir ? JSON.stringify(dir) : tail(r) || "nothing"}; install the checkout's dependencies once, then run the check again`);
}

/** The pnpm version a root pins (`packageManager: pnpm@x.y.z`, its own package.json or the repository root's), or null. */
export function pinnedPnpm(dir, { readFile = (f) => fs.readFileSync(f, "utf8") } = {}) {
  try { const pm = JSON.parse(readFile(path.join(dir, "package.json"))).packageManager; const m = /^pnpm@(\d+\.\d+\.\d+)/.exec(String(pm ?? "")); return m ? m[1] : null; } catch { return null; }
}

/** Dependencies whose install scripts the repository allows (pnpm-workspace.yaml `allowBuilds: { name: true }`, `onlyBuiltDependencies`). */
export function allowedBuilds(dir, { readFile = (f) => fs.readFileSync(f, "utf8") } = {}) {
  const names = new Set();
  let ws = "";
  try { ws = readFile(path.join(dir, "pnpm-workspace.yaml")); } catch { ws = ""; }
  const block = (key) => { const m = new RegExp(`^${key}:\\s*\\n((?:[ \\t]+.*\\n?|\\s*#.*\\n?|\\s*\\n)*)`, "m").exec(ws); return m ? m[1] : ""; };
  for (const line of block("allowBuilds").split("\n")) { const m = /^\s+['"]?([^'":#\s]+)['"]?\s*:\s*true\b/.exec(line); if (m) names.add(m[1]); }
  for (const line of block("onlyBuiltDependencies").split("\n")) { const m = /^\s+-\s+['"]?([^'"#\s]+)['"]?/.exec(line); if (m) names.add(m[1]); }
  try { for (const n of JSON.parse(readFile(path.join(dir, "package.json"))).pnpm?.onlyBuiltDependencies ?? []) names.add(n); } catch { /* none */ }
  return [...names].sort();
}

/**
 * Installs each pnpm root's dependencies into the snapshot from the commit's lockfile and the primary checkout's existing store.
 * Returns one record per root ({ root, storeDir, storeSource, pnpm, lockfileSha256, ms, allowedBuildsNotRun }); throws CheckError on a
 * store miss, a pnpm that is not the pinned one, or any install failure.
 */
export function installSnapshotDependencies({ checkout, snapshot, roots, env = process.env, spawn = defaultSpawn, now = () => Date.now(), readFile, exists }) {
  const records = [];
  const versionOf = (cwd) => { const r = spawn("pnpm", ["--version"], { cwd, env: { ...env, COREPACK_ENABLE_AUTO_PIN: "0" } }); return failed(r) ? null : text(r).trim().split(/\r?\n/).pop(); };
  for (const root of roots) {
    const dir = path.join(snapshot, root);
    if (!fs.existsSync(path.join(dir, "pnpm-lock.yaml"))) throw new CheckError(`pnpm root ${root} of the commit has no pnpm-lock.yaml: dependencies cannot be installed from a lockfile`);
    const { storeDir, source } = resolvePrimaryStore({ checkout, root, env, spawn, ...(readFile ? { readFile } : {}), ...(exists ? { exists } : {}) });
    const pin = pinnedPnpm(dir) ?? pinnedPnpm(snapshot);
    const version = versionOf(path.dirname(snapshot));
    if (!version) throw new CheckError("pnpm is not runnable on this host; the snapshot's dependencies cannot be installed");
    if (pin && version !== pin) throw new CheckError(`pnpm ${version} is on PATH, but root ${root} of the commit pins pnpm@${pin} (packageManager); install the pinned pnpm`);
    const started = now();
    const r = spawn("pnpm", ["install", "--offline", "--frozen-lockfile", "--ignore-scripts", "--ignore-pnpmfile", "--store-dir", storeDir, "--reporter", "append-only"], {
      cwd: dir, env: { ...env, COREPACK_ENABLE_AUTO_PIN: "0", CI: "true" }, maxBuffer: 1 << 28,
    });
    const ms = now() - started;
    if (failed(r)) {
      const out = `${r?.stdout ?? ""}\n${r?.stderr ?? ""}`;
      const miss = /ERR_PNPM_NO_OFFLINE_TARBALL[^\n]*?(https?:\/\/\S+?\/-\/(\S+?)\.tgz)/.exec(out);
      if (miss) throw new CheckError(`root ${root}: ${miss[2]} is missing from the store ${storeDir} (offline install, no network fallback); install the checkout's dependencies so the store holds it, then run the check again`);
      throw new CheckError(`root ${root}: offline dependency install failed (store ${storeDir}): ${tail(r) || r?.error?.message || `exit ${r?.status}`}`);
    }
    const lockfileSha256 = crypto.createHash("sha256").update(fs.readFileSync(path.join(dir, "pnpm-lock.yaml"))).digest("hex");
    records.push({ root, storeDir, storeSource: source, pnpm: version, pinned: pin, lockfileSha256, ms, allowedBuildsNotRun: allowedBuilds(dir) });
  }
  return records;
}

/** One line per installed root, for the check's output and evidence. */
export function describeInstall(record) {
  return `snapshot dependencies root=${record.root} pnpm=${record.pnpm}${record.pinned ? ` (pinned ${record.pinned})` : ""} store=${record.storeDir} (from ${record.storeSource}) lockfile=sha256:${record.lockfileSha256} ms=${record.ms}${record.allowedBuildsNotRun.length ? ` build-scripts-not-run=${record.allowedBuildsNotRun.join(",")}` : ""}`;
}

/** The deterministic-red marker for a failed run, or null when any failure is not one a re-run of the same inputs reproduces. */
export function deterministicMarker(scanResult, env) {
  const failing = scanResult.scans.filter((r) => r.status !== "pass");
  if (!failing.length || !env) return null;
  // A fail row with no code is a scan's own findings (an internal check's list); every other code names why it could not judge.
  const eligible = failing.every((r) => r.status === "fail" && !isLiveScan(r.id) && (r.code === undefined || r.code === null || DETERMINISTIC_CODES.has(r.code)));
  return eligible ? `${DETERMINISTIC_MARKER} failed=${failing.map((r) => r.id).join(",")} env=${env}` : null;
}

/** Repository-relative files under `root`: git's view when `root` is a work tree's top level, else a walk (no node_modules, no .git). */
export function rootFiles(root, spawn = defaultSpawn) {
  const top = spawn("git", ["rev-parse", "--show-toplevel"], { cwd: root });
  if (!failed(top) && path.resolve(text(top).trim()) === path.resolve(root)) {
    const r = spawn("git", ["ls-files", "-z", "--cached", "--others", "--exclude-standard"], { cwd: root, maxBuffer: 1 << 30 });
    if (!failed(r)) return text(r).split("\0").filter(Boolean).map((f) => f.split(path.sep).join("/"));
  }
  const out = [];
  const walk = (d, rel) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      if (e.name === "node_modules" || e.name === ".git") continue;
      const r = rel ? `${rel}/${e.name}` : e.name;
      if (e.isDirectory()) walk(path.join(d, e.name), r); else if (e.isFile()) out.push(r);
    }
  };
  walk(root, "");
  return out.sort();
}

/**
 * `rightkit-gate live-scans --repo <root>`: the live remote scans of the tree at <root>, planned exactly as the full check plans them
 * (planScans, all files changed) and run through the same runner. `deps` supplies the gate's unit discovery.
 */
export async function liveScansCommand(o, args, { discoverPnpmRoots, discoverNpmUnits, plan = planScans, run = runScans }) {
  const i = args.indexOf("--repo");
  if (i < 0 || !args[i + 1] || args[i + 1].startsWith("--") || args.length !== 2) { o.err("rightkit-gate: live-scans takes exactly --repo <checked tree root>"); return 2; }
  const root = path.resolve(o.cwd, args[i + 1]);
  let scans;
  try {
    const files = rootFiles(root, o.spawn);
    const tracked = (name) => files.filter((f) => f === name || f.endsWith(`/${name}`));
    const readFile = (f) => o.readFile(f);
    const npmRoots = discoverPnpmRoots({ root, tracked, exists: o.exists });
    const npmUnits = discoverNpmUnits({ root, tracked, readFile, exists: o.exists });
    const full = plan({ changed: files, contentRoot: root, liveRoot: root, trustedRoot: root, includeTests: false, rustUnits: [], npmUnits, npmRoots, tier: "release", exists: o.exists, readFile: o.readFile, platform: o.platform });
    const live = { ...full, scans: full.scans.filter((s) => isLiveScan(s.id)), deferred: [] };
    const result = await run(live, { classes: ["A"], jobs: 2 });
    scans = result.scans.map((r) => ({ id: r.id, status: r.status === "pass" ? "passed" : "failed", ...(r.status === "pass" ? {} : { detail: String(r.detail ?? r.status).slice(0, 300) }) }));
  } catch (e) {
    o.err(`rightkit-gate: live-scans could not run: ${e.message}`);
    return 2;
  }
  o.out(JSON.stringify({ schema: LIVE_SCANS_SCHEMA, scans: scans.map(({ id, status }) => ({ id, status })) }));
  for (const s of scans) if (s.status === "failed") o.err(`rightkit-gate: live scan ${s.id} failed: ${s.detail}`);
  return scans.every((s) => s.status === "passed") ? 0 : 1;
}

export { LIVE_SCAN_IDS, SnapshotError };
