// The snapshot lock, the one exclusion of every route that syncs or reads a snapshot (reset plan sections 3 and 4; it replaces the per-repository
// build ownership): taken before the sync, held through the batch, joined by a nested gate, reclaimed only on the stale-owner rule.
// Real git, real files, no broker: the seam body's broker proof is a `judge` function, the rest is the production code.
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { SnapshotError, WINDOWS_JOB_SCHEMA, acquireSnapshotLock, containLockHolder, liveLockChildren, queryJobHolder, lockView, recordLockChild, windowsJobLive, repositoryOf, snapshotCommand, syncSnapshot, takeSnapshotLock } from "./snapshot.mjs";
import { spawn as spawnAsync } from "node:child_process";

const GIT_ENV = { GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t", GIT_CONFIG_GLOBAL: os.devNull, GIT_CONFIG_NOSYSTEM: "1" };

/** One committed file in a throwaway repository, and a snapshot root beside it (managed root: none, so no volume rule applies). */
function fixture() {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "rk-snaplock-"));
  const repo = path.join(base, "repo");
  fs.mkdirSync(repo);
  const git = (...args) => {
    const r = spawnSync("git", args, { cwd: repo, encoding: "utf8", env: { ...process.env, ...GIT_ENV } });
    assert.equal(r.status, 0, r.stderr);
    return r.stdout.trim();
  };
  git("init", "-q");
  fs.writeFileSync(path.join(repo, "a.txt"), "a\n");
  git("add", "a.txt");
  git("-c", "commit.gpgsign=false", "commit", "-q", "-m", "one");
  const root = path.join(base, "snapshots");
  const checkout = repositoryOf(repo).checkout;
  const lockFile = path.join(root, `${repositoryOf(repo).key}.lock`);
  return { base, repo, root, checkout, lockFile, commit: git("rev-parse", "HEAD") };
}
const take = (f, lock = {}) => takeSnapshotLock({ checkout: f.repo, root: f.root, managedRoot: null, lock });
const sync = (f, lock = {}) => syncSnapshot({ checkout: f.repo, rev: f.commit, root: f.root, managedRoot: null, lock });

/** A pid no process has: kill(pid, 0) answers ESRCH. */
function deadPid() {
  let pid = 2 ** 22 + 7;
  for (;;) { try { process.kill(pid, 0); pid += 1; } catch (e) { if (e.code === "ESRCH") return pid; pid += 1; } }
}
const seamIdentity = (pid, start = "x") => `${process.platform === "win32" ? "win32" : process.platform}:${pid}:${start}`;
const writeLock = (f, body) => { fs.mkdirSync(f.root, { recursive: true, mode: 0o700 }); fs.writeFileSync(f.lockFile, JSON.stringify(body)); };
const busy = (fn, pattern) => assert.throws(fn, (e) => e instanceof SnapshotError && e.code === "busy" && pattern.test(e.message), `expected a busy refusal matching ${pattern}`);

test("the legacy lock (no broker on the host) is the one lock file, with its token, and is released by its taker only", () => {
  const f = fixture();
  const release = take(f);
  assert.equal(release.lockFile, f.lockFile);
  const body = JSON.parse(fs.readFileSync(f.lockFile, "utf8"));
  assert.equal(body.pid, process.pid); assert.equal(body.host, os.hostname()); assert.equal(body.token, release.token); assert.ok(body.startedAt);
  assert.equal(release.held(), true);
  busy(() => take(f), /held by pid/);
  release();
  assert.equal(fs.existsSync(f.lockFile), false);
  assert.equal(release.held(), false);
  release(); // a second release removes nothing and does not throw
  const again = take(f); assert.notEqual(again.token, release.token); again();
});

