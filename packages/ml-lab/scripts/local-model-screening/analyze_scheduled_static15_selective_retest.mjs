#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { percentile, wordErrorRate } from "./run_scheduled_static15_selective_retest.mjs";

const DEFAULT_RUN_ROOT = "E:\\models\\heardright-local-screening\\scheduled-static15-local-llm-selective-v1";
const ARMS = ["warm_final", "prefix_prefill"];
const CLOUD_L1_CLEANUP_MS = Object.freeze({
  metric: "cleanup_stage_wall_ms",
  model: "qwen/qwen3.6-27b",
  prompt_version: "l1_app_polish_prompt_v5",
  sample_count: 93,
  p50: 360,
  p95: 570,
  max: 742,
  evidence: "docs/evidence/l1-local-model-screening-setup-2026-07-20/cloud-yardstick-current.json",
});

function valueAfter(name, fallback) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : fallback;
}

function listAfter(name) {
  return String(valueAfter(name, "")).split(/[\s,]+/).map((value) => value.trim()).filter(Boolean);
}

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

function readJsonl(file) {
  return fs.readFileSync(file, "utf8").split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line));
}

function mean(values) {
  return values.length ? values.reduce((sum, value) => sum + value, 0) / values.length : null;
}

function normalizedTokens(text) {
  return String(text ?? "").toLowerCase().match(/[\p{L}\p{N}]+/gu) ?? [];
}

function tokenF1(reference, hypothesis) {
  const left = normalizedTokens(reference);
  const right = normalizedTokens(hypothesis);
  if (!left.length && !right.length) return 1;
  const counts = new Map();
  for (const token of left) counts.set(token, (counts.get(token) ?? 0) + 1);
  let overlap = 0;
  for (const token of right) {
    const count = counts.get(token) ?? 0;
    if (count > 0) {
      overlap += 1;
      counts.set(token, count - 1);
    }
  }
  const precision = right.length ? overlap / right.length : 0;
  const recall = left.length ? overlap / left.length : 0;
  return precision + recall ? (2 * precision * recall) / (precision + recall) : 0;
}

