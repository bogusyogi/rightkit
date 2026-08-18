import assert from 'node:assert/strict';
import { mkdtemp, readdir, readFile, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { DurableStateStore } from './state-store.mjs';

const database = async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'rightkit-state-store-'));
  return { root, databasePath: path.join(root, 'state', 'broker.sqlite') };
};

test('WAL transaction records terminal state transition & exactly one idempotent receipt', async () => {
  const fixture = await database();
  try {
    const store = await DurableStateStore.open(fixture);
    store.create({ id: 'request-1', data: { workspace: 'rightkit' }, occurredAt: '2026-08-14T00:00:00.000Z' });
    store.transition({ id: 'request-1', from: 'SUBMITTED', to: 'CLASSIFIED', data: { capability: 'BUILD' }, occurredAt: '2026-08-14T00:00:01.000Z' });
    store.transition({ id: 'request-1', from: 'CLASSIFIED', to: 'QUEUED', occurredAt: '2026-08-14T00:00:02.000Z' });
    store.transition({ id: 'request-1', from: 'QUEUED', to: 'ADMITTED', occurredAt: '2026-08-14T00:00:03.000Z' });
    store.transition({ id: 'request-1', from: 'ADMITTED', to: 'STARTING', occurredAt: '2026-08-14T00:00:04.000Z' });
    store.transition({ id: 'request-1', from: 'STARTING', to: 'RUNNING', occurredAt: '2026-08-14T00:00:05.000Z' });
    store.transition({ id: 'request-1', from: 'RUNNING', to: 'REAPING', occurredAt: '2026-08-14T00:00:06.000Z' });
    const completed = store.transition({ id: 'request-1', from: 'REAPING', to: 'SUCCEEDED', data: { exitCode: 0 }, receipt: { id: 'request-1', state: 'SUCCEEDED', exitCode: 0 }, occurredAt: '2026-08-14T00:00:07.000Z' });
    const retry = store.transition({ id: 'request-1', from: 'REAPING', to: 'SUCCEEDED', receipt: { id: 'request-1', state: 'SUCCEEDED', exitCode: 99 }, occurredAt: '2026-08-14T00:00:08.000Z' });
    assert.equal(completed.idempotent, false);
    assert.equal(retry.idempotent, true);
    assert.deepEqual(retry.receipt, { id: 'request-1', state: 'SUCCEEDED', exitCode: 0 });
    assert.equal(store.entry('request-1').state, 'SUCCEEDED');
    store.close();
    const reopened = await DurableStateStore.open(fixture);
    assert.deepEqual(reopened.receipt('request-1').receipt, { id: 'request-1', state: 'SUCCEEDED', exitCode: 0 });
    assert.equal((await readdir(path.join(fixture.root, 'state'))).includes('broker.sqlite-wal'), true);
    reopened.close();
  } finally { await rm(fixture.root, { recursive: true, force: true }); }
});

test('restart recovery terminalizes queued work but preserves active ownership until explicit reaping recovery', async () => {
  const fixture = await database();
  try {
    const store = await DurableStateStore.open(fixture);
    store.create({ id: 'queued', data: { reservationBytes: 10 }, occurredAt: '2026-08-14T00:00:00.000Z' });
    store.create({ id: 'running', data: { reservationBytes: 20, ownership: { pid: 42, processGroupId: 42, platform: 'posix', identity: 'process-start-42' } }, occurredAt: '2026-08-14T00:00:00.000Z' });
    store.transition({ id: 'running', from: 'SUBMITTED', to: 'CLASSIFIED', occurredAt: '2026-08-14T00:00:01.000Z' });
    store.transition({ id: 'running', from: 'CLASSIFIED', to: 'QUEUED', occurredAt: '2026-08-14T00:00:02.000Z' });
    store.transition({ id: 'running', from: 'QUEUED', to: 'ADMITTED', occurredAt: '2026-08-14T00:00:03.000Z' });
    store.transition({ id: 'running', from: 'ADMITTED', to: 'STARTING', occurredAt: '2026-08-14T00:00:04.000Z' });
    store.transition({ id: 'running', from: 'STARTING', to: 'RUNNING', occurredAt: '2026-08-14T00:00:05.000Z' });
    store.close();
    const restarted = await DurableStateStore.open(fixture);
    const recovered = restarted.recoverOpenRequests({ occurredAt: '2026-08-14T00:01:00.000Z' });
    assert.deepEqual(recovered.map(({ entry }) => entry.id), ['queued']);
    assert.deepEqual(restarted.entries({ states: ['CANCELLED'] }).map(({ id, state, data }) => ({ id, state, reason: data.cancellationReason })), [
      { id: 'queued', state: 'CANCELLED', reason: 'broker-restart' },
    ]);
    assert.equal(restarted.recoverOpenRequests().length, 0);
    assert.equal(restarted.receipt('queued').receipt.recoveredFrom, 'SUBMITTED');
    assert.equal(restarted.entry('running').state, 'RUNNING');
    assert.equal(restarted.entry('running').data.ownership.identity, 'process-start-42');
    assert.equal(restarted.receipt('running'), null);
    const reaped = restarted.recoverOpenRequest({ id: 'running', data: { cancellationReason: 'broker-restart', reaped: true }, occurredAt: '2026-08-14T00:02:00.000Z' });
    assert.equal(reaped.entry.state, 'CANCELLED');
    assert.equal(restarted.receipt('running').receipt.recoveredFrom, 'RUNNING');
    restarted.close();
    const bytes = await readFile(fixture.databasePath);
    assert.ok(bytes.length > 0);
  } finally { await rm(fixture.root, { recursive: true, force: true }); }
});

