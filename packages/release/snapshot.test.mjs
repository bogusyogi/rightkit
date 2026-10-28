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
import { SnapshotError, keyPath, repoKey, snapshotRoot, snapshotTree, syncSnapshot, verifySnapshot, acquireSnapshotLock } from "./snapshot.mjs";
import { main } from "./bin/rightkit-gate.mjs";

const posixOnly = process.platform === "win32" ? "POSIX exec bits and links" : false;
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
  assert.equal(snapshotRoot({ platform: "darwin", env: { RIGHTKIT_DATA_ROOT: "/Volumes/E/.rightkit-managed" }, home: "/Users/a" }), "/Volumes/E/.rightkit-managed/snapshots");
  assert.equal(snapshotRoot({ platform: "win32", env: {}, home: "C:\\Users\\a" }), "D:\\.rightkit-managed\\snapshots");
  assert.equal(snapshotRoot({ platform: "linux", env: { XDG_STATE_HOME: "/s" }, home: "/home/a" }), "/s/rightkit/snapshots");
  assert.equal(snapshotRoot({ platform: "linux", env: {}, home: "/home/a" }), "/home/a/.local/state/rightkit/snapshots");
  assert.equal(snapshotRoot({ platform: "linux", env: { XDG_STATE_HOME: "relative" }, home: "/home/a" }), "/home/a/.local/state/rightkit/snapshots");
  assert.equal(snapshotRoot({ platform: "linux", env: { RIGHTKIT_SNAPSHOT_ROOT: "/data/snaps", XDG_STATE_HOME: "/s" }, home: "/home/a" }), "/data/snaps");
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

