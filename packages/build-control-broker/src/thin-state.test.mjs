import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { ThinState } from './thin-state.mjs';

async function fixture({ advanceClock = false } = {}) {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-thin-state-'));
  const workspace = join(root, 'workspace');
  await (await import('node:fs/promises')).mkdir(workspace);
  let clock = Date.parse('2026-08-22T00:00:00Z');
  const state = await ThinState.open({ externalRoot: root, now: () => { const value = new Date(clock); if (advanceClock) clock += 1_000; return value; } });
  return { root, workspace, state, close: () => rm(root, { recursive: true, force: true }) };
}

test('creates required global & persistent workspace generation layout', async () => {
  const item = await fixture();
  try {
    const workspace = await item.state.workspace(item.workspace);
    assert.equal(workspace.state.currentGeneration, 1);
    for (const path of [item.state.paths.state, item.state.paths.cargoHome, item.state.paths.compilerCache, item.state.paths.tools, item.state.paths.active, item.state.paths.trash, item.state.paths.logs, item.state.paths.receipts, workspace.layout.target, workspace.layout.temp]) await assert.doesNotReject((await import('node:fs/promises')).stat(path));
    assert.equal(workspace.layout.temp, join(item.root, 'tmp'));
    assert.equal(workspace.layout.build, undefined);
    assert.equal(JSON.parse(await readFile(workspace.identityPath, 'utf8')).canonicalRoot, item.workspace);
  } finally { await item.close(); }
});

test('normal completion preserves generation & writes terminal receipt', async () => {
  const item = await fixture();
  try {
    const running = await item.state.markRunning(item.workspace, { id: 'job-1', ownership: { pid: 42 }, command: 'cargo check' });
    assert.equal(running.state.active.id, 'job-1');
    await item.state.finish(item.workspace, 'job-1', { status: 'SUCCEEDED', generation: 1 });
    const current = await item.state.workspace(item.workspace);
    assert.equal(current.state.currentGeneration, 1);
    assert.equal(current.state.active, null);
    assert.deepEqual(await item.state.receipt('job-1'), { id: 'job-1', status: 'SUCCEEDED', generation: 1 });
  } finally { await item.close(); }
});

test('restart rotates only workspace with active writer & records ABANDONED', async () => {
  const item = await fixture();
  try {
    const other = join(item.root, 'other'); await (await import('node:fs/promises')).mkdir(other);
    await item.state.workspace(other);
    await item.state.markRunning(item.workspace, { id: 'job-crashed', ownership: { pid: 51 } });
    const calls = [];
    const recovered = await item.state.recoverActive({ reap: async (ownership) => { calls.push(ownership); return { reaped: false, reason: 'still-alive' }; } });
    assert.equal(recovered.length, 1);
    assert.deepEqual(calls, [{ pid: 51 }]);
    assert.equal((await item.state.workspace(item.workspace)).state.currentGeneration, 2);
    assert.equal((await item.state.workspace(other)).state.currentGeneration, 1);
    assert.equal((await item.state.receipt('job-crashed')).status, 'ABANDONED');
  } finally { await item.close(); }
});

test('thin GC deletes oldest retired generation while preserving current generation', async () => {
  const item = await fixture();
  try {
    const first = await item.state.workspace(item.workspace); await writeFile(join(first.layout.target, 'old.bin'), Buffer.alloc(16));
    const second = await item.state.rotate(item.workspace, 'test'); await writeFile(join(second.layout.target, 'current.bin'), Buffer.alloc(16));
    const result = await item.state.gc({ capBytes: 20 });
    assert.deepEqual(result.deleted.map(({ generation }) => generation), [1]);
    assert.equal(result.evicted.length, 0);
    assert.equal(result.unenforceable, null);
    await assert.rejects(stat(first.layout.root), { code: 'ENOENT' }); await assert.doesNotReject(stat(second.layout.root));
  } finally { await item.close(); }
});

test('markRunning records LRU usage & finish marks the generation size dirty', async () => {
  const item = await fixture();
  try {
    const workspace = await item.state.workspace(item.workspace);
    assert.equal(workspace.state.lastUsedAt, undefined);
    await item.state.markRunning(item.workspace, { id: 'job-lru', command: 'cargo build' });
    await item.state.finish(item.workspace, 'job-lru', { status: 'SUCCEEDED' });
    const current = await item.state.workspace(item.workspace);
    assert.equal(typeof current.state.lastUsedAt, 'string');
    assert.equal(current.state.sizeDirty, true);
  } finally { await item.close(); }
});