test('namespace maintenance lease is generation-CAS, trash transitions are idempotent, & new generations clear trash state', async () => {
  const fixture = await database(); const namespace = 'a'.repeat(64); const trashId = `${namespace}-1-maintenance`;
  try {
    const store = await DurableStateStore.open(fixture);
    store.upsertNamespace({ namespace, generation: 1, state: 'RELEASED', terminalAt: '2026-08-14T00:00:00.000Z', lruAt: '2026-08-14T00:00:00.000Z' });
    assert.deepEqual(store.getNamespace({ namespace }), { namespace, generation: 1, state: 'RELEASED', terminalAt: '2026-08-14T00:00:00.000Z', lruAt: '2026-08-14T00:00:00.000Z', trashId: null, trashedAt: null, deletedAt: null, repositoryIdentity: null });
    const lease = store.claimMaintenanceLease({ namespace, generation: 1 });
    assert.ok(lease?.leaseId);
    assert.equal(store.claimMaintenanceLease({ namespace, generation: 1 }), null);
    assert.throws(() => store.upsertNamespace({ namespace, generation: 1, state: 'ACTIVE' }), /active namespace cannot replace a maintenance lease/);
    assert.throws(() => store.markNamespaceTrashed({ namespace, generation: 0, leaseId: lease.leaseId, trashId, trashedAt: '2026-08-14T00:01:00.000Z' }), /stale namespace generation/);
    assert.throws(() => store.markNamespaceTrashed({ namespace, generation: 1, leaseId: 'wrong', trashId, trashedAt: '2026-08-14T00:01:00.000Z' }), /maintenance lease is not current/);
    store.markNamespaceTrashed({ namespace, generation: 1, leaseId: lease.leaseId, trashId, trashedAt: '2026-08-14T00:01:00.000Z' });
    store.releaseMaintenanceLease({ namespace, generation: 1, leaseId: lease.leaseId });
    store.markNamespaceTrashed({ namespace, generation: 1, leaseId: 'already-released', trashId, trashedAt: '2026-08-14T00:01:00.000Z' });
    assert.equal(store.getNamespace({ namespace }).state, 'TRASHED');
    store.markTrashDeleted({ namespace, generation: 1, trashId, deletedAt: '2026-08-14T00:02:00.000Z' });
    store.markTrashDeleted({ namespace, generation: 1, trashId, deletedAt: '2026-08-14T00:02:00.000Z' });
    assert.equal(store.getNamespace({ namespace }).state, 'DELETED');
    assert.throws(() => store.upsertNamespace({ namespace, generation: 1, state: 'ACTIVE' }), /trashed namespace requires a newer generation/);
    store.upsertNamespace({ namespace, generation: 2, state: 'ACTIVE' });
    assert.deepEqual(store.getNamespace({ namespace }), { namespace, generation: 2, state: 'ACTIVE', terminalAt: null, lruAt: null, trashId: null, trashedAt: null, deletedAt: null, repositoryIdentity: null });
    assert.equal(store.claimMaintenanceLease({ namespace, generation: 1 }), null);
    assert.throws(() => store.upsertNamespace({ namespace, generation: 1, state: 'RELEASED' }), /stale namespace generation/);
    store.close();
  } finally { await rm(fixture.root, { recursive: true, force: true }); }
});

test('invalid or mismatched lifecycle mutations leave durable state untouched', async () => {
  const fixture = await database();
  try {
    const store = await DurableStateStore.open(fixture);
    store.create({ id: 'request-2' });
    assert.throws(() => store.transition({ id: 'request-2', from: 'SUBMITTED', to: 'SUCCEEDED', receipt: null }), /terminal transition requires a receipt/);
    assert.throws(() => store.transition({ id: 'request-2', from: 'SUBMITTED', to: 'RUNNING' }), /invalid lifecycle transition/);
    assert.throws(() => store.transition({ id: 'request-2', from: 'QUEUED', to: 'RUNNING' }), /invalid lifecycle transition/);
    assert.equal(store.entry('request-2').state, 'SUBMITTED');
    assert.equal(store.receipt('request-2'), null);
    store.close();
  } finally { await rm(fixture.root, { recursive: true, force: true }); }
});

