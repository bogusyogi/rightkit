import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { CACHE_PROOF_MARKER, assertCacheProofTarget, clearCacheProofTarget, initializeCacheProofTarget } from './cache-proof.mjs';

test('proof cleanup removes only marked fixture target children', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rk-proof-'));
  try {
    const fixtureWorkspace = join(root, 'fixture-rust');
    const target = join(root, 'rightkit-build-control', 'workspaces', 'a'.repeat(32), 'generations', '000001', 'target');
    await initializeCacheProofTarget({ target, expectedTarget: target, managedRoot: root, fixtureWorkspace });
    await mkdir(join(target, 'debug'), { recursive: true }); await writeFile(join(target, 'debug', 'artifact'), 'x');
    const result = await clearCacheProofTarget({ target, expectedTarget: target, managedRoot: root, fixtureWorkspace });
    assert.deepEqual(result.removed, ['debug']);
    assert.match(await readFile(join(target, CACHE_PROOF_MARKER), 'utf8'), /rightkit\.cache-proof-target\.v1/);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('proof cleanup rejects broad, wrong-workspace & non-generation paths', () => {
  const managedRoot = '/managed'; const fixtureWorkspace = '/managed/fixture-rust';
  assert.throws(() => assertCacheProofTarget({ target: managedRoot, expectedTarget: managedRoot, managedRoot, fixtureWorkspace }), /protected root|outside managed/);
  assert.throws(() => assertCacheProofTarget({ target: '/managed/compiler-cache', expectedTarget: '/managed/compiler-cache', managedRoot, fixtureWorkspace }), /outside managed/);
  assert.throws(() => assertCacheProofTarget({ target: '/managed/rightkit-build-control/workspaces/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/generations/000001/target', expectedTarget: '/managed/rightkit-build-control/workspaces/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/generations/000002/target', managedRoot, fixtureWorkspace }), /does not match/);
  assert.throws(() => assertCacheProofTarget({ target: '/managed/rightkit-build-control/workspaces/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/generations/000001/target', expectedTarget: '/managed/rightkit-build-control/workspaces/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/generations/000001/target', managedRoot, fixtureWorkspace: '/repo' }), /workspace/);
});
