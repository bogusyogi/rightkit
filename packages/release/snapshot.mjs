// The gate-owned commit snapshot (docs/GATE-CONTRACT.md §1 "Commit-only build" and "Broker interface"; rightkit#1254, #1255).
//
// A build compiles a selected COMMIT, never the editable checkout. The gate keeps one persistent source directory per repository
// under the managed root and syncs the commit into it by content: only changed bytes are written, an unchanged file keeps its
// mtime (so Cargo's fingerprints stay warm), files the commit no longer has are removed, tracked file types & modes are kept, and
// no link resolves into the checkout. Outputs & caches stay where they already are; only the source is separate.
//
// The snapshot's identity is the contract's §1 method, seeded per rightkit#1255 so it agrees with the broker's `snapshot_tree_of`:
// a temporary index (in a private temp directory, never in the checkout's `.git`) starts as the commit's tree (`read-tree <sha>^{tree}`), every file the
// snapshot holds is staged from it (`--work-tree=<snapshot> add -A`) against the checkout's object database, then `write-tree`.
// A committed file an ignore rule also matches stays in the id; untracked files an ignore rule excludes (build outputs) do not.
// The checkout's own index, HEAD, refs, worktrees, stash and files are never touched; only objects are written.
//
// Roots: Mac & Dell `<managed root>/snapshots/<repo-key>` with the managed root the host's fixed one (managed-roots.mjs legacyRoot:
// `/Volumes/D/.rightkit-managed`, `D:\.rightkit-managed`), never an environment variable; Linux the `snapshotRoot` the gate's per-user
// host configuration file names (snapshot-root-config.mjs: `~/.config/rightkit/gate.json`), else `$XDG_STATE_HOME/rightkit/snapshots`,
// else `~/.local/state/rightkit/snapshots`; no process environment variable or flag selects a root (RIGHTKIT_SNAPSHOT_ROOT is not
// read), because two concurrent runs could otherwise pick two roots for one repository. `repo-key` = first 16 hex of sha256(canonical
// repository path), the key `checked-trees` uses. Scratch & temp are never the root (they are disposable): a resolved root under a
// disk-policy scratch root or a temp directory is refused on every host. In-process callers inject a root as a parameter
// (`snapshotRoot` of `snapshotCommand`'s and the gate's `o`).
//
// The gate never creates the managed root (Mac & Dell): it creates `snapshots` and `snapshots/<repo-key>` beneath one that exists,
// and on the Mac that managed root must be a different device from `/`, so an unmounted volume can never yield a boot-volume
// substitute (requireManagedRoot). A snapshot root is private to this user: created 0700, and an existing POSIX root that grants
// group or other any access, or that another user owns, is refused.
//
// Exclusion: the snapshot lock, the one exclusion of every route that syncs or reads a snapshot (`<root>/<repo-key>.lock`; reset plan
// section 3). The gate takes it before the sync (`takeSnapshotLock`) and holds it through every job and test of the batch; the sync
// re-checks it before every write and before the verdict, and the broker re-verifies the tree. On a broker host its body is the seam's
// `{ pid, identity, commit, token }` and a dead holder's lock is reclaimed only on the broker's proof that the snapshot is idle; on a host
// with no broker (Linux direct) it is `{ pid, host, startedAt, token }` and a holder proven gone (ESRCH, or the pid reused) is stale.

import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { readDiskPolicy, scratchRoots } from "./disk-policy.mjs";
import { resolveManagedRoots } from "./managed-roots.mjs";
import { HostConfigError, hostConfigFile, readSnapshotRootConfig } from "./snapshot-root-config.mjs";
import { discoveryGit, discoveryRun, sealedRepoInfo, sealedRun, withSealedGit } from "./snapshot-git.mjs";
import { SOURCE_IDENTITY_FILE, sourceIdentityBytes } from "./snapshot-identity.mjs";
import { parseIdentity, processIdentity } from "./gate-seam.mjs";
// The error class and the managed-root guard live in managed-root-guard.mjs (decision D4 rule 1).
import { SnapshotError, requireManagedRoot } from "./managed-root-guard.mjs";

export { SnapshotError, requireManagedRoot };