test('v1 migration drains, maps terminals, detects unknowns & drift', async () => {
  const f = await database(); const ns = 'b'.repeat(64); const legacy = path.join(f.root, 'legacy.json');
  try {
    await (await import('node:fs/promises')).writeFile(legacy, JSON.stringify({ entries: [{ id: 'live', state: 'RUNNING' }] }));
    await assert.rejects(() => DurableStateStore.open({ databasePath: f.databasePath, legacyStatePath: legacy }), /RK_V1_DRAIN_REQUIRED|drain/);
    await (await import('node:fs/promises')).writeFile(legacy, JSON.stringify({ entries: [{ id: 'r', state: 'RELEASED', roots: { namespace: ns }, lruAt: '2026-01-02T00:00:00Z' }, { id: 'f', state: 'FAILED' }, { id: 'c', state: 'CANCELLED' }] }));
    const s = await DurableStateStore.open({ databasePath: f.databasePath, legacyStatePath: legacy }); assert.equal(s.entry('r').state, 'SUCCEEDED'); assert.equal(s.entry('f').state, 'FAILED'); assert.equal(s.entry('c').state, 'CANCELLED'); assert.equal(s.getNamespace({ namespace: ns }).lruAt, '2026-01-02T00:00:00Z'); s.close();
    await (await import('node:fs/promises')).writeFile(legacy, '{}'); await assert.rejects(() => DurableStateStore.open({ databasePath: f.databasePath, legacyStatePath: legacy }), /legacy state changed/);
  } finally { await rm(f.root, { recursive: true, force: true }); }
});

test('v1 migration strips a leading UTF-8 BOM instead of failing opaquely', async () => {
  // PowerShell's Set-Content/Out-File write this by default on Windows.
  const f = await database(); const legacy = path.join(f.root, 'legacy-bom.json');
  try {
    await (await import('node:fs/promises')).writeFile(legacy, `﻿${JSON.stringify({ entries: [{ id: 'r', state: 'RELEASED' }] })}`);
    const s = await DurableStateStore.open({ databasePath: f.databasePath, legacyStatePath: legacy });
    assert.equal(s.entry('r').state, 'SUCCEEDED');
    s.close();
  } finally { await rm(f.root, { recursive: true, force: true }); }
});

test('request reservations are idempotent, bounded, durable & reconciled', async () => {
  const f = await database(); try { let s = await DurableStateStore.open(f); assert.equal(s.reserve('q', 'disk', 10, 4).idempotent, false); assert.equal(s.reserve('q', 'disk', 10, 4).idempotent, true); assert.throws(() => s.reserve('z', 'disk', 10, 7), /overcommit/); s.create({ id: 'q' }); s.releaseReservation({ requestId: 'q' }); assert.equal(s.releaseReservation({ requestId: 'q' }).idempotent, true); s.close(); s = await DurableStateStore.open(f); assert.deepEqual(s.reservationTotals(), []); s.close(); } finally { await rm(f.root, { recursive: true, force: true }); }
});

test('trash intent crash boundaries require lease', async () => {
  const f = await database(); const ns = 'c'.repeat(64); try { const s = await DurableStateStore.open(f); s.upsertNamespace({ namespace: ns, generation: 1, state: 'RELEASED' }); const lease = s.claimMaintenanceLease({ namespace: ns, generation: 1 }); const source = path.join(f.root, 'source'); const target = path.join(f.root, 'target'); await (await import('node:fs/promises')).writeFile(source, 'x'); s.prepareNamespaceTrash({ namespace: ns, generation: 1, leaseId: lease.leaseId, trashId: 't', sourcePath: source, targetPath: target }); assert.throws(() => s.commitNamespaceTrash({ namespace: ns, generation: 1, trashId: 't' }), /trash rename has not completed/); await (await import('node:fs/promises')).rename(source, target); assert.equal(s.commitNamespaceTrash({ namespace: ns, generation: 1, trashId: 't' }).state, 'TRASHED'); s.close(); } finally { await rm(f.root, { recursive: true, force: true }); }
});

