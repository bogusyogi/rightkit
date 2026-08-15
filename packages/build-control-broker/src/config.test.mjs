import assert from 'node:assert/strict';
import test from 'node:test';
import { loadBrokerConfig, validateBrokerConfig } from './config.mjs';

const fixture = () => ({ broker: { externalRoot: { path: '/managed', volumeId: 'disk-1', mounted: true, writable: true, internal: false }, capacityBytes: 100, stateFile: '/managed/state.json', receiptsDir: '/managed/receipts' }, unixSocket: '/managed/broker.sock', tools: [], shims: {}, defaults: {}, outputPolicy: { mode: 'enforce', allowedSourceDirs: [], scanRoot: 'repository' }, bounds: { heartbeatIntervalMs: 10, diskSampleIntervalMs: 10, processGraceMs: 0, freeFloorBytes: 0, quotaWatermarkBytes: null, retentionMs: null, gcIntervalMs: 10 } });
const addCargoTool = (value) => { value.tools = [{ id: 'cargo-pinned', path: '/opt/rust/bin/cargo', root: '/opt/rust', sha256: 'a'.repeat(64) }]; value.shims = { cargo: 'cargo-pinned' }; return value; };

test('config accepts explicit known settings & freezes its outer object', () => {
  const config = validateBrokerConfig(fixture());
  assert.equal(config.broker.externalRoot.path, '/managed');
  assert.deepEqual(config.sccache, { mode: 'disabled', lifecycleOwner: null, requireOwnershipProof: false });
  assert.ok(Object.isFrozen(config)); assert.ok(Object.isFrozen(config.broker)); assert.ok(Object.isFrozen(config.broker.externalRoot));
});

test('config refuses managed sccache without broker ownership proof contract', () => {
  const value = addCargoTool(fixture()); value.defaults = { sccacheToolId: 'cargo-pinned', cacheMode: 'managed' }; value.sccache = { mode: 'managed', lifecycleOwner: 'broker', requireOwnershipProof: false };
  assert.throws(() => validateBrokerConfig(value), /ownership proof/);
  value.sccache.requireOwnershipProof = true;
  assert.equal(validateBrokerConfig(value).sccache.mode, 'managed');
});

test('config accepts a Windows managed-root path on any coordinator host', () => {
  const value = fixture(); value.broker.externalRoot.path = 'D:\\RightKitManagedAgent\\external'; value.broker.stateFile = 'D:\\RightKitManagedAgent\\external\\state.json'; value.broker.receiptsDir = 'D:\\RightKitManagedAgent\\external\\receipts'; delete value.unixSocket; value.windowsPipe = '\\\\.\\pipe\\rightkit-managed-agent';
  assert.equal(validateBrokerConfig(value).broker.externalRoot.path, 'D:\\RightKitManagedAgent\\external');
});

test('config rejects unknown settings at every owned boundary', () => {
  for (const change of [
    (v) => { v.ignored = true; },
    (v) => { v.broker.pollIntervalMs = 1; },
    (v) => { v.broker.externalRoot.mutable = true; },
    (v) => { v.outputPolicy.recursiveScan = true; },
    (v) => { v.bounds.unknownBound = 1; }
  ]) { const value = fixture(); change(value); assert.throws(() => validateBrokerConfig(value), /unknown/); }
});

test('v1 poll interval is accepted only as disk-sample compatibility input', () => {
  const value = fixture(); delete value.bounds.diskSampleIntervalMs; value.schemaVersion = 1; value.bounds.pollIntervalMs = 123;
  assert.equal(validateBrokerConfig(value).bounds.diskSampleIntervalMs, 123);
});

test('config loader wraps invalid JSON & validates parsed values', async () => {
  await assert.rejects(loadBrokerConfig('/config.json', { readFile: async () => '{' }), /invalid broker config/);
  await assert.rejects(loadBrokerConfig('/config.json', { readFile: async () => JSON.stringify({ ...fixture(), unixSocket: 'relative.sock' }) }), /absolute path/);
});

test('config validates digest-pinned external command registrations', () => {
  const value = addCargoTool(fixture()); value.externalCommands = [{ shim: 'cargo', command: 'about', toolId: 'cargo-pinned', lane: 'CONTROL', digest: 'a'.repeat(64) }];
  const config = validateBrokerConfig(value); assert.equal(config.externalCommands[0].toolId, 'cargo-pinned'); assert.ok(Object.isFrozen(config.externalCommands));
  for (const mutate of [
    (entry) => { entry.digest = 'bad'; },
    (entry) => { entry.lane = 'MAINTENANCE'; },
    (entry) => { entry.extra = true; },
    (entry) => { entry.shim = 'rustc'; }
  ]) { const candidate = addCargoTool(fixture()); candidate.externalCommands = [{ shim: 'cargo', command: 'about', toolId: 'cargo-pinned', lane: 'CONTROL', digest: 'a'.repeat(64) }]; mutate(candidate.externalCommands[0]); assert.throws(() => validateBrokerConfig(candidate)); }
  const duplicate = addCargoTool(fixture()); duplicate.externalCommands = [value.externalCommands[0], value.externalCommands[0]]; assert.throws(() => validateBrokerConfig(duplicate), /duplicate/);
});

