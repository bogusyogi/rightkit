// Host-wide build ownership (build-ownership.mjs; GATE-CONTRACT §1, joint plan step 4 G1, E6 "a second request ... never replaces
// the snapshot mid-stage"). Fake-probe cases pin each liveness rule; the real fixtures kill a gate whose child survives and show
// the next gate (and the next snapshot sync) refused until the child is gone, then reclaimed with the proof.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";
import { spawn, spawnSync } from "./background-process.mjs";
import {
  OWNERSHIP_ENV, OWNERSHIP_SCHEMA, HOLDER_SCHEMA, OwnershipBusy, acquireBuildOwnership, brokerLeaseState, childrenDir, holderFile, holderScript, holderScriptFile, jobName, judgeOwner, namesDir, readCleanMarker, cleanMarkerFile, CLEAN_SCHEMA, hostBootId, recordRootBirth, readRootBirth, rootBirthFile, ROOT_BIRTH_SCHEMA, ownershipFile, processProbe, queryHolder, readHolder, spawnUnderOwnership, CMD_LINE_LIMIT,
} from "./build-ownership.mjs";
import { keyPath, repoKey, snapshotCommand, snapshotRoot, syncSnapshot, verifySnapshot } from "./snapshot.mjs";

const tmp = (t, prefix) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix))); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };
const NOW = Date.now();
const iso = (ms) => new Date(ms).toISOString();

/** A probe whose answers the test sets; `self` is this test process. */
function fakeProbe(platform, over = {}) {
  return {
    platform,
    identity: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW - 60_000, group: platform === "win32" ? null : 4242 } : { alive: false, startedMs: null, group: null }),
    bootTime: () => NOW - 86_400_000,
    groupSnapshot: () => group([]),
    tokenHolders: () => null,
    snapshotUsers: () => null,
    // The owner's parent: the shell it ran under (a peer of the owner, never its child).
    ancestors: () => [{ pid: 5000, startedMs: NOW - 9e6 }],
    jobState: () => ({ open: true, active: 1 }),
    contain: ({ file }) => { fs.writeFileSync(file, JSON.stringify({ schema: HOLDER_SCHEMA, state: "holding", active: 1, holder: 1 })); return { state: "holding", contained: true }; },
    // The holder's fresh answer as a live gate's release asks for it: only the gate in its job.
    queryHolder: () => ({ active: 1, holder: 1, at: iso(NOW), nonce: "q" }),
    ...over,
  };
}
const noLease = () => { throw new Error("the lease must not be asked"); };
/** A process table: `rows` as { pid, ppid, startedMs }; `own` the asking gate's chain. */
function group(rows, own = [process.pid]) {
  const all = [{ pid: 1, ppid: 0, startedMs: NOW - 9e7 }, ...rows];
  return { members: rows, byPid: new Map(all.map((r) => [r.pid, r])), own: new Set(own) };
}
function deadOwner(file, platform, over = {}) {
  const handle = over.handle ?? "11111111-2222-3333-4444-555555555555";
  const record = {
    schema: OWNERSHIP_SCHEMA, handle, host: os.hostname(), platform, pid: 999_999, startedMs: NOW - 120_000,
    containment: platform === "win32" ? { kind: "job", name: jobName(handle) } : { kind: "group", group: 777 },
    ancestors: [{ pid: 5000, startedMs: NOW - 500_000 }],
    stage: "compile", snapshot: "/snap/x", commit: "c".repeat(40), acquiredAt: iso(NOW - 100_000), broker: { submitted: false }, ...over,
  };
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify(record));
  return record;
}
const acquire = (file, platform, probe, extra = {}) => acquireBuildOwnership({ file, platform, probe, lease: noLease, env: {}, exportEnv: false, stage: "test", ...extra });

test("ownership: a fresh host is held with a fresh handle, the containment and no broker submit; release removes it", (t) => {
  for (const platform of ["darwin", "linux", "win32"]) {
    const file = path.join(tmp(t, "rk-own-"), "build-ownership", "owner.json");
    const own = acquire(file, platform, fakeProbe(platform));
    const rec = JSON.parse(fs.readFileSync(file, "utf8"));
    assert.equal(rec.schema, OWNERSHIP_SCHEMA); assert.equal(rec.handle, own.handle); assert.equal(rec.pid, process.pid);
    assert.deepEqual(rec.containment, platform === "win32" ? { kind: "job", name: jobName(own.handle), contained: true } : { kind: "group", group: 4242 }, platform);
    assert.equal(rec.broker.submitted, false); assert.equal(own.held(), true);
    assert.equal(own.release().released, true, platform); assert.equal(fs.existsSync(file), false);
  }
});

test("ownership: a live owner is busy; a nested gate with the owner's handle joins and never releases; another handle is refused", (t) => {
  const file = path.join(tmp(t, "rk-own-"), "owner.json");
  const first = acquire(file, "darwin", fakeProbe("darwin"));
  const alive = fakeProbe("darwin", { identity: () => ({ alive: true, startedMs: NOW - 60_000, group: 4242 }) });
  assert.throws(() => acquire(file, "darwin", alive), (e) => e instanceof OwnershipBusy && /held by pid/.test(e.message));
  const nested = acquireBuildOwnership({ file, platform: "darwin", probe: alive, lease: noLease, env: { [OWNERSHIP_ENV]: first.handle }, exportEnv: false });
  assert.equal(nested.joined, true); nested.release(); assert.equal(first.held(), true, "a joined gate never releases");
  assert.throws(() => acquireBuildOwnership({ file, platform: "darwin", probe: alive, lease: noLease, env: { [OWNERSHIP_ENV]: "other" }, exportEnv: false }), /not the current owner/);
  fs.writeFileSync(file, "not json");
  assert.throws(() => acquire(file, "darwin", fakeProbe("darwin")), /cannot be read as an owner record/);
});

test("ownership POSIX: a dead owner is reclaimed only when its group has no member started under it and no process carries its handle", (t) => {
  const dir = tmp(t, "rk-own-");
  const file = path.join(dir, "owner.json");
  const rec = deadOwner(file, "linux");
  // The shell (5000) predates the ownership; the dead owner's orphaned child (5001, parent now pid 1) and its grandchild (5002)
  // are covered; a later `| tee` (5003, child of the shell) and a gate started later by the same script (5004) are not.
  const shell = { pid: 5000, ppid: 1, startedMs: NOW - 500_000 };
  const orphan = { pid: 5001, ppid: 1, startedMs: NOW - 50_000 };
  const grandchild = { pid: 5002, ppid: 5001, startedMs: NOW - 40_000 };
  const tee = { pid: 5003, ppid: 5000, startedMs: NOW - 1_000 };
  const later = { pid: 5004, ppid: 5000, startedMs: NOW - 1_000 };
  assert.throws(() => acquire(file, "linux", fakeProbe("linux", { groupSnapshot: () => group([shell, orphan, grandchild, tee, later]) })), /group 777 still has 2 member\(s\) started under this ownership: 5001, 5002/);
  assert.throws(() => acquire(file, "linux", fakeProbe("linux", { groupSnapshot: () => group([grandchild, { ...orphan, ppid: 6000 }]) })), /5002/, "a member whose ancestry cannot be followed counts");
  assert.throws(() => acquire(file, "linux", fakeProbe("linux", { groupSnapshot: () => { throw new Error("ps failed"); } })), /cannot be listed \(ps failed\)/);
  assert.throws(() => acquire(file, "linux", fakeProbe("linux", { tokenHolders: () => [6001] })), new RegExp(`6001 still carry ${OWNERSHIP_ENV}=${rec.handle}`));
  assert.throws(() => acquire(file, "linux", fakeProbe("linux", { tokenHolders: () => { throw new Error("proc"); } })), /cannot be listed \(proc\)/);
  const own = acquire(file, "linux", fakeProbe("linux", { groupSnapshot: () => group([shell, tee, later]), tokenHolders: () => [] }));
  assert.equal(own.reclaimed.from, rec.handle);
  assert.deepEqual(own.reclaimed.proofs, [`owner pid 999999 is gone`, "process group 777 has no member started under this ownership", `no process carries ${OWNERSHIP_ENV}=${rec.handle}`]);
  // A pid that now runs another process (another start) is a dead owner; one whose start agrees is alive.
  deadOwner(file, "darwin");
  const reused = fakeProbe("darwin", { identity: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW, group: 4242 } : { alive: true, startedMs: NOW - 10_000, group: 1 }) });
  assert.match(acquire(file, "darwin", reused).reclaimed.proofs[0], /now runs another process/);
  deadOwner(file, "darwin");
  const same = fakeProbe("darwin", { identity: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW, group: 4242 } : { alive: true, startedMs: NOW - 120_500, group: 1 }) });
  assert.throws(() => acquire(file, "darwin", same), /held by pid 999999/);
});

test("ownership POSIX: every process group a gate started detached must be empty; a half-recorded spawn keeps the host; the environment and snapshot-use nets each hold it", (t) => {
  const dir = tmp(t, "rk-own-");
  const file = path.join(dir, "owner.json");
  const rec = deadOwner(file, "darwin");
  const reg = childrenDir(file, rec.handle);
  fs.mkdirSync(reg, { recursive: true });
  fs.writeFileSync(path.join(reg, "intent-1-x.json"), "{}");
  assert.throws(() => acquire(file, "darwin", fakeProbe("darwin")), /detached spawn was in flight .* process group is unknown/);
  fs.rmSync(path.join(reg, "intent-1-x.json"));
  fs.writeFileSync(path.join(reg, "group-8100.json"), JSON.stringify({ group: 8100, startedMs: NOW - 90_000, command: "eslint" }));
  const scanGroup = { members: [{ pid: 8100, ppid: 1, startedMs: NOW - 89_000 }, { pid: 8101, ppid: 8100, startedMs: NOW - 80_000 }], byPid: new Map(), own: new Set() };
  const probe = (over) => fakeProbe("darwin", { groupSnapshot: (g) => (g === 8100 ? scanGroup : group([])), ...over });
  assert.throws(() => acquire(file, "darwin", probe()), /detached process group 8100 \(eslint\) still has 2 member\(s\): 8100, 8101/);
  assert.throws(() => acquire(file, "darwin", probe({ groupSnapshot: (g) => { if (g === 8100) throw new Error("ps"); return group([]); } })), /detached process group 8100 cannot be listed/);
  // A pid reused by an older process is not the scan's group.
  const reused = { members: [{ pid: 8100, ppid: 1, startedMs: NOW - 500_000 }], byPid: new Map(), own: new Set() };
  assert.throws(() => acquire(file, "darwin", probe({ groupSnapshot: (g) => (g === 8100 ? reused : group([])), tokenHolders: () => [9100] })), /9100 still carry/);
  assert.throws(() => acquire(file, "darwin", probe({ groupSnapshot: (g) => (g === 8100 ? reused : group([])), snapshotUsers: () => [{ pid: 9200, how: "open file /snap/x/a.rs" }, { pid: 9201, how: "argument" }] })), /9200 \(open file \/snap\/x\/a.rs\), 9201 \(argument\) still use the snapshot \/snap\/x/);
  assert.throws(() => acquire(file, "darwin", probe({ groupSnapshot: (g) => (g === 8100 ? reused : group([])), snapshotUsers: () => { throw new Error("lsof"); } })), /processes using the snapshot cannot be listed \(lsof\)/);
  const own = acquire(file, "darwin", probe({ groupSnapshot: (g) => (g === 8100 ? reused : group([])), tokenHolders: () => [], snapshotUsers: () => [] }));
  assert.match(own.reclaimed.proofs.join("; "), /the 1 detached process group\(s\) it started are empty; no process carries .*; no process has a working directory, executable, open or mapped file or argument inside or naming \/snap\/x/);
  assert.equal(fs.existsSync(reg), false, "the reclaimed owner's detached-child record goes with it");
});

