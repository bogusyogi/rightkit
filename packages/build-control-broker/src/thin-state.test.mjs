import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, readdir, realpath, rm, stat, utimes, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { atomicJson, directorySize, replaceAtomicFile, ThinState, thinWorkspaceKey } from './thin-state.mjs';

async function fixture({ advanceClock = false } = {}) {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-thin-state-'));
  const workspace = join(root, 'workspace');
  await (await import('node:fs/promises')).mkdir(workspace);
  let clock = Date.parse('2026-08-22T00:00:00Z');
  const state = await ThinState.open({ externalRoot: root, now: () => { const value = new Date(clock); if (advanceClock) clock += 1_000; return value; } });
  return { root, workspace, state, close: () => rm(root, { recursive: true, force: true }) };
}

test('Windows atomic replacement retries transient sharing failures', async () => {
  const waits = []; let attempts = 0;
  await replaceAtomicFile('state.tmp', 'state.json', {
    platform: 'win32', retryDelays: [5, 10, 20], wait: async (milliseconds) => waits.push(milliseconds),
    renameFile: async () => {
      attempts += 1;
      if (attempts < 3) throw Object.assign(new Error('sharing violation'), { code: 'EPERM' });
    }
  });
  assert.equal(attempts, 3);
  assert.deepEqual(waits, [5, 10]);
});

