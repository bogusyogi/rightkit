// Phase D (AXA-1): behavioral scenario evaluation, generalized from Cortex's
// evals/ax/run-behavioral.mjs. Scenarios are JSON-compatible YAML (parsed as
// JSON, no YAML dependency — matching Cortex's own approach) loaded from the
// per-repo scenariosDir named in .rightax.json.
//
// Generalization from Cortex's engine (I3: engine here, evidence in the
// product repo): Cortex's stub driver used a switch over Cortex-specific
// scenario "branches" — that switch WAS the product-specific coupling. Here
// each scenario embeds its own scripted stub trial
// (fixture.stub.trial = {operations, reasonCodes, finalEnvironment, answer}),
// so the shared engine needs no per-product branch knowledge; it only runs
// the trial and verifies the final environment before admitting any claim,
// exactly like Cortex's runner did.
//
// Invariant I2 (enforced by construction, not just banner prose): the stub
// driver can prove the harness only. runScenarios() always sets
// result.provenBy = "stub" unless a real multi-agent matrix executed: driver
// !== "stub", AND every scenario was attempted by >=2 distinct agentIds,
// AND every one of those agentIds completed >=5 trials OF THAT SCENARIO
// (F2 fix — this is a per-scenario, per-agent count, not a total-trials
// count: two agents splitting k=5 trials 3-and-2 is not a matrix, because
// neither agent individually cleared the bar; a genuine 2-agent matrix needs
// k>=10 for that scenario, 5 per agent). report.mjs refuses to render a
// "pass"/"clean" verdict for behavioral results while provenBy is "stub" —
// it renders "unproven" regardless of trial outcomes.

import { existsSync, readFileSync, readdirSync } from "node:fs";
import { basename, join } from "node:path";

export function listScenarioFiles(scenariosDir) {
  if (!existsSync(scenariosDir)) return [];
  return readdirSync(scenariosDir).filter((name) => (name.endsWith(".yaml") || name.endsWith(".yml")) && !name.startsWith("_")).sort();
}

export function loadScenarios(scenariosDir) {
  const scenarios = [];
  const loadErrors = [];
  for (const name of listScenarioFiles(scenariosDir)) {
    const path = join(scenariosDir, name);
    try {
      scenarios.push({ ...JSON.parse(readFileSync(path, "utf8")), _source: basename(path) });
    } catch (error) {
      loadErrors.push({ file: name, message: String(error?.message ?? error) });
    }
  }
  return { scenarios, loadErrors };
}

function deepEqual(left, right) {
  if (left === right) return true;
  if (typeof left !== typeof right) return false;
  if (left === null || right === null) return false;
  if (Array.isArray(left) && Array.isArray(right)) return left.length === right.length && left.every((item, index) => deepEqual(item, right[index]));
  if (typeof left === "object" && typeof right === "object") {
    const leftKeys = Object.keys(left).sort();
    const rightKeys = Object.keys(right).sort();
    return leftKeys.length === rightKeys.length && leftKeys.every((key, index) => rightKeys[index] === key && deepEqual(left[key], right[key]));
  }
  return false;
}

function verifyFinalEnvironment(expected, actual) {
  const mismatches = [];
  const actualKeys = new Set(Object.keys(actual ?? {}));
  for (const [key, value] of Object.entries(expected ?? {})) {
    if (!actualKeys.has(key)) { mismatches.push(`finalEnvironment missing key ${key}`); continue; }
    if (!deepEqual(actual[key], value)) mismatches.push(`finalEnvironment.${key} mismatch: expected ${JSON.stringify(value)}, got ${JSON.stringify(actual[key])}`);
  }
  return { ok: mismatches.length === 0, mismatches };
}

