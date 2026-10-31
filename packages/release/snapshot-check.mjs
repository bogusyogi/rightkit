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
import { LIVE_SCAN_IDS, defaultSpawn, isLiveScan, isRegistryScan, launchGuardEnv, planScans, runScans } from "./deterministic-scans.mjs";
import { compileLaunch } from "./class-a-launch-guard.mjs";
import { SnapshotError, dependencyDirs, isInside, snapshotProblem, snapshotTree, treeEntries } from "./snapshot.mjs";
import { discoveryRun, sealedRun } from "./snapshot-git.mjs";

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
// A path is outside only as `..`, `..<sep>…` or absolute: a sibling named `..x` is inside (snapshot.mjs isInside; review item5-r2 #4).
const inside = (child, parent) => isInside(child, parent, path);

/** The `--snapshot <dir>` of a check: absolute, an existing real directory, not inside the checkout and not holding it. */
export function snapshotOption(args, { checkout }) {
  const i = args.indexOf("--snapshot");
  if (i < 0) return null;
  const dir = args[i + 1];
  if (!dir || dir.startsWith("--")) throw new CheckError("check --snapshot needs a directory");
  if (!path.isAbsolute(dir)) throw new CheckError(`check --snapshot needs an absolute directory, not ${JSON.stringify(dir)}`);
  // Full class A (`--full-a`, the door's) or the content checks alone (plain `check --commit`): either way the snapshot is the commit's bytes.
  if (!args.includes("--commit")) throw new CheckError("check --snapshot judges the snapshot of a selected commit: it needs --commit <sha>");
  if (args.includes("--worktree")) throw new CheckError("check --snapshot never checks the working tree: drop --worktree");
  let st;
  try { st = fs.lstatSync(dir); } catch { throw new CheckError(`snapshot ${dir} does not exist`); }
  if (!st.isDirectory()) throw new CheckError(`snapshot ${dir} is not a directory (a link is never followed)`);
  const real = fs.realpathSync.native(dir);
  if (inside(real, checkout) || inside(checkout, real)) throw new CheckError(`snapshot ${real} and checkout ${checkout} overlap`);
  return real;
}

/**
 * `{ commit, tree }` of a revision in the checkout's object database. The sealed invocation (snapshot-git.mjs, item 5 D3): the
 * revision is resolved under the sealed environment with replace refs off, so a replace ref cannot change what <sha>^{tree} means.
 * `spawn` is accepted for the callers that still pass one and is not used: the gate's identity git never goes through an injected runner.
 */
export function commitOf(checkout, rev, spawn = defaultSpawn) { // eslint-disable-line no-unused-vars
  const name = String(rev ?? "");
  if (!name || name.startsWith("-") || name.includes("\0")) throw new CheckError(`commit ${name} does not resolve in ${checkout}`);
  const c = discoveryRun(checkout, ["rev-parse", "--verify", "--quiet", `${name}^{commit}`]);
  if (failed(c)) throw new CheckError(`commit ${name} does not resolve in ${checkout}`);
  const commit = text(c).trim();
  const t = discoveryRun(checkout, ["rev-parse", "--verify", "--quiet", `${commit}^{tree}`]);
  if (failed(t)) throw new CheckError(`commit ${commit} has no tree`);
  return { commit, tree: text(t).trim() };
}

/**
 * The snapshot holds exactly the commit (E6, GATE-CONTRACT §1): no path under it the commit's tree lacks (ignored ones included; the
 * only allowances are snapshot.mjs's named pnpm dependency directories and the gate's config-only .git/config), no link resolving out
 * of it or into the checkout, and the seeded tree id (the broker's method) equal to the commit's tree. Otherwise CheckError. Returns the tree id.
 *
 * Stage-output ledger (row 5.09d), the same walk as snapshot.mjs verifySnapshot (one implementation, snapshotProblem): given `ledger`
 * (the entries an earlier verify returned) and/or `admitNew` (the END of a stage only), the stage's own ignored, untracked outputs
 * are admitted and the ledger must still match exactly. Then the result is `{ tree, ledger: [{ path, type, size, sha256, execBit }] }`.
 */
