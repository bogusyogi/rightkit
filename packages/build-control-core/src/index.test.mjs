import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, symlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { LOCAL_OUTPUT_SCAN_POLICY, MANAGED_RUST_ENVIRONMENT_KEYS, PROFILES, captureManagedRustEnvironment, externalRoots, immutableToolRoot, normalizeBuildRequest, normalizeManagedAgentRequest, scanForbiddenLocalOutputs, scanWorkspaceForUnmanagedOutputReceipt, scanWorkspaceForUnmanagedOutputs, validateManagedRustEnvironment, validateManagedRustupInvocation } from './index.mjs';

test('normalizes registered request without executable authority', () => {
  const request = normalizeBuildRequest({ frontend: 'cargo', argv: ['test'], workspace: '/external/work', environment: { CI: '1' } });
  assert.deepEqual(request, { frontend: 'cargo', argv: ['test'], workspace: '/external/work', environment: { CI: '1' } });
  assert.throws(() => normalizeBuildRequest({ frontend: 'unknown', argv: [], workspace: '/external/work' }));
});
test('immutable tool roots reject mutable or escaping paths', () => {
  assert.equal(immutableToolRoot({ path: '/external/tools/cargo', root: '/external/tools', sha256: 'a'.repeat(64) }).path, '/external/tools/cargo');
  assert.throws(() => immutableToolRoot({ path: '/external/cargo', root: '/external/tools', sha256: 'a'.repeat(64) }));
});
test('managed-agent normalization rejects redirect vectors & unmanaged tools', () => {
  const request = { frontend: 'cargo', argv: ['test'], workspace: '/workspace', environment: { CI: '1' }, managedToolRoot: '/managed/tools' };
  const normalized = normalizeManagedAgentRequest({ ...request, configuration: { net: { retry: 2 } } });
  assert.equal(normalized.profile, PROFILES.MANAGED_AGENT);
  assert.equal(normalized.managedToolRoot, '/managed/tools');
  assert.deepEqual(normalized.configuration, { net: { retry: 2 } });
  assert.throws(() => normalizeManagedAgentRequest({ ...request, argv: ['build', '--target-dir', '/tmp/target'] }), /redirect/);
  assert.throws(() => normalizeManagedAgentRequest({ ...request, argv: ['build', '--config', 'build.target-dir=/tmp/target'] }), /redirect/);
  assert.throws(() => normalizeManagedAgentRequest({ ...request, argv: ['+nightly', 'test'] }), /redirect/);
  assert.throws(() => normalizeManagedAgentRequest({ ...request, environment: { CARGO_TARGET_DIR: '/tmp/target' } }), /environment/);
  assert.throws(() => normalizeManagedAgentRequest({ ...request, environment: { cargo_build_build_dir: '/tmp/build' } }), /environment/);
  assert.throws(() => normalizeManagedAgentRequest({ ...request, environment: { CARGO_BUILD_TARGET_DIR: '/tmp/target' } }), /environment/);
  assert.throws(() => normalizeManagedAgentRequest({ ...request, environment: { CARGO_INSTALL_ROOT: '/tmp/install' } }), /environment/);
  assert.throws(() => normalizeManagedAgentRequest({ ...request, environment: { npm_config_cache: '/tmp/npm' } }), /environment/);
  assert.throws(() => normalizeManagedAgentRequest({ ...request, environment: { npm_config_store_dir: '/tmp/pnpm' } }), /environment/);
  assert.throws(() => normalizeManagedAgentRequest({ ...request, configuration: { build: { targetDir: '/tmp/target' } } }), /configuration/);
  assert.throws(() => normalizeManagedAgentRequest({ ...request, configuration: { env: { CARGO_TARGET_DIR: '/tmp/target' } } }), /configuration/);
  assert.throws(() => normalizeManagedAgentRequest({ ...request, argv: ['/usr/local/bin/cargo', 'test'] }), /redirect/);
  assert.doesNotThrow(() => normalizeManagedAgentRequest({ ...request, argv: ['test', '--', '--target-dir'] }));
  assert.doesNotThrow(() => normalizeBuildRequest({ ...request, profile: PROFILES.STRICT_SYSTEM, argv: ['--target-dir', '/managed/target'] }));
});

