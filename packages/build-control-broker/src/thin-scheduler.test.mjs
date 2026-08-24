import assert from 'node:assert/strict';
import test from 'node:test';
import { ThinScheduler, deriveCompileCosts } from './thin-scheduler.mjs';
import { compileClass, ThinCommandPolicy } from './thin-command-policy.mjs';

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

test('compileClass maps classified capabilities onto dual-host command classes', () => {
  const policy = new ThinCommandPolicy();
  assert.equal(compileClass(policy.classify('cargo', ['check'])), 'ANALYSIS');
  assert.equal(compileClass(policy.classify('cargo', ['clippy'])), 'ANALYSIS');
  assert.equal(compileClass(policy.classify('cargo', ['test'])), 'TEST');
  assert.equal(compileClass(policy.classify('cargo', ['build'])), 'BUILD');
  assert.equal(compileClass(policy.classify('cargo', ['run'])), 'BUILD');
  assert.equal(compileClass(policy.classify('cargo', ['metadata', '--locked'])), null);
  assert.equal(compileClass({ lane: 'BUILD', workspaceAccess: 'write' }), null);
});

test('deriveCompileCosts yields three analysis / two test / one build at budget 12', () => {
  assert.deepEqual(deriveCompileCosts(12), { ANALYSIS: 4, TEST: 6, BUILD: 12 });
  assert.deepEqual(deriveCompileCosts(4), { ANALYSIS: 1, TEST: 2, BUILD: 4 });
  assert.throws(() => deriveCompileCosts(0), TypeError);
});

test('aggregate budget admits analysis mixtures & blocks over-budget roots', async () => {
  const scheduler = new ThinScheduler({ buildConcurrency: 8, compileBudget: 12 });
  const costs = deriveCompileCosts(12);
  const first = await scheduler.acquire({ id: 'check-1', capability: BUILD, workspaceKey: 'a', compileCost: costs.ANALYSIS }).permit;
  const second = await scheduler.acquire({ id: 'check-2', capability: BUILD, workspaceKey: 'b', compileCost: costs.ANALYSIS }).permit;
  const third = await scheduler.acquire({ id: 'check-3', capability: BUILD, workspaceKey: 'c', compileCost: costs.ANALYSIS }).permit;
  // Fourth analysis root would exceed 4+4+4=12: queues.
  const fourth = scheduler.acquire({ id: 'check-4', capability: BUILD, workspaceKey: 'd', compileCost: costs.ANALYSIS });
  await tick();
  assert.equal(scheduler.snapshot.active.length, 3);
  assert.deepEqual(scheduler.snapshot.queued.map(({ id }) => id), ['check-4']);
  first.release();
  await tick();
  assert.deepEqual(scheduler.snapshot.active.map(({ id }) => id).sort(), ['check-2', 'check-3', 'check-4']);
  second.release(); third.release(); (await fourth.permit).release();
});

test('aggregate budget admits test/analysis mixtures while total fits', async () => {
  const scheduler = new ThinScheduler({ buildConcurrency: 8, compileBudget: 12 });
  const costs = deriveCompileCosts(12);
  const testRoot = await scheduler.acquire({ id: 'test-1', capability: BUILD, workspaceKey: 't', compileCost: costs.TEST }).permit;
  const analysisA = await scheduler.acquire({ id: 'check-a', capability: BUILD, workspaceKey: 'ca', compileCost: costs.ANALYSIS }).permit;
  // 6+4=10 <= 12 admitted; a second analysis root would reach 14 > 12: queued.
  assert.equal(scheduler.snapshot.active.length, 2);
  const analysisB = scheduler.acquire({ id: 'check-b', capability: BUILD, workspaceKey: 'cb', compileCost: costs.ANALYSIS });
  await tick();
  assert.deepEqual(scheduler.snapshot.queued.map(({ id }) => id), ['check-b']);
  testRoot.release();
  await tick();
  assert.deepEqual(scheduler.snapshot.active.map(({ id }) => id).sort(), ['check-a', 'check-b']);
  analysisA.release(); (await analysisB.permit).release();
});

test('one heavy build consumes the full budget until it releases', async () => {
  const scheduler = new ThinScheduler({ buildConcurrency: 8, compileBudget: 12 });
  const costs = deriveCompileCosts(12);
  const build = await scheduler.acquire({ id: 'build-heavy', capability: BUILD, workspaceKey: 'h', compileCost: costs.BUILD }).permit;
  const check = scheduler.acquire({ id: 'check-x', capability: BUILD, workspaceKey: 'x', compileCost: costs.ANALYSIS });
  await tick();
  assert.equal(scheduler.snapshot.active.length, 1);
  build.release();
  await tick();
  (await check.permit).release();
});

test('lane caps remain a secondary bound under an aggregate budget', async () => {
  // Budget would allow four 1-cost analysis roots but ANALYSIS lane cap is 2.
  const scheduler = new ThinScheduler({ analysisConcurrency: 2, compileBudget: 12 });
  const costs = deriveCompileCosts(12);
  const a = await scheduler.acquire({ id: 'an-1', capability: { lane: 'ANALYSIS', workspaceAccess: 'read' }, workspaceKey: '1', compileCost: costs.ANALYSIS }).permit;
  const b = await scheduler.acquire({ id: 'an-2', capability: { lane: 'ANALYSIS', workspaceAccess: 'read' }, workspaceKey: '2', compileCost: costs.ANALYSIS }).permit;
  const c = scheduler.acquire({ id: 'an-3', capability: { lane: 'ANALYSIS', workspaceAccess: 'read' }, workspaceKey: '3', compileCost: costs.ANALYSIS });
  await tick();
  assert.equal(scheduler.snapshot.active.length, 2);
  a.release(); b.release();
  (await c.permit).release();
});

test('legacy lane-only behavior is unchanged without a compileBudget', async () => {
  const scheduler = new ThinScheduler({ buildConcurrency: 3 });
  const roots = [await scheduler.acquire({ id: 'b1', capability: BUILD, workspaceKey: '1' }).permit,
                 await scheduler.acquire({ id: 'b2', capability: BUILD, workspaceKey: '2' }).permit];
  const queuedThird = scheduler.acquire({ id: 'b3', capability: BUILD, workspaceKey: '3', compileCost: null });
  await tick();
  // No compileBudget configured: all three roots admit concurrently exactly as
  // under the pre-dual-host scheduler.
  assert.equal(scheduler.snapshot.active.length, 3);
  roots.forEach((permit) => permit.release());
  (await queuedThird.permit).release();
  assert.throws(() => scheduler.acquire({ id: 'bad', capability: BUILD, compileCost: 4 }), TypeError);
});
