import { statfsSync } from 'node:fs';
import { availableParallelism as cpuCount, totalmem } from 'node:os';

const GiB = 1024 ** 3;

/** Measured 2026-10-05 on the Windows host: heavy managed builds peaked at
 * 14.9 GiB job-object commit with 16 Cargo jobs (~0.93 GiB per job). Rounded
 * up to 1 GiB; re-derive from receipts rather than guessing a new value. */
export const MEASURED_BUILD_MEMORY_PER_JOB_BYTES = GiB;
/** Builds may be admitted up to this share of physical memory (design §2). */
export const BUILD_ALLOCATABLE_MEMORY_FRACTION = 0.8;
/** Target-cache GC keeps at least this share of the managed volume free. */
export const DISK_FREE_FLOOR_FRACTION = 0.1;
/** sccache may use at most this share of the managed volume. */
export const COMPILER_CACHE_VOLUME_FRACTION = 0.05;
/** Build outputs may use at most this share of the managed volume. */
export const TARGET_CACHE_VOLUME_FRACTION = 0.15;
/** Absolute ceilings: build storage is a fixed budget, never "the rest of the disk". GC evicts
 * idle generations past the cap before admitting builds. Measure from receipts before raising. */
export const TARGET_CACHE_MAX_BYTES = 400 * GiB;
export const COMPILER_CACHE_MAX_BYTES = 60 * GiB;

/** Bounds regenerated from the host on every bootstrap; never carried forward as operator tuning. */
export const HOST_DERIVED_BOUNDS = Object.freeze(['compileBudgetUnits', 'buildCostUnits', 'buildConcurrency', 'targetCacheCapBytes', 'gcFreeFloorBytes']);

export function volumeBytes(path, { statfs = statfsSync } = {}) {
  const stats = statfs(path);
  return Number(stats.blocks) * Number(stats.bsize);
}

const budgetUnits = (totalMemoryBytes) => Math.max(1, Math.floor((Number.isFinite(totalMemoryBytes) && totalMemoryBytes > 0 ? totalMemoryBytes : GiB) * BUILD_ALLOCATABLE_MEMORY_FRACTION / GiB));

/** Every logical CPU, unless build-allocatable memory cannot hold that many jobs at the measured per-job peak. */
export function deriveCargoBuildJobs({ cpus = cpuCount(), totalMemoryBytes = totalmem(), memoryPerJobBytes = MEASURED_BUILD_MEMORY_PER_JOB_BYTES } = {}) {
  const logical = Number.isSafeInteger(cpus) && cpus > 0 ? cpus : 1;
  return Math.max(1, Math.min(logical, Math.floor(budgetUnits(totalMemoryBytes) * GiB / memoryPerJobBytes)));
}

/** One sizing rule for every host, derived from logical CPUs, physical memory, & managed volume size.
 * Compile budget units are GiB of build-allocatable memory; a build root costs its Cargo jobs times the
 * measured per-job memory, so concurrency follows RAM instead of a fixed slot count. */
export function deriveHostSizing({ cpus = cpuCount(), totalMemoryBytes = totalmem(), managedVolumeBytes, cargoBuildJobs = null, memoryPerJobBytes = MEASURED_BUILD_MEMORY_PER_JOB_BYTES } = {}) {
  if (!Number.isFinite(totalMemoryBytes) || totalMemoryBytes <= 0) throw new TypeError('totalMemoryBytes must be positive');
  if (!Number.isSafeInteger(managedVolumeBytes) || managedVolumeBytes <= 0) throw new TypeError('managedVolumeBytes must be a positive integer');
  const jobs = cargoBuildJobs ?? deriveCargoBuildJobs({ cpus, totalMemoryBytes, memoryPerJobBytes });
  if (!Number.isSafeInteger(jobs) || jobs < 1) throw new TypeError('cargoBuildJobs must be a positive integer');
  const compileBudgetUnits = budgetUnits(totalMemoryBytes);
  const buildCostUnits = Math.max(1, Math.min(compileBudgetUnits, Math.ceil(jobs * memoryPerJobBytes / GiB)));
  const gcFreeFloorBytes = Math.floor(managedVolumeBytes * DISK_FREE_FLOOR_FRACTION);
  const capacityBytes = managedVolumeBytes - gcFreeFloorBytes;
  return Object.freeze({
    cargoBuildJobs: jobs,
    compileBudgetUnits,
    buildCostUnits,
    buildConcurrency: Math.max(1, Math.floor(compileBudgetUnits / buildCostUnits)),
    capacityBytes,
    targetCacheCapBytes: Math.min(capacityBytes, TARGET_CACHE_MAX_BYTES, Math.floor(managedVolumeBytes * TARGET_CACHE_VOLUME_FRACTION)),
    gcFreeFloorBytes,
    compilerCacheMaxBytes: Math.min(COMPILER_CACHE_MAX_BYTES, Math.floor(managedVolumeBytes * COMPILER_CACHE_VOLUME_FRACTION))
  });
}

/** The host-derived subset of broker bounds. */
export function hostBounds(sizing) {
  return Object.fromEntries(HOST_DERIVED_BOUNDS.map((key) => [key, sizing[key]]));
}