test('atomic JSON removes its temporary file after bounded replacement failure', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-thin-state-atomic-'));
  try {
    await assert.rejects(atomicJson(join(root, 'state.json'), { state: 'test' }, {
      platform: 'win32', retryDelays: [1], wait: async () => {},
      renameFile: async () => { throw Object.assign(new Error('sharing violation'), { code: 'EPERM' }); }
    }), { code: 'EPERM' });
    assert.deepEqual(await readdir(root), []);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('atomic replacement does not retry non-transient failures', async () => {
  let attempts = 0;
  await assert.rejects(replaceAtomicFile('state.tmp', 'state.json', {
    platform: 'win32', wait: async () => assert.fail('permanent failure must not wait'),
    renameFile: async () => { attempts += 1; throw Object.assign(new Error('access denied'), { code: 'EACCES' }); }
  }), { code: 'EACCES' });
  assert.equal(attempts, 1);
});

test('creates required global & persistent workspace generation layout', async () => {
  const item = await fixture();
  try {
    const workspace = await item.state.workspace(item.workspace);
    assert.equal(workspace.state.currentGeneration, 1);
    for (const path of [item.state.paths.state, item.state.paths.cargoHome, item.state.paths.compilerCache, item.state.paths.tools, item.state.paths.active, item.state.paths.trash, item.state.paths.logs, item.state.paths.receipts, workspace.layout.target, workspace.layout.temp]) await assert.doesNotReject((await import('node:fs/promises')).stat(path));
    assert.equal(workspace.layout.temp, join(item.root, 'tmp'));
    assert.equal(workspace.layout.target, join(workspace.layout.root, 'target'));
    assert.equal(workspace.layout.leaf, 'target');
    assert.equal(workspace.layout.leafReason, 'persisted (neither-exists-default-target)');
    assert.equal(workspace.layout.orphanLeaf, null);
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
    // 128 bytes over a 72-byte cap reclaims to 90 % (64): one eviction suffices.
    const result = await item.state.gc({ capBytes: 72, evictIdle, recentMs: 0 });
    assert.deepEqual(evicted, [newRoot]);
    assert.equal(result.evicted.length, 1);
    assert.equal(result.evicted[0].evictedGeneration, 1);
    assert.equal(result.unenforceable, null);
    assert.ok(result.afterBytes <= 72, `afterBytes ${result.afterBytes} must be within cap`);
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

test('dirty generations are remeasured in the same GC pass that decides the cap', async () => {
  const item = await fixture();
  try {
    const workspace = await item.state.workspace(item.workspace);
    await writeFile(join(workspace.layout.target, 'small.bin'), Buffer.alloc(16));
    // First GC caches the size at 16 bytes.
    await item.state.gc({ capBytes: 1_000_000 });
    assert.equal((await item.state.workspace(item.workspace)).state.lastMeasuredBytes, 16);
    // Artifact growth marks the workspace dirty; the cached 16 must not be
    // used for the next cap decision.
    await writeFile(join(workspace.layout.target, 'grown.bin'), Buffer.alloc(512));
    await item.state.markRunning(item.workspace, { id: 'job-grow', command: 'cargo build' });
    await item.state.finish(item.workspace, 'job-grow', { status: 'SUCCEEDED' });
    const measured = await item.state.gc({ capBytes: 8 });
    // Fresh walk must see the grown size (>= 528), not the stale 16.
    assert.ok(measured.beforeBytes >= 528, `stale size used for cap decision: ${measured.beforeBytes}`);
    assert.equal(measured.unenforceable.overByBytes, measured.beforeBytes - 8);
    const persisted = (await item.state.workspace(item.workspace)).state;
    assert.equal(persisted.lastMeasuredBytes, measured.beforeBytes);
    assert.equal(persisted.sizeDirty, false);
  } finally { await item.close(); }
});

test('GC continues past a refused oldest-LRU candidate to the next eligible workspace', async () => {
  const item = await fixture({ advanceClock: true });
  try {
    const fs = await import('node:fs/promises');
    const oldest = join(item.root, 'app-oldest'); const newest = join(item.root, 'app-newest');
    await fs.mkdir(oldest); await fs.mkdir(newest);
    for (const root of [oldest, newest]) {
      const workspace = await item.state.workspace(root);
      await writeFile(join(workspace.layout.target, 'w.bin'), Buffer.alloc(64));
    }
    await item.state.markRunning(oldest, { id: 'job-o', command: 'cargo build' }); await item.state.finish(oldest, 'job-o', { status: 'SUCCEEDED' });
    await item.state.markRunning(newest, { id: 'job-n', command: 'cargo build' }); await item.state.finish(newest, 'job-n', { status: 'SUCCEEDED' });
    const refused = [];
    // Refuse only the oldest candidate (e.g. its scheduler write slot is busy).
    const evictIdle = async (candidate) => {
      if (candidate.canonicalRoot === oldest) { refused.push(candidate.canonicalRoot); return null; }
      return item.state.evictIdleWorkspace(candidate.canonicalRoot, { sizeBytes: candidate.sizeBytes });
    };
    const result = await item.state.gc({ capBytes: 64, evictIdle, recentMs: 0 });
    assert.deepEqual(refused, [oldest], 'oldest-LRU must be attempted first');
    assert.deepEqual(result.evicted.map((entry) => entry.canonicalRoot), [newest], 'GC must continue to the next candidate after refusal');
    assert.equal(result.unenforceable, null, 'cap must be enforceable once a later candidate evicts');
    await assert.doesNotReject(stat((await item.state.workspace(oldest)).layout.root));
  } finally { await item.close(); }
});

test('crash window between state transition and rename is harmless: orphaned old generation is retired-GC-able', async () => {
  const item = await fixture();
  try {
    const workspace = await item.state.workspace(item.workspace);
    await writeFile(join(workspace.layout.target, 'orphan.bin'), Buffer.alloc(64));
    // Simulate the crash window in evictIdleWorkspace: state advanced to
    // generation 2 (persisted) but the generation-1 directory was never
    // renamed/deleted — exactly what a broker crash between atomicJson and
    // rename leaves behind.
    const fs = await import('node:fs/promises');
    const statePath = workspace.statePath;
    const state = JSON.parse(await readFile(statePath, 'utf8'));
    await fs.mkdir(join(workspace.directory, 'g', '002', 'target'), { recursive: true });
    await (await import('node:fs/promises')).writeFile(statePath, JSON.stringify({ ...state, currentGeneration: 2, active: null, updatedAt: state.updatedAt }));
    // The next GC must treat the leftover generation 1 as a retired candidate
    // (not current, not active) and reclaim it over cap.
    const result = await item.state.gc({ capBytes: 16 });
    assert.deepEqual(result.deleted.map(({ generation }) => generation), [1]);
    await assert.rejects(stat(workspace.layout.root), { code: 'ENOENT' });
    assert.equal((await item.state.workspace(item.workspace)).state.currentGeneration, 2);
  } finally { await item.close(); }
});

test('pre-v2.8 state.json without LRU metadata migrates lazily through GC', async () => {
  const item = await fixture();
  try {
    const workspace = await item.state.workspace(item.workspace);
    await writeFile(join(workspace.layout.target, 'legacy.bin'), Buffer.alloc(32));
    // Rewrite state.json in the legacy shape: none of lastUsedAt, sizeDirty,
    // lastMeasuredGeneration, lastMeasuredBytes, lastMeasuredAt exist.
    const fs = await import('node:fs/promises');
    const legacy = { schemaVersion: 1, currentGeneration: 1, active: null, updatedAt: '2026-08-01T00:00:00.000Z' };
    await fs.writeFile(workspace.statePath, JSON.stringify(legacy));
    const loaded = await item.state.workspace(item.workspace);
    assert.equal(loaded.state.currentGeneration, 1, 'legacy state must validate & load');
    // GC must measure (no cached size), persist metadata, and not throw.
    const result = await item.state.gc({ capBytes: 1_000_000 });
    assert.equal(result.beforeBytes, 32);
    const migrated = (await item.state.workspace(item.workspace)).state;
    assert.equal(migrated.lastMeasuredGeneration, 1);
    assert.equal(migrated.lastMeasuredBytes, 32);
    assert.equal(migrated.sizeDirty, false);
    assert.equal(typeof migrated.lastMeasuredAt, 'string');
    // markRunning/finish still upgrade legacy state in place.
    await item.state.markRunning(item.workspace, { id: 'job-legacy', command: 'cargo build' });
    await item.state.finish(item.workspace, 'job-legacy', { status: 'SUCCEEDED' });
    const after = (await item.state.workspace(item.workspace)).state;
    assert.equal(typeof after.lastUsedAt, 'string');
    assert.equal(after.sizeDirty, true);
  } finally { await item.close(); }
});

test('directorySize tolerates files and directories vanishing during the scan', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-thin-size-'));
  try {
    const { mkdir } = await import('node:fs/promises');
    await writeFile(join(root, 'keep.bin'), Buffer.alloc(10));
    await mkdir(join(root, 'rustcAbC123')); await writeFile(join(root, 'rustcAbC123', 'tmp.o'), Buffer.alloc(100));
    await writeFile(join(root, 'gone.lock'), Buffer.alloc(5));
    const { readdir: realReaddir, lstat: realLstat } = await import('node:fs/promises');
    const total = await directorySize(root, {
      readdirFn: async (path, options) => {
        if (path.endsWith('rustcAbC123')) throw Object.assign(new Error('gone'), { code: 'ENOENT' });
        return realReaddir(path, options);
      },
      lstatFn: async (path) => {
        if (path.endsWith('gone.lock')) throw Object.assign(new Error('gone'), { code: 'ENOENT' });
        return realLstat(path);
      }
    });
    assert.equal(total, 10);
    await assert.rejects(directorySize(root, { readdirFn: async () => { throw Object.assign(new Error('denied'), { code: 'EACCES' }); } }), { code: 'EACCES' });
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('GC uses advisory sizes for active trees and never persists them as measured', async () => {
  const item = await fixture();
  try {
    const workspace = await item.state.workspace(item.workspace);
    await writeFile(join(workspace.layout.target, 'w.bin'), Buffer.alloc(64));
    await item.state.markRunning(item.workspace, { id: 'job-active', command: 'cargo build' });
    const { mkdir } = await import('node:fs/promises');
    // A rustc temp dir inside the active tree must never be walked.
    await mkdir(join(workspace.layout.target, 'rustcTmp1'));
    const result = await item.state.gc({ capBytes: 1_000_000 });
    assert.equal(result.advisorySizedTrees, 1);
    assert.equal(result.beforeBytes, 0);
    assert.equal((await item.state.workspace(item.workspace)).state.lastMeasuredBytes ?? null, null);
    await item.state.finish(item.workspace, 'job-active', { status: 'SUCCEEDED' });
  } finally { await item.close(); }
});

async function leafFixture({ target = false, t = false, newer = null }) {
  const item = await fixture();
  const key = thinWorkspaceKey(await realpath(item.workspace));
  const generation = join(item.root, 'rightkit-build-control', 'ws', key, 'g', '001');
  const seed = async (leaf, ageSeconds) => {
    const fingerprint = join(generation, leaf, 'x86_64-pc-windows-msvc', 'release', '.fingerprint', 'crate-1');
    await mkdir(fingerprint, { recursive: true });
    await writeFile(join(fingerprint, 'f'), 'x');
    const when = new Date(Date.now() - ageSeconds * 1000);
    for (const path of [fingerprint, join(fingerprint, 'f'), join(generation, leaf, 'x86_64-pc-windows-msvc', 'release', '.fingerprint')]) await utimes(path, when, when);
  };
  if (target) await seed('target', newer === 'target' ? 10 : 1000);
  if (t) await seed('t', newer === 't' ? 10 : 1000);
  return { item, generation, key };
}

test('leaf selection: only target populated stays on target and reports no orphan', async () => {
  const { item } = await leafFixture({ target: true });
  try {
    const workspace = await item.state.workspace(item.workspace);
    assert.equal(workspace.layout.leaf, 'target');
    assert.match(workspace.layout.leafReason, /only-target-nonempty/);
    assert.equal(workspace.layout.orphanLeaf, null);
    await assert.rejects(stat(join(workspace.layout.root, 't')), { code: 'ENOENT' });
  } finally { await item.close(); }
});

test('leaf selection: only t populated keeps t, never creates target', async () => {
  const { item } = await leafFixture({ t: true });
  try {
    const workspace = await item.state.workspace(item.workspace);
    assert.equal(workspace.layout.leaf, 't');
    assert.equal(workspace.layout.target, join(workspace.layout.root, 't'));
    assert.match(workspace.layout.leafReason, /only-t-nonempty/);
    await assert.rejects(stat(join(workspace.layout.root, 'target')), { code: 'ENOENT' });
  } finally { await item.close(); }
});

test('leaf selection: both populated picks newer activity and reports orphan without deleting it', async () => {
  for (const winner of ['target', 't']) {
    const { item } = await leafFixture({ target: true, t: true, newer: winner });
    try {
      const workspace = await item.state.workspace(item.workspace);
      assert.equal(workspace.layout.leaf, winner);
      assert.match(workspace.layout.leafReason, /both-nonempty-newer-activity/);
      const other = winner === 't' ? 'target' : 't';
      assert.deepEqual({ ...workspace.layout.orphanLeaf }, { leaf: other, path: join(workspace.layout.root, other), sizeBytes: null });
      await assert.doesNotReject(stat(join(workspace.layout.root, other)));
    } finally { await item.close(); }
  }
});

test('leaf selection: neither exists defaults to target; persisted value wins over later disk state and survives rotation defaults', async () => {
  const { item, generation } = await leafFixture({});
  try {
    const first = await item.state.workspace(item.workspace);
    assert.equal(first.layout.leaf, 'target');
    // A populated `t` appearing later must not flip a persisted choice.
    await mkdir(join(generation, 't', 'x'), { recursive: true });
    await writeFile(join(generation, 't', 'x', 'f'), 'y');
    const again = await item.state.workspace(item.workspace);
    assert.equal(again.layout.leaf, 'target');
    assert.equal(again.layout.orphanLeaf.leaf, 't');
    assert.match(again.layout.leafReason, /^persisted/);
    assert.equal(JSON.parse(await readFile(first.statePath, 'utf8')).leaves['001'].leaf, 'target');
    const rotated = await item.state.rotate(item.workspace, 'test');
    assert.equal(rotated.layout.leaf, 'target');
    assert.equal(JSON.parse(await readFile(first.statePath, 'utf8')).leaves['002'].leaf, 'target');
  } finally { await item.close(); }
});

test('leaf selection: a persisted t is honoured even when target is also populated', async () => {
  const { item, generation, key } = await leafFixture({ target: true, t: true, newer: 'target' });
  try {
    await atomicJson(join(item.root, 'rightkit-build-control', 'ws', key, 'state.json'), { schemaVersion: 1, currentGeneration: 1, active: null, residents: [], leaves: { '001': { leaf: 't', reason: 'operator' } }, updatedAt: '2026-08-22T00:00:00.000Z' });
    const workspace = await item.state.workspace(item.workspace);
    assert.equal(workspace.layout.leaf, 't');
    assert.equal(workspace.layout.target, join(generation, 't'));
  } finally { await item.close(); }
});

async function warmWorkspace(item, name, bytes = 64) {
  const root = join(item.root, name); await mkdir(root, { recursive: true });
  const workspace = await item.state.workspace(root);
  await writeFile(join(workspace.layout.target, 'w.bin'), Buffer.alloc(bytes));
  const id = `job-${name.replace(/[^A-Za-z0-9]/g, '-')}`;
  await item.state.markRunning(root, { id, command: 'cargo build' }); await item.state.finish(root, id, { status: 'SUCCEEDED' });
  return { root, workspace };
}
const evictVia = (item, seen = []) => async (candidate) => { seen.push(candidate); return item.state.evictIdleWorkspace(candidate.canonicalRoot, { sizeBytes: candidate.sizeBytes, reason: candidate.reason }); };

test('GC protects recently used warm trees and reports the cap as unenforceable instead', async () => {
  const item = await fixture();
  try {
    await warmWorkspace(item, 'a'); await warmWorkspace(item, 'b');
    const seen = [];
    const result = await item.state.gc({ capBytes: 64, evictIdle: evictVia(item, seen) });
    assert.equal(seen.length, 0); assert.equal(result.evicted.length, 0);
    assert.ok(result.unenforceable);
  } finally { await item.close(); }
});

test('GC never rotates empty generations', async () => {
  const item = await fixture();
  try {
    await item.state.workspace(item.workspace);
    const seen = [];
    await item.state.gc({ capBytes: 1, evictIdle: evictVia(item, seen), recentMs: 0, staleMs: -1 });
    assert.equal(seen.length, 0);
  } finally { await item.close(); }
});

test('GC reclaims a workspace whose source root was deleted', async () => {
  const item = await fixture();
  try {
    const { root, workspace } = await warmWorkspace(item, 'gone');
    await rm(root, { recursive: true, force: true });
    const result = await item.state.gc({ capBytes: 1_000_000 });
    assert.deepEqual(result.reclaimed.deadRoots.map((entry) => entry.workspaceKey), [workspace.key]);
    await assert.rejects(stat(workspace.directory));
  } finally { await item.close(); }
});

test('GC reclaims the unpersisted sibling leaf but never the persisted one', async () => {
  const item = await fixture();
  try {
    const { workspace } = await warmWorkspace(item, 'leafy');
    assert.equal(workspace.layout.leaf, 'target');
    const orphan = join(workspace.layout.root, 't'); await mkdir(orphan); await writeFile(join(orphan, 'old.bin'), Buffer.alloc(32));
    const result = await item.state.gc({ capBytes: 1_000_000 });
    assert.equal(result.reclaimed.orphanLeaves.length, 1); assert.equal(result.reclaimed.orphanLeaves[0].sizeBytes, 32);
    await assert.rejects(stat(orphan));
    await assert.doesNotReject(stat(join(workspace.layout.target, 'w.bin')));
  } finally { await item.close(); }
});

test('GC breaches recency protection only under disk-floor pressure', async () => {
  const item = await fixture();
  try {
    await warmWorkspace(item, 'a'); await warmWorkspace(item, 'b');
    const seen = [];
    const result = await item.state.gc({ capBytes: 1_000_000, evictIdle: evictVia(item, seen), freeBytes: async () => 10, freeFloorBytes: 100, graceMs: 0 });
    assert.ok(result.effectiveCapBytes < result.beforeBytes);
    assert.ok(result.evicted.length >= 1);
    assert.ok(seen.every((candidate) => candidate.reason === 'disk-floor-eviction'));
  } finally { await item.close(); }
});

test('GC never evicts a just-used generation under disk-floor pressure (grace window)', async () => {
  const item = await fixture();
  try {
    // A build that just finished is still being consumed (right-release dev bundles after cargo exits).
    await warmWorkspace(item, 'fresh');
    const seen = [];
    const result = await item.state.gc({ capBytes: 1_000_000, evictIdle: evictVia(item, seen), freeBytes: async () => 10, freeFloorBytes: 100 });
    assert.equal(result.evicted.length, 0);
    assert.equal(seen.length, 0);
  } finally { await item.close(); }
});

test('GC retires stale idle trees regardless of cap', async () => {
  const item = await fixture({ advanceClock: true });
  try {
    await warmWorkspace(item, 'old');
    const seen = [];
    const result = await item.state.gc({ capBytes: 1_000_000, evictIdle: evictVia(item, seen), staleMs: 500 });
    assert.equal(result.evicted.length, 1); assert.equal(seen[0].reason, 'stale-idle-eviction');
  } finally { await item.close(); }
});

test('a repository cache root adopts its single warm Cargo workspace instead of starting cold', async () => {
  const item = await fixture();
  try {
    const repo = join(item.root, 'repo'); await mkdir(join(repo, '.git'), { recursive: true });
    const { workspace: sub } = await warmWorkspace(item, join('repo', 'app'));
    const adopted = await item.state.workspace(repo);
    assert.equal(adopted.key, sub.key);
    assert.equal(adopted.layout.target, sub.layout.target);
    const decision = JSON.parse(await readFile(join(item.root, 'rightkit-build-control', 'ws', thinWorkspaceKey(await realpath(repo)), 'adoption.json'), 'utf8'));
    assert.equal(decision.result, 'adopted');
    assert.equal((await item.state.workspace(repo)).key, sub.key, 'decision is stable');
  } finally { await item.close(); }
});

test('adoption refuses ambiguous candidates and nested repositories', async () => {
  const item = await fixture();
  try {
    const repo = join(item.root, 'repo'); await mkdir(join(repo, '.git'), { recursive: true });
    await warmWorkspace(item, join('repo', 'one')); await warmWorkspace(item, join('repo', 'two'));
    const own = await item.state.workspace(repo);
    assert.equal(own.key, thinWorkspaceKey(await realpath(repo)));
    const outer = join(item.root, 'outer'); await mkdir(join(outer, '.git'), { recursive: true });
    await mkdir(join(outer, 'nested', '.git'), { recursive: true });
    await warmWorkspace(item, join('outer', 'nested', 'app'));
    assert.equal((await item.state.workspace(outer)).key, thinWorkspaceKey(await realpath(outer)));
  } finally { await item.close(); }
});

test('adoption prefers a much larger warm workspace over a thin own tree and re-decides after rotation', async () => {
  const item = await fixture();
  try {
    const repo = join(item.root, 'repo'); await mkdir(join(repo, '.git'), { recursive: true });
    // Own cache root already holds a small tree (e.g. a lone cargo check) before adoption is decided.
    const own = await item.state.workspace(repo);
    assert.equal(own.key, thinWorkspaceKey(await realpath(repo)), 'no candidates yet: stays on its own key');
    await writeFile(join(own.layout.target, 'check.bin'), Buffer.alloc(8));
    const { workspace: sub } = await warmWorkspace(item, join('repo', 'engine'), 4096);
    await item.state.evictIdleWorkspace(repo, { reason: 'cache-budget-eviction' });
    await writeFile(join((await item.state.workspace(repo)).layout.target, 'check.bin'), Buffer.alloc(8)).catch(() => {});
    const decided = await item.state.workspace(repo);
    assert.equal(decided.key, sub.key, 'a 4 KiB warm tree beats an 8-byte own tree');
  } finally { await item.close(); }
});
