// GB-21, GB-23, GB-37, GB-69, GB-85: every carried submission is accounted for BY NAME in the build ownership.
// A request that provably never started does not wedge the next owner; an ambiguous send, a start that may have run and a completed sibling never count as complete;
// the submit hook's result is checked before anything is sent; a successful stage's release waits (bounded, by the broker's own --wait-ms) for every handle to be REAPED.
//
// Evidence labels. "data:" titles call the real production functions (the real build ownership, the real lease parser) with constructed inputs and are NOT broker evidence; "real broker:" titles
// run the installed RustRight client against an isolated instance of the installed broker (test-support/real-broker.mjs). No simulated broker exists in this file.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { OwnershipBusy, acquireBuildOwnership, brokerLeaseState } from "./build-ownership.mjs";
import { markBrokerUnsent } from "./ownership-mark.mjs";
import { brokerSection, carryBrokerCargo, createSnapshotBroker, provenNoSpawn } from "./snapshot-broker.mjs";
import { startRealBroker } from "./test-support/real-broker.mjs";

const POSIX = process.platform === "win32" ? "the carried client environment is POSIX only" : false;
const tmp = (t, p) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), p))); t.after(() => fs.rmSync(d, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 })); return d; };
const NOW = Date.now();
const probe = () => ({ platform: "darwin", identity: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW - 60_000, group: 4242 } : { alive: false, startedMs: null, group: null }), bootTime: () => NOW - 86_400_000, groupSnapshot: () => ({ members: [], byPid: new Map([[1, { pid: 1, ppid: 0, startedMs: NOW - 9e7 }]]), own: new Set([process.pid]) }), tokenHolders: () => null, snapshotUsers: () => null, ancestors: () => [{ pid: 5000, startedMs: NOW - 9e6 }] });
const acquire = (file, lease, extra = {}) => acquireBuildOwnership({ file, platform: "darwin", probe: probe(), lease, env: {}, exportEnv: false, stage: "compile", snapshot: "/snap/x", ...extra });
const crash = (file) => { const rec = JSON.parse(fs.readFileSync(file, "utf8")); fs.writeFileSync(file, JSON.stringify({ ...rec, pid: 999_999, startedMs: NOW - 120_000, acquiredAt: new Date(NOW - 100_000).toISOString() })); return rec; };
const noLease = () => { throw new Error("no lease expected"); };
const GATE = "/gate/bin/rightkit-gate.mjs";
const COMMIT = { sha: "c".repeat(40), tree: "e".repeat(40) };
const WORKSPACES = { "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n', "crates/a/src/lib.rs": "\n" };
const writeTree = (root, files) => { for (const [rel, body] of Object.entries(files)) { const f = path.join(root, rel); fs.mkdirSync(path.dirname(f), { recursive: true }); fs.writeFileSync(f, body); } };
const PINS = { host: "host-1", keyId: "0".repeat(64), publicKey: "04".padEnd(130, "0"), brokerDigest: "b".repeat(64) };
/**
 * Plain checkout and snapshot directories and the broker section around a client: the installed one of an isolated real broker (`rb`, whose control root, data root and environment the call
 * uses), or, with none, a client path that is only ever the target of an injected `spawn` (a file that exists, never started: node itself).
 */
function host(t, { rb = null } = {}) {
  const root = tmp(t, "rk-acct-host-");
  const checkout = path.join(root, "checkout"); const snapshot = path.join(rb ? path.join(rb.dataRoot, "snapshots") : path.join(root, "data", "snapshots"), "0123456789abcdef");
  writeTree(checkout, WORKSPACES); writeTree(snapshot, WORKSPACES);
  const client = rb?.clientPath ?? process.execPath;
  const sb = { controlRoot: rb?.controlRoot ?? path.join(root, "control"), commit: COMMIT, snapshot: { path: snapshot, expectedTree: COMMIT.tree }, declaredInputs: [], handle: "unused", pins: PINS, checkout, outputRoots: [path.join(rb?.dataRoot ?? path.join(root, "data"), "rightkit-build-control", "ws")], platform: process.platform, client };
  const env = { PATH: process.env.PATH, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: GATE, ...(rb?.env ?? {}) };
  const lease = (q) => (rb ? brokerLeaseState({ ...q, env: rb.env, platform: process.platform, client: rb.clientPath }) : (() => { throw new Error("no broker in this test"); })());
  return { root, checkout, snapshot, client, sb, env, lease, rb };
}
const cargo = (fx, section, extra = {}) => carryBrokerCargo({ words: ["check"], cwd: path.join(fx.snapshot, "crates", "a"), env: fx.env, section, workspace: fx.snapshot, checkoutWorkspace: fx.checkout, record: () => {}, err: () => {}, stdio: "capture", ...extra });
/** A recording `spawn` standing for the OS when the client is started: what the gate asked the OS to run, and nothing more. */
const recording = (reply = { status: 0, stdout: "", stderr: "" }) => { const calls = []; const spawn = (file, args, options) => { calls.push({ file, args, options }); return typeof reply === "function" ? reply(calls.length - 1) : reply; }; spawn.calls = calls; return spawn; };
const table = (answers) => { const asked = []; const lease = (q) => { asked.push(q); const a = answers[q.handle] ?? { state: "UNKNOWN", members: [] }; return { state: a.state, why: `answer ${a.state}`, ...(a.members ? { members: a.members } : {}) }; }; lease.asked = asked; return lease; };
// ---- GB-69 ----
test("data: [GB-69] a submit hook that records nothing, another handle's record or no record of this sub-handle sends zero bytes; a throwing hook too; a valid hook behaves as the env-file path", { skip: POSIX }, (t) => {
  const fx = host(t);
  const handle = "own-1";
  const sectionWith = (hook) => brokerSection({ ...fx.sb, handle, markBrokerSubmit: hook });
  const good = (h) => ({ handle, broker: { submitted: true, handles: [h] } });
  const spawn = recording();
  for (const [name, hook] of [["a no-op", () => undefined], ["a wrong handle", (h) => ({ handle: "someone-else", broker: { submitted: true, handles: [h] } })], ["a record without this sub-handle", () => ({ handle, broker: { submitted: true, handles: ["x#other"] } })], ["not submitted", (h) => ({ handle, broker: { submitted: false, handles: [h] } })], ["a throwing hook", () => { throw new Error("lock busy"); }]]) {
    const r = cargo(fx, sectionWith(hook), { spawn });
    assert.equal(r.outcome.status, "refused", name); assert.equal(r.outcome.code, "RK_CARGO_BROKER_OWNERSHIP", name);
    assert.equal(spawn.calls.length, 0, `${name}: zero transport calls`);
  }
  assert.notEqual(cargo(fx, sectionWith(good), { spawn }).outcome.status, "refused");
  assert.equal(spawn.calls.length, 1, "a valid hook lets the one client call start");
});

test("real broker: [GB-69] a submit hook that records nothing sends zero requests to the real broker; a valid one reaches it", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "acct-hook" });
  t.after(() => rb.stop());
  const fx = host(t, { rb });
  const atBroker = () => (fs.existsSync(rb.receiptsDir) ? fs.readdirSync(rb.receiptsDir).length : 0);
  const none = cargo(fx, brokerSection({ ...fx.sb, handle: "own-1", markBrokerSubmit: () => undefined }));
  assert.equal(none.outcome.code, "RK_CARGO_BROKER_OWNERSHIP");
  assert.equal(atBroker(), 0, "nothing was sent");
  const file = path.join(tmp(t, "rk-own-real-hook-"), "build-ownership", "owner.json");
  const own = acquire(file, noLease, { snapshot: fx.snapshot });
  const sent = cargo(fx, brokerSection({ ...fx.sb, handle: own.handle, markBrokerSubmit: own.markBrokerSubmit, markBrokerUnsent: own.markBrokerUnsent }));
  assert.notEqual(sent.outcome.status, "refused", JSON.stringify(sent.outcome));
  assert.equal(JSON.parse(fs.readFileSync(file, "utf8")).broker.handles.length, 1, "the record names the sent sub-handle");
});

