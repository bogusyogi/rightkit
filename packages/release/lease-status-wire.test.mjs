// GB-23 / GB-85 (BROKER-CONTRACT section 16): SNAPSHOT_LEASE_STATUS is consumed exactly as RustRight answers it, wherever it is read (the release, the reclaim, the submission accounting)
// and as the fake broker produces it. The answer is {type, schema, state, snapshot, ownership, members[], waitedMs, waitTimedOut}: `members` is one row per submission under the handle
// (the aggregate `state` REAPED needs at least one member and every member REAPED); `waitMs` (the CLI's --wait-ms, 0 to 30000, clamped) makes the broker answer as soon as the aggregate is
// REAPED or at the bound with the state it has; `waitedMs` is the time it waited and `waitTimedOut` is true exactly when a wait was asked and the state is not REAPED. A gate releases
// ownership only on a well-formed REAPED answer and keeps it UNKNOWN, with the reason, on waitTimedOut, on any contradiction and on any answer it cannot read.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { acquireBuildOwnership, brokerLeaseState } from "./build-ownership.mjs";
import { spawnSync } from "./background-process.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

const POSIX = process.platform === "win32" ? "the fixture client is POSIX only" : false;
const SCHEMA = "rustright.snapshot-lease-status.v1";
const tmp = (t, p) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), p))); t.after(() => fs.rmSync(d, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 })); return d; };
const ask = (fx, handle, wait = null, snapshot = fx.snapshot) => {
  const r = spawnSync(fx.client, ["snapshot-lease", "status", "--snapshot", snapshot, "--ownership", handle, ...(wait === null ? [] : ["--wait-ms", String(wait)])], { encoding: "utf8" });
  return { status: r.status, answer: r.status === 0 ? JSON.parse(String(r.stdout).trim().split("\n").pop()) : null, stderr: String(r.stderr) };
};
const answerOf = (over = {}) => ({ schema: SCHEMA, state: "REAPED", snapshot: "/s", ownership: "h", members: [{ handle: "h#1", state: "REAPED", spawned: true }], waitedMs: 0, waitTimedOut: false, ...over });
const reading = (answer, { waitMs = 0 } = {}) => brokerLeaseState({ snapshot: "/s", handle: "h", client: "/c", waitMs, spawn: () => ({ status: 0, stdout: `${JSON.stringify(answer)}\n`, stderr: "" }) });

// ---- the fake broker answers as the real one does ----
test("[GB-85] the fake broker's answer carries members, waitedMs and waitTimedOut as the real broker's does: no wait answers at once and is not a timeout; a wait that cannot be satisfied ends at its bound with the state it has and says so", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  fx.setBehavior({ completedHandles: [`${fx.handle}#done`] });
  const plain = ask(fx, "nobody").answer;
  assert.deepEqual(Object.keys(plain).sort(), ["members", "ownership", "schema", "snapshot", "state", "type", "waitTimedOut", "waitedMs"], "every field of the real answer, always");
  assert.deepEqual([plain.state, plain.waitTimedOut, Number.isInteger(plain.waitedMs) && plain.waitedMs >= 0 && plain.waitedMs < 150, plain.members], ["UNKNOWN", false, true, []]);
  const started = Date.now();
  const timed = ask(fx, "nobody", 150).answer;
  assert.deepEqual([timed.state, timed.waitTimedOut, timed.waitedMs >= 150], ["UNKNOWN", true, true], JSON.stringify(timed));
  assert.ok(Date.now() - started < 10_000);
  const reaped = ask(fx, fx.handle, 5000).answer;
  assert.deepEqual([reaped.state, reaped.waitTimedOut, reaped.waitedMs < 1000, reaped.members.map((m) => m.handle)], ["REAPED", false, true, [`${fx.handle}#done`]], "a settled handle answers at once, whatever the wait");
  assert.equal(ask(fx, fx.handle, "x").status, 2, "a wait that is not a number is a usage error, as the real CLI's is");
});

