// Row 5.25 (incident F8): a test process must never create, write, rename or remove anything under the real managed root's
// `snapshots/` or `build-ownership/`. The guard lives in test-support/broker-guard.mjs (every test file imports it); these tests hold
// it: an injected temporary root works normally, a root declared real refuses every mutating call with RK_TEST_REAL_MANAGED_ROOT and
// is left byte-for-byte as it was, and on a Mac or Dell the actual real root is protected the same way.
import { REAL_ROOT_ERROR_CODE, REAL_ROOT_OPT_IN_ENV, protectManagedRoot, realManagedRoots, withRealManagedRoot } from "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import childProcess from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { repoKey, keyPath, syncSnapshot, verifySnapshot } from "./snapshot.mjs";

const named = (e) => e?.code === REAL_ROOT_ERROR_CODE && /RK_TEST_REAL_MANAGED_ROOT/.test(e.message);
const tmp = (t, prefix) => {
  const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
  t.after(() => fs.rmSync(d, { recursive: true, force: true }));
  return d;
};
/** Every entry under `dir` with its type, size and mtime: a refused call leaves this unchanged. */
function listing(dir) {
  const out = {};
  const walk = (d) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      const s = fs.lstatSync(p);
      out[path.relative(dir, p).split(path.sep).join("/")] = `${e.isDirectory() ? "dir" : e.isSymbolicLink() ? "link" : "file"}:${s.size}:${s.mtimeMs}`;
      if (e.isDirectory()) walk(p);
    }
  };
  if (fs.existsSync(dir)) walk(dir);
  return out;
}
function git(cwd, ...args) {
  const r = spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", ...args], { cwd, encoding: "utf8" });
  if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr}`);
  return r.stdout.trim();
}

/** A temporary directory standing in for a real managed root, declared real to the guard, holding one entry in each protected directory. */
function pretendRealRoot(t) {
  // Cleanup order matters: the root stays declared real until it is unprotected, and a recursive removal of it while protected is itself a refused mutation (Node's recursive rmSync removes
  // directories through the patched fs on some platforms and natively on others), so the unprotect is registered BEFORE the removal and runs first.
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-guard-real-")));
  const root = path.join(base, ".rightkit-managed");
  let unprotect = () => {};
  t.after(() => unprotect());
  t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  for (const sub of ["snapshots", "build-ownership"]) {
    fs.mkdirSync(path.join(root, sub, "keep"), { recursive: true });
    fs.writeFileSync(path.join(root, sub, "keep", "bytes.txt"), `${sub} content\n`);
  }
  fs.mkdirSync(path.join(root, "elsewhere"));
  // Private, as a real snapshot root is (row 5.22), so the F8 shape below reaches the guard and not the root-mode refusal.
  if (process.platform !== "win32") for (const sub of ["snapshots", "build-ownership"]) fs.chmodSync(path.join(root, sub), 0o700);
  unprotect = protectManagedRoot(root);
  return { root, snapshots: path.join(root, "snapshots"), owners: path.join(root, "build-ownership"), unprotect };
}

test("5.25 an injected temporary root works normally: sync, verify and plain writes under snapshots/ and build-ownership/ of a temp root are not touched by the guard", (t) => {
  const base = tmp(t, "rk-guard-ok-");
  for (const sub of ["snapshots", "build-ownership"]) {
    const dir = path.join(base, sub, "x");
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, "a"), "a");
    fs.renameSync(path.join(dir, "a"), path.join(dir, "b"));
    fs.rmSync(dir, { recursive: true });
  }
  const repo = tmp(t, "rk-guard-repo-");
  git(repo, "init", "-q", "-b", "main");
  fs.writeFileSync(path.join(repo, "a.txt"), "a\n");
  git(repo, "add", "-A");
  git(repo, "commit", "-q", "-m", "c");
  const commit = git(repo, "rev-parse", "HEAD");
  const root = path.join(tmp(t, "rk-guard-root-"), "snapshots");
  fs.mkdirSync(root, { mode: 0o700 });
  const r = syncSnapshot({ checkout: repo, rev: commit, root });
  assert.equal(r.path, path.join(root, repoKey(keyPath(repo))));
  assert.equal(verifySnapshot({ checkout: repo, rev: commit, root }).commit, commit);
  const child = childProcess.spawnSync(process.execPath, ["-e", "process.exit(0)"], { cwd: r.path });
  assert.equal(child.status, 0, "a child working in an injected root launches");
});

test("5.25 a root declared real refuses every mutating fs call (sync, callback, promise, stream) and child launches into it with RK_TEST_REAL_MANAGED_ROOT, and is left exactly as it was", async (t) => {
  const real = pretendRealRoot(t);
  const before = { snapshots: listing(real.snapshots), owners: listing(real.owners) };
  for (const dir of [real.snapshots, real.owners]) {
    const keep = path.join(dir, "keep");
    const bytes = path.join(keep, "bytes.txt");
    const fresh = path.join(dir, "fresh");
    const sync = {
      "mkdirSync": () => fs.mkdirSync(fresh, { recursive: true }),
      "mkdirSync (nested)": () => fs.mkdirSync(path.join(dir, "a", "b", "c"), { recursive: true }),
      "writeFileSync": () => fs.writeFileSync(fresh, "x"),
      "writeFileSync (over a file)": () => fs.writeFileSync(bytes, "overwritten"),
      "appendFileSync": () => fs.appendFileSync(bytes, "x"),
      "rmSync": () => fs.rmSync(keep, { recursive: true, force: true }),
      "rmSync (the protected directory itself)": () => fs.rmSync(dir, { recursive: true, force: true }),
      "rmdirSync": () => fs.rmdirSync(keep),
      "unlinkSync": () => fs.unlinkSync(bytes),
      "renameSync (out of it)": () => fs.renameSync(bytes, path.join(real.root, "elsewhere", "moved")),
      "renameSync (into it)": () => { fs.writeFileSync(path.join(real.root, "elsewhere", "src"), "x"); fs.renameSync(path.join(real.root, "elsewhere", "src"), fresh); },
      "copyFileSync (to it)": () => fs.copyFileSync(path.join(real.root, "elsewhere", "src"), fresh),
      "linkSync (new name in it)": () => fs.linkSync(path.join(real.root, "elsewhere", "src"), fresh),
      "symlinkSync (link in it)": () => fs.symlinkSync(bytes, fresh),
      "truncateSync": () => fs.truncateSync(bytes, 0),
      "utimesSync": () => fs.utimesSync(bytes, new Date(0), new Date(0)),
      "chmodSync": () => fs.chmodSync(bytes, 0o600),
      "openSync for write": () => fs.openSync(fresh, "w"),
      "openSync for append": () => fs.openSync(bytes, "a"),
      "openSync read-write": () => fs.openSync(bytes, "r+"),
      "openSync O_CREAT": () => fs.openSync(fresh, fs.constants.O_WRONLY | fs.constants.O_CREAT),
      "mkdtempSync": () => fs.mkdtempSync(path.join(dir, "tmp-")),
      "cpSync (to it)": () => fs.cpSync(path.join(real.root, "elsewhere"), path.join(dir, "copy"), { recursive: true }),
      "createWriteStream": () => fs.createWriteStream(fresh),
      "a relative path that resolves into it": () => fs.writeFileSync(path.relative(process.cwd(), fresh), "x"),
      "a file URL": () => fs.writeFileSync(new URL(`file://${process.platform === "win32" ? "/" : ""}${fresh.split(path.sep).join("/")}`), "x"),
      "a spelling with ..": () => fs.writeFileSync(path.join(dir, "keep", "..", "fresh"), "x"),
    };
    // The guard answers before the file system does; the unprotected sibling holds the sources some calls name.
    fs.writeFileSync(path.join(real.root, "elsewhere", "src"), "x");
    for (const [name, call] of Object.entries(sync)) assert.throws(call, named, `${name} under ${path.basename(dir)}`);
    // Callback forms throw before they start.
    assert.throws(() => fs.writeFile(fresh, "x", () => {}), named, "writeFile (callback)");
    assert.throws(() => fs.mkdir(fresh, () => {}), named, "mkdir (callback)");
    assert.throws(() => fs.rm(keep, { recursive: true }, () => {}), named, "rm (callback)");
    assert.throws(() => fs.rename(bytes, path.join(real.root, "elsewhere", "m"), () => {}), named, "rename (callback)");
    assert.throws(() => fs.open(fresh, "w", () => {}), named, "open (callback)");
    // Promise forms reject.
    for (const [name, call] of Object.entries({
      "promises.writeFile": () => fs.promises.writeFile(fresh, "x"),
      "promises.appendFile": () => fs.promises.appendFile(bytes, "x"),
      "promises.mkdir": () => fs.promises.mkdir(fresh, { recursive: true }),
      "promises.rm": () => fs.promises.rm(keep, { recursive: true, force: true }),
      "promises.rename": () => fs.promises.rename(bytes, path.join(real.root, "elsewhere", "m")),
      "promises.unlink": () => fs.promises.unlink(bytes),
      "promises.copyFile": () => fs.promises.copyFile(path.join(real.root, "elsewhere", "src"), fresh),
      "promises.open for write": () => fs.promises.open(fresh, "w"),
      "promises.mkdtemp": () => fs.promises.mkdtemp(path.join(dir, "tmp-")),
    })) await assert.rejects(call, named, name);
    // Children: the working directory and the owner-record variable are the two ways a child reaches the root.
    assert.throws(() => childProcess.spawnSync(process.execPath, ["-e", "0"], { cwd: keep }), named, "spawnSync with cwd in the root");
    assert.throws(() => childProcess.execFileSync(process.execPath, ["-e", "0"], { cwd: dir }), named, "execFileSync with cwd in the root");
    assert.throws(() => childProcess.spawn(process.execPath, ["-e", "0"], { cwd: dir }), named, "spawn with cwd in the root");
    assert.throws(() => childProcess.spawnSync(process.execPath, ["-e", "0"], { env: { ...process.env, RIGHTKIT_BUILD_OWNERSHIP_FILE: path.join(real.owners, "owner.json") } }), named, "spawnSync with the real owner record");
    await assert.rejects(() => new Promise((res, rej) => { try { childProcess.execFile(process.execPath, ["-e", "0"], { cwd: dir }, (e) => (e ? rej(e) : res())); } catch (e) { rej(e); } }), named, "execFile with cwd in the root");
  }
  assert.deepEqual({ snapshots: listing(real.snapshots), owners: listing(real.owners) }, before, "nothing under snapshots/ or build-ownership/ changed");
  assert.equal(fs.readFileSync(path.join(real.snapshots, "keep", "bytes.txt"), "utf8"), "snapshots content\n");
  assert.equal(fs.readFileSync(path.join(real.owners, "keep", "bytes.txt"), "utf8"), "build-ownership content\n");
  // The sync of a snapshot into a root declared real is the F8 shape: it refuses before it writes anything.
  const repo = tmp(t, "rk-guard-f8-");
  git(repo, "init", "-q", "-b", "main");
  fs.writeFileSync(path.join(repo, "a.txt"), "a\n");
  git(repo, "add", "-A");
  git(repo, "commit", "-q", "-m", "c");
  const snapshotsBefore = listing(real.snapshots);
  assert.throws(() => syncSnapshot({ checkout: repo, rev: "HEAD", root: real.snapshots }), named);
  assert.deepEqual(listing(real.snapshots), snapshotsBefore, "the refused sync wrote and removed nothing");
});

