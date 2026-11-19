// The contract test's judgement of a cold run (S11) and a run after a toolchain change (S17's third run), on the gate's real output lines
// (bin/rightkit-gate.mjs: "cache lookup: P planned, H hit, M unexpected miss(es) (...), U uncacheable by design; report <file> (digest <64 hex>)" and
// "RK_CACHE_MISS: M row(s) miss the cache ...; they run now"). Adrian 2026-11-08: a miss runs the row, explained, and never stops the run
// (replacing 2026-11-07 "Test checks the stop"; decided by astra62 under Adrian's delegation, 2026-11-18). The negative cases are astra62's review 7.
import { test } from "node:test";
import assert from "node:assert/strict";
import { assertMissRun, missReportOf } from "./gate-acceptance.mjs";

const NEW = "no stored result (a new row, or the cache is empty)";
const CHANGED = "inputs or environment changed";
const lookupLine = (file, { planned, hit, misses }) => `rightkit-gate: cache lookup: ${planned} planned, ${hit} hit, ${misses} unexpected miss(es) (${misses} x), 0 uncacheable by design; report ${file} (digest ${"a".repeat(64)})`;
const announce = (n) => `rightkit-gate: RK_CACHE_MISS: ${n} row(s) miss the cache without a change to their own files; they run now. Each miss is explained in the report`;
// A run: `lookups` = [{ file, planned, hit, rows: [{ id, cause, evidence? }] }], `coverage` = [[id, class, coverage, status, key?]].
function rig({ lookups, coverage, exit, announced = null, extraLog = "" }) {
  const log = [...lookups.map((l, i) => `${lookupLine(l.file, { planned: l.planned, hit: l.hit, misses: l.lineMisses ?? l.rows.length })}\n${announce((announced ?? lookups.map((x) => x.lineMisses ?? x.rows.length))[i])}`), extraLog].join("\n");
  const files = Object.fromEntries(lookups.map((l) => [l.file, { planned: l.planned, hit: l.hit, misses: l.reportMisses ?? l.rows.length, rows: l.rows }]));
  const reports = missReportOf(log, (f) => { if (!files[f] || lookups.find((l) => l.file === f)?.unreadable) throw new Error(`ENOENT ${f}`); return files[f]; });
  const rows = coverage === null ? null : new Map(coverage.map(([id, cls, cov, status, key = `k-${id}`, group]) => [id, { id, class: cls, coverage: cov, status, key, ...(group ? { group } : {}) }]));
  return { run: { log, exit, rows }, reports };
}
const judge = (r, opts) => assertMissRun(r.run, r.reports, opts);
const text = (v) => v.problems.join("\n");

test("cold: every eligible row missed with 'no stored result', ran, nothing hit: passes", () => {
  const r = rig({ lookups: [{ file: "a.json", planned: 2, hit: 0, rows: [{ id: "a", cause: NEW }, { id: "b", cause: NEW }] }], coverage: [["a", "A", "ran", "pass"], ["b", "A", "ran", "pass"]], exit: 0 });
  assert.deepEqual(judge(r, { cause: "new" }), { ok: true, problems: [] });
});

test("cold, astra62 case 1: two misses announced and run, the report holds one row: fails", () => {
  const r = rig({ lookups: [{ file: "a.json", planned: 2, hit: 0, lineMisses: 2, reportMisses: 1, rows: [{ id: "a", cause: NEW }] }], coverage: [["a", "A", "ran", "pass"], ["b", "A", "ran", "pass"]], exit: 0 });
  const v = judge(r, { cause: "new" });
  assert.equal(v.ok, false); assert.match(text(v), /lookup 1: the report says 2 planned, 0 hit, 1 misses and holds 1 rows; the line says 2, 0, 2/); assert.match(text(v), /b: ran with a cache key but no miss report names it/);
});

