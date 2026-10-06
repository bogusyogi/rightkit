import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { comparePackedTrees, packedArchivePath, verifyRegistryParity } from './registry-parity.mjs';

test('registry parity accepts current object and legacy array pnpm pack JSON', () => {
  assert.equal(packedArchivePath('{"filename":"/tmp/rightkit-release.tgz"}'), '/tmp/rightkit-release.tgz');
  assert.equal(packedArchivePath('[{"filename":"/tmp/rightkit-release.tgz"}]'), '/tmp/rightkit-release.tgz');
  assert.throws(() => packedArchivePath('{}'), /omitted filename/);
});

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
    version: JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8')).version,
  });
  await assert.rejects(
    verifyRegistryParity({ fetchImpl: notFound }),
    /registry metadata failed: HTTP 404/,
  );
});

test('registry parity covers every published @rightkit package, not only release', async () => {
  const { publishedPackageNames, verifyAllRegistryParity } = await import('./registry-parity.mjs');
  const names = await publishedPackageNames();
  for (const name of ['@rightkit/release', '@rightkit/media-ui', '@rightkit/app-shell', '@rightkit/qa', '@rightkit/git']) assert.ok(names.includes(name), name);
  const requested = [];
  const notFound = async (url) => { requested.push(url); return { status: 404, ok: false }; };
  const results = await verifyAllRegistryParity({ allowUnpublished: true, fetchImpl: notFound, names: ['@rightkit/media-ui', '@rightkit/app-shell'] });
  assert.deepEqual(results.map((r) => [r.name, r.status]), [['@rightkit/media-ui', 'UNPUBLISHED'], ['@rightkit/app-shell', 'UNPUBLISHED']]);
  assert.ok(requested[0].includes('%40rightkit%2Fmedia-ui'));
});
