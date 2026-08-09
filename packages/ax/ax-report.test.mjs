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
  const sarif = toSarif({ staticResult: staticResultWith(rules) });
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

// CLAIM-FIDELITY regression: `ax report --format sarif` previously called
// toSarif(staticResult) only, so a run with a clean static gate but a
// failing conformance or behavioral gate emitted a SARIF doc with zero
// results — reading as CLEAN while gateResult/gateExitCode said "fail".
// toSarif's `results` emptiness must always match gateResult === "pass".
test("CLAIM-FIDELITY: a conformance-only failure yields non-empty SARIF matching gateExitCode", () => {
  const staticResult = staticResultWith([]);
  const conformanceResult = {
    rows: [{ tool: "tool_a", status: "fail", probes: [{ kind: "success-path", status: "fail", detail: "server accepted call missing required arguments" }], checks: {} }],
    totals: { pass: 0, fail: 1 },
    verdict: "fail",
  };
  const behavioralResult = null;
  const report = aggregateReport({ manifest: MANIFEST, staticResult, conformanceResult, behavioralResult });
  assert.equal(report.gateResult, "fail");
  const exitCode = gateExitCode(report);
  assert.equal(exitCode, 1);
  const sarif = toSarif({ staticResult, conformanceResult, behavioralResult });
  assert.ok(sarif.runs[0].results.length > 0, "SARIF must not be empty when the conformance gate failed");
  assert.ok(sarif.runs[0].results.some((result) => result.ruleId.startsWith("conformance-")));
});

test("CLAIM-FIDELITY: a behavioral-only failure yields non-empty SARIF matching gateExitCode", () => {
  const manifest = { ax: { gates: { static: "hard", conformance: "hard", behavioral: "hard" } } };
  const staticResult = staticResultWith([]);
  const conformanceResult = { rows: [], totals: {}, verdict: "pass" };
  const behavioralResult = {
    provenBy: "stub",
    scenarioCount: 1,
    scenarioResults: { "scenario-1": { status: "fail", passCount: 0, failCount: 1 } },
    trials: [{ scenarioId: "scenario-1", trials: [{ trial: 1, status: "fail", failures: ["final environment mismatch"] }] }],
    totals: { pass: 0, fail: 1 },
    verdict: "fail",
  };
  const report = aggregateReport({ manifest, staticResult, conformanceResult, behavioralResult });
  assert.equal(report.gateResult, "fail");
  const exitCode = gateExitCode(report);
  assert.equal(exitCode, 1);
  const sarif = toSarif({ staticResult, conformanceResult, behavioralResult });
  assert.ok(sarif.runs[0].results.length > 0, "SARIF must not be empty when the behavioral gate failed");
  assert.ok(sarif.runs[0].results.some((result) => result.ruleId.startsWith("behavioral-")));
});

test("CLAIM-FIDELITY: SARIF stays empty only when gateResult is pass", () => {
  const staticResult = staticResultWith([]);
  const conformanceResult = { rows: [], totals: {}, verdict: "pass" };
  const behavioralResult = { provenBy: "stub", scenarioCount: 0, scenarioResults: {}, trials: [], totals: {}, verdict: "pass" };
  const report = aggregateReport({ manifest: MANIFEST, staticResult, conformanceResult, behavioralResult });
  assert.equal(report.gateResult, "pass");
  assert.equal(gateExitCode(report), 0);
  const sarif = toSarif({ staticResult, conformanceResult, behavioralResult });
  assert.equal(sarif.runs[0].results.length, 0);
});

// F1 (round 2) regression: the exported aggregator must not trust a
// precomputed top-level `verdict` in isolation. A hand-crafted (forged, or
// simply miscomputed upstream) gate result carrying `verdict: "pass"`
// alongside a genuinely failing leaf finding must still surface as a
// failure — the property this proves is "recompute health from leaf
// findings", exercised once per gate shape (static rules[], conformance
// rows[], behavioral scenarioResults{}).
test("F1: a forged top-level pass verdict never suppresses a failing STATIC leaf rule", () => {
  const manifest = { ax: { gates: { static: "hard", conformance: "hard", behavioral: "report-only" } } };
  const staticResult = { schemaVersion: 1, gate: "static", verdict: "pass", rules: [{ ruleId: "static-1-tool-set", severity: "error", status: "fail", claim: "x", evidence: "y", remediation: "z" }] };
  const conformanceResult = { rows: [], totals: {}, verdict: "pass" };
  const report = aggregateReport({ manifest, staticResult, conformanceResult, behavioralResult: null });
  assert.equal(report.gateResult, "fail");
  assert.equal(gateExitCode(report), 1);
  assert.equal(report.gates.static.status, "fail");
});

