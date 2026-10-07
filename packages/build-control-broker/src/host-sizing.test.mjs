import assert from 'node:assert/strict';
import test from 'node:test';
import { HOST_DERIVED_BOUNDS, deriveHostSizing, hostBounds } from './host-sizing.mjs';

const GiB = 1024 ** 3;

test('Windows workstation (20 CPUs, 64 GiB) sizes builds from RAM, not fixed slots', () => {
  const sizing = deriveHostSizing({ cpus: 20, totalMemoryBytes: 64 * GiB, managedVolumeBytes: 3726 * GiB });
  assert.equal(sizing.cargoBuildJobs, 20);
  assert.equal(sizing.compileBudgetUnits, 51);
  assert.equal(sizing.buildCostUnits, 20);
  assert.equal(sizing.buildConcurrency, 2);
  assert.equal(sizing.gcFreeFloorBytes, Math.floor(3726 * GiB * 0.1));
  assert.equal(sizing.targetCacheCapBytes, 400 * GiB, 'a 3.7 TB volume still gets a bounded build budget');
  assert.equal(sizing.compilerCacheMaxBytes, 150 * GiB);
});

test('Mac (12 CPUs, 24 GiB) gets the same rule: all CPUs, one full-width build fits', () => {
  const sizing = deriveHostSizing({ cpus: 12, totalMemoryBytes: 24 * GiB, managedVolumeBytes: 927 * GiB });
  assert.equal(sizing.cargoBuildJobs, 12);
  assert.equal(sizing.compileBudgetUnits, 19);
  assert.equal(sizing.buildCostUnits, 12);
  assert.equal(sizing.buildConcurrency, 1);
  assert.equal(sizing.compilerCacheMaxBytes, 150 * GiB);
  assert.equal(sizing.targetCacheCapBytes, 400 * GiB);
});

test('an explicit job override still prices the build by its jobs', () => {
  const sizing = deriveHostSizing({ cpus: 12, totalMemoryBytes: 24 * GiB, managedVolumeBytes: 927 * GiB, cargoBuildJobs: 6 });
  assert.equal(sizing.buildCostUnits, 6);
  assert.equal(sizing.buildConcurrency, 3);
});

test('hostBounds carries exactly the host-derived bound keys', () => {
  const bounds = hostBounds(deriveHostSizing({ cpus: 4, totalMemoryBytes: 8 * GiB, managedVolumeBytes: 100 * GiB }));
  assert.deepEqual(Object.keys(bounds).sort(), [...HOST_DERIVED_BOUNDS].sort());
  assert.throws(() => deriveHostSizing({ cpus: 4, totalMemoryBytes: 8 * GiB }), TypeError);
});
