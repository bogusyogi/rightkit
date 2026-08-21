import test from 'node:test';
import assert from 'node:assert/strict';
import { CommandCapabilityRegistry, UnknownManagedCommandError, classifyBrokerOperation, classifyManagedCommand, createCommandCapabilityRegistry } from './command-capabilities.mjs';

test('external Cargo tools fail closed without pinned registration', () => {
  assert.throws(() => classifyManagedCommand('cargo', ['about', '--version']), UnknownManagedCommandError);
  assert.throws(() => classifyManagedCommand('cargo', ['nextest', 'run']), UnknownManagedCommandError);
  const registry = new CommandCapabilityRegistry({ cargo: {
    about: { command: 'about', lane: 'CONTROL', buildCapable: false, external: true, externalToolId: 'cargo-about', digest: 'a'.repeat(64) }
  } });
  assert.equal(registry.classify('cargo', ['about']).externalToolId, 'cargo-about');
});

test('compiler version probes are CONTROL while compiler work is BUILD', () => {
  assert.equal(classifyManagedCommand('cargo', ['--locked', 'build']).lane, 'BUILD');
  for (const shim of ['rustc', 'rustdoc']) for (const flag of ['--version', '-V', '-vV']) assert.equal(classifyManagedCommand(shim, [flag]).lane, 'CONTROL');
  assert.equal(classifyManagedCommand('rustc', ['src/main.rs']).lane, 'BUILD');
  assert.equal(classifyManagedCommand('rustdoc', ['src/lib.rs']).buildCapable, true);
  assert.equal(classifyManagedCommand('rustc', ['-vV', 'src/main.rs']).lane, 'BUILD');
  const cargoAboutProbe = ['-', '--crate-name', '___', '--print=file-names', '--crate-type', 'bin', '--crate-type', 'rlib', '--print=sysroot', '--print=cfg', '-Wwarnings'];
  assert.equal(classifyManagedCommand('rustc', cargoAboutProbe).lane, 'CONTROL');
  assert.equal(classifyManagedCommand('rustc', [...cargoAboutProbe, '-o', '/tmp/escape']).lane, 'BUILD');
});

test('Cargo commands use explicit CONTROL, BUILD, or WORKSPACE_WRITE lanes', () => {
  assert.equal(classifyManagedCommand('cargo', ['fmt', '--check']).lane, 'CONTROL');
  assert.equal(classifyManagedCommand('cargo', ['fmt']).lane, 'WORKSPACE_WRITE');
  assert.equal(classifyManagedCommand('cargo', ['fmt', '--', '--check']).lane, 'WORKSPACE_WRITE');
  assert.equal(classifyManagedCommand('cargo', ['metadata']).lane, 'WORKSPACE_WRITE');
  assert.equal(classifyManagedCommand('cargo', ['--locked', 'metadata']).lane, 'CONTROL');
  assert.equal(classifyManagedCommand('cargo', ['--frozen', 'metadata']).lane, 'CONTROL');
  assert.equal(classifyManagedCommand('cargo', ['tree']).lane, 'CONTROL');
  for (const command of ['clean', 'update', 'vendor', 'generate-lockfile']) assert.equal(classifyManagedCommand('cargo', [command]).lane, 'WORKSPACE_WRITE');
  for (const command of ['build', 'check', 'test', 'bench', 'clippy', 'doc', 'install', 'run']) assert.equal(classifyManagedCommand('cargo', [command]).lane, 'BUILD');
});

test('MAINTENANCE is explicit broker-only work, never inferred from Cargo', () => {
  assert.deepEqual(classifyBrokerOperation('gc'), { operation: 'gc', lane: 'MAINTENANCE', buildCapable: false, internal: true });
  assert.throws(() => classifyManagedCommand('cargo', ['gc']), UnknownManagedCommandError);
  assert.throws(() => classifyBrokerOperation('unregistered'), (error) => error.code === 'RK_UNKNOWN_COMMAND');
});

test('rustup registers only CONTROL inspection commands with global options', () => {
  assert.equal(classifyManagedCommand('rustup', ['--verbose', 'show']).lane, 'CONTROL');
  assert.equal(classifyManagedCommand('rustup', ['-q', '--version']).command, '--version');
  assert.equal(classifyManagedCommand('rustup', ['--profile', 'minimal', 'show']).command, 'show');
  assert.equal(classifyManagedCommand('rustup', ['target', 'list', '--installed']).lane, 'CONTROL');
  assert.equal(classifyManagedCommand('rustup', ['--quiet', 'target', 'list', '--installed']).command, 'target-list-installed');
  assert.equal(classifyManagedCommand('rustup', ['target', 'list']).lane, 'CONTROL');
  assert.equal(classifyManagedCommand('rustup', ['--quiet', 'target', 'list']).command, 'target-list');
  // cargo-guard resolves the real toolchain through `rustup which cargo|rustc`;
  // a read-only lookup the broker must admit or every guarded Cargo build fails.
  assert.equal(classifyManagedCommand('rustup', ['which', 'cargo']).command, 'which');
  assert.equal(classifyManagedCommand('rustup', ['which', 'cargo']).lane, 'CONTROL');
  assert.equal(classifyManagedCommand('rustup', ['which', 'rustc']).lane, 'CONTROL');
});

