import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, parse } from 'node:path';
import test from 'node:test';
import { loadBrokerConfig, preserveOperatorTunedConfig, validateBrokerConfig } from './config.mjs';

function fixture(root = '/managed') {
  return {
    schemaVersion: 2,
    broker: { externalRoot: { path: root, volumeId: 'disk-1', mounted: true, writable: true, internal: false }, capacityBytes: 100_000, stateFile: `${root}/state.json`, receiptsDir: `${root}/receipts`, activationFencePath: `${root}/activation.json` },
    unixSocket: `${root}/broker.sock`,
    tools: [{ id: 'cargo', path: `${root}/tools/cargo`, root: `${root}/tools`, sha256: 'a'.repeat(64) }, { id: 'rustc', path: `${root}/tools/rustc`, root: `${root}/tools`, sha256: 'b'.repeat(64) }, { id: 'rustdoc', path: `${root}/tools/rustdoc`, root: `${root}/tools`, sha256: 'c'.repeat(64) }],
    shims: { cargo: 'cargo', rustc: 'rustc', rustdoc: 'rustdoc' },
    defaults: { toolchain: 'stable-aarch64-apple-darwin', targetTriple: 'aarch64-apple-darwin', targetTriples: ['aarch64-apple-darwin'], buildProfile: 'development', rustcToolId: 'rustc', rustdocToolId: 'rustdoc', cacheMode: 'disabled', cargoBuildJobs: 4 },
    sccache: { mode: 'disabled', clientSide: false, lifecycleOwner: null, requireOwnershipProof: false, maxSizeBytes: 20_000 },
    outputPolicy: { mode: 'enforce', allowedSourceDirs: [], scanRoot: 'repository' },
    bounds: { heartbeatIntervalMs: 5_000, processGraceMs: 5_000, freeFloorBytes: 0, jobDeadlineMs: 0, outputBufferBytes: 8_192, outputHeadBytes: 1_024, outputTailBytes: 1_024, controlConcurrency: 4, analysisConcurrency: 2, buildConcurrency: 1, workspaceWriteConcurrency: 4, workspaceDiscoveryTtlMs: 600_000, workspaceDiscoveryTimeoutMs: 2_000 },
  };
}

test('config accepts exact thin settings & freezes result', () => {
  const config = validateBrokerConfig(fixture()); assert.equal(config.bounds.buildConcurrency, 1); assert.equal(Object.isFrozen(config), true); assert.equal(Object.isFrozen(config.broker), true);
});

test('config accepts aggregate compile budget units & rejects invalid values', () => {
  const value = fixture(); value.bounds.compileBudgetUnits = 12;
  const config = validateBrokerConfig(value); assert.equal(config.bounds.compileBudgetUnits, 12);
  const zero = fixture(); zero.bounds.compileBudgetUnits = 0;
  assert.throws(() => validateBrokerConfig(zero), /compileBudgetUnits/);
  const fractional = fixture(); fractional.bounds.compileBudgetUnits = 1.5;
  assert.throws(() => validateBrokerConfig(fractional), /compileBudgetUnits/);
});

test('config rejects predecessor workflow, reservation, & maintenance concepts', () => {
  for (const mutate of [
    (v) => { v.broker.databasePath = '/managed/state.sqlite'; },
    (v) => { v.broker.eventLogPath = '/managed/events.ndjson'; },
    (v) => { v.broker.usedBytes = 1; },
    (v) => { v.defaults.reservationBytes = 1; },
    (v) => { v.bounds.maintenanceConcurrency = 1; },
    (v) => { v.bounds.quotaWatermarkBytes = 1; },
  ]) { const value = fixture(); mutate(value); assert.throws(() => validateBrokerConfig(value), /unknown/); }
});

test('config accepts Windows managed paths on any coordinator host', () => {
  const root = 'D:\\RightKitManagedAgent'; const value = fixture(root); delete value.unixSocket; value.windowsPipe = '\\\\.\\pipe\\rightkit-managed-agent';
  for (const tool of value.tools) { tool.root = `${root}\\tools`; tool.path = `${root}\\tools\\${tool.id}.exe`; }
  value.broker.stateFile = `${root}\\state.json`; value.broker.receiptsDir = `${root}\\receipts`; value.broker.activationFencePath = `${root}\\activation.json`;
  assert.equal(validateBrokerConfig(value).windowsPipe, value.windowsPipe);
});

test('client-side sccache requires broker gateway ownership proof', () => {
  const value = fixture(); value.tools.push({ id: 'sccache', path: '/managed/tools/sccache', root: '/managed/tools', sha256: 'd'.repeat(64) }); value.defaults.sccacheToolId = 'sccache'; value.defaults.cacheMode = 'managed'; value.sccache = { mode: 'managed', clientSide: true, lifecycleOwner: 'broker', requireOwnershipProof: true, expectedVersion: '0.17.0', maxSizeBytes: 20_000 };
  const validated = validateBrokerConfig(value);
  assert.equal(validated.sccache.clientSide, true);
  assert.equal(validated.sccache.gatewayPort, 4_227);
  value.sccache.lifecycleOwner = null; assert.throws(() => validateBrokerConfig(value), /ownership proof/);
});

