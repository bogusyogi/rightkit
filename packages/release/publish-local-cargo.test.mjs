// What a compile report must carry before `right-release publish-local cargo` uploads a crate (rightkit#1471, rightkit#1560).
import assert from "node:assert/strict";
import test from "node:test";
import { compileReportProblems, unitListCompileProblems, UNIT_LIST_COMPILE_SCHEMA, UNIT_REPORT_SCHEMA } from "./publish-local-cargo.mjs";

const commit = "8e6f0c433d00192f6947c0cb204a487507cbcce3";
const tree = "ae706fe0a86330d6f683b49234ebc53be7168bfd";
const name = "rightkit-agent-engine-tasks";
const unit = `cargo:${name}`;
const seam = () => ({ seam: true, requestId: "d1257d64-5043-4248-b8b7-3e35b8b05e2d", receiptSha256: "9".repeat(64) });
const command = (over = {}) => ({ selector: unit, id: `publisher:${unit}:1`, commit, tree, attempts: [{ id: `publisher:${unit}:1`, status: "pass", snapshotEvidence: seam() }], ...over });
const unitList = (over = {}) => ({
  schema: UNIT_LIST_COMPILE_SCHEMA, runId: "68d9d224-6434-4e00-8d52-78c113535fc4", commit, unit, status: "passed",
  requirements: [{ selector: unit, id: `publisher:${unit}:0`, status: "passed" }, { selector: unit, id: `publisher:${unit}:1`, status: "pass" }],
  commands: [command()],
  scans: [{ id: `clippy:${name}`, status: "pass", snapshotEvidence: seam(), sharedOrigin: true }],
  ...over,
});
const ctx = { name, version: "0.1.1", commit, tree };

test("a unit-list receipt that passed on the exact commit and tree with broker seam receipts proves the crate (rightkit#1560)", () => {
  assert.deepEqual(compileReportProblems(unitList(), ctx), []);
  assert.deepEqual(unitListCompileProblems(unitList(), ctx), []);
});

test("a unit-list receipt is refused for another unit, another commit or tree, or a status other than passed", () => {
  assert.match(compileReportProblems(unitList({ unit: "cargo:rightkit-agents" }), ctx).join("; "), /unit is "cargo:rightkit-agents"/);
  assert.match(compileReportProblems(unitList({ unit: `npm:${name}` }), ctx).join("; "), /unit is/);
  assert.match(compileReportProblems(unitList({ commit: "0".repeat(40) }), ctx).join("; "), /commit 0{40} != /);
  for (const status of ["failed", "blocked", "pass", undefined]) assert.match(compileReportProblems(unitList({ status }), ctx).join("; "), /unit status is/);
  assert.match(compileReportProblems(unitList({ commands: [command({ tree: "1".repeat(40) })] }), ctx).join("; "), /ran on tree 1{40}/);
  assert.match(compileReportProblems(unitList({ commands: [command({ commit: "2".repeat(40) })] }), ctx).join("; "), /ran on commit 2{40}/);
  assert.match(compileReportProblems(unitList({ commands: [command({ selector: "cargo:rightkit-agents" })] }), ctx).join("; "), /belongs to "cargo:rightkit-agents"/);
});

