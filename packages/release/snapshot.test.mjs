// Fixtures for the gate-owned commit snapshot (snapshot.mjs). Each test names the clause of docs/GATE-CONTRACT.md it holds:
// §1 "Commit-only build" (content sync, mtimes kept, obsolete removed, types/modes, no link into the checkout, the checkout never
// read beyond its commits), §1 "Broker interface" (root, repo-key, the seeded identity of rightkit#1255), E1 (a dirty checkout does
// not block and none of it reaches the snapshot) and E6 (tampered content, types, modes, planted files and links refuse).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawn, spawnSync } from "./background-process.mjs";
import { SnapshotError, hostManagedRootFor, keyPath, neutralIgnored, repoKey, repositoryOf, requireManagedRoot, resolveCommit, treeEntries, snapshotCommand, snapshotRoot, snapshotTree, syncSnapshot, verifySnapshot, acquireSnapshotLock } from "./snapshot.mjs";
import { holderFile, ownershipFile, readHolder } from "./build-ownership.mjs";
import { HOST_CONFIG_SCHEMA, hostConfigFile, readSnapshotRootConfig } from "./snapshot-root-config.mjs";
import { main, classAEnvironmentDigest, installedDependencyDigest } from "./bin/rightkit-gate.mjs";
import { commitFiles } from "./snapshot-check.mjs";
import { SEALED_ATTRIBUTES, SEALED_GIT, createSealedGitDir, sealedConfigArgs, sealedEnv } from "./snapshot-git.mjs";
import { pathToFileURL } from "node:url";

