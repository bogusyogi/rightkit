import test from 'node:test';
import assert from 'node:assert/strict';

// Fake `check` only: this test never reaches the updater endpoint or any other network host.
await import('./ts-resolve.mjs');
const { createAppUpdateClient } = await import('../lib/app-update.ts');

test('no update offered by the fake check resolves to up_to_date', async () => {
  let calls = 0;
  const client = createAppUpdateClient(async () => { calls += 1; return null; });
  const status = await client.checkOnce();
  assert.equal(calls, 1);
  assert.equal(status.kind, 'up_to_date');
});

test('a patch offered by the fake check is reported as an available, eligible update', async () => {
  const client = createAppUpdateClient(async () => ({
    version: '9.9.9',
    rawJson: { version: '9.9.9', tier: 'patch', platforms: {} },
    downloadAndInstall: async () => {},
  }));
  const status = await client.checkOnce();
  assert.equal(status.kind, 'available');
  assert.equal(status.version, '9.9.9');
});
