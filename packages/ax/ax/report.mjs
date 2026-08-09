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
//
// F1 (round 2) fix: a gate result's own `verdict` field is a SIGNAL, never
// the sole SOURCE of truth. The previous fix computed failure from each
// gate's top-level `verdict` alone — so a hand-built (or bugged-upstream)
// gate object like `{ verdict: "pass", rules: [{ status: "fail" }] }` still
// read as a passing gate here, because nothing ever looked at `rules`
// itself. Each gate now ALSO scans its own leaf findings directly
// (staticResult.rules[].status, conformanceResult.rows[].status,
// behavioralResult.scenarioResults{}.status, and any `.fatal`) and fails if
// EITHER the leaf scan OR the verdict field says fail. A caller cannot flip
// a real leaf failure to PASS by forging/miscomputing the verdict field,
// because the leaf scan is unconditional; a caller that supplies verdict
// alone with no leaf array (e.g. this file's own unit tests, or a future
// gate module with a different internal shape) still gets an honest result
// from that field.
//
// F1 (round 3) fix: the round-2 scan only checked ONE level of leaf —
// rows[].status itself, scenarioResults{}.status itself — so a row/scenario
// whose OWN summary said "pass" while something nested underneath it
// (rows[].probes[].status from ax/conformance.mjs, or the
// trials[].trials[].status entries ax/behavioral.mjs records alongside
// scenarioResults) said "fail" still read as a passing gate: nothing looked
// past the first `.status` field. The scan is now fully recursive — it walks
// every object/array under the gate's leaf root and fails on ANY nested
// `.status === "fail"` found at ANY depth, not just the outermost summary.
// This makes the property depth-independent: a future nesting level added to
// either module's report shape is covered by construction, not by adding
// another named field here.
// Bound only to stop pathological/cyclic structures, never to bless deep input.
// At the cap we cannot prove the remaining subtree is clean, so we FAIL CLOSED
// (return true = "a failure may exist here") — the package never claims pass on
// evidence it did not inspect. A real report is a handful of levels deep; a node
// nested past this cap is itself anomalous and correctly treated as unverifiable.
const MAX_LEAF_SCAN_DEPTH = 256;
function anyNestedStatusFails(node, depth = 0) {
  if (node === null || typeof node !== "object") return false;
  if (depth > MAX_LEAF_SCAN_DEPTH) return true;
  if (Array.isArray(node)) return node.some((item) => anyNestedStatusFails(item, depth + 1));
  if (node.status === "fail") return true;
  return Object.values(node).some((value) => anyNestedStatusFails(value, depth + 1));
}

const SARIF_LEVEL = { error: "error", warning: "warning", info: "note" };

function reportIntegrity({ staticResult, conformanceResult, behavioralResult, adversarialResult }) {
  const executedGateCount = [staticResult, conformanceResult, behavioralResult, adversarialResult]
    .filter(Boolean)
    .length;
  const artifactDigests = [
    conformanceResult?.artifact?.digest,
    behavioralResult?.artifactDigest,
    adversarialResult?.artifactDigest,
  ].filter((digest) => typeof digest === "string" && digest.trim() !== "");
  const uniqueArtifactDigests = [...new Set(artifactDigests)];
  const fatal = executedGateCount === 0
    ? { code: "no_gates_executed", message: "report contains no executed gates" }
    : uniqueArtifactDigests.length > 1
      ? { code: "artifact_digest_mismatch", message: "executed artifact-bound gates reference different artifacts" }
      : null;
  return { executedGateCount, artifactDigest: uniqueArtifactDigests[0] ?? null, fatal };
}

