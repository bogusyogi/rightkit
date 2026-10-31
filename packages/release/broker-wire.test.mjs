// GB-53: wire bounds in one unit (1 MiB, newline included) and the gate's handling of FRAME_OVER_CAP and RK_RESPONSE_OVER_CAP (BROKER-CONTRACT section 14).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { FRAME_OVER_CAP, MAX_BROKER_LINE_BYTES, MAX_REQUEST_BYTES, REQUEST_ENVELOPE_RESERVE_BYTES, drainFrames, lineFits, overCapProblem, requestOverCap } from "./broker-wire.mjs";
import { brokerSection, carryBrokerAdapter, carryBrokerCargo, receiptSync } from "./snapshot-broker.mjs";
import { writeSnapshotRequest } from "./snapshot-request.mjs";
import { makeFakeBroker, subHandleStub } from "./test-support/fake-broker.mjs";

test("[GB-53] both limits are 1 MiB with the newline counted: the longest body is 1 MiB - 1 (exact limit minus one, at, plus one)", () => {
  assert.equal(MAX_REQUEST_BYTES, 1048576); assert.equal(MAX_BROKER_LINE_BYTES, 1048576);
  assert.equal(lineFits(MAX_BROKER_LINE_BYTES - 2), true);
  assert.equal(lineFits(MAX_BROKER_LINE_BYTES - 1), true, "body + newline == limit");
  assert.equal(lineFits(MAX_BROKER_LINE_BYTES), false, "a 1 MiB body plus its newline is one byte over");
  assert.equal(lineFits(MAX_BROKER_LINE_BYTES + 1), false);
});

test("[GB-53] a request that cannot fit one request line is refused before the carrier is written (no job can exist); declared inputs plus the envelope are the unit", (t) => {
  const fx = makeFakeBroker(t);
  const make = (inputs) => writeSnapshotRequest({ controlRoot: fx.controlRoot, commit: fx.sb.commit, snapshot: fx.sb.snapshot, declaredInputs: inputs, handle: fx.handle, primaryCwd: fx.checkout, argv: ["check"], nonce: "a".repeat(64) });
  const dir = path.join(fx.controlRoot, "snapshot-requests");
  const before = () => (fs.existsSync(dir) ? fs.readdirSync(dir).length : 0);
  const room = MAX_REQUEST_BYTES - 1 - REQUEST_ENVELOPE_RESERVE_BYTES;
  // One input of about `n` bytes: (JSON overhead 2 quotes in the array).
  const sized = (n) => ["x".repeat(n)];
  const base = Buffer.byteLength(JSON.stringify({ inputs: [""] })) + Buffer.byteLength(JSON.stringify({ commit: fx.sb.commit, snapshot: fx.sb.snapshot, ownership: { handle: `${fx.handle}#0123456789abcdef0123456789abcdef0123` }, primaryCwd: fx.checkout, argv: ["check"] }));
  assert.doesNotThrow(() => make(sized(room - base - 200)));
  const files = before();
  assert.throws(() => make(sized(room + 10)), (e) => e.code === "RK_REQUEST_OVER_CAP" && /request line/.test(e.message));
  assert.equal(before(), files, "nothing was written");
  assert.equal(requestOverCap(room), null);
  assert.match(requestOverCap(room + 1), /newline counts/);
});

test("[GB-53] a client that reads frames drains every complete frame before it bounds the incomplete one: a limit-sized frame followed by the next frame's start is valid", () => {
  const limit = 64;
  const frame = (n) => `{"type":"X","p":"${"a".repeat(n - 19)}"}`; // body of exactly n bytes
  assert.equal(Buffer.byteLength(frame(63)), 63);
  const ok = drainFrames(`${frame(63)}\n{"type":"Y"`, limit);
  assert.equal(ok.error, null, "63 + newline == 64: valid even though the next frame already started in the same read");
  assert.equal(ok.frames.length, 2);
  assert.match(drainFrames(`${frame(64)}\n`, limit).error, /exceeds the 64-byte limit/, "a 64-byte body plus its newline is over");
  assert.match(drainFrames(`{"type":"X","p":"${"a".repeat(80)}`, limit).error, /incomplete frame/, "an incomplete frame already at the limit cannot fit it");
  assert.equal(drainFrames("a tool's own very long output line ".repeat(10), limit).error, null, "cargo's output is not a wire frame");
  assert.equal(drainFrames(`${frame(20)}\n${frame(20)}\n${frame(20)}\n`, limit).frames.length, 3, "several frames in one read");
});

