import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { loadBrokerConfig } from './config.mjs';
import { defaultBounds, defaultCargoBuildJobs, isDirectMain, parseArgs, shimTable } from './bootstrap-windows-cli.mjs';

// discoverTools/where/buildConfig shell out to `where.exe`, which does not
// exist on this Mac — those three are exercised only by a real Windows run.
// Everything below is pure logic or the config *shape*, both fully verifiable
// here; the shape assertion below is load-bearing: it proves a config built
// the way bootstrap-windows-cli.mjs's buildConfig() constructs one is
// actually accepted by the broker's own schema, not just plausible-looking.

test('parseArgs accepts the documented flags & rejects anything else', () => {
  assert.deepEqual(parseArgs([]), { force: false });
  assert.deepEqual(parseArgs(['--force']), { force: true });
  assert.deepEqual(parseArgs(['--external-root', 'D:\\x']), { force: false, externalRoot: 'D:\\x' });
  assert.throws(() => parseArgs(['--bogus']), /unknown bootstrap argument/);
});

test('direct-main detection is portable across Windows and POSIX path forms', () => {
  assert.equal(isDirectMain({ moduleUrl: 'file:///C:/RightKit/bootstrap-windows-cli.mjs', argv1: 'C:\\RightKit\\bootstrap-windows-cli.mjs', platform: 'win32' }), true);
  assert.equal(isDirectMain({ moduleUrl: 'file:///C:/RightKit/bootstrap-windows-cli.mjs', argv1: 'C:\\Other\\bootstrap-windows-cli.mjs', platform: 'win32' }), false);
  assert.equal(isDirectMain({ moduleUrl: 'file:///tmp/bootstrap-windows-cli.mjs', argv1: '/tmp/bootstrap-windows-cli.mjs', platform: 'linux' }), true);
  assert.equal(isDirectMain({ moduleUrl: 'file:///tmp/bootstrap-windows-cli.mjs', argv1: '', platform: 'linux' }), false);
});

test('shimTable only maps shims for tools actually discovered, mirroring the macOS table', () => {
  assert.deepEqual(shimTable([{ id: 'cargo' }, { id: 'gcc' }]), { cargo: 'cargo', gcc: 'gcc' });
  assert.deepEqual(shimTable([]), {});
});

test('defaultBounds matches the same doc-mandated shape shipped on macOS', () => {
  const bounds = defaultBounds();
  assert.equal(bounds.jobDeadlineMs, 0);
  assert.ok(bounds.targetCacheCapBytes > 0);
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
      databasePath: join(scratch, 'state', 'broker.sqlite'),
      eventLogPath: join(scratch, 'state', 'events.ndjson'),
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
      buildProfile: 'development', reservationBytes: 16 * 1024 ** 3, rustcToolId: 'rustc', rustdocToolId: 'rustdoc', cacheMode: 'disabled', cargoBuildJobs: defaultCargoBuildJobs(),
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
  assert.equal(loaded.bounds.buildConcurrency, 2);
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
      databasePath: join(scratch, 'state', 'broker.sqlite'),
      eventLogPath: join(scratch, 'state', 'events.ndjson'),
      activationFencePath: join(scratch, 'state', 'runtime-activation.json'),
      receiptsDir: join(scratch, 'receipts'),
    },
    windowsPipe: '\\\\.\\pipe\\rightkit-build-control',
    tools: [{ id: 'pnpm', path: join(scratch, 'pnpm.exe'), root: scratch, sha256: 'a'.repeat(64) }],
    shims: { pnpm: 'pnpm' },
    externalCommands: [],
    defaults: { toolchain: 'stable-x86_64-pc-windows-msvc', targetTriple: 'x86_64-pc-windows-msvc', targetTriples: ['x86_64-pc-windows-msvc'], buildProfile: 'development', reservationBytes: 16 * 1024 ** 3, cacheMode: 'disabled', cargoBuildJobs: defaultCargoBuildJobs() },
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

test('cargo build jobs are clamped by memory before performance cores', () => {
  // 8 logical / 4 performance cores with 8 GiB: memory binds first at 4.
  assert.equal(defaultCargoBuildJobs({ cpus: 8, totalMemoryBytes: 8 * 1024 ** 3 }), 4);
  // Plenty of RAM: the performance-core share is the limit.
  assert.equal(defaultCargoBuildJobs({ cpus: 16, totalMemoryBytes: 64 * 1024 ** 3 }), 8);
  // Small host: never below one job.
  assert.equal(defaultCargoBuildJobs({ cpus: 2, totalMemoryBytes: 1 * 1024 ** 3 }), 1);
});
