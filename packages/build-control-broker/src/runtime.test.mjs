import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFile as nodeExecFile } from 'node:child_process';
import { cp, lstat, mkdir, mkdtemp, readFile, readdir, readlink, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { promisify } from 'node:util';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { OWNERSHIP_SCANNER_DIR, OWNERSHIP_SCANNER_ENTRIES, committedOwnershipScanner, RUNTIME_ACTIVATION_FENCE_SCHEMA, commitRuntimeActivationFence, functionalFailureDetail, functionalResourceFailure, healthProbeEndpoint, installManagedRuntime, loadRuntimeWindowsJobAddon, normalizeGitScopedPath, prepareRuntimeActivationFence, removeHealthProbeRoot, replaceRuntimeSelection, snapshotRuntimeSelection, stageManagedRuntime, syncDirectoryForPlatform } from './runtime.mjs';

const execFile = promisify(nodeExecFile);
const sourcePackages = fileURLToPath(new URL('../..', import.meta.url));
const canonicalTempDir = async (prefix) => realpath(await mkdtemp(join(tmpdir(), prefix)));
const git = async (cwd, args) => (await execFile('git', ['-C', cwd, ...args], { encoding: 'utf8' })).stdout.trim();

test('Git provenance scopes use forward slashes even from Windows paths', () => {
  assert.equal(normalizeGitScopedPath('tools\\rightkit\\packages'), 'tools/rightkit/packages');
  assert.equal(normalizeGitScopedPath('packages/build-control-broker'), 'packages/build-control-broker');
});

test('Windows runtime selection replaces an existing directory link recoverably', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'rk-win-select-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  await mkdir(join(root, 'runtime-old')); await mkdir(join(root, 'runtime-new'));
  const current = join(root, 'runtime-current'); const next = join(root, '.runtime-current-next');
  await symlink('runtime-old', current); await symlink('runtime-new', next);
  await replaceRuntimeSelection({ root, current, next, platform: 'win32' });
  assert.equal(await readlink(current), 'runtime-new');
  assert.equal((await readdir(root)).some((name) => name.startsWith('.runtime-current-displaced-')), false);
});

test('Windows directory fsync EPERM degrades while POSIX remains strict', async () => {
  const denied = async () => ({ sync: async () => { const error = new Error('directory fsync unsupported'); error.code = 'EPERM'; throw error; }, close: async () => {} });
  await syncDirectoryForPlatform('C:\\runtime', { platform: 'win32', openDirectory: denied });
  await assert.rejects(syncDirectoryForPlatform('/runtime', { platform: 'darwin', openDirectory: denied }), /directory fsync unsupported/);
});

test('Windows health synchronously loads Job Object addon from candidate runtime', () => {
  let addonPath;
  const addon = { assignRunningProcessToNewJob: () => {} };
  const loaded = loadRuntimeWindowsJobAddon('/managed/runtime-candidate', { loadAddon: (value) => { addonPath = value; return addon; } });
  assert.equal(loaded, addon);
  assert.match(addonPath, /runtime-candidate[\\/]build-control-broker[\\/]native[\\/]windows-job[\\/]build[\\/]Release[\\/]rightkit_windows_job\.node$/);
});

test('Windows health cleanup uses bounded retry only for probe root', async () => {
  let observed;
  await removeHealthProbeRoot('C:\\probe-root', { platform: 'win32', remove: async (path, options) => { observed = { path, options }; } });
  assert.deepEqual(observed, { path: 'C:\\probe-root', options: { recursive: true, force: true, maxRetries: 3, retryDelay: 100 } });
  observed = null;
  await removeHealthProbeRoot('/probe-root', { platform: 'darwin', remove: async (path, options) => { observed = { path, options }; } });
  assert.deepEqual(observed, { path: '/probe-root', options: { recursive: true, force: true } });
});

test('Windows health probes use unique named pipes while POSIX keeps socket paths', () => {
  assert.match(healthProbeEndpoint('win32', 'probe-1'), /^\\\\\.\\pipe\\rightkit-health-probe-1$/);
  assert.equal(healthProbeEndpoint('darwin', 'probe-1'), join(tmpdir(), 'rk-probe-1.health.sock'));
});

