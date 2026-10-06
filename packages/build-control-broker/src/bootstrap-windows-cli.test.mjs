import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { loadBrokerConfig } from './config.mjs';
import { defaultBounds, defaultCargoBuildJobs, ensureRemoteRoutingRoots, isDirectMain, parseArgs, shimTable, stageManagedTools, supportsClientSideSccache, whereUnmanaged } from './bootstrap-windows-cli.mjs';

// discoverTools/where/buildConfig shell out to `where.exe`, which does not
// exist on this Mac — those three are exercised only by a real Windows run.
// Everything below is pure logic or the config *shape*, both fully verifiable
// here; the shape assertion below is load-bearing: it proves a config built
// the way bootstrap-windows-cli.mjs's buildConfig() constructs one is
// actually accepted by the broker's own schema, not just plausible-looking.

test('parseArgs accepts the documented flags & rejects anything else', () => {
  assert.deepEqual(parseArgs([]), { force: false, drainTimeoutMs: 900_000 });
  assert.deepEqual(parseArgs(['--force']), { force: true, drainTimeoutMs: 900_000 });
  assert.equal(parseArgs(['--drain-timeout-ms', '0']).drainTimeoutMs, 0);
  assert.throws(() => parseArgs(['--drain-timeout-ms', '-1']), /non-negative integer/);
  assert.deepEqual(parseArgs(['--external-root', 'D:\\x']), { force: false, drainTimeoutMs: 900_000, externalRoot:'D:\\x' });
  assert.throws(() => parseArgs(['--bogus']), /unknown bootstrap argument/);
});

test('direct-main detection is portable across Windows and POSIX path forms', () => {
  assert.equal(isDirectMain({ moduleUrl: 'file:///C:/RightKit/bootstrap-windows-cli.mjs', argv1: 'C:\\RightKit\\bootstrap-windows-cli.mjs', platform: 'win32' }), true);
  assert.equal(isDirectMain({ moduleUrl: 'file:///C:/RightKit/bootstrap-windows-cli.mjs', argv1: 'C:\\Other\\bootstrap-windows-cli.mjs', platform: 'win32' }), false);
  assert.equal(isDirectMain({ moduleUrl: 'file:///tmp/bootstrap-windows-cli.mjs', argv1: '/tmp/bootstrap-windows-cli.mjs', platform: 'linux' }), true);
  assert.equal(isDirectMain({ moduleUrl: 'file:///tmp/bootstrap-windows-cli.mjs', argv1: '', platform: 'linux' }), false);
});

test('bootstrap creates remote snapshot inbox before first scp transfer', (t) => {
  const scratch = mkdtempSync(join(tmpdir(), 'rk-win-remote-inputs-'));
  t.after(() => rmSync(scratch, { recursive: true, force: true }));
  const inputs = ensureRemoteRoutingRoots(scratch);
  assert.equal(inputs, join(scratch, 'remote-inputs'));
  assert.equal(existsSync(inputs), true);
});

test('shimTable only maps shims for tools actually discovered, mirroring the macOS table', () => {
  assert.deepEqual(shimTable([{ id: 'cargo' }, { id: 'gcc' }]), { cargo: 'cargo', gcc: 'gcc' });
  assert.deepEqual(shimTable([]), {});
});

test('Windows Cargo-job derivation uses the shared host sizing rule', () => {
  // Shared host sizing: every logical CPU unless 80 % of RAM at 1 GiB/job binds first.
  assert.equal(defaultCargoBuildJobs({ cpus: 20, totalMemoryBytes: 64 * 1024 ** 3 }), 20);
  assert.equal(defaultCargoBuildJobs({ cpus: 12, totalMemoryBytes: 24 * 1024 ** 3 }), 12);
  assert.equal(defaultCargoBuildJobs({ cpus: 8, totalMemoryBytes: 6 * 1024 ** 3 }), 4);
  assert.equal(defaultCargoBuildJobs({ cpus: 2, totalMemoryBytes: 1 * 1024 ** 3 }), 1);
  assert.equal(defaultCargoBuildJobs({ cpus: 0, totalMemoryBytes: 0 }), 1);
});

