// When broker work "may exist" under a build ownership (build-ownership.mjs markBrokerSubmit). The record is marked ONLY immediately
// before a snapshot-bound (carried) request is sent, under that request's own sub-handle `<handle>#<uuid>`, and only that mark makes a
// reclaim wait for the broker's REAPED. An installed carrier map and an uncarried probe (`rustc -vV`, `--version`, doctor) never mark:
// the broker has no lease for a handle it never saw a snapshot-bound request under, so it could never answer REAPED and the host
// would stay locked until a manual clear (gate 5c388c71, stage-sidecar's `rustc -vV`).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";
import { spawn, spawnSync } from "./background-process.mjs";
import { OWNERSHIP_ENV, OWNERSHIP_FILE_ENV, OWNERSHIP_SCHEMA, OwnershipBusy, acquireBuildOwnership, markCarriedSend, markLockFile } from "./build-ownership.mjs";
import { MARK_LOCK_SCHEMA, markBrokerSubmit, withMarkLock } from "./ownership-mark.mjs";
import { CARRIED_CARGO_MAP_ENV, installCarrier } from "./carried-cargo.mjs";
import { startRealBroker } from "./test-support/real-broker.mjs";

const tmp = (t, prefix) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix))); t.after(() => fs.rmSync(d, { recursive: true, force: true, maxRetries: 20, retryDelay: 250 })); return d; };
const POSIX = process.platform === "win32" ? "the fixture client is POSIX only" : false;
const NOW = Date.now();
const GATE = "/gate/bin/rightkit-gate.mjs";

/** A probe: this test process is the live gate; every other pid is gone. */
const probe = () => ({
  platform: "darwin",
  identity: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW - 60_000, group: 4242 } : { alive: false, startedMs: null, group: null }),
  bootTime: () => NOW - 86_400_000,
  groupSnapshot: () => ({ members: [], byPid: new Map([[1, { pid: 1, ppid: 0, startedMs: NOW - 9e7 }]]), own: new Set([process.pid]) }),
  tokenHolders: () => null, snapshotUsers: () => null, ancestors: () => [{ pid: 5000, startedMs: NOW - 9e6 }],
});
const leaseOf = (state) => { const asked = []; const lease = (q) => { asked.push(q); return { state, why: `fake ${state}` }; }; lease.asked = asked; return lease; };
const HOST_HANDLE = "own-7";
const COMMIT = { sha: "c".repeat(40), tree: "e".repeat(40) };
const WORKSPACES = { "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n', "crates/a/src/lib.rs": "\n" };
const writeTree = (root, files) => { for (const [rel, body] of Object.entries(files)) { const f = path.join(root, rel); fs.mkdirSync(path.dirname(f), { recursive: true }); fs.writeFileSync(f, body); } };
/**
 * Plain data for the carrier: a checkout and a snapshot tree, the gate's ownership record and a broker section naming `client`. No broker runs here; a test that
 * needs one passes the real isolated broker's client. The pins are placeholders (never a receipt): no test below judges a receipt.
 */
function hostData(t, { snapshotsRoot, client }) {
  const root = tmp(t, "rk-own-host-");
  const checkout = path.join(root, "checkout"); const snapshot = path.join(snapshotsRoot ?? path.join(root, "snapshots"), "0123456789abcdef");
  writeTree(checkout, WORKSPACES); writeTree(snapshot, WORKSPACES);
  const ownership = path.join(root, "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(ownership), { recursive: true });
  fs.writeFileSync(ownership, JSON.stringify({ handle: HOST_HANDLE, broker: { submitted: false } }));
  const section = { client, controlRoot: path.join(root, "control"), commit: COMMIT, snapshot: { path: snapshot, expectedTree: COMMIT.tree }, handle: HOST_HANDLE, checkout, pins: { host: "host-1", keyId: "0".repeat(64), publicKey: "04".padEnd(130, "0"), brokerDigest: "b".repeat(64) }, outputRoots: [], platform: process.platform, sentinel: null };
  return { root, checkout, snapshot, ownership, ownershipEnv: { [OWNERSHIP_ENV]: HOST_HANDLE, [OWNERSHIP_FILE_ENV]: ownership }, section, owner: () => JSON.parse(fs.readFileSync(ownership, "utf8")) };
}
/** The real installed broker as an isolated instance, with the host data's snapshot under its own data root. */
async function realHost(t, name) {
  const broker = await startRealBroker({ name });
  t.after(() => broker.stop());
  return { broker, ...hostData(t, { snapshotsRoot: path.join(broker.dataRoot, "snapshots"), client: broker.clientPath }) };
}
const requestsAtBroker = (broker) => (fs.existsSync(broker.receiptsDir) ? fs.readdirSync(broker.receiptsDir).length : 0);
const noLease = () => { throw new Error("the broker lease must not be asked when no carried request was sent"); };
const acquire = (file, lease, extra = {}) => acquireBuildOwnership({ file, platform: "darwin", probe: probe(), lease, env: {}, exportEnv: false, stage: "compile", snapshot: "/snap/x", ...extra });
/** The held record, as if its gate had died (a crash): another pid, the same handle, snapshot and broker state. */
const crash = (file) => { const rec = JSON.parse(fs.readFileSync(file, "utf8")); fs.writeFileSync(file, JSON.stringify({ ...rec, pid: 999_999, startedMs: NOW - 120_000, acquiredAt: new Date(NOW - 100_000).toISOString() })); return rec; };

test("data: a stage that installs its carrier and sends only an uncarried probe, then fails, leaves a reclaimable ownership", (t) => {
  const dir = tmp(t, "rk-own-sub-");
  const file = path.join(dir, "build-ownership", "owner.json");
  const fx = hostData(t, { client: path.join(dir, "no-client") });
  const own = acquire(file, noLease);
  // Installing the carrier map for the stage's children sends nothing and records nothing.
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout }, section: { ...fx.section, handle: own.handle }, platform: "darwin", env: {} });
  t.after(() => installed.cleanup());
  assert.ok(installed.env[CARRIED_CARGO_MAP_ENV]);
  // The child's only broker call is an uncarried toolchain probe (no snapshot carrier): it is not a carried send, so the record is untouched.
  const before = JSON.parse(fs.readFileSync(file, "utf8"));
  assert.equal(before.broker.submitted, false);
  assert.equal(before.broker.handles, undefined);
  // The child fails on a config error; the gate dies with the stage. The next acquire proves process and containment gone and needs no lease.
  crash(file);
  const next = acquire(file, noLease);
  assert.equal(next.reclaimed.from, own.handle);
  assert.doesNotMatch(next.reclaimed.proofs.join("; "), /lease/);
  next.release();
});

