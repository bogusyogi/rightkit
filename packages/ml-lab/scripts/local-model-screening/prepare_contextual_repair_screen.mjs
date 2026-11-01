#!/usr/bin/env node
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { assertBenchmarkCorpusShape, buildRequest, parseCapturedRow, sha256, stableJson } from "./contextual_repair_core.mjs";

const scriptDir = path.dirname(fileURLToPath(new URL(import.meta.url)));
const appRoot = path.resolve(scriptDir, "../..");
const defaultRecipes = path.join(appRoot, "bakeoff", "l1-local-contextual-repair-v1", "candidate-recipes.json");
const defaultCapture = path.join(process.env.LOCALAPPDATA ?? "", "HeardRightNext", "polish-payloads.jsonl");
const defaultCloudYardstick = path.resolve(appRoot, "..", "docs", "evidence", "l1-local-model-screening-setup-2026-07-20", "cloud-yardstick-current.json");
const defaultModelRoot = process.platform === "win32"
  ? "E:\\models\\heardright-local-screening"
  : path.join(os.homedir(), "heardright-local-screening");
const defaultRunRoot = path.join(process.env.LOCALAPPDATA ?? path.join(os.homedir(), ".local", "share"), "HeardRightNext", "bakeoffs", "l1-local-contextual-repair-v1");
const runtimeSourceNames = [
  "contextual_repair_core.mjs",
  "run_contextual_repair_screen.mjs",
  "run_contextual_repair_phase.mjs",
  "run_contextual_repair_overnight.mjs",
  "score_contextual_repair_screen.mjs",
  "start_contextual_repair_overnight_windows.ps1",
];