test("a legacy lock whose holder is proven gone is reclaimed by the next taker, which names the dead owner and the proof", () => {
  const f = fixture();
  const dead = deadPid();
  writeLock(f, { pid: dead, host: os.hostname(), startedAt: "2026-11-09T00:00:00.000Z", token: "old" });
  const release = take(f);
  assert.deepEqual(Object.keys(release.reclaimed).sort(), ["from", "proof"]);
  assert.equal(release.reclaimed.from, `pid ${dead}`); assert.match(release.reclaimed.proof, /is gone/);
  assert.equal(JSON.parse(fs.readFileSync(f.lockFile, "utf8")).token, release.token);
  assert.equal(fs.existsSync(`${f.lockFile}.reclaim`), false, "the reclaim reservation never outlives the reclaim");
  release();
  const fresh = take(f); assert.equal(fresh.reclaimed, undefined, "a free snapshot reclaims nothing"); fresh();
});

test("a legacy lock is never moved when its holder is live, another host's, unreadable or being reclaimed", () => {
  const f = fixture();
  const original = (body) => { writeLock(f, body); return fs.readFileSync(f.lockFile, "utf8"); };
  let text = original({ pid: process.pid, host: os.hostname(), startedAt: "t", token: "live" });
  busy(() => take(f), /held by pid/); assert.equal(fs.readFileSync(f.lockFile, "utf8"), text);
  text = original({ pid: deadPid(), host: `${os.hostname()}-elsewhere`, startedAt: "t", token: "other" });
  busy(() => take(f), /on .*-elsewhere/); assert.equal(fs.readFileSync(f.lockFile, "utf8"), text);
  fs.writeFileSync(f.lockFile, "not json"); busy(() => take(f), /cannot be read as a lock/); assert.equal(fs.readFileSync(f.lockFile, "utf8"), "not json");
  text = original({ pid: -3, host: os.hostname(), token: "bad" }); busy(() => take(f), /cannot be read as a lock/);
  text = original({ pid: deadPid(), host: os.hostname(), startedAt: "t", token: "stale" });
  fs.writeFileSync(`${f.lockFile}.reclaim`, "someone else");
  busy(() => take(f), /being reclaimed by another process/); assert.equal(fs.readFileSync(f.lockFile, "utf8"), text);
});

test("a legacy lock that names its holder's birth identity tells a reused pid from the holder; an unreadable identity proves nothing", () => {
  const f = fixture();
  const identityOf = (answer) => (pid) => (pid === process.pid ? answer : null);
  const stale = { pid: process.pid, host: os.hostname(), startedAt: "t", token: "was-mine", birth: "x:birth-A" };
  writeLock(f, stale);
  busy(() => take(f, { identityOf: identityOf("x:birth-A") }), /held by pid/);
  busy(() => take(f, { identityOf: identityOf(null) }), /held by pid/);
  const reused = take(f, { identityOf: identityOf("x:birth-B") });
  assert.equal(reused.reclaimed.from, `pid ${process.pid}`);
  assert.equal(JSON.parse(fs.readFileSync(f.lockFile, "utf8")).birth, "x:birth-B", "the new lock carries this process's own birth identity");
  reused();
});