const posixOnly = process.platform === "win32" ? "POSIX process semantics (pid 1)" : false;
const emptyConfig = path.join(fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-snap-cfg-"))), "gitconfig");
fs.writeFileSync(emptyConfig, "");
// Fixture repositories see no global or system git configuration (no user excludes file, no fsmonitor): the results are the method's.
Object.assign(process.env, { GIT_CONFIG_GLOBAL: emptyConfig, GIT_CONFIG_NOSYSTEM: "1", GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t" });

function git(cwd, ...args) {
  const r = spawnSync("git", args, { cwd, encoding: "utf8" });
  if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr}`);
  return r.stdout.trim();
}
function tmp(t, prefix) {
  const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
  t.after(() => fs.rmSync(d, { recursive: true, force: true }));
  return d;
}
function write(root, files) {
  for (const [name, body] of Object.entries(files)) {
    const p = path.join(root, ...name.split("/"));
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.writeFileSync(p, body);
  }
}
function commitAll(repo, message = "c") { git(repo, "add", "-A"); git(repo, "commit", "-q", "--allow-empty", "-m", message); return git(repo, "rev-parse", "HEAD"); }
// Links. Windows creates symbolic links only with Developer Mode or elevation. Where it can, every link case below runs for real
// (the Dell session is elevated); where it cannot, the asserted behaviour is that a commit holding a link fails closed: the sync
// refuses (SnapshotError "error") and the snapshot it leaves is not reported as the commit's (verification refuses it).
const canLink = (() => {
  let d = null;
  try { d = fs.mkdtempSync(path.join(os.tmpdir(), "rk-snap-link-")); fs.writeFileSync(path.join(d, "a"), "x"); fs.symlinkSync("a", path.join(d, "l")); return true; }
  catch { return false; }
  finally { if (d) fs.rmSync(d, { recursive: true, force: true }); }
})();
/** Commits a symbolic link (mode 120000) through the index, so the fixture does not depend on the host git's core.symlinks (the Dell's is false). */
function commitLink(repo, name, to, message = "link") {
  git(repo, "update-index", "--add", "--cacheinfo", `120000,${blobOf(repo, to)},${name}`);
  git(repo, "commit", "-q", "-m", message);
  return git(repo, "rev-parse", "HEAD");
}
/** The repository path as a link target reaches it from the filesystem root: no drive, forward slashes (a chain of `..` ends at the root). */
const fromRoot = (p) => (process.platform === "win32" ? p.slice(2).replace(/\\/g, "/").replace(/^\//, "") : p.slice(1));
/** Without symlink privilege: a commit with a link refuses the sync and what is left behind is not the commit. */
function assertNoLinkHost(t) {
  const f = fixture(t);
  const c = commitLink(f.repo, "src/alias.rs", "lib.rs");
  assert.throws(() => syncSnapshot({ checkout: f.repo, rev: c, root: f.root }), (e) => e instanceof SnapshotError && e.code === "error" && /alias\.rs/.test(e.message) && /symbolic links/.test(e.message));
  assert.equal(fs.existsSync(path.join(f.dir, "src", "alias.rs")), false);
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: c, root: f.root }), (e) => e.code === "mismatch");
}

function fixture(t, files = { "Cargo.toml": "[workspace]\n", "src/lib.rs": "pub fn a() {}\n", "README.md": "x\n", ".gitignore": "target/\nnode_modules/\n*.log\n" }) {
  const repo = tmp(t, "rk-snap-repo-");
  git(repo, "init", "-q", "-b", "main");
  write(repo, files);
  const commit = commitAll(repo);
  // Each fixture's root has a parent of its own, so the owner record derived beside it (ownershipFile beside: root) is its own too:
  // one test's kept or stale record can never make a later test busy.
  const root = path.join(tmp(t, "rk-snap-own-"), "root");
  fs.mkdirSync(root, { mode: 0o700 });
  return { repo, root, commit, dir: path.join(root, repoKey(keyPath(repo))) };
}
function mtimes(dir) {
  const out = {};
  const walk = (d) => { for (const e of fs.readdirSync(d, { withFileTypes: true })) { const p = path.join(d, e.name); if (e.isDirectory()) walk(p); else out[path.relative(dir, p)] = fs.lstatSync(p, { bigint: true }).mtimeNs; } };
  walk(dir);
  return out;
}

/** A per-user host configuration file naming `snapshotRoot` (or any body), in its own temporary home; returns the file's path. */
function hostConfig(t, body, { home = tmp(t, "rk-snap-home-") } = {}) {
  const file = path.join(home, ".config", "rightkit", "gate.json");
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, typeof body === "string" ? body : JSON.stringify({ schema: HOST_CONFIG_SCHEMA, ...body }));
  return file;
}
/** A configuration file location that holds nothing. */
const noConfig = (t) => path.join(tmp(t, "rk-snap-noconfig-"), ".config", "rightkit", "gate.json");

test("§1 root & repo-key: Mac/Dell under the managed data root, Linux config file, else XDG state, else ~/.local/state; key = sha256(canonical path)[:16]", (t) => {
  const configFile = noConfig(t);
  assert.equal(repoKey("/Volumes/D/claude/rightkit"), crypto.createHash("sha256").update("/Volumes/D/claude/rightkit").digest("hex").slice(0, 16));
  // The keys the broker's checked-trees directories already use on each host (read 2026-10-29): Mac hashes the path as is,
  // Dell the verbatim form Rust's fs::canonicalize returns.
  assert.equal(repoKey(keyPath("/Volumes/D/claude/rightkit", "darwin")), "770a65dcafd91981");
  assert.equal(keyPath("D:\\Claude\\rightkit", "win32"), "\\\\?\\D:\\Claude\\rightkit");
  assert.equal(repoKey(keyPath("D:\\Claude\\rightkit", "win32")), "226c20f30c489153");
  assert.equal(keyPath("\\\\?\\D:\\x", "win32"), "\\\\?\\D:\\x");
  assert.equal(keyPath("\\\\srv\\share\\repo", "win32"), "\\\\?\\UNC\\srv\\share\\repo");
  assert.equal(keyPath("/home/a/repo", "linux"), "/home/a/repo");
  assert.equal(snapshotRoot({ platform: "darwin", env: {}, home: "/Users/a" }), "/Volumes/D/.rightkit-managed/snapshots");
  // Mac & Dell: the host's fixed managed root, never an environment variable (review item5-r1 finding 9).
  assert.equal(snapshotRoot({ platform: "darwin", env: { RIGHTKIT_DATA_ROOT: "/Volumes/E/.rightkit-managed", RIGHTKIT_MANAGED_ROOT: "/x", RIGHTKIT_SNAPSHOT_ROOT: "/y/snaps", XDG_STATE_HOME: "/s" }, home: "/Users/a" }), "/Volumes/D/.rightkit-managed/snapshots");
  assert.equal(snapshotRoot({ platform: "win32", env: { RIGHTKIT_DATA_ROOT: "E:\\m", RIGHTKIT_SNAPSHOT_ROOT: "E:\\snaps" }, home: "C:\\Users\\a" }), "D:\\.rightkit-managed\\snapshots");
  assert.equal(snapshotRoot({ platform: "win32", env: {}, home: "C:\\Users\\a" }), "D:\\.rightkit-managed\\snapshots");
  // Linux (row 5.16): with no configuration file, the contract's named default; with neither variable, ~/.local/state.
  assert.equal(snapshotRoot({ platform: "linux", env: { XDG_STATE_HOME: "/s" }, home: "/home/a", configFile }), "/s/rightkit/snapshots");
  assert.equal(snapshotRoot({ platform: "linux", env: {}, home: "/home/a", configFile }), "/home/a/.local/state/rightkit/snapshots");
  assert.equal(snapshotRoot({ platform: "linux", env: { XDG_STATE_HOME: "relative" }, home: "/home/a", configFile }), "/home/a/.local/state/rightkit/snapshots");
  // The root a host configuration file names wins over XDG state; RIGHTKIT_SNAPSHOT_ROOT is not a source at all.
  assert.equal(snapshotRoot({ platform: "linux", env: { XDG_STATE_HOME: "/s" }, home: "/home/a", configFile: hostConfig(t, { snapshotRoot: "/data/snaps" }) }), "/data/snaps");
  assert.equal(snapshotRoot({ platform: "linux", env: { RIGHTKIT_SNAPSHOT_ROOT: "/data/snaps", XDG_STATE_HOME: "/s" }, home: "/home/a", configFile }), "/s/rightkit/snapshots", "the variable of an earlier round changes nothing");
  assert.equal(snapshotRoot({ platform: "linux", env: { RIGHTKIT_SNAPSHOT_ROOT: "/data/snaps" }, home: "/home/a", configFile: hostConfig(t, { snapshotRoot: "/data/real" }) }), "/data/real");
});

// Review item5-r1 finding 9 (GATE-CONTRACT §1: the root is durable, "scratch is never the snapshot root because it is disposable").
const refusesRoot = (opts, source) => assert.throws(() => snapshotRoot(opts), (e) => e instanceof SnapshotError && e.code === "error" && e.message.includes(source) && /durable/.test(e.message), JSON.stringify(opts));
const CONFIG_SOURCE = "gate host configuration";

test("§1 root durability: a root under a scratch root or a temp directory is refused on every host, naming where the root came from", (t) => {
  const linux = (env, configFile = noConfig(t)) => ({ platform: "linux", env, home: "/home/a", configFile });
  for (const root of ["/Volumes/D/scratch/snaps", "/Volumes/D/scratch", "/tmp/snaps", "/private/tmp/x/snaps", "/var/tmp/snaps", "/private/var/folders/ab/cd/T/snaps", "/var/folders/ab/cd/T/snaps", ...(process.platform === "win32" ? [] : [path.join(os.tmpdir(), "snaps")])]) {
    refusesRoot(linux({}, hostConfig(t, { snapshotRoot: root })), CONFIG_SOURCE);
  }
  refusesRoot(linux({ XDG_STATE_HOME: "/Volumes/D/scratch/state" }), "XDG_STATE_HOME");
  refusesRoot(linux({ XDG_STATE_HOME: "/tmp/state" }), "XDG_STATE_HOME");
  refusesRoot({ platform: "linux", env: {}, home: "/tmp/home", configFile: noConfig(t) }, "the home directory /tmp/home");
  // A temp directory the host names by environment (TMPDIR / TEMP / TMP) counts too, and a root outside every one of them is fine.
  refusesRoot(linux({ TMPDIR: "/data/t" }, hostConfig(t, { snapshotRoot: "/data/t/snaps" })), CONFIG_SOURCE);
  assert.equal(snapshotRoot(linux({ TMPDIR: "/data/t" }, hostConfig(t, { snapshotRoot: "/data/snaps" }))), "/data/snaps");
  // A TMPDIR of "/" must not turn every root into a refusal.
  assert.equal(snapshotRoot(linux({ TMPDIR: "/" }, hostConfig(t, { snapshotRoot: "/data/snaps" }))), "/data/snaps");
  // Windows: the Dell's fixed root is fine; the same check reads %TEMP% & friends, case-insensitively, with Windows paths.
  assert.equal(snapshotRoot({ platform: "win32", env: { TEMP: "C:\\Users\\a\\AppData\\Local\\Temp" }, home: "C:\\Users\\a" }), "D:\\.rightkit-managed\\snapshots");
  refusesRoot({ platform: "win32", env: { TEMP: "d:\\.RIGHTKIT-MANAGED" }, home: "C:\\Users\\a" }, "the host's managed root");
});

test("§1 root: no environment variable relocates the Mac/Dell root; the one-time test variable of an earlier round is not read", (t) => {
  const ignored = { RIGHTKIT_SNAPSHOT_TEST_RUN: "zzz", RIGHTKIT_SNAPSHOT_TEST_ROOT: "/y", RIGHTKIT_SNAPSHOT_ROOT: "/y", RIGHTKIT_DATA_ROOT: "/y", XDG_STATE_HOME: "/y" };
  assert.equal(snapshotRoot({ platform: "darwin", env: ignored, home: "/Users/a" }), "/Volumes/D/.rightkit-managed/snapshots");
  assert.equal(snapshotRoot({ platform: "win32", env: ignored, home: "C:\\Users\\a" }), "D:\\.rightkit-managed\\snapshots");
  // Linux reads only the host configuration file and XDG state; the test variable changes neither.
  const configFile = noConfig(t);
  assert.equal(snapshotRoot({ platform: "linux", env: { RIGHTKIT_SNAPSHOT_TEST_RUN: "zzz", XDG_STATE_HOME: "/s" }, home: "/home/a", configFile }), "/s/rightkit/snapshots");
  assert.equal(snapshotRoot({ platform: "linux", env: { RIGHTKIT_SNAPSHOT_TEST_RUN: "zzz", RIGHTKIT_SNAPSHOT_ROOT: "/data/snaps" }, home: "/home/a", configFile: hostConfig(t, { snapshotRoot: "/data/real" }) }), "/data/real");
  // And the owner record, derived from the root, follows the same fixed path.
  const owner = (env) => ownershipFile({ platform: "darwin", env, home: "/Users/a" });
  assert.equal(owner({ RIGHTKIT_SNAPSHOT_TEST_RUN: "zzz" }), owner({}));
  assert.equal(owner({}), "/Volumes/D/.rightkit-managed/build-ownership/owner.json");
});

test("§1 root durability: a sibling named like `..x` is inside scratch or temp, not outside (a path is outside only as `..` or `..<sep>…`)", (t) => {
  const linux = (root) => ({ platform: "linux", env: {}, home: "/home/a", configFile: hostConfig(t, { snapshotRoot: root }) });
  for (const root of ["/Volumes/D/scratch/..x/s", "/tmp/..x", "/tmp/...", "/private/var/folders/..cache/T/s"]) refusesRoot(linux(root), CONFIG_SOURCE);
  // The configuration file demands a normalised path, so a root reached through `..` is refused as written, not collapsed.
  assert.throws(() => snapshotRoot(linux("/Volumes/D/scratch/../data/s")), (e) => e.code === "error" && /normalised/.test(e.message));
  assert.equal(snapshotRoot(linux("/Volumes/D/..scratch/s")), "/Volumes/D/..scratch/s", "a sibling of scratch whose name starts with `..` is not under it");
  assert.equal(snapshotRoot(linux("/data/..x/snaps")), "/data/..x/snaps");
});

// Row 5.16: the Linux root comes from one per-user host configuration file and nothing else the process can be handed.
test("§1 Linux host configuration (5.16): the file is ~/.config/rightkit/gate.json in the account's home; a bad file refuses, never falls back; no variable selects it", { skip: process.platform === "win32" ? "POSIX ownership and modes" : false }, (t) => {
  const home = tmp(t, "rk-snap-cfg-home-");
  assert.equal(hostConfigFile(home), path.posix.join(home, ".config", "rightkit", "gate.json"));
  const refuses = (body, re, name) => assert.throws(() => snapshotRoot({ platform: "linux", env: {}, home: "/home/a", configFile: hostConfig(t, body) }), (e) => e instanceof SnapshotError && e.code === "error" && re.test(e.message), name);
  refuses("{not json", /cannot be read as JSON/, "malformed");
  refuses({ schema: "rightkit.gate-host-config/0", snapshotRoot: "/data/s" }, /schema/, "wrong schema");
  refuses({ snapshotRoot: "relative/s" }, /relative/, "relative root");
  refuses({ snapshotRoot: "/data/s/" }, /normalised/, "trailing separator");
  refuses({ snapshotRoot: "/data//s" }, /normalised/, "doubled separator");
  refuses({ snapshotRoot: "/" }, /normalised/, "the filesystem root");
  refuses({ snapshotRoot: 7 }, /non-empty string/, "not a string");
  refuses({ snapshotroot: "/data/s" }, /unknown key/, "a misspelt key would silently select the default root");
  // A file another principal can change, a link (it could live inside a repository) and another user's file are not host configuration.
  const open = hostConfig(t, { snapshotRoot: "/data/s" });
  fs.chmodSync(open, 0o666);
  assert.throws(() => snapshotRoot({ platform: "linux", env: {}, home: "/home/a", configFile: open }), (e) => /writable by group or other/.test(e.message));
  const real = hostConfig(t, { snapshotRoot: "/data/s" });
  const linked = path.join(tmp(t, "rk-snap-cfg-link-"), "gate.json");
  fs.symlinkSync(real, linked);
  assert.throws(() => snapshotRoot({ platform: "linux", env: {}, home: "/home/a", configFile: linked }), (e) => /not a regular file/.test(e.message));
  assert.throws(() => readSnapshotRootConfig(real, { getuid: () => process.getuid() + 1 }), (e) => /owned by uid/.test(e.message));
  // The directory that holds the file must not be writable by group or other either.
  const looseHome = tmp(t, "rk-snap-cfg-loose-");
  const loose = hostConfig(t, { snapshotRoot: "/data/s" }, { home: looseHome });
  fs.chmodSync(path.dirname(loose), 0o777);
  assert.throws(() => snapshotRoot({ platform: "linux", env: {}, home: "/home/a", configFile: loose }), (e) => /directory/.test(e.message));
  fs.chmodSync(path.dirname(loose), 0o755);
  // A valid file is honoured; a file with no snapshotRoot leaves the contract's default.
  assert.equal(snapshotRoot({ platform: "linux", env: {}, home: "/home/a", configFile: hostConfig(t, { snapshotRoot: "/data/s" }) }), "/data/s");
  assert.equal(snapshotRoot({ platform: "linux", env: { XDG_STATE_HOME: "/s" }, home: "/home/a", configFile: hostConfig(t, {}) }), "/s/rightkit/snapshots");
  // Neither HOME nor XDG_CONFIG_HOME nor any RIGHTKIT variable moves the file: it is found from the account database.
  const mover = { HOME: home, XDG_CONFIG_HOME: home, RIGHTKIT_SNAPSHOT_ROOT: "/data/hostile", RIGHTKIT_CONFIG: home };
  const saved = { ...process.env };
  Object.assign(process.env, mover);
  t.after(() => { for (const k of Object.keys(mover)) { if (k in saved) process.env[k] = saved[k]; else delete process.env[k]; } });
  assert.equal(hostConfigFile(), path.posix.join(os.userInfo().homedir, ".config", "rightkit", "gate.json"), "HOME does not select the configuration file: the account database does");
  assert.notEqual(hostConfigFile(), path.posix.join(home, ".config", "rightkit", "gate.json"));
});

test("snapshot CLI: an injected snapshotRoot is the root, and the environment is not consulted for it", (t) => {
  const f = fixture(t);
  const out = [], err = [];
  const env = { ...process.env, RIGHTKIT_SNAPSHOT_ROOT: "/Volumes/D/scratch/never", RIGHTKIT_SNAPSHOT_TEST_RUN: "x/../y" };
  const code = snapshotCommand({ cwd: f.repo, env, platform: process.platform, snapshotRoot: f.root, out: (m) => out.push(m), err: (m) => err.push(m), acquireOwnership: (q) => { assert.equal(q.snapshot, f.dir); assert.equal(q.file, path.join(path.dirname(f.root), "build-ownership", "owner.json")); return { handle: "h", joined: false, reclaimed: null, release: () => ({ released: true }) }; } }, ["--commit", f.commit, "--json"]);
  assert.equal(code, 0, err.join("\n"));
  assert.equal(JSON.parse(out.at(-1)).path, f.dir);
  // Without the injection the root is the host's: on Linux the host configuration file's (here a scratch root: refused before anything
  // runs); the environment variable of the same name names nothing.
  const refused = [];
  assert.equal(snapshotCommand({ cwd: f.repo, env, platform: "linux", home: "/home/a", configFile: hostConfig(t, { snapshotRoot: "/Volumes/D/scratch/never" }), out: () => {}, err: (m) => refused.push(m), acquireOwnership: () => { throw new Error("must not acquire"); } }, ["--commit", f.commit]), 2);
  assert.match(refused.join("\n"), /gate host configuration.*scratch root/);
  const ignored = [];
  assert.equal(snapshotCommand({ cwd: f.repo, env, platform: "linux", home: "/home/a", configFile: noConfig(t), snapshotRoot: undefined, out: () => {}, err: (m) => ignored.push(m), acquireOwnership: () => { throw new Error("stop here: the root was resolved without the variable"); } }, ["--commit", f.commit]), 2);
  assert.doesNotMatch(ignored.join("\n"), /scratch root/, "RIGHTKIT_SNAPSHOT_ROOT was not read");
});

test("§1 commit-only build & E1: the snapshot is the commit's tree; a dirty checkout neither blocks nor leaks; the checkout is untouched", (t) => {
  const f = fixture(t);
  write(f.repo, { "src/lib.rs": "pub fn dirty() {}\n", "untracked.rs": "x\n" });
  const statusBefore = git(f.repo, "status", "--porcelain", "--untracked-files=all");
  const indexBefore = fs.readFileSync(path.join(f.repo, ".git", "index"));
  const r = syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  assert.equal(r.path, f.dir);
  assert.equal(r.tree, git(f.repo, "rev-parse", `${f.commit}^{tree}`));
  assert.equal(fs.readFileSync(path.join(f.dir, "src/lib.rs"), "utf8"), "pub fn a() {}\n");
  assert.equal(fs.existsSync(path.join(f.dir, "untracked.rs")), false);
  assert.equal(fs.existsSync(path.join(f.dir, ".git")), false);
  assert.equal(git(f.repo, "status", "--porcelain", "--untracked-files=all"), statusBefore);
  assert.deepEqual(fs.readFileSync(path.join(f.repo, ".git", "index")), indexBefore);
  assert.deepEqual(fs.readdirSync(path.join(f.repo, ".git")).filter((n) => n.startsWith("rightkit-snapshot-")), [], "temporary indexes are removed");
});

test("§1 warm: a repeat sync writes nothing and keeps every mtime; a one-file commit writes that file only", (t) => {
  const f = fixture(t);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  const before = mtimes(f.dir);
  const again = syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  assert.deepEqual([again.written, again.removed, again.modesFixed, again.linksRemoved], [0, 0, 0, 0]);
  assert.deepEqual(mtimes(f.dir), before);
  write(f.repo, { "src/lib.rs": "pub fn b() {}\n" });
  const next = commitAll(f.repo);
  const r = syncSnapshot({ checkout: f.repo, rev: next, root: f.root });
  assert.deepEqual([r.written, r.removed], [1, 0]);
  const after = mtimes(f.dir);
  for (const [file, ns] of Object.entries(before)) if (file !== path.join("src", "lib.rs")) assert.equal(after[file], ns, `${file} kept its mtime`);
  assert.notEqual(after[path.join("src", "lib.rs")], before[path.join("src", "lib.rs")]);
});

test("§1 obsolete files: a file the next commit deletes is removed, including one an ignore rule matches", (t) => {
  const f = fixture(t);
  write(f.repo, { "kept.log": "committed despite *.log\n", "old.rs": "x\n" });
  git(f.repo, "add", "-f", "kept.log", "old.rs");
  const withLog = commitAll(f.repo);
  syncSnapshot({ checkout: f.repo, rev: withLog, root: f.root });
  assert.ok(fs.existsSync(path.join(f.dir, "kept.log")));
  git(f.repo, "rm", "-q", "kept.log", "old.rs");
  const without = commitAll(f.repo);
  const r = syncSnapshot({ checkout: f.repo, rev: without, root: f.root });
  assert.equal(r.removed, 2);
  assert.equal(fs.existsSync(path.join(f.dir, "kept.log")), false, "an ignored stale file would be unattested build input");
  assert.equal(fs.existsSync(path.join(f.dir, "old.rs")), false);
});

test("§1 interrupted sync: a file an interrupted sync left behind (ignored by the repository) is removed by the next sync", (t) => {
  const f = fixture(t);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  const state = JSON.parse(fs.readFileSync(`${f.dir}.state.json`, "utf8"));
  fs.writeFileSync(`${f.dir}.state.json`, JSON.stringify({ ...state, pending: [...state.paths, "half.log"] }));
  write(f.dir, { "half.log": "left by an interrupted sync\n" });
  const r = syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  assert.equal(r.removed, 1);
  assert.equal(fs.existsSync(path.join(f.dir, "half.log")), false);
});

test("rightkit#1255: a committed file an ignore rule matches stays in the id; an empty-index start would get it wrong", (t) => {
  const f = fixture(t);
  write(f.repo, { "kept.log": "force-added\n" });
  git(f.repo, "add", "-f", "kept.log");
  const commit = commitAll(f.repo);
  const tree = git(f.repo, "rev-parse", `${commit}^{tree}`);
  const r = syncSnapshot({ checkout: f.repo, rev: commit, root: f.root });
  assert.equal(r.tree, tree);
  // The unseeded method (empty temporary index) drops kept.log and yields another id: the reason the seed is required.
  const index = path.join(f.repo, ".git", "unseeded.index");
  const env = { ...process.env, GIT_INDEX_FILE: index };
  const run = (...a) => spawnSync("git", [`--git-dir=${path.join(f.repo, ".git")}`, `--work-tree=${f.dir}`, ...a], { cwd: f.dir, env, encoding: "utf8" });
  run("add", "-A");
  const unseeded = run("write-tree").stdout.trim();
  fs.rmSync(index, { force: true });
  assert.notEqual(unseeded, tree);
  // Seeded, deleting the ignored committed file changes the id (an empty start is blind to it).
  fs.rmSync(path.join(f.dir, "kept.log"));
  assert.notEqual(snapshotTree({ checkout: f.repo, dir: f.dir, baseTree: tree }), tree);
});

test("E6: a changed byte, a deleted file, a planted visible file refuse verification; the next sync repairs them", (t) => {
  const f = fixture(t);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  const refuses = () => assert.throws(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e instanceof SnapshotError && e.code === "mismatch");
  verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  fs.appendFileSync(path.join(f.dir, "src/lib.rs"), "// tampered\n");
  refuses();
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  fs.rmSync(path.join(f.dir, "README.md"));
  refuses();
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  write(f.dir, { "src/planted.rs": "pub fn x() {}\n" });
  refuses();
  const r = syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  assert.equal(r.removed, 1);
  assert.equal(fs.existsSync(path.join(f.dir, "src/planted.rs")), false);
  verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
});

// Review item5-r1 finding 1 (E6, GATE-CONTRACT §1 "exact snapshot", "no link inside the snapshot may resolve into the editable
// checkout"): the seeded tree id ignores what an ignore rule matches and follows no link, so verification also walks the directory.
const refusesPath = (f, name, rev = f.commit) => assert.throws(() => verifySnapshot({ checkout: f.repo, rev, root: f.root }), (e) => e instanceof SnapshotError && e.code === "mismatch" && e.message.includes(name), `verify refuses ${name}`);
const dirLinkType = process.platform === "win32" ? "junction" : "dir";

test("E6 exact: a planted ignored file, a planted untracked file and an empty directory refuse verification naming the path; the next sync repairs", (t) => {
  const f = fixture(t);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  write(f.dir, { "target/evil.rs": "pub fn evil() {}\n" });
  refusesPath(f, "target");
  fs.rmSync(path.join(f.dir, "target"), { recursive: true });
  write(f.dir, { "planted.log": "ignored by *.log\n" });
  refusesPath(f, "planted.log");
  fs.rmSync(path.join(f.dir, "planted.log"));
  write(f.dir, { "src/planted.rs": "visible\n" });
  refusesPath(f, "src/planted.rs");
  fs.rmSync(path.join(f.dir, "src/planted.rs"));
  fs.mkdirSync(path.join(f.dir, "src", "empty"));
  refusesPath(f, "src/empty");
  fs.rmSync(path.join(f.dir, "src", "empty"), { recursive: true });
  verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  write(f.dir, { "src/nested/deep/build.o": "x\n", "planted.log": "x\n" });
  const r = syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  assert.equal(r.removed, 2);
  verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
});

test("E6 exact: a planted directory link or junction (into the checkout, outside the snapshot, or inside it) refuses verification", (t) => {
  const f = fixture(t);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  const elsewhere = tmp(t, "rk-snap-elsewhere-");
  fs.mkdirSync(path.join(f.repo, "outputs"));
  const link = path.join(f.dir, "target");
  // Into the editable checkout: ignored by the tree id (target/ is an ignore rule), refused here.
  fs.symlinkSync(path.join(f.repo, "outputs"), link, dirLinkType);
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e.code === "mismatch" && /target/.test(e.message));
  fs.rmSync(link, { recursive: true, force: true });
  // Outside the snapshot, elsewhere on the disk.
  fs.symlinkSync(elsewhere, link, dirLinkType);
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e.code === "mismatch" && /target/.test(e.message));
  fs.rmSync(link, { recursive: true, force: true });
  // Even a link that stays inside the snapshot is not in the commit's tree.
  fs.symlinkSync(path.join(f.dir, "src"), link, dirLinkType);
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e.code === "mismatch" && /target/.test(e.message));
  fs.rmSync(link, { recursive: true, force: true });
  verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
});

test("E6 exact: a planted file link into the checkout or out of the snapshot (an ignored name, a visible name) refuses verification", (t) => {
  if (!canLink) return assertNoLinkHost(t);
  const f = fixture(t);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  write(f.repo, { "secret.rs": "uncommitted\n" });
  const elsewhere = tmp(t, "rk-snap-elsewhere-");
  write(elsewhere, { "x.rs": "outside\n" });
  fs.mkdirSync(path.join(f.dir, "target"));
  const cases = [["target/escape.rs", path.join(f.repo, "secret.rs")], ["notes.log", path.join(f.repo, "secret.rs")], ["target/outside.rs", path.join(elsewhere, "x.rs")], ["src/alias.rs", "lib.rs"]];
  for (const [name, to] of cases) {
    fs.symlinkSync(to, path.join(f.dir, ...name.split("/")));
    assert.throws(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e.code === "mismatch" && e.message.includes(name.split("/")[0]), name);
    fs.rmSync(path.join(f.dir, ...name.split("/")));
  }
  fs.rmSync(path.join(f.dir, "target"), { recursive: true });
  verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
});

test("E6 exact: a committed link that is retargeted, dangles or leaves the snapshot refuses verification naming the link", (t) => {
  if (!canLink) return assertNoLinkHost(t);
  const f = fixture(t);
  const c = commitLink(f.repo, "src/alias.rs", "lib.rs");
  syncSnapshot({ checkout: f.repo, rev: c, root: f.root });
  const alias = path.join(f.dir, "src", "alias.rs");
  fs.rmSync(alias);
  fs.symlinkSync(path.join(f.repo, "src", "lib.rs"), alias);
  refusesPath(f, "src/alias.rs", c);
  fs.rmSync(alias);
  fs.symlinkSync("missing.rs", alias);
  refusesPath(f, "src/alias.rs", c);
  fs.rmSync(alias);
  fs.symlinkSync("lib.rs", alias);
  verifySnapshot({ checkout: f.repo, rev: c, root: f.root });
});

// Review item5-r2 #4: a directory whose name starts with `..` is inside the snapshot, not a way out of it.
test("E6 exact: a committed link into a `..x` directory of the snapshot is inside it, and a planted one is refused only as an extra", (t) => {
  if (!canLink) return assertNoLinkHost(t);
  const f = fixture(t, { "Cargo.toml": "[workspace]\n", "src/lib.rs": "pub fn a() {}\n", "..x/a.rs": "pub fn x() {}\n", ".gitignore": "target/\n" });
  const c = commitLink(f.repo, "src/alias.rs", "../..x/a.rs");
  const r = syncSnapshot({ checkout: f.repo, rev: c, root: f.root });
  // Windows reports the target with backslashes; the link is the same one.
  assert.equal(fs.readlinkSync(path.join(f.dir, "src", "alias.rs")).replace(/\\/g, "/"), "../..x/a.rs");
  assert.equal(fs.readFileSync(path.join(f.dir, "src", "alias.rs"), "utf8"), "pub fn x() {}\n", "the link reads the file in the `..x` directory");
  assert.equal(r.linksRemoved, 0);
  assert.equal(verifySnapshot({ checkout: f.repo, rev: c, root: f.root }).commit, c);
  // A repeat sync leaves the link as it is (separator spelling is not a change): nothing written.
  const again = syncSnapshot({ checkout: f.repo, rev: c, root: f.root });
  assert.deepEqual([again.written, again.removed], [0, 0]);
  // A planted link into `..x` is a link the commit lacks, refused for that (not as an escape).
  fs.symlinkSync(path.join(f.dir, "..x", "a.rs"), path.join(f.dir, "planted.rs"));
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: c, root: f.root }), (e) => e.code === "mismatch" && /planted\.rs is not in the commit's tree/.test(e.message));
});

const dependencyFixture = (t) => fixture(t, { "package.json": "{\"name\":\"root\",\"private\":true}\n", "pnpm-lock.yaml": "lockfileVersion: '9.0'\n", "apps/web/package.json": "{\"name\":\"web\"}\n", "tools/package.json": "{\"name\":\"tools\"}\n", "src/lib.rs": "x\n", ".gitignore": "target/\nnode_modules/\n" });

test("E6 exact: installed pnpm dependencies are the one allowed extra: named node_modules under a lockfile root only; links in them still may not leave the snapshot", (t) => {
  const f = dependencyFixture(t);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  // What installSnapshotDependencies leaves: node_modules at the root and at a workspace member, a link inside the virtual store.
  write(f.dir, { "node_modules/.pnpm/dep@1.0.0/node_modules/dep/index.js": "module.exports = 1;\n", "node_modules/.modules.yaml": "layoutVersion: 5\n", "apps/web/node_modules/.keep": "x\n" });
  fs.symlinkSync(path.join(f.dir, "node_modules", ".pnpm", "dep@1.0.0", "node_modules", "dep"), path.join(f.dir, "node_modules", "dep"), dirLinkType);
  assert.equal(verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }).commit, f.commit);
  // Not an allowance for anything else ignored: build outputs, an unrelated ignored file, a node_modules the commit's layout does not host.
  for (const [name, offender, cleanup] of [["target/debug/x.o", "target", "target"], ["src/node_modules/x.js", "src/node_modules", "src/node_modules"], ["node_modules_extra/x.js", "node_modules_extra", "node_modules_extra"]]) {
    write(f.dir, { [name]: "x\n" });
    refusesPath(f, offender);
    fs.rmSync(path.join(f.dir, ...cleanup.split("/")), { recursive: true, force: true });
  }
  verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  // A link inside the dependency directory into the editable checkout, and one out of the snapshot, refuse.
  const elsewhere = tmp(t, "rk-snap-elsewhere-");
  fs.mkdirSync(path.join(f.repo, "src-out"));
  for (const [to, why] of [[path.join(f.repo, "src-out"), /resolving into the editable checkout/], [elsewhere, /resolving outside the snapshot/]]) {
    const link = path.join(f.dir, "node_modules", "escape");
    fs.symlinkSync(to, link, dirLinkType);
    assert.throws(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e.code === "mismatch" && /node_modules\/escape/.test(e.message) && why.test(e.message));
    fs.rmSync(link, { recursive: true, force: true });
  }
  // A planted link in place of the dependency directory itself is not an installed directory.
  fs.rmSync(path.join(f.dir, "node_modules"), { recursive: true, force: true });
  fs.symlinkSync(path.join(f.repo, "src-out"), path.join(f.dir, "node_modules"), dirLinkType);
  refusesPath(f, "node_modules");
  fs.rmSync(path.join(f.dir, "node_modules"), { recursive: true, force: true });
  // The next sync prunes the install ("exact"), and verification then holds with no allowance used.
  write(f.dir, { "node_modules/x/index.js": "x\n" });
  const r = syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  assert.equal(r.removed, 2, "node_modules and apps/web/node_modules");
  assert.equal(fs.existsSync(path.join(f.dir, "node_modules")), false);
});

test("E6 exact: node_modules where the commit has no pnpm lockfile is refused (nothing would have installed it)", (t) => {
  const f = fixture(t);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  write(f.dir, { "node_modules/dep/index.js": "x\n" });
  refusesPath(f, "node_modules");
});

test("§1 gate-written .git/config: only the config-only origin file is allowed under .git", (t) => {
  const f = fixture(t);
  git(f.repo, "remote", "add", "origin", "git@github.com:example/repo.git");
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  write(f.dir, { ".git/config": '[remote "origin"]\n\turl = git@github.com:example/repo.git\n' });
  verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  for (const body of ['[core]\n\tfsmonitor = /bin/sh\n', '[remote "origin"]\n\turl = x\n[core]\n\tfsmonitor = y\n']) {
    write(f.dir, { ".git/config": body });
    refusesPath(f, ".git/config");
  }
  write(f.dir, { ".git/config": '[remote "origin"]\n\turl = x\n', ".git/HEAD": "ref: refs/heads/main\n" });
  refusesPath(f, ".git");
  const r = syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  assert.equal(r.removed, 1);
  assert.equal(fs.existsSync(path.join(f.dir, ".git")), false);
});

// Review item5-r1 finding 8: the success exit hid a kept ownership record.
test("snapshot CLI: a sync whose build ownership release keeps the record exits 1 with an explicit line; a released one exits 0", (t) => {
  const f = fixture(t);
  const env = process.env;
  const run = (release) => {
    const out = [], err = [];
    const code = snapshotCommand({ cwd: f.repo, env, snapshotRoot: f.root, platform: process.platform, out: (m) => out.push(m), err: (m) => err.push(m), acquireOwnership: () => ({ handle: "h-keep", joined: false, reclaimed: null, release }) }, ["--commit", f.commit, "--json"]);
    return { code, out, err };
  };
  const kept = run(() => ({ released: false, why: "survivor still reads snapshot" }));
  assert.equal(kept.code, 1);
  assert.match(kept.err.join("\n"), /build ownership h-keep kept: survivor still reads snapshot/);
  const released = run(() => ({ released: true }));
  assert.equal(released.code, 0, released.err.join("\n"));
  assert.deepEqual(released.err, []);
  // A joined ownership (the caller's own, held by an outer gate) is not this command's to release: still exit 0.
  const out = [], err = [];
  assert.equal(snapshotCommand({ cwd: f.repo, env, snapshotRoot: f.root, platform: process.platform, out: (m) => out.push(m), err: (m) => err.push(m), acquireOwnership: () => ({ handle: "h-outer", joined: true, reclaimed: null, release: () => ({ released: false, why: "joined" }) }) }, ["--commit", f.commit]), 0, err.join("\n"));
  // A failed sync keeps its own nonzero code even when the release also keeps the record.
  fs.mkdirSync(f.root, { recursive: true });
  fs.writeFileSync(path.join(f.root, `${repoKey(keyPath(f.repo))}.lock`), JSON.stringify({ pid: process.pid, host: os.hostname(), startedAt: "now" }));
  const failedOut = [];
  assert.equal(snapshotCommand({ cwd: f.repo, env, snapshotRoot: f.root, platform: process.platform, out: () => {}, err: (m) => failedOut.push(m), acquireOwnership: () => ({ handle: "h-keep", joined: false, reclaimed: null, release: () => ({ released: false, why: "kept" }) }) }, ["--commit", f.commit]), 1);
  assert.match(failedOut.join("\n"), /snapshot busy/);
  assert.match(failedOut.join("\n"), /kept: kept/);
});

test("E6 modes: a changed exec bit refuses; a mode-only commit fixes the mode without rewriting bytes (Windows: the mode is the tree's, the disk holds none)", (t) => {
  const f = fixture(t);
  write(f.repo, { "run.sh": "#!/bin/sh\n" });
  const c1 = commitAll(f.repo);
  syncSnapshot({ checkout: f.repo, rev: c1, root: f.root });
  const run = path.join(f.dir, "run.sh");
  if (process.platform === "win32") {
    // NTFS has no exec bit (GATE-CONTRACT §1 "types & modes preserved" can only mean the tree's): the snapshot's identity is the
    // commit's tree, whose entry mode comes from the seeded index, and a chmod changes nothing the tree can see. So a "tampered" mode
    // verifies (there is nothing on disk to tamper), and a mode-only commit changes the tree id while the sync rewrites no byte.
    fs.chmodSync(run, 0o755);
    assert.equal(verifySnapshot({ checkout: f.repo, rev: c1, root: f.root }).commit, c1);
    const before = fs.lstatSync(run, { bigint: true }).mtimeNs;
    git(f.repo, "update-index", "--chmod=+x", "run.sh");
    git(f.repo, "commit", "-q", "-m", "mode only");
    const c2 = git(f.repo, "rev-parse", "HEAD");
    assert.match(git(f.repo, "ls-tree", c2, "run.sh"), /^100755 blob /, "the commit records the exec mode");
    const r = syncSnapshot({ checkout: f.repo, rev: c2, root: f.root });
    assert.deepEqual([r.written, r.modesFixed], [0, 0]);
    assert.equal(r.tree, git(f.repo, "rev-parse", `${c2}^{tree}`));
    assert.notEqual(r.tree, git(f.repo, "rev-parse", `${c1}^{tree}`), "the mode-only commit is a different tree");
    assert.equal(fs.lstatSync(run, { bigint: true }).mtimeNs, before);
    assert.equal(verifySnapshot({ checkout: f.repo, rev: c2, root: f.root }).tree, r.tree);
    return;
  }
  fs.chmodSync(run, 0o755);
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: c1, root: f.root }), (e) => e.code === "mismatch");
  syncSnapshot({ checkout: f.repo, rev: c1, root: f.root });
  verifySnapshot({ checkout: f.repo, rev: c1, root: f.root });
  const before = fs.lstatSync(run, { bigint: true }).mtimeNs;
  fs.chmodSync(path.join(f.repo, "run.sh"), 0o755);
  const c2 = commitAll(f.repo);
  const r = syncSnapshot({ checkout: f.repo, rev: c2, root: f.root });
  assert.deepEqual([r.written, r.modesFixed], [0, 1]);
  assert.equal(fs.lstatSync(run, { bigint: true }).mtimeNs, before);
  assert.equal(fs.statSync(run).mode & 0o111, 0o111);
});

test("§1 no link into the checkout: a planted link or junction (ignored or in place of a directory) is removed and never written through", (t) => {
  const f = fixture(t);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  // A directory link: a junction on Windows (no privilege needed, the kind a Dell snapshot could meet), a symlink elsewhere.
  const dirLink = process.platform === "win32" ? "junction" : "dir";
  fs.mkdirSync(path.join(f.repo, "node_modules"), { recursive: true });
  fs.symlinkSync(path.join(f.repo, "node_modules"), path.join(f.dir, "node_modules"), dirLink);
  fs.rmSync(path.join(f.dir, "src"), { recursive: true });
  fs.symlinkSync(path.join(f.repo, "src"), path.join(f.dir, "src"), dirLink);
  write(f.repo, { "src/lib.rs": "pub fn dirty() {}\n" });
  const r = syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  assert.ok(r.linksRemoved >= 2, `links removed: ${r.linksRemoved}`);
  assert.equal(fs.lstatSync(path.join(f.dir, "src")).isDirectory(), true);
  assert.equal(fs.existsSync(path.join(f.dir, "node_modules")), false);
  assert.equal(fs.readFileSync(path.join(f.repo, "src/lib.rs"), "utf8"), "pub fn dirty() {}\n", "the checkout was not written through the link");
  assert.equal(fs.readFileSync(path.join(f.dir, "src/lib.rs"), "utf8"), "pub fn a() {}\n");
});

test("§1 committed links: a relative link inside the snapshot is kept as a link; one out of the snapshot refuses before writing", (t) => {
  if (!canLink) return assertNoLinkHost(t);
  const f = fixture(t);
  const inside = commitLink(f.repo, "src/alias.rs", "lib.rs");
  syncSnapshot({ checkout: f.repo, rev: inside, root: f.root });
  assert.equal(fs.readlinkSync(path.join(f.dir, "src", "alias.rs")), "lib.rs");
  assert.ok(fs.lstatSync(path.join(f.dir, "src", "alias.rs")).isSymbolicLink());
  verifySnapshot({ checkout: f.repo, rev: inside, root: f.root });
  const outside = commitLink(f.repo, "src/escape.rs", "../../outside", "escape");
  assert.throws(() => syncSnapshot({ checkout: f.repo, rev: outside, root: f.root }), (e) => e.code === "refused" && /escape\.rs/.test(e.message));
  assert.equal(fs.existsSync(path.join(f.dir, "src", "escape.rs")), false);
});

test("§1 gitlinks refuse before anything is created", (t) => {
  const f = fixture(t);
  git(f.repo, "update-index", "--add", "--cacheinfo", `160000,${f.commit},vendor/sub`);
  git(f.repo, "commit", "-q", "-m", "gitlink");
  const commit = git(f.repo, "rev-parse", "HEAD");
  assert.throws(() => syncSnapshot({ checkout: f.repo, rev: commit, root: f.root }), (e) => e.code === "refused" && /gitlink/.test(e.message));
  assert.equal(fs.existsSync(f.dir), false);
});

test("exclusion: a live holder makes a second sync busy; a dead holder's lock is replaced", (t) => {
  const f = fixture(t);
  fs.mkdirSync(f.root, { recursive: true });
  fs.writeFileSync(`${f.dir}.lock`, JSON.stringify({ pid: process.pid, host: os.hostname(), startedAt: "now" }));
  assert.throws(() => syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e.code === "busy");
  fs.writeFileSync(`${f.dir}.lock`, JSON.stringify({ pid: 2 ** 22 + 12345, host: os.hostname(), startedAt: "then" }));
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  assert.equal(fs.existsSync(`${f.dir}.lock`), false);
  const release = acquireSnapshotLock(`${f.dir}.lock`);
  assert.throws(() => acquireSnapshotLock(`${f.dir}.lock`), (e) => e.code === "busy");
  release();
});

test("CLI: rightkit-gate snapshot syncs and --verify checks; exit 0 match, 1 mismatch, 2 usage", async (t) => {
  const f = fixture(t);
  const root = f.root;
  const out = [], err = [];
  const io = { cwd: f.repo, env: process.env, snapshotRoot: root, out: (m) => out.push(m), err: (m) => err.push(m) };
  assert.equal(await main(["snapshot", "--commit", f.commit, "--json"], io), 0, err.join("\n"));
  const r = JSON.parse(out.at(-1));
  assert.equal(r.path, path.join(root, repoKey(keyPath(f.repo))));
  assert.equal(r.tree, git(f.repo, "rev-parse", `${f.commit}^{tree}`));
  assert.equal(await main(["snapshot", "--verify"], io), 0, err.join("\n"));
  fs.appendFileSync(path.join(r.path, "README.md"), "tampered\n");
  assert.equal(await main(["snapshot", "--verify"], io), 1);
  assert.match(err.at(-1), /snapshot mismatch/);
  assert.equal(await main(["snapshot", "--worktree"], io), 2);
  assert.equal(await main(["snapshot", "--commit"], io), 2);
});

// Oracle step-1 review counterexamples: a link chain that `path.resolve` collapses lexically but the OS follows out of the
// snapshot, and a case-colliding link that redirected a write into the checkout on APFS.
function blobOf(repo, text) {
  const r = spawnSync("git", ["hash-object", "-w", "--stdin"], { cwd: repo, input: text, encoding: "utf8" });
  if (r.status !== 0) throw new Error(r.stderr);
  return r.stdout.trim();
}
const caseInsensitiveHost = process.platform === "darwin" || process.platform === "win32" ? false : "case collisions are refused on Mac & Windows only";

test("Oracle #1: a committed link chain that escapes physically (c/b -> .., a -> c/b/…/../<checkout>/secret) refuses and is not left behind", (t) => {
  if (!canLink) return assertNoLinkHost(t);
  const f = fixture(t);
  write(f.repo, { "secret.txt": "uncommitted checkout content\n" });
  const hops = 12;
  git(f.repo, "update-index", "--add", "--cacheinfo", `120000,${blobOf(f.repo, "..")},c/b`);
  git(f.repo, "update-index", "--add", "--cacheinfo", `120000,${blobOf(f.repo, "c/b/".repeat(hops) + "../".repeat(2 * hops) + fromRoot(f.repo) + "/secret.txt")},a`);
  git(f.repo, "commit", "-q", "-m", "chain");
  const commit = git(f.repo, "rev-parse", "HEAD");
  assert.throws(() => syncSnapshot({ checkout: f.repo, rev: commit, root: f.root }), (e) => e.code === "refused" && /\ba\b/.test(e.message));
  assert.equal(fs.lstatSync(path.join(f.dir, "a"), { throwIfNoEntry: false }), undefined, "the escaping link is removed");
});

test("Oracle #1: a planted link escaping through a committed link chain is removed", (t) => {
  if (!canLink) return assertNoLinkHost(t);
  const f = fixture(t);
  git(f.repo, "update-index", "--add", "--cacheinfo", `120000,${blobOf(f.repo, "..")},c/b`);
  git(f.repo, "commit", "-q", "-m", "up-link");
  const commit = git(f.repo, "rev-parse", "HEAD");
  syncSnapshot({ checkout: f.repo, rev: commit, root: f.root });
  fs.mkdirSync(path.join(f.repo, "node_modules"), { recursive: true });
  fs.symlinkSync("c/b/".repeat(12) + "../".repeat(24) + fromRoot(f.repo) + "/node_modules", path.join(f.dir, "node_modules"), dirLinkType);
  const r = syncSnapshot({ checkout: f.repo, rev: commit, root: f.root });
  assert.equal(r.linksRemoved, 1);
  assert.equal(fs.lstatSync(path.join(f.dir, "node_modules"), { throwIfNoEntry: false }), undefined);
});

test("Oracle #2: paths differing only in case refuse before anything is written; the checkout is never written", { skip: caseInsensitiveHost }, (t) => {
  const f = fixture(t);
  write(f.repo, { "victim/x.txt": "CHECKOUT ORIGINAL\n" });
  git(f.repo, "update-index", "--add", "--cacheinfo", `120000,${blobOf(f.repo, "c/b/".repeat(12) + "../".repeat(24) + fromRoot(f.repo) + "/victim")},Foo`);
  git(f.repo, "update-index", "--add", "--cacheinfo", `100644,${blobOf(f.repo, "WRITTEN BY SNAPSHOT SYNC\n")},foo/x.txt`);
  git(f.repo, "commit", "-q", "-m", "case collision");
  const commit = git(f.repo, "rev-parse", "HEAD");
  assert.throws(() => syncSnapshot({ checkout: f.repo, rev: commit, root: f.root }), (e) => e.code === "refused" && /differ only in case/.test(e.message));
  assert.equal(fs.existsSync(f.dir), false);
  assert.equal(fs.readFileSync(path.join(f.repo, "victim/x.txt"), "utf8"), "CHECKOUT ORIGINAL\n");
});

test("a dangling committed link refuses and is removed", (t) => {
  if (!canLink) return assertNoLinkHost(t);
  const f = fixture(t);
  const commit = commitLink(f.repo, "src/dangling.rs", "missing.rs");
  assert.throws(() => syncSnapshot({ checkout: f.repo, rev: commit, root: f.root }), (e) => e.code === "refused" && /dangling\.rs/.test(e.message));
  assert.equal(fs.lstatSync(path.join(f.dir, "src", "dangling.rs"), { throwIfNoEntry: false }), undefined);
});

test("Oracle #3: pruning a stale path never deletes through a planted parent link or junction into the checkout", (t) => {
  const f = fixture(t);
  const a = f.commit;
  write(f.repo, { "src/new.rs": "pub fn new() {}\n" });
  const b = commitAll(f.repo);
  syncSnapshot({ checkout: f.repo, rev: b, root: f.root });
  write(f.repo, { "src/new.rs": "pub fn new() { /* uncommitted work */ }\n" });
  fs.rmSync(path.join(f.dir, "src"), { recursive: true });
  fs.symlinkSync(path.join(f.repo, "src"), path.join(f.dir, "src"), process.platform === "win32" ? "junction" : "dir");
  const r = syncSnapshot({ checkout: f.repo, rev: a, root: f.root });
  assert.equal(r.linksRemoved, 1);
  assert.equal(fs.readFileSync(path.join(f.repo, "src/new.rs"), "utf8"), "pub fn new() { /* uncommitted work */ }\n", "the checkout's file survives");
  assert.equal(fs.lstatSync(path.join(f.dir, "src")).isDirectory(), true);
  assert.equal(fs.existsSync(path.join(f.dir, "src/new.rs")), false);
  verifySnapshot({ checkout: f.repo, rev: a, root: f.root });
});

test("§1 exact: untracked files are removed whether an ignore rule matches them or not, with leftover directories", (t) => {
  const f = fixture(t);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  write(f.dir, { "target/debug/stale.o": "old output\n", "notes.log": "ignored\n", "src/extra.rs": "visible\n", ".env": "SECRET=planted\n" });
  const r = syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  assert.equal(r.removed, 4);
  for (const p of ["target", "notes.log", "src/extra.rs", ".env"]) assert.equal(fs.existsSync(path.join(f.dir, p)), false, p);
  assert.deepEqual(fs.readdirSync(f.dir).sort(), [".gitignore", "Cargo.toml", "README.md", "src"]);
});

test("§1 exact: files matched by .git/info/exclude or the user's global excludes file are removed too", (t) => {
  const f = fixture(t);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  fs.mkdirSync(path.join(f.repo, ".git", "info"), { recursive: true });
  fs.appendFileSync(path.join(f.repo, ".git", "info", "exclude"), "local-only.rs\n");
  const globalExcludes = path.join(f.root, "..", `global-excludes-${path.basename(f.root)}`);
  fs.writeFileSync(globalExcludes, "*.cache\n");
  t.after(() => fs.rmSync(globalExcludes, { force: true }));
  git(f.repo, "config", "core.excludesFile", globalExcludes);
  write(f.dir, { "local-only.rs": "stale\n", "src/build.cache": "stale\n" });
  // The seeded identity alone cannot see them; the verifier walks the directory and refuses (E6), and the sync removes them.
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e.code === "mismatch" && /local-only\.rs/.test(e.message));
  const r = syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  assert.equal(r.removed, 2);
  assert.equal(fs.existsSync(path.join(f.dir, "local-only.rs")), false);
  assert.equal(fs.existsSync(path.join(f.dir, "src/build.cache")), false);
});

test("coordinator Oracle shape: an obsolete directory replaced by a planted link to checkout/src never deletes checkout/src/x", (t) => {
  const f = fixture(t);
  const a = f.commit;
  write(f.repo, { "obsolete/x": "committed in B\n" });
  const b = commitAll(f.repo);
  syncSnapshot({ checkout: f.repo, rev: b, root: f.root });
  write(f.repo, { "src/x": "uncommitted checkout file\n" });
  fs.rmSync(path.join(f.dir, "obsolete"), { recursive: true });
  fs.symlinkSync(path.join(f.repo, "src"), path.join(f.dir, "obsolete"), process.platform === "win32" ? "junction" : "dir");
  const statusBefore = git(f.repo, "status", "--porcelain", "--untracked-files=all");
  syncSnapshot({ checkout: f.repo, rev: a, root: f.root });
  assert.equal(fs.readFileSync(path.join(f.repo, "src/x"), "utf8"), "uncommitted checkout file\n");
  assert.equal(git(f.repo, "status", "--porcelain", "--untracked-files=all"), statusBefore);
  assert.equal(fs.lstatSync(path.join(f.dir, "obsolete"), { throwIfNoEntry: false }), undefined);
  verifySnapshot({ checkout: f.repo, rev: a, root: f.root });
});

test("exclusion race: contenders arriving while a dead holder's lock is reclaimed never both hold it", (t) => {
  const f = fixture(t);
  fs.mkdirSync(f.root, { recursive: true });
  const lock = `${f.dir}.lock`;
  const dead = JSON.stringify({ pid: 2 ** 22 + 4321, host: os.hostname(), startedAt: "then", token: "dead" });
  fs.writeFileSync(lock, dead);
  const outcomes = {};
  const attempt = (name, hooks) => { try { outcomes[name] = acquireSnapshotLock(lock, hooks); } catch (e) { outcomes[name] = e.code; } };
  // B judges the lock stale; A then judges it stale too, before B starts reclaiming (the case that let two enter before).
  // While B reclaims, C arrives (the put-back gap of the move-aside design).
  attempt("B", { afterStaleRead: () => attempt("A"), whileReclaiming: () => attempt("C") });
  const holders = Object.entries(outcomes).filter(([, v]) => typeof v === "function" && v.held());
  assert.equal(holders.length, 1, `exactly one holder: ${JSON.stringify(Object.fromEntries(Object.entries(outcomes).map(([k, v]) => [k, typeof v === "function" ? (v.held() ? "held" : "lost") : v])))}`);
  for (const [, v] of Object.entries(outcomes)) if (typeof v !== "function") assert.equal(v, "busy");
  assert.deepEqual(fs.readdirSync(f.root).filter((n) => n.endsWith(".reclaim")), [], "the reclaim file is removed");
  holders[0][1]();
  assert.equal(fs.existsSync(lock), false);
  // A reclaim file left by a reclaimer that died fails closed.
  fs.writeFileSync(lock, dead);
  fs.writeFileSync(`${lock}.reclaim`, "x");
  assert.throws(() => acquireSnapshotLock(lock), (e) => e.code === "busy" && /being reclaimed/.test(e.message));
  fs.rmSync(`${lock}.reclaim`);
  // An empty or unparseable lock is never reclaimed, however old (Oracle: an empty stale lock equals a lock being created).
  const lockOf = (pid) => JSON.stringify({ pid, host: os.hostname(), startedAt: "then", token: "t" });
  for (const junk of ["", "{not json", JSON.stringify({ pid: "x" }), lockOf(1.5), lockOf(0), lockOf(-7), lockOf(2 ** 31), lockOf(Number.MAX_SAFE_INTEGER), JSON.stringify({ pid: 2 ** 22 + 1, host: 7 })]) {
    fs.writeFileSync(lock, junk);
    fs.utimesSync(lock, new Date(0), new Date(0));
    assert.throws(() => acquireSnapshotLock(lock), (e) => e.code === "busy" && /cannot be read as a lock/.test(e.message));
    assert.equal(fs.readFileSync(lock, "utf8"), junk, "left in place");
  }
  fs.rmSync(lock);
  // A lock appears whole: no window in which it exists empty, and no private publish files are left behind.
  const release = acquireSnapshotLock(lock);
  assert.match(fs.readFileSync(lock, "utf8"), /"token":/);
  assert.deepEqual(fs.readdirSync(f.root).filter((n) => n.endsWith(".tmp")), []);
  release();
});

test("stale means proven gone: a holder pid that answers EPERM (pid 1) is busy, never reclaimed", { skip: posixOnly || (process.getuid?.() === 0 ? "root can signal pid 1" : false) }, (t) => {
  const f = fixture(t);
  fs.mkdirSync(f.root, { recursive: true });
  const lock = `${f.dir}.lock`;
  const body = JSON.stringify({ pid: 1, host: os.hostname(), startedAt: "then", token: "init" });
  fs.writeFileSync(lock, body);
  assert.throws(() => acquireSnapshotLock(lock), (e) => e.code === "busy" && /held by pid 1|cannot be proven gone \(EPERM\)/.test(e.message));
  assert.equal(fs.readFileSync(lock, "utf8"), body, "left in place");
});

// ============================================================================================================================
// Item 5, lane L5S (docs/GATE-CONTRACT.md §1 "Commit-only build" and "Broker interface" Snapshot root; signed map rows 5.05, 5.07,
// 5.08, 5.09, 5.12, 5.13, 5.15, 5.16 (root rule above), 5.18 and the root-mode half of 5.22). Each title names its row.
// ============================================================================================================================

/** Commits `name` as a plain file, a directory holding `a.rs`, or a link (mode 120000) to `body`, replacing whatever the path was. */
function setEntry(repo, name, kind, body) {
  git(repo, "rm", "-r", "-q", "--cached", "--ignore-unmatch", name);
  fs.rmSync(path.join(repo, name), { recursive: true, force: true });
  if (kind === "file") { write(repo, { [name]: body }); git(repo, "add", name); }
  else if (kind === "dir") { write(repo, { [`${name}/a.rs`]: body }); git(repo, "add", name); }
  else git(repo, "update-index", "--add", "--cacheinfo", `120000,${blobOf(repo, body)},${name}`);
  git(repo, "commit", "-q", "-m", `${name} is a ${kind}`);
  return git(repo, "rev-parse", "HEAD");
}
const kindOf = (p) => { const s = fs.lstatSync(p, { throwIfNoEntry: false }); return !s ? "none" : s.isSymbolicLink() ? "link" : s.isDirectory() ? "dir" : "file"; };

test("5.05 type flips: file, directory, link and file again across commits converge, forward and back, and verify at every step", (t) => {
  const f = fixture(t);
  const steps = [["file", "one\n"], ["dir", "two\n"], ...(canLink ? [["link", "src/lib.rs"], ["link", "src"]] : []), ["file", "three\n"], ["dir", "four\n"]];
  const commits = [{ rev: f.commit, kind: "none" }];
  for (const [kind, body] of steps) commits.push({ rev: setEntry(f.repo, "x", kind, body), kind });
  // Forward, then back through every commit, then forward again: every transition between two kinds is crossed in both directions.
  const order = [...commits, ...[...commits].reverse(), ...commits];
  for (const c of order) {
    const r = syncSnapshot({ checkout: f.repo, rev: c.rev, root: f.root });
    assert.equal(r.commit, c.rev);
    assert.equal(kindOf(path.join(f.dir, "x")), c.kind, `x is a ${c.kind} at ${c.rev.slice(0, 8)}`);
    assert.equal(verifySnapshot({ checkout: f.repo, rev: c.rev, root: f.root }).commit, c.rev);
    assert.equal(fs.existsSync(path.join(f.dir, "x", "a.rs")), c.kind === "dir", "no leftover of the previous type under or beside x");
  }
});

test("5.05 type flips planted in the snapshot refuse at verify, at the start and again at the end, and the next sync converges", (t) => {
  const f = fixture(t);
  const asFile = setEntry(f.repo, "x", "file", "committed\n");
  const asDir = setEntry(f.repo, "x", "dir", "committed\n");
  const asLink = canLink ? setEntry(f.repo, "x", "link", "src/lib.rs") : null;
  const x = path.join(f.dir, "x");
  const plant = {
    "a file where the commit has a directory": () => { fs.rmSync(x, { recursive: true }); fs.writeFileSync(x, "planted\n"); },
    "a link where the commit has a directory": () => { fs.rmSync(x, { recursive: true }); fs.symlinkSync(path.join(f.dir, "src"), x, dirLinkType); },
    "a directory where the commit has a file": () => { fs.rmSync(x); fs.mkdirSync(x); fs.writeFileSync(path.join(x, "a.rs"), "planted\n"); },
    "a link where the commit has a file": () => { fs.rmSync(x); fs.symlinkSync("src/lib.rs", x); },
    "a plain file holding the text of the committed link": () => { fs.rmSync(x); fs.writeFileSync(x, "src/lib.rs"); },
    "a directory where the commit has a link": () => { fs.rmSync(x); fs.mkdirSync(x); },
  };
  const cases = [["a file where the commit has a directory", asDir], ["a link where the commit has a directory", asDir], ["a directory where the commit has a file", asFile], ...(canLink ? [["a link where the commit has a file", asFile], ["a plain file holding the text of the committed link", asLink], ["a directory where the commit has a link", asLink]] : [])];
  for (const [name, rev] of cases) {
    if (name.includes("link") && !canLink) continue;
    syncSnapshot({ checkout: f.repo, rev, root: f.root });
    verifySnapshot({ checkout: f.repo, rev, root: f.root });            // the start verify
    plant[name]();                                                       // planted between the start and the end verify
    assert.throws(() => verifySnapshot({ checkout: f.repo, rev, root: f.root }), (e) => e instanceof SnapshotError && e.code === "mismatch" && /\bx\b/.test(e.message), name);
    assert.equal(syncSnapshot({ checkout: f.repo, rev, root: f.root }).commit, rev, `${name}: the next sync converges`);
    verifySnapshot({ checkout: f.repo, rev, root: f.root });
  }
  if (!canLink) assertNoLinkHost(t);
});

test("5.05 a commit with a gitlink is refused with a named reason before any write, and the snapshot of the previous commit is untouched", (t) => {
  const f = fixture(t);
  assert.deepEqual(fs.readdirSync(f.root), []);
  git(f.repo, "update-index", "--add", "--cacheinfo", `160000,${f.commit},vendor/sub`);
  git(f.repo, "commit", "-q", "-m", "gitlink");
  const withGitlink = git(f.repo, "rev-parse", "HEAD");
  // Nothing exists yet: the refusal creates nothing at all, not even the root's lock, state or birth record.
  assert.throws(() => syncSnapshot({ checkout: f.repo, rev: withGitlink, root: f.root }), (e) => e instanceof SnapshotError && e.code === "refused" && e.reason === "gitlink" && /vendor\/sub/.test(e.message));
  assert.deepEqual(fs.readdirSync(f.root), []);
  // With a snapshot of the previous commit in place: its bytes, mtimes and the root's listing do not change.
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  const before = { names: fs.readdirSync(f.root).sort(), mtimes: mtimes(f.dir), state: fs.readFileSync(`${f.dir}.state.json`, "utf8") };
  const err = [];
  assert.equal(snapshotCommand({ cwd: f.repo, env: process.env, platform: process.platform, snapshotRoot: f.root, out: () => {}, err: (m) => err.push(m), acquireOwnership: () => ({ handle: "h", joined: false, reclaimed: null, release: () => ({ released: true }) }) }, ["--commit", withGitlink]), 1);
  assert.match(err.join("\n"), /snapshot refused \(gitlink\)/);
  assert.deepEqual({ names: fs.readdirSync(f.root).sort(), mtimes: mtimes(f.dir), state: fs.readFileSync(`${f.dir}.state.json`, "utf8") }, before);
  verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
});

// Row 5.07: a hard link from the snapshot to a checkout file carries a later checkout edit into the build with the bytes the tree id saw.
test("5.07 hard links: a sync over a hard-linked file ends with nlink 1 and verifies; the checkout file is neither written nor chmod-ed through it", (t) => {
  const f = fixture(t, { "Cargo.toml": "[workspace]\n", "src/lib.rs": "pub fn a() {}\n", "tool.sh": "#!/bin/sh\n", ".gitignore": "target/\n" });
  if (process.platform !== "win32") fs.chmodSync(path.join(f.repo, "tool.sh"), 0o755);
  const c = commitAll(f.repo, "tool");
  syncSnapshot({ checkout: f.repo, rev: c, root: f.root });
  for (const name of ["src/lib.rs", "tool.sh"]) {
    const inSnap = path.join(f.dir, ...name.split("/"));
    const inRepo = path.join(f.repo, ...name.split("/"));
    fs.rmSync(inSnap);
    fs.linkSync(inRepo, inSnap);                                         // equal bytes: the tree id cannot see it
    assert.ok(fs.lstatSync(inSnap).nlink > 1, "the fixture planted a hard link");
  }
  if (process.platform !== "win32") fs.chmodSync(path.join(f.repo, "tool.sh"), 0o644); // through the link: the snapshot's name now reads 0644 too
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: c, root: f.root }), (e) => e.code === "mismatch" && /hard link/.test(e.message));
  const r = syncSnapshot({ checkout: f.repo, rev: c, root: f.root });
  assert.equal(r.hardLinksReplaced, 2);
  for (const name of ["src/lib.rs", "tool.sh"]) {
    assert.equal(fs.lstatSync(path.join(f.dir, ...name.split("/"))).nlink, 1, `${name} is a fresh copy`);
    assert.equal(fs.lstatSync(path.join(f.repo, ...name.split("/"))).nlink, 1, `${name} in the checkout lost its other name`);
  }
  if (process.platform !== "win32") {
    assert.equal(fs.statSync(path.join(f.repo, "tool.sh")).mode & 0o777, 0o644, "the checkout file was not chmod-ed through the link");
    assert.equal(fs.statSync(path.join(f.dir, "tool.sh")).mode & 0o111, 0o111, "the snapshot's copy has the committed exec bit");
  }
  verifySnapshot({ checkout: f.repo, rev: c, root: f.root });
  // A later checkout edit does not reach the snapshot.
  write(f.repo, { "src/lib.rs": "pub fn edited_after() {}\n" });
  assert.equal(fs.readFileSync(path.join(f.dir, "src", "lib.rs"), "utf8"), "pub fn a() {}\n");
  assert.equal(syncSnapshot({ checkout: f.repo, rev: c, root: f.root }).hardLinksReplaced, 0);
});

test("5.07 hard links: a hard link planted between the start verify and the end verify, and an extra hard link, refuse; pnpm's own links inside node_modules are not refused", (t) => {
  const f = dependencyFixture(t);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root });                    // the start verify
  const lib = path.join(f.dir, "src", "lib.rs");
  fs.rmSync(lib);
  fs.linkSync(path.join(f.repo, "src", "lib.rs"), lib);                                  // between start and end
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e.code === "mismatch" && /src\/lib\.rs has \d+ hard links/.test(e.message));
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  write(f.repo, { "outside.rs": "x\n" });
  fs.linkSync(path.join(f.repo, "outside.rs"), path.join(f.dir, "extra.rs"));            // an extra, hard-linked
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e.code === "mismatch" && /extra\.rs is not in the commit's tree/.test(e.message));
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  // pnpm hard-links package files out of its store into node_modules; the dependency bytes are bound by the environment digest instead.
  write(f.dir, { "node_modules/.pnpm/dep@1.0.0/node_modules/dep/index.js": "module.exports = 1;\n" });
  fs.linkSync(path.join(f.dir, "node_modules", ".pnpm", "dep@1.0.0", "node_modules", "dep", "index.js"), path.join(f.dir, "node_modules", "store-twin.js"));
  assert.equal(verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }).commit, f.commit);
});

// Row 5.08: the CLI-level negatives of `rightkit-gate snapshot --verify` (the snapshot-check E17 cases are item 6).
test("5.08 snapshot --verify exits 0 on a fresh sync and 1 for each tamper: a byte, an ignored file, an untracked file, an empty directory, an exec bit, a link retarget, a link into a ..x directory, a planted target/", async (t) => {
  const f = fixture(t, { "Cargo.toml": "[workspace]\n", "src/lib.rs": "pub fn a() {}\n", "run.sh": "#!/bin/sh\n", "..x/a.rs": "pub fn x() {}\n", ".gitignore": "target/\n*.log\n" });
  if (process.platform !== "win32") fs.chmodSync(path.join(f.repo, "run.sh"), 0o755);
  commitAll(f.repo, "run.sh");
  const c = canLink ? commitLink(f.repo, "src/alias.rs", "lib.rs") : git(f.repo, "rev-parse", "HEAD");
  const out = [], err = [];
  // Build ownership is not this test's subject (rows 5.19 to 5.21 are): a stub keeps a host-wide hold out of the verdict.
  const io = { cwd: f.repo, env: process.env, snapshotRoot: f.root, out: (m) => out.push(m), err: (m) => err.push(m), acquireOwnership: () => ({ handle: "h", joined: false, reclaimed: null, release: () => ({ released: true }) }) };
  const cli = (...a) => { out.length = 0; err.length = 0; return main(["snapshot", ...a], io); };
  assert.equal(await cli("--commit", c), 0, err.join("\n"));
  assert.equal(await cli("--verify", "--commit", c), 0, err.join("\n"));
  const cases = [
    ["a changed byte", () => fs.appendFileSync(path.join(f.dir, "src", "lib.rs"), "// x\n"), "not tree"],
    ["an added ignored file", () => write(f.dir, { "planted.log": "x\n" }), "planted.log"],
    ["an added untracked file", () => write(f.dir, { "src/planted.rs": "x\n" }), "src/planted.rs"],
    ["an empty directory", () => fs.mkdirSync(path.join(f.dir, "src", "empty")), "src/empty"],
    ["a planted target/", () => write(f.dir, { "target/debug/x.o": "x\n" }), "target"],
    ...(process.platform === "win32" ? [] : [["an exec-bit change", () => fs.chmodSync(path.join(f.dir, "run.sh"), 0o644), "run.sh"]]),
    ...(canLink ? [
      ["a link retarget to a file inside the snapshot", () => { fs.rmSync(path.join(f.dir, "src", "alias.rs")); fs.symlinkSync("../Cargo.toml", path.join(f.dir, "src", "alias.rs")); }, "src/alias.rs"],
      ["a link retarget into the checkout", () => { fs.rmSync(path.join(f.dir, "src", "alias.rs")); fs.symlinkSync(path.join(f.repo, "src", "lib.rs"), path.join(f.dir, "src", "alias.rs")); }, "src/alias.rs"],
      ["a link into a ..x sibling directory inside the snapshot", () => fs.symlinkSync(path.join(f.dir, "..x", "a.rs"), path.join(f.dir, "planted.rs")), "planted.rs"],
    ] : []),
  ];
  for (const [name, tamper, where] of cases) {
    tamper();
    assert.equal(await cli("--verify", "--commit", c), 1, `${name}: ${err.join("\n")}`);
    assert.match(err.join("\n"), /snapshot mismatch/, name);
    assert.ok(err.join("\n").includes(where), `${name} names ${where}: ${err.join("\n")}`);
    assert.equal(await cli("--commit", c), 0, `${name}: the next sync repairs it: ${err.join("\n")}`);
    assert.equal(await cli("--verify", "--commit", c), 0, `${name}: verified again: ${err.join("\n")}`);
  }
  if (!canLink) assertNoLinkHost(t);
});

// Row 5.09: the only extras are the gate's own, and they are bounded.
test("5.09 node_modules is allowed only under a root with a committed pnpm lockfile; a non-pnpm directory under a lockfile root refuses", (t) => {
  const f = fixture(t, { "package.json": "{\"name\":\"root\",\"private\":true}\n", "apps/web/package.json": "{\"name\":\"web\"}\n", "apps/web/pnpm-lock.yaml": "lockfileVersion: '9.0'\n", "apps/web/src/a.js": "x\n", "tools/package.json": "{\"name\":\"tools\"}\n", ".gitignore": "node_modules/\n" });
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  write(f.dir, { "apps/web/node_modules/.pnpm/d@1/node_modules/d/i.js": "x\n" });
  assert.equal(verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }).commit, f.commit, "under the lockfile root");
  for (const offender of ["node_modules", "tools/node_modules", "apps/web/src/node_modules", "apps/web/vendor", "apps/web/.pnpm-store", "apps/web/dist"]) {
    write(f.dir, { [`${offender}/x.js`]: "x\n" });
    refusesPath(f, offender);
    fs.rmSync(path.join(f.dir, ...offender.split("/")), { recursive: true, force: true });
  }
  verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
});

test("5.09 .git/config is allowed only byte-equal to what the gate writes for this checkout's origin: another URL (ext::), another section, another file or no origin refuse", (t) => {
  const f = fixture(t);
  const origin = "git@github.com:example/repo.git";
  git(f.repo, "remote", "add", "origin", origin);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  const body = `[remote "origin"]\n\turl = ${origin}\n`;
  write(f.dir, { ".git/config": body });
  assert.equal(verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }).commit, f.commit, "the gate's own file");
  for (const [name, bad] of [["another URL", '[remote "origin"]\n\turl = git@evil.example:x/y.git\n'], ["ext::", '[remote "origin"]\n\turl = ext::sh -c id\n'], ["a second section", `${body}[core]\n\tfsmonitor = /bin/sh\n`], ["a trailing space", body.replace(/\n$/, " \n")], ["CRLF", body.replace(/\n/g, "\r\n")], ["no newline", body.replace(/\n$/, "")], ["empty", ""]]) {
    write(f.dir, { ".git/config": bad });
    refusesPath(f, ".git/config");
    assert.ok(name);
  }
  write(f.dir, { ".git/config": body, ".git/hooks/pre-commit": "#!/bin/sh\n" });
  refusesPath(f, ".git");
  fs.rmSync(path.join(f.dir, ".git", "hooks"), { recursive: true });
  write(f.dir, { ".git/HEAD": "ref: refs/heads/main\n" });
  refusesPath(f, ".git");
  fs.rmSync(path.join(f.dir, ".git", "HEAD"));
  fs.rmSync(path.join(f.dir, ".git", "config"));
  fs.mkdirSync(path.join(f.dir, ".git", "config"));                                      // a directory named config
  refusesPath(f, ".git/config");
  fs.rmSync(path.join(f.dir, ".git"), { recursive: true });
  // A checkout with no origin gets no .git at all (the gate writes none).
  git(f.repo, "remote", "remove", "origin");
  write(f.dir, { ".git/config": body });
  refusesPath(f, ".git");
});

test("5.09 the installed dependency environment is bound by the recorded digest: a changed byte, a planted file, a deleted file and a retargeted link refuse at the end verify", (t) => {
  const f = dependencyFixture(t);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  write(f.dir, { "node_modules/.pnpm/dep@1.0.0/node_modules/dep/index.js": "module.exports = 1;\n", "node_modules/.modules.yaml": "layoutVersion: 5\n" });
  if (canLink) fs.symlinkSync(path.join(f.dir, "node_modules", ".pnpm", "dep@1.0.0", "node_modules", "dep"), path.join(f.dir, "node_modules", "dep"), dirLinkType);
  const files = commitFiles(f.repo, f.commit).files;
  // The binding is the gate's installed-dependency digest (classAEnvironmentDigest hashes it with the gate and tool identity).
  const compute = ({ dir }) => crypto.createHash("sha256").update(installedDependencyDigest({ root: dir, files })).digest("hex");
  const environment = { expected: compute({ dir: f.dir }), compute };
  const verify = () => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root, environment });
  assert.equal(verify().env, environment.expected, "an unchanged install verifies");
  const dep = path.join(f.dir, "node_modules", ".pnpm", "dep@1.0.0", "node_modules", "dep", "index.js");
  const tamper = [
    ["a dependency byte changed after install", () => fs.appendFileSync(dep, "// tampered\n"), () => fs.writeFileSync(dep, "module.exports = 1;\n")],
    ["a planted node_modules/evil/index.js", () => write(f.dir, { "node_modules/evil/index.js": "evil\n" }), () => fs.rmSync(path.join(f.dir, "node_modules", "evil"), { recursive: true })],
    ["a deleted dependency file", () => fs.rmSync(dep), () => fs.writeFileSync(dep, "module.exports = 1;\n")],
    ["a changed install state", () => fs.appendFileSync(path.join(f.dir, "node_modules", ".modules.yaml"), "x: 1\n"), () => fs.writeFileSync(path.join(f.dir, "node_modules", ".modules.yaml"), "layoutVersion: 5\n")],
  ];
  for (const [name, plant, restore] of tamper) {
    plant();
    assert.throws(verify, (e) => e instanceof SnapshotError && e.code === "mismatch" && /installed environment .* changed/.test(e.message), name);
    restore();
    assert.equal(verify().commit, f.commit, `${name}: restored`);
  }
  // The digest is required to be a digest, and a computation that fails refuses (never passes).
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root, environment: { expected: "x", compute } }), (e) => e.code === "error");
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root, environment: { expected: environment.expected, compute: () => { throw new Error("cannot hash"); } } }), (e) => e.code === "error" && /cannot hash/.test(e.message));
});

test("5.09 snapshot --verify --env <digest> through the gate: the real environment digest of the install, exit 0; a changed dependency byte, exit 1; usage errors, exit 2", async (t) => {
  const f = dependencyFixture(t);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  write(f.dir, { "node_modules/.pnpm/dep@1.0.0/node_modules/dep/index.js": "module.exports = 1;\n" });
  // pnpm's version is faked: the digest's identity inputs are the node version, the gate's own files and the installed bytes.
  const spawnPnpm = (cmd, args, opts = {}) => (cmd === "pnpm" ? { status: 0, stdout: "11.25.0\n", stderr: "" } : spawnSync(cmd, args, { encoding: "utf8", ...opts }));
  const out = [], err = [];
  const io = { cwd: f.repo, env: process.env, snapshotRoot: f.root, spawn: spawnPnpm, out: (m) => out.push(m), err: (m) => err.push(m) };
  const digest = classAEnvironmentDigest({ root: f.dir, files: commitFiles(f.repo, f.commit).files, spawn: spawnPnpm });
  assert.equal(await main(["snapshot", "--verify", "--commit", f.commit, "--env", digest], io), 0, err.join("\n"));
  fs.appendFileSync(path.join(f.dir, "node_modules", ".pnpm", "dep@1.0.0", "node_modules", "dep", "index.js"), "// tampered\n");
  err.length = 0;
  assert.equal(await main(["snapshot", "--verify", "--commit", f.commit, "--env", digest], io), 1);
  assert.match(err.join("\n"), /snapshot mismatch: .*installed environment .* changed/);
  assert.equal(await main(["snapshot", "--verify", "--env", "nothex"], io), 2);
  assert.equal(await main(["snapshot", "--commit", f.commit, "--env", digest], io), 2, "--env only checks");
});

// Row 5.12: a failed, unknown-commit or interrupted sync never yields a snapshot reported as a commit it is not.
test("5.12 an unknown revision refuses with exit 1 before any write: the snapshot's bytes, mtimes and the root's listing are unchanged, for sync and for verify", async (t) => {
  const f = fixture(t);
  const out = [], err = [];
  const io = { cwd: f.repo, env: process.env, snapshotRoot: f.root, out: (m) => out.push(m), err: (m) => err.push(m), acquireOwnership: () => ({ handle: "h", joined: false, reclaimed: null, release: () => ({ released: true }) }) };
  const cli = (...a) => { out.length = 0; err.length = 0; return main(["snapshot", ...a], io); };
  const bad = ["0123456789abcdef0123456789abcdef01234567", "no-such-branch", `${f.commit}~50`, git(f.repo, "rev-parse", `${f.commit}^{tree}`), "-x"];
  // No snapshot yet: nothing is created at all.
  for (const rev of bad) assert.equal(await cli("--commit", rev), 1, `${rev}: ${err.join("\n")}`);
  assert.deepEqual(fs.readdirSync(f.root), [], "an unknown revision created nothing in the root");
  assert.equal(await cli("--commit", bad[0]), 1);
  assert.match(err.join("\n"), /snapshot refused \(unknown-revision\)/);
  // A snapshot in place: untouched by each refusal.
  assert.equal(await cli("--commit", f.commit), 0, err.join("\n"));
  const snapshotOf = () => ({ names: fs.readdirSync(f.root).sort(), mtimes: mtimes(f.dir), bytes: fs.readFileSync(path.join(f.dir, "src", "lib.rs"), "utf8"), state: fs.readFileSync(`${f.dir}.state.json`, "utf8") });
  const before = snapshotOf();
  for (const rev of bad.slice(0, 4)) {
    assert.equal(await cli("--commit", rev), 1, `${rev} (sync)`);
    assert.equal(await cli("--verify", "--commit", rev), 1, `${rev} (verify)`);
    assert.match(err.join("\n"), /unknown-revision/);
  }
  assert.deepEqual(snapshotOf(), before);
  assert.equal(await cli("--verify", "--commit", f.commit), 0, err.join("\n"));
});

/** A gate process of its own that syncs `rev` and kills itself (SIGKILL; TerminateProcess on Windows) after its `killAfter`-th rename into the snapshot. */
async function killedSync(t, { repo, rev, root, dir, killAfter, ownershipFile: file = null }) {
  const work = tmp(t, "rk-snap-kill-");
  const ready = path.join(work, "ready");
  const driver = path.join(work, "driver.mjs");
  fs.writeFileSync(driver, `
import fs from "node:fs";
import path from "node:path";
const { spawn } = await import("node:child" + "_process");
const a = JSON.parse(process.env.RK_L5S_ARGS);
const { syncSnapshot } = await import(a.snapshot);
if (a.file) {
  const { acquireBuildOwnership } = await import(a.ownership);
  acquireBuildOwnership({ file: a.file, stage: "snapshot", snapshot: a.dir, env: process.env });
  const child = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { stdio: "ignore" });
  child.unref();
  fs.writeFileSync(a.ready + ".child", String(child.pid));
}
let renames = 0;
const rename = fs.renameSync;
fs.renameSync = (from, to) => { const r = rename(from, to); if (String(to).startsWith(a.dir + path.sep) && ++renames === a.killAfter) { fs.writeFileSync(a.ready, String(process.pid)); process.kill(process.pid, "SIGKILL"); } return r; };
syncSnapshot({ checkout: a.checkout, rev: a.rev, root: a.root });
fs.writeFileSync(a.ready + ".completed", "the sync finished before it could be killed");
`);
  const env = { ...process.env, NODE_OPTIONS: "", RK_L5S_ARGS: JSON.stringify({ snapshot: pathToFileURL(path.resolve("snapshot.mjs")).href, ownership: pathToFileURL(path.resolve("build-ownership.mjs")).href, file, checkout: repo, rev, root, dir, killAfter, ready }) };
  delete env.RIGHTKIT_BUILD_OWNERSHIP; delete env.RIGHTKIT_BUILD_OWNERSHIP_FILE;
  const child = spawn(process.execPath, [driver], { env, stdio: ["ignore", "ignore", "inherit"], detached: process.platform !== "win32" });
  await new Promise((res) => child.on("exit", res));
  assert.equal(fs.existsSync(`${ready}.completed`), false, "the sync was killed in its write phase, not after it");
  assert.equal(fs.existsSync(ready), true, "the kill point was reached");
  const sleeper = fs.existsSync(`${ready}.child`) ? Number(fs.readFileSync(`${ready}.child`, "utf8")) : null;
  if (sleeper) t.after(() => { try { process.kill(sleeper, "SIGKILL"); } catch { /* gone */ } });
  return { gate: Number(fs.readFileSync(ready, "utf8")), sleeper };
}
const alive = (pid) => { try { process.kill(pid, 0); return true; } catch { return false; } };
async function untilGone(pid, ms = 20_000) { const end = Date.now() + ms; while (alive(pid)) { if (Date.now() > end) throw new Error(`pid ${pid} survived`); await new Promise((r) => setTimeout(r, 50)); } }

test("5.12 a sync killed in its write phase leaves a snapshot that --verify refuses for the old commit and for the new one; the next sync converges and verifies", async (t) => {
  const f = fixture(t, { "Cargo.toml": "[workspace]\n", "a.rs": "a1\n", "b.rs": "b1\n", "c.rs": "c1\n", "d.rs": "d1\n", ".gitignore": "target/\n" });
  const first = f.commit;
  write(f.repo, { "a.rs": "a2\n", "b.rs": "b2\n", "c.rs": "c2\n", "d.rs": "d2\n" });
  const second = commitAll(f.repo, "all four change");
  syncSnapshot({ checkout: f.repo, rev: first, root: f.root });
  // A staging directory a dead sync left behind is not accumulated: the next sync removes it.
  fs.mkdirSync(path.join(`${f.dir}.staging`, "dead-sync"), { recursive: true });
  fs.writeFileSync(path.join(`${f.dir}.staging`, "dead-sync", "0"), "half");
  await killedSync(t, { repo: f.repo, rev: second, root: f.root, dir: f.dir, killAfter: 2 });
  const out = [], err = [];
  const cli = (...a) => { out.length = 0; err.length = 0; return main(["snapshot", ...a], { cwd: f.repo, env: process.env, snapshotRoot: f.root, out: (m) => out.push(m), err: (m) => err.push(m), acquireOwnership: () => ({ handle: "h", joined: false, reclaimed: null, release: () => ({ released: true }) }) }); };
  assert.equal(await cli("--verify", "--commit", first), 1, `old commit: ${err.join("\n")}`);
  assert.match(err.join("\n"), /snapshot mismatch/);
  assert.equal(await cli("--verify", "--commit", second), 1, `new commit: ${err.join("\n")}`);
  assert.match(err.join("\n"), /snapshot mismatch/);
  // Neither the old nor the new commit is reported: one file moved to the new commit, the rest stayed.
  const heads = ["a.rs", "b.rs", "c.rs", "d.rs"].map((n) => fs.readFileSync(path.join(f.dir, n), "utf8").trim());
  assert.ok(heads.some((h) => h.endsWith("2")) && heads.some((h) => h.endsWith("1")), `a partial snapshot: ${heads}`);
  assert.equal(await cli("--commit", second), 0, `the next sync converges: ${err.join("\n")}`);
  assert.equal(await cli("--verify", "--commit", second), 0, err.join("\n"));
  assert.equal(fs.existsSync(`${f.dir}.staging`), false, "the dead sync's staging directory is gone");
  assert.equal(fs.existsSync(`${f.dir}.lock`), false, "the dead sync's lock was reclaimed and released");
});

test("5.12 the killed gate's ownership stays held while its recorded child survives: a second sync is busy, the snapshot stays partial, and it syncs once the child is gone", { timeout: 180_000 }, async (t) => {
  const f = fixture(t, { "Cargo.toml": "[workspace]\n", "a.rs": "a1\n", "b.rs": "b1\n", "c.rs": "c1\n", ".gitignore": "target/\n" });
  const first = f.commit;
  write(f.repo, { "a.rs": "a2\n", "b.rs": "b2\n", "c.rs": "c2\n" });
  const second = commitAll(f.repo, "all change");
  syncSnapshot({ checkout: f.repo, rev: first, root: f.root });
  const file = ownershipFile({ platform: process.platform, beside: f.root });
  const killed = await killedSync(t, { repo: f.repo, rev: second, root: f.root, dir: f.dir, killAfter: 2, ownershipFile: file });
  const handle = JSON.parse(fs.readFileSync(file, "utf8")).handle;
  assert.ok(handle, "the killed gate's owner record is still there");
  // `replay`: the step's ownership judges the host from the RECORDED real-Mac process table (test-support/mac-process-table.mjs, built
  // by the ownership lane, signed/item5-sage-decisions-2.md D1) instead of the live host's, overlaid with the step's own process chain:
  // the killed gate and its sleeper are absent from the table, so they are gone, and no unrelated live daemon can decide the verdict.
  const stepWith = (replay, ...args) => {
    const work = tmp(t, "rk-snap-step-");
    const driver = path.join(work, "step.mjs");
    fs.writeFileSync(driver, `
const { snapshotCommand } = await import(${JSON.stringify(pathToFileURL(path.resolve("snapshot.mjs")).href)});
const lines = [];
let acquireOwnership;
if (${JSON.stringify(replay)}) {
  const { acquireBuildOwnership } = await import(${JSON.stringify(pathToFileURL(path.resolve("build-ownership.mjs")).href)});
  const { macTableProbe } = await import(${JSON.stringify(pathToFileURL(path.resolve("test-support", "mac-process-table.mjs")).href)});
  const probe = macTableProbe({ own: [process.pid, process.ppid] });
  acquireOwnership = (q) => acquireBuildOwnership({ ...q, probe });
}
const code = snapshotCommand({ cwd: ${JSON.stringify(f.repo)}, env: process.env, platform: process.platform, snapshotRoot: ${JSON.stringify(f.root)}, out: (m) => lines.push(m), err: (m) => lines.push(m), ...(acquireOwnership ? { acquireOwnership } : {}) }, ${JSON.stringify(args)});
console.log(JSON.stringify({ code, lines }));
process.exit(0);
`);
    const env = { ...process.env, NODE_OPTIONS: "" }; delete env.RIGHTKIT_BUILD_OWNERSHIP; delete env.RIGHTKIT_BUILD_OWNERSHIP_FILE;
    const r = spawnSync(process.execPath, [driver], { env, encoding: "utf8", timeout: 120_000 });
    try { return JSON.parse(String(r.stdout).trim().split(/\r?\n/).pop()); } catch { return { code: null, lines: [r.stdout, r.stderr] }; }
  };
  const step = (...args) => stepWith(false, ...args);
  const verifiesAs = (rev) => { try { verifySnapshot({ checkout: f.repo, rev, root: f.root }); return true; } catch (e) { return e.code === "mismatch" ? false : Promise.reject(e); } };
  if (process.platform === "win32") {
    // Every member of the dead gate's job died with it (the G1 fixture): the record drains, and the next sync reclaims it.
    const end = Date.now() + 60_000;
    while (readHolder(holderFile(file, handle))?.state !== "drained") { if (Date.now() > end) throw new Error("the killed gate's job never drained"); await new Promise((r) => setTimeout(r, 200)); }
  } else {
    assert.ok(alive(killed.sleeper), "the gate's recorded child outlived it");
    const busy = step("--commit", second, "--json");
    assert.equal(busy.code, 1, `busy, not synced: ${JSON.stringify(busy)}`);
    assert.match(busy.lines.join("\n"), /snapshot busy: build ownership .* is gone but/);
    assert.equal(verifiesAs(first), false, "the snapshot is partial: not the old commit");
    assert.equal(verifiesAs(second), false, "the snapshot is partial: not the new commit");
    process.kill(killed.sleeper, "SIGKILL");
    await untilGone(killed.sleeper);
  }
  // The "syncs once the child is gone" half: exit 0 AND a clean release, one outcome only. On the Mac the host's judgement comes from the
  // recorded process table (a test that passes on two outcomes is not evidence); the busy half above stays native.
  const synced = stepWith(process.platform === "darwin", "--commit", second, "--json");
  assert.equal(synced.code, 0, JSON.stringify(synced));
  assert.doesNotMatch(synced.lines.join("\n"), /kept:/, "the release was clean");
  const r = JSON.parse(synced.lines.find((l) => l.startsWith("{")));
  assert.equal(r.commit, second);
  assert.equal(r.ownership.reclaimed.from, handle, "the sync reclaimed the dead gate's ownership with the proof");
  assert.equal(verifySnapshot({ checkout: f.repo, rev: second, root: f.root }).commit, second);
});

// Row 5.13: the sync never touches the primary checkout.
/** Everything about a checkout's git state a sync must leave alone: every file of `.git` but the object database, and git's own views. */
function checkoutState(repo) {
  const gitDir = path.join(repo, ".git");
  const files = {};
  const walk = (d) => { for (const e of fs.readdirSync(d, { withFileTypes: true })) { const p = path.join(d, e.name); const rel = path.relative(gitDir, p).split(path.sep).join("/"); if (rel === "objects") continue; if (e.isDirectory()) walk(p); else files[rel] = crypto.createHash("sha256").update(fs.readFileSync(p)).digest("hex"); } };
  walk(gitDir);
  const g = (...a) => { const r = spawnSync("git", a, { cwd: repo, encoding: "utf8", env: { ...process.env, GIT_OPTIONAL_LOCKS: "0" } }); return `${r.status}:${r.stdout}`; };
  return {
    head: fs.readFileSync(path.join(gitDir, "HEAD"), "utf8"),
    index: crypto.createHash("sha256").update(fs.readFileSync(path.join(gitDir, "index"))).digest("hex"),
    status: g("status", "--porcelain=v2", "--branch", "--untracked-files=all"),
    stash: g("stash", "list"),
    refs: g("for-each-ref", "--format=%(refname) %(objectname)"),
    worktrees: fs.existsSync(path.join(gitDir, "worktrees")) ? fs.readdirSync(path.join(gitDir, "worktrees")).sort() : [],
    files,
  };
}

test("5.13 the sync never touches the checkout: HEAD, index, status, stash, refs, worktrees and every file of .git but objects are byte-equal before and after; the assertion sees each kind of change", (t) => {
  const f = fixture(t);
  const older = f.commit;
  write(f.repo, { "src/lib.rs": "pub fn b() {}\n", "added.rs": "x\n" });
  commitAll(f.repo, "head moves on");
  write(f.repo, { "README.md": "dirty edit\n" });
  git(f.repo, "stash", "push", "-q", "-m", "kept", "--", "README.md");
  write(f.repo, { "README.md": "dirty again\n", "untracked.rs": "u\n" });
  git(f.repo, "add", "untracked.rs");
  git(f.repo, "worktree", "add", "-q", "-b", "side", path.join(tmp(t, "rk-snap-wt-"), "wt"));
  const before = checkoutState(f.repo);
  assert.notDeepEqual(before.worktrees, [], "the fixture has a linked worktree");
  assert.match(before.stash, /kept/);
  // A non-HEAD commit: sync, warm repeat, verify and the CLI, all through the real code.
  const r = syncSnapshot({ checkout: f.repo, rev: older, root: f.root });
  assert.equal(r.commit, older);
  syncSnapshot({ checkout: f.repo, rev: older, root: f.root });
  verifySnapshot({ checkout: f.repo, rev: older, root: f.root });
  assert.equal(snapshotCommand({ cwd: f.repo, env: process.env, platform: process.platform, snapshotRoot: f.root, out: () => {}, err: () => {}, acquireOwnership: () => ({ handle: "h", joined: false, reclaimed: null, release: () => ({ released: true }) }) }, ["--commit", older]), 0);
  assert.deepEqual(checkoutState(f.repo), before);
  // The assertion can see a change: each double touches the checkout the way a clone, worktree, branch or stash would.
  const doubles = {
    "the index": () => git(f.repo, "add", "-A"),
    "HEAD": () => git(f.repo, "checkout", "-q", "--detach", older),
    "a branch": () => git(f.repo, "branch", "double-branch"),
    "the stash": () => git(f.repo, "stash", "push", "-q", "-m", "double"),
    "a worktree": () => git(f.repo, "worktree", "add", "-q", "--detach", path.join(tmp(t, "rk-snap-wt2-"), "wt"), older),
    "a file of .git": () => fs.writeFileSync(path.join(f.repo, ".git", "rightkit-double"), "x"),
  };
  for (const [name, double] of Object.entries(doubles)) {
    const was = checkoutState(f.repo);
    double();
    assert.notDeepEqual(checkoutState(f.repo), was, `the state check sees a change to ${name}`);
  }
});

// Row 5.15: the gate never creates the managed root, and on the Mac the managed root is on another device than `/`.
// A fixture's managed root is a temporary directory on the boot volume; the Mac device rule is injected away except where it is the subject.
const managedVolume = (p) => (p === "/" ? 1n : 2n);
test("5.15 an absent managed root refuses with a named reason and nothing exists afterwards; sync, verify and the CLI (which takes no ownership first)", (t) => {
  const f = fixture(t);
  const parent = tmp(t, "rk-snap-parent-");
  const managed = path.join(parent, ".rightkit-managed");
  const root = path.join(managed, "snapshots");
  const refuses = (fn, reason) => assert.throws(fn, (e) => e instanceof SnapshotError && e.code === "refused" && e.reason === reason, reason);
  refuses(() => syncSnapshot({ checkout: f.repo, rev: f.commit, root, managedRoot: managed, device: managedVolume }), "managed-root-missing");
  refuses(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root, managedRoot: managed, device: managedVolume }), "managed-root-missing");
  assert.deepEqual(fs.readdirSync(parent), [], "nothing was created, not the managed root, not snapshots");
  const err = [];
  assert.equal(snapshotCommand({ cwd: f.repo, env: process.env, platform: process.platform, snapshotRoot: root, managedRoot: managed, device: managedVolume, out: () => {}, err: (m) => err.push(m), acquireOwnership: () => { throw new Error("ownership must not be taken first: it creates directories"); } }, ["--commit", f.commit]), 1);
  assert.match(err.join("\n"), /snapshot refused \(managed-root-missing\)/);
  assert.deepEqual(fs.readdirSync(parent), []);
  // A managed root that is a file or a link is not a managed root either.
  fs.writeFileSync(managed, "x");
  refuses(() => syncSnapshot({ checkout: f.repo, rev: f.commit, root, managedRoot: managed, device: managedVolume }), "managed-root-missing");
  fs.rmSync(managed);
  // The root's own parent must exist as well: the gate creates `snapshots`, never the directories above it.
  fs.mkdirSync(managed);
  refuses(() => syncSnapshot({ checkout: f.repo, rev: f.commit, root: path.join(managed, "deeper", "snapshots"), managedRoot: managed, device: managedVolume }), "root-parent-missing");
  assert.deepEqual(fs.readdirSync(managed), []);
});

test("5.15 an existing managed root gets snapshots/<repo-key> created beneath it and is itself left alone (its mode and listing)", (t) => {
  const f = fixture(t);
  const managed = path.join(tmp(t, "rk-snap-parent-"), ".rightkit-managed");
  fs.mkdirSync(managed, { mode: 0o755 });
  const before = { mode: fs.statSync(managed).mode, names: fs.readdirSync(managed) };
  const root = path.join(managed, "snapshots");
  const r = syncSnapshot({ checkout: f.repo, rev: f.commit, root, managedRoot: managed, device: managedVolume });
  assert.equal(r.path, path.join(root, repoKey(keyPath(f.repo))));
  assert.deepEqual(fs.readdirSync(managed), ["snapshots"]);
  assert.equal(fs.statSync(managed).mode, before.mode);
  verifySnapshot({ checkout: f.repo, rev: f.commit, root, managedRoot: managed, device: managedVolume });
});

test("5.15 Mac: a managed root on the same device as / refuses (an unmounted volume cannot yield a boot-volume substitute); another device passes; Windows has no device rule", (t) => {
  const managed = tmp(t, "rk-snap-managed-");
  const onBoot = () => 7n;
  const split = (p) => (p === "/" ? 7n : 9n);
  assert.throws(() => requireManagedRoot(managed, { platform: "darwin", device: onBoot }), (e) => e instanceof SnapshotError && e.code === "refused" && e.reason === "managed-root-on-boot-volume");
  assert.equal(requireManagedRoot(managed, { platform: "darwin", device: split }), managed);
  assert.equal(requireManagedRoot(managed, { platform: "win32", device: onBoot }), managed);
  assert.equal(requireManagedRoot(managed, { platform: "linux", device: onBoot }), managed);
  assert.throws(() => requireManagedRoot(managed, { platform: "darwin", device: () => { throw new Error("EIO"); } }), (e) => e.code === "error");
  // Through the sync, where the host really is a Mac: the same refusal, before any write.
  if (process.platform === "darwin") {
    const f = fixture(t);
    const root = path.join(managed, "snapshots");
    assert.throws(() => syncSnapshot({ checkout: f.repo, rev: f.commit, root, managedRoot: managed, device: onBoot }), (e) => e.reason === "managed-root-on-boot-volume");
    assert.deepEqual(fs.readdirSync(managed), []);
    assert.equal(syncSnapshot({ checkout: f.repo, rev: f.commit, root, managedRoot: managed, device: split }).commit, f.commit);
  }
});

test("5.15 the host's snapshot root is guarded by its path, however it is passed: spellings of the real root, and roots under it, find the managed root; no other root does", () => {
  assert.equal(hostManagedRootFor("/Volumes/D/.rightkit-managed/snapshots", { platform: "darwin" }), "/Volumes/D/.rightkit-managed");
  assert.equal(hostManagedRootFor("/volumes/d/.RIGHTKIT-managed/snapshots", { platform: "darwin" }), "/Volumes/D/.rightkit-managed", "a case-variant spelling is the same root on APFS");
  assert.equal(hostManagedRootFor("/Volumes/D/.rightkit-managed/snapshots/../snapshots", { platform: "darwin" }), "/Volumes/D/.rightkit-managed");
  assert.equal(hostManagedRootFor("/Volumes/D/.rightkit-managed/test-only/snapshots", { platform: "darwin" }), "/Volumes/D/.rightkit-managed");
  assert.equal(hostManagedRootFor("D:\\.rightkit-managed\\snapshots", { platform: "win32" }), "D:\\.rightkit-managed");
  assert.equal(hostManagedRootFor("d:\\.RightKit-Managed\\snapshots", { platform: "win32" }), "D:\\.rightkit-managed");
  assert.equal(hostManagedRootFor("/Volumes/D/.rightkit-managed-other/snapshots", { platform: "darwin" }), null, "a sibling is not under it");
  assert.equal(hostManagedRootFor("/Volumes/D/.rightkit-managed/../x/snapshots", { platform: "darwin" }), null);
  assert.equal(hostManagedRootFor("/tmp/x/snapshots", { platform: "darwin" }), null);
  assert.equal(hostManagedRootFor("/home/a/.local/state/rightkit/snapshots", { platform: "linux" }), null);
});

// Native on Mac and Dell: the real managed root. Read-only against the real root, and one write test inside a test-only subdirectory
// of it, removed afterwards. The row's own subject: the gate verifies it can create and write under the real managed root.
const realManaged = resolveManagedRootForHost();
function resolveManagedRootForHost() {
  if (process.platform !== "darwin" && process.platform !== "win32") return null;
  return process.platform === "win32" ? "D:\\.rightkit-managed" : "/Volumes/D/.rightkit-managed";
}
test("5.15 native: the real managed root exists, is a real directory (and on the Mac not on the device of /); the real snapshot root is read back read-only", { skip: realManaged && fs.existsSync(realManaged) ? false : "the host has no real managed root (a Mac or Dell with the managed volume)" }, () => {
  assert.equal(requireManagedRoot(realManaged), realManaged);
  assert.equal(snapshotRoot(), path.join(realManaged, "snapshots"));
  assert.equal(hostManagedRootFor(snapshotRoot()), realManaged);
  const st = fs.lstatSync(realManaged);
  assert.ok(st.isDirectory() && !st.isSymbolicLink());
  if (process.platform === "darwin") assert.notEqual(fs.statSync(realManaged, { bigint: true }).dev, fs.statSync("/", { bigint: true }).dev, "the managed root is on its own volume");
  const real = path.join(realManaged, "snapshots");
  if (fs.existsSync(real) && process.platform !== "win32") assert.equal(fs.statSync(real).mode & 0o077, 0, `the real snapshot root ${real} grants nothing to group or other`);
});

test("5.15 native: sync and verify beneath the real managed root (a test-only subdirectory, removed afterwards); the path is read back, and on the Mac it is on the managed volume", { skip: realManaged && fs.existsSync(realManaged) ? false : "the host has no real managed root (a Mac or Dell with the managed volume)" }, (t) => {
  const f = fixture(t);
  const sub = path.join(realManaged, `rk-l5s-5-15-${process.pid}-${crypto.randomBytes(4).toString("hex")}`);
  assert.ok(path.basename(sub).startsWith("rk-l5s-5-15-"));
  fs.mkdirSync(sub, { mode: 0o700 });
  t.after(() => fs.rmSync(sub, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 }));
  const root = path.join(sub, "snapshots");
  assert.equal(hostManagedRootFor(root), realManaged, "the guard keys on the real managed root for a root beneath it");
  const r = syncSnapshot({ checkout: f.repo, rev: f.commit, root });
  assert.equal(r.path, path.join(root, repoKey(keyPath(f.repo))));
  assert.equal(path.dirname(path.dirname(r.path)), sub);
  assert.equal(verifySnapshot({ checkout: f.repo, rev: f.commit, root }).path, r.path, "the path read back");
  assert.equal(fs.readFileSync(path.join(r.path, "src", "lib.rs"), "utf8"), "pub fn a() {}\n");
  if (process.platform === "darwin") assert.notEqual(fs.statSync(r.path, { bigint: true }).dev, fs.statSync("/", { bigint: true }).dev, "the snapshot sits on the managed volume");
  if (process.platform !== "win32") assert.equal(fs.statSync(root).mode & 0o777, 0o700, "the root the gate created is 0700");
});

// Row 5.16: the Linux root, end to end through the CLI (the resolution rules are tested with the root tests above).
test("5.16 native Linux root: a configured root is honoured by the CLI; with no file, XDG state; with neither, ~/.local/state; RIGHTKIT_SNAPSHOT_ROOT changes nothing", { skip: process.platform === "linux" ? false : "the Linux root rule; the resolution is also asserted for platform linux on every host" }, (t) => {
  const f = fixture(t);
  const home = path.join(os.homedir(), `.rk-l5s-root-test-${process.pid}-${crypto.randomBytes(3).toString("hex")}`);
  t.after(() => fs.rmSync(home, { recursive: true, force: true }));
  const run = (o) => { const out = [], err = []; const code = snapshotCommand({ cwd: f.repo, platform: "linux", out: (m) => out.push(m), err: (m) => err.push(m), acquireOwnership: () => ({ handle: "h", joined: false, reclaimed: null, release: () => ({ released: true }) }), ...o }, ["--commit", f.commit, "--json"]); return { code, out, err }; };
  const key = repoKey(keyPath(f.repo));
  const configured = path.join(home, "configured-snaps");
  let r = run({ env: { ...process.env, RIGHTKIT_SNAPSHOT_ROOT: path.join(home, "hostile") }, home, configFile: hostConfig(t, { snapshotRoot: configured }) });
  assert.equal(r.code, 0, r.err.join("\n"));
  assert.equal(JSON.parse(r.out.at(-1)).path, path.join(configured, key));
  assert.equal(fs.statSync(configured).mode & 0o777, 0o700, "the root the gate created is 0700");
  const xdg = path.join(home, "xdg");
  r = run({ env: { ...process.env, XDG_STATE_HOME: xdg, RIGHTKIT_SNAPSHOT_ROOT: path.join(home, "hostile") }, home, configFile: noConfig(t) });
  assert.equal(r.code, 0, r.err.join("\n"));
  assert.equal(JSON.parse(r.out.at(-1)).path, path.join(xdg, "rightkit", "snapshots", key));
  r = run({ env: { ...process.env, XDG_STATE_HOME: "", RIGHTKIT_SNAPSHOT_ROOT: path.join(home, "hostile") }, home, configFile: noConfig(t) });
  assert.equal(r.code, 0, r.err.join("\n"));
  assert.equal(JSON.parse(r.out.at(-1)).path, path.join(home, ".local", "state", "rightkit", "snapshots", key));
  assert.equal(fs.existsSync(path.join(home, "hostile")), false, "the variable selected nothing");
});

// Row 5.18: repo-key = sha256(canonical repository path)[:16].
test("5.18 one checkout, one key and one directory, whatever its spelling: a link, a relative path, a trailing separator, a subdirectory and (Mac, Windows) a case variant; distinct repositories have distinct keys", (t) => {
  const f = fixture(t);
  const want = repositoryOf(f.repo);
  assert.equal(want.key, repoKey(keyPath(fs.realpathSync.native(f.repo))));
  const spellings = { "the path itself": f.repo, "a trailing separator": f.repo + path.sep, "a subdirectory of the checkout": path.join(f.repo, "src") };
  const relative = path.relative(process.cwd(), f.repo);
  if (!path.isAbsolute(relative)) spellings["a relative path"] = relative;
  const linkDir = tmp(t, "rk-snap-spell-");
  try { fs.symlinkSync(f.repo, path.join(linkDir, "lnk"), dirLinkType); spellings["a link to it"] = path.join(linkDir, "lnk"); } catch { /* no link privilege: the other spellings still run */ }
  if (spellings["a link to it"]) spellings["a link to it, with a trailing separator"] = path.join(linkDir, "lnk") + path.sep;
  if (process.platform === "darwin" || process.platform === "win32") {
    const base = path.basename(f.repo);
    const swapped = base.replace(/[a-z]/gi, (c) => (c === c.toLowerCase() ? c.toUpperCase() : c.toLowerCase()));
    const variant = path.join(path.dirname(f.repo), swapped);
    if (swapped !== base && fs.existsSync(variant) && fs.statSync(variant).ino === fs.statSync(f.repo).ino) spellings["a case variant"] = variant;
    else t.diagnostic(`no distinct case variant of ${f.repo} resolves to it here`);
    assert.ok(spellings["a case variant"], "the case-insensitive volume resolves a case variant");
  }
  for (const [name, spelled] of Object.entries(spellings)) {
    const got = repositoryOf(spelled);
    assert.equal(got.key, want.key, `${name}: one key`);
    assert.equal(got.checkout, want.checkout, `${name}: one canonical path`);
    const r = syncSnapshot({ checkout: spelled, rev: f.commit, root: f.root });
    assert.equal(r.path, f.dir, `${name}: one directory`);
    assert.equal(r.repoKey, want.key);
  }
  assert.deepEqual(fs.readdirSync(f.root).filter((n) => /^[0-9a-f]{16}$/.test(n)), [want.key], "one snapshot directory for all spellings");
  const other = fixture(t);
  assert.notEqual(repositoryOf(other.repo).key, want.key, "distinct repositories have distinct keys");
  assert.notEqual(other.dir, f.dir);
});

// Row 5.22, root-mode half: a snapshot root is private to this user.
const posixModes = process.platform === "win32" ? "POSIX modes" : false;
test("5.22 a snapshot root the gate creates is 0700, whatever the umask, with every directory it had to create", { skip: posixModes }, (t) => {
  const f = fixture(t);
  const top = path.join(tmp(t, "rk-snap-mode-"), "a", "b");
  const root = path.join(top, "snapshots");
  const umask = process.umask(0);
  try { syncSnapshot({ checkout: f.repo, rev: f.commit, root }); } finally { process.umask(umask); }
  assert.equal(fs.statSync(root).mode & 0o777, 0o700);
  assert.equal(fs.statSync(top).mode & 0o777, 0o700, "a directory made on the way is private too");
  verifySnapshot({ checkout: f.repo, rev: f.commit, root });
});

test("5.22 an existing root that grants group or other any access is refused by sync, verify and the CLI, naming the mode, and nothing in it is touched", { skip: posixModes }, (t) => {
  const f = fixture(t);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  const before = { names: fs.readdirSync(f.root).sort(), mtimes: mtimes(f.dir) };
  for (const mode of [0o755, 0o750, 0o705, 0o770, 0o707, 0o711, 0o777]) {
    fs.chmodSync(f.root, mode);
    const label = mode.toString(8);
    assert.throws(() => syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e instanceof SnapshotError && e.code === "refused" && e.reason === "root-open-mode" && e.message.includes(`0${label}`), `sync at ${label}`);
    assert.throws(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e.reason === "root-open-mode", `verify at ${label}`);
    const err = [];
    assert.equal(snapshotCommand({ cwd: f.repo, env: process.env, platform: process.platform, snapshotRoot: f.root, out: () => {}, err: (m) => err.push(m), acquireOwnership: () => { throw new Error("must not acquire"); } }, ["--commit", f.commit]), 1, `CLI at ${label}`);
    assert.match(err.join("\n"), /snapshot refused \(root-open-mode\)/);
    assert.equal(snapshotCommand({ cwd: f.repo, env: process.env, platform: process.platform, snapshotRoot: f.root, out: () => {}, err: () => {} }, ["--verify", "--commit", f.commit]), 1, `CLI verify at ${label}`);
  }
  fs.chmodSync(f.root, 0o700);
  assert.deepEqual({ names: fs.readdirSync(f.root).sort(), mtimes: mtimes(f.dir) }, before, "the refusals wrote nothing and deleted nothing");
  assert.equal(syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root }).commit, f.commit, "a private root works");
  // A root that predates birth records is refused when open, before the migration could empty it.
  fs.chmodSync(f.root, 0o755);
  fs.writeFileSync(path.join(f.root, "0123456789abcdef"), "a snapshot-shaped entry the migration would delete");
  assert.throws(() => syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e.reason === "root-open-mode");
  assert.equal(fs.existsSync(path.join(f.root, "0123456789abcdef")), true);
});

test("5.13 the temporary index is outside the checkout's .git: it lives in the temp directory (a temp directory that cannot be made fails the identity; .git is not a fallback)", (t) => {
  const f = fixture(t);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  const before = checkoutState(f.repo);
  const saved = { TMPDIR: process.env.TMPDIR, TEMP: process.env.TEMP, TMP: process.env.TMP };
  const restore = () => { for (const [k, v] of Object.entries(saved)) { if (v === undefined) delete process.env[k]; else process.env[k] = v; } };
  t.after(restore);
  const gone = path.join(f.root, "no-such-temp-directory");
  Object.assign(process.env, { TMPDIR: gone, TEMP: gone, TMP: gone });
  assert.throws(() => snapshotTree({ checkout: f.repo, dir: f.dir, baseTree: git(f.repo, "rev-parse", `${f.commit}^{tree}`) }), (e) => e.code === "ENOENT" || /ENOENT|no such file/i.test(e.message));
  restore();
  assert.equal(snapshotTree({ checkout: f.repo, dir: f.dir, baseTree: git(f.repo, "rev-parse", `${f.commit}^{tree}`) }), git(f.repo, "rev-parse", `${f.commit}^{tree}`));
  assert.deepEqual(checkoutState(f.repo), before, "neither run left anything in .git");
});

test("5.13 a hook of the editable checkout never runs while the gate computes the snapshot's identity (it could write into the snapshot after it was walked)", (t) => {
  const f = fixture(t);
  const marker = path.join(f.root, "..", `hook-ran-${path.basename(f.root)}`);
  t.after(() => fs.rmSync(marker, { force: true }));
  const hook = path.join(f.repo, ".git", "hooks", "post-index-change");
  fs.mkdirSync(path.dirname(hook), { recursive: true });
  fs.writeFileSync(hook, `#!/bin/sh\necho ran >> '${marker.replace(/\\/g, "/")}'\n`, { mode: 0o755 });
  // The hook is real: a plain index write in the checkout runs it (so its absence below is the gate's doing, not a broken fixture).
  write(f.repo, { "probe.txt": "x\n" });
  git(f.repo, "add", "probe.txt");
  assert.equal(fs.existsSync(marker), true, "the fixture's hook runs when git itself writes the checkout's index");
  fs.rmSync(marker);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  assert.equal(fs.existsSync(marker), false, "the gate's git runs never executed the checkout's hook");
});

