import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { addJobsFailure } from "../../src/queue/lib/errorMessage";
import { alreadyQueuedNotice, duplicatesOf } from "../../src/queue/lib/alreadyQueued";

const dup = (input: string) => ({ input, existing_job_id: `job-${input.length}` });

describe("already queued notice (#109)", () => {
  it("reads duplicates from an add_jobs rejection", () => {
    const rejection = { accepted: [], error: "", duplicates: [dup("https://example.com/a")] };
    assert.deepEqual(duplicatesOf(rejection), [dup("https://example.com/a")]);
  });

  it("returns no duplicates for a whole-batch string rejection", () => {
    assert.deepEqual(duplicatesOf("[down] engine is down"), []);
    assert.deepEqual(duplicatesOf(null), []);
  });

  it("names a single duplicate input", () => {
    assert.equal(
      alreadyQueuedNotice([dup("https://example.com/a")]),
      "Already queued: https://example.com/a",
    );
  });

  it("counts several duplicates", () => {
    assert.equal(
      alreadyQueuedNotice([dup("https://example.com/a"), dup("https://example.com/b")]),
      "Already queued: 2 links",
    );
  });

  it("is null when nothing was a duplicate", () => {
    assert.equal(alreadyQueuedNotice([]), null);
  });

  it("duplicate-only rejection has no error message, so it is not a failure", () => {
    const rejection = { accepted: [], error: "", duplicates: [dup("https://example.com/a")] };
    const failed = addJobsFailure(rejection);
    assert.equal(failed.message, "");
    assert.equal(alreadyQueuedNotice(duplicatesOf(rejection)) !== null, true);
  });

  it("mixed rejection keeps accepted jobs and a real error alongside the notice", () => {
    const rejection = {
      accepted: [{ id: "j1" }],
      error: "[down] engine is down",
      duplicates: [dup("https://example.com/a")],
    };
    const failed = addJobsFailure(rejection);
    assert.equal(failed.accepted.length, 1);
    assert.equal(failed.message, "[down] engine is down");
    assert.notEqual(alreadyQueuedNotice(duplicatesOf(rejection)), null);
  });
});
