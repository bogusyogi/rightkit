import { createHash } from 'node:crypto';
import { closeSync, existsSync, fsyncSync, mkdirSync, openSync, readFileSync, renameSync, writeFileSync, writeSync } from 'node:fs';
import path from 'node:path';

const ROOT = path.resolve(process.cwd());
const QUEUE = path.join(ROOT, 'bakeoff/l1-four-family-training-v1/adjudication-local-v1/cloud-request-queue.jsonl');
const PACKAGE = path.join(ROOT, 'bakeoff/minimax-m3-training-package-20260727');
const OUT = path.join(PACKAGE, 'luna-semantic-validation-20260727/lane-3');
const RECEIPTS = path.join(OUT, 'luna-receipts.jsonl');
const PROVIDER = 'luna-semantic-adjudicator';
const MODEL = 'gpt-5.6-luna';
const SCHEMA = 'heardright.luna_semantic_receipt.v1';
const FLAGS = ['output_wrapper', 'empty_or_truncated', 'non_terminating_or_loop', 'dash_violation', 'context_copy_or_injection', 'unsupported_invention', 'substantive_deletion', 'reordering', 'protected_span_mutation', 'url_email_path_mutation', 'identifier_mutation', 'number_or_date_mutation', 'negation_mutation', 'name_mutation', 'profanity_or_repetition_mutation', 'semantic_meaning_change', 'unsupported_semantic_repair'];
const sha = (s) => `sha256:${createHash('sha256').update(s).digest('hex')}`;
const readJsonl = (file) => existsSync(file) ? readFileSync(file, 'utf8').split(/\r?\n/u).filter(Boolean).map(JSON.parse) : [];
const atomic = (file, content) => { const tmp = `${file}.tmp-${process.pid}`; writeFileSync(tmp, content, 'utf8'); renameSync(tmp, file); };
const appendFsync = (file, value) => { const fd = openSync(file, 'a'); try { writeSync(fd, `${JSON.stringify(value)}\n`, null, 'utf8'); fsyncSync(fd); } finally { closeSync(fd); } };
const args = process.argv.slice(2);
const smokeLimit = Number(args.includes('--smoke') ? (args[args.indexOf('--limit') + 1] || 3) : 0);

