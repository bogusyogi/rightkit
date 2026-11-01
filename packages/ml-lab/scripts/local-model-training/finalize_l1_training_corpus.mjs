#!/usr/bin/env node
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { basename, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { normalizeCandidate, toTrainingRow } from "./build_dataset.mjs";

const EXPECTED_ROWS = 1777;
const SHORT_QUOTAS = {
  short_noop: 135,
  short_single_repair: 113,
  short_punctuation: 90,
  short_filler_or_restart: 67,
  short_dictated_format: 45,
};
const PRECISION_QUOTAS = {
  names: 87,
  numbers_dates_money: 35,
  urls_emails_paths: 82,
  code_identifiers: 82,
  negation_qualifiers: 87,
  profanity_tone: 77,
  deliberate_repetition: 77,
  semantic_asr_repair: 97,
  compound_protected_spans: 84,
};
const LONG_QUOTAS = {
  contextual_semantic_repair: 59,
  false_start_or_self_correction: 52,
  no_op_or_intentional_repetition: 31,
  misleading_context: 48,
  multi_error_repair: 68,
  protected_span: 73,
  truncation_or_deletion_adversarial: 11,
};

const fail = (message) => { throw new Error(message); };
const shaText = (text) => `sha256:${createHash("sha256").update(text).digest("hex")}`;
const shaFile = (path) => shaText(readFileSync(path));
const value = (argv, name) => {
  const index = argv.indexOf(name);
  return index < 0 ? undefined : argv[index + 1];
};
const values = (argv, name) => argv.flatMap((item, index) => item === name ? [argv[index + 1]] : []).filter(Boolean);

function readJsonl(path) {
  const text = readFileSync(path, "utf8");
  const rows = text.split(/\r?\n/).filter(Boolean).map((line, index) => {
    try { return { row: JSON.parse(line), line }; }
    catch (error) { fail(`${path}:${index + 1}: ${error.message}`); }
  });
  return rows;
}

function uniqueById(records, label) {
  const result = new Map();
  for (const record of records) {
    const id = record.row.row_id;
    if (typeof id !== "string" || !id) fail(`${label}: missing row_id`);
    if (result.has(id)) fail(`${label}: duplicate ${id}`);
    result.set(id, record);
  }
  return result;
}

function atomicJsonl(path, rows) {
  const content = `${rows.map((row) => JSON.stringify(row)).join("\n")}\n`;
  const temporary = `${path}.tmp-${process.pid}`;
  writeFileSync(temporary, content);
  renameSync(temporary, path);
  return { path, rows: rows.length, sha256: shaText(content) };
}

function atomicJson(path, value) {
  const content = `${JSON.stringify(value, null, 2)}\n`;
  const temporary = `${path}.tmp-${process.pid}`;
  writeFileSync(temporary, content);
  renameSync(temporary, path);
  return shaText(content);
}

function validateCalibration(expectedPath, minimaxPath, terraPath) {
  if (!expectedPath || !minimaxPath || !terraPath) fail("all three calibration paths are required");
  const expected = uniqueById(readJsonl(expectedPath), "calibration expected");
  const minimax = uniqueById(readJsonl(minimaxPath), "calibration MiniMax");
  const terra = uniqueById(readJsonl(terraPath), "calibration Terra");
  if (expected.size !== 10 || minimax.size !== 10 || terra.size !== 10) fail("calibration must contain exactly 10 rows per lane");
  for (const [id, expectation] of expected) {
    const wanted = expectation.row.expected_verdict ?? expectation.row.verdict;
    const mm = minimax.get(id)?.row;
    const tt = terra.get(id)?.row;
    if (!mm || !tt) fail(`calibration coverage missing ${id}`);
    if ((mm.verdict ?? mm.decision) !== wanted || (tt.verdict ?? tt.decision) !== wanted) fail(`calibration mismatch ${id}`);
  }
  return { rows: 10, expected_sha256: shaFile(expectedPath), minimax_sha256: shaFile(minimaxPath), terra_sha256: shaFile(terraPath) };
}

function validateJudges({ queue, minimaxRoot, terraPath, supersessionPath }) {
  const queueRecords = readJsonl(queue);
  const queueById = uniqueById(queueRecords, "queue");
  if (queueById.size !== EXPECTED_ROWS) fail(`queue has ${queueById.size}, expected ${EXPECTED_ROWS}`);
  for (const [id, { row }] of queueById) {
    if (row.proposed_output_matches_authorized_replay !== true) fail(`${id}: replay flag is not true`);
    const outputHash = shaText(row.proposed_output);
    if (row.output_hash !== outputHash || row.authorized_replay_output_hash !== outputHash) fail(`${id}: output/replay hash mismatch`);
  }

  const minimaxRecords = [];
  for (let shard = 0; shard < 5; shard += 1) {
    const path = join(minimaxRoot, `shard-${shard}`, "judge-receipts.jsonl");
    minimaxRecords.push(...readJsonl(path));
  }
  const minimaxById = uniqueById(minimaxRecords, "MiniMax receipts");
  if (minimaxById.size !== EXPECTED_ROWS) fail(`MiniMax coverage ${minimaxById.size}`);
  const terraById = uniqueById(readJsonl(terraPath), "Terra decisions");
  if (terraById.size !== EXPECTED_ROWS) fail(`Terra coverage ${terraById.size}`);
  const supersession = JSON.parse(readFileSync(supersessionPath, "utf8"));
  let superseded = 0;
  for (const [id, queueRecord] of queueById) {
    const mmRecord = minimaxById.get(id);
    const mm = mmRecord?.row;
    const terraRecord = terraById.get(id);
    const terra = terraRecord?.row;
    if (!mm || !terra) fail(`${id}: missing judge coverage`);
    if (mm.input_binding_hash !== queueRecord.row.input_binding_hash || mm.request_hash !== shaText(JSON.stringify(queueRecord.row))) fail(`${id}: MiniMax binding/request mismatch`);
    if (terra.verdict !== "accept") fail(`${id}: Terra rejected`);
    if (mm.verdict !== "accept") {
      if (supersession.row_id !== id || superseded !== 0) fail(`${id}: unsuperseded MiniMax rejection`);
      if (supersession.original_minimax_receipt_hash !== shaText(mmRecord.line)) fail(`${id}: supersession MiniMax hash mismatch`);
      if (supersession.host_validation?.authorized_replay_exact !== true || supersession.host_validation?.output_hash !== queueRecord.row.output_hash) fail(`${id}: supersession host replay mismatch`);
      if (supersession.original_terra_decision?.provider_id === mm.provider_id || supersession.original_terra_decision?.verdict !== "accept" || supersession.original_terra_decision?.decision_hash !== shaText(terraRecord.line)) fail(`${id}: supersession independent judge mismatch`);
      if (supersession.independent_retry?.provider_id !== mm.provider_id || supersession.independent_retry?.verdict !== "accept") fail(`${id}: MiniMax retry did not accept`);
      superseded += 1;
    }
  }
  if (superseded !== 1) fail(`expected exactly one recorded false-reject supersession, found ${superseded}`);
  return { queueById, minimaxRecords, terraRecords: [...terraById.values()], superseded };
}

function acceptedCandidate(source, queueRow, ledgerRef) {
  const row = structuredClone(source);
  row.lifecycle = "accepted";
  row.split = "train";
  row.training_eligible = true;
  row.input_binding_hash = queueRow.input_binding_hash;
  row.gold_output = queueRow.proposed_output;
  row.target_basis = "generator_authorized_replay_dual_judged";
  row.adjudication_status = "accepted";
  const reason = "Exact deterministic authorized replay; Terra accepted; MiniMax accepted or immutable false-reject supersession accepted.";
  if (row.schema_version === "heardright.l1_precision_candidate.v2") {
    row.adjudication = { status: "accepted", adjudicator_ids: ["minimax", "terra", "host-replay"], adjudicated_at: new Date(0).toISOString(), reason, ledger_ref: ledgerRef };
  } else if (row.schema_version === "heardright.l1.scheduled-static15.candidate.v2") {
    row.adjudication = { status: "accepted", primary_reviewer: "minimax", secondary_reviewer: "terra", decision: "accept", rationale: reason, reviewed_at: new Date(0).toISOString(), cloud_l1_comparison: "not_applicable_synthetic_authority", ledger_ref: ledgerRef };
  } else {
    row.adjudicator = "minimax+terra+host-replay";
    row.adjudication_reason = reason;
    row.adjudication_ledger_ref = ledgerRef;
  }
  return row;
}

function rank(rows) {
  return [...rows].sort((a, b) => shaText(a.row_id).localeCompare(shaText(b.row_id)) || a.row_id.localeCompare(b.row_id));
}

function takeExact(rows, field, quotas, label) {
  const chosen = [];
  for (const [bucket, count] of Object.entries(quotas)) {
    const candidates = rank(rows.filter((row) => row[field] === bucket));
    if (candidates.length < count) fail(`${label}/${bucket}: ${candidates.length} available, ${count} required`);
    chosen.push(...candidates.slice(0, count));
  }
  return chosen;
}

function longBucket(row) {
  const tags = new Set(row.failure_tags ?? []);
  for (const key of Object.keys(LONG_QUOTAS)) if (tags.has(key)) return key;
  fail(`${row.row_id}: no recognized long-context bucket`);
}

export function select1500(accepted) {
  const short = accepted.filter((row) => row.row_id.startsWith("short-v1-"));
  const precision = accepted.filter((row) => row.row_id.startsWith("pc-"));
  const long = accepted.filter((row) => row.row_id.startsWith("ss15-"));
  const selectedShort = takeExact(short, "sub_bucket", SHORT_QUOTAS, "short");
  const selectedPrecision = takeExact(precision, "primary_bucket", PRECISION_QUOTAS, "precision");
  for (const row of long) row.__selection_bucket = longBucket(row);
  const longCounts = Object.fromEntries(Object.keys(LONG_QUOTAS).map((key) => [key, long.filter((row) => row.__selection_bucket === key).length]));
  for (const [key, wanted] of Object.entries(LONG_QUOTAS)) if (longCounts[key] !== wanted) fail(`long/${key}: ${longCounts[key]} available, exactly ${wanted} required`);
  const selectedLong = rank(long);
  for (const row of long) delete row.__selection_bucket;
  const selected = [...selectedShort, ...selectedPrecision, ...selectedLong];
  if (selected.length !== 1500 || new Set(selected.map((row) => row.row_id)).size !== 1500) fail(`selection cardinality ${selected.length}`);
  return { selected, counts: { short: selectedShort.length, precision: selectedPrecision.length, long: selectedLong.length, short_quotas: SHORT_QUOTAS, precision_quotas: PRECISION_QUOTAS, long_quotas: LONG_QUOTAS } };
}

export function canonicalTrainingRows(rows) {
  return rows.map((row) => {
    const exportRow = { ...row, source_record_hash: row.source_hash, source_hash: shaText(row.input_text) };
    return toTrainingRow(normalizeCandidate(exportRow));
  });
}

function selectDevelopmentRows(accepted, selected) {
  const selectedIds = new Set(selected.map((row) => row.row_id));
  const unused = accepted.filter((row) => !selectedIds.has(row.row_id));
  const short = rank(unused.filter((row) => row.row_id.startsWith("short-v1-"))).slice(0, 50);
  const precision = rank(unused.filter((row) => row.row_id.startsWith("pc-"))).slice(0, 50);
  if (short.length !== 50 || precision.length !== 50) fail("source-disjoint development pool cannot supply 50 short and 50 precision rows");
  return { development: [...short, ...precision], preflight: [...short.slice(0, 15), ...precision.slice(0, 15)] };
}

function main(argv = process.argv.slice(2)) {
  const queue = resolve(value(argv, "--queue") ?? fail("--queue is required"));
  const minimaxRoot = resolve(value(argv, "--minimax-root") ?? fail("--minimax-root is required"));
  const terra = resolve(value(argv, "--terra") ?? fail("--terra is required"));
  const supersession = resolve(value(argv, "--supersession") ?? fail("--supersession is required"));
  const corpusPaths = values(argv, "--corpus").map((path) => resolve(path));
  if (corpusPaths.length !== 3) fail("exactly three --corpus files are required");
  const outDir = resolve(value(argv, "--out-dir") ?? fail("--out-dir is required"));
  mkdirSync(outDir, { recursive: true });
  const calibration = validateCalibration(resolve(value(argv, "--calibration-expected")), resolve(value(argv, "--minimax-calibration")), resolve(value(argv, "--terra-calibration")));
  const judges = validateJudges({ queue, minimaxRoot, terraPath: terra, supersessionPath: supersession });
  const sourceRecords = corpusPaths.flatMap(readJsonl);
  const sources = uniqueById(sourceRecords, "source corpus");
  const accepted = [...judges.queueById].map(([id, queueRecord]) => {
    const source = sources.get(id)?.row;
    if (!source) fail(`${id}: missing source row`);
    return acceptedCandidate(source, queueRecord.row, `heardright-l1-final-evidence:${id}`);
  });
  const acceptedReceipt = atomicJsonl(join(outDir, "accepted-corpus.final.jsonl"), accepted);
  const selection = select1500(accepted);
  const development = selectDevelopmentRows(accepted, selection.selected);
  const selectedReceipt = atomicJsonl(join(outDir, "selected-1500.accepted.jsonl"), selection.selected);
  const trainingRows = canonicalTrainingRows(selection.selected);
  const developmentRows = canonicalTrainingRows(development.development);
  const preflightRows = canonicalTrainingRows(development.preflight);
  const trainingReceipt = atomicJsonl(join(outDir, "training-1500.jsonl"), trainingRows);
  const smokeReceipt = atomicJsonl(join(outDir, "smoke-2.jsonl"), trainingRows.slice(0, 2));
  const developmentReceipt = atomicJsonl(join(outDir, "development-100.jsonl"), developmentRows);
  const preflightReceipt = atomicJsonl(join(outDir, "gemma-preflight-30.jsonl"), preflightRows);
  const judgeReceipt = atomicJsonl(join(outDir, "judge-receipts.combined.jsonl"), [...judges.minimaxRecords.map(({ row }) => row), ...judges.terraRecords.map(({ row }) => row)]);
  const summary = {
    schema_version: "heardright.l1.training_evidence_summary.v1",
    status: "ready_for_dataset_export",
    queue: { path: queue, rows: EXPECTED_ROWS, sha256: shaFile(queue) },
    judges: { minimax_accept: 1776, minimax_false_reject_superseded: judges.superseded, terra_accept: 1777, combined: judgeReceipt },
    calibration,
    corpus_sources: corpusPaths.map((path) => ({ path, sha256: shaFile(path) })),
    accepted: acceptedReceipt,
    selected: selectedReceipt,
    training: trainingReceipt,
    smoke: smokeReceipt,
    development: developmentReceipt,
    gemma_preflight: preflightReceipt,
    selection_counts: selection.counts,
    supersession: { path: supersession, sha256: shaFile(supersession) },
  };
  summary.summary_sha256 = atomicJson(join(outDir, "evidence-summary.json"), summary);
  process.stdout.write(`${JSON.stringify(summary, null, 2)}\n`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