// Order matters: environment is verified BEFORE any claim is admitted. If the
// environment does not match, claims are never evaluated.
export function evaluateTrial(scenario, trial) {
  const failures = [];
  const expected = scenario.expected ?? {};
  const env = verifyFinalEnvironment(expected.finalEnvironment ?? {}, trial.finalEnvironment);
  if (!env.ok) return { status: "fail", envVerified: false, claimsChecked: false, failures: [`final environment mismatch (verified before claims): ${env.mismatches.join("; ")}`] };

  for (const claim of expected.requiredClaims ?? []) if (!String(trial.answer ?? "").includes(claim)) failures.push(`required claim missing from final response: ${claim}`);
  for (const claim of expected.forbiddenClaims ?? []) if (String(trial.answer ?? "").includes(claim)) failures.push(`forbidden claim present in final response: ${claim}`);
  for (const code of expected.requiredReasonCodes ?? []) if (!(trial.reasonCodes ?? []).includes(code)) failures.push(`required reason code missing: ${code}`);
  for (const code of expected.forbiddenReasonCodes ?? []) if ((trial.reasonCodes ?? []).includes(code)) failures.push(`forbidden reason code present: ${code}`);
  for (const op of expected.requiredOperations ?? []) if (!(trial.operations ?? []).includes(op)) failures.push(`required operation missing: ${op}`);
  for (const op of trial.operations ?? []) if (!(scenario.allowedOperations ?? []).includes(op)) failures.push(`operation outside allowedOperations: ${op}`);
  for (const op of scenario.forbiddenOperations ?? []) if ((trial.operations ?? []).includes(op)) failures.push(`forbidden operation executed: ${op}`);

  const budgets = scenario.budgets ?? {};
  if (typeof budgets.maxToolCalls === "number" && (trial.operations ?? []).length > budgets.maxToolCalls) failures.push(`tool call budget exceeded: ${trial.operations.length} > ${budgets.maxToolCalls}`);
  if (typeof budgets.maxInvalidCalls === "number" && (trial.invalidCalls ?? 0) > budgets.maxInvalidCalls) failures.push(`invalid call budget exceeded: ${trial.invalidCalls} > ${budgets.maxInvalidCalls}`);
  if (typeof budgets.maxContextTokens === "number" && (trial.contextTokens ?? 0) > budgets.maxContextTokens) failures.push(`context token budget exceeded: ${trial.contextTokens} > ${budgets.maxContextTokens}`);

  return { status: failures.length === 0 ? "pass" : "fail", envVerified: true, claimsChecked: true, failures };
}

// Data-driven stub trial: the scenario declares exactly what the scripted
// agent "did" and its final answer. This is harness validation only — see
// the module banner. Returns null when the scenario has no stub fixture.
export function stubTrial(scenario) {
  const stub = scenario.fixture?.stub?.trial;
  if (!stub || typeof stub !== "object") return null;
  return {
    driver: "stub",
    operations: Array.isArray(stub.operations) ? stub.operations : [],
    reasonCodes: Array.isArray(stub.reasonCodes) ? stub.reasonCodes : [],
    finalEnvironment: stub.finalEnvironment ?? {},
    answer: String(stub.answer ?? ""),
    contextTokens: Number.isFinite(stub.contextTokens) ? stub.contextTokens : 0,
    invalidCalls: Number.isFinite(stub.invalidCalls) ? stub.invalidCalls : 0,
    durationMs: 40 + (Array.isArray(stub.operations) ? stub.operations.length : 0) * 8,
  };
}

const MIN_TRIALS_PER_AGENT = 5;
const MIN_DISTINCT_AGENTS = 2;

// F2: counts trials per distinct agentId within a single scenario's own
// trial list (never summed across scenarios or across agents) — the exact
// count the I2 real-matrix bar is defined against.
function countTrialsByAgent(trials) {
  const counts = new Map();
  for (const trial of trials) {
    if (!trial.agentId) continue;
    counts.set(trial.agentId, (counts.get(trial.agentId) ?? 0) + 1);
  }
  return counts;
}