test("a carried request marks the record before it is sent: submitted, and its sub-handle; an unknown lease refuses a reclaim, REAPED allows it", (t) => {
  const dir = tmp(t, "rk-own-sub-");
  const file = path.join(dir, "build-ownership", "owner.json");
  const own = acquire(file, noLease);
  const sub1 = `${own.handle}#11111111-aaaa`; const sub2 = `${own.handle}#22222222-bbbb`;
  own.markBrokerSubmit(sub1); own.markBrokerSubmit(sub2); own.markBrokerSubmit(sub1);
  const rec = JSON.parse(fs.readFileSync(file, "utf8"));
  assert.equal(rec.broker.submitted, true);
  assert.deepEqual(rec.broker.handles, [sub1, sub2], "each sub-handle is recorded once, in send order");
  assert.equal(fs.existsSync(markLockFile(file)), false, "the mark lock is not left behind");
  for (const state of ["UNKNOWN", "ACTIVE"]) {
    crash(file);
    const lease = leaseOf(state);
    assert.throws(() => acquire(file, lease), (e) => e instanceof OwnershipBusy && new RegExp(`broker work may exist under this handle and its lease is ${state}`).test(e.message), state);
    assert.deepEqual(lease.asked[0], { snapshot: "/snap/x", handle: own.handle }, "the lease is asked for the bare handle, which covers every sub-handle");
  }
  const reaped = leaseOf("REAPED");
  const next = acquire(file, reaped);
  assert.match(next.reclaimed.proofs.join("; "), /lease for .* is REAPED/);
  assert.equal(fs.existsSync(markLockFile(file)), false);
  next.release();
});

test("a mark names exactly a sub-handle of the holder: the bare handle, another's sub-handle, a malformed or a missing one is refused and nothing is recorded", (t) => {
  const file = path.join(tmp(t, "rk-own-sub-"), "build-ownership", "owner.json");
  const own = acquire(file, noLease);
  for (const bad of [undefined, null, "", own.handle, `${own.handle}#`, `${own.handle}#a/b`, `${own.handle}x#1`, "another-handle#1"]) {
    assert.throws(() => own.markBrokerSubmit(bad), (e) => e instanceof OwnershipBusy, String(bad));
  }
  assert.throws(() => markCarriedSend({ file, handle: "not-the-owner", callHandle: "not-the-owner#1" }), /no longer held by not-the-owner/);
  assert.equal(JSON.parse(fs.readFileSync(file, "utf8")).broker.submitted, false);
  own.release();
});