export const SNAPSHOT_SCHEMA = "rightkit.snapshot/1";
const STATE_SCHEMA = "rightkit.snapshot-state/1";
const FILE_MODES = new Set(["100644", "100755"]);
const LINK_MODE = "120000";
// Every git call that reads objects or computes identity goes through the sealed runner (snapshot-git.mjs, item 5 D3): the
// checkout's configuration, attributes, hooks, filters and replace refs cannot reach it.
const MAX_BUFFER = 1 << 30;
const BLOB_CHUNK = 500;

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
export function isInside(child, parent, paths = path) {
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
 * never an environment variable (RIGHTKIT_DATA_ROOT & friends move build outputs, not snapshots). Linux: the `snapshotRoot` of the
 * gate's per-user host configuration file (`configFile`, default snapshot-root-config.mjs hostConfigFile()), else
 * `$XDG_STATE_HOME/rightkit/snapshots` (the contract's named default, read only when no file names a root), else
 * `<home>/.local/state/rightkit/snapshots`. RIGHTKIT_SNAPSHOT_ROOT and every other variable are not consulted: a variable could
 * select two roots for one repository in two concurrent runs. The result is refused when it lies under scratch or temp, on every
 * host. Callers that need another root (tests) pass it as a parameter.
 */
export function snapshotRoot({ platform = process.platform, env = process.env, home = os.homedir(), configFile } = {}) {
  const paths = platform === "win32" ? path.win32 : path.posix;
  let root; let source;
  if (platform === "darwin" || platform === "win32") {
    const base = resolveManagedRoots({ env: {}, platform, home }).gateRoot;
    root = paths.join(base, "snapshots");
    source = `the host's managed root ${base}`;
  } else {
    const file = configFile ?? hostConfigFile();
    let configured = null;
    try { configured = readSnapshotRootConfig(file); }
    catch (e) { if (e instanceof HostConfigError) throw new SnapshotError("error", e.message); throw e; }
    if (configured) {
      root = configured;
      source = `the gate host configuration ${file}`;
    } else {
      const fromXdg = env.XDG_STATE_HOME && paths.isAbsolute(env.XDG_STATE_HOME);
      root = paths.join(fromXdg ? env.XDG_STATE_HOME : paths.join(home, ".local", "state"), "rightkit", "snapshots");
      source = fromXdg ? "XDG_STATE_HOME" : `the home directory ${home}`;
    }
  }
  refuseDisposableRoot(root, source, { platform, env });
  return root;
}

const foldHost = (platform) => (platform === "darwin" || platform === "win32" ? (n) => n.toLowerCase() : (n) => n);

/**
 * The host's managed root when `root` is, or lies under, the host's real snapshot root location (Mac & Dell), else null. Keyed on the
 * path, not on who calls: the door passes the real root explicitly, and it must be guarded as well as the default. Compared folded
 * on Mac and Windows (case-insensitive volumes), so a spelling `/volumes/d/...` is the same root.
 */
export function hostManagedRootFor(root, { platform = process.platform, home = os.homedir() } = {}) {
  if (platform !== "darwin" && platform !== "win32") return null;
  const paths = platform === "win32" ? path.win32 : path.posix;
  const managed = resolveManagedRoots({ env: {}, platform, home }).gateRoot;
  const fold = foldHost(platform);
  return isInside(fold(paths.resolve(root)), fold(paths.resolve(managed)), paths) ? managed : null;
}


/**
 * A snapshot root is private to this user (row 5.22): on POSIX an existing root that grants group or other any access refuses, and
 * so does one another user owns. `st` is the lstat of the root (a link or non-directory is the caller's refusal).
 */
function requirePrivateRoot(root, st, { platform = process.platform, getuid = process.getuid?.bind(process) } = {}) {
  if (platform === "win32" || !st?.isDirectory()) return;
  const mode = Number(st.mode) & 0o777;
  if ((mode & 0o077) !== 0) throw new SnapshotError("refused", `snapshot root ${root} is mode ${mode.toString(8).padStart(4, "0")}: it grants group or other access, and a snapshot root is private to this user (chmod 700 it, or remove it and let the gate create it)`, "root-open-mode");
  if (getuid && Number(st.uid) !== getuid()) throw new SnapshotError("refused", `snapshot root ${root} is owned by uid ${st.uid}, not by this user (uid ${getuid()})`, "root-not-owned");
}

/**
 * Everything about the root that must hold before the first write: the managed root exists and is on the managed volume, the
 * directory the root will be created in exists (so the creation makes exactly the root, never a managed root), and an existing root
 * is a real directory this user alone can use.
 */
function guardRoot(root, managedRoot, { device } = {}) {
  if (managedRoot) {
    requireManagedRoot(managedRoot, { device });
    const parent = lstat(path.dirname(root));
    if (!parent?.isDirectory()) throw new SnapshotError("refused", `${path.dirname(root)}, where snapshot root ${root} would be created, does not exist; the gate creates only the root and the snapshots beneath it`, "root-parent-missing");
  }
  const st = lstat(root);
  if (st && !st.isDirectory()) throw new SnapshotError("refused", `snapshot root ${root} is not a directory (a link is never followed)`);
  requirePrivateRoot(root, st);
}

/** A git command that finds the checkout's repository from `cwd` (discovery only: sealed environment, no replace refs); a failure is a SnapshotError("error"). */
function runGit(cwd, args) {
  try { return discoveryGit(cwd, args); } catch (e) { throw new SnapshotError("error", e.message); }
}
/** A sealed object read against `checkout`'s object database (ls-tree, cat-file): see snapshot-git.mjs. */
function sealedGitOut(checkout, args, options) {
  const r = sealedRun(checkout, args, options);
  if (r.error || r.status !== 0) throw new SnapshotError("error", `git ${args.join(" ")}: ${r.error ? r.error.message : String(r.stderr ?? "").trim() || `exit ${r.status}`}`);
  return r.stdout;
}

/** The repository a checkout belongs to: canonical top level, absolute git directory and object format. */
export function repositoryOf(checkout) {
  const top = runGit(checkout, ["rev-parse", "--show-toplevel"]).trim();
  const canonical = fs.realpathSync.native(top);
  const gitDir = runGit(canonical, ["rev-parse", "--absolute-git-dir"]).trim();
  const objectFormat = runGit(canonical, ["rev-parse", "--show-object-format"]).trim() || "sha1";
  const { commonDir } = sealedRepoInfo(canonical);
  return { checkout: canonical, gitDir, commonDir, objectFormat, key: repoKey(keyPath(canonical)) };
}

/**
 * `{ commit, tree }` of a revision. A revision that is not a commit is refused (named "unknown-revision"), never guessed at, and
 * before anything is written (GATE-CONTRACT §1 "a revision that is not a commit is an error"); git failing to run is an error.
 */
export function resolveCommit(checkout, rev = "HEAD") {
  const name = String(rev);
  if (!name || name.startsWith("-") || name.includes("\0")) throw new SnapshotError("refused", `revision ${JSON.stringify(name)} is not a commit`, "unknown-revision");
  // The one call that reads the checkout's refs: under the sealed environment and GIT_NO_REPLACE_OBJECTS (a replace ref must not change what <sha>^{tree} means).
  const r = discoveryRun(checkout, ["rev-parse", "--verify", "--quiet", `${name}^{commit}`]);
  if (r.error) throw new SnapshotError("error", `git rev-parse ${name}: ${r.error.message}`);
  // `rev-parse --verify` exits 1 when the name does not resolve to a commit (unknown, or an object of another type, which it reports
  // on stderr); git itself failing (not a repository, a damaged object database) exits 128 and is an error.
  if (r.status === 1) throw new SnapshotError("refused", `revision ${JSON.stringify(name)} is not a commit in ${checkout}`, "unknown-revision");
  if (r.status !== 0) throw new SnapshotError("error", `git rev-parse ${name}: ${String(r.stderr ?? "").trim() || `exit ${r.status}`}`);
  const commit = r.stdout.trim();
  const tree = runGit(checkout, ["rev-parse", "--verify", "--quiet", `${commit}^{tree}`]).trim();
  return { commit, tree };
}

/**
 * The tree id of `dir` by the contract's §1 method, seeded from `baseTree` (rightkit#1255). Reads nothing of the checkout but its
 * object database and ignore rules; writes only objects (into the checkout's object database) and a temporary index kept outside the
 * checkout's `.git`, always removed.
 */
export function snapshotTree({ checkout, dir, baseTree, repo = repositoryOf(checkout), indexStore = treeIndexStoreOf(dir) }) {
  // The sealed runner (snapshot-git.mjs): a private git directory T holds the index and receives every object written; the
  // checkout's git directory is not written at all (row 5.13), and its configuration, attributes, hooks and filters cannot reach the id.
  const once = (stored) => withSealedGit({ commonDir: repo.commonDir, objectFormat: repo.objectFormat, snapshot: dir }, (git) => {
    const index = path.join(git.T, "index");
    try {
      if (stored) fs.copyFileSync(stored, index); else git.ok(["read-tree", baseTree]);
      git.ok(["add", "-A"]);
      const id = git.ok(["write-tree"]).trim();
      if (!/^[0-9a-f]{40}([0-9a-f]{24})?$/.test(id)) throw new SnapshotError("error", `git write-tree printed ${JSON.stringify(id)}`);
      if (indexStore) {
        try { const next = `${indexStore}.${process.pid}.tmp`; fs.copyFileSync(index, next); fs.renameSync(next, indexStore); } catch { /* a cache: the next run starts fresh */ }
      }
      return id;
    } catch (e) {
      throw e instanceof SnapshotError ? e : new SnapshotError("error", e.message);
    }
  });
  if (indexStore && fs.existsSync(indexStore)) {
    try { return once(indexStore); } catch { try { fs.rmSync(indexStore, { force: true }); } catch { /* best effort */ } }
  }
  return once(null);
}

/** Where a gate snapshot's tree index is kept (`<snapshot>.tree-index` beside it), or null for any other directory (a checkout). */
export function treeIndexStoreOf(dir) {
  try {
    const resolved = path.resolve(dir);
    if (!/^[0-9a-f]{16}$/.test(path.basename(resolved))) return null;
    const root = snapshotRoot();
    return path.resolve(path.dirname(resolved)) === path.resolve(root) ? `${resolved}.tree-index` : null;
  } catch { return null; }
}

/**
 * Removes every entry under `dir` the commit does not hold: untracked files whether an ignore rule matches them or not, leftover
 * directories, links that are not committed. §1 makes the snapshot exactly the commit; a stale ignored file is unattested build
 * input even though the seeded tree id cannot see it. Never follows a link (only real directories are entered); names compare
 * case-insensitively where the host filesystem does (Mac, Windows; colliding names were refused before this runs).
 */
function pruneToTree(dir, entries, platform = process.platform, keepRoot = new Set(), keepDirs = new Set()) {
  const fold = platform === "darwin" || platform === "win32" ? (n) => n.toLowerCase() : (n) => n;
  const files = new Set(entries.map((e) => fold(e.path)));
  const dirs = new Set();
  for (const e of entries) { const parts = e.path.split("/"); for (let i = 1; i < parts.length; i++) dirs.add(fold(parts.slice(0, i).join("/"))); }
  let removed = 0;
  const walk = (d, rel) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const r = rel ? `${rel}/${e.name}` : e.name;
      const p = path.join(d, e.name);
      // keepDirs: the commit's own generated pnpm dependency directories (dependencyDirs), real directories only, kept whole for the
      // dependency step, which reuses them only when every installed byte matches its last completed install and otherwise removes them.
      if (e.isDirectory()) { if (keepDirs.has(fold(r))) continue; if (dirs.has(fold(r))) walk(p, r); else { fs.rmSync(p, { recursive: true, force: true }); removed++; } }
      else if (!files.has(fold(r)) && !(rel === "" && keepRoot.has(fold(r)))) { fs.unlinkSync(p); removed++; }
    }
  };
  walk(dir, "");
  return removed;
}