test('Windows tool discovery skips installed RightKit agent shims', async (t) => {
  const scratch = mkdtempSync(join(tmpdir(), 'rk-win-where-'));
  t.after(() => rmSync(scratch, { recursive: true, force: true }));
  const shimDir = join(scratch, 'agent-bin'); const realDir = join(scratch, 'toolchain');
  mkdirSync(shimDir); mkdirSync(realDir);
  const shim = join(shimDir, 'cargo.exe'); const real = join(realDir, 'cargo.exe');
  writeFileSync(join(shimDir, '.rightkit-managed-agent-shims.json'), '{}');
  writeFileSync(shim, 'shim'); writeFileSync(real, 'cargo');
  const selected = await whereUnmanaged('cargo.exe', async () => ({ stdout: `${shim}\r\n${real}\r\n` }));
  assert.equal(selected, real);
});

test('staging preserves bundled tools & copies standalone Cargo tools', async (t) => {
  const scratch = mkdtempSync(join(tmpdir(), 'rk-win-stage-tools-'));
  t.after(() => rmSync(scratch, { recursive: true, force: true }));
  const rustcPath = join(scratch, 'toolchain', 'bin', 'rustc.exe');
  const upgradePath = join(scratch, 'cargo-upgrade.exe');
  mkdirSync(join(scratch, 'toolchain', 'bin'), { recursive: true });
  writeFileSync(rustcPath, 'rustc');
  writeFileSync(upgradePath, 'upgrade');
  const tools = [
    { id: 'rustc', path: rustcPath, root: join(scratch, 'toolchain', 'bin'), sha256: createHash('sha256').update(readFileSync(rustcPath)).digest('hex') },
    { id: 'cargo-upgrade', path: upgradePath, root: scratch, sha256: createHash('sha256').update(readFileSync(upgradePath)).digest('hex') },
  ];
  const [rustc, upgrade] = await stageManagedTools(tools, join(scratch, 'external'));
  assert.equal(rustc.path, rustcPath);
  assert.notEqual(upgrade.path, upgradePath);
  assert.equal(existsSync(upgrade.path), true);
});

test('defaultBounds matches the same doc-mandated shape shipped on macOS', () => {
  const bounds = defaultBounds();
  assert.equal(bounds.jobDeadlineMs, 0);
  assert.equal(bounds.buildConcurrency, 3);
  assert.equal(bounds.workspaceDiscoveryTimeoutMs, 30_000);
  assert.ok(bounds.targetCacheCapBytes > 0);
  assert.equal(bounds.maintenanceConcurrency, undefined);
});