test("[GB-53] FRAME_OVER_CAP and RK_RESPONSE_OVER_CAP are typed truncation: a status read, and captured output of a carried call, refuse", (t) => {
  assert.equal(overCapProblem({ type: "STATUS", id: "x", state: "RUNNING" }), null);
  assert.equal(overCapProblem({ type: FRAME_OVER_CAP, for: "STATUS", bytes: 2000000, limit: 1048576 }).code, "frame-over-cap");
  const result = { type: "RESULT", id: "x", exitCode: 1, status: "FAILED", code: "RK_RESPONSE_OVER_CAP", originalStatus: "SUCCEEDED", bytes: 2000000, limit: 1048576 };
  assert.match(overCapProblem(result).message, /never a success/);
  // receiptSync: the status reply is an over-cap outcome.
  for (const [doc, code] of [[{ type: FRAME_OVER_CAP, for: "STATUS", bytes: 2e6, limit: 1048576 }, "frame-over-cap"], [result, "response-over-cap"]]) {
    const spawn = () => ({ status: 0, stdout: JSON.stringify(doc), stderr: "" });
    assert.throws(() => receiptSync({ client: "rustright", requestId: "x", env: {}, spawn }), (e) => e.code === code, code);
  }
  // A carried call whose captured output lost a frame is not verified, even though its receipt verified.
  const fx = makeFakeBroker(t, { readOnlySnapshots: true });
  const section = brokerSection({ ...fx.sb, markBrokerSubmit: subHandleStub(fx.handle) });
  const env = { PATH: fx.root, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs" };
  const run = () => { const records = []; return { records, r: carryBrokerCargo({ words: ["check"], cwd: path.join(fx.snapshot, "crates", "a"), env, section, workspace: fx.snapshot, checkoutWorkspace: fx.checkout, record: (e) => records.push(e), err: () => {}, stdio: "capture" }) }; };
  fx.setBehavior({});
  { const first = run().r; assert.equal(first.outcome.status, "verified", JSON.stringify(first.outcome)); }
  fx.setBehavior({ stdout: `${JSON.stringify({ type: FRAME_OVER_CAP, for: "JOB_EVENT", bytes: 2000000, limit: 1048576 })}\n` });
  const lost = run();
  assert.equal(lost.r.outcome.status, "unverified");
  assert.equal(lost.r.outcome.code, "frame-over-cap");
  assert.equal(lost.records.at(-1).status, "unverified", "the ledger records the truncation, not a verified call");
  // The same for a carried adapter call (the fixture client prints the same frame through its job output).
  const adapterRun = () => carryBrokerAdapter({ adapter: "rightkit-pnpm", words: ["run", "build"], cwd: fx.snapshot, env, section, record: () => {}, err: () => {}, stdio: "capture" });
  const lostAdapter = adapterRun();
  assert.equal(lostAdapter.outcome.status, "unverified"); assert.equal(lostAdapter.outcome.code, "frame-over-cap");
  fx.setBehavior({});
  assert.equal(adapterRun().outcome.status, "verified");
});

// ---- GB-84: control and status replies are diagnosed, not collapsed into a generic receipt failure ----
import { diagnoseControlReply } from "./broker-wire.mjs";
import { loadVector } from "./test-support/vectors.mjs";

test("[GB-84] every case of the control-status vector reaches RustRight's diagnosis kind, state, code and refusal class", () => {
  const vector = loadVector("control-status-vectors.v1.json");
  assert.ok(vector.cases.length >= 19);
  for (const c of vector.cases) {
    const got = diagnoseControlReply(c.event, vector.expectedId);
    assert.equal(got.kind, c.kind, c.name);
    if (c.state !== undefined) assert.equal(got.state, c.state, c.name);
    if (c.code !== undefined) assert.equal(got.code, c.code, c.name);
    if (c.refusal !== undefined) assert.equal(got.refusal, c.refusal, c.name);
  }
});

test("[GB-84] receiptSync gives each diagnosis its own recoverable error: live, unavailable, refused (paused, suspended), malformed; only a terminal reply yields a receipt", () => {
  const read = (doc, exit = 0) => receiptSync({ client: "rustright", requestId: "job-1", env: {}, spawn: () => ({ status: exit, stdout: JSON.stringify(doc), stderr: "" }) });
  const code = (doc, exit) => { try { read(doc, exit); return "no error"; } catch (e) { return [e.code, e.diagnosis?.kind, e.message]; } };
  assert.deepEqual(read({ type: "STATUS", id: "job-1", state: "SUCCEEDED", receipt: { id: "job-1", state: "SUCCEEDED" } }), { id: "job-1", state: "SUCCEEDED" });
  for (const live of [{ type: "STATUS", id: "job-1", state: "RUNNING" }, { type: "STATUS", id: "job-1", state: "QUEUED", receipt: null }]) {
    const [c, kind, message] = code(live);
    assert.deepEqual([c, kind], ["receipt-not-ready", "live"]); assert.match(message, /request id job-1 is the durable handle/);
  }
  const [uc, uk, um] = code({ type: "STATUS", id: "job-1", state: "UNAVAILABLE", diagnostic: { code: "EIO" } });
  assert.deepEqual([uc, uk], ["broker-unavailable", "unavailable"]); assert.match(um, /EIO/);
  const paused = code({ type: "RESULT", exitCode: 125, status: "REJECTED", error: "admission is paused", code: "RK_ADMISSION_PAUSED" }, 125);
  assert.deepEqual(paused.slice(0, 2), ["broker-refused-paused", "refused"]); assert.match(paused[2], /RK_ADMISSION_PAUSED.*nothing was spawned/);
  const suspended = code({ type: "RESULT", exitCode: 125, status: "REJECTED", error: "journaled", code: "RK_JOB_SUSPENDED" }, 125);
  assert.deepEqual(suspended.slice(0, 2), ["broker-refused-suspended", "refused"]); assert.match(suspended[2], /ask again then, by this id/);
  assert.equal(code({ type: "RESULT", exitCode: 125, status: "REJECTED", code: "RK_JOB_NOT_FOUND" }, 125)[0], "broker-refused-not-found");
  // Malformed: a terminal state with no receipt, another request's receipt or id, no state: never green.
  for (const bad of [{ type: "STATUS", id: "job-1", state: "SUCCEEDED" }, { type: "STATUS", id: "job-1", state: "SUCCEEDED", receipt: { id: "job-2" } }, { type: "STATUS", id: "job-2", state: "RUNNING" }, { type: "STATUS", id: "job-1" }, { type: "STATUS", id: "job-1", state: "SUCCEEDED", receipt: "ok" }]) {
    assert.deepEqual(code(bad).slice(0, 2), ["receipt", "malformed"], JSON.stringify(bad));
  }
  // A terminal reply with a failing client exit is not a build outcome either.
  assert.equal(code({ type: "STATUS", id: "job-1", state: "SUCCEEDED", receipt: { id: "job-1" } }, 1)[0], "receipt");
});

// ---- GB-54: pause and resume ----
import { ADMISSION_EVENT, PAUSE_ADMISSION, RESUME_ADMISSION, admissionRequest, parseAdmissionEvent, parseAdmissionRequest, refusalInCapturedOutput } from "./broker-wire.mjs";

test("[GB-54] pause and resume requests and the ADMISSION reply round-trip through the typed shapes; a reason or holder of the wrong shape is refused, unknown optional fields are ignored", () => {
  const runtime = { version: "x", brokerDigest: "b".repeat(64) };
  const pause = admissionRequest(PAUSE_ADMISSION, { protocol: "rustright.broker/1", runtime, reason: "install", holder: { pid: 4242, startTimeMs: 1700000000000 } });
  assert.deepEqual(pause, { type: "PAUSE_ADMISSION", protocol: "rustright.broker/1", runtime, reason: "install", holder: { pid: 4242, startTimeMs: 1700000000000 } });
  assert.deepEqual(parseAdmissionRequest(JSON.parse(JSON.stringify(pause))), { ok: true, request: pause }, "a captured request round-trips");
  assert.deepEqual(admissionRequest(PAUSE_ADMISSION, { protocol: "p", runtime }), { type: "PAUSE_ADMISSION", protocol: "p", runtime }, "reason and holder are optional");
  assert.deepEqual(admissionRequest(RESUME_ADMISSION, { protocol: "p", runtime, reason: "ignored" }), { type: "RESUME_ADMISSION", protocol: "p", runtime }, "resume carries the envelope only");
  assert.deepEqual(parseAdmissionRequest({ ...pause, future: 1 }).request, pause, "unknown optional fields are ignored (additive compatibility)");
  for (const bad of [{ reason: 7 }, { holder: "me" }, { holder: { pid: -1 } }, { holder: { pid: 2 ** 33 } }, { holder: { pid: 1, startTimeMs: -5 } }, { holder: { startTimeMs: 1 } }]) {
    assert.equal(parseAdmissionRequest({ ...pause, ...bad }).ok, false, JSON.stringify(bad));
    assert.throws(() => admissionRequest(PAUSE_ADMISSION, { protocol: "p", runtime, ...bad }), (e) => e.code === "RK_ADMISSION_REQUEST_INVALID");
  }
  assert.equal(parseAdmissionRequest({ type: "PROMOTE", protocol: "p", runtime }).ok, false);
  assert.deepEqual(parseAdmissionEvent({ type: ADMISSION_EVENT, paused: true, reason: "install", holder: { pid: 1 }, persistError: "EIO" }), { ok: true, paused: true, reason: "install", holder: { pid: 1 }, persistError: "EIO" });
  assert.deepEqual(parseAdmissionEvent({ type: "ADMISSION", paused: false }), { ok: true, paused: false });
  for (const bad of [{ type: "ADMISSION" }, { type: "ADMISSION", paused: "yes" }, { type: "ADMISSION", paused: true, holder: { pid: "x" } }, { type: "STATUS", paused: true }]) assert.equal(parseAdmissionEvent(bad).ok, false, JSON.stringify(bad));
});

test("[GB-54] a paused carried submit is a typed no-spawn refusal, not a missing request reference", (t) => {
  const paused = { type: "RESULT", exitCode: 125, status: "REJECTED", error: "admission is paused; resume with 'rustright resume-admission'", code: "RK_ADMISSION_PAUSED" };
  assert.deepEqual(refusalInCapturedOutput(`cargo noise\n${JSON.stringify(paused)}\n`), { code: "RK_ADMISSION_PAUSED", refusal: "paused", retryable: false, message: paused.error });
  assert.equal(refusalInCapturedOutput("plain output\n"), null);
  const fx = makeFakeBroker(t, { readOnlySnapshots: true });
  const section = brokerSection({ ...fx.sb, markBrokerSubmit: subHandleStub(fx.handle) });
  const env = { PATH: fx.root, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs" };
  fx.setBehavior({ refuseWith: paused });
  const records = [];
  const r = carryBrokerCargo({ words: ["check"], cwd: path.join(fx.snapshot, "crates", "a"), env, section, workspace: fx.snapshot, checkoutWorkspace: fx.checkout, record: (e) => records.push(e), err: () => {}, stdio: "capture" });
  assert.equal(r.outcome.status, "unverified");
  assert.equal(r.outcome.code, "broker-refused-paused");
  assert.match(r.outcome.message, /admission is paused and nothing was spawned/);
  assert.equal(records.at(-1).code, "broker-refused-paused");
});

test("[GB-66] a compile the broker refuses at admission with RK_TOOLCHAIN_IDENTITY_UNAVAILABLE is a typed toolchain-unavailable outcome, nothing ran", (t) => {
  const fx = makeFakeBroker(t, { readOnlySnapshots: true });
  const section = brokerSection({ ...fx.sb, markBrokerSubmit: subHandleStub(fx.handle) });
  const env = { PATH: fx.root, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs" };
  fx.setBehavior({ refuseWith: { type: "RESULT", exitCode: 125, status: "REJECTED", error: "no registered cargo", code: "RK_TOOLCHAIN_IDENTITY_UNAVAILABLE" } });
  const r = carryBrokerCargo({ words: ["check"], cwd: path.join(fx.snapshot, "crates", "a"), env, section, workspace: fx.snapshot, checkoutWorkspace: fx.checkout, record: () => {}, err: () => {}, stdio: "capture" });
  assert.equal(r.outcome.code, "toolchain-unavailable");
  assert.match(r.outcome.message, /RK_TOOLCHAIN_IDENTITY_UNAVAILABLE.*nothing ran/);
});

// ---- GB-82: the fixture broker's lease status is the production table's (vector lease-status-vectors.v1.json) ----
import { spawnSync as spawnClient } from "./background-process.mjs";

test("[GB-82] the fixture broker answers snapshot-lease status as the production lease table does for every step of the lease-status vector", (t) => {
  const vector = loadVector("lease-status-vectors.v1.json");
  for (const c of vector.cases) {
    const fx = makeFakeBroker(t, { readOnlySnapshots: true });
    const behavior = { heldHandles: [], completedHandles: [], unknownHandles: [], refusedHandles: {} };
    const apply = () => fx.setBehavior({ ...behavior });
    const query = (handle) => { const r = spawnClient(fx.client, ["snapshot-lease", "status", "--snapshot", fx.snapshot, "--ownership", handle], { encoding: "utf8" }); assert.equal(r.status, 0, r.stderr); return JSON.parse(r.stdout); };
    let last = null;
    for (const step of c.steps) {
      if (step.op === "acquire") { behavior.heldHandles.push(step.handle); last = step.handle; apply(); }
      else if (step.op === "complete") { behavior.completedHandles.push(...behavior.heldHandles); behavior.heldHandles = []; apply(); }
      else if (step.op === "unknown-start") { behavior.unknownHandles.push(step.handle); apply(); }
      else if (step.op === "refuse") { behavior.refusedHandles[step.handle] = step.code ?? "RK_CHECK_ERROR"; apply(); }
      else if (step.op === "restart") { /* the completion is a file: it outlives the process */ }
      else if (step.op === "query") {
        const answer = query(step.handle);
        assert.equal(answer.state, step.state, `${c.name}: ${step.handle}`);
        assert.equal(answer.type, "SNAPSHOT_LEASE_STATUS"); assert.equal(answer.ownership, step.handle); assert.equal(answer.snapshot, fx.snapshot);
        if (step.members) assert.deepEqual(answer.members.map((m) => ({ handle: m.handle, state: m.state, ...(m.spawned === false ? { spawned: false } : {}), ...(m.refused ? { refused: m.refused } : {}) })), step.members.map((m) => ({ handle: m.handle, state: m.state, ...(m.spawned === false ? { spawned: false } : {}), ...(m.refused ? { refused: m.refused } : {}) })), `${c.name}: members of ${step.handle}`);
      } else assert.fail(`unknown step ${step.op}`);
    }
    void last;
  }
});

test("[GB-82] a carried call's completion survives in the fixture broker (a durable completed handle) and a never-held handle stays UNKNOWN, not REAPED", (t) => {
  const fx = makeFakeBroker(t, { readOnlySnapshots: true });
  const section = brokerSection({ ...fx.sb, markBrokerSubmit: subHandleStub(fx.handle) });
  const env = { PATH: fx.root, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs" };
  const status = (handle) => JSON.parse(spawnClient(fx.client, ["snapshot-lease", "status", "--snapshot", fx.snapshot, "--ownership", handle], { encoding: "utf8" }).stdout);
  assert.equal(status(fx.handle).state, "UNKNOWN", "nothing was ever held under this handle");
  const r = carryBrokerCargo({ words: ["check"], cwd: path.join(fx.snapshot, "crates", "a"), env, section, workspace: fx.snapshot, checkoutWorkspace: fx.checkout, record: () => {}, err: () => {}, stdio: "capture" });
  assert.equal(r.outcome.status, "verified");
  const after = status(fx.handle);
  assert.equal(after.state, "REAPED"); assert.equal(after.members.length, 1); assert.match(after.members[0].handle, new RegExp(`^${fx.handle}#`));
  assert.equal(status(fx.handle.slice(0, 3)).state, "UNKNOWN", "a prefix of a handle covers nothing");
});

// ---- GB-52: operation deadline, cancel and a vanished gate ----
import { CANCEL_WAIT_ENV, OPERATION_DEADLINE_ENV, STAGE_DEADLINE_AT_ENV, clientGuardMs, operationDeadlineEnv, operationOutcomeInOutput } from "./broker-wire.mjs";
import { scanOperationEnv } from "./deterministic-scans.mjs";

test("[GB-52] the operation deadline is the call's own budget: a user-set cap is never replaced, none means none, a stage's remaining budget is passed", () => {
  assert.deepEqual(operationDeadlineEnv({ env: {} }), {}, "no budget, no deadline: no universal duration is invented");
  assert.deepEqual(operationDeadlineEnv({ env: {}, deadlineMs: 90_000 }), { RUSTRIGHT_OPERATION_DEADLINE_MS: "90000" });
  assert.deepEqual(operationDeadlineEnv({ env: { [OPERATION_DEADLINE_ENV]: "5000" }, deadlineMs: 90_000 }), {}, "a user-set cap wins");
  assert.deepEqual(operationDeadlineEnv({ env: { [STAGE_DEADLINE_AT_ENV]: "1700000060000" }, now: 1_700_000_000_000 }), { RUSTRIGHT_OPERATION_DEADLINE_MS: "60000" }, "the remaining stage budget");
  assert.equal(operationDeadlineEnv({ env: { [STAGE_DEADLINE_AT_ENV]: "1700000000000" }, now: 1_700_000_900_000 }).RUSTRIGHT_OPERATION_DEADLINE_MS, "1", "an expired budget still asks for the shortest deadline: the job is cancelled, not run");
  assert.equal(clientGuardMs({}), null);
  assert.equal(clientGuardMs({ [OPERATION_DEADLINE_ENV]: "1000" }), 1000 + 30_000 + 15_000, "deadline + the client's default cancel wait + margin");
  assert.equal(clientGuardMs({ [OPERATION_DEADLINE_ENV]: "1000", [CANCEL_WAIT_ENV]: "2000" }), 1000 + 2000 + 15_000);
  assert.deepEqual(scanOperationEnv({}, 600_000), { RUSTRIGHT_OPERATION_DEADLINE_MS: "580000", RUSTRIGHT_CANCEL_WAIT_MS: "15000" });
  assert.deepEqual(scanOperationEnv({ [OPERATION_DEADLINE_ENV]: "9" }, 600_000), {}, "the user's cap wins over a scan's");
});

test("[GB-52] the client's own words are read: deadline with cancel confirmation and terminal state, busy apart from unavailable", () => {
  assert.deepEqual(operationOutcomeInOutput("rustright: operation deadline of 1500 ms passed; request 5e3f-aa: cancelConfirmed: true, terminal state: CANCELLED\n"), { code: "operation-deadline", retryable: false, deadlineMs: 1500, requestId: "5e3f-aa", cancelConfirmed: true, terminalState: "CANCELLED" });
  assert.equal(operationOutcomeInOutput("operation deadline of 9 ms passed; request x1: cancelConfirmed: false, terminal state: not observed (the broker was unreachable)").terminalState, null);
  assert.equal(operationOutcomeInOutput("RK_BROKER_BUSY: every pipe instance is busy").code, "broker-busy");
  assert.equal(operationOutcomeInOutput("RK_BROKER_UNAVAILABLE: connection refused").code, "broker-unavailable");
  assert.equal(operationOutcomeInOutput("RK_OPERATION_DEADLINE").code, "operation-deadline");
  assert.equal(operationOutcomeInOutput("all good"), null);
});

test("[GB-52] a carried call whose client ran out its deadline, was stopped by the outer guard, or met a busy broker is not a build outcome and keeps its sub-handle marked (ownership UNKNOWN)", (t) => {
  const fx = makeFakeBroker(t, { readOnlySnapshots: true });
  const marks = [];
  const section = brokerSection({ ...fx.sb, markBrokerSubmit: subHandleStub(fx.handle, (h) => marks.push(h)) });
  const env = { PATH: fx.root, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs" };
  const seen = [];
  const run = (spawn, extra = {}) => carryBrokerCargo({ words: ["check"], cwd: path.join(fx.snapshot, "crates", "a"), env, section, workspace: fx.snapshot, checkoutWorkspace: fx.checkout, record: () => {}, err: () => {}, stdio: "capture", spawn: (file, args, options) => { seen.push(options); return spawn(file, args, options); }, ...extra });
  const deadline = run(() => ({ status: 125, stdout: "", stderr: "rustright: operation deadline of 1500 ms passed; request abc-1: cancelConfirmed: false, terminal state: not observed\n" }), { deadlineMs: 1500 });
  assert.equal(deadline.outcome.code, "operation-deadline"); assert.match(deadline.outcome.message, /cancelConfirmed: false, terminal state: not observed.*ownership stays UNKNOWN/);
  assert.equal(seen[0].env.RUSTRIGHT_OPERATION_DEADLINE_MS, "1500"); assert.equal(seen[0].timeout, 1500 + 30_000 + 15_000, "the outer guard follows the client's own cancel wait");
  const guarded = run(() => ({ error: Object.assign(new Error("spawnSync ETIMEDOUT"), { code: "ETIMEDOUT" }), status: null, stdout: "", stderr: "" }), { deadlineMs: 1000 });
  assert.equal(guarded.outcome.code, "operation-deadline"); assert.match(guarded.outcome.message, /outer guard/);
  // A busy broker is a wait the gate retries within the call's deadline (typed-refusals.test.mjs); with the deadline already spent the first answer is final and keeps its own code.
  assert.equal(run(() => ({ status: 125, stdout: "", stderr: "RK_BROKER_BUSY: busy\n" }), { deadlineMs: 1 }).outcome.code, "broker-busy");
  assert.equal(run(() => ({ status: 125, stdout: "", stderr: "RK_BROKER_UNAVAILABLE: down\n" })).outcome.code, "broker-unavailable");
  assert.equal(marks.length, 4, "every sub-handle stays recorded: none is cleared by a timeout, a cancel or an outage (reconciled by the request id)");
  // Without a budget nothing is invented: no deadline variable, no guard.
  const plain = run(() => ({ status: 0, stdout: "", stderr: "" }));
  assert.equal(plain.outcome.status !== undefined, true);
  assert.equal(Object.hasOwn(seen.at(-1).env, "RUSTRIGHT_OPERATION_DEADLINE_MS"), false); assert.equal(Object.hasOwn(seen.at(-1), "timeout"), false);
});
