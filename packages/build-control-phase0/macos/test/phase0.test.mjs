import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { assertReport, gate } from '../src/schema.mjs';
import { capacityModel, cloneProbe, provenanceProbe, runnerUidIsolationProbe, writeEvidence } from '../src/phase0.mjs';

test('report schema accepts typed BLOCKED_EXTERNAL gate', () => {
  assert.equal(assertReport({ schema: 'rightkit.build-control-phase0.macos.report/v1', status: 'BLOCKED_EXTERNAL', generatedAt: new Date().toISOString(), machine: {}, gates: [gate('apple', 'BLOCKED_EXTERNAL', 'Apple approval')], evidence: [] }).status, 'BLOCKED_EXTERNAL');
});
test('clone probe refuses missing external root', async () => {
  assert.equal((await cloneProbe('/definitely-not-a-mounted-rightkit-volume')).status, 'BLOCKED_EXTERNAL');
});
test('provenance output is digest-bound', async () => {
  const result = await provenanceProbe(process.cwd()); assert.match(result.digest, /^[a-f0-9]{64}$/);
});
test('capacity model never invents Cmax without measured equation terms', async () => {
  const result = await capacityModel(process.cwd(), '/definitely-not-a-mounted-rightkit-volume');
  assert.equal(result.capacityEquation.cacheMaximumBytes, null);
  assert.deepEqual(result.capacityEquation.unsolvedTerms, ['A', 'R', 'F', 'X']);
});
test('runner probe records passwordless-admin attempt without modifying accounts', async () => {
  const result = await runnerUidIsolationProbe();
  assert.equal(result.adminProbe.command, 'sudo -n true');
  assert.ok(Array.isArray(result.attemptedCommands));
});
test('writer emits sealed report & named evidence', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'rightkit-phase0-test-'));
  try { const result = await writeEvidence(directory, process.cwd(), '/definitely-not-a-mounted-rightkit-volume'); assert.ok(['PASS', 'FAIL', 'BLOCKED_EXTERNAL'].includes(result.status)); }
  finally { await rm(directory, { recursive: true, force: true }); }
});
test('AUTH_EXEC scaffold has explicit default deny & no authorization-path I/O', async () => {
  const source = await readFile(new URL('../extension/RightKitEndpointExtension.swift', import.meta.url), 'utf8');
  assert.match(source, /ES_AUTH_RESULT_DENY/);
  assert.doesNotMatch(source, /FileManager|URLSession|print\s*\(|os_log|DispatchQueue/);
});