test("cold: an empty report, an unreadable report, an announcement that differs, a hit, a stop: each fails", () => {
  const empty = rig({ lookups: [{ file: "a.json", planned: 1, hit: 0, lineMisses: 1, reportMisses: 0, rows: [] }], coverage: [["a", "A", "ran", "pass"]], exit: 0 });
  assert.match(text(judge(empty, { cause: "new" })), /a: ran with a cache key but no miss report names it/);
  const unreadable = rig({ lookups: [{ file: "a.json", planned: 1, hit: 0, unreadable: true, rows: [{ id: "a", cause: NEW }] }], coverage: [["a", "A", "ran", "pass"]], exit: 0 });
  assert.match(text(judge(unreadable, { cause: "new" })), /lookup 1: the report a\.json cannot be read/);
  const misannounced = rig({ lookups: [{ file: "a.json", planned: 1, hit: 0, rows: [{ id: "a", cause: NEW }] }], announced: [3], coverage: [["a", "A", "ran", "pass"]], exit: 0 });
  assert.match(text(judge(misannounced, { cause: "new" })), /announcements \(3\) do not match the lookups' misses \(1\)/);
  const hit = rig({ lookups: [{ file: "a.json", planned: 2, hit: 1, rows: [{ id: "a", cause: NEW }] }], coverage: [["a", "A", "ran", "pass"], ["b", "A", "hit", "pass"]], exit: 0 });
  assert.match(text(judge(hit, { cause: "new" })), /hit on a run whose cache was moved aside/);
  const stopped = { run: { log: "rightkit-gate: BLOCKED: RK_CACHE_MISS", exit: 1, rows: new Map() }, reports: [] };
  assert.match(text(judge(stopped, { cause: "new" })), /stopped on RK_CACHE_MISS/); assert.match(text(judge(stopped, { cause: "new" })), /no cache lookup names a miss report/);
});

// The real shape of S17's third run (Dell, 2026-11-18): class A misses explained by the changed toolchain variable, class B members "no stored result"
// and then answered by their group's key, the rows that failed before ran again.
const changedRun = (over = {}) => rig({
  lookups: [
    { file: "a.json", planned: 2, hit: 1, rows: over.aRows ?? [{ id: "cargo-deny:rk", cause: CHANGED, evidence: { inputs: [], inheritedEnv: ["RUSTUP_TOOLCHAIN"] } }] },
    { file: "b.json", planned: 3, hit: 1, rows: over.bRows ?? [{ id: "clippy:rk:rk-core", cause: NEW }, { id: "test:rust:rk:rk-core", cause: NEW }] },
  ],
  coverage: over.coverage ?? [["cargo-deny:rk", "A", "ran", "pass"], ["tsc:x", "A", "hit", "pass"], ["clippy:rk:rk-core", "B", "hit", "pass", "k-c", "clippy:batch:822ddd83"], ["test:rust:rk:rk-core", "B", "ran", "fail"], ["fmt:rk", "B", "hit", "pass"]],
  exit: 1,
});
const S17 = { cause: "changed", ids: ["test:rust:rk:rk-core"], priorFailed: ["test:rust:rk:rk-core"], changedVar: "RUSTUP_TOOLCHAIN" };

test("toolchain change: evidenced class A misses, group members answered by their group, the failing row ran again: passes", () => {
  assert.deepEqual(judge(changedRun(), S17), { ok: true, problems: [] });
});

test("toolchain change, astra62 case 2: a blank cause and an 'unknown' cause fail", () => {
  const v = judge(changedRun({ aRows: [{ id: "cargo-deny:rk", cause: "" }, { id: "x", cause: "unknown" }], coverage: [["cargo-deny:rk", "A", "ran", "pass"], ["x", "A", "ran", "pass"], ["clippy:rk:rk-core", "B", "hit", "pass", "k-c", "clippy:batch:822ddd83"], ["test:rust:rk:rk-core", "B", "ran", "fail"]] }), S17);
  assert.match(text(v), /cargo-deny:rk: cause "\(blank\)" is not an evidenced explanation/); assert.match(text(v), /x: cause "unknown" is not an evidenced explanation/);
});

test("toolchain change: an unrelated change, a cause without evidence, an unsupported 'no stored result', a failing row reused: each fails", () => {
  const unrelated = judge(changedRun({ aRows: [{ id: "cargo-deny:rk", cause: CHANGED, evidence: { inheritedEnv: ["PATH"] } }] }), S17);
  assert.match(text(unrelated), /no miss names RUSTUP_TOOLCHAIN as changed/);
  const noEvidence = judge(changedRun({ aRows: [{ id: "cargo-deny:rk", cause: CHANGED, evidence: {} }] }), S17);
  assert.match(text(noEvidence), /cargo-deny:rk: cause "inputs or environment changed" is not an evidenced explanation/);
  const classANew = judge(changedRun({ aRows: [{ id: "cargo-deny:rk", cause: NEW }, { id: "pnpm-audit:app", cause: CHANGED, evidence: { inheritedEnv: ["RUSTUP_TOOLCHAIN"] } }], coverage: [["cargo-deny:rk", "A", "ran", "pass"], ["pnpm-audit:app", "A", "ran", "pass"], ["clippy:rk:rk-core", "B", "hit", "pass", "k-c", "clippy:batch:822ddd83"], ["test:rust:rk:rk-core", "B", "ran", "fail"]] }), S17);
  assert.match(text(classANew), /cargo-deny:rk: cause "no stored result[^"]*" is not an evidenced explanation/);
  const reused = judge(changedRun({ coverage: [["cargo-deny:rk", "A", "ran", "pass"], ["clippy:rk:rk-core", "B", "hit", "pass", "k-c", "clippy:batch:822ddd83"], ["test:rust:rk:rk-core", "B", "hit", "fail"]] }), S17);
  assert.match(text(reused), /test:rust:rk:rk-core: failed before and did not run again \(coverage hit\)/);
  const base = changedRun();
  assert.match(text(judge({ ...base, run: { ...base.run, exit: 0 } }, S17)), /exit 0 does not agree with the rows \(a row failed\)/);
  assert.match(text(judge({ ...base, run: { ...base.run, rows: null } }, S17)), /no readable coverage report/);
});

test("astra62 review 8: a class B hit without group proof, an unknown cause carrying tool data, a report under another file name: each fails", () => {
  // A standalone class B row that missed with "no stored result" and was then a hit, with no group named: nothing explains it.
  const standalone = judge(changedRun({ coverage: [["cargo-deny:rk", "A", "ran", "pass"], ["clippy:rk:rk-core", "B", "hit", "pass"], ["test:rust:rk:rk-core", "B", "ran", "fail"]] }), S17);
  assert.match(text(standalone), /clippy:rk:rk-core: named as a miss but did not run \(coverage hit\)/);
  assert.match(text(standalone), /clippy:rk:rk-core: cause "no stored result[^"]*" is not an evidenced explanation/);
  // An unrecognised cause is not rescued by tool data; a recognised cause with an empty tool object is not evidence.
  const toolUnknown = judge(changedRun({ aRows: [{ id: "cargo-deny:rk", cause: "unknown", evidence: { tool: { from: "a", to: "b" }, inheritedEnv: ["RUSTUP_TOOLCHAIN"] } }] }), S17);
  assert.match(text(toolUnknown), /cargo-deny:rk: cause "unknown" is not an evidenced explanation/);
  const emptyTool = judge(changedRun({ aRows: [{ id: "cargo-deny:rk", cause: CHANGED, evidence: { tool: {} } }] }), S17);
  assert.match(text(emptyTool), /cargo-deny:rk: cause "inputs or environment changed" is not an evidenced explanation/);
  // Each lookup is matched to the report it names; a report under another name is not substituted.
  const base = changedRun();
  const renamed = judge({ ...base, reports: base.reports.map((r, i) => (i === 1 ? { ...r, file: "other.json" } : r)) }, S17);
  assert.match(text(renamed), /lookup 2: its report b\.json was not read/);
});

test("coverage names the batched group that answered a member row, and nothing for a row that ran alone", async () => {
  const { buildCoverageReport } = await import("./gate-coverage.mjs");
  const plan = { scans: [{ id: "clippy:a", class: "B" }, { id: "clippy:b", class: "B" }] };
  const scanResult = { scans: [{ id: "clippy:a", status: "pass", cached: true, batchedWith: { id: "clippy:batch:822ddd83", units: ["a", "b"] } }, { id: "clippy:b", status: "pass" }] };
  const rows = new Map(buildCoverageReport({ plan, scanResult, keys: new Map([["clippy:a", "k1"], ["clippy:b", "k2"]]) }).scans.map((r) => [r.id, r]));
  assert.equal(rows.get("clippy:a").group, "clippy:batch:822ddd83"); assert.equal(rows.get("clippy:a").coverage, "hit");
  assert.equal(Object.hasOwn(rows.get("clippy:b"), "group"), false);
});
