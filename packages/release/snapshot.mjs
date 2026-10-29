// The gate-owned commit snapshot (docs/GATE-CONTRACT.md §1 "Commit-only build" and "Broker interface"; rightkit#1254, #1255).
//
// A build compiles a selected COMMIT, never the editable checkout. The gate keeps one persistent source directory per repository
// under the managed root and syncs the commit into it by content: only changed bytes are written, an unchanged file keeps its
// mtime (so Cargo's fingerprints stay warm), files the commit no longer has are removed, tracked file types & modes are kept, and
// no link resolves into the checkout. Outputs & caches stay where they already are; only the source is separate.
//
// The snapshot's identity is the contract's §1 method, seeded per rightkit#1255 so it agrees with the broker's `snapshot_tree_of`:
// a temporary index in the checkout's git directory starts as the commit's tree (`read-tree <sha>^{tree}`), every file the
// snapshot holds is staged from it (`--work-tree=<snapshot> add -A`) against the checkout's object database, then `write-tree`.
// A committed file an ignore rule also matches stays in the id; untracked files an ignore rule excludes (build outputs) do not.
// The checkout's own index, HEAD, refs and files are never touched; only objects are written.
//
// Roots: Mac & Dell `<managed root>/snapshots/<repo-key>` with the managed root the host's fixed one (managed-roots.mjs legacyRoot:
// `/Volumes/D/.rightkit-managed`, `D:\.rightkit-managed`), never an environment variable; Linux `$RIGHTKIT_SNAPSHOT_ROOT/<repo-key>`
// when configured, else `$XDG_STATE_HOME/rightkit/snapshots/<repo-key>`, else `~/.local/state/rightkit/snapshots/<repo-key>`.
// `repo-key` = first 16 hex of sha256(canonical repository path), the key `checked-trees` uses. Scratch & temp are never the root
// (they are disposable): a resolved root under a disk-policy scratch root or a temp directory is refused on every host. In-process
// callers inject a root as a parameter (`snapshotRoot` of `snapshotCommand`'s and the gate's `o`); no environment variable relocates
// the root on Mac & Dell, and on Linux only the configured RIGHTKIT_SNAPSHOT_ROOT / XDG state do.
//
// Exclusion: one sync per snapshot at a time (`<root>/<repo-key>.lock`, re-checked before every write and before the verdict).
// It narrows, but cannot close, the window in which two processes both replace one stale lock; host-wide build ownership held
// from before the sync through the stage (joint plan step 4) is the exclusion builds rely on, and the broker re-verifies the tree.

import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { readDiskPolicy, scratchRoots } from "./disk-policy.mjs";
import { resolveManagedRoots } from "./managed-roots.mjs";
import { acquireBuildOwnership, migrateBirthlessRoot, OwnershipBusy, ownershipFile, recordRootBirth } from "./build-ownership.mjs";

export const SNAPSHOT_SCHEMA = "rightkit.snapshot/1";
const STATE_SCHEMA = "rightkit.snapshot-state/1";
const FILE_MODES = new Set(["100644", "100755"]);
const LINK_MODE = "120000";
const GIT_SAFE = ["-c", "core.fsmonitor=false", "-c", "core.untrackedCache=false"];
const MAX_BUFFER = 1 << 30;
const BLOB_CHUNK = 500;

export class SnapshotError extends Error {
  /** `code` is "mismatch" (the snapshot is not the commit's tree), "busy" (another sync holds it), "refused" (the commit cannot be
   * materialised exactly: a gitlink, an unsafe path or a link out of the snapshot) or "error" (could not run). */
  constructor(code, message) { super(message); this.code = code; }
}

/** First 16 hex digits of sha256 over the canonical repository path: the key RustRight's `checked-trees` & journals use. */
export function repoKey(canonicalRepoPath) {
  return crypto.createHash("sha256").update(String(canonicalRepoPath)).digest("hex").slice(0, 16);
}

/**
 * The repository path the key is taken over, spelled as RustRight's `fs::canonicalize` spells it, so the snapshot's key equals
 * the broker's `checked-trees` key on every host: unchanged on Mac & Linux, the verbatim `\\?\` form on Windows
 * (Dell 2026-10-29: `checked-trees/226c20f30c489153` = sha256 of `\\?\D:\Claude\rightkit`).
 */
export function keyPath(canonicalRepoPath, platform = process.platform) {
  const p = String(canonicalRepoPath);
  if (platform !== "win32" || p.startsWith("\\\\?\\")) return p;
  return p.startsWith("\\\\") ? `\\\\?\\UNC\\${p.slice(2)}` : `\\\\?\\${p}`;
}

/** Whether `child` is `parent` or lies under it, by `paths`' relative path: outside only when it is exactly `..`, starts with `..` + the
 * separator, or is absolute (a sibling named `..x` is inside). */
function isInside(child, parent, paths = path) {
  const rel = paths.relative(parent, child);
  return rel === "" || (rel !== ".." && !rel.startsWith(`..${paths.sep}`) && !paths.isAbsolute(rel));
}

