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
// Roots: Mac & Dell `<managed data root>/snapshots/<repo-key>` (the root follows RIGHTKIT_DATA_ROOT & root migration); Linux
// `$RIGHTKIT_SNAPSHOT_ROOT/<repo-key>` when configured, else `$XDG_STATE_HOME/rightkit/snapshots/<repo-key>`, else
// `~/.local/state/rightkit/snapshots/<repo-key>`. `repo-key` = first 16 hex of sha256(canonical repository path), the key
// `checked-trees` uses. Scratch is never the root: it is disposable.
//
// Exclusion: one sync per snapshot at a time (`<root>/<repo-key>.lock`). Host-wide build ownership held from before the sync
// through the stage is step 4 of the joint plan; this lock never lets two syncs overwrite each other.

import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { resolveManagedRoots } from "./managed-roots.mjs";

export const SNAPSHOT_SCHEMA = "rightkit.snapshot/1";
const STATE_SCHEMA = "rightkit.snapshot-state/1";
const FILE_MODES = new Set(["100644", "100755"]);
const LINK_MODE = "120000";
const GIT_SAFE = ["-c", "core.fsmonitor=false", "-c", "core.untrackedCache=false"];
const MAX_BUFFER = 1 << 30;

export class SnapshotError extends Error {
  /** `code` is "mismatch" (the snapshot is not the commit's tree), "busy" (another sync holds it), "refused" (the commit cannot be
   * materialised exactly: a gitlink, an unsafe path or a link out of the snapshot) or "error" (could not run). */
  constructor(code, message) { super(message); this.code = code; }
}

/** First 16 hex digits of sha256 over the canonical repository path: the key RustRight's `checked-trees` & journals use. */
export function repoKey(canonicalRepoPath) {
  return crypto.createHash("sha256").update(String(canonicalRepoPath)).digest("hex").slice(0, 16);
}

