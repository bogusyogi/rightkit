import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { isAbsolute, join, parse } from 'node:path';
import { BUILD_CONTROL_PROTOCOL, GITHUB_ACTIONS_ONLY, GITHUB_ACTIONS_ONLY_ERROR, LOCAL_OUTPUT_SCAN_POLICY, MANAGED_RUST_ENVIRONMENT_KEYS, PROFILES, RUNTIME_MANIFEST_SCHEMA, SOURCE_RUNTIME_IDENTITY, assertRepositoryExecutionAllowed, captureDelegatedCompilerEnvironment, captureManagedRustEnvironment, externalRoots, immutableToolRoot, injectBrokerEnvironment, normalizeBuildRequest, normalizeCargoTargetArgv, normalizeCompilerOutputArgv, normalizeDelegatedCompilerEnvironment, normalizeManagedAgentRequest, normalizeRequestEnvironment, normalizeRuntimeIdentity, repositoryExecutionPolicy, scanForbiddenLocalOutputs, scanWorkspaceForUnmanagedOutputReceipt, scanWorkspaceForUnmanagedOutputs, validateManagedRustEnvironment, validateManagedRustupInvocation } from './index.mjs';

const fixturePath = (...segments) => join(parse(tmpdir()).root, ...segments);

test('v2 runtime identity requires immutable client & broker digests', () => {
  assert.equal(BUILD_CONTROL_PROTOCOL, 'rightkit.build-control.client.v2'); assert.equal(SOURCE_RUNTIME_IDENTITY.schema, RUNTIME_MANIFEST_SCHEMA);
  assert.deepEqual(normalizeRuntimeIdentity(SOURCE_RUNTIME_IDENTITY), SOURCE_RUNTIME_IDENTITY);
  assert.throws(() => normalizeRuntimeIdentity({ ...SOURCE_RUNTIME_IDENTITY, clientDigest: 'mutable' }), /runtime v2 identity/);
});

