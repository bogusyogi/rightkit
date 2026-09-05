import assert from 'node:assert/strict';
import test from 'node:test';
import { ThinCommandPolicy } from './thin-command-policy.mjs';

const previousAllowCargoClean = process.env.RIGHTKIT_ALLOW_CARGO_CLEAN;
delete process.env.RIGHTKIT_ALLOW_CARGO_CLEAN;
test.after(() => { if (previousAllowCargoClean === undefined) delete process.env.RIGHTKIT_ALLOW_CARGO_CLEAN; else process.env.RIGHTKIT_ALLOW_CARGO_CLEAN = previousAllowCargoClean; });

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

test('cargo install is intentionally denied, including nested and external shadowing', () => {
  assert.throws(() => policy.classify('cargo', ['install', 'some-crate']), { code: 'RK_COMMAND_DENIED', name: 'DeniedThinCommandError' });
  assert.throws(() => policy.classifyNested(null, 'cargo', ['install', 'some-crate']), { code: 'RK_COMMAND_DENIED' });
  assert.throws(() => new ThinCommandPolicy({ externalCommands: [{ shim: 'cargo', command: 'install', toolId: 'evil', digest }] }), /cannot shadow/);
});

test('cargo clean fails closed by default & requires explicit opt-in', () => {
  assert.throws(() => policy.classify('cargo', ['clean']), { code: 'RK_CLEAN_REQUIRES_OPT_IN', name: 'CleanRequiresOptInError' });
  assert.throws(() => policy.classifyNested(null, 'cargo', ['clean']), { code: 'RK_CLEAN_REQUIRES_OPT_IN', name: 'CleanRequiresOptInError' });
  assert.throws(() => new ThinCommandPolicy({}).classify('cargo', ['clean']), { code: 'RK_CLEAN_REQUIRES_OPT_IN' });
  const optedIn = new ThinCommandPolicy({ allowCargoClean: true });
  const capability = optedIn.classify('cargo', ['clean']);
  assert.equal(capability.lane, 'WORKSPACE_WRITE');
  assert.equal(capability.artifactWrite, true);
});

test('cargo clean denial accurately distinguishes target artifacts from shared sccache', () => {
  assert.throws(
    () => policy.classify('cargo', ['clean']),
    (error) => /warm Cargo target artifacts/.test(error.message) && /does not clear the shared sccache store/.test(error.message),
  );
});

test('cargo clean opt-in reads from the broker process environment, not a caller default', () => {
  process.env.RIGHTKIT_ALLOW_CARGO_CLEAN = '1';
  try { assert.equal(new ThinCommandPolicy({}).classify('cargo', ['clean']).lane, 'WORKSPACE_WRITE'); }
  finally { delete process.env.RIGHTKIT_ALLOW_CARGO_CLEAN; }
});