test("a unit-list receipt is refused without commands, requirements or scans, or when any of them did not pass through the broker seam", () => {
  assert.match(compileReportProblems(unitList({ commands: [] }), ctx).join("; "), /no broker command is recorded/);
  assert.match(compileReportProblems(unitList({ commands: undefined }), ctx).join("; "), /no broker command is recorded/);
  assert.match(compileReportProblems(unitList({ requirements: [] }), ctx).join("; "), /no requirement is recorded/);
  assert.match(compileReportProblems(unitList({ scans: [] }), ctx).join("; "), /no scan is recorded/);
  // The last attempt decides: an earlier pass followed by a failure is no pass.
  const failedLast = command({ attempts: [{ status: "pass", snapshotEvidence: seam() }, { status: "fail", snapshotEvidence: seam() }] });
  assert.match(compileReportProblems(unitList({ commands: [failedLast] }), ctx).join("; "), /no passing broker seam receipt as its last attempt/);
  assert.deepEqual(compileReportProblems(unitList({ commands: [command({ attempts: [{ status: "fail", snapshotEvidence: seam() }, { status: "pass", snapshotEvidence: seam() }] })] }), ctx), []);
  assert.match(compileReportProblems(unitList({ commands: [command({ attempts: [] })] }), ctx).join("; "), /no passing broker seam receipt/);
  // A pass that is not a broker seam receipt (no seam flag, no request id, no receipt digest) is no proof of a broker compile.
  for (const evidence of [{ ...seam(), seam: false }, { ...seam(), requestId: "" }, { ...seam(), receiptSha256: "abc" }, undefined]) {
    assert.match(compileReportProblems(unitList({ commands: [command({ attempts: [{ status: "pass", snapshotEvidence: evidence }] })] }), ctx).join("; "), /no passing broker seam receipt/);
  }
  // A scan with evidence of its own must show a broker seam receipt; a scan answered from the gate's cache carries none and passes on its status.
  for (const evidence of [{ ...seam(), seam: false }, { ...seam(), requestId: "" }, { ...seam(), receiptSha256: "abc" }, null, {}]) {
    assert.match(compileReportProblems(unitList({ scans: [{ id: "clippy:x", status: "pass", snapshotEvidence: evidence }] }), ctx).join("; "), /scan clippy:x carries evidence that is not a broker seam receipt/);
  }
  assert.deepEqual(compileReportProblems(unitList({ scans: [{ id: "doc:x", status: "pass", sharedOrigin: true }] }), ctx), []);
  assert.match(compileReportProblems(unitList({ scans: [{ id: "doc:x", status: "fail", snapshotEvidence: seam() }] }), ctx).join("; "), /scan doc:x is "fail"/);
  assert.match(compileReportProblems(unitList({ scans: [{ id: "doc:x", status: "cached" }] }), ctx).join("; "), /scan doc:x is "cached"/);
  // The unit's own commands never pass on status alone.
  assert.match(compileReportProblems(unitList({ commands: [command({ attempts: [{ status: "pass" }] })] }), ctx).join("; "), /no passing broker seam receipt/);
  assert.match(compileReportProblems(unitList({ requirements: [{ selector: unit, id: "r", status: "failed" }] }), ctx).join("; "), /requirement r/);
  assert.match(compileReportProblems(unitList({ requirements: [{ selector: "cargo:rightkit-agents", id: "r", status: "passed" }] }), ctx).join("; "), /requirement r/);
});

test("the single-unit gate report keeps its own rule, and an unknown schema is refused", () => {
  const report = { schema: UNIT_REPORT_SCHEMA, unit: { kind: "cargo", name, version: "0.1.1" }, commit, tree, executor: "broker", stages: { check: { ok: true }, compile: { ok: true } } };
  assert.deepEqual(compileReportProblems(report, ctx), []);
  assert.match(compileReportProblems({ ...report, executor: "direct" }, ctx).join("; "), /not a broker host's compile/);
  assert.match(compileReportProblems({ ...report, unit: { kind: "cargo", name, version: "0.1.0" } }, ctx).join("; "), /unit version 0\.1\.0 != 0\.1\.1/);
  assert.match(compileReportProblems({ ...report, stages: { check: { ok: true }, compile: { ok: false } } }, ctx).join("; "), /stage compile is not ok/);
  assert.match(compileReportProblems({ ...unitList(), schema: "rightkit.unit-list-report/1" }, ctx).join("; "), /schema is "rightkit\.unit-list-report\/1"/);
  assert.match(compileReportProblems(null, ctx).join("; "), /schema is undefined/);
});
