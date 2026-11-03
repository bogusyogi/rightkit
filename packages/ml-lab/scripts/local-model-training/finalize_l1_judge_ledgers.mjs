#!/usr/bin/env node
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, readdirSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";

const sha256 = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const canonical = (value) => JSON.stringify(value);
const readJsonl = (file) => readFileSync(file, "utf8").split(/\r?\n/u).filter(Boolean).map((line, index) => {
  try { return JSON.parse(line); } catch (error) { throw new Error(`${file}:${index + 1}: ${error.message}`); }
});
const writeAtomic = (file, value) => {
  const temporary = `${file}.tmp-${process.pid}`;
  writeFileSync(temporary, value, "utf8");
  renameSync(temporary, file);
};
const writeJsonl = (file, rows) => writeAtomic(file, rows.length ? `${rows.map(canonical).join("\n")}\n` : "");
const arg = (name) => process.argv[process.argv.indexOf(name) + 1];
const requireArg = (name) => arg(name) ?? (() => { throw new Error(`missing ${name}`); })();

const queueFile = requireArg("--queue");
const terraFile = requireArg("--terra");
const minimaxRoot = requireArg("--minimax-root");
const corpusDir = requireArg("--corpus-dir");
const outputDir = requireArg("--out");

const queue = readJsonl(queueFile);
const terra = readJsonl(terraFile);
const minimaxFiles = readdirSync(minimaxRoot, { withFileTypes: true })
  .filter((entry) => entry.isDirectory() && /^shard-\d+$/u.test(entry.name))
  .map((entry) => path.join(minimaxRoot, entry.name, "judge-receipts.jsonl"));
const minimax = minimaxFiles.flatMap(readJsonl);
const corpusFiles = readdirSync(corpusDir).filter((name) => name.endsWith(".jsonl"));
const corpus = corpusFiles.flatMap((name) => readJsonl(path.join(corpusDir, name)));

const byUniqueId = (rows, label) => {
  const map = new Map();
  for (const row of rows) {
    if (!row.row_id || map.has(row.row_id)) throw new Error(`${label}: duplicate or missing row_id ${row.row_id ?? "<missing>"}`);
    map.set(row.row_id, row);
  }
  return map;
};
const queueById = byUniqueId(queue, "queue");
const terraById = byUniqueId(terra, "terra");
const minimaxById = byUniqueId(minimax, "minimax");
const corpusById = byUniqueId(corpus, "corpus");
for (const [label, map] of [["terra", terraById], ["minimax", minimaxById]]) {
  if (map.size !== queue.length) throw new Error(`${label}: expected ${queue.length} rows, found ${map.size}`);
  for (const row of queue) if (!map.has(row.row_id)) throw new Error(`${label}: missing ${row.row_id}`);
}
for (let index = 0; index < queue.length; index += 1) {
  if (terra[index]?.row_id !== queue[index].row_id) throw new Error(`terra: order mismatch at ${index}`);
}

const flagAllowlist = new Set([
  "output_wrapper", "empty_or_truncated", "non_terminating_or_loop", "dash_violation",
  "context_copy_or_injection", "unsupported_invention", "substantive_deletion", "reordering",
  "protected_span_mutation", "url_email_path_mutation", "identifier_mutation", "number_or_date_mutation",
  "negation_mutation", "name_mutation", "profanity_or_repetition_mutation", "semantic_meaning_change",
  "unsupported_semantic_repair",
]);
const validateDecision = (decision, label) => {
  if (!new Set(["accept", "reject"]).has(decision.verdict)) throw new Error(`${label} ${decision.row_id}: invalid verdict`);
  if (!Array.isArray(decision.critical_flags) || decision.critical_flags.some((flag) => !flagAllowlist.has(flag))) throw new Error(`${label} ${decision.row_id}: invalid flags`);
  if ((decision.verdict === "accept") !== (decision.critical_flags.length === 0)) throw new Error(`${label} ${decision.row_id}: verdict/flag mismatch`);
};

const terraPromptContract = {
  contract: "heardright.l1.judge.terra.v1",
  decision_fields: ["row_id", "verdict", "critical_flags", "rationale"],
  rule: "Accept only a complete, conservative, context-supported transcript repair that preserves required spans and performs only permitted edits/deletions; reject with typed critical flags otherwise.",
  flag_allowlist: [...flagAllowlist],
};
const terraArtifactRef = "provider-model-id:gpt-5.6-terra;provider-weight-revision-not-exposed";
const terraMeta = {
  provider_id: "openai-codex",
  judge_model: "gpt-5.6-terra",
  judge_model_artifact_hash: sha256(terraArtifactRef),
  judge_model_artifact_ref: terraArtifactRef,
  prompt_hash: sha256(canonical(terraPromptContract)),
  params_hash: sha256(canonical({ reasoning_effort: "inherited", sampling: "provider-managed" })),
  config_hash: sha256(canonical({ route: "collaboration-subagent", isolation: "blind-to-minimax-and-hidden-gold", batch_size: 100 })),
};

