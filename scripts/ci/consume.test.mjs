import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { consumeNpm } from './consume.mjs';

test('logs retains generic portability failures even when named recipe reports success', () => {
  const scratchRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-consume-test-'));
  const unit = { name: '@rightkit/logs', version: '0.1.6', file: 'logs.tgz', sha256: 'a'.repeat(64), size: 100 };
  let invoked = false;
  try {
    const rows = consumeNpm({ units: [unit], packDir: scratchRoot, scratchRoot, npm: { command: 'npm', prefix: [], shell: false }, spawn: () => ({ status: 1, stderr: 'fixture portability failure' }), npmConsumer: () => { invoked = true; return { name: unit.name, version: unit.version, archiveSha256: unit.sha256, archiveSize: unit.size, qualification: 'R2a', verified: true }; } });
    const row = rows.get(unit.name);
    assert.equal(invoked, true); assert.equal(row.report.verified, false); assert.ok(row.report.portability); assert.ok(row.problems.some(problem => problem.includes('fixture portability failure')));
  } finally { fs.rmSync(scratchRoot, { recursive: true, force: true }); }
});
test('generic npm portability never becomes PK R2a & never invokes named logs runner', () => {
  const scratchRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'rk-consume-test-'));
  try {
    const rows = consumeNpm({ units: [{ name: '@rightkit/other', version: '1.0.0', file: 'other.tgz' }], packDir: scratchRoot, scratchRoot, npm: { command: 'npm', prefix: [], shell: false }, spawn: () => ({ status: 1, stderr: 'fixture' }), npmConsumer: () => { throw new Error('must not invoke'); } });
    assert.equal(rows.get('@rightkit/other').report.qualification, undefined);
  } finally { fs.rmSync(scratchRoot, { recursive: true, force: true }); }
});