test('Orthic public repositories fail closed outside GitHub Actions', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rightkit-public-policy-'));
  try {
    await mkdir(join(root, '.git'));
    await writeFile(join(root, '.git', 'config'), '[remote "origin"]\n\turl = https://github.com/Orthic-Labs/example.git\n');
    assert.throws(() => repositoryExecutionPolicy(root), (error) => error.code === 'RK_EXECUTION_POLICY_MISSING');
    await writeFile(join(root, '.rightgit.json'), `${JSON.stringify({ compile: GITHUB_ACTIONS_ONLY })}\n`);
    assert.equal(repositoryExecutionPolicy(root).compile, GITHUB_ACTIONS_ONLY);
    assert.throws(() => assertRepositoryExecutionAllowed({ workspace: root, environment: {}, operation: 'cargo test' }), (error) => error.code === GITHUB_ACTIONS_ONLY_ERROR);
    assert.equal(assertRepositoryExecutionAllowed({ workspace: root, environment: { GITHUB_ACTIONS: 'true' } }).compile, GITHUB_ACTIONS_ONLY);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('normalizes registered request without executable authority', () => {
  const workspace = fixturePath('external', 'work');
  const request = normalizeBuildRequest({ frontend: 'cargo', argv: ['test'], workspace, environment: { CI: '1' } });
  assert.deepEqual(request, { frontend: 'cargo', argv: ['test'], workspace, environment: { CI: '1' } });
  assert.throws(() => normalizeBuildRequest({ frontend: 'unknown', argv: [], workspace }));
});
test('immutable tool roots reject mutable or escaping paths', () => {
  const root = fixturePath('external', 'tools'); const cargo = join(root, 'cargo');
  assert.equal(immutableToolRoot({ path: cargo, root, sha256: 'a'.repeat(64) }).path, cargo);
  assert.throws(() => immutableToolRoot({ path: fixturePath('external', 'cargo'), root, sha256: 'a'.repeat(64) }));
  if (process.platform === 'win32') assert.throws(() => immutableToolRoot({ path: 'D:\\external\\cargo.exe', root: 'C:\\external\\tools', sha256: 'a'.repeat(64) }));
});
test('managed-agent normalization rejects redirect vectors & unmanaged tools', () => {
  const request = { frontend: 'cargo', argv: ['test'], workspace: fixturePath('workspace'), environment: { CI: '1' }, managedToolRoot: fixturePath('managed', 'tools') };
  const normalized = normalizeManagedAgentRequest({ ...request, configuration: { net: { retry: 2 } } });
  assert.equal(normalized.profile, PROFILES.MANAGED_AGENT);
  assert.equal(normalized.managedToolRoot, request.managedToolRoot);
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
  assert.throws(() => normalizeManagedAgentRequest({ ...request, argv: [fixturePath('usr', 'local', 'bin', 'cargo'), 'test'] }), /redirect/);
  assert.throws(() => normalizeManagedAgentRequest({ ...request, argv: [join(fixturePath('usr', 'local', 'bin'), 'cargo.exe'), 'test'] }), /redirect/);
  if (process.platform === 'win32') assert.throws(() => normalizeManagedAgentRequest({ ...request, argv: ['D:\\unmanaged\\cargo.exe', 'test'] }), /redirect/);
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

test('request environment is allowlisted while broker runtime injection wins', () => {
  assert.deepEqual(normalizeRequestEnvironment({ ci: '1', RUST_LOG: 'warn', TERM: 'xterm-256color', __CF_USER_TEXT_ENCODING: '0x1F5:0x0:0x0' }), { CI: '1', RUST_LOG: 'warn', TERM: 'xterm-256color', __CF_USER_TEXT_ENCODING: '0x1F5:0x0:0x0' });
  for (const key of ['DYLD_INSERT_LIBRARIES', 'DYLD_LIBRARY_PATH', 'LD_PRELOAD', 'LD_AUDIT', 'NODE_OPTIONS', 'RUSTC_WRAPPER', 'RUSTC', 'RUSTFLAGS', 'CARGO_BUILD_RUSTC', 'CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER', 'CC', 'CXX', 'AR', 'PATH', 'HOME', 'TOKEN_FILE']) {
    assert.throws(() => normalizeRequestEnvironment({ [key]: '/untrusted' }), /broker-controlled|allowlisted/);
  }
  assert.throws(() => normalizeRequestEnvironment({ CI: '1', ci: '0' }), /duplicate/);
  assert.throws(() => normalizeRequestEnvironment({ CI: 'safe\0unsafe' }), /without NUL/);
  assert.deepEqual(injectBrokerEnvironment({ requestEnvironment: { CI: 'untrusted', RUST_LOG: 'info' }, runtimeEnvironment: { CI: 'broker', CARGO_TARGET_DIR: '/broker/target', npm_config_cache: '/broker/npm', PATH: '/broker/bin' } }), {
    CI: 'broker', RUST_LOG: 'info', CARGO_TARGET_DIR: '/broker/target', npm_config_cache: '/broker/npm', PATH: '/broker/bin'
  });
});

test('delegated compiler environment preserves Cargo semantics without process authority', () => {
  const captured = captureDelegatedCompilerEnvironment({ CARGO_PKG_VERSION: '1.2.3', OUT_DIR: '/managed/target/out', CUSTOM_BUILD_VALUE: 'yes', PATH: '/untrusted', RUSTDOC: '/untrusted/rustdoc', RUSTC_BOOTSTRAP: '1', CARGO_MAKEFLAGS: '-j --jobserver-fds=4,6', RIGHTKIT_PARENT_REQUEST_ID: 'parent', __CF_USER_TEXT_ENCODING: '0x1F5:0x0:0x0' });
  assert.deepEqual(captured, { CARGO_PKG_VERSION: '1.2.3', OUT_DIR: '/managed/target/out', CUSTOM_BUILD_VALUE: 'yes' });
  assert.deepEqual(normalizeDelegatedCompilerEnvironment(captured), captured);
  assert.deepEqual(injectBrokerEnvironment({ requestEnvironment: captured, runtimeEnvironment: { PATH: '/broker/bin' }, normalizeEnvironment: normalizeDelegatedCompilerEnvironment }), { ...captured, PATH: '/broker/bin' });
  assert.deepEqual(normalizeDelegatedCompilerEnvironment({ __CF_USER_TEXT_ENCODING: '0x1F5:0x0:0x0' }), {});
  for (const key of ['PATH', 'RUSTDOC', 'RUSTC_BOOTSTRAP', 'CARGO_MAKEFLAGS', 'DYLD_LIBRARY_PATH', 'RIGHTKIT_PARENT_REQUEST_ID', 'SCCACHE_SERVER_UDS']) assert.throws(() => normalizeDelegatedCompilerEnvironment({ [key]: '/untrusted' }), /broker-controlled/);
});

test('raw rustc & rustdoc outputs are broker-owned regardless of caller paths', () => {
  const root = fixturePath('broker', 'layout', 'raw-output'); const targetTriple = 'aarch64-apple-darwin';
  assert.deepEqual(normalizeCompilerOutputArgv(['src/main.rs', '-o', 'product/bin', '--out-dir=/product/target', '--emit=dep-info=product/main.d,link=/product/main'], { compiler: 'rustc', outputRoot: root, targetTriple }), [
    'src/main.rs', '-o', join(root, 'bin'), `--out-dir=${root}`, `--emit=dep-info=${join(root, 'main.d')},link=${join(root, 'main')}`, '--target', targetTriple
  ]);
  assert.deepEqual(normalizeCompilerOutputArgv(['lib.rs', '-oapp', '--emit', 'metadata,mir=../../product/app.mir'], { compiler: 'rustc', outputRoot: root, targetTriple }), [
    'lib.rs', '-o', join(root, 'app'), '--emit', `metadata,mir=${join(root, 'app.mir')}`, '--out-dir', root, '--target', targetTriple
  ]);
  assert.deepEqual(normalizeCompilerOutputArgv(['doc.rs', '--output', '../site', '--emit=link'], { compiler: 'rustdoc', outputRoot: root, targetTriple }), [
    'doc.rs', '--output', join(root, 'site'), '--emit=link', '--out-dir', root, '--target', targetTriple
  ]);
  assert.deepEqual(normalizeCompilerOutputArgv(['src.rs', '-C', 'incremental=/tmp/escape', '-Cprofile-generate=/tmp/profile'], { compiler: 'rustc', outputRoot: root, targetTriple }), ['src.rs', '-C', `incremental=${join(root, 'incremental')}`, `-Cprofile-generate=${join(root, 'profile')}`, '--out-dir', root, '--target', targetTriple]);
  assert.throws(() => normalizeCompilerOutputArgv(['src.rs', '-Clinker=/tmp/evil'], { compiler: 'rustc', outputRoot: root, targetTriple }), /authority is forbidden/);
  assert.throws(() => normalizeCompilerOutputArgv(['src.rs', '-o'], { compiler: 'rustc', outputRoot: root, targetTriple }), /requires an output path/);
  assert.throws(() => normalizeCompilerOutputArgv(['src.rs', '--emit=dep-info='], { compiler: 'rustc', outputRoot: root, targetTriple }), /must name a file/);
  assert.throws(() => normalizeCompilerOutputArgv(['src.rs'], { compiler: 'cc', outputRoot: root, targetTriple }), /rustc or rustdoc/);
});

test('Cargo target normalization injects broker target & rejects caller control', () => {
  assert.deepEqual(normalizeCargoTargetArgv(['test'], 'aarch64-apple-darwin'), ['test', '--target', 'aarch64-apple-darwin']);
  assert.deepEqual(normalizeCargoTargetArgv(['build', '--target=aarch64-apple-darwin'], 'aarch64-apple-darwin'), ['build', '--target=aarch64-apple-darwin']);
  assert.deepEqual(normalizeCargoTargetArgv(['test', '--', '--target', 'passed-to-test'], 'aarch64-apple-darwin'), ['test', '--target', 'aarch64-apple-darwin', '--', '--target', 'passed-to-test']);
  assert.deepEqual(normalizeCargoTargetArgv(['test', '--', '--target-dir', 'passed-to-test'], 'aarch64-apple-darwin', '/managed/target'), ['test', '--target', 'aarch64-apple-darwin', '--target-dir', '/managed/target', '--', '--target-dir', 'passed-to-test']);
  assert.deepEqual(normalizeCargoTargetArgv(['build', '--target-dir=/old/generation'], 'aarch64-apple-darwin', '/managed/target'), ['build', '--target-dir=/managed/target', '--target', 'aarch64-apple-darwin']);
  assert.throws(() => normalizeCargoTargetArgv(['test', '--target', 'x86_64-apple-darwin'], 'aarch64-apple-darwin'), /must match/);
  assert.throws(() => normalizeCargoTargetArgv(['test', '--target'], 'aarch64-apple-darwin'), /requires a target triple/);
});

test('rustup administration stays available but rustup-run build tools fail closed', () => {
  assert.doesNotThrow(() => validateManagedRustupInvocation('rustup', ['toolchain', 'list']));
  assert.throws(() => validateManagedRustupInvocation('rustup', ['run', 'stable', 'cargo', 'test']), /cannot launch/);
  assert.throws(() => validateManagedRustupInvocation('rustup', ['--verbose', 'run', 'stable', 'rustc.exe']), /cannot launch/);
});
test('external roots are deterministic & output scan policy identifies local payload roots', () => {
  const externalRoot = fixturePath('Volumes', 'builds');
  const input = { externalRoot, repositoryIdentity: 'git:abc123', toolchain: 'rust-1.97.1', target: 'aarch64-apple-darwin', buildProfile: 'release', policyProfile: PROFILES.MANAGED_AGENT };
  const first = externalRoots(input); const second = externalRoots(input);
  const debug = externalRoots({ ...input, buildProfile: 'debug', policyProfile: PROFILES.STRICT_SYSTEM });
  const otherArch = externalRoots({ ...input, target: 'x86_64-apple-darwin' });
  const otherRepo = externalRoots({ ...input, repositoryIdentity: 'git:def456' });
  const otherToolchain = externalRoots({ ...input, toolchain: 'rust-1.98.0' });
  assert.deepEqual(first, second); assert.equal(first.root, join(externalRoot, 'rightkit-build-control', 'workspaces', first.namespace)); assert.match(first.namespace, /^[a-f0-9]{16}$/);
  assert.equal(first.policyProfile, PROFILES.MANAGED_AGENT);
  // Cargo profile & target triple must not split the namespace; Cargo separates them internally.
  assert.equal(first.namespace, debug.namespace); assert.equal(first.namespace, otherArch.namespace);
  assert.notEqual(first.namespace, otherRepo.namespace); assert.notEqual(first.namespace, otherToolchain.namespace);
  const buildControlRoot = join(externalRoot, 'rightkit-build-control');
  for (const value of Object.values(first).filter((value) => typeof value === 'string' && isAbsolute(value))) assert.ok(value.startsWith(buildControlRoot));
  assert.equal(first.cargoHome, join(buildControlRoot, 'global', 'cargo-home'));
  assert.equal(first.compilerCache, join(buildControlRoot, 'global', 'compiler-cache'));
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