test("spawnUnderOwnership: only a detached POSIX spawn under a held ownership is recorded, intent first, then its group", (t) => {
  const dir = tmp(t, "rk-own-");
  const file = path.join(dir, "build-ownership", "owner.json");
  const env = { [OWNERSHIP_ENV]: "h-9", RIGHTKIT_BUILD_OWNERSHIP_FILE: file };
  const reg = childrenDir(file, "h-9");
  const seen = [];
  const fake = (cmd, args, opts) => { seen.push(fs.existsSync(reg) ? fs.readdirSync(reg) : []); return { pid: 4321 }; };
  spawnUnderOwnership(fake, "eslint", [], { detached: true }, { env, platform: "darwin" });
  assert.match(seen[0][0], /^intent-/, "the intent exists while the child is created");
  assert.deepEqual(fs.readdirSync(reg), ["group-4321.json"]);
  for (const [opts, ctx] of [[{}, { env, platform: "darwin" }], [{ detached: true }, { env, platform: "win32" }], [{ detached: true }, { env: {}, platform: "darwin" }]]) {
    fs.rmSync(reg, { recursive: true, force: true });
    spawnUnderOwnership(fake, "x", [], opts, ctx);
    assert.equal(fs.existsSync(reg), false, JSON.stringify([opts, ctx.platform]));
  }
  // A parent that pauses after the child exists still records a bound from before the spawn (review of 05a0ab74).
  fs.rmSync(reg, { recursive: true, force: true });
  const before = Date.now();
  spawnUnderOwnership(() => { Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 2500); return { pid: 4400 }; }, "eslint", [], { detached: true }, { env, platform: "darwin" });
  const recorded = JSON.parse(fs.readFileSync(path.join(reg, "group-4400.json"), "utf8"));
  assert.ok(recorded.startedMs <= before, `bound ${recorded.startedMs} is from before the spawn (${before}), not after the 2.5 s pause`);
  // ...so a member that started right at the spawn, 2.5 s before the record was written, is still the scan's group.
  fs.writeFileSync(file, JSON.stringify({ schema: OWNERSHIP_SCHEMA, handle: "h-9", host: os.hostname(), platform: "darwin", pid: 999_999, startedMs: before - 5000, containment: { kind: "group", group: 777 }, snapshot: null, acquiredAt: iso(before - 4000), broker: { submitted: false } }));
  const member = { members: [{ pid: 4400, ppid: 1, startedMs: before }], byPid: new Map(), own: new Set() };
  assert.throws(() => acquire(file, "darwin", fakeProbe("darwin", { groupSnapshot: (g) => (g === 4400 ? member : group([])) })), /detached process group 4400 \(eslint\) still has 1 member/);
  fs.rmSync(file);
  fs.rmSync(reg, { recursive: true, force: true });
  assert.throws(() => spawnUnderOwnership(() => { throw new Error("ENOENT"); }, "x", [], { detached: true }, { env, platform: "darwin" }), /ENOENT/);
  assert.deepEqual(fs.readdirSync(reg), [], "a spawn that failed leaves nothing behind");
});