test('evictIdleWorkspace rotates an idle current generation & deletes the retired directory', async () => {
  const item = await fixture();
  try {
    const workspace = await item.state.workspace(item.workspace);
    await writeFile(join(workspace.layout.target, 'warm.bin'), Buffer.alloc(64));
    const result = await item.state.evictIdleWorkspace(item.workspace);
    assert.equal(result.evictedGeneration, 1);
    assert.equal(result.newGeneration, 2);
    assert.equal(result.reason, 'cache-budget-eviction');
    await assert.rejects(stat(workspace.layout.root), { code: 'ENOENT' });
    const current = await item.state.workspace(item.workspace);
    assert.equal(current.state.currentGeneration, 2);
    assert.equal(current.state.sizeDirty, false);
    assert.equal(current.state.lastMeasuredBytes, 0);
  } finally { await item.close(); }
});

test('evictIdleWorkspace never evicts an active generation', async () => {
  const item = await fixture();
  try {
    await item.state.markRunning(item.workspace, { id: 'job-active', command: 'cargo build' });
    assert.equal(await item.state.evictIdleWorkspace(item.workspace), null);
    const current = await item.state.workspace(item.workspace);
    assert.equal(current.state.currentGeneration, 1);
    await assert.doesNotReject(stat(current.layout.root));
  } finally { await item.close(); }
});

test('GC evicts idle current generations LRU-first when the cap requires it', async () => {
  const item = await fixture({ advanceClock: true });
  try {
    const mkdtempImport = await import('node:fs/promises');
    const oldRoot = join(item.root, 'app-old'); const newRoot = join(item.root, 'app-new');
    await mkdtempImport.mkdir(oldRoot); await mkdtempImport.mkdir(newRoot);
    const oldWorkspace = await item.state.workspace(oldRoot);
    const newWorkspace = await item.state.workspace(newRoot);
    await writeFile(join(oldWorkspace.layout.target, 'w.bin'), Buffer.alloc(64));
    await writeFile(join(newWorkspace.layout.target, 'w.bin'), Buffer.alloc(64));
    // oldRoot used most recently: newRoot must be evicted first.
    await item.state.markRunning(newRoot, { id: 'job-old-1', command: 'cargo build' }); await item.state.finish(newRoot, 'job-old-1', { status: 'SUCCEEDED' });
    await item.state.markRunning(oldRoot, { id: 'job-new-1', command: 'cargo build' }); await item.state.finish(oldRoot, 'job-new-1', { status: 'SUCCEEDED' });
    const evicted = [];
    const evictIdle = async (candidate) => { evicted.push(candidate.canonicalRoot); return item.state.evictIdleWorkspace(candidate.canonicalRoot, { sizeBytes: candidate.sizeBytes }); };
    const result = await item.state.gc({ capBytes: 64, evictIdle });
    assert.deepEqual(evicted, [newRoot]);
    assert.equal(result.evicted.length, 1);
    assert.equal(result.evicted[0].evictedGeneration, 1);
    assert.equal(result.unenforceable, null);
    assert.ok(result.afterBytes <= 64, `afterBytes ${result.afterBytes} must be within cap`);
    assert.equal((await item.state.workspace(oldRoot)).state.currentGeneration, 1);
    assert.equal((await item.state.workspace(newRoot)).state.currentGeneration, 2);
  } finally { await item.close(); }
});

test('GC reports an unenforceable cap instead of wedging when eviction is refused', async () => {
  const item = await fixture();
  try {
    const workspace = await item.state.workspace(item.workspace);
    await writeFile(join(workspace.layout.target, 'w.bin'), Buffer.alloc(64));
    const result = await item.state.gc({ capBytes: 8, evictIdle: async () => null });
    assert.equal(result.evicted.length, 0);
    assert.equal(result.unenforceable.overByBytes, 56);
    await assert.doesNotReject(stat(workspace.layout.root));
  } finally { await item.close(); }
});

test('GC reuses cached current-generation sizes until the workspace is dirty', async () => {
  const item = await fixture();
  try {
    const workspace = await item.state.workspace(item.workspace);
    await writeFile(join(workspace.layout.target, 'w.bin'), Buffer.alloc(32));
    // First GC measures & caches; second GC with a fresh state read must reuse it.
    await item.state.gc({ capBytes: 10_000 });
    const cached = (await item.state.workspace(item.workspace)).state;
    assert.equal(cached.lastMeasuredGeneration, 1);
    assert.equal(cached.lastMeasuredBytes, 32);
    assert.equal(cached.sizeDirty, false);
    const again = await item.state.gc({ capBytes: 10_000 });
    assert.equal(again.beforeBytes, 32);
    assert.deepEqual(again.deleted, []);
    assert.equal(again.evicted.length, 0);
  } finally { await item.close(); }
});
