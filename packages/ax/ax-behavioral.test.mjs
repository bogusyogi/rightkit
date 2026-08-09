import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { evaluateTrial, runMatrix, runScenarios } from "./ax/behavioral.mjs";
import { fileURLToPath } from "node:url";

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
//
// F2 (round 2): real-matrix now additionally requires the caller to declare
// its agent roster explicitly (`agents`) — see ax/behavioral.mjs's
// scenarioMeetsRealMatrixBar banner for why identity honesty is a caller
// contract this module cannot verify from strings alone. This test declares
// exactly the two agents its trials use.
test("a real multi-agent matrix (>=2 agents, >=5 trials EACH) is provenBy real-matrix", async () => {
  const dir = scenariosDir();
  writeScenario(dir, "matrix-scenario");
  let call = 0;
  const result = await runScenarios({
    scenariosDir: dir,
    k: 10,
    driver: "multi-agent",
    agents: ["agent-a", "agent-b"],
    runTrial: async () => {
      call += 1;
      return { operations: ["tool_a"], reasonCodes: [], finalEnvironment: { toolCalls: 1 }, answer: "done", agentId: call % 2 === 0 ? "agent-a" : "agent-b" };
    },
  });
  assert.equal(result.provenBy, "real-matrix");
  assert.deepEqual(result.scenarioResults["matrix-scenario"].agentTrialCounts, { "agent-b": 5, "agent-a": 5 });
});

// F2 regression: a genuine 5+5 split across two agents whose IDs the trials
// use is NOT real-matrix if the caller never declared a roster — the roster
// declaration is required, not optional/inferred from observed agentIds.
test("F2: a clean 5+5 split with NO declared roster is not real-matrix", async () => {
  const dir = scenariosDir();
  writeScenario(dir, "undeclared-roster");
  let call = 0;
  const result = await runScenarios({
    scenariosDir: dir,
    k: 10,
    driver: "multi-agent",
    // agents intentionally omitted
    runTrial: async () => {
      call += 1;
      return { operations: ["tool_a"], reasonCodes: [], finalEnvironment: { toolCalls: 1 }, answer: "done", agentId: call % 2 === 0 ? "agent-a" : "agent-b" };
    },
  });
  assert.equal(result.provenBy, "stub");
});

// F2 regression: a declared roster still respects the per-agent >=5 bar —
// declaring a roster is necessary but not sufficient.
test("F2: a declared roster with an uneven 3+2 split is still not real-matrix", async () => {
  const dir = scenariosDir();
  writeScenario(dir, "declared-roster-uneven");
  let call = 0;
  const result = await runScenarios({
    scenariosDir: dir,
    k: 5,
    driver: "external",
    agents: ["agent-a", "agent-b"],
    runTrial: async () => {
      call += 1;
      return { operations: ["tool_a"], reasonCodes: [], finalEnvironment: { toolCalls: 1 }, answer: "done", agentId: call <= 3 ? "agent-a" : "agent-b" };
    },
  });
  assert.equal(result.provenBy, "stub");
});

// F2 regression (alias collapse in the display data): whitespace, casing,
// and padding aliases of the SAME agent must merge into one bucket in
// agentTrialCounts rather than counting as distinct agents.
test("F2: whitespace/case/padding aliases of one agent collapse into a single count", async () => {
  const dir = scenariosDir();
  writeScenario(dir, "alias-collapse");
  const aliasIds = ["agent-a", " Agent-A ", "agent-a\t", "AGENT-A"];
  let call = 0;
  const result = await runScenarios({
    scenariosDir: dir,
    k: 4,
    driver: "external",
    runTrial: async () => {
      const agentId = aliasIds[call];
      call += 1;
      return { operations: ["tool_a"], reasonCodes: [], finalEnvironment: { toolCalls: 1 }, answer: "done", agentId };
    },
  });
  assert.deepEqual(result.scenarioResults["alias-collapse"].agentTrialCounts, { "agent-a": 4 });
});

// F2 regression: normalization applies to the DECLARED ROSTER too — a
// caller that "declares" two IDs which alias to the same normalized string
// has not declared two distinct agents, so real-matrix must never trigger,
// no matter how many trials are run.
test("F2: a declared roster whose two entries alias to one normalized agent is never real-matrix", async () => {
  const dir = scenariosDir();
  writeScenario(dir, "aliased-roster");
  const result = await runScenarios({
    scenariosDir: dir,
    k: 10,
    driver: "external",
    agents: ["agent-a", " Agent-A "], // both normalize to "agent-a" — one declared agent, not two
    runTrial: async () => ({ operations: ["tool_a"], reasonCodes: [], finalEnvironment: { toolCalls: 1 }, answer: "done", agentId: "agent-a" }),
  });
  assert.equal(result.provenBy, "stub");
});

