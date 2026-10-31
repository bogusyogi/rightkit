// GB-84 style diagnosis of the typed refusals RustRight answers (BROKER-CONTRACT section 16): RK_BROKER_BUSY (GB-52), RK_RECOVERY_PENDING (GB-78), RK_STORAGE_BUDGET_UNAVAILABLE (GB-39),
// RK_ADAPTER_PIN_REQUIRED (GB-51) and RK_OPERATION_DEADLINE (GB-52). Each maps to its own diagnosis, never to a generic receipt failure and never to success. The three that clear on their own
// (busy, recovery pending, storage budget unavailable) are retryable: the gate waits and retries within the call's deadline, each retry a new carrier and sub-handle; the other two are not.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { TYPED_REFUSALS, diagnoseControlReply, operationOutcomeInOutput, refusalInCapturedOutput, typedRefusalOf } from "./broker-wire.mjs";
import { brokerSection, carryBrokerAdapter, carryBrokerCargo } from "./snapshot-broker.mjs";
import { spawnSync } from "./background-process.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

const POSIX = process.platform === "win32" ? "the fixture client is POSIX only" : false;
const GATE = "/gate/bin/rightkit-gate.mjs";
const CODES = ["RK_BROKER_BUSY", "RK_RECOVERY_PENDING", "RK_STORAGE_BUDGET_UNAVAILABLE", "RK_ADAPTER_PIN_REQUIRED", "RK_OPERATION_DEADLINE"];
const EXPECTED = { RK_BROKER_BUSY: ["broker-busy", true], RK_RECOVERY_PENDING: ["recovery-pending", true], RK_STORAGE_BUDGET_UNAVAILABLE: ["storage-budget-unavailable", true], RK_ADAPTER_PIN_REQUIRED: ["adapter-pin-required", false], RK_OPERATION_DEADLINE: ["operation-deadline", false] };
const MESSAGES = {
  RK_RECOVERY_PENDING: "RK_RECOVERY_PENDING: restart recovery of workspace k1 is still running (waited 120000 ms); it was not admitted, and the request can be retried when recovery ends",
  RK_STORAGE_BUDGET_UNAVAILABLE: "RK_STORAGE_BUDGET_UNAVAILABLE: the storage budget could not be settled within 600000 ms because a maintenance pass is still running; the root was not admitted: retry when it ends",
  RK_ADAPTER_PIN_REQUIRED: "RK_ADAPTER_PIN_REQUIRED: gradle/wrapper/gradle-wrapper.properties carries no distributionSha256Sum",
};
const rejected = (code) => ({ type: "RESULT", exitCode: 125, status: "REJECTED", error: MESSAGES[code] ?? `${code}: refused`, code });

test("[GB-84] every typed refusal has its own diagnosis class and says whether it is retryable; the five are distinct from each other and from the older refusals", () => {
  assert.deepEqual(Object.keys(TYPED_REFUSALS).sort(), [...CODES].sort());
  const classes = new Set();
  for (const code of CODES) {
    const got = typedRefusalOf({ code });
    assert.deepEqual([got.refusal, got.retryable], EXPECTED[code], code);
    classes.add(got.refusal);
  }
  assert.equal(classes.size, 5, "five distinct diagnoses");
  for (const old of ["paused", "suspended", "not-found", "activation-pending", "cutover-drain", "other"]) assert.equal(classes.has(old), false, old);
  assert.equal(typedRefusalOf({ code: "RK_SOMETHING_NEW" }), null);
  assert.equal(typedRefusalOf({}), null);
});

test("[GB-84] a REJECTED RESULT carrying one of them is the typed refusal (kind refused, its class, retryable), never malformed or a generic refusal", () => {
  for (const code of ["RK_RECOVERY_PENDING", "RK_STORAGE_BUDGET_UNAVAILABLE", "RK_ADAPTER_PIN_REQUIRED"]) {
    const got = diagnoseControlReply(rejected(code));
    assert.deepEqual([got.kind, got.code, got.refusal, got.retryable], ["refused", code, EXPECTED[code][0], EXPECTED[code][1]], code);
    assert.match(got.message, new RegExp(code));
  }
  // The older vector's classes are unchanged, and a code this client does not know is still `other`, not retryable.
  assert.equal(diagnoseControlReply({ type: "RESULT", exitCode: 125, status: "REJECTED", error: "x", code: "RK_ADMISSION_PAUSED" }).refusal, "paused");
  const unknown = diagnoseControlReply({ type: "RESULT", exitCode: 125, status: "REJECTED", error: "x", code: "RK_SOMETHING_NEW" });
  assert.deepEqual([unknown.refusal, unknown.retryable], ["other", false]);
});