/** `p` with every link on its existing part resolved (the rest as written), spelled for `paths` (path.posix or path.win32). */
function canonicalPath(p, paths) {
  // A path spelled for another platform than this host's (a simulated host) names nothing on this disk: it stays as written.
  if (paths !== (process.platform === "win32" ? path.win32 : path.posix)) return paths.resolve(p);
  const rest = [];
  let at = paths.resolve(p);
  for (;;) {
    try { return paths.join(fs.realpathSync.native(at), ...rest.reverse()); }
    catch {
      const up = paths.dirname(at);
      if (up === at) return paths.resolve(p);
      rest.push(paths.basename(at));
      at = up;
    }
  }
}

/** Throws SnapshotError("error") when `root` lies under a disk-policy scratch root or a temp directory: a snapshot is durable. */
function refuseDisposableRoot(root, source, { platform, env }) {
  const paths = platform === "win32" ? path.win32 : path.posix;
  const within = (child, parent) => isInside(child, parent, paths);
  let scratch;
  try {
    const policy = readDiskPolicy();
    scratch = ["darwin", "win32"].flatMap((p) => scratchRoots(policy, { platform: p, env, home: os.homedir() }));
  } catch (e) { throw new SnapshotError("error", `cannot read the disk policy that names the scratch roots, so snapshot root ${root} (from ${source}) cannot be checked: ${e.message}`); }
  const temps = [os.tmpdir(), "/tmp", "/var/tmp", "/private/tmp", "/var/folders", "/private/var/folders", env.TMPDIR, env.TEMP, env.TMP];
  const real = canonicalPath(root, paths);
  const candidates = [...scratch.map((d) => ["scratch root", d]), ...temps.filter((d) => typeof d === "string" && d).map((d) => ["temp directory", d])];
  for (const [kind, dir] of candidates) {
    const canon = canonicalPath(dir, paths);
    if (paths.dirname(canon) === canon || paths.dirname(paths.resolve(dir)) === paths.resolve(dir)) continue; // a filesystem root is no scratch or temp directory (a TMPDIR of "/" must not refuse everything)
    if (within(real, canon) || within(paths.resolve(root), paths.resolve(dir))) throw new SnapshotError("error", `snapshot root ${root} (from ${source}) is under the ${kind} ${dir}: a snapshot is durable, and scratch & temp are disposable (docs/GATE-CONTRACT.md §1)`);
  }
}

/**
 * The directory every snapshot of this host lives under. Mac & Dell: `<managed root>/snapshots` with the host's fixed managed root,
 * never an environment variable (RIGHTKIT_DATA_ROOT & friends move build outputs, not snapshots). Linux: RIGHTKIT_SNAPSHOT_ROOT when
 * configured and absolute, else XDG state. The result is refused when it lies under scratch or temp, on every host. Callers that need
 * another root (tests) pass it as a parameter; nothing here reads a variable that moves the root.
 */
export function snapshotRoot({ platform = process.platform, env = process.env, home = os.homedir() } = {}) {
  const paths = platform === "win32" ? path.win32 : path.posix;
  let root; let source;
  if (platform === "darwin" || platform === "win32") {
    const base = resolveManagedRoots({ env: {}, platform, home }).dataRoot;
    root = paths.join(base, "snapshots");
    source = `the host's managed root ${base}`;
  } else if (env.RIGHTKIT_SNAPSHOT_ROOT && paths.isAbsolute(env.RIGHTKIT_SNAPSHOT_ROOT)) {
    root = env.RIGHTKIT_SNAPSHOT_ROOT;
    source = "RIGHTKIT_SNAPSHOT_ROOT";
  } else {
    const fromXdg = env.XDG_STATE_HOME && paths.isAbsolute(env.XDG_STATE_HOME);
    root = paths.join(fromXdg ? env.XDG_STATE_HOME : paths.join(home, ".local", "state"), "rightkit", "snapshots");
    source = fromXdg ? "XDG_STATE_HOME" : `the home directory ${home}`;
  }
  refuseDisposableRoot(root, source, { platform, env });
  return root;
}

function runGit(cwd, args, { env, input, binary = false } = {}) {
  const r = spawnSync("git", args, { cwd, env: env ? { ...process.env, ...env } : process.env, input, maxBuffer: MAX_BUFFER, ...(binary ? {} : { encoding: "utf8" }) });
  if (r.error || r.status !== 0) {
    const why = r.error ? r.error.message : String(r.stderr ?? "").trim() || `exit ${r.status}`;
    throw new SnapshotError("error", `git ${args.filter((a) => a !== "-c" && !a.startsWith("core.")).join(" ")}: ${why}`);
  }
  return r.stdout;
}

/** The repository a checkout belongs to: canonical top level, absolute git directory and object format. */
export function repositoryOf(checkout) {
  const top = runGit(checkout, ["rev-parse", "--show-toplevel"]).trim();
  const canonical = fs.realpathSync.native(top);
  const gitDir = runGit(canonical, ["rev-parse", "--absolute-git-dir"]).trim();
  const objectFormat = runGit(canonical, ["rev-parse", "--show-object-format"]).trim() || "sha1";
  return { checkout: canonical, gitDir, objectFormat, key: repoKey(keyPath(canonical)) };
}

/** `{ commit, tree }` of a revision; a revision that is not a commit is an error, never a guess. */
export function resolveCommit(checkout, rev = "HEAD") {
  const commit = runGit(checkout, ["rev-parse", "--verify", "--quiet", `${rev}^{commit}`]).trim();
  const tree = runGit(checkout, ["rev-parse", "--verify", "--quiet", `${commit}^{tree}`]).trim();
  return { commit, tree };
}

