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

test('reads the current broker schema admission.trustedEntry { path, sha256 }, and the entry bytes still bind', () => {
  const gate = Buffer.from('gate');
  const sha256 = createHash('sha256').update(gate).digest('hex');
  const gatePath = '/managed/trusted-gates/x/node_modules/@rightkit/release/bin/rightkit-gate.mjs';
  const config = (pin) => JSON.stringify({ admission: { trustedEntry: pin } });
  const readWith = (pin, bytes = gate) => (p) => (p.endsWith('config.json') ? config(pin) : bytes);
  assert.equal(trustedGate({ platform: 'darwin', home: '/user', read: readWith({ path: gatePath, sha256 }), realpath: (p) => p, treeHash: () => { throw new Error('no tree pin, no tree check'); } }), gatePath);
  assert.throws(() => trustedGate({ platform: 'darwin', home: '/user', read: readWith({ path: gatePath, sha256 }, Buffer.from('changed')), realpath: (p) => p }), /digest does not match/);
  assert.throws(() => trustedGate({ platform: 'darwin', home: '/user', read: readWith({ path: 'relative.mjs', sha256 }), realpath: (p) => p }), /missing or invalid/);
  // The new key wins over a stale old one.
  const both = (p) => (p.endsWith('config.json') ? JSON.stringify({ admission: { trustedEntry: { path: gatePath, sha256 } }, buildRules: { checkAdmission: { trustedGate: { path: '/old/gate.mjs', sha256: 'f'.repeat(64), treeSha256: 'a'.repeat(64) } } } }) : gate);
  assert.equal(trustedGate({ platform: 'darwin', home: '/user', read: both, realpath: (p) => p }), gatePath);
});
