import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { createHash } from 'node:crypto';
import { drainStoppedV1Broker } from './v1-drain.mjs';

const digest = (value) => createHash('sha256').update(value).digest('hex');
const fixture = async (state) => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'rightkit-v1-drain-'));
  const statePath = path.join(root, 'state', 'broker.json'); const receiptsDir = path.join(root, 'receipts');
  await mkdir(path.dirname(statePath), { recursive: true }); await mkdir(receiptsDir, { recursive: true });
  await writeFile(statePath, JSON.stringify(state));
  return { root, statePath, receiptsDir };
};
const options = (f, overrides = {}) => ({ statePath: f.statePath, receiptsDir: f.receiptsDir, assertBrokerStopped: async () => ({ stopped: true }), assertOwnershipAbsent: async () => ({ absent: true }), occurredAt: '2026-08-15T00:00:00.000Z', ...overrides });

test('requires stop proof before it reads or changes v1 state', async () => {
  const f = await fixture({ entries: [{ id: 'queued', state: 'QUEUED' }] });
  try {
    const before = await readFile(f.statePath, 'utf8');
    await assert.rejects(drainStoppedV1Broker(options(f, { assertBrokerStopped: async () => ({ stopped: false }) })), (error) => error.code === 'RK_V1_BROKER_STILL_RUNNING');
    assert.equal(await readFile(f.statePath, 'utf8'), before);
  } finally { await rm(f.root, { recursive: true, force: true }); }
});

test('atomically terminalizes queued + proven-absent active entries while preserving existing evidence', async () => {
  const f = await fixture({ version: 2, usedBytes: 12, entries: [
    { id: 'old', state: 'RELEASED', receipt: 'preserved' },
    { id: 'queued', state: 'QUEUED' },
    { id: 'active', state: 'ACTIVE', ownership: { pid: 101, processGroupId: 101, platform: 'posix' } },
  ] });
  try {
    const original = await readFile(f.statePath, 'utf8');
    await writeFile(path.join(f.receiptsDir, 'old.json'), '{"old":true}\n');
    const proofs = [];
    const result = await drainStoppedV1Broker(options(f, { assertOwnershipAbsent: async (value) => { proofs.push(value); return { absent: true }; } }));
    assert.equal(result.changed, true); assert.deepEqual(result.cancelledIds, ['queued', 'active']);
    assert.equal(await readFile(result.backupPath, 'utf8'), original);
    assert.ok(result.backupPath.endsWith(`${digest(original)}.bak`));
    assert.equal(await readFile(path.join(f.receiptsDir, 'old.json'), 'utf8'), '{"old":true}\n');
    assert.deepEqual(proofs.map(({ id, ownership }) => ({ id, ownership })), [{ id: 'active', ownership: { pid: 101, processGroupId: 101, platform: 'posix', pgid: 101 } }]);
    const saved = JSON.parse(await readFile(f.statePath, 'utf8'));
    assert.deepEqual(saved.entries.map(({ id, state, cancellationReason }) => ({ id, state, cancellationReason })), [
      { id: 'old', state: 'RELEASED', cancellationReason: undefined },
      { id: 'queued', state: 'CANCELLED', cancellationReason: 'v2-cutover-drain' },
      { id: 'active', state: 'CANCELLED', cancellationReason: 'v2-cutover-drain' },
    ]);
    const receipt = JSON.parse(await readFile(result.receiptPath, 'utf8'));
    assert.deepEqual(receipt.cancelledIds, ['queued', 'active']); assert.equal(receipt.sourceSha256, digest(original));
  } finally { await rm(f.root, { recursive: true, force: true }); }
});

test('rejects active ownership unless exact absence proof succeeds without changing source', async () => {
  const f = await fixture({ entries: [{ id: 'active', state: 'ACTIVE', ownership: { pid: 101, pgid: 101 } }] });
  try {
    const before = await readFile(f.statePath, 'utf8');
    await assert.rejects(drainStoppedV1Broker(options(f, { assertOwnershipAbsent: async () => ({ absent: false }) })), (error) => error.code === 'RK_V1_OWNERSHIP_LIVE');
    assert.equal(await readFile(f.statePath, 'utf8'), before);
  } finally { await rm(f.root, { recursive: true, force: true }); }
});

test('rejects unverifiable or impossible v1 entries without changing source', async () => {
  for (const entry of [
    { id: 'active', state: 'ACTIVE' },
    { id: 'queued', state: 'QUEUED', ownership: { pid: 1, pgid: 1 } },
    { id: 'unknown', state: 'RUNNING' },
  ]) {
    const f = await fixture({ entries: [entry] });
    try {
      const before = await readFile(f.statePath, 'utf8');
      await assert.rejects(drainStoppedV1Broker(options(f)), (error) => ['RK_V1_OWNERSHIP_UNKNOWN', 'RK_V1_UNKNOWN_STATE'].includes(error.code));
      assert.equal(await readFile(f.statePath, 'utf8'), before);
    } finally { await rm(f.root, { recursive: true, force: true }); }
  }
});

test('leaves fully terminal v1 state untouched', async () => {
  const f = await fixture({ entries: [{ id: 'done', state: 'CANCELLED' }] });
  try {
    const before = await readFile(f.statePath, 'utf8');
    const result = await drainStoppedV1Broker(options(f));
    assert.deepEqual(result, { changed: false, sourceSha256: digest(before), backupPath: null, receiptPath: null, cancelledIds: [] });
    assert.equal(await readFile(f.statePath, 'utf8'), before);
  } finally { await rm(f.root, { recursive: true, force: true }); }
});
