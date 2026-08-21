import assert from 'node:assert/strict';
import test from 'node:test';
import { ThinCommandPolicy } from './thin-command-policy.mjs';

const digest = 'a'.repeat(64);
const policy = new ThinCommandPolicy({ externalCommands: [
  { shim: 'cargo', command: 'outdated', toolId: 'cargo-outdated', digest },
  { shim: 'cargo', command: 'upgrade', toolId: 'cargo-edit', digest },
  { shim: 'cargo', command: 'nextest', toolId: 'cargo-nextest', digest }
] });

test('classifies canonical resource classes from one policy', () => {
  assert.equal(policy.classify('cargo', ['--version']).lane, 'CONTROL');
  assert.equal(policy.classify('cargo', ['outdated']).lane, 'ANALYSIS');
  assert.equal(policy.classify('cargo', ['upgrade']).lane, 'WORKSPACE_WRITE');
  assert.equal(policy.classify('cargo', ['check']).lane, 'BUILD');
  assert.equal(policy.classify('cargo', ['nextest', 'run']).lane, 'BUILD');
  assert.equal(policy.classify('rustup', ['target', 'add', 'aarch64-apple-darwin']).lane, 'TOOLCHAIN_WRITE');
  assert.equal(policy.classify('rustup', ['toolchain', 'install', 'stable']).lane, 'TOOLCHAIN_WRITE');
});

test('locked metadata is read-only while mutating variants take writer lock', () => {
  assert.equal(policy.classify('cargo', ['metadata', '--locked']).workspaceAccess, 'read');
  assert.equal(policy.classify('cargo', ['metadata']).workspaceAccess, 'write');
  assert.equal(policy.classify('cargo', ['fmt', '--check']).workspaceAccess, 'read');
  assert.equal(policy.classify('cargo', ['fmt']).workspaceAccess, 'write');
});

test('unknown & unregistered external commands fail closed', () => {
  assert.throws(() => policy.classify('cargo', ['mystery']), { code: 'RK_UNKNOWN_COMMAND' });
  assert.throws(() => new ThinCommandPolicy({ externalCommands: [{ shim: 'cargo', command: 'metadata', toolId: 'evil', digest }] }), /cannot shadow/);
});