test('server-side sccache requires broker ownership proof', () => {
  const value = fixture(); value.tools.push({ id: 'sccache', path: '/managed/tools/sccache', root: '/managed/tools', sha256: 'd'.repeat(64) }); value.defaults.sccacheToolId = 'sccache'; value.defaults.cacheMode = 'managed'; value.sccache = { mode: 'managed', clientSide: false, lifecycleOwner: null, requireOwnershipProof: false, expectedVersion: '0.17.0', maxSizeBytes: 20_000 };
  assert.throws(() => validateBrokerConfig(value), /ownership proof/);
});

test('config validates external command pins & bounded output', () => {
  const value = fixture(); value.tools.push({ id: 'cargo-upgrade', path: '/managed/tools/cargo-upgrade', root: '/managed/tools', sha256: 'd'.repeat(64) }); value.externalCommands = [{ shim: 'cargo', command: 'upgrade', toolId: 'cargo-upgrade', lane: 'WORKSPACE_WRITE', digest: 'd'.repeat(64) }];
  assert.equal(validateBrokerConfig(value).externalCommands[0].command, 'upgrade');
  value.bounds.outputHeadBytes = 7_500; assert.throws(() => validateBrokerConfig(value), /outputHeadBytes.*outputTailBytes/);
});

test('config rejects unknown keys, escaped paths, & malformed endpoints', () => {
  const unknown = fixture(); unknown.mystery = true; assert.throws(() => validateBrokerConfig(unknown), /unknown broker config key/);
  const nativeRoot = join(parse(tmpdir()).root, 'managed'); const escaped = fixture(nativeRoot); escaped.broker.receiptsDir = join(parse(nativeRoot).root, 'elsewhere'); assert.throws(() => validateBrokerConfig(escaped), /remain below/);
  const endpoint = fixture(); endpoint.unixSocket = 'relative.sock'; assert.throws(() => validateBrokerConfig(endpoint), /absolute/);
});

test('config loader strips UTF-8 BOM & wraps malformed JSON', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rk-config-')); const path = join(root, 'config.json');
  try { await writeFile(path, `﻿${JSON.stringify(fixture(root))}`); assert.equal((await loadBrokerConfig(path)).schemaVersion, 2); await writeFile(path, '{'); await assert.rejects(loadBrokerConfig(path), /invalid broker config/); }
  finally { await rm(root, { recursive: true, force: true }); }
});

test('force re-bootstrap preserves operator-tuned maxSizeBytes & valid bounds, drops invalid bounds', () => {
  const next = fixture(); const previous = fixture(); next.bounds.workspaceDiscoveryTimeoutMs = 30_000; next.bounds.buildConcurrency = 2;
  previous.sccache.maxSizeBytes = 10_000; previous.bounds.buildConcurrency = 3; previous.bounds.analysisConcurrency = 5;
  assert.deepEqual(preserveOperatorTunedConfig(next, previous), ['sccache.maxSizeBytes', 'bounds']);
  // Operator tuning survives; host-derived bounds are regenerated from the host.
  assert.equal(next.sccache.maxSizeBytes, 10_000); assert.equal(next.bounds.analysisConcurrency, 5); assert.equal(next.bounds.buildConcurrency, 2); assert.equal(next.bounds.workspaceDiscoveryTimeoutMs, 30_000);
  // A previous sccache size above the volume-derived ceiling is not carried forward.
  const capped = fixture(); const oversized = fixture(); oversized.sccache.maxSizeBytes = 268_435_456_000;
  assert.deepEqual(preserveOperatorTunedConfig(capped, oversized), ['bounds']); assert.equal(capped.sccache.maxSizeBytes, 20_000);
  const tuned = fixture(); const tunedPrevious = fixture(); tuned.bounds.workspaceDiscoveryTimeoutMs = 30_000; tunedPrevious.bounds.workspaceDiscoveryTimeoutMs = 7_500;
  assert.deepEqual(preserveOperatorTunedConfig(tuned, tunedPrevious), ['bounds']); assert.equal(tuned.bounds.workspaceDiscoveryTimeoutMs, 7_500);
  const invalid = fixture(); const badPrevious = fixture(); badPrevious.sccache.maxSizeBytes = -1; badPrevious.bounds.analysisConcurrency = 0;
  const generatedBounds = invalid.bounds;
  assert.deepEqual(preserveOperatorTunedConfig(invalid, badPrevious), []);
  assert.equal(invalid.sccache.maxSizeBytes, 20_000); assert.equal(invalid.bounds, generatedBounds);
  assert.deepEqual(preserveOperatorTunedConfig(fixture(), null), []);
});