// CLAIM-FIDELITY fix: toSarif previously only read staticResult, so a run
// failing on CONFORMANCE or BEHAVIORAL (with static clean) emitted a SARIF
// doc with zero results — reading as CLEAN while `gateResult` was "fail".
// For a package whose entire purpose is claim fidelity, a SARIF artifact
// that can disagree with the gate it describes is unacceptable. toSarif now
// takes the raw per-gate results (the same three the CLI already has,
// pre-aggregation) and emits a SARIF result for every failing static rule,
// every failing conformance row/probe, and every failing behavioral
// scenario/trial. A fail-closed fallback (below) additionally guarantees
// the invariant holds even for gate shapes this mapping does not
// specifically recognize: SARIF `results` emptiness is equivalent to
// `gateResult === "pass"`, always — never just "usually".
export function toSarif({ staticResult, conformanceResult, behavioralResult, adversarialResult } = {}, toolName = "right-ax") {
  const rules = new Map();
  const results = [];

  function addRule(id, claim) {
    if (!rules.has(id)) rules.set(id, { id, shortDescription: { text: claim } });
  }
  function pushResult(ruleId, claim, severity, evidenceText) {
    addRule(ruleId, claim);
    results.push({
      ruleId,
      level: SARIF_LEVEL[severity] ?? "warning",
      message: { text: `${claim} — ${evidenceText ?? ""}`.trim() },
      locations: [{ physicalLocation: { artifactLocation: { uri: "." } } }],
    });
  }

  // Static gate: one result per failing rule (unchanged behavior).
  for (const finding of staticResult?.rules ?? []) {
    addRule(finding.ruleId, finding.claim);
    if (finding.status !== "fail") continue;
    pushResult(finding.ruleId, finding.claim, finding.severity, finding.evidence);
  }

  // Conformance gate: one result per failing row (a row fails if its own
  // status says so, or any probe/check nested under it does).
  for (const row of conformanceResult?.rows ?? []) {
    const rowFailed = row.status === "fail" || anyNestedStatusFails(row.probes) || anyNestedStatusFails(row.checks);
    if (!rowFailed) continue;
    const toolLabel = row.tool ?? "unknown-tool";
    const failingProbes = (row.probes ?? []).filter((probe) => probe.status === "fail");
    const evidence = failingProbes.length
      ? failingProbes.map((probe) => `${probe.kind}: ${probe.detail ?? probe.sample ?? "failed"}`).join("; ")
      : "conformance row failed";
    pushResult(`conformance-${toolLabel}`, `conformance probe: ${toolLabel}`, "error", evidence);
  }

  // Behavioral gate: one result per failing scenario summary, plus one per
  // failing trial (a trial can fail while its scenario summary — computed
  // separately — has not yet been read, so both leaf roots are scanned).
  for (const [scenarioId, scenarioResult] of Object.entries(behavioralResult?.scenarioResults ?? {})) {
    if (scenarioResult?.status !== "fail") continue;
    pushResult(`behavioral-${scenarioId}`, `behavioral scenario: ${scenarioId}`, "error", `${scenarioResult.failCount ?? "?"} failing trial(s)`);
  }
  for (const trialGroup of behavioralResult?.trials ?? []) {
    const scenarioId = trialGroup?.scenarioId ?? "unknown-scenario";
    for (const trial of trialGroup?.trials ?? []) {
      if (trial?.status !== "fail") continue;
      const evidence = Array.isArray(trial.failures) && trial.failures.length ? trial.failures.join("; ") : "trial failed";
      pushResult(`behavioral-${scenarioId}-trial-${trial.trial ?? "?"}`, `behavioral trial: ${scenarioId}`, "error", evidence);
    }
  }

  // Fail-closed fallback: recompute each gate's own fail/pass the same way
  // aggregateReport does (isFailing + the recursive leaf scan), independent
  // of the specific-shape mapping above. If a gate is failing but nothing
  // was pushed for it — an unrecognized/future result shape, a synthetic
  // test fixture, a `.fatal` with no leaf array — emit one generic result
  // for that gate rather than let the SARIF doc go clean while the gate
  // did not. This is what keeps the invariant true by construction instead
  // of by enumeration.
  const staticFailed = isFailing(staticResult, staticLeafFailed);
  const conformanceFailed = isFailing(conformanceResult, conformanceLeafFailed);
  const behavioralFailed = isFailing(behavioralResult, behavioralLeafFailed);
  const mappedStatic = results.some((result) => result.ruleId.startsWith("static") || staticResult?.rules?.some((rule) => rule.ruleId === result.ruleId));
  if (staticFailed && !mappedStatic) {
    pushResult("static-gate-failed", "static gate failed", "error", "static gate reported a failure not captured by per-rule mapping");
  }
  if (conformanceFailed && !results.some((result) => result.ruleId.startsWith("conformance-"))) {
    pushResult("conformance-gate-failed", "conformance gate failed", "error", "conformance gate reported a failure not captured by per-row mapping");
  }
  if (behavioralFailed && !results.some((result) => result.ruleId.startsWith("behavioral-"))) {
    pushResult("behavioral-gate-failed", "behavioral gate failed", "error", "behavioral gate reported a failure not captured by per-scenario mapping");
  }
  for (const item of adversarialResult?.cases ?? []) if (item.status === "fail") pushResult(`adversarial-${item.caseId ?? "case"}`, "adversarial case", "error", (item.reasons ?? []).join("; "));
  if (isFailing(adversarialResult, result => anyNestedStatusFails(result?.cases)) && !results.some(result => result.ruleId.startsWith("adversarial-"))) pushResult("adversarial-gate-failed", "adversarial gate failed", "error", "adversarial gate reported a failure");
  const integrity = reportIntegrity({ staticResult, conformanceResult, behavioralResult, adversarialResult });
  if (integrity.fatal) {
    const ruleId = integrity.fatal.code === "no_gates_executed" ? "report-no-gates-executed" : "report-artifact-digest-mismatch";
    pushResult(ruleId, integrity.fatal.code, "error", integrity.fatal.message);
  }

  return {
    $schema: "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/Schemata/sarif-schema-2.1.0.json",
    version: "2.1.0",
    runs: [{ tool: { driver: { name: toolName, informationUri: "https://github.com/adrdsouza/claude", rules: [...rules.values()] } }, results }],
  };
}

