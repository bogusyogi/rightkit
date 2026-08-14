import test from 'node:test';
import assert from 'node:assert/strict';
import { classifyManagedCommand } from './command-capabilities.mjs';
import { CapabilityScheduler, isBuildCapableRust, MachineFifoScheduler } from './scheduler.mjs';

test('machine lane grants strict FIFO across repositories', async () => {
  const scheduler = new MachineFifoScheduler({ heartbeatIntervalMs: 1_000 });
  const order = []; const first = scheduler.acquire({ id: 'one', repositoryIdentity: 'repo-a', onHeartbeat: (event) => order.push(`one:${event.state}`) });
  const second = scheduler.acquire({ id: 'two', repositoryIdentity: 'repo-b', onHeartbeat: (event) => order.push(`two:${event.state}`) });
  const third = scheduler.acquire({ id: 'three', repositoryIdentity: 'repo-a', onHeartbeat: (event) => order.push(`three:${event.state}`) });
  const one = await first.permit; assert.equal(scheduler.snapshot.active, 'one'); one.release();
  const two = await second.permit; assert.equal(scheduler.snapshot.active, 'two'); two.release();
  const three = await third.permit; assert.equal(scheduler.snapshot.active, 'three'); three.release();
  assert.deepEqual(order.filter((value) => value.endsWith(':ACTIVE')), ['one:ACTIVE', 'two:ACTIVE', 'three:ACTIVE']);
});

test('queued cancellation removes ticket without disturbing active build', async () => {
  const scheduler = new MachineFifoScheduler(); const first = scheduler.acquire({ id: 'one', repositoryIdentity: 'repo' }); const second = scheduler.acquire({ id: 'two', repositoryIdentity: 'repo' });
  const active = await first.permit; assert.equal(second.cancel(), true); await assert.rejects(second.permit, { code: 'RK_QUEUE_CANCELLED' }); assert.equal(scheduler.snapshot.active, 'one'); active.release();
});

test('only build-capable Rust enters heavy lane', () => {
  assert.equal(isBuildCapableRust('cargo', ['test']), true); assert.equal(isBuildCapableRust('rustc', ['--version']), true);
  assert.equal(isBuildCapableRust('cargo', ['metadata']), false); assert.equal(isBuildCapableRust('cargo', ['--version']), false); assert.equal(isBuildCapableRust('pnpm', ['test']), false);
});

test('CONTROL capacity stays independent from an active BUILD permit', async () => {
  const scheduler = new CapabilityScheduler({ controlConcurrency: 1, buildConcurrency: 1 });
  const build = scheduler.acquire({ id: 'build', lane: 'BUILD', repositoryIdentity: 'repo' });
  const control = scheduler.acquire({ id: 'about', lane: 'CONTROL', repositoryIdentity: 'repo' });
  const [buildPermit, controlPermit] = await Promise.all([build.permit, control.permit]);
  assert.deepEqual(scheduler.snapshot.BUILD.active, ['build']);
  assert.deepEqual(scheduler.snapshot.CONTROL.active, ['about']);
  controlPermit.release(); buildPermit.release();
});

test('same repository BUILD work stays exclusive even when buildConcurrency is raised', async () => {
  const scheduler = new CapabilityScheduler({ buildConcurrency: 3 });
  const first = scheduler.acquire({ id: 'first', lane: 'BUILD', repositoryIdentity: 'repo' });
  const second = scheduler.acquire({ id: 'second', lane: 'BUILD', repositoryIdentity: 'repo' });
  const other = scheduler.acquire({ id: 'other', lane: 'BUILD', repositoryIdentity: 'other' });
  const firstPermit = await first.permit;
  const otherPermit = await other.permit;
  assert.deepEqual(scheduler.snapshot.BUILD, { active: ['first', 'other'], queued: ['second'] });
  firstPermit.release();
  const secondPermit = await second.permit;
  secondPermit.release(); otherPermit.release();
});

test('registered CONTROL command never acquires a BUILD permit', async () => {
  const scheduler = new CapabilityScheduler();
  const build = scheduler.acquire({ id: 'build', lane: 'BUILD', repositoryIdentity: 'repo' });
  const capability = classifyManagedCommand('cargo', ['tree']);
  const control = scheduler.acquire({ id: 'tree', lane: capability.lane, repositoryIdentity: 'repo' });
  const [buildPermit, controlPermit] = await Promise.all([build.permit, control.permit]);
  assert.deepEqual(scheduler.snapshot.BUILD.active, ['build']);
  assert.deepEqual(scheduler.snapshot.CONTROL.active, ['tree']);
  controlPermit.release(); buildPermit.release();
});

