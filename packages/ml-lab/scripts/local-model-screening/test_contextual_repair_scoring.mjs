#!/usr/bin/env node
import assert from "node:assert/strict";
import childProcess from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(new URL(import.meta.url)));
const scorer = path.join(scriptDir, "score_contextual_repair_screen.mjs");
const runRoot = fs.mkdtempSync(path.join(os.tmpdir(), "heardright-scoring-test-"));

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function writeJsonl(file, rows) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${rows.map(JSON.stringify).join("\n")}\n`);
}

function run(...args) {
  return childProcess.spawnSync(process.execPath, [scorer, ...args, "--run-root", runRoot], {
    encoding: "utf8",
    windowsHide: true,
  });
}

function result(caseId, phase, wallMs = 100) {
  return {
    row_id: `candidate__recipe__${caseId}`,
    case_id: caseId,
    candidate_id: "candidate",
    recipe_id: "recipe",
    output_mode: "full_text_v1",
    phase,
    status: "ok",
    captured_at: `2026-07-21T00:00:0${phase === "tune_a" ? 1 : 2}.000Z`,
    cleanup_wall_ms: wallMs,
    materialized_output: "correct output",
    mechanical_flags: [],
  };
}

try {
  writeJsonl(path.join(runRoot, "private", "references.tune.jsonl"), [
    { case_id: "case-a", cloud_reference: "correct output" },
    { case_id: "case-b", cloud_reference: "correct output" },
  ]);
  writeJsonl(path.join(runRoot, "results", "tune_a", "candidate__recipe.jsonl"), [
    result("case-a", "tune_a"),
    result("case-b", "tune_a", 110),
  ]);

  let scored = run("--report", "--phase", "tune_a");
  assert.equal(scored.status, 0, scored.stderr);
  let summary = JSON.parse(fs.readFileSync(path.join(runRoot, "reports", "tune_a.summary.json"), "utf8")).summaries[0];
  assert.equal(summary.mechanically_eligible, 2);
  assert.equal(summary.accepted, 0, "unrated outputs must never count as accepted");
  assert.equal(summary.speed_parity, null, "accepted-output parity is unavailable until blind ratings exist");
  assert.equal(summary.human_review_complete, false);

  writeJsonl(path.join(runRoot, "private", "human-ratings.jsonl"), [
    { candidate_id: "candidate", recipe_id: "recipe", case_id: "case-a", semantic_1_to_5: 5, conservative_1_to_5: 5, critical_failure: false },
    { candidate_id: "candidate", recipe_id: "recipe", case_id: "case-b", semantic_1_to_5: 4, conservative_1_to_5: 5, critical_failure: false },
  ]);
  scored = run("--report", "--phase", "tune_a");
  assert.equal(scored.status, 0, scored.stderr);
  summary = JSON.parse(fs.readFileSync(path.join(runRoot, "reports", "tune_a.summary.json"), "utf8")).summaries[0];
  assert.equal(summary.accepted, 2);
  assert.equal(summary.speed_parity, true);
  assert.equal(summary.human_review_complete, true);

  writeJsonl(path.join(runRoot, "private", "human-ratings.jsonl"), [
    { candidate_id: "candidate", recipe_id: "recipe", case_id: "case-a", semantic_1_to_5: 5, conservative_1_to_5: 5, critical_failure: false },
    { candidate_id: "candidate", recipe_id: "recipe", case_id: "case-b", semantic_1_to_5: 1, conservative_1_to_5: 1, critical_failure: true },
  ]);
  scored = run("--report", "--phase", "tune_a");
  assert.equal(scored.status, 0, scored.stderr);
  summary = JSON.parse(fs.readFileSync(path.join(runRoot, "reports", "tune_a.summary.json"), "utf8")).summaries[0];
  assert.equal(summary.accepted, 1);
  assert.equal(summary.critical_failures, 1);
  assert.equal(summary.human_review_complete, true, "a completed review may contain a critical failure");
  assert.equal(summary.final_quality_complete, true);
  assert.equal(summary.speed_parity, null, "a critical failure cannot leave an unrated/empty accepted latency pass");

  writeJsonl(path.join(runRoot, "private", "human-ratings.jsonl"), [
    { candidate_id: "candidate", recipe_id: "recipe", case_id: "case-a", semantic_1_to_5: 5, conservative_1_to_5: 5, critical_failure: false },
    { candidate_id: "candidate", recipe_id: "recipe", case_id: "case-b", semantic_1_to_5: 4, conservative_1_to_5: 5, critical_failure: false },
  ]);

  writeJson(path.join(runRoot, "candidate-visible", "candidate-recipes.json"), {
    candidates: [{ id: "candidate", recipes: [{ id: "recipe", output_mode: "full_text_v1" }] }],
  });
  const frozen = run("--freeze-winners");
  assert.equal(frozen.status, 0, `freeze must derive the expected tuning count from references, not hardcode 58:\n${frozen.stderr}`);
  assert.equal(JSON.parse(fs.readFileSync(path.join(runRoot, "private", "winners.freeze.json"), "utf8")).winners.length, 1);
} finally {
  fs.rmSync(runRoot, { recursive: true, force: true });
}

console.log(JSON.stringify({ status: "ok", unrated_is_not_accepted: true, dynamic_tune_count: true }));