test('functional health surfaces FAILED receipt error and reason details', () => {
  assert.equal(functionalFailureDetail({ type: 'RESULT', exitCode: 1, status: 'FAILED', receipt: { error: 'addon rejected' } }), 'addon rejected');
  assert.equal(functionalFailureDetail({ type: 'RESULT', exitCode: 1, status: 'FAILED', receipt: { reason: 'digest mismatch' } }), 'digest mismatch');
});

test('native functional health requires a real resource snapshot', () => {
  assert.equal(functionalResourceFailure({ receipt: { resources: { processTree: { status: 'UNAVAILABLE', reason: 'addon-missing' } } } }), 'addon-missing');
  assert.equal(functionalResourceFailure({ receipt: { resources: { processTree: { status: 'OK' } } } }), 'resource-snapshot-invalid');
  assert.equal(functionalResourceFailure({ receipt: { resources: { processTree: { status: 'OK', cpuUserMs: 1, cpuKernelMs: 1, cpuTotalMs: 2, peakMemoryBytes: 1024, memoryKind: 'physical-footprint', totalProcesses: 1, activeProcesses: 0 } } } }), null);
});

async function sourceFixture() {
  const root = await canonicalTempDir('rk-runtime-source-'); const packages = join(root, 'packages'); const healthWorkspace = join(root, 'health');
  for (const name of ['build-control-broker', 'build-control-core', 'build-control-clients']) await cp(join(sourcePackages, name), join(packages, name), { recursive: true });
  await mkdir(join(packages, 'release'), { recursive: true });
  await cp(join(sourcePackages, 'release', 'managed-roots.mjs'), join(packages, 'release', 'managed-roots.mjs'));
  const scanner = await committedOwnershipScanner(sourcePackages, 'HEAD');
  for (const [file, bytes] of scanner.files) { await mkdir(dirname(join(packages, 'release', file)), { recursive: true }); await writeFile(join(packages, 'release', file), bytes); }
  await writeFile(join(packages, 'release', 'package.json'), scanner.releasePackageJson);
  const addonRelative = 'build-control-broker/native/windows-job/build/Release/rightkit_windows_job.node'; const addonPath = join(packages, addonRelative); const addonBytes = Buffer.from('MZ-fixture-rightkit-job-addon');
  await mkdir(join(packages, 'build-control-broker/native/windows-job/build/Release'), { recursive: true }); await writeFile(addonPath, addonBytes);
  const macAddonPath = join(packages, 'build-control-broker/native/macos-resource/build/Release/rightkit_macos_resource.node'); const macAddonBytes = Buffer.from('fixture-macos-resource-addon');
  await mkdir(join(packages, 'build-control-broker/native/macos-resource/build/Release'), { recursive: true }); await writeFile(macAddonPath, macAddonBytes);
  await mkdir(healthWorkspace); await writeFile(join(healthWorkspace, 'Cargo.toml'), '[package]\nname = "rk-runtime-health"\nversion = "0.0.0"\nedition = "2021"\n');
  await git(root, ['init']); await git(root, ['config', 'user.email', 'rightkit-test@example.invalid']); await git(root, ['config', 'user.name', 'RightKit Test']);
  await git(root, ['add', '-f', 'packages']); await git(root, ['commit', '-m', 'runtime fixture']);
  return { root, packages, healthWorkspace, macosResourceAddon: { path: macAddonPath, sha256: createHash('sha256').update(macAddonBytes).digest('hex'), sizeBytes: macAddonBytes.length }, windowsJobAddon: { path: addonPath, sha256: createHash('sha256').update(addonBytes).digest('hex'), sizeBytes: addonBytes.length }, revision: await git(root, ['rev-parse', 'HEAD']), tree: await git(root, ['rev-parse', 'HEAD:packages']) };
}

