import test from 'node:test';
import os from "node:os";
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { dependencyDigest, canonicalConfigFingerprint, preparationInputComponents } from './snapshot-dependency-reuse.mjs';

test('dependency proof binds bytes, metadata and modes', (t) => {
  const scratch = ((root) => (fs.existsSync(root) ? root : os.tmpdir()))(process.platform === 'win32' ? 'D:\\scratch' : '/Volumes/D/scratch');
  const root = fs.mkdtempSync(path.join(scratch, 'rk-deps-proof-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const modules = path.join(root, 'node_modules'); fs.mkdirSync(modules);
  const pkg = path.join(modules, 'pkg.js'); fs.writeFileSync(pkg, 'one');
  const before = dependencyDigest(root, ['node_modules']);
  fs.writeFileSync(pkg, 'two'); assert.notEqual(dependencyDigest(root, ['node_modules']), before);
  fs.writeFileSync(pkg, 'one'); fs.writeFileSync(path.join(modules, '.pnpm-workspace-state-v1.json'), '{}');
  assert.notEqual(dependencyDigest(root, ['node_modules']), before);
  fs.rmSync(path.join(modules, '.pnpm-workspace-state-v1.json'));
  if (process.platform !== 'win32') {
    fs.chmodSync(pkg, 0o755); assert.notEqual(dependencyDigest(root, ['node_modules']), before);
    fs.chmodSync(pkg, 0o644);
  }
  fs.rmSync(pkg); assert.notEqual(dependencyDigest(root, ['node_modules']), before);
});

test('preparation inputs ignore only per-run ownership and approved scheduling controls', () => {
  const input = { source: 'tree', toolchain: 'tool', config: 'config', store: 'store', env: { RIGHTKIT_BUILD_OWNERSHIP: 'first', RIGHTKIT_BUILD_OWNERSHIP_FILE: '/owned', NODE_OPTIONS: 'same', RK_GATE_JOBS: '4' } };
  const fingerprint = (env) => preparationInputComponents({ ...input, env });
  const before = fingerprint(input.env);
  assert.deepEqual(fingerprint({ ...input.env, RIGHTKIT_BUILD_OWNERSHIP: 'second', RK_GATE_JOBS: '8' }), before);
  assert.notEqual(fingerprint({ ...input.env, NODE_OPTIONS: 'changed' }).environment, before.environment);
  assert.notEqual(fingerprint({ ...input.env, RIGHTKIT_BUILD_OWNERSHIP_FILE: '/other' }).environment, before.environment);
  assert.notEqual(preparationInputComponents({ ...input, config: 'changed' }).config, before.config);
});

test('config identity ignores object order but binds values, array order and file hashes', () => {
  const files = [['user', 'hash-a'], ['global', 'hash-b']];
  const fingerprint = (config, ids = files) => canonicalConfigFingerprint(config, ids);
  const before = fingerprint({ a: [1, 2], b: { x: 1, y: 2 } });
  assert.equal(fingerprint({ b: { y: 2, x: 1 }, a: [1, 2] }), before);
  assert.notEqual(fingerprint({ b: { y: 2, x: 1 }, a: [2, 1] }), before);
  assert.notEqual(fingerprint({ b: { y: 3, x: 1 }, a: [1, 2] }), before);
  assert.notEqual(fingerprint({ a: [1, 2], b: { x: 1, y: 2 } }, [['user', 'changed'], ['global', 'hash-b']]), before);
});