test("5.25 what the guard leaves alone: reads, the rest of the managed root, case-variant spellings are refused, and the declared opt-ins (env, withRealManagedRoot) and unregistering allow the write", async (t) => {
  const real = pretendRealRoot(t);
  // Reads are free.
  assert.deepEqual(fs.readdirSync(real.snapshots), ["keep"]);
  assert.equal(fs.readFileSync(path.join(real.snapshots, "keep", "bytes.txt"), "utf8"), "snapshots content\n");
  assert.ok(fs.statSync(real.owners).isDirectory());
  assert.equal(fs.existsSync(path.join(real.snapshots, "nope")), false);
  fs.closeSync(fs.openSync(path.join(real.snapshots, "keep", "bytes.txt"), "r"));
  const handle = await fs.promises.open(path.join(real.snapshots, "keep", "bytes.txt"), "r");
  await handle.close();
  // The rest of the managed root is not protected: a native test keeps its own test-only subdirectory there.
  fs.writeFileSync(path.join(real.root, "elsewhere", "ok.txt"), "ok");
  fs.mkdirSync(path.join(real.root, "rk-test-only-subdirectory"));
  fs.rmSync(path.join(real.root, "rk-test-only-subdirectory"), { recursive: true });
  // A sibling that merely starts with the same letters is not the protected directory.
  fs.mkdirSync(path.join(real.root, "snapshots-extra"));
  // Case variants are the same directory on a Mac or Windows volume, and refused there.
  if (process.platform === "darwin" || process.platform === "win32") assert.throws(() => fs.writeFileSync(path.join(real.root, "SNAPSHOTS", "x"), "x"), named);
  // The declared real-root run: the environment opt-in, and the scoped one (sync and async).
  const wrote = path.join(real.snapshots, "declared.txt");
  const saved = process.env[REAL_ROOT_OPT_IN_ENV];
  t.after(() => { if (saved === undefined) delete process.env[REAL_ROOT_OPT_IN_ENV]; else process.env[REAL_ROOT_OPT_IN_ENV] = saved; });
  process.env[REAL_ROOT_OPT_IN_ENV] = "1";
  fs.writeFileSync(wrote, "declared by the environment");
  fs.rmSync(wrote);
  delete process.env[REAL_ROOT_OPT_IN_ENV];
  assert.throws(() => fs.writeFileSync(wrote, "x"), named, "the opt-in ended with the variable");
  withRealManagedRoot(() => { fs.writeFileSync(wrote, "declared in scope"); fs.rmSync(wrote); });
  await withRealManagedRoot(async () => { await fs.promises.writeFile(wrote, "declared in async scope"); await fs.promises.rm(wrote); });
  assert.throws(() => fs.writeFileSync(wrote, "x"), named, "the scope ended with the function");
  assert.throws(() => withRealManagedRoot(() => { throw new Error("inside"); }), /inside/);
  assert.throws(() => fs.writeFileSync(wrote, "x"), named, "a throwing scope ends too");
  // Unregistering (the test double's undo) lifts the protection.
  real.unprotect();
  fs.writeFileSync(wrote, "after");
  fs.rmSync(wrote);
});