test("a child process's carried send marks the shared owner record; a child that sends nothing, or only an uncarried probe, does not (real broker)", { skip: POSIX }, async (t) => {
  const fx = await realHost(t, "own-child-send");
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout }, section: fx.section, platform: "darwin", env: {} });
  t.after(() => installed.cleanup());
  const cwd = path.join(fx.snapshot, "crates", "a");
  // The child is a separate process that holds no ownership object: it inherits the handle and the record's path (as a right-release child of the gate does).
  const child = (script, env = {}) => spawnSync(process.execPath, ["--input-type=module", "-e", script], { cwd, encoding: "utf8", env: { PATH: process.env.PATH, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: GATE, ...fx.broker.env, ...installed.env, ...env } });
  const managed = pathToFileURL(path.join(import.meta.dirname, "managed-cargo.mjs")).href;
  const carriedBuild = `import { runCargoSync } from ${JSON.stringify(managed)}; const r = runCargoSync(["build"], { cwd: ${JSON.stringify(cwd)}, env: process.env, encoding: "utf8" }, { platform: "darwin" }); process.stderr.write(r.stderr ?? ""); process.exit(r.status ?? 1);`;
  // A child that only exits (or fails on a config error before any call) sends nothing.
  assert.equal(child("process.exit(3)", fx.ownershipEnv).status, 3);
  assert.equal(fx.owner().broker.submitted, false);
  // The child's carried call: the record is marked before the send, with that request's sub-handle.
  // The real broker judges the call (the snapshot here is not one it made, so it may refuse): the mark landing before the send is the assertion, not the compile's outcome.
  const sent = child(carriedBuild, fx.ownershipEnv);
  assert.doesNotMatch(sent.stderr, /RK_CARGO_BROKER_OWNERSHIP/, sent.stderr);
  const rec = fx.owner();
  assert.equal(rec.broker.submitted, true);
  assert.equal(rec.broker.handles.length, 1);
  assert.match(rec.broker.handles[0], /^own-7#[0-9a-f-]{36}$/);
  // A second child call appends its own sub-handle; the first stays.
  const again = child(carriedBuild, fx.ownershipEnv);
  assert.doesNotMatch(again.stderr, /RK_CARGO_BROKER_OWNERSHIP/, again.stderr);
  const after = fx.owner();
  assert.equal(after.broker.handles.length, 2);
  assert.equal(after.broker.handles[0], rec.broker.handles[0]);
  assert.notEqual(after.broker.handles[1], rec.broker.handles[0]);
});

test("a carried request is not sent when its mark cannot land: no ownership in the environment, another handle's, or a record that is gone (real broker)", { skip: POSIX }, async (t) => {
  const fx = await realHost(t, "own-mark-refused");
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout }, section: fx.section, platform: "darwin", env: {} });
  t.after(() => installed.cleanup());
  const cwd = path.join(fx.snapshot, "crates", "a");
  const managed = pathToFileURL(path.join(import.meta.dirname, "managed-cargo.mjs")).href;
  const script = `import { runCargoSync } from ${JSON.stringify(managed)}; const r = runCargoSync(["build"], { cwd: ${JSON.stringify(cwd)}, env: process.env, encoding: "utf8" }, { platform: "darwin" }); process.stderr.write(r.stderr ?? ""); process.exit(r.status ?? 1);`;
  const run = (env) => spawnSync(process.execPath, ["--input-type=module", "-e", script], { cwd, encoding: "utf8", env: { PATH: process.env.PATH, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: GATE, ...fx.broker.env, ...installed.env, ...env } });
  const key = Object.keys(fx.ownershipEnv);
  for (const [name, env] of [["no ownership", {}], ["another handle", { [key[0]]: "someone-else", [key[1]]: fx.ownership }]]) {
    const r = run(env);
    assert.notEqual(r.status, 0, name);
    assert.match(r.stderr, /RK_CARGO_BROKER_OWNERSHIP/, name);
    assert.equal(fx.owner().broker.submitted, false, `${name}: nothing was recorded`);
  }
  fs.rmSync(fx.ownership);
  const gone = run(fx.ownershipEnv);
  assert.notEqual(gone.status, 0);
  assert.match(gone.stderr, /RK_CARGO_BROKER_OWNERSHIP/);
  assert.equal(requestsAtBroker(fx.broker), 0, "nothing reached the real broker (it holds no receipt or request)");
});