test('staging creates content-addressed immutable provenance-bound bundle without activation', async () => {
  const root = await canonicalTempDir('rk-runtime-'); const fixture = await sourceFixture(); try {
    await mkdir(join(fixture.packages, 'unrelated-release'));
    await writeFile(join(fixture.packages, 'unrelated-release', 'draft.mjs'), 'uncommitted unrelated work');
    const staged = await stageManagedRuntime({ externalRoot: root, sourcePackagesRoot: fixture.packages, version: 'release-a', sourceRevision: fixture.revision, sourceTreeDigest: fixture.tree, windowsJobAddon: fixture.windowsJobAddon, macosResourceAddon: fixture.macosResourceAddon });
    assert.equal(staged.version, 'release-a');
    const resolver = join(staged.runtime, 'release', 'managed-roots.mjs');
    assert.equal(await readFile(resolver, 'utf8'), await readFile(join(sourcePackages, 'release', 'managed-roots.mjs'), 'utf8'));
    await execFile(process.execPath, ['--input-type=module', '-e', `await import(${JSON.stringify(pathToFileURL(join(staged.runtime, 'build-control-clients', 'src', 'index.mjs')).href)})`], { cwd: root });
    assert.equal(staged.manifest.sourceRevision, fixture.revision); assert.equal(staged.manifest.sourceTreeDigest, fixture.tree);
    assert.equal((await readFile(join(staged.runtime, 'runtime-manifest.json'), 'utf8')).includes(staged.manifest.brokerDigest), true);
    assert.equal(JSON.parse(await readFile(join(staged.runtime, 'build-control-broker', 'node_modules', 'smol-toml', 'package.json'), 'utf8')).version, '1.7.0');
    assert.equal(normalizeGitScopedPath(await readlink(join(staged.runtime, 'build-control-broker', 'node_modules', '@rightkit', 'build-control-clients'))), '../../../build-control-clients');
    await assert.rejects(readlink(join(root, 'runtime-current')));
    assert.equal((await readdir(root)).some((name) => name.includes('.staging')), false);
  } finally { await rm(root, { recursive: true, force: true }); await rm(fixture.root, { recursive: true, force: true }); }
});

test('same source content returns on-disk release identity instead of requested conflicting label', async () => {
  const root = await canonicalTempDir('rk-runtime-'); const fixture = await sourceFixture(); try {
    const first = await stageManagedRuntime({ externalRoot: root, sourcePackagesRoot: fixture.packages, version: 'release-a', windowsJobAddon: fixture.windowsJobAddon, macosResourceAddon: fixture.macosResourceAddon });
    const second = await stageManagedRuntime({ externalRoot: root, sourcePackagesRoot: fixture.packages, version: 'release-b', windowsJobAddon: fixture.windowsJobAddon, macosResourceAddon: fixture.macosResourceAddon });
    assert.equal(second.runtime, first.runtime); assert.equal(second.version, 'release-a'); assert.equal(second.manifest.version, 'release-a');
  } finally { await rm(root, { recursive: true, force: true }); await rm(fixture.root, { recursive: true, force: true }); }
});

test('staging refuses dirty or untracked package source', async () => {
  const root = await canonicalTempDir('rk-runtime-'); const fixture = await sourceFixture(); try {
    await writeFile(join(fixture.packages, 'build-control-core', 'untracked-runtime-input.mjs'), 'export default 1;\n');
    await assert.rejects(stageManagedRuntime({ externalRoot: root, sourcePackagesRoot: fixture.packages, version: 'release-a' }), /dirty or contains untracked/);
  } finally { await rm(root, { recursive: true, force: true }); await rm(fixture.root, { recursive: true, force: true }); }
});

test('Windows staging requires and verifies the native Job Object addon descriptor', async () => {
  const root = await canonicalTempDir('rk-runtime-'); const fixture = await sourceFixture();
  try {
    const relative = 'build-control-broker/native/windows-job/build/Release/rightkit_windows_job.node';
    const addonPath = join(fixture.packages, relative); const bytes = Buffer.from('MZ-fake-rightkit-job-addon');
    await mkdir(join(fixture.packages, 'build-control-broker/native/windows-job/build/Release'), { recursive: true }); await writeFile(addonPath, bytes);
    await git(fixture.root, ['add', '-f', 'packages']); await git(fixture.root, ['commit', '-m', 'native Windows addon']);
    const descriptor = { path: addonPath, sha256: createHash('sha256').update(bytes).digest('hex'), sizeBytes: bytes.length };
    const staged = await stageManagedRuntime({ externalRoot: root, sourcePackagesRoot: fixture.packages, version: 'windows-addon', platform: 'win32', windowsJobAddon: descriptor });
    assert.deepEqual(staged.manifest.windowsJobAddon, { path: relative, sha256: descriptor.sha256, sizeBytes: bytes.length });
    assert.deepEqual(await readFile(join(staged.runtime, relative)), bytes);
    await assert.rejects(stageManagedRuntime({ externalRoot: root, sourcePackagesRoot: fixture.packages, version: 'windows-size-mismatch', platform: 'win32', windowsJobAddon: { ...descriptor, sizeBytes: bytes.length + 1 } }), /size mismatch/);
    await assert.rejects(stageManagedRuntime({ externalRoot: root, sourcePackagesRoot: fixture.packages, version: 'windows-mismatch', platform: 'win32', windowsJobAddon: { ...descriptor, sha256: '0'.repeat(64) } }), /digest mismatch/);
    const outside = join(fixture.root, 'outside-addon.node'); await writeFile(outside, bytes); await rm(addonPath); await symlink(outside, addonPath);
    await git(fixture.root, ['add', '-f', 'packages']); await git(fixture.root, ['commit', '-m', 'native Windows addon symlink']);
    await assert.rejects(stageManagedRuntime({ externalRoot: root, sourcePackagesRoot: fixture.packages, version: 'windows-symlink', platform: 'win32', windowsJobAddon: descriptor }), /regular file/);
    await assert.rejects(stageManagedRuntime({ externalRoot: root, sourcePackagesRoot: fixture.packages, version: 'windows-missing', platform: 'win32' }), /path, digest & size are required/);
  } finally { await rm(root, { recursive: true, force: true }); await rm(fixture.root, { recursive: true, force: true }); }
});

