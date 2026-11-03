import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { durableEvaluationBinding, main, runStageInDir } from "./orchestrate.mjs";

const ROOT = dirname(fileURLToPath(import.meta.url));

test("every child launch uses the output-directory boundary", () => {
  const source = readFileSync(join(ROOT, "orchestrate.mjs"), "utf8");
  const directCalls = source.match(/\brunStage\(/gu) ?? [];
  assert.equal(directCalls.length, 2, "only runStage definition and runStageInDir delegation may call runStage");
  for (const required of ["baselineEvalDir", "epochEvalDir", "selectionDir", "compatibilityDir", "backendPreflightDir", "autotuneDir", "fullMergeDir", "exportDir", "ggufEvalDir", "cleanupDir"])
    assert.match(source, new RegExp(`runStageInDir\\([^\\n]+${required}`, "u"), required);
  assert.equal((source.match(/evaluationOutputs\(/gu) ?? []).length, 4, "helper plus Gemma, baseline, and epoch evaluation calls must bind the durable view");
  assert.match(source, /"--action", "export-q8"[^\n]+"--eval-input", selectedEvalInput\.path[^\n]+"--validation-artifact", selectedEval/u);
  assert.match(source, /"--action", "gguf-evaluate"[^\n]+"--eval-input", selectedEvalInput\.path/u);
});

test("selected evaluation binding requires the retained in-stage normalized view and exact hash", () => {
  const root = mkdtempSync(join(tmpdir(), "heardright-durable-binding-"));
  try {
    const stage = join(root, "baseline-evaluation"); mkdirSync(stage);
    const view = join(stage, "normalized-evaluation.jsonl"); writeFileSync(view, '{"id":"one","input_text":"hello"}\n');
    const digest = `sha256:${createHash("sha256").update(readFileSync(view)).digest("hex")}`;
    const evaluation = join(stage, "evaluation.json"); writeFileSync(evaluation, JSON.stringify({ eval_input: view, eval_input_sha256: digest }));
    assert.deepEqual(durableEvaluationBinding(evaluation), { path: resolve(view), sha256: digest });
    writeFileSync(view, "tampered\n");
    assert.throws(() => durableEvaluationBinding(evaluation), /hash mismatch/u);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("live-stage stub observes its output directory before child invocation", () => {
  const root = mkdtempSync(join(tmpdir(), "heardright-stage-dir-")), stage = join(root, "baseline-evaluation"), output = join(stage, "evaluation.json");
  try {
    const program = "const fs=require('fs');if(!fs.existsSync(process.argv[1]))process.exit(41);fs.writeFileSync(process.argv[2],'ok')";
    const context = { outputDir: root, state: { started_at: new Date().toISOString(), authorization: { hourly_rate_usd: 0.1, max_hours: 24, max_usd: 20 }, events: [] }, persist() {} };
    runStageInDir(context, "test:baseline-evaluate", stage, [process.execPath, "-e", program, stage, output], [output]);
    assert.equal(existsSync(output), true);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("copied-bundle-compatible dry path writes state", () => {
  const root = mkdtempSync(join(tmpdir(), "heardright-copied-dry-"));
  try {
    const bundleRoot = resolve(ROOT, "..", ".."), policy = join(bundleRoot, "evaluation-policy.v2.json"), control = join(bundleRoot, "evaluation-policy-control.v1.json");
    const policyArgs = existsSync(policy) && existsSync(control) ? ["--evaluation-policy", policy, "--evaluation-policy-control", control] : [];
    const state = main(["--dry-run", "--output-dir", root, ...policyArgs]);
    assert.deepEqual(state.lane_order, ["falcon", "granite", "gemma", "sotto"]);
    assert.equal(existsSync(join(root, "state.json")), true);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