test("[GB-84] a terminal FAILED reply of a root the broker refused before it ran (the receipt names the code, or the error starts with it) is the typed refusal, not a receipt to validate", () => {
  for (const [code, receipt] of [["RK_STORAGE_BUDGET_UNAVAILABLE", { id: "job-1", state: "FAILED", exitCode: 125, code: "RK_STORAGE_BUDGET_UNAVAILABLE", error: MESSAGES.RK_STORAGE_BUDGET_UNAVAILABLE }], ["RK_ADAPTER_PIN_REQUIRED", { id: "job-1", state: "FAILED", exitCode: 125, error: MESSAGES.RK_ADAPTER_PIN_REQUIRED }], ["RK_RECOVERY_PENDING", { id: "job-1", state: "FAILED", exitCode: 125, error: MESSAGES.RK_RECOVERY_PENDING }]]) {
    for (const event of [{ type: "STATUS", id: "job-1", state: "FAILED", receipt }, { type: "RESULT", id: "job-1", exitCode: 125, status: "FAILED", receipt }]) {
      const got = diagnoseControlReply(event, "job-1");
      assert.deepEqual([got.kind, got.code, got.refusal, got.retryable], ["refused", code, EXPECTED[code][0], EXPECTED[code][1]], `${event.type} ${code}`);
    }
  }
  // Negatives: a failed build (exit 101, no code) is still a terminal outcome; a code merely named inside a log line is not a refusal; a SUCCEEDED receipt is never turned into one.
  assert.equal(diagnoseControlReply({ type: "STATUS", id: "job-1", state: "FAILED", receipt: { id: "job-1", state: "FAILED", exitCode: 101 } }, "job-1").kind, "terminal");
  assert.equal(diagnoseControlReply({ type: "STATUS", id: "job-1", state: "FAILED", receipt: { id: "job-1", state: "FAILED", exitCode: 101, error: "cargo said: see RK_RECOVERY_PENDING in the docs" } }, "job-1").kind, "terminal");
  assert.equal(diagnoseControlReply({ type: "STATUS", id: "job-1", state: "SUCCEEDED", receipt: { id: "job-1", state: "SUCCEEDED", exitCode: 0, code: "RK_RECOVERY_PENDING" } }, "job-1").kind, "terminal");
});

test("[GB-84] the client's captured output names the refusal; busy, unavailable and the operation deadline are told apart, busy is retryable and the deadline is not", () => {
  const out = `${JSON.stringify(rejected("RK_RECOVERY_PENDING"))}\n`;
  const refusal = refusalInCapturedOutput(out);
  assert.deepEqual([refusal.code, refusal.refusal, refusal.retryable], ["RK_RECOVERY_PENDING", "recovery-pending", true]);
  assert.deepEqual([operationOutcomeInOutput("RK_BROKER_BUSY: every pipe instance stayed busy").code, operationOutcomeInOutput("RK_BROKER_BUSY: x").retryable], ["broker-busy", true]);
  assert.equal(operationOutcomeInOutput("RK_BROKER_UNAVAILABLE: no broker").code, "broker-unavailable");
  assert.equal(operationOutcomeInOutput("RK_BROKER_UNAVAILABLE: no broker").retryable, false);
  const deadline = operationOutcomeInOutput("operation deadline of 5000 ms passed; request abc-1: cancelConfirmed: true, terminal state: CANCELLED");
  assert.deepEqual([deadline.code, deadline.retryable, deadline.cancelConfirmed], ["operation-deadline", false, true]);
  assert.equal(operationOutcomeInOutput("RK_OPERATION_DEADLINE").retryable, false);
});