test("F1: a forged top-level pass verdict never suppresses a failing CONFORMANCE leaf row", () => {
  const manifest = { ax: { gates: { static: "hard", conformance: "hard", behavioral: "report-only" } } };
  const staticResult = staticResultWith([]);
  const conformanceResult = { schemaVersion: 1, gate: "conformance", verdict: "pass", fatal: null, rows: [{ tool: "bad_tool", status: "fail", reasons: ["something failed"] }] };
  const report = aggregateReport({ manifest, staticResult, conformanceResult, behavioralResult: null });
  assert.equal(report.gateResult, "fail");
  assert.equal(gateExitCode(report), 1);
  assert.equal(report.gates.conformance.status, "fail");
});

test("F1: a forged top-level pass verdict never suppresses a failing BEHAVIORAL leaf scenario (hard gate)", () => {
  const manifest = { ax: { gates: { static: "hard", conformance: "hard", behavioral: "hard" } } };
  const staticResult = staticResultWith([]);
  const conformanceResult = { rows: [], totals: {}, verdict: "pass" };
  const behavioralResult = { provenBy: "stub", verdict: "pass", scenarioResults: { "s-1": { status: "fail", passCount: 0, failCount: 5 } }, totals: { pass: 0, fail: 5 } };
  const report = aggregateReport({ manifest, staticResult, conformanceResult, behavioralResult });
  assert.equal(report.gateResult, "fail");
  assert.equal(gateExitCode(report), 1);
  assert.equal(report.gates.behavioral.status, "fail");
});

// A fatal field is itself a leaf-level fact (not a derived verdict) and must
// independently force failure even if a caller also, incorrectly, marked
// verdict "pass" alongside it.
test("F1: a `.fatal` gate result forces failure even alongside a forged pass verdict", () => {
  const manifest = { ax: { gates: { static: "hard", conformance: "hard", behavioral: "report-only" } } };
  const staticResult = staticResultWith([]);
  const conformanceResult = { rows: [], totals: {}, verdict: "pass", fatal: { code: "no_tools", message: "nothing to probe" } };
  const report = aggregateReport({ manifest, staticResult, conformanceResult, behavioralResult: null });
  assert.equal(report.gateResult, "fail");
  assert.equal(report.gates.conformance.status, "fail");
});

// F1 (round 3) regression: the round-2 leaf scan only checked ONE level deep
// (rows[].status itself). A row/scenario SUMMARY that says "pass" while a
// nested leaf underneath it (a conformance probe, or a behavioral trial)
// says "fail" must still surface as a failing gate — that nested leaf is
// where the real evidence lives, and the summary next to it is exactly the
// kind of intermediate this file's own I1 banner says can never be trusted
// alone. Exercised across both hard and report-only severities, because
// severity must only ever change presentation, never visibility.
test("F1: a passing CONFORMANCE row summary with a failing nested probe is never masked, at any severity", () => {
  for (const conformanceSeverity of ["hard", "report-only"]) {
    const manifest = { ax: { gates: { static: "hard", conformance: conformanceSeverity, behavioral: "report-only" } } };
    const staticResult = staticResultWith([]);
    const conformanceResult = {
      schemaVersion: 1,
      gate: "conformance",
      verdict: "pass",
      fatal: null,
      rows: [{
        tool: "sneaky_tool",
        status: "pass", // the row's OWN summary lies
        reasons: [],
        probes: [
          { kind: "valid-args", status: "pass" },
          { kind: "missing-required", status: "fail", detail: "nested probe failure the row summary did not reflect" },
        ],
      }],
    };
    const report = aggregateReport({ manifest, staticResult, conformanceResult, behavioralResult: null });
    assert.equal(report.gateResult, "fail", `conformance severity=${conformanceSeverity}`);
    assert.equal(gateExitCode(report), 1);
    assert.equal(report.gates.conformance.status, conformanceSeverity === "hard" ? "fail" : "report-only-failing");
  }
});