test('rustup admits only exact target add grammar into TOOLCHAIN_WRITE', () => {
  assert.equal(classifyManagedCommand('rustup', ['target', 'add', 'aarch64-apple-darwin']).lane, 'TOOLCHAIN_WRITE');
  for (const argv of [
    ['target', 'add'], ['target', 'add', 'aarch64-apple-darwin', '--force'], ['target', 'add', '--toolchain', 'stable'],
    ['target', 'remove', 'aarch64-apple-darwin'], ['update'], ['component', 'add', 'rust-src'], ['run', 'stable', 'rustc'],
    ['toolchain', 'install', 'stable'], ['target', 'add', '../escape']
  ]) assert.throws(() => classifyManagedCommand('rustup', argv), UnknownManagedCommandError);
});

test('rustup administration, run, & unknown commands reject', () => {
  for (const command of ['default', 'toolchain', 'target', 'component', 'run', 'update', 'unregistered']) assert.throws(() => classifyManagedCommand('rustup', [command]), UnknownManagedCommandError);
  assert.throws(() => classifyManagedCommand('rustup', ['target', 'list', '--unknown']), UnknownManagedCommandError);
});

test('unknown command & unknown shim reject without capability guessing', () => {
  assert.throws(() => classifyManagedCommand('cargo', ['unregistered-command']), UnknownManagedCommandError);
  assert.throws(() => classifyManagedCommand('pnpm', ['test']), (error) => error.code === 'RK_UNKNOWN_COMMAND');
});

test('registry accepts explicitly supplied capabilities only', () => {
  const registry = new CommandCapabilityRegistry({ cargo: { inspect: { command: 'inspect', lane: 'CONTROL', buildCapable: false } } });
  assert.equal(registry.classify('cargo', ['inspect']).lane, 'CONTROL');
  assert.throws(() => registry.classify('cargo', ['build']), /unregistered managed command/);
});

test('wildcard external registrations require a SHA-256 digest pin', () => {
  const registry = new CommandCapabilityRegistry({ cargo: { '*': { command: '*', lane: 'CONTROL', buildCapable: false } } });
  assert.throws(() => registry.classify('cargo', ['plugin']), UnknownManagedCommandError);
  const pinned = new CommandCapabilityRegistry({ cargo: { '*': { command: '*', lane: 'CONTROL', buildCapable: false, digest: 'a'.repeat(64) } } });
  assert.equal(pinned.classify('cargo', ['plugin']).lane, 'CONTROL');
});

test('factory installs only explicit external tool pins', () => {
  const registry = createCommandCapabilityRegistry({ externalCommands: [{ shim: 'cargo', command: 'nextest', toolId: 'cargo-nextest', lane: 'BUILD', digest: 'b'.repeat(64) }] });
  const capability = registry.classify('cargo', ['nextest', 'run']);
  assert.equal(capability.externalToolId, 'cargo-nextest'); assert.equal(capability.externalDigest, 'b'.repeat(64));
  assert.throws(() => createCommandCapabilityRegistry({ externalCommands: [{ shim: 'cargo', command: 'nextest', toolId: 'x', lane: 'BUILD', digest: 'bad' }] }));
  assert.throws(() => createCommandCapabilityRegistry({ externalCommands: [{ shim: 'cargo', command: 'nextest', toolId: 'x', lane: 'BUILD', digest: 'b'.repeat(64) }, { shim: 'cargo', command: 'nextest', toolId: 'y', lane: 'BUILD', digest: 'c'.repeat(64) }] }));
});

test('any digest-pinned external Cargo tool registers, not just a hardcoded pair', () => {
  const registry = createCommandCapabilityRegistry({ externalCommands: [
    { shim: 'cargo', command: 'outdated', toolId: 'cargo-outdated', lane: 'WORKSPACE_WRITE', digest: 'd'.repeat(64) },
    { shim: 'cargo', command: 'machete', toolId: 'cargo-machete', lane: 'WORKSPACE_WRITE', digest: 'e'.repeat(64) },
    { shim: 'cargo', command: 'audit', toolId: 'cargo-audit', lane: 'CONTROL', digest: 'f'.repeat(64) },
    { shim: 'cargo', command: 'deny', toolId: 'cargo-deny', lane: 'CONTROL', digest: '0'.repeat(64) }
  ] });
  assert.equal(registry.classify('cargo', ['outdated']).lane, 'WORKSPACE_WRITE');
  assert.equal(registry.classify('cargo', ['machete']).externalToolId, 'cargo-machete');
  assert.equal(registry.classify('cargo', ['audit']).lane, 'CONTROL');
  assert.equal(registry.classify('cargo', ['deny', 'check']).externalDigest, '0'.repeat(64));
});

test('unpinned external Cargo tools still fail closed', () => {
  // Declaring the placeholder must not make the command reachable on its own.
  for (const command of ['outdated', 'machete', 'audit', 'deny']) {
    assert.throws(() => classifyManagedCommand('cargo', [command]), UnknownManagedCommandError);
  }
});

test('an external registration can never shadow a built-in Cargo command', () => {
  for (const command of ['build', 'test', 'clean', 'metadata', 'install']) {
    assert.throws(
      () => createCommandCapabilityRegistry({ externalCommands: [{ shim: 'cargo', command, toolId: 'evil', lane: 'BUILD', digest: 'a'.repeat(64) }] }),
      /cannot shadow built-in/,
      `${command} was shadowable`
    );
  }
});

test('external Cargo command names are bounded tokens', () => {
  for (const command of ['--version', '../evil', 'Outdated', 'a b', '-x']) {
    assert.throws(() => createCommandCapabilityRegistry({ externalCommands: [{ shim: 'cargo', command, toolId: 'x', lane: 'CONTROL', digest: 'a'.repeat(64) }] }));
  }
});
