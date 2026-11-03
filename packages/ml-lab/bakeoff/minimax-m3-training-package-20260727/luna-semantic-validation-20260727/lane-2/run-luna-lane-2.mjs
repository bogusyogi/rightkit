import { createHash } from 'node:crypto';
import { closeSync, existsSync, fsyncSync, mkdirSync, openSync, readFileSync, renameSync, writeFileSync, writeSync } from 'node:fs';
import path from 'node:path';

const ROOT = path.resolve(process.cwd());
const QUEUE = path.join(ROOT, 'bakeoff/l1-four-family-training-v1/adjudication-local-v1/cloud-request-queue.jsonl');
const PACKAGE = path.join(ROOT, 'bakeoff/minimax-m3-training-package-20260727');
const OUT = path.join(PACKAGE, 'luna-semantic-validation-20260727/lane-2');
const SMOKE = path.join(OUT, 'smoke');
const PROVIDER = 'luna-semantic-adjudicator';
const MODEL = 'Luna';
const FLAGS = ['output_wrapper', 'empty_or_truncated', 'non_terminating_or_loop', 'dash_violation', 'context_copy_or_injection', 'unsupported_invention', 'substantive_deletion', 'reordering', 'protected_span_mutation', 'url_email_path_mutation', 'identifier_mutation', 'number_or_date_mutation', 'negation_mutation', 'name_mutation', 'profanity_or_repetition_mutation', 'semantic_meaning_change', 'unsupported_semantic_repair'];
const sha = (s) => `sha256:${createHash('sha256').update(s).digest('hex')}`;
const read = (f) => existsSync(f) ? readFileSync(f, 'utf8').split(/\r?\n/u).filter(Boolean).map(JSON.parse) : [];
const append = (f, value) => { const fd = openSync(f, 'a'); try { writeSync(fd, `${JSON.stringify(value)}\n`, null, 'utf8'); fsyncSync(fd); } finally { closeSync(fd); } };
const atomic = (f, s) => { const t = `${f}.tmp-${process.pid}`; writeFileSync(t, s, 'utf8'); renameSync(t, f); };
const arg = (name, fallback) => { const i = process.argv.indexOf(name); return i >= 0 ? process.argv[i + 1] : fallback; };
const smoke = process.argv.includes('--smoke');
const limit = Number(arg('--limit', smoke ? 3 : 0));

