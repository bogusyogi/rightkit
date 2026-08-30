import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { SOURCE_RUNTIME_IDENTITY } from '@rightkit/build-control-core';
import { beginCutoverDrain, cutoverDrainPending, endCutoverDrain, proveBrokerRequestsDrained } from './cutover-shared.mjs';

test('cutover drain fence blocks admissions only while its owner is live', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-cutover-drain-'));
  try {
    const fence = await beginCutoverDrain(root);
    assert.equal(await cutoverDrainPending(root), true);
    await assert.rejects(beginCutoverDrain(root), /already owned/);
    await endCutoverDrain(fence);
    assert.equal(await cutoverDrainPending(root), false);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('broker drain proof rejects every admitting, queued, or running root', async () => {
  const loadIdentity = async () => SOURCE_RUNTIME_IDENTITY;
  const send = async (command) => {
    assert.equal(command.type, 'DRAIN_STATUS');
    return { type: 'DRAIN_STATUS', open: 1, inFlight: [{ id: 'analysis-1', state: 'RUNNING', command: 'cargo test', elapsedMs: 75_000 }] };
  };
  await assert.rejects(proveBrokerRequestsDrained('/tmp/rightkit', '/tmp/broker.sock', { loadIdentity, send }), /analysis-1 \(RUNNING, 75000ms, cargo test\).*rightkit list/);
  const result = await proveBrokerRequestsDrained('/tmp/rightkit', '/tmp/broker.sock', { loadIdentity, send: async () => ({ type: 'DRAIN_STATUS', open: 0, inFlight: [] }) });
  assert.equal(result.open, 0); assert.equal(result.present, true);
  const absent = await proveBrokerRequestsDrained('/tmp/rightkit', '/tmp/broker.sock', { loadIdentity, send: async () => { throw new Error('RightKit broker unavailable: connect ENOENT'); } });
  assert.equal(absent.present, false);
});