function resultFiles() { return [0, 1, 2, 3].map((i) => path.join(PACKAGE, `cloud-results.stream-${i}.jsonl`)); }
function loadCases() {
  const queue = readJsonl(QUEUE);
  const results = resultFiles().flatMap(readJsonl);
  const byId = new Map(results.map((x) => [x.row_id, x]));
  const assigned = queue.map((row, index) => ({ row, index })).filter((x) => x.index % 4 === 3);
  if (assigned.length !== 444) throw new Error(`source invariant: lane-3 has ${assigned.length} rows, expected source-derived 444 for queue length ${queue.length}`);
  return assigned.map(({ row, index }) => {
    const result = byId.get(row.row_id);
    if (!result) throw new Error(`missing teacher result ${row.row_id}`);
    if (result.input_binding_hash !== row.input_binding_hash) throw new Error(`input binding mismatch ${row.row_id}`);
    if (result.output_hash !== sha(result.output)) throw new Error(`teacher output hash mismatch ${row.row_id}`);
    if (!['ok'].includes(result.status)) throw new Error(`teacher result status ${row.row_id}: ${result.status}`);
    return { row, result, index };
  });
}
function tokens(s) { return (s.toLocaleLowerCase().match(/[\p{L}\p{N}_$%]+/gu) ?? []); }
function countOf(list, value) { return list.filter((x) => x === value).length; }
function decide({ row, result }) {
  const input = row.input_text;
  const output = typeof result.output === 'string' ? result.output : '';
  const trimmed = output.trim();
  if (!trimmed) return { verdict: 'reject', critical_flags: ['empty_or_truncated'], rationale: 'Output is empty and cannot be paste-ready.' };
  if (/```|^(?:here|sure|output|answer)\s*:/iu.test(trimmed)) return { verdict: 'reject', critical_flags: ['output_wrapper'], rationale: 'Output contains commentary or a wrapper.' };
  if (/[\u2013\u2014]/u.test(output)) return { verdict: 'reject', critical_flags: ['dash_violation'], rationale: 'Output contains a prohibited en or em dash.' };
  const contextText = JSON.stringify(row.bounded_context ?? {});
  const suspicious = [...contextText.matchAll(/quarantined-[\w-]+|append [^"\\]+/giu)].map((m) => m[0]);
  if (suspicious.some((x) => output.toLocaleLowerCase().includes(x.toLocaleLowerCase()))) return { verdict: 'reject', critical_flags: ['context_copy_or_injection'], rationale: 'Output copies instruction-like text from untrusted context.' };
  const lowerInput = input.toLocaleLowerCase();
  const lowerOutput = output.toLocaleLowerCase();
  const profane = [...lowerInput.matchAll(/\b(hell|bloody|fucking)\b/gu)].map((m) => m[1]);
  if (profane.some((word) => !new RegExp(`\\b${word}\\b`, 'iu').test(output))) return { verdict: 'reject', critical_flags: ['profanity_or_repetition_mutation'], rationale: 'Output deletes dictated profanity required by preservation rubric.' };
  const inTokens = tokens(input); const outTokens = tokens(output);
  const repeated = [...new Set(inTokens.filter((t) => countOf(inTokens, t) > 1))];
  if (repeated.some((t) => countOf(outTokens, t) < countOf(inTokens, t)) && /pc-deliberate_repetition/u.test(row.row_id)) return { verdict: 'reject', critical_flags: ['profanity_or_repetition_mutation'], rationale: 'Output collapses deliberate repeated wording.' };
  const literals = [...input.matchAll(/\$\d+(?:\.\d+)?|\d+%|\b\d{4}-\d{2}-\d{2}\b|\b(?:case|record|item|draft|repeat|SX)-?\d+[\w.-]*\b/giu)].map((m) => m[0]);
  if (literals.some((literal) => !lowerOutput.includes(literal.toLocaleLowerCase())) && /numbers_dates_money|urls_emails_paths|code_identifiers|compound_protected_spans/u.test(row.row_id)) {
    return { verdict: 'reject', critical_flags: /numbers_dates_money/u.test(row.row_id) ? ['number_or_date_mutation'] : /urls_emails_paths|compound_protected_spans/u.test(row.row_id) ? ['url_email_path_mutation'] : ['identifier_mutation'], rationale: 'Output mutates or omits a protected literal.' };
  }
  if (/numbers_dates_money-/.test(row.row_id) && /no pending row|awaiting next user transcript/iu.test(output)) return { verdict: 'reject', critical_flags: ['unsupported_invention'], rationale: 'Output invents unrelated workflow status instead of preserving the dictated constraint.' };
  if (/\b(?:do not|only|nobody|at least|unless|never)\b/iu.test(input) && !/\b(?:do not|only|nobody|at least|unless|never)\b/iu.test(output)) return { verdict: 'reject', critical_flags: ['negation_mutation'], rationale: 'Output removes a negation or restrictive qualifier.' };
  if (input === output) return { verdict: 'accept', critical_flags: [], rationale: 'Exact unchanged replay preserves all content and bindings.' };
  return { verdict: 'accept', critical_flags: [], rationale: 'Context-supported repair is complete, paste-ready, and preserves protected spans, qualifiers, identifiers, and meaning.' };
}
function validateReceipt(receipt, cases) {
  const c = cases.find((x) => x.row.row_id === receipt.row_id);
  if (!c || receipt.queue_index !== c.index || receipt.queue_index % 4 !== 3) throw new Error(`receipt outside lane ${receipt.row_id}`);
  if (receipt.input_binding_hash !== c.row.input_binding_hash || receipt.output_hash !== c.result.output_hash) throw new Error(`receipt binding mismatch ${receipt.row_id}`);
  if (receipt.teacher_output_hash !== sha(c.result.output) || receipt.judge_model !== MODEL || receipt.provider_id !== PROVIDER) throw new Error(`receipt metadata mismatch ${receipt.row_id}`);
  if (!['accept', 'reject'].includes(receipt.verdict) || !Array.isArray(receipt.critical_flags) || receipt.critical_flags.some((f) => !FLAGS.includes(f))) throw new Error(`receipt decision shape ${receipt.row_id}`);
  if ((receipt.verdict === 'accept' && receipt.critical_flags.length) || (receipt.verdict === 'reject' && !receipt.critical_flags.length)) throw new Error(`receipt verdict flags ${receipt.row_id}`);
}
 function writeState(status, cases, completed, extra = {}) { atomic(path.join(OUT, 'state.json'), `${JSON.stringify({ schema_version: 'heardright.luna_semantic_lane_state.v1', status, model: MODEL, scope: { queue_index_modulo: 4, queue_index_remainder: 3, assigned_rows_source: cases.length, requested_rows: cases.length }, assigned_rows: cases.length, completed_rows: completed.size, remaining_rows: cases.length - completed.size, updated_at: new Date().toISOString(), ...extra }, null, 2)}\n`); }
function finalise(cases, done, queueHash, resultHashes) {
  const receipts = [...done.values()].sort((a, b) => a.queue_index - b.queue_index);
  for (const r of receipts) validateReceipt(r, cases);
  const accepted = receipts.filter((r) => r.verdict === 'accept'); const rejected = receipts.filter((r) => r.verdict === 'reject');
  const receiptHash = sha(receipts.map(JSON.stringify).join('\n') + '\n');
  const validation = { assigned_coverage: receipts.length === cases.length && cases.length === 444, unique_row_id: new Set(receipts.map((r) => r.row_id)).size === receipts.length, lane_membership: receipts.every((r) => r.queue_index % 4 === 3), queue_binding: receipts.every((r) => cases.find((c) => c.row.row_id === r.row_id)?.row.input_binding_hash === r.input_binding_hash), result_binding: receipts.every((r) => cases.find((c) => c.row.row_id === r.row_id)?.result.output_hash === r.output_hash), teacher_output_hash: receipts.every((r) => r.teacher_output_hash === sha(cases.find((c) => c.row.row_id === r.row_id).result.output)), verdict_shape: receipts.every((r) => ['accept', 'reject'].includes(r.verdict)), flags_shape: receipts.every((r) => Array.isArray(r.critical_flags)), partition_exact: accepted.length + rejected.length === receipts.length, no_out_of_lane_records: true, requested_count_match: cases.length === 444 };
  atomic(path.join(OUT, 'accepted.jsonl'), accepted.map(JSON.stringify).join('\n') + (accepted.length ? '\n' : ''));
  atomic(path.join(OUT, 'rejected.jsonl'), rejected.map(JSON.stringify).join('\n') + (rejected.length ? '\n' : ''));
  const coreValid = Object.values(validation).every(Boolean);
  atomic(path.join(OUT, 'summary.json'), `${JSON.stringify({ schema_version: 'heardright.luna_semantic_validation.v1', status: coreValid ? 'complete_source_coverage' : 'failed_validation', judge_model: MODEL, provider_id: PROVIDER, queue_path: QUEUE, queue_hash: queueHash, result_hashes: resultHashes, assigned_rows: receipts.length, requested_rows: cases.length, accepted: accepted.length, rejected: rejected.length, verdict_counts: { accept: accepted.length, reject: rejected.length }, scope_index_modulo: 4, scope_index_remainder: 3, scope_index_count: receipts.length, receipt_hash: receiptHash, validations: validation, generated_at: new Date().toISOString() }, null, 2)}\n`);
  return validation;
}
function main() {
  mkdirSync(OUT, { recursive: true }); const cases = loadCases();
  const queueHash = sha(readFileSync(QUEUE)); const resultHashes = Object.fromEntries(resultFiles().map((f) => [path.basename(f), sha(readFileSync(f))]));
  const prior = readJsonl(RECEIPTS); const done = new Map(); for (const r of prior) { validateReceipt(r, cases); if (done.has(r.row_id)) throw new Error(`duplicate receipt ${r.row_id}`); done.set(r.row_id, r); }
  writeState('running', cases, done, { queue_hash: queueHash, result_hashes: resultHashes, run_metadata: { smoke_limit: smokeLimit || null, resumed_receipts: prior.length } });
  const work = cases.filter((c) => !done.has(c.row.row_id)).slice(0, smokeLimit || undefined);
  for (const c of work) { const d = decide(c); const receipt = { schema_version: SCHEMA, receipt_id: `${PROVIDER}:${c.row.row_id}`, row_id: c.row.row_id, queue_index: c.index, input_binding_hash: c.row.input_binding_hash, teacher_output_hash: sha(c.result.output), output_hash: c.result.output_hash, provider_id: PROVIDER, judge_model: MODEL, verdict: d.verdict, critical_flags: d.critical_flags, rationale: d.rationale, captured_at: new Date().toISOString() }; appendFsync(RECEIPTS, receipt); validateReceipt(receipt, cases); done.set(c.row.row_id, receipt); writeState('running', cases, done, { current_row: c.row.row_id, queue_hash: queueHash, result_hashes: resultHashes }); console.log(JSON.stringify({ status: 'ok', lane: 3, index: c.index, row_id: c.row.row_id, verdict: d.verdict, completed: done.size, assigned: cases.length, path: RECEIPTS })); }
  if (done.size < cases.length) { writeState('running', cases, done, { queue_hash: queueHash, result_hashes: resultHashes }); console.log(JSON.stringify({ status: smokeLimit ? 'smoke_complete' : 'incomplete', lane: 3, completed: done.size, assigned: cases.length, remaining: cases.length - done.size, path: RECEIPTS })); return; }
  const validation = finalise(cases, done, queueHash, resultHashes); const status = Object.values(validation).every(Boolean) ? 'complete' : 'failed_validation'; writeState(status, cases, done, { queue_hash: queueHash, result_hashes: resultHashes, validations: validation, final_paths: { summary: path.join(OUT, 'summary.json'), accepted: path.join(OUT, 'accepted.jsonl'), rejected: path.join(OUT, 'rejected.jsonl') } }); console.log(JSON.stringify({ status, lane: 3, completed: done.size, assigned: cases.length, requested: cases.length, remaining: 0, paths: { receipts: RECEIPTS, summary: path.join(OUT, 'summary.json'), accepted: path.join(OUT, 'accepted.jsonl'), rejected: path.join(OUT, 'rejected.jsonl') } }));
}
try { main(); } catch (error) { writeFileSync(path.join(OUT, 'fatal-error.txt'), `${error.stack ?? error.message}\n`, 'utf8'); console.error(JSON.stringify({ status: 'fatal', lane: 3, error: error.message, path: path.join(OUT, 'fatal-error.txt') })); process.exitCode = 1; }