test('Mac staging requires a regular, digest-pinned native resource addon', { skip: process.platform !== 'darwin' }, async () => {
  const root = await canonicalTempDir('rk-mac-runtime-'); const fixture = await sourceFixture();
  try {
    const args = { externalRoot: root, sourcePackagesRoot: fixture.packages, version: 'mac-addon', platform: 'darwin' };
    const descriptor = fixture.macosResourceAddon;
    const staged = await stageManagedRuntime({ ...args, macosResourceAddon: descriptor });
    assert.equal(staged.manifest.macosResourceAddon.sha256, descriptor.sha256);
    assert.equal((await readFile(join(staged.runtime, staged.manifest.macosResourceAddon.path))).length, descriptor.sizeBytes);
    await assert.rejects(stageManagedRuntime(args), /path, digest & size are required/);
    await assert.rejects(stageManagedRuntime({ ...args, macosResourceAddon: { ...descriptor, sha256: '0'.repeat(64) } }), /digest mismatch/);
    await assert.rejects(stageManagedRuntime({ ...args, macosResourceAddon: { ...descriptor, sizeBytes: descriptor.sizeBytes + 1 } }), /size mismatch/);
  } finally { await rm(root, { recursive: true, force: true }); await rm(fixture.root, { recursive: true, force: true }); }
});

test('activation fence remains PENDING through candidate probe then rolls selection back without admission', async () => {
  const root = await canonicalTempDir('rk-runtime-'); const fixture = await sourceFixture(); try {
    const fence = join(root, 'state', 'activation-fence.json'); const prior = join(root, 'runtime-prior'); await cp(join(fixture.packages, 'build-control-core'), prior, { recursive: true }); await symlink('runtime-prior', join(root, 'runtime-current'));
    const configPath = join(root, 'config.json');
    const oldBytes = Buffer.from('\uFEFF{\n  "schemaVersion": 2\n}\n\n');
    await writeFile(configPath, oldBytes);
    const rollbackSnapshot = await snapshotRuntimeSelection({ root, configPath });
    await writeFile(configPath, '{"schemaVersion":2,"nodePath":"/new/node"}\n');
    let rollback = false; let stopped = false;
    await assert.rejects(installManagedRuntime({
      externalRoot: root, sourcePackagesRoot: fixture.packages, configPath, rollbackSnapshot, stopService: async () => { stopped = true; assert.notEqual(await readlink(join(root, 'runtime-current')), 'runtime-prior'); }, healthWorkspace: fixture.healthWorkspace, version: 'release-a', config: { broker: { externalRoot: { path: root }, activationFencePath: fence } },
      windowsJobAddon: fixture.windowsJobAddon, macosResourceAddon: fixture.macosResourceAddon, health: async () => {}, restartService: async ({ activation }) => {
        const state = JSON.parse(await readFile(activation.path, 'utf8')); assert.equal(state.schema, RUNTIME_ACTIVATION_FENCE_SCHEMA); assert.equal(state.state, 'PENDING');
        assert.equal(state.state === 'COMMITTED', false, 'candidate must reject SUBMIT/MANAGED_AGENT while PENDING'); return { pid: 101, runtimeIdentity: activation.runtime };
      }, probeService: async () => { throw new Error('post-switch probe refused'); }, rollbackService: async () => {
        assert.equal(stopped, true);
        assert.equal(await readlink(join(root, 'runtime-current')), 'runtime-prior');
        assert.deepEqual(await readFile(configPath), oldBytes);
        assert.equal(Object.hasOwn(JSON.parse(oldBytes.toString().replace(/^\uFEFF/, '')), 'nodePath'), false);
        rollback = true;
      }, processProbe: async () => ({ alive: true, runtimePath: root }),
    }), /activation rolled back/);
    assert.equal(rollback, true); assert.equal(await readlink(join(root, 'runtime-current')), 'runtime-prior'); await assert.rejects(readFile(fence));
  } finally { await rm(root, { recursive: true, force: true }); await rm(fixture.root, { recursive: true, force: true }); }
});

