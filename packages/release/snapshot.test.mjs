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
import { spawnSync } from "./background-process.mjs";
import { SnapshotError, keyPath, repoKey, snapshotCommand, snapshotRoot, snapshotTree, syncSnapshot, verifySnapshot, acquireSnapshotLock } from "./snapshot.mjs";
import { ownershipFile } from "./build-ownership.mjs";
import { main } from "./bin/rightkit-gate.mjs";

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
  const root = tmp(t, "rk-snap-root-");
  return { repo, root, commit, dir: path.join(root, repoKey(keyPath(repo))) };
}
function mtimes(dir) {
  const out = {};
  const walk = (d) => { for (const e of fs.readdirSync(d, { withFileTypes: true })) { const p = path.join(d, e.name); if (e.isDirectory()) walk(p); else out[path.relative(dir, p)] = fs.lstatSync(p, { bigint: true }).mtimeNs; } };
  walk(dir);
  return out;
}

test("§1 root & repo-key: Mac/Dell under the managed data root, Linux XDG state with fallback; key = sha256(canonical path)[:16]", () => {
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
  assert.equal(snapshotRoot({ platform: "linux", env: { XDG_STATE_HOME: "/s" }, home: "/home/a" }), "/s/rightkit/snapshots");
  assert.equal(snapshotRoot({ platform: "linux", env: {}, home: "/home/a" }), "/home/a/.local/state/rightkit/snapshots");
  assert.equal(snapshotRoot({ platform: "linux", env: { XDG_STATE_HOME: "relative" }, home: "/home/a" }), "/home/a/.local/state/rightkit/snapshots");
  assert.equal(snapshotRoot({ platform: "linux", env: { RIGHTKIT_SNAPSHOT_ROOT: "/data/snaps", XDG_STATE_HOME: "/s" }, home: "/home/a" }), "/data/snaps");
});

// Review item5-r1 finding 9 (GATE-CONTRACT §1: the root is durable, "scratch is never the snapshot root because it is disposable").
const refusesRoot = (opts, source) => assert.throws(() => snapshotRoot(opts), (e) => e instanceof SnapshotError && e.code === "error" && e.message.includes(source) && /durable/.test(e.message), JSON.stringify(opts));

test("§1 root durability: a root under a scratch root or a temp directory is refused on every host, naming where the root came from", () => {
  const linux = (env) => ({ platform: "linux", env, home: "/home/a" });
  for (const root of ["/Volumes/D/scratch/snaps", "/Volumes/D/scratch", "/tmp/snaps", "/private/tmp/x/snaps", "/var/tmp/snaps", "/private/var/folders/ab/cd/T/snaps", "/var/folders/ab/cd/T/snaps", ...(process.platform === "win32" ? [] : [path.join(os.tmpdir(), "snaps")])]) {
    refusesRoot(linux({ RIGHTKIT_SNAPSHOT_ROOT: root }), "RIGHTKIT_SNAPSHOT_ROOT");
  }
  refusesRoot(linux({ XDG_STATE_HOME: "/Volumes/D/scratch/state" }), "XDG_STATE_HOME");
  refusesRoot(linux({ XDG_STATE_HOME: "/tmp/state" }), "XDG_STATE_HOME");
  refusesRoot({ platform: "linux", env: {}, home: "/tmp/home" }, "the home directory /tmp/home");
  // A temp directory the host names by environment (TMPDIR / TEMP / TMP) counts too, and a root outside every one of them is fine.
  refusesRoot(linux({ RIGHTKIT_SNAPSHOT_ROOT: "/data/t/snaps", TMPDIR: "/data/t" }), "RIGHTKIT_SNAPSHOT_ROOT");
  assert.equal(snapshotRoot(linux({ RIGHTKIT_SNAPSHOT_ROOT: "/data/snaps", TMPDIR: "/data/t" })), "/data/snaps");
  // A TMPDIR of "/" must not turn every root into a refusal.
  assert.equal(snapshotRoot(linux({ RIGHTKIT_SNAPSHOT_ROOT: "/data/snaps", TMPDIR: "/" })), "/data/snaps");
  // Windows: the Dell's fixed root is fine; the same check reads %TEMP% & friends, case-insensitively, with Windows paths.
  assert.equal(snapshotRoot({ platform: "win32", env: { TEMP: "C:\\Users\\a\\AppData\\Local\\Temp" }, home: "C:\\Users\\a" }), "D:\\.rightkit-managed\\snapshots");
  refusesRoot({ platform: "win32", env: { TEMP: "d:\\.RIGHTKIT-MANAGED" }, home: "C:\\Users\\a" }, "the host's managed root");
});