/**
 * The tree id of `dir` by the contract's §1 method, seeded from `baseTree` (rightkit#1255). Reads nothing of the checkout but its
 * object database and ignore rules; writes only objects and a temporary index that is always removed.
 */
export function snapshotTree({ checkout, dir, baseTree, repo = repositoryOf(checkout) }) {
  const index = path.join(repo.gitDir, `rightkit-snapshot-${crypto.randomUUID()}.index`);
  const env = { GIT_INDEX_FILE: index };
  const git = (...a) => runGit(dir, [...GIT_SAFE, `--git-dir=${repo.gitDir}`, `--work-tree=${dir}`, ...a], { env });
  try {
    git("read-tree", baseTree);
    git("add", "-A");
    const id = git("write-tree").trim();
    if (!/^[0-9a-f]{40}([0-9a-f]{24})?$/.test(id)) throw new SnapshotError("error", `git write-tree printed ${JSON.stringify(id)}`);
    return id;
  } finally {
    for (const f of [index, `${index}.lock`]) fs.rmSync(f, { force: true });
  }
}

/**
 * Removes every entry under `dir` the commit does not hold: untracked files whether an ignore rule matches them or not, leftover
 * directories, links that are not committed. §1 makes the snapshot exactly the commit; a stale ignored file is unattested build
 * input even though the seeded tree id cannot see it. Never follows a link (only real directories are entered); names compare
 * case-insensitively where the host filesystem does (Mac, Windows; colliding names were refused before this runs).
 */
function pruneToTree(dir, entries, platform = process.platform) {
  const fold = platform === "darwin" || platform === "win32" ? (n) => n.toLowerCase() : (n) => n;
  const files = new Set(entries.map((e) => fold(e.path)));
  const dirs = new Set();
  for (const e of entries) { const parts = e.path.split("/"); for (let i = 1; i < parts.length; i++) dirs.add(fold(parts.slice(0, i).join("/"))); }
  let removed = 0;
  const walk = (d, rel) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const r = rel ? `${rel}/${e.name}` : e.name;
      const p = path.join(d, e.name);
      if (e.isDirectory()) { if (dirs.has(fold(r))) walk(p, r); else { fs.rmSync(p, { recursive: true, force: true }); removed++; } }
      else if (!files.has(fold(r))) { fs.unlinkSync(p); removed++; }
    }
  };
  walk(dir, "");
  return removed;
}

/** Every entry of the commit's tree: `{ mode, oid, path }`, path in git's `/` form. Gitlinks & unsafe paths refuse. */
export function treeEntries(checkout, commit) {
  const out = runGit(checkout, ["ls-tree", "-r", "-z", "--full-tree", commit]);
  const entries = [];
  for (const record of out.split("\0").filter(Boolean)) {
    const tab = record.indexOf("\t");
    const [mode, type, oid] = record.slice(0, tab).split(" ");
    const file = record.slice(tab + 1);
    const parts = file.split("/");
    if (!file || file.startsWith("/") || parts.some((p) => !p || p === "." || p === ".." || p.toLowerCase() === ".git") || /[\\:]/.test(file) && process.platform === "win32") {
      throw new SnapshotError("refused", `commit ${commit} has a path a snapshot cannot hold exactly: ${JSON.stringify(file)}`);
    }
    if (type === "commit" || mode === "160000") throw new SnapshotError("refused", `commit ${commit} has a gitlink at ${file}: a snapshot materialises files, not nested repositories`);
    if (type !== "blob" || !(FILE_MODES.has(mode) || mode === LINK_MODE)) throw new SnapshotError("refused", `commit ${commit} has ${type} ${mode} at ${file}`);
    entries.push({ mode, oid, path: file });
  }
  return entries;
}

/** On Mac & Windows (case-insensitive by default) two tree paths, or a path and a directory, that differ only in case cannot both
 * exist: one would replace the other, and a link could redirect a write. Refused before anything is written. */
function refuseCaseCollisions(entries, commit, platform = process.platform) {
  if (platform !== "darwin" && platform !== "win32") return;
  const seen = new Map();
  for (const e of entries) {
    const parts = e.path.split("/");
    for (let i = 1; i <= parts.length; i++) {
      const p = parts.slice(0, i).join("/");
      const k = p.toLowerCase();
      const other = seen.get(k);
      if (other !== undefined && other !== p) throw new SnapshotError("refused", `commit ${commit} has ${JSON.stringify(other)} and ${JSON.stringify(p)}, which differ only in case`);
      seen.set(k, p);
    }
  }
}

/**
 * The ONLY entries a snapshot may hold beyond its commit's tree (GATE-CONTRACT §1 "exact snapshot"; E6 "plant a stale file, a link").
 * Build outputs are never among them: the snapshot holds no target/, the executor writes to the checkout's own output directory.
 * Two narrow, named allowances remain, each a thing the gate itself puts there:
 *   1. pnpm dependency directories: a `node_modules` directly inside a directory that holds a committed `package.json` and sits at or
 *      under a directory holding a committed `pnpm-lock.yaml` (snapshot-check.mjs installSnapshotDependencies installs only where a
 *      lockfile exists, into each root and its workspace members). Nothing else ignored by git is allowed, and a link inside such a
 *      directory is still held to the no-escape rule.
 *   2. the config-only `.git/config` of `[remote "origin"]\n\turl = <url>\n` that `check --snapshot` writes for rustright's grant
 *      binding (rightkit#1215) and removes when the check ends: `.git` holding exactly that one file and nothing else.
 */