// ---- the carried call ----
const env = (fx) => ({ PATH: process.env.PATH, RUSTRIGHT_CALLING_GATE: GATE, ...fx.ownershipEnv });
const clock = () => { const state = { at: 1_000_000, slept: [] }; return { state, now: () => state.at, sleep: (ms) => { state.slept.push(ms); state.at += ms; } }; };
const cargoCall = (fx, extra = {}) => carryBrokerCargo({ words: ["build", "--locked"], cwd: path.join(fx.snapshot, "crates", "a"), env: env(fx), section: brokerSection(fx.sb), workspace: fx.snapshot, checkoutWorkspace: fx.checkout, record: () => {}, err: () => {}, stdio: "capture", ...extra });
const leaseMembers = (fx, handle) => JSON.parse(spawnSync(fx.client, ["snapshot-lease", "status", "--snapshot", fx.snapshot, "--ownership", handle], { encoding: "utf8" }).stdout).members;

test("[GB-78] a carried call refused with RK_RECOVERY_PENDING waits and retries (a new carrier and sub-handle each time) until the broker admits it, and ends verified with one ledger id; each refused attempt is a settled, spawned:false submission", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  fx.setBehavior({ refuseWith: rejected("RK_RECOVERY_PENDING"), refuseTimes: 2 });
  const records = []; const c = clock();
  const r = cargoCall(fx, { record: (e) => records.push(e), now: c.now, sleep: c.sleep, deadlineMs: 600_000 });
  assert.equal(r.outcome.status, "verified", JSON.stringify(r.outcome));
  assert.equal(r.status, 0);
  assert.equal(c.state.slept.length, 2, "it waited before each retry");
  assert.ok(c.state.slept.every((ms) => ms > 0 && ms <= 5000), JSON.stringify(c.state.slept));
  assert.equal(fx.submits.length, 3, "three attempts, three sub-handles (a handle is spent once)");
  assert.equal(new Set(fx.submits).size, 3);
  assert.equal(new Set(records.map((e) => e.id)).size, 1, "one ledger id: the final record stands for the call");
  assert.equal(records.at(-1).status, "verified");
  const members = leaseMembers(fx, fx.handle);
  assert.deepEqual(members.filter((m) => m.refused === "RK_RECOVERY_PENDING").map((m) => [m.state, m.spawned]), [["REAPED", false], ["REAPED", false]], "the refused attempts are settled and ran nothing");
  assert.equal(fx.calls().length, 1, "only the admitted attempt reached the broker");
});

test("[GB-78] negative: a refusal that does not clear within the call's deadline is a typed failure (never verified, never a generic receipt error), after bounded waits that never pass the deadline", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  fx.setBehavior({ refuseWith: rejected("RK_RECOVERY_PENDING") });
  const c = clock();
  const r = cargoCall(fx, { now: c.now, sleep: c.sleep, deadlineMs: 20_000 });
  assert.equal(r.outcome.status, "unverified");
  assert.equal(r.outcome.code, "broker-refused-recovery-pending");
  assert.match(r.outcome.message, /RK_RECOVERY_PENDING/); assert.match(r.outcome.message, /retried \d+ time/); assert.match(r.outcome.message, /deadline/);
  assert.notEqual(r.status, 0);
  assert.ok(c.state.at - 1_000_000 <= 20_000, `the waits stay inside the deadline (${c.state.at - 1_000_000} ms)`);
  assert.ok(fx.submits.length >= 2 && fx.submits.length <= 12, `bounded attempts: ${fx.submits.length}`);
  assert.equal(fx.calls().length, 0, "nothing was ever admitted");
});

test("[GB-39] RK_STORAGE_BUDGET_UNAVAILABLE is retryable with its own diagnosis; RK_ADAPTER_PIN_REQUIRED is not retried and says what pins it; both end as typed failures, never success", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  fx.setBehavior({ refuseWith: rejected("RK_STORAGE_BUDGET_UNAVAILABLE"), refuseTimes: 1 });
  const c = clock();
  const ok = cargoCall(fx, { now: c.now, sleep: c.sleep, deadlineMs: 600_000 });
  assert.equal(ok.outcome.status, "verified"); assert.equal(c.state.slept.length, 1);
  fx.setBehavior({ refuseWith: rejected("RK_STORAGE_BUDGET_UNAVAILABLE") });
  const stuck = cargoCall(fx, { now: c.now, sleep: c.sleep, deadlineMs: 10_000 });
  assert.equal(stuck.outcome.code, "broker-refused-storage-budget-unavailable");
  assert.match(stuck.outcome.message, /storage budget/);
  // A refusal that waiting cannot clear: one attempt, no sleep.
  const before = c.state.slept.length; const submitsBefore = fx.submits.length;
  fx.setBehavior({ refuseWith: rejected("RK_ADAPTER_PIN_REQUIRED") });
  const pin = cargoCall(fx, { now: c.now, sleep: c.sleep, deadlineMs: 600_000 });
  assert.equal(pin.outcome.code, "broker-refused-adapter-pin-required");
  assert.match(pin.outcome.message, /distributionSha256Sum|pin/);
  assert.equal(c.state.slept.length, before, "never retried");
  assert.equal(fx.submits.length - submitsBefore, 1, "exactly one attempt");
  assert.notEqual(pin.status, 0);
});

