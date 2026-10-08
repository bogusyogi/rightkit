import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { migrateStorageConfig, parseArgs, planStorageMigration } from './storage-migration.mjs';

const GiB = 1024 ** 3;
const TARGET_CACHE_GB = 150_000_000_000;
const baseConfig = (root) => ({ schemaVersion: 2, unrelated: { keep: true }, broker: { externalRoot: { path: root, volumeId: 'volume-1', mounted: false, writable: false, internal: false }, capacityBytes: 10, stateFile: join(root, 'state.json'), receiptsDir: join(root, 'receipts') }, sccache: { maxSizeBytes: 123, operatorValue: 'keep' }, bounds: { targetCacheCapBytes: 10, gcFreeFloorBytes: 2, operatorValue: 'keep' } });
const discovery = async (root) => ({ path: root, volumeId: 'volume-1', capacityBytes: 1_000 * GiB, mounted: true, writable: true });

test('storage migration refreshes only volume and host budget fields', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rk-storage-plan-'));
  try {
    const original = baseConfig(root);
    const plan = await planStorageMigration(original, { cpus: 8, totalMemoryBytes: 16 * GiB, discover: discovery });
    assert.equal(plan.config.unrelated.keep, true);
    assert.equal(plan.config.bounds.operatorValue, 'keep');
    assert.equal(plan.config.sccache.maxSizeBytes, 123);
    assert.equal(plan.config.sccache.operatorValue, 'keep');
    assert.equal(plan.config.broker.externalRoot.volumeId, 'volume-1');
    assert.equal(plan.config.broker.externalRoot.mounted, true);
    assert.equal(plan.config.broker.capacityBytes, 900 * GiB);
    assert.equal(plan.config.bounds.gcFreeFloorBytes, 100 * GiB);
    assert.equal(plan.config.bounds.targetCacheCapBytes, TARGET_CACHE_GB);
    assert.equal(original.broker.capacityBytes, 10);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('migration discovers unknown or replaced volumes, but lead apply needs explicit rebinding', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rk-storage-volume-'));
  try {
    const unknown = { ...baseConfig(root), broker: { ...baseConfig(root).broker, externalRoot: { ...baseConfig(root).broker.externalRoot, volumeId: 'unknown' } } };
    const unknownPlan = await planStorageMigration(unknown, { discover: discovery });
    assert.equal(unknownPlan.rebind, true); assert.equal(unknownPlan.expectedVolumeId, 'volume-1');
    await assert.rejects(planStorageMigration(unknown, { discover: discovery, apply: true }), /expected-volume-id/);
    await assert.rejects(planStorageMigration(unknown, { discover: async () => ({ volumeId: 'unknown', capacityBytes: 1_000 * GiB }) }), /identity is unknown/);
    const replaced = await planStorageMigration(baseConfig(root), { discover: async () => ({ volumeId: 'other', capacityBytes: 1_000 * GiB }) });
    assert.equal(replaced.rebind, true); assert.equal(replaced.config.broker.externalRoot.volumeId, 'other');
    await assert.rejects(planStorageMigration(baseConfig(root), { discover: async () => ({ volumeId: 'other', capacityBytes: 1_000 * GiB }), apply: true, expectedVolumeId: 'wrong' }), /does not match/);
    await assert.doesNotReject(planStorageMigration(baseConfig(root), { discover: async () => ({ volumeId: 'other', capacityBytes: 1_000 * GiB }), apply: true, expectedVolumeId: 'other' }));
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('dry-run leaves config untouched; apply requires lead & keeps backup', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rk-storage-apply-')); const path = join(root, 'config.json');
  try {
    await writeFile(path, JSON.stringify(baseConfig(root)));
    const before = await readFile(path, 'utf8');
    const dry = await migrateStorageConfig(path, { discover: discovery, cpus: 8, totalMemoryBytes: 16 * GiB });
    assert.equal(dry.applied, false); assert.equal(await readFile(path, 'utf8'), before);
    await assert.rejects(migrateStorageConfig(path, { discover: discovery, apply: true }), /lead/);
    const applied = await migrateStorageConfig(path, { discover: discovery, apply: true, lead: true, cpus: 8, totalMemoryBytes: 16 * GiB });
    assert.equal(applied.applied, true); assert.ok(applied.backupPath); assert.equal(JSON.parse(await readFile(applied.backupPath, 'utf8')).broker.capacityBytes, 10);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('storage migration rejects bootstrap force', () => {
  assert.throws(() => parseArgs(['--config', '/tmp/config.json', '--force']), /--force/);
  assert.throws(() => parseArgs(['--config', '/tmp/config.json', '--apply']), /lead/);
  assert.equal(parseArgs(['--config', '/tmp/config.json', '--expected-volume-id', 'disk-1']).expectedVolumeId, 'disk-1');
});
