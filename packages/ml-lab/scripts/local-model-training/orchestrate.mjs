#!/usr/bin/env node
import { createHash } from "node:crypto";
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { basename, dirname, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createSyncRequest, logicalBytes, pruneSyncedCheckpoint, verifySyncReceipt } from "./checkpoint_lifecycle.mjs";
import { validatePacket, validateSmokeGateReceipt } from "./finalize_smoke_gate.mjs";

const ROOT = dirname(fileURLToPath(import.meta.url));
const PACKET_ROOT = resolve(ROOT, "..", "..");
const RECIPES_PATH = resolve(ROOT, "model-recipes.json");
const TRAIN = resolve(ROOT, "train.py");
const CAPTURE = resolve(ROOT, "capture_native_assets.py");
const PREPARE_SOURCE = resolve(ROOT, "prepare_source_checkpoint.py");
const CONVERTER_ADAPTER = resolve(ROOT, "convert_hf_to_q8.py");
const GGUF_EVALUATOR = resolve(ROOT, "evaluate_gguf.py");
const VALIDATOR = resolve(ROOT, "validate_output.mjs");
const AUTOTUNE = resolve(ROOT, "autotune_fullft_batch.py");
const BACKEND_SELECTOR = resolve(ROOT, "select_full_weight_backend.py");
const NATIVE_FULL_TRAINER = resolve(ROOT, "train_transformers_native_full.py");
const NATIVE_FULL_FINALIZER = resolve(ROOT, "finalize_transformers_native_full.py");
const SOURCE_RESOLVER = resolve(ROOT, "resolve_exact_model_sources.py");
const SOURCE_POLICY_PATH = resolve(PACKET_ROOT, "bakeoff", "l1-four-family-training-v1", "evaluation-policy.v2.json");
const SOURCE_POLICY_CONTROL_PATH = resolve(PACKET_ROOT, "bakeoff", "l1-four-family-training-v1", "evaluation-policy-control.v1.json");
const POLICY_PATH = existsSync(resolve(PACKET_ROOT, "evaluation-policy.v2.json")) ? resolve(PACKET_ROOT, "evaluation-policy.v2.json") : SOURCE_POLICY_PATH;
const POLICY_CONTROL_PATH = existsSync(resolve(PACKET_ROOT, "evaluation-policy-control.v1.json")) ? resolve(PACKET_ROOT, "evaluation-policy-control.v1.json") : SOURCE_POLICY_CONTROL_PATH;
const DEFAULT_LANES = ["falcon", "granite", "gemma", "sotto"];
const OPT_IN_LANES = ["falcon_1_5b", "falcon_1_5b_deep"];
const IMAGE = "pytorch/pytorch:2.7.1-cuda12.6-cudnn9-devel@sha256:639b8229ccfd8a3aa803cf49c33d6d6fe406750d79aaf723fe8c0eb1060d8cff";
const fail = (message) => { throw new Error(message); };
const value = (argv, name) => { const index = argv.indexOf(name); return index < 0 ? undefined : argv[index + 1]; };
const atomic = (path, object) => { const temporary = `${path}.tmp-${process.pid}`; writeFileSync(temporary, `${JSON.stringify(object, null, 2)}\n`); renameSync(temporary, path); };
const shaBuffer = (buffer) => `sha256:${createHash("sha256").update(buffer).digest("hex")}`;
const sha = (path) => shaBuffer(readFileSync(path));
const canonicalHash = (value) => shaBuffer(Buffer.from(JSON.stringify(value)));

function treeHash(path) {
  const root = resolve(path);
  const entries = [];
  const walk = (current, prefix = "") => {
    for (const item of readdirSync(current, { withFileTypes: true })) {
      const itemPath = resolve(current, item.name), relative = prefix ? `${prefix}/${item.name}` : item.name;
      if (item.isDirectory()) walk(itemPath, relative);
      else if (item.isFile()) entries.push([relative, statSync(itemPath).size, sha(itemPath)]);
    }
  };
  walk(root);
  return canonicalHash(entries);
}

function options(argv) {
  const dry = argv.includes("--dry-run"), live = argv.includes("--live"), outputDir = value(argv, "--output-dir");
  if (dry === live) fail("choose exactly one of --dry-run or --live");
  if (!outputDir) fail("--output-dir is required");
  const profile = value(argv, "--profile") ?? "full";
  if (!["smoke", "full"].includes(profile)) fail("--profile must be smoke or full");
  return {
    dry, live, profile, outputDir: resolve(outputDir), authorization: value(argv, "--live-authorization"),
    recipes: resolve(value(argv, "--recipes") ?? RECIPES_PATH), environment: value(argv, "--environment"),
    converter: value(argv, "--llama-cpp-converter"), llamaCli: value(argv, "--llama-cli"),
    trainingInput: value(argv, "--training-input"), evalInput: value(argv, "--eval-input"),
    gemmaPreflightInput: value(argv, "--gemma-preflight-input"), python: value(argv, "--python") ?? "python",
    evaluationPolicy: resolve(value(argv, "--evaluation-policy") ?? POLICY_PATH),
    evaluationPolicyControl: resolve(value(argv, "--evaluation-policy-control") ?? POLICY_CONTROL_PATH),
    includeOptInCandidates: argv.includes("--include-opt-in-candidates"),
    syncTimeoutSeconds: Number(value(argv, "--sync-timeout-seconds") ?? 43_200),
  };
}

function loadRecipes(path) {
  if (!existsSync(path)) fail(`recipe manifest is missing: ${path}`);
  const recipes = JSON.parse(readFileSync(path, "utf8"));
  for (const family of DEFAULT_LANES) if (!recipes.families?.[family]) fail(`recipe family missing: ${family}`);
  for (const family of DEFAULT_LANES) if (!Array.isArray(recipes.families[family].training_backend?.priority) || recipes.families[family].training_backend.priority.length === 0) fail(`recipe family missing full-weight backend priority: ${family}`);
  if (!recipes.gemma_preflight?.stock_source_id || !recipes.gemma_preflight?.grmr_source_id) fail("Gemma preflight source bindings are missing");
  if (!Array.isArray(recipes.candidate_pool?.falcon_h1_1_5b) || recipes.candidate_pool.falcon_h1_1_5b.length !== 2) fail("two opt-in Falcon 1.5B candidates are required");
  return recipes;
}

function loadEvaluationPolicy(policyPath, controlPath) {
  if (!existsSync(policyPath) || !existsSync(controlPath)) fail("evaluation policy or policy control is missing");
  const policy = JSON.parse(readFileSync(policyPath, "utf8")), control = JSON.parse(readFileSync(controlPath, "utf8"));
  if (policy.schema_version !== "heardright.l1.full_weight_evaluation_policy.v2" || control.schema_version !== "heardright.l1.evaluation_policy_control.v1") fail("unsupported evaluation policy schema");
  if (sha(policyPath) !== control.policy?.sha256) fail("evaluation policy hash does not match policy control");
  if (policy.roster?.full_weight_only !== true || policy.evidence_sets?.sealed_product_holdout?.never_leaves_local_private_storage !== true) fail("evaluation policy does not enforce full-weight-only training and local-only holdout isolation");
  if (policy.checkpoint_sync_and_prune?.vast_disk_cap_gb !== 50) fail("evaluation policy must bind the 50 GB Vast disk cap");
  return { path: policyPath, sha256: sha(policyPath), control_path: controlPath, control_sha256: sha(controlPath), policy, control };
}

