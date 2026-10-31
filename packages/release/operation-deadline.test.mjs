// GB-52 (BROKER-CONTRACT section 16): the carried call has an end-to-end deadline and explicit cancellation. The gate runs the RustRight client with RUSTRIGHT_OPERATION_DEADLINE_MS (the call's own
// budget, never a user-set cap replaced) and RUSTRIGHT_CANCEL_WAIT_MS; past the deadline the client cancels the job BY ID and polls its terminal state, and the gate then asserts what the broker
// says about every submission under the call's sub-handle (SNAPSHOT_LEASE_STATUS members, GB-23/GB-85): REAPED means settled, anything else keeps ownership held. The job is never abandoned and
// never resubmitted.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { brokerSection, carryBrokerAdapter, carryBrokerCargo } from "./snapshot-broker.mjs";
import { STAGE_DEADLINE_AT_ENV } from "./broker-wire.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

const POSIX = process.platform === "win32" ? "the fixture client is POSIX only" : false;
const GATE = "/gate/bin/rightkit-gate.mjs";
const DEADLINE_OUT = "operation deadline of 5000 ms passed; request abc-1: cancelConfirmed: true, terminal state: CANCELLED\n";
const env = (fx, extra = {}) => ({ PATH: process.env.PATH, RUSTRIGHT_CALLING_GATE: GATE, ...fx.ownershipEnv, ...extra });
const cargo = (fx, extra = {}, callEnv = env(fx)) => carryBrokerCargo({ words: ["check"], cwd: path.join(fx.snapshot, "crates", "a"), env: callEnv, section: brokerSection(fx.sb), workspace: fx.snapshot, checkoutWorkspace: fx.checkout, record: () => {}, err: () => {}, stdio: "capture", ...extra });

test("[GB-52] the carried client runs with the call's deadline and the cancel wait: the remaining stage budget becomes the operation deadline, a user-set cap or cancel wait is never replaced, no budget sets none", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  const seen = [];
  const spawn = (file, args, options) => { seen.push(options); return { status: 0, stdout: "", stderr: "" }; };
  const call = (callEnv, extra = {}) => cargo(fx, { spawn, ...extra }, callEnv);
  call(env(fx, { [STAGE_DEADLINE_AT_ENV]: String(Date.now() + 90_000) }));
  const fromBudget = Number(seen.at(-1).env.RUSTRIGHT_OPERATION_DEADLINE_MS);
  assert.ok(fromBudget > 80_000 && fromBudget <= 90_000, `the remaining stage budget: ${fromBudget}`);
  assert.equal(seen.at(-1).timeout, fromBudget + 30_000 + 15_000, "the outer guard follows the client's own cancel wait");
  call(env(fx), { deadlineMs: 12_000 });
  assert.equal(seen.at(-1).env.RUSTRIGHT_OPERATION_DEADLINE_MS, "12000", "the call's own deadline");
  call(env(fx, { RUSTRIGHT_OPERATION_DEADLINE_MS: "777", RUSTRIGHT_CANCEL_WAIT_MS: "4000" }), { deadlineMs: 12_000 });
  assert.deepEqual([seen.at(-1).env.RUSTRIGHT_OPERATION_DEADLINE_MS, seen.at(-1).env.RUSTRIGHT_CANCEL_WAIT_MS], ["777", "4000"], "a user-set cap and cancel wait stand");
  assert.equal(seen.at(-1).timeout, 777 + 4000 + 15_000);
  call(env(fx));
  assert.equal(Object.hasOwn(seen.at(-1).env, "RUSTRIGHT_OPERATION_DEADLINE_MS"), false, "no budget: no deadline is invented");
  assert.equal(Object.hasOwn(seen.at(-1), "timeout"), false);
});