test("F1: a passing BEHAVIORAL scenario summary with a failing nested trial is never masked, at any severity", () => {
  for (const behavioralSeverity of ["hard", "report-only"]) {
    const manifest = { ax: { gates: { static: "hard", conformance: "hard", behavioral: behavioralSeverity } } };
    const staticResult = staticResultWith([]);
    const conformanceResult = { rows: [], totals: {}, verdict: "pass" };
    const behavioralResult = {
      provenBy: "stub",
      verdict: "pass",
      // scenarioResults says pass; the actual per-trial record right next to
      // it (report.trials, produced by runScenarios) says otherwise.
      scenarioResults: { "s-1": { status: "pass", passCount: 5, failCount: 0 } },
      trials: [{ scenarioId: "s-1", trials: [{ trial: 1, status: "pass" }, { trial: 2, status: "fail", failures: ["nested trial failure the scenario summary did not reflect"] }] }],
      totals: { pass: 4, fail: 1 },
    };
    const report = aggregateReport({ manifest, staticResult, conformanceResult, behavioralResult });
    assert.equal(report.gateResult, "fail", `behavioral severity=${behavioralSeverity}`);
    assert.equal(gateExitCode(report), 1);
    assert.equal(report.gates.behavioral.status, behavioralSeverity === "hard" ? "fail" : "report-only-failing");
  }
});

// F1 (round 3): proves the scan is genuinely recursive (depth-independent),
// not two more hardcoded named fields (rows[].probes[] and
// trials[].trials[]) bolted onto the round-2 fix. A failing status buried
// three levels under an arbitrary, made-up nested shape must still surface —
// a future report shape gains this property by construction.
test("F1: a failing status buried arbitrarily deep under a passing row is never masked (generic recursion)", () => {
  const manifest = { ax: { gates: { static: "hard", conformance: "hard", behavioral: "report-only" } } };
  const staticResult = staticResultWith([]);
  const conformanceResult = {
    verdict: "pass",
    rows: [{
      tool: "deep_tool",
      status: "pass",
      nested: { level2: [{ level3: { checks: [{ status: "pass" }, { status: "fail" }] } }] },
    }],
  };
  const report = aggregateReport({ manifest, staticResult, conformanceResult, behavioralResult: null });
  assert.equal(report.gateResult, "fail");
  assert.equal(gateExitCode(report), 1);
  assert.equal(report.gates.conformance.status, "fail");
});

// Severity x status matrix: every combination of {static,conformance,
// behavioral} severity in {"hard","report-only"} with the OTHER two gates
// clean must still surface the one real failure as gateResult "fail"/exit 1
// — severity only ever changes per-gate presentation, never whether a real
// failure is visible at the top level.
test("F1: severity x status matrix — a single real failure at any severity combination is never masked", () => {
  const severities = ["hard", "report-only"];
  for (const staticSeverity of severities) {
    for (const conformanceSeverity of severities) {
      for (const behavioralSeverity of severities) {
        const manifest = { ax: { gates: { static: staticSeverity, conformance: conformanceSeverity, behavioral: behavioralSeverity } } };
        const staticResult = staticResultWith([{ ruleId: "static-1-tool-set", severity: "error", status: "fail", claim: "x", evidence: "y", remediation: "z" }]);
        const conformanceResult = { rows: [], totals: {}, verdict: "pass" };
        const behavioralResult = { provenBy: "stub", verdict: "pass", scenarioResults: {}, totals: {} };
        const report = aggregateReport({ manifest, staticResult, conformanceResult, behavioralResult });
        assert.equal(report.gateResult, "fail", `static=${staticSeverity} conformance=${conformanceSeverity} behavioral=${behavioralSeverity}`);
        assert.equal(gateExitCode(report), 1);
      }
    }
  }
});

test("F1 depth: a failing leaf nested beyond the old scan cap fails closed, never PASS", () => {
  // Build a status:fail leaf buried 70 levels deep (past the retired 64 cap).
  let node = { status: "fail" };
  for (let i = 0; i < 70; i += 1) node = { child: node };
  const report = aggregateReport({
    staticResult: staticResultWith([node]),
    gates: { static: "hard" },
  });
  assert.equal(report.gateResult, "fail");
  assert.equal(gateExitCode(report), 1);
});

