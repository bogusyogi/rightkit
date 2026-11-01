#!/usr/bin/env node
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { nearestRankPercentile, sha256 } from "./contextual_repair_core.mjs";

function valueAfter(flag, fallback = null) {
  const index = process.argv.indexOf(flag);
  return index === -1 ? fallback : process.argv[index + 1];
}

const defaultDataRoot = process.env.LOCALAPPDATA ?? path.join(os.homedir(), ".local", "share");
const runRoot = path.resolve(valueAfter("--run-root", path.join(defaultDataRoot, "HeardRightNext", "bakeoffs", "l1-local-contextual-repair-v1")));

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

function readJsonl(file) {
  if (!fs.existsSync(file)) return [];
  return fs.readFileSync(file, "utf8").split(/\r?\n/).filter(Boolean).map(JSON.parse);
}

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function tokenize(text) {
  return String(text ?? "").toLocaleLowerCase("en-US").match(/[\p{L}\p{N}_./:@#%+-]+/gu) ?? [];
}

function multiset(tokens) {
  const result = new Map();
  for (const token of tokens) result.set(token, (result.get(token) ?? 0) + 1);
  return result;
}

function tokenF1(a, b) {
  const left = tokenize(a);
  const right = tokenize(b);
  if (!left.length && !right.length) return 1;
  if (!left.length || !right.length) return 0;
  const lset = multiset(left);
  const rset = multiset(right);
  let overlap = 0;
  for (const [token, count] of lset) overlap += Math.min(count, rset.get(token) ?? 0);
  const precision = overlap / left.length;
  const recall = overlap / right.length;
  return precision + recall === 0 ? 0 : (2 * precision * recall) / (precision + recall);
}

function mean(values) {
  return values.length ? values.reduce((sum, value) => sum + value, 0) / values.length : null;
}

function loadRatings() {
  const file = path.join(runRoot, "private", "human-ratings.jsonl");
  const map = new Map();
  for (const row of readJsonl(file)) map.set(`${row.candidate_id}__${row.recipe_id}__${row.case_id}`, row);
  return map;
}

function loadReferences() {
  const files = [path.join(runRoot, "private", "references.tune.jsonl"), path.join(runRoot, "private", "sealed-holdout.jsonl")];
  const map = new Map();
  for (const file of files) {
    for (const row of readJsonl(file)) {
      const reference = row.private_reference ?? row;
      map.set(reference.case_id, reference);
    }
  }
  return map;
}

function loadPhaseResults(phase) {
  const root = path.join(runRoot, "results", phase);
  if (!fs.existsSync(root)) return [];
  return fs.readdirSync(root).filter((name) => name.endsWith(".jsonl")).flatMap((name) => readJsonl(path.join(root, name)));
}

function loadCloudYardstick() {
  const controlFile = path.join(runRoot, "private", "control.json");
  const recorded = fs.existsSync(controlFile) ? readJson(controlFile).cloud_yardstick?.cleanup_stage_wall_ms : null;
  return {
    sample_count: recorded?.sample_count ?? 93,
    p50: recorded?.p50 ?? 360,
    p95: recorded?.p95 ?? 570,
    max: recorded?.max ?? 742,
  };
}

function summarizeGroup(rows, references, ratings, cloudYardstick) {
  const latest = new Map();
  for (const row of rows) latest.set(row.row_id, row);
  const attempts = [...latest.values()];
  const enriched = attempts.map((row) => {
    const rating = ratings.get(`${row.candidate_id}__${row.recipe_id}__${row.case_id}`) ?? null;
    const reference = references.get(row.case_id)?.cloud_reference ?? null;
    const mechanicallyEligible = row.status === "ok" && Array.isArray(row.mechanical_flags) && row.mechanical_flags.length === 0;
    const ratingComplete = Number.isFinite(rating?.semantic_1_to_5)
      && Number.isFinite(rating?.conservative_1_to_5)
      && typeof rating?.critical_failure === "boolean";
    const criticalFailure = ratingComplete ? rating.critical_failure : null;
    return {
      ...row,
      mechanically_eligible: mechanicallyEligible,
      human_rating_complete: ratingComplete,
      accepted: mechanicallyEligible && ratingComplete && !criticalFailure,
      reference_token_f1: row.status === "ok" && reference ? tokenF1(row.materialized_output, reference) : null,
      human_semantic_1_to_5: rating?.semantic_1_to_5 ?? null,
      human_conservative_1_to_5: rating?.conservative_1_to_5 ?? null,
      critical_failure: criticalFailure,
    };
  });
  const accepted = enriched.filter((row) => row.accepted);
  const mechanicallyEligible = enriched.filter((row) => row.mechanically_eligible);
  const coldRowIds = new Set();
  const byPhase = Map.groupBy(enriched, (row) => row.phase ?? "unknown");
  for (const phaseRows of byPhase.values()) {
    const firstSuccess = phaseRows.filter((row) => row.status === "ok").sort((a, b) => String(a.captured_at).localeCompare(String(b.captured_at)))[0];
    if (firstSuccess) coldRowIds.add(firstSuccess.row_id);
  }
  const allLatency = enriched
    .filter((row) => !coldRowIds.has(row.row_id))
    .map((row) => Number.isFinite(row.cleanup_wall_ms) ? row.cleanup_wall_ms : row.elapsed_ms)
    .filter(Number.isFinite);
  const acceptedLatency = accepted.filter((row) => !coldRowIds.has(row.row_id) && Number.isFinite(row.cleanup_wall_ms)).map((row) => row.cleanup_wall_ms);
  const mechanicallyEligibleLatency = mechanicallyEligible.filter((row) => !coldRowIds.has(row.row_id) && Number.isFinite(row.cleanup_wall_ms)).map((row) => row.cleanup_wall_ms);
  const referenceF1 = mechanicallyEligible.map((row) => row.reference_token_f1).filter(Number.isFinite);
  const ratedEligible = mechanicallyEligible.filter((row) => row.human_rating_complete);
  const semantic = ratedEligible.map((row) => row.human_semantic_1_to_5).filter(Number.isFinite);
  const conservative = ratedEligible.map((row) => row.human_conservative_1_to_5).filter(Number.isFinite);
  const acceptedP50 = nearestRankPercentile(acceptedLatency, 0.5);
  const acceptedP95 = nearestRankPercentile(acceptedLatency, 0.95);
  const humanReviewComplete = enriched.length > 0 && enriched.every((row) => row.human_rating_complete);
  return {
    candidate_id: enriched[0]?.candidate_id ?? null,
    recipe_id: enriched[0]?.recipe_id ?? null,
    output_mode: enriched[0]?.output_mode ?? null,
    attempts: enriched.length,
    successes: enriched.filter((row) => row.status === "ok").length,
    excluded_cold_rows: coldRowIds.size,
    mechanically_eligible: mechanicallyEligible.length,
    mechanically_eligible_rate: enriched.length ? mechanicallyEligible.length / enriched.length : 0,
    accepted: accepted.length,
    accepted_rate: enriched.length ? accepted.length / enriched.length : 0,
    critical_failures: enriched.filter((row) => row.critical_failure === true).length,
    human_ratings: enriched.filter((row) => row.human_rating_complete).length,
    human_review_complete: humanReviewComplete,
    mechanical_failure_counts: Object.fromEntries([...new Set(enriched.flatMap((row) => row.mechanical_flags ?? []))].sort().map((flag) => [flag, enriched.filter((row) => row.mechanical_flags?.includes(flag)).length])),
    reference_token_f1_mean: mean(referenceF1),
    human_semantic_mean: mean(semantic),
    human_conservative_mean: mean(conservative),
    cleanup_wall_all_p50_ms: nearestRankPercentile(allLatency, 0.5),
    cleanup_wall_all_p95_ms: nearestRankPercentile(allLatency, 0.95),
    cleanup_wall_mechanically_eligible_p50_ms: nearestRankPercentile(mechanicallyEligibleLatency, 0.5),
    cleanup_wall_mechanically_eligible_p95_ms: nearestRankPercentile(mechanicallyEligibleLatency, 0.95),
    cleanup_wall_accepted_p50_ms: acceptedP50,
    cleanup_wall_accepted_p95_ms: acceptedP95,
    cloud_p50_delta_ms: acceptedP50 === null ? null : acceptedP50 - cloudYardstick.p50,
    cloud_p95_delta_ms: acceptedP95 === null ? null : acceptedP95 - cloudYardstick.p95,
    cloud_p95_ratio: acceptedP95 === null ? null : acceptedP95 / cloudYardstick.p95,
    speed_parity: acceptedLatency.length === 0 ? null : acceptedP50 <= cloudYardstick.p50 && acceptedP95 <= cloudYardstick.p95,
    final_quality_complete: humanReviewComplete,
    peak_process_working_set_bytes: Math.max(0, ...enriched.map((row) => row.process_working_set_bytes ?? 0)) || null,
    peak_gpu_memory_bytes: Math.max(0, ...enriched.map((row) => row.gpu_memory_bytes ?? 0)) || null,
    rows: enriched,
  };
}

function groupSummaries(rows, references, ratings, cloudYardstick = loadCloudYardstick()) {
  const groups = new Map();
  for (const row of rows) {
    const key = `${row.candidate_id}__${row.recipe_id}`;
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(row);
  }
  return [...groups.values()].map((group) => summarizeGroup(group, references, ratings, cloudYardstick));
}

function recipeComparator(a, b) {
  const bothReviewed = a.human_review_complete && b.human_review_complete;
  return (bothReviewed ? b.accepted_rate - a.accepted_rate : 0)
    || a.critical_failures - b.critical_failures
    || b.mechanically_eligible_rate - a.mechanically_eligible_rate
    || (b.human_semantic_mean ?? -1) - (a.human_semantic_mean ?? -1)
    || (b.human_conservative_mean ?? -1) - (a.human_conservative_mean ?? -1)
    || (b.reference_token_f1_mean ?? -1) - (a.reference_token_f1_mean ?? -1)
    || (a.cleanup_wall_mechanically_eligible_p95_ms ?? Number.POSITIVE_INFINITY) - (b.cleanup_wall_mechanically_eligible_p95_ms ?? Number.POSITIVE_INFINITY)
    || (a.peak_process_working_set_bytes ?? Number.POSITIVE_INFINITY) - (b.peak_process_working_set_bytes ?? Number.POSITIVE_INFINITY);
}

function stageA(references, ratings) {
  const summaries = groupSummaries(loadPhaseResults("tune_a"), references, ratings);
  const byCandidate = Map.groupBy(summaries, (row) => row.candidate_id);
  const survivors = [];
  for (const [candidateId, candidateRows] of byCandidate) {
    const ranked = candidateRows.sort(recipeComparator);
    const best = ranked[0];
    for (const row of ranked) {
      if (row.attempts === 0) continue;
      if (row.mechanically_eligible_rate >= 0.6 && row.mechanically_eligible_rate >= best.mechanically_eligible_rate - 0.15) {
        survivors.push({ candidate_id: candidateId, recipe_id: row.recipe_id, mechanically_eligible_rate: row.mechanically_eligible_rate, reference_token_f1_mean: row.reference_token_f1_mean });
      }
    }
    if (!survivors.some((row) => row.candidate_id === candidateId) && best) survivors.push({ candidate_id: candidateId, recipe_id: best.recipe_id, mechanically_eligible_rate: best.mechanically_eligible_rate, reference_token_f1_mean: best.reference_token_f1_mean });
  }
  const output = { schema: "heardright.contextual_repair_survivors.v1", created_at: new Date().toISOString(), selection_note: "Before blind ratings, mechanical eligibility and cloud-output similarity are tuning proxies only; they are never accepted quality or speed parity. Human critical failures dominate once ratings exist.", survivors, summaries: summaries.map(({ rows: _rows, ...summary }) => summary) };
  const file = path.join(runRoot, "private", "survivors.stage_a.json");
  writeJson(file, output);
  console.log(JSON.stringify({ status: "stage_a_scored", candidates: byCandidate.size, survivors: survivors.length, file }, null, 2));
}

function freezeWinners(references, ratings) {
  const tuneA = loadPhaseResults("tune_a");
  const tuneB = loadPhaseResults("tune_b");
  const summaries = groupSummaries([...tuneA, ...tuneB], references, ratings);
  const recipesFile = path.join(runRoot, "candidate-visible", "candidate-recipes.json");
  const recipes = readJson(recipesFile);
  const byCandidate = Map.groupBy(summaries, (row) => row.candidate_id);
  const expectedTuneAttempts = new Set(readJsonl(path.join(runRoot, "private", "references.tune.jsonl")).map((row) => row.case_id ?? row.private_reference?.case_id)).size;
  if (expectedTuneAttempts < 1) throw new Error("cannot freeze: tuning references are empty");
  if (byCandidate.size !== recipes.candidates.length) throw new Error(`cannot freeze: have results for ${byCandidate.size}/${recipes.candidates.length} candidates`);
  const winners = [];
  for (const candidate of recipes.candidates) {
    const ranked = (byCandidate.get(candidate.id) ?? []).filter((row) => row.attempts === expectedTuneAttempts).sort(recipeComparator);
    if (!ranked.length) throw new Error(`cannot freeze ${candidate.id}: every recipe needs all ${expectedTuneAttempts} tuning attempts`);
    const winner = ranked[0];
    const recipe = candidate.recipes.find((row) => row.id === winner.recipe_id);
    winners.push({
      candidate_id: candidate.id,
      recipe_id: winner.recipe_id,
      recipe_sha256: sha256(JSON.stringify(recipe)),
      tuning_result_sha256: sha256(JSON.stringify(winner.rows.map((row) => ({ row_id: row.row_id, status: row.status, materialized_output: row.materialized_output, cleanup_wall_ms: row.cleanup_wall_ms, mechanical_flags: row.mechanical_flags })))),
      accepted_rate: winner.accepted_rate,
      mechanically_eligible_rate: winner.mechanically_eligible_rate,
      human_review_complete: winner.human_review_complete,
      selection_basis: winner.human_review_complete ? "blind_human_acceptance_then_quality_then_speed" : "mechanical_and_cloud_reference_tuning_proxy_pending_blind_review",
      critical_failures: winner.critical_failures,
      reference_token_f1_mean: winner.reference_token_f1_mean,
      cleanup_wall_accepted_p95_ms: winner.cleanup_wall_accepted_p95_ms,
    });
  }
  const freeze = { schema: "heardright.contextual_repair_winners.v1", benchmark_id: "l1-local-contextual-repair-v1", frozen_at: new Date().toISOString(), recipes_sha256: sha256(fs.readFileSync(recipesFile)), winners };
  const file = path.join(runRoot, "private", "winners.freeze.json");
  writeJson(file, freeze);
  console.log(JSON.stringify({ status: "winners_frozen_holdout_not_run", winners: winners.length, file, freeze_sha256: sha256(fs.readFileSync(file)) }, null, 2));
}

function report(phase, references, ratings) {
  const cloudYardstick = loadCloudYardstick();
  const summaries = groupSummaries(loadPhaseResults(phase), references, ratings, cloudYardstick).sort(recipeComparator);
  const compact = summaries.map(({ rows: _rows, ...summary }) => summary);
  const output = {
    schema: "heardright.contextual_repair_report.v1",
    benchmark_id: "l1-local-contextual-repair-v1",
    phase,
    created_at: new Date().toISOString(),
    cloud_cleanup_stage_wall_ms: cloudYardstick,
    interpretation: "Accepted-output speed parity is null until every included output has an explicit blind human rating. Mechanical eligibility, reference similarity, and mechanically eligible latency are tuning diagnostics only.",
    summaries: compact,
  };
  const file = path.join(runRoot, "reports", `${phase}.summary.json`);
  writeJson(file, output);
  console.log(JSON.stringify({ status: "report_written", phase, candidates: summaries.length, file }, null, 2));
}

function main() {
  const references = loadReferences();
  const ratings = loadRatings();
  if (process.argv.includes("--stage-a")) return stageA(references, ratings);
  if (process.argv.includes("--freeze-winners")) return freezeWinners(references, ratings);
  if (process.argv.includes("--report")) return report(valueAfter("--phase", "holdout"), references, ratings);
  throw new Error("choose --stage-a, --freeze-winners, or --report --phase <phase>");
}

try { main(); } catch (error) {
  console.error(error.stack ?? String(error));
  process.exitCode = 1;
}
