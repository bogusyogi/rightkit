// When broker work "may exist" under a build ownership (build-ownership.mjs markBrokerSubmit). The record is marked ONLY immediately
// before a snapshot-bound (carried) request is sent, under that request's own sub-handle `<handle>#<uuid>`, and only that mark makes a
// reclaim wait for the broker's REAPED. An installed carrier map and an uncarried probe (`rustc -vV`, `--version`, doctor) never mark:
// the broker has no lease for a handle it never saw a snapshot-bound request under, so it could never answer REAPED and the host
// would stay locked until a manual clear (gate 5c388c71, stage-sidecar's `rustc -vV`).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";
import { spawn, spawnSync } from "./background-process.mjs";
import { OWNERSHIP_SCHEMA, OwnershipBusy, acquireBuildOwnership, markCarriedSend, markLockFile } from "./build-ownership.mjs";
import { CARRIED_CARGO_MAP_ENV, installCarrier } from "./carried-cargo.mjs";
import { brokerSection } from "./snapshot-broker.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

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
const noLease = () => { throw new Error("the broker lease must not be asked when no carried request was sent"); };
const acquire = (file, lease, extra = {}) => acquireBuildOwnership({ file, platform: "darwin", probe: probe(), lease, env: {}, exportEnv: false, stage: "compile", snapshot: "/snap/x", ...extra });
/** The held record, as if its gate had died (a crash): another pid, the same handle, snapshot and broker state. */
const crash = (file) => { const rec = JSON.parse(fs.readFileSync(file, "utf8")); fs.writeFileSync(file, JSON.stringify({ ...rec, pid: 999_999, startedMs: NOW - 120_000, acquiredAt: new Date(NOW - 100_000).toISOString() })); return rec; };

test("a stage that installs its carrier and sends only an uncarried probe, then fails, leaves a reclaimable ownership", { skip: POSIX }, (t) => {
  const dir = tmp(t, "rk-own-sub-");
  const file = path.join(dir, "build-ownership", "owner.json");
  const fx = makeFakeBroker(t);
  const own = acquire(file, noLease);
  // Installing the carrier map for the stage's children sends nothing and records nothing.
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout }, section: { ...brokerSection(fx.sb), handle: own.handle }, platform: "darwin", env: {} });
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

test("a child process's carried send marks the shared owner record; a child that sends nothing, or only an uncarried probe, does not", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout, native: path.join(fx.checkout, "native") }, section: brokerSection(fx.sb), platform: "darwin", env: {} });
  t.after(() => installed.cleanup());
  const cwd = path.join(fx.snapshot, "crates", "a");
  // The child is a separate process that holds no ownership object: it inherits the handle and the record's path (as a right-release child of the gate does).
  const child = (script, env = {}) => spawnSync(process.execPath, ["--input-type=module", "-e", script], { cwd, encoding: "utf8", env: { PATH: process.env.PATH, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: GATE, ...installed.env, ...env } });
  const managed = pathToFileURL(path.join(import.meta.dirname, "managed-cargo.mjs")).href;
  const carriedBuild = `import { runCargoSync } from ${JSON.stringify(managed)}; const r = runCargoSync(["build"], { cwd: ${JSON.stringify(cwd)}, env: process.env, encoding: "utf8" }, { platform: "darwin" }); process.stderr.write(r.stderr ?? ""); process.exit(r.status ?? 1);`;
  // A child that only exits (or fails on a config error before any call) sends nothing.
  assert.equal(child("process.exit(3)", fx.ownershipEnv).status, 3);
  assert.equal(fx.owner().broker.submitted, false);
  // The child's carried call: the record is marked before the send, with that request's sub-handle.
  const sent = child(carriedBuild, fx.ownershipEnv);
  assert.equal(sent.status, 0, sent.stderr);
  const rec = fx.owner();
  assert.equal(rec.broker.submitted, true);
  assert.equal(rec.broker.handles.length, 1);
  assert.match(rec.broker.handles[0], /^own-7#[0-9a-f-]{36}$/);
  assert.equal(fx.calls().length, 1);
  // A second child call appends its own sub-handle; the first stays.
  const again = child(carriedBuild, fx.ownershipEnv);
  assert.equal(again.status, 0, again.stderr);
  const after = fx.owner();
  assert.equal(after.broker.handles.length, 2);
  assert.equal(after.broker.handles[0], rec.broker.handles[0]);
  assert.notEqual(after.broker.handles[1], rec.broker.handles[0]);
});

test("a carried request is not sent when its mark cannot land: no ownership in the environment, another handle's, or a record that is gone", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout }, section: brokerSection(fx.sb), platform: "darwin", env: {} });
  t.after(() => installed.cleanup());
  const cwd = path.join(fx.snapshot, "crates", "a");
  const managed = pathToFileURL(path.join(import.meta.dirname, "managed-cargo.mjs")).href;
  const script = `import { runCargoSync } from ${JSON.stringify(managed)}; const r = runCargoSync(["build"], { cwd: ${JSON.stringify(cwd)}, env: process.env, encoding: "utf8" }, { platform: "darwin" }); process.stderr.write(r.stderr ?? ""); process.exit(r.status ?? 1);`;
  const run = (env) => spawnSync(process.execPath, ["--input-type=module", "-e", script], { cwd, encoding: "utf8", env: { PATH: process.env.PATH, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: GATE, ...installed.env, ...env } });
  const key = Object.keys(fx.ownershipEnv);
  for (const [name, env] of [["no ownership", {}], ["another handle", { [key[0]]: "someone-else", [key[1]]: fx.ownership }]]) {
    const r = run(env);
    assert.notEqual(r.status, 0, name);
    assert.match(r.stderr, /RK_CARGO_BROKER_OWNERSHIP/, name);
  }
  fs.rmSync(fx.ownership);
  const gone = run(fx.ownershipEnv);
  assert.notEqual(gone.status, 0);
  assert.match(gone.stderr, /RK_CARGO_BROKER_OWNERSHIP/);
  assert.deepEqual(fx.calls(), [], "nothing reached the broker");
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
