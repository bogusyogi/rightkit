import assert from 'node:assert/strict';
import test from 'node:test';
import { parseCacheProofArgs } from './cache-proof-cli.mjs';

test('cache proof CLI bounds iterations & rejects unknown arguments', () => {
  assert.deepEqual(parseCacheProofArgs([]), { iterations: 3 });
  assert.deepEqual(parseCacheProofArgs(['--iterations', '5', '--external-root', '/managed', '--rightkit', '/managed/rightkit']), { iterations: 5, externalRoot: '/managed', rightkit: '/managed/rightkit' });
  assert.throws(() => parseCacheProofArgs(['--iterations', '0']), /1 to 10/);
  assert.throws(() => parseCacheProofArgs(['--unknown']), /unknown/);
});