test('the Windows-shaped config buildConfig() constructs is accepted by the broker’s own schema', async (t) => {
  const scratch = mkdtempSync(join(tmpdir(), 'rk-win-bootstrap-'));
  t.after(() => rmSync(scratch, { recursive: true, force: true }));
  // Hand-built to the exact shape buildConfig() in bootstrap-windows-cli.mjs
  // produces, with one fake tool standing in for a real `where.exe` discovery.
  const config = {
    schemaVersion: 2,
    broker: {
      externalRoot: { path: scratch, volumeId: 'unknown', mounted: true, writable: true, internal: false },
      capacityBytes: 200 * 1024 ** 3,
      stateFile: join(scratch, 'state', 'broker.json'),
      activationFencePath: join(scratch, 'state', 'runtime-activation.json'),
      receiptsDir: join(scratch, 'receipts'),
    },
    windowsPipe: '\\\\.\\pipe\\rightkit-build-control',
    tools: [
      { id: 'cargo', path: join(scratch, 'cargo.exe'), root: scratch, sha256: 'a'.repeat(64) },
      { id: 'rustc', path: join(scratch, 'rustc.exe'), root: scratch, sha256: 'b'.repeat(64) },
      { id: 'rustdoc', path: join(scratch, 'rustdoc.exe'), root: scratch, sha256: 'c'.repeat(64) },
    ],
    shims: { cargo: 'cargo', rustc: 'rustc', rustdoc: 'rustdoc' },
    externalCommands: [],
    defaults: {
      toolchain: 'stable-x86_64-pc-windows-msvc', targetTriple: 'x86_64-pc-windows-msvc', targetTriples: ['x86_64-pc-windows-msvc'],
      buildProfile: 'development', rustcToolId: 'rustc', rustdocToolId: 'rustdoc', cacheMode: 'disabled', cargoBuildJobs: defaultCargoBuildJobs(),
    },
    sccache: { mode: 'disabled', lifecycleOwner: null, requireOwnershipProof: false, maxSizeBytes: 20 * 1024 ** 3 },
    outputPolicy: { mode: 'enforce', allowedSourceDirs: [], scanRoot: 'repository' },
    bounds: defaultBounds(),
  };
  const configPath = join(scratch, 'config.json');
  writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`);
  const loaded = await loadBrokerConfig(configPath);
  assert.equal(loaded.windowsPipe, '\\\\.\\pipe\\rightkit-build-control');
  assert.equal(loaded.sccache.mode, 'disabled');
  assert.equal(loaded.bounds.buildConcurrency, 3);
  assert.equal(loaded.bounds.analysisConcurrency, 2);
});

test('a config with no rustc/rustdoc discovered omits rustcToolId/rustdocToolId rather than pointing at an unregistered tool', async (t) => {
  // Regression test: buildConfig() originally set these unconditionally, which
  // rejected the whole config whenever rustc/rustdoc were not on PATH — caught
  // only by testing a fixture that (correctly) mirrors an incomplete toolchain.
  const scratch = mkdtempSync(join(tmpdir(), 'rk-win-bootstrap-'));
  t.after(() => rmSync(scratch, { recursive: true, force: true }));
  const config = {
    schemaVersion: 2,
    broker: {
      externalRoot: { path: scratch, volumeId: 'unknown', mounted: true, writable: true, internal: false },
      capacityBytes: 200 * 1024 ** 3,
      stateFile: join(scratch, 'state', 'broker.json'),
      activationFencePath: join(scratch, 'state', 'runtime-activation.json'),
      receiptsDir: join(scratch, 'receipts'),
    },
    windowsPipe: '\\\\.\\pipe\\rightkit-build-control',
    tools: [{ id: 'pnpm', path: join(scratch, 'pnpm.exe'), root: scratch, sha256: 'a'.repeat(64) }],
    shims: { pnpm: 'pnpm' },
    externalCommands: [],
    defaults: { toolchain: 'stable-x86_64-pc-windows-msvc', targetTriple: 'x86_64-pc-windows-msvc', targetTriples: ['x86_64-pc-windows-msvc'], buildProfile: 'development', cacheMode: 'disabled', cargoBuildJobs: defaultCargoBuildJobs() },
    sccache: { mode: 'disabled', lifecycleOwner: null, requireOwnershipProof: false, maxSizeBytes: 20 * 1024 ** 3 },
    outputPolicy: { mode: 'enforce', allowedSourceDirs: [], scanRoot: 'repository' },
    bounds: defaultBounds(),
  };
  const configPath = join(scratch, 'config.json');
  writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`);
  const loaded = await loadBrokerConfig(configPath);
  assert.equal(loaded.defaults.rustcToolId, undefined);
  assert.equal(loaded.defaults.rustdocToolId, undefined);
});

test('Windows Cargo jobs are no longer capped at a fixed sixteen', () => {
  assert.equal(defaultCargoBuildJobs({ cpus: 32, totalMemoryBytes: 128 * 1024 ** 3 }), 32);
});

test('client-side sccache requires version 0.17 or newer', async () => {
  assert.equal(await supportsClientSideSccache('C:\\sccache.exe', async () => ({ stdout: 'sccache 0.16.0\r\n' })), false);
  assert.equal(await supportsClientSideSccache('C:\\sccache.exe', async () => ({ stdout: 'sccache 0.17.1\r\n' })), true);
});

test('Windows bootstrap requires a High-integrity token', async () => {
  const { assertElevatedWindows } = await import('./bootstrap-windows-cli.mjs');
  const groups = (label) => async () => ({ stdout: `Mandatory Label ${label} Mandatory Level ${label === 'High' ? 'S-1-16-12288' : 'S-1-16-8192'}
` });
  await assert.doesNotReject(assertElevatedWindows({ run: groups('High') }));
  await assert.rejects(assertElevatedWindows({ run: groups('Medium') }), /must run elevated/);
  await assert.rejects(assertElevatedWindows({ run: async () => { throw new Error('whoami missing'); } }), /cannot determine Windows elevation/);
});
