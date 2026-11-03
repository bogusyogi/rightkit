#!/usr/bin/env node
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, renameSync, statSync, writeFileSync } from "node:fs";
import { basename, dirname, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const DEFAULT_LANES = ["falcon", "granite", "gemma", "sotto"];
const ACCEPTED_RUN_STATUSES = new Set(["complete", "complete_with_rejections", "completed_no_viable_candidates"]);
const TERMINAL_LANE_STATUSES = new Set(["viable_candidate", "rejected_no_safe_checkpoint", "rejected_no_safe_source", "no_trained_improvement", "unsupported_for_full_weight_training"]);
const QUALITY_NEUTRAL_STATUSES = new Set(["rejected_no_safe_checkpoint", "rejected_no_safe_source", "no_trained_improvement", "unsupported_for_full_weight_training"]);
const RECOVERY_ALLOWED_FILES = new Set([
  "control.json",
  "scripts/local-model-training/orchestrate.mjs",
  "scripts/local-model-training/launch_vast.mjs",
  "scripts/local-model-training/checkpoint_lifecycle.mjs",
  "scripts/local-model-training/sync_checkpoints_windows.mjs",
  "scripts/local-model-training/test_checkpoint_lifecycle.mjs",
  "scripts/local-model-training/test_sync_checkpoints_windows.mjs",
  "scripts/local-model-training/finalize_smoke_gate.mjs",
  "scripts/local-model-training/test_finalize_smoke_gate.mjs",
  "scripts/local-model-training/paid_run_watchdog.py",
  "scripts/local-model-training/test_paid_run_watchdog.py",
  "scripts/local-model-training/finalize_remote_run_bundle.py",
  "scripts/local-model-training/test_finalize_remote_run_bundle.py",
]);
const LEGACY_V11 = {
  archive_sha256: "sha256:ff1fae20e95da34ab63f2c3a7f1ad66740785a71ad87f27462a27eb5c25f3d94",
  archive_manifest_sha256: "sha256:a5df73e89e11742b2853fb23102776342f264548d307f5a4777720d5b6833aed",
  packet_manifest_sha256: "sha256:03772bca5a1272d10155fdd79082244c8d6b215034cee47b5dc8bd90707c7ab0",
  packet_control_sha256: "sha256:82b07bd73a0cd8332c0c1eab8a71eb04eafe0049c4f17b014ec737e450e94e04",
  smoke_dataset_sha256: "sha256:f4a99a21cf6ef25a9de2c987b99c87af69146c010aa3bebef5cc830412a5913e",
  recipes_sha256: "sha256:c954218f0bfd892520dc7cea19a2fa99ccfbbbd6d82ba196be3b51c4eb6c69f2",
  requirements_sha256: "sha256:97bf2a8dceac027635e2644a74630764ea4a1fe3f01d2651e859d04fe5f89700",
};
const fail = (message) => { throw new Error(`Smoke gate: ${message}`); };
const shaBuffer = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const sha = (path) => shaBuffer(readFileSync(path));
const canonicalHash = (value) => shaBuffer(Buffer.from(JSON.stringify(value)));
const value = (argv, name) => { const index = argv.indexOf(name); return index < 0 ? undefined : argv[index + 1]; };

function treeHash(directory) {
  const root = resolve(directory), entries = [];
  const walk = (current, prefix = "") => {
    for (const item of readdirSync(current, { withFileTypes: true })) {
      const itemPath = resolve(current, item.name), relative = prefix ? `${prefix}/${item.name}` : item.name;
      if (item.isDirectory()) walk(itemPath, relative);
      else if (item.isFile()) entries.push([relative, statSync(itemPath).size, sha(itemPath)]);
    }
  };
  walk(root); return canonicalHash(entries);
}

const outputHash = (path) => statSync(path).isDirectory() ? treeHash(path) : sha(path);
const receiptName = (id) => `${id.replaceAll(":", "--")}.json`;

function externalPath(original, smokeRoot, runRoot) {
  if (typeof original !== "string" || !original) fail("receipt contains an invalid external path");
  if (existsSync(original)) return resolve(original);
  const normalized = original.replaceAll("\\", "/");
  for (const [marker, base] of [["/run/", runRoot], ["/runtime/", resolve(smokeRoot, "runtime")]]) {
    const index = normalized.lastIndexOf(marker);
    if (index >= 0) {
      const suffix = normalized.slice(index + marker.length).split("/").filter(Boolean);
      if (suffix.some((part) => part === "..")) fail("external receipt path escapes its smoke root");
      return resolve(base, ...suffix);
    }
  }
  fail(`cannot safely re-root external path: ${original}`);
}

function validateStageReceipt(path, smokeRoot, runRoot, expectedId = null, { allowArchivedMissingOutputs = false } = {}) {
  if (!existsSync(path)) fail(`stage receipt is missing: ${path}`);
  const receipt = JSON.parse(readFileSync(path, "utf8"));
  if (receipt.schema_version !== "heardright.l1.stage_receipt.v1" || !receipt.id || (expectedId && receipt.id !== expectedId)) fail(`invalid stage receipt: ${path}`);
  const outputs = Object.entries(receipt.outputs ?? {});
  if (outputs.length === 0) fail(`stage receipt has no hash-bound outputs: ${receipt.id}`);
  const archivedOmissions = [];
  for (const [original, expected] of outputs) {
    const local = externalPath(original, smokeRoot, runRoot);
    if (!existsSync(local)) {
      if (!allowArchivedMissingOutputs) fail(`stage output hash mismatch: ${receipt.id}: ${original}`);
      archivedOmissions.push({ path: original, sha256: expected });
      continue;
    }
    if (outputHash(local) !== expected) fail(`stage output hash mismatch: ${receipt.id}: ${original}`);
  }
  return { id: receipt.id, path: resolve(path), sha256: sha(path), output_count: outputs.length, archived_omitted_outputs: archivedOmissions };
}

export function validatePacket(packetDir) {
  const root = resolve(packetDir), manifestPath = resolve(root, "bundle-manifest.json"), controlPath = resolve(root, "control.json");
  if (!existsSync(manifestPath) || !existsSync(controlPath)) fail("packet manifest or control is missing");
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8")), control = JSON.parse(readFileSync(controlPath, "utf8"));
  if (manifest.control_sha256 !== sha(controlPath)) fail("packet manifest does not bind control");
  for (const item of manifest.files ?? []) {
    const path = resolve(root, ...item.path.split("/"));
    if (!existsSync(path) || statSync(path).size !== item.bytes || sha(path) !== item.sha256) fail(`packet file binding mismatch: ${item.path}`);
  }
  const scripts = resolve(root, "scripts", "local-model-training");
  for (const [name, expected] of Object.entries(control.runtime_files ?? {})) {
    const path = resolve(scripts, ...name.split("/"));
    if (!existsSync(path) || sha(path) !== expected) fail(`packet runtime binding mismatch: ${name}`);
  }
  return { root, manifest_path: manifestPath, manifest_sha256: sha(manifestPath), control_path: controlPath, control_sha256: sha(controlPath), manifest, control };
}

function archivedManifestEntries(path) {
  const entries = new Map();
  for (const line of readFileSync(path, "utf8").split(/\r?\n/u).filter(Boolean)) {
    const match = /^([0-9a-f]{64})\s+\.\/(.+)$/u.exec(line);
    if (!match) fail("legacy archive manifest contains an invalid row");
    if (entries.has(match[2])) fail(`legacy archive manifest duplicates a path: ${match[2]}`);
    entries.set(match[2], `sha256:${match[1]}`);
  }
  return entries;
}

function assertArchivedFile(path, smokeRoot, entries) {
  const key = relative(smokeRoot, path).split("\\").join("/");
  if (!existsSync(path) || entries.get(key) !== sha(path)) fail(`legacy extracted evidence is not archive-bound: ${key}`);
}

function validateLegacyV11Source(evidenceRoot, smokeRoot, statePath) {
  const archive = resolve(evidenceRoot, "heardright-v11-smoke-final-evidence.tar.gz"), archiveManifestPath = resolve(evidenceRoot, "heardright-v11-smoke-final-evidence.manifest.sha256");
  if (!existsSync(archive) || !existsSync(archiveManifestPath) || sha(archive) !== LEGACY_V11.archive_sha256 || sha(archiveManifestPath) !== LEGACY_V11.archive_manifest_sha256) fail("legacy v11 archive or archive manifest hash mismatch");
  const entries = archivedManifestEntries(archiveManifestPath), packetRoot = resolve(smokeRoot, "packet"), manifestPath = resolve(packetRoot, "bundle-manifest.json"), controlPath = resolve(packetRoot, "control.json");
  if (!existsSync(manifestPath) || !existsSync(controlPath) || sha(manifestPath) !== LEGACY_V11.packet_manifest_sha256 || sha(controlPath) !== LEGACY_V11.packet_control_sha256) fail("legacy v11 embedded packet manifest or control hash mismatch");
  assertArchivedFile(manifestPath, smokeRoot, entries); assertArchivedFile(controlPath, smokeRoot, entries); assertArchivedFile(statePath, smokeRoot, entries);
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8")), control = JSON.parse(readFileSync(controlPath, "utf8"));
  if (manifest.control_sha256 !== LEGACY_V11.packet_control_sha256 || control.smoke?.rows !== 2 || control.smoke?.sha256 !== LEGACY_V11.smoke_dataset_sha256) fail("legacy v11 control does not bind the exact two-row smoke dataset");
  const smokePath = resolve(packetRoot, "data", "smoke-2.jsonl"), recipesPath = resolve(packetRoot, "scripts", "local-model-training", "model-recipes.json");
  for (const [path, expected] of [[smokePath, LEGACY_V11.smoke_dataset_sha256], [recipesPath, LEGACY_V11.recipes_sha256]]) {
    if (!existsSync(path) || sha(path) !== expected) fail(`legacy v11 source file hash mismatch: ${path}`);
    assertArchivedFile(path, smokeRoot, entries);
  }
  const manifestFiles = new Map((manifest.files ?? []).map((item) => [item.path, item]));
  for (const [name, expected] of [["data/smoke-2.jsonl", LEGACY_V11.smoke_dataset_sha256], ["scripts/local-model-training/model-recipes.json", LEGACY_V11.recipes_sha256]]) if (manifestFiles.get(name)?.sha256 !== expected) fail(`legacy v11 manifest does not bind ${name}`);
  if (control.runtime_files?.["model-recipes.json"] !== LEGACY_V11.recipes_sha256 || control.runtime_files?.["requirements.vast-cu126.pins.txt"] !== LEGACY_V11.requirements_sha256) fail("legacy v11 control runtime bindings are invalid");
  if (control.training?.training_mode !== "full_parameter_only" || control.candidate_roster?.training_mode !== "full_parameter_only" || control.execution_gate?.full_parameter_only !== true) fail("legacy v11 control does not prove full-weight-only training");
  const recipes = JSON.parse(readFileSync(recipesPath, "utf8"));
  if (/lora/i.test(JSON.stringify({ control, recipes })) || !DEFAULT_LANES.every((lane) => recipes.families?.[lane]?.training_backend?.priority?.length > 0)) fail("legacy v11 control or recipes do not prove the no-LoRA full-weight contract");
  return { kind: "legacy_v11_archived_smoke", root: packetRoot, manifest_path: manifestPath, manifest_sha256: sha(manifestPath), control_path: controlPath, control_sha256: sha(controlPath), manifest, control, smoke_path: smokePath, smoke_sha256: sha(smokePath), recipes_path: recipesPath, recipes_sha256: sha(recipesPath), requirements_sha256: LEGACY_V11.requirements_sha256, archive_manifest_entries: entries, archive_binding: LEGACY_V11 };
}

function requiredLaneReceipts(laneId, lane, runRoot, smokeRoot, exactLegacyV11) {
  const stageDir = resolve(runRoot, "stage-receipts"), required = lane.status === "rejected_no_safe_source"
    ? ["gemma:prepare:stock", "gemma:evaluate:stock", "gemma:prepare:grmr", "gemma:evaluate:grmr", "gemma:select-source", "gemma:release-gpu"]
    : [`${laneId}:backend-preflight:full_parameter`, `${laneId}:release-gpu`];
  if (lane.status !== "unsupported_for_full_weight_training" && lane.status !== "rejected_no_safe_source") required.push(`${laneId}:evaluate:baseline`, `${laneId}:train:full_parameter:epoch-1`, `${laneId}:evaluate:full_parameter:epoch-1`);
  const receipts = required.map((id) => validateStageReceipt(resolve(stageDir, receiptName(id)), smokeRoot, runRoot, id, { allowArchivedMissingOutputs: exactLegacyV11 && id === `${laneId}:train:full_parameter:epoch-1` }));
  const cleanup = lane.cleanup;
  if (!cleanup?.receipt || !cleanup?.receipt_sha256) fail(`${laneId} has no cleanup receipt binding`);
  const cleanupPath = externalPath(cleanup.receipt, smokeRoot, runRoot);
  if (sha(cleanupPath) !== cleanup.receipt_sha256 || !receipts.some((item) => item.path === cleanupPath)) fail(`${laneId} cleanup receipt binding is invalid`);
  return receipts;
}

export function evaluateSmokeOutput(smokeOutput, packetDir) {
  const requestedRoot = resolve(smokeOutput), smokeRoot = existsSync(resolve(requestedRoot, "extracted", "run", "state.json")) ? resolve(requestedRoot, "extracted") : requestedRoot;
  const evidenceRoot = smokeRoot === requestedRoot ? dirname(smokeRoot) : requestedRoot;
  const runRoot = existsSync(resolve(smokeRoot, "run", "state.json")) ? resolve(smokeRoot, "run") : smokeRoot;
  const statePath = resolve(runRoot, "state.json");
  if (!existsSync(statePath)) fail("smoke state.json is missing");
  const targetPacket = validatePacket(packetDir), state = JSON.parse(readFileSync(statePath, "utf8"));
  if (state.mode !== "live" || state.profile !== "smoke" || !ACCEPTED_RUN_STATUSES.has(state.status)) fail("smoke run status/profile is not an accepted terminal infrastructure state");
  if (state.active_stage !== null || state.active_lock !== null || state.error || state.events?.some((event) => event.kind === "run_failed")) fail("smoke run contains an active stage, lock, or system/stage failure");
  if (JSON.stringify(state.lane_order) !== JSON.stringify(DEFAULT_LANES) || Object.keys(state.lanes ?? {}).sort().join(",") !== [...DEFAULT_LANES].sort().join(",")) fail("smoke run does not contain exactly the requested default-four lanes");
  const legacyCandidate = !state.packet_binding && (existsSync(resolve(evidenceRoot, "heardright-v11-smoke-final-evidence.tar.gz")) || existsSync(resolve(smokeRoot, "packet", "bundle-manifest.json")));
  const sourcePacket = legacyCandidate ? validateLegacyV11Source(evidenceRoot, smokeRoot, statePath) : { kind: "current_packet_smoke", ...targetPacket, smoke_path: resolve(targetPacket.root, "data", "smoke-2.jsonl"), smoke_sha256: sha(resolve(targetPacket.root, "data", "smoke-2.jsonl")), recipes_path: resolve(targetPacket.root, "scripts", "local-model-training", "model-recipes.json"), recipes_sha256: sha(resolve(targetPacket.root, "scripts", "local-model-training", "model-recipes.json")), requirements_sha256: sha(resolve(targetPacket.root, "scripts", "local-model-training", "requirements.vast-cu126.pins.txt")), archive_manifest_entries: null, archive_binding: null };
  const exactLegacyV11 = sourcePacket.kind === "legacy_v11_archived_smoke";
  const provesFullWeight = state.training_contract?.mode === "full" && state.training_contract?.lora === false || exactLegacyV11 && DEFAULT_LANES.every((id) => state.lanes[id]?.stages?.some((stage) => stage.includes("full_parameter")));
  if (!provesFullWeight || /lora/i.test(JSON.stringify(state.lanes))) fail("smoke run does not prove the no-LoRA full-weight contract");
  if (!exactLegacyV11 && (state.packet_binding?.manifest_sha256 !== sourcePacket.manifest_sha256 || state.packet_binding?.control_sha256 !== sourcePacket.control_sha256)) fail("smoke state is bound to a different packet or control");
  const smokeSha = sourcePacket.smoke_sha256;
  if (!exactLegacyV11 && (state.dataset_binding?.training_sha256 !== smokeSha || state.dataset_binding?.evaluation_sha256 !== smokeSha)) fail("smoke state dataset hash does not match the exact packet smoke dataset");
  if (state.recipes_sha256 !== sourcePacket.recipes_sha256) fail("smoke recipe hash does not match source smoke packet");
  const runtimePath = externalPath(state.runtime_binding?.receipt, smokeRoot, runRoot);
  if (!existsSync(runtimePath) || sha(runtimePath) !== state.runtime_binding?.receipt_sha256) fail("smoke runtime receipt hash mismatch");
  if (state.runtime_binding?.requirements_sha256 !== sourcePacket.requirements_sha256) fail("smoke runtime requirements do not match source smoke packet");
  if (exactLegacyV11) assertArchivedFile(runtimePath, smokeRoot, sourcePacket.archive_manifest_entries);
  const allStageReceipts = readdirSync(resolve(runRoot, "stage-receipts"), { withFileTypes: true }).filter((item) => item.isFile() && item.name.endsWith(".json")).map((item) => {
    const id = item.name.slice(0, -5).replaceAll("--", ":");
    const receiptPath = resolve(runRoot, "stage-receipts", item.name);
    if (exactLegacyV11) assertArchivedFile(receiptPath, smokeRoot, sourcePacket.archive_manifest_entries);
    return validateStageReceipt(receiptPath, smokeRoot, runRoot, null, { allowArchivedMissingOutputs: exactLegacyV11 && DEFAULT_LANES.some((laneId) => id === `${laneId}:train:full_parameter:epoch-1`) });
  });
  const completed = new Set((state.events ?? []).filter((event) => event.kind === "stage_completed").map((event) => event.id));
  const started = (state.events ?? []).filter((event) => event.kind === "stage_started").map((event) => event.id);
  if (started.some((id) => !completed.has(id))) fail("smoke contains a stage that started without completing");
  const lanes = [];
  for (const laneId of DEFAULT_LANES) {
    const lane = state.lanes[laneId];
    if (!TERMINAL_LANE_STATUSES.has(lane.status)) fail(`${laneId} is not terminal`);
    const receipts = requiredLaneReceipts(laneId, lane, runRoot, smokeRoot, exactLegacyV11);
    let backendPreflightSha256 = null, sourcePreflight = null;
    if (lane.status === "rejected_no_safe_source") {
      if (laneId !== "gemma" || lane.backend_preflight || !lane.source_preflight || lane.source_preflight.status !== "rejected_no_safe_source" || lane.source_preflight.selected_source !== null) fail("Gemma no-safe-source disposition is malformed");
      const preflight = lane.source_preflight, artifact = externalPath(preflight.artifact, smokeRoot, runRoot), receipt = externalPath(preflight.receipt, smokeRoot, runRoot);
      if (!existsSync(artifact) || sha(artifact) !== preflight.artifact_sha256 || !existsSync(receipt) || sha(receipt) !== preflight.receipt_sha256 || !receipts.some((item) => item.id === "gemma:select-source" && item.path === receipt)) fail("Gemma no-safe-source artifact or receipt binding is invalid");
      if (!Array.isArray(preflight.evaluations) || preflight.evaluations.length !== 2 || preflight.evaluations.map((item) => item.candidate).join(",") !== "stock,grmr") fail("Gemma no-safe-source evaluations are incomplete");
      for (const item of preflight.evaluations) {
        const evaluation = externalPath(item.evaluation_artifact, smokeRoot, runRoot), evaluationReceipt = externalPath(item.evaluation_receipt, smokeRoot, runRoot);
        if (!Array.isArray(item.vector) || item.vector.length < 2 || !item.vector.every((value) => Number.isFinite(value) && value >= 0) || item.vector[0] === 0 || !existsSync(evaluation) || sha(evaluation) !== item.evaluation_sha256 || !existsSync(evaluationReceipt) || sha(evaluationReceipt) !== item.receipt_sha256 || !receipts.some((bound) => bound.path === evaluationReceipt)) fail(`Gemma ${item.candidate} unsafe evaluation binding is invalid`);
      }
      if (allStageReceipts.some((item) => /^gemma:(?:compatibility|backend-preflight|autotune|train|merge|export|gguf-evaluate)/u.test(item.id))) fail("Gemma no-safe-source run contains forbidden downstream stages");
      sourcePreflight = { artifact_sha256: preflight.artifact_sha256, receipt_sha256: preflight.receipt_sha256, selected_source: null, evaluations: preflight.evaluations.map(({ candidate, vector, evaluation_sha256, receipt_sha256 }) => ({ candidate, vector, evaluation_sha256, receipt_sha256 })) };
    } else {
      if (!lane.backend_preflight?.receipt || !lane.backend_preflight?.receipt_sha256) fail(`${laneId} has no backend preflight binding`);
      const backendPath = externalPath(lane.backend_preflight.receipt, smokeRoot, runRoot);
      if (!existsSync(backendPath) || sha(backendPath) !== lane.backend_preflight.receipt_sha256) fail(`${laneId} backend preflight binding is invalid`);
      backendPreflightSha256 = lane.backend_preflight.receipt_sha256;
    }
    lanes.push({ lane: laneId, disposition: lane.status, model_quality_success: lane.status === "viable_candidate", infrastructure_supported_rejection: QUALITY_NEUTRAL_STATUSES.has(lane.status), backend_preflight_sha256: backendPreflightSha256, source_preflight: sourcePreflight, cleanup_receipt_sha256: lane.cleanup.receipt_sha256, required_stage_receipts: receipts.map(({ id, sha256 }) => ({ id, sha256 })) });
  }
  return { sourcePacket, targetPacket, smokeRoot, runRoot, statePath, state, runtimePath, smokeSha, allStageReceipts, lanes };
}

export function signedReceiptPayload(result) {
  return {
    schema_version: "heardright.l1.smoke_gate_receipt.v2",
    status: "infrastructure_pass",
    model_quality_interpretation: "neutral; completed_no_viable_candidates is not candidate success",
    source_smoke_packet: { kind: result.sourcePacket.kind, manifest_sha256: result.sourcePacket.manifest_sha256, control_sha256: result.sourcePacket.control_sha256, smoke_dataset_sha256: result.sourcePacket.smoke_sha256, recipes_sha256: result.sourcePacket.recipes_sha256, archive_sha256: result.sourcePacket.archive_binding?.archive_sha256 ?? null, archive_manifest_sha256: result.sourcePacket.archive_binding?.archive_manifest_sha256 ?? null },
    target_full_packet: { manifest_sha256: result.targetPacket.manifest_sha256, control_sha256: result.targetPacket.control_sha256 },
    smoke: { profile: "smoke", dataset_sha256: result.smokeSha, state_sha256: sha(result.statePath), runtime_receipt_sha256: sha(result.runtimePath), accepted_run_status: result.state.status },
    lane_order: DEFAULT_LANES,
    lanes: result.lanes,
    all_stage_receipts: result.allStageReceipts.map(({ id, sha256 }) => ({ id, sha256 })),
  };
}

function receiptPayload(path) {
  const receipt = JSON.parse(readFileSync(path, "utf8")), { signature, ...payload } = receipt;
  if (signature?.algorithm !== "sha256" || signature?.payload_sha256 !== canonicalHash(payload)) fail("smoke gate receipt signature or schema is invalid");
  return { receipt, payload, signature };
}

function manifestDiff(parentPacket, targetPacket) {
  const parent = new Map((parentPacket.manifest.files ?? []).map((item) => [item.path, item]));
  const target = new Map((targetPacket.manifest.files ?? []).map((item) => [item.path, item]));
  if ([...parent.keys()].some((path) => !target.has(path))) fail("recovery packet removes a packet file");
  return [...new Set([...parent.keys(), ...target.keys()])].sort().filter((path) => !parent.has(path) || parent.get(path).sha256 !== target.get(path).sha256 || parent.get(path).bytes !== target.get(path).bytes).map((path) => ({
    path,
    before_sha256: parent.get(path)?.sha256 ?? null,
    after_sha256: target.get(path).sha256,
    before_bytes: parent.get(path)?.bytes ?? null,
    after_bytes: target.get(path).bytes,
  }));
}

function controlSemantics(control) {
  const { runtime_files: _runtimeFiles, ...semantic } = control;
  return semantic;
}

function validateRecoveryChangeSet(parentPacket, targetPacket, expected = null) {
  const changed = manifestDiff(parentPacket, targetPacket);
  if (changed.some((item) => !RECOVERY_ALLOWED_FILES.has(item.path))) fail(`recovery packet changes a non-operational file: ${changed.find((item) => !RECOVERY_ALLOWED_FILES.has(item.path)).path}`);
  if (canonicalHash(controlSemantics(parentPacket.control)) !== canonicalHash(controlSemantics(targetPacket.control))) fail("recovery packet changes frozen control semantics");
  if (expected && canonicalHash(changed) !== canonicalHash(expected)) fail("recovery receipt changed-file manifest does not match the packets");
  return changed;
}

export function finalizeRecovery(parentReceiptPath, parentPacketDir, targetPacketDir, output, reason) {
  if (typeof reason !== "string" || !reason.trim()) fail("recovery receipt requires a concrete reason");
  const parentReceipt = resolve(parentReceiptPath), parentPacket = validatePacket(parentPacketDir), targetPacket = validatePacket(targetPacketDir);
  const parentValidated = validateSmokeGateReceipt(parentReceipt, parentPacket.root);
  const { payload: parentPayload, signature: parentSignature } = receiptPayload(parentReceipt);
  const changedFiles = validateRecoveryChangeSet(parentPacket, targetPacket);
  if (changedFiles.length === 0) fail("recovery receipt requires at least one operational runtime change");
  const payload = {
    schema_version: "heardright.l1.smoke_gate_recovery_receipt.v1",
    status: "infrastructure_recovery_pass",
    reason: reason.trim(),
    parent_receipt: { path: parentReceipt, sha256: parentValidated.sha256, payload_sha256: parentSignature.payload_sha256 },
    parent_packet: { path: parentPacket.root, manifest_sha256: parentPacket.manifest_sha256, control_sha256: parentPacket.control_sha256 },
    target_packet: { manifest_sha256: targetPacket.manifest_sha256, control_sha256: targetPacket.control_sha256 },
    source_smoke_packet: parentPayload.source_smoke_packet,
    smoke: parentPayload.smoke,
    lane_order: parentPayload.lane_order,
    changed_files: changedFiles,
  };
  const receipt = { ...payload, signature: { algorithm: "sha256", payload_sha256: canonicalHash(payload) } };
  const target = resolve(output);
  mkdirSync(dirname(target), { recursive: true });
  const temporary = `${target}.tmp-${process.pid}`;
  writeFileSync(temporary, `${JSON.stringify(receipt, null, 2)}\n`);
  renameSync(temporary, target);
  return { path: target, sha256: sha(target), status: receipt.status };
}

function validateRecoveryReceipt(recoveryPath, parentReceiptPath, targetPacket) {
  const path = resolve(recoveryPath);
  if (!existsSync(path)) fail("smoke gate receipt is bound to a different target packet and no recovery receipt exists");
  const { payload } = receiptPayload(path);
  if (payload.schema_version !== "heardright.l1.smoke_gate_recovery_receipt.v1" || payload.status !== "infrastructure_recovery_pass") fail("smoke gate recovery receipt schema or status is invalid");
  if (payload.parent_receipt?.path !== resolve(parentReceiptPath) || payload.parent_receipt?.sha256 !== sha(resolve(parentReceiptPath))) fail("recovery receipt does not bind the original authorized smoke receipt");
  const parentPacket = validatePacket(payload.parent_packet?.path ?? "");
  const parentValidated = validateSmokeGateReceipt(parentReceiptPath, parentPacket.root);
  if (parentValidated.sha256 !== payload.parent_receipt.sha256 || parentPacket.manifest_sha256 !== payload.parent_packet.manifest_sha256 || parentPacket.control_sha256 !== payload.parent_packet.control_sha256) fail("recovery receipt parent packet binding is invalid");
  if (targetPacket.manifest_sha256 !== payload.target_packet?.manifest_sha256 || targetPacket.control_sha256 !== payload.target_packet?.control_sha256) fail("recovery receipt target packet binding is invalid");
  validateRecoveryChangeSet(parentPacket, targetPacket, payload.changed_files);
  return { path, sha256: sha(path), parent: parentValidated, payload };
}

export function validateSmokeGateReceipt(receiptPath, packetDir) {
  const path = resolve(receiptPath);
  if (!existsSync(path)) fail("full authorization smoke gate receipt is missing");
  const { payload } = receiptPayload(path), packet = validatePacket(packetDir);
  const directTarget = payload.target_full_packet?.manifest_sha256 === packet.manifest_sha256 && payload.target_full_packet?.control_sha256 === packet.control_sha256;
  if (payload.schema_version !== "heardright.l1.smoke_gate_receipt.v2" || payload.status !== "infrastructure_pass") fail("smoke gate receipt signature or schema is invalid");
  if (JSON.stringify(payload.lane_order) !== JSON.stringify(DEFAULT_LANES) || payload.lanes?.length !== 4) fail("smoke gate receipt is bound to a different target packet or lane roster");
  let recovery = null;
  if (!directTarget) recovery = validateRecoveryReceipt(resolve(packet.root, "authorization", "recovery-smoke-gate-receipt.json"), path, packet);
  const source = payload.source_smoke_packet;
  const legacySourceMatches = source?.kind === "legacy_v11_archived_smoke" && source.manifest_sha256 === LEGACY_V11.packet_manifest_sha256 && source.control_sha256 === LEGACY_V11.packet_control_sha256 && source.smoke_dataset_sha256 === LEGACY_V11.smoke_dataset_sha256 && source.recipes_sha256 === LEGACY_V11.recipes_sha256 && source.archive_sha256 === LEGACY_V11.archive_sha256 && source.archive_manifest_sha256 === LEGACY_V11.archive_manifest_sha256;
  const currentSourceMatches = source?.kind === "current_packet_smoke" && source.manifest_sha256 === packet.manifest_sha256 && source.control_sha256 === packet.control_sha256 && source.smoke_dataset_sha256 === sha(resolve(packet.root, "data", "smoke-2.jsonl")) && source.recipes_sha256 === sha(resolve(packet.root, "scripts", "local-model-training", "model-recipes.json")) && source.archive_sha256 === null && source.archive_manifest_sha256 === null;
  if (!legacySourceMatches && !currentSourceMatches && !recovery) fail("smoke gate receipt source evidence hashes do not match an accepted source packet");
  return { path, sha256: sha(path), status: payload.status, accepted_run_status: payload.smoke.accepted_run_status, packet_manifest_sha256: packet.manifest_sha256, packet_control_sha256: packet.control_sha256, recovery_receipt_sha256: recovery?.sha256 ?? null };
}

export function finalize(smokeOutput, packetDir, output) {
  const result = evaluateSmokeOutput(smokeOutput, packetDir), payload = signedReceiptPayload(result), receipt = { ...payload, signature: { algorithm: "sha256", payload_sha256: canonicalHash(payload) } };
  const target = output ? resolve(output) : resolve(result.smokeRoot, "smoke-gate-receipt.json");
  mkdirSync(dirname(target), { recursive: true }); const temporary = `${target}.tmp-${process.pid}`;
  writeFileSync(temporary, `${JSON.stringify(receipt, null, 2)}\n`); renameSync(temporary, target);
  return { path: target, sha256: sha(target), status: receipt.status };
}

function main(argv = process.argv.slice(2)) {
  if (argv.includes("--help")) {
    process.stdout.write("Usage: node finalize_smoke_gate.mjs --smoke-output <path> --packet-dir <path> [--output <path>]\n");
    return;
  }
  const smokeOutput = value(argv, "--smoke-output"), packetDir = value(argv, "--packet-dir"), output = value(argv, "--output");
  if (!smokeOutput || !packetDir) fail("--smoke-output and --packet-dir are required");
  process.stdout.write(`${JSON.stringify(finalize(smokeOutput, packetDir, output))}\n`);
}

if (process.argv[1] && basename(process.argv[1]).toLowerCase() === basename(fileURLToPath(import.meta.url)).toLowerCase()) {
  try { main(); } catch (error) { process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`); process.exitCode = 1; }
}