export function verifySnapshotDir({ checkout, dir, commit, tree, when, ledger, admitNew }) {
  const ledgerMode = ledger !== undefined || admitNew === true;
  let inspected;
  try { inspected = snapshotProblem({ dir, entries: treeEntries(checkout, commit), checkout, ledger, admitNew }); }
  catch (e) { throw new CheckError(`cannot list snapshot ${dir} ${when}: ${e.message}`); }
  const extra = ledgerMode ? inspected.problem : inspected;
  if (extra) throw new CheckError(`snapshot ${dir} is not exactly commit ${commit}${ledgerMode ? " plus its stage-output ledger" : ""} ${when}: ${extra}`);
  let actual;
  try { actual = snapshotTree({ checkout, dir, baseTree: tree }); }
  catch (e) { throw new CheckError(`cannot compute the tree of snapshot ${dir} ${when}: ${e.message}`); }
  if (actual !== tree) throw new CheckError(`snapshot ${dir} is tree ${actual} ${when}, not tree ${tree} of commit ${commit}`);
  return ledgerMode ? { tree: actual, ledger: inspected.admitted } : actual;
}

/** Every file of the commit with its blob id, from the checkout's object database: what the snapshot holds. */
export function commitFiles(checkout, commit, spawn = defaultSpawn) { // eslint-disable-line no-unused-vars
  const r = sealedRun(checkout, ["ls-tree", "-r", "-z", "--full-tree", commit]);
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

/** `storeDir` from a node_modules/.modules.yaml: pnpm writes `"storeDir": "<JSON string>"` (Windows paths escaped, `D:\\x`) or `storeDir: <abs>`. */
export function modulesStoreDir(yamlText) {
  const src = String(yamlText ?? "");
  const quoted = /^\s*"?storeDir"?\s*:\s*("(?:[^"\\\n]|\\.)*")\s*,?\s*$/m.exec(src);
  if (quoted) { try { return JSON.parse(quoted[1]); } catch { return null; } }
  const bare = /^\s*storeDir\s*:\s*([^"'\s][^\n]*?)\s*$/m.exec(src);
  return bare ? bare[1].trim() : null;
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
 * A dependency whose install script the repository allows but `--ignore-scripts` skipped must be shown usable without it, never
 * assumed: its own bin runs with `--version`, or, with no bin, the package loads in a fresh node. The probe runs before class A is
 * green, so it runs under the class A launch guard (launchGuardEnv): it can never start a compile; a refused launch fails the probe.
 * A package that is itself a compile tool the guard lists (esbuild) is never launched here: class A cannot launch it either, so its
 * usability is proven where it is launched, after class A, and it is recorded as such. A package the root does not have installed is
 * not required. Returns [{ name, version, probe, result }]; throws CheckError naming the package and root when one is installed but
 * not usable.
 */
export function probeSkippedBuilds({ dir, root, names, spawn = defaultSpawn, env = process.env, node = process.execPath }) {
  const results = [];
  const store = path.join(dir, "node_modules", ".pnpm");
  let entries = [];
  try { entries = fs.readdirSync(store); } catch { entries = []; }
  for (const name of names) {
    const flat = name.replace("/", "+");
    for (const entry of entries.filter((e) => e.startsWith(`${flat}@`)).sort()) {
      const pkgDir = path.join(store, entry, "node_modules", ...name.split("/"));
      let manifest;
      try { manifest = JSON.parse(fs.readFileSync(path.join(pkgDir, "package.json"), "utf8")); } catch { continue; }
      const bins = typeof manifest.bin === "string" ? { [name.split("/").pop()]: manifest.bin } : (manifest.bin ?? {});
      const binName = Object.keys(bins).find((b) => b === name.split("/").pop()) ?? Object.keys(bins)[0];
      const compiler = compileLaunch(name.split("/").pop()) ?? (binName ? compileLaunch(binName) : null);
      if (compiler) { results.push({ name, version: manifest.version, probe: "none", result: `compile tool (${compiler}): class A never launches it; its usability is proven where it is launched, after class A` }); continue; }
      const probe = binName ? [path.join(pkgDir, bins[binName]), "--version"] : ["-e", `require(${JSON.stringify(pkgDir)})`];
      const r = spawn(node, probe, { cwd: dir, env: { ...env, ...launchGuardEnv(env) }, boundedMs: 60_000 });
      const out = text(r).trim().split(/\r?\n/).pop() ?? "";
      if (failed(r)) {
        const refused = `${r?.stderr ?? ""}\n${r?.stdout ?? ""}`.split(/\r?\n/).find((l) => l.includes("class A refuses a compile/emit launch"));
        const why = refused ? `it launched a compile, which the class A launch guard refused: ${refused.replace(/^rightkit-gate: /, "").trim()}` : (tail(r) || r?.error?.message || `exit ${r?.status}`);
        throw new CheckError(`root ${root}: ${name}@${manifest.version} needs its install script (allowed by the repository, not run under --ignore-scripts) and is not usable without it: ${binName ? `${binName} --version` : "require"} failed: ${why}`);
      }
      results.push({ name, version: manifest.version, probe: binName ? `${binName} --version` : "require", result: out || "loaded" });
    }
  }
  return results;
}

/**
 * Installs each pnpm root's dependencies into the snapshot from the commit's lockfile and the primary checkout's existing store.
 * Returns one record per root ({ root, storeDir, storeSource, pnpm, lockfileSha256, ms, allowedBuildsNotRun }); throws CheckError on a
 * store miss, a pnpm that is not the pinned one, or any install failure. A failed install invalidates only that root's generated
 * dependency directories: remove them without following links before reporting the original error. Other roots and stores stay warm.
 */
export function installSnapshotDependencies({ checkout, snapshot, commit, tree, ledger, roots, packageDirs, env = process.env, spawn = defaultSpawn, now = () => Date.now(), readFile, exists, probe = probeSkippedBuilds }) {
  const records = [];
  // Cleanup authority comes from the selected commit, never a path supplied by pnpm or an untracked manifest. Verify before any
  // installer runs; a preflight failure has generated nothing and must not remove existing dependencies.
  if (!commit || !tree) throw new CheckError("snapshot dependency install needs the selected commit and tree");
  const base = path.resolve(snapshot);
  try {
    const st = fs.lstatSync(base);
    if (!st.isDirectory() || fs.realpathSync.native(base) !== base) throw new Error("not a real directory");
  } catch (error) { throw new CheckError(`snapshot dependency install needs a real snapshot directory: ${snapshot}: ${error.message}`); }
  if (inside(base, checkout) || inside(checkout, base)) throw new CheckError(`snapshot ${base} and checkout ${checkout} overlap`);
  verifySnapshotDir({ checkout, dir: base, commit, tree, when: "before dependency installation", ...(ledger !== undefined ? { ledger } : {}) });
  const entries = treeEntries(checkout, commit);
  const fold = (s) => process.platform === "darwin" || process.platform === "win32" ? s.toLowerCase() : s;
  const dirOf = (p) => p.includes("/") ? p.slice(0, p.lastIndexOf("/")) : "";
  const lockRoots = entries.filter((e) => e.path === "pnpm-lock.yaml" || e.path.endsWith("/pnpm-lock.yaml")).map((e) => fold(dirOf(e.path))).filter((p) => !p.split("/").includes("node_modules"));
  // The verifier folds names on Mac/Windows. Filesystem operations retain committed spelling even on a case-sensitive Mac volume.
  const generated = [...dependencyDirs(entries, "linux")];
  const generatedFolded = new Set(generated.map(fold));
  const nearestLock = (member) => lockRoots.filter((r) => !r || member === r || member.startsWith(`${r}/`)).sort((a, b) => b.length - a.length)[0];
  const selectedByRoot = new Map();
  if (packageDirs !== undefined) {
    if (!Array.isArray(packageDirs)) throw new CheckError("snapshot dependency packageDirs must be an array of committed manifest directories");
    const manifests = new Map(entries.filter((e) => e.path === "package.json" || e.path.endsWith("/package.json")).map((e) => [fold(dirOf(e.path)), dirOf(e.path)]));
    const installRoots = new Set(roots.map((r) => fold(path.relative(base, path.resolve(base, r)).split(path.sep).join("/"))));
    for (const selected of packageDirs) {
      if (typeof selected !== "string" || path.isAbsolute(selected) || !inside(path.resolve(base, selected), base)) throw new CheckError(`dependency package ${String(selected)} is not relative to the snapshot`);
      const relative = path.relative(base, path.resolve(base, selected)).split(path.sep).join("/");
      const actual = manifests.get(fold(relative));
      const owner = nearestLock(fold(relative));
      if (actual === undefined || owner === undefined || !installRoots.has(owner) || /[*?\[\]{}!^()\\]/.test(actual) || actual.includes("...")) throw new CheckError(`dependency package ${selected} is not an exact committed manifest directory owned by an install root`);
      if (!selectedByRoot.has(owner)) selectedByRoot.set(owner, new Set());
      selectedByRoot.get(owner).add(actual);
    }
  }
  const cleanupFor = (root) => {
    const relative = path.relative(base, path.resolve(base, root)).split(path.sep).join("/");
    const owner = fold(relative);
    if (!inside(path.resolve(base, root), base) || !lockRoots.includes(owner) || !generatedFolded.has(owner ? `${owner}/node_modules` : "node_modules")) throw new CheckError(`root ${root}: not a committed pnpm install root inside the snapshot`);
    const dirs = generated.filter((d) => {
      const member = fold(dirOf(d));
      return nearestLock(member) === owner;
    });
    for (const d of dirs) {
      if (entries.some((e) => fold(e.path) === fold(d) || fold(e.path).startsWith(`${fold(d)}/`))) throw new CheckError(`root ${root}: cannot install over committed dependency path ${d}`);
    }
    // Capture physical source parents before pnpm starts. During cleanup check them from the snapshot down, never traversing a
    // replaced parent or a junction. rmSync unlinks a dependency-directory symlink itself and never follows links within a tree.
    const parents = new Map();
    for (const d of dirs) {
      const parts = d.split("/").slice(0, -1);
      for (let i = 0; i <= parts.length; i++) {
        const p = path.join(base, ...parts.slice(0, i));
        if (!parents.has(p)) {
          let stat;
          try { stat = fs.lstatSync(p); }
          catch (error) { throw new CheckError(`root ${root}: cannot inspect dependency parent ${p}: ${error.message}`); }
          if (!stat.isDirectory()) throw new CheckError(`root ${root}: dependency parent is not a real directory: ${p}`);
          parents.set(p, stat);
        }
      }
    }
    return () => {
      for (const [p, before] of parents) {
        const after = fs.lstatSync(p);
        if (!after.isDirectory() || after.dev !== before.dev || after.ino !== before.ino) throw new Error(`dependency parent changed: ${p}`);
      }
      for (const d of dirs) fs.rmSync(path.join(base, ...d.split("/")), { recursive: true, force: true });
      verifySnapshotDir({ checkout, dir: base, commit, tree, when: "after failed dependency installation cleanup", ...(ledger !== undefined ? { ledger } : {}) });
    };
  };
  const versionOf = (cwd) => { const r = spawn("pnpm", ["--version"], { cwd, env: { ...env, COREPACK_ENABLE_AUTO_PIN: "0" } }); return failed(r) ? null : text(r).trim().split(/\r?\n/).pop(); };
  for (const root of roots) {
    const dir = path.resolve(base, root);
    if (!inside(dir, base)) throw new CheckError(`root ${root}: dependency install is outside the snapshot`);
    // No lockfile: nothing to install from. Not an error here: the pnpm-lockfile scan judges it, and checks that need dependencies fail.
    if (!fs.existsSync(path.join(dir, "pnpm-lock.yaml"))) { records.push({ root, skipped: "no pnpm-lock.yaml in the commit: nothing to install from (the pnpm-lockfile scan judges it)" }); continue; }
    const cleanup = cleanupFor(root);
    const owner = fold(path.relative(base, dir).split(path.sep).join("/"));
    const selected = selectedByRoot.get(owner);
    if (packageDirs !== undefined && !selected?.size) throw new CheckError(`root ${root}: no dependency packages selected; refusing an unfiltered install`);
    const filters = packageDirs === undefined ? [] : [...selected].sort().flatMap((p) => ["--filter", `./${path.relative(dir, path.join(base, p)).split(path.sep).join("/")}`]);
    if (packageDirs !== undefined) filters.push("--fail-if-no-match", "--config.legacy-dir-filtering=false", "--config.include-workspace-root=false");
    const { storeDir, source } = resolvePrimaryStore({ checkout, root, env, spawn, ...(readFile ? { readFile } : {}), ...(exists ? { exists } : {}) });
    const pin = pinnedPnpm(dir) ?? pinnedPnpm(snapshot);
    // Asked where the install runs: pnpm switches itself to the root's packageManager version there (Dell 2026-10-29: an installed
    // 11.24.0 answers 11.24.0 in a neutral directory and runs 11.25.0 in the root), so that is the pnpm that will install.
    const version = versionOf(dir);
    if (!version) throw new CheckError("pnpm is not runnable on this host; the snapshot's dependencies cannot be installed");
    if (pin && version !== pin) throw new CheckError(`pnpm ${version} is on PATH, but root ${root} of the commit pins pnpm@${pin} (packageManager); install the pinned pnpm`);
    const started = now();
    // The virtual store is pinned inside the snapshot and the global virtual store off, whatever the host's pnpm config says: the
    // install writes nowhere but the snapshot (links to the existing store's content are the only reads outside it).
    let r;
    try {
      r = spawn("pnpm", [...filters, "install", "--offline", "--frozen-lockfile", "--ignore-scripts", "--ignore-pnpmfile", "--store-dir", storeDir, "--virtual-store-dir", "node_modules/.pnpm", "--config.enable-global-virtual-store=false", "--reporter", "append-only"], {
        cwd: dir, env: { ...env, COREPACK_ENABLE_AUTO_PIN: "0", CI: "true" }, maxBuffer: 1 << 28,
      });
    } catch (error) { r = { error }; }
    const ms = now() - started;
    if (failed(r) || /No projects matched the filters/.test(`${r?.stdout ?? ""}\n${r?.stderr ?? ""}`)) {
      const out = `${r?.stdout ?? ""}\n${r?.stderr ?? ""}`;
      const miss = /ERR_PNPM_NO_OFFLINE_TARBALL[^\n]*?(https?:\/\/\S+?\/-\/(\S+?)\.tgz)/.exec(out);
      const message = miss
        ? `root ${root}: ${miss[2]} is missing from the store ${storeDir} (offline install, no network fallback); install the checkout's dependencies so the store holds it, then run the check again`
        : `root ${root}: offline dependency install failed (store ${storeDir}): ${tail(r) || r?.error?.message || `exit ${r?.status}`}`;
      try { cleanup(); } catch (error) { throw new CheckError(`${message}; dependency cleanup failed: ${error.message}`); }
      throw new CheckError(`${message}; invalidated generated dependency output for owning lockroot ${root}`);
    }
    const lockfileSha256 = crypto.createHash("sha256").update(fs.readFileSync(path.join(dir, "pnpm-lock.yaml"))).digest("hex");
    const skipped = probe ? probe({ dir, root, names: allowedBuilds(dir), spawn, env }) : [];
    records.push({ root, storeDir, storeSource: source, pnpm: version, pinned: pin, lockfileSha256, ms, allowedBuildsNotRun: skipped });
  }
  return records;
}

/** One line per installed root, for the check's output and evidence. */
export function describeInstall(record) {
  if (record.skipped) return `snapshot dependencies root=${record.root} not installed: ${record.skipped}`;
  return `snapshot dependencies root=${record.root} pnpm=${record.pnpm}${record.pinned ? ` (pinned ${record.pinned})` : ""} store=${record.storeDir} (from ${record.storeSource}) lockfile=sha256:${record.lockfileSha256} ms=${record.ms}${record.allowedBuildsNotRun.length ? ` build-scripts-not-run=${record.allowedBuildsNotRun.map((b) => (b.probe === "none" ? `${b.name}@${b.version}(deferred, not proven here: ${b.result})` : `${b.name}@${b.version}(usable: ${b.probe} -> ${b.result})`)).join(",")}` : ""}`;
}

/** The deterministic-red marker for a failed run, or null when any failure is not one a re-run of the same inputs reproduces. */
export function deterministicMarker(scanResult, env) {
  const failing = scanResult.scans.filter((r) => r.status !== "pass");
  if (!failing.length || !env) return null;
  // A fail row with no code is a scan's own findings (an internal check's list); every other code names why it could not judge.
  const eligible = failing.every((r) => r.status === "fail" && !isLiveScan(r.id) && !isRegistryScan(r.id) && (r.code === undefined || r.code === null || DETERMINISTIC_CODES.has(r.code)));
  return eligible ? `${DETERMINISTIC_MARKER} failed=${failing.map((r) => r.id).join(",")} env=${env}` : null;
}

/** Repository-relative files under `root`: git's view when `root` is a work tree's top level, else a walk (no node_modules, no .git). */
export function rootFiles(root, spawn = defaultSpawn) {
  const top = spawn("git", ["rev-parse", "--show-toplevel"], { cwd: root });
  if (!failed(top) && path.resolve(text(top).trim()) === path.resolve(root)) {
    const r = spawn("git", ["ls-files", "-z", "--cached", "--others", "--exclude-standard"], { cwd: root, maxBuffer: 1 << 30 });
    if (!failed(r)) return text(r).split("\0").filter(Boolean).map((f) => f.split(path.sep).join("/"));
  }
  // No git (a gate-owned snapshot): list what git would, a link as one entry (never followed into), and only real directories
  // entered. A link must land, physically, inside the root on a file; one that leaves it (or dangles) stops the listing rather than
  // silently dropping what it names (a committed apps/web/package.json -> ../../data/web.json is a manifest the audit must see).
  const realRoot = fs.realpathSync.native(root);
  const out = [];
  const walk = (d, rel) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      if (e.name === "node_modules" || e.name === ".git") continue;
      const r = rel ? `${rel}/${e.name}` : e.name;
      const p = path.join(d, e.name);
      if (e.isSymbolicLink()) {
        let real = null;
        try { real = fs.realpathSync.native(p); } catch { real = null; }
        if (!real || !inside(real, realRoot)) throw new CheckError(`${r} is a link that does not land inside ${root}; the live scans cannot list the tree safely`);
        if (fs.statSync(real).isFile()) out.push(r);
      } else if (e.isDirectory()) walk(p, r);
      else if (e.isFile()) out.push(r);
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
    // A tree with a pnpm lockfile always owes a live audit: planning none there is a listing or planning failure, never an empty pass.
    const lockfiles = files.filter((f) => /(^|\/)pnpm-lock\.yaml$/.test(f));
    if (lockfiles.length && !live.scans.length) throw new Error(`the tree holds ${lockfiles.join(", ")} but no live scan was planned; refusing to answer an empty refresh`);
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
