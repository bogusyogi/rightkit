import { createHash } from 'node:crypto';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { packageDigest, trustedGate } from './host-gate.mjs';

test('selects only the configured gate whose entry bytes match the pin', () => {
  const gate = Buffer.from('gate');
  const sha256 = createHash('sha256').update(gate).digest('hex');
  const treeSha256 = 'a'.repeat(64);
  const gatePath = '/pin/node_modules/@rightkit/release/bin/gate.mjs';
  const read = (file) => file.endsWith('config.json')
    ? JSON.stringify({ buildRules: { checkAdmission: { trustedGate: { path: gatePath, sha256, treeSha256 } } } }) : gate;
  assert.equal(trustedGate({ platform: 'darwin', home: '/user', read, realpath: (p) => p, treeHash: () => treeSha256 }), gatePath);
  assert.throws(() => trustedGate({ platform: 'darwin', home: '/user', read: (p) => p.endsWith('config.json') ? read(p) : Buffer.from('changed'), realpath: (p) => p, treeHash: () => treeSha256 }), /digest/);
  assert.throws(() => trustedGate({ platform: 'darwin', home: '/user', read, realpath: (p) => p, treeHash: () => 'b'.repeat(64) }), /package digest/);
  assert.throws(() => trustedGate({ platform: 'darwin', home: '/user', read: () => '{}', realpath: (p) => p }), /missing or invalid/);
  assert.throws(() => trustedGate({ platform: 'linux' }), /managed Mac and Windows/);
});

test('accepts a Windows absolute pin path when selecting Windows configuration', () => {
  const bytes = Buffer.from('gate');
  const sha256 = createHash('sha256').update(bytes).digest('hex');
  const treeSha256 = 'a'.repeat(64);
  const gatePath = 'D:\\pin\\node_modules\\@rightkit\\release\\bin\\gate.mjs';
  const read = (file) => file.endsWith('config.json') ? JSON.stringify({ buildRules: { checkAdmission: { trustedGate: { path: gatePath, sha256, treeSha256 } } } }) : bytes;
  assert.equal(trustedGate({ platform: 'win32', read, realpath: (p) => p, treeHash: () => treeSha256 }), gatePath);
});

test('package digest binds imported files as well as entry bytes', () => {
  const root = mkdtempSync(join(tmpdir(), 'host-gate-test-'));
  try {
    mkdirSync(join(root, 'bin'));
    writeFileSync(join(root, 'bin', 'gate.mjs'), 'gate');
    writeFileSync(join(root, 'import.mjs'), 'first');
    const before = packageDigest(root);
    writeFileSync(join(root, 'import.mjs'), 'second');
    assert.notEqual(packageDigest(root), before);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