const foldFor = (platform) => (platform === "darwin" || platform === "win32" ? (n) => n.toLowerCase() : (n) => n);
const GIT_CONFIG_ONLY = /^\[remote "origin"\]\n\turl = [^\r\n]*\n$/;

/** Folded relative paths of the `node_modules` directories allowance 1 admits, derived from the commit's own entries. */
export function dependencyDirs(entries, platform = process.platform) {
  const fold = foldFor(platform);
  const dirOf = (p) => { const i = p.lastIndexOf("/"); return i < 0 ? "" : p.slice(0, i); };
  const holding = (name) => entries.filter((e) => e.path === name || e.path.endsWith(`/${name}`)).map((e) => dirOf(e.path)).filter((d) => !d.split("/").includes("node_modules"));
  const lockRoots = holding("pnpm-lock.yaml");
  const out = new Set();
  for (const member of holding("package.json")) {
    if (lockRoots.some((r) => r === "" || member === r || member.startsWith(`${r}/`))) out.add(fold(member ? `${member}/node_modules` : "node_modules"));
  }
  return out;
}

/**
 * What stops `dir` being exactly the commit's tree that the seeded tree id cannot see (it ignores what an ignore rule matches and
 * follows no link): the first path under `dir` that is not in `entries` (ignored or not, empty directories included) and not one of
 * the two allowances above, or the first link (committed or not) that does not resolve, physically, to something inside `dir` and
 * outside `checkout`. Never follows a link to enter it. Returns a message naming the path, or null.
 */
export function snapshotProblem({ dir, entries, checkout, platform = process.platform }) {
  const fold = foldFor(platform);
  const files = new Set(entries.map((e) => fold(e.path)));
  const dirs = new Set();
  for (const e of entries) { const parts = e.path.split("/"); for (let i = 1; i < parts.length; i++) dirs.add(fold(parts.slice(0, i).join("/"))); }
  const deps = dependencyDirs(entries, platform);
  let realDir; let realCheckout = checkout;
  try { realDir = fs.realpathSync.native(dir); } catch (e) { return `${dir} cannot be resolved: ${e.message}`; }
  try { realCheckout = fs.realpathSync.native(checkout); } catch { /* the path as given */ }
  const linkProblem = (rel, p) => {
    let real;
    try { real = fs.realpathSync.native(p); } catch { return `${rel} is a link that does not resolve (dangling or looping); it cannot be proven to stay inside the snapshot`; }
    if (inside(real, realCheckout)) return `${rel} is a link resolving into the editable checkout (${real})`;
    if (!inside(real, realDir)) return `${rel} is a link resolving outside the snapshot (${real})`;
    return null;
  };
  const gitConfigProblem = (rel, p) => {
    const names = fs.readdirSync(p);
    if (names.length !== 1 || names[0] !== "config") return `${rel} is not in the commit's tree (a .git directory may hold only the gate's config-only origin file)`;
    const config = path.join(p, "config");
    const s = lstat(config);
    if (!s?.isFile() || !GIT_CONFIG_ONLY.test(fs.readFileSync(config, "utf8"))) return `${rel}/config is not in the commit's tree (it may hold only the gate's remote "origin" url)`;
    return null;
  };
  const walk = (d, rel, inDeps) => {
    const names = fs.readdirSync(d, { withFileTypes: true }).sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
    for (const e of names) {
      const r = rel ? `${rel}/${e.name}` : e.name;
      const p = path.join(d, e.name);
      const key = fold(r);
      let found = null;
      if (e.isSymbolicLink()) {
        // The landing is named first when it is the worse news (into the checkout, out of the snapshot); a link the commit lacks is refused either way.
        found = linkProblem(r, p);
        if (!found && !inDeps && !files.has(key)) found = `${r} is not in the commit's tree (a link the commit does not have)`;
      } else if (e.isDirectory()) {
        if (inDeps) found = walk(p, r, true);
        else if (deps.has(key)) found = walk(p, r, true);
        else if (dirs.has(key)) found = walk(p, r, false);
        else if (key === ".git") found = gitConfigProblem(r, p);
        else found = `${r} is not in the commit's tree (a directory the commit does not have)`;
      } else if (!inDeps && !files.has(key)) found = `${r} is not in the commit's tree`;
      if (found) return found;
    }
    return null;
  };
  return walk(dir, "", false);
}

function blobId(content, objectFormat) {
  const h = crypto.createHash(objectFormat === "sha256" ? "sha256" : "sha1");
  h.update(`blob ${content.length}\0`);
  h.update(content);
  return h.digest("hex");
}

