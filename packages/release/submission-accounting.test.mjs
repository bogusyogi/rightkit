// GB-21, GB-23, GB-37, GB-69, GB-85: every carried submission is accounted for BY NAME in the build ownership.
// A request that provably never started does not wedge the next owner; an ambiguous send, a start that may have run and a completed sibling never count as complete;
// the submit hook's result is checked before anything is sent; a successful stage's release waits (bounded, by the broker's own --wait-ms) for every handle to be REAPED.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { OwnershipBusy, acquireBuildOwnership, brokerLeaseState } from "./build-ownership.mjs";
import { markBrokerUnsent } from "./ownership-mark.mjs";
import { brokerSection, carryBrokerAdapter, carryBrokerCargo, createSnapshotBroker, provenNoSpawn } from "./snapshot-broker.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

const POSIX = process.platform === "win32" ? "the fixture client is POSIX only" : false;
const tmp = (t, p) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), p))); t.after(() => fs.rmSync(d, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 })); return d; };
const NOW = Date.now();
const probe = () => ({ platform: "darwin", identity: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW - 60_000, group: 4242 } : { alive: false, startedMs: null, group: null }), bootTime: () => NOW - 86_400_000, groupSnapshot: () => ({ members: [], byPid: new Map([[1, { pid: 1, ppid: 0, startedMs: NOW - 9e7 }]]), own: new Set([process.pid]) }), tokenHolders: () => null, snapshotUsers: () => null, ancestors: () => [{ pid: 5000, startedMs: NOW - 9e6 }] });
const acquire = (file, lease, extra = {}) => acquireBuildOwnership({ file, platform: "darwin", probe: probe(), lease, env: {}, exportEnv: false, stage: "compile", snapshot: "/snap/x", ...extra });
const crash = (file) => { const rec = JSON.parse(fs.readFileSync(file, "utf8")); fs.writeFileSync(file, JSON.stringify({ ...rec, pid: 999_999, startedMs: NOW - 120_000, acquiredAt: new Date(NOW - 100_000).toISOString() })); return rec; };
const table = (answers) => { const asked = []; const lease = (q) => { asked.push(q); const a = answers[q.handle] ?? { state: "UNKNOWN", members: [] }; return { state: a.state, why: `fake ${a.state}`, ...(a.members ? { members: a.members } : {}) }; }; lease.asked = asked; return lease; };
const noLease = () => { throw new Error("no lease expected"); };
const GATE = "/gate/bin/rightkit-gate.mjs";
const env = (fx) => ({ PATH: fx.root, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: GATE });
const cargo = (fx, section, extra = {}) => carryBrokerCargo({ words: ["check"], cwd: path.join(fx.snapshot, "crates", "a"), env: env(fx), section, workspace: fx.snapshot, checkoutWorkspace: fx.checkout, record: () => {}, err: () => {}, stdio: "capture", ...extra });

// ---- GB-69 ----
test("[GB-69] a submit hook that records nothing, another handle's record or no record of this sub-handle sends zero bytes; a throwing hook too; a valid hook behaves as the env-file path", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  const sectionWith = (hook) => brokerSection({ ...fx.sb, markBrokerSubmit: hook });
  const good = (h) => ({ handle: fx.handle, broker: { submitted: true, handles: [h] } });
  for (const [name, hook] of [["a no-op", () => undefined], ["a wrong handle", (h) => ({ handle: "someone-else", broker: { submitted: true, handles: [h] } })], ["a record without this sub-handle", () => ({ handle: fx.handle, broker: { submitted: true, handles: ["x#other"] } })], ["not submitted", (h) => ({ handle: fx.handle, broker: { submitted: false, handles: [h] } })], ["a throwing hook", () => { throw new Error("lock busy"); }]]) {
    const r = cargo(fx, sectionWith(hook));
    assert.equal(r.outcome.status, "refused", name); assert.equal(r.outcome.code, "RK_CARGO_BROKER_OWNERSHIP", name);
    assert.equal(fx.calls().length, 0, `${name}: zero transport calls`);
  }
  assert.equal(cargo(fx, sectionWith(good)).outcome.status, "verified");
  assert.equal(fx.calls().length, 1);
});