function authorize(parsed) {
  if (!parsed.live) return null;
  if (!parsed.authorization || !existsSync(parsed.authorization)) fail("live authorization is required via --live-authorization <file>");
  const authorization = JSON.parse(readFileSync(parsed.authorization, "utf8"));
  if (authorization?.approved !== true || authorization?.scope !== "heardright_l1_four_family_training") fail("live authorization does not approve heardright_l1_four_family_training");
  if (authorization.profile !== parsed.profile) fail(`authorization profile must exactly match ${parsed.profile}`);
  if (typeof authorization.offer_id !== "string" || !authorization.offer_id.trim()) fail("authorization must bind the exact Vast offer_id");
  if (!(authorization.hourly_rate_usd > 0) || !(authorization.max_usd > 0 && authorization.max_usd <= 20) || !(authorization.max_hours > 0 && authorization.max_hours <= 24)) fail("authorization must bind a positive hourly rate and caps no greater than USD 20 / 24 hours");
  let smokeGate = null;
  if (parsed.profile === "full") {
    if (!authorization.smoke_gate_receipt || !/^sha256:[0-9a-f]{64}$/.test(authorization.smoke_gate_receipt_sha256 ?? "")) fail("full authorization requires smoke_gate_receipt and smoke_gate_receipt_sha256");
    smokeGate = validateSmokeGateReceipt(authorization.smoke_gate_receipt, PACKET_ROOT);
    if (smokeGate.sha256 !== authorization.smoke_gate_receipt_sha256) fail("full authorization smoke gate receipt hash mismatch");
  }
  return { path: resolve(parsed.authorization), sha256: sha(parsed.authorization), profile: authorization.profile, offer_id: authorization.offer_id, hourly_rate_usd: authorization.hourly_rate_usd, max_usd: authorization.max_usd, max_hours: authorization.max_hours, smoke_gate: smokeGate };
}

export function validateExactTokenizerPreflight(binding, recipesPath) {
  const tokenizerPath = resolve(binding?.path ?? "");
  if (!existsSync(tokenizerPath) || sha(tokenizerPath) !== binding?.sha256) fail("runtime exact tokenizer preflight receipt is missing or hash-mismatched");
  const tokenizer = JSON.parse(readFileSync(tokenizerPath, "utf8"));
  const audited = Array.isArray(tokenizer.candidates) && tokenizer.candidates.length === 6 && tokenizer.candidates.every((item) => item.integer_token_sequence === true && Number.isInteger(item.encoded_tokens) && item.encoded_tokens > 0 && Number.isInteger(item.token_id_min) && Number.isInteger(item.token_id_max) && /^sha256:[0-9a-f]{64}$/.test(item.input_ids_sha256 ?? ""));
  if (tokenizer.schema_version !== "heardright.l1.exact_tokenizer_preflight.v1" || tokenizer.status !== "passed" || tokenizer.transformers_version !== "5.5.0" || tokenizer.candidate_count !== 6 || !audited || tokenizer.recipes_sha256 !== sha(recipesPath)) fail("runtime exact tokenizer preflight did not pass and audit all six bound candidates under Transformers 5.5.0");
  return { path: tokenizerPath, sha256: sha(tokenizerPath), transformers_version: tokenizer.transformers_version, candidate_count: tokenizer.candidate_count };
}

function runtime(parsed) {
  if (!parsed.environment && !parsed.converter && !parsed.llamaCli) return null;
  if (!parsed.environment || !parsed.converter || !parsed.llamaCli) fail("runtime handoff requires --environment, --llama-cpp-converter, and --llama-cli together");
  const receiptPath = resolve(parsed.environment), converter = resolve(parsed.converter), cli = resolve(parsed.llamaCli);
  if (!existsSync(receiptPath) || !existsSync(converter) || !existsSync(cli)) fail("runtime receipt, converter, or cli is missing");
  const receipt = JSON.parse(readFileSync(receiptPath, "utf8")), requirements = resolve(receipt.requirements_path ?? "");
  if (receipt.schema_version !== 2 || receipt.container_image !== IMAGE || receipt.llama_cpp?.remote !== "https://github.com/ggml-org/llama.cpp.git" || !/^[0-9a-f]{40}$/.test(receipt.llama_cpp?.commit ?? "")) fail("runtime environment receipt has an unsupported schema, image, or llama.cpp revision");
  if (!existsSync(requirements) || sha(requirements) !== receipt.requirements_sha256) fail("runtime environment requirements hash mismatch");
  if (resolve(receipt.llama_cpp.converter ?? "") !== converter || resolve(receipt.llama_cpp.cli ?? "") !== cli) fail("runtime llama.cpp paths do not match environment receipt");
  const tokenizer = validateExactTokenizerPreflight(receipt.exact_tokenizer_preflight, parsed.recipes);
  return { receipt: receiptPath, receipt_sha256: sha(receiptPath), requirements, requirements_sha256: sha(requirements), exact_tokenizer_preflight: tokenizer, llama_cpp: { remote: receipt.llama_cpp.remote, commit: receipt.llama_cpp.commit, converter, converter_sha256: sha(converter), cli, cli_sha256: sha(cli) } };
}

function familySource(family, recipes) {
  const recipe = recipes.families[family];
  return family === "gemma" ? recipe.stock : recipe.source;
}

function laneSpec(id, recipes) {
  if (DEFAULT_LANES.includes(id)) return { id, family: id, source: familySource(id, recipes), opt_in: false };
  const candidate = recipes.candidate_pool.falcon_h1_1_5b[id === "falcon_1_5b" ? 0 : 1];
  return { id, family: "falcon", source: { id: candidate.id, revision: candidate.revision }, opt_in: true };
}

function lane(spec, recipes) {
  const stages = ["compatibility", "backend-preflight:full_parameter", "autotune:full_parameter", "baseline:development", "train:full_parameter:per_epoch", "sync:per_epoch", "evaluate:full_parameter:per_epoch", "select-or-rollback", "merge:full_parameter", "export-q8", "gguf-evaluate", "release-gpu"];
  return { family: spec.family, source: spec.source, opt_in: spec.opt_in, preflight: spec.family === "gemma" ? [recipes.families.gemma.stock, recipes.families.gemma.grmr] : [], stages };
}

function evaluationVector(path) {
  const artifact = JSON.parse(readFileSync(path, "utf8")), safety = artifact.aggregate?.safety ?? {};
  const hardFailures = Object.values(safety).reduce((sum, value) => sum + (Number(value) || 0), 0);
  const correctionMisses = Number(artifact.aggregate?.quality?.correction_misses);
  if (!Number.isFinite(correctionMisses)) fail(`evaluation lacks correction_misses required for epoch selection: ${path}`);
  return [hardFailures, correctionMisses];
}

function strictlyImproves(current, best) {
  if (current[0] !== 0) return false;
  for (let index = 0; index < current.length; index += 1) {
    if (current[index] < best[index]) return true;
    if (current[index] > best[index]) return false;
  }
  return false;
}

function policyAllowsStockBaselineExport(policy) {
  return policy.epoch_policy?.allow_stock_baseline_export === true;
}