test("parallel carried sends from several processes all land in the record", { skip: POSIX }, async (t) => {
  const file = path.join(tmp(t, "rk-own-sub-"), "build-ownership", "owner.json");
  const own = acquire(file, noLease);
  const mod = pathToFileURL(path.join(import.meta.dirname, "build-ownership.mjs")).href;
  const n = 6;
  const procs = Array.from({ length: n }, (_, i) => new Promise((resolve) => {
    const c = spawn(process.execPath, ["--input-type=module", "-e", `import { markCarriedSend } from ${JSON.stringify(mod)}; for (let k = 0; k < 5; k++) markCarriedSend({ file: ${JSON.stringify(file)}, handle: ${JSON.stringify(own.handle)}, callHandle: ${JSON.stringify(`${own.handle}#p${i}-`)} + k });`], { stdio: "pipe" });
    let err = ""; c.stderr.on("data", (d) => { err += d; });
    c.on("close", (code) => resolve({ code, err }));
  }));
  for (const r of await Promise.all(procs)) assert.equal(r.code, 0, r.err);
  const rec = JSON.parse(fs.readFileSync(file, "utf8"));
  assert.equal(rec.broker.handles.length, n * 5, "no lost update");
  assert.equal(new Set(rec.broker.handles).size, n * 5);
  assert.equal(fs.existsSync(markLockFile(file)), false);
  assert.equal(rec.schema, OWNERSHIP_SCHEMA);
  fs.rmSync(file, { force: true });
});

// Submit lock safety (audit GB-22): the lock names its holder (pid, start, boot) and a nonce; age never proves death.
const lockFixture = (t) => { const dir = tmp(t, "rk-own-lock-"); const file = path.join(dir, "owner.json"); fs.writeFileSync(file, JSON.stringify({ handle: "h", broker: { submitted: false } })); return { dir, file, lock: markLockFile(file) }; };
const ME = { pid: process.pid, startedMs: NOW - 5000, bootId: "boot-aaaaaaaaaaaaaaaa" };
const identityOf = ({ alive = new Map(), me = ME, boot = ME.bootId } = {}) => ({ self: () => me, probe: (pid) => { const v = alive.get(pid); if (v instanceof Error) throw v; return v ?? { alive: false, startedMs: null }; }, bootId: () => boot });
const lockRecord = (over = {}) => ({ schema: MARK_LOCK_SCHEMA, pid: 4242, startedMs: NOW - 90_000, bootId: ME.bootId, host: os.hostname(), nonce: crypto.randomUUID(), at: new Date(NOW - 60_000).toISOString(), ...over });
const noSleep = () => {};

test("[GB-22] a live holder's lock is never stolen, however old; the waiter fails closed with OwnershipBusy", (t) => {
  const { file, lock } = lockFixture(t);
  const rec = lockRecord(); fs.writeFileSync(lock, JSON.stringify(rec));
  fs.utimesSync(lock, new Date(NOW - 3_600_000), new Date(NOW - 3_600_000)); // an hour old: the old rule stole it after ten seconds
  const identity = identityOf({ alive: new Map([[4242, { alive: true, startedMs: rec.startedMs + 400 }]]) });
  let ran = false;
  assert.throws(() => withMarkLock(file, () => { ran = true; }, { identity, sleep: noSleep, waitMs: 20 }), (e) => e instanceof OwnershipBusy && e.detail.verdict === "live" && /is alive/.test(e.message));
  assert.equal(ran, false);
  assert.equal(JSON.parse(fs.readFileSync(lock, "utf8")).nonce, rec.nonce, "the live holder's lock is untouched");
});

test("[GB-22] uncertain liveness fails closed: an unreadable lock, an unreadable identity, another host and a missing start are all left in place", (t) => {
  const { file, lock } = lockFixture(t);
  const cases = [
    ["garbage", "garbage", identityOf()],
    ["legacy pid-only lock", "4242", identityOf()],
    ["identity unreadable", JSON.stringify(lockRecord()), identityOf({ alive: new Map([[4242, new Error("ps failed")]]) })],
    ["another host", JSON.stringify(lockRecord({ host: "some-other-host" })), identityOf()],
    ["alive, no recorded start", JSON.stringify(lockRecord({ startedMs: null })), identityOf({ alive: new Map([[4242, { alive: true, startedMs: NOW }]]) })],
    ["alive, start unreadable", JSON.stringify(lockRecord()), identityOf({ alive: new Map([[4242, { alive: true, startedMs: null }]]) })],
  ];
  for (const [name, text, identity] of cases) {
    fs.writeFileSync(lock, text);
    assert.throws(() => withMarkLock(file, () => assert.fail("must not run"), { identity, sleep: noSleep, waitMs: 10 }), (e) => e instanceof OwnershipBusy, name);
    assert.equal(fs.readFileSync(lock, "utf8"), text, `${name}: the lock is untouched`);
  }
});

test("[GB-22] a holder proven dead is reclaimed: pid gone, pid reused by a process that started at another time, or another boot", (t) => {
  const { file, lock } = lockFixture(t);
  const rec = lockRecord();
  const cases = [
    ["pid gone", identityOf()],
    ["pid reused", identityOf({ alive: new Map([[4242, { alive: true, startedMs: rec.startedMs + 3_600_000 }]]) })],
    ["rebooted", identityOf({ boot: "boot-bbbbbbbbbbbbbbbb", alive: new Map([[4242, { alive: true, startedMs: rec.startedMs }]]) })],
  ];
  for (const [name, identity] of cases) {
    fs.writeFileSync(lock, JSON.stringify(rec));
    let ran = 0;
    withMarkLock(file, () => { ran += 1; assert.equal(JSON.parse(fs.readFileSync(lock, "utf8")).pid, process.pid, "the lock is now this process's"); }, { identity, sleep: noSleep, waitMs: 1000 });
    assert.equal(ran, 1, name);
    assert.equal(fs.existsSync(lock), false, `${name}: released`);
    assert.deepEqual(fs.readdirSync(path.dirname(lock)).filter((n) => n.includes("steal") || n.endsWith(".tmp")), [], `${name}: no claim or temp file left`);
  }
});

test("[GB-22] a dead holder's lock is stolen by one claimant only: a second observer of the same dead lock never removes the winner's lock", (t) => {
  const { file, lock } = lockFixture(t);
  const dead = lockRecord(); const text = JSON.stringify(dead);
  fs.writeFileSync(lock, text);
  const identity = identityOf();
  // A claimant is mid-steal (holds the claim, alive): the second waits and does not remove the dead lock.
  const claimant = lockRecord({ pid: 5151, nonce: crypto.randomUUID() });
  fs.writeFileSync(`${lock}.steal-${dead.nonce}`, JSON.stringify(claimant));
  const both = identityOf({ alive: new Map([[5151, { alive: true, startedMs: claimant.startedMs }]]) });
  assert.throws(() => withMarkLock(file, () => assert.fail("must not run"), { identity: both, sleep: noSleep, waitMs: 10 }), (e) => e instanceof OwnershipBusy);
  assert.equal(fs.readFileSync(lock, "utf8"), text, "the dead lock stays while a live claimant owns its steal");
  // The claimant died too: its claim is dropped, then the dead lock is taken.
  fs.rmSync(`${lock}.steal-${dead.nonce}`);
  fs.writeFileSync(`${lock}.steal-${dead.nonce}`, JSON.stringify(claimant));
  let ran = 0; withMarkLock(file, () => { ran += 1; }, { identity, sleep: noSleep, waitMs: 1000 });
  assert.equal(ran, 1);
});

test("[GB-22] release is token-checked: a resumed old holder whose lock was replaced does not remove the successor's lock", (t) => {
  const { file, lock } = lockFixture(t);
  const successor = lockRecord({ pid: 7777, nonce: crypto.randomUUID() });
  withMarkLock(file, () => {
    // While this holder is "suspended", its lock is replaced by another holder's.
    fs.rmSync(lock); fs.writeFileSync(lock, JSON.stringify(successor));
  }, { identity: identityOf(), sleep: noSleep, waitMs: 100 });
  assert.equal(JSON.parse(fs.readFileSync(lock, "utf8")).nonce, successor.nonce, "the successor's lock survives the old holder's release");
});

test("[GB-22] real processes: a live holder in another process blocks a mark; once it exits its lock is reclaimed and the mark lands", { skip: POSIX }, async (t) => {
  const file = path.join(tmp(t, "rk-own-lock-"), "build-ownership", "owner.json");
  const own = acquire(file, noLease);
  const holder = spawn(process.execPath, ["--input-type=module", "-e", `
    import { withMarkLock } from ${JSON.stringify(pathToFileURL(path.join(import.meta.dirname, "ownership-mark.mjs")).href)};
    withMarkLock(${JSON.stringify(file)}, () => { process.stdout.write("held\\n"); Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 20000); });`], { stdio: ["ignore", "pipe", "inherit"] });
  t.after(() => holder.kill("SIGKILL"));
  await new Promise((resolve, reject) => { holder.stdout.once("data", resolve); holder.once("exit", () => reject(new Error("the holder exited before holding"))); });
  const sub = `${own.handle}#aaaa1111`;
  assert.throws(() => markBrokerSubmit(file, own.handle, sub, { waitMs: 300 }), (e) => e instanceof OwnershipBusy && /is alive/.test(e.message), "a live holder in another process is never displaced");
  assert.equal(JSON.parse(fs.readFileSync(file, "utf8")).broker.submitted, false, "nothing was recorded");
  holder.kill("SIGKILL");
  await new Promise((resolve) => holder.once("exit", resolve));
  markBrokerSubmit(file, own.handle, sub, { waitMs: 15_000 });
  assert.deepEqual(JSON.parse(fs.readFileSync(file, "utf8")).broker.handles, [sub], "the dead holder's lock was reclaimed and the mark landed");
});

test("[GB-23] a reclaim needs REAPED for EACH recorded sub-handle, asked by its exact name: the bare handle (REAPED once any sibling completed) is not enough", (t) => {
  const file = path.join(tmp(t, "rk-own-sub-"), "build-ownership", "owner.json");
  const own = acquire(file, noLease);
  const sub1 = `${own.handle}#11111111-aaaa`; const sub2 = `${own.handle}#22222222-bbbb`; const sub3 = `${own.handle}#33333333-cccc`;
  own.markBrokerSubmit(sub1); own.markBrokerSubmit(sub2); own.markBrokerSubmit(sub3);
  // The broker's answers: the bare handle and sub1 completed; sub2 is UNKNOWN (sent, no completion); sub3 completed.
  const answers = (table) => { const asked = []; const lease = (q) => { asked.push(q.handle); return { state: table[q.handle] ?? "UNKNOWN", why: `fake ${table[q.handle] ?? "UNKNOWN"}` }; }; lease.asked = asked; return lease; };
  crash(file);
  const partial = answers({ [own.handle]: "REAPED", [sub1]: "REAPED", [sub3]: "REAPED" });
  assert.throws(() => acquire(file, partial), (e) => e instanceof OwnershipBusy && new RegExp(`sub-handle ${sub2} and its lease is UNKNOWN`).test(e.message));
  assert.deepEqual(partial.asked, [own.handle, sub1, sub2], "the parent first, then each sub-handle in send order, stopping at the first that is not REAPED");
  assert.equal(JSON.parse(fs.readFileSync(file, "utf8")).handle, own.handle, "the record is still held");
  // An ACTIVE sub-handle also holds it.
  assert.throws(() => acquire(file, answers({ [own.handle]: "REAPED", [sub1]: "ACTIVE" })), (e) => e instanceof OwnershipBusy && /ACTIVE/.test(e.message));
  // Every sub-handle REAPED: the record is reclaimed, and each one was asked exactly once.
  const all = answers({ [own.handle]: "REAPED", [sub1]: "REAPED", [sub2]: "REAPED", [sub3]: "REAPED" });
  const next = acquire(file, all);
  assert.deepEqual(all.asked, [own.handle, sub1, sub2, sub3]);
  assert.match(next.reclaimed.proofs.join("; "), /each of 3 sub-handle\(s\) is REAPED/);
  next.release();
});

test("[GB-23] a record that lists something that is not a sub-handle of its own handle is never reclaimed", (t) => {
  const file = path.join(tmp(t, "rk-own-sub-"), "build-ownership", "owner.json");
  const own = acquire(file, noLease);
  own.markBrokerSubmit(`${own.handle}#11111111-aaaa`);
  const rec = JSON.parse(fs.readFileSync(file, "utf8")); rec.broker.handles.push("someone-else#1"); fs.writeFileSync(file, JSON.stringify(rec));
  crash(file);
  assert.throws(() => acquire(file, leaseOf("REAPED")), (e) => e instanceof OwnershipBusy && /not a sub-handle/.test(e.message));
});
