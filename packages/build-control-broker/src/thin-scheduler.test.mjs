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

test('queued interactive BUILD starts before installer & batch without preemption', async () => {
  const scheduler = new ThinScheduler({ buildConcurrency: 1 });
  const active = scheduler.acquire({ id: 'active', capability: BUILD, workspaceKey: 'active' });
  const activePermit = await active.permit;
  const batch = scheduler.acquire({ id: 'batch', capability: BUILD, workspaceKey: 'batch', priority: 'BATCH' });
  const installer = scheduler.acquire({ id: 'installer', capability: BUILD, workspaceKey: 'installer', priority: 'INSTALLER' });
  const interactive = scheduler.acquire({ id: 'interactive', capability: BUILD, workspaceKey: 'interactive', priority: 'INTERACTIVE' });
  await tick();
  assert.equal(scheduler.snapshot.active[0].id, 'active');
  activePermit.release();
  const interactivePermit = await interactive.permit;
  assert.equal(scheduler.snapshot.active[0].id, 'interactive');
  interactivePermit.release();
  (await installer.permit).release();
  (await batch.permit).release();
});

test('tryRun acquires immediately or rejects — never queues behind an active writer', async () => {
  const scheduler = new ThinScheduler({ buildConcurrency: 1, controlConcurrency: 2, analysisConcurrency: 2, workspaceWriteConcurrency: 2, now: Date.now });
  const build = scheduler.acquire({ id: 'build-1', capability: { lane: 'BUILD', workspaceAccess: 'write', compile: true }, workspaceKey: 'ws' });
  const buildPermit = await build.permit;
  // Same workspace, write access: must reject (writer active), not queue.
  await assert.rejects(scheduler.acquire({ id: 'gc-1', capability: { lane: 'WORKSPACE_WRITE', workspaceAccess: 'write' }, workspaceKey: 'ws', tryRun: true }).permit, { code: 'RK_QUEUE_UNAVAILABLE' });
  // Different workspace with free write capacity: acquires immediately.
  const free = scheduler.acquire({ id: 'gc-2', capability: { lane: 'WORKSPACE_WRITE', workspaceAccess: 'write' }, workspaceKey: 'other', tryRun: true });
  const freePermit = await free.permit;
  assert.equal(free.cancel(), false);
  freePermit.release();
  buildPermit.release();
  // After release the workspace is evictable again.
  const retry = scheduler.acquire({ id: 'gc-3', capability: { lane: 'WORKSPACE_WRITE', workspaceAccess: 'write' }, workspaceKey: 'ws', tryRun: true });
  const retryPermit = await retry.permit;
  retryPermit.release();
});