test("the seam body is written whole and a dead holder's lock is reclaimed only on the broker's proof", () => {
  const f = fixture();
  const dead = deadPid();
  const body = { pid: process.pid, identity: seamIdentity(process.pid), commit: f.commit, token: "mine" };
  const old = { pid: dead, identity: seamIdentity(dead, "gone"), commit: f.commit, token: "old" };
  const mine = take(f, { body });
  assert.deepEqual(JSON.parse(fs.readFileSync(f.lockFile, "utf8")), body); mine();
  writeLock(f, old);
  busy(() => take(f, { body }), /reclaimed only on the broker's proof/);
  busy(() => take(f, { body, judge: () => false }), /gave no such proof/);
  assert.deepEqual(JSON.parse(fs.readFileSync(f.lockFile, "utf8")), old, "no proof, no change");
  let asked = null;
  const reclaimed = take(f, { body, judge: (holder) => { asked = holder; return true; } });
  assert.deepEqual(asked, old);
  assert.equal(reclaimed.reclaimed.from, old.identity); assert.match(reclaimed.reclaimed.proof, /the broker showed the snapshot idle/);
  assert.deepEqual(JSON.parse(fs.readFileSync(f.lockFile, "utf8")), body);
  reclaimed();
});

test("a live seam holder is never reclaimed, whatever the broker says", () => {
  const f = fixture();
  const live = { pid: process.pid, identity: seamIdentity(process.pid, "birth-A"), commit: f.commit, token: "live" };
  writeLock(f, live);
  busy(() => take(f, { body: { ...live, token: "next" }, judge: () => true, identityOf: () => seamIdentity(process.pid, "birth-A") }), /is held by/);
  assert.equal(JSON.parse(fs.readFileSync(f.lockFile, "utf8")).token, "live");
});

test("the sync joins the lock its caller took: it re-checks it, never releases it, and refuses another snapshot's lock or one that is not held", () => {
  const f = fixture();
  const release = take(f);
  const first = sync(f, { held: lockView(release) });
  assert.equal(first.commit, f.commit); assert.equal(first.release, undefined, "a sync hands back no release");
  assert.equal(fs.readFileSync(path.join(first.path, "a.txt"), "utf8"), "a\n");
  assert.equal(release.held(), true, "the sync did not release the lock it joined");
  const second = sync(f, { held: lockView(release) });
  assert.equal(second.written, 0, "the second sync of an unchanged commit writes nothing");
  assert.equal(release.held(), true);
  assert.throws(() => sync(f, { held: { lockFile: path.join(f.root, "other.lock"), held: () => true } }), (e) => e.code === "error" && /is not the lock of/.test(e.message));
  assert.throws(() => sync(f, { held: { lockFile: f.lockFile } }), (e) => e.code === "error");
  release();
  assert.throws(() => sync(f, { held: { lockFile: f.lockFile, held: () => false } }), (e) => e.code === "busy" && /is not held by this process/.test(e.message));
});

test("a sync that takes its own lock holds it for the sync alone, and a refused sync leaves no lock", () => {
  const f = fixture();
  const plain = sync(f);
  assert.equal(plain.release, undefined); assert.equal(fs.existsSync(f.lockFile), false);
  const other = take(f);
  assert.throws(() => sync(f), (e) => e.code === "busy" && /held by pid/.test(e.message));
  assert.equal(other.held(), true, "a refused sync never touches the lock of its holder");
  other();
  assert.throws(() => syncSnapshot({ checkout: f.repo, rev: "0".repeat(40), root: f.root, managedRoot: null }));
  assert.equal(fs.existsSync(f.lockFile), false, "a refused sync (unknown revision) leaves no lock behind");
});

test("what a nested gate is handed can show that the lock is held and cannot release it", () => {
  const f = fixture();
  const release = take(f);
  const view = lockView(release);
  assert.deepEqual(Object.keys(view).sort(), ["held", "joined", "lockFile", "token"]);
  assert.equal(view.token, release.token); assert.equal(view.lockFile, f.lockFile); assert.equal(view.joined, true); assert.equal(view.held(), true);
  assert.equal(view.release, undefined);
  release();
  assert.equal(view.held(), false, "the view follows the lock");
});

test("`snapshot` syncs under the lock its host route names, refuses when the route refuses, and is busy while another process holds the snapshot", () => {
  const f = fixture();
  const lines = []; const errors = [];
  const o = { cwd: f.repo, snapshotRoot: f.root, managedRoot: null, env: process.env, platform: process.platform, out: (l) => lines.push(l), err: (l) => errors.push(l) };
  let asked = null;
  const body = { pid: process.pid, identity: seamIdentity(process.pid), commit: f.commit, token: "seam-token" };
  assert.equal(snapshotCommand(o, ["--commit", f.commit], { lockFor: (q) => { asked = q; return { lock: { body } }; } }), 0, errors.join("\n"));
  assert.equal(asked.commit, f.commit); assert.equal(asked.root, f.root); assert.equal(path.resolve(asked.checkout), path.resolve(f.repo));
  assert.match(lines.join("\n"), /snapshot .* commit=[0-9a-f]{40} tree=[0-9a-f]{40} written=\d+/);
  assert.equal(fs.existsSync(f.lockFile), false, "the lock covers the sync alone");
  // The route refuses: nothing is synced.
  const refusing = []; fs.rmSync(path.join(f.root, repositoryOf(f.repo).key), { recursive: true, force: true });
  assert.equal(snapshotCommand({ ...o, err: (l) => refusing.push(l) }, [], { lockFor: () => ({ refusal: "rightkit-gate: BLOCKED: RK_SEAM_IDENTITY: no identity; nothing ran" }) }), 1);
  assert.deepEqual(refusing, ["rightkit-gate: BLOCKED: RK_SEAM_IDENTITY: no identity; nothing ran"]);
  assert.equal(fs.existsSync(path.join(f.root, repositoryOf(f.repo).key)), false);
  // Another process holds the snapshot: busy, exit 1, and the holder's lock is untouched.
  const holder = take(f);
  const blocked = []; const before = fs.readFileSync(f.lockFile, "utf8");
  assert.equal(snapshotCommand({ ...o, err: (l) => blocked.push(l) }, ["--commit", f.commit]), 1);
  assert.match(blocked.join("\n"), /snapshot busy: .*held by pid/);
  assert.equal(fs.readFileSync(f.lockFile, "utf8"), before);
  holder();
  // A verify takes no lock at all.
  assert.equal(snapshotCommand(o, ["--commit", f.commit]), 0);
  const verified = []; const hold2 = take(f);
  assert.equal(snapshotCommand({ ...o, out: (l) => verified.push(l) }, ["--commit", f.commit, "--verify"], { lockFor: () => assert.fail("a verify takes no lock") }), 0);
  assert.match(verified.join("\n"), /verified/); hold2();
});

test("a dead holder's lock is not reclaimed while a process group it recorded still runs; it is once the group ends (gate-owned containment outside the broker)", { skip: process.platform === "win32" }, async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-lock-children-"));
  const lockFile = path.join(dir, "repo.lock");
  // A holder that is gone: a pid that does not exist on this host, legacy body.
  let dead = 4_000_000; while (dead > 3_000_000) { try { process.kill(dead, 0); dead -= 1; } catch (e) { if (e.code === "ESRCH") break; dead -= 1; } }
  fs.writeFileSync(lockFile, JSON.stringify({ pid: dead, host: os.hostname(), startedAt: new Date().toISOString(), token: "old" }));
  const child = spawnAsync("sleep", ["30"], { detached: true, stdio: "ignore" });
  try {
    fs.mkdirSync(`${lockFile}.children`); fs.writeFileSync(path.join(`${lockFile}.children`, `group-${child.pid}.json`), JSON.stringify({ group: child.pid }));
    assert.deepEqual(liveLockChildren(lockFile), [child.pid]);
    assert.throws(() => acquireSnapshotLock(lockFile), (e) => e instanceof SnapshotError && /process group\(s\) it started still run/.test(e.message));
    assert.ok(fs.existsSync(lockFile), "the lock stays");
  } finally { try { process.kill(-child.pid, "SIGKILL"); } catch { /* gone */ } }
  await new Promise((r) => child.once("exit", r));
  for (let i = 0; i < 50 && liveLockChildren(lockFile).length; i += 1) await new Promise((r) => setTimeout(r, 20));
  const mine = acquireSnapshotLock(lockFile);
  assert.ok(mine.reclaimed, "reclaimed once the group ended"); assert.equal(fs.existsSync(`${lockFile}.children`), false, "the dead records go with the reclaim");
  mine();
});