// ============================================================================================================================
// Row 5.09d: the stage-output ledger (signed/item7-sage-decisions-2.md Q1, "Ownership: L5S"). An app-shaped fixture: committed
// src-tauri, a .gitignore covering the sidecar directory and gen/, and what a door stage leaves behind.
// ============================================================================================================================
const APP_FILES = {
  ".gitignore": "target/\nnode_modules/\n.right-release/\nsrc-tauri/binaries/\nsrc-tauri/gen/\n*.log\n",
  "package.json": "{\"name\":\"app\",\"private\":true}\n",
  "src-tauri/Cargo.toml": "[package]\nname = \"app\"\nversion = \"0.0.0\"\n",
  "src-tauri/build.rs": "fn main() {}\n",
  "src-tauri/src/main.rs": "fn main() {}\n",
  "src-tauri/tauri.conf.json": "{\"bundle\":{\"externalBin\":[\"binaries/tool\"]}}\n",
};
const TOOL = Buffer.from("not really an executable, but bytes a compile reads\n");
/** What the door's stage-sidecar step and a Tauri build leave in the snapshot: a staged sidecar, its receipt and gen/schemas. */
function stageOutputs(dir) {
  const tool = path.join(dir, "src-tauri", "binaries", "tool-x86_64-unknown-linux-gnu");
  fs.mkdirSync(path.dirname(tool), { recursive: true });
  fs.writeFileSync(tool, TOOL, { mode: 0o755 });
  if (process.platform !== "win32") fs.chmodSync(tool, 0o755);
  write(dir, { "src-tauri/binaries/stage-sidecar.receipt.json": "{\"staged\":1}\n", "src-tauri/gen/schemas/capabilities.json": "{}\n" });
  return tool;
}
const sha = (b) => crypto.createHash("sha256").update(b).digest("hex");
const appFixture = (t) => { const f = fixture(t, APP_FILES); syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root }); return f; };
const endVerify = (f, extra = {}) => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root, admitNew: true, ...extra });
const startVerify = (f, ledger) => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root, ledger });
const refusesLedger = (fn, re, name) => assert.throws(fn, (e) => e instanceof SnapshotError && e.code === "mismatch" && re.test(e.message), name);

