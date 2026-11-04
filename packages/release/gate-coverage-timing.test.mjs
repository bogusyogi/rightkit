import { strict as assert } from "node:assert";
import { test } from "node:test";
import { buildCoverageReport } from "./gate-coverage.mjs";

test("coverage projects shared invocation timing and unknown values", () => {
  const timing = { invocationId: "batch-1", elapsedMs: 123.5, cacheLookupMs: 3, executionMs: null };
  const plan = { scans: [{ id: "a", class: "B" }, { id: "b", class: "B" }, { id: "c", class: "B" }] };
  const scanResult = { scans: [{ id: "a", status: "pass", timing, batchAttempts: [{ id: "failed-batch", status: "fail", timing }] }, { id: "b", status: "pass", timing, scanElapsedMs: 200 }] };
  const { scans } = buildCoverageReport({ plan, scanResult });
  assert.deepEqual(scans[0].timing, timing);
  assert.deepEqual(scans[0].batchAttempts[0].timing, timing);
  assert.equal(scans[1].timing.invocationId, scans[0].timing.invocationId);
  assert.equal(scans[1].scanElapsedMs, 200);
  assert.equal(scans[2].timing, null);
  assert.equal(scans[2].scanElapsedMs, null);
});
