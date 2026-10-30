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