test("I2: adversarial nested failure prevents pass and maps to SARIF", () => { const adversarialResult={verdict:"pass",artifactDigest:"adv",cases:[{caseId:"leak",status:"pass",nested:{status:"fail"},reasons:["canary echoed"]}]}; const report=aggregateReport({manifest:MANIFEST,staticResult:staticResultWith([]),conformanceResult:{rows:[],verdict:"pass"},behavioralResult:{provenBy:"stub",verdict:"pass"},adversarialResult}); assert.equal(report.gateResult,"fail"); assert.match(report.gates.behavioral.claim,/UNPROVEN/); assert.ok(toSarif({staticResult:staticResultWith([]),conformanceResult:{rows:[]},behavioralResult:null,adversarialResult}).runs[0].results.some(x=>x.ruleId.startsWith("adversarial-"))); });
test("I2: JSON/markdown retain all digest types", () => { const report=aggregateReport({manifest:MANIFEST,staticResult:staticResultWith([]),conformanceResult:{rows:[],verdict:"pass",artifact:{digest:"artifact"}},behavioralResult:{provenBy:"stub",verdict:"pass",matrixDigest:"matrix",scenarioDigest:"scenarios"},adversarialResult:{verdict:"pass",artifactDigest:"adversarial",cases:[]}}); assert.deepEqual(report.digests,{artifact:"artifact",matrix:"matrix",scenarios:"scenarios",adversarial:"adversarial"}); assert.match(renderMarkdown(report),/adversarial/); });

test("EC-AX-F2: no executed gates is fatal in JSON, markdown, SARIF, and exit code", () => {
  const report = aggregateReport({ manifest: MANIFEST, staticResult: null, conformanceResult: null, behavioralResult: null, adversarialResult: null });
  assert.equal(report.executedGateCount, 0);
  assert.deepEqual(report.fatal, { code: "no_gates_executed", message: "report contains no executed gates" });
  assert.equal(gateExitCode(report), 1);
  assert.match(renderMarkdown(report), /report contains no executed gates/);
  const sarif = toSarif({});
  assert.deepEqual(sarif.runs[0].results.map((result) => [result.ruleId, result.level]), [["report-no-gates-executed", "error"]]);
});

test("EC-AX-F2: matrix-only evidence does not invent an artifact digest", () => {
  const report = aggregateReport({ manifest: MANIFEST, staticResult: null, conformanceResult: null, behavioralResult: { provenBy: "real-matrix", verdict: "pass", matrixDigest: "matrix" }, adversarialResult: null });
  assert.equal(report.executedGateCount, 1);
  assert.equal(report.fatal, null);
  assert.equal(report.digests.artifact, null);
  assert.equal(gateExitCode(report), 0);
});

test("EC-AX-F2: matching executed artifact digests pass", () => {
  const report = aggregateReport({ manifest: MANIFEST, staticResult: null, conformanceResult: { verdict: "pass", artifact: { digest: "shared" } }, behavioralResult: { provenBy: "real-matrix", verdict: "pass", artifactDigest: "shared" }, adversarialResult: { verdict: "pass", artifactDigest: "shared", cases: [] } });
  assert.equal(report.executedGateCount, 3);
  assert.equal(report.fatal, null);
  assert.equal(report.digests.artifact, "shared");
  assert.equal(gateExitCode(report), 0);
});

test("EC-AX-F2: mismatched executed artifact digests fail in JSON, markdown, SARIF, and exit code", () => {
  const conformanceResult = { verdict: "pass", artifact: { digest: "one" } };
  const adversarialResult = { verdict: "pass", artifactDigest: "two", cases: [] };
  const report = aggregateReport({ manifest: MANIFEST, staticResult: null, conformanceResult, behavioralResult: null, adversarialResult });
  assert.equal(report.executedGateCount, 2);
  assert.deepEqual(report.fatal, { code: "artifact_digest_mismatch", message: "executed artifact-bound gates reference different artifacts" });
  assert.equal(gateExitCode(report), 1);
  assert.match(renderMarkdown(report), /executed artifact-bound gates reference different artifacts/);
  const sarif = toSarif({ conformanceResult, adversarialResult });
  assert.deepEqual(sarif.runs[0].results.map((result) => [result.ruleId, result.level]), [["report-artifact-digest-mismatch", "error"]]);
});
