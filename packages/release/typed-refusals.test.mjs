// GB-84 style diagnosis of the typed refusals RustRight answers (BROKER-CONTRACT section 16): RK_BROKER_BUSY (GB-52), RK_RECOVERY_PENDING (GB-78), RK_STORAGE_BUDGET_UNAVAILABLE (GB-39),
// RK_ADAPTER_PIN_REQUIRED (GB-51) and RK_OPERATION_DEADLINE (GB-52). Each maps to its own diagnosis, never to a generic receipt failure and never to success. The three that clear on their own
// (busy, recovery pending, storage budget unavailable) are retryable: the gate waits and retries within the call's deadline, each retry a new carrier and sub-handle; the other two are not.
//
// Evidence labels. "data:" titles call the real production functions with constructed inputs (the typed frames the contract names, a recording spawn) and are NOT broker evidence; "real broker:"
// titles run the installed RustRight client against an isolated instance of the installed broker (test-support/real-broker.mjs). No simulated broker exists in this file.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { TYPED_REFUSALS, diagnoseControlReply, operationOutcomeInOutput, refusalInCapturedOutput, typedRefusalOf } from "./broker-wire.mjs";
import { brokerSection, carryBrokerAdapter, carryBrokerCargo } from "./snapshot-broker.mjs";
import os from "node:os";
import { acquireBuildOwnership, brokerLeaseState } from "./build-ownership.mjs";
import { receiptSync } from "./snapshot-broker.mjs";
import { startRealBroker } from "./test-support/real-broker.mjs";

const POSIX = process.platform === "win32" ? "the carried client environment is POSIX only" : false;
const GATE = "/gate/bin/rightkit-gate.mjs";
const CODES = ["RK_BROKER_BUSY", "RK_RECOVERY_PENDING", "RK_STORAGE_BUDGET_UNAVAILABLE", "RK_ADAPTER_PIN_REQUIRED", "RK_OPERATION_DEADLINE"];
const EXPECTED = { RK_BROKER_BUSY: ["broker-busy", true], RK_RECOVERY_PENDING: ["recovery-pending", true], RK_STORAGE_BUDGET_UNAVAILABLE: ["storage-budget-unavailable", true], RK_ADAPTER_PIN_REQUIRED: ["adapter-pin-required", false], RK_OPERATION_DEADLINE: ["operation-deadline", false] };
const MESSAGES = {
  RK_RECOVERY_PENDING: "RK_RECOVERY_PENDING: restart recovery of workspace k1 is still running (waited 120000 ms); it was not admitted, and the request can be retried when recovery ends",
  RK_STORAGE_BUDGET_UNAVAILABLE: "RK_STORAGE_BUDGET_UNAVAILABLE: the storage budget could not be settled within 600000 ms because a maintenance pass is still running; the root was not admitted: retry when it ends",
  RK_ADAPTER_PIN_REQUIRED: "RK_ADAPTER_PIN_REQUIRED: gradle/wrapper/gradle-wrapper.properties carries no distributionSha256Sum",
};
const rejected = (code) => ({ type: "RESULT", exitCode: 125, status: "REJECTED", error: MESSAGES[code] ?? `${code}: refused`, code });