// F1: never trust a single precomputed field — a result fails when EITHER
// its own `.fatal`/`.verdict` says fail OR the caller-supplied leaf-status
// scanner (`leafFailed`) finds a failing leaf. `leafFailed` is a function so
// each gate type can scan its own leaf shape (see the *LeafFailed functions
// below); it returns false, never throws, when the expected leaf array is
// absent so a minimal/summary-only result object still resolves from
// `.verdict` alone.
function isFailing(result, leafFailed) {
  if (!result) return false;
  return Boolean(result.fatal) || result.verdict === "fail" || Boolean(leafFailed(result));
}

// F1 (round 3): each leaf scan now recurses into the FULL subtree rooted at
// its gate's finding array/map — not just the top status field of each
// direct item — so a failing probe nested under a pass-looking row, or a
// failing trial nested under a pass-looking scenario, is still caught. See
// anyNestedStatusFails above.
function staticLeafFailed(result) {
  return anyNestedStatusFails(result?.rules);
}
function conformanceLeafFailed(result) {
  return anyNestedStatusFails(result?.rows);
}
function behavioralLeafFailed(result) {
  // Two independent leaf roots: scenarioResults{} (the per-scenario summary,
  // itself possibly holding nested detail) and trials[] (the actual
  // per-trial records — trials[].trials[].status — which is where a real
  // trial failure lives even when the scenario summary next to it says
  // "pass").
  return anyNestedStatusFails(result?.scenarioResults ? Object.values(result.scenarioResults) : []) || anyNestedStatusFails(result?.trials);
}

// F1: status is severity-aware for presentation, but the underlying failure
// (`failed`) is never hidden by "report-only" — a report-only gate that is
// actually failing renders "report-only-failing", never a bare "report-only"
// or "pass".
function gateStatus(result, severity, failed) {
  if (!result) return "not-run";
  if (severity === "report-only") return failed ? "report-only-failing" : "report-only";
  return failed ? "fail" : "pass";
}

export function aggregateReport({ manifest, staticResult, conformanceResult, behavioralResult, adversarialResult }) {
  const gates = manifest?.ax?.gates ?? { static: "hard", conformance: "hard", behavioral: "report-only", adversarial: "report-only" };
  const staticFailed = isFailing(staticResult, staticLeafFailed);
  const conformanceFailed = isFailing(conformanceResult, conformanceLeafFailed);
  const behavioralFailed = isFailing(behavioralResult, behavioralLeafFailed);
  const adversarialFailed = isFailing(adversarialResult, result => anyNestedStatusFails(result?.cases));
  const staticStatus = gateStatus(staticResult, gates.static, staticFailed);
  const conformanceStatus = gateStatus(conformanceResult, gates.conformance, conformanceFailed);
  const behavioralProvenBy = behavioralResult?.provenBy ?? "stub";
  const behavioralStatus = gateStatus(behavioralResult, gates.behavioral, behavioralFailed);
  const integrity = reportIntegrity({ staticResult, conformanceResult, behavioralResult, adversarialResult });

  // F1: the top-level result must never read PASS while any rule or probe
  // failed, at any severity — severity only changes per-gate presentation
  // (see gateStatus), never whether a real failure is visible here. Each of
  // staticFailed/conformanceFailed/behavioralFailed already combines its
  // gate's own leaf scan with its verdict/fatal fields (see isFailing), so a
  // contradictory or forged intermediate (e.g. `verdict: "pass"` alongside a
  // failing leaf rule) cannot flip this to "pass".
  const anyFailure = staticFailed || conformanceFailed || behavioralFailed || adversarialFailed || Boolean(integrity.fatal);

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
      adversarial: { severity: gates.adversarial ?? "report-only", status: adversarialResult ? gateStatus(adversarialResult, gates.adversarial ?? "report-only", adversarialFailed) : "not-run", caseCount: adversarialResult?.cases?.length ?? 0, digest: adversarialResult?.artifactDigest ?? null },
    },
    // gateResult, never "verdict" or "signoff": right-ax gates and reports,
    // it never certifies (I1). Nemesis/Seer own any clean/sign-off claim.
    // Its value is a factual "did anything fail" statement (see anyFailure
    // above), not a severity-weighted CI-blocking decision — a caller that
    // wants to block only on hard gates reads gates.<name>.severity/status
    // directly instead of this field.
    gateResult: anyFailure ? "fail" : "pass",
    signoff: null,
    fatal: integrity.fatal,
    executedGateCount: integrity.executedGateCount,
    digests: { artifact: integrity.artifactDigest, matrix: behavioralResult?.matrixDigest ?? null, scenarios: behavioralResult?.scenarioDigest ?? null, adversarial: adversarialResult?.artifactDigest ?? null },
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
    ...(report.fatal ? [`- Fatal: **${report.fatal.code}** — ${report.fatal.message}`] : []),
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
    `| adversarial | ${report.gates.adversarial.severity} | ${report.gates.adversarial.status} | ${report.gates.adversarial.caseCount} cases |`,
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
