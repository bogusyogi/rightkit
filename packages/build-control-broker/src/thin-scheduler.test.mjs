import assert from 'node:assert/strict';
import test from 'node:test';
import { ThinScheduler } from './thin-scheduler.mjs';

const cap = (lane, workspaceAccess, compile = false) => ({ lane, workspaceAccess, compile });
const BUILD = cap('BUILD', 'write', true); const CONTROL = cap('CONTROL', 'none'); const ANALYSIS = cap('ANALYSIS', 'read'); const WRITE = cap('WORKSPACE_WRITE', 'write');
const tick = () => new Promise((resolve) => setImmediate(resolve));

test('one global BUILD runs while second queues & CONTROL remains responsive', async () => {
  const scheduler = new ThinScheduler({ buildConcurrency: 1 });
  const first = scheduler.acquire({ id: 'build-1', capability: BUILD, workspaceKey: 'a' });
  const second = scheduler.acquire({ id: 'build-2', capability: BUILD, workspaceKey: 'b' });
  const control = scheduler.acquire({ id: 'control', capability: CONTROL });
  const firstPermit = await first.permit; const controlPermit = await control.permit;
  await tick();
  assert.deepEqual(scheduler.snapshot.queued.map(({ id }) => id), ['build-2']);
  controlPermit.release(); firstPermit.release();
  (await second.permit).release();
});

test('ANALYSIS does not consume BUILD slot but obeys workspace writer exclusion', async () => {
  const scheduler = new ThinScheduler({ buildConcurrency: 1 });
  const analysis = scheduler.acquire({ id: 'analysis', capability: ANALYSIS, workspaceKey: 'a' });
  const buildOther = scheduler.acquire({ id: 'build-other', capability: BUILD, workspaceKey: 'b' });
  const writerSame = scheduler.acquire({ id: 'writer-same', capability: WRITE, workspaceKey: 'a' });
  const analysisPermit = await analysis.permit; const buildPermit = await buildOther.permit;
  await tick(); assert.deepEqual(scheduler.snapshot.queued.map(({ id }) => id), ['writer-same']);
  analysisPermit.release(); (await writerSame.permit).release(); buildPermit.release();
});
