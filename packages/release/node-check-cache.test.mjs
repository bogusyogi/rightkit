import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { createNodeCheckCacheDigest } from './node-check-cache.mjs';

function fixture(t) {
  const scratch = process.platform === 'win32' ? 'D:\\scratch' : '/Volumes/D/scratch';
  const base = fs.mkdtempSync(path.join(scratch, 'rk-node-check-'));
  t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  const snapshot = path.join(base, 'snapshot'), gatePackage = path.join(base, 'gate');
  fs.mkdirSync(snapshot); fs.mkdirSync(gatePackage);
  fs.writeFileSync(path.join(snapshot, 'entry.js'), 'const value = 1;\n');
  fs.writeFileSync(path.join(gatePackage, 'gate.mjs'), 'export {};\n');
  const nodePath = path.join(base, 'node-copy');
  fs.copyFileSync(process.execPath, nodePath);
  const scan = { id: 'node-check:entry.js', class: 'A', tool: 'node', root: 'content', args: ['--check', 'entry.js'] };
  const context = { cwd: snapshot, command: nodePath, childEnv: { PATH: '/fixed', RIGHTKIT_BUILD_OWNERSHIP: 'first' } };
  const make = (tree = 'tree') => createNodeCheckCacheDigest({ snapshot, tree, files: ['entry.js'], gateIdentity: 'gate', gatePackage, nodePath });
  return { base, snapshot, gatePackage, nodePath, scan, context, make };
}

test('plain syntax check binds actual files and environment while ownership alone is stable', (t) => {
  const f = fixture(t), key = () => f.make()(f.scan, f.context);
  const original = key(); assert.match(original, /^[0-9a-f]{64}$/);
  assert.equal(f.make()(f.scan, { ...f.context, childEnv: { ...f.context.childEnv, RIGHTKIT_BUILD_OWNERSHIP: 'second' } }), original);
  assert.notEqual(f.make('changed-tree')(f.scan, f.context), original);
  assert.notEqual(f.make()(f.scan, { ...f.context, childEnv: { ...f.context.childEnv, PATH: '/changed' } }), original);
  fs.writeFileSync(path.join(f.snapshot, 'entry.js'), 'const value = 2;\n');
  assert.notEqual(key(), original);
  fs.writeFileSync(path.join(f.snapshot, 'entry.js'), 'const value = 1;\n');
  fs.writeFileSync(path.join(f.base, 'package.json'), '{"type":"module"}\n');
  assert.notEqual(key(), original);
  fs.rmSync(path.join(f.base, 'package.json'));
  fs.writeFileSync(path.join(f.gatePackage, 'gate.mjs'), 'export const changed = true;\n');
  assert.notEqual(key(), original);
  fs.writeFileSync(path.join(f.gatePackage, 'gate.mjs'), 'export {};\n');
  fs.appendFileSync(f.nodePath, Buffer.from([0]));
  assert.notEqual(key(), original);
});

test('unsafe options and non-plain rows never receive a key', (t) => {
  const f = fixture(t), digest = f.make();
  assert.equal(digest(f.scan, { ...f.context, childEnv: { ...f.context.childEnv, NODE_OPTIONS: '--require ./preload.js' } }), null);
  assert.equal(digest({ ...f.scan, args: ['--check', 'entry.js', '--extra'] }, f.context), null);
  assert.equal(digest({ ...f.scan, executes: true }, f.context), null);
});

test('a source under an aliased directory never receives a key', (t) => {
  const f = fixture(t), real = path.join(f.snapshot, 'real');
  fs.mkdirSync(real);
  fs.writeFileSync(path.join(real, 'entry.js'), 'const value = 1;\n');
  fs.symlinkSync(real, path.join(f.snapshot, 'alias'), process.platform === 'win32' ? 'junction' : 'dir');
  const scan = { ...f.scan, id: 'node-check:alias/entry.js', args: ['--check', 'alias/entry.js'] };
  const digest = createNodeCheckCacheDigest({ snapshot: f.snapshot, tree: 'tree', files: ['alias/entry.js'], gateIdentity: 'gate', gatePackage: f.gatePackage, nodePath: f.nodePath });
  assert.equal(digest(scan, f.context), null);
});