test('atomic activation rejects GC lease or pre-rename intent before source recreation', async () => {
  const f = await database(); const ns = 'd'.repeat(64); const trashId = `${ns}-4-race`;
  try {
    const s = await DurableStateStore.open(f);
    s.upsertNamespace({ namespace: ns, generation: 4, state: 'RELEASED' });
    const lease = s.claimMaintenanceLease({ namespace: ns, generation: 4 });
    assert.throws(() => s.activateNamespace({ namespace: ns }), (error) => error.code === 'RK_NAMESPACE_GC_LEASED');
    assert.equal(s.getNamespace({ namespace: ns }).state, 'RELEASED');
    const source = path.join(f.root, 'source'); const target = path.join(f.root, 'target');
    await (await import('node:fs/promises')).writeFile(source, 'x');
    s.prepareNamespaceTrash({ namespace: ns, generation: 4, leaseId: lease.leaseId, trashId, sourcePath: source, targetPath: target });
    assert.throws(() => s.activateNamespace({ namespace: ns }), (error) => error.code === 'RK_NAMESPACE_GC_PENDING');
    assert.equal(s.getNamespace({ namespace: ns }).state, 'RELEASED');
    await (await import('node:fs/promises')).rename(source, target);
    s.commitNamespaceTrash({ namespace: ns, generation: 4, trashId });
    assert.equal(s.activateNamespace({ namespace: ns }).generation, 5);
    assert.equal(s.getNamespace({ namespace: ns }).state, 'ACTIVE');
    s.close();
  } finally { await rm(f.root, { recursive: true, force: true }); }
});

test('old-generation trash deletion completes after successor activation', async () => {
  const f = await database(); const ns = 'e'.repeat(64); const trashId = `${ns}-7-old`;
  try {
    const s = await DurableStateStore.open(f);
    s.upsertNamespace({ namespace: ns, generation: 7, state: 'RELEASED' });
    const lease = s.claimMaintenanceLease({ namespace: ns, generation: 7 });
    const source = path.join(f.root, 'source'); const target = path.join(f.root, 'target');
    await (await import('node:fs/promises')).writeFile(source, 'x');
    s.prepareNamespaceTrash({ namespace: ns, generation: 7, leaseId: lease.leaseId, trashId, sourcePath: source, targetPath: target });
    await (await import('node:fs/promises')).rename(source, target);
    s.commitNamespaceTrash({ namespace: ns, generation: 7, trashId });
    s.activateNamespace({ namespace: ns });
    const queued = s.maintenanceEntry({ namespace: ns, generation: 7, trashId });
    assert.equal(queued.state, 'PENDING'); assert.equal(queued.generation, 7); assert.equal(queued.trashId, trashId);
    s.markTrashDeleted({ namespace: ns, generation: 7, trashId, deletedAt: '2026-08-15T00:00:00.000Z' });
    assert.equal(s.getNamespace({ namespace: ns }).generation, 8);
    assert.equal(s.getNamespace({ namespace: ns }).state, 'ACTIVE');
    assert.equal(s.maintenanceEntry({ namespace: ns, generation: 7, trashId }).state, 'DONE');
    s.close();
  } finally { await rm(f.root, { recursive: true, force: true }); }
});

test('namespace repository identity round-trips & its column migration is idempotent', async () => {
  const fixture = await database(); const namespace = 'e'.repeat(64);
  try {
    // A pre-migration database: the namespaces table without the identity
    // column, exactly what an existing broker file looks like.
    const legacy = await DurableStateStore.open(fixture);
    legacy.close();
    const { DatabaseSync } = await import('node:sqlite');
    const raw = new DatabaseSync(fixture.databasePath);
    raw.exec("DELETE FROM migration_markers WHERE name = 'namespaces-repository-identity'");
    raw.exec('ALTER TABLE namespaces DROP COLUMN repository_identity');
    raw.close();

    const store = await DurableStateStore.open(fixture);
    store.upsertNamespace({ namespace, generation: 1, state: 'RELEASED', terminalAt: '2026-08-14T00:00:00.000Z', lruAt: '2026-08-14T00:00:00.000Z', repositoryIdentity: '/repo/one' });
    assert.equal(store.getNamespace({ namespace }).repositoryIdentity, '/repo/one');
    // Last writer wins for a namespace that changes hands.
    store.upsertNamespace({ namespace, generation: 2, state: 'RELEASED', terminalAt: '2026-08-15T00:00:00.000Z', lruAt: '2026-08-15T00:00:00.000Z', repositoryIdentity: '/repo/two' });
    assert.equal(store.getNamespace({ namespace }).repositoryIdentity, '/repo/two');
    store.close();

    // Reopening runs the migration path again & must neither throw nor alter
    // the column twice.
    const reopened = await DurableStateStore.open(fixture);
    assert.equal(reopened.getNamespace({ namespace }).repositoryIdentity, '/repo/two');
    const activated = reopened.activateNamespace({ namespace, repositoryIdentity: '/repo/three' });
    assert.equal(activated.repositoryIdentity, '/repo/three');
    reopened.close();
  } finally { await rm(fixture.root, { recursive: true, force: true }); }
});