/** Every entry of the commit's tree: `{ mode, oid, path }`, path in git's `/` form. Gitlinks & unsafe paths refuse. */
export function treeEntries(checkout, commit) {
  const out = sealedGitOut(checkout, ["ls-tree", "-r", "-z", "--full-tree", commit]);
  const entries = [];
  for (const record of out.split("\0").filter(Boolean)) {
    const tab = record.indexOf("\t");
    const [mode, type, oid] = record.slice(0, tab).split(" ");
    const file = record.slice(tab + 1);
    const parts = file.split("/");
    if (!file || file.startsWith("/") || parts.some((p) => !p || p === "." || p === ".." || p.toLowerCase() === ".git") || /[\\:]/.test(file) && process.platform === "win32") {
      throw new SnapshotError("refused", `commit ${commit} has a path a snapshot cannot hold exactly: ${JSON.stringify(file)}`);
    }
    if (type === "commit" || mode === "160000") throw new SnapshotError("refused", `commit ${commit} has a gitlink at ${file}: a snapshot materialises files, not nested repositories (an exact snapshot of it cannot be made from the object database); nothing was written`, "gitlink");
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
 *      binding (rightkit#1215) and removes when the check ends: `.git` holding exactly that one file and nothing else, and the file
 *      byte-equal to what the gate writes for THIS checkout's origin (gateGitConfigBody over `git config --get remote.origin.url`).
 *      Any other URL (`ext::sh -c id` was the r2#5 probe), any other section, any other byte is an extra. A checkout with no origin
 *      gets no `.git` at all.
 *   3. the generated source-identity file `.rightkit-source-identity.json` at the snapshot root (snapshot-identity.mjs, GATE-CONTRACT
 *      GC-A23; rightkit#1504): admitted only when the caller names the snapshot's `commit`, the commit tracks no file of that name, it
 *      is a regular, non-executable file with one hard link, its bytes equal sourceIdentityBytes() for that commit exactly, and the
 *      commit's own ignore rules ignore it (so the seeded tree id, the broker's included, still equals `<sha>^{tree}`). Without
 *      `commit`, with other bytes, or unignored, it is an extra like any other file.
 * Nothing else is an extra the gate may leave, and the dependency bytes of allowance 1 are bound by the environment digest
 * (`rightkit-gate env-digest --snapshot`, which `verifySnapshot` takes as `environment`), so a changed or planted byte under a
 * `node_modules` is refused at the end verify even though this walk lets the directory be.
 */
const foldFor = (platform) => (platform === "darwin" || platform === "win32" ? (n) => n.toLowerCase() : (n) => n);

/** The bytes the gate writes to the snapshot's `.git/config` for an origin URL (bin/rightkit-gate.mjs writes exactly this). */
export const gateGitConfigBody = (originUrl) => `[remote "origin"]\n\turl = ${originUrl}\n`;

/** The checkout's `remote.origin.url` as the gate reads it (`git config --get remote.origin.url`, trimmed), "" when it has none. */
export function originUrlOf(checkout) {
  const r = spawnSync("git", ["config", "--get", "remote.origin.url"], { cwd: checkout, encoding: "utf8", maxBuffer: 1 << 20 });
  return r.error || r.status !== 0 ? "" : String(r.stdout ?? "").trim();
}

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
 * What stops `dir` being exactly the commit's tree that the seeded tree id cannot see (it ignores what an ignore rule matches,
 * follows no link, and under `core.symlinks=false` or without exec bits cannot tell a link from a file holding its text): the first
 * path under `dir` that is not in `entries` (ignored or not, empty directories included) and not one of the two allowances above;
 * the first link (committed or not) that does not resolve, physically, to something inside `dir` and outside `checkout`; the first
 * entry whose TYPE is not the commit's (a file where the commit has a link or the reverse, from `lstat`, independent of git's
 * configuration), whose link text is not the committed text, or, on POSIX, whose exec bit is not the commit's; and the first
 * non-directory outside the dependency directories with more than one hard link (a checkout file hard-linked into the snapshot
 * carries every later checkout edit into the build, with bytes that may still equal the commit's), unless every other name for it
 * is inside the snapshot's own dependency directories (pnpm's link of a committed `file:` package). Hard links inside the
 * dependency directories are pnpm's own store links; their bytes are bound by the environment digest instead. Never follows a link
 * to enter it. Returns a message naming the path, or null.
 *
 * The stage-output ledger (item 5 row 5.09d; signed/item7-sage-decisions-2.md Q1). Given `ledger` and/or `admitNew`, the walk does not
 * refuse the first path the commit lacks: it collects every such path as a candidate entry and admits it only when ALL hold:
 *   - it is untracked (not in `entries`) and ignored by the COMMIT'S OWN ignore rules, judged by the neutral oracle (neutralIgnored:
 *     `git check-ignore --no-index` against an empty bare GIT_DIR with core.excludesFile at the null device, so neither the checkout's
 *     info/exclude nor any global excludes file widens what is ignored; probe P4);
 *   - it is a regular file or a directory, never a link of any kind, and a file has nlink 1;
 *   - no path segment is `target`, `.right-release`, `.git` or `node_modules` (outside the pnpm roots, which are not candidates),
 *     and it is not named `.gitignore` (an untracked ignore file would make the oracle ignore whatever it names).
 * `ledger` (an array of `{ path, type, size, sha256, execBit }` recorded by an earlier verify) must still match exactly, byte for byte,
 * and none of its entries may be missing (an absent directory that only parents ledger files is implied by them). `admitNew` (end of a
 * stage only) additionally admits candidates the ledger does not hold. Without `admitNew`, a candidate outside the ledger is refused,
 * so the start of a stage stays as strict as the sync. In this mode the result is `{ problem, admitted }`, `admitted` being every
 * admitted entry (ledger and new), sorted by path; otherwise it is the message or null as before.
 */
export function snapshotProblem(options) {
  const r = inspectSnapshot(options);
  return options.ledger !== undefined || options.admitNew ? r : r.problem;
}

const RESERVED_SEGMENTS = new Set(["target", ".right-release", ".git", "node_modules"]);

/**
 * Which of `paths` (relative, `/`-separated, existing under `dir`) the snapshot's own ignore rules ignore, and nothing else: the
 * sealed runner (snapshot-git.mjs) gives an empty info/exclude and an empty private file for core.excludesFile (never a null device,
 * so Windows needs no `NUL`), and `--no-index` so only the work tree's `.gitignore` files count. Returns a Set of the ignored paths as given.
 */
export function neutralIgnored(dir, paths) {
  if (!paths.length) return new Set();
  return withSealedGit({ snapshot: dir }, (git) => {
    const r = git.run(["check-ignore", "--no-index", "-z", "--stdin"], { input: `${paths.join("\0")}\0` });
    if (r.error) throw new SnapshotError("error", `git check-ignore: ${r.error.message}`);
    // Exit 0: some path is ignored; 1: none is; anything else is git failing, which must never read as "not ignored" or "ignored".
    if (r.status !== 0 && r.status !== 1) throw new SnapshotError("error", `git check-ignore: ${String(r.stderr).trim() || `exit ${r.status}`}`);
    return new Set(String(r.stdout ?? "").split("\0").filter(Boolean));
  });
}

/** sha256 of a file, read in chunks (a staged sidecar can be large). */
function sha256File(file) {
  const h = crypto.createHash("sha256");
  const fd = fs.openSync(file, "r");
  try {
    const buf = Buffer.allocUnsafe(1 << 20);
    for (let n; (n = fs.readSync(fd, buf, 0, buf.length, null)) > 0;) h.update(buf.subarray(0, n));
  } finally { fs.closeSync(fd); }
  return h.digest("hex");
}

function inspectSnapshot({ dir, entries, checkout, commit, platform = process.platform, originUrl, ledger, admitNew = false }) {
  const problem = (message) => ({ problem: message, admitted: [] });
  const ledgerMode = ledger !== undefined || admitNew === true;
  const fold = foldFor(platform);
  const candidates = [];
  const byKey = new Map(entries.map((e) => [fold(e.path), e]));
  const dirs = new Set();
  for (const e of entries) { const parts = e.path.split("/"); for (let i = 1; i < parts.length; i++) dirs.add(fold(parts.slice(0, i).join("/"))); }
  const deps = dependencyDirs(entries, platform);
  const shared = []; // committed files with more than one hard link, judged after the walk (sharedLinkProblem)
  const depRoots = []; // the dependency directories the walk entered
  let realDir; let realCheckout = checkout;
  try { realDir = fs.realpathSync.native(dir); } catch (e) { return problem(`${dir} cannot be resolved: ${e.message}`); }
  try { realCheckout = fs.realpathSync.native(checkout); } catch { /* the path as given */ }
  const linkProblem = (rel, p) => {
    let real;
    try { real = fs.realpathSync.native(p); } catch { return `${rel} is a link that does not resolve (dangling or looping); it cannot be proven to stay inside the snapshot`; }
    if (inside(real, realCheckout)) return `${rel} is a link resolving into the editable checkout (${real})`;
    if (!inside(real, realDir)) return `${rel} is a link resolving outside the snapshot (${real})`;
    return null;
  };
  // The committed text of every committed link, read once, lazily (a tree with no links reads nothing).
  let linkTexts = null;
  const committedLinkText = (entry) => {
    if (!linkTexts) {
      const links = entries.filter((x) => x.mode === LINK_MODE);
      const blobs = links.length ? readBlobs(checkout, links.map((x) => x.oid)) : new Map();
      linkTexts = new Map(links.map((x) => [x.path, blobs.get(x.oid).toString("utf8")]));
    }
    return linkTexts.get(entry.path);
  };
  const gitConfigProblem = (rel, p) => {
    const names = fs.readdirSync(p);
    if (names.length !== 1 || names[0] !== "config") return `${rel} is not in the commit's tree (a .git directory may hold only the gate's config-only origin file)`;
    const config = path.join(p, "config");
    const s = lstat(config);
    const origin = originUrl ?? originUrlOf(checkout);
    if (!origin) return `${rel} is not in the commit's tree (the checkout has no origin, so the gate writes no .git/config)`;
    if (!s?.isFile() || fs.readFileSync(config, "utf8") !== gateGitConfigBody(origin)) return `${rel}/config is not in the commit's tree (it is not byte-equal to the gate's config-only file for this checkout's origin)`;
    return null;
  };
  // Allowance 3: the generated source-identity file (see the allowances above). Judged here, never as a ledger candidate, so the ledger
  // and the tree id stay exactly what they were.
  const identityName = fold(SOURCE_IDENTITY_FILE);
  const identityProblem = (r, p) => {
    if (!commit) return `${r} is not in the commit's tree (the source-identity file is admitted only when the snapshot's commit is named)`;
    const s = lstat(p);
    if (!s?.isFile()) return `${r} is not in the commit's tree (it is not a regular file)`;
    if (s.nlink > 1n) return `${r} is not in the commit's tree and has ${s.nlink} hard links (the gate's source-identity file has one)`;
    if (platform !== "win32" && (Number(s.mode) & 0o111) !== 0) return `${r} is not in the commit's tree and is executable (the gate writes the source-identity file without an exec bit)`;
    let want;
    try { want = sourceIdentityBytes({ checkout, commit, entries }); }
    catch (e) { return `${r} is not in the commit's tree and the source identity of commit ${commit} cannot be recomputed to judge it: ${e.message}`; }
    if (!fs.readFileSync(p).equals(want)) return `${r} is not in the commit's tree and is not the gate's source identity for commit ${commit} (its bytes differ from the deterministic file)`;
    if (!neutralIgnored(dir, [r]).has(r)) return `${r} is the gate's source identity but the commit's own ignore rules do not ignore it, so it would change the tree id (add /${SOURCE_IDENTITY_FILE} to the commit's .gitignore)`;
    return null;
  };
  const fileProblem = (r, p, entry) => {
    const s = lstat(p);
    if (!s) return `${r} vanished while it was checked`;
    if (!s.isFile() && !s.isSymbolicLink()) return `${r} is not a regular file or a link`;
    if (s.nlink > 1n) shared.push({ r, dev: s.dev, ino: s.ino, nlink: s.nlink });
    const committedLink = entry.mode === LINK_MODE;
    if (s.isSymbolicLink() !== committedLink) return committedLink ? `${r} is a plain file, but the commit holds a link there (a file holding the link's text is not the link)` : `${r} is a link, but the commit holds a file there`;
    if (committedLink) {
      const want = committedLinkText(entry);
      const have = linkText(fs.readlinkSync(p));
      if (have !== want) return `${r} is a link to ${JSON.stringify(have)}, but the commit's link is to ${JSON.stringify(want)}`;
    } else if (platform !== "win32" && ((Number(s.mode) & 0o111) !== 0) !== (entry.mode === "100755")) {
      return `${r} has mode ${(Number(s.mode) & 0o777).toString(8)}, but the commit holds ${entry.mode === "100755" ? "an executable" : "a non-executable"} file`;
    }
    return null;
  };
  // Ledger mode: a path the commit lacks is a candidate. Everything about it that does not need git is decided here; whether the
  // commit's own rules ignore it is decided for all candidates at once afterwards (neutralIgnored).
  const reserved = (rel) => {
    const parts = rel.split("/").map(fold);
    const bad = parts.find((x) => RESERVED_SEGMENTS.has(x));
    if (bad !== undefined) return `${rel} is under ${JSON.stringify(bad)}, a reserved path no stage output may occupy (target/ is the checkout's output directory, .right-release holds right-release's, .git is the gate's config-only file, node_modules is allowed only as the pnpm roots' installed dependencies)`;
    if (fold(parts.at(-1)) === ".gitignore") return `${rel} is an ignore file the commit does not have: an untracked .gitignore would decide what counts as ignored`;
    return null;
  };
  const candidate = (p, rel) => {
    const bad = reserved(rel);
    if (bad) return bad;
    const s = lstat(p);
    if (!s) return `${rel} vanished while it was checked`;
    if (s.isSymbolicLink()) return `${rel} is a link: a link is never admitted to the stage-output ledger`;
    if (!s.isFile()) return `${rel} is not a regular file or a directory`;
    if (s.nlink > 1n) return `${rel} has ${s.nlink} hard links: a checkout edit made through another name for it would reach the build`;
    candidates.push({ path: rel, type: "file", size: Number(s.size), sha256: sha256File(p), execBit: platform !== "win32" && (Number(s.mode) & 0o111) !== 0 });
    return null;
  };
  const candidateDir = (p, rel) => {
    const bad = reserved(rel);
    if (bad) return bad;
    candidates.push({ path: rel, type: "dir", size: 0, sha256: null, execBit: false });
    for (const e of fs.readdirSync(p, { withFileTypes: true }).sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))) {
      const r = `${rel}/${e.name}`;
      const q = path.join(p, e.name);
      const found = e.isSymbolicLink() ? `${r} is a link: a link is never admitted to the stage-output ledger` : e.isDirectory() ? candidateDir(q, r) : candidate(q, r);
      if (found) return found;
    }
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
        if (!found && !inDeps) found = !byKey.has(key) ? (ledgerMode ? `${r} is a link the commit does not have: a link is never admitted to the stage-output ledger` : `${r} is not in the commit's tree (a link the commit does not have)`) : fileProblem(r, p, byKey.get(key));
      } else if (e.isDirectory()) {
        if (inDeps) found = walk(p, r, true);
        else if (deps.has(key)) { depRoots.push(p); found = walk(p, r, true); }
        else if (dirs.has(key)) found = walk(p, r, false);
        else if (key === ".git") found = gitConfigProblem(r, p);
        else found = ledgerMode ? candidateDir(p, r) : `${r} is not in the commit's tree (a directory the commit does not have)`;
      } else if (!inDeps) found = !byKey.has(key) ? (rel === "" && key === identityName ? identityProblem(r, p) : ledgerMode ? candidate(p, r) : `${r} is not in the commit's tree`) : fileProblem(r, p, byKey.get(key));
      if (found) return found;
    }
    return null;
  };
  // A committed file may share its inode only with names inside the snapshot's own installed dependencies: pnpm hard-links a
  // committed `file:` package's files into node_modules/.pnpm on NTFS (ViewRight's vendor/docx-editor on the Dell). Every other
  // name, a checkout file or a store file included, is unaccounted for and refused. Only files named like a shared file are stat'ed.
  const sharedLinkProblem = () => {
    if (!shared.length) return null;
    const idOf = (s) => `${s.dev}:${s.ino}`;
    const counts = new Map(shared.map((x) => [idOf(x), 0n]));
    const names = new Set(shared.map((x) => fold(x.r.slice(x.r.lastIndexOf("/") + 1))));
    const scan = (d) => {
      for (const e of fs.readdirSync(d, { withFileTypes: true })) {
        const q = path.join(d, e.name);
        if (e.isSymbolicLink()) continue;
        if (e.isDirectory()) scan(q);
        else if (names.has(fold(e.name))) { const s = lstat(q); const id = s && idOf(s); if (id && counts.has(id)) counts.set(id, counts.get(id) + 1n); }
      }
    };
    for (const d of depRoots) scan(d);
    for (const x of shared) {
      const inside = counts.get(idOf(x));
      if (x.nlink !== 1n + inside) return `${x.r} has ${x.nlink} hard links and only ${inside} other name(s) for it are in the snapshot's installed dependencies: a checkout edit made through another name for it would reach the build`;
    }
    return null;
  };
  const found = walk(dir, "", false) ?? sharedLinkProblem();
  if (found || !ledgerMode) return { problem: found, admitted: [] };

  // The ledger's entries, keyed like the walk's, must all be here and byte-identical; anything else the walk met is new.
  const held = new Map((ledger ?? []).map((e) => [fold(e.path), e]));
  // A tool-created empty directory the commit's own ignore rules ignore (xcodebuild's `.swiftpm/xcode`) holds no byte and is no ledger entry: dropped here, deepest first so a
  // directory left holding only such directories goes too. One the rules do not ignore stays a candidate and is refused below, as are reserved paths (refused above).
  const ignored = neutralIgnored(dir, candidates.map((c) => c.path));
  for (const d of candidates.filter((c) => c.type === "dir" && ignored.has(c.path) && !held.has(fold(c.path))).sort((a, b) => b.path.length - a.path.length)) {
    if (!candidates.some((c) => c !== d && c.path.startsWith(`${d.path}/`))) candidates.splice(candidates.indexOf(d), 1);
  }
  const seen = new Map(candidates.map((c) => [fold(c.path), c]));
  for (const [key, want] of held) {
    const have = seen.get(key);
    if (!have) return problem(`${want.path} is a ledger entry (recorded at the end of an earlier stage) and is missing`);
    if (have.type !== want.type || have.size !== want.size || have.sha256 !== want.sha256 || have.execBit !== want.execBit) {
      return problem(`${want.path} is a ledger entry and changed since it was recorded (${want.type} ${want.size} bytes sha256 ${want.sha256}${want.execBit ? " executable" : ""} was recorded; ${have.type} ${have.size} bytes sha256 ${have.sha256}${have.execBit ? " executable" : ""} is here)`);
    }
  }
  // A directory that only parents ledger entries is implied by them; any other candidate must be admitted as new.
  const parents = new Set();
  for (const e of held.values()) { const parts = e.path.split("/"); for (let i = 1; i < parts.length; i++) parents.add(fold(parts.slice(0, i).join("/"))); }
  for (const c of candidates) {
    const key = fold(c.path);
    if (!held.has(key) && !(c.type === "dir" && parents.has(key)) && !admitNew) return problem(`${c.path} is not in the commit's tree and not in the stage-output ledger (new stage outputs are admitted only at the end of a stage)`);
  }
  // Every candidate, ledger or new, must be ignored by the commit's own rules, as the neutral oracle judges them (`ignored`, above).
  // Git's own semantics for a directory: `git status` shows nothing for one whose every entry is ignored (or that holds nothing), even
  // when no rule names the directory itself (a root rule `src-tauri/gen/schemas/` leaves `src-tauri/gen` unnamed). Judged deepest
  // first, so a directory counts as ignored exactly when every file and subdirectory below it does; a single unignored file still refuses.
  for (const d of candidates.filter((c) => c.type === "dir" && !ignored.has(c.path)).sort((a, b) => b.path.length - a.path.length)) {
    const below = candidates.filter((c) => c.path.startsWith(`${d.path}/`));
    if (below.every((c) => ignored.has(c.path))) ignored.add(d.path);
  }
  for (const c of candidates) if (!ignored.has(c.path)) return problem(`${c.path} is not in the commit's tree and is not ignored by the commit's own ignore rules (the checkout's info/exclude and global excludes do not count)`);
  return { problem: null, admitted: [...candidates].sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0)) };
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
    const out = sealedGitOut(checkout, ["cat-file", "--batch"], { input: `${chunk.join("\n")}\n`, binary: true });
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
// ---- the lock holder's own detached children (minimal process containment of the gate's own work outside the broker) -------------------------
// The gate runs test executables, right-release builds and heavy commands itself (GC-A08: tests run outside the broker), POSIX ones detached in their own
// process group so a timeout or cancel kills the whole group. A gate that crashes can leave such a group running and still reading or writing the snapshot,
// and the broker's SNAPSHOT_STATUS proof covers only broker jobs. So while this process holds a snapshot lock, every detached child it starts is recorded beside
// the lock (`<lock>.children/group-<pid>.json`, removed when the child exits), and a dead holder's lock is never reclaimed while a recorded group still answers.
// Windows children stay in the gate's job object and are not recorded. Residual, named: a crash between the spawn returning and the record being written
// leaves that one group unrecorded.
let heldLockFile = null;
const childrenDirOf = (lockFile) => `${lockFile}.children`;
/** The variable that hands the held lock's record directory to this process's own children (right-release, heavy-command), which record their detached children there. */
export const LOCK_CHILDREN_ENV = "RIGHTKIT_SNAPSHOT_LOCK_CHILDREN";
/** Records a detached POSIX child of this process's held snapshot lock; returns the child. */
export function recordLockChild(child, { platform = process.platform, env = process.env } = {}) {
  const dir = heldLockFile ? childrenDirOf(heldLockFile) : (typeof env?.[LOCK_CHILDREN_ENV] === "string" && path.isAbsolute(env[LOCK_CHILDREN_ENV]) ? env[LOCK_CHILDREN_ENV] : null);
  if (!dir || platform === "win32" || !child || !Number.isInteger(child.pid) || child.pid <= 0) return child;
  const file = path.join(dir, `group-${child.pid}.json`);
  try { fs.mkdirSync(dir, { recursive: true, mode: 0o700 }); fs.writeFileSync(file, JSON.stringify({ group: child.pid, by: process.pid, at: new Date().toISOString() }), { mode: 0o600 }); }
  catch (error) { try { process.kill(-child.pid, "SIGKILL"); } catch { /* already gone */ } throw new SnapshotError("error", `the snapshot lock's child record ${file} cannot be written (${error.message}); the child was stopped`); }
  const forget = () => { try { fs.rmSync(file, { force: true }); } catch { /* left: a dead group never blocks a reclaim */ } };
  if (typeof child.once === "function") child.once("exit", forget);
  return child;
}
/** The recorded process groups of `lockFile` that still answer (kill(-group, 0) is not ESRCH); a group that cannot be judged counts as alive. */
export function liveLockChildren(lockFile, { kill = (pid, sig) => process.kill(pid, sig) } = {}) {
  let names = [];
  try { names = fs.readdirSync(childrenDirOf(lockFile)).filter((n) => /^group-\d+\.json$/.test(n)); } catch { return []; }
  const live = [];
  for (const n of names) {
    const group = Number(n.slice("group-".length, -".json".length));
    try { kill(-group, 0); live.push(group); } catch (e) { if (e?.code !== "ESRCH") live.push(group); }
  }
  return live;
}

