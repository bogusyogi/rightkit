import assert from "node:assert/strict";
import test from "node:test";

import { cancelAndReapManagedRequest, firstManagedRequestId, latestManagedRequestId, releaseProgressLimits, terminalResultFromOutput } from "./progress-control.mjs";

test("release progress stays bounded without killing a healthy long native build at 90 minutes", () => {
  assert.deepEqual(releaseProgressLimits({}), { inactivityMs: 600_000, absoluteMs: 14_400_000 });
  assert.deepEqual(releaseProgressLimits({ RIGHT_RELEASE_STALL_MS: "1000", RIGHT_RELEASE_ABSOLUTE_MS: "5000" }), { inactivityMs: 1_000, absoluteMs: 5_000 });
  assert.throws(() => releaseProgressLimits({ RIGHT_RELEASE_STALL_MS: "1000", RIGHT_RELEASE_ABSOLUTE_MS: "1000" }), /must exceed/);
  assert.throws(() => releaseProgressLimits({ RIGHT_RELEASE_STALL_MS: "nope" }), /positive finite/);
});

test("declared inner liveness owner always exits before the outer watchdog", () => {
  const ownerTimeoutMs = 45 * 60 * 1000;
  assert.deepEqual(
    releaseProgressLimits({}, { livenessOwnerTimeoutMs: ownerTimeoutMs }),
    { inactivityMs: 50 * 60 * 1000, absoluteMs: 4 * 60 * 60 * 1000 },
  );
  assert.deepEqual(
    releaseProgressLimits({ RIGHT_RELEASE_STALL_MS: String(60 * 60 * 1000) }, { livenessOwnerTimeoutMs: ownerTimeoutMs }),
    { inactivityMs: 60 * 60 * 1000, absoluteMs: 4 * 60 * 60 * 1000 },
  );
  assert.throws(
    () => releaseProgressLimits({ RIGHT_RELEASE_ABSOLUTE_MS: String(49 * 60 * 1000) }, { livenessOwnerTimeoutMs: ownerTimeoutMs }),
    /must exceed effective release inactivity limit/,
  );
  assert.throws(() => releaseProgressLimits({}, { livenessOwnerTimeoutMs: 0 }), /positive finite/);
});

test("latest managed request id follows streamed root Cargo requests", () => {
  const first = "11111111-1111-4111-8111-111111111111";
  const second = "22222222-2222-4222-8222-222222222222";
  assert.equal(latestManagedRequestId(`rightkit: request ${first}\nrightkit managed-agent: request ${second}`), second);
  assert.equal(firstManagedRequestId(`rightkit: request ${first}\nrightkit managed-agent: request ${second}`), first);
  assert.equal(latestManagedRequestId("ordinary output", first), first);
});

test("managed cancellation issues CANCEL, waits for terminal RESULT, & returns reap receipt", async () => {
  const id = "33333333-3333-4333-8333-333333333333";
  const calls = [];
  const run = async (args) => {
    calls.push(args);
    if (args[0] === "cancel") return { code: 0, output: JSON.stringify({ type: "CANCEL_REQUESTED", id }) };
    return { code: 130, output: `compiler tail\n${JSON.stringify({ type: "RESULT", id, status: "CANCELLED", exitCode: 130, receipt: { reap: { reaped: true } } })}\n` };
  };
  assert.deepEqual(await cancelAndReapManagedRequest(id, { run }), {
    status: "CANCELLED", id, exitCode: 130, receipt: { reap: { reaped: true } }, cancelExitCode: 0, attachExitCode: 130,
  });
  assert.deepEqual(calls, [["cancel", id], ["attach", id]]);
});

test("missing terminal broker result fails closed", async () => {
  const run = async () => ({ code: 125, output: "broker closed", timedOut: false });
  await assert.rejects(cancelAndReapManagedRequest("44444444-4444-4444-8444-444444444444", { run }), /lacked terminal RESULT/);
  assert.equal(terminalResultFromOutput("noise\n"), null);
});