test("§1 root: no environment variable relocates the Mac/Dell root; the one-time test variable of an earlier round is not read", () => {
  const ignored = { RIGHTKIT_SNAPSHOT_TEST_RUN: "zzz", RIGHTKIT_SNAPSHOT_TEST_ROOT: "/y", RIGHTKIT_SNAPSHOT_ROOT: "/y", RIGHTKIT_DATA_ROOT: "/y", XDG_STATE_HOME: "/y" };
  assert.equal(snapshotRoot({ platform: "darwin", env: ignored, home: "/Users/a" }), "/Volumes/D/.rightkit-managed/snapshots");
  assert.equal(snapshotRoot({ platform: "win32", env: ignored, home: "C:\\Users\\a" }), "D:\\.rightkit-managed\\snapshots");
  // Linux reads only the configured root and XDG state; the test variable changes neither.
  assert.equal(snapshotRoot({ platform: "linux", env: { RIGHTKIT_SNAPSHOT_TEST_RUN: "zzz", XDG_STATE_HOME: "/s" }, home: "/home/a" }), "/s/rightkit/snapshots");
  assert.equal(snapshotRoot({ platform: "linux", env: { RIGHTKIT_SNAPSHOT_TEST_RUN: "zzz", RIGHTKIT_SNAPSHOT_ROOT: "/data/snaps" }, home: "/home/a" }), "/data/snaps");
  // And the owner record, derived from the root, follows the same fixed path.
  const owner = (env) => ownershipFile({ platform: "darwin", env, home: "/Users/a" });
  assert.equal(owner({ RIGHTKIT_SNAPSHOT_TEST_RUN: "zzz" }), owner({}));
  assert.equal(owner({}), "/Volumes/D/.rightkit-managed/build-ownership/owner.json");
});

test("§1 root durability: a sibling named like `..x` is inside scratch or temp, not outside (a path is outside only as `..` or `..<sep>…`)", () => {
  const linux = (root) => ({ platform: "linux", env: { RIGHTKIT_SNAPSHOT_ROOT: root }, home: "/home/a" });
  for (const root of ["/Volumes/D/scratch/..x/s", "/tmp/..x", "/tmp/...", "/private/var/folders/..cache/T/s"]) refusesRoot(linux(root), "RIGHTKIT_SNAPSHOT_ROOT");
  assert.equal(snapshotRoot(linux("/Volumes/D/scratch/../data/s")), "/Volumes/D/scratch/../data/s", "`..` leaves scratch, so a root reached through it is not under it");
  assert.equal(snapshotRoot(linux("/Volumes/D/..scratch/s")), "/Volumes/D/..scratch/s", "a sibling of scratch whose name starts with `..` is not under it");
  assert.equal(snapshotRoot(linux("/data/..x/snaps")), "/data/..x/snaps");
});

test("snapshot CLI: an injected snapshotRoot is the root, and the environment is not consulted for it", (t) => {
  const f = fixture(t);
  const out = [], err = [];
  const env = { ...process.env, RIGHTKIT_SNAPSHOT_ROOT: "/Volumes/D/scratch/never", RIGHTKIT_SNAPSHOT_TEST_RUN: "x/../y" };
  const code = snapshotCommand({ cwd: f.repo, env, platform: process.platform, snapshotRoot: f.root, out: (m) => out.push(m), err: (m) => err.push(m), acquireOwnership: (q) => { assert.equal(q.snapshot, f.dir); assert.equal(q.file, path.join(path.dirname(f.root), "build-ownership", "owner.json")); return { handle: "h", joined: false, reclaimed: null, release: () => ({ released: true }) }; } }, ["--commit", f.commit, "--json"]);
  assert.equal(code, 0, err.join("\n"));
  assert.equal(JSON.parse(out.at(-1)).path, f.dir);
  // Without the injection the root is the host's (Linux: the scratch root named here is refused before anything runs).
  const refused = [];
  assert.equal(snapshotCommand({ cwd: f.repo, env, platform: "linux", out: () => {}, err: (m) => refused.push(m), acquireOwnership: () => { throw new Error("must not acquire"); } }, ["--commit", f.commit]), 2);
  assert.match(refused.join("\n"), /RIGHTKIT_SNAPSHOT_ROOT.*scratch root/);
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