export function candidateDisposition({ attempts, selectedEpoch, selectedManifest, selectedEvaluation, selectedVector }, policy) {
  const evidence = attempts.map((attempt) => ({ epoch: attempt.epoch, checkpoint_manifest: attempt.checkpoint_manifest, evaluation: attempt.evaluation, vector: attempt.vector, hard_invalid: attempt.vector[0] !== 0 }));
  if (!attempts.some((attempt) => attempt.vector[0] === 0)) return { status: "rejected_no_safe_checkpoint", exportable: false, selected_epoch: null, checkpoint_manifest: null, evaluation: null, vector: null, evidence };
  if (selectedVector[0] !== 0) fail("safe candidate exists but selection remains hard-invalid");
  if (selectedEpoch === 0 && !policyAllowsStockBaselineExport(policy)) return { status: "no_trained_improvement", exportable: false, selected_epoch: 0, checkpoint_manifest: selectedManifest, evaluation: selectedEvaluation, vector: selectedVector, evidence };
  return { status: "viable_candidate", exportable: true, selected_epoch: selectedEpoch, checkpoint_manifest: selectedManifest, evaluation: selectedEvaluation, vector: selectedVector, evidence, stock_baseline_export: selectedEpoch === 0 };
}

export function executeDispositionActions(disposition, { exportCandidate, cleanup }) {
  let exported = false;
  if (disposition.exportable) { exportCandidate(); exported = true; }
  return { status: disposition.status, exported, cleanup: cleanup() };
}

export function gemmaSourceDisposition({ stock, grmr }) {
  const normalized = [stock, grmr].map((candidate, index) => {
    const expectedName = index === 0 ? "stock" : "grmr", evaluation = candidate?.evaluation, vector = evaluation?.vector;
    const valid = candidate?.name === expectedName && typeof candidate.source?.id === "string" && typeof candidate.source?.revision === "string" && typeof evaluation?.path === "string" && /^sha256:[0-9a-f]{64}$/u.test(evaluation?.sha256 ?? "") && typeof evaluation?.receipt === "string" && /^sha256:[0-9a-f]{64}$/u.test(evaluation?.receipt_sha256 ?? "") && Array.isArray(vector) && vector.length >= 2 && vector.every((value) => Number.isFinite(value) && value >= 0);
    if (!valid) fail(`malformed Gemma preflight evidence for ${expectedName}`);
    return { candidate: expectedName, source: candidate.source, evaluation_artifact: evaluation.path, evaluation_sha256: evaluation.sha256, evaluation_receipt: evaluation.receipt, receipt_sha256: evaluation.receipt_sha256, vector: [...vector], safety_valid: vector[0] === 0 };
  });
  const safeCandidates = normalized.filter((item) => item.safety_valid).map((item) => item.candidate);
  if (safeCandidates.length > 0) return { kind: "selection_required", safe_candidates: safeCandidates, evaluations: normalized };
  const leastUnsafe = [...normalized].sort((left, right) => left.vector[0] - right.vector[0] || left.vector[1] - right.vector[1] || left.candidate.localeCompare(right.candidate))[0];
  return { kind: "provisional_selection", status: "selected_least_unsafe_for_training", selected_candidate: leastUnsafe.candidate, selected_source: leastUnsafe.source, selected_revision: leastUnsafe.source.revision, evaluations: normalized };
}

export function gemmaSourceDispositionFromFiles({ stock, grmr }) {
  const bound = [stock, grmr].map((candidate, index) => {
    const expectedName = index === 0 ? "stock" : "grmr";
    if (candidate?.name !== expectedName || !existsSync(candidate.evaluation_path) || !existsSync(candidate.receipt_path) || !receiptOutputsAreValid(candidate.receipt_path)) fail(`Gemma preflight evaluation receipt is invalid for ${expectedName}`);
    const receipt = JSON.parse(readFileSync(candidate.receipt_path, "utf8"));
    if (receipt.schema_version !== "heardright.l1.stage_receipt.v1" || receipt.id !== `gemma:evaluate:${expectedName}` || receipt.outputs?.[resolve(candidate.evaluation_path)] !== sha(candidate.evaluation_path)) fail(`Gemma preflight evaluation receipt is invalid for ${expectedName}`);
    return { name: expectedName, source: candidate.source, evaluation: { path: resolve(candidate.evaluation_path), sha256: sha(candidate.evaluation_path), receipt: resolve(candidate.receipt_path), receipt_sha256: sha(candidate.receipt_path), vector: evaluationVector(candidate.evaluation_path) } };
  });
  return gemmaSourceDisposition({ stock: bound[0], grmr: bound[1] });
}

export function settleGemmaSourceRejection(context, spec, marker, { cleanup }) {
  if (spec.family !== "gemma" || marker?.kind !== "lane_skip" || marker?.status !== "rejected_no_safe_source" || marker.selected_source !== null) fail("invalid Gemma source-rejection marker");
  const lane = context.state.lanes[spec.id];
  lane.source_preflight = marker;
  lane.selection = { status: marker.status, selected_source: null, evaluations: marker.evaluations };
  lane.status = "cleanup_pending_after_rejection";
  context.state.events.push({ kind: "family_rejected", lane: spec.id, status: marker.status, selected_source: null, evidence: marker.evaluations, at: new Date().toISOString() });
  context.persist();
  lane.cleanup = cleanup();
  lane.export_invoked = false;
  lane.train_invoked = false;
  lane.status = marker.status;
  context.persist();
  return lane.selection;
}

function waitForReceipt(path, timeoutSeconds) {
  const started = Date.now(), pause = new Int32Array(new SharedArrayBuffer(4));
  while (!existsSync(path)) {
    if ((Date.now() - started) / 1000 >= timeoutSeconds) fail(`timed out waiting for Windows checkpoint sync receipt: ${path}`);
    Atomics.wait(pause, 0, 0, 5_000);
  }
}

function syncEpochCheckpoint(context, laneId, epoch, checkpointManifest, evaluation) {
  const requests = resolve(context.outputDir, "sync-requests"), receipts = resolve(context.outputDir, "sync-receipts");
  mkdirSync(requests, { recursive: true }); mkdirSync(receipts, { recursive: true });
  const requestPath = resolve(requests, `${laneId}-epoch-${epoch}.request.json`), receiptPath = resolve(receipts, `${laneId}-epoch-${epoch}.receipt.json`);
  if (!existsSync(requestPath)) createSyncRequest({ lane: laneId, epoch, checkpointManifest, evaluation, trainingSha256: sha(context.parsed.trainingInput), policyControlSha256: context.evaluationPolicy.control_sha256, receiptPath, output: requestPath });
  context.state.events.push({ kind: "checkpoint_sync_requested", lane: laneId, epoch, request: requestPath, request_sha256: sha(requestPath), at: new Date().toISOString() });
  context.persist();
  waitForReceipt(receiptPath, context.parsed.syncTimeoutSeconds);
  const receipt = verifySyncReceipt(requestPath, receiptPath);
  context.state.events.push({ kind: "checkpoint_sync_verified", lane: laneId, epoch, receipt: receiptPath, receipt_sha256: sha(receiptPath), destination: receipt.destination, at: new Date().toISOString() });
  context.persist();
  return { epoch, checkpoint_manifest: checkpointManifest, checkpoint_dir: JSON.parse(readFileSync(checkpointManifest, "utf8")).checkpoint_dir, request: requestPath, receipt: receiptPath };
}

function enforceDiskWatermark(context, laneId, selectedManifest, rejectedEpoch) {
  const used = logicalBytes(context.outputDir), threshold = 45 * 1024 ** 3;
  if (used < threshold) return;
  const selectedDir = JSON.parse(readFileSync(selectedManifest, "utf8")).checkpoint_dir;
  const synced = context.state.lanes[laneId].synced_epochs ?? [];
  for (const item of synced) {
    if (item.epoch === rejectedEpoch || resolve(item.checkpoint_dir) === resolve(selectedDir) || !existsSync(item.checkpoint_dir)) continue;
    const result = pruneSyncedCheckpoint({ runRoot: context.outputDir, checkpointDir: item.checkpoint_dir, requestPath: item.request, receiptPath: item.receipt, protectedPaths: [selectedDir], usedThresholdBytes: threshold });
    if (result.pruned) context.state.events.push({ kind: "checkpoint_pruned_after_windows_sync", lane: laneId, epoch: item.epoch, removed: result.removed, used_bytes_before: result.used_bytes_before, at: new Date().toISOString() });
    if (logicalBytes(context.outputDir) < threshold) { context.persist(); return; }
  }
  fail(`run uses at least 45 GB and no safely synced unselected checkpoint can be pruned for ${laneId}; preserving the 5 GB reserve requires stopping`);
}