test("[GB-85] the fake broker's wait is the real wait: it answers as soon as the aggregate is REAPED (waitedMs below the bound, not a timeout), and at the bound with ACTIVE and waitTimedOut when it never settles in time", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  fx.setBehavior({ heldHandles: [`${fx.handle}#a`], settleAfterMs: 120 });
  const settled = ask(fx, fx.handle, 5000).answer;
  assert.deepEqual([settled.state, settled.waitTimedOut, settled.waitedMs >= 120 && settled.waitedMs < 5000], ["REAPED", false, true], JSON.stringify(settled));
  assert.deepEqual(settled.members.map((m) => [m.handle, m.state]), [[`${fx.handle}#a`, "REAPED"]]);
  const early = ask(fx, fx.handle, 40).answer;
  assert.deepEqual([early.state, early.waitTimedOut], ["ACTIVE", true], "the wait ended before the lease completed");
  assert.equal(ask(fx, fx.handle).answer.state, "ACTIVE", "no wait: the state it has, not a timeout");
  assert.equal(ask(fx, fx.handle).answer.waitTimedOut, false);
});

// ---- the gate consumes it exactly ----
test("[GB-85] a well-formed answer is read whole: the aggregate state, the member rows, waitedMs and waitTimedOut:false", () => {
  const ok = reading(answerOf({ waitedMs: 37 }), { waitMs: 500 });
  assert.deepEqual([ok.state, ok.waitTimedOut, ok.waitedMs], ["REAPED", false, 37]);
  assert.deepEqual(ok.members, [{ handle: "h#1", state: "REAPED", spawned: true }]);
  const asked = [];
  brokerLeaseState({ snapshot: "/s", handle: "h", client: "/c", waitMs: 99_999, spawn: (c, a) => { asked.push(a); return { status: 0, stdout: JSON.stringify(answerOf()) }; } });
  assert.equal(asked[0].at(-1), "30000", "waitMs is clamped to the broker's 30000");
  const plain = [];
  brokerLeaseState({ snapshot: "/s", handle: "h", client: "/c", spawn: (c, a) => { plain.push(a); return { status: 0, stdout: JSON.stringify(answerOf()) }; } });
  assert.equal(plain[0].includes("--wait-ms"), false, "no wait asked: none sent");
});

test("[GB-85] negative: an answer that contradicts itself or lacks what the real broker always sends is UNKNOWN with the reason, never REAPED and never an ACTIVE the gate would wait on", () => {
  const cases = [
    ["waitTimedOut missing after a wait was asked", answerOf({ waitTimedOut: undefined }), /waitTimedOut/],
    ["waitedMs missing after a wait was asked", answerOf({ waitedMs: undefined }), /waitedMs/],
    ["waitTimedOut not a boolean", answerOf({ waitTimedOut: "false" }), /waitTimedOut/],
    ["waitedMs negative", answerOf({ waitedMs: -1 }), /waitedMs/],
    ["waitedMs a fraction", answerOf({ waitedMs: 1.5 }), /waitedMs/],
    ["REAPED yet waitTimedOut", answerOf({ waitTimedOut: true, waitedMs: 15000 }), /waitTimedOut|contradict/],
    ["not REAPED after a wait without waitTimedOut", answerOf({ state: "ACTIVE", members: [{ handle: "h#1", state: "ACTIVE", spawned: true }] }), /contradict|waitTimedOut/],
    ["REAPED with no member", answerOf({ members: [] }), /no member|REAPED/],
    ["REAPED with a member that is not", answerOf({ members: [{ handle: "h#1", state: "REAPED", spawned: true }, { handle: "h#2", state: "UNKNOWN", spawned: true }] }), /aggregate|REAPED/],
    ["members absent", answerOf({ members: undefined }), /members/],
  ];
  for (const [name, answer, why] of cases) {
    const got = reading(answer, { waitMs: 500 });
    assert.equal(got.state, "UNKNOWN", name);
    assert.match(got.why, why, name);
  }
  // Without a wait the broker still sends the fields; a plain query that lacks them is read for what it is (the aggregate and the members), but a malformed one is refused all the same.
  assert.equal(reading(answerOf({ waitTimedOut: "no" })).state, "UNKNOWN");
  assert.equal(reading(answerOf({ waitedMs: "5" })).state, "UNKNOWN");
});

