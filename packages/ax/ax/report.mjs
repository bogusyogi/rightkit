// Aggregate scorecard: JSON + SARIF 2.1.0 + markdown, with per-gate exit
// codes. Invariant I1: right-ax never signs off — the top-level field is
// named `gateResult` (values "pass"/"fail"), never "verdict" or "signoff".
// F1 fix: `gateResult` reflects whether ANY rule or probe failed at ANY
// gate, regardless of that gate's configured severity. A "report-only"
// severity means a failing gate does not, by itself, change how a caller
// wires CI exit-code enforcement for THAT gate — it never means the overall
// report is allowed to claim PASS while a rule is failing. Per-gate `status`
// distinguishes "report-only" (passing, non-blocking) from
// "report-only-failing" (failing, still visible) so severity configuration
// is legible without ever being able to hide a real failure behind it. The
// behavioral section is hard-pinned to "unproven" language whenever
// provenBy !== "real-matrix" (invariant I2), regardless of trial pass/fail.

const SARIF_LEVEL = { error: "error", warning: "warning", info: "note" };

export function toSarif(staticResult, toolName = "right-ax") {
  const rules = new Map();
  const results = [];
  for (const finding of staticResult?.rules ?? []) {
    if (!rules.has(finding.ruleId)) rules.set(finding.ruleId, { id: finding.ruleId, shortDescription: { text: finding.claim } });
    if (finding.status !== "fail") continue;
    results.push({
      ruleId: finding.ruleId,
      level: SARIF_LEVEL[finding.severity] ?? "warning",
      message: { text: `${finding.claim} — ${finding.evidence ?? ""}`.trim() },
      locations: [{ physicalLocation: { artifactLocation: { uri: "." } } }],
    });
  }
  return {
    $schema: "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/Schemata/sarif-schema-2.1.0.json",
    version: "2.1.0",
    runs: [{ tool: { driver: { name: toolName, informationUri: "https://github.com/adrdsouza/claude", rules: [...rules.values()] } }, results }],
  };
}

function isFailing(result) {
  return Boolean(result) && result.verdict !== "pass";
}

// F1: status is severity-aware for presentation, but the underlying failure
// (isFailing) is never hidden by "report-only" — a report-only gate that is
// actually failing renders "report-only-failing", never a bare "report-only"
// or "pass".
function gateStatus(result, severity) {
  if (!result) return "not-run";
  const failed = isFailing(result);
  if (severity === "report-only") return failed ? "report-only-failing" : "report-only";
  return failed ? "fail" : "pass";
}

export function aggregateReport({ manifest, staticResult, conformanceResult, behavioralResult }) {
  const gates = manifest?.ax?.gates ?? { static: "hard", conformance: "hard", behavioral: "report-only" };
  const staticStatus = gateStatus(staticResult, gates.static);
  const conformanceStatus = gateStatus(conformanceResult, gates.conformance);
  const behavioralProvenBy = behavioralResult?.provenBy ?? "stub";
  const behavioralStatus = gateStatus(behavioralResult, gates.behavioral);

  // F1: the top-level result must never read PASS while any rule or probe
  // failed, at any severity — severity only changes per-gate presentation
  // (see gateStatus), never whether a real failure is visible here.
  const anyFailure = isFailing(staticResult) || isFailing(conformanceResult) || isFailing(behavioralResult);

  const report = {
    schemaVersion: 1,
    generatedAt: new Date().toISOString(),
    gates: {
      static: { severity: gates.static, status: staticStatus, ruleCount: staticResult?.rules?.length ?? 0, failCount: (staticResult?.rules ?? []).filter((rule) => rule.status === "fail").length },
      conformance: { severity: gates.conformance, status: conformanceStatus, rowCount: conformanceResult?.rows?.length ?? 0, totals: conformanceResult?.totals ?? null },
      behavioral: {
        severity: gates.behavioral,
        status: behavioralResult ? behavioralStatus : "not-run",
        provenBy: behavioralProvenBy,
        scenarioCount: behavioralResult?.scenarioCount ?? 0,
        totals: behavioralResult?.totals ?? null,
        // I1/I2: never "pass" or "clean" language here — always unproven
        // unless a real multi-agent matrix produced this result.
        claim: behavioralProvenBy === "real-matrix"
          ? "real-matrix executed; see retirement-condition fields for release-threshold review"
          : "UNPROVEN — the stub driver proves harness plumbing only, never real-agent AX reliability",
      },
    },
    // gateResult, never "verdict" or "signoff": right-ax gates and reports,
    // it never certifies (I1). Nemesis/Seer own any clean/sign-off claim.
    // Its value is a factual "did anything fail" statement (see anyFailure
    // above), not a severity-weighted CI-blocking decision — a caller that
    // wants to block only on hard gates reads gates.<name>.severity/status
    // directly instead of this field.
    gateResult: anyFailure ? "fail" : "pass",
    signoff: null,
  };
  return report;
}

function markdownEscape(text) {
  return String(text ?? "").replace(/\|/g, "\\|").replace(/\r?\n/g, " ");
}

export function renderMarkdown(report) {
  const lines = [
    "# right-ax scorecard",
    "",
    `- Generated: ${report.generatedAt}`,
    `- Gate result: **${report.gateResult.toUpperCase()}**`,
    "",
    "right-ax gates and reports; it never signs off. Verdict authority stays with Nemesis/Seer.",
    "",
    "## Gates",
    "",
    "| gate | severity | status | detail |",
    "|---|---|---|---|",
    `| static | ${report.gates.static.severity} | ${report.gates.static.status} | ${report.gates.static.failCount}/${report.gates.static.ruleCount} rules failing |`,
    `| conformance | ${report.gates.conformance.severity} | ${report.gates.conformance.status} | ${report.gates.conformance.rowCount} tools probed |`,
    `| behavioral | ${report.gates.behavioral.severity} | ${report.gates.behavioral.status} | ${markdownEscape(report.gates.behavioral.claim)} |`,
    "",
  ];
  if (report.gates.behavioral.provenBy !== "real-matrix") {
    lines.push(
      "## UNPROVEN",
      "",
      "Behavioral AX is formally UNPROVEN. A stub-driver pass proves only that the",
      "harness runs scenarios and verifies the final environment before admitting a",
      "claim — it does not prove a real agent achieves the scenario goals. The",
      "banner is removed only when a real multi-agent matrix produced this",
      "report: every scenario attempted by >=2 distinct agents, with EACH of",
      "those agents individually completing >=5 trials of that scenario (an",
      "uneven split, e.g. 3 trials for one agent and 2 for another, does not",
      "qualify even though the total reaches 5).",
      "",
    );
  }
  return lines.join("\n");
}

export function gateExitCode(report) {
  return report.gateResult === "pass" ? 0 : 1;
}