test('captures every controlled Rust/Cargo override for fail-closed admission', () => {
  const ambient = Object.fromEntries(MANAGED_RUST_ENVIRONMENT_KEYS.map((key) => [key.toLowerCase(), `/unsafe/${key}`]));
  ambient.PATH = '/safe/path'; ambient.CI = '1';
  const captured = captureManagedRustEnvironment(ambient);
  assert.equal(Object.keys(captured).length, MANAGED_RUST_ENVIRONMENT_KEYS.length);
  assert.equal(captured.PATH, undefined);
  assert.throws(() => normalizeManagedAgentRequest({ frontend: 'cargo', argv: ['build'], workspace: '/workspace', environment: captured }), /environment/);
  assert.deepEqual(validateManagedRustEnvironment({ CARGO_TARGET_DIR: '/managed/target' }, { CARGO_TARGET_DIR: '/managed/target' }), { CARGO_TARGET_DIR: '/managed/target' });
  assert.throws(() => validateManagedRustEnvironment({ CARGO_TARGET_DIR: '/unsafe' }, { CARGO_TARGET_DIR: '/managed/target' }), /conflicting CARGO_TARGET_DIR/);
  assert.throws(() => validateManagedRustEnvironment({ RUSTFLAGS: '-C target-dir=/unsafe' }, {}), /conflicting RUSTFLAGS/);
});