test("5.09d the end-of-stage verify admits the stage's ignored outputs and returns them as {path, type, size, sha256, execBit}; the tree id stays the commit's", (t) => {
  const f = appFixture(t);
  assert.deepEqual(endVerify(f).ledger, [], "a stage that wrote nothing admits nothing");
  const tool = stageOutputs(f.dir);
  const r = endVerify(f);
  assert.equal(r.tree, git(f.repo, "rev-parse", `${f.commit}^{tree}`), "an ignored output does not change the tree id");
  const byPath = Object.fromEntries(r.ledger.map((e) => [e.path, e]));
  assert.deepEqual(Object.keys(byPath).sort(), [
    "src-tauri/binaries", "src-tauri/binaries/stage-sidecar.receipt.json", "src-tauri/binaries/tool-x86_64-unknown-linux-gnu",
    "src-tauri/gen", "src-tauri/gen/schemas", "src-tauri/gen/schemas/capabilities.json",
  ]);
  assert.deepEqual(byPath["src-tauri/binaries/tool-x86_64-unknown-linux-gnu"], { path: "src-tauri/binaries/tool-x86_64-unknown-linux-gnu", type: "file", size: TOOL.length, sha256: sha(TOOL), execBit: process.platform !== "win32" });
  assert.deepEqual(byPath["src-tauri/gen/schemas/capabilities.json"], { path: "src-tauri/gen/schemas/capabilities.json", type: "file", size: 3, sha256: sha("{}\n"), execBit: false });
  assert.deepEqual(byPath["src-tauri/binaries"], { path: "src-tauri/binaries", type: "dir", size: 0, sha256: null, execBit: false });
  assert.deepEqual(r.ledger.map((e) => e.path), [...r.ledger.map((e) => e.path)].sort(), "sorted by path");
  // The next stage's start verify: exactly tree + ledger passes; its result carries the same entries.
  assert.deepEqual(startVerify(f, r.ledger).ledger, r.ledger);
  // The ledger may list files only: the directories that parent them are implied.
  assert.equal(startVerify(f, r.ledger.filter((e) => e.type === "file")).ledger.length, r.ledger.length);
  // Re-admitting at a later stage's end with the same ledger and a rewritten generated output is fine (generated entries are not in the ledger).
  fs.writeFileSync(path.join(f.dir, "src-tauri", "gen", "schemas", "capabilities.json"), "{\"v\":2}\n");
  const second = endVerify(f, { ledger: r.ledger.filter((e) => e.path.startsWith("src-tauri/binaries")) });
  assert.equal(second.ledger.find((e) => e.path.endsWith("capabilities.json")).sha256, sha("{\"v\":2}\n"));
  // Without a ledger option nothing is admitted: the plain verify (and `snapshot --verify`) stays as strict as the sync.
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e.code === "mismatch" && /src-tauri\/binaries is not in the commit's tree/.test(e.message));
  assert.equal("ledger" in verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root, dir: f.dir, ledger: [] , admitNew: true }), true);
  // The next sync removes every ledger entry (5.03 is unchanged) and the plain verify holds again.
  const synced = syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  assert.ok(synced.removed >= 1);
  assert.equal(fs.existsSync(tool), false);
  assert.equal(verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }).commit, f.commit);
});

