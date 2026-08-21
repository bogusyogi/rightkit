import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { ThinState } from './thin-state.mjs';

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-thin-state-'));
  const workspace = join(root, 'workspace');
  await (await import('node:fs/promises')).mkdir(workspace);
  const state = await ThinState.open({ externalRoot: root, now: () => new Date('2026-08-22T00:00:00Z') });
  return { root, workspace, state, close: () => rm(root, { recursive: true, force: true }) };
}

test('creates required global & persistent workspace generation layout', async () => {
  const item = await fixture();
  try {
    const workspace = await item.state.workspace(item.workspace);
    assert.equal(workspace.state.currentGeneration, 1);
    for (const path of [item.state.paths.state, item.state.paths.cargoHome, item.state.paths.compilerCache, item.state.paths.tools, item.state.paths.active, item.state.paths.logs, item.state.paths.receipts, workspace.layout.target, workspace.layout.temp]) await assert.doesNotReject((await import('node:fs/promises')).stat(path));
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