// F2: a scenario clears the real-matrix bar only when >=2 distinct agents
// each individually completed >=5 trials of THIS scenario — an uneven split
// (e.g. 3 trials for agent-a, 2 for agent-b) does not count, even though the
// total (5) meets the old, incorrect "k >= 5" check.
function scenarioAgentCounts(trials) {
  return Object.fromEntries(countTrialsByAgent(trials));
}
function scenarioMeetsRealMatrixBar(trials) {
  const counts = countTrialsByAgent(trials);
  if (counts.size < MIN_DISTINCT_AGENTS) return false;
  return [...counts.values()].every((count) => count >= MIN_TRIALS_PER_AGENT);
}

// Real-agent driver: shells out to an externally supplied runner (never the
// bundled stub). `runTrial` is caller-injected — e.g. wired to `claude -p`
// or another CLI — so this package never hardcodes a provider. Distinct
// agent identities are tracked via `agentId` for the I2 real-matrix check.
export async function runScenarios({ scenariosDir, k = 3, driver = "stub", agents = [], runTrial }) {
  const { scenarios, loadErrors } = loadScenarios(scenariosDir);
  const report = { schemaVersion: 1, gate: "behavioral", driver, k, agents, scenarioCount: scenarios.length, scenarioIds: scenarios.map((scenario) => scenario.scenarioId).sort(), trials: [], scenarioResults: {}, totals: { pass: 0, fail: 0, "skipped-pending": 0 }, loadErrors, fatal: null, verdict: "fail", provenBy: driver === "stub" ? "stub" : "real-matrix-claimed" };

  if (loadErrors.length) report.fatal = { code: "scenario_load_errors", message: `${loadErrors.length} scenario file(s) failed to parse` };
  else if (scenarios.length === 0) report.fatal = { code: "no_scenarios", message: `no scenario files found under ${scenariosDir}` };

  if (!report.fatal) {
    for (const scenario of scenarios) {
      const trials = [];
      for (let trialIndex = 0; trialIndex < k; trialIndex += 1) {
        const raw = driver === "stub" ? stubTrial(scenario) : await runTrial?.(scenario, { trialIndex, driver });
        if (!raw) { trials.push({ trial: trialIndex + 1, status: "fail", envVerified: false, claimsChecked: false, failures: [`${driver} driver produced no trial for scenario ${scenario.scenarioId}`] }); continue; }
        const evaluated = evaluateTrial(scenario, raw);
        trials.push({ trial: trialIndex + 1, status: evaluated.status, envVerified: evaluated.envVerified, claimsChecked: evaluated.claimsChecked, failures: evaluated.failures, operations: raw.operations, reasonCodes: raw.reasonCodes, finalEnvironment: raw.finalEnvironment, agentId: raw.agentId ?? null });
      }
      report.trials.push({ scenarioId: scenario.scenarioId, trials });
      const passCount = trials.filter((trial) => trial.status === "pass").length;
      const failCount = trials.filter((trial) => trial.status === "fail").length;
      // F11: zero trials (e.g. k=0, or a driver that produced nothing) must
      // never vacuously report "pass" — no trials is no evidence, not a
      // clean result.
      const noTrials = trials.length === 0;
      report.scenarioResults[scenario.scenarioId] = { status: !noTrials && failCount === 0 ? "pass" : "fail", passCount, failCount, noTrials, agentTrialCounts: scenarioAgentCounts(trials) };
    }
  }

  for (const entry of report.trials) for (const trial of entry.trials) report.totals[trial.status] = (report.totals[trial.status] ?? 0) + 1;

  // F2: a real matrix requires every scenario to individually clear the
  // per-agent bar (>=2 distinct agents, >=5 trials each, for THAT scenario).
  // report.trials is empty when report.fatal is set, so `every` is
  // vacuously true in that case — the `!report.fatal` guard below is load-
  // bearing and must stay.
  const realMatrix = driver !== "stub" && !report.fatal && report.trials.length > 0 && report.trials.every((entry) => scenarioMeetsRealMatrixBar(entry.trials));
  report.provenBy = realMatrix ? "real-matrix" : "stub";
  report.verdict = !report.fatal && Object.values(report.scenarioResults).every((result) => result.status !== "fail") ? "pass" : "fail";
  return report;
}