test('config rejects malformed tool registrations & unregistered references', () => {
  for (const mutate of [
    (v) => { addCargoTool(v).tools[0].extra = true; },
    (v) => { addCargoTool(v).tools[0].path = '/other/cargo'; },
    (v) => { addCargoTool(v).tools[0].path = 'relative/cargo'; },
    (v) => { addCargoTool(v).tools[0].sha256 = 'not-a-digest'; },
    (v) => { addCargoTool(v).tools.push({ ...v.tools[0] }); },
    (v) => { v.shims = { cargo: 'unregistered' }; },
    (v) => { addCargoTool(v); v.defaults = { rustcToolId: 'unregistered' }; }
  ]) { const value = fixture(); mutate(value); assert.throws(() => validateBrokerConfig(value)); }
});

test('config validates every defaults field & sccache cross-field contract', () => {
  const valid = addCargoTool(fixture()); valid.defaults = { repositoryIdentity: 'repo-1', toolchain: 'stable', targetTriple: 'x86_64-apple-darwin', targetTriples: ['x86_64-apple-darwin', 'aarch64-apple-darwin'], buildProfile: 'development', reservationBytes: 100, rustcToolId: 'cargo-pinned', rustdocToolId: 'cargo-pinned', sccacheToolId: 'cargo-pinned', cacheMode: 'managed', cargoBuildJobs: 4 }; valid.sccache = { mode: 'managed', lifecycleOwner: 'broker', requireOwnershipProof: true };
  assert.equal(validateBrokerConfig(valid).defaults.cargoBuildJobs, 4);
  for (const mutate of [
    (v) => { addCargoTool(v); v.defaults = { unknown: true }; },
    (v) => { addCargoTool(v); v.defaults = { targetTriple: 'x86_64-apple-darwin', targetTriples: ['aarch64-apple-darwin'] }; },
    (v) => { addCargoTool(v); v.defaults = { targetTriples: ['x86_64-apple-darwin', 'x86_64-apple-darwin'] }; },
    (v) => { addCargoTool(v); v.defaults = { cargoBuildJobs: 0 }; },
    (v) => { addCargoTool(v); v.defaults = { reservationBytes: 101 }; },
    (v) => { addCargoTool(v); v.defaults = { cacheMode: 'automatic' }; },
    (v) => { addCargoTool(v); v.sccache = { mode: 'disabled', lifecycleOwner: 'broker', requireOwnershipProof: true }; }
  ]) { const value = fixture(); mutate(value); assert.throws(() => validateBrokerConfig(value)); }
});

test('config rejects missing or malformed endpoints, escaped state paths & invalid output policy', () => {
  for (const mutate of [
    (v) => { delete v.unixSocket; },
    (v) => { v.windowsPipe = 'rightkit'; },
    (v) => { v.broker.stateFile = '/state.json'; },
    (v) => { v.broker.receiptsDir = '/managed/../receipts'; },
    (v) => { v.outputPolicy.allowedSourceDirs = ['src', '../escape']; },
    (v) => { v.outputPolicy.mode = 'audit'; },
    (v) => { v.outputPolicy.scanRoot = 'workspace'; }
  ]) { const value = fixture(); mutate(value); assert.throws(() => validateBrokerConfig(value)); }
});

test('config enforces capacity & bounded-output relationships without conflating disk floor with quota', () => {
  const valid = fixture(); valid.broker.usedBytes = 100; valid.bounds.freeFloorBytes = 10_000; valid.bounds.outputBufferBytes = 4_096; valid.bounds.outputHeadBytes = 2_048; valid.bounds.outputTailBytes = 2_048;
  assert.equal(validateBrokerConfig(valid).bounds.freeFloorBytes, 10_000);
  for (const mutate of [
    (v) => { v.broker.usedBytes = 101; },
    (v) => { v.bounds.quotaWatermarkBytes = 101; },
    (v) => { v.bounds.outputBufferBytes = 1_024; v.bounds.outputHeadBytes = 1_024; v.bounds.outputTailBytes = 1; },
    (v) => { v.bounds.eventHistoryEntries = 15; },
    (v) => { v.bounds.eventLogMaxBytes = 1_023; },
    (v) => { v.schemaVersion = 2; v.bounds.pollIntervalMs = 10; }
  ]) { const value = fixture(); mutate(value); assert.throws(() => validateBrokerConfig(value)); }
});

test('schema v2 requires a contained activation fence while v1 remains migration-compatible', () => {
  const missing = fixture(); missing.schemaVersion = 2;
  assert.throws(() => validateBrokerConfig(missing), /activationFencePath/);
  const valid = fixture(); valid.schemaVersion = 2; valid.broker.activationFencePath = '/managed/state/activation-fence.json';
  assert.equal(validateBrokerConfig(valid).broker.activationFencePath, '/managed/state/activation-fence.json');
  const escaped = fixture(); escaped.schemaVersion = 2; escaped.broker.activationFencePath = '/outside/fence.json';
  assert.throws(() => validateBrokerConfig(escaped), /below broker\.externalRoot/);
});