export function resumeCheckpointFromManifest(manifestPath) {
  const declared = resolve(JSON.parse(readFileSync(manifestPath, "utf8")).checkpoint_dir ?? "");
  if (existsSync(resolve(declared, "trainer_state.json")) || existsSync(resolve(declared, "trainer-state.json"))) return declared;
  const parent = dirname(declared);
  const candidates = readdirSync(parent, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && /^checkpoint-\d+$/u.test(entry.name))
    .map((entry) => resolve(parent, entry.name))
    .filter((candidate) => existsSync(resolve(candidate, "trainer_state.json")) || existsSync(resolve(candidate, "trainer-state.json")));
  if (candidates.length !== 1) fail(`unable to resolve one populated resume checkpoint from ${manifestPath}`);
  return candidates[0];
}

export function resumeTrainingArgument(manifestPath) {
  resumeCheckpointFromManifest(manifestPath);
  return resolve(manifestPath);
}

function outputHash(path) {
  if (!existsSync(path)) fail(`expected stage output is missing: ${path}`);
  return statSync(path).isDirectory() ? treeHash(path) : sha(path);
}

function stageReceiptPath(context, id) {
  return resolve(context.outputDir, "stage-receipts", `${id.replaceAll(":", "--")}.json`);
}

function runStage(context, id, command, outputs) {
  const stageDir = resolve(context.outputDir, "stage-receipts"), receiptPath = stageReceiptPath(context, id);
  mkdirSync(stageDir, { recursive: true });
  const commandSha = canonicalHash(command);
  if (existsSync(receiptPath)) {
    const receipt = JSON.parse(readFileSync(receiptPath, "utf8"));
    if (receipt.command_sha256 === commandSha && outputs.every((path) => existsSync(path) && receipt.outputs?.[resolve(path)] === outputHash(path))) {
      context.state.events.push({ kind: "stage_resumed", id, at: new Date().toISOString() });
      context.persist();
      return receipt;
    }
    fail(`stale or hash-mismatched stage receipt blocks resume: ${receiptPath}`);
  }
  const elapsedHours = (Date.now() - Date.parse(context.state.started_at)) / 3_600_000, estimatedUsd = elapsedHours * context.state.authorization.hourly_rate_usd;
  context.state.budget = { elapsed_hours: elapsedHours, estimated_usd: estimatedUsd, max_hours: context.state.authorization.max_hours, max_usd: context.state.authorization.max_usd };
  if (elapsedHours >= context.state.authorization.max_hours || estimatedUsd >= context.state.authorization.max_usd) fail(`authorized Vast budget exhausted before stage ${id}`);
  context.state.active_stage = id;
  context.state.events.push({ kind: "stage_started", id, at: new Date().toISOString(), command_sha256: commandSha });
  context.persist();
  const logPath = resolve(stageDir, `${id.replaceAll(":", "--")}.log`), fd = openSync(logPath, "a");
  let result;
  try { result = spawnSync(command[0], command.slice(1), { cwd: context.outputDir, stdio: ["ignore", fd, fd], timeout: 12 * 60 * 60 * 1000 }); }
  finally { closeSync(fd); }
  if (result.error) fail(`${id} failed to launch: ${result.error.message}`);
  if (result.status !== 0) fail(`${id} exited ${result.status}; inspect ${logPath}`);
  const receipt = { schema_version: "heardright.l1.stage_receipt.v1", id, command_sha256: commandSha, outputs: Object.fromEntries(outputs.map((path) => [resolve(path), outputHash(path)])), log: logPath, completed_at: new Date().toISOString() };
  atomic(receiptPath, receipt);
  context.state.active_stage = null;
  context.state.events.push({ kind: "stage_completed", id, at: receipt.completed_at, receipt: receiptPath, receipt_sha256: sha(receiptPath) });
  context.persist();
  return receipt;
}

export function runStageInDir(context, id, outputDir, command, outputs) {
  mkdirSync(outputDir, { recursive: true });
  return runStage(context, id, command, outputs);
}

function py(context, script, args) { return [context.parsed.python, script, ...args]; }
function trainArgs(family, action, outputDir, source, binding = null, resolution = null) {
  const args = ["--family", family, "--action", action, "--output-dir", outputDir, "--model-source", source.id, "--model-revision", source.revision, "--live"];
  if (binding) args.push("--selection-artifact", binding.path, "--selection-artifact-sha256", binding.sha256, "--selected-template-sha256", binding.template_sha256, "--selected-module-manifest-sha256", binding.modules_sha256);
  if (resolution) args.push("--resolved-model-source", resolution.resolved_path, "--resolved-source-receipt", resolution.receipt);
  return args;
}

function exactSourceResolution(context, source) {
  const matches = context.sourceResolution.entries.filter((entry) => entry.source_id === source.id && entry.revision === source.revision);
  if (matches.length !== 1 || !existsSync(matches[0].resolved_path) || basename(matches[0].resolved_path) !== source.revision) fail(`exact source resolution missing for ${source.id}@${source.revision}`);
  return { ...matches[0], receipt: context.sourceResolution.path, receipt_sha256: context.sourceResolution.sha256 };
}

function evaluationOutputs(outputDir) {
  return [resolve(outputDir, "normalized-evaluation.jsonl"), resolve(outputDir, "evaluation.json"), resolve(outputDir, "evaluate.manifest.json")];
}

export function durableEvaluationBinding(evaluationPath) {
  const evaluation = JSON.parse(readFileSync(evaluationPath, "utf8"));
  const input = resolve(String(evaluation.eval_input ?? "")), expectedRoot = dirname(resolve(evaluationPath));
  if (!existsSync(input) || dirname(input) !== expectedRoot || basename(input) !== "normalized-evaluation.jsonl") fail(`evaluation does not bind a durable normalized view: ${evaluationPath}`);
  const inputSha256 = sha(input);
  if (evaluation.eval_input_sha256 !== inputSha256) fail(`evaluation normalized view hash mismatch: ${evaluationPath}`);
  return { path: input, sha256: inputSha256 };
}

function writeGemmaBinding(context, key, source) {
  const capture = JSON.parse(readFileSync(resolve(context.outputDir, "native-assets.json"), "utf8")), entry = capture.entries[key];
  if (!entry || entry.source_id !== source.id || entry.revision !== source.revision) fail(`native capture binding missing for ${key}`);
  const root = resolve(context.outputDir, "gemma-bindings", key); mkdirSync(root, { recursive: true });
  const template = resolve(root, "chat-template.txt"), modules = resolve(root, "modules.json"), selection = resolve(root, "selection.json");
  writeFileSync(template, entry.chat_template, "utf8");
  atomic(modules, entry.modules);
  const payload = { schema_version: "heardright.l1.provisional_selection.v1", selected_model_source: source.id, selected_model_revision: source.revision, selected_template_sha256: sha(template), selected_module_manifest_sha256: sha(modules), provisional_for_preflight: true };
  atomic(selection, payload);
  return { path: selection, sha256: sha(selection), template_sha256: payload.selected_template_sha256, modules_sha256: payload.selected_module_manifest_sha256, source };
}

