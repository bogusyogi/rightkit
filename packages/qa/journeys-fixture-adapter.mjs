#!/usr/bin/env node
// RightKit-owned FIXTURE adapter for the journeys protocol v1 (docs/E2E-TESTING-STANDARD.md, D4).
// It runs no app: it answers the selection so the runner, the validator and their refusals can be tested.
// Usage: node journeys-fixture-adapter.mjs <app-binary-file>
// Behaviour is chosen by RK_FIXTURE_MODE; see MODES. It is self-contained so it needs no build.
import { createHash } from 'node:crypto';
import fs from 'node:fs';

const MODES = [
  'pass', 'skip', 'fail', 'error', 'dup', 'missing', 'unselected', 'stale-nonce', 'stale-run', 'wrong-digest',
  'exit1', 'zero-units', 'noop', 'no-end', 'no-start', 'bad-isolation', 'bad-end-count',
];
const mode = process.env.RK_FIXTURE_MODE ?? 'pass';
if (!MODES.includes(mode)) {
  console.error(`unknown RK_FIXTURE_MODE ${mode}`);
  process.exit(64);
}
if (process.env.RIGHTKIT_JOURNEYS_PROTOCOL !== '1') {
  console.error('RIGHTKIT_JOURNEYS_PROTOCOL must be 1');
  process.exit(64);
}
const selection = JSON.parse(fs.readFileSync(process.env.RIGHTKIT_JOURNEYS_SELECTION, 'utf8'));
const resultsPath = process.env.RIGHTKIT_JOURNEYS_RESULTS;
const sha = (file) => createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const now = () => new Date().toISOString();

let runId = selection.run_id;
let nonce = selection.nonce;
const emit = (record) => fs.appendFileSync(resultsPath, `${JSON.stringify({ ...record, run_id: runId, nonce })}\n`);

const appFile = process.argv[2];
const artifacts = [{ role: 'adapter', path: process.argv[1], sha256: sha(process.argv[1]) }];
if (appFile) artifacts.push({ role: 'app', path: appFile, sha256: mode === 'wrong-digest' ? '0'.repeat(64) : sha(appFile) });

const ok = { total: 1, passed: 1, failed: 0, skipped: 0 };
const ids = selection.journeys.map((j) => j.id);
const record = (id, status, units, reason = null) => ({
  type: 'journey', id, status, started_at: now(), duration_ms: 1, units, evidence: [], reason,
});

if (mode !== 'no-start') {
  emit({
    type: 'start', protocol: 1, adapter: 'rightkit-fixture', adapter_version: '1', started_at: now(), artifacts,
    isolation: {
      data_dir: process.env.RIGHTKIT_QA_DATA_DIR ?? '',
      guard: mode === 'bad-isolation' ? 'failed' : 'passed',
      qa_mode: 'runtime',
    },
  });
}
let reported = 0;
if (mode !== 'noop') {
  ids.forEach((id, i) => {
    if (mode === 'missing' && i === ids.length - 1) return;
    let rec = record(id, 'passed', ok);
    if (i === 0) {
      if (mode === 'skip') rec = record(id, 'skipped', { total: 1, passed: 0, failed: 0, skipped: 1 }, 'skipped by fixture');
      if (mode === 'fail') rec = record(id, 'failed', { total: 1, passed: 0, failed: 1, skipped: 0 }, 'failed by fixture');
      if (mode === 'error') rec = record(id, 'errored', { total: 0, passed: 0, failed: 0, skipped: 0 }, 'errored by fixture');
      if (mode === 'zero-units') rec = record(id, 'passed', { total: 0, passed: 0, failed: 0, skipped: 0 });
      if (mode === 'stale-nonce') nonce = 'f'.repeat(32);
      if (mode === 'stale-run') runId = '00000000-0000-4000-8000-000000000000';
    }
    emit(rec);
    reported++;
    if (i === 0 && mode === 'dup') {
      emit(rec);
      reported++;
    }
    if (i === 0) {
      nonce = selection.nonce;
      runId = selection.run_id;
    }
  });
  if (mode === 'unselected') {
    emit(record('J-999', 'passed', ok));
    reported++;
  }
}
if (mode !== 'no-end') {
  emit({
    type: 'end',
    selected: ids.length,
    reported: mode === 'bad-end-count' ? reported + 1 : reported,
    ended_at: now(),
  });
}
process.exit(mode === 'exit1' ? 1 : 0);
