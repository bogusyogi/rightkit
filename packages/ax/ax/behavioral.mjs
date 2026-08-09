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
import { createHash } from "node:crypto";
import { basename, join } from "node:path";
import { runAdapter } from "./adapter.mjs";
import { resolveArtifact, verifyArtifactUnchanged } from "./artifact.mjs";

function canonicalJson(value) {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value && typeof value === "object") return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(",")}}`;
  return JSON.stringify(value);
}

function digestValue(value) { return createHash("sha256").update(canonicalJson(value)).digest("hex"); }
function executionFingerprint(adapter) { return digestValue({ model: adapter.model.normalize("NFKC").trim(), version: adapter.version.normalize("NFKC").trim(), command: adapter.command.normalize("NFKC").trim(), args: adapter.args }); }

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

// F2 (round 2): identity is normalized — trimmed, casefolded, and internal
// whitespace collapsed — BEFORE it is used as a counting key, anywhere. This
// alone fixes trivially-spoofable "distinct" identities: "agent-a",
// " agent-a ", "Agent-A", and "agent-a\t" are all the same agent and must
// collapse to one bucket, not count as up to four.
//
// F2 (round 3): case/whitespace normalization alone still let two Unicode
// strings that render as, and are canonically equivalent to, the same
// identity count as distinct agents — e.g. ASCII "agent-a" versus a fullwidth
// "ａｇｅｎｔ-ａ" alias survive casefold/trim untouched because they are
// different code points, not different case/whitespace. Unicode NFKC
// normalization (String.prototype.normalize) is applied FIRST, before
// casefold/trim/whitespace-collapse, so canonical-equivalent forms (fullwidth
// variants, compatibility ligatures, etc.) collapse to the same normalized
// key exactly like a plain casing/whitespace alias does.
function normalizeAgentId(id) {
  if (typeof id !== "string") return null;
  const normalized = id.normalize("NFKC").trim().toLowerCase().replace(/\s+/g, " ");
  return normalized.length ? normalized : null;
}

// F2: counts trials per distinct (normalized) agentId within a single
// scenario's own trial list (never summed across scenarios or across
// agents). This is DISPLAY data (report.scenarioResults[id].agentTrialCounts)
// for debugging/visibility — it merges trivial aliases but is intentionally
// NOT itself the real-matrix gate; see scenarioMeetsRealMatrixBar, which
// additionally requires the caller's declared roster.
function countTrialsByAgent(trials) {
  const counts = new Map();
  for (const trial of trials) {
    const normalized = normalizeAgentId(trial.agentId);
    if (!normalized) continue;
    counts.set(normalized, (counts.get(normalized) ?? 0) + 1);
  }
  return counts;
}
function scenarioAgentCounts(trials) {
  return Object.fromEntries(countTrialsByAgent(trials));
}

// F2: builds the declared-roster set a real-matrix claim is checked against.
// Normalization matches normalizeAgentId so a roster entry and a trial's
// agentId referring to the same agent always compare equal regardless of
// whitespace/case/padding.
function normalizeRoster(agents) {
  const roster = new Set();
  for (const id of agents ?? []) {
    const normalized = normalizeAgentId(id);
    if (normalized) roster.add(normalized);
  }
  return roster;
}

// F2: identity honesty is a CALLER CONTRACT, not a property string analysis
// can prove. Normalizing trivial aliases (whitespace/case/padding — handled
// above) collapses accidental near-duplicates, but nothing in this module
// can distinguish a genuine second human/agent from one actor deliberately
// supplying two distinct-looking IDs it fully controls — that is not a
// string-shape problem, and this code does not pretend to solve it. Instead,
// the CALLER must explicitly declare its agent roster (the `agents` array
// passed to runScenarios), asserting "these N identifiers are N distinct
// real agents." The real-matrix bar only ever credits trials whose
// (normalized) agentId is a member of that declared, normalized roster, AND
// the roster itself must normalize to >=2 distinct entries — a caller that
// "declares" two IDs which alias to the same normalized string has not
// declared two agents. A caller that dishonestly declares two IDs it
// controls as if they were two agents has broken the contract; no code here
// can detect that, and no code here should claim to.
//
// A scenario clears the real-matrix bar only when >=2 distinct DECLARED
// agents each individually completed >=5 trials of THIS scenario — an
// uneven split (e.g. 3 trials for agent-a, 2 for agent-b) does not count,
// even though the total (5) meets the old, incorrect "k >= 5" check.
function scenarioMeetsRealMatrixBar(trials, declaredRoster) {
  if (declaredRoster.size < MIN_DISTINCT_AGENTS) return false;
  const counts = new Map();
  for (const trial of trials) {
    const normalized = normalizeAgentId(trial.agentId);
    if (!normalized || !declaredRoster.has(normalized)) continue; // undeclared identity earns no real-matrix credit
    counts.set(normalized, (counts.get(normalized) ?? 0) + 1);
  }
  if (counts.size < MIN_DISTINCT_AGENTS) return false;
  return [...counts.values()].every((count) => count >= MIN_TRIALS_PER_AGENT);
}

// F2 (round 3): scenarioMeetsRealMatrixBar already refuses to CREDIT an
// undeclared identity toward the per-agent bar, but crediting was the only
// thing it withheld — a trial run by an agentId outside the declared roster
// was otherwise silently tolerated and the matrix still passed as clean once
// the declared agents alone cleared the bar. That is not an honest matrix:
// an unexpected participant means the run mixed something the caller never
// declared into what it is now claiming as a controlled multi-agent
// comparison. Any trial (across any scenario) whose agentId is present but
// does NOT normalize into the declared roster forces the whole run out of
// real-matrix, exactly like every other way of failing to clear the bar.
function hasUndeclaredAgentTrial(trialEntries, declaredRoster) {
  for (const entry of trialEntries) {
    for (const trial of entry.trials) {
      if (trial.agentId == null) continue; // no identity claimed — nothing to compare against the roster
      const normalized = normalizeAgentId(trial.agentId);
      if (!normalized || !declaredRoster.has(normalized)) return true;
    }
  }
  return false;
}

// Real-agent driver: shells out to an externally supplied runner (never the
// bundled stub). `runTrial` is caller-injected — e.g. wired to `claude -p`
// or another CLI — so this package never hardcodes a provider. Distinct
// agent identities are tracked via `agentId` for the I2 real-matrix check;
// `agents` is the caller's explicit declared roster (F2 — see
// scenarioMeetsRealMatrixBar's banner) and is REQUIRED (>=2 distinct
// normalized entries) for provenBy to ever become "real-matrix".
export async function runScenarios({ scenariosDir, k = 3, driver = "stub", agents = [], runTrial }) {
  const declaredRoster = normalizeRoster(agents);
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
  // per-agent bar (>=2 distinct DECLARED agents, >=5 trials each, for THAT
  // scenario — see scenarioMeetsRealMatrixBar). report.trials is empty when
  // report.fatal is set, so `every` is vacuously true in that case — the
  // `!report.fatal` guard below is load-bearing and must stay.
  //
  // F2 (round 3): a trial run by an agentId outside the declared roster
  // (hasUndeclaredAgentTrial) forces the run out of real-matrix even if the
  // declared agents themselves cleared the bar — see that function's banner.
  const realMatrix = driver !== "stub" && !report.fatal && report.trials.length > 0 && !hasUndeclaredAgentTrial(report.trials, declaredRoster) && report.trials.every((entry) => scenarioMeetsRealMatrixBar(entry.trials, declaredRoster));
  report.provenBy = realMatrix ? "real-matrix" : "stub";
  report.verdict = !report.fatal && Object.values(report.scenarioResults).every((result) => result.status !== "fail") ? "pass" : "fail";
  return report;
}

export async function runMatrix({ repoPath, artifactRoot, scenariosDir, matrix }) {
  let artifact;
  try { artifact = resolveArtifact({ repoPath, artifactRoot }); }
  catch (error) { return { schemaVersion: 2, gate: "behavioral", artifact: null, scenarioResults: {}, trials: [], totals: { pass: 0, fail: 0 }, fatal: { code: "invalid_artifact", reason: error.message }, verdict: "fail", claimBoundary: "UNPROVEN" }; }
  const { scenarios, loadErrors } = loadScenarios(scenariosDir);
  const adapters = [...(matrix?.adapters ?? [])].sort((a,b)=>a.id.trim().toLowerCase().localeCompare(b.id.trim().toLowerCase())); const trialCount = matrix?.trialsPerAgent;
  const report = { schemaVersion: 2, gate: "behavioral", scenarioResults: {}, trials: [], totals: { pass: 0, fail: 0 }, loadErrors, fatal: null, verdict: "fail", claimBoundary:"UNPROVEN" };
  const identities = adapters.every((adapter) => typeof adapter.model === "string" && typeof adapter.version === "string" && typeof adapter.command === "string" && Array.isArray(adapter.args)) ? adapters.map(executionFingerprint) : [];
  const invalid=!Number.isInteger(trialCount)||trialCount<5||adapters.length<2||new Set(adapters.map(x=>x.id.trim().toLowerCase())).size!==adapters.length||new Set(identities).size!==adapters.length||adapters.some(a=>["id","model","version","command"].some(k=>typeof a[k]!=="string"||!a[k].trim())||!Array.isArray(a.args))||!matrix?.limits||!["timeoutMs","maxToolCalls","maxTokens","maxCostUsd"].every(k=>Number.isFinite(matrix.limits[k]));
  if (loadErrors.length || !scenarios.length || invalid) { report.fatal = { code: loadErrors.length?"scenario_load_failed":"matrix_invalid", reason:"invalid configuration" }; report.artifact=verifyArtifactUnchanged(artifact); report.artifactDigest=report.artifact.digest; if(!report.artifact.proof.immutable)report.fatal={code:"artifact_mutated"}; return report; }
  const scenarioEvidence = scenarios.map(({ _source, ...scenario }) => ({ source: _source, scenario })).sort((a, b) => a.scenario.scenarioId.localeCompare(b.scenario.scenarioId));
  report.scenarioCount=scenarios.length; report.scenarioIds=scenarios.map((scenario)=>scenario.scenarioId).sort(); report.scenarioDigest=digestValue(scenarioEvidence);
  report.adapters=adapters.map((adapter,index)=>({adapterId:adapter.id,executionFingerprint:identities[index]})); report.matrixDigest=digestValue({adapters:adapters.map(({id,model,version,command,args})=>({id,model,version,command,args})),trialsPerAgent:trialCount,limits:matrix.limits});
  for (const scenario of scenarios) {
    const trials = [];
    for (const adapter of adapters) for (let trialIndex = 1; trialIndex <= trialCount; trialIndex += 1) try {
      const raw = await runAdapter({ adapter, request: { schemaVersion: 1, repoRoot: repoPath, artifactRoot: artifact.root, scenario, trialIndex, agent: { id: adapter.id, model: adapter.model, version: adapter.version }, limits: matrix.limits }, timeoutMs: matrix.limits.timeoutMs });
      const result = evaluateTrial(scenario, raw); trials.push({ trial: trialIndex, agentId: raw.agentId, ...result, response: raw });
    } catch (error) { trials.push({ trial: trialIndex, agentId: adapter.id, status: "fail", envVerified: false, claimsChecked: false, failures: [error.code ?? "adapter_protocol_invalid"], error: { code:error.code ?? "adapter_protocol_invalid", details:error.details ?? {} } }); }
    const counts = Object.fromEntries(adapters.map((adapter) => [adapter.id, trials.filter((row) => row.agentId === adapter.id && row.status === "pass").length]));
    report.trials.push({ scenarioId: scenario.scenarioId, trials }); report.scenarioResults[scenario.scenarioId] = { status: Object.values(counts).every((count) => count >= trialCount) ? "pass" : "fail", agentTrialCounts: counts, failCount: trials.filter((x) => x.status !== "pass").length };
  }
  for (const group of report.trials) for (const trial of group.trials) report.totals[trial.status] = (report.totals[trial.status] ?? 0) + 1;
  const incomplete=report.trials.some(g=>g.trials.some(t=>t.error)); report.claimBoundary=incomplete?"UNPROVEN":"PROVEN"; report.provenBy=incomplete?"real-matrix-incomplete":"real-matrix"; report.artifact=verifyArtifactUnchanged(artifact); report.artifactDigest=report.artifact.digest; if(!report.artifact.proof.immutable){report.fatal={code:"artifact_mutated"};report.verdict="fail";return report;} report.verdict = Object.values(report.scenarioResults).every((x) => x.status === "pass") ? "pass" : "fail"; return report;
}