const terraReceipts = queue.map((request) => {
  const decision = terraById.get(request.row_id);
  validateDecision(decision, "terra");
  return {
    schema_version: "heardright.l1_judge_receipt.v1",
    receipt_id: `openai-terra-subagent:${request.row_id}`,
    row_id: request.row_id,
    input_binding_hash: request.input_binding_hash,
    ...terraMeta,
    request_hash: sha256(canonical(request)),
    output_hash: request.output_hash,
    verdict: decision.verdict,
    critical_flags: decision.critical_flags,
    rationale: decision.rationale,
  };
});

for (const receipt of minimax) {
  const request = queueById.get(receipt.row_id);
  validateDecision(receipt, "minimax");
  if (!request || receipt.input_binding_hash !== request.input_binding_hash || receipt.output_hash !== request.output_hash) throw new Error(`minimax ${receipt.row_id}: binding mismatch`);
}

const mandatoryBuckets = new Set(["names", "numbers_dates_money", "code_identifiers", "negation_qualifiers", "urls_emails_paths", "compound_protected_spans", "deliberate_repetition"]);
const riskReasons = (request, source, left, right) => {
  const reasons = [];
  if (left.verdict !== right.verdict) reasons.push("judge_disagreement");
  if (request.disposition?.kind === "cloud_human_audio_semantic" || source?.failure_tags?.includes("contextual_semantic_repair")) reasons.push("semantic_asr_substitution");
  const searchable = canonical({ tags: source?.failure_tags ?? [], context_relevance: source?.context_relevance ?? "", context: source?.bounded_context ?? {} });
  if (/hostile|misleading|adversarial|injection|counterfactual/iu.test(searchable)) reasons.push("hostile_or_misleading_context");
  if (mandatoryBuckets.has(source?.primary_bucket)) reasons.push("precision_critical_bucket");
  if ((source?.failure_tags ?? []).some((tag) => /deletion|truncat|loop|negation|name_precision|numeric_precision|identifier_precision/iu.test(tag))) reasons.push("mandatory_failure_class");
  const flags = [...left.critical_flags, ...right.critical_flags];
  if (flags.some((flag) => /deletion|truncated|loop|negation|name|number|identifier|url|email|path|profanity|repetition/iu.test(flag))) reasons.push("critical_failure_class");
  if ((source?.chunk_count ?? 0) > 1 || (source?.scheduled_background_windows ?? 0) > 0 || (source?.duration_s ?? 0) > 15) reasons.push("long_multichunk_boundary");
  return [...new Set(reasons)];
};
const auditSelected = (rowId) => Number.parseInt(sha256(rowId).slice(-8), 16) % 10 === 0;
const records = queue.map((request) => {
  const source = corpusById.get(request.row_id);
  if (!source) throw new Error(`corpus: missing ${request.row_id}`);
  const mm = minimaxById.get(request.row_id);
  const tt = terraById.get(request.row_id);
  const bothAccept = mm.verdict === "accept" && tt.verdict === "accept";
  const bothReject = mm.verdict === "reject" && tt.verdict === "reject";
  return { request, source, minimax: mm, terra: tt, bothAccept, bothReject, reasons: riskReasons(request, source, mm, tt) };
});
const lowRiskConsensus = records.filter((record) => record.bothAccept && record.reasons.length === 0);
const calibration = new Set(lowRiskConsensus.slice(0, 100).map((record) => record.request.row_id));
const humanQueue = records.filter((record) => {
  if (record.reasons.length) return true;
  if (!record.bothAccept) return false;
  return calibration.has(record.request.row_id) || auditSelected(record.request.row_id);
}).map((record) => ({
  schema_version: "heardright.l1_human_review_request.v1",
  row_id: record.request.row_id,
  input_binding_hash: record.request.input_binding_hash,
  input_text: record.request.input_text,
  bounded_context: record.request.bounded_context,
  proposed_output: record.request.proposed_output,
  protected_spans: record.request.protected_spans,
  permitted_edits: record.request.permitted_edits,
  deletion_permissions: record.request.deletion_permissions,
  minimax_verdict: record.minimax.verdict,
  minimax_flags: record.minimax.critical_flags,
  terra_verdict: record.terra.verdict,
  terra_flags: record.terra.critical_flags,
  review_reasons: [...record.reasons, ...(calibration.has(record.request.row_id) ? ["calibration_first_100"] : []), ...(!record.reasons.length && auditSelected(record.request.row_id) ? ["deterministic_10_percent_audit"] : [])],
  status: "pending_human_review",
}));