function gemmaPreflight(context) {
  const existing = context.state.lanes.gemma?.source_preflight;
  if (context.state.lanes.gemma?.status === "rejected_no_safe_source" && terminalLaneIsDurable(context, "gemma")) return existing;
  const root = resolve(context.outputDir, "gemma-preflight"), stock = context.recipes.families.gemma.stock, grmr = context.recipes.families.gemma.grmr;
  mkdirSync(root, { recursive: true });
  const bindings = { stock: writeGemmaBinding(context, "gemma_stock", stock), grmr: writeGemmaBinding(context, "gemma_grmr", grmr) };
  for (const name of ["stock", "grmr"]) {
    const source = name === "stock" ? stock : grmr, base = resolve(root, name), manifest = resolve(base, "train.manifest.json"), evaluation = resolve(base, "evaluation.json");
    const resolution = exactSourceResolution(context, source);
    runStageInDir(context, `gemma:prepare:${name}`, base, py(context, PREPARE_SOURCE, ["--model-source", source.id, "--model-revision", source.revision, "--resolved-model-source", resolution.resolved_path, "--output-dir", base]), [manifest]);
    runStageInDir(context, `gemma:evaluate:${name}`, base, py(context, TRAIN, [...trainArgs("gemma", "evaluate", base, source, bindings[name], resolution), "--checkpoint-artifact", manifest, "--eval-input", context.parsed.gemmaPreflightInput]), evaluationOutputs(base));
  }
  const selectionDir = resolve(root, "selection"), manifest = resolve(selectionDir, "gemma-preflight.manifest.json"); mkdirSync(selectionDir, { recursive: true });
  const sourceDisposition = gemmaSourceDispositionFromFiles({
    stock: { name: "stock", source: stock, evaluation_path: resolve(root, "stock", "evaluation.json"), receipt_path: stageReceiptPath(context, "gemma:evaluate:stock") },
    grmr: { name: "grmr", source: grmr, evaluation_path: resolve(root, "grmr", "evaluation.json"), receipt_path: stageReceiptPath(context, "gemma:evaluate:grmr") },
  });
  if (sourceDisposition.kind === "provisional_selection") {
    const selected = bindings[sourceDisposition.selected_candidate];
    if (!selected) fail("Gemma provisional source is missing its bound selection artifact");
    atomic(manifest, { schema_version: "heardright.l1.gemma_preflight.v2", status: sourceDisposition.status, selected_model_source: selected.source.id, selected_model_revision: selected.source.revision, selected_template_sha256: selected.template_sha256, selected_module_manifest_sha256: selected.modules_sha256, source_preflight_safety_valid: false, evaluations: sourceDisposition.evaluations });
    const receipt = recordInternalStage(context, "gemma:select-source", [manifest]);
    const marker = { kind: "selected_source", path: manifest, sha256: sha(manifest), template_sha256: selected.template_sha256, modules_sha256: selected.modules_sha256, source: selected.source, source_preflight_safety_valid: false, source_evidence: sourceDisposition.evaluations, source_disposition: sourceDisposition, receipt: stageReceiptPath(context, "gemma:select-source"), receipt_sha256: sha(stageReceiptPath(context, "gemma:select-source")) };
    context.state.lanes.gemma.source_preflight = marker; context.persist();
    return marker;
  }
  runStageInDir(context, "gemma:select-source", selectionDir, py(context, TRAIN, ["--family", "gemma", "--action", "gemma-preflight", "--output-dir", selectionDir, "--stock-artifact", resolve(root, "stock", "evaluation.json"), "--grmr-artifact", resolve(root, "grmr", "evaluation.json"), "--live"]), [manifest]);
  const selected = JSON.parse(readFileSync(manifest, "utf8"));
  const source = { id: selected.selected_model_source, revision: selected.selected_model_revision };
  const selectedName = source.id === stock.id && source.revision === stock.revision ? "stock" : source.id === grmr.id && source.revision === grmr.revision ? "grmr" : null;
  if (!selectedName || !sourceDisposition.safe_candidates.includes(selectedName)) fail("Gemma source selector returned an unsafe or unbound source");
  return { kind: "selected_source", path: manifest, sha256: sha(manifest), template_sha256: selected.selected_template_sha256, modules_sha256: selected.selected_module_manifest_sha256, source, source_evidence: sourceDisposition.evaluations };
}

function recordInternalStage(context, id, outputs) {
  const receiptPath = stageReceiptPath(context, id);
  if (existsSync(receiptPath) && receiptOutputsAreValid(receiptPath)) return JSON.parse(readFileSync(receiptPath, "utf8"));
  context.state.active_stage = { id, command: ["internal", id], started_at: new Date().toISOString() };
  context.state.events.push({ kind: "stage_started", id, at: context.state.active_stage.started_at }); context.persist();
  const completedAt = new Date().toISOString(), receipt = { schema_version: "heardright.l1.stage_receipt.v1", id, command_sha256: canonicalHash(["internal", id]), outputs: Object.fromEntries(outputs.map((path) => [resolve(path), outputHash(path)])), completed_at: completedAt };
  atomic(receiptPath, receipt);
  context.state.active_stage = null; context.state.events.push({ kind: "stage_completed", id, at: completedAt, receipt: receiptPath, receipt_sha256: sha(receiptPath) }); context.persist();
  return receipt;
}

function cleanupFamily(context, laneId, family, root) {
  const cleanupDir = resolve(root, "cleanup"), cleanupManifest = resolve(cleanupDir, "cleanup.manifest.json"), stageId = `${laneId}:release-gpu`;
  runStageInDir(context, stageId, cleanupDir, py(context, TRAIN, ["--family", family, "--action", "cleanup", "--output-dir", cleanupDir, "--live"]), [cleanupManifest]);
  const receipt = stageReceiptPath(context, stageId);
  return { manifest: cleanupManifest, manifest_sha256: sha(cleanupManifest), receipt, receipt_sha256: sha(receipt) };
}

function receiptOutputsAreValid(receiptPath) {
  if (!existsSync(receiptPath)) return false;
  const receipt = JSON.parse(readFileSync(receiptPath, "utf8"));
  return Object.entries(receipt.outputs ?? {}).every(([path, digest]) => existsSync(path) && outputHash(path) === digest);
}

function terminalLaneIsDurable(context, laneId) {
  const laneState = context.state.lanes[laneId];
  if (!new Set(["viable_candidate", "rejected_no_safe_checkpoint", "rejected_no_safe_source", "no_trained_improvement", "unsupported_for_full_weight_training"]).has(laneState.status)) return false;
  if (!laneState.cleanup?.receipt || !receiptOutputsAreValid(laneState.cleanup.receipt)) return false;
  if (laneState.status === "rejected_no_safe_source" && (!laneState.source_preflight?.receipt || sha(laneState.source_preflight.receipt) !== laneState.source_preflight.receipt_sha256 || !receiptOutputsAreValid(laneState.source_preflight.receipt))) return false;
  if (laneState.status !== "viable_candidate") return true;
  return [`${laneId}:export-q8`, `${laneId}:gguf-evaluate`].every((id) => receiptOutputsAreValid(stageReceiptPath(context, id)));
}