test("5.09d the start verify holds the ledger exactly: a changed staged byte, a missing entry, a new entry and a changed exec bit refuse; so does a staged sidecar changed before the end verify", (t) => {
  const f = appFixture(t);
  const tool = stageOutputs(f.dir);
  const { ledger } = endVerify(f);
  const toolRel = "src-tauri/binaries/tool-x86_64-unknown-linux-gnu";
  // Changed between compile and package (the next stage's start verify).
  fs.appendFileSync(tool, "tampered\n");
  refusesLedger(() => startVerify(f, ledger), new RegExp(`${toolRel} is a ledger entry and changed`), "a ledger byte changed");
  // Staged entries are immutable: the end verify of the same stage refuses too when the staged ledger is passed with admitNew.
  refusesLedger(() => endVerify(f, { ledger }), /ledger entry and changed/, "a staged sidecar changed between staging and the end verify");
  fs.writeFileSync(tool, TOOL, { mode: 0o755 });
  if (process.platform !== "win32") fs.chmodSync(tool, 0o755);
  assert.equal(startVerify(f, ledger).commit, f.commit, "restored");
  if (process.platform !== "win32") {
    fs.chmodSync(tool, 0o644);
    refusesLedger(() => startVerify(f, ledger), /ledger entry and changed/, "the exec bit");
    fs.chmodSync(tool, 0o755);
  }
  // Missing.
  fs.rmSync(tool);
  refusesLedger(() => startVerify(f, ledger), new RegExp(`${toolRel} is a ledger entry .* and is missing`), "a missing entry");
  fs.writeFileSync(tool, TOOL, { mode: 0o755 });
  if (process.platform !== "win32") fs.chmodSync(tool, 0o755);
  // A new entry at the start of a stage is refused (admitNew is for the end of a stage only), ignored or not.
  write(f.dir, { "src-tauri/gen/schemas/new.json": "{}\n" });
  refusesLedger(() => startVerify(f, ledger), /src-tauri\/gen\/schemas\/new\.json is not in the commit's tree and not in the stage-output ledger/, "a new entry at the start");
  fs.rmSync(path.join(f.dir, "src-tauri", "gen", "schemas", "new.json"));
  assert.equal(startVerify(f, ledger).commit, f.commit);
  // An ignored file planted BEFORE a stage: the start verify (no admitNew, empty ledger) stays strict, as does the plain verify.
  const g = appFixture(t);
  write(g.dir, { "planted.log": "ignored by *.log\n" });
  refusesLedger(() => startVerify(g, []), /planted\.log is not in the commit's tree and not in the stage-output ledger/, "planted before a stage");
  assert.throws(() => verifySnapshot({ checkout: g.repo, rev: g.commit, root: g.root }), (e) => e.code === "mismatch" && /planted\.log/.test(e.message));
});

test("5.09d admission refuses: an output at a non-ignored path, one the checkout's info/exclude or a global excludes file ignores, a planted .gitignore, and a planted .rightkit-ledger.json", (t) => {
  const f = appFixture(t);
  const plant = (files) => write(f.dir, files);
  const clean = (...names) => { for (const n of names) fs.rmSync(path.join(f.dir, ...n.split("/")), { recursive: true, force: true }); };
  plant({ "src-tauri/notes.txt": "not ignored\n" });
  refusesLedger(() => endVerify(f), /src-tauri\/notes\.txt is not in the commit's tree and is not ignored by the commit's own ignore rules/, "a non-ignored path");
  plant({ "src-tauri/other/x.bin": "x\n" });
  clean("src-tauri/notes.txt");
  refusesLedger(() => endVerify(f), /src-tauri\/other is not in the commit's tree and is not ignored/, "a non-ignored directory");
  clean("src-tauri/other");
  // Ignored only by the checkout's info/exclude.
  fs.mkdirSync(path.join(f.repo, ".git", "info"), { recursive: true });
  fs.appendFileSync(path.join(f.repo, ".git", "info", "exclude"), "local-only.rs\n");
  plant({ "local-only.rs": "x\n" });
  refusesLedger(() => endVerify(f), /local-only\.rs is not in the commit's tree and is not ignored/, "info/exclude");
  clean("local-only.rs");
  // Ignored only by a global excludes file.
  const globalExcludes = path.join(tmp(t, "rk-snap-gx-"), "excludes");
  fs.writeFileSync(globalExcludes, "*.cache\n");
  git(f.repo, "config", "core.excludesFile", globalExcludes);
  plant({ "src-tauri/x.cache": "x\n" });
  refusesLedger(() => endVerify(f), /x\.cache is not in the commit's tree and is not ignored/, "a global excludes file");
  clean("src-tauri/x.cache");
  // An untracked .gitignore would decide what counts as ignored: it is refused as such, and what it would name is not admitted either.
  plant({ "src-tauri/binaries/.gitignore": "*\n", "src-tauri/binaries/x": "x\n" });
  refusesLedger(() => endVerify(f), /\.gitignore is an ignore file the commit does not have/, "a planted .gitignore in an ignored directory");
  clean("src-tauri/binaries");
  plant({ "evil/.gitignore": "*\n", "evil/payload": "x\n" });
  refusesLedger(() => endVerify(f), /evil\/\.gitignore is an ignore file the commit does not have|evil is not in the commit's tree and is not ignored/, "a planted .gitignore that would ignore its own directory");
  clean("evil");
  // A ledger record planted in the snapshot is just an unledgered extra (a record stored in the tree it attests is invalid).
  plant({ ".rightkit-ledger.json": "{\"entries\":[]}\n" });
  refusesLedger(() => endVerify(f), /\.rightkit-ledger\.json is not in the commit's tree and is not ignored/, "a planted ledger file");
  clean(".rightkit-ledger.json");
  assert.deepEqual(endVerify(f).ledger, [], "the fixture is clean again");
});

test("5.09d admission refuses reserved paths even when ignored: target/, .right-release/, .git, node_modules outside the pnpm roots; a link; a hard link", (t) => {
  const f = appFixture(t);
  const refuse = (rel, body, re, name) => {
    write(f.dir, { [rel]: body });
    refusesLedger(() => endVerify(f), re, name ?? rel);
    fs.rmSync(path.join(f.dir, ...rel.split("/")[0] === "src-tauri" ? ["src-tauri", rel.split("/")[1]] : [rel.split("/")[0]]), { recursive: true, force: true });
  };
  refuse("src-tauri/target/debug/app", "x\n", /src-tauri\/target is under "target", a reserved path/);
  refuse("target/debug/x.o", "x\n", /target is under "target"/);
  refuse(".right-release/state.json", "{}\n", /\.right-release is under "\.right-release"/);
  refuse("src-tauri/sub/.git/config", "x\n", /is under ".git"/, "a .git below the root");
  refuse("node_modules/x/index.js", "x\n", /node_modules is under "node_modules"/, "node_modules where no pnpm root installs");
  // A link of any kind is refused, and a link inside an admitted directory too.
  if (canLink) {
    fs.mkdirSync(path.join(f.dir, "src-tauri", "binaries"), { recursive: true });
    fs.symlinkSync(path.join(f.dir, "src-tauri", "Cargo.toml"), path.join(f.dir, "src-tauri", "binaries", "link"));
    refusesLedger(() => endVerify(f), /binaries\/link is a link/, "a link inside an admitted directory");
    fs.rmSync(path.join(f.dir, "src-tauri", "binaries"), { recursive: true });
    fs.mkdirSync(path.join(f.repo, "outputs"));
    fs.mkdirSync(path.join(f.dir, "src-tauri", "gen"));
    fs.symlinkSync(path.join(f.repo, "outputs"), path.join(f.dir, "src-tauri", "gen", "out"), dirLinkType);
    refusesLedger(() => endVerify(f), /gen\/out is a link/, "a directory link into the checkout");
    fs.rmSync(path.join(f.dir, "src-tauri", "gen"), { recursive: true });
  } else assertNoLinkHost(t);
  // A hard link to a checkout file with equal bytes.
  write(f.repo, { "payload.txt": "bytes\n" });
  fs.mkdirSync(path.join(f.dir, "src-tauri", "binaries"), { recursive: true });
  fs.linkSync(path.join(f.repo, "payload.txt"), path.join(f.dir, "src-tauri", "binaries", "hard"));
  refusesLedger(() => endVerify(f), /binaries\/hard has \d+ hard links/, "a hard link");
  fs.rmSync(path.join(f.dir, "src-tauri", "binaries"), { recursive: true });
  assert.deepEqual(endVerify(f).ledger, []);
});

test("5.09d a generator that rewrites a tracked file is not admitted: the tree id refuses it (genright's tracked gen/schemas shape)", (t) => {
  const f = fixture(t, { ...APP_FILES, "tracked/gen/schemas.json": "{\"v\":1}\n" });
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  stageOutputs(f.dir);
  assert.equal(endVerify(f).commit, f.commit);
  fs.writeFileSync(path.join(f.dir, "tracked", "gen", "schemas.json"), "{\"v\":2}\n");
  assert.throws(() => endVerify(f), (e) => e.code === "mismatch" && /is tree [0-9a-f]+, not tree/.test(e.message));
  fs.rmSync(path.join(f.dir, "tracked", "gen", "schemas.json"));
  assert.throws(() => endVerify(f), (e) => e.code === "mismatch");
});

test("5.09d the neutral ignore oracle: only the snapshot's own .gitignore counts, never the checkout's info/exclude or a global excludes file (on Windows the null device is NUL: this test proves it there)", (t) => {
  const f = appFixture(t);
  fs.mkdirSync(path.join(f.repo, ".git", "info"), { recursive: true });
  fs.appendFileSync(path.join(f.repo, ".git", "info", "exclude"), "*.local\n");
  const globalExcludes = path.join(tmp(t, "rk-snap-gx-"), "excludes");
  fs.writeFileSync(globalExcludes, "*.globalx\n");
  git(f.repo, "config", "core.excludesFile", globalExcludes);
  write(f.dir, { "a.log": "x\n", "src-tauri/binaries/b": "x\n", "c.local": "x\n", "d.globalx": "x\n", "e.txt": "x\n" });
  assert.deepEqual([...neutralIgnored(f.dir, ["a.log", "src-tauri/binaries/b", "c.local", "d.globalx", "e.txt"])].sort(), ["a.log", "src-tauri/binaries/b"]);
  assert.deepEqual([...neutralIgnored(f.dir, [])], []);
  // The control: with the checkout's own git directory the same paths ARE ignored (info/exclude, global excludes), which is why the oracle may not use it.
  const control = (p) => spawnSync("git", [`--git-dir=${path.join(f.repo, ".git")}`, `--work-tree=${f.dir}`, "check-ignore", "--no-index", "-q", p], { cwd: f.dir }).status;
  assert.equal(control("c.local"), 0, "the checkout's info/exclude widens what its own git directory ignores");
  assert.equal(control("d.globalx"), 0, "and so does a global excludes file");
  assert.equal(control("e.txt"), 1);
  // A committed nested .gitignore is the commit's own rule and counts.
  const g = fixture(t, { ...APP_FILES, "sub/.gitignore": "*.gen\n", "sub/keep.txt": "k\n" });
  syncSnapshot({ checkout: g.repo, rev: g.commit, root: g.root });
  write(g.dir, { "sub/x.gen": "x\n", "other.gen": "x\n" });
  assert.deepEqual([...neutralIgnored(g.dir, ["sub/x.gen", "other.gen"])], ["sub/x.gen"]);
});

test("5.09d the CLI `snapshot --verify` has no ledger: it still refuses an added ignored file (5.08 unchanged) while the library admits it at the end of a stage", async (t) => {
  const f = appFixture(t);
  stageOutputs(f.dir);
  const out = [], err = [];
  const io = { cwd: f.repo, env: process.env, snapshotRoot: f.root, out: (m) => out.push(m), err: (m) => err.push(m) };
  assert.equal(await main(["snapshot", "--verify", "--commit", f.commit], io), 1);
  assert.match(err.join("\n"), /snapshot mismatch: .*src-tauri\/binaries is not in the commit's tree/);
  assert.equal(await main(["snapshot", "--verify", "--commit", f.commit, "--ledger", "x"], io), 2, "no flag turns the CLI's verify into an admitting one");
  assert.equal(endVerify(f).commit, f.commit);
});

// ============================================================================================================================
// Item 5 D3 (signed/item5-sage-decisions-2.md): the sealed git invocation. The checkout's git configuration cannot reach the
// snapshot's identity: no filter driver, no eol conversion, no attributes from info/attributes or an attributes file, no hook, no
// user-level configuration, no replace ref. Each fixture is built in a temporary directory.
// ============================================================================================================================
const fwd = (p) => p.split(path.sep).join("/");
const trueTree = (repo, rev) => spawnSync("git", ["rev-parse", `${rev}^{tree}`], { cwd: repo, encoding: "utf8", env: { ...process.env, GIT_NO_REPLACE_OBJECTS: "1" } }).stdout.trim();

/**
 * A checkout armed against the gate: `variant` names what is planted. The filter writes `marker` and rewrites `hellp` to `hello` (so it
 * would MASK a one-byte tamper); the hook writes `marker` too. Returns { f, marker, env } where env is what the gate process inherits.
 */
function armedCheckout(t, variant) {
  const f = fixture(t, { "Cargo.toml": "[workspace]\n", "a.txt": "hello\n", "lf.txt": "one\ntwo\n", ".gitignore": "target/\n" });
  const work = tmp(t, "rk-snap-armed-");
  const marker = path.join(work, "marker");
  const script = path.join(work, "filter.mjs");
  fs.writeFileSync(script, `import fs from "node:fs";\nfs.appendFileSync(${JSON.stringify(marker)}, "filter ran\\n");\nlet d = "";\nprocess.stdin.setEncoding("utf8");\nprocess.stdin.on("data", (c) => { d += c; });\nprocess.stdin.on("end", () => process.stdout.write(d.replace(/hellp/g, "hello")));\n`);
  const command = `"${fwd(process.execPath)}" "${fwd(script)}"`;
  const cfg = (...a) => git(f.repo, "config", ...a);
  const attributes = (text) => { fs.mkdirSync(path.join(f.repo, ".git", "info"), { recursive: true }); fs.appendFileSync(path.join(f.repo, ".git", "info", "attributes"), text); };
  const env = {};
  if (variant === "filter" || variant === "all") { cfg("filter.evil.clean", command); cfg("filter.evil.smudge", command); cfg("filter.evil.required", "false"); attributes("* filter=evil\n"); }
  if (variant === "autocrlf" || variant === "all") { cfg("core.autocrlf", "true"); attributes("* text eol=crlf\n"); }
  if (variant === "global-attributes" || variant === "all") {
    cfg("filter.evil.clean", command);
    const file = path.join(work, "global-attributes");
    fs.writeFileSync(file, "* filter=evil\n");
    cfg("core.attributesFile", fwd(file));
  }
  if (variant === "user-level" || variant === "all") {
    // The gate's own environment carries a user-level filter and attributes (probe P3b): XDG_CONFIG_HOME / HOME / GIT_CONFIG_GLOBAL.
    const user = path.join(work, "user");
    fs.mkdirSync(path.join(user, "git"), { recursive: true });
    fs.writeFileSync(path.join(user, "git", "config"), `[filter "usr"]\n\tclean = ${command.replace(/\\/g, "\\\\").replace(/"/g, "\\\"")}\n\trequired = false\n`);
    fs.writeFileSync(path.join(user, "git", "attributes"), "* filter=usr\n");
    Object.assign(env, { XDG_CONFIG_HOME: user, HOME: user, GIT_CONFIG_GLOBAL: path.join(user, "git", "config") });
  }
  if (variant === "hook" || variant === "all") {
    const hook = path.join(f.repo, ".git", "hooks", "post-index-change");
    fs.mkdirSync(path.dirname(hook), { recursive: true });
    fs.writeFileSync(hook, `#!/bin/sh\necho hook >> '${fwd(marker)}'\n`, { mode: 0o755 });
    cfg("core.hooksPath", fwd(path.dirname(hook)));
  }
  return { f, marker, env };
}
/** Runs `fn` with `env` merged into this process's environment (the gate process's), then restores it. */
async function withEnv(env, fn) {
  const saved = Object.fromEntries(Object.keys(env).map((k) => [k, process.env[k]]));
  Object.assign(process.env, env);
  try { return await fn(); } finally { for (const [k, v] of Object.entries(saved)) { if (v === undefined) delete process.env[k]; else process.env[k] = v; } }
}
/** The retired invocation (a test double): the checkout's own git directory, the process environment, only fsmonitor and hooks turned off. */
function retiredIdentity(f, dir, baseTree) {
  const index = path.join(tmp(t0, "rk-snap-retired-"), "index");
  const run = (...a) => spawnSync("git", ["-c", "core.fsmonitor=false", "-c", "core.untrackedCache=false", "-c", `core.hooksPath=${os.devNull}`, `--git-dir=${path.join(f.repo, ".git")}`, `--work-tree=${dir}`, ...a], { cwd: dir, env: { ...process.env, GIT_INDEX_FILE: index }, encoding: "utf8" });
  run("read-tree", baseTree); run("add", "-A");
  return run("write-tree").stdout.trim();
}
let t0 = null;

test("D3 sealed identity: no filter driver, eol conversion, attribute file, hook or user-level configuration of the checkout or the gate's environment reaches the id or runs; the exact snapshot verifies and its id is <sha>^{tree}", async (t) => {
  t0 = t;
  for (const variant of ["filter", "autocrlf", "global-attributes", "user-level", "hook", "all", "none"]) {
    const { f, marker, env } = armedCheckout(t, variant);
    const expected = trueTree(f.repo, f.commit);
    await withEnv(env, async () => {
      const r = syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
      assert.equal(r.tree, expected, `${variant}: the id of the synced snapshot is <sha>^{tree}`);
      assert.equal(verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }).tree, expected, variant);
      // The CLI: sync and --verify.
      const out = [], err = [];
      const io = { cwd: f.repo, env: process.env, snapshotRoot: f.root, out: (m) => out.push(m), err: (m) => err.push(m), acquireOwnership: () => ({ handle: "h", joined: false, reclaimed: null, release: () => ({ released: true }) }) };
      assert.equal(await main(["snapshot", "--commit", f.commit], io), 0, `${variant}: ${err.join("\n")}`);
      assert.equal(await main(["snapshot", "--verify", "--commit", f.commit], io), 0, `${variant}: ${err.join("\n")}`);
      assert.equal(fs.existsSync(marker), false, `${variant}: neither the filter nor the hook ever ran`);
      // The masked tamper: the filter would rewrite hellp to hello, hiding a one-byte change; CRLF would be folded to LF by autocrlf.
      if (variant !== "none") {
        fs.writeFileSync(path.join(f.dir, "a.txt"), "hellp\n");
        assert.throws(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e.code === "mismatch", `${variant}: the one-byte tamper is detected`);
        assert.equal(await main(["snapshot", "--verify", "--commit", f.commit], io), 1, `${variant}: CLI exit 1 for the one-byte tamper`);
        fs.writeFileSync(path.join(f.dir, "a.txt"), "hello\n");
        fs.writeFileSync(path.join(f.dir, "lf.txt"), "one\r\ntwo\r\n");
        assert.throws(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e.code === "mismatch", `${variant}: the LF-to-CRLF tamper is detected`);
        assert.equal(await main(["snapshot", "--verify", "--commit", f.commit], io), 1);
        assert.equal(fs.existsSync(marker), false, `${variant}: still nothing ran`);
      } else assert.equal(verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }).tree, expected, "positive control: a clean checkout agrees");
    });
  }
});

test("D3 negative control: the retired invocation over the same armed checkout runs the filter and hides the tamper (so the sealed test can see the defect)", (t) => {
  t0 = t;
  const { f, marker } = armedCheckout(t, "filter");
  const expected = trueTree(f.repo, f.commit);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  assert.equal(fs.existsSync(marker), false, "the sealed sync ran nothing");
  fs.writeFileSync(path.join(f.dir, "a.txt"), "hellp\n");
  const masked = retiredIdentity(f, f.dir, expected);
  assert.equal(fs.existsSync(marker), true, "the retired invocation executed the checkout's filter");
  assert.equal(masked, expected, "and the filter hid the one-byte tamper behind the commit's own id");
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e.code === "mismatch");
});

test("D3 replace refs: `git replace` changes what <sha>^{tree} means for plain git, never for the gate: the sync writes and verifies the true commit's tree", (t) => {
  const f = fixture(t);
  const c1 = f.commit;
  write(f.repo, { "src/lib.rs": "pub fn other() {}\n", "extra.rs": "x\n" });
  const c2 = commitAll(f.repo, "c2");
  git(f.repo, "replace", c1, c2);
  assert.equal(git(f.repo, "rev-parse", `${c1}^{tree}`), git(f.repo, "rev-parse", `${c2}^{tree}`), "plain git reads the replacement's tree");
  const real = trueTree(f.repo, c1);
  assert.notEqual(real, git(f.repo, "rev-parse", `${c2}^{tree}`));
  assert.deepEqual(resolveCommit(f.repo, c1), { commit: c1, tree: real });
  const r = syncSnapshot({ checkout: f.repo, rev: c1, root: f.root });
  assert.equal(r.tree, real);
  assert.equal(fs.readFileSync(path.join(f.dir, "src", "lib.rs"), "utf8"), "pub fn a() {}\n");
  assert.equal(fs.existsSync(path.join(f.dir, "extra.rs")), false);
  assert.equal(verifySnapshot({ checkout: f.repo, rev: c1, root: f.root }).tree, real);
  assert.deepEqual(treeEntries(f.repo, c1).map((e) => e.path).sort(), ["Cargo.toml", ".gitignore", "README.md", "src/lib.rs"].sort());
});

test("D3 committed attributes (probes P3c, P3d): a raw CRLF blob, a raw $Id$ blob and a BOM-less working-tree-encoding blob verify exactly; the $Id$ tamper and the CRLF-to-LF tamper refuse", (t) => {
  const f = fixture(t, { "Cargo.toml": "[workspace]\n", ".gitattributes": "*.txt text eol=lf ident\n*.u16 working-tree-encoding=UTF-16\n", ".gitignore": "target/\n" });
  const raw = { "crlf.txt": "a\r\nb\r\n", "id.txt": "$Id: deadbeef $\n", "x.u16": "abc\n" };
  for (const [name, body] of Object.entries(raw)) git(f.repo, "update-index", "--add", "--cacheinfo", `100644,${blobOf(f.repo, body)},${name}`);
  git(f.repo, "commit", "-q", "-m", "raw blobs");
  const c = git(f.repo, "rev-parse", "HEAD");
  const expected = trueTree(f.repo, c);
  const r = syncSnapshot({ checkout: f.repo, rev: c, root: f.root });
  assert.equal(r.tree, expected);
  for (const [name, body] of Object.entries(raw)) assert.equal(fs.readFileSync(path.join(f.dir, name), "utf8"), body, `${name} holds the blob's raw bytes`);
  assert.equal(verifySnapshot({ checkout: f.repo, rev: c, root: f.root }).tree, expected);
  fs.writeFileSync(path.join(f.dir, "id.txt"), "$Id$\n");
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: c, root: f.root }), (e) => e.code === "mismatch", "the $Id$ tamper");
  fs.writeFileSync(path.join(f.dir, "id.txt"), raw["id.txt"]);
  fs.writeFileSync(path.join(f.dir, "crlf.txt"), "a\nb\n");
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: c, root: f.root }), (e) => e.code === "mismatch", "the CRLF-to-LF tamper");
});

