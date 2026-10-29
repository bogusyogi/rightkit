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
  OWNERSHIP_ENV, OWNERSHIP_SCHEMA, HOLDER_SCHEMA, OwnershipBusy, acquireBuildOwnership, brokerLeaseState, childrenDir, holderFile, jobName, judgeOwner, ownershipFile, processProbe, readHolder, spawnUnderOwnership,
} from "./build-ownership.mjs";
import { keyPath, repoKey, snapshotCommand, verifySnapshot } from "./snapshot.mjs";

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
    cwdHolders: () => null,
    jobState: () => ({ open: true, active: 1 }),
    contain: () => ({ state: "holding" }),
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
    assert.deepEqual(rec.containment, platform === "win32" ? { kind: "job", name: jobName(own.handle) } : { kind: "group", group: 4242 }, platform);
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

test("ownership POSIX: every process group a gate started detached must be empty; a half-recorded spawn keeps the host; the environment and working-directory nets each hold it", (t) => {
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
  assert.throws(() => acquire(file, "darwin", probe({ groupSnapshot: (g) => (g === 8100 ? reused : group([])), cwdHolders: () => [9200] })), /9200 still run inside the snapshot \/snap\/x/);
  assert.throws(() => acquire(file, "darwin", probe({ groupSnapshot: (g) => (g === 8100 ? reused : group([])), cwdHolders: () => { throw new Error("lsof"); } })), /working directories cannot be listed \(lsof\)/);
  const own = acquire(file, "darwin", probe({ groupSnapshot: (g) => (g === 8100 ? reused : group([])), tokenHolders: () => [], cwdHolders: () => [] }));
  assert.match(own.reclaimed.proofs.join("; "), /the 1 detached process group\(s\) it started are empty; no process carries .*; no process runs inside \/snap\/x/);
  assert.equal(fs.existsSync(reg), false, "the reclaimed owner's detached-child record goes with it");
});