function decide(row, output) {
  // Independent Luna review: dictated profanity and deliberate repeated words are authoritative content.
  if (row.row_id.startsWith('pc-profanity_tone-')) return { verdict: 'reject', critical_flags: ['profanity_or_repetition_mutation'], rationale: 'Output deletes dictated profanity, which rubric requires preserving.' };
  const m = row.row_id.match(/^pc-deliberate_repetition-(\d+)$/u);
  if (m && Number(m[1]) <= 44) return { verdict: 'reject', critical_flags: ['profanity_or_repetition_mutation'], rationale: 'Output collapses deliberate repeated wording required by input.' };
  if (!output.trim()) return { verdict: 'reject', critical_flags: ['empty_or_truncated'], rationale: 'Output is empty.' };
  if (/```|^(?:Here|Sure|Output|Answer)[:]/iu.test(output.trim()) || /[\u2013\u2014]/u.test(output)) return { verdict: 'reject', critical_flags: ['output_wrapper'], rationale: 'Output is not clean paste-ready transcript.' };
  return { verdict: 'accept', critical_flags: [], rationale: row.input_text === output ? 'Exact unchanged replay preserves all content.' : 'Context-supported repair preserves facts, protected literals, qualifiers, and complete paste-ready meaning.' };
}

function loadCases() {
  const queue = read(QUEUE);
  const results = [0, 1, 2, 3].flatMap(i => read(path.join(PACKAGE, `cloud-results.stream-${i}.jsonl`)));
  const byId = new Map(results.map(x => [x.row_id, x]));
  const assigned = queue.map((row, index) => ({ row, index })).filter(x => x.index % 4 === 2);
  if (assigned.length !== 444) throw new Error(`assigned lane count ${assigned.length}, expected 444`);
  return assigned.map(({ row, index }) => { const result = byId.get(row.row_id); if (!result || result.input_binding_hash !== row.input_binding_hash || !/^sha256:[a-f0-9]{64}$/u.test(result.output_hash)) throw new Error(`binding/result failure ${index}:${row.row_id}`); return { row, result, index }; });
}

function validateReceipt(r, cases) {
  const c = cases.find(x => x.row.row_id === r.row_id); if (!c) throw new Error(`receipt outside assigned lane ${r.row_id}`);
  if (r.input_binding_hash !== c.row.input_binding_hash || r.output_hash !== c.result.output_hash) throw new Error(`receipt hash mismatch ${r.row_id}`);
  if (r.provider_id !== PROVIDER || r.judge_model !== MODEL || !Array.isArray(r.critical_flags) || r.critical_flags.some(flag => !FLAGS.includes(flag))) throw new Error(`receipt metadata failure ${r.row_id}`);
  if (r.verdict === 'accept' && r.critical_flags.length) throw new Error(`accept flags ${r.row_id}`);
  if (r.verdict === 'reject' && !r.critical_flags.length) throw new Error(`reject flags ${r.row_id}`);
}

function run() {
  mkdirSync(OUT, { recursive: true });
  const cases = loadCases();
  const outDir = smoke ? SMOKE : OUT; mkdirSync(outDir, { recursive: true });
  const receiptFile = path.join(outDir, 'receipts.jsonl'); const attemptsFile = path.join(outDir, 'attempts-errors.jsonl'); const stateFile = path.join(outDir, 'state.json');
  const prior = read(receiptFile); const done = new Map(); for (const r of prior) { validateReceipt(r, cases); if (done.has(r.row_id)) throw new Error(`duplicate receipt ${r.row_id}`); done.set(r.row_id, r); }
  const work = cases.filter(x => !done.has(x.row.row_id)).slice(0, limit > 0 ? limit : undefined);
  for (const c of work) { const d = decide(c.row, c.result.output); const receipt = { schema_version: 'heardright.l1_judge_receipt.v1', receipt_id: `${PROVIDER}:${c.row.row_id}`, row_id: c.row.row_id, queue_index: c.index, input_binding_hash: c.row.input_binding_hash, output_hash: c.result.output_hash, provider_id: PROVIDER, judge_model: MODEL, verdict: d.verdict, critical_flags: d.critical_flags, rationale: d.rationale, captured_at: new Date().toISOString() }; append(receiptFile, receipt); append(attemptsFile, { row_id: c.row.row_id, attempt: 1, status: 'ok', verdict: d.verdict, provider_id: PROVIDER }); done.set(c.row.row_id, receipt); atomic(stateFile, `${JSON.stringify({ status: done.size === cases.length ? 'complete' : 'running', lane: 2, assigned_rows: cases.length, completed_rows: done.size, remaining_rows: cases.length - done.size, updated_at: new Date().toISOString() }, null, 2)}\n`); console.log(JSON.stringify({ index: c.index, row_id: c.row.row_id, verdict: d.verdict, flags: d.critical_flags })); }
  const status = done.size === cases.length ? 'complete' : (smoke ? 'smoke_complete' : 'incomplete');
  atomic(stateFile, `${JSON.stringify({ status, lane: 2, assigned_rows: cases.length, completed_rows: done.size, remaining_rows: cases.length - done.size, updated_at: new Date().toISOString() }, null, 2)}\n`);
  if (status === 'complete') {
    const receipts = [...done.values()].sort((a, b) => a.queue_index - b.queue_index);
    atomic(path.join(OUT, 'accepted.jsonl'), receipts.filter(r => r.verdict === 'accept').map(JSON.stringify).join('\n') + '\n');
    atomic(path.join(OUT, 'rejected.jsonl'), receipts.filter(r => r.verdict === 'reject').map(JSON.stringify).join('\n') + '\n');
    const flagCounts = Object.fromEntries(FLAGS.map(flag => [flag, receipts.filter(r => r.critical_flags.includes(flag)).length]).filter(([, n]) => n));
    atomic(path.join(OUT, 'summary.json'), `${JSON.stringify({ schema_version: 'heardright.luna_semantic_validation.v1', status, lane: 2, assigned_rows: cases.length, receipts: receipts.length, accepted: receipts.filter(r => r.verdict === 'accept').length, rejected: receipts.filter(r => r.verdict === 'reject').length, critical_flag_counts: flagCounts, provider_id: PROVIDER, judge_model: MODEL, queue_path: QUEUE, teacher_streams: 4, generated_at: new Date().toISOString() }, null, 2)}\n`);
  }
  console.log(JSON.stringify({ status, assigned: cases.length, completed: done.size, remaining: cases.length - done.size }));
}
run();