// ---- GB-37 ----
test("[GB-37] createSnapshotBroker -> brokerSection -> a carried call forwards the exact sub-handle to the REAL ownership record before the send; a missing hook refuses", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  const file = path.join(tmp(t, "rk-own-e2e-"), "build-ownership", "owner.json");
  const own = acquire(file, noLease, { snapshot: fx.snapshot });
  const sb = createSnapshotBroker({ env: { RIGHTKIT_CONTROL_ROOT: fx.controlRoot, RIGHTKIT_DATA_ROOT: fx.dataRoot }, platform: process.platform, checkout: fx.checkout, commit: fx.sb.commit, snapshot: fx.sb.snapshot, ownership: own, client: fx.client, gateFile: GATE, dataRoot: fx.dataRoot, loadPins: () => ({ ...fx.pins, source: "test" }) });
  const r = cargo(fx, brokerSection(sb));
  assert.equal(r.outcome.status, "verified", JSON.stringify(r.outcome));
  const rec = JSON.parse(fs.readFileSync(file, "utf8"));
  assert.equal(rec.broker.submitted, true); assert.equal(rec.broker.handles.length, 1);
  assert.match(rec.broker.handles[0], new RegExp(`^${own.handle}#[A-Za-z0-9._-]+$`), "the real ownership recorded the carried request's own sub-handle");
  assert.equal(r.outcome.requestId !== undefined, true);
  // A hook that is not there cannot be called: the section carries none and the call falls to the env path, which refuses with no ownership in the environment.
  const bare = brokerSection({ ...sb, markBrokerSubmit: undefined });
  assert.equal(cargo(fx, bare).outcome.code, "RK_CARGO_BROKER_OWNERSHIP");
});

// ---- GB-21 ----
test("[GB-21] a client that is not there is refused BEFORE the mark (nothing recorded, nothing to reclaim); a client that vanishes after the check is marked unsent and does not wedge the next owner", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  const file = path.join(tmp(t, "rk-own-unsent-"), "build-ownership", "owner.json");
  const own = acquire(file, noLease, { snapshot: fx.snapshot });
  const sectionFor = (client) => brokerSection({ ...fx.sb, client, handle: own.handle, markBrokerSubmit: own.markBrokerSubmit, markBrokerUnsent: own.markBrokerUnsent });
  // 1. No such executable: refused before anything is marked.
  const missing = cargo(fx, sectionFor(path.join(fx.root, "no-such-rustright")));
  assert.equal(missing.outcome.code, "RK_CARGO_BROKER_CLIENT"); assert.match(missing.outcome.message, /nothing was marked or sent/);
  assert.equal(JSON.parse(fs.readFileSync(file, "utf8")).broker.submitted, false);
  // 2. The client existed at the check and failed to start (ENOENT / EINVAL / EACCES at spawn): marked, then proven unsent.
  for (const code of ["ENOENT", "EINVAL", "EACCES"]) {
    const r = cargo(fx, sectionFor(fx.client), { spawn: () => ({ error: Object.assign(new Error(`spawn ${code}`), { code }), status: null, stdout: "", stderr: "" }) });
    assert.equal(r.outcome.status, "unverified", code);
    assert.equal(provenNoSpawn({ code }), true);
  }
  const rec = JSON.parse(fs.readFileSync(file, "utf8"));
  assert.equal(rec.broker.handles.length, 3); assert.equal(Object.keys(rec.broker.unsent).length, 3, "each never-started sub-handle is recorded unsent under the lock");
  // 3. The next owner reclaims it: each sub-handle is UNKNOWN with no members at the broker, and the client proved it never started.
  crash(file);
  const lease = table({});
  const next = acquire(file, lease, { snapshot: fx.snapshot });
  assert.match(next.reclaimed.proofs.join("; "), /3 sub-handle\(s\) is accounted for: 0 REAPED, 3 proven never started/);
  next.release();
});

test("[GB-21] an ambiguous send is never marked unsent: a timeout after the client started, a refused-but-started client and a broker that knows the handle keep the record held", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  const file = path.join(tmp(t, "rk-own-amb-"), "build-ownership", "owner.json");
  const own = acquire(file, noLease, { snapshot: fx.snapshot });
  const section = brokerSection({ ...fx.sb, handle: own.handle, markBrokerSubmit: own.markBrokerSubmit, markBrokerUnsent: own.markBrokerUnsent });
  assert.equal(provenNoSpawn({ code: "ETIMEDOUT" }), false); assert.equal(provenNoSpawn(null), false);
  const r = cargo(fx, section, { spawn: () => ({ error: Object.assign(new Error("timed out"), { code: "ETIMEDOUT" }), status: null, stdout: "", stderr: "" }) });
  assert.equal(r.outcome.status, "unverified");
  const rec = JSON.parse(fs.readFileSync(file, "utf8"));
  assert.equal(rec.broker.handles.length, 1); assert.equal(rec.broker.unsent, undefined, "uncertainty is not erased from a timeout");
  crash(file);
  assert.throws(() => acquire(file, table({}), { snapshot: fx.snapshot }), (e) => e instanceof OwnershipBusy && /lease is UNKNOWN/.test(e.message));
  // An unsent mark with a broker that nevertheless holds something under the name is not settled either.
  const sub = rec.broker.handles[0];
  const rec2 = JSON.parse(fs.readFileSync(file, "utf8")); fs.writeFileSync(file, JSON.stringify({ ...rec2, broker: { ...rec2.broker, unsent: { [sub]: { reason: "x", at: "now" } } } }));
  assert.throws(() => acquire(file, table({ [sub]: { state: "UNKNOWN", members: [{ handle: sub, state: "UNKNOWN" }] } }), { snapshot: fs.realpathSync(fx.snapshot) }), (e) => e instanceof OwnershipBusy && new RegExp(`sub-handle ${sub.replace("#", "\\#")}`).test(e.message));
});

