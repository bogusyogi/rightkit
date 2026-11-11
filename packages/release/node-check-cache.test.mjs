import test from 'node:test';
import os from "node:os";
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { createNodeCheckCacheDigest } from './node-check-cache.mjs';

function fixture(t) {
  const scratch = ((root) => (fs.existsSync(root) ? root : os.tmpdir()))(process.platform === 'win32' ? 'D:\\scratch' : '/Volumes/D/scratch');
  const base = fs.mkdtempSync(path.join(scratch, 'rk-node-check-'));
  t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  const snapshot = path.join(base, 'snapshot'), gatePackage = path.join(base, 'gate');
  fs.mkdirSync(snapshot); fs.mkdirSync(gatePackage);
  fs.writeFileSync(path.join(snapshot, 'entry.js'), 'const value = 1;\n');
  fs.writeFileSync(path.join(gatePackage, 'gate.mjs'), 'export {};\n');
  const nodePath = path.join(base, 'node-copy');
  fs.copyFileSync(process.execPath, nodePath);
  // planScans adds this field to every row when snapshot targets are bound.
  const scan = { id: 'node-check:entry.js', class: 'A', tool: 'node', root: 'content', args: ['--check', 'entry.js'], env: { RK_SNAPSHOT_TARGETS: '{}' } };
  const context = { cwd: snapshot, command: nodePath, childEnv: { PATH: '/fixed', RIGHTKIT_BUILD_OWNERSHIP: 'first', CODEX_TOOL_CALL_ID: 'first', CODEX_OTHER: 'fixed', NODE_REPL_TRUSTED_BROWSER_CLIENT_SHA256S: 'host-hash', ...scan.env } };
  const make = (tree = 'tree') => createNodeCheckCacheDigest({ snapshot, tree, files: ['entry.js'], gateIdentity: 'gate', gatePackage, nodePath });
  return { base, snapshot, gatePackage, nodePath, scan, context, make };
}

test('plain syntax check binds actual files and environment while ownership alone is stable', (t) => {
  const f = fixture(t), key = () => f.make()(f.scan, f.context);
  const original = key(); assert.match(original, /^[0-9a-f]{64}$/);
  assert.equal(f.make()(f.scan, { ...f.context, childEnv: { ...f.context.childEnv, RIGHTKIT_BUILD_OWNERSHIP: 'second' } }), original);
  assert.equal(f.make()(f.scan, { ...f.context, childEnv: { ...f.context.childEnv, CODEX_TOOL_CALL_ID: 'second' } }), original);
  assert.notEqual(f.make()(f.scan, { ...f.context, childEnv: { ...f.context.childEnv, CODEX_OTHER: 'changed' } }), original);
  assert.notEqual(f.make('changed-tree')(f.scan, f.context), original);
  assert.notEqual(f.make()(f.scan, { ...f.context, childEnv: { ...f.context.childEnv, PATH: '/changed' } }), original);
  assert.notEqual(f.make()(f.scan, { ...f.context, childEnv: { ...f.context.childEnv, NODE_REPL_TRUSTED_BROWSER_CLIENT_SHA256S: 'other-host-hash' } }), original);
  assert.notEqual(f.make()({ ...f.scan, env: { RK_SNAPSHOT_TARGETS: '{"changed":true}' } }, { ...f.context, childEnv: { ...f.context.childEnv, RK_SNAPSHOT_TARGETS: '{"changed":true}' } }), original);
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
  assert.equal(digest(f.scan, { ...f.context, childEnv: { ...f.context.childEnv, NODE_REPL_EXTERNAL_MODULE: './preload.js' } }), null);
  assert.equal(digest({ ...f.scan, args: ['--check', 'entry.js', '--extra'] }, f.context), null);
  assert.equal(digest({ ...f.scan, executes: true }, f.context), null);
  assert.equal(digest({ ...f.scan, env: { ...f.scan.env, UNEXPECTED: 'x' } }, f.context), null);
  const reasons = [];
  const diagnosed = createNodeCheckCacheDigest({ snapshot: f.snapshot, tree: 'tree', files: ['entry.js'], gateIdentity: 'gate', gatePackage: f.gatePackage, nodePath: f.nodePath, onReject: (reason) => reasons.push(reason) });
  assert.equal(diagnosed(f.scan, { ...f.context, childEnv: { ...f.context.childEnv, NODE_OPTIONS: '--require ./preload.js' } }), null);
  assert.deepEqual(reasons, ['node-options']);
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

test('only verified per-run wrapper paths are normalized', (t) => {
  const f = fixture(t), digest = f.make();
  const originalEnv = { PATH: '/original', CARGO: '/user/cargo', CARGO_TARGET_DIR: '/user/target' };
  const context = (name, original = originalEnv) => {
    const dir = path.join(f.base, name), bin = path.join(dir, 'bin'), shim = path.join(bin, 'cargo'), sentinel = path.join(dir, 'unbound');
    const perScan = path.join(sentinel, crypto.createHash('sha256').update(f.scan.id).digest('hex').slice(0, 16));
    return { ...f.context, shimProof: { bin, shim, sentinel, originalEnv: original }, childEnv: { ...f.context.childEnv, PATH: `${bin}${path.delimiter}${original.PATH}`, CARGO: shim, CARGO_TARGET_DIR: perScan, CARGO_BUILD_TARGET_DIR: perScan } };
  };
  const first = context('one'), second = context('two');
  const key = digest(f.scan, first); assert.match(key, /^[0-9a-f]{64}$/);
  assert.equal(digest(f.scan, second), key);
  assert.notEqual(digest(f.scan, context('two', { ...originalEnv, CARGO: '/other/cargo' })), key);
  assert.notEqual(digest(f.scan, context('two', { ...originalEnv, PATH: '/other/path' })), key);
  assert.equal(digest(f.scan, { ...second, childEnv: { ...second.childEnv, CARGO_TARGET_DIR: '/unverified' } }), null);
  assert.equal(digest(f.scan, { ...second, childEnv: { ...second.childEnv, PATH: '/unverified' } }), null);
  const captured = [];
  const capture = (ctx) => createNodeCheckCacheDigest({ snapshot: f.snapshot, tree: 'tree', files: ['entry.js'], gateIdentity: 'gate', gatePackage: f.gatePackage, nodePath: f.nodePath, diagnosticScanId: f.scan.id, onDiagnostic: (row) => captured.push(row) })(f.scan, ctx);
  capture(first);
  capture(context('two', { ...originalEnv, CARGO: '/other/cargo' }));
  assert.match(captured[0].overriddenEnvironment, /^[0-9a-f]{64}$/);
  assert.notEqual(captured[0].overriddenEnvironment, captured[1].overriddenEnvironment);
});