const disagreement = records.filter((record) => !record.bothAccept && !record.bothReject);
const doubleReject = records.filter((record) => record.bothReject);
const doubleAccept = records.filter((record) => record.bothAccept);
const missingIntendedEditAuthority = records.filter((record) => record.request.proposed_output !== record.request.input_text
  && record.request.proposed_output === record.source.candidate_output
  && (record.request.permitted_edits ?? []).length === 0
  && (record.request.deletion_permissions ?? []).length === 0);
const intendedCandidateNotAccepted = records.filter((record) => record.request.proposed_output === record.source.candidate_output && !record.bothAccept);
const humanIds = new Set(humanQueue.map((row) => row.row_id));
const autoAcceptPendingCalibration = doubleAccept.filter((record) => !humanIds.has(record.request.row_id));
const summary = {
  schema_version: "heardright.l1_judge_finalization.v1",
  status: "judges_complete_calibration_failed_no_promotion",
  valid_for_promotion: false,
  queue_rows: queue.length,
  minimax: { accepts: minimax.filter((row) => row.verdict === "accept").length, rejects: minimax.filter((row) => row.verdict === "reject").length },
  terra: { accepts: terra.filter((row) => row.verdict === "accept").length, rejects: terra.filter((row) => row.verdict === "reject").length },
  consensus: { accept_accept: doubleAccept.length, reject_reject: doubleReject.length, disagreements: disagreement.length },
  human_review: { total: humanQueue.length, calibration_first_100: humanQueue.filter((row) => row.review_reasons.includes("calibration_first_100")).length, disagreement: humanQueue.filter((row) => row.review_reasons.includes("judge_disagreement")).length, deterministic_10_percent_audit: humanQueue.filter((row) => row.review_reasons.includes("deterministic_10_percent_audit")).length },
  auto_accept_candidates_pending_calibration: autoAcceptPendingCalibration.length,
  calibration_failures: {
    intended_outputs_missing_permitted_edit_authority: missingIntendedEditAuthority.length,
    intended_candidate_outputs_not_accepted_by_both_judges: intendedCandidateNotAccepted.length,
  },
  promotion_performed: false,
  hashes: {
    queue: sha256(readFileSync(queueFile)),
    minimax_receipts: sha256(minimax.map(canonical).join("\n") + "\n"),
    terra_decisions: sha256(readFileSync(terraFile)),
  },
  provenance_note: "Terra artifact hash binds the provider model identifier because the provider does not expose a weight revision.",
};

mkdirSync(outputDir, { recursive: true });
writeJsonl(path.join(outputDir, "minimax-judge-receipts.jsonl"), queue.map((row) => minimaxById.get(row.row_id)));
writeJsonl(path.join(outputDir, "terra-judge-receipts.jsonl"), terraReceipts);
writeJsonl(path.join(outputDir, "judge-receipts.jsonl"), queue.flatMap((row, index) => [minimaxById.get(row.row_id), terraReceipts[index]]));
writeJsonl(path.join(outputDir, "consensus-accept-pending-calibration.jsonl"), doubleAccept.map((record) => record.request));
writeJsonl(path.join(outputDir, "consensus-reject.jsonl"), doubleReject.map((record) => ({ row_id: record.request.row_id, minimax_flags: record.minimax.critical_flags, terra_flags: record.terra.critical_flags })));
writeJsonl(path.join(outputDir, "judge-disagreements.jsonl"), disagreement.map((record) => ({ row_id: record.request.row_id, minimax_verdict: record.minimax.verdict, minimax_flags: record.minimax.critical_flags, terra_verdict: record.terra.verdict, terra_flags: record.terra.critical_flags })));
writeJsonl(path.join(outputDir, "human-review-queue.jsonl"), humanQueue);
writeAtomic(path.join(outputDir, "judge-summary.json"), `${JSON.stringify(summary, null, 2)}\n`);
console.log(JSON.stringify({ output_dir: path.resolve(outputDir), ...summary }, null, 2));