test("[GB-21] an unsent mark is only for a sub-handle already marked as sent, of this holder, with a reason", (t) => {
  const file = path.join(tmp(t, "rk-own-unsent-"), "build-ownership", "owner.json");
  const own = acquire(file, noLease);
  const sub = `${own.handle}#aaaa-1111`;
  assert.throws(() => own.markBrokerUnsent(sub, "client missing"), /never marked as sent/);
  own.markBrokerSubmit(sub);
  assert.throws(() => own.markBrokerUnsent(sub, ""), /names the local proof/);
  assert.throws(() => own.markBrokerUnsent(own.handle, "x"), /only a sub-handle/);
  assert.throws(() => markBrokerUnsent(file, "other-holder", `other-holder#1`, "x"), /no longer held/);
  own.markBrokerUnsent(sub, "client missing");
  assert.equal(JSON.parse(fs.readFileSync(file, "utf8")).broker.unsent[sub].reason, "client missing");
});

// ---- GB-23 ----
test("[GB-23] a completed sibling never stands in for an unknown or active submission, and a REAPED answer must list the submission it names", (t) => {
  const file = path.join(tmp(t, "rk-own-members-"), "build-ownership", "owner.json");
  const own = acquire(file, noLease);
  const a = `${own.handle}#a`; const b = `${own.handle}#b`;
  own.markBrokerSubmit(a); own.markBrokerSubmit(b);
  crash(file);
  const done = (h) => ({ state: "REAPED", members: [{ handle: h, state: "REAPED", spawned: true }] });
  // The bare handle is REAPED (a completed): b is a claimed submission with no completion: UNKNOWN. The record is held.
  assert.throws(() => acquire(file, table({ [own.handle]: { state: "REAPED", members: [{ handle: a, state: "REAPED" }, { handle: b, state: "UNKNOWN" }] }, [a]: done(a), [b]: { state: "UNKNOWN", members: [{ handle: b, state: "UNKNOWN" }] } })), (e) => /#b( and its lease is UNKNOWN|.* as UNKNOWN among the members)/.test(e.message));
  // An ACTIVE sibling holds it.
  assert.throws(() => acquire(file, table({ [own.handle]: { state: "ACTIVE", members: [{ handle: a, state: "REAPED" }, { handle: b, state: "ACTIVE" }] } })), (e) => /ACTIVE/.test(e.message));
  // REAPED for a sub-handle whose answer lists no completed member of that name is inconsistent: held.
  assert.throws(() => acquire(file, table({ [own.handle]: done(a), [a]: { state: "REAPED", members: [] } })), (e) => /lists no completed submission under that name/.test(e.message));
  // Members that are not submissions under the queried handle are not an answer at all.
  const sub = brokerLeaseState({ snapshot: "/s", handle: own.handle, client: "/c", spawn: () => ({ status: 0, stdout: JSON.stringify({ schema: "rustright.snapshot-lease-status.v1", state: "REAPED", snapshot: "/s", ownership: own.handle, members: [{ handle: "other#1", state: "REAPED" }] }) }) });
  assert.equal(sub.state, "UNKNOWN"); assert.match(sub.why, /not submissions under/);
  // Both completed, with their members: reclaimed.
  const next = acquire(file, table({ [own.handle]: { state: "REAPED", members: [{ handle: a, state: "REAPED" }, { handle: b, state: "REAPED" }] }, [a]: done(a), [b]: done(b) }));
  assert.match(next.reclaimed.proofs.join("; "), /each of 2 sub-handle\(s\) is REAPED/);
  next.release();
});

// ---- GB-85 ----
test("[GB-85] a stage's release waits for every handle to be REAPED by the broker's own bounded --wait-ms; on timeout it keeps ownership UNKNOWN and says why; an old client is asked plainly", (t) => {
  const file = path.join(tmp(t, "rk-own-wait-"), "build-ownership", "owner.json");
  const own = acquire(file, table({}), { snapshot: "/snap/x" });
  const sub = `${own.handle}#a`; own.markBrokerSubmit(sub);
  // The release asks with a wait; a timeout keeps the record held (never released, never a poll of the build).
  const timedOut = (handle) => ({ state: "UNKNOWN", why: "waitTimedOut", members: [{ handle: sub, state: "ACTIVE" }] });
  void timedOut;
  const asked = [];
  const own2 = acquire(path.join(tmp(t, "rk-own-wait-"), "build-ownership", "owner.json"), (q) => { asked.push(q); return { state: "UNKNOWN", why: "still ACTIVE after waiting 15000 ms (waitTimedOut); ownership stays UNKNOWN", waitTimedOut: true }; }, { snapshot: "/snap/x" });
  own2.markBrokerSubmit(`${own2.handle}#z`);
  const held = own2.release();
  assert.equal(held.released, false); assert.match(held.why, /waitTimedOut|stays UNKNOWN/);
  assert.ok(asked.length >= 1 && asked.every((q) => q.waitMs === 15_000), `the release waits (bounded): ${JSON.stringify(asked)}`);
  // Every handle REAPED after the wait: released, with the proofs.
  const ok = acquire(path.join(tmp(t, "rk-own-wait-"), "build-ownership", "owner.json"), (q) => ({ state: "REAPED", why: "ok", members: [{ handle: q.handle.includes("#") ? q.handle : `${q.handle}#z`, state: "REAPED" }] }), { snapshot: "/snap/x" });
  ok.markBrokerSubmit(`${ok.handle}#z`);
  assert.equal(ok.release().released, true);
  // The client command line.
  const argvs = [];
  const spawn = (client, args) => { argvs.push(args); return args.includes("--wait-ms") ? { status: 2, stdout: "", stderr: "error: unexpected argument '--wait-ms'" } : { status: 0, stdout: JSON.stringify({ schema: "rustright.snapshot-lease-status.v1", state: "REAPED", snapshot: "/s", ownership: "h", members: [{ handle: "h#1", state: "REAPED" }], waitedMs: 0, waitTimedOut: false }) }; };
  const state = brokerLeaseState({ snapshot: "/s", handle: "h", client: "/c", spawn, waitMs: 500 });
  assert.equal(state.state, "REAPED");
  assert.deepEqual(argvs.map((a) => a.includes("--wait-ms") ? a.at(-1) : null), ["500", null], "asked with the wait, then plainly when the client predates it");
  const timed = brokerLeaseState({ snapshot: "/s", handle: "h", client: "/c", waitMs: 99_999, spawn: (c, a) => ({ status: 0, stdout: JSON.stringify({ schema: "rustright.snapshot-lease-status.v1", state: "ACTIVE", snapshot: "/s", ownership: "h", waitedMs: 30000, waitTimedOut: true, members: [{ handle: "h#1", state: "ACTIVE", spawned: true }] }), stderr: String(a.at(-1)) }) });
  assert.equal(timed.state, "UNKNOWN"); assert.equal(timed.waitTimedOut, true); assert.match(timed.why, /waitTimedOut/);
});

// ---- GB-57 (RightKit side of stage members sharing one snapshot lease) ----
test("[GB-57] each carried call of one stage has its own handle <stage handle>#<call> under ONE commit and tree; every member is recorded and must be REAPED by name before the stage's ownership is released", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  const file = path.join(tmp(t, "rk-own-stage-"), "build-ownership", "owner.json");
  const own = acquire(file, noLease, { snapshot: fx.snapshot });
  const section = brokerSection({ ...fx.sb, handle: own.handle, markBrokerSubmit: own.markBrokerSubmit, markBrokerUnsent: own.markBrokerUnsent });
  const records = [];
  for (let i = 0; i < 3; i += 1) assert.equal(cargo(fx, section, { record: (e) => records.push(e) }).outcome.status, "verified");
  const handles = JSON.parse(fs.readFileSync(file, "utf8")).broker.handles;
  assert.equal(new Set(handles).size, 3, "a handle is spent once: three calls, three handles");
  assert.ok(handles.every((h) => h.startsWith(`${own.handle}#`)), "the stage owner is the part before the first #");
  assert.equal(new Set(records.filter((r) => r.status === "verified").map((r) => r.ownershipHandle)).size, 3);
  // The stage's release asks for each member by name (the fixture broker lists them as members), and releases only when all are REAPED.
  const asked = []; const lease = (q) => { asked.push(q.handle); const ok = q.handle === own.handle ? handles : [q.handle]; return { state: "REAPED", why: "REAPED", members: ok.map((h) => ({ handle: h, state: "REAPED" })) }; };
  crash(file);
  const next = acquire(file, lease, { snapshot: fx.snapshot });
  assert.deepEqual(asked, [own.handle, ...handles], "the aggregate first, then every member by its exact name");
  next.release();
});