const caseHost = process.platform === "darwin" || process.platform === "win32" ? false : "a case-sensitive host: a case-only rename is two different names";
test("D3 case drift (Mac, Dell): a case-only rename planted in the snapshot refuses; a commit that renames a file and a directory by case only, synced over its parent, converges and verifies", { skip: caseHost }, (t) => {
  const f = fixture(t);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  fs.renameSync(path.join(f.dir, "README.md"), path.join(f.dir, "tmp-case"));
  fs.renameSync(path.join(f.dir, "tmp-case"), path.join(f.dir, "Readme.md"));
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root }), (e) => e.code === "mismatch", "the case drift is part of the id");
  assert.equal(syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root }).commit, f.commit);
  assert.ok(fs.readdirSync(f.dir).includes("README.md") && !fs.readdirSync(f.dir).includes("Readme.md"), "the sync restored the commit's spelling");
  verifySnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  // A commit that renames by case only.
  const readme = git(f.repo, "rev-parse", `${f.commit}:README.md`);
  const lib = git(f.repo, "rev-parse", `${f.commit}:src/lib.rs`);
  git(f.repo, "rm", "-q", "--cached", "README.md", "src/lib.rs");
  git(f.repo, "update-index", "--add", "--cacheinfo", `100644,${readme},Readme.md`);
  git(f.repo, "update-index", "--add", "--cacheinfo", `100644,${lib},Src/lib.rs`);
  git(f.repo, "commit", "-q", "-m", "case-only renames");
  const renamed = git(f.repo, "rev-parse", "HEAD");
  const before = fs.lstatSync(path.join(f.dir, "README.md"), { bigint: true }).mtimeNs;
  const r = syncSnapshot({ checkout: f.repo, rev: renamed, root: f.root });
  assert.ok(r.casesFixed >= 2, `renamed in place: ${r.casesFixed}`);
  assert.ok(fs.readdirSync(f.dir).includes("Readme.md") && fs.readdirSync(f.dir).includes("Src"), "the new spellings are on disk");
  assert.ok(!fs.readdirSync(f.dir).includes("README.md") && !fs.readdirSync(f.dir).includes("src"));
  assert.equal(fs.lstatSync(path.join(f.dir, "Readme.md"), { bigint: true }).mtimeNs, before, "the rename kept the file's mtime");
  assert.equal(verifySnapshot({ checkout: f.repo, rev: renamed, root: f.root }).tree, trueTree(f.repo, renamed));
});