test("[GB-52] RK_BROKER_BUSY (every pipe instance busy: no byte was sent) is retried within the deadline, the unsent sub-handle is released by the gate's own proof, and a busy broker that stays busy is a typed failure distinct from unavailable", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  fx.setBehavior({ busy: true, busyTimes: 2 });
  const c = clock();
  const r = cargoCall(fx, { now: c.now, sleep: c.sleep, deadlineMs: 600_000 });
  assert.equal(r.outcome.status, "verified", JSON.stringify(r.outcome));
  assert.equal(c.state.slept.length, 2);
  const marked = fx.owner().broker;
  assert.equal(Object.keys(marked.unsent ?? {}).length, 2, "the two attempts that never connected are recorded unsent");
  fx.setBehavior({ busy: true });
  const stuck = cargoCall(fx, { now: c.now, sleep: c.sleep, deadlineMs: 15_000 });
  assert.equal(stuck.outcome.code, "broker-busy");
  assert.match(stuck.outcome.message, /RK_BROKER_BUSY/); assert.doesNotMatch(stuck.outcome.message, /unavailable/i);
  assert.notEqual(stuck.status, 0);
});

test("[GB-52] RK_OPERATION_DEADLINE is not retried: the client already cancelled by id, and the job is neither abandoned nor resubmitted", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  fx.setBehavior({ stdout: "operation deadline of 5000 ms passed; request abc-1: cancelConfirmed: true, terminal state: CANCELLED\n", exit: 1 });
  const c = clock();
  const r = cargoCall(fx, { now: c.now, sleep: c.sleep, deadlineMs: 600_000 });
  assert.equal(r.outcome.code, "operation-deadline");
  assert.equal(c.state.slept.length, 0);
  assert.equal(fx.submits.length, 1);
});

test("[GB-78] an adapter step refused with RK_RECOVERY_PENDING is retried the same way, and RK_ADAPTER_PIN_REQUIRED is not", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  const call = (extra = {}) => carryBrokerAdapter({ adapter: "rightkit-pnpm", words: ["run", "build"], cwd: fx.snapshot, env: env(fx), section: brokerSection(fx.sb), record: () => {}, err: () => {}, stdio: "capture", ...extra });
  fx.setBehavior({ refuseWith: rejected("RK_RECOVERY_PENDING"), refuseTimes: 1 });
  const c = clock();
  const ok = call({ now: c.now, sleep: c.sleep, deadlineMs: 600_000 });
  assert.equal(ok.outcome.status, "verified", JSON.stringify(ok.outcome));
  assert.equal(c.state.slept.length, 1); assert.equal(fx.submits.length, 2);
  fx.setBehavior({ refuseWith: rejected("RK_ADAPTER_PIN_REQUIRED") });
  const pin = call({ now: c.now, sleep: c.sleep, deadlineMs: 600_000 });
  assert.equal(pin.outcome.code, "broker-refused-adapter-pin-required");
  assert.equal(c.state.slept.length, 1, "no further wait");
});

test("[GB-78] the retry never spends more than the deadline and never starts when none is left: with the deadline already passed the first refusal is final", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  fx.setBehavior({ refuseWith: rejected("RK_RECOVERY_PENDING") });
  const c = clock();
  const r = cargoCall(fx, { now: c.now, sleep: c.sleep, deadlineMs: 1 });
  assert.equal(r.outcome.code, "broker-refused-recovery-pending");
  assert.equal(c.state.slept.length, 0);
  assert.equal(fx.submits.length, 1);
  assert.equal(fs.existsSync(fx.client), true);
});
