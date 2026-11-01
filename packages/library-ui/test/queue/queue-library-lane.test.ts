import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { JobInfo } from "../../src/queue/contract";
import { loadedRowsNotice } from "../../src/queue/lib/historyPage";
import { addJobsFailure } from "../../src/queue/lib/errorMessage";
import { countQueueChips, countWords, isRunningStage, showsOutputChip } from "../../src/queue/lib/queueView";

function job(overrides: Partial<JobInfo>): JobInfo {
  return {
    id: "j",
    kind: "youtube",
    stage: "queued",
    pct: 0,
    input: "https://example.com/x",
    title: null,
    needs_auth: null,
    media: null,
    ...overrides,
  } as JobInfo;
}

describe("queue chip counts (#224)", () => {
  it("counts running, ready, failed and needs-auth in one pass", () => {
    const counts = countQueueChips([
      job({ id: "a", stage: "queued" }),
      job({ id: "b", stage: "downloading" as JobInfo["stage"] }),
      job({ id: "c", stage: "done" }),
      job({ id: "d", stage: "error" }),
      job({ id: "e", stage: "cancelled" }),
      job({ id: "f", stage: "queued", needs_auth: "youtube" } as Partial<JobInfo>),
    ]);
    assert.equal(counts.running, 1);
    assert.equal(counts.ready, 1);
    assert.equal(counts.failed, 1);
    assert.equal(counts.needs, 1);
  });

  it("counts categories and kinds without filtering the list", () => {
    const counts = countQueueChips([
      job({ kind: "youtube" }),
      job({ kind: "youtube" }),
      job({ kind: "local_file" }),
    ]);
    assert.equal(counts.kinds.youtube, 2);
    assert.equal(counts.kinds.local_file, 1);
    assert.equal(counts.categories.media, 2);
    assert.equal(counts.categories.document, 1);
  });

  it("returns zeroed counts for an empty queue", () => {
    const counts = countQueueChips([]);
    assert.equal(counts.running + counts.ready + counts.failed + counts.needs, 0);
    assert.deepEqual(counts.kinds, {});
  });

  it("isRunningStage excludes queued, done, error and cancelled", () => {
    assert.equal(isRunningStage({ stage: "queued" } as JobInfo), false);
    assert.equal(isRunningStage({ stage: "done" } as JobInfo), false);
    assert.equal(isRunningStage({ stage: "error" } as JobInfo), false);
    assert.equal(isRunningStage({ stage: "cancelled" } as JobInfo), false);
    assert.equal(isRunningStage({ stage: "downloading" as JobInfo["stage"] }), true);
  });
});

describe("output chip (#227)", () => {
  it("hides the default transcribe chip", () => {
    assert.equal(showsOutputChip({ media: null }), false);
    assert.equal(showsOutputChip({ media: { action: "transcribe" } as JobInfo["media"] }), false);
  });

  it("shows the chip for non-default outputs", () => {
    assert.equal(showsOutputChip({ media: { action: "both" } as JobInfo["media"] }), true);
  });
});

describe("word count (#224)", () => {
  it("counts words and tolerates empty input", () => {
    assert.equal(countWords("  one two\n three "), 3);
    assert.equal(countWords(""), 0);
    assert.equal(countWords(null), 0);
  });
});

describe("library loaded-rows notice (#228)", () => {
  it("is absent when every page is loaded", () => {
    assert.equal(loadedRowsNotice(40, false), null);
  });

  it("states the cap when more pages exist", () => {
    const notice = loadedRowsNotice(100, true);
    assert.ok(notice?.includes("100 newest"));
  });
});

describe("add_jobs failure split (#95)", () => {
  it("returns a whole-batch string with no accepted jobs", () => {
    const failure = addJobsFailure("[down] engine is down");
    assert.deepEqual(failure.accepted, []);
    assert.equal(failure.message, "[down] engine is down");
  });

  it("keeps accepted jobs from a partial success", () => {
    const accepted = [{ id: "a" }] as unknown as Parameters<typeof addJobsFailure>[0];
    const failure = addJobsFailure({ accepted: [accepted], error: "[timeout] rpc timed out" });
    assert.equal(failure.accepted.length, 1);
    assert.equal(failure.message, "[timeout] rpc timed out");
  });
}); 
