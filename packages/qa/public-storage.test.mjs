import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { createManagedRun, finishManagedRun, pruneManagedRuns, RUN_MARKER } from './dist/storage.js';

test('public storage contract writes shared marker & reports retention accounting', () => {
  const root = fs.mkdtempSync(path.join(fs.realpathSync.native(os.tmpdir()), 'rightkit-qa-storage-'));
  try {
    const run = createManagedRun({ managedRoot: root, app: 'contract', runId: 'run-1', leaseMs: 60_000 });
    const markerPath = path.join(run.root, RUN_MARKER);
    const marker = JSON.parse(fs.readFileSync(markerPath, 'utf8'));
    assert.deepEqual(Object.keys(marker).sort(), ['app', 'createdAt', 'leaseUntil', 'ownerPid', 'runId', 'schemaVersion']);
    assert.equal(marker.schemaVersion, 1);
    const finished = finishManagedRun(run);
    assert.equal(finished.ownerPid, 0);
    assert.equal(finished.leaseUntil, 0);
    assert.equal(typeof finished.completedAt, 'number');
    const report = pruneManagedRuns(root, 'contract');
    assert.equal(typeof report.totalBytes, 'number');
    assert.equal(typeof report.overCapBytes, 'number');
    assert.equal(typeof report.blockedBytes, 'number');
    assert.equal(typeof report.unknownBytes, 'number');
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});
