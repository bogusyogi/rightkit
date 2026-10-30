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
  OWNERSHIP_ENV, OWNERSHIP_SCHEMA, HOLDER_SCHEMA, OwnershipBusy, acquireBuildOwnership, brokerLeaseState, childrenDir, holderFile, holderScript, holderScriptFile, jobName, judgeOwner, namesDir, readCleanMarker, cleanMarkerFile, CLEAN_SCHEMA, hostBootId, migrateBirthlessRoot, recordRootBirth, readRootBirth, rootBirthFile, ROOT_BIRTH_SCHEMA, OWNERSHIP_FILE_ENV, ownershipFile, ensureOwnershipDir, classifyHiddenProcesses, HIDDEN_WALK_LIMIT, NOT_MANAGED_LINE, blameKind, OWNERSHIP_DIRNAME, processProbe, queryHolder, readHolder, spawnUnderOwnership, CMD_LINE_LIMIT,
} from "./build-ownership.mjs";
import { keyPath, repoKey, snapshotCommand, syncSnapshot, verifySnapshot } from "./snapshot.mjs";
import { capturedMs, loadMacTable, replaySpawn } from "./test-support/mac-replay.mjs";

const tmp = (t, prefix) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix))); t.after(() => fs.rmSync(d, { recursive: true, force: true, maxRetries: 20, retryDelay: 250 })); return d; }; // a Windows job holder may still write into the directory as the test ends
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
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
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
  fs.mkdirSync(path.dirname(file), { mode: 0o700 }); // the ownership directory exists while a gate holds it (children are created one level deep)
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
  own.markBrokerSubmit(`${own.handle}#c1`);
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
  // A directory named `..x` inside the snapshot is inside it (it is not "outside" because its name starts with two dots); `..` and `../k2` are outside.
  assert.deepEqual(probe({ lsof: "p127\nfcwd\nn/snap/k/..x/deep\np128\nf4r\nn/snap/k/..\np129\nf5r\nn/snap/k/../k2/a\n" }).snapshotUsers("/snap/k"), [{ pid: 127, how: "working directory" }], "..x is inside; the parent and a sibling are not");
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
    // A directory named `..x` inside the snapshot: its readers are inside (cwd, descriptor, mapping); `/snap/k/..` itself (the parent) is not.
    3009: { cwd: `${SNAP}/..x/sub` }, 3010: { fds: { 6: `${SNAP}/..x` } }, 3011: { maps: `7f00-7f01 r--p 0 fe:01 1 ${SNAP}/..x/lib.so\n` }, 3012: { cwd: `${SNAP}/..` },
  });
  const byPid = (a, b) => a.pid - b.pid;
  assert.deepEqual(linuxProbe(fake2).snapshotUsers(SNAP).sort(byPid), [
    { pid: 3001, how: "working directory" }, { pid: 3002, how: "root directory" }, { pid: 3003, how: "executable" }, { pid: 3004, how: `open file ${SNAP}/src/lib.rs` },
    { pid: 3005, how: `mapped file ${SNAP}/target/x.so` }, { pid: 3006, how: "argument" }, { pid: 3008, how: `mapped file ${SNAP}/gone.so` },
    { pid: 3009, how: "working directory" }, { pid: 3010, how: `open file ${SNAP}/..x` }, { pid: 3011, how: `mapped file ${SNAP}/..x/lib.so` },
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

test("ownership POSIX [injected /proc]: an unreadable process keeps the host held and the reclaim message names why; only an older one than the baseline (last clean release, else the record's acquisition) is cleared", (t) => {
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
  // (d) no clean-release marker and no birth: the anchor is the dead record's own acquisition (item5-sage-decisions-2 D1 point 2).
  deadOwner(file, "linux", { snapshot: "/snap/k" });
  assert.ok(acquire(file, "linux", asProbe(...unreadableAgent)).reclaimed, "(d) no marker, started before the record's acquisition: cleared");
  fake.files.set(`${PROC}/4001/stat`, `4001 (agent) S 1 4001 4001 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 ${Math.round((NOW - 50_000 - BTIME * 1000) / 10)} 0 0\n`);
  deadOwner(file, "linux", { snapshot: "/snap/k" });
  assert.throws(() => acquire(file, "linux", asProbe(...unreadableAgent)), /4001 .* not before the baseline/, "(d) no marker, started after the record's acquisition: held");
  fake.files.set(`${PROC}/4001/stat`, `4001 (agent) S 1 4001 4001 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 ${Math.round((NOW - 3_600_000 - BTIME * 1000) / 10)} 0 0\n`);
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
  assert.equal(hostBootId({ platform: "darwin", spawn: () => ({ status: 0, stdout: "B73EFB5F-98EF-436A-9C4C-22790CEED886\n" }) }), "B73EFB5F-98EF-436A-9C4C-22790CEED886");
  assert.equal(hostBootId({ platform: "darwin", spawn: () => ({ status: 1, stdout: "" }) }), null, "kern.bootsessionuuid unreadable: no boot identity, so no anchor");
  assert.equal(hostBootId({ platform: "darwin", spawn: () => ({ status: 0, stdout: "garbage" }) }), null);
  assert.equal(hostBootId({ platform: "darwin", spawn: () => ({ status: 0, stdout: "{ sec = 1700000000, usec = 5 } Tue Nov 14 22:13:20 2023\n" }) }), null, "kern.boottime is not the boot identity");
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

test("ownership POSIX [injected /proc]: the snapshot root's birth is the baseline: a pre-birth unreadable process (ssh-agent) lets the first release clean; post-birth holds; with no birth record, another boot's birth or an unreadable boot id the anchor is the acquisition; a recreated root is born again", (t) => {
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
  // D1 (5.23 amended): with no baseline the anchor is this record's own acquisition, never "none". No birth record: the agent from
  // before the acquisition (6001) is dropped, the process that started after it (6002) is held.
  own = fresh(probe());
  r = own.release();
  assert.equal(r.released, false); assert.match(r.why, /6002 .* not before the baseline/); assert.doesNotMatch(r.why, /6001/, "no birth: the pre-acquisition agent is dropped");
  assert.doesNotMatch(r.why, /no baseline for this ownership/, "there is always an anchor now");
  // A birth of another boot: the same pair.
  own = fresh(probe("boot-2")); born();
  r = own.release();
  assert.equal(r.released, false); assert.match(r.why, /6002 .* not before the baseline/); assert.doesNotMatch(r.why, /6001/, "another boot's birth: dropped");
  // A birth whose boot id cannot be read now: the same pair.
  own = fresh(probe(null)); born();
  r = own.release();
  assert.equal(r.released, false); assert.match(r.why, /6002 .* not before the baseline/); assert.doesNotMatch(r.why, /6001/, "unreadable boot id: dropped");
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

/** A tiny repository with one commit, for the sync-level migration fixtures. */
function tinyRepo(t) {
  const git = (cwd, ...args) => { const r = spawnSync("git", args, { cwd, encoding: "utf8", env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t" } }); if (r.status !== 0) throw new Error(r.stderr); return r.stdout.trim(); };
  const repo = tmp(t, "rk-own-mig-repo-");
  git(repo, "init", "-q", "-b", "main"); fs.writeFileSync(path.join(repo, "a.txt"), "one\n"); git(repo, "add", "-A"); git(repo, "commit", "-q", "-m", "a");
  return repo;
}
const OLD_KEY = "0123456789abcdef";
/** A snapshot root as an older gate left it: snapshot directories, a lock and a state file, and no birth record. */
function oldRoot(root, { birth = null } = {}) {
  fs.mkdirSync(root, { recursive: true, mode: 0o700 }); // a snapshot root is private to this user (snapshot.mjs refuses a group- or other-accessible one)
  fs.mkdirSync(path.join(root, OLD_KEY, "src"), { recursive: true }); fs.writeFileSync(path.join(root, OLD_KEY, "src", "lib.rs"), "old\n");
  fs.writeFileSync(path.join(root, `${OLD_KEY}.state.json`), "{}"); fs.writeFileSync(path.join(root, `${OLD_KEY}.lock`), "");
  if (birth) recordRootBirth(root, birth);
}

test("migration: a free host's birthless root is emptied and born anew under this process's own ownership; every other shape is left strict, untouched", (t) => {
  const base = tmp(t, "rk-own-mig-"); const root = path.join(base, "snapshots"); const file = path.join(base, "build-ownership", "owner.json");
  const probe = () => fakeProbe("linux", { bootId: () => "boot-1" });
  const exists = (n) => fs.existsSync(path.join(root, n));
  const env = (own) => ({ [OWNERSHIP_ENV]: own.handle, [OWNERSHIP_FILE_ENV]: file });
  // (a) free host, birthless root: recreated and born; its contents are gone.
  oldRoot(root);
  let own = acquire(file, "linux", probe());
  assert.equal(JSON.parse(fs.readFileSync(file, "utf8")).freeHost, true);
  let r = migrateBirthlessRoot(root, { env: env(own), bootId: "boot-1" });
  assert.equal(r.migrated, true, JSON.stringify(r));
  assert.deepEqual(fs.readdirSync(root), [path.basename(rootBirthFile(root))], "only the new birth is left");
  assert.equal(readRootBirth(root, "boot-1") !== null, true);
  // ...and once born, a second call leaves it alone (a root that already has a birth).
  oldRoot(root);
  r = migrateBirthlessRoot(root, { env: env(own), bootId: "boot-1" });
  assert.equal(r.migrated, false); assert.match(r.why, /already has a birth record of this boot/); assert.equal(exists(OLD_KEY), true, "untouched");
  own.release();
  // A birth record from ANOTHER boot, on a free host: migrated exactly like a birthless root, and reborn with the current boot id.
  fs.rmSync(root, { recursive: true }); oldRoot(root, { birth: { now: () => NOW - 9_000_000, bootId: "boot-0" } });
  assert.equal(readRootBirth(root, "boot-1"), null, "that birth gives no baseline in this boot");
  own = acquire(file, "linux", probe());
  r = migrateBirthlessRoot(root, { env: env(own), bootId: "boot-1" });
  assert.equal(r.migrated, true, JSON.stringify(r)); assert.match(r.why, /another boot \("boot-0", not "boot-1"\)/);
  assert.deepEqual(fs.readdirSync(root), [path.basename(rootBirthFile(root))], "the old snapshots are gone");
  assert.ok(readRootBirth(root, "boot-1") >= NOW, "reborn with the current boot id, now");
  assert.equal(readRootBirth(root, "boot-0"), null);
  own.release();
  // A birth with no boot id (written when the boot could not be read) cannot match any boot: it is migrated too.
  fs.rmSync(root, { recursive: true }); oldRoot(root, { birth: { bootId: null } });
  own = acquire(file, "linux", probe());
  assert.equal(migrateBirthlessRoot(root, { env: env(own), bootId: "boot-1" }).migrated, true);
  own.release();
  // An unreadable birth record is not a birth of another boot: untouched.
  fs.rmSync(root, { recursive: true }); oldRoot(root); fs.writeFileSync(rootBirthFile(root), "not json");
  own = acquire(file, "linux", probe());
  r = migrateBirthlessRoot(root, { env: env(own), bootId: "boot-1" });
  assert.equal(r.migrated, false); assert.match(r.why, /birth record cannot be read/); assert.equal(exists(OLD_KEY), true);
  own.release();
  // An unreadable current boot id leaves the root untouched, birthless or other-boot alike.
  for (const birth of [null, { now: () => NOW - 9_000_000, bootId: "boot-0" }]) {
    fs.rmSync(root, { recursive: true }); oldRoot(root, { birth });
    own = acquire(file, "linux", probe());
    for (const unreadable of [null, ""]) {
      r = migrateBirthlessRoot(root, { env: env(own), bootId: unreadable });
      assert.equal(r.migrated, false); assert.match(r.why, /boot's identity cannot be read/); assert.equal(exists(OLD_KEY), true, "untouched");
    }
    own.release();
  }
  // An other-boot birth on a held or reclaimed host is untouched (the held cases below, with a birth record of another boot).
  fs.rmSync(root, { recursive: true }); oldRoot(root, { birth: { now: () => NOW - 9_000_000, bootId: "boot-0" } });
  deadOwner(file, "linux");
  own = acquire(file, "linux", probe());          // a reclaim
  r = migrateBirthlessRoot(root, { env: env(own), bootId: "boot-1" });
  assert.equal(r.migrated, false); assert.match(r.why, /not taken on a free host/); assert.equal(exists(OLD_KEY), true); assert.equal(readRootBirth(root, "boot-0") !== null, true, "the old birth stays");
  own.release();
  fs.writeFileSync(file, JSON.stringify({ schema: OWNERSHIP_SCHEMA, handle: "other-h", pid: process.pid + 1, freeHost: true, acquiredAt: iso(NOW), host: os.hostname() }));
  r = migrateBirthlessRoot(root, { env: { [OWNERSHIP_ENV]: "other-h", [OWNERSHIP_FILE_ENV]: file }, bootId: "boot-1" });
  assert.equal(r.migrated, false); assert.match(r.why, /does not hold the owner record/); assert.equal(exists(OLD_KEY), true);
  fs.rmSync(file);
  // (b) a held host (a record that is not this process's free-host ownership): a reclaimed record, another process's record, no ownership at all.
  fs.rmSync(root, { recursive: true }); oldRoot(root);
  deadOwner(file, "linux");
  own = acquire(file, "linux", probe());          // a reclaim
  assert.equal(own.reclaimed !== null, true); assert.equal(JSON.parse(fs.readFileSync(file, "utf8")).freeHost, false);
  r = migrateBirthlessRoot(root, { env: env(own), bootId: "boot-1" });
  assert.equal(r.migrated, false); assert.match(r.why, /not taken on a free host/); assert.equal(exists(OLD_KEY), true);
  own.release();
  fs.writeFileSync(file, JSON.stringify({ schema: OWNERSHIP_SCHEMA, handle: "other-h", pid: process.pid + 1, freeHost: true, acquiredAt: iso(NOW), host: os.hostname() }));
  r = migrateBirthlessRoot(root, { env: { [OWNERSHIP_ENV]: "other-h", [OWNERSHIP_FILE_ENV]: file }, bootId: "boot-1" });
  assert.equal(r.migrated, false); assert.match(r.why, /does not hold the owner record/); assert.equal(exists(OLD_KEY), true);
  r = migrateBirthlessRoot(root, { env: { [OWNERSHIP_ENV]: "mine", [OWNERSHIP_FILE_ENV]: file }, bootId: "boot-1" });
  assert.equal(r.migrated, false); assert.match(r.why, /does not hold the owner record/, "a handle that is not the record's");
  r = migrateBirthlessRoot(root, { env: {}, bootId: "boot-1" });
  assert.equal(r.migrated, false); assert.match(r.why, /holds no build ownership/);
  fs.rmSync(file);
  // A root that is some other directory is never emptied; a missing root is the sync's to create; a link is not a root.
  fs.rmSync(root, { recursive: true }); oldRoot(root); fs.writeFileSync(path.join(root, "notes.txt"), "mine");
  own = acquire(file, "linux", probe());
  r = migrateBirthlessRoot(root, { env: env(own), bootId: "boot-1" });
  assert.equal(r.migrated, false); assert.match(r.why, /"notes.txt" in the root is not a snapshot artifact/); assert.equal(exists(OLD_KEY), true); assert.equal(exists("notes.txt"), true);
  fs.rmSync(root, { recursive: true });
  assert.match(migrateBirthlessRoot(root, { env: env(own), bootId: "boot-1" }).why, /missing or is not a plain directory/);
  if (process.platform !== "win32") {
    const real = path.join(base, "real"); oldRoot(real); fs.symlinkSync(real, root);
    assert.match(migrateBirthlessRoot(root, { env: env(own), bootId: "boot-1" }).why, /missing or is not a plain directory/);
    assert.equal(fs.existsSync(path.join(real, OLD_KEY)), true, "a link is never followed");
  }
  own.release();
});

test("migration: syncSnapshot under a free-host ownership empties a birthless root before its first write and births it; a held host's sync leaves the old snapshots", (t) => {
  const base = tmp(t, "rk-own-mig2-"); const root = path.join(base, "snapshots"); const file = path.join(base, "build-ownership", "owner.json");
  const repo = tinyRepo(t);
  const probe = () => fakeProbe("linux", { bootId: () => "boot-1" });
  oldRoot(root);
  const own = acquire(file, "linux", probe(), { exportEnv: true });
  try {
    syncSnapshot({ checkout: repo, root });
    assert.equal(fs.existsSync(path.join(root, repoKey(keyPath(repo)))), true, "the new snapshot is written");
    if (hostBootId() === null) {
      // Windows has no boot id (and no unreadable-process rule): nothing is migrated, the root stays as it was.
      assert.equal(fs.existsSync(path.join(root, OLD_KEY)), true); assert.equal(fs.existsSync(rootBirthFile(root)), false);
    } else {
      assert.equal(fs.existsSync(path.join(root, OLD_KEY)), false, "the old snapshot is gone");
      assert.equal(fs.existsSync(rootBirthFile(root)), true, "and the root has its birth");
    }
  } finally { own.release(); }
  // No ownership (a bare sync, as a test or a verify-only path calls it): the old root is left as it was, birthless.
  fs.rmSync(root, { recursive: true }); oldRoot(root);
  syncSnapshot({ checkout: repo, root });
  assert.equal(fs.existsSync(path.join(root, OLD_KEY)), true); assert.equal(fs.existsSync(rootBirthFile(root)), false);
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
  const spawnSpy = (cmd, args) => { argv = [cmd, ...args]; return { status: 0, stdout: 'log line\n{"schema":"rustright.snapshot-lease-status.v1","state":"REAPED","snapshot":"/s","ownership":"h"}\n' }; };
  assert.equal(brokerLeaseState({ ...q, spawn: spawnSpy }).state, "REAPED");
  assert.deepEqual(argv, ["/x/rustright", "snapshot-lease", "status", "--snapshot", "/s", "--ownership", "h"]);
  // The answer must echo the queried snapshot and handle (RustRight echoes both): no echo, or another pair's, is UNKNOWN (held).
  const echo = (extra) => answer(JSON.stringify({ schema: "rustright.snapshot-lease-status.v1", state: "REAPED", ...extra }));
  assert.equal(brokerLeaseState({ ...q, spawn: echo({}) }).state, "UNKNOWN", "no echo");
  assert.equal(brokerLeaseState({ ...q, spawn: echo({ snapshot: "/s" }) }).state, "UNKNOWN", "no ownership echo");
  assert.equal(brokerLeaseState({ ...q, spawn: echo({ snapshot: "/other", ownership: "h" }) }).state, "UNKNOWN", "another snapshot");
  assert.equal(brokerLeaseState({ ...q, spawn: echo({ snapshot: "/s", ownership: "other" }) }).state, "UNKNOWN", "another handle");
  assert.equal(brokerLeaseState({ ...q, spawn: echo({ snapshot: "/s", ownership: "h" }) }).state, "REAPED");
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
async function crashingGate(t, file, { detachChild = process.platform === "win32", mode = "plain", snapshot = null, viaNode = false, subreaper = false, anchored = process.platform === "darwin" } = {}) {
  // macOS: a process whose environment `ps eww` hides is held after the ownership's anchor (row 5.24a), and with no anchor every such
  // process is held, which on a real Mac is dozens. So a fixture whose next gate must reclaim gives the snapshot root a birth first
  // (the anchor, as a real gate's root has), exactly as a root the gate created has.
  if (anchored) {
    const root = snapshot ? path.dirname(snapshot) : path.join(path.dirname(path.dirname(file)), "snapshots");
    fs.mkdirSync(root, { recursive: true, mode: 0o700 }); recordRootBirth(root, { bootId: hostBootId() });
    snapshot ??= path.join(root, OLD_KEY);
  }
  const script = path.join(path.dirname(file), "fake-gate.mjs");
  const SCANS = pathToFileURL(path.resolve("deterministic-scans.mjs")).href;
  // plain: a child of the gate (detached on request). scan: the real class A spawner (detached, its own group) with the
  // ownership variable scrubbed and its cwd outside any snapshot, so only the detached-child record can hold the host.
  // platform-binary: /bin/sleep started detached in the snapshot with an empty environment: only the working-directory net sees it.
  fs.writeFileSync(script, `
const { acquireBuildOwnership } = await import(${JSON.stringify(MODULE)});
const own = acquireBuildOwnership({ file: ${JSON.stringify(file)}, stage: "fixture", snapshot: ${JSON.stringify(snapshot)} });
const { spawn } = await import("node:child" + "_process");
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
} else if (${JSON.stringify(mode)} === "hidden-orphan") {
  // gate -> node A (its own session) -> /bin/sleep (an Apple platform binary: it carries the ownership variable, but \`ps eww\` shows no
  // environment for it), cwd "/", touching nothing in the snapshot; A exits, so macOS (no subreaper) orphans the sleep to launchd.
  const fs = await import("node:fs");
  const a = spawn(process.execPath, ["-e", "const c = require('child' + '_process').spawn('/bin/sleep', ['120'], { stdio: 'ignore', cwd: '/' }); process.stdout.write(String(c.pid)); process.exit(0)"], { detached: true, stdio: ["ignore", "pipe", "inherit"] });
  pid = Number(await new Promise((res, rej) => { a.stdout.once("data", (d) => res(String(d).trim())); a.once("exit", (code) => rej(new Error("A exited " + code + " before reporting"))); }));
  await new Promise((res) => (a.exitCode !== null ? res() : a.once("exit", res)));
  const { spawnSync } = await import("node:child" + "_process");
  for (let i = 0; i < 100; i++) { if (spawnSync("ps", ["-o", "ppid=", "-p", String(pid)], { encoding: "utf8" }).stdout.trim() === "1") break; await new Promise((r) => setTimeout(r, 50)); }
} else if (${JSON.stringify(mode)}.startsWith("orphan-")) {
  // The lead's probe shapes (signed/item5-sage-decisions-2.md "Lead probe 2026-10-29T07:40:35Z"; RR's m4probe.sh and shapes.sh): each
  // leaves /bin/sleep (an Apple platform binary: \`ps eww\` hides the ownership variable it carries) orphaned to launchd, at cwd "/",
  // touching nothing in the snapshot, from a shell in its own session, so neither the owner's group nor a recorded child covers it.
  const fs = await import("node:fs");
  const pidFile = ${JSON.stringify(file)} + ".orphan-pid"; fs.rmSync(pidFile, { force: true });
  const shell = {
    // RR M4: fork, the parent exits at once, the child starts a session and execs the sleep.
    "orphan-setsid": "/usr/bin/perl -e 'use POSIX; exit 0 if fork; POSIX::setsid(); open(STDIN, \\"<\\", \\"/dev/null\\"); open(STDOUT, \\">\\", \\"/dev/null\\"); open(STDERR, \\">\\", \\"/dev/null\\"); open(my $f, \\">\\", $ENV{PID_FILE}); print $f $$; close $f; exec \\"/bin/sleep\\", \\"120\\"'",
    // A subshell's background job: the subshell exits, the sleep is reparented.
    "orphan-subshell": "( /bin/sleep 120 </dev/null >/dev/null 2>&1 & echo $! > \\"$PID_FILE\\" )",
    // nohup in the background of a shell that then exits.
    "orphan-nohup": "nohup /bin/sleep 120 </dev/null >/dev/null 2>&1 & echo $! > \\"$PID_FILE\\"",
  }[${JSON.stringify(mode)}];
  if (!shell) throw new Error("unknown orphan shape " + ${JSON.stringify(mode)});
  const sh = spawn("/bin/sh", ["-c", shell], { detached: true, stdio: "ignore", cwd: "/", env: { ...process.env, PID_FILE: pidFile } });
  await new Promise((res) => (sh.exitCode !== null ? res() : sh.once("exit", res)));
  for (let i = 0; i < 200 && !(fs.existsSync(pidFile) && fs.readFileSync(pidFile, "utf8").trim()); i++) await new Promise((r) => setTimeout(r, 25));
  pid = Number(fs.readFileSync(pidFile, "utf8").trim());
  const { spawnSync } = await import("node:child" + "_process");
  for (let i = 0; i < 100; i++) { if (spawnSync("ps", ["-o", "ppid=", "-p", String(pid)], { encoding: "utf8" }).stdout.trim() === "1") break; await new Promise((r) => setTimeout(r, 50)); }
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
function gateStep(t, kind, args, env, { nested = null } = {}) {
  const SNAP = pathToFileURL(path.resolve("snapshot.mjs")).href;
  const driver = path.join(fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-own-step-"))), "step.mjs");
  t.after(() => fs.rmSync(path.dirname(driver), { recursive: true, force: true }));
  fs.writeFileSync(driver, `
const a = ${JSON.stringify(args)};
if (${JSON.stringify(kind)} === "acquire") {
  const { acquireBuildOwnership } = await import(${JSON.stringify(MODULE)});
  // The next gate works on the snapshot the crashed gate had (a gate of the same repository); on macOS that root's birth is the anchor
  // its own release is judged by.
  let snapshot = a.snapshot; if (snapshot === undefined) { try { snapshot = JSON.parse((await import("node:fs")).readFileSync(a.file, "utf8")).snapshot ?? undefined; } catch { snapshot = undefined; } }
  try { const own = acquireBuildOwnership({ file: a.file, stage: "next", snapshot, env: process.env }); const released = own.release(); console.log(JSON.stringify({ ok: true, reclaimed: own.reclaimed, released })); }
  catch (e) { console.log(JSON.stringify({ ok: false, code: e.code, message: e.message })); }
} else if (${JSON.stringify(kind)} === "sync") {
  // The door's shape: take ownership (exported to this process's environment), then sync into the root, then release.
  const { acquireBuildOwnership } = await import(${JSON.stringify(MODULE)});
  const { syncSnapshot } = await import(${JSON.stringify(SNAP)});
  let own = null;
  try {
    own = acquireBuildOwnership({ file: a.file, stage: "snapshot", snapshot: a.snapshot, env: process.env });
    const synced = syncSnapshot({ checkout: a.checkout, root: a.root });
    const released = own.release();
    console.log(JSON.stringify({ ok: true, released, reclaimed: own.reclaimed, commit: synced.commit }));
  } catch (e) { console.log(JSON.stringify({ ok: false, code: e.code, message: e.message, released: own?.release?.() })); }
} else {
  const { snapshotCommand } = await import(${JSON.stringify(SNAP)});
  const lines = [];
  const code = snapshotCommand({ cwd: a.cwd, env: process.env, platform: process.platform, snapshotRoot: a.snapshotRoot, out: (m) => lines.push(m), err: (m) => lines.push(m) }, a.args);
  console.log(JSON.stringify({ code, lines }));
}
process.exit(0);
`);
  const childEnv = { ...env, NODE_OPTIONS: "" }; delete childEnv[OWNERSHIP_ENV]; delete childEnv.RIGHTKIT_BUILD_OWNERSHIP_FILE;
  if (nested) childEnv[OWNERSHIP_ENV] = nested; // a nested gate: started under a live owner's handle
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
const { spawn, spawnSync } = await import("node:child" + "_process");
const { acquireBuildOwnership } = await import(${JSON.stringify(MODULE)});
const r = { };
try {
  const own = acquireBuildOwnership({ file: ${JSON.stringify(file)}, stage: "fixture" });
  r.contained = own.contained; r.job = own.owner.containment.name;
  const child = spawn(process.execPath, ["-e", "const c = require('child' + '_process').spawn('ping.exe', ['-n', '120', '127.0.0.1'], { stdio: 'ignore' }); c.on('error', (e) => { process.stderr.write(String(e)); process.exit(3); }); process.stdout.write(String(c.pid)); setInterval(() => {}, 1000);"], { stdio: ["ignore", "pipe", "pipe"] });
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
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
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
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
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
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const g = await crashingGate(t, file, { detachChild: true });
  await g.kill(g.gate);
  const env = { ...process.env }; delete env[OWNERSHIP_ENV];
  assert.throws(() => acquireBuildOwnership({ file, env, exportEnv: false }), new RegExp(`${g.child} still carry ${OWNERSHIP_ENV}`));
  await g.kill(g.child);
  assert.equal(acquireBuildOwnership({ file, env, exportEnv: false }).reclaimed.from, g.handle);
});

test("[native] G1 4b review: a class A scan child the real spawner detached (own group, variable scrubbed, cwd elsewhere) holds the host until it is gone", { skip: process.platform === "win32" && "Windows children stay in the gate's job", timeout: 60_000 }, async (t) => {
  const file = path.join(tmp(t, "rk-own-native-"), "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
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
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
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
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
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
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const g = await crashingGate(t, file, { mode: "escaped-argv", snapshot });
  await g.kill(g.gate);
  const env = { ...process.env }; delete env[OWNERSHIP_ENV];
  assert.throws(() => acquireBuildOwnership({ file, env, exportEnv: false }), new RegExp(`${g.child} \\(argument\\) still use the snapshot`));
  await g.kill(g.child);
  assert.equal(acquireBuildOwnership({ file, env, exportEnv: false }).reclaimed.from, g.handle);
});

test("[native] review item5-r1 #4: an orphan of the gate's group with the variable scrubbed and cwd elsewhere holds the host (group net)", { skip: process.platform === "win32" && "Windows children stay in the gate's job", timeout: 60_000 }, async (t) => {
  const file = path.join(tmp(t, "rk-own-native-"), "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const g = await crashingGate(t, file, { mode: "scrubbed" });
  await g.kill(g.gate);
  const env = { ...process.env }; delete env[OWNERSHIP_ENV];
  assert.throws(() => acquireBuildOwnership({ file, env, exportEnv: false }), new RegExp(`started under this ownership: ${g.child}`));
  await g.kill(g.child);
  assert.equal(acquireBuildOwnership({ file, env, exportEnv: false }).reclaimed.from, g.handle);
});

test("[native, Linux + python3] review item5-r1 #4: an orphan adopted by a subreaper (an ancestor that predates the ownership) is still attributed to the owner", { skip: !HAS_SUBREAPER && "needs Linux and python3 (prctl PR_SET_CHILD_SUBREAPER)", timeout: 60_000 }, async (t) => {
  const file = path.join(tmp(t, "rk-own-native-"), "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
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

test("[native, Linux + python3] review item5-r2 #7: a real first acquisition on a pre-existing birthless root, with a non-dumpable same-user process, now releases", { skip: !HAS_SUBREAPER && "needs Linux and python3 (prctl PR_SET_DUMPABLE)", timeout: 90_000 }, async (t) => {
  const base = tmp(t, "rk-own-native-"); const root = path.join(base, "snapshots"); const file = path.join(base, "build-ownership", "owner.json");
  const repo = tinyRepo(t); oldRoot(root);
  // An agent from before the ownership (ssh-agent, gpg-agent): its own user's, non-dumpable, so /proc/<pid>/environ refuses reads.
  const ready = path.join(base, "agent.ready");
  const agent = spawn("python3", ["-c", "import ctypes, sys, time\nctypes.CDLL(None).prctl(4, 0, 0, 0, 0)\nopen(sys.argv[1], 'w').write('1')\ntime.sleep(600)", ready], { stdio: "ignore" });
  t.after(() => { try { agent.kill("SIGKILL"); } catch { /* gone */ } });
  await until(() => fs.existsSync(ready), 20_000);
  let readable = true; try { fs.readFileSync(`/proc/${agent.pid}/environ`); } catch (e) { readable = !["EACCES", "EPERM"].includes(e.code); }
  if (readable) return t.skip("this user can read a non-dumpable process (root?)");
  // The reading error of a process start is up to 2 s: the agent must be clearly older than the ownership.
  await new Promise((r) => setTimeout(r, 3_500));
  const step = gateStep(t, "sync", { file, checkout: repo, root, snapshot: path.join(root, repoKey(keyPath(repo))) }, process.env);
  assert.equal(step.ok, true, JSON.stringify(step));
  assert.equal(step.released.released, true, `released with the agent alive: ${JSON.stringify(step.released)}`);
  assert.equal(fs.existsSync(file), false, "the host is free again");
  assert.equal(fs.existsSync(path.join(root, OLD_KEY)), false, "the birthless root was emptied");
  assert.equal(fs.existsSync(rootBirthFile(root)), true, "and born");
  assert.doesNotThrow(() => process.kill(agent.pid, 0), "the agent was never touched");
});

test("[native] E6/G1: a snapshot sync is refused while a crashed gate's child survives, the snapshot keeps its tree, and syncs once the child is gone", { timeout: 120_000 }, async (t) => {
  const git = (cwd, ...args) => { const r = spawnSync("git", args, { cwd, encoding: "utf8", env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t" } }); if (r.status !== 0) throw new Error(r.stderr); return r.stdout.trim(); };
  const repo = tmp(t, "rk-own-repo-");
  git(repo, "init", "-q", "-b", "main"); fs.writeFileSync(path.join(repo, "a.txt"), "one\n"); git(repo, "add", "-A"); git(repo, "commit", "-q", "-m", "a");
  const first = git(repo, "rev-parse", "HEAD");
  // Each gate process of this fixture is a driver script that calls snapshotCommand in-process: the root is passed to it as a
  // parameter (never an environment variable), and the owner record lives beside it.
  const env = { ...process.env }; delete env[OWNERSHIP_ENV];
  const snaps = path.join(tmp(t, "rk-own-root-"), "snapshots");
  // Each sync is a gate process of its own.
  const sync = (args) => gateStep(t, "snapshot", { cwd: repo, args, snapshotRoot: snaps }, env);
  let step = sync(["--commit", first, "--json"]);
  assert.equal(step.code, 0, JSON.stringify(step));
  const file = ownershipFile({ platform: process.platform, beside: snaps });
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

// ======================================================================================================================
// Item 5, lane L5O: rows 5.17, 5.20, 5.22 (ownership directory), 5.23 (M and D native), 5.24 (Linux, real cargo), 5.24a (macOS).
// ======================================================================================================================

/** Runs `fn` with these variables set in this process, then restores them. */
function withEnv(env, fn) {
  const saved = {};
  for (const k of Object.keys(env)) { saved[k] = process.env[k]; process.env[k] = env[k]; }
  try { return fn(); } finally { for (const k of Object.keys(env)) { if (saved[k] === undefined) delete process.env[k]; else process.env[k] = saved[k]; } }
}
const HOSTILE_ENV = {
  XDG_STATE_HOME: "/hostile/xdg", RIGHTKIT_SNAPSHOT_ROOT: "/hostile/snaps", RIGHTKIT_SNAPSHOT_TEST_RUN: "1", HOME: "/hostile/home", USERPROFILE: "C:\\hostile",
  RIGHTKIT_DATA_ROOT: "/hostile/data", RIGHTKIT_MANAGED_ROOT: "/hostile/managed", RIGHTKIT_EXTERNAL_ROOT: "/hostile/ext", RIGHTKIT_CONTROL_ROOT: "/hostile/ctl", TMPDIR: "/hostile/tmp",
};

test("row 5.17: the ownership record has ONE fixed location per host, independent of the snapshot root and of every environment variable", () => {
  const managed = { darwin: "/Volumes/D/.rightkit-managed/build-ownership/owner.json", win32: "D:\\.rightkit-managed\\build-ownership\\owner.json" };
  for (const platform of ["darwin", "win32"]) {
    for (const env of [{}, HOSTILE_ENV]) assert.equal(withEnv(env, () => ownershipFile({ platform })), managed[platform], `${platform} with ${Object.keys(env).length} variables set`);
  }
  // Linux: the passwd home of the account, never $HOME, never XDG_STATE_HOME, never a configured snapshot root.
  const account = os.userInfo().homedir;
  const linuxDefault = path.posix.join(account, ".local", "state", "rightkit", OWNERSHIP_DIRNAME, "owner.json");
  for (const env of [{}, HOSTILE_ENV, { XDG_STATE_HOME: "/other/xdg" }, { RIGHTKIT_SNAPSHOT_ROOT: "/other/snaps" }, { HOME: "/other/home" }]) {
    assert.equal(withEnv(env, () => ownershipFile({ platform: "linux" })), linuxDefault, JSON.stringify(env));
  }
  // Passing the arguments the old derivation read (a snapshot root, an environment) moves nothing.
  assert.equal(ownershipFile({ platform: "linux", home: "/h", snapshotRoot: "/a/snapshots", env: { XDG_STATE_HOME: "/x", RIGHTKIT_SNAPSHOT_ROOT: "/y" } }), "/h/.local/state/rightkit/build-ownership/owner.json");
  assert.equal(ownershipFile({ platform: "darwin", home: "/Users/a", snapshotRoot: "/a/snapshots", env: HOSTILE_ENV }), managed.darwin);
  // The two injection points fixtures use are explicit and never a variable.
  assert.equal(ownershipFile({ platform: "linux", home: "/fixture/home" }), "/fixture/home/.local/state/rightkit/build-ownership/owner.json");
  assert.equal(ownershipFile({ platform: "linux", beside: "/fixture/snaps" }), "/fixture/build-ownership/owner.json");
  assert.throws(() => ownershipFile({ platform: "linux", beside: "" }), TypeError);
});

/** The calls of `ownershipFile(...)` in `text` that derive the location from a snapshot root or the environment (the removed behaviour). */
function ownershipCallViolations(text) {
  const bad = []; let calls = 0;
  for (const m of text.matchAll(/(?<!function )\bownershipFile\(\s*(\{[^}]*\})?\s*\)/g)) {
    calls++;
    const args = m[1] ?? "";
    if (/\bsnapshotRoot\s*:/.test(args) || /\benv\s*:/.test(args) || /\bhome\s*:/.test(args)) bad.push(m[0]);
    for (const b of args.matchAll(/\bbeside\s*:\s*([^,}]+)/g)) if (b[1].trim() !== "o.snapshotRoot") bad.push(m[0]);
  }
  return { bad, calls };
}

test("row 5.17: no production caller derives the ownership location from the snapshot root or an environment variable", () => {
  const files = [];
  for (const dir of [".", "bin", "cli"]) for (const name of fs.readdirSync(dir)) if (name.endsWith(".mjs") && !name.endsWith(".test.mjs")) files.push(path.join(dir, name));
  let calls = 0; const bad = [];
  for (const f of files) { const r = ownershipCallViolations(fs.readFileSync(f, "utf8")); calls += r.calls; bad.push(...r.bad.map((b) => `${f}: ${b}`)); }
  assert.deepEqual(bad, [], "a production call passes a derived root or an environment");
  assert.ok(calls >= 5, `the scan saw the production calls (${calls})`);
  // The scan sees the defect: the old call shapes are violations.
  assert.equal(ownershipCallViolations("ownershipFile({ platform: o.platform, env: o.env, snapshotRoot: root })").bad.length, 1);
  assert.equal(ownershipCallViolations("ownershipFile({ platform: o.platform, snapshotRoot: snapshotsAt })").bad.length, 1);
  assert.equal(ownershipCallViolations("ownershipFile({ platform: o.platform, beside: snapshotsAt })").bad.length, 1);
  assert.equal(ownershipCallViolations("ownershipFile({ platform: o.platform, beside: o.snapshotRoot })").bad.length, 0);
});

test("row 5.17 [native]: two gates whose environments differ only in XDG_STATE_HOME (or a configured snapshot root) contend for ONE record; the old derivation (a test double) holds two", { timeout: 180_000 }, async (t) => {
  const home = path.join(tmp(t, "rk-own-fixed-"), "home"); fs.mkdirSync(home, { mode: 0o700 });
  const pairs = [
    ["XDG_STATE_HOME", { XDG_STATE_HOME: path.join(home, "xdg-a") }, { XDG_STATE_HOME: path.join(home, "xdg-b") }],
    ["RIGHTKIT_SNAPSHOT_ROOT", { RIGHTKIT_SNAPSHOT_ROOT: path.join(home, "root-a", "snaps") }, { RIGHTKIT_SNAPSHOT_ROOT: path.join(home, "root-b", "snaps") }],
  ];
  // The production derivation: the environment is not an input.
  const fixed = (env) => withEnv(env, () => ownershipFile({ platform: "linux", home }));
  // The removed derivation, as a test double: the record sits beside the snapshot root, which the environment chooses.
  const old = (env) => withEnv(env, () => {
    const root = env.RIGHTKIT_SNAPSHOT_ROOT ?? path.join(env.XDG_STATE_HOME ?? path.join(home, ".local", "state"), "rightkit", "snapshots");
    return path.join(path.dirname(root), "build-ownership", "owner.json");
  });
  for (const [variable, envA, envB] of pairs) {
    for (const [name, locate, expectBusy] of [["fixed location", fixed, true], ["old derivation (test double)", old, false]]) {
      const fileA = locate(envA); const fileB = locate(envB);
      assert.equal(fileA === fileB, expectBusy, `${name}: the two environments ${expectBusy ? "share" : "choose different"} records (${variable})`);
      fs.mkdirSync(path.dirname(fileA), { recursive: true, mode: 0o700 }); fs.mkdirSync(path.dirname(fileB), { recursive: true, mode: 0o700 });
      const holder = await crashingGate(t, fileA, { anchored: false });
      const second = gateStep(t, "acquire", { file: fileB, snapshot: null }, { ...process.env, ...envB });
      if (expectBusy) {
        assert.equal(second.ok, false, `${name} (${variable}): the second gate is busy: ${JSON.stringify(second)}`); assert.equal(second.code, "busy");
        assert.match(second.message, new RegExp(`held by pid ${holder.gate}`));
      } else {
        assert.equal(second.ok, true, `${name} (${variable}): the second gate took a record of its own while the first was held, which is the defect: ${JSON.stringify(second)}`);
      }
      await holder.kill(holder.gate); await holder.kill(holder.child);
      for (const f of new Set([fileA, fileB])) fs.rmSync(path.dirname(f), { recursive: true, force: true });
    }
  }
});

test("row 5.20: a nested gate joins only the LIVE owner's exact handle and a matching record; a fabricated, stale, dead-owner, reused-pid, foreign-host or malformed record is refused", (t) => {
  const file = path.join(tmp(t, "rk-own-join-"), "owner.json");
  const first = acquire(file, "darwin", fakeProbe("darwin"));
  const alive = fakeProbe("darwin", { identity: () => ({ alive: true, startedMs: NOW - 60_000, group: 4242 }) });
  const join = (probe, env, platform = "darwin") => acquireBuildOwnership({ file, platform, probe, lease: noLease, env, exportEnv: false });
  const joined = join(alive, { [OWNERSHIP_ENV]: first.handle });
  assert.equal(joined.joined, true); assert.equal(joined.handle, first.handle); assert.equal(joined.release().released, false, "a joined gate never releases");
  assert.throws(() => join(alive, { [OWNERSHIP_ENV]: "fabricated-handle" }), /not the current owner/);
  // Stale: the previous owner released and another gate took the host; the old handle names nobody.
  assert.equal(first.release().released, true);
  const second = acquire(file, "darwin", fakeProbe("darwin"));
  assert.throws(() => join(alive, { [OWNERSHIP_ENV]: first.handle }), /not the current owner/, "a previous owner's handle cannot join the new owner");
  assert.equal(join(alive, { [OWNERSHIP_ENV]: second.handle }).joined, true);
  // The exact handle, but the owner is no longer running as the process that wrote the record (a survivor inherited the variable).
  const dead = fakeProbe("darwin", { identity: () => ({ alive: false, startedMs: null, group: null }) });
  assert.throws(() => join(dead, { [OWNERSHIP_ENV]: second.handle }), (e) => e instanceof OwnershipBusy && /no longer the process that took it/.test(e.message));
  const reused = fakeProbe("darwin", { identity: () => ({ alive: true, startedMs: NOW - 5_000, group: 1 }) });
  assert.throws(() => join(reused, { [OWNERSHIP_ENV]: second.handle }), /no longer the process that took it/, "the pid now runs another process");
  const unknown = fakeProbe("darwin", { identity: () => { throw new Error("ps broke"); } });
  assert.throws(() => join(unknown, { [OWNERSHIP_ENV]: second.handle }), (e) => e instanceof OwnershipBusy && /cannot be proven alive \(ps broke\)/.test(e.message));
  // A record of another host, or one that is not a well-formed owner record, is not joined even when the handle matches.
  const record = JSON.parse(fs.readFileSync(file, "utf8"));
  fs.writeFileSync(file, JSON.stringify({ ...record, host: "some-other-host" }));
  assert.throws(() => join(alive, { [OWNERSHIP_ENV]: second.handle }), /not a well-formed owner record of this host/);
  fs.writeFileSync(file, JSON.stringify({ handle: second.handle, pid: process.pid }));
  assert.throws(() => join(alive, { [OWNERSHIP_ENV]: second.handle }), /not a well-formed owner record of this host/);
  fs.writeFileSync(file, JSON.stringify(record));
  assert.equal(join(alive, { [OWNERSHIP_ENV]: second.handle }).joined, true, "restored: it joins again");
});

test("row 5.20 [native]: a gate for another repository started mid-stage is busy and never syncs; a nested gate under the live owner's handle joins", { timeout: 180_000 }, async (t) => {
  const repoA = tinyRepo(t); const repoB = tinyRepo(t);
  const snaps = path.join(tmp(t, "rk-own-xrepo-"), "snapshots");
  const file = ownershipFile({ platform: process.platform, beside: snaps });
  const keyB = repoKey(keyPath(repoB));
  assert.notEqual(repoKey(keyPath(repoA)), keyB, "two repositories, two keys");
  fs.mkdirSync(snaps, { recursive: true, mode: 0o700 }); fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  // A gate on repository A holds the host (the stage is running; the gate is alive).
  const g = await crashingGate(t, file, { snapshot: path.join(snaps, repoKey(keyPath(repoA))) });
  // A gate on repository B, started mid-stage, is busy and writes nothing.
  const before = fs.readdirSync(snaps).sort();
  const b = gateStep(t, "snapshot", { cwd: repoB, args: ["--commit", "HEAD", "--json"], snapshotRoot: snaps }, process.env);
  assert.equal(b.code, 1, JSON.stringify(b));
  assert.match(b.lines.join("\n"), new RegExp(`snapshot busy: build ownership .*held by pid ${g.gate}`));
  assert.deepEqual(fs.readdirSync(snaps).sort(), before, "the busy gate wrote nothing into the root");
  assert.equal(fs.existsSync(path.join(snaps, keyB)), false, "repository B was never synced");
  // A nested gate under A's live handle joins (it is A's own work).
  const nested = gateStep(t, "snapshot", { cwd: repoB, args: ["--commit", "HEAD", "--json"], snapshotRoot: snaps }, process.env, { nested: g.handle });
  assert.equal(nested.code, 0, JSON.stringify(nested));
  const r = JSON.parse(nested.lines.find((l) => l.startsWith("{")));
  assert.equal(r.ownership.joined, true); assert.equal(r.ownership.handle, g.handle);
  assert.equal(fs.existsSync(path.join(snaps, keyB)), true, "the nested gate synced");
  assert.equal(JSON.parse(fs.readFileSync(file, "utf8")).handle, g.handle, "and the record is still the live owner's: a joined gate never releases");
  // A handle that is not the live owner's cannot join: the gate is busy, not joined.
  const stale = gateStep(t, "snapshot", { cwd: repoB, args: ["--commit", "HEAD", "--json"], snapshotRoot: snaps }, process.env, { nested: "11111111-2222-3333-4444-555555555555" });
  assert.equal(stale.code, 1, JSON.stringify(stale)); assert.match(stale.lines.join("\n"), /is not the current owner/);
});

test("row 5.22: the ownership directory is created mode 0700, and an existing one that grants group or other access is refused with nothing taken", { skip: process.platform === "win32" && "POSIX mode bits: Windows has no group/other bits" }, (t) => {
  const base = tmp(t, "rk-own-mode-");
  const file = path.join(base, "a", "b", OWNERSHIP_DIRNAME, "owner.json");
  const own = acquire(file, "linux", fakeProbe("linux"));
  for (const d of [path.dirname(file), path.join(base, "a", "b"), path.join(base, "a")]) assert.equal(fs.statSync(d).mode & 0o777, 0o700, `${d} is private`);
  assert.equal(own.release().released, true);
  for (const mode of [0o755, 0o750, 0o705, 0o770, 0o777, 0o710, 0o701, 0o740, 0o704]) {
    fs.chmodSync(path.dirname(file), mode);
    assert.throws(() => acquire(file, "linux", fakeProbe("linux")), (e) => e instanceof OwnershipBusy && e.code === "busy" && new RegExp(`is mode ${mode.toString(8)}: group or other users can reach it`).test(e.message), mode.toString(8));
    assert.equal(fs.existsSync(file), false, `a ${mode.toString(8)} directory: no record was taken`);
  }
  fs.chmodSync(path.dirname(file), 0o704);
  assert.throws(() => ensureOwnershipDir(path.dirname(file), { platform: "darwin" }), /is mode 704/);
  fs.chmodSync(path.dirname(file), 0o700);
  assert.equal(acquire(file, "linux", fakeProbe("linux")).release().released, true, "owner-only again: it acquires");
  fs.writeFileSync(path.join(base, "notadir"), "x");
  assert.throws(() => ensureOwnershipDir(path.join(base, "notadir"), { platform: "linux" }), (e) => e.code === "EEXIST" || e instanceof OwnershipBusy, "a file is not an ownership directory");
});

test("row 5.23 [native macOS, Windows]: a real first acquisition on a pre-existing birthless root releases (macOS: emptied and born anew; Windows: job-based, the birth unused)", { skip: process.platform === "linux" && "Linux: the python3 native above", timeout: 180_000 }, (t) => {
  const base = tmp(t, "rk-own-first-"); const root = path.join(base, "snapshots"); const file = path.join(base, OWNERSHIP_DIRNAME, "owner.json");
  const repo = tinyRepo(t); oldRoot(root);
  assert.equal(fs.existsSync(rootBirthFile(root)), false, "a root an older gate made: no birth");
  const step = gateStep(t, "sync", { file, checkout: repo, root, snapshot: path.join(root, repoKey(keyPath(repo))) }, process.env);
  assert.equal(step.ok, true, JSON.stringify(step));
  assert.equal(step.released.released, true, `the first acquisition releases: ${JSON.stringify(step.released)}`);
  assert.equal(fs.existsSync(file), false, "the host is free again");
  assert.equal(step.reclaimed, null, "a free host, never a reclaim");
  if (process.platform === "win32") {
    assert.equal(fs.existsSync(rootBirthFile(root)), false, "Windows ownership is job-based: no birth is written or read");
    assert.equal(fs.existsSync(path.join(root, OLD_KEY)), true, "and nothing was emptied (no boot identity to migrate on)");
  } else {
    assert.equal(fs.existsSync(path.join(root, OLD_KEY)), false, "the birthless root was emptied");
    assert.notEqual(readRootBirth(root, hostBootId()), null, "and born anew in this boot");
  }
  assert.ok(fs.existsSync(path.join(root, repoKey(keyPath(repo)))), "the sync itself landed");
});

// ---------------------------------------------------------------------------------------------------------------------
// Row 5.24: ownership is held while a REAL compiler reads the snapshot (Linux direct: cargo and rustc are in the gate's tree).
// ---------------------------------------------------------------------------------------------------------------------

const REAL_TOOLS = process.platform === "linux" ? (() => { const r = spawnSync("sh", ["-c", "command -v cargo && command -v rustc"], { encoding: "utf8", env: { ...process.env, NODE_OPTIONS: "" } }); return r.status === 0 ? r.stdout.trim().split("\n") : null; })() : null;
const HAS_FIFO = process.platform === "linux" && spawnSync("mkfifo", ["--version"], { stdio: "ignore" }).status === 0;
const pidsNamed = (name) => spawnSync("pgrep", ["-x", name], { encoding: "utf8" }).stdout.split("\n").filter(Boolean).map(Number);
/** The real build-script binaries of `realCrate` (comm is cut to 15 characters, so match the command line). */
const buildScriptPids = () => spawnSync("pgrep", ["-f", "/held-[0-9a-f]+/build-script-build$"], { encoding: "utf8" }).stdout.split("\n").filter(Boolean).map(Number);
const alivePid = (pid) => { try { process.kill(pid, 0); return true; } catch { return false; } };
/**
 * The pid of the real rustc compiling the crate of `realCrate` (`--crate-name held`): it is blocked opening the FIFO the crate includes
 * until the test writes it, with its working directory in the snapshot (cargo runs it there). The compile of the build script is another
 * crate name and has ended by then.
 */
const rustcOnCrate = () => pidsNamed("rustc").find((pid) => { try { return fs.readFileSync(`/proc/${pid}/cmdline`, "utf8").includes("--crate-name\0held\0"); } catch { return false; } });

/**
 * A snapshot directory holding a real crate: a build.rs that waits until the test removes `hold-build`, and a library that rustc cannot
 * finish until the test writes the FIFO `hold-rustc` (`include_bytes!` blocks reading a FIFO until a writer closes it). So a real
 * build tool, a real build-script binary and a real rustc are each alive in turn, deterministically, reading the snapshot.
 */
function realCrate(dir) {
  fs.mkdirSync(path.join(dir, "src"), { recursive: true });
  fs.writeFileSync(path.join(dir, "Cargo.toml"), '[package]\nname = "held"\nversion = "0.0.0"\nedition = "2021"\nbuild = "build.rs"\n');
  fs.writeFileSync(path.join(dir, "build.rs"), 'fn main() { let flag = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("hold-build"); while flag.exists() { std::thread::sleep(std::time::Duration::from_millis(50)); } }\n');
  fs.writeFileSync(path.join(dir, "src", "lib.rs"), 'pub static HELD: &[u8] = include_bytes!("../hold-rustc");\n');
  fs.writeFileSync(path.join(dir, "hold-build"), "x");
  assert.equal(spawnSync("mkfifo", [path.join(dir, "hold-rustc")]).status, 0);
}

/** Starts a fake gate that takes ownership, runs the REAL build tool in the snapshot and reports; it releases only when asked. */
async function realBuildGate(t, file, snapshot, target) {
  const script = path.join(path.dirname(file), "real-build-gate.mjs"); const info = `${file}.build-info`;
  fs.writeFileSync(script, `
const { acquireBuildOwnership } = await import(${JSON.stringify(MODULE)});
const { spawn } = await import("node:child" + "_process");
const fs = await import("node:fs");
const own = acquireBuildOwnership({ file: ${JSON.stringify(file)}, stage: "compile", snapshot: ${JSON.stringify(snapshot)} });
const env = { ...process.env, CARGO_TARGET_DIR: ${JSON.stringify(target)}, CARGO_TERM_COLOR: "never" };
delete env.NODE_OPTIONS;
const build = spawn("cargo", ["build", "--offline", "--manifest-path", ${JSON.stringify(path.join(snapshot, "Cargo.toml"))}], { cwd: ${JSON.stringify(snapshot)}, env, stdio: ["ignore", "ignore", "ignore"] });
build.on("exit", (code) => fs.writeFileSync(${JSON.stringify(info)} + ".exit", String(code)));
const { execFileSync } = await import("node:child" + "_process");
const versions = ["cargo", "rustc"].map((n) => execFileSync(n, ["--version"], { env, encoding: "utf8" }).trim()).join("; ");
fs.writeFileSync(${JSON.stringify(info)}, JSON.stringify({ handle: own.handle, gate: process.pid, build: build.pid, group: own.owner.containment.group, versions }));
// The gate tries to release whenever a file asks it to, and writes the answer back.
for (;;) {
  if (fs.existsSync(${JSON.stringify(info)} + ".release-now")) { fs.rmSync(${JSON.stringify(info)} + ".release-now"); const r = own.release(); fs.writeFileSync(${JSON.stringify(info)} + ".release-answer", JSON.stringify(r)); if (r.released) process.exit(0); }
  await new Promise((r) => setTimeout(r, 50));
}
`);
  const env = { ...process.env }; delete env[OWNERSHIP_ENV]; delete env.NODE_OPTIONS;
  const gate = spawn(process.execPath, [script], { stdio: ["ignore", "ignore", "inherit"], env, detached: true });
  t.after(() => { try { process.kill(-gate.pid, "SIGKILL"); } catch { /* gone */ } for (const pid of [...buildScriptPids(), ...pidsNamed("rustc"), ...pidsNamed("cargo")]) { try { if (fs.readFileSync(`/proc/${pid}/cmdline`, "utf8").includes(path.dirname(path.dirname(file)))) process.kill(pid, "SIGKILL"); } catch { /* gone */ } } });
  await until(() => fs.existsSync(info), 60_000);
  return { ...JSON.parse(fs.readFileSync(info, "utf8")), info };
}
const askRelease = async (g) => {
  fs.rmSync(`${g.info}.release-answer`, { force: true }); fs.writeFileSync(`${g.info}.release-now`, "1");
  await until(() => fs.existsSync(`${g.info}.release-answer`), 30_000); return JSON.parse(fs.readFileSync(`${g.info}.release-answer`, "utf8"));
};

test("row 5.24 [native Linux, real cargo and rustc]: the gate holds while a real build runs; its release is refused until cargo, the build script and rustc are gone", { skip: (!REAL_TOOLS && "needs Linux with real cargo and rustc") || (!HAS_FIFO && "needs mkfifo"), timeout: 300_000 }, async (t) => {
  const base = tmp(t, "rk-own-real-"); const file = path.join(base, OWNERSHIP_DIRNAME, "owner.json"); const snapshot = path.join(base, "snapshots", "k"); const target = path.join(base, "target");
  fs.mkdirSync(path.dirname(file), { mode: 0o700 }); fs.mkdirSync(path.dirname(snapshot), { mode: 0o700 }); recordRootBirth(path.dirname(snapshot), { bootId: hostBootId() }); realCrate(snapshot);
  const g = await realBuildGate(t, file, snapshot, target);
  t.diagnostic(`real toolchain: ${REAL_TOOLS.join(", ")}; ${g.versions}`);
  // Phase 1: cargo has compiled build.rs with rustc and runs the build-script binary, which waits.
  await until(() => buildScriptPids().length > 0, 120_000);
  const buildScript = buildScriptPids()[0];
  assert.equal(alivePid(g.build), true, "a real cargo is running");
  assert.equal(fs.readFileSync(`/proc/${g.build}/comm`, "utf8").trim(), "cargo", "the gate's child is the real cargo");
  // The gate is alive, so a second gate is busy (held by pid), and the gate's own release is refused while the build's tree lives.
  const second = gateStep(t, "acquire", { file }, process.env);
  assert.equal(second.ok, false, JSON.stringify(second)); assert.equal(second.code, "busy"); assert.match(second.message, new RegExp(`held by pid ${g.gate}`));
  const refused = await askRelease(g);
  assert.equal(refused.released, false, `release while cargo builds: ${JSON.stringify(refused)}`);
  assert.match(refused.why, new RegExp(`process group ${g.group} still has \\d+ member\\(s\\) started under this ownership: .*\\b${g.build}\\b`), "the group net names the real cargo");
  assert.match(refused.why, new RegExp(`\\b${buildScript}\\b`), "and the build-script binary it is running");
  // Phase 2: the build script ends; rustc then compiles the crate and blocks reading the FIFO inside the snapshot.
  fs.rmSync(path.join(snapshot, "hold-build"));
  await until(() => rustcOnCrate() !== undefined, 120_000);
  const rustc = rustcOnCrate();
  assert.equal(fs.readFileSync(`/proc/${rustc}/comm`, "utf8").trim(), "rustc", "the real rustc");
  assert.ok(fs.readlinkSync(`/proc/${rustc}/cwd`).startsWith(snapshot), "compiling inside the snapshot");
  const refusedAgain = await askRelease(g);
  assert.equal(refusedAgain.released, false, `release while rustc reads the snapshot: ${JSON.stringify(refusedAgain)}`);
  assert.match(refusedAgain.why, new RegExp(`\\b${rustc}\\b`), "the real rustc is named");
  // Phase 3: rustc is released from the FIFO; cargo finishes; only then does the gate release.
  fs.writeFileSync(path.join(snapshot, "hold-rustc"), "bytes");
  await until(() => !alivePid(g.build) && !alivePid(rustc), 120_000);
  assert.equal(fs.readFileSync(`${g.info}.exit`, "utf8"), "0", "the real build succeeded");
  const released = await askRelease(g);
  assert.equal(released.released, true, `released once cargo and rustc are gone: ${JSON.stringify(released)}`);
  assert.equal(fs.existsSync(file), false, "the host is free again");
});

test("row 5.24 [native Linux, real cargo and rustc]: the gate SIGKILLed mid-compile: a second SYNC is refused while cargo or rustc lives, and succeeds once they exit", { skip: (!REAL_TOOLS && "needs Linux with real cargo and rustc") || (!HAS_FIFO && "needs mkfifo"), timeout: 300_000 }, async (t) => {
  const base = tmp(t, "rk-own-real-kill-"); const file = path.join(base, OWNERSHIP_DIRNAME, "owner.json"); const snapshot = path.join(base, "snapshots", "k"); const target = path.join(base, "target");
  fs.mkdirSync(path.dirname(file), { mode: 0o700 }); fs.mkdirSync(path.dirname(snapshot), { mode: 0o700 }); recordRootBirth(path.dirname(snapshot), { bootId: hostBootId() }); realCrate(snapshot);
  const repo = tinyRepo(t); const root = path.dirname(snapshot);
  assert.equal(ownershipFile({ platform: "linux", beside: root }), file, "the sync below contends for this very record");
  const sync = () => gateStep(t, "snapshot", { cwd: repo, args: ["--commit", "HEAD", "--json"], snapshotRoot: root }, process.env);
  const g = await realBuildGate(t, file, snapshot, target);
  await until(() => buildScriptPids().length > 0, 120_000);
  const buildScript = buildScriptPids()[0];
  process.kill(g.gate, "SIGKILL"); await until(() => !alivePid(g.gate), 10_000);
  assert.equal(alivePid(g.build), true, "the real cargo outlives its gate");
  // While cargo and its build script live: the sync is refused, names them, and writes nothing.
  let busy = sync();
  assert.equal(busy.code, 1, JSON.stringify(busy));
  assert.match(busy.lines.join("\n"), new RegExp(`snapshot busy: build ownership .*owner pid ${g.gate} is gone but .*\\b${g.build}\\b`), "cargo is named");
  assert.equal(fs.existsSync(path.join(root, repoKey(keyPath(repo)))), false, "nothing was synced");
  // The build script ends and rustc blocks opening the FIFO: still refused, now naming the real rustc.
  fs.rmSync(path.join(snapshot, "hold-build"));
  await until(() => rustcOnCrate() !== undefined, 120_000);
  const rustc = rustcOnCrate();
  busy = sync();
  assert.equal(busy.code, 1, JSON.stringify(busy));
  assert.match(busy.lines.join("\n"), new RegExp(`snapshot busy: .*\\b${rustc}\\b`), "the real rustc is named");
  assert.equal(fs.existsSync(path.join(root, repoKey(keyPath(repo)))), false, "still nothing synced");
  // They exit; the next sync reclaims with the proof and syncs.
  fs.writeFileSync(path.join(snapshot, "hold-rustc"), "bytes");
  await until(() => !alivePid(g.build) && !alivePid(rustc) && !alivePid(buildScript), 120_000);
  const next = sync();
  assert.equal(next.code, 0, JSON.stringify(next));
  const r = JSON.parse(next.lines.find((l) => l.startsWith("{")));
  assert.equal(r.ownership.reclaimed.from, g.handle); assert.equal(fs.existsSync(path.join(root, repoKey(keyPath(repo)))), true, "synced");
  assert.equal(fs.existsSync(file), false, "and released");
});

// ---------------------------------------------------------------------------------------------------------------------
// Row 5.24a' (signed/item5-sage-decisions-2.md D1 and D2): macOS processes whose environment `ps eww` hides (every Apple platform
// binary): held after the anchor unless lineage clears them; a launchd top is judged by `launchctl blame`. Injected tables; the recorded
// real-Mac table; the natives judge only processes the test itself created.
// ---------------------------------------------------------------------------------------------------------------------

const T = (s) => Date.UTC(2026, 9, 29, 10, 0, s);   // 10:00:ss
const ANCHOR = T(5) + 500;                            // 10:00:05.500: the anchor second is 10:00:05, and an anchor-second start counts as after it
const hp = (pid, ppid, startedMs, over = {}) => ({ pid, ppid, group: pid, startedMs, uid: 501, state: "S", hidden: true, uninspected: false, command: `/bin/p${pid}`, ...over });
const MANAGED = (line = "ipc (mach)") => ({ rc: 0, line });
const UNMANAGED = { rc: 3, line: NOT_MANAGED_LINE };
const LAUNCHD = hp(1, 0, T(-3 * 86400), { group: 1, uid: 0, hidden: false, command: "/sbin/launchd" });
const TERMINAL = hp(100, 1, T(1), { hidden: false, command: "Terminal" });         // predates the anchor, not launchd
const LOGIN = hp(110, 100, T(7), { command: "/usr/bin/login" });
const ZSH = hp(120, 110, T(8), { command: "-zsh" });                                // the shell the gate runs from
const GATE = hp(200, 120, T(10), { hidden: false, command: "node gate.mjs" });
const OWNER = { pid: 200, startedMs: T(10), acquiredAt: new Date(T(10)).toISOString(), containment: { kind: "group", group: 200 }, ancestors: [{ pid: 120, startedMs: T(8) }, { pid: 110, startedMs: T(7) }, { pid: 100, startedMs: T(1) }] };
const PRE = hp(60, 1, T(2), { hidden: false, command: "/Applications/Agent" });  // a process that predates the anchor and is not launchd
const world = (...extra) => [LAUNCHD, TERMINAL, LOGIN, ZSH, GATE, PRE, ...extra];
/** `reread` of the table itself: each pid's parent and start as listed (`changes` overrides per pid, for the pid-reuse cases). */
const rereadOf = (procs, changes = {}) => (pids) => new Map(pids.filter((p) => procs.some((x) => x.pid === p)).map((p) => { const x = procs.find((q) => q.pid === p); return [p, { ppid: x.ppid, startedMs: x.startedMs, ...changes[p] }]; }));
/** Classifies `procs`; `judge.calls` lists the pids `blame` was asked about. Default: nothing is managed by launchd. */
function judge(procs, over = {}) {
  const calls = []; const answer = over.blame ?? (() => UNMANAGED);
  const held = classifyHiddenProcesses({ procs, own: new Set([200]), uid: 501, owner: OWNER, anchorMs: ANCHOR, reread: rereadOf(procs, over.changes), ...over, blame: (pid) => { calls.push(pid); return answer(pid); } });
  judge.calls = calls; return held;
}
const heldPids = (held) => held.map((h) => h.pid).sort((a, b) => a - b);
const namesPid = (held, pid) => held.some((h) => h.pid === pid && new RegExp(`pid ${pid}\\b`).test(h.why));

test("row 5.24a' [injected table]: a hidden process drops by its start before the anchor, under a launchd job (every blame kind seen on a real Mac), under a pre-anchor ancestor, or at the record's acquisition when there is no baseline; others are not candidates", () => {
  assert.deepEqual(judge(world(hp(300, 1, T(4)))), [], "before the anchor second: dropped, though it is an unmanaged orphan");
  assert.deepEqual(judge.calls, [], "and launchctl is not asked");
  for (const line of ["ipc (mach)", "xpc event", "launch job demand", "speculative", "incomplete work", "non-ipc demand", "semaphore", "system support", "ipc (socket)"]) {
    assert.deepEqual(judge(world(hp(310, 1, T(8))), { blame: () => MANAGED(line) }), [], `a launchd job (${line}) after the anchor drops`);
  }
  // Terminal (itself a launchd job), login, zsh and /bin/sleep, all after the anchor: only the top is asked, once.
  const shell = world(hp(320, 1, T(7), { command: "Terminal" }), hp(321, 320, T(8), { command: "/usr/bin/login" }), hp(322, 321, T(9), { command: "-zsh" }), hp(323, 322, T(10), { command: "/bin/sleep 60" }));
  assert.deepEqual(judge(shell, { blame: () => MANAGED("xpc event") }), [], "an ordinary shell under a post-anchor Terminal");
  assert.deepEqual(judge.calls, [320], "blame is asked for the top only, once");
  assert.deepEqual(judge(world(hp(121, 100, T(7), { command: "/usr/bin/login" }), hp(125, 121, T(8), { command: "-zsh" }), hp(126, 125, T(9), { command: "/bin/sleep 60" }))), [], "under a pre-anchor, not-launchd Terminal");
  assert.deepEqual(judge.calls, [], "no blame for a lineage that ends at a pre-anchor process");
  // No baseline: the anchor is the record's acquisition (T(10)); an unmanaged orphan from before it drops, so the gate never wedges on it.
  assert.deepEqual(judge(world(hp(330, 1, T(9))), { anchorMs: NaN }), [], "no baseline: started before acquiredAt");
  assert.deepEqual(judge(world(hp(331, 1, T(8), { hidden: false, uninspected: true })), { anchorMs: NaN }), [], "an uninspected one too");
  assert.deepEqual(judge(world(hp(310, 1, T(11))), { anchorMs: NaN, blame: () => MANAGED() }), [], "no baseline, after acquiredAt, under a launchd job: dropped");
  // Not candidates.
  assert.deepEqual(judge(world(hp(340, 1, T(8), { uid: 0 }))), [], "another user's hidden process is skipped (the 0700 premise of row 5.22)");
  assert.deepEqual(judge(world(hp(341, 1, T(8), { hidden: false }))), [], "a visible-environment process that lsof inspected");
  assert.deepEqual(judge(world(hp(342, 1, T(8), { state: "Z" }))), [], "a zombie reads nothing");
  assert.deepEqual(judge(world(hp(343, 1, T(11), { hidden: false, uninspected: true }), hp(344, 1, T(3), { hidden: false, uninspected: true }))).map((h) => h.pid), [343], "lsof could not inspect a visible one: classified (the pre-anchor one drops)");
  assert.deepEqual(judge(world(hp(345, 1, T(11))), { alive: () => false }), [], "a process that exited before the verdict is no reader");
  assert.deepEqual(judge(world()), [], "the gate, its ancestors and the listing it ran");
});

test("row 5.24a' [injected table]: held, each reason naming the pid: a top that is not managed, a blame that gives no usable answer, a top that is not the same process on the re-read, traced, no baseline but started at the acquisition", () => {
  let h = judge(world(hp(401, 1, T(11))));
  assert.deepEqual(heldPids(h), [401]); assert.ok(namesPid(h, 401)); assert.match(h[0].why, /not managed by launchd/);
  assert.deepEqual(judge.calls, [401]);
  for (const [name, blame] of [["another exit code", () => ({ rc: 1, line: "boom" })], ["empty output", () => ({ rc: 0, line: "" })], ["the not-managed text with exit 0 (a pipeline's exit, as in the lead's probe script)", () => ({ rc: 0, line: NOT_MANAGED_LINE })], ["a spawn error", () => { throw new Error("spawn launchctl ENOENT"); }], ["a timeout", () => ({ rc: null, line: "spawnSync launchctl ETIMEDOUT" })], ["no answer", () => null], ["exit 3 with other text", () => ({ rc: 3, line: "something else" })]]) {
    h = judge(world(hp(402, 1, T(11))), { blame });
    assert.deepEqual(heldPids(h), [402], name); assert.ok(namesPid(h, 402), name); assert.match(h[0].why, /no usable answer/, name);
  }
  // Binding against pid reuse: the cleared top is re-listed after the blame calls.
  const tops = world(hp(410, 1, T(8)), hp(411, 410, T(9)));
  assert.deepEqual(judge(tops, { blame: () => MANAGED() }), [], "unchanged: dropped");
  h = judge(tops, { blame: () => MANAGED(), changes: { 410: { startedMs: T(8) + 5000 } } });
  assert.deepEqual(heldPids(h), [410, 411], "another lstart on the re-read"); assert.match(h[0].why, /not the same process on the re-read/);
  h = judge(tops, { blame: () => MANAGED(), changes: { 410: { ppid: 77 } } });
  assert.deepEqual(heldPids(h), [410, 411], "a ppid other than 1 on the re-read");
  h = judge(tops, { blame: () => MANAGED(), reread: () => new Map() });
  assert.deepEqual(heldPids(h), [410, 411]); assert.match(h[0].why, /gone on the re-read/);
  h = judge(tops, { blame: () => MANAGED(), reread: () => { throw new Error("ps -p"); } });
  assert.deepEqual(heldPids(h), [410, 411]); assert.match(h[0].why, /could not be re-read/);
  h = judge(tops, { blame: () => MANAGED(), reread: null });
  assert.deepEqual(heldPids(h), [410, 411], "no re-read at all: the answer is not bound");
  // Traced (stat X): P itself, or a process on its walk.
  h = judge(world(hp(420, 1, T(11), { state: "SX" })), { blame: () => MANAGED() });
  assert.deepEqual(heldPids(h), [420]); assert.match(h[0].why, /traced/);
  h = judge(world(hp(430, 1, T(8)), hp(431, 430, T(9), { state: "SX" }), hp(432, 431, T(10))), { blame: () => MANAGED() });
  assert.deepEqual(heldPids(h), [431, 432], "an ancestor on the walk is traced"); assert.match(h.find((x) => x.pid === 432).why, /pid 431 on its walk is traced/);
  h = judge(world(hp(433, 1, T(3), { state: "SX" })));
  assert.deepEqual(heldPids(h), [433], "traced, even before the anchor");
  // No baseline: an unmanaged orphan that started at or after the record's acquisition (the second counts as after) is held.
  h = judge(world(hp(440, 1, T(10)), hp(441, 1, T(9))), { anchorMs: NaN });
  assert.deepEqual(heldPids(h), [440]); assert.match(h[0].why, /not managed by launchd/);
  h = judge(world(hp(442, 60, T(11))), { anchorMs: NaN, owner: { ...OWNER, acquiredAt: "garbage" } });
  assert.deepEqual(heldPids(h), [442]); assert.match(h[0].why, /no readable acquisition time/);
});

test("row 5.24a' [injected table]: held by the owner's group, a recorded detached group, the owner or a recorded child on the walk, an unreadable parent, pid reuse, a walk past the limit, the anchor second, an unreadable start", () => {
  let h = judge(world(hp(450, 60, T(11), { group: 200 })));
  assert.deepEqual(heldPids(h), [450]); assert.ok(namesPid(h, 450)); assert.match(h[0].why, /process group 200, the owner's group/);
  const recorded = [{ group: 500, command: "/bin/sh" }];
  h = judge(world(hp(501, 60, T(11), { group: 500 })), { recorded });
  assert.deepEqual(heldPids(h), [501]); assert.match(h[0].why, /a detached group this gate recorded/);
  h = judge(world(hp(500, 60, T(11), { group: 500 }), hp(510, 500, T(12), { group: 510 })), { recorded });
  assert.deepEqual(heldPids(h), [500, 510]); assert.match(h.find((x) => x.pid === 510).why, /reaches a child this gate recorded \(pid 500\)/);
  h = judge(world(hp(515, 200, T(12), { group: 515 }), hp(516, 515, T(13), { group: 516 })));
  assert.deepEqual(heldPids(h), [515, 516], "a child of the owner, and its child"); assert.match(h.find((x) => x.pid === 516).why, /reaches the owner \(pid 200\)/);
  h = judge(world(hp(430, 9999, T(11))));
  assert.deepEqual(heldPids(h), [430]); assert.match(h[0].why, /the parent of pid 430 \(pid 9999\) cannot be read/);
  h = judge(world(hp(460, 461, T(11)), hp(461, 60, T(12))));
  assert.deepEqual(heldPids(h), [460], "461's parent predates the anchor: it drops"); assert.match(h[0].why, /pid 461 started after its child pid 460 \(pid reuse\)/);
  const N = HIDDEN_WALK_LIMIT + 76;
  const chain = Array.from({ length: N }, (_, i) => hp(10_000 + i, i === N - 1 ? 60 : 10_001 + i, T(11), { group: 10_000 + i }));
  h = judge(world(...chain));
  assert.ok(h.some((x) => x.pid === 10_000 && /the ancestry walk passed 1024 steps/.test(x.why)), "the deepest member is held for the walk limit");
  assert.equal(h.some((x) => x.pid === 10_000 + N - 1), false, "the member nearest the pre-anchor ancestor drops");
  h = judge(world(hp(470, 1, T(5)), hp(471, 1, T(4))));
  assert.deepEqual(heldPids(h), [470], "a start in the anchor's own second counts as after it; one second earlier drops");
  h = judge(world(hp(480, 60, NaN)));
  assert.deepEqual(heldPids(h), [480]); assert.match(h[0].why, /its start time cannot be read/);
  h = judge(world(hp(490, 1, T(11), { hidden: false, uninspected: true })));
  assert.match(h[0].why, /`lsof` cannot inspect it/);
  // Never by default: the same hidden orphan, visible and inspected, is cleared; hidden and unmanaged, it is held.
  assert.deepEqual(judge(world(hp(495, 1, T(11), { hidden: false }))), []);
  assert.deepEqual(heldPids(judge(world(hp(495, 1, T(11))))), [495]);
});

test("row 5.24a' D2 [injected table]: `gate | tee` and `{ gate; } | tee` are not held (the owner's group, but a child of a recorded ancestor, still walked); every other owner-group process is", () => {
  const tee = (pid, ppid, over = {}) => hp(pid, ppid, T(10), { group: 200, command: "/usr/bin/tee", ...over });
  assert.deepEqual(judge(world(tee(340, 120))), [], "`gate | tee`: the parent is ancestors[0]");
  assert.deepEqual(judge(world(tee(341, 110))), [], "`{ gate; } | tee`: the parent is ancestors[1]");
  assert.deepEqual(judge(world(hp(350, 120, T(11)), hp(351, 350, T(12)))), [], "the ancestor's other children, and theirs");
  // Negatives.
  let h = judge(world(tee(342, 200)));
  assert.deepEqual(heldPids(h), [342], "a child of the gate itself in the owner's group"); assert.match(h[0].why, /the owner's group/);
  const reused = [LAUNCHD, TERMINAL, LOGIN, { ...ZSH, startedMs: T(30) }, GATE, PRE, tee(343, 120, { startedMs: T(31) })];
  assert.deepEqual(heldPids(judge(reused)).includes(343), true, "a parent with an ancestor's pid but another start (pid reuse)");
  h = judge(world(tee(344, 120, { state: "SX" })));
  assert.deepEqual(heldPids(h), [344], "P traced"); assert.match(h[0].why, /traced/);
  h = judge([LAUNCHD, TERMINAL, LOGIN, { ...ZSH, state: "SX" }, GATE, PRE, tee(345, 120)]);
  assert.deepEqual(heldPids(h), [345], "the parent (a recorded ancestor) is traced: no exemption"); assert.match(h[0].why, /the owner's group/);
  h = judge(world(hp(346, 120, T(11), { group: 500 })), { recorded: [{ group: 500, command: "cargo" }] });
  assert.deepEqual(heldPids(h), [346], "a member of a recorded detached group whose parent is an ancestor: no exemption"); assert.match(h[0].why, /detached group this gate recorded/);
  // The exemption is never an immediate drop: an exempted process whose walk ends at a launchd top that is not managed is held.
  const owner2 = { ...OWNER, ancestors: [{ pid: 120, startedMs: T(8) }, { pid: 130, startedMs: T(7) }] };
  const mini = [LAUNCHD, hp(130, 1, T(7), { command: "sshd-session" }), hp(120, 130, T(8), { command: "-zsh" }), GATE, tee(347, 120)];
  h = judge(mini, { owner: owner2 });
  assert.deepEqual(heldPids(h), [347]); assert.match(h[0].why, /pid 130 is an orphan.*not managed by launchd/);
  assert.deepEqual(judge(mini, { owner: owner2, blame: () => MANAGED("ipc (socket)") }), [], "the same walk under a launchd job (an ssh session) drops");
});

const DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]; const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const lstart = (ms) => { const d = new Date(ms); return `${DAYS[d.getDay()]} ${MONTHS[d.getMonth()]} ${String(d.getDate()).padStart(2, " ")} ${[d.getHours(), d.getMinutes(), d.getSeconds()].map((n) => String(n).padStart(2, "0")).join(":")} ${d.getFullYear()}`; };
/**
 * A `spawn` standing in for macOS ps/lsof/launchctl, built from a process table. `env`: pid -> what `ps eww` appends (undefined: nothing, so
 * the environment is hidden). `blame`: pid -> { rc, line } (default: not managed). `changes`: pid -> what the `ps -p` re-read shows instead.
 * `seen` collects the options each launchctl call got.
 */
function macSpawn(procs, { env = {}, eww = {}, lsof = "all", blame = {}, changes = {}, psFail = false, ewwFail = false, lsofStatus = 0, seen = [] } = {}) {
  return (command, args, options) => {
    const done = (stdout, status = 0, stderr = "") => ({ status, stdout, stderr: status ? (stderr || "boom") : stderr, pid: 900_000 + Math.floor(Math.random() * 1000) });
    if (command === "launchctl") {
      seen.push({ args, options }); const b = blame[args[1]] ?? UNMANAGED;
      return b === "spawn-error" ? { error: new Error("spawn launchctl ENOENT"), status: null, stdout: "", stderr: "" } : done(b.rc === 0 ? `${b.line}\n` : "", b.rc, b.rc === 0 ? "" : `${b.line}\n`);
    }
    if (command === "ps" && args[0] === "eww") return ewwFail ? done("", 1) : done(procs.map((p) => `${String(p.pid).padStart(6)}     ${eww[p.pid] ?? (env[p.pid] === undefined ? p.command : `${p.command} ${env[p.pid]}`)}`).join("\n"));
    if (command === "ps" && args.includes("-p")) {
      const want = String(args[args.indexOf("-p") + 1]).split(",").map(Number);
      return done(procs.filter((p) => want.includes(p.pid)).map((p) => `${String(p.pid).padStart(6)} ${String(changes[p.pid]?.ppid ?? p.ppid).padStart(6)} ${lstart(changes[p.pid]?.startedMs ?? p.startedMs)}`).join("\n"));
    }
    if (command === "ps") return psFail ? done("", 1) : done(procs.map((p) => `${String(p.pid).padStart(6)} ${String(p.ppid).padStart(6)} ${String(p.group).padStart(6)} ${String(p.uid).padStart(5)} ${p.state.padEnd(3)} ${Number.isFinite(p.startedMs) ? lstart(p.startedMs) : "garbage"}     ${p.command}`).join("\n"));
    if (command === "lsof") return done((lsof === "all" ? procs : procs.filter((p) => lsof.includes(p.pid))).map((p) => `p${p.pid}\nfcwd\nn/`).join("\n"), lsofStatus);
    throw new Error(`unexpected ${command}`);
  };
}
const macProbe = (spawn) => processProbe({ platform: "darwin", spawn, kill: () => true, uid: 501 });

test("row 5.24a' [injected ps/eww/lsof/launchctl]: hidden is `ps eww` showing nothing beyond the command; launchctl blame runs under LC_ALL=C with a 5 s limit; a failed listing, a spawn error or a re-read that differs is held", () => {
  const orphan = (pid, over = {}) => hp(pid, 1, T(11), { hidden: false, ...over });
  const procs = world(
    orphan(601, { command: "/bin/sleep 60" }),                     // eww line = its command: hidden
    orphan(602, { command: "/usr/bin/env -i /bin/sleep 61" }),     // an empty environment prints nothing more either: hidden (fail closed)
    orphan(603, { command: "/bin/zsh -c x" }),                     // eww shows another command line (it exec'd between the two listings): hidden
    orphan(604, { command: "/opt/homebrew/bin/node a.js" }),       // shows its environment, lsof inspected it: cleared
    orphan(605, { command: "/opt/homebrew/bin/node b.js" }),       // shows its environment but lsof could not inspect it: classified
    orphan(606, { command: "/bin/sleep 62" }),                     // no eww line at all: hidden
    hp(607, 1, T(11), { command: "/usr/sbin/mdworker" }),          // hidden, but a launchd job
  );
  const env = { 604: "PATH=/x FOO=1", 605: "PATH=/x FOO=2", 200: "PATH=/x", 60: "A=b", 100: "A=b", 1: "A=b" };
  const seen = [];
  const base = macSpawn(procs, { env, eww: { 603: "/bin/sh" }, lsof: procs.map((p) => p.pid).filter((p) => p !== 605), blame: { 607: MANAGED("xpc event") }, seen });
  const noLine = (c, a, o) => { const r = base(c, a, o); if (c === "ps" && a[0] === "eww") r.stdout = r.stdout.split("\n").filter((l) => !/^\s*606\s/.test(l)).join("\n"); return r; };
  const held = macProbe(noLine).hiddenHolders({ owner: OWNER, anchorMs: ANCHOR, recorded: [] });
  assert.deepEqual(heldPids(held), [601, 602, 603, 605, 606], "604 is visible and inspected: cleared; 607 is a launchd job: dropped");
  assert.match(held.find((h) => h.pid === 605).why, /`lsof` cannot inspect it/);
  assert.match(held.find((h) => h.pid === 601).why, /environment is hidden/);
  assert.match(held.find((h) => h.pid === 601).why, /not managed by launchd/);
  assert.ok(seen.length >= 1 && seen.every((c) => c.args[0] === "blame" && c.options.timeout === 5000 && c.options.env.LC_ALL === "C"), "launchctl blame: LC_ALL=C, 5 s");
  assert.equal(new Set(seen.map((c) => c.args[1])).size, seen.length, "one answer per pid within a sample");
  // A spawn error of launchctl is unknown, so held; a re-read that shows another start holds the processes under the cleared top.
  const err = macProbe(macSpawn(procs, { env, blame: { 601: "spawn-error", 607: MANAGED() } })).hiddenHolders({ owner: OWNER, anchorMs: ANCHOR });
  assert.match(err.find((h) => h.pid === 601).why, /no usable answer/);
  const under = [...procs, hp(608, 607, T(12), { command: "/bin/sleep 63" })];
  assert.equal(macProbe(macSpawn(under, { env, blame: { 607: MANAGED() } })).hiddenHolders({ owner: OWNER, anchorMs: ANCHOR }).some((h) => h.pid === 608), false, "under a launchd job, re-read unchanged: dropped");
  const reused = macProbe(macSpawn(under, { env, blame: { 607: MANAGED() }, changes: { 607: { startedMs: T(30) } } })).hiddenHolders({ owner: OWNER, anchorMs: ANCHOR });
  assert.ok(reused.some((h) => h.pid === 608 && /not the same process on the re-read/.test(h.why)), "the cleared top reads as another process: held");
  // The traced flag is the `ps stat` column.
  const traced = macProbe(macSpawn(world(hp(609, 1, T(11), { state: "SX" })), { env, blame: { 609: MANAGED() } })).hiddenHolders({ owner: OWNER, anchorMs: ANCHOR });
  assert.deepEqual(heldPids(traced), [609]); assert.match(traced[0].why, /traced/);
  for (const [name, opts] of [["ps fails", { psFail: true }], ["ps eww fails", { ewwFail: true }], ["lsof exits non-zero", { lsofStatus: 1 }]]) {
    assert.throws(() => macProbe(macSpawn(procs, { env, ...opts })).hiddenHolders({ owner: OWNER, anchorMs: ANCHOR }), /boom|exit/, name);
  }
  assert.equal(processProbe({ platform: "linux", spawn: scripted(), proc: PROC }).hiddenHolders({ owner: OWNER, anchorMs: ANCHOR }), null, "Linux has no such net: it reads /proc/<pid>/environ under the unreadable rule");
  assert.equal(processProbe({ platform: "win32" }).hiddenHolders, undefined, "Windows has none: job containment");
  assert.throws(() => processProbe({ platform: "darwin", spawn: macSpawn(procs), kill: () => true, uid: NaN }).hiddenHolders({ owner: OWNER, anchorMs: ANCHOR }), /user id is unknown/, "without this process's user id nothing can be told from another user's");
});

test("row 5.24a' [recorded real-Mac table replay]: with the anchor ten minutes before the capture nothing is held, whatever launchd started in that window; one injected not-managed hidden orphan is held, and only it", () => {
  const fx = loadMacTable(); const cap = capturedMs(fx);
  const owner = { pid: 999_999, startedMs: cap - 3_000_000, acquiredAt: new Date(cap - 600_000).toISOString(), containment: { kind: "group", group: 999_999 }, ancestors: [] };
  const probe = (overlay = []) => processProbe({ platform: "darwin", spawn: replaySpawn(fx, { uid: 501, overlay }), kill: () => true, uid: 501 });
  const window = fx.rows.filter((r) => r.self && r.hidden && r.ppid === 1 && r.blame?.rc === 0 && Date.parse(r.lstart) >= cap - 600_000);
  assert.ok(window.length >= 1, `the recording holds launchd jobs started within its last ten minutes (${window.length}), the case that wedged the first rule`);
  assert.deepEqual(probe().hiddenHolders({ owner, anchorMs: cap - 600_000, recorded: [] }), [], "A = capture minus 10 minutes");
  assert.deepEqual(probe().hiddenHolders({ owner, anchorMs: NaN, recorded: [] }), [], "no baseline: the anchor is the record's acquisition, the same ten minutes");
  assert.deepEqual(probe().hiddenHolders({ owner, anchorMs: cap - 6 * 3600_000, recorded: [] }), [], "a six hour anchor");
  const orphan = { pid: 777_001, ppid: 1, pgid: 777_001, self: true, stat: "S", lstart: fx.capturedAt, argv0: "sleep", hidden: true, lsofInspected: true, blame: { rc: 3, line: NOT_MANAGED_LINE } };
  const held = probe([orphan]).hiddenHolders({ owner, anchorMs: cap - 600_000, recorded: [] });
  assert.deepEqual(heldPids(held), [777_001], "exactly the injected orphan"); assert.match(held[0].why, /not managed by launchd/);
  assert.deepEqual(heldPids(probe([{ ...orphan, blame: { rc: 0, line: "xpc event" } }]).hiddenHolders({ owner, anchorMs: cap - 600_000 })), [], "the same row as a launchd job: dropped");
});

test("row 5.24a' [injected]: the anchor is the root's birth (capped at the acquisition) or the last clean release, else the record's own acquisition; a missing birth, another boot's birth or an unreadable boot id never means 'no anchor'", (t) => {
  const base = tmp(t, "rk-own-anchor-"); const root = path.join(base, "snapshots"); fs.mkdirSync(root, { mode: 0o700 });
  const file = path.join(base, OWNERSHIP_DIRNAME, "owner.json"); const snapshot = path.join(root, OLD_KEY);
  const sec = (ms) => Math.floor(ms / 1000) * 1000;
  const acquiredAt = NOW - 100_000;
  const table = (...extra) => [{ ...LAUNCHD, startedMs: NOW - 9e7 }, hp(50, 1, sec(NOW - 7_200_000), { hidden: false, command: "Terminal" }), ...extra];
  const probeFor = (procs, over = {}, blames = {}) => fakeProbe("darwin", {
    bootId: () => "boot-1", tokenHolders: () => [], snapshotUsers: () => [],
    hiddenHolders: (ctx) => classifyHiddenProcesses({ procs, own: new Set([process.pid]), uid: 501, ...ctx, alive: () => true, blame: (pid) => blames[pid] ?? UNMANAGED, reread: rereadOf(procs) }), ...over,
  });
  const judged = (procs, ownerOver = {}, probeOver = {}, blames = {}) => {
    const rec = deadOwner(file, "darwin", { snapshot, acquiredAt: iso(acquiredAt), ancestors: [], ...ownerOver });
    return judgeOwner(JSON.stringify(rec), { file, probe: probeFor(procs, probeOver, blames), lease: noLease });
  };
  const old = hp(60, 1, sec(NOW - 3_600_000));          // an hour old, hidden, an unmanaged orphan
  const shellKid = hp(61, 50, sec(NOW - 50_000), { command: "-zsh" }); const kid2 = hp(62, 61, sec(NOW - 40_000));
  const clean = table(old, shellKid, kid2);
  const after = (pid, at = NOW - 30_000, over = {}) => hp(pid, 1, sec(at), over);
  // The 5.23 rewrites: no birth and no clean marker. Before the acquisition: dropped. After it: held.
  assert.doesNotMatch(judged(clean).proofs.join("; "), /reported, not held/, "no birth: the old orphan predates the acquisition");
  assert.match(judged([...clean, after(70)]).proofs.join("; "), /reported, not held:.*70: pid 70\b.*not managed by launchd/s);
  // With a birth of this boot, before the acquisition: the anchor.
  recordRootBirth(root, { now: () => NOW - 200_000, bootId: "boot-1" });
  assert.doesNotMatch(judged(clean).proofs.join("; "), /reported, not held/);
  assert.match(judged([...clean, after(71)]).proofs.join("; "), /reported, not held:.*71: pid 71\b/s);
  assert.doesNotMatch(judged([...clean, after(72)], {}, {}, { 72: MANAGED() }).proofs.join("; "), /reported, not held/, "after the anchor, under a launchd job");
  // The birth is capped at the acquisition: a birth recorded AFTER it does not move the anchor later.
  fs.rmSync(rootBirthFile(root)); recordRootBirth(root, { now: () => NOW - 10_000, bootId: "boot-1" });
  assert.match(judged([...clean, after(73, NOW - 50_000)]).proofs.join("; "), /reported, not held:.*73: pid 73\b/s, "started after the acquisition but before the (late) birth: after the anchor");
  assert.doesNotMatch(judged([...clean, after(74, NOW - 200_000)]).proofs.join("; "), /reported, not held/, "started before the acquisition: before the anchor");
  // Another boot's birth, an unreadable boot id: the same pair as no birth.
  fs.rmSync(rootBirthFile(root)); recordRootBirth(root, { now: () => NOW - 200_000, bootId: "boot-0" });
  assert.doesNotMatch(judged(clean).proofs.join("; "), /reported, not held/, "another boot's birth: before the acquisition dropped");
  assert.match(judged([...clean, after(75)]).proofs.join("; "), /reported, not held:.*75: pid 75\b/s, "another boot's birth: after the acquisition held");
  assert.doesNotMatch(judged(clean, {}, { bootId: () => null }).proofs.join("; "), /reported, not held/, "an unreadable boot id: before the acquisition dropped");
  assert.match(judged([...clean, after(76)], {}, { bootId: () => null }).proofs.join("; "), /reported, not held:.*76: pid 76\b/s, "an unreadable boot id: after the acquisition held");
  assert.match(judged([...clean, after(77)], {}, { bootId: () => { throw new Error("sysctl"); } }).proofs.join("; "), /reported, not held:.*77: pid 77\b/s);
  // The last clean release is an anchor too; a record with no snapshot at all anchors at its acquisition (and releases clean under launchd jobs).
  fs.rmSync(rootBirthFile(root));
  assert.doesNotMatch(judged(clean, { cleanBefore: iso(NOW - 500_000) }).proofs.join("; "), /reported, not held/);
  const noSnapshot = judged([...clean, after(78), after(79)], { snapshot: null }, {}, { 78: MANAGED("launch job demand"), 79: MANAGED("xpc event") });
  assert.doesNotMatch(noSnapshot.proofs.join("; "), /reported, not held/, "snapshot: null, only launchd jobs after the anchor: nothing reported");
  assert.match(judged([...clean, after(80)], { snapshot: null }).proofs.join("; "), /reported, not held:.*80: pid 80\b/s, "snapshot: null, an unmanaged orphan after the acquisition: held");
  // A recorded detached group is held through the registry.
  recordRootBirth(root, { now: () => NOW - 200_000, bootId: "boot-1" });
  const rec = deadOwner(file, "darwin", { snapshot, acquiredAt: iso(acquiredAt), ancestors: [] });
  const reg = childrenDir(file, rec.handle); fs.mkdirSync(reg, { recursive: true });
  fs.writeFileSync(path.join(reg, "group-800.json"), JSON.stringify({ group: 800, startedMs: NOW - 90_000, command: "cargo" }));
  assert.throws(() => judgeOwner(JSON.stringify(rec), { file, probe: probeFor([...clean, hp(801, 50, sec(NOW - 30_000), { group: 800 })], { groupSnapshot: (g) => ({ members: g === 800 ? [{ pid: 801, startedMs: NOW - 30_000 }] : [] }) }), lease: noLease }), (e) => e instanceof OwnershipBusy && /detached process group 800 .*801/s.test(e.message), "a recorded detached group is still held by its tracked lineage");
});

test("rightkit#1313 [injected]: an unattributed hidden-environment process started after the anchor is reported and never held; tracked groups, intents, the exact token and actual snapshot users still hold", (t) => {
  const base = tmp(t, "rk-own-1313-"); const root = path.join(base, "snapshots"); fs.mkdirSync(root, { mode: 0o700 });
  const file = path.join(base, OWNERSHIP_DIRNAME, "owner.json"); const snapshot = path.join(root, OLD_KEY);
  const sec = (ms) => Math.floor(ms / 1000) * 1000;
  const acquiredAt = NOW - 100_000;
  const table = (...extra) => [{ ...LAUNCHD, startedMs: NOW - 9e7 }, hp(50, 1, sec(NOW - 7_200_000), { hidden: false, command: "Terminal" }), ...extra];
  const unrelated = hp(900, 1, sec(NOW - 30_000), { command: "/bin/zsh -c node-test-suite" }); // hidden env, started after the anchor, no lineage
  const probeFor = (procs, over = {}) => fakeProbe("darwin", {
    bootId: () => "boot-1", tokenHolders: () => [], snapshotUsers: () => [],
    hiddenHolders: (ctx) => classifyHiddenProcesses({ procs, own: new Set([process.pid]), uid: 501, ...ctx, alive: () => true, blame: () => UNMANAGED, reread: rereadOf(procs) }), ...over,
  });
  const rec = () => deadOwner(file, "darwin", { snapshot, acquiredAt: iso(acquiredAt), ancestors: [] });
  const judge = (record, procs, over) => judgeOwner(JSON.stringify(record), { file, probe: probeFor(procs, over), lease: noLease });
  // POSITIVE: reclaimed, with the process named in the output.
  const out = judge(rec(), table(unrelated));
  assert.match(out.proofs.join("; "), /reported, not held:.*900: pid 900\b/s);
  assert.ok(!out.proofs.some((x) => /may still be this ownership/.test(x)));
  // NEGATIVE: a tracked detached child group still holds, even with the unrelated process present.
  const r1 = rec(); const reg = childrenDir(file, r1.handle); fs.mkdirSync(reg, { recursive: true });
  fs.writeFileSync(path.join(reg, "group-800.json"), JSON.stringify({ group: 800, startedMs: NOW - 90_000, command: "cargo" }));
  const live = [{ pid: 801, startedMs: NOW - 30_000 }];
  assert.throws(() => judge(r1, table(unrelated), { groupSnapshot: (g) => ({ members: g === 800 ? live : [] }) }), (e) => e instanceof OwnershipBusy && /detached process group 800/.test(e.message));
  fs.writeFileSync(path.join(reg, "intent-1"), "{}");
  fs.rmSync(path.join(reg, "group-800.json"));
  assert.throws(() => judge(r1, table(unrelated)), (e) => e instanceof OwnershipBusy && /detached spawn was in flight/.test(e.message));
  fs.rmSync(path.join(reg, "intent-1"));
  // The exact ownership token and an actual snapshot user still hold.
  assert.throws(() => judge(rec(), table(unrelated), { tokenHolders: () => [777] }), (e) => e instanceof OwnershipBusy && /777.*still carry/.test(e.message));
  assert.throws(() => judge(rec(), table(unrelated), { snapshotUsers: () => [{ pid: 778, how: "cwd" }] }), (e) => e instanceof OwnershipBusy && /778 \(cwd\).*still use the snapshot/.test(e.message));
  // A broker lease that is not REAPED still holds.
  const submitted = { ...rec(), broker: { submitted: true } };
  assert.throws(() => judgeOwner(JSON.stringify(submitted), { file, probe: probeFor(table(unrelated)), lease: () => ({ state: "ACTIVE", why: "x" }) }), (e) => e instanceof OwnershipBusy);
});

// ---- native macOS: the real Apple platform binary under ownership ----

/** The facts the reproduction rests on, read from the real `ps`: the parent, and whether `ps eww` shows this process's environment. */
function macFacts(pid) {
  const ppid = Number(spawnSync("ps", ["-o", "ppid=", "-p", String(pid)], { encoding: "utf8" }).stdout.trim());
  const command = spawnSync("ps", ["-ww", "-o", "command=", "-p", String(pid)], { encoding: "utf8" }).stdout.trim();
  const shown = spawnSync("ps", ["eww", "-o", "command=", "-p", String(pid)], { encoding: "utf8" }).stdout.trim();
  return { ppid, command, envShown: shown.length > command.length && shown.startsWith(`${command} `) };
}
/** `launchctl blame` as the gate asks it, for a pid of the test's own making. */
const blameOfPid = (pid) => { const r = spawnSync("launchctl", ["blame", String(pid)], { encoding: "utf8", env: { ...process.env, LC_ALL: "C" }, timeout: 5000 }); return { rc: r.status, line: `${r.stdout}${r.stderr}`.split("\n").find((l) => l.trim() !== "")?.trim() ?? "" }; };

test("row 5.24a' [native macOS, G2 reproduction]: a gate descendant that is an Apple platform binary, started its own session, touches nothing in the snapshot and was orphaned to launchd is reported, never held: ownership releases while it lives (rightkit#1313)", { skip: process.platform !== "darwin" && "macOS: `ps eww` hides Apple platform binaries only there", timeout: 180_000 }, async (t) => {
  const base = tmp(t, "rk-own-hidden-"); const file = path.join(base, OWNERSHIP_DIRNAME, "owner.json"); const snapshot = path.join(base, "snapshots", "k");
  fs.mkdirSync(path.join(base, "snapshots"), { mode: 0o700 }); fs.mkdirSync(snapshot, { recursive: true }); fs.mkdirSync(path.dirname(file), { mode: 0o700 });
  const g = await crashingGate(t, file, { mode: "hidden-orphan", snapshot });
  const facts = macFacts(g.child);
  t.diagnostic(`the descendant: pid ${g.child} ${JSON.stringify(facts)}`);
  assert.equal(facts.command, "/bin/sleep 120", "an Apple platform binary");
  assert.equal(facts.ppid, 1, "orphaned to launchd: macOS has no subreaper");
  assert.equal(facts.envShown, false, "`ps eww` shows no environment for it, so the token net cannot see the handle it carries");
  assert.equal(blameKind(blameOfPid(g.child)), "unmanaged", "launchctl blame: a reparented orphan is not managed by launchd");
  const lsofPid = spawnSync("lsof", ["-n", "-P", "-p", String(g.child), "-Fn"], { encoding: "utf8" }).stdout;
  assert.equal(lsofPid.split("\n").some((l) => l.startsWith("n") && l.includes(snapshot)), false, "and it touches nothing in the snapshot at sampling time");
  await g.kill(g.gate);
  // rightkit#1313: hidden environment plus start time is no attribution. The unattributed orphan stays alive, is reported, and never holds the host.
  const next = gateStep(t, "acquire", { file }, process.env);
  assert.equal(next.ok, true, `an unattributed hidden orphan does not hold the host: ${JSON.stringify(next)}`); assert.equal(next.reclaimed.from, g.handle);
  assert.match(next.reclaimed.proofs.join("; "), new RegExp(`reported, not held:.*${g.child}`), "it is reported in the output");
  assert.doesNotThrow(() => process.kill(g.child, 0), "and it is left alive");
  await g.kill(g.child);
});

// The lead's probe (signed/item5-sage-decisions-2.md "Lead probe 2026-10-29T07:40:35Z"): launchctl blame says "not managed" for each of
// these orphan shapes, the one that read launchd as the kernel birth parent (RR's M4) included. Since rightkit#1313 none holds: each is reported.
for (const shape of ["orphan-setsid", "orphan-subshell", "orphan-nohup"]) {
  test(`row 5.24a' [native macOS, lead probe shape ${shape}]: a gate descendant orphaned to launchd this way is not managed by launchd; it is reported and left alive, and never holds the host (rightkit#1313)`, { skip: process.platform !== "darwin" && "macOS: `ps eww` hides Apple platform binaries only there", timeout: 180_000 }, async (t) => {
    const base = tmp(t, "rk-own-shape-"); const file = path.join(base, OWNERSHIP_DIRNAME, "owner.json"); const snapshot = path.join(base, "snapshots", "k");
    fs.mkdirSync(path.join(base, "snapshots"), { mode: 0o700 }); fs.mkdirSync(snapshot, { recursive: true }); fs.mkdirSync(path.dirname(file), { mode: 0o700 });
    const g = await crashingGate(t, file, { mode: shape, snapshot });
    const facts = macFacts(g.child);
    t.diagnostic(`${shape}: pid ${g.child} ${JSON.stringify(facts)}`);
    assert.equal(facts.command, "/bin/sleep 120", "an Apple platform binary");
    assert.equal(facts.ppid, 1, "orphaned to launchd");
    assert.equal(facts.envShown, false, "`ps eww` shows no environment for it");
    assert.equal(blameKind(blameOfPid(g.child)), "unmanaged", "launchctl blame: not managed by launchd");
    await g.kill(g.gate);
    const next = gateStep(t, "acquire", { file }, process.env);
    assert.equal(next.ok, true, `an unattributed hidden orphan does not hold the host: ${JSON.stringify(next)}`); assert.equal(next.reclaimed.from, g.handle);
    assert.match(next.reclaimed.proofs.join("; "), new RegExp(`reported, not held:.*${g.child}`), "reported in the output");
    assert.doesNotThrow(() => process.kill(g.child, 0), "and left alive");
    await g.kill(g.child);
  });
}

test("row 5.24a' [native macOS, liveness]: judges the rule, not the host: every pid held is re-blamed by the test and is not managed; none is in the test's own tree; the shell opened after the anchor and the platform binary it runs are not held", { skip: process.platform !== "darwin" && "macOS only", timeout: 180_000 }, async (t) => {
  const base = tmp(t, "rk-own-shell-"); const file = path.join(base, OWNERSHIP_DIRNAME, "owner.json"); const snapshot = path.join(base, "snapshots", "k");
  fs.mkdirSync(path.dirname(file), { mode: 0o700 });
  // A terminal stand-in that predates the anchor (the root's birth is recorded when the gate fixture starts).
  const helperCode = 'const { spawn } = require("node:child" + "_process"); process.stdin.on("data", () => { const sh = spawn("/bin/zsh", ["-c", "/bin/sleep 120; true"], { stdio: "ignore" }); process.stdout.write(sh.pid + "\\n"); }); setInterval(() => {}, 1000);';
  const helper = spawn(process.execPath, ["-e", helperCode], { stdio: ["pipe", "pipe", "inherit"], env: { ...process.env, NODE_OPTIONS: "" } });
  t.after(() => { try { helper.kill("SIGKILL"); } catch { /* gone */ } });
  await new Promise((r) => setTimeout(r, 1500)); // the helper is a whole second older than the anchor (ps lstart has one-second resolution)
  const g = await crashingGate(t, file, { snapshot });          // the gate takes ownership; the root is born now: the anchor is later than the helper
  helper.stdin.write("go\n");
  const shell = Number(await new Promise((res) => helper.stdout.once("data", (d) => res(String(d).trim()))));
  // Its children first: once the shell is gone they are orphaned to launchd, where they would be a (real) false hold for every later test.
  t.after(() => { for (const p of [...pidsOfParent(shell), shell]) { try { process.kill(p, "SIGKILL"); } catch { /* gone */ } } });
  await new Promise((r) => setTimeout(r, 600));
  const record = JSON.parse(fs.readFileSync(file, "utf8"));
  const anchor = Math.min(readRootBirth(path.dirname(snapshot), hostBootId()), Date.parse(record.acquiredAt));
  const held = processProbe({ platform: "darwin" }).hiddenHolders({ owner: record, anchorMs: anchor, recorded: [] });
  const tree = new Set([shell, helper.pid, ...pidsOfParent(shell), g.gate, g.child]);
  t.diagnostic(`shell ${shell} (opened after the anchor ${new Date(anchor).toISOString()}); held by the real classification: ${JSON.stringify(held.map((h) => h.pid))}`);
  for (const h of held) {
    assert.equal(tree.has(h.pid), false, `a held process is in the test's own tree: ${h.why}`);
    assert.doesNotMatch(h.why, new RegExp(`pid (${[...tree].join("|")})\\b.*(parent|top)`), "no held reason names the test's own tree");
    assert.notEqual(blameKind(blameOfPid(h.pid)), "managed", `a launchd-managed process was held: ${h.why}`);
  }
  assert.equal(held.some((h) => h.pid === shell), false, "the shell opened after the anchor is not held");
  assert.equal(held.some((h) => h.pid === helper.pid), false, "nor is the terminal stand-in that predates it");
  for (const kid of pidsOfParent(shell)) assert.equal(held.some((h) => h.pid === kid), false, `the command it runs is not held (pid ${kid})`);
  assert.equal(JSON.parse(fs.readFileSync(file, "utf8")).handle, g.handle, "the gate still holds");
});
const pidsOfParent = (ppid) => spawnSync("ps", ["-A", "-o", "pid=,ppid="], { encoding: "utf8" }).stdout.split("\n").map((l) => l.trim().split(/\s+/).map(Number)).filter(([, pp]) => pp === ppid).map(([p]) => p);

test("row 5.24a' D2 [native macOS]: `zsh -c '<gate> | /usr/bin/tee log'`: the gate acquires, then releases while tee is alive, and the release is clean", { skip: process.platform !== "darwin" && "macOS only", timeout: 180_000 }, async (t) => {
  const base = tmp(t, "rk-own-tee-"); const file = path.join(base, OWNERSHIP_DIRNAME, "owner.json"); const root = path.join(base, "snapshots"); const result = path.join(base, "result.json");
  fs.mkdirSync(path.dirname(file), { mode: 0o700 }); fs.mkdirSync(root, { mode: 0o700 }); recordRootBirth(root, { bootId: hostBootId() });
  await new Promise((r) => setTimeout(r, 1200));   // the anchor second must pass: zsh and tee start in a later one
  const driver = path.join(base, "gate-tee.mjs");
  fs.writeFileSync(driver, `
const { acquireBuildOwnership } = await import(${JSON.stringify(MODULE)});
const fs = await import("node:fs");
const own = acquireBuildOwnership({ file: ${JSON.stringify(file)}, stage: "tee", snapshot: ${JSON.stringify(path.join(root, OLD_KEY))}, env: process.env });
// tee is alive here: it holds this process's stdout open until it exits
const alive = (await import("node:child" + "_process")).spawnSync("pgrep", ["-x", "tee"], { encoding: "utf8" }).stdout.trim().split("\\n").filter(Boolean);
fs.writeFileSync(${JSON.stringify(result)}, JSON.stringify({ release: own.release(), teeAlive: alive.length > 0 }));
`);
  const env = { ...process.env, NODE_OPTIONS: "" }; delete env[OWNERSHIP_ENV]; delete env.RIGHTKIT_BUILD_OWNERSHIP_FILE;
  const zsh = spawn("/bin/zsh", ["-c", `${JSON.stringify(process.execPath)} ${JSON.stringify(driver)} | /usr/bin/tee ${JSON.stringify(path.join(base, "log"))}`], { env, stdio: "ignore" });
  t.after(() => { try { zsh.kill("SIGKILL"); } catch { /* gone */ } });
  await until(() => fs.existsSync(result), 60_000);
  const r = JSON.parse(fs.readFileSync(result, "utf8"));
  assert.equal(r.teeAlive, true, "tee was alive at the release");
  assert.equal(r.release.released, true, `the release is clean with tee alive: ${JSON.stringify(r.release)}`);
});


// ---------------------------------------------------------------------------------------------------------------------
// Decision D4 (rows 5.15, 5.17): build ownership never creates the managed root. Injected temporary roots only (the real managed root
// is protected by the 5.25 guard).
// ---------------------------------------------------------------------------------------------------------------------

const isRefusal = (reason) => (e) => e?.code === "refused" && e.reason === reason && /managed root/.test(e.message);
const splitDevice = (p) => (p === "/" ? 1n : 2n);   // the managed root on another device than /
const bootDevice = () => 1n;                         // the managed root on the boot volume
/** Takes ownership at the host's own location inside the injected managed root `managedRoot`. */
const acquireIn = (managedRoot, platform, device, extra = {}) => acquireBuildOwnership({ file: path.join(managedRoot, OWNERSHIP_DIRNAME, "owner.json"), platform, probe: fakeProbe(platform), lease: noLease, env: {}, exportEnv: false, stage: "d4", managedRoot, device, ...extra });

test("D4 rule 1: the guard and its error class live in managed-root-guard.mjs, which build-ownership.mjs imports; snapshot.mjs re-exports the same two, and build-ownership.mjs never imports snapshot.mjs", async () => {
  const guard = await import("./managed-root-guard.mjs");
  const snapshot = await import("./snapshot.mjs");
  assert.equal(snapshot.requireManagedRoot, guard.requireManagedRoot, "one guard, not a copy");
  assert.equal(snapshot.SnapshotError, guard.SnapshotError, "one error class: a refusal from either path is instanceof the same SnapshotError");
  const imports = [...fs.readFileSync(new URL("./build-ownership.mjs", import.meta.url), "utf8").matchAll(/^import\b[^;]*?from\s+"([^"]+)"/gm)].map((m) => m[1]);
  assert.ok(imports.includes("./managed-root-guard.mjs"), imports.join(", "));
  assert.equal(imports.includes("./snapshot.mjs"), false, "no import cycle with snapshot.mjs");
  const absent = path.join(os.tmpdir(), `rk-d4-absent-${process.pid}-${Date.now()}`);
  assert.throws(() => guard.requireManagedRoot(absent, { platform: "linux" }), (e) => e instanceof snapshot.SnapshotError && e.code === "refused" && e.reason === "managed-root-missing");
  assert.equal(fs.existsSync(absent), false, "the guard creates nothing");
});

test("D4: the managed-root guard is FIRST in acquireBuildOwnership: an absent root, a link, or the boot volume is refused by name, and nothing is created", (t) => {
  const base = tmp(t, "rk-own-d4-");
  for (const platform of ["darwin", "win32"]) {
    const absent = path.join(base, `absent-${platform}`);
    assert.throws(() => acquireIn(absent, platform, splitDevice), isRefusal("managed-root-missing"), platform);
    assert.equal(fs.existsSync(absent), false, `${platform}: the managed root was not created`);
    // The guard is before the environment and the record: a nested gate's handle makes no difference.
    assert.throws(() => acquireIn(absent, platform, splitDevice, { env: { [OWNERSHIP_ENV]: "x" } }), isRefusal("managed-root-missing"), `${platform}: before the environment is read`);
    assert.deepEqual(fs.readdirSync(base).filter((n) => n.startsWith("absent")), [], `${platform}: nothing anywhere`);
  }
  // The managed root as a link: refused, and its target is untouched.
  const target = path.join(base, "target"); fs.mkdirSync(target, { mode: 0o700 }); const link = path.join(base, "link");
  let linked = true; try { fs.symlinkSync(target, link, "dir"); } catch (e) { linked = false; t.diagnostic(`no link here (${e.code}): the link case is not run`); }
  if (linked) { assert.throws(() => acquireIn(link, "darwin", splitDevice), isRefusal("managed-root-missing")); assert.deepEqual(fs.readdirSync(target), [], "the target of the link is untouched"); }
  // The Mac managed root on the boot device: refused (an unmounted volume's boot-volume substitute).
  const onBoot = path.join(base, "onboot"); fs.mkdirSync(onBoot, { mode: 0o700 });
  assert.throws(() => acquireIn(onBoot, "darwin", bootDevice), isRefusal("managed-root-on-boot-volume"));
  assert.deepEqual(fs.readdirSync(onBoot), [], "nothing was created on the boot-volume root");
  // A device that cannot be read is an error, not a pass.
  assert.throws(() => acquireIn(onBoot, "darwin", () => { throw new Error("EIO"); }), (e) => e.code === "error");
  // Linux has no managed root: the same absent path acquires (the durable state directory is created whole).
  const linux = acquireBuildOwnership({ file: path.join(base, "state", "a", OWNERSHIP_DIRNAME, "owner.json"), platform: "linux", probe: fakeProbe("linux"), lease: noLease, env: {}, exportEnv: false, managedRoot: path.join(base, "absent-linux"), device: splitDevice });
  assert.equal(linux.release().released, true);
  assert.equal(fs.existsSync(path.join(base, "absent-linux")), false);
});

test("D4: an existing managed root gets exactly `build-ownership/` (mode 0700) and nothing else; the directory is one level, and the children directory too", { skip: process.platform === "win32" && "POSIX mode bits" }, (t) => {
  const base = tmp(t, "rk-own-d4-ok-"); const managed = path.join(base, "managed"); fs.mkdirSync(managed, { mode: 0o700 });
  const own = acquireIn(managed, "darwin", splitDevice);
  assert.deepEqual(fs.readdirSync(managed), [OWNERSHIP_DIRNAME], "the managed root holds only build-ownership/");
  assert.equal(fs.statSync(path.join(managed, OWNERSHIP_DIRNAME)).mode & 0o777, 0o700);
  assert.deepEqual(fs.readdirSync(path.join(managed, OWNERSHIP_DIRNAME)), ["owner.json"]);
  assert.equal(own.release().released, true);
  assert.deepEqual(fs.readdirSync(managed), [OWNERSHIP_DIRNAME]);
  // One level only on Mac & Dell: a missing parent is an error and creates nothing; on Linux the state directory is created whole.
  const deep = path.join(base, "no", "such", OWNERSHIP_DIRNAME);
  assert.throws(() => ensureOwnershipDir(deep, { platform: "darwin" }), (e) => e.code === "ENOENT");
  assert.equal(fs.existsSync(path.join(base, "no")), false, "no parent was created");
  ensureOwnershipDir(deep, { platform: "linux" }); assert.equal(fs.existsSync(deep), true);
  ensureOwnershipDir(path.join(managed, OWNERSHIP_DIRNAME), { platform: "darwin" });   // an existing directory is accepted (EEXIST)
  // The detached-children directory is one level inside an existing ownership directory, never a recursive creation.
  const file = path.join(managed, OWNERSHIP_DIRNAME, "owner.json");
  spawnUnderOwnership(() => ({ pid: 4321 }), "x", [], { detached: true }, { env: { [OWNERSHIP_ENV]: "h-d4", RIGHTKIT_BUILD_OWNERSHIP_FILE: file }, platform: "darwin" });
  assert.equal(fs.existsSync(childrenDir(file, "h-d4")), true);
  const gone = path.join(base, "gone", OWNERSHIP_DIRNAME, "owner.json");
  assert.throws(() => spawnUnderOwnership(() => ({ pid: 1 }), "x", [], { detached: true }, { env: { [OWNERSHIP_ENV]: "h-d4", RIGHTKIT_BUILD_OWNERSHIP_FILE: gone }, platform: "darwin" }), (e) => e.code === "ENOENT");
  assert.equal(fs.existsSync(path.join(base, "gone")), false);
});

test("D4: each entry point maps the refusal by name and creates nothing: the door (BLOCKED, exit 1), check --snapshot (RK_CHECK_ERROR, exit 2), the snapshot CLI (exit 1), the clippy route (BLOCKED)", async (t) => {
  const { main } = await import("./bin/rightkit-gate.mjs");
  const dir = tmp(t, "rk-own-d4-entry-"); const absent = path.join(dir, "managed");
  // The real guard, reached through the callers' own `acquireOwnership` seam with an injected absent managed root.
  const guarded = (q) => acquireBuildOwnership({ ...q, platform: "darwin", managedRoot: absent, device: splitDevice, probe: fakeProbe("darwin"), lease: noLease, exportEnv: false, env: {} });
  const untouched = (what) => { assert.equal(fs.existsSync(absent), false, `${what}: the managed root was not created`); assert.equal(fs.existsSync(path.join(dir, OWNERSHIP_DIRNAME)), false, `${what}: no build-ownership directory`); };
  const snapshotRoot = path.join(dir, "snapshots");
  // check --snapshot
  let out = [];
  let code = await main(["check", "--commit", "HEAD", "--full-a", "--snapshot", path.join(dir, "snap")], { cwd: dir, env: process.env, snapshotRoot, out: (m) => out.push(m), err: (m) => out.push(m), appCheck: async () => { out.push("appCheck ran"); return 0; }, acquireOwnership: guarded });
  assert.equal(code, 2, out.join("\n")); assert.match(out.join("\n"), /RK_CHECK_ERROR: .*managed root .* does not exist.*nothing ran, nothing created/); assert.doesNotMatch(out.join("\n"), /appCheck ran/); untouched("check --snapshot");
  // the door
  const repo = tinyRepo(t); const sha = String(spawnSync("git", ["rev-parse", "HEAD"], { cwd: repo, encoding: "utf8" }).stdout).trim();
  fs.writeFileSync(path.join(repo, "package.json"), '{"name":"x","private":true}\n'); spawnSync("git", ["add", "-A"], { cwd: repo }); spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "p"], { cwd: repo });
  const sha2 = String(spawnSync("git", ["rev-parse", "HEAD"], { cwd: repo, encoding: "utf8" }).stdout).trim(); assert.notEqual(sha, sha2);
  const env = { ...process.env }; delete env.GITHUB_ACTIONS; delete env.RIGHTKIT_BUILD_OWNERSHIP; delete env.RIGHTKIT_BUILD_OWNERSHIP_FILE;
  out = [];
  code = await main(["--to", "compile", "--commit", sha2], { cwd: repo, platform: "linux", snapshotRoot, env, spawn: (c, a, opts) => (c === "git" ? spawnSync(c, a, { ...opts, encoding: "utf8" }) : { status: 1, stdout: "", stderr: "not in this fixture" }), out: (m) => out.push(m), err: (m) => out.push(m), runCheck: async () => { out.push("check ran"); return 0; }, runCompile: async () => { out.push("compile ran"); return 0; }, acquireOwnership: guarded });
  assert.equal(code, 1, out.join("\n")); assert.match(out.join("\n"), /BLOCKED: .*managed root .* does not exist.*nothing ran, nothing created/); assert.doesNotMatch(out.join("\n"), /(check|compile) ran/); untouched("the door");
  assert.equal(fs.existsSync(snapshotRoot), false, "the door synced nothing");
  // the snapshot CLI: its own guard (the snapshot lane's) and ownership's agree, exit 1, nothing created
  const { snapshotCommand } = await import("./snapshot.mjs");
  out = [];
  code = snapshotCommand({ cwd: repo, env, platform: "darwin", snapshotRoot, managedRoot: absent, device: splitDevice, out: (m) => out.push(m), err: (m) => out.push(m), acquireOwnership: guarded }, ["--commit", sha2]);
  assert.equal(code, 1, out.join("\n")); assert.match(out.join("\n"), /managed root .* does not exist/); untouched("the snapshot CLI");
  assert.equal(fs.existsSync(snapshotRoot), false);
  // ownership's own refusal reaches the CLI's catch even when the snapshot guard is not the one that fires
  out = [];
  code = snapshotCommand({ cwd: repo, env, platform: "darwin", snapshotRoot, managedRoot: dir, device: splitDevice, out: (m) => out.push(m), err: (m) => out.push(m), acquireOwnership: guarded }, ["--commit", sha2]);
  assert.equal(code, 1, out.join("\n")); assert.match(out.join("\n"), /refused.*managed root .* does not exist/); untouched("the snapshot CLI (ownership's guard)");
});

// ---- Windows process identity (found by row 5.20's native on the Dell, 2026-10-29): another process's start time, in any time zone ----

test("Windows identity/bootTime: the start and boot times are Unix milliseconds from DateTimeOffset, never a subtraction of a LOCAL 1970-01-01 (which is off by the zone's offset)", () => {
  const scripts = [];
  const spawn = (cmd, args) => {
    const script = Buffer.from(args[args.indexOf("-EncodedCommand") + 1], "base64").toString("utf16le"); scripts.push(script);
    return { status: 0, stdout: /LastBootUpTime/.test(script) ? "BOOT:1793040152500\r\n" : "START:1793253444190\r\n", stderr: "" };
  };
  const probe = processProbe({ platform: "win32", spawn, kill: () => true });
  assert.deepEqual(probe.identity(4242), { alive: true, startedMs: 1793253444190, group: null });
  assert.equal(probe.bootTime(), 1793040152500);
  assert.equal(scripts.length, 2);
  for (const s of scripts) { assert.match(s, /\[DateTimeOffset\]/); assert.match(s, /ToUnixTimeMilliseconds\(\)/); assert.doesNotMatch(s, /1970-01-01/, "the epoch is not parsed as a local time"); }
});

test("[native Windows] a LIVE gate started by another process reads as the same process: its identity agrees with its own time origin, and the boot time agrees with the uptime", { skip: process.platform !== "win32" && "Windows PowerShell Get-Process", timeout: 120_000 }, async (t) => {
  const dir = tmp(t, "rk-own-ident-"); const info = path.join(dir, "info.json"); const script = path.join(dir, "live.mjs");
  fs.writeFileSync(script, `import fs from "node:fs"; fs.writeFileSync(${JSON.stringify(info)}, JSON.stringify({ pid: process.pid, origin: Math.round(performance.timeOrigin) })); setInterval(() => {}, 1000);`);
  // Started the way a shell starts a gate (cmd start), as the door's nested gates and a second request see it.
  const live = spawn("cmd.exe", ["/d", "/c", "start", "\"\"", "/b", "/wait", process.execPath, script], { stdio: "ignore", env: { ...process.env, NODE_OPTIONS: "" } });
  t.after(() => { try { live.kill(); } catch { /* gone */ } });
  await until(() => fs.existsSync(info), 60_000);
  const g = JSON.parse(fs.readFileSync(info, "utf8"));
  t.after(() => { try { process.kill(g.pid, "SIGKILL"); } catch { /* gone */ } });
  const probe = processProbe({ platform: "win32" });
  const id = probe.identity(g.pid);
  assert.equal(id.alive, true);
  assert.ok(Math.abs(id.startedMs - g.origin) <= 3000, `identity ${id.startedMs} vs the process's own time origin ${g.origin}: ${id.startedMs - g.origin} ms apart (a local-epoch bug is the zone offset, ${-new Date().getTimezoneOffset()} min)`);
  const boot = probe.bootTime();
  const expected = Date.now() - os.uptime() * 1000;
  assert.ok(Math.abs(boot - expected) <= 300_000, `boot time ${new Date(boot).toISOString()} vs now minus uptime ${new Date(expected).toISOString()}`);
});
