import assert from 'node:assert/strict';
import test from 'node:test';
import { isDirectCacheProofMain, parseCacheProofArgs, provesCacheableReuse } from './cache-proof-cli.mjs';

test('cache proof CLI bounds iterations & rejects unknown arguments', () => {
  assert.deepEqual(parseCacheProofArgs([]), { iterations: 3 });
  assert.deepEqual(parseCacheProofArgs(['--iterations', '5', '--external-root', '/managed', '--rightkit', '/managed/rightkit']), { iterations: 5, externalRoot: '/managed', rightkit: '/managed/rightkit' });
  assert.throws(() => parseCacheProofArgs(['--iterations', '0']), /1 to 10/);
  assert.throws(() => parseCacheProofArgs(['--unknown']), /unknown/);
});

test('cache proof direct-main detection works on Windows & POSIX', () => {
  assert.equal(isDirectCacheProofMain({ moduleUrl: 'file:///D:/Claude/cache-proof-cli.mjs', argv1: 'D:\\Claude\\cache-proof-cli.mjs', platform: 'win32' }), true);
  assert.equal(isDirectCacheProofMain({ moduleUrl: 'file:///tmp/cache-proof-cli.mjs', argv1: '/tmp/cache-proof-cli.mjs', platform: 'darwin' }), true);
  assert.equal(isDirectCacheProofMain({
    moduleUrl: 'file:///D:/managed/runtime-versioned/cache-proof-cli.mjs',
    argv1: 'D:\\managed\\runtime-current\\cache-proof-cli.mjs',
    platform: 'win32',
    canonicalize: (path) => path.replaceAll('/', '\\').replace('runtime-current', 'runtime-versioned')
  }), true);
});

test('cache proof accepts 100% cacheable hits while retaining bypass telemetry', () => {
  assert.equal(provesCacheableReuse({ status: 'MIXED', hits: 1, misses: 0, bypassed: 1, hitRate: 1 }), true);
  assert.equal(provesCacheableReuse({ status: 'MIXED', hits: 1, misses: 1, bypassed: 0, hitRate: 0.5 }), false);
  assert.equal(provesCacheableReuse({ status: 'BYPASSED', hits: 0, misses: 0, bypassed: 1, hitRate: null }), false);
});