test("ownership Windows: a dead owner is reclaimed only when its job is empty, or gone with its holder's drain (or failure) recorded", (t) => {
  const dir = tmp(t, "rk-own-");
  const file = path.join(dir, "owner.json");
  const rec = deadOwner(file, "win32");
  assert.throws(() => acquire(file, "win32", fakeProbe("win32", { jobState: () => ({ open: true, active: 2 }) })), /still holds 2 process/);
  assert.throws(() => acquire(file, "win32", fakeProbe("win32", { jobState: () => { throw new Error("powershell: x"); } })), /cannot be read \(powershell: x\)/);
  assert.throws(() => acquire(file, "win32", fakeProbe("win32", { jobState: () => ({ open: false }) })), /holder recorded no drain \(no holder record\)/);
  fs.writeFileSync(holderFile(file, rec.handle), JSON.stringify({ schema: HOLDER_SCHEMA, state: "holding", holder: 1 }));
  assert.throws(() => acquire(file, "win32", fakeProbe("win32", { jobState: () => ({ open: false }) })), /holder recorded no drain \(holding\)/, "a holder killed before the drain is unknown");
  fs.writeFileSync(holderFile(file, rec.handle), JSON.stringify({ schema: HOLDER_SCHEMA, state: "drained", holder: 7, at: iso(NOW) }));
  const own = acquire(file, "win32", fakeProbe("win32", { jobState: () => ({ open: false }) }));
  assert.match(own.reclaimed.proofs.join("; "), /drained \(holder pid 7/);
  assert.equal(fs.existsSync(holderFile(file, rec.handle)), false, "the reclaimed owner's holder record goes with it");
  own.release();
  const rec2 = deadOwner(file, "win32", { handle: "aaaaaaaa-2222-3333-4444-555555555555" });
  assert.equal(acquire(file, "win32", fakeProbe("win32", { jobState: () => ({ open: true, active: 0 }) })).reclaimed.from, rec2.handle);
  // A gate whose children were never in its job (contained: false) is never cleared by the job, drained or not; only a reboot.
  const rec3 = deadOwner(file, "win32", { handle: "bbbbbbbb-2222-3333-4444-555555555555", containment: { kind: "job", name: jobName("bbbbbbbb-2222-3333-4444-555555555555"), contained: false } });
  fs.writeFileSync(holderFile(file, rec3.handle), JSON.stringify({ schema: HOLDER_SCHEMA, state: "drained", holder: 7, at: iso(NOW) }));
  assert.throws(() => acquire(file, "win32", fakeProbe("win32", { jobState: () => ({ open: false }) })), /never held its gate's children .* until the host reboots/);
  assert.equal(acquire(file, "win32", fakeProbe("win32", { bootTime: () => NOW, jobState: () => ({ open: false }) })).reclaimed.from, rec3.handle, "a reboot clears it");
});

test("ownership: a reboot after the record proves its work gone; broker work needs REAPED; a submit is marked before it happens", (t) => {
  const dir = tmp(t, "rk-own-");
  const file = path.join(dir, "owner.json");
  deadOwner(file, "darwin");
  const rebooted = fakeProbe("darwin", { bootTime: () => NOW, groupSnapshot: () => { throw new Error("must not be asked"); } });
  assert.match(acquire(file, "darwin", rebooted).reclaimed.proofs.join("; "), /rebooted after the record/);
  for (const [state, ok] of [["ACTIVE", false], ["UNKNOWN", false], ["REAPED", true]]) {
    deadOwner(file, "darwin", { broker: { submitted: true } });
    const asked = [];
    const lease = (q) => { asked.push(q); return { state, why: `fake ${state}` }; };
    const go = () => acquireBuildOwnership({ file, platform: "darwin", probe: fakeProbe("darwin"), lease, env: {}, exportEnv: false });
    if (ok) assert.match(go().reclaimed.proofs.join("; "), /lease .* is REAPED/);
    else assert.throws(go, new RegExp(`lease is ${state}`));
    assert.deepEqual(asked[0], { snapshot: "/snap/x", handle: "11111111-2222-3333-4444-555555555555" });
  }
  fs.rmSync(file, { force: true });
  const leases = [];
  const own = acquireBuildOwnership({ file, platform: "darwin", probe: fakeProbe("darwin"), lease: (q) => { leases.push(q); return { state: "ACTIVE", why: "x" }; }, env: {}, exportEnv: false, snapshot: "/snap/y" });
  own.markBrokerSubmit();
  assert.equal(JSON.parse(fs.readFileSync(file, "utf8")).broker.submitted, true);
  const r = own.release();
  assert.equal(r.released, false, "a gate whose broker lease is still ACTIVE leaves the record for the next gate");
  assert.match(r.why, /lease is ACTIVE/); assert.equal(leases.length, 1); assert.equal(fs.existsSync(file), true);
});

test("ownership Windows: a failure after the record is published (the containment write) removes the record and throws busy; nothing is left held", (t) => {
  const file = path.join(tmp(t, "rk-own-"), "build-ownership", "owner.json");
  // owner.json cannot be replaced (the Dell's intermittent EPERM on rename): done() must not leave the record to a gate that is not running.
  const rename = t.mock.method(fs, "renameSync", () => { throw Object.assign(new Error("EPERM: operation not permitted, rename"), { code: "EPERM" }); });
  assert.throws(() => acquire(file, "win32", fakeProbe("win32")), (e) => e instanceof OwnershipBusy && /containment could not be recorded \(EPERM/.test(e.message));
  assert.ok(rename.mock.callCount() >= 1);
  assert.equal(fs.existsSync(file), false, "no record is left behind");
  assert.deepEqual(fs.readdirSync(path.dirname(file)).filter((n) => n.endsWith(".tmp")), [], "no temp file is left behind");
  rename.mock.restore();
  // The host is free again: the next gate acquires at once.
  const own = acquire(file, "win32", fakeProbe("win32"));
  assert.equal(own.held(), true); own.release();
});

test("ownership: a normal release with a survivor leaves the record; a gate that cannot be contained never holds", (t) => {
  const dir = tmp(t, "rk-own-");
  const file = path.join(dir, "owner.json");
  // Windows release asks the holder for a fresh count (a probe it spawned would sit in the job itself); its last report is never read.
  let answer = () => ({ active: 3, holder: 9, at: iso(NOW), nonce: "q" });
  const own = acquire(file, "win32", fakeProbe("win32", { jobState: () => { throw new Error("release must not spawn a job probe"); }, queryHolder: () => answer() }));
  assert.match(own.release().why, /still holds 3 process\(es\) \(this gate is one\)/); assert.equal(fs.existsSync(file), true);
  // The holder's last report says only the gate is left, but the holder does not answer now: unknown, so held (BLOCK: a cached
  // count is not a proof).
  fs.writeFileSync(holderFile(file, own.handle), JSON.stringify({ schema: HOLDER_SCHEMA, state: "holding", active: 1, holder: 9 }));
  answer = () => { throw new Error("the job holder did not answer a fresh count within 5000 ms"); };
  assert.match(own.release().why, /gave no fresh count \(the job holder did not answer/); assert.equal(fs.existsSync(file), true);
  answer = () => ({ active: 0, holder: 9, at: iso(NOW), nonce: "q" });
  assert.match(own.release().why, /counted 0 process\(es\), but this live gate is one/); assert.equal(fs.existsSync(file), true);
  answer = () => ({ active: 1, holder: 9, at: iso(NOW), nonce: "q" });
  const freed = own.release();
  assert.equal(freed.released, true, "a fresh answer of only the gate: released"); assert.match(freed.proofs.join("; "), /holder 9 counted it .* for query q/);
  // A live gate whose record is not proven contained is fenced like a crash: held, however empty the job looks.
  const loose = acquire(file, "win32", fakeProbe("win32"));
  const recLoose = JSON.parse(fs.readFileSync(file, "utf8")); fs.writeFileSync(file, JSON.stringify({ ...recLoose, containment: { ...recLoose.containment, contained: false } }));
  assert.match(loose.release().why, /never proven to hold this gate's children \(contained: false\).*until the host reboots/); assert.equal(fs.existsSync(file), true);
  fs.rmSync(file, { force: true });
  // A gate the self-check finds outside its job never starts work: refused at acquisition, no record left behind.
  for (const contained of [false, undefined]) {
    assert.throws(() => acquire(file, "win32", fakeProbe("win32", { contain: () => ({ state: "holding", contained }) })), (e) => e instanceof OwnershipBusy && /would not stay in job/.test(e.message), String(contained));
    assert.equal(fs.existsSync(file), false, `no record for an uncontained gate (${contained})`);
  }
  // At release the gate is alive: its own child (ppid = this process) is a survivor; a peer of the shell is not.
  const posix = acquire(file, "linux", fakeProbe("linux", { groupSnapshot: () => group([{ pid: 9, ppid: process.pid, startedMs: Date.now() }, { pid: 10, ppid: 5000, startedMs: Date.now() }, { pid: 5000, ppid: 1, startedMs: NOW - 9e6 }], []) }));
  assert.match(posix.release().why, /1 member\(s\) started under this ownership: 9$/); assert.equal(fs.existsSync(file), true);
  fs.rmSync(file);
  assert.throws(() => acquire(file, "win32", fakeProbe("win32", { contain: () => { throw new Error("the job holder could not contain the gate: AssignProcessToJobObject failed (5)"); } })), /could not contain the gate/);
  assert.equal(fs.existsSync(file), false, "the record of a gate that was never contained is removed");
});

test("queryHolder: only an answer that echoes this query's nonce, written after it, counts; a silent or stale holder is unknown", async (t) => {
  // A real second process plays the holder (the PowerShell holder's protocol: read <file>.query, count, write <file>.ack).
  const dir = tmp(t, "rk-own-query-");
  const file = path.join(dir, "holder-h.json");
  fs.writeFileSync(file, JSON.stringify({ schema: HOLDER_SCHEMA, state: "holding", active: 1, holder: 1 }), "utf8");
  const responder = (mode) => {
    const script = `const fs = require("fs"); const f = ${JSON.stringify(file)}; let n = 0;
      const tick = () => { try { const q = JSON.parse(fs.readFileSync(f + ".query", "utf8")); n += 1;
        const nonce = ${JSON.stringify(mode)} === "stale" ? "0".repeat(32) : q.nonce;
        fs.writeFileSync(f + ".ack.tmp", JSON.stringify({ schema: ${JSON.stringify(HOLDER_SCHEMA)}, nonce, active: 1 + n, holder: process.pid, at: new Date().toISOString() })); fs.renameSync(f + ".ack.tmp", f + ".ack"); } catch {} };
      setInterval(tick, 20); setTimeout(() => process.exit(0), 20000);`;
    const child = spawn(process.execPath, ["-e", script], { stdio: "ignore" });
    t.after(() => { try { child.kill(); } catch { /* gone */ } });
    return child;
  };
  // No holder answers: the last report (active 1) is not used.
  assert.throws(() => queryHolder(file, { timeoutMs: 300 }), /did not answer a fresh count within 300 ms/);
  assert.equal(fs.existsSync(`${file}.query`), false, "the query is withdrawn");
  // A holder that answers with another nonce (an old answer) is not this query's answer.
  const stale = responder("stale");
  await new Promise((r) => setTimeout(r, 200));
  assert.throws(() => queryHolder(file, { timeoutMs: 400 }), /did not answer a fresh count/);
  stale.kill(); await new Promise((r) => setTimeout(r, 100));
  // A live holder counts afresh for each query (its count here rises per query: each answer is new, not cached).
  const live = responder("live");
  await new Promise((r) => setTimeout(r, 200));
  const first = queryHolder(file, { timeoutMs: 5000 });
  const second = queryHolder(file, { timeoutMs: 5000 });
  assert.ok(Number.isInteger(first.active) && second.active > first.active, `${first.active} then ${second.active}`);
  assert.notEqual(first.nonce, second.nonce); assert.equal(first.holder, live.pid);
  assert.equal(fs.existsSync(`${file}.query`), false, "the query is withdrawn once answered");
});

test("Windows holder launch: the script is a file PowerShell runs with -File, so the command line stays far under cmd.exe's limit", (t) => {
  // Dell 2026-10-29 (dell-ev7-build-ownership.log): passed inline as -EncodedCommand the holder reached ~9000 characters and
  // cmd.exe (8191 at most) never started it; every native Windows ownership test then timed out waiting for "holding".
  const dir = tmp(t, "rk-own-launch-");
  const file = path.join(dir, "holder-h.json");
  let seen = null;
  const probe = processProbe({
    platform: "win32",
    spawn: () => ({ status: 0, stdout: "IN\n", stderr: "" }),
    spawnHolder: (cmd, args) => {
      const script = args[args.indexOf("-File") + 1];
      seen = { cmd, args, line: [cmd, ...args].join(" "), script, body: fs.readFileSync(script, "utf8"), mode: fs.statSync(script).mode & 0o777 };
      fs.writeFileSync(file, JSON.stringify({ schema: HOLDER_SCHEMA, state: "holding", holder: 1 }));
      return { unref() {} };
    },
  });
  const held = probe.contain({ name: jobName("h"), pid: 4242, file, timeoutMs: 2000 });
  assert.equal(held.contained, true);
  assert.equal(seen.cmd, "cmd.exe"); assert.equal(seen.script, holderScriptFile(file));
  assert.ok(seen.line.length < CMD_LINE_LIMIT / 10, `launch is ${seen.line.length} characters`);
  assert.ok(!seen.args.includes("-EncodedCommand"), "no inline script");
  assert.equal(seen.body, holderScript({ name: jobName("h"), pid: 4242, file, selfDelete: true }));
  assert.match(seen.body, /Remove-Item -LiteralPath \$PSCommandPath/);
  if (process.platform !== "win32") assert.equal(seen.mode, 0o600);
  assert.equal(fs.existsSync(holderScriptFile(file)), false, "the script file does not outlive the launch");
  // Inline, the same script would not fit: the reason it is a file.
  assert.ok(Buffer.from(holderScript({ name: jobName("0".repeat(36)), pid: 4242, file }), "utf16le").toString("base64").length > CMD_LINE_LIMIT);
});

// ---- Injected probes. macOS paths run through a scripted ps/lsof; Linux paths run on a fake /proc tree (`proc`) with injected readers,
// so both are exercised on any host. The real-/proc and real-lsof paths are covered by the [native] fixtures below. ----

const PS_ROW = (pid, ppid, pgid) => `${pid} ${ppid} ${pgid} Thu Oct 29 00:00:00 2026`;
/** A scripted `ps`/`lsof`: the process table `rows` (pid, ppid, pgid), `args` the `ps -ww` listing, `lsof` the -F output. */
function scripted({ rows = [[process.pid, 1, process.pid]], args = [], lsof = "", lsofStatus = 0, eww = "" } = {}) {
  return (cmd, argv) => {
    if (cmd === "lsof") return { status: lsofStatus, stdout: lsof, stderr: lsofStatus ? "lsof: WARNING: can't stat()" : "", pid: 61 };
    if (argv[0] === "eww") return { status: 0, stdout: eww, pid: 62 };
    if (argv.includes("pgid=")) return { status: 0, stdout: rows.map((r) => PS_ROW(...r)).join("\n"), pid: 60 };
    return { status: 0, stdout: args.map(([pid, command]) => `${pid} ${command}`).join("\n"), pid: 63 };
  };
}

test("probes [injected ps/lsof]: a listing that did not finish cleanly is unknown; the snapshot-use net sees cwd, descriptors, mappings and arguments", () => {
  const probe = (over) => processProbe({ platform: "darwin", spawn: scripted(over), uid: 501 });
  assert.throws(() => probe({ lsofStatus: 1, lsof: "p123\nfcwd\nn/snap/k\n" }).snapshotUsers("/snap/k"), /lsof: lsof: WARNING/);
  const lsof = "p123\nfcwd\nn/snap/k/src\np124\nfcwd\nn/elsewhere\nf7r\nn/snap/k/src/lib.rs\np125\nftxt\nn/usr/bin/cc\nfmem\nn/snap/k/target/x.dylib\np126\nf3u\nn/snap/k2/other\n";
  assert.deepEqual(probe({ lsof }).snapshotUsers("/snap/k"), [{ pid: 123, how: "working directory" }, { pid: 124, how: "open file /snap/k/src/lib.rs" }, { pid: 125, how: "mapped file /snap/k/target/x.dylib" }], "a sibling snapshot's file is not this snapshot's");
  // A process that dropped the variable, left the session and runs elsewhere, holding nothing open, is still found by its argument.
  assert.deepEqual(probe({ args: [[301, "rustc --crate-name a /snap/k/src/lib.rs"], [302, "rustc /snap/k.old/src/lib.rs"], [303, "cargo build --manifest-path=/snap/k/Cargo.toml"]] }).snapshotUsers("/snap/k"), [{ pid: 301, how: "argument" }, { pid: 303, how: "argument" }]);
  // This gate's own chain is never its own user (a shell whose command line names the snapshot, the ps and lsof it ran).
  assert.deepEqual(probe({ args: [[process.pid, `node gate --snapshot /snap/k`], [63, "ps -A /snap/k"]], lsof: `p${process.pid}\nfcwd\nn/snap/k\np61\nfcwd\nn/snap/k\n` }).snapshotUsers("/snap/k"), []);
  assert.throws(() => processProbe({ platform: "darwin", spawn: (cmd, argv) => (cmd === "ps" && !argv.includes("pgid=") ? { status: 1, stdout: "", stderr: "ps: failed" } : scripted()(cmd, argv)) }).snapshotUsers("/snap/k"), /ps arguments: ps: failed/);
  const env = (status) => processProbe({ platform: "darwin", spawn: (cmd, args) => (args[0] === "eww" ? { status, stdout: `77 node RIGHTKIT_BUILD_OWNERSHIP=h1 PATH=/x\n` } : { status: 0, stdout: "", pid: 1 }) });
  assert.throws(() => env(1).tokenHolders("h1"), /ps eww/);
  assert.deepEqual(env(0).tokenHolders("h1"), [77]);
  assert.deepEqual(env(0).tokenHolders("h"), [], "a prefix of the handle is not the handle");
});

test("namesDir: the directory or a path under it, never a longer sibling", () => {
  assert.equal(namesDir("rustc /snap/k/src/lib.rs", "/snap/k"), true);
  assert.equal(namesDir("--manifest-path=/snap/k/Cargo.toml", "/snap/k"), true);
  assert.equal(namesDir("cd /snap/k", "/snap/k"), true);
  assert.equal(namesDir("a\0/snap/k\0b", "/snap/k"), true);
  assert.equal(namesDir("/snap/k.old/x /snap/kk /snap/k-2", "/snap/k"), false);
  assert.equal(namesDir("/mnt/snap/k/x", "/snap/k"), false, "a longer path that merely ends in the directory's name");
  assert.equal(namesDir("/snap/k/", "/snap/k/"), true);
});

const BTIME = Math.floor((NOW - 2e7) / 1000);
const PROC = "/fakeproc";
const eno = (code, f) => Object.assign(new Error(`${code}: ${f}`), { code });
/**
 * A fake /proc held in memory (no files or symlinks, so it behaves the same on every host): `procs` is { pid: { uid, start (epoch ms;
 * default long before any ownership), environ, cwd, root, exe, fds: { n: target }, maps, cmdline } }. `files`, `links` and `pids` may
 * be changed by a test.
 */
function fakeProc(procs) {
  const files = new Map([[`${PROC}/stat`, `cpu  1 2 3\nbtime ${BTIME}\nprocesses 9\n`]]);
  const links = new Map(); const pids = new Set();
  for (const [pid, p] of Object.entries(procs)) {
    pids.add(pid);
    // Fields 3..22 of /proc/<pid>/stat; the comm holds a space and parentheses, as a real one may.
    files.set(`${PROC}/${pid}/stat`, `${pid} (a b) (c) S 1 ${pid} ${pid} 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 ${Math.round(((p.start ?? NOW - 1e7) - BTIME * 1000) / 10)} 0 0\n`);
    files.set(`${PROC}/${pid}/status`, `Name:\tx\nState:\t${p.state ?? "S (sleeping)"}\nUid:\t${p.uid ?? 1000}\t${p.uid ?? 1000}\t${p.uid ?? 1000}\t${p.uid ?? 1000}\n`);
    files.set(`${PROC}/${pid}/environ`, p.environ ?? "A=1\0");
    files.set(`${PROC}/${pid}/cmdline`, p.cmdline ?? "sleep\0");
    files.set(`${PROC}/${pid}/maps`, p.maps ?? "");
    for (const link of ["cwd", "root", "exe"]) links.set(`${PROC}/${pid}/${link}`, p[link] ?? "/");
    for (const [n, target] of Object.entries(p.fds ?? {})) links.set(`${PROC}/${pid}/fd/${n}`, target);
  }
  return {
    files, links, pids,
    exists: (f) => f === PROC,
    readFile: (f, enc) => { if (!files.has(f)) throw eno("ENOENT", f); return enc ? files.get(f) : Buffer.from(files.get(f)); },
    readlink: (f) => { if (!links.has(f)) throw eno("ENOENT", f); return links.get(f); },
    readdir: (d) => {
      if (d === PROC) return [...pids];
      const m = /^\/fakeproc\/(\d+)\/fd$/.exec(d);
      if (m && pids.has(m[1])) return [...links.keys()].filter((k) => k.startsWith(`${d}/`)).map((k) => k.slice(d.length + 1));
      throw eno("ENOENT", d);
    },
  };
}
/** A reader of `fake` (`kind`: readFile, readlink or readdir) that fails with `code` on the paths `re` matches. */
const deny = (fake, kind, re, code = "EACCES") => ({ [kind]: (f, ...rest) => { if (re.test(f)) throw eno(code, f); return fake[kind](f, ...rest); } });
const linuxProbe = (fake, ...overs) => processProbe({ platform: "linux", spawn: scripted(), proc: PROC, uid: 1000, exists: fake.exists, readFile: fake.readFile, readlink: fake.readlink, readdir: fake.readdir, ...Object.assign({}, ...overs) });

test("probes [injected /proc]: an unreadable entry of this user's is UNKNOWN (held), never 'not a reader'; gone and other users' entries are skipped", () => {
  const SNAP = "/snap/k";
  const fake = fakeProc({ 2001: {}, 2002: { uid: 0 }, 2003: {} });
  // Environment net.
  assert.deepEqual(linuxProbe(fake).tokenHolders("h1"), []);
  fake.files.set(`${PROC}/2003/environ`, "A=1\0RIGHTKIT_BUILD_OWNERSHIP=h1\0");
  assert.deepEqual(linuxProbe(fake).tokenHolders("h1"), [2003]);
  assert.throws(() => linuxProbe(fake, deny(fake, "readFile", /\/2001\/environ$/)).tokenHolders("h1"), /\(a process of this user[^)]*\) is unreadable \(EACCES\).*unknown/, "this user's process, environment unreadable");
  assert.throws(() => linuxProbe(fake, deny(fake, "readFile", /\/2001\/environ$/, "EPERM")).tokenHolders("h1"), /unreadable \(EPERM\)/);
  assert.deepEqual(linuxProbe(fake, deny(fake, "readFile", /\/2002\/environ$/)).tokenHolders("h1"), [2003], "another user's process is skipped");
  assert.deepEqual(linuxProbe(fake, deny(fake, "readFile", /\/200[12]\/environ$/, "ENOENT")).tokenHolders("h1"), [2003], "a process that vanished is not a reader");
  assert.throws(() => linuxProbe(fake, deny(fake, "readFile", /\/2001\/(environ|status)$/)).tokenHolders("h1"), /is unreadable \(EACCES\) and so is its owner/, "an owner that cannot be read is unknown");
  assert.throws(() => linuxProbe(fake, deny(fake, "readFile", /\/2001\/environ$/, "EIO")).tokenHolders("h1"), /EIO/, "any other failure is unknown too");
  // A real Linux host without /proc is unknown (throws); a Linux platform simulated on another host has nothing to read (no net).
  const missing = processProbe({ platform: "linux", spawn: scripted(), proc: "/nowhere", exists: () => false, uid: 1000 });
  if (process.platform === "linux") assert.throws(() => missing.tokenHolders("h1"), /no .* on this linux host/);
  else assert.equal(missing.tokenHolders("h1"), null);
  // Snapshot-use net: every signal, one process each.
  const fake2 = fakeProc({
    3001: { cwd: `${SNAP}/src` }, 3002: { root: SNAP }, 3003: { exe: `${SNAP}/bin/tool` }, 3004: { fds: { 0: "/dev/null", 5: `${SNAP}/src/lib.rs` } },
    3005: { maps: `7f00-7f01 r--p 00000000 fe:01 1234 ${SNAP}/target/x.so\n7f02-7f03 r-xp 0 fe:01 7 /usr/lib/libc.so\n` }, 3006: { cmdline: `rustc\0${SNAP}/src/lib.rs\0` },
    3007: { cwd: "/snap/k2", fds: { 3: "pipe:[99]", 4: "/snap/k2/x" }, maps: "7f00-7f01 r--p 0 fe:01 1 /snap/k2/y (deleted)\n", cmdline: "x\0/snap/k.old/a\0" }, 3008: { maps: `7f00-7f01 r--p 0 fe:01 1 ${SNAP}/gone.so (deleted)\n` },
  });
  const byPid = (a, b) => a.pid - b.pid;
  assert.deepEqual(linuxProbe(fake2).snapshotUsers(SNAP).sort(byPid), [
    { pid: 3001, how: "working directory" }, { pid: 3002, how: "root directory" }, { pid: 3003, how: "executable" }, { pid: 3004, how: `open file ${SNAP}/src/lib.rs` },
    { pid: 3005, how: `mapped file ${SNAP}/target/x.so` }, { pid: 3006, how: "argument" }, { pid: 3008, how: `mapped file ${SNAP}/gone.so` },
  ]);
  // Unreadable: each reader's failure on this user's process fails the net; another user's is skipped; a vanished entry is no reader.
  for (const what of [/\/3007\/cwd$/, /\/3007\/exe$/, /\/3007\/fd$/, /\/3007\/fd\/4$/, /\/3007\/maps$/, /\/3007\/cmdline$/]) {
    const probe = () => linuxProbe(fake2, deny(fake2, "readFile", what), deny(fake2, "readlink", what), deny(fake2, "readdir", what));
    assert.throws(() => probe().snapshotUsers(SNAP), /3007 \(a process of this user[^)]*\) is unreadable \(EACCES\)/, String(what));
    fake2.files.set(`${PROC}/3007/status`, "Uid:\t0\t0\t0\t0\n");
    assert.equal(probe().snapshotUsers(SNAP).some((u) => u.pid === 3007), false, `another user's ${what} skipped`);
    fake2.files.set(`${PROC}/3007/status`, "Uid:\t1000\t1000\t1000\t1000\n");
  }
  assert.throws(() => linuxProbe(fake2, deny(fake2, "readlink", /\/3007\/cwd$/, "EIO")).snapshotUsers(SNAP), /EIO/);
  // The gate's own chain is not its own user.
  assert.equal(linuxProbe(fake2, { spawn: scripted({ rows: [[process.pid, 3006, process.pid], [3006, 1, 3006]] }) }).snapshotUsers(SNAP).some((u) => u.pid === 3006), false);
});

test("probes [injected /proc]: an unreadable process of this user is skipped only if it predates the previous owner's acquisition (whose release proved zero readers); everything else is held", () => {
  const SNAP = "/snap/k";
  // THE INVARIANT: `prev` is the acquisition of the previous owner whose release proved zero readers; this ownership began at NOW - 100 s.
  const prev = NOW - 600_000;
  const fake = fakeProc({
    5001: { start: NOW - 3_600_000 },     // (a) unreadable, older than the previous acquisition: skipped
    5002: { start: NOW - 10_000 },        // (b) unreadable, after this ownership began
    5006: { start: NOW - 400_000 },       // (b) unreadable, between the previous acquisition and this ownership: may be the previous owner's leftover
    5003: { start: prev - 2_500 },        // unreadable, older than prev but inside the start-reading error (START_SLACK_MS): not provably older
    5004: { start: prev - 3_500 },        // unreadable, older than prev and the reading error: skipped
    5005: { start: NOW - 3_600_000, uid: 0 },
    5007: { start: NOW - 10_000, state: "Z (zombie)" },   // a zombie reads nothing, however new, and its /proc entries refuse reads
    5008: { start: NOW - 10_000, state: "R (running)" },
  });
  const noEnv = (pid) => deny(fake, "readFile", new RegExp(`/${pid}/environ$`));
  const noLink = (pid, ...links) => deny(fake, "readlink", new RegExp(`/${pid}/(${links.join("|")})$`));
  const noStat = (pid, code = "EACCES") => deny(fake, "readFile", new RegExp(`/${pid}/(environ|stat)$`), code);
  const tokens = (pid, ...overs) => linuxProbe(fake, noEnv(pid), ...overs).tokenHolders("h1", prev);
  assert.deepEqual(tokens(5001), [], "(a) unreadable, older than the previous acquisition: skipped");
  assert.throws(() => tokens(5002), /5002 \(a process of this user[^)]*\) is unreadable \(EACCES\) and it started at .* not before the baseline/, "(b) unreadable and newer: held");
  assert.throws(() => tokens(5006), /5006 .* not before the baseline/, "(b) between the previous acquisition and this ownership: held");
  assert.throws(() => tokens(5003), /5003 .* not before the baseline/, "inside the btime resolution: held");
  assert.deepEqual(tokens(5004), [], "older than the previous acquisition and the resolution: skipped");
  assert.deepEqual(tokens(5005), [], "another user's: skipped");
  assert.deepEqual(tokens(5007), [], "a zombie: reads nothing, skipped even when new and with no marker");
  assert.deepEqual(linuxProbe(fake, noEnv(5007)).tokenHolders("h1", null), [], "a zombie: skipped with no marker");
  assert.throws(() => tokens(5008), /5008 \(a process of this user: x, R\) is unreadable/, "a running process that is new: held");
  // (c)/(d) there is no baseline for this ownership (held, unknown or reclaimed; or no marker): nothing unreadable is skipped, however old.
  for (const none of [undefined, NaN, null]) assert.throws(() => linuxProbe(fake, noEnv(5001)).tokenHolders("h1", none), /5001 .* there is no baseline for this ownership/, `skipBefore ${none}`);
  // (e) stat itself unreadable (or malformed, or boot time unknown): the process is still unknown, so held.
  assert.throws(() => linuxProbe(fake, noStat(5001)).tokenHolders("h1", prev), /5001 \(a process of this user[^)]*\) is unreadable \(EACCES\) and its start time cannot be read/);
  assert.deepEqual(linuxProbe(fake, noStat(5001, "ENOENT")).tokenHolders("h1", prev), [], "a process that vanished while its stat was read is no reader");
  const goodStat = fake.files.get(`${PROC}/5001/stat`);
  fake.files.set(`${PROC}/5001/stat`, "5001 (x) S garbage\n");
  assert.throws(() => tokens(5001), /5001 .* its start time cannot be read \(cannot read the start time/, "a malformed stat is unknown");
  fake.files.set(`${PROC}/5001/stat`, goodStat);
  assert.throws(() => linuxProbe(fake, deny(fake, "readFile", /^\/fakeproc\/stat$|\/5001\/environ$/)).tokenHolders("h1", prev), /5001 .* its start time cannot be read/, "no boot time: unknown");
  // The same rule in the snapshot-use net, for each reader.
  for (const link of ["cwd", "root", "exe"]) {
    const users = (pid, skip = prev) => linuxProbe(fake, noLink(pid, link)).snapshotUsers(SNAP, skip);
    assert.deepEqual(users(5001), [], `${link}: older is skipped`);
    assert.throws(() => users(5002), /5002 \(a process of this user[^)]*\) is unreadable \(EACCES\) and it started at/, `${link}: newer is held`);
    assert.throws(() => users(5006), /5006 .* not before the baseline/, `${link}: between is held`);
    assert.throws(() => users(5001, null), /5001 .* there is no baseline for this ownership/, `${link}: no marker is held`);
  }
  const mapsDenied = (pid) => linuxProbe(fake, deny(fake, "readFile", new RegExp(`/${pid}/maps$`))).snapshotUsers(SNAP, prev);
  assert.deepEqual(mapsDenied(5001), []); assert.throws(() => mapsDenied(5002), /5002 .* not before the baseline/);
  assert.throws(() => linuxProbe(fake, noStat(5001), noLink(5001, "cwd")).snapshotUsers(SNAP, prev), /5001 .* its start time cannot be read/);
  // An older process that is readable is judged by what it reads, not skipped for being old.
  fake.links.set(`${PROC}/5004/cwd`, `${SNAP}/src`);
  assert.deepEqual(linuxProbe(fake).snapshotUsers(SNAP, prev), [{ pid: 5004, how: "working directory" }]);
});

test("ownership POSIX [injected /proc]: an unreadable process keeps the host held and the reclaim message names why; only an older one than the last clean release's acquisition is cleared", (t) => {
  const dir = tmp(t, "rk-own-");
  const file = path.join(dir, "owner.json");
  const rec = deadOwner(file, "linux", { snapshot: "/snap/k" });
  const fake = fakeProc({ 4001: { start: NOW - 10_000 }, 4002: { cwd: "/snap/k", start: NOW - 10_000 } });
  const asProbe = (...overs) => { const p = linuxProbe(fake, ...overs); return fakeProbe("linux", { tokenHolders: (h, skipBefore) => p.tokenHolders(h, skipBefore), snapshotUsers: (d, skipBefore) => p.snapshotUsers(d, skipBefore) }); };
  assert.throws(() => acquire(file, "linux", asProbe(deny(fake, "readFile", /\/4001\/environ$/))), /owner pid 999999 is gone but processes carrying RIGHTKIT_BUILD_OWNERSHIP cannot be listed \(.*4001 \(a process of this user[^)]*\) is unreadable \(EACCES\)/);
  assert.throws(() => acquire(file, "linux", asProbe(deny(fake, "readlink", /\/4001\/exe$/))), /processes using the snapshot cannot be listed \(.*4001 \(a process of this user[^)]*\) is unreadable/);
  assert.throws(() => acquire(file, "linux", asProbe()), /process\(es\) 4002 \(working directory\) still use the snapshot \/snap\/k/);
  fake.pids.delete("4002");
  const own = acquire(file, "linux", asProbe());
  assert.equal(own.reclaimed.from, rec.handle);
  assert.match(own.reclaimed.proofs.join("; "), /no process has a working directory, executable, open or mapped file or argument inside or naming \/snap\/k/);
  // The unreadable process is old (an agent), and the dead owner's record names the last clean release's acquisition (cleanBefore).
  const unreadableAgent = [deny(fake, "readFile", /\/4001\/environ$/), deny(fake, "readlink", /\/4001\/(cwd|exe)$/)];
  fake.files.set(`${PROC}/4001/stat`, `4001 (agent) S 1 4001 4001 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 ${Math.round((NOW - 3_600_000 - BTIME * 1000) / 10)} 0 0\n`);
  deadOwner(file, "linux", { snapshot: "/snap/k" });
  assert.throws(() => acquire(file, "linux", asProbe(...unreadableAgent)), /4001 .* there is no baseline for this ownership/, "(d) no clean-release marker on the record: held however old");
  deadOwner(file, "linux", { snapshot: "/snap/k", cleanBefore: iso(NOW - 600_000) });
  assert.ok(acquire(file, "linux", asProbe(...unreadableAgent)).reclaimed, "(a) older than the last clean release's acquisition: cleared");
  fake.files.set(`${PROC}/4001/stat`, `4001 (agent) S 1 4001 4001 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 ${Math.round((NOW - 400_000 - BTIME * 1000) / 10)} 0 0\n`);
  deadOwner(file, "linux", { snapshot: "/snap/k", cleanBefore: iso(NOW - 600_000) });
  assert.throws(() => acquire(file, "linux", asProbe(...unreadableAgent)), /4001 .* not before the baseline/, "(b) started after the previous acquisition, before this ownership: held");
});

test("ownership: a clean release leaves a durable marker of its acquisition; the next record on a free host carries it; a held release leaves none; a reclaim never has it", (t) => {
  const file = path.join(tmp(t, "rk-own-"), "build-ownership", "owner.json");
  let carriers = [];
  const probe = () => fakeProbe("linux", { tokenHolders: () => carriers, snapshotUsers: () => [] });
  const cleanBefore = () => JSON.parse(fs.readFileSync(file, "utf8")).cleanBefore;
  const first = acquire(file, "linux", probe());
  assert.equal(cleanBefore(), null, "(d) no marker yet: the record carries none");
  assert.equal(readCleanMarker(file), null);
  assert.equal(first.release().released, true);
  assert.equal(readCleanMarker(file), first.owner.acquiredAt, "a clean release leaves the marker: its own acquisition");
  assert.equal(fs.existsSync(cleanMarkerFile(file)), true); assert.equal(fs.existsSync(file), false);
  const second = acquire(file, "linux", probe());
  assert.equal(cleanBefore(), first.owner.acquiredAt, "the next record on the free host carries the last clean release's acquisition");
  // (c) a release that ends held leaves the marker as it was (no newer one) and the record, which only a reclaim can clear.
  carriers = [4242];
  assert.equal(second.release().released, false);
  assert.equal(readCleanMarker(file), first.owner.acquiredAt, "a held release writes no marker");
  assert.equal(fs.existsSync(file), true);
  carriers = [];
  // (c) a reclaim (the previous release was not clean) never carries the marker, whatever the file says.
  deadOwner(file, "linux");
  const reclaimed = acquire(file, "linux", probe());
  assert.ok(reclaimed.reclaimed); assert.equal(cleanBefore(), null, "a reclaimed record carries no marker");
  assert.equal(reclaimed.release().released, true);
  assert.equal(readCleanMarker(file), reclaimed.owner.acquiredAt);
  // A missing, unparseable or foreign marker is no marker.
  for (const body of ["not json", JSON.stringify({ schema: "other", acquiredAt: iso(NOW) }), JSON.stringify({ schema: CLEAN_SCHEMA, acquiredAt: "nope" })]) {
    fs.writeFileSync(cleanMarkerFile(file), body);
    assert.equal(readCleanMarker(file), null, body);
    const own = acquire(file, "linux", probe()); assert.equal(cleanBefore(), null); own.release();
  }
});

test("hostBootId, recordRootBirth, readRootBirth: a birth is written once, and read back only for the boot it happened in", (t) => {
  assert.equal(hostBootId({ platform: "linux", readFile: () => "3f2b9c1e-0000-4000-8000-123456789abc\n" }), "3f2b9c1e-0000-4000-8000-123456789abc");
  assert.equal(hostBootId({ platform: "linux", readFile: () => "garbage" }), null);
  assert.equal(hostBootId({ platform: "linux", readFile: () => { throw new Error("no proc"); } }), null);
  assert.equal(hostBootId({ platform: "darwin", spawn: () => ({ status: 0, stdout: "{ sec = 1700000000, usec = 5 } Tue Nov 14 22:13:20 2023\n" }) }), "1700000000.5");
  assert.equal(hostBootId({ platform: "darwin", spawn: () => ({ status: 1, stdout: "" }) }), null);
  assert.equal(hostBootId({ platform: "win32" }), null);
  const root = path.join(tmp(t, "rk-own-birth-"), "snapshots"); fs.mkdirSync(root);
  assert.equal(readRootBirth(root, "b1"), null, "no record: no birth");
  assert.equal(recordRootBirth(root, { now: () => NOW - 5000, bootId: "b1" }), true);
  assert.equal(recordRootBirth(root, { now: () => NOW, bootId: "b1" }), false, "never over an existing birth");
  const rec = JSON.parse(fs.readFileSync(rootBirthFile(root), "utf8"));
  assert.deepEqual(rec, { schema: ROOT_BIRTH_SCHEMA, createdAt: iso(NOW - 5000), bootId: "b1" });
  assert.equal(readRootBirth(root, "b1"), NOW - 5000);
  assert.equal(readRootBirth(root, "b2"), null, "another boot: no baseline"); assert.equal(readRootBirth(root, null), null); assert.equal(readRootBirth(root, ""), null);
  for (const body of ["not json", JSON.stringify({ ...rec, schema: "x" }), JSON.stringify({ ...rec, createdAt: "nope" }), JSON.stringify({ ...rec, bootId: null })]) { fs.writeFileSync(rootBirthFile(root), body); assert.equal(readRootBirth(root, "b1"), null, body); }
  // A boot id that cannot be read writes a record nothing can match.
  fs.rmSync(rootBirthFile(root)); assert.equal(recordRootBirth(root, { bootId: null }), true); assert.equal(readRootBirth(root, "b1"), null);
});

test("snapshot sync: a root it creates is born (record with the boot), an existing root keeps what it had", (t) => {
  const git = (cwd, ...args) => { const r = spawnSync("git", args, { cwd, encoding: "utf8", env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t" } }); if (r.status !== 0) throw new Error(r.stderr); return r.stdout.trim(); };
  const repo = tmp(t, "rk-own-birth-repo-"); const root = path.join(tmp(t, "rk-own-birth-root-"), "snapshots");
  git(repo, "init", "-q", "-b", "main"); fs.writeFileSync(path.join(repo, "a.txt"), "one\n"); git(repo, "add", "-A"); git(repo, "commit", "-q", "-m", "a");
  const before = Date.now();
  syncSnapshot({ checkout: repo, root });
  const born = JSON.parse(fs.readFileSync(rootBirthFile(root), "utf8"));
  assert.equal(born.schema, ROOT_BIRTH_SCHEMA); assert.equal(born.bootId, hostBootId());
  assert.ok(Date.parse(born.createdAt) >= before - 1000 && Date.parse(born.createdAt) <= Date.now() + 1000, born.createdAt);
  // A second sync into the existing root leaves its birth alone; a root without a birth is not given one later (no baseline).
  syncSnapshot({ checkout: repo, root });
  assert.deepEqual(JSON.parse(fs.readFileSync(rootBirthFile(root), "utf8")), born);
  fs.rmSync(rootBirthFile(root));
  syncSnapshot({ checkout: repo, root });
  assert.equal(fs.existsSync(rootBirthFile(root)), false);
  // A deleted and recreated root is born again.
  fs.rmSync(root, { recursive: true, force: true });
  syncSnapshot({ checkout: repo, root });
  assert.equal(fs.existsSync(rootBirthFile(root)), true);
});

test("ownership POSIX [injected /proc]: the snapshot root's birth is the baseline: a pre-birth unreadable process (ssh-agent) lets the first release clean; post-birth, no birth record, another boot all hold; a recreated root is born again", (t) => {
  const base = tmp(t, "rk-own-"); const root = path.join(base, "snapshots"); const snap = path.join(root, "k"); const file = path.join(base, "build-ownership", "owner.json");
  const fake = fakeProc({ 6001: { start: NOW - 3_600_000 }, 6002: { start: NOW + 60_000 } });   // an agent from long before; a process from after the ownership
  const probe = (bootId = "boot-1", pids = [6001, 6002]) => {
    const p = linuxProbe(fake, deny(fake, "readFile", new RegExp(`/(${pids.join("|")})/environ$`)), deny(fake, "readlink", new RegExp(`/(${pids.join("|")})/(cwd|root|exe)$`)));
    return fakeProbe("linux", { bootId: () => bootId, tokenHolders: (h, s) => p.tokenHolders(h, s), snapshotUsers: (d, s) => p.snapshotUsers(d, s) });
  };
  const born = () => recordRootBirth(root, { now: () => Date.now(), bootId: "boot-1" }); // what the sync does, after the ownership began
  const fresh = (pr) => { fs.rmSync(file, { force: true }); fs.rmSync(cleanMarkerFile(file), { force: true }); fs.rmSync(root, { recursive: true, force: true }); fs.mkdirSync(snap, { recursive: true }); return acquire(file, "linux", pr, { snapshot: snap }); };
  // Post-birth: the process from after the ownership is held, the agent from before is not.
  let own = fresh(probe()); born();
  let r = own.release();
  assert.equal(r.released, false); assert.match(r.why, /6002 \(a process of this user[^)]*\) is unreadable \(EACCES\) and it started at .* not before the baseline/); assert.doesNotMatch(r.why, /6001/);
  // No birth record: strict, nothing skipped.
  own = fresh(probe());
  r = own.release();
  assert.equal(r.released, false); assert.match(r.why, /6001 .* there is no baseline for this ownership/);
  // A birth of another boot: no baseline.
  own = fresh(probe("boot-2")); born();
  r = own.release();
  assert.equal(r.released, false); assert.match(r.why, /6001 .* there is no baseline for this ownership/);
  // A birth whose boot id cannot be read now: no baseline.
  own = fresh(probe(null)); born();
  assert.match(own.release().why, /there is no baseline/);
  // The first acquisition with only the pre-birth agent: released clean (and the marker is left for the next owner).
  own = fresh(probe("boot-1", [6001])); born();
  r = own.release();
  assert.equal(r.released, true, JSON.stringify(r)); assert.equal(readCleanMarker(file), own.owner.acquiredAt);
  // Newer than the birth but the birth is capped at this ownership's acquisition: a process that started after the acquisition
  // and before the birth (the gate's own, say) is not baseline.
  fake.files.set(`${PROC}/6001/stat`, `6001 (x) S 1 6001 6001 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 ${Math.round((Date.now() + 10_000 - BTIME * 1000) / 10)} 0 0\n`);
  own = fresh(probe("boot-1", [6001]));
  recordRootBirth(root, { now: () => Date.now() + 60_000, bootId: "boot-1" });
  assert.match(own.release().why, /6001 .* not before the baseline/, "started after the acquisition, before the birth: held");
  fake.files.set(`${PROC}/6001/stat`, `6001 (x) S 1 6001 6001 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 ${Math.round((NOW - 3_600_000 - BTIME * 1000) / 10)} 0 0\n`);
  // A deleted and recreated root is born again, later.
  fs.rmSync(file, { force: true }); fs.rmSync(root, { recursive: true, force: true }); fs.mkdirSync(root, { recursive: true });
  recordRootBirth(root, { now: () => NOW - 7_200_000, bootId: "boot-1" });
  assert.equal(readRootBirth(root, "boot-1"), NOW - 7_200_000);
  fs.rmSync(root, { recursive: true }); fs.mkdirSync(root, { recursive: true });
  assert.equal(readRootBirth(root, "boot-1"), null, "the new root has no birth until it is recorded");
  assert.equal(recordRootBirth(root, { now: () => NOW - 60_000, bootId: "boot-1" }), true);
  assert.equal(readRootBirth(root, "boot-1"), NOW - 60_000, "a new birth");
});

test("ownership POSIX [injected table]: an orphan a subreaper adopted is still the owner's work; only a recorded parent or a group-mate makes a peer", (t) => {
  const file = path.join(tmp(t, "rk-own-"), "owner.json");
  const me = { pid: process.pid, ppid: 5000, startedMs: NOW - 1_000 };
  const shell = { pid: 5000, ppid: 1, startedMs: NOW - 500_000, group: 777 };
  const subreaper = { pid: 4000, ppid: 1, startedMs: NOW - 900_000, group: 4000 };
  const orphan = { pid: 5001, ppid: 4000, startedMs: NOW - 50_000, group: 777 };
  const probe = (rows, own = [process.pid]) => fakeProbe("linux", { groupSnapshot: () => group(rows, own) });
  const held = (record, rows, own, re) => { deadOwner(file, "linux", record); assert.throws(() => acquire(file, "linux", probe(rows, own)), re); };
  const clear = (record, rows, own) => { deadOwner(file, "linux", record); return acquire(file, "linux", probe(rows, own)); };
  // (c) the subreaper (systemd --user, WSL's init) adopted the orphan: its parent predates the ownership and is no peer.
  held({}, [subreaper, orphan], undefined, /group 777 still has 1 member\(s\) started under this ownership: 5001/);
  // ...even when the subreaper is also an ancestor of the asking gate (it usually is).
  held({}, [subreaper, orphan], [process.pid, 4000], /5001/);
  // ...and when it was started within the ownership's slack, the walk goes on to ITS parent and still ends at a non-peer.
  held({}, [{ ...subreaper, startedMs: NOW - 100_500, ppid: 3000 }, { pid: 3000, ppid: 1, startedMs: NOW - 800_000 }, orphan], undefined, /5001/);
  // A peer: the owner's recorded parent (`gate | tee`), or a parent that shares the member's group (a script shell, no job control).
  const tee = { pid: 5003, ppid: 5000, startedMs: NOW - 1_000, group: 777 };
  assert.equal(clear({}, [shell, tee]).reclaimed.pid, 999_999);
  assert.equal(clear({ ancestors: [] }, [shell, tee]).reclaimed.pid, 999_999, "the shell shares the member's group");
  held({ ancestors: [], }, [{ ...shell, group: 1 }, tee], undefined, /5003/);
  held({ ancestors: null }, [{ ...shell, group: 1 }, tee], undefined, /5003/);
  held({ ancestors: [{ pid: 5000, startedMs: NOW - 1_000 }] }, [{ ...shell, group: 1 }, tee], undefined, /5003/);
  // A record written without ancestors cannot tell a peer from an adopted orphan, so it is covered.
  held({ ancestors: undefined }, [{ ...shell, group: 1 }, tee], undefined, /5003/);
  // The owner's recorded ancestors are never its children, even when they started inside the slack before the acquisition.
  const youngShell = { pid: 5010, ppid: 4000, startedMs: NOW - 100_500, group: 777 };
  assert.equal(clear({ ancestors: [{ pid: 5010, startedMs: NOW - 100_500 }] }, [subreaper, youngShell]).reclaimed.pid, 999_999);
  held({ ancestors: [{ pid: 5010, startedMs: NOW - 100_500 }] }, [subreaper, { ...youngShell, pid: 5011 }], undefined, /5011/);
  // The asking gate's own children are not the owner's work, whoever adopted whom.
  const mine = { pid: 5006, ppid: process.pid, startedMs: NOW - 500, group: 777 };
  assert.equal(clear({}, [shell, me, mine]).reclaimed.pid, 999_999);
  // An orphan under pid 1, and a grandchild of the owner's orphan, still count.
  held({}, [{ ...orphan, ppid: 1 }, { pid: 5002, ppid: 5001, startedMs: NOW - 40_000, group: 777 }], undefined, /5001, 5002/);
});

test("brokerLeaseState: only a well-formed rustright.snapshot-lease-status.v1 answer counts; everything else is UNKNOWN", () => {
  const answer = (stdout, status = 0) => () => ({ status, stdout, stderr: status ? "rustright: unknown command snapshot-lease" : "" });
  const q = { snapshot: "/s", handle: "h", env: {}, platform: "darwin", client: "/x/rustright" };
  assert.equal(brokerLeaseState({ ...q, client: null }).state, "UNKNOWN", "no client");
  assert.equal(brokerLeaseState({ ...q, spawn: answer("", 2) }).state, "UNKNOWN");
  assert.equal(brokerLeaseState({ ...q, spawn: answer('{"schema":"other","state":"REAPED"}') }).state, "UNKNOWN");
  assert.equal(brokerLeaseState({ ...q, spawn: answer('{"schema":"rustright.snapshot-lease-status.v1","state":"GONE"}') }).state, "UNKNOWN");
  let argv;
  const spawnSpy = (cmd, args) => { argv = [cmd, ...args]; return { status: 0, stdout: 'log line\n{"schema":"rustright.snapshot-lease-status.v1","state":"REAPED"}\n' }; };
  assert.equal(brokerLeaseState({ ...q, spawn: spawnSpy }).state, "REAPED");
  assert.deepEqual(argv, ["/x/rustright", "snapshot-lease", "status", "--snapshot", "/s", "--ownership", "h"]);
});

// ---- Real processes: a gate killed while its child survives. ----

const MODULE = pathToFileURL(path.resolve("build-ownership.mjs")).href;
// Linux only: a subreaper (PR_SET_CHILD_SUBREAPER = 36; the setting survives nothing but its own process) that starts `sh` in a session
// of its own, which runs the gate and then sleeps. When the gate is killed, its orphaned children are adopted by THIS process (the
// subreaper, an ancestor of the gate that predates the ownership), as systemd --user and WSL's init adopt them.
const SUBREAPER_PY = [
  "import ctypes, os, subprocess, sys, time",
  "libc = ctypes.CDLL(None, use_errno=True)",
  "if libc.prctl(36, 1, 0, 0, 0) != 0: sys.exit(3)",
  "subprocess.Popen(['sh', '-c', '\"$0\" \"$1\"; exec sleep 600', sys.argv[1], sys.argv[2]], start_new_session=True)",
  "while True:",
  "    try: os.waitpid(-1, 0)",
  "    except ChildProcessError: time.sleep(0.2)",
].join("\n");
const HAS_SUBREAPER = process.platform === "linux" && spawnSync("python3", ["-c", "import ctypes"], { stdio: "ignore" }).status === 0;

/** Starts a fake gate that takes ownership of `file` and starts one child that outlives it; resolves with their pids. */
async function crashingGate(t, file, { detachChild = process.platform === "win32", mode = "plain", snapshot = null, viaNode = false, subreaper = false } = {}) {
  const script = path.join(path.dirname(file), "fake-gate.mjs");
  const SCANS = pathToFileURL(path.resolve("deterministic-scans.mjs")).href;
  // plain: a child of the gate (detached on request). scan: the real class A spawner (detached, its own group) with the
  // ownership variable scrubbed and its cwd outside any snapshot, so only the detached-child record can hold the host.
  // platform-binary: /bin/sleep started detached in the snapshot with an empty environment: only the working-directory net sees it.
  fs.writeFileSync(script, `
const { acquireBuildOwnership } = await import(${JSON.stringify(MODULE)});
const own = acquireBuildOwnership({ file: ${JSON.stringify(file)}, stage: "fixture", snapshot: ${JSON.stringify(snapshot)} });
const { spawn } = await import("node:child_process");
let pid;
if (${JSON.stringify(mode)} === "scan") {
  const { defaultSpawnAsync } = await import(${JSON.stringify(SCANS)});
  const env = { ...process.env }; delete env.${OWNERSHIP_ENV};
  defaultSpawnAsync(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { signal: new AbortController().signal, env, cwd: ${JSON.stringify(os.tmpdir())}, onSpawn: (p) => { pid = p; } });
} else if (${JSON.stringify(mode)} === "scrubbed") {
  // Same group as the gate, variable scrubbed, cwd elsewhere: only the group net (and the orphan's ancestry) can see it.
  const env = { ...process.env }; delete env.${OWNERSHIP_ENV}; delete env.RIGHTKIT_BUILD_OWNERSHIP_FILE;
  const c = spawn(process.execPath, ["-e", "process.chdir('/'); setInterval(() => {}, 1000)"], { stdio: "ignore", env, cwd: "/" }); c.unref(); pid = c.pid;
} else if (${JSON.stringify(mode)} === "escaped" || ${JSON.stringify(mode)} === "escaped-argv") {
  // A descendant that started its own session (detached), dropped the variable, runs at "/" and, in "escaped", holds a file of the
  // snapshot open (in "escaped-argv", only names the snapshot in an argument). The raw spawn records no group: only the snapshot-use
  // net sees it. It reports ready once it holds the file.
  const fs = await import("node:fs");
  const held = ${JSON.stringify(snapshot)} + "/held.txt"; fs.writeFileSync(held, "x");
  const ready = ${JSON.stringify(file)} + ".ready"; fs.rmSync(ready, { force: true });
  const env = { ...process.env, HELD_FILE: held, READY_FILE: ready }; delete env.${OWNERSHIP_ENV}; delete env.RIGHTKIT_BUILD_OWNERSHIP_FILE;
  const code = ${JSON.stringify(mode)} === "escaped"
    ? "const fs = require('fs'); process.chdir('/'); fs.openSync(process.env.HELD_FILE, 'r'); fs.writeFileSync(process.env.READY_FILE, '1'); setInterval(() => {}, 1000)"
    : "process.chdir('/'); require('fs').writeFileSync(process.env.READY_FILE, '1'); setInterval(() => {}, 1000)";
  const args = ["-e", code]; if (${JSON.stringify(mode)} === "escaped-argv") args.push(${JSON.stringify(snapshot)} + "/src/lib.rs");
  const c = spawn(process.execPath, args, { stdio: "ignore", detached: true, env, cwd: "/" }); c.unref(); pid = c.pid;
  while (!fs.existsSync(ready)) await new Promise((r) => setTimeout(r, 50));
} else if (${JSON.stringify(mode)} === "platform-binary") {
  const c = spawn("/bin/sleep", ["60"], { stdio: "ignore", detached: true, env: {}, cwd: ${JSON.stringify(snapshot)} }); c.unref(); pid = c.pid;
} else {
  const c = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { stdio: "ignore", detached: ${detachChild} }); c.unref(); pid = c.pid;
}
(await import("node:fs")).writeFileSync(${JSON.stringify(file)} + ".gate-info", JSON.stringify({ handle: own.handle, gate: process.pid, ppid: process.ppid, child: pid, contained: own.contained, job: own.owner.containment.name ?? null }));
setInterval(() => {}, 1000);
`);
  const env = { ...process.env }; delete env[OWNERSHIP_ENV]; delete env.NODE_OPTIONS;
  // POSIX: the fake gate leads its own process group, as a gate started from a shell leads its job's. Windows: started the way a
  // shell starts it (cmd start), outside the kill-on-close job Node keeps for its own children; viaNode starts it from Node.
  const infoFile = `${file}.gate-info`; fs.rmSync(infoFile, { force: true });
  const gate = subreaper
    ? spawn("python3", ["-c", SUBREAPER_PY, process.execPath, script], { stdio: ["ignore", "ignore", "inherit"], env, detached: true })
    : process.platform === "win32" && !viaNode
    ? spawn("cmd.exe", ["/d", "/c", "start", "\"\"", "/b", "/wait", process.execPath, script], { stdio: "ignore", env })
    : spawn(process.execPath, [script], { stdio: ["ignore", "ignore", "inherit"], env, detached: process.platform !== "win32" });
  let exited = null; gate.on("exit", (code) => { exited = code; });
  await until(() => fs.existsSync(infoFile) || exited !== null, 90_000);
  if (!fs.existsSync(infoFile)) throw new Error(`fake gate exited ${exited} before reporting`);
  const info = JSON.parse(fs.readFileSync(infoFile, "utf8"));
  t.after(() => { for (const pid of [info.gate, info.child]) { try { process.kill(pid, "SIGKILL"); } catch { /* gone */ } } });
  if (subreaper) t.after(() => { try { gate.kill("SIGKILL"); } catch { /* gone */ } try { process.kill(-info.ppid, "SIGKILL"); } catch { /* gone */ } });
  return { ...info, ownChild: info.child, kill: async (pid) => { try { process.kill(pid, "SIGKILL"); } catch { /* gone */ } for (let i = 0; i < 200; i++) { try { process.kill(pid, 0); } catch { return; } await new Promise((r) => setTimeout(r, 50)); } throw new Error(`pid ${pid} survived SIGKILL`); } };
}
/**
 * Windows: every descendant a gate starts through Node lands in Node's kill-on-close job, nested under the gate's job (which allows no
 * breakaway), so it dies with the gate (measured on the Dell, 2026-10-29). A member that outlives the gate is made here: a sleeper
 * started outside the gate is put into its job by name. Returns the sleeper's pid.
 */
function survivorInJob(t, jobName) {
  const sleeper = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { stdio: "ignore", detached: true });
  sleeper.unref();
  t.after(() => { try { process.kill(sleeper.pid, "SIGKILL"); } catch { /* gone */ } });
  const script = [
    "$ErrorActionPreference = 'Stop'",
    'Add-Type @"',
    "using System; using System.Runtime.InteropServices;",
    "public static class RkJobT {",
    '  [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)] public static extern IntPtr OpenJobObjectW(uint access, bool inherit, string name);',
    '  [DllImport("kernel32.dll", SetLastError=true)] public static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);',
    '  [DllImport("kernel32.dll", SetLastError=true)] public static extern bool AssignProcessToJobObject(IntPtr job, IntPtr proc);',
    "}",
    '"@',
    `$j = [RkJobT]::OpenJobObjectW(0x0001, $false, '${jobName}'); if ($j -eq [IntPtr]::Zero) { throw "open job failed" }`,
    `$p = [RkJobT]::OpenProcess(0x0101, $false, [uint32]${sleeper.pid}); if (-not [RkJobT]::AssignProcessToJobObject($j, $p)) { throw "assign failed" }`,
    "'ASSIGNED'",
  ].join("\n");
  const r = spawnSync("powershell.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", Buffer.from(script, "utf16le").toString("base64")], { encoding: "utf8", timeout: 60_000 });
  if (!/ASSIGNED/.test(String(r.stdout))) throw new Error(`could not put a survivor into ${jobName}: ${r.stderr}`);
  return sleeper.pid;
}

/**
 * One gate step in a process of its own, as a real gate is one process (and on Windows a process that joined a job never leaves it,
 * so acquiring in this test process would leave it, and every later child, inside that job). Returns the step's JSON line.
 */
function gateStep(t, kind, args, env) {
  const SNAP = pathToFileURL(path.resolve("snapshot.mjs")).href;
  const driver = path.join(fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-own-step-"))), "step.mjs");
  t.after(() => fs.rmSync(path.dirname(driver), { recursive: true, force: true }));
  fs.writeFileSync(driver, `
const a = ${JSON.stringify(args)};
if (${JSON.stringify(kind)} === "acquire") {
  const { acquireBuildOwnership } = await import(${JSON.stringify(MODULE)});
  try { const own = acquireBuildOwnership({ file: a.file, stage: "next", env: process.env }); const released = own.release(); console.log(JSON.stringify({ ok: true, reclaimed: own.reclaimed, released })); }
  catch (e) { console.log(JSON.stringify({ ok: false, code: e.code, message: e.message })); }
} else {
  const { snapshotCommand } = await import(${JSON.stringify(SNAP)});
  const lines = [];
  const code = snapshotCommand({ cwd: a.cwd, env: process.env, platform: process.platform, out: (m) => lines.push(m), err: (m) => lines.push(m) }, a.args);
  console.log(JSON.stringify({ code, lines }));
}
process.exit(0);
`);
  const childEnv = { ...env, NODE_OPTIONS: "" }; delete childEnv[OWNERSHIP_ENV]; delete childEnv.RIGHTKIT_BUILD_OWNERSHIP_FILE;
  const r = spawnSync(process.execPath, [driver], { env: childEnv, encoding: "utf8", timeout: 120_000 });
  try { return JSON.parse(String(r.stdout).trim().split(/\r?\n/).pop()); } catch { return { stepFailed: true, status: r.status, stdout: r.stdout, stderr: r.stderr }; }
}
async function until(fn, ms = 20_000) { const end = Date.now() + ms; for (;;) { if (fn()) return; if (Date.now() > end) throw new Error("timed out"); await new Promise((r) => setTimeout(r, 100)); } }

test("[native] G1 Windows: a live gate's grandchild (node -> long-lived exe) is in the gate's job, and release stays held until it is gone", { skip: process.platform !== "win32" && "Windows job containment", timeout: 180_000 }, async (t) => {
  // Review of 9565fff9a: if Node's SILENT_BREAKAWAY let a grandchild leave the gate's job, the holder's fresh count would miss it
  // and release could clear with cargo still running. Measured here on the real holder: gate -> node child -> ping.exe.
  const dir = tmp(t, "rk-own-grand-");
  const file = path.join(dir, "build-ownership", "owner.json");
  const out = path.join(dir, "measure.json");
  const script = path.join(dir, "grand-gate.mjs");
  fs.writeFileSync(script, `
const fs = await import("node:fs");
const { spawn, spawnSync } = await import("node:child_process");
const { acquireBuildOwnership } = await import(${JSON.stringify(MODULE)});
const r = { };
try {
  const own = acquireBuildOwnership({ file: ${JSON.stringify(file)}, stage: "fixture" });
  r.contained = own.contained; r.job = own.owner.containment.name;
  const child = spawn(process.execPath, ["-e", "const c = require('child_process').spawn('ping.exe', ['-n', '120', '127.0.0.1'], { stdio: 'ignore' }); c.on('error', (e) => { process.stderr.write(String(e)); process.exit(3); }); process.stdout.write(String(c.pid)); setInterval(() => {}, 1000);"], { stdio: ["ignore", "pipe", "pipe"] });
  r.child = child.pid;
  let childErr = ""; child.stderr.on("data", (d) => { childErr += d; });
  r.grandchild = await new Promise((res, rej) => { child.stdout.once("data", (d) => res(Number(String(d).trim()))); child.once("exit", (code) => rej(new Error("the node child exited " + code + " before reporting: " + childErr))); setTimeout(() => rej(new Error("the node child did not report within 30 s: " + childErr)), 30000); });
  const ps = [
    "$ErrorActionPreference = 'Stop'",
    'Add-Type @"',
    "using System; using System.Runtime.InteropServices;",
    "public static class RkJobG {",
    '  [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)] public static extern IntPtr OpenJobObjectW(uint access, bool inherit, string name);',
    '  [DllImport("kernel32.dll", SetLastError=true)] public static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);',
    '  [DllImport("kernel32.dll", SetLastError=true)] public static extern bool IsProcessInJob(IntPtr proc, IntPtr job, out bool result);',
    "}",
    '"@',
    "$j = [RkJobG]::OpenJobObjectW(0x0004, $false, '" + r.job + "'); if ($j -eq [IntPtr]::Zero) { 'ERR:job'; exit 0 }",
    "foreach ($id in " + r.child + "," + r.grandchild + ") { $p = [RkJobG]::OpenProcess(0x1000, $false, [uint32]$id); $in = $false; if ($p -eq [IntPtr]::Zero -or -not [RkJobG]::IsProcessInJob($p, $j, [ref]$in)) { [string]$id + '=ERR' } elseif ($in) { [string]$id + '=IN' } else { [string]$id + '=OUT' } }",
  ].join("\\n");
  const m = spawnSync("powershell.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", Buffer.from(ps, "utf16le").toString("base64")], { encoding: "utf8", timeout: 60000 });
  r.inJob = String(m.stdout).trim().split(/\\r?\\n/);
  r.releaseWhileAlive = own.release();
  spawnSync("taskkill.exe", ["/T", "/F", "/PID", String(child.pid)], { stdio: "ignore" });
  for (let i = 0; i < 100; i++) { try { process.kill(r.grandchild, 0); await new Promise((res) => setTimeout(res, 100)); } catch { break; } }
  r.releaseAfter = own.release();
} catch (e) { r.error = e.message; }
fs.writeFileSync(${JSON.stringify(out)}, JSON.stringify(r));
process.exit(0);
`);
  const env = { ...process.env }; delete env[OWNERSHIP_ENV]; delete env.NODE_OPTIONS;
  // Started the way a shell starts a gate (cmd start), as in crashingGate.
  const gate = spawn("cmd.exe", ["/d", "/c", "start", "\"\"", "/b", "/wait", process.execPath, script], { stdio: "ignore", env });
  t.after(() => { try { gate.kill(); } catch { /* gone */ } });
  await until(() => fs.existsSync(out), 150_000);
  const r = JSON.parse(fs.readFileSync(out, "utf8"));
  t.diagnostic(`measurement: ${JSON.stringify(r)}`);
  assert.equal(r.error, undefined, r.error);
  assert.equal(r.contained, true);
  assert.deepEqual(r.inJob, [`${r.child}=IN`, `${r.grandchild}=IN`], "the node child and its long-lived grandchild are both in the gate's job");
  assert.equal(r.releaseWhileAlive.released, false, "release stays held while the grandchild runs");
  assert.match(r.releaseWhileAlive.why, /still holds \d+ process\(es\) \(this gate is one\)/);
  assert.equal(r.releaseAfter.released, true, `released once the grandchild is gone (${r.releaseAfter.why})`);
  assert.match(r.releaseAfter.proofs.join("; "), /holds only this gate \(holder \d+ counted it at .* for query [0-9a-f]{32}\)/);
});

test("[native] G1: a gate killed while its child still runs keeps the host owned until the child is gone, then the next gate reclaims it with the proof", { timeout: 120_000 }, async (t) => {
  const file = path.join(tmp(t, "rk-own-native-"), "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const g = await crashingGate(t, file);
  if (process.platform === "win32") {
    // Windows (measured on the Dell, D:\\scratch\\rk-step4\\dell-job-death-experiment.log): when a contained gate dies, every member
    // of its job dies with it, its own children and a process put into the job from outside alike, so no member outlives it and the
    // busy-while-a-member-lives path cannot be produced natively here (the unit tests hold it). What is proven natively: the members
    // die, the job drains, and the next gate reclaims on the holder's drained record.
    assert.equal(g.contained, true, "the self-check found this gate's children in its job");
    const outsider = survivorInJob(t, g.job);
    await g.kill(g.gate);
    for (const pid of [g.ownChild, outsider]) await until(() => { try { process.kill(pid, 0); return false; } catch { return true; } }, 10_000);
    await until(() => readHolder(holderFile(file, g.handle))?.state === "drained");
    const next = gateStep(t, "acquire", { file }, process.env);
    assert.equal(next.ok, true, JSON.stringify(next));
    assert.match(next.reclaimed.proofs.join("; "), /drained \(holder pid/);
    assert.equal(next.released.released, true, JSON.stringify(next.released));
    return;
  }
  await g.kill(g.gate);
  // The next gate is a process of its own.
  const busy = gateStep(t, "acquire", { file }, process.env);
  // POSIX: the group net names the orphan, or (WSL2, whose per-session init adopts orphans as a subreaper) the environment net does.
  assert.equal(busy.ok, false, JSON.stringify(busy)); assert.equal(busy.code, "busy");
  assert.match(busy.message, new RegExp(process.platform === "win32" ? "still holds 1 process" : `(started under this ownership: ${g.child}|${g.child} still carry)`), "a surviving child keeps ownership");
  await g.kill(g.child);
  if (process.platform === "win32") await until(() => readHolder(holderFile(file, g.handle))?.state === "drained");
  const next = gateStep(t, "acquire", { file }, process.env);
  assert.equal(next.ok, true, JSON.stringify(next));
  assert.equal(next.reclaimed.from, g.handle);
  assert.match(next.reclaimed.proofs.join("; "), process.platform === "win32" ? /drained|holds no process/ : /has no member started under this ownership/);
  assert.equal(next.released.released, true, JSON.stringify(next.released));
});

test("[native] G1 Windows: a crashed gate's own children die with it (Node's kill-on-close job, nested under the gate's job), so its job drains and the next gate reclaims with the holder's proof", { skip: process.platform !== "win32" && "Windows jobs only", timeout: 120_000 }, async (t) => {
  const file = path.join(tmp(t, "rk-own-native-"), "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const g = await crashingGate(t, file);
  assert.equal(g.contained, true);
  await g.kill(g.gate);
  await until(() => { try { process.kill(g.ownChild, 0); return false; } catch { return true; } }, 10_000);
  await until(() => readHolder(holderFile(file, g.handle))?.state === "drained");
  const next = gateStep(t, "acquire", { file }, process.env);
  assert.equal(next.ok, true, JSON.stringify(next));
  assert.match(next.reclaimed.proofs.join("; "), /drained \(holder pid/);
});

test("[native] G1 POSIX: a child that left the gate's process group but kept its environment still holds the host (environment net)", { skip: process.platform === "win32" && "Windows children stay in the gate's job", timeout: 60_000 }, async (t) => {
  const file = path.join(tmp(t, "rk-own-native-"), "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const g = await crashingGate(t, file, { detachChild: true });
  await g.kill(g.gate);
  const env = { ...process.env }; delete env[OWNERSHIP_ENV];
  assert.throws(() => acquireBuildOwnership({ file, env, exportEnv: false }), new RegExp(`${g.child} still carry ${OWNERSHIP_ENV}`));
  await g.kill(g.child);
  assert.equal(acquireBuildOwnership({ file, env, exportEnv: false }).reclaimed.from, g.handle);
});

test("[native] G1 4b review: a class A scan child the real spawner detached (own group, variable scrubbed, cwd elsewhere) holds the host until it is gone", { skip: process.platform === "win32" && "Windows children stay in the gate's job", timeout: 60_000 }, async (t) => {
  const file = path.join(tmp(t, "rk-own-native-"), "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const g = await crashingGate(t, file, { mode: "scan" });
  await g.kill(g.gate);
  const env = { ...process.env }; delete env[OWNERSHIP_ENV];
  assert.throws(() => acquireBuildOwnership({ file, env, exportEnv: false }), new RegExp(`detached process group ${g.child} .* still has 1 member\\(s\\): ${g.child}`));
  await g.kill(g.child);
  const own = acquireBuildOwnership({ file, env, exportEnv: false });
  assert.equal(own.reclaimed.from, g.handle);
  assert.match(own.reclaimed.proofs.join("; "), /the 1 detached process group\(s\) it started are empty/);
});

test("[native] G1 POSIX: a detached Apple-style platform binary with an empty environment running inside the snapshot holds the host (working-directory net)", { skip: process.platform === "win32" && "Windows children stay in the gate's job", timeout: 60_000 }, async (t) => {
  const base = tmp(t, "rk-own-native-");
  const file = path.join(base, "build-ownership", "owner.json");
  const snapshot = path.join(base, "snapshots", "k"); fs.mkdirSync(snapshot, { recursive: true });
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const g = await crashingGate(t, file, { mode: "platform-binary", snapshot });
  await g.kill(g.gate);
  const env = { ...process.env }; delete env[OWNERSHIP_ENV];
  assert.throws(() => acquireBuildOwnership({ file, env, exportEnv: false }), new RegExp(`${g.child} \\(working directory\\) still use the snapshot`));
  await g.kill(g.child);
  assert.equal(acquireBuildOwnership({ file, env, exportEnv: false }).reclaimed.from, g.handle);
});

test("[native] review item5-r1 #4: a descendant that started its own session, dropped the variable, runs outside the snapshot and holds a file of it open still holds the host (open-file net)", { skip: process.platform === "win32" && "Windows children stay in the gate's job", timeout: 60_000 }, async (t) => {
  const base = tmp(t, "rk-own-native-");
  const file = path.join(base, "build-ownership", "owner.json");
  const snapshot = path.join(base, "snapshots", "k"); fs.mkdirSync(snapshot, { recursive: true });
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const g = await crashingGate(t, file, { mode: "escaped", snapshot });
  await g.kill(g.gate);
  const env = { ...process.env }; delete env[OWNERSHIP_ENV];
  assert.throws(() => acquireBuildOwnership({ file, env, exportEnv: false }), (e) => e instanceof OwnershipBusy && new RegExp(`${g.child} \\(open file [^)]*held\\.txt\\) still use the snapshot ${snapshot.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`).test(e.message), "not reclaimed, and the message names why");
  await g.kill(g.child);
  const own = acquireBuildOwnership({ file, env, exportEnv: false });
  assert.equal(own.reclaimed.from, g.handle);
  assert.match(own.reclaimed.proofs.join("; "), /no process has a working directory, executable, open or mapped file or argument/);
});

test("[native] review item5-r1 #4: a descendant that started its own session and dropped the variable, outside the snapshot, whose argument names the snapshot still holds the host (argument net)", { skip: process.platform === "win32" && "Windows children stay in the gate's job", timeout: 60_000 }, async (t) => {
  const base = tmp(t, "rk-own-native-");
  const file = path.join(base, "build-ownership", "owner.json");
  const snapshot = path.join(base, "snapshots", "k"); fs.mkdirSync(snapshot, { recursive: true });
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const g = await crashingGate(t, file, { mode: "escaped-argv", snapshot });
  await g.kill(g.gate);
  const env = { ...process.env }; delete env[OWNERSHIP_ENV];
  assert.throws(() => acquireBuildOwnership({ file, env, exportEnv: false }), new RegExp(`${g.child} \\(argument\\) still use the snapshot`));
  await g.kill(g.child);
  assert.equal(acquireBuildOwnership({ file, env, exportEnv: false }).reclaimed.from, g.handle);
});

test("[native] review item5-r1 #4: an orphan of the gate's group with the variable scrubbed and cwd elsewhere holds the host (group net)", { skip: process.platform === "win32" && "Windows children stay in the gate's job", timeout: 60_000 }, async (t) => {
  const file = path.join(tmp(t, "rk-own-native-"), "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const g = await crashingGate(t, file, { mode: "scrubbed" });
  await g.kill(g.gate);
  const env = { ...process.env }; delete env[OWNERSHIP_ENV];
  assert.throws(() => acquireBuildOwnership({ file, env, exportEnv: false }), new RegExp(`started under this ownership: ${g.child}`));
  await g.kill(g.child);
  assert.equal(acquireBuildOwnership({ file, env, exportEnv: false }).reclaimed.from, g.handle);
});

test("[native, Linux + python3] review item5-r1 #4: an orphan adopted by a subreaper (an ancestor that predates the ownership) is still attributed to the owner", { skip: !HAS_SUBREAPER && "needs Linux and python3 (prctl PR_SET_CHILD_SUBREAPER)", timeout: 60_000 }, async (t) => {
  const file = path.join(tmp(t, "rk-own-native-"), "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const g = await crashingGate(t, file, { mode: "scrubbed", subreaper: true });
  await g.kill(g.gate);
  // The orphan's parent is now the subreaper, not pid 1.
  const ppid = Number(/^PPid:\s+(\d+)/m.exec(fs.readFileSync(`/proc/${g.child}/status`, "utf8"))[1]);
  assert.notEqual(ppid, 1, "the subreaper adopted the orphan");
  // The next gate is a process of its own (as a real next gate is): the subreaper is then no child of the asking gate.
  const busy = gateStep(t, "acquire", { file }, process.env);
  assert.equal(busy.ok, false, JSON.stringify(busy)); assert.equal(busy.code, "busy");
  assert.match(busy.message, new RegExp(`process group \\d+ still has \\d+ member\\(s\\) started under this ownership: ${g.child}`), "attributed through the subreaper");
  await g.kill(g.child);
  const next = gateStep(t, "acquire", { file }, process.env);
  assert.equal(next.ok, true, JSON.stringify(next)); assert.equal(next.reclaimed.from, g.handle);
});

test("[native] E6/G1: a snapshot sync is refused while a crashed gate's child survives, the snapshot keeps its tree, and syncs once the child is gone", { timeout: 120_000 }, async (t) => {
  const git = (cwd, ...args) => { const r = spawnSync("git", args, { cwd, encoding: "utf8", env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t" } }); if (r.status !== 0) throw new Error(r.stderr); return r.stdout.trim(); };
  const repo = tmp(t, "rk-own-repo-");
  git(repo, "init", "-q", "-b", "main"); fs.writeFileSync(path.join(repo, "a.txt"), "one\n"); git(repo, "add", "-A"); git(repo, "commit", "-q", "-m", "a");
  const first = git(repo, "rev-parse", "HEAD");
  // Each gate process of this fixture can only be handed its environment: it names a per-run directory under the host's durable
  // managed root (<managed root>/test-roots/<id>, snapshotRoot's one test mechanism), removed when the test ends.
  const env = { ...process.env, RIGHTKIT_SNAPSHOT_TEST_RUN: `own-${process.pid}-${Date.now().toString(36)}` }; delete env[OWNERSHIP_ENV];
  const snaps = snapshotRoot({ env });
  assert.equal(path.basename(path.dirname(path.dirname(snaps))), "test-roots", snaps);
  t.after(() => fs.rmSync(path.dirname(snaps), { recursive: true, force: true }));
  // Each sync is a gate process of its own.
  const sync = (args) => gateStep(t, "snapshot", { cwd: repo, args }, env);
  let step = sync(["--commit", first, "--json"]);
  assert.equal(step.code, 0, JSON.stringify(step));
  const file = ownershipFile({ platform: process.platform, env });
  const g = await crashingGate(t, file);
  await g.kill(g.gate);
  fs.writeFileSync(path.join(repo, "a.txt"), "two\n"); git(repo, "commit", "-qam", "b");
  const second = git(repo, "rev-parse", "HEAD");
  if (process.platform === "win32") {
    // Every member of the crashed gate's job died with it (see the G1 fixture): the next sync reclaims on the drained record.
    await until(() => readHolder(holderFile(file, g.handle))?.state === "drained");
    step = sync(["--commit", second, "--json"]);
    assert.equal(step.code, 0, JSON.stringify(step));
    const r = JSON.parse(step.lines.find((l) => l.startsWith("{")));
    assert.equal(r.commit, second); assert.equal(r.ownership.reclaimed.from, g.handle);
    assert.match(r.ownership.reclaimed.proofs.join("; "), /drained/);
    return;
  }
  step = sync(["--commit", second, "--json"]);
  assert.equal(step.code, 1, `busy, not synced: ${JSON.stringify(step)}`);
  assert.match(step.lines.join("\n"), /snapshot busy: build ownership .* is gone but/);
  const dir = path.join(snaps, repoKey(keyPath(repo)));
  assert.equal(verifySnapshot({ checkout: repo, rev: first, root: snaps, dir }).commit, first, "the snapshot still holds the first commit");
  await g.kill(g.child);
  if (process.platform === "win32") await until(() => readHolder(holderFile(file, g.handle))?.state === "drained");
  step = sync(["--commit", second, "--json"]);
  assert.equal(step.code, 0, JSON.stringify(step));
  const r = JSON.parse(step.lines.find((l) => l.startsWith("{")));
  assert.equal(r.commit, second); assert.equal(r.ownership.reclaimed.from, g.handle);
  assert.equal(fs.existsSync(file), false, "the sync released the host");
});

test("check --snapshot: ownership comes first; busy runs nothing (RK_CHECK_ERROR, exit 2); a held ownership is released however the check ends", async (t) => {
  const { main } = await import("./bin/rightkit-gate.mjs");
  const dir = tmp(t, "rk-own-check-");
  const calls = [];
  const io = (acquireOwnership) => ({ cwd: dir, env: process.env, snapshotRoot: path.join(dir, "snapshots"), out: (m) => calls.push(`out ${m}`), err: (m) => calls.push(`err ${m}`), appCheck: async () => { calls.push("appCheck"); return 0; }, acquireOwnership });
  const busy = () => { calls.push("acquire"); throw new OwnershipBusy("build ownership /x/owner.json: held by pid 4 (compile of abc) since then"); };
  assert.equal(await main(["check", "--commit", "HEAD", "--full-a", "--snapshot", path.join(dir, "snap")], io(busy)), 2);
  assert.deepEqual(calls.slice(0, 1), ["acquire"]);
  assert.match(calls.join("\n"), /RK_CHECK_ERROR: build ownership \/x\/owner.json: held by pid 4 .*; nothing ran/);
  assert.ok(!calls.includes("appCheck"), "nothing ran");
  calls.length = 0;
  let released = 0; const seen = [];
  const held = (q) => { seen.push(q); calls.push("acquire"); return { handle: "h-1", joined: false, reclaimed: null, release: () => { released++; return { released: true }; } }; };
  const code = await main(["check", "--commit", "HEAD", "--full-a", "--snapshot", path.join(dir, "snap")], io(held));
  assert.equal(code, 2, "the snapshot does not exist, so the check itself refuses, after the ownership was taken");
  assert.equal(calls[0], "acquire"); assert.match(calls[1], /build ownership held h-1/);
  assert.equal(released, 1, "released once");
  assert.equal(seen.length, 1, "the inner pass of the same check does not acquire again");
  assert.equal(seen[0].stage, "check"); assert.equal(seen[0].snapshot, path.join(dir, "snap"));
});