test('activation commits only after exact post-switch process proof', async () => {
  const root = await canonicalTempDir('rk-runtime-'); const fixture = await sourceFixture(); try {
    const fence = join(root, 'state', 'activation-fence.json'); let runtime;
    await mkdir(join(root, 'runtime-prior')); await symlink('runtime-prior', join(root, 'runtime-current'));
    const configPath = join(root, 'config.json');
    await writeFile(configPath, '{"schemaVersion":2}\n');
    const rollbackSnapshot = await snapshotRuntimeSelection({ root, configPath });
    const candidateBytes = '{"schemaVersion":2,"defaults":{"cargoBuildJobs":7}}\n';
    await writeFile(configPath, candidateBytes);
    const result = await installManagedRuntime({
      externalRoot: root, sourcePackagesRoot: fixture.packages, configPath, rollbackSnapshot, stopService: async () => { throw new Error('must not stop committed candidate'); }, healthWorkspace: fixture.healthWorkspace, version: 'release-a', config: { broker: { externalRoot: { path: root }, activationFencePath: fence } }, windowsJobAddon: fixture.windowsJobAddon, macosResourceAddon: fixture.macosResourceAddon, health: async () => {},
      restartService: async ({ manifest, runtime: candidate }) => { runtime = candidate; return { pid: 101, runtimeIdentity: manifest }; },
      probeService: async ({ manifest }) => ({ pid: 101, runtimeIdentity: manifest }), rollbackService: async () => { throw new Error('must not rollback committed candidate'); }, processProbe: async () => ({ alive: true, runtimePath: runtime }),
    });
    assert.equal(result.version, 'release-a'); assert.deepEqual(JSON.parse(await readFile(fence, 'utf8')).state, 'COMMITTED');
    assert.equal(await readFile(configPath, 'utf8'), candidateBytes);
    await assert.rejects(readFile(rollbackSnapshot.path), { code: 'ENOENT' });
  } finally { await rm(root, { recursive: true, force: true }); await rm(fixture.root, { recursive: true, force: true }); }
});

test('failed candidate stop preserves pending fence & never restores a mixed config/runtime pair', async (t) => {
  const root = await canonicalTempDir('rk-runtime-stop-'); const fixture = await sourceFixture();
  t.after(async () => { await rm(root, { recursive: true, force: true }); await rm(fixture.root, { recursive: true, force: true }); });
  await mkdir(join(root, 'runtime-prior')); await symlink('runtime-prior', join(root, 'runtime-current'));
  const configPath = join(root, 'config.json'); const fence = join(root, 'state', 'activation-fence.json');
  await writeFile(configPath, '{"schemaVersion":2}\n');
  const rollbackSnapshot = await snapshotRuntimeSelection({ root, configPath });
  const candidateBytes = '{"schemaVersion":2,"defaults":{"cargoBuildJobs":7}}\n';
  await writeFile(configPath, candidateBytes);
  let rollbackCalled = false;
  await assert.rejects(installManagedRuntime({
    externalRoot: root, sourcePackagesRoot: fixture.packages, configPath, rollbackSnapshot,
    healthWorkspace: fixture.healthWorkspace, version: 'stop-failure', config: { broker: { externalRoot: { path: root }, activationFencePath: fence } },
    windowsJobAddon: fixture.windowsJobAddon, macosResourceAddon: fixture.macosResourceAddon, health: async () => {},
    restartService: async ({ manifest }) => ({ pid: 101, runtimeIdentity: manifest }),
    probeService: async () => { throw new Error('candidate probe failed'); },
    stopService: async () => { throw new Error('candidate still running'); },
    rollbackService: async () => { rollbackCalled = true; }, processProbe: async () => ({ alive: true }),
  }), /selected candidate without rollback: candidate still running/);
  assert.equal(rollbackCalled, false);
  assert.notEqual(await readlink(join(root, 'runtime-current')), 'runtime-prior');
  assert.equal(await readFile(configPath, 'utf8'), candidateBytes);
  assert.equal(JSON.parse(await readFile(fence, 'utf8')).state, 'PENDING');
  assert.equal(JSON.parse(await readFile(rollbackSnapshot.path, 'utf8')).prior, 'runtime-prior');
});