export function acquireSnapshotLock(lockFile, hooks = {}, { body: seamBody = null, judge = null, identityOf = (pid) => processIdentity(pid, { platform: process.platform, spawnSync: (c, a, o) => spawnSync(c, a, o) }) } = {}) {
  // The seam's lock body (reset plan section 3, gate-seam.mjs snapshotLockBody): exactly { pid, identity, commit, token }, which the broker reads at
  // submission and before spawn. Without one, the legacy body { pid, host, startedAt, token } is written.
  // The legacy body also names this process's birth identity (when readable), so a reused pid is told from the holder on a host with no broker proof to lean on.
  let birth = null; if (!seamBody) { try { birth = identityOf(process.pid) ?? null; } catch { birth = null; } }
  const body = JSON.stringify(seamBody ?? { pid: process.pid, host: os.hostname(), startedAt: new Date().toISOString(), token: crypto.randomUUID(), ...(typeof birth === "string" && birth ? { birth } : {}) });
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
    heldLockFile = lockFile;
    process.env[LOCK_CHILDREN_ENV] = childrenDirOf(lockFile); // inherited by this process's children (they spawn with process.env or a copy made after this)
    const release = () => {
      if (!held()) return;
      // A recorded child group still running keeps the lock: its holder is this process, so a later run sees a live holder, and after this process exits the reclaim waits for the group.
      if (liveLockChildren(lockFile).length) return;
      try { fs.rmSync(childrenDirOf(lockFile), { recursive: true, force: true }); } catch { /* empty records */ }
      fs.rmSync(lockFile, { force: true });
      if (heldLockFile === lockFile) { heldLockFile = null; delete process.env[LOCK_CHILDREN_ENV]; }
    };
    release.held = held;
    release.lockFile = lockFile;
    release.token = JSON.parse(body).token;
    return release;
  };
  const read = () => { try { return fs.readFileSync(lockFile, "utf8"); } catch (e) { if (e.code === "ENOENT") return null; throw new SnapshotError("error", `cannot read ${lockFile}: ${e.message}`); } };
  const judgeStale = (observed) => {
    let holder = null;
    try { holder = JSON.parse(observed); } catch { holder = null; }
    // Locks appear whole, so one that cannot be parsed was not written by this protocol: never reclaimed automatically.
    if (!holder || !Number.isInteger(holder.pid) || holder.pid <= 0 || holder.pid > 0x7fffffff) throw busy(`snapshot lock ${lockFile} cannot be read as a lock; remove it only if no sync is running`);
    const seam = typeof holder.identity === "string" ? parseIdentity(holder.identity) : null;
    if (seam) {
      // A seam lock: the holder's identity names its platform and pid; another platform's lock is never ours to judge.
      if (seam.pid !== holder.pid || seam.platform !== process.platform) throw busy(`snapshot is held by ${holder.identity} for commit ${holder.commit}`);
    } else {
      if (typeof holder.host !== "string" || !holder.host) throw busy(`snapshot lock ${lockFile} cannot be read as a lock; remove it only if no sync is running`);
      if (holder.host !== os.hostname()) throw busy(`snapshot is held by pid ${holder.pid} on ${holder.host} since ${holder.startedAt}`);
    }
    // Only a holder proven gone (ESRCH) is stale; any other answer, EPERM included, leaves the lock alone.
    let probe = null;
    try { process.kill(holder.pid, 0); } catch (k) { probe = k.code ?? String(k); }
    if (probe === null && seam) {
      // The pid is alive. A seam lock carries the holder's birth identity, so a reused pid is told from the holder: another birth identity means the holder is gone (and the
      // broker's proof still decides the reclaim, below); the same one means it is live; an identity that cannot be read proves nothing and leaves the lock alone.
      let now = null; try { now = identityOf(holder.pid); } catch { now = null; }
      if (typeof now !== "string" || !now) throw busy(`snapshot lock holder pid ${holder.pid} is alive and its birth identity cannot be read to tell it from a reused pid (${holder.identity})`);
      if (now === holder.identity) throw busy(`snapshot is held by ${holder.identity} for commit ${holder.commit}`);
      probe = "ESRCH"; // the pid now belongs to another process: the holder is gone
    }
    if (probe === null && typeof holder.birth === "string" && holder.birth) {
      // A legacy lock that names its holder's birth identity: another identity at the pid means the pid was reused and the holder is gone; the same one is live; unreadable proves nothing.
      let now = null; try { now = identityOf(holder.pid); } catch { now = null; }
      if (typeof now === "string" && now && now !== holder.birth) probe = "ESRCH";
    }
    if (probe === null) throw busy(`snapshot is held by pid ${holder.pid} since ${holder.startedAt}`);
    if (probe !== "ESRCH") throw busy(`snapshot lock holder pid ${holder.pid} cannot be proven gone (${probe})`);
    // The holder is gone, but a process group it started (a test executable, a build, a heavy command) may still run against the snapshot: never reclaimed then.
    const groups = (hooks.liveChildren ?? liveLockChildren)(lockFile);
    if (groups.length) throw busy(`snapshot lock holder pid ${holder.pid} is gone, but process group(s) it started still run (${groups.join(", ")}); the lock is reclaimed once they end`);
    // A dead seam holder may still have jobs queued, running or finalizing in the broker, or an unreaped process tree: its lock is reclaimed only when
    // the caller's `judge(holder)` returns true on the broker's proof that all use of the snapshot ended. No judge, or an inconclusive one, means busy.
    if (seam && (typeof judge !== "function" || judge(holder) !== true)) throw busy(`snapshot lock holder ${holder.identity} is gone, but the lock is reclaimed only on the broker's proof that every job of commit ${holder.commit} ended${typeof judge === "function" ? " (the broker gave no such proof)" : " (no broker proof is wired: GAP, reset plan section 3)"}`);
    // The proof the reclaim rests on, named for the run's log line (`snapshot lock held <token> (reclaimed from <from>: <proof>)`).
    return seam
      ? { from: holder.identity, proof: `owner ${holder.identity} is gone and the broker showed the snapshot idle for commit ${holder.commit}` }
      : { from: `pid ${holder.pid}`, proof: `owner pid ${holder.pid} on ${holder.host} is gone (no process answers to it)` };
  };
  const first = create();
  if (first) return first;
  const observed = read();
  if (observed === null) { const again = create(); if (again) return again; throw busy(`snapshot lock ${lockFile} was taken by another process`); }
  const reclaimed = judgeStale(observed);
  hooks.afterStaleRead?.();
  const reclaim = `${lockFile}.reclaim`;
  if (!publish(reclaim, body)) throw busy(`snapshot lock ${lockFile} is being reclaimed by another process (remove ${reclaim} only if no sync is running)`);
  try {
    hooks.whileReclaiming?.();
    if (read() !== observed) throw busy(`snapshot lock ${lockFile} changed before it could be reclaimed`);
    fs.rmSync(lockFile, { force: true });
    try { fs.rmSync(childrenDirOf(lockFile), { recursive: true, force: true }); } catch { /* only dead groups were recorded */ }
    const mine = create();
    if (!mine) throw busy(`snapshot lock ${lockFile} was taken by another process`);
    mine.reclaimed = reclaimed;
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

/**
 * On a case-insensitive host (Mac, Windows) a snapshot entry that exists under another case than the commit's spelling (a commit
 * that renames a file or directory by case only, synced over its parent) is renamed to the exact spelling, keeping its bytes and
 * mtime: the sealed identity runs with core.ignoreCase=false, so the on-disk spelling is part of the id (item 5 D3, probe P3d).
 * `listing` caches directory listings for one sync. Returns how many components it renamed.
 */
function exactCase(dir, rel, listing, platform = process.platform) {
  if (platform !== "darwin" && platform !== "win32") return 0;
  let at = dir; let fixed = 0;
  for (const part of rel.split("/")) {
    let names = listing.get(at);
    if (!names) { try { names = new Set(fs.readdirSync(at)); } catch { return fixed; } listing.set(at, names); }
    if (!names.has(part)) {
      const variant = [...names].find((n) => n.toLowerCase() === part.toLowerCase());
      if (variant === undefined) return fixed;
      const tmp = path.join(at, `.rk-case-${crypto.randomUUID()}`);
      fs.renameSync(path.join(at, variant), tmp);
      fs.renameSync(tmp, path.join(at, part));
      names.delete(variant); names.add(part);
      fixed++;
    }
    at = path.join(at, part);
  }
  return fixed;
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
 * Everything that must hold before the first write under the snapshot root, and where the repository's snapshot lives: the managed root exists on the
 * managed volume, the root is a private real directory (created 0700 when missing, beneath a parent that exists) and does not overlap the checkout.
 * Returns `{ dir }`, the snapshot directory. Shared by the sync and by `takeSnapshotLock`, so a lock is never the first thing written to a boot-volume
 * substitute for an unmounted managed root.
 */
function prepareSnapshotDir({ repo, root, managedRoot, device }) {
  guardRoot(root, managedRoot, { device });
  fs.mkdirSync(root, { recursive: true, mode: 0o700 });
  const rootStat = lstat(root);
  if (!rootStat?.isDirectory()) throw new SnapshotError("refused", `snapshot root ${root} is not a directory (a link is never followed)`);
  const dir = path.join(root, repo.key);
  if (inside(dir, repo.checkout) || inside(repo.checkout, dir)) throw new SnapshotError("refused", `snapshot ${dir} and checkout ${repo.checkout} overlap`);
  return { dir };
}

/**
 * Takes the snapshot lock of `checkout`'s repository (`<root>/<repo-key>.lock`) and returns its release function, which also carries `held()`, `token`,
 * `lockFile` and, when a dead holder's lock was reclaimed, `reclaimed: { from, proof }`. The one exclusion of every route that syncs or reads a snapshot:
 * taken before the sync, held through every job and test of the batch, released only by the process that took it. `lock` is `{ body, judge, identityOf }`
 * as `acquireSnapshotLock` takes them (the seam's body and broker proof on a broker host; none on a host with no broker). Throws SnapshotError("busy")
 * while another process holds the snapshot.
 */
export function takeSnapshotLock({ checkout, root = snapshotRoot(), managedRoot = hostManagedRootFor(root), device, lock = {} } = {}) {
  const repo = repositoryOf(checkout);
  const { dir } = prepareSnapshotDir({ repo, root, managedRoot, device });
  return acquireSnapshotLock(`${dir}.lock`, {}, { body: lock.body ?? null, judge: lock.judge ?? null, ...(lock.identityOf ? { identityOf: lock.identityOf } : {}) });
}

/**
 * What a nested gate is handed to join the lock of the run that holds it: the token, the lock file and `held()`, and no way to release it. A nested check,
 * compile or sync passes it down and never takes a second lock; only the process that took the lock releases it.
 */
export const lockView = (release) => ({ token: release.token, lockFile: release.lockFile, held: release.held, joined: true });

/**
 * Syncs commit `rev` of the repository at `checkout` into its snapshot directory and verifies the result is exactly the commit's
 * tree. Returns `{ schema, path, repoKey, commit, tree, written, removed, modesFixed, unchanged, linksRemoved, hardLinksReplaced, ms }`.
 * Throws SnapshotError: "refused" before anything is written when the commit cannot be held exactly (a gitlink, an unsafe path, an
 * unknown revision) or the root is not one a snapshot may live in (a missing or boot-volume managed root, an open-mode root);
 * "busy" while another sync holds the snapshot; "mismatch" when the result is not the commit's tree; "error" when git or the
 * filesystem fails. `managedRoot` (default: the host's, when `root` is or lies under the host's snapshot root) is the managed root
 * that must already exist; `device` reads a path's device (tests inject it).
 */
// `lock`: the snapshot lock of this sync (reset plan section 3). `held` is a lock the caller already took with `takeSnapshotLock` for this same snapshot: the sync
// uses it, re-checks it before every write and never releases it (the caller holds it through the batch). Without `held` the sync takes the lock itself, for the sync
// alone: `body` is the seam's lock body the broker reads (gate-seam.mjs snapshotLockBody) and `judge` decides a dead holder's reclaim on the broker's proof.
export function syncSnapshot({ checkout, rev = "HEAD", root = snapshotRoot(), managedRoot = hostManagedRootFor(root), device, now = () => Date.now(), lock = {} } = {}) {
  const started = now();
  const repo = repositoryOf(checkout);
  const { commit, tree } = resolveCommit(repo.checkout, rev);
  const entries = treeEntries(repo.checkout, commit);
  refuseCaseCollisions(entries, commit);
  // Before the first write: the managed root exists on the managed volume, and an existing root is private to this user.
  const { dir } = prepareSnapshotDir({ repo, root, managedRoot, device });
  const joined = lock.held ?? null;
  if (joined) {
    if (typeof joined.held !== "function" || joined.lockFile !== `${dir}.lock`) throw new SnapshotError("error", `the snapshot lock ${joined.lockFile ?? "(none)"} is not the lock of ${dir}`);
    if (!joined.held()) throw new SnapshotError("busy", `snapshot lock ${dir}.lock is not held by this process`);
  }
  const release = joined ?? acquireSnapshotLock(`${dir}.lock`, {}, { body: lock.body ?? null, judge: lock.judge ?? null, ...(lock.identityOf ? { identityOf: lock.identityOf } : {}) });
  const assertLockHeld = () => { if (!release.held()) throw new SnapshotError("busy", `snapshot lock ${dir}.lock is no longer held by this sync`); };
  try {
    const existing = lstat(dir);
    if (existing && !existing.isDirectory()) removeEntry(dir);
    fs.mkdirSync(dir, { recursive: true, mode: 0o755 });
    const stateFile = `${dir}.state.json`;
    const prior = readState(stateFile);
    const wanted = new Map(entries.map((e) => [e.path, e]));
    const cache = prior?.stat ?? {};
    let removed = 0, written = 0, modesFixed = 0, unchanged = 0, linksRemoved = 0, hardLinksReplaced = 0, casesFixed = 0;
    const caseListing = new Map();

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
      casesFixed += exactCase(dir, e.path, caseListing);
      ensureParents(dir, target);
      const s = lstat(target);
      // A hard-linked file (another name for it outside this snapshot, perhaps a checkout file with equal bytes) is never kept, and
      // never chmod-ed or written through: it is replaced by a fresh copy (temp + rename), which leaves the other name alone.
      if (s?.isFile() && s.nlink > 1n) hardLinksReplaced++;
      else if (s?.isFile()) {
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
    // The snapshot lock is held, so any staging directory still here belongs to a sync that died: it is removed, never accumulated.
    fs.rmSync(stagingRoot, { recursive: true, force: true });
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
        casesFixed += exactCase(dir, e.path, new Map());
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

    // 4. Everything the commit does not hold is removed: the snapshot is exactly the commit (§1). The one exception is the gate's own
    // source-identity file when it already holds this commit's exact bytes (kept, so its mtime and Cargo's fingerprints stay warm).
    assertLockHeld();
    let identityBytes = null;
    const identity = { file: SOURCE_IDENTITY_FILE, written: false, kept: false, reason: null };
    if (repo.objectFormat !== "sha1") identity.reason = `the repository's object format is ${repo.objectFormat}, and the source identity records SHA-1 ids`;
    else if (wanted.has(SOURCE_IDENTITY_FILE)) identity.reason = `the commit itself tracks ${SOURCE_IDENTITY_FILE}`;
    else {
      try { identityBytes = sourceIdentityBytes({ checkout: repo.checkout, commit, entries }); }
      catch (e) { throw new SnapshotError("error", `the source identity of commit ${commit} cannot be computed: ${e.message}`); }
    }
    const identityPath = path.join(dir, SOURCE_IDENTITY_FILE);
    const keepIdentity = (() => {
      if (!identityBytes) return false;
      const s = lstat(identityPath);
      return !!s?.isFile() && s.nlink === 1n && (process.platform === "win32" || (Number(s.mode) & 0o111) === 0) && fs.readFileSync(identityPath).equals(identityBytes);
    })();
    // GC-A24: a `.git` that holds exactly the gate's config-only origin file (gateGitConfigBody of the checkout's origin, the one
    // allowance snapshotProblem admits) is kept, so class A does not create and remove it on every run; any other `.git` is pruned.
    const keepGit = (() => {
      try {
        if (entries.some((e) => e.path === ".git" || e.path.startsWith(".git/"))) return false;
        const g = path.join(dir, ".git"); const st = lstat(g);
        if (!st?.isDirectory() || st.isSymbolicLink()) return false;
        const names = fs.readdirSync(g); if (names.length !== 1 || names[0] !== "config") return false;
        const c = lstat(path.join(g, "config")); if (!c?.isFile() || c.nlink !== 1n) return false;
        const origin = originUrlOf(repo.checkout);
        return Boolean(origin) && fs.readFileSync(path.join(g, "config"), "utf8") === gateGitConfigBody(origin);
      } catch { return false; }
    })();
    const keepDeps = dependencyDirs(entries, process.platform);
    if (keepGit) keepDeps.add(foldFor(process.platform)(".git"));
    removed += pruneToTree(dir, entries, process.platform, keepIdentity ? new Set([foldFor(process.platform)(SOURCE_IDENTITY_FILE)]) : new Set(), keepDeps);
    if (identityBytes) {
      // Written only when the commit's own ignore rules ignore the name (they are in the snapshot now): an unignored file would change the
      // tree id the broker computes too, so the gate would break every build of that repository. Then nothing is written and the reason is reported.
      if (!neutralIgnored(dir, [SOURCE_IDENTITY_FILE]).has(SOURCE_IDENTITY_FILE)) {
        identity.reason = `the commit's own ignore rules do not ignore /${SOURCE_IDENTITY_FILE}; add it to the commit's .gitignore and no source identity is written to this snapshot until then`;
        if (keepIdentity) fs.unlinkSync(identityPath); // a copy kept from an earlier sync would now be an unignored extra
      } else if (keepIdentity) identity.kept = true;
      else {
        assertLockHeld();
        const stagingRoot2 = `${dir}.staging`;
        const tmp = path.join(stagingRoot2, `identity-${crypto.randomUUID()}`);
        fs.mkdirSync(stagingRoot2, { recursive: true, mode: 0o700 });
        try {
          fs.writeFileSync(tmp, identityBytes, { mode: 0o644, flag: "wx" });
          if (process.platform !== "win32") fs.chmodSync(tmp, 0o644);
          fs.renameSync(tmp, identityPath);
          identity.written = true;
        } finally {
          fs.rmSync(tmp, { force: true });
          try { fs.rmdirSync(stagingRoot2); } catch { /* another sync's staging, or not empty */ }
        }
      }
    }

    assertLockHeld();
    const extra = snapshotProblem({ dir, entries, checkout: repo.checkout, commit });
    if (extra) throw new SnapshotError("mismatch", `snapshot ${dir} is not exactly commit ${commit} after sync: ${extra}`);
    const actual = snapshotTree({ checkout: repo.checkout, dir, baseTree: tree, repo });
    if (actual !== tree) throw new SnapshotError("mismatch", `snapshot ${dir} is tree ${actual} after sync, not tree ${tree} of commit ${commit}`);
    writeState(stateFile, { schema: STATE_SCHEMA, repo: repo.checkout, repoKey: repo.key, commit, tree, paths: [...wanted.keys()], stat: nextStat, syncedAt: new Date(now()).toISOString() });
    return { schema: SNAPSHOT_SCHEMA, path: dir, repoKey: repo.key, commit, tree, written, removed, modesFixed, unchanged, linksRemoved, hardLinksReplaced, casesFixed, identity, ms: now() - started };
  } finally {
    // A lock the caller took is the caller's to release; a lock this sync took ends with it.
    if (!joined) release();
  }
}

/**
 * Verifies, without changing anything, that the snapshot of `checkout` holds exactly the tree of `rev`: its seeded tree id, no path
 * the commit lacks (ignored files included), nothing of another type, mode, link text or hard-link count than the commit's, no link
 * resolving out of the snapshot or into the checkout, and, when `environment` is given, the installed dependency environment the
 * gate recorded after installing. `environment` is `{ expected, compute }`: `expected` is the `env=<sha256>` that
 * `rightkit-gate env-digest --snapshot` printed after the install, `compute({ checkout, dir, commit, tree })` returns the current
 * digest (the caller supplies it: the digest is the gate's, bin/rightkit-gate.mjs classAEnvironmentDigest); a difference is a
 * mismatch, so dependency bytes changed or planted mid-stage refuse at the end verify. Callers run this at the start and again at
 * the end of a stage (the stage-level calls are item 7, 7.08). `managedRoot` and `device` as for syncSnapshot.
 *
 * Stage-output ledger (row 5.09d, snapshotProblem): `ledger` is the array of entries an earlier verify returned (the start of a stage
 * passes the whole ledger and no `admitNew`: nothing new, nothing changed, nothing missing); `admitNew: true` (the END of a stage
 * only) also admits the ignored, untracked, regular-file-or-directory outputs the stage's children wrote. Either option makes the
 * result carry `ledger: [{ path, type, size, sha256, execBit }]`, every admitted entry. The tree id is still the commit's tree:
 * an ignored output does not change it. With neither option nothing beyond the commit (and 5.09's allowances) is accepted.
 */
export function verifySnapshot({ checkout, rev = "HEAD", root = snapshotRoot(), dir, managedRoot = hostManagedRootFor(root), device, environment, ledger, admitNew } = {}) {
  const repo = repositoryOf(checkout);
  const { commit, tree } = resolveCommit(repo.checkout, rev);
  if (managedRoot) requireManagedRoot(managedRoot, { device });
  const at = dir ?? path.join(root, repo.key);
  // The root the snapshot sits directly in is private to this user, as at sync (a root another principal can use proves nothing).
  if (path.resolve(path.dirname(at)) === path.resolve(root)) requirePrivateRoot(root, lstat(root));
  if (!lstat(at)?.isDirectory()) throw new SnapshotError("mismatch", `no snapshot directory at ${at}`);
  // The seeded tree id cannot see an ignored file or follow a link, so the directory itself is walked first (E6, §1 exact snapshot).
  const inspected = snapshotProblem({ dir: at, entries: treeEntries(repo.checkout, commit), checkout: repo.checkout, commit, ledger, admitNew });
  const extra = ledger !== undefined || admitNew ? inspected.problem : inspected;
  if (extra) throw new SnapshotError("mismatch", `the snapshot at ${at} is not exactly commit ${commit}${ledger !== undefined || admitNew ? " plus its stage-output ledger" : ""}: ${extra}`);
  const actual = snapshotTree({ checkout: repo.checkout, dir: at, baseTree: tree, repo });
  if (actual !== tree) throw new SnapshotError("mismatch", `the snapshot at ${at} is tree ${actual}, not tree ${tree} of commit ${commit}`);
  if (environment !== undefined) {
    if (!environment || !/^[0-9a-f]{64}$/.test(String(environment.expected ?? "")) || typeof environment.compute !== "function") throw new SnapshotError("error", "verify with an environment needs { expected: <64 hex env digest>, compute: <function> }");
    let now;
    try { now = environment.compute({ checkout: repo.checkout, dir: at, commit, tree }); }
    catch (e) { throw new SnapshotError("error", `the environment digest of the snapshot at ${at} cannot be computed: ${e.message}`); }
    if (now !== environment.expected) throw new SnapshotError("mismatch", `the installed environment of the snapshot at ${at} changed: env=${now}, recorded env=${environment.expected}`);
  }
  return { schema: SNAPSHOT_SCHEMA, path: at, repoKey: repo.key, commit, tree, ...(environment !== undefined ? { env: environment.expected } : {}), ...(ledger !== undefined || admitNew ? { ledger: inspected.admitted } : {}) };
}

/**
 * `rightkit-gate snapshot [--commit <rev>] [--verify [--env <sha256>]] [--json]`: sync (or with --verify only check) the commit's
 * snapshot. `--env <sha256>` (verify only) also requires the installed dependency environment to still be the recorded
 * `rightkit-gate env-digest --snapshot` digest; the gate supplies `deps.environmentDigest({ checkout, dir, commit, tree })`.
 * Exit 0 the snapshot is the commit's tree and its lock was released, 1 mismatch / refused (unknown
 * revision, a missing managed root, an open-mode root, a gitlink ...) / busy (another process holds the snapshot lock), 2 could not run.
 * A sync runs under the snapshot lock only, taken before the first write: `deps.lockFor({ checkout, commit, root })` (the gate's seam lock on a broker host)
 * returns `{ lock }` (the lock arguments of `acquireSnapshotLock`) or `{ refusal }`; without it the legacy lock is taken. The lock covers the sync alone: this
 * command submits no job. `o.home`, `o.configFile`, `o.managedRoot`, `o.device` are test injections.
 */
export function snapshotCommand(o, args, deps = {}) {
  const allowed = new Set(["--commit", "--verify", "--json", "--env"]);
  for (let i = 0; i < args.length; i++) {
    if (!allowed.has(args[i])) { o.err(`rightkit-gate: snapshot takes only --commit <rev>, --verify, --env <sha256> and --json, not ${JSON.stringify(args[i])}`); return 2; }
    if (args[i] === "--commit") { if (!args[i + 1] || args[i + 1].startsWith("--")) { o.err("rightkit-gate: snapshot --commit needs a revision"); return 2; } i++; }
    if (args[i] === "--env") { if (!/^[0-9a-f]{64}$/.test(args[i + 1] ?? "")) { o.err("rightkit-gate: snapshot --env needs the 64-hex digest `rightkit-gate env-digest --snapshot` printed"); return 2; } i++; }
  }
  const verify = args.includes("--verify");
  const envAt = args.indexOf("--env");
  if (envAt >= 0 && !verify) { o.err("rightkit-gate: snapshot --env only checks a snapshot: it needs --verify"); return 2; }
  if (envAt >= 0 && typeof deps.environmentDigest !== "function") { o.err("rightkit-gate: snapshot --env cannot compute the environment digest in this process"); return 2; }
  const i = args.indexOf("--commit");
  const rev = i >= 0 ? args[i + 1] : "HEAD";
  let code = 2;
  try {
    const platform = o.platform ?? process.platform;
    const root = o.snapshotRoot ?? snapshotRoot({ platform, env: o.env, ...(o.home ? { home: o.home } : {}), ...(o.configFile ? { configFile: o.configFile } : {}) });
    const managedRoot = o.managedRoot !== undefined ? o.managedRoot : hostManagedRootFor(root, { platform });
    // The managed root exists (the gate never creates it) and the root is private, BEFORE the lock is taken: taking it
    // writes into the root, and must never be what brings a missing managed root into being.
    guardRoot(root, managedRoot, { device: o.device });
    let lock = {};
    if (!verify && typeof deps.lockFor === "function") {
      const taken = deps.lockFor({ checkout: o.cwd, commit: resolveCommit(repositoryOf(o.cwd).checkout, rev).commit, root });
      if (taken?.refusal) { o.err(taken.refusal); return 1; }
      lock = taken?.lock ?? {};
    }
    // A sync replaces snapshot bytes, so it runs only under the snapshot lock, taken before the first write and released when the sync ends (this
    // command submits no job); a verify only reads.
    const r = verify ? verifySnapshot({ checkout: o.cwd, rev, root, managedRoot, device: o.device, ...(envAt >= 0 ? { environment: { expected: args[envAt + 1], compute: deps.environmentDigest } } : {}) })
      : syncSnapshot({ checkout: o.cwd, rev, root, managedRoot, device: o.device, lock: { body: lock.body ?? null, judge: lock.judge ?? null, ...(lock.identityOf ? { identityOf: lock.identityOf } : {}) } });
    if (args.includes("--json")) o.out(JSON.stringify(r));
    else o.out(`rightkit-gate: snapshot ${r.path} commit=${r.commit} tree=${r.tree}${"written" in r ? ` written=${r.written} removed=${r.removed} modes=${r.modesFixed} links=${r.linksRemoved} unchanged=${r.unchanged} ms=${r.ms}` : " verified"}`);
    code = 0;
  } catch (e) {
    o.err(`rightkit-gate: snapshot ${e.code ?? "error"}${e.reason ? ` (${e.reason})` : ""}: ${e.message}`);
    code = e instanceof SnapshotError && e.code !== "error" ? 1 : 2;
  }
  return code;
}
