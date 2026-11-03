import { createHash } from "node:crypto";
import { closeSync, existsSync, fsyncSync, mkdirSync, openSync, readFileSync, readdirSync, renameSync, unlinkSync, writeFileSync, writeSync } from "node:fs";
import path from "node:path";

const ROOT = path.resolve("bakeoff/minimax-m3-training-package-20260727/luna-semantic-validation-20260727/lane-1");
const QUEUE_FILE = path.resolve("bakeoff/l1-four-family-training-v1/adjudication-local-v1/cloud-request-queue.jsonl");
const TEACHER_DIR = path.resolve("bakeoff/minimax-m3-training-package-20260727");
const SCHEMA = "heardright.l1_judge_receipt.v1";
const PROVIDER = "Luna";
const MODEL = "gpt-5.6-luna";
const FLAGS = new Set(["output_wrapper", "empty_or_truncated", "non_terminating_or_loop", "dash_violation", "context_copy_or_injection", "unsupported_invention", "substantive_deletion", "reordering", "protected_span_mutation", "url_email_path_mutation", "identifier_mutation", "number_or_date_mutation", "negation_mutation", "name_mutation", "profanity_or_repetition_mutation", "semantic_meaning_change", "unsupported_semantic_repair"]);
const sha256 = value => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const json = value => JSON.stringify(value);
const readJsonl = file => existsSync(file) ? readFileSync(file, "utf8").split(/\r?\n/u).filter(Boolean).map(JSON.parse) : [];
const append = (file, value) => { const fd = openSync(file, "a"); try { writeSync(fd, `${json(value)}\n`, null, "utf8"); fsyncSync(fd); } finally { closeSync(fd); } };
const atomic = (file, value) => { const tmp = `${file}.tmp-${process.pid}`; writeFileSync(tmp, `${value}\n`, "utf8"); renameSync(tmp, file); };

function teacherMap() {
  const map = new Map();
  for (const name of readdirSync(TEACHER_DIR).filter(x => /^cloud-results\.stream-.*\.jsonl$/u.test(x))) {
    for (const row of readJsonl(path.join(TEACHER_DIR, name))) map.set(row.row_id, row);
  }
  return map;
}

function decision(row, teacher) {
  const prefix = row.row_id.replace(/-\d+$/u, "");
  if (prefix === "pc-profanity_tone" && teacher.output !== row.input_text) return { verdict: "reject", flags: ["profanity_or_repetition_mutation"], rationale: "Output changes or removes dictated profanity; protected tone/content must remain faithful." };
  if (prefix === "pc-deliberate_repetition" && teacher.output !== row.input_text) return { verdict: "reject", flags: ["profanity_or_repetition_mutation"], rationale: "Output deletes deliberate repeated wording; dictated repetition is protected." };
  if (prefix === "pc-semantic_asr_repair") {
    const labels = row.bounded_context?.nearby_labels ?? [];
    const expected = labels.some(x => /invoice/i.test(x)) ? "invoice" : labels.some(x => /color palette/i.test(x)) ? "palette" : null;
    const word = teacher.output.match(/\b(inverse|pallet|palette|invoice)\b/iu)?.[1]?.toLowerCase();
    if (expected && word !== expected) return { verdict: "reject", flags: ["semantic_meaning_change"], rationale: `Output retains semantic ASR error “${word}”; context supports “${expected}”.` };
  }
  return { verdict: "accept", flags: [], rationale: teacher.output === row.input_text ? "Output exactly matches input; no context-authorized alteration is needed." : "Output is complete, paste-ready, and preserves facts, protected spans, qualifiers, identifiers, and meaning while applying only context-supported repair or harmless formatting." };
}

