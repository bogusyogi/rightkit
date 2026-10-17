import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

import { decideSupersede, median, olderRuns, parseSettings, runDurationMs } from "./supersede.mjs";

const NOW = Date.parse("2026-10-17T12:00:00Z");
const MIN = 60_000;
const startedAgo = (ms) => new Date(NOW - ms).toISOString();

test("median handles odd, even, empty and unusable values", () => {
  assert.equal(median([3, 1, 2]), 2);
  assert.equal(median([4, 1, 2, 3]), 2.5);
  assert.equal(median([]), null);
  assert.equal(median([Number.NaN, -1]), null);
});

test("runDurationMs reads start and update times", () => {
  assert.equal(runDurationMs({ run_started_at: "2026-10-17T10:00:00Z", updated_at: "2026-10-17T10:10:00Z" }), 10 * MIN);
  assert.equal(runDurationMs({ run_started_at: "2026-10-17T10:10:00Z", updated_at: "2026-10-17T10:00:00Z" }), null);
  assert.equal(runDurationMs({}), null);
});

test("a run that has only just started is cancelled", () => {
  const decision = decideSupersede({ run: { status: "in_progress", run_started_at: startedAgo(2 * MIN) }, medianMs: 20 * MIN, nowMs: NOW });
  assert.equal(decision.cancel, true);
});

test("a run past the grace fraction of the median is left alone", () => {
  const decision = decideSupersede({ run: { status: "in_progress", run_started_at: startedAgo(15 * MIN) }, medianMs: 20 * MIN, nowMs: NOW });
  assert.equal(decision.cancel, false);
});

test("the grace boundary is exact: 0.7 of 20 minutes is 14 minutes", () => {
  const at = (ms) => decideSupersede({ run: { status: "in_progress", run_started_at: startedAgo(ms) }, medianMs: 20 * MIN, nowMs: NOW }).cancel;
  assert.equal(at(14 * MIN - 1), true);
  assert.equal(at(14 * MIN), false);
});

test("graceFraction changes the boundary", () => {
  const run = { status: "in_progress", run_started_at: startedAgo(10 * MIN) };
  assert.equal(decideSupersede({ run, medianMs: 20 * MIN, nowMs: NOW, graceFraction: 0.7 }).cancel, true);
  assert.equal(decideSupersede({ run, medianMs: 20 * MIN, nowMs: NOW, graceFraction: 0.4 }).cancel, false);
});

test("a run with a failed job is cancelled even when nearly done", () => {
  const run = { status: "in_progress", run_started_at: startedAgo(19 * MIN) };
  for (const conclusion of ["failure", "timed_out"]) {
    assert.equal(decideSupersede({ run, jobs: [{ conclusion: "success" }, { conclusion }], medianMs: 20 * MIN, nowMs: NOW }).cancel, true, conclusion);
  }
  assert.equal(decideSupersede({ run, jobs: [{ conclusion: "success" }, { conclusion: null }], medianMs: 20 * MIN, nowMs: NOW }).cancel, false);
});

test("a queued run that never started is cancelled", () => {
  assert.equal(decideSupersede({ run: { status: "queued", created_at: startedAgo(MIN) }, medianMs: null, nowMs: NOW }).cancel, true);
});

test("without a median nothing is cancelled for being young", () => {
  const run = { status: "in_progress", run_started_at: startedAgo(MIN) };
  assert.equal(decideSupersede({ run, medianMs: null, nowMs: NOW }).cancel, false);
  assert.equal(decideSupersede({ run, medianMs: 0, nowMs: NOW }).cancel, false);
});

test("olderRuns keeps only lower run numbers and never the current run", () => {
  const current = { id: 30, run_number: 12 };
  const runs = [{ id: 31, run_number: 13 }, { id: 30, run_number: 12 }, { id: 29, run_number: 11 }, { id: 27, run_number: 9 }];
  assert.deepEqual(olderRuns(runs, current).map((run) => run.id), [27, 29]);
});

test("parseSettings defaults and validates", () => {
  assert.deepEqual(parseSettings({}), { graceFraction: 0.7, medianWindow: 10 });
  assert.deepEqual(parseSettings({ RIGHT_GIT_GRACE_FRACTION: "0.5", RIGHT_GIT_MEDIAN_WINDOW: "5" }), { graceFraction: 0.5, medianWindow: 5 });
  assert.throws(() => parseSettings({ RIGHT_GIT_GRACE_FRACTION: "0" }));
  assert.throws(() => parseSettings({ RIGHT_GIT_GRACE_FRACTION: "1.5" }));
  assert.throws(() => parseSettings({ RIGHT_GIT_MEDIAN_WINDOW: "0" }));
  assert.throws(() => parseSettings({ RIGHT_GIT_MEDIAN_WINDOW: "2.5" }));
});

test("the script is safe to embed verbatim in a workflow heredoc", () => {
  const source = fs.readFileSync(new URL("./supersede.mjs", import.meta.url), "utf8");
  assert.doesNotMatch(source, /\$\{/, "no template placeholder or expression syntax");
  assert.doesNotMatch(source, /^RIGHT_GIT_SUPERSEDE_EOF$/m);
  assert.doesNotMatch(source, /\r/);
});