// ---- GB-37 ----
test("real broker: [GB-37] createSnapshotBroker -> brokerSection -> a carried call forwards the exact sub-handle to the REAL ownership record before the send; a missing hook refuses", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "acct-e2e" });
  t.after(() => rb.stop());
  const fx = host(t, { rb });
  const file = path.join(tmp(t, "rk-own-e2e-"), "build-ownership", "owner.json");
  const own = acquire(file, noLease, { snapshot: fx.snapshot });
  const sb = createSnapshotBroker({ env: { RIGHTKIT_CONTROL_ROOT: rb.controlRoot, RIGHTKIT_DATA_ROOT: rb.dataRoot }, platform: process.platform, checkout: fx.checkout, commit: COMMIT, snapshot: { path: fx.snapshot, expectedTree: COMMIT.tree }, ownership: own, client: rb.clientPath, gateFile: GATE, dataRoot: rb.dataRoot, loadPins: () => ({ ...PINS, source: "test" }) });
  const r = cargo(fx, brokerSection(sb));
  assert.notEqual(r.outcome.status, "refused", JSON.stringify(r.outcome));
  const rec = JSON.parse(fs.readFileSync(file, "utf8"));
  assert.equal(rec.broker.submitted, true); assert.equal(rec.broker.handles.length, 1);
  assert.match(rec.broker.handles[0], new RegExp(`^${own.handle}#[A-Za-z0-9._-]+$`), "the real ownership recorded the carried request's own sub-handle");
  // A hook that is not there cannot be called: the section carries none and the call falls to the env path, which refuses with no ownership in the environment.
  const bare = brokerSection({ ...sb, markBrokerSubmit: undefined });
  assert.equal(cargo(fx, bare).outcome.code, "RK_CARGO_BROKER_OWNERSHIP");
});