test("§1 interrupted sync: paths recorded as pending are pruned by the next sync", (t) => {
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

test("E6 modes: a changed exec bit refuses; a mode-only commit fixes the mode without rewriting bytes", { skip: posixOnly }, (t) => {
  const f = fixture(t);
  write(f.repo, { "run.sh": "#!/bin/sh\n" });
  const c1 = commitAll(f.repo);
  syncSnapshot({ checkout: f.repo, rev: c1, root: f.root });
  fs.chmodSync(path.join(f.dir, "run.sh"), 0o755);
  assert.throws(() => verifySnapshot({ checkout: f.repo, rev: c1, root: f.root }), (e) => e.code === "mismatch");
  syncSnapshot({ checkout: f.repo, rev: c1, root: f.root });
  verifySnapshot({ checkout: f.repo, rev: c1, root: f.root });
  const before = fs.lstatSync(path.join(f.dir, "run.sh"), { bigint: true }).mtimeNs;
  fs.chmodSync(path.join(f.repo, "run.sh"), 0o755);
  const c2 = commitAll(f.repo);
  const r = syncSnapshot({ checkout: f.repo, rev: c2, root: f.root });
  assert.deepEqual([r.written, r.modesFixed], [0, 1]);
  assert.equal(fs.lstatSync(path.join(f.dir, "run.sh"), { bigint: true }).mtimeNs, before);
  assert.equal(fs.statSync(path.join(f.dir, "run.sh")).mode & 0o111, 0o111);
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

test("§1 committed links: a relative link inside the snapshot is kept as a link; one out of the snapshot refuses before writing", { skip: posixOnly }, (t) => {
  const f = fixture(t);
  fs.symlinkSync("lib.rs", path.join(f.repo, "src", "alias.rs"));
  const inside = commitAll(f.repo);
  syncSnapshot({ checkout: f.repo, rev: inside, root: f.root });
  assert.equal(fs.readlinkSync(path.join(f.dir, "src", "alias.rs")), "lib.rs");
  verifySnapshot({ checkout: f.repo, rev: inside, root: f.root });
  fs.symlinkSync("../../outside", path.join(f.repo, "src", "escape.rs"));
  const outside = commitAll(f.repo);
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
  const env = { ...process.env, RIGHTKIT_DATA_ROOT: path.dirname(f.root), RIGHTKIT_SNAPSHOT_ROOT: f.root };
  const root = snapshotRoot({ env });
  const out = [], err = [];
  const io = { cwd: f.repo, env, out: (m) => out.push(m), err: (m) => err.push(m) };
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

test("Oracle #1: a committed link chain that escapes physically (c/b -> .., a -> c/b/…/../<checkout>/secret) refuses and is not left behind", { skip: posixOnly }, (t) => {
  const f = fixture(t);
  write(f.repo, { "secret.txt": "uncommitted checkout content\n" });
  const hops = 12;
  git(f.repo, "update-index", "--add", "--cacheinfo", `120000,${blobOf(f.repo, "..")},c/b`);
  git(f.repo, "update-index", "--add", "--cacheinfo", `120000,${blobOf(f.repo, "c/b/".repeat(hops) + "../".repeat(2 * hops) + f.repo.slice(1) + "/secret.txt")},a`);
  git(f.repo, "commit", "-q", "-m", "chain");
  const commit = git(f.repo, "rev-parse", "HEAD");
  assert.throws(() => syncSnapshot({ checkout: f.repo, rev: commit, root: f.root }), (e) => e.code === "refused" && /\ba\b/.test(e.message));
  assert.equal(fs.lstatSync(path.join(f.dir, "a"), { throwIfNoEntry: false }), undefined, "the escaping link is removed");
});

test("Oracle #1: a planted link escaping through a committed link chain is removed", { skip: posixOnly }, (t) => {
  const f = fixture(t);
  git(f.repo, "update-index", "--add", "--cacheinfo", `120000,${blobOf(f.repo, "..")},c/b`);
  git(f.repo, "commit", "-q", "-m", "up-link");
  const commit = git(f.repo, "rev-parse", "HEAD");
  syncSnapshot({ checkout: f.repo, rev: commit, root: f.root });
  fs.mkdirSync(path.join(f.repo, "node_modules"), { recursive: true });
  fs.symlinkSync("c/b/".repeat(12) + "../".repeat(24) + f.repo.slice(1) + "/node_modules", path.join(f.dir, "node_modules"));
  const r = syncSnapshot({ checkout: f.repo, rev: commit, root: f.root });
  assert.equal(r.linksRemoved, 1);
  assert.equal(fs.lstatSync(path.join(f.dir, "node_modules"), { throwIfNoEntry: false }), undefined);
});

test("Oracle #2: paths differing only in case refuse before anything is written; the checkout is never written", { skip: caseInsensitiveHost }, (t) => {
  const f = fixture(t);
  write(f.repo, { "victim/x.txt": "CHECKOUT ORIGINAL\n" });
  git(f.repo, "update-index", "--add", "--cacheinfo", `120000,${blobOf(f.repo, "c/b/".repeat(12) + "../".repeat(24) + f.repo.slice(1) + "/victim")},Foo`);
  git(f.repo, "update-index", "--add", "--cacheinfo", `100644,${blobOf(f.repo, "WRITTEN BY SNAPSHOT SYNC\n")},foo/x.txt`);
  git(f.repo, "commit", "-q", "-m", "case collision");
  const commit = git(f.repo, "rev-parse", "HEAD");
  assert.throws(() => syncSnapshot({ checkout: f.repo, rev: commit, root: f.root }), (e) => e.code === "refused" && /differ only in case/.test(e.message));
  assert.equal(fs.existsSync(f.dir), false);
  assert.equal(fs.readFileSync(path.join(f.repo, "victim/x.txt"), "utf8"), "CHECKOUT ORIGINAL\n");
});

test("a dangling committed link refuses and is removed", { skip: posixOnly }, (t) => {
  const f = fixture(t);
  fs.symlinkSync("missing.rs", path.join(f.repo, "src", "dangling.rs"));
  const commit = commitAll(f.repo);
  assert.throws(() => syncSnapshot({ checkout: f.repo, rev: commit, root: f.root }), (e) => e.code === "refused" && /dangling\.rs/.test(e.message));
  assert.equal(fs.lstatSync(path.join(f.dir, "src", "dangling.rs"), { throwIfNoEntry: false }), undefined);
});