test("a recorded launcher that exits while a descendant survives in its group keeps its record and the lock (regression: the record went with the root's exit)", { skip: process.platform === "win32" }, async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-lock-launcher-"));
  const lockFile = path.join(dir, "repo.lock");
  const release = acquireSnapshotLock(lockFile);
  // The launcher backgrounds a sleep with its own stdio and exits at once; the sleep stays in the launcher's process group.
  const launcher = recordLockChild(spawnAsync("/bin/sh", ["-c", "sleep 30 </dev/null >/dev/null 2>&1 & exit 0"], { detached: true, stdio: "ignore" }));
  const record = path.join(`${lockFile}.children`, `group-${launcher.pid}.json`);
  try {
    await new Promise((r) => launcher.once("exit", r));
    await new Promise((r) => setImmediate(r));
    assert.ok(fs.existsSync(record), "the record outlives the launcher while its group runs");
    assert.deepEqual(liveLockChildren(lockFile), [launcher.pid]);
    release();
    assert.ok(fs.existsSync(lockFile), "release keeps the lock while the survivor runs");
  } finally { try { process.kill(-launcher.pid, "SIGKILL"); } catch { /* gone */ } }
  for (let i = 0; i < 100 && liveLockChildren(lockFile).length; i += 1) await new Promise((r) => setTimeout(r, 20));
  assert.deepEqual(liveLockChildren(lockFile), [], "the group is gone");
  release();
  assert.equal(fs.existsSync(lockFile), false, "released once the group ended");
});