test('WORKSPACE_WRITE serializes each repository while allowing different repositories', async () => {
  const scheduler = new CapabilityScheduler({ workspaceWriteConcurrency: 2 });
  const first = scheduler.acquire({ id: 'first', lane: 'WORKSPACE_WRITE', repositoryIdentity: 'repo-a' });
  const sameRepo = scheduler.acquire({ id: 'same-repo', lane: 'WORKSPACE_WRITE', repositoryIdentity: 'repo-a' });
  const otherRepo = scheduler.acquire({ id: 'other-repo', lane: 'WORKSPACE_WRITE', repositoryIdentity: 'repo-b' });
  const [firstPermit, otherPermit] = await Promise.all([first.permit, otherRepo.permit]);
  assert.deepEqual(scheduler.snapshot.WORKSPACE_WRITE, { active: ['first', 'other-repo'], queued: ['same-repo'] });
  firstPermit.release(); const samePermit = await sameRepo.permit;
  samePermit.release(); otherPermit.release();
});

test('WORKSPACE_WRITE waits for active same-repository BUILD then drains on release', async () => {
  const scheduler = new CapabilityScheduler();
  const build = scheduler.acquire({ id: 'build', lane: 'BUILD', repositoryIdentity: 'repo-a' });
  const write = scheduler.acquire({ id: 'clean', lane: 'WORKSPACE_WRITE', repositoryIdentity: 'repo-a' });
  const buildPermit = await build.permit;
  assert.deepEqual(scheduler.snapshot.WORKSPACE_WRITE, { active: [], queued: ['clean'] });
  buildPermit.release(); const writePermit = await write.permit;
  assert.deepEqual(scheduler.snapshot.WORKSPACE_WRITE.active, ['clean']);
  writePermit.release();
});

test('BUILD waits for active same-repository WORKSPACE_WRITE then drains on release', async () => {
  const scheduler = new CapabilityScheduler();
  const write = scheduler.acquire({ id: 'update', lane: 'WORKSPACE_WRITE', repositoryIdentity: 'repo-a' });
  const build = scheduler.acquire({ id: 'build', lane: 'BUILD', repositoryIdentity: 'repo-a' });
  const writePermit = await write.permit;
  assert.deepEqual(scheduler.snapshot.BUILD, { active: [], queued: ['build'] });
  writePermit.release(); const buildPermit = await build.permit;
  assert.deepEqual(scheduler.snapshot.BUILD.active, ['build']);
  buildPermit.release();
});

test('bounded MAINTENANCE is independent from active BUILD work', async () => {
  const scheduler = new CapabilityScheduler({ maintenanceConcurrency: 1 });
  const build = scheduler.acquire({ id: 'build', lane: 'BUILD', repositoryIdentity: 'repo' });
  const maintenance = scheduler.acquire({ id: 'gc', lane: 'MAINTENANCE', repositoryIdentity: 'broker' });
  const [buildPermit, maintenancePermit] = await Promise.all([build.permit, maintenance.permit]);
  assert.deepEqual(scheduler.snapshot.BUILD.active, ['build']);
  assert.deepEqual(scheduler.snapshot.MAINTENANCE.active, ['gc']);
  maintenancePermit.release(); buildPermit.release();
});

test('installer is next queued BUILD request but never preempts an active build', async () => {
  const scheduler = new CapabilityScheduler();
  const active = scheduler.acquire({ id: 'active', lane: 'BUILD', repositoryIdentity: 'repo' });
  const batch = scheduler.acquire({ id: 'batch', lane: 'BUILD', repositoryIdentity: 'repo' });
  const installer = scheduler.acquire({ id: 'installer', lane: 'BUILD', priority: 'INSTALLER', repositoryIdentity: 'repo' });
  const activePermit = await active.permit;
  assert.deepEqual(scheduler.snapshot.BUILD, { active: ['active'], queued: ['batch', 'installer'] });
  activePermit.release();
  const installerPermit = await installer.permit;
  assert.deepEqual(scheduler.snapshot.BUILD.active, ['installer']);
  installerPermit.release(); (await batch.permit).release();
});

test('BUILD priority aging prevents installers from starving old batch work', async () => {
  let now = 0;
  const scheduler = new CapabilityScheduler({ priorityAgingMs: 10, now: () => now });
  const active = scheduler.acquire({ id: 'active', lane: 'BUILD', repositoryIdentity: 'repo' });
  const oldBatch = scheduler.acquire({ id: 'old-batch', lane: 'BUILD', repositoryIdentity: 'repo' });
  await active.permit;
  now = 20;
  const installer = scheduler.acquire({ id: 'installer', lane: 'BUILD', priority: 'INSTALLER', repositoryIdentity: 'repo' });
  (await active.permit).release();
  const oldPermit = await oldBatch.permit;
  assert.deepEqual(scheduler.snapshot.BUILD.active, ['old-batch']);
  oldPermit.release(); (await installer.permit).release();
});
