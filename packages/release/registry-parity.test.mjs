import assert from 'node:assert/strict';
import test from 'node:test';
import { comparePackedTrees, verifyRegistryParity } from './registry-parity.mjs';

test('registry parity rejects same-version packed-byte drift', () => {
  assert.deepEqual(comparePackedTrees([['build-release.mjs', 'new']], [['build-release.mjs', 'old']]), {
    equal: false,
    differences: ['build-release.mjs'],
  });
});

test('registry parity accepts identical packed trees', () => {
  assert.deepEqual(comparePackedTrees([['a.mjs', 'one'], ['package.json', 'two']], [['a.mjs', 'one'], ['package.json', 'two']]), {
    equal: true,
    differences: [],
  });
});

test('registry parity distinguishes an unpublished version from registry failure', async () => {
  const notFound = async () => ({ status: 404, ok: false });
  assert.deepEqual(await verifyRegistryParity({ allowUnpublished: true, fetchImpl: notFound }), {
    status: 'UNPUBLISHED',
    name: '@rightkit/release',
    version: '0.2.67',
  });
  await assert.rejects(
    verifyRegistryParity({ fetchImpl: notFound }),
    /registry metadata failed: HTTP 404/,
  );
});
