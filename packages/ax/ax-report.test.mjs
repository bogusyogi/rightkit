import assert from "node:assert/strict";
import test from "node:test";
import { aggregateReport, gateExitCode, renderMarkdown, toSarif } from "./ax/report.mjs";

const MANIFEST = { ax: { gates: { static: "hard", conformance: "hard", behavioral: "report-only" } } };

function staticResultWith(rules) {
  return { schemaVersion: 1, gate: "static", rules, verdict: rules.every((r) => r.status !== "fail") ? "pass" : "fail" };
}

test("SARIF output is a well-formed 2.1.0 log with one result per failing rule", () => {
  const rules = [
    { ruleId: "static-2-wire-description", severity: "error", status: "fail", claim: "wire description", evidence: "tool_a missing", remediation: "add it" },
    { ruleId: "static-3-strict-input-schema", severity: "error", status: "pass", claim: "strict schema", evidence: "ok", remediation: null },
  ];
  const sarif = toSarif(staticResultWith(rules));
  assert.equal(sarif.version, "2.1.0");
  assert.equal(sarif.runs.length, 1);
  assert.equal(sarif.runs[0].tool.driver.name, "right-ax");
  assert.equal(sarif.runs[0].results.length, 1);
  assert.equal(sarif.runs[0].results[0].ruleId, "static-2-wire-description");
  assert.equal(sarif.runs[0].results[0].level, "error");
  assert.ok(sarif.runs[0].results[0].message.text.includes("tool_a missing"));
});

test("gateResult is fail when a hard gate fails, and the report never uses signoff/clean language", () => {
  const staticResult = staticResultWith([{ ruleId: "static-1-tool-set", severity: "error", status: "fail", claim: "x", evidence: "y", remediation: "z" }]);
  const conformanceResult = { rows: [], totals: { pass: 0, fail: 0 }, verdict: "pass" };
  const report = aggregateReport({ manifest: MANIFEST, staticResult, conformanceResult, behavioralResult: null });
  assert.equal(report.gateResult, "fail");
  assert.equal(report.signoff, null);
  assert.equal(gateExitCode(report), 1);
  const markdown = renderMarkdown(report);
  assert.ok(!/\bclean\b/i.test(markdown));
  assert.ok(markdown.includes("never signs off"));
});

test("behavioral claim stays UNPROVEN even when every stub trial passes", () => {
  const staticResult = staticResultWith([]);
  const conformanceResult = { rows: [], totals: {}, verdict: "pass" };
  const behavioralResult = { provenBy: "stub", scenarioCount: 3, totals: { pass: 9, fail: 0 }, verdict: "pass" };
  const report = aggregateReport({ manifest: MANIFEST, staticResult, conformanceResult, behavioralResult });
  assert.match(report.gates.behavioral.claim, /UNPROVEN/);
  assert.equal(report.gates.behavioral.status, "report-only");
  // report-only behavioral does not fail the overall hard-gate result
  assert.equal(report.gateResult, "pass");
  const markdown = renderMarkdown(report);
  assert.ok(markdown.includes("## UNPROVEN"));
});

test("a hard behavioral gate fails the overall gateResult when the matrix has not passed", () => {
  const manifest = { ax: { gates: { static: "hard", conformance: "hard", behavioral: "hard" } } };
  const staticResult = staticResultWith([]);
  const conformanceResult = { rows: [], totals: {}, verdict: "pass" };
  const behavioralResult = { provenBy: "stub", scenarioCount: 1, totals: {}, verdict: "fail" };
  const report = aggregateReport({ manifest, staticResult, conformanceResult, behavioralResult });
  assert.equal(report.gates.behavioral.status, "fail");
  assert.equal(report.gateResult, "fail");
});

test("real-matrix provenance drops the UNPROVEN language", () => {
  const staticResult = staticResultWith([]);
  const conformanceResult = { rows: [], totals: {}, verdict: "pass" };
  const behavioralResult = { provenBy: "real-matrix", scenarioCount: 3, totals: { pass: 15, fail: 0 }, verdict: "pass" };
  const report = aggregateReport({ manifest: MANIFEST, staticResult, conformanceResult, behavioralResult });
  assert.ok(!/UNPROVEN/.test(report.gates.behavioral.claim));
});

// F1 regression: a static gate configured "report-only" with a genuinely
// failing rule must never render as an overall PASS, and the exit code must
// not read as success. Before the fix, gateStatus() returned the literal
// string "report-only" for ANY report-only gate regardless of its actual
// verdict, and hardGatesPass ignored report-only gates entirely — so
// `right-ax ax report --format markdown` printed "Gate result: **PASS**"
// and exited 0 while 1/14 static rules were failing.
test("F1: a report-only static gate with a real failing rule never renders as PASS and never exits 0", () => {
  const manifest = { ax: { gates: { static: "report-only", conformance: "report-only", behavioral: "report-only" } } };
  const staticResult = staticResultWith([
    { ruleId: "static-2-wire-description", severity: "error", status: "fail", claim: "wire description", evidence: "tool_a missing", remediation: "add it" },
  ]);
  const conformanceResult = { rows: [], totals: {}, verdict: "pass" };
  const behavioralResult = { provenBy: "stub", scenarioCount: 1, totals: { pass: 5, fail: 0 }, verdict: "pass" };
  const report = aggregateReport({ manifest, staticResult, conformanceResult, behavioralResult });
  assert.equal(report.gateResult, "fail");
  assert.equal(gateExitCode(report), 1);
  assert.equal(report.gates.static.status, "report-only-failing");
  const markdown = renderMarkdown(report);
  assert.ok(!/Gate result: \*\*PASS\*\*/.test(markdown), markdown);
  assert.match(markdown, /Gate result: \*\*FAIL\*\*/);
});

// A report-only gate with NO underlying failure still reads as an honest
// pass — severity configuration is legible, not just suppressed to "fail
// always".
test("F1: a report-only static gate with zero failing rules still renders PASS", () => {
  const manifest = { ax: { gates: { static: "report-only", conformance: "report-only", behavioral: "report-only" } } };
  const staticResult = staticResultWith([]);
  const conformanceResult = { rows: [], totals: {}, verdict: "pass" };
  const report = aggregateReport({ manifest, staticResult, conformanceResult, behavioralResult: null });
  assert.equal(report.gateResult, "pass");
  assert.equal(report.gates.static.status, "report-only");
});
