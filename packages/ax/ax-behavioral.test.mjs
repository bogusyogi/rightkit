import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { evaluateTrial, runScenarios } from "./ax/behavioral.mjs";

function scenariosDir() {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-ax-behavioral-"));
  mkdirSync(dir, { recursive: true });
  return dir;
}

function writeScenario(dir, name, overrides = {}) {
  const scenario = {
    scenarioId: name,
    goal: "exercise the stub driver",
    allowedOperations: ["tool_a"],
    forbiddenOperations: ["tool_b"],
    expected: { finalEnvironment: { toolCalls: 1 }, requiredClaims: ["done"], forbiddenClaims: ["oops"] },
    fixture: { stub: { trial: { operations: ["tool_a"], reasonCodes: [], finalEnvironment: { toolCalls: 1 }, answer: "done" } } },
    ...overrides,
  };
  writeFileSync(path.join(dir, `${name}.yaml`), JSON.stringify(scenario));
}

test("environment is verified before claims: a mismatched final environment fails without checking claims", () => {
  const scenario = { expected: { finalEnvironment: { toolCalls: 1 }, requiredClaims: ["done"] } };
  const trial = { finalEnvironment: { toolCalls: 2 }, answer: "done", operations: [], reasonCodes: [] };
  const result = evaluateTrial(scenario, trial);
  assert.equal(result.status, "fail");
  assert.equal(result.envVerified, false);
  assert.equal(result.claimsChecked, false);
  assert.ok(result.failures[0].includes("verified before claims"));
});

test("a passing stub scenario reports pass and the stub-driver provenance", async () => {
  const dir = scenariosDir();
  writeScenario(dir, "happy-path");
  const result = await runScenarios({ scenariosDir: dir, k: 3, driver: "stub" });
  assert.equal(result.fatal, null);
  assert.equal(result.scenarioResults["happy-path"].status, "pass");
  assert.equal(result.verdict, "pass");
  assert.equal(result.provenBy, "stub");
});

test("behavioral results carry UNPROVEN (provenBy stub) even when every trial passes and driver is not the bundled stub name but no real matrix ran", async () => {
  const dir = scenariosDir();
  writeScenario(dir, "single-agent");
  // A single named agent with fewer than 5 trials does not meet the I2
  // real-matrix bar (>=2 distinct agents, >=5 trials each) even though it
  // is not the literal "stub" driver string.
  const result = await runScenarios({
    scenariosDir: dir,
    k: 3,
    driver: "claude-opus",
    runTrial: async (scenario) => ({ operations: ["tool_a"], reasonCodes: [], finalEnvironment: { toolCalls: 1 }, answer: "done", agentId: "claude-opus" }),
  });
  assert.equal(result.provenBy, "stub");
});

// NOTE: this test previously used k=5 with agentId alternating strictly
// between two agents, which (with 5 total trials) actually produces an
// uneven 3-vs-2 split — exactly the F2 defect shape. It passed before the
// fix only because the old check was `distinctAgents.size >= 2 && k >= 5`
// (total trials, not per-agent). Corrected to k=10 with a clean 5-and-5
// split, which is what "a real multi-agent matrix" is supposed to mean; the
// F2 regression test below covers the uneven-split case explicitly.
test("a real multi-agent matrix (>=2 agents, >=5 trials EACH) is provenBy real-matrix", async () => {
  const dir = scenariosDir();
  writeScenario(dir, "matrix-scenario");
  let call = 0;
  const result = await runScenarios({
    scenariosDir: dir,
    k: 10,
    driver: "multi-agent",
    runTrial: async () => {
      call += 1;
      return { operations: ["tool_a"], reasonCodes: [], finalEnvironment: { toolCalls: 1 }, answer: "done", agentId: call % 2 === 0 ? "agent-a" : "agent-b" };
    },
  });
  assert.equal(result.provenBy, "real-matrix");
  assert.deepEqual(result.scenarioResults["matrix-scenario"].agentTrialCounts, { "agent-b": 5, "agent-a": 5 });
});

// F2 regression: the exact defect shape from the QA repro — 2 distinct
// agents, 5 total trials, but split unevenly (3-and-2) so NEITHER agent
// individually cleared the >=5-trials bar. The old check
// (`distinctAgents.size >= 2 && k >= 5`, where k counts TOTAL trials per
// scenario) mislabelled this "real-matrix". It must report "stub".
test("F2: an uneven split (3-and-2) across 2 agents at k=5 total is NOT a real matrix", async () => {
  const dir = scenariosDir();
  writeScenario(dir, "uneven-split");
  let call = 0;
  const result = await runScenarios({
    scenariosDir: dir,
    k: 5,
    driver: "external",
    runTrial: async () => {
      call += 1;
      // agent-a gets trials 1-3 (3 trials), agent-b gets trials 4-5 (2 trials)
      return { operations: ["tool_a"], reasonCodes: [], finalEnvironment: { toolCalls: 1 }, answer: "done", agentId: call <= 3 ? "agent-a" : "agent-b" };
    },
  });
  assert.equal(result.scenarioResults["uneven-split"].agentTrialCounts["agent-a"], 3);
  assert.equal(result.scenarioResults["uneven-split"].agentTrialCounts["agent-b"], 2);
  assert.equal(result.provenBy, "stub");
});

// F11 regression: zero trials must never vacuously "pass". Before the fix,
// k=0 produced an empty trials array, passCount=0, failCount=0, and
// `failCount === 0` reported "pass" — a scenario that was never actually
// exercised was reported clean.
test("F11: k=0 (zero trials) is a fail, not a vacuous pass", async () => {
  const dir = scenariosDir();
  writeScenario(dir, "zero-trials");
  const result = await runScenarios({ scenariosDir: dir, k: 0, driver: "stub" });
  assert.equal(result.scenarioResults["zero-trials"].status, "fail");
  assert.equal(result.scenarioResults["zero-trials"].noTrials, true);
  assert.equal(result.verdict, "fail");
});

test("a scenario missing its stub fixture fails rather than silently passing", async () => {
  const dir = scenariosDir();
  writeScenario(dir, "no-fixture", { fixture: {} });
  const result = await runScenarios({ scenariosDir: dir, k: 1, driver: "stub" });
  assert.equal(result.scenarioResults["no-fixture"].status, "fail");
});

test("no scenario files is a typed fatal, not zero-scenario silent pass", async () => {
  const dir = scenariosDir();
  const result = await runScenarios({ scenariosDir: dir, k: 1, driver: "stub" });
  assert.equal(result.fatal.code, "no_scenarios");
  assert.equal(result.verdict, "fail");
});
