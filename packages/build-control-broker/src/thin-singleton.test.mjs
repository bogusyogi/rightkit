import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { acquireThinBrokerSingleton } from './thin-singleton.mjs';

const identity = async (pid) => ({ status: 'OK', identity: `started-${pid}` });

test('second broker rejects while first owns state', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-thin-singleton-'));
  try {
    const first = await acquireThinBrokerSingleton({ stateRoot: root, pid: 101, readIdentity: identity, kill: () => {} });
    await assert.rejects(acquireThinBrokerSingleton({ stateRoot: root, pid: 202, readIdentity: identity, kill: () => {} }), { code: 'RK_BROKER_ALREADY_RUNNING' });
    assert.equal(await first.release(), true);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('stale owner rotates aside & new broker acquires', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-thin-singleton-'));
  try {
    await acquireThinBrokerSingleton({ stateRoot: root, pid: 101, readIdentity: identity, kill: () => {} });
    const second = await acquireThinBrokerSingleton({ stateRoot: root, pid: 202, readIdentity: identity, kill: () => { throw Object.assign(new Error('gone'), { code: 'ESRCH' }); } });
    assert.equal(second.owner.pid, 202);
    await second.release();
  } finally { await rm(root, { recursive: true, force: true }); }
});