test("[GB-52] after the client's deadline and cancel the gate asks the broker for the sub-handle's members and reports them: REAPED once the job settled, held while it is still ACTIVE; the job is never resubmitted", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  const handleOf = () => fx.submits.at(-1);
  // The broker settles the cancelled job while the gate waits: REAPED.
  fx.setBehavior({ stdout: DEADLINE_OUT, exit: 1 });
  const submitsBefore = fx.submits.length;
  const first = cargo(fx, { deadlineMs: 5000 });
  assert.equal(first.outcome.code, "operation-deadline");
  assert.equal(fx.submits.length, submitsBefore + 1, "never resubmitted");
  // A call that completed at the fake broker is a REAPED member (the fixture job finishes inside the call): the cancel is confirmed and the sub-handle settled.
  assert.equal(first.outcome.subHandle.handle, handleOf());
  assert.equal(first.outcome.subHandle.state, "REAPED");
  assert.equal(first.outcome.reaped, true);
  assert.match(first.outcome.message, /after the cancel the broker reports sub-handle .* REAPED/);
  // The broker still holds the cancelled job: after the (bounded) wait it is still ACTIVE, so the broker's answer is a timeout: ownership stays held, said so, never released.
  fx.setBehavior({ stdout: DEADLINE_OUT, exit: 1 });
  const held = cargo(fx, { deadlineMs: 5000, spawn: (file, args, options) => { const r = fx.realSpawn(file, args, options); fx.setBehavior({ stdout: DEADLINE_OUT, exit: 1, heldHandles: [fx.submits.at(-1)] }); return r; } }, env(fx, { RUSTRIGHT_CANCEL_WAIT_MS: "300" }));
  assert.equal(held.outcome.code, "operation-deadline");
  assert.equal(held.outcome.reaped, false);
  assert.deepEqual([held.outcome.subHandle.state, held.outcome.subHandle.waitTimedOut], ["UNKNOWN", true], "a wait that timed out keeps ownership UNKNOWN");
  assert.deepEqual(held.outcome.subHandle.members.map((m) => m.state), ["ACTIVE"], "the member rows travel with the answer");
  assert.match(held.outcome.message, /ACTIVE after waiting \d+ ms.*ownership stays held until it is REAPED/s);
  assert.notEqual(held.status, 0);
});

test("[GB-52] the same for a carried adapter step, and a broker that cannot be asked leaves ownership held (UNKNOWN), never released", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  fx.setBehavior({ stdout: DEADLINE_OUT, exit: 1 });
  const adapter = (extra = {}) => carryBrokerAdapter({ adapter: "rightkit-pnpm", words: ["run", "build"], cwd: fx.snapshot, env: env(fx), section: brokerSection(fx.sb), record: () => {}, err: () => {}, stdio: "capture", deadlineMs: 5000, ...extra });
  const settled = adapter();
  assert.equal(settled.outcome.code, "operation-deadline");
  assert.deepEqual([settled.outcome.reaped, settled.outcome.subHandle.state], [true, "REAPED"]);
  // No lease reader (a gate without the broker's lease status): nothing is asserted, and the outcome says ownership stays UNKNOWN.
  const bare = brokerSection({ ...fx.sb, leaseStatus: undefined });
  const unasked = carryBrokerAdapter({ adapter: "rightkit-pnpm", words: ["run", "build"], cwd: fx.snapshot, env: env(fx), section: bare, record: () => {}, err: () => {}, stdio: "capture", deadlineMs: 5000 });
  assert.equal(unasked.outcome.code, "operation-deadline");
  assert.equal(unasked.outcome.reaped, false);
  assert.match(unasked.outcome.message, /ownership stays UNKNOWN/);
  // A lease reader that throws or answers UNKNOWN: held.
  const broken = brokerSection({ ...fx.sb, leaseStatus: () => { throw new Error("broker unreachable"); } });
  const failed = carryBrokerAdapter({ adapter: "rightkit-pnpm", words: ["run", "build"], cwd: fx.snapshot, env: env(fx), section: broken, record: () => {}, err: () => {}, stdio: "capture", deadlineMs: 5000 });
  assert.equal(failed.outcome.reaped, false);
  assert.match(failed.outcome.message, /broker unreachable|ownership stays UNKNOWN/);
  assert.equal(fs.existsSync(fx.client), true);
});