// ---- GB-21 ----
test("real broker: [GB-21] a client that is not there is refused BEFORE the mark (nothing recorded, nothing to reclaim); a client that vanishes after the check is marked unsent and does not wedge the next owner", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "acct-unsent" });
  t.after(() => rb.stop());
  const fx = host(t, { rb });
  const file = path.join(tmp(t, "rk-own-unsent-"), "build-ownership", "owner.json");
  const own = acquire(file, noLease, { snapshot: fx.snapshot });
  const sectionFor = (client) => brokerSection({ ...fx.sb, client, handle: own.handle, markBrokerSubmit: own.markBrokerSubmit, markBrokerUnsent: own.markBrokerUnsent });
  // 1. No such executable: refused before anything is marked.
  const missing = cargo(fx, sectionFor(path.join(fx.root, "no-such-rustright")));
  assert.equal(missing.outcome.code, "RK_CARGO_BROKER_CLIENT"); assert.match(missing.outcome.message, /nothing was marked or sent/);
  assert.equal(JSON.parse(fs.readFileSync(file, "utf8")).broker.submitted, false);
  // 2. The client existed at the check and failed to start (ENOENT / EINVAL / EACCES at spawn, the OS's own error): marked, then proven unsent.
  for (const code of ["ENOENT", "EINVAL", "EACCES"]) {
    const r = cargo(fx, sectionFor(fx.client), { spawn: () => ({ error: Object.assign(new Error(`spawn ${code}`), { code }), status: null, stdout: "", stderr: "" }) });
    assert.equal(r.outcome.status, "unverified", code);
    assert.equal(provenNoSpawn({ code }), true);
  }
  const rec = JSON.parse(fs.readFileSync(file, "utf8"));
  assert.equal(rec.broker.handles.length, 3); assert.equal(Object.keys(rec.broker.unsent).length, 3, "each never-started sub-handle is recorded unsent under the lock");
  // 3. The next owner reclaims it. The broker's answer is the REAL broker's: it never saw these sub-handles, so each is UNKNOWN with no members, and the client proved it never started.
  crash(file);
  const next = acquire(file, fx.lease, { snapshot: fx.snapshot });
  assert.match(next.reclaimed.proofs.join("; "), /3 sub-handle\(s\) is accounted for: 0 REAPED, 3 proven never started/);
  next.release();
});

test("real broker: [GB-21] an ambiguous send is never marked unsent: a timeout after the client started keeps the record held, and a broker that holds something under an unsent name does not settle it either", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "acct-amb" });
  t.after(() => rb.stop());
  const fx = host(t, { rb });
  const file = path.join(tmp(t, "rk-own-amb-"), "build-ownership", "owner.json");
  const own = acquire(file, noLease, { snapshot: fx.snapshot });
  const section = brokerSection({ ...fx.sb, handle: own.handle, markBrokerSubmit: own.markBrokerSubmit, markBrokerUnsent: own.markBrokerUnsent });
  assert.equal(provenNoSpawn({ code: "ETIMEDOUT" }), false); assert.equal(provenNoSpawn(null), false);
  const r = cargo(fx, section, { spawn: () => ({ error: Object.assign(new Error("timed out"), { code: "ETIMEDOUT" }), status: null, stdout: "", stderr: "" }) });
  assert.equal(r.outcome.status, "unverified");
  const rec = JSON.parse(fs.readFileSync(file, "utf8"));
  assert.equal(rec.broker.handles.length, 1); assert.equal(rec.broker.unsent, undefined, "uncertainty is not erased from a timeout");
  crash(file);
  // The real broker has no completion for a request it never received: UNKNOWN, so the ambiguous send keeps the record held.
  assert.throws(() => acquire(file, fx.lease, { snapshot: fx.snapshot }), (e) => e instanceof OwnershipBusy && /lease is UNKNOWN/.test(e.message));
  // An unsent mark with a broker that nevertheless holds something under the name is not settled either (the answer is a constructed one: a real broker is never made to hold a request it never received).
  const sub = rec.broker.handles[0];
  const rec2 = JSON.parse(fs.readFileSync(file, "utf8")); fs.writeFileSync(file, JSON.stringify({ ...rec2, broker: { ...rec2.broker, unsent: { [sub]: { reason: "x", at: "now" } } } }));
  assert.throws(() => acquire(file, table({ [sub]: { state: "UNKNOWN", members: [{ handle: sub, state: "UNKNOWN" }] } }), { snapshot: fs.realpathSync(fx.snapshot) }), (e) => e instanceof OwnershipBusy && new RegExp(`sub-handle ${sub.replace("#", "\\#")}`).test(e.message));
});

