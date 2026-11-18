import { test } from "node:test";
import assert from "node:assert/strict";
import { buildCoverageReport, limitedEvidenceOf } from "./gate-coverage.mjs";
import { assertLimitedEvidence, validateCoverage } from "./gate-acceptance.mjs";

test("cross-target rows are type-check-only evidence in coverage and in the compile receipt, and the runner accepts both (NN38)", () => {
  // Row ids as the witness fixture's compile planned them on 2026-11-17.
  const ids = ["clippy:app/engine:witness-engine", "clippy:app/engine:witness-engine@x86_64-unknown-linux-gnu", "clippy:app/engine:witness-engine@aarch64-apple-darwin", "test:rust:rk:rk-util"];
  const plan = { scans: ids.map((id) => ({ id, class: "B" })) };
  const scanResult = { scans: [{ id: ids[0], status: "pass" }, { id: ids[1], status: "pass", cached: true }, { id: ids[2], status: "pass" }, { id: ids[3], status: "pass" }] };
  const report = buildCoverageReport({ plan, scanResult, keys: new Map(ids.map((id) => [id, "k".repeat(64)])), commit: "a".repeat(40), startedAt: "2026-11-18T00:00:00.000Z", endedAt: "2026-11-18T00:00:01.000Z" });
  const byId = new Map(report.scans.map((r) => [r.id, r]));
  assert.equal(byId.get(ids[1]).evidence, "type-check-only"); assert.equal(byId.get(ids[2]).evidence, "type-check-only");
  assert.equal(byId.get(ids[0]).evidence, undefined); assert.equal(byId.get(ids[3]).evidence, undefined);
  assert.deepEqual(limitedEvidenceOf(report), [{ id: ids[1], evidence: "type-check-only" }, { id: ids[2], evidence: "type-check-only" }]);
  // The runner: the coverage schema allows the field, and NN38 passes with the receipt's list.
  assert.deepEqual(validateCoverage({ ...report, schema: report.schema }).problems.filter((p) => /evidence/.test(p)), []);
  const rows = new Map(report.scans.map((r) => [r.id, r]));
  const ok = assertLimitedEvidence({ S1: { rows, receipt: { stages: { compile: { limitedEvidence: limitedEvidenceOf(report) } } } } });
  assert.equal(ok.ok, true, ok.problems.join("; "));
  const missing = assertLimitedEvidence({ S1: { rows, receipt: { stages: { compile: {} } } } });
  assert.equal(missing.ok, false);
});
