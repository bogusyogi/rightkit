import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readdir, rm, utimes, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { sweepTemporary } from './temp-sweep.mjs';

test('sweeps only entries older than the cutoff and the oldest running job, keeping live endpoints', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rk-sweep-test-'));
  try {
    const now = Date.now(); const hours = (n) => new Date(now - n * 3_600_000);
    await mkdir(join(root, '.tmpOld')); await writeFile(join(root, '.tmpOld', 'f'), 'x');
    await mkdir(join(root, '.tmpRecent'));
    await mkdir(join(root, '.tmpOwnedByRunningJob'));
    await writeFile(join(root, 'sccache-gateway.sock'), '');
    await utimes(join(root, '.tmpOld'), hours(10), hours(10));
    await utimes(join(root, '.tmpRecent'), hours(1), hours(1));
    await utimes(join(root, '.tmpOwnedByRunningJob'), hours(8), hours(8));
    await utimes(join(root, 'sccache-gateway.sock'), hours(48), hours(48));
    // A job that started 9 h ago may own anything touched after it started.
    const result = await sweepTemporary(root, { now, notAfterMs: now - 9 * 3_600_000 });
    assert.equal(result.removed, 1);
    assert.deepEqual((await readdir(root)).sort(), ['.tmpOwnedByRunningJob', '.tmpRecent', 'sccache-gateway.sock']);
    const idle = await sweepTemporary(root, { now });
    assert.equal(idle.removed, 1, 'with no running job, the 8 h entry goes');
  } finally { await rm(root, { recursive: true, force: true }); }
});
