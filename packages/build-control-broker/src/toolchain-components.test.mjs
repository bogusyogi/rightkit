import test from 'node:test';
import assert from 'node:assert/strict';
import { inspectManagedTargetStd, requireManagedTargetStd, rustupToolchainSelector } from './toolchain-components.mjs';

test('broker-fixed selector removes namespace-only stable prefix without accepting caller input', () => {
  assert.equal(rustupToolchainSelector('stable-1.97.1-aarch64-apple-darwin'), '1.97.1-aarch64-apple-darwin');
  assert.equal(rustupToolchainSelector('stable'), 'stable');
});

test('target inspection binds compiler identity, managed Rustup home, pinned toolchain, & installed std', async () => {
  const calls = [];
  const run = async (_path, argv, options) => { calls.push({ argv, home: options.env.RUSTUP_HOME }); return argv[0] === 'run' ? { stdout: 'release: 1.97.1\nhost: aarch64-apple-darwin\n' } : { stdout: 'aarch64-apple-darwin\nx86_64-apple-darwin\n' }; };
  const evidence = await inspectManagedTargetStd({ rustupPath: '/managed/rustup', rustupHome: '/managed/home', pinnedToolchain: 'stable-1.97.1-aarch64-apple-darwin', target: 'x86_64-apple-darwin', run });
  assert.equal(evidence.available, true); assert.equal(evidence.toolchain, '1.97.1-aarch64-apple-darwin'); assert.equal(evidence.compiler.release, '1.97.1');
  assert.deepEqual(calls.map(({ argv }) => argv), [['run', '1.97.1-aarch64-apple-darwin', 'rustc', '-vV'], ['target', 'list', '--installed', '--toolchain', '1.97.1-aarch64-apple-darwin']]);
  assert.deepEqual(calls.map(({ home }) => home), ['/managed/home', '/managed/home']);
});

test('missing target std fails with typed evidence', async () => {
  const run = async (_path, argv) => argv[0] === 'run' ? { stdout: 'release: 1.97.1\nhost: aarch64-apple-darwin\n' } : { stdout: 'aarch64-apple-darwin\n' };
  const evidence = await inspectManagedTargetStd({ rustupPath: '/managed/rustup', rustupHome: '/managed/home', pinnedToolchain: 'stable-1.97.1-aarch64-apple-darwin', target: 'x86_64-apple-darwin', run });
  assert.throws(() => requireManagedTargetStd(evidence), { code: 'RK_TARGET_STD_UNAVAILABLE' });
});

test('compiler identity mismatch fails closed', async () => {
  const run = async (_path, argv) => argv[0] === 'run' ? { stdout: 'release: 1.96.0\nhost: aarch64-apple-darwin\n' } : { stdout: 'x86_64-apple-darwin\n' };
  await assert.rejects(inspectManagedTargetStd({ rustupPath: '/managed/rustup', rustupHome: '/managed/home', pinnedToolchain: 'stable-1.97.1-aarch64-apple-darwin', target: 'x86_64-apple-darwin', run }), { code: 'RK_TOOLCHAIN_IDENTITY_MISMATCH' });
});