test("data: [GB-21] an unsent mark is only for a sub-handle already marked as sent, of this holder, with a reason", (t) => {
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
test("data: [GB-23] a completed sibling never stands in for an unknown or active submission, and a REAPED answer must list the submission it names", (t) => {
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
test("data: [GB-85] a stage's release waits for every handle to be REAPED by the broker's own bounded --wait-ms; on timeout it keeps ownership UNKNOWN and says why; an old client is asked plainly", (t) => {
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

// ---- GB-85, from the real broker ----
test("real broker: [GB-85] the real client accepts the bounded --wait-ms of `snapshot-lease status` and answers a handle it never admitted UNKNOWN, never REAPED", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "acct-wait" });
  t.after(() => rb.stop());
  const fx = host(t, { rb });
  const state = brokerLeaseState({ snapshot: fx.snapshot, handle: "never-admitted#a", env: rb.env, platform: process.platform, client: rb.clientPath, waitMs: 500 });
  assert.notEqual(state.state, "REAPED", JSON.stringify(state));
  assert.doesNotMatch(String(state.why ?? ""), /unexpected argument|--wait-ms/, "the installed client reads --wait-ms");
  const raw = rb.run(["snapshot-lease", "status", "--snapshot", fx.snapshot, "--ownership", "never-admitted#a", "--wait-ms", "500"]);
  const doc = JSON.parse(raw.stdout);
  assert.equal(doc.schema, "rustright.snapshot-lease-status.v1");
  assert.equal(doc.state, "UNKNOWN"); assert.deepEqual(doc.members, []);
});

// ---- GB-57 (RightKit side of stage members sharing one snapshot lease) ----
test("data: [GB-57] the stage's release asks for each member by name (the aggregate first), and releases only when all are REAPED", { skip: POSIX }, (t) => {
  const file = path.join(tmp(t, "rk-own-stage-"), "build-ownership", "owner.json");
  const own = acquire(file, noLease, { snapshot: "/snap/x" });
  const handles = [1, 2, 3].map((i) => `${own.handle}#call-${i}`);
  for (const h of handles) own.markBrokerSubmit(h);
  const asked = []; const lease = (q) => { asked.push(q.handle); const ok = q.handle === own.handle ? handles : [q.handle]; return { state: "REAPED", why: "REAPED", members: ok.map((h) => ({ handle: h, state: "REAPED" })) }; };
  crash(file);
  const next = acquire(file, lease, { snapshot: "/snap/x" });
  assert.deepEqual(asked, [own.handle, ...handles], "the aggregate first, then every member by its exact name");
  next.release();
});

test("real broker: [GB-57] each carried call of one stage has its own handle <stage handle>#<call>; every member is recorded, and the stage's ownership is reclaimed only if the real broker reports every one REAPED", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "acct-stage" });
  t.after(() => rb.stop());
  const fx = host(t, { rb });
  const file = path.join(tmp(t, "rk-own-stage-"), "build-ownership", "owner.json");
  const own = acquire(file, noLease, { snapshot: fx.snapshot });
  const section = brokerSection({ ...fx.sb, handle: own.handle, markBrokerSubmit: own.markBrokerSubmit, markBrokerUnsent: own.markBrokerUnsent });
  const records = [];
  for (let i = 0; i < 3; i += 1) assert.notEqual(cargo(fx, section, { record: (e) => records.push(e) }).outcome.status, "refused");
  const handles = JSON.parse(fs.readFileSync(file, "utf8")).broker.handles;
  assert.equal(new Set(handles).size, 3, "a handle is spent once: three calls, three handles");
  assert.ok(handles.every((h) => h.startsWith(`${own.handle}#`)), "the stage owner is the part before the first #");
  assert.equal(new Set(records.filter((r) => r.status === "pending").map((r) => r.ownershipHandle)).size, 3);
  crash(file);
  // The release answers to the real broker. It may be reclaimed only when every member is REAPED there; otherwise the record stays held.
  let next = null; let held = null;
  try { next = acquire(file, fx.lease, { snapshot: fx.snapshot }); } catch (e) { held = e; }
  if (next) {
    for (const h of handles) assert.equal(fx.lease({ snapshot: fx.snapshot, handle: h }).state, "REAPED", `${h} was reclaimed, so the real broker must report it REAPED`);
    next.release();
  } else assert.ok(held instanceof OwnershipBusy, String(held));
});