/** Blob contents for `oids` via `git cat-file --batch`, in bounded chunks. */
function readBlobs(checkout, oids) {
  const blobs = new Map();
  const unique = [...new Set(oids)];
  for (let i = 0; i < unique.length; i += 2000) {
    const chunk = unique.slice(i, i + 2000);
    const out = runGit(checkout, ["cat-file", "--batch"], { input: `${chunk.join("\n")}\n`, binary: true });
    let at = 0;
    for (const oid of chunk) {
      const nl = out.indexOf(0x0a, at);
      const header = out.subarray(at, nl).toString("utf8").split(" ");
      if (header[0] !== oid || header[1] !== "blob") throw new SnapshotError("error", `git cat-file answered ${JSON.stringify(header.join(" "))} for ${oid}`);
      const size = Number(header[2]);
      blobs.set(oid, out.subarray(nl + 1, nl + 1 + size));
      at = nl + 1 + size + 1;
    }
  }
  return blobs;
}

const inside = (child, parent) => isInside(child, parent, path);
const lstat = (p) => { try { return fs.lstatSync(p, { bigint: true }); } catch (e) { if (e.code === "ENOENT" || e.code === "ENOTDIR") return null; throw e; } };
const statKey = (s) => `${s.size}:${s.mtimeNs}:${s.ctimeNs}:${s.ino}`;

/**
 * Exclusive per-snapshot lock. A process holds it only by publishing `lockFile` exclusively, and the lock appears whole: its body
 * is written to a private file first and hard-linked into place (`link` fails if the name exists), so no lock is ever observable
 * empty or partial and every lock carries a random token. A lock whose holder process is gone on this host is stale; reclaiming
 * it is serialised by a second exclusive file, `<lockFile>.reclaim`: the one reclaimer re-reads the lock under it, and removes it
 * only if it is still byte-for-byte the stale lock it judged (a dead holder cannot release, no other reclaimer can act, and a
 * newer lock always differs by its token), then competes for the lock by the same exclusive publish as everyone else. A live
 * lock is never moved or removed by anyone but its holder, so two processes never both hold it. Busy, never reclaimed: a live
 * holder, another host's lock, a lock that cannot be parsed (left by a writer of another kind or a damaged disk), a reclaim in
 * progress. A reclaimer that died mid-reclaim leaves the reclaim file and every later sync is busy until it is removed (fail
 * closed); so does a dead holder whose pid was reused. `hooks` exist for the deterministic race fixture only.
 */
export function acquireSnapshotLock(lockFile, hooks = {}) {
  const body = JSON.stringify({ pid: process.pid, host: os.hostname(), startedAt: new Date().toISOString(), token: crypto.randomUUID() });
  const held = () => { try { return fs.readFileSync(lockFile, "utf8") === body; } catch { return false; } };
  const busy = (why) => new SnapshotError("busy", why);
  const publish = (target, text) => {
    const tmp = `${target}.${crypto.randomUUID()}.tmp`;
    fs.writeFileSync(tmp, text, { flag: "wx", mode: 0o600 });
    try { fs.linkSync(tmp, target); return true; }
    catch (e) { if (e.code === "EEXIST") return false; throw new SnapshotError("error", `cannot create ${target}: ${e.message}`); }
    finally { fs.rmSync(tmp, { force: true }); }
  };
  const create = () => {
    if (!publish(lockFile, body)) return false;
    if (!held()) throw busy(`snapshot lock ${lockFile} was replaced by another process`);
    const release = () => { if (held()) fs.rmSync(lockFile, { force: true }); };
    release.held = held;
    return release;
  };
  const read = () => { try { return fs.readFileSync(lockFile, "utf8"); } catch (e) { if (e.code === "ENOENT") return null; throw new SnapshotError("error", `cannot read ${lockFile}: ${e.message}`); } };
  const judgeStale = (observed) => {
    let holder = null;
    try { holder = JSON.parse(observed); } catch { holder = null; }
    // Locks appear whole, so one that cannot be parsed was not written by this protocol: never reclaimed automatically.
    if (!holder || !Number.isInteger(holder.pid) || holder.pid <= 0 || holder.pid > 0x7fffffff || typeof holder.host !== "string" || !holder.host) throw busy(`snapshot lock ${lockFile} cannot be read as a lock; remove it only if no sync is running`);
    if (holder.host !== os.hostname()) throw busy(`snapshot is held by pid ${holder.pid} on ${holder.host} since ${holder.startedAt}`);
    // Only a holder proven gone (ESRCH) is stale; any other answer, EPERM included, leaves the lock alone.
    let probe = null;
    try { process.kill(holder.pid, 0); } catch (k) { probe = k.code ?? String(k); }
    if (probe === null) throw busy(`snapshot is held by pid ${holder.pid} since ${holder.startedAt}`);
    if (probe !== "ESRCH") throw busy(`snapshot lock holder pid ${holder.pid} cannot be proven gone (${probe})`);
  };
  const first = create();
  if (first) return first;
  const observed = read();
  if (observed === null) { const again = create(); if (again) return again; throw busy(`snapshot lock ${lockFile} was taken by another process`); }
  judgeStale(observed);
  hooks.afterStaleRead?.();
  const reclaim = `${lockFile}.reclaim`;
  if (!publish(reclaim, body)) throw busy(`snapshot lock ${lockFile} is being reclaimed by another process (remove ${reclaim} only if no sync is running)`);
  try {
    hooks.whileReclaiming?.();
    if (read() !== observed) throw busy(`snapshot lock ${lockFile} changed before it could be reclaimed`);
    fs.rmSync(lockFile, { force: true });
    const mine = create();
    if (!mine) throw busy(`snapshot lock ${lockFile} was taken by another process`);
    return mine;
  } finally {
    fs.rmSync(reclaim, { force: true });
  }
}