/** The directory every snapshot of this host lives under. */
export function snapshotRoot({ platform = process.platform, env = process.env, home = os.homedir() } = {}) {
  const paths = platform === "win32" ? path.win32 : path.posix;
  if (platform === "darwin" || platform === "win32") return paths.join(resolveManagedRoots({ env, platform, home }).dataRoot, "snapshots");
  if (env.RIGHTKIT_SNAPSHOT_ROOT && paths.isAbsolute(env.RIGHTKIT_SNAPSHOT_ROOT)) return env.RIGHTKIT_SNAPSHOT_ROOT;
  const state = env.XDG_STATE_HOME && paths.isAbsolute(env.XDG_STATE_HOME) ? env.XDG_STATE_HOME : paths.join(home, ".local", "state");
  return paths.join(state, "rightkit", "snapshots");
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
  return { checkout: canonical, gitDir, objectFormat, key: repoKey(canonical) };
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

/** Untracked files under `dir` that no ignore rule excludes, against an index seeded from `baseTree` (what `add -A` would add). */
function untrackedVisible({ dir, baseTree, repo }) {
  const index = path.join(repo.gitDir, `rightkit-snapshot-${crypto.randomUUID()}.index`);
  const env = { GIT_INDEX_FILE: index };
  const git = (...a) => runGit(dir, [...GIT_SAFE, `--git-dir=${repo.gitDir}`, `--work-tree=${dir}`, ...a], { env });
  try {
    git("read-tree", baseTree);
    return git("ls-files", "-z", "--others", "--exclude-standard").split("\0").filter(Boolean);
  } finally {
    for (const f of [index, `${index}.lock`]) fs.rmSync(f, { force: true });
  }
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

const inside = (child, parent) => { const rel = path.relative(parent, child); return rel === "" || (!!rel && !rel.startsWith("..") && !path.isAbsolute(rel)); };
const lstat = (p) => { try { return fs.lstatSync(p, { bigint: true }); } catch (e) { if (e.code === "ENOENT" || e.code === "ENOTDIR") return null; throw e; } };
const statKey = (s) => `${s.size}:${s.mtimeNs}:${s.ctimeNs}:${s.ino}`;

/** Exclusive per-snapshot lock; a lock whose holder process is gone on this host is stale and replaced once. */
export function acquireSnapshotLock(lockFile) {
  const body = JSON.stringify({ pid: process.pid, host: os.hostname(), startedAt: new Date().toISOString() });
  for (let attempt = 0; attempt < 2; attempt++) {
    try { fs.writeFileSync(lockFile, body, { flag: "wx", mode: 0o600 }); return () => fs.rmSync(lockFile, { force: true }); }
    catch (e) {
      if (e.code !== "EEXIST") throw new SnapshotError("error", `cannot create ${lockFile}: ${e.message}`);
      let holder = null;
      try { holder = JSON.parse(fs.readFileSync(lockFile, "utf8")); } catch { holder = null; }
      const alive = holder && holder.host === os.hostname() && (() => { try { process.kill(holder.pid, 0); return true; } catch (k) { return k.code === "EPERM"; } })();
      if (alive || attempt > 0 || (holder && holder.host !== os.hostname())) throw new SnapshotError("busy", `snapshot is held by ${holder ? `pid ${holder.pid} on ${holder.host} since ${holder.startedAt}` : `an unreadable lock ${lockFile}`}`);
      fs.rmSync(lockFile, { force: true });
    }
  }
  throw new SnapshotError("busy", `snapshot lock ${lockFile} could not be taken`);
}

function readState(file) {
  try { const s = JSON.parse(fs.readFileSync(file, "utf8")); return s?.schema === STATE_SCHEMA ? s : null; } catch { return null; }
}
function writeState(file, state) {
  const tmp = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(state), { mode: 0o600 });
  fs.renameSync(tmp, file);
}

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
  fs.mkdirSync(root, { recursive: true, mode: 0o700 });
  const rootStat = lstat(root);
  if (!rootStat?.isDirectory()) throw new SnapshotError("refused", `snapshot root ${root} is not a directory (a link is never followed)`);
  const dir = path.join(root, repo.key);
  if (inside(dir, repo.checkout) || inside(repo.checkout, dir)) throw new SnapshotError("refused", `snapshot ${dir} and checkout ${repo.checkout} overlap`);
  const release = acquireSnapshotLock(`${dir}.lock`);
  try {
    const existing = lstat(dir);
    if (existing && !existing.isDirectory()) removeEntry(dir);
    fs.mkdirSync(dir, { recursive: true, mode: 0o755 });
    const stateFile = `${dir}.state.json`;
    const prior = readState(stateFile);
    const wanted = new Map(entries.map((e) => [e.path, e]));
    // Before any change, the union of what this snapshot may hold from earlier syncs, so an interrupted sync still prunes.
    const previous = new Set([...(prior?.paths ?? []), ...(prior?.pending ?? [])]);
    writeState(stateFile, { ...(prior ?? {}), schema: STATE_SCHEMA, pending: [...new Set([...previous, ...wanted.keys()])] });
    const cache = prior?.stat ?? {};
    let removed = 0, written = 0, modesFixed = 0, unchanged = 0, linksRemoved = 0;

    // 1. Files an earlier commit put here that this commit does not have (ignored ones too: they would be stale build input).
    for (const p of previous) if (!wanted.has(p) && removeEntry(path.join(dir, ...p.split("/")))) removed++;
    // 2. No link may resolve into the checkout or out of the snapshot; a committed link is checked against its committed target.
    const committedLinks = new Map(entries.filter((e) => e.mode === LINK_MODE).map((e) => [path.join(dir, ...e.path.split("/")), e]));
    for (const link of linksUnder(dir)) {
      if (committedLinks.has(link)) continue;
      let target;
      try { target = path.resolve(path.dirname(link), fs.readlinkSync(link)); } catch { target = null; }
      if (!target || inside(target, repo.checkout) || !inside(target, dir)) { removeEntry(link); linksRemoved++; }
    }

    // 3. Every tracked file: unchanged ones are left alone (same bytes & mode keep their mtime), the rest are written.
    const need = [];
    const nextStat = {};
    for (const e of entries) {
      const target = path.join(dir, ...e.path.split("/"));
      ensureParents(dir, target);
      const s = lstat(target);
      if (e.mode === LINK_MODE) { need.push(e); continue; }
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
    const blobs = readBlobs(repo.checkout, need.map((e) => e.oid));
    // Writes land in a sibling staging directory on the same volume, then rename into place: an interrupted sync never leaves a
    // half-written or stray file inside the snapshot.
    const staging = `${dir}.staging`;
    fs.rmSync(staging, { recursive: true, force: true });
    fs.mkdirSync(staging, { mode: 0o700 });
    let serial = 0;
    for (const e of need) {
      const target = path.join(dir, ...e.path.split("/"));
      const body = blobs.get(e.oid);
      if (e.mode === LINK_MODE) {
        const to = body.toString("utf8");
        const resolved = path.resolve(path.dirname(target), to);
        if (path.isAbsolute(to) || !inside(resolved, dir) || inside(resolved, repo.checkout)) throw new SnapshotError("refused", `commit ${commit} links ${e.path} -> ${to}, which resolves out of the snapshot`);
        const s = lstat(target);
        if (s?.isSymbolicLink() && fs.readlinkSync(target) === to) { unchanged++; continue; }
        if (s) removeEntry(target);
        fs.symlinkSync(to, target);
        written++;
        continue;
      }
      const s = lstat(target);
      if (s && !s.isFile()) removeEntry(target);
      const tmp = path.join(staging, String(serial++));
      fs.writeFileSync(tmp, body, { mode: e.mode === "100755" ? 0o755 : 0o644 });
      if (process.platform !== "win32") fs.chmodSync(tmp, e.mode === "100755" ? 0o755 : 0o644);
      fs.renameSync(tmp, target);
      nextStat[e.path] = { oid: e.oid, stat: statKey(lstat(target)) };
      written++;
    }

    fs.rmSync(staging, { recursive: true, force: true });

    // 4. Anything else `add -A` would see (a planted or leftover file no ignore rule excludes) is removed, with directories it emptied.
    for (const rel of untrackedVisible({ dir, baseTree: tree, repo })) {
      const target = path.join(dir, ...rel.split("/"));
      if (removeEntry(target)) removed++;
      for (let d = path.dirname(target); d !== dir && inside(d, dir); d = path.dirname(d)) {
        try { if (fs.readdirSync(d).length) break; fs.rmdirSync(d); } catch { break; }
      }
    }

    const actual = snapshotTree({ checkout: repo.checkout, dir, baseTree: tree, repo });
    if (actual !== tree) throw new SnapshotError("mismatch", `snapshot ${dir} is tree ${actual} after sync, not tree ${tree} of commit ${commit}`);
    writeState(stateFile, { schema: STATE_SCHEMA, repo: repo.checkout, repoKey: repo.key, commit, tree, paths: [...wanted.keys()], stat: nextStat, syncedAt: new Date(now()).toISOString() });
    return { schema: SNAPSHOT_SCHEMA, path: dir, repoKey: repo.key, commit, tree, written, removed, modesFixed, unchanged, linksRemoved, ms: now() - started };
  } finally {
    release();
  }
}

/** Verifies, without changing anything, that the snapshot of `checkout` holds exactly the tree of `rev`. */
export function verifySnapshot({ checkout, rev = "HEAD", root = snapshotRoot(), dir } = {}) {
  const repo = repositoryOf(checkout);
  const { commit, tree } = resolveCommit(repo.checkout, rev);
  const at = dir ?? path.join(root, repo.key);
  if (!lstat(at)?.isDirectory()) throw new SnapshotError("mismatch", `no snapshot directory at ${at}`);
  const actual = snapshotTree({ checkout: repo.checkout, dir: at, baseTree: tree, repo });
  if (actual !== tree) throw new SnapshotError("mismatch", `the snapshot at ${at} is tree ${actual}, not tree ${tree} of commit ${commit}`);
  return { schema: SNAPSHOT_SCHEMA, path: at, repoKey: repo.key, commit, tree };
}

/**
 * `rightkit-gate snapshot [--commit <rev>] [--verify] [--json]`: sync (or with --verify only check) the commit's snapshot.
 * Exit 0 the snapshot is the commit's tree, 1 mismatch / refused / busy, 2 could not run.
 */
export function snapshotCommand(o, args) {
  const allowed = new Set(["--commit", "--verify", "--json"]);
  for (let i = 0; i < args.length; i++) {
    if (!allowed.has(args[i])) { o.err(`rightkit-gate: snapshot takes only --commit <rev>, --verify and --json, not ${JSON.stringify(args[i])}`); return 2; }
    if (args[i] === "--commit") { if (!args[i + 1] || args[i + 1].startsWith("--")) { o.err("rightkit-gate: snapshot --commit needs a revision"); return 2; } i++; }
  }
  const i = args.indexOf("--commit");
  const rev = i >= 0 ? args[i + 1] : "HEAD";
  try {
    const r = args.includes("--verify") ? verifySnapshot({ checkout: o.cwd, rev, root: snapshotRoot({ platform: o.platform, env: o.env }) })
      : syncSnapshot({ checkout: o.cwd, rev, root: snapshotRoot({ platform: o.platform, env: o.env }) });
    if (args.includes("--json")) o.out(JSON.stringify(r));
    else o.out(`rightkit-gate: snapshot ${r.path} commit=${r.commit} tree=${r.tree}${"written" in r ? ` written=${r.written} removed=${r.removed} modes=${r.modesFixed} links=${r.linksRemoved} unchanged=${r.unchanged} ms=${r.ms}` : " verified"}`);
    return 0;
  } catch (e) {
    o.err(`rightkit-gate: snapshot ${e.code ?? "error"}: ${e.message}`);
    return e instanceof SnapshotError && e.code !== "error" ? 1 : 2;
  }
}