test('rustup administration stays available but rustup-run build tools fail closed', () => {
  assert.doesNotThrow(() => validateManagedRustupInvocation('rustup', ['toolchain', 'list']));
  assert.throws(() => validateManagedRustupInvocation('rustup', ['run', 'stable', 'cargo', 'test']), /cannot launch/);
  assert.throws(() => validateManagedRustupInvocation('rustup', ['--verbose', 'run', 'stable', 'rustc.exe']), /cannot launch/);
});
test('external roots are deterministic & output scan policy identifies local payload roots', () => {
  const input = { externalRoot: '/Volumes/builds', repositoryIdentity: 'git:abc123', toolchain: 'rust-1.97.1', target: 'aarch64-apple-darwin', buildProfile: 'release', policyProfile: PROFILES.MANAGED_AGENT };
  const first = externalRoots(input); const second = externalRoots(input);
  const debug = externalRoots({ ...input, buildProfile: 'debug', policyProfile: PROFILES.STRICT_SYSTEM });
  assert.deepEqual(first, second); assert.match(first.root, /^\/Volumes\/builds\/rightkit-build-control\/workspaces\/[a-f0-9]{64}$/);
  assert.equal(first.policyProfile, PROFILES.MANAGED_AGENT); assert.notEqual(first.namespace, debug.namespace);
  for (const value of Object.values(first).filter((value) => typeof value === 'string' && value.startsWith('/'))) assert.ok(value.startsWith('/Volumes/builds/rightkit-build-control/'));
  assert.equal(first.cargoHome, '/Volumes/builds/rightkit-build-control/global/cargo-home');
  assert.equal(first.compilerCache, '/Volumes/builds/rightkit-build-control/global/compiler-cache');
  assert.deepEqual(scanForbiddenLocalOutputs(['src', 'target', '.cargo-target', 'CMakeFiles', 'cmake-build-fast', 'DerivedData', 'README.md']), ['target', '.cargo-target', 'CMakeFiles', 'cmake-build-fast', 'DerivedData']);
  assert.ok(!LOCAL_OUTPUT_SCAN_POLICY.forbiddenNames.includes('node_modules'));
});
test('workspace scanner finds pre-existing & newly-created nested unmanaged outputs without following symlinks', async () => {
  const workspace = await mkdtemp(join(tmpdir(), 'rightkit-core-'));
  try {
    await Promise.all(['src-tauri/target', '.cache/foo-target', '.right-release/cache', 'nested/cmake-build-fast', 'node_modules/pkg'].map((path) => mkdir(join(workspace, path), { recursive: true })));
    await mkdir(join(workspace, 'new-crate/target'), { recursive: true });
    await symlink(join(workspace, 'src-tauri'), join(workspace, 'linked-src-tauri'));
    const findings = await scanWorkspaceForUnmanagedOutputs(workspace);
    assert.deepEqual(findings, ['.cache/foo-target', '.right-release/cache', 'nested/cmake-build-fast', 'new-crate/target', 'src-tauri/target']);
    assert.ok(!findings.some((path) => path.startsWith('linked-src-tauri/')));
  } finally { await rm(workspace, { recursive: true, force: true }); }
});
test('workspace scanner emits actionable, bounded receipt data', async () => {
  const workspace = await mkdtemp(join(tmpdir(), 'rightkit-core-'));
  try {
    await mkdir(join(workspace, 'crates/tool/target/debug'), { recursive: true });
    await mkdir(join(workspace, 'build-output'), { recursive: true });
    const receipt = await scanWorkspaceForUnmanagedOutputReceipt(workspace, { maxEntries: 20, maxDepth: 4, timeoutMs: 100, now: () => 0 });
    assert.deepEqual(receipt, {
      workspace, entriesScanned: 4, maxEntries: 20, maxDepth: 4, timeoutMs: 100,
      skippedDirectoryNames: LOCAL_OUTPUT_SCAN_POLICY.skippedDirectoryNames,
      findings: [
        { relativePath: 'build-output', path: join(workspace, 'build-output'), reason: LOCAL_OUTPUT_SCAN_POLICY.reason },
        { relativePath: 'crates/tool/target', path: join(workspace, 'crates/tool/target'), reason: LOCAL_OUTPUT_SCAN_POLICY.reason }
      ]
    });
  } finally { await rm(workspace, { recursive: true, force: true }); }
});
test('workspace scanner honors valid allowlist & fails closed on escapes or entry cap', async () => {
  const workspace = await mkdtemp(join(tmpdir(), 'rightkit-core-'));
  try {
    await mkdir(join(workspace, 'src-tauri/target'), { recursive: true }); await mkdir(join(workspace, 'build'), { recursive: true });
    assert.deepEqual(await scanWorkspaceForUnmanagedOutputs(workspace, { allowedRelativePaths: ['src-tauri/target'] }), ['build']);
    await assert.rejects(scanWorkspaceForUnmanagedOutputs(workspace, { allowedRelativePaths: ['../outside'] }), /escapes/);
    await assert.rejects(scanWorkspaceForUnmanagedOutputs(workspace, { maxEntries: 1 }), /entry cap/);
  } finally { await rm(workspace, { recursive: true, force: true }); }
});
test('workspace scanner skips dependency/vendor trees but still scans sibling source', async () => {
  const workspace = await mkdtemp(join(tmpdir(), 'rightkit-core-'));
  try {
    await mkdir(join(workspace, 'node_modules/pkg/target'), { recursive: true });
    await mkdir(join(workspace, 'vendor/crate/target'), { recursive: true });
    await mkdir(join(workspace, 'third_party/crate/target'), { recursive: true });
    await mkdir(join(workspace, 'app/target'), { recursive: true });
    assert.deepEqual(await scanWorkspaceForUnmanagedOutputs(workspace, { maxEntries: 4 }), ['app/target']);
  } finally { await rm(workspace, { recursive: true, force: true }); }
});
test('workspace scanner treats nested Git checkouts as separate workspaces', async () => {
  const workspace = await mkdtemp(join(tmpdir(), 'rightkit-core-'));
  try {
    await mkdir(join(workspace, 'repos/tool/.git'), { recursive: true });
    await mkdir(join(workspace, 'repos/tool/target'), { recursive: true });
    await mkdir(join(workspace, 'app/target'), { recursive: true });
    assert.deepEqual(await scanWorkspaceForUnmanagedOutputs(workspace), ['app/target']);
  } finally { await rm(workspace, { recursive: true, force: true }); }
});
test('workspace scanner fails closed on depth & elapsed-time caps, including hung filesystem calls', async () => {
  const workspace = await mkdtemp(join(tmpdir(), 'rightkit-core-'));
  try {
    await mkdir(join(workspace, 'one/two/target'), { recursive: true });
    await assert.rejects(scanWorkspaceForUnmanagedOutputs(workspace, { maxDepth: 1 }), /depth cap/);
    let calls = 0;
    await assert.rejects(scanWorkspaceForUnmanagedOutputs(workspace, { timeoutMs: 1, now: () => (++calls > 1 ? 2 : 0) }), /time cap/);
    await assert.rejects(scanWorkspaceForUnmanagedOutputs(workspace, { timeoutMs: 10, readdir: () => new Promise(() => {}) }), /time cap/);
  } finally { await rm(workspace, { recursive: true, force: true }); }
});