test("an unreadable child-record directory counts as a live group: a dead holder's lock is not reclaimed (only ENOENT means no records)", { skip: process.platform === "win32" || process.getuid?.() === 0 }, () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-lock-unreadable-"));
  const lockFile = path.join(dir, "repo.lock");
  assert.deepEqual(liveLockChildren(lockFile), [], "no record directory: no records");
  let dead = 4_000_000; while (dead > 3_000_000) { try { process.kill(dead, 0); dead -= 1; } catch (e) { if (e.code === "ESRCH") break; dead -= 1; } }
  fs.writeFileSync(lockFile, JSON.stringify({ pid: dead, host: os.hostname(), startedAt: new Date().toISOString(), token: "old" }));
  const records = `${lockFile}.children`;
  fs.mkdirSync(records); fs.chmodSync(records, 0o000);
  try {
    const live = liveLockChildren(lockFile);
    assert.equal(live.length, 1); assert.match(String(live[0]), /^unreadable \(EACCES\)$/);
    assert.throws(() => acquireSnapshotLock(lockFile), (e) => e instanceof SnapshotError && e.code === "busy" && /still run \(unreadable/.test(e.message));
    assert.ok(fs.existsSync(lockFile), "the lock stays");
    assert.match(String(liveLockChildren(lockFile, { readdir: () => { throw Object.assign(new Error("io"), { code: "EIO" }); } })[0]), /unreadable \(EIO\)/);
  } finally { fs.chmodSync(records, 0o700); }
});

// Windows: the holder's tree in a named job (the Windows behaviour itself runs only on the Dell; these pin the logic around it with the OS calls faked).
const fakeWindowsOps = ({ state = "holding", why = "", check = "IN" } = {}) => ({
  startHolder(script) { const file = script.slice(0, -".ps1".length); assert.ok(fs.existsSync(script), "the holder script is written before the launch"); fs.writeFileSync(file, JSON.stringify({ schema: WINDOWS_JOB_SCHEMA, state, why, job: "j" })); },
  selfCheck: () => check, sleep: () => {},
});

test("Windows: the new holder is put in its lock's named job before work runs; a holder that cannot contain it refuses the lock and leaves none", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-lock-win-"));
  const lockFile = path.join(dir, "repo.lock");
  const release = acquireSnapshotLock(lockFile, {}, { platform: "win32", windowsOps: fakeWindowsOps() });
  const record = JSON.parse(fs.readFileSync(path.join(`${lockFile}.children`, "job.json"), "utf8"));
  assert.equal(record.contained, true); assert.match(record.job, /^Local\\RightKitSnapshotLock-/);
  assert.equal(fs.existsSync(path.join(`${lockFile}.children`, "job-holder.json.ps1")), false, "the script file goes once the holder reported");
  release();
  const other = path.join(dir, "other.lock");
  assert.throws(() => acquireSnapshotLock(other, {}, { platform: "win32", windowsOps: fakeWindowsOps({ state: "failed", why: "AssignProcessToJobObject failed (5)" }) }), (e) => e instanceof SnapshotError && /could not contain the gate: AssignProcessToJobObject failed/.test(e.message));
  assert.equal(fs.existsSync(other), false, "no lock is kept without containment");
  const third = path.join(dir, "third.lock");
  assert.throws(() => acquireSnapshotLock(third, {}, { platform: "win32", windowsOps: fakeWindowsOps({ check: "OUT" }) }), (e) => e instanceof SnapshotError && /not contained by its job .*self-check OUT.*refused before any work ran/.test(e.message));
  assert.equal(fs.existsSync(third), false, "an uncontained gate keeps no lock");
});