test("data: [GB-84] every typed refusal has its own diagnosis class and says whether it is retryable; the five are distinct from each other and from the older refusals", () => {
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

test("data: [GB-84] a REJECTED RESULT carrying one of them is the typed refusal (kind refused, its class, retryable), never malformed or a generic refusal", () => {
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

test("data: [GB-84] a terminal FAILED reply of a root the broker refused before it ran (the receipt names the code, or the error starts with it) is the typed refusal, not a receipt to validate", () => {
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

test("data: [GB-84] the client's captured output names the refusal; busy, unavailable and the operation deadline are told apart, busy is retryable and the deadline is not", () => {
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
const COMMIT = { sha: "c".repeat(40), tree: "e".repeat(40) };
const WORKSPACES = { "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n', "crates/a/src/lib.rs": "\n" };
const writeTree = (root, files) => { for (const [rel, body] of Object.entries(files)) { const f = path.join(root, rel); fs.mkdirSync(path.dirname(f), { recursive: true }); fs.writeFileSync(f, body); } };
const NOW = Date.now();
const probe = () => ({ platform: "darwin", identity: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW - 60_000, group: 4242 } : { alive: false, startedMs: null, group: null }), bootTime: () => NOW - 86_400_000, groupSnapshot: () => ({ members: [], byPid: new Map([[1, { pid: 1, ppid: 0, startedMs: NOW - 9e7 }]]), own: new Set([process.pid]) }), tokenHolders: () => null, snapshotUsers: () => null, ancestors: () => [{ pid: 5000, startedMs: NOW - 9e6 }] });
const BUSY = "rustright: RK_BROKER_BUSY: RustRight broker busy: every pipe instance of \\\\.\\pipe\\rustright-broker stayed busy for 120s: All pipe instances are busy. (os error 231)\n";

/** A host for carried calls around the REAL build ownership (plain checkout and snapshot directories); with `rb` the client, carrier root and lease reader are the isolated real broker's. */
function host(t, { rb = null } = {}) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-typed-"))); t.after(() => fs.rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 }));
  const checkout = path.join(root, "checkout"); const snapshot = path.join(rb ? path.join(rb.dataRoot, "snapshots") : path.join(root, "data", "snapshots"), "0123456789abcdef");
  writeTree(checkout, WORKSPACES); writeTree(snapshot, WORKSPACES);
  const own = acquireBuildOwnership({ file: path.join(root, "build-ownership", "owner.json"), platform: "darwin", probe: probe(), lease: () => { throw new Error("no lease query expected"); }, env: {}, exportEnv: false, stage: "compile", snapshot });
  const marks = [];
  const sb = {
    controlRoot: rb?.controlRoot ?? path.join(root, "control"), commit: COMMIT, snapshot: { path: snapshot, expectedTree: COMMIT.tree }, declaredInputs: [], handle: own.handle,
    pins: { host: "host-1", keyId: "0".repeat(64), publicKey: "04".padEnd(130, "0"), brokerDigest: "b".repeat(64) },
    markBrokerSubmit: (callHandle) => { marks.push(callHandle); return own.markBrokerSubmit(callHandle); }, markBrokerUnsent: own.markBrokerUnsent,
    checkout, outputRoots: [path.join(rb?.dataRoot ?? path.join(root, "data"), "rightkit-build-control", "ws")], platform: process.platform, client: rb?.clientPath ?? process.execPath,
  };
  const env = { PATH: process.env.PATH, RUSTRIGHT_CALLING_GATE: GATE, ...(rb?.env ?? {}) };
  const cargoCall = (extra = {}) => carryBrokerCargo({ words: ["build", "--locked"], cwd: path.join(snapshot, "crates", "a"), env, section: brokerSection(sb), workspace: snapshot, checkoutWorkspace: checkout, record: () => {}, err: () => {}, stdio: "capture", ...extra });
  const adapterCall = (extra = {}) => carryBrokerAdapter({ adapter: "rightkit-pnpm", words: ["run", "build"], cwd: snapshot, env, section: brokerSection(sb), record: () => {}, err: () => {}, stdio: "capture", ...extra });
  return { root, checkout, snapshot, own, sb, marks, env, cargoCall, adapterCall, owner: () => JSON.parse(fs.readFileSync(own.file, "utf8")) };
}
/** A recording `spawn` that answers the carried client's own invocation from `script(n)` (n counts from 0); the client text it returns is the constructed input of a data test. */
const scripted = (script) => { const calls = []; const spawn = (file, args, options) => { calls.push({ file, args, options }); return script(calls.length - 1); }; spawn.calls = calls; return spawn; };
const refusedReply = (code) => ({ status: 125, stdout: `${JSON.stringify(rejected(code))}\n`, stderr: "" });
const admitted = { status: 0, stdout: "", stderr: "" };
const first = (n, reply) => (i) => (i < n ? reply : admitted);
const clock = () => { const state = { at: 1_000_000, slept: [] }; return { state, now: () => state.at, sleep: (ms) => { state.slept.push(ms); state.at += ms; } }; };

test("data: [GB-78] a carried call refused with RK_RECOVERY_PENDING waits and retries (a new carrier and sub-handle each time) until the client no longer answers the refusal, under one ledger id; the call ends unverified only because no receipt exists here", { skip: POSIX }, (t) => {
  const fx = host(t);
  const spawn = scripted(first(2, refusedReply("RK_RECOVERY_PENDING")));
  const records = []; const c = clock();
  const r = fx.cargoCall({ spawn, record: (e) => records.push(e), now: c.now, sleep: c.sleep, deadlineMs: 600_000 });
  assert.equal(spawn.calls.length, 3, "two refused attempts, then the attempt the client did not refuse");
  assert.equal(c.state.slept.length, 2, "it waited before each retry");
  assert.ok(c.state.slept.every((ms) => ms > 0 && ms <= 5000), JSON.stringify(c.state.slept));
  assert.equal(fx.marks.length, 3, "three attempts, three sub-handles (a handle is spent once)");
  assert.equal(new Set(fx.marks).size, 3);
  assert.equal(new Set(records.map((e) => e.id)).size, 1, "one ledger id: the final record stands for the call");
  assert.doesNotMatch(String(r.outcome.code), /^broker-refused-/, "the last attempt was not a refusal");
  assert.notEqual(records.at(-1).code, "broker-refused-recovery-pending");
});

test("data: [GB-78] negative: a refusal that does not clear within the call's deadline is a typed failure (never verified, never a generic receipt error), after bounded waits that never pass the deadline", { skip: POSIX }, (t) => {
  const fx = host(t);
  const spawn = scripted(() => refusedReply("RK_RECOVERY_PENDING"));
  const c = clock();
  const r = fx.cargoCall({ spawn, now: c.now, sleep: c.sleep, deadlineMs: 20_000 });
  assert.equal(r.outcome.status, "unverified");
  assert.equal(r.outcome.code, "broker-refused-recovery-pending");
  assert.match(r.outcome.message, /RK_RECOVERY_PENDING/); assert.match(r.outcome.message, /retried \d+ time/); assert.match(r.outcome.message, /deadline/);
  assert.notEqual(r.status, 0);
  assert.ok(c.state.at - 1_000_000 <= 20_000, `the waits stay inside the deadline (${c.state.at - 1_000_000} ms)`);
  assert.ok(fx.marks.length >= 2 && fx.marks.length <= 12, `bounded attempts: ${fx.marks.length}`);
});

test("data: [GB-39] RK_STORAGE_BUDGET_UNAVAILABLE is retryable with its own diagnosis; RK_ADAPTER_PIN_REQUIRED is not retried and says what pins it; both end as typed failures, never success", { skip: POSIX }, (t) => {
  const fx = host(t);
  const c = clock();
  fx.cargoCall({ spawn: scripted(first(1, refusedReply("RK_STORAGE_BUDGET_UNAVAILABLE"))), now: c.now, sleep: c.sleep, deadlineMs: 600_000 });
  assert.equal(c.state.slept.length, 1);
  const stuck = fx.cargoCall({ spawn: scripted(() => refusedReply("RK_STORAGE_BUDGET_UNAVAILABLE")), now: c.now, sleep: c.sleep, deadlineMs: 10_000 });
  assert.equal(stuck.outcome.code, "broker-refused-storage-budget-unavailable");
  assert.match(stuck.outcome.message, /storage budget/);
  // A refusal that waiting cannot clear: one attempt, no sleep.
  const before = c.state.slept.length; const marksBefore = fx.marks.length;
  const pin = fx.cargoCall({ spawn: scripted(() => refusedReply("RK_ADAPTER_PIN_REQUIRED")), now: c.now, sleep: c.sleep, deadlineMs: 600_000 });
  assert.equal(pin.outcome.code, "broker-refused-adapter-pin-required");
  assert.match(pin.outcome.message, /distributionSha256Sum|pin/);
  assert.equal(c.state.slept.length, before, "never retried");
  assert.equal(fx.marks.length - marksBefore, 1, "exactly one attempt");
  assert.notEqual(pin.status, 0);
});

test("data: [GB-52] RK_BROKER_BUSY (every pipe instance busy: no byte was sent) is retried within the deadline, the unsent sub-handle is released by the gate's own proof in the REAL ownership record, and a busy broker that stays busy is a typed failure distinct from unavailable", { skip: POSIX }, (t) => {
  const fx = host(t);
  const busy = { status: 1, stdout: "", stderr: BUSY };
  const c = clock();
  fx.cargoCall({ spawn: scripted(first(2, busy)), now: c.now, sleep: c.sleep, deadlineMs: 600_000 });
  assert.equal(c.state.slept.length, 2);
  assert.equal(Object.keys(fx.owner().broker.unsent ?? {}).length, 2, "the two attempts that never connected are recorded unsent");
  const stuck = fx.cargoCall({ spawn: scripted(() => busy), now: c.now, sleep: c.sleep, deadlineMs: 15_000 });
  assert.equal(stuck.outcome.code, "broker-busy");
  assert.match(stuck.outcome.message, /RK_BROKER_BUSY/); assert.doesNotMatch(stuck.outcome.message, /unavailable/i);
  assert.notEqual(stuck.status, 0);
});

test("data: [GB-52] RK_OPERATION_DEADLINE is not retried: the client already cancelled by id, and the job is neither abandoned nor resubmitted", { skip: POSIX }, (t) => {
  const fx = host(t);
  const c = clock();
  const r = fx.cargoCall({ spawn: scripted(() => ({ status: 1, stdout: "operation deadline of 5000 ms passed; request abc-1: cancelConfirmed: true, terminal state: CANCELLED\n", stderr: "" })), now: c.now, sleep: c.sleep, deadlineMs: 600_000 });
  assert.equal(r.outcome.code, "operation-deadline");
  assert.equal(c.state.slept.length, 0);
  assert.equal(fx.marks.length, 1);
});

test("data: [GB-78] an adapter step refused with RK_RECOVERY_PENDING is retried the same way, and RK_ADAPTER_PIN_REQUIRED is not", { skip: POSIX }, (t) => {
  const fx = host(t);
  const c = clock();
  fx.adapterCall({ spawn: scripted(first(1, refusedReply("RK_RECOVERY_PENDING"))), now: c.now, sleep: c.sleep, deadlineMs: 600_000 });
  assert.equal(c.state.slept.length, 1); assert.equal(fx.marks.length, 2);
  const pin = fx.adapterCall({ spawn: scripted(() => refusedReply("RK_ADAPTER_PIN_REQUIRED")), now: c.now, sleep: c.sleep, deadlineMs: 600_000 });
  assert.equal(pin.outcome.code, "broker-refused-adapter-pin-required");
  assert.equal(c.state.slept.length, 1, "no further wait");
});

test("data: [GB-78] the retry never spends more than the deadline and never starts when none is left: with the deadline already passed the first refusal is final", { skip: POSIX }, (t) => {
  const fx = host(t);
  const c = clock();
  const r = fx.cargoCall({ spawn: scripted(() => refusedReply("RK_RECOVERY_PENDING")), now: c.now, sleep: c.sleep, deadlineMs: 1 });
  assert.equal(r.outcome.code, "broker-refused-recovery-pending");
  assert.equal(c.state.slept.length, 0);
  assert.equal(fx.marks.length, 1);
});

// ---- the same answers, from the real broker ----
test("real broker: [GB-78] a carried request the real broker refuses or cannot admit is a settled, spawned:false submission under its sub-handle, never ACTIVE, and the call is never verified", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "typed-refused" });
  t.after(() => rb.stop());
  const fx = host(t, { rb });
  const r = fx.cargoCall({ deadlineMs: 20_000 });
  assert.notEqual(r.outcome.status, "verified", "the plain-directory snapshot is not one the real broker holds");
  assert.notEqual(r.status, 0);
  assert.ok(fx.marks.length >= 1);
  for (const handle of fx.marks) {
    const lease = brokerLeaseState({ snapshot: fx.snapshot, handle, env: rb.env, platform: process.platform, client: rb.clientPath });
    assert.notEqual(lease.state, "ACTIVE", `${handle}: a refused request holds nothing: ${JSON.stringify(lease)}`);
    for (const m of lease.members ?? []) assert.deepEqual([m.state, m.spawned], ["REAPED", false], `${handle}: a refused submission ran nothing: ${JSON.stringify(m)}`);
  }
});

test("real broker: [GB-84] a status request for a job the real broker never had is a typed non-terminal answer, never a receipt", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "typed-status" });
  t.after(() => rb.stop());
  const id = "00000000-0000-4000-8000-000000000000";
  assert.throws(() => receiptSync({ client: rb.clientPath, requestId: id, env: rb.env }), (e) => typeof e.code === "string" && e.code !== "" && !/verified/.test(e.code), "the real broker has no receipt for this id");
});

test("real broker: [GB-52] a broker that is not running is RK_BROKER_UNAVAILABLE, told apart from busy and never retried as a wait", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "typed-down" });
  const fx = host(t, { rb });
  const rootOfInstance = path.dirname(rb.controlRoot);
  await rb.stop();
  t.after(() => fs.rmSync(rootOfInstance, { recursive: true, force: true }));
  const c = clock();
  const r = fx.cargoCall({ now: c.now, sleep: c.sleep, deadlineMs: 600_000 });
  assert.notEqual(r.outcome.status, "verified");
  assert.notEqual(r.outcome.code, "broker-busy", "no broker is not a busy broker");
  assert.equal(c.state.slept.length, 0, "an outage is not a wait that clears on its own");
  assert.equal(fx.marks.length, 1, "exactly one attempt");
});
