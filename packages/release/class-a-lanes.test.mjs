// NN39: the gate's own record of when each Class A row was ready, taken by a lane and ended, and how many lanes its queue had
// (deterministic-scans.mjs runScans `classALanes`). Its way into the coverage report (gate-coverage.mjs `lane`) is held until the contract test accepts that field.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { runScans } from "./deterministic-scans.mjs";

function fixture(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-class-a-lanes-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  fs.writeFileSync(path.join(dir, "package.json"), '{"license":"MIT"}');
  fs.writeFileSync(path.join(dir, "notes.txt"), "original\n");
  const plan = {
    roots: { contentRoot: dir, liveRoot: dir },
    scans: [
      { id: "license", class: "A", command: "internal:license", internal: "license", files: ["package.json"] },
      { id: "secret-scan", class: "A", command: "internal:secret-scan", internal: "secret-scan", dirs: ["."] },
    ],
  };
  return { plan, trackedFiles: ["package.json", "notes.txt"] };
}

test("every Class A row that was answered has a lane record: ready, then taken, then ended, with its queue's lane count", async (t) => {
  const f = fixture(t);
  for (const jobs of [1, 2]) {
    const before = Date.now();
    const result = await runScans(f.plan, { classes: ["A"], jobs, trackedFiles: f.trackedFiles });
    const after = Date.now();
    assert.deepEqual(result.scans.map((s) => s.status), ["pass", "pass"], JSON.stringify(result.scans));
    assert.deepEqual(result.classALanes.map((l) => l.id).sort(), ["license", "secret-scan"]);
    for (const l of result.classALanes) {
      assert.equal(l.lanes, jobs);
      assert.ok(before <= l.readyAt && l.readyAt <= l.startedAt && l.startedAt <= l.endedAt && l.endedAt <= after, JSON.stringify(l));
    }
    // One queue: both rows were ready at the same moment, the moment the queue was opened to its lanes.
    assert.equal(result.classALanes[0].readyAt, result.classALanes[1].readyAt);
  }
});