test('fence token prevents stale or substituted pending state from commit', async () => {
  const root = await canonicalTempDir('rk-runtime-'); try {
    const manifest = { schema: 'rightkit.build-control.runtime.v2', version: 'release-a', clientDigest: 'a'.repeat(64), brokerDigest: 'b'.repeat(64) };
    const activation = await prepareRuntimeActivationFence({ path: join(root, 'activation.json'), manifest });
    await commitRuntimeActivationFence(activation);
    assert.equal(JSON.parse(await readFile(activation.path, 'utf8')).state, 'COMMITTED');
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('failed staging removes its unique staging directory', async () => {
  const root = await canonicalTempDir('rk-runtime-'); try {
    await assert.rejects(stageManagedRuntime({ externalRoot: root, sourcePackagesRoot: join(root, 'missing'), version: 'bad-v1' }));
    assert.deepEqual(await readdir(root), []);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('staged runtime carries a loadable ownership scanner and its catalog, bound into the broker digest', async () => {
  const root = await canonicalTempDir('rk-runtime-'); const fixture = await sourceFixture(); try {
    const staged = await stageManagedRuntime({ externalRoot: root, sourcePackagesRoot: fixture.packages, version: 'scanner', sourceRevision: fixture.revision, sourceTreeDigest: fixture.tree, windowsJobAddon: fixture.windowsJobAddon, macosResourceAddon: fixture.macosResourceAddon });
    const scannerRoot = join(staged.runtime, OWNERSHIP_SCANNER_DIR);
    for (const file of OWNERSHIP_SCANNER_ENTRIES) assert.ok(await lstat(join(scannerRoot, file)), `${file} staged`);
    const scanner = await import(pathToFileURL(join(scannerRoot, 'ownership.mjs')).href);
    const catalog = JSON.parse(await readFile(join(scannerRoot, 'rightkit-ownership.json'), 'utf8'));
    assert.equal(typeof scanner.checkApp, 'function');
    assert.ok(catalog.capabilities.length > 0);
    // Uncommitted scanner edits neither block staging nor leak into the runtime.
    await writeFile(join(fixture.packages, 'release', 'ownership.mjs'), 'throw new Error("uncommitted");\n');
    const again = await stageManagedRuntime({ externalRoot: root, sourcePackagesRoot: fixture.packages, version: 'scanner-dirty', sourceRevision: fixture.revision, sourceTreeDigest: fixture.tree, windowsJobAddon: fixture.windowsJobAddon, macosResourceAddon: fixture.macosResourceAddon });
    assert.equal(again.runtime, staged.runtime);
  } finally { await rm(root, { recursive: true, force: true }); await rm(fixture.root, { recursive: true, force: true }); }
});

test('candidate health waits for data initialization & fails closed on unreadable data or timeout', async () => {
  const { waitForBrokerDataReady } = await import('./runtime.mjs');
  let calls = 0; let clock = 0;
  const pending = { type: 'STATUS', state: 'UNAVAILABLE', code: 'RK_DATA_ROOT_INITIALIZING' };
  const status = await waitForBrokerDataReady({ timeoutMs: 100, now: () => clock, sleep: async (ms) => { clock += ms; }, query: async () => ++calls === 1 ? pending : { type: 'STATUS', state: 'READY' } });
  assert.equal(status.state, 'READY'); assert.equal(calls, 2);
  await assert.rejects(waitForBrokerDataReady({ timeoutMs: 100, query: async () => ({ type: 'STATUS', code: 'EXTERNAL_ROOT_UNREADABLE' }) }), { code: 'EXTERNAL_ROOT_UNREADABLE' });
  clock = 0;
  await assert.rejects(waitForBrokerDataReady({ timeoutMs: 25, now: () => clock, sleep: async (ms) => { clock += ms; }, query: async () => pending }), /timed out/);
});