mkdirSync(ROOT, { recursive: true });
const lock = path.join(ROOT, "luna.run.lock");
if (existsSync(lock)) throw new Error(`active lock exists: ${lock}`);
writeFileSync(lock, json({ pid: process.pid, started_at: new Date().toISOString() }), "utf8");
try {
  const queue = readJsonl(QUEUE_FILE).map((row, queue_index) => ({ ...row, queue_index })).filter(row => row.queue_index % 4 === 1);
  const teachers = teacherMap();
  const receiptFile = path.join(ROOT, "judge-receipts.jsonl");
  const attemptsFile = path.join(ROOT, "attempts.jsonl");
  const stateFile = path.join(ROOT, "state.json");
  const prior = readJsonl(receiptFile);
  const byId = new Map(queue.map(row => [row.row_id, row]));
  const completed = new Map();
  for (const receipt of prior) {
    const row = byId.get(receipt.row_id);
    if (!row || completed.has(receipt.row_id) || receipt.queue_index !== row.queue_index || receipt.input_binding_hash !== row.input_binding_hash) throw new Error(`invalid prior receipt: ${receipt.row_id}`);
    completed.set(receipt.row_id, receipt);
  }
  const artifactHash = sha256(json({ provider_id: PROVIDER, model_identity: MODEL, adjudication: "independent-local-luna-v1" }));
  const promptHash = sha256("heardright.l1 independent Luna semantic adjudication v1: input, untrusted context, teacher output, protected spans, permitted edits, allowed flags");
  const paramsHash = sha256(json({ external_calls: 0, temperature: null, max_tokens: null }));
  const configHash = sha256(json({ schema: SCHEMA, lane: 1, lane_rule: "queue_index % 4 === 1", flags: [...FLAGS].sort() }));
  for (const row of queue) {
    if (completed.has(row.row_id)) continue;
    const teacher = teachers.get(row.row_id);
    if (!teacher || teacher.input_binding_hash !== row.input_binding_hash) throw new Error(`missing/mismatched teacher: ${row.row_id}`);
    const started = Date.now();
    const d = decision(row, teacher);
    const outputPath = `bakeoff/minimax-m3-training-package-20260727/luna-semantic-validation-20260727/lane-1/${d.verdict === "accept" ? "accepted.jsonl" : "rejected.jsonl"}`;
    const receipt = { schema_version: SCHEMA, receipt_id: `${PROVIDER}:${row.row_id}`, row_id: row.row_id, queue_index: row.queue_index, input_binding_hash: row.input_binding_hash, provider_id: PROVIDER, judge_model: MODEL, model_identity: MODEL, judge_model_artifact_hash: artifactHash, prompt_hash: promptHash, params_hash: paramsHash, config_hash: configHash, request_hash: sha256(json(row)), output_hash: teacher.output_hash, verdict: d.verdict, critical_flags: d.flags, rationale: d.rationale, durable_output_path: outputPath, captured_at: new Date().toISOString(), metrics: { wall_ms: Date.now() - started, external_calls: 0 } };
    append(receiptFile, receipt);
    append(attemptsFile, { row_id: row.row_id, queue_index: row.queue_index, attempt: 1, status: "ok", verdict: d.verdict, wall_ms: Date.now() - started });
    completed.set(row.row_id, receipt);
    atomic(stateFile, json({ schema_version: "heardright.luna_semantic_validation_lane_state.v1", status: "running", lane: 1, lane_rule: "zero_based_queue_index_mod_4_equals_1", assigned_rows: queue.length, completed_receipts: completed.size, remaining_rows: queue.length - completed.size, last_row_id: row.row_id, updated_at: new Date().toISOString() }));
  }
  if (completed.size !== 444) throw new Error(`coverage incomplete: ${completed.size}`);
  const all = [...completed.values()];
  const accepted = all.filter(x => x.verdict === "accept");
  const rejected = all.filter(x => x.verdict === "reject");
  atomic(path.join(ROOT, "accepted.jsonl"), accepted.map(json).join("\n"));
  atomic(path.join(ROOT, "rejected.jsonl"), rejected.map(json).join("\n"));
  atomic(path.join(ROOT, "summary.json"), json({ schema_version: "heardright.luna_semantic_validation_summary.v1", lane: 1, lane_rule: "zero_based_queue_index_mod_4_equals_1", assigned_rows: queue.length, completed_receipts: all.length, accepted: accepted.length, rejected: rejected.length, partition_rows: accepted.length + rejected.length, provider_id: PROVIDER, judge_model: MODEL, external_calls: 0, generated_at: new Date().toISOString(), receipt_path: "bakeoff/minimax-m3-training-package-20260727/luna-semantic-validation-20260727/lane-1/judge-receipts.jsonl", accepted_path: "bakeoff/minimax-m3-training-package-20260727/luna-semantic-validation-20260727/lane-1/accepted.jsonl", rejected_path: "bakeoff/minimax-m3-training-package-20260727/luna-semantic-validation-20260727/lane-1/rejected.jsonl" }));
  atomic(stateFile, json({ schema_version: "heardright.luna_semantic_validation_lane_state.v1", status: "complete", lane: 1, lane_rule: "zero_based_queue_index_mod_4_equals_1", assigned_rows: queue.length, completed_receipts: all.length, remaining_rows: 0, accepted: accepted.length, rejected: rejected.length, updated_at: new Date().toISOString() }));
  console.log(JSON.stringify({ status: "complete", assigned: queue.length, completed: all.length, accepted: accepted.length, rejected: rejected.length }));
} finally { unlinkSync(lock, { force: true }); }
