import test from 'node:test';
import assert from 'node:assert/strict';
import { lstat, mkdtemp, mkdir, readFile, rm, writeFile, symlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { MaintenanceWorker } from './maintenance-worker.mjs';

const namespace = 'a'.repeat(64);
const newerGeneration = 2;

function stateStore(records = new Map()) {
  const calls = []; let serial = 0;
  return {
    calls,
    async getNamespace({ namespace: value }) { calls.push(['get', value]); return records.get(value) ?? null; },
    async claimMaintenanceLease({ namespace: value, generation }) { calls.push(['claim', value, generation]); const record = records.get(value); return record?.generation === generation ? { leaseId: `lease-${++serial}` } : null; },
    async releaseMaintenanceLease(value) { calls.push(['release', value]); },
    async markNamespaceTrashed({ namespace: value, generation, trashId }) { calls.push(['trashed', value, generation, trashId]); records.set(value, { ...records.get(value), trashId, state: 'TRASHED' }); },
    async markTrashDeleted({ namespace: value, generation, trashId }) { calls.push(['deleted', value, generation, trashId]); records.set(value, { ...records.get(value), trashId, state: 'DELETED' }); }
  };
}

async function fixture(record = { namespace, generation: 1, state: 'RELEASED', terminalAt: '2020-01-01T00:00:00.000Z' }) {
  const root = await mkdtemp(path.join(tmpdir(), 'maintenance-worker-')); const workspacesRoot = path.join(root, 'workspaces'); const trashRoot = path.join(root, 'trash'); await mkdir(path.join(workspacesRoot, namespace), { recursive: true });
  return { root, workspacesRoot, trashRoot, store: stateStore(new Map([[namespace, record]])) };
}

test('retention uses bounded async inventory & atomically moves a known terminal namespace to trash', async () => {
  const value = await fixture();
  try {
    await writeFile(path.join(value.workspacesRoot, namespace, 'warm'), 'keep');
    const worker = new MaintenanceWorker({ ...value, stateStore: value.store, maxBatchEntries: 1 });
    const result = await worker.retentionBatch({ expiredBefore: '2021-01-01T00:00:00.000Z', maxNamespaces: 1, inventoryLimit: 1 });
    assert.equal(result.moved.length, 1); const [{ trashId, path: trashPath }] = result.moved;
    assert.equal(await readFile(path.join(trashPath, 'warm'), 'utf8'), 'keep');
    assert.deepEqual((await worker.orphanDryRun()).orphans, []);
    assert.ok(value.store.calls.some(([kind, , , id]) => kind === 'trashed' && id === trashId));
  } finally { await rm(value.root, { recursive: true, force: true }); }
});

test('generation changes after claim prevent a stale worker from renaming a live namespace', async () => {
  const value = await fixture();
  try {
    const originalClaim = value.store.claimMaintenanceLease; const originalGet = value.store.getNamespace; let generationAdvanced = false;
    value.store.claimMaintenanceLease = async (input) => { const claim = await originalClaim(input); generationAdvanced = true; return claim; };
    // This models a new active generation appearing between claim & revalidation.
    value.store.getNamespace = async (input) => generationAdvanced ? { namespace, generation: newerGeneration, state: 'ACTIVE' } : originalGet(input);
    const worker = new MaintenanceWorker({ ...value, stateStore: value.store });
    const result = await worker.moveToTrash({ namespace, generation: 1 });
    assert.equal(result.status, 'SKIPPED_STALE_GENERATION');
    assert.equal((await lstat(path.join(value.workspacesRoot, namespace))).isDirectory(), true);
  } finally { await rm(value.root, { recursive: true, force: true }); }
});

test('same-generation activation after claim prevents namespace rename', async () => {
  const value = await fixture();
  try {
    const originalClaim = value.store.claimMaintenanceLease; const originalGet = value.store.getNamespace; let activated = false;
    value.store.claimMaintenanceLease = async (input) => { const claim = await originalClaim(input); activated = true; return claim; };
    value.store.getNamespace = async (input) => activated ? { namespace, generation: 1, state: 'ACTIVE' } : originalGet(input);
    const worker = new MaintenanceWorker({ ...value, stateStore: value.store });
    const result = await worker.moveToTrash({ namespace, generation: 1, expiredBefore: '2021-01-01T00:00:00.000Z' });
    assert.equal(result.status, 'SKIPPED_NOT_RETENTION_ELIGIBLE');
    assert.equal((await lstat(path.join(value.workspacesRoot, namespace))).isDirectory(), true);
    assert.equal(value.store.calls.some(([kind]) => kind === 'trashed'), false);
  } finally { await rm(value.root, { recursive: true, force: true }); }
});

test('orphan inspection is dry-run only & never mutates unknown namespace roots', async () => {
  const value = await fixture();
  try {
    const orphan = 'b'.repeat(64); await mkdir(path.join(value.workspacesRoot, orphan));
    const worker = new MaintenanceWorker({ ...value, stateStore: value.store }); const result = await worker.orphanDryRun();
    assert.equal(result.mode, 'dry-run'); assert.deepEqual(result.orphans.map((item) => item.namespace), [orphan]);
    await readFile(path.join(value.workspacesRoot, orphan)).catch((error) => assert.equal(error.code, 'EISDIR'));
    assert.equal(value.store.calls.some(([kind]) => kind === 'claim'), false);
  } finally { await rm(value.root, { recursive: true, force: true }); }
});

test('trash deletion performs bounded batches & has no admission dependency', async () => {
  const value = await fixture();
  try {
    const worker = new MaintenanceWorker({ ...value, stateStore: value.store, maxBatchEntries: 2 }); const moved = await worker.moveToTrash({ namespace, generation: 1 });
    await writeFile(path.join(moved.path, 'one'), '1'); await writeFile(path.join(moved.path, 'two'), '2');
    let batch; do { batch = await worker.deleteTrashBatch({ namespace, generation: 1, trashId: moved.trashId, maxEntries: 2 }); assert.ok(batch.deletedEntries <= 2); } while (!batch.complete);
    assert.equal(value.store.calls.some(([kind]) => kind === 'admission'), false);
    assert.ok(value.store.calls.some(([kind]) => kind === 'deleted'));
  } finally { await rm(value.root, { recursive: true, force: true }); }
});

test('retention resumes past truncated inventory with cursor', async () => {
  const value = await fixture();
  const namespaces = [namespace, 'b'.repeat(64), 'c'.repeat(64)];
  try {
    for (const name of namespaces.slice(1)) { await mkdir(path.join(value.workspacesRoot, name)); value.store.calls; }
    const records = new Map(namespaces.map((name) => [name, { namespace: name, generation: 1, state: 'RELEASED', terminalAt: '2020-01-01T00:00:00.000Z' }]));
    Object.assign(value, { store: stateStore(records) });
    const worker = new MaintenanceWorker({ ...value, stateStore: value.store, maxBatchEntries: 1 });
    let cursor = null; const moved = [];
    do { const batch = await worker.retentionBatch({ expiredBefore: '2021-01-01T00:00:00.000Z', maxNamespaces: 1, inventoryLimit: 1, inventoryCursor: cursor }); moved.push(...batch.moved); cursor = batch.nextCursor; } while (cursor);
    assert.equal(moved.length, 3);
  } finally { await rm(value.root, { recursive: true, force: true }); }
});

test('inventory metadata work never exceeds requested page size', async () => {
  const value = await fixture();
  try {
    for (let index = 0; index < 100; index += 1) await mkdir(path.join(value.workspacesRoot, `${index.toString(16).padStart(64, '0')}`), { recursive: true });
    let lstatCalls = 0;
    const worker = new MaintenanceWorker({ ...value, stateStore: value.store, operations: {
      lstat: async (...args) => { lstatCalls += 1; return lstat(...args); }
    } });
    const first = await worker.inventory({ limit: 1 });
    assert.equal(first.entries.length, 1); assert.equal(lstatCalls, 1); assert.ok(first.nextCursor);
    const second = await worker.inventory({ limit: 1, cursor: first.nextCursor });
    assert.equal(second.entries.length, 1); assert.equal(lstatCalls, 2);
  } finally { await rm(value.root, { recursive: true, force: true }); }
});

test('trash deletion cursor survives worker restart for large trash trees', async () => {
  const value = await fixture();
  try {
    const worker = new MaintenanceWorker({ ...value, stateStore: value.store, maxBatchEntries: 4 }); const moved = await worker.moveToTrash({ namespace, generation: 1 });
    for (let index = 0; index < 20; index += 1) await writeFile(path.join(moved.path, String(index).padStart(2, '0')), 'x');
    let cursor = null; let total = 0; let batch;
    do { const restarted = new MaintenanceWorker({ ...value, stateStore: value.store, maxBatchEntries: 4 }); batch = await restarted.deleteTrashBatch({ namespace, generation: 1, trashId: moved.trashId, maxEntries: 4, cursor }); total += batch.deletedEntries; cursor = batch.nextCursor; } while (!batch.complete);
    assert.equal(total, 20); assert.equal(cursor, null);
  } finally { await rm(value.root, { recursive: true, force: true }); }
});

test('wide trash deletion scans at most its batch bound & resumes safely after every restart', async () => {
  const value = await fixture();
  try {
    const mover = new MaintenanceWorker({ ...value, stateStore: value.store, maxBatchEntries: 1 }); const moved = await mover.moveToTrash({ namespace, generation: 1 });
    for (let index = 0; index < 100; index += 1) await writeFile(path.join(moved.path, `wide-${String(index).padStart(3, '0')}`), 'x');
    let cursor = null; let deleted = 0; let batches = 0;
    do {
      let lstatCalls = 0;
      const worker = new MaintenanceWorker({ ...value, stateStore: value.store, maxBatchEntries: 1, operations: {
        lstat: async (...args) => { lstatCalls += 1; return lstat(...args); }
      } });
      const result = await worker.deleteTrashBatch({ namespace, generation: 1, trashId: moved.trashId, maxEntries: 1, cursor });
      assert.ok(result.scannedEntries <= 1, `scanned ${result.scannedEntries}`);
      assert.ok(result.deletedEntries <= 1, `deleted ${result.deletedEntries}`);
      // Old candidate-list traversal called lstat once per remaining sibling.
      assert.ok(lstatCalls <= 4, `wide scan made ${lstatCalls} metadata calls`);
      cursor = result.nextCursor; deleted += result.deletedEntries; batches += 1;
      if (result.complete) break;
      assert.ok(batches < 250, 'bounded traversal must make forward progress');
    } while (true);
    assert.equal(deleted, 100); assert.equal(cursor, null);
  } finally { await rm(value.root, { recursive: true, force: true }); }
});

test('deep trash traversal keeps a bounded stack, rejects excess depth, & completes after restart', async () => {
  const value = await fixture();
  try {
    const mover = new MaintenanceWorker({ ...value, stateStore: value.store, maxBatchEntries: 1, maxDepth: 8 }); const moved = await mover.moveToTrash({ namespace, generation: 1 });
    let leaf = moved.path;
    for (let index = 0; index < 6; index += 1) { leaf = path.join(leaf, `d${index}`); await mkdir(leaf); }
    await writeFile(path.join(leaf, 'leaf'), 'x');
    let cursor = null; let complete = false; let batches = 0;
    while (!complete) {
      const restarted = new MaintenanceWorker({ ...value, stateStore: value.store, maxBatchEntries: 1, maxDepth: 8 });
      const result = await restarted.deleteTrashBatch({ namespace, generation: 1, trashId: moved.trashId, maxEntries: 1, cursor });
      assert.ok(result.scannedEntries <= 1); assert.ok(result.nextCursor === null || result.nextCursor.length < 512);
      cursor = result.nextCursor; complete = result.complete; batches += 1;
      assert.ok(batches < 32, 'deep traversal must make forward progress');
    }

    const second = await fixture();
    try {
      const shallow = new MaintenanceWorker({ ...second, stateStore: second.store, maxDepth: 1 }); const shallowMoved = await shallow.moveToTrash({ namespace, generation: 1 });
      await mkdir(path.join(shallowMoved.path, 'one', 'two'), { recursive: true });
      await assert.rejects(() => shallow.deleteTrashBatch({ namespace, generation: 1, trashId: shallowMoved.trashId, maxEntries: 8 }), /depth bound/);
    } finally { await rm(second.root, { recursive: true, force: true }); }
  } finally { await rm(value.root, { recursive: true, force: true }); }
});

test('retention includes v2 successful & timed-out namespaces', async () => {
  for (const state of ['SUCCEEDED', 'TIMED_OUT']) {
    const value = await fixture({ namespace, generation: 1, state, terminalAt: '2020-01-01T00:00:00.000Z' });
    try {
      const worker = new MaintenanceWorker({ ...value, stateStore: value.store });
      const result = await worker.retentionBatch({ expiredBefore: '2021-01-01T00:00:00.000Z' });
      assert.equal(result.moved.length, 1, state);
    } finally { await rm(value.root, { recursive: true, force: true }); }
  }
});

test('pending maintenance queue keeps old-generation trash deletable after activation', async () => {
  const value = await fixture();
  try {
    const mover = new MaintenanceWorker({ ...value, stateStore: value.store, maxBatchEntries: 1 }); const moved = await mover.moveToTrash({ namespace, generation: 1 });
    await writeFile(path.join(moved.path, 'old-generation-file'), 'x');
    value.store.getNamespace = async () => ({ namespace, generation: 2, state: 'ACTIVE', trashId: null });
    value.store.maintenanceEntry = async ({ namespace: valueNamespace, generation, trashId }) => valueNamespace === namespace && generation === 1 && trashId === moved.trashId ? { state: 'PENDING', cursor: null } : null;
    let cursor = null; let result;
    do {
      result = await new MaintenanceWorker({ ...value, stateStore: value.store, maxBatchEntries: 1 }).deleteTrashBatch({ namespace, generation: 1, trashId: moved.trashId, maxEntries: 1, cursor });
      cursor = result.nextCursor;
    } while (!result.complete);
    assert.ok(value.store.calls.some(([kind]) => kind === 'deleted'));
  } finally { await rm(value.root, { recursive: true, force: true }); }
});

test('trash intent reconciliation handles crash boundary states safely', async () => {
  const value = await fixture();
  try {
    const pending = { namespace, generation: 1, trashId: `${namespace}-1-intent`, sourcePath: path.join(value.workspacesRoot, namespace), targetPath: path.join(value.trashRoot, `${namespace}-1-intent`), leaseId: 'lease-1' };
    const calls = []; const store = { ...value.store, prepareNamespaceTrash: async () => {}, commitNamespaceTrash: async () => calls.push('commit'), clearNamespaceTrashIntent: async () => calls.push('clear'), pendingTrashIntents: async () => [pending] };
    const worker = new MaintenanceWorker({ ...value, stateStore: store }); await mkdir(value.trashRoot, { recursive: true });
    await worker.reconcileTrashIntent(pending); assert.deepEqual(calls, ['clear']);
    await rm(pending.sourcePath, { recursive: true, force: true }); await mkdir(pending.targetPath, { recursive: true }); calls.length = 0; await worker.reconcileTrashIntent(pending); assert.deepEqual(calls, ['commit']);
    await mkdir(pending.sourcePath, { recursive: true }); await assert.rejects(() => worker.reconcileTrashIntent(pending), { code: 'RK_TRASH_AMBIGUOUS' });
    await rm(pending.sourcePath, { recursive: true, force: true }); await rm(pending.targetPath, { recursive: true, force: true }); await assert.rejects(() => worker.reconcileTrashIntent(pending), { code: 'RK_TRASH_AMBIGUOUS' });
    await assert.rejects(() => worker.reconcileTrashIntent({ ...pending, targetPath: path.join(value.trashRoot, 'wrong') }), { code: 'RK_TRASH_AMBIGUOUS' });
    await symlink(value.workspacesRoot, pending.targetPath); await assert.rejects(() => worker.reconcileTrashIntent(pending), { code: 'RK_TRASH_AMBIGUOUS' });
  } finally { await rm(value.root, { recursive: true, force: true }); }
});