function runFamily(context, spec, gemmaBinding = null) {
  const { id: laneId, family } = spec;
  if (family === "gemma" && gemmaBinding?.kind === "lane_skip") return settleGemmaSourceRejection(context, spec, gemmaBinding, { cleanup: () => cleanupFamily(context, laneId, family, resolve(context.outputDir, laneId)) });
  const root = resolve(context.outputDir, laneId), recipe = context.recipes.families[family], source = gemmaBinding?.source ?? spec.source, binding = family === "gemma" ? gemmaBinding : null;
  const resolution = family === "gemma" ? exactSourceResolution(context, source) : null;
  mkdirSync(root, { recursive: true });
  const compatibilityDir = resolve(root, "compatibility");
  runStageInDir(context, `${laneId}:compatibility`, compatibilityDir, py(context, TRAIN, trainArgs(family, "compatibility", compatibilityDir, source, binding, resolution)), [resolve(compatibilityDir, "compatibility.manifest.json")]);
  const backendPreflightDir = resolve(root, "backend-preflight"), backendReceiptPath = resolve(backendPreflightDir, "receipt.json");
  runStageInDir(context, `${laneId}:backend-preflight:full_parameter`, backendPreflightDir, py(context, BACKEND_SELECTOR, ["--family", family, "--model-source", source.id, "--model-revision", source.revision, ...(resolution ? ["--resolved-model-source", resolution.resolved_path] : []), "--training-input", context.parsed.trainingInput, "--recipes", context.parsed.recipes, "--output", backendReceiptPath]), [backendReceiptPath]);
  const backendReceipt = JSON.parse(readFileSync(backendReceiptPath, "utf8"));
  context.state.lanes[laneId].backend_preflight = { receipt: backendReceiptPath, receipt_sha256: sha(backendReceiptPath), status: backendReceipt.status, selected: backendReceipt.selected, attempts: backendReceipt.attempts };
  if (backendReceipt.status === "unsupported_for_full_weight_training") {
    const disposition = { status: "unsupported_for_full_weight_training", exportable: false, selected_epoch: null, checkpoint_manifest: null, evaluation: null, vector: null, evidence: backendReceipt.attempts };
    context.state.lanes[laneId].selection = disposition;
    context.state.lanes[laneId].status = "cleanup_pending_after_rejection";
    context.state.events.push({ kind: "family_rejected", lane: laneId, status: disposition.status, selected_epoch: null, evidence: disposition.evidence, at: new Date().toISOString() });
    context.persist();
    const settled = executeDispositionActions(disposition, { exportCandidate: () => fail(`unsupported ${laneId} reached export`), cleanup: () => cleanupFamily(context, laneId, family, root) });
    context.state.lanes[laneId].cleanup = settled.cleanup;
    context.state.lanes[laneId].export_invoked = settled.exported;
    context.state.lanes[laneId].status = disposition.status;
    context.persist();
    return disposition;
  }
  if (backendReceipt.status !== "selected" || !backendReceipt.selected?.backend) fail(`invalid full-weight backend receipt for ${laneId}`);
  const trainingBackend = backendReceipt.selected.backend, optimizer = backendReceipt.selected.optimizer;
  if (!new Set(["unsloth_full", "transformers_native_full"]).has(trainingBackend)) fail(`unsupported selected training backend for ${laneId}: ${trainingBackend}`);
  if (!new Set(["adamw_8bit", "torch_adamw"]).has(optimizer)) fail(`backend preflight did not bind an optimizer for ${laneId}`);
  let batch = 2, accumulation = 8;
  if (context.parsed.profile === "full") {
    const autotuneDir = resolve(root, "fullft-autotune"), autotuneReceipt = resolve(autotuneDir, "receipt.json");
    runStageInDir(context, `${laneId}:autotune:full_parameter`, autotuneDir, py(context, AUTOTUNE, ["--family", family, "--backend", trainingBackend, "--optimizer", optimizer, "--model-source", source.id, "--model-revision", source.revision, ...(resolution ? ["--resolved-model-source", resolution.resolved_path] : []), "--training-input", context.parsed.trainingInput, "--recipes", context.parsed.recipes, "--output", autotuneReceipt]), [autotuneReceipt]);
    const selection = JSON.parse(readFileSync(autotuneReceipt, "utf8")).selected;
    batch = selection.device_batch; accumulation = selection.gradient_accumulation;
  }
  context.state.lanes[laneId].autotune = { training_backend: trainingBackend, optimizer, device_batch: batch, gradient_accumulation: accumulation, effective_batch: batch * accumulation };
  context.persist();
  const baselineDir = resolve(root, "baseline"), baselineManifest = resolve(baselineDir, "train.manifest.json"), baselineEvalDir = resolve(root, "baseline-evaluation"), baselineEval = resolve(baselineEvalDir, "evaluation.json");
  runStageInDir(context, `${laneId}:prepare:baseline`, baselineDir, py(context, PREPARE_SOURCE, ["--model-source", source.id, "--model-revision", source.revision, ...(resolution ? ["--resolved-model-source", resolution.resolved_path] : []), "--output-dir", baselineDir]), [baselineManifest]);
  runStageInDir(context, `${laneId}:evaluate:baseline`, baselineEvalDir, py(context, TRAIN, [...trainArgs(family, "evaluate", baselineEvalDir, source, binding, resolution), "--checkpoint-artifact", baselineManifest, "--eval-input", context.parsed.evalInput]), evaluationOutputs(baselineEvalDir));
  const fullEpochs = context.parsed.profile === "smoke" ? 1 : recipe.training.full_parameter_epochs;
  let priorManifest, selectedManifest = baselineManifest, selectedEval = baselineEval, bestVector = evaluationVector(baselineEval), selectedEpoch = 0;
  const attempts = [{ epoch: 0, checkpoint_manifest: baselineManifest, evaluation: baselineEval, vector: bestVector }];
  for (let epoch = 1; epoch <= fullEpochs; epoch += 1) {
    const epochDir = resolve(root, `full-epoch-${epoch}`), epochManifest = resolve(epochDir, "train.manifest.json");
    const resumeArgs = priorManifest ? ["--resume-from-checkpoint", resumeTrainingArgument(priorManifest)] : [];
    const trainer = trainingBackend === "transformers_native_full" ? NATIVE_FULL_TRAINER : TRAIN;
    const backendArgs = trainingBackend === "transformers_native_full" ? ["--recipes", context.parsed.recipes, "--optimizer", optimizer] : [];
    runStageInDir(context, `${laneId}:train:full_parameter:epoch-${epoch}`, epochDir, py(context, trainer, [...trainArgs(family, "train", epochDir, source, binding, resolution), "--training-input", context.parsed.trainingInput, "--mode", "full", "--epochs", String(epoch), "--batch-size", String(batch), "--gradient-accumulation", String(accumulation), ...backendArgs, ...resumeArgs]), [epochManifest]);
    const epochEvalDir = resolve(root, `full-evaluation-epoch-${epoch}`), epochEval = resolve(epochEvalDir, "evaluation.json");
    runStageInDir(context, `${laneId}:evaluate:full_parameter:epoch-${epoch}`, epochEvalDir, py(context, TRAIN, [...trainArgs(family, "evaluate", epochEvalDir, source, binding, resolution), "--checkpoint-artifact", epochManifest, "--eval-input", context.parsed.evalInput]), evaluationOutputs(epochEvalDir));
    if (context.parsed.profile === "full") {
      const synced = syncEpochCheckpoint(context, laneId, epoch, epochManifest, epochEval);
      context.state.lanes[laneId].synced_epochs = [...(context.state.lanes[laneId].synced_epochs ?? []), synced];
      context.persist();
    }
    const vector = evaluationVector(epochEval), improved = strictlyImproves(vector, bestVector);
    attempts.push({ epoch, checkpoint_manifest: epochManifest, evaluation: epochEval, vector });
    context.state.events.push({ kind: improved ? "epoch_selected" : "epoch_rejected_and_rollback", lane: laneId, epoch, vector, prior_best_vector: bestVector, at: new Date().toISOString() });
    priorManifest = epochManifest;
    if (improved) { selectedManifest = epochManifest; selectedEval = epochEval; bestVector = vector; selectedEpoch = epoch; }
    context.state.lanes[laneId].selection = { selected_epoch: selectedEpoch, checkpoint_manifest: selectedManifest, evaluation: selectedEval, vector: bestVector };
    context.persist();
    if (context.parsed.profile === "full") enforceDiskWatermark(context, laneId, selectedManifest, null);
  }
  const disposition = candidateDisposition({ attempts, selectedEpoch, selectedManifest, selectedEvaluation: selectedEval, selectedVector: bestVector }, context.evaluationPolicy.policy);
  context.state.lanes[laneId].selection = disposition;
  context.state.lanes[laneId].status = disposition.exportable ? "selected_for_export" : "cleanup_pending_after_rejection";
  context.state.events.push({ kind: disposition.exportable ? "family_selected_for_export" : "family_rejected", lane: laneId, status: disposition.status, selected_epoch: disposition.selected_epoch, evidence: disposition.evidence, at: new Date().toISOString() });
  context.persist();
  if (!disposition.exportable) {
    const settled = executeDispositionActions(disposition, { exportCandidate: () => fail(`non-exportable ${laneId} reached export`), cleanup: () => cleanupFamily(context, laneId, family, root) });
    context.state.lanes[laneId].cleanup = settled.cleanup;
    context.state.lanes[laneId].export_invoked = settled.exported;
    context.state.lanes[laneId].status = disposition.status;
    context.persist();
    return disposition;
  }
  if (context.parsed.profile === "full") {
    const lastAttempt = attempts.at(-1);
    enforceDiskWatermark(context, laneId, selectedManifest, lastAttempt.epoch === selectedEpoch ? null : lastAttempt.epoch);
  }
  const fullMergeDir = resolve(root, "full-merged");
  const mergeCommand = trainingBackend === "transformers_native_full"
    ? py(context, NATIVE_FULL_FINALIZER, ["--family", family, "--selected-checkpoint", selectedManifest, "--output-dir", fullMergeDir, "--live"])
    : py(context, TRAIN, [...trainArgs(family, "merge", fullMergeDir, source, binding, resolution), "--selected-checkpoint", selectedManifest]);
  runStageInDir(context, `${laneId}:merge:full_parameter`, fullMergeDir, mergeCommand, [resolve(fullMergeDir, "merged"), resolve(fullMergeDir, "merge.manifest.json")]);
  const selectedMerged = resolve(fullMergeDir, "merged");
  const selectedEvalInput = durableEvaluationBinding(selectedEval);
  const exportDir = resolve(root, "q8"), gguf = resolve(exportDir, `${laneId}-q8_0.gguf`), converterConfig = resolve(exportDir, "converter.json"); mkdirSync(exportDir, { recursive: true });
  atomic(converterConfig, { llama_cpp_converter: context.binding.llama_cpp.converter, llama_cpp_converter_sha256: context.binding.llama_cpp.converter_sha256 });
  runStageInDir(context, `${laneId}:export-q8`, exportDir, py(context, TRAIN, ["--family", family, "--action", "export-q8", "--output-dir", exportDir, "--source-checkpoint", selectedMerged, "--source-checkpoint-sha256", treeHash(selectedMerged), "--converter", CONVERTER_ADAPTER, "--converter-sha256", sha(CONVERTER_ADAPTER), "--converter-arg=--config", "--converter-arg", converterConfig, "--exported-artifact", gguf, "--eval-input", selectedEvalInput.path, "--eval-input-sha256", selectedEvalInput.sha256, "--validation-artifact", selectedEval, "--live"]), [gguf, resolve(exportDir, "export-q8.manifest.json")]);
  const ggufEvalDir = resolve(root, "gguf-evaluation"), ggufEval = resolve(ggufEvalDir, "evaluation.json"), evaluatorConfig = resolve(ggufEvalDir, "evaluator.json"); mkdirSync(ggufEvalDir, { recursive: true });
  atomic(evaluatorConfig, { llama_cli: context.binding.llama_cpp.cli, llama_cli_sha256: context.binding.llama_cpp.cli_sha256, validator: VALIDATOR, validator_sha256: sha(VALIDATOR) });
  runStageInDir(context, `${laneId}:gguf-evaluate`, ggufEvalDir, py(context, TRAIN, ["--family", family, "--action", "gguf-evaluate", "--output-dir", ggufEvalDir, "--gguf-artifact", gguf, "--gguf-artifact-sha256", sha(gguf), "--eval-input", selectedEvalInput.path, "--eval-input-sha256", selectedEvalInput.sha256, "--gguf-evaluator", GGUF_EVALUATOR, "--gguf-evaluator-sha256", sha(GGUF_EVALUATOR), "--gguf-evaluator-arg=--config", "--gguf-evaluator-arg", evaluatorConfig, "--gguf-evaluator-arg=--family", "--gguf-evaluator-arg", family, "--gguf-evaluator-arg=--model-source", "--gguf-evaluator-arg", source.id, "--gguf-evaluator-arg=--model-revision", "--gguf-evaluator-arg", source.revision, "--gguf-eval-output", ggufEval, "--model-source", source.id, "--model-revision", source.revision, "--live"]), [ggufEval, resolve(ggufEvalDir, "gguf-evaluate.manifest.json")]);
  context.state.lanes[laneId].cleanup = cleanupFamily(context, laneId, family, root);
  context.state.lanes[laneId].status = "viable_candidate";
  context.persist();
  return disposition;
}