function readState(file) {
  try { const s = JSON.parse(fs.readFileSync(file, "utf8")); return s?.schema === STATE_SCHEMA ? s : null; } catch { return null; }
}
function writeState(file, state) {
  const tmp = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(state), { mode: 0o600 });
  fs.renameSync(tmp, file);
}

/** A link's target as git records it: Windows reports `..\\x` for a link made from `../x`, so separators compare as `/` there. */
const linkText = (text) => (process.platform === "win32" ? text.replace(/\\/g, "/") : text);

/** Removes `target` without following links: a link or file is unlinked, a directory removed with its contents. */
function removeEntry(target) {
  const s = lstat(target);
  if (!s) return false;
  if (s.isDirectory()) fs.rmSync(target, { recursive: true, force: true }); else fs.unlinkSync(target);
  return true;
}

/** Every link under `dir`, never followed (a Windows junction is a link here too). */
function linksUnder(dir) {
  const found = [];
  const walk = (d) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      if (e.isSymbolicLink()) found.push(p);
      else if (e.isDirectory()) walk(p);
    }
  };
  walk(dir);
  return found;
}

/** Makes every ancestor of `file` below `dir` a real directory: a file or link in the way is removed, never written through. */
function ensureParents(dir, file) {
  let at = dir;
  for (const part of path.relative(dir, path.dirname(file)).split(path.sep).filter(Boolean)) {
    at = path.join(at, part);
    const s = lstat(at);
    if (s?.isDirectory()) continue;
    if (s) removeEntry(at);
    fs.mkdirSync(at, { mode: 0o755 });
  }
}

/**
 * Syncs commit `rev` of the repository at `checkout` into its snapshot directory and verifies the result is exactly the commit's
 * tree. Returns `{ schema, path, repoKey, commit, tree, written, removed, modesFixed, unchanged, linksRemoved, ms }`.
 * Throws SnapshotError: "refused" before anything is written when the commit cannot be held exactly, "busy" while another sync
 * holds the snapshot, "mismatch" when the result is not the commit's tree, "error" when git or the filesystem fails.
 */