function objectNames(repo) {
  const out = [];
  const walk = (d) => { for (const e of fs.readdirSync(d, { withFileTypes: true })) { const p = path.join(d, e.name); if (e.isDirectory()) walk(p); else out.push(path.relative(path.join(repo, ".git", "objects"), p).split(path.sep).join("/")); } };
  walk(path.join(repo, ".git", "objects"));
  return out.sort();
}
test("D3 / 5.13 the checkout's objects/ listing is unchanged by sync, verify and the identity: every object the sealed git writes lands in its private directory", (t) => {
  const f = fixture(t);
  write(f.repo, { "src/lib.rs": "pub fn b() {}\n" });
  const next = commitAll(f.repo, "next");
  const before = objectNames(f.repo);
  syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root });
  syncSnapshot({ checkout: f.repo, rev: next, root: f.root });
  verifySnapshot({ checkout: f.repo, rev: next, root: f.root });
  write(f.dir, { "src/lib.rs": "tampered, so the identity hashes a blob git has never seen\n" });
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: next, root: f.root }), (e) => e.code === "mismatch");
  assert.deepEqual(objectNames(f.repo), before, "no loose object, pack or directory was added to the checkout");
});

test("D3 the sealed invocation is one exported constant: the -c flags in order, the attributes, the fixed environment; fileMode per platform; no variable but the allowlist reaches git", () => {
  const T = path.join(os.tmpdir(), "T");
  const args = sealedConfigArgs(T, "linux");
  assert.deepEqual(args.filter((a) => a !== "-c"), [
    "core.autocrlf=false", "core.safecrlf=false", "core.eol=lf", "core.fileMode=true", "core.symlinks=true", "core.ignoreCase=false", "core.precomposeUnicode=false",
    `core.attributesFile=${path.join(T, "empty")}`, `core.excludesFile=${path.join(T, "empty")}`, `core.hooksPath=${path.join(T, "hooks")}`,
    "core.fsmonitor=false", "core.untrackedCache=false", "core.splitIndex=false", "core.sparseCheckout=false", "index.sparse=false", "core.useReplaceRefs=false",
  ]);
  assert.ok(sealedConfigArgs(T, "win32").includes("core.fileMode=false"));
  assert.ok(sealedConfigArgs(T, "darwin").includes("core.fileMode=true"));
  assert.equal(SEALED_GIT.attributes, "* -text -ident -filter -working-tree-encoding !eol\n");
  const hostile = { PATH: "/bin", SystemRoot: "C:\\Windows", GIT_EXEC_PATH: "/evil", GIT_ALTERNATE_OBJECT_DIRECTORIES: "/evil", GIT_CONFIG_PARAMETERS: "'x=y'", GIT_CONFIG_COUNT: "1", GIT_ATTR_SOURCE: "x", GIT_TRACE: "1", GIT_REPLACE_REF_BASE: "x", GIT_OBJECT_DIRECTORY: "/evil", HOME: "/evil", EDITOR: "x" };
  const env = sealedEnv(T, { snapshot: "/snap", platform: "linux", base: hostile });
  assert.deepEqual(Object.keys(env).sort(), ["GIT_ATTR_NOSYSTEM", "GIT_CONFIG_GLOBAL", "GIT_CONFIG_NOSYSTEM", "GIT_DIR", "GIT_INDEX_FILE", "GIT_NO_LAZY_FETCH", "GIT_NO_REPLACE_OBJECTS", "GIT_OPTIONAL_LOCKS", "GIT_TERMINAL_PROMPT", "GIT_WORK_TREE", "HOME", "LC_ALL", "PATH", "XDG_CONFIG_HOME"]);
  assert.equal(env.HOME, T);
  assert.equal(env.GIT_DIR, T);
  assert.equal(env.GIT_WORK_TREE, "/snap");
  const win = sealedEnv(T, { snapshot: "/snap", platform: "win32", base: hostile });
  assert.deepEqual([win.SystemRoot, win.TEMP, win.TMP], ["C:\\Windows", T, T]);
  // The private directory's layout.
  const dir = createSealedGitDir({ commonDir: "/x/.git", objectFormat: "sha1" });
  try {
    assert.equal(fs.readFileSync(path.join(dir, "HEAD"), "utf8"), "ref: refs/heads/none\n");
    assert.equal(fs.readFileSync(path.join(dir, "info", "exclude"), "utf8"), "");
    assert.equal(fs.readFileSync(path.join(dir, "info", "attributes"), "utf8"), SEALED_ATTRIBUTES);
    assert.equal(fs.readFileSync(path.join(dir, "objects", "info", "alternates"), "utf8"), "/x/.git/objects\n");
    assert.match(fs.readFileSync(path.join(dir, "config"), "utf8"), /^\[core\]\n\trepositoryformatversion = 0\n\tbare = false\n$/);
    assert.deepEqual(fs.readdirSync(path.join(dir, "hooks")), []);
    if (process.platform !== "win32") assert.equal(fs.statSync(dir).mode & 0o777, 0o700);
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
  const sha256Dir = createSealedGitDir({ commonDir: "/x/.git", objectFormat: "sha256" });
  try { assert.match(fs.readFileSync(path.join(sha256Dir, "config"), "utf8"), /repositoryformatversion = 1\n\tbare = false\n\[extensions\]\n\tobjectFormat = sha256\n/); } finally { fs.rmSync(sha256Dir, { recursive: true, force: true }); }
});