// F2 regression: normalization matches trial agentIds against the declared
// roster too, so a genuinely declared roster of 2 agents whose trials use
// noisy (but equivalent) casing/padding still clears the real-matrix bar —
// normalization is not one-sided.
test("F2: declared roster + noisy-but-equivalent trial agentId casing still clears real-matrix", async () => {
  const dir = scenariosDir();
  writeScenario(dir, "roster-and-trial-noise");
  const noisyIds = [" agent-a ", "AGENT-A", "agent-a", "Agent-A", "agent-a\t", "agent-b", " agent-b ", "AGENT-B", "Agent-B", "agent-b\t"];
  let call = 0;
  const result = await runScenarios({
    scenariosDir: dir,
    k: 10,
    driver: "external",
    agents: [" Agent-A ", "agent-b"],
    runTrial: async () => {
      const agentId = noisyIds[call];
      call += 1;
      return { operations: ["tool_a"], reasonCodes: [], finalEnvironment: { toolCalls: 1 }, answer: "done", agentId };
    },
  });
  assert.equal(result.provenBy, "real-matrix");
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

// F2 (round 3) regression: a Unicode NFKC-equivalent alias of an already-
// counted agent — not merely a different case/whitespace form of the same
// ASCII string, but a genuinely different code-point sequence that is
// canonically equivalent under compatibility normalization — must collapse
// into the same bucket. "ａｇｅｎｔ－ａ" (fullwidth Unicode forms) is a
// different string than "agent-a" byte-for-byte, but normalize("NFKC")
// maps it to the identical canonical form.
test("F2: an NFKC-equivalent fullwidth alias collapses into the same agent as its ASCII form (agentTrialCounts and roster both)", async () => {
  const dir = scenariosDir();
  writeScenario(dir, "nfkc-alias");
  const fullwidthAlias = "ａｇｅｎｔ－ａ"; // "ａｇｅｎｔ－ａ"
  assert.equal(fullwidthAlias.normalize("NFKC"), "agent-a");
  const ids = ["agent-a", "agent-a", "agent-a", "agent-a", "agent-a", fullwidthAlias, fullwidthAlias, fullwidthAlias, fullwidthAlias, fullwidthAlias];
  let call = 0;
  const result = await runScenarios({
    scenariosDir: dir,
    k: 10,
    driver: "external",
    agents: ["agent-a", fullwidthAlias], // both normalize to "agent-a" — one declared agent, not two
    runTrial: async () => {
      const agentId = ids[call];
      call += 1;
      return { operations: ["tool_a"], reasonCodes: [], finalEnvironment: { toolCalls: 1 }, answer: "done", agentId };
    },
  });
  assert.deepEqual(result.scenarioResults["nfkc-alias"].agentTrialCounts, { "agent-a": 10 });
  // Roster normalizes to one distinct entry too, so this can never become a
  // real matrix no matter how many trials are run under either spelling.
  assert.equal(result.provenBy, "stub");
});

// F2 (round 3) regression: the exact QA repro shape — a genuine 2-agent
// declared roster each clearing the >=5-trials bar, PLUS one extra trial run
// by a third, undeclared agentId. Before the fix, scenarioMeetsRealMatrixBar
// silently ignored the rogue trial for counting purposes but the overall run
// still reported provenBy "real-matrix" — an unexpected participant in what
// is being claimed as a controlled comparison must invalidate that claim,
// not just fail to earn it credit.
test("F2: a declared 5+5 matrix plus one undeclared rogue agent's trial is rejected (never real-matrix)", async () => {
  const dir = scenariosDir();
  writeScenario(dir, "rogue-trial");
  let call = 0;
  const result = await runScenarios({
    scenariosDir: dir,
    k: 11,
    driver: "external",
    agents: ["agent-a", "agent-b"],
    runTrial: async () => {
      call += 1;
      // Trials 1-5: agent-a. Trials 6-10: agent-b. Trial 11: an undeclared
      // rogue identity nobody declared as part of the roster.
      const agentId = call <= 5 ? "agent-a" : call <= 10 ? "agent-b" : "rogue-agent";
      return { operations: ["tool_a"], reasonCodes: [], finalEnvironment: { toolCalls: 1 }, answer: "done", agentId };
    },
  });
  assert.equal(result.scenarioResults["rogue-trial"].agentTrialCounts["agent-a"], 5);
  assert.equal(result.scenarioResults["rogue-trial"].agentTrialCounts["agent-b"], 5);
  assert.equal(result.scenarioResults["rogue-trial"].agentTrialCounts["rogue-agent"], 1);
  assert.equal(result.provenBy, "stub", "an undeclared agent's trial must force the run out of real-matrix");
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

const fake = fileURLToPath(new URL("./fixtures/fake-ax-adapter.mjs", import.meta.url));
const artifactInput = () => ({ repoPath: mkdtempSync(path.join(os.tmpdir(), "ax-matrix-repo-")), artifactRoot: mkdtempSync(path.join(os.tmpdir(), "ax-matrix-artifact-")) });
const matrix = (mode = "pass", overrides = {}) => ({ adapters: ["alpha", "beta"].map((id) => ({ id, model: `m-${id}`, version: "1", command: process.execPath, args: [fake, mode, id] })), trialsPerAgent: 5, limits: { timeoutMs: 5000, maxToolCalls: 2, maxTokens: 2, maxCostUsd: 1 }, ...overrides });
test("matrix happy path binds full evidence", async () => { const dir = scenariosDir(); writeScenario(dir, "matrix"); const input=artifactInput(); const a = await runMatrix({ ...input, scenariosDir: dir, matrix: matrix() }); const b = await runMatrix({ ...artifactInput(), scenariosDir: dir, matrix: { ...matrix(), adapters: [...matrix().adapters].reverse() } }); assert.equal(a.verdict, "pass"); assert.deepEqual(a.scenarioResults.matrix.agentTrialCounts, { alpha: 5, beta: 5 }); assert.equal(a.provenBy, "real-matrix"); assert.equal(a.matrixDigest, b.matrixDigest); assert.equal(a.scenarioCount,1); assert.deepEqual(a.scenarioIds,["matrix"]); assert.equal(a.artifactDigest,a.artifact.digest); assert.deepEqual(a.trials[0].trials.map((row)=>row.trial),[1,2,3,4,5,1,2,3,4,5]); assert.ok(a.trials[0].trials.every((row)=>path.isAbsolute(row.response.transcriptSha256)===false)); });
test("matrix digest binds command and limits while scenario digest binds content", async () => { const dir = scenariosDir(); writeScenario(dir,"bound"); const base=await runMatrix({...artifactInput(),scenariosDir:dir,matrix:matrix()}); const changed=matrix(); changed.limits={...changed.limits,maxTokens:3}; const other=await runMatrix({...artifactInput(),scenariosDir:dir,matrix:changed}); assert.notEqual(base.matrixDigest,other.matrixDigest); writeFileSync(path.join(dir,"bound.yaml"),JSON.stringify({scenarioId:"bound",expected:{requiredClaims:["changed"]}})); const content=await runMatrix({...artifactInput(),scenariosDir:dir,matrix:matrix()}); assert.notEqual(base.scenarioDigest,content.scenarioDigest); });
test("duplicate execution identity is invalid", async () => { const dir=scenariosDir(); writeScenario(dir,"duplicate"); const adapters=matrix().adapters; const bad=[adapters[0],{...adapters[0],id:"beta"}]; const result=await runMatrix({...artifactInput(),scenariosDir:dir,matrix:matrix("pass",{adapters:bad})}); assert.equal(result.fatal.code,"matrix_invalid"); });
test("domain failure is complete real matrix with fail verdict", async () => { const dir = scenariosDir(); writeScenario(dir, "domain"); const r = await runMatrix({ ...artifactInput(), scenariosDir: dir, matrix: matrix("domain-fail") }); assert.equal(r.provenBy, "real-matrix"); assert.equal(r.claimBoundary, "PROVEN"); assert.equal(r.verdict, "fail"); });
test("timeout is incomplete and UNPROVEN", async () => { const dir = scenariosDir(); writeScenario(dir, "timeout"); const r = await runMatrix({ ...artifactInput(), scenariosDir: dir, matrix: matrix("timeout", { limits: { timeoutMs: 50, maxToolCalls: 2, maxTokens: 2, maxCostUsd: 1 } }) }); assert.equal(r.claimBoundary, "UNPROVEN"); assert.equal(r.verdict, "fail"); });
test("invalid matrix forms reject before spawn", async () => { const dir = scenariosDir(); writeScenario(dir, "invalid"); for (const bad of [{ adapters: [matrix().adapters[0], { ...matrix().adapters[0], id: " ALPHA " }] }, { trialsPerAgent: 4 }, { adapters: [{ ...matrix().adapters[0], model: "" }, matrix().adapters[1]] }, { limits: { timeoutMs: 1 } }]) { const r = await runMatrix({ ...artifactInput(), scenariosDir: dir, matrix: matrix("exit", bad) }); assert.equal(r.fatal.code, "matrix_invalid"); } });