export function syncSnapshot({ checkout, rev = "HEAD", root = snapshotRoot(), now = () => Date.now() } = {}) {
  const started = now();
  const repo = repositoryOf(checkout);
  const { commit, tree } = resolveCommit(repo.checkout, rev);
  const entries = treeEntries(repo.checkout, commit);
  refuseCaseCollisions(entries, commit);
  // mkdir returns the first directory it created: a root that did not exist is born now, and build ownership keeps that birth (and
  // the boot it happened in) as the baseline for processes it cannot read.
  // A root made before birth records existed is emptied and born anew, only under this process's own free-host ownership, before
  // this sync's first write (build-ownership.mjs migrateBirthlessRoot).
  migrateBirthlessRoot(root);
  if (fs.mkdirSync(root, { recursive: true, mode: 0o700 }) !== undefined) recordRootBirth(root);
  const rootStat = lstat(root);
  if (!rootStat?.isDirectory()) throw new SnapshotError("refused", `snapshot root ${root} is not a directory (a link is never followed)`);
  const dir = path.join(root, repo.key);
  if (inside(dir, repo.checkout) || inside(repo.checkout, dir)) throw new SnapshotError("refused", `snapshot ${dir} and checkout ${repo.checkout} overlap`);
  const release = acquireSnapshotLock(`${dir}.lock`);
  const assertLockHeld = () => { if (!release.held()) throw new SnapshotError("busy", `snapshot lock ${dir}.lock is no longer held by this sync`); };
  try {
    const existing = lstat(dir);
    if (existing && !existing.isDirectory()) removeEntry(dir);
    fs.mkdirSync(dir, { recursive: true, mode: 0o755 });
    const stateFile = `${dir}.state.json`;
    const prior = readState(stateFile);
    const wanted = new Map(entries.map((e) => [e.path, e]));
    const cache = prior?.stat ?? {};
    let removed = 0, written = 0, modesFixed = 0, unchanged = 0, linksRemoved = 0;

    // Where a path physically lands, following every link on the way: `path.resolve` collapses `..` lexically and would miss a
    // chain such as `c/b -> ..` that the operating system follows (Oracle, step 1 review).
    const realDir = fs.realpathSync.native(dir);
    if (inside(realDir, repo.checkout) || inside(repo.checkout, realDir)) throw new SnapshotError("refused", `snapshot ${realDir} and checkout ${repo.checkout} overlap`);
    const landsSafely = (p) => { let real; try { real = fs.realpathSync.native(p); } catch { return false; } return inside(real, realDir) && !inside(real, repo.checkout); };
    const parentSafe = (target) => {
      if (!landsSafely(path.dirname(target))) throw new SnapshotError("refused", `${path.relative(dir, target)}: its directory resolves out of the snapshot; nothing is written through it`);
    };

    const sweepLinks = (keep = new Set()) => {
      for (const link of linksUnder(dir)) if (!keep.has(link) && !landsSafely(link)) { removeEntry(link); linksRemoved++; }
    };

    // 1. A link already here (planted, or from an earlier sync) stays only if it lands inside the snapshot and not in the checkout.
    // This runs before anything is removed or written, so no later step can pass through such a link. Nothing is deleted by a
    // path that crosses a link: stale entries go only in the final walk, which enters real directories alone (Oracle, re-review).
    assertLockHeld();
    sweepLinks();

    // 3. Every tracked file: unchanged ones are left alone (same bytes & mode keep their mtime), the rest are written. Links are
    // created only after every file, so creating one can never redirect a later write.
    const need = [];
    const links = [];
    const nextStat = {};
    for (const e of entries) {
      const target = path.join(dir, ...e.path.split("/"));
      if (e.mode === LINK_MODE) { links.push(e); continue; }
      ensureParents(dir, target);
      const s = lstat(target);
      if (s?.isFile()) {
        const known = cache[e.path];
        const same = known && known.oid === e.oid && known.stat === statKey(s) ? true : blobId(fs.readFileSync(target), repo.objectFormat) === e.oid;
        if (same) {
          const exec = (Number(s.mode) & 0o111) !== 0;
          if (process.platform !== "win32" && exec !== (e.mode === "100755")) { fs.chmodSync(target, e.mode === "100755" ? 0o755 : 0o644); modesFixed++; }
          else unchanged++;
          nextStat[e.path] = { oid: e.oid, stat: statKey(lstat(target)) };
          continue;
        }
      }
      need.push(e);
    }
    // Writes land in this sync's own staging directory on the same volume, then rename into place: an interrupted sync never
    // leaves a half-written or stray file inside the snapshot. Blobs are read and written a chunk at a time.
    const stagingRoot = `${dir}.staging`;
    const staging = path.join(stagingRoot, crypto.randomUUID());
    fs.mkdirSync(staging, { recursive: true, mode: 0o700 });
    try {
      let serial = 0;
      for (let i = 0; i < need.length; i += BLOB_CHUNK) {
        const chunk = need.slice(i, i + BLOB_CHUNK);
        const blobs = readBlobs(repo.checkout, chunk.map((e) => e.oid));
        for (const e of chunk) {
          assertLockHeld();
          const target = path.join(dir, ...e.path.split("/"));
          ensureParents(dir, target);
          parentSafe(target);
          const s = lstat(target);
          if (s && !s.isFile()) removeEntry(target);
          const tmp = path.join(staging, String(serial++));
          fs.writeFileSync(tmp, blobs.get(e.oid), { mode: e.mode === "100755" ? 0o755 : 0o644 });
          if (process.platform !== "win32") fs.chmodSync(tmp, e.mode === "100755" ? 0o755 : 0o644);
          fs.renameSync(tmp, target);
          nextStat[e.path] = { oid: e.oid, stat: statKey(lstat(target)) };
          written++;
        }
      }
      assertLockHeld();
      const linkBlobs = readBlobs(repo.checkout, links.map((e) => e.oid));
      for (const e of links) {
        const target = path.join(dir, ...e.path.split("/"));
        const to = linkBlobs.get(e.oid).toString("utf8");
        if (path.isAbsolute(to) || path.win32.isAbsolute(to)) throw new SnapshotError("refused", `commit ${commit} links ${e.path} -> ${to}: an absolute link cannot stay inside the snapshot`);
        ensureParents(dir, target);
        parentSafe(target);
        const s = lstat(target);
        if (s?.isSymbolicLink() && linkText(fs.readlinkSync(target)) === to) { unchanged++; continue; }
        if (s) removeEntry(target);
        try { fs.symlinkSync(to, target); }
        catch (err) { throw new SnapshotError("error", `cannot create the link ${e.path} -> ${to}: ${err.code ?? err.message}${process.platform === "win32" ? " (Windows creates symbolic links only with Developer Mode or elevation; a commit holding a link cannot be synced without it)" : ""}`); }
        written++;
      }
      // Every committed link must land, physically, on something inside the snapshot that is not in the checkout. One that does
      // not (an escape through a chain, a dangling or looping link) is removed before the refusal, so it is never left in place.
      const unsafe = links.map((e) => e.path).filter((p) => !landsSafely(path.join(dir, ...p.split("/"))));
      if (unsafe.length) {
        for (const p of unsafe) removeEntry(path.join(dir, ...p.split("/")));
        throw new SnapshotError("refused", `commit ${commit} has links that do not land inside the snapshot: ${unsafe.join(", ")}`);
      }
      // A planted link can resolve differently once the committed links exist: sweep the others again.
      sweepLinks(new Set(links.map((e) => path.join(dir, ...e.path.split("/")))));
    } finally {
      fs.rmSync(staging, { recursive: true, force: true });
      try { fs.rmdirSync(stagingRoot); } catch { /* another sync's staging, or not empty */ }
    }

    // 4. Everything the commit does not hold is removed: the snapshot is exactly the commit (§1).
    assertLockHeld();
    removed += pruneToTree(dir, entries);

    assertLockHeld();
    const extra = snapshotProblem({ dir, entries, checkout: repo.checkout });
    if (extra) throw new SnapshotError("mismatch", `snapshot ${dir} is not exactly commit ${commit} after sync: ${extra}`);
    const actual = snapshotTree({ checkout: repo.checkout, dir, baseTree: tree, repo });
    if (actual !== tree) throw new SnapshotError("mismatch", `snapshot ${dir} is tree ${actual} after sync, not tree ${tree} of commit ${commit}`);
    writeState(stateFile, { schema: STATE_SCHEMA, repo: repo.checkout, repoKey: repo.key, commit, tree, paths: [...wanted.keys()], stat: nextStat, syncedAt: new Date(now()).toISOString() });
    return { schema: SNAPSHOT_SCHEMA, path: dir, repoKey: repo.key, commit, tree, written, removed, modesFixed, unchanged, linksRemoved, ms: now() - started };
  } finally {
    release();
  }
}

