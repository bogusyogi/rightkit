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
  assert.equal(policy.classify('cargo', ['metadata', '--offline', '--no-deps', '--format-version', '1']).workspaceAccess, 'observe');
  assert.equal(policy.classify('cargo', ['fmt', '--check']).workspaceAccess, 'read');
  assert.equal(policy.classify('cargo', ['fmt']).workspaceAccess, 'write');
});

test('unknown & unregistered external commands fail closed', () => {
  assert.throws(() => policy.classify('cargo', ['mystery']), { code: 'RK_UNKNOWN_COMMAND' });
  assert.throws(() => new ThinCommandPolicy({ externalCommands: [{ shim: 'cargo', command: 'metadata', toolId: 'evil', digest }] }), /cannot shadow/);
});

test('cargo install is intentionally denied, including nested and external shadowing', () => {
  assert.throws(() => policy.classify('cargo', ['install', 'some-crate']), { code: 'RK_COMMAND_DENIED', name: 'DeniedThinCommandError' });
  assert.throws(() => policy.classifyNested(null, 'cargo', ['install', 'some-crate']), { code: 'RK_COMMAND_DENIED' });
  assert.throws(() => new ThinCommandPolicy({ externalCommands: [{ shim: 'cargo', command: 'install', toolId: 'evil', digest }] }), /cannot shadow/);
});

test('cargo login is a credential-stdin control command; publish builds; tokens never ride argv', () => {
  const login = policy.classify('cargo', ['login']);
  assert.equal(login.lane, 'CONTROL'); assert.equal(login.credentialStdin, true); assert.equal(login.compile, false);
  assert.equal(policy.classify('cargo', ['login', '--registry', 'crates-io', '-q']).credentialStdin, true);
  assert.equal(policy.classify('cargo', ['+stable', 'login', '--registry=crates-io']).credentialStdin, true);
  for (const argv of [['login', 'test-token-value'], ['login', '--registry', 'r', 'test-token-value'], ['login', '--', '--secret']]) {
    assert.throws(() => policy.classify('cargo', argv), (error) => error.code === 'RK_COMMAND_DENIED' && !error.message.includes('test-token-value'));
  }
  const publish = policy.classify('cargo', ['publish', '-p', 'rightkit-db', '--dry-run']);
  assert.equal(publish.lane, 'BUILD'); assert.equal(publish.compile, true); assert.equal(publish.credentialStdin, undefined);
  assert.equal(policy.classify('cargo', ['package', '--list', '-p', 'rightkit-db']).lane, 'BUILD');
  assert.throws(() => policy.classify('cargo', ['publish', '--token', 'test-token-value']), /never on the command line/);
  assert.throws(() => policy.classify('cargo', ['publish', '--token=test-token-value']), /never on the command line/);
});

test('cargo read-only queries, scaffolding, and registry account commands are registered', () => {
  for (const command of ['info', 'pkgid', 'verify-project', 'report', 'config']) assert.equal(policy.classify('cargo', [command]).lane, 'ANALYSIS', command);
  for (const command of ['new', 'init']) assert.equal(policy.classify('cargo', [command, 'x']).lane, 'WORKSPACE_WRITE', command);
  for (const command of ['logout', 'owner', 'yank']) assert.equal(policy.classify('cargo', [command]).lane, 'CONTROL', command);
  for (const command of ['owner', 'yank']) assert.throws(() => policy.classify('cargo', [command, '--token', 'test-token-value']), /never on the command line/);
  assert.throws(() => policy.classify('cargo', ['uninstall', 'x']), (error) => error.code === 'RK_COMMAND_DENIED');
  for (const command of ['outdated', 'update', 'package']) assert.ok(policy.classify('cargo', [command]), command);
});
