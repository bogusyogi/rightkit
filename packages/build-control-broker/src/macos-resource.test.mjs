import assert from 'node:assert/strict';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { loadMacosResourceAddon, resetMacosResourceAddonCacheForTests } from './macos-resource.mjs';

test('macOS resource addon loader remains unavailable before immutable staging', () => {
  resetMacosResourceAddonCacheForTests();
  assert.equal(loadMacosResourceAddon({ exists: () => false }), null);
  assert.equal(loadMacosResourceAddon({ exists: () => { throw new Error('cache must be used'); } }), null);
});

test('macOS resource addon loader rejects an untrusted descriptor before require', () => {
  resetMacosResourceAddonCacheForTests();
  let reads = 0;
  const addon = loadMacosResourceAddon({
    exists: () => true,
    path: fileURLToPath(new URL('../native/macos-resource/macos_resource.c', import.meta.url)),
    manifestPath: '/tmp/rightkit-runtime-manifest.json',
    readFile: () => { reads += 1; return JSON.stringify({ macosResourceAddon: { path: 'wrong.node', sha256: '0'.repeat(64), sizeBytes: 1 } }); },
  });
  assert.equal(addon, null);
  assert.ok(reads >= 1);
});
