import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { ThinLogger } from './thin-logger.mjs';

test('serializes append-only NDJSON without per-event sync contract', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-thin-log-')); const path = join(root, 'logs', 'events.ndjson');
  try {
    const logger = await ThinLogger.open({ path, now: () => new Date('2026-08-22T01:02:03Z') });
    await Promise.all([logger.append({ type: 'QUEUED', id: 'one' }), logger.append({ type: 'STARTED', id: 'one' })]);
    await logger.close();
    const rows = (await readFile(path, 'utf8')).trim().split('\n').map(JSON.parse);
    assert.deepEqual(rows.map(({ type }) => type), ['QUEUED', 'STARTED']);
    assert.equal(rows[0].at, '2026-08-22T01:02:03.000Z');
  } finally { await rm(root, { recursive: true, force: true }); }
});