export function summarizeRunStatus(lanes, laneOrder) {
  const statuses = laneOrder.map((id) => lanes[id]?.status);
  const viable = statuses.filter((status) => status === "viable_candidate").length;
  const rejected = statuses.filter((status) => status === "rejected_no_safe_checkpoint" || status === "rejected_no_safe_source" || status === "no_trained_improvement" || status === "unsupported_for_full_weight_training").length;
  if (viable === 0 && rejected === laneOrder.length) return "completed_no_viable_candidates";
  if (viable > 0 && rejected > 0 && viable + rejected === laneOrder.length) return "complete_with_rejections";
  if (viable === laneOrder.length) return "complete";
  fail(`lane outcomes are incomplete: ${JSON.stringify(Object.fromEntries(laneOrder.map((id) => [id, lanes[id]?.status ?? null])))}`);
}

export function runFamiliesSequentially(context, laneSpecs, gemmaBinding = null, execute = runFamily) {
  for (const spec of laneSpecs) {
    if (terminalLaneIsDurable(context, spec.id)) {
      context.state.events.push({ kind: "terminal_family_resumed", lane: spec.id, status: context.state.lanes[spec.id].status, at: new Date().toISOString() });
      context.persist();
      continue;
    }
    execute(context, spec, spec.family === "gemma" ? gemmaBinding : null);
  }
  return summarizeRunStatus(context.state.lanes, laneSpecs.map((spec) => spec.id));
}

