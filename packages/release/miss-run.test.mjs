// The contract test's judgement of a cold run (S11) and a run after a toolchain change (S17's third run), on the gate's real output lines
// (bin/rightkit-gate.mjs: "cache lookup: ... report <file> (digest <64 hex>)" and "RK_CACHE_MISS: N row(s) miss the cache ...; they run now").
// Adrian 2026-11-08: a miss runs the row, explained, and never stops the run (replacing 2026-11-07 "Test checks the stop"; astra62, 2026-11-18).
import { test } from "node:test";
import assert from "node:assert/strict";
import { assertMissRun, missReportOf } from "./gate-acceptance.mjs";

const digest = "a".repeat(64);
const logOf = (misses, extra = "") => `rightkit-gate: cache lookup: 3 planned, 0 hit, ${misses} unexpected miss(es) (${misses} no stored result (a new row, or the cache is empty)), 0 uncacheable by design; report D:/runs/rightkit-gate-misses/x.json (digest ${digest})\nrightkit-gate: RK_CACHE_MISS: ${misses} row(s) miss the cache without a change to their own files; they run now. Each miss is explained in the report; a miss where nothing changed is a cache bug: file it with the report\n${extra}`;
const report = (rows, hit = 0) => (file) => ({ file, schema: "rightkit.gate-miss-report/1", hit, rows });
const rowsOf = (entries) => new Map(entries.map(([id, coverage, status]) => [id, { id, class: "B", coverage, status }]));

test("a cold run that ran every miss, each 'no stored result', passes", () => {
  const log = logOf(2);
  const r = missReportOf(log, report([{ id: "a", cause: "no stored result (a new row, or the cache is empty)" }, { id: "b", cause: "no stored result (a new row, or the cache is empty)" }]));
  assert.deepEqual(assertMissRun({ log, exit: 0, rows: rowsOf([["a", "ran", "pass"], ["b", "ran", "pass"]]) }, r, { cause: "new" }), { ok: true, problems: [] });
});

test("a stop, a hit, a miss that did not run, a missing announcement or an exit that disagrees with the rows each fail", () => {
  const cold = (rows) => missReportOf(logOf(1), report(rows));
  const one = [{ id: "a", cause: "no stored result (a new row, or the cache is empty)" }];
  const stopped = assertMissRun({ log: "rightkit-gate: BLOCKED: RK_CACHE_MISS", exit: 1, rows: rowsOf([["a", "skipped", "not-run"]]) }, cold(one), { cause: "new" });
  assert.match(stopped.problems.join("\n"), /stopped on RK_CACHE_MISS/); assert.match(stopped.problems.join("\n"), /did not announce/); assert.match(stopped.problems.join("\n"), /a: a miss that did not run/);
  assert.match(assertMissRun({ log: logOf(1), exit: 0, rows: rowsOf([["a", "hit", "pass"]]) }, cold(one), { cause: "new" }).problems.join("\n"), /a: a miss that did not run \(coverage hit\)/);
  assert.match(assertMissRun({ log: logOf(1), exit: 0, rows: rowsOf([["a", "ran", "pass"]]) }, missReportOf(logOf(1), report(one, 2)), { cause: "new" }).problems.join("\n"), /2 row\(s\) hit on a run whose cache was moved aside/);
  assert.match(assertMissRun({ log: logOf(1), exit: 0, rows: rowsOf([["a", "ran", "fail"]]) }, cold(one), { cause: "new" }).problems.join("\n"), /exit 0 does not agree with the rows \(a row failed\)/);
  assert.match(assertMissRun({ log: logOf(1), exit: 1, rows: rowsOf([["a", "ran", "pass"]]) }, cold(one), { cause: "new" }).problems.join("\n"), /exit 1 does not agree/);
  assert.match(assertMissRun({ log: logOf(1), exit: 0, rows: null }, cold(one), { cause: "new" }).problems.join("\n"), /no readable coverage/);
});

test("after a toolchain change, every failing row is named with cause 'changed' and runs again", () => {
  const changed = "inputs or environment changed";
  const log = logOf(1);
  const ok = assertMissRun({ log, exit: 1, rows: rowsOf([["test:rust:rk:rk-core:2", "ran", "fail"]]) }, missReportOf(log, report([{ id: "test:rust:rk:rk-core:2", cause: changed }])), { cause: "changed", ids: ["test:rust:rk:rk-core:2"] });
  assert.deepEqual(ok, { ok: true, problems: [] });
  const reused = assertMissRun({ log, exit: 1, rows: rowsOf([["test:rust:rk:rk-core:2", "hit", "fail"], ["x", "ran", "fail"]]) }, missReportOf(log, report([{ id: "x", cause: changed }])), { cause: "changed", ids: ["test:rust:rk:rk-core:2"] });
  assert.match(reused.problems.join("\n"), /rk-core:2: not in the miss report: the gate would have reused it/);
  assert.match(reused.problems.join("\n"), /rk-core:2: a miss that did not run \(coverage hit\)/);
});

test("a run that looks the cache up twice (a compile's check rows, then its compile rows) is judged on every report it names", () => {
  const line = (file) => `rightkit-gate: cache lookup: 9 planned, 1 hit, 1 unexpected miss(es) (1 inputs or environment changed), 0 uncacheable by design; report ${file} (digest ${"b".repeat(64)})`;
  const log = `${line("D:/r/one.json")}\n${line("D:/r/two.json")}\nrightkit-gate: RK_CACHE_MISS: 1 row(s) miss the cache without a change to their own files; they run now.\n`;
  const reports = { "D:/r/one.json": { hit: 1, rows: [{ id: "clippy:a", cause: "inputs or environment changed" }] }, "D:/r/two.json": { hit: 2, rows: [{ id: "test:rust:a", cause: "no stored result (a new row, or the cache is empty)" }] } };
  const merged = missReportOf(log, (f) => reports[f]);
  assert.deepEqual(merged.files, ["D:/r/one.json", "D:/r/two.json"]); assert.equal(merged.hit, 3); assert.deepEqual(merged.rows.map((r) => r.id), ["clippy:a", "test:rust:a"]);
  const v = assertMissRun({ log, exit: 1, rows: new Map([["clippy:a", { coverage: "ran", status: "pass" }], ["test:rust:a", { coverage: "ran", status: "fail" }]]) }, merged, { cause: "changed", ids: ["test:rust:a"] });
  assert.deepEqual(v, { ok: true, problems: [] });
});
