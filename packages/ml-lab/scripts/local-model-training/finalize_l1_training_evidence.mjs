#!/usr/bin/env node
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";

const hash = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const fileHash = (file) => hash(readFileSync(file));
const HASH = /^sha256:[a-f0-9]{64}$/u;

function option(name, required = true) {
  const index = process.argv.indexOf(name);
  const value = index < 0 ? null : process.argv[index + 1];
  if (required && (!value || value.startsWith("--"))) throw new Error(`REJECTED CLI: ${name} is required`);
  return value;
}
function countOption(name, fallback) {
  const value = option(name, false);
  if (value === null) return fallback;
  if (!/^\d+$/u.test(value) || Number(value) < 1) throw new Error(`REJECTED CLI: ${name} must be a positive integer`);
  return Number(value);
}
function readJsonl(file) {
  if (!existsSync(file)) throw new Error(`REJECTED missing file: ${file}`);
  return readFileSync(file, "utf8").split(/\r?\n/u).filter(Boolean).map((line, lineNumber) => {
    try { return JSON.parse(line); } catch (error) { throw new Error(`REJECTED ${file}:${lineNumber + 1}: invalid JSON (${error.message})`); }
  });
}
function asUniqueMap(entries, label) {
  const result = new Map();
  for (const entry of entries) {
    if (typeof entry.row_id !== "string" || entry.row_id === "") throw new Error(`REJECTED ${label}: missing row_id`);
    if (result.has(entry.row_id)) throw new Error(`REJECTED ${label}: duplicate row_id ${entry.row_id}`);
    result.set(entry.row_id, entry);
  }
  return result;
}
function exactCoverage(expected, actual, label) {
  if (expected.size !== actual.size) throw new Error(`REJECTED ${label}: coverage count ${actual.size} != ${expected.size}`);
  for (const rowId of expected.keys()) if (!actual.has(rowId)) throw new Error(`REJECTED ${label}: missing row_id ${rowId}`);
  for (const rowId of actual.keys()) if (!expected.has(rowId)) throw new Error(`REJECTED ${label}: extra row_id ${rowId}`);
}
function validateQueueEntry(row) {
  if (row.proposed_output_matches_authorized_replay !== true || row.output_hash !== row.authorized_replay_output_hash || !HASH.test(row.output_hash)) throw new Error(`REJECTED queue ${row.row_id}: authorized replay binding`);
  if (!HASH.test(row.input_binding_hash)) throw new Error(`REJECTED queue ${row.row_id}: input binding hash`);
}
function validateMiniMaxBinding(receipt, queueRow) {
  if (receipt.schema_version !== "heardright.l1_judge_receipt.v1") throw new Error(`REJECTED MiniMax ${receipt.row_id}: receipt schema`);
  if (receipt.input_binding_hash !== queueRow.input_binding_hash || receipt.output_hash !== queueRow.output_hash) throw new Error(`REJECTED MiniMax ${receipt.row_id}: payload binding mismatch`);
  if (receipt.request_hash !== hash(JSON.stringify(queueRow))) throw new Error(`REJECTED MiniMax ${receipt.row_id}: request hash mismatch`);
}
function cleanAccept(receipt) { return receipt.verdict === "accept" && Array.isArray(receipt.critical_flags) && receipt.critical_flags.length === 0; }
function validateTerra(decision) {
  if (decision.verdict !== "accept" || !Array.isArray(decision.critical_flags) || decision.critical_flags.length !== 0) throw new Error(`REJECTED Terra ${decision.row_id}: not clean accept`);
}
function atomic(file, value) {
  mkdirSync(path.dirname(file), { recursive: true });
  const temporary = `${file}.tmp-${process.pid}`;
  writeFileSync(temporary, value, "utf8");
  renameSync(temporary, file);
}
function main() {
  const queueFile = option("--queue"); const minimaxRoot = option("--minimax-root"); const terraFile = option("--terra"); const outputDirectory = option("--out-dir");
  const calibrationExpectedFile = option("--calibration-expected"); const minimaxCalibrationFile = option("--minimax-calibration"); const terraCalibrationFile = option("--terra-calibration");
  const expectedRows = countOption("--expected-rows", 1777); const expectedCalibrationRows = countOption("--expected-calibration-rows", 10);
  const queue = asUniqueMap(readJsonl(queueFile), "queue");
  if (queue.size !== expectedRows) throw new Error(`REJECTED queue: expected ${expectedRows} rows, found ${queue.size}`);
  for (const row of queue.values()) validateQueueEntry(row);
  const shardNames = readdirSync(minimaxRoot, { withFileTypes: true }).filter((entry) => entry.isDirectory() && /^shard-[0-4]$/u.test(entry.name)).map((entry) => entry.name).sort();
  if (shardNames.join(",") !== "shard-0,shard-1,shard-2,shard-3,shard-4") throw new Error("REJECTED MiniMax: exactly five shard directories required");
  const shardFiles = shardNames.map((name) => path.join(minimaxRoot, name, "judge-receipts.jsonl"));
  const minimax = asUniqueMap(shardFiles.flatMap(readJsonl), "MiniMax receipts"); const terra = asUniqueMap(readJsonl(terraFile), "Terra decisions");
  exactCoverage(queue, minimax, "MiniMax receipts"); exactCoverage(queue, terra, "Terra decisions");
  const supersedingFile = option("--superseding", false);
  const superseding = supersedingFile === null ? new Map() : asUniqueMap(readJsonl(supersedingFile), "superseding adjudications");
  const dispositions = new Map();
  for (const [rowId, row] of queue) {
    const receipt = minimax.get(rowId), terraDecision = terra.get(rowId); validateMiniMaxBinding(receipt, row); validateTerra(terraDecision);
    if (cleanAccept(receipt)) { if (superseding.has(rowId)) throw new Error(`REJECTED superseding ${rowId}: direct MiniMax accept cannot be superseded`); dispositions.set(rowId, "direct_accept"); continue; }
    const replacement = superseding.get(rowId);
    if (!replacement) throw new Error(`REJECTED MiniMax ${rowId}: not clean accept`);
    if (replacement.schema_version !== "heardright.l1.superseding_adjudication.v1") throw new Error(`REJECTED superseding ${rowId}: schema_version`);
    if (replacement.original_minimax_receipt_hash !== hash(JSON.stringify(receipt))) throw new Error(`REJECTED superseding ${rowId}: original receipt hash`);
    if (replacement.host_validation?.authorized_replay_exact !== true || replacement.host_validation?.output_hash !== row.output_hash) throw new Error(`REJECTED superseding ${rowId}: deterministic host validation`);
    if (replacement.tie_break?.provider_id === receipt.provider_id || replacement.tie_break?.verdict !== "accept" || replacement.tie_break?.decision_hash !== hash(JSON.stringify(terraDecision))) throw new Error(`REJECTED superseding ${rowId}: independent tie-break binding`);
    dispositions.set(rowId, "superseded_by_independent_tiebreak");
  }
  for (const rowId of superseding.keys()) if (!queue.has(rowId)) throw new Error(`REJECTED superseding ${rowId}: extra row_id`);
  const calibration = asUniqueMap(readJsonl(calibrationExpectedFile), "calibration expected"); const miniCalibration = asUniqueMap(readJsonl(minimaxCalibrationFile), "MiniMax calibration"); const terraCalibration = asUniqueMap(readJsonl(terraCalibrationFile), "Terra calibration");
  if (calibration.size !== expectedCalibrationRows) throw new Error(`REJECTED calibration: expected ${expectedCalibrationRows}, found ${calibration.size}`);
  exactCoverage(calibration, miniCalibration, "MiniMax calibration"); exactCoverage(calibration, terraCalibration, "Terra calibration");
  for (const [rowId, row] of calibration) { validateQueueEntry(row); validateMiniMaxBinding(miniCalibration.get(rowId), row); if (!cleanAccept(miniCalibration.get(rowId))) throw new Error(`REJECTED MiniMax calibration ${rowId}: not clean accept`); validateTerra(terraCalibration.get(rowId)); }
  const receipts = [...queue.keys()].sort().map((rowId) => ({ row_id: rowId, input_binding_hash: queue.get(rowId).input_binding_hash, output_hash: queue.get(rowId).output_hash, authorized_replay_output_hash: queue.get(rowId).authorized_replay_output_hash, proposed_output_matches_authorized_replay: true, minimax_receipt_hash: hash(JSON.stringify(minimax.get(rowId))), terra_decision_hash: hash(JSON.stringify(terra.get(rowId))), minimax_disposition: dispositions.get(rowId), superseding_adjudication_hash: superseding.has(rowId) ? hash(JSON.stringify(superseding.get(rowId))) : null, accepted_by_both: true }));
  const summary = { schema_version: "heardright.l1_training_evidence.v1", coverage: { queue_rows: queue.size, minimax_receipts: minimax.size, terra_receipts: terra.size, accepted_by_both: receipts.length }, calibration: { expected_rows: calibration.size, accepted_by_both: calibration.size }, superseding_adjudications: { rows: superseding.size, sha256: supersedingFile === null ? null : fileHash(supersedingFile) }, bindings: { queue_sha256: fileHash(queueFile), minimax_shards: Object.fromEntries(shardFiles.map((file) => [path.basename(path.dirname(file)), fileHash(file)])), terra_sha256: fileHash(terraFile), calibration_expected_sha256: fileHash(calibrationExpectedFile), minimax_calibration_sha256: fileHash(minimaxCalibrationFile), terra_calibration_sha256: fileHash(terraCalibrationFile) } };
  atomic(path.join(outputDirectory, "judge-receipts.jsonl"), `${receipts.map(JSON.stringify).join("\n")}\n`);
  atomic(path.join(outputDirectory, "summary.json"), `${JSON.stringify(summary, null, 2)}\n`);
  console.log(JSON.stringify({ status: "ok", ...summary.coverage, calibration: summary.calibration.accepted_by_both, out_dir: outputDirectory }));
}
try { main(); } catch (error) { console.error(error.message.startsWith("REJECTED") ? error.message : `REJECTED: ${error.message}`); process.exitCode = 1; }