test("5.25 the real roots are the host's fixed managed root (Mac, Dell), never a throwaway root; no real root on Linux", () => {
  assert.deepEqual(realManagedRoots({ platform: "linux", env: {}, home: "/home/a" }), []);
  assert.ok(realManagedRoots({ platform: "darwin", env: {}, home: "/Users/a" }).includes("/Volumes/D/.rightkit-managed"));
  assert.ok(realManagedRoots({ platform: "win32", env: {}, home: "C:\\Users\\a" }).includes("D:\\.rightkit-managed"));
  // A data root the environment names counts when it is real, and never when it is this guard's own throwaway root or a temp directory.
  const viaEnv = realManagedRoots({ platform: "darwin", env: { RIGHTKIT_DATA_ROOT: "/Volumes/E/.rightkit-managed" }, home: "/Users/a" });
  assert.ok(viaEnv.includes("/Volumes/E/.rightkit-managed") && viaEnv.includes("/Volumes/D/.rightkit-managed"));
  const throwaway = path.join(os.tmpdir(), "rightkit-release-test-root-abc");
  assert.deepEqual(realManagedRoots({ platform: "darwin", env: { RIGHTKIT_DATA_ROOT: throwaway, RIGHTKIT_GUARD_DATA_ROOT: throwaway }, home: "/Users/a" }), ["/Volumes/D/.rightkit-managed"]);
  assert.deepEqual(realManagedRoots({ platform: "darwin", env: { RIGHTKIT_DATA_ROOT: path.join(os.tmpdir(), "any") }, home: "/Users/a" }), ["/Volumes/D/.rightkit-managed"]);
});