test("[GB-85] a timeout reports how long the broker waited and keeps the state it had, and never reads as success", () => {
  const got = reading(answerOf({ state: "ACTIVE", waitTimedOut: true, waitedMs: 15000, members: [{ handle: "h#1", state: "ACTIVE", spawned: true }] }), { waitMs: 15_000 });
  assert.deepEqual([got.state, got.waitTimedOut, got.waitedMs], ["UNKNOWN", true, 15000]);
  assert.match(got.why, /after waiting 15000 ms/);
  assert.deepEqual(got.members.map((m) => m.state), ["ACTIVE"], "the member rows travel with the timeout");
});

// ---- release, reclaim and accounting ----
const NOW = Date.now();
const probe = () => ({ platform: "darwin", identity: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW - 60_000, group: 4242 } : { alive: false, startedMs: null, group: null }), bootTime: () => NOW - 86_400_000, groupSnapshot: () => ({ members: [], byPid: new Map([[1, { pid: 1, ppid: 0, startedMs: NOW - 9e7 }]]), own: new Set([process.pid]) }), tokenHolders: () => null, snapshotUsers: () => null, ancestors: () => [] });
const acquire = (file, lease, extra = {}) => acquireBuildOwnership({ file, platform: "darwin", probe: probe(), lease, env: {}, exportEnv: false, stage: "compile", snapshot: "/snap/x", ...extra });

test("[GB-85] a stage's release asks the broker once with its bounded wait, over the real client wire, and releases when the broker settles inside it (the fake's lease completes after the terminal result, as the real one does)", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  const file = path.join(tmp(t, "rk-lease-wire-"), "build-ownership", "owner.json");
  const own = acquire(file, (q) => brokerLeaseState({ ...q, env: {}, platform: process.platform, client: fx.client }), { snapshot: fx.snapshot });
  const sub = `${own.handle}#a`; own.markBrokerSubmit(sub);
  fx.setBehavior({ heldHandles: [sub], settleAfterMs: 100 });
  const released = own.release();
  assert.equal(released.released, true, JSON.stringify(released));
});

test("[GB-23] the release reconciles each recorded sub-handle against the members of the answer: a bare REAPED answer whose own member row for a recorded sub-handle is not REAPED keeps ownership", (t) => {
  const row = (handle, state) => ({ handle, state, spawned: true });
  const owned = (bare) => {
    const own = acquire(path.join(tmp(t, "rk-lease-members-"), "build-ownership", "owner.json"), (q) => (q.handle.includes("#") ? { state: "REAPED", why: "ok", members: [row(q.handle, "REAPED")] } : { state: "REAPED", why: "ok", members: bare(q.handle) }), { snapshot: "/snap/x" });
    own.markBrokerSubmit(`${own.handle}#a`); own.markBrokerSubmit(`${own.handle}#b`);
    return own;
  };
  // The aggregate said REAPED, but the answer's own row for #b is ACTIVE: contradictory, kept.
  const lying = owned((h) => [row(`${h}#a`, "REAPED"), row(`${h}#b`, "ACTIVE")]).release();
  assert.equal(lying.released, false);
  assert.match(String(lying.why), /ACTIVE|member/);
  // Every recorded sub-handle is a REAPED member of the answer: released.
  assert.equal(owned((h) => [row(`${h}#a`, "REAPED"), row(`${h}#b`, "REAPED")]).release().released, true);
});
