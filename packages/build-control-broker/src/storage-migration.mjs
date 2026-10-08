#!/usr/bin/env node
import { constants } from 'node:fs';
import { copyFile, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { basename, dirname, join } from 'node:path';
import { availableParallelism, totalmem } from 'node:os';
import { fileURLToPath } from 'node:url';
import { deriveHostSizing } from './host-sizing.mjs';
import { discoverManagedVolume } from './volume-discovery.mjs';

const isDirectMain = () => typeof process.argv[1] === 'string' && fileURLToPath(import.meta.url) === process.argv[1];

export function parseArgs(argv) {
  const options = { apply: false, lead: false };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    if (flag === '--config') {
      options.configPath = argv[++index];
      if (typeof options.configPath !== 'string' || !options.configPath) throw new TypeError('--config requires a path');
      continue;
    }
    if (flag === '--apply') { options.apply = true; continue; }
    if (flag === '--lead') { options.lead = true; continue; }
    if (flag === '--expected-volume-id') {
      options.expectedVolumeId = argv[++index];
      if (typeof options.expectedVolumeId !== 'string' || !options.expectedVolumeId.trim()) throw new TypeError('--expected-volume-id requires a value');
      options.expectedVolumeId = options.expectedVolumeId.trim();
      continue;
    }
    if (flag === '--force') throw new TypeError('--force is not supported by storage migration');
    throw new TypeError(`unknown storage migration argument: ${flag}`);
  }
  if (options.apply && !options.lead) throw new Error('--apply is lead-only; pass --lead');
  return options;
}

function object(value, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new TypeError(`${label} must be an object`);
  return value;
}

function configuredVolumeId(config) {
  const value = config?.broker?.externalRoot?.volumeId;
  return typeof value === 'string' && value.trim() ? value.trim() : null;
}

/** Build a targeted config update. It never mutates supplied config and never
 * copies generated fields over unrelated hand-edited fields. */
export async function planStorageMigration(config, { platform = process.platform, cpus = availableParallelism(), totalMemoryBytes = totalmem(), discover = discoverManagedVolume, apply = false, expectedVolumeId = null } = {}) {
  object(config, 'broker config');
  const root = config?.broker?.externalRoot?.path;
  if (typeof root !== 'string' || !root) throw new TypeError('broker.externalRoot.path is required');
  const volume = await discover(root, { platform });
  if (!volume?.volumeId) throw new Error('refusing storage migration: current volume identity is unavailable');
  const configuredId = configuredVolumeId(config);
  const currentId = String(volume.volumeId).trim();
  if (!currentId || currentId.toLowerCase() === 'unknown') throw new Error('refusing storage migration: current volume identity is unknown');
  const sameConfiguredVolume = Boolean(configuredId && configuredId.toLowerCase() !== 'unknown' && currentId.toLowerCase() === configuredId.toLowerCase());
  const rebind = !sameConfiguredVolume;
  if (expectedVolumeId !== null && (typeof expectedVolumeId !== 'string' || !expectedVolumeId.trim())) throw new TypeError('expectedVolumeId must be a non-empty string');
  if (expectedVolumeId !== null && expectedVolumeId.trim().toLowerCase() !== currentId.toLowerCase()) throw new Error(`refusing storage migration: expected volume ${expectedVolumeId} does not match current volume ${currentId}`);
  if (apply && rebind && expectedVolumeId === null) throw new Error(`storage migration requires --expected-volume-id ${currentId} when rebinding volume identity`);
  const sizing = deriveHostSizing({ cpus, totalMemoryBytes, managedVolumeBytes: volume.capacityBytes });
  const next = structuredClone(config);
  next.broker = { ...(next.broker ?? {}), capacityBytes: sizing.capacityBytes, externalRoot: { ...(next.broker?.externalRoot ?? {}), volumeId: currentId, mounted: true, writable: volume.writable !== false } };
  next.bounds = { ...(next.bounds ?? {}), gcFreeFloorBytes: sizing.gcFreeFloorBytes, targetCacheCapBytes: sizing.targetCacheCapBytes };
  return Object.freeze({ config: next, volume, sizing, rebind, expectedVolumeId: currentId, changes: Object.freeze({ 'broker.capacityBytes': sizing.capacityBytes, 'broker.externalRoot.volumeId': currentId, 'broker.externalRoot.mounted': true, 'broker.externalRoot.writable': volume.writable !== false, 'bounds.gcFreeFloorBytes': sizing.gcFreeFloorBytes, 'bounds.targetCacheCapBytes': sizing.targetCacheCapBytes }) });
}

async function backupPath(configPath, { copy = copyFile, now = Date.now } = {}) {
  const base = `${configPath}.bak`;
  try { await copy(configPath, base, constants.COPYFILE_EXCL); return base; }
  catch (error) { if (error.code !== 'EEXIST') throw error; const path = `${base}.${now()}`; await copy(configPath, path, constants.COPYFILE_EXCL); return path; }
}

async function atomicWriteConfig(path, value) {
  const temporary = join(dirname(path), `.${basename(path)}-${process.pid}-${Date.now()}.tmp`);
  try { await writeFile(temporary, `${JSON.stringify(value, null, 2)}\n`, { encoding: 'utf8', mode: 0o600, flag: 'wx' }); await rename(temporary, path); }
  finally { await rm(temporary, { force: true }).catch(() => {}); }
}

export async function migrateStorageConfig(configPath, { apply = false, lead = false, read = readFile, write = atomicWriteConfig, backup = backupPath, ...options } = {}) {
  if (typeof configPath !== 'string' || !configPath) throw new TypeError('config path is required');
  if (apply && !lead) throw new Error('storage config apply is lead-only; pass lead authorization');
  const source = await read(configPath, 'utf8');
  let parsed;
  try { parsed = JSON.parse(String(source).replace(/^\uFEFF/, '')); } catch (error) { throw new TypeError(`invalid broker config: ${error.message}`); }
  const plan = await planStorageMigration(parsed, { ...options, apply });
  if (!apply) return Object.freeze({ ...plan, applied: false, backupPath: null });
  const saved = await backup(configPath);
  await write(configPath, plan.config);
  return Object.freeze({ ...plan, applied: true, backupPath: saved });
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const configPath = options.configPath ?? process.env.RIGHTKIT_BUILD_BROKER_CONFIG;
  if (!configPath) throw new Error('explicit broker config path is required');
  const result = await migrateStorageConfig(configPath, options);
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

if (isDirectMain()) main().catch((error) => { process.stderr.write(`${error.message}\n`); process.exitCode = 1; });