export function main(argv = process.argv.slice(2)) {
  const parsed = options(argv), approval = authorize(parsed), recipes = loadRecipes(parsed.recipes), binding = runtime(parsed), evaluationPolicy = loadEvaluationPolicy(parsed.evaluationPolicy, parsed.evaluationPolicyControl);
  const laneOrder = [...DEFAULT_LANES, ...(parsed.includeOptInCandidates ? OPT_IN_LANES : [])], laneSpecs = laneOrder.map((id) => laneSpec(id, recipes));
  mkdirSync(parsed.outputDir, { recursive: true });
  const lock = resolve(parsed.outputDir, "active-run.lock"), statePath = resolve(parsed.outputDir, "state.json");
  if (existsSync(lock)) fail(`active training lock exists: ${lock}`);
  const prior = existsSync(statePath) ? JSON.parse(readFileSync(statePath, "utf8")) : null;
  if (parsed.live && prior && (prior.mode !== "live" || prior.profile !== parsed.profile || prior.recipes_sha256 !== sha(parsed.recipes) || prior.authorization?.sha256 !== approval.sha256 || prior.evaluation_policy?.sha256 !== evaluationPolicy.sha256 || JSON.stringify(prior.lane_order) !== JSON.stringify(laneOrder))) fail("existing run state is not bound to this live profile, recipe, policy, lane roster, and authorization");
  atomic(lock, { schema_version: "heardright.l1.active_lock.v1", pid: process.pid, mode: parsed.dry ? "dry_run" : "live", profile: parsed.profile });
  const startedAt = parsed.live && prior?.started_at ? prior.started_at : new Date().toISOString();
  const events = parsed.live && Array.isArray(prior?.events) ? [...prior.events, { kind: "run_resumed", at: new Date().toISOString() }] : [{ kind: "prepared", mode: parsed.dry ? "dry_run" : "live", profile: parsed.profile, at: startedAt }];
  const freshLanes = Object.fromEntries(laneSpecs.map((spec) => [spec.id, lane(spec, recipes)]));
  const lanes = parsed.live && prior?.lanes ? Object.fromEntries(laneSpecs.map((spec) => [spec.id, { ...freshLanes[spec.id], ...(prior.lanes[spec.id] ?? {}), family: freshLanes[spec.id].family, source: freshLanes[spec.id].source, opt_in: freshLanes[spec.id].opt_in, preflight: freshLanes[spec.id].preflight, stages: freshLanes[spec.id].stages }])) : freshLanes;
  const state = { schema_version: "heardright.l1.orchestrator_state.v4", status: "prepared", mode: parsed.dry ? "dry_run" : "live", profile: parsed.profile, started_at: startedAt, recipes_path: parsed.recipes, recipes_sha256: sha(parsed.recipes), evaluation_policy: { path: evaluationPolicy.path, sha256: evaluationPolicy.sha256, control_path: evaluationPolicy.control_path, control_sha256: evaluationPolicy.control_sha256 }, authorization: approval, runtime_binding: binding, lane_order: laneOrder, lanes, active_stage: null, active_lock: lock, events };
  const context = { parsed, recipes, binding, evaluationPolicy, outputDir: parsed.outputDir, state, sourceResolution: null, persist: () => atomic(statePath, state) };
  context.persist();
  try {
    if (parsed.live) {
      for (const path of [parsed.trainingInput, parsed.evalInput, ...(parsed.profile === "full" ? [parsed.gemmaPreflightInput] : [])]) if (!path || !existsSync(path)) fail("live training requires existing training, evaluation, and (for full runs) Gemma preflight inputs");
      if (parsed.profile === "full" && (sha(parsed.trainingInput) !== evaluationPolicy.policy.evidence_sets.training.sha256 || sha(parsed.evalInput) !== evaluationPolicy.policy.evidence_sets.development.sha256)) fail("training or development input does not match the frozen evaluation policy");
      if (parsed.profile === "smoke" && sha(parsed.trainingInput) !== sha(parsed.evalInput)) fail("smoke training and evaluation must use the same frozen two-row input");
      if (parsed.profile === "full" && sha(parsed.gemmaPreflightInput) !== evaluationPolicy.policy.evidence_sets.gemma_preflight.sha256) fail("Gemma preflight input does not match the frozen evaluation policy");
      if (!binding) fail("live training requires a bound runtime receipt");
      const packet = validatePacket(PACKET_ROOT);
      state.packet_binding = { manifest_sha256: packet.manifest_sha256, control_sha256: packet.control_sha256 };
      state.dataset_binding = { training_sha256: sha(parsed.trainingInput), evaluation_sha256: sha(parsed.evalInput) };
      state.training_contract = { mode: "full", lora: false };
      context.persist();
      const sourceResolutionPath = resolve(parsed.outputDir, "exact-source-resolution.json");
      runStageInDir(context, "global:resolve-exact-model-sources", resolve(parsed.outputDir, "source-resolution"), py(context, SOURCE_RESOLVER, ["--recipes", parsed.recipes, "--output", sourceResolutionPath]), [sourceResolutionPath]);
      const sourceResolution = JSON.parse(readFileSync(sourceResolutionPath, "utf8"));
      if (sourceResolution.schema_version !== "heardright.l1.exact_source_resolution.v1" || sourceResolution.status !== "passed" || sourceResolution.recipes_sha256 !== sha(parsed.recipes) || sourceResolution.entry_count !== 7) fail("exact model source resolution did not bind all candidate and Gemma preflight revisions");
      context.sourceResolution = { path: sourceResolutionPath, sha256: sha(sourceResolutionPath), entries: sourceResolution.entries };
      state.exact_source_resolution = { path: sourceResolutionPath, sha256: context.sourceResolution.sha256, entry_count: sourceResolution.entry_count }; context.persist();
      const captureSummary = resolve(parsed.outputDir, "native-assets.json");
      runStageInDir(context, "global:capture-native-assets", resolve(parsed.outputDir, "native-assets"), py(context, CAPTURE, ["--recipes", parsed.recipes, "--output-dir", resolve(parsed.outputDir, "native-assets"), "--summary", captureSummary]), [captureSummary]);
      const gemmaBinding = parsed.profile === "full" ? gemmaPreflight(context) : writeGemmaBinding(context, "gemma_stock", recipes.families.gemma.stock);
      state.status = "running"; context.persist();
      state.status = runFamiliesSequentially(context, laneSpecs, gemmaBinding);
      state.events.push({ kind: "run_complete", status: state.status, at: new Date().toISOString(), lane_count: laneOrder.length, viable_count: laneOrder.filter((id) => state.lanes[id].status === "viable_candidate").length, rejected_count: laneOrder.filter((id) => new Set(["rejected_no_safe_checkpoint", "rejected_no_safe_source", "no_trained_improvement", "unsupported_for_full_weight_training"]).has(state.lanes[id].status)).length });
    } else {
      state.events.push({ kind: "dry_run_complete", at: new Date().toISOString(), lane_count: laneOrder.length });
      state.status = "prepared";
    }
    state.active_lock = null; context.persist();
  } catch (error) {
    state.status = "failed"; state.error = error instanceof Error ? error.message : String(error); state.events.push({ kind: "run_failed", at: new Date().toISOString(), error: state.error }); state.active_lock = null; context.persist(); throw error;
  } finally { if (existsSync(lock)) rmSync(lock, { force: true }); }
  process.stdout.write(`${JSON.stringify({ status: state.status, state: statePath })}\n`);
  return state;
}

if (process.argv[1] && !process.execArgv.includes("-e") && (process.platform === "win32" ? basename(process.argv[1]).toLowerCase() === basename(fileURLToPath(import.meta.url)).toLowerCase() : basename(process.argv[1]) === basename(fileURLToPath(import.meta.url)))) {
  try { main(); } catch (error) { process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`); process.exitCode = 1; }
}