/** Verifies, without changing anything, that the snapshot of `checkout` holds exactly the tree of `rev`: its seeded tree id, no path the
 * commit lacks (ignored files included) and no link resolving out of the snapshot or into the checkout. */
export function verifySnapshot({ checkout, rev = "HEAD", root = snapshotRoot(), dir } = {}) {
  const repo = repositoryOf(checkout);
  const { commit, tree } = resolveCommit(repo.checkout, rev);
  const at = dir ?? path.join(root, repo.key);
  if (!lstat(at)?.isDirectory()) throw new SnapshotError("mismatch", `no snapshot directory at ${at}`);
  // The seeded tree id cannot see an ignored file or follow a link, so the directory itself is walked first (E6, §1 exact snapshot).
  const extra = snapshotProblem({ dir: at, entries: treeEntries(repo.checkout, commit), checkout: repo.checkout });
  if (extra) throw new SnapshotError("mismatch", `the snapshot at ${at} is not exactly commit ${commit}: ${extra}`);
  const actual = snapshotTree({ checkout: repo.checkout, dir: at, baseTree: tree, repo });
  if (actual !== tree) throw new SnapshotError("mismatch", `the snapshot at ${at} is tree ${actual}, not tree ${tree} of commit ${commit}`);
  return { schema: SNAPSHOT_SCHEMA, path: at, repoKey: repo.key, commit, tree };
}

/**
 * `rightkit-gate snapshot [--commit <rev>] [--verify] [--json]`: sync (or with --verify only check) the commit's snapshot.
 * Exit 0 the snapshot is the commit's tree and the build ownership taken for it was released, 1 mismatch / refused / busy or the
 * ownership record was kept (a survivor may still read the snapshot), 2 could not run.
 */
export function snapshotCommand(o, args) {
  const allowed = new Set(["--commit", "--verify", "--json"]);
  for (let i = 0; i < args.length; i++) {
    if (!allowed.has(args[i])) { o.err(`rightkit-gate: snapshot takes only --commit <rev>, --verify and --json, not ${JSON.stringify(args[i])}`); return 2; }
    if (args[i] === "--commit") { if (!args[i + 1] || args[i + 1].startsWith("--")) { o.err("rightkit-gate: snapshot --commit needs a revision"); return 2; } i++; }
  }
  const i = args.indexOf("--commit");
  const rev = i >= 0 ? args[i + 1] : "HEAD";
  let ownership = null;
  let code = 2;
  try {
    const root = o.snapshotRoot ?? snapshotRoot({ platform: o.platform, env: o.env });
    // A sync replaces snapshot bytes, so it runs only under the host's build ownership, taken before the first write (joined when
    // this gate already holds it); a verify only reads.
    if (!args.includes("--verify")) {
      const repo = repositoryOf(o.cwd);
      ownership = (o.acquireOwnership ?? acquireBuildOwnership)({ file: ownershipFile({ platform: o.platform, env: o.env, snapshotRoot: root }), stage: "snapshot", snapshot: path.join(root, repo.key), commit: resolveCommit(repo.checkout, rev).commit, platform: o.platform, env: o.env });
    }
    const r = args.includes("--verify") ? verifySnapshot({ checkout: o.cwd, rev, root })
      : syncSnapshot({ checkout: o.cwd, rev, root });
    if (ownership) r.ownership = { handle: ownership.handle, joined: ownership.joined, reclaimed: ownership.reclaimed };
    if (args.includes("--json")) o.out(JSON.stringify(r));
    else o.out(`rightkit-gate: snapshot ${r.path} commit=${r.commit} tree=${r.tree}${"written" in r ? ` written=${r.written} removed=${r.removed} modes=${r.modesFixed} links=${r.linksRemoved} unchanged=${r.unchanged} ms=${r.ms}` : " verified"}`);
    code = 0;
  } catch (e) {
    o.err(`rightkit-gate: snapshot ${e.code ?? "error"}: ${e.message}`);
    code = (e instanceof SnapshotError && e.code !== "error") || e instanceof OwnershipBusy ? 1 : 2;
  } finally {
    const r = ownership?.release();
    // A kept record means a survivor may still read the snapshot (or the host is not released): never a clean exit for a caller to script on.
    if (r?.released === false && !ownership.joined) {
      o.err(`rightkit-gate: snapshot failed: build ownership ${ownership.handle} kept: ${r.why}`);
      if (code === 0) code = 1;
    }
  }
  return code;
}
