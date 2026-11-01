import assert from "node:assert/strict";
import { test } from "node:test";
import type { Stage } from "../../src/queue/contract";
import { isActiveStage } from "../../src/queue/lib/jobDisplay";
import { isRunningStage } from "../../src/queue/lib/queueView";

test("isActiveStage is true only for in-flight stages", () => {
  const active: Stage[] = ["detecting", "downloading", "fetched", "transcribing", "writing"];
  const inactive: Stage[] = ["queued", "done", "error", "cancelled"];
  for (const stage of active) assert.equal(isActiveStage(stage), true, stage);
  for (const stage of inactive) assert.equal(isActiveStage(stage), false, stage);
});

test("isActiveStage with includeQueued also counts queued jobs as in flight", () => {
  assert.equal(isActiveStage("queued", { includeQueued: true }), true);
  assert.equal(isActiveStage("done", { includeQueued: true }), false);
  assert.equal(isActiveStage("error", { includeQueued: true }), false);
  assert.equal(isActiveStage("cancelled", { includeQueued: true }), false);
});

test("isRunningStage agrees with isActiveStage for every stage", () => {
  const stages: Stage[] = ["queued", "detecting", "downloading", "fetched", "transcribing", "writing", "done", "error", "cancelled"];
  for (const stage of stages) {
    assert.equal(isRunningStage({ stage }), isActiveStage(stage), stage);
  }
});