function valueAfter(flag, fallback) {
  const index = process.argv.indexOf(flag);
  return index === -1 ? fallback : process.argv[index + 1];
}

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function writeJsonl(file, rows) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${rows.map((row) => JSON.stringify(row)).join("\n")}\n`);
}

function fileHash(file) {
  return sha256(fs.readFileSync(file));
}

function runtimeFiles(modelRoot) {
  const llamaServer = path.resolve(valueAfter(
    "--llama-server",
    process.platform === "win32"
      ? path.join(modelRoot, "runtimes", "llama.cpp", "b10069", "cuda-12.4", "llama-server.exe")
      : path.join(modelRoot, "runtimes", "llama.cpp", "llama-server"),
  ));
  const files = [
    ...runtimeSourceNames.map((name) => ({ role: "runner_source", file: path.join(scriptDir, name) })),
    { role: "llama_server", file: llamaServer },
  ];
  for (const entry of files) if (!fs.existsSync(entry.file)) throw new Error(`runtime file missing: ${entry.file}`);
  return files.map((entry) => ({ ...entry, file: path.resolve(entry.file), sha256: fileHash(entry.file) }));
}

function loadCapturedCases(capturePath) {
  const availableRows = fs.readFileSync(capturePath, "utf8")
    .split(/\r?\n/)
    .filter(Boolean)
    .map((line, index) => ({ raw: JSON.parse(line), index }))
    .filter(({ raw }) => raw.prompt_version === "l1_app_polish_prompt_v5");
  const frozenRows = availableRows.slice(0, 144);
  if (frozenRows.length !== 144) throw new Error(`need the frozen first 144 prompt-v5 rows, found ${frozenRows.length}`);
  const rows = frozenRows
    .map(({ raw, index }) => ({ ...parseCapturedRow(raw), index }));
  const groups = new Map();
  for (const row of rows) {
    if (!groups.has(row.transcript_group_sha256)) groups.set(row.transcript_group_sha256, []);
    groups.get(row.transcript_group_sha256).push(row);
  }
  const cases = [...groups.entries()].map(([groupHash, members]) => {
    const ranked = [...members].sort((a, b) => Number(Boolean(a.cloud_reference)) - Number(Boolean(b.cloud_reference)) || a.index - b.index);
    const selected = ranked.at(-1);
    return {
      case_id: `c_${groupHash.slice(0, 16)}`,
      transcript_group_sha256: groupHash,
      transcript: selected.transcript,
      context: selected.context,
      input_sha256: sha256(stableJson({ transcript: selected.transcript, context: selected.context })),
      request_sha256: selected.request_sha256,
      captured_at: selected.captured_at,
      cloud_model: selected.cloud_model,
      cloud_provider: selected.cloud_provider,
      cloud_reference: selected.cloud_reference,
      cloud_error: selected.cloud_error,
      duplicate_count: members.length,
    };
  }).sort((a, b) => a.case_id.localeCompare(b.case_id));
  if (cases.length !== 116) throw new Error(`corpus contract mismatch: unique=${cases.length} (expected 116)`);
  return {
    availableCapturedRows: availableRows.length,
    capturedRows: rows.length,
    frozenCapturePrefixSha256: sha256(frozenRows.map(({ raw }) => stableJson(raw)).join("\n")),
    cases,
  };
}

function splitCases(cases) {
  const tune = cases.filter((_, index) => index % 2 === 0);
  const holdout = cases.filter((_, index) => index % 2 === 1);
  const tuneA = tune.slice(0, 16);
  const tuneB = tune.slice(tuneA.length);
  assertBenchmarkCorpusShape({ unique: cases.length, tune: tune.length, tuneA: tuneA.length, tuneB: tuneB.length, holdout: holdout.length });
  return { tune, tuneA, tuneB, holdout };
}

function publicCase(row, phase) {
  return {
    case_id: row.case_id,
    phase,
    transcript: row.transcript,
    context: row.context,
    input_sha256: row.input_sha256,
  };
}

function privateReference(row, split) {
  return {
    case_id: row.case_id,
    split,
    input_sha256: row.input_sha256,
    request_sha256: row.request_sha256,
    captured_at: row.captured_at,
    cloud_model: row.cloud_model,
    cloud_provider: row.cloud_provider,
    cloud_reference: row.cloud_reference,
    cloud_error: row.cloud_error,
    duplicate_count: row.duplicate_count,
  };
}

function resolveModels(modelRoot, recipes) {
  const lockPath = path.join(modelRoot, "manifest.lock.json");
  const lock = readJson(lockPath);
  const byId = new Map(lock.models.map((model) => [model.id, model]));
  const resolved = recipes.candidates.map((candidate) => {
    const locked = byId.get(candidate.id);
    if (!locked) throw new Error(`model lock is missing ${candidate.id}`);
    const modelPath = path.join(modelRoot, "models", candidate.id, candidate.model_file);
    if (!fs.existsSync(modelPath)) throw new Error(`model file is missing: ${modelPath}`);
    const actualBytes = fs.statSync(modelPath).size;
    if (actualBytes !== locked.resolved_bytes) throw new Error(`model size mismatch: ${candidate.id}`);
    if (locked.resolved_sha256 !== locked.expected_sha256 && locked.expected_sha256) throw new Error(`model lock SHA disagreement: ${candidate.id}`);
    return {
      ...candidate,
      model_path: modelPath,
      model_sha256: locked.resolved_sha256,
      model_bytes: actualBytes,
      resolved_revision: locked.resolved_revision,
    };
  });
  return { lockPath, lockSha256: fileHash(lockPath), candidates: resolved };
}

function buildTuneManifest(candidate, recipe, cases, phase) {
  return {
    schema: "heardright.contextual_repair_candidate_manifest.v1",
    benchmark_id: "l1-local-contextual-repair-v1",
    phase,
    candidate_id: candidate.id,
    recipe_id: recipe.id,
    output_mode: recipe.output_mode,
    model_path: candidate.model_path,
    model_sha256: candidate.model_sha256,
    model_bytes: candidate.model_bytes,
    rows: cases.map((captured) => ({
      row_id: `${candidate.id}__${recipe.id}__${captured.case_id}`,
      case_id: captured.case_id,
      input_sha256: captured.input_sha256,
      request: buildRequest(candidate, recipe, captured),
    })),
  };
}

function writeCandidatePackage(runRoot, recipes, resolved, split) {
  const visibleRoot = path.join(runRoot, "candidate-visible");
  fs.rmSync(visibleRoot, { recursive: true, force: true });
  fs.mkdirSync(visibleRoot, { recursive: true });
  const recipeSnapshot = path.join(visibleRoot, "candidate-recipes.json");
  writeJson(recipeSnapshot, recipes);
  writeJsonl(path.join(visibleRoot, "tune", "cases.tune_a.jsonl"), split.tuneA.map((row) => publicCase(row, "tune_a")));
  writeJsonl(path.join(visibleRoot, "tune", "cases.tune_b.jsonl"), split.tuneB.map((row) => publicCase(row, "tune_b")));
  const manifestLedger = [];
  for (const candidate of resolved.candidates) {
    for (const recipe of candidate.recipes) {
      for (const [phase, cases] of [["tune_a", split.tuneA], ["tune_b", split.tuneB]]) {
        const manifest = buildTuneManifest(candidate, recipe, cases, phase);
        const file = path.join(visibleRoot, "tune", "manifests", candidate.id, `${recipe.id}.${phase}.json`);
        writeJson(file, manifest);
        manifestLedger.push({ candidate_id: candidate.id, recipe_id: recipe.id, phase, rows: cases.length, file, sha256: fileHash(file) });
      }
    }
  }
  return { visibleRoot, recipeSnapshot, manifestLedger };
}

function writePrivatePackage(runRoot, capturePath, recipesPath, resolved, split, corpusStats, visible) {
  const privateRoot = path.join(runRoot, "private");
  fs.rmSync(privateRoot, { recursive: true, force: true });
  fs.mkdirSync(privateRoot, { recursive: true });
  writeJsonl(path.join(privateRoot, "references.tune.jsonl"), split.tune.map((row) => privateReference(row, "tune")));
  writeJsonl(path.join(privateRoot, "sealed-holdout.jsonl"), split.holdout.map((row) => ({
    ...publicCase(row, "holdout"),
    private_reference: privateReference(row, "holdout"),
  })));
  const cloudYardstickPath = path.resolve(valueAfter("--cloud-yardstick", defaultCloudYardstick));
  const cloudYardstick = readJson(cloudYardstickPath);
  if (cloudYardstick.schema !== "heardright.contextual_repair_cloud_yardstick.v1") throw new Error("cloud yardstick schema mismatch");
  if (!cloudYardstick.cleanup_stage_wall_ms?.sample_count || !cloudYardstick.provider_total_ms?.sample_count) throw new Error("cloud yardstick has no frozen samples");
  const control = {
    schema: "heardright.contextual_repair_control.v1",
    benchmark_id: "l1-local-contextual-repair-v1",
    created_at: new Date().toISOString(),
    status: "prepared_not_run",
    objective: "Select the best deployable recipe independently for each model, freeze it on tuning data, then evaluate once on untouched holdout data.",
    corpus: {
      capture_path: capturePath,
      prompt_version: "l1_app_polish_prompt_v5",
      available_captured_rows_at_prepare: corpusStats.availableCapturedRows,
      captured_rows: corpusStats.capturedRows,
      frozen_capture_prefix_sha256: corpusStats.frozenCapturePrefixSha256,
      unique_transcript_groups: corpusStats.cases.length,
      split_unit: "deduplicated transcript group",
      duplicate_groups_collapsed_before_split: corpusStats.capturedRows - corpusStats.cases.length,
      tune_cases: split.tune.length,
      tune_a_cases: split.tuneA.length,
      tune_b_cases: split.tuneB.length,
      holdout_cases: split.holdout.length,
      transcript_group_overlap: 0,
      overlap_guarantee: "Each deduplicated transcript group is allocated exactly once before tune/holdout splitting.",
    },
    cloud_yardstick: {
      cleanup_stage_wall_ms: cloudYardstick.cleanup_stage_wall_ms,
      provider_total_ms: cloudYardstick.provider_total_ms,
      full_asr_plus_llm_ms: { p50: 762, p95: 945, label: "canonical June Qwen3.6 evidence" },
      frozen_evidence_path: cloudYardstickPath,
      frozen_evidence_sha256: fileHash(cloudYardstickPath),
      calculator_path: cloudYardstick.provenance?.calculator ?? null,
      calculator_sha256: cloudYardstick.provenance?.calculator_sha256 ?? null,
      comparison_rule: `Compare blind-human-accepted local cleanup wall p50/p95 with ${cloudYardstick.cleanup_stage_wall_ms.p50}/${cloudYardstick.cleanup_stage_wall_ms.p95} ms. Until ratings are complete, report mechanically eligible latency as tuning-only and speed parity as unavailable.`,
    },
    selection: {
      stage_a: "Run both bespoke recipes for every model on tune_a. Use mechanical eligibility and cloud-reference similarity only as tuning proxies until blind ratings exist.",
      stage_b: "Run surviving recipes on tune_b. If blind ratings are complete, select by human acceptance and quality; otherwise freeze the mechanical/reference proxy winner explicitly pending blind review.",
      freeze: "Write winners.freeze.json with candidate, recipe, recipe hash, result hash, and timestamp. No prompt or generation change is permitted afterward.",
      holdout: "Only the frozen recipe may see the sealed holdout. A changed recipe creates a new benchmark version.",
    },
    speed_metrics: [
      "cold_start_ms",
      "prompt_prefill_ms",
      "decode_ms",
      "model_generation_ms",
      "materialization_ms",
      "cleanup_wall_ms_all_attempts",
      "cleanup_wall_ms_accepted_outputs",
      "tokens_per_second",
      "peak_process_working_set_bytes",
      "peak_gpu_or_unified_memory_bytes"
    ],
    recipes_path: recipesPath,
    recipes_sha256: fileHash(visible.recipeSnapshot),
    model_lock_path: resolved.lockPath,
    model_lock_sha256: resolved.lockSha256,
    node_version: process.version,
    runtime_files: runtimeFiles(path.dirname(resolved.lockPath)),
    candidate_visible_root: visible.visibleRoot,
    candidate_visible_manifests: visible.manifestLedger,
  };
  const controlPath = path.join(privateRoot, "control.json");
  writeJson(controlPath, control);
  writeJson(path.join(privateRoot, "approval-ledger.template.json"), {
    schema: "heardright.contextual_repair_approval.v1",
    approved: false,
    approved_by: null,
    approved_at: null,
    control_sha256: fileHash(controlPath),
    recipes_sha256: control.recipes_sha256,
    permitted_phases: [],
  });
  return { privateRoot, controlPath, controlSha256: fileHash(controlPath) };
}

function main() {
  const capturePath = path.resolve(valueAfter("--capture", defaultCapture));
  const recipesPath = path.resolve(valueAfter("--recipes", defaultRecipes));
  const modelRoot = path.resolve(valueAfter("--model-root", defaultModelRoot));
  const runRoot = path.resolve(valueAfter("--run-root", defaultRunRoot));
  if (!fs.existsSync(capturePath)) throw new Error(`capture log is missing: ${capturePath}`);
  const recipes = readJson(recipesPath);
  if (recipes.candidates.length !== 19) throw new Error(`recipe roster must contain 19 candidates, found ${recipes.candidates.length}`);
  for (const candidate of recipes.candidates) {
    if (candidate.recipes.length < 2) throw new Error(`${candidate.id} needs at least two bespoke recipes`);
    const ids = new Set(candidate.recipes.map((recipe) => recipe.id));
    if (ids.size !== candidate.recipes.length) throw new Error(`${candidate.id} has duplicate recipe ids`);
  }
  const corpusStats = loadCapturedCases(capturePath);
  const split = splitCases(corpusStats.cases);
  const resolved = resolveModels(modelRoot, recipes);
  const visible = writeCandidatePackage(runRoot, recipes, resolved, split);
  const privatePackage = writePrivatePackage(runRoot, capturePath, recipesPath, resolved, split, corpusStats, visible);
  console.log(JSON.stringify({
    status: "prepared_not_run",
    run_root: runRoot,
    candidates: resolved.candidates.length,
    recipes: resolved.candidates.reduce((sum, candidate) => sum + candidate.recipes.length, 0),
    tune_a_cases: split.tuneA.length,
    tune_b_cases: split.tuneB.length,
    holdout_cases: split.holdout.length,
    tune_manifest_rows: visible.manifestLedger.reduce((sum, row) => sum + row.rows, 0),
    control_sha256: privatePackage.controlSha256,
  }, null, 2));
}

main();