test("spawnUnderOwnership: only a detached POSIX spawn under a held ownership is recorded, intent first, then its group", (t) => {
  const dir = tmp(t, "rk-own-");
  const env = { [OWNERSHIP_ENV]: "h-9", RIGHTKIT_DATA_ROOT: dir, RIGHTKIT_SNAPSHOT_ROOT: path.join(dir, "snapshots") };
  const reg = childrenDir(ownershipFile({ platform: "darwin", env }), "h-9");
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
  const file = ownershipFile({ platform: "darwin", env });
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

test("ownership: a normal release with a survivor leaves the record; a gate that cannot be contained never holds", (t) => {
  const dir = tmp(t, "rk-own-");
  const file = path.join(dir, "owner.json");
  const own = acquire(file, "win32", fakeProbe("win32", { jobState: () => ({ open: true, active: 3 }) }));
  assert.match(own.release().why, /still holds 3 process\(es\) \(this gate is one\)/); assert.equal(fs.existsSync(file), true);
  fs.rmSync(file);
  // At release the gate is alive: its own child (ppid = this process) is a survivor; a peer of the shell is not.
  const posix = acquire(file, "linux", fakeProbe("linux", { groupSnapshot: () => group([{ pid: 9, ppid: process.pid, startedMs: Date.now() }, { pid: 10, ppid: 5000, startedMs: Date.now() }, { pid: 5000, ppid: 1, startedMs: NOW - 9e6 }], []) }));
  assert.match(posix.release().why, /1 member\(s\) started under this ownership: 9$/); assert.equal(fs.existsSync(file), true);
  fs.rmSync(file);
  assert.throws(() => acquire(file, "win32", fakeProbe("win32", { contain: () => { throw new Error("the job holder could not contain the gate: AssignProcessToJobObject failed (5)"); } })), /could not contain the gate/);
  assert.equal(fs.existsSync(file), false, "the record of a gate that was never contained is removed");
});

test("probes: a listing that did not finish cleanly is unknown, never a complete answer", { skip: fs.existsSync("/proc/self") && "this host reads /proc, not ps and lsof" }, () => {
  const probe = (status, stdout) => processProbe({ platform: "darwin", spawn: (cmd) => (cmd === "ps" ? { status: 0, stdout: `${process.pid} 1 ${process.pid} Thu Oct 29 00:00:00 2026\n`, pid: 1 } : { status, stdout, stderr: "lsof: WARNING: can't stat()" }), uid: 501 });
  assert.throws(() => probe(1, "p123\nfcwd\nn/snap/k\n").cwdHolders("/snap/k"), /lsof: lsof: WARNING/);
  assert.deepEqual(probe(0, "p123\nfcwd\nn/snap/k/src\np124\nfcwd\nn/elsewhere\n").cwdHolders("/snap/k"), [123]);
  const env = (status) => processProbe({ platform: "darwin", spawn: (cmd, args) => (args[0] === "eww" ? { status, stdout: `77 node RIGHTKIT_BUILD_OWNERSHIP=h1 PATH=/x\n` } : { status: 0, stdout: "", pid: 1 }) });
  assert.throws(() => env(1).tokenHolders("h1"), /ps eww/);
  assert.deepEqual(env(0).tokenHolders("h1"), [77]);
  assert.deepEqual(env(0).tokenHolders("h"), [], "a prefix of the handle is not the handle");
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
/** Starts a fake gate that takes ownership of `file` and starts one child that outlives it; resolves with their pids. */
async function crashingGate(t, file, { detachChild = process.platform === "win32", mode = "plain", snapshot = null } = {}) {
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
} else if (${JSON.stringify(mode)} === "platform-binary") {
  const c = spawn("/bin/sleep", ["60"], { stdio: "ignore", detached: true, env: {}, cwd: ${JSON.stringify(snapshot)} }); c.unref(); pid = c.pid;
} else {
  const c = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { stdio: "ignore", detached: ${detachChild} }); c.unref(); pid = c.pid;
}
process.stdout.write(JSON.stringify({ handle: own.handle, gate: process.pid, child: pid }) + "\\n");
setInterval(() => {}, 1000);
`);
  const env = { ...process.env }; delete env[OWNERSHIP_ENV]; delete env.NODE_OPTIONS;
  // POSIX: the fake gate leads its own process group, as a gate started from a shell leads its job's.
  const gate = spawn(process.execPath, [script], { stdio: ["ignore", "pipe", "inherit"], env, detached: process.platform !== "win32" });
  const line = await new Promise((resolve, reject) => {
    let buf = ""; gate.stdout.on("data", (d) => { buf += d; if (buf.includes("\n")) resolve(buf.split("\n")[0]); });
    gate.on("exit", (code) => reject(new Error(`fake gate exited ${code} before reporting`)));
  });
  const info = JSON.parse(line);
  t.after(() => { for (const pid of [info.gate, info.child]) { try { process.kill(pid, "SIGKILL"); } catch { /* gone */ } } });
  return { ...info, kill: async (pid) => { try { process.kill(pid, "SIGKILL"); } catch { /* gone */ } for (let i = 0; i < 200; i++) { try { process.kill(pid, 0); } catch { return; } await new Promise((r) => setTimeout(r, 50)); } throw new Error(`pid ${pid} survived SIGKILL`); } };
}
async function until(fn, ms = 20_000) { const end = Date.now() + ms; for (;;) { if (fn()) return; if (Date.now() > end) throw new Error("timed out"); await new Promise((r) => setTimeout(r, 100)); } }

test("[native] G1: a gate killed while its child still runs keeps the host owned until the child is gone, then the next gate reclaims it with the proof", { timeout: 120_000 }, async (t) => {
  const file = path.join(tmp(t, "rk-own-native-"), "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const g = await crashingGate(t, file);
  await g.kill(g.gate);
  const env = { ...process.env }; delete env[OWNERSHIP_ENV];
  const next = () => acquireBuildOwnership({ file, stage: "next", env, exportEnv: false });
  assert.throws(next, (e) => e instanceof OwnershipBusy && new RegExp(process.platform === "win32" ? "still holds 1 process" : `started under this ownership: ${g.child}`).test(e.message), "a surviving child keeps ownership");
  await g.kill(g.child);
  if (process.platform === "win32") await until(() => readHolder(holderFile(file, g.handle))?.state === "drained");
  const own = next();
  assert.equal(own.reclaimed.from, g.handle);
  assert.match(own.reclaimed.proofs.join("; "), process.platform === "win32" ? /drained|holds no process/ : /has no member started under this ownership/);
  assert.equal(own.release().released, true);
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
  assert.throws(() => acquireBuildOwnership({ file, env, exportEnv: false }), new RegExp(`${g.child} still run inside the snapshot`));
  await g.kill(g.child);
  assert.equal(acquireBuildOwnership({ file, env, exportEnv: false }).reclaimed.from, g.handle);
});

test("[native] E6/G1: a snapshot sync is refused while a crashed gate's child survives, the snapshot keeps its tree, and syncs once the child is gone", { timeout: 120_000 }, async (t) => {
  const git = (cwd, ...args) => { const r = spawnSync("git", args, { cwd, encoding: "utf8", env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t" } }); if (r.status !== 0) throw new Error(r.stderr); return r.stdout.trim(); };
  const repo = tmp(t, "rk-own-repo-"); const root = tmp(t, "rk-own-root-");
  git(repo, "init", "-q", "-b", "main"); fs.writeFileSync(path.join(repo, "a.txt"), "one\n"); git(repo, "add", "-A"); git(repo, "commit", "-q", "-m", "a");
  const first = git(repo, "rev-parse", "HEAD");
  const env = { ...process.env, RIGHTKIT_DATA_ROOT: root, RIGHTKIT_SNAPSHOT_ROOT: path.join(root, "snapshots") }; delete env[OWNERSHIP_ENV];
  const lines = []; const o = { cwd: repo, env, platform: process.platform, out: (m) => lines.push(m), err: (m) => lines.push(m) };
  assert.equal(snapshotCommand(o, ["--commit", first, "--json"]), 0, lines.join("\n"));
  const file = ownershipFile({ platform: process.platform, env });
  const g = await crashingGate(t, file);
  await g.kill(g.gate);
  fs.writeFileSync(path.join(repo, "a.txt"), "two\n"); git(repo, "commit", "-qam", "b");
  const second = git(repo, "rev-parse", "HEAD");
  lines.length = 0;
  assert.equal(snapshotCommand(o, ["--commit", second]), 1, "busy, not synced");
  assert.match(lines.join("\n"), /snapshot busy: build ownership .* is gone but/);
  const dir = path.join(path.join(root, "snapshots"), repoKey(keyPath(repo)));
  assert.equal(verifySnapshot({ checkout: repo, rev: first, root: path.join(root, "snapshots"), dir }).commit, first, "the snapshot still holds the first commit");
  await g.kill(g.child);
  if (process.platform === "win32") await until(() => readHolder(holderFile(file, g.handle))?.state === "drained");
  lines.length = 0;
  assert.equal(snapshotCommand(o, ["--commit", second, "--json"]), 0, lines.join("\n"));
  const r = JSON.parse(lines.find((l) => l.startsWith("{")));
  assert.equal(r.commit, second); assert.equal(r.ownership.reclaimed.from, g.handle);
  assert.equal(fs.existsSync(file), false, "the sync released the host");
});

test("check --snapshot: ownership comes first; busy runs nothing (RK_CHECK_ERROR, exit 2); a held ownership is released however the check ends", async (t) => {
  const { main } = await import("./bin/rightkit-gate.mjs");
  const dir = tmp(t, "rk-own-check-");
  const calls = [];
  const io = (acquireOwnership) => ({ cwd: dir, env: { ...process.env, RIGHTKIT_DATA_ROOT: dir, RIGHTKIT_SNAPSHOT_ROOT: path.join(dir, "snapshots") }, out: (m) => calls.push(`out ${m}`), err: (m) => calls.push(`err ${m}`), appCheck: async () => { calls.push("appCheck"); return 0; }, acquireOwnership });
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