const realRoot = process.platform === "darwin" ? "/Volumes/D/.rightkit-managed" : process.platform === "win32" ? "D:\\.rightkit-managed" : null;
test("5.25 native (Mac, Dell): the actual real managed root's snapshots/ and build-ownership/ refuse every probe with RK_TEST_REAL_MANAGED_ROOT and their listings are unchanged", { skip: realRoot && fs.existsSync(realRoot) ? false : "the host has no real managed root (a Mac or Dell with the managed volume)" }, async (t) => {
  const probe = `rk-guard-probe-${process.pid}-${Date.now()}`;
  const subs = ["snapshots", "build-ownership"].map((n) => path.join(realRoot, n)).filter((d) => fs.existsSync(d));
  const before = subs.map((d) => [d, listing(d)]);
  // If the guard were broken a probe would exist: it is removed (declared, and only a name this test made).
  t.after(() => withRealManagedRoot(() => { for (const d of ["snapshots", "build-ownership"]) fs.rmSync(path.join(realRoot, d, probe), { recursive: true, force: true }); }));
  for (const sub of ["snapshots", "build-ownership"]) {
    const target = path.join(realRoot, sub, probe);
    assert.throws(() => fs.mkdirSync(target), named, `mkdirSync ${sub}`);
    assert.throws(() => fs.mkdirSync(target, { recursive: true }), named, `mkdirSync recursive ${sub}`);
    assert.throws(() => fs.writeFileSync(target, "x"), named, `writeFileSync ${sub}`);
    assert.throws(() => fs.appendFileSync(target, "x"), named, `appendFileSync ${sub}`);
    assert.throws(() => fs.renameSync(path.join(os.tmpdir(), probe), target), named, `renameSync into ${sub}`);
    assert.throws(() => fs.rmSync(target, { recursive: true, force: true }), named, `rmSync ${sub}`);
    assert.throws(() => fs.openSync(target, "w"), named, `openSync ${sub}`);
    await assert.rejects(() => fs.promises.writeFile(target, "x"), named, `promises.writeFile ${sub}`);
    await assert.rejects(() => fs.promises.mkdir(target), named, `promises.mkdir ${sub}`);
    assert.throws(() => childProcess.spawnSync(process.execPath, ["-e", "0"], { cwd: path.join(realRoot, sub) }), named, `a child with cwd in ${sub}`);
  }
  assert.throws(() => childProcess.spawnSync(process.execPath, ["-e", "0"], { env: { ...process.env, RIGHTKIT_BUILD_OWNERSHIP_FILE: path.join(realRoot, "build-ownership", "owner.json") } }), named, "a child with the real owner record");
  assert.deepEqual(subs.map((d) => [d, listing(d)]), before, "the real snapshots/ and build-ownership/ are byte-for-byte as they were");
  assert.ok(fs.existsSync(realRoot));
});