test("Windows: what still runs in a lock's job: a fresh count from the holder; drained or no record is none; uncontained (at release too) or unanswered is live", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-lock-winlive-"));
  const lockFile = path.join(dir, "repo.lock");
  assert.deepEqual(windowsJobLive(lockFile), [], "no job record: no job was made for this lock");
  containLockHolder(lockFile, "tok", { ops: fakeWindowsOps() });
  const at = (active) => ({ query: () => ({ active }) });
  assert.equal(windowsJobLive(lockFile, at(1)).length, 1, "a dead holder's job with a survivor is live");
  assert.deepEqual(windowsJobLive(lockFile, at(0)), []);
  assert.deepEqual(windowsJobLive(lockFile, { self: 1, ...at(1) }), [], "at its own release the holder is the one process");
  assert.equal(windowsJobLive(lockFile, { self: 1, ...at(2) }).length, 1, "a child still running keeps the lock at release");
  assert.match(windowsJobLive(lockFile, { query: () => { throw new Error("no answer"); } })[0], /unknown \(no answer\)/);
  fs.writeFileSync(path.join(`${lockFile}.children`, "job-holder.json"), JSON.stringify({ schema: WINDOWS_JOB_SCHEMA, state: "drained" }));
  assert.deepEqual(windowsJobLive(lockFile, { query: () => assert.fail("a drained job needs no query") }), []);
  const out = path.join(dir, "out.lock"); containLockHolder(out, "o", { ops: fakeWindowsOps() });
  fs.writeFileSync(path.join(`${out}.children`, "job.json"), JSON.stringify({ schema: WINDOWS_JOB_SCHEMA, job: "j", contained: false }));
  assert.match(windowsJobLive(out, at(0))[0], /did not contain/); assert.match(windowsJobLive(out, { self: 1, ...at(1) })[0], /did not contain/, "an uncontained record never authorizes the holder's own release");
  fs.writeFileSync(path.join(`${out}.children`, "job.json"), "{");
  assert.equal(liveLockChildren(out, { platform: "win32" }).length, 1, "an unreadable record is live");
});

test("Windows: a fresh count is the holder's answer to this query's own nonce; a stale ack or no answer is never a count", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-lock-winq-"));
  const file = path.join(dir, "job-holder.json");
  fs.writeFileSync(file, JSON.stringify({ schema: WINDOWS_JOB_SCHEMA, state: "holding" }));
  fs.writeFileSync(`${file}.ack`, JSON.stringify({ schema: WINDOWS_JOB_SCHEMA, nonce: "f".repeat(32), active: 0 }));
  // The fake holder answers from inside the waiter's sleep, as the real one does from its own loop.
  const answer = (active) => () => { const q = JSON.parse(fs.readFileSync(`${file}.query`, "utf8")); fs.writeFileSync(`${file}.ack`, JSON.stringify({ schema: WINDOWS_JOB_SCHEMA, nonce: q.nonce, active })); };
  assert.deepEqual(queryJobHolder(file, { sleep: answer(3) }), { active: 3 });
  assert.throws(() => queryJobHolder(file, { timeoutMs: 30, sleep: () => {} }), /did not answer a fresh count/);
  assert.equal(fs.existsSync(`${file}.query`), false);
});
