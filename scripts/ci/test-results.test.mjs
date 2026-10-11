import assert from "node:assert/strict";
import test from "node:test";
import { judgeUnit, ResultScanner, scanChunks, stripAnsi, TEST_COMMAND } from "./test-results.mjs";

const libtest = (passed, failed = 0, ignored = 0) => `test result: ${failed ? "FAILED" : "ok"}. ${passed} passed; ${failed} failed; ${ignored} ignored; 0 measured; 0 filtered out; finished in 0.01s\n`;

test("libtest summaries are summed per binary, ignored included", () => {
  const counts = scanChunks([`running 3 tests\n${libtest(3)}\nrunning 0 tests\n${libtest(0)}running 2 tests\n${libtest(1, 0, 1)}`]);
  assert.deepEqual(counts, { executed: 4, failed: 0, ignored: 1, skipped: 0, binaries: 3 });
  assert.equal(scanChunks([libtest(2, 1)]).failed, 1);
});

test("a SKIP marker counts with or without a prefix, and when split across chunks", () => {
  assert.equal(scanChunks(["SKIP e2e: no model\n"]).skipped, 1);
  assert.equal(scanChunks(["  SKIP e2e\n"]).skipped, 1);
  assert.equal(scanChunks(["[browser] SKIP Chrome for Testing close test: missing\n"]).skipped, 1);
  assert.equal(scanChunks(["asr: SKIP whisper\n"]).skipped, 1);
  assert.equal(scanChunks(["[brow", "ser] SK", "IP Chrome for Testing\nnext line\n"]).skipped, 1, "one marker split over three chunks is counted once");
  assert.equal(scanChunks(["[browser] SKIP unterminated at end of stream"]).skipped, 1, "a last line without a newline is flushed");
  assert.equal(scanChunks([`${libtest(1).slice(0, 20)}`, `${libtest(1).slice(20)}`]).executed, 1, "a summary split across chunks is still read");
  assert.equal(scanChunks(["SKIPPED is not a marker\nskip lower case\ntest foo_skip ... ok\n"]).skipped, 0);
});

test("a multi-byte character split across chunks does not corrupt the line", () => {
  const bytes = Buffer.from("ℹ pass 4\n");
  const scanner = new ResultScanner();
  scanner.push(bytes.subarray(0, 1)); scanner.push(bytes.subarray(1));
  assert.equal(scanner.finish().executed, 4);
});

test("node:test summaries in tap and spec form, and vitest summaries", () => {
  assert.deepEqual(scanChunks(["# tests 9\n# pass 7\n# fail 1\n# cancelled 0\n# skipped 1\n# todo 0\n"]), { executed: 7, failed: 1, ignored: 0, skipped: 1, binaries: 0 });
  assert.deepEqual(scanChunks(["ℹ tests 5\nℹ pass 5\nℹ fail 0\nℹ skipped 0\n"]), { executed: 5, failed: 0, ignored: 0, skipped: 0, binaries: 0 });
  assert.equal(scanChunks(["# skipped 2\n"]).skipped, 2);
  assert.deepEqual(scanChunks([" Test Files  2 passed (2)\n      Tests  1 failed | 11 passed | 2 skipped (14)\n"]), { executed: 11, failed: 1, ignored: 0, skipped: 2, binaries: 0 });
  assert.equal(scanChunks(["\u001b[2m      Tests \u001b[22m \u001b[1m\u001b[32m12 passed\u001b[39m\u001b[22m\u001b[90m (12)\u001b[39m\n"]).executed, 12, "ANSI colour is stripped first");
  assert.equal(stripAnsi("\u001b[32mok\u001b[0m"), "ok");
});

test("only test commands count toward a unit", () => {
  for (const command of ["cargo test -p rightkit-fs --no-fail-fast -- --show-output", "pnpm --dir packages/x run test", "node --test *.test.mjs"]) assert.match(command, TEST_COMMAND);
  for (const command of ["cargo clippy -p x --no-deps", "cargo check -p x --all-targets", "pnpm -r run build"]) assert.doesNotMatch(command, TEST_COMMAND);
});

test("a unit passes only with executed tests and no unapproved ignored or skipped case", () => {
  const step = (over = {}) => ({ isTest: true, executed: 5, failed: 0, ignored: 0, skipped: 0, ...over });
  assert.equal(judgeUnit({ name: "u", steps: [step(), { isTest: false }] }).status, "pass");
  assert.match(judgeUnit({ name: "u", steps: [{ isTest: false }] }).problems.join(), /no test step ran/);
  assert.match(judgeUnit({ name: "u", steps: [step({ executed: 0 })] }).problems.join(), /no test executed/);
  assert.equal(judgeUnit({ name: "u", steps: [step({ executed: 0 }), step({ executed: 3 })] }).status, "pass", "the total across a unit's feature sets and serial reruns counts");
  const ignored = judgeUnit({ name: "u", steps: [step({ ignored: 2 })] });
  assert.equal(ignored.status, "fail");
  assert.match(ignored.problems.join(), /allow_ignored/);
  assert.equal(judgeUnit({ name: "u", steps: [step({ ignored: 2 })], allowIgnored: ["u"] }).status, "allowed-skips");
  assert.equal(judgeUnit({ name: "u", steps: [step({ ignored: 2 })], allowIgnored: ["other"] }).status, "fail");
  const skipped = judgeUnit({ name: "u", steps: [step({ skipped: 1 })] });
  assert.match(skipped.problems.join(), /allow_missing_assets/);
  assert.equal(judgeUnit({ name: "u", steps: [step({ skipped: 1 })], allowMissingAssets: ["u"] }).status, "allowed-skips");
  assert.equal(judgeUnit({ name: "u", steps: [step({ skipped: 1, ignored: 1 })], allowMissingAssets: ["u"] }).status, "fail", "an allowance covers only its own kind");
  assert.deepEqual([ignored.executed, ignored.ignored, ignored.skipped], [5, 2, 0], "counts stay in the evidence");
});
