import assert from 'node:assert/strict';
import test from 'node:test';
import { cacheCohortKey, cargoCohort, classifyCacheReuse, summarizeCacheCohorts } from './cache-telemetry.mjs';

test('reuse status never confuses service health with cache proof', () => {
  assert.deepEqual(classifyCacheReuse(null), { status: 'NOT_MEASURED', hits: null, misses: null, bypassed: null, hitRate: null });
  assert.deepEqual(classifyCacheReuse({ cache_hits: { counts: {} }, cache_misses: { counts: {} } }), { status: 'NO_COMPILATION', hits: 0, misses: 0, bypassed: 0, hitRate: null });
  assert.deepEqual(classifyCacheReuse({ cache_hits: { counts: { Rust: 3 } }, cache_misses: { counts: {} } }), { status: 'PROVEN', hits: 3, misses: 0, bypassed: 0, hitRate: 1 });
  assert.deepEqual(classifyCacheReuse({ requests_not_cacheable: 2 }), { status: 'BYPASSED', hits: 0, misses: 0, bypassed: 2, hitRate: null });
  assert.equal(classifyCacheReuse({ non_cacheable_compilations: 1 }).status, 'BYPASSED');
  assert.equal(classifyCacheReuse({ cache_hits: { counts: { Rust: 3 } }, requests_not_cacheable: 1 }).status, 'PROVEN');
  assert.equal(classifyCacheReuse({ cache_hits: { counts: { Rust: 1 } }, cache_misses: { counts: { Rust: 3 } } }).status, 'MIXED');
  assert.equal(classifyCacheReuse({ cache_hits: { counts: {} }, cache_misses: { counts: { Rust: 2 } } }).status, 'MISS');
});

test('reporting never blends cohorts or turns unmeasured service health into hits', () => {
  const first = cargoCohort({ argv: ['check'], workspace: '/repo', platform: 'darwin', hostArch: 'arm64', hostName: 'mac' });
  const second = cargoCohort({ argv: ['test'], workspace: '/repo', platform: 'darwin', hostArch: 'arm64', hostName: 'mac' });
  const report = summarizeCacheCohorts([
    { cacheCohort: first, cache: { healthStatus: 'SERVICE_OK', reuse: { status: 'NOT_MEASURED', hits: null, misses: null } } },
    { cacheCohort: first, cache: { reuse: { status: 'PROVEN', hits: 4, misses: 0 } } },
    { cacheCohort: second, cache: { reuse: { status: 'MISS', hits: 0, misses: 2 } } }
  ]);
  assert.equal(report.length, 2);
  assert.deepEqual(report.find((entry) => entry.cohort.command === 'cargo check'), { cohort: first, jobs: 2, measuredJobs: 1, notMeasuredJobs: 1, noCompilationJobs: 0, bypassedJobs: 0, hits: 4, misses: 0, bypassed: 0, hitRate: 1 });
  assert.equal(report.find((entry) => entry.cohort.command === 'cargo test').hitRate, 0);
});

test('cohorts split exact command, profile, features, target, toolchain & host', () => {
  const base = { workspace: '/repo', toolchain: 'stable-1.98', targetTriple: 'aarch64-apple-darwin', compilerIdentity: { rustc: { sha256: 'a' } }, cacheEpoch: 'epoch-1', platform: 'darwin', hostArch: 'arm64', hostName: 'mac' };
  const check = cargoCohort({ ...base, argv: ['check', '--features', 'z,a'] });
  const testRun = cargoCohort({ ...base, argv: ['test', '--release', '--features=z,a', '--target=x86_64-apple-darwin'] });
  assert.deepEqual(check.features, ['a', 'z']);
  assert.equal(check.profile, 'development');
  assert.equal(testRun.profile, 'release');
  assert.equal(testRun.targetTriple, 'x86_64-apple-darwin');
  assert.notEqual(cacheCohortKey(check), cacheCohortKey(testRun));
});
