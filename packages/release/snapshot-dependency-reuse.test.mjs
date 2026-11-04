import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { dependencyDigest } from './snapshot-dependency-reuse.mjs';

test('dependency proof binds bytes, metadata and modes', (t) => {
  const scratch = process.platform === 'win32' ? 'D:\\scratch' : '/Volumes/D/scratch';
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
