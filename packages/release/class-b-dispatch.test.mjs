// GC-A27 NN09: compatible compiles are combined, and independent groups go to the broker when they are ready. Found by acceptance case 1 on gate 0.6.63
// (2026-11-24, rightkit#1514): with two rows of a workspace in flight, and a combined command counting as several rows, the last of 30 commands was submitted
// three minutes after the first; and a crate listed `solo` in ci/shards.json (a CI job-sizing list) was compiled apart from its workspace for no build reason.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { scheduleBrokerHostClassB } from "./deterministic-scans.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const scans = Array.from({ length: 6 }, (_, i) => ({ id: `row-${i}`, cwd: "rk" }));
const lanes = { compile: [0, 1, 2, 3], doc: [4], tests: [5] };
/** Runs the scheduler with rows that end only when released; returns how many had started before the first one ended. */
async function startedBeforeFirstEnd(pipeline) {
  const started = []; const release = [];
  const done = scheduleBrokerHostClassB({ scans, lanes, carried: () => true, localJobs: 3, pipeline, run: (i) => { started.push(i); return new Promise((resolve) => release.push(resolve)); } });
  await new Promise((resolve) => setImmediate(resolve));
  const count = started.length;
  while (started.length < scans.length || release.length) { release.shift()?.(); await new Promise((resolve) => setImmediate(resolve)); }
  await done;
  return { count, order: started };
}

test("on a broker host one row of a workspace is in flight at a time, in plan order (they share one output folder and Cargo's lock)", async () => {
  const one = await startedBeforeFirstEnd(1);
  assert.equal(one.count, 1); assert.deepEqual(one.order, [0, 1, 2, 3, 4, 5]);
});
test("every workspace has its own chain, started at the same time", async () => {
  const started = []; const release = [];
  const ws = ["a", "b", "c"].flatMap((w) => [{ id: `${w}1`, cwd: w }, { id: `${w}2`, cwd: w }]);
  const done = scheduleBrokerHostClassB({ scans: ws, lanes: { compile: [0, 1, 2, 3, 4, 5] }, carried: () => true, localJobs: 3, pipeline: 1, run: (i) => { started.push(ws[i].id); return new Promise((r) => release.push(r)); } });
  await new Promise((r) => setImmediate(r));
  assert.deepEqual(started.sort(), ["a1", "b1", "c1"]);
  while (release.length || started.length < 6) { release.shift()?.(); await new Promise((r) => setImmediate(r)); }
  await done;
});
test("the gate keeps one command per workspace in flight, and ci/shards.json solo is no reason to split a compile", () => {
  const gate = fs.readFileSync(path.join(here, "bin", "rightkit-gate.mjs"), "utf8"); const scansSrc = fs.readFileSync(path.join(here, "deterministic-scans.mjs"), "utf8");
  assert.match(gate, /classBPipeline: 1,/);
  assert.doesNotMatch(scansSrc, /why = "ci\/shards\.json solo"/); assert.doesNotMatch(scansSrc, /clippyBatchPlan\(clippyEntries, \{ solo: shardSolo/);
  assert.doesNotMatch(gate, /solo\.includes\(unit\) \|\| sidecarBoundCrate/);
});