function hashFile(file) {
  return crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function resultFile(runRoot, pass, candidateId, recipeId, arm) {
  return path.join(runRoot, "results", pass, `${candidateId}__${recipeId}__${arm}.jsonl`);
}

function summarizeArm(rows, casesById, scorerById) {
  const latencies = rows.map((row) => row.cleanup_wall_ms).filter(Number.isFinite);
  const endToEnd = rows.map((row) => row.simulated_end_to_end_stop_ms).filter(Number.isFinite);
  const effective = rows.map((row) => {
    const captured = casesById.get(row.case_id);
    const safe = row.status === "ok" && !(row.mechanical_flags ?? []).length;
    return {
      row,
      safe,
      output: safe ? row.materialized_output : captured.transcript,
      source: captured.transcript,
      cloud: scorerById.get(row.case_id)?.cloud_output ?? null,
    };
  });
  const safeRows = effective.filter((item) => item.safe);
  const cloudRows = effective.filter((item) => item.cloud);
  const cleanupP50 = percentile(latencies, 0.5);
  const cleanupP95 = percentile(latencies, 0.95);
  return {
    attempts: rows.length,
    safe_rows: safeRows.length,
    fallback_rows: rows.length - safeRows.length,
    safe_changed_rows: safeRows.filter((item) => item.output !== item.source).length,
    safe_noop_rows: safeRows.filter((item) => item.output === item.source).length,
    cleanup_p50_ms: cleanupP50,
    cleanup_p95_ms: cleanupP95,
    cloud_l1_cleanup_p50_ms: CLOUD_L1_CLEANUP_MS.p50,
    cloud_l1_cleanup_p95_ms: CLOUD_L1_CLEANUP_MS.p95,
    cleanup_p50_delta_vs_cloud_l1_ms: cleanupP50 - CLOUD_L1_CLEANUP_MS.p50,
    cleanup_p95_delta_vs_cloud_l1_ms: cleanupP95 - CLOUD_L1_CLEANUP_MS.p95,
    cleanup_p50_ratio_vs_cloud_l1: cleanupP50 / CLOUD_L1_CLEANUP_MS.p50,
    cleanup_p95_ratio_vs_cloud_l1: cleanupP95 / CLOUD_L1_CLEANUP_MS.p95,
    end_to_end_stop_p95_ms: percentile(endToEnd, 0.95),
    cloud_l1_wer_mean: mean(cloudRows.map((item) => wordErrorRate(item.cloud, item.output))),
    effective_cloud_token_f1_mean: mean(cloudRows.map((item) => tokenF1(item.cloud, item.output))),
    cloud_reference_rows: cloudRows.length,
    cache_n_p50: percentile(rows.map((row) => row.timings?.cache_n).filter(Number.isFinite), 0.5),
    prompt_n_p50: percentile(rows.map((row) => row.timings?.prompt_n).filter(Number.isFinite), 0.5),
    peak_process_working_set_bytes: Math.max(0, ...rows.map((row) => row.process_working_set_bytes ?? 0)) || null,
  };
}

function chooseArm(arms) {
  return Object.entries(arms).sort(([, left], [, right]) =>
    right.safe_rows - left.safe_rows
    || (left.cloud_l1_wer_mean ?? Number.POSITIVE_INFINITY) - (right.cloud_l1_wer_mean ?? Number.POSITIVE_INFINITY)
    || left.cleanup_p95_ms - right.cleanup_p95_ms
  )[0][0];
}

function assertFullTextRecipes(snapshot, label) {
  for (const candidate of snapshot.candidates) {
    const recipe = candidate.recipes[0];
    if (recipe?.output_mode !== "full_text_v1") {
      throw new Error(`${label} candidate ${candidate.id} is ineligible: deployment comparison requires full_text_v1`);
    }
  }
}

function main() {
  const runRoot = path.resolve(valueAfter("--run-root", DEFAULT_RUN_ROOT));
  const additionalRunRootValue = valueAfter("--additional-run-root", null);
  const additionalRunRoot = additionalRunRootValue ? path.resolve(additionalRunRootValue) : null;
  const shortlist = listAfter("--shortlist");
  const cases = readJsonl(path.join(runRoot, "candidate-visible", "cases.jsonl"));
  const scorer = readJsonl(path.join(runRoot, "private", "scorer.jsonl"));
  const casesById = new Map(cases.map((row) => [row.case_id, row]));
  const scorerById = new Map(scorer.map((row) => [row.case_id, row]));
  const recipesByPass = new Map([["pass1", readJson(path.join(runRoot, "candidate-visible", "recipes.pass1.json"))]]);
  const pass2RecipeFile = path.join(runRoot, "candidate-visible", "recipes.pass2.json");
  if (fs.existsSync(pass2RecipeFile)) recipesByPass.set("pass2", readJson(pass2RecipeFile));
  for (const [pass, snapshot] of recipesByPass) assertFullTextRecipes(snapshot, pass);
  const pass2Ids = new Set((recipesByPass.get("pass2")?.candidates ?? []).map((candidate) => candidate.id));
  let candidates = recipesByPass.get("pass1").candidates.map((pass1Candidate) => {
    const pass = pass2Ids.has(pass1Candidate.id) ? "pass2" : "pass1";
    const candidate = recipesByPass.get(pass).candidates.find((item) => item.id === pass1Candidate.id);
    const recipe = candidate.recipes[0];
    const pass1Recipe = pass1Candidate.recipes[0];
    const pass1Arms = Object.fromEntries(ARMS.map((arm) => [arm, summarizeArm(readJsonl(resultFile(runRoot, "pass1", pass1Candidate.id, pass1Recipe.id, arm)), casesById, scorerById)]));
    const arms = Object.fromEntries(ARMS.map((arm) => [arm, summarizeArm(readJsonl(resultFile(runRoot, pass, candidate.id, recipe.id, arm)), casesById, scorerById)]));
    const selectedArm = chooseArm(arms);
    const coldRecipe = pass1Candidate.recipes[0];
    const cold = readJsonl(resultFile(runRoot, "pass1", pass1Candidate.id, coldRecipe.id, "cold"))[0];
    return {
      candidate_id: candidate.id,
      selected_pass: pass,
      recipe_id: recipe.id,
      pass1_recipe_id: pass1Recipe.id,
      selected_arm: selectedArm,
      cloud_cleanup_parity_p95: arms[selectedArm].cleanup_p95_ms <= CLOUD_L1_CLEANUP_MS.p95,
      cold: {
        server_startup_ms: cold.startup_ms,
        first_cleanup_ms: cold.cleanup_wall_ms,
        startup_plus_first_cleanup_ms: cold.cold_total_ms,
      },
      pass1_arms: pass1Arms,
      arms,
    };
  });
  const sourceFiles = [
    path.join(runRoot, "private", "control.pass1.json"),
    path.join(runRoot, "reports", "pass1.summary.json"),
  ];
  if (recipesByPass.has("pass2")) sourceFiles.push(
    path.join(runRoot, "private", "control.pass2.json"),
    path.join(runRoot, "reports", "pass2.summary.json"),
  );
  if (additionalRunRoot) {
    const additionalCases = readJsonl(path.join(additionalRunRoot, "candidate-visible", "cases.jsonl"));
    const expectedCaseIds = cases.map((row) => row.case_id);
    const additionalCaseIds = additionalCases.map((row) => row.case_id);
    if (JSON.stringify(additionalCaseIds) !== JSON.stringify(expectedCaseIds)) throw new Error("additional run case roster differs from primary run");
    const additionalScorer = readJsonl(path.join(additionalRunRoot, "private", "scorer.jsonl"));
    const additionalCasesById = new Map(additionalCases.map((row) => [row.case_id, row]));
    const additionalScorerById = new Map(additionalScorer.map((row) => [row.case_id, row]));
    const additionalRecipes = readJson(path.join(additionalRunRoot, "candidate-visible", "recipes.pass1.json"));
    assertFullTextRecipes(additionalRecipes, "additional pass1");
    candidates.push(...additionalRecipes.candidates.map((candidate) => {
      const recipe = candidate.recipes[0];
      const arms = Object.fromEntries(ARMS.map((arm) => [arm, summarizeArm(
        readJsonl(resultFile(additionalRunRoot, "pass1", candidate.id, recipe.id, arm)),
        additionalCasesById,
        additionalScorerById,
      )]));
      const selectedArm = chooseArm(arms);
      const cold = readJsonl(resultFile(additionalRunRoot, "pass1", candidate.id, recipe.id, "cold"))[0];
      return {
        candidate_id: candidate.id,
        selected_pass: "pass1",
        recipe_id: recipe.id,
        pass1_recipe_id: recipe.id,
        selected_arm: selectedArm,
        cloud_cleanup_parity_p95: arms[selectedArm].cleanup_p95_ms <= CLOUD_L1_CLEANUP_MS.p95,
        cold: {
          server_startup_ms: cold.startup_ms,
          first_cleanup_ms: cold.cleanup_wall_ms,
          startup_plus_first_cleanup_ms: cold.cold_total_ms,
        },
        pass1_arms: arms,
        arms,
      };
    }));
    sourceFiles.push(
      path.join(additionalRunRoot, "private", "control.pass1.json"),
      path.join(additionalRunRoot, "reports", "pass1.summary.json"),
    );
  }
  if (shortlist.length) {
    const byId = new Map(candidates.map((candidate) => [candidate.candidate_id, candidate]));
    candidates = shortlist.map((id) => {
      const candidate = byId.get(id);
      if (!candidate) throw new Error(`shortlisted candidate missing from run evidence: ${id}`);
      return candidate;
    });
  }
  const report = {
    schema: "heardright.scheduled_static15_selective_final_comparison.v2",
    created_at: new Date().toISOString(),
    run_root: runRoot,
    run_roots: [runRoot, ...(additionalRunRoot ? [additionalRunRoot] : [])],
    sample: {
      cases: cases.length,
      duration_s_min: Math.min(...cases.map((row) => row.duration_s)),
      duration_s_max: Math.max(...cases.map((row) => row.duration_s)),
      scheduled_background_windows_min: Math.min(...cases.map((row) => row.scheduled_background_windows)),
      scheduled_background_windows_max: Math.max(...cases.map((row) => row.scheduled_background_windows)),
    },
    cloud_yardstick_ms: CLOUD_L1_CLEANUP_MS,
    fallback_policy: "Use the unchanged Scheduled Static-15 transcript when generation rejects or deterministic mechanical guards fire.",
    candidates,
    provenance: sourceFiles.map((file) => ({ file, sha256: hashFile(file) })),
  };
  const output = path.join(runRoot, "reports", "final-comparison.json");
  fs.writeFileSync(output, `${JSON.stringify(report, null, 2)}\n`);
  console.log(JSON.stringify({ status: "analyzed", output, candidates: candidates.length }, null, 2));
}

main();
